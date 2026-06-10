# Build Steps: `build.witchy`

Install-time code execution — npm's `postinstall`, cargo's `build.rs` — is the
classic supply-chain attack surface, because in those ecosystems the build script
runs with *your* full ambient authority. witchy splits the problem in two:

1. **Resolving and installing a rune executes nothing.** Source is fetched,
   hash-verified, parsed, and type-checked. There is no install hook to exploit.
2. **A rune that genuinely needs build-time codegen ships a *build step*** — and
   a build step is a witchy program, so it is governed by capabilities like
   everything else: typed, footprinted, granted per-rune, gated on widening.

## Yes, there is a `build.witchy`

A rune that needs codegen puts it in `src/build.witchy`, whose entrypoint is a
top-level `fn build` taking **only build capabilities** (its first parameter is
a `BuildOut`):

```witchy
fn build(out: BuildOut, schema: BuildRead, cc: BuildExec):
    let proto = read_build(schema, "api.proto")
    write_out(out, "api.witchy", run_tool(cc, "protoc", proto))
```

`build` is to build-time what `main` is to runtime: the single place authority
enters. The type checker enforces the separation in both directions — a runtime
capability (`Net`, `Dir`, …) in `build`'s signature is a compile error, and a
build capability in `main`'s is too. A build step's only *product* is source: it
writes generated `.witchy` into a confined output sandbox, which then flows into
the ordinary parse → link → type-check pipeline.

The five build capabilities, kind-only in the type (the *specifics* — which
directory, which tool — live in the consumer's grant):

| Capability | Grants | Operation |
|---|---|---|
| `BuildOut` | write into this rune's own confined output sandbox — the only cap granted automatically | `write_out(out, name, contents)` |
| `BuildRead` | read project files, confined to a granted subtree | `read_build(r, name)` |
| `BuildEnv` | read *named* env vars on an allow-list | `get_build_env(e, key)` |
| `BuildNet` | fetch from an allow-list of hosts | `fetch_build(n, host, path)` |
| `BuildExec` | invoke a *named* external tool (`protoc`…) — the most sensitive | `run_tool(x, tool, stdin)` |

## Safe by default: the grant

Suppose `app` depends on `genlib`, and `genlib` ships the build step above. The
moment you build, the tool refuses — *before anything runs*:

```text
error: `genlib` build step demands build capabilities you have not granted: BuildExec.
  Add a [build.grants."genlib"] entry authorizing them (read/exec/net/env).
```

`BuildOut` was granted automatically (it can only write into `genlib`'s own
sandbox); `BuildExec` was not. You authorize it — *that rune, that tool*:

```toml
[build.grants."genlib"]
exec = ["protoc"]
```

```text
build OK: `app` (1 dependency resolved, linked + type-checked)
  dependency tree max authority: build[BuildExec, BuildOut]
```

## Gated against the future: the widening

Now the scary scenario: a new version of `genlib`'s build step starts reaching
for the network. The lock catches the change (the content hash no longer
matches), and `witchy update` runs the gate:

```text
BLOCKED: this change would widen your dependency tree's capability footprint.

  + BuildNet  (build) introduced by: genlib
  (upgrade) genlib would additionally demand build[BuildNet]

No authority is granted yet — this is a conscious choice you must make.
To accept, re-run:  witchy update --allow-build-cap BuildNet
```

Accepting records the new footprint in the lock as your reviewed baseline. And
note the **two independent layers**: even after `--allow-build-cap BuildNet`,
the build *still* refuses until you also add `net = [...]` to the
`[build.grants."genlib"]` entry — the gate is "I have seen and accepted this
demand"; the grant is "and here is the attenuated instance it may actually use."
A compromised dependency that "needs network at build time" stops cold, twice.

## Running a build step

`witchy build-step` runs one directly, under exactly the grants you give on the
command line — useful for developing a build step, or for wiring generation into
CI explicitly:

```sh
witchy build-step genlib/src/build.witchy --out gen/ --read proto/ --exec protoc
```

The step runs with zero ambient authority — only the minted, confined caps. A
`write_out` to `../escape.txt` is rejected by the same path-confinement the
runtime `Dir` uses; an un-allow-listed tool is refused before it starts; an
ungranted `BuildRead` never gets minted at all.

One honest status note: today `witchy build` *gates and locks* build-time
authority but does not yet auto-execute `build.witchy` during resolution — you
run generation via `build-step` (and commit or vendor the output). Auto-running
build steps inside the zero-ambient WASM sandbox is the next piece of
[the plan](https://github.com/insanitybit/witchy/blob/master/docs/build-time-execution-plan.md).
The *preferred* path, even then, stays authoring-time codegen: run the build
once, vendor the generated source, and your consumers run no build step at all.

## Determinism, tiered

The lock's `determinism` field records what reproducibility the build can
promise: `guaranteed` when every build step is pure (the capability model
removes clocks, env, network, and randomness *by construction* unless granted),
and `pinned-only` once `BuildExec`/`BuildNet` enter — their outputs are
content-hashed into the lock, so rebuilds still pin even when the outside world
is involved.

That's the supply-chain story, end to end: authority that is typed, computed,
granted explicitly, pinned in the lock, and unable to widen silently — at runtime
*and* at build time. Next: how the three backends keep one meaning.
