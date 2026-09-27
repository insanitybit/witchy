use witchy_runtime::runtime::{Capabilities, Runtime, comptime::ComptimeLimits};
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
use super::temp_dir::TempDir;
use std::process::Command;

fn owners() -> AuthenticatedModuleOwners {
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/ordered-comptime", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let assignments = [("generator".into(), ModuleLoadIdentity::new(workspace, ["generator"]).unwrap())]
        .into_iter().chain(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
            (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
        )));
    AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap()
}

fn limits() -> ComptimeLimits {
    ComptimeLimits { fuel: 2_000_000, memory_pages: 64, fresh_identifiers: 4,
        host_output_bytes: 4096, syntax_nodes: 10_000, syntax_bytes: 100_000,
        emitted_nodes: 10_000, emitted_bytes: 100_000 }
}

#[test]
fn admitted_original_blocks_feed_checked_successors_in_normal_and_opt() {
    struct ResetOptimization;
    impl Drop for ResetOptimization { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}comptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            40\n    )\ncomptime:\n    if answer() == 40:\n        emit_item(quote item:\n            fn result() -> Int:\n                answer() + 2\n        )\nfn main(console: Console):\n    console.print(\"${{result()}}\")\n");
        let sources = vec![("generator".into(), source.into_bytes())];
        let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/comptime", "1").unwrap();
        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
        let mut assignments = vec![("generator".into(), ModuleLoadIdentity::new(workspace, ["generator"]).unwrap())];
        assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
            (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
        )));
        let checked = witchy::canonical_comptime::check_source_bundle(
            sources, "generator",
            ["generator".to_owned()].into_iter().collect(),
            AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap(), limits(),
        ).expect("both original blocks execute in order through admitted unoptimized Wasm");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("generated destination passes ordinary checking and admission");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["42"], "independent expectation: generated answer 40 + 2");
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

#[test]
fn generated_item_is_rechecked_before_successor_executes() {
    let source = "comptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            \"wrong\"\n    )\ncomptime:\n    fail(\"second block must not execute\")\nfn main() -> Int:\n    0\n";
    let error = match witchy::canonical_comptime::check_source_bundle(
        vec![("generator".into(), source.as_bytes().to_vec())], "generator",
        ["generator".to_owned()].into_iter().collect(), owners(), limits(),
    ) { Ok(_) => panic!("invalid generated item entered destination"), Err(error) => error };
    assert!(error.contains("Int") && error.contains("String"), "{error}");
    assert!(!error.contains("second block must not execute"), "{error}");
}

#[test]
fn generated_block_executes_before_next_original_block() {
    let folder = TempDir::new("admitted-generated-comptime-block");
    let source = folder.join("main.witchy");
    std::fs::write(&source, "comptime:\n    emit_item(quote item:\n        comptime:\n            emit_item(quote item:\n                fn first() -> Int:\n                    40\n            )\n    )\ncomptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            first() + 2\n    )\nfn main(console: Console):\n    console.print(\"${answer()}\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");

}

#[test]
fn admitted_blocks_see_types_from_prior_output() {
    let folder = TempDir::new("admitted-module-types");
    let source = folder.join("main.witchy");
    std::fs::write(&source, "type User:\n    name: String\n    age: Int\ncomptime:\n    if module_types.length() == 1 && module_types[0].fields.length() == 2 && module_types[0].fields[0].name == \"name\":\n        emit_item(quote item:\n            type Extra:\n                value: Int\n        )\ncomptime:\n    if module_types.length() == 2 && module_types[1].name == \"Extra\":\n        emit_item(quote item:\n            fn answer() -> Int:\n                42\n        )\nfn main(console: Console):\n    console.print(\"${answer()}\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");

    std::fs::write(&source, "comptime:\n    if module_types.length() == 0:\n        emit_item(quote item:\n            fn answer() -> Int:\n                0\n        )\nfn main(console: Console):\n    console.print(\"${answer()}\")\n").unwrap();
    let empty = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(empty.status.success(), "{}", String::from_utf8_lossy(&empty.stderr));
    assert_eq!(String::from_utf8_lossy(&empty.stdout), "0\n");
}

