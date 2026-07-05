---
rfc: 0056
title: "Keyword arguments: generalize labeled construction to every call"
status: implemented
created: 2026-07-03
predecessors:
  - "0043 (declared mutation — shares the call-shape surface; lands first)"
  - "0050 (method-call generalization — defines fn-as-value; labels must not conflict)"
  - "0049 (naming lexicon — parameter names are already API)"
tracking:
---

# RFC-0056: Keyword arguments — generalize labeled construction to every call

> Provisional syntax throughout. Code blocks are intentionally **not** tagged
> `witchy` so the doc-examples sweep does not compile pre-implementation
> snippets.

## Summary

Record construction already accepts labels: `Point(y: 2, x: 1)` parses, is
reordered to the declared field order, and is validated at link time — unknown,
duplicate, and missing fields each get their own error
(`crates/witchy-syntax/src/records.rs:40-92`). Ordinary calls do not:
`greet(name: "ada", excited: true)` is a parse error (probed 2026-07-03:
``expected `)`, found `:```). This RFC deletes that special case in the general
direction: **any direct call may label its arguments with the callee's declared
parameter names.** Labels are checked and reordered to positional at the same
desugar layer records use today, so neither backend changes — parity by
construction. A small companion feature, constant default parameter values,
makes trailing options omittable. Labels never attach to function *values*:
indirect calls stay positional, so nothing here conflicts with RFC-0050's
module-functions-as-values.

## Motivation

**Call-site readability.** The consistency audit's probe corpus is full of
calls whose trailing arguments are unreadable without the signature:

```
string.substring(s, 2, 7)          # start? end? length?
greet("ada", true)                 # true what?
cmp.maximum(xs, 0)                 # is 0 an element or a default?
list.find_or(xs, pred, 0)
```

witchy's identity is Python layout + readable names; readable names that are
invisible at the call site pay half their rent. With labels:

```
string.substring(s, start: 2, end: 7)
greet("ada", excited: true)
cmp.maximum(xs, default: 0)
```

**The lexicon already made parameter names API.** RFC-0049's conventions
treat signature parameter names as user-facing (they render in the generated
`spec/stdlib.md`, and 0049 ships doc-only renames like `option.filter`'s
`pred` → `keep` to keep them coherent). If parameter names are worth
standardizing, they are worth being usable.

**The special case already exists — backwards.** Constructors are the *only*
callable that accepts labels today. `records.rs:68` even reports the asymmetry
as an error message: `` `name(field: value, ...)` is named-field construction,
but `name` is not a record type ``. One callable kind having labels and the
other not is precisely the shape of irregularity this RFC series exists to
remove (CLAUDE.md: build the general mechanism).

**Defaults-by-convention sprawl.** std encodes default arguments as name
variants — `get_or`, `head_or`, `last_or`, `find_or`
(`std/list.witchy:175-189`, `std/dict.witchy:27`), `cmp.maximum(xs, default)`
(`std/cmp.witchy:183`). RFC-0044 keeps the `_or` family (the name states the
total behavior, which is the point), but *new* API should not need a name
variant per optional parameter.

## Design

### Labels at direct call sites

```
fn substring(s: String, start: Int, end: Int) -> String: ...

substring(s, start: 2, end: 7)     # ok
substring(s, 2, end: 7)            # ok — positional prefix, labeled suffix
substring(start: 2, s: s, end: 7)  # ok — labels may reorder freely
substring(s, end: 7, 2)            # error: positional argument after a labeled one
substring(s, start: 2, start: 3)   # error: `start` is bound twice
substring(s, begin: 2, end: 7)     # error: `substring` has no parameter `begin`
substring(s, start: 2)             # error: missing argument `end` (no default)
```

Rules, all enforced at the desugar/link layer:

1. **Labels are the callee's declared parameter names.** No separate label
   syntax in declarations (unlike Swift); the declaration you already wrote is
   the labeling contract.
2. **Positional prefix, labeled suffix.** Positional arguments bind
   left-to-right as today; labeled arguments may appear in any order after
   them. A positional argument after a labeled one is a parse-time error.
