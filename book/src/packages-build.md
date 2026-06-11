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
directory, which tool, which variable — live in the consumer's grant):

| Capability | Grants | Operation |
|---|---|---|
| `BuildOut` | write into this rune's own confined output sandbox — needs no naming once execution is accepted | `write_out(out, name, contents)` |
| `BuildRead` | read project files, confined to a granted subtree | `read_build(r, name)` |
| `BuildEnv` | read env vars — but **only the keys named** in the grant, never the whole environment | `get_build_env(e, key)` |
| `BuildNet` | fetch from an allow-list of hosts | `fetch_build(n, host, path)` |
| `BuildExec` | invoke a *named* external tool (`protoc`…) — the most sensitive | `run_tool(x, tool, stdin)` |

## Default deny — for *execution itself*

Suppose `app` depends on `genlib`, and `genlib` ships the build step above. The
first refusal isn't about which capabilities it wants — it's about the fact that
it wants to run code at all:

```text
error: `genlib` ships a build step, and build-time code execution is denied by default.
  Add a [build.grants."genlib"] section to accept it — an empty section permits only
  the confined output sandbox (BuildOut); name read/exec/net/env kinds to grant more.
```

You consent to *any* code execution before you consent to *safe* code execution.
Even a build step that demands nothing beyond its own confined `BuildOut` sandbox
is refused until you write the section — an **empty** `[build.grants."genlib"]`
is that consent, and it permits only `BuildOut`. With the section present, the
second layer kicks in: every kind beyond `BuildOut` must be named.

```text
error: `genlib` build step demands build capabilities you have not granted: BuildExec.
  Add them to [build.grants."genlib"] (read/exec/net/env).
```

So you authorize it — *that rune, that tool*:

```toml
[build.grants."genlib"]
exec = ["protoc"]
```

```text
build OK: `app` (1 dependency resolved, linked + type-checked)
  dependency tree max authority: build[BuildExec, BuildOut]
```

(For env vars the same shape applies: `env = ["TARGET"]` lets the step read
`TARGET` and nothing else — `get_build_env` refuses any key the grant doesn't
name, even if it exists in your environment.)

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

Accepting records the new footprint in the lock as your reviewed baseline.
(`witchy add` runs the same gate when a dependency *first* introduces a
build-axis kind — even `BuildOut` alone must be `--allow-build-cap`'d in.) And
note the **two independent layers**: even after `--allow-build-cap BuildNet`,
the build *still* refuses until you also add `net = [...]` to the
`[build.grants."genlib"]` entry — the gate is "I have seen and accepted this
demand"; the grant is "and here is the attenuated instance it may actually use."
A compromised dependency that "needs network at build time" stops cold, twice.

## Build steps run during `witchy build` — audited before and after

With the grants in place, `witchy build`/`run` execute a dependency's build step
automatically: the `build` module is *excluded* from your program's link, run
confined under exactly its `[build.grants]`, and the `.witchy` source it emits
joins the link like any module — your code can `import` it directly.

And because **generated code is still code**, the pipeline recomputes the rune's
footprint over its shipped *plus generated* source and gates it against the
locked baseline. A build step that tries to smuggle authority in by *generating*
capability-hungry code is refused:

```text
error: `genlib`'s build step generated source that WIDENS its footprint (+ Net)
beyond the locked baseline — refusing. Generated code is still code; it cannot
smuggle in authority the version was not accepted with.
```

You can also run a step directly — useful while developing one, or to vendor its
output instead of running it on every build (the *preferred* path for published
runes: run the build once, vendor the generated source, and your consumers run
no build step at all):

```sh
witchy build-step genlib/src/build.witchy --out gen/ --read proto/ --exec protoc
```

Either way the step runs with zero ambient authority — only the minted, confined
caps. A `write_out` to `../escape.txt` is rejected by the same path-confinement
the runtime `Dir` uses; an un-allow-listed tool is refused before it starts; an
ungranted `BuildRead` never gets minted at all. A **deterministic** step (one that
only writes generated source and reads project files) runs in the **zero-ambient
WASM sandbox**: it is compiled and instantiated with *only* its `write_out` /
`read_build` host functions linked, so the dangerous host functions don't even
exist for it to call. Steps that `exec` a tool or hit the network run on the
interpreter, where the WASM boundary adds nothing — the dangerous operation is a
native process or socket, gated identically by the allow-list either way.

## Determinism, tiered

The lock's `determinism` field records what reproducibility the build can
promise: `guaranteed` when every build step is pure (the capability model
removes clocks, env, network, and randomness *by construction* unless granted),
and `pinned-only` once `BuildExec`/`BuildNet` enter. Deterministic steps are
also *cached*: a content hash over the build source and its granted inputs keys
the output, so an unchanged step never re-runs — while a step that touches the
outside world re-runs every time, since its output may depend on external
state.

That's the supply-chain story, end to end: authority that is typed, computed,
granted explicitly, pinned in the lock, and unable to widen silently — at runtime
*and* at build time. Next: how the three backends keep one meaning.
