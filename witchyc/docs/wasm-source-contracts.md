# Fresh contracts tied to source function bodies

The first source-backed contract slice accepts nongeneric private pure U64
functions: U64 parameters/result, immutable U64 lets, checked addition, calls to
other contracted functions, and one final return. A contract needs at least one
ensures clause and exactly one `proof: auto` request. Requires are optional;
combined requires/ensures and parameters each have a limit of 32.

```text
fn bump(x: U64) -> U64:
    requires x < 18446744073709551615
    ensures result == x + 1
    proof:
        auto
    return x + 1

fn main(console: Console<Write>):
    console.print_int(bump(9))
```

This slice passed independent source review and focused real-kernel qualification
at c42e884. An auto directive requests checking; it does not grant acceptance.

## One clause scope and one actual body

The semantic checker resolves clauses once beside that exact FunctionId/logical
body. Requires can name only parameter LogicalIds. Ensures can also name a
distinct ghost result role, later bound to the actual return expression. Runtime
let names are in neither clause scope. A contract parameter named result refuses;
a runtime let named result cannot capture the ghost return role.

Clauses use mathematical Nat addition, U64 literals/parameter bounds and supported
Bool propositions/comparisons/connectives. Clause intermediates are mathematical,
not runtime additions. Subtraction/multiplication/division, match postconditions
and other unsupported contract forms refuse. Source function contracts on
generics, main, non-U64 values, mutable/control/effectful contexts or recursive
contract graphs also refuse. Ordinary programs without clauses/direct contract
calls retain their existing checking and zero-new-goal behavior.

VC generation reads the primary typed body, not source AST or a reconstructed
function definition. Immutable lets and runtime arithmetic use named SSA bindings.
Each runtime addition in a contracted body contributes an independent range
condition captured at its actual point, before any later call facts exist. The
final component proves return range and ensures using the actual returned value.
Changing x+1 to x+2 changes that proposition even if the authored ensures stays.

## Every typed call is a separate obligation

Each actual typed call to a contracted function produces its own precondition
goal from the actual argument values and caller bounds/requires/preceding lets.
Sites are numbered per caller in deterministic argument-evaluation order. A
deduplicated call-graph edge is never used as a substitute for these occurrences.
Calls with different arguments, nested calls and unused selected callers remain
represented. Empty requires still produce a True precondition obligation.

After that site's precondition, a fresh symbolic U64 result may use the callee's
separately checked range/postconditions. The callee body is not inlined or
rechecked at a call. Acyclic contracts plus a complete single proof batch support
this modular interpretation. Earlier obligations cannot use later call facts.

Caller contexts initially support straight-line U64 parameters/immutable lets,
pure U64 expressions/evaluations/returns and normal main Console entry/print
bookkeeping. Calls inside unsupported control, effects, data flow or generic
contexts refuse explicitly. An unrelated borrow/MUST error anywhere still fails
the whole semantic program before proof execution.

Symbolic contexts use LogicalIds and shared let bindings. Postconditions remain
lazy until their fresh result is needed; relevance follows dependencies rather
than copying every prior call's facts. Consequently repeated constant calls have
separate, bounded preconditions without growing irrelevant contexts. The new
slice enforces 4096-node and 64KiB total expected-statement budgets per caller,
with append-before-allocation; no global ordinary-program token/call limit is
introduced.

## Existing kernel batch and trust boundary

Generated source goals and every selected authored ghost lemma share one existing
Lean invocation, expected-definition/theorem equality audit, actual kernel checks,
axiom allowlist, per-theorem dependency inventory and fresh success marker. The
trusted operator installation/current-tool/source/compiler/model/module hashes,
deadlines and atomic publication remain unchanged. Zero complete goals do not
resolve or hash an operator proof installation.

The model identity keeps ghost.v1 mathematics unchanged and adds explicit
checked-source-U64.v1 semantics when source goals exist. Ghost saturating Nat
subtraction is not treated as runtime subtraction. Translation, VC completeness,
layouts, allocator and emission remain identified trusted compiler components;
the kernel proves the generated expected propositions.

Source obligations add goal_kind=function_body or call_precondition and function
to the existing obligations inventory. Call goals also include callee and site;
their declaration is the caller name. Unique batch theorem indices, expected
statements/hashes, checked nodes, fresh outcome and audited dependencies remain.
No second certificate family, native evaluator, allowlist or reusable admission
is created; proof_admission and admission_reuse remain false.

Contracts/directives emit no runtime instructions and retain arithmetic guards.
Focused controls confirm byte erasure for the same runtime body. Claims concern
normal-return partial correctness, not stack/host/interruption freedom or whole
program totality. Generic/stateful Vec/sequence/owner/drop/frame contracts, unsafe
VCs, String and persistent admission remain open; this slice does not complete
those goals or establish a performance result.
