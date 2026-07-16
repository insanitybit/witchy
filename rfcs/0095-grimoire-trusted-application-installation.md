---
rfc: 0095
title: "Grimoire and trusted application installation from Coven"
status: proposed
created: 2026-07-15
superseded-by:
tracking: >
  Proposed. Rename the self-hosted package-manager application to Grimoire,
  ship it through the trusted-executable model, and extend Coven releases with
  signed target-specific application artifacts consumed by `witchy install`.
related:
  - "0004 (self-hosted CLI — Grimoire remains the package-management frontend)"
  - "0005 (unforgeable capabilities — Grimoire delegates authority explicitly)"
  - "0013 (grant documents — portable applications remain consumer-granted)"
  - "0061 (release versioning — immutable promoted releases and provenance)"
  - "0092 (trusted application executables — the artifact installed here)"
---

# RFC-0095: Grimoire and trusted application installation from Coven

## Summary

Turn Witchy's self-hosted package manager into a first-class trusted
application named **Grimoire**, and add a Cargo-like installation path for
trusted Witchy applications:

```sh
witchy install acme/wrg
wrg pattern ./src
```

Coven stores publisher-built `trusted-exe` artifacts for concrete target
triples alongside a rune's source release. The promoted, signed release record
commits to the complete artifact manifest. `witchy install` resolves a released
version, selects the current host target, verifies the existing Coven trust
chain and the executable's digest, and atomically places it under
`$WITCHY_HOME/bin` without executing it.

The package-management implementation is called **Grimoire**. It continues to
ship inside the trusted `witchy` bootstrap and is also buildable as its own
trusted executable. The primary integrated commands remain concise top-level
commands such as `witchy add`, `witchy install`, and `witchy publish`;
`witchy pm` remains a compatibility entry during the rename. This RFC does not
standardize `grim` as an executable alias.

## Motivation

### `trusted-exe` solved the artifact, not discovery or installation

RFC-0092 made it possible to build one ordinary executable containing a Witchy
application, its compiled WASM, and the Witchy runtime. That closes the runtime
UX gap: an installed application needs neither Witchy nor launch-time grant
flags.

The distribution loop is still manual. An author must choose somewhere to host
each platform binary, publish checksums separately, explain which file matches
which machine, and write an installer. A recipient must decide whether those
pieces refer to the same release. `curl | bash` is an acceptable transport, but
it should not be the only integrated story Witchy can offer.

Coven already has the harder half of this problem:

- immutable package coordinates;
- staged versus promoted releases;
- publisher and promoter identity separation;
- signed release records and TUF-style metadata;
- rollback, freeze, and freshness defenses;
- content verification before project dependencies are materialized.

Target-specific executable artifacts should reuse that release identity and
trust chain instead of creating a second application registry.

### Installing an application is not adding a dependency

The existing `add` operation is deliberately inert. It resolves verified
source, vendors it into a project, and never executes package code. Its
capability-widening gate answers: "what authority could this dependency demand
from my application?"

Installing a trusted application is a different action:

| operation | installed material | execution trust decision |
|---|---|---|
| `witchy add acme/json` | verified source in the current project | none at install time; the root application still controls delegation |
| `witchy install acme/wrg` | publisher-built native executable in the user's bin directory | running the executable trusts the complete artifact |

Conflating these operations would weaken both stories. `add` must not start
installing or executing native programs. `install` must not imply that a
trusted executable remains confined by a consumer's launch grants.

### The package manager is itself a trusted application

A useful package manager needs real authority. It reads and writes projects,
contacts registries, writes an installation directory, reads authentication
material, and may invoke the Witchy compiler for build, run, test, and doc
commands. Asking users to spell grants for those resources on every invocation
would recreate the friction RFC-0092 removed.

Grimoire is therefore an intended `trusted-exe` consumer. Its root receives
only the resources selected by its trusted target binding plan. Capability
typing remains valuable inside it: a semver resolver receives no `Dir`, an
artifact verifier receives bytes but no `Exec`, and a dependency receives a
real capability only when Grimoire deliberately passes one.

The trusted `witchy` bootstrap continues to embed the same Grimoire source.
This avoids a circular requirement that Grimoire already be installed before a
user can install Grimoire. The standalone artifact is dogfood and an optional
independent distribution, not the only bootstrap.

### The trust statement must be honest

For an installed application, the user trusts:

1. the local Witchy/Grimoire installer to enforce resolution and verification;
2. the pinned Coven metadata root and its signed release state;
3. the publisher and promoter identities named by that release; and
4. the complete downloaded native executable when they later run it.

An untrusted mirror does not need to be trusted: signed coordinates and the
artifact digest bind its response. Coven binding source and binaries to one
release does **not** prove that a publisher-built binary was reproducibly
derived from the accompanying source. The initial design reports provenance
and verifies identity; it does not claim source-to-binary reproducibility it
cannot establish.

## Design

### 1. Name and command surface

The self-hosted package-management application is named **Grimoire**. Source,
documentation, cache keys, and diagnostics migrate from the generic `pm` name
to `grimoire` when implementation begins.

The normal integrated UX is top-level:

```sh
witchy add acme/json
witchy install acme/wrg
witchy publish
witchy update
```

The standalone trusted application exposes the same frontend:

```sh
grimoire add acme/json
grimoire install acme/wrg
```

`witchy pm <verb>` remains a compatibility alias during the migration and must
execute the same Grimoire module, not a forked implementation. An optional
`witchy grimoire <verb>` spelling may exist for discoverability, but it is not
required when a top-level verb is unambiguous.

The short executable name `grim` is not reserved by this RFC. A global command
alias is a compatibility commitment and has existing ecosystem collision risk;
shell aliases and downstream package names remain outside the language design.

### 2. Installation UX

The initial command is:

```text
witchy install [--registry <url>] [--force] <name>[@<version-requirement>]
```

Examples:

```sh
witchy install acme/wrg
witchy install acme/wrg@1.4.2
COVEN_URL=http://127.0.0.1:8787 witchy install acme/wrg
```

The default requirement is the highest released version eligible under the
existing staging-cooldown policy. `--registry` overrides registry discovery for
this invocation. The installer selects the host target itself; package data
cannot persuade an x86-64 host to install an AArch64 artifact.

The installation root is:

```text
$WITCHY_HOME/bin
```

where `WITCHY_HOME` defaults to `$HOME/.witchy` on Unix and the corresponding
per-user Witchy data root on other hosts. The trusted launch machinery resolves
this directory into a real `Dir` capability before Grimoire starts. Grimoire
does not turn an arbitrary environment string into ambient filesystem
authority.

`install` prints the coordinate, selected target, publisher/promoter identity,
artifact digest, declared trusted-root bindings when available, and final path.
The explicit `install` command is the trust action; there is no interactive
permission prompt that would make scripts hang or imply that individual
capabilities can confine a malicious native artifact.

If the destination already exists, installation fails unless `--force` was
provided. `--force` still performs complete verification before replacement.
It never weakens target selection, release-state, signature, or digest checks.

The initial RFC installs one command per rune release. Bundles containing
multiple commands, desktop assets, services, or platform installers are future
work.

### 3. Grimoire's trusted target bindings

The standalone Grimoire application's root needs these authority classes:

| parameter | purpose | binding |
|---|---|---|
| project `Dir` | project-local add/build/update work | launch cwd |
| install `Dir` | atomically place applications and receipts | `$WITCHY_HOME/bin` provider |
| toolchain `Dir[Read]` | name the Witchy compiler for delegated `Exec` | executable/toolchain directory provider |
| `Net` | contact the configured Coven registry | explicit system network policy for the trusted distribution |
| `Exec` | drive the colocated Witchy compiler for build/run/test/doc | allow only the `witchy` program through the toolchain `Dir` |
| `Env`, `Clock`, `Console`, argv | configuration, cooldowns, diagnostics | conventional process providers |

This requires two additive directory providers in the RFC-0092 binding
vocabulary:

```toml
[targets.trusted-exe.dirs]
project = { from = "cwd" }
install = { from = "witchy-home", path = "bin" }
toolchain = { from = "exe-dir" }

[targets.trusted-exe.net]
registry = { from = "system" }

[targets.trusted-exe.exec]
runner = { from = "allow", programs = ["witchy"] }
```

`witchy-home` resolves the user's configured Witchy home and a confined relative
subpath. `exe-dir` resolves the canonical directory containing the launcher.
Neither provider reads application argv or searches arbitrary paths. Missing,
unwritable, or incompatible bindings fail at startup with an actionable
diagnostic.

The embedded bootstrap must resolve the same logical providers through the
existing runtime grant machinery. There must not be one permissive Rust grant
implementation for `witchy install` and a different restrictive implementation
for standalone Grimoire.

