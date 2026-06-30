//! RFC-0030: the single `WITCHY_OPT` optimization lever.
//!
//! One environment variable controls which optimizations the compiler applies.
//! It affects ONLY performance, never observable behavior — every setting must
//! produce identical output on both backends (the parity invariant). That is
//! what the differential de-opt sweep checks: for each optimization `O`,
//! `WITCHY_OPT=-O` must match `WITCHY_OPT=all` must match `WITCHY_OPT=none`.
//!
//! Grammar (comma-separated): the base is the production default unless the first
//! token is `all` (everything, including opt-in passes) or `none` (nothing);
//! then `-x` removes an optimization and `+x` / `x` adds one. Examples:
//!
//! ```text
//! WITCHY_OPT=none           # the canonical de-opt reference oracle
//! WITCHY_OPT=-inplace       # production default minus in-place mutation
//! WITCHY_OPT=-sroa,-views   # default minus scalar replacement and confined views
//! WITCHY_OPT=wasm-opt       # default plus the opt-in Binaryen post-pass
//! WITCHY_OPT=none,inplace   # ONLY in-place (allowlist from nothing)
//! ```
//!
//! A thread-local override (`set_for_tests`) lets in-process differential tests
//! sweep settings without racing the process environment. This replaces the
//! per-toggle env vars (`WITCHY_NO_INPLACE`, `WITCHY_WASM_OPT`) — one lever, one
//! registry, one sweep.

use std::cell::Cell;
use std::sync::OnceLock;

/// One optimization in the registry. Adding an optimization to the compiler
/// means adding a variant here (and to [`Opt::ALL`]), not a new env var.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Opt {
    /// Uniqueness-driven in-place mutation (off ⇒ copy-per-update). Replaces the
    /// former `WITCHY_NO_INPLACE` forced-copy mode.
    InPlace,
    /// Confined zero-copy borrows / views (off ⇒ materialize to a copy). RFC-0028.
    Views,
    /// Escape-driven scalar replacement of non-escaping aggregates (off ⇒ heap).
    /// RFC-0027.
    Sroa,
    /// Region / loop-watermark reclamation (off ⇒ no early reclaim).
    Region,
    /// RC-floor in-place reuse: a confined, never-aliased `var` reassigned to a
    /// same-length list literal overwrites its buffer in place instead of allocating
    /// (off ⇒ allocate a fresh list each reassignment, leaking the old). RFC-0016.
    RcElide,
    /// AST constant folding + propagation (off ⇒ evaluate at runtime).
    Fold,
    /// Packed / unboxed layouts: a confined `List` of fixed-scalar records stored
    /// as one flat inline buffer instead of an array of pointers to boxed records
    /// (off ⇒ the uniform boxed layout). Opt-in (the opt-mode asymptotic lever).
    /// RFC-0027.
    Unbox,
    /// RC-floor reclamation: free a confined, never-aliased heap `var`'s old buffer
    /// when it is overwritten by a fresh one (`x = f(x)` for ANY `f`), reusing it via
    /// a size-classed free-list — so generally-escaping / cache-eviction garbage is
    /// reclaimed, not leaked (off ⇒ leak it). Convention/escape-oracle-driven, general
    /// over all operations (no per-method code). Opt-in until complete. RFC-0016.
    RcFloor,
    /// The opt-in Binaryen post-pass (off by default). Replaces `WITCHY_WASM_OPT`.
    WasmOpt,
    /// (RFC-0034 L3) Closure devirtualization: a closure local provably bound to one
    /// lambda and never reassigned is called with a direct `call $__lamw{i}` instead
    /// of a `call_indirect` through its runtime code-index word — which also lets the
    /// Binaryen pass inline the lambda body (off ⇒ every closure call is indirect).
    DirectCall,
    // NOTE: the registry holds ONLY optimizations the compiler actually performs —
    // every entry must pass the differential de-opt sweep AND prove it fired
    // (RFC-0030's contract). For a MEMORY lever that proof is a `witchy stats`
    // counter (heap/reowns); for a call-SHAPE lever like `direct-call`, which moves
    // no bytes, the firing proof is a codegen-shape assertion (the emitted `call`
    // vs `call_indirect`, see `codegen_tests::devirtualizes_*`). A planned lever
    // with no consumer is still NOT registered (a phantom lever toggles a no-op,
    // passing the sweep trivially and lying about coverage).
}

impl Opt {
    /// Every optimization, in a stable order — drives the differential de-opt
    /// sweep and `witchy stats` reporting.
    pub const ALL: [Opt; 10] = [
        Opt::InPlace,
        Opt::Views,
        Opt::Sroa,
        Opt::Region,
        Opt::RcElide,
        Opt::Fold,
        Opt::Unbox,
        Opt::RcFloor,
        Opt::WasmOpt,
        Opt::DirectCall,
    ];

    /// The token used in `WITCHY_OPT` and reported by `witchy stats`.
    pub fn name(self) -> &'static str {
        match self {
            Opt::InPlace => "inplace",
            Opt::Views => "views",
            Opt::Sroa => "sroa",
            Opt::Region => "region",
            Opt::RcElide => "rc-elide",
            Opt::Fold => "fold",
            Opt::Unbox => "unbox",
            Opt::RcFloor => "rc-floor",
            Opt::WasmOpt => "wasm-opt",
            Opt::DirectCall => "direct-call",
        }
    }

    fn from_name(s: &str) -> Option<Opt> {
        Opt::ALL.into_iter().find(|o| o.name() == s)
    }

    /// In the production default set? Experimental / costly passes are opt-in
    /// (they must be named explicitly in `WITCHY_OPT`).
    fn default_on(self) -> bool {
        !matches!(self, Opt::WasmOpt | Opt::Unbox | Opt::RcFloor)
    }

    fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

