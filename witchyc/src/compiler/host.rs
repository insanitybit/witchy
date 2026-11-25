//! Host capabilities beyond the console and allocator (book: capabilities-authority.md):
//! a directory subtree and one file in it, each with one right (cookbook-files.md); the
//! wall clock; and randomness.
//!
//! A value of one of these types is a handle the host issued. Source cannot make one: the only
//! ways to get one are `main`'s parameters and the operations below, which take an existing
//! handle. Every operation is one host import of the type (i64, i64) -> (i64). Most pass one
//! number; the bulk operations (`raw_read`, `raw_write`, `raw_send`, `raw_recv`) pass a checked
//! `&[U8]` or `&mut [U8]` as `address | length << 32`, and the host reads or writes exactly
//! that range of the program's memory (memory 0, exported as `memory`; the allocator's is
//! never exported), checking it against the memory's size. They give back
//! `count | error << 32`, so a partial transfer says how far it got. The host checks each
//! handle's kind and right again on every call.
//!
//! Each family is one grant of the host: its authorizer, which hands `main` its value, and
//! its operations, imported together in this order when a program uses the family.

/// What a directory or file allows. `All` is the book's bare `Dir`: both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Right { Read, Write, All }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// `Secret(Read)` is `Secret<Reveal>` (its bytes can be read), `Secret(Write)` is
/// `Secret<Seal>` (it can be used by handle, to sign); `Secret(All)` is both.
/// `Grants` reads the values the run grants to `grantable capability` types: data, never
/// authority; no source names it (parser/grants.rs).
pub(super) enum Host { Dir(Right), File(Right), Clock, Rand, Env,
    /// The words the operator gave this launch after `--` (`main(args: Args)`): read-only.
    Args, Net, Socket, Listener,
    /// Connections waited on together; holding it is holding the sockets in it.
    Group,
    /// A computation that may pause, on its own stack (library/wasm-tasks.wy). No host
    /// function sees it: it is a block of the program's memory, and is never `main`'s.
    Task, Exec, Run, Secret(Right), SecretStore, Grants,
    /// `main(targets: Signer)`: a fixed ML-DSA-65 signing key for one `witchy-2` role, bound by
    /// name like a fixed secret. It signs messages that begin with its role's domain and gives
    /// its public key; it has no operation that reads the key, and no `Secret` narrows to it.
    Signer, TlsSigner, Mldsa87Signer, Sqlite, LocalService, P384Key }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Family { Files,
    /// An optional file-family extension. Its only operation requires a read handle.
    FileSnapshot, Clock, Rand, Env, Net, Exec, Secrets, Grants, Args, Sqlite, LocalService,
    /// Public computation on bytes the program passes (SHA-256, SHA-384, ML-DSA-65 verification): no
    /// authorizer and no handle, imported when an emitted function calls one, granted to every
    /// run. It confers nothing: the result is a function of the bytes given.
    Crypto }

impl Right {
    pub fn named(name: &str) -> Option<Self> { match name { "Read" => Some(Self::Read), "Write" => Some(Self::Write), _ => None } }
    pub fn reads(self) -> bool { self != Self::Write }
    pub fn writes(self) -> bool { self != Self::Read }
    /// How the host is told which right an open asks for; it checks the handle allows it.
    pub fn code(self) -> u64 { match self { Self::Read => 0, Self::Write => 1, Self::All => 2 } }
}

/// Whether a value of type `given` may be used where `wanted` is expected by dropping rights:
/// a full `Dir` for a `Dir<Read>` or `Dir<Write>`, by value or through a shared reference.
pub(super) fn narrows(given: super::logical_body::Ty, wanted: super::logical_body::Ty) -> bool {
    use super::logical_body::{Pointee, Ty};
    let narrower = |a: Host, b: Host| matches!((a, b), (Host::Dir(Right::All), Host::Dir(Right::Read | Right::Write)) | (Host::Secret(Right::All), Host::Secret(Right::Read | Right::Write)));
    match (given, wanted) {
        (Ty::Host(a), Ty::Host(b)) => narrower(a, b),
        (Ty::Borrow { mutable: false, pointee: Pointee::Host(a) }, Ty::Borrow { mutable: false, pointee: Pointee::Host(b) }) => narrower(a, b),
        _ => false,
    }
}

