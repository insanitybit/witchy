//! Capability inspection, diffing, and grant-check command services.

use witchy_caps::{capabilities as capability_model, grants};
use witchy_interp::comptime;
use witchy_syntax::parser;
use crate::link_file_checked;

/// Read, parse, and compute the host-capability footprint of a source file.
pub(crate) fn analyze_file(path: &str) -> Result<capability_model::Footprint, String> {
    // BUG-179: a footprint computed over code that doesn't type-check is meaningless
    // (an undefined-function call, a type error). Link + type-check the whole program
    // first, so `caps`/`caps-diff` refuse a source that `check` would reject rather
    // than reporting a footprint for it.
    let (_checked, _stem) = link_file_checked(path)?;
    // Report the footprint of the ENTRY file's own items (unprefixed names, matching
    // the existing per-function output) — but with its `comptime:` blocks EXPANDED
    // (BUG-178). A `comptime:` block that `emit`s `pub fn generated(net: Net)` adds a
    // real capability-bearing API; `capabilities::analyze` treats generated code
    // exactly like handwritten code, so it must see the expanded items. This is the
    // same additive per-module pass the linker runs, applied to the single module.
    let src = std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    let mut module = parser::parse_module(&src).map_err(|e| e.to_string())?;
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    comptime::expand(stem, &mut module).map_err(|e| format!("{path}: {e}"))?;
    Ok(capability_model::analyze(&module))
}

/// Print the host-capability footprint of a single source file: every
/// capability-touching function (entry points and private helpers), and the
/// union over the entry points.
pub(crate) fn report_capabilities(path: &str) -> Result<(), String> {
    let fp = analyze_file(path)?;
    let show = capability_model::show_caps;
    println!("Host-capability footprint of {path}:");
    let width = fp
        .per_function
        .iter()
        .map(|e| e.name.len())
        .max()
        .unwrap_or(0)
        .max("total".len());
    for e in &fp.per_function {
        let refined = if e.brands.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = e.brands.iter().map(String::as_str).collect();
            format!("  (refined: {})", names.join(", "))
        };
        println!("  {:<width$}  {}{}", e.name, show(&e.capabilities), refined);
    }
    println!("  {:<width$}  {}", "total", show(&fp.total));
    // RFC-0038: the grantable user-capability axis — bare policy tokens `main`
    // receives (e.g. `UiRoot`), carrying no host authority but reviewable as a
    // widening (a new package in the policy TCB / new library-effect authority).
    if !fp.user_caps.is_empty() {
        let names: Vec<&str> = fp.user_caps.iter().map(String::as_str).collect();
        println!("  {:<width$}  {}", "user caps", names.join(", "));
    }
    // The build axis (only when the rune ships a `build` step). Runtime authority
    // is enforced by the type system; the build footprint is the supply-chain
    // signal — what a rune's build step is allowed to do, outside the consumer's
    // type-checked call graph.
    if !fp.build.is_empty() {
        println!("Build-time footprint of {path}:");
        println!("  {:<width$}  {}", "build", show(&fp.build));
    }
    Ok(())
}

/// Print the derived Content-Security-Policy (RFC-0137) based on the program's capability footprint.
pub(crate) fn report_derived_csp(path: &str) -> Result<(), String> {
    let fp = analyze_file(path)?;
    let mut host_sources = Vec::new();
    let has_net = fp.total.contains_key("Net") || fp.total.contains_key("Fetch");
    let has_ui_fetch = fp.user_caps.iter().any(|c| c.contains("UiFetch") || c.contains("UiRoot"));
    if has_net || has_ui_fetch {
        host_sources.push("'self'");
    }
    let policy = witchy_confinement::Policy::default();
    let has_compartment = fp.user_caps.iter().any(|c| c.contains("Compartment"));
    let csp = policy.derive_full_csp(&host_sources, has_compartment)?;
    println!("{csp}");
    Ok(())
}

