use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::Type;

use crate::layout::{
    ClosedTypeResolver, LayoutBundle, LayoutId, LayoutInterner, LayoutSize, RcHeader,
    ReferenceKind, ResolvedNamed, ScalarKind,
};
use crate::safewir::{
    AccessMode, ByteRange, CallableMemoryContract, CallableMemoryResult, ContractExtent,
    ContractProjection, ContractProjectionStep, ContractSizeExpr, DefinitionId, Extent,
    GenerationId, ObjectIdentity, ObjectRef, ParameterMemoryContract, ParameterMemoryEffect,
    ParameterMemoryEffects, ParameterMemoryEntry, Place, Projection, ProjectionStep, Provenance,
    RuleId, SizeExpr,
};
use crate::safewir_validate::{
    validate_candidate, CallArgument, CandidateFunction, CandidateModule, HostArgument,
    MemoryWidth, Operation, ParameterBinding, PostValidationTransform, ValidationBudget,
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

fn tuple_layouts() -> (LayoutBundle, LayoutId, LayoutId, u64) {
    let mut interner = LayoutInterner::new();
    let int_type = Type::Named("Int".into(), Vec::new());
    let int = interner.intern_type(&int_type, &Resolver).unwrap();
    let tuple = interner
        .intern_type(&Type::Tuple(vec![int_type.clone(), int_type]), &Resolver)
        .unwrap();
    let LayoutSize::Fixed(size) = interner.get(tuple).unwrap().size() else {
        panic!("tuple layout must be static")
    };
    (
        LayoutBundle::from_interner(&interner, [tuple]).unwrap(),
        tuple,
        int,
        u64::from(size),
    )
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

fn nested_tuple_layouts() -> (LayoutBundle, LayoutId, LayoutId, u64) {
    let mut interner = LayoutInterner::new();
    let int_type = Type::Named("Int".into(), Vec::new());
    let int = interner.intern_type(&int_type, &Resolver).unwrap();
    let inner = Type::Tuple(vec![int_type.clone(), int_type.clone()]);
    let outer = interner
        .intern_type(&Type::Tuple(vec![inner, int_type]), &Resolver)
        .unwrap();
    let LayoutSize::Fixed(size) = interner.get(outer).unwrap().size() else {
        panic!("nested tuple layout must be static")
    };
    (
        LayoutBundle::from_interner(&interner, [outer]).unwrap(),
        outer,
        int,
        u64::from(size),
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
        effects,
    }
}

fn no_memory_contract() -> CallableMemoryContract {
    CallableMemoryContract { parameters: Vec::new(), result: CallableMemoryResult::NoMemory }
}

fn module(layouts: LayoutBundle) -> CandidateModule {
    CandidateModule {
        checked_source_entries: 1,
        layouts,
        imports: BTreeMap::new(),
        host_imports: BTreeMap::new(),
        functions: vec![CandidateFunction {
            name: "main".into(),
            contract: no_memory_contract(),
            parameters: Vec::new(),
            operations: Vec::new(),
        }],
        contains_raw_memory: false,
        contains_raw_body: false,
        post_validation_transform: PostValidationTransform::FixedTemplateExpansionAndErasure,
    }
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
        operations: vec![Operation::ReturnObject { object: input.identity }],
    }
}

fn fresh_function(
    name: &str,
    layout: LayoutId,
    extent: u64,
    with_parameter: bool,
) -> CandidateFunction {
    let result = object(2, 1, layout, extent, Provenance::Fresh);
    let input = object(1, 1, layout, extent, Provenance::Unknown);
    CandidateFunction {
        name: name.into(),
        contract: CallableMemoryContract {
            parameters: if with_parameter {
                vec![parameter(
                    ParameterMemoryEntry::BorrowedRead,
                    layout,
                    extent,
                    ParameterMemoryEffects::none(),
                )]
            } else {
                Vec::new()
            },
            result: CallableMemoryResult::Fresh {
                layout,
                extent: exact_extent(extent),
            },
        },
        parameters: if with_parameter {
            vec![ParameterBinding {
                definition: input.identity.root,
                object: Some(input),
            }]
        } else {
            Vec::new()
        },
        operations: vec![
            Operation::Alloc { result: result.clone() },
            Operation::ReturnObject { object: result.identity },
        ],
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
fn direct_calls_use_the_final_body_and_closures_use_the_closed_world_join() {
    let (layouts, layout) = scalar_layouts();
    let mut direct = module(layouts.clone());
    direct.functions.push(fresh_function("allocate", layout, 8, false));
    let result = object(10, 1, layout, 8, Provenance::Fresh);
    direct.functions[0].operations.push(Operation::Call {
        callable: "allocate".into(),
        arguments: Vec::new(),
        result: Some(result.clone()),
    });
    direct.functions[0].operations.push(Operation::Release { object: result.identity });
    validate_candidate(&direct, ValidationBudget::default()).unwrap();

    let mut closure = module(layouts);
    closure.functions.push(alias_function("capture", layout, 8));
    let source = object(10, 1, layout, 8, Provenance::Fresh);
    let result = alias_result(&source, 11);
    closure.functions[0].operations.extend([
        Operation::Alloc { result: source.clone() },
        Operation::ClosureCall {
            callable: "capture".into(),
            captures: vec![CallArgument::Object(source.identity)],
            arguments: Vec::new(),
            result: Some(result.clone()),
        },
        Operation::Load {
            place: place(result, AccessMode::Read),
            width: MemoryWidth::I64,
        },
    ]);
    validate_candidate(&closure, ValidationBudget::default()).unwrap();
}

#[test]
fn closure_calls_require_the_named_body_to_match_combined_arity() {
    let (layouts, layout) = scalar_layouts();
    let mut rejected = module(layouts);
    rejected.functions.push(alias_function("capture", layout, 8));
    rejected.functions.push(fresh_function("zero_arity_fresh", layout, 8, false));
    rejected.functions[0].operations.push(Operation::ClosureCall {
        callable: "capture".into(),
        captures: Vec::new(),
        arguments: Vec::new(),
        result: Some(object(10, 1, layout, 8, Provenance::Fresh)),
    });
    assert_rule(rejected, RuleId::CheckedCallableContract);
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
fn fresh_dynamic_extents_resolve_checked_local_constant_definitions() {
    let (layouts, list, _, base, stride) = list_layouts();
    let count = DefinitionId::new(1);
    let allocated = ObjectRef {
        identity: identity(2, 1),
        layout: list,
        extent: Extent::Counted {
            header: SizeExpr::Constant(u64::from(base)),
            count,
            stride: SizeExpr::Constant(u64::from(stride)),
        },
        provenance: Provenance::Fresh,
    };
    let contract_extent = ContractExtent::Counted {
        header: ContractSizeExpr::Constant(u64::from(base)),
        count: ContractSizeExpr::Constant(2),
        stride: ContractSizeExpr::Constant(u64::from(stride)),
    };
    let allocate = CandidateFunction {
        name: "allocate_list_two".into(),
        contract: CallableMemoryContract {
            parameters: Vec::new(),
            result: CallableMemoryResult::Fresh {
                layout: list,
                extent: contract_extent.clone(),
            },
        },
        parameters: Vec::new(),
        operations: vec![
            Operation::DefineScalar { definition: count, value: 2 },
            Operation::Alloc { result: allocated.clone() },
            Operation::ReturnObject { object: allocated.identity },
        ],
    };
    let mut accepted = module(layouts);
    accepted.functions.push(allocate);
    let caller_count = DefinitionId::new(10);
    let caller_result = ObjectRef {
        identity: identity(11, 1),
        layout: list,
        extent: Extent::Counted {
            header: SizeExpr::Constant(u64::from(base)),
            count: caller_count,
            stride: SizeExpr::Constant(u64::from(stride)),
        },
        provenance: Provenance::Fresh,
    };
    accepted.functions[0].operations.extend([
        Operation::DefineScalar { definition: caller_count, value: 2 },
        Operation::Call {
            callable: "allocate_list_two".into(),
            arguments: Vec::new(),
            result: Some(caller_result.clone()),
        },
        Operation::Release { object: caller_result.identity },
    ]);
    validate_candidate(&accepted, ValidationBudget::default()).unwrap();
}

#[test]
fn indirect_calls_join_every_closed_world_compatible_target() {
    let (layouts, layout) = scalar_layouts();
    let mut accepted = module(layouts.clone());
    accepted.functions.push(alias_function("left", layout, 8));
    accepted.functions.push(alias_function("right", layout, 8));
    let source = object(10, 1, layout, 8, Provenance::Fresh);
    let result = alias_result(&source, 11);
    accepted.functions[0].operations.extend([
        Operation::Alloc { result: source.clone() },
        Operation::IndirectCall {
            arguments: vec![CallArgument::Object(source.identity)],
            result: Some(result),
        },
    ]);
    validate_candidate(&accepted, ValidationBudget::default()).unwrap();

    let mut mixed = module(layouts);
    mixed.functions.push(alias_function("alias", layout, 8));
    mixed.functions.push(fresh_function("fresh", layout, 8, true));
    let source = object(10, 1, layout, 8, Provenance::Fresh);
    mixed.functions[0].operations.extend([
        Operation::Alloc { result: source.clone() },
        Operation::IndirectCall {
            arguments: vec![CallArgument::Object(source.identity)],
            result: Some(object(11, 1, layout, 8, Provenance::Fresh)),
        },
    ]);
    assert_rule(mixed, RuleId::UnknownResultIsNotFresh);
}

#[test]
fn recursive_components_reach_a_conservative_deterministic_fixpoint() {
    let (layouts, layout) = scalar_layouts();
    let mut recursive = module(layouts);
    recursive.functions.push(CandidateFunction {
        name: "recursive_no_memory".into(),
        contract: no_memory_contract(),
        parameters: Vec::new(),
        operations: vec![Operation::Call {
            callable: "recursive_no_memory".into(),
            arguments: Vec::new(),
            result: None,
        }],
    });
    let first = validate_candidate(&recursive, ValidationBudget::default()).unwrap().metrics();
    let second = validate_candidate(&recursive, ValidationBudget::default()).unwrap().metrics();
    assert_eq!(first, second);

    let mut forged = recursive;
    forged.functions.push(CandidateFunction {
        name: "recursive_fresh".into(),
        contract: CallableMemoryContract {
            parameters: Vec::new(),
            result: CallableMemoryResult::Fresh { layout, extent: exact_extent(8) },
        },
        parameters: Vec::new(),
        operations: vec![
            Operation::Call {
                callable: "recursive_fresh".into(),
                arguments: Vec::new(),
                result: Some(object(1, 1, layout, 8, Provenance::Fresh)),
            },
            Operation::ReturnObject { object: identity(1, 1) },
        ],
    });
    assert_rule(forged, RuleId::CheckedCallableContract);
}

#[test]
fn projection_results_use_descriptor_identity_not_candidate_shape() {
    let (layouts, tuple, int, tuple_extent) = tuple_layouts();
    let source = object(1, 1, tuple, tuple_extent, Provenance::Unknown);
    let projection = ContractProjection::from_steps([ContractProjectionStep::Field { index: 1 }]);
    let actual_projection = Projection::root().project(ProjectionStep::Field { index: 1 });
    let result = ObjectRef {
        identity: identity(2, 1),
        layout: int,
        extent: Extent::Exact(SizeExpr::Constant(8)),
        provenance: Provenance::Aliases {
            object: source.identity,
            projection: actual_projection,
        },
    };
    let project = CandidateFunction {
        name: "project".into(),
        contract: CallableMemoryContract {
            parameters: vec![parameter(
                ParameterMemoryEntry::BorrowedRead,
                tuple,
                tuple_extent,
                ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
            )],
            result: CallableMemoryResult::Aliases {
                parameter: 0,
                projection: projection.clone(),
            },
        },
        parameters: vec![ParameterBinding {
            definition: source.identity.root,
            object: Some(source.clone()),
        }],
        operations: vec![
            Operation::ProjectObject {
                source: source.identity,
                projection,
                result: result.clone(),
            },
            Operation::ReturnObject { object: result.identity },
        ],
    };
    let mut accepted = module(layouts.clone());
    accepted.functions.push(project.clone());
    validate_candidate(&accepted, ValidationBudget::default()).unwrap();

    let mut forged = module(layouts);
    let mut project = project;
    let Operation::ProjectObject { result, .. } = &mut project.operations[0] else {
        unreachable!()
    };
    result.layout = tuple;
    forged.functions.push(project);
    assert_rule(forged, RuleId::CheckedCallableContract);
}

#[test]
fn list_alias_projections_are_value_stable_and_bounded_by_the_source_count() {
    let (layouts, list, int, base, stride) = list_layouts();
    let count = DefinitionId::new(1);
    let canonical_index = DefinitionId::new(2);
    let equivalent_index = DefinitionId::new(3);
    let source = ObjectRef {
        identity: identity(10, 1),
        layout: list,
        extent: Extent::Counted {
            header: SizeExpr::Constant(u64::from(base)),
            count,
            stride: SizeExpr::Constant(u64::from(stride)),
        },
        provenance: Provenance::Fresh,
    };
    let result = ObjectRef {
        identity: identity(11, 1),
        layout: int,
        extent: Extent::Exact(SizeExpr::Constant(u64::from(stride))),
        provenance: Provenance::Aliases {
            object: source.identity,
            projection: Projection::root().project(ProjectionStep::Element {
                index: equivalent_index,
            }),
        },
    };
    let mut accepted = module(layouts.clone());
    accepted.functions[0].operations = vec![
        Operation::DefineScalar { definition: count, value: 2 },
        Operation::DefineScalar { definition: canonical_index, value: 1 },
        Operation::DefineScalar { definition: equivalent_index, value: 1 },
        Operation::Alloc { result: source.clone() },
        Operation::ProjectObject {
            source: source.identity,
            projection: ContractProjection::from_steps([
                ContractProjectionStep::Element { index: 1 },
            ]),
            result: result.clone(),
        },
        Operation::Load {
            place: place(result.clone(), AccessMode::Read),
            width: MemoryWidth::I64,
        },
    ];
    validate_candidate(&accepted, ValidationBudget::default()).unwrap();

    let outside = DefinitionId::new(4);
    let mut element_oob = accepted.clone();
    element_oob.functions[0].operations.pop();
    element_oob.functions[0].operations.insert(
        3,
        Operation::DefineScalar { definition: outside, value: 3 },
    );
    let Operation::ProjectObject { projection, result, .. } =
        &mut element_oob.functions[0].operations[5]
    else {
        unreachable!()
    };
    *projection = ContractProjection::from_steps([
        ContractProjectionStep::Element { index: 3 },
    ]);
    result.provenance = Provenance::Aliases {
        object: source.identity,
        projection: Projection::root().project(ProjectionStep::Element { index: outside }),
    };
    assert_rule(element_oob, RuleId::ExactRangeSource);

    let mut slice_oob = accepted.clone();
    slice_oob.functions[0].operations.pop();
    let Operation::ProjectObject { projection, result, .. } =
        &mut slice_oob.functions[0].operations[4]
    else {
        unreachable!()
    };
    *projection = ContractProjection::from_steps([ContractProjectionStep::Slice {
        start: 1,
        length: 2,
    }]);
    result.extent = Extent::Exact(SizeExpr::Constant(u64::from(stride) * 2));
    result.provenance = Provenance::Aliases {
        object: source.identity,
        projection: Projection::root().project(ProjectionStep::Slice {
            start: canonical_index,
            length: count,
        }),
    };
    assert_rule(slice_oob, RuleId::ExactRangeSource);

    let mut chained_header = accepted;
    chained_header.functions[0].operations.pop();
    let Operation::ProjectObject { projection, result, .. } =
        &mut chained_header.functions[0].operations[4]
    else {
        unreachable!()
    };
    *projection = ContractProjection::from_steps([
        ContractProjectionStep::Header { component: 0 },
        ContractProjectionStep::Field { index: 0 },
    ]);
    result.provenance = Provenance::Aliases {
        object: source.identity,
        projection: Projection::from_steps([
            ProjectionStep::Header { component: 0 },
            ProjectionStep::Field { index: 0 },
        ]),
    };
    assert_rule(chained_header, RuleId::CheckedCallableContract);
}

#[test]
fn chained_projection_and_reuse_effect_ancestry_are_transitive() {
    let (layouts, outer, int, outer_extent) = nested_tuple_layouts();
    let source = object(1, 1, outer, outer_extent, Provenance::Unknown);
    let first_projection = ContractProjection::from_steps([
        ContractProjectionStep::Field { index: 0 },
    ]);
    let second_projection = ContractProjection::from_steps([
        ContractProjectionStep::Field { index: 1 },
    ]);
    let first = ObjectRef {
        identity: identity(2, 1),
        layout: {
            let (_, tuple, _, _) = tuple_layouts();
            tuple
        },
        extent: Extent::Exact(SizeExpr::Constant(16)),
        provenance: Provenance::Aliases {
            object: source.identity,
            projection: Projection::root().project(ProjectionStep::Field { index: 0 }),
        },
    };
    let second = ObjectRef {
        identity: identity(3, 1),
        layout: int,
        extent: Extent::Exact(SizeExpr::Constant(8)),
        provenance: Provenance::Aliases {
            object: first.identity,
            projection: Projection::root().project(ProjectionStep::Field { index: 1 }),
        },
    };
    let mut projection = module(layouts.clone());
    projection.functions.push(CandidateFunction {
        name: "nested_project".into(),
        contract: CallableMemoryContract {
            parameters: vec![parameter(
                ParameterMemoryEntry::BorrowedRead,
                outer,
                outer_extent,
                ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
            )],
            result: CallableMemoryResult::Aliases {
                parameter: 0,
                projection: first_projection.projected(&second_projection),
            },
        },
        parameters: vec![ParameterBinding {
            definition: source.identity.root,
            object: Some(source.clone()),
        }],
        operations: vec![
            Operation::ProjectObject {
                source: source.identity,
                projection: first_projection,
                result: first.clone(),
            },
            Operation::ProjectObject {
                source: first.identity,
                projection: second_projection,
                result: second.clone(),
            },
            Operation::ReturnObject { object: second.identity },
        ],
    });
    validate_candidate(&projection, ValidationBudget::default()).unwrap();

    let (_, scalar) = scalar_layouts();
    let input = object(1, 1, scalar, 8, Provenance::Unknown);
    let reused = object(1, 2, scalar, 8, Provenance::Fresh);
    let mut reuse = module(scalar_layouts().0);
    reuse.functions.push(CandidateFunction {
        name: "reuse_and_escape".into(),
        contract: CallableMemoryContract {
            parameters: vec![parameter(
                ParameterMemoryEntry::Unique,
                scalar,
                8,
                ParameterMemoryEffects::only(ParameterMemoryEffect::Consume)
                    .with(ParameterMemoryEffect::Write)
                    .with(ParameterMemoryEffect::Escape),
            )],
            result: CallableMemoryResult::Unknown,
        },
        parameters: vec![ParameterBinding {
            definition: input.identity.root,
            object: Some(input.clone()),
        }],
        operations: vec![
            Operation::Reuse { old: input.identity, new: reused.clone() },
            Operation::ReturnObject { object: reused.identity },
        ],
    });
    validate_candidate(&reuse, ValidationBudget::default()).unwrap();
}

#[test]
fn shared_results_and_transitive_alias_effects_are_checked() {
    let (layouts, layout) = scalar_layouts();
    let left = object(1, 1, layout, 8, Provenance::Unknown);
    let right = object(2, 1, layout, 8, Provenance::Unknown);
    let sources = BTreeSet::from([left.identity, right.identity]);
    let merged = ObjectRef {
        identity: identity(3, 9),
        layout,
        extent: Extent::Exact(SizeExpr::Constant(8)),
        provenance: Provenance::MayAlias(sources),
    };
    let shared = CandidateFunction {
        name: "shared".into(),
        contract: CallableMemoryContract {
            parameters: vec![
                parameter(
                    ParameterMemoryEntry::Shared,
                    layout,
                    8,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                ),
                parameter(
                    ParameterMemoryEntry::Shared,
                    layout,
                    8,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Escape),
                ),
            ],
            result: CallableMemoryResult::Shared(BTreeSet::from([0, 1])),
        },
        parameters: vec![
            ParameterBinding { definition: left.identity.root, object: Some(left.clone()) },
            ParameterBinding { definition: right.identity.root, object: Some(right.clone()) },
        ],
        operations: vec![
            Operation::MergeObjectAliases {
                sources: vec![left.identity, right.identity],
                result: merged.clone(),
            },
            Operation::ReturnObject { object: merged.identity },
        ],
    };
    let mut accepted = module(layouts.clone());
    accepted.functions.push(shared.clone());
    validate_candidate(&accepted, ValidationBudget::default()).unwrap();

    let mut deduplicated = module(layouts.clone());
    deduplicated.functions.push(shared);
    let same_source = object(10, 1, layout, 8, Provenance::Fresh);
    let deduplicated_result = ObjectRef {
        identity: identity(11, 1),
        layout,
        extent: Extent::Exact(SizeExpr::Constant(8)),
        provenance: Provenance::MayAlias(BTreeSet::from([same_source.identity])),
    };
    deduplicated.functions[0].operations.extend([
        Operation::Alloc { result: same_source.clone() },
        Operation::Call {
            callable: "shared".into(),
            arguments: vec![
                CallArgument::Object(same_source.identity),
                CallArgument::Object(same_source.identity),
            ],
            result: Some(deduplicated_result),
        },
    ]);
    validate_candidate(&deduplicated, ValidationBudget::default()).unwrap();

    let mut duplicate = module(layouts);
    let first = object(1, 1, layout, 8, Provenance::Unknown);
    let second = object(2, 1, layout, 8, Provenance::Unknown);
    duplicate.functions.push(CandidateFunction {
        name: "write_two".into(),
        contract: CallableMemoryContract {
            parameters: vec![
                parameter(
                    ParameterMemoryEntry::BorrowedWrite,
                    layout,
                    8,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Write),
                ),
                parameter(
                    ParameterMemoryEntry::BorrowedWrite,
                    layout,
                    8,
                    ParameterMemoryEffects::only(ParameterMemoryEffect::Write),
                ),
            ],
            result: CallableMemoryResult::NoMemory,
        },
        parameters: vec![
            ParameterBinding { definition: first.identity.root, object: Some(first.clone()) },
            ParameterBinding { definition: second.identity.root, object: Some(second.clone()) },
        ],
        operations: vec![
            Operation::Store { place: place(first, AccessMode::Write), width: MemoryWidth::I64 },
            Operation::Store { place: place(second, AccessMode::Write), width: MemoryWidth::I64 },
        ],
    });
    let source = object(10, 1, layout, 8, Provenance::Fresh);
    duplicate.functions[0].operations.extend([
        Operation::Alloc { result: source.clone() },
        Operation::Call {
            callable: "write_two".into(),
            arguments: vec![
                CallArgument::Object(source.identity),
                CallArgument::Object(source.identity),
            ],
            result: None,
        },
    ]);
    assert_rule(duplicate, RuleId::CheckedCallableContract);
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
        operations: vec![Operation::Retain { object: input.identity }],
    });
    assert_rule(stale_effect, RuleId::CheckedCallableContract);
}

