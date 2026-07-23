# Shipping and Deployment

Witchy separates a program from the authority used to run it. That makes the
first distribution decision straightforward:

- Ship **portable WebAssembly** when the recipient should choose every grant.
- Ship a **trusted executable** when running the installed application is
  itself the trust decision.
- Ship a **static browser bundle** when the browser is the host and its
  capability menu is sufficient.

These forms share compiled Witchy semantics, but their host and trust boundaries
are intentionally different.

## Portable WebAssembly

Compile a source entrypoint to a standalone module:

```sh
witchy compile app.witchy --out app.wasm
```

The module contains no ambient operating-system authority. Its imports describe
what a host must provide. The recipient can run it under the reference sandbox
with exactly the grants they accept:

```sh
witchy sandbox --dir ./data app.wasm report.txt
witchy sandbox --fetch https://api.example.com app.wasm
```

`Fetch` is the portable HTTP(S) root. Browser hosts provide it through
`fetch()`, while native hosts provide it directly or derive it by narrowing
`Net`. In both cases it carries an origin allowlist, rejects cross-origin
requests before I/O, and treats redirects uniformly rather than silently
widening authority.

For larger policies, distribute a suggested grant document separately from the
module so the recipient can inspect and edit it:

```toml
[dirs]
data = { root = "./data", rights = ["Read"] }

[fetch]
api = ["https://api.example.com"]

[env]
environment = ["LANG"]
```

```sh
witchy grants-check app.witchy app.grants.toml
witchy sandbox --grants app.grants.toml app.witchy
```

The source-level `grants-check` recomputes the footprint rather than trusting a
manifest claim. At launch, each named grant binds to the same-named parameter
of `main`; missing authority is an error and surplus authority is reported.

### Defense in depth

The WebAssembly host interface is the primary capability boundary. Native
sandbox launches also derive an outer policy from the same resolved grants:

- filesystem roots use descriptor/handle-relative operations rather than
  canonicalize-then-open paths;
- supported Linux hosts add Landlock filesystem/TCP restrictions;
- supported Linux architectures add a thread-synchronized seccomp promise
  filter for filesystem, network, process, and bypass-prone syscall classes;
- executable children are selected through already-open authority and inherit
  the outer fence.

Best-effort mode reports unavailable or partial layers and continues with the
guest capability boundary intact. Required mode fails before `main` unless
every implemented layer for that host is fully enforced:

```sh
witchy sandbox --confine=required --dir ./data app.wasm
```

Required mode is a deployment assertion, not a portability promise. A portable
module can run on a host that lacks a particular kernel fence; a deployment
that requires that fence must reject such a host.

## Trusted executables

A trusted executable packages the host launcher, compiled WebAssembly, and a
checked binding plan into one native file:

```sh
witchy --release build --target trusted-exe
```

The default output is `target/release/<rune-name>`. The destination does not
need a Witchy toolchain or separate `.wasm`, but running the file trusts the
application, its embedded runtime, and its distributor. It is an installation
artifact, not a way to make an untrusted author safe.

### Bind `main` in the manifest

Every resource parameter must have an unambiguous provider under
`[targets.trusted-exe]`:

```toml
[targets.trusted-exe]
confine = "required"

[targets.trusted-exe.dirs]
workspace = { from = "cwd" }

[targets.trusted-exe.files]
config = { from = "path", path = "./app.toml" }

[targets.trusted-exe.fetch]
api = { from = "allow", origins = ["https://api.example.com"] }

[targets.trusted-exe.env]
environment = { from = "system", names = ["HOME", "LANG"] }

[targets.trusted-exe.exec]
runner = { from = "allow", programs = ["git"] }

[targets.trusted-exe.secrets]
token = { from = "env:APP_TOKEN", use-only = true }
```

Directory and file bindings retain their opened authority from admission
through runtime construction; replacing a parent name after admission cannot
redirect them. Declared type rights still apply, so a `Dir[Read]` binding does
not gain write access. `Env` names are explicit, `Fetch` origins and `Exec`
programs are allowlisted, and a dependency cannot widen the application's
authenticated root binding plan.

`Console`, `Clock`, `Rand`, argv, and an empty `SecretStore` use conventional
process providers. Bare `Secret`, `NativeLoader`, and user-defined grantable
roots are rejected until a safe provider exists. The build fails instead of
guessing.

The launcher validates a versioned descriptor, host ABI, embedded payload, and
SHA-256 digests before granting authority. Tampering or truncation therefore
fails before application execution.

### Example: minigrep

The repository's `examples/minigrep` binds its read-only `root` to the launch
directory and grants only the `IGNORE_CASE` environment name:

```toml
[targets.trusted-exe.dirs]
root = { from = "cwd" }

[targets.trusted-exe.env]
env = { from = "system", names = ["IGNORE_CASE"] }
```

Build and move it like an ordinary native tool:

```sh
witchy --release build --target trusted-exe examples/minigrep
cp examples/minigrep/target/release/minigrep ~/bin/minigrep
cd ~/notes
minigrep nobody poem.txt
```

`root` follows the directory at launch, not the build or installation
directory, and remains confined to that opened subtree.

## Static browser bundles

The repository can build this book and its Glamour docs application as static
files:

```sh
./scripts/build-docs.sh dist
python3 -m http.server -d dist 8000
```

The bundle contains the browser compiler, docs application, classified example
manifest, and fixed child-frame bootstrap. Supported cells run in fresh opaque
frames. Their derived CSP uses `connect-src` for exactly the granted `Fetch`
origins, providing a browser analogue to native outer confinement. Unsupported
native capabilities remain visibly non-runnable.

The browser host is not interchangeable with the native host: it has no raw
`Net`, `Exec`, or host filesystem root. Write shared code against portable
roots such as `Fetch`, use optional capabilities when graceful degradation is
part of the API, or expose host-specific entrypoints with distinct footprints.

## What is not a shipping promise

The package manager, Coven registry, trusted publishing, and hosted browser
deployment are substantial dogfood, but they are not a supported public
distribution service. Witchy 0.1.0 is still a private release candidate until
the exact queue-settled commit passes the release workflow and native platform
matrix and receives explicit publication approval.

Use the repository's
[product-status ledger](https://github.com/insanitybit/witchy/blob/master/PRODUCT-STATUS.md)
for supported-preview versus experimental surfaces, and
[release-readiness ledger](https://github.com/insanitybit/witchy/blob/master/RELEASE-READINESS.md)
for candidate evidence. RFC status alone is not release evidence.
