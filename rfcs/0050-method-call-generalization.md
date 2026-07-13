---
rfc: 0050
title: "Method calls from type ownership, not an allowlist; module functions as values"
status: implemented
created: 2026-07-03
predecessors:
  - "0042 (module namespaces — the type→module ownership this derives from)"
  - "0046 (typed trait dispatch — the receiver typing this consumes)"
  - "scratch/consistency-analysis-2026-07-03.md §4 (UFCS allowlist + functions-as-values evidence)"
tracking: "Part 2 (module functions as values) shipped; Part 1 (method calls from type ownership) shipped after RFC-0042"
---

# RFC-0050: Method calls from type ownership, not an allowlist; module functions as values

> Provisional snippets; code blocks are deliberately **not** tagged `witchy`
> so the doc-examples sweep does not execute pre-implementation code.

## Summary

Two closures of the same gap. (1) UFCS method syntax on built-in types is a
**hardcoded allowlist** — `builtin_method_module`
(crates/witchy-types/src/traits.rs:1213-1228) maps exactly
List/Dict/String/Set/Option/Result/Iter (+ Secret/SecretStore) to their std
modules, so `s.length()` works while `b.length()` on a Bytes is a type error.
Replace it with a **derived type→module ownership map**: a type declared in
module X gets X's public receiver-first functions as methods; builtins that
predate modules get one explicit ownership declaration each. (2)
Module-qualified functions become **first-class values**: `xs.map(list.length)`
today fails with ``unbound variable `list``` — an error that doesn't know
modules exist — while a user-defined function passes bare; `list.length` in
expression position will resolve to a function value via compiler
eta-expansion.

One of a set (0042–0055) with one thesis, already law in CLAUDE.md: **facts
must live in local, typed declarations — not global censuses, allowlists, or
string heuristics.** The fact here is "which module's functions are this
type's methods"; the declaration is the type's own `type` item in its module;
the allowlist is the census to delete. The Bytes hole is not an oversight to
patch — it is what allowlists *do*: every type not in the table is silently
second-class, exactly the per-case special-casing CLAUDE.md forbids
(`dict.remove` leaked for the same structural reason).

## Motivation (all probed 2026-07-03)

- `bytes.from_string("hi")` then `b.length()` → ``type error: no method
  `length` on `Bytes` …`` while the same call shape on String works. Bytes is
  documented (std/bytes.witchy:5-6) as *sharing String's memory layout*; it is
  second-class purely because `builtin_method_module` (traits.rs:1213-1228)
  has no `"Bytes"` arm. Also
  probed: `d.human()` on Duration and `n.abs()` on Int fail identically;
  `x.sqrt()` on Float has no owner at all today.
- User types are fine: `impl` blocks give methods, and that path is untouched.
  The gap is exactly std's *module-function* types.
- `let f = list.length` and `xs.map(list.length)` → ``unbound variable
  `list``` — the resolver treats `list.length` in expression position as a
  field read of an unbound variable. A user `fn double(n: Int)` passes bare
  as a value (probed green); a lambda wrapper works (probed green). So the
  stdlib's functions are the only functions in the language you cannot pass —
  std is again second-class in its own language. Trait methods as values
  (`let f = show`) also fail; they stay out of scope here (they need
  dispatch-at-the-use-site; see Alternatives).

Consequences beyond ergonomics: every combinator pipeline over std functions
needs a lambda shim (`fn(x): list.length(x)`), which is friction directly
against the Iter-completion goal of [RFC-0046](0046-typed-trait-dispatch.md),
and doc examples teach two idioms for "pass a function" depending on where
the function happens to live.

## Design

### Part 1 — UFCS from type ownership

**The rule.** A method call `recv.m(args)` whose receiver's type is
statically known (post-[RFC-0046](0046-typed-trait-dispatch.md): typeck knows
it; until then: today's resolution) resolves in this order — unchanged except
for the last step:

1. an `impl`/trait method for the receiver's type (as today; user and std
   `impl`s win over module functions);
