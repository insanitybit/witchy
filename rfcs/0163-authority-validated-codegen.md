---
rfc: 0163
title: "Zero-cost authority-validated code generation"
status: proposed
created: 2026-08-27
superseded-by:
related:
  - "0126 (Capabilities and explicit effects as a core language contract)"
  - "0157 (Trusted publishers and executable state)"
  - "0158 (macOS Seatbelt confinement provider)"
  - "0162 (Validated SafeWIR for zero-cost object-memory safety)"
tracking: "Companion proposal to RFC-0162. No implementation or release-safety claim is made until every acceptance row is complete."
---

# RFC-0163: Zero-cost authority-validated code generation

## Summary

RFC-0162 prevents optimized generated code from using Witchy object memory
unsafely. That is necessary, but it is not the complete dependency-sandbox
claim. A compiler defect could still synthesize a well-formed host call for a
zero-representation capability, an encoder could emit a different security-
relevant instruction than the validated WIR requested, or a binary optimizer
could change the bytes after SafeWIR facts were erased.

This RFC adds the other static admission layers required for the claim:

1. an independent, capability-relevant source judgment establishes the only
   authority roots available to each function;
2. final WIR carries ghost authority identities, rights, and provenance through
   calls, closures, aggregates, control flow, and host operations;
3. one combined validator admits only WIR that satisfies both RFC-0162 object
   safety and authority non-amplification;
4. a small proof-checked final-byte admission kernel confirms that the encoder
   emitted the admitted memory operations, references, imports, and host calls;
5. no unvalidated binary transformation runs after that check;
6. adversarial source compilation occurs in a capability-empty worker and its
   output re-enters through the same independent admission boundary; and
7. artifact, cache, launch, and outer-confinement policies bind the exact
   admitted bytes to the exact grants with which they may execute.

The ghost authority graph, proof objects, and admission records exist only
during compilation or artifact admission. They add no instruction, branch,
load, store, import, global, table entry, memory byte, tag, or capability handle
to a production Witchy module.

## Relationship to RFC-0162

RFC-0162 remains the object-memory judgment. This RFC does not duplicate its
layout, range, lifetime, alias, ownership, or generation rules. It composes
with them.

The boundary is:

- RFC-0162 answers, "Can this final operation access this live object in this
  way?"
- RFC-0163 answers, "Can this final function exercise this host authority, did
  it obtain that authority from an explicit grant, and do the executable bytes
  still contain only the operations that were admitted?"

Neither answer substitutes for the other. SEC-080 used an RFC-0162 violation to
manufacture a zero-representation `Console` value. SafeWIR blocks that exact
memory-corruption route, but a separate compiler bug that directly creates a
`console.print` host call would not necessarily violate an object-memory rule.
Authority provenance must therefore be checked independently at the same final
boundary.

RFC-0162 currently permits a pinned Binaryen executable to remain in the
trusted computing base. This RFC narrows the release claim: an executable may
be called authority-validated only when its exact final bytes have passed the
admission kernel. A pinned postprocessor is reproducible, not proved correct.

## Motivation

### A module import is not a function's authority

Witchy links publisher and dependency functions into one Wasm module. Publisher
code may legitimately cause `console.print`, `dir.read`, or another host import
to exist in that module. The import's existence cannot authorize every linked
function to call it.

This distinction matters most for capabilities whose receiver has no guest
runtime representation. `Console`, `SecretStore`, build capabilities, and
other source-typed or import-granted authorities may compile to no Wasm operand
at the call site. Source typing says that the receiver exists, but the emitted
host call by itself cannot demonstrate where it came from.

A malicious dependency with this signature:

```witchy
pub fn slice(text: String) -> String:
    text
```

has no authority even if another function in the linked module receives a
`Console`. Likewise:

```witchy
pub fn read_file(file: File[Read]) -> String:
    file.read()
```

may use only the specific file and rights passed to it. A compiler defect must
not turn either function into a caller of a module-wide `Console` import.

### An unsound source type claim must not bless final code

SafeWIR intentionally distrusts optimizer facts. The same rule must apply to
capability types and effects. Copying `Console` into an annotation, assigning a
host operation to the wrong source expression, confusing generic
instantiations, or attaching an overly broad function effect cannot create
authority merely because the ordinary type checker produced that metadata.

The final authority checker does not need to prove every source semantic rule.
It does need an independent derivation of every fact whose falsity could widen
host authority or reinterpret a runtime representation.

### Validation must cover the bytes that execute

The SafeWIR validator runs before encoding. A bug in a trusted encoder can
change an address, width, reference kind, host import, or argument order.
Binaryen currently runs after encoding in some production paths and may rewrite
the same operations after the ghost facts are gone.

Standard Wasm validation catches malformed stack and reference typing. It does
not know Witchy's object boundaries or which source-level authority permitted a
host call. Pinning, differential tests, and fuzzing reduce risk but do not
establish the universal admission claim.

