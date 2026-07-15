---
rfc: 0092
title: Trusted application executables
status: proposed
created: 2026-07-15
superseded-by:
tracking: >
  Proposed. Add a trusted executable build target containing the Witchy runtime
  and one compiled application. Running the resulting binary is the user's
  trust decision, as it is for an ordinary native executable. The launcher
  resolves build-defined bindings for the capabilities declared by `main`;
  portable `.wasm` remains consumer-hosted and explicit-grant.
related:
  - "0004 (self-hosted CLI — the Witchy compiler is a candidate trusted application)"
  - "0005 (unforgeable capabilities — dependencies receive only delegated values)"
  - "0013 (grant documents — retained for untrusted portable programs)"
  - "0086 (native extensions — trusted native code joins the executable TCB)"
---

# RFC-0092: Trusted application executables

## Summary

Add a build target that produces a normal, self-contained executable from a
Witchy application:

```sh
witchy --release build --target trusted-exe
./target/release/wrg TODO ./src
```

The executable contains a native launcher, the Witchy runtime, and the
application's compiled WASM. It runs on a machine without Witchy installed and
does not require Witchy capability flags.

At build time, the application author binds each resource-bearing capability
parameter of `main` to a process resource recipe. A `Dir[Read]` can be rooted at
the launch working directory or at a configured path. Each recipe resolves to
exactly one directory subtree when the executable starts. If `main` does not
declare `Net`, the compiled application receives no network capability.

Running this artifact means trusting the whole executable: application,
embedded runtime, and distributor. That is the same decision a user makes when
running a Rust, Go, or C program. Capabilities remain valuable *inside* the
application because linked dependencies receive only the capabilities the
trusted root passes to them.

This RFC defines a binary target, not an application ecosystem. The executable
may be copied directly, attached to a release, installed by a package manager,
or installed with `curl | bash`. Coven, an app store, persistent trust receipts,
and a Witchy-specific installer are not required.

## Motivation

### The two useful modes Witchy has today

Witchy already serves two audiences well.

A developer can run trusted local source:

```sh
witchy run
witchy src/tool.witchy
```

This is convenient, but it assumes the complete Witchy compiler/runtime and a
source project are present.

A recipient can instead run a portable, possibly untrusted WASM module with
their own trusted Witchy host:

```sh
witchy sandbox --dir ./workspace --net api.example.com:443 app.wasm
```

This is the right distribution model when the recipient wants to confine the
program. The program author does not supply the runtime enforcing the grant,
and the user explicitly chooses its real resources.

### The missing mode

Neither path feels like installing a conventional application. A user of a
search tool expects:

```sh
wrg pattern /tmp/project
```

Today its author must ask the user to install Witchy, download source or WASM,
understand Witchy's capability vocabulary, and repeat `/tmp/project` in a
separate `--dir` grant. That second spelling is useful when the user distrusts
the program. It is ceremony when the user has deliberately installed and
trusted the search tool.

The obvious workaround is a wrapper script:

```sh
#!/bin/sh
exec witchy sandbox --dir / wrg.wasm "$@"
```

The wrapper recreates an executable poorly. It requires a separate runtime,
usually over-grants, complicates argument forwarding, and turns one signable
artifact into several moving parts.

The Witchy compiler itself has the same problem. A user who installs `witchy`
already trusts it to read projects, resolve packages, compile code, and broker
child sandboxes. Requiring that user to spell the compiler's own grants on every
invocation would not create a meaningful security boundary.

### Who the user trusts

The portable and trusted artifacts answer different questions:

| Artifact | User trusts | Application root receives |
|---|---|---|
| `app.wasm` | the consumer's Witchy host; the app only within its grant | explicit launch-time grants |
| `trusted-exe` | the complete distributed executable | resources selected by its embedded build-time bindings |

For portable WASM, the application author cannot replace the runtime claimed to
confine the application. For a trusted executable, the user intentionally
accepts the author/distributor's entire artifact, just as with another native
command.

This changes the outer trust boundary, not Witchy's internal programming model.
The trusted root still cannot conjure capability values in guest code. A linked
dependency still receives no filesystem or network authority merely because it
is linked. The root must pass those typed values explicitly.

## Proposal

### Build command and output

Add the project build target `trusted-exe`:

```sh
witchy build --target trusted-exe
witchy --release build --target trusted-exe
witchy --release build --target trusted-exe --out dist/wrg
```

Without `--out`, the root rune name determines the executable name:

```text
target/debug/<name>
target/release/<name>
```

The word `trusted` is intentional. `exe` alone would hide the changed security
posture. The application still executes as WASM inside the launcher; this RFC
does not add a second Witchy code generator.

The first implementation builds for the compiler's host platform. Additional
OS/architecture launcher targets are future work. Each output is one ordinary
ELF, Mach-O, or PE executable, subject to the platform's usual signing and
packaging tools.

`witchy emit-wasm` and ordinary portable builds keep producing consumer-hosted
WASM. A developer must select `trusted-exe` explicitly.

### Executable shape

The executable consists of:

```text
native launcher
├── Witchy runtime and Wasmtime
├── embedded app.wasm
└── versioned payload descriptor
    ├── payload location and digest
    ├── required host ABI
    └── witchy.launch contract digest
```

The Witchy toolchain ships the launcher template. Building an application must
not require the author to install Rust or C merely to link the result.

The launcher validates the descriptor, embedded WASM, imports, host ABI, and
`witchy.launch` contract before instantiation. These checks detect corruption
and packaging mistakes. They do not defend against a malicious distributor,
who controls the entire executable the user chose to trust.

The embedded WASM remains the canonical application payload so the executable
uses the same compiled backend and launch contract as portable distribution.
An engine-specific ahead-of-time image may be an optimization later, but does
not define a different application ABI.

### Startup

The launcher:

1. Validates the embedded payload.
2. Derives the root grant from the declared parameters of `main`.
3. Resolves the embedded build-time binding for each resource parameter.
4. Links only the operations admitted by their declared rights.
5. Passes process arguments as ordinary application data and invokes `main`.
6. Maps output, exit status, traps, and signals to normal process behavior.

The grant comes from `main`, not the union of every linked function's footprint.
A dependency exporting `fetch(net)` cannot widen an entrypoint that declares no
`Net`.

The launcher reserves no capability command-line flags. Tokens such as `--dir`
and `--net` belong to the application if it defines them. The binary must behave
like a normal command, not like an implicit `witchy sandbox` invocation.

### Build-time capability bindings

The `main` signature determines *kind and rights*, but not resource identity.
These declarations are different:

```witchy
fn main(project: Dir[Read], filesystem: Dir[Read]):
    // Both values have the same type and different intended roots.
    return
```

The trusted-executable target therefore includes a binding table keyed by the
source parameter name. Illustrative `witchy.toml` syntax is:

```toml
[targets.trusted-exe.dirs]
project = { from = "cwd" }
filesystem = { from = "path", path = "/" }
```

This is target packaging policy, not a change to the source capability type.
The builder verifies that every resource-bearing `main` parameter has exactly
one compatible binding and embeds the checked table beside the launch contract.
Missing, extra, mistyped, or duplicate bindings fail the build. There is no
implicit fallback from an unbound `Dir` to `/`.

The table is the trusted executable's build-time, self-approved grant recipe.
Its parsing, parameter-name matching, footprint cross-check, and resource
opening should reuse the RFC-0013 grant machinery. Portable WASM still treats a
shipped grant document only as a request for the consumer to approve; this
target may embed an approved recipe because the consumer trusts the complete
binary.

The initial directory bindings are:

| Binding | Runtime meaning |
|---|---|
| `{ from = "cwd" }` | root is the process working directory at launch |
| `{ from = "path", path = "..." }` | root is the configured path, resolved on the target machine at launch |

`cwd` is appropriate for a project-local tool. `path` supports an application
intentionally rooted at a known data directory, configuration directory, or
filesystem root. A relative configured path is resolved from launch cwd, not
from the machine that built the executable. On POSIX,
`{ from = "path", path = "/" }` explicitly selects `/`.

Every binding produces an ordinary Witchy `Dir` with exactly one subtree root.
It does not introduce a second working base or a different path grammar. Guest
operations remain relative to that root and continue to reject absolute paths,
`..`, and symlink escape. To reach the OS path `/tmp/x` through a `Dir` rooted
at `/`, the application supplies the Dir-relative path `tmp/x`; the absolute
guest string `/tmp/x` remains invalid.

A POSIX file utility that accepts both cwd-relative and absolute CLI arguments
can bind two explicit roots:

```toml
[targets.trusted-exe.dirs]
cwd = { from = "cwd" }
root = { from = "path", path = "/" }
```

