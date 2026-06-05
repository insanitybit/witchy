#!/usr/bin/env python3
"""One-time migration: rewrite witchy `r#"..."#` test sources to brace-free form.

For each raw string literal containing a brace, pipe its content through
`witchy fmt` (which only succeeds for witchy source that round-trips to the same
AST). Replace the content when fmt succeeds; leave everything else untouched, so
TOML / HTTP / non-witchy raw strings are never touched.
"""
import re
import subprocess
import sys

BIN = "target/debug/witchy"
TMP = "/tmp/_debrace.witchy"


def reformat(src: str):
    with open(TMP, "w") as f:
        f.write(src)
    r = subprocess.run([BIN, "fmt", TMP], capture_output=True)
    if r.returncode != 0:
        return None
    with open(TMP) as f:
        return f.read()


def main(path: str, apply: bool):
    text = open(path).read()
    changed = [0]

    def repl(m):
        content = m.group(1)
        if "{" not in content and "}" not in content:
            return m.group(0)
        out = reformat(content)
        if out is None:
            return m.group(0)
        if "{" in out or "}" in out:
            return m.group(0)  # safety: never emit braces
        changed[0] += 1
        return 'r#"\n' + out + '"#'

    new = re.sub(r'r#"(.*?)"#', repl, text, flags=re.DOTALL)
    if apply and new != text:
        open(path, "w").write(new)
    print(f"{path}: {changed[0]} raw strings reformatted{' (written)' if apply else ' (dry-run)'}")


if __name__ == "__main__":
    apply = "--apply" in sys.argv
    files = [a for a in sys.argv[1:] if not a.startswith("--")]
    for p in files:
        main(p, apply)
