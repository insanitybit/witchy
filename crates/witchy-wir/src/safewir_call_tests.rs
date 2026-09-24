use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::Type;

use crate::layout::{
    ClosedTypeResolver, LayoutBundle, LayoutId, LayoutInterner, LayoutSize, RcHeader,
    ReferenceKind, ResolvedNamed, ScalarKind,
};
use crate::safewir::{
    AccessMode, ByteRange, CallableMemoryContract, CallableMemoryResult, ContractExtent,
    ContractProjection, ContractSizeExpr, DefinitionId, Extent,
    GenerationId, ObjectIdentity, ObjectRef, ParameterMemoryContract, ParameterMemoryEffect,
    ParameterMemoryEffects, ParameterMemoryEntry, Place, Projection, Provenance,
    ReallocationContract, ReallocationTemplateId, ResultCountedExtent, RuleId, SizeExpr,
};
use crate::safewir_validate::{
    validate_candidate, CallArgument, CandidateFunction, CandidateModule, HostArgument,
    Operation, ParameterBinding, PostValidationTransform, ValidationBudget,
    ValidationDiagnostic,
};

struct Resolver;

impl ClosedTypeResolver for Resolver {
    fn resolve_named<'a>(
        &'a self,
        name: &str,
        _arguments: &[Type],
    ) -> Option<ResolvedNamed<'a>> {
        Some(match name {
            "Int" => ResolvedNamed::Scalar(ScalarKind::Int),
            "List" => ResolvedNamed::PackedList { rc: RcHeader::Required },
            "String" => ResolvedNamed::Reference(ReferenceKind::Owning),
            _ => return None,
        })
    }
}

fn scalar_layouts() -> (LayoutBundle, LayoutId) {
    let mut interner = LayoutInterner::new();
    let int = interner
        .intern_type(&Type::Named("Int".into(), Vec::new()), &Resolver)
        .unwrap();
    (LayoutBundle::from_interner(&interner, [int]).unwrap(), int)
}

fn list_layouts() -> (LayoutBundle, LayoutId, LayoutId, u32, u32) {
    let mut interner = LayoutInterner::new();
    let int = interner
        .intern_type(&Type::Named("Int".into(), Vec::new()), &Resolver)
        .unwrap();
    let list = interner
        .intern_type(
            &Type::Named(
                "List".into(),
                vec![Type::Named("Int".into(), Vec::new())],
            ),
            &Resolver,
        )
        .unwrap();
    let LayoutSize::Dynamic { base, stride } = interner.get(list).unwrap().size() else {
        panic!("list layout must be dynamic")
    };
    (
        LayoutBundle::from_interner(&interner, [list]).unwrap(),
        list,
        int,
        base,
        stride,
    )
}

fn identity(root: u32, generation: u32) -> ObjectIdentity {
    ObjectIdentity {
        root: DefinitionId::new(root),
        generation: GenerationId::new(generation),
    }
}

fn object(
    root: u32,
    generation: u32,
    layout: LayoutId,
    extent: u64,
    provenance: Provenance,
) -> ObjectRef {
    ObjectRef {
        identity: identity(root, generation),
        layout,
        extent: Extent::Exact(SizeExpr::Constant(extent)),
        provenance,
    }
}

fn place(object: ObjectRef, access: AccessMode) -> Place {
    let length = match &object.extent {
        Extent::Exact(SizeExpr::Constant(length)) => *length,
        _ => panic!("test place requires a constant extent"),
    };
    Place {
        range: ByteRange {
            object: object.identity,
            start: SizeExpr::Constant(0),
            length: SizeExpr::Constant(length),
        },
        object,
        projection: Projection::root(),
        access,
    }
}

fn exact_extent(extent: u64) -> ContractExtent {
    ContractExtent::Exact(ContractSizeExpr::Constant(extent))
}

fn parameter(
    entry: ParameterMemoryEntry,
    layout: LayoutId,
    extent: u64,
    effects: ParameterMemoryEffects,
) -> ParameterMemoryContract {
    ParameterMemoryContract {
        entry,
        layout: Some(layout),
        extent: Some(exact_extent(extent)),
        dynamic_header: None,
        effects,
    }
}

fn no_memory_contract() -> CallableMemoryContract {
    CallableMemoryContract { parameters: Vec::new(), result: CallableMemoryResult::NoMemory }
}

