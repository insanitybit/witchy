# RFC-0087 migration report

Date: 2026-07-14

Baseline: `fa1ac5c8` (convention-bearing function types, before the uniform
write-back runtime and source cut)

This report records the one-cut migration required by RFC-0087. Source-text
counts are useful sizing evidence, but the acceptance authority is resolution
and type checking: the linked standard library, executable book examples,
examples, nested example projects, top-level projects, and benchmarks all pass
through the post-cut compiler.

## Declaration census

The baseline had 26 public `var`-receiver operations. Each was classified by
whether it denotes imperative mutation or a derived value:

| Family | Imperative after the cut | Pure after the cut |
| --- | --- | --- |
| `List` | `push`, `reverse`, `sort_by`, `sort`, `remove`, `set_at`, `update_at` | `concat` |
| `Dict` | `insert`, `update`, `remove` | `merge` |
| `Set` | `insert`, `remove` | none |
| `String` | none | `replace`, `to_upper`, `to_lower`, `trim`, `pad_left`, `pad_right`, `center`, `strip_prefix`, `strip_suffix`, `replace_first`, `trim_start`, `trim_end` |

The imperative operations no longer return their receiver. `List.push`,
`reverse`, `sort_by`, `sort`, `set_at`, and `update_at` return `Nil`;
`List.remove`, `Set.insert`, and `Set.remove` return `Bool`; and `Dict.insert`
and `Dict.remove` return the displaced `Option` value. `Dict.update` returns
`Nil`. `List.pop`, `List.pop_front`, and `List.swap` are new uniform `var`
operations. Pure operations have no `var` parameter.

The four PRNG operations were a separate state-threading class. `next`,
`next_below`, `next_bool`, and `choice` now take `var r: Rng`, return only their
ordinary result, and write the new generator state back through `r`.

## Call-site census

The pre-cut review found 386 self-reassignment candidates, about 20 mutator
calls nested in argument position, one executed temporary-receiver chain in the
book, 17 statement insertions whose auxiliary result is intentionally ignored,
and the 26 declaration decisions above. These source classes overlap, so they
are a sizing floor rather than totals to add together.

The final diff removes 231 directly recognizable self-rebind lines for the
`List`, `Dict`, and `Set` operations. Remaining affected calls were resolved and
migrated according to intent:

| Old use | Post-cut form |
| --- | --- |
| `xs = list.push(xs, x)` and equivalent method calls | statement call on the mutable place |
| derived-copy expression | explicit copy into a `var`, then statement mutation |
| temporary-receiver chain | named mutable local, then one statement per mutation |
| mutator nested in an argument | preceding mutation statement plus the updated place |
| tuple-threaded PRNG call | ordinary result binding; generator writes back |
| ignored `Dict`/`Set` auxiliary result | bare `var` call; write-back makes discard legal |

The migration changes executable source in 23 `std/` files, 37 `examples/`
files (including nested projects), 8 book files, 3 top-level project files, and
9 benchmark files. The generated `book/examples.json` manifest was regenerated
after the source cut.

Residual guards on the final tree find no tuple-threaded PRNG calls and no old
self-returning `List`/`Dict`/`Set` assignment form. A derived-copy use must now
be written explicitly; the compiler rejects a temporary or immutable place
passed to a `var` parameter. Bare non-`Nil` calls remain discard errors unless
their resolved declaration performs `var` write-back.

## Type-resolved verification

The executable corpus gate links the real standard library and compiles or runs
all book examples, standalone examples, and nested example projects on the
interpreter and compiled backend. The post-cut run covers 925 cases. Focused
RFC-0087 tests additionally resolve free calls, method calls, generic calls,
trait dispatch, indirect function values, nested field/index places,
caller-side and callee-side `?`, `??`, auxiliary-result discard, and locals live
across the shipped async segment lowering.

This compiler pass is the final accounting for affected calls: an old
self-returning use cannot type-check against the new declarations, and a
misclassified derived-copy or temporary use fails the writable-place check.

## Performance gate

Three kernel-clock samples after one warmup were compared on the same machine.
Lower is better.

| Benchmark | Baseline ns | Post-cut ns | Change |
| --- | ---: | ---: | ---: |
| `list_index` | 4,847,792 | 4,890,292 | +0.88% |
| `binary_trees` | 62,850,625 | 62,741,667 | -0.17% |
| `expr_eval` | 12,004,542 | 11,993,916 | -0.09% |

All three remain inside RFC-0051's 5% per-benchmark threshold. The former
memory-cliff kernels `word_count`, `dict_count`, `list_sum`, and `knucleotide`
all complete under the post-cut implementation. The write-back lowering is
therefore keyed into the existing ownership/in-place machinery rather than
silently falling back to whole-buffer copies.
