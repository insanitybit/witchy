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

