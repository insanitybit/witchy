---
status: implemented
note: Imported from docs/ under RFC-0001. Frozen design record — current behavior lives in spec/ and the code.
---

# Secrets & encryption: the witchy design

> **2026-06-23 — status correction.** This RFC is `implemented`. The near-term
> design (`SecretStore` capability + `Secret` handle, `get`/`require`/
> `crypto.reveal`, `--secret`/`--secret-file`/`--signing-key`) **shipped on both
> backends and is parity-tested** — see the "Implementation status (shipped)"
> section below. Two later in-body status lines are therefore stale: the
> "Status: DESIGN (not yet implemented)" note immediately below, and the
> "Implementation status (2026-06-18) … REMAINING (the WASM backend + CLI +
> migration)" section near the end (that list is all done). Only the longer-term
> `secrets` library (sealed types, `Redacted`, facets) is still unbuilt. Body
> left intact as the historical record per rfcs/README.

> Status: DESIGN (not yet implemented). This document consolidates the approach;
> code blocks use provisional syntax and are intentionally NOT tagged `witchy`
> (so the doc-examples test does not try to compile them).

## Why a new approach

witchy already has a capability called `Secret`. It is too specific: it is really an
**Ed25519 signing key** (`Value::Secret([u8;32])`, granted via `--signing-key`, used by
`crypto.sign`). Its whole point is that it confers the *power to sign* while the key
bytes never enter guest memory. That is genuine authority — but it monopolizes the word
"secret", and it does not address the things people usually mean by a secret: a password
from an env var, an API token from a file, a value fetched from a vault.

The realization that drives this design: **"secret" is an umbrella over three orthogonal
concerns.** Conflating them is why the current model feels off.

| Axis | Concern | Right mechanism |
|---|---|---|
| **Authority** | use a key you can't read | a capability (`SigningKey`) |
| **Redaction** | don't print / log it | a stdlib value type (`Redacted`) |
| **Memory lifetime** | don't let it linger in RAM | a scoped block (`secret:`) |

(The memory-lifetime axis mirrors Go 1.26's `runtime/secret`: secrecy as a runtime
guarantee about *all temporary storage* a block touches, not a property of one value.)

## Near-term minimal design (recommended): `SecretStore` + `Secret` with operations

The full library-defined-capability model below is the north star. The **practical
near-term step** — and all coven actually needs — is smaller and does not add any new
"capability language" machinery. It just generalizes today's single `Secret` (a lone
signing key from `--signing-key`) into a small store of *named* secrets.

**CLI — replace `--signing-key` with named secrets (repeatable):**
```
--secret <name>=<value>        # inline (e.g. a hex seed, an API token)
--secret-file <name>=<path>    # load the secret's bytes from a file
```

**`SecretStore`** — a host capability granted to `main`, holding the named secrets:
```
fn main(..., secrets: SecretStore):
    match secrets.get("signing"):       # -> Option(Secret)
        Some(key) -> ...
        None -> fail("no `signing` secret was granted")
```

**`Secret`** — a single named secret, obtained from the store, with operations (key
bytes stay host-side, keyed by a handle — never copied into guest memory):
```
key.sign(msg)          # -> String (hex Ed25519 signature); also crypto.sign(key, msg)
key.public_key()       # -> String (hex public key); also crypto.public_key(key)
key.reveal()           # -> String (the raw value — for value secrets like tokens/passwords)
```

