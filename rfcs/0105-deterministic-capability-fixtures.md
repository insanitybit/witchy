# RFC-0105: Deterministic capability fixtures and test transcripts

Status: accepted; implementation in progress

## Summary

Witchy tests need deterministic substitutes for capability providers, not a
second authority system and not global monkeypatching. This RFC defines one
serializable `FixturePlan` and one resulting `TestTranscript` for the
interpreter, native Wasm host, and browser Wasm host. The same plan must produce
the same guest-observable values, errors, call order, and transcript on every
supported backend.

Plain tests remain authority-free. A fixture is inert data authenticated by the
test runner and interpreted only by test-only providers. It cannot contain a
host path, socket, process handle, environment accessor, clock, random source,
or callback. Integration tests remain the only test tier that can receive real
host grants, and those grants stay explicit.

This RFC is the narrow successor permitted by RFC-0077's closed mock tail. It
does not reopen RFC-0084 interception, add source-level monkeypatching, or make
capability values forgeable.

## Motivation

Witchy already has pieces of a testing story:

- RFC-0077 gives plain tests zero real authority, permits test-boundary
  construction of sealed domain data, and defines explicit `Dir` and `Net`
  integration grants. Its former `testing.mock_dir` experiment has been removed
  in favor of this RFC's single fixture path.
- RFC-0091 provides opt-in browser capability providers.
- RFC-0102 requires deterministic fixture providers for portable I/O roots.
- RFC-0013 and RFC-0057 define grant documents and attenuation.
- `WITCHY_RAND_SEED`, browser documentation fixtures, and Console input queues
  provided local deterministic behavior before this shared contract.

Those mechanisms do not form one system. They use different configuration
shapes, duplicate in-memory filesystem behavior, do not share failure scripts,
do not produce a common transcript, and are not exposed through a complete
`witchy test` workflow. A test can demonstrate a happy-path directory read but
cannot portably assert that a request used the narrowed origin, that a process
was not invoked, that the second read failed, or that interpreter and Wasm
observed the same provider contract.

Capability-oriented software needs stronger tests than ambient-I/O software.
Tests must be able to prove both what happened and what did not happen.

## Terminology

This RFC uses the following terms precisely:

- **Collaborator**: ordinary Witchy data, function, or trait value supplied to
  domain code. Collaborators are the preferred unit-testing seam.
- **Stub**: a collaborator or fixture provider configured to return values
  without asserting how it is called.
- **Fake**: a deterministic implementation with useful behavior, such as an
  in-memory filesystem.
- **Strict mock**: a fixture provider with an ordered script. An unexpected,
  missing, duplicated, or differently attenuated operation fails the test.
- **Fixture capability**: a real sealed Witchy capability value backed only by
  runner-authenticated inert fixture data. It has no route to host authority.
- **Integration grant**: an explicit real host capability supplied only to a
  local package's integration test.
- **Browser provider**: an explicitly enabled page host provider. It may be
  real or fixture-backed; browser virtualization alone does not make it a test
  fixture.
- **Fixture plan**: the complete immutable input to test providers.
- **Transcript**: the ordered, normalized record of provider observations and
  test output.

The standard library may use `fake_*` and `stub_*` names for ordinary
collaborators. The names `fixture_*` and `mock_*` are reserved for values
created through the authenticated test boundary.

## Trust boundary

### Plain tests

A plain test gets no real host grants. This includes filesystem, network,
Fetch, environment, subprocess, secrets, clock, random, and stdin. RFC-0102's
VM workers are a zero-authority host facility rather than a capability root;
using their deterministic sequential semantics does not grant authority. The
runner may supply fixture-backed capabilities requested by the test's signature
and present in its fixture plan.

Fixture plans are data, not authority. They must be fully decoded and validated
before any guest module is instantiated. No plan field may name:

- an ambient or preopened host path;
- an unrestricted URL or raw socket destination;
- an executable path or host command;
- a process environment source;
- a host secret store;
- a native file descriptor, externref, callback, or implementation object.

Strings that resemble paths, URLs, commands, or environment names are merely
keys in the corresponding fixture namespace.

The test linker exposes fixture imports only while linking the synthesized test
artifact. Production `run`, `check`, `compile`, `build`, package-manager,
comptime, and ordinary browser instantiation paths do not accept a
`FixturePlan`.

