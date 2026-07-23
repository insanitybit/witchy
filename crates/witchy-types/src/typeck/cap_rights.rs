//! Capability rights-set parsing and formatting for the `Dir`/`File`/`Net`
//! capability family. Each capability is decomposed by right so the footprint
//! distinguishes (e.g.) read-only from writing code, and an op that needs a
//! right it wasn't granted is a compile-time error (RFC-0012/0073).

use std::fmt;

use witchy_syntax::ast;
use witchy_cap_model::{CapabilityKind, CapabilityRight};

use super::{terr, TypeError};

/// The operations a `Dir` capability permits. Decomposing the capability by
/// right makes the footprint distinguish read-only from writing code, and an op
/// that needs a right it wasn't granted is a compile-time error. Bare `Dir` is
/// the full set; `Dir[Read]`/`Dir[Write]` narrow it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirRights {
    pub read: bool,
    pub write: bool,
}

impl DirRights {
    pub fn full() -> Self {
        DirRights { read: true, write: true }
    }
}

impl fmt::Display for DirRights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.read, self.write) {
            (true, true) => write!(f, "Dir"),
            (true, false) => write!(f, "Dir[Read]"),
            (false, true) => write!(f, "Dir[Write]"),
            (false, false) => write!(f, "Dir[]"),
        }
    }
}

/// Parse the `[Read, Write]` rights arguments shared by the read/write
/// capability family (`Dir`, `File`) — one parser, so a new right is added in
/// exactly one place (RFC-0073). `None` means "no args": the full set, by the
/// family's bare-name convention.
fn read_write_args(kind: CapabilityKind, args: &[ast::Type]) -> Option<(bool, bool)> {
    if args.is_empty() {
        return None;
    }
    let (mut read, mut write) = (false, false);
    for a in args {
        if let ast::Type::Named(n, _) = a {
            match kind.right(n) {
                Some(CapabilityRight::Read) => read = true,
                Some(CapabilityRight::Write) => write = true,
                _ => {}
            }
        }
    }
    Some((read, write))
}

/// Interpret a `Dir`'s type arguments as its rights. Bare `Dir` (no args) is the
/// full set; `Dir[Read]`/`Dir[Write]`/`Dir[Read, Write]` narrow it.
pub(super) fn dir_rights(args: &[ast::Type]) -> DirRights {
    match read_write_args(CapabilityKind::Dir, args) {
        None => DirRights::full(),
        Some((read, write)) => DirRights { read, write },
    }
}

/// The operations a `File` capability permits — the *leaf* of the same hierarchy
/// as `Dir` (authority to one file vs. one subtree, RFC-0012). Mirrors `DirRights`:
/// a `File` carries no path-scope to refine (it is already a leaf), so its only
/// refinement axis is its rights. (`Exec` — folding ambient `Exec` into
/// `File[Exec]` — is a later addition; today a `File` is read/write.)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileRights {
    pub read: bool,
    pub write: bool,
}

impl FileRights {
    pub fn full() -> Self {
        FileRights { read: true, write: true }
    }
}

impl fmt::Display for FileRights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.read, self.write) {
            (true, true) => write!(f, "File"),
            (true, false) => write!(f, "File[Read]"),
            (false, true) => write!(f, "File[Write]"),
            (false, false) => write!(f, "File[]"),
        }
    }
}

/// Interpret a `File`'s type arguments as its rights (bare `File` is the full set).
pub(super) fn file_rights(args: &[ast::Type]) -> FileRights {
    match read_write_args(CapabilityKind::File, args) {
        None => FileRights::full(),
        Some((read, write)) => FileRights { read, write },
    }
}

/// The rights a `Net` capability permits, on two independent axes. **Verbs**:
/// `Connect` lets code dial out (`connect`, `restrict`); `Listen` lets it accept
/// inbound (`listen`) — distinguishing a client from a server. **Transports**:
/// `Tcp`/`Udp`/`Uds` — though only TCP is implemented at runtime, so `connect`/
/// `listen` require `Tcp`; `Udp`/`Uds` are type-level markers that keep the
/// taxonomy expressible (and auditable) even though the transport isn't.
///
/// Each axis defaults independently: an unmentioned axis is *full*. Bare `Net` is
/// full verbs + full transports; `Net[Connect]` is connect-only over all
/// transports; `Net[Tcp]` is all verbs over TCP only; `Net[Connect, Tcp]` is both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetRights {
    pub connect: bool,
    pub listen: bool,
    pub tcp: bool,
    pub udp: bool,
    pub uds: bool,
}

