//! Capability footprint analysis — the auditable core of witchy's supply-chain
//! story.
//!
//! Witchy's host capabilities (`Console`, `Dir`, `Net`) are unforgeable: no
//! expression can construct one, and there is no ambient authority. A capability
//! can only enter code as a parameter. Therefore a function's authority is
//! exactly its capability-typed parameters, and a module's footprint is the
//! union over its entry points (public functions and `main`). Unlike Go — where
//! any dependency runs with your
//! full ambient authority — this makes "what can this code touch?" statically
//! computable, so a dependency that *widens* its footprint (suddenly asks for
//! `Net`, or asks for a `Net` it can now *listen* on) is visible and gateable.
//!
//! The footprint is right-precise: a capability carries the *verbs* it permits
//! (`Dir[Read]`, `Net[Connect]`), so the audit distinguishes a read-only loader
//! from one that writes files, or a client from a server. Bare `Dir`/`Net` carry
//! the full right-set.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use witchy_syntax::ast::{Item, Module, Type};

/// The host capabilities the runtime grants at an entry point.
pub const HOST_CAPABILITIES: &[&str] =
    &["Console", "Clock", "Env", "Secret", "SecretStore", "Dir", "File", "Net", "Exec"];

/// The build-time capabilities a rune's `build` entrypoint may demand — the
/// parallel set to the runtime host caps, tracked on a separate axis. Kind-only
/// (the specific tool/dir/host/var is the consumer's grant, not the type), so
/// they carry no rights. See rfcs/build-time-execution-plan.md.
pub const BUILD_CAPABILITIES: &[&str] =
    &["BuildOut", "BuildRead", "BuildEnv", "BuildNet", "BuildExec"];

/// The rights (verbs) a single capability permits. Empty for `Console`, which
/// has no sub-verbs.
pub type Rights = BTreeSet<&'static str>;

/// A capability footprint: each present capability mapped to the union of rights
/// demanded for it. `Dir` ⇒ `{Read, Write}`, `Net` ⇒ `{Connect, Listen}`.
pub type CapSet = BTreeMap<&'static str, Rights>;

fn host_cap(name: &str) -> Option<&'static str> {
    HOST_CAPABILITIES.iter().copied().find(|c| *c == name)
}

fn build_cap(name: &str) -> Option<&'static str> {
    BUILD_CAPABILITIES.iter().copied().find(|c| *c == name)
}

/// Build-time capability kinds reachable from a type (no rights — kind-only).
/// Used only over the `build` entrypoint's parameters; recurses through tuples/
/// generics for soundness even though a build cap is normally a direct param.
fn build_caps_in(ty: &Type, out: &mut CapSet) {
    match ty {
        Type::Named(name, args) => {
            if let Some(b) = build_cap(name) {
                out.entry(b).or_default();
            }
            for a in args {
                build_caps_in(a, out);
            }
        }
        Type::Tuple(ts) => ts.iter().for_each(|t| build_caps_in(t, out)),
        Type::Fn(params, ret) => {
            params.iter().for_each(|p| build_caps_in(p, out));
            build_caps_in(ret, out);
        }
    }
}

/// The full right-set for a capability — what a *bare* `Dir`/`Net` (no brackets)
/// confers. `Console` and unknown names have no rights.
fn full_rights(cap: &str) -> Rights {
    match cap {
        "Dir" => ["Read", "Write"].into_iter().collect(),
        "File" => ["Read", "Write"].into_iter().collect(),
        // `Net` has two axes: verbs and transports. Bare `Net` is full on both.
        "Net" => ["Connect", "Listen", "Tcp", "Udp", "Uds"].into_iter().collect(),
        _ => Rights::new(),
    }
}

const NET_VERBS: [&str; 2] = ["Connect", "Listen"];
const NET_TRANSPORTS: [&str; 3] = ["Tcp", "Udp", "Uds"];

