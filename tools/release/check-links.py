#!/usr/bin/env python3
"""Checks that relative Markdown links resolve inside a tree.

Usage: check-links.py ROOT [FILE.md ...]

Without files, checks every .md file under ROOT (outside node_modules, target and .git).
A link must point to an existing file or directory inside ROOT; a #fragment on a Markdown
target must match one of its headings (GitHub-style anchors). External links (with a URL
scheme) and links inside code spans or fenced blocks are skipped. Prints each broken link
as FILE:LINE: TARGET (reason) and exits 1 if there is any.
"""

import re
import sys
from pathlib import Path

SKIP_DIRS = {"node_modules", "target", ".git"}
LINK = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
FENCE = re.compile(r"^\s*(```|~~~)")
CODE_SPAN = re.compile(r"`[^`]*`")


def anchors(path, cache={}):
    if path not in cache:
        found, counts, fenced = set(), {}, False
        for line in path.read_text(encoding="utf-8").splitlines():
            if FENCE.match(line):
                fenced = not fenced
                continue
            match = None if fenced else re.match(r"^#{1,6}\s+(.*?)\s*#*\s*$", line)
            if match:
                slug = re.sub(r"[^\w\- ]", "", match.group(1).strip().lower()).replace(" ", "-")
                n = counts.get(slug, 0)
                counts[slug] = n + 1
                found.add(slug if n == 0 else f"{slug}-{n}")
        cache[path] = found
    return cache[path]


def check(root, md):
    problems, fenced = [], False
    for number, line in enumerate(md.read_text(encoding="utf-8").splitlines(), 1):
        if FENCE.match(line):
            fenced = not fenced
            continue
        if fenced:
            continue
        for match in LINK.finditer(CODE_SPAN.sub("", line)):
            target = match.group(1)
            if re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", target):
                continue
            path_part, _, fragment = target.partition("#")
            dest = (md.parent / path_part).resolve() if path_part else md.resolve()
            where = f"{md.relative_to(root)}:{number}: {target}"
            if not dest.is_relative_to(root):
                problems.append(f"{where} (outside the tree)")
            elif not dest.exists():
                problems.append(f"{where} (missing)")
            elif fragment and dest.suffix == ".md" and fragment not in anchors(dest):
                problems.append(f"{where} (no such heading)")
    return problems


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__.strip().splitlines()[2])
    root = Path(sys.argv[1]).resolve()
    if len(sys.argv) > 2:
        files = [Path(name).resolve() for name in sys.argv[2:]]
    else:
        files = [
            path for path in sorted(root.rglob("*.md"))
            if not SKIP_DIRS.intersection(path.relative_to(root).parts)
        ]
    problems = [problem for md in files for problem in check(root, md)]
    for problem in problems:
        print(problem)
    print(f"checked {len(files)} Markdown files: {len(problems)} broken links", file=sys.stderr)
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
