#!/usr/bin/env python3
"""One-time migration: rewrite witchy test sources (raw `r#"..."#` and plain
`"..."` literals) from braces to brace-free off-side form.

Each candidate is piped through `witchy fmt`, which only succeeds for witchy
source that round-trips to the same AST. A literal is rewritten only when fmt
succeeds AND the content looks like a witchy module (starts with a top-level
item keyword), so TOML / HTTP / format strings are never touched. Always review
the diff before committing.
"""
import re
import subprocess
import sys

BIN = "target/debug/witchy"
TMP = "/tmp/_debrace.witchy"
ITEM_RE = re.compile(r'^\s*(pub\s+)?(fn|type|actor|impl|trait)\b')


def reformat(src: str):
    with open(TMP, "w") as f:
        f.write(src)
    r = subprocess.run([BIN, "fmt", TMP], capture_output=True)
    if r.returncode != 0:
        return None
    with open(TMP) as f:
        return f.read()


def unescape(s: str) -> str:
    out, i = [], 0
    table = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", '"': '"', "\\": "\\"}
    while i < len(s):
        if s[i] == "\\" and i + 1 < len(s):
            out.append(table.get(s[i + 1], s[i + 1]))
            i += 2
        else:
            out.append(s[i])
            i += 1
    return "".join(out)


def convert(content: str):
    """Return reformatted brace-free source, or None to leave the literal alone."""
    if "{" not in content and "}" not in content:
        return None
    if not ITEM_RE.match(content):
        return None
    out = reformat(content)
    if out is None or "{" in out or "}" in out:
        return None
    return out


def main(path: str, apply: bool):
    text = open(path).read()
    n = [0]

    def raw_repl(m):
        out = convert(m.group(1))
        if out is None:
            return m.group(0)
        n[0] += 1
        return 'r#"\n' + out + '"#'

    def plain_repl(m):
        out = convert(unescape(m.group(1)))
        if out is None:
            return m.group(0)
        n[0] += 1
        return 'r#"\n' + out + '"#'

    text = re.sub(r'r#"(.*?)"#', raw_repl, text, flags=re.DOTALL)
    text = re.sub(r'(?<![a-zA-Z0-9_#])"((?:[^"\\]|\\.)*)"', plain_repl, text)

    if apply:
        open(path, "w").write(text)
    print(f"{path}: {n[0]} literals reformatted{' (written)' if apply else ' (dry-run)'}")


if __name__ == "__main__":
    apply = "--apply" in sys.argv
    for p in [a for a in sys.argv[1:] if not a.startswith("--")]:
        main(p, apply)
