//! Shared spelling for compiler-internal capability-operation calls.
//!
//! Source-level capability ops are methods (`console.print("hi")`). During trait
//! lowering they become ordinary calls, but keeping a private marker on those
//! calls preserves the fact that the user wrote method syntax. RFC-0076's
//! bare-form rejection depends on that distinction.

use std::sync::OnceLock;

// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashSet, HashSetExt as _};

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

#[doc(hidden)]
pub const RECEIVER_TYPES: &[(ReceiverKind, &str)] = &[
    (ReceiverKind::Console, "Console"),
    (ReceiverKind::Clock, "Clock"),
    (ReceiverKind::Rand, "Rand"),
    (ReceiverKind::Env, "Env"),
    (ReceiverKind::Exec, "Exec"),
    (ReceiverKind::BuildOut, "BuildOut"),
    (ReceiverKind::BuildRead, "BuildRead"),
    (ReceiverKind::BuildEnv, "BuildEnv"),
    (ReceiverKind::BuildNet, "BuildNet"),
    (ReceiverKind::BuildExec, "BuildExec"),
    (ReceiverKind::File, "File"),
    (ReceiverKind::Dir, "Dir"),
    (ReceiverKind::Net, "Net"),
    (ReceiverKind::Fetch, "Fetch"),
    (ReceiverKind::Socket, "Socket"),
    (ReceiverKind::Listener, "Listener"),
];

impl ReceiverKind {
    /// Resolve the nominal Witchy capability type to its operation receiver.
    pub fn from_type_name(name: &str) -> Option<Self> {
        RECEIVER_TYPES
            .iter()
            .find_map(|(receiver, type_name)| (*type_name == name).then_some(*receiver))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentShape {
    String,
    Bytes,
    Int,
    Bool,
    Secret,
    ListString,
    Dir,
    DirPolicy,
    NetPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultShape {
    SameReceiver,
    Nil,
    Int,
    String,
    Bytes,
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
    /// Lowered operands after the receiver argument.
    pub arguments: &'static [ArgumentShape],
    pub result: ResultShape,
    /// Whether a run-time failure is a value: the operation returns
    /// `Result(result, CapError)` instead of trapping (RFC-0126 rule 3).
    pub fallible: bool,
    pub suggestion: &'static str,
}

/// The canonical name of the preluded error type every fallible capability
/// operation returns.
pub const CAP_ERROR_TYPE: &str = "caperror.CapError";

/// The `CapError` constructors, indexed by the host response status that
/// selects them (status 0 is success).
pub const CAP_ERROR_VARIANTS: [&str; 5] = [
    "caperror.Denied", "caperror.NotFound", "caperror.NotAFile", "caperror.AlreadyExists", "caperror.Io",
];

impl CapOp {
    pub fn total_arity(self) -> usize {
        self.arguments.len() + 1
    }
}

macro_rules! op {
    // `Shape?` marks an operation whose run-time failure is a `CapError`.
    (
        $name:literal,
        $receiver:ident,
        [$($argument:ident),* $(,)?],
        $result:ident ?,
        $suggestion:literal
    ) => {
        op!(@row $name, $receiver, [$($argument),*], $result, true, $suggestion)
    };
    (
        $name:literal,
        $receiver:ident,
        [$($argument:ident),* $(,)?],
        $result:ident,
        $suggestion:literal
    ) => {
        op!(@row $name, $receiver, [$($argument),*], $result, false, $suggestion)
    };
    (@row $name:literal, $receiver:ident, [$($argument:ident),*], $result:ident, $fallible:literal, $suggestion:literal) => {
        CapOp {
            name: $name,
            receiver: ReceiverKind::$receiver,
            arguments: &[$(ArgumentShape::$argument),*],
            result: ResultShape::$result,
            fallible: $fallible,
            suggestion: $suggestion,
        }
    };
}

pub const OPS: &[CapOp] = &[
    op!("print", Console, [String], Nil, "console.print(message)"),
    op!("read_line", Console, [], String, "console.read_line()"),
    op!("now", Clock, [], Int, "clock.now()"),
    op!("now_monotonic", Clock, [], Int, "clock.now_monotonic()"),
    op!("rand_u64", Rand, [], Int, "rand.rand_u64()"),
    op!("get_env", Env, [String], OptionString, "env.get_env(name)"),
    op!("only", Env, [ListString], SameReceiver, "env.only(names)"),
    op!(
        "exec",
        Exec,
        [Dir, String, String, String],
        String,
        "exec.exec(dir, path, args, stdin)"
    ),
    op!("only", Exec, [ListString], SameReceiver, "exec.only(programs)"),
    op!(
        "write_out",
        BuildOut,
        [String, String],
        Nil,
        "out.write_out(path, contents)"
    ),
    op!("read_build", BuildRead, [String], String, "build_read.read_build(path)"),
    op!(
        "get_build_env",
        BuildEnv,
        [String],
        OptionString,
        "build_env.get_build_env(name)"
    ),
    op!(
        "fetch_build",
        BuildNet,
        [String, String],
        String,
        "build_net.fetch_build(host, path)"
    ),
    op!(
        "run_tool",
        BuildExec,
        [String, String],
        String,
        "build_exec.run_tool(tool, input)"
    ),
    op!("read", File, [], String, "file.read()"),
    op!("write", File, [String], Nil, "file.write(data)"),
    op!("only", Dir, [DirPolicy], SameReceiver, "cap.only(policy)"),
    op!("list", Dir, [], ListString, "dir.list()"),
    op!("read", Dir, [String], String?, "dir.read(path)"),
    op!("exists", Dir, [String], Bool, "dir.exists(path)"),
    op!("is_dir", Dir, [String], Bool, "dir.is_dir(path)"),
    op!("subtree", Dir, [String], Dir, "dir.subtree(path)"),
    op!("make_dir", Dir, [String], Nil, "dir.make_dir(path)"),
    op!("read_file", Dir, [String], File, "dir.read_file(path)"),
    op!("write_file", Dir, [String], File, "dir.write_file(path)"),
    op!("write", Dir, [String, String], Nil, "dir.write(path, data)"),
    op!("append", Dir, [String, String], Nil, "dir.append(path, data)"),
    op!("create_new", Dir, [String, String], Bool, "dir.create_new(path, data)"),
    op!("replace", Dir, [String, String], Nil, "dir.replace(path, data)"),
    op!("rename", Dir, [String, String], Nil, "dir.rename(from, to)"),
    op!("read_bytes", Dir, [String], Bytes, "dir.read_bytes(path)"),
    op!("write_bytes", Dir, [String, Bytes], Nil, "dir.write_bytes(path, data)"),
    op!("append_bytes", Dir, [String, Bytes], Nil, "dir.append_bytes(path, data)"),
    op!("set_executable", Dir, [String], Nil, "dir.set_executable(path)"),
    op!("connect", Net, [String], Socket, "net.connect(addr)"),
    op!("try_connect", Net, [String], OptionSocket, "net.try_connect(addr)"),
    op!("listen", Net, [String], Listener, "net.listen(addr)"),
    op!(
        "listen_tls",
        Net,
        [String, String, Secret],
        Listener,
        "net.listen_tls(addr, cert_pem, key)"
    ),
    op!("only", Net, [NetPolicy], SameReceiver, "cap.only(policy)"),
    op!("fetch", Net, [String], Fetch, "net.fetch(origins)"),
    op!("only", Fetch, [String], SameReceiver, "fetch.only(origins)"),
    op!(
        "send_raw",
        Fetch,
        [String, String, String, String],
        String,
        "fetch.send_raw(method, url, headers, body)"
    ),
    op!("deny", Net, [NetPolicy], SameReceiver, "net.deny(policy)"),
    op!("resolve", Net, [String], ListString, "net.resolve(host)"),
    op!(
        "connect_pinned",
        Net,
        [String, String, Int, Bool],
        Socket,
        "net.connect_pinned(ip, host, port, secure)"
    ),
    op!(
        "try_connect_pinned",
        Net,
        [String, String, Int, Bool],
        OptionSocket,
        "net.try_connect_pinned(ip, host, port, secure)"
    ),
    op!("accept", Listener, [], Socket, "listener.accept()"),
    op!("serve_pool", Listener, [], Nil, "listener.serve_pool()"),
    op!("send_line", Socket, [String], Nil, "socket.send_line(line)"),
    op!("send_bytes", Socket, [String], Nil, "socket.send_bytes(bytes)"),
    op!("recv_line", Socket, [], String, "socket.recv_line()"),
    op!("recv_all", Socket, [], String, "socket.recv_all()"),
    op!("recv_bytes", Socket, [Int], String, "socket.recv_bytes(n)"),
    op!("close", Socket, [], Nil, "socket.close()"),
];

pub fn call_name(method: &str) -> String {
    format!("{PREFIX}{method}")
}

pub fn is_marked(name: &str) -> bool {
    name.starts_with(PREFIX)
}

fn op_names() -> &'static HashSet<&'static str> {
    static NAMES: OnceLock<HashSet<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut set = HashSet::with_capacity(OPS.len());
        set.extend(OPS.iter().map(|op| op.name));
        set
    })
}

pub fn is_op_name(name: &str) -> bool {
    let name = surface_name(name);
    op_names().contains(name)
}

pub(crate) fn op_info(name: &str, total_arity: usize) -> Option<&'static CapOp> {
    let name = surface_name(name);
    OPS.iter().find(|op| op.name == name && op.total_arity() == total_arity)
}

