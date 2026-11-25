# Design: interpolation as a value

Status: decided 2026-11-16; replaces the `.print("...")` rewrite of the same day, which
recognized a method by its name (a special case the user rejected, rightly).

## The feature

An interpolated literal, `"total ${t}, ready ${r}"`, is an expression anywhere. Its value
is a *format*: the literal's text pieces and its values, in order. It does not build any
text by itself, so making one needs no allocator. What consumes it decides:

- `console.print(&message)` writes the pieces to the console: no allocator;
- `text.push_format(&allocator, &message)` appends them to a `String`: the allocator is
  explicit, where the memory is.

Both are ordinary library functions over a library trait `Format`; nothing in the compiler
knows their names.

## How

The parser turns each interpolated literal into a sealed record of its values, with an
`impl Format` (the same way closures become records with a call trait). `Format` has one
method per kind of sink this first version has:

```
pub trait Format:
    fn write_to(self: &Self, console: &Console<Write>)
    fn push_onto(self: &Self, text: &mut String, allocator: &Allocator) -> Result<(), U64>
```

and each value's own `Show` (`show_on` for the console, `push_on` for a String) does the
work for its piece. A program's type joins by implementing `Show`.

A sink trait (`fn write_to<S: Sink>(self: &Self, sink: &mut S)`) would replace the two
methods with one; it needs a console sink that holds the console, which is the next step,
not this one.

## What a piece may be

`Show` and `Format` take `self` by value: a piece is a copied value or a view. Numbers,
Bools, text literals, a program's copied types implementing `Show`, and `Span`, the view of
a String: `"hello ${name.span()}"` (`console/print-span.wy`). A format holding a view keeps
the String lent while it is in use (`print-span-closed.refused.wy`). An owned `String` is
not a piece; its view is (`print-string.refused.wy`).

This needed one change to the type rules: a *shared view* (a record holding shared
references and no unique one, like `Span`) may be a generic record's type argument and a
generic function's copied type argument (semantic_types.rs `shared_view`). Buffers still
refuse it, so a view is never stored in a heap cell, and a `&` still never points at a value
that holds a reference: that rule keeps loans one level deep, which is why `Show` takes its
value by value rather than by `&`.