/// Compare the capability footprints of two versions of a module and report any
/// *widening* — host authority the newer version demands that the older did not.
/// Returns whether it widened so the caller can fail the supply-chain gate. This
/// is what makes `witchy` dependency updates auditable: unlike Go, where a
/// version bump can silently start touching the network, a widening is visible
/// and blockable here.
pub(crate) fn report_capability_diff(old_path: &str, new_path: &str) -> Result<bool, String> {
    let old = analyze_file(old_path)?;
    let new = analyze_file(new_path)?;
    let d = capability_model::diff(&old, &new);
    println!("Capability footprint diff {old_path} -> {new_path}:");
    println!("  old total:  {}", capability_model::show_caps(&old.total));
    println!("  new total:  {}", capability_model::show_caps(&new.total));
    println!("  added:      {}", capability_model::show_caps(&d.added));
    println!("  removed:    {}", capability_model::show_caps(&d.removed));
    if !old.build.is_empty() || !new.build.is_empty() {
        println!("  build old:  {}", capability_model::show_caps(&old.build));
        println!("  build new:  {}", capability_model::show_caps(&new.build));
        println!("  build +:    {}", capability_model::show_caps(&d.build_added));
        println!("  build -:    {}", capability_model::show_caps(&d.build_removed));
    }
    let join = |s: &std::collections::BTreeSet<String>| {
        if s.is_empty() {
            "(none)".to_string()
        } else {
            s.iter().cloned().collect::<Vec<_>>().join(", ")
        }
    };
    if !d.refinements_dropped.is_empty() || !d.refinements_gained.is_empty() {
        println!(
            "  refinements: dropped {} / gained {}",
            join(&d.refinements_dropped),
            join(&d.refinements_gained)
        );
    }
    if !old.user_caps.is_empty() || !new.user_caps.is_empty() {
        println!("  user caps +: {}", join(&d.user_caps_added));
        println!("  user caps -: {}", join(&d.user_caps_removed));
    }
    let mut flagged = false;
    if !d.user_caps_added.is_empty() {
        // A new grantable (user) capability carries no host authority, but it IS a
        // widening: `main` now receives a policy token it did not before, expanding
        // the policy TCB — and `FootprintDiff::widened` counts it, so the exit code
        // is 2. Surface it in the message too, so the two agree (BUG-314): previously
        // this printed "OK: no widening" yet exited 2.
        println!(
            "USER-CAP WIDENING: the newer version's `main` receives new grantable capabilities ({}). \
             They confer no host authority but widen the policy TCB — review before trusting.",
            join(&d.user_caps_added)
        );
        flagged = true;
    }
    if d.build_widened() {
        // The high-signal supply-chain event: build-time execution is outside the
        // consumer's type-checked call graph, so a new build cap is the thing the
        // gate must catch.
        println!(
            "BUILD WIDENING: the newer version's build step demands new build-time authority ({}). \
             It cannot run until you grant it (`--allow-build-cap` + a `[build.grants]` entry).",
            capability_model::show_caps(&d.build_added)
        );
        flagged = true;
    }
    if !d.added.is_empty() {
        println!(
            "WIDENING: the newer version demands new host authority ({}). Review before trusting.",
            capability_model::show_caps(&d.added)
        );
        flagged = true;
    }
    if !flagged {
        if !d.refinements_dropped.is_empty() {
            // Same authority on both axes, but a brand was dropped — a confined
            // capability loosened to its bare form. Not a widening, but an intent
            // change worth surfacing.
            println!(
                "OK on authority, but a refinement was dropped ({}): a confined capability loosened to its bare form. Worth a look.",
                join(&d.refinements_dropped)
            );
        } else {
            println!("OK: no widening — the newer version demands no new authority on either axis.");
        }
    }
    Ok(d.widened())
}

