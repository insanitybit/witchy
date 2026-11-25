# Fresh selected proof checks

This bridge erases authored proof work from runtime code and checks every selected
claim, including unused lemmas, with Lean 4.36.0. An executable's selected claims
are all of its own files' and packages', and every standard-library function it emits; the
library's others never run in it, are not sent, and are named in the report
(`unproved_unreached`). Scalar ghost syntax is described
below. Bounded [U64 source contracts](wasm-source-contracts.md),
[generic Init contracts](wasm-storage-contracts.md) and
[record body/caller contracts](wasm-vec-caller-proof.md) use the same fresh batch
and audit. Unsupported obligations refuse explicitly. All runtime functions,
types and ownership pass the trusted checker before any proof
tool is resolved. Every public compile, report, measurement, build and inspection
entry point enforces the same fresh proof check.

```witchy
lemma upper(x: U8):
    ensures x <= 255
    proof:
        auto

fn main():
    return
```

Parameters support U8, U16, U32, U64 and Bool. Unsigned ghost values are natural
numbers with upper-bound hypotheses supplied by the shared scalar descriptor.
Ghost `+` means total natural-number addition; ghost `-` means saturating natural
subtraction. These may differ from trapping runtime arithmetic: a freestanding
lemma asserts no runtime postcondition or overflow safety. Booleans, equality,
comparisons, negation, conjunction, disjunction and pure `requires` premises are
supported. Unsupported expressions, parameter types and proof steps refuse.
The sole initial proof step is `auto`, translated into a fixed kernel-producing
Lean tactic sequence. User identifiers are resolved and replaced with generated
identifiers; no raw user Lean or tactic text is accepted.

Zero obligations retain the existing receipt and module bytes, invoke no Lean,
resolve no Lean configuration and perform no proof identity hashing. Adding a
pure lemma changes proof records while leaving runtime Wasm unchanged.

## Trusted operator configuration

For proof-bearing source, the operator supplies the absolute installation root
in `WITCHY_WASM_LEAN_ROOT`. Its `bin/lean` is executed directly with an empty
environment, explicit installation `lib/lean`, kernel trust level zero, one thread, 2048 MiB Lean memory
limit and a ten-second checker deadline. There is no PATH/lake/SDK/bootstrap
lookup. The root, compiler and system dynamic loader are trusted operator
installation inputs, not authority supplied by source or an artifact. A malicious
replacement of the configured checker is outside this boundary; public hashes
cannot turn such a replacement into a trusted checker.

The entire installation is SHA256 snapshotted before and after checking,
including imported libraries, source and tool files. In-root links to regular
files bind both link text and their content-hashed targets. Links outside the
root, directory links and special files refuse. Limits are 100,000 entries,
depth 32 and 8 GiB content. This initial
implementation uses system CommonCrypto on macOS and refuses proof-bearing
source on unsupported platforms. Identity hashing is fresh, has no cache or
protected-store shortcut, and may dominate proof cost; it is not a speedup claim.
The operator must prevent concurrent malicious edits, including edit-and-restore
races, to the installation and compiler. System libraries/platform outside that
installation remain trusted and are not represented by its content hash.

The generated Lean module contains an exact proposition definition and theorem
for each selected lemma. A generated trusted audit verifies that every expected
name is a theorem with exactly its expected proposition type, checks the complete
nonempty inventory, rejects new axioms or uninventoried theorems, and recursively
collects theorem axioms. Only `propext`, `Classical.choice` and `Quot.sound` are
allowed; `sorryAx`, native-evaluation axioms and other assumptions refuse. The
compiler requires the request-bound audit marker and one concrete axiom inventory
per theorem, as well as successful Lean exit. Exit zero alone never suffices.

The `fresh_kernel_checks` receipt binds source, compiler, logical model, generated
module, audit and actual installation content. Obligations record expected theorem
names/statements and fresh outcomes; the audit records actual logical axioms.
This is fresh local checking, **not reusable library admission**, an installed
trusted store or complete consensus AdmissionRecord. Compiler translation, goal
completeness and Wasm emission remain trusted.

## Artifact rechecking and process bounds

The artifact API accepts a separate operator `leanRoot` option; its CLI accepts
`--lean-root ABSOLUTE_INSTALLATION`. Neither reads a tool path from packaged data.
Every run freshly recompiles the packaged source and compares exact Wasm and
receipt, including current proof identities. Self-rehashing a false proof,
swapping module bytes, or changing a proposition cannot bypass that check.
Source snapshots do not authenticate an author or a separately edited source file.

The artifact compiler runs in a fresh POSIX process group with a 30-second
deadline and a combined 64 KiB output limit. On termination the entire group is
killed and compiler completion awaited before scratch removal. The direct
compiler also kills and waits for its Lean child on checker timeout/output failure.
Generated proof code launches no subprocesses. These bounds assume a schedulable
trusted parent; whole-process availability and native RSS are not guaranteed.
Old output publication still occurs only after all source/proof checks succeed.
