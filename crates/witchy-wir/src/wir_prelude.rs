//! Pre-compiled static runtime prelude for the WIR (witchy IR) WASM backend.
//!
//! The legacy backend (`src/codegen.rs::compile_module_with`) builds every
//! module by concatenating ~75 hand-written WAT helper functions (`$mkN`,
//! `$concat`, `$str_eq`, the `$list_*` / `$dict_*` / `$crypto_*` families, the
//! host-wrapper helpers, …) into the module text, then handing the whole text
//! to wasmtime. The WIR migration wants those helpers as *binary* — assembled
//! once, then spliced verbatim into each user module's code section.
//!
//! This module does exactly that: it assembles a single self-contained wasm
//! module (imports + memory + globals + closure types + table + every helper),
//! compiles it to wasm bytes ONCE (lazily, behind a `OnceLock`, so the `wat`
//! crate never runs on the per-program hot path), then reads the binary back
//! with `wasmparser` to expose, per helper: its name, its `(params, results)`
//! signature, and its raw code-section body bytes (locals + instructions, with
//! NO leading size LEB). Imports, globals, and the table are exposed too.
//!
//! # Index layout (what a downstream `lower_module` MUST match)
//!
//! `Prelude` assumes the encoder lays the module out as:
//!
//! 1. **Type section**: closure call types `$clos0 … $clos4` occupy type
//!    indices `0 … 4` (params `(i32 env, i64×N)`, result `i64`). Helper and
//!    user function types follow.
//! 2. **Function index space** (imports first, per the wasm spec):
//!    * indices `0 .. imports.len()` — the imported host functions, in the
//!      exact order of [`Prelude::imports`] (which mirrors `codegen.rs`'s
//!      `emit_imports`, every feature on: `print`, then the `*_host` family).
//!    * the next `funcs.len()` indices — the prelude helpers, in the exact
//!      order of [`Prelude::funcs`]: first `$mk0 … $mk8`, then the static
//!      helpers in [`HELPER_NAMES`] order (the canonical
//!      `emit_data_globals_helpers` "all features on" order).
//!    * user functions follow, at `imports.len() + funcs.len() ..`.
//! 3. **Globals**: `$heap` (mut i32) then `$__witchy_reowns` (mut i64),
//!    matching [`Prelude::globals`]. Region copy-out scratch globals follow.
//! 4. **Table 0** (`funcref`): present for `call_indirect (type $clos1)` inside
//!    `$dict_update` / `$dict_update_cap`. The encoder owns its final size and
//!    elem segments; [`Prelude::table_size`] is the prelude's own minimum.
//!
//! Because a helper body's `call $other` / `call_indirect (type $closN)`
//! operands are encoded as *indices*, the raw bodies splice unchanged ONLY when
//! the encoder reproduces this exact ordering. The whole point of pinning the
//! order here is to make that contract explicit.
//!
//! NOTE: the binary path reads ONLY the helper NAMES ([`HELPER_NAMES`]) and the
//! import signatures (parsed from [`PRELUDE_IMPORTS_WAT`]); the helper BODIES are
//! emitted by the `wir_helper` registry, so the hand-written `*_WAT` body consts
//! that used to live here are gone. This file is hand-maintained — there is no
//! generator (the old `scripts/gen_wir_prelude.py` emitted the retired body
//! consts and was removed).

// Some items below (e.g. `MAX_MK`) are only referenced indirectly; keep the
// crate-wide dead-code lint off this module so the prelude's pinned-order
// declarations stay even when a particular one has no direct reader.

use std::sync::OnceLock;

/// Function names for the static prelude helpers, in canonical
/// `emit_data_globals_helpers` "all features on" order. The binary path uses
/// these names as its resolution filter; the bodies come from the `wir_helper`
/// registry.
const HELPER_NAMES: &[&str] = &[
    "ensure",
    "concat",
    "list_at",
    "list_push",
    "list_push_cap",
    "str_append_cap",
    "dict_insert_cap",
    "dict_update_cap",
    "list_concat",
    "list_drop",
    "starts_with",
    "ends_with",
    "substr",
    "ascii_case",
    "crypto_sha256",
    "crypto_rune_hash",
    "field_str_get",
    "field_intlist_get",
    "field_strlist_get",
    "compiler_footprint",
    "compiler_diff",
    "compiler_doc",
    "compiler_doc_result_json",
    "build_user_cap_field",
    "float_to_str",
    "encoding",
    "get_env",
    "dir_read",
    "file_read",
    "build_read",
    "build_out_write",
    "dir_list",
    "net_resolve",
    "net_recv_line",
    "net_recv_all",
    "net_recv_bytes",
    "build_args",
    "crypto_sign",
    "crypto_public_key",
    "crypto_reveal",
    "secretstore_lookup",
    "f_nan_guard",
    "f_lt",
    "f_le",
    "f_gt",
    "f_ge",
    "float_to_int",
    "split",
    "str_chars",
    "find_byte",
    "byte_to_char",
    "str_index_of",
    "char_to_byte",
    "str_substring",
    "match_at",
    "replace",
    "str_to_int",
    "is_ws",
    "trim",
    "dict_new",
    "dict_hash",
    "dict_find",
    "dict_index_put",
    "dict_index_build",
    "key_eq",
    "dict_insert",
    "dict_get_or",
    "dict_has",
    "dict_remove",
    "dict_update",
    "dict_keys",
    "dict_values",
    "dict_pairs",
    "print_str",
    "int_to_string",
    "str_eq",
    "str_cmp",
];