fn module(layouts: LayoutBundle) -> CandidateModule {
    CandidateModule {
        gc: Default::default(),
        checked_source_entries: 1,
        layouts,
        imports: BTreeMap::new(),
        host_imports: BTreeMap::new(),
        reallocation_templates: Default::default(),
        functions: vec![CandidateFunction {
            name: "main".into(),
            contract: no_memory_contract(),
            scalar_results: Vec::new(),
            parameters: Vec::new(),
            operations: Vec::new(),
        }],
        contains_raw_memory: false,
        contains_raw_body: false,
        post_validation_transform: PostValidationTransform::FixedTemplateExpansionAndErasure,
    }
}

fn reallocation_function(
    list: LayoutId,
    base: u32,
    stride: u32,
    root_result: usize,
    capacity_result: usize,
) -> CandidateFunction {
    let root = ObjectRef {
        identity: identity(1, 1),
        layout: list,
        extent: Extent::Counted {
            header: SizeExpr::Constant(u64::from(base)),
            count: DefinitionId::new(2),
            stride: SizeExpr::Constant(u64::from(stride)),
        },
        provenance: Provenance::Unknown,
    };
    CandidateFunction {
        name: "reallocate".into(),
        contract: CallableMemoryContract {
            parameters: vec![
                ParameterMemoryContract {
                    entry: ParameterMemoryEntry::BorrowedRead,
                    layout: Some(list),
                    extent: Some(ContractExtent::Counted {
                        header: ContractSizeExpr::Constant(u64::from(base)),
                        count: ContractSizeExpr::Parameter(1),
                        stride: ContractSizeExpr::Constant(u64::from(stride)),
                    }),
                    dynamic_header: Some(
                        crate::safewir::DynamicHeaderInvariant::BoundedByCountedExtent {
                            component: 0,
                        },
                    ),
                    effects: ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
                },
                ParameterMemoryContract {
                    entry: ParameterMemoryEntry::NoMemory,
                    layout: None,
                    extent: None,
                    dynamic_header: None,
                    effects: ParameterMemoryEffects::none(),
                },
            ],
            result: CallableMemoryResult::Reallocated(ReallocationContract {
                source_parameter: 0,
                capacity_parameter: 1,
                root_result,
                capacity_result,
                layout: list,
                extent: ResultCountedExtent {
                    header_bytes: u64::from(base),
                    capacity_result,
                    stride_bytes: u64::from(stride),
                },
                dynamic_header_components: vec![0],
                template: ReallocationTemplateId::new(7),
            }),
        },
        scalar_results: vec![crate::wir::Kind::I32, crate::wir::Kind::I32],
        parameters: vec![
            ParameterBinding { definition: DefinitionId::new(1), object: Some(root) },
            ParameterBinding { definition: DefinitionId::new(2), object: None },
        ],
        operations: Vec::new(),
    }
}

#[test]
fn reallocation_contract_rejects_noncanonical_extent_and_header_components() {
    let (layouts, list, _, base, stride) = list_layouts();
    let mut bad_extent = module(layouts.clone());
    let mut function = reallocation_function(list, base, stride, 0, 1);
    let CallableMemoryResult::Reallocated(contract) = &mut function.contract.result else {
        panic!("fixture must carry a reallocation contract");
    };
    contract.extent.header_bytes += 1;
    bad_extent.functions = vec![function];
    let error = validate_candidate(&bad_extent, ValidationBudget::default()).unwrap_err();
    assert_eq!(error.diagnostic, ValidationDiagnostic::Rule(RuleId::CheckedCallableContract));
    assert!(error.to_string().contains("extent contradicts its canonical layout"), "{error}");

    let mut bad_header = module(layouts);
    let mut function = reallocation_function(list, base, stride, 0, 1);
    let CallableMemoryResult::Reallocated(contract) = &mut function.contract.result else {
        panic!("fixture must carry a reallocation contract");
    };
    contract.dynamic_header_components = vec![2];
    bad_header.functions = vec![function];
    let error = validate_candidate(&bad_header, ValidationBudget::default()).unwrap_err();
    assert_eq!(error.diagnostic, ValidationDiagnostic::Rule(RuleId::CheckedCallableContract));
    assert!(error.to_string().contains("noncanonical dynamic header component"), "{error}");
}

fn alias_function(name: &str, layout: LayoutId, extent: u64) -> CandidateFunction {
    let input = object(1, 1, layout, extent, Provenance::Unknown);
    CandidateFunction {
        name: name.into(),
        contract: CallableMemoryContract {
            parameters: vec![parameter(
                ParameterMemoryEntry::BorrowedRead,
                layout,
                extent,
                ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
            )],
            result: CallableMemoryResult::Aliases {
                parameter: 0,
                projection: ContractProjection::default(),
            },
        },
        parameters: vec![ParameterBinding {
            definition: input.identity.root,
            object: Some(input.clone()),
        }],
        scalar_results: Vec::new(),
        operations: vec![Operation::ReturnObject { object: input.identity }],
    }
}

