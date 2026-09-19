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
    with_graph_terminal(edges, Terminator::Return { values: Vec::new() }, test);
}

fn with_graph_terminal(edges: &[&[u32]], terminal: Terminator, test: impl FnOnce(&StructuredEmitter<'_>, &PlanEmitter<'_>)) {
    let mut function = graph(edges);
    for block in &mut function.blocks {
        if edges[block.id.get() as usize].is_empty() { block.terminator = terminal.clone(); }
    }
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
        immediate_dominators: std::cell::OnceCell::new(),
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
        immediate_dominators: std::cell::OnceCell::new(),
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

#[test]
fn cached_natural_loop_membership_matches_full_sets_for_nested_loops() {
    with_graph(&[&[1], &[2, 5], &[3], &[4, 1], &[3], &[]], |encoder, planner| {
        for header in [1, 3] {
            let header = BlockId::new(header);
            let encoded_members = encoder.natural_loop_members(header).unwrap();
            let planned_members = planner.natural_loop_members(header).unwrap();
            for _ in 0..64 {
                for block in (0..6).chain([99]).map(BlockId::new) {
                    assert_eq!(encoder.in_natural_loop(header, block).unwrap(),
                        block != header && encoded_members.contains(&block));
                    assert_eq!(planner.in_natural_loop(header, block).unwrap(),
                        block != header && planned_members.contains(&block));
                }
            }
        }
        assert!(encoder.in_natural_loop(BlockId::new(1), BlockId::new(4)).unwrap());
        assert!(!planner.in_natural_loop(BlockId::new(3), BlockId::new(2)).unwrap());
        assert!(matches!(encoder.in_natural_loop(BlockId::new(99), BlockId::new(0)),
            Err(EncodeError::MissingBlock { .. })));
        assert_eq!(planner.in_natural_loop(BlockId::new(99), BlockId::new(0)),
            Err(CanonicalAdmissionError::UnsupportedPlan));
    });
}

// Saved pre-change predicate, including its exact-key memo and active-path
// ordering. It deliberately does not call either new query implementation.
trait OldPathGraph {
    type Error: std::fmt::Debug + PartialEq;
    fn terminal(&self, block: BlockId) -> Result<bool, Self::Error>;
    fn header(&self, block: BlockId) -> Result<bool, Self::Error>;
    fn exit(&self, block: BlockId) -> Result<Option<BlockId>, Self::Error>;
    fn next(&self, block: BlockId) -> Result<Vec<BlockId>, Self::Error>;
}

impl OldPathGraph for StructuredEmitter<'_> {
    type Error = EncodeError;
    fn terminal(&self, block: BlockId) -> Result<bool, Self::Error> { self.is_terminal(block) }
    fn header(&self, block: BlockId) -> Result<bool, Self::Error> { self.is_loop_header(block) }
    fn exit(&self, block: BlockId) -> Result<Option<BlockId>, Self::Error> { self.loop_exit(block) }
    fn next(&self, block: BlockId) -> Result<Vec<BlockId>, Self::Error> { self.successors(block) }
}

impl OldPathGraph for PlanEmitter<'_> {
    type Error = CanonicalAdmissionError;
    fn terminal(&self, block: BlockId) -> Result<bool, Self::Error> { self.is_terminal(block) }
    fn header(&self, block: BlockId) -> Result<bool, Self::Error> { self.is_loop_header(block) }
    fn exit(&self, block: BlockId) -> Result<Option<BlockId>, Self::Error> { self.loop_exit(block) }
    fn next(&self, block: BlockId) -> Result<Vec<BlockId>, Self::Error> { self.successors(block) }
}

fn old_paths<G: OldPathGraph>(
    graph: &G,
    start: BlockId,
    merges: &[BlockId],
    active: &mut BTreeSet<BlockId>,
    facts: &mut BTreeMap<(BlockId, Vec<BlockId>), bool>,
    visits: &mut usize,
) -> Result<bool, G::Error> {
    *visits += 1;
    if merges.contains(&start) || graph.terminal(start)? { return Ok(true); }
    let mut merge_key = merges.to_vec();
    merge_key.sort();
    if let Some(result) = facts.get(&(start, merge_key.clone())) { return Ok(*result); }
    if !active.insert(start) { return Ok(false); }
    let result = if graph.header(start)? {
        let exit = graph.exit(start)?;
        let mut body_merges = vec![start];
        body_merges.extend(exit);
        let mut result = true;
        for successor in graph.next(start)? {
            if !old_paths(graph, successor, &body_merges, active, facts, visits)? {
                result = false;
                break;
            }
        }
        result && match exit {
            Some(exit) => old_paths(graph, exit, merges, active, facts, visits)?,
            None => true,
        }
    } else {
        let mut result = true;
        for successor in graph.next(start)? {
            if !old_paths(graph, successor, merges, active, facts, visits)? {
                result = false;
                break;
            }
        }
        result
    };
    active.remove(&start);
    facts.insert((start, merge_key), result);
    Ok(result)
}