/// The closure call-signature arities pre-declared in the prelude type section
/// (`$clos0 … $clos{MAX_CLOS}`). A future user module may need higher arities;
/// it appends them after these fixed indices.
pub const MAX_CLOS: usize = 4;

/// The largest record/constructor arity for which a `$mkN` allocator is
/// pre-baked. Arities above this are emitted per-module by the encoder.
pub const MAX_MK: usize = 8;

/// A single value type in a prelude signature, mirrored from wasm core valtypes.
/// Beyond the four numerics, `ExternRef` names the wasm `externref` — the
/// unforgeable representation a migrated capability import takes (RFC-0005:
/// `mint_dir`/`dir_*`, `mint_file`/`file_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmTy {
    I32,
    I64,
    F32,
    F64,
    ExternRef,
}

/// An imported host function the prelude depends on.
#[derive(Debug, Clone)]
pub struct PreludeImport {
    /// The two-level wasm import name, e.g. `("witchy", "print")`.
    pub module: String,
    pub name: String,
    pub params: Vec<WasmTy>,
    pub results: Vec<WasmTy>,
}

/// A pre-compiled prelude helper function: its `$name`, its signature, and the
/// raw code-section body (locals declarations + instruction bytes, WITHOUT the
/// leading body-size LEB — the encoder writes that itself).
#[derive(Debug, Clone)]
pub struct PreludeFunc {
    pub name: String,
    pub params: Vec<WasmTy>,
    pub results: Vec<WasmTy>,
    pub raw_body: Vec<u8>,
}

/// A prelude global: its name, value type, mutability, and a single literal
/// `i32`/`i64` init constant (the only init forms the prelude uses).
#[derive(Debug, Clone)]
pub struct PreludeGlobal {
    pub name: String,
    pub ty: WasmTy,
    pub mutable: bool,
    pub init_i64: i64,
}

/// The fully extracted, splice-ready static runtime prelude. See the module
/// doc for the index layout the fields assume.
#[derive(Debug, Clone)]
pub struct Prelude {
    pub imports: Vec<PreludeImport>,
    pub globals: Vec<PreludeGlobal>,
    pub funcs: Vec<PreludeFunc>,
    /// Minimum size of table 0 the prelude itself requires (it constructs no
    /// lambdas, so this is 0; the table exists only so `call_indirect` validates).
    pub table_size: u32,
    /// Element-segment entries the prelude installs (none — the prelude builds
    /// no lambdas). Present so the downstream encoder's shape matches.
    pub elems: Vec<String>,
}

