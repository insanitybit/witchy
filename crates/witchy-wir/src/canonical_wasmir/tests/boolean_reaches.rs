use super::*;

fn signature() -> FunctionSignature {
    FunctionSignature {
        id: FunctionSignatureId::new(0), parameters: Vec::new(), results: Vec::new(),
        contract: CallableContract { parameters: Vec::new(), results: Vec::new(), parameter_effects: BTreeMap::new() },
    }
}

fn graph(edges: &[&[u32]]) -> Function {
    let mut value_types = BTreeMap::new();
    let blocks = edges.iter().enumerate().map(|(index, successors)| {
        let mut operations = Vec::new();
        let terminator = match *successors {
            [] => Terminator::Return { values: Vec::new() },
            [target] => Terminator::Branch { target: BlockId::new(*target), arguments: Vec::new() },
            [left, right] => {
                let condition = ValueId::new(index as u32);
                value_types.insert(condition, ValueType::Bool);
                operations.push(Operation::ConstBool { destination: condition, value: true });
                Terminator::BranchIf { condition,
                    then_target: BlockId::new(*left), then_arguments: Vec::new(),
                    else_target: BlockId::new(*right), else_arguments: Vec::new() }
            }
            _ => panic!("fixture has at most two successors"),
        };
        BasicBlock { id: BlockId::new(index as u32), parameters: Vec::new(), operations, terminator }
    }).collect();
    Function { id: FunctionId::new(0), signature: signature().id, entry: BlockId::new(0), value_types, blocks }
}

fn candidate(function: Function) -> CanonicalModule<LoweredCandidate> {
    CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0), checked_source: CheckedSourceIdentity::from_digest([31; 32]),
        layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
        signatures: vec![signature()], imports: Vec::new(), functions: vec![function], proper_tail_sites: Vec::new(),
    })
}

// Exercise the actual two private query implementations, each against its
// own unchanged full-distance implementation. No production helper or cache
// is introduced for testing. Arbitrary closed graphs need not be structured.
fn with_graph(edges: &[&[u32]], test: impl FnOnce(&StructuredEmitter<'_>, &PlanEmitter<'_>)) {
    let function = graph(edges);
    let signature = signature();
    let layouts = LayoutInterner::new();
    let imports = BTreeMap::new();
    let function_indices = BTreeMap::new();
    let signature_indices = BTreeMap::new();
    let import_indices = BTreeMap::new();
    let static_data = BTreeMap::new();
    let target = lower_function(&function, &layouts, &imports, &[(signature.id, signature.clone())].into(), &function_indices).unwrap();
    let layout = local_layout(&function, &signature, &layouts).unwrap();
    let successors: BTreeMap<_, Vec<_>> = edges.iter().enumerate().map(|(index, next)|
        (BlockId::new(index as u32), next.iter().copied().map(BlockId::new).collect())).collect();
    let mut predecessors: BTreeMap<_, Vec<_>> = successors.keys().map(|id| (*id, Vec::new())).collect();
    for (from, next) in &successors {
        for to in next { predecessors.get_mut(to).expect("query fixture is a closed graph").push(*from); }
    }
    let mut body = WasmFunctionEncoder::new([]);
    let encoder = StructuredEmitter {
        body: &mut body, source_function: &function, target_function: &target,
        blocks_by_id: target.blocks.iter().map(|block| (block.id, block)).collect(),
        layout: &layout, layouts: &layouts, function_indices: &function_indices, signature_indices: &signature_indices,
        import_indices: &import_indices, imports: &imports, static_data: &static_data, owner_cleanup_function: None,
        successors_by_block: successors.clone(), predecessors_by_block: predecessors.clone(),
        emitted: BTreeSet::new(), labels: Vec::new(), loops: Vec::new(), loop_exits: Vec::new(),
        loop_headers: RefCell::new(BTreeMap::new()), loop_exit_targets: RefCell::new(BTreeMap::new()),
        back_edge_source_facts: RefCell::new(BTreeMap::new()), natural_loop_facts: RefCell::new(BTreeMap::new()),
        reaches_avoiding_facts: RefCell::new(BTreeMap::new()), path_facts: RefCell::new(BTreeMap::new()),
    };
    let planner = PlanEmitter {
        source_function: &function, blocks_by_id: function.blocks.iter().map(|block| (block.id, block)).collect(),
        layout: &layout, layouts: &layouts, function_indices: &function_indices, signature_indices: &signature_indices,
        imports: &imports, import_indices: &import_indices, static_data_offsets: &static_data, owner_cleanup_function: None,
        successors_by_block: successors, predecessors_by_block: predecessors,
        plan: Vec::new(), emitted: BTreeSet::new(), labels: Vec::new(), loops: Vec::new(), aggregates: BTreeMap::new(), loop_exits: Vec::new(),
        loop_headers: RefCell::new(BTreeMap::new()), loop_exit_targets: RefCell::new(BTreeMap::new()),
        back_edge_source_facts: RefCell::new(BTreeMap::new()), natural_loop_facts: RefCell::new(BTreeMap::new()),
        reaches_avoiding_facts: RefCell::new(BTreeMap::new()), path_facts: RefCell::new(BTreeMap::new()),
    };
    test(&encoder, &planner);
}

