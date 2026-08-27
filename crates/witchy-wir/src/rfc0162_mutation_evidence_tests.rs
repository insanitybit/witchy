use std::collections::BTreeMap;

use witchy_syntax::ast::Type;

use crate::layout::{
    ClosedTypeResolver, LayoutBundle, LayoutInterner, RcHeader, ReferenceKind, ResolvedNamed,
    ScalarKind,
};
use crate::safewir::{
    CallableMemoryContract, CallableMemoryResult, ContractExtent, ContractSizeExpr, DefinitionId,
    Extent, GenerationId, ObjectIdentity, ObjectRef, ParameterMemoryContract,
    ParameterMemoryEffects, ParameterMemoryEntry, Provenance, RuleId, SizeExpr,
};
use crate::safewir_call::check_callable_contracts;
use crate::safewir_validate::{
    CandidateFunction, CandidateModule, Operation, ParameterBinding, PostValidationTransform,
    ValidationBudget, ValidationDiagnostic,
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
            "List" => ResolvedNamed::PackedList {
                rc: RcHeader::Required,
            },
            "String" => ResolvedNamed::Reference(ReferenceKind::Owning),
            _ => return None,
        })
    }
}

#[test]
fn callable_checker_recomputes_parameter_effects() {
    let mut interner = LayoutInterner::new();
    let layout = interner
        .intern_type(&Type::Named("Int".into(), Vec::new()), &Resolver)
        .unwrap();
    let layouts = LayoutBundle::from_interner(&interner, [layout]).unwrap();
    let identity = ObjectIdentity {
        root: DefinitionId::new(1),
        generation: GenerationId::new(1),
    };
    let parameter = ObjectRef {
        identity,
        layout,
        extent: Extent::Exact(SizeExpr::Constant(8)),
        provenance: Provenance::Unknown,
    };
    let module = CandidateModule {
        checked_source_entries: 1,
        layouts,
        imports: BTreeMap::new(),
        host_imports: BTreeMap::new(),
        functions: vec![CandidateFunction {
            name: "main".into(),
            contract: CallableMemoryContract {
                parameters: vec![ParameterMemoryContract {
                    entry: ParameterMemoryEntry::Unique,
                    layout: Some(layout),
                    extent: Some(ContractExtent::Exact(ContractSizeExpr::Constant(8))),
                    effects: ParameterMemoryEffects::none(),
                }],
                result: CallableMemoryResult::NoMemory,
            },
            parameters: vec![ParameterBinding {
                definition: identity.root,
                object: Some(parameter),
            }],
            operations: vec![Operation::Release { object: identity }],
        }],
        contains_raw_memory: false,
        contains_raw_body: false,
        post_validation_transform: PostValidationTransform::FixedTemplateExpansionAndErasure,
    };

    let Err(error) = check_callable_contracts(&module, &interner, ValidationBudget::default())
    else {
        panic!("stale declared parameter effects were accepted");
    };
    assert_eq!(
        error.diagnostic,
        ValidationDiagnostic::Rule(RuleId::CheckedCallableContract)
    );
}