fn alias_result(source: &ObjectRef, root: u32) -> ObjectRef {
    ObjectRef {
        identity: identity(root, source.identity.generation.get()),
        layout: source.layout,
        extent: source.extent.clone(),
        provenance: Provenance::Aliases {
            object: source.identity,
            projection: Projection::root(),
        },
    }
}

fn assert_rule(module: CandidateModule, rule: RuleId) {
    let error = validate_candidate(&module, ValidationBudget::default()).unwrap_err();
    assert_eq!(error.diagnostic, ValidationDiagnostic::Rule(rule), "{error}");
}

#[test]
fn stale_fresh_summaries_and_missing_result_authority_never_mint_storage() {
    let (layouts, layout) = scalar_layouts();
    let mut stale = module(layouts.clone());
    let mut dishonest = alias_function("dishonest", layout, 8);
    dishonest.contract.result = CallableMemoryResult::Fresh {
        layout,
        extent: exact_extent(8),
    };
    stale.functions.push(dishonest);
    assert_rule(stale, RuleId::CheckedCallableContract);

    let mut unknown = module(layouts);
    unknown.functions.push(CandidateFunction {
        name: "unknown".into(),
        contract: CallableMemoryContract::unknown(0),
        scalar_results: Vec::new(),
        parameters: Vec::new(),
        operations: Vec::new(),
    });
    unknown.functions[0].operations.push(Operation::Call {
        callable: "unknown".into(),
        arguments: Vec::new(),
        result: Some(object(10, 1, layout, 8, Provenance::Fresh)),
    });
    assert_rule(unknown, RuleId::UnknownResultIsNotFresh);
}

#[test]
fn imported_and_host_calls_require_catalog_authentication() {
    let (layouts, layout) = scalar_layouts();
    let identity_contract = CallableMemoryContract {
        parameters: vec![parameter(
            ParameterMemoryEntry::BorrowedRead,
            layout,
            8,
            ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
        )],
        result: CallableMemoryResult::Aliases {
            parameter: 0,
            projection: ContractProjection::default(),
        },
    };
    let read_contract = CallableMemoryContract {
        parameters: vec![parameter(
            ParameterMemoryEntry::BorrowedRead,
            layout,
            8,
            ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
        )],
        result: CallableMemoryResult::NoMemory,
    };
    let mut accepted = module(layouts.clone());
    accepted.imports.insert("safewir.primitive.identity_i64".into(), identity_contract);
    accepted.host_imports.insert("safewir.host.read_i64".into(), read_contract);
    let source = object(10, 1, layout, 8, Provenance::Fresh);
    let alias = alias_result(&source, 11);
    accepted.functions[0].operations.extend([
        Operation::Alloc { result: source.clone() },
        Operation::Call {
            callable: "safewir.primitive.identity_i64".into(),
            arguments: vec![CallArgument::Object(source.identity)],
            result: Some(alias),
        },
        Operation::HostCall {
            import: "safewir.host.read_i64".into(),
            arguments: vec![HostArgument::Buffer(place(source, AccessMode::Read))],
            result: None,
        },
    ]);
    validate_candidate(&accepted, ValidationBudget::default()).unwrap();

    let mut forged = module(layouts);
    forged.imports.insert("candidate.attested".into(), CallableMemoryContract {
        parameters: Vec::new(),
        result: CallableMemoryResult::Fresh { layout, extent: exact_extent(8) },
    });
    assert_rule(forged, RuleId::CheckedCallableContract);
}

#[test]
fn consume_retain_escape_and_sec078_alias_laundering_matrix_rejects() {
    let (layouts, layout) = scalar_layouts();
    let mut sec078 = module(layouts.clone());
    sec078.functions.push(alias_function("identity", layout, 8));
    let source = object(10, 1, layout, 8, Provenance::Fresh);
    let alias = alias_result(&source, 11);
    sec078.functions[0].operations.extend([
        Operation::Alloc { result: source.clone() },
        Operation::Call {
            callable: "identity".into(),
            arguments: vec![CallArgument::Object(source.identity)],
            result: Some(alias),
        },
        Operation::Release { object: source.identity },
    ]);
    assert_rule(sec078, RuleId::DestructiveProvenance);

    let mut stale_effect = module(layouts);
    let input = object(1, 1, layout, 8, Provenance::Unknown);
    stale_effect.functions.push(CandidateFunction {
        name: "retain_without_summary".into(),
        contract: CallableMemoryContract {
            parameters: vec![parameter(
                ParameterMemoryEntry::Shared,
                layout,
                8,
                ParameterMemoryEffects::none(),
            )],
            result: CallableMemoryResult::NoMemory,
        },
        parameters: vec![ParameterBinding {
            definition: input.identity.root,
            object: Some(input.clone()),
        }],
        scalar_results: Vec::new(),
        operations: vec![Operation::Retain { object: input.identity }],
    });
    assert_rule(stale_effect, RuleId::CheckedCallableContract);
}

