# Proofs of scalar code

A function can state `requires`, `ensures` and loop
`invariant` clauses, with `proof: auto`. The compiler reads the function's typed body
(the same one it emits Wasm from), produces one statement over natural numbers, and the
Lean kernel checks it. The generator starts in `src/compiler/scalar_vc.rs` and uses modules in `src/compiler/scalar_vc/`.

These are claims about numbers. Claims about what a buffer holds are the memory and
record models' (`docs/wasm-proofs.md`); a generic function whose clauses name `memory.*`
or sealed fields is proved there, and everything else here.

## What a function may be

- Any function, generic or not. Parameters and result can be of any type; clauses speak
  of the ones that are numbers or Bools, and of records through methods (below). A
  value of a type parameter is one nothing is known about. A generic function whose
  clauses are in the memory or record models' vocabulary is still proved there.
- Body: anything the language has. Numbers and Bools are followed exactly through
  `let`/`var`, assignment, `+ - * / %`, comparisons, `&&`, `||`, `!`, `if`/`else`,
  `while`, `match`, `break`/`continue` (in a loop without invariants), early `return`
  and calls, whether they are locals, fields of a record local, or fields of a record
  behind a reference. Everything else is a value nothing is known about.
- Clauses: names of scalar parameters (and, in an invariant, of the scalar locals in
  scope), literals, `+ - * / %`, comparisons, `&& || !`, a Bool name as a proposition,
  `result` when it is a number or Bool, `value.method()`, a field that is a number
  (`pair.left`, `two.a.left`, through a reference or not), and `*value` for a
  reference to a number.
- When the function returns a `Result`, `ensures match result:` with an `Ok` and an
  `Err` arm states clauses for each variant. They speak of the parameters, and of the
  value the variant holds through the arm's binder: the binder itself when it is a
  number (`Ok(h): h + h == n`), or its number fields (`Ok(made): made.low <= made.high`).

## Records in clauses: `items.len()`

A clause cannot name a field of a sealed type from outside its file, so it reads the
field the way the program does: `items.len()`, and in an `ensures` or an invariant
`old(items.len())` for the value on entry. The method must be one that only returns a
field of its receiver (`fn len(value: &Vec<T>) -> U64: return (*value).length`); the
compiler checks that against the method's body, so the clause and a call of the method
in the code mean the same field. A method that computes anything is refused in a
clause.

A parameter taken by value is read with `old(...)`: after the body it may be gone.

## Fields in clauses: `pair.left`, `*value`

```
fn grow(pair: &mut Pair):
    requires pair.left < 100
    ensures pair.left == old(pair.left) + 1
    ensures pair.right == old(pair.right)
```

A clause names a number that is a field of a parameter, at any depth of records, or
`*value` when the parameter is a reference to a number; `old(...)` of either is its
value on entry. In a loop invariant the root may be a local. The field is the same
key the walk already used for a method that returns it, so the two spellings mean one
thing. A field of a sealed type is named this way only in the type's own file;
elsewhere its methods are the way to read it, and the clause is refused.

A caller that lends part of a record (`grow(&mut two.a)`) reads the callee's clauses
as facts about that part and keeps what it knew of the rest. A callee with no
contract leaves nothing known about what it was lent.

`tests/fixtures/fields/proved.wy` runs in Wasmtime. Twelve `*.refused.wy` beside it
must not compile: a sum that is one too many, a callee's `requires` not shown, a
clause about the field that was not lent, a callee with no contract, a field the
callee did not speak of, a U8 claimed below 100, a record or a whole referent
overwritten, a store through `*value` claimed unchanged, a loop step past its
invariant, a by-value parameter named without `old`, and a record compared whole.

### Fields of the result, and Bool fields

```
fn order(a: U64, b: U64) -> Pair:
    ensures result.low <= result.high

fn bump(flag: &mut Flag):
    ensures flag.on
    ensures flag.count == old(flag.count) + 1
```

An `ensures` may read a field of a record the function returns, at any depth. At
each `return` the walk follows the returned record the way it follows one written to
a local: a constructor's parts, a local's known parts, or what a callee's contract
says of its own result. A caller reads the clause as a fact about the parts of the
value it receives, by the rule that already carried `ensures match result:` values.
`result.field` is refused in `requires` and inside `old(...)`; a result that is a sum
is still read only through `ensures match result:`.

