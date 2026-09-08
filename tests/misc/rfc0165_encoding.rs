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

const BASE64_BYTES_ENCODE_SOURCE: &str = r#"
import bytes
import encoding

fn make_bytes(xs: List(Int)) -> Bytes:
    bytes.from_list(xs).unwrap_or(bytes.from_string(""))

fn standard(xs: List(Int)) -> String:
    encoding.base64_encode_bytes(make_bytes(xs))

fn url(xs: List(Int)) -> String:
    encoding.base64url_encode_bytes(make_bytes(xs))

fn main(console: Console):
    console.print(standard([]))
    console.print(standard([0]))
    console.print(standard([255]))
    console.print(standard([0, 255]))
    console.print(standard([0, 255, 1]))
    console.print(url([]))
    console.print(url([0]))
    console.print(url([255]))
    console.print(url([0, 255]))
    console.print(url([0, 255, 1]))
"#;

const STRING_FROM_CODE_SOURCE: &str = r#"
import string

fn main(console: Console):
    console.print(string.from_code(65))
    console.print(string.from_code(127))
    console.print(string.from_code(128))
    console.print(string.from_code(2047))
    console.print(string.from_code(2048))
    console.print(string.from_code(65535))
    console.print(string.from_code(65536))
    console.print(string.from_code(1114111))
    console.print(string.from_code(-1))
    console.print(string.from_code(1114112))
    console.print(string.from_code(55296))
    console.print(string.from_code(57343))
"#;

const TERMINAL_MATCH_SOURCE: &str = r#"
fn scan(values: List(String)) -> Result(String, String):
    var i = 0
    while i < list.length(values):
        let s = list.at(values, i)
        match s:
            "bad" -> return Err(s)
            _ -> i = i + 1
    Ok("done")

fn main(console: Console):
    match scan(["ok", "ok"]):
        Ok(value) -> console.print(value)
        Err(_) -> console.print("unexpected error")
    match scan(["ok", "bad"]):
        Ok(_) -> console.print("unexpected success")
        Err(e) -> console.print(e)
"#;

const VALUE_MATCH_TERMINAL_SOURCE: &str = r#"
fn choose(value: String) -> String:
    let selected = match value:
        "bad" -> return "returned"
        _ -> 7
    "${selected}"

fn main(console: Console):
    console.print(choose("bad"))
    console.print(choose("ok"))
"#;

