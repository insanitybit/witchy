# Canonical route census over examples/*/src/*.witchy (2026-09-05)

Source: read-only classification of `witchy check` on the canonical SafeWIR route,
branch impl/rfc0165-interpreter after commits b1b36b2d6 and 9dd975ff8. The 56
"passing" files all lack a `main`, so canonical lowering never ran on them; the
true admission rate on runnable examples was 0 of 107. Items B1-B13 are
programs inside the foundation subset that the canonical route refuses or
crashes on; they are canonical defects, not exclusions.

# RFC-0165 canonical-SafeWIR census: classification of the 107 failing examples

Route: `witchy check` on the canonical SafeWIR path, binary
`/private/tmp/witchy-rfc0165-interpreter-target/debug/witchy`,
worktree `.codex/worktrees/rfc0165-interpreter`.
Corpus: 163 `examples/*/src/*.witchy`; 107 fail, 56 "pass".

Foundation milestone subset (RFC-0165 migration step 7 + 0165-memory-mode-boundary):
scalar control flow; UTF-8 strings; scalar and string lists; runtime-checked
indexing and list mutation; direct and indirect top-level calls; `let`/`var`/`own`
owned-reference ABI; loops and proper tails; packed scalar records.

---

## 0. Headline

**The 56 "passes" are vacuous and the foundation subset is essentially
non-functional.** Every one of the 56 passing files is a `*_test.witchy`
library with no `main` (plus `examples/app/src/strutil.witchy`); a main-less
module is never canonically lowered, so `ok` proves nothing — see §4.
Of the 163 files, **107 have a `main` and 107 fail: 0% of runnable programs
admit.**

Direct probing (scratchpad `probes/`) shows the failure is not concentrated in
the excluded families. These minimal in-subset programs all fail:

| probe | result |
|---|---|
| `console.print("n=${n}")` | `MissingBinding("@render")` |
| `if c: print() else: print()` | `UnsupportedExpression("console.print used as a value")` |
| `if c: print()` (no else) | `UnsupportedExpression("if without else")` |
| `while i < 3: print(...)` in a fn holding `Console` | `InitialValidation(Safety(InvalidOwnership{FunctionId(0), ValueId(2)}))` |
| `match d: 1 -> ...; _ -> ...` | `Admission(Lowering(UnsupportedControlFlow "unstructured or unreachable block"))` |
| `"hi " + name` returned from a fn | `OwnedReleasePlacement("MissingToken(ValueId(3))")` |
| `fn bump(var n: Int)` | `UnsupportedConvention{..., convention: Var}` |
| `fn f(let xs: List(Int))` | **PANIC** `unreachable` at `canonical_lower.rs:472` |
| `type Point: x: Int; y: Int` | `unsupported canonical v1 layout type` |
| `for i in 0..3` | `UnsupportedExpression("range")` |
| `if n < 0: return 0` | `UnsupportedStatement("return in block")` |
| `(f - 32.0) * 5.0` | `UnsupportedExpression("non-integer arithmetic")` |

What *does* admit: straight-line `console.print` of string literals and `+`
concatenation of literals, `while` loops in a function with **no** capability
parameter, `xs.push` / `xs[i]` in a capability-free function, and indirect
calls through an `fn(Int,Int)->Int` parameter.

---

## 1. Failure families (all 107)

