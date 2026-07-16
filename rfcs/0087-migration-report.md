# RFC-0087 migration report

Date: 2026-07-16

Historical source-cut baseline: `fa1ac5c8` (convention-bearing function types,
before the uniform write-back runtime and source cut).

Current verification baseline: canonical `master` at `e68a0ed5`.

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

Embedded Witchy fixtures and generators in the Rust integration suite were
migrated in the same cut. Derived-copy fuzz expressions use small pure wrappers
that copy into a local and invoke the uniform `var` operation, so the generated
programs remain valid while continuing to exercise mutation and both backends.

Residual guards on the final tree find no tuple-threaded PRNG calls and no old
self-returning `List`/`Dict`/`Set` assignment form. A derived-copy use must now
be written explicitly; the compiler rejects a temporary or immutable place
passed to a `var` parameter. Bare non-`Nil` calls remain discard errors unless
their resolved declaration performs `var` write-back.

## Type-resolved verification

`cargo run --bin rfc0087-census -- .` is the durable compiler census. It parses,
links, lowers methods/traits to their exact callee declarations, reads `var`
conventions from those declarations, and type-checks 271 Witchy sources plus
170 Witchy blocks from the README, spec, and book. Its complete stable output is
checked in at [`0087-migration-census.tsv`](0087-migration-census.tsv) and
freshness-tested by `tests/rfc0087_migration_census.rs`.

The current resolved totals are 25 entry-source `var` declarations and 471
lowered `var` call instances inspected. The obsolete migration-error classes
are empty: zero mechanical self-reassignments, zero immutable arguments passed
to `var`, and zero temporary `var` arguments. Nine expression-position calls
are intentional uses of the independent result (PRNG steps, extraction, and
the teaching examples), and 14 statement-position calls intentionally discard
an auxiliary result. The snapshot lists every such judgment by source line and
resolved callee; no regex or method-name allowlist contributes to the counts.

Two top-level project entries fail before RFC-0087 checking on the already-known
RFC-0005 representation boundary: `projects/coven/src/coven.witchy` and
`projects/coven-web/src/coven_web.witchy` capture a `Dir`-carrying value in a
closure. The exact compiler diagnostics are retained in the snapshot as
externally owned evidence. The census still lowers and classifies their direct
`var` calls, and neither contains an RFC-0087 migration-error finding. This
slice does not modify representation or closure-ABI files.

The executable corpus gate separately links the real standard library and
compiles or runs book examples, standalone examples, and nested example
projects on the interpreter and compiled backend. Focused RFC-0087 tests also
resolve free calls, method calls, generic calls, trait dispatch, indirect
function values, nested field/index places, caller-side and callee-side `?`,
`??`, auxiliary-result discard, and locals live across the shipped async
segment lowering.

## Performance gate

Current master carries the dedicated seven-kernel
[`rfc0087_inplace_gate.sh`](../benchmarks/rfc0087_inplace_gate.sh) and frozen
reference used by
[`0087-performance-report.md`](0087-performance-report.md). It compares the
shipping optimized configuration with the supported `WITCHY_OPT=-inplace`
forced-copy oracle for `word_count`, `dict_count`, `list_sum`, `knucleotide`,
`list_index`, `binary_trees`, and `expr_eval`.

The locked best-of-three evidence proves that all optimized kernels complete,
the four memory-cliff kernels fail only under forced-copy, the three numeric
kernels retain a material in-place advantage, and their optimized timings pass
the RFC-0051 5% non-regression threshold. The harness, reference, and report
landed through the serialized full gate in batch commit `5974a032`; no
per-operation fast path or new `*_cap` helper was added.

## Current validation contract

The checked-in census is not a prose-only claim. Its integration test reruns
the complete repository scan and requires byte-for-byte equality with
`0087-migration-census.tsv`. `scripts/test-for-paths.sh` selects the fast
workspace shard for this Rust-backed tool, so the slice must also pass current
workspace tests and clippy before queue submission. The merge coordinator then
runs the serialized full gate before landing.

The two retained RFC-0005 representation-boundary rejections are explicit
snapshot rows, not skipped files. All 439 corpus entries are parsed and linked
far enough to classify their resolved `var` calls, and every RFC-0087
migration-error category is required to remain at zero.