### Integration tests

Integration tests may receive explicitly declared real grants. Real grants and
fixture providers are distinct inputs and distinct transcript origins. A
single capability family cannot be both real and fixture-backed in one test.
The runner rejects that ambiguity before compilation.

Only tests owned by the selected package can receive integration grants.
Dependency tests remain authority-free. Imported helpers cannot inherit the
test module's authority to construct sealed values or fixture capabilities.

### Test-only sealed construction

RFC-0077's authenticated construction rule remains unchanged. Only source
items proved to originate in the selected package's test module may construct
otherwise sealed domain data for tests. The privilege is lexical and
definition-site based, not call-site based, and cannot be re-exported,
qualified through another module, captured in a production closure, or retained
in a production artifact.

Fixture capability handles are never source-constructible. They are minted by
the test linker from a validated plan and branded by the backend. A forged,
stale, cross-family, or cross-instance handle must be rejected.

## The fixture contract

`witchy-testkit` owns a versioned, serializable contract:

```text
FixturePlan {
    version: 1,
    console: ConsoleFixture?,
    clock: ClockFixture?,
    rand: RandFixture?,
    env: EnvFixture?,
    filesystem: FilesystemFixture?,
    fetch: FetchFixture?,
    secrets: SecretStoreFixture?,
    exec: ExecFixture?,
    argv: List(String)?,
    expectations: Expectations
}

TestTranscript {
    version: 1,
    seed: Int?,
    events: List(TestEvent),
    stdout: List(String),
    stderr: List(String),
    result: TestResult
}
```

The on-disk and browser-message representation is canonical JSON. Unknown
versions, duplicate object keys, unknown fields, invalid UTF-8, invalid scalar
values, oversized plans, and internally inconsistent scripts are errors.
Canonical serialization sorts map keys, preserves script and event order, and
uses explicit tagged variants. JSON numbers are not used where host integer
width could differ; such values are decimal strings with checked bounds.

The Rust contract crate contains only data types, validation, canonicalization,
shared deterministic state machines, and normalized errors. It cannot depend on
Wasmtime, the interpreter, browser code, CLI code, ambient filesystem APIs, or
network APIs. Runtime and interpreter adapters depend on it. The browser host
implements the same documented wire contract and is checked against canonical
vectors emitted by the Rust crate.

Every event contains:

```text
TestEvent {
    sequence: Int,
    family: CapabilityFamily,
    operation: String,
    target: String?,
    arguments: canonical data,
    effective_rights: List(String),
    outcome: success(value summary) | error(FixtureError),
    source: SourceLocation?
}
```

Guest-observable bytes and errors are exact. Secret bytes are never copied into
the transcript; secret events contain only the fixture name, operation,
disclosure policy, byte length where already observable, and outcome.

## Provider matrix

### Console

The plan supplies an ordered line-input script and optional write expectations.
Writes are captured as exact UTF-8 strings in event order. Exhausted input,
invalid fixture bytes, configured read failure, and configured write failure
are deterministic. Host stdin/stdout are never consulted by a plain test.

### Clock

The plan supplies an ordered sequence of nanosecond timestamps or a checked
start/step clock. Exhaustion is an error unless the plan explicitly selects
repeat-last. Time never advances because of host scheduling.

### Rand

The plan supplies either a `u64` seed for the shared SplitMix64 sequence or an
ordered sequence of `u64` values and failures. The plan seed replaces
`WITCHY_RAND_SEED` for tests. The environment variable remains a developer
diagnostic for non-test parity and is not the fixture API.

### Env

The plan supplies an immutable map and an initial allow-list. Missing and
present-empty values remain distinct. `only` can narrow but never widen the
handle. No process environment lookup occurs.

### Dir and File

One shared in-memory filesystem state machine backs interpreter and native Wasm
tests. The browser adapter uses the same wire vectors and semantics.

The plan describes normalized relative paths, bytes, directory entries, and
initial rights. It supports read and write when granted, handle-relative
resolution, file cursors where the production contract exposes them,
subdirectories, entry policies, and configured per-operation failures.
Absolute paths, `..`, NUL, duplicate normalized paths, and a file/directory
collision are rejected.

Operations enforce the same rights and attenuation rules as real providers.
Partial reads and writes are scripted as explicit byte counts. A fixture may
model permission denied, not found, already exists, not a directory, invalid
data, interrupted, timeout, and generic provider failure only where that error
is representable by the production contract.