### Malicious source also attacks the compiler process

The dependency source is adversarial input to parsers, type checking,
specialization, optimization, diagnostics, and caches. A memory-safe generated
module does not help if processing that source compromises a compiler process
that holds developer credentials, ambient filesystem access, network access,
or executable-state write authority.

Compiler memory safety and process confinement are distinct mitigations. This
RFC requires both for the authority-validated build profile.

### Current baseline

At proposal time, current master has substantial RFC-0162 scaffolding but no
authority-validated release path:

- `safewir_validate::HostArgument` distinguishes scalar values from checked
  object buffers, but it has no authority identity or rights provenance;
- `validate_final_wir_report` is a report/candidate API rather than a mandatory
  production encoder gate;
- the RFC-0162 validator deliberately rejects unchecked `ProvenIndex` until its
  final-CFG dominance authority is implemented;
- `Console`, `Clock`, and `Rand` have zero guest representation, while
  `SecretStore` is root authority without a guest-held handle; their safety
  therefore depends on source typing and import/link grants outside SafeWIR's
  object judgment;
- Binaryen may still transform Wasm after SafeWIR facts would be erased; and
- RFC-0162's acceptance ledger remains pending and explicitly says report-only
  validation creates no security claim.

These are implementation facts, not criticisms of the staged RFC-0162 work.
They identify the interfaces this companion must close before promotion.

## Goals

1. Ensure that every final host operation is authorized by an explicit root or
   a monotone derivation from one, including zero-representation capabilities.
2. Detect capability/type confusion independently of the ordinary type checker
   and independently of optimizer-supplied facts.
3. Compose authority validation with RFC-0162 so neither memory corruption nor
   well-typed host-call synthesis can widen a dependency's authority.
4. Validate the exact security-relevant instructions in the bytes that execute.
5. Remove unvalidated postprocessing from the authority-validated release path.
6. Contain compiler-process compromise while accepting adversarial dependency
   source.
7. Add no runtime work or representation to generated Witchy code.
8. Preserve optimizer freedom before the final validation boundary.
9. Make every residual trusted component and outer-confinement assumption
   explicit in the release claim.

## Non-goals

- Proving complete source-to-Wasm semantic equivalence. An authority-safe
  program may still compute a wrong ordinary value.
- Proving the entire Witchy parser, type checker, optimizer, or linker correct.
  Their candidate output is checked at narrower security boundaries.
- Proving Wasmtime, a browser engine, the operating system, firmware, or
  hardware correct.
- Protecting arbitrary third-party Wasm, native extensions, or AOT native code
  that did not pass this admission pipeline.
- Authenticating executable state against an actor already granted authority
  to overwrite it. RFC-0157 owns that trust decision.
- Eliminating denial of service. Compiler, validation, memory, fuel, recursion,
  and host-operation limits remain mandatory, but exhausting a declared limit
  may reject or terminate execution.
- Adding dynamic capability checks, shadow stacks, tags, pointer capabilities,
  or per-operation monitors to production Witchy binaries.

## Threat model and claims

### Attacker

The attacker controls a Witchy dependency's complete source and ordinary input.
The dependency may choose types, generics, closures, reflection, async shapes,
allocation patterns, control flow, and optimization triggers. It may be linked
with trusted publisher code that holds powerful capabilities. It may receive no
capability, or it may receive one narrowly scoped benign capability such as a
read-only `File` for a JSON configuration.

The publisher, selected root grants, Witchy toolchain distribution, admission
kernel, and configured platform-confinement provider are trusted. An actor with
publisher or executable-state write authority is governed by RFC-0157 rather
than treated as a sandboxed dependency.

### Static authority claim

For every execution of an authority-validated artifact:

- every host operation in its possible trace is authorized by an authority
  identity derived from an explicit entry grant or explicit delegated
  parameter;
- the operation's kind, rights, scope, target, and target availability are no
  broader than that authority;
- no function gains authority merely because another function caused the same
  host import to be linked;
- no scalar, object-memory value, tag, generic instantiation, reflection value,
  closure environment, indirect call, or control-flow join can mint authority;
- a capability result is created only by a catalogued derivation whose input
  authority permits it, and refinement never widens rights or scope;
- every host buffer or reference operand also satisfies RFC-0162's live-object,
  layout, bounds, ownership, and reference-separation judgment; and
- the exact executable bytes contain no unmatched security-relevant operation.

This is an authority non-amplification claim. It does not assert that every
authorized operation has the intended business meaning or that every pure
calculation is correct.

### Compiler-input containment claim

Adversarial source compilation for this profile runs without developer or host
runtime authority. A compromised worker can crash, consume its bounded
resources, or emit arbitrary candidate data. It cannot directly read ambient
secrets, access the network, execute tools, modify the source tree, or publish
an executable. Candidate output remains untrusted until independently parsed
and admitted by the parent.

