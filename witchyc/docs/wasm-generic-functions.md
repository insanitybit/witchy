# Generic functions over Copy storage

Generic functions extend the existing logical-body path. Each selected source
body is checked once, including unused generic definitions and unchosen branches.
Concrete clients instantiate that checked body without reparsing, AST retyping,
ownership replay or Lean invocation per instance. Focused Rust/Node checks and
independent review qualify this local slice, including a real scalar-proof
artifact control. That control does not prove generic function or Vec contracts.

```text
fn id<T>(value: T) -> T:
    return value

fn main():
    let byte: U8 = id(1)
    let word: U64 = 2
    let same_word = id(word)
```

Type parameters are scoped by FunctionId and parameter index. The fixed initial
family is Copy, non-unit/nonzero storage with alignment at most eight. A parameter
has these semantic constraints, never a guessed U8/zero-width layout. Under `T: Any`
owners are type arguments too, and `()` is one; a borrowed reference or a view is not. Record template parameter
scope remains separate. Duplicate/conflicting parameter names and generic main
refuse. Existing source function contracts, invariants and unsafe features remain
refused; generic ownership checking does not prove Vec functional contracts.

## Inference without argument effects

Calls gather unique substitutions from expected result and all argument semantic
types before actual argument checking. An earlier literal can receive its type
from a later typed argument. Nested calls can use the surrounding expected type.
An uncontextual literal-only `id(1)` refuses ambiguity rather than guessing a
width. Phantom result parameters that no context/argument constrains also refuse.
Conflicting typed arguments refuse as a type mismatch.

Inference changes no Flow/loan state and performs no source evaluation. Actual
arguments are checked once in source order under the solved types and existing
call loans. Generic record constructors retain their requirement for matching
expected record context. No numeric/equality operation or field access on an
arbitrary T is justified by the Copy-storage family; carrying/copying T is valid.

## Constraints and recursive instances

Bodies retain semantic element-width obligations and call edges with substitutions
in caller types. After all selected bodies are checked, requirements propagate
through those edges without walking source AST or rechecking ownership. Requirement
states are deduplicated per definition; at most 4096 states per definition, not a
global call/token ceiling. Repeated empty-edge visits are not new constraints.

Recursive SCCs must preserve identical parameter substitutions on each edge.
Same-substitution cleanup and mutual recursion are supported; expanding
polymorphic recursion refuses even in unused generic definitions. Function scopes
are compared by real parameter identities, not parameter spelling or index alone.

One deterministic instance inventory keys FunctionId plus actual concrete types.
Nongeneric definitions occupy the initial prefix in source order. Generic instances
are discovered by ordered typed-body traversal, with argument calls first and a
registered/active instance before traversing its callees. Repeated keys reuse the
same ordinal; recursion never starts another body check.

Only actual substituted types enter the concrete lowerer. Unbound parameters
refuse physical lowering. Concrete types map independently across U8, U64, Bool,
arrays and nominal padded records; no global parameter-to-first-client cache is
used. Generic record identity retains definition plus arguments, not printed
names. Canonical names are deferred and appended under depth16/4096-visit and
64KiB name bounds before allocation; symbolic parameters have no concrete name.

The source-instance limit derives from the existing host maximum of 256 total
functions: actual imports plus emitted instances plus six arithmetic helpers plus
six heap helpers when needed. It is not a new arbitrary call window. Target
parameter/result/local/frame/descriptor limits and propagated element constraints
are checked per actual instance before proof/emission where previously required.
Generated temporary/module-size checks retain their existing later stage.

## Ordinary source Vec algorithms

Generic source functions can now relay, extract/rebuild one-owner records, borrow
Buffer<T>, push into spare capacity, take the last Copy element and attempt release.
Failed push can return an ordinary one-owner record containing Vec<T> and its Copy
item. Failed release reconstructs the wrapper with the exact returned Buffer.
Cleanup can recurse with the same T. These are library bodies using existing
storage primitives; the compiler recognizes no Vec name or algorithm.

The storage operation limit is 30 element carriers. Complete Result wrappers have
their own 32-carrier limit: `Result<Vec<T>, Refused<T>>` with Refused containing
Vec+item requires `7 + carriers(T)`, so that particular API fits at most 25.
Generic functions do not waive any concrete ABI bound. Full growth/reserve,
functional contracts, owner elements, ZST/drop, String and unsafe VCs remain future.

## Diagnostic inventory and binding

All programs use `witchy.logical-body.v2`. semantic_body counts N selected
definitions/body checks and M concrete instances, with ordered per-definition
checks/instance counts. selected_functions and coverage keep every semantic
definition once, including unused generics with zero instances.

function_instances lists definition, canonical concrete type_arguments and actual
ordinal for each emitted instance. Calls/helper indices and that receipt use the
same inventory. Existing typed-buffer v2 profile/map/six helpers, operator grants,
fresh source/compiler/module/host/proof/artifact/publication binding remain in
force. Counters are diagnostics, not generic proof or reusable admission. No
performance or overall Vec completion is claimed by this feature.

## Type arguments from a later use

`var numbers = Vec.new(allocator, 2)?` names no element type. When a `let` or `var` of a
`Type.constructor(...)` call (with or without `?`) leaves type arguments open, the first
later call of one of `Type`'s methods on it settles them: `numbers.push_mut(allocator, 0)`
says the elements are `U64` (`inference/later-use.wy`). That call's arguments are typed
as they would be at the binding: literals, and variables already in scope. A loop counter
or a variable bound later says nothing, and an annotation is still needed
(`inference/no-use.refused.wy` is the case with no use at all). Implemented in
`src/compiler/check/statements.rs`, `later_use`, feeding the same hint a `match` arm's typed `let` gives.