/// The `(import "witchy" ...)` lines the prelude helpers reference, in the
/// canonical `emit_imports` order (every feature on). `$print` first, then the
/// `*_host` family. The encoder MUST place these at function indices `0..` in
/// exactly this order for the spliced bodies' `call` operands to resolve.
const PRELUDE_IMPORTS_WAT: &str = r#"  (import "witchy" "print" (func $print (param i32 i32)))
  (import "witchy" "crypto.sha256" (func $crypto_sha256_host (param i32 i32)))
  (import "witchy" "crypto.rune_hash" (func $crypto_rune_hash_host (param i32 i32 i32)))
  (import "witchy" "compiler_footprint_len" (func $compiler_footprint_len_host (param i32) (result i32)))
  (import "witchy" "compiler_diff_len" (func $compiler_diff_len_host (param i32 i32) (result i32)))
  (import "witchy" "compiler_doc_len" (func $compiler_doc_len_host (param i32 i32) (result i32)))
  (import "witchy" "compiler_doc_result_json_len" (func $compiler_doc_result_json_len_host (param i32 i32) (result i32)))
  (import "witchy" "user_cap_field_len" (func $user_cap_field_len_host (param i32 i32) (result i32)))
  (import "witchy" "field_str_len" (func $field_str_len_host (param i32) (result i32)))
  (import "witchy" "field_intlist_len" (func $field_intlist_len_host (param i32) (result i32)))
  (import "witchy" "field_strlist_size" (func $field_strlist_size_host (param i32) (result i32)))
  (import "witchy" "float_to_str" (func $float_to_str_host (param f64 i32) (result i32)))
  (import "witchy" "encoding" (func $encoding_host (param i32 i32 i32) (result i32)))
  (import "witchy" "crypto.sign" (func $crypto_sign_host (param externref i32 i32)))
  (import "witchy" "crypto.public_key" (func $crypto_public_key_host (param externref i32)))
  (import "witchy" "secretstore_lookup" (func $secretstore_lookup_host (param i32) (result externref)))
  (import "witchy" "crypto_reveal_len" (func $crypto_reveal_len_host (param externref) (result i32)))
  (import "witchy" "mint_secret" (func $mint_secret_host (param i32) (result externref)))
  (import "witchy" "env_len" (func $env_len_host (param i32) (result i32)))
  (import "witchy" "env_fill" (func $env_fill_host (param i32 i32)))
  (import "witchy" "dir_read_len" (func $dir_read_len_host (param externref i32) (result i32)))
  (import "witchy" "dir_list_size" (func $dir_list_size_host (param externref) (result i32)))
  (import "witchy" "args_size" (func $args_size_host (result i32)))
  (import "witchy" "testing_mock_dir" (func $testing_mock_dir_host (param i32) (result externref)))
  (import "witchy" "write_pending_list" (func $write_pending_list_host (param i32)))
  (import "witchy" "vm_par_map_run" (func $vm_par_map_run_host (param i32 i32) (result i32)))
  (import "witchy" "vm_par_map_write" (func $vm_par_map_write_host (param i32)))
  (import "witchy" "vm_par_map_bytes_run" (func $vm_par_map_bytes_run_host (param i32 i32) (result i32)))
  (import "witchy" "vm_par_map_bytes_write" (func $vm_par_map_bytes_write_host (param i32)))
  (import "witchy" "vm_with_dir_run" (func $vm_with_dir_run_host (param externref i32 i32) (result i32)))
  (import "witchy" "vm_serve_run" (func $vm_serve_run_host (param i32 i32 i32) (result i32)))
  (import "witchy" "build_read_len" (func $build_read_len_host (param i32) (result i32)))
  (import "witchy" "build_out_write" (func $build_out_write_host (param i32 i32)))
  (import "witchy" "build_env_len" (func $build_env_len_host (param i32) (result i32)))
  (import "witchy" "build_env_fill" (func $build_env_fill_host (param i32 i32)))
  (import "witchy" "build_fetch_len" (func $build_fetch_len_host (param i32 i32) (result i32)))
  (import "witchy" "build_exec_run" (func $build_exec_run_host (param i32 i32) (result i32)))
  (import "witchy" "net_recv_line_len" (func $net_recv_line_len_host (param externref) (result i32)))
  (import "witchy" "net_recv_all_len" (func $net_recv_all_len_host (param externref) (result i32)))
  (import "witchy" "net_recv_bytes_len" (func $net_recv_bytes_len_host (param externref i64) (result i32)))
  (import "witchy" "fill_pending" (func $fill_pending_host (param i32)))
  (import "witchy" "crypto.sha512" (func $crypto_sha512_host (param i32 i32)))
  (import "witchy" "crypto.sha3_256" (func $crypto_sha3_256_host (param i32 i32)))
  (import "witchy" "crypto.hmac_sha256" (func $crypto_hmac_sha256_host (param i32 i32 i32)))
  (import "witchy" "print_int" (func $print_int (param i64)))
  (import "witchy" "print_float" (func $print_float (param f64)))
  (import "witchy" "string_from_code" (func $string_from_code_host (param i64 i32) (result i32)))
  (import "witchy" "mint_dir" (func $mint_dir_host (param i32) (result externref)))
  (import "witchy" "dir_subdir" (func $dir_subdir_host (param externref i32) (result externref)))
  (import "witchy" "dir_only" (func $dir_only_host (param externref i32) (result externref)))
  (import "witchy" "dir_exists" (func $dir_exists_host (param externref i32) (result i32)))
  (import "witchy" "dir_is_dir" (func $dir_is_dir_host (param externref i32) (result i32)))
  (import "witchy" "dir_write" (func $dir_write_host (param externref i32 i32)))
  (import "witchy" "dir_append" (func $dir_append_host (param externref i32 i32)))
  (import "witchy" "dir_make_dir" (func $dir_make_dir_host (param externref i32)))
  (import "witchy" "dir_open" (func $dir_open_host (param externref i32) (result externref)))
  (import "witchy" "dir_create" (func $dir_create_host (param externref i32) (result externref)))
  (import "witchy" "mint_file" (func $mint_file_host (param i32) (result externref)))
  (import "witchy" "file_read_len" (func $file_read_len_host (param externref) (result i32)))
  (import "witchy" "file_write" (func $file_write_host (param externref i32)))
  (import "witchy" "mint_net" (func $mint_net_host (param i32) (result externref)))
  (import "witchy" "mint_fetch" (func $mint_fetch_host (param i32) (result externref)))
  (import "witchy" "fetch_only" (func $fetch_only_host (param externref i32) (result externref)))
  (import "witchy" "fetch_send_len" (func $fetch_send_host (param externref i32 i32 i32 i32) (result i32)))
  (import "witchy" "net_fetch" (func $net_fetch_host (param externref i32) (result externref)))
  (import "witchy" "net_connect" (func $net_connect_host (param externref i32) (result externref)))
  (import "witchy" "net_try_connect" (func $net_try_connect_host (param externref i32) (result externref)))
  (import "witchy" "net_resolve_size" (func $net_resolve_size_host (param externref i32) (result i32)))
  (import "witchy" "net_connect_pinned" (func $net_connect_pinned_host (param externref i32 i32 i64 i32) (result externref)))
  (import "witchy" "net_try_connect_pinned" (func $net_try_connect_pinned_host (param externref i32 i32 i64 i32) (result externref)))
  (import "witchy" "net_listen" (func $net_listen_host (param externref i32) (result externref)))
  (import "witchy" "net_listen_tls" (func $net_listen_tls_host (param externref i32 i32 externref) (result externref)))
  (import "witchy" "net_accept" (func $net_accept_host (param externref) (result externref)))
  (import "witchy" "serve_pool" (func $serve_pool_host (param externref)))
  (import "witchy" "net_restrict" (func $net_restrict_host (param externref i32) (result externref)))
  (import "witchy" "net_deny" (func $net_deny_host (param externref i32) (result externref)))
  (import "witchy" "net_send_line" (func $net_send_line_host (param externref i32)))
  (import "witchy" "net_send_bytes" (func $net_send_bytes_host (param externref i32)))
  (import "witchy" "net_close" (func $net_close_host (param externref)))
  (import "witchy" "now" (func $now_host (result i64)))
  (import "witchy" "now_monotonic" (func $now_monotonic_host (result i64)))
  (import "witchy" "rand_u64" (func $rand_u64_host (result i64)))
  (import "witchy" "regex_match_spans_len" (func $regex_match_spans_len_host (param i32 i32) (result i32)))
  (import "witchy" "crypto.__ecdsa_p256_verify_status" (func $crypto_ecdsa_p256_verify_status (param i32 i32 i32) (result i64)))
  (import "witchy" "crypto.__ecdsa_p256_verify_hex_status" (func $crypto_ecdsa_p256_verify_hex_status (param i32 i32 i32) (result i64)))
  (import "witchy" "crypto.__rsa_pkcs1_sha256_verify_status" (func $crypto_rsa_pkcs1_sha256_verify_status (param i32 i32 i32) (result i64)))
  (import "witchy" "crypto.__ed25519_verify_status" (func $crypto_ed25519_verify_status (param i32 i32 i32) (result i64)))
  (import "witchy" "exec_run" (func $exec_run_host (param externref i32 i32 i32) (result i32)))
  (import "witchy" "heap_register" (func $heap_register_host (param i32 i32)))
  (import "witchy" "heap_frontier" (func $heap_frontier_host (param i32)))
  (import "witchy" "__witchy_abort" (func $__witchy_abort (param i32 i64 i64 i32)))