### Outer-confinement claim

`witchy sandbox --confine=required` adds the RFC-0103/RFC-0158 platform boundary
around the admitted runtime. That boundary is defense in depth against a
Wasmtime or host-handler defect. Authority validation alone assumes those
components execute their contracts correctly; the stronger confined claim
requires a provider that reports complete enforcement for the selected grants.

## Design

### 1. One final executable admission type

The production encoder and artifact publisher accept only a private wrapper:

```text
AdmittedWirModule {
    source_authority: CheckedAuthorityProgram,
    object_safety: ValidatedWirModule,
    authority_safety: ValidatedAuthorityWir,
    rule_schema: AdmissionSchemaId,
}
```

The final-byte checker returns:

```text
AdmittedExecutable {
    wasm_digest: WasmDigest,
    authority_contract_digest: AuthorityContractDigest,
    admission_schema: AdmissionSchemaId,
    toolchain_identity: ToolchainIdentity,
    postprocessor: None,
}
```

Both constructors are private to the admission kernel. Diagnostic reports,
optimizer annotations, cached type information, serialized certificates, and
ordinary `WirModule` values cannot be converted into either wrapper.

The wrapper is proof of a successful local check, not bearer authority for the
guest. It is never encoded into guest memory or made visible to Witchy source.

### 2. Independent capability-relevant source judgment

The ordinary parser and type checker remain responsible for language
diagnostics and full typing. A smaller `authority_source_check` independently
rechecks the closed program properties on which sandbox authority depends.

Its authoritative input is the exact bounded source bundle, module graph, and
entry-grant declaration supplied to the compilation worker. It parses and
resolves the authority-relevant source projection independently. A worker AST,
resolved symbol graph, typed annotation, inferred effect, or source-operation
map is only an untrusted search aid. The checker independently derives:

- the nominal identity and transitive capability content of every parameter,
  local, result, aggregate, closure capture, async frame, and dynamic boundary;
- exact capability kind, rights, scope, target availability, and refinement;
- function, closure, indirect-call, task, and host-operation effect contracts;
- call argument/result compatibility for every capability-bearing value;
- the only root-authority introductions at entrypoints and checked host
  bindings; and
- every allowed derivation from a root capability to a narrower capability.

It need not infer ordinary arithmetic types twice unless an arithmetic fact is
used in an authority judgment. It must parse and type every form that may
contain, carry, select, capture, return, delegate, or exercise authority. An
unknown generic, reflection, `Dynamic`, native, raw, or indirect form fails
closed.

The authority-relevant grammar, name-resolution rules, and typing judgments
are part of the Lean admission model. This deliberately makes a parser or
resolver bug an untrusted proposal defect rather than a way to change the
source program being authorized. The checker reports the original source span
for diagnostics, but a span is never proof that two operations correspond.

The result is a canonical closed-world graph:

```text
CheckedAuthorityProgram {
    entries: Map<FunctionId, EntryGrantContract>,
    functions: Map<FunctionId, AuthorityContract>,
    host_operations: Map<HostOperationId, HostAuthorityContract>,
    type_authority_shapes: Map<CanonicalTypeId, AuthorityShape>,
}
```

The checker keys functions, types, and operations by canonical identities, not
source names. It recomputes contracts from bodies and the closed call graph.
Missing information means no authority, not arbitrary authority.

### 3. Ghost authority identities in final WIR

Final WIR gains a security-only authority dialect:

```text
AuthorityRef {
    id: AuthorityId,
    kind: AuthorityKindId,
    rights: RightsSet,
    scope: AuthorityScope,
    origin: RootGrantId | ParameterOrigin | DerivedOrigin,
}

AuthorityContract {
    parameters: [AuthorityShape],
    captures: [AuthorityShape],
    result: AuthorityShape,
    effects: [HostEffect],
}

AuthorityUse {
    authority: AuthorityId,
    operation: HostOperationId,
    required_rights: RightsSet,
    required_scope: AuthorityScope,
}

HostAuthorityUses = [AuthorityUse]
```

Authority operations are limited to:

- `ReceiveRoot`, for an authenticated entry grant;
- `Pass`, `Borrow`, and `Return`, through a checked callable contract;
- `Capture`, into a closure or resumable frame whose contract records it;
- `Narrow`, which may only reduce rights or scope;
- `Derive`, through a catalogued host or standard-library derivation contract;
- `Join`, which retains only authority valid on every incoming edge; and
- `Use`, attached to a specific host operation.

There is no `Mint`, scalar cast, object load, deserialization, reflection, or
name lookup that creates an `AuthorityRef`.

An externref capability pairs its ghost `AuthorityId` with the exact reference-
typed definition that carries the host object. A zero-representation capability
still has an `AuthorityId`, even though encoding erases its receiver operand.
The absence of runtime representation changes the encoder, not the authority
judgment.