| # | Family | n | Representative error | In foundation subset? |
|---|---|---|---|---|
| 1 | Capability / concurrency handle layout (`Dir`, `File`, `Env`, `Net`, `chan.Sender/Receiver`, `task.Task`) | 19 | ``unsupported canonical v1 layout type `Named("Dir", [Named("Read", [])])` `` | **no** (host APIs, async, channels excluded) |
| 2 | `Option(T)` layout | 12 | ``unsupported canonical v1 layout type `Named("Option", [Named("Int", [])])` `` | **no** (Option/Result excluded) |
| 3 | **String interpolation** `"${x}"` | 9 | `MissingBinding("@render")` | **YES — BUG** |
| 4 | Generic specialization of list combinators / closures | 8 | `UnsupportedCanonicalObligation("generic specialization \`List__map__Int__Int\`")` | **no** (closures, comprehensions excluded) |
| 5 | `List` of user/nominal element type | 5 | ``layout type `Named("List", [Named("bank.Account", [])])` `` | **no** (general records/sums) |
| 6 | `Result(T,E)` layout | 5 | ``layout type `Named("Result", [Named("Int",[]), Named("String",[])])` `` | **no** |
| 7 | Tuple inside a `List` | 5 | ``layout type `Named("List", [Tuple([...])])` `` | **no** (tuples excluded) |
| 8 | User record / sum nominal type | 12 | ``layout type `records.Point` `` / `eval.Expr` / `shapes.Shape` | **mixed** — sums no; **packed scalar records YES — BUG** (see §2) |
| 9 | Lambda expression | 3 | `UnsupportedExpression("lambda")` | **no** |
| 10 | Interpolation *inside* a closure | 3 | `UnsupportedCanonicalObligation("closure witness: canonical closure capture \`@render\` is not a resolved lexical binding...")` | **YES for the `@render` half — BUG + misattribution** |
| 11 | Owned-release placement | 3 | `OwnedReleasePlacement("MissingToken(ValueId(3))")` | **YES — BUG** |
| 12 | Stdlib nominal type (`http.Request`, `json.Json`, `time.DateTime`, `prng.Rng`, `bst.Tree`, `generic_stack.Stack`, `opt_mode.ScanState`, `dynamic_reflection.Counter`, `commands.Op`, `traits.Square`) | 3+9 | ``layout type `http.Request` `` | **no** |
| 13 | Tuple layout ("unresolved closed layout type") | 2 | ``canonical layout rejected: <tuple>.0: unresolved closed layout type `String` `` | **no** (tuples) — but **error text is wrong**, §3 |
| 14 | Missing stdlib/method binding | 5 | `MissingBinding("string.split")`, `String__pad_left`, `string.char_count`, `regex.matches`, `duration.human` | **mixed**; `string.split`/`pad_left`/`char_count` are UTF-8 string ops — **borderline BUG**, §2 |
| 15 | Missing **cross-module user** binding | 1 | `MissingBinding("strutil.shout")` | **YES — BUG** (direct top-level call) |
| 16 | Nested / non-Int-scalar `List` (`List(List(Bool))`, `List(Float)`) | 2 | ``layout type `Named("List", [Named("List", [Named("Bool", [])])])` `` | borderline (scalar lists are in; nesting is not stated) |
| 17 | `var` parameter convention | 1 | `UnsupportedConvention { function: "signature FunctionSignatureId(1)", parameter: "0", convention: Var }` | **YES — BUG** |
| 18 | Unstructured control flow from `match` | 1 | `codegen error: ... Admission(Lowering("Lowering(UnsupportedControlFlow { function: FunctionId(0), operation: \"unstructured or unreachable block\" })"))` | **YES — BUG** |
| 19 | `return` in a block | 1 | `UnsupportedStatement("return in block")` | **YES — BUG** (scalar control flow) |
| 20 | Short-circuit `&&` / `\|\|` | 1 | `UnsupportedExpression("short-circuit binary operation")` | **YES — BUG** |
| 21 | Float arithmetic | 1 | `UnsupportedExpression("non-integer arithmetic")` | **YES — BUG** (Float is a scalar) |
| 22 | List patterns in `match` | 1 | `UnsupportedExpression("match pattern")` | borderline (complex patterns excluded; list shapes on a scalar list arguably in) |
| 23 | `Bytes` | 1 | `UnsupportedType(Named("Bytes", []))` | **no** |
| 24 | Function value inside a `List` | 1 | ``layout type `Named("List", [Fn([...])])` `` | **no** |
| 25 | **Compiler panic** (no refusal at all) | 1 | `thread 'main' panicked at crates/witchy-lower/src/canonical_lower.rs:472:18: internal error: entered unreachable code` | **YES — BUG**, §2/§3 |

(Family 8 and 12 overlap in the raw table; the counts above split them by whether
the nominal type is user-defined or stdlib. Total = 107.)

---

## 2. Canonical BUGS — programs inside the foundation subset that fail

### B1. String interpolation is unimplemented (12 files)
`MissingBinding("@render")` for every `"${x}"`. UTF-8 strings are in the subset;
interpolation is the primary way examples produce strings.
`examples/{hello,fizzbuzz,loops,floats,math_demo,queens,wrap,compute,constants}` (9)
plus `examples/{list_ops,pipeline,sort}` where the same `@render` surfaces as a
bogus "closure witness" failure (3). `hello.witchy` — the flagship program — fails
here.
Construct: string interpolation lowering never binds the `@render` builtin.

### B2. `if`/`else` whose branch value is a `Nil`-returning capability call
`UnsupportedExpression("console.print used as a value")`.
Probe: `if a > 0 && a < 5: console.print("yes") else: console.print("no")`.
Scalar control flow is in the subset; a statement-position `if` whose branches
end in `console.print` must lower.

### B3. `if` without `else` is refused
`UnsupportedExpression("if without else")`. Probe `q_ifnoelse.witchy`.
Core scalar control flow.

### B4. `while`/`for` inside a function that holds a capability parameter
`InitialValidation(Safety(InvalidOwnership { function: FunctionId(0), object: ValueId(2) }))`.
Isolated: `r_while_nocap.witchy` (loop, no `Console` in scope) is **ok**;
`r_while_print.witchy` (identical loop in `fn main(console: Console)`) fails.
The loop back-edge does not retain/release the capability's owned-reference
token. "Loops and proper tails" + the owned ABI are both in the subset. This is
the single defect behind `examples/subscript` and every loop-plus-print program.

### B5. `match` on integer literals produces unstructured control flow
`examples/signs/src/signs.witchy`; probe `q_matchpos.witchy` (`match d: 1 -> ...; _ -> ...`)
reproduces without negative literals. Scalar control flow.

### B6. Owned-release placement for a String returned from a helper
`OwnedReleasePlacement("MissingToken(ValueId(N))")` —
`examples/strings` (`"hello, " + name` returned from `shout`),
`examples/ownership` (`own name: String`), `examples/subscript`.
Probes `p_str.witchy`, `p_own.witchy` reproduce in 5 lines.
`let`/`var`/`own` owned-reference ABI is explicitly in the subset.

### B7. `var` parameters are refused
`examples/mutate`; probe `p_varparam.witchy`.
`UnsupportedConvention{..., convention: Var}`. The memory-mode-boundary amendment
says "Keep `let`, `var`, `own`" and lower them to the owned-reference ABI.

### B8. Explicit `let` parameters PANIC the compiler
`pub fn first(let xs: List(Int))` →
`thread 'main' panicked at crates/witchy-lower/src/canonical_lower.rs:472:18:
internal error: entered unreachable code`.
Root cause: the `match (value_type, convention)` at `canonical_lower.rs:468-472`
handles only `Convention::{Let, Own, Var}`; `Convention::Borrow` (the explicit
`let` keyword on a parameter, `ast.rs:476`) falls through to `_ => unreachable!()`.
Reachable from ordinary source; hit by `examples/regions/src/regions.witchy`
(`pub fn summarize(let words: List(String))`) — which is why its census line shows
only a warning: the process aborted rather than reporting an error.
Probes: `s_borrow.witchy`, `s_borrowstr.witchy`.

