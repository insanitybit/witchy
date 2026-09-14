# RFC-0165 implementation amendment: one memory model, checked opt boundaries

Decision date: 2026-09-04. This amendment records the user's agreed direction
after the RFC's external reviews. It supersedes conflicting migration choices,
not the independent-checker, proper-tail-call, or final-byte safety obligations.
Implementation and acceptance remain incomplete.

## One language, one value semantics

Normal Witchy and `opt` use the same observable value semantics. `opt` is a
checked interface to stronger resource and representation requirements, not a
second runtime memory model. It is useful to think of the boundary like FFI,
but both sides lower through the same SafeWIR and checked ABI.

Keep `let`, `var`, `own`, ordinary function calls, and mutation syntax such as
`xs.push(x)`. Callers do not write `xs = xs.push(x)`. Bounds are checked against
logical length, never spare capacity. Capacity growth policies need not agree
between modes or compiler versions unless the language explicitly promises it.

The baseline uses owned references with checked retain, transfer, and release.
It does not introduce a general tracing collector. Internal uniqueness remains
a useful property even if the source `unique` annotation is withdrawn.

## Scope of resource contracts

Every enforced requirement identifies whether it applies to argument
preparation, the callee's execution, returned values, or the entire caller.
Do not use an unqualified 'no copy' flag for all four meanings.

- A normal caller may prepare a value for a stricter callee, including making a
  private copy where value semantics permits. Preparation occurs before entry
  and does not constitute permission for the callee to violate its contract.
- An `opt` caller may perform that same preparation only if its own checked
  requirements permit it. Otherwise compilation rejects the call precisely.
- A promised representation or ownership precondition must be established by
  checked lowering, an appropriate runtime test and safe preparation, or a
  verified static fact. An annotation or function name never supplies it.
- A missing optimization fact retains the conservative operation or check.
  A missing intrinsic type, extent, ownership rule, or ABI contract rejects.
- Stronger analyses may eliminate copies, retains, and guards later. They may
  not silently redefine what an existing resource contract promises.

This does not allow hidden payload copies inside a promised no-copy operation.
Normal-mode adaptation is a separately identified part of the call. Existing
source restrictions remain enforced until their replacement is implemented;
there is no interval in which a guarantee is accepted and ignored.

## Initial support choices

| Surface | Initial contract |
|---|---|
| `let`, `var`, `own` | Preserve source behavior; lower to an owned reference ABI |
| Normal calling `opt` | Permit checked boundary preparation; no lifetime syntax required in normal code |
| Read-only string/collection views | Managed owner reference plus validated projection/range; no dangling interior pointer |
| Source `unique` | Retain only the authenticated outer `var unique` write-back convention; defer `own unique`, unique results, locals, and nested qualifiers with a fail-closed diagnostic |
| Internal uniqueness | Retain for safe runtime in-place mutation and later static optimization |
| Escaping mutable references | Deferred; reject rather than replace write-through semantics with copying |
| Region-backed public interfaces | Deferred; do not pretend an RC retain pins rewound region storage |
| Existing frontend lifetime checks | May remain as source restrictions; do not require a second backend borrow checker for owned read-only views |

Removing `unique` from a stdlib signature is not merely deleting a token. Each
affected API must state its replacement convention and preserve its ordinary
value behavior. Explicit no-copy/representation promises that cannot yet be
honored are unsupported, not optimization hints. Advanced-contract withdrawals
are recorded separately from accidental ordinary-language coverage failures.

## Owned ABI and lifetimes

Each memory-bearing parameter receives one owned reference token. The callee
must transfer it, store it, or release it on every normal exit. Non-tail callers
clone only when retaining their own token; tail callers transfer references
before the tail edge, with no residual caller cleanup. Proper tail calls remain
required language semantics, including with collection parameters.

Reference storage classes are explicit. RC operations never touch Bump,
Scratch, static data, or unknown storage as though an RC header existed.
Static data is immortal without header writes. A shared result must carry a
real owner/reference discipline, not just a `Shared` label.

A read-only view owns its root and a range checked at creation. Its release
targets the root, never the interior address. Mutable access must not invalidate
a live view: value-semantic copy-on-write or a checked exclusive operation is
required. A place is an ephemeral projection used while its owning reference
is live, not a storable unowned pointer.

Refcount-one authorizes mutation of that storage only. It says nothing about
the uniqueness of child objects. Nested mutation separately establishes the
required ownership along the modified path. Cloning a pointer collection
retains its children; moving slots transfers their tokens exactly once.

