use std::{collections::BTreeSet, fs, path::{Path, PathBuf}};

use syn::visit::Visit;
use syn::{ExprStruct, Fields, FnArg, Item, ItemFn, ItemStruct, Path as SynPath, Type, Visibility};

fn manifest_dir() -> PathBuf {
    PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for tests"),
    )
}

fn read_source(relative: &str) -> String {
    let path = manifest_dir().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The private canonical backend: `canonical_wasmir.rs` and the sections split
/// out of it into child files. Its other child modules are separate kernels.
const CANONICAL_BACKEND_FILES: &[&str] = &[
    "canonical_wasmir.rs",
    "canonical_wasmir/lowering.rs",
    "canonical_wasmir/correspondence.rs",
    "canonical_wasmir/emit.rs",
    "canonical_wasmir/structured.rs",
    "canonical_wasmir/plan.rs",
    "canonical_wasmir/plan_match.rs",
];

fn functions<'a>(file: &'a syn::File) -> impl Iterator<Item = &'a ItemFn> {
    file.items.iter().filter_map(|item| match item {
        Item::Fn(function) => Some(function),
        _ => None,
    })
}

fn type_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Reference(reference) => type_name(&reference.elem),
        Type::Path(path) => path.path.segments.last().map(|segment| segment.ident.to_string()),
        _ => None,
    }
}

fn takes(function: &ItemFn, name: &str) -> bool {
    function.sig.inputs.iter().any(|argument| match argument {
        FnArg::Typed(argument) => type_name(&argument.ty).as_deref() == Some(name),
        FnArg::Receiver(_) => false,
    })
}

fn is_validated_artifact_admission(function: &ItemFn) -> bool {
    let test_helper = function.sig.ident == "admit_canonical"
        && cfg_text(function) == "test";
    takes(function, "ValidatedSafeWir")
        && (test_helper || (matches!(function.sig.ident.to_string().as_str(),
            "admit_canonical" | "admit_canonical_with_observer")
            && takes(function, "CanonicalSourceView")))
}

fn cfg_text(function: &ItemFn) -> String {
    function
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("cfg"))
        .filter_map(|attribute| match &attribute.meta {
            syn::Meta::List(list) => Some(list.tokens.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_cfg(function: &ItemFn, text: &str) -> bool {
    cfg_text(function).contains(text)
}

fn is_public(function: &ItemFn) -> bool {
    matches!(function.vis, Visibility::Public(_))
}

fn named<'a>(file: &'a syn::File, name: &str) -> &'a ItemFn {
    functions(file)
        .find(|function| function.sig.ident == name)
        .unwrap_or_else(|| panic!("missing encoder entrypoint {name}"))
}

#[derive(Default)]
struct CalledFunctions(BTreeSet<String>);

impl Visit<'_> for CalledFunctions {
    fn visit_expr_call(&mut self, expression: &syn::ExprCall) {
        if let syn::Expr::Path(path) = expression.func.as_ref()
            && let Some(segment) = path.path.segments.last()
        {
            self.0.insert(segment.ident.to_string());
        }
        syn::visit::visit_expr_call(self, expression);
    }
}

fn called_functions(file: &syn::File) -> BTreeSet<String> {
    let mut calls = CalledFunctions::default();
    calls.visit_file(file);
    calls.0
}

