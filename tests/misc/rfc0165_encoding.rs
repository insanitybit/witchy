//! RFC-0165 real-standard-library encoding reachability and execution slice.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

const HEX_RAW_VALID_SOURCE: &str = r#"
import encoding
import bytes

fn main(console: Console):
    let decoded = encoding.hex_decode_bytes_raw("09aFfF")
    console.print("${bytes.at(decoded, 0)} ${bytes.at(decoded, 1)} ${bytes.at(decoded, 2)}")
"#;

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

