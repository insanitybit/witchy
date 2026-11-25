# Bit operations

```
let low = x & 255
let flags = a | b
let toggled = a ^ mask
let inverted: U8 = ~byte
let high = x >> 32
```

`& | ^ ~ << >>` work on unsigned integers, U8 to U64 (`arithmetic/bits.wy`). `& | ^` take two
values of one type; `~` flips every bit of its type; `<<` and `>>` shift a value of any
unsigned type by a U64 amount, giving the value's type. I64, F64 and Bool have none
(`&&` and `||` are the Bool operators).

- Precedence, as the book has it: looser than `+ -`, tighter than the comparisons: shifts, then
  `&`, then `^`, then `|`. So `1 + 2 << 3` is 24 and `a & 4 == 4` is `(a & 4) == 4`.
- A shift by the type's width or more traps (`arithmetic/bits-shift-wide.traps.wy`); Wasm
  would shift by the amount modulo 64, which the original Witchy took, and Witchy 2 treats
  as the same kind of mistake as an overflow. Bits shifted out are lost, and a narrow type's
  `<<` and `~` keep only its own bits (`200 << 1` is 144 in a U8).
- In a proved function the result is an unknown value of its type, except that `a & b` is at
  most each side and `a >> n` at most `a`; a shift's amount must be shown below the width,
  as an overflow must be shown not to happen (`arithmetic/bits-shift-unproved.refused.wy`).
  Functions proved by the memory or record model do not use bit operations yet.
