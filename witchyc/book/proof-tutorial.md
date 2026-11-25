# Prove a small function

Witchy's unsigned arithmetic is checked. To promise that adding one won't overflow, give the function a condition that excludes the largest input.

Save this as `next.wy`.

```witchy
import console

fn next(value: U64) -> U64:
    requires value < 10
    ensures result == value + 1
    proof:
        auto
    return value + 1

fn main(console: Console<Write>):
    console.print_int(next(4))
```

Use the [Wasm build and run commands](start.md) with `next.wy`. Expect `5`.

1. `requires` tells the body what it may assume. The caller must establish the condition. The literal `4` is smaller than ten.
2. `ensures` says exactly what the return value must be. The compiler generates obligations from the checked body, and Lean checks them. `auto` asks Witchy to discharge supported goals automatically.
3. Change the return expression to `value + 2` and rebuild. The program now violates its promise, so proof checking rejects it. Restore the body and try calling `next(10)` to see a caller-side failure instead.

A precondition is part of the API. If you want a function callers can use with every input, write a guard and return an error for inputs it cannot handle. The [contract guide](programs.md) shows how this works when a function returns a reference into a vector. In either case, read the trust boundary in the [proof reference](../docs/wasm-proofs.md).
