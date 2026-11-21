#!/usr/bin/env python3
"""Snapshot CJK string literals before/after comment translation (audit helper)."""

import json
import re
import sys
from pathlib import Path

HAN = re.compile("[\u4e00-\u9fff]")
BACKSLASH = chr(92)


def string_literals(src: str):
    out = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            j = src.find("\n", i)
            i = n if j < 0 else j
        elif c == "/" and i + 1 < n and src[i + 1] == "*":
            j = src.find("*/", i + 2)
            i = n if j < 0 else j + 2
        elif c == "r" and i + 1 < n and (src[i + 1] == '"' or src[i + 1] == "#"):
            j, hashes = i + 1, 0
            while j < n and src[j] == "#":
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                close = '"' + "#" * hashes
                k = src.find(close, j + 1)
                if k < 0:
                    break
                out.append(src[j + 1 : k])
                i = k + len(close)
                continue
            i += 1
        elif c == '"':
            j, buf = i + 1, []
            while j < n:
                if src[j] == BACKSLASH:
                    buf.append(src[j : j + 2])
                    j += 2
                    continue
                if src[j] == '"':
                    break
                buf.append(src[j])
                j += 1
            out.append("".join(buf))
            i = j + 1
        else:
            i += 1
    return [s for s in out if HAN.search(s)]


def main() -> int:
    mode, target = sys.argv[1], Path(sys.argv[2])
    data = {}
    for path in sorted(Path("src").rglob("*.rs")):
        values = string_literals(path.read_text(encoding="utf-8"))
        if values:
            data[path.as_posix()] = values
    target.write_text(json.dumps(data, ensure_ascii=False, indent=1), encoding="utf-8")
    print(f"{mode}: {len(data)} files, {sum(len(v) for v in data.values())} CJK strings")
    return 0


if __name__ == "__main__":
    sys.exit(main())
