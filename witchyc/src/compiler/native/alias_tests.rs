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

#[test]
fn a_lane_joined_with_a_constant_filler_is_its_address() {
    // The `Ok` arm passes the element's address, the `Err` arm a constant; the load is on `Ok`.
    let (mut function, entry, heap) = function(1);
    let param = function.dfg.block_params(entry)[0];
    let (ok, err, join, read, done) = (function.dfg.make_block(), function.dfg.make_block(), function.dfg.make_block(), function.dfg.make_block(), function.dfg.make_block());
    for block in [ok, err, join, read, done] {
        function.layout.append_block(block);
    }
    let tag = function.dfg.append_block_param(join, I64);
    let lane = function.dfg.append_block_param(join, I64);
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
    pos.ins().brif(tag, done, &[], read, &[]);
    pos.goto_bottom(read);
    let value = pos.ins().load(I64, MemFlagsData::new(), lane, 0);
    pos.ins().return_(&[value]);
    pos.goto_bottom(done);
    pos.ins().return_(&[one]);
    assert!(annotate(&mut function, &[(param, root(false))]).is_ok());
    assert_eq!(loads(&function).0, loads(&function).1);
}

#[test]
fn a_loop_offset_that_starts_at_a_constant_is_not_a_filler() {
    // `off` starts at 0 and grows by 8 each time round: it is not the address it is added to.
    // From the review: the filler rule took it for one and the address walk never ended.
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
fn a_filler_is_small_and_nothing_large_is_added_past_it() {
    for (filler, past, described) in [(1, 8, true), (4096, 8, false), (1, 4096, false)] {
        let (mut function, param) = diamond(
            |pos, param, _| (pos.ins().iconst(I64, filler), param),
            |pos, _, lane, _| pos.ins().load(I64, MemFlagsData::new(), lane, past as i32),
        );
        assert_eq!(annotate(&mut function, &[(param, root(true))]).is_ok(), described, "filler {filler}, {past} past it");
    }
}

#[test]
fn a_handle_never_comes_through_a_filler() {
    // A constant handle names a real slot: no filler is passed over on the way to one.
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
