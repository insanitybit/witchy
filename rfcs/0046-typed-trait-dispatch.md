---
rfc: 0046
title: "Typed trait dispatch: retire the string shadow type system"
status: implemented
created: 2026-07-03
tracking: "completed 2026-07-10: dispatch and Mono use structured Type; rendered keys are output-only"
predecessors:
  - "language-evolution.md Phase 0 (typed lowering — the TypeTable this completes)"
  - "0042 (module namespaces — the other half of 'facts live in declarations')"
  - "scratch/full-evaluation-2026-07-03.md Theme B (the evidence base)"
---

> 2026-07-03: PARTIALLY implemented and merged (a842068). Dispatch and
> monomorphization now read typeck's TypeTable as the primary source; the
> acceptance criteria hold (iter.collect infers through generic chains; trait
> calls resolve on typed expressions; Eq-bounded list search compiles for record
> element types on WASM; bonus iter.any/iter.all). BUT the string shadow type
> system (`head_type_name` + its parsers/shape tables in traits.rs) was NOT
> deleted — RFC steps 1/4 remain. A follow-up pass must remove the dead shadow
> encoder to finish the RFC (tracked separately). Verified by the two-eval
> triage (scratch/deep-eval/MERGED-TRIAGE.md item 22).
>
> 2026-07-08 bridge fix: the residual `head_type_name` path now derives generic
> constructor results from type declarations instead of hard-coding `Some(x)`.
> This keeps generated helpers coherent after RFC-0069 normalization
> (`show.render(Box(3))`/`json.stringify(Box(3))` see `Box<Int>`), but it is
> still a bridge inside the string-scope fallback. The RFC's final target remains
> deleting the fallback and reading structured checker types directly.
>
> 2026-07-10 structured-dispatch cut: the lexical dispatch pass no longer has a
> string shadow type system. `Ctx` scopes carry `Type`; declaration-driven call
> results, literals, constructors, generic record fields, loop elements, and
> nested constructor/Option/Result/list patterns are judged and substituted as
> structured types. Partially inferred declarations preserve their nominal
> shape (`dict.new() -> Dict(k, v)`) so owner-method resolution does not depend
> on concrete generic arguments. Encoding now occurs only at the terminal
> impl/method-table key and is never parsed by `Ctx`. The redundant string
> capability-return and pattern-payload adapters were deleted. The fast gate is
> green at 1603/1603 with focused representation-boundary tests.
>
> At that checkpoint the RFC remained **in progress**: `Mono` and specialization
> call renaming still used `Scope<String>`. The completion note below supersedes
> that temporary boundary.
>
> 2026-07-10 completion: `Mono`, generated-body walking, specialization call
> renaming, generic argument resolution, body-annotation substitution, and all
> lexical scopes now carry structured `Type`. One structural unifier handles
> named, tuple, function, nested-container, argument-position, and expected-
> result bindings; unresolved actual variables are explicitly non-evidence, so
> later concrete arguments refine them instead of producing order-dependent
> failures. The old `Scope<String>` path, `head_type_name`, encoder/decoder,
> list/generic/tuple parsers, per-shape generic resolver, string substitution,
> constructor/pattern shadow tables, and head-only return map are deleted.
> `traits.rs` shrank by 832 net lines in the completion cut. Canonical rendered
> keys remain only at impl-table, memo, and mangle boundaries and are never
> parsed back into a type; receiver head/module/generic decisions read `Type`
> directly. The fast gate is green at 1603/1603 with warning-denied Clippy plus
> focused interpreter/WASM generic-dispatch regressions. RFC-0046 is implemented.

# RFC-0046: Typed trait dispatch — retire the string shadow type system

> Provisional snippets throughout; code blocks are deliberately **not** tagged
> `witchy` so the doc-examples sweep does not execute pre-implementation code.

## Summary

Trait dispatch and monomorphization today run a **second, best-effort type
system** in `crates/witchy-types/src/traits.rs`, parallel to the real HM
inference in `typeck.rs`: types are encoded as *strings* (`"List<Int>"`,
`"Tuple2<Int,String>"`) and recovered by hand-rolled string parsers and
hardcoded shape tables. This RFC deletes that shadow system and makes dispatch
consume typeck's real `TypeTable` (`typeck.rs:3119-3131`) — the resolved `Ty`
of every expression, which `annotate` already computes and which traits.rs
already threads (but barely uses). One type system, one answer.

