---
rfc: 0045
title: Aborts carry their message on the compiled backend
status: implemented
created: 2026-07-03
predecessors:
  - "0023 (checked heap — the precedent for always-linked, authority-free diagnostic imports)"
  - "0037 (correctness harness — the differential gate this extends to message parity)"
  - "0044 (std error policy — rule 3 requires 'legibly on both backends'; this is 'legibly')"
tracking:
---

> **Implementation update (2026-07-10, branch
> `fix/bug107-strict-abort-diagnostics`).** The original message channel is
> extended to complete diagnostic parity. A single exported mutable `i64`
> (`__witchy_diagnostic_site`) carries an interned lexical-function pointer in its
> high 32 bits and the source line in its low 32 bits (§c). Codegen threads the
> site through helpers from the lowered WIR's actual abort dependencies, not from
> a second hand-maintained operation list; the global changes only on an abort
> edge. Integer division/modulo traps are routed through
> shared templates, closures retain their lexical diagnostic owner on both
> backends, and the differential harness requires byte-for-byte equality for
> every both-error result (§d). The browser's compiled-abort matrix pins every
> pure template and the packed-site ABI (§e).
>
> This implementation deliberately does **not** emit `witchy.sites` or
> `witchy.templates` custom sections. The packed global avoids a site table; Rust
> hosts share `DiagTemplate` directly, while the dependency-free JavaScript host
> keeps a small mirrored renderer whose complete matrix is the drift detector.
> This is a representation amendment, not a behavioral deferral. The browser
> compiler/runtime matrix is green. A 20M-call dynamic `list.at` kernel measured
> +0.35% against the existing release compiler; after preserving raw Wasm for
> provably nontrapping literal divisors, `list.at(xs, i % 4)` measured a 0.987
> median ratio over 25 alternating Node/V8 samples. Native runtime snapshots and
> the 14-case browser-host matrix are green; the development host still needs
> its documented dyld launch workaround until the toolchain repair lands.

# RFC-0045: Aborts carry their message on the compiled backend

## Summary