A Bool field is a proposition wherever a Bool name is: alone, under `!`, joined by
`&&` and `||`, compared with `==` and `!=`, and inside `old(...)`. It is not a number:
`flag.on + 1` is refused.

`tests/fixtures/scalar/result-fields.wy` runs in Wasmtime. Six `*.refused.wy` beside
it must not compile: a pair returned the wrong way round, `result.low` in a
`requires`, a subtraction the callee's clause does not cover, a flag nothing sets, a
callee's `requires` on a flag passed off, and a Bool field used as a number.

## Clauses for each variant

```
fn push_mut(vector: &mut Vec<T>, allocator: &Allocator, item: T) -> Result<(), T>:
    ensures match result:
        Ok:
            vector.len() == old(vector.len()) + 1
        Err(back):
            vector.len() == old(vector.len())
```

At a `return Ok(...)` only the `Ok` clauses are shown, at a `return Err(...)` only the
`Err` clauses. Where a call's result is returned as it is (`return
vector.push_spare_mut(move item)`), each arm's clauses are shown from what the callee's
contract says of that arm. Where the variant cannot be read from the `return`, every
clause of both arms must hold. A caller gets each arm's clauses in the matching arm of
its `match`, `let ... else` or `?`, as for the Vec library's other contracts.

What a variant holds can be named through the arm's binder, and a caller's arm learns it
of the value it binds:

```
fn half(n: U64) -> Result<U64, U64>:
    ensures match result:
        Ok(h):
            h + h == n
        Err(code):
            code == n

fn rest(n: U64) -> U64:
    ensures true
    match half(n):
        Ok(h):
            return n - h        // no check: h + h == n, so h <= n
        Err(code):
            return 0
```

A `return Ok(value)` or `return Err(value)` built in place shows the arm's clauses of that
value; a callee's Result returned as it is carries the callee's facts (`return half(n)`).
An arm's facts reach only that arm of the caller (`tests/fixtures/scalar/caller-arm-*`,
`pass-on-arm-facts*`).

## Type invariants over plain fields

A sealed type may say what holds of its fields, and functions proved over numbers in its
file keep it (decisions.md; docs/proposal-scalar-invariants.md):

```
sealed type Account:
    balance: U64
    limit: U64
    invariant balance <= limit
