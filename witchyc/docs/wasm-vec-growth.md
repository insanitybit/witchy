# Ordinary source Vec with recoverable growth

`library/wasm-vec.wy` defines a generic Copy-element Vec using the existing
checked Buffer primitives. It has no main. A client currently concatenates its
UTF8 library snapshot and source into one closed compiler input; this is closed
module bundling, not import syntax or a compiler-recognized Vec implementation.
The source library passed independent review and focused qualification at
ad5ce5b: nine ordinary controls and one optional scalar-proof artifact control.
No runtime contract syntax or proof infrastructure changes are part of this library.

The wellformed sequence convention is explicit: stored capacity equals the
actual Buffer capacity, length is at most capacity, slots below length are Full
in sequence order, and the remaining slots are Empty. The source typechecker
enforces borrowing and owner transfer; it does not prove this sequence invariant.
The public Vec record permits forged headers, so successful source typechecking
alone is no functional correctness certificate.

Superseded: `Vec` is now sealed and states this convention as a type invariant
(see "Type invariants" below). Headers cannot be forged, the library no longer
re-checks them at run time, and the passages here about forged headers describe
the earlier library.

| API | Behavior |
| --- | --- |
| `Vec.new` | Fallible allocation, initially empty at requested capacity. |
| `Vec.len` / `Vec.capacity` | Read stored length / actual Buffer capacity. |
| `Vec.push_spare` | Initialize next slot; failure returns Vec and Copy item. |
| `Vec.take_last` | Take the final slot; success returns item and remaining Vec. |
| `Vec.try_release` | Empty release; failure returns the owner unchanged. |
| `Vec.close` | Destructively take all actual slots and release their allocation. |
| `Vec.reserve` | Preserve at least requested capacity, with recoverable growth. |
| `Vec.push` | Spare push, or reserve with 0→1 / doubling growth, then push. |

Reserve checks stored capacity and length against the actual allocation before
mutation. A no-growth request returns the same owner without allocating or
validating the prefix. Growth allocates a new Buffer first, then reads and copies
the old prefix without taking from it. It also checks the old tail is Empty.
Any allocation, prefix-read, new-slot initialization or tail-state failure cleans
the partial new Buffer and returns the original Vec. Only after copying and
validation succeed does it drain/release the old allocation and publish the new
Vec with the same length and order. Cleanup scans actual capacity, takes Full
slots, and handles release Err by retaining and retrying its returned owner.

Forged capacity/length headers fail unchanged in reserve, push, spare push,
take-last and try-release. Growing reserve also rejects prefix holes and Full
tail slots without changing the old allocation. No-growth is not an invariant
validator; targeted push/take use normal slot-state errors, and close deliberately
destroys all actual slots regardless of header claims. Invalid/stale raw handles
or allocator corruption may trap under the existing discarded-instance policy;
there is no guest unwind guarantee. Rollback preserves the old owner, capacity,
initialized state and contents. Temporary allocation/free generations may advance:
global allocator memory is not promised to roll back bit for bit.

Doubling checks `capacity <= MAX_U64/2` before addition. The current bounded
physical heap makes that overflow branch unreachable for wellformed vectors;
tests of MAX requests and forged headers do not claim to execute it. Allocation
descriptor-size arithmetic remains the existing allocator's responsibility.
Normal test capacities are at most four. Slot exhaustion uses capacity-zero
holders, unwinds each returned owner, and preserves an unrelated live Vec.

Tests cover U8, U64, Bool and padded Copy records, actual sequence/capacity output,
prefix-copy commit, stale old token refusal, no-growth identity, Full release Err,
oversized allocation, partial-new cleanup, actual pool reuse and no normal-path
live allocation leaks. Diagnostic export copies execute actual emitted functions
and six helpers, deriving indices from concrete instance count M. Production
must reject them. Metadata entry address zero is valid. Such copies and byte
snapshots do not prove allocator, stack or object invariants.

