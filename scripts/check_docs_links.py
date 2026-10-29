#!/usr/bin/env python3
"""Check local inline Markdown links in the README and documentation.

This checks target paths, including image assets. It deliberately does not
validate external URLs, fragments, reference-style links or raw HTML links.
Fenced blocks and inline code are ignored. Run from the checkout, or use --root.
"""
from __future__ import annotations

import argparse
from pathlib import Path
import re
from urllib.parse import unquote, urlsplit


_ESCAPE = re.compile(r"\\([!\"#$%&'()*+,\-./:;<=>?@\[\]\\^_`{|}~])")


def without_code(text: str) -> str:
    """Mask code without changing line positions used in diagnostics."""
    lines = []
    fence = None
    for line in text.splitlines(keepends=True):
        opening = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", line.rstrip("\r\n"))
        if fence is not None:
            if opening and opening[1][0] == fence[0] and len(opening[1]) >= fence[1] and not opening[2].strip():
                fence = None
            lines.append("".join("\n" if c == "\n" else " " for c in line))
        elif opening and not (opening[1][0] == "`" and "`" in opening[2]):
            fence = (opening[1][0], len(opening[1]))
            lines.append("".join("\n" if c == "\n" else " " for c in line))
        else:
            lines.append(line)
    text = "".join(lines)
    result = list(text)
    cursor = 0
    while cursor < len(text):
        if text[cursor] != "`" or (cursor and text[cursor - 1] == "\\"):
            cursor += 1
            continue
        end = cursor
        while end < len(text) and text[end] == "`":
            end += 1
        marker = text[cursor:end]
        closing = end
        while True:
            closing = text.find(marker, closing)
            if closing < 0:
                break
            before = closing > 0 and text[closing - 1] == "`"
            after = closing + len(marker) < len(text) and text[closing + len(marker)] == "`"
            if not before and not after:
                break
            closing += len(marker)
        if closing < 0:
            cursor = end
            continue
        for position in range(cursor, closing + len(marker)):
            if result[position] != "\n":
                result[position] = " "
        cursor = closing + len(marker)
    return "".join(result)


def inline_links(text: str):
    """Yield (line, destination); balance parentheses and support <paths>."""
    text = without_code(text)
    cursor = 0
    while cursor < len(text):
        if text[cursor] != "[" or (cursor and text[cursor - 1] == "\\"):
            cursor += 1
            continue
        start = cursor
        depth = 1
        cursor += 1
        while cursor < len(text) and depth:
            if text[cursor] == "\\":
                cursor += 2
                continue
            if text[cursor] == "[":
                depth += 1
            elif text[cursor] == "]":
                depth -= 1
            cursor += 1
        if depth or cursor >= len(text) or text[cursor] != "(":
            continue
        cursor += 1
        while cursor < len(text) and text[cursor].isspace():
            cursor += 1
        destination = cursor
        if cursor < len(text) and text[cursor] == "<":
            cursor += 1
            destination = cursor
            while cursor < len(text) and text[cursor] not in ">\n":
                cursor += 2 if text[cursor] == "\\" else 1
            if cursor >= len(text) or text[cursor] != ">":
                continue
            value = text[destination:cursor]
            cursor += 1
        else:
            nested = 0
            while cursor < len(text):
                char = text[cursor]
                if char == "\\":
                    cursor += 2
                    continue
                if char == "(":
                    nested += 1
                elif char == ")":
                    if nested == 0:
                        break
                    nested -= 1
                elif char.isspace() and nested == 0:
                    break
                cursor += 1
            value = text[destination:cursor]
        # Verify a closing delimiter, permitting a quoted optional title.
        while cursor < len(text) and text[cursor].isspace():
            cursor += 1
        if cursor < len(text) and text[cursor] in "\"'":
            quote = text[cursor]
            cursor += 1
            while cursor < len(text) and text[cursor] != quote:
                cursor += 2 if text[cursor] == "\\" else 1
            cursor += 1
            while cursor < len(text) and text[cursor].isspace():
                cursor += 1
        if cursor < len(text) and text[cursor] == ")":
            yield text.count("\n", 0, start) + 1, _ESCAPE.sub(r"\1", value)
            cursor += 1


def check_links(root: Path, files: list[Path]) -> tuple[int, list[str]]:
    root = root.resolve()
    checked = 0
    errors = []
    for document in files:
        document = document.resolve()
        label = str(document.relative_to(root))
        try:
            text = document.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            errors.append(f"{label}: cannot read Markdown ({type(error).__name__})")
            continue
        for line, reference in inline_links(text):
            try:
                parsed = urlsplit(reference)
            except ValueError:
                errors.append(f"{label}:{line}: invalid link: {reference}")
                continue
            if parsed.scheme or parsed.netloc or not parsed.path:
                continue
            path = unquote(parsed.path)
            target = ((root / path.lstrip("/")) if path.startswith("/") else (document.parent / path)).resolve()
            checked += 1
            if target != root and root not in target.parents:
                errors.append(f"{label}:{line}: path leaves repository: {reference}")
            elif not target.exists():
                errors.append(f"{label}:{line}: missing local target: {reference}")
    return checked, errors


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    root = args.root.resolve()
    files = [root / "README.md", root / "BENCHMARKS.md"] + sorted((root / "docs").rglob("*.md"))
    checked, errors = check_links(root, files)
    if not (root / "docs").is_dir():
        errors.append("docs/: documentation directory is missing")
    if errors:
        raise SystemExit("Markdown link validation failed:\n" + "\n".join(errors))
    print(f"Validated {len(files)} Markdown files and {checked} local inline targets; fragments and external URLs are not checked.")


if __name__ == "__main__":
    main()
