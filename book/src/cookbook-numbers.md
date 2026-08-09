# Numbers and Math

Most languages quietly promote your `Int` to a `Float` when you mix them, and
most of the time that's fine. The times it isn't are the ones that hurt: money
that drifts a cent, a total that stops being exact somewhere past 2^53, a
comparison that fails for reasons you can't see in the source. witchy won't do
it. `1 + 2.0` is a type error.

There are two numeric types and they stay apart: `Int` is a 64-bit signed
integer, `Float` is a 64-bit IEEE double. You convert deliberately with
`math.to_float` and `math.to_int`. That's a few extra characters at the
boundary, and in exchange integer code stays exact and float code stays
predictable.

The `math` module holds the operations the operators don't: number theory,
integer roots, base conversion, and the float companions of each.

## Integer math

```witchy
import math

fn main(console: Console):
    console.print("gcd(48, 36) = ${math.gcd(48, 36)}")
    console.print("lcm(4, 6) = ${math.lcm(4, 6)}")
    console.print("2^10 = ${math.pow(2, 10)}")
    console.print("isqrt(50) = ${math.isqrt(50)}")
    console.print("17 prime? ${math.is_prime(17)}")
    console.print("clamp(120, 0, 100) = ${math.clamp(120, 0, 100)}")
    console.print("255 in hex = ${math.to_hex(255)}")
    console.print("13 in base 2 = ${math.to_binary(13)}")
```

```text
gcd(48, 36) = 12
lcm(4, 6) = 12
2^10 = 1024
isqrt(50) = 7
17 prime? true
clamp(120, 0, 100) = 100
255 in hex = ff
13 in base 2 = 1101
```

`isqrt` is the integer square root (floor), distinct from the float `sqrt`.
`to_hex`, `to_binary`, and the general `to_base` render an `Int` as a string in
another base - handy for hashing output and bit-twiddling. `pow` and `factorial`
work in exact integer arithmetic up to the 64-bit range.

## Floating-point math

Float operations mirror the integer ones with a `float_` prefix where a name
would otherwise clash (`float_min`, `float_abs`, `float_clamp`), and add the ones
that only make sense for reals (`sqrt`). Because float output would otherwise
differ in its last digits, format it explicitly with `format_float(x, decimals)`
rather than interpolating a raw `Float`:

```witchy
import math

fn main(console: Console):
    let x = math.sqrt(2.0)
    console.print("sqrt(2) ~ ${math.format_float(x, 4)}")
    console.print("to_float(7) / 2 = ${math.format_float(math.to_float(7) / 2.0, 2)}")
    console.print("float_max(3.5, 2.1) = ${math.format_float(math.float_max(3.5, 2.1), 1)}")
```

```text
sqrt(2) ~ 1.4142
to_float(7) / 2 = 3.50
float_max(3.5, 2.1) = 3.5
```

Note `math.to_float(7) / 2.0`: the `7` becomes a `Float` *before* the division,
so this is float division. Divide two `Int`s and you get integer division
(truncating toward zero); `math.ceil_div` and `math.round_div` give you the other
rounding modes without a detour through floats.

The rule of thumb: stay in `Int` for counts, indices, money-in-cents, and
anything that must be exact; move to `Float` only for genuine measurements, and
convert at the boundary on purpose.