3. **Exactly-once binding.** Duplicate labels, unknown labels, and unbound
   parameters are errors — the same three diagnostics `records.rs::build`
   already produces for constructors, reworded for functions.
4. **Direct calls only.** A call is *direct* when the callee is statically a
   named function: a module-qualified call (`string.substring(...)`), a
   user-defined function, a UFCS method call (post-resolution), or a
   constructor (today's behavior, now the same mechanism). Calls through a
   function-typed *value* (`f(2, 7)` where `f` came from `let f = ...` or a
   parameter) are positional-only; labeling one is an error naming the rule:
   ``labels need the callee's declaration — `f` is a function value``.
5. **Labels never enter function types.** `fn(Int, Int) -> String` stays the
   whole type. Consequently: passing `substring` as a value (RFC-0050) erases
   labels; two functions differing only in parameter names have identical
   types; changing a parameter name is *source*-breaking for labeled callers
   but never *type*-breaking.
6. **UFCS receivers are positional.** `s.substring(start: 2, end: 7)` — the
   receiver slot is bound by the method-call form itself and cannot be
   labeled.

### Constant default parameter values

```
fn split(s: String, sep: String = " ") -> List(String): ...
fn connect(net: Net, host: String, port: Int = 443, tls: Bool = true) -> ...

split(s)                    # sep = " "
connect(net, "example.com", tls: false)   # port stays 443
```

- Defaults are permitted on any suffix of the parameter list (a defaulted
  parameter may not precede a non-defaulted one).
- A default is a **closed constant expression**: literals (including duration
  literals, `""`, `[]`), `None`, `true`/`false`, and constructors of such —
  no calls, no references to other parameters or module state. This is
  deliberately the smallest useful set; it covers flags, sentinels-by-name,
  and empty containers, which is what real defaults overwhelmingly are.
  Widening (e.g. referencing earlier parameters) is a possible later RFC once
  there is demand; starting narrow keeps evaluation-order questions out of
  the language.
- Desugar: the compiler splices the default expression into the call site for
  each omitted parameter, then proceeds as a fully-applied positional call.
  Closed-ness makes the splice hygienic by construction — there is nothing to
  capture. (This is the same source-splice discipline tagged literals use.)
- Capabilities cannot be defaulted (a default is a value, and capability
  values cannot be minted — the constant-expression rule already excludes
  them; stated explicitly so the property is load-bearing, not incidental).
- Function values ignore defaults: `let f = split` has type
  `fn(String, String) -> List(String)` and must be called with both
  arguments. Defaults, like labels, are a property of the declaration used at
  direct call sites — not of the function value.

### Implementation sketch

The machinery is `records.rs` generalized:

- **Parser** (`crates/witchy-syntax/src/parser.rs:1463` `call_args`): accept
  `ident: expr` items after the positional prefix in any call's argument
  list, producing `Vec<Arg>` where `Arg = Positional(Expr) | Labeled(String,
  Expr)`. Today the label form is only reachable for constructor-shaped
  callees; this makes the grammar uniform. No ambiguity: `ident : expr` has
  no other meaning inside call parentheses.
- **Desugar/link**: extend the existing field-reorder pass
  (`records.rs::build`) into a general argument-resolution pass that looks up
  the callee's declared parameter list (record constructors keep their
  field-order table; functions use their signatures, which the linker already
  has), validates rules 2-3, splices defaults, and emits today's positional
  `Expr::Call`/`Expr::Ctor`. **Everything downstream — typeck, both backends,
  the WIR — is unchanged.** Parity is by construction because the divergence
  surface is empty.
- **Interpreter**: no change (it sees positional calls).
- **`witchy fmt`**: formats labeled calls as written; no canonicalization in
  this RFC.
- **Docs generation**: `witchy doc` already renders parameter names;
  defaulted parameters render as `sep: String = " "`.

### Ordering relative to the RFC set