#[test]
fn exact_paths_match_old_predicate_for_reconvergence_loops_and_stop_contexts() {
    let graphs: &[&[&[u32]]] = &[
        &[&[1, 2], &[3], &[3], &[4, 5], &[6], &[6], &[]],
        &[&[1], &[2, 3], &[1], &[]],
        &[&[1], &[2, 5], &[3], &[4, 1], &[3], &[]],
        &[&[1, 2], &[2], &[1]], // Irreducible cycle, not a natural-loop shortcut.
        &[&[1, 2], &[], &[], &[3]], // Disconnected closed loop and terminal exits.
    ];
    for edges in graphs {
        with_graph(edges, |encoder, planner| {
            let mut encoded_facts = BTreeMap::new();
            let mut planned_facts = BTreeMap::new();
            let mut visits = 0;
            // Keep the old and new caches alive through the same sequence,
            // including smaller, incomparable, reordered and duplicate stops.
            for raw_stops in [vec![1], vec![1, 2], vec![2, 1, 1], vec![], vec![2], vec![99], vec![0]] {
                let stops: Vec<_> = raw_stops.into_iter().map(BlockId::new).collect();
                for raw_start in 0..edges.len() as u32 {
                    let start = BlockId::new(raw_start);
                    let mut old_active = BTreeSet::new();
                    let mut new_active = BTreeSet::new();
                    let expected = old_paths(encoder, start, &stops, &mut old_active, &mut encoded_facts, &mut visits);
                    let actual = encoder.all_paths_reach_or_terminate(start, &stops, &mut new_active);
                    assert_eq!(actual, expected, "encoder edges={edges:?}, start={start:?}, stops={stops:?}");
                    assert_eq!(new_active, old_active);
                    let mut old_active = BTreeSet::new();
                    let mut new_active = BTreeSet::new();
                    let expected = old_paths(planner, start, &stops, &mut old_active, &mut planned_facts, &mut visits);
                    let actual = planner.all_paths_reach_or_terminate(start, &stops, &mut new_active);
                    assert_eq!(actual, expected, "planner edges={edges:?}, start={start:?}, stops={stops:?}");
                    assert_eq!(new_active, old_active);
                }
            }
        });
    }
}

#[test]
fn exact_paths_preserve_active_and_distinct_stop_contexts() {
    with_graph(&[&[1], &[2], &[]], |encoder, planner| {
        let root = BlockId::new(0);
        let end = BlockId::new(2);
        assert!(encoder.all_paths_reach_or_terminate(root, &[end], &mut BTreeSet::new()).unwrap());
        assert!(planner.all_paths_reach_or_terminate(root, &[end], &mut BTreeSet::new()).unwrap());
        // Warming one stop key must not override an active-path refusal for
        // another key. Keep the original exact memo's lookup order.
        for (ancestor, extra) in [(0, 98), (1, 99)] {
            let stops = [end, BlockId::new(extra)];
            let initial = BTreeSet::from([BlockId::new(ancestor)]);
            let mut encoded_active = initial.clone();
            let mut planned_active = initial.clone();
            assert!(!encoder.all_paths_reach_or_terminate(root, &stops, &mut encoded_active).unwrap());
            assert!(!planner.all_paths_reach_or_terminate(root, &stops, &mut planned_active).unwrap());
            assert_eq!(encoded_active, initial);
            assert_eq!(planned_active, initial);
            assert_eq!(old_paths(encoder, root, &stops, &mut initial.clone(), &mut BTreeMap::new(), &mut 0).unwrap(), false);
            assert_eq!(old_paths(planner, root, &stops, &mut initial.clone(), &mut BTreeMap::new(), &mut 0).unwrap(), false);
        }
    });
    with_graph(&[&[1, 2], &[2], &[1]], |encoder, planner| {
        for (stops, expected) in [(vec![1], true), (vec![], false), (vec![99], false), (vec![2], true)] {
            let stops: Vec<_> = stops.into_iter().map(BlockId::new).collect();
            assert_eq!(encoder.all_paths_reach_or_terminate(BlockId::new(0), &stops, &mut BTreeSet::new()).unwrap(), expected);
            assert_eq!(planner.all_paths_reach_or_terminate(BlockId::new(0), &stops, &mut BTreeSet::new()).unwrap(), expected);
        }
    });
}