The Copy element storage limit is thirty carriers; push/take wrapper Results
impose a narrower `7 + element_carriers <= 32` bound, at most twenty-five. Owner
elements and zero-sized types remain unsupported. Artifact controls bind the
exact bundled source/library snapshot, current compiler, receipt, module and
independent operator grants, with self-hashed tamper and publication preservation.
One optional ordinary Lean root control checks only a scalar ghost lemma and
proof-byte erasure, not the Vec algorithm. Generic functional proofs, owner T,
ZST, String, unsafe admission, reusable proof admission, performance and overall
goal completion remain open.

## Elements that own storage

A plain type parameter `T` means Copy storage: the body may copy and drop a
`T`, and an owner cannot be substituted. A parameter declared `T: Any` accepts
any storable value, owners included. The checker then treats every `T` in the
body as a linear owner: it needs an explicit `move`, is consumed exactly once
and can never be dropped. This is a property of the type parameter, not of Vec.

A Buffer element or record type argument may be Copy storage or any record or
Buffer that owns.

A record may hold several owners. `move value.field` takes the owner out of a
record that has exactly one and drops the Copy remainder; a record with several
is opened whole with `let Name(a, b) = move value`, which binds every field in
declaration order. Each owner bound this way is owed exactly as any other local
owner, so none can be lost. Opening a sealed record belongs to its declaring
file.

Which cell operations are allowed follows from what they do to the cell:

| Operation | `T: Any` | Why |
| --- | --- | --- |
| `memory.take`, `memory.take_unchecked` | yes | move the value out and empty the cell |
| `memory.init_unchecked` | yes | moves the value into a proved-empty cell |
| `memory.initialized`, `memory.capacity` | yes | look at occupancy only |
| `memory.get_unchecked`, `memory.read` | no | copy the cell |
| `memory.init`, `memory.swap` | no | drop the payload when the guard fails |

`memory.try_release` already refuses an allocation with an occupied cell, so
stored owners cannot be released along with their Buffer.

The Vec functions whose bodies only move are declared `T: Any`: `Vec.new`,
`Vec.len`, `Vec.capacity`, `Vec.try_release`, `Vec.push_spare`,
`Vec.push_spare_mut`, `Vec.take_last`, `Vec.take_last_mut` and
`memory_init_unchecked`. Their proofs are unchanged; an element is still an
opaque value to the kernel. `tests/fixtures/vec-string/owned.wy` stores Strings
in a `Vec<String>`, takes them back, closes each and releases the vector, in
Wasmtime. `tests/fixtures/vec-string/recover.wy` does the same through
`VecPushError<String>` and `VecTaken<String>`, which carry a vector and a
String together.

Growth works for owning elements too (see "Growth by moving" below). Not
available for them: `Vec.get` (it copies; there are no element references yet),
`Vec.close` and `Vec.truncate` (they drop what they take out). A vector of
owners is emptied with `Vec.take_last` or `Vec.take_last_mut` and then released
with `Vec.try_release`.

## Sealed types

`sealed type Name:` confines three things to the source file that declares the
type: calling its constructor, assigning to its fields (directly or through a
borrow), and extracting its owner field with `move value.field`. Any file may
still read its fields, pass it around and call the declaring file's functions.
`Vec` and `String` are sealed, so a program importing the library can no longer
forge a length or capacity. Text concatenated into one file is one file: the
boundary is the import, not the declaration order.

Sealing is enforced by the checker. It is what makes a type invariant (below)
hold everywhere.

## Anonymous records

`.{first: String, count: U64}` is a record type with no declaration. Its
identity is its set of labelled fields: label order does not matter, and two
functions that write the same shape mean the same type. It is never the same
type as a named record with the same fields.

```
fn both(allocator: &Allocator) -> Result<.{first: String, second: String}, U64>:
    ...
    return Ok(.{first: move one, second: move two})

    let .{first, second: other} = move found
```

A value is written `.{label: value}`; `.{first, second}` is short for
`.{first: first, second: second}`. Fields are read with `value.label`. A record
that owns is opened whole with `let .{label: name, ...} = value`, naming every
field once in any order, with the same ownership rule as a named record: each
owner bound is still owed. Anonymous records work as generic arguments and
results, as Result payloads and as Buffer elements.

