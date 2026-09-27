//! Capability, authority and ABI-security pins.

mod canonical_abi {
    use witchy_canonical_source::abi::CanonicalCallableAbiManifest;
    use witchy_types::canonical_abi::*;
    use witchy_types::typeck::{self, TypedModule};
    use witchy_syntax::ast::Item;

    fn typed(source: &str) -> TypedModule {
        let module = witchy_syntax::parser::parse_module(source).expect("parse");
        typeck::annotate_checked(module).expect("type check")
    }

    fn manifest(source: &str) -> CanonicalCallableAbiManifest {
        let typed = typed(source);
        let functions = typed
            .module()
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(function) => Some(function.name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        derive_manifest(&typed, &functions).expect("manifest")
    }

    #[test]
    fn registry_secret_store_callable_abi_is_closed_and_kind_bound() {
        let store = manifest("fn keep(store: SecretStore) -> SecretStore:\n    store\n");
        let secret = manifest("fn keep(secret: Secret) -> Secret:\n    secret\n");
        let clock = manifest("fn keep(clock: Clock) -> Clock:\n    clock\n");
        assert_ne!(store.digest(), secret.digest());
        assert_ne!(store.digest(), clock.digest());
        assert_ne!(secret.digest(), clock.digest());
    }

    #[test]
    fn nested_int_list_slice_has_exact_checked_and_declared_identity() {
        let typed = typed("fn relay(view: &[List(Int)]) -> &[List(Int)]:\n    view\n");
        let context = Context::from_module(typed.module());
        let element = witchy_types::typeck::Ty::List(std::rc::Rc::new(witchy_types::typeck::Ty::Int));
        let checked = rooted_view_from_ty(&witchy_types::typeck::Ty::Named("Slice".into(), vec![element].into()), &context)
            .expect("exact nested-list slice has a checked carrier");
        let signature = checked_declaration_signature(&typed, "relay")
            .expect("checked declaration signature");
        let witchy_syntax::ast::Type::Fn(parameters, _, _, _) = signature else { panic!("callable signature") };
        let witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, declared_payload) = &parameters[0] else {
            panic!("rooted nested-list slice parameter")
        };
        let declared = rooted_view_from_ast(declared_payload, &context)
            .expect("exact declared carrier");
        assert_eq!(checked, declared);
        let whole = rooted_view_from_ty(
            &witchy_types::typeck::Ty::List(std::rc::Rc::new(witchy_types::typeck::Ty::List(std::rc::Rc::new(witchy_types::typeck::Ty::Int)))), &context,
        ).expect("whole nested-list root");
        assert_ne!(checked, whole, "slice projection and whole root cannot share an ABI key");

        let raw = witchy_syntax::ast::Type::Named("List".into(), vec![witchy_syntax::ast::Type::Named("Int".into(), Vec::new())]);
        for qualifier in [witchy_syntax::ast::TypeQual::Unique, witchy_syntax::ast::TypeQual::Frozen] {
            let near_miss = witchy_syntax::ast::Type::Slice(Box::new(witchy_syntax::ast::Type::Qualified(qualifier, Box::new(raw.clone()))));
            assert!(rooted_view_from_ast(&near_miss, &context).is_err(),
                "qualified child cannot inherit the exact nested-list slice key");
        }
    }

