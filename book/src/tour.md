# A Tour of the Language

witchy uses familiar expression-oriented syntax around an explicit capability
model. Readers of Python will recognize indentation-delimited blocks; readers of
Rust or ML-family languages will recognize algebraic data types, exhaustive
matching, traits, and `Result`.

- **Layout by indentation**, like Python. A block opens with `:` and a newline.
- **Expression-oriented**: `if`, `match`, and blocks have values.
- **Inference inside, annotations at the edges**: function parameters are
  annotated; locals are inferred.
- **No `null`, no exceptions**: absence is `Option`, failure is `Result`.
- **Immutable by default**: `let` binds, `var` is the opt-in for mutation.

## Reading source

A block begins after `:` and uses four spaces per indentation level. The
canonical formatter, `witchy fmt`, writes that layout. `//` starts a line
comment and `/* ... */` delimits a block comment. Function and variable names
conventionally use `lower_snake_case`; types and constructors use
`UpperCamelCase`. Lowercase names in type position, such as `a` in `List(a)`,
are generic type variables.

Source is UTF-8. Strings support `\n`, `\t`, `\r`, `\0`, `\\`, `\"`, and
`\$`, with `${expression}` for interpolation. The values chapter covers the
remaining literal forms.

The tour is organized as follows:

- [**Values and Types**](tour-values.md)
- [**Functions and Control Flow**](tour-functions.md)
- [**Data: Records and Enums**](tour-data.md)
- [**Uniform `var` Write-Back**](mutating-methods.md)
- [**Mutating Elements in a Loop: `for var`**](for-var.md)
- [**Errors as Values**](tour-errors.md)
- [**Generics and Traits**](tour-generics.md)
- [**Reflection**](tour-reflection.md)
- [**Generators and Iterators**](tour-iterators.md)
- [**Modules and Source Files**](tour-modules.md)
- [**Compile-Time Code**](tour-comptime.md)

Every `witchy` code block in the book is checked. Complete non-negative examples
are also executed through the browser host and receive a Run button when their
authority has an honest browser provider. Each click runs in a fresh
opaque-origin frame under grant-derived CSP. Partial declarations, deliberate
errors, and examples requiring native-only authority remain read-only and
include the grant needed to run them locally.
