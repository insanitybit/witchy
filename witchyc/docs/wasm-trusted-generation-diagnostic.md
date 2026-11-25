# Explicit trusted-generation diagnostic loop

`wasm-session-trusted-generation-diagnostic FILE SDK_SHA256` is a nonpublishing
performance experiment. It uses the existing native session transport and actual
parser/typechecker/source VC/emitter pipeline. Every selected proof, including
unused lemmas, gets a fresh command environment, kernel check and complete existing
auditor. Only imports are retained; no program theorem or admission is reused.
No Wasm or portable proof artifact is written. Output contains the Wasm SHA and a
separately named `trusted_generation_session_diagnostic` identity.

The operator selects a private generation through the existing absolute
`WITCHY_WASM_LEAN_ROOT` and `WITCHY_WASM_NATIVE_HELPER` configuration and supplies
its intended lowercase64hex full SHA-v1 SDK identity as a command argument.
The helper/compiler/import/audit identities are bound to the session. Bad identity
syntax refuses before stdin/source access; the actual SDK identity is compared
before LOAD. Full SDK snapshots A before LOAD and B afterward must match that
expected identity. The first proof request records two snapshots; following proof
requests record zero. Proof-free input remains lazy, records zero snapshots and
does not select/load a toolchain. Source edits use the existing bounded stdin
`check REQUEST_ID EXPECTED_SOURCE_SHA256` protocol and fresh source rereads.

This command explicitly assumes
`selected_readonly_generation_no_concurrent_mutation`. The trusted parent must
prepare a fresh owned private tree with distinct-inode copies/clones, no hardlinks
to original files, in-root link closure and ordinary write bits removed. Preserve
the full existing content/tree identity, close writer FDs and retain the complete
copy for lazy reads. The owner promises not to chmod, replace, rebuild or write
selected files until the session ends. The compiler does not verify this lifecycle
or defend against a malicious owner/debugger/root. Readonly bits alone do not
prove immutability. The report says `verified_immutable:false` and
`sdk_path_currentness:false`; it never creates a sealed/Verified capability.

The selected version lasts until exit. Original SDK pathname updates are not
selected in place. Root/helper/context changes refuse and require a fresh session;
no automatic refresh or fallback can hide a failure. Helper/compiler/source
content checks, protocol binding, bounds, late-output/status checks, owned process
cleanup and the eight-request cap remain. New mode stops repeated installation
walks after startup under the declared owner assumption. Helper protocol remains
physically `UNSEALED_DIAGNOSTIC`; the outer mode distinguishes
`TRUSTED_GENERATION_DIAGNOSTIC` and explicitly reports the selected policy.

Existing `wasm-session-diagnostic` retains full SDK A/B for every proof request;
Builds without a ready binding and inspection retain strict checks and `fresh.v1`.
The separate `WITCHY_WASM_READY_BINDING` operator configuration enables the
[production selected-generation path](wasm-production-build.md). Source or artifact
data cannot choose it. The new constructor is reachable only
through the explicit diagnostic API/command. All source/proof output and private
raw evidence behavior matches the existing diagnostic route.

This source change is uncompiled/unexecuted at worker freeze. Root qualification
must include real repeated positive/same-name edits and false-unused refusal,
semantic wrong expected SDK refusal before LOAD, selected-copy ordinary-write
refusal, original-copy mutation independence, helper/root change refusal and
late-error/timeout/reap controls. Compare three small matched whole-loop trials
only after those checks, separating setup A/B from recurring edit latency. No
speedup or production currentness-policy approval is claimed by source alone.
