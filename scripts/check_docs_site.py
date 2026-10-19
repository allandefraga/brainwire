#!/usr/bin/env python3
"""Validate local links and assets in the standalone documentation site."""

from __future__ import annotations

import re
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit


ROOT = Path("docs-site").resolve()


class References(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.local: list[str] = []
        self.ids: set[str] = set()

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if values.get("id"):
            self.ids.add(values["id"] or "")
        for name in ("href", "src"):
            value = values.get(name)
            if value:
                self.local.append(value)


def main() -> None:
    html_files = sorted(ROOT.rglob("*.html"))
    if not html_files:
        raise SystemExit("docs-site has no HTML files")

    errors: list[str] = []
    checked = 0
    for html_file in html_files:
        parser = References()
        parser.feed(html_file.read_text(encoding="utf-8"))
        for reference in parser.local:
            parsed = urlsplit(reference)
            if parsed.scheme or parsed.netloc:
                continue
            if not parsed.path:
                if parsed.fragment and unquote(parsed.fragment) not in parser.ids:
                    errors.append(f"{html_file.relative_to(ROOT)}: missing #{parsed.fragment}")
                continue

            target = (html_file.parent / unquote(parsed.path)).resolve()
            if target != ROOT and ROOT not in target.parents:
                errors.append(f"{html_file.relative_to(ROOT)}: path escapes docs-site: {reference}")
                continue
            if target.is_dir():
                target /= "index.html"
            checked += 1
            if not target.is_file():
                errors.append(f"{html_file.relative_to(ROOT)}: missing {reference}")

        for css_file in re.findall(r"url\((['\"]?)(.*?)\1\)", html_file.read_text(encoding="utf-8")):
            value = css_file[1].strip()
            if not value or value.startswith(("data:", "http:", "https:", "#")):
                continue
            target = (html_file.parent / unquote(value)).resolve()
            if target != ROOT and ROOT not in target.parents:
                errors.append(f"{html_file.relative_to(ROOT)}: CSS path escapes docs-site: {value}")
            elif not target.is_file():
                errors.append(f"{html_file.relative_to(ROOT)}: missing CSS asset {value}")
            else:
                checked += 1

    if errors:
        raise SystemExit("Documentation site validation failed:\n" + "\n".join(errors))
    print(f"Validated {len(html_files)} HTML page(s) and {checked} local link/asset reference(s).")


if __name__ == "__main__":
    main()
