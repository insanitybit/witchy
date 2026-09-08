//! RFC-0165 real-standard-library encoding reachability and execution slice.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

const HEX_ENCODE_SOURCE: &str = r#"
import encoding

fn main(console: Console):
    console.print(encoding.hex_encode(""))
    console.print(encoding.hex_encode("Hi"))
    console.print(encoding.hex_encode("é🙂"))
"#;

const HEX_DECODE_SOURCE: &str = r#"
import encoding

fn main(console: Console):
    match encoding.hex_decode(""):
        Ok(value) -> console.print(value)
        Err(_) -> console.print("decode error")
    match encoding.hex_decode("4869"):
        Ok(value) -> console.print(value)
        Err(_) -> console.print("decode error")
    match encoding.hex_decode("c3a9f09f9982"):
        Ok(value) -> console.print(value)
        Err(_) -> console.print("decode error")
    match encoding.hex_decode("f"):
        Ok(_) -> console.print("bad odd")
        Err(_) -> console.print("odd error")
    match encoding.hex_decode("gg"):
        Ok(_) -> console.print("bad digit")
        Err(_) -> console.print("digit error")
"#;

const HEX_RAW_VALID_SOURCE: &str = r#"
import encoding
import bytes

fn main(console: Console):
    let decoded = encoding.hex_decode_bytes_raw("09aFfF")
    console.print("${bytes.at(decoded, 0)} ${bytes.at(decoded, 1)} ${bytes.at(decoded, 2)}")
"#;

const BASE64_ENCODE_SOURCE: &str = r#"
import encoding

fn main(console: Console):
    console.print(encoding.base64_encode(""))
    console.print(encoding.base64_encode("f"))
    console.print(encoding.base64_encode("fo"))
    console.print(encoding.base64_encode("foo"))
    console.print(encoding.base64_encode("foob"))
    console.print(encoding.base64_encode("é🙂"))
"#;