    #[test]
    fn qualified_rooted_view_manifest_keeps_checked_source_storage() {
        let with_option = |source: &str| format!("type Option:\n    Some(a)\n    None\n\n{source}");
        let ordinary = manifest(&with_option("fn relay(view: &Option(String)) -> &Option(String):\n    view\n"));
        let frozen = manifest(&with_option("fn relay(view: &Option(frozen String)) -> &Option(frozen String):\n    view\n"));
        assert_ne!(ordinary.digest(), frozen.digest(),
            "a frozen child cannot inherit the ordinary record-root ABI key");

        let qualified_list = typed(&with_option("fn relay(view: &Option(frozen List(Int))) -> &Option(frozen List(Int)):\n    view\n"));
        assert!(derive_manifest(&qualified_list, &["relay".into()]).is_err(),
            "an erased qualified List child must remain outside the record-root ABI");

        let nested = typed(&with_option("fn accept(f: fn(&Option(frozen String)) -> Int) -> Int:\n    0\n"));
        assert!(derive_manifest(&nested, &["accept".into()]).is_err(),
            "a nested qualified rooted-view callable needs a structural ABI key");

        let scalar = manifest("fn accept(f: fn(&Int) -> Int) -> Int:\n    0\n");
        let qualified_scalar = manifest("fn accept(f: fn(&frozen Int) -> Int) -> Int:\n    0\n");
        assert_ne!(scalar.digest(), qualified_scalar.digest(),
            "a nested scalar-place view retains its checked payload qualifier");

        let original_sum = manifest("type Shade:\n    Bright(Int)\n    Dark(String)\nfn relay(view: &Shade) -> &Shade:\n    view\n");
        let renamed_tag = manifest("type Shade:\n    Light(Int)\n    Dark(String)\nfn relay(view: &Shade) -> &Shade:\n    view\n");
        let changed_field = manifest("type Shade:\n    Bright(Duration)\n    Dark(String)\nfn relay(view: &Shade) -> &Shade:\n    view\n");
        assert_ne!(original_sum.digest(), renamed_tag.digest(),
            "a constructor rename cannot inherit the old nominal root key");
        assert_ne!(original_sum.digest(), changed_field.digest(),
            "a changed field type cannot inherit the old nominal root key");
        let original_record = manifest("type Boxed:\n    Original(Int)\nfn relay(view: &Boxed) -> &Boxed:\n    view\n");
        let renamed_record = manifest("type Boxed:\n    Renamed(Int)\nfn relay(view: &Boxed) -> &Boxed:\n    view\n");
        assert_ne!(original_record.digest(), renamed_record.digest(),
            "a one-constructor rename cannot inherit the old nominal root key");
        let original_label = manifest("type Holder:\n    first: Int\nfn relay(view: &Holder) -> &Holder:\n    view\n");
        let renamed_label = manifest("type Holder:\n    second: Int\nfn relay(view: &Holder) -> &Holder:\n    view\n");
        assert_ne!(original_label.digest(), renamed_label.digest(),
            "a field-label rename cannot inherit the old nominal root key");
        let nested = manifest("type Inner:\n    Value(Int)\ntype Outer:\n    Wrap(Inner)\nfn relay(view: &Outer) -> &Outer:\n    view\n");
        let changed_nested = manifest("type Inner:\n    Value(String)\ntype Outer:\n    Wrap(Inner)\nfn relay(view: &Outer) -> &Outer:\n    view\n");
        assert_ne!(nested.digest(), changed_nested.digest(),
            "a nested declaration change must change the outer root key");
    }
}

mod storage {
    use witchy_canonical_source::capabilities::externref_cap_name;

    #[test]
    fn externref_capability_set_is_explicit_and_closed() {
        for name in [
            "Dir", "File", "Env", "Exec", "Net", "Fetch", "Socket", "Listener", "Secret",
        ] {
            assert_eq!(externref_cap_name(name), Some(name));
        }
        for name in ["Console", "Clock", "Rand", "SecretStore"] {
            assert_eq!(externref_cap_name(name), None, "{name}");
        }
    }
}

mod traits {

    #[test]
    fn local_callable_result_does_not_erase_network_rights() {
        let error = witchy_types::typeck::check_str(
            "fn probe(net: Net[Listen, Tcp]) -> Fetch:\n    let acquire = fn() -> Net[Listen, Tcp]: net\n    let client = acquire()\n    client.fetch(\"http://127.0.0.1:1\")\n"
        ).unwrap_err();
        assert!(error.contains("Connect"), "{error}");
    }
}

mod pipeline {
    use witchy_types::pipeline::*;
    use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
    use witchy_syntax::{linker, ast::Module, origin::OriginTable};

    fn no_expand(
        _name: &str,
        _module: &mut Module,
        _siblings: &[(String, Module)],
    ) -> Result<OriginTable, String> {
        Ok(OriginTable::default())
    }

