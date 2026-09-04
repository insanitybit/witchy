# Canonical lowering rejected non-string references without a String layout

## Status

Fixed on `impl/rfc0165-canonical-v1`.

## Reproducer

```witchy
fn main() -> Int:
    var xs = [1]
    xs.push(2)
    xs[1]
```

## Failure

Canonical lowering returned `MissingLayout(Named("String", []))` even though the
checked program contains no strings.

## Cause

The generic `Expr::Var` path attempted to classify every `Ref` value as a string
by resolving the module's canonical String layout with `?`. Modules containing a
non-string reference but no String layout therefore failed before the actual
reference type could be handled.

## Fix

String-specific extent bootstrapping now runs only when a String layout exists
and matches the value's layout. Failure to find an unrelated String layout no
longer rejects another reference type.

## Regression evidence

`normal_mode_list_push_reallocates_and_preserves_mutation_syntax` exercises the
production admission and execution path for a `List(Int)` module with no String
layout.