#[test]
fn boolean_reaches_matches_distances_for_cycles_diamonds_and_disconnected_nodes() {
    with_graph(&[&[1, 2], &[3], &[3], &[4], &[3], &[5], &[]], |encoder, planner| {
        for from in 0..7 {
            let from = BlockId::new(from);
            let encoded_distances = encoder.distances_from(from).unwrap();
            let planned_distances = planner.distances_from(from).unwrap();
            for to in (0..7).chain([99]) {
                let to = BlockId::new(to);
                assert_eq!(encoder.reaches(from, to).unwrap(), encoded_distances.contains_key(&to));
                assert_eq!(planner.reaches(from, to).unwrap(), planned_distances.contains_key(&to));
            }
        }
        assert!(encoder.reaches(BlockId::new(0), BlockId::new(4)).unwrap());
        assert!(!planner.reaches(BlockId::new(4), BlockId::new(0)).unwrap());
        assert!(planner.reaches(BlockId::new(6), BlockId::new(6)).unwrap());
        assert!(matches!(encoder.reaches(BlockId::new(99), BlockId::new(99)), Err(EncodeError::MissingBlock { .. })));
        assert_eq!(planner.reaches(BlockId::new(99), BlockId::new(99)), Err(CanonicalAdmissionError::UnsupportedPlan));
    });
}

#[test]
fn boolean_reaches_matches_distances_on_long_chain() {
    let edges: Vec<Vec<u32>> = (0..256).map(|index| if index == 255 { Vec::new() } else { vec![index + 1] }).collect();
    with_graph(&edges.iter().map(Vec::as_slice).collect::<Vec<_>>(), |encoder, planner| {
        for from in [0, 1, 127, 255] {
            let from = BlockId::new(from);
            let encoded = encoder.distances_from(from).unwrap();
            let planned = planner.distances_from(from).unwrap();
            for to in [0, 1, 127, 255] {
                let to = BlockId::new(to);
                assert_eq!(encoder.reaches(from, to).unwrap(), encoded.contains_key(&to));
                assert_eq!(planner.reaches(from, to).unwrap(), planned.contains_key(&to));
            }
        }
    });
}

#[test]
fn boolean_reaches_missing_sibling_edge_refuses_before_backend_traversal() {
    // The left sibling is immediately reachable, but the other sibling has
    // a missing successor. Early query success is not admission authority.
    let function = graph(&[&[1, 2], &[], &[99]]);
    assert!(matches!(crate::canonical_safewir::check_initial(candidate(function.clone())),
        Err(crate::canonical_safewir::InitialCheckError::MissingBlock { block, .. }) if block == BlockId::new(99)));
    let signature = signature();
    let layouts = LayoutInterner::new();
    let imports = BTreeMap::new();
    let functions = BTreeMap::new();
    let signatures = [(signature.id, signature.clone())].into();
    let target = lower_function(&function, &layouts, &imports, &signatures, &functions).unwrap();
    let helpers = outlined_memory::Helpers::new(super::checked_scalar().module(), 1).unwrap();
    // Probe the real encoder and independent-plan graph constructors too,
    // rather than asserting that a hand-made query context validates a CFG.
    assert!(matches!(encode_function(&function, &signature, &target, &functions, &BTreeMap::new(),
        &BTreeMap::new(), &imports, &BTreeMap::new(), &layouts, &[], None, &helpers),
        Err(EncodeError::MissingBlock { block, .. }) if block == BlockId::new(99)));
    assert!(matches!(expected_function_plan(&function, &signature, &layouts, &functions, &BTreeMap::new(),
        &imports, &BTreeMap::new(), &BTreeMap::new(), None, &[], &helpers), Err(CanonicalAdmissionError::UnsupportedPlan)));
}

#[test]
fn boolean_reaches_closed_diamond_still_passes_independent_admission() {
    let checked = check_initial_with_test_witness(candidate(graph(&[&[1, 2], &[3], &[3], &[]]))).unwrap();
    let validated = validate_final_for_test(checked).unwrap();
    let target = lower_validated(&validated).unwrap();
    let checked_for_bytes = check_initial_with_test_witness(candidate(graph(&[&[1, 2], &[3], &[3], &[]]))).unwrap();
    let bytes = encode_validated(validate_final_for_test(checked_for_bytes).unwrap()).unwrap();
    decode_and_match_plan(validated.module(), &target, &bytes).unwrap();
}
