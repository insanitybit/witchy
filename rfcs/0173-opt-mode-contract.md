---
rfc: 0173
title: "Deferred opt-mode and cross-mode contract"
status: deferred
created: 2026-10-03
tracking: "Design extracted from RFC-0167; no current implementation or release acceptance claim."
---

# RFC-0173: Deferred opt-mode and cross-mode contract

RFC-0167 owns normal-mode syntax, value semantics, `must` disposition,
immutable rooted views, and capability call meaning. This RFC owns the
additional opt-mode proof rules and cross-mode boundary. It is deferred until
normal-mode language and RFC-0126 capability grants settle. Existing opt paths
must preserve the shared RFC-0167 and RFC-0126 safety rules or refuse; this RFC
is not an optimizer switch or a current completion claim.

The deferred opt and cross-mode evidence rows remain in the
[RFC-0167 acceptance ledger](0167-acceptance-ledger.md) as historical and
future acceptance inputs. They do not gate the current normal-mode milestone.

## Normal and opt interoperation

This section specifies the deferred opt-mode design. Its cross-mode examples
are not acceptance gates for the current normal-mode compiler milestone.

`mode opt` is a module source contract, not an optimizer switch. Immutable view
syntax has the same meaning in both modes. Opt mode additionally rejects missing
proofs for requested ownership and no-hidden-copy contracts. It does not promise
every operation is allocation-free. Turning
optional optimization passes on or off cannot change accepted source semantics.

Normal code imports opt modules and calls their exports using the declared value
conventions. Immutable views may cross a source-linked boundary only through the
root-plus-projection carrier in the signature; they are never detached into an
owned value or erased from a nested type. A separately compiled ABI may reject
view-bearing exports until its authenticated metadata and carrier support them,
but it cannot silently change the signature.

```text
// fast.witchy
mode opt

pub fn set_first(var xs: unique List(Int)):
    xs[0] = 9

// app.witchy (normal)
import fast

fn demo() -> Int:
    var xs = [1, 2]
    let snapshot = xs
    fast.set_first(xs)
    snapshot[0] + xs[0]       // 10; snapshot is unchanged
```

The call protocol is:

1. Evaluate arguments once in written order and validate conventions.
2. On the normal caller side, establish callee preconditions. Reuse storage when
   proved safe; otherwise detach copyable storage. Never copy affine resources
   or authority-bearing resources to repair a contract. Existing immutable
   views already own their snapshot carrier and require no alias repair.
3. Enter the opt body with the same proven contract as an opt caller supplies.
   Its body has no hidden alias-repair fallback.
4. Return owned results and commit `var` write-backs on structured completion.
   Preserve snapshots, authority, consumption, and error semantics throughout.

An opt caller of the same export must supply the proof or explicit copy itself.
A failed proof is not a runtime trap or an automatic switch to normal semantics.
Generic instantiation, callbacks, trait witnesses, and function values preserve
these entry contracts. Copy preparation belongs to a normal call site, not a
second function identity that can leak through a function pointer.

Preparation must establish the preconditions of the actual operation, including
nested storage when it will be mutated. A unique outer shell is not proof of
unique children. Normal-side preparation cannot repair a consumed binding,
escaping call-scoped access, a live disposition obligation, missing callable
provenance, an authority violation, or an unsupported carrier/layout. None of
these failures authorizes an implicit await, cleanup effect, or mode switch.

Opt modules may depend on opt modules and authenticated standard-library
contracts. They do not import arbitrary normal user modules or invoke
unqualified normal callbacks whose hidden repair behavior would defeat their
contract. A normal higher-order caller can receive an opt function through a
convention-preserving interface; an opt higher-order caller
requires a checked opt-compatible callable contract. No new surface `opt fn`
spelling is introduced: provenance is retained in checked callable identity,
and erasing it cannot make a call admissible.