Lands **after** RFC-0043 (declared mutation) and RFC-0050 (method-call
generalization): all three touch call-shape resolution, and 0043/0050 change
*which* callee a call resolves to — argument labeling must run against the
resolved callee. No dependency on 0042/0046; independent of the std cuts
(0044/0049), though new std API written after this RFC should prefer a
defaulted parameter over a new `_or` variant (add to 0049's CONTRIBUTING
rules when this ships).

### What this does NOT do

- No `**kwargs`/variadic capture — witchy has no heterogeneous dict to
  receive one, and options-bag use cases are anonymous records.
- No labels in function types, no label-based overloading, no
  label-only-differing signatures.
- No retro-fitting std: the `_or` family stays (RFC-0044 grandfathers
  behavior-in-the-name); folding it into defaults would churn the same
  functions a third time. A later, separate cut can revisit once labels are
  established idiom.

## Alternatives

- **Swift-style declared labels** (external names distinct from internal
  names, labels part of the function type). Rejected: it doubles the naming
  surface RFC-0049 just standardized, and label-carrying function types
  collide head-on with RFC-0050's labels-erase-on-value rule. Swift needs
  labels-in-types because it has overloading; witchy doesn't.
- **Options-bag records** (`fn greet(opts: GreetOpts)`). Works today for
  declared types, but costs a type declaration per function, anonymous record
  types can't appear as parameter types (probed: `.{...}` in a signature is a
  parse error), and it moves the readability into a second definition the
  reader must find. Kept as the idiom for genuinely open-ended config;
  rejected as the general answer.
- **Do nothing.** Calls stay positional; readability lives in `_or`-style
  name variants and comments. Viable — but it leaves the
  constructors-have-labels asymmetry in place, and the audit's evidence is
  that defaults-by-convention is already sprawling (four `_or` functions in
  list alone). The special case then never gets deleted, only worked around.

## Drawbacks

- **Parameter names become breaking-change surface.** Renaming a parameter
  breaks labeled callers. This is real but already half-true (names render in
  generated docs, and 0049 treats them as API); the mitigation is the 0049
  lexicon rules, which make names boring and stable.
- **Two ways to write every call.** `substring(s, 2, 7)` and
  `substring(s, start: 2, end: 7)` both compile; house style has to come from
  convention (suggested: label anything a reader can't infer from the
  receiver — Bools, bare numerics, same-typed pairs), not the compiler.
- **Desugar-layer resolution means labels are invisible to typeck errors.**
  A type error in a labeled argument reports the positional call the desugar
  produced. Mitigable by carrying the label into the argument's error span;
  should be done in the same change, and is easier after 0046's structured
  spans.
- **Defaults hide arity.** A call site no longer says how many arguments the
  callee takes. The closed-constant restriction keeps what's hidden trivial.

## Prior art

- **Python** — keyword arguments + defaults; the mental model borrowed here
  (labels are parameter names; values don't carry them), minus `**kwargs` and
  minus mutable-default-argument hazards (witchy defaults are constants,
  spliced per call — the `def f(x=[])` trap is unrepresentable).
- **Swift** — argument labels in types; studied and rejected (see
  Alternatives).
- **Rust** — rejected keyword args largely over interactions with fn types,
  traits, and overloading-adjacent inference; witchy avoids the conflict by
  keeping labels out of types entirely (rule 5).
- **OCaml** — labeled arguments that *do* enter types; instructive as the
  other pole: label inference is powerful and famously confusing with partial
  application. witchy has no currying, but 0050's fn-values are
  partial-application-shaped enough to warrant the erasure rule.
- Internal: `crates/witchy-syntax/src/records.rs` (the existing labeled
  construction this generalizes), RFC-0022 (place-assignment sugar — the
  precedent for desugar-layer features that are backend-invisible).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below (e.g. "> 2026-07-01: clarified X").
  - The current behavior lives in spec/ and the code — NOT here.
-->

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9).

**Status-accuracy corrections.** The grammar claim checks out: `ident: expr`
inside call parens is unambiguous.