/// RFC-0013: cross-check a grant document against a program's computed footprint.
/// Returns `true` when there is an UNDER-grant (the fatal case): the code needs
/// authority the grant withholds, so the program would fail at the missing
/// capability anyway. An over-grant (authority the code never uses) only warns.
pub(crate) fn report_grant_check(prog_path: &str, grants_path: &str) -> Result<bool, String> {
    let footprint = analyze_file(prog_path)?;
    let doc_src = std::fs::read_to_string(grants_path)
        .map_err(|e| format!("cannot read `{grants_path}`: {e}"))?;
    let doc = grants::GrantDoc::parse(&doc_src)?;
    let grant = doc.cap_set();
    let check = grants::cross_check(&grant, &footprint.total);
    println!("Grant cross-check: `{grants_path}` vs the footprint of `{prog_path}`");
    println!("  code needs:  {}", capability_model::show_caps(&footprint.total));
    println!("  grant gives: {}", capability_model::show_caps(&grant));
    if check.clean() {
        println!("  OK: the grant matches what the code exercises exactly.");
    }
    if !check.over_grant.is_empty() {
        println!(
            "  WARN over-grant (authority the code never exercises): {}",
            capability_model::show_caps(&check.over_grant)
        );
    }
    if !check.under_grant.is_empty() {
        println!(
            "  ERROR under-grant (authority the code needs but the grant withholds): {}",
            capability_model::show_caps(&check.under_grant)
        );
    }
    // (RFC-0060/BUG-610) `SecretStore` is one capability, but each granted secret
    // carries its own reveal policy — and the cap-set axis cannot express that.
    // Print it per secret, so the cross-check reports which secrets the code could
    // read into guest memory rather than only that a store was granted.
    for (name, s) in &doc.secrets {
        println!(
            "  secret {name}: {}{}",
            s.from,
            capability_model::secret_reveal_suffix(s.sealed)
        );
    }
    Ok(!check.sufficient())
}

/// (RFC-0060/BUG-610) Compare two grant documents and fail when the newer one
/// LOOSENS a secret's reveal policy. The footprint axis cannot catch this: a
/// document that deletes `sealed = true` grants the same `SecretStore` and so
/// cross-checks as "matches exactly", while the program gains the authority to
/// read that secret's bytes. Returns `true` when a loosening was found.
pub(crate) fn report_grant_diff(old_path: &str, new_path: &str) -> Result<bool, String> {
    let read = |p: &str| -> Result<grants::GrantDoc, String> {
        let src = std::fs::read_to_string(p).map_err(|e| format!("cannot read `{p}`: {e}"))?;
        grants::GrantDoc::parse(&src)
    };
    let (old, new) = (read(old_path)?, read(new_path)?);
    println!("Grant diff: `{old_path}` -> `{new_path}`");
    let mut loosened: Vec<String> = Vec::new();
    let mut added_revealable: Vec<String> = Vec::new();
    for (name, new_secret) in &new.secrets {
        match old.secrets.get(name) {
            // Was sealed, now revealable: a strictly larger authority over the
            // same secret. This is the case the footprint axis cannot see.
            Some(old_secret) if old_secret.sealed && !new_secret.sealed => {
                loosened.push(name.clone());
            }
            Some(_) => {}
            // A brand-new revealable secret is also new read authority.
            None if !new_secret.sealed => added_revealable.push(name.clone()),
            None => {}
        }
    }
    if !loosened.is_empty() {
        println!(
            "  SECRET WIDENING (sealed dropped — the program can now reveal these): {}",
            loosened.join(", ")
        );
    }
    if !added_revealable.is_empty() {
        println!(
            "  SECRET WIDENING (new revealable secret): {}",
            added_revealable.join(", ")
        );
    }
    if loosened.is_empty() && added_revealable.is_empty() {
        println!("  OK: no secret reveal policy was loosened.");
    }
    Ok(!loosened.is_empty() || !added_revealable.is_empty())
}