fn rust_sources(root: &Path) -> Vec<(PathBuf, String)> {
    fn collect(directory: &Path, files: &mut Vec<(PathBuf, String)>) {
        for entry in fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        {
            let entry = entry.unwrap_or_else(|error| panic!("read directory entry: {error}"));
            let path = entry.path();
            if path.is_dir() {
                collect(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let source = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                files.push((path, source));
            }
        }
    }

    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

// These audits inspect named identifiers, not arbitrary Rust syntax. Avoid a
// full AST for unrelated ASCII files. Raw identifiers retain their spelling
// after `r#`; comments/string matches only cause harmless extra parsing. Let
// syn handle non-ASCII or escape-bearing source without assumptions about
// identifier normalization or future syntax. Compilation remains the syntax
// check for files with none of the protected names.
fn may_name_any(source: &str, names: &[&str]) -> bool {
    !source.is_ascii() || source.contains('\\') || names.iter().any(|name| source.contains(name))
}

#[test]
fn source_surface_prefilter_keeps_raw_unicode_and_escape_candidates() {
    let names = ["WasmIr", "ValidatedSafeWir"];
    for source in [
        "struct WasmIr {}",
        "struct r#ValidatedSafeWir {}",
        "fn make() { let _ = crate::ValidatedSafeWir {}; }",
        "// WasmIr in a comment is a conservative false positive\nfn unrelated() {}",
        "const TEXT: &str = \"ValidatedSafeWir\";",
        "struct Café {}",
        "const TEXT: &str = \"\\u{57}asmIr\";",
    ] {
        syn::parse_file(source).expect("valid Rust fixture");
        assert!(may_name_any(source, &names), "candidate must reach syn: {source}");
    }
    let escaped_identifier = r"struct \u{57}asmIr {}";
    assert!(may_name_any(escaped_identifier, &names));
    assert!(syn::parse_file(escaped_identifier).is_err(),
        "identifier escapes must reach the parser, not disappear in the prefilter");
    assert!(!may_name_any("struct Unrelated { field: u32 }", &names));
}

fn path_ends_in(path: &SynPath, name: &str) -> bool {
    path.segments.last().is_some_and(|segment| segment.ident == name)
}

#[derive(Default)]
struct WasmIrSurface {
    references: usize,
    declarations: Vec<ItemStruct>,
    constructions: Vec<(String, bool, bool)>,
    current_function: Option<(String, bool, bool)>,
}

#[derive(Default)]
struct ValidatedSafeWirSurface {
    declarations: Vec<ItemStruct>,
    constructions: Vec<String>,
    current_function: Option<String>,
}

impl<'ast> Visit<'ast> for ValidatedSafeWirSurface {
    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        if item.ident == "ValidatedSafeWir" {
            self.declarations.push(item.clone());
        }
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_expr_struct(&mut self, expression: &'ast ExprStruct) {
        if path_ends_in(&expression.path, "ValidatedSafeWir") {
            self.constructions.push(
                self.current_function
                    .clone()
                    .unwrap_or_else(|| "<item initializer>".to_string()),
            );
        }
        syn::visit::visit_expr_struct(self, expression);
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        let previous = self.current_function.replace(function.sig.ident.to_string());
        syn::visit::visit_item_fn(self, function);
        self.current_function = previous;
    }
}

impl<'ast> Visit<'ast> for WasmIrSurface {
    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        if item.ident == "WasmIr" {
            self.references += 1;
            self.declarations.push(item.clone());
        }
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_type_path(&mut self, path: &'ast syn::TypePath) {
        if path_ends_in(&path.path, "WasmIr") {
            self.references += 1;
        }
        syn::visit::visit_type_path(self, path);
    }

    fn visit_expr_struct(&mut self, expression: &'ast ExprStruct) {
        if path_ends_in(&expression.path, "WasmIr") {
            self.references += 1;
            let (function, is_public, takes_validated) = self
                .current_function
                .clone()
                .unwrap_or_else(|| ("<item initializer>".to_string(), true, false));
            self.constructions.push((function, is_public, takes_validated));
        }
        syn::visit::visit_expr_struct(self, expression);
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        let context = (
            function.sig.ident.to_string(),
            is_public(function),
            takes(function, "ValidatedSafeWir"),
        );
        let previous = self.current_function.replace(context);
        syn::visit::visit_item_fn(self, function);
        self.current_function = previous;
    }
}

#[test]
fn canonical_wasmir_has_one_private_validated_construction_surface() {
    let manifest_dir = manifest_dir();
    let source_root = manifest_dir.join("src");
    let backend = CANONICAL_BACKEND_FILES
        .iter()
        .map(|file| source_root.join(file))
        .collect::<Vec<_>>();
    let mut canonical_surface = WasmIrSurface::default();

    for (path, source) in rust_sources(&source_root) {
        if !may_name_any(&source, &["WasmIr"]) {
            continue;
        }
        let file = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} parses: {error}", path.display()));
        let mut surface = WasmIrSurface::default();
        surface.visit_file(&file);
        if backend.contains(&path) {
            canonical_surface.references += surface.references;
            canonical_surface.declarations.extend(surface.declarations);
            canonical_surface.constructions.extend(surface.constructions);
        } else {
            assert_eq!(
                surface.references, 0,
                "WasmIr must not be referenced outside the private canonical backend: {}",
                path.display()
            );
        }
    }

    assert_eq!(canonical_surface.declarations.len(), 1, "WasmIr must have one declaration");
    let declaration = &canonical_surface.declarations[0];
    assert!(matches!(declaration.vis, Visibility::Inherited));
    if let Fields::Named(fields) = &declaration.fields {
        assert!(
            fields.named.iter().all(|field| matches!(field.vis, Visibility::Inherited)),
            "WasmIr fields must remain private"
        );
    } else {
        panic!("WasmIr must retain its named private fields");
    }

    assert_eq!(canonical_surface.constructions.len(), 1, "WasmIr must have one construction site");
    let (function, is_public, takes_validated) = &canonical_surface.constructions[0];
    assert!(!is_public, "WasmIr construction site `{function}` must be private");
    assert!(
        *takes_validated,
        "WasmIr construction site `{function}` must directly accept ValidatedSafeWir"
    );
}

#[test]
fn validated_safewir_has_one_checker_owned_construction_surface() {
    let source_root = manifest_dir().join("src");
    let authority = source_root.join("canonical_safewir/authority.rs");
    let mut declarations = Vec::new();
    let mut constructions = Vec::new();

    for (path, source) in rust_sources(&source_root) {
        if !may_name_any(&source, &["ValidatedSafeWir"]) {
            continue;
        }
        let file = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} parses: {error}", path.display()));
        let mut surface = ValidatedSafeWirSurface::default();
        surface.visit_file(&file);
        declarations.extend(surface.declarations.into_iter().map(|item| (path.clone(), item)));
        constructions.extend(
            surface
                .constructions
                .into_iter()
                .map(|function| (path.clone(), function)),
        );
    }

    assert_eq!(declarations.len(), 1, "ValidatedSafeWir must have one declaration");
    let (declaration_path, declaration) = &declarations[0];
    assert_eq!(declaration_path, &authority, "validated authority belongs to the checker");
    let Fields::Named(fields) = &declaration.fields else {
        panic!("ValidatedSafeWir must retain named private fields");
    };
    assert!(
        fields.named.iter().all(|field| matches!(field.vis, Visibility::Inherited)),
        "ValidatedSafeWir fields must remain private"
    );

    assert_eq!(
        constructions,
        vec![
            (authority.clone(), "validate_final_inner".to_string()),
            (authority, "validate_optimized_final_inner".to_string()),
        ],
        "only zero-pass and relation-bound final checking may construct ValidatedSafeWir"
    );
}

