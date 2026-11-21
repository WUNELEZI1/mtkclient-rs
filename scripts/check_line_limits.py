#!/usr/bin/env python3
"""Enforce the project rule: every hand-maintained .rs file is at most 500 lines."""

import sys
from pathlib import Path

LIMIT = 500
EXEMPT = {"src/system/chips_generated.rs"}


def main() -> int:
    violations = []
    for path in sorted(Path("src").rglob("*.rs")):
        rel = path.as_posix()
        if rel in EXEMPT:
            continue
        count = len(path.read_text(encoding="utf-8").splitlines())
        if count > LIMIT:
            violations.append((count, rel))
    for count, rel in violations:
        print(f"{rel}: {count} lines")
    print(f"files over {LIMIT} lines: {len(violations)}")
    return 1 if violations else 0


if __name__ == "__main__":
    sys.exit(main())