This is one of a set (0042–0055) with a single thesis, the same one CLAUDE.md
states as law for optimizations: **facts must live in local, typed
declarations — not global censuses, allowlists, or string heuristics.** Here
the fact is "what type is this expression", the declaration is typeck's
inference judgment, and the string encoding is the heuristic to delete.
[RFC-0043](0043-declared-mutation-writeback.md) applies the thesis to
write-back; [RFC-0050](0050-method-call-generalization.md) to method syntax.

## Motivation

### What the shadow system is (verified against source, 2026-07-03)

`head_type_name` (traits.rs:1035-1112) guesses an expression's type as a
string, consulting `Scope: HashMap<String, String>` (variable → type-name
string, :484), `fn_rets` (function → return-type **head only** — `build_tables`
at :1380-1382 stores `n.clone()` from `Type::Named(n, _)`, **discarding the
generic arguments**, so `string.split`'s `List(String)` becomes bare `"List"`),
and a family of string parsers: `apply_subst` (:1118-1140, whole-token
substitution over the encoded string), `list_elem` (:1142-1144,
`strip_prefix("List<")`), `generic_arg` (:1149-1153, `find('<')`/`rfind('>')`),
`tuple_args` (:1158-1176, a depth-counting comma splitter), `head_of`
(:1356-1358). `type_to_scope_name_d` carries a recursion cap
(`SCOPE_NAME_MAX_DEPTH = 32`, :1318) because a degenerate type would otherwise
overflow the encoder. On top sit the shape tables:

- `builtin_ret` (:1024-1031) knows exactly **four** intrinsic returns
  (`int_to_string`/`__render` → String, `string_length`/`char_count` → Int).
- `recover_generic_call` (:1401-1431) special-cases `list.at` **by name**
  (:1413-1414); every other intrinsic's element type is unrecoverable.
- `bind_type_var` (:1436-1453) binds a return type variable from exactly
  **three** parameter shapes: `a`, `List(a)`, `Option(a)`. A `Dict(k, v)`,
  `Iter(a)`, `Result(a, e)`, or user `Box(a)` parameter contributes nothing.
- `cap_op_return_type` (:1192-1204) hardcodes nine capability-op returns.

The one thing the shadow system does right — documented at :1399-1400 and
:1022-1023 — is its safety invariant: **a wrong guess only ever yields a type
error, never wrong code.** Every consumer falls back to "unresolved" and the
post-mono pass re-finds the failure loudly. That invariant is non-negotiable
and this RFC preserves it (trivially: the TypeTable is not a guess).

### What it costs users (each probed against the shipped binary)

1. **`iter.collect` cannot infer through generic chains.** A helper
   `fn firsts(xs: List(a)) -> List(a): iter.collect(iter.take(...))` fails
   even when the *caller* ascribes `let ys: List(Int) = firsts([1,2,3])` —
   the bounded `FromIterator` template inside the generic body has no concrete
   string to bind. Error site: traits.rs:2462.
2. **Trait calls fail on builtin-call results.**
   `say(console, list.at(parts, 0))` where `parts = string.split("a,b", ",")`
   fails to resolve `Show` — `fn_rets` stripped `List(String)` to `"List"`, so
   `list_elem("List")` is `None`. The same program with a user function of
   declared return type works. Same syntax, opposite outcomes, decided by
   which lookup table happened to keep the type.
3. **`list.unique` does not compile on WASM for record element types** —
   "cannot compile to WASM: … (an interpreter-only feature?)". This is the
   *sole* reason the `cmp.member`/`cmp.index_of`/`cmp.count`/`cmp.unique`
   quadruplet (std/cmp.witchy:230-256) exists: the Eq-bounded cmp forms
   monomorphize where the unbounded list forms cannot (2026-06-28
   investigation, re-verified in the 2026-07-03 evaluation).
4. **std never uses its own Iter library.** Zero combinator/collect use across
   46 std modules (verified: no `import iter` anywhere in std/). The stdlib
   avoids its own lazy layer because inference through it is unreliable — the
   clearest "the library fights its own language" signal we have.
5. **The fallback error misleads.** "cannot infer the result type for
   `iter.collect` — … ascribe the binding (`let x: List(Int) = …`)" names
   `List(Int)` regardless of the actual expected type, and (case 1) the advice
   doesn't work inside a generic body anyway.

The failure mode is structural, not a bug list: every new expression form,
intrinsic, or parameter shape needs its own string-table entry, and the ones
that don't get an entry fail with no location (traits.rs diagnostics carry no
line — `TypeError` is a bare `message: String`, typeck.rs:320-322). The shadow
system will keep growing shape-by-shape forever; that is the definition of the
per-case special-casing CLAUDE.md forbids.

