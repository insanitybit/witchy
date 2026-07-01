---
rfc: 0037
title: A best-in-class correctness harness — differential, sanitized, metamorphic, coverage-guided
status: proposed
created: 2026-07-01
predecessors:
  - "0023 (checked heap — the redzone sanitizer this extends)"
  - "0016 / 0035 (RC floor — the reclamation whose UAFs this must catch)"
  - "spec/architecture.md (twin backends: the interpreter oracle vs compiled WASM)"
tracking: "A free-at-overwrite use-after-free lived ~2 days across the entire life of the
  rc-floor lever (shipped 2bd94f7, fixed e41f6d3). It was memory-safe on the default path
  (rc-floor is opt-in) and hid from every test. This RFC makes the harness best-in-class so
  that class — and OOB, type confusion, leaks, and optimization-order bugs — cannot hide."
---

# RFC-0037: A best-in-class correctness harness

## Summary

witchy's greatest correctness asset is its shape: **twin backends with a memory-safe
oracle.** The tree-walking interpreter is written in memory-safe Rust and is the reference
semantics; the compiled-WASM path must match it byte-for-byte. Differential testing against
that oracle is the primary weapon, and it is strong. But a recent use-after-free (a
free-at-overwrite pass freeing a buffer that aliased a borrowed param) lived for ~2 days —
memory-safe on the default path, invisible to every test — and was found only by a *manual*
example sweep under the opt-in lever. That is a harness gap, not bad luck.

This RFC specifies a layered upgrade so critical errors — use-after-free, out-of-bounds,
type confusion, leaks, and optimization-order divergence — are caught **automatically,
early, and minimally**. The design leans on witchy's specific strengths: the memory-safe
oracle, the **acyclic value-semantics heap** (RC is complete → a leak is *always* a bug),
the universal `[rc][size]` header ([RFC-0035](0035-completing-the-rc-floor.md)) as a place
to hang type tags, and the capability model as a fuzzable invariant.

## Baseline — what the harness already has

- **Differential fuzzer** (`tests/differential_fuzz.rs`): generates random well-typed
  programs, runs `witchy parity` (interp vs compiled), fails on `DIVERGE` or a host crash,
  under `WITCHY_HEAP_CHECK=1` (redzones). Runs one opt config: `WITCHY_OPT=all,-wasm-opt`.
- **Oracle example sweeps** (`verify_file`): every example agrees interp-vs-compiled — under
  the default opts (`every_compilable_example_agrees_on_both_backends`) and, since the UAF,
  under **every opt-in lever** (`assert_examples_agree_under`).
- **Metamorphic force-copy** (`examples_agree_under_inplace_and_forced_copy`): in-place ==
  forced-copy output, NO oracle.
- **Checked heap / redzones** ([RFC-0023](0023-checked-heap.md)): a poisoned 8-byte redzone
  after each allocation; a post-run sweep proves no overrun.
- **`__witchy_live_cells`** (RFC-0035): a leak metric.
- The heap-type-matrix corpus (`rc_corpus_*`).

## The gaps (root-caused from the UAF, plus general)

- **G1 — the generator's grammar is narrow.** It emits only a `main` of independent `print`
  statements over three fixed record types. It generates **no user functions** (so no
  parameters, no `let`/`var`/`own` conventions, no `move`, no recursion, no cross-function
  aliasing), and it *deliberately avoids* self-referential reassignment (kind 22's comment:
  "never read the var → no self-ref bail"). The UAF's shape — `var s = param; s = op(s)` —
  is **outside the grammar**, so it could never be generated.
- **G2 — one opt config is fuzzed.** `all,-wasm-opt`. A bug that appears only under a
  different lever combination (or a single lever) is unexercised.
- **G3 — redzones catch OVERRUN, not USE-AFTER-FREE.** The UAF overwrote a *live object's
  own data* via a reused freed block; the trailing redzone was intact. The checked heap has
  no freed-block poisoning or quarantine, so reuse-after-free is invisible to it. This is why
  the fuzzer (which runs under `WITCHY_HEAP_CHECK`) still would not have caught it even if the
  generator had produced the shape and it had corrupted a value the program later re-read
  *before* the redzone sweep.