    fn dynamic_source_owners(package: &str) -> AuthenticatedModuleOwners {
        let application = PackageCoordinate::new(PackageSource::Workspace, package, "0.1.0")
            .expect("application coordinate");
        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0")
            .expect("toolchain coordinate");
        let mut assignments = vec![(
            "main".to_string(),
            ModuleLoadIdentity::new(application, ["main"]).expect("main owner"),
        )];
        assignments.extend(linker::STD_MODULES.iter().map(|module| (
            (*module).to_string(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *module]).expect("std owner"),
        )));
        AuthenticatedModuleOwners::from_loader_assignments(assignments).expect("owners")
    }

    #[test]
    fn canonical_dynamic_runtime_plan_keeps_capability_arguments_out_of_payloads() {
        use witchy_syntax::ast::{Expr, Item, Type};
        let source = b"import dynamic\n\n@dynamic\npub fn announce(self: Int, console: Console) -> Int:\n    console.print(\"called\")\n    self\n\nfn main(console: Console):\n    let value = dynamic.dynamic(7)\n    let _ = dynamic.call_with(value, \"announce\", [], console)\n";
        let checked = link_checked_source_bundle_with(
            vec![("main".into(), source.to_vec())], "main", witchy_syntax::linker::ComptimeExpander::new(no_expand), None,
            Some(dynamic_source_owners("example/dynamic-capability")),
            None,
        ).expect("capability arguments remain statically checked values");
        let canonical = checked.canonical_source().unwrap();
        let typed = canonical.typed_module();
        let plan = canonical.dynamic_runtime_plan().unwrap();
        let method = plan.methods().iter().find(|method| method.function == "main.announce").unwrap();
        assert!(matches!(method.parameters.as_slice(),
            [witchy_types::runtime_type::RuntimeMethodParameterDescriptor::Capability(capability)]
                if capability.ty == Type::Named("Console".into(), vec![])));
        let mut calls = 0;
        witchy_syntax::ast::visit::visit_module_exprs::<String>(typed.module(), &mut |expr| {
            if let Expr::Call { name, args } = expr && name == "main.announce" {
                calls += 1;
                assert_eq!(args.len(), 2);
                assert_eq!(typed.table().type_of(&args[1]).and_then(witchy_types::typeck::ty_to_ast),
                    Some(Type::Named("Console".into(), vec![])), "capability is not boxed or encoded as Int");
            }
            Ok(())
        }).unwrap();
        assert!(calls > 0, "authenticated method branch is generated");
        let payload = typed.module().items.iter().find_map(|item| match item {
            Item::Type(definition) if definition.name == "$canonical.DynamicPayload" => Some(definition),
            _ => None,
        }).unwrap();
        assert!(payload.variants.iter().all(|variant|
            variant.fields == vec![Type::Named("Int".into(), vec![])]));

        for (declaration, receiver) in [("", "Console"), ("type Stash:\n    console: Console\n\n", "Stash")] {
            let source = format!("import dynamic\n\n{declaration}@dynamic\npub fn bad(self: {receiver}) -> Int:\n    0\n\nfn main() -> Int:\n    0\n");
            let error = link_checked_source_bundle_with(
                vec![("main".into(), source.into_bytes())], "main", witchy_syntax::linker::ComptimeExpander::new(no_expand), None,
                Some(dynamic_source_owners("example/forbidden-payload")),
                None,
            ).expect_err("direct and transitive capability payloads must refuse");
            assert!(error.to_string().contains("must be capability-free"), "{error}");
        }
    }

    /// A rights-qualified capability parameter is inside the canonical ABI
    /// (RFC-0165 slice 42): the rights are encoded by name, so the program
    /// binds its manifest and records no callable-ABI obligation.
    #[test]
    fn checked_source_binds_rights_qualified_capability_parameters() {
        let checked = link_checked_from_source_bundle(
            vec![(
                "main".to_string(),
                b"fn peek(dir: Dir[Read]) -> Int:\n    1\n\nfn main(dir: Dir) -> Int:\n    peek(dir)\n".to_vec(),
            )],
            "main",
            witchy_syntax::linker::ComptimeExpander::new(no_expand),
        )
        .expect("a valid program with a rights-qualified capability checks");
        let canonical = checked.canonical_source().expect("canonical source");
        assert!(
            !canonical.obligations().iter().any(|obligation| matches!(obligation, CanonicalObligation::CallableAbi(_))),
            "{:?}",
            canonical.obligations()
        );
        assert!(canonical.closed_callable_abi("main").is_some(), "the entry binds its callable ABI");
        assert!(!canonical.callable_abi_manifest().entries().is_empty(), "the inventory is bound");
    }
}

mod runtime_type {
    use witchy_types::runtime_type::*;
    use witchy_syntax::ast::{CallableQualifiers, Convention, Type};

    #[test]
    fn type_derived_access_signature_cannot_claim_checked_runtime_authority() {
        let string = Type::Named("String".to_string(), Vec::new());
        let ty = Type::Fn(
            vec![string.clone()],
            Box::new(string),
            vec![Convention::Borrow],
            CallableQualifiers::ORDINARY,
        );
        let type_only_signature = witchy_canonical_source::access::AccessSignature::from_function_type(&ty)
            .expect("type-only callable signature");
        assert_eq!(
            type_only_signature.params()[0].kind(),
            witchy_canonical_source::access::AccessKind::SharedBorrow,
        );

        let public_identity = RuntimeTypeIdentity::from_resolved_type(&ty, &|_, _| None)
            .expect("public type-only runtime identity");
        let RuntimeTypeIdentity::Function { access, .. } = public_identity else {
            panic!("function type must produce a callable identity")
        };
        assert!(
            access.is_conservative(),
            "type-only access facts must remain conservative",
        );
        assert!(!access.is_checked());
    }

    #[test]
    fn unit_has_one_identity_and_capabilities_are_rejected() {
        let unit = RuntimeTypeIdentity::from_resolved_type(&Type::Tuple(Vec::new()), &|_, _| None)
            .expect("unit identity");
        assert_eq!(unit, RuntimeTypeIdentity::Primitive(PrimitiveType::Unit));

        let error = RuntimeTypeIdentity::from_resolved_type(
            &Type::Named("Console".to_string(), Vec::new()),
            &|_, _| None,
        )
        .expect_err("capability descriptors are forbidden");
        assert_eq!(error, RuntimeTypeError::CapabilityType("Console".to_string()));
    }

}