#[test]
fn admitted_type_info_resolves_staged_type_names_before_execution() {
    let folder = TempDir::new("admitted-type-info");
    let source = folder.join("main.witchy");
    std::fs::write(&source, "import meta\ntype User:\n    name: String\n    age: Int\ncomptime:\n    let info = type_info(User)\n    if info.name == \"User\" && info.fields.length() == 2 && info.fields[0].name == \"name\":\n        emit_item(quote item:\n            fn answer() -> Int:\n                42\n        )\nfn main(console: Console):\n    console.print(\"${answer()}\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");

    std::fs::write(&source, "import meta\ntype Boxed(a):\n    value: a\ntype NumberBox = Boxed(Int)\ncomptime:\n    emit_item(quote item:\n        type Earlier:\n            n: Int\n    )\ncomptime:\n    let box_info = type_info(NumberBox)\n    let earlier = type_info(Earlier)\n    match box_info.fields[0].type_expr:\n        meta.TNamed(name, _) ->\n            if name == \"Int\" && earlier.fields[0].name == \"n\":\n                emit_item(quote item:\n                    fn answer() -> Int:\n                        43\n                )\n        _ -> fail(\"generic type argument was not substituted\")\nfn main(console: Console):\n    console.print(\"${answer()}\")\n").unwrap();
    let generic = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        generic.status.success(),
        "{}",
        String::from_utf8_lossy(&generic.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&generic.stdout), "43\n");

    std::fs::write(
        &source,
        "import meta\ncomptime:\n    let _ = type_info(Missing)\nfn main() -> Int:\n    0\n",
    )
    .unwrap();
    let missing = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr)
            .contains("unknown type `Missing`")
    );

    std::fs::write(&source, "import meta\ncomptime:\n    let name = \"User\"\n    let _ = type_info(name)\nfn main() -> Int:\n    0\n").unwrap();
    let value = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(!value.status.success());
    assert!(
        String::from_utf8_lossy(&value.stderr).contains("type_info operand must be a type name")
    );
}

#[test]
fn admitted_reflected_field_types_build_a_typed_record_in_both_modes() {
    let folder = TempDir::new("admitted-reflected-record-types");
    let source = folder.join("main.witchy");
    for mode in ["", "mode opt\n"] {
        std::fs::write(&source, format!("{mode}import meta\ntype User:\n    name: String\n    age: Int\ncomptime:\n    let info = type_info(User)\n    let name_ty = meta.type_expr(info.fields[0].type_expr)\n    let age_ty = meta.type_expr(info.fields[1].type_expr)\n    emit_item(meta.record_type(meta.ident(\"Mirror\"), [(meta.ident(\"name\"), name_ty), (meta.ident(\"age\"), age_ty)]))\nfn main(console: Console):\n    let value = Mirror(\"Ada\", 42)\n    console.print(\"${{value.name}}/${{value.age}}\")\n")).unwrap();
        let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(String::from_utf8_lossy(&run.stdout), "Ada/42\n");
    }
}

