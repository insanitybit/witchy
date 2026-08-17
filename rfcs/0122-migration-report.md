# RFC-0122 migration report

This report records the repository-source census used by acceptance row 21.
It is deliberately separate from the migrator's unit fixtures: a successful
fixture does not establish that the checked repository needs no rewrite.

## Scope

The census covers every tracked `*.witchy` source file on the report's parent
revision. It searches for retired direct-relation spellings:

```sh
rg -n "let\\('|var\\('|View\\(|String\\('[A-Za-z]" -g '*.witchy'
```

The corpus contains 307 source files. There are no `let('`, `var('`, or
`String('a)` direct-relation spellings. The remaining raw `View(` matches are:

- `AccessView(Int)` in `std/dynamic.witchy`, which is an unrelated constructor;
- comments in `std/borrow.witchy` and `projects/glamour/src/glamour.witchy`;
- `std/meta.witchy`, which renders the legacy spelling as reflection text.

They are not migrator inputs and therefore require no source rewrite.

## Command evidence

The migrator's executable coverage remains:

- `rfc0122_reference_migration_rewrites_proven_local_parameter_calls`;
- `rfc0122_reference_migration_rewrites_resolved_imported_calls`;
- `rfc0122_reference_migration_requires_overloads_to_agree_before_borrowing`.
- `migration_command_rewrites_then_checks_without_mutating_ambiguous_sources`.

The first two prove only authenticated direct places are borrowed. The third
proves conflicting overloads are reported rather than guessed. The command
fixture proves the executable `--check`/rewrite lifecycle and that unresolved
ownership is reported without mutating the source. Together with this corpus
census, no legacy source rewrite remains on this revision. Future legacy
spellings must rerun this report before row 21 can be reconsidered.
