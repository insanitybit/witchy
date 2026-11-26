//! `annotate` on small CLIF functions shaped as lowering writes them, then Cranelift's mid-end:
//! regions where the loan rule describes every access, a load of one parameter's buffer kept
//! across a store to another's, a read after a write through the same storage kept, and no
//! regions at all where any access is not described.
use super::*;
use cranelift_codegen::cursor::{Cursor, FuncCursor};
use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlagsData, Signature, UserExternalName, UserFuncName, types::I64};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::{Context, settings, settings::Configurable};

fn root(mutable: bool) -> Root {
    Root { mutable, owners: [0].into() }
}

/// A function of `params` pointer parameters and a heap symbol.
fn function(params: usize) -> (ir::Function, ir::Block, ir::GlobalValue) {
    let mut signature = Signature::new(CallConv::SystemV);
    signature.params.resize(params, AbiParam::new(I64));
    signature.returns.push(AbiParam::new(I64));
    let mut function = ir::Function::with_name_signature(UserFuncName::user(0, 0), signature);
    let name = function.declare_imported_user_function(UserExternalName::new(1, 0));
    let heap = function.create_global_value(GlobalValueData::Symbol { name: ExternalName::User(name), offset: 0.into(), colocated: false, tls: false });
    let block = function.dfg.make_block();
    function.layout.append_block(block);
    for _ in 0..params {
        function.dfg.append_block_param(block, I64);
    }
    (function, block, heap)
}

/// The buffer pointer for the owner field at `record + 0`, as `live_inline` finds it.
fn buffer(pos: &mut FuncCursor, record: Value, heap: ir::GlobalValue) -> Value {
    let flags = MemFlagsData::new();
    let handle = pos.ins().load(I64, flags, record, 0);
    let mask = pos.ins().iconst(I64, 0xffff_ffff);
    let id = pos.ins().band(handle, mask);
    let one = pos.ins().iconst(I64, -1);
    let index = pos.ins().iadd(id, one);
    let table = pos.ins().symbol_value(I64, heap);
    let sixteen = pos.ins().iconst(I64, 16);
    let offset = pos.ins().imul(index, sixteen);
    let slot = pos.ins().iadd(table, offset);
    let generation = pos.ins().uload32(flags, slot, 8);
    let shift = pos.ins().iconst(I64, 32);
    let wanted = pos.ins().ushr(handle, shift);
    let stale = pos.ins().icmp(ir::condcodes::IntCC::NotEqual, generation, wanted);
    pos.ins().trapnz(stale, ir::TrapCode::unwrap_user(1));
    let buffer = pos.ins().load(I64, flags, slot, 0);
    pos.ins().trapz(buffer, ir::TrapCode::unwrap_user(1));
    buffer
}

/// The address of element `index` (a constant) of the buffer at `buffer`.
fn element(pos: &mut FuncCursor, buffer: Value, index: i64) -> Value {
    let payload = pos.ins().iconst(I64, HEADER);
    let payload = pos.ins().iadd(buffer, payload);
    let index = pos.ins().iconst(I64, index);
    let eight = pos.ins().iconst(I64, 8);
    let offset = pos.ins().imul(index, eight);
    pos.ins().iadd(payload, offset)
}

fn optimize(function: ir::Function) -> ir::Function {
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").expect("a setting");
    let isa = cranelift_codegen::isa::lookup_by_name("aarch64-apple-darwin").expect("arm64").finish(settings::Flags::new(flags)).expect("an isa");
    let mut context = Context::for_function(function);
    context.optimize(&*isa, &mut Default::default()).expect("optimizes");
    cranelift_codegen::verify_function(&context.func, isa.flags()).expect("verifies");
    context.func
}

/// The loads left, and how many carry a region.
fn loads(function: &ir::Function) -> (usize, usize) {
    let mut found = (0, 0);
    for block in function.layout.blocks() {
        for inst in function.layout.block_insts(block) {
            if function.dfg.insts[inst].opcode().can_load() {
                found.0 += 1;
                found.1 += usize::from(function.dfg.insts[inst].alias_region(&function.dfg).is_some());
            }
        }
    }
    found
}

/// Read `src[0]`, write `dst[1]`, read `src[0]` again: with `src` and `dst` the given values.
fn read_write_read(src_is: usize, dst_is: usize) -> (ir::Function, Vec<Value>) {
    let (mut function, block, heap) = function(2);
    let params = function.dfg.block_params(block).to_vec();
    let mut pos = FuncCursor::new(&mut function).at_bottom(block);
    let source = buffer(&mut pos, params[src_is], heap);
    let at = element(&mut pos, source, 0);
    let first = pos.ins().load(I64, MemFlagsData::new(), at, 0);
    let target = buffer(&mut pos, params[dst_is], heap);
    let to = element(&mut pos, target, 1);
    pos.ins().store(MemFlagsData::new(), first, to, 0);
    let again = buffer(&mut pos, params[src_is], heap);
    let at = element(&mut pos, again, 0);
    let second = pos.ins().load(I64, MemFlagsData::new(), at, 0);
    let sum = pos.ins().iadd(first, second);
    pos.ins().return_(&[sum]);
    (function, params)
}

