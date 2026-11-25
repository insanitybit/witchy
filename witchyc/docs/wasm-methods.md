# Methods

```witchy
impl Counter:
    fn new(step: U64) -> Counter:
        return Counter(0, step)

    fn value(self: &Counter) -> U64:
        return (*self).count

    fn bump(self: &mut Counter):
        (*self).count = (*self).count + (*self).step

    fn finish(self: Counter) -> U64:
        return self.count * 10

var counter = Counter.new(3)
counter.bump()
console.print_int(counter.value())
console.print_int(counter.finish())
```

`tests/fixtures/methods/counter.wy` and `tests/fixtures/methods/vec.wy` run in Wasmtime.

## Rules

- An `impl` block names a declared record or sum type. `impl<T> Pair<T>:` covers every
  instance; a block for one instance (`impl Pair<U64>`) is refused. `impl<T: Any> Vec<T>:`
  makes no assumption that `T` can be copied or dropped.
- A method is the function `Type.method`. Nothing else is special about it: it can be
  generic, carry a contract and a proof, and live in a different file from the type. A sealed
  type's fields are still written only in the file that declares the type.
- `value.method(args)` looks the method up by the declared type of `value` and passes
  `value` as the first argument, in the way the first parameter asks:

  | First parameter | Receiver is passed as |
  |---|---|
  | `&Type` | `&value` |
  | `&mut Type` | `&mut value` |
  | `Type`, a Copy type | a copy |
  | `Type`, an owning type | `move value`; the name is gone afterwards |

  A receiver that is already a reference is passed as it is.
- `Type.function(args)` calls a function of the block that takes no receiver.
- A method that changes its receiver needs a named one. A method that only
  reads it takes any Copy value: `pair.swap().head()`, `Point(3, 4).sum()`,
  `42.show(out, allocator)`. An owner with no name is refused, since nothing
  could consume it afterwards: bind it with `let`.

## Not yet

The Vec library is written as methods: `names.push(&allocator, move name)`, `names.len()`,
`Vec.new(&allocator, 0)`. Its buffer helpers (`memory_*`) stay plain
functions because they are not operations on a declared type. The String library is the same:
`text.push_scalar(&allocator, 233)`, `text.byte_len()`, with `utf8_valid` and `utf8_push`
left as plain functions over `Vec<U8>`. Methods on anonymous records are not supported: they
have no declared name. A library may declare methods on a scalar (`42.show(...)`) and on
`Result`.

## Methods of one instance

```
impl Vec<U64>:
    fn sum(self: &Vec<U64>) -> U64: ...

impl Console<Write>:
    fn line(console: &Console<Write>, text: Text): ...
```

An impl block with no parameters of its own, naming one instance of a generic
type, gives that instance methods. Each is an ordinary function whose
parameters are of that instance, so calling it on another instance is refused
as a type mismatch (`tests/fixtures/methods/instance-other.refused.wy`). A
type's methods are still one namespace across its instances: two instances
cannot each have a method of one name (`instance-twice.refused.wy`), and a
trait is implemented for every instance or none (`instance-trait.refused.wy`).
Sealing is unchanged: such a block outside the type's file cannot reach its
fields (`instance-sealed.refused.wy`).

`Console<Write>` and `Console<Read>` are the two instances of the built-in
console type, and `library/wasm-console.wy` gives them their methods this way:
`console.line("…")`, `console.write_u64(n)`, `input.read_line(&allocator, &mut text)`.
The built-in `print_int`, `print_byte` and `next_byte` are read when no library
method of that name exists.