### 4. Final authority validation

`validate_authority_wir` distrusts both ordinary type-checker annotations and
optimizer-provided authority metadata. It checks final WIR against the
`CheckedAuthorityProgram` and independently recomputes authority flow.

The rules include:

1. Every authority identity has exactly one permitted origin.
2. Every function parameter, capture, and result agrees with its source-derived
   contract.
3. Every direct or indirect call passes an authority shape no broader than the
   callee accepts.
4. Generic substitution is invariant over authority kind and monotone over
   refinements. Erasure cannot merge authority-bearing and pure instantiations.
5. Closure and async-frame construction records every captured authority;
   invocation or resumption cannot observe an unrecorded root.
6. Control-flow joins intersect authority facts. A fact present on only one
   incoming edge cannot authorize a later use.
7. Narrowing and derivation are monotone according to the canonical host
   contract. Unknown derivation rejects.
8. Every `HostCall` carries the complete ordered set of `AuthorityUse` values
   required by its host contract. A one-authority method normally carries one;
   a broker or transfer operation may require more than one; a catalogued pure
   operation carries none. Missing, extra, reordered, or aliased uses reject.
9. Import presence, import name, source variable name, and a scalar value are
   never authority evidence.
10. Host-call arguments, results, buffers, reference kinds, and target support
    agree with both the authority contract and RFC-0162 object/layout rules.

The validator performs a closed-call-graph fixpoint. Recursive or indirect
components receive the union of possible effects and the intersection of
available authority. Unknown calls cannot exercise a host import.

An optimizer may remove an unused authority value or an unreachable host call.
It may specialize or inline functions. It need not preserve source-level token
names. It must leave a final graph from which the same or narrower authority
flow can be derived. The final validator, not a pass-maintained label, decides.

### 5. Combined object and authority judgment

The production admission function is conceptually:

```text
admit_wir(source, candidate):
    source_authority = authority_source_check(source)
    object_safety = validate_safewir(candidate)
    authority_safety = validate_authority_wir(source_authority, candidate)
    cross_check(object_safety, authority_safety)
    return AdmittedWirModule(...)
```

`cross_check` requires both judgments to refer to the same final functions,
definitions, objects, layouts, calls, and control-flow nodes. Separate reports
for slightly different candidates cannot be combined.

This catches the two relevant families:

- a stale range, lifetime, layout, or ownership fact cannot authorize memory
  corruption that fabricates data or redirects a privileged caller; and
- a type/effect/codegen bug cannot synthesize a host call even when it remains
  within Wasm memory and reference rules.

Validation accepts or rejects. It does not patch a rejected candidate. As in
RFC-0162, the compiler may independently validate a retained semantic-form
candidate; it may not rewrite the rejected candidate after validation.

### 6. Lean admission model

The security claim has one normative formal model. Lean definitions cover:

- canonical authority kinds, rights, scopes, origins, and monotone derivation;
- source authority contracts;
- SafeWIR's object, generation, range, ownership, and layout interface;
- final AuthorityWIR transfer, join, call, and fixpoint rules;
- security-relevant encoder templates; and
- the host-event and object-memory observations used by the claim.

The central theorem is:

```text
admit source candidate bytes = some certificate
    -> wasm_valid bytes
    -> object_memory_safe bytes
       /\ host_trace_authorized source.root_grants bytes
       /\ no_authority_amplification source candidate bytes
```

The theorem is progress/preservation for the scoped safety judgment, not full
program equivalence. Ordinary wrong answers remain outside it unless they
change object safety or host authority.

The Rust compiler may retain a fast validator for diagnostics, but its success
does not create `AdmittedExecutable`. A small executable checker compiled from
the Lean model consumes canonical candidate bytes, the source authority graph,
and the final Wasm. Its successful result is the production admission
authority. The Lean parser for this canonical format is part of the proved
checker; arbitrary Rust deserialization is not trusted to construct its terms.

CI checks the theorem with the pinned Lean toolchain, but CI proof success alone
is insufficient. The shipped compiler invokes the corresponding pinned checker
for every authority-validated artifact. The Lean compiler/runtime, its native
toolchain, the canonical input marshalling boundary, and the small invocation
shim are recorded trusted dependencies.

If an executable Lean checker proves impractical, a proof-producing Rust
validator plus a smaller Lean-derived certificate checker is an allowed
implementation only if the certificate checker independently verifies every
rule application and final-byte obligation. Trusting a Boolean or summary from
the Rust producer is not allowed.

### 7. Final-byte admission

The encoder accepts only `AdmittedWirModule`. It emits ordinary Wasm plus an
in-memory instruction map. The map never becomes trusted artifact metadata.

The admission checker parses the exact final bytes and requires a one-to-one
match for every:

- linear-memory load, store, copy, fill, grow, and heap-global update;
- reference conversion, reference-bearing local, call, table, struct, and
  array operation;
- allocation, retain, release, reuse, and fixed kernel template expansion;
- host import declaration and host call;
- security-relevant function signature and indirect-call target; and
- checked or proven trap boundary whose removal affects object safety.

The check is intentionally narrower than arbitrary source-to-Wasm translation
validation. Pure arithmetic and other non-security-relevant instructions need
not match exactly. Security-relevant instructions must match an admitted
operation or a fixed proved kernel template. Missing, duplicate, changed, or
unattributed operations reject the bytes.

The instruction map is an untrusted search aid. The checker derives opcodes,
operands, control context, imports, and types from the final bytes and compares
them with the sealed admitted obligations. Changing both the bytes and map does
not create authority.

Ordinary `wasmparser` validation remains mandatory and independent.

### 8. No unvalidated postprocessor

An authority-validated artifact has `postprocessor: None`. Binaryen or any other
binary optimizer may not run after final-byte admission, and bytes produced by
one may not reuse the earlier admission result.

A future postprocessor may enter this profile only by one of three routes:

1. it returns a final low-level form that re-enters the complete object,
   authority, and final-byte judgments;
2. it emits a proof independently accepted by the admission checker; or
3. its required transformations are implemented before final validation.

Pinning its executable, hashing flags, fuzzing it, or comparing a corpus does
not satisfy this rule. Those remain valuable quality controls for non-admitted
profiles and migration evidence.

Binaryen may be used offline as an optimization oracle. Witchy can compare its
output, identify profitable transformations, and implement equivalent
optimizations in WIR before the final check. The authority-validated profile
does not become the default release profile until the protected runtime-
performance corpus meets its existing gates without unvalidated
postprocessing. Security is not traded for an unmeasured performance cut.

### 9. Compiler-process hardening and confinement

The authority-validated build profile treats the source-processing worker as
untrusted with respect to the developer machine.

The parent constructs a canonical, bounded source bundle and starts a fresh
worker with:

- no network, process-spawn, keychain, credential, environment-secret, device,
  or ambient filesystem authority;
- read-only access only to the canonical source bundle and pinned toolchain
  resources required for compilation;
- a fresh writable scratch directory that is not executable state;
- explicit CPU, wall-time, memory, output-size, file-count, recursion, parser,
  type-expansion, and specialization budgets; and
- Landlock, Seatbelt, or an equivalent provider in required mode.

The worker returns only a length-delimited canonical candidate. The parent
treats every byte as hostile, applies its own size and recursion limits, parses
through the admission kernel, and publishes nothing unless admission succeeds.
Worker diagnostics are bounded text and cannot request terminal control,
credential prompts, or executable actions.

Compiler-owned Rust crates on the source-to-candidate path deny unsafe code.
The exact feature-resolved dependency graph has a checked unsafe-code census.
Each exception records reachability from hostile input, unsafe operations,
version, owner, upstream evidence, fuzz/Miri coverage, and replacement or
process-isolation rationale.

This layer contains bugs that precede SafeWIR. It does not make kernel or OS
bugs impossible, so the confinement provider and worker launcher remain in the
compiler-input trusted computing base.

### 10. Artifact, cache, and launch binding

The admitted record binds:

- exact final Wasm digest;
- source authority contract digest;
- SafeWIR and AuthorityWIR rule schema versions;
- canonical layout and host-operation catalog versions;
- Lean checker and toolchain identities;
- encoder/kernel template identity;
- target features and Wasm engine compatibility; and
- the explicit absence of a postprocessor.

Persistent cache hits match every field and revalidate the Wasm digest and
ordinary Wasm structure before constructing the private launch wrapper. Cached
source types, optimizer facts, admission maps, or serialized Rust objects never
construct authority.

At launch, the selected root grants must cover the checked entry contract. The
runtime exposes exactly the imports in the admitted import manifest and no
broader host function under an alias. The grant binding follows RFC-0126. The
publisher and executable-state trust decision follows RFC-0157.

The artifact record is outside guest-visible memory. Existing artifact/cache
validation may retain it, but no guest instruction consults it during
execution.

### 11. Runtime and platform containment

Static admission protects against generated-code memory and authority defects.
It still trusts the Wasm engine and host implementation to execute admitted
operations correctly.

The strongest Witchy sandbox claim therefore composes with capability-derived
outer confinement:

```text
authority-validated
    = admitted generated code under the named engine/host TCB

authority-validated + confine-required
    = admitted generated code plus a deny-default OS boundary derived from
      the same root grants
```

The second claim is available only when the platform provider reports complete
coverage for the requested resource classes. Partial or unavailable
confinement fails closed under `--confine=required`. This RFC does not relabel
partial confinement as a proof of engine correctness.

## Zero-runtime-cost contract