2. host-capability intrinsics (as today, `is_host_capability`);
3. **the owning module's public function**: if the receiver's type is
   *declared in* module X and X exports `pub fn m` whose first parameter
   accepts the receiver's type, lower to `X.m(recv, args)`.

"Declared in" is derived, not tabulated: [RFC-0042](0042-module-namespaces.md)
gives every type a home module as part of namespacing (its `aliases::resolve`
machinery knows, for every type name in scope, which module's `type` item it
names). `Set` is declared in std/set.witchy:12 → `set` owns it; `Iter` in
std/iter.witchy:20 → `iter`; a third-party rune's `type Matrix` in module
`matrix` gets `matrix.*` as methods with **zero** compiler involvement — the
map is a projection of the program, so it can never have a Bytes-shaped hole.
**Dependency: this derivation rides on RFC-0042's module-scoped types.**
Landing order is 0042 → this RFC; a standalone table would be re-creating the
allowlist.

**Builtins that predate modules.** Int, Float, Bool, String, List, Dict,
Bytes, Duration, Option, Result are checker-native (`Ty` variants in
typeck.rs), declared nowhere. Each gets **one explicit ownership
declaration**, in the std module that is already its de-facto API home, via a
module-level annotation the linker reads (one line, in the owning module,
next to the functions it blesses):

```
// std/bytes.witchy
owns type Bytes
```

Assignments: `String`→`string`, `List`→`list`, `Dict`→`dict`,
`Bytes`→`bytes`, `Duration`→`duration`, `Option`→`option`,
`Result`→`result`. (`Set` and `Iter` need no line — they are declared types.)
Exactly one module may claim a type; a second `owns` for the same builtin is
a link error. The declaration is the *fact in a declaration*: greppable,
documented where the functions live, and rendered into spec/stdlib.md.

**Int and Float: excluded, deliberately.** Weighed honestly: `math` is the
candidate owner, but math is *not* Int's API home the way `list` is List's —
its Int surface is a grab-bag (`gcd`, `is_prime`, `to_base`) alongside
Float functions with `float_`-prefixed names (`float_abs`, `float_min`)
that exist precisely because the module mixes two receiver types;
`n.is_prime()` reading as "a method of Int" also implies a coherence
(`n.to_base(16)`, `x.format_float(2)`?) the module doesn't have. Granting
`math` ownership of *two* types would make method resolution on it
ambiguous-by-signature rather than by-type. Decision: **no owner for Int and
Float now**; `n.abs()` stays an error whose message names `math.abs(n)`. If
[RFC-0049](0049-naming-lexicon.md)'s cleanup ever splits `math` into
`int`/`float` modules, each naturally `owns` its type and the exclusion
dissolves — the mechanism is ready, the assignment just isn't earned yet.

**What this fixes on day one** (acceptance tests): `b.length()`,
`b.slice(0, 2)`, `b.to_list()` on Bytes; `d.human()`, `d.to_seconds()` on
Duration; and — because the map is derived — the first third-party rune type
whose module functions become methods without touching the compiler.
`builtin_method_module` is **deleted**; the suite staying green with it gone
is the proof (the CLAUDE.md bar).

**Bytes API completion** (the audit's Tier-0 item 9 rides along): with
ownership in place, `bytes` grows `from_list`/`contains`/`index_of` per
[RFC-0044](0044-std-error-policy.md)'s shapes — method syntax makes the gaps
visible, the error-policy RFC owns the signatures.

### Part 2 — module-qualified functions as values

`module.name` in **expression position** (not call position), where `module`
is an imported (or prelude) module and `name` is a `pub fn` it exports,
resolves to a function value. Implementation: **compiler eta-expansion at the
use site** — the resolver rewrites the expression to a lambda of the
function's declared arity:

```
xs.map(list.length)        # becomes
xs.map(fn(x0): list.length(x0))
```

Decided over emitting a first-class function *reference* because it requires
zero backend work: both backends already implement lambdas and the closure
ABI; an eta-expanded lambda captures nothing, so no allocation-behavior
change on the hot path either backend cares about; and typeck sees an
ordinary lambda whose body is an ordinary call, so inference, capability
checking, and the WASM lowering are all untouched. The rewrite happens in
the same resolution pass that today produces the "unbound variable" error,
where the module import set is in scope. Equality on function values is
[RFC-0047](0047-one-equality.md)'s problem (its recommendation — reject `==`
on function types — makes the eta-expansion unobservable).