Every runtime abort on the compiled backend — out-of-bounds index,
`string.to_int` on junk, NaN ordering, `fail("…")` — surfaces as the same bare
`wasm trap: wasm `unreachable` instruction executed`. The interpreter says
``runtime error: `p20.test_fail`, line 4: the reason``; the backend users
actually run says nothing. `fail(msg)` literally **evaluates and drops its
message** ([`crates/witchy-lower/src/codegen/builtins.rs:358-368`](../crates/witchy-lower/src/codegen/builtins.rs), comment:
"evaluate (and drop) the message, then `unreachable`"). This RFC adds one
always-linked, authority-free host import, `__witchy_abort`, called before the
trap; routes every abort through it with the interpreter's exact diagnostic; and
promotes error parity from "both error" to byte-for-byte diagnostic equality in
the differential harness.

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
- `WITCHY_WASM_BACKTRACE=1` exists ([`src/main.rs:1811-1816`](../src/main.rs); the emitted name
  section — [`crates/witchy-wir/src/wir_encode.rs:295-309`](../crates/witchy-wir/src/wir_encode.rs) — makes frames
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
__witchy_abort(template: i32, a: i64, b: i64, str_ptr: i32)
```

The host formats the message (below) and returns a trap (`bail!`), so the
call never returns; codegen still emits `unreachable` after it so the emitted
function stays stack-typed even if a host ignored the contract.

**Why always linked, and why this doesn't violate deny-by-omission.** The
precedent is explicit in `crates/witchy-runtime/src/runtime.rs:562-568`: the
RFC-0023 checked-heap imports "are not capabilities — they grant no authority
… so they are always defined." `__witchy_abort` grants strictly less than
those: it cannot read or write outside guest memory; `str_ptr` names an ordinary
witchy string whose length is read from its in-memory header. It cannot return
data to the guest (it never returns), and its only effect is to *terminate
execution with a label* — an ability the guest already has via `unreachable`.
It is a diagnostic channel to the host's own stderr, not an authority.
Correspondingly it is excluded from the
capability footprint exactly as `heap_register` is: `witchy caps` and the
coven widening gate never see it.

### (b) Message templates — shared in Rust, executable mirror in JavaScript

The message *text* must match the interpreter's byte-for-byte. The Rust
implementation has one owner: `witchy-syntax/src/diag.rs` (every core crate
already depends on witchy-syntax), with a `DiagTemplate` enum per abort class
and one `render(&self, a, b, s) -> String` — e.g.

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

### (c) Site information — one packed global, derived from lowered WIR

The interpreter prefixes ``runtime error: `module.func`, line N: …``
(`rt_at_line`, `crates/witchy-interp/src/interpreter.rs`). Compiled modules that
can reach a host-backed failure export one mutable `i64` global named
`__witchy_diagnostic_site`. Its high 32 bits are a pointer to an interned static
witchy string containing the lexical function name; its low 32 bits are the
source line. Zero means unavailable. Routed aborts read the packed value in the
host import; other host errors are contextualized when `Vm::run` surfaces them.
The four-argument abort-import signature therefore stays unchanged.

Codegen already has a line for each source statement. After lowering one
statement, it asks the WIR artifact whether that statement directly reaches a
host import or does so through the helper registry's transitive `import_deps`.
If so, every host-backed helper call receives the packed site as
a final `i64` argument. Module assembly applies the same registry-derived rule
to helper-to-helper calls and adds the site parameter to those helpers. A helper
writes the exported global only immediately before its actual host edge.

This placement is compositional. Nested arguments finish before the outer
helper receives its site; a successful nested call and an async interleave do
not mutate diagnostic state. The WIR helper registry remains the single owner
of host reachability, with no parallel source-operation list or custom section.

Failing callees publish their more precise innermost statement; successful
functions and closures restore the caller's complete diagnostic context in the
interpreter. Lifted lambdas use their lexical owner, and that owner participates
in both closure cache keys; the interpreter stores it in closure values.
Escaping closures therefore report the function that contains their source,
not whichever function happened to invoke them.

### (d) Message parity becomes a testable property

`parity_check`'s `(Err(i), Err(c))` arm uses exact string equality. The complete
diagnostic must match, including `runtime error:`, lexical function, source line,
message class, and dynamic values. This strict rule applies to every both-error
pair, not only routed runtime aborts: a bare Wasm trap, missing source location,
or backend-specific rejection is a divergence. The host's `bail!` root cause is
the string `run_wasm_bytes` exposes, without wasmtime's wrapper.

This closes the occurrence-vs-semantics gap: a compiled backend that traps at
the wrong site or for the wrong reason now diverges loudly.

### (e) The browser shim

The pure-compute shim (`web/witchy-runtime/witchy-runtime.mjs`) provides
`__witchy_abort` in its non-capability import set (it sits beside `print` in the
pure tier, under the same authority argument as (a)). The handler reads the
packed exported site, renders the template, and throws a JS `Error` whose
`.message` is the complete `runtime error: …` diagnostic.

JavaScript deliberately mirrors the small `DiagTemplate::render` switch rather
than shipping a custom format-string section and parser in every module. A
compiled browser test exercises every pure template, dynamic hole, nested named
function, and escaping lambda against committed complete-message oracles. The
capability-only `SecretRequired` template is pinned on the native path; the pure
browser cannot instantiate the capability program that reaches it.

### (f) `WITCHY_WASM_BACKTRACE` stays, and gets documented

Unchanged semantics: set it to also dump the full named-frame backtrace under
the message. It becomes the documented "frames add-on" in the spec's
diagnostics section and `witchy --help`'s environment table (today it is
documented nowhere user-facing).

### Binary-size cost

Message bodies remain host-side templates: **zero guest bytes** per static
message. There is no site table. A module that can reach a host failure gains one
exported mutable `i64`, interned lexical-owner strings, and one trailing `i64`
site argument at each host-backed helper call. The global write executes only
immediately before the host edge. Bounds-elided operations have no host dependency
and pay neither cost. `fail`'s dynamic
strings were already in the data segment. The extra constant argument on hot
calls such as `list.at` is the measurable risk; benchmark results must stay
within noise before merge. Static nonzero remainder divisors, and division
divisors other than `0`/`-1`, retain raw Wasm operations because the compiler can
prove those cases cannot reach a diagnostic edge.

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
  but duplicates every message body per binary and forfeits the shared Rust
  renderer. Stable template ids with host rendering are smaller.
- **`witchy.sites` / `witchy.templates` custom sections.** They avoid the small
  JavaScript mirror, but require two metadata formats and parsers in every host
  while still needing mutable call-site state for shared helpers. The packed
  global plus an executable browser matrix is the smaller 0.1 contract.
- **DWARF/source-map debug info.** Gives lines "for free" in devtools-class
  hosts, but is large, unsupported in the pure shim, and still carries no
  dynamic message. Complementary at best; not this.
- **Do nothing.** The status quo fails RFC-0044 rule 3, keeps `witchy test`
  the only place users see real errors, and leaves the harness's error arm
  vacuous. Rejected.

## Drawbacks

- **A new ABI surface.** `__witchy_abort`, stable template ids, and the packed
  `__witchy_diagnostic_site` global are part of the compiled contract. Mitigated by
  the exact differential gate and browser matrix: compiler/host skew fails
  immediately.
- **Every host must implement it.** wasmtime runtime, browser shim, and any
  future embedder. The always-linked choice means an embedder that forgets it
  fails at instantiation (loud), not at first abort (silent) — that is the
  right failure mode, but it is still a checklist item.
- **Hot-path cost of site propagation** — one extra `i64` constant argument on
  a host-backed helper call; the global write occurs only at the host edge. The argument
  cost is small but nonzero and benchmark-gated.
- **A small host mirror remains in JavaScript.** Native code and the interpreter
  share the Rust renderer; the dependency-free browser host duplicates the
  template switch. Its compiled-abort matrix is therefore part of the ABI gate.
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
- **wasmtime's trap backtraces + the name section** — retained as the optional
  full-frame debugging layer under `WITCHY_WASM_BACKTRACE`; source identity for
  the primary diagnostic uses the packed site instead.
- **RFC-0023's checked-heap imports** — the in-repo precedent that a
  no-authority diagnostic import may be always-linked without violating
  deny-by-omission.