#[test]
fn a3_witness_and_digest_types_have_no_public_minting_surface() {
    let source_root = manifest_dir().join("src");
    let mut declarations = BTreeSet::new();
    let protected_public_types = [
        "CheckedLoweringWitness",
        "CheckedInitialSafeWir",
        "CheckedOptimizedSafeWir",
        "ValidatedSafeWir",
    ];
    let protected_private_types = [
        "CheckedSourceWitness",
        "CheckedOptimizationWitness",
        "FinalSafeWirWitness",
        "ExactModuleWitness",
    ];

    for (path, source) in rust_sources(&source_root) {
        if !may_name_any(&source, &protected_public_types)
            && !may_name_any(&source, &protected_private_types)
        {
            continue;
        }
        let file = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} parses: {error}", path.display()));
        for item in &file.items {
            let Item::Struct(item) = item else { continue };
            let name = item.ident.to_string();
            if protected_public_types.contains(&name.as_str()) {
                assert!(
                    matches!(item.vis, Visibility::Public(_)),
                    "{name} must remain publicly usable only through checker APIs"
                );
                let Fields::Named(fields) = &item.fields else {
                    panic!("{name} must keep named private fields");
                };
                assert!(
                    fields.named.iter().all(|field| matches!(field.vis, Visibility::Inherited)),
                    "{name} fields must not be publicly constructible"
                );
                declarations.insert(name.clone());
            }
            if protected_private_types.contains(&name.as_str()) {
                assert!(
                    !matches!(item.vis, Visibility::Public(_)),
                    "{name} must not be externally visible"
                );
                declarations.insert(name);
            }
        }
    }

    for name in protected_public_types.into_iter().chain(protected_private_types) {
        assert!(declarations.contains(name), "missing protected A3 type {name}");
    }

    let history = read_source("src/canonical_safewir/history.rs");
    let history_file = syn::parse_file(&history).expect("history.rs parses");
    let module_digest_impl_has_public_constructor = history_file.items.iter().any(|item| {
        let Item::Impl(item) = item else { return false };
        let syn::Type::Path(self_ty) = item.self_ty.as_ref() else { return false };
        self_ty.path.is_ident("ModuleDigest")
            && item.items.iter().any(|item| {
                let syn::ImplItem::Fn(function) = item else { return false };
                function.sig.ident == "from_bytes"
                    && matches!(function.vis, Visibility::Public(_))
            })
    });
    assert!(
        !module_digest_impl_has_public_constructor,
        "ModuleDigest must not expose a public constructor"
    );
}