#[test]
fn a_read_of_one_parameter_survives_a_store_to_another() {
    let (mut function, params) = read_write_read(0, 1);
    let (plain, _) = loads(&optimize(function.clone()));
    let tagged = annotate(&mut function, &[(params[0], root(false)), (params[1], root(true))]).expect("every access is described");
    assert_eq!(tagged, loads(&function).0 + 1, "every load and the store carry a region");
    let (left, with_region) = loads(&optimize(function));
    assert_eq!(left, with_region);
    // The second read of `src`: its handle, slot, pointer and element are the first read's.
    assert!(left + 4 <= plain, "{left} loads with regions, {plain} without");
}

#[test]
fn a_read_after_a_write_through_the_same_storage_stays() {
    // `dst` is `src`: one `&mut` parameter, reached twice (as through a reborrow handed to an
    // inlined callee). The store must stay between the two reads.
    let (mut function, params) = read_write_read(1, 1);
    annotate(&mut function, &[(params[0], root(false)), (params[1], root(true))]).expect("every access is described");
    let optimized = optimize(function);
    let mut payload_loads = 0;
    let mut stored = false;
    for block in optimized.layout.blocks() {
        for inst in optimized.layout.block_insts(block) {
            let data = &optimized.dfg.insts[inst];
            stored |= data.opcode().can_store();
            // Element loads are after the store too: the second read is a load, not the first.
            if stored && data.opcode() == Opcode::Load {
                payload_loads += 1;
            }
        }
    }
    assert!(stored && payload_loads >= 1, "the read after the store was taken from before it");
}

#[test]
fn two_unique_parameters_get_regions_of_their_own() {
    let (mut function, params) = read_write_read(0, 1);
    annotate(&mut function, &[(params[0], root(true)), (params[1], root(true))]).expect("described");
    let regions: BTreeSet<_> = function.layout.blocks().flat_map(|b| function.layout.block_insts(b)).filter_map(|i| function.dfg.insts[i].alias_region(&function.dfg)).collect();
    // The slot table, and each parameter's record, header and payload (no header is read here).
    assert_eq!(regions.len(), 5);
}

#[test]
fn an_access_the_loan_rule_does_not_describe_leaves_no_region() {
    // A handle read from the payload of another buffer: two hops from the parameter.
    let (mut function, block, heap) = function(1);
    let param = function.dfg.block_params(block)[0];
    let mut pos = FuncCursor::new(&mut function).at_bottom(block);
    let outer = buffer(&mut pos, param, heap);
    let inner_record = element(&mut pos, outer, 0);
    let inner = buffer(&mut pos, inner_record, heap);
    let at = element(&mut pos, inner, 0);
    let read = pos.ins().load(I64, MemFlagsData::new(), at, 0);
    pos.ins().return_(&[read]);
    assert!(annotate(&mut function, &[(param, root(true))]).is_err());
    assert_eq!(loads(&function).1, 0);

    // A parameter the lowering did not describe.
    let (mut function, params) = read_write_read(0, 1);
    assert!(annotate(&mut function, &[(params[0], root(false))]).is_err());
    assert_eq!(loads(&function).1, 0);

    // A store through a shared borrow.
    let (mut function, params) = read_write_read(0, 1);
    assert!(annotate(&mut function, &[(params[0], root(false)), (params[1], root(false))]).is_err());
    assert_eq!(loads(&function).1, 0);

    // A handle read from a field that is not an owner.
    let (mut function, params) = read_write_read(0, 1);
    let field = Root { mutable: true, owners: [8].into() };
    assert!(annotate(&mut function, &[(params[0], root(false)), (params[1], field)]).is_err());
    assert_eq!(loads(&function).1, 0);
}

/// How the test of a tagged join's tag reaches the read of its lane.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// `join: brif tag, done, read; read: load lane` (the `Ok` arm's tag is 0).
    Tested,
    /// The address is computed in `join`, before the test, and read after it.
    ComputedBeforeTest,
    /// The load is in `join` itself, before the test.
    ReadBeforeTest,
    /// `join: brif (tag == 1), read, done`: the test lets the `Err` arm's tag through.
    WrongTag,
    /// `join: brif tag, aside, read; aside: jump merge; read: jump merge; merge: load lane`: the
    /// load is reached from `join` through `aside` too, where the tag was not 0.
    TestedOnOnePath,
    /// `join: brif tag, done, read; read: jump next(lane); next(hop): load hop`.
    PassedOn,
}

