# The language at a glance

A tour of Witchy syntax and guarantees, followed by a [feature index](#feature-index).
The examples point to runnable fixtures and more detailed language guides.
[tour.wy](../tests/fixtures/programs/tour.wy) exercises the core constructs together.
For the libraries, see [collections](library-collections.md) and [text](library-text.md).

The fixture references use the Wasmtime host. Consult the [native target guide](native-backend.md)
for ARM64 coverage, runtime grants, and sandboxing.

## Source basics

Witchy files use the `.wy` extension. A colon opens a block, with four spaces
for each indentation level. `//` begins a line comment. An expression can
span lines inside parentheses, brackets, or braces. Bind an immutable name
with `let` and a reassignable one with `var`. A unique reference can also
permit writes to its referent.

`with`, `async`, and `await` are reserved for future syntax.

## Programs and capabilities

```
import console

fn main(console: Console<Write>, allocator: Allocator):
    console.print("hello")
```

The launcher supplies the grants declared by `main`, such as `--output` and
`--allocator`. A function gets authority through its arguments, including through
a reference, an owner containing a capability, or a closure that captures one.
The Wasmtime host also supplies capabilities such as `Dir<Read>`, `Clock`, and
`Listener`. [Capability and launch rules](wasm-capabilities.md)

A library can narrow an existing capability with `capability ConfigDir from Dir<Read>`,
which wraps it in a sealed type. A `grantable capability` declares sealed policy
data supplied at launch with `--grant=Type.field=value`. Its fields are `U64` or
`Bool`. Host authority travels through capability values.
[User-defined capabilities](wasm-capabilities.md#user-defined-capabilities) ·
[Grantable capabilities](wasm-capabilities.md#grantable-capabilities)

## Modules and packages

Write imports before declarations. `import vec` selects a standard module,
`import "helpers.wy"` loads a source file, and `import "@name"` selects a package
dependency. A declaration must be visible through the importing file's own
imports. Mark functions and types with `pub` to expose them to other modules.
[Modules and privacy](wasm-modules.md) · [Packages and policies](wasm-packages.md)

## Values

| Written | Type |
|---|---|
| `42`, `0xFF`, `1_000` | `U64` by default; `U8`, `U16`, `U32`, `I64` where the type says |
| `1.5`, `2.0e-3` | `F64` ([wasm-f64.md](wasm-f64.md)) |
| `250ms`, `30s`, `1m30s`, `2h` | `Duration` (`import time`; `d.ms()` is the count, `"${d}"` shows `1m30s`: `arithmetic/durations*.wy`) |
| `'a'` | the character's Unicode scalar value |
| `"hello"`, `"\$"`, `"\0"` | `&str`, borrowed UTF-8; a `Text` where the value is stored ([text literals](wasm-text-format.md)) |
| `"total ${n}"` | an interpolated format ([interpolation](design-format.md), [Text and formatting](wasm-text-format.md)) |
| `true`, `()` | `Bool`, unit |
| `(1, true)` | a tuple ([wasm-tuples.md](wasm-tuples.md)) |
| `[1, 2, 3]`, `[0; 8]` | fixed arrays |

A fixed array is sliced with `.slice()` and `.slice_mut()`; a `Vec` with `.as_slice()` and
`.as_mut_slice()` (the refusal for either written on the other says which). A number is
changed to another type by a call that can fail where the value may not fit: `I64.from(n)`
of a `U64` is a `Result<I64, U64>`, taken out with `?` or `match` before it is an operand
(`I64.from_bits` keeps the 64 bits and cannot fail; `U8.to_u64` and the other widenings give
the number).

Arithmetic traps on overflow; `& | ^ ~ << >>` work on unsigned numbers
([wasm-bits.md](wasm-bits.md)). On a program's own types `+ - *` call `add`, `sub`, `mul`
and `< > <= >=` call `before` ([operators](wasm-traits.md#operators); `traits/operators.wy`;
a chain of up to 41 of them in one expression, `traits/operator-chain.wy`).

## Functions

```
fn scale(n: U64, by: U64 = 10, plus: U64 = 0) -> U64:
    n * by + plus                         // the last line is the value

scale(4)                                  // 40
scale(n: 4, by: 2)                        // labels are checked
scale(4, plus: 1)                         // skip a default by label
fn least(a: impl Before, b: impl Hash) -> U64
```

([wasm-functions.md](wasm-functions.md); `functions/defaults.wy`, `labels.wy`,
`traits/impl-parameter.wy`.)

## Records, sums and tuples

```
type Point derive(Eq, Hash, Before, Discard):
    x: U64
    y: U64

type Shape:
    Circle(U64)
    Square(U64)
    Dot

let p = Point(x: 1, y: 2)
let q = Point(..p, y: 5)                  // the other fields from p
let (quotient, rest) = divide(17, 5)
var (count, total): (U64, U64) = (0, 0)
```

([wasm-sums.md](wasm-sums.md), [wasm-derive.md](wasm-derive.md),
[wasm-tuples.md](wasm-tuples.md); `derive/point.wy`, `fields/spread.wy`, `tuples/var.wy`.)

A type alias such as `type Counter = U64` gives its target a second spelling.
Anonymous records use labelled fields, as in `.{x: U64, y: U64}` and
`.{x: 1, y: 2}`. An anonymous sum such as `.[Done | Failed(U64)]` takes
values such as `.Done` and `.Failed(7)`. A compatible wider anonymous sum can
receive their values. [Anonymous records and tuples](wasm-tuples.md) ·
[Anonymous sums](wasm-unions.md)

## Brands and scoped type identity

Use a brand to pair values issued by the same opening. The graph library
uses `Node<G>` and `Distances<G>` to make that relationship part of the types.

```witchy
import graph

fn improve<G: Brand>(distance: &mut Distances<G>, node: Node<G>, cost: U64):
    if cost < distance.get(node):
        distance.set(node, cost)

fn first_step(roads: &Graph, allocator: &Allocator) -> Result<(), U64>:
    let infinity: U64 = 1000000
    brand roads.nodes() as nodes:
        var distance = nodes.distances(allocator, infinity)?
        if let Some(home) = nodes.first():
            distance.set(home, 0)
            for next in nodes.neighbors(home):
                improve(&mut distance, next, 1)
        distance.close()
    return Ok(())
```

The `brand` keyword opens a scope and binds the result to the name after
`as`. In `brand roads.nodes() as nodes:`, the method declares one
`Fresh` parameter. Each opening site supplies a distinct, unnameable type
argument. Only the opening expression may call that function. Helpers
declare `G: Brand` to accept an existing brand inferred from their arguments.
Even two openings of the same graph have distinct types. Branded values
stay inside the block, including when nested in another type. An unbranded
result, such as a node number, can leave. Re-admitting that number with
`nodes.node(i)` checks it against the receiving graph.

Brands are erased after checking. A brand says which opening issued a
value. Loans govern access to backing storage, and the library governs
bounds, occupancy, and any reuse of slots.

[Introduction to brands](../book/brands.md) ·
[Brand rules and graph API](design-brands.md) ·
[Runnable graph example](../tests/fixtures/brands/graph-first-step.wy) ·
[Cross-opening refusal](../tests/fixtures/brands/same-graph-twice.refused.wy) ·
[Escape refusal](../tests/fixtures/brands/escape.refused.wy)

## Control flow

```
if n > 3: "big" else: "small"            // one line: a value, or a statement
match shape:
    Circle(r) if r > 10:                  // a guard
        1000
    Circle(r):
        r
    _:
        0
match n:
    0:
        "none"
    1..=9:
        "few"
    many:
        "lots"
if let Some(v) = lookup(1):
    console.print_int(v)
let v = if let Ok(n) = map.get(&k): *n else: 0
while let Ok(item) = items.take_last_mut():
    item.close()
for i in 0..n:                            // also a..=b, and `for x in &vec`
for (i, item) in names.enumerate():       // the index too; `for (a, b) in pairs` takes apart
let Ok(file): File<Read> = dir.open("in.txt") else Err(error):
    return
let text = dir.read_text(&allocator, "rows.csv")?
```

([wasm-sums.md](wasm-sums.md); `sum/guards.wy`, `arithmetic/match-binding.wy`,
`sum/if-let*.wy`, `sum/while-let.wy`, `loops/inclusive.wy`, `loops/enumerate.wy`,
`loops/tuple-pattern.wy`, `try/chain.wy`.)

A refutable `let` evaluates the right-hand side once. It binds the payload
for the rest of the block on success. The failure arm must leave with
`return`, `break`, `continue`, or `unreachable`. A written payload type can
supply missing generic arguments to the call on the right.
[Refutable bindings](wasm-results.md#refutable-let-with-a-written-type) ·
[Typed binding example](../tests/fixtures/guard/typed.wy)

A multiline `if` or `match` can also produce a value. Each path that continues
supplies its last expression as the value. The same form works with `return` and
assignment. [Value-producing blocks](wasm-choose.md)

## Owners

A `String`, `Vec`, `File`, `Task` or record holding one is an owner.
Transfer it with `move`, or consume it through an operation such as `close`
or a supported `discard`. The checker refuses lost, doubled and used-after-move
owners. `using name = value:` calls the type's `Discard` implementation
on block exit if the binding still holds the owner.
[Discard](wasm-discard.md) · [Scoped cleanup](wasm-using.md)

References (`&v`, `&mut v`) lend a place without moving it. A unique loan
excludes overlapping loans, while disjoint fields can be lent independently.
A live loan prevents conflicting writes, moves and consumption of the owner.
A returned reference must come from an eligible input loan. Returning a
reference into local storage is refused.
[Reference and loan rules](wasm-references.md) ·
[Local escape refusal](../tests/fixtures/slices/local-escapes.refused.wy) ·
[Mutation during a slice loan](../tests/fixtures/slices/mut-vec-push.refused.wy)

An owner behind a `&mut` is replaced
with `memory.exchange`, or assigned (`(*shelf).slot = Full(move item)`) where the function's
proof shows the value there holds no owner. A value proved to hold no owner is ended with
`memory.vacant(move value)`: nothing is closed, because there is nothing to close. Where
either proof fails, the refusal points at that statement and names the place
([wasm-exchange.md](wasm-exchange.md)).

```
var names = move made                     // a Vec<String>
names.push_mut(&allocator, move name)
console.print_int(points[i].x)            // an element's field by index
cells[0].count += 10
```

A returned view keeps its backing storage lent. A function can specify the
origin of a loan for a particular result variant with `borrows Ok from left`.
Destructuring a view can expose the individual loans it holds. An inherent
method can use a view receiver such as `self: Self { &mut count }` to borrow
only the listed fields.
[Result loans](wasm-references.md#where-a-variant-borrows-from) ·
[View receivers](wasm-references.md#methods-that-borrow-some-fields-view-receivers) ·
[Views](wasm-views.md)

## Typed storage and links

`memory.try_alloc(&allocator, count)` returns a `Result<Buffer<T>, U64>`.
A buffer owns capacity and tracks which typed cells are initialized.
`memory.init`, `read`, `ref`, `mut`, and `take` check the cell before using it.
`take` moves the value out and leaves an empty cell. `memory.try_init` returns
the original item on failure, and `memory.try_release` returns a buffer that
still contains values. Consume owned elements before releasing their storage.
Allocation remains fallible even when one buffer holds many elements. Operations such as `memory.get_unchecked` and
`memory.move_prefix_unchecked` require proof admission for their exact range
and cell-state obligations.

`Vec<T>` owns contiguous elements. A length separates initialized elements
from spare capacity. `String` uses a `Vec<U8>` with a UTF-8 invariant. The Wasm
allocator reserves bitmap space for general buffers that permit holes. Dense
vectors use length-based updates on both targets and leave those bits untouched.
[Typed storage](wasm-typed-storage.md) ·
[Storage contracts](wasm-storage-contracts.md) ·
[Fallible initialization with owners](../tests/fixtures/buffer/try-init-buffers.wy)

`Boxed<T>` owns a heap value when nonempty. A `Weak<T>` can name that allocation
in data without keeping it live. A proof must connect a weak link to a live
loan before code can follow it. The `cell_list` library also uses a membership
proof to keep traversal within the chosen list.
[Boxes, weak links, and linked lists](wasm-linked-storage.md)

## Closures, generators and tasks

```
let sq = fn(n: U64): n * n                // the result type from the body
gen fn upto(limit: U64) -> U64:
    for i in 0..limit:
        yield i
impl Counter:
    gen fn upto(self: &Counter) -> U64:   // a generator method
        ...
let Ok(task): Task = Task.spawn(&allocator, 1024, worker, 3) else Err(given):
    return
```

([Closures](wasm-closures.md), [Generators](wasm-generators.md), [Tasks](wasm-tasks.md);
`closures/inferred-result.wy`, `generators/method.wy`, `programs/scheduler.wy`.)

Public functions that may pause declare `suspends`.
A `nosuspend:` block limits its calls to functions that cannot pause, and
a generic bound such as `T: Read nosuspend` limits calls through that bound.
[Suspension rules](design-async.md#the-rules) ·
[Effect checks](../tests/fixtures/effects/)

## Contracts and proofs

```
fn mix(a: U64, b: U64) -> U64:
    requires a < 1000000
    requires b < 1000000
    ensures result >= a
    proof:
        auto
    a * 2 + b
```

For supported contracts, the compiler generates obligations from the checked
source and the Lean kernel checks them before publication. `requires` supplies
facts to the body and creates obligations at calls. `ensures` describes the
result or state on return, and `old(...)` refers to supported entry-state
observations. Loop invariants and `assert` supply additional claims to prove.
Named `predicate` declarations let a module reuse specification expressions.
Proof models support different forms of state and control flow.
[Source contracts](wasm-source-contracts.md) ·
[Scalar proofs](wasm-scalar-proofs.md) · [Predicates](wasm-spec-predicates.md)

- `pure fn` limits authority through parameters, including nested capabilities.
  It can compute and trap. [Purity](wasm-capabilities.md#pure-fn)
- `nopanic fn` checks that the function and what it calls cannot trap from
  the operations they use, taking admitted proofs into account. Resource
  exhaustion is a separate runtime limit. [Trap analysis](wasm-traps.md)
- `unreachable` explicitly traps if execution reaches it. A supported scalar
  proof must show that path is impossible. [Unreachable paths](wasm-unreachable.md)

When a scalar proof covers the relevant operations, the Wasm emitter can omit
overflow or bounds checks. The native emitter can omit proved bounds checks
and checks on slice accesses that the source checker establishes are in range.
[Native lowering rules](native-backend.md#supported-operations-and-trust-boundary)

## Sealed types and invariants

```
sealed type Ring<T: Any + Discard>:
    slots: Vec<Option<T>>
    head: U64
    count: U64
    capacity: U64
    invariant head < capacity
    invariant count <= capacity
```

- **Sealed**: only the file that declares the type names its fields, builds a value
  (`Ring(slots: ..., head: 0, ...)`) or takes one apart. Other files hold values and call
  the type's `pub` functions.
- **`invariant CLAUSE`** says what is true of the plain fields (numbers, `Bool`s, which
  variant a field holds) of every value. Each function in the file that takes one may
  assume it; each that builds one or writes a field through `&mut` is proved, by the Lean
  kernel, to leave it true at every `return`. A clause that does
  not prove is refused by name: ``not proved: the invariant `count <= capacity` ...``.
- **`invariant when PATTERN = field:`** states clauses for the values in which `field`
  is that variant (`when Some(job) = held: id >= 1`).
- **What the defining file may do while the invariant is broken**: between two field
  writes through a `&mut` parameter the value is used only through its fields; it is not
  lent whole, copied, or passed to a method until the invariant holds again.
- **A field the invariants do not read can be lent**: in the type's own file, in a function
  proved over numbers, `(*ring).slots.at_mut(i)` keeps a `&mut` into `slots` while `head` and `count` stay proved, because no clause names
  `slots`. A field an invariant reads (as a value or as a `when` subject) is written
  directly, never through a kept reference (`tests/fixtures/sealed-storage/`;
  [wasm-references.md](wasm-references.md)).
- **`invariant forall i in (0, items.len()): CLAUSE`** says the clause of every element of a
  `Vec` field: `forall i in (0, spans.len()): spans[i].start + spans[i].len <= bytes.len()`.
  The clause reads number fields of `items[i]` and of the type itself. It is shown after every
  call that changes the elements (`push_mut`, `v[i] = x`, `set`, `swap`, `take_last_mut`) and
  every change to a field it reads, and is a fact wherever an element is read (`items[i]`,
  `items.at(i)`, a slice from `items.as_slice()`), so a read through a stored offset needs no
  range test. A value is built with the field empty; a field that bounds the elements may
  grow and not shrink ([proposal-element-invariants.md](proposal-element-invariants.md),
  `tests/fixtures/element-invariants/`).
- **A clause may read a number through a method of a field** (`invariant items.len() ==
  capacity`, `ensures queue.items.len() == ...`) when the method's body is one `return` of a
  field of its receiver (`Vec.len` returns `length`): the proof reads it as that field. Such a
  field may also be lent for good to a call whose result cannot reach that number by its
  signature (`(*q).items.at_mut(i)` answers a `&mut T`, never a `&mut U64` into the header):
  the length stays what the callee's `ensures` says (`at_mut`, `set`, `swap`, `exchange`,
  `trade`, `swap_through` and `v[i] = x` keep it), and anything that changes it (`push`, `pop`,
  `truncate_mut`, `clear_mut`, exchanging the whole Vec) is followed or forgotten, so the
  invariant is shown again at each `return` (`tests/fixtures/sealed-storage/len-*`,
  `ring-length.wy`, `pq.wy`). A clause outside the vocabulary is refused at its own line,
  saying which construct it is.
- **An owner in a field is assigned** (`(*lease).held = Some(move job)`) where the proof
  shows what was there holds no owner, which is what a `when` clause is usually for; and
  a value proved to hold none is ended with `memory.vacant(move value)`
  ([wasm-exchange.md](wasm-exchange.md), [wasm-scalar-proofs.md](wasm-scalar-proofs.md)).

## Rules met in the first hour

- **A name is bound once in its scope.** `let x = ...` where an `x` is already in scope is
  refused, in a nested block and in a `match` arm too (two arms may each bind `name`; an arm
  may not bind a name the function already has). `_` is the exception: it names nothing.
- **A `Result` is not dropped.** A call that gives a `Result` is handled (`match`,
  `let Ok(v): T = ... else Err(e):`, `if let`), passed on with `?`, or ignored in so many
  words: `let _ = risky()`. Left as a statement it is refused (`result/dropped.refused.wy`,
  `result/ignored.wy`). `let _ =` does not drop a value that holds an owner.
  The appends of `String` (`push`, `push_text`, `push_u64`, `push_format`, ...) and
  `Vec.reserve_mut` answer `Result<(), U64>`, so they are held to this rule: `Err(1)` is no
  room, `Err(2)` an argument that was refused (not a scalar, an end inside one). In a function
  that returns `Result<_, U64>`, `text.push_text(a, "x")?`; elsewhere `if let Err(_) = ...:`.
  `.is_ok()` and `.is_err()` (`import string` brings them) read well in a condition and cost
  a call on a reference each, about twenty instructions: use a `match` or `if let` in a loop.
  `Vec.set`, `swap` and `exchange` answer `Result<(), U64>` too (`Err(1)`: not an element).
  Where the index is known to be one, write `items[i] = value`, which stops the run if it
  is not.
- **`console.line` takes a `&str`, `console.print` a format.** `console.line("done")` writes
  the text and a newline; `console.print("total ${n}")` writes anything with `${...}` in it,
  and a newline; `print_int`, `write` and `write_string` are the pieces
  ([library-text.md](library-text.md)).
- **After a nested pattern, the outer variant still needs its arm.** `Ok(Some(v)):` and
  `Ok(None):` do not cover `Err`: write `Err(_):` (or `Err(e):`) as well, or a `_:` arm.
  `Variant(_)` is refused where the value holds an owner, which has to be named and closed.
- **`using name = value:` has no `else`.** It closes `name` at the end of the block; to
  handle a failure first, take the value out with `let Ok(name): T = ... else Err(e):`.
- **A value that holds references is opened like any other**: `let Pair(left, right) = pair`,
  and `Two(a, b):` in a `match`. Each name that holds a reference keeps what the whole value
  borrowed until that name's last use (`views/opened.wy`, `views/variant-two-spans.wy`).
- **Methods and functions that need an import of their own.** A type's file gives its core
  methods; the rest are added by other modules, and the refusal names the module to import:

  | Wanted | Import |
  |---|---|
  | `text.span()`, `Span`, `write_span`, `get_by_span` | `span` |
  | `find_text`, `slice`, `parse_u64`, `push_u64`, `equals_text` on a `String` | `text` |
  | `replace(&mut place, next)` | `replace` |
  | `value.discard()` on numbers, `Vec<T>`, `HashMap`, and `using` | `discard` |
  | `sort`, `before`, `Order` | `sort` |
  | `+ - *` on your own types (`Add`, `Sub`, `Mul`) | `ops` |
  | `Some`/`None` helpers (`take`, `is_some`) | `option` |
  | `min`, `max` and other number helpers | `int` |

## Package tests

Run `grimoire test .` from a package with a `test.wy`. A `test fn` declares a
case in a selected test module. Cases can return `Result<(), Failure>` and
use `?` with helpers from `import testing`. A preceding `using provider` can
select a fixture provider. A suite can also expose inputs in
`main(tests: Tests, grants: TestGrants)`, as in the outcomes fixture, which
offers a value for the cases that ask for `input: U64`.

Test helpers can declare a caller-site parameter as
`site: U64 = source_site()`. A direct call can use the default or supply a
named site such as `source_site("nonzero")` for diagnostics.
[Inputs and assertion example](../tests/packages/outcomes/test.wy) ·
[Assertion helpers](../library/wasm-testing.wy)

## Feature index

The index lists implemented language and library surfaces. Host grants and
lowering coverage vary by target. See [native coverage](native-backend.md#supported-operations-and-trust-boundary)
and the [Wasmtime host](wasm-production-build.md).

| Feature | Guide or example |
|---|---|
| Numeric widths, checked conversions, bitwise operations, signed numbers, and F64 | [Numbers](#values), [bits](wasm-bits.md), [signed](wasm-signed.md), [F64](wasm-f64.md) |
| Constants, literal defaults, argument labels, and caller-site defaults | [Constants](wasm-constants.md), [functions](wasm-functions.md), [package tests](#package-tests) |
| Generic functions and records, `Any`, trait bounds, `impl Trait`, and blanket implementations | [Generic functions](wasm-generic-functions.md), [generic records](wasm-generic-records.md), [traits](wasm-traits.md) |
| Methods, equality, ordering, and operator dispatch | [Methods](wasm-methods.md), [traits and operators](wasm-traits.md#operators) |
| Nominal records, aliases, tuples, structural records, sums, and anonymous unions | [Data types](#records-sums-and-tuples), [sums](wasm-sums.md), [anonymous unions](wasm-unions.md) |
| Target-dependent branches with `static if` | [Target selection](wasm-derive.md#target-selection-in-ordinary-code) |
| Derives, imported recipes, `by:` and `for:`, and compile-time iteration over fields and variants | [Derive recipes](wasm-derive.md#recipes-from-a-module), [sum recipe fixture](../tests/fixtures/derive/recipe-variants.wy) |
| `HashKey`, `Equivalent<K>`, and query types by reference | [Key law](wasm-derive.md#the-key-law), [KeyMap](library-collections.md#import-keymap-librarywasm-keymapwy) |
| Match guards, nested patterns, `if let`, `while let`, conditional expressions, and value-producing blocks | [Sums and matching](wasm-sums.md), [value-producing blocks](wasm-choose.md) |
| `Result` and `Option`, refutable `let`, and propagation with `?` | [Results](wasm-results.md), [matching](wasm-sums.md), [propagation fixture](../tests/fixtures/try/chain.wy) |
| Ranges, `for`, `enumerate`, `break`, `continue`, and iteration loans | [Iteration](wasm-iteration.md) |
| Moves, `Discard`, `using`, owner records, exchange, and proved vacant assignment | [Owners](#owners), [discard](wasm-discard.md), [using](wasm-using.md), [exchange](wasm-exchange.md) |
| Shared and unique references, loan lifetimes, result borrowing, view receivers, and evaluation order | [References](wasm-references.md), [borrow order](wasm-borrow-order.md) |
| Arrays, indexed places, slice and sub-slice loans, and views that carry loans | [Indexing](wasm-index.md), [slices](design-slices.md), [views](wasm-views.md) |
| `brand`, `Fresh` issuance, `Brand` inference, restricted escape, and graph nodes and tables | [Brands](#brands-and-scoped-type-identity), [graph API](design-brands.md#the-graph-library), [proposals](design-brands.md#proposed-extensions) |
| Typed buffers, allocation and occupancy, and bulk slice copies and fills | [Storage](#typed-storage-and-links), [proved buffer operations](wasm-proof-memory.md), [slice operations](design-slices.md#operations-built) |
| Heap boxes, weak links, and proofs of linked-list membership | [Linked storage](wasm-linked-storage.md) |
| Sealed types, constructor privacy, and representation invariants | [Sealed types](#sealed-types-and-invariants) |
| Function values, inferred closure types, captures, and closures used once | [Closures](wasm-closures.md) |
| `gen fn`, `yield`, generator methods, and nested generators | [Generators](wasm-generators.md) |
| Tasks, suspension, and `nosuspend` | [Tasks](wasm-tasks.md), [suspension rules](design-async.md) |
| Contracts, `old`, loop invariants, `assert`, lemmas, named predicates, and guarded clauses | [Contracts](#contracts-and-proofs), [proof models](wasm-proofs.md), [named predicates](wasm-spec-predicates.md) |
| `pure fn`, `nopanic fn`, `unreachable`, and defined runtime traps | [Function guarantees](#contracts-and-proofs), [trap diagnostics](wasm-traps.md) |
| Imports, privacy, package graph, and file policies | [Modules](wasm-modules.md), [packages](wasm-packages.md), [capability policy](wasm-capabilities.md) |
| Capability wrappers, grantable policy data, and reviewed grant documents | [Capabilities](wasm-capabilities.md), [grant documents](wasm-capabilities.md#grant-documents) |
| Console, files, network and HTTP, process execution, environment, arguments, time, randomness, crypto, and secrets | [Host grants](wasm-capabilities.md), [files](wasm-files.md), [HTTP](wasm-http.md) |
| Text, owned strings, borrowed spans, interpolation, encoding, and JSON | [Text library](library-text.md), [formats](wasm-text-format.md), [JSON](wasm-json.md) |
| Vec, Deque, Heap, HashMap, KeyMap, sorting, linked lists, and graphs | [Collections](library-collections.md), [linked lists](wasm-linked-storage.md#walking-a-linked-list), [graphs](design-brands.md#the-graph-library) |
| Package test cases, fixtures, and source-site diagnostics | [Package tests](#package-tests) |
| Wasm and native ARM64 execution | [Wasmtime build and launch](wasm-production-build.md), [native coverage](native-backend.md) |

## Limits that remain

These limits bound the compiler's work, the shape of a value, and the Wasm
host's module size. Runtime memory and native frame limits have their own
settings. [Wasm memory limits](wasm-memory-sizes.md) · [Native target](native-backend.md)

| Limit | Value | What to do at it |
|---|---|---|
| Source file | 64 KiB | split the file |
| Whole program (all files) | 1 MiB, 64 files, imports 16 deep | |
| Declarations in a program | 4096 | |
| Record, array, sum and Buffer types laid out (every instance of a generic type counts) | 4096 | fewer distinct instances |
| Fields of a record | 32 | nest a record |
| Numbers a record holds in all (a slot each: an array field counts per element) | 32 | keep a long array beside the record, or in a `Vec` |
| Elements of a fixed array | 256 | a `Vec` |
| Numbers a sum value holds, tag included | 32 | box the large variant, or hold a `Vec` |
| Numbers in a function's parameters, and in its result | 32 each | pass a reference; return through `&mut` |
| Locals outside the frame | 256 numbers | index a large array in place |
| Nesting of blocks and expressions | about 60 levels | name the inner part |
| Module the engine loads by default | 64 KiB (`--module-bytes`) | |

A proved function cannot yet take or hold a record that holds a reference (a `Span`):
read what the proof needs outside it (`text.len()`, once) and pass the number in
(docs/decisions.md, 2026-11-22, has the plan for lifting this).

## A server with state

`tests/fixtures/net/counting-server.wy` is a whole one: it counts the requests each path has
had and answers with the count. Its shape is the shape of any service that keeps something
between requests:

```
import net
import http

type Counts:                              // the state
    hits: HashMap<String, U64>
    served: U64

fn main(console: Console<Write>, listener: Listener, allocator: Allocator):
    var counts = Counts(move made, 0)     // owned by main, for the whole run
    var buffer = move room                // one request buffer, reused
    while serving:
        match listener.accept():
            Ok(socket):
                let _ = serve(&mut counts, &allocator, &socket, &mut buffer)
                socket.close()
            Err(failure):                 // TimedOut: no one connected in the host's wait
                serving = false
```

One arm for every `Err` is right here only because stopping is what every accept failure
should do. Do not carry the shape to a read: `Err(failure)` on `data.read_text("log")` covers
`NotFound` (start empty) and every other failure (stop, or the next write replaces a file that
was only unreadable). Match `NotFound` by name and treat the rest as a failure.

- **The state is `main`'s, lent `&mut` to one request at a time.** Nothing is global; a
  function that is not given `&mut Counts` cannot change it, and one not given the `Socket`
  cannot answer.
- **`serve` reads until `http.has_head` says the head is whole**, with `socket.read_some`
  into the buffer, and returns `Result<U64, NetError>` so `?` carries a failed read out.
- **`http.request_on(socket, bytes)` parses the request and checks its `Host`** against the
  listener the run granted; its `Err` is the status to answer (400 or 421). A server that
  uses `http.request` alone answers under whatever name the client used.
- **The parts of the request are `Span`s into the buffer**: nothing is copied until a key is
  kept (`get_or_insert` makes the `String` key only for a path not seen before).
- **The reply is built in a `String` and sent with `socket.write_all`**; each `Result` is
  handled or ignored in so many words.
- **Run it** with a grant for each of `main`'s parameters:
  `witchy-wasmtime-host server.wasm --output --allocator --listen=127.0.0.1:8080`. Output is
  shown when the run ends unless the launcher streams it; the run's budgets (`--net-bytes`,
  `--deadline-ms`, fuel) are per run, so a long-lived server raises them
  ([wasm-capabilities.md](wasm-capabilities.md)).
- For one task per connection, with a time limit each, see `examples/echo/tasks.wy` and
  [design-async.md](design-async.md); `import net` no longer leaves too little room for it.