**Footprint / capability model:** `SecretStore` is the host capability that shows up in
footprints (like `Dir`/`Net`). `Secret` is a value you *get* from it, not separately
granted — and *which* names are available is a launch grant, not part of the type
(consistent with witchy's "kind in the type, the specific resource at launch"). Keep it
unfussy: `reveal()` returns a plain `String` for now; the `Redacted` type and facets are
later refinements, not required here.

## Implementation status (shipped — both backends, parity-tested)

`SecretStore` is a host capability (`src/capabilities.rs`); a `Secret` is a value you get
from it. A `Secret` is raw granted bytes held host-side and never copied into guest
memory — on WASM it is an i32 **handle** into a host-side secret table; the bytes only
cross the boundary through `crypto.reveal`. Signing parity holds: the same seed yields the
same signature on both backends.

**Reading secrets** (`std/secretstore.witchy`):
- `secrets.get(name) -> Option(Secret)` — the named secret, or `None` if not granted.
- `secrets.require(name) -> Secret` — a *required* secret (no `Option`); fails loudly if
  absent. Used for things like a server's root signing key.

**Operating on a `Secret`** (`std/crypto.witchy`) — callable module-qualified or as a
method (the receiver type `Secret` resolves to the `crypto` module):
- `crypto.sign(key, msg)` / `key.sign(msg)` — hex Ed25519 signature.
- `crypto.public_key(key)` / `key.public_key()` — hex public key.
- `crypto.reveal(key)` / `key.reveal()` — the raw bytes as a `String`, for value secrets
  (tokens, passwords) handed to an external sink. `sign`/`public_key` interpret the bytes
  as a hex (or 32-raw-byte) Ed25519 seed.

**Granting secrets at launch** (repeatable):
```
--secret <name>=<value>        # inline (e.g. an API token, a password)
--secret-file <name>=<path>    # the secret's bytes from a file
--signing-key <path>           # sugar for --secret-file signing=<path> (the `signing` slot, handle 0)
```
A `Secret` `main` parameter (always handle 0) and `SecretStore.get("signing")` therefore
agree. A program whose `main` binds `Secret`/`SecretStore` requires at least one granted
secret, else it is refused before running.

**coven** (`projects/coven`) dogfoods this: `main(console, net, root, secrets:
SecretStore, clock, args)` does `secrets.require("signing")` and signs records with that
`Secret`. It launches with `--signing-key root.seed` (the sugar), so existing launchers
and the e2e suite keep working.

`reveal()` returns a plain `String`; the `Redacted` type and facets below remain later
refinements, not required here.

## Core thesis: libraries mint capabilities, the language doesn't

Do not bake a `Secrets` capability into the language. Instead, **let libraries define
their own capabilities** — the object-capability ideal: no privileged built-in set, any
sealed value can be a capability, and a `secrets` library mints domain-specific ones.
This matches witchy's own north stars (stdlib-not-builtins, generic-over-special,
minimize built-ins), and it generalizes beyond secrets (a `db` rune can define
`Db[Query|Migrate]`, an http client `HttpClient[host=...]`, etc.).

**The non-negotiable invariant that keeps it sound:** a library may *attenuate / compose*
authority into a new opaque cap type, but may **never mint authority from nothing**.
Constructing a cap must *consume* host roots (`secrets.connect(net, root)`), so its
authority is rooted in caps the caller already held. The fixed host capability set stays
the trust anchor; everything else is library-derived.

## What witchy already has

Most of the safety machinery exists today:
- **Branded capabilities** — a one-field user type wrapping a host cap — with a footprint
  analysis that *sees through* the brand to the underlying host cap (tested:
  `a_branded_capability_cannot_hide_a_widening`, `library_cannot_fabricate_a_capability`).
- **Capability narrowing** (`cap as Dir[Read]`) and the **capability firewall**
  (`retain` / `without`).
- **Computed footprints** (coven): authority is computed from *use*, so a library cannot
  understate the `Net` it actually exercises.

## The minimal delta: two general primitives

Everything in this design rides on adding exactly two primitives (neither is
secrets-specific):

1. **Sealed types** — a type whose constructor is PRIVATE to its defining module, so the
   module is the sole, unforgeable minter. This promotes today's implicit "branded
   capability" into a first-class feature and is the unforgeability anchor.
2. **Capabilities as storable / sendable values** — a cap can live in a struct field and
   cross spawn/channel boundaries (servers, pools, concurrent fetches). This is the real
   usability blocker today (caps-in-messages is currently a LOUD gap).

Everything else below is *library code*, not language features.

## The library: a `Secrets` / `SecretsManager` capability

A `secrets` library defines a sealed `SecretsManager` capability constructed from host
roots. It is **KMS-shaped**: keys are referenced by handle and operations are performed
*by the service* — `sign`, `public_key`, `decrypt`, `hmac`, `derive`, and `reveal`.

**`reveal` is the dividing operation:**
- A signing/encryption key is registered WITHOUT `reveal` — you use it, you never read
  it ("use a key you can't read"). This is AWS-KMS-shaped.
- A value secret (DB password, API token) is registered WITH `reveal`, because the
  program must emit the bytes to an external sink (a connection string, an HTTP header).
  This is AWS-Secrets-Manager-shaped. For these, "can't read" is impossible by
  definition, so `reveal` is the dangerous op you narrow away whenever you can.

So **KMS and Secrets Manager are the same capability**, distinguished by whether `reveal`
is granted and what backend the host wires up.

### Attenuation = the facet pattern (not a rights lattice)

To hand a less-trusted dependency "sign-only", the library returns a *narrower sealed
type* exposing fewer methods:

```
let signer = sm.signer_for("release/*")   # a Signer facet: only .sign(), scoped
publisher.run(signer)                      # publisher gets a Signer, NOT sm
```

Least authority is then enforced by **ordinary type-checking** — `publisher` has no
`reveal` method to call, cannot reconstruct `sm` (sealed), and `sign` refuses keys
outside its scope. No rights-lattice syntax, no scope-in-types. (Scope strings are
runtime arguments — consistent with witchy's existing "kind in the type, the specific
resource is a launch/runtime grant".)

### Provisioning: the app fetches dynamically

Do NOT enumerate every secret as a launch flag — real apps decide *which* secret *when*
at runtime (per-tenant, per-request, on rotation). `SecretsManager` is a **live client**:
the launcher configures only the backend + root creds + a scope ceiling (the namespace
the app may touch); the app fetches by name at runtime.

```
let token = sm.reveal("tenant/${req.tenant}/api-token")?   # runtime name, may fail
```

Remote backends (Vault / AWS Secrets Manager) make ops fallible and possibly async, so
`Secrets` operations return `Result` / `T!`. The host does the network call with the
host's creds, so the app never holds the backend's root creds or `Net` for it.

### Redaction and lifetime, as library/scope mechanisms

- `reveal` returns a value typed **`Redacted`** — a sealed type with NO `Show` impl, so it
  can't be interpolated or logged; an explicit `.expose()` is the one greppable escape
  hatch, used only at the sink that needs the bytes.
- For the memory-lifetime axis, a **`secret:` block** would guarantee the storage it
  touches is wiped on exit (the hardest, lowest-priority piece — non-observable, and
  hard to make parity-equal across the interpreter and WASM backends).

### Policy is library code

Revocation (lease/TTL), quotas, time windows, and confinement are NOT language features —
they are runtime checks the sealed cap performs (the caretaker pattern):

```
let (signer, revoke) = sm.revocable_signer("release/*")
publisher.run(signer)
revoke()        # signer.sign() now returns Err — pure library, no language support
```

## How the three axes compose

`Secrets` (authority) governs *whether* you may fetch/reveal and *which* namespace →
`reveal` returns a `Redacted` value (redaction) → consumed inside a `secret:` scope
(lifetime). One coherent story; each axis is a separate, composable mechanism.

## Worked example: HTTPS server + Postgres over mTLS

This exercises the whole design — three library caps, sealed TLS identities whose private
keys are never revealed, a `Redacted` password, narrowing, a stored pool, concurrent
startup, and the error model.

```
import server
import postgres
import secrets

type App:
    db: postgres.Pool            # a capability stored in app state (primitive #2)

fn main(net: Net, env: Env):
    let sm = secrets.from_env(env)

    # load every startup secret concurrently; one grouped error if any fail
    let (db_id, db_ca, db_pw, web_id) = gather:
        spawn sm.tls_identity("db/client-cert", "db/client-key") ? "db client identity"
        spawn sm.trust_anchor("db/ca-cert")                      ? "db CA"
        spawn sm.reveal("db/password")                           ? "db password"
        spawn sm.tls_identity("web/server-cert", "web/server-key") ? "web identity"
    ?

    # Postgres pool over MUTUAL TLS + user/password. db_pw is Redacted, exposed
    # only to the driver. The TLS private keys never leave the secrets service.
    let db = postgres.connect_pool(net as Net[Connect], postgres.Config(
        host: "db.internal:5432",
        user: "app",
        password: db_pw,
        tls: postgres.mtls(db_id, db_ca),
        pool_size: 16,
    ))? "connecting to postgres over mTLS"

    # handle() receives App, NOT Net — request code literally cannot open sockets.
    server.https(net as Net[Listen], web_id).serve(App(db), handle)? "starting HTTPS server"

fn handle(app: App, req: Request) -> Response!:
    let id = req.path_param("id")
    let user = app.db.query_one("select name, email from users where id = $1", [int(id)])? "loading user ${id}"
    server.json(200, user)
```

## How a library AUTHOR defines the capability

```
# rune: secrets
capability SecretsManager:           # declares the cap + its rights vocabulary
    rights: Sign, Decrypt, CreateSecret, Reveal

sealed type SecretsManager:          # only this module can construct it
    client: Client
    root: Secret

# SEALED constructor must CONSUME host roots — authority is rooted, never minted.
pub fn connect(n: Net[Connect], root: Secret) -> SecretsManager!:
    Ok(SecretsManager(net.dial(n, "kms:443")? "connecting to KMS", root))

impl SecretsManager:
    fn sign(self, name: String, msg: String) -> String!: ...        # no reveal
    fn reveal(self, name: String) -> Redacted!: ...                 # the dangerous op
    fn signer_for(self, scope: String) -> Signer: Signer(self, scope)  # a facet
```

## Security properties — and honest limits

**Wins:** kills ambient-authority supply-chain attacks (a rune with no cap in its
footprint cannot touch net/fs/env); least authority at re-delegation; `Redacted` kills a
whole class of secret-in-logs incidents; one API over env/file/Vault/KMS backends;
mechanically auditable footprints.

**Limits to be honest about:**
- **Re-delegation, not origination.** The app that owns the roots keeps them (the
  see-through footprint shows `Net`/`Secret` even when wrapped). The win is containing
  what you pass *onward* to less-trusted code, not the app magically lacking authority.
- **Trust is relocated, not eliminated.** A malicious cap-library can be a *confused
  deputy* (offer a `proxy(url)` op that uses its inner `Net` on the caller's behalf). The
  footprint catches *unwrapping*, not deputy operations. Cap-minting libraries become
  high-value targets you must still trust and minimize.
- **No built-in revocation / TTL / quota.** These are library responsibilities; the
  footprint audits *kind* of authority, not *extent* or *policy*. (Prior art to borrow:
  macaroons — attenuable bearer creds with caveats.)
- **Scope checks are runtime, not compile-time** (names are dynamic), consistent with
  witchy's "extent is a runtime grant" model.

## Relationship to today's `Secret`

Rename the current `Secret` capability to **`SigningKey`** (it is authority over a key,
nothing more). It then becomes one library-minted root among others — the `secrets`
library builds `SecretsManager` on top of `SigningKey` + `Net`. The generic word "secret"
stops naming a single language feature.

## Implementation status (2026-06-18)

DONE on the **interpreter backend** (build + 990 tests green, additive — `crypto.sign`
and the existing `Secret` param still work):
- `SecretStore` is a registered capability type (`capabilities.rs`, `typeck.rs`).
- `Value::Secret` generalized to raw bytes; `Value::SecretStore` added and granted to a
  `main(secrets: SecretStore)`; `secretstore.get(name) -> Option(Secret)` (interpreter-
  intercepted, since a `SecretStore` is not a `NativeValue`).
- `Secret` operations: `crypto.sign`/`crypto.public_key` normalize a seed (32 raw or 64
  hex bytes), new `crypto.reveal`; method routing so `key.sign(msg)`/`.public_key()`/
  `.reveal()` and `store.get(name)` resolve. `std/secretstore.witchy` + docs.
- The granted signing key is exposed as the `signing` secret, so `get("signing")` works.

REMAINING (the WASM backend + CLI + migration):
- **WASM backend** — coven runs on WASM (`compiler.footprint` is a WASM host import), and
  the run CLI is WASM-only, so `SecretStore` needs WASM support: a host secrets table +
  host imports for `secretstore.get` / `secret.reveal` (sign/public_key already exist as
  host ops), and WIR-codegen lowering. This is the linchpin for coven and is deep codegen
  work — do it as a focused pass, not rushed.
- **CLI** `--secret`/`--secret-file` parsing + the value-secret map plumbing (inert until
  the WASM backend lands, since the run CLI is WASM-only).
- **Migrate coven** to `secrets.get("signing")` once the WASM backend is in.

## Roadmap

1. **Sealed types** (private constructors) — the unforgeability primitive.
2. **Caps as storable / sendable values** — close the caps-in-messages gap.
3. Ship `secrets` as a *library* against (1)+(2): `SecretsManager`, facets, `Redacted`,
   backend constructors (env/file/KMS), policy/revocation as runtime code.
4. (Later, north star) capability/effect polymorphism — footprint-as-inferred-type,
   fronted by comptime `check_type` predicates with custom compile-time errors. Blocked on
   the comptime→typecheck→footprint phase ordering; both Zig and Rust ship types→values
   and defer values→types, which validates deferring this half.
5. (Someday, hard) the `secret:` memory-erasure block — non-observable, hard to make
   parity-equal across backends; lowest priority.

## Validation

The library-defined-capability model was checked against a second, unrelated domain
(Database/Postgres) and a capstone (HTTPS + Postgres over mTLS) — both fall out of the
same two primitives with zero new language machinery, which is the evidence the design is
general rather than secrets-specific.
