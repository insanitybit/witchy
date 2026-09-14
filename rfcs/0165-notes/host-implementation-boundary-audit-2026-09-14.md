# Canonical host implementation boundary audit, 2026-09-14

Baseline: `aca70ad99` (`master` when the audit worktree was created).

## Result

No caller-controlled host-implementation substitution exists in the current
artifact-typed native launch path. `Runtime::spawn_admitted` accepts an
`AdmittedCanonicalArtifact`, constructs its own private Wasmtime `Linker`, and
registers only crate-private handlers through `link_capability_imports` and the
admitted closure-owner registry. Neither a linker nor raw host functions are an
argument. Worker launch repeats the same registration path. Fixture handlers
are reachable only through the distinct fixture/raw-module APIs or through the
explicit admitted Console fixture contract.

The canonical checker authenticates an import against the compiler-owned
`CanonicalAbiCatalog`. That identity covers the ABI version, module and name,
Wasm shape, authority classification, and declared authority families. The
Wasm backend emits that exact module/name/signature. Wasmtime independently
checks the emitted signature against the fixed native registration at
instantiation.

This establishes linkage identity, not handler correctness. The Rust host
modules below remain in the runtime TCB and must obey the buffer, capability,
reentrancy, and result contracts they implement. Hashing those modules with the
same build that executes them would be self-attestation and would not establish
that they obey the contracts.

The browser hosts are a separate deployment TCB. They do not consume
`AdmittedCanonicalArtifact` or claim Rust typestate authority. Both
`web/witchy-host.js` and `web/witchy-runtime/witchy-runtime.mjs` construct their
own import objects. Provider options supply typed capability behavior through
those fixed wrappers; they do not supply raw Wasm import functions. The browser
ABI test checks the independently maintained ABI version and exact
deny-by-omission import surface. Most canonical slice imports are deliberately
absent, so current canonical artifacts using them fail closed in that host.

## Complete canonical routing

The table groups every import that `CanonicalAbiCatalog` can currently project
into a candidate. A candidate declares only the subset its checked functions
reach.

| Canonical imports | Native implementation |
|---|---|
| `console_print`, `console_print_at`, `__witchy_abort_at`, `__witchy_list_bounds_at` | `runtime/host/console.rs`; `console_print_at` uses the session-authenticated canonical fixture handler when fixture support is enabled and a rejecting handler otherwise |
| `float_to_str` | `runtime/host/staging.rs` |
| `dir_read_into`, `dir_list_into`, `dir_exists_at`, `dir_is_dir_at`, `dir_write_at`, `dir_subtree_at`, `dir_read_file`, `file_read_into` | `runtime/host/filesystem.rs` |
| `env_has_var`, `env_get_into`, `clock_now` | `runtime/host/clock_env_rand.rs` |
| `dir_create_new_at`, `dir_replace_at`, `dir_read_bytes_at`, `dir_bytes_response_size`, `dir_bytes_response_into`, `dir_write_bytes_at`, `dir_append_bytes_at`, `dir_set_executable_at`, `dir_rename_at`, `dir_make_dir_at` | `runtime/host/filesystem.rs` |
| `build_out_write_at` | `runtime/host/build_out.rs` |
| `build_read_at`, `build_read_response_size`, `build_read_response_into`, `build_response_is_some` | `runtime/host/build_read.rs` |
| `build_env_at` | `runtime/host/build_env.rs` plus the shared build-response handlers |
| `build_exec_run` | `runtime/host/build_exec.rs` plus the shared build-response handlers |
| `build_net_fetch` | `runtime/host/build_net.rs` plus the shared build-response handlers |
| `exec_run_at`, `exec_response_size`, `exec_response_into` | `runtime/host/exec.rs` |
| `net_fetch_at`, `fetch_send_at`, `fetch_response_size`, `fetch_response_into`, `fetch_only_at` | `runtime/host/fetch.rs` |
| `net_listen_at`, `net_accept`, `net_send_slice`, `net_recv_line_size`, `net_recv_bytes_size`, `net_take_received_into`, `net_close`, `serve_pool`, `net_recv_all_size`, `net_connect_at` | `runtime/host/canonical_network.rs`, `runtime/host/network.rs`, and `runtime/host/vm_worker.rs` |
| `secretstore_lookup_at` | `runtime/host/secret.rs` |
| `crypto_sign_into`, `crypto_public_key_into`, `crypto_ed25519_verify_slices`, `crypto_rsa_pkcs1_sha256_verify_slices`, `crypto_sha256_slices`, `crypto_rune_hash_slices` | `runtime/host/crypto.rs` |
| `compiler_footprint_into`, `compiler_diff_into`, `compiler_doc_into`, `compiler_doc_result_json_into` | bounded adapters in `runtime/compiler.rs`; the injected service receives owned strings and returns owned bytes, never guest memory |
| `regex_match_spans_into` | `runtime/host/regex.rs` |
| `vm_service_start`, `vm_service_step`, `vm_service_response_size`, `vm_service_response_into`, `vm_service_close` | `runtime/host/vm_service.rs`, registered by the admitted VM-service path |

Capability-bearing handlers authenticate the dynamic externref kind, issuer,
scope, and rights before using guest slices. Internal-service response handles
are host-minted and type-checked before access. Buffer handlers obtain memory
from the active `Caller` and perform their own checked range conversion; the
SafeWIR call-site check is not their only boundary.

## Existing mechanical evidence

- `native_host_links_every_catalog_import_with_the_declared_signature` builds a
  module importing the complete compiler prelude catalog and instantiates it
  against a maximal native host. A missing or differently typed registration
  fails before execution.
- `browser_host_implements_exactly_the_catalogued_browser_surface` compares the
  compiler-owned browser classification and ABI version with the browser
  runtime's independent exported list, then instantiates through the actual
  import object. Missing, extra, or differently typed entries fail.
- Canonical positive launch tests compile checked source to an
  `AdmittedCanonicalArtifact` and enter it only with `spawn_admitted`.
- Capability and fixture tests pass null, wrong-kind, wrong-session, and
  insufficient-rights externrefs to the actual handlers and require refusal
  before guest-memory access.

These tests establish routing and fail-closed mismatch behavior. They do not
replace semantic review of each handler implementation.

## Future interface boundary

If Witchy adds a pluggable native linker, dynamically loaded host functions, or
an artifact-typed browser launcher, that new sink must accept a trusted
`HostRuntimeManifest` rather than a bare import object. The narrow interface is:

```text
HostRuntimeManifest {
    abi_version,
    implementations: (module, name) -> independently reviewed contract version,
}
```

Before instantiation, the sink must require an exact manifest entry for every
import in the admitted artifact and reject a missing or mismatched entry. The
manifest belongs to launch context, not `CandidateId`, so a runtime patch does
not change compiler artifact identity. Its declarations must be maintained by
the runtime implementation, independently of the compiler catalog; deriving
both sides from `abi_import_info` would be self-attestation. An adversarial test
must replace one implementation contract version while preserving the Wasm
signature and require launch refusal, and a positive test must execute admitted
source through the matching manifest.

That interface is not added now because the current admitted native sink has no
substitution input and the current browser sink has no admitted authority. The
remaining security obligation is an independent semantic audit of the fixed
runtime handlers, not an identity-binding defect.