Scope and rules:

- Works anywhere an expression works: arguments, `let` bindings, list
  literals, returns. `let f = list.length; f([1,2])` is well-typed.
- The function must be resolvable exactly as a *call* to it would be
  (import checked, name exists — the existing `resolve_call` rules,
  linker.rs:1569-1588); errors reuse those messages.
- **Capability ops and trait methods are excluded.** Bare intrinsics
  (`read`, `connect`) are not module functions and passing one as a value
  would smuggle an authority-shaped hole past the footprint analysis; trait
  methods (`show`) have no single function to reference until the receiver
  type is known — eta-expansion can't pick the impl. Both keep their current
  errors, with the trait-method error upgraded to say why and name the fix
  (`fn(x): x.show()` — the receiver-typed lambda dispatches normally).

**The error when resolution genuinely fails.** Today's ``unbound variable
`list``` becomes, when the base names a std/imported module:
``` `list.length` names a module function — it is now a value; this error
means the name is wrong: module `list` has no function `lenght` ``` (i.e.
the two real failure cases — unknown function, missing import — reuse the
call-position diagnostics verbatim; "unbound variable" never mentions a
module again).

### Parity

Both parts are source-to-source rewrites on the single linked AST before
either backend lowers (the same guarantee RFC-0028 rode): the backends
cannot diverge on them. Differential tests pin: each newly-methoded type
(Bytes/Duration probes above) and an eta-expansion passed through `map`,
`fold`, a channel, and a stored record field, on both backends.

## Alternatives

