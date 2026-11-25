# Module constants

```
let WIDTH = 16
let CELLS = WIDTH * WIDTH
let DEBUG: Bool = false
```

A top-level `let` declares a module constant, as in Witchy 1 (book:
tour-modules.md). It is evaluated when the program is parsed and every use of
the name in an expression is the value itself. There is no global state and no
run-time cost.

- The value is a `U64` or a `Bool`, made of literals, `+ - * / %`, comparisons,
  `&& || !` and constants declared above it. Overflow, going below zero and
  division by zero are refused, as in any arithmetic.
- A written type must match the value.
- A constant's name means the constant everywhere. Declaring a local, parameter,
  loop variable, match binder or function with the same name is refused, since
  every use of it would read as the constant.
- A `U64` constant is substituted as a literal, so like any literal it can stand
  where a narrower number is expected if its value fits there.

Implemented in `src/parser/constant.rs`: a pass over the tokens before parsing
lifts the top-level `let` lines out, so nothing after the parser changes.

## Compile-time function calls

```witchy
const fn bucket_mask(capacity: U64) -> U64:
    return capacity - 1

const SMALL_MASK: U64 = bucket_mask(16)
```

A module-scope `const` uses the same scope and visibility rules as a
module-scope `let`. Evaluation is required. An overflow, underflow or division
by zero refuses the build. Calling `bucket_mask(capacity)` in an ordinary
function still works with a runtime argument and uses checked arithmetic.

The initial const-function subset accepts `U64` and `Bool`, one explicit
`return` expression, scalar operators, and calls to earlier const functions
in the same file. Define a const function before the initializer that calls
it. Compile-time evaluation is bounded. Ordinary calls still go through
source checking and native or Wasm lowering.