"#;

/// The number of host imports the prelude declares (used to split function
/// indices: imports `0..IMPORT_COUNT`, helpers after).
pub const IMPORT_COUNT: usize = 91;

/// Version of the public `"witchy"` host-import contract.
pub const WITCHY_ABI_VERSION: u32 = 3;

/// The role an import plays at the host boundary. This classification is part
/// of the public Wasm ABI: it tells embedders which imports grant authority,
/// which are pure mechanics, and which are runtime/toolchain plumbing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiImportClass {
    PureInfrastructure,
    CapabilityAuthority,
    LaunchInput,
    InternalService,
    RuntimeDiagnostic,
}

impl AbiImportClass {
    pub const fn label(self) -> &'static str {
        match self {
            Self::PureInfrastructure => "pure infrastructure",
            Self::CapabilityAuthority => "capability authority",
            Self::LaunchInput => "launch input",
            Self::InternalService => "internal/toolchain service",
            Self::RuntimeDiagnostic => "runtime diagnostic",
        }
    }
}

/// The concrete authority family an import observes or exercises. Classes say
/// whether an import is authority-bearing at all; this enum says which grant
/// family owns it, so launchers and hosts do not need local import-name tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AbiImportAuthority {
    Secret,
    Env,
    DirGrant,
    DirRead,
    DirWrite,
    FileGrant,
    FileAuthority,
    NetGrant,
    NetConnect,
    NetListen,
    FetchGrant,
    Fetch,
    Clock,
    Rand,
    Exec,
    BuildRead,
    BuildOut,
    BuildEnv,
    BuildFetch,
    BuildExec,
}