- **G4 — oracle-dependence.** If the interpreter is *also* wrong, differential testing agrees
  on a wrong answer. There is no oracle-independent check of *correctness* (only of
  *agreement* and *memory-safety-via-redzone*).
- **G5 — no coverage feedback.** The generator is blind to which compiler/runtime edges it
  exercises; it cannot steer toward unexplored paths.
- **G6 — no minimization.** A failing seed is a 160-statement program; the human bisects.

## The proposal — layered defenses

Ordered by how directly each would have caught the UAF; see "Rollout" for priority.

### 1. Generator overhaul — grammar-complete, structure-aware, self-checking

The single highest-value change. Extend the generator (keep it type-directed so output stays
well-typed) to emit the shapes it currently can't:

- **User functions** with every parameter convention (`let`/`var`/`own`) and `move` at call
  sites; multiple params; returns of every type incl. **tuples of owned buffers** (the
  RFC-0036 executor shape); **recursion** (direct and mutual).
- **Self-referential reassignment** `x = f(x, …)` and **alias-init** `var s = y` / `var s = r.field`
  — the exact class that hid, plus the `own`/`var` param aliasing (`fn f(p): var s = p; s = op(s)`).
- **Closures/lambdas** (capturing + non-capturing), passed as args and stored.
- **User ADTs + `match`** (incl. nested `match`, `if let`, `while let`, heap payloads) and
  **channels/async** (behind the concurrency imports) so the executor path is fuzzed.
- **A grammar-coverage meta-assertion:** track which AST node kinds the generator emitted
  across a run and fail if any reachable kind was *never* produced — turning "did we cover the
  grammar" from an assumption into a test. (This alone would have flagged "we never generate
  user functions.")

### 2. Cross-lever differential — the strongest oracle-free net

Generalize the force-copy metamorphic to **every lever**: the *same* program, compiled under
any two opt configurations, must produce identical output. Apply it to (a) the fuzzer — run
each generated program under a set of configs and diff the outputs pairwise; and (b) the
example sweep — already per-lever, add pairwise. This needs no oracle and no `WITCHY_HEAP_CHECK`:
**any lever that changes observable behavior is a bug, full stop.** The UAF would have been a
`default` vs `rc-floor` output divergence on any program with the alias-init shape.

### 3. Sanitizer modes — catch corruption at the source

Debug-only instrumentation that turns silent corruption into an immediate, localized trap:

- **Use-after-free sanitizer (freed-block poison + quarantine).** Under a `WITCHY_UAF_CHECK`
  build, `$rc_free` (a) fills the freed block with a trap pattern and (b) does **not** reuse
  it immediately — it goes to a delay queue, so a stale reader reads poison (→ divergence /
  a tagged trap) instead of still-valid bytes. Optionally a shadow map marks freed addresses;
  any load/store to a shadowed address traps with the free site. **This directly catches the
  class the redzone missed** — a use-after-free of a reused block.
- **Underrun redzones.** Add a poisoned redzone *before* each object too (currently only
  trailing) — catches header/underrun writes (the dict index word, the `[rc][size]` header).
- **Type-confusion sanitizer.** We now have a per-object header (RFC-0035). In a debug build,
  write a **type tag** into it at allocation; on every typed access (`.field`, `list.at`,
  `match`, `$rcopy`, marshaling) assert the tag matches the static expectation. Catches a
  wrong-shape read / a layout (`unbox`/`packed`) mismatch at the access, not three
  statements later.
- **Leak/`live_cells` assertion.** Because the value-semantics heap is **acyclic**, RC is
  complete: a program that drops all its roots must end at `live_cells == 0`. Add a mode that
  asserts it for such programs under rc-floor — a leak *or* a missed drop is then a test
  failure (and an *over*-drop shows up as a UAF/divergence). This is a witchy-specific
  invariant most languages can't assert.

### 4. Metamorphic & property testing — oracle-independent correctness

Beyond *agreement*, check *correctness* without trusting the interpreter:

- **Algebraic stdlib properties** run on both backends AND checked against the law:
  `reverse ∘ reverse == id`; `sort` idempotent + a permutation of its input; `len(a ++ b) ==
  len(a) + len(b)`; `dict.get(dict.insert(d, k, v), k) == Some(v)`; `parse ∘ fmt ∘ parse ==
  parse`; `decode ∘ encode == id`. A law violation is a bug even if both backends agree.
