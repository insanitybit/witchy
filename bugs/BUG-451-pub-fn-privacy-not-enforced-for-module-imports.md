# BUG-451: `pub fn` privacy is not enforced for module imports

Severity: HIGH
Status: FIXED
Verification: SOURCE
Component: language frontend, module namespaces, `pub fn`, import privacy, stdlib API surface
Found: 2026-07-05
Fixed: 2026-07-06 (`d4b8679`)

## Resolution

Fixed by `d4b8679` (`syntax: enforce module function privacy`).

The linker now carries each function's `public` flag in its module function
table. Same-module calls can still use private helpers, but imported/module-
qualified calls and module-qualified function values require `pub fn`. The type
resolver also validates `from X import f` against public functions only.

Validation: `./scripts/check.sh` green in
`/Users/cobrien/workspace/witchy-module-privacy`:
`1444 passed / 2 skipped`, plus build, clippy, Witchy fmt, and wasm playground
build.

## Summary

The language docs say `pub fn` is exported from its module and every other
function is module-private, but the linker resolves imported calls from a table
that contains every function, not just public ones.

That makes private helper functions callable from any module that imports the
helper's module:

```witchy
// helper.witchy
fn secret() -> Int:
    42

// main.witchy
import helper

fn main() -> Int:
    helper.secret()
```

The same leak exists for `from helper import secret` validation and for
value-position `helper.secret` eta-expansion. Docs and `witchy which` hide
non-`pub` functions, so the implemented language has a hidden callable API that
does not match its public surface.

## Evidence

- `book/src/tour-functions.md:21-22` says a `pub fn` is exported and everything
  else is module-private.
- `spec/language.md:920-921` says `pub` items are importable and everything else
  is module-private.
- `crates/witchy-syntax/src/ast.rs:170-172` carries a `Function.public` flag.
- `crates/witchy-syntax/src/linker.rs:522-533` builds the module function table
  by inserting every `Item::Function`, but stores only `EtaSig`; `public` is
  discarded.
- `crates/witchy-syntax/src/linker.rs:1518-1537` accepts a qualified
  `module.fn(...)` call when that table contains `fn`, with no public check.
- `crates/witchy-syntax/src/linker.rs:1549-1565` accepts a bare imported call
  when exactly one imported module contains the function, again with no public
  check.
- `crates/witchy-syntax/src/linker.rs:1138-1185` validates `module.fn` in value
  position through the same `resolve_call(...)` path before eta-expanding it.
- `crates/witchy-syntax/src/type_resolve.rs:94-99` describes the world function
  table as "exported function names", but `:107-128` also inserts every function
  regardless of `Function.public`; `:231-260` therefore validates
  `from X import private_helper` as an exported function import whenever the
  name appears in that all-functions table.
- `crates/witchy-syntax/src/doc.rs:236-242` has a unit test asserting private
  functions are omitted from generated docs, reinforcing the intended split.

## Impact

- A dependency cannot keep implementation helpers out of its consumer-facing
  language surface. Any non-`pub` helper is still linkable by qualified name.
- Generated docs and `witchy which` can omit a function while the compiler still
  treats it as callable, making the language feel inconsistent and hacky.
- Stdlib modules such as `iter` contain many non-`pub` helper functions
  (`*_step`, `*_from`, etc.) that users can accidentally depend on despite being
  written as internal helpers.
- `from X import Y` claims to bind exported functions, but currently accepts
  private functions as if they were exported.

This is distinct from BUG-433. BUG-433 covers impl methods that are callable
across modules but invisible to docs/`which`. BUG-451 covers ordinary top-level
free functions whose existing `public` flag is ignored by the import resolver.

It is also distinct from BUG-175/BUG-176, which cover `pub` markers on
non-function declarations. BUG-451 is about the one top-level item kind that
already has a public/private bit.

## Expected

Keep separate facts for:

- functions declared in the current module, where private calls are valid;
- functions exported by another module, where only `pub fn` is visible.

Then enforce the exported table consistently for every cross-module spelling:

- qualified calls: `module.fn(...)`;
- bare function calls enabled by `from module import fn`;
- value-position function references: `module.fn`;
- `from module import fn` validation.

The fix may need to mark any intentionally cross-module std helper as `pub` or
introduce a deliberate package/internal visibility concept. It should not keep
the current accidental behavior where every helper is public by omission.

## Acceptance

- A module can call its own private `fn helper(...)`.
- A module importing another module cannot call `other.helper(...)` unless
  `helper` is declared `pub fn`.
- `from other import helper` rejects non-`pub` functions with a clear privacy
  diagnostic.
- `let f = other.helper` rejects non-`pub` functions with the same privacy rule.
- Existing public stdlib functions remain callable, and any std-internal
  cross-module helper dependency is made explicit by the chosen visibility
  design.
- Regression tests cover qualified calls, from-imported calls, and value-position
  references for both public and private functions.
