use witchy_runtime::runtime::{Capabilities, Runtime, comptime::ComptimeLimits};
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
use super::temp_dir::TempDir;
use std::process::Command;

fn owners() -> AuthenticatedModuleOwners {
    owners_for(["generator"])
}

fn owners_for<'a>(user_modules: impl IntoIterator<Item = &'a str>) -> AuthenticatedModuleOwners {
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/ordered-comptime", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let assignments = user_modules.into_iter().map(|name| (
        name.to_owned(), ModuleLoadIdentity::new(workspace.clone(), [name]).unwrap(),
    )).chain(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
            (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
        )));
    AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap()
}

#[test]
fn admitted_local_tag_after_source_block_in_both_modes() {
    for mode in ["", "mode opt\n"] {
        let source = format!(r#"{mode}import meta
comptime fn literal(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
    meta.expr_int(42)
comptime:
    emit_item(quote item:
        fn helper() -> Int:
            1
    )
fn main(console: Console):
    console.print("${{literal"hello" + helper()}}")
"#);
        let checked = witchy::canonical_comptime::check_source_bundle_with_local_tag(
            vec![("generator".into(), source.into_bytes()),
                ("meta".into(), witchy_syntax::linker::bundled_source("meta").unwrap().as_bytes().to_vec())],
            "generator", ["generator".to_owned()].into_iter().collect(), owners(), limits(),
        ).unwrap_or_else(|error| panic!("local tag admitted in {mode:?}: {error}"));
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("local tag destination admits on Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["43"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

#[test]
fn admitted_typed_async_generation_in_both_modes() {
    for mode in ["", "mode opt\n"] {
        let source = format!(r#"{mode}import meta
comptime:
    let int_ty = quote type:
        Int
    let base_body = meta.block([], Some(meta.expr_int(42)))
    emit_item(meta.function_async(true, meta.ident("base"), [], Some(int_ty), base_body))
    let call = meta.expr_named_call(meta.ident("base"), [])
    let answer_body = meta.block([], Some(meta.expr_await(call)))
    emit_item(meta.function_async(true, meta.ident("answer"), [], Some(int_ty), answer_body))
async fn main(console: Console):
    console.print("${{base().await}}")
    console.print("${{answer().await}}")
"#);
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes()),
                ("meta".into(), witchy_syntax::linker::bundled_source("meta").unwrap().as_bytes().to_vec())],
            "generator", ["generator".to_owned()].into_iter().collect(), owners(), limits(),
        ).unwrap_or_else(|error| panic!("typed async item enters destination checking in {mode:?}: {error}"));
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("typed async item admits on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["42", "42"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

fn limits() -> ComptimeLimits {
    ComptimeLimits { fuel: 2_000_000, memory_pages: 64, fresh_identifiers: 4,
        host_output_bytes: 4096, syntax_nodes: 10_000, syntax_bytes: 100_000,
        emitted_nodes: 10_000, emitted_bytes: 100_000 }
}

#[test]
fn admitted_typed_unary_and_range_generation_in_both_modes() {
    for mode in ["", "mode opt\n"] {
        let source = format!(r#"{mode}import meta
comptime:
    let int_ty = quote type:
        Int
    let ints_ty = quote type:
        List(Int)
    let negative = meta.expr_unary("-", meta.expr_int(4))
    emit_item(meta.function(true, meta.ident("negative"), [], Some(int_ty), negative))
    let span = meta.expr_range(meta.expr_int(1), meta.expr_int(4), true)
    emit_item(meta.function(true, meta.ident("range_values"), [], Some(ints_ty), span))
fn main(console: Console):
    let values = range_values()
    console.print("${{negative() + values[0] + values.length()}}")
"#);
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes()),
                ("meta".into(), witchy_syntax::linker::bundled_source("meta").unwrap().as_bytes().to_vec())],
            "generator", ["generator".to_owned()].into_iter().collect(), owners(), limits(),
        ).expect("typed unary and range items enter destination checking");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("typed unary and range admit on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["1"]); // -(4) + 1 + four inclusive range values
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
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
    email: String
comptime fn builder_items(info: meta.TypeInfo) -> List(meta.ItemSyntax):
    builder_items_for(info, meta.type_expr(meta.TNamed(info.name, [])), meta.type_expr(meta.TNamed("Option", [meta.TNamed(info.name, [])])))
comptime fn builder_items_for(info: meta.TypeInfo, user_ty: meta.TypeSyntax, build_ty: meta.TypeSyntax) -> List(meta.ItemSyntax):
    let builder_name = info.name + "Builder"
    let builder_ty = meta.type_expr(meta.TNamed(builder_name, []))
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
    let user = meta.expr_record_type(user_ty, user_values)
    methods.push(meta.function(true, meta.ident("build"), [meta.param_own(meta.ident("self"), builder_ty)],
        Some(build_ty),
        meta.expr_call(some_expr, [user])))
    [meta.record_type(meta.ident(builder_name), slots), meta.inherent_impl(builder_ty, methods)]
comptime:
    for item in builder_items(type_info(User)):
        emit_item(item)
fn main(console: Console):
    let complete = UserBuilder(None, None, None).with_name("Ada").with_age(42).with_email("ada@example.com").build()
    let incomplete = UserBuilder(None, None, None).with_name("Ada").build()
    match complete:
        Some(user) -> console.print("${user.name}/${user.age}/${user.email}")
        None -> console.print("missing")
    match incomplete:
        Some(_) -> console.print("unexpected")
        None -> console.print("None")
"#;
    for mode in ["", "mode opt\n"] {
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![
                ("generator".into(), format!("{mode}{program}").into_bytes()),
                ("meta".into(), witchy_syntax::linker::bundled_source("meta")
                    .expect("bundled meta").as_bytes().to_vec()),
            ],
            "generator", ["generator".to_owned()].into_iter().collect(),
            owners(), witchy::canonical_comptime::source_limits(),
        ).expect("reflected builder enters ordinary destination checking");
        let generated_origins = checked.module().items.iter().enumerate().filter_map(|(index, item)| {
            let is_builder = match item {
                witchy_syntax::ast::Item::Type(ty) => ty.name.ends_with("UserBuilder"),
                witchy_syntax::ast::Item::Impl(implementation) =>
                    implementation.type_name.ends_with("UserBuilder"),
                _ => false,
            };
            is_builder.then(|| (
                index,
                checked.module().item_lines.get(index).copied().unwrap_or(0),
                checked.origins().origin_for_item(index).map(|entry| (
                    entry.origin.definition.start.line,
                    entry.origin.invocation.start.line,
                )),
            ))
        }).collect::<Vec<_>>();
        assert_eq!(generated_origins.len(), 2, "{generated_origins:?}");
        assert!(generated_origins.iter().all(|(_, _, origin)| matches!(origin, Some((definition, invocation))
            if *definition > 1 && *invocation > 1)),
            "builder items need source origins: {generated_origins:?}");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("generated builder admits on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["Ada/42/ada@example.com", "None"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));

        if mode.is_empty() {
            let stale = program.replace("UserBuilder(None, None, None)", "UserBuilder(None, None)")
                .replace(".with_age(42).with_email(\"ada@example.com\")", ".with_age(42)");
            std::fs::write(&source, format!("{mode}{stale}")).unwrap();
            let stale = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
            assert!(!stale.status.success(), "old two-slot construction must fail with the added field");
        }
    }

    let (builder_definition, _) = program.split_once("fn main(console: Console):")
        .expect("builder source has an entry point");
    std::fs::write(folder.join("dep.witchy"), format!("mode opt\n{builder_definition}")).unwrap();
    let entry = r#"import dep
fn main(console: Console):
    let complete = dep.UserBuilder(None, None, None).with_name("Ada").with_age(42).with_email("ada@example.com").build()
    let incomplete = dep.UserBuilder(None, None, None).with_name("Ada").build()
    match complete:
        Some(user) -> console.print("${user.name}/${user.age}/${user.email}")
        None -> console.print("missing")
    match incomplete:
        Some(_) -> console.print("unexpected")
        None -> console.print("None")
"#;
    std::fs::write(&source, entry).unwrap();
    let normal_to_opt = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(normal_to_opt.status.success(), "{}", String::from_utf8_lossy(&normal_to_opt.stderr));
    assert_eq!(String::from_utf8_lossy(&normal_to_opt.stdout), "Ada/42/ada@example.com\nNone\n");

    let reused_name = entry.replace(
        "    let complete = dep.UserBuilder(None, None, None).with_name(\"Ada\")",
        "    let name = \"Ada\"\n    let complete = dep.UserBuilder(None, None, None).with_name(name)",
    ) + "    console.print(name)\n";
    std::fs::write(&source, reused_name).unwrap();
    let rejected = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(!rejected.status.success(), "normal caller cannot reuse a value consumed by an opt setter");
    let error = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        ["consumed", "moved", "transferred", "ownership", "unique"].iter()
            .any(|word| error.contains(word)),
        "{error}",
    );

    let imported = program
        .replacen("type User:\n    name: String\n    age: Int\n    email: String\n", "from dep_normal import User\n", 1)
        .replacen(
            "    for item in builder_items(type_info(User)):\n        emit_item(item)",
            "    for item in builder_items(type_info(dep_normal.User)):\n        emit_item(item)\n    for item in builder_items_for(type_info(dep_normal.Account), meta.type_expr(meta.TNamed(\"dep_normal.Account\", [])), meta.type_expr(meta.TNamed(\"Option\", [meta.TNamed(\"dep_normal.Account\", [])]))):\n        emit_item(item)\n    for item in builder_items(type_info(dep_opt.OptUser)):\n        emit_item(item)\n    for item in builder_items_for(type_info(dep_opt.OptAccount), meta.type_expr(meta.TNamed(\"dep_opt.OptAccount\", [])), meta.type_expr(meta.TNamed(\"Option\", [meta.TNamed(\"dep_opt.OptAccount\", [])]))):\n        emit_item(item)",
            1,
        );
    let imported = imported.replace(
        "fn main(console: Console):\n    let complete = UserBuilder(None, None, None).with_name(\"Ada\").with_age(42).with_email(\"ada@example.com\").build()",
        "fn main(console: Console):\n    let account = AccountBuilder(None, None).with_name(\"Grace\").with_age(37).build()\n    let opt_complete = OptUserBuilder(None, None).with_name(\"Beth\").with_age(28).build()\n    let opt_incomplete = OptUserBuilder(None, None).with_name(\"Beth\").build()\n    let opt_account = OptAccountBuilder(None, None).with_name(\"Ivy\").with_age(33).build()\n    let complete = UserBuilder(None, None, None).with_name(\"Ada\").with_age(42).with_email(\"ada@example.com\").build()",
    ).replace(
        "        None -> console.print(\"None\")\n",
        "        None -> console.print(\"None\")\n    match account:\n        Some(value) -> console.print(\"${value.name}/${value.age}\")\n        None -> console.print(\"missing account\")\n    match opt_complete:\n        Some(value) -> console.print(\"${value.name}/${value.age}\")\n        None -> console.print(\"missing opt user\")\n    match opt_incomplete:\n        Some(_) -> console.print(\"unexpected opt user\")\n        None -> console.print(\"None\")\n    match opt_account:\n        Some(value) -> console.print(\"${value.name}/${value.age}\")\n        None -> console.print(\"missing opt account\")\n",
    );
    let imported = format!("import dep_normal\nimport dep_opt\nfrom dep_opt import OptUser, OptAccount\n{imported}");
    std::fs::write(folder.join("dep_normal.witchy"),
        "type User:\n    name: String\n    age: Int\n    email: String\ntype Account:\n    name: String\n    age: Int\n").unwrap();
    std::fs::write(folder.join("dep_opt.witchy"),
        "mode opt\ntype OptUser:\n    name: String\n    age: Int\ntype OptAccount:\n    name: String\n    age: Int\n").unwrap();
    std::fs::write(&source, &imported).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "Ada/42/ada@example.com\nNone\nGrace/37\nBeth/28\nNone\nIvy/33\n");
}

#[test]
fn admitted_type_info_uses_imported_definition_identity_across_modes() {
    let folder = TempDir::new("admitted-imported-type-info");
    let source = folder.join("main.witchy");
    let dependency = folder.join("dep.witchy");
    for (dependency_mode, entry_mode) in [("", ""), ("mode opt\n", ""), ("mode opt\n", "mode opt\n")] {
        std::fs::write(&dependency, format!("{dependency_mode}type Inner:\n    value: Int\ntype Wrapper:\n    child: Inner\ntype Boxed(a):\n    item: a\n")).unwrap();
        std::fs::write(folder.join("other.witchy"), format!("{entry_mode}type Inner:\n    value: Int\n")).unwrap();
        std::fs::write(&source, format!("{entry_mode}import dep\nimport other\nfrom dep import Boxed\nimport meta\ncomptime:\n    let wrapped = type_info(dep.Wrapper)\n    let boxed = type_info(Boxed(Int))\n    let qualified = type_info(dep.Boxed(Int))\n    let caller_boxed = type_info(dep.Boxed(other.Inner))\n    match wrapped.fields[0].type_expr:\n        meta.TNamed(name, _) ->\n            match caller_boxed.fields[0].type_expr:\n                meta.TNamed(caller_name, _) ->\n                    if name == \"dep.Inner\" && caller_name == \"other.Inner\" && boxed.fields[0].name == \"item\" && qualified.fields[0].name == \"item\":\n                        emit_item(quote item:\n                            fn answer() -> Int:\n                                44\n                        )\n                    else: fail(\"wrong imported type identity\")\n                _ -> fail(\"wrong caller type argument\")\n        _ -> fail(\"wrong imported field type\")\nfn main(console: Console):\n    console.print(\"${{answer()}}\")\n")).unwrap();
        let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(String::from_utf8_lossy(&run.stdout), "44\n");
    }

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
fn type_info_refuses_runtime_use_and_unknown_nominals() {
    let folder = TempDir::new("type-info-refusals");
    let source = folder.join("main.witchy");
    for (program, expected) in [
        ("import meta\ntype User:\n    name: String\nfn main() -> Int:\n    let info = type_info(User)\n    0\n", "unknown function `type_info`"),
        ("import meta\ncomptime:\n    let info = type_info(Missing)\nfn main() -> Int:\n    0\n", "unknown type `Missing`"),
    ] {
        std::fs::write(&source, program).unwrap();
        let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg("check").arg(&source).output().unwrap();
        assert!(!run.status.success(), "type_info must stay in the checked compile-time type namespace");
        let error = String::from_utf8_lossy(&run.stderr);
        assert!(error.contains(expected), "{error}");
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
fn imported_blocks_follow_admitted_dependency_graph() {
    let positive_sources = vec![
        ("left".into(), "comptime:\n    emit_item(quote item:\n        pub fn value() -> Int:\n            19\n    )\n".as_bytes().to_vec()),
        ("right".into(), "import left\ncomptime:\n    if left.value() == 19:\n        emit_item(quote item:\n            pub fn value() -> Int:\n                23\n        )\n".as_bytes().to_vec()),
        ("extra".into(), "comptime:\n    emit_item(quote item:\n        pub fn value() -> Int:\n            23\n    )\n".as_bytes().to_vec()),
    ];
    let main_source = "import left\nimport right\nimport extra\ncomptime:\n    if right.value() == 23:\n        emit_item(quote item:\n            fn answer() -> Int:\n                left.value() + extra.value()\n        )\nfn main(console: Console):\n    console.print(\"${answer()}\")\n";
    let producers = ["left", "right", "extra", "main"].map(str::to_owned);
    let user_modules = std::collections::HashSet::from(["left".into(), "right".into(), "extra".into(), "main".into()]);
    let mut sources = positive_sources;
    sources.push(("main".into(), main_source.as_bytes().to_vec()));
    let (checked, _) = witchy::canonical_comptime::check_and_expand_source_bundle_with_producer_graph(
        sources, "main", &producers, user_modules.clone(),
        owners_for(["left", "right", "extra", "main"]), limits(),
    ).expect("valid producer graph expands in dependency order");
    let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("expanded producer graph admits on unoptimized Wasm");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), ["42"], "independent dependency result: left 19 + extra 23");
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));

    let sources = vec![
        ("left".into(), "comptime:\n    emit_item(quote item:\n        pub fn value() -> Int:\n            \"wrong\"\n    )\n".as_bytes().to_vec()),
        ("right".into(), "import left\ncomptime:\n    fail(\"later producer ran\")\n".as_bytes().to_vec()),
        ("extra".into(), "comptime:\n    emit_item(quote item:\n        pub fn value() -> Int:\n            23\n    )\n".as_bytes().to_vec()),
        ("main".into(), main_source.as_bytes().to_vec()),
    ];
    let check = witchy::canonical_comptime::check_and_expand_source_bundle_with_producer_graph(
        sources, "main", &producers, user_modules, owners_for(["left", "right", "extra", "main"]), limits(),
    ).expect_err("invalid first producer must stop the dependent graph");
    assert!(check.contains("Int") && check.contains("String"), "{check}");
    assert!(!check.contains("later producer ran"), "{check}");
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
    let relay = folder.join("relay.witchy");
    std::fs::write(&producer, "comptime:\n    emit_item(quote item:\n        pub fn answer() -> Int:\n            42\n    )\n").unwrap();
    std::fs::write(&relay, "import dep\ncomptime:\n    if dep.answer() == 42:\n        emit_item(quote item:\n            pub fn answer() -> Int:\n                dep.answer()\n        )\n").unwrap();
    std::fs::write(&entry, "import relay\nfn test_answer():\n    if relay.answer() != 42:\n        fail(\"wrong imported helper\")\n").unwrap();
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

#[test]
fn admitted_typed_binary_and_index_generation_in_both_modes() {
    for mode in ["", "mode opt\n"] {
        let source = format!(r#"{mode}import meta
comptime:
    let int_ty = quote type:
        Int
    let answer = meta.expr_binary("+", meta.expr_int(39), meta.expr_int(3))
    emit_item(meta.function(true, meta.ident("answer"), [], Some(int_ty), answer))
    let values = quote expr:
        [8, 9]
    let second = meta.expr_index(values, meta.expr_int(1))
    emit_item(meta.function(true, meta.ident("second"), [], Some(int_ty), second))
fn main(console: Console):
    console.print("${{answer() + second()}}")
"#);
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes()),
                ("meta".into(), witchy_syntax::linker::bundled_source("meta").unwrap().as_bytes().to_vec())],
            "generator", ["generator".to_owned()].into_iter().collect(), owners(), limits(),
        ).expect("typed binary and index items enter destination checking");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("typed binary and index admit on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["51"]); // (39 + 3) + [8, 9][1]
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}
#[test]
fn admitted_typed_list_and_tuple_generation_in_both_modes() {
    for mode in ["", "mode opt\n"] {
        let source = format!(r#"{mode}import meta
comptime:
    let list_ty = quote type:
        List(Int)
    let pair_ty = quote type:
        (Int, Int)
    let values = meta.expr_list([meta.expr_int(5), meta.expr_int(6)])
    emit_item(meta.function(true, meta.ident("values"), [], Some(list_ty), values))
    let pair = meta.expr_tuple([meta.expr_int(7), meta.expr_int(8)])
    emit_item(meta.function(true, meta.ident("pair"), [], Some(pair_ty), pair))
fn main(console: Console):
    let xs = values()
    let p = pair()
    console.print("${{xs[0] + xs[1] + p.0 + p.1}}")
"#);
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes()),
                ("meta".into(), witchy_syntax::linker::bundled_source("meta").unwrap().as_bytes().to_vec())],
            "generator", ["generator".to_owned()].into_iter().collect(), owners(), limits(),
        ).expect("typed list and tuple items enter destination checking");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("typed list and tuple admit on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["26"]); // 5 + 6 + 7 + 8
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}
#[test]
fn admitted_typed_for_while_and_assignment_generation_in_both_modes() {
    for mode in ["", "mode opt\n"] {
        let source = format!(r#"{mode}import meta
comptime:
    let int_ty = quote type:
        Int
    let sum = meta.ident("sum")
    let i = meta.ident("i")
    let add = quote expr:
        sum + i
    let for_body = meta.block([meta.stmt_assign(sum, add)], None)
    let span = meta.expr_range(meta.expr_int(1), meta.expr_int(4), true)
    let for_loop = meta.expr_for(i, span, for_body)
    let condition = quote expr:
        sum < 12
    let increment = quote expr:
        sum + 1
    let while_body = meta.block([meta.stmt_assign(sum, increment)], None)
    let while_loop = meta.expr_while(condition, while_body)
    let result = quote expr:
        sum
    let body = meta.block([meta.stmt_let(true, sum, Some(int_ty), meta.expr_int(0)), meta.stmt_expr(for_loop), meta.stmt_expr(while_loop)], Some(result))
    emit_item(meta.function_block(true, meta.ident("sum_loops"), [], Some(int_ty), body))
fn main(console: Console):
    console.print("${{sum_loops()}}")
"#);
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes()),
                ("meta".into(), witchy_syntax::linker::bundled_source("meta").unwrap().as_bytes().to_vec())],
            "generator", ["generator".to_owned()].into_iter().collect(), owners(), limits(),
        ).expect("typed loop and assignment items enter destination checking");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("typed loops admit on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["12"]); // 1 + 2 + 3 + 4, then two increments
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}
#[test]
fn admitted_typed_pattern_let_generation_in_both_modes() {
    for mode in ["", "mode opt\n"] {
        let source = format!(r#"{mode}import meta
comptime:
    let int_ty = quote type:
        Int
    let pair = meta.expr_tuple([meta.expr_int(20), meta.expr_int(22)])
    let pattern = meta.pattern_tuple([meta.pattern_var(meta.ident("left")), meta.pattern_var(meta.ident("right"))])
    let answer = quote expr:
        left + right
    let body = meta.block([meta.stmt_let_pattern(pattern, pair)], Some(answer))
    emit_item(meta.function_block(true, meta.ident("answer"), [], Some(int_ty), body))
fn main(console: Console):
    console.print("${{answer()}}")
"#);
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes()),
                ("meta".into(), witchy_syntax::linker::bundled_source("meta").unwrap().as_bytes().to_vec())],
            "generator", ["generator".to_owned()].into_iter().collect(), owners(), limits(),
        ).expect("typed destructuring item enters destination checking");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("typed destructuring admits on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["42"]); // 20 + 22
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

#[test]
fn admitted_typed_while_let_generation_in_both_modes() {
    for mode in ["", "mode opt\n"] {
        let source = format!(r#"{mode}import meta
comptime:
    let int_ty = quote type:
        Int
    let option_ty = quote type:
        Option(Int)
    let current = meta.ident("current")
    let sum = meta.ident("sum")
    let item = meta.ident("item")
    let some = quote expr:
        Some(1)
    let current_ref = quote expr:
        current
    let add = quote expr:
        sum + item
    let none = quote expr:
        None
    let result = quote expr:
        sum
    let pattern = meta.pattern_ctor(meta.ident("Some"), [meta.pattern_var(item)])
    let loop_body = meta.block([meta.stmt_assign(sum, add), meta.stmt_assign(current, none)], None)
    let loop_expr = meta.expr_while_let(pattern, current_ref, loop_body)
    let body = meta.block([meta.stmt_let(true, current, Some(option_ty), some), meta.stmt_let(true, sum, Some(int_ty), meta.expr_int(0)), meta.stmt_expr(loop_expr)], Some(result))
    emit_item(meta.function_block(true, meta.ident("sum_once"), [], Some(int_ty), body))
fn main(console: Console):
    console.print("${{sum_once()}}")
"#);
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes()),
                ("meta".into(), witchy_syntax::linker::bundled_source("meta").unwrap().as_bytes().to_vec())],
            "generator", ["generator".to_owned()].into_iter().collect(), owners(), limits(),
        ).expect("typed while-let item enters destination checking");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("typed while-let admits on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["1"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

#[test]
fn admitted_typed_method_call_generation_in_both_modes() {
    for mode in ["", "mode opt\n"] {
        let source = format!(r#"{mode}import meta
comptime:
    let int_ty = quote type:
        Int
    let receiver = meta.expr_string("abc")
    let body = meta.expr_method_call(receiver, meta.ident("length"), [])
    emit_item(meta.function(true, meta.ident("answer"), [], Some(int_ty), body))
fn main(console: Console):
    console.print("${{answer()}}")
"#);
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes()),
                ("meta".into(), witchy_syntax::linker::bundled_source("meta").unwrap().as_bytes().to_vec())],
            "generator", ["generator".to_owned()].into_iter().collect(), owners(), limits(),
        ).expect("typed method call enters destination checking");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("typed method call admits on unoptimized Wasm");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["3"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}