`testing.mock_dir` and its interpreter/Wasmtime-specific filesystem backings are
deleted. The fixture plan is the only capability virtualization path.

### Fetch

The plan declares allowed origins and an ordered request script. Each expected
request includes method, normalized URL, selected headers, and body bytes.
Each response is status, selected headers, body bytes, or a normalized failure.
Redirects are explicit script steps and are rechecked against the effective
origin grant.

No plain-test Fetch provider performs DNS, opens a socket, invokes browser
`fetch`, or falls through after a script miss. Raw `Net` is intentionally not
fixture-backed by this RFC: portable application code should inject a
collaborator or use Fetch. Real raw Net remains integration-only.

### SecretStore

The plan supplies named opaque byte strings and disclosure flags. Fixture
handles obey the same branding, narrowing, reveal, signing, and non-printable
rules as production handles. Transcripts redact bytes and signatures. Missing,
forbidden reveal, invalid key material, and scripted provider failure are
covered. Fixture secrets never enter production grant documents.

### Exec

The plan supplies logical tool names and an ordered invocation script. An
invocation records tool, argv, input bytes, and effective allow-list, and
returns scripted stdout, stderr, and exit status or a spawn/timeout/I/O failure.
No fixture field is interpreted as a host executable path, and no subprocess is
spawned. Narrowing cannot add tools.

### VM facility (explicit fixture exclusion)

VM is not a fixture family and `FixturePlan` has no `vm` field. Witchy's shipped
surface is RFC-0102's zero-authority, same-module `vm.par_map`, `vm.with_dir`,
and `vm.serve` facility, not a logical child-module spawn capability. The
interpreter executes the specified sequential reference semantics; native Wasm
and the browser use fresh worker instances. `vm.with_dir` may receive a
fixture-backed `Dir`, whose authority and transcript remain owned by the shared
filesystem provider. No VM worker inherits any other fixture family.

Plans that contain `vm` are rejected as unknown rather than accepted by an
unreachable adapter. Tests cover the real facility through interpreter/Wasm/
browser parity. Shared-memory, externally scheduled, and freely racing worker
behavior remain outside the deterministic contract instead of being simulated.

### Argv

The plan supplies an immutable string list. Argv is launch input rather than
authority, but it participates in the plan so all backends receive identical
input and transcript metadata.

### User-defined capability records

For an owned test module, the runner recursively assembles a concrete,
non-generic named-field capability record whose leaves are supported fixture
roots or argv. It flattens those leaves into the compiler-generated test
driver's ordinary root parameters, then constructs the sealed record only at
the authenticated test boundary. A missing or unsupported leaf is diagnosed
with its full field path. Recursive aggregates terminate with a diagnostic.
Dependency tests cannot inherit this construction privilege.

Fixture plans do not invent defaults for arbitrary domain data. Put such values
in an ordinary collaborator, argv, or the relevant provider's explicit plan
data.

## Scripting and expectations

Provider scripts are FIFO per fixture instance. Each step has an operation,
matcher, and outcome. A matcher can use exact values and explicitly supported
wildcards; regexes, executable predicates, callbacks, and backend-specific code
are forbidden.

Strict expectations may assert:

- exact calls and order;
- unordered groups where order is intentionally irrelevant;
- minimum and maximum call counts;
- exact effective rights and allow-lists;
- no call to an operation or family;
- complete script consumption;
- final filesystem contents;
- captured Console and Exec output;

An unexpected call fails at that call with the test source location when the
compiler can provide it. An unconsumed required step fails at test completion
and points to the fixture declaration. Cleanup runs after success, guest
failure, timeout, and provider failure. Cleanup failure is reported without
hiding the primary failure.

Plans have deterministic size, event, byte, and value-recursion limits.
Limit errors are normal test failures, never panics or unbounded allocation.

## `witchy test`

The command accepts a file, directory, project, or rune and supports:

```text
witchy test <file.witchy|dir>
    --list
    --filter <text>
    --backend interpreter|wasm|both
    --fixtures <plan.json>
    --seed <u64>
    --show-output
    --format human|json
    --integration
    --dir <root>
    --net <addr>
```