For the same final optimization decisions, enabling authority admission changes
none of the guest-executed Wasm instructions or runtime representations.

Production Wasm contains no new:

- instructions, branches, checks, calls, traps, loads, stores, or allocations;
- imports, functions, tables, memories, globals, tags, fields, or custom
  sections required at execution;
- capability handles for zero-representation authorities;
- shadow metadata, proof objects, source maps, or authority identities; or
- runtime validation or monitoring hooks.

Compilation, artifact admission, and the first bounded cache verification may
take longer. A confined launch may have the platform cost already specified by
RFC-0103/RFC-0158. Those costs are reported separately and may not be hidden as
guest execution overhead.

The zero-cost gate compares authority admission enabled and disabled after
fixing identical optimization choices. It checks exact executable code bytes,
instruction counts, imports, globals, memories, tables, code size, allocations,
and protected benchmark distributions. Out-of-band artifact metadata is
reported separately and excluded only when it is never mapped into the guest.

## Testing and adversarial evidence

### Source and final-WIR authority matrix

Accepted and rejected fixtures cover:

- no-capability `String -> String` dependency functions;
- a dependency receiving only `File[Read]` for benign content;
- module-wide imports used by a different authorized function;
- every zero-representation capability;
- rights and scope narrowing, attempted widening, derived capabilities, and
  target availability;
- aliases, generic instantiation and variance, nominal wrappers, tagged sums,
  tuples, lists, dictionaries, closures, recursion, and indirect calls;
- async tasks, resumable frames, channel payloads, task inheritance, and joins;
- reflection, serialization, `Dynamic`, build steps, and rejected native/raw
  boundaries;
- source annotations, typed-AST facts, function effects, or operation IDs
  mutated after ordinary type checking; and
- final WIR with an injected, renamed, reordered, or differently authorized
  host call.

Every rule has an accepted fixture, a rejected fixture, and a mutation that
must be killed by the admission checker.

### SEC-080 and SEC-081

The initial companion corpus includes:

| Finding | Permanent source evidence | Additional RFC-0163 evidence |
|---|---|---|
| SEC-080, stale bounds facts enabling cross-frame corruption and capability forgery | `src/example_tests/capabilities.rs::capability_free_dependency_cannot_rewrite_caller_path_via_bounds_elision_sec_080`; `file_only_dependency_cannot_forge_console_via_bounds_elision_sec_080` | Reject forged `ProvenIndex`; reject zero-representation `Console` use without an authority origin; mutate final bytes to inject `console.print` and require final-byte rejection |
| SEC-081, stale nonempty proof after list/bound redefinition | `crates/witchy-lower/tests/codegen.rs::normal_countdown_scopes_constant_zero_elision_to_its_outer_loop` | Reject a range fact tied to the prior `DefinitionId` or object generation; reject a final unchecked load not matched by the admitted obligation |

The SEC-080 no-capability and file-only scenarios must remain end-to-end
offensive tests. It is insufficient to assert only that an internal token was
absent.

### Final-byte and postprocessor mutations

Starting from an admitted module, tests mutate exactly one property at a time:

- memory opcode, width, alignment, offset, address expression, or control
  dominance;
- allocation, retain, release, or reuse template;
- host import, host call target, argument order, rights, or zero-representation
  receiver erasure;
- function signature, table type, reference kind, or indirect target;
- instruction-map attribution, duplicate, omission, or candidate digest; and
- one transformation from every configured Binaryen pass.

Every changed security-relevant byte sequence must reject unless it returns
through a new complete admission. Pure arithmetic changes may be accepted only
when they cannot alter a checked address, bound, layout discriminator, control
guard, authority use, host argument, or security-relevant trap.

### Compiler-worker escape probes

Integration tests give malicious source fixtures paths, environment markers,
network listeners, credential helpers, executable files, symlinks, oversized
diagnostics, recursion bombs, and malformed candidate output. The worker must
either compile within its explicit bundle/scratch contract or fail without an
observable access. The parent must remain healthy and publish no artifact.

These tests exercise confinement. They are not evidence that the compiler has
no memory-safety defects.

### Formal-model evidence

CI and release evidence record:

- pinned Lean version and theorem digest;
- complete rule-to-definition, implementation, fixture, and mutation mapping;
- proof-checking result from a clean toolchain build;
- equivalence vectors between the diagnostic Rust checker and authoritative
  admission checker;
- malformed canonical-input and parser-budget corpus; and
- the exact checker/toolchain identity bound into admitted artifacts.

Differential agreement is a debugging aid. On disagreement, the authoritative
checker decides admission and the release blocks until the mismatch is
explained.

## Delivery plan

The shared interfaces are frozen before independent tracks begin:

```text
canonical source/type/host-operation identities
                         |
            +------------+-------------+
            |                          |
 source authority judgment      SafeWIR completion
            |                          |
            +------------+-------------+
                         |
                final AuthorityWIR
                         |
            combined WIR admission schema
                         |
            +------------+-------------+
            |                          |
      Lean admission model       final-byte checker
            |                          |
            +------------+-------------+
                         |
           no-postprocessor release path
                         |
       artifact binding + compiler confinement
                         |
       authority-validated confined release gate
```

Implementation tracks are:

1. **Contract census:** enumerate every capability, right, derivation, host
   operation, zero-representation receiver, target restriction, compiler input,
   postprocessor, cache, and launch path.
2. **Source authority judgment:** implement the independent closed-world
   checker and canonical `CheckedAuthorityProgram`.
3. **AuthorityWIR:** add ghost identities and contracts, then validate final
   calls, captures, joins, refinements, derivations, and host uses.
4. **Combined admission:** bind AuthorityWIR to RFC-0162's exact final candidate
   and make private wrappers the only production encoder/publisher entry.
5. **Lean kernel:** formalize the judgment, prove non-amplification and object-
   safety composition, and ship the authoritative checker or certificate
   checker.
6. **Final bytes:** validate every security-relevant instruction and standard
   Wasm structure against admitted obligations.
7. **Postprocessor closure:** disable post-admission Binaryen and recover its
   protected performance wins before the profile becomes the default.
8. **Compiler confinement:** isolate adversarial source processing, enforce
   budgets, and publish the unsafe-dependency census.
9. **Artifact and runtime composition:** bind exact bytes, schemas, toolchain,
   grants, imports, cache, and required outer confinement.
10. **Adversarial promotion:** close the SEC, mutation, worker-escape,
    zero-runtime-cost, performance, and release evidence ledgers.

Tracks 2, 3, 5, 6, and 8 may proceed concurrently after track 1 freezes the
identities and canonical formats. Track 4 is the integration critical path.
The merge queue serializes landing, not implementation.

The profile is experimental and cannot be advertised as authority-validated
until every acceptance row is complete. Partial report-mode validators are
valuable development tools but create no release claim.

## Acceptance ledger

No row may be closed by prose, ordinary type-checker success, backend agreement,
a green subset of optimizer modes, a pinned tool version, differential fuzzing
alone, or the existence of ghost metadata that the final validator does not
rederive.

| Contract | State | Required evidence |
|---|---|---|
| Capability and host-operation census is complete | pending | Every RFC-0126 capability, zero-representation authority, right, derivation, target, and import maps to a canonical contract |
| Authority source judgment is independent | pending | Typed-annotation, effect, generic, capture, dynamic, async, and operation-ID mutations are rejected from parsed source |
| No-capability function has no authority | pending | `String -> String` dependencies cannot use any linked host import across direct, indirect, inlined, specialized, closure, or async forms |
| Narrow capability cannot widen | pending | File-only, refined Dir/Net/Secret, derived-capability, alias, aggregate, and return matrices remain monotone |
| Zero-representation capabilities have provenance | pending | Every such host call requires a ghost root/parameter/derivation lineage even though the operand erases |
| Final WIR authority is rederived | pending | Optimizer metadata mutations, stale identities, joins, recursion, and call-graph summaries reject |
| SafeWIR and AuthorityWIR bind the same candidate | pending | Mismatched candidate/function/definition/control/layout identities cannot combine |
| Production has one private WIR admission gate | pending | No encoder, artifact, cache, debug, raw-body, or fallback path accepts ordinary WIR |
| Lean model states and proves the scoped claim | pending | Pinned theorem proves object-safety composition, host-trace authorization, and authority non-amplification |
| Shipped admission uses the proved checker | pending | Every claimed artifact runs the pinned Lean-derived checker or independently complete certificate checker |
| Exact final bytes are checked | pending | Every memory/reference/kernel/host mutation rejects; ordinary Wasm validation also passes |
| No unvalidated postprocessor remains | pending | Claimed artifacts record `postprocessor: None`; transformed bytes cannot reuse admission |
| Protected performance is retained | pending | Required former Binaryen wins exist before admission or meet approved no-regression gates |
| Compiler-owned unsafe surface is bounded | pending | `deny(unsafe_code)` plus feature-resolved dependency census, owners, evidence, budgets, and fail-closed CI |
| Adversarial compilation is confined | pending | Capability-empty worker and malicious access/escape probes pass on every claimed host platform |
| Artifact/cache/launch binding is exact | pending | Digest/schema/toolchain/import/grant mismatches re-enter admission or reject; no serialized fact constructs authority |
| SEC-080 and SEC-081 are complete corpus members | pending | Source, forged-WIR, authority, final-byte, and end-to-end exploit tests all land |
| Generated execution has zero safety overhead | pending | Exact code/instruction/import/global/memory/table/allocation and protected benchmark evidence |
| Confined claim fails closed | pending | `--confine=required` rejects partial/unavailable providers and uses the same grant contract |

