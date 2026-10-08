//! Black-box codegen tests: everything here drives the crate's PUBLIC API
//! (the canonical `compile_checked_*` entries). The body keeps one level of
//! indentation inside `mod tests` so embedded witchy source literals
//! (indentation-sensitive!) stay byte-identical.

mod tests {
    use witchy_lower::codegen::*;
    use witchy_syntax::parser::parse_module;

    #[test]
    fn operation_catalog_names_are_intrinsics() {
        use witchy_syntax::intrinsics;

        for operations in [
            intrinsics::STRING_OPERATIONS,
            intrinsics::MATH_OPERATIONS,
            intrinsics::LIST_OPERATIONS,
            intrinsics::DICT_OPERATIONS,
            intrinsics::CRYPTO_OPERATIONS,
            intrinsics::SECRETSTORE_OPERATIONS,
            intrinsics::REGEX_OPERATIONS,
        ] {
            for &name in operations {
                let spec = intrinsics::lookup(name).expect("cataloged operation");
                assert!(
                    witchy_types::typeck::intrinsic(name),
                    "{} must not compile its self-recursive std placeholder",
                    spec.name
                );
            }
        }
    }

    fn gc_wasm_features() -> wasmparser::WasmFeatures {
        wasmparser::WasmFeatures::default()
            | wasmparser::WasmFeatures::GC
            | wasmparser::WasmFeatures::REFERENCE_TYPES
            | wasmparser::WasmFeatures::FUNCTION_REFERENCES
    }

