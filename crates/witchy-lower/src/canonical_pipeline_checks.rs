//! Admission mutation witnesses driven by the integration test binary.

use witchy_wir::canonical_safewir::{Operation, Terminator, ValueId, check_initial_with_witness, validate_final};

/// The lowered candidate of checked source, before any guard pass or check,
/// for witchy-wir's rewrite-relation self-checks.
pub fn lowered_candidate(
    checked: &witchy_types::pipeline::CheckedModule,
) -> witchy_wir::canonical_safewir::CanonicalModule<witchy_wir::canonical_safewir::LoweredCandidate> {
    let source = checked.canonical_source().expect("canonical source");
    let layouts = crate::canonical_layouts::derive_layout_bundle(source).expect("checked layouts");
    crate::canonical_lower::lower_typed_module(source, layouts).expect("typed candidate")
        .with_launch_contract_payload(source.launch_contract_payload().to_vec())
}

pub fn task_producer_substitution_requires_actual_execution_provenance() {
    use witchy_types::runtime_type::{
        AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
    };
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::none()));
    let worker = "mode opt\nimport task\n\npub async fn compatible() -> Int:\n    7\n\npub async fn execute(own recipe: task.Task(Int)) -> Int:\n    recipe.await\n";
    let main = "import worker\n\nasync fn ordinary() -> Int:\n    9\n\nasync fn main(console: Console):\n    let first = worker.execute(worker.compatible()).await\n    let second = ordinary().await\n    console.print(\"${first * 10 + second}\")\n";
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "example/task-producer", "0.1.0")
        .expect("workspace coordinate");
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "0.1.0")
        .expect("toolchain coordinate");
    let mut assignments = ["main", "worker"].map(|name| (
        name.to_owned(), ModuleLoadIdentity::new(workspace.clone(), [name]).expect("user owner"),
    )).to_vec();
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
        (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).expect("std owner"),
    )));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments).expect("owners");
    let checked = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("main".into(), main.as_bytes().to_vec()), ("worker".into(), worker.as_bytes().to_vec())],
        "main", witchy_syntax::linker::ComptimeExpander::new(
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())),
        &std::collections::HashSet::from(["main".into(), "worker".into()]), owners,
    ).expect("checked source: opt recipe enters opt consumer; ordinary recipe stays normal");
    let source = checked.canonical_source().expect("canonical source");
    let role = source.view().deferred_task_role().expect("authenticated generic Task/Step storage role");
    assert_ne!(role.task_identity(), role.step_identity(), "Task and Step are distinct checked roles");
    let layouts = crate::canonical_layouts::derive_layout_bundle(source).expect("checked layouts");
    let candidate = crate::canonical_lower::lower_typed_module(source, layouts).expect("typed candidate")
        .with_launch_contract_payload(source.launch_contract_payload().to_vec());
    let distinct_ordinals = candidate.source_abi_bindings().entries().iter().find_map(|binding| {
        let runtime = binding.source_declaration_ordinal();
        source.view().closed_callable_item_ordinal(runtime)
            .filter(|item| *item != runtime).map(|item| (runtime, item))
    }).expect("runtime callable and source item ordinals diverge in this bundle");
    let mapped = source.view().closed_callable_declaration(distinct_ordinals.0).expect("mapped callable");
    assert!(source.view().module().items.get(distinct_ordinals.1 as usize)
        .is_some_and(|item| matches!(item, witchy_syntax::ast::Item::Function(function) if std::ptr::eq(function, mapped))));
    let function_id = |name: &str| candidate.source_abi_bindings().entries().iter().find_map(|binding| {
        let function = source.view().closed_callable_declaration(binding.source_declaration_ordinal())?;
        (function.name == name).then_some(binding.function())
    }).unwrap_or_else(|| panic!("missing source callable {name}; related: {:?}",
        source.typed_module().module().items.iter().filter_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) if function.name.contains("ordinary") => Some(&function.name),
            _ => None,
        }).collect::<Vec<_>>()));
    let compatible = function_id("worker.compatible");
    let ordinary = function_id("main.ordinary");
    let task_dispatch = function_id("task.poll__Int");
    let signature = |id| {
        let function = candidate.functions().iter().find(|f| f.id == id).expect("bound function");
        candidate.signatures().iter().find(|s| s.id == function.signature).expect("signature")
    };
    let expected = signature(compatible);
    let replacement = signature(ordinary);
    assert_eq!(expected.parameters, replacement.parameters, "same input ABI");
    assert_eq!(expected.results, replacement.results, "same output ABI");
    assert_eq!(expected.contract, replacement.contract, "same ownership ABI");
    witchy_canonical_source::aggregates::with_shape_memo(source.type_definitions(), || {
        let baseline = check_initial_with_witness(candidate.clone(), &source.view()).expect("baseline initial admission");
        let baseline = validate_final(baseline, &source.view()).expect("baseline final admission");
        witchy_wir::admit_canonical_with_observer(baseline, &source.view(), |_, _| ())
            .expect("unoptimized baseline admitted Wasm");
        let mut substitutions = 0;
        let mutated = candidate.clone().transform_functions(|functions| {
            for function in functions {
                for block in &mut function.blocks {
                    for operation in &mut block.operations {
                        if let Operation::Call { callee, .. } = operation
                            && *callee == compatible
                        {
                            *callee = ordinary;
                            substitutions += 1;
                        }
                    }
                }
            }
        });
        assert_eq!(substitutions, 1, "mutate one actual task producer, not source metadata");
        let admitted = check_initial_with_witness(mutated, &source.view())
            .map_err(|error| format!("initial: {error:?}"))
            .and_then(|initial| validate_final(initial, &source.view()).map_err(|error| format!("final: {error:?}")))
            .and_then(|validated| witchy_wir::admit_canonical_with_observer(validated, &source.view(), |_, _| ())
                .map_err(|error| format!("Wasm admission: {error:?}")));
        let direct_rejected = admitted.as_ref().is_err_and(|error| error.contains("TaskBehaviorMismatch"));
        let factory_runtime_ordinal = candidate.source_abi_bindings().entries().iter()
            .find(|binding| binding.function() == compatible)
            .expect("factory source binding").source_declaration_ordinal();
        let ordinary_runtime_ordinal = candidate.source_abi_bindings().entries().iter()
            .find(|binding| binding.function() == ordinary)
            .expect("ordinary source binding").source_declaration_ordinal();
        let source_item = |runtime_ordinal| {
            let declaration = source.view().closed_callable_declaration(runtime_ordinal).expect("closed callable");
            source.view().module().items.iter().position(|item| match item {
                witchy_syntax::ast::Item::Function(function) => std::ptr::eq(function, declaration), _ => false,
            }).expect("exact final source item") as u32
        };
        let factory_ordinal = source_item(factory_runtime_ordinal);
        let ordinary_ordinal = source_item(ordinary_runtime_ordinal);
        let thunk_for = |ordinal| candidate.closure_bindings().entries().iter().find_map(|binding| {
            let witness = source.closure_witness().entries().get(binding.closure() as usize)?;
            (witness.id().item == ordinal && witness.id().method.is_none()).then_some(binding.function())
        }).expect("generated async thunk closure binding");
        let compatible_thunk = thunk_for(factory_ordinal);
        let ordinary_thunk = thunk_for(ordinary_ordinal);
        let lazy = function_id("task.lazy__Int");
        let poll = candidate.functions().iter().find(|function| function.id == lazy).expect("lazy wrapper")
            .blocks.iter().flat_map(|block| &block.operations).find_map(|operation| match operation {
                Operation::FunctionValue { function, .. } => Some(*function), _ => None,
            }).expect("lazy poll closure");
        let poll_signature = candidate.functions().iter().find(|function| function.id == poll).expect("poll function").signature;
        let poll_alternates = candidate.functions().iter()
            .filter(|function| function.signature == poll_signature)
            .map(|function| function.id).filter(|function| *function != poll).collect::<Vec<_>>();
        assert_eq!(poll_alternates.len(), 1, "fixture has one same-ABI alternate poll body");
        let alternate_poll = poll_alternates[0];
        assert_eq!(signature(compatible_thunk).parameters, signature(ordinary_thunk).parameters,
            "same thunk input ABI");
        assert_eq!(signature(compatible_thunk).results, signature(ordinary_thunk).results,
            "same thunk output ABI");
        let mut thunk_substitutions = 0;
        let mutated_factory = candidate.clone().transform_functions(|functions| {
            for function in functions {
                if function.id != compatible { continue }
                for block in &mut function.blocks {
                    for operation in &mut block.operations {
                        if let Operation::FunctionValue { function, .. } = operation
                            && *function == compatible_thunk
                        {
                            *function = ordinary_thunk;
                            thunk_substitutions += 1;
                        }
                    }
                }
            }
        });
        assert_eq!(thunk_substitutions, 1, "mutate actual stored execution closure");
        let factory_admitted = check_initial_with_witness(mutated_factory, &source.view())
            .map_err(|error| format!("initial: {error:?}"))
            .and_then(|initial| validate_final(initial, &source.view()).map_err(|error| format!("final: {error:?}")))
            .and_then(|validated| witchy_wir::admit_canonical_with_observer(validated, &source.view(), |_, _| ())
                .map_err(|error| format!("Wasm admission: {error:?}")));
        assert!(direct_rejected, "same-ABI normal producer must not inherit the opt source operand's proof");
        assert!(factory_admitted.as_ref().is_err_and(|error| error.contains("TaskBehaviorMismatch")),
            "same-ABI normal thunk must fail task authority, error {:?}", factory_admitted.as_ref().err());
        let mut poll_substitutions = 0;
        let mutated_wrapper = candidate.clone().transform_functions(|functions| {
            for function in functions {
                if function.id != lazy { continue }
                for block in &mut function.blocks {
                    for operation in &mut block.operations {
                        if let Operation::FunctionValue { function, .. } = operation
                            && *function == poll
                        {
                            *function = alternate_poll;
                            poll_substitutions += 1;
                        }
                    }
                }
            }
        });
        assert_eq!(poll_substitutions, 1, "mutate the actual task.lazy poll adapter");
        let wrapper_admitted = check_initial_with_witness(mutated_wrapper, &source.view())
            .map_err(|error| format!("initial: {error:?}"))
            .and_then(|initial| validate_final(initial, &source.view()).map_err(|error| format!("final: {error:?}")))
            .and_then(|validated| witchy_wir::admit_canonical_with_observer(validated, &source.view(), |_, _| ())
                .map_err(|error| format!("Wasm admission: {error:?}")));
        assert!(wrapper_admitted.as_ref().is_err_and(|error| error.contains("TaskBehaviorMismatch")),
            "same-ABI poll replacement must fail task authority, error {:?}", wrapper_admitted.as_ref().err());
        let mut alternate_returns = 0;
        let mutated_wrapper_transport = candidate.clone().transform_functions(|functions| {
            for function in functions {
                if function.id != lazy { continue }
                for block in &mut function.blocks {
                    let Terminator::Return { values } = &block.terminator else { continue };
                    let [old] = values.as_slice() else { continue };
                    let old = *old;
                    let fresh = ValueId::new(function.value_types.keys().map(|value| value.get()).max().unwrap_or(0) + 1);
                    function.value_types.insert(fresh, function.value_types[&old]);
                    block.operations.push(Operation::DropRef { object: old });
                    block.operations.push(Operation::Call {
                        destinations: vec![fresh], callee: ordinary, arguments: vec![],
                    });
                    block.terminator = Terminator::Return { values: vec![fresh] };
                    alternate_returns += 1;
                }
            }
        });
        assert_eq!(alternate_returns, 1, "mutate lazy wrapper's actual returned recipe");
        let transport_admitted = check_initial_with_witness(mutated_wrapper_transport, &source.view())
            .map_err(|error| format!("initial: {error:?}"))
            .and_then(|initial| validate_final(initial, &source.view()).map_err(|error| format!("final: {error:?}")))
            .and_then(|validated| witchy_wir::admit_canonical_with_observer(validated, &source.view(), |_, _| ())
                .map_err(|error| format!("Wasm admission: {error:?}")));
        assert!(transport_admitted.as_ref().is_err_and(|error| error.contains("TaskBehaviorMismatch")),
            "unused checked thunk must fail task authority, error {:?}", transport_admitted.as_ref().err());
        let mut poll_invocations = 0;
        let mutated_poll_body = candidate.clone().transform_functions(|functions| {
            for function in functions {
                if function.id != poll { continue }
                for block in &mut function.blocks {
                    let Some(index) = block.operations.iter().position(|operation|
                        matches!(operation, Operation::VarCall { .. })) else { continue };
                    let Operation::VarCall { destinations, arguments, .. } = &block.operations[index] else { unreachable!() };
                    let [destination] = destinations.as_slice() else { continue };
                    let [witchy_wir::canonical_safewir::CallArgument::Move(input)] = arguments.as_slice() else { continue };
                    let destination = *destination;
                    let input = *input;
                    block.operations[index] = Operation::DropRef { object: input };
                    block.operations.insert(index + 1, Operation::Call {
                        destinations: vec![destination], callee: ordinary, arguments: vec![],
                    });
                    poll_invocations += 1;
                }
            }
        });
        assert_eq!(poll_invocations, 1, "mutate the selected poll callback's actual invocation");
        let body_admitted = check_initial_with_witness(mutated_poll_body, &source.view())
            .map_err(|error| format!("initial: {error:?}"))
            .and_then(|initial| validate_final(initial, &source.view()).map_err(|error| format!("final: {error:?}")))
            .and_then(|validated| witchy_wir::admit_canonical_with_observer(validated, &source.view(), |_, _| ())
                .map_err(|error| format!("Wasm admission: {error:?}")));
        assert!(body_admitted.as_ref().is_err_and(|error| error.contains("TaskBehaviorMismatch")),
            "selected poll substitution must fail task authority, error {:?}", body_admitted.as_ref().err());
        let alternate_poll_signature = signature(alternate_poll).id;
        let mut dispatch_substitutions = 0;
        let mutated_dispatch = candidate.clone().transform_functions(|functions| {
            for function in functions {
                if function.id != task_dispatch { continue }
                for block in &mut function.blocks {
                    let Terminator::VarTailCall { callee, signature, .. } = &mut block.terminator else { continue };
                    assert_eq!(*signature, alternate_poll_signature);
                    let fresh = ValueId::new(function.value_types.keys().map(|value| value.get()).max().unwrap_or(0) + 1);
                    function.value_types.insert(fresh, witchy_wir::canonical_safewir::ValueType::Callable(*signature));
                    block.operations.push(Operation::FunctionValue { destination: fresh, function: alternate_poll });
                    *callee = fresh;
                    dispatch_substitutions += 1;
                }
            }
        });
        assert_eq!(dispatch_substitutions, 1, "mutate the Task poller's actual callable dispatch");
        let dispatch_admitted = check_initial_with_witness(mutated_dispatch, &source.view())
            .map_err(|error| format!("initial: {error:?}"))
            .and_then(|initial| validate_final(initial, &source.view()).map_err(|error| format!("final: {error:?}")))
            .and_then(|validated| witchy_wir::admit_canonical_with_observer(validated, &source.view(), |_, _| ())
                .map_err(|error| format!("Wasm admission: {error:?}")));
        assert!(dispatch_admitted.as_ref().is_err_and(|error| error.contains("TaskBehaviorMismatch")),
            "Task poller must invoke the carried callable, error {:?}", dispatch_admitted.as_ref().err());
        let mut thunk_dispatches = 0;
        let mutated_thunk_body = candidate.clone().transform_functions(|functions| {
            for function in functions {
                if function.id != compatible_thunk { continue }
                for block in &mut function.blocks {
                    if let Terminator::TailCall { callee, arguments, .. } = &mut block.terminator {
                        *callee = ordinary;
                        arguments.clear();
                        thunk_dispatches += 1;
                    }
                }
            }
        });
        assert_eq!(thunk_dispatches, 1, "mutate the selected async thunk's deferred dispatch");
        let thunk_body_admitted = check_initial_with_witness(mutated_thunk_body, &source.view())
            .map_err(|error| format!("initial: {error:?}"))
            .and_then(|initial| validate_final(initial, &source.view()).map_err(|error| format!("final: {error:?}")))
            .and_then(|validated| witchy_wir::admit_canonical_with_observer(validated, &source.view(), |_, _| ())
                .map_err(|error| format!("Wasm admission: {error:?}")));
        assert!(thunk_body_admitted.is_err(), "selected opt thunk must not delegate its deferred body to ordinary code");
    });
    witchy_syntax::opt::set_for_tests(None);
}