#[test]
fn exact_paths_preserve_exact_refusal_after_active_query() {
    for (active_stops, repeated_stops) in [
        (vec![2, 3], vec![3, 2]),
        (vec![3, 2, 2], vec![2, 3, 2]),
    ] {
        with_graph(&[&[1], &[2], &[3], &[]], |encoder, planner| {
            let root = BlockId::new(0);
            let mut encoded_facts = BTreeMap::new();
            let mut planned_facts = BTreeMap::new();
            let mut visits = 0;
            // Keep all three queries in the same memo history. The final
            // empty-active query must retain the exact false from step two,
            // even though step one succeeded with fewer stopping points.
            for (raw_stops, initial_active, expected) in [
                (vec![3], BTreeSet::new(), true),
                (active_stops, BTreeSet::from([BlockId::new(1)]), false),
                (repeated_stops, BTreeSet::new(), false),
            ] {
                let stops: Vec<_> = raw_stops.into_iter().map(BlockId::new).collect();
                let mut old_active = initial_active.clone();
                let mut encoded_active = initial_active.clone();
                let old_encoded = old_paths(encoder, root, &stops, &mut old_active, &mut encoded_facts, &mut visits);
                assert_eq!(old_encoded, Ok(expected));
                assert_eq!(encoder.all_paths_reach_or_terminate(root, &stops, &mut encoded_active), old_encoded);
                assert_eq!(encoded_active, old_active);
                let mut old_active = initial_active.clone();
                let mut planned_active = initial_active;
                let old_planned = old_paths(planner, root, &stops, &mut old_active, &mut planned_facts, &mut visits);
                assert_eq!(old_planned, Ok(expected));
                assert_eq!(planner.all_paths_reach_or_terminate(root, &stops, &mut planned_active), old_planned);
                assert_eq!(planned_active, old_active);
            }
            // Pin terminal/stop precedence separately from exact-cache
            // precedence. These deliberately seeded entries are not claims
            // that a normal query can cache a refusal for a terminal block.
            for (start, stops) in [(BlockId::new(3), vec![]), (root, vec![root])] {
                let key = (start, stops.clone());
                encoder.path_query_facts(&stops).borrow_mut().insert(start, false);
                planner.path_query_facts(&stops).borrow_mut().insert(start, false);
                encoded_facts.insert(key.clone(), false);
                planned_facts.insert(key, false);
                assert_eq!(old_paths(encoder, start, &stops, &mut BTreeSet::new(), &mut encoded_facts, &mut visits), Ok(true));
                assert_eq!(old_paths(planner, start, &stops, &mut BTreeSet::new(), &mut planned_facts, &mut visits), Ok(true));
                assert_eq!(encoder.all_paths_reach_or_terminate(start, &stops, &mut BTreeSet::new()), Ok(true));
                assert_eq!(planner.all_paths_reach_or_terminate(start, &stops, &mut BTreeSet::new()), Ok(true));
            }
        });
    }
}

#[test]
fn exact_paths_preserve_terminal_and_missing_block_behavior() {
    for terminal in [Terminator::Return { values: Vec::new() }, Terminator::Trap, Terminator::Unreachable] {
        with_graph_terminal(&[&[1, 2], &[], &[]], terminal, |encoder, planner| {
            for start in [0, 1, 2, 99].map(BlockId::new) {
                for stops in [vec![], vec![BlockId::new(1)], vec![BlockId::new(99)]] {
                    let encoded = old_paths(encoder, start, &stops, &mut BTreeSet::new(), &mut BTreeMap::new(), &mut 0);
                    let planned = old_paths(planner, start, &stops, &mut BTreeSet::new(), &mut BTreeMap::new(), &mut 0);
                    assert_eq!(encoder.all_paths_reach_or_terminate(start, &stops, &mut BTreeSet::new()), encoded);
                    assert_eq!(planner.all_paths_reach_or_terminate(start, &stops, &mut BTreeSet::new()), planned);
                }
            }
        });
    }
}

