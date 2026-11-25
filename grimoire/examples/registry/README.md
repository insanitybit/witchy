# Filesystem registry compatibility demo

For ordinary package development, use the [Coven HTTP quickstart](../../docs/coven-http.md).

This demo reuses the [local-run](../local-run) example: its `config-reader` package, published at an exact version to a filesystem registry, and its app, which now depends on that version by digest instead of on a sibling checkout. The app still owns its grants.

## Commands

The compiler and the host are Witchy 2 at `f951b90f` (the witchy-2 profile), a later snapshot than the local-run handoff's binaries:

- **The compiler** names a package `@name` in policies wherever its copy is, and holds each package to the packages it requires.
- **The host** opens Coven's granted paths without following links, and computes ML-DSA-65 and SHA-384 with AWS-LC's FIPS module (`libaws_lc_fips_*_crypto.dylib` beside it), for every `witchy-2` signature (the operator's and the owner's) and every content digest. `COVEN` is the launcher in a Coven checkout on `feat/witchy2-local-registry`.
- **The proof environment.** Coven's programs use the standard library, so `WITCHY_WASM_READY_BINDING` and `WITCHY_PROOF_CACHE` are set as for the compiler's own tests.

```sh
export WITCHYC=$(command -v witchyc)
export WITCHY_WASMTIME_HOST=$(command -v witchy-wasmtime-host)
export COVEN=$(command -v coven)
demo_script="$(pwd -P)/examples/registry/demo.sh"  # from the Grimoire checkout
mkdir demo && cd demo
"$demo_script"
```

[`demo.sh`](demo.sh) runs these steps, in order:

1. Make two witchy-2 keys, each a 32-byte seed only its owner can read: the registry operator's and config-reader's owner's. `coven key --key operator` writes its public key to `operator.pub` and prints its key ID (`witchy-2:` and 96 hex digits). Copy `config-reader` into `publisher/` and the app into `consumer/`. Delete the app's `main.packages` and add to its `witchy.toml` `[registry]` (the operator's key ID, `operator = "witchy-2:..."`, and the registry's identifier, `id = "demo"`), `[dependencies]`, and `[owners]` (the owner's key ID).
2. `coven publish publisher/config-reader --registry registry --key owner`, which signs the release and the package's index in the engine. Then append a line to `reader.wy` and publish 0.1.0 again. The registry's operator then signs a snapshot (`coven snapshot --registry registry --key operator --id demo`), which publishes 0.1.0 to clients.
3. `grimoire lock consumer/app`, `rm -r publisher`, then `grimoire run consumer/app`.
4. Change the registry copy and run, with a compiler wrapper that logs each invocation. Rewrite the registry's digest record (`sha384:`) to match the change and run again: the owner's release signature no longer verifies for it.
5. Replace `main.grants.toml` with an empty `[files]` table and run. Then restore it.
6. Change `main.policy` so that `@config-reader` may use nothing, and run. Then restore it.
7. Publish 0.1.1 with one more function, and snapshot it. Depend on it, and `grimoire lock`. Run, then `grimoire review --accept`, then run again.

Transitive graphs (`config-reader` requiring `digits`) are covered by `tests/registry_run.py`. Those tests also cover a conflicting version, a lock missing a package, and a release deep in the graph that runs only after review.

## Observed output

This is from 2026-11-20 on macOS, with Wasmtime 49.0.3 in the host.

```text
== 1. publish config-reader 0.1.0, signed by its owner; publishing other source as 0.1.0 fails
config-reader 0.1.0: published sha384:b77b9d80e37dc5dfc3966f736978fb00a3b0c4990f8d43b1ca96507c5b5d5ddcaf4b2c44191512cdaf231c14f8806017
config-reader 0.1.0: signed by witchy-2:5aaf45d1f5b980fce634ac40c5cc86afea797a61a12a64d9ca606d449a917ae2215ceb543184a6f8b7f2320a8fa4aa58
config-reader: index 1
config-reader 0.1.0: refused: this version is already published with different source or requirements (sha384:5a6e58c11183758932ea4c3e4bbb65e52048837297c4e666c9cfba4eeb6d8d874b39c43f42685bc0322ca9454dbf82ea is not the recorded digest)
exit 1
-- the operator signs the snapshot that publishes it to clients
config-reader: index 1
snapshot 1 of registry demo, valid for 24 hours
== 2. lock, remove the publisher checkout, run
locked config-reader 0.1.0 sha384:b77b9d80e37dc5dfc3966f736978fb00a3b0c4990f8d43b1ca96507c5b5d5ddcaf4b2c44191512cdaf231c14f8806017 (required by witchy.toml)
8080
exit 0
== 3. change the registry copy after locking: refused before compilation
config-reader 0.1.0: refused: the registry's package does not match its recorded digest (it hashes to sha384:4f213aa3f33068cd150e63ef34cee490baab3902c1bb0253b5f7381d676b6acaa15a4f4b7ed58c0093a7215623b2f590)
grimoire: config-reader 0.1.0 was not fetched from the registry
exit 1
coven: config-reader 0.1.0: the release signature does not verify for sha384:4f213aa3f33068cd150e63ef34cee490baab3902c1bb0253b5f7381d676b6acaa15a4f4b7ed58c0093a7215623b2f590
grimoire: config-reader 0.1.0 was not fetched from the registry
exit 1
compiler invocations during both refused runs:
  witchyc wasm-grants verify.wy
  witchyc build verify.wy
  witchyc wasm-grants verify.wy
  witchyc wasm-grants fetch.wy
  witchyc build fetch.wy
  witchyc wasm-grants verify.wy
  witchyc wasm-grants verify.wy
== 4. without the app's file grant, the locked dependency cannot read settings.conf
error: config: main takes a File<Read>, and the document grants none ([files].config)
exit 1
== 5. the app's policy holds the registry copy: config-reader given nothing is refused
W0100 0..0: $TMPDIR/grimoire-…/app/main.policy: the program does not fit its policy: @config-reader can use File<Read>, which the policy does not allow
exit 1
== 6. an update locks, and runs only after the app's author reviews it
config-reader 0.1.1: published sha384:338892c66bccec47c5496322dc8664485caa96087dbd1cf0c102c11f1b742b9bae00532ddeaeac363cb5ca2a7f4b8096
config-reader 0.1.1: signed by witchy-2:5aaf45d1f5b980fce634ac40c5cc86afea797a61a12a64d9ca606d449a917ae2215ceb543184a6f8b7f2320a8fa4aa58
config-reader: index 2
config-reader: index 2
snapshot 2 of registry demo, valid for 24 hours
locked config-reader 0.1.1 sha384:338892c66bccec47c5496322dc8664485caa96087dbd1cf0c102c11f1b742b9bae00532ddeaeac363cb5ca2a7f4b8096 (required by witchy.toml)
W0100 0..0: $TMPDIR/grimoire-…/app/main.policy: the program does not fit its policy: @config-reader has changed since its line was reviewed: its digest is now sha384:11e22b1c0371c79eb5c96af6aebe4f64f62a889d2b0cde563ae3d2d2f6dcc8abec6518f179e2edd22a2c744b9479d027
exit 1
@config-reader: 1 reachable functions; can use File<Read>
  no unchecked memory operations
main.wy: 1 reachable functions; can use Console<Write>, File<Read>
  no unchecked memory operations
repinned @config-reader: sha384:a34caf00e91722c0faa689cdeaba7587bbb4376e1a1569066111e2c06d26e677595c7b62e816697a68798c75506d5066 -> sha384:11e22b1c0371c79eb5c96af6aebe4f64f62a889d2b0cde563ae3d2d2f6dcc8abec6518f179e2edd22a2c744b9479d027
fits the policy
exit 0
8080
exit 0
```

In both refused runs of step 3, the only programs compiled were Coven's own: `verify.wy`, which checks the witchy-2 signatures (the operator's snapshot, and the owner's index and release) in the engine, and `fetch.wy`. The first run is refused by `fetch.wy` (the copy does not hash to the recorded digest); the second, with the digest record rewritten, by the release signature, which the owner made over the original digest. The app's `main.wy` was never built, and Wasmtime never ran it. In step 4 the run stops at the grant check. `config-reader` gets a file only as the `&File<Read>` that `main` hands it, and Witchy 2 code has no ambient file access.

Step 5 and step 6 are the app's policy, `main.policy`, at work. It is applied before anything is proved, and the host is never started:

- **Step 5.** The policy gives `config-reader` nothing, so it may not use the file that `main` holds and passes it.
- **Step 6.** The policy pins the reviewed code by digest. A new version locks but does not run until the author reviews it. `grimoire review` shows what each file of the locked graph uses, and `--accept` records the new digest. Accepting changes no capability: a version that reached further would still be refused until the author edits its line.

## How it works

**Registry layout.** A version is `registry/NAME/VERSION/{package, source.wy, digest}`.
- `package` is the version's record: format, name, version, and one `requires NAME VERSION` line per dependency.
- `digest` holds `sha384:<hex>` of `package` followed by `source.wy`.
- A release of several files (`witchy-package 2`) uses the same slot: `package` is the record, which lists the entry and each file's path, length and SHA-384, and `source.wy` is the files' bytes in that order. The digest is unchanged in meaning, so it covers every file. See [multi-file packages](../../docs/multi-file-packages.md).

A digest therefore pins the coordinate and the exact requirements as well as the source. A registry cannot serve one package's files as another's, or drop a requirement unnoticed.

**The lock.** `witchy.lock` records every package of the graph with its version, digest and own `requires`, so a reader sees which package brought each one in.
- **Kept pins.** `grimoire lock` keeps the digest of a version it already pins, so the same name and version cannot later stand for other bytes.
- **One version per name.** It refuses two versions of one package, and names both chains.
- **Atomic.** It replaces the lock whole.
- **Frozen runs.** `grimoire run` resolves the graph again from the verified records, against the lock, and refuses a lock that misses or adds a package.

**The policy.** The executable's `main.policy` names each package `@name`, whether it comes from a checkout (`main.packages`) or the registry.
- **Required.** Grimoire stages the policy beside the staged entry, and refuses to run any package, from either source, without one.
- **Reviewed digest.** A policy line's digest is `sha384:` and the SHA-384 of the package's source file, the same in both modes.
- **Confined imports.** The compiler confines a package's imports to its own directory, so a build reads only the locked bytes.
- **`grimoire review`.** It shows what each file of the locked graph uses, and drafts a missing policy. `--accept` repins the digests and keeps the result only if the program then fits.

**Registry programs.** These are Witchy 2 programs in Coven's `witchy2/` directory, run in Wasmtime with only the paths each needs.

- **`publish.wy`** gets one bundle, the record and then the source, because `main` may take only one `File<Read>`.
  - It reads the bundle once (at most 1 MiB), hashes it with the engine's SHA-384, and stages those bytes split into `package` and `source.wy`, then claims the version by creating `digest` exclusively.
  - If the version is already claimed, the same bundle is stored again and other bytes are refused. Storing again completes a publish that stopped after its claim.
  - Staged files reach the disk only when finished, so a refused publish writes nothing.
- **`fetch.wy`** reads `package` and `source.wy` once, checks the approved SHA-384 digest, coordinate, record boundary, and package contents, and stages those bytes into the consumer's cache.
  - It keeps the copies only if the hash equals both the registry's record and the locked digest.
  - The bytes Grimoire compiles are therefore the bytes that were checked.
- **`digest.wy`** prints a file's `sha384:` digest; Coven's tests compare the engine's SHA-384 with Python's `hashlib` at SHA-384's block boundaries.
- **`verify.wy`** checks `witchy-2` (ML-DSA-65) signatures with `witchy2_verify`, which the engine computes, for a fixed list of roles; **`sign.wy`** signs with a `Signer` granted `sign/witchy-2/ROLE` for the one role Coven asks (`snapshot`, `release`, `index`, ...), the seed on a pipe of its own.

## Limitations

**Host bootstrap.** These parts are Python, not Witchy 2:

- Coven's launcher reads the package's TOML, checks the name, exact version and requirements (each must be published), writes the canonical `package` record, creates `NAME/VERSION/`, and writes each program's grant document.
- Grimoire reads the manifest and lock, parses each verified `package` record, resolves the graph, writes `witchy.lock`, and stages the app's `.wy` files beside a generated `main.packages`. The compiler reads the package list only from beside the entry, and canonicalizes symlinks.

**Language gaps.**

- Every Coven program uses `Vec`, `fs` and `crypto`, whose proofs need the proof environment. The package programs used to avoid them and hash in Witchy code (a streaming SHA-256); the profile's digest is the engine's SHA-384, which takes a whole slice, so a version (record and source) is read into memory and is at most 1 MiB.
- A program has no arguments. Names, versions and the expected digest reach it as granted paths and a granted file.
- `main` takes at most one parameter of each capability type, so publish gets the record and source as one bundle.

**Trust and scope.**

- The registry is local. This demo's package is signed by its owner, and the app declares both keys. Package indexes, owner pins, rotation and recovery, and the registry snapshot are described in the [Grimoire README](../../README.md#registry-packages) and Coven's.
- There is no staging, promotion, yanking or footprint review. These are what the Witchy 1 Coven prototype does.
- A package is its entry source and the files of its own directory that it imports. Dependencies are exact versions, with no ranges. The compiler holds each package to the packages its record requires: Grimoire writes them into the generated list, so a lock's edges are the build's.
- The compiler reads the staged copies by path, in a private temporary directory, rather than through a sealed handle.
- Coven refuses a symlink at a version's `NAME` or `NAME/VERSION` directory. Its programs' grants say `[paths] links = false`, so the host opens each granted path from `/` without following a link. A slot swapped for a link after Coven's checks is refused, not followed.
- Grimoire refuses a dependency cycle. Coven never publishes one, but files written straight into the registry can.
- A publish that crashes between claiming `digest` and finishing `source.wy` leaves a claimed version with no source. Fetch refuses it until the same source is published again (Coven `witchy2/test_registry.py`). Only the original bytes can complete it.
- Coven caches its built programs per user, by content; Grimoire rebuilds the app on each run.