fn workspace_manifests() -> Vec<PathBuf> {
    let root = manifest_dir().join("../..");
    let mut manifests = vec![root.join("Cargo.toml")];
    let crates = root.join("crates");
    for entry in fs::read_dir(&crates).unwrap_or_else(|error| panic!("read {}: {error}", crates.display())) {
        let entry = entry.unwrap_or_else(|error| panic!("read workspace crate: {error}"));
        let manifest = entry.path().join("Cargo.toml");
        if manifest.is_file() {
            manifests.push(manifest);
        }
    }
    manifests.sort();
    manifests
}

#[test]
fn raw_encoder_feature_is_dev_only_in_the_workspace_feature_tree() {
    let mut uses = 0;
    for manifest in workspace_manifests() {
        let source = fs::read_to_string(&manifest)
            .unwrap_or_else(|error| panic!("read {}: {error}", manifest.display()));
        let mut section = String::new();
        for (line_number, line) in source.lines().enumerate() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.starts_with('[') && line.ends_with(']') {
                section = line.to_string();
            }
            if line.contains("\"raw-encoder-test-api\"") {
                uses += 1;
                assert!(
                    section.contains("dev-dependencies"),
                    "raw-encoder-test-api must be reachable only through dev-dependencies: {}:{} ({section})",
                    manifest.display(),
                    line_number + 1
                );
            }
        }
    }
    assert!(uses > 0, "workspace must retain an explicit dev-only raw encoder test feature edge");
}

// The synthetic feature is an explicit opt-in, never an edge from a normal
// dependency/default feature. Fail closed on unrecognized activation sites.
// One test-only feature may forward to it: the runtime's `self-checks`, which
// only the root package's dev-dependency enables. `self-checks` is itself
// pinned, so no production edge can reach it.
fn production_test_feature_violations(source: &str, owner: &str) -> Vec<usize> {
    let mut section = "";
    let mut violations = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line;
            continue;
        }
        let features = line.split(['"', '\'']).skip(1).step_by(2)
            .filter(|value| matches!(value.rsplit('/').next(),
                Some("raw-encoder-test-api" | "raw-module-test-api" | "legacy-encoder-bridge"
                    | "checked-source-test-api" | "self-checks")))
            .collect::<Vec<_>>();
        if features.is_empty() || section.contains("dev-dependencies") {
            continue;
        }
        let key = line.split_once('=').map(|(key, _)| key.trim());
        let test_forward = section == "[features]" && match (owner, key) {
            ("witchy-runtime", Some("self-checks")) =>
                features.iter().all(|feature| feature.starts_with("witchy-wir/")),
            _ => false,
        };
        if !test_forward {
            violations.push(index + 1);
        }
    }
    violations
}

#[test]
fn production_features_do_not_activate_legacy_encoding() {
    for manifest in workspace_manifests() {
        let source = fs::read_to_string(&manifest).expect("read manifest");
        let manifest = manifest.canonicalize().unwrap();
        let owner = manifest.parent().and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str()).unwrap_or_default();
        let violations = production_test_feature_violations(&source, owner);
        assert!(violations.is_empty(),
            "production test/bridge feature activation in {} at {violations:?}", manifest.display());
    }
}

#[test]
fn production_feature_pin_rejects_dependency_and_default_activation() {
    for source in [
        "[dependencies]\nwitchy-wir = { features = [\"legacy-encoder-bridge\"] }\n",
        "[dependencies]\nwitchy-lower = { features = [\"raw-module-test-api\"] }\n",
        "[features]\ndefault = [\"raw-module-test-api\"]\n",
        "[features]\nnative = [\"witchy-wir/legacy-encoder-bridge\"]\n",
        "[features]\ndefault = [\"alias\"]\nalias = [\"witchy-lower/raw-module-test-api\"]\n",
        "[dependencies]\nwitchy-wir = { features = [\"checked-source-test-api\"] }\n",
        "[dependencies]\nwitchy-runtime = { features = [\"self-checks\"] }\n",
        "[features]\nnative = [\"witchy-runtime/self-checks\"]\n",
    ] {
        assert!(!production_test_feature_violations(source, "witchy-lower").is_empty(), "{source}");
    }
    assert!(!production_test_feature_violations(
        "[features]\nself-checks = [\"witchy-wir/raw-encoder-test-api\"]\n", "witchy-caps",
    ).is_empty());
    assert!(production_test_feature_violations(
        "[features]\nself-checks = [\"native\", \"witchy-wir/raw-encoder-test-api\"]\n",
        "witchy-runtime",
    ).is_empty());
    assert!(production_test_feature_violations(
        "[dev-dependencies]\nwitchy-runtime = { features = [\"self-checks\"] }\n",
        "witchy",
    ).is_empty());
}