### B9. Packed scalar records are refused
`examples/records`, `examples/record_compiled` — both are literally
`type Point: x: Int; y: Int`, the textbook packed scalar record the milestone
names as covered. Error: ``unsupported canonical v1 layout type `records.Point` ``.
Probe `s_rec.witchy` reproduces with no interpolation involved.

### B10. Cross-module direct call to a user function
`examples/app/src/app.witchy` → `MissingBinding("strutil.shout")`.
`strutil.shout` is a `pub pure fn shout(name: String) -> String` in the same rune.
"Direct and indirect top-level calls" is in the subset.

### B11. Short-circuit `&&` / `||`
`examples/ranges` → `UnsupportedExpression("short-circuit binary operation")`.
Scalar control flow.

### B12. `return` in a block
`examples/guard` → `UnsupportedStatement("return in block")`. Early return is
ordinary scalar control flow; probe `p_return.witchy`.

### B13. Float arithmetic
`examples/temperature` → `UnsupportedExpression("non-integer arithmetic")`.
`Float` is a scalar; the subset says "scalar control flow" and nothing excludes
`Float`. Flagged as in-subset pending an explicit RFC exclusion.

### Borderline (call out, do not silently accept)
- `string.split` (`examples/wordcount`), `String__pad_left` (`examples/matrix`),
  `string.char_count` (`examples/maze`) — ordinary UTF-8 string operations
  reported as missing bindings. All three programs *also* use Dict (excluded),
  so the refusal is arguably fine, but the reported cause is a string op.
- `List(List(Int))` / `List(List(Bool))` / `List(Float)` — "scalar and string
  lists" does not say whether nesting or `Float` elements are covered.
- List-shape patterns (`examples/listmatch`) on a `List(Int)`.

---

## 3. Imprecise or misattributed diagnostics

1. **`MissingBinding("@render")`** names an internal builtin id, not the source
   construct. The user wrote `"${n}"`; the message should say string
   interpolation is unsupported. 9 files.
2. **`closure witness: canonical closure capture \`@render\` is not a resolved
   lexical binding or authenticated module reference`** — misattributes an
   unimplemented interpolation builtin to a closure-capture rule. 3 files
   (`list_ops`, `pipeline`, `sort`).
3. **`UnsupportedExpression("console.print used as a value")`** — `console.print`
   is not being used as a value; it is the last expression of an `if` branch
   whose value type is `Nil`. Blames the wrong construct.
4. **``canonical layout rejected: <tuple>.0: unresolved closed layout type `String` ``**
   (`examples/generics`, `examples/tuples`) — `String` *is* supported; the real
   cause is that tuples are unsupported. The message names the innermost
   supported type instead of the unsupported container.
5. **`UnsupportedConvention { function: "signature FunctionSignatureId(1)",
   parameter: "0", convention: Var }`** — names an internal signature id and a
   0-based index instead of the function name and the parameter name.
6. **`InitialValidation(Safety(InvalidOwnership { function: FunctionId(0),
   object: ValueId(28) }))`** — a raw internal id pair with no source span, no
   construct, and no explanation. This is the most common in-subset failure
   (every loop touching a capability) and it is the least diagnosable.
7. **`codegen error: canonical SafeWIR pipeline failed:
   Admission(Lowering("Lowering(UnsupportedControlFlow {...})"))`**
   (`examples/signs`) — an *unsupported-lowering* condition reported as a
   `codegen error`, with a doubly nested, escaped `Debug` string
   (`Lowering("Lowering(...)")`). It should surface on the
   `unsupported lowering:` channel like its siblings.
8. **`OwnedReleasePlacement("MissingToken(ValueId(6))")`** — internal value id,
   no source construct.
9. **`examples/regions/src/regions.witchy` produces no refusal at all** — it
   panics with `internal error: entered unreachable code`. The census recorded
   its unstable-`region` *warning* as the "first error line", so the crash is
   invisible in the raw census. Every excluded family is required to fail with a
   *precise canonical refusal*; a panic is neither precise nor a refusal.
10. Generic-specialization errors dump an unordered semicolon-joined list of
    mangled symbols (`List__sort_by__String; list.merge__String; ...`) with no
    source location.

---

## 4. The 56 "passing" files are all libraries with no `main`

All 56 lack a `fn main`; 55 are `*_test.witchy` in-rune test modules and one is
`examples/app/src/strutil.witchy`. Verified that `check` reports `ok` for them
*regardless of what they contain*: `examples/eval/src/eval_test.witchy` builds
and matches the sum type `Expr` (`Mul(Add(Num(2), Num(3)), Num(4))`) — an
explicitly excluded family — and still reports `ok`, while its sibling
`examples/eval/src/eval.witchy` is refused on `eval.Expr`. Canonical lowering is
never reached without a `main`, so `ok` here means "nothing was lowered".

Full list (all 56):
aliases, anagram, app(strutil_test + strutil), brainfuck, bst, calc, calculator,
closures, compute, constants, dedup, diff, dijkstra, durations, eval, generators,
generic_stack, generics, guard, hello, higher_order_sum, higher_order, jq,
largest, lazy_fib, let_patterns, life, list_ops, listmatch, matrix, maze,
minigrep, opt_mode, option_std, ownership, pascal, queens, ranges, regex,
regions, result, rle, roman, rpn, signs, sort, stats, strings, subscript,
sudoku, temperature, toposort, try, tuples, wrap
(each as `examples/<name>/src/<name>_test.witchy`, plus
`examples/app/src/strutil.witchy`).

