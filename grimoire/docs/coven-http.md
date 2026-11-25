# Publish a Witchy library and run a clean consumer

`witchyc` compiles Witchy to core WebAssembly. `grimoire` builds, tests, locks, publishes and runs projects. `coven serve` is the registry server. The Wasmtime host executes the module with the grants owned by the executable.

## Install the pre-release tools

### Install from a pinned client bundle

Use a prebuilt bundle to install on a consumer machine without the
monorepo, Rust, MBX, or a Cargo cache. The standalone installer uses
Python 3.11 or newer and, on macOS, `otool` to check the Wasmtime host's
library paths.

On a trusted build machine, build `witchyc` and `witchy-wasmtime-host`
in release mode with MBX from the same revision as the Python tools.
Pass the absolute paths of those binaries to the bundler. On macOS,
keep the host's AWS-LC dylib in its build directory or beside the host.

```sh
python3 grimoire/install.py --make-bundle witchy-client.zip \
  --compiler-binary "$compiler" --host-binary "$wasmtime_host" --coven coven
```

For a client ready to contact a TUF mirror, build and check the four
[TUF workers](../../coven/witchy2/tuf.md#fetch-and-discover-releases-without-a-compiler)
with the matching toolchain. Pass `--tuf-workers /path/to/worker-directory`
to `--make-bundle`. The installer carries their exact bytes and digest
pins in the independently pinned ZIP. The installed Coven selects these
workers for TUF operations. On every invocation, the Wasmtime host checks
the module bytes against the pin before running them. The consumer still
supplies registry trust anchors and a checker for building application source.

For compiler-free package packing, also build Coven's bounded TOML
manifest worker on the trusted build machine. Check its capability
policy and pass the resulting Wasm module to the bundler as
`--manifest-worker /path/to/read_package_manifest.wasm`.

The current manifest worker validates the package name, exact version,
and each dependency before returning a success receipt with the manifest
digest. It refuses reserved names and self-dependencies. The launcher
holds the package directory while it makes a bounded manifest snapshot
for the worker. It checks the returned digest and reads source files
through that same directory. The launcher checks the source path stays
inside the package and checks coordinates itself for earlier pinned
workers' receipts.

```sh
reader=coven/witchy2/read_package_manifest
"$compiler" wasm-caps --policy "$reader.policy" "$reader.wy"
"$compiler" build "$reader.wy" -o read_package_manifest.wasm \
  --heap-pages=512 --slots=4096 --stack-bytes=65536
```

The installed Coven passes the worker's bundled pin to the host for
each manifest read. The worker can read the one manifest, allocate
memory, and return the projected package fields and the exact SHA-384
of the original bytes. The installer checks that the worker parses a
synthetic manifest before publishing the prefix.

Provision the SHA-384 pin of the exact `grimoire/install.py` from the
trusted build checkout and the bundle pin printed by `--make-bundle`.
Use an authenticated release channel independent of the mirror under
test. On the build machine, compute the installer pin with
`shasum -a 384 grimoire/install.py` or `sha384sum grimoire/install.py` and
prefix the hexadecimal digest with `sha384:`.

On the consumer, save the standalone installer as `install.py` and
set `installer_pin` and `bundle_pin` to the independently supplied pins.
Verify the installer before executing it.

```sh
(
  hex="${installer_pin#sha384:}"
  case "$hex" in
    *[!0-9a-f]*) exit 1 ;;
  esac
  test "$installer_pin" = "sha384:$hex" && test "${#hex}" -eq 96 || exit 1
  if command -v sha384sum >/dev/null 2>&1; then
    set -- sha384sum
  else
    set -- shasum -a 384
  fi
  printf '%s  install.py\n' "$hex" | "$@" -c - &&
    python3 install.py --from-bundle witchy-client.zip --bundle-pin "$bundle_pin" \
      --prefix "$HOME/.local/witchy"
) && export PATH="$HOME/.local/witchy/bin:$PATH"
```

The installer checks the ZIP's exact bytes, target platform, and contents.
Keep the reviewed installer, bundle and independent bundle pin. To
check the installed bundle members and launcher links without executing
them, run:

```sh
python3 install.py --verify-bundle witchy-client.zip --bundle-pin "$bundle_pin" \
  --prefix "$HOME/.local/witchy"
```

Verification compares the complete prefix with the pinned bundle and
its two launcher links. It refuses unlisted files or directories and
hard-linked bundle members. The launchers leave the prefix free of
generated Python bytecode. Use a fresh dedicated prefix and a trusted
Python environment. Rebuild older bundles whose launchers write bytecode
inside the prefix before using the exact-tree verification workflow.

On macOS and Linux, it stages the bundle in a private directory beside
the requested prefix and publishes it after the requested checks pass.
The parent directory must protect the staged tree from other users.
The filesystem must support an atomic no-replace directory rename.
A failed check leaves the requested prefix available for retry. The
installer refuses an existing path, including a dangling symlink.
It checks that the installed Wasmtime host starts and loads its crypto
library. If the bundle includes TUF workers, the installer starts the pinned selector, HTTPS and loopback downloaders,
and release verifier on that host. Each must refuse a synthetic request
without publishing an output file. The downloaders have a zero-byte
network budget and only loopback port zero in scope. This check uses no
registry credentials and runs even with `--no-self-check`. The Grimoire
self-check runs a fresh proved slice access and requires a false
postcondition to fail before it emits a module. To defer it until a
trusted proof checker is available, add `--no-self-check`, then use
`python3 install.py --check-only --prefix "$HOME/.local/witchy"`.
The proof toolchain is provisioned separately as described below. A
consumer can use a compatible full Lean SDK without building a helper.
Before selecting a provisioned SDK, compare the output of
`witchyc wasm-toolchain-id /absolute/lean-sdk` with the SDK identity
you obtained independently from a trusted release channel. Then
follow [Consume releases through Grimoire](../../coven/witchy2/tuf.md#consume-releases-through-grimoire)
for the HTTPS registry, TUF root, release trust, and lock workflow.

### Build and install from source

Start in the Witchy repository root containing `witchyc/`, `grimoire/` and `coven/`. The installation uses Python 3.11+, Rust, MBX and the Wasmtime host's dependencies already in the local Cargo or MBX cache: it builds offline and downloads nothing. The measured platform is macOS arm64.

`install.py` builds `witchyc` and `witchy-wasmtime-host` with MBX, and installs into one prefix under your home directory (default `~/.local/witchy`; never a system directory, and nothing is written through a symbolic link): the two binaries and, beside the engine, AWS-LC's FIPS crypto library it loads (macOS: found at `@executable_path` only, checked with `otool -L`, no `DYLD_` variable), and Grimoire and Coven with every Python module each loads and Coven's Witchy sources. The temporary-project self-check exercises both proof admission and library builds. It uses the proof toolchain of the next step.

```sh
src=$(pwd -P)
prefix="$HOME/.local/witchy"
python3 "$src/grimoire/install.py" --witchy "$src/witchyc" --coven "$src/coven" --prefix "$prefix" --no-self-check
export PATH="$prefix/bin:$PATH"
```

Registry guests use Witchy proofs. For fresh proof checking, point `WITCHY_WASM_LEAN_ROOT` to an absolute trusted Lean installation. For a prepared proof session, use an installed operator-built proof helper and SDK with an absolute canonical `WITCHY_WASM_READY_BINDING` path. To build a binding with an installed compatible Lean SDK, set `WITCHY_LEAN_SDK` to its absolute path and use a new directory. Keep the installed generation read-only as required by the selected proof toolchain. Each account can set `WITCHY_PROOF_CACHE` to its own existing private absolute directory.

```sh
python3 "$src/witchyc/scripts/build-proof-session-helper.py" \
  --compiler "$prefix/bin/witchyc" --sdk "$WITCHY_LEAN_SDK" \
  --out "$prefix/checker" --body-interface
export WITCHY_WASM_READY_BINDING="$prefix/checker/ready-binding.txt"
python3 "$src/grimoire/install.py" --check-only --prefix "$prefix"
witchyc --help
grimoire --help
coven serve --help
```

With a binding already exported, one `install.py` call without `--no-self-check` installs and checks. A failed build says so (offline: a missing dependency is not fetched); a module the tools load that is not in the checkout stops the install before anything is written.

## Start Coven and establish the operator

The example uses separate homes for service, operator, author and consumer. Run the commands in the same shell. The registry listens on IPv4 loopback port 8787. The compatibility HTTP client
runs a Witchy worker granted only that loopback endpoint, one private upload
file, and one private response file. A 60-second host deadline covers the
entire exchange, including a response that keeps making progress. The worker
commits its private response only after it passes HTTP framing and the
16 MiB body limit. The existing signed-object verification remains part of
registry admission and consumption.

Coven builds the checked intake and reply workers before opening the listener.
Each accepted connection becomes the worker's read-only standard input.
The worker checks a head of at most 16 KiB, the exact loopback Host,
the route and its Origin rules, and a single bounded Content-Length
for JSON publications. It reads exactly that many bytes under the
intake deadline and commits them to a private file. Coven dispatches
only a completed request to the existing SQLite transaction and
signature checks. The worker can write that one staging file and has
no dial, listen, or database grant. Package publication stages only the
target package, the signed snapshot, and dependency digest markers.
The commit is limited to the requested version's standard files and
its package index. Operator snapshots still stage the entire registry.

Coven stages the resulting reply as one private read-only file. The reply
worker receives that file, bounded response metadata, and the accepted
socket as writable standard output. It uses the checked HTTP writer to
produce the HTTP/1.0 response and fixed-length body. The reply has a single
deadline that includes staging, worker execution, and a blocked socket
write. Both workers end with the listener process.

For separate operators and publishers, run them under separate OS accounts and transfer the registry URL, registry ID and operator **public key ID** out of band.

```sh
work=$(cd "$(mktemp -d)" && pwd -P)
cd "$work"
mkdir -m 700 service operator author consumer
role() (
  cd "$work/$1" || exit 1
  shift
  export HOME="$(pwd -P)" XDG_STATE_HOME="$(pwd -P)/state" XDG_CACHE_HOME="$(pwd -P)/cache"
  export WITCHY_PROOF_CACHE="$(pwd -P)/proofs"
  mkdir -p -m 700 "$WITCHY_PROOF_CACHE"
  exec "$@"
)
url=http://127.0.0.1:8787
(
  cd service
  export HOME="$(pwd -P)" XDG_STATE_HOME="$(pwd -P)/state" XDG_CACHE_HOME="$(pwd -P)/cache"
  export WITCHY_PROOF_CACHE="$(pwd -P)/proofs"
  mkdir -m 700 "$WITCHY_PROOF_CACHE"
  exec coven serve --store "$work/service/store" --port 8787
) >"$work/serve.log" 2>&1 &
server=$!
# Continue when serve.log says 'Coven listening at http://127.0.0.1:8787'.
while ! grep -q 'Coven listening at' "$work/serve.log"; do
  kill -0 "$server" 2>/dev/null || { cat "$work/serve.log"; exit 1; }
  sleep .1
done
role operator python3 -c 'import os; fd=os.open("operator",os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600); os.write(fd,os.urandom(32)); os.close(fd)'
operator=$(role operator coven key --key operator | cut -d' ' -f2)
role operator coven snapshot --registry "$work/service/store" --key operator --first --id demo
```

First-snapshot bootstrap is a local administrative action. An actual service operator needs access to the private store for that action. Ordinary snapshots after bootstrap go over HTTP. Only the operator's signing process reads its private seed. The service can restart with the same store and URL.

## Publish as the author

```sh
cp -R "$src/grimoire/examples/local-run/config-reader" author/library
owner=$(role author grimoire keygen publisher | cut -d' ' -f2)
cat >>author/library/witchy.toml <<EOF

[registry]
url = "$url"
operator = "$operator"
id = "demo"
EOF
role author grimoire build library
role author grimoire publish library --key publisher
role operator coven snapshot --registry "$url" --key operator --id demo --min-base 1 || true  # review only: nothing signed
# Read the review: `review: first claim: config-reader by witchy-2:...` names the new package's
# owner and recovery keys and versions. Nothing is signed yet. Check the owner against $owner
# (out of band, in practice), then admit that claim by name:
role operator coven snapshot --registry "$url" --key operator --id demo --min-base 1 --admit config-reader
```

The library build compiles an import of the exact package source and its locked dependencies. Its `target/config-reader-0.1.0.coven` contains the canonical release record followed by that source. Publication signs the release and package index locally. Anyone who can reach the port can publish, so the operator's snapshot of a served registry lists every change since its last snapshot and signs nothing until each package's first claim is named with `--admit NAME`. Grimoire never admits a claim for the operator. Later versions, withdrawals and rotations of an admitted package need no `--admit`; the owner's signature chain decides them. Snapshot 2 admits the index. An earlier snapshot continues to name its earlier package set.

## Lock, build, run and test as a new consumer

```sh
cp -R "$src/grimoire/examples/local-run/app" consumer/app
rm consumer/app/main.packages  # selected example copy had a sibling-checkout dependency
cat >>consumer/app/witchy.toml <<EOF

[registry]
url = "$url"
operator = "$operator"
id = "demo"

[dependencies]
config-reader = "0.1.0"

[owners]
config-reader = "$owner"

[[test]]
name = "reads-port"
project = "."
stdout = "8080\n"
EOF
role consumer grimoire lock app
role consumer grimoire build app
role consumer grimoire run app --record build.json
role consumer grimoire test app
kill "$server"
wait "$server"
```

The app prints `8080` and the test prints `ok reads-port`. The executable build writes `target/main.wasm`. The lock holds the version, package digest, publisher and snapshot pins. The build record includes the module digest, the actual host flags, and the digest of the grant document bytes read before the build. A consumer with only the operator anchor may explicitly elect first-use trust for a package owner with `grimoire lock --trust-first-use`. Checking the owner ID out of band and declaring it avoids that election. Returning clients retain the normal retirement and rollback constraints.

Grimoire checks the toolchain pins, resolves dependency pins, and records
the run against one captured lock revision. An absent lock stays absent
for the command. If another command replaces the lock during resolution,
`grimoire lock` refuses to overwrite the newer bytes. Run it again against
the updated lock.

Both compiler paths check the grant document against the bytes and
resolved parent directory Grimoire captured. The standalone command takes
a SHA-384 document pin and a directory with `--document-base=DIR`.
This binds the starting directory of relative grants even when the
document is named through a symbolic link. A Wasm grant review receipt
records the exact document bytes used for that review.

When Grimoire invokes a standalone compiler, its grant check returns an
input key covering the source graph, including imported files and
package edges, and the adjacent policy. The subsequent build checks that
key and refuses changes before it can replace the module. Compiler
wrappers forward `--print-input-key` on the grant check and
`--reviewed-input-key=KEY` on the build.

Each `[[test]]` names an executable project within this project and its exact expected UTF-8 stdout. Grimoire runs the case through the ordinary project launcher, with its own manifest, lock, policy, grants and limits. Nonzero exit or different output fails the test. A project without test declarations reports that fact and returns failure.

Coven transports public signed objects and package bytes over HTTP. Client verification and registry admission use the existing canonical formats, ML-DSA-65 signatures and SHA-384 identities. The Witchy guests perform signing, verification, package publication and checked extraction. Python implements project orchestration, loopback HTTP, SQLite storage and the existing snapshot/rotation policy. Fetching a package gives its Witchy code no socket capability. For a separate application that serves HTTP with its executable's own listener grant, see the [HTTP file server](../examples/http-file-server/README.md).
