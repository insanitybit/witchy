---
rfc: 0045
title: Aborts carry their message on the compiled backend
status: proposed
created: 2026-07-03
predecessors:
  - "0023 (checked heap — the precedent for always-linked, authority-free diagnostic imports)"
  - "0037 (correctness harness — the differential gate this extends to message parity)"
  - "0044 (std error policy — rule 3 requires 'legibly on both backends'; this is 'legibly')"
tracking:
---

# RFC-0045: Aborts carry their message on the compiled backend

## Summary

Every runtime abort on the compiled backend — out-of-bounds index,
`string.to_int` on junk, NaN ordering, `fail("…")` — surfaces as the same bare
`wasm trap: wasm `unreachable` instruction executed`. The interpreter says
``runtime error: `p20.test_fail`, line 4: the reason``; the backend users
actually run says nothing. `fail(msg)` literally **evaluates and drops its
message** (`crates/witchy-lower/src/codegen/builtins.rs:358-368`, comment:
"evaluate (and drop) the message, then `unreachable`"). This RFC adds one
always-linked, authority-free host import, `__witchy_abort`, called before the
trap; routes every abort through it with the interpreter's exact message; and
promotes **message parity** from "both error" to "same error text" in the
differential harness.

## Motivation

Probed at HEAD (PATH binary):

- `fail("the reason")` → `wasm trap: wasm `unreachable` instruction executed`.
  The same program under `witchy test` (which runs the **interpreter** —
  `src/main.rs:1523`) says ``FAILED: `p20.test_fail`, line 4: the reason``.
- `list.at(xs, 5)`, `string.to_int("junk")`, and NaN ordering are all the
  identical bare trap; the interpreter distinguishes them precisely
  (``list index 5 out of bounds (length 2)``, ``cannot parse `junk` as an
  Int``). Division by zero is the one abort with its own trap text — because
  wasm itself supplies it.
- `WITCHY_WASM_BACKTRACE=1` exists (`src/main.rs:1811-1816`; the emitted name
  section — `crates/witchy-wir/src/wir_encode.rs:295-309` — makes frames
  readable) but adds only *frames*, not the message, and is documented in no
  user-facing place.

The consequences compound. RFC-0044's rule 3 ("contract violation aborts
identically and *legibly* on both backends") converts several silent defaults
into aborts — indefensible if an abort is an unreadable trap. The differential
harness's `(Err(_), Err(_))` arm (`src/main.rs:1655-1661`) accepts *any* pair
of errors as agreement, so a compiled abort at the wrong site for the wrong
reason still passes — the "error-parity checks occurrence, not semantics" gap
the 2026-07-03 evaluation flagged (SEC-024/031 slipped through exactly there).
And the project's own north star — the interpreter is the reference, the
compiled path must match it — currently exempts the entire diagnostic surface.

## Design

### (a) The `__witchy_abort` import — always linked

One new host import in the `"witchy"` module:

```
__witchy_abort(site: i32, template: i32, a: i64, b: i64, str_ptr: i32, str_len: i32)
```

The host formats the message (below) and returns a trap (`bail!`), so the
call never returns; codegen still emits `unreachable` after it so the emitted
function stays stack-typed even if a host ignored the contract.

**Why always linked, and why this doesn't violate deny-by-omission.** The
precedent is explicit in `crates/witchy-runtime/src/runtime.rs:562-568`: the
RFC-0023 checked-heap imports "are not capabilities — they grant no authority
… so they are always defined." `__witchy_abort` grants strictly less than
those: it cannot read or write guest memory beyond the `(str_ptr, str_len)`
the guest hands it, it cannot return data to the guest (it never returns),
and its only effect is to *terminate execution with a label* — an ability the
guest already has via `unreachable`. It is a diagnostic channel to the host's
own stderr, not an authority. Correspondingly it is excluded from the
capability footprint exactly as `heap_register` is: `witchy caps` and the
coven widening gate never see it.

### (b) Message templates — one source of truth, shared with the interpreter

The message *text* must match the interpreter's byte-for-byte, so it must not
be written twice. Add `witchy-syntax/src/diag.rs` (every crate already
depends on witchy-syntax): a `DiagTemplate` enum with one variant per abort
class and one `render(&self, a, b, s) -> String` — e.g.