### The wire is already half-connected

typeck's `annotate` (typeck.rs:3184-3195) runs the real checker over the
exact lowered AST and produces a `TypeTable` keyed by expression identity
(`&Expr as *const _ as usize`), populated only where the type is fully
concrete. traits.rs **already receives it**: `Ctx.table` (:527) and
`Mono.table` (:2035) exist, and are consulted at five sites — the
tuple-destructure fallback (:645), the binary-operand fallback (:796), the
method-receiver fallback (:877), the mono argument binder (:2213), and the
mono result-type probe (:2440). Each of these was added as a patch where the
string scope failed; each converts the real `Ty` back into… a scope-name
string (`ty_to_ast` → `type_to_scope_name`), to feed the string machinery.
The architecture question was answered by these patches; this RFC finishes
the inversion: **the table is the primary source and the string encoding is
deleted**, instead of the table being the fifth fallback of the string system.

## Design

### 1. One type representation inside dispatch: `Ty`

`Ctx::type_name`/`Mono`'s scope machinery change signature from
`Option<String>` to `Option<Ty>` (typeck's resolved type). Dispatch keys —
the impl table's receiver, the mono memo key, the specialization mangle —
derive from `Ty` by one function (`ty_head(&Ty) -> &str` and a canonical
`ty_key(&Ty) -> String` used *only* as a map key/mangled suffix, never
re-parsed). The rule that makes this safe to enforce mechanically: **no
function may take a type apart by string inspection.** `list_elem`,
`generic_arg`, `tuple_args`, `apply_subst`, `head_of`-over-encodings, and
`SCOPE_NAME_MAX_DEPTH` are deleted; their callers pattern-match `Ty::List(e)`,
`Ty::Named(n, args)`, `Ty::Tuple(ts)` directly.

### 2. The two-pass architecture stays; the table feeds both

Today's pipeline (traits.rs:280-459) is: a *quiet* pre-mono dispatch pass with
an **empty** table (:311-312), then `annotate` (:339-349), then
monomorphization with the real table, then the loud post-mono dispatch pass.
That order is kept — it exists so annotate sees a checkable module — with one
change: after the quiet pass and annotate, **the loud pass and mono read types
from the table first**, falling back to the local judgment forms that need no
inference (literals, constructors, declared parameters — the cases
`head_type_name` got right). Where the table has no entry (the type has free
variables), dispatch reports "unresolved", exactly today's failure — never a
guess. The safety invariant is preserved by construction: the table *is* the
checker's answer, and absence still means a loud type error downstream.

If a resolution made by the loud pass would enable further typing (it rewrites
`MethodCall` into `Call`), re-annotate and re-run to a fixpoint with a small
bound (two rounds suffices for every known case: one to resolve method calls,
one to type their results). Each round is whole-module and deterministic, so
backend parity is unaffected — this all happens on the single linked AST both
backends consume.

### 3. Deletions

Once dispatch and mono read `Ty`:

- `head_type_name`, `builtin_ret`, `recover_generic_call`, `bind_type_var`,
  `cap_op_return_type`, and the string parsers/encoder listed above are
  **deleted** — the checker already types intrinsic calls, capability ops,
  literals, field reads, and generic returns; the tables were re-deriving what
  `annotate` knows. Deleting them with the suite green is the proof of the
  generalization (the same bar CLAUDE.md sets for the `*_cap`/`self_*` zoo).
- `fn_rets` (the head-only map) is deleted; `fn_sigs` remains only if the
  fixpoint needs declared signatures for not-yet-annotated rounds — expected
  to go too.
- `Scope: HashMap<String, String>` becomes `HashMap<String, Ty>` or is
  subsumed by the table entirely.

Intrinsics that today have no checker judgment (if any surface during
migration) get **typed declarations in the checker** — a signature entry, not
a name-matched return-string.

### 4. Migration order

Each step lands separately, behind the differential suite (`check.sh --fast`
green, the 655-test corpus + the WITCHY_OPT invariance sweep) plus the
acceptance tests below:

1. **Mono first** (it already half-uses the table at :2213/:2440): result-type
   binding (`result_ty`) becomes primary for template resolution; the
   `bind_type_var` shape table is retired when the table + declared signatures
   cover its three shapes. Gate: acceptance (a).
2. **Dispatch receiver typing**: `Ctx::type_name` returns table-first `Ty`.
   Gate: acceptance (b).
3. **Eq/Ord-bounded std collections**: give `list.unique`/`contains`/
   `index_of`/`position` `where a: Eq` bounds; they now monomorphize for
   record types on WASM. Gate: acceptance (c). The deletion of the `cmp.*`
   quadruplet is then a follow-up *cut* owned by
   [RFC-0049](0049-naming-lexicon.md)/[RFC-0044](0044-std-error-policy.md)
   (shapes and bounds land here; names and deletions land there — 0044:136
   already freezes the quadruplet against double churn).
4. **String-machinery deletion** + the misleading fallback error replaced by
   one that names the actual unresolved variable and the actual expected
   shape. Gate: the suite green with the tables gone.
5. **Iter completion and std dogfooding**: `any`/`all`/`min`/`max`/`position`/
   `last`/`scan`/`flatten` on `Iter` (today unwritable because their
   `where`-bounded signatures don't survive inference), then std adopts Iter
   internally where it reads better. Gate: acceptance (d).

### 5. Acceptance tests

(a) `iter.collect` infers through generic chains — the probed `firsts`
    program compiles and runs identically on both backends, without
    ascription at either site.
(b) trait calls resolve on any expression the checker types:
    `say(console, list.at(string.split("a,b", ","), 0))` resolves `Show`;
    a differential test pins it.
(c) `list.unique([Point(1,2), Point(1,2)])` compiles and runs on WASM with a
    record element type, under an `Eq` bound.
(d) the Iter combinator set above exists, is documented, and at least three
    std modules use Iter internally with no output change.
(e) the existing differential suite, the book fences, and the WITCHY_OPT
    sweep stay green at every step — no new backend divergence, no new
    interpreter-only feature.

### 6. Structured spans: a named follow-up, not in scope

`TypeError` has no span fields (typeck.rs:320-322); location is prose-prefixed
by `at_loc`, and the LSP regexes line numbers back out of the message
(`extract_line`, src/lsp.rs:359). Dispatch-on-the-table makes span-carrying
errors *possible* (the table key is the expression; the expression knows its
line), but threading spans through `TypeError` touches every error site and
the LSP protocol surface — that is [RFC-0054](0054-structured-errors.md)'s
compiler-diagnostics half and is deliberately **out of scope** here, so this
RFC's risk stays confined to dispatch. This RFC only commits to not making
locations worse: diagnostics raised from dispatch carry at least the enclosing
function + line that `at_loc` provides today.

## Alternatives

- **Do nothing / keep patching shapes.** Each user-visible failure above has a
  known one-table-entry fix (add `string.split` to a full-signature map, add
  `Dict(k,v)` to `bind_type_var`, …). Rejected: that is the strategy that
  produced the current system — five fallback layers deep, each sound, none
  sufficient, cost paid per-shape forever. The evaluation's Theme B verdict
  ("it will keep growing shape-by-shape until dispatch consumes typeck's real
  TypeTable") is the do-nothing forecast.
- **Full re-architecture: dispatch inside the checker** (resolve trait calls
  during inference, Rust-style obligation solving). Strictly better end state,
  strictly larger blast radius — it rewrites typeck's core loop rather than
  traits.rs's lookups, and loses the two-pass structure the derive/comptime
  passes depend on. The table-threading design gets ~all of the user-visible
  wins while keeping the checker untouched; a checker-integrated solver
  remains open as a future RFC once this one has shrunk traits.rs.
- **Keep strings but centralize the encoding** (one parser module, tested).
  Rejected: the encoding itself is the defect — `fn_rets` dropping generic
  args and the 32-depth cap are not parser bugs, they are lossy-representation
  bugs. A tested lossy channel is still lossy.

## Drawbacks

Honest risk profile — this is the largest open compiler item, and the reports
say so:

- **Blast radius.** traits.rs is 2,566 lines and every generic program in the
  corpus flows through it; monomorphization decisions change which
  specializations exist, which changes emitted WASM function sets. Mitigation
  is the migration order (mono first, deletions last), the differential suite
  as the gate for every step, and the rule that the table is *additive* until
  step 4 — the string fallback stays alive until the suite proves it dead.
- **The fixpoint re-annotate is new machinery** with a compile-time cost
  (annotate is a full check pass; two rounds ≈ 2× check time on trait-heavy
  modules). Acceptable: `witchy check` is interactive-fast today and check
  time is not the bottleneck; measure and cap at two rounds.
- **`annotate` failure degrades everything at once.** Today an annotate error
  yields an empty table and the string system limps on; post-deletion it
  yields "unresolved" dispatch errors. This is *by design* (the module already
  passed `check`, so annotate failing is a compiler bug we want loud —
  `WITCHY_DEBUG_ANNOTATE` exists), but it converts silent degradation into
  visible failure, and the transition will surface latent annotate gaps.
  Budget for fixing those in step 1-2 rather than treating them as blockers.
- **Behavioral deltas where the string system guessed right by luck.**
  Programs that today resolve via a stale/wrong scope string but happen to
  type-check could change diagnostics. The invariant says they could never
  have produced wrong *code*, so the delta is error-message churn — pin the
  important ones in the message-asserting frontend tests (79 exist).
- **Sequencing pressure.** 0053 (rendering) and 0054 (structured errors) gate
  on this RFC, and 0043/0050 want its receiver typing. Slipping it slips the
  set — the reason it lands in five separately-green steps rather than one.

## Prior art

- Rust's trait resolution operates on the inference context's `Ty`, never on
  rendered type strings; the "shadow type system that re-parses its own
  pretty-printer" is a known anti-pattern this RFC exits.
- The project's own typed-lowering keystone (rfcs/language-evolution.md
  Phase 0) built `annotate` for exactly this consumption; the five existing
  table fallback sites in traits.rs are its proof of concept.
- CLAUDE.md's no-special-casing rule (and rfcs/0016's thesis): one general
  mechanism, per-case tables are debt whose deletion-while-green is the proof.

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9).