impl AbiImportAuthority {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Secret => "Secret",
            Self::Env => "Env",
            Self::DirGrant => "Dir.grant",
            Self::DirRead => "Dir.Read",
            Self::DirWrite => "Dir.Write",
            Self::FileGrant => "File.grant",
            Self::FileAuthority => "File.authority",
            Self::NetGrant => "Net.grant",
            Self::NetConnect => "Net.Connect",
            Self::NetListen => "Net.Listen",
            Self::FetchGrant => "Fetch.grant",
            Self::Fetch => "Fetch",
            Self::Clock => "Clock",
            Self::Rand => "Rand",
            Self::Exec => "Exec",
            Self::BuildRead => "Build.Read",
            Self::BuildOut => "Build.Out",
            Self::BuildEnv => "Build.Env",
            Self::BuildFetch => "Build.Fetch",
            Self::BuildExec => "Build.Exec",
        }
    }
}

const NO_AUTHORITIES: &[AbiImportAuthority] = &[];
const AUTH_SECRET: &[AbiImportAuthority] = &[AbiImportAuthority::Secret];
const AUTH_ENV: &[AbiImportAuthority] = &[AbiImportAuthority::Env];
const AUTH_DIR_GRANT: &[AbiImportAuthority] = &[AbiImportAuthority::DirGrant];
const AUTH_DIR_READ: &[AbiImportAuthority] = &[AbiImportAuthority::DirRead];
const AUTH_DIR_WRITE: &[AbiImportAuthority] = &[AbiImportAuthority::DirWrite];
const AUTH_FILE_GRANT: &[AbiImportAuthority] = &[AbiImportAuthority::FileGrant];
const AUTH_FILE_AUTHORITY: &[AbiImportAuthority] = &[AbiImportAuthority::FileAuthority];
const AUTH_NET_GRANT: &[AbiImportAuthority] = &[AbiImportAuthority::NetGrant];
const AUTH_NET_CONNECT: &[AbiImportAuthority] = &[AbiImportAuthority::NetConnect];
const AUTH_NET_LISTEN: &[AbiImportAuthority] = &[AbiImportAuthority::NetListen];
const AUTH_NET_LISTEN_SECRET: &[AbiImportAuthority] =
    &[AbiImportAuthority::NetListen, AbiImportAuthority::Secret];
const AUTH_FETCH_GRANT: &[AbiImportAuthority] = &[AbiImportAuthority::FetchGrant];
const AUTH_FETCH: &[AbiImportAuthority] = &[AbiImportAuthority::Fetch];
const AUTH_CLOCK: &[AbiImportAuthority] = &[AbiImportAuthority::Clock];
const AUTH_RAND: &[AbiImportAuthority] = &[AbiImportAuthority::Rand];
const AUTH_EXEC: &[AbiImportAuthority] = &[AbiImportAuthority::Exec];
const AUTH_BUILD_READ: &[AbiImportAuthority] = &[AbiImportAuthority::BuildRead];
const AUTH_BUILD_OUT: &[AbiImportAuthority] = &[AbiImportAuthority::BuildOut];
const AUTH_BUILD_ENV: &[AbiImportAuthority] = &[AbiImportAuthority::BuildEnv];
const AUTH_BUILD_FETCH: &[AbiImportAuthority] = &[AbiImportAuthority::BuildFetch];
const AUTH_BUILD_EXEC: &[AbiImportAuthority] = &[AbiImportAuthority::BuildExec];

/// Public ABI metadata layered over the canonical prelude signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbiImportInfo {
    pub class: AbiImportClass,
    pub authorities: &'static [AbiImportAuthority],
    /// Whether the deny-by-omission browser host implements this import.
    pub browser: bool,
}

