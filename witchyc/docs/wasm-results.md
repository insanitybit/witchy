# Concrete copyable Result values

> **Historical.** This page records a milestone of 2026-11 and its limits then; most have
> since been lifted. For what holds now see docs/wasm-references.md, docs/wasm-sums.md and
> docs/wasm-unions.md, docs/wasm-generic-functions.md and docs/wasm-generic-records.md,
> docs/wasm-memory-sizes.md, docs/wasm-hashmap.md and library/wasm-vec.wy.

The direct compiler supports concrete `Result<T,E>` in locals and private
by-value function parameters/results. Each arm can contain an existing copyable
scalar, Bool, concrete record/fixed array, or Unit. This is a failure-return
foundation; it does not implement allocation, owning payloads or generic Vec.

```text
fn choose(ok: Bool) -> Result<U8, U64>:
    if ok:
        return Ok(7)
    else:
        return Err(300)

fn dispatch(value: Result<U8, U64>) -> U64:
    match value:
        Ok(payload):
            return U8.to_u64(payload)
        Err(payload):
            return payload

fn main(console: Console<Write>):
    console.print_int(dispatch(choose(true)))
    console.print_int(dispatch(Err(500)))
```

`Ok` and `Err` each require one payload. An explicit annotation, function return
or parameter context must determine both arm types. Unconstrained `let x = Ok(1)`
refuses. Context propagates into concrete record/array constructors and literals,
with existing nominal identity and width checks. `Result`, `Ok` and `Err` are
reserved selected record/function/lemma declaration names. Locals, arm binders
and Console parameters can shadow constructor names; a shadowed name is not
silently reinterpreted as a constructor.

## Exhaustive matching and active bindings

A match requires exactly one `Ok` and one `Err` arm, in either source order.
Duplicate, unknown, missing and wildcard patterns refuse. Nonunit payloads require
one immutable arm-scoped binder. A Unit arm can omit its binder (`Ok:`) or bind
valid active Unit (`Ok(unit):`). That binder has no carrier, address or storage;
reading it produces Unit, which can be passed into `Ok(unit)`/`Err(unit)` when
that constructor context expects Unit. Ordinary Unit locals/parameters retain
existing restrictions. The binder is never the tag or an adjacent arm carrier.

Every arm is checked, including a statically unchosen arm, and unused selected
functions still undergo checking. Arm scopes restore to the scope before the
binder, so a binder never leaks into another arm or after the match. Existing
no-shadow rules apply within an arm; names can be reused between distinct arms.
A nonunit function needs all reachable alternatives to return, or a later return
covering fallthrough. The scrutinee is evaluated and captured exactly once.
`?`, guards, generic functions and runtime contracts remain unsupported.

Only the active payload becomes a source value. An active record/array binder
can be copied or shared-borrowed as an ordinary initialized value. The binder is
immutable; mutation requires a separate mutable copy. Existing canonical-root
loan rules, concrete layouts, frame guards and padding-preserving initialized
cell copies continue to apply to those active ordinary values.

## Private representation, not a record

One sum descriptor is interned inside the existing layout registry. The logical
value is exactly `Ok(T)` or `Err(E)`. It is not a source record with tag/Ok/Err
fields. The private register ABI is `[i32 tag, flat(T), flat(E)]`, tags 0/1.
The tag has an internal carrier type distinct from source Bool. Unit contributes
zero payload carriers: `Result<(),U64>` is `[tag,Err]`, `Result<U64,()>` is
`[tag,Ok]`, and `Result<(),()>` is `[tag]`.

Constructor IR contains only arm identity and the typed active payload. Inactive
machine zeroes appear only during physical emission. They are not typed Expr
values, initialized source fields, owners, proof facts or cleanup inputs. Local
copies/private calls may carry this sealed physical representation; only match
can obtain its selected typed payload. Dispatch tests tag 0 and tag 1 explicitly;
any other tag traps before binding a payload. It never defaults to Err. There is
no tag projection, inactive payload access, sum equality or implicit Bool/integer
conversion.

The tag and both payload ranges count toward the 32-carrier ABI/shape limit.
Aggregate and sum descriptors together are limited to 128. Existing type-depth,
source-size, operation, local and temporary bounds remain active.

## Explicit nonstorage boundary

Result values have no memory layout in this slice. Checked storage lookup and
initialized-cell traversal reject sums and private tags. Nested Results, Results
in record/array fields or generic record arguments (including unused phantom
arguments), borrowed/addressed Results, reference or owning payloads refuse.
There is no fake zero-byte Result layout and no unconditional traversal of both
alternatives' payloads. A storable active record/array copy is distinct from its
sealed containing sum.

Owning payloads require later affine/move/drop flow and variant-aware storage,
cleanup and failure-preservation rules. This copyable slice establishes none of
those rules for Buffer/Allocator/Vec/String. It introduces no unsafe operation,
new proof axiom/certificate, admission reuse, protected-store action or native
execution fallback. All runtime/type checks finish before fresh scalar lemma
checking; a valid ghost lemma cannot excuse a malformed match.

## Receipt and runtime compatibility

