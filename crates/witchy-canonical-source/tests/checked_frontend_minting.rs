//! RFC-0170: `CheckedSourceWitness` and `CanonicalSourceView` live in this
//! contract crate so `witchy-wir` need not depend on the checker, which means
//! Rust visibility can no longer confine their constructors to
//! `witchy-types`. This test does: every call of a checked-frontend-only
//! constructor across the workspace must be at its one pinned site, and both
//! types must keep private fields so no struct literal can bypass it.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use syn::visit::Visit;
use syn::{Fields, Item, Visibility};

const FRONTEND_ONLY: [(&str, &str); 3] = [
    ("mint_for_checked_frontend", "crates/witchy-types/src/pipeline.rs"),
    ("rebind_launch_contract_for_build_recipe", "crates/witchy-types/src/pipeline/build_recipe.rs"),
    ("from_checked_frontend", "crates/witchy-types/src/pipeline.rs"),
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("workspace root")
}

/// Every tracked Rust file: crates, the root package, benches, tools and
/// build scripts alike. Untracked scratch files cannot reach the gate.
fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let output = Command::new("git").arg("-C").arg(root).args(["ls-files", "-z", "--", "*.rs"])
        .output().expect("git ls-files runs");
    assert!(output.status.success(), "git ls-files: {}", String::from_utf8_lossy(&output.stderr));
    output.stdout.split(|byte| *byte == 0).filter(|path| !path.is_empty())
        .map(|path| root.join(String::from_utf8_lossy(path).as_ref())).collect()
}

/// syn does not parse macro bodies, so a constructor named inside `vec![..]`
/// or `assert!(..)` is found by scanning the raw token stream.
fn token_idents(tokens: proc_macro2::TokenStream, names: &mut Vec<String>) {
    for token in tokens {
        match token {
            proc_macro2::TokenTree::Ident(ident) => names.push(ident.to_string()),
            proc_macro2::TokenTree::Group(group) => token_idents(group.stream(), names),
            _ => {}
        }
    }
}

#[derive(Default)]
struct Calls(Vec<String>);

impl<'ast> Visit<'ast> for Calls {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = call.func.as_ref()
            && let Some(segment) = path.path.segments.last()
        {
            self.0.push(segment.ident.to_string());
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.0.push(call.method.to_string());
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        // A constructor passed as a function value is a call site too.
        if let Some(segment) = path.path.segments.last() {
            self.0.push(segment.ident.to_string());
        }
        syn::visit::visit_expr_path(self, path);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        token_idents(mac.tokens.clone(), &mut self.0);
        syn::visit::visit_macro(self, mac);
    }
}

#[test]
fn checked_source_authority_has_one_frontend_minting_site() {
    let root = workspace_root();
    let files = rust_sources(&root);
    let mut sites = BTreeSet::new();
    for path in &files {
        let source = fs::read_to_string(path).expect("read source");
        // A call names its constructor, so only files that mention one need
        // the (slow, unoptimized) full parse.
        if !FRONTEND_ONLY.iter().any(|(constructor, _)| source.contains(constructor)) {
            continue;
        }
        let file = syn::parse_file(&source).unwrap_or_else(|error| panic!("{} parses: {error}", path.display()));
        let mut calls = Calls::default();
        calls.visit_file(&file);
        let relative = path.strip_prefix(&root).expect("inside workspace").to_string_lossy().replace('\\', "/");
        for (constructor, _) in FRONTEND_ONLY {
            if calls.0.iter().any(|call| call == constructor) {
                sites.insert((constructor, relative.clone()));
            }
        }
    }
    let expected = FRONTEND_ONLY.iter().map(|(constructor, site)| (*constructor, site.to_string())).collect::<BTreeSet<_>>();
    assert_eq!(sites, expected, "a checked-frontend-only constructor gained a call site");
}

#[test]
fn checked_source_authority_types_keep_private_fields() {
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/source.rs")).expect("source.rs");
    let file = syn::parse_file(&source).expect("source.rs parses");
    let mut seen = BTreeSet::new();
    for item in &file.items {
        let Item::Struct(item) = item else { continue };
        let name = item.ident.to_string();
        if name == "CheckedSourceWitness" || name == "CanonicalSourceView" {
            let Fields::Named(fields) = &item.fields else { panic!("{name} keeps named fields") };
            assert!(fields.named.iter().all(|field| matches!(field.vis, Visibility::Inherited)),
                "{name} fields must stay private so only its pinned constructor builds one");
            seen.insert(name);
        }
    }
    assert_eq!(seen.len(), 2, "both checked-source authority types are declared in source.rs");
}
