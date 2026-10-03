# Historical RFC recovery and authority map

The working RFC directory was consolidated on 2026-10-03. Git retains every
removed file. For any old RFC number or filename, find its last revision and
read the exact body:

```sh
git log --all -- rfcs/0134-codegen-and-compute-optimizations.md
git show c55ce5c41:rfcs/0134-codegen-and-compute-optimizations.md
```

`c55ce5c41` is the baseline immediately before this broad consolidation.
For documents retired earlier, use `git log --all -- <path>` to find an older
revision; the 2026-09-16 language consolidation baseline is `418a69b9`.
Retired RFC numbers are never reused. Existing source or test identifiers may
retain a retired number as a historical name.

| Historical topic | Current authority |
|---|---|
| Source syntax, ownership, views, resources, async, comptime, normal/opt boundary | [RFC-0167](0167-witchy-language-contract.md) and its [ledger](0167-acceptance-ledger.md) |
| Capabilities, effect grants, confinement, and delegation | [RFC-0126](0126-capability-effects-contract.md), current [capabilities spec](../spec/capabilities.md), and source/tests |
| SafeWIR validation, layouts, memory, ownership, canonical compiler route | [RFC-0165](0165-safewir-canonical-ir.md), [schema-1 judgment](0162-safewir-judgment.md), and the [ledger](0165-acceptance-ledger.md) |
| Authority-validated code generation | [RFC-0163 proposal](0163-authority-validated-codegen.md); no completion claim |
| Performance, compiler speed, benchmark roadmaps | [Current work](current-work.md), current profiles, and matched benchmark artifacts |
| Release, package manager, hosted Coven, Glamour, browser, agent workflow | Current source and product/operational docs; proposals remain parked until explicitly revived |

Old `implemented`, `PROVEN`, or benchmark labels describe their recorded
revision. They do not certify the current compiler, a wider language contract,
or a stronger safety theorem. Deleting a document does not cancel an obligation
preserved in a retained contract or current work map.