/// Classify one canonical host import. Every name is explicit so adding an
/// import cannot silently inherit authority or browser availability.
pub fn abi_import_info(name: &str) -> Option<AbiImportInfo> {
    use AbiImportClass as C;

    let class = match name {
        "print"
        | "crypto.sha256"
        | "crypto.rune_hash"
        | "float_to_str"
        | "encoding"
        | "write_pending_list"
        | "fill_pending"
        | "crypto.sha512"
        | "crypto.sha3_256"
        | "crypto.hmac_sha256"
        | "print_int"
        | "print_float"
        | "string_from_code"
        | "regex_match_spans_len"
        | "crypto.__ecdsa_p256_verify_status"
        | "crypto.__ecdsa_p256_verify_hex_status"
        | "crypto.__rsa_pkcs1_sha256_verify_status"
        | "crypto.__ed25519_verify_status" => C::PureInfrastructure,

        "crypto.sign"
        | "crypto.public_key"
        | "secretstore_lookup"
        | "crypto_reveal_len"
        | "mint_secret"
        | "env_len"
        | "env_fill"
        | "dir_read_len"
        | "dir_list_size"
        | "build_read_len"
        | "build_out_write"
        | "build_env_len"
        | "build_env_fill"
        | "build_fetch_len"
        | "build_exec_run"
        | "net_recv_line_len"
        | "net_recv_all_len"
        | "net_recv_bytes_len"
        | "dir_subdir"
        | "dir_only"
        | "dir_exists"
        | "dir_is_dir"
        | "dir_write"
        | "dir_append"
        | "dir_make_dir"
        | "mint_dir"
        | "dir_open"
        | "dir_create"
        | "mint_file"
        | "file_read_len"
        | "file_write"
        | "mint_net"
        | "mint_fetch"
        | "fetch_only"
        | "fetch_send_len"
        | "net_fetch"
        | "net_connect"
        | "net_try_connect"
        | "net_resolve_size"
        | "net_connect_pinned"
        | "net_try_connect_pinned"
        | "net_listen"
        | "net_listen_tls"
        | "net_accept"
        | "net_restrict"
        | "net_deny"
        | "net_send_line"
        | "net_send_bytes"
        | "net_close"
        | "serve_pool"
        | "now"
        | "now_monotonic"
        | "rand_u64"
        | "exec_run" => C::CapabilityAuthority,

        "args_size" | "user_cap_field_len" => C::LaunchInput,

        "compiler_footprint_len"
        | "compiler_diff_len"
        | "compiler_doc_len"
        | "compiler_doc_result_json_len"
        | "field_str_len"
        | "field_intlist_len"
        | "field_strlist_size"
        | "vm_par_map_run"
        | "vm_par_map_write"
        | "vm_par_map_bytes_run"
        | "vm_par_map_bytes_write"
        | "vm_with_dir_run"
        | "vm_serve_run"
        | "testing_mock_dir" => C::InternalService,

        "heap_register" | "heap_frontier" | "__witchy_abort" => C::RuntimeDiagnostic,
        _ => return None,
    };

    let authorities = match name {
        "crypto.sign" | "crypto.public_key" | "secretstore_lookup" | "crypto_reveal_len" | "mint_secret" => AUTH_SECRET,
        "env_len" | "env_fill" => AUTH_ENV,
        "mint_dir" => AUTH_DIR_GRANT,
        "dir_read_len"
        | "dir_list_size"
        | "dir_subdir"
        | "dir_only"
        | "dir_exists"
        | "dir_is_dir"
        | "dir_open" => AUTH_DIR_READ,
        "dir_write" | "dir_append" | "dir_make_dir" | "dir_create" => AUTH_DIR_WRITE,
        "mint_file" => AUTH_FILE_GRANT,
        "file_read_len" | "file_write" => AUTH_FILE_AUTHORITY,
        "mint_net" => AUTH_NET_GRANT,
        "mint_fetch" => AUTH_FETCH_GRANT,
        "fetch_only" | "fetch_send_len" => AUTH_FETCH,
        "net_fetch" => AUTH_NET_CONNECT,
        "net_connect"
        | "net_try_connect"
        | "net_resolve_size"
        | "net_connect_pinned"
        | "net_try_connect_pinned"
        | "net_restrict"
        | "net_deny"
        | "net_send_line"
        | "net_send_bytes"
        | "net_close"
        | "net_recv_line_len"
        | "net_recv_all_len"
        | "net_recv_bytes_len" => AUTH_NET_CONNECT,
        "net_listen" | "net_accept" | "serve_pool" => AUTH_NET_LISTEN,
        "net_listen_tls" => AUTH_NET_LISTEN_SECRET,
        "now" | "now_monotonic" => AUTH_CLOCK,
        "rand_u64" => AUTH_RAND,
        "exec_run" => AUTH_EXEC,
        "build_read_len" => AUTH_BUILD_READ,
        "build_out_write" => AUTH_BUILD_OUT,
        "build_env_len" | "build_env_fill" => AUTH_BUILD_ENV,
        "build_fetch_len" => AUTH_BUILD_FETCH,
        "build_exec_run" => AUTH_BUILD_EXEC,
        _ => NO_AUTHORITIES,
    };

    let browser = matches!(
        name,
        "print"
            | "crypto.sha256"
            | "crypto.rune_hash"
            | "user_cap_field_len"
            | "field_str_len"
            | "field_intlist_len"
            | "field_strlist_size"
            | "float_to_str"
            | "encoding"
            | "write_pending_list"
            | "fill_pending"
            | "crypto.sha512"
            | "crypto.sha3_256"
            | "crypto.hmac_sha256"
            | "print_int"
            | "print_float"
            | "string_from_code"
            | "regex_match_spans_len"
            | "crypto.__ecdsa_p256_verify_status"
            | "crypto.__ecdsa_p256_verify_hex_status"
            | "crypto.__rsa_pkcs1_sha256_verify_status"
            | "crypto.__ed25519_verify_status"
            | "args_size"
            | "__witchy_abort"
    );
    Some(AbiImportInfo { class, authorities, browser })
}