#[test]
fn emit_wat_uses_canonical_admission_before_diagnostic_printing() {
    let compile = syn::parse_file(&read_source("../../src/commands/compile.rs"))
        .expect("CLI compiler module parses");
    let mut calls = CalledFunctions::default();
    calls.visit_block(&named(&compile, "emit_wat_file").block);
    assert_eq!(calls.0, BTreeSet::from([
        "link_file_checked".into(), "enforce_performance_modes".into(),
        "compile_checked_module_artifact".into(), "print_admitted_wat".into(),
    ]));
    let mut printer_calls = CalledFunctions::default();
    printer_calls.visit_block(&named(&compile, "print_admitted_wat").block);
    assert!(printer_calls.0.contains("print_bytes"));
    assert!(!printer_calls.0.contains("to_wat"));
}

#[test]
fn canonical_wasm_encoding_is_private_and_requires_final_authority() {
    let lib = syn::parse_file(&read_source("src/lib.rs")).expect("lib.rs parses");
    let canonical_module = lib
        .items
        .iter()
        .find_map(|item| match item {
            Item::Mod(module) if module.ident == "canonical_wasmir" => Some(module),
            _ => None,
        })
        .expect("canonical WasmIR module is declared");
    assert!(matches!(canonical_module.vis, Visibility::Inherited));

    let source = read_source("src/canonical_wasmir.rs");
    let file = syn::parse_file(&source).expect("canonical_wasmir.rs parses");
    let encoder = named(&file, "encode_validated");
    assert!(matches!(encoder.vis, Visibility::Inherited));
    assert!(takes(encoder, "ValidatedSafeWir"));
    for name in ["admit_canonical", "admit_canonical_with_observer"] {
        let admission = named(&file, name);
        assert!(is_public(admission));
        assert!(is_validated_artifact_admission(admission), "{name}");
    }
    assert!(
        functions(&file).all(|function| {
            !is_public(function) || is_validated_artifact_admission(function)
        }),
        "canonical WasmIR must expose only validated artifact admission"
    );
}

#[test]
fn canonical_admission_surface_pin_rejects_unvalidated_and_raw_routes() {
    for name in ["admit_canonical", "admit_canonical_with_observer"] {
        let valid: ItemFn = syn::parse_str(&format!(
            "pub fn {name}(module: ValidatedSafeWir, source: &CanonicalSourceView<'_>) {{}}"
        )).unwrap();
        assert!(is_validated_artifact_admission(&valid));
        for parameters in [
            "module: Candidate, source: &CanonicalSourceView<'_>",
            "module: ValidatedSafeWir, source: SourceBundleDigest",
            "module: WasmIr",
        ] {
            let invalid: ItemFn = syn::parse_str(&format!("pub fn {name}({parameters}) {{}}"))
                .unwrap();
            assert!(!is_validated_artifact_admission(&invalid), "{name}: {parameters}");
        }
    }
    let raw: ItemFn = syn::parse_str(
        "pub fn encode_raw(module: ValidatedSafeWir, source: &CanonicalSourceView<'_>) {}"
    ).unwrap();
    assert!(!is_validated_artifact_admission(&raw));
    let test_helper: ItemFn = syn::parse_str(
        "#[cfg(test)] pub fn admit_canonical(module: ValidatedSafeWir) {}"
    ).unwrap();
    assert!(is_validated_artifact_admission(&test_helper));
    for configuration in ["not(test)", "any(test, feature = \"native\")"] {
        let production: ItemFn = syn::parse_str(&format!(
            "#[cfg({configuration})] pub fn admit_canonical(module: ValidatedSafeWir) {{}}"
        )).unwrap();
        assert!(!is_validated_artifact_admission(&production), "{configuration}");
    }
}