/// `entry: brif param, ok, err; ok: jump join(0, element); err: jump join(1, 1)`, then `shape`.
fn tagged(shape: Shape) -> (ir::Function, Value) {
    let (mut function, entry, heap) = function(1);
    let param = function.dfg.block_params(entry)[0];
    let [ok, err, join, read, done, aside, merge] = std::array::from_fn(|_| function.dfg.make_block());
    for block in [ok, err, join, read, done, aside, merge] {
        function.layout.append_block(block);
    }
    let tag = function.dfg.append_block_param(join, I64);
    let lane = function.dfg.append_block_param(join, I64);
    let hop = function.dfg.append_block_param(merge, I64);
    let mut pos = FuncCursor::new(&mut function).at_bottom(entry);
    pos.ins().brif(param, ok, &[], err, &[]);
    pos.goto_bottom(ok);
    let source = buffer(&mut pos, param, heap);
    let at = element(&mut pos, source, 2);
    let zero = pos.ins().iconst(I64, 0);
    pos.ins().jump(join, &[zero.into(), at.into()]);
    pos.goto_bottom(err);
    let one = pos.ins().iconst(I64, 1);
    pos.ins().jump(join, &[one.into(), one.into()]);
    pos.goto_bottom(join);
    let mut early = None;
    let mut address = lane;
    match shape {
        Shape::ComputedBeforeTest => address = pos.ins().iadd_imm(lane, 0),
        Shape::ReadBeforeTest => early = Some(pos.ins().load(I64, MemFlagsData::new(), lane, 0)),
        _ => {}
    }
    match shape {
        Shape::WrongTag => {
            let is_err = pos.ins().icmp_imm(ir::condcodes::IntCC::Equal, tag, 1);
            pos.ins().brif(is_err, read, &[], done, &[]);
        }
        Shape::TestedOnOnePath => {
            pos.ins().brif(tag, aside, &[], read, &[]);
        }
        _ => {
            pos.ins().brif(tag, done, &[], read, &[]);
        }
    }
    pos.goto_bottom(read);
    match shape {
        Shape::TestedOnOnePath => {
            pos.ins().jump(merge, &[one.into()]);
            pos.goto_bottom(aside);
            pos.ins().jump(merge, &[one.into()]);
            pos.goto_bottom(merge);
            let value = pos.ins().load(I64, MemFlagsData::new(), lane, 0);
            pos.ins().return_(&[value]);
        }
        Shape::PassedOn => {
            pos.ins().jump(merge, &[lane.into()]);
            pos.goto_bottom(merge);
            let value = pos.ins().load(I64, MemFlagsData::new(), hop, 0);
            pos.ins().return_(&[value]);
            pos.goto_bottom(aside);
            pos.ins().return_(&[one]);
        }
        _ => {
            let value = early.unwrap_or_else(|| pos.ins().load(I64, MemFlagsData::new(), address, 0));
            pos.ins().return_(&[value]);
            pos.goto_bottom(aside);
            pos.ins().return_(&[one]);
            pos.goto_bottom(merge);
            pos.ins().return_(&[one]);
        }
    }
    pos.goto_bottom(done);
    pos.ins().return_(&[one]);
    (function, param)
}

#[test]
fn a_lane_is_its_address_after_a_test_that_excludes_the_other_arm() {
    let (mut function, param) = tagged(Shape::Tested);
    assert!(annotate(&mut function, &[(param, root(false))]).is_ok());
    assert_eq!(loads(&function).0, loads(&function).1);
}

#[test]
fn a_lane_used_where_its_tag_is_not_known_is_no_address() {
    // Each shape reaches a use of the lane on a path where the `Err` arm may have given it.
    let mut described = Vec::new();
    for shape in [Shape::ComputedBeforeTest, Shape::ReadBeforeTest, Shape::WrongTag, Shape::TestedOnOnePath, Shape::PassedOn] {
        let (mut function, param) = tagged(shape);
        if annotate(&mut function, &[(param, root(false))]).is_ok() || loads(&function).1 != 0 {
            described.push(shape);
        }
    }
    assert!(described.is_empty(), "described: {described:?}");
}