- `ListIndexOob` → `list index {a} out of bounds (length {b})`
- `BytesIndexOob`, `StringIndexOob`, `DictMissing` … (one per interpreter
  message currently produced at an abort site)
- `ParseInt` → ``cannot parse `{s}` as an Int``
- `NanOrder` → `cannot compare NaN` (the interpreter's existing wording,
  `interpreter.rs:2689`)
- `Fail` → `{s}` (the dynamic message, verbatim)

The **interpreter** is migrated to construct these errors *through the same
templates* (a mechanical refactor of its `format!` sites), so divergence
becomes a type error, not a test failure. The **wasmtime host** renders the
template on `__witchy_abort`. The template ids are part of the compiled
ABI (appended to the existing prelude index contract in `wir_prelude`).

### (c) Site information — function from frames, line from a site table

The interpreter prefixes ``runtime error: `module.func`, line N: …``
(`rt_at_line`, `crates/witchy-interp/src/interpreter.rs:245-258`). Two
channels recover the same on the compiled side:

- **Function name — free, from the existing name section.** The
  `__witchy_abort` host handler captures the wasm backtrace (the same frames
  `WITCHY_WASM_BACKTRACE` prints) and takes the innermost frame that is not a
  runtime helper (helpers are enumerable: the `wir_helpers` name list). The
  name section already survives to the binary and wasmtime already resolves
  frames through it — that machinery is proven; note honestly that it proves
  *names* survive, not lines, which is why lines need their own channel.
- **Line — a site table.** Codegen already has per-statement lines
  (`Block.lines`, `crates/witchy-syntax/src/ast.rs:260-261`). Emit a custom
  section `witchy.sites`: `site_id → (func_name, line)`. At each abort the
  guest passes `site`:
  - **Inline abort sites** (the ~5 in `codegen/mod.rs` + `fail` in
    `builtins.rs`) know their statement: codegen assigns a fresh site id and
    passes it as a constant.
  - **Shared-helper sites** (the ~8 `Unreachable`s inside `wir_helpers` —
    `list_at`'s bounds check etc. — which cannot know their caller) read a
    mutable global `$witchy_site` that codegen sets to the call site's id
    immediately before invoking any may-trap helper. `site = 0` means
    "unknown" and the prefix degrades to the frame-derived function name only.

Both hosts parse `witchy.sites` like the name section: pure metadata, ignored
for execution, parity-safe.

### (d) Message parity becomes a testable property

`verify_file`'s `(Err(i), Err(c))` arm changes from unconditional agreement to
**string equality**: when the interpreter returns `Err(msg)`, the compiled run
must abort with the *same* `runtime error: …` text (the host's `bail!` message
is already what `run_wasm_bytes` surfaces via `root_cause()`,
`src/main.rs:1806-1817`). Rollout in two notches to keep the gate green while
sites are converted:

1. **Lenient**: compare only when the compiled message is non-empty (a bare
   `unreachable` still passes) — new sites become load-bearing the moment
   they land.
2. **Strict** (the DoD): a bare `unreachable` reaching the host **is itself a
   failure** in the differential suite. Every abort must be routed. The
   capability-refusal precedent shows this end state is achievable: `` `..`
   escapes the Dir capability`` already prints identically on both backends
   because the message is produced host-side once.

This closes the occurrence-vs-semantics gap: a compiled backend that traps at
the wrong site or for the wrong reason now diverges loudly.

### (e) The browser shim

The pure-compute shim (`web/witchy-runtime/witchy-runtime.mjs`) adds
`__witchy_abort` to its non-capability import set (it sits beside `print` in
the "pure modules" tier — same authority argument as (a), and it must be
present or every footprint-empty module that can abort fails to instantiate).
The handler renders the template and throws a JS `Error` whose `.message` is
the same `runtime error: …` string; the existing shim tests gain an abort
case asserting the text matches a committed oracle. Templates are **not**
hand-mirrored in JS: the compiler emits the rendered template *format strings*
into a `witchy.templates` custom section, and both hosts (Rust and JS)
substitute `{a}`/`{b}`/`{s}` from that section — the compiler stays the single
source of truth.

### (f) `WITCHY_WASM_BACKTRACE` stays, and gets documented

Unchanged semantics: set it to also dump the full named-frame backtrace under
the message. It becomes the documented "frames add-on" in the spec's
diagnostics section and `witchy --help`'s environment table (today it is
documented nowhere user-facing).

