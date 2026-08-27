use std::collections::BTreeMap;

use witchy_syntax::ast::Type;
use witchy_wir::layout::{
    ClosedTypeResolver, LayoutBundle, LayoutId, LayoutInterner, RcHeader, ReferenceKind,
    ResolvedNamed, ScalarKind,
};
use witchy_wir::safewir::{
    AccessMode, ByteRange, CallableMemoryContract, CallableMemoryResult, ContractExtent,
    ContractSizeExpr, DefinitionId, Extent, GenerationId, ObjectIdentity, ObjectRef,
    ParameterMemoryContract, ParameterMemoryEffect, ParameterMemoryEffects,
    ParameterMemoryEntry, Place, Projection, Provenance, RuleId, SizeExpr,
};
use witchy_wir::safewir_validate::{
    validate_candidate, CallArgument, CandidateFunction, CandidateModule, ControlId, CopyKind,
    HostArgument, MemoryWidth, Operation, ParameterBinding, PostValidationTransform, ValidationBudget,
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

fn layouts() -> (LayoutBundle, LayoutId) {
    let mut interner = LayoutInterner::new();
    let id = interner
        .intern_type(&Type::Named("Int".into(), Vec::new()), &Resolver)
        .unwrap();
    (LayoutBundle::from_interner(&interner, [id]).unwrap(), id)
}

fn identity(root: u32, generation: u32) -> ObjectIdentity {
    ObjectIdentity {
        root: DefinitionId::new(root),
        generation: GenerationId::new(generation),
    }
}

fn object(layout: LayoutId) -> ObjectRef {
    ObjectRef {
        identity: identity(3, 1),
        layout,
        extent: Extent::Exact(SizeExpr::Constant(8)),
        provenance: Provenance::Fresh,
    }
}

fn place(object: ObjectRef, access: AccessMode) -> Place {
    Place {
        range: ByteRange {
            object: object.identity,
            start: SizeExpr::Constant(0),
            length: SizeExpr::Constant(8),
        },
        object,
        projection: Projection::root(),
        access,
    }
}

fn base_module() -> CandidateModule {
    let (layouts, layout) = layouts();
    CandidateModule {
        checked_source_entries: 1,
        layouts,
        imports: BTreeMap::new(),
        host_imports: BTreeMap::new(),
        functions: vec![CandidateFunction {
            name: "main".into(),
            contract: CallableMemoryContract {
                parameters: Vec::new(),
                result: CallableMemoryResult::NoMemory,
            },
            parameters: Vec::new(),
            operations: vec![Operation::Alloc { result: object(layout) }],
        }],
        contains_raw_memory: false,
        contains_raw_body: false,
        post_validation_transform: PostValidationTransform::FixedTemplateExpansionAndErasure,
    }
}

fn assert_rule(module: CandidateModule, expected: RuleId) {
    let error = validate_candidate(&module, ValidationBudget::default()).unwrap_err();
    assert_eq!(
        error.diagnostic,
        ValidationDiagnostic::Rule(expected),
        "{error}"
    );
}

#[test]
fn scalar_to_reference_cast_is_rejected() {
    let mut module = base_module();
    module.functions[0].operations = vec![Operation::ReferenceCastFromScalar];
    assert_rule(module, RuleId::ReferenceSeparation);
}

#[test]
fn destructive_operation_requires_fresh_unique_generation() {
    let mut module = base_module();
    let allocated = object(layouts().1);
    module.functions[0].operations.extend([
        Operation::Retain {
            object: allocated.identity,
        },
        Operation::Release {
            object: allocated.identity,
        },
    ]);
    assert_rule(module, RuleId::DestructiveProvenance);
}

#[test]
fn loop_validation_reaches_the_stable_header() {
    let mut module = base_module();
    let allocated = object(layouts().1);
    module.functions[0].operations.extend([
        Operation::DefineScalar {
            definition: DefinitionId::new(1),
            value: 1,
        },
        Operation::Loop {
            control: ControlId::new(1),
            body: vec![
                Operation::Retain {
                    object: allocated.identity,
                },
                Operation::Branch {
                    target: ControlId::new(1),
                    condition: Some(DefinitionId::new(1)),
                },
            ],
        },
    ]);
    let report = validate_candidate(&module, ValidationBudget::default()).unwrap();
    assert_eq!(report.metrics().operations, 7);
}

#[test]
fn unknown_callable_result_never_mints_a_fresh_object() {
    let mut module = base_module();
    module
        .imports
        .insert("unknown".into(), CallableMemoryContract::unknown(1));
    let argument = object(layouts().1);
    let mut result = object(layouts().1);
    result.identity = identity(10, 1);
    module.functions[0].operations.push(Operation::Call {
        callable: "unknown".into(),
        arguments: vec![CallArgument::Object(argument.identity)],
        result: Some(result),
    });
    assert_rule(module, RuleId::UnknownResultIsNotFresh);
}

#[test]
fn bulk_copy_requires_exclusive_destination_authority() {
    let mut module = base_module();
    let allocated = object(layouts().1);
    module.functions[0].operations.extend([
        Operation::Retain {
            object: allocated.identity,
        },
        Operation::Copy {
            source: place(allocated.clone(), AccessMode::Read),
            destination: place(allocated, AccessMode::Write),
            kind: CopyKind::MayOverlap,
        },
    ]);
    assert_rule(module, RuleId::BulkRanges);
}

#[test]
fn place_range_generation_must_match_its_object() {
    let mut module = base_module();
    let allocated = object(layouts().1);
    let mut access = place(allocated, AccessMode::Read);
    access.range.object = identity(3, 2);
    module.functions[0].operations.push(Operation::Load {
        place: access,
        width: MemoryWidth::I64,
    });
    assert_rule(module, RuleId::LiveGeneration);
}

#[test]
fn host_read_contract_rejects_a_write_buffer() {
    let mut module = base_module();
    module.host_imports.insert(
        "read".into(),
        CallableMemoryContract {
            parameters: vec![ParameterMemoryContract {
                entry: ParameterMemoryEntry::BorrowedRead,
                layout: Some(layouts().1),
                extent: Some(ContractExtent::Exact(ContractSizeExpr::Constant(8))),
                effects: ParameterMemoryEffects::only(ParameterMemoryEffect::Read),
            }],
            result: CallableMemoryResult::NoMemory,
        },
    );
    let allocated = object(layouts().1);
    module.functions[0].operations.push(Operation::HostCall {
        import: "read".into(),
        arguments: vec![HostArgument::Buffer(place(allocated, AccessMode::Write))],
        result: None,
    });
    assert_rule(module, RuleId::HostBufferAbi);
}

#[test]
fn initial_parameter_facts_respect_the_fact_budget() {
    let mut module = base_module();
    module.functions = vec![CandidateFunction {
        name: "main".into(),
        contract: CallableMemoryContract {
            parameters: vec![ParameterMemoryContract {
                entry: ParameterMemoryEntry::NoMemory,
                layout: None,
                extent: None,
                effects: ParameterMemoryEffects::none(),
            }],
            result: CallableMemoryResult::NoMemory,
        },
        parameters: vec![ParameterBinding {
            definition: DefinitionId::new(1),
            object: None,
        }],
        operations: Vec::new(),
    }];
    let error = validate_candidate(
        &module,
        ValidationBudget {
            facts: 0,
            ..ValidationBudget::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        error.diagnostic,
        ValidationDiagnostic::Rule(RuleId::BudgetExhaustionFailsClosed)
    );
}