**Status-accuracy corrections — this note corrects a FALSE tracking claim.** The
RFC's tracking note asserts acceptance criterion (a) — `iter.collect` inferring
through generic `from_list` chains — is met. It is not: it FAILS on the
2026-07-04 binary, probed both with and without caller ascription. Ledger against
the plan: step 2 DONE (table-first dispatch, traits.rs:592-608); step 3 DONE
(Eq-bounded list.*); step 1 NOT done (`bind_type_var` still handles only 3
shapes; `fn_rets` is still a lossy head-only map); step 4 NOT done (the entire
shadow zoo is alive and grew — traits.rs went 2,566 → 2,642 lines, and
`recover_generic_call` gained a new `list.at` special case since the RFC was
written); step 5 barely started (only iter.any/all; zero std imports of iter).
The §2 fixpoint re-annotate is not implemented — which is exactly why
acceptance (a) fails.

**Required revisions / actions.** (1) This dated note replaces the false
acceptance claim. (2) Schedule steps 1 → 4 → 5 as the next compiler item — this
RFC is the keystone of the dependency spine, and RFC-0043 is landing on its
resolution point now. (3) Reconcile CLAUDE.md: its trait-dispatch section
directs new fixes INTO `recover_generic_call` — the very function step 4
deletes. The RFC, CLAUDE.md, and the code currently tell three different
stories.