#[test]
fn a_loop_offset_that_starts_at_a_constant_is_an_index() {
    // `off` starts at 0 and grows by 8 each time round: it is not the address it is added to.
    // From the review: an earlier rule took the start for a filler and the walk never ended.
    let (mut function, entry, _) = function(1);
    let param = function.dfg.block_params(entry)[0];
    let (head, exit) = (function.dfg.make_block(), function.dfg.make_block());
    function.layout.append_block(head);
    function.layout.append_block(exit);
    let off = function.dfg.append_block_param(head, I64);
    let mut pos = FuncCursor::new(&mut function).at_bottom(entry);
    let zero = pos.ins().iconst(I64, 0);
    pos.ins().jump(head, &[zero.into()]);
    pos.goto_bottom(head);
    let at = pos.ins().iadd(param, off);
    let value = pos.ins().load(I64, MemFlagsData::new(), at, 0);
    let eight = pos.ins().iconst(I64, 8);
    let next = pos.ins().iadd(off, eight);
    pos.ins().brif(value, head, &[next.into()], exit, &[]);
    pos.goto_bottom(exit);
    pos.ins().return_(&[value]);
    // Within the record, at an offset that varies: the record's region, and no endless walk.
    assert!(annotate(&mut function, &[(param, root(true))]).is_ok());
}

/// Read `place`, write the read plus one there, read it again: the second read must give the
/// written value, not the first read's.
fn write_between(header: bool) -> ir::Function {
    let (mut function, block, heap) = function(1);
    let param = function.dfg.block_params(block)[0];
    let mut pos = FuncCursor::new(&mut function).at_bottom(block);
    let (base, offset) = if header { (buffer(&mut pos, param, heap), 28) } else { (param, 8) };
    let first = pos.ins().load(I64, MemFlagsData::new(), base, offset);
    let one = pos.ins().iconst(I64, 1);
    let written = pos.ins().iadd(first, one);
    let base = if header { buffer(&mut pos, param, heap) } else { param };
    pos.ins().store(MemFlagsData::new(), written, base, offset);
    let base = if header { buffer(&mut pos, param, heap) } else { param };
    let second = pos.ins().load(I64, MemFlagsData::new(), base, offset);
    pos.ins().return_(&[second]);
    annotate(&mut function, &[(param, root(true))]).expect("described");
    optimize(function)
}

#[test]
fn a_write_to_a_record_or_a_header_is_seen_by_the_next_read() {
    for header in [false, true] {
        let optimized = write_between(header);
        let returned = optimized.layout.blocks().flat_map(|b| optimized.layout.block_insts(b)).find(|&i| optimized.dfg.insts[i].opcode() == Opcode::Return).expect("a return");
        let value = optimized.dfg.resolve_aliases(optimized.dfg.inst_args(returned)[0]);
        // Either loaded again after the store, or the stored sum itself: never the first load.
        let ir::ValueDef::Result(def, _) = optimized.dfg.value_def(value) else { panic!("a result") };
        let first_load = optimized.layout.blocks().flat_map(|b| optimized.layout.block_insts(b)).find(|&i| {
            let data = &optimized.dfg.insts[i];
            data.opcode().can_load() && matches!(*data, InstructionData::Load { offset, .. } if i32::from(offset) == if header { 28 } else { 8 })
        });
        assert_ne!(Some(def), first_load, "header {header}: the read after the write gave the value from before it");
    }
}

/// `entry: brif param, a, b; a: jump join(first); b: jump join(second); join(lane): ...`, with
/// `first` and `second` made by `arms` in the entry block, and `body` making the access.
fn diamond(arms: impl FnOnce(&mut FuncCursor, Value, ir::GlobalValue) -> (Value, Value), body: impl FnOnce(&mut FuncCursor, Value, Value, ir::GlobalValue) -> Value) -> (ir::Function, Value) {
    let (mut function, entry, heap) = function(1);
    let param = function.dfg.block_params(entry)[0];
    let (a, b, join) = (function.dfg.make_block(), function.dfg.make_block(), function.dfg.make_block());
    for block in [a, b, join] {
        function.layout.append_block(block);
    }
    let lane = function.dfg.append_block_param(join, I64);
    let mut pos = FuncCursor::new(&mut function).at_bottom(entry);
    let (first, second) = arms(&mut pos, param, heap);
    pos.ins().brif(param, a, &[], b, &[]);
    pos.goto_bottom(a);
    pos.ins().jump(join, &[first.into()]);
    pos.goto_bottom(b);
    pos.ins().jump(join, &[second.into()]);
    pos.goto_bottom(join);
    let value = body(&mut pos, param, lane, heap);
    pos.ins().return_(&[value]);
    (function, param)
}

