---
verified: fc38bbb8
---

# Compiled Value Model

This is the shared map between the interpreter's semantic values and the
compiled backend's WebAssembly representation. The interpreter remains the
oracle for behavior; the compiled backend must represent the same values without
inventing observable differences.

## Universal Slot

Compiled Witchy carries ordinary values in an 8-byte slot. The WIR type
`Kind` says how a slot is viewed at a typed boundary:

| Witchy value | WIR kind | Compiled representation |
|---|---|---|
| `Int` | `I64` | signed `i64` |
| `Float` | `F64`/slot | IEEE `f64`, bit-reinterpreted through an `i64` slot when stored generically |
| `Bool` | `I32`/slot | `0` or `1`, widened to the slot when stored generically |
| `Nil` | `I32`/slot | zero sentinel |
| `String` | `I32` pointer | pointer to `[len: i32][utf8 bytes...]` |
| `Bytes` | `I32` pointer | same flat `[len: i32][bytes...]` layout as `String` |
| `List(T)` | `I32` pointer | pointer to `[count: i32][slot0][slot1]...` unless a declared `packed` confined list uses its flat record layout |
| tuple `(A, B, ...)` | `I32` pointer | pointer to `[arity: i32][slot0][slot1]...` |
| record / enum payload | `I32` pointer | pointer to `[tag/size word][slot fields...]`, with type tags checked where layout confusion is possible |
| function / closure | `I32` pointer or static function | capture-free functions are direct; capturing closures use an environment record plus function index |
| legacy host capability | `I32` handle | integer host handle, granted and checked by the wasmtime runtime |
| externref capability | `ExternRef` | opaque host reference; never stored in linear memory |
| cap-carrying aggregate | `GcRef(_)` | typed wasm GC struct reference once RFC-0005 stage 4 applies |

The canonical slot/kind definitions live in `crates/witchy-wir/src/wir.rs`.
Lowering converts with `to_slot`/`from_slot`; adding a value type means updating
those conversions and the interpreter in the same change.

## Heap Objects

Reference-counted linear-memory objects reserve two words before the user
pointer:

```text
ptr - 8: rc:i32
ptr - 4: size_and_tag:i32
ptr + 0: payload...
```

`size_and_tag & RC_SIZE_MASK` is the payload size. The high bits may carry the
layout/type tag used by checked unboxing and packed-record guards. The shared
constants and the tag hash live in `crates/witchy-wir/src/layout.rs`; runtime
and lowering import those facts instead of copying them.

When checked heap mode is enabled, allocation poisons
`[end, end + HEAP_REDZONE)` and the runtime sweep verifies that no compiled
helper wrote past the object. `HEAP_REDZONE` also lives in
`witchy_wir::layout`.

## Backend Responsibilities

| Stage | Responsibility |
|---|---|
| `witchy-interp` | Defines observable semantics over `Value`; no compiled layout assumptions should leak into user behavior. |
| `witchy-types` | Rejects programs whose values cannot be represented consistently, including caps inside unsupported slots and packed values used outside their layout contract. |
| `witchy-lower` | Chooses WIR kinds and layout helpers from static types; emits structural helpers per shape. |
| `witchy-wir` | Owns the IR data model, shared layout facts, helper library, and wasm encoding. |
| `witchy-runtime` | Instantiates compiled modules, grants host capabilities, checks heap redzones, and traps with shared diagnostic text. |

Any new semantic value, builtin, operator, host import, trap, or layout change
must preserve this table or update it in the same commit.