**Verdict.** Fix the false tracking note; finish steps 1/4/5. Priority: high —
the highest of the review.

## Implementation note (2026-07-04, steps 1/4/5 + BUG-001/004)

Steps 1, 5, and the two coupled bugs landed; step 4 landed as a bounded,
green-verified deletion with a precisely-scoped residual.

**Step 1 (fixpoint) — DONE.** Acceptance (a) passes on both backends with no
ascription: a generic helper's bounded `iter.collect` resolves because
`lower_with` now runs annotate → monomorphize → **re-annotate** to a fixpoint
(memo persisted; bounded rounds). A generic function that transitively calls a
bounded template is itself a no-fallback template on both backends
(`no_fallback_template_names`) — its obligation propagates to concrete call
sites. The loud pass gets a fresh table over the final module.

**Step 5 (iter) — DONE.** `min`/`max`/`last`/`position`/`scan`/`flatten` added
(acceptance d, clause 1). Three std modules dogfood iter internally
(`path.drop_last`, `csv.encode_row`, `semver.best` — clause 2). The collection
core (list/set/string/cmp/option) sits UPSTREAM of iter and cannot import it, so
the dogfooded modules are leaf-ish; noted for future work.

**Step 4 (delete the shadow system) — PARTIAL, by structural necessity.**
DELETED: `recover_generic_call` + `bind_type_var` (the per-shape guessers —
`list.at` matched BY NAME, exactly three bindable parameter shapes) and
`builtin_ret`. Their capability is NOT gone: `declared_call_result` replaces
them with step 1's general structural binding — one unification of the callee's
DECLARED signature against the arguments' known types (structured `Type`s, no
string surgery), which also answers concrete declared returns with their FULL
encoding (de-lossifying the `fn_rets` head-only failure, motivation case 2) and
types `xs[i]` from the base's list type. The full gate proved this judgment is
still required by the empty-table quiet pass (examples/diff + examples/life:
a let bound to `table.at(i-1)` as a method receiver — annotate hard-errors on
unresolved MethodCalls, so the table cannot bootstrap itself). CLAUDE.md's
dispatch section was corrected (it routed fixes into the deleted
`recover_generic_call`).

