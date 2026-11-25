# Generators: `gen fn` and `yield`

```
gen fn squares(limit: U64) -> U64:
    var n: U64 = 1
    while n <= limit:
        yield n * n
        n += 1

fn main(console: Console<Write>):
    for square in squares(4):
        console.print_int(square)
```

A `gen fn` writes a sequence as an ordinary loop (book: introduction.md). Calling it
runs nothing: `squares(4)` is a value holding the argument. `for` runs the body a step at
a time, each `yield` handing out one value, until the body ends or reaches `return` (at any depth: inside a `match`, a `for` or a loop
that does not yield, `generators/return-nested.wy`)
(`tests/fixtures/generators/squares.wy`: several yields, `break` and `continue` in a
loop that yields, a value kept between steps, `return` to stop, and stepping by hand).

## What it is

The parser turns a `gen fn` into ordinary declarations before anything is checked
(src/parser/generator.rs; design in docs/design-generators.md), so the checker, the proof
models and the code generator see nothing new:

- a sealed record named after the function (`squares` makes `Squares`), holding the
  arguments, the locals and the step it stopped at;
- `squares(limit)`, which builds one;
- `Squares.next_mut(&mut self, cursor: &mut U64) -> Result<U64, ()>`, which runs the body
  from where it stopped to the next `yield` (`Ok`) or to its end (`Err(())`, and every
  call after). The body is cut into steps at each `yield` and at each loop or branch
  around one; a step is one arm of a loop over the step number.

`for x in squares(4)` uses `next_mut` when the type has no `at` and no `next`; a
generator held in a `var` can also be stepped by hand with `next_mut`.

## What the first version takes

- Locals are numbers (F64 included: `f64/generator.wy`) or Bools. Yielded values may also be
  records without references (2026-11-18, `generators/yield-record.wy`); a value holding a
  reference is refused (`yield-view.refused.wy`). A `gen fn` may be generic, and a generator
  method of a generic impl takes the impl's type parameters: the record is generic over them
  (`generators/generic*.wy`); its fields are checked for authority as written, each type
  parameter read as `()` (`generic-capability.refused.wy`). Arguments may also be shared references
  and slices: `gen fn evens(items: &[U64]) -> U64` walks a container, and the generator then
  keeps what it borrows lent while it is in use (`generators/over-slice.wy`, `over-vec.wy`,
  `over-slice-changed.refused.wy`, `over-slice-escape.refused.wy`). What the record keeps
  holds no capability (`generators/*.refused.wy`). Arguments may be owners:
  `gen fn above(items: Vec<U64>, limit: U64)` holds the Vec, its steps borrow it, and
  `g.finish()` hands it back, done with or not, to be closed or passed on; with several,
  `finish` gives an anonymous record of them (`.{left, right}`). A generator holding one is
  itself an owner, so forgetting `finish` is refused (`owner.wy`, `owners.wy`,
  `owner-unfinished.refused.wy`). An owner *local* kept across a `yield` is refused with
  that advice: which owners are initialized at each `yield` is a flow fact, known only after
  checking, and this rewrite runs before; the typed state-machine lowering of
  docs/design-async.md (step 7) is where it belongs.
- A local's type is written (`var n: U64 = 1`) unless its value shows it: a whole number is a
  U64, a decimal an F64, a comparison a Bool, a parameter or earlier local its own type, and
  arithmetic the type of an operand that shows one, and a plain function's call its signature's
  result type (`generators/local-types.wy`, `closures/captures-calls.wy`); a generic call's
  result is written out. A name is used once.
- A `yield` sits in the body, in `while` loops, in counted `for i in a..b` loops (the
  counter and the end are kept between steps; the end is read once and `continue` still
  moves the counter on: `generators/counted.wy`), in `if` branches, and in `match`
  arms (`generators/match.wy`). An arm's binding is in scope for the arm's first steps
  and a leading `yield`; a use after that is refused by name, and the value is copied
  into a typed local instead (`match-binding-late.refused.wy`). A `yield` inside `for item
  in container` (a parameter or local, a field of one such as `self.items`, or `*name`) keeps the position between
  steps; `item` is like a `match` binding, so copy it into a typed local before a `yield` it
  must outlive (`generators/for-container.wy`, `for-container-item-late.refused.wy`);
  `for index, item in` keeps the index as a local too. An `if` that yields may test the
  element in its condition (it is evaluated before the step), and when each branch begins
  with its `yield` that value may name the element too: `if *x % 2 == 0: yield *x`
  (`generators/filter.wy`, `filter-field.wy`). One inside a `let ... else`
  or a value `match` is refused with what to write instead.
- No type parameters and no contracts.

Owner and reference arguments kept between steps are covered above.

An adversarial review found `break` and `continue` inside a `let ... else` arm, a
`using` body or a value `match` copied through untranslated, aimed at the generator's own
loop: wrong values or an early end, never unsafe memory. They are translated like an
`if` branch's now (`generators/continue-in-guard.wy`, `break-in-choose.wy`,
`break-outside.refused.wy`). A name bound by `let ... else` or a value `match` at the
level being cut is not kept between steps: using it after a `yield` is refused.

## Generator methods

```
impl Counter:
    gen fn upto(self: &Counter) -> U64:
        var i: U64 = 0
        while i < self.n:
            yield i
            i = i + self.step
```

A `gen fn` in an impl (not a trait's) is a method that makes the generator: `for x in
c.upto():` (`generators/method*.wy`). Its state type is named after the type and the method
(`CounterUpto`); the receiver is held in it like any parameter, so `self: Counter` copies or
takes the value and `self: &Counter` borrows it, which leaves it readable while the loop runs.
The result may be written `-> Iter<U64>`, the book's spelling of `-> U64`.

## A generator inside a generator

A `gen fn` whose parameters are all numbers or Bools makes a record of numbers, so another
`gen fn` may hold one as a local and step it with `for`:

```
gen fn fibonacci(limit: U64) -> U64:
    var a: U64 = 0
    var b: U64 = 1
    while a < limit:
        yield a
        let next = a + b
        a = b
        b = next

gen fn even_fibonacci(limit: U64) -> U64:
    var numbers = fibonacci(limit)
    for n in numbers:
        if n % 2 == 0:
            yield n
```

The inner generator is bound to a name first (`var numbers = ...`); the two functions may be
declared in either order. One that holds a reference (`gen fn items(v: &Vec<U64>)`)
is not a record of numbers and is refused as a local (`tests/fixtures/generators/compose*`).

## A `for` over a generator

`for item in g(arguments):` over a `gen fn` runs the generator's body in place: its locals are
the loop's, and each `yield value` is the loop's body with `item` bound to the value, so no record
is made and nothing is saved or restored between items. `break` in the loop's body stops the
generator, `continue` goes on to its next `yield`, and the generator's `return` ends the loop, as
stepping it would (src/compiler/inline_generator.rs; `tests/fixtures/generators/inline-flow.wy`).

A generator is stepped as before when it is generic, holds an owner, the loop is in a function
with a proof (its obligations would change), the loop has an index or invariants, or its body uses
something the loop's file may not: a private function or type of another file
(`inline-private.wy`). The result is checked as ordinary code, so nothing here is trusted.

A counting generator, 12M items: 27.0 to 4.1 ms. `text.chars()` over 12M characters: 71 to 49 ms.