Grimoire's root holds the union needed by its command set, but command handlers
must pass narrower values to helpers and dependencies. In particular, artifact
parsing and digest verification do not receive `Exec`, and downloaded code is
never called during installation.

### 4. Coven artifact model

A rune release continues to require its canonical source tree and manifest.
Libraries may have no application artifacts. An installable application adds a
target-indexed artifact manifest while the version is staged:

```text
ArtifactManifest {
    version: 1,
    artifacts: {
        "aarch64-apple-darwin": {
            kind: "witchy-trusted-exe-v1",
            command: "wrg",
            sha256: "...",
            size: 12345678,
            binding_plan_sha256: "...",
            authority: ["root: cwd Dir[Read]"]
        },
        "x86_64-unknown-linux-gnu": { ... }
    }
}
```

Target names use canonical Rust/Witchy target triples. `command` must be a
single portable filename component: non-empty, no separators, no `.` or `..`,
and no platform suffix supplied by an untrusted path. The client derives any
required platform suffix from the selected target.

The logical artifact is raw bytes. A first transport may encode those bytes in
base64 because today's self-hosted HTTP surface is text-bodied, but the signed
`sha256` is the digest of the decoded executable bytes, not of JSON or base64
text. Size is also the decoded byte length. `binding_plan_sha256` must match the
authenticated binding plan embedded by `trusted-exe`; `authority` is its
canonical human-readable rendering. The rendering is useful audit information,
not a claim that a hostile native publisher is sandboxed from the user.

The release record moves to an unambiguous versioned signing payload that
includes the artifact-manifest digest. Existing source-only records remain
verifiable as their original version. Promotion signs and freezes:

- package name and version;
- source hash and computed runtime footprint;
- publisher, promoter, provenance, and release timestamp;
- artifact-manifest digest, including every target entry.

TUF-style targets/snapshot metadata therefore commits to the release record
that commits to the exact artifact set. A mirror cannot substitute a target,
command name, executable, or source tree without breaking verification.

### 5. Artifact publishing lifecycle

Application artifacts are publisher-built; Coven is not a build farm.

The lifecycle is:

```text
publish source → staged version
upload one or more target artifacts → still staged
promote → released source and artifact manifest become immutable
```

Only the publisher identity and authorization policy that staged the version
may add an artifact; anonymous local Coven keeps its existing local identity
semantics. A target entry is write-once: replacing bytes for the same target
requires a new package version rather than silently changing a candidate
another person may already be reviewing. Promotion may release a source-only
library or an application with any non-empty subset of supported targets.

No artifact may be added, removed, or replaced after promotion. Adding another
platform later requires a new version. This is intentionally strict: a package
coordinate denotes one immutable reviewed release, not a source version with a
mutable set of native programs.

The protocol must work against a locally running anonymous Coven and with
artifacts built on a developer's machine. Hosted CI, GitHub Actions, and a Coven
build service are not prerequisites. Authenticated registries reuse their
existing publisher and promoter policy; this RFC does not introduce long-lived
upload tokens.

### 6. Resolution and verification

`install` reuses the existing package trust pipeline rather than implementing a
second registry client. In order, it must:

1. resolve the version requirement against released, non-yanked, cooldown-
   eligible records;
2. verify the registry root pin and timestamp/snapshot freshness;
3. verify the signed record and bind its own name/version to the requested
   coordinate;
4. read and verify the artifact manifest committed by that record;
5. select the exact current host target;
6. fetch the artifact and verify decoded size and SHA-256;
7. validate that it is a structurally supported Witchy trusted executable for
   the current host ABI, and that its embedded binding-plan digest and rendered
   authority match the signed manifest, without executing it;
8. atomically install it and write a local receipt.

Missing target artifacts fail with a message such as:

```text
acme/wrg@1.4.2 has no trusted executable for aarch64-apple-darwin
available targets: x86_64-unknown-linux-gnu
```

A staged or yanked record, stale/frozen metadata, coordinate mismatch, malformed
manifest, unexpected command name, unsupported format, size mismatch, digest
mismatch, or incompatible host ABI fails closed before the destination changes.

### 7. Byte-safe and atomic installation substrate

The current self-hosted source protocol is text-oriented. Installing native
executables requires general byte-safe primitives, not an installer-specific
escape hatch:

- `Dir`/`File` byte reads and writes using `Bytes`;
- SHA-256 over `Bytes`;
- a byte-preserving artifact transport (raw or explicitly decoded base64);
- confined creation of a temporary file;
- executable permission handling on platforms that require it;
- atomic rename/replace within one granted directory.

These operations extend the same `Dir`/`File` confinement and rights checks as
text I/O. They must not add ambient filesystem helpers or a bespoke
`__install_executable` intrinsic that bypasses the capability model.

Installation writes a temporary sibling, verifies all bytes before exposure,
sets the required executable metadata, and atomically renames it into place. On
any error it removes only its own temporary file and leaves the previous
installation unchanged.

The receipt lives under `$WITCHY_HOME` and records at least package coordinate,
registry/root-key identity, target, command, artifact digest, and installed
path. It is local bookkeeping, not a substitute for signed Coven metadata.
Uninstall, automatic updates, and garbage collection may use it later but are
outside the initial command.

### 8. Local end-to-end acceptance test

The feature is incomplete until one deterministic test exercises the real
self-hosted registry and installer locally, with no external network or hosted
CI:

1. Start the real `witchy coven-serve` on loopback with a temporary store.
2. Build the committed minigrep fixture as a current-host `trusted-exe`.
3. Publish its source and upload the executable to a staged release.
4. Promote the release with the existing separation-of-duties fixture.
5. Set `WITCHY_HOME` to a temporary directory and run
   `witchy install acme/minigrep`.
6. Remove the source/build tree and run the installed command with an empty
   `PATH`, proving the artifact contains its runtime and needs no grant flags.
7. Verify its cwd-bound read succeeds and a `..` escape remains denied.

The same focused test tier must also prove:

- a staged or yanked version is not installable;
- a release without the current host target reports the available targets;
- artifact tampering or manifest/digest mismatch leaves no installed file;
- install never invokes the downloaded program (a fixture marker remains
  absent until the test explicitly runs it);
- a failed `--force` replacement preserves the previous executable; and
- dependency source installation remains inert and unchanged.

The test uses one locally built artifact and a small number of registry
transactions. It must not depend on random seeds, wall-clock sleeps, a public
registry, environment mutation outside RAII-scoped test state, or GitHub
Actions.

### 9. Implementation sequence and ownership

Land this in narrow, reviewable cuts:

1. **Byte substrate:** byte-safe confined file I/O, hashing, executable metadata,
   and atomic rename. Coordinate with the active operation-catalog work before
   touching the Bytes/FS surface.
2. **Coven protocol:** versioned artifact manifest, staged upload/read routes,
   record-signing update, storage, and metadata binding.
3. **Grimoire installer:** shared resolution/verification, host selection,
   atomic install, receipt, and actionable diagnostics.
4. **Trusted Grimoire target:** additive `witchy-home`/`exe-dir` providers and a
   manifest that builds the same frontend as a standalone trusted executable.
5. **Rename:** migrate `projects/pm` and user-facing `pm` prose while retaining
   the compatibility route.
6. **End to end:** real Coven publish/promote/install/run and adversarial failure
   cases.

The protocol and installer should not be implemented by expanding into compiler
lowering, monomorphization, RFC-0090, or unrelated capability representation
work. If a required byte or aggregate operation is blocked on active RFC-0005
work, report that boundary rather than adding a second representation.

## Security properties

- `add` remains source-only and executes no package code.
- `install` never executes the downloaded application, including for version or
  format discovery.
- Only released, non-yanked, eligible records resolve by default.
- The requested coordinate, signed record, artifact manifest, selected target,
  downloaded bytes, and destination command name are bound end to end.
- A registry or mirror cannot use metadata to select a foreign host target or
  write outside the install `Dir`.
- Partial, corrupt, or failed downloads never replace an existing command.
- Grimoire uses explicit runtime grants; its dependencies receive no ambient
  filesystem, network, secret, or execution authority.
- Running the installed program is still a whole-artifact trust decision. The
  installer does not misrepresent a trusted native executable as an untrusted
  sandbox.

## Alternatives

### Keep `pm` as the public name

`pm` is short but generic, difficult to search for, and says nothing about its
relationship to Witchy and Coven. Grimoire gives the self-hosted application a
stable identity while top-level `witchy` verbs keep routine commands concise.

### Standardize `grim` as the executable

Rejected for the initial contract. A short global command creates avoidable
collision and discoverability risk. Implementations may offer local aliases,
but scripts should use `witchy install` or the full `grimoire` name.