Its `main` uses `cwd` for relative arguments and, after ordinary application
path parsing removes the leading root separator, uses `root` for absolute
arguments. The launcher does not inspect argv or silently combine the two roots.
Other target platforms declare the concrete roots their application supports;
a future portable OS-path abstraction would be a separate design.

The rights come only from the parameter type. The binding does not restate them:
`project: Dir[Read]` remains unable to write under every binding. Any `subtree`,
`only`, or other attenuation creates a normal narrower capability. None of this
changes sandbox-granted `Dir` behavior.

A direct `File` has a particular identity, so it requires a fixed build binding:

```toml
[targets.trusted-exe.files]
config = { from = "path", path = "./wrg.toml" }
```

The path is resolved when the executable starts, and startup fails if the file
cannot be opened with the rights declared by `config: File[...]`. A file selected
by application argv cannot use this mechanism: the launcher does not know the
application's CLI grammar. Such an application takes a bound `Dir`, parses argv,
and opens or attenuates a `File` itself.

Capabilities with one conventional process binding need no manifest entry:

| `main` parameter | Runtime value |
|---|---|
| `Console` | process standard streams |
| `Clock` | operating-system clocks |
| `Rand` | operating-system randomness |
| `Env` | inherited process environment |
| `SecretStore` | an empty store, or named providers from target configuration |

`Net` and `Exec` also require explicit target bindings because an author may
choose unrestricted OS-visible access or a narrower compiled-in policy. `Exec`
still does not name programs by itself; as in the existing standard library,
the root also needs a compatible bound `Dir[Read]`.

Named `SecretStore` entries may reuse RFC-0013 providers such as
`{ from = "env:GITHUB_TOKEN" }`: the provider name is embedded, while secret
bytes are resolved into an opaque host value at startup and never embedded. A
bare `Secret` has no safe conventional value, and a `NativeLoader` needs a fixed
approved module set. The initial implementation rejects these root parameters
at build time. Future work may add providers such as an OS keychain or
content-identified extension set, but it must not embed secret bytes merely for
convenience or reintroduce required launch flags.

### Capabilities inside the application

A trusted search tool could declare:

```witchy
fn main(console: Console, cwd: Dir[Read], root: Dir[Read], args: List[String]) -> Int:
    // Route relative/absolute argv paths, then pass only the selected Dir onward.
    0
```

The end user runs `wrg` without grants. Inside the application, a pure regex
library receives no capability, a walker receives `Dir[Read]` only when the app
passes it, and no dependency can recover `Net`, write, `Exec`, or secrets.

The trusted root may deliberately pass its full root capability to a
dependency. That is allowed: the user trusts the root application's decisions.
Witchy makes the delegation explicit and reviewable instead of implicitly
available to every linked function.

Dependency build steps remain governed by existing build-only grants. Producing
a trusted final executable does not give dependency build code real access
to the developer's machine.

## Distribution

The output is an ordinary executable, so ordinary distribution is sufficient:

```sh
curl -fsSL https://example.com/install-wrg.sh | sh
wrg pattern path
```

An author may instead use GitHub releases, Homebrew, apt, Nix, winget, an
internal package server, or a copied file. Those systems may verify signatures,
checksums, provenance, or reproducible builds. None changes the executable's
runtime semantics.

Coven may distribute these executables in the future, but this RFC neither
requires nor designs that integration. It also does not define an app store,
update service, install database, permission prompt, or trust receipt. Running
the installed binary is the trust decision.

## The Witchy compiler as a trusted application

An official `witchy` binary is a primary use case. It may eventually be built
from a self-hosted Witchy application with `trusted-exe`, while the current
Rust-built executable remains the bootstrap equivalent.

Trusting the compiler's root does not widen programs it handles. Comptime keeps
its budget and capability rules, dependency build steps keep separate build
grants, and `witchy sandbox` children keep only their explicit grants. A child
module must never inherit the compiler process's bound `Dir`, `Env`, `Net`,
`Exec`, or secrets.

The compiler is a trusted broker, not an excuse to collapse nested boundaries.

## Relationship to the current `No build-exe` rule

The binary-distribution specification currently rejects a self-contained
executable because an untrusted program author could bundle a runtime that
ignores the user's grants. Preserve that rule for its intended case:

> An untrusted portable program cannot provide the runtime claimed to confine
> it. Run its WASM with the consumer's trusted Witchy host.

Add the complementary rule:

> A trusted application may provide its runtime. Executing the standalone
> binary means trusting the complete artifact; its root is not sandboxed from
> the user.

