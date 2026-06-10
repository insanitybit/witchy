# A Tour of the Language

The capability story is what makes witchy *different*, but underneath it is an
ordinary, pleasant statically-typed language, and you need to be fluent in that
part before the security part feels natural. This chapter is the tour.

If you know a typed functional-ish language, most of this will be familiar with
a glance — skim the code, read the bits that surprise you. witchy's flavor:

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

A note you'll appreciate later: everything in this chapter is *pure* — the
examples take a `Console` only to show their results. None of this code can
touch the world, and that's the normal case. Effects are the exception, and they
get their own chapter.
