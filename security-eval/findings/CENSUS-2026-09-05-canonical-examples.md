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

## Progress (owner, 2026-09-07, twentieth slice: substring, trim, joins inside loops)

- **`string.substring`** (Unicode-scalar indices, clamped, empty when
  inverted) finds both byte offsets with a branch-free sequence-start count
  and copies the bytes between them; **`string.trim`** scans the ASCII
  whitespace set both backends strip (space, tab, LF, VT, FF, CR) from both
  ends. Each scan is a loop that decides its exit after the header, and a
  loop argument must be a dedicated copy (the checker renames an argument
  to the header parameter in every fact table, so passing the length
  directly took the length fact with it).
- **Joins inside loops (encoder).** A branch side that reaches the
  enclosing merge flows into it rather than escaping (emitting it inline
  emitted the merge twice); a join candidate's paths may leave through the
  enclosing loop's exit or back edge instead of reaching the merge; and
  candidates come only from paths that stay in the loop region, so a block
  reached through the back edge (the next iteration's `if`) never wins on
  distance. `fizzbuzz` regressed on the last of these for one build and is
  pinned by the census.
- Pinned by `admitted_substring_and_trim_scan_characters_and_ascii_whitespace`.
- Census: 75 of 163 check; 19 runnable examples admit and agree under
  three-legged parity (`matrix`, `loops`, `fizzbuzz` are new).

## Progress (owner, 2026-09-07, twenty-first slice: float conversions, Bool render, duration identities)

- **One unary float operation** (`FloatUnary`: `Sqrt`, `TruncateToInt`,
  `FromInt`) joins the SafeWIR vocabulary across the checker, the oracle,
  the encoder (`f64.sqrt`, `i64.trunc_sat_f64_s`, `f64.convert_i64_s`), the
  plan mirror, and the history digest. `math.to_int` traps on NaN through a
  separate terminal block before the saturating truncation (a guard's
  condition must be an unsigned order, so the NaN test is a branch).
- **`Bool` renders** from the static `falsetrue` table: a branch selects the
  word's offset and length as a scalar phi and the bytes are copied.
- **Duration conversions** are identities at runtime. At a tail position the
  statement lowering tail-called any name it did not bind, and typeck's
  tail-site classifier counted the call as a proper tail site; both now
  treat an intrinsic-named call (the catalog and the duration conversions)
  as a value.
- Pinned by `admitted_float_conversions_bool_render_and_duration_identities_run`.
- Census: 76 of 163 check; 20 runnable examples admit and agree under
  three-legged parity (`temperature` is new).

## Progress (owner, 2026-09-07, twenty-second slice: heap records)

- **Ordinary records are immutable reference-counted heap aggregates.** A
  one-variant record with scalar fields (not declared `packed`) lowers to a
  fixed-size RC allocation with a field store per field; a field read is a
  field load; a spread update allocates a new record and loads the fields
  it does not name; a copy is a reference clone; release placement drops it
  like any other reference. The layout catalog resolves such a record as a
  packed-record layout viewed as packed (the interner accepts only packed
  one-variant definitions), so the inline and heap forms share one field
  layout and differ only in value type (`InlineAggregate` versus `Ref`).
- **Layers touched.** Checker: a scalar-field record or tuple drops by
  reclaiming its bytes. Encoder and plan mirror: fixed-size allocations
  (constant size, no header stores) and field projections (object plus
  field offset, no index). ABI authority: a nominal record parameter or
  result binds to a reference whose layout carries exactly the declared
  scalar fields, looked up from the checked module's definitions. Lowering:
  the record table now covers packed and heap records, the signature
  derivation maps a heap record to a reference, and a rebound record joins
  through a phi without count/capacity lanes.
- Pinned by `admitted_heap_records_construct_read_update_join_and_cross_calls`.
- Census: 79 of 163 check; 23 runnable examples admit and agree under
  three-legged parity (`records`, `record_compiled` are new). `opt_mode`
  stays out: it declares a `unique` record, a deferred v1 qualifier.

## Progress (owner, 2026-09-07, twenty-third slice: sum types, Option, Result over scalars)

- **One scalar aggregate shape** (`witchy_types::canonical_aggregates`) now
  serves the layout catalog, the lowering, and the ABI authority: a record
  is its fields; a sum type is an `Int` tag in slot 0 followed by every
  variant's fields in declaration order, so each payload has one static
  slot and a construction initializes every slot (zeros elsewhere). Scalar
  `Option(T)` and `Result(T, E)` are the same shape under the keys
  `Option(Int)`, `Result(Int, Bool)`, interned as nominals without
  arguments because the interner's packed-record rule takes none.
- **Lowering.** Constructors allocate the aggregate; a constructor pattern
  compares the tag (a one-variant record's pattern always matches) and
  binds payload slots in the arm; `??` loads the payload or evaluates the
  default in its own block; `?` returns the function's own failure (`None`,
  or `Err` carrying the same scalar error) from a terminal block. A
  fixed-layout `if`/`match` result joins through a phi without extent lanes,
  unique when every arm holds a fresh token.
- Pinned by `admitted_sum_types_option_and_result_over_scalars_run`.
- Census: 83 of 163 check; 27 runnable examples admit and agree under
  three-legged parity (`shapes`, `commands`, `list_more`, `option_std` are
  new). Left in this family:
  lists of aggregates (`equality`), `string.ends_with` (`parse_kv`), string
  ordering (`rle`), unions with reference payloads (`rpn`, `bank`).

## Progress (owner, 2026-09-07, twenty-fourth slice: string searches, ordering, find, to_int)

- **String intrinsics over byte loops.** `starts_with`, `ends_with`, and
  `contains` compare byte ranges (every read guarded against its own
  string's length, so no range proof is needed); `<`/`<=`/`>`/`>=` on
  strings compute the sign of the first differing byte, else of the length
  difference (UTF-8 byte order is code-point order); `find` reports the
  character index of the first matching window or `-1`; `to_int` parses
  the signed decimal between the surrounding ASCII whitespace, accumulating
  negatively so the minimum parses, with explicit range checks (the
  encoder emits a trapping `Add` only) and a trap on junk or overflow.
- **Encoder path-fact memo.** The structuring's path check enumerated paths
  afresh at every branch; a function with a dozen sequential loops spun
  for twenty minutes. A proven "every path from this block reaches one of
  these merges or terminates" fact never depends on the walk's active set
  (a cut cycle only yields a negative answer), so both mirrors memoize the
  positive answers.
- Pinned by `admitted_string_searches_ordering_find_and_to_int_scan_bytes`.
- Census: 84 of 163 check; 28 runnable examples admit and agree under
  three-legged parity (`parse_kv` is new). `rle` now needs dynamic
  `string.chars` (a UTF-8 decoder into a list of strings).

## Progress (owner, 2026-09-07, twenty-fifth slice: reference payloads in aggregates)

- **Strings and lists inside records and sums.** An aggregate field may be
  a `String` or a scalar list: it lives behind an owned pointer slot in the
  record layout, which the layout catalog now builds directly (the
  interner's type resolver cannot place a reference inside a record). A
  construction copies the payload into a fresh object (value semantics)
  and fills every other reference slot of a sum with a fresh empty value,
  so every owned slot holds an object for the destructor. A field read or
  pattern binding takes a shared clone.
- **Checker.** A record field is a constant reference slot; once every
  owned pointer field holds a stored reference the object is complete and
  the summary flag replaces the per-slot facts (a join could not reconcile
  those across fresh identities); a complete object's field load yields a
  shared child; drops of fixed layouts descend into their pointer fields;
  reclaiming a pointer-bearing fixed layout is refused. Record children
  interned as pointer value slots are inline fields whose layout is the
  slot, which every field walk recognizes.
- **Encoder and mirror.** Field projections address pointer fields as
  32-bit object addresses with their pointee; the destructor of a fixed
  layout drops the child behind each owned pointer field; the scratch
  pool counts that depth. The ABI authority binds a reference field to the
  slot's pointee layout.
- **Lowering.** `??` over a string payload materializes a static default
  and joins through a shared phi with extent lanes; string literal
  patterns are equality tests against static literals collected from
  patterns; string concatenation loads operand extents lazily.
- Pinned by `admitted_aggregates_with_string_payloads_construct_match_and_propagate`.
- Census: 88 of 163 check; 32 runnable examples admit and agree under
  three-legged parity (`try`, `roman`, `record_update` are new).

## Progress (owner, 2026-09-07, twenty-sixth slice: tuples)

- **Tuples are anonymous aggregates**: `(Int, String)` is a one-variant
  record of its items behind one reference, keyed by its rendering, with
  a nested tuple behind its own pointer slot. Construction, `.n` access,
  `let (a, b) = …` (also inside tail blocks), tuple patterns of names and
  wildcards, tuples as `Option` payloads, and tuples as function results
  all go through the aggregate machinery; the ABI authority binds a tuple
  parameter or result to a reference whose slots match its items.
- **Loop exits stay a reachability question.** The early-return filter for
  a loop's leaving edges used the structured path check, whose walk from
  one loop's exit descended into the next loop's exit computation; a large
  function overflowed the stack. An edge only returns when its reach
  (never re-entering the header) holds no loop header, which is acyclic
  with terminal sinks.
- Pinned by `admitted_tuples_construct_index_destructure_and_nest`.
- Census: 91 of 163 check; 35 runnable examples admit and agree under
  three-legged parity (`tuples`, `generics`, `calculator` are new).

## Slice 27 (2026-09-07): lists of aggregates, tuples, and floats

Element predicates are definitions-aware (`canonical_aggregates::is_v1_list_element_type`,
`is_v1_field_type`, one nominal level, `Bool` elements still excluded); an aggregate element
sits behind an owned pointer slot to its record layout (`list_layout`, `aggregate_layout`,
authority `layout_matches_source`). New checker/encoder ground: pointer-bearing `CloneRange`
(retains every element, mirroring `CloneRef`; both prefixes become shared; the plan mirror and
the oracle retain too), loaded children and elements are complete (`Store` now requires the
stored value complete), stored-slot and range facts resolve through the allocation identity so
a `CloneRef` alias sees them. Lowering: list growth clones the old prefix instead of moving it
(the old list may be shared), a `var`-bound reference passed to a callee is copied (its token
stays unique for a later push), fixed-layout loop lanes follow the general shared/unique rule,
and `known_shared_value` sees through unspecified body block parameters.

Newly agree: `bank`, `display`, `stats`, `zip`, `toposort` (plus alias-of-literal pushes).
Census: 97/163 check, 41 runnable three-legged parity, 0 disagree. Still refused: `largest`
(nested constructor pattern), `inventory` (dict), `dijkstra` (Bool list).

## Slice 28 (2026-09-07): dynamic `string.chars`, `string.split`, `string.replace`

Two shared lowering kernels: a UTF-8 sequence loop (sequence length from the lead byte) and
a non-overlapping match-scan loop, both folding owned pieces into an accumulator threaded as
`[object, count, capacity]` lanes through owned block parameters (`new_block_owned`,
`accumulator_lanes`, `split_folded_lanes`); the push template is now `push_owned_element`
and concatenation `concat_string_values`. Semantics follow the interpreter: an empty
separator yields the whole string; an empty `from` surrounds every character.

Fixes surfaced on the way: fresh copies decouple their extent from the caller's length id
(`copy_string_slice`), `push_empty_value` lists get distinct count/capacity ids, the while
exit records string lanes in the string tables, a tail-position `if` scopes each arm's
facts, a join lane authenticates an untouched arm's extent at that arm's end (scoped), call
results count as shared for phi planning, and the shared-value trace treats a loop cycle as
neutral. The checker's dataflow now holds a block's failure until its states converge and
reports only a failure that stands afterwards (a kernel loop's exit is visited with the
entry state first).

Newly agree: `rle`, `rpn`, `wrap`, `regex_demo`. Census: 101/163 check, 45 runnable
three-legged parity, 0 disagree. `anagram`/`text` now stop at lambdas; `wordcount` at dicts.

## Slice 29 (2026-09-07): Bool lists, `for` over loaded rows, copies of shared lists

`List(Bool)` elements are the one-byte `ScalarKind::Bool` scalar (the layout lookup and the
ABI authority matched the legacy 8-byte `BoolValueSlot`, which the checker refuses as a
value). A `for` source without count facts (an element loaded out of a list of lists)
authenticates its header. A pointer-bearing `CloneRange` accepts any managed source (a
parameter's class is unknown); the `for` header rule no longer forces a unique phi on a
lane holding a known-shared value. Newly agree: `dijkstra`, `life`, `sudoku`. Census:
104/163 check, 48 runnable three-legged parity, 0 disagree. Remaining in-subset: `while let`
(needs `break` and type-table entries for the desugared scrutinee), `Float` render,
`largest` (generic + trait bound).

## Slice 30 (2026-09-07): `while let`, `break`, `continue`, nested tuple payloads

Typeck infers a `while let`'s source scrutinee, pattern bindings, and body directly (the
transient desugar keeps the loop's other checks), so the lowering reads their types. The
lowering keeps a loop-context stack: `break` leaves through the exit with the state lanes
(the exit takes the header lanes' phi authority when a body breaks), `continue` re-enters
the header advancing a `for` cursor or range counter. `while let` is a loop whose header
binds the scrutinee (threaded into the body as its own lane, or the lane's body parameter
when the scrutinee is a lane) and tests the pattern; a tuple payload inside a constructor
pattern destructures through its slot. Newly agree: `let_patterns`. Census: 105/163 check,
49 runnable three-legged parity, 0 disagree.

Known limit (not pursued: coverage expansion stops here per the plan correction): a `var`
alias of a list threaded beside its source as loop lanes shares the source's extent ids,
which the edge renaming cannot tell apart (`var rest = stack; while let [top, ..tail] = rest`).

## Slice 31 (2026-09-07): capture-free scalar lambdas as function values

A lambda whose closure witness records no captures and whose parameters and result are
scalars becomes a function of its own: `canonical_closure::lambda_sites` yields each lambda's
structural identity and node address; the lowering synthesizes a function per site (lowering
the original body node, since the type table is keyed by node address), emits `FunctionValue`
at the use site, keys tail sites inside the lambda by its `lambda_path`, and binds the function
to its closure witness (`CandidateClosureBinding`, digest-bound) rather than a source
declaration; the authority admits such a binding only when the witness exists, captures
nothing, and carries the function's signature. Capturing lambdas and lambdas over references
are refused by name. `ast::visit::visit_block`/`visit_expr` now hand the visitor references
with the block's lifetime. Newly agree: `list_pipeline`, `predicates`, `std_demo` (std `map`,
`filter`, `fold`, `all`, `any` over `List(Int)` with lambdas). Census: 108/163 check, 52
runnable three-legged parity, 0 disagree.

## Slice 32 (2026-09-07): lambdas over references, `as`, tail sites inside lambdas

Indirect calls accept reference arguments under the direct-call conventions (the checker's
move-into-shared rule already admitted them; the lowering's refusal is lifted), so lambdas
over strings and lists work, including through the std combinators. `as` is the value itself
when both sides share one representation (capability narrowing, alias ascription). A tail
site inside a lambda body is addressed under the lambda's path plus `Child(0)` and is
attributed to its owner in the checked manifest while the synthesized function discharges it
(`ProperTailSite::discharged_by`, outside the digest identity). A loop header lane that
started unique but is carried a known-shared value on a back edge (a call result) becomes a
shared phi. Newly agree: `aliases`, `anagram`. Census: 110/163 check, 54 runnable
three-legged parity, 0 disagree. `text` still needs `string.to_upper` (an ASCII byte map in
the interpreter; needs a raw-byte element store rule).

## Slice 33 (2026-09-07): `string.to_upper` / `string.to_lower`

ASCII case mapping (the interpreter's rule) over a fresh copy, one byte at a time and
branch-free (the sign bit of `(b - low) | (high - b)` selects the shift). The checker types
a raw-byte element write as a `Size` store the path must prove fits a byte (`proven_at_most`
on a guard against a constant), and the backend stores it with `i32.store8`. Newly agree:
`text`. Census: 111/163 check, 55 runnable three-legged parity, 0 disagree.

## Slice 34 (2026-09-07): `@render` in lambdas, nested constructor patterns, write-back lanes

The render intrinsic referenced inside a lambda body is an authenticated intrinsic, not a
lexical capture (the closure witness had refused `@render` as an unresolved capture). A
refutable payload pattern (`Some(Less)`, `Some(Circle(r))`) tests its slot only once the
outer tag test selected the variant: the payload test sits behind the tag test as a
short-circuit conjunction (`short_circuit_and`), and the arm block binds nested payloads
through their own slot loads. The derived `PartialOrd` on a record is exactly this shape.
An element read from a list of records no longer loads a string extent (only a string
element carries one). A `var` record a loop body passes to a write-back parameter is
rebound by that call as an assignment rebinds it, so its header lane is a shared phi and
the write-back's unique result weakens into it on the back edge. Newly agree: `dice`,
`largest`, `list_ops`, `pipeline`, `sort`. Census: 116/163 check, 60 runnable three-legged
parity, 0 disagree.

## Slice 35 (2026-09-07): structural render of aggregates

`"${x}"` on a sum, record, tuple, or list renders structurally, in the interpreter's
format: a payload-free variant is its bare name, a variant or record is `Name(a, b)`, a
tuple `(a, b)`, a list `[a, b]`, nested aggregates recurse, and a `String` inside prints
bare. A sum dispatches on its tag into per-variant arms that join on owned string lanes;
a list folds its rendered elements through a counted loop whose separator (`, ` or the
empty string) is itself a string-lane join. The static-payload pre-pass registers exactly
the punctuation and names a rendered type reads (`collect_render_payloads`, mirroring the
lowering) so the checker's unreferenced-segment rule stays exact. Newly agree: `equality`
(an `Option(Int)` render). Census: 118/163 check, 62 runnable three-legged parity, 0
disagree. `Float` render remains refused: the prelude's `witchy.float_to_str` import
writes a guest buffer, and the v1 host-call rule admits read-only imports only, so
admitting it is a memory-mode boundary decision, not a lowering slice.

## Slice 36 (2026-09-07): `Float` render through the authenticated `float_to_str` import

The prelude's `witchy.float_to_str` import is projected into the canonical ABI catalog beside
`print`: a `Float` value and a write buffer of at least 32 bytes, returning the byte length.
The checker admits a write buffer only on a unique, non-static raw-byte object and bounds
the slice by the object's capacity (a read buffer stays bounded by the logical length); the
guest then publishes the written length under its own capacity guard. The encoder passes the
address alone for a write buffer (the plan mirror agrees), and the oracle interpreter runs
the shared `render_float` rule into the buffer's cells. A module declares only the imports
its functions reach. Newly agree: `floats`. Census: 119/163 check, 63 runnable three-legged
parity, 0 disagree.

## Slice 37 (2026-09-07): dictionaries as tuples of two lists; `unique` on `var` parameters

`Dict(k, v)` is its canonical representation everywhere the lowering and the layout pass
see a type: a tuple of the key list and the value list, in insertion order
(`canonical_aggregates::dictionary_as_tuple`), so every dictionary rule is a tuple-and-list
rule and the legacy dense body never reaches the canonical bundle. The intrinsics lower
through the list kernels: `insert` searches the keys (an `Int` compare or a string
equality) and either overwrites the value slot of a fresh copy or grows both fresh copies,
each arm finishing its own dictionary so the join carries the finished tuple and never
the two lists (whose prefix facts do not survive a join); `get_or`/`at`/`contains_key`/
`length`/`keys`/`values`/`pairs` read through the same lists. Keys are `Int` or `String`,
values a scalar or a `String` for reads and a scalar for writes; `remove`, `update`, and
`get_or_insert` are refused by name. `unique` on a `var` parameter reads as the write-back
convention the canonical ABI already carries (the callee holds the value's token and
hands it back), so the std wrappers lower; `unique` elsewhere stays a deferred qualifier.
A `var` rebound by a write-back call plans as a pushed (unique) lane. The write-buffer
host-call rule gained four checker tests (admitted within capacity; refused beyond
capacity, on a shared object, and under a read-only contract). Newly agree: `wordcount`,
`inventory`. Census: 121/163 check, 65 runnable three-legged parity, 0 disagree. `maze`
now trips a pre-existing gap independent of dictionaries: a `for` whose body holds a
`while` reading outer references (the inner loop threads every outer name as a lane and
renames its token; the enclosing `for` threads only rebound names, so the token does not
flow back to its header) — reproduced without any dictionary.

## Slice 38 (2026-09-07): loops thread only the names they rebind

`while`, `while let`, and range `for` carried every outer binding as a header lane, renaming
each reference's token into the lane; a `for` (which threads only rebound names) around
such a loop then lost the token on its own back edge (`OwnedReleasePlacement:
IncompatibleOwnedEdge`). All loops now thread only the names their bodies rebind (assign,
push, or pass to a write-back parameter), as `for` did; every other outer binding keeps its
value and its token across the loop. A dictionary-typed signature matches the candidate
ABI by its tuple shape. Newly agree: `maze`. Census: 122/163 check, 66 runnable
three-legged parity, 0 disagree.

## Slice 39 (2026-09-07): closures as records of code and `Int` captures

Every `fn` value is a closure record: a reference-counted heap record whose slot 0 is a
`Callable` lane (a new 4-byte scalar kind holding a function table index) and whose
remaining slots hold `Int` captures. Every `fn` type used as a value gets one closure layout
(distinct slot counts keep distinct signatures apart) and one closure signature: the
declaration signature with the record prepended as a shared parameter. A lambda allocates
a record with its captures' current values and its synthesized function reads them back
from its leading parameter; a named function used as a value allocates a record whose code
is an adapter (one forwarding call, then the record's drop), bound by a new
`CandidateAdapterBinding` the authority validates structurally; a call through a `fn` value
loads the code slot, clones the record, and passes the clone first (`VarCall`, or
`VarTailCall` in tail position). The checker types a Callable slot only as the signature the
module registered for its layout (`ClosureSlot` rule, `closure_layouts` in the digest);
the candidate ABI matches a `fn`-typed parameter or result against its closure record; the
closure witness admits `Int` captures that fit the record. Non-`Int` captures refuse by name.
Newly agree: `higher_order`, `higher_order_sum`, `closures`. Census: 125/163 check, 69
runnable three-legged parity, 0 disagree. `plugin_host` stays refused: a list of `fn` values
and a lambda capturing a capability are both outside v1 by policy.

## Slice 40 (2026-09-07): independent review of the foundation, and its fixes

Three bounded adversarial reviews (closures; pointer-bearing copies, writable host buffers,
and `unique` on `var`; loop lanes and release placement) ran against the branch with
probe programs on all three legs. No interpreter/Wasm disagreement was produced. One
checker fail-open was found and closed: a fresh closure record's code slot could be loaded
(and dispatched through) before it was stored, because the checker tracked initialization
only for reference slots; the `ClosureSlot` rule now records a `Store` of a callable lane
per container (renamed across edges, intersected at joins) and refuses a `Load`, a call
argument, a return, or a drop of a fresh record whose slot was never stored
(`closure_slot_load_before_store_is_refused`). Also closed: range facts retained by token
instead of allocation identity after `MoveRange`/`CloneRange`/prefix consolidation (an alias
could keep a stale initialized-prefix fact); an adapter that never released its environment
was admitted (a leak per call; the release is now mandatory); the oracle's range copy now
refuses to overwrite a stored reference cell like the checker; the `string.from_code` host
gained the bound check its float sibling had. Refusals of valid programs closed: a `var`
list or string written back inside a loop (the replacement now carries its extent lanes), a
bound closure called in statement position, `fn(...) -> Nil` bodies and `let _ = f(n)`, a
`var` parameter that is also returned (the result lane is an owned copy), and a `let` alias
live across a `var` record or dictionary call (the callee receives its own copy, as lists
already did). Reviewed and left as recorded limits: `closure_slot_assignment` keeps layouts
apart by padding rather than by a structural signature identity; the zero-width branch of
`meet_range_states` adopts the other edge's prefix fact (sound while the fact's length is
the block's own count lane); `Callable` lanes share one Wasm function type per shape, so
signature identity rests on the checker, not on `call_indirect`. Census unchanged at
125/163 check, 69 runnable three-legged parity, 0 disagree; wir lib 567, lower canonical
91, launch 57.

## Slice 41 (2026-09-07): `Bytes`

`Bytes` is admitted as the raw-byte buffer layout under its own source type: every
extent, ownership, copy, and release rule for strings applies unchanged (the authority
matches a `Bytes` parameter or result against the runtime buffer layout the way it matches
`String`; aggregate fields, list elements, and empty slots take the same layout). The
`__bytes_*` intrinsics behind `std/bytes` (and the public `bytes.at` alias, which the scope
resolves to the intrinsic rather than the wrapper) are byte kernels over that buffer with
the interpreter's rules: `at`/`set_at`/`swap` abort on an out-of-range index, a byte value
outside `0..=255` and a negative count abort, `slice` clamps both ends and yields empty for
a start past the end, `push`/`append`/`concat`/`reserve`/`from_string`/`from_list` copy into
a fresh buffer, `with_capacity` allocates an empty buffer of that capacity, and
`to_string` is the lossy UTF-8 decode of `String::from_utf8_lossy` (a lead-byte class
table with the exact continuation ranges; each maximal invalid prefix, the lead plus the
continuation bytes found valid before the first bad or missing one, becomes one U+FFFD,
whose payload is registered whenever the program decodes). A render shows `Bytes(len=N)`.
Two facts that bit: the checker bounds a raw-byte element write by the buffer's
*capacity*, so a growing kernel guards its stores against the capacity lane it allocated
rather than the count it publishes (`push_byte_store`); and the 4-byte lead `F4` admits
only continuation `80..8F` (the first table let `F4 90 80 80` through as valid; the
interpreter, and Rust, replace all four bytes).

Also fixed on the way: `canonical_scope` cached its result under a fingerprint of function
names and statement counts, so two programs of the same shape compiled on one thread (the
launch tests' sequential fixtures) shared a stale scope, and a std function reached only by
the second (`bytes.length`) was reported as a missing binding. The cache is gone; each
lowering pass takes the scope once (`lower_typed_module`, `project_layouts`, the payload
and render scans, `canonical_tail_sites`).

A bounded adversarial review of the slice (memory safety of the emitted IR, 240 randomized
UTF-8 sequences plus the hand-picked edges, twelve abort fixtures, leak receipts through the
oracle) found one divergence, fixed: a `with_capacity`/`reserve` hint above `u32::MAX - 4`
trapped compiled where the interpreter clamps; the hint is now sign-checked and clamped
(`BYTES_CAPACITY_HINT_LIMIT`), since capacity is unobservable. Recorded limit: the witness
matches `String` and `Bytes` against the same runtime buffer layout, so a signature that
swaps the two spellings is invisible to admission (bit-identical representation, no
memory-safety consequence, a weaker typed witness). Pre-existing, not Bytes-specific:
mutating an aliased copy inside a `while` is refused with `InvalidExtent` for strings too.

Newly agree: `brainfuck`. Census: 126/163 check, 70 runnable three-legged parity, 0
disagree; launch 60 (kernels, a `from_utf8_lossy`-adjudicated decode battery, and the
abort fixtures), lower canonical 91, wir lib 567. `time_and_encoding` remains refused on
the `encoding.*` intrinsics (hex/base64 kernels), the next byte-level slice.

## Slice 42 (2026-09-08): `Dir` and `Env` capability host calls

The first authority-bearing operations on the canonical route, built against the frozen
contract in `rfcs/0165-notes/dir-env-host-contract.md` by three parallel tracks (the
`witchy-wir` catalog/backend/oracle, the `witchy-runtime` host functions and launch
minting, and the lowering). A capability is an opaque `Capability(kind)` token (0 Console,
1 Dir with any rights arguments, 2 Env) minted only by the launcher, by grant ordinal, from
the entry signature's kinds (`CanonicalEntryCapability::{Dir, Env}`); rights and subtree
scope stay in the host's authority object and a violation is a host trap. Six stateless,
bounded imports replace the frozen route's staged `pending` reads: `dir_read_into`,
`dir_exists_at`, `dir_write_at`, `dir_subtree_at`, `env_has_var`, `env_get_into`; paths and
data cross as checked read slices, a read lands in a `Write` slice (a new backend buffer
form, `(ptr, capacity)`, keyed on the place's access) whose extent the checker bounds by
the buffer's capacity. The guest asks the host with an empty buffer for the full length,
allocates exactly that, asks again, and publishes the second answer under the buffer's own
capacity guard (a source that grew between the calls aborts rather than truncating). The
catalog projection (`with_capability_imports`) requires each import's authenticated
authority family (`DirRead`/`DirWrite`/`DirGrant`/`Env`) to match, the mirror of the rule
that refuses an authority-bearing `print`. `get_env` selects the `Option` tag from
`env_has_var` and reads only when present. `dir as Dir[Read]` is the same token; the
canonical ABI encodes a capability's rights by name (they were "unresolved nominal types"
before, and a rights-qualified parameter was a callable-ABI obligation; it now binds).

The oracle serves the six imports from an `OracleFixture` (files, environment) with a
root-directory and process-environment fallback used by `witchy parity`'s third leg and the
oracle example test, so a `Dir`/`Env` program runs on all three legs against the same
working directory (dev grants); the launch tests seed a temporary directory and assert the
expected contents independently. Not modeled by the oracle: rights (a violation stays a
host-side trap on the runtime leg), `dir.list` (its host-laid-out list stays refused), and
the shared `witchy-test-host` fixture adapter (the fixture is a map with a filesystem
fallback, not the test-host protocol; recorded as the follow-up). RFC-0163's ghost-authority
validation is not claimed: this slice is the object-level boundary.

Newly agree: `files` (the census now runs parity for `Dir`/`Env` mains too). Census: 127/163
check, 71 runnable three-legged parity, 0 disagree; launch 68 (four capability fixtures:
read/exists/write/subtree through a scratch directory, a 100 KB read past the probe
buffer, an escaping path and a read-only write trapping, `get_env` present/absent and the
allow-list trap, entry refusal without a grant), wir lib 580, lower canonical 91, runtime
82, types 419. `minigrep` now stops at its `args: List(String)` entry parameter (the launch
protocol mints capabilities only); `caps_audit`/`caps_guard`/`coven_check` at `json`,
`capability_rights` at `Net`, `branded_caps` at a nominal wrapper `Option`.

