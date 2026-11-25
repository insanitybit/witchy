#!/usr/bin/env python3
"""Does a program change what is proved of the library it imports?

Builds every given program with `wasm-checked` and, for each standard-library function,
collects the statements of its proof obligations. Programs that import the same standard
modules in the same order should state the same goals for the same library function: that is
what a checked-library cache would have to assume (docs/decisions.md, 2026-11-21). Prints
each library function whose statements differ between two such programs.

usage: library-prefix-stability.py COMPILER LIBRARY_DIR PROGRAM.wy...
The environment selects the checker binding and proof cache, as for any build.
"""
import collections, hashlib, json, os, re, subprocess, sys, tempfile
from concurrent.futures import ThreadPoolExecutor

compiler, library, programs = sys.argv[1], sys.argv[2], sys.argv[3:]
IMPORT = re.compile(r'import (?:"(?:[^"]*/)?(wasm-[a-z-]+\.wy)"|"([a-z-]+\.wy)"|([a-z_]+))\s*$')
HEAD = re.compile(r"^(?:    )?(?:pub |pure |nopanic )*fn\s+([A-Za-z_0-9]+)")
OWNER = re.compile(r"^impl(?:<[^>]*>)?\s+(?:.*\s+for\s+)?([A-Za-z0-9_]+)")

def imports(text, standard):
    for line in text.splitlines():
        m = IMPORT.match(line)
        if not m: continue
        if m.group(1): yield m.group(1)
        elif m.group(2) and standard: yield m.group(2)
        elif m.group(3): yield "wasm-" + m.group(3).replace("_", "-") + ".wy"

def prefix(path, seen=None, order=None):
    """The standard modules a program's bundle holds, in bundle order (imports first)."""
    seen, order = set() if seen is None else seen, [] if order is None else order
    def visit(text, standard, base):
        for line in text.splitlines():
            m = IMPORT.match(line)
            if not m: continue
            name = m.group(1) or (m.group(2) if standard else None) or (m.group(3) and "wasm-" + m.group(3).replace("_", "-") + ".wy")
            if name and os.path.exists(os.path.join(library, name)):
                if name not in seen:
                    seen.add(name); visit(open(os.path.join(library, name)).read(), True, library); order.append(name)
            elif m.group(0).startswith('import "') and not standard:
                local = os.path.join(base, line.split('"')[1])
                if os.path.exists(local) and local not in seen:
                    seen.add(local); visit(open(local).read(), False, os.path.dirname(local))
    visit(open(path).read(), False, os.path.dirname(path))
    return tuple(order)

names = set()
for file in os.listdir(library):
    owner = None
    for line in open(os.path.join(library, file)).read().splitlines():
        if line.startswith("impl"): owner = (OWNER.match(line) or [None, None])[1]
        elif line and not line[0].isspace(): owner = None
        m = HEAD.match(line)
        if m: names.add(f"{owner}.{m.group(1)}" if owner and line.startswith(" ") else m.group(1))

def build(path):
    with tempfile.TemporaryDirectory() as scratch:
        try: done = subprocess.run([compiler, "wasm-checked", path, "-o", os.path.join(scratch, "m.wasm")], capture_output=True, text=True, timeout=60)
        except subprocess.TimeoutExpired: return path, None, None
    if done.returncode != 0 or not done.stdout.startswith("{"): return path, None, None
    return path, prefix(path), json.loads(done.stdout).get("obligations", [])

with ThreadPoolExecutor(8) as pool: results = list(pool.map(build, programs))
built = [r for r in results if r[2] is not None]
# (import prefix, library function) -> digest of all its statements, in order -> programs
seen = collections.defaultdict(lambda: collections.defaultdict(list))
for path, modules, obligations in built:
    per = collections.defaultdict(list)
    for o in obligations:
        if o.get("declaration") in names: per[o["declaration"]].append((o.get("goal_kind"), o.get("statement_sha256")))
    for name, goals in per.items():
        seen[(modules, name)][hashlib.sha256(json.dumps(goals).encode()).hexdigest()].append(path)
groups = len({modules for modules, _ in seen}); unstable = {key: by for key, by in seen.items() if len(by) > 1}
print(f"{len(programs)} programs, {len(built)} built, {groups} import prefixes, {len(seen)} (prefix, library function) pairs, {len(unstable)} with differing statements")
for (modules, name), by in sorted(unstable.items(), key=lambda item: item[0][1])[:40]:
    print(f"  {name}: " + "; ".join(f"{len(paths)} programs e.g. {paths[0]}" for paths in by.values()))
