//! RFC-0167 row C: unit syntax is one source representation and one ABI shape.

use witchy::ast::Type;
use witchy_types::canonical_abi::{CanonicalResultKind, CanonicalScalarKind};
use witchy_types::pipeline::CanonicalCheckedSource;

const UNIT_MATRIX: &str = r#"
pub fn explicit_free() -> ():
    ()

pub fn omitted_free():
    ()

pub fn explicit_generic(value: a) -> ():
    ()

pub fn omitted_generic(value: a):
    ()

type Meter:
    Meter(Int)

impl Meter:
    pub fn explicit_method(self) -> ():
        ()

    pub fn omitted_method(self):
        ()

fn private_value():
    41

fn effect(console: Console) -> Int:
    console.print("effect")
    7

fn main(console: Console):
    explicit_free()
    omitted_free()
    explicit_generic(1)
    omitted_generic(1)
    let meter = Meter(0)
    meter.explicit_method()
    meter.omitted_method()
    let _ = effect(console)
    console.print("${private_value()}")
"#;

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        witchy::opt::set_for_tests(None);
    }
}

fn checked_unit_matrix() -> witchy_types::pipeline::CheckedModule {
    witchy::resolve_std_only_checked(UNIT_MATRIX).expect("RFC-0167 unit matrix checks")
}

fn named_callables(source: &CanonicalCheckedSource, marker: &str) -> Vec<String> {
    source
        .typed_module()
        .module()
        .items
        .iter()
        .filter_map(|item| match item {
            witchy::ast::Item::Function(function)
                if function.name.contains(marker)
                    && source.callable_signature(&function.name).is_some() =>
            {
                Some(function.name.clone())
            }
            _ => None,
        })
        .collect()
}

fn has_retired_nil(ty: &Type) -> bool {
    match ty {
        Type::Named(name, arguments) => {
            (name == "Nil" && arguments.is_empty()) || arguments.iter().any(has_retired_nil)
        }
        Type::Tuple(items) => items.iter().any(has_retired_nil),
        Type::Fn(parameters, result, _, _) => {
            parameters.iter().any(has_retired_nil) || has_retired_nil(result)
        }
        Type::Qualified(_, inner) | Type::Slice(inner) => has_retired_nil(inner),
        Type::Dyn(_, arguments) => arguments.iter().any(has_retired_nil),
        Type::RecordCompose { base, fields } => {
            has_retired_nil(base) || fields.iter().any(|(_, field)| has_retired_nil(field))
        }
    }
}

fn unit_result(ty: &Type) -> bool {
    matches!(ty.unqualified(), Type::Tuple(items) if items.is_empty())
}

fn assert_unit_abi(source: &CanonicalCheckedSource, name: &str) {
    let signature = source
        .callable_signature(name)
        .unwrap_or_else(|| panic!("canonical callable signature for {name}"));
    let Type::Fn(_, result, _, _) = signature.unqualified() else {
        panic!("canonical callable signature for {name} is not a function: {signature:?}");
    };
    assert!(unit_result(result), "{name} must retain `()` as its result syntax: {signature:?}");
    assert!(!has_retired_nil(signature), "{name} reintroduced retired `Nil`: {signature:?}");

    let abi = source
        .closed_callable_abi(name)
        .unwrap_or_else(|| panic!("closed ABI for {name}"));
    assert_eq!(abi.result, CanonicalResultKind::Scalar(CanonicalScalarKind::Unit));
}

#[test]
fn public_unit_spellings_share_source_abi_and_execute_on_canonical_oracle_and_unoptimized_wasm() {
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = checked_unit_matrix();
    let canonical = checked.canonical_source().expect("canonical checked source");

    for marker in [
        "explicit_free",
        "omitted_free",
        "explicit_generic",
        "omitted_generic",
        "explicit_method",
        "omitted_method",
    ] {
        let names = named_callables(canonical, marker);
        assert!(!names.is_empty(), "the exercised `{marker}` callable must be concrete");
        for name in names {
            assert_unit_abi(canonical, &name);
        }
    }

    for (explicit, omitted) in [
        ("explicit_free", "omitted_free"),
        ("explicit_generic", "omitted_generic"),
        ("explicit_method", "omitted_method"),
    ] {
        let explicit = named_callables(canonical, explicit);
        let omitted = named_callables(canonical, omitted);
        assert_eq!(explicit.len(), 1, "one exercised explicit callable");
        assert_eq!(omitted.len(), 1, "one exercised omitted callable");
        assert_eq!(
            canonical.closed_callable_abi(&explicit[0]).unwrap().key,
            canonical.closed_callable_abi(&omitted[0]).unwrap().key,
            "explicit and omitted unit have the same source ABI",
        );
    }

    let private = named_callables(canonical, "private_value");
    assert_eq!(private.len(), 1);
    let private_abi = canonical.closed_callable_abi(&private[0]).unwrap();
    assert_eq!(private_abi.result, CanonicalResultKind::Scalar(CanonicalScalarKind::Int));

    let expected = vec!["effect".to_owned(), "41".to_owned()];
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("unoptimized canonical unit matrix admits");
    let entry = artifact.entry().expect("canonical unit-matrix entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry
            .export()
            .strip_prefix('f')
            .expect("numeric entry export")
            .parse()
            .expect("numeric entry function"),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("canonical unit matrix validates for the oracle");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability)
        .collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated,
        function,
        arguments,
        Default::default(),
    )
    .expect("canonical interpreter executes unit matrix");
    assert_eq!(oracle.output, expected, "canonical interpreter matches the independent expectation");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter heap audit");

    let mut runtime = witchy::runtime::Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            witchy::runtime::Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            128,
    )
        .expect("spawn admitted unit matrix");
    vm.run().expect("run admitted unit matrix");
    assert_eq!(vm.output(), expected, "unoptimized canonical Wasm matches the independent expectation");
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn public_inferred_nonunit_free_functions_and_methods_are_rejected_at_the_source_boundary() {
    for source in [
        "pub fn value():\n    1\n\nfn main():\n    ()\n",
        "type Meter:\n    Meter(Int)\n\nimpl Meter:\n    pub fn value(self):\n        1\n\nfn main():\n    ()\n",
    ] {
        let error = match witchy::resolve_std_only_checked(source) {
            Ok(_) => panic!("a public inferred non-unit result must not become a source ABI"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("inferred non-unit"),
            "source-boundary diagnostic should identify the missing public result annotation: {error}",
        );
    }
}