- **Semantics-preserving input transforms.** A program and a transform that must not change
  its output — reorder independent statements, wrap a value in an identity function, insert a
  dead binding, α-rename — must agree. Catches optimization-order, aliasing, and
  dead-code-interaction bugs.

### 5. Coverage-guided fuzzing

Instrument the compiler + runtime (Rust `-C instrument-coverage`, or a lightweight edge
counter) and feed coverage back into the generator (libFuzzer/AFL-style but **structure-aware**
via the grammar of §1): keep and mutate inputs that reach new edges; persist a growing corpus
seeded from the examples, `std`, and the heap-type-matrix. Turns blind random search into a
directed one that reaches the rare paths (the `unbox` flat-read site, the executor's nested
match, the cold `list_set_cap` copy path).

### 6. Minimization (shrinking)

On any failing seed, automatically shrink — drop statements/functions, simplify expressions,
remove params — re-running the differential/sanitizer check until a minimal reproducer
remains, and report *that*. A 5-line repro instead of a 160-statement seed.

### 7. Host-level UB detection

The compiler and runtime are Rust. Run the suite under **Miri** (and/or ASan on the wasmtime
embedding) on a schedule — catches undefined behavior *in the toolchain itself* (a different
class than guest bugs: a bad `unsafe`, an uninit read, a data race), which no differential
guest test can see.

### 8. Bounded-exhaustive small-scope testing

Enumerate **all** well-typed programs up to a small size bound (a handful of statements, a
tiny function set, small literals) and run each through the differential + sanitizer checks.
The small-scope hypothesis — most bugs manifest on small inputs — means this deterministically
covers shapes random search reaches only by luck. A bounded enumeration that includes
"function with a `var` local aliasing a param, then reassigned" would have hit the UAF on
*every* run, not eventually.

## How each layer catches the classes we care about

| Error class | Caught by |
|---|---|
| **Use-after-free** (this bug) | Generator §1 (produces the shape) → cross-lever §2 diverges, OR UAF sanitizer §3 traps at the reuse, OR bounded-exhaustive §8 hits it deterministically |
| **Out-of-bounds / overrun** | redzones (have) + underrun redzones §3; differential trap-vs-value |
| **Type confusion / layout mismatch** (`unbox`/`packed`) | type-tag sanitizer §3; cross-lever §2 (unbox vs boxed); per-lever sweep (have) |
| **Leaks / missed or double drops** | `live_cells==0` assertion §3 (acyclic-heap invariant) |
| **Optimization-order / aliasing** | cross-lever differential §2; semantics-preserving transforms §4 |
| **Wrong results (oracle also wrong)** | algebraic properties §4 |
| **Toolchain UB** | Miri/ASan §7 |
| **Capability-enforcement holes** | fuzz capability footprints: a program demanding a cap it wasn't granted must be rejected *identically* on both backends (extends §1/§2 into the security model) |

## Rollout / prioritization

- **P0 — cheap, directly catches this class.** Cross-lever differential (§2, generalize the
  force-copy test to all levers, in both the fuzzer and the sweep) + generator user-functions
  with param aliasing and self-referential reassignment (§1). Days, not weeks; catches the UAF
  class immediately with no new infrastructure.
- **P1.** UAF freed-block sanitizer (§3) + minimization (§6). The sanitizer localizes any
  future reclamation bug to its free site.
- **P2.** Coverage-guided fuzzing (§5) + type-tag sanitizer (§3) + `live_cells==0` mode (§3) +
  algebraic properties (§4).
- **P3.** Miri in the `--full` gate (§7) + bounded-exhaustive enumeration (§8) + metamorphic
  transforms (§4).

## Implementation status

Shipped (in `tests/differential_fuzz.rs`, `crates/witchy-wir/src/wir_helpers/mod.rs`,
`scripts/check.sh`):

- **§2 cross-lever differential + §1 grammar-complete generator (P0).** The fuzzer runs every
  program under a config set (`none`, default, `rc-floor`, `unbox`, `all,-wasm-opt`); the
  generator emits a risky-shape helper library (user functions with `let`/`own` params,
  closures, direct + mutual recursion, a tuple-of-owned-buffers return) and statement kinds for
  the exact class that hid the UAF — a local `var` alias-initing a borrowed param/local then
  self-referentially reassigning, with the shared value re-read as a trip-wire. A
  grammar-coverage meta-assertion fails if any kind is never emitted. Counts are
  env-overridable. Teeth-tested: neutralizing the real `escape.rs` alias-init exclusion makes
  the generated shapes DIVERGE under `rc-floor`.