#[test]
fn two_constants_are_not_one_offset() {
    // From the review: `entry: brif c, a, join(48); a: jump join(8); join(off): read buf + off and
    // buf + 8`. The entry dominates the join, so 48 was taken for the value and 8 for a filler:
    // `off` became 48 and one location (`buf + 8`) was under two regions.
    let (mut function, entry, heap) = function(1);
    let param = function.dfg.block_params(entry)[0];
    let (a, join) = (function.dfg.make_block(), function.dfg.make_block());
    function.layout.append_block(a);
    function.layout.append_block(join);
    let off = function.dfg.append_block_param(join, I64);
    let mut pos = FuncCursor::new(&mut function).at_bottom(entry);
    let source = buffer(&mut pos, param, heap);
    let far = pos.ins().iconst(I64, 48);
    pos.ins().brif(param, a, &[], join, &[far.into()]);
    pos.goto_bottom(a);
    let near = pos.ins().iconst(I64, 8);
    pos.ins().jump(join, &[near.into()]);
    pos.goto_bottom(join);
    let at = pos.ins().iadd(source, off);
    let first = pos.ins().load(I64, MemFlagsData::new(), at, 0);
    let second = pos.ins().load(I64, MemFlagsData::new(), source, 8);
    let sum = pos.ins().iadd(first, second);
    pos.ins().return_(&[sum]);
    assert!(annotate(&mut function, &[(param, root(true))]).is_err());
    assert_eq!(loads(&function).1, 0);
}

#[test]
fn a_lane_with_no_tag_is_never_an_address() {
    // `join(lane)` gets an address on one arm and a constant on the other, and nothing tells
    // the arms apart: whatever the constant, the access is not described. (The pass no longer
    // reasons about what an access at a small constant address would do.)
    for (filler, past) in [(1, 8), (4096, 8), (1, 4096)] {
        let (mut function, param) = diamond(
            |pos, param, heap| {
                let source = buffer(pos, param, heap);
                (pos.ins().iconst(I64, filler), element(pos, source, 0))
            },
            |pos, _, lane, _| pos.ins().load(I64, MemFlagsData::new(), lane, past as i32),
        );
        assert!(annotate(&mut function, &[(param, root(true))]).is_err(), "filler {filler}, {past} past it");
        assert_eq!(loads(&function).1, 0);
    }
}

#[test]
fn a_handle_never_comes_through_an_untagged_join() {
    // A constant on one arm and a handle on the other, and no tag: no handle is resolved.
    let (mut function, param) = diamond(
        |pos, param, _| (pos.ins().iconst(I64, 1), pos.ins().load(I64, MemFlagsData::new(), param, 0)),
        |pos, _, handle, heap| {
            let mask = pos.ins().iconst(I64, 0xffff_ffff);
            let id = pos.ins().band(handle, mask);
            let one = pos.ins().iconst(I64, -1);
            let index = pos.ins().iadd(id, one);
            let table = pos.ins().symbol_value(I64, heap);
            let sixteen = pos.ins().iconst(I64, 16);
            let offset = pos.ins().imul(index, sixteen);
            let slot = pos.ins().iadd(table, offset);
            let buffer = pos.ins().load(I64, MemFlagsData::new(), slot, 0);
            pos.ins().load(I64, MemFlagsData::new(), buffer, 28)
        },
    );
    assert!(annotate(&mut function, &[(param, root(true))]).is_err());
    assert_eq!(loads(&function).1, 0);
}

#[test]
fn an_offset_from_a_join_is_an_index_like_any_other() {
    // From the third review round: `buf + 40 + imul(lane, 8)` with `lane` an index or a constant.
    // Nothing is resolved through the join: the lane is an unknown index into the payload, as any
    // index is, and the access is the payload's whichever arm gave it.
    let (mut function, param) = diamond(
        |pos, param, _| (pos.ins().iconst(I64, 1), pos.ins().load(I64, MemFlagsData::new(), param, 8)),
        |pos, param, lane, heap| {
            let source = buffer(pos, param, heap);
            let payload = pos.ins().iconst(I64, HEADER);
            let payload = pos.ins().iadd(source, payload);
            let eight = pos.ins().iconst(I64, 8);
            let offset = pos.ins().imul(lane, eight);
            let at = pos.ins().iadd(payload, offset);
            pos.ins().load(I64, MemFlagsData::new(), at, 0)
        },
    );
    assert!(annotate(&mut function, &[(param, root(true))]).is_ok());
    let regions: BTreeSet<String> = function.layout.blocks().flat_map(|b| function.layout.block_insts(b)).filter_map(|i| function.dfg.insts[i].alias_region(&function.dfg)).map(|r| function.dfg.alias_regions[r].description.to_string()).collect();
    assert!(regions.contains("Payload(Unique(0))"), "{regions:?}");
}