impl Host {
    /// `Dir<Read>`: the type as written, which is also its namespace of methods.
    pub fn name(self) -> &'static str {
        match self {
            Self::Dir(Right::Read) => "Dir<Read>", Self::Dir(Right::Write) => "Dir<Write>", Self::Dir(Right::All) => "Dir",
            Self::File(Right::Read) => "File<Read>", Self::File(Right::Write) => "File<Write>", Self::File(Right::All) => "File",
            Self::Clock => "Clock", Self::Rand => "Rand", Self::Env => "Env", Self::Args => "Args", Self::Net => "Net", Self::Socket => "Socket", Self::Listener => "Listener", Self::Group => "Group", Self::Task => "Task", Self::Exec => "Exec", Self::Run => "Run",
            Self::Secret(Right::Read) => "Secret<Reveal>", Self::Secret(Right::Write) => "Secret<Seal>", Self::Secret(Right::All) => "Secret", Self::SecretStore => "SecretStore", Self::Grants => "#Grants",
            Self::Signer => "Signer", Self::TlsSigner => "TlsSigner", Self::Mldsa87Signer => "Mldsa87Signer", Self::Sqlite => "Sqlite", Self::LocalService => "LocalService", Self::P384Key => "P384Key",
        }
    }
    pub fn named(name: &str, right: &str) -> Option<Self> {
        let right = Right::named(right)?;
        match name { "Dir" => Some(Self::Dir(right)), "File" => Some(Self::File(right)), _ => None }
    }
    /// `Secret<Reveal>` and `Secret<Seal>`.
    pub fn secret(right: &str) -> Option<Self> {
        match right { "Reveal" => Some(Self::Secret(Right::Read)), "Seal" => Some(Self::Secret(Right::Write)), _ => None }
    }
    /// The narrower types whose methods a type with both rights also has, in order.
    pub fn views(self) -> &'static [&'static str] {
        match self { Self::Dir(Right::All) => &["Dir<Read>", "Dir<Write>"], Self::Secret(Right::All) => &["Secret<Reveal>", "Secret<Seal>"], _ => &[] }
    }
    pub fn family(self) -> Family {
        match self { Self::Dir(_) | Self::File(_) => Family::Files, Self::Clock => Family::Clock, Self::Rand | Self::P384Key => Family::Rand, Self::Env => Family::Env, Self::Args => Family::Args, Self::Net | Self::Socket | Self::Listener | Self::Group => Family::Net,
            // Never asked: a task has no host functions (`imported`).
            Self::Task => Family::Grants, Self::Exec | Self::Run => Family::Exec, Self::Secret(_) | Self::SecretStore | Self::Signer | Self::TlsSigner | Self::Mldsa87Signer => Family::Secrets, Self::Grants => Family::Grants, Self::Sqlite => Family::Sqlite, Self::LocalService => Family::LocalService }
    }
    /// Whether the host issues values of this type, so holding one imports its family.
    pub fn imported(self) -> bool { self != Self::Task }
    /// The capabilities `main` may be given: the roots of each family. A single file is a root
    /// too (`--file`, `--write-file`): the least authority for a program that needs one file.
    /// A fixed secret is a root too (`main(signing: Secret<Seal>)`), with exactly one right:
    /// each such parameter is its own slot, bound by its name (`secret_slot`).
    pub fn root(self) -> bool { matches!(self, Self::Dir(_) | Self::File(Right::Read | Right::Write) | Self::Clock | Self::Rand | Self::Env | Self::Args | Self::Net | Self::Listener | Self::Socket | Self::Exec | Self::SecretStore | Self::Grants | Self::Secret(Right::Read | Right::Write) | Self::Signer | Self::TlsSigner | Self::Mldsa87Signer | Self::Sqlite | Self::LocalService) }
    /// The number the family's authorizer takes for this root: a directory's right, or 3 and 4
    /// for the granted file to read and the granted file to write; 1 for the granted listening
    /// address, which shares the network family with `Net`; 2 for the one connection an
    /// instance of a per-connection program is started for (`main(connection: Socket)`).
    pub fn code(self) -> u64 { match self { Self::Dir(right) => right.code(), Self::File(Right::Read) => 3, Self::File(Right::Write) => 4, Self::Listener => 1, Self::Socket => 2, _ => 0 } }
    /// A fixed slot's kind, for `main`'s parameters bound by name: 0 `Secret<Reveal>`, 1
    /// `Secret<Seal>`, 2 `Signer`. None: not a slot (a bare `Secret` or a store is never one).
    pub fn slot_kind(self) -> Option<u8> {
        match self { Self::Secret(Right::Read) => Some(0), Self::Secret(Right::Write) => Some(1), Self::Signer => Some(2), Self::TlsSigner => Some(3), Self::Mldsa87Signer => Some(4), _ => None }
    }
}

/// The number `secret_authorize` takes for `main`'s slot parameter at `index` (its place
/// among `main`'s `Secret<...>` and `Signer` parameters, the order of the module's
/// `witchy.secret-slots` section) of `kind` (`Host::slot_kind`): `16 * (index + 1) + kind`. 0
/// is the `SecretStore`. The host checks the index and kind against the section and its plan.
pub(super) fn secret_slot(index: usize, kind: u8) -> u64 { 16 * (index as u64 + 1) + u64::from(kind) }

impl Family {
    pub const ALL: [Self; 13] = [Self::Files, Self::FileSnapshot, Self::Clock, Self::Rand, Self::Env, Self::Net, Self::Exec, Self::Secrets, Self::Grants, Self::Args, Self::Sqlite, Self::LocalService, Self::Crypto];
    /// The family's authorizer import, which hands `main` its value; a public family has none.
    pub fn authorize(self) -> Option<&'static str> {
        Some(match self { Self::Files => "dir_authorize", Self::Clock => "clock_authorize", Self::Rand => "rand_authorize", Self::Env => "env_authorize", Self::Net => "net_authorize", Self::Exec => "exec_authorize", Self::Secrets => "secret_authorize", Self::Grants => "grant_authorize", Self::Args => "args_authorize", Self::Sqlite => "sqlite_authorize", Self::LocalService => "local_service_authorize", Self::Crypto | Self::FileSnapshot => return None })
    }
    /// How many imports the family has: its authorizer, if any, then one per operation.
    pub fn imports(self) -> u32 { u32::from(self.authorize().is_some()) + self.ops().len() as u32 }
    pub fn ops(self) -> &'static [HostOp] {
        use HostOp::*;
        match self {
            Self::Files => &[PathByte, OpenFile, OpenDir, MakeDir, Status, ReadByte, WriteByte, Close, Abandon,
                Probe, OpenList, Remove, OpenAppend, OpenNew, RenameFrom, Rename, ReadBulk, WriteBulk],
            Self::FileSnapshot => &[SnapshotRemaining],
            Self::Clock => &[ClockNow, ClockTimer, ClockTimerEnd],
            Self::Rand => &[RandNext, RandP384Agree, RandP384Start, RandP384Public, RandP384Finish, RandP384Close],
            Self::Env => &[EnvNameByte, EnvLookup, EnvValueByte],
            Self::Args => &[ArgsCount, ArgsSelect, ArgsByte],
            Self::Net => &[NetAddrByte, NetOnly, NetConnect, NetStatus, NetSendByte, NetRecvByte, NetClose, NetAccept, NetSend, NetRecv,
                NetGroup, GroupAdd, GroupTake, NetWait, NetTryRecv, NetTrySend, NetTryAccept, NetHandle, NetPoll, NetAcceptedOn],
            Self::Exec => &[ExecNameByte, ExecOnly, ExecArgByte, ExecArgEnd, ExecRun, ExecStatus, ExecOutByte, ExecCode,
                ExecInputByte, ExecDeadline, ExecOutputLimit, ExecErrByte, ExecCwd, ExecEnvNameByte, ExecEnvValueByte, ExecEnvEnd],
            Self::Secrets => &[SecretNameByte, SecretGet, SecretStatus, SecretByte, SecretMessageByte, SecretHmac,
                SecretDigestByte, SignerPublicKey, SignerSign, TlsSignerSign, SecretHmacSha384, Mldsa87SignerPublicKey, Mldsa87SignerSign],
            Self::Grants => &[GrantNameByte, GrantLookup],
            Self::Sqlite => &[SqliteRun],
            Self::LocalService => &[LocalExchange],
            Self::Crypto => &[Sha256, Sha384, MlDsa65Verify, Sha512, MlDsa87Verify,
                Aes128GcmSeal, Aes128GcmOpen, Aes256GcmSeal, Aes256GcmOpen, VerifyTag48, X509P384Verify,
                P256Verify, RsaPkcs1Sha256Verify],
        }
    }
    pub fn ordinal(self) -> usize { Self::ALL.iter().position(|f| *f == self).expect("listed") }
}