- **§3 use-after-free sanitizer (`WITCHY_UAF_CHECK=1`).** `$rc_free` fills the freed payload
  with a `0xDEADBEEF` poison pattern (size-guarded against a pre-rc-header buffer) and then
  relinks the block for reuse exactly as before — so it is STRICTLY ADDITIVE: reuse-corruption
  detection is preserved unchanged, and a stale read of an *un-reused* block now reads poison
  deterministically. Verified zero false positives (a correct compiler agrees across the fuzzer
  and 57 examples under `rc-floor`+`UAF_CHECK`), no regression (every previously-caught UAF
  still DIVERGES), locked in by `uaf_sanitizer_is_false_positive_free` and a wider `--full` sweep.
- **§4 algebraic / metamorphic properties (P2).** `metamorphic_property_laws` fuzzes random data
  through fixed stdlib laws (`reverse∘reverse == id`, `sort` idempotent + length-preserving,
  `len(a ++ b) == len a + len b`, `dict.get(insert(d,k,v),k) == v`, string concat length,
  `string.reverse∘reverse == id`, `len(repeat(s,n)) == len(s)·n`) and checks the printed VALUE,
  not just backend agreement — so a law that is `false` on *both* backends (an oracle that is
  itself wrong, gap G4) is still a failure. The only layer that does not trust the interpreter.
- **§4 semantics-preserving transform — dead-alloc invariant (P3).** `metamorphic_dead_alloc_invariant`
  runs a program of alias/self-ref units and a TWIN that interleaves DEAD (unused) heap
  allocations; the two must print identically under each lever. Inserting unused allocations
  cannot change meaning but DOES change allocation order / free-list state, so a reclamation
  bug that reuses a freed block differently between the variants diverges. Oracle-independent
  (base vs twin, same backend), zero false positives on correct code. A regression net for the
  reuse-order-sensitive fragile class; the *current* free-at-overwrite bug class does not trigger
  it because that class self-reuses its freed block (no window for a dead alloc to intercept).
- **§6 minimization / shrinking (P1).** On any DIVERGE or host crash the fuzzer now greedily
  delta-debugs the failing program (drop a line while the failure persists, to a fixpoint,
  under a call budget) and reports the MINIMAL reproducer instead of the 100-statement seed.
  A structural line whose removal ends the failure is kept automatically. Unit-tested via a
  synthetic oracle (`shrink_reduces_to_minimal_repro`); demonstrated end-to-end on the escape.rs
  mutant, where a 58-line failing program shrank to the 5-line use-after-free unit.

Honest scoping finding from the teeth-tests: on RANDOM programs the plain cross-lever net catches
a reuse-after-free only when the freed block's *offset-0* word is observably corrupted — which,
for the free-at-overwrite class, happens reliably because `$rc_free` clobbers offset 0 with the
freelist link (a string's length → wrong value/trap). The sanitizer's marginal, deterministic
addition is the harder class: a stale read at *offset 4..* of an un-reused block, and cases where
a non-zero freelist head leaves offset 0 looking valid. The remaining items (minimization §6,
type-tag + `live_cells==0` sanitizers §3, coverage-guided fuzzing §5, semantics-preserving
transforms §4, Miri §7, bounded-exhaustive §8) are unstarted.

## Integration

`./scripts/check.sh` is the green gate (there is no external CI). Fold the P0/P1 additions
into `--fast`; the heavier sweeps (coverage-guided long runs, Miri, bounded-exhaustive) into
`--full` and/or a scheduled long-fuzz job. Keep sanitizer modes **debug-only** behind env
flags (`WITCHY_HEAP_CHECK`, a new `WITCHY_UAF_CHECK`, `WITCHY_TYPE_CHECK`) so production codegen
is untouched.

## Non-goals

- Formal verification / a proof of the compiler (out of scope; this is empirical assurance).
- Making sanitizer modes production-default (they trade speed for detection).
- Replacing the interpreter oracle (it stays the reference; §4 only *reduces* reliance on it).