`implemented` requires every row. A complete SafeWIR implementation without
authority provenance, a complete authority checker without exact final-byte
admission, or exact bytes followed by Binaryen is not this RFC.

## Alternatives

### Trust RFC-0126 source typing and import pruning

Rejected. They remain first-line correctness checks, but the threat model is a
compiler bug induced by adversarial source. Module-wide import availability and
ordinary typed annotations cannot serve as independent authority evidence.

### Represent every capability as an externref

Insufficient by itself. Uniform externrefs would remove the special
zero-representation receiver case and may be attractive for simplicity, but a
compiler can still pass the wrong reference, widen a refinement, confuse a call
contract, or synthesize an authorized caller. It would also change runtime
representations and potentially add work. Ghost authority validation is needed
regardless of the chosen carrier.

### Add runtime capability checks

Rejected for the generated-code path. Dynamic handles, tags, monitors, or
per-host-call token validation would add runtime representation or work. This
RFC obtains the same non-amplification property at compile time and erases the
evidence.

### Pin and fuzz Binaryen

Retained as quality evidence, rejected as a proof. A deterministic bug in the
pinned optimizer remains a deterministic sandbox bug. The claimed bytes must
be checked after the last transformation.

### Prove the complete compiler in Lean

Deferred as unnecessary for this claim. The smaller admission judgment checks
the untrusted compiler's final candidate and exact security-relevant output.
Complete compiler verification would also prove ordinary semantics, which is
valuable but not required to prevent authority amplification.

### General translation validation of arbitrary optimized Wasm

Deferred. It would permit arbitrary postprocessing, but proving refinement for
all Wasm control flow, memory, GC, and host effects is substantially broader
than matching the sealed encoder/kernel output. This RFC chooses the smaller
kernel and moves required optimizations before it.

### Rely only on outer OS confinement

Rejected. Outer confinement reduces the blast radius of engine and host bugs,
but it cannot reconstruct Witchy's internal per-function delegation. A process
may legitimately receive several broad roots while an untrusted dependency is
supposed to receive none. Static authority validation preserves that internal
boundary.

## Drawbacks

- Compile time, compiler memory, toolchain size, and build-system complexity
  increase.
- The independent authority checker duplicates a security-relevant subset of
  source typing and must evolve with every capability-bearing language form.
- Shipping a Lean-derived checker introduces a new compiler-side toolchain and
  trusted invocation boundary.
- Exact final-byte matching constrains encoder implementation and prohibits
  arbitrary postprocessing after admission.
- Recovering Binaryen's useful transformations before validation may require
  substantial optimizer work even though the final runtime target has no
  safety overhead.
- Compiler-worker confinement must be implemented and tested per host platform.
- The formal model can omit a real behavior. Mutation tests, SEC corpus tests,
  independent review, and outer confinement remain necessary even after a
  proof succeeds.
- The strongest claim still names Wasmtime, host handlers, the Lean/native
  toolchain, confinement provider, OS, and hardware as residual trusted
  components. This RFC makes that boundary smaller and explicit; it does not
  make it disappear.

## Prior art

- **RichWasm** uses erased linear and unrestricted capabilities as a typed
  compilation target and proves type safety. RFC-0162 adopts its object-memory
  lesson; this RFC extends the erased-capability idea to host authority and
  exact executable admission.
  <https://dl.acm.org/doi/full/10.1145/3656444>
- **Proof-carrying code and typed assembly language** establish the pattern of
  checking a small target-level safety argument instead of trusting the
  producer.
- **CompCert validation a posteriori** demonstrates that a complicated
  untrusted algorithm may propose output while a smaller checker owns
  acceptance.
- **RockSalt and LFI verification** show that an executable admission checker
  can be the security boundary and that its invariant can be mechanically
  proved. SafeWIR plus authority flow is more stateful and interprocedural, so
  this RFC uses the same architecture without claiming the same proof size.
  <https://arxiv.org/abs/2508.15898>
- **Robustly safe compilation** frames compiler security as preservation of
  source-level safety properties in adversarial target contexts. The host-trace
  authorization theorem is Witchy's narrower, capability-specific property.
- **CHERI and object-capability systems** separate authority provenance from
  ordinary integer data. Witchy's externref carriers provide that runtime
  separation for represented roots; ghost AuthorityWIR provides the static
  lineage required for erased receivers.
- **Alive2 and translation validation** motivate checking the producer's final
  result. This RFC deliberately validates only the closed object/authority
  property and sealed encoder output, rather than all semantic equivalence.
- **Language-server and build sandboxes** motivate processing hostile source in
  a capability-empty worker whose output is parsed as untrusted data. Witchy's
  capability-derived Landlock and Seatbelt providers supply the existing outer
  mechanism.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code, not here.
-->