#[test]
fn real_std_hex_encode_agrees_with_oracle_and_admitted_wasm() {
    let checked = witchy::resolve_std_only_checked(HEX_ENCODE_SOURCE).expect("real std hex source checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("real std hex_encode must admit");
    let source_output = witchy::interpreter::run_module_args(checked.module().clone(), ".", Vec::new(), Vec::new())
        .expect("source interpreter runs the real std checked module");
    assert_eq!(source_output, vec!["", "4869", "c3a9f09f9982"], "source interpreter expected output");
    let entry = artifact.entry().expect("hex_encode program entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("hex source validates for oracle");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(InterpreterValue::entry_capability)
        .collect();
    let oracle = interpret_program(
        &validated,
        function,
        arguments,
        InterpreterConfig { max_steps: 20_000_000_000, ..Default::default() },
    )
    .expect("canonical oracle runs real std hex_encode");
    assert_eq!(oracle.output, source_output, "canonical oracle must match source interpreter");
    assert_eq!(oracle.leaked_objects, 0, "source oracle must not leak");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn admitted real std hex_encode");
    vm.run().expect("admitted Wasm runs real std hex_encode");
    assert_eq!(vm.output(), source_output, "admitted Wasm must match source interpreter");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}

#[test]
fn real_std_base64_encode_agrees_on_lengths_and_utf8() {
    let checked = witchy::resolve_std_only_checked(BASE64_ENCODE_SOURCE).expect("real std base64 source checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("real std base64_encode must admit");
    let expected = vec!["", "Zg==", "Zm8=", "Zm9v", "Zm9vYg==", "w6nwn5mC"];
    let source_output = witchy::interpreter::run_module_args(checked.module().clone(), ".", Vec::new(), Vec::new())
        .expect("source interpreter runs the real std checked base64 module");
    assert_eq!(source_output, expected, "source interpreter expected base64 output");

    let entry = artifact.entry().expect("base64_encode program entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("base64 source validates for oracle");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(InterpreterValue::entry_capability)
        .collect();
    let oracle = interpret_program(
        &validated,
        function,
        arguments,
        InterpreterConfig { max_steps: 20_000_000_000, ..Default::default() },
    )
    .expect("canonical oracle runs real std base64_encode");
    assert_eq!(oracle.output, source_output, "canonical oracle must match source interpreter");
    assert_eq!(oracle.leaked_objects, 0, "base64 source oracle must not leak");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn admitted real std base64_encode");
    vm.run().expect("admitted Wasm runs real std base64_encode");
    assert_eq!(vm.output(), source_output, "admitted Wasm must match source interpreter");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}

#[test]
fn raw_hex_decode_rejects_malformed_bytes_and_preserves_nibble_boundaries() {
    let privacy_error = witchy::resolve_std_only_checked(HEX_RAW_VALID_SOURCE)
        .err().expect("the raw helper remains private to encoding");
    assert!(privacy_error.to_string().contains("private to module `encoding`"));
    // Add a test-only entry inside the actual bundled module. Its private raw
    // helper and dependencies are unchanged, and all sources are parsed and
    // checked together before the production compiler sees the program.
    let kernel_checked = |input: &str, render_bytes: bool| {
        let render = if render_bytes {
            "console.print(\"${bytes.at(decoded, 0)} ${bytes.at(decoded, 1)} ${bytes.at(decoded, 2)}\")"
        } else {
            "console.print(\"${bytes.length(decoded)}\")"
        };
        let bundled = witchy_syntax::linker::bundled_source("encoding").expect("bundled encoding source");
        let entry = format!("{bundled}\npub fn test_raw_kernel(console: Console):\n    let decoded = hex_decode_bytes_raw(\"{input}\")\n    {render}\n");
        let main = "import encoding\n\nfn main(console: Console):\n    encoding.test_raw_kernel(console)\n";
        let mut pending = vec![("encoding".to_string(), entry), ("main".to_string(), main.to_owned())];
        let mut loaded = std::collections::HashSet::from(["encoding".to_string(), "main".to_string()]);
        for &prelude in witchy_syntax::linker::PRELUDE_MODULES {
            if loaded.insert(prelude.to_owned()) {
                let source = witchy_syntax::linker::bundled_source(prelude)
                    .expect("bundled implicit prelude");
                pending.push((prelude.to_owned(), source.to_owned()));
            }
        }
        let mut sources = Vec::new();
        while let Some((name, source)) = pending.pop() {
            let module = witchy_syntax::parser::parse_module(&source).expect("bundled test module parses");
            for import in module.imports {
                if loaded.insert(import.clone()) {
                    let dependency = witchy_syntax::linker::bundled_source(&import).expect("bundled dependency");
                    pending.push((import, dependency.to_owned()));
                }
            }
            sources.push((name, source.into_bytes()));
        }
        // Reserved std module names require loader-authenticated ownership,
        // just as they do in resolve_std_only_checked. Do not relax that gate.
        use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", env!("CARGO_PKG_VERSION")).unwrap();
        let application = PackageCoordinate::new(PackageSource::Workspace, "witchy/source", env!("CARGO_PKG_VERSION")).unwrap();
        let assignments: Vec<_> = sources.iter().map(|(name, _)| {
            let owner = if name == "main" {
                ModuleLoadIdentity::new(application.clone(), ["source", "main"]).unwrap()
            } else {
                ModuleLoadIdentity::new(toolchain.clone(), ["std", name.as_str()]).unwrap()
            };
            (name.clone(), owner)
        }).collect();
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap();
        witchy_interp::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
            sources, "main", &std::collections::HashSet::from(["main".to_string()]), owners,
        ).expect("test-only authenticated encoding entry checks with its real dependencies")
    };
    let checked = kernel_checked("09aFfF", true);
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("valid raw hex helper admits");
    let source_output = witchy::interpreter::run_module_args(checked.module().clone(), ".", Vec::new(), Vec::new())
        .expect("source interpreter accepts valid raw hex");
    assert_eq!(source_output, vec!["9 175 255"]);
    let entry = artifact.entry().expect("raw hex entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("valid raw hex validates for oracle");
    let oracle = interpret_program(&validated, function, arguments, InterpreterConfig::default()).expect("oracle accepts valid raw hex");
    assert_eq!(oracle.output, source_output);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn valid raw hex artifact");
    vm.run().expect("Wasm accepts valid raw hex");
    assert_eq!(vm.output(), source_output);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);

    for malformed in ["f", "0g", "/0", ":0", "éé"] {
        let checked = kernel_checked(malformed, false);
        assert!(witchy::interpreter::run_module_args(checked.module().clone(), ".", Vec::new(), Vec::new()).is_err(), "source raw helper rejects {malformed:?}");
        let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("malformed raw hex still lowers to a trapping kernel");
        let entry = artifact.entry().expect("malformed raw hex entry");
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let validated = codegen::validate_checked_module_for_oracle(&checked).expect("malformed raw hex validates before execution");
        assert!(interpret_program(&validated, function, arguments, InterpreterConfig::default()).is_err(), "oracle raw helper traps {malformed:?}");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime
            .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
            .expect("spawn malformed raw hex artifact");
        assert!(vm.run().is_err(), "Wasm raw helper traps {malformed:?}");
    }
}

#[test]
fn real_std_hex_decode_agrees_with_oracle_and_admitted_wasm() {
    let checked = witchy::resolve_std_only_checked(HEX_DECODE_SOURCE).expect("real std hex source checks");
    let canonical = checked.canonical_source().expect("canonical source");
    let map_ok_name = canonical
        .typed_module()
        .module()
        .items
        .iter()
        .find_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) if function.name.starts_with("Result__map_ok__") => Some(function.name.clone()),
            _ => None,
        })
        .expect("real std hex_decode must monomorphize Result.map_ok");
    assert!(canonical.callable_signature(&map_ok_name).is_some(), "reachable map_ok must be in the runtime inventory");
    assert!(!canonical.obligations().iter().any(|obligation| {
        matches!(obligation, witchy_types::pipeline::CanonicalObligation::Specialization(name) if name.starts_with("Result__map_ok__"))
    }), "reachable map_ok must not be hidden as an unreachable obligation");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("real std hex_decode must admit");
    let source_output = witchy::interpreter::run_module_args(checked.module().clone(), ".", Vec::new(), Vec::new())
        .expect("source interpreter runs the real std checked module");
    assert_eq!(source_output, vec!["", "Hi", "é🙂", "odd error", "digit error"], "source interpreter expected decoded output");
    let entry = artifact.entry().expect("hex_decode program entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(InterpreterValue::entry_capability)
        .collect();
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("hex decode source validates for oracle");
    let oracle = interpret_program(
        &validated,
        function,
        arguments,
        InterpreterConfig { max_steps: 20_000_000_000, ..Default::default() },
    )
    .expect("canonical oracle runs real std hex_decode");
    assert_eq!(oracle.output, source_output, "canonical oracle must match source interpreter");
    assert_eq!(oracle.leaked_objects, 0, "hex decode oracle must not leak");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn admitted real std hex_decode");
    vm.run().expect("admitted Wasm runs real std hex_decode");
    assert_eq!(vm.output(), source_output, "admitted Wasm must match source interpreter");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}