#[test]
fn program_defined_builder_uses_type_info_in_both_modes() {
    let folder = TempDir::new("program-defined-builder");
    let source = folder.join("main.witchy");
    let program = r#"import meta
type User:
    name: String
    age: Int
comptime fn builder_items(info: meta.TypeInfo) -> List(meta.ItemSyntax):
    let builder_name = info.name + "Builder"
    let builder_ty = meta.type_expr(meta.TNamed(builder_name, []))
    let user_ty = meta.type_expr(meta.TNamed(info.name, []))
    let some_expr = quote expr:
        Some
    let value_expr = quote expr:
        value
    let self_expr = quote expr:
        self
    var slots = []
    for field in info.fields:
        slots.push((meta.ident(field.name), meta.type_expr(meta.TNamed("Option", [field.type_expr]))))
    var methods = []
    for field in info.fields:
        var values = []
        for other in info.fields:
            let value = if other.name == field.name:
                meta.expr_call(some_expr, [value_expr])
            else:
                meta.expr_field(self_expr, meta.ident(other.name))
            values.push((meta.ident(other.name), value))
        methods.push(meta.function(true, meta.ident("with_" + field.name),
            [meta.param_own(meta.ident("self"), builder_ty), meta.param_own(meta.ident("value"), meta.type_expr(field.type_expr))],
            Some(builder_ty), meta.expr_record(meta.ident(builder_name), values)))
    var user_values = []
    for field in info.fields:
        user_values.push((meta.ident(field.name), meta.expr_try(meta.expr_field(self_expr, meta.ident(field.name)))))
    let user = meta.expr_record(meta.ident(info.name), user_values)
    methods.push(meta.function(true, meta.ident("build"), [meta.param_own(meta.ident("self"), builder_ty)],
        Some(meta.type_expr(meta.TNamed("Option", [meta.TNamed(info.name, [])]))),
        meta.expr_call(some_expr, [user])))
    [meta.record_type(meta.ident(builder_name), slots), meta.inherent_impl(builder_ty, methods)]
comptime:
    for item in builder_items(type_info(User)):
        emit_item(item)
fn main(console: Console):
    let complete = UserBuilder(None, None).with_name("Ada").with_age(42).build()
    let incomplete = UserBuilder(None, None).with_name("Ada").build()
    match complete:
        Some(user) -> console.print("${user.name}/${user.age}")
        None -> console.print("missing")
    match incomplete:
        Some(_) -> console.print("unexpected")
        None -> console.print("None")
"#;
    for mode in ["", "mode opt\n"] {
        std::fs::write(&source, format!("{mode}{program}")).unwrap();
        let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(String::from_utf8_lossy(&run.stdout), "Ada/42\nNone\n");

        let expanded = program.replacen("    age: Int\n", "    age: Int\n    email: String\n", 1);
        std::fs::write(&source, format!("{mode}{expanded}")).unwrap();
        let stale = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
        assert!(!stale.status.success(), "old two-slot construction must fail after adding a field");

        let updated = expanded.replace("UserBuilder(None, None)", "UserBuilder(None, None, None)")
            .replace(".with_age(42).build()", ".with_age(42).with_email(\"ada@example.com\").build()");
        std::fs::write(&source, format!("{mode}{updated}")).unwrap();
        let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(String::from_utf8_lossy(&run.stdout), "Ada/42\nNone\n");
    }
}

#[test]
fn admitted_type_info_uses_imported_definition_identity_across_modes() {
    let folder = TempDir::new("admitted-imported-type-info");
    let source = folder.join("main.witchy");
    let dependency = folder.join("dep.witchy");
    for (dependency_mode, entry_mode) in [("", ""), ("mode opt\n", ""), ("mode opt\n", "mode opt\n")] {
        std::fs::write(&dependency, format!("{dependency_mode}type Inner:\n    value: Int\ntype Wrapper:\n    child: Inner\ntype Boxed(a):\n    item: a\n")).unwrap();
        std::fs::write(&source, format!("{entry_mode}import dep\nfrom dep import Boxed\nimport meta\ncomptime:\n    let wrapped = type_info(dep.Wrapper)\n    let boxed = type_info(Boxed(Int))\n    let qualified = type_info(dep.Boxed(Int))\n    match wrapped.fields[0].type_expr:\n        meta.TNamed(name, _) ->\n            if name == \"dep.Inner\" && boxed.fields[0].name == \"item\" && qualified.fields[0].name == \"item\":\n                emit_item(quote item:\n                    fn answer() -> Int:\n                        44\n                )\n        _ -> fail(\"wrong imported type identity\")\nfn main(console: Console):\n    console.print(\"${{answer()}}\")\n")).unwrap();
        let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(String::from_utf8_lossy(&run.stdout), "44\n");
    }

    std::fs::write(folder.join("other.witchy"), "type Inner:\n    x: Int\n").unwrap();
    std::fs::write(&dependency, "type Boxed(a):\n    item: a\n").unwrap();
    std::fs::write(&source, "import dep\nimport other\nimport meta\ncomptime:\n    let info = type_info(dep.Boxed(other.Inner))\n    match info.fields[0].type_expr:\n        meta.TNamed(name, _) ->\n            if name == \"other.Inner\":\n                emit_item(quote item:\n                    fn answer() -> Int:\n                        45\n                )\n        _ -> fail(\"wrong caller type argument\")\nfn main(console: Console):\n    console.print(\"${answer()}\")\n").unwrap();
    let cross = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(cross.status.success(), "{}", String::from_utf8_lossy(&cross.stderr));
    assert_eq!(String::from_utf8_lossy(&cross.stdout), "45\n");

    std::fs::write(folder.join("other.witchy"), "type Hidden:\n    x: Int\n").unwrap();
    std::fs::write(&dependency, "import other\ntype Wrapper:\n    child: other.Hidden\n").unwrap();
    std::fs::write(&source, "import dep\ncomptime:\n    let _ = type_info(other.Hidden)\nfn main() -> Int:\n    0\n").unwrap();
    let hidden = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(!hidden.status.success());
    assert!(String::from_utf8_lossy(&hidden.stderr).contains("module `other` is not imported"));
}