#[test]
fn projected_borrow_cannot_be_promoted_to_retained_authority() {
    let (layouts, layout) = scalar_layouts();
    let input = object(1, 1, layout, 8, Provenance::Unknown);
    let alias = alias_result(&input, 2);
    let mut rejected = module(layouts);
    rejected.functions.push(CandidateFunction {
        name: "retain_borrowed_projection".into(),
        contract: CallableMemoryContract {
            parameters: vec![parameter(
                ParameterMemoryEntry::BorrowedRead,
                layout,
                8,
                ParameterMemoryEffects::only(ParameterMemoryEffect::Retain),
            )],
            result: CallableMemoryResult::NoMemory,
        },
        parameters: vec![ParameterBinding {
            definition: input.identity.root,
            object: Some(input.clone()),
        }],
        scalar_results: Vec::new(),
        operations: vec![
            Operation::ProjectObject {
                source: input.identity,
                projection: ContractProjection::default(),
                result: alias.clone(),
            },
            Operation::Retain { object: alias.identity },
        ],
    });
    assert_rule(rejected, RuleId::ConsumeOnce);
}

#[test]
fn call_rule_mutation_matrix_kills_each_authority_weakening() {
    let (layouts, layout) = scalar_layouts();
    let mut mutations = Vec::new();

    let mut forged_fresh = module(layouts.clone());
    let mut alias = alias_function("forged_fresh", layout, 8);
    alias.contract.result = CallableMemoryResult::Fresh { layout, extent: exact_extent(8) };
    forged_fresh.functions.push(alias);
    mutations.push(("trust-attached-fresh", forged_fresh, RuleId::CheckedCallableContract));

    let mut forged_import = module(layouts.clone());
    forged_import.imports.insert("forged.import".into(), CallableMemoryContract::unknown(0));
    mutations.push(("skip-import-authentication", forged_import, RuleId::CheckedCallableContract));

    let mut empty_shared = module(layouts.clone());
    empty_shared.functions.push(CandidateFunction {
        name: "empty_shared".into(),
        contract: CallableMemoryContract {
            parameters: Vec::new(),
            result: CallableMemoryResult::Shared(BTreeSet::new()),
        },
        parameters: Vec::new(),
        scalar_results: Vec::new(),
        operations: Vec::new(),
    });
    mutations.push(("accept-empty-shared", empty_shared, RuleId::CheckedCallableContract));

    let mut unknown_fresh = module(layouts);
    unknown_fresh.functions.push(CandidateFunction {
        name: "unknown".into(),
        contract: CallableMemoryContract::unknown(0),
        scalar_results: Vec::new(),
        parameters: Vec::new(),
        operations: Vec::new(),
    });
    unknown_fresh.functions[0].operations.push(Operation::Call {
        callable: "unknown".into(),
        arguments: Vec::new(),
        result: Some(object(10, 1, layout, 8, Provenance::Fresh)),
    });
    mutations.push(("unknown-to-fresh", unknown_fresh, RuleId::UnknownResultIsNotFresh));

    let (host_layouts, host_layout) = scalar_layouts();
    let mut wrong_host_access = module(host_layouts);
    wrong_host_access.host_imports.insert(
        "safewir.host.read_i64".into(),
        CallableMemoryContract {
            parameters: vec![parameter(
                ParameterMemoryEntry::BorrowedRead,
                host_layout,
                8,
                ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
            )],
            result: CallableMemoryResult::NoMemory,
        },
    );
    let host_source = object(20, 1, host_layout, 8, Provenance::Fresh);
    wrong_host_access.functions[0].operations.extend([
        Operation::Alloc { result: host_source.clone() },
        Operation::HostCall {
            import: "safewir.host.read_i64".into(),
            arguments: vec![HostArgument::Buffer(place(
                host_source,
                AccessMode::Write,
            ))],
            result: None,
        },
    ]);
    mutations.push(("weaken-host-buffer-mode", wrong_host_access, RuleId::HostBufferAbi));

    for (mutation, module, expected) in mutations {
        let error = validate_candidate(&module, ValidationBudget::default()).unwrap_err();
        assert_eq!(
            error.diagnostic,
            ValidationDiagnostic::Rule(expected),
            "CALL mutation `{mutation}` survived: {error}",
        );
    }
}
