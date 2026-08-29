---
rfc: 0164
title: "Compiler Performance: Arena Allocation, Type Interning, and Incremental Reuse"
status: proposed
created: 2026-08-29
superseded-by:
tracking: "Proposes the compile-time performance program that follows RFC-0146's
  runtime-performance program. Distinct axis: this is about how fast `witchy
  build`/`check`/the test suite runs the COMPILER, not how fast a compiled
  witchy program runs. Grounded in `sample`-profiled evidence gathered while
  landing the merge-gate cost audit and the RFC-0111 layout-resolver
  memoization fix; raw profiles are not committed, the findings that motivated
  each phase are."
---

# RFC-0164: Compiler Performance — Arena Allocation, Type Interning, and Incremental Reuse

## Summary

A `sample` profile of an ordinary (non-`mode opt`) compile found that
**~45-50% of total CPU is in the system allocator** (`malloc`/`free`/`memset`),
not in any compiler algorithm, and that `typeck::Ty` clones and drops are
concentrated hot leaves. A separate profile of a `mode opt` compile found
**26% of total CPU** in one redundant recursive type-graph walk (fixed this
session: memoize three pure functions in
`crates/witchy-lower/src/codegen/assembly.rs`, -55% on that path). Landing
mimalloc as the global allocator and routing `cc-rs` builds through `sccache`
measured **~30-45% less wall time** on the allocation-heavy path (interleaved
A/B; this machine's load swings too much for a naive before/after to be
trustworthy) — but that makes each allocation *cheaper*, it does not make the
compiler do *fewer* of them.

This RFC proposes the next tier: change what the compiler's own internal data
structures look like, following the same architecture that makes the Go, Zig,
and Jai compilers fast. None of them are fast because of a cleverer algorithm
in the front end; they are fast because their own implementation avoids
per-node heap allocation and gives types/symbols an identity that is cheap to
compare, hash, and copy.

## Motivation: what we measured this session