**Consequence for the acceptance ledger:** any "56/163 admitted" figure
overstates coverage. The true runnable-program admission rate on the canonical
route is 0/107.

## Addendum (owner probes, 2026-09-05)

- **B14** `own` convention on a scalar parameter (`fn scalar(own value: Int) -> Int`)
  lowers and then the initial checker refuses `UnknownCallableContract { function }`.
  The refusal surfaces as `codegen error ... InitialValidation(Safety(...))`, i.e.
  `Rejected`, although the program is valid: a checker-side contract gap for `own`
  scalars is a canonical limit and should surface as an unsupported-lowering
  refusal at the lowering boundary, not as malformed compiler output.
- **B6 (confirmed)** `own` convention on a `String` parameter returned from the
  callee fails `OwnedReleasePlacement("MissingToken(ValueId(n)))`.
- The unit test `canonical_lower::tests::ordinary_own_identity_transfers_reference_without_clone_and_admits`
  pins exactly this pair and is left red on purpose; do not weaken it.

## Progress (owner, 2026-09-05, second slice)

- **B8 fixed**: an explicit `let` parameter (`Convention::Borrow`) is the default
  convention spelled out; it is now accepted at the signature, the function
  check, the call site, and the authority ABI comparator. `examples/regions`
  no longer panics; it stops at string interpolation.
- **B4 fixed**: the edge object-state check applies only to linear-memory
  references; capabilities, GC references, and callables threaded through a
  loop header carry no object state. The independent plan matcher also
  compared local kinds by two libraries' `Debug` renderings, which disagree for
  `externref`; both sides now normalize to one kind label. `while` loops in a
  function holding `console` run end to end.
- **B2/B3 fixed**: a unit function's final expression is lowered as an effect
  whatever its shape; `if` without `else` and `if`/`match` whose arms print
  now admit and run.
- **B11 fixed**: `&&` and `||` lower to explicit control flow with a `Bool`
  join parameter.
- **B5 fixed** (lowering owned it, see `BUG-canonical-match-cfg-unstructured-block.md`);
  the encoder's `find_join` additionally treats a terminating side as
  bypassing the join, and the plan's comparison table is now the exact mirror
  of the encoder's, so an exhaustive `match` without a wildcard admits.
- **B10 partially addressed**: declarations qualified by a user library module
  are in the canonical entry scope (std and playground modules are policy
  exclusions; mangled names never qualify). `examples/app` now reaches the
  B6 `String`-return release-placement gap instead of `MissingBinding`.
- Diagnostics: `WITCHY_CANONICAL_PLAN_DEBUG=1` prints the first divergence
  between the emitted body and the independently derived plan.
- New in-subset gap observed: `for i in lo..hi` range loops are refused as
  `UnsupportedExpression("range")` (`q_forin`). Not yet fixed.

## Progress (owner, 2026-09-05, third slice)

- **B6 fixed for literals**: a string literal passed to an owned-token
  parameter (`let`/`own`) was lowered as `CloneRef`/move of a static object,
  which the release placer refuses (`MissingToken`) and the checker's
  allocation-class rule forbids. The call site now materializes a fresh
  reference-counted copy (`clone_string_for_storage`), source-unobservable
  under value semantics. Returning a literal directly is unchanged: the
  backend's `CloneRef`/`DropRef` skip pointers below the heap base, so a caller
  holding a static result is safe, and the existing static-pointer pin stands.
- Census: 60 of 163 check; `app`, `ownership`, and `strings` now run end to end
  (the first runnable examples to admit).

## Progress (owner, 2026-09-05, fourth slice)

- **B12 fixed**: an early `return` as the last statement of an `if` arm lowers;
  the arm has no join edge and the other arm continues as the fall-through. The
  structured encoder and the plan mirror accept the continuing side's block as
  the merge when the other side only terminates.
- **B14/B6 (`own`) fixed**: the contract derivation mapped a returned scalar
  parameter to `Unknown` and marked scalar parameters as escaping with memory
  effects; scalars carry no memory provenance, so both now derive `Value`. The
  `own`-convention pin test is green.
- Diagnostics: contract refusals name their judgment under
  `WITCHY_CANONICAL_PLAN_DEBUG=1`.
- Census: 60 of 163 check.

## Progress (owner, 2026-09-05, fifth slice)

- **B15 fixed**: `for i in lo..hi` and `lo..=hi` range loops lower as a
  counted loop over an `Int` cursor (`lower_range_for`); `q_forin` and the
  launch test `admitted_range_for_loops_run` run through `spawn_admitted`.
- **B13 fixed**: `Float` arithmetic (`+ - * / %`) and comparisons lower; the
  checker's arithmetic judgment accepts `Float` operands for those five
  operators only, and the plan mirror gained the `f64` arithmetic, comparison,
  and `f64.const` tokens so the encoder's emitted body and the independent
  plan still compare token for token. `admitted_float_arithmetic_and_integer_division_run`
  pins Celsius conversion plus `/` and `%` on `Int`.
- Plan-matcher totality: `planned_arithmetic_token` maps every arithmetic
  operator for `Int` and `Float`, so an unmapped operator is a plan divergence
  named under `WITCHY_CANONICAL_PLAN_DEBUG=1`, never a silent `UnsupportedPlan`.