Destruction is layout-directed and releases initialized owned children.
Partial initialization, replaced slots, closure captures, managed views, and
mixed GC/RC containers require explicit accounting. GC does not release an RC
pointer merely because its bytes were stored in a GC field. Unsupported mixed
storage fails closed until its lifetime protocol is modeled.

The initial owned-value representation must not create strong reference cycles.
Recursive code uses callable identities rather than owning self-captures.
Self-insertion uses value-semantic copying/sharing of the prior value, not a
back-edge to mutable storage. Any future cyclic representation needs a stated
reclamation policy before admission; 'RC will handle it' is not one.

Host buffer lifetimes are part of authenticated host contracts. Same-instance
reentrancy requires pinned owners and cannot run while an unmodeled exclusive
access is active. Isolated worker instances are not same-instance reentrancy.
Host references to linear storage cannot outlive their retained owner tokens.
A host write is admitted only through an authenticated write-buffer role: the
slice lies inside the capacity of a unique, non-static object, the host receives
its address alone, and the guest publishes the written length afterwards under
its own capacity guard. The `float_to_str` render import is the one such import
projected today.

A `fn` value is a closure record: slot 0 is a `Callable` lane the checker types
only as the signature the module registered for that layout, the remaining slots
are `Int` captures, and the function in the slot takes the record as its shared
leading parameter. No function index enters or leaves linear memory under any
other type; a capability is never a capture; a call through a `fn` value is an
indirect call whose target set is every function of the registered signature.

## IR information and optimizations

SafeWIR carries actual types, layouts, projections, logical extents versus
capacity, ownership operations, arithmetic/trap semantics, call conventions,
tail obligations, and host roles. Derived ranges, aliases, escape facts,
uniqueness, and profitability live in analysis caches keyed by revision and
program point. Absence of a cache must not change program meaning.

Initial production usability does not depend on optional optimization passes.
Runtime checks and correct in-place RC branches are the safety/performance
floor. Later transformations emit candidates for independent rechecking;
semantic-preservation confidence is a separate obligation from memory safety.
Testing/fuzzing is evidence, not a universal proof of pass composition.

## Capability host calls: `Dir` and `Env` (design, slice 42)

Console is the only capability the canonical route carries today, and its one
operation (`print`) is a pure-infrastructure import over a read slice. `Dir`
and `Env` are the first authority-bearing operations, so they fix the shape
for every later capability. The design covers the four points a capability
needs beyond a print-shaped contract: host behavior, buffer ownership, scope,
and reentrancy.

**Kinds and tokens.** A capability is a `ValueType::Capability(kind)` token
whose kind is fixed by the authenticated catalog, never selected by the
candidate: `0` Console, `1` Dir, `2` Env. The token is an externref minted by
the host; it is never an integer, never lives in linear memory, and the checker
treats it as opaque (the standing constraint). Rights (`Dir[Read]`,
`Dir[Write]`) and scope (a subtree) live in the host's authority object behind
the externref, exactly as the frozen route's `DirAuthority` does; a violation is
a host trap, which is the interpreter's abort. Narrowing (`dir.subtree(p)`,
`dir as Dir[Read]`) is a host call whose result role is `Capability(1)`
(`HostResultRole::Capability`), so a narrowed token only ever comes from the
host and only from a token of the same kind. There is no `Mint` operation in the
module: the entry's `Dir`/`Env` parameters are minted by the launcher by grant
ordinal (`CanonicalEntryCapability::{Dir, Env}` beside `Console`; the runtime
mints from `dirs[i]` / the `Env` grant and refuses to launch without the grant,
as it does for Console).