#[test]
fn exact_paths_share_stop_keys_but_preserve_every_node_fact() {
    let mut edges = Vec::new();
    for diamond in 0..32 {
        let base = diamond * 3;
        edges.push(vec![base + 1, base + 2]);
        edges.push(vec![base + 3]);
        edges.push(vec![base + 3]);
    }
    edges.push(Vec::new());
    with_graph(&edges.iter().map(Vec::as_slice).collect::<Vec<_>>(), |encoder, planner| {
        let start = BlockId::new(0);
        let end = BlockId::new(96);
        let mut encoded_facts = BTreeMap::new();
        let mut planned_facts = BTreeMap::new();
        let mut encoded_visits = 0;
        let mut planned_visits = 0;
        // The duplicate-heavy key was previously cloned and sorted at every
        // nonterminal node. It is now retained once per exact memo group.
        let stops = vec![end; 64];
        for _ in 0..2 {
            assert_eq!(encoder.all_paths_reach_or_terminate(start, &stops, &mut BTreeSet::new()),
                old_paths(encoder, start, &stops, &mut BTreeSet::new(), &mut encoded_facts, &mut encoded_visits));
            assert_eq!(planner.all_paths_reach_or_terminate(start, &stops, &mut BTreeSet::new()),
                old_paths(planner, start, &stops, &mut BTreeSet::new(), &mut planned_facts, &mut planned_visits));
        }
        assert_eq!(encoder.path_facts.borrow().len(), 1);
        assert_eq!(planner.path_facts.borrow().len(), 1);
        assert_eq!(encoder.path_query_facts(&stops).borrow().len(), 96);
        assert_eq!(planner.path_query_facts(&stops).borrow().len(), 96);
        assert_eq!(flatten_path_facts(&encoder.path_facts.borrow()), encoded_facts);
        assert_eq!(flatten_path_facts(&planner.path_facts.borrow()), planned_facts);
        assert!(std::rc::Rc::ptr_eq(&encoder.path_query_facts(&stops), &encoder.path_query_facts(&stops)));
        assert!(std::rc::Rc::ptr_eq(&planner.path_query_facts(&stops), &planner.path_query_facts(&stops)));
        assert!(!std::rc::Rc::ptr_eq(&encoder.path_query_facts(&stops), &planner.path_query_facts(&stops)));
        // Equal stop membership is not equal memo identity: duplicate keys
        // remain distinct, and the same suffix is still traversed under each.
        let distinct = vec![end];
        assert_eq!(encoder.all_paths_reach_or_terminate(start, &distinct, &mut BTreeSet::new()),
            old_paths(encoder, start, &distinct, &mut BTreeSet::new(), &mut encoded_facts, &mut encoded_visits));
        assert_eq!(planner.all_paths_reach_or_terminate(start, &distinct, &mut BTreeSet::new()),
            old_paths(planner, start, &distinct, &mut BTreeSet::new(), &mut planned_facts, &mut planned_visits));
        assert_eq!(encoder.path_facts.borrow().len(), 2);
        assert_eq!(planner.path_facts.borrow().len(), 2);
        assert!(!std::rc::Rc::ptr_eq(&encoder.path_query_facts(&stops), &encoder.path_query_facts(&distinct)));
        assert!(!std::rc::Rc::ptr_eq(&planner.path_query_facts(&stops), &planner.path_query_facts(&distinct)));
        assert_eq!(flatten_path_facts(&encoder.path_facts.borrow()), encoded_facts);
        assert_eq!(flatten_path_facts(&planner.path_facts.borrow()), planned_facts);
    });
}

fn flatten_path_facts(
    groups: &BTreeMap<Vec<BlockId>, std::rc::Rc<RefCell<BTreeMap<BlockId, bool>>>>,
) -> BTreeMap<(BlockId, Vec<BlockId>), bool> {
    groups.iter().flat_map(|(stops, nodes)| {
        nodes.borrow().iter().map(|(block, result)| ((*block, stops.clone()), *result)).collect::<Vec<_>>()
    }).collect()
}

#[test]
fn exact_paths_preserve_descendant_refusals_and_positive_memo_population() {
    with_graph(&[&[1], &[2], &[3], &[]], |encoder, planner| {
        let mut encoded_facts = BTreeMap::new();
        let mut planned_facts = BTreeMap::new();
        let mut visits = 0;
        for (raw_start, raw_stops, raw_active, expected) in [
            (0, vec![3], vec![], true),
            (1, vec![3, 99], vec![2], false),
            (0, vec![3, 99], vec![], false),
            // This successful query must populate the exact node memo, not
            // merely return a positive answer from a different stop key.
            (0, vec![2, 3], vec![], true),
            // The old memo is checked before active insertion, so that same
            // exact success must survive even with the root already active.
            (0, vec![3, 2], vec![0], true),
        ] {
            let start = BlockId::new(raw_start);
            let stops: Vec<_> = raw_stops.into_iter().map(BlockId::new).collect();
            let initial: BTreeSet<_> = raw_active.into_iter().map(BlockId::new).collect();
            let mut old_active = initial.clone();
            let mut new_active = initial.clone();
            let old = old_paths(encoder, start, &stops, &mut old_active, &mut encoded_facts, &mut visits);
            assert_eq!(old, Ok(expected));
            assert_eq!(encoder.all_paths_reach_or_terminate(start, &stops, &mut new_active), old);
            assert_eq!(new_active, old_active);
            assert_eq!(flatten_path_facts(&encoder.path_facts.borrow()), encoded_facts);
            let mut old_active = initial.clone();
            let mut new_active = initial;
            let old = old_paths(planner, start, &stops, &mut old_active, &mut planned_facts, &mut visits);
            assert_eq!(old, Ok(expected));
            assert_eq!(planner.all_paths_reach_or_terminate(start, &stops, &mut new_active), old);
            assert_eq!(new_active, old_active);
            assert_eq!(flatten_path_facts(&planner.path_facts.borrow()), planned_facts);
        }
    });
}