There is no contradiction. Portable WASM answers “how can I run this without
trusting it beyond a grant?” The trusted executable answers “how can I install
and run this Witchy application like a normal command while retaining explicit
delegation inside it?”

## Security properties and limits

The target preserves these Witchy properties:

- Root authority is still visible in `main`'s typed parameters.
- Concrete root identities and scopes are visible in the executable target's
  checked build configuration.
- Real capabilities remain host-minted values unavailable to ordinary guest
  code.
- Rights such as `Dir[Read]` still restrict available operations.
- Dependencies receive capabilities only through explicit delegation.
- A dependency's footprint cannot widen the root grant.
- Dependency builds and sandboxed children retain their separate boundaries.
- Portable WASM retains consumer-hosted, explicit-grant semantics.

The target deliberately does not promise that the trusted root is confined from
the user. A malicious distributor can alter both the application and embedded
runtime. The root can deliberately delegate broad authority, behave as a
confused deputy, return false results, or consume resources. The compiler,
runtime, Wasmtime, and any native extensions are part of the trusted computing
base. The OS account remains the outer authority ceiling.

These are the consequences of running any trusted native binary. Witchy adds
useful internal authority structure; it does not make an untrusted executable
safe merely because its payload was written in Witchy.

## Rejected alternatives

- **Require grants on every run.** Keep this for untrusted programs. It is the
  wrong UX after a user has intentionally installed and trusted an application.
- **Ship a wrapper around WASM.** This retains a runtime prerequisite and makes
  grants, signing, and argument forwarding ad hoc.
- **Make host operations global.** Convenience at the root must not erase
  explicit capability parameters or dependency isolation.
- **Add a universal `System` capability.** This would discard capability kinds
  and rights, leaving the root unable to delegate narrowly.
- **Infer grants from argv.** Only the application knows whether a string is a
  path, address, pattern, output name, or ordinary data.
- **Build Coven installation first.** Distribution provenance and updates are
  independent of the binary's format and runtime semantics.

## Implementation outline

1. Define a versioned embedded-payload descriptor and ship a host launcher
   template.
2. Package the ordinary compiled WASM output into one native executable.
3. Add checked target bindings and providers for the supported root capability
   kinds by extending the existing grant-resolution and confinement machinery.
4. Add `witchy build --target trusted-exe`, release/debug output naming, and
   `--out`.
5. Test both a search-tool fixture and the Witchy compiler use case, including
   dependency and child-sandbox non-inheritance.
6. Update `spec/binary-distribution.md` to document both artifact trust models.

## Acceptance criteria

The RFC is implemented when:

1. `witchy --release build --target trusted-exe` emits one host-native
   executable that runs on a machine without Witchy installed.
2. The executable embeds and validates the same WASM and launch contract as the
   portable compiled backend.
3. Argv, cwd, environment, standard streams, exit status, traps, and signals
   behave like a normal command with no reserved Witchy grant flags.
4. A `cwd`-bound `Dir[Read]` rejects paths outside launch cwd. A POSIX search
   fixture separately bound to cwd and `/` accepts relative and absolute
   application arguments by routing them to Dir-relative paths; neither
   capability accepts an absolute guest path, escapes its root, or writes.
5. A root without `Net`, `Exec`, write, secrets, or another capability family
   cannot call that operation through the Witchy runtime.
6. A linked dependency's broader footprint does not widen the entrypoint grant,
   and it receives a real capability only when the root passes one.
7. Dependency build steps and sandboxed child programs inherit none of the
   trusted root's real authority.
8. Missing, extra, duplicate, or type-incompatible target bindings, and root
   parameters with no supported binding, fail during the build with actionable
   diagnostics rather than prompting at runtime or being omitted.
9. The same source emitted as portable WASM still requires a consumer-controlled
   host and explicit real-resource grants.
10. Corrupt payloads and incompatible host ABI or launch metadata fail before
    `main` runs.
11. User documentation says plainly that running the executable trusts its
    application, embedded runtime, and distributor.

## Conclusion

Witchy's secure development model should be available to ordinary applications
without making ordinary users operate a capability launcher. A developer can
write a compiler, search tool, editor, or service whose root authority is
explicit and whose dependencies receive only deliberate delegations. An end
user can install and run it like any other executable.

The design needs one new artifact, not a new application ecosystem:

```text
trusted native launcher + Witchy runtime + app.wasm
```

Portable WASM remains the answer for code the user does not trust.
`trusted-exe` is the answer when running the binary is itself the trust decision.
