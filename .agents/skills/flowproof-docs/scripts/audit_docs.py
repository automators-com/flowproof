#!/usr/bin/env python3
"""Audit public Flowproof Markdown for broken local links and density hotspots."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote


LINK_RE = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")
WORD_RE = re.compile(r"\b[\w'-]+\b")
SKIP_SCHEMES = ("http://", "https://", "mailto:", "tel:")


def markdown_files(root: Path) -> list[Path]:
    files = [root / "README.md"] if (root / "README.md").is_file() else []
    files.extend(sorted((root / "docs").rglob("*.md")))
    return files


def link_target(raw: str) -> str:
    target = raw.strip()
    if target.startswith("<") and ">" in target:
        target = target[1 : target.index(">")]
    else:
        target = target.split(maxsplit=1)[0]
    return unquote(target.split("#", 1)[0])


def broken_links(root: Path, files: list[Path]) -> list[str]:
    findings: list[str] = []
    for page in files:
        for line_number, line in enumerate(page.read_text(encoding="utf-8").splitlines(), 1):
            for match in LINK_RE.finditer(line):
                raw = match.group(1)
                if raw.startswith(("#", *SKIP_SCHEMES)):
                    continue
                target = link_target(raw)
                if not target:
                    continue
                resolved = (page.parent / target).resolve()
                exists = resolved.exists()
                if not exists and not resolved.suffix:
                    exists = resolved.with_suffix(".md").is_file()
                if not exists:
                    findings.append(f"{page.relative_to(root)}:{line_number}: {raw}")
    return findings


def paragraphs(text: str) -> list[str]:
    blocks = re.split(r"\n\s*\n", text)
    return [
        block.replace("\n", " ")
        for block in blocks
        if block.strip()
        and not block.lstrip().startswith(("```", "---", "#", "|", "- ", "1. "))
    ]


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    files = markdown_files(root)
    broken = broken_links(root, files)

    print(f"Audited {len(files)} public Markdown files.")
    print("\nBroken local links:")
    if broken:
        for finding in broken:
            print(f"  {finding}")
    else:
        print("  none")

    dense_pages: list[tuple[int, Path]] = []
    dense_paragraphs: list[tuple[int, Path]] = []
    for page in files:
        text = page.read_text(encoding="utf-8")
        word_count = len(WORD_RE.findall(text))
        if word_count >= 1500:
            dense_pages.append((word_count, page))
        longest = max((len(WORD_RE.findall(p)) for p in paragraphs(text)), default=0)
        if longest >= 120:
            dense_paragraphs.append((longest, page))

    print("\nPages with at least 1,500 words (review, not failure):")
    for count, page in sorted(dense_pages, reverse=True):
        print(f"  {count:5}  {page.relative_to(root)}")
    if not dense_pages:
        print("  none")

    print("\nPages with a paragraph of at least 120 words (review, not failure):")
    for count, page in sorted(dense_paragraphs, reverse=True):
        print(f"  {count:5}  {page.relative_to(root)}")
    if not dense_paragraphs:
        print("  none")

    return 1 if broken else 0


if __name__ == "__main__":
    raise SystemExit(main())