    fn gc_wasm_payloads(
        wasm: &[u8],
    ) -> impl Iterator<Item = wasmparser::Result<wasmparser::Payload<'_>>> {
        wasmparser::Validator::new_with_features(gc_wasm_features())
            .validate_all(wasm)
            .expect("valid Wasm GC module");
        wasmparser::Parser::new(0).parse_all(wasm)
    }

    fn import_param_types(wasm: &[u8]) -> std::collections::BTreeMap<String, Vec<wasmparser::ValType>> {
        let mut func_param_types = Vec::new();
        let mut imports = Vec::new();
        for payload in gc_wasm_payloads(wasm) {
            match payload.expect("valid wasm") {
                wasmparser::Payload::TypeSection(reader) => {
                    for group in reader {
                        for ty in group.expect("recursive type group").into_types() {
                            let params = match ty.composite_type.inner {
                                wasmparser::CompositeInnerType::Func(func) => {
                                    Some(func.params().to_vec())
                                }
                                _ => None,
                            };
                            func_param_types.push(params);
                        }
                    }
                }
                wasmparser::Payload::ImportSection(reader) => {
                    for imp in reader.into_imports() {
                        let imp = imp.expect("import");
                        if let wasmparser::TypeRef::Func(idx) = imp.ty {
                            imports.push((imp.name.to_string(), idx as usize));
                        }
                    }
                }
                _ => {}
            }
        }
        imports
            .into_iter()
            .map(|(name, idx)| {
                let params = func_param_types
                    .get(idx)
                    .and_then(|params| params.as_ref())
                    .unwrap_or_else(|| panic!("missing function type {idx} for import {name}"));
                (name, params.clone())
            })
            .collect()
    }

    #[test]
    fn grantful_build_primitives_compile_to_build_imports_only() {
        use witchy_types::runtime_type::{
            AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
        };
        use witchy_wir::CapabilityKind::{BuildEnv, BuildExec, BuildOut};

        // BuildNet is still unsupported by this production baseline. Prove the
        // admitted BuildEnv/BuildExec family through the real source recipe.
        let source = "import option\nfn build(out: BuildOut, env: BuildEnv, cc: BuildExec):\n    let v = match env.get_build_env(\"WITCHY_BUILD_ALLOWED\"):\n        Some(x) -> x\n        None -> \"unset\"\n    out.write_out(\"x.witchy\", v + cc.run_tool(\"cat\", \"input\"))\n";
        let workspace = PackageCoordinate::new(
            PackageSource::Workspace, "example/build-import-test", "0.1.0",
        ).expect("workspace coordinate");
        let toolchain = PackageCoordinate::new(
            PackageSource::Toolchain, "witchy/stdlib", "0.1.0",
        ).expect("toolchain coordinate");
        let mut assignments = vec![(
            "main".to_owned(),
            ModuleLoadIdentity::new(workspace, ["main"]).expect("main owner"),
        )];
        assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
            (*name).to_owned(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).expect("std owner"),
        )));
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
            .expect("authenticated fixture ownership");
        let users = std::collections::HashSet::from(["main".to_owned()]);
        let recipe = witchy_types::pipeline::check_build_recipe_from_source_bundle(
            vec![("main".to_owned(), source.as_bytes().to_vec())],
            "main", witchy_syntax::linker::ComptimeExpander::new(no_comptime_expansion), &users, owners,
        ).expect("check original build source and generated recipe");
        let artifact = witchy_lower::codegen::compile_checked_build_recipe_artifact(&recipe)
            .expect_lowered("admit grantful build recipe");
        assert_eq!(artifact.entry().expect("build entry").capabilities(), &[BuildOut, BuildEnv, BuildExec]);
        let wasm = artifact.wasm();
        let mut imports = Vec::new();
        for payload in gc_wasm_payloads(&wasm) {
            if let wasmparser::Payload::ImportSection(reader) = payload.expect("valid wasm") {
                for imp in reader.into_imports() {
                    imports.push(imp.expect("import").name.to_string());
                }
            }
        }

        for needed in [
            "build_out_write_at",
            "build_env_at",
            "build_response_is_some",
            "build_read_response_size",
            "build_read_response_into",
            "build_exec_run",
        ] {
            assert!(imports.iter().any(|i| i == needed), "build import `{needed}` missing: {imports:?}");
        }
        for forbidden in [
            "env_len", "env_fill", "exec_run", "exec_run_at", "env_get_at", "cap_response_status",
            "net_connect",
            "build_out_write", "build_env_len", "build_env_fill", "build_fetch_len",
        ] {
            assert!(
                !imports.iter().any(|i| i == forbidden),
                "build primitive must not lower to runtime import `{forbidden}`: {imports:?}"
            );
        }
        let params = import_param_types(wasm);
        for (name, count) in [("build_out_write_at", 5), ("build_env_at", 3), ("build_exec_run", 5)] {
            let types = params.get(name).expect("required capability import");
            assert_eq!(types.len(), count, "checked slice ABI for {name}");
            assert_eq!(types.first(), Some(&wasmparser::ValType::EXTERNREF),
                "{name} must carry a capability reference, not an integer");
            assert!(types[1..].iter().all(|ty| *ty == wasmparser::ValType::I32),
                "{name} must use explicit pointer/length lanes after its capability");
        }
    }

    fn no_comptime_expansion(
        _name: &str,
        _module: &mut witchy_syntax::ast::Module,
        _modules: &[(String, witchy_syntax::ast::Module)],
    ) -> Result<witchy_syntax::origin::OriginTable, String> {
        Ok(witchy_syntax::origin::OriginTable::default())
    }

    fn compile_authenticated_dynamic(src: &str) -> LoweringOutcome<Vec<u8>> {
        use witchy_types::runtime_type::{
            AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
        };

        fn no_expand(
            _name: &str,
            _module: &mut witchy_syntax::ast::Module,
            _siblings: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let module = parse_module(src).expect("parse Dynamic Wasm fixture");
        let workspace = PackageCoordinate::new(
            PackageSource::Workspace,
            "example/dynamic-test",
            "0.1.0",
        )
        .expect("workspace coordinate");
        let toolchain = PackageCoordinate::new(
            PackageSource::Toolchain,
            "witchy/stdlib",
            "0.1.0",
        )
        .expect("toolchain coordinate");
        let mut assignments = vec![(
            "main".to_string(),
            ModuleLoadIdentity::new(workspace, ["main"]).expect("main owner"),
        )];
        assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|std_module| {
            (
                (*std_module).to_string(),
                ModuleLoadIdentity::new(toolchain.clone(), ["std", *std_module])
                    .expect("std owner"),
            )
        }));
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
            .expect("authenticated owners");
        let checked = witchy_types::pipeline::link_checked_authenticated(
            vec![("main".into(), module)],
            "main",
            witchy_syntax::linker::ComptimeExpander::new(no_expand),
            owners,
        )
        .expect("authenticated Dynamic checked link");
        compile_checked_module_binary(&checked)
    }

    #[test]
    fn dynamic_descriptor_without_source_witness_rejects_fail_closed() {
        let error = compile_authenticated_dynamic(
            "import dynamic\nimport reflect\n\ntype User:\n    name: String\n    age: Int\n\ntype Box(a):\n    Box(a)\n\nimpl Reflect for User:\n    fn reflect(self) -> reflect.Mirror:\n        reflect.MNil\n\nimpl Reflect for Box(a):\n    fn reflect(self) -> reflect.Mirror:\n        reflect.MNil\n\nfn main(console: Console):\n    let value = dynamic.dynamic(7)\n    console.print(dynamic.type_name(dynamic.type_of(value)))\n    console.print(dynamic.type_name(dynamic.runtime_type(Int)))\n    console.print(dynamic.type_name(dynamic.runtime_type(User)))\n    console.print(dynamic.type_name(dynamic.runtime_type(Box(Int))))\n    console.print(dynamic.type_name(dynamic.runtime_type(.{age: Int, name: String})))\n    console.print(dynamic.type_name(dynamic.runtime_type(.[Count(Int) | Label(String)])))\n    let exact: Option(Int) = dynamic.try_decode(value)\n    match exact:\n        Some(number) -> console.print(\"${number}\")\n        None -> console.print(\"missing-int\")\n    let mismatch: Option(String) = dynamic.try_decode(value)\n    match mismatch:\n        Some(text) -> console.print(text)\n        None -> console.print(\"none\")\n    let decoded: Result(Int, dynamic.DynamicError) = dynamic.decode(value)\n    match decoded:\n        Ok(number) -> console.print(\"decoded-${number}\")\n        Err(_) -> console.print(\"decode-failed\")\n    let wrong: Result(String, dynamic.DynamicError) = dynamic.decode(value)\n    match wrong:\n        Ok(text) -> console.print(text)\n        Err(dynamic.TypeMismatch(actual)) -> console.print(\"mismatch-${dynamic.type_name(actual)}\")\n        Err(_) -> console.print(\"unexpected-dynamic-error\")\n    let person = dynamic.dynamic(User(\"Ada\", 42))\n    let decoded_person: Option(User) = dynamic.try_decode(person)\n    match decoded_person:\n        Some(user) -> console.print(user.name)\n        None -> console.print(\"missing-user\")\n    let words = dynamic.dynamic([\"alpha\", \"beta\"])\n    let decoded_words: Option(List(String)) = dynamic.try_decode(words)\n    match decoded_words:\n        Some(items) -> console.print(list.at(items, 1))\n        None -> console.print(\"missing-words\")\n    let boxed = dynamic.dynamic(Box(11))\n    let decoded_box: Option(Box(Int)) = dynamic.try_decode(boxed)\n    match decoded_box:\n        Some(Box(number)) -> console.print(\"box-${number}\")\n        None -> console.print(\"missing-box\")\n    let record = dynamic.dynamic(.{name: \"Nia\", age: 9})\n    let decoded_record: Option(.{age: Int, name: String}) = dynamic.try_decode(record)\n    match decoded_record:\n        Some(found) -> console.print(found.name)\n        None -> console.print(\"missing-record\")\n    let choice: .[Count(Int) | Label(String)] = .Count(5)\n    let encoded_choice = dynamic.dynamic(choice)\n    let decoded_choice: Option(.[Count(Int) | Label(String)]) = dynamic.try_decode(encoded_choice)\n    match decoded_choice:\n        Some(.Count(number)) -> console.print(\"count-${number}\")\n        Some(.Label(label)) -> console.print(label)\n        None -> console.print(\"missing-choice\")\n",
        );
        let error = error.expect_rejected(
            "Dynamic descriptor aggregate decoding remains fail-closed until its canonical source witness is admitted",
        );
        assert!(
            error.to_string().contains("MissingCheckedSourceWitness"),
            "unexpected Dynamic rejection: {error}"
        );
    }

    #[test]
    fn forced_copy_dict_update_without_source_witness_rejects_fail_closed() {
        let default = witchy_syntax::opt::OptSet::default_set();
        witchy_syntax::opt::set_for_tests(Some(default.without(witchy_syntax::opt::Opt::InPlace)));
        let app = parse_module(include_str!("../../../benchmarks/knucleotide.witchy"))
            .expect("parse forced-copy knucleotide fixture");
        let checked = witchy_types::pipeline::link_checked(
            vec![("app".to_string(), app)],
            "app",
            witchy_syntax::linker::ComptimeExpander::new(no_comptime_expansion),
        )
        .expect("link forced-copy knucleotide fixture");
        let result = compile_checked_module_binary(&checked);
        witchy_syntax::opt::set_for_tests(None);
        let error = result.expect_rejected("forced-copy knucleotide remains fail-closed without a checked source witness");
        assert!(
            error.to_string().contains("MissingCheckedSourceWitness"),
            "unexpected forced-copy rejection: {error}"
        );
    }
}

#[path = "codegen/canonical_layouts.rs"]
mod canonical_layouts;
#[path = "codegen/canonical_pipeline.rs"]
mod canonical_pipeline;
#[path = "codegen/storage_move.rs"]
mod storage_move;
#[path = "codegen/capture_free_closure.rs"]
mod capture_free_closure;
#[path = "codegen/unique_fields.rs"]
mod unique_fields;
#[path = "codegen/byte_overwrite.rs"]
mod byte_overwrite;
