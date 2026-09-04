---
rfc: 0166
title: Symbolic grant directory providers ($tmp, $home, and $cwd)
status: proposed
created: 2026-09-04
tracking:
---

# RFC-0166: Symbolic grant directory providers ($tmp, $home, and $cwd)

## Summary

Witchy capability grant documents (`grants.toml`, [RFC-0013](0013-capability-grant-documents.md))
and CLI sandbox invocations (`witchy sandbox --dir`) currently require literal on-disk
filesystem paths for `Dir` capability roots (e.g. `root = "/tmp"` or `root = "./data"`).

This RFC proposes **symbolic directory providers** for grant definitions:
- `root = "$tmp"` (or `"$scratch"`): A private, ephemeral temporary directory provisioned
  by the host runtime and automatically cleaned up on process exit.
- `root = "$home"` (and subpaths like `"$home/.config/myapp"`): A portable reference to the
  user's home directory, resolved by the host at grant admission time.
- `root = "$cwd"` (and subpaths like `"$cwd/data"`): Explicit resolution against the process
  current working directory.

These providers enable portable, cross-platform grant manifests that run without editing
across developer workstations, CI runners, and production sandboxes while strengthening
capability confinement.

## Motivation

### 1. Literal paths break portability
A grant document checked into a repository cannot hardcode `/Users/username` or `/home/user`.
Even `/tmp` is unportable across operating systems: macOS uses `/var/folders/...`, Linux uses
`/tmp`, and Windows uses `%TEMP%` or `%LOCALAPPDATA%\Temp`. When a grant document requires a
temporary directory or a user cache directory, authors are forced to rely on manual script
wrappers rather than checking in a reusable, declarative `grants.toml`.

### 2. Ambient `/tmp` is insecure and collision-prone
Setting `root = "/tmp"` grants authority over the shared global `/tmp` volume. This exposes
unrelated files from other host processes, risks symlink attacks and race conditions (TOCTOU),
and leaves abandoned files behind if the program aborts. What temporary workloads actually
want is a private, isolated directory tree that exists only for the lifetime of the process.

### 3. Home directory access without over-granting `Env`
Today, accessing files under a user's home directory forces authors into an awkward dilemma:
- Hardcode the user's absolute path into `grants.toml` (unportable), or
- Request the broad `Env` capability so the code can read `HOME` at runtime. But `Env` grants
  access to ambient environment variables (including sensitive tokens and keys), and reading
  `HOME` still only gives a path string, not an unforgeable `Dir` capability.

Allowing a grant to declare `root = "$home/.cache/myapp"` cleanly solves this: the program
receives a `Dir` capability strictly confined to its own application cache, without receiving
access to the rest of the user's home directory and without needing the `Env` capability at all.

## Design

### Grant document syntax

In `grants.toml`, the `root` field of a `[dirs.<name>]` section recognizes symbolic provider
prefixes:

```toml
# app.grants.toml

# Private ephemeral scratch directory; cleaned up automatically on exit
[dirs.scratch]
root = "$tmp"
rights = ["Read", "Write"]

# Confined user configuration directory under $HOME
[dirs.config]
root = "$home/.config/myapp"
rights = ["Read", "Write"]

# Read-only access to current working directory
[dirs.project]
root = "$cwd"
rights = ["Read"]
```

### CLI sandbox syntax

The same symbolic tokens are supported directly on the CLI:

```sh
# Run with an isolated ephemeral temporary directory
witchy sandbox --dir $tmp main.witchy

# Run with a confined subdirectory under user home
witchy sandbox --dir $home/.cache/myapp main.witchy
```

*(Note: Shells that do not define `$tmp` pass the literal string `$tmp` to `witchy`, which
recognizes the symbolic token. If a shell environment variable of the same name exists,
standard shell expansion remains compatible).*

### Provider semantics

#### 1. `$tmp` / `$scratch`
- **Provisioning**: The host runtime provisions a fresh, private temporary directory using
  `tempfile::Builder::new().prefix("witchy-tmp-").tempdir()`.
- **Lifecycle**: The runtime session retains ownership of the temporary directory guard for
  the entire duration of the process.
- **Cleanup**: When the Witchy process terminates (normally or via a trap/panic), the guard
  is dropped, removing the temporary directory and all contents from the host filesystem.
- **Confinement**: The granted `Dir` capability is confined to that private directory instance.
  It cannot navigate above the temporary directory into the parent system temp directory.

#### 2. `$home`
- **Resolution**: Resolved host-side via `std::env::var_os("HOME")` on Unix and `USERPROFILE`
  (or `FOLDERID_Profile`) on Windows.
- **Subpaths**: Optional subpaths (e.g. `$home/.config/myapp` or `$home/Library/Caches/myapp`)
  are resolved relative to the resolved home path.
- **Parent Traversal Rejection**: Traversal tokens (`..`) that attempt to navigate above the
  home directory are rejected during grant admission.
- **Directory Creation**: If the target subpath does not exist:
  - If the grant includes `Write` rights, the host creates the directory hierarchy
    (equivalent to `mkdir -p`).
  - If the grant is `Read`-only, admission fails with an error explaining the missing path.

#### 3. `$cwd`
- **Resolution**: Resolved to the current working directory of the invoking process at grant
  admission time.
- **Subpaths**: `$cwd/subpath` resolves to `<cwd>/subpath`.

### Capability cross-check integration

`witchy grants-check <prog.witchy> <grants.toml>` validates symbolic provider declarations:
- Confirms that the declared rights (`Read`, `Write`) match the program's static footprint.
- Confirms that required host environmental variables (such as `$HOME`) are available if the
  check is performed in an active host environment.

## Confinement and Security Invariants

1. **Monotonic attenuation preserved**: A program granted `$home/.config/myapp` can never
   read or write `~/.ssh` or other sibling directories; the `Dir` capability anchor is placed
   at the subpath.
2. **Zero ambient leak to guest code**: Guest Witchy code receives an opaque `Dir` handle. The
   guest does not receive the raw host path string unless explicitly granted `Env`.
3. **Hermetic test runs**: Tests and automated benchmarks can declare `root = "$tmp"`,
   guaranteeing test isolation and zero leftover filesystem pollution.

## Alternatives Considered

1. **Continue relying on shell expansion (`witchy sandbox --dir $(mktemp -d)`)**:
   Works for manual terminal commands, but fails for declarative `grants.toml` files checked
   into version control and for packaged application manifests.
2. **Granting `Env` and opening paths dynamically in code**:
   Violates the core Witchy capability principle: ambient paths are consumed once by the host
   at launch, and guest code operates exclusively on capability handles. Guest code cannot
   synthesize or ambiently open arbitrary filesystem paths.

## Prior Art

- [RFC-0013](0013-capability-grant-documents.md): Capability grant documents.
- [RFC-0092](0092-trusted-application-executables.md): Trusted application executables.
- [RFC-0095](0095-grimoire-trusted-application-installation.md): Grimoire application
  installation and `$WITCHY_HOME`.
- WebAssembly System Interface (WASI) filesystem pre-opens.
