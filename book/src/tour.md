# A Tour of the Language

`witchy` spends a fair bit of its complexity budget on its capabilities model. Elsewhere, it aims to be pretty approachable. There are a couple of quirks to the language but if you've used languages like Python, Typescript, or Rust, you'll likely have a handle on 80% or more of the language at a glance.

- **Layout by indentation**, like Python. A block opens with `:` and a newline.
- **Expression-oriented**: `if`, `match`, and blocks have values.
- **Inference inside, annotations at the edges**: function parameters are
  annotated; locals are inferred.
- **No `null`, no exceptions**: absence is `Option`, failure is `Result`.
- **Immutable by default**: `let` binds, `var` is the opt-in for mutation.

The sections:

- [**Values and Types**](tour-values.md)
- [**Functions and Control Flow**](tour-functions.md)
- [**Data: Records and Enums**](tour-data.md)
- [**Errors as Values**](tour-errors.md)
- [**Generics and Traits**](tour-generics.md)

You can run virtually all examples in the documentation in a browser sandbox or even locally - they can only write to the console and, presumably, if they try to do something else they should fail.
