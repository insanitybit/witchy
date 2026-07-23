//! Sandbox & grant policy adapters: map declared/CLI grants to runtime
//! FsRights and run a file under the capability sandbox or an explicit grant
//! document. Extracted from the composition root.

use crate::commands::execution::run_linked_compiled;
use crate::{
    ast, capabilities, enforce_performance_modes, grants, link_file, linked_has_main, runtime,
    typeck,
};

fn fs_rights_from_type_args(args: &[ast::Type]) -> runtime::FsRights {
    if args.is_empty() {
        return runtime::FsRights::new(true, true);
    }
    let has = |needle: &str| {
        args.iter()
            .any(|a| matches!(a, ast::Type::Named(n, inner) if n == needle && inner.is_empty()))
    };
    runtime::FsRights::new(has("Read"), has("Write"))
}

pub(crate) fn main_fs_param_rights(linked: &ast::Module) -> (Vec<runtime::FsRights>, Vec<runtime::FsRights>) {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    if let Some(ast::Item::Function(main)) =
        linked.items.iter().find(|it| matches!(it, ast::Item::Function(f) if f.name == "main"))
    {
        for p in &main.params {
            match &p.ty {
                Some(ast::Type::Named(n, args)) if n == "Dir" => dirs.push(fs_rights_from_type_args(args)),
                Some(ast::Type::Named(n, args)) if n == "File" => files.push(fs_rights_from_type_args(args)),
                _ => {}
            }
        }
    }
    (dirs, files)
}

fn grant_fs_rights(label: &str, rights: &[String]) -> Result<runtime::FsRights, String> {
    let mut out = runtime::FsRights::new(false, false);
    for r in rights {
        match r.as_str() {
            "Read" => out.read = true,
            "Write" => out.write = true,
            other => return Err(format!("grant `{label}` names unknown filesystem right `{other}`")),
        }
    }
    Ok(out)
}

fn require_exact_fs_rights(
    label: &str,
    declared: runtime::FsRights,
    required: runtime::FsRights,
) -> Result<(), String> {
    if declared == required {
        Ok(())
    } else {
        Err(format!(
            "grant `{label}` rights {:?} do not match `main` parameter rights {:?}",
            declared, required
        ))
    }
}