/// Map a bracketed marker to its canonical right name, or `None` if it isn't a
/// recognized right for that capability.
fn right_marker(cap: &str, marker: &str) -> Option<&'static str> {
    match (cap, marker) {
        ("Dir", "Read") => Some("Read"),
        ("Dir", "Write") => Some("Write"),
        ("File", "Read") => Some("Read"),
        ("File", "Write") => Some("Write"),
        ("Net", "Connect") => Some("Connect"),
        ("Net", "Listen") => Some("Listen"),
        ("Net", "Tcp") => Some("Tcp"),
        ("Net", "Udp") => Some("Udp"),
        ("Net", "Uds") => Some("Uds"),
        _ => None,
    }
}

/// Whether the concrete address `target` (`host:port`) is admitted by an
/// allowlist entry `pattern`. Patterns generalize an exact `host:port` along two
/// independent axes — the host may be a CIDR block, the port may be `*`:
/// `host:*` (any port on that host), `A.B.C.D/n:port` (any IPv4 in the block,
/// that port), or `A.B.C.D/n:*` (any IPv4 in the block, any port).
///
/// Exact string equality is the fast path and the fallback (so existing
/// `host:port` allowlists behave unchanged). Shared by BOTH backends so the
/// network confinement check is one implementation — the same discipline as
/// `confine::resolve` for `Dir` (a deliberate parity/security invariant).
///
/// `target` is expected to be concrete (a literal IP or a resolved host); the
/// connect path additionally re-checks the *resolved* IP, so a CIDR pattern with
/// a hostname target is matched against the address actually dialed, not the
/// name (DNS-rebinding safe — see the interpreter/runtime connect sites).
pub fn address_admits(pattern: &str, target: &str) -> bool {
    if pattern == target {
        return true;
    }
    let (phost, pport) = split_host_port(pattern);
    let (thost, tport) = split_host_port(target);
    if pport != "*" && pport != tport {
        return false;
    }
    if phost == thost {
        return true;
    }
    // A CIDR host pattern admits a literal IPv4 target inside the block.
    if let Some((base, bits)) = parse_ipv4_cidr(phost) {
        if let Ok(ip) = thost.parse::<std::net::Ipv4Addr>() {
            return ipv4_in_cidr(ip, base, bits);
        }
    }
    false
}

/// Split `host:port` on the LAST colon (so a bracketed IPv6 `[::1]:80` keeps its
/// host intact). A pattern with no port part has an empty port (matches nothing
/// but an empty target — callers always pass `host:port`).
fn split_host_port(s: &str) -> (&str, &str) {
    match s.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => (s, ""),
    }
}

fn parse_ipv4_cidr(s: &str) -> Option<(std::net::Ipv4Addr, u8)> {
    let (ip, bits) = s.split_once('/')?;
    let ip: std::net::Ipv4Addr = ip.parse().ok()?;
    let bits: u8 = bits.parse().ok()?;
    (bits <= 32).then_some((ip, bits))
}

fn ipv4_in_cidr(ip: std::net::Ipv4Addr, base: std::net::Ipv4Addr, bits: u8) -> bool {
    if bits == 0 {
        return true;
    }
    let mask = if bits == 32 { u32::MAX } else { !((1u32 << (32 - bits)) - 1) };
    (u32::from(ip) & mask) == (u32::from(base) & mask)
}

/// Whether the allowlist admits `target` — the per-op confinement check used by
/// `listen`/`restrict`/`connect` on both backends. An entry prefixed `!` is a DENY
/// (RFC-0011 `net.deny`): `target` is admitted iff some allow entry matches it AND
/// no deny entry matches it (`effective = allows \ denies`). Deny is monotone — it
/// can only subtract — so a flat allowlist with no `!` entries behaves unchanged.
pub fn net_allows(allow: &[String], target: &str) -> bool {
    if allow.iter().filter_map(|p| p.strip_prefix('!')).any(|d| address_admits(d, target)) {
        return false;
    }
    allow.iter().filter(|p| !p.starts_with('!')).any(|p| address_admits(p, target))
}