#[test]
fn a_join_passed_on_to_another_stays_unresolved() {
    // From the automated review: `join(p1)` has a constant on one arm and no tag; a later
    // `next(p2)` receives only `p1`. `p2` is `p1`, which is no single value: the handle is refused.
    let (mut function, param) = diamond(
        |pos, param, _| (pos.ins().iconst(I64, 1), pos.ins().load(I64, MemFlagsData::new(), param, 0)),
        |pos, _, lane, _| lane,
    );
    // Replace the return with a hop to `next(lane)`, where the handle is used.
    let join = function.layout.blocks().last().expect("the join");
    let ret = function.layout.last_inst(join).expect("a return");
    function.layout.remove_inst(ret);
    let next = function.dfg.make_block();
    function.layout.append_block(next);
    let hop = function.dfg.append_block_param(next, I64);
    let lane = function.dfg.block_params(join)[0];
    let heap = heap_symbols(&function).into_iter().next().expect("a heap");
    let mut pos = FuncCursor::new(&mut function).at_bottom(join);
    pos.ins().jump(next, &[lane.into()]);
    pos.goto_bottom(next);
    let mask = pos.ins().iconst(I64, 0xffff_ffff);
    let id = pos.ins().band(hop, mask);
    let one = pos.ins().iconst(I64, -1);
    let index = pos.ins().iadd(id, one);
    let table = pos.ins().symbol_value(I64, heap);
    let sixteen = pos.ins().iconst(I64, 16);
    let offset = pos.ins().imul(index, sixteen);
    let slot = pos.ins().iadd(table, offset);
    let buffer = pos.ins().load(I64, MemFlagsData::new(), slot, 0);
    let read = pos.ins().load(I64, MemFlagsData::new(), buffer, 28);
    pos.ins().return_(&[read]);
    assert!(annotate(&mut function, &[(param, root(true))]).is_err());
    assert_eq!(loads(&function).1, 0);
}

#[test]
fn a_pointer_carried_around_a_loop_with_a_counter_is_its_parameter() {
    // `entry: jump head(0, param); head(i, p): read p + 8; brif .., head(i + 1, p2), exit` with
    // `p2` a parameter of a second block that receives `p`. `head` gets a constant counter on
    // the entry edge, so it looks like a tagged join while `p2` is still unresolved: `p` must be
    // found to be `param` first (from the spread loop: the whole function lost its regions).
    let (mut function, entry, _) = function(1);
    let param = function.dfg.block_params(entry)[0];
    let (head, body, exit) = (function.dfg.make_block(), function.dfg.make_block(), function.dfg.make_block());
    for block in [head, body, exit] {
        function.layout.append_block(block);
    }
    let i = function.dfg.append_block_param(head, I64);
    let p = function.dfg.append_block_param(head, I64);
    let p2 = function.dfg.append_block_param(body, I64);
    let mut pos = FuncCursor::new(&mut function).at_bottom(entry);
    let zero = pos.ins().iconst(I64, 0);
    pos.ins().jump(head, &[zero.into(), param.into()]);
    pos.goto_bottom(head);
    let value = pos.ins().load(I64, MemFlagsData::new(), p, 8);
    pos.ins().brif(value, body, &[p.into()], exit, &[]);
    pos.goto_bottom(body);
    let next = pos.ins().iadd_imm(i, 1);
    pos.ins().jump(head, &[next.into(), p2.into()]);
    pos.goto_bottom(exit);
    pos.ins().return_(&[value]);
    assert!(annotate(&mut function, &[(param, root(true))]).is_ok());
    assert_eq!(loads(&function).0, loads(&function).1);
}

#[test]
fn a_back_edge_that_retags_a_carried_lane_is_another_arm() {
    // From the review of the strict rule: `a: jump join(x, a); b: jump join(0, b);
    // join(tag, lane): brif tag == 1, then, other; other: jump join(1, lane)`. The back edge
    // carries `lane` unchanged but sets the tag to 1, so after `b, other, join` the test passes
    // with `lane` still `b`. The store through `lane` was tagged as `a`'s record and the second
    // read of `b` was taken from the first.
    let (mut function, entry, _) = function(3);
    let params = function.dfg.block_params(entry).to_vec();
    let (a, b, x) = (params[0], params[1], params[2]);
    let [arm_a, arm_b, join, then, other] = std::array::from_fn(|_| function.dfg.make_block());
    for block in [arm_a, arm_b, join, then, other] {
        function.layout.append_block(block);
    }
    let tag = function.dfg.append_block_param(join, I64);
    let lane = function.dfg.append_block_param(join, I64);
    let mut pos = FuncCursor::new(&mut function).at_bottom(entry);
    pos.ins().brif(x, arm_a, &[], arm_b, &[]);
    pos.goto_bottom(arm_a);
    pos.ins().jump(join, &[x.into(), a.into()]);
    pos.goto_bottom(arm_b);
    let zero = pos.ins().iconst(I64, 0);
    pos.ins().jump(join, &[zero.into(), b.into()]);
    pos.goto_bottom(join);
    let is_one = pos.ins().icmp_imm(ir::condcodes::IntCC::Equal, tag, 1);
    pos.ins().brif(is_one, then, &[], other, &[]);
    pos.goto_bottom(other);
    let one = pos.ins().iconst(I64, 1);
    pos.ins().jump(join, &[one.into(), lane.into()]);
    pos.goto_bottom(then);
    let first = pos.ins().load(I64, MemFlagsData::new(), b, 0);
    let seven = pos.ins().iconst(I64, 7);
    pos.ins().store(MemFlagsData::new(), seven, lane, 0);
    let second = pos.ins().load(I64, MemFlagsData::new(), b, 0);
    let sum = pos.ins().iadd(first, second);
    pos.ins().return_(&[sum]);
    assert!(annotate(&mut function, &[(a, root(true)), (b, root(true))]).is_err());
    assert_eq!(loads(&function).1, 0);
}

