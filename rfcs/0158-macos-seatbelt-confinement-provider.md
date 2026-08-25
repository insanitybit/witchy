---
rfc: 0158
title: "macOS Seatbelt confinement provider"
status: proposed
created: 2026-08-25
predecessors:
  - "[0103](0103-derived-platform-confinement.md) (derived platform confinement — this RFC fills its `Seatbelt` future-work slice; every doctrine section there governs here)"
related:
  - "[0102](0102-portable-roots-and-the-fetch-capability.md) (provider doctrine and host menus — the capability mirror of this decision)"
  - "[0157](0157-trusted-publishers-and-executable-state.md) (trusted publishers — the deployment mode that most wants `--confine=required`)"
  - "[0020](0020-rebinding-resistant-http.md) (pinned dials — the finer network enforcement this layer only bounds)"
tracking: >
  Proposed. Feasibility was measured, not assumed: the API surface, the
  SBPL preamble, exec inheritance, and network expressiveness were each
  probed on macOS 26.3.1 (Darwin 25.3, Apple Silicon) before this RFC was
  written. Every result those probes produced is reproduced inline in
  §Measured feasibility, so this document stands alone; the throwaway C
  probes and profile template live in the gitignored
  `scratch/confine-demo/` and are not tracked evidence.
---

# RFC-0158: macOS Seatbelt confinement provider

> Provisional syntax. Code blocks here are intentionally **not** tagged
> `witchy` so the doc-examples test does not try to compile them.

## Summary

RFC-0103 derives a target-neutral confinement `Policy` from a program's
resolved capability grants and enforces it at the platform boundary. Linux
has two providers (Landlock, seccomp-bpf) and the browser has one (derived
CSP). macOS has none: every layer reports `unavailable`, and
`--confine=required` refuses to launch.

This RFC adds a macOS provider built on Seatbelt — the sandbox that backs
`sandbox-exec`, Chromium's renderer confinement, and Nix's build isolation.
The derivation is unchanged; only the translation is new. `Policy` compiles
to an SBPL profile applied to the process immediately before guest
execution, exactly where `crate::linux::apply` runs today.

It does not change any capability semantics, add any grant vocabulary, or
alter program-observable behaviour. It closes the gap where the platform
witchy is primarily *developed* on is the one platform whose strict launch
mode cannot run.

## Motivation

### The development platform is the unenforced platform

The asymmetry is backwards. `witchy sandbox --confine=required` is the
deployment posture RFC-0103 exists to offer, and on a macOS workstation it
produces:

```
required platform confinement is unavailable on this host
```

Every agent and contributor developing witchy on macOS therefore exercises
the *single-layer* configuration exclusively, and the derived-policy code
path gets no local coverage at all — the derivation is only ever executed
for real on Linux CI. A provider here is worth as much for the feedback loop
as for the deployment.

### The threat model does not change at the platform boundary

RFC-0103's argument is that a bug in the type checker, the wasm kernel's
import handlers, `confine`, or wasmtime yields the process's full ambient
authority. That argument is platform-independent. A macOS host today has
exactly one enforcing layer for the same trust base.

### The slice is genuinely bounded, and now measured

RFC-0103 listed Seatbelt under Future work as "a bounded, owner-sized
slice." Before proposing it, the four load-bearing questions were probed on
macOS 26.3.1 from an ordinary **unsigned, un-entitled** CLI binary. All four
came back favourable, and one produced a finding that would otherwise cost
an implementer a day.

## Measured feasibility

Everything in this section was executed on macOS 26.3.1 (Darwin 25.3, Apple
Silicon), not inferred. Each block is captured output.

### The API is reachable without signing or entitlements

`/usr/lib/libsandbox.1.dylib` `dlopen`s and exports the two-step form:

| Symbol | Via `RTLD_DEFAULT` | Via explicit `dlopen` |
|---|---|---|
| `sandbox_init` | present | present |
| `sandbox_init_with_parameters` | present | present |
| `sandbox_compile_string` | absent | **present** |
| `sandbox_apply` | absent | **present** |
| `sandbox_free_profile` | absent | present |

`sandbox_init_with_parameters(sbpl, flags, params, &errorbuf)` applies an
inline SBPL string when **`flags` is `0`**. This is the single most
error-prone constant in the API: `0x0001` is `SANDBOX_NAMED` (named profiles
only) and `0x0002` — the value one would guess for "string" — fails with the
misleading `File name too long`.

