//! Shared spelling for compiler-internal capability-operation calls.
//!
//! Source-level capability ops are methods (`console.print("hi")`). During trait
//! lowering they become ordinary calls, but keeping a private marker on those
//! calls preserves the fact that the user wrote method syntax. RFC-0076's
//! bare-form rejection depends on that distinction.

pub const PREFIX: &str = "__capop.";

pub const OPS: &[&str] = &[
    "accept",
    "append",
    "close",
    "connect",
    "connect_pinned",
    "deny",
    "exec",
    "exists",
    "fetch_build",
    "get_build_env",
    "get_env",
    "is_dir",
    "list",
    "listen",
    "listen_tls",
    "make_dir",
    "now",
    "now_monotonic",
    "only",
    "print",
    "rand_u64",
    "read",
    "read_build",
    "read_file",
    "recv_all",
    "recv_bytes",
    "recv_line",
    "resolve",
    "run_tool",
    "send_bytes",
    "send_line",
    "serve_pool",
    "subtree",
    "try_connect",
    "try_connect_pinned",
    "write",
    "write_file",
    "write_out",
];

pub fn call_name(method: &str) -> String {
    format!("{PREFIX}{method}")
}

pub fn is_marked(name: &str) -> bool {
    name.starts_with(PREFIX)
}

pub fn is_op_name(name: &str) -> bool {
    OPS.contains(&surface_name(name))
}

pub fn surface_name(name: &str) -> &str {
    name.strip_prefix(PREFIX).unwrap_or(name)
}
