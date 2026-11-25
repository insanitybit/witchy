# Closures: `fn(n: U64) -> U64: n + by`

```
fn apply(f: fn(U64) -> U64, x: U64) -> U64:
    return f(x)

fn main(console: Console<Write>):
    let by: U64 = 10
    let add = fn(n: U64) -> U64: n + by
    console.print_int(apply(add, 5))                         // 15
    console.print_int(apply(fn(n: U64) -> U64: n * n, 6))    // 36
```

A `fn(params) -> R: expression`, or `fn(params) -> R:` with an indented block ending in
`return` (`closures/blocks.wy`), is a closure (book: tour-functions.md, "Closures"). It
captures the variables it names by value, when it is made: a later change to `by` does not
reach it (`closures/shapes.wy`). A parameter of a function type takes any closure of that
shape, and is called like a function (`closures/basics.wy`: one, two and no arguments,
nested calls, a closure bound to a local and called directly).

`import query` gives `Vec<T>` methods that take one: `values.count_where(fn(n: U64) ->
Bool: n > limit)`, `any` and `all` (`closures/vec.wy`), `map` (which may change the
element type) and `filter` (`closures/map-filter.wy`), and `count_ref` and `any_ref`, which
lend each element (`closures/lent-elements.wy`). They are a module of their
own so that the Vec library's proof does not grow for programs that do not use them.

## What it is

The parser turns closures into ordinary declarations (src/parser/closure.rs), so the
checker, the proof models and the code generator see nothing new:

- each closure is a sealed record holding its captures, and an `impl WitchyFn1<U64, U64>`
  (by arity) whose `call` copies the captures into locals and returns the body;
- `f: fn(U64) -> U64` makes the function generic, `f: F` with `F: WitchyFn1<U64, U64>`,
  so each closure passed gets its own copy of the function (no indirect calls);
- `f(x)` is `f.call(x)` where `f` is such a parameter or a local bound to a closure.

A trait method called through a bound takes the trait's arguments from that bound
(`src/compiler/check/inference.rs`, `infer_call`), which is what types `f(f(x))`.

## What the first version takes

- "Numbers" here include F64 (`f64/closure.wy`); a captured local's type is evident from a
  float literal, as from a declared type.
- A closure's arguments are numbers, Bools or shared references (`fn(w: &String) ->
  Bool`, `closures/lent-elements.wy` with `import query`'s `count_ref` and `any_ref` over
  a `Vec<String>`); its result is a number, a Bool or `()`, never a reference: through the
  call trait an argument and the result are different type parameters, so a reference
  result could outlive its loan (`lent-result.refused.wy`, a hand-written impl). Its
  captures are numbers, Bools, shared references or slices (a closure holding one is a view:
  it keeps the loan while in use and cannot outlive what it borrows,
  `capture-reference*.wy`), or capabilities (`owner-capture.refused.wy`); a function type in a signature may also name type
  parameters in scope (`fn(T) -> Bool` in a method of `Vec<T>`). A
  closure holds no owner, so nothing is closed through one; a reference it holds is a loan the
  checker follows like any view's.
- A closure's result type may be left out when its body shows it, since closures become
  records and traits before checking: a comparison, `!`, `&&`, `||` or a Bool is a Bool;
  arithmetic and bit operations have the type of an operand that is a parameter, a captured
  variable or a decimal number (`fn(n: U64): by * n + 1`); a block that returns nothing gives
  `()`. A bare integer (`fn(): 7`), a call, or a block ending in a value states it
  (`closures/inferred-*.wy`).
- What a closure captures has a declared type: a parameter,
  a typed `let`/`var`, or one whose value plainly says its type (a number, a decimal, a
  comparison, arithmetic of a typed name: `let by = 3`, `closures/captures-inferred.wy`; a call
of a plain function whose signature names a number, Bool or F64: `closures/captures-calls.wy`), `let shared =
  allocator.share()` (also on a clock or random source) and `let b = move a` or `let b = a` of a
  typed variable (`packages/httpd-app.wy`); anything else needs `let name: Type = ...`
  (`untyped-capture.refused.wy`).
- A function type is a parameter's or a result's own type, not inside `&` or another
  type. A function may return a closure (`fn adder(by: U64) -> fn(U64) -> U64`,
  `closures/returned.wy`) when every `return` gives the same closure literal: its result
  is then that closure's record, which callers call or pass on but cannot write or build.
  Returning one of two closures is refused (`returned-two.refused.wy`); each closure is
  its own type.
- Any local holding a closure is called like a function: the checker reads `add10(5)` as
  `add10.call(5)` when `add10` is a local (`src/compiler/check/methods.rs`, `method_call`).
- A closure of another shape is refused where one is expected (`wrong-shape.refused.wy`).
- What is captured is what is in scope where the closure is made: parameters, typed
  `let`/`var` bindings, a typed `let Ok(x): T = ... else` (`closures/guard-capture.wy`)
  and a counted loop's counter (`closures/scopes.wy`); a name a finished block bound is
  not. A `match` arm's or container `for`'s binding is not captured yet (refused; copy it
  into a typed `let`).
- `WitchyFn1` may be implemented by hand for a program's own type, which is then callable
  where a function type is expected, beside closures of the same shape
  (`closures/callable-record.wy`). A program does not declare its own `WitchyFn<n>`
  (`declare-call-trait.refused.wy`).

## Work run once: closures that take over what they capture

```
match race(&allocator, 2048, fn() -> (): listen(move reader), fn() -> (): nap(move clock)):
    Ok(winner): ...
