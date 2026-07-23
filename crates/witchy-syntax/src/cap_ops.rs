//! Shared spelling for compiler-internal capability-operation calls.
//!
//! Source-level capability ops are methods (`console.print("hi")`). During trait
//! lowering they become ordinary calls, but keeping a private marker on those
//! calls preserves the fact that the user wrote method syntax. RFC-0076's
//! bare-form rejection depends on that distinction.

pub const PREFIX: &str = "__capop.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverKind {
    Console,
    Clock,
    Rand,
    Env,
    Exec,
    BuildOut,
    BuildRead,
    BuildEnv,
    BuildNet,
    BuildExec,
    File,
    Dir,
    Net,
    Fetch,
    Socket,
    Listener,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultShape {
    SameReceiver,
    Nil,
    Int,
    String,
    Bool,
    ListString,
    OptionString,
    Dir,
    File,
    Fetch,
    Socket,
    OptionSocket,
    Listener,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapOp {
    pub name: &'static str,
    pub receiver: ReceiverKind,
    /// Total lowered arity, including the receiver argument.
    pub total_arity: usize,
    pub result: ResultShape,
    pub suggestion: &'static str,
}

macro_rules! op {
    ($name:literal, $receiver:ident, $arity:literal, $result:ident, $suggestion:literal) => {
        CapOp {
            name: $name,
            receiver: ReceiverKind::$receiver,
            total_arity: $arity,
            result: ResultShape::$result,
            suggestion: $suggestion,
        }
    };
}

pub const OPS: &[CapOp] = &[
    op!("print", Console, 2, Nil, "console.print(message)"),
    op!("now", Clock, 1, Int, "clock.now()"),
    op!("now_monotonic", Clock, 1, Int, "clock.now_monotonic()"),
    op!("rand_u64", Rand, 1, Int, "rand.rand_u64()"),
    op!("get_env", Env, 2, OptionString, "env.get_env(name)"),
    op!("only", Env, 2, SameReceiver, "env.only(names)"),
    op!("exec", Exec, 5, String, "exec.exec(dir, path, args, stdin)"),
    op!("write_out", BuildOut, 3, Nil, "out.write_out(path, contents)"),
    op!("read_build", BuildRead, 2, String, "build_read.read_build(path)"),
    op!("get_build_env", BuildEnv, 2, OptionString, "build_env.get_build_env(name)"),
    op!("fetch_build", BuildNet, 3, String, "build_net.fetch_build(host, path)"),
    op!("run_tool", BuildExec, 3, String, "build_exec.run_tool(tool, input)"),
    op!("read", File, 1, String, "file.read()"),
    op!("write", File, 2, Nil, "file.write(data)"),
    op!("only", Dir, 2, SameReceiver, "cap.only(policy)"),
    op!("list", Dir, 1, ListString, "dir.list()"),
    op!("read", Dir, 2, String, "dir.read(path)"),
    op!("exists", Dir, 2, Bool, "dir.exists(path)"),
    op!("is_dir", Dir, 2, Bool, "dir.is_dir(path)"),
    op!("subtree", Dir, 2, Dir, "dir.subtree(path)"),
    op!("make_dir", Dir, 2, Nil, "dir.make_dir(path)"),
    op!("read_file", Dir, 2, File, "dir.read_file(path)"),
    op!("write_file", Dir, 2, File, "dir.write_file(path)"),
    op!("write", Dir, 3, Nil, "dir.write(path, data)"),
    op!("append", Dir, 3, Nil, "dir.append(path, data)"),
    op!("connect", Net, 2, Socket, "net.connect(addr)"),
    op!("try_connect", Net, 2, OptionSocket, "net.try_connect(addr)"),
    op!("listen", Net, 2, Listener, "net.listen(addr)"),
    op!("listen_tls", Net, 4, Listener, "net.listen_tls(addr, cert_pem, key)"),
    op!("only", Net, 2, SameReceiver, "cap.only(policy)"),
    op!("fetch", Net, 2, Fetch, "net.fetch(origins)"),
    op!("only", Fetch, 2, SameReceiver, "fetch.only(origins)"),
    op!(
        "send_raw",
        Fetch,
        5,
        String,
        "fetch.send_raw(method, url, headers, body)"
    ),
    op!("deny", Net, 2, SameReceiver, "net.deny(policy)"),
    op!("resolve", Net, 2, String, "net.resolve(host)"),
    op!("connect_pinned", Net, 5, Socket, "net.connect_pinned(ip, host, port, secure)"),
    op!(
        "try_connect_pinned",
        Net,
        5,
        OptionSocket,
        "net.try_connect_pinned(ip, host, port, secure)"
    ),
    op!("accept", Listener, 1, Socket, "listener.accept()"),
    op!("serve_pool", Listener, 1, Nil, "listener.serve_pool()"),
    op!("send_line", Socket, 2, Nil, "socket.send_line(line)"),
    op!("send_bytes", Socket, 2, Nil, "socket.send_bytes(bytes)"),
    op!("recv_line", Socket, 1, String, "socket.recv_line()"),
    op!("recv_all", Socket, 1, String, "socket.recv_all()"),
    op!("recv_bytes", Socket, 2, String, "socket.recv_bytes(n)"),
    op!("close", Socket, 1, Nil, "socket.close()"),
];

pub fn call_name(method: &str) -> String {
    format!("{PREFIX}{method}")
}

pub fn is_marked(name: &str) -> bool {
    name.starts_with(PREFIX)
}

pub fn is_op_name(name: &str) -> bool {
    let name = surface_name(name);
    OPS.iter().any(|op| op.name == name)
}

pub(crate) fn op_info(name: &str, total_arity: usize) -> Option<&'static CapOp> {
    let name = surface_name(name);
    OPS.iter().find(|op| op.name == name && op.total_arity == total_arity)
}

pub fn diagnostic_suggestion(name: &str, total_arity: usize) -> Option<&'static str> {
    op_info(name, total_arity)
        .or_else(|| {
            let name = surface_name(name);
            OPS.iter().find(|op| op.name == name)
        })
        .map(|op| op.suggestion)
}

pub fn result_shape(name: &str, total_arity: usize) -> Option<ResultShape> {
    op_info(name, total_arity).map(|op| op.result)
}

pub fn receiver_supports(name: &str, receiver: ReceiverKind) -> bool {
    let name = surface_name(name);
    OPS.iter().any(|op| op.name == name && op.receiver == receiver)
}

pub fn surface_name(name: &str) -> &str {
    name.strip_prefix(PREFIX).unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_disambiguated_overloads() {
        assert_eq!(op_info("read", 1).map(|op| op.receiver), Some(ReceiverKind::File));
        assert_eq!(op_info("read", 2).map(|op| op.receiver), Some(ReceiverKind::Dir));
        assert_eq!(op_info("write", 2).map(|op| op.receiver), Some(ReceiverKind::File));
        assert_eq!(op_info("write", 3).map(|op| op.receiver), Some(ReceiverKind::Dir));
    }

    #[test]
    fn marked_names_resolve_to_surface_catalog_rows() {
        assert!(is_op_name("__capop.listen_tls"));
        assert_eq!(
            result_shape("__capop.listen_tls", 4),
            Some(ResultShape::Listener)
        );
        assert_eq!(
            diagnostic_suggestion("__capop.fetch_build", 3),
            Some("build_net.fetch_build(host, path)")
        );
    }
}