- Census: 60 of 163 check (unchanged). `temperature` now stops at
  `MissingBinding("math.to_float")`, a std cross-module call outside the
  compact subset, instead of `Float` arithmetic.

## Progress (owner, 2026-09-06, sixth slice)

- **B1 fixed**: `"${e}"` (the render intrinsic) lowers for `String` (identity)
  and `Int`. An `Int` renders through a static `0123456789` table with
  one-byte `CopyBytes` slices whose ends are trapping adds guarded against the
  object's count, and a branch-free sign copy of zero or one bytes from a
  static `-` table (`value >> 63` mask), so `Int::MIN` is never negated.
  `Float`, `Bool`, and `Duration` renders refuse by name. The digit tables are
  registered only when some render argument is checked as `Int`, because the
  checker refuses an unreferenced static segment.
- Checker fact discovered while doing so: a branch argument is renamed to its
  block parameter in every fact table, so a value whose facts are needed after
  a loop must never be passed as a loop argument; the renderer starts each loop
  from a dedicated copy.
- **Nested loops fixed (encoder)**: the loop-header test used global
  reachability, and an inner loop's exit reaches the inner header again through
  the enclosing back edge, so any loop nested in a loop was refused as an
  irreducible `BranchIf`. Both the encoder and the plan mirror now recognise a
  header by a dominated predecessor (a back edge) whose `then` side lies in the
  natural loop and whose `else` side does not, memoized per function.
- **B9 (qualified names) fixed**: the checker spells an entry-module type as
  `entry.Point` while the definition keeps the bare name; the layout
  projection and the packed-record map key both spellings. The plan mirror's
  inline-aggregate field map was per block, unlike the encoder's, so a packed
  field read after any block split failed admission; it is now function-wide.
  Ordinary heap records (`type Point:` without `packed`) remain outside the
  compact subset, and packed records as call parameters are not modelled.
- **B7 fixed**: a scalar `var` parameter is a value in and a result lane out,
  keeping the write-back marker: the parameter carries no memory token, the
  call site passes a value, the callee's exit returns the current binding, and
  the release placer skips scalar lanes. Direct and indirect calls both write
  back; `examples/mutate` runs.
- **Early `return` inside a loop over a list parameter fixed (checker)**: the
  loop header refused to join because extent promotion is lazy: the entry edge
  carried the list's header-prefix extent and the back edge the counted extent
  promoted by the body's element read. The join now promotes both sides from
  their own facts before comparing; nothing is widened. `examples/guard` runs.
- Census: 66 of 163 check; 10 runnable examples admit and agree under
  `witchy parity` (`app`, `compute`, `constants`, `fizzbuzz`, `guard`,
  `hello`, `mutate`, `ownership`, `signs`, `strings`).
- Diagnostics: `WITCHY_CANONICAL_LOWER_DEBUG=1` dumps each lowered function
  after release placement; `WITCHY_CANONICAL_PLAN_DEBUG=1` now also prints the
  two object states at a refused memory join.

## Progress (owner, 2026-09-06, seventh slice)

- **Exit drops after loop rebinding fixed**: the lowering dropped owned
  parameters at every exit by their entry value ids, but a loop threads list
  parameters through block parameters, so the placer's token lived under the
  current binding and the entry-id drop was a `MissingToken`. Exit drops now
  resolve each parameter to its current binding.
- **`var` alias of a shared reference fixed**: `var next = placed` bound the
  parameter's token to a second name; the copy-on-write push then dropped a
  token the parameter still owned. A `var` initialised from a reference another
  binding holds now clones its own token (a static string materialises an owned
  copy) and authenticates its own header.
- **Guarded `match` arms with a short-circuit guard fixed**: the guard's
  `BranchIf` was installed on the arm block the guard started in, overwriting
  the `&&` lowering's terminator and leaving its join block unterminated
  (`MissingReturn`). The guard now terminates the block it ended in.
- **Nested list indexing fixed**: an element of a list of lists carried no
  recorded extent; `lower_index` loads it once.
- **`if` join of a rebound list fixed**: a list rebound in only one arm reached
  the join without extent facts, so an enclosing `while` had no count lane at
  its back edge; rebound references load their header at the join.
- **Checker join (extent demotion)**: a back edge may carry a counted extent
  whose count was loaded inside the loop body, which the entry edge cannot
  know. When two states of one object differ only in that one side holds the
  header-prefix form and the other the counted form with the same header size,
  the join keeps the header-prefix form (the weaker fact of the same object);
  the body re-derives the count from its own loads. The same holds for two
  counted forms whose count and capacity were loaded on different paths (the
  entry's header loads versus a loop's carried lanes). Nothing else may differ.
- Census: 68 of 163 check; 12 runnable examples admit and agree under parity
  (`ranges` and `subscript` are new). `queens` now stops at a release-placement
  `MissingToken` inside its recursive search and still needs `list.at`, a std
  call outside the compact subset. `listmatch` needs list patterns
  (`[]`, `[x, ..rest]`), not yet lowered.

## Progress (owner, 2026-09-06, eighth slice)

- **Join result lanes own their token**: an `if`/`match` arm whose value is a
  reference some binding also holds (`if done: placed else: ...`, or
  `if flag: left else: right` over two parameters) now clones that reference
  for the result lane. The old pin that such a join must refuse is replaced:
  the join is no forged token, each arm clones the parameter it returns, and
  both parameter tokens release at the return. `queens` now lowers entirely
  and stops at `list.at`, a std call outside the compact subset.