/// What an operation's number operand is: none (0), fixed by the receiver's type, or given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operand { None, Fixed(u64), Given(super::scalar::Scalar),
    /// A `&[U8]` (false) or `&mut [U8]` (true): the range the host reads or fills.
    Bytes(bool),
    /// A `&mut [U64]` the host reads and fills (a wait's entries).
    Words,
    /// A handle given away: the value is moved to the host (a socket into a group).
    Handle(Host),
    /// Several byte slices, each `&[U8]` (false) or `&mut [U8]` (true), in order: one number
    /// each (`address | length << 32`). The host checks every range before it reads or writes.
    Slices(&'static [bool]) }

/// What an operation gives back in source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Out { Nothing, Number, Handle(Host) }

/// The operations, each one import. The first operand is a handle; the second a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostOp {
    /// Add one byte to the directory's path being built. 0.
    PathByte,
    /// Open the file at the path built so far: for reading in a `Dir<Read>`, created or
    /// replaced in a `Dir<Write>`. Always a new handle; `Status` says whether it failed.
    OpenFile,
    /// The directory at the path built so far, with the same right.
    OpenDir,
    /// Make the directory at the path built so far. An error code, or 0.
    MakeDir,
    /// 0, or the error code an open failed with.
    Status,
    /// The next byte of a file being read, or 256 at its end.
    ReadByte,
    /// Add one byte to a file being written. An error code, or 0.
    WriteByte,
    /// Give the handle back. A file being written reaches the disk now, whole. An error code, or 0.
    Close,
    /// Give the handle back; a file being written is dropped, and nothing on the disk changes. 0.
    Abandon,
    /// What is at the path built so far: 0 nothing (or not a plain path), 1 a file, 2 a directory.
    Probe,
    /// The names in this directory, sorted, each followed by a newline (a directory's with `/`
    /// before it), as a file to read. Names that are not UTF-8 or hold a newline are left out.
    OpenList,
    /// Remove the file at the path built so far. An error code, or 0.
    Remove,
    /// The file at the path built so far, to be written starting from its present contents.
    OpenAppend,
    /// A new file at the path built so far: `finish` fails with "already exists" if something
    /// is there by then, so of two writers exactly one creates it.
    OpenNew,
    /// Keep the path built so far as what the next `Rename` moves, and start a new path. 0.
    RenameFrom,
    /// Move the file or directory kept by `RenameFrom` to the path built so far, both inside
    /// this directory; a file already there is replaced. An error code, or 0.
    Rename,
    /// Up to the slice's length from a file being read, into the slice: the count, 0 at the end.
    ReadBulk,
    /// Unread bytes in the Wasmtime host's admitted read snapshot.
    SnapshotRemaining,
    /// The slice added to a file being written: `count | error << 32`.
    WriteBulk,
    /// Milliseconds since the Unix epoch, by the host's wall clock.
    ClockNow,
    /// A timer due at the given wall-clock millisecond: a handle number a poll may name.
    ClockTimer,
    /// Gives a timer's number back.
    ClockTimerEnd,
    /// Add one byte to the `Type.field` name of a grant being looked up. 0.
    GrantNameByte,
    /// The value the run grants under the name built (a number; 1 or 0 for a Bool). A name
    /// the run does not grant stops the run. The name starts over.
    GrantLookup,
    /// 64 bits from the host's source of cryptographically secure randomness.
    RandNext,
    RandP384Agree,
    RandP384Start,
    RandP384Public,
    RandP384Finish,
    RandP384Close,
    /// Add one byte to the name of the variable being looked up. 0.
    EnvNameByte,
    /// Look the name up: the value's length in bytes, or 2^64 - 1 when the run grants no
    /// variable of that name (absent and ungranted look the same). The name starts over.
    EnvLookup,
    /// The byte at an index of the value last looked up, or 256 past its end.
    EnvValueByte,
    /// How many words the launch gave.
    ArgsCount,
    /// Choose the word at an index: its length in bytes, or 2^64 - 1 when there is none there.
    ArgsSelect,
    /// The byte at an index of the word last chosen, or 256 past its end.
    ArgsByte,
    /// Add one byte to the `host:port` being built. 0.
    NetAddrByte,
    /// A `Net` that reaches only the address built, if this one reaches it. Always a new
    /// handle; `NetStatus` says whether it failed.
    NetOnly,
    /// A TCP connection to the address built, if this `Net` reaches it.
    NetConnect,
    /// 0, or the error code a `NetOnly` or `NetConnect` failed with.
    NetStatus,
    /// Queue one byte to send. An error code, or 0.
    NetSendByte,
    /// Send what is queued, then the next byte received, 256 at the end, or 257 on an error.
    NetRecvByte,
    /// Send what is queued and give the handle back. An error code, or 0.
    NetClose,
    /// The next connection to the granted listening address, as a socket. Always a new handle;
    /// `NetStatus` says whether it failed (no one connected in time).
    NetAccept,
    /// Send the slice (after anything queued): as much as the connection takes,
    /// `count | error << 32`.
    NetSend,
    /// Receive up to the slice's length into it: the count, 0 at the end, or `error << 32`.
    NetRecv,
    /// A group of connections, from a listener (which it may wait on too) or a `Net`.
    NetGroup,
    /// Move a socket into the group: its slot.
    GroupAdd,
    /// The socket in a slot, given back.
    GroupTake,
    /// Wait until an entry (`slot << 8 | wanted`) is ready; marks each, gives how many.
    NetWait,
    /// What has been received, without waiting: `count | error << 32` (7: nothing yet).
    NetTryRecv,
    /// As much as the socket takes now, without waiting: `count | error << 32`.
    NetTrySend,
    /// A waiting connection, without waiting: a socket whose status may be 7, none yet.
    NetTryAccept,
    /// The number of a socket's or listener's handle, for a wait entry: a number names, and
    /// can never become, a handle.
    NetHandle,
    /// Wait until an entry (`handle << 8 | wanted`) is ready; marks each, gives how many.
    NetPoll,
    /// The granted listening address a socket was accepted on, written to the slice as
    /// `host:port` (an IPv6 literal in brackets): its length, or 0 for a socket the program
    /// dialled or a slice too short. From the run's grant, never from what the peer sent.
    NetAcceptedOn,
    /// Add one byte to the name of the program being chosen. 0.
    ExecNameByte,
    /// An `Exec` that may run only the program named, if this one may. Always a new handle;
    /// `ExecStatus` says whether it failed.
    ExecOnly,
    /// Add one byte to the argument being built. 0.
    ExecArgByte,
    /// The argument built is complete; the next starts empty. 0.
    ExecArgEnd,
    /// Run the program named with the arguments built, wait for it, and hold what it wrote to
    /// its standard output. Always a new handle; `ExecStatus` says whether it failed.
    ExecRun,
    /// 0, or the error code an `ExecOnly` or `ExecRun` failed with.
    ExecStatus,
    /// The next byte the program wrote, or 256 at the end.
    ExecOutByte,
    /// The program's exit code (0 to 255), or 256 when a signal ended it.
    ExecCode,
    /// Add one byte to what the next run's program reads on its standard input. 0.
    ExecInputByte,
    /// The next run's time limit in milliseconds: at most the run's own (a larger one fails the
    /// run with 7, 0 with 2). 0.
    ExecDeadline,
    /// The most the next run may hand back, standard output and error together: at most what
    /// the grant gives all programs (a larger one fails the run with 7). 0.
    ExecOutputLimit,
    /// The next byte the program wrote to its standard error, or 256 at the end.
    ExecErrByte,
    /// A granted directory, given to the next run as its working directory: the handle is moved
    /// to the host, which keeps the opened directory (never its path) and closes it after. 0.
    ExecCwd,
    /// One byte of the name of a variable the next run's program gets. 0.
    ExecEnvNameByte,
    /// One byte of that variable's value. 0.
    ExecEnvValueByte,
    /// The variable built is complete: a name the grant does not declare, or a malformed one,
    /// fails the next run (1 or 2). 0.
    ExecEnvEnd,
    /// Add one byte to the name of the secret being looked up. 0.
    SecretNameByte,
    /// The secret of the name built, if the run grants it: with both rights (operand 0, the
    /// run must grant reading) or for sealing only (1). Always a new handle; `SecretStatus`
    /// says whether it failed: 1 not granted, 2 granted for sealing only.
    SecretGet,
    /// 0, or the error code a `SecretGet` failed with.
    SecretStatus,
    /// The byte of the secret at an index, 256 past its end, or 257 when the run granted it
    /// for sealing only.
    SecretByte,
    /// Add one byte to the message to sign. 0.
    SecretMessageByte,
    /// Sign the message built (HMAC-SHA-256 with the secret as key), keep the 32-byte
    /// digest, and start a new message. 0.
    SecretHmac,
    SecretHmacSha384,
    /// The byte of the last digest at an index, or 256 past its end.
    SecretDigestByte,
    /// A signer's ML-DSA-65 public key (1952 bytes) into a slice of exactly that length. 0, or
    /// 2 when the slice is another length.
    SignerPublicKey,
    /// Sign a message (the first slice) with a signer's key, the signature (3309 bytes) into
    /// the second slice. 0; 1 the message does not begin with the signer's domain; 2 the
    /// signature slice is not 3309 bytes; 3 the message is too long; 4 signing failed.
    SignerSign,
    TlsSignerSign,
    Mldsa87SignerPublicKey,
    Mldsa87SignerSign,
    /// SHA-256 of the first slice into the second, which is 32 bytes. 0; 2 another length; 3
    /// the input is too long; 4 the provider failed.
    Sha256,
    /// SHA-384 of the first slice into the second, which is 48 bytes. 0; 2 another length; 3
    /// the input is too long; 4 the provider failed.
    Sha384,
    /// FIPS 204 ML-DSA-65 verification (empty context) of a signature (third slice) over a
    /// message (second) by a public key (first): 1 valid, 0 anything else.
    MlDsa65Verify,
    /// SHA-512 of the first slice into the 64-byte second slice.
    Sha512,
    /// Pure ML-DSA-87 verification with empty context.
    MlDsa87Verify,
    Aes128GcmSeal,
    Aes128GcmOpen,
    Aes256GcmSeal,
    Aes256GcmOpen,
    VerifyTag48,
    X509P384Verify,
    P256Verify,
    RsaPkcs1Sha256Verify,
    SqliteRun,
    LocalExchange,
}