/// Whether a host import is cataloged as using a specific authority family.
pub fn abi_import_uses_authority(name: &str, authority: AbiImportAuthority) -> bool {
    abi_import_info(name).is_some_and(|info| info.authorities.contains(&authority))
}

fn abi_authority_list(authorities: &[AbiImportAuthority]) -> String {
    if authorities.is_empty() {
        "none".to_string()
    } else {
        authorities.iter().map(|a| a.label()).collect::<Vec<_>>().join(", ")
    }
}

fn wasm_ty_name(ty: WasmTy) -> &'static str {
    match ty {
        WasmTy::I32 => "i32",
        WasmTy::I64 => "i64",
        WasmTy::F32 => "f32",
        WasmTy::F64 => "f64",
        WasmTy::ExternRef => "externref",
    }
}

fn abi_signature(import: &PreludeImport) -> String {
    let params = import.params.iter().copied().map(wasm_ty_name).collect::<Vec<_>>().join(", ");
    if import.results.is_empty() {
        format!("({params})")
    } else {
        let results = import.results.iter().copied().map(wasm_ty_name).collect::<Vec<_>>().join(", ");
        format!("({params}) -> {results}")
    }
}

/// Render the complete generated import table committed in `spec/wasm-abi.md`.
/// The spec test compares this output byte-for-byte, so names, signatures,
/// classifications, browser support, and the declared count cannot drift.
pub fn render_abi_import_catalog() -> String {
    use std::fmt::Write as _;

    let mut out = String::from(
        "| import | signature | class | authority | browser |\n\
         | --- | --- | --- | --- | --- |\n",
    );
    for import in &prelude().imports {
        let info = abi_import_info(&import.name)
            .unwrap_or_else(|| panic!("ABI import `{}` has no classification", import.name));
        writeln!(
            out,
            "| `{}` | `{}` | {} | {} | {} |",
            import.name,
            abi_signature(import),
            info.class.label(),
            abi_authority_list(info.authorities),
            if info.browser { "provided" } else { "omitted" },
        )
        .expect("writing to a String cannot fail");
    }
    out
}

/// The full ordered name list for the funcs section: `$mk0..$mk{MAX_MK}` then
/// the static helper names. Matches the order the prelude emits bodies, so
/// the i-th code-section entry is `func_names()[i]`.
fn func_names() -> Vec<String> {
    let mut v: Vec<String> = (0..=MAX_MK).map(|n| format!("mk{n}")).collect();
    v.extend(HELPER_NAMES.iter().map(|s| s.to_string()));
    v
}

/// Parse the host-import declarations straight out of `PRELUDE_IMPORTS_WAT` — the
/// same `Vec<PreludeImport>` the committed blob yields, but with NO `wat` crate.
/// Each line is `(import "witchy" "<name>" (func $<alias> (param T…)* (result T…)?))`:
/// `<name>` is the second quoted string; params/results are the whitespace-split
/// valtypes inside the `(param …)` / `(result …)` groups. The step that lets
/// `build_prelude` stop depending on a WAT-assembled blob.
fn parse_prelude_imports(wat: &str) -> Vec<PreludeImport> {
    fn ty(t: &str) -> WasmTy {
        match t {
            "i32" => WasmTy::I32,
            "i64" => WasmTy::I64,
            "f32" => WasmTy::F32,
            "f64" => WasmTy::F64,
            "externref" => WasmTy::ExternRef,
            other => panic!("prelude import uses an unexpected valtype `{other}`"),
        }
    }
    let mut out = Vec::new();
    for line in wat.lines() {
        let line = line.trim();
        if !line.starts_with("(import") {
            continue;
        }
        let mut quoted = line.split('"');
        quoted.next(); // text before the first quote
        let module = quoted.next().unwrap_or_default().to_string();
        quoted.next(); // text between the two quoted strings
        let name = quoted.next().unwrap_or_default().to_string();
        let mut params = Vec::new();
        let mut rest = line;
        while let Some(i) = rest.find("(param ") {
            let after = &rest[i + "(param ".len()..];
            let end = after.find(')').expect("unterminated (param …) in a prelude import");
            params.extend(after[..end].split_whitespace().map(ty));
            rest = &after[end..];
        }
        let mut results = Vec::new();
        if let Some(i) = line.find("(result ") {
            let after = &line[i + "(result ".len()..];
            let end = after.find(')').expect("unterminated (result …) in a prelude import");
            results.extend(after[..end].split_whitespace().map(ty));
        }
        out.push(PreludeImport { module, name, params, results });
    }
    out
}

