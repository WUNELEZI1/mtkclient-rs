#!/usr/bin/env python3
"""Report CJK text in Rust comments only (strings and raw strings are skipped)."""

import re
import subprocess
import sys
from pathlib import Path

HAN = re.compile("[\u4e00-\u9fff]")
BS = chr(92)


def scan(src: str):
    """Yield (line_number, comment_text) and collect CJK string literals."""
    comments, strings = [], []
    i, n, line = 0, len(src), 1
    while i < n:
        c = src[i]
        if c == "\n":
            line += 1
            i += 1
            continue
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            start_line = line
            j = src.find("\n", i)
            if j < 0:
                j = n
            comments.append((start_line, src[i:j]))
            i = j
            continue
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            start_line = line
            j = src.find("*/", i + 2)
            j = n if j < 0 else j + 2
            seg = src[i:j]
            comments.append((start_line, seg))
            line += seg.count("\n")
            i = j
            continue
        if c == "r" and i + 1 < n and (src[i + 1] == '"' or src[i + 1] == "#"):
            j, hashes = i + 1, 0
            while j < n and src[j] == "#":
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                close = '"' + "#" * hashes
                end = src.find(close, j + 1)
                end = n - 1 if end < 0 else end
                seg = src[j + 1 : end]
                if HAN.search(seg):
                    strings.append(seg)
                line += src[i : end + len(close)].count("\n")
                i = end + len(close)
                continue
            i += 1
            continue
        if c == '"':
            j, buf = i + 1, []
            while j < n:
                if src[j] == BS:
                    if j + 1 < n and src[j + 1] == "\n":
                        line += 1
                    buf.append(src[j : j + 2])
                    j += 2
                    continue
                if src[j] == '"':
                    break
                if src[j] == "\n":
                    line += 1
                buf.append(src[j])
                j += 1
            seg = "".join(buf)
            if HAN.search(seg):
                strings.append(seg)
            i = j + 1
            continue
        if c == "'" and i + 2 < n:
            if src[i + 1] == BS:
                # char escape such as '\n' or '\u{...}'
                j = i + 2
                while j < n and src[j] != "'" and j < i + 12:
                    j += 1
                i = j + 1
                continue
            if src[i + 2] == "'":
                # plain char literal 'x'
                i += 3
                continue
            # everything else is a lifetime ('a, 'static) - leave it to the scanner
            i += 1
            continue
        i += 1
    return comments, strings


def main() -> int:
    hits = []
    for path in sorted(Path("src").rglob("*.rs")):
        comments, _ = scan(path.read_text(encoding="utf-8"))
        for number, text in comments:
            if HAN.search(text):
                hits.append(f"{path.as_posix()}:{number}: {text.strip()[:150]}")
    for hit in hits:
        print(hit)
    print(f"Chinese comment segments: {len(hits)}")
    return 1 if hits else 0


if __name__ == "__main__":
    sys.exit(main())
