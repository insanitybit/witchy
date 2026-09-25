use std::collections::BTreeMap;

use witchy_syntax::ast::Type;

use crate::layout::{
    ClosedTypeResolver, LayoutBundle, LayoutId, LayoutInterner, RcHeader,
    ReferenceKind, ResolvedNamed, ScalarKind,
};
use crate::safewir::{
    AccessMode, ByteRange, CallableMemoryContract, CallableMemoryResult, ContractExtent,
    ContractProjection, ContractSizeExpr, DefinitionId, Extent,
    GenerationId, ObjectIdentity, ObjectRef, ParameterMemoryContract, ParameterMemoryEffect,
    ParameterMemoryEffects, ParameterMemoryEntry, Place, Projection, Provenance,
    RuleId, SizeExpr,
};
use crate::safewir_validate::{
    validate_candidate, CallArgument, CandidateFunction, CandidateModule, HostArgument,
    Operation, PostValidationTransform, ValidationBudget,
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

pub fn imported_and_host_calls_require_catalog_authentication() {
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