Specifically, a callable parameter declared in an opt module requires an
opt-compatible callable, including nested callable inputs/results. A normal
caller passing an arbitrary normal closure fails that precondition; copy
preparation cannot repair it. Exported signature metadata carries this rule so
separate compilation never relies on inspecting the callee's body.

The normal root does not disable an imported module's source checks. Conversely,
an opt dependency does not opt the normal root into stricter syntax. The runtime
carrier ABI must preserve these facts without relying on optional inlining,
specialization, ownership extraction, or bounds-check elimination.

## Normal code using opt-defined types

Execution mode belongs to the declared callable body, not to the module of a
value's nominal type. Importing an opt-defined type does not opt its normal
consumer into stricter source rules. It also does not erase qualifications,
stored obligations, authority, or representation requirements. Invoking an opt
method, constructor callable, or trait witness uses that callable's entry
contract; reading or moving admitted data does not itself switch modes.

| Imported value | Required semantics in normal code | Additional requirement at an opt call |
|---|---|---|
| Plain copyable record | Ordinary snapshot copying and lawful mutation; preserve nominal identity. | The callee's conventions and ownership preconditions. |
| Record with unique storage | Preserve affine handles; an independent data copy needs a lawful operation and its own proof. | Prove the root or child actually being mutated; outer uniqueness is insufficient. |
| Frozen payload | Preserve deep immutability; whole-value transfer or replacement remains legal. | No permission to mutate frozen payloads. |
| Must-bearing value | Preserve or discharge each live obligation on every structured exit. | A checked consuming/transfer operation; no resource copy as repair. |
| Eligible immutable view | Preserve its retained root and projection, including nested carriers. | The admitted signature and authenticated carrier; no hidden conversion to owned data. |
| Stored callback, task, or iterator | Preserve its actual invocation, lifecycle, and execution contract. | Authenticate the behavior used by the callee; the enclosing type's module is not evidence. |
| Sealed, packed, or existential value | Preserve construction authority, admitted layout, and actual witness identity. | Preserve those same facts through the supported ABI and selected callable. |

This table applies to admitted operations. An opt-only packed declaration does
not imply that every normal-side operation or external ABI supports that layout.
Likewise, an opt-defined record containing a function does not authenticate an
arbitrary function stored in it. Construction, updates, projection, generic
transport, and trait dispatch must preserve the actual callable proof or refuse
a use that requires it. Exported contracts must carry sufficient evidence without
depending on optional specialization or inspecting another module's body.

## Callback compatibility is an API contract

The transitive rule deliberately favors a compositional opt guarantee over
accepting every normal callback. Normal-side argument copying cannot establish
how a callback behaves when invoked. `pure` does not establish opt compatibility:
pure code may allocate and perform ownership repair. Trait implementations and
nested callable results preserve the same requirement as direct callbacks.

```text
// fast.witchy
mode opt
pub fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)
pub fn twice(x: Int) -> Int:
    x + x

// main.witchy, normal mode
import fast
fn ordinary(x: Int) -> Int:
    x + x

// Accepted target: the supplied callable has a checked opt contract.
fn accepted() -> Int:
    fast.apply(fast.twice, 3)

// Rejected: ordinary's identical arithmetic does not declare that contract.
fn rejected() -> Int:
    fast.apply(ordinary, 3)
```

Changing a public module to opt may strengthen its callback and trait-witness
requirements even when printed value types look unchanged. Treat that change
as API compatibility work, not merely a backend optimization. Exported metadata
and diagnostics must expose the requirement. Normal orchestration may call opt
kernels without becoming opt itself. Conditional callback-effect contracts or
an exception for ordinary callbacks require a separate language decision; this
amendment adds neither.

An opt factory is one way to supply compatible behavior, not a usability proof.
Its captures, dependencies, and returned callables must satisfy their contracts.
A normal factory returning an opt function or task is still a normal callable.
Moving input evaluation into a factory can also change capture time, effects,
and replay behavior; equal result types do not establish an equivalent API.