**Host behavior and buffer ownership.** The frozen route stages a read in host
state (`dir_read_len` fills `pending`, the guest allocates, `fill_pending`
writes an unbounded staged payload at a guest pointer). That protocol is not
admissible here: it trusts the guest allocation, and it keeps host state across
two calls. The canonical imports are stateless and bounded, on the
`float_to_str` pattern: `dir_read_into(dir, path: Read Slice, out: Write
AtLeast(capacity)) -> Size` writes at most `capacity` bytes of the file and
returns the file's full length; `env_get_into(env, name: Read Slice, out: Write
AtLeast(capacity)) -> Size` does the same for a variable; `env_has_var(env,
name)` answers whether it is set (and allowed), and a read of an unset variable
traps. The guest allocates a unique, non-static buffer, calls, and publishes
`min(returned, capacity)` as the count after a guard; a length above the
capacity means reallocate to that length and call again, and the count is again
bounded by the second call's guard, never by the first call's answer (the file
may change between calls). `dir_exists(dir, path: Read Slice) -> Bool`,
`dir_write(dir, path: Read Slice, data: Read Slice)`, and
`dir_subtree(dir, path: Read Slice) -> Capability(1)` take checked slices; a
path is a string slice the memory checker re-establishes, not a length-prefixed
object address. The host never retains a guest pointer past the call and never
writes outside the slice it was handed. `dir.list()` is built guest-side from a
bounded fill of the NUL-joined names (`dir_list_into`), never a host-laid-out
list.

**Scope and reentrancy.** Every capability import is `HostReentrancy::Forbidden`
and `HostMutation::WriteBuffers` (reads) or `ReadOnly` (existence, write,
narrowing, which mutate the host, not guest memory). The import's authenticated
metadata must name the authority it exercises (`DirRead`, `DirWrite`,
`DirGrant`, `Env`); the projection refuses a capability import whose authorities
are empty or do not match its kind, the mirror image of the pure-infrastructure
rule that refuses an authority-bearing `print`. A call site passes the token as
`HostCallArgument::Capability` and the checker requires exactly the contract's
kind (`check/host.rs`); a scalar, a reference, or a token of another kind
rejects. Nothing in this slice claims RFC-0163's ghost-authority validation; it
claims the object-level boundary only, and the acceptance ledger keeps the
authority-flow judgment as a separate obligation.

**Oracle.** Capability, confinement, and ABI security take strict differential
coverage (RFC-0135), so the canonical interpreter serves these imports itself
rather than recording a debt: an `OracleFixture` (files, environment) with a
root-directory and process-environment fallback. `witchy parity`'s third leg
and the oracle example test point the fallback at the working directory the
backend was granted, so all three legs read the same files; the launch tests
seed a temporary directory and assert its expected contents independently.
Rights are not modeled by the oracle (a violation is a host trap on the
runtime leg), and the shared `witchy-test-host` fixture adapter is the
follow-up, not this slice.

**Source surface admitted by the slice.** `dir.read(path)`, `dir.list()`, `dir.exists(path)`,
`dir.write(path, data)`, `dir.subtree(path)`, `env.get_env(name)`, `Dir` and
`Env` as entry parameters and as ordinary parameters passed down (the token is a
scalar-like value: copied on pass, no release), and `dir as Dir[Read]`. The
census targets are `files`, `minigrep`, `caps_audit`, `caps_guard`,
`capability_rights`, `coven_check`, and `branded_caps`.

## Implementation order and evidence

1. Repair recorded checker, owned-ABI, source-binding, and executable-oracle
   defects with focused positive and adversarial tests.
2. Establish central ownership insertion and layout-driven destruction, including
   pointer collections and initialized-prefix handling. Stop scattering cleanup
   guesses across source constructs.
3. Exercise owned calls and tails, managed views, normal-to-opt preparation,
   opt-to-opt rejection, and nested mutation through the same core.
4. Migrate remaining ordinary constructs by composition of those primitives.
   Retire obsolete loan/contract reconstruction paths rather than extending them.
5. Demonstrate one useful analysis-driven optimization and its near misses.
6. Complete supported-corpus admission/execution evidence and independent
   security review, then perform the coordinated production cutover and gate.

`Bytes` is the same raw-byte buffer layout as `String` under its own source
type, so every extent, ownership, copy, and release rule for strings applies
unchanged; the two differ only in the operations lowering admits on them. The
`Bytes` intrinsics are byte kernels over that buffer, with the interpreter's
rules as the expected result: an index or a byte value out of range aborts,
`slice` clamps both ends, and `to_string` decodes UTF-8 lossily (each maximal
invalid prefix becomes one U+FFFD, the `from_utf8_lossy` contract). A raw-byte
element write is bounded by the buffer's capacity, so a growing kernel guards
its stores against the capacity lane it allocated, not the count it will
publish.

No new SafeWIR completion claim follows from the frozen legacy route. Before
cutover, canonical lowering remains the development route, with no per-program
fallback disguising its coverage gaps. After atomic cutover, production bytes
require validated SafeWIR authority and exact-byte admission exclusively.

Compiler-wide memory safety is not established by this amendment, a successful
corpus, an assurance record, or a type wrapper. Acceptance needs evidence about
the checker, backend correspondence, runtime lifetime behavior, host boundary,
and enabled transformations under the explicitly stated frontend assumptions.
