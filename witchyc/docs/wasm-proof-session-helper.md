# Reproducible diagnostic proof helper

The packaged helper retains only the fixed imported SDK context. Every supplied
program declaration, inline tactic, theorem, unused lemma, expected type and axiom
audit is checked afresh. It is an `UNSEALED_DIAGNOSTIC` backend, not portable
admission. Without an explicit ready binding, publication uses strict one-shot full SDK A/B checks.
The body-interface builder also emits the trusted operator record for
[production builds](wasm-production-build.md).

`tools/proof-session/Resident.lean` is the working plain native helper source;
the builder copies it unchanged and performs no bootstrap-prefix generation.
The helper validates/removes only the exact fixed import header, then processes
the entire remaining module in a fresh command state. No selected program proof
or fixed tactic is cached. The earlier fixed-prelude bootstrap experiment is
parked: matched results did not establish a material practical improvement.

`src/compiler/proof_prelude.lean.in` remains the Rust generator's authoritative prefix,
included byte-for-byte. It does not become a prechecked helper environment and
does not change generated proof-module bytes or any model/audit identity.

First build the compiler through the project's normal MBX workflow. Its new
read-only identity command grants no proof, selection or immutability authority:

```
/absolute/compiler wasm-toolchain-id /absolute/selected-sdk
```

The JSON algorithm/domain are `sha256` and
`witchy.scalar.lean-installation.v1`; `hex` uses the existing complete tree walker.
Relative roots refuse. The command performs no SDK writes or configuration change.

Root/operator must admit host resources and supply an existing selected SDK.
For low-latency generation mode, prepare a fresh distinct-inode private readonly
copy/clone externally, close own writer FDs, preserve in-root links/content scope
and keep it unchanged throughout build/session. The builder does not prepare,
chmod, install, select or update SDKs. Protected SDK administrator action remains
pending; this command neither requests nor authorizes it.

```
python3 scripts/build-proof-session-helper.py \
  --compiler /absolute/compiler \
  --sdk /absolute/private-selected-sdk \
  --out /absolute/fresh-helper-output \
  --mbx /absolute/mbx --rustc /absolute/rustc \
  --sysroot /absolute/existing-macos-system-headers
```

Output must be new and outside the SDK/source tree; all outputs are preserved.
No package installation, network, global SDK selection or privilege is used.
Without --sysroot, the only fallback is the existing readonly xcrun getter.
The script records the current host deployment target, uses installed SDK Lean
trust0/threads1/memory2048, compiles the Rust ABI adapter through MBX, and uses leanc shared linking with private SDK
RPATH and `/usr/bin/clang`. It snapshots SDK A/B with the supplied compiler CLI,
checks compiler/source pins afterward, and pins the actual generated helper.
The adapter requires the pinned Lean 4.35 macOS ARM64 runtime headers. The builder
refuses a different object ABI. `--mbx` and `--rustc` may be omitted when both
executables are on PATH.
Manifest `built-unqualified` means build completion only; Root must still qualify
native positives/false-unused/same-name/default-module parity before use.

`scripts/test-proof-helper-abi.py` checks accepted and rejected proofs through
the plain helper and through each supplied body-interface binding. It also checks
that rejection preserves the prior output. Pass the compiler, plain helper, SDK,
and one or more `--binding` paths. The plain helper can be built with Rust ASan
and Lean's generated C with Clang ASan and UBSan for the same focused check.
That UBSan instrumentation covers generated C, while the Rust adapter has ASan
instrumentation. Lean's prebuilt libraries are outside this instrumentation.
For example, with `LEAN_SDK`, `HELPER_BUILD`, `COMPILER`, and `BODY_BINDING`
set to absolute paths, and `mbx` on PATH:

```sh
mbx exec "$(rustup which --toolchain nightly rustc)" \
  -Zsanitizer=address --edition=2024 --crate-type=lib --emit=obj \
  -Copt-level=1 -Cpanic=abort tools/proof-session/native.rs -o /tmp/witchy-helper-asan.o
LEAN_CC="$(brew --prefix llvm)/bin/clang" "$LEAN_SDK/bin/leanc" \
  -O1 -g -leanshared -fsanitize=address,undefined -fno-sanitize-recover=all \
  "-Wl,-rpath,$LEAN_SDK/lib/lean" -L "$LEAN_SDK/lib" \
  -o /tmp/witchy-helper-sanitized "$HELPER_BUILD/Resident.c" /tmp/witchy-helper-asan.o
ASAN_OPTIONS=detect_leaks=0 python3 scripts/test-proof-helper-abi.py \
  --compiler "$COMPILER" --plain-helper /tmp/witchy-helper-sanitized \
  --sdk "$LEAN_SDK" --binding "$BODY_BINDING"
```

The SDK and plain helper build must come from the pinned Lean version. Leak
detection is disabled for Lean's retained runtime objects in this check.

Every subprocess has an owned session/PGID, deadline, monitored64KiB logs,
bounded reads and retained raw files. Launch needs16GiB, live disk floor4GiB.
Cancellation kills/waits only its unreaped owned child group; unknown descendants
keep `group_released:false` in the manifest and require host reconciliation.
A hard kill cannot run cleanup: Root must inspect recorded child claims before
releasing reservations. Do not duplicate observer cleanup or use deleted targets.

After qualification, set existing operator configuration and keep this process's
stdin open across edits (source hash must match the exact current file each time):

```
export WITCHY_WASM_LEAN_ROOT=/absolute/private-selected-sdk
export WITCHY_WASM_NATIVE_HELPER=/absolute/fresh-helper-output/proof-session-helper
/absolute/compiler wasm-session-trusted-generation-diagnostic /absolute/program.wy EXPECTED_SDK_SHA256
check edit1 EXPECTED_SOURCE_SHA256
```

Edit in another terminal, compute the new source SHA and send another check line.
EOF closes/reaps; restart after at most8 proof requests. Do not raise request,
10s,2GiB or64KiB bounds. Records contain Wasm SHA only; no Wasm/artifact publication.
`wasm-session-diagnostic FILE` instead retains full SDK scans per proof request.
The exact helper's historical A/B comment describes that route; Rust's explicit
selected-generation policy controls the opt-in route, not the helper itself.

Selected-generation mode assumes
`selected_readonly_generation_no_concurrent_mutation`, reports
`verified_immutable:false`/`sdk_path_currentness:false`, and selects one version
until exit. Original SDK path updates select the next session, not an in-place
refresh. Helper/compiler/source identity checks and full fresh proof/audit remain.
No kernel seal, adversarial-owner protection or production policy approval is
claimed. A mutable mapped backing still matters if the owner breaks that promise.
