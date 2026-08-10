//! Runtime-helper registry and dependency catalog.
//!
//! Helper constructors remain owned by their implementation domains. This
//! module is the single name-to-spec dispatcher consumed by lowering.

use super::*;
use super::encoding::{crypto_hash_helper, crypto_keyed_helper, crypto_xof_helper};
use super::host::{
    compiler_introspect_helper, host_call_helper_ret, host_call_helper_typed,
    host_void_helper, host_void_helper_typed, net_recv_helper, staged_string_helper,
    staged_string_helper_typed,
    two_phase_helper_typed,
};
use crate::wir::*;

/// A WIR-native prelude helper plus the module-level resources it needs (so a
/// pruned module declares only the imports/globals/table its reached helpers
/// actually touch — capability-minimal).
pub struct WirHelperSpec {
    pub func: WirFunc,
    /// Other prelude helpers this one calls (transitively pulled in).
    pub helper_deps: &'static [&'static str],
    /// Host imports (the `witchy` field names) this helper calls directly.
    pub import_deps: &'static [&'static str],
    /// Whether it reads/writes the `$heap` / `$__witchy_reowns` globals.
    pub uses_heap: bool,
    /// Whether it does a `call_indirect` (needs table 0).
    pub uses_table: bool,
}

/// Look up a runtime helper by name, returning its [`WirHelperSpec`]: the
/// function plus the other helpers and host imports it depends on. Returns
/// `None` for a name with no WIR-native helper, in which case `wir_encode` falls
/// back to the raw-body prelude blob (`wir_prelude`).
pub fn wir_helper(name: &str) -> Option<WirHelperSpec> {
    match name {
        "print_str" => Some(WirHelperSpec {
            func: print_str_helper(),
            helper_deps: &[],
            import_deps: &["print"],
            uses_heap: false,
            uses_table: false,
        }),
        "console_read" => Some(WirHelperSpec {
            func: staged_string_helper("console_read", &[], "console_read_len"),
            helper_deps: &["rc_alloc"],
            import_deps: &["console_read_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "ensure" => {
            let checked = heap_check_enabled();
            let import_deps: &'static [&'static str] =
                if checked { &["heap_frontier"] } else { &[] };
            Some(WirHelperSpec {
                func: ensure_helper(checked),
                helper_deps: &[],
                import_deps,
                uses_heap: true,
                uses_table: false,
            })
        }
        // (RFC-0023) Only ever reached when the checked codegen emits a call to it.
        "__heap_reclaim" => Some(WirHelperSpec {
            func: heap_reclaim_helper(),
            helper_deps: &[],
            import_deps: &["heap_frontier"],
            uses_heap: false,
            uses_table: false,
        }),
        "bump_alloc" => Some(WirHelperSpec {
            func: bump_alloc_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "rc_alloc" => Some(WirHelperSpec {
            func: rc_alloc_helper(),
            helper_deps: &["ensure", "bump_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "rc_free" => {
            let heap_check = heap_check_enabled();
            let uaf_check = uaf_check_enabled();
            let import_deps: &'static [&'static str] =
                if heap_check && uaf_check { &["heap_unregister"] } else { &[] };
            Some(WirHelperSpec {
                func: rc_free_helper(),
                helper_deps: &[],
                import_deps,
                uses_heap: true,
                uses_table: false,
            })
        }
        "rc_dup" => Some(WirHelperSpec {
            func: rc_dup_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "leaf_dup" => Some(WirHelperSpec {
            func: leaf_dup_helper(),
            helper_deps: &["rc_dup"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "leaf_drop" => Some(WirHelperSpec {
            func: leaf_drop_helper(),
            helper_deps: &["rc_drop"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "slot_take_or_dup" => Some(WirHelperSpec {
            func: slot_take_or_dup_helper(),
            helper_deps: &["leaf_dup"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "rc_drop" => Some(WirHelperSpec {
            func: rc_drop_helper(),
            helper_deps: &["rc_free"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_at" => Some(WirHelperSpec {
            func: list_at_helper(),
            helper_deps: &[],
            // (RFC-0045) routes its OOB abort through `__witchy_abort`.
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "list_at_view" => Some(WirHelperSpec {
            func: list_at_view_helper(),
            helper_deps: &[],
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "bytes_at" => Some(WirHelperSpec {
            func: bytes_at_helper(),
            helper_deps: &[],
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "bytes_from_list" => Some(WirHelperSpec {
            func: bytes_from_list_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_len_view" => Some(WirHelperSpec {
            func: list_len_view_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "int_to_string" => {
            let checked = heap_check_enabled();
            let import_deps: &'static [&'static str] =
                if checked { &["heap_register"] } else { &[] };
            Some(WirHelperSpec {
                func: int_to_string_helper(checked),
                helper_deps: &["rc_alloc"],
                import_deps,
                uses_heap: true,
                uses_table: false,
            })
        }
        "str_eq" => Some(WirHelperSpec {
            func: str_eq_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "find_byte" => Some(WirHelperSpec {
            func: find_byte_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "starts_with" => Some(WirHelperSpec {
            func: starts_with_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "ends_with" => Some(WirHelperSpec {
            func: ends_with_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "byte_to_char" => Some(WirHelperSpec {
            func: byte_to_char_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "char_count" => Some(WirHelperSpec {
            func: char_count_helper(),
            helper_deps: &["byte_to_char"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "substr" => Some(WirHelperSpec {
            func: substr_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "char_to_byte" => Some(WirHelperSpec {
            func: char_to_byte_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "str_substring" => Some(WirHelperSpec {
            func: str_substring_helper(),
            helper_deps: &["char_to_byte", "substr"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "bytes_slice" => Some(WirHelperSpec {
            func: bytes_slice_helper(),
            helper_deps: &["substr"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "bytes_to_string" => Some(WirHelperSpec {
            func: bytes_to_string_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["encoding"],
            uses_heap: true,
            uses_table: false,
        }),
        "is_ws" => Some(WirHelperSpec {
            func: is_ws_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "trim" => Some(WirHelperSpec {
            func: trim_helper(),
            helper_deps: &["is_ws", "substr"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "str_index_of" => Some(WirHelperSpec {
            func: str_index_of_helper(),
            helper_deps: &["find_byte", "byte_to_char"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "concat" => Some(WirHelperSpec {
            func: concat_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_push_cap" => Some(WirHelperSpec {
            func: list_push_cap_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_push" => Some(WirHelperSpec {
            func: list_push_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "split" => Some(WirHelperSpec {
            func: split_helper(),
            helper_deps: &["rc_alloc", "substr", "list_push"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "str_chars" => Some(WirHelperSpec {
            func: str_chars_helper(),
            helper_deps: &["rc_alloc", "byte_to_char", "str_substring", "list_push"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_concat" => Some(WirHelperSpec {
            func: list_concat_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_drop" => Some(WirHelperSpec {
            func: list_drop_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "ascii_case" => Some(WirHelperSpec {
            func: ascii_case_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "match_at" => Some(WirHelperSpec {
            func: match_at_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "encoding" => Some(WirHelperSpec {
            func: encoding_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["encoding"],
            uses_heap: true,
            uses_table: false,
        }),
        "string_from_code" => Some(WirHelperSpec {
            func: string_from_code_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["string_from_code"],
            uses_heap: true,
            uses_table: false,
        }),
        "build_args" => Some(WirHelperSpec {
            func: build_args_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["args_size", "write_pending_list"],
            uses_heap: true,
            uses_table: false,
        }),
        "compiler_footprint" => Some(WirHelperSpec {
            func: compiler_introspect_helper("compiler_footprint", "compiler_footprint_len", 1),
            helper_deps: &["rc_alloc"],
            import_deps: &["compiler_footprint_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        // RFC-0038: materialize one policy field of a bare grantable capability
        // grant as a guest `String` (same host-staged-string pattern). Codegen wraps
        // N of these in `mk{N}` to build the sealed record at the root.
        "build_user_cap_field" => Some(WirHelperSpec {
            func: compiler_introspect_helper("build_user_cap_field", "user_cap_field_len", 2),
            helper_deps: &["rc_alloc"],
            import_deps: &["user_cap_field_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "compiler_diff" => Some(WirHelperSpec {
            func: compiler_introspect_helper("compiler_diff", "compiler_diff_len", 2),
            helper_deps: &["rc_alloc"],
            import_deps: &["compiler_diff_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "compiler_doc" => Some(WirHelperSpec {
            func: compiler_introspect_helper("compiler_doc", "compiler_doc_len", 2),
            helper_deps: &["rc_alloc"],
            import_deps: &["compiler_doc_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "compiler_doc_result_json" => Some(WirHelperSpec {
            func: compiler_introspect_helper("compiler_doc_result_json", "compiler_doc_result_json_len", 2),
            helper_deps: &["rc_alloc"],
            import_deps: &["compiler_doc_result_json_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "now" => Some(WirHelperSpec {
            func: host_call_helper_ret("now", "now", 0, WirTy::Int),
            helper_deps: &[],
            import_deps: &["now"],
            uses_heap: false,
            uses_table: false,
        }),
        "now_monotonic" => Some(WirHelperSpec {
            func: host_call_helper_ret("now_monotonic", "now_monotonic", 0, WirTy::Int),
            helper_deps: &[],
            import_deps: &["now_monotonic"],
            uses_heap: false,
            uses_table: false,
        }),
        "rand_u64" => Some(WirHelperSpec {
            func: host_call_helper_ret("rand_u64", "rand_u64", 0, WirTy::Int),
            helper_deps: &[],
            import_deps: &["rand_u64"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_subdir" => Some(WirHelperSpec {
            func: host_call_helper_typed("dir_subdir", "dir_subdir", &[WirTy::Extern, WirTy::Str], WirTy::Extern),
            helper_deps: &[],
            import_deps: &["dir_subdir"],
            uses_heap: false,
            uses_table: false,
        }),
        // RFC-0011/RFC-0005: `dir.only(DirPolicy)` narrows a Dir's entry policy,
        // minting a fresh unforgeable Dir externref.
        "dir_only" => Some(WirHelperSpec {
            func: host_call_helper_typed("dir_only", "dir_only", &[WirTy::Extern, WirTy::Str], WirTy::Extern),
            helper_deps: &[],
            import_deps: &["dir_only"],
            uses_heap: false,
            uses_table: false,
        }),
        "fetch_only" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "fetch_only",
                "fetch_only",
                &[WirTy::Extern, WirTy::Str],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["fetch_only"],
            uses_heap: false,
            uses_table: false,
        }),
        "env_only" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "env_only",
                "env_only",
                &[WirTy::Extern, WirTy::List(Box::new(WirTy::Str))],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["env_only"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_fetch" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "net_fetch",
                "net_fetch",
                &[WirTy::Extern, WirTy::Str],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["net_fetch"],
            uses_heap: false,
            uses_table: false,
        }),
        "fetch_send" => Some(WirHelperSpec {
            func: staged_string_helper_typed(
                "fetch_send",
                &[
                    ("fetch".into(), WirTy::Extern),
                    ("method".into(), WirTy::Str),
                    ("url".into(), WirTy::Str),
                    ("headers".into(), WirTy::Str),
                    ("body".into(), WirTy::Str),
                ],
                "fetch_send_len",
            ),
            helper_deps: &["rc_alloc"],
            import_deps: &["fetch_send_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        // RFC-0012/RFC-0005 Stage 2: `dir.open`/`dir.create` navigate a Dir to a
        // confined File externref; `file_write` consumes that externref. Each wraps
        // its host import so user code stays free of direct CallHosts.
        "dir_open" => Some(WirHelperSpec {
            func: host_call_helper_typed("dir_open", "dir_open", &[WirTy::Extern, WirTy::Str], WirTy::Extern),
            helper_deps: &[],
            import_deps: &["dir_open"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_create" => Some(WirHelperSpec {
            func: host_call_helper_typed("dir_create", "dir_create", &[WirTy::Extern, WirTy::Str], WirTy::Extern),
            helper_deps: &[],
            import_deps: &["dir_create"],
            uses_heap: false,
            uses_table: false,
        }),
        "file_write" => Some(WirHelperSpec {
            func: host_void_helper_typed("file_write", "file_write", &[WirTy::Extern, WirTy::Str]),
            helper_deps: &[],
            import_deps: &["file_write"],
            uses_heap: false,
            uses_table: false,
        }),
        // Resolve a named secret to a nullable opaque externref (`None` is null).
        // Wraps the `secretstore_lookup` host import so user code stays free of
        // direct CallHosts; the bytes never enter the guest.
        "secretstore_lookup" => Some(WirHelperSpec {
            func: host_call_helper_typed("secretstore_lookup", "secretstore_lookup", &[WirTy::Str], WirTy::Extern),
            helper_deps: &[],
            import_deps: &["secretstore_lookup"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_exists" => Some(WirHelperSpec {
            func: host_call_helper_typed("dir_exists", "dir_exists", &[WirTy::Extern, WirTy::Str], WirTy::Bool),
            helper_deps: &[],
            import_deps: &["dir_exists"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_is_dir" => Some(WirHelperSpec {
            func: host_call_helper_typed("dir_is_dir", "dir_is_dir", &[WirTy::Extern, WirTy::Str], WirTy::Bool),
            helper_deps: &[],
            import_deps: &["dir_is_dir"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_write_bytes" => Some(WirHelperSpec {
            func: host_void_helper_typed("dir_write_bytes", "dir_write_bytes", &[WirTy::Extern, WirTy::Str, WirTy::Str]),
            helper_deps: &[],
            import_deps: &["dir_write_bytes"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_write" => Some(WirHelperSpec {
            func: host_void_helper_typed("dir_write", "dir_write", &[WirTy::Extern, WirTy::Str, WirTy::Str]),
            helper_deps: &[],
            import_deps: &["dir_write"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_append" => Some(WirHelperSpec {
            func: host_void_helper_typed("dir_append", "dir_append", &[WirTy::Extern, WirTy::Str, WirTy::Str]),
            helper_deps: &[],
            import_deps: &["dir_append"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_make_dir" => Some(WirHelperSpec {
            func: host_void_helper_typed("dir_make_dir", "dir_make_dir", &[WirTy::Extern, WirTy::Str]),
            helper_deps: &[],
            import_deps: &["dir_make_dir"],
            uses_heap: false,
            uses_table: false,
        }),
        // RFC-0118 atomic primitives. `dir_create_new` returns Bool (1 = created,
        // 0 = already existed); `dir_replace`/`dir_rename` are void effects.
        "dir_create_new" => Some(WirHelperSpec {
            func: host_call_helper_typed("dir_create_new", "dir_create_new", &[WirTy::Extern, WirTy::Str, WirTy::Str], WirTy::Bool),
            helper_deps: &[],
            import_deps: &["dir_create_new"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_replace" => Some(WirHelperSpec {
            func: host_void_helper_typed("dir_replace", "dir_replace", &[WirTy::Extern, WirTy::Str, WirTy::Str]),
            helper_deps: &[],
            import_deps: &["dir_replace"],
            uses_heap: false,
            uses_table: false,
        }),
        "dir_rename" => Some(WirHelperSpec {
            func: host_void_helper_typed("dir_rename", "dir_rename", &[WirTy::Extern, WirTy::Str, WirTy::Str]),
            helper_deps: &[],
            import_deps: &["dir_rename"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_connect" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "net_connect",
                "net_connect",
                &[WirTy::Extern, WirTy::Str],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["net_connect"],
            uses_heap: false,
            uses_table: false,
        }),
        // Fallible dial: returns nullable externref `Some(Socket)`/`None`. A
        // capability violation still traps host-side.
        "net_try_connect" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "net_try_connect",
                "net_try_connect",
                &[WirTy::Extern, WirTy::Str],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["net_try_connect"],
            uses_heap: false,
            uses_table: false,
        }),
        // (RFC-0020) Pinned dials — thin passthroughs like `net_connect`, but the
        // fourth param is the i64 `port` (an `Int`), so a typed helper spells the
        // signature `(net, ip, host, port, secure) -> Socket` out.
        "net_connect_pinned" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "net_connect_pinned",
                "net_connect_pinned",
                &[WirTy::Extern, WirTy::Str, WirTy::Str, WirTy::Int, WirTy::Bool],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["net_connect_pinned"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_try_connect_pinned" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "net_try_connect_pinned",
                "net_try_connect_pinned",
                &[WirTy::Extern, WirTy::Str, WirTy::Str, WirTy::Int, WirTy::Bool],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["net_try_connect_pinned"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_listen" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "net_listen",
                "net_listen",
                &[WirTy::Extern, WirTy::Str],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["net_listen"],
            uses_heap: false,
            uses_table: false,
        }),
        // (RFC-0060) HTTPS listen: `(net, addr, cert_pem, key) -> Listener`.
        // Both authority-bearing args are externrefs; the private key bytes stay
        // host-side.
        "net_listen_tls" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "net_listen_tls",
                "net_listen_tls",
                &[WirTy::Extern, WirTy::Str, WirTy::Str, WirTy::Extern],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["net_listen_tls"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_accept" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "net_accept",
                "net_accept",
                &[WirTy::Extern],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["net_accept"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_restrict" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "net_restrict",
                "net_restrict",
                &[WirTy::Extern, WirTy::Str],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["net_restrict"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_deny" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "net_deny",
                "net_deny",
                &[WirTy::Extern, WirTy::Str],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["net_deny"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_send_line" => Some(WirHelperSpec {
            func: host_void_helper_typed("net_send_line", "net_send_line", &[WirTy::Extern, WirTy::Str]),
            helper_deps: &[],
            import_deps: &["net_send_line"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_send_bytes" => Some(WirHelperSpec {
            func: host_void_helper_typed("net_send_bytes", "net_send_bytes", &[WirTy::Extern, WirTy::Str]),
            helper_deps: &[],
            import_deps: &["net_send_bytes"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_close" => Some(WirHelperSpec {
            func: host_void_helper_typed("net_close", "net_close", &[WirTy::Extern]),
            helper_deps: &[],
            import_deps: &["net_close"],
            uses_heap: false,
            uses_table: false,
        }),
        "serve_pool" => Some(WirHelperSpec {
            func: host_void_helper_typed("serve_pool", "serve_pool", &[WirTy::Extern]),
            helper_deps: &[],
            import_deps: &["serve_pool"],
            uses_heap: false,
            uses_table: false,
        }),
        "net_recv_line" => Some(WirHelperSpec {
            func: net_recv_helper("net_recv_line", "net_recv_line_len", false),
            helper_deps: &["rc_alloc"],
            import_deps: &["net_recv_line_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "net_recv_all" => Some(WirHelperSpec {
            func: net_recv_helper("net_recv_all", "net_recv_all_len", false),
            helper_deps: &["rc_alloc"],
            import_deps: &["net_recv_all_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "net_recv_bytes" => Some(WirHelperSpec {
            func: net_recv_helper("net_recv_bytes", "net_recv_bytes_len", true),
            helper_deps: &["rc_alloc"],
            import_deps: &["net_recv_bytes_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        // The region globals ($rcopy_wm/$rcopy_base/$rcopy_delta/$__region_copy_bytes)
        // this touches are declared by `assemble` when `cg.uses_region` is set.
        "rcopy_str" => Some(WirHelperSpec {
            func: rcopy_str_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "float_to_str" => Some(WirHelperSpec {
            func: float_to_str_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["float_to_str"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sha256" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_sha256", "crypto.sha256", 64, &["in"]),
            helper_deps: &["rc_alloc"],
            import_deps: &["crypto.sha256"],
            uses_heap: true,
            uses_table: false,
        }),
        // RFC-0095: SHA-256 over raw Bytes. A Bytes pointer has the same
        // `[len][payload]` layout as a Str, so the string hash helper works
        // unchanged — only the host reads the input as bytes and the guest wraps
        // the input pointer as Bytes rather than Str.
        "crypto_sha256_bytes" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_sha256_bytes", "crypto.sha256_bytes", 64, &["in"]),
            helper_deps: &["rc_alloc"],
            import_deps: &["crypto.sha256_bytes"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sha512" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_sha512", "crypto.sha512", 128, &["in"]),
            helper_deps: &["rc_alloc"],
            import_deps: &["crypto.sha512"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sha3_256" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_sha3_256", "crypto.sha3_256", 64, &["in"]),
            helper_deps: &["rc_alloc"],
            import_deps: &["crypto.sha3_256"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_hmac_sha256" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_hmac_sha256", "crypto.hmac_sha256", 64, &["key", "msg"]),
            helper_deps: &["rc_alloc"],
            import_deps: &["crypto.hmac_sha256"],
            uses_heap: true,
            uses_table: false,
        }),
        // (RFC-0106) SHAKE XOFs: variable-length raw-Bytes output via a direct
        // host output pointer. Native-only; the browser host omits the imports.
        "crypto_shake128" => Some(WirHelperSpec {
            func: crypto_xof_helper("crypto_shake128", "crypto.__shake128"),
            helper_deps: &["rc_alloc"],
            import_deps: &["crypto.__shake128"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_shake256" => Some(WirHelperSpec {
            func: crypto_xof_helper("crypto_shake256", "crypto.__shake256"),
            helper_deps: &["rc_alloc"],
            import_deps: &["crypto.__shake256"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_rune_hash" => Some(WirHelperSpec {
            // paths + contents are List(String) pointers; the host hashes them
            // into a fixed 71-char digest.
            func: crypto_hash_helper("crypto_rune_hash", "crypto.rune_hash", 71, &["paths", "contents"]),
            helper_deps: &["rc_alloc"],
            import_deps: &["crypto.rune_hash"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sign" => Some(WirHelperSpec {
            // The Secret capability: the host signs `msg` with the never-exposed
            // seed and writes a 128-char hex signature.
            func: crypto_keyed_helper("crypto_sign", "crypto.sign", 128, true),
            helper_deps: &["rc_alloc"],
            import_deps: &["crypto.sign"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_public_key" => Some(WirHelperSpec {
            // No input — the host writes the seed's 64-char hex public key.
            func: crypto_keyed_helper("crypto_public_key", "crypto.public_key", 64, false),
            helper_deps: &["rc_alloc"],
            import_deps: &["crypto.public_key"],
            uses_heap: true,
            uses_table: false,
        }),
        // The verifier status helpers read three string headers and return an
        // Int status: 1 valid, 0 invalid signature, negative malformed input.
        "crypto_ecdsa_p256_verify_status" => Some(WirHelperSpec {
            func: host_call_helper_ret(
                "crypto_ecdsa_p256_verify_status",
                "crypto.__ecdsa_p256_verify_status",
                3,
                WirTy::Int,
            ),
            helper_deps: &[],
            import_deps: &["crypto.__ecdsa_p256_verify_status"],
            uses_heap: false,
            uses_table: false,
        }),
        "crypto_ecdsa_p256_verify_hex_status" => Some(WirHelperSpec {
            func: host_call_helper_ret(
                "crypto_ecdsa_p256_verify_hex_status",
                "crypto.__ecdsa_p256_verify_hex_status",
                3,
                WirTy::Int,
            ),
            helper_deps: &[],
            import_deps: &["crypto.__ecdsa_p256_verify_hex_status"],
            uses_heap: false,
            uses_table: false,
        }),
        "crypto_rsa_pkcs1_sha256_verify_status" => Some(WirHelperSpec {
            func: host_call_helper_ret(
                "crypto_rsa_pkcs1_sha256_verify_status",
                "crypto.__rsa_pkcs1_sha256_verify_status",
                3,
                WirTy::Int,
            ),
            helper_deps: &[],
            import_deps: &["crypto.__rsa_pkcs1_sha256_verify_status"],
            uses_heap: false,
            uses_table: false,
        }),
        "crypto_ed25519_verify_status" => Some(WirHelperSpec {
            func: host_call_helper_ret(
                "crypto_ed25519_verify_status",
                "crypto.__ed25519_verify_status",
                3,
                WirTy::Int,
            ),
            helper_deps: &[],
            import_deps: &["crypto.__ed25519_verify_status"],
            uses_heap: false,
            uses_table: false,
        }),
        "regex_match_spans" => Some(WirHelperSpec {
            func: regex_match_spans_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["regex_match_spans_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "dir_read" => Some(WirHelperSpec {
            func: dir_read_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["dir_read_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        // RFC-0095 byte-safe read: same two-phase protocol as dir_read, raw bytes.
        "dir_read_bytes" => Some(WirHelperSpec {
            func: dir_read_bytes_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["dir_read_bytes_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "file_read" => Some(WirHelperSpec {
            func: file_read_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["file_read_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "exec" => Some(WirHelperSpec {
            func: exec_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["exec_run", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "exec_only" => Some(WirHelperSpec {
            func: host_call_helper_typed(
                "exec_only",
                "exec_only",
                &[WirTy::Extern, WirTy::List(Box::new(WirTy::Str))],
                WirTy::Extern,
            ),
            helper_deps: &[],
            import_deps: &["exec_only"],
            uses_heap: false,
            uses_table: false,
        }),
        "crypto_reveal" => Some(WirHelperSpec {
            func: crypto_reveal_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["crypto_reveal_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "build_read" => Some(WirHelperSpec {
            func: build_read_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["build_read_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "build_out_write" => Some(WirHelperSpec {
            func: host_void_helper("build_out_write", "build_out_write", 2),
            helper_deps: &[],
            import_deps: &["build_out_write"],
            uses_heap: false,
            uses_table: false,
        }),
        "build_get_env" => Some(WirHelperSpec {
            func: build_get_env_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["build_env_len", "build_env_fill"],
            uses_heap: true,
            uses_table: false,
        }),
        "build_fetch" => Some(WirHelperSpec {
            func: staged_string_helper("build_fetch", &["host", "path"], "build_fetch_len"),
            helper_deps: &["rc_alloc"],
            import_deps: &["build_fetch_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "build_exec" => Some(WirHelperSpec {
            func: staged_string_helper("build_exec", &["tool", "input"], "build_exec_run"),
            helper_deps: &["rc_alloc"],
            import_deps: &["build_exec_run", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "dir_list" => Some(WirHelperSpec {
            func: dir_list_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["dir_list_size", "write_pending_list"],
            uses_heap: true,
            uses_table: false,
        }),
        // (RFC-0020) `net.net.resolve(host) -> List(String)` — the resolved IP literals.
        // The two-phase staged-list protocol, identical shape to `dir_list`: the host
        // resolves the name NOW and reports the marshaled byte size (`net_resolve_size`),
        // then `write_pending_list` lays the `List(String)` into the reserved block.
        "net_resolve" => Some(WirHelperSpec {
            func: two_phase_helper_typed(
                "net_resolve",
                &[("h".into(), WirTy::Extern), ("host".into(), WirTy::Str)],
                "net_resolve_size",
                "write_pending_list",
            ),
            helper_deps: &["rc_alloc"],
            import_deps: &["net_resolve_size", "write_pending_list"],
            uses_heap: true,
            uses_table: false,
        }),
        "vm_par_map" => Some(WirHelperSpec {
            func: vm_par_map_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["vm_par_map_run", "vm_par_map_write"],
            uses_heap: true,
            uses_table: true,
        }),
        "vm_par_map_bytes" => Some(WirHelperSpec {
            func: vm_par_map_bytes_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["vm_par_map_bytes_run", "vm_par_map_bytes_write"],
            uses_heap: true,
            uses_table: true,
        }),
        "vm_with_dir" => Some(WirHelperSpec {
            func: vm_with_dir_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["vm_with_dir_run", "fill_pending"],
            uses_heap: true,
            uses_table: true,
        }),
        "vm_serve" => Some(WirHelperSpec {
            func: vm_serve_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["vm_serve_run", "vm_par_map_bytes_write"],
            uses_heap: true,
            uses_table: true,
        }),
        "get_env" => Some(WirHelperSpec {
            func: get_env_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &["env_len", "env_fill"],
            uses_heap: true,
            uses_table: false,
        }),
        "replace" => Some(WirHelperSpec {
            func: replace_helper(),
            helper_deps: &["rc_alloc", "match_at"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "str_to_int" => Some(WirHelperSpec {
            func: str_to_int_helper(),
            helper_deps: &["is_ws"],
            // (RFC-0045) routes its parse-failure aborts through `__witchy_abort`.
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "key_eq" => Some(WirHelperSpec {
            func: key_eq_helper(),
            helper_deps: &["str_eq"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_hash" => Some(WirHelperSpec {
            func: dict_hash_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_find" => Some(WirHelperSpec {
            func: dict_find_helper(),
            helper_deps: &["key_eq", "dict_hash"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_new" => Some(WirHelperSpec {
            func: dict_new_helper(),
            helper_deps: &["rc_alloc", "ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_insert" => Some(WirHelperSpec {
            func: dict_insert_helper(),
            helper_deps: &["rc_alloc", "dict_find"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_insert_extract" => Some(WirHelperSpec {
            func: dict_insert_extract_helper(),
            helper_deps: &[
                "rc_alloc",
                "rc_free",
                "dict_find",
                "dict_index_put",
                "dict_reindex",
                "leaf_dup",
                "leaf_drop",
                "slot_take_or_dup",
            ],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_get_or" => Some(WirHelperSpec {
            func: dict_get_or_helper(),
            helper_deps: &["dict_find"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_at" => Some(WirHelperSpec {
            func: dict_at_helper(),
            helper_deps: &["dict_find"],
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_update" => Some(WirHelperSpec {
            func: dict_update_helper(),
            helper_deps: &["dict_get_or", "dict_insert"],
            import_deps: &[],
            uses_heap: true,
            uses_table: true,
        }),
        "dict_insert_cap" => Some(WirHelperSpec {
            func: dict_insert_cap_helper(),
            helper_deps: &["rc_alloc", "dict_find", "bump_alloc", "dict_index_put"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_index_put" => Some(WirHelperSpec {
            func: dict_index_put_helper(),
            helper_deps: &["dict_hash"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_reindex" => Some(WirHelperSpec {
            func: dict_reindex_helper(),
            helper_deps: &["bump_alloc", "dict_index_put"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "str_append_cap" => Some(WirHelperSpec {
            func: str_append_cap_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_set_cap" => Some(WirHelperSpec {
            func: list_set_cap_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_pop_extract" => Some(WirHelperSpec {
            func: list_pop_extract_helper(),
            helper_deps: &["rc_alloc", "leaf_dup", "slot_take_or_dup"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_update_cap" => Some(WirHelperSpec {
            func: list_update_cap_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: true,
        }),
        "dict_update_cap" => Some(WirHelperSpec {
            func: dict_update_cap_helper(),
            helper_deps: &["dict_get_or", "dict_insert_cap"],
            import_deps: &[],
            uses_heap: true,
            uses_table: true,
        }),
        "dict_has" => Some(WirHelperSpec {
            func: dict_has_helper(),
            helper_deps: &["dict_find"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_keys" => Some(WirHelperSpec {
            func: dict_project_helper("dict_keys", 4),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_values" => Some(WirHelperSpec {
            func: dict_project_helper("dict_values", 12),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_pairs" => Some(WirHelperSpec {
            func: dict_pairs_helper(),
            helper_deps: &["rc_alloc"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_remove" => Some(WirHelperSpec {
            func: dict_remove_helper(),
            helper_deps: &["rc_alloc", "ensure", "key_eq"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_remove_extract" => Some(WirHelperSpec {
            func: dict_remove_extract_helper(),
            helper_deps: &[
                "rc_alloc",
                "dict_find",
                "dict_reindex",
                "leaf_dup",
                "leaf_drop",
                "slot_take_or_dup",
            ],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "float_to_int" => Some(WirHelperSpec {
            func: float_to_int_helper(),
            helper_deps: &[],
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "int_div" => Some(WirHelperSpec {
            func: int_div_helper(),
            helper_deps: &[],
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "int_rem" => Some(WirHelperSpec {
            func: int_rem_helper(),
            helper_deps: &[],
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "f_lt" => Some(WirHelperSpec {
            func: float_cmp_helper("f_lt", BinOp::Lt),
            helper_deps: &[],
            // (RFC-0045) routes its NaN-ordering abort through `__witchy_abort`.
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "f_le" => Some(WirHelperSpec {
            func: float_cmp_helper("f_le", BinOp::Le),
            helper_deps: &[],
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "f_gt" => Some(WirHelperSpec {
            func: float_cmp_helper("f_gt", BinOp::Gt),
            helper_deps: &[],
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "f_ge" => Some(WirHelperSpec {
            func: float_cmp_helper("f_ge", BinOp::Ge),
            helper_deps: &[],
            import_deps: &["__witchy_abort"],
            uses_heap: false,
            uses_table: false,
        }),
        "str_cmp" => Some(WirHelperSpec {
            func: str_cmp_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        _ => {
            // `$mk{n}`: the n-field aggregate allocators (each calls `$ensure`).
            // The WAT path emits one for any arity a tuple/record/closure needs, so
            // the registry must too — a 9-field record or a closure with 8+ captures
            // would otherwise reach an undeclared `$mk9`. The bound is a sanity cap
            // on parsing, far above any realistic aggregate.
            if let Some(rest) = name.strip_prefix("mk") {
                if let Ok(n) = rest.parse::<usize>() {
                    if n <= 256 {
                        // (RFC-0023) Opt-in checked codegen: each aggregate allocator
                        // emits a redzone + `heap_register` so an out-of-object overrun
                        // is caught by the post-run sweep. Off by default (zero cost).
                        let checked = heap_check_enabled();
                        let import_deps: &'static [&'static str] =
                            if checked { &["heap_register"] } else { &[] };
                        return Some(WirHelperSpec {
                            func: mk_helper(n, checked),
                            helper_deps: &["rc_alloc", "ensure"],
                            import_deps,
                            uses_heap: true,
                            uses_table: false,
                        });
                    }
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod intrinsic_catalog_tests {
    use super::*;

    #[test]
    fn cataloged_static_wir_helpers_exist() {
        for intrinsic in witchy_syntax::intrinsics::ALL {
            for helper in intrinsic.wir_helpers {
                assert!(
                    wir_helper(helper).is_some(),
                    "intrinsic {} names missing WIR helper {}",
                    intrinsic.name,
                    helper
                );
            }
        }
    }
}