pub fn diagnostic_suggestion(name: &str, total_arity: usize) -> Option<&'static str> {
    op_info(name, total_arity)
        .or_else(|| {
            let name = surface_name(name);
            OPS.iter().find(|op| op.name == name)
        })
        .map(|op| op.suggestion)
}

pub fn result_shape(name: &str, total_arity: usize) -> Option<(ResultShape, bool)> {
    op_info(name, total_arity).map(|op| (op.result, op.fallible))
}

/// The unique operation named `name` on `receiver`.
///
/// Receiver-qualified lookup is the semantic authority for capability-op
/// membership and arity. It disambiguates shared method names such as
/// `read`, `write`, and `only` without downstream handwritten name tables.
pub fn operation(name: &str, receiver: ReceiverKind) -> Option<&'static CapOp> {
    let name = surface_name(name);
    OPS.iter()
        .find(|op| op.name == name && op.receiver == receiver)
}

/// Resolve a surface name only when it identifies one catalog row.
///
/// Overloaded names such as `read`, `write`, and `only` require receiver-aware
/// lookup through [`operation`].
pub fn unique_operation(name: &str) -> Option<&'static CapOp> {
    let name = surface_name(name);
    let mut matches = OPS.iter().filter(|operation| operation.name == name);
    let operation = matches.next()?;
    matches.next().is_none().then_some(operation)
}

pub fn receiver_supports(name: &str, receiver: ReceiverKind) -> bool {
    operation(name, receiver).is_some()
}

pub fn surface_name(name: &str) -> &str {
    name.strip_prefix(PREFIX).unwrap_or(name)
}
