#!/usr/bin/env python3
"""Write SHA256 checksums for every file in a directory."""

import argparse
import hashlib
import sys
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", help="Directory containing release artifacts")
    parser.add_argument("--output", required=True, help="Checksum file path")
    args = parser.parse_args()

    root = Path(args.directory)
    output = Path(args.output)
    files = sorted(p for p in root.rglob("*") if p.is_file() and p.resolve() != output.resolve())
    lines = [f"{sha256(path)}  {path.relative_to(root).as_posix()}" for path in files]
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"Wrote {len(lines)} checksums to {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