#[test]
fn production_encoding_requires_checked_wasm_ir_authority() {
    let source = read_source("src/canonical_wasmir.rs");
    let file = syn::parse_file(&source).expect("canonical_wasmir.rs parses");

    let checked = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Struct(structure) if structure.ident == "CheckedWasmIr" => Some(structure),
            _ => None,
        })
        .expect("canonical backend must declare CheckedWasmIr");
    assert!(matches!(checked.vis, Visibility::Inherited));

    for name in ["encode_checked", "encode_checked_unvalidated"] {
        let encoder = named(&file, name);
        assert!(matches!(encoder.vis, Visibility::Inherited), "{name} must stay private");
        assert!(takes(encoder, "CheckedWasmIr"), "{name} must take checked WasmIR authority");
        assert!(
            !takes(encoder, "ValidatedSafeWir"),
            "production encoding must not accept SafeWIR authority directly ({name})"
        );
    }

    let admission = named(&file, "admit_canonical");
    let mut wrapper_calls = CalledFunctions::default();
    wrapper_calls.visit_item_fn(admission);
    assert_eq!(wrapper_calls.0, BTreeSet::from(["admit_canonical_with_observer".into()]),
        "ordinary admission must use the same checked admission path as observation");
    let admission = named(&file, "admit_canonical_with_observer");
    let mut calls = CalledFunctions::default();
    calls.visit_item_fn(admission);
    assert!(
        calls.0.contains("encode_checked_unvalidated"),
        "canonical admission must encode only checked WasmIR authority"
    );
    assert!(
        !calls.0.contains("encode_validated"),
        "canonical admission must not call the test-only SafeWIR encoder helper"
    );

    #[derive(Default)]
    struct AdmissionStages(Vec<String>);
    impl Visit<'_> for AdmissionStages {
        fn visit_expr_call(&mut self, expression: &syn::ExprCall) {
            if let syn::Expr::Path(path) = expression.func.as_ref()
                && let Some(segment) = path.path.segments.last()
            {
                let name = segment.ident.to_string();
                if matches!(name.as_str(), "derive_entry" | "authorize_wasm_ir" |
                    "encode_checked" | "encode_checked_unvalidated" | "decode_and_match_plan"
                    | "admit_parts" | "observe")
                {
                    self.0.push(name);
                }
            }
            syn::visit::visit_expr_call(self, expression);
        }
    }
    let mut stages = AdmissionStages::default();
    stages.visit_block(&admission.block);
    // Admission encodes without the encoder-side validation because
    // `decode_and_match_plan` validates these exact bytes next; that ordering
    // is what keeps every admitted module validated.
    assert_eq!(stages.0, ["derive_entry", "authorize_wasm_ir", "encode_checked_unvalidated",
        "decode_and_match_plan", "admit_parts", "observe"],
        "observation follows checked encoding, validated decoded-byte matching, and artifact admission");

    let direct_safe_wir_encoders = functions(&file)
        .filter(|function| function.sig.ident.to_string().contains("encode"))
        .filter(|function| takes(function, "ValidatedSafeWir"))
        .collect::<Vec<_>>();
    assert_eq!(
        direct_safe_wir_encoders.len(),
        1,
        "only one test helper may accept ValidatedSafeWir directly"
    );
    let test_helper = direct_safe_wir_encoders[0];
    assert_eq!(test_helper.sig.ident, "encode_validated");
    assert!(
        has_cfg(test_helper, "test"),
        "direct SafeWIR encoding must remain test-only"
    );
}

#[test]
fn canonical_capabilities_lower_only_to_externref() {
    let source = CANONICAL_BACKEND_FILES
        .iter()
        .map(|file| read_source(&format!("src/{file}")))
        .collect::<String>();

    assert!(
        source.contains("ValueType::Capability(_) => Ok(ValType::EXTERNREF)"),
        "canonical capability values must be represented as Wasm externref"
    );
    assert!(
        source.contains("ValueType::NullableCapability(_) => Ok(ValType::EXTERNREF)"),
        "canonical nullable capability values must be represented as Wasm externref"
    );
    assert!(
        source.contains("HostParameterRole::Capability(_) => parameters.push(ValType::EXTERNREF)"),
        "canonical capability parameters must be represented as Wasm externref"
    );
    assert!(
        source.contains("HostResultRole::Capability(_) | HostResultRole::NullableCapability(_) => results.push(ValType::EXTERNREF)"),
        "canonical capability and nullable capability results must be represented as Wasm externref"
    );
    for integer_type in ["I32", "I64", "F32", "F64", "V128"] {
        for role in ["Capability", "NullableCapability"] {
            assert!(
                !source.contains(&format!("ValueType::{role}(_) => Ok(ValType::{integer_type})")),
                "canonical {role} values must not map to ValType::{integer_type}"
            );
        }
    }
}
