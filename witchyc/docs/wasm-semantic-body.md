# One logical body before concrete lowering

Every accepted nongeneric function now follows the same pipeline: source syntax,
one semantic body check, one concrete lowering, existing proof checks when needed,
then mechanical Wasm emission. Inspect also runs concrete lowering before proof
resolution. There is no direct source-to-physical checker or fallback route.
Generic functions, source function contracts/invariants and unchecked operations
remain refused. This source cutover awaits independent review and focused checks.

## Semantic identity and ownership

The primary logical body uses semantic types and logical binding IDs. A source
binding is one local regardless of physical carrier count; even a named Unit match
binder receives its own logical ID. LogicalLocal retains type, mutability and a
source position where the AST provides it. Parameters and match binders without
individual AST spans use an absent position.

Semantic types identify scalars, capabilities, Buffer elements, nominal record
instances, fixed arrays, Result alternatives and call-only references. Their
registry IDs address semantic definitions, never physical layout descriptors.
Record template AST fields are resolved once into semantic template nodes. The
template store retains no competing AST field-definition map. Template parameter
nodes have no physical layout or stand-in type; generic function parameters are
not enabled in this stage.

All selected/unused record templates still undergo symbolic role/complexity
validation under existing Copy-storage parameter constraints. Semantic resolution
itself enforces depth/visit limits before descending. Copy/discard/one-owner
properties come from semantic fields. Buffer/record canonical names derive only
from these resolved semantic types, preserving nominal identity and substitution.

Logical places retain canonical root identity, root/result types and permission.
Field paths contain ordinals; index paths contain a checked typed expression and
array length. They contain no byte offsets, field slots, element stride or flat
width. InstanceCall names a FunctionId, arguments and result; stage1 substitutions
are empty. Calls retain source function order and are lowered to the same source
prefix as before.

The existing Flow and loan ledger run only during semantic checking. They retain
whole moves, whole-wrapper extraction, active Result ownership, scope expiry,
branch/loop availability, outer call loans, guard-conditional payload joins and
parent availability before and after a Copy assignment RHS. No ownership replay
occurs while assigning registers.

## The sole concrete boundary

Concrete lowering consumes the typed body and semantic type store without AST
reparsing, synthetic AST construction or source retyping. It maps logical locals
to carrier ranges; field ordinals to canonical descriptor slots/byte offsets;
array paths to actual stride/carriers; addressed logical roots to existing Frame
cells; and function IDs to emitted source indices. Named Unit binders may occupy
zero physical carriers while keeping distinct logical identity.

Existing closed physical IR remains solely the emitter input. Existing Layouts,
Cell traversal, Frame planner, scalar encoders and allocator helpers remain the
physical storage/code mechanism. Concrete descriptor metadata carries properties
supplied by semantic typing, rather than redoing source ownership classification.
The physical layout builder no longer owns AST definitions/type resolution or a
second canonical source-name formatter.

Parameter/result/local/descriptor/frame target limits are validated after semantic
checking and before proof, emission or publication. Storage operations record
element-width obligations in their logical function. The lowerer checks <=30
element carriers before constructing physical Result shapes, retaining the
operation-specific width refusal for valid read/take payloads. A source containing
both a wide type and an invalid payload may fail semantic type/name checking
first. This is static diagnostic order: runtime identity/index/state guards still
finish before conditional payload evaluation and all typed element accesses.

The existing generated-temporary limit (4096 emitter locals) and actual module
byte limit are checked during emission, after proof checking and before
publication. They are separate from the concrete source-local limit of 256.

## Observable diagnostic inventory

Compile reports add the diagnostic-only `semantic_body` object with contract
`witchy.logical-body.v1`, selected_definitions, checked_bodies,
concrete_instances and ordered body_checks entries containing name/checks/instances.
The body counter increments at the actual single semantic body entry. All stage1
functions, including main and unused nongeneric helpers, lower once; ghost lemmas
are excluded. This inventory is neither a proof certificate nor reusable admission.

Current schema fields, selected function order, source proof/refusal behavior,
typed-buffer v2 profile, map, six-helper indices, operator grants and fresh
source/compiler/module/host/proof/artifact/publication checks remain in force.
Concrete output changes do not authorize additional host capabilities.

## Remaining generic-function work

The next stage still needs real function-scoped TypeParams, unique argument and
expected-result inference, propagated trait/target constraints, same-substitution
recursive instances and rejection of expanding polymorphic recursion. Those must
extend this body, rather than invoke another checker or substitute U8/zero width
as a universal type. Generic source Vec algorithms remain ordinary library code.
No new Lean correctness infrastructure, unsafe VC bypass, admission reuse,
measured performance result or overall Vec completion is established here.