/// The error a backend MUST raise when `main` binds a host capability the host
/// cannot actually mint, so BOTH backends refuse identically and the spec's
/// "the root grant is always concrete — the host hands `main` a real capability or
/// that parameter doesn't exist" invariant holds (spec §13). Today the only such
/// case is a bare `Secret` with no signing key: a `Secret` *is* the key, so —
/// unlike an empty `Net` allowlist or an empty `SecretStore`, which are real
/// capabilities with no resources — there is no "empty" `Secret` to hand over.
/// Returns `None` when every parameter is grantable. Shared by the run paths so
/// the interpreter and the compiled backend can never drift on this.
pub fn unmintable_main_cap(main_params: &[witchy_syntax::ast::Param], has_signing_key: bool) -> Option<String> {
    let binds_secret = main_params
        .iter()
        .any(|p| matches!(&p.ty, Some(witchy_syntax::ast::Type::Named(n, _)) if n == "Secret"));
    if binds_secret && !has_signing_key {
        return Some(
            "`main` requires a `Secret`, but the host granted none \
             (provide `--signing-key <hex-seed-file>`)"
                .to_string(),
        );
    }
    None
}

/// Resolve `addr` and return the concrete socket addresses a `connect` may dial.
/// Rebinding-safe: a CIDR/IP allowlist is matched against the *resolved IP*, and
/// the connect is made to that exact address — so a hostile resolver cannot point
/// an allowlisted name at a disallowed host. A hostname that is itself allowlisted
/// by string falls back to all of its resolved addresses (hostname allowlists are
/// an ergonomic, explicitly non-rebinding-proof form; prefer IP/CIDR for untrusted
/// peers). Used by `connect`/`try_connect` on both backends.
pub fn resolve_admitted(allow: &[String], addr: &str) -> Result<Vec<std::net::SocketAddr>, String> {
    use std::net::ToSocketAddrs;
    let denied = || format!("`{addr}` is not permitted by this Net capability");
    // Whether the address STRING itself is allowlisted (an exact `host:port` or a
    // literal-IP pattern). The capability denial takes precedence over any DNS
    // failure, so a disallowed host reports "not permitted", never a resolver leak.
    let name_ok = net_allows(allow, addr);
    match addr.to_socket_addrs() {
        Ok(iter) => {
            let resolved: Vec<std::net::SocketAddr> = iter.collect();
            // Rebinding-safe: a CIDR/IP allowlist is matched against the resolved
            // IP, and the connect is made to exactly that address.
            let ip_ok: Vec<std::net::SocketAddr> = resolved
                .iter()
                .copied()
                .filter(|sa| net_allows(allow, &sa.to_string()))
                .collect();
            if !ip_ok.is_empty() {
                Ok(ip_ok)
            } else if name_ok {
                // A hostname allowlisted by string (not rebinding-proof): dial it.
                Ok(resolved)
            } else {
                Err(denied())
            }
        }
        // Could not resolve. A genuine dial failure only if the name was allowed;
        // otherwise it is a capability denial (don't leak the resolver error).
        Err(e) if name_ok => Err(format!("`{addr}` could not be resolved: {e}")),
        Err(_) => Err(denied()),
    }
}

/// The rights a capability-typed annotation confers: the bracketed markers if
/// present, else (bare capability) the full set. `Net`'s two axes default
/// independently — an axis with no marker mentioned is full — so `Net[Connect]`
/// keeps all transports, matching the type system.
fn rights_from_args(cap: &'static str, args: &[Type]) -> Rights {
    if args.is_empty() {
        return full_rights(cap);
    }
    let mut r = Rights::new();
    for a in args {
        if let Type::Named(n, _) = a {
            if let Some(m) = right_marker(cap, n) {
                r.insert(m);
            }
        }
    }
    if cap == "Net" {
        if !NET_VERBS.iter().any(|v| r.contains(v)) {
            r.extend(NET_VERBS);
        }
        if !NET_TRANSPORTS.iter().any(|t| r.contains(t)) {
            r.extend(NET_TRANSPORTS);
        }
    }
    r
}