impl NetRights {
    pub fn full() -> Self {
        NetRights { connect: true, listen: true, tcp: true, udp: true, uds: true }
    }

    pub(super) fn verbs_full(&self) -> bool {
        self.connect && self.listen
    }

    pub(super) fn transports_full(&self) -> bool {
        self.tcp && self.udp && self.uds
    }
}

impl fmt::Display for NetRights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.verbs_full() && self.transports_full() {
            return write!(f, "Net");
        }
        // List only the narrowed axes; an axis at its full set is omitted (so
        // `Net[Connect]` reads as "connect-only, any transport").
        let mut parts: Vec<&str> = Vec::new();
        if !self.verbs_full() {
            if self.connect {
                parts.push("Connect");
            }
            if self.listen {
                parts.push("Listen");
            }
        }
        if !self.transports_full() {
            if self.tcp {
                parts.push("Tcp");
            }
            if self.udp {
                parts.push("Udp");
            }
            if self.uds {
                parts.push("Uds");
            }
        }
        write!(f, "Net[{}]", parts.join(", "))
    }
}

/// Interpret a `Net`'s type arguments as its rights. Bare `Net` (no args) is the
/// full set. Each axis defaults to full independently: `Net[Connect]` keeps all
/// transports, `Net[Tcp]` keeps all verbs. Unrecognized markers are ignored.
pub(super) fn net_rights(args: &[ast::Type]) -> NetRights {
    if args.is_empty() {
        return NetRights::full();
    }
    let mut r = NetRights { connect: false, listen: false, tcp: false, udp: false, uds: false };
    let (mut saw_verb, mut saw_transport) = (false, false);
    for a in args {
        if let ast::Type::Named(n, _) = a {
            match CapabilityKind::Net.right(n) {
                Some(CapabilityRight::Connect) => (r.connect, saw_verb) = (true, true),
                Some(CapabilityRight::Listen) => (r.listen, saw_verb) = (true, true),
                Some(CapabilityRight::Tcp) => (r.tcp, saw_transport) = (true, true),
                Some(CapabilityRight::Udp) => (r.udp, saw_transport) = (true, true),
                Some(CapabilityRight::Uds) => (r.uds, saw_transport) = (true, true),
                _ => {}
            }
        }
    }
    if !saw_verb {
        (r.connect, r.listen) = (true, true);
    }
    if !saw_transport {
        (r.tcp, r.udp, r.uds) = (true, true, true);
    }
    r
}

/// The rights markers each capability kind admits inside `[...]`. A marker
/// outside this vocabulary is a typo (`Dir[Reed]`) or a rejected right
/// (`Net[Tls]`), and is rejected at check time rather than silently dropped —
/// keeping the declared authority shape faithful to the source (BUG-154). The
/// single source of truth the rights-interpreting functions
/// (`dir_rights`/`file_rights`/`net_rights`) match against.
/// Reject any bracket marker on a `Dir`/`File`/`Net` capability that is not in its
/// catalog vocabulary. An empty list (`Dir[]`) is legal (no rights); each
/// marker must be a bare, argument-less name from the allowed set.
pub(super) fn validate_cap_markers(cap: &str, args: &[ast::Type]) -> Result<(), TypeError> {
    let kind = CapabilityKind::from_name(cap);
    let allowed = kind.map(CapabilityKind::rights).unwrap_or(&[]);
    for a in args {
        let ok = matches!(a, ast::Type::Named(m, margs)
            if margs.is_empty() && kind.is_some_and(|kind| kind.right(m).is_some()));
        if !ok {
            let found = match a {
                ast::Type::Named(m, _) => m.clone(),
                _ => format!("{a:?}"),
            };
            let allowed = allowed.iter().map(|right| right.name()).collect::<Vec<_>>();
            return terr(format!(
                "unknown `{cap}` right `{found}` — `{cap}` admits {}",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}
