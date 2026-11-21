#!/usr/bin/env python3
"""Generate a minimal CycloneDX 1.5 SBOM from `cargo metadata`."""

import argparse
import json
import subprocess
import sys
import uuid
from pathlib import Path


def load_metadata() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def component(package: dict) -> dict:
    item = {
        "type": "library" if package.get("source") else "application",
        "bom-ref": f"pkg:cargo/{package['name']}@{package['version']}",
        "name": package["name"],
        "version": package["version"],
    }
    if package.get("license"):
        item["licenses"] = [{"license": {"id": package["license"]}}]
    if package.get("source"):
        item["purl"] = f"pkg:cargo/{package['name']}@{package['version']}"
    return item


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, help="Output JSON path")
    args = parser.parse_args()

    metadata = load_metadata()
    packages = metadata.get("packages", [])
    sbom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{uuid.uuid4()}",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "name": "mtkclient-rs",
                "version": next(
                    (p["version"] for p in packages if p["name"] == "mtkclient-rs"), "unknown"
                ),
                "licenses": [{"license": {"id": "GPL-3.0-or-later"}}],
                "purl": "pkg:cargo/mtkclient-rs",
            }
        },
        "components": [component(p) for p in packages],
    }

    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(sbom, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"SBOM written to {output} ({len(sbom['components'])} components)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
