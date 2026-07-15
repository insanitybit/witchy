---
rfc: 0077
title: "Test doubles in witchy — tests are permissive; the VM sandbox is the only boundary"
status: accepted
created: 2026-07-08
tracking: >
  Accepted in slices. Sealed domain-data construction is implemented for the
  entry module under `witchy test` and remains production-strict elsewhere.
  Plain tests run with zero real host grants; unused effectful production code
  is pruned from the synthesized test artifact. `testing.mock_dir` is
  implemented as a read-only in-memory `Dir[Read]` backend for both test tiers.
  The real-capability integration tier is implemented for explicit `Dir` and
  `Net` grants, with manifest/lock-resolved dependency tests held at zero real
  authority. Other mock backends and real capability kinds remain later work.
related:
  - "0002 (user-definable capabilities — sealing is a correctness contract, not the security boundary)"
  - "0005 (unforgeable capabilities — production invariant; the VM, not the seal, is the perimeter)"
  - "0013 (capability grant documents — how real authority is granted at run)"
  - "0044 (contract rules — a fake's failure shapes should match the real one's)"
  - "0046 / 0050 (trait bounds + method dispatch — the injection seam this rests on)"
  - "0072 (diagnostic goldens — lock the test-runner surface once this lands)"
---

# RFC-0077: Test doubles in witchy

> **Implementation status (2026-07-15):** the runner split is implemented:
> `witchy test` links the entry test module in test mode, allowing direct
> construction of foreign `sealed type` values so tests can exercise malformed
> domain data. Production `run`/`check`/`compile`/`build`/comptime paths remain
> strict, imported dependency modules do not inherit the privilege, and sealed
> capabilities remain strict unless an explicit mock backend exists. Plain tests
> instantiate under zero real host capability grants; the synthesized test
> `main` and compiled-backend reachability pruning keep unused effectful
> production functions out of the test artifact. The first mock capability
> backend, `testing.mock_dir`, is implemented for read-only in-memory `Dir[Read]`
> reads/lists/subtrees/file navigation in both tiers. Capability-parameterized
> tests run only under `witchy test --integration`; repeated `--dir` and `--net`
> flags feed the ordinary compiled runtime grant machinery. Package ownership
> comes from resolved manifest path dependencies and pinned lockfile vendor
> entries, so dependency tests receive zero real grants and linked functions
> cannot widen the synthesized entrypoint's authority.

## Summary

The guiding principle: **as long as the VM sandbox boundary is held, tests
should be a near free-for-all** — so you can forge values, substitute
collaborators, reach into internals, and trace execution to genuinely
stress-test your code. In-language guarantees (unforgeable capabilities,
invariant-guarding sealed types) are *production-correctness* contracts, NOT the
security boundary. The security boundary is the WASM VM: a test can construct
domain data in-language and still cannot touch the host beyond the capabilities
the VM was granted, because a real capability is a host-minted handle the guest
cannot manufacture — a forged domain value is inert data with no host reach.

This inverts the usual worry. We do NOT need to protect sealing from tests;
sealing was never the thing keeping the host safe. So the design is permissive:

1. **Non-sealed test doubles: already work.** Depend on a `trait` (or a
   `fn`-typed parameter), inject a fake impl in a test. This is witchy's
   mocking, today, no new machinery — a docs gap, not a language gap.
2. **Sealed domain-data doubles: allowed under the test runner.** The entry
   test module may construct foreign `sealed type` values — for example a
   `Version` with an arbitrary shape — *because doing so is safe*: the fake is
   data, and the VM boundary still holds.
3. **Mock capability constructors: explicit runtime work.** In-memory
   `testing.mock_dir([...])` now mints a read-only `Dir[Read]` backed by guest
   data, with the ordinary `read`/`exists`/`is_dir`/`subtree`/`list`/`read_file`
   surface and no real filesystem grant. Scripted `Clock`, mock `Net`/`Env`/
   `Rng`, and similar capability-typed test doubles still need explicit
   backends. Until a backend lands for a capability, sealed capability values
   remain strict even under `witchy test`.
4. **Real capabilities in tests: also allowed** (integration tier) for tests
   that want an actual effect.

One invariant governs all three, and it is about the VM, not the value:

> **A test can construct or inject domain data freely; it can still reach the
> host only through capabilities the VM was actually granted.** A mock
> capability grants power over its own in-memory state, never a real host
> effect, because the host mints and mediates every real handle.

## Why this is safe (the mechanical fact, verified)

`witchy test` runs each test COMPILED, in the WASM VM (`src/main.rs`:
`run_tests_in_module` → `compile_module_binary` → `run_wasm_bytes`), not through
the interpreter's direct-`std::fs` path. In the VM:

- A real capability is a **host externref handle**. The host creates it, hands it
  to the guest, and mediates every op on it. The guest cannot forge a handle;
  the *number* of real handles the VM holds is fixed by the grant, independent
  of anything the guest constructs.
- A test-constructed sealed domain value is plain heap data. Mock capabilities
  must likewise route to explicit in-memory backends (§2), never to real host
  authority, because no host function is linked for authority the VM was not
  granted.

So permitting in-language construction of sealed domain data changes NOTHING
about host safety. The test runner relaxes domain-data construction only; sealed
capabilities stay locked until mock backends are explicit runtime values.
Sealing stays enforced for production `run`/`compile` (where it IS the
correctness contract).

## Design

### 1. Non-sealed doubles — document, don't build

No language change. Write code against a `trait` (or `fn` param); inject a fake
impl in `test_*`. Verified today:

```witchy
trait Gateway:
    fn charge(self, cents: Int) -> String
type FakeGateway:
    canned: String
impl Gateway for FakeGateway:
    fn charge(self, cents: Int) -> String:
        self.canned
fn checkout(g: t, cents: Int) -> String where t: Gateway:   // depends on interface
    g.charge(cents)
// checkout(FakeGateway("FAKE-OK"), 500) type-checks and runs.
```

Add a book chapter ("Testing with collaborators / test doubles") teaching this
as the primary mocking story; CONTRIBUTING gains a lexicon line ("a test double
is an injected trait impl, not an interception"). This covers most real cases.

### 2. Sealed domain-data doubles under the test runner

Two test privileges the runner eventually grants to `test_*` code that plain
`run` does not:

- **Sealed domain-data construction** — under the test runner, entry `test_*`
  code may
  construct sealed domain types directly (`Version(-1, 0, 0)`, a `Set` with a
  duplicate, a `Url` with impossible fields). This is *deliberately permitted*:
  testing how your code handles a malformed `Version` is good testing, and the
  seal exists for production correctness, which the test is not. Provided as a
  test-mode privilege, not a general escape (production sealing is unchanged).
- **Mock capability constructors** — `testing.mock_dir([...])` is the first
  implemented one: a host-recognized in-memory backend on both backends that
  supports ordinary read-only `Dir[Read]` operations without granting the test
  VM a real filesystem root. Future `mock_clock(ms)`, `mock_net(responses)`,
  `mock_env([...])`, `mock_rng(seq)`, and similar capability-typed values must
  land with the same explicit backend and differential coverage.

Both are gated to the test runner (§3) and both are safe by §"Why this is safe":
a forged sealed domain value is inert data; a mock capability has no real host
handle.

### 3. Test-mode gating

The sealed-construction relaxation is available only when the entry ran through
`witchy test`, following the `mode opt` precedent (a linker-enforced mode —
`crates/witchy-syntax/src/linker.rs`). A production `run`/`check`/`compile`/
`build`/comptime path that constructs a sealed type from outside its module is
the same error it is today. Imported dependency modules linked into a test are
also production-strict; only the entry test module gets the privilege.

### 4. Real capabilities in tests (the integration tier)

Orthogonal to doubling. A `test_*` may declare capability parameters and, under
explicit `witchy test --integration [--dir …] [--net …]`, receive REAL authority
for tests that want a real effect. This runner split is implemented for `Dir`
and `Net`: plain `witchy test` stays zero real grant, parameterized tests fail
loudly unless integration mode and all required grants are present, and test
entrypoints execute as compiled WASM with their capability parameters forwarded.

**Supply-chain boundary = whose test it is.** `witchy test <dir>` sweeps every
`.witchy` including vendored dependencies. A **dependency's** swept tests always
run with zero REAL grant, even under `--integration` — you opt YOUR tests into
real authority, never a dependency's. This is the one place the free-for-all is
bounded, and correctly: forging values and mocks is harmless (data, no host
reach), but a *real* granted `Dir`/`Net` is genuine authority, so a malicious
dep's test must never get one. Mocks/forgery need no such bound — they can't
breach the VM regardless of whose code builds them.

### 5. The invariant, made mechanical

- Non-sealed double: an ordinary value with exactly its interface's methods —
  can't exceed them (automatic).
- Mock capability / forged sealed domain value: heap data + a type tag, no real
  host handle — the VM's real-grant set is independent of it. `testing.mock_dir`
  is mediated by a host `externref`, but that backing contains only the guest's
  own in-memory path/content map. Even in hostile code it touches only its own
  heap/mock state or is inert data.
- In the completed runner split, dependency-swept tests get zero REAL grant
  regardless of flags (§4).

There is no configuration in which an in-language construction yields a real host
effect the VM was not granted. That is the whole safety argument, and it rests on
the VM, not on the type system policing what tests may build.

## Alternatives

### Keep sealing enforced even in tests (the previous draft)

Rejected in favor of the permissive model. The earlier draft guarded sealing as
if it were the security boundary and refused to let tests forge sealed types.
But sealing is a production-correctness contract; the VM sandbox is the security
boundary, and it holds regardless of what a test constructs (§"Why this is
safe"). Refusing test-time forgery costs real testing power (you couldn't feed
your code a malformed `Version` to see it cope) for zero security gain. So:
permit it, gated to the test runner.

### Ruby/RSpec-style monkeypatching (dynamic-dispatch interception)

Still rejected, but now for an ergonomic/consistency reason, not a security one.
Top-level functions are statically bound (verified: `greet = fn(): …` is
"assignment to unbound variable"), so there is no dispatch seam to interpose on;
adding one only for tests would make tests exercise a different dispatch model
than production. The permissive model gives the *power* people want mocking for
(substitute anything, including sealed things) via injection + test-mode
construction, without a second dispatch mechanism. Note the security objection
from the previous draft no longer applies — interception would be safe too under
the VM-boundary argument — it is simply unnecessary and inconsistent with static
dispatch.

### Make capabilities ordinary traits so injection covers everything

Rejected: user-writable `impl Dir` is forgeable capabilities in PRODUCTION,
defeating RFC-0005. The test-mode relaxation (§2) gets the test-time benefit
without opening production.

## Drawbacks

- Mock capability backends are real runtime work per capability, both backends,
  differential-tested.
- A test-mode sealed-construction relaxation is a new linker privilege to
  implement and bound carefully (it must be UNavailable in every non-test entry
  path, including comptime and build steps).
- Mock fidelity is a maintenance surface (a `mock_dir` drifting from real `Dir`
  semantics gives false confidence) — define against the RFC-0044 contract.
- Two test tiers (plain vs `--integration`) is surface to learn.

## Ordering

1. **Docs first, zero code:** the "testing with collaborators" chapter (§1) —
   ships immediately, likely satisfies most demand.
2. Sealed domain-data relaxation (§2, construction) + test-mode gate (§3).
   Implemented 2026-07-13 for the entry module only.
3. Confinement split (§4) — runner work; hardens the dependency-test real-grant
   floor explicitly. Implemented 2026-07-15 for explicit `Dir`/`Net` grants.
4. Mock capability backends (§2, capabilities).
5. Lock the `witchy test` surface with RFC-0072 goldens.

## Riders (acceptance conditions, 2026-07-08)

1. **Mock capabilities require a runner grant split.** The first implemented
   slice constructs only sealed domain data and keeps sealed capabilities
   strict. Mock `Dir`/`Clock`/`Net`/`Env`/`Rng` values need explicit in-memory
   backends plus a plain-test path with zero real host grants; integration tests
   are the separate path that deliberately receives real authority.
2. **The test-mode gate must be closed against every non-test entry**, and
   its absence goldened (RFC-0072): `run`, `compile`, `build` steps, comptime
   evaluation, and `pm`-driven builds each get a golden proving `testing.*`
   and sealed-construction-outside-home are rejected there. The gate is a
   linker attribute; the goldens are what keep it from silently widening.

## Residual runtime note (2026-07-13)

The 2026-07-09 deferral was about a pre-RFC-0005 hazard: root capabilities were
guest `i32` table indexes, so relaxing sealed capabilities under a broadly
granted test VM could collide with real host handles. RFC-0005's externref
migration removes that representation hazard for root capabilities, but it does
not by itself implement mock capability semantics. `testing.mock_dir` is the
first explicit backend; other capability mocks remain production-strict until
their backends exist.

Resume the runtime portions only after all of these are true:

1. Plain nullary tests instantiate under a genuinely authority-free capability
   set, and compiled-backend reachability pruning proves unused effectful
   production functions cannot receive ambient test authority. Implemented for
   the current `witchy test` runner.
2. Mock capabilities have explicit in-memory backends on the compiled path, with
   behavior tested against the real capability contracts.
3. Integration tests remain an opt-in path for real authority, and dependency
   tests cannot receive those grants. Implemented 2026-07-15 for `Dir`/`Net`.
4. Negative end-to-end tests prove that a plain test cannot read cwd, inspect
   ambient environment, use randomness/time, or reach the network, and that the
   test-only linker privilege is absent from every production/build path.

The implemented sealed-domain-data relaxation is unaffected: it constructs
ordinary heap data, not host authority.

## Prior art

Go — interface injection for user types (§1) + in-memory fakes for the std
library's sealed surfaces (`fstest.MapFS`, `net`/`httptest`) — direct precedent
for §2's mock capabilities. Rust — `#[cfg(test)]` gating + trait-object
injection; test code may also reach `pub(crate)` internals a production consumer
cannot, which is the same "tests are more permissive than production, safely"
principle as §2's sealed relaxation. Ruby/RSpec — dynamic interception (the
rejected model).