/// Union `src` into `dst`, merging the rights of capabilities present in both.
fn merge_into(dst: &mut CapSet, src: &CapSet) {
    for (cap, rights) in src {
        dst.entry(cap).or_default().extend(rights.iter().copied());
    }
}

/// Host capabilities (with rights) reachable from a type, resolving user types
/// through `taint`. A capability wrapped in a type — a brand like
/// `ConfigDir(Dir[Read])`, or any record holding one — still confers that
/// authority (at those rights) on whoever receives the value, so the analyzer
/// must see through the wrapper to stay sound.
fn caps_in(ty: &Type, taint: &HashMap<String, CapSet>, out: &mut CapSet) {
    match ty {
        Type::Named(name, args) => {
            if let Some(h) = host_cap(name) {
                out.entry(h).or_default().extend(rights_from_args(h, args));
            }
            if let Some(caps) = taint.get(name) {
                merge_into(out, caps);
            }
            for a in args {
                caps_in(a, taint, out);
            }
        }
        Type::Tuple(ts) => {
            for t in ts {
                caps_in(t, taint, out);
            }
        }
        Type::Fn(params, ret) => {
            for p in params {
                caps_in(p, taint, out);
            }
            caps_in(ret, taint, out);
        }
    }
}

/// For each user type, the host capabilities (with rights) a value of it carries
/// (transitively through its fields). Computed to a fixpoint, since a type may
/// be tainted by another tainted user type.
fn taint_map(module: &Module) -> HashMap<String, CapSet> {
    let mut map: HashMap<String, CapSet> = HashMap::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            map.entry(t.name.clone()).or_default();
        }
    }
    loop {
        let mut changed = false;
        for item in &module.items {
            let Item::Type(t) = item else { continue };
            let mut acc = CapSet::new();
            for v in &t.variants {
                for fty in &v.fields {
                    caps_in(fty, &map, &mut acc);
                }
            }
            let slot = map.entry(t.name.clone()).or_default();
            let before = slot.clone();
            merge_into(slot, &acc);
            if *slot != before {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    map
}

/// Single-field newtype brands (`type ConfigDir: ConfigDir(Dir)`): a one-variant,
/// one-field type wrapping exactly one host capability (directly or via another
/// brand). The brand name is reported as a refinement of the bare capability —
/// authority-equivalent to it, but carrying the program's intent.
fn brand_map(
    module: &Module,
    taint: &HashMap<String, CapSet>,
) -> HashMap<String, &'static str> {
    let mut brands = HashMap::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            if t.variants.len() == 1 && t.variants[0].fields.len() == 1 {
                let mut caps = CapSet::new();
                caps_in(&t.variants[0].fields[0], taint, &mut caps);
                if caps.len() == 1 {
                    brands.insert(t.name.clone(), *caps.keys().next().unwrap());
                }
            }
        }
    }
    brands
}

/// One entry point: the host capabilities (with rights) it requires, plus the
/// names of any capability brands it receives them through (a display-only
/// refinement).
#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub capabilities: CapSet,
    pub brands: BTreeSet<String>,
}

/// A module's capability footprint: each entry point's requirements and the
/// union across all of them (the maximum host authority the module can wield).
pub struct Footprint {
    pub entries: Vec<Entry>,
    /// Every capability-touching function in declaration order — the entry
    /// points plus private helpers whose signatures carry capability types.
    /// Display-only (`witchy caps`): the gate and the published footprint
    /// records use `entries`/`total`, where private helpers are subsumed by
    /// whichever entry point reaches them.
    pub per_function: Vec<Entry>,
    pub total: CapSet,
    /// The capability brands (refinements) used anywhere in the module — the
    /// union of every entry's brands. Authority-equivalent to their host caps,
    /// but a finer-grained record of *intent*.
    pub brands: BTreeSet<String>,
    /// The **build-time** footprint: the build capabilities the rune's `build`
    /// entrypoint demands (empty if it ships no build step). A separate axis from
    /// the runtime `total` — they are granted and gated independently.
    pub build: CapSet,
}