Fixture runs default to `both`; ordinary and integration runs default to Wasm.
`both` runs the same normalized plan on interpreter and Wasm and compares
result, guest-observable error, output, and transcript. Browser parity is a
checked book/e2e shard rather than a required local browser launch for every
`witchy test`.

Discovery order and JSON output are stable. Filtering occurs after discovery
and before compilation. `--list` performs discovery and validation without
running tests. Output is captured by default; failed fixture output is shown
automatically and `--show-output` also shows passing output. JSON schema 2
retains transcripts and partial output on pass and failure. Exit status `0`
means success, `1` means completed tests failed, and `2` means usage, fixture,
or infrastructure failure. Every usage and fixture error includes the
responsible CLI argument or fixture source location.

Inline fixture syntax in Witchy source is not added by this RFC. A test chooses
a named plan through runner metadata or a sidecar fixture file. This keeps
fixture authority and parsing outside the language and avoids a new compiler
feature.

## Standard library surface

`std/testing.witchy` remains small. It provides assertions and ordinary
collaborator helpers. It may expose typed references to runner-provided fixture
roots, but it does not parse fixture plans, implement provider state machines,
hold global registries, or mint authority.

Assertions over transcripts are runner assertions or pure helpers over a
read-only transcript value. They cannot mutate provider state or retrieve
secret payloads.

## Browser and documentation

Every complete flagship example has a canonical fixture plan in the book
manifest. The documentation runner creates a fresh opaque sandbox frame,
derives CSP from the declared capability footprint, sends only the canonical
plan and compiled artifact, and destroys the frame after completion.

The frame cannot use parent callbacks as providers. It returns only the
normalized transcript and result. The parent verifies origin, nonce, message
shape, size limits, test identity, and completion state. A stale or duplicate
message is rejected.

The browser adapter must pass the same canonical provider vectors as Rust.
Browser-only implementation limits are explicit errors and cannot silently
change a fixture outcome.

## Flagship example

The book includes one capability-native application that:

- reads configuration through narrowed Env;
- obtains credentials through SecretStore without revealing them;
- fetches origin-scoped data;
- maintains a cache through narrowed Dir/File;
- uses Clock for freshness;
- reports through Console;
- optionally invokes an allowlisted logical Exec tool or sequential VM worker.

Its test progression demonstrates:

1. pure domain tests with functions and traits;
2. a stateful fake;
3. strict fixture expectations;
4. malformed and failing provider outcomes;
5. rights and origin attenuation;
6. an explicit integration test with real grants;
7. interpreter/Wasm transcript parity;
8. the same complete examples in fresh browser frames.

The example must not add a capability merely to exercise this RFC. The Exec
capability and VM facility are included only where the application has a
credible use for them.

## Diagnostics and provenance

Fixture validation diagnostics point into the fixture file. Runtime provider
failures carry the guest call's source location when debug provenance is
available and identify the fixture step that supplied the outcome. Parity
reports show the first differing event and both normalized values.

Diagnostics never include secret bytes, unrestricted host paths, process
environment values, or backend object identities.

## Compatibility and migration

This RFC does not promise compatibility for pre-0.1 testing internals.
Duplicate and environment-variable-driven test paths are removed as their
callers migrate:

- `testing.mock_dir` and both backend-local mock filesystems are removed;
- direct `Capabilities::console_input` test setup migrates to `FixturePlan`;
- test use of `WITCHY_RAND_SEED` migrates to the plan seed;
- documentation-specific Fetch fixtures migrate to the canonical plan;
- backend-local mock state machines are deleted.

Production provider APIs remain source-compatible unless a redundant public
test escape must be removed to preserve the trust boundary.

## Rejected alternatives

### Global monkeypatching

It is definition-ambiguous, order-dependent, hostile to concurrency, and can
replace code outside the caller's authority. Ordinary collaborators and sealed
fixture providers are sufficient.

### Reusing real provider configuration

A temporary directory, loopback server, process environment, fake executable,
or browser callback still grants real authority and introduces host variance.
Those belong in explicit integration tests.

### Fixture-backed raw Net

Socket scheduling, packet boundaries, DNS, TLS, and partial transport behavior
would create a second networking stack. Fetch covers the portable application
case. Raw Net remains integration-only until a separate RFC proves a bounded
contract.

### Backend-specific fixture formats

They make parity unverifiable and ensure behavior drifts. One canonical plan is
the point of this RFC.