The whole family is annotated `API_DEPRECATED(… macos(10.5, 10.8))` in
`sandbox.h`, i.e. deprecated since 2012 and still load-bearing for Chromium,
Nix, Bazel, and `sandbox-exec` fourteen years later. §Drawbacks treats that
risk directly.

### A deny-default profile confines an ordinary process

```
(version 1)
(deny default)
… preamble …
(allow file-read* (subpath "<the one granted tree>"))
```

yields, after arming:

```
in-grant   <granted>/notes.txt   OK      the only bytes this program may ever touch
out-grant  /etc/passwd           DENIED  (Operation not permitted)
```

### Children inherit the fence

This is the property RFC-0103's "Exec children: the declared hole" section
depends on, and it holds identically to a Landlock domain. A `posix_spawn`ed
`/bin/cat` under the parent's profile reads inside the grant and is refused
outside it:

```
cat <granted>/notes.txt   -> exit 0
cat /etc/passwd           -> cat: /etc/passwd: Operation not permitted
cat ~/.ssh/known_hosts    -> cat: …/known_hosts: Operation not permitted
```

So the `exec = { programs = [...], child-paths = [...] }` vocabulary
RFC-0103 defined for Linux carries over unchanged, with the same meaning.

### Network expressiveness matches Landlock's, exactly

SBPL rejects any remote host but `*` or `localhost`:

```
(remote ip "127.0.0.1:9999")
  -> host must be * or localhost in network address
```

Ports are expressible; hosts are not. That is the *same* ceiling as
Landlock ABI v4, which is port-granular. Measured behaviour with a single
allowed port:

```
in-policy      connect 127.0.0.1:9999 -> Connection refused   (fence allowed it)
out-of-policy  connect 127.0.0.1:9998 -> Operation not permitted
```

`Policy.network` therefore maps across with no loss of fidelity relative to
Linux, and RFC-0103's framing survives verbatim: the kernel layer bounds the
blast radius; RFC-0020's pinned dials remain the finer enforcement.

### The finding an implementer would otherwise lose a day to

A *tight* allow-list aborts every `exec` in dyld with **SIGABRT and no
diagnostic whatsoever** — dyld's own attempt to write the error is denied by
the same profile, so nothing reaches stderr and nothing reaches the unified
log.

Bisecting it: `(allow file-read* (subpath "/"))` works. No union of
`(subpath "/bin")`, `(subpath "/usr/lib")`, `(subpath "/System")`,
`(subpath "/private")`, `(subpath "/dev")` does. The missing rule is the
**root directory node itself**:

```
(allow file-read* (literal "/"))
```

Adding it fixes the abort. A second rule is then needed for ancestor
traversal into the grant trees:

```
(allow file-read-metadata)      ; unrestricted
```

Narrowing `file-read-metadata` to the grant subpaths re-breaks opening files
*inside* those subpaths. Both rules are preamble, not policy: they widen
metadata and the root node, never file contents.

## Design

### Shape

A `crates/witchy-confinement/src/macos.rs` sibling to `linux.rs`, behind
`#[cfg(target_os = "macos")]`, exposing the same entry point:

```
pub(crate) fn apply(policy: &Policy, mode: EnforcementMode)
    -> Result<EnforcementReport, EnforcementError>
```

`provider.rs` gains a `#[cfg(target_os = "macos")]` arm alongside the Linux
one, and `platform_provider()` / `syscall_provider()` return `"seatbelt"`.
Nothing above the crate changes: `src/commands/confinement.rs` already
prints whatever layers come back, and `EnforcementMode` already means the
right thing.

The crate's dependency-bottom position is preserved — the provider links
`libsandbox` by symbol, not by adding a compiler, runtime, or Wasmtime
dependency.

### Policy → SBPL

| `Policy` element | SBPL |
|---|---|
| `FsRule { scope: Tree, access.read }` | `(allow file-read* (subpath "<canonical>"))` |
| `FsRule { scope: Tree, access.write }` | `(allow file-write* (subpath "<canonical>"))` |
| `FsRule { scope: File, access.read }` | `(allow file-read* (literal "<canonical>"))` |
| `FsRule { scope: File, access.write }` | `(allow file-write* (literal "<canonical>"))` |
| `FsAccess.execute` | `(allow process-exec (subpath "<canonical>"))` |
| `network.connect` port *p* | `(allow network-outbound (remote ip "*:p"))` |
| `network.bind` port *p* | `(allow network-bind (local ip "*:p"))` |
| no network grant | omit both — `(deny default)` covers it |
| `SyscallClass::Process` absent | omit `process-exec*`/`process-fork` |

