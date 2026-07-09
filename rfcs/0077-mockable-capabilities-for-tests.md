---
rfc: 0077
title: "Test doubles in witchy — tests are permissive; the VM sandbox is the only boundary"
status: proposed
created: 2026-07-08
tracking: design conversation 2026-07-08 (witchy test ergonomics + capability model); no prior test-double RFC
related:
  - "0002 (user-definable capabilities — sealing is a correctness contract, not the security boundary)"
  - "0005 (unforgeable capabilities — production invariant; the VM, not the seal, is the perimeter)"
  - "0013 (capability grant documents — how real authority is granted at run)"
  - "0044 (contract rules — a fake's failure shapes should match the real one's)"
  - "0046 / 0050 (trait bounds + method dispatch — the injection seam this rests on)"
  - "0072 (diagnostic goldens — lock the test-runner surface once this lands)"
---

# RFC-0077: Test doubles in witchy

## Summary

The guiding principle: **as long as the VM sandbox boundary is held, tests
should be a near free-for-all** — so you can forge values, substitute
collaborators, reach into internals, and trace execution to genuinely
stress-test your code. In-language guarantees (unforgeable capabilities,
invariant-guarding sealed types) are *production-correctness* contracts, NOT the
security boundary. The security boundary is the WASM VM: a test can construct
whatever it likes in-language and still cannot touch the host beyond the
capabilities the VM was granted, because a real capability is a host-minted
handle the guest cannot manufacture — a forged value is inert data with no
host reach.

This inverts the usual worry. We do NOT need to protect sealing from tests;
sealing was never the thing keeping the host safe. So the design is permissive:

1. **Non-sealed test doubles: already work.** Depend on a `trait` (or a
   `fn`-typed parameter), inject a fake impl in a test. This is witchy's
   mocking, today, no new machinery — a docs gap, not a language gap.
2. **Sealed-type doubles (capabilities AND sealed domain types): allowed under
   the test runner.** `std/testing` may mint fakes of sealed types — an
   in-memory `Dir`, a scripted `Clock`, a `Version` with an arbitrary shape —
   *because doing so is safe*: the fake is data, and the VM boundary still holds.
3. **Real capabilities in tests: also allowed** (integration tier) for tests
   that want an actual effect.

One invariant governs all three, and it is about the VM, not the value:

> **A test can construct or inject anything in-language; it can still reach the
> host only through capabilities the VM was actually granted.** A forged or
> mock capability grants power over its own in-memory state, never a real host
> effect — not because the language forbids constructing it, but because the
> host mints and mediates every real handle, and a guest-constructed value has
> none.

## Why this is safe (the mechanical fact, verified)

`witchy test` runs each test COMPILED, in the WASM VM (`src/main.rs`:
`run_tests_in_module` → `compile_module_binary` → `run_wasm_bytes`), not through
the interpreter's direct-`std::fs` path. In the VM:

- A real capability is a **host externref handle**. The host creates it, hands it
  to the guest, and mediates every op on it. The guest cannot forge a handle;
  the *number* of real handles the VM holds is fixed by the grant, independent
  of anything the guest constructs.
- A guest-constructed "capability" — a mock `Dir`, or even a forged sealed value
  if we permit it — is a plain heap value with a capability *type tag* but no
  host handle. A `write` against it routes to the in-memory backend (§2), never
  to a host function, because no host function is linked for authority the VM
  wasn't granted.

So permitting in-language forgery of sealed values changes NOTHING about host
safety. The interpreter's `Value::Dir(path, …)` → `std::fs::write` path (which a
forged value *could* abuse) is not the test execution path; the VM is, and the
VM's safety comes from handle-mediation, not from the type system refusing to
build the value. Sealing stays enforced for production `run`/`compile` (where it
IS the correctness contract); the test runner relaxes it.

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

### 2. Sealed-type doubles under the test runner

Two capabilities the test runner grants to `test_*` code that plain `run` does
not:

- **Mock capability constructors** — `testing.mock_dir([...])`,
  `mock_clock(ms)`, `mock_net(responses)`, `mock_env([...])`, `mock_rng(seq)`:
  capability-typed values backed by in-memory state. The code under test takes
  an ordinary `Dir[Read]` and is unaware. Host-recognized in-memory backends on
  both backends (real runtime work, per capability).
- **Sealed-construction relaxation** — under the test runner, `test_*` code may
  construct sealed domain types directly (`Version(-1, 0, 0)`, a `Set` with a
  duplicate, a `Url` with impossible fields). This is *deliberately permitted*:
  testing how your code handles a malformed `Version` is good testing, and the
  seal exists for production correctness, which the test is not. Provided as a
  test-mode privilege, not a general escape (production sealing is unchanged).

Both are gated to the test runner (§3) and both are safe by §"Why this is safe":
a forged sealed value is inert data; a mock capability has no host handle.

### 3. Test-mode gating

`std/testing` and the sealed-construction relaxation are available only when the
entry ran through `witchy test`, following the `mode opt` precedent (a
transitive, linker-enforced attribute — `crates/witchy-syntax/src/linker.rs`). A
production `run`/`compile`/`build` that imports `testing` or constructs a sealed
type from outside its module is the same error it is today. So none of this
surface exists in a shipped artifact.

### 4. Real capabilities in tests (the integration tier)

Orthogonal to doubling. A `test_*` may declare capability parameters and, under
explicit `witchy test --integration [--dir …] [--net …]`, receive REAL authority
for tests that want a real effect. Plain `witchy test` stays zero real grant.

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
- Mock capability / forged sealed value: heap data + a type tag, no host handle
  — the VM's real-grant set is independent of it. Even in hostile code it
  touches only its own heap or is inert data.
- Dependency-swept tests get zero REAL grant regardless of flags (§4).

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
2. Confinement split (§4) — runner work; hardens the dependency-test real-grant
   floor explicitly.
3. Sealed relaxation (§2, construction) + mock backends (§2, capabilities) +
   test-mode gate (§3).
4. Lock the `witchy test` surface with RFC-0072 goldens.

## Prior art

Go — interface injection for user types (§1) + in-memory fakes for the std
library's sealed surfaces (`fstest.MapFS`, `net`/`httptest`) — direct precedent
for §2's mock capabilities. Rust — `#[cfg(test)]` gating + trait-object
injection; test code may also reach `pub(crate)` internals a production consumer
cannot, which is the same "tests are more permissive than production, safely"
principle as §2's sealed relaxation. Ruby/RSpec — dynamic interception (the
rejected model).