```

- Every value of the type received (by value, `&` or `&mut`) satisfies the invariant: it
  is assumed, and no caller shows it. An account taken out of a `Vec` is deposited into
  with no proof at the call.
- The invariant is shown of each `&mut` one at every return, and of a returned one,
  `return Account(...)` or, for `-> Result<Account, E>`, `return Ok(Account(...))`.
- A value of the type is built only in such a `return`, and its fields are written only
  directly through a `&mut` parameter. Between those writes the invariant may be broken,
  so there the parameter is used only through its fields: not lent, read whole, copied or
  called a method on.
- What it buys: `deposit`'s `limit - balance` and `balance + amount` compile with no
  checks (`tests/fixtures/programs/ledger-invariant.wy`, `tests/fixtures/invariants/`).

## `memory.exchange`

`memory.exchange(&mut a, &mut b)` swaps what is known: every scalar part of `a` now
has the value the same part of `b` had, and the other way round. Parts nothing was
known about stay unknown. This is how `Vec.reserve_mut` is proved to keep the length
while the vector is held by value for `reserve` and put back.

## What a call brings in

- A scalar contract: its `requires` is an obligation at the call, its `ensures` facts
  after it.
- A memory or record contract (the Vec library's): the clauses that compare numbers,
  such as `output.length == old(input.length) + 1` or `code == 1`, become facts about
  the argument's fields before and after the call and about the result's. Clauses in
  an `Ok` or `Err` arm hold in the matching arm of a `match` on the result, a
  `let ... else`, or `?`. Clauses about memory (`memory.cell`, `memory.unchanged`) say
  nothing here.
- No contract: a value lent with `&mut` is unknown afterwards; everything else is as
  it was.

So `tests/fixtures/clients/vec.wy` proves, from Vec's contracts alone, that a loop
bounded by `items.len()` never takes `get`'s `Err` arm, that a failed second push is
undone, and that draining leaves `len() == 0`.

## What is shown

For a function with a contract, one goal, a conjunction of:

- every `+` and `*` stays in the range of its type, every `-` has `right <= left`,
  every `/` and `%` has a nonzero divisor (so the body cannot trap on arithmetic);
- each loop invariant on entry to its loop and again after each pass;
- every `ensures` at every `return`.

For any function at all (contract or not, any parameter types), one goal per call of a
function with a scalar contract: the callee's `requires`, from what is known on the way
to the call: enclosing `if` and `while` conditions, earlier assignments, earlier calls'
`ensures`.

Each of these is a sequent: the facts known at that point, then the claim. Facts that
share no name with the claim, directly or through other facts, are left out; that can
only make the statement harder to prove.

## What is assumed

- A callee's `ensures` after a call. Its own goal proves it; cycles among contracted
  functions are refused, so no proof leans on itself.
- After an arithmetic operation, that it was in range (it trapped otherwise).
- At a loop head, the invariants; after the loop, the invariants and the negated
  condition. Locals the body assigns are otherwise unknown at the head. This is partial
  correctness: nothing shows a loop ends (fuel bounds it at run time).
- A value lent with `&mut` to a call is unknown afterwards, apart from what the
  callee's contract says. Lent any other way, it is never followed again.
- A callee's memory or record contract is proved by its own model; the facts taken
  from it here are a reading of the same resolved clauses.

## What remains trusted

The walk in `scalar_vc.rs` and its reading of the typed body (that the statement
describes the code), including which places a store or a loan can change, the checker and emitter that turn the same body into Wasm, and the
Lean kernel and helper. The mathematics is Lean's: naturals, `omega`, `simp`.

## `break` and `continue` in a loop with invariants

A pass that ends early still ends with the invariants. `continue` goes back to the
head, so the invariants are shown where it stands, as they are at the end of the
body. `break` leaves the loop, and the invariants are shown where it stands too.

After a loop with no `break`, the invariants hold and the condition is false. After
a loop that `break` can leave, the invariants hold and nothing is said of the
condition: every way out has shown the invariants, and that is all the ways out
share. A claim that needs more is carried by an invariant, as in

```
var found = items.len()
for index in 0..items.len():
    invariant found <= items.len()
    ...
        found = index
        break