- Loaded strings record their capacity lane (a byte buffer's authenticated
  capacity is its count), so a join over two string parameters is a shared
  phi; the checker promotes a shared-phi argument's header extent at the edge
  from the facts already in the state instead of waiting for an element access.
- **Named gap (next)**: a join whose arms contribute a parameter-derived
  shared reference and a fresh unique allocation (`if xs.length() == n: xs
  else: <var out = xs; out.push(n); out>`) is still refused
  (`IncompatibleMemoryJoin`): the two object states differ in ownership
  (`Shared` versus `Unique`), allocation class (`None` versus
  `ReferenceCounted`), and the immutability flags. Admitting it needs a
  reviewed ownership weakening (unique to shared at a shared phi) plus a join
  rule for those three fields; that is a checker-model decision, not a
  lowering fix, so it is recorded rather than pushed in.
- Census unchanged at 68 of 163 check and 12 runnable examples with parity.

## Progress (owner, 2026-09-06, ninth slice)

- **Proper tail calls inside `if`/`match`/block arms fixed**: the lowering
  recognised a proper-tail site only as the function's final statement, while
  the manifest also marks calls in tail arms, so every tail-recursive function
  written with an `if` or `match` was refused by the checker as
  `MissingTailObligation`. A final `if`/`match`/block that contains nested
  manifest sites is now lowered in tail context along the manifest path
  (`Node(If)/Child(1|2)`, `Node(Match)/Arm(k)/Value`, block statements): each
  arm exits the function, with a proper tail call at every marked site and a
  direct return elsewhere; there is no join. Intrinsic sites (a host print, a
  header read, a render) are exempted wherever they sit, by resolving the
  manifest path to the expression it names.
- RFC-0090 resource evidence now exists through `spawn_admitted`: 300,000 self
  and mutual tail transitions inside `if`/`match` arms run with a 256-slot
  budget (`admitted_proper_tail_calls_inside_if_and_match_arms_run_in_constant_stack`),
  and `tests/rfc0090/tail_positions.rs` passes on the canonical route.
- Census unchanged at 68 of 163 check and 12 runnable examples with parity.

## Progress (owner, 2026-09-06, tenth slice)

- **Shared-versus-fresh join decided and fixed**: the checker now weakens a
  unique reference-counted token to shared at a shared phi, under the same
  reasoning as a call weakening a unique argument into a shared parameter
  (one token, write authority lost), and only for a sole live token. The meet
  of two shared managed states of a phi object keeps only what both establish:
  the allocation class survives only when both name it, the object is
  immutable, and owned slots count as initialized only if both sides say so.
  Identity, layout, extent, and ownership must agree exactly. The lowering
  treats a fresh reference-counted allocation as eligible for the shared phi.
  `if xs.length() == n: xs else: <fresh list>` runs (launch test).

## Progress (owner, 2026-09-06, eleventh slice)

- **List patterns fixed**: `[]`, `[a, b]`, `[a, ..]`, and `[a, ..rest]` lower
  over lists of scalars. The length test (exact, or at least the named
  prefix) is the arm's dispatch condition; element bindings are guarded
  scalar loads emitted in the arm block, and `..rest` is a fresh list holding
  a copy of the remaining elements (value semantics, source not consumed),
  whose slice bounds are the trapping-add facts the checker's range rule
  consumes. Nested element patterns and lists of references refuse by name.
- **Encoder**: a branch side that is a straight chain of unconditional
  branches ending in a terminal block (the release placer's drop edge block
  ahead of a trap) counts as terminal, provided the chain touches no block the
  other side reaches without passing back through the branching block. Two
  wrong drafts of this rule (any acyclic side; any block the other side reaches
  at all) mis-joined ordinary matches and a returning arm inside a loop; both
  were caught by the launch tests and the census before commit.
- **Join lanes and tokens**: a static lane joined with a token-carrying lane
  is materialized into an owned copy in its own arm, so the placer sees one
  token shape on every edge. Token-ness follows the explicit CFG: an owned
  entry parameter, a minting operation, or a block parameter fed by a
  token-carrying argument (loop parameters), never a static or a join over
  statics. Concat, render, and copied strings record their capacity lane, so
  string joins over them are shared phis.
- Census: 69 of 163 check; 13 runnable examples admit and agree under parity
  (`listmatch` is new). `queens` stops only at `list.at`.

## Progress (owner, 2026-09-06, twelfth slice)