/// A set of enabled optimizations (a small bitset over [`Opt`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OptSet(u32);

impl OptSet {
    /// Nothing enabled — the de-opt reference oracle.
    pub fn none() -> OptSet {
        OptSet(0)
    }

    /// Everything enabled, including the opt-in passes.
    pub fn all() -> OptSet {
        OptSet(Opt::ALL.iter().fold(0, |m, o| m | o.bit()))
    }

    /// The production default: every `default_on` optimization.
    pub fn default_set() -> OptSet {
        OptSet(
            Opt::ALL
                .iter()
                .filter(|o| o.default_on())
                .fold(0, |m, o| m | o.bit()),
        )
    }

    pub fn contains(self, o: Opt) -> bool {
        self.0 & o.bit() != 0
    }

    #[must_use]
    pub fn with(self, o: Opt) -> OptSet {
        OptSet(self.0 | o.bit())
    }

    #[must_use]
    pub fn without(self, o: Opt) -> OptSet {
        OptSet(self.0 & !o.bit())
    }
}

fn known_names() -> String {
    Opt::ALL
        .iter()
        .map(|o| o.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse a `WITCHY_OPT` value into an [`OptSet`]. Returns a human-readable error
/// for an unknown token or a misplaced `all`/`none`.
pub fn parse(spec: &str) -> Result<OptSet, String> {
    let mut set = OptSet::default_set();
    for (i, raw) in spec.split(',').map(str::trim).filter(|t| !t.is_empty()).enumerate() {
        match raw {
            "all" if i == 0 => set = OptSet::all(),
            "none" if i == 0 => set = OptSet::none(),
            "all" | "none" => {
                return Err(format!("`{raw}` must be the first token in WITCHY_OPT"));
            }
            _ => {
                let (add, name) = match raw.strip_prefix('-') {
                    Some(rest) => (false, rest),
                    None => (true, raw.strip_prefix('+').unwrap_or(raw)),
                };
                let o = Opt::from_name(name).ok_or_else(|| {
                    format!("unknown optimization `{name}` in WITCHY_OPT (known: {})", known_names())
                })?;
                set = if add { set.with(o) } else { set.without(o) };
            }
        }
    }
    Ok(set)
}

fn env_default() -> OptSet {
    static CACHE: OnceLock<OptSet> = OnceLock::new();
    *CACHE.get_or_init(|| match std::env::var("WITCHY_OPT") {
        Ok(spec) => parse(&spec).unwrap_or_else(|e| panic!("WITCHY_OPT: {e}")),
        Err(_) => OptSet::default_set(),
    })
}

thread_local! {
    static OVERRIDE: Cell<Option<OptSet>> = const { Cell::new(None) };
}

/// Is optimization `o` enabled for this compilation? Consults the thread-local
/// test override first, then the cached `WITCHY_OPT` environment value.
pub fn enabled(o: Opt) -> bool {
    OVERRIDE
        .with(Cell::get)
        .unwrap_or_else(env_default)
        .contains(o)
}

/// Override the active optimization set for in-process differential tests
/// (`Some(set)`), or fall back to the environment (`None`). Always compiled (not
/// `#[cfg(test)]`) so the `witchy` binary's cross-crate tests can reach it.
#[doc(hidden)]
pub fn set_for_tests(set: Option<OptSet>) {
    OVERRIDE.with(|c| c.set(set));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_production_set_with_optin_off() {
        let d = OptSet::default_set();
        assert!(d.contains(Opt::InPlace));
        assert!(d.contains(Opt::Region));
        assert!(!d.contains(Opt::WasmOpt), "wasm-opt is opt-in");
        assert!(!d.contains(Opt::Unbox), "unbox (packed layouts) is opt-in");
        assert!(d.contains(Opt::Views) && d.contains(Opt::Sroa), "default includes shipped opts");
    }

    #[test]
    fn none_and_all_keywords() {
        assert_eq!(parse("none").unwrap(), OptSet::none());
        assert_eq!(parse("all").unwrap(), OptSet::all());
        assert!(OptSet::all().contains(Opt::WasmOpt));
        assert!(!OptSet::none().contains(Opt::InPlace));
    }

    #[test]
    fn subtractive_from_default() {
        let s = parse("-inplace").unwrap();
        assert!(!s.contains(Opt::InPlace));
        assert!(s.contains(Opt::Region), "only inplace removed");
        assert!(!s.contains(Opt::WasmOpt), "still opt-in off");
    }

    #[test]
    fn additive_and_allowlist() {
        assert!(parse("wasm-opt").unwrap().contains(Opt::WasmOpt));
        let only = parse("none,inplace,+views").unwrap();
        assert!(only.contains(Opt::InPlace) && only.contains(Opt::Views));
        assert!(!only.contains(Opt::Region), "allowlist excludes the rest");
    }

    #[test]
    fn errors_on_unknown_and_misplaced_keyword() {
        assert!(parse("bogus").is_err());
        assert!(parse("-nope").is_err());
        assert!(parse("inplace,all").is_err(), "`all` must be first");
    }

    #[test]
    fn override_takes_precedence() {
        set_for_tests(Some(OptSet::none()));
        assert!(!enabled(Opt::InPlace));
        set_for_tests(Some(OptSet::default_set()));
        assert!(enabled(Opt::InPlace));
        set_for_tests(None);
    }

    #[test]
    fn names_round_trip() {
        for o in Opt::ALL {
            assert_eq!(Opt::from_name(o.name()), Some(o));
        }
    }
}