fn build_prelude() -> Prelude {
    // The binary codegen path reads ONLY the import signatures (for the pruned
    // module's import declarations) and the helper NAMES (the resolution filter
    // at codegen `helper_names`) — the helper BODIES are vestigial (the raw-body
    // emit path is retired: `assemble_wir_module` either prunes from the
    // `wir_helper` registry or bails), and globals/table/elems are unused (the
    // pruned module builds its own heap/reowns/region globals + lambda table). So
    // construct the prelude straight from Rust: imports parsed from
    // `PRELUDE_IMPORTS_WAT` (no `wat` crate), funcs = the `HELPER_NAMES` set with
    // empty stub bodies. No committed `prelude.wasm` blob, no WAT assembly.
    Prelude {
        imports: parse_prelude_imports(PRELUDE_IMPORTS_WAT),
        funcs: func_names()
            .into_iter()
            .map(|name| PreludeFunc {
                name,
                params: Vec::new(),
                results: Vec::new(),
                raw_body: Vec::new(),
            })
            .collect(),
        globals: Vec::new(),
        table_size: 0,
        elems: Vec::new(),
    }
}

/// The pre-compiled, splice-ready static runtime prelude. Assembled and
/// extracted ONCE, lazily, on first call (so the `wat` crate stays off the hot
/// per-program compile path).
pub fn prelude() -> &'static Prelude {
    static PRELUDE: OnceLock<Prelude> = OnceLock::new();
    PRELUDE.get_or_init(build_prelude)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pure-Rust import parser yields the complete prelude import set straight
    /// from `PRELUDE_IMPORTS_WAT` — no `wat` crate, no committed blob. (It was once
    /// checked byte-for-byte against the `wat`-assembled blob, commit 86bfda8,
    /// before `build_prelude` switched to this parser; now guard the count + sigs.)
    #[test]
    fn parsed_prelude_imports_are_complete() {
        let imports = parse_prelude_imports(PRELUDE_IMPORTS_WAT);
        assert_eq!(imports.len(), IMPORT_COUNT, "parsed {} imports, expected {IMPORT_COUNT}", imports.len());
        // The first import: `print` (two i32s, no result).
        assert_eq!(imports[0].module, "witchy");
        assert_eq!(imports[0].name, "print");
        assert_eq!(imports[0].params, vec![WasmTy::I32, WasmTy::I32]);
        assert!(imports[0].results.is_empty());
        // A result-returning, param-less import: `now` -> i64.
        let now = imports.iter().find(|i| i.name == "now").expect("the `now` import is present");
        assert!(now.params.is_empty());
        assert_eq!(now.results, vec![WasmTy::I64]);
    }

    #[test]
    fn every_prelude_import_has_abi_metadata() {
        let imports = &prelude().imports;
        let names = imports.iter().map(|i| i.name.as_str()).collect::<std::collections::BTreeSet<_>>();
        assert_eq!(names.len(), imports.len(), "ABI import names must be unique");
        for import in imports {
            assert!(
                abi_import_info(&import.name).is_some(),
                "ABI import `{}` needs an explicit class and browser decision",
                import.name
            );
        }
        assert_eq!(render_abi_import_catalog().lines().count(), IMPORT_COUNT + 2);
        assert!(
            abi_import_info("args_size").is_some_and(|info| info.browser),
            "argv launch input is implemented by the browser host"
        );
    }

    /// The prelude exposes the helper NAMES the binary path's resolution filter
    /// needs (the `HELPER_NAMES` set). It no longer carries the helper bodies or
    /// signatures — the binary path emits the `wir_helper` registry forms — so the
    /// `Prelude` is built directly in Rust, with no `prelude.wasm` blob.
    #[test]
    fn prelude_exposes_the_helper_names() {
        let p = prelude();
        assert_eq!(p.funcs.len(), func_names().len(), "helper-name count");
        let has = |n: &str| p.funcs.iter().any(|f| f.name == n);
        for n in ["concat", "str_eq", "mk1"] {
            assert!(has(n), "{n} present in the prelude helper names");
        }
        for n in 0..=MAX_MK {
            assert!(has(&format!("mk{n}")), "mk{n} present");
        }
    }
}
