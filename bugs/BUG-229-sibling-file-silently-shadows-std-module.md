# BUG-229: A sibling `.witchy` file silently shadows a bundled std module, with errors blaming std

Severity: MED
Status: FIXED
Verified: 2026-07-09 FIXED on fix/std-shadow-diagnostic
Component: linker module resolution (sibling search path vs std registry), diagnostics

## Problem

`import bytes` resolves to a **sibling file** `bytes.witchy` in the entry
file's directory before the bundled std module of the same name — per
spec/language.md:894 the search order is sibling-then-std, and the linker's
"locally provided modules take precedence" rule (linker.rs:439) implements
it. That precedence is a reasonable default, but it is **completely silent**,
and the resulting errors are attributed to *std*:

```console
$ ls scratch/round2-audit/ | grep bytes
bytes.witchy          # an unrelated scratch file that happens to be named bytes
bookbytes.witchy      # the book's tour-vm example, verbatim

$ witchy check scratch/round2-audit/bookbytes.witchy
link error: module `bytes` has no function `from_string`
```

The message asserts std's `bytes` lacks `from_string` — false and deeply
confusing (I lost an hour to it; a user gets no hint that their own
`bytes.witchy` was picked, or even that a file was picked over std at all).
When the shadowing file exports functions with matching names/arities, the
substitution is **silent and semantic**, not an error — the supply-chain
variant of that is BUG-100 (build-step generated modules) and BUG-086 (entry
selection), but the plain local-development variant deserves its own fix:
any scratch/test/example directory accumulates files with std-like names
(`bytes.witchy`, `json.witchy`, `set.witchy` are natural probe names), and
every `.witchy` file in that directory changes meaning when one appears.

## Repro

```console
$ mkdir /tmp/shadow && cd /tmp/shadow
$ printf 'fn unrelated() -> Int:\n    1\n' > bytes.witchy
$ cat > app.witchy <<'EOF'
import bytes

fn main(console: Console):
    let b = bytes.from_string("hi")
    print(console, "${bytes.length(b)}")
EOF
$ witchy check app.witchy
link error: module `bytes` has no function `from_string`
```

Remove `bytes.witchy` → checks ok, parity agrees. (Original hit: the book's
`tour-vm.md` bytes example failed inside my scratch dir for this reason.)

## Fix direction

Two independent improvements, either sufficient for the diagnosis pain, both
cheap:

1. **Name the winner in the error.** When a dotted call fails to resolve in a
   module that shadows a bundled std name, say so: ``module `bytes` (from
   ./bytes.witchy, which shadows the standard library module of the same
   name) has no function `from_string` — rename the file or use its real
   API``. The linker knows both facts (`std_source(name).is_some()` and the
   sibling path) at the point it builds the FnTable.
2. **Warn on the shadow itself.** A sibling/`--dep` module whose name is in
   `STD_MODULES` gets a check-time warning (or error under `witchy pm`
   builds, aligning with BUG-100's fix): shadowing std is almost never
   intended and is one `import` away from silent behavior substitution.

A differential test: a project with a sibling `list.witchy` — pin whichever
contract (warn/error/precise-error-text) is chosen.

## Fixed

The CLI file/dependency loader now tracks which modules came from actual user
files, and passes that origin set into the linker. When a user-provided module
has the same name as a bundled std module and a dotted call/reference misses, the
link error now names the shadow explicitly:

```text
module `bytes` is provided by this program and shadows the bundled standard-library module `bytes`; it has no function `from_string`
```

Bundled std fallback imports are not marked as user modules, so ordinary
`import list; list.nonexistent(...)` keeps the old direct missing-function
diagnostic instead of falsely claiming std shadows itself. Covered by
`std_shadowing_module_missing_function_names_shadow`.