- **B11 launch entry bound into the bytes**: admission encodes the derived
  `CanonicalEntry` (export, capability tags, result convention) as a
  `witchy.entry` custom section, emits it after `witchy.launch`, and matches it
  exactly during exact-byte admission (section order, payload), so the entry
  is covered by the artifact's Wasm digest. A launcher that has only the
  `.wasm` file reads it back (`witchy_wir::canonical_entry_from_wasm`, a
  module outside the private encoder, which the route gate keeps to admission
  alone); `Runtime::spawn` instantiates such bytes exactly (no Binaryen, no
  cache) and enters through the entry, and the CLI's exit-code convention reads
  the entry's result kind. `witchy compile --out app.wasm` then `witchy
  app.wasm` prints and exits with the `Int` result.
- **Historical reds restored, not hidden**: the nine `witchy-wir` tests red
  at `HEAD` are green. Six shared an allocate-only fixture that predates
  callee-drops (it now releases its token); the pointer-list fixture returns
  its clone and drops the container; the reference-count boundary fixture
  gets the destructor's five scratch slots; the two checker pins release the
  buffers they allocate; the pointer-bearing catalog pin now asserts the
  earlier checker refusal (no recursive destructor for that layout).
- **Encoder defect found by the restored fixture**: the recursive destructor
  stored a loaded child pointer in the current depth's fourth scratch slot
  while the recursive drop read the next depth's first slot, which nothing
  set, so a dropped list of owned pointers never released its children (a
  leak, covered identically by the plan mirror and therefore admitted). Both
  sides now store the child in the next depth's slot; the restored test pins
  the child's count after the container's drop.
- New in-subset refusal recorded: pushing into a `var` list of strings is
  refused by the checker as `UpdateCount(pointer prefix missing)`.

## Progress (owner, 2026-09-06, thirteenth slice)

- **Lists of strings fixed**: reading an element of a `List(String)`
  parameter, iterating one, pushing into a local list of strings, and a `var`
  string accumulator rebound in a loop all run (launch test
  `admitted_lists_of_strings_are_read_iterated_and_pushed`, parity agreeing).
  - Checker: a managed parameter's `initialized_owned_slots` is the ABI's
    statement that every slot below its runtime count holds an owned
    reference-counted element, so header-extent promotion (and the element
    load fallback) seeds that prefix as a range fact instead of refusing
    `UpdateCount(pointer prefix missing)`.
  - Lowering: a `var` initialised from a static string owns a copy from the
    start; loops thread strings like lists (length and capacity lanes, a
    unique phi for an owned string, a shared phi for one from a shared
    source); list lanes in `while` and range loops keep their previous
    unspecified ownership, since a shared list parameter threaded through them
    must not meet a unique phi (a draft that marked them regressed
    `subscript` and the recursive-search test, caught before commit).
  - Extent lanes across any edge are distinct value ids: the checker renames a
    duplicated branch argument to its last parameter, which put a string's
    count on its capacity lane; a string's capacity lane is now a fresh copy
    at loop entries, back edges, and `if`/`match` result joins.
- Still outside the subset: pushing into a copy of a `List(String)` parameter
  (moving elements out of shared storage; pointer-bearing `CloneRange` is
  fail-closed until the typed transfer kernel exists).

## Progress (owner, 2026-09-06, fourteenth slice: execution oracle)

- **Executable SafeWIR oracle**: the checker-side interpreter now models
  capability tokens, reference-bearing loads and stores, byte and range
  copies and moves, recursive release of a dropped object's children, and the
  console print host call (lines recorded with the host's trailing-newline
  rule). `interpret_program` runs an entry as the launcher would and reports
  the printed lines, the result, and the count of reference-counted objects
  still live at return.
- **Wasm heap audit**: admitted modules export their allocator globals (bump
  pointer, free-list head, heap base); `Vm::canonical_heap_leaks` walks the
  heap after the entry returns and counts objects not on the reclaimed free
  list. The development runner refuses a program that leaks.
- **Three-legged parity**: `witchy parity` runs the oracle on the validated
  graph admission encoded and requires identical output, identical
  success/failure, and no surviving object; any disagreement, including the
  oracle refusing a program the backend ran, is loud.
  `tests/misc/rfc0165_oracle.rs` pins the oracle over the foundation programs
  and every admitted example. This is the guard the destructor scratch-slot
  leak would have tripped.
- `list.at` lowers as the bounds-checked element read (`xs.at(i)` is `xs[i]`);
  `queens` now clears lowering and stops at a checker join in `solve`.

## Progress (owner, 2026-09-06, fifteenth slice: canonical scope by reachability)

- **Canonical scope follows reachability**: the scope is now the entry scope
  plus every function reachable from it by exact name, transitively, through
  standard-library bodies and monomorphized instances (call sites name their
  instances after specialization). Generic templates reached by name are not
  in scope (only their instances are), and a callee named like a compiler
  intrinsic (`string.chars`, `list.push`) never pulls in the std wrapper of
  the same name. One judgment (`witchy_types::pipeline::canonical_scope`,
  memoized per module) feeds the proper-tail manifest, the callable witness,
  the layouts, and the lowering, so their ordinals agree. The forged
  judgment stays name-based: an instance claiming the entry scope's namespace
  without an exact checked body still refuses; an unreachable library
  instance is still a digest-bound obligation.
- Consequence: reachable library specializations (`util.identity__Int`, a
  std `math.max`) are runtime functions and lower; the pins that expected them
  to be obligations now expect admission. Programs that used to stop at
  `MissingBinding`/`UnsupportedCanonicalObligation` now stop, precisely, at
  the first construct inside the reached std body the subset lacks:
  `fail`, `string.length`, `int_to_duration`, `regex.match_spans`, lambdas in
  `List.filter`/`map`, `Option` layouts. The census count is unchanged (69
  check, 13 runnable) until those land.
- **Oracle step budget** raised so long-running admitted programs are not
  refused by the oracle's own limit.

## Progress (owner, 2026-09-06, sixteenth slice: joins, loops, allocator growth)

- **Rebound bindings at `if`/`match` joins** get a phi with extent lanes: an
  alias of another binding is cloned, a static lane next to a token lane is
  materialized, the phi is unique when every arm holds its own token and
  shared when every arm is shared-phi eligible; otherwise the join keeps
  demanding one identity. Loop headers mark a lane only for a name the body
  rebinds: shared for a shared source, unique when the body pushes in place,
  shared when it merely reassigns (so the header agrees with a body-local
  `if` join); `for` loops keep their unique rule for lists.
- **Allocator grows linear memory on demand** (encoder and plan mirror): an
  object past the current memory grows by the pages needed, and a refused
  growth traps before any write. This was a backend divergence the oracle
  caught: `regions` pushes 1000 strings into a copy-on-write list and ran out
  of the single initial page while the oracle, with unbounded cells,
  succeeded. The allocation pin now expects growth, and a trap only under a
  host memory cap.
- **Encoder terminal side**: a side terminates when every path from it ends in
  a terminal block through structured loops and it shares no block with the
  other side (a `fail` arm that renders its message loops before trapping).
- `fail(msg)` lowers as a trap after evaluating its message and must end its
  block; `string.length` reads the byte length. Clones share the source's
  header facts (checker and lowering agree on the extent lane ids), and a
  shared managed object's class and immutability facts meet at any block,
  with a capacity-only extent difference resolved at the count; a consumed
  token's residual facts never block a join.
- Census: 71 of 163 check; 15 runnable examples admit and agree under
  three-legged parity (`math_demo` and `regions` are new).


## Progress (owner, 2026-09-06, seventeenth slice: structured loops with late exits)

- **Loop headers are back-edge targets, not `while` shapes.** The encoder and
  its plan mirror recognized a loop only when the header's own `BranchIf`
  chose between body and exit. `while a && b` (and `||`) decides the exit in
  a later block after a boolean phi, so `queens` was refused as an
  "irreducible" branch. Now any dominated back edge names a header; a loop
  has one exit (the `while` header's `else`, otherwise the one non-terminal
  block the loop branches out to; `return` blocks are emitted inline); the
  canonical `while` form keeps its established bytes, and any other header is
  emitted structurally inside `block { loop { … } }` with branches back to
  the header and out to the exit as `br`. A loop that only leaves by
  returning is followed by `unreachable` so the function's result stack
  stays valid. `all_paths_reach_or_terminate` takes a merge set so a body
  path may end at the header or the exit.
- **Index expression types**: typeck inferred `xs[i - 1]` through a cloned
  transient, so the source index expression had no table entry and the
  lowering refused it. The index is now inferred directly as well.
- Pinned by `admitted_while_loops_with_short_circuit_conditions_run`
  (`&&`, `||`, early return inside the loop, compound index).
- Census: 72 of 163 check; 16 runnable examples admit and agree under
  three-legged parity (`queens` is new).

## Progress (owner, 2026-09-07, eighteenth slice: byte scans, converging loop exits)

- **Raw byte reads.** A string's element is a raw byte with no value type;
  the checker now types a read of one as a zero-extended `Size` (writes stay
  byte copies), the encoder and plan mirror load it with `i32.load8_u`, and
  the oracle already decoded bytes as sizes. On that: string `==`/`!=` is a
  length test guarding a byte-scan loop with an early exit, and
  `string.char_count` counts UTF-8 sequence starts without a branch
  (`(((b >> 6) ^ 2) + 3) >> 2`).
- **Loops whose exits converge.** Release placement puts a drop block on
  each edge out of the byte scan (length mismatch, first differing byte),
  so the loop has several exit edges. The loop's continuation is now the
  nearest block every exit edge reaches; the edge blocks are emitted inside
  the loop ahead of their `br`. An edge whose paths only return through
  blocks nothing outside the loop enters is an early return, emitted
  inline; a return block shared with a path from before the loop (the
  `queens` search) stays a continuation. A branch side "escapes" when
  every path from it ends at an enclosing loop's header or exit and shares
  no block with the other side. The path check shares its visited set
  through loop bodies (fresh sets let a walk re-enter an enclosing loop
  forever), and a side's own first block may be the merge when the other
  side flows into it.
- **`for` carried bindings.** The `for` lowering scanned only the body's
  top-level statements for rebinding, so `n = n + 1` inside an `if` was
  dropped and the loop silently kept the entry value. `witchy parity`
  caught it (the oracle runs the same SafeWIR and cannot). The nested
  rebinding scan the `while` lowering uses now selects the loop state.
- Pinned by `admitted_for_loops_carry_bindings_assigned_inside_nested_arms`
  and `admitted_string_equality_and_char_count_scan_bytes`.
- Census: 72 of 163 check; 16 runnable examples admit and agree under
  three-legged parity.

## Progress (owner, 2026-09-07, nineteenth slice: nested lists, scoped facts, shared prefixes)

- **Nested list storage.** A list stored into a list (`out.push(column)`,
  `[a, [3]]`) is copied into a fresh exact-count allocation through
  `CloneRange` (scalar elements only; a list of references still needs
  explicit ownership).
- **Region-scoped extent facts.** The lowering cached header loads (string
  lengths, list counts and capacities) function-wide, so a length loaded in
  one `if` arm was reused in the other arm, where it did not dominate. Each
  `if`/`match` arm now starts from the facts before the branch, the arms'
  facts are merged back for the join's lane planning, and a loop's exit
  restores the facts before the loop, keeping the join's own parameters.
- **Zero-start prefixes across joins (checker).** A prefix fact whose start
  constant was defined in a loop body loses that constant when it is
  renamed through the header; the `zero_start` flag now matches a zero
  start both when a `MoveRange` consumes the prefix and when two prefixes
  meet at a header, so a list of strings pushed in both arms of an `if`
  and again in a later loop keeps its initialized prefix.
- **Shared-marked prefixes move.** A dynamic shared element read marks the
  prefix shared; relocating the owned slots keeps every clone valid, so a
  `MoveRange` accepts the mark and carries it to the new allocation
  (`lcs_table` reads a row then pushes the next).
- Pinned by `admitted_string_pushes_in_arms_and_across_loops_keep_the_prefix`
  and `admitted_list_of_lists_grows_after_a_shared_element_read`.
- Census: 73 of 163 check; 17 runnable examples admit and agree under
  three-legged parity (`diff` is new).