#[test]
fn a_back_edge_that_carries_both_tag_and_lane_keeps_them_together() {
    // As above, but the back edge passes the tag through too: the pair stays one entry's.
    let (mut function, entry, heap) = function(2);
    let params = function.dfg.block_params(entry).to_vec();
    let (param, x) = (params[0], params[1]);
    let [ok, err, join, read, other, done] = std::array::from_fn(|_| function.dfg.make_block());
    for block in [ok, err, join, read, other, done] {
        function.layout.append_block(block);
    }
    let tag = function.dfg.append_block_param(join, I64);
    let lane = function.dfg.append_block_param(join, I64);
    let mut pos = FuncCursor::new(&mut function).at_bottom(entry);
    pos.ins().brif(x, ok, &[], err, &[]);
    pos.goto_bottom(ok);
    let source = buffer(&mut pos, param, heap);
    let at = element(&mut pos, source, 2);
    let zero = pos.ins().iconst(I64, 0);
    pos.ins().jump(join, &[zero.into(), at.into()]);
    pos.goto_bottom(err);
    let one = pos.ins().iconst(I64, 1);
    pos.ins().jump(join, &[one.into(), one.into()]);
    pos.goto_bottom(join);
    pos.ins().brif(tag, done, &[], read, &[]);
    pos.goto_bottom(read);
    let value = pos.ins().load(I64, MemFlagsData::new(), lane, 0);
    pos.ins().brif(value, other, &[], done, &[]);
    pos.goto_bottom(other);
    pos.ins().jump(join, &[tag.into(), lane.into()]);
    pos.goto_bottom(done);
    pos.ins().return_(&[one]);
    assert!(annotate(&mut function, &[(param, root(false))]).is_ok());
    assert_eq!(loads(&function).0, loads(&function).1);
}

#[test]
fn a_lane_whose_value_is_another_joins_lane_is_no_address() {
    // entry(p, q): jump j2(0, 0)
    // j2(l2, t2): x = t2 + 7; jump j(p, x)
    // j(l, t): brif t2 == 1, b, c
    // c: brif t == 0, a, d      a: jump j2(l, 1)      d: jump j(q, 0)
    // b: brif t == 8, u, y      u: load q; store 7 -> l2; load q
    // Run: j2(0,0) j(p,7) c d j(q,0) c a j2(q,1) j(p,8) b u: l2 is q. From the second review of
    // the strict rule: `l2` was a tagged lane standing for `l`, and `l` was tested where `l2` is
    // used, not where `l2` copied it, so the store was tagged as `p`'s record.
    let (mut function, entry, _) = function(2);
    let params = function.dfg.block_params(entry).to_vec();
    let (p, q) = (params[0], params[1]);
    let [j2, j, a, b, c, d, u, y] = std::array::from_fn(|_| function.dfg.make_block());
    for block in [j2, j, a, b, c, d, u, y] {
        function.layout.append_block(block);
    }
    let l2 = function.dfg.append_block_param(j2, I64);
    let t2 = function.dfg.append_block_param(j2, I64);
    let l = function.dfg.append_block_param(j, I64);
    let t = function.dfg.append_block_param(j, I64);
    let mut pos = FuncCursor::new(&mut function).at_bottom(entry);
    let zero = pos.ins().iconst(I64, 0);
    pos.ins().jump(j2, &[zero.into(), zero.into()]);
    pos.goto_bottom(j2);
    let x = pos.ins().iadd_imm(t2, 7);
    pos.ins().jump(j, &[p.into(), x.into()]);
    pos.goto_bottom(j);
    let one = pos.ins().iconst(I64, 1);
    let is_one = pos.ins().icmp(ir::condcodes::IntCC::Equal, t2, one);
    pos.ins().brif(is_one, b, &[], c, &[]);
    pos.goto_bottom(c);
    let zero_c = pos.ins().iconst(I64, 0);
    let is_zero = pos.ins().icmp(ir::condcodes::IntCC::Equal, t, zero_c);
    pos.ins().brif(is_zero, a, &[], d, &[]);
    pos.goto_bottom(a);
    let one_a = pos.ins().iconst(I64, 1);
    pos.ins().jump(j2, &[l.into(), one_a.into()]);
    pos.goto_bottom(d);
    let zero_d = pos.ins().iconst(I64, 0);
    pos.ins().jump(j, &[q.into(), zero_d.into()]);
    pos.goto_bottom(b);
    let eight = pos.ins().iconst(I64, 8);
    let is_eight = pos.ins().icmp(ir::condcodes::IntCC::Equal, t, eight);
    pos.ins().brif(is_eight, u, &[], y, &[]);
    pos.goto_bottom(u);
    let first = pos.ins().load(I64, MemFlagsData::new(), q, 0);
    let seven = pos.ins().iconst(I64, 7);
    pos.ins().store(MemFlagsData::new(), seven, l2, 0);
    let second = pos.ins().load(I64, MemFlagsData::new(), q, 0);
    let sum = pos.ins().iadd(first, second);
    pos.ins().return_(&[sum]);
    pos.goto_bottom(y);
    pos.ins().return_(&[zero]);
    let outcome = annotate(&mut function, &[(p, root(true)), (q, root(true))]);
    assert!(outcome.is_err(), "the store through l2 (really q) was tagged");
    assert_eq!(loads(&function).1, 0);
}

