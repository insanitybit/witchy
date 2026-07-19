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
    &["Console", "Clock", "Rand", "Env", "Secret", "SecretStore", "Dir", "File", "Net", "Exec"];

/// Capability values that are derived from host capabilities at runtime, rather
/// than granted as root authorities at an entry point.
pub const DERIVED_CAPABILITIES: &[&str] = &["Socket", "Listener"];

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

pub fn is_host_capability(name: &str) -> bool {
    host_cap(name).is_some()
}

pub fn is_build_capability(name: &str) -> bool {
    build_cap(name).is_some()
}

pub fn is_capability_type_name(name: &str) -> bool {
    is_host_capability(name)
        || is_build_capability(name)
        || DERIVED_CAPABILITIES.contains(&name)
}

/// Build-time capability kinds reachable from a type (no rights — kind-only).
/// Used only over the `build` entrypoint's parameters; recurses through tuples/
/// generics for soundness even though a build cap is normally a direct param.
fn build_caps_in(ty: &Type, out: &mut CapSet) {
    match ty {
        Type::Qualified(_, inner) => build_caps_in(inner, out),
        Type::Named(name, args) => {
            if let Some(b) = build_cap(name) {
                out.entry(b).or_default();
            }
            for a in args {
                build_caps_in(a, out);
            }
        }
        // (RFC-0081) A dyn value is never itself a capability; scan args only.
        Type::Dyn(_, args) => args.iter().for_each(|a| build_caps_in(a, out)),
        Type::Tuple(ts) => ts.iter().for_each(|t| build_caps_in(t, out)),
        Type::RecordCompose { base, fields } => {
            build_caps_in(base, out);
            fields.iter().for_each(|(_, ty)| build_caps_in(ty, out));
        }
        Type::Fn(params, ret, _) => {
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
    // A CIDR host pattern admits a literal IP target inside the block — IPv4…
    if let Some((base, bits)) = parse_ipv4_cidr(phost) {
        if let Ok(ip) = thost.parse::<std::net::Ipv4Addr>() {
            return ipv4_in_cidr(ip, base, bits);
        }
    }
    // …and IPv6 (RFC-0020: closes a silent `confine.private()` gap — its `::1/128`,
    // `fe80::/10`, `fc00::/7` ranges only ever exact-matched before, so an internal IPv6
    // address slipped past `net.deny(confine.private())`). Hosts may be bracketed (`[::1]`).
    if let Some((base, bits)) = parse_ipv6_cidr(phost) {
        if let Ok(ip) = strip_brackets(thost).parse::<std::net::Ipv6Addr>() {
            return ipv6_in_cidr(ip, base, bits);
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

/// Strip a single pair of surrounding brackets (`[::1]` → `::1`), the standard way to
/// write an IPv6 host so `host:port` splitting on the last colon is unambiguous.
fn strip_brackets(h: &str) -> &str {
    h.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(h)
}

fn parse_ipv6_cidr(s: &str) -> Option<(std::net::Ipv6Addr, u8)> {
    let (ip, bits) = s.split_once('/')?;
    let ip: std::net::Ipv6Addr = strip_brackets(ip).parse().ok()?;
    let bits: u8 = bits.parse().ok()?;
    (bits <= 128).then_some((ip, bits))
}

fn ipv6_in_cidr(ip: std::net::Ipv6Addr, base: std::net::Ipv6Addr, bits: u8) -> bool {
    if bits == 0 {
        return true;
    }
    let mask: u128 = if bits == 128 { u128::MAX } else { !((1u128 << (128 - bits)) - 1) };
    (u128::from(ip) & mask) == (u128::from(base) & mask)
}

/// Whether the allowlist admits `target` — the per-op confinement check used by
/// `listen`/`restrict`/`connect` on both backends. An entry prefixed `!` is a DENY
/// (RFC-0011 `net.deny`): `target` is admitted iff some allow entry matches it AND
/// no deny entry matches it (`effective = allows \ denies`). Deny is monotone — it
/// can only subtract — so a flat allowlist with no `!` entries behaves unchanged.
pub fn net_allows(allow: &[String], target: &str) -> bool {
    if net_denied(allow, target) {
        return false;
    }
    allow.iter().filter(|p| !p.starts_with('!')).any(|p| address_admits(p, target))
}

/// Whether any `!`-DENY entry in `allow` matches `target`. Deny is monotone and
/// applies regardless of *how* the target was admitted — by CIDR/IP or by an
/// allowlisted hostname — so this is consulted both here and by `resolve_admitted`
/// on the resolved IPs of a name-allowlisted destination (the SSRF/rebinding floor).
fn net_denied(allow: &[String], target: &str) -> bool {
    allow.iter().filter_map(|p| p.strip_prefix('!')).any(|d| address_admits(d, target))
}

/// Narrow `allow` to the `\n`-joined `patterns` of a `NetPolicy` (`net.only` /
/// `restrict`). Each new pattern must already be admitted by the current set
/// (refinement only shrinks); on a pattern that isn't, returns it as the `Err`.
///
/// Crucially, the parent's `!`-DENY entries are CARRIED FORWARD into the narrowed
/// set. `only(P)` is the intersection `current ∩ P`, and `current = allows \ denies`,
/// so a `deny` already in effect must survive a later `only` — otherwise
/// `net.deny(X).only(superset-of-X)` would silently re-admit `X`, breaking RFC-0011's
/// "refinement can only ever shrink the set". Shared by BOTH backends so the
/// narrowing is one implementation (no parity drift), the same discipline as
/// `net_allows`/`address_admits`.
pub fn net_only(allow: &[String], patterns: &str) -> Result<Vec<String>, String> {
    let mut narrowed = Vec::new();
    for p in patterns.split('\n') {
        if !net_allows(allow, p) {
            return Err(p.to_string());
        }
        narrowed.push(p.to_string());
    }
    narrowed.extend(allow.iter().filter(|e| e.starts_with('!')).cloned());
    Ok(narrowed)
}

/// The deny-everything `Dir` policy sentinel: an impossible refinement (e.g.
/// `only(ext(".txt")).only(ext(".md"))`) narrows to this, and [`dir_admits`] denies
/// every entry — file OR directory — under it. A bare NUL cannot occur in any real
/// `ext:`/`kind:` pattern, so it is unambiguous.
const DIR_DENY_ALL: &str = "\u{0}";

/// A `Dir` entry policy (RFC-0011): a `\n`-joined set of `ext:<suffix>` and
/// `kind:file`/`kind:dir` patterns, in TWO dimensions (name-suffix, entry-kind).
/// `""` means unrestricted. `only(refine)` **additionally requires** `refine`'s
/// constraints: within a dimension it INTERSECTS (refinement only shrinks); a
/// dimension present in `refine` but not `current` is ADDED (cross-dimension AND, so
/// `only(files()).only(ext(".txt"))` requires both). An emptied dimension is
/// impossible, so the whole policy collapses to [`DIR_DENY_ALL`]. Shared by both
/// backends so the narrowing is one implementation.
pub fn dir_only(current: &str, refine: &str) -> String {
    if refine.is_empty() {
        return current.to_string();
    }
    use std::collections::{BTreeMap, BTreeSet};
    // Group patterns by dimension (the part before `:`), e.g. "ext" / "kind".
    fn group(s: &str) -> BTreeMap<&str, BTreeSet<&str>> {
        let mut m: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for p in s.split('\n') {
            if let Some((dim, _)) = p.split_once(':') {
                m.entry(dim).or_default().insert(p);
            }
        }
        m
    }
    let refi = group(refine);
    // BUG-257: a non-empty refine that yields NO valid `dim:pattern` (a raw/garbled
    // constraint with no `:` form) must FAIL CLOSED — an unrecognized narrowing can only
    // deny, never silently keep the current (wider) policy or install the garbage as-is.
    if refi.is_empty() {
        return DIR_DENY_ALL.to_string();
    }
    if current.is_empty() {
        return refine.to_string();
    }
    let cur = group(current);
    let mut out: BTreeSet<&str> = BTreeSet::new();
    // Dimensions `current` constrains that `refine` does not: carry them forward.
    for (dim, pats) in &cur {
        if !refi.contains_key(dim) {
            out.extend(pats.iter().copied());
        }
    }
    for (dim, rpats) in &refi {
        match cur.get(dim) {
            // Same dimension: intersect (refinement shrinks). Empty → impossible.
            Some(cpats) => {
                let common: BTreeSet<&str> = rpats.intersection(cpats).copied().collect();
                if common.is_empty() {
                    return DIR_DENY_ALL.to_string();
                }
                out.extend(common);
            }
            // New dimension: AND it in.
            None => out.extend(rpats.iter().copied()),
        }
    }
    out.into_iter().collect::<Vec<_>>().join("\n")
}

/// Whether a `Dir`'s entry policy admits touching the entry `name`, which is a
/// directory iff `is_dir` (RFC-0011). An empty policy admits everything. Otherwise
/// every constrained DIMENSION must admit (intersection across dimensions; union
/// within one). An `ext:<suffix>` pattern constrains FILE names only — a directory
/// entry passes the ext dimension automatically, so an `ext`-only policy never
/// restricts directory traversal (backward-compatible with pre-`kind` policies);
/// `kind:file` admits only files, `kind:dir` only directories. The [`DIR_DENY_ALL`]
/// sentinel denies everything. Enforced at file access AND directory traversal on
/// both backends, the filesystem analog of `net_allows`.
pub fn dir_admits(policy: &str, name: &str, is_dir: bool) -> bool {
    if policy.is_empty() {
        return true;
    }
    let (mut has_ext, mut ext_ok) = (false, false);
    let (mut has_kind, mut kind_ok) = (false, false);
    for p in policy.split('\n') {
        if p == DIR_DENY_ALL {
            return false;
        } else if let Some(ext) = p.strip_prefix("ext:") {
            has_ext = true;
            // A directory entry is not constrained by a file-suffix pattern.
            if is_dir || name.ends_with(ext) {
                ext_ok = true;
            }
        } else if let Some(kind) = p.strip_prefix("kind:") {
            has_kind = true;
            if (kind == "dir") == is_dir {
                kind_ok = true;
            }
        }
    }
    (!has_ext || ext_ok) && (!has_kind || kind_ok)
}

/// (RFC-0060) The error both backends raise when `crypto.reveal` is called on a
/// **use-only** secret. A use-only secret (granted `--secret name=value,use-only`,
/// or a served TLS key) may be consumed by handle but never read back into guest
/// memory. Defined once here so the interpreter and the compiled runtime surface
/// the SAME text and cannot drift on this refusal.
pub const USE_ONLY_SECRET_REVEAL_ERROR: &str =
    "this secret is use-only and cannot be revealed; use it by handle (e.g. crypto.sign or server.serve_tls)";

/// Whether `secret`'s bytes are the host's signing key (the `--signing-key` seed).
/// A `Secret` equal to the signing key — the bare `Secret` capability, or
/// `SecretStore.require("signing")` — is SIGN-ONLY (`crypto.sign`/`public_key`) and
/// must not be revealable: otherwise handing code a key to sign with also lets it
/// exfiltrate the key. Named `--secret`/`--secret-file` value-secrets stay revealable.
/// Both backends gate `crypto.reveal` through this one identity rule, so they can
/// never drift, and the comparison is constant-time so a guessed secret leaks nothing.
pub fn secret_is_signing_key(signing_key: Option<&[u8]>, secret: &[u8]) -> bool {
    match signing_key {
        Some(seed) if seed.len() == secret.len() => {
            let mut diff = 0u8;
            for (a, b) in seed.iter().zip(secret.iter()) {
                diff |= a ^ b;
            }
            diff == 0
        }
        _ => false,
    }
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
                // A hostname allowlisted by string (not rebinding-proof): dial it,
                // but STILL honor `!`-deny entries against the resolved IPs. A name
                // allowlist widens *which names* may be dialed; it must never
                // re-admit an IP a `net.deny(private())` subtracted — otherwise
                // `localhost`/an attacker-controlled name resolving to 127.0.0.1
                // (or any RFC-1918 / metadata address) would connect despite the
                // deny, defeating the SSRF/rebinding floor.
                let not_denied: Vec<std::net::SocketAddr> =
                    resolved.into_iter().filter(|sa| !net_denied(allow, &sa.to_string())).collect();
                if not_denied.is_empty() {
                    Err(denied())
                } else {
                    Ok(not_denied)
                }
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
        Type::Qualified(_, inner) => caps_in(inner, taint, out),
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
        // (RFC-0081) A dyn value is never itself a capability, and its head is a
        // trait name (never a taint-map key); scan args only.
        Type::Dyn(_, args) => {
            for a in args {
                caps_in(a, taint, out);
            }
        }
        Type::Tuple(ts) => {
            for t in ts {
                caps_in(t, taint, out);
            }
        }
        Type::RecordCompose { base, fields } => {
            caps_in(base, taint, out);
            for (_, ty) in fields {
                caps_in(ty, taint, out);
            }
        }
        Type::Fn(params, ret, _) => {
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
    /// RFC-0038: the bare **grantable** user capabilities an entry point receives
    /// (e.g. `UiRoot`). A separate axis from `total`: these carry no host authority
    /// (they're policy-only, minted from a `[user_caps]` grant), but a dependency
    /// that starts requiring one — or a new one — is a widening a reviewer/Coven
    /// gate must see, since granting one puts the declaring package in the policy TCB.
    pub user_caps: BTreeSet<String>,
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
    /// RFC-0038: grantable user caps the newer version requires that the older did
    /// not (a widening — new UI-effect/library authority, and a new package in the
    /// policy TCB), and ones it dropped (a narrowing, always safe).
    pub user_caps_added: BTreeSet<String>,
    pub user_caps_removed: BTreeSet<String>,
}

impl FootprintDiff {
    /// Whether the newer footprint demands authority the older one did not. This
    /// is the signal the install/CI gate fails on: new authority — a new
    /// capability or a new right on an existing one — must be an explicit,
    /// reviewed decision, never something a version bump slips in. Brand changes
    /// are intentional refinements, not authority, so they never trip this.
    pub fn widened(&self) -> bool {
        !self.added.is_empty() || !self.build_added.is_empty() || !self.user_caps_added.is_empty()
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
        user_caps_added: new.user_caps.difference(&old.user_caps).cloned().collect(),
        user_caps_removed: old.user_caps.difference(&new.user_caps).cloned().collect(),
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

fn analyze_signature(
    name: String,
    types: Vec<&Type>,
    is_entry: bool,
    taint: &HashMap<String, CapSet>,
    brands: &HashMap<String, &'static str>,
    grantable: &std::collections::HashSet<&str>,
    user_caps: &mut BTreeSet<String>,
) -> Entry {
    let mut capabilities = CapSet::new();
    let mut entry_brands = BTreeSet::new();
    for ty in types {
        // RFC-0038: a bare grantable cap carries no host authority, so it is
        // invisible to `caps_in` — record it on its own axis (entry points only).
        if is_entry {
            if let Type::Named(n, _) = ty {
                if grantable.contains(n.as_str()) {
                    user_caps.insert(n.clone());
                }
            }
        }
        let mut caps = CapSet::new();
        caps_in(ty, taint, &mut caps);
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
    Entry {
        name,
        capabilities,
        brands: entry_brands,
    }
}

fn impl_method_entry_name(im: &witchy_syntax::ast::ImplDef, method: &str) -> String {
    match &im.trait_name {
        Some(trait_name) => format!("{trait_name} for {}.{method}", im.type_name),
        None => format!("{}.{method}", im.type_name),
    }
}

pub fn analyze(module: &Module) -> Footprint {
    let taint = taint_map(module);
    let brands = brand_map(module, &taint);
    // RFC-0038: names of bare grantable capabilities declared in the module.
    let grantable: std::collections::HashSet<&str> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect();
    let mut user_caps: BTreeSet<String> = BTreeSet::new();
    let mut entries = Vec::new();
    let mut per_function = Vec::new();
    let mut total = CapSet::new();
    for item in &module.items {
        // The capability-bearing types at this entry point: a public function's
        // (or `main`'s) parameters. Private functions get the same signature scan,
        // but report-only. Public impl methods are callable API too, so they are
        // scanned under the same rule; private impl helpers remain report-only.
        let mut item_entries = Vec::new();
        match item {
            Item::Function(f) if f.comptime_only => {}
            Item::Function(f) => {
                item_entries.push((
                    f.name.clone(),
                    f.params.iter().filter_map(|p| p.ty.as_ref()).collect(),
                    f.public || f.name == "main",
                ));
            }
            Item::Impl(im) => {
                for method in &im.methods {
                    item_entries.push((
                        impl_method_entry_name(im, &method.name),
                        method.params.iter().filter_map(|p| p.ty.as_ref()).collect(),
                        method.public,
                    ));
                }
            }
            _ => {}
        }
        for (name, types, is_entry) in item_entries {
            let entry = analyze_signature(
                name,
                types,
                is_entry,
                &taint,
                &brands,
                &grantable,
                &mut user_caps,
            );
            if is_entry {
                merge_into(&mut total, &entry.capabilities);
                per_function.push(entry.clone());
                entries.push(entry);
            } else if !entry.capabilities.is_empty() {
                per_function.push(entry);
            }
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
        user_caps,
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

#[cfg(test)]
mod dir_policy_tests {
    use super::{dir_admits, dir_only};

    // RFC-0011: a Dir entry policy admits everything when empty, else only the
    // allowed extensions; `dir.only` intersects (refinement shrinks), and a
    // disjoint refinement admits nothing. (The `false` = the entry is a file.)
    #[test]
    fn dir_ext_policy_admits_and_intersects() {
        assert!(dir_admits("", "anything.bin", false), "empty policy is unrestricted");
        assert!(dir_admits("ext:.txt", "notes.txt", false));
        assert!(!dir_admits("ext:.txt", "secret.key", false));
        assert_eq!(dir_only("", "ext:.txt"), "ext:.txt");
        assert_eq!(dir_only("ext:.txt\next:.md", "ext:.txt"), "ext:.txt");
        // disjoint intersection -> admits nothing (file OR dir)
        let none = dir_only("ext:.txt", "ext:.md");
        assert!(!dir_admits(&none, "x.txt", false) && !dir_admits(&none, "x.md", false));
        assert!(!dir_admits(&none, "sub", true), "the deny-all sentinel denies dirs too");
    }

    // RFC-0011: the `kind:` dimension gates by entry type, AND-composed with `ext:`.
    #[test]
    fn dir_kind_policy_gates_by_entry_type() {
        // `files()` admits file entries, denies directory entries.
        assert!(dir_admits("kind:file", "notes.txt", false));
        assert!(!dir_admits("kind:file", "sub", true));
        // `dirs()` is the mirror.
        assert!(dir_admits("kind:dir", "sub", true));
        assert!(!dir_admits("kind:dir", "notes.txt", false));
        // An `ext`-only policy never restricts directory traversal (backward-compat).
        assert!(dir_admits("ext:.txt", "sub", true), "ext gates files, not dirs");
        // Cross-dimension AND: `only(files()).only(ext(".txt"))`.
        let both = dir_only("kind:file", "ext:.txt");
        assert!(dir_admits(&both, "notes.txt", false), ".txt file admitted");
        assert!(!dir_admits(&both, "notes.md", false), "non-.txt file denied");
        assert!(!dir_admits(&both, "sub", true), "directory denied by kind:file");
    }
}

#[cfg(test)]
mod grantable_footprint_tests {
    use super::{analyze, diff};
    use witchy_syntax::parser::parse_module;

    #[test]
    fn grantable_cap_is_a_footprint_axis_and_widening() {
        // (RFC-0038) a grantable cap at `main` shows on the `user_caps` axis, and
        // carries no host authority (absent from `total`).
        let with = parse_module(
            "grantable capability UiRoot:\n    policy: String\n\nfn main(console: Console, ui: UiRoot):\n    console.print(\"ok\")\n",
        )
        .unwrap();
        let fp_with = analyze(&with);
        assert!(fp_with.user_caps.contains("UiRoot"));
        assert!(!fp_with.total.contains_key("UiRoot"), "a bare cap carries no host authority");

        // Requiring a grantable cap a prior version did not is a widening; dropping
        // it is a safe narrowing.
        let without = parse_module("fn main(console: Console):\n    console.print(\"ok\")\n").unwrap();
        let fp_without = analyze(&without);
        let widened = diff(&fp_without, &fp_with);
        assert!(widened.user_caps_added.contains("UiRoot"));
        assert!(widened.widened(), "a newly-required grantable cap widens the footprint");
        assert!(!diff(&fp_with, &fp_without).widened(), "dropping it is a narrowing");
    }
}

#[cfg(test)]
mod net_only_tests {
    use super::{address_admits, net_allows, net_only};

    // RFC-0011 monotonicity: a `deny` in effect must survive a later `only`, even an
    // `only` of the enclosing block. Regression for the `only`-drops-`deny` re-widening.
    #[test]
    fn only_of_enclosing_block_preserves_a_prior_deny() {
        let granted = vec!["127.0.0.0/8:*".to_string()];
        let denied = {
            let mut a = granted.clone();
            a.push("!127.0.0.1:1".to_string());
            a
        };
        // Sanity: the deny is honored before any `only`.
        assert!(!net_allows(&denied, "127.0.0.1:1"));
        // `only(127.0.0.0/8:*)` must NOT re-admit the denied host.
        let narrowed = net_only(&denied, "127.0.0.0/8:*").expect("block is admitted");
        assert!(
            !net_allows(&narrowed, "127.0.0.1:1"),
            "`only` re-widened a denied address: {narrowed:?}"
        );
        // A non-denied host in the block stays reachable.
        assert!(net_allows(&narrowed, "127.0.0.2:80"));
    }

    // RFC-0020: `confine.private()` -> `net.deny(...)` appends the private-range
    // CIDRs as `!`-deny entries; an address resolving into any of them (loopback,
    // RFC-1918, the 169.254.169.254 metadata IP, CGNAT, "this host") is refused,
    // while public addresses stay reachable. This is the SSRF/DNS-rebinding defense.
    #[test]
    fn private_ranges_deny_internal_addresses() {
        let ranges = [
            "127.0.0.0/8", "10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16",
            "169.254.0.0/16", "100.64.0.0/10", "0.0.0.0/8",
        ];
        // Allow-all base, then deny the private ranges — the `net.deny(private())` shape.
        let mut allow = vec!["0.0.0.0/0:*".to_string()];
        allow.extend(ranges.iter().map(|c| format!("!{c}:*")));
        for internal in [
            "127.0.0.1:443", "10.1.2.3:443", "172.16.5.5:443", "172.31.0.1:443",
            "192.168.1.1:443", "169.254.169.254:80", "100.64.0.1:443", "0.1.2.3:443",
        ] {
            assert!(!net_allows(&allow, internal), "{internal} should be denied by confine.private()");
        }
        // Public addresses remain reachable.
        assert!(net_allows(&allow, "8.8.8.8:443"));
        assert!(net_allows(&allow, "93.184.216.34:443"));
    }

    // RFC-0020 step 1: the IPv6 half of the SSRF/rebinding defense. `confine.private()`'s IPv6
    // ranges (`::1/128`, `fe80::/10`, `fc00::/7`) are now CIDR-MATCHED — before this they only
    // ever exact-matched, so an internal IPv6 address slipped past `net.deny(confine.private())`.
    // Hosts are bracketed (`[::1]`), the standard way to keep `host:port` unambiguous.
    #[test]
    fn private_ranges_deny_internal_ipv6() {
        // Allow all IPv6, then deny the private ranges (the `net.deny(private())` shape).
        let mut allow = vec!["::/0:*".to_string()];
        for c in ["::1/128", "fe80::/10", "fc00::/7"] {
            allow.push(format!("!{c}:*"));
        }
        for internal in ["[::1]:443", "[fe80::1]:80", "[fc00::1]:443", "[fdff:ffff::1]:443"] {
            assert!(!net_allows(&allow, internal), "{internal} should be denied by confine.private()");
        }
        // Public IPv6 (Google + Cloudflare DNS) stays reachable.
        assert!(net_allows(&allow, "[2001:4860:4860::8888]:443"), "public IPv6 stays reachable");
        assert!(net_allows(&allow, "[2606:4700:4700::1111]:443"));
        // Directly: the CIDR host-match, bracketed and not.
        assert!(address_admits("fc00::/7:*", "[fd12:3456::1]:443"));
        assert!(!address_admits("fe80::/10:*", "[2001:db8::1]:80"));
        assert!(address_admits("::1/128:*", "[::1]:22"));
    }

    #[test]
    fn only_rejects_a_pattern_outside_the_current_set() {
        let granted = vec!["10.0.0.5:6379".to_string()];
        // Can't widen a single host to the whole block.
        assert_eq!(net_only(&granted, "10.0.0.0/8:*"), Err("10.0.0.0/8:*".to_string()));
    }

    #[test]
    fn only_narrows_to_an_admitted_host() {
        let granted = vec!["10.0.0.0/8:*".to_string()];
        let narrowed = net_only(&granted, "10.0.0.5:6379").expect("host is in the block");
        assert!(net_allows(&narrowed, "10.0.0.5:6379"));
        assert!(!net_allows(&narrowed, "10.0.0.6:6379"));
    }
}

#[cfg(test)]
mod resolve_admitted_tests {
    use super::resolve_admitted;

    // A name-allowlisted destination whose resolved IP is `!`-deny-matched must NOT
    // be dialed: the deny floor applies to the resolved IPs even when the address
    // STRING itself is allowlisted. Regression for the SSRF bypass where
    // `net.deny(private())` was defeated by an allowlisted `localhost`.
    #[test]
    fn name_allowlisted_but_resolved_ip_denied_is_refused() {
        // `localhost:0` is allowlisted by string but resolves to loopback, which
        // the `!127.0.0.0/8` / `!::1/128` deny entries subtract.
        let allow = vec![
            "localhost:0".to_string(),
            "!127.0.0.0/8:*".to_string(),
            "!::1/128:*".to_string(),
        ];
        let err = resolve_admitted(&allow, "localhost:0")
            .expect_err("all resolved loopback IPs are deny-matched → must be refused");
        assert!(err.contains("not permitted"), "expected a capability denial, got: {err}");
    }

    // The deny floor only subtracts the denied IPs; if a name resolves to at least
    // one non-denied address the connect still proceeds to that address.
    #[test]
    fn name_allowlisted_keeps_non_denied_resolved_ips() {
        // A literal-IP destination that is allowlisted and NOT denied resolves to
        // exactly itself and is admitted.
        let allow = vec!["93.184.216.34:80".to_string(), "!127.0.0.0/8:*".to_string()];
        let dialed = resolve_admitted(&allow, "93.184.216.34:80").expect("public IP is admitted");
        assert!(dialed.iter().all(|sa| sa.to_string() != "127.0.0.1:80"));
        assert!(!dialed.is_empty());
    }
}