- **Keep the allowlist, add the missing entries** (Bytes today, Duration
  tomorrow, the next rune's type never). Rejected: the hole *is* the
  mechanism — a table in the compiler cannot know about third-party types at
  all, so post-0042 it structurally cannot serve the ecosystem; and each
  entry is exactly the per-case special-casing CLAUDE.md forbids. The
  audit's option "document the allowlist as policy" is the same alternative
  in surrender form.
- **Types opt into UFCS by annotation** (`type Matrix with methods` or a
  `derive(Methods)`). Weighed: it is at least a *declaration*, thesis-
  compatible. Rejected because it re-introduces a decision with no real
  choice behind it — no module author wants their type method-less while
  every std type has methods — so the annotation would be boilerplate on
  100% of types, i.e. a tax, not information. Ownership-by-declaration-site
  carries the same fact with zero new syntax. (The `owns` line for
  checker-native builtins is the honest residue: those types genuinely have
  no declaration site, so the fact must be written *somewhere*, once each.)
- **True first-class function references** instead of eta-expansion (a
  function-pointer value both backends understand). Cleaner object model,
  and it would extend to trait methods later; but it touches both backends'
  closure ABI and the WASM function-table story for a result the user cannot
  distinguish from the lambda (given 0047 rejects function `==`).
  Eta-expansion is the smaller mechanism that is observably identical;
  revisit references only if a future feature (serializing functions?) makes
  the difference observable.
- **Fix only the error messages** (tell the user to write the lambda).
  Cheaper, honest, and worth doing *anyway* in the interim — but it leaves
  std functions permanently second-class values, and the audit's "two kinds
  of function name, only one first-class" incoherence stands.

## Drawbacks

- **Method-name collisions become possible where the table made them
  impossible.** If a module both declares a type and exports a function whose
  name matches a trait method implemented for that type, order rule 1
  (impls win) decides — but a *new* `impl` can now shadow an existing
  module-function method, changing which function a `.m()` call names. This
  is the same shadowing question every language with methods has; the
  resolution order is fixed and documented, and — unlike RFC-0043's old
  census — it is per-type, so an unrelated `impl Bag` can never affect a
  List call. A `witchy check` note when an impl shadows an owning-module
  function is cheap insurance.
- **`owns` is new (tiny) surface**: one keyword, seven lines in std, a link
  error, doc rendering. It exists only for checker-native builtins; if the
  builtins ever become declared prelude types (a plausible 0042 follow-up),
  the keyword is deleted.
- **Eta-expansion is a desugar users can't see**, and desugars have bitten
  this project before (the `||` triple-role, the old write-back census). The
  mitigations are that it is *local* (one expression, no global facts), typed
  by the ordinary checker after expansion, and pinned by differential tests.
- **Int/Float stay method-less**, which keeps one visible asymmetry
  (`d.human()` works, `n.abs()` doesn't) and this RFC's own text as the only
  justification. The error message carrying `math.abs(n)` is the mitigation;
  0049's module split is the cure if it happens.
- **Sequencing**: Part 1 gates on [RFC-0042](0042-module-namespaces.md) (the
  ownership derivation) and is strongest after
  [RFC-0046](0046-typed-trait-dispatch.md) (receiver typing that resolves
  more sites); Part 2 gates on neither and can land first. Statement-position
  method calls produced by Part 1 immediately fall under
  [RFC-0043](0043-declared-mutation-writeback.md)'s declared-write-back rule
  — a newly-methoded `var`-receiver function gets statement-form write-back,
  and a non-mutator gets 0043's discard error, with no extra machinery: the
  two RFCs compose because both key on the resolved callee's declaration.

## Prior art

- The project's own capability doctrine: authority is a value you pass, not
  an ambient table — this RFC applies the same shape to method dispatch
  (functions of the type's home, not a compiler registry).
- Rust: methods come from the type's impl blocks + traits in scope — a
  derived, per-type fact; there is no central "types with methods" table.
  Uniform paths (`Vec::len` as a value) are the Part-2 analogue.
- Python: `str.upper` is a first-class value because methods *are* module
  attributes; witchy gets the equivalent via eta-expansion without binding
  its object model to it.
- D/Nim UFCS: fully general receiver-first call rewriting; witchy's version
  is deliberately narrower (owning module only, impls win) to keep "where
  can this method come from" a one-sentence answer.

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9).

**Status-accuracy corrections.** All three motivating probes reproduce:
`Bytes.length`, `Duration.human`, and the `unbound variable list` error.

**Required revisions.** Split the RFC. Part 1 (the derived type→module ownership
map) is hard-gated on RFC-0042 — defer until it lands. Part 2 (eta-expand
`module.fn` as a value) is implementable now with two revisions: (1) exclude
post-0043 var-procedures from eta-expansion (or name the real cause in the
error); (2) note that inference quality gates on RFC-0046 step 1 — unannotated
lambdas with free type vars land in exactly the territory where 0046's
acceptance (a) currently fails.

**Verdict.** Split: Part 2 implement-now (after the revisions); Part 1 defer
until 0042 lands. Priority: medium-high (Part 2) / medium (Part 1).

## Implementation note (2026-07-05, Part 2)

Part 2 is implemented. At the time of this note, Part 1 remained deferred
because it was hard-gated on RFC-0042's module-scoped type ownership, and the
`builtin_method_module` allowlist was untouched. See the 2026-07-07 note below
for the Part 1 landing.

**Where.** All in the linker, `crates/witchy-syntax/src/linker.rs` — a
source-to-source rewrite on the single linked AST before either backend lowers,
so parity holds by construction:

- `FnTable`'s inner value carries, per exported function, an `EtaSig {arity,
  is_var_procedure}` (built alongside the existing name set; membership stays
  `contains_key`).
- `rewrite_expr`'s `Expr::Field` arm: when the base is a bare module name in
  scope here (a prelude module or one this module imports) and not shadowed by a
  local, `module.field` is a module-qualified *value* reference. It is validated
  exactly as a call (`resolve_call`) and rewritten by `eta_lambda` into
  `fn(__eta0, …): module.field(__eta0, …)` at the callee's full declared arity.

**Inference (the review's revision 2).** The eta-lambda's parameters carry no
type annotation, so a generic callee (`list.length : List(a) -> Int`) yields a
lambda with free type vars — exactly the shape the review flagged. Post-merge
RFC-0046 resolves it: the annotate/mono fixpoint types the lambda from its use
site. Verified running on both backends — `xs.map(list.length)`, a two-argument
`fold` reducer (`math.max` and the generic mutator `list.concat`), a `let`
binding, and a function value stored in a record field.

**Exclusions (the review's revision 1).** A Nil-returning `var`-procedure
(RFC-0043) is rejected up front with a link error that names the real cause (a
`let` lambda parameter cannot satisfy the `var` demand); RFC-0043 mutators
(return self) are *not* excluded — their value form is a pure call. Capability
intrinsics and trait methods are bare names, not `module.fn`, so they are
naturally out of scope and keep their current errors.

**RFC-0056 reconciliation.** Labels and constant defaults never attach to a
function *value*: eta-expansion uses the full positional arity, so a
defaulted-parameter function becomes a lambda taking every argument. Pinned by
`module_function_value_uses_full_declared_arity_backends_agree`.

**Error quality.** A wrong function name on an in-scope module now reuses the
call-position diagnostic (``module `list` has no function `lenght` ``); ``unbound
variable `list` `` no longer appears for a module base.

**Tests.** Five differential tests in `src/example_tests.rs`
(`module_function_*` / `module_var_procedure_*`).

## Implementation note (2026-07-07, Part 1)

Part 1 is implemented. Method fallback no longer depends on a compiler census of
method-capable types. For ordinary module-scoped types, the owner is derived from
the RFC-0042 canonical type name (`matrix.Matrix` → `matrix`), and only public
receiver-first functions from that owner module become methods. Ambient builtins
that still have no ordinary source declaration keep a small owner table until
those types stop being ambient; this table now includes the motivating Bytes and
Duration holes. Direct checker-only paths still lower ambient builtin methods
without needing linked std item metadata, matching the old behavior for `List`
and friends.

Acceptance tests cover Bytes/Duration methods, a user `matrix.Matrix` module
whose public receiver-first functions become methods without a compiler entry,
and the privacy boundary: a private receiver-first helper in the owner module is
not callable as a cross-module method.

## Post-implementation note (2026-07-08): std container methods are a separate cut

The compiler-level RFC is implemented, but this should not be read as saying the
standard library's container API is already idiomatic `impl`-method API. `List`,
`Dict`, `Set`, `String`, and friends still expose most operations as receiver-
first free functions (`list.push(xs, x)`, `dict.insert(d, k, v)`,
`set.remove(s, x)`) that *also* work through dot-call because RFC-0050's UFCS
fallback resolves public owner-module functions as methods.

That is a compatibility bridge, not the final design decision for std. User
types and trait protocols already use real `impl` methods with `self` /
`var self` / `own self`; leaving std containers permanently in a different
idiom would keep the flagship library slightly second-class in its own
language. The follow-up decision belongs here because it is about the public
method model, not about one container operation:

- either convert the std container surface to real `impl` methods, keeping only
  genuinely useful free-function aliases;
- or explicitly bless receiver-first free functions as the stdlib house style
  and document why user-defined APIs should imitate it.

The current coherence direction is the first option, but it must be sequenced
after the in-place `var` write-back path is pinned. The BUG-558 regressions now
cover loop-watermark write-back through both whole-record and record-field
mutators (`loop_watermark_rejects_outer_var_writeback` and
`loop_watermark_rejects_outer_var_record_field_writeback`); any broad stdlib
conversion to `var self` methods should keep those tests green and add at least
one differential test per converted mutator family.

### Probe result (2026-07-08): std methods need one more compiler rule

A narrow attempt to add real `impl List(a)` methods for `push` and `concat`
proved the desired end state but exposed two prerequisites that must land before
the stdlib conversion:

- Inherent methods are currently callable as bare functions (`push([1], 2)`).
  That is acceptable for local user APIs, but it reopens the exact stdlib global
  fallback that RFC-0070 D5 closed. Std-owned inherent methods must not leak
  unqualified names from prelude/imported modules, or the method sweep
  invalidates the plain-import/no-bare-functions cleanup.
- The in-place optimization recognizes the existing std mutator path, but a
  delegating `impl List(a).push(var self, ...)` no longer satisfies
  `stats::tests::mutating_method_statement_is_in_place`. The conversion must
  either teach the in-place classifier about resolved std inherent methods or
  move the primitive body behind the method without losing the optimization.

So the migration pattern is still right, but the next implementation step is
compiler support, not a mechanical stdlib edit. Keep the existing free functions
until both prerequisites are pinned by tests; then convert one mutator family at
a time.

Follow-up progress: the bare-dispatch prerequisite is now pinned by
`ambient_std_inherent_methods_do_not_become_bare_functions`. Ambient std-owned
inherent methods remain receiver-callable, but they do not enter the bare
function dispatch set, so a future `impl List(a).push` cannot make
`push([1], 2)` type-check again. The remaining blocker for the first real
stdlib conversion is the in-place classifier recognizing resolved std inherent
mutators without losing `mutating_method_statement_is_in_place`.

That second blocker is now closed for the owner-function migration pattern:
ambient std-owned inherent methods alias to the existing owner-module function
when that function exists. So `xs.push(i)` dispatches through the real
`impl List(a)` method surface but lowers to `list.push(xs, i)`, preserving the
established in-place self-assign shape. `List.push` and `List.concat` are the
first converted slice, with
`std_list_impl_methods_and_free_functions_coexist_on_both_backends` pinning
record-field write-back, statement-form write-back, module function calls, and
compiled/interpreter parity.

The next string slice extends the same standard-library rule from pure mutators
to the common read/combinator surface: `String` now has real inherent methods for
operations such as `length`, `split`, `contains`, `index_of`, `split_once`,
`parse_int`, `lines`, and the existing value-mutators. Module functions remain
the stable function-value and explicit-module surface.

Follow-up progress: `Bytes` now has real inherent methods for its primary
receiver-first surface (`length`, `at`, `get`, `concat`, `slice`, conversion,
search, and prefix/suffix checks). The module functions remain as explicit
module calls and first-class values, but ordinary byte-buffer code can use the
same receiver syntax as `String`, `List`, `Dict`, and `Set`.

Follow-up progress: `Option` and `Result` now have real inherent methods for
their primary combinator and conversion surfaces (`map`, `map_or`,
`and_then`, `filter`, `or`, `or_else`, `ok_or`, `zip`, `flatten`, `map_ok`,
`map_err`, `ok`, `err`, and the defaulting unwrap helpers). The module
functions remain the stable function-value surface, but fallible/control-flow
code no longer has to switch idioms when it moves from a user type method
chain to `Option` or `Result`.

Follow-up progress (2026-07-13): `List.map` is the first std operation whose
implementation body moved fully into an inherent method. The source-level
`fn list.map(xs, f)` wrapper is gone; `list.map(xs, f)` and value-position
`list.map` are linker/compiler aliases to the generated method implementation
symbol (`List__map`). This keeps old module-qualified and function-value code
working while avoiding a duplicated std wrapper that calls out to the method.