`FsScope::Path` resolves to `literal` or `subpath` from the same opened
object the provider uses to install the rule, matching the Landlock
provider's contract.

### The preamble

Emitted verbatim ahead of the derived rules, and owned centrally the way
seccomp's promise classes are:

```
(version 1)
(deny default)
(allow process-fork)
(allow signal sysctl-read mach-lookup ipc-posix-shm*)
(allow file-read-metadata)
(allow file-read* (literal "/"))
(allow file-read* (subpath "/bin") (subpath "/usr/lib") (subpath "/System"))
(allow file-write-data)
```

Two notes on scope. `file-write-data` unrestricted is the analogue of
Landlock leaving pre-opened descriptors alone: `Console` writes to inherited
fds must not need a rule, and Seatbelt filters writes on open descriptors.
Tightening it to `(literal "/dev/stdout")`-style rules is future work, not a
correctness requirement — under the superset invariant, over-permitting in
the preamble is the safe direction. The `/bin`, `/usr/lib`, `/System`
subtrees are the loader and platform-binary paths; they are read-only and
carry no `file-write*` or `process-exec` right unless a grant supplies one.

### Path canonicalization

SBPL matches resolved paths, so every emitted path must be `realpath`'d
before emission. On macOS this is not cosmetic: `/tmp`, `/var`, and `/etc`
are symlinks into `/private`, and a rule written against the unresolved form
silently matches nothing. The provider canonicalizes through the same open
handle the grant already holds — `ConfinedDir::display_path` /
`ConfinedFile::display_path` — rather than re-resolving an ambient string,
preserving phase 1's handle-anchored property.

`materialize_write_targets` in `src/commands/confinement.rs` already
pre-creates write-grant targets so a rule can be anchored on them; that
logic is platform-neutral and applies here for the same reason.

### Layer reporting

macOS has no seccomp analogue. `SyscallClass` maps to Seatbelt *operation*
classes rather than syscall numbers, and the provider must say so rather
than overclaim:

| `Layer` | provider | status |
|---|---|---|
| `Filesystem` | `seatbelt` | `Enforced` |
| `Tcp` | `seatbelt` | `Enforced` (`Partial` when `has_unexpressed_transport`, as on Linux) |
| `Syscalls` | `seatbelt` | `Partial` — "operation classes enforced; no syscall-level filter on this platform" |

`Partial` for `Syscalls` means `--confine=required` fails on macOS under
`EnforcementReport::fully_enforced`. That is the correct default: `required`
means *every* implemented layer enforces, and macOS genuinely cannot deny at
syscall granularity. Deployments that want the two layers macOS does provide
are served by `best-effort`, which now reports two `Enforced` layers instead
of three `Unavailable` ones. If experience shows `required` should accept a
platform's best expressible policy, that is a change to RFC-0103's mode
vocabulary and belongs in its own RFC, not smuggled in through a provider.

### Fail-closed symbol resolution

The provider resolves `sandbox_init_with_parameters` at runtime and treats
absence as `LayerStatus::Unavailable` — so a future macOS that finally
removes the deprecated family degrades to today's behaviour rather than
crashing, and `required` still refuses to launch. Preferring
`sandbox_compile_string` + `sandbox_apply` when both resolve gives a
separate compile-error path from the apply-error path, which is worth having
for diagnosing a malformed derived profile; `sandbox_init_with_parameters`
is the fallback.

### Menu

Per RFC-0102, the host menu advertises enforcement:

```
# menus/native-macos.toml (excerpt)
[enforcement]
fs      = "seatbelt"
net     = "seatbelt-port"
syscall = "seatbelt-classes"    # operation classes, not syscall numbers
per-op  = "handle-anchored"
```

## Non-goals

- **No per-guest granularity.** Seatbelt is per-process, exactly as
  RFC-0103 records for Landlock. A process hosting several VMs is confined
  to the union of their grants.
- **No remote-host network rules.** SBPL cannot express them. The host
  layer remains the address-granular enforcer.
- **No `Env`/`Secret` enforcement.** No platform analogue, same honest gap
  as Linux.
- **No profile for the reusable embedded runtime.** Arming stays above the
  runtime, in `src/commands/confinement.rs`, because the policy is
  process-wide and irreversible.
- **No App Sandbox adoption**, and therefore no code-signing or
  entitlement requirement for `witchy` or for programs it runs.

## Alternatives

**Do nothing.** macOS keeps one enforcing layer and `required` stays
unusable there. Rejected: the platform gap is largest precisely where the
project's own development happens.

