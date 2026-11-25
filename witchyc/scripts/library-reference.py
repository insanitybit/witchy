#!/usr/bin/env python3
"""A Markdown reference of the public functions in some standard library modules: each signature
as written, with the first sentence of the comment above it. Run from the repository root; the
pages are not edited by hand:

  python3 scripts/library-reference.py --write    write docs/library-text.md and library-collections.md
  python3 scripts/library-reference.py --check    exit 1, naming any page the library has moved past
  python3 scripts/library-reference.py '--title=T' MODULE...   print a page for other modules

Functions named `memory_*` are public for proofs, not for programs, and are left out."""
import io, re, sys

# The pages written, and the modules in each.
PAGES = {
    'docs/library-text.md': ('Text, numbers as text, console output and encodings',
                             ['string', 'text', 'span', 'console', 'int', 'float', 'encoding']),
    'docs/library-collections.md': ('Collections', ['vec', 'hashmap', 'keymap', 'sort', 'deque', 'heap']),
}

def entries(path):
    lines = open(path).read().split('\n')
    owner = None
    out = []
    for i, line in enumerate(lines):
        m = re.match(r'^impl(?:<[^>]*>)?\s+(?:([A-Za-z_]+(?:<[^>]*>)?)\s+for\s+)?([A-Za-z_][A-Za-z_0-9]*(?:<[^>]*>)?):', line)
        if m:
            owner = (m.group(2), m.group(1))
            continue
        if line and not line.startswith(' ') and not line.startswith('impl'):
            if not line.startswith('//'):
                owner = None
        # Type parameters may nest one level (`<Q: Any + Hash + Equivalent<K>>`).
        m = re.match(r'^(\s*)pub (?:nopanic )?fn ([a-z_0-9]+)(<(?:[^<>]|<[^<>]*>)*>)?\((.*)\)( -> (.*))?:\s*$', line)
        if not m:
            continue
        # The comment lines just above, joined; its first sentence.
        j = i - 1
        comment = []
        while j >= 0 and lines[j].strip().startswith('//'):
            comment.insert(0, lines[j].strip()[2:].strip())
            j -= 1
        text = ' '.join(comment)
        first = re.split(r'(?<=[.!?])\s', text, maxsplit=1)[0] if text else ''
        params = m.group(4)
        result = m.group(6) or '()'
        name = m.group(2)
        # Helpers public for proofs, not for programs.
        if name.startswith('memory_'):
            continue
        if m.group(1) and owner:
            shown = f'{owner[0]}.{name}'
        else:
            shown = name
        out.append((shown, f'{m.group(3) or ""}({params}) -> {result}', first))
    return out

def cell(text):
    return text.replace('|', '\\|')

def page(title, modules):
    out = io.StringIO()
    out.write(f'# {title}: library reference\n\n')
    out.write('Generated from the library source by `scripts/library-reference.py`; each line is a public\n')
    out.write('function as declared, with the first sentence of its comment. `self: &String` is called as\n')
    out.write('`text.name(...)`; a function with no `self` is called as `name(...)`.\n\n')
    for module in modules:
        path = f'library/wasm-{module.replace("_", "-")}.wy'
        rows = entries(path)
        if not rows:
            continue
        out.write(f'## `import {module}` ({path})\n\n')
        out.write('| Function | Signature | What it does |\n')
        out.write('| --- | --- | --- |\n')
        for shown, signature, first in rows:
            out.write(f'| `{cell(shown)}` | `{cell(signature)}` | {cell(first)} |\n')
        out.write('\n')
    return out.getvalue()

def main():
    args = sys.argv[1:]
    if args in (['--check'], ['--write']):
        stale = []
        for path, (title, modules) in PAGES.items():
            text = page(title, modules)
            if args[0] == '--write':
                open(path, 'w').write(text)
            elif open(path).read() != text:
                stale.append(path)
        if stale:
            print('library reference out of date: ' + ' '.join(stale) + ' (python3 scripts/library-reference.py --write)')
            sys.exit(1)
        return
    title = 'Library'
    if args and args[0].startswith('--title='):
        title = args.pop(0)[len('--title='):]
    sys.stdout.write(page(title, args))

main()