### Binary-size cost

Message bodies are host-side templates: **zero guest bytes** per message. The
guest cost is the site table (~8 bytes/site plus shared name references) and
one `i32.const; global.set` before each may-trap helper call. Abort sites at
HEAD: 13 static emission sites (1 in `codegen/builtins.rs`, 4 in
`codegen/mod.rs`, 8 in `wir_helpers`) plus one site per user `fail`/`match`
lowering — order tens to low hundreds of table entries for a large program,
i.e. well under a kilobyte against multi-hundred-KB binaries. `fail`'s dynamic
strings are already interned in the data segment today (they are evaluated,
just dropped); no new data. The `global.set` on hot paths (`list.at`) is the
one measurable risk — gate the merge on the benchmark suite's kernel-clock
numbers staying within noise; if it doesn't, fall back to site-id-as-argument
threading for the two hottest helpers only.

## Alternatives

- **Trap-code-only** (distinguish abort classes but carry no text — e.g. one
  `unreachable` per class, disambiguated by frame). Rejected: still not
  legible — `fail`'s message and the `` `junk` ``/index values are the entire
  point, and RFC-0044's error-string voice rules would be unenforceable on
  half the surface.
- **Interpreter-rerun-on-trap** (on a compiled trap, re-execute under the
  interpreter to recover the message). Rejected honestly: doubles execution
  time on the failure path, replays effects (a program that wrote a file or
  sent bytes before aborting does it twice), diverges under nondeterminism
  (Clock/Rand), and silently reports the *wrong* message whenever the two
  backends disagree — which is precisely the case the harness exists to catch.
- **Full message strings in guest data segments** (no host templates). Works,
  but duplicates every message body per binary, forfeits the shared-template
  parity-by-construction with the interpreter, and makes the JS shim a third
  copy. The template-table-index scheme *is* the adopted design; this is the
  heavier variant it replaces.
- **DWARF/source-map debug info.** Gives lines "for free" in devtools-class
  hosts, but is large, unsupported in the pure shim, and still carries no
  dynamic message. Complementary at best; not this.
- **Do nothing.** The status quo fails RFC-0044 rule 3, keeps `witchy test`
  the only place users see real errors, and leaves the harness's error arm
  vacuous. Rejected.

## Drawbacks

- **A new ABI surface.** `__witchy_abort` + two custom sections + the
  `$witchy_site` global become part of the compiled contract; the prelude
  index seam (already flagged as fragile in the 0037-era notes) gains
  template-id constants. Mitigated by the differential gate: any skew between
  compiler and host is an immediate strict-mode failure.
- **Every host must implement it.** wasmtime runtime, browser shim, and any
  future embedder. The always-linked choice means an embedder that forgets it
  fails at instantiation (loud), not at first abort (silent) — that is the
  right failure mode, but it is still a checklist item.
- **Hot-path cost of the site global** — small but nonzero; bounded by the
  benchmark gate above, with a named fallback.
- **The interpreter refactor is wide**: every abort-class `format!` site moves
  to templates. Mechanical, but it touches the most-trusted artifact in the
  repo; the message-pinned tests (79 of them) are the net.
- Strict message parity makes *improving* an error message a two-backend,
  one-commit change forever. That is the point, but it is a real tax on
  wording tweaks.

## Prior art

- **Rust's `panic!` on wasm32**: the exact same problem (panics become bare
  `unreachable` without a handler) and the same solution shape — an imported
  hook (`panic_hook` / `console_error_panic_hook`) that carries the message
  out before the trap.
- **wasmtime's trap backtraces + the name section** — the frames half of this
  design, already shipped here for `WITCHY_WASM_BACKTRACE` (see the coven
  publish OOB debugging record).
- **RFC-0023's checked-heap imports** — the in-repo precedent that a
  no-authority diagnostic import may be always-linked without violating
  deny-by-omission.