### Compile from source on the recipient's machine

This is closer to `cargo install` and gives the recipient a stronger connection
between reviewed source and output. It also requires a compatible Witchy
toolchain, repeats compilation on every machine, cannot install onto a machine
that only wants the resulting application, and does not exercise the
distribution artifact RFC-0092 introduced. A future `--from-source` mode may be
valuable, but it is not the default proposed here.

### Have Coven build every artifact

Rejected. It turns Coven into a privileged multi-platform build farm, requires
substantial isolation and reproducibility policy, and makes local/private
operation depend on infrastructure the repository does not have. Publishers
build artifacts; Coven authenticates, freezes, and serves them.

### Store binaries without attaching them to source releases

Rejected. A parallel binary namespace would duplicate version resolution,
identity, promotion, yanking, cooldowns, and TUF metadata. One immutable release
may contain source only or source plus trusted application artifacts.

### Install portable WASM and generate a wrapper

Rejected. That preserves the consumer-granted sandbox model and recreates the
multi-file wrapper/runtime UX RFC-0092 intentionally replaced. Portable WASM
remains available for applications the recipient does not trust.

### Use a specialized native installer implementation

Rejected as the product architecture. A Rust-only `install` command could write
bytes sooner, but it would fork Coven verification and avoid dogfooding the
self-hosted package manager. General byte I/O and atomic file operations belong
in the capability-safe platform and benefit applications beyond Grimoire.

## Drawbacks

- Every supported target multiplies registry storage and publisher work.
- A promoted version cannot gain a newly built platform artifact without a
  version bump.
- The initial release trusts publisher-built binaries without proving they are
  reproducible from the published source.
- Grimoire is a high-authority application. A bug in its root command dispatch
  can misuse installation, project, network, or compiler capabilities even
  though its dependencies remain confined.
- Byte-safe filesystem operations, executable permissions, atomic replacement,
  and host target detection introduce real cross-platform implementation work.
- Renaming a mature internal `pm` surface creates temporary aliases and
  documentation churn.
- A standalone Grimoire installed somewhere other than the expected Witchy
  toolchain directory may be unable to drive `witchy` until its binding plan is
  rebuilt or the toolchain is colocated.

## Non-goals

- An application store UI, ratings, search ranking, or paid distribution.
- Automatic background updates or executing installers after download.
- Platform-native package generation such as Homebrew formulae, deb/rpm, MSI,
  pkg, or DMG.
- Proving source-to-binary reproducibility or requiring remote attestations.
- Installing arbitrary non-Witchy native executables under a trusted-exe label.
- Replacing portable WASM, explicit consumer grants, or dependency source
  resolution.
- Requiring GitHub Actions, hosted CI, or a Coven-operated build farm.

## Acceptance criteria

This RFC is implemented when:

1. The package-manager frontend is named Grimoire, `witchy install` is public,
   and `witchy pm` routes compatibly to the same implementation.
2. The same Grimoire source builds and runs as a trusted executable with checked
   project, install, registry, and compiler bindings.
3. Coven stages, signs, promotes, serves, and yanks target-specific
   `trusted-exe` artifacts as part of immutable source releases.
4. The installer verifies the complete existing metadata chain plus artifact
   manifest, target, size, digest, embedded binding plan, rendered authority,
   and supported executable format before any destination change.
5. Installation is byte-safe, executable, atomic, non-executing, and confined
   to `$WITCHY_HOME`.
6. Source-only libraries continue to publish and `add` exactly as before.
7. The real local Coven end-to-end test publishes, promotes, installs, and runs
   minigrep, while the adversarial staged/yanked/target/tamper/atomic cases fail
   closed.
8. Documentation states plainly that installing and later running the artifact
   trusts its publisher-built native executable; accompanying source and
   provenance do not by themselves prove reproducibility.

## Conclusion

RFC-0092 made a Witchy application look like a normal executable. This RFC
makes it install like one without abandoning Witchy's package trust model:

```text
publisher-built trusted-exe
        ↓ staged with source
Coven-signed immutable release
        ↓ verified for this host
Grimoire atomic installation
        ↓
ordinary command, no Witchy or grant flags required
```

Grimoire is trusted because package management necessarily brokers real
resources. Coven does not make the installed application untrusted or
sandboxed; it makes the identity, release state, target, and bytes precise.
That is the missing distribution layer between "build a trusted executable"
and "an end user can install and run a Witchy application."