### Source-level fixture literals

They unnecessarily expand the language, parser, type checker, and privilege
surface. External authenticated data is adequate.

## Implementation structure

The intended crate layering is:

```text
witchy-cap-model
        |
witchy-testkit          canonical plan, transcript, state machines, errors
   |             |
witchy-runtime   witchy-interp
        \         /
          witchy CLI
```

`witchy-testkit` may depend on `witchy-cap-model` and serialization/hash crates.
It must not depend on syntax, type checking, lowering, runtime, interpreter, or
CLI crates. Capability-family names and rights used by the wire contract come
from `witchy-cap-model` or a smaller dependency, not duplicated strings.

Runtime adapters translate ABI calls to testkit operations. Interpreter
adapters translate evaluator calls to the same operations. Neither adapter
reimplements normalization, rights checks, script matching, failure sequencing,
or transcript construction.

## Acceptance ledger

This RFC is implemented only when every item below has checked evidence:

- [x] `witchy-testkit` has no ambient authority and no forbidden dependency
  edge.
- [x] Canonical plan parsing rejects malformed, ambiguous, oversized, cyclic,
  and unknown input without panic.
- [x] Console, Clock, Rand, Env, Dir/File, Fetch, SecretStore, Exec, argv,
  and user capability records satisfy the provider contracts above.
- [ ] The real zero-authority VM facility retains sequential interpreter,
  native Wasm, and browser parity; fixture plans reject a `vm` family, and
  `vm.with_dir` can observe only its explicitly passed fixture-backed `Dir`.
- [x] Raw Net is rejected in plain fixture plans and remains explicit
  integration authority.
- [x] Every provider supports its meaningful production error shapes,
  sequencing, exhaustion, and cleanup behavior.
- [x] Interpreter and Wasm consume the same testkit state machines.
- [x] Browser passes canonical wire vectors and full showcase transcript parity.
- [x] Handles are branded, rights never widen, origins never widen, and fixture
  data cannot become host authority.
- [ ] Plain tests, dependency tests, production commands, comptime, build, and
  package-manager paths have adversarial non-escalation tests.
- [x] `witchy test` implements discovery, listing, filtering, capture,
  deterministic seeds, fixture selection, integration grants, backend choice,
  parity, stable exits, source diagnostics, and JSON output.
- [x] Focused provider tests cover success, malformed data, permission failures,
  partial I/O, timeout, process failure, unexpected calls, missing calls,
  transcript ordering, cleanup failure, and configured limits.
- [x] `std/testing.witchy` and all compatibility paths are reduced to the
  smallest final surface.
- [ ] The flagship application and every complete book example run from the
  private extracted artifact in fresh opaque browser frames with derived CSP.
- [x] Book, CLI help, README, architecture, status, RFC references, and examples
  manifest state the same current truth.
- [ ] Warning-denied Clippy, formatting check, focused tests, interpreter/Wasm
  parity, browser/book, e2e, private installed-artifact smoke, and serialized
  full gate pass on the exact merged commit.
- [ ] No task-owned branch, worktree, compatibility shim, TODO, deferred phase,
  or queue entry remains in flight.

The five unchecked rows are live closure blockers, not deferred scope:

- fixture-backed `vm.with_dir` and its browser callback adapter remain
  unavailable;
- the full plain/dependency/production/comptime/build/package-manager
  non-escalation matrix still needs one checked adversarial inventory;
- the private extracted docs artifact has not yet run the flagship and every
  complete book example in fresh opaque derived-CSP frames;
- e2e, extracted-artifact smoke, and the serialized full gate must pass on the
  exact landing commit;
- task branches, worktrees, compatibility residue, and queue entries can be
  removed only after that landing.

## RFC relationships

- RFC-0077 remains authoritative for test privilege, ownership, pruning, and
  real integration grants. This RFC fills its explicitly closed mock tail.
- RFC-0091 remains authoritative for opt-in browser providers. This RFC defines
  when those providers are fixture-backed and how parity is proved.
- RFC-0102 remains authoritative for portable roots and provider menus. This
  RFC supplies its deterministic fixture-provider doctrine.
- RFC-0013 and RFC-0057 remain authoritative for real grants and attenuation.
  Fixture plans are not grant documents.
- RFC-0084 remains deferred. This RFC does not implement interception.
