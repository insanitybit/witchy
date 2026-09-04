use std::{collections::BTreeSet, fs};

use syn::visit::Visit;
use syn::{FnArg, Item, ItemFn, Type, Visibility};

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
    assert!(
        functions(&file).all(|function| !is_public(function)),
        "canonical WasmIR must not grow a public raw-byte producer"
    );
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
fn unassured_legacy_inliners_have_no_production_caller() {
    let optimizer = syn::parse_file(&read_source("src/wir_opt.rs")).expect("wir_opt.rs parses");
    for name in ["inline", "recursive_inline"] {
        let module = optimizer
            .items
            .iter()
            .find_map(|item| match item {
                Item::Mod(module) if module.ident == name => Some(module),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing legacy inliner module `{name}`"));
        let cfg = module
            .attrs
            .iter()
            .filter(|attribute| attribute.path().is_ident("cfg"))
            .filter_map(|attribute| match &attribute.meta {
                syn::Meta::List(list) => Some(list.tokens.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(cfg.contains("test"), "legacy inliner `{name}` must remain test-only");
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