/// What changed between two versions of a module's footprint. `added` is a
/// *widening* — host authority the newer version demands that the older did not.
/// That includes a wholly new capability (a dependency that suddenly asks for
/// `Net`) *and* a new right on an existing one (a `Net[Connect]` that can now
/// also `Listen`). `removed` is a narrowing (a dropped capability or right),
/// which is always safe. The supply-chain gate blocks on widening.
///
/// `refinements_dropped`/`refinements_gained` track *brand* changes. They never
/// change host authority — a brand is authority-equivalent to its host cap — so
/// they don't fail the gate, but a dropped refinement (a confined `ConfigDir`
/// loosened back to a raw `Dir`) is an intent change worth surfacing in review.
pub struct FootprintDiff {
    pub added: CapSet,
    pub removed: CapSet,
    /// Build-axis changes, tracked independently of the runtime axis. A
    /// `build_added` is a build-time widening — "this version now wants to `exec`
    /// / reach the network at build time" — gated separately (`--allow-build-cap`).
    pub build_added: CapSet,
    pub build_removed: CapSet,
    pub refinements_dropped: BTreeSet<String>,
    pub refinements_gained: BTreeSet<String>,
}

impl FootprintDiff {
    /// Whether the newer footprint demands authority the older one did not. This
    /// is the signal the install/CI gate fails on: new authority — a new
    /// capability or a new right on an existing one — must be an explicit,
    /// reviewed decision, never something a version bump slips in. Brand changes
    /// are intentional refinements, not authority, so they never trip this.
    pub fn widened(&self) -> bool {
        !self.added.is_empty() || !self.build_added.is_empty()
    }

    /// Whether the *build* axis specifically widened — the consuming project must
    /// grant the new build capability (and pass `--allow-build-cap`) to proceed.
    pub fn build_widened(&self) -> bool {
        !self.build_added.is_empty()
    }
}

/// The capabilities/rights present in `a` but not `b`: a wholly new capability,
/// or new rights on a shared one. The primitive behind both directions of a diff
/// (and the RFC-0013 grant cross-check in `crate::grants`).
pub(crate) fn cap_delta(a: &CapSet, b: &CapSet) -> CapSet {
    let mut out = CapSet::new();
    for (cap, ar) in a {
        match b.get(cap) {
            None => {
                out.insert(cap, ar.clone());
            }
            Some(br) => {
                let extra: Rights = ar.difference(br).copied().collect();
                if !extra.is_empty() {
                    out.insert(cap, extra);
                }
            }
        }
    }
    out
}

/// Compare two footprints by their total authority — the primitive behind the
/// block-on-widening gate. Because capabilities are unforgeable and only enter
/// through parameters, a module cannot gain authority without changing a public
/// entry point's signature, so this total-level diff fully captures a widening.
/// Brand differences are reported alongside as refinement (intent) changes.
pub fn diff(old: &Footprint, new: &Footprint) -> FootprintDiff {
    FootprintDiff {
        added: cap_delta(&new.total, &old.total),
        removed: cap_delta(&old.total, &new.total),
        build_added: cap_delta(&new.build, &old.build),
        build_removed: cap_delta(&old.build, &new.build),
        refinements_dropped: old.brands.difference(&new.brands).cloned().collect(),
        refinements_gained: new.brands.difference(&old.brands).cloned().collect(),
    }
}

