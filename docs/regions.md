# `region:` — user-controlled temporary allocation scopes

A `region:` block gives short-term allocations an explicit lifetime: everything
allocated inside dies at the block's end, and **the block's value is what
escapes**. It is the user-facing member of the reclamation family the compiler
already applies implicitly (per-message actor resets, escape-free loop
watermarks, program exit).

```witchy
let summary = region:
    let parsed = parse_huge_input(text)   // big intermediates, freed at `end`
    summarize(parsed)                      // only the block's value survives
```

**Ground rule: a region never changes observable behavior — only when memory
is reclaimed.** The interpreter treats it as a plain block; the WASM tier may
even skip the machinery entirely (e.g. watermark pool exhausted) and remain
correct. Every phase below preserves this, which is also what makes the
feature parity-testable for free.

## Semantics

1. **Value escape by copy-out.** At the block's end, the value is deep-copied
   down to the region's entry watermark and the heap resets just past it.
2. **Watermark short-circuit.** While copying, any sub-value whose pointer is
   *below* the watermark is already parent-side and is returned as-is, no
   traversal. Sound because of rule 3. Consequences: returning a pre-existing
   value copies nothing; a mixed result copies only its region-born bytes —
   the theoretical minimum short of full region inference.
3. **No outer pointer assignments.** Assigning a variable declared outside the
   region is a TYPE ERROR unless the variable's type is scalar (Int, Float,
   Bool, Duration). This includes the linear-update forms
   (`acc = push(acc, x)`): they are assignments. Host-side state (actor
   fields) is exempt — those writes copy content out by construction.
   Unlike the loop optimizer (which silently skips the reset), `region:` is
   explicit, so violations are loud, at check time, on both backends.
4. **No `yield` inside a region** (a generator frame outlives the block).
5. **Capability operations are unrestricted** — handles are scalars and the
   authority lives host-side; `send`/`print`/`write` copy content out at the
   call.

## Syntax & AST

- `region:` introduces a block expression, same family as `retain:`/
  `without:`. Optional result ascription: `region -> List(String):` — when
  present, the copy-out shape is guaranteed at check time instead of
  inferred from the tail expression.
- AST: a `region: Option<RegionAnn>` annotation on `Block` (mirroring
  `restrict: Option<CapRestrict>`), `RegionAnn { ty: Option<Type> }`. Riding
  on `Block` means every existing walker (escape scans, fn-ref collection,
  lowering passes, eq/shape machinery) continues to work untouched —
  the same trick `retain`/`without` use.

## Status — 2026-06-11: Phases 1–3 SHIPPED

Phase 1 (2adea75), Phase 2 (536bdc3), Phase 3 (f63b4eb). Measured:
region-vs-no-region bench 48 ms vs 75 ms with flat memory; the passthrough
zero-copy property is asserted in tests via `__region_copy_bytes`. Phase 4
remains in the drawer per its own gate: the counter shows copy-outs moving
only result-sized bytes in every current workload.

## Phases

### Phase 1 — front end (parser, typeck, interpreter, fmt)

- Lexer/parser: `region` keyword, optional `-> Type`, block body; `Block.region`.
- Typecheck: block value typing as normal; ascription unifies with the tail;
  the outer-assignment rule (3) and yield rule (4) enforced HERE so both
  backends reject identically. Reuse the assignment-target scan from the
  codegen loop-reset analysis, lifted to typeck with declared types instead
  of WASM kinds.
- Interpreter: evaluate the block; nothing else.
- `witchy fmt` round-trips `region:` and `region -> T:`.
- Exit: parser/typeck/fmt tests; a parity test that a `region:` program runs
  identically on both backends (the WASM side still UNoptimized — plain
  block — proving the never-changes-behavior rule before any machinery).

### Phase 2 — WASM reclamation (watermark + copy-out)

- Watermark: reuse the loop pool (`$__witchy_wm_N`, shared `wm_level`
  nesting budget). Pool exhausted → compile as a plain block (sound, rule 0).
- Copy-out: memoized per-shape `$rcopy_<shape>` helpers generated like the
  `eq_`/`ts_` families from the same shape machinery:
  - scalars: identity (never reach a helper);
  - String: byte copy;
  - List: spine copy + per-element recursion;
  - Tuple/Record/ADT: header + per-slot recursion (reserve-before-generate
    cycle safety makes recursive ADTs work; runtime recursion = structure
    depth);
  - Dict: count + entries (key/value recursion) and the hidden index word
    written as 0 — the index points region-side and must not survive; it
    rebuilds on the next owned growth.
  - Every helper starts with the short-circuit: `if ptr < wm: return ptr`.
- Shape source: the ascription when present, else `eq_shape_of` on the tail;
  an unresolvable shape is a loud compile error naming the fix
  (ascribe the region) — the established boundary discipline.
- Emission: capture watermark → compile body → call the root copy helper with
  (value, wm) → reset heap to wm → bump past the copied bytes. The copy
  helpers allocate AT the watermark by running with `$heap` already reset —
  they are ordinary allocating code whose output lands exactly where the
  region began. (No sliding window needed: reset first, then copy, because
  rule 3 guarantees the source bytes above the old heap location are not
  clobbered until copied — copies proceed source-above/dest-below with
  `memory.copy`'s overlap guarantee for the contiguous cases, and fresh
  allocation for structured ones. The plan's first implementation may
  instead copy ABOVE the live data then `memory.copy` the finished block
  down once — simpler to verify, one extra move of result-sized bytes;
  measure, then pick.)
- Exit: parity tests per result shape (string/list/dict/record/recursive ADT,
  nested regions, region-in-loop, loop-in-region, parent-value passthrough);
  a soak proving reclamation (big per-region garbage, constant memory);
  the rejection tests from Phase 1 still hold.

### Phase 3 — instrumentation, bench, docs

- A `(mut i64)` exported global `__region_copy_bytes` accumulated by the copy
  helpers; surfaced by `WITCHY_REGION_STATS=1` after a run. The
  parent-passthrough test asserts ZERO copied bytes for a borrowed result.
- `bench/`: a region workload (parse-summarize shape) vs the no-region
  variant, recording reclamation benefit and copy-out cost.
- Docs: language.md section, capabilities-unaffected note, book performance
  appendix (`region:` joins the ownership knobs), examples/ program.

### Phase 4 — destination inference (IN THE DRAWER, measurement-gated)

The "infer where trivial, copy otherwise" extension. Pre-specified first
target: the block's tail value is an eligible linear-update accumulator with
SCALAR elements (`List(Int)`/`List(Float)`) — its spine allocations route to
a parent-side allocation lane (dual-ended arena: scratch grows down from the
memory top, side flag consulted by allocators, `$ensure` collision checks),
making the copy-out zero for that case. Costs touch every allocating helper;
the copy it eliminates is one contiguous `memory.copy` at memory bandwidth
(~tens of µs per 100k elements). **Build only if `__region_copy_bytes`
shows region exits moving serious volume in real workloads.** The fallback
IS the semantics, so this lands as a pure optimization with zero language
change.

## Non-goals

- Full region inference (MLKit-style) — research-grade, superseded by the
  watermark short-circuit for every measured case.
- Region handles / named regions (`with region as r`) — no operation needs
  the reification; lexical scope is the API.
- Changing the interpreter's memory behavior — Rust ownership already frees.
