# RFC-0162 source SEC corpus

These fixtures are shared by the source-level regression tests and the exact
pre/post production-Binaryen corpus. Each test asserts an independent expected
result; backend agreement alone is not acceptance evidence.

| Finding | Historical trigger | Permanent source evidence |
|---|---|---|
| SEC-036, alias-initialized free-at-overwrite | `var rest = s` in string `rfind` let the first reassignment reclaim the still-live caller buffer; the TOML reproducer was the original landed regression in `e41f6d371` | `example_tests::rc_corpus::{rc_floor_toml_get_array_alias_init_is_sound_sec_036,alias_initialized_var_field_and_index_stay_live_sec_036}` and both `sec036-*.witchy` fixtures, including direct String and List paths |
| SEC-037, container-element RC use-after-free | minigrep retained each `contents.lines()` element in `hits`; incorrect RC handling at the element read/copy let later RC traffic corrupt or prematurely reclaim storage still observed through `hits` | `example_tests::examples_programs::minigrep_lines_survive_source_container_sec_037` and the unchanged `examples/minigrep/src/minigrep.witchy`; the production case enables UAF poisoning but not the separate known non-gating dup residual under `WITCHY_RC_ASSERT` documented by RFC-0051 |
| SEC-038, unchecked `bytes.at` | positive and negative out-of-object indices read adjacent linear memory | `example_tests::bytes::bytes_index_out_of_bounds_errors_on_both_backends` and the two `sec038-*.witchy` fixtures |
| SEC-039, literal free-at-overwrite | `var text = "abc"; text = text.trim()` put static data on the free list | `example_tests::region::rc_free_at_overwrite_does_not_free_a_literal_sec_039` and `sec039-literal-overwrite.witchy` |
| SEC-078, aliased call-result overwrite | an identity-like or copy-on-write call result, or a later alias assignment, was treated as fresh and reclaimable despite a retained caller alias | master commit `da098299a` landed three source-level interpreter/optimized-Wasm regressions; this track's exact production corpus pins the recursive dependency-call trigger, without claiming production cases for the two additional source-only variants |

SEC-036 and SEC-037 remain separate because their historical findings establish
different triggers and fixes: SEC-036 was free-at-overwrite of an alias-initialized
binding, while SEC-037 was missing/incorrect RC handling for a container element
that escaped into another container. The production corpus must therefore name
and execute them separately.

Forged-WIR rejections and encoder-template tests are intentionally not claimed
here; those belong to RFC-0162's validator and encoder tracks.