**Required revisions — three design gaps.** (1) Argument evaluation order is
unstated: labels permit reordering, so declared-order vs written-order must be
pinned (record literals already silently reorder — the same decision applies).
(2) Layer contradiction: rule 4 grants labels to method calls "post-resolution",
but the sketch does everything at desugar/link time. Recommended: direct calls
at link; method-call labels at the traits.rs resolution point RFC-0043 just
established (or exclude method-call labels in v1). (3) Defaults × var/own
parameter conventions are unspecified — `var` + default should be an error.

**Verdict.** Needs-revision; implement after RFC-0043. Keep the `_or` family (no
triple churn). Priority: medium.

## Implementation note (2026-07-05) — status: implemented

Shipped as designed, with the three review gaps resolved:

1. **Evaluation order = SOURCE order.** A labeled/reordered call binds each
   *written* argument to a temp (`let __kwN = …`) in the order written, then
   passes the temps to the callee in declared order. When the written arguments
   already appear in declared order (the common case, `substring(s, start: 2,
   end: 7)`), no temps are emitted — the positional call already evaluates
   left-to-right; temps (an `Expr::Block`) appear only when labels actually
   reorder. Constant defaults are spliced directly (no temp — a constant has no
   effect and no order). Covered by `keyword_args_source_order_backends_agree`.

2. **Method-call labels EXCLUDED in v1.** Only DIRECT free/module calls (a
   statically-known callee at the link layer) carry labels. A label on a UFCS
   method call is a **parse error** (the callee resolves later, by receiver type
   in `traits.rs`, so there is no declaration to bind against yet). A label
   through a function *value* (`Apply`) is likewise rejected. Rule 4's
   "post-resolution method labels" is therefore deferred to a later RFC.

3. **`var` + default = compile error** (parse time): a `var` writes back to a
   caller variable, so an omitted argument has nothing to write to. `own` +
   default is allowed. Capability-minting params are never defaultable — the
   closed-constant rule already excludes them (a capability cannot be written as
   a literal), so no extra check is needed.

**Grammar (verified unambiguous).** `ident: expr` inside call parens has no other
meaning: lambdas begin with `fn`, there is no ternary or slice colon, dicts are
built by function (no `{k: v}` literal), and record/anon-record colons live in
`.{…}`/uppercase-ctor forms. A pre-implementation probe confirmed
`greet(name: "ada")` was previously a hard parse error, so the syntax was free.

**Where it lives.** A new `Expr::LabeledCall` node is produced by the parser ONLY
when an argument is labeled (an all-positional call stays `Expr::Call`). A new
link pass, `crate::keyword_args::resolve`, runs after `linker::resolve_methods`
and before folding/typeck: it validates labels against the callee's declared
parameters (unknown / duplicate / missing — the same three diagnostics
`records.rs::build` produces for a record), reorders to positional, and splices
constant defaults. Defaults on plain positional `Expr::Call`s are spliced by the
same pass. Both backends only ever see positional `Call`s (and the temp-binding
`Block`), so parity is by construction — a `LabeledCall` reaching a backend is a
loud `unreachable!`, never silent divergence.

**Scope punts (v1).** (a) Method-call and value-call forms get neither labels nor
default-omission — a defaulted std function must be called as `module.f(x)` (or
`f(x)`), not `x.f()`, to omit a default. (b) A type error inside a labeled
argument still reports the positional call the desugar produced (the label→span
carry the RFC's Drawbacks note flags is not done). (c) The `_or` family is
retained (RFC-0044 grandfathering).

**Reconciliation with RFC-0050 Part 2 (value-erasure).** Consistent, not
conflicting: labels/defaults are properties of the *declaration used at a direct
call site*, never of a function type or value. They are erased at the link layer
before any value could carry them — `Expr::LabeledCall` only ever names a
statically-known callee, and an eta-expanded / passed function value flows
through `Expr::Var`/`Expr::Apply`, which are positional-only (the parser rejects a
label there). So passing `substring` as a value drops its labels and defaults by
construction, exactly as 0050 requires; there is no point at which a label or
default attaches to a value.