/// Render one capability with its rights for human output: a bare name when it
/// has the full right-set (or none, like `Console`), else bracketed —
/// `Console`, `Dir`, `Dir[Read]`, `Net[Connect]`, `Net[Connect, Tcp]`.
pub fn show_cap(name: &str, rights: &Rights) -> String {
    if rights.is_empty() || *rights == full_rights(name) {
        return name.to_string();
    }
    if name == "Net" {
        // Two axes: omit an axis that is at its full set, so the expanded
        // `{Connect, Tcp, Udp, Uds}` prints as `Net[Connect]`, not the verbose
        // transport list. (Mirrors `NetRights`' Display in the type checker.)
        let mut parts: Vec<&str> = Vec::new();
        if !NET_VERBS.iter().all(|v| rights.contains(v)) {
            parts.extend(NET_VERBS.iter().filter(|v| rights.contains(*v)));
        }
        if !NET_TRANSPORTS.iter().all(|t| rights.contains(t)) {
            parts.extend(NET_TRANSPORTS.iter().filter(|t| rights.contains(*t)));
        }
        return format!("Net[{}]", parts.join(", "));
    }
    format!(
        "{name}[{}]",
        rights.iter().copied().collect::<Vec<_>>().join(", ")
    )
}

/// Render a whole capability set as a comma-joined list, or `(none)`.
pub fn show_caps(caps: &CapSet) -> String {
    if caps.is_empty() {
        "(none)".to_string()
    } else {
        caps.iter()
            .map(|(n, r)| show_cap(n, r))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn analyze(module: &Module) -> Footprint {
    let taint = taint_map(module);
    let brands = brand_map(module, &taint);
    let mut entries = Vec::new();
    let mut per_function = Vec::new();
    let mut total = CapSet::new();
    for item in &module.items {
        // The capability-bearing types at this entry point: a public function's
        // (or `main`'s) parameters. Private
        // functions get the same signature scan, but report-only.
        let (name, types, is_entry): (String, Vec<&Type>, bool) = match item {
            Item::Function(f) => (
                f.name.clone(),
                f.params.iter().filter_map(|p| p.ty.as_ref()).collect(),
                f.public || f.name == "main",
            ),
            _ => continue,
        };
        let mut capabilities = CapSet::new();
        let mut entry_brands = BTreeSet::new();
        for ty in types {
            let mut caps = CapSet::new();
            caps_in(ty, &taint, &mut caps);
            if caps.is_empty() {
                continue;
            }
            merge_into(&mut capabilities, &caps);
            // A directly-named brand is recorded as a refinement.
            if let Type::Named(n, _) = ty {
                if brands.contains_key(n.as_str()) {
                    entry_brands.insert(n.clone());
                }
            }
        }
        let entry = Entry {
            name,
            capabilities,
            brands: entry_brands,
        };
        if is_entry {
            merge_into(&mut total, &entry.capabilities);
            per_function.push(entry.clone());
            entries.push(entry);
        } else if !entry.capabilities.is_empty() {
            per_function.push(entry);
        }
    }
    let brands = entries.iter().flat_map(|e| e.brands.iter().cloned()).collect();
    // The build axis: the build capabilities the `build` entrypoint demands,
    // computed identically over its signature (§4.1). Build caps can only appear
    // there (the signature checks enforce it), so this never overlaps `total`.
    let mut build = CapSet::new();
    if let Some(b) = witchy_syntax::build_entry::build_entrypoint(module) {
        for ty in b.params.iter().filter_map(|p| p.ty.as_ref()) {
            build_caps_in(ty, &mut build);
        }
    }
    Footprint {
        entries,
        per_function,
        total,
        brands,
        build,
    }
}

/// The host authority a *run* of this module grants: `main`'s parameters alone.
///
/// Authority originates solely at `main` — witchy has no ambient capabilities, so a
/// function `main` never calls can never be reached holding one. This differs from
/// `analyze().total`, the whole-program union over every public entry point: once a
/// program is *linked*, the std modules' own `pub fn`s become items too, and a
/// verify-only program that imports `crypto` would otherwise inherit `crypto.sign`'s
/// `Secret` in its grant. `total` is the right surface for the supply-chain gate
/// (what a consumer COULD exercise through a rune's API); a run wants only what its
/// `main` actually receives. Empty when there is no `main`.
pub fn run_grant(module: &Module) -> CapSet {
    analyze(module)
        .entries
        .into_iter()
        .find(|e| e.name == "main")
        .map(|e| e.capabilities)
        .unwrap_or_default()
}