#[test]
fn real_std_string_from_code_matches_rust_scalar_encoding_across_boundaries() {
    let checked = witchy::resolve_std_only_checked(STRING_FROM_CODE_SOURCE).expect("real std string source checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("real std string.from_code must admit");
    let codes = [65_i64, 127, 128, 2047, 2048, 65535, 65536, 1114111, -1, 1114112, 55296, 57343];
    let expected: Vec<String> = codes
        .iter()
        .map(|&code| u32::try_from(code).ok().and_then(char::from_u32).unwrap_or('\u{FFFD}').to_string())
        .collect();
    let source_output = witchy::interpreter::run_module_args(checked.module().clone(), ".", Vec::new(), Vec::new())
        .expect("source interpreter runs the real std checked string.from_code module");
    assert_eq!(source_output, expected, "source interpreter must follow Rust Unicode scalar semantics");

    let entry = artifact.entry().expect("string.from_code program entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("string source validates for oracle");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(InterpreterValue::entry_capability)
        .collect();
    let oracle = interpret_program(&validated, function, arguments, InterpreterConfig::default())
        .expect("canonical oracle runs real std string.from_code");
    assert_eq!(oracle.output, source_output, "canonical oracle must match source interpreter");
    assert_eq!(oracle.leaked_objects, 0, "string.from_code oracle must not leak");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn admitted real std string.from_code");
    vm.run().expect("admitted Wasm runs real std string.from_code");
    assert_eq!(vm.output(), source_output, "admitted Wasm must match source interpreter");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}

#[test]
fn terminal_match_arm_does_not_join_consumed_string_into_loop() {
    let checked = witchy::resolve_std_only_checked(TERMINAL_MATCH_SOURCE).expect("terminal match source checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("terminal match source must admit");
    let expected = vec!["done".to_owned(), "bad".to_owned()];
    let source_output = witchy::interpreter::run_module_args(checked.module().clone(), ".", Vec::new(), Vec::new())
        .expect("source interpreter runs terminal match source");
    assert_eq!(source_output, expected, "source interpreter expected terminal-match output");

    let entry = artifact.entry().expect("terminal match program entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("terminal match validates for oracle");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(InterpreterValue::entry_capability)
        .collect();
    let oracle = interpret_program(&validated, function, arguments, InterpreterConfig::default())
        .expect("canonical oracle runs terminal match source");
    assert_eq!(oracle.output, source_output, "canonical oracle must match source interpreter");
    assert_eq!(oracle.leaked_objects, 0, "terminal match oracle must not leak");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn admitted terminal match source");
    vm.run().expect("admitted Wasm runs terminal match source");
    assert_eq!(vm.output(), source_output, "admitted Wasm must match source interpreter");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}

#[test]
fn value_match_uses_first_continuing_arm_type_after_terminal_arm() {
    let checked = witchy::resolve_std_only_checked(VALUE_MATCH_TERMINAL_SOURCE)
        .expect("value match source checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("value match with a terminal first arm must admit");
    let expected = vec!["returned".to_owned(), "7".to_owned()];
    let source_output = witchy::interpreter::run_module_args(checked.module().clone(), ".", Vec::new(), Vec::new())
        .expect("source interpreter runs value match source");
    assert_eq!(source_output, expected, "source interpreter expected value-match output");

    let entry = artifact.entry().expect("value match program entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("value match validates for oracle");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(InterpreterValue::entry_capability)
        .collect();
    let oracle = interpret_program(&validated, function, arguments, InterpreterConfig::default())
        .expect("canonical oracle runs value match source");
    assert_eq!(oracle.output, source_output, "canonical oracle must match source interpreter");
    assert_eq!(oracle.leaked_objects, 0, "value match oracle must not leak");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn admitted value match source");
    vm.run().expect("admitted Wasm runs value match source");
    assert_eq!(vm.output(), source_output, "admitted Wasm must match source interpreter");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}

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
fn real_std_base64_byte_encoders_agree_on_padding_and_url_alphabet() {
    let checked = witchy::resolve_std_only_checked(BASE64_BYTES_ENCODE_SOURCE)
        .expect("real std byte base64 source checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("real std byte base64 encoders must admit");
    let expected = vec![
        "", "AA==", "/w==", "AP8=", "AP8B",
        "", "AA", "_w", "AP8", "AP8B",
    ];
    let source_output = witchy::interpreter::run_module_args(checked.module().clone(), ".", Vec::new(), Vec::new())
        .expect("source interpreter runs real std byte base64 encoders");
    assert_eq!(source_output, expected, "source interpreter expected byte base64 output");

    let entry = artifact.entry().expect("byte base64 program entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("byte base64 source validates for oracle");
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
    .expect("canonical oracle runs real std byte base64 encoders");
    assert_eq!(oracle.output, source_output, "canonical oracle must match source byte base64 output");
    assert_eq!(oracle.leaked_objects, 0, "byte base64 source oracle must not leak");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn admitted real std byte base64 encoders");
    vm.run().expect("admitted Wasm runs real std byte base64 encoders");
    assert_eq!(vm.output(), source_output, "admitted Wasm must match source byte base64 output");
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

#[test]
fn raw_base64_streaming_decoders_match_source_oracle_and_wasm() {
    let cases = [
        ("encoding.base64_decode_bytes_raw", "", "${bytes.length(decoded)}", "0"),
        ("encoding.base64_decode_bytes_raw", "=", "${bytes.length(decoded)}", "0"),
        ("encoding.base64_decode_bytes_raw", "0123", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}:${bytes.at(decoded, 1)}:${bytes.at(decoded, 2)}", "3:211:93:183"),
        ("encoding.base64_decode_bytes_raw", "T", "${bytes.length(decoded)}", "0"),
        ("encoding.base64_decode_bytes_raw", "TQ@rest", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}", "1:77"),
        ("encoding.base64_decode_bytes_raw", "T=Q", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}", "1:77"),
        ("encoding.base64_decode_bytes_raw", "T\\nQ", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}", "1:77"),
        ("encoding.base64_decode_bytes_raw", "TQ==", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}", "1:77"),
        ("encoding.base64_decode_bytes_raw", "TQ", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}", "1:77"),
        ("encoding.base64_decode_bytes_raw", "T\u{000b}Q", "${bytes.length(decoded)}", "0"),
        ("encoding.base64_decode_bytes_raw", "T\t\n\u{000c}\rQ", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}", "1:77"),
        ("encoding.base64_decode_bytes_raw", "TWE=", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}:${bytes.at(decoded, 1)}", "2:77:97"),
        ("encoding.base64_decode_bytes_raw", "TWE", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}:${bytes.at(decoded, 1)}", "2:77:97"),
        ("encoding.base64_decode_bytes_raw", "TWFu", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}:${bytes.at(decoded, 1)}:${bytes.at(decoded, 2)}", "3:77:97:110"),
        ("encoding.base64_decode_bytes_raw", "+///", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}:${bytes.at(decoded, 1)}:${bytes.at(decoded, 2)}", "3:251:255:255"),
        ("encoding.base64url_decode_bytes_raw", "-__", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}:${bytes.at(decoded, 1)}", "2:251:255"),
        ("encoding.base64url_decode_bytes_raw", "TQérest", "${bytes.length(decoded)}:${bytes.at(decoded, 0)}", "1:77"),
    ];
    let functions = cases
        .iter()
        .enumerate()
        .map(|(index, (intrinsic, input, render, _expected))| {
            format!(
                "pub fn test_base64_{index}(console: Console):\n    let decoded = {intrinsic}(\"{input}\")\n    console.print(\"{render}\")\n"
            )
        })
        .collect::<String>();
    let calls = cases
        .iter()
        .enumerate()
        .map(|(index, _)| format!("    encoding.test_base64_{index}(console)\n"))
        .collect::<String>();
    let bundled = witchy_syntax::linker::bundled_source("encoding").expect("bundled encoding source");
    let entry = format!("{bundled}\n{functions}");
    let main = format!("import encoding\n\nfn main(console: Console):\n{calls}");
    let mut pending = vec![("encoding".to_string(), entry), ("main".to_string(), main)];
    let mut loaded = std::collections::HashSet::from(["encoding".to_string(), "main".to_string()]);
    for &prelude in witchy_syntax::linker::PRELUDE_MODULES {
        if loaded.insert(prelude.to_owned()) {
            let source = witchy_syntax::linker::bundled_source(prelude).expect("bundled implicit prelude");
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
    let checked = witchy_interp::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        sources, "main", &std::collections::HashSet::from(["main".to_string()]), owners,
    ).expect("authenticated raw base64 fixture checks");
    let expected = cases.iter().map(|(_, _, _, expected)| (*expected).to_owned()).collect::<Vec<_>>();
    let native_output = cases.iter().map(|(intrinsic, input, render, _)| {
        let native = witchy_runtime::native::lookup(intrinsic).expect("cataloged raw base64 native binding");
        let native_input = input.replace("\\n", "\n");
        let value = native(&[witchy_runtime::value::NativeValue::Str(native_input)])
            .expect("audited raw base64 native reference");
        let witchy_runtime::value::NativeValue::Bytes(bytes) = value else {
            panic!("raw base64 native reference returned a non-Bytes value");
        };
        let mut fields = vec![bytes.len().to_string()];
        for index in 0..render.matches("bytes.at(decoded,").count() {
            fields.push(bytes[index].to_string());
        }
        fields.join(":")
    }).collect::<Vec<_>>();
    assert_eq!(native_output, expected, "audited native raw base64 reference contract");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("raw base64 decoders admit");
    let entry = artifact.entry().expect("raw base64 entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("raw base64 validates for oracle");
    let oracle = interpret_program(&validated, function, arguments, InterpreterConfig::default()).expect("oracle runs raw base64 fixture");
    assert_eq!(oracle.output, native_output, "canonical oracle must match audited native reference");
    assert_eq!(oracle.leaked_objects, 0, "raw base64 oracle must not leak");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn admitted raw base64 fixture");
    vm.run().expect("Wasm runs raw base64 fixture");
    assert_eq!(vm.output(), native_output, "admitted Wasm must match audited native reference");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}

#[test]
fn checked_base64_wrappers_match_source_oracle_and_wasm() {
    let source = r#"
import encoding
fn main(console: Console):
    for encoded in ["TQ==", "TWE", "TWFu"]:
        match encoding.base64_decode(encoded):
            Ok(value) -> console.print(value)
            Err(_) -> console.print("unexpected error")
    match encoding.base64_decode("T@Q"):
        Ok(_) -> console.print("unexpected success")
        Err(_) -> console.print("invalid standard")
    match encoding.base64url_decode("TQ"):
        Ok(value) -> console.print(value)
        Err(_) -> console.print("unexpected error")
    match encoding.base64url_decode("TQ=="):
        Ok(_) -> console.print("unexpected success")
        Err(_) -> console.print("invalid url")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("public Base64 source checks");
    let expected = vec!["M", "Ma", "Man", "invalid standard", "M", "invalid url"];
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter runs public Base64 wrappers");
    assert_eq!(source_output, expected, "public wrapper reference semantics");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("public Base64 wrappers admit");
    let entry = artifact.entry().expect("public Base64 entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("public Base64 wrappers validate for oracle");
    let oracle = interpret_program(&validated, function, arguments, InterpreterConfig::default())
        .expect("oracle runs public Base64 wrappers");
    assert_eq!(oracle.output, source_output);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256,
    ).expect("spawn admitted public Base64 wrappers");
    vm.run().expect("Wasm runs public Base64 wrappers");
    assert_eq!(vm.output(), source_output);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}
