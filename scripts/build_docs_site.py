#!/usr/bin/env python3
"""Build the Pages landing pages and Markdown guides into a static directory."""

from __future__ import annotations

import argparse
import html
import posixpath
import re
import shutil
from pathlib import Path
from urllib.parse import quote, unquote, urlsplit, urlunsplit

from markdown_it import MarkdownIt


ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "https://github.com/allandefraga/brainwire"
SOURCE_PREFIX = f"{REPOSITORY}/blob/main/"
IMAGE_TYPES = {".gif", ".jpeg", ".jpg", ".png", ".webp"}
README_SECTIONS = {
    "quickstart": "start",
    "try-a-connection-with-docker": "docker-quickstart",
    "connect-a-python-application-optional": "python",
    "try-agent-context": "agent-memory",
    "explore-in-the-terminal": "terminal",
    "current-limits": "limits",
}


def relative_url(target: Path, page: Path) -> str:
    return quote(posixpath.relpath(target.as_posix(), page.parent.as_posix()))


def rewrite_url(value: str, source: Path, page: Path, guides: dict[Path, Path], output: Path) -> str:
    if value.startswith(SOURCE_PREFIX):
        parsed = urlsplit(value.removeprefix(SOURCE_PREFIX))
        target = (ROOT / unquote(parsed.path)).resolve()
    else:
        parsed = urlsplit(value)
        if parsed.scheme or parsed.netloc or not parsed.path or parsed.path.startswith("/"):
            return value
        target = (source.parent / unquote(parsed.path)).resolve()
    if not target.is_relative_to(ROOT):
        return value
    if target.is_dir() and target / "README.md" in guides:
        target /= "README.md"
    fragment = parsed.fragment
    if target == ROOT / "README.md" and (not fragment or fragment in README_SECTIONS):
        new_url = relative_url(Path("index.html"), page)
        fragment = README_SECTIONS.get(fragment, "overview")
    elif target in guides:
        new_url = relative_url(guides[target], page)
    elif target.is_file() and target.suffix.lower() in IMAGE_TYPES:
        asset = Path("guide-assets") / target.relative_to(ROOT)
        (output / asset).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(target, output / asset)
        new_url = relative_url(asset, page)
    else:
        kind = "tree" if target.is_dir() else "blob"
        new_url = f"{REPOSITORY}/{kind}/main/{quote(target.relative_to(ROOT).as_posix())}"
    return urlunsplit(("", "", new_url, parsed.query, fragment))


def render_guide(markdown: MarkdownIt, source: Path, page: Path, guides: dict[Path, Path], output: Path) -> str:
    env = {}
    tokens = markdown.parse(source.read_text(encoding="utf-8"), env)
    title = None
    headings = {}
    ids = set()
    for index, token in enumerate(tokens):
        if token.type == "heading_open":
            label = "".join(child.content for child in tokens[index + 1].children or []
                            if child.type in {"text", "code_inline"})
            if token.tag == "h1" and title is None:
                title = label
            base = re.sub(r"[^\w -]", "", label.lower()).replace(" ", "-")
            if base:
                slug = base
                suffix = 1
                while slug in ids:
                    slug = f"{base}-{suffix}"
                    suffix += 1
                ids.add(slug)
                token.attrSet("id", slug)
                if token.tag in {"h2", "h3"}:
                    headings[slug] = label
        for child in token.children or []:
            attribute = {"link_open": "href", "image": "src"}.get(child.type)
            if attribute and child.attrGet(attribute):
                child.attrSet(attribute, rewrite_url(child.attrGet(attribute), source, page, guides, output))
    return wrap_guide(title or source.stem, headings, markdown.renderer.render(tokens, markdown.options, env), source, page)


def wrap_guide(title: str, headings: dict[str, str], body: str, source: Path, page: Path) -> str:
    title = html.escape(title)
    home = relative_url(Path("index.html"), page)
    styles = relative_url(Path("styles.css"), page)
    logo = relative_url(Path("assets/brainwire-logo.svg"), page)
    source_url = SOURCE_PREFIX + quote(source.relative_to(ROOT).as_posix())
    contents = "".join(f'<a href="#{quote(slug)}">{html.escape(label)}</a>' for slug, label in headings.items())
    language = "pt-BR" if source.name.endswith(".pt-BR.md") else "en"
    return f'''<!doctype html>
<html lang="{language}">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title} · Brainwire</title>
  <link rel="stylesheet" href="{styles}">
</head>
<body>
<a class="skip" href="#main">Skip to content</a>
<div class="docs-layout">
  <aside class="sidebar">
    <a class="brand" href="{home}"><img src="{logo}" alt="Brainwire" width="500" height="128"></a>
    <nav class="section-nav" aria-label="Guide sections">
      <a href="{home}#guides">All guides</a>
      {contents}
    </nav>
  </aside>
  <main id="main">
    <header class="topbar"><a href="{home}">Documentation</a><a href="{source_url}">View Markdown source</a></header>
    <article class="guide-content">{body}</article>
  </main>
</div>
</body>
</html>
'''


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="new or empty directory for the generated site")
    output = parser.parse_args().output.resolve()
    if output.is_relative_to(ROOT / "docs-site"):
        parser.error("output directory must be outside docs-site")
    if output.exists() and any(output.iterdir()):
        parser.error("output directory must be empty")
    shutil.copytree(ROOT / "docs-site", output, dirs_exist_ok=True)

    docs_root = ROOT / "docs"
    internal_plans = docs_root / "superpowers" / "plans"
    docs = sorted(source for source in docs_root.rglob("*.md") if not source.is_relative_to(internal_plans))
    extra = [ROOT / "BENCHMARKS.md", *ROOT.glob("examples/**/README.md"),
             ROOT / "benchmarks/comparison/README.md"]
    guides = {source: Path("guides") / source.relative_to(docs_root).with_suffix(".html") for source in docs}
    guides.update({source: Path("reference") / source.relative_to(ROOT).with_suffix(".html") for source in extra})
    markdown = MarkdownIt("commonmark", {"html": False}).enable(["table", "strikethrough"])
    for source, page in guides.items():
        if source.is_symlink() or not source.resolve().is_relative_to(ROOT):
            raise SystemExit(f"Guide must be a repository file: {source}")
        destination = output / page
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(render_guide(markdown, source, page, guides, output), encoding="utf-8")

    for name in ("index.html", "pt-br.html"):
        landing = output / name
        source = ROOT / "docs-site" / name

        def link(match: re.Match[str]) -> str:
            href = rewrite_url(html.unescape(match[1]), source, Path(name), guides, output)
            return f'href="{html.escape(href, quote=True)}"'

        pattern = rf'href="({re.escape(SOURCE_PREFIX)}[^"]+)"'
        landing.write_text(re.sub(pattern, link, source.read_text(encoding="utf-8")), encoding="utf-8")
    print(f"Built {len(guides)} Markdown guides and the documentation landing pages in {output}.")


if __name__ == "__main__":
    main()