```

A closure whose body moves something it captured (`move reader`) takes it over: owners
(a `Socket`, a `Vec`, a record holding them) and capabilities alike are moved into its
record, and it is run once. It implements `WitchyOnce0<R>`: `call_once(self)` takes the
record apart into locals of the captured names and runs the body; `discard(self)` gives up
what it took over without running it, calling each owner's own `discard` (`impl Discard`, or
a method of that name, as `Socket` has), so each owner it takes over must have one
(`once-no-discard.refused.wy`). It holds no reference in this first version. It may take
arguments: `fn(extra: U64) -> U64: total(move numbers, extra)` is `WitchyOnce1<U64, U64>`, called
`work.call_once(100)` (`once-argument.wy`); only one with none can be a task's work.

- Run as a task's work, such a closure first checks whether its task was told to stop before it
  began (`first_word()`); if so it discards what it took over and runs nothing, so a race given
  up or a shutdown does not start work it only means to stop. The check is in `start_task`,
  the task's function, not in `call_once`: called anywhere else, the closure runs
  (`tasks/cleanup-after-cancel.wy`). A task's work gives nothing back (`once-returns.refused.wy`).
- `Task.spawn(&allocator, bytes, closure)` runs one as a task: the task's function is the
  closure's own `call_once` (`once-task.wy`). If there is no memory, `Err(closure)` gives it
  back, unstarted; `closure.discard()` gives up what it holds.
- A type parameter bounded by `WitchyOnce0` (`W: Any + WitchyOnce0<()>`) is work: it may be
  given authority by value, which its bound shows, and `wasm-caps` reports it through the
  type argument (`race: Clock (in a closure) (through a type argument)`). Generic code may
  start it as a task; each instance runs that closure's own `call_once` (`once-generic.wy`).
- `race` and `both` in the tasks library (docs/wasm-tasks.md) run two such closures as tasks
  and handle a task that cannot be made: `net/timeout-race.wy` is `net/timeout.wy` with the
  31-line racing function down to 8.

## Callbacks that hold authority

```
fn run_steps(each: &fn(U64) -> (), count: U64):
    for i in 0..count:
        each(i)

fn main(console: Console<Write>):
    let log = fn(n: U64) -> (): console.print_int(n)
    run_steps(&log, 3)
```

A closure may capture a capability that needs no finishing (`Console`, `Dir`, `File<Read>`,
`Clock`, `Rand`, `Env`, `Net`, `Listener`, `Exec`, `SecretStore`): it takes it over, as
`move` does, and `main` no longer has it (`authority-moved.refused.wy`). Inside the closure
it is reached where it is held, never copied (book: capabilities-narrowing.md, "an
ordinary callback may exercise authority captured by its creator, while exposing only its
narrower callable interface to the receiver"). Design: docs/design-closure-authority.md.

- Such a closure is lent: the receiver takes `&fn(...)` and is given `&log`, so it can call
  the callback but neither copy nor keep it. Handing it over by value is refused
  (`authority-by-value.refused.wy`). A closure's result may be `()`.
- `wasm-caps` follows it: a generic function holds what its type arguments hold, so
  `run_steps` is reported with `Console<Write> (in a closure) (through a type argument)`,
  and `wasm-caps-diff` lists it as a change for `run_steps` (`closures/authority.wy`; unit
  test `a_callback_carries_its_makers_authority_to_the_receiver`). A trait's method reached
  with a given type reaches that type's own method, so a hand-written callable record is
  followed the same way.
- A `pure fn` takes no lent callback (`authority-pure.refused.wy`).
- A `File<Write>` (which must be finished) and owners are not captured
  (`owner-capture.refused.wy`).