#[test]
fn transitive_retain_effects_are_recomputed_through_the_final_call_graph() {
    let (layouts, layout) = scalar_layouts();
    let leaf_input = object(1, 1, layout, 8, Provenance::Unknown);
    let leaf = CandidateFunction {
        name: "retain_leaf".into(),
        contract: CallableMemoryContract {
            parameters: vec![parameter(
                ParameterMemoryEntry::Shared,
                layout,
                8,
                ParameterMemoryEffects::only(ParameterMemoryEffect::Retain),
            )],
            result: CallableMemoryResult::NoMemory,
        },
        parameters: vec![ParameterBinding {
            definition: leaf_input.identity.root,
            object: Some(leaf_input.clone()),
        }],
        operations: vec![Operation::Retain { object: leaf_input.identity }],
    };
    let wrapper_input = object(1, 1, layout, 8, Provenance::Unknown);
    let wrapper = CandidateFunction {
        name: "retain_wrapper".into(),
        contract: CallableMemoryContract {
            parameters: vec![parameter(
                ParameterMemoryEntry::Shared,
                layout,
                8,
                ParameterMemoryEffects::only(ParameterMemoryEffect::Retain),
            )],
            result: CallableMemoryResult::NoMemory,
        },
        parameters: vec![ParameterBinding {
            definition: wrapper_input.identity.root,
            object: Some(wrapper_input.clone()),
        }],
        operations: vec![Operation::Call {
            callable: "retain_leaf".into(),
            arguments: vec![CallArgument::Object(wrapper_input.identity)],
            result: None,
        }],
    };
    let mut accepted = module(layouts.clone());
    accepted.functions.extend([leaf.clone(), wrapper.clone()]);
    validate_candidate(&accepted, ValidationBudget::default()).unwrap();

    let mut rejected = module(layouts);
    let mut stale = wrapper;
    stale.contract.parameters[0].effects = ParameterMemoryEffects::none();
    rejected.functions.extend([leaf, stale]);
    assert_rule(rejected, RuleId::CheckedCallableContract);
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
fn callable_summary_work_is_charged_to_the_operation_budget() {
    let (layouts, _) = scalar_layouts();
    let mut candidate = module(layouts);
    candidate.functions[0].operations.push(Operation::DefineScalar {
        definition: DefinitionId::new(1),
        value: 0,
    });
    let error = validate_candidate(
        &candidate,
        ValidationBudget {
            operations: 1,
            ..ValidationBudget::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        error.diagnostic,
        ValidationDiagnostic::Rule(RuleId::BudgetExhaustionFailsClosed),
        "call summary work escaped its budget: {error}",
    );
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
        operations: Vec::new(),
    });
    mutations.push(("accept-empty-shared", empty_shared, RuleId::CheckedCallableContract));

    let mut unknown_fresh = module(layouts);
    unknown_fresh.functions.push(CandidateFunction {
        name: "unknown".into(),
        contract: CallableMemoryContract::unknown(0),
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
