# BUG-539: `Bytes` absent from core render/reflect protocols

Status: FIXED (this commit)
Component: `std/bytes`, `std/show`, `std/reflect`, compiled `__render`

`Bytes` is now part of the ordinary core data surface:

- `std/show.witchy` implements `Show for Bytes` as `Bytes(len=N)`.
- `std/reflect.witchy` implements `Reflect for Bytes` as `MList` of byte `MInt`
  values, so `reflect.debug(bytes.from_string("hi"))` renders `[104, 105]` and
  `json.stringify` emits a JSON array of byte integers.
- the compiled backend distinguishes `Bytes` from generic pointer-shaped data for
  raw interpolation/`__render`, matching the interpreter's concise
  `Bytes(len=N)` fallback.

The regression
`bytes_are_showable_reflectable_and_renderable_on_both_backends` covers `show.say`,
`show.render([bytes])`, interpolation through `Show`, raw no-`show` interpolation,
derived `Reflect` containing `Bytes`, `reflect.debug`, and `json.stringify` on both
the interpreter and compiled WASM backend.