impl HostOp {
    pub fn import(self) -> &'static str {
        match self {
            Self::PathByte => "fs_path_byte", Self::OpenFile => "fs_open_file", Self::OpenDir => "fs_open_dir",
            Self::MakeDir => "fs_make_dir", Self::Status => "fs_status", Self::ReadByte => "fs_read_u8",
            Self::WriteByte => "fs_write_u8", Self::Close => "fs_close", Self::Abandon => "fs_abandon",
            Self::Probe => "fs_probe", Self::OpenList => "fs_open_list", Self::Remove => "fs_remove",
            Self::OpenAppend => "fs_open_append", Self::OpenNew => "fs_open_new",
            Self::RenameFrom => "fs_rename_from", Self::Rename => "fs_rename", Self::ReadBulk => "fs_read", Self::WriteBulk => "fs_write",
            Self::SnapshotRemaining => "fs_snapshot_remaining",
            Self::ClockNow => "clock_now_ms", Self::ClockTimer => "clock_timer", Self::ClockTimerEnd => "clock_timer_end", Self::RandNext => "rand_u64", Self::GrantNameByte => "grant_name_byte", Self::GrantLookup => "grant_lookup",
            Self::RandP384Agree => "rand_p384_agree",
            Self::RandP384Start => "rand_p384_start",
            Self::RandP384Public => "rand_p384_public",
            Self::RandP384Finish => "rand_p384_finish",
            Self::RandP384Close => "rand_p384_close",
            Self::EnvNameByte => "env_name_byte", Self::EnvLookup => "env_lookup", Self::EnvValueByte => "env_value_byte",
            Self::ArgsCount => "args_count", Self::ArgsSelect => "args_select", Self::ArgsByte => "args_byte",
            Self::NetAddrByte => "net_addr_byte", Self::NetOnly => "net_only", Self::NetConnect => "net_connect", Self::NetStatus => "net_status",
            Self::NetSendByte => "net_send_u8", Self::NetRecvByte => "net_recv_u8", Self::NetClose => "net_close", Self::NetAccept => "net_accept",
            Self::NetSend => "net_send", Self::NetRecv => "net_recv",
            Self::NetGroup => "net_group", Self::GroupAdd => "net_group_add", Self::GroupTake => "net_group_take", Self::NetWait => "net_wait",
            Self::NetTryRecv => "net_try_recv", Self::NetTrySend => "net_try_send", Self::NetTryAccept => "net_try_accept",
            Self::NetHandle => "net_handle", Self::NetPoll => "net_poll", Self::NetAcceptedOn => "net_accepted_on",
            Self::ExecNameByte => "exec_name_byte", Self::ExecOnly => "exec_only", Self::ExecArgByte => "exec_arg_byte", Self::ExecArgEnd => "exec_arg_end",
            Self::ExecRun => "exec_run", Self::ExecStatus => "exec_status", Self::ExecOutByte => "exec_out_u8", Self::ExecCode => "exec_code",
            Self::ExecInputByte => "exec_input_u8", Self::ExecDeadline => "exec_deadline_ms", Self::ExecOutputLimit => "exec_output_limit", Self::ExecErrByte => "exec_err_u8",
            Self::ExecCwd => "exec_cwd", Self::ExecEnvNameByte => "exec_env_name_u8", Self::ExecEnvValueByte => "exec_env_value_u8", Self::ExecEnvEnd => "exec_env_end",
            Self::SecretNameByte => "secret_name_byte", Self::SecretGet => "secret_get", Self::SecretStatus => "secret_status", Self::SecretByte => "secret_byte",
            Self::SecretMessageByte => "secret_message_byte", Self::SecretHmac => "secret_hmac", Self::SecretDigestByte => "secret_digest_byte",
            Self::SecretHmacSha384 => "secret_hmac_sha384",
            Self::SignerPublicKey => "signer_public_key", Self::SignerSign => "signer_sign", Self::TlsSignerSign => "tls_signer_sign",
            Self::Mldsa87SignerPublicKey => "mldsa87_signer_public_key", Self::Mldsa87SignerSign => "mldsa87_signer_sign",
            Self::Aes128GcmSeal => "crypto_tls13_aes128gcm_seal",
            Self::Aes128GcmOpen => "crypto_tls13_aes128gcm_open",
            Self::Aes256GcmSeal => "crypto_tls13_aes256gcm_seal",
            Self::Aes256GcmOpen => "crypto_tls13_aes256gcm_open",
            Self::VerifyTag48 => "crypto_verify_tag48",
            Self::X509P384Verify => "crypto_x509_p384_verify",
            Self::P256Verify => "crypto_p256_verify",
            Self::RsaPkcs1Sha256Verify => "crypto_rsa_pkcs1_sha256_verify",
            Self::SqliteRun => "sqlite_run", Self::LocalExchange => "local_service_exchange",
            Self::Sha512 => "crypto_sha512", Self::MlDsa87Verify => "crypto_mldsa87_verify",
            Self::Sha256 => "crypto_sha256", Self::Sha384 => "crypto_sha384", Self::MlDsa65Verify => "crypto_mldsa65_verify",
        }
    }
    pub fn family(self) -> Family {
        match self {
            Self::SnapshotRemaining => Family::FileSnapshot,
            Self::ClockNow | Self::ClockTimer | Self::ClockTimerEnd => Family::Clock, Self::RandNext | Self::RandP384Agree | Self::RandP384Start | Self::RandP384Public | Self::RandP384Finish | Self::RandP384Close => Family::Rand, Self::GrantNameByte | Self::GrantLookup => Family::Grants,
            Self::EnvNameByte | Self::EnvLookup | Self::EnvValueByte => Family::Env,
            Self::ArgsCount | Self::ArgsSelect | Self::ArgsByte => Family::Args,
            Self::NetAddrByte | Self::NetOnly | Self::NetConnect | Self::NetStatus | Self::NetSendByte | Self::NetRecvByte | Self::NetClose | Self::NetAccept | Self::NetSend | Self::NetRecv
                | Self::NetGroup | Self::GroupAdd | Self::GroupTake | Self::NetWait | Self::NetTryRecv | Self::NetTrySend | Self::NetTryAccept
                | Self::NetHandle | Self::NetPoll | Self::NetAcceptedOn => Family::Net,
            Self::ExecNameByte | Self::ExecOnly | Self::ExecArgByte | Self::ExecArgEnd | Self::ExecRun | Self::ExecStatus | Self::ExecOutByte | Self::ExecCode
                | Self::ExecInputByte | Self::ExecDeadline | Self::ExecOutputLimit | Self::ExecErrByte
                | Self::ExecCwd | Self::ExecEnvNameByte | Self::ExecEnvValueByte | Self::ExecEnvEnd => Family::Exec,
            Self::SecretNameByte | Self::SecretGet | Self::SecretStatus | Self::SecretByte | Self::SecretMessageByte | Self::SecretHmac | Self::SecretDigestByte
                | Self::SignerPublicKey | Self::SignerSign | Self::TlsSignerSign | Self::SecretHmacSha384
                | Self::Mldsa87SignerPublicKey | Self::Mldsa87SignerSign => Family::Secrets,
            Self::SqliteRun => Family::Sqlite, Self::LocalExchange => Family::LocalService,
            Self::Sha256 | Self::Sha384 | Self::MlDsa65Verify | Self::Sha512 | Self::MlDsa87Verify
                | Self::Aes128GcmSeal | Self::Aes128GcmOpen | Self::Aes256GcmSeal | Self::Aes256GcmOpen
                | Self::VerifyTag48 | Self::X509P384Verify | Self::P256Verify | Self::RsaPkcs1Sha256Verify => Family::Crypto,
            _ => Family::Files,
        }
    }
    /// The position of the operation's import after its family's authorizer.
    pub fn ordinal(self) -> u32 { self.family().ops().iter().position(|op| *op == self).expect("listed") as u32 }
    /// Its import's index from the family's first import: past the authorizer, if any.
    pub fn offset(self) -> u32 { u32::from(self.family().authorize().is_some()) + self.ordinal() }
    /// Host operations take i64 parameters and return one i64.
    pub fn arity(self) -> usize {
        match self {
            Self::SignerSign | Self::TlsSignerSign | Self::MlDsa65Verify | Self::MlDsa87Verify
                | Self::RandP384Finish | Self::P256Verify | Self::SecretHmacSha384 | Self::Mldsa87SignerSign | Self::LocalExchange => 3,
            Self::RandP384Agree | Self::RsaPkcs1Sha256Verify => 4,
            Self::SqliteRun | Self::Aes128GcmSeal | Self::Aes128GcmOpen | Self::Aes256GcmSeal | Self::Aes256GcmOpen | Self::X509P384Verify => 5,
            _ => 2,
        }
    }
    /// A public operation, `provider.raw_sha256(data, digest)`: no receiver, so no authority; the
    /// slices it takes, in order.
    pub fn public(name: &str) -> Option<(Self, &'static [bool])> {
        Some(match name {
            "raw_sha256" => (Self::Sha256, &[false, true]),
            "raw_sha384" => (Self::Sha384, &[false, true]),
            "raw_mldsa65_verify" => (Self::MlDsa65Verify, &[false, false, false]),
            "raw_tls13_aes128gcm_seal" => (Self::Aes128GcmSeal, &[false, false, false, false, true]),
            "raw_tls13_aes128gcm_open" => (Self::Aes128GcmOpen, &[false, false, false, false, true]),
            "raw_tls13_aes256gcm_seal" => (Self::Aes256GcmSeal, &[false, false, false, false, true]),
            "raw_tls13_aes256gcm_open" => (Self::Aes256GcmOpen, &[false, false, false, false, true]),
            "raw_verify_tag48" => (Self::VerifyTag48, &[false, false]),
            "raw_x509_p384_verify" => (Self::X509P384Verify, &[false, false, false, false, false]),
            "raw_p256_verify" => (Self::P256Verify, &[false, false, false]),
            "raw_rsa_pkcs1_sha256_verify" => (Self::RsaPkcs1Sha256Verify, &[false, false, false, false]),
            "raw_sha512" => (Self::Sha512, &[false, true]),
            "raw_mldsa87_verify" => (Self::MlDsa87Verify, &[false, false, false]),
            _ => return None,
        })
    }
    /// The source method `receiver.raw_*` for this operation on a receiver of type `on`, its
    /// number operand, and its result. None: not available there, which is how a right limits
    /// what a handle can do. An open says which right it asks for, and the host checks the
    /// handle allows it, so the host enforces the same right the type does.
    pub fn method(name: &str, on: Host) -> Option<(Self, Operand, Out)> {
        use super::scalar::Scalar;
        let files = on.family() == Family::Files;
        let (reads, writes) = match on { Host::Dir(right) => (right.reads(), right.writes()), _ => (false, false) };
        Some(match (name, on) {
            ("raw_path_byte", Host::Dir(_)) => (Self::PathByte, Operand::Given(Scalar::U8), Out::Nothing),
            // A file has one right: a full `Dir` opens through its `Dir<Read>` or `Dir<Write>` view.
            ("raw_open_file", Host::Dir(right @ (Right::Read | Right::Write))) => (Self::OpenFile, Operand::Fixed(right.code()), Out::Handle(Host::File(right))),
            ("raw_open_dir", Host::Dir(right)) => (Self::OpenDir, Operand::Fixed(right.code()), Out::Handle(Host::Dir(right))),
            ("raw_make_dir", Host::Dir(_)) if writes => (Self::MakeDir, Operand::None, Out::Number),
            ("raw_status", _) if files => (Self::Status, Operand::None, Out::Number),
            ("raw_read_byte", Host::File(Right::Read)) => (Self::ReadByte, Operand::None, Out::Number),
            ("raw_write_byte", Host::File(Right::Write)) => (Self::WriteByte, Operand::Given(Scalar::U8), Out::Number),
            ("raw_snapshot_remaining", Host::File(Right::Read)) => (Self::SnapshotRemaining, Operand::None, Out::Number),
            ("raw_read", Host::File(Right::Read)) => (Self::ReadBulk, Operand::Bytes(true), Out::Number),
            ("raw_write", Host::File(Right::Write)) => (Self::WriteBulk, Operand::Bytes(false), Out::Number),
            ("raw_close", _) if files => (Self::Close, Operand::None, Out::Number),
            ("raw_abandon", _) if files => (Self::Abandon, Operand::None, Out::Nothing),
            ("raw_probe", Host::Dir(_)) if reads => (Self::Probe, Operand::None, Out::Number),
            ("raw_open_list", Host::Dir(_)) if reads => (Self::OpenList, Operand::None, Out::Handle(Host::File(Right::Read))),
            ("raw_remove", Host::Dir(_)) if writes => (Self::Remove, Operand::None, Out::Number),
            ("raw_open_append", Host::Dir(_)) if writes => (Self::OpenAppend, Operand::None, Out::Handle(Host::File(Right::Write))),
            ("raw_open_new", Host::Dir(_)) if writes => (Self::OpenNew, Operand::None, Out::Handle(Host::File(Right::Write))),
            ("raw_rename_from", Host::Dir(_)) if writes => (Self::RenameFrom, Operand::None, Out::Nothing),
            ("raw_rename", Host::Dir(_)) if writes => (Self::Rename, Operand::None, Out::Number),
            ("now_ms", Host::Clock) => (Self::ClockNow, Operand::None, Out::Number),
            ("raw_timer", Host::Clock) => (Self::ClockTimer, Operand::Given(Scalar::U64), Out::Number),
            ("raw_timer_end", Host::Clock) => (Self::ClockTimerEnd, Operand::Given(Scalar::U64), Out::Nothing),
            ("raw_name_byte", Host::Grants) => (Self::GrantNameByte, Operand::Given(Scalar::U8), Out::Nothing),
            ("raw_lookup", Host::Grants) => (Self::GrantLookup, Operand::None, Out::Number),
            ("next_u64", Host::Rand) => (Self::RandNext, Operand::None, Out::Number),
            ("raw_p384_agree", Host::Rand) => (Self::RandP384Agree, Operand::Slices(&[false, true, true]), Out::Number),
            ("raw_p384_start", Host::Rand) => (Self::RandP384Start, Operand::None, Out::Handle(Host::P384Key)),
            ("raw_p384_public", Host::P384Key) => (Self::RandP384Public, Operand::Bytes(true), Out::Number),
            ("raw_p384_finish", Host::P384Key) => (Self::RandP384Finish, Operand::Slices(&[false, true]), Out::Number),
            ("raw_close", Host::P384Key) => (Self::RandP384Close, Operand::None, Out::Nothing),
            ("raw_name_byte", Host::Env) => (Self::EnvNameByte, Operand::Given(Scalar::U8), Out::Nothing),
            ("raw_lookup", Host::Env) => (Self::EnvLookup, Operand::None, Out::Number),
            ("raw_value_byte", Host::Env) => (Self::EnvValueByte, Operand::Given(Scalar::U64), Out::Number),
            ("raw_count", Host::Args) => (Self::ArgsCount, Operand::None, Out::Number),
            ("raw_select", Host::Args) => (Self::ArgsSelect, Operand::Given(Scalar::U64), Out::Number),
            ("raw_byte", Host::Args) => (Self::ArgsByte, Operand::Given(Scalar::U64), Out::Number),
            ("raw_addr_byte", Host::Net) => (Self::NetAddrByte, Operand::Given(Scalar::U8), Out::Nothing),
            ("raw_only", Host::Net) => (Self::NetOnly, Operand::None, Out::Handle(Host::Net)),
            ("raw_connect", Host::Net) => (Self::NetConnect, Operand::None, Out::Handle(Host::Socket)),
            ("raw_accept", Host::Listener) => (Self::NetAccept, Operand::None, Out::Handle(Host::Socket)),
            ("raw_name_byte", Host::Exec) => (Self::ExecNameByte, Operand::Given(Scalar::U8), Out::Nothing),
            ("raw_only", Host::Exec) => (Self::ExecOnly, Operand::None, Out::Handle(Host::Exec)),
            ("raw_arg_byte", Host::Exec) => (Self::ExecArgByte, Operand::Given(Scalar::U8), Out::Nothing),
            ("raw_arg_end", Host::Exec) => (Self::ExecArgEnd, Operand::None, Out::Nothing),
            ("raw_run", Host::Exec) => (Self::ExecRun, Operand::None, Out::Handle(Host::Run)),
            ("raw_status", Host::Exec | Host::Run) => (Self::ExecStatus, Operand::None, Out::Number),
            ("raw_out_byte", Host::Run) => (Self::ExecOutByte, Operand::None, Out::Number),
            ("raw_code", Host::Run) => (Self::ExecCode, Operand::None, Out::Number),
            ("raw_input_byte", Host::Exec) => (Self::ExecInputByte, Operand::Given(Scalar::U8), Out::Nothing),
            ("raw_deadline_ms", Host::Exec) => (Self::ExecDeadline, Operand::Given(Scalar::U64), Out::Nothing),
            ("raw_output_limit", Host::Exec) => (Self::ExecOutputLimit, Operand::Given(Scalar::U64), Out::Nothing),
            ("raw_err_byte", Host::Run) => (Self::ExecErrByte, Operand::None, Out::Number),
            // A directory to run in, by right (a full `Dir` is given as either).
            ("raw_cwd", Host::Exec) => (Self::ExecCwd, Operand::Handle(Host::Dir(Right::Read)), Out::Nothing),
            ("raw_cwd_write", Host::Exec) => (Self::ExecCwd, Operand::Handle(Host::Dir(Right::Write)), Out::Nothing),
            ("raw_env_name_byte", Host::Exec) => (Self::ExecEnvNameByte, Operand::Given(Scalar::U8), Out::Nothing),
            ("raw_env_value_byte", Host::Exec) => (Self::ExecEnvValueByte, Operand::Given(Scalar::U8), Out::Nothing),
            ("raw_env_end", Host::Exec) => (Self::ExecEnvEnd, Operand::None, Out::Nothing),
            ("raw_name_byte", Host::SecretStore) => (Self::SecretNameByte, Operand::Given(Scalar::U8), Out::Nothing),
            // The full secret, which the run must have granted for reading; or its sealing view,
            // whichever way it was granted. The host is told which and checks it.
            ("raw_get", Host::SecretStore) => (Self::SecretGet, Operand::Fixed(0), Out::Handle(Host::Secret(Right::All))),
            ("raw_get_sealed", Host::SecretStore) => (Self::SecretGet, Operand::Fixed(1), Out::Handle(Host::Secret(Right::Write))),
            ("raw_status", Host::SecretStore | Host::Secret(_)) => (Self::SecretStatus, Operand::None, Out::Number),
            // Reading the bytes is `Reveal`'s alone; signing needs only `Seal`.
            ("raw_byte", Host::Secret(Right::Read | Right::All)) => (Self::SecretByte, Operand::Given(Scalar::U64), Out::Number),
            ("raw_message_byte", Host::Secret(Right::Write | Right::All)) => (Self::SecretMessageByte, Operand::Given(Scalar::U8), Out::Nothing),
            ("raw_hmac", Host::Secret(Right::Write | Right::All)) => (Self::SecretHmac, Operand::None, Out::Number),
            ("raw_hmac_sha384", Host::Secret(Right::Write | Right::All)) => (Self::SecretHmacSha384, Operand::Slices(&[false, true]), Out::Number),
            ("raw_digest_byte", Host::Secret(Right::Write | Right::All)) => (Self::SecretDigestByte, Operand::Given(Scalar::U64), Out::Number),
            // A signer gives its public key and signs; nothing reads its key.
            ("raw_exchange", Host::LocalService) => (Self::LocalExchange, Operand::Slices(&[false, true]), Out::Number),
            ("raw_transact", Host::Sqlite) => (Self::SqliteRun, Operand::Slices(&[false, false, false, true]), Out::Number),
            ("raw_public_key", Host::Signer) => (Self::SignerPublicKey, Operand::Bytes(true), Out::Number),
            ("raw_sign", Host::Signer) => (Self::SignerSign, Operand::Slices(&[false, true]), Out::Number),
            ("raw_public_key", Host::Mldsa87Signer) => (Self::Mldsa87SignerPublicKey, Operand::Bytes(true), Out::Number),
            ("raw_sign", Host::Mldsa87Signer) => (Self::Mldsa87SignerSign, Operand::Slices(&[false, true]), Out::Number),
            ("raw_certificate_verify", Host::TlsSigner) => (Self::TlsSignerSign, Operand::Slices(&[false, true]), Out::Number),
            ("raw_status", Host::Net | Host::Socket) => (Self::NetStatus, Operand::None, Out::Number),
            ("raw_send_byte", Host::Socket) => (Self::NetSendByte, Operand::Given(Scalar::U8), Out::Number),
            ("raw_recv_byte", Host::Socket) => (Self::NetRecvByte, Operand::None, Out::Number),
            ("raw_send", Host::Socket) => (Self::NetSend, Operand::Bytes(false), Out::Number),
            ("raw_group", Host::Listener | Host::Net) => (Self::NetGroup, Operand::None, Out::Handle(Host::Group)),
            ("raw_add", Host::Group) => (Self::GroupAdd, Operand::Handle(Host::Socket), Out::Number),
            ("raw_take", Host::Group) => (Self::GroupTake, Operand::Given(Scalar::U64), Out::Handle(Host::Socket)),
            ("raw_wait", Host::Group) => (Self::NetWait, Operand::Words, Out::Number),
            ("raw_try_recv", Host::Socket) => (Self::NetTryRecv, Operand::Bytes(true), Out::Number),
            ("raw_try_send", Host::Socket) => (Self::NetTrySend, Operand::Bytes(false), Out::Number),
            ("raw_try_accept", Host::Listener) => (Self::NetTryAccept, Operand::None, Out::Handle(Host::Socket)),
            ("raw_handle", Host::Socket | Host::Listener) => (Self::NetHandle, Operand::None, Out::Number),
            ("raw_poll", Host::Listener | Host::Net) => (Self::NetPoll, Operand::Words, Out::Number),
            ("raw_recv", Host::Socket) => (Self::NetRecv, Operand::Bytes(true), Out::Number),
            ("raw_accepted_on", Host::Socket) => (Self::NetAcceptedOn, Operand::Bytes(true), Out::Number),
            ("raw_close", Host::Net | Host::Socket | Host::Group) => (Self::NetClose, Operand::None, Out::Number),
            _ => return None,
        })
    }
    /// Whether the operation takes its receiver by value: the handle is given back to the host.
    pub fn consumes(self) -> bool { matches!(self, Self::Close | Self::Abandon | Self::NetClose | Self::RandP384Finish | Self::RandP384Close) }
}
