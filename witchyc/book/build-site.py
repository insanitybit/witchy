#!/usr/bin/env python3
"""Build the book and its linked reference pages with mdBook."""
from html import escape, unescape
import posixpath
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

book = Path(__file__).resolve().parent
repo = book.parent
destination = Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/witchy-book-site").resolve()
links = re.compile(r"(?<=\]\()([^()\s]+)(?=\))")

with tempfile.TemporaryDirectory(prefix="witchy-book-") as directory:
    stage = Path(directory)
    src = stage / "src"
    src.mkdir()
    shutil.copy2(book / "site.toml", stage / "book.toml")
    shutil.copytree(book / "theme", stage / "theme")
    pending = list(book.glob("*.md"))
    seen = set()
    while pending:
        path = pending.pop()
        if path in seen:
            continue
        seen.add(path)
        target = src / path.relative_to(repo)
        target.parent.mkdir(parents=True, exist_ok=True)
        if path.suffix != ".md":
            shutil.copy2(path, target)
            continue

        def follow(match):
            url = match[0]
            local = url.split("#", 1)[0]
            if not local or ":" in local:
                return url
            resolved = (path.parent / local).resolve()
            if resolved.is_relative_to(repo):
                if resolved.is_file():
                    pending.append(resolved)
                elif resolved.is_dir():
                    files = sorted(f for f in resolved.iterdir() if f.is_file())
                    listing = src / resolved.relative_to(repo) / "index.html"
                    listing.parent.mkdir(parents=True, exist_ok=True)
                    items = []
                    for file in files:
                        pending.append(file)
                        items.append(f'<li><a href="{escape(file.name, quote=True)}">{escape(file.name)}</a></li>')
                    listing.write_text(f"<!doctype html><meta charset=utf-8><title>{escape(resolved.name)}</title><ul>{''.join(items)}</ul>")
                    return local.rstrip("/") + "/index.html"
            return url

        body = links.sub(follow, path.read_text())
        target.write_text(body)

    summary = src / "SUMMARY.md"
    nav = (src / "book/SUMMARY.md").read_text()
    nav = links.sub(lambda m: "book/" + m[0], nav)
    welcome = src / "book/README.md"
    def root_link(match):
        url = match[0]
        path, separator, anchor = url.partition("#")
        if not path or ":" in path:
            return url
        return posixpath.normpath("book/" + path) + separator + anchor

    (src / "README.md").write_text(links.sub(root_link, welcome.read_text()))
    nav = nav.replace("(book/README.md)", "(README.md)", 1)
    nav += "\n# Reference\n\n"
    for path in sorted(seen):
        if path.suffix == ".md" and path.parent != book:
            relative = path.relative_to(repo)
            nav += f"- [{path.stem.replace('-', ' ').title()}]({relative})\n"
    summary.write_text(nav)
    subprocess.run(["mdbook", "build", stage, "--dest-dir", destination], check=True)

    tokens = re.compile(
        r'(?P<comment>//[^\n]*)|(?P<string>"(?:\\.|[^"\\])*")'
        r'|(?P<number>\b(?:0[xX][0-9a-fA-F_]+|[0-9][0-9_]*(?:\.[0-9_]+)?))'
        r'|(?P<word>\b[A-Za-z_][A-Za-z_0-9]*\b)'
    )
    keywords = set(
        "as assert brand break const continue else ensures false fn for gen if impl import in "
        "invariant let match move mut nopanic old predicate proof pub pure requires return "
        "sealed trait true type unreachable using var while yield with async await".split()
    )

    def highlight(block):
        source = unescape(block[1])
        pieces = []
        cursor = 0
        for token in tokens.finditer(source):
            pieces.append(escape(source[cursor:token.start()]))
            value = token[0]
            kind = token.lastgroup
            if kind == "word":
                kind = "keyword" if value in keywords else "type" if value[0].isupper() else None
            escaped = escape(value)
            pieces.append(f'<span class="wy-{kind}">{escaped}</span>' if kind else escaped)
            cursor = token.end()
        pieces.append(escape(source[cursor:]))
        return '<code class="language-witchy nohighlight">' + "".join(pieces) + "</code>"

    for page in destination.rglob("*.html"):
        original = page.read_text()
        rendered = re.sub(r'<code class="language-witchy">(.*?)</code>', highlight, original, flags=re.S)
        if rendered != original:
            page.write_text(rendered)
