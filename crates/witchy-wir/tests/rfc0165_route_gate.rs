use std::{collections::BTreeSet, fs, path::{Path, PathBuf}};

use syn::visit::Visit;
use syn::{ExprStruct, Fields, FnArg, Item, ItemFn, ItemStruct, Path as SynPath, Type, Visibility};

fn read_source(relative: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path}: {error}"))
}

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
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source_root = manifest_dir.join("src");
    let canonical = source_root.join("canonical_wasmir.rs");
    let mut canonical_surface = WasmIrSurface::default();

    for (path, source) in rust_sources(&source_root) {
        let file = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} parses: {error}", path.display()));
        let mut surface = WasmIrSurface::default();
        surface.visit_file(&file);
        if path == canonical {
            canonical_surface = surface;
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
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let authority = source_root.join("canonical_safewir/authority.rs");
    let mut declarations = Vec::new();
    let mut constructions = Vec::new();

    for (path, source) in rust_sources(&source_root) {
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
        vec![(authority, "validate_final_inner".to_string())],
        "only final checking may construct ValidatedSafeWir"
    );
}

#[test]
fn a3_witness_and_digest_types_have_no_public_minting_surface() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut declarations = BTreeSet::new();
    let protected_public_types = [
        "CheckedLoweringWitness",
        "CheckedInitialSafeWir",
        "ValidatedSafeWir",
    ];
    let protected_private_types = [
        "CheckedSourceWitness",
        "FinalSafeWirWitness",
        "ExactModuleWitness",
    ];

    for (path, source) in rust_sources(&source_root) {
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
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
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

#[test]
fn raw_wir_byte_entrypoints_are_sealed_or_are_the_documented_bridge() {
    let source = read_source("src/wir_encode.rs");
    let file = syn::parse_file(&source).expect("wir_encode.rs parses");

    let encode = named(&file, "encode");
    assert!(takes(encode, "WirModule"));
    assert!(has_cfg(encode, "test"));
    assert!(has_cfg(encode, "raw-encoder-test-api"));

    let raw = named(&file, "try_encode_with_gc");
    assert!(takes(raw, "WirModule"));
    assert!(has_cfg(raw, "test"));
    assert!(has_cfg(raw, "raw-encoder-test-api"));

    let gc = named(&file, "encode_with_gc");
    assert!(takes(gc, "WirModule"));
    assert!(!is_public(gc), "the raw GC helper must remain crate-private");
    assert!(has_cfg(gc, "test"));
    assert!(has_cfg(gc, "raw-encoder-test-api"));

    let bridge = named(&file, "try_encode_with_gc_source_map");
    assert!(is_public(bridge));
    assert!(takes(bridge, "WirModule"));
    assert!(has_cfg(bridge, "test"));
    assert!(has_cfg(bridge, "legacy-encoder-bridge"));
    assert!(!has_cfg(bridge, "raw-encoder-test-api"));

    for function in functions(&file).filter(|function| is_public(function) && takes(function, "WirModule")) {
        let is_bridge = function.sig.ident == "try_encode_with_gc_source_map";
        let is_test_surface = has_cfg(function, "test") || has_cfg(function, "raw-encoder-test-api");
        assert!(
            is_bridge || is_test_surface,
            "new ungated public WIR byte entrypoint: {}",
            function.sig.ident
        );
    }
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
    let admission = named(&file, "admit_canonical");
    assert!(is_public(admission));
    assert!(takes(admission, "ValidatedSafeWir"));
    assert!(
        functions(&file).all(|function| {
            !is_public(function) || function.sig.ident == "admit_canonical"
        }),
        "canonical WasmIR must expose only validated artifact admission"
    );
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

    let encoder = named(&file, "encode_checked");
    assert!(matches!(encoder.vis, Visibility::Inherited));
    assert!(takes(encoder, "CheckedWasmIr"));
    assert!(
        !takes(encoder, "ValidatedSafeWir"),
        "production encoding must not accept SafeWIR authority directly"
    );

    let admission = named(&file, "admit_canonical");
    let mut calls = CalledFunctions::default();
    calls.visit_item_fn(admission);
    assert!(
        calls.0.contains("encode_checked"),
        "canonical admission must encode only checked WasmIR authority"
    );
    assert!(
        !calls.0.contains("encode_validated"),
        "canonical admission must not call the test-only SafeWIR encoder helper"
    );

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
    let source = read_source("src/canonical_wasmir.rs");

    assert!(
        source.contains("ValueType::Capability(_) => Ok(ValType::EXTERNREF)"),
        "canonical capability values must be represented as Wasm externref"
    );
    assert!(
        source.contains("HostParameterRole::Capability(_) => parameters.push(ValType::EXTERNREF)"),
        "canonical capability parameters must be represented as Wasm externref"
    );
    assert!(
        source.contains("HostResultRole::Capability(_) => results.push(ValType::EXTERNREF)"),
        "canonical capability results must be represented as Wasm externref"
    );
    for integer_type in ["I32", "I64", "F32", "F64", "V128"] {
        assert!(
            !source.contains(&format!("ValueType::Capability(_) => Ok(ValType::{integer_type})")),
            "canonical capability values must not map to ValType::{integer_type}"
        );
    }
}

#[test]
fn validated_legacy_encoder_still_consumes_only_bound_authority() {
    let source = read_source("src/wir_encode.rs");
    let file = syn::parse_file(&source).expect("wir_encode.rs parses");
    for name in ["try_encode_validated", "try_encode_validated_with_source_map"] {
        let function = named(&file, name);
        assert!(is_public(function));
        assert!(takes(function, "ValidatedWirModule"));
    }
}

#[test]
fn deleted_legacy_inliners_have_no_implementation_or_invocation() {
    let optimizer = syn::parse_file(&read_source("src/wir_opt.rs")).expect("wir_opt.rs parses");
    for name in ["inline", "recursive_inline"] {
        assert!(optimizer
            .items
            .iter()
            .all(|item| !matches!(item, Item::Mod(module) if module.ident == name)),
            "deleted legacy inliner module `{name}` is still registered",
        );
    }

    let optimizer_calls = called_functions(&optimizer);
    assert!(!optimizer_calls.contains("inline_direct_calls"));
    assert!(!optimizer_calls.contains("inline_recursive_calls"));

    let assembly = syn::parse_file(&read_source("../witchy-lower/src/codegen/assembly.rs"))
        .expect("assembly.rs parses");
    let assembly_calls = called_functions(&assembly);
    assert!(!assembly_calls.contains("inline_direct_calls"));
    assert!(!assembly_calls.contains("inline_recursive_calls"));
}
