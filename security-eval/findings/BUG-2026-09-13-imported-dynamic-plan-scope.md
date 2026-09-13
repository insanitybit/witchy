# Imported dynamic method plan loses its checked declaration

Status: confirmed fail-closed coverage regression; repair pending.
This finding does not demonstrate an admission or memory-safety bypass.

## Reproducer and evidence

On scope candidate `7469df90a36b7f6a148ae1fdfb0ad6d3408dc05c`, put these
two files in a workspace source directory with a normal `witchy.toml`:

`entry.witchy`:

```witchy
import library
fn main(console: Console):
    library.inspect(console)
```

`library.witchy`:

```witchy
import dynamic
import reflect

type Counter derive(Reflect):
    value: Int

@dynamic
pub fn add(self: Counter, amount: Int) -> Counter:
    Counter(self.value + amount)

pub fn inspect(console: Console):
    let value = dynamic.dynamic(Counter(4))
    console.print(dynamic.type_name(dynamic.type_of(value)))
```

The existing warmed binary reproduced the refusal in 0.64 seconds:

```sh
/private/tmp/witchy-rfc0165-net-half-close-target/debug/witchy check \
  /private/tmp/witchy-scope-metadata.RG6MC9/src/entry.witchy
```

Exit status: 1. Diagnostic: `type error: prepared dynamic method lost checked signature`.
The exact manifest, sources, and `result.log` are preserved in that directory.

## Cause and required repair

The narrowed `canonical_scope` stops treating imported declarations as roots.
The authenticated dynamic method plan still names `library.add`, but the
closed-dynamic preparation requires its checked declaration after pruning.
The existing dynamic-reflection example explicitly calls `dynamic.call`, which
introduces a method reference and does not exercise this metadata-only case.

Reconcile source-bound dynamic-plan dependencies with executable scope. Preserve
the checked-signature identity requirement, unused-import source checking,
ordinary callback reachability, and tail/closure obligations. Do not restore all
imported declarations as roots or exclude declarations merely because their
types are unsupported. An imported metadata-only reflection probe must admit;
the existing reachable-unsupported and unrelated-unused-declaration controls
must retain their respective refusal and admission outcomes.

## Landing state

The integration retained the scope patch as `c0189a178`. Landing candidate
`71ea5cf42b910a57975bb9191492dd7670f21d24` was intentionally stopped before
landing after independent review identified this case. Queue change
`mq-cef3c15092fec4c92765af7c5c0de5dec084c3b0`, attempt
`mqa-66321b919e5d47e207535467b6e06b0b7bfe1945`, records `red` at
`2026-09-13T04:04:27Z` because its own test process group was cancelled.
That event is not a failing workspace assertion or a completed validation run.
The branch and all incomplete project CLI changes remain preserved.