```

`tests/fixtures/scalar/jumps.wy` runs in Wasmtime: a search that breaks, a count
that continues, a loop with `continue` whose exit is still exact, a `break` in an
inner loop without invariants, and `while condition?:` with an invariant. Three
refusals: an invariant false at a `break`, one false at a `continue`, and
`result == n` claimed of a loop that may `break` at 5.

## `ensures true`: the arithmetic cannot trap

A function with a contract shows that every `+ - * / %` in its body stays in range.
`ensures true` asks for exactly that and nothing else. The library uses it where a
comment used to make the argument:

- `U64.hash` and `I64.hash`: the product of two factors below 2^32;
- `String.hash`: each step stays below 2^32, by an invariant;
- `String.parse_u64`: the guard before `value * 10 + digit`. Changing its `5` to a
  `6` is refused.
- `write_u64` and `write_u64_right` in the console library: a division by `scale`,
  which is never zero there. The first version's loop ran `while scale > 1`, and the
  proof refused it: nothing says `scale` is a power of ten, so `scale / 10` could
  be zero. The loop now runs `while scale >= 10`.

A method that only reads its arguments (every parameter is `&` or a number) and
never says `memory.` in its body or clauses is proved by this walk even in the file
of a sealed type, where the memory model used to claim it and could not follow
loops or `%`. A function that can change or consume a value keeps the memory model,
which is what covers a type's invariant.

`tests/fixtures/scalar/no-trap.wy` runs in Wasmtime; `no-trap-unbounded.refused.wy`
must not compile. Cost: two more goals in a program that uses a String map, about
60 ms on a first build and nothing measurable once cached.

## Limits

- Nonlinear claims (`result == a * b` with two variables) are stated correctly but the
  automatic prover is linear arithmetic; most are refused as not proved. A product of two
  bounded values is bounded (`a * b <= 10000` from `a <= 100` and `b <= 100`), including factors
  that are differences, quotients by a number, or products of bounded values (`b * (100 - w)`),
  so the overflow of such arithmetic is proved (`invariants/percent-blend.wy`). Division by a
  variable is reasoned about in one shape: a part scaled over its whole, `m * L / y <= L` where
  a fact (a `requires` or a conjunct of one) says `m <= y` or `m < y`
  (`scalar/part-over-whole.wy`). Any quotient or remainder by a variable is known to be at
  most its dividend, and a remainder below its divisor (`a / b <= a`, `i % n < n`), each fact
  proved by its core lemma (`scalar/variable-divisor.wy`).
- After a loop that `break` can leave, only the invariants are known, not that the
  condition is false.
- No quantifiers, no ghost variables. `old(...)` takes a method call on a parameter,
  a field, or `*value`.
- A clause cannot name an array element, or a field of a result that is a sum.
- Nothing is known about elements: `get` returning what `push` stored is the memory
  model's `memory.cell`, which these proofs do not read.
- A function whose contract speaks of memory cannot use `memory.exchange`.

## Controls

`tests/fixtures/clients/in-place.wy` runs in Wasmtime: a loop of `push_mut` whose
answer is the growth in length, a generic function proved, an exchange, and clauses
for each variant read by a caller. Eight `*.refused.wy` beside it must not compile:
a push that may fail claimed to add one, a successful push claimed to change nothing,
room claimed after a failed reserve, a false claim in a generic function, an exchange
claimed to keep its left side, a false `Ok` clause, a false `Err` clause, and a
result handed on from a callee whose contract says less.

`tests/fixtures/clients/vec.wy` runs in Wasmtime; nine `*.refused.wy` beside it must
not compile: a count claimed strictly below the length, a loop index with no bound, the
last index of a possibly empty Vec (in the function and at a call), a failed push not
undone, a wrong drain count, a `get` that may fail, a length claimed unchanged across
a helper that takes `&mut`, and a clause calling a method that computes.

`tests/fixtures/scalar/proved.wy` runs in Wasmtime. The twelve `*.refused.wy` beside it
are false claims that must not compile: a wrong `ensures`, overflow, subtraction below
zero, division by a possibly-zero value behind the wrong side of `||`, a missing, broken
or initially false invariant, a branch that is forgotten, an early return that breaks
the claim, a call whose `requires` does not follow inside a loop, a local changed
through `&mut`, and a wrong Bool result. Unit tests in `src/compiler/source_vc.rs` check the
shape of the generated statements.

## I64 values

An I64 name is an integer in the generated statement (`Int`), every other name
a natural number. The walk keeps the set of integer names; a comparison with
an integer on either side is written with integer literals, and a comparison
that would put an integer beside a natural number is refused, at the clause
when it is read and again when a statement is written.

`+ - *` on I64 are exact, with the obligation and afterwards the fact that the
result is between the least and greatest I64. `/` and `%` have the obligation
that they cannot trap and give a new name with only its range. Details and
tests: `docs/wasm-signed.md`.

## Proved slice reads

A slice read `bytes[index]` in a function with `proof: auto` loses its check when the path to
it already says `index < bytes.len()` in so many words (`if index >= bytes.len(): return`,
`while i < bytes.len():`): the slice's length is followed like a field of the local, and the read
becomes a goal. A read whose path does not say so asks nothing and keeps its check, so a
function never fails its proof for a read it did not try to prove; and the checks of a function
go only when every slice read in it was shown (`SLICE_PROVED`). Writes (`s[i] = v`) keep theirs.
`tests/fixtures/slices/proved-read*`.

A contract may name a slice parameter's length, `requires xs.len() > 0`: the same length the
reads follow, so `xs[0]` is shown from it. A caller shows the `requires` for the slice it passes
by name; the length of `a.slice()` of a fixed array is the array's, and a slice copied from another
keeps its length (`slices/requires-length*`). An index a callee promises below a slice's length
(`ensures result < s.len()`) is read in a proved caller with no test, as is one a loop invariant
keeps below it (`slices/index-from-contract.wy`: the whole program never traps).
