//! Forward joins are admitted from the source plan, not trusted encoder output.
use super::*;

fn candidate(wrong_arity: bool, bad_dominance: bool, same_target: bool) -> CanonicalModule<LoweredCandidate> {
    let [first, second, x, y, left, right, ten, product, result] = [1,2,3,4,5,6,7,8,9].map(ValueId::new);
    let parameter = |value, value_type| BlockParameter { value, value_type, ownership: None };
    let branch = |target, arguments| Terminator::Branch { target: BlockId::new(target), arguments };
    let choose = |condition, yes, no| Terminator::BranchIf { condition,
        then_target: BlockId::new(yes), then_arguments: vec![],
        else_target: BlockId::new(no), else_arguments: vec![] };
    let mut fallback = vec![y, if bad_dominance { result } else { x }];
    if wrong_arity { fallback.pop(); }
    let mut blocks = vec![
        BasicBlock { id: BlockId::new(0), parameters: vec![parameter(first,ValueType::Bool), parameter(second,ValueType::Bool),
            parameter(x,ValueType::Int), parameter(y,ValueType::Int)], operations: vec![], terminator: choose(first,1,2) },
        BasicBlock { id: BlockId::new(1), parameters: vec![], operations: vec![], terminator: choose(second,3,2) },
        BasicBlock { id: BlockId::new(2), parameters: vec![], operations: vec![], terminator: branch(4,fallback) },
        BasicBlock { id: BlockId::new(3), parameters: vec![], operations: vec![], terminator: branch(4,vec![x,y]) },
        BasicBlock { id: BlockId::new(4), parameters: vec![parameter(left,ValueType::Int),parameter(right,ValueType::Int)],
            operations: vec![Operation::ConstInt { destination: ten,value:10 },
                Operation::Arithmetic { destination: product, operator: ArithmeticOperator::Multiply,left,right:ten,overflow:OverflowBehavior::Wrap },
                Operation::Arithmetic { destination: result, operator: ArithmeticOperator::Add,left:product,right,overflow:OverflowBehavior::Wrap }],
            terminator: Terminator::Return { values: vec![result] } },
    ];
    if same_target {
        blocks[0].terminator = Terminator::BranchIf { condition: first,
            then_target: BlockId::new(4), then_arguments: vec![x,y],
            else_target: BlockId::new(4), else_arguments: vec![y,x] };
        blocks.retain(|block| matches!(block.id.get(),0|4));
    }
    CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(301), checked_source: CheckedSourceIdentity::from_digest([31;32]),
        layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(), imports: vec![], proper_tail_sites: vec![],
        signatures: vec![scalar_signature_with(FunctionSignatureId::new(301),
            vec![ValueType::Bool, ValueType::Bool, ValueType::Int, ValueType::Int], vec![ValueType::Int])],
        functions: vec![Function {
            id: FunctionId::new(301), signature: FunctionSignatureId::new(301), entry: BlockId::new(0),
            value_types: [(first,ValueType::Bool),(second,ValueType::Bool),(x,ValueType::Int),(y,ValueType::Int),
                (left,ValueType::Int),(right,ValueType::Int),(ten,ValueType::Int),(product,ValueType::Int),(result,ValueType::Int)].into_iter().collect(),
            blocks,
        }],
    })
}

pub fn source_edges_and_forward_schedule_remain_checked() {
    let arity = check_initial_with_test_witness(candidate(true,false,false)).err().expect("wrong edge arity must refuse");
    assert!(format!("{arity:?}").contains("Arity"), "{arity:?}");
    let dominance = check_initial_with_test_witness(candidate(false,true,false)).err().expect("forward value use must refuse");
    assert!(format!("{dominance:?}").contains("ValueDoesNotDominateUse"), "{dominance:?}");
    let b = BlockId::new;
    let graph = BTreeMap::from([(b(0),vec![b(2),b(1)]),(b(1),vec![b(2)]),(b(2),vec![])]);
    assert_eq!(super::super::forward_cfg::schedule(b(0),&graph), Ok(Some(vec![b(0),b(1),b(2)])));
    assert_eq!(super::super::forward_cfg::region(b(0),&graph,&[b(2)]), Ok(Some(vec![b(0),b(1)])));
    let mut missing = graph.clone(); missing.get_mut(&b(1)).unwrap().push(b(7));
    assert_eq!(super::super::forward_cfg::schedule(b(0),&missing), Err(()));
    assert_eq!(super::super::forward_cfg::region(b(0),&missing,&[b(7)]), Err(()));
    let mut unreachable = graph.clone(); unreachable.insert(b(7),vec![]);
    assert_eq!(super::super::forward_cfg::schedule(b(0),&unreachable), Err(()));
    let mut cyclic = graph; cyclic.get_mut(&b(2)).unwrap().push(b(1));
    assert_eq!(super::super::forward_cfg::schedule(b(0),&cyclic), Ok(None));
    assert_eq!(super::super::forward_cfg::region(b(0),&cyclic,&[]), Ok(None));
    assert_eq!(super::super::forward_cfg::region(b(0),&cyclic,&[b(2)]), Ok(Some(vec![b(0),b(1)])));
}

#[cfg(feature = "native")]
pub fn shared_forward_join_executes_and_mutated_edge_refuses() {
    let validated = || finalize(candidate(false,false,false));
    let artifact = admit_canonical_for_test(validated()).expect("shared forward join admits");
    let engine = crate::canonical_wasmir::fixture_engine();
    let module = wasmtime::Module::new(&engine,artifact.wasm()).unwrap();
    let mut store = wasmtime::Store::new(&engine,());
    let instance = wasmtime::Instance::new(&mut store,&module,&[]).unwrap();
    let run = instance.get_typed_func::<(i32,i32,i64,i64),i64>(&mut store,"f301").unwrap();
    for (first,second,expected) in [(1,1,37),(1,0,73),(0,1,73),(0,0,73)] {
        assert_eq!(run.call(&mut store,(first,second,3,7)).unwrap(),expected);
    }
    let same = admit_canonical_for_test(finalize(candidate(false,false,true))).expect("same-target edge arguments admit");
    let same = wasmtime::Module::new(&engine,same.wasm()).unwrap();
    let same = wasmtime::Instance::new(&mut store,&same,&[]).unwrap();
    let same = same.get_typed_func::<(i32,i32,i64,i64),i64>(&mut store,"f301").unwrap();
    assert_eq!(same.call(&mut store,(1,0,3,7)).unwrap(),37);
    assert_eq!(same.call(&mut store,(0,1,3,7)).unwrap(),73);
    // Keep the mutation valid Wasm while redirecting one forward edge. The
    // independent source plan must reject it, not merely validate its types.
    let mut bytes = artifact.wasm().to_vec();
    let mut branch_offset = None;
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        if let wasmparser::Payload::CodeSectionEntry(body) = payload.unwrap() {
            let mut reader = body.get_operators_reader().unwrap();
            while !reader.eof() {
                let offset = reader.original_position();
                if let wasmparser::Operator::Br { relative_depth } = reader.read().unwrap()
                    && relative_depth > 1 && relative_depth < 64
                { branch_offset = Some((offset,relative_depth)); break; }
            }
            if branch_offset.is_some() { break; }
        }
    }
    let (offset,depth) = branch_offset.expect("forward conditional edge");
    bytes[offset+1] = (depth-1) as u8;
    wasmparser::validate(&bytes).expect("redirected edge remains structurally valid Wasm");
    assert!(admit_canonical_with_bytes_for_test(validated(),bytes).is_err());
}