- **RFC-0111 layout resolver** (`crates/witchy-lower/src/codegen/assembly.rs`):
  `contains_bool_list`, `type_contains_dynamic_list`, and
  `type_requests_specialized_layout` were each re-derived from scratch at
  every node of `collect_specialized_layout_requests`'s type-tree walk, with
  no caching across calls — including re-expanding named-type definitions
  (std's own recursive scheduler types) on every visit. Fixed by memoizing
  each at its public entry point (sound because every entry point starts with
  an empty substitution/cycle-guard state, so the result is a pure function of
  `(ty, resolver)`). Measured: 10.8s → 4.8s on a representative `mode opt`
  compile, min-of-3.
- **Allocator overhead**: a `sample` profile of an ordinary compile (a small
  program with a few records, a match, a couple of `list.map`/`list.fold`
  calls) showed leaf-sample time roughly: 10% `_xzm_free`, 7% `_xzm_xzone_malloc`,
  6% a deduplicated malloc-family symbol, 5% drop-glue for `witchy_wir::WirExpr`,
  4% drop-glue for `typeck::Ty`, plus `Ty::clone`/`Expr::clone`/`Type::clone`
  each appearing as their own hot leaves. Summed, allocator-family functions
  account for roughly **45-50%** of total on-CPU time.
- **`typeck::Ty`** (`crates/witchy-types/src/typeck.rs`, ~9.6k lines, tracked
  as oversized debt in `spec/architecture-ledger.md`) is a large `enum`
  carrying `String`/`Vec`/`Box` payloads in most variants, `#[derive(Clone)]`.
  The RFC-0046 annotate/mono fixpoint (`lower_with` →
  `annotate_with_conversion_fns` → `run_check_selected`) re-walks and
  re-clones types across multiple rounds until convergence — every round pays
  the deep-clone cost again for types that, in the common case, did not
  change.
- **mimalloc + sccache CC routing** (landed this session): mimalloc as the
  global allocator (`src/lib.rs`, one `#[global_allocator]` covering the CLI
  binary and every integration test binary that links the `witchy` lib), plus
  routing `cc-rs`'s C/C++ compiler through `sccache` so this repo's per-agent
  `target-<agent>` convention doesn't repay every native dependency's C
  compile from scratch. Measured with an interleaved A/B (alternating
  binaries built with each allocator, same fixture, same few minutes — a
  same-binary before/after is not trustworthy on this machine, whose load
  average swings 10x within seconds): mimalloc won 9/9 rounds, ~30-45% less
  wall time. This is real and worth keeping, but it treats the allocation
  count as fixed and makes each one faster — it is a multiplier on top of
  whatever the true allocation count is, not a fix to the count itself.

## Prior art: how Go, Zig, and Jai are actually fast

Grounded in each project's own public design writing, not folklore:

- **Go**: package-level parallel compilation — the package DAG is the
  compiler's own unit of independent, cacheable work, so unrelated packages
  compile concurrently and a content-hash-keyed build cache skips
  recompiling anything unchanged. The backend (built on a fairly simple SSA
  form) is deliberately tuned for compile speed over peak codegen quality.
- **Zig**: the compiler's own implementation leans on arena/bump allocators
  pervasively — Andrew Kelley has described this as a deliberate, explicit
  policy, not an incidental choice. Types and values live in an
  `InternPool`: a flat, arena-backed table where identity is an integer
  index, so equality is an integer compare, hashing is trivial, and "cloning"
  a type is copying an index, never a deep structural clone. The incremental
  compiler tracks per-declaration dependencies and re-analyzes only what a
  change could have affected, not the whole program. A non-LLVM backend
  exists specifically for Debug builds, trading codegen quality for compile
  speed — the same trade witchy's `mode opt` split already makes at the
  *language* level, but Zig also makes it in *how the compiler itself runs*.
- **Jai** (per Jonathan Blow's public compiler-performance talks): an
  explicit rejection of "one AST node = one heap allocation" in the front
  end; comptime execution goes through a bytecode interpreter rather than
  tree-walking; the compiler's own working set favors batched,
  structure-of-arrays layouts for cache locality. The framing Blow uses is
  direct: the compiler is a program, and programs are fast when written like
  programs, not assembled from individually-boxed, individually-owned tree
  nodes.

**The convergent lesson**: all three attack the same two things, independent
of language or implementation style —

1. **allocation count** (arena/bump allocation, not merely a faster general
   allocator), and
2. **representation identity** (interned handles with O(1) clone/eq/hash,
   not deep-structural values passed by value or by `Box`).

Parallelism and incremental reuse compound on top of that foundation. They do
not substitute for it — parallelizing or caching an allocation-heavy,
deep-clone-heavy compiler still leaves each unit of work expensive.

## Proposal: a phased plan

### Phase 0 — landed this session (baseline this RFC builds on)

- Layout-resolver memoization (assembly.rs), -55% on the `mode opt` path it
  touches.
- mimalloc + sccache CC routing, ~30-45% less wall time on the
  allocation-heavy ordinary-compile path.

Both are real, both are kept. Neither changes the compiler's allocation
*count*, which is why this RFC proposes going further rather than stopping
here.

### Phase 1 — intern `typeck::Ty` (`witchy-types`)

Replace `Ty`'s pervasive deep clones with a cheap, `Copy` handle (`TyId`, an
index into an arena-backed intern pool owned by the `Checker`/type table for
one compile). Structurally-identical types — `Int`, `String`,
`List(Int)`, and every generic instantiation that recurs across a program —
collapse to one entry; comparison and hashing become integer operations;
`.clone()` becomes a `Copy`.

Scoped to the *checked* type representation first, not the AST `Type`
(`witchy-syntax`): the fixpoint's hot loop operates on `Ty`, and this is
where the profiled clone/drop-glue cost concentrates. AST `Type` interning is
a candidate follow-up if evidence after Phase 1 still points there — decide
with a fresh profile, not in advance.

This is the largest lever identified: it directly targets the plurality of
this session's profiled hot leaves (`Ty::clone`, `Ty` drop-glue,
`hashbrown::HashMap<String, Ty, ...>` traffic in the checker's environments).

### Phase 2 — arena-allocate the AST → WIR pipeline (`witchy-lower`, `witchy-wir`)

Bump-allocate WIR nodes for one compile unit and free the arena in bulk at
the end of `compile_source`, instead of individually heap-allocated,
individually-dropped `Vec<Box<...>>` trees (the `WirExpr` drop-glue hotspot
this session's profile found). Sequenced *after* Phase 1: `Ty` interning
changes signatures WIR construction already depends on, so building the WIR
arena around the pre-interning shape would be rework.

### Phase 3 — reduce fixpoint re-computation (`witchy-types::traits::lower_with`)

Investigate whether the RFC-0046 annotate/mono fixpoint can track which
nodes actually changed between rounds and skip re-walking untouched
subtrees, instead of re-running `infer_block_inner` over the whole function
body on every round regardless. Needs its own fresh profile once Phase 1
changes the underlying cost model — clone cost mostly disappears, so the
next-highest leaf becomes visible and should drive the actual design, not
guesswork now.

### Phase 4 (separately scoped, longer horizon) — persistent incremental compilation

`witchy build`/`check` recompiles the whole linked program from scratch on
every invocation. Zig's per-declaration incremental cache and Go's
package-level build cache both avoid exactly this. Different subsystem (the
CLI/linker, not typeck/codegen internals) and a much larger design surface —
named here so it is not lost, not scoped in this RFC. Worth its own RFC once
Phases 1-3 land: interned representations make a persistent cache's
serialization format meaningfully simpler to define than it would be against
today's deep-tree types.

### Phase 5 (separately scoped, longer horizon) — parallel compilation of independent modules

Needs an audit of whether witchy's module-linking model forms a DAG cheaply
parallelizable the way Go compiles independent packages concurrently. Named,
not scoped here.

## Non-goals

- **Not** removing the `*_cap`/`self_*` runtime optimization zoo (RFC-0016 /
  RFC-0051 territory) — that is compiled-*program* runtime performance, a
  different axis from this RFC's compile-time scope.
- **Not** proposing a container-type swap to `hashbrown::HashMap` directly.
  `std::collections::HashMap` has been built on hashbrown's `RawTable`
  internally since ~Rust 1.36; swapping the container type while keeping the
  same (already-adopted) `foldhash` hasher would not be expected to move the
  needle, and this session found no evidence otherwise. The real remaining
  hash-related gap is the still-partial `foldhash` migration itself (roughly
  550+ call sites across several crates still construct a bare
  `std::collections::HashMap`/`HashSet`, defaulting to `SipHash`, found in an
  earlier audit this session) — a smaller, separate, mechanical cleanup, not
  part of this RFC's phases.
- **Not** re-litigating whether RFC-0135's stable-semantics parity tests
  belong in the merge gate — they do; this program does not touch that.

## Acceptance criteria (draft; refine per phase before it starts)

- **Phase 1**: a fresh `sample` profile of the same ordinary-compile fixture
  used this session shows `Ty`-attributable clone/drop-glue time reduced by
  an order of magnitude; full differential/parity corpus green (RFC-0135);
  no observable behavior change — interning is representation-only.
- **Phase 2**: a fresh profile shows the malloc/free leaf share meaningfully
  below the ~45-50% baseline recorded in this RFC; measured on both the
  small fixture used this session and at least one larger real program (std
  itself, or `projects/grimoire`) to confirm the win holds at scale.
- Every phase ships its own before/after `sample` (or equivalent) profile in
  the landing commit — this session's standing practice, not a new
  requirement invented for this RFC.

## Risks

- `Ty`/WIR interning touches most of `witchy-types` and `witchy-lower` — a
  large blast radius on RFC-0135 parity-sensitive code. Each phase needs the
  full differential corpus green, not the smaller scoped test slices this
  session used for the lower-risk memoization and allocator changes.
- Arena allocation trades peak memory for allocation count; needs measuring
  on the largest real programs in the corpus, not just small fixtures, to
  confirm it does not regress a memory-bound case.
- Both `typeck.rs` and `codegen/mod.rs` are already tracked as oversized
  files in `spec/architecture-ledger.md`. An interning refactor touches a lot
  of `typeck.rs`; coordinate the change with any planned split rather than
  writing more code into the same monolith and compounding that debt
  silently.

## Tracking

Each phase gets its own worktree and its own before/after measurement,
independent of the others — per this session's practice, not a new process.
Phase 1 is the recommended starting point: it is the single change most
directly supported by this session's profiling evidence, and Phases 2-3
depend on the type representation it produces.