RESIDUAL, kept with reasons (NOT a new shape table — the existing local-judgment
core): `head_type_name` + its string parsers (`list_elem`/`generic_arg`/
`tuple_args`/`head_of`/`apply_subst`/`type_to_scope_name`/`SCOPE_NAME_MAX_DEPTH`)
and `cap_op_return_type`. They remain because (1) the **quiet pre-mono pass runs
with an empty table** and the checker HARD-ERRORS on any surviving `MethodCall`
(typeck `Expr::MethodCall` arm), so local receiver typing is structurally
required to resolve method syntax *before* annotate can produce a table; (2)
mono walks freshly-generated specialization bodies (clones, not yet in any
table) within a round via this path; (3) `head_of` also backs the architectural
concrete-type → head → generic-impl lookup in `lookup_impl`. Full deletion needs
a larger restructure — a **lenient-`MethodCall` annotate** to hand the quiet pass
a table, plus extending+re-annotating generated bodies before walking them — the
checker-adjacent change this RFC deliberately kept out of scope for blast radius.
That restructure is the tracked follow-up; the string zoo is now dead-cited-code-
free (nothing routes new fixes into it) even though the local-judgment core
survives.

**BUG-001 (Mono comparator hijack) — FIXED.** `rename_calls_block` threads a
`Scope`; a call on a bound local (a `fn`-typed comparator param named like a
trait method) is never renamed to the impl. Regression test on both backends.

**BUG-004 (address-keyed TypeTable) — MITIGATED (smallest sound fix).**
`infer_transient` stops recording throwaway desugar-temp subtrees, closing the
free-then-reuse false-hit hole at its source. Stable node identity remains the
architectural long-term fix (it does not fit monomorphization, which needs cloned
nodes to have FRESH identity).