/// Swap the owner fields at `a + 0` and `b + 0` (`memory.exchange` lowers to plain loads and stores).
fn exchange(pos: &mut FuncCursor, a: Value, b: Value) {
    let x = pos.ins().load(I64, MemFlagsData::new(), a, 0);
    let y = pos.ins().load(I64, MemFlagsData::new(), b, 0);
    pos.ins().store(MemFlagsData::new(), y, a, 0);
    pos.ins().store(MemFlagsData::new(), x, b, 0);
}

#[test]
fn a_buffer_moved_between_owners_leaves_no_region() {
    // From the third review of the strict rule: read `b.items[0]`, exchange `a.items` and
    // `b.items`, write `a.items[0]` (which is `b`'s old buffer), exchange back, read
    // `b.items[0]` again. A buffer's header and payload went to the parameter whose owner field
    // the handle was read from, as if a buffer never changed owner within a function: the write
    // was `a`'s payload, the reads `b`'s, and the second read was taken from the first.
    let (mut function, block, heap) = function(2);
    let params = function.dfg.block_params(block).to_vec();
    let (a, b) = (params[0], params[1]);
    let mut pos = FuncCursor::new(&mut function).at_bottom(block);
    let source = buffer(&mut pos, b, heap);
    let at = element(&mut pos, source, 0);
    let first = pos.ins().load(I64, MemFlagsData::new(), at, 0);
    exchange(&mut pos, a, b);
    let through_a = buffer(&mut pos, a, heap);
    let to = element(&mut pos, through_a, 0);
    let seven = pos.ins().iconst(I64, 7);
    pos.ins().store(MemFlagsData::new(), seven, to, 0);
    exchange(&mut pos, a, b);
    let again = buffer(&mut pos, b, heap);
    let at = element(&mut pos, again, 0);
    let second = pos.ins().load(I64, MemFlagsData::new(), at, 0);
    let both = pos.ins().iadd(first, second);
    pos.ins().return_(&[both]);
    assert!(annotate(&mut function, &[(a, root(true)), (b, root(true))]).is_err());
    assert_eq!(loads(&function).1, 0);
}

#[test]
fn a_store_at_a_variable_offset_into_a_record_with_no_owner_keeps_its_regions() {
    // A fixed array of numbers written at a computed index: no owner field to overwrite.
    let (mut function, block, _) = function(1);
    let param = function.dfg.block_params(block)[0];
    let mut pos = FuncCursor::new(&mut function).at_bottom(block);
    let index = pos.ins().load(I64, MemFlagsData::new(), param, 0);
    let eight = pos.ins().iconst(I64, 8);
    let offset = pos.ins().imul(index, eight);
    let at = pos.ins().iadd(param, offset);
    pos.ins().store(MemFlagsData::new(), index, at, 8);
    pos.ins().return_(&[index]);
    let numbers = Root { mutable: true, owners: BTreeSet::new() };
    assert!(annotate(&mut function.clone(), &[(param, numbers)]).is_ok());
    // With an owner field the same store could overwrite it.
    assert!(annotate(&mut function, &[(param, root(true))]).is_err());
}