#[test]
fn type_info_descriptor_cannot_be_forged_or_rewritten() {
    let folder = TempDir::new("sealed-type-info");
    let source = folder.join("main.witchy");
    for program in [
        "import meta\ntype User:\n    name: String\ncomptime:\n    let fake = meta.TypeInfo(\"User\", meta.TypeRecord, [], [], [])\n    emit_item(quote item:\n        fn answer() -> Int:\n            42\n    )\nfn main() -> Int:\n    answer()\n",
        "import meta\ntype User:\n    name: String\ncomptime:\n    var info = type_info(User)\n    info.name = \"Other\"\n    emit_item(quote item:\n        fn answer() -> Int:\n            42\n    )\nfn main() -> Int:\n    answer()\n",
    ] {
        std::fs::write(&source, program).unwrap();
        let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
        assert!(!run.status.success(), "TypeInfo authority must survive source checking");
        let error = String::from_utf8_lossy(&run.stderr);
        assert!(error.contains("sealed") && error.contains("TypeInfo"), "{error}");
    }
}

#[test]
fn admitted_typed_record_head_preserves_imported_nominal_identity() {
    let folder = TempDir::new("typed-imported-record-head");
    let source = folder.join("main.witchy");
    std::fs::write(folder.join("dep.witchy"),
        "mode opt\ntype User:\n    name: String\n    age: Int\n").unwrap();
    std::fs::write(&source,
        "import dep\nimport meta\ncomptime:\n    let user_ty = quote type:\n        dep.User\n    let user = meta.expr_record_type(user_ty, [(meta.ident(\"name\"), meta.expr_string(\"Ada\")), (meta.ident(\"age\"), meta.expr_int(42))])\n    emit_item(meta.function(true, meta.ident(\"make_user\"), [], Some(user_ty), user))\nfn main(console: Console):\n    let user = make_user()\n    console.print(\"${user.name}/${user.age}\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "Ada/42\n");
}

#[test]
fn admitted_comptime_helper_looks_up_prior_emitted_type() {
    let folder = TempDir::new("admitted-comptime-helper-type-info");
    let source = folder.join("main.witchy");
    for mode in ["", "mode opt\n"] {
        std::fs::write(&source, format!("{mode}import meta\ncomptime:\n    emit_item(quote item:\n        type Extra:\n            value: Int\n    )\ncomptime fn count_fields(offset: Int) -> Int:\n    let info = type_info(Extra)\n    list.length(info.fields) + offset\ncomptime:\n    if count_fields(41) == 42:\n        emit_item(quote item:\n            fn answer() -> Int:\n                42\n        )\nfn main(console: Console):\n    console.print(\"${{answer()}}\")\n")).unwrap();
        let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");
    }
}

#[test]
fn admitted_generated_comptime_helper_uses_checked_type_lookup() {
    let folder = TempDir::new("admitted-generated-comptime-helper");
    let source = folder.join("main.witchy");
    std::fs::write(&source, "import meta\ntype User:\n    name: String\ncomptime:\n    emit_item(quote item:\n        comptime fn fields_plus(offset: Int) -> Int:\n            let info = type_info(User)\n            list.length(info.fields) + offset\n    )\ncomptime:\n    if fields_plus(41) == 42:\n        emit_item(quote item:\n            fn answer() -> Int:\n                42\n        )\nfn main(console: Console):\n    console.print(\"${answer()}\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");

    std::fs::write(&source, "type User:\n    name: String\ncomptime:\n    emit_item(quote item:\n        fn forbidden() -> Int:\n            let info = type_info(User)\n            list.length(info.fields)\n    )\ncomptime:\n    fail(\"later block ran\")\nfn main() -> Int:\n    0\n").unwrap();
    let rejected = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(!rejected.status.success());
    let error = String::from_utf8_lossy(&rejected.stderr);
    assert!(error.contains("unknown function `type_info`"), "{error}");
    assert!(!error.contains("later block ran"), "{error}");
}

#[test]
fn production_source_loader_executes_original_blocks_through_admitted_wasm() {
    let folder = TempDir::new("admitted-comptime-loader");
    let source = folder.join("main.witchy");
    std::fs::write(&source, "fn helper() -> Int:\n    39\nfn relay() -> Int:\n    helper() + 1\nfn unused(console: Console):\n    console.print(\"must stay out of the compiler session\")\ncomptime:\n    if relay() == 40:\n        emit_item(quote item:\n            fn answer() -> Int:\n                40\n        )\ncomptime:\n    emit_item(quote item:\n        fn result() -> Int:\n            answer() + 2\n    )\nfn main(console: Console):\n    console.print(\"${result()}\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");

    std::fs::write(&source, "comptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            \"wrong\"\n    )\ncomptime:\n    fail(\"second block must not execute\")\nfn main() -> Int:\n    0\n").unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("check").arg(&source).output().unwrap();
    assert!(!check.status.success(), "invalid generated source was accepted");
    let diagnostic = String::from_utf8_lossy(&check.stderr);
    assert!(diagnostic.contains("Int") && diagnostic.contains("String"), "{diagnostic}");
    assert!(!diagnostic.contains("second block must not execute"), "{diagnostic}");
}

#[test]
fn imported_original_block_feeds_authenticated_entry() {
    let folder = TempDir::new("admitted-imported-comptime");
    let entry = folder.join("main.witchy");
    std::fs::write(folder.join("dep.witchy"), "comptime:\n    emit_item(quote item:\n        pub fn answer() -> Int:\n            42\n    )\n").unwrap();
    std::fs::write(&entry, "import dep\nfn main(console: Console):\n    console.print(\"${dep.answer()}\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&entry).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");

    std::fs::write(folder.join("dep.witchy"), "comptime:\n    emit_item(quote item:\n        pub fn answer() -> Int:\n            \"wrong\"\n    )\n").unwrap();
    let expand = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("expand").arg(&entry).output().unwrap();
    assert!(!expand.status.success(), "invalid imported output was displayed as accepted syntax");
    let error = String::from_utf8_lossy(&expand.stderr);
    assert!(error.contains("Int") && error.contains("String"), "{error}");
}

#[test]
fn bare_test_driver_uses_admitted_original_block_helpers() {
    let folder = TempDir::new("admitted-comptime-test-driver");
    let source = folder.join("tests.witchy");
    std::fs::write(&source, "comptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            42\n    )\nfn test_answer():\n    if answer() != 42:\n        fail(\"wrong admitted helper\")\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("test").arg(&source).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));

    std::fs::write(&source, "comptime:\n    emit_item(quote item:\n        fn test_forged():\n            ()\n    )\n").unwrap();
    let forged = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("test").arg(&source).output().unwrap();
    assert!(!forged.status.success(), "generated syntax cannot select a raw test target");
    let error = format!("{}{}", String::from_utf8_lossy(&forged.stderr),
        String::from_utf8_lossy(&forged.stdout));
    assert!(error.contains("unknown or ambiguous entry test") || error.contains("declared in the entry source"), "{error}");
}

#[test]
fn imported_original_block_feeds_bare_test_driver() {
    let folder = TempDir::new("admitted-imported-comptime-test");
    let entry = folder.join("tests.witchy");
    let producer = folder.join("dep.witchy");
    std::fs::write(&producer, "comptime:\n    emit_item(quote item:\n        pub fn answer() -> Int:\n            42\n    )\n").unwrap();
    std::fs::write(&entry, "import dep\nfn test_answer():\n    if dep.answer() != 42:\n        fail(\"wrong imported helper\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("test").arg(&entry).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));

    std::fs::write(&producer, "comptime:\n    emit_item(quote item:\n        pub fn answer() -> Int:\n            \"wrong\"\n    )\n").unwrap();
    let invalid = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("test").arg(&entry).output().unwrap();
    assert!(!invalid.status.success(), "invalid imported output reached the test driver");
    let error = String::from_utf8_lossy(&invalid.stderr);
    assert!(error.contains("Int") && error.contains("String"), "{error}");
}
