//! Dictionary replacement must preserve value semantics and owned children.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

fn assert_three_legs(source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).expect("dictionary source checks");
    assert_checked_three_legs(&checked, expected);
}

fn assert_checked_three_legs(checked: &witchy_types::pipeline::CheckedModule, expected: &[&str]) {
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter executes dictionary mutation");
    assert_eq!(source_output, expected, "independent expected output");
    let artifact = codegen::compile_checked_module_artifact(checked)
        .expect_lowered("dictionary mutation admits");
    let entry = artifact.entry().expect("source entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(checked)
        .expect("dictionary source validates independently");
    let arguments = entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability).collect();
    let oracle = interpret_program(&validated, function, arguments, InterpreterConfig {
        max_steps: 20_000_000_000, ..Default::default()
    }).expect("canonical interpreter executes dictionary mutation");
    assert_eq!(oracle.output, source_output);
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter ownership balance");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256,
    ).expect("admitted dictionary source");
    vm.run().expect("Wasm executes dictionary mutation");
    assert_eq!(vm.output(), source_output);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}

#[test]
fn string_dictionary_replacement_preserves_snapshot_and_displaced_value() {
    assert_three_legs(r#"
fn label(value: Option(String)) -> String:
    match value:
        Some(text) -> "some:" + text
        None -> "none"

fn main(console: Console):
    var d: Dict(String, String) = dict.new()
    console.print(label(dict.insert(d, "a", "first")))
    let snapshot = d
    console.print(label(dict.insert(d, "a", "replacement")))
    console.print(label(dict.insert(d, "b", "last")))
    console.print(dict.at(snapshot, "a"))
    console.print(dict.at(d, "a"))
    console.print(dict.at(d, "b"))
    console.print(dict.keys(d).join(","))
"#, &["none", "some:first", "none", "first", "replacement", "last", "a,b"]);
}

#[test]
fn int_key_string_values_preserve_empty_utf8_and_option_identity() {
    let source = r#"
fn label(value: Option(String)) -> String:
    match value:
        Some(text) -> "some:" + text
        None -> "none"

fn main(console: Console):
    var d: Dict(Int, String) = dict.new()
    console.print(label(dict.insert(d, 7, "")))
    console.print(label(dict.insert(d, 7, "UTF8")))
    console.print(label(dict.insert(d, 8, "tail")))
    console.print(label(dict.insert(d, 7, "")))
    console.print(dict.at(d, 7))
    console.print(dict.at(d, 8))
"#.replace("UTF8", "\u{03bb}");
    let old = format!("some:{}", '\u{03bb}');
    let expected = ["none", "some:", "none", old.as_str(), "", "tail"];
    assert_three_legs(&source, &expected);
    assert_three_legs(&format!("mode opt\n{source}"), &expected);
}

#[test]
fn pure_string_dictionary_assignment_needs_no_extracted_option() {
    assert_three_legs(r#"
fn main(console: Console):
    var d: Dict(Int, String) = dict.new()
    d[1] = "old"
    let snapshot = d
    d[1] = "new"
    d[2] = "appended"
    console.print(dict.at(snapshot, 1))
    console.print(dict.at(d, 1))
    console.print(dict.at(d, 2))
"#, &["old", "new", "appended"]);
}

#[test]
fn retained_nonliteral_string_survives_reinsertion_and_replacement() {
    assert_three_legs(r#"
fn make(part: String) -> String:
    part + "-right"

fn main(console: Console):
    var d: Dict(Int, String) = dict.new()
    let _ = dict.insert(d, 1, make("left"))
    let retained = dict.at(d, 1)
    let snapshot = d
    let _ = dict.insert(d, 2, retained)
    let _ = dict.insert(d, 1, retained)
    let displaced = dict.insert(d, 1, retained + "!")
    console.print(retained)
    console.print(dict.at(snapshot, 1))
    console.print(dict.at(d, 1))
    console.print(dict.at(d, 2))
    match displaced:
        Some(text) -> console.print("some:" + text)
        None -> console.print("none")
"#, &["left-right", "left-right", "left-right!", "left-right", "some:left-right"]);
}

#[test]
fn string_dictionary_removal_preserves_order_snapshots_and_displaced_values() {
    assert_three_legs(r#"
fn label(value: Option(String)) -> String:
    match value:
        Some(text) -> "some:" + text
        None -> "none"

fn main(console: Console):
    var d: Dict(String, String) = dict.new()
    let _ = dict.insert(d, "a", "first")
    let _ = dict.insert(d, "b", "")
    let _ = dict.insert(d, "c", "last")
    let snapshot = d
    console.print(label(dict.remove(d, "b")))
    console.print(dict.keys(d).join(","))
    console.print(label(dict.remove(d, "missing")))
    console.print(dict.keys(d).join(","))
    console.print(dict.keys(snapshot).join(","))
    console.print(dict.at(snapshot, "b"))
    console.print(label(dict.remove(d, "a")))
    console.print(dict.keys(d).join(","))
    console.print(label(dict.remove(d, "c")))
    console.print(dict.keys(d).join(","))
    console.print(label(dict.remove(d, "c")))
"#, &["some:", "a,c", "none", "a,c", "a,b,c", "", "some:first", "c", "some:last", "", "none"]);
}

#[test]
fn scalar_dictionary_removal_handles_middle_first_last_and_empty_in_both_modes() {
    let source = r#"
fn label(value: Option(Int)) -> String:
    match value:
        Some(n) -> n.show()
        None -> "none"

fn main(console: Console):
    var d: Dict(Int, Int) = dict.new()
    let _ = dict.insert(d, 1, 10)
    let _ = dict.insert(d, 2, 20)
    let _ = dict.insert(d, 3, 30)
    console.print(label(dict.remove(d, 2)))
    console.print(label(dict.remove(d, 1)))
    console.print(label(dict.remove(d, 3)))
    console.print(label(dict.remove(d, 3)))
"#;
    let expected = ["20", "10", "30", "none"];
    assert_three_legs(source, &expected);
    assert_three_legs(&format!("mode opt\n{source}"), &expected);
}

#[test]
fn pure_dictionary_removal_preserves_multi_entry_tail_and_missing_snapshots() {
    let source = r#"
fn main(console: Console):
    var d: Dict(Int, String) = dict.new()
    let _ = dict.insert(d, 1, "first")
    let _ = dict.insert(d, 2, "middle")
    let _ = dict.insert(d, 3, "last")
    let shorter = dict.__remove(d, 3)
    let unchanged = dict.__remove(shorter, 99)
    console.print(dict.values(d).join(","))
    console.print(dict.values(shorter).join(","))
    console.print(dict.values(unchanged).join(","))
"#;
    let privacy_error = witchy::resolve_std_only_checked(source)
        .err().expect("the pure dictionary operation remains private");
    assert!(privacy_error.to_string().contains("private to module"));

    // Exercise the private operation inside its actual bundled module, without
    // changing production std sources or weakening reserved-module ownership.
    let bundled = witchy_syntax::linker::bundled_source("dict").expect("bundled dict source");
    let entry = source.replacen(
        "fn main(console: Console):", "pub fn test_pure_remove(console: Console):", 1,
    );
    let main = "fn main(console: Console):\n    dict.test_pure_remove(console)\n";
    let mut pending = vec![
        ("dict".to_string(), format!("{bundled}\n{entry}")),
        ("main".to_string(), main.to_owned()),
    ];
    let mut loaded = std::collections::HashSet::from(["dict".to_string(), "main".to_string()]);
    for &prelude in witchy_syntax::linker::PRELUDE_MODULES {
        if loaded.insert(prelude.to_owned()) {
            let prelude_source = witchy_syntax::linker::bundled_source(prelude)
                .expect("bundled implicit prelude");
            pending.push((prelude.to_owned(), prelude_source.to_owned()));
        }
    }
    let mut sources = Vec::new();
    while let Some((name, module_source)) = pending.pop() {
        let module = witchy_syntax::parser::parse_module(&module_source)
            .expect("bundled test module parses");
        for import in module.imports {
            if loaded.insert(import.clone()) {
                let dependency = witchy_syntax::linker::bundled_source(&import)
                    .expect("bundled dependency");
                pending.push((import, dependency.to_owned()));
            }
        }
        sources.push((name, module_source.into_bytes()));
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
    ).expect("authenticated dictionary entry checks with its real dependencies");
    assert_checked_three_legs(&checked, &["first,middle,last", "first,middle", "first,middle"]);
}