Receipts without sums omit `sum_types`. With sums, that additive array records
`id`, `kind: "Result"`, `representation: "private-registers"`, `storage: false`,
`tag_carrier: "i32"`, `tag_slot: 0`, total `carrier_count`, and a `variants` array.
Each variant records name, logical tag, concrete type, `carrier_start` and
`carrier_count`. Existing aggregate metadata binds concrete payload layouts.
The historical check.v1 schema name and proof/admission flags remain unchanged.

No new host opcode, memory map, import, grant, artifact schema or exported entry
is needed. The scalar/frame host validates actual bytes; exact trusted-compiler
source/module/receipt rechecking still applies. Production exports only the
no-argument/no-result `run` entry. A private matcher export used to inject invalid
tags is explicitly unadmitted test instrumentation and must be refused by the
production host. It must derive the compiler function index from actual imports
and selected function order, not replace bytes or recreate dispatch logic.

Compiler source controls cover construction/context, Unit/Bool and record/array
payloads, sealed representation, all-arm checking, scopes, limits and reserved
names. Actual alternative outputs, invalid-tag execution, once-only side effects,
active-frame behavior and artifact/publication preservation require the
separately coordinated focused runtime checks. Writing these controls alone is
not execution evidence or completion of the Witchy ownership/proof milestones.

## Early exit: `?` and refutable `let`

```witchy
fn word(allocator: &Allocator, input: String, first: U64, second: U64) -> Result<String, String>:
    let one = String.push_scalar(allocator, move input, first)?
    let two = String.push_scalar(allocator, move one, second)?
    return Ok(move two)

fn quarter_or(value: U64, otherwise: U64) -> U64:
    let Ok(part) = quarter(value) else Err(code):
        return otherwise + code
    return part
```

`let name = value?` binds the `Ok` value, or returns `Err` with the same error; an
anonymous error set widens into the function's (`docs/wasm-unions.md`). `value?`
alone is for a `Result<(), E>`. `let Ok(name) = value else Err(e):` runs the block for the
other arm; that block must leave the function.

An error of another type is converted when the function's error type says how: if the
function returns `Result<T, F>`, the `?` meets an `E`, and `F` has a static
`pub fn from(error: E) -> F`, the error leaves as `Err(F.from(error))`
(`tests/fixtures/try/try-from.wy`). `FsError` has one for an allocator's code, so file code
writes `using var text: String = String.new(allocator)?:` beside `dir.read_text(...)?`
(`tests/fixtures/fs/dir-walk.wy`). Without a `from`, the refusal names both error types and the
`from` to add.

`?` also works where a value is wanted: `var word = String.new(allocator)?`,
`current = half(current)?`, `add(half(a)?, half(b)?)`, `return Ok(half(v)?)`,
`match half(v)?:`, `if half(v)? > 2:` (`tests/fixtures/try/inside.wy`). The `?` that runs
first is taken out of the statement and its value given a hidden name; the statement,
reading that name, starts the success arm. A `?` is never moved ahead of what is written
before it: in `add(add(v, 1), half(v)?)` the inner `add` is first bound to a hidden name,
then the `?` runs, then the outer call (`tests/fixtures/try/after-call.wy`); arithmetic
before it is bound the same way, so a trap there still comes first
(`conditional/try-after-arithmetic.traps.wy`).

Both are a `match` whose success arm holds the rest of the block
(`src/compiler/early_exit.rs`). Ownership checking, code generation and proofs see only that
match, so nothing else changed: a contracted function can use `?` on a proved callee
(`push_pair` in `tests/fixtures/try/chain.wy`).

Limits:

| Limit | Refusal |
|---|---|
| The function must return a `Result` whose error type is the `?`'s, or has a `from` for it | "the function must return a Result", "the same error type" |
| Inside a larger value, nothing but names, fields, borrows and moves runs before the `?` | "runs after an earlier call in the same statement" |
| No `?` on the right of `&&` or `\|\|`, which may not run | "may not run: use `if`" |
| No `?` in a binding with a proof | "no place in a binding with a proof" |
| An owner still held when the error leaves is refused by the ordinary ownership check | the usual unconsumed-owner message |

The last limit is why the library returns the owner inside the error
(`Result<String, String>`): then `?` hands it back and nothing is left behind.

### `while condition?:`

The condition is asked again before each pass, so its `?` cannot be moved ahead of
the loop as it is for other statements. The loop becomes `while true:` whose body
starts with `if !(condition?): break`; that `if` is then an ordinary statement
holding a `?`. `continue` still reaches the condition, which is the first thing in
the body. Invariants stay on the loop; the `break` shows them like any other
(`docs/wasm-scalar-proofs.md`). `tests/fixtures/try/while.wy` runs in Wasmtime.

## Refutable `let` with a written type

```
let Ok(items): Vec<U64> = Vec.new(&allocator, 4) else Err(code):
    console.print_int(code)
    return
```

The type after the pattern is what `items` is. It reaches a generic call in the
value the way `let items: Vec<U64> = match ...` does, so the call needs no other
annotation; the rest of the block follows with `items` bound. The failure arm
of every refutable `let` must leave (`return`, `break`, `continue`,
`unreachable`, or an `if`/`match` all of whose paths do). Before this, a failure
arm that fell through silently skipped the rest of the block when the function
returned nothing. Fixtures: `tests/fixtures/guard/`.