`type Pair<T> = .{first: T, second: T}` gives a shape a name without making a
new type: the alias and the written-out shape are interchangeable, and an alias
may stand for any type. Because an alias is not a type of its own it has no
constructor and cannot be sealed.

Fields are stored in label order. Operands may be written in any order unless
one of them has an effect (a call, for instance); then they must be written in
label order or bound to names first, so evaluation order is always the order on
the page. There is no width conversion between shapes, and an anonymous record
cannot be sealed or carry an invariant: a type that needs either is declared
with `type`. `tests/fixtures/anonymous/pair.wy` runs these forms in Wasmtime.

## Type invariants

A sealed type may state what every value of it satisfies:

```
sealed type Vec<T>:
    data: Buffer<T>
    length: U64
    capacity: U64
    invariant memory.prefix(data, length, capacity)
```

The expression is written over the type's own fields. `memory.prefix(buffer,
length, capacity)` says the stated capacity is the allocation's, the length
fits, every cell below the length holds a value and every cell from the length
on is empty. Only a sealed type may state an invariant, since anyone could break
it otherwise.

How it is used, in both proof models (owned records and borrowed memory):

- A proved function assumes the invariant of every sealed value it receives, at
  any depth inside its parameters (`input.bytes` of a `String`, for example).
- In the file that declares the type, a proved function must show the invariant
  for every sealed value it returns, for the final state of every `&mut` borrow
  of one, and for every sealed value it passes to another proved function.
- In any other file nothing is owed: such code cannot build, write or take apart
  the value, so it cannot hold a broken one. It may rely on the invariant of
  whatever a library function hands back.
- In the declaring file, a function that builds a value of the type, writes its
  fields or takes it apart must carry a proof. Reading a field needs none.
- A loop in the declaring file states the invariant of a sealed local it changes
  as an ordinary loop invariant, since the value may be broken mid-body.

Inside the declaring file, a function proved over owned records that lends a
sealed value to a helper proved over borrowed memory shows the invariant at the
call in the helper's vocabulary. Other files owe nothing there either.

Consequences for the library. The Vec functions no longer compare the stored
capacity with the allocation or test the length before working; `Vec.get` reads
below the length without a run-time initialization test; `Vec.push_spare` writes
at the length without one. `vec_well_formed` is gone, as is the tail scan in
`Vec.reserve`. Contract clauses the invariant already implies
(`output.capacity == memory.capacity(output.data)` and the like) were removed.
`Vec.push` grows a full vector to twice its capacity plus one. Refusal controls:
dropping or miscounting the length update in `Vec.push_spare`,
`Vec.push_spare_mut`, `Vec.take_last` or `Vec.take_last_mut`, constructing with
a wrong length in `Vec.new` or a wrong capacity in `Vec.reserve`, and reading at
`index <= length` in `Vec.get` are each rejected by Lean.

Proof scripts changed in three general ways: a leaf that is a premise word for
word is closed by that premise; an equality stated in both directions is kept
once (the pair used to send rewriting into a loop); and scripts are tried
cheapest first. The memory model gained checked subtraction and a matched
checked `memory.take`.

Still trusted: that the checker enforces sealing, and the translation of the
invariant into both models' statements.

## Unchecked take

`memory.take_unchecked(&mut buffer, index) -> T` moves the value out of a cell
and leaves the cell empty. It performs no bound or occupancy test at run time,
only the token and type validation every Buffer use performs. It is legal only
in a function with a contract and `proof: auto`, where two obligations are
kernel-checked at the call: `index < capacity` and the cell holds a value.
Nothing is copied, so the element type may own storage (`T: Any`).

`Vec.take_last` and `Vec.take_last_mut` use it: the Vec invariant says every
cell below the length holds a value, so with a non-zero length the last cell
can be taken without a guard, and neither function has a storage-failure branch
any more. `Vec.take_last_mut` returns `Err(2)` only for an empty vector.
Refusal controls: taking at `length` instead of `length - 1`, dropping the
emptiness test, and leaving the length unchanged afterwards are each rejected
by Lean in both functions.

For these proofs a fact about one cell of a filled prefix is derived from the
prefix by first showing the cell's bound with arithmetic; the scripts try that
only after the direct search fails.

Trusted: the emitted Wasm for the operation (cell load, bitmap bit cleared by
subtraction, occupancy count decremented), which mirrors `init_unchecked`.

## Growth by moving

`Vec.reserve` and `Vec.push` are `T: Any`. Growing allocates the new buffer,
moves the stored values across, releases the old buffer and builds the new Vec:

- `memory_move_prefix(&mut source, &mut target, count)` takes each of the first
  `count` values out of `source` with `memory.take_unchecked` and stores it in
  the same cell of `target` with `memory.init_unchecked`. It requires `count`
  within both capacities, the source cells filled and the target cells empty,
  and cannot fail. It ensures the source prefix is empty, the target prefix
  holds the source's old values, and the rest of both buffers is unchanged.
- `memory_release_empty(buffer)` requires `memory.empty_all(buffer)` and
  releases it. Its failure arm calls itself; a self-call is never assumed, so
  the proof must show that arm cannot be reached.
- `Vec.reserve` shows both callees' requirements from the Vec invariant and the
  fresh allocation, and the invariant of the Vec it returns.

Nothing is copied, so there is no partial failure to roll back: reserve fails
only when allocation does, and then returns the vector untouched.
`tests/fixtures/vec-string/grow.wy` pushes five Strings into a `Vec<String>`
that starts with capacity 0 (capacities 1, 3, 7), takes them back and releases
everything, in Wasmtime.

General mechanisms this needed:

- A function proved over owned records may call a proved function that has
  `requires`. Each requirement is a leaf of the caller's body proof, read
  against the actual arguments. This holds for callees proved in either model.
  Before, such calls were refused.
- The borrowed-memory model accepts two exclusively borrowed buffers, and a
  Unit result for a two-buffer function.
- `memory.empty_all(buffer)` is accepted in owned-record contracts.

Refusal controls: dropping any of `memory_move_prefix`'s three state
requirements, moving one value too many, skipping the move before the release,
and dropping `memory_release_empty`'s requirement are each rejected by Lean.

`memory_copy_prefix` is gone; nothing in the library copies a prefix any more.

Proof scripts: whether a cell lies inside each allocation is now split on only
as a second attempt. With two buffers the splits quadrupled the cases of every
loop leaf and were almost never needed.

## Contracts of the in-place operations

`Vec.push_spare_mut` and `Vec.take_last_mut` work through `&mut Vec<T>`. Both keep the
allocation and the capacity.

| Function | `Ok` | `Err` |
|---|---|---|
| `Vec.push_spare_mut` | length is one more; the cell at the old length holds `item`; every other cell is unchanged | the vector was full; nothing changed; `item` comes back |
| `Vec.take_last_mut` | length is one less; the cell at the new length held the result; every other cell is unchanged | code 2; the vector was empty; nothing changed |

A `memory.cell` clause is checked as two facts: the value is present, and the cell holds it.

## Lending a record to a proved operation

A proved function can lend a record it owns to another proved function that takes it by
`&mut`. `Vec.take_last` is now this:

```witchy
    var vector = move input
    match Vec.take_last_mut(&mut vector):
        Ok(item):
            return Ok(VecTaken(move vector, move item))
        Err(code):
            return Err(move vector)
```

What the proof of the caller knows after the call:

- The allocation is the same one, with the same capacity. A function proved over a borrow
  has no allocator and nothing to swap in.
- The cells and every number in the record are unknown, except for what the callee's
  `ensures` clauses say. `old(...)` in those clauses is the record as it was before the call.
- The record's type invariant holds again, because the callee proved it keeps it.

The callee's `requires`, and the invariant of the record handed over, are checked at the
call as for any other proved callee.

Limits: the lent record holds numbers and one buffer (a record that also holds an element
is refused); the callee returns nothing, a number, a Bool, an element, or a `Result` of those.

This costs no proof time and saves none: `Vec.take_last` is 3 lines instead of 8 and the
Vec library checks in the same time.