/// Compile a program and run it in the WASM VM granted EXACTLY its computed
/// footprint, announcing the grant on stderr. The `Dir` root and `Net` allowlist
/// are host policy (the `--dir`/`--net` flags); the program's footprint decides
/// whether they are granted at all.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_file_sandboxed(
    path: &str,
    dir_roots: Vec<std::path::PathBuf>,
    file_grants: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
    fetch_origins: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
) -> Result<(Vec<String>, Option<i32>), String> {
    let (linked, stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    enforce_performance_modes(&linked, &stem)?;
    if !linked_has_main(&linked) {
        return Err(format!("`{path}` has no `main` to run"));
    }
    // The sandbox grants EXACTLY what a run gives `main` (see `run_grant`) — not the
    // whole-program union, so a verify-only program that imports `crypto` is not
    // forced to be handed a `Secret` it never binds.
    let grant = capabilities::run_grant(&linked);
    // (BUG-116/RFC-0005) A bare `Secret` is the root signing-key externref; a
    // named `--secret` populates a `SecretStore`, not the bare `Secret`.
    // Requiring `--signing-key` specifically stops a named value from being
    // revealed as the root key.
    if grant.contains_key("Secret") && signing_key.is_none() {
        return Err(format!(
            "`{path}` needs a root `Secret` (the signing key), but none was granted — provide `--signing-key <seed-file>` (a named `--secret`/`--secret-file` populates a `SecretStore`, not the bare `Secret`)"
        ));
    }
    eprintln!(
        "sandboxing `{path}` \u{2014} granted exactly: {}",
        capabilities::show_caps(&grant)
    );
    run_linked_compiled(&linked, dir_roots, file_grants, net_allow, fetch_origins, None, args, signing_key, named_secrets, Vec::new(), true, false)
}

/// Resolve a `[secrets]` entry's `from = "env:VAR"` to the secret bytes the host
/// holds. The grant document never carries the value — only where to fetch it.
fn resolve_secret_from(from: &str) -> Result<Vec<u8>, String> {
    grants::resolve_secret_provider(from).map_err(|error| format!("grant {error}"))
}

/// `witchy sandbox --grants app.grants.toml <prog.witchy>` (RFC-0013): run a
/// program against a grant document instead of individual flags. The grant is
/// cross-checked against the computed footprint — an over-request warns, an
/// under-grant aborts — and each `Dir`/`File` `main` parameter is bound to the
/// document entry of the SAME NAME (`[files].config` → the `config` parameter).
pub(crate) fn run_file_grants(
    path: &str,
    grants_path: &str,
    accept_grants: bool,
    args: Vec<String>,
) -> Result<(Vec<String>, Option<i32>), String> {
    use std::io::IsTerminal;
    let (linked, stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    enforce_performance_modes(&linked, &stem)?;
    let doc_src = std::fs::read_to_string(grants_path)
        .map_err(|e| format!("cannot read `{grants_path}`: {e}"))?;
    let doc = grants::GrantDoc::parse(&doc_src)?;

    // Cross-check the grant against what a RUN actually exercises — `main`'s own
    // parameter row (`run_grant`), not `analyze().total` (the whole-program union
    // over every public entry point). `total` includes a linked library's `pub fn`s
    // (a dep's `pub fn fetch(net)`, std `crypto.sign`'s `Secret`) that this run never
    // reaches, so cross-checking against it can reject a launch grant that is in fact
    // exactly what `main` receives — a wrongly-refused, valid grant (BUG-016).
    let needed = capabilities::run_grant(&linked);
    let check = grants::cross_check(&doc.cap_set(), &needed);
    if !check.over_grant.is_empty() {
        eprintln!(
            "warning: grant `{grants_path}` over-requests {} \u{2014} the code never exercises it",
            capabilities::show_caps(&check.over_grant)
        );
    }
    if !check.sufficient() {
        return Err(format!(
            "grant `{grants_path}` is insufficient: the code needs {} which the grant withholds",
            capabilities::show_caps(&check.under_grant)
        ));
    }

    // Bind each `Dir`/`File` `main` parameter to its same-named document entry, in
    // declaration order (the positional grant the runtime expects). `Net` is one
    // allowlist (all `[net]` addresses); secrets are named and host-resolved.
    let mut dir_roots: Vec<std::path::PathBuf> = Vec::new();
    let mut file_grants: Vec<std::path::PathBuf> = Vec::new();
    // RFC-0038: grantable-cap params, in declaration order, each field pulled from
    // its `[user_caps]` entry in the cap's field order (must match codegen's k / N).
    let grantable: std::collections::HashMap<&str, &ast::TypeDef> = linked
        .items
        .iter()
        .filter_map(|it| match it {
            ast::Item::Type(t) if t.grantable => Some((t.name.as_str(), t)),
            _ => None,
        })
        .collect();
    let mut user_cap_fields: Vec<Vec<String>> = Vec::new();
    let mut net_allow: Vec<String> = doc.net.values().flatten().cloned().collect();
    net_allow.sort();
    net_allow.dedup();
    let mut fetch_origins: Vec<String> = Vec::new();
    let mut env_allow: Option<Vec<String>> = None;
    let mut named_secrets: Vec<runtime::SecretGrant> = Vec::new();
    for (name, s) in &doc.secrets {
        // BUG-146: carry the document's `use-only` modifier through to the runtime
        // grant — otherwise a grant that declares a signing/TLS key as unrevealable
        // was silently lowered to a revealable secret (`crypto.reveal` would leak it).
        named_secrets.push(runtime::SecretGrant {
            name: name.clone(),
            bytes: resolve_secret_from(&s.from)?,
            use_only: s.use_only,
        });
    }
    if let Some(ast::Item::Function(main)) =
        linked.items.iter().find(|it| matches!(it, ast::Item::Function(f) if f.name == "main"))
    {
        for p in &main.params {
            match &p.ty {
                Some(ast::Type::Named(n, _)) if n == "File" => {
                    let g = doc.files.get(&p.name).ok_or_else(|| {
                        format!("grant `{grants_path}` has no `[files].{}` for `main` parameter `{}`", p.name, p.name)
                    })?;
                    let declared = grant_fs_rights(&format!("[files].{}", p.name), &g.rights)?;
                    let required = match &p.ty {
                        Some(ast::Type::Named(_, args)) => fs_rights_from_type_args(args),
                        _ => runtime::FsRights::new(false, false),
                    };
                    require_exact_fs_rights(&format!("[files].{}", p.name), declared, required)?;
                    file_grants.push(std::path::PathBuf::from(&g.path));
                }
                Some(ast::Type::Named(n, _)) if n == "Dir" => {
                    let g = doc.dirs.get(&p.name).ok_or_else(|| {
                        format!("grant `{grants_path}` has no `[dirs].{}` for `main` parameter `{}`", p.name, p.name)
                    })?;
                    let declared = grant_fs_rights(&format!("[dirs].{}", p.name), &g.rights)?;
                    let required = match &p.ty {
                        Some(ast::Type::Named(_, args)) => fs_rights_from_type_args(args),
                        _ => runtime::FsRights::new(false, false),
                    };
                    require_exact_fs_rights(&format!("[dirs].{}", p.name), declared, required)?;
                    dir_roots.push(std::path::PathBuf::from(&g.root));
                }
                Some(ast::Type::Named(n, _)) if n == "Fetch" => {
                    if !fetch_origins.is_empty() {
                        return Err(
                            "grant documents currently support one `Fetch` parameter per entrypoint"
                                .to_string(),
                        );
                    }
                    let origins = doc.fetch.get(&p.name).ok_or_else(|| {
                        format!(
                            "grant `{grants_path}` has no `[fetch].{}` for `main` parameter `{}`",
                            p.name, p.name
                        )
                    })?;
                    witchy_runtime::fetch::FetchPolicy::allow(origins.clone()).map_err(
                        |error| {
                            format!(
                                "grant `[fetch].{}` contains an invalid origin: {error}",
                                p.name
                            )
                        },
                    )?;
                    fetch_origins = origins.clone();
                }
                Some(ast::Type::Named(n, _)) if n == "Env" => {
                    if env_allow.is_some() {
                        return Err(
                            "grant documents currently support one `Env` parameter per entrypoint"
                                .to_string(),
                        );
                    }
                    let names = doc.env.get(&p.name).ok_or_else(|| {
                        format!(
                            "grant `{grants_path}` has no `[env].{}` for `main` parameter `{}`",
                            p.name, p.name
                        )
                    })?;
                    let mut names = names.clone();
                    names.sort();
                    names.dedup();
                    env_allow = Some(names);
                }
                Some(ast::Type::Named(n, _)) if grantable.contains_key(n.as_str()) => {
                    // RFC-0038: a bare grantable cap — pull each policy field from the
                    // `[user_caps]` entry in the cap's declared field order.
                    let uc = doc.user_caps.get(&p.name).ok_or_else(|| {
                        format!("grant `{grants_path}` has no `[user_caps].{}` for `main` parameter `{}` (type `{n}`)", p.name, p.name)
                    })?;
                    let t = grantable[n.as_str()];
                    let names: &[String] = t.variants.first().map(|v| v.field_names.as_slice()).unwrap_or(&[]);
                    let mut vals = Vec::with_capacity(names.len());
                    for fname in names {
                        let v = uc.fields.get(fname).ok_or_else(|| {
                            format!("`[user_caps].{}` is missing field `{fname}` required by `{n}`", p.name)
                        })?;
                        let s = v.as_str().ok_or_else(|| {
                            format!("`[user_caps].{}` field `{fname}` must be a string", p.name)
                        })?;
                        vals.push(s.to_string());
                    }
                    user_cap_fields.push(vals);
                }
                _ => {}
            }
        }
    }
    // Show exactly what `main` will receive — a reviewable diff — then, unless
    // pre-accepted (`--accept-grants`) or non-interactive, require confirmation
    // before handing the authority over (RFC-0013's approval step).
    eprintln!("grant `{grants_path}` for `{path}` confers:");
    eprintln!("  capabilities: {}", capabilities::show_caps(&doc.cap_set()));
    for (name, g) in &doc.dirs {
        eprintln!("  dir    {name}: {}", g.root);
    }
    for (name, g) in &doc.files {
        eprintln!("  file   {name}: {}", g.path);
    }
    if !net_allow.is_empty() {
        eprintln!("  net:   {}", net_allow.join(", "));
    }
    for (name, origins) in &doc.fetch {
        eprintln!("  fetch  {name}: {}", origins.join(", "));
    }
    for (name, names) in &doc.env {
        eprintln!("  env    {name}: {}", names.join(", "));
    }
    for (name, s) in &doc.secrets {
        eprintln!("  secret {name}: {}", s.from);
    }
    if !accept_grants && std::io::stdin().is_terminal() {
        use std::io::Write;
        eprint!("Approve and run? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| format!("reading confirmation: {e}"))?;
        if !matches!(line.trim().chars().next(), Some('y' | 'Y')) {
            return Err("grant not approved \u{2014} aborting".to_string());
        }
    }
    // Secrets reach the program by name through the `SecretStore` (`require`/`get`);
    // the bare root `Secret` (`--signing-key`) is not granted via documents here.
    run_linked_compiled(&linked, dir_roots, file_grants, net_allow, fetch_origins, env_allow, args, None, named_secrets, user_cap_fields, true, false)
}