**Re-exec into a `sandbox-exec` child.** Spawn `/usr/bin/sandbox-exec -f
profile witchy …` instead of arming in-process. Rejected: it adds a process
to every strict launch, exposes the derived profile in `argv` and the
process table, depends on an ambient `PATH` binary, and makes the launcher's
own grant resolution happen *outside* the fence it is deriving. In-process
arming keeps the RFC-0103 ordering (resolve grants → arm → run guest) intact.

**App Sandbox via entitlements.** The supported, non-deprecated API.
Rejected: it requires code signing with a provisioning profile, is designed
for a *fixed* entitlement set decided at build time, and cannot express a
policy derived per launch from a program's grants. It is structurally the
wrong shape for capability-derived confinement.

**Endpoint Security / a system extension.** Rejected: requires an Apple-
granted entitlement, root installation, and a resident daemon — wildly
disproportionate for a CLI, and it would make witchy's fence depend on
privileged system state rather than on the launching process.

**Wait for a non-deprecated replacement.** Rejected: there is no announced
successor for dynamic per-launch policy, and the provider architecture
already makes this replaceable — that is what "one provider per platform"
buys.

## Drawbacks

- **A deprecated API is load-bearing.** Mitigated structurally (a provider
  is replaceable by construction), operationally (runtime symbol resolution,
  `Unavailable` on absence), and by company: `sandbox-exec` and every major
  macOS sandbox consumer depend on the same family.
- **SBPL is undocumented and version-sensitive.** The preamble is exactly
  the kind of thing that breaks on an OS upgrade, and it breaks *silently*
  (SIGABRT, no diagnostic). This is the main ongoing maintenance cost, and
  §Evidence answers it with a canary rather than with optimism.
- **`Syscalls` reports `Partial` forever on macOS**, so `required` stays
  unavailable there under the current mode vocabulary. Honest, but it means
  this RFC improves `best-effort` without unlocking `required`.
- **The preamble over-permits** (`file-read-metadata`, `file-write-data`,
  the root node). Correct per the superset invariant, but it means the macOS
  fence is a coarser bound than the Linux one, and the report should not
  imply otherwise.

## Prior art

- **Chromium's macOS sandbox** — the canonical production consumer of
  `sandbox_init_with_parameters` with parameterized SBPL, from an
  unprivileged process.
- **Nix and Bazel darwin sandboxes** — deny-default SBPL profiles generated
  per build from a declared input set; the closest existing analogue to
  deriving a profile from a grant set.
- **`sandbox-exec(1)` and `/System/Library/Sandbox/Profiles/*.sb`** — the
  reference corpus for operation names and rule forms.
- **OpenBSD `pledge`/`unveil`** — the promise-class doctrine RFC-0103
  adopted for seccomp; the operation-class mapping here is the same idea
  applied to Seatbelt's vocabulary.

## Implementation phases and evidence

1. **Provider + derivation.** `macos.rs`, `provider.rs` arm, menu entry.
   Evidence: unit tests over `Policy → SBPL` text, including the
   canonicalization cases (`/tmp` → `/private/tmp`) and the empty-policy
   case.
2. **Arming and reporting.** Evidence: a `#[cfg(target_os = "macos")]`
   child-process test in the shape of
   `linux_seccomp::tests::seccomp_denies_socket_without_network_promise` —
   arm a policy in a child, then assert `PermissionDenied` outside the grant
   and success inside it, for filesystem and for an out-of-policy port.
3. **Exec inheritance.** Evidence: a child-spawn test asserting the
   RFC-0103 `child-paths` contract holds on macOS — inherited denial by
   default, declared widening honoured.
4. **The superset canary.** Evidence: the full differential suite run on
   macOS under an armed best-effort fence. Per RFC-0103's superset
   invariant, *any* outer-fence denial during that run is a derivation bug
   and fails the build. This is also the mechanism that catches an OS
   upgrade invalidating the preamble, which is the drawback above.
5. **Docs.** `book/src/capabilities-sandbox.md` ("other hosts report the
   layer as unavailable") and its "Outer enforcement is provider-specific"
   bullet both become stale on landing and are updated with the change.

## Compatibility

No language, grant-vocabulary, or program-observable change. On Linux and
the browser, nothing moves. On macOS, `witchy sandbox` gains two `Enforced`
layers where it previously reported three `Unavailable` ones, and
`--confine=required` continues to refuse — for a newly accurate reason,
reported per-layer rather than as a flat host-level refusal.

Per the superset invariant, a program that runs today runs identically after
this lands. Any behaviour difference is a derivation bug, not a semantic
change.
