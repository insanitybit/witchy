use cranelift_codegen::{flowgraph::ControlFlowGraph, ir};
use std::collections::{BTreeMap, BTreeSet};

// Inlining can expose both sides of an aggregate copy. Along a linear block sequence, forward
// an exact stack load from an earlier store. Clear the tracked stores whenever
// an instruction can write through an address we cannot resolve.
pub(super) fn forward_stack_loads(function: &mut ir::Function) {
    use ir::{InstructionData, MemFlagsData, Opcode};

    let cfg = ControlFlowGraph::with_function(function);
    let stack = &function.stencil.sized_stack_slots;
    let layout = &mut function.stencil.layout;
    let dfg = &mut function.stencil.dfg;
    let mut stores = BTreeMap::<(ir::StackSlot, u32), (ir::Value, u32)>::new();
    let mut block = layout.entry_block();
    let mut previous = None;
    while let Some(current) = block {
        block = layout.next_block(current);
        let mut predecessors = cfg.pred_iter(current);
        let linear = previous.is_some()
            && predecessors.next().map(|p| p.block) == previous
            && predecessors.next().is_none();
        if !linear {
            stores.clear();
        }
        previous = Some(current);
        let mut instruction = layout.first_inst(current);
        while let Some(inst) = instruction {
            instruction = layout.next_inst(inst);
            let data = dfg.insts[inst];
            let access = match data {
                InstructionData::Load {
                    opcode: Opcode::Load,
                    arg,
                    offset,
                    flags,
                } if dfg.mem_flags[flags] == MemFlagsData::new() => {
                    Some((arg, i32::from(offset), dfg.first_result(inst), false))
                }
                InstructionData::Store {
                    opcode: Opcode::Store,
                    args,
                    offset,
                    flags,
                } if dfg.mem_flags[flags] == MemFlagsData::new() => {
                    Some((args[1], i32::from(offset), args[0], true))
                }
                _ => None,
            };
            let resolved = access.and_then(|(address, displacement, value, store)| {
                let (stack_slot, address_offset) = stack_address(dfg, address)?;
                let start =
                    u32::try_from(address_offset.checked_add(i64::from(displacement))?).ok()?;
                let end = start.checked_add(dfg.value_type(value).bytes())?;
                if end > stack[stack_slot].size {
                    return None;
                }
                Some((stack_slot, start, end, value, store))
            });
            if let Some((slot, start, end, value, is_store)) = resolved {
                if is_store {
                    stores.retain(|&(other_slot, other_start), &mut (_, other_end)| {
                        slot != other_slot || end <= other_start || other_end <= start
                    });
                    if stores.len() == 16 {
                        stores.clear();
                    }
                    stores.insert((slot, start), (value, end));
                } else if let Some(&(stored, stored_end)) = stores.get(&(slot, start))
                    && end == stored_end
                    && dfg.value_type(value) == dfg.value_type(stored)
                {
                    dfg.clear_results(inst);
                    dfg.change_to_alias(value, stored);
                    layout.remove_inst(inst);
                }
            } else {
                let opcode = data.opcode();
                if opcode.can_store() || opcode.is_call() || opcode.other_side_effects() {
                    stores.clear();
                }
            }
        }
    }
}

pub(super) fn stack_address(
    dfg: &ir::DataFlowGraph,
    mut base: ir::Value,
) -> Option<(ir::StackSlot, i64)> {
    use ir::{InstructionData, Opcode, ValueDef};

    let mut displacement = 0_i64;
    // Stack places normally have one constant index. Bound the amount of
    // address analysis per operand even for deeply nested source places.
    for _ in 0..8 {
        let ValueDef::Result(inst, _) = dfg.value_def(dfg.resolve_aliases(base)) else {
            return None;
        };
        match dfg.insts[inst] {
            InstructionData::StackAddr {
                opcode: Opcode::StackAddr,
                stack_slot,
                offset,
            } => {
                return Some((
                    stack_slot,
                    displacement.checked_add(i64::from(i32::from(offset)))?,
                ));
            }
            InstructionData::Binary {
                opcode: Opcode::Iadd,
                args,
            } => {
                let ValueDef::Result(constant, _) = dfg.value_def(dfg.resolve_aliases(args[1]))
                else {
                    return None;
                };
                let InstructionData::UnaryImm {
                    opcode: Opcode::Iconst,
                    imm,
                } = dfg.insts[constant]
                else {
                    return None;
                };
                displacement = displacement.checked_add(i64::from(imm))?;
                base = args[0];
            }
            _ => return None,
        }
    }
    None
}

// A stack temporary with no reads and no escaping addresses is private to this
// function. Drop only ordinary stores wholly within such a slot.
pub(super) fn remove_unobserved_stack_stores(function: &mut ir::Function) {
    use ir::{InstructionData, MemFlagsData, Opcode};

    let stencil = &mut function.stencil;
    let mut observed = BTreeSet::new();
    let mut stores = Vec::new();
    for block in stencil.layout.blocks() {
        for inst in stencil.layout.block_insts(block) {
            let data = stencil.dfg.insts[inst];
            // Any instruction directly naming a slot other than stack_addr
            // may observe it without going through an SSA address.
            if let InstructionData::StackAddr {
                opcode, stack_slot, ..
            } = data
                && opcode != Opcode::StackAddr
            {
                observed.insert(stack_slot);
            }
            for (index, argument) in stencil.dfg.inst_values(inst).enumerate() {
                let Some((slot, base)) = stack_address(&stencil.dfg, argument) else {
                    continue;
                };
                match data {
                    InstructionData::Binary {
                        opcode: Opcode::Iadd,
                        ..
                    } if index == 0
                        && stack_address(&stencil.dfg, stencil.dfg.first_result(inst))
                            .is_some() => {}
                    InstructionData::Store {
                        opcode: Opcode::Store,
                        args,
                        offset,
                        flags,
                    } if index == 1
                        && stencil.dfg.mem_flags[flags] == MemFlagsData::new()
                        && base
                            .checked_add(i64::from(i32::from(offset)))
                            .and_then(|start| u32::try_from(start).ok())
                            .and_then(|start| {
                                start.checked_add(stencil.dfg.value_type(args[0]).bytes())
                            })
                            .is_some_and(|end| end <= stencil.sized_stack_slots[slot].size) =>
                    {
                        stores.push((inst, slot));
                    }
                    _ => {
                        observed.insert(slot);
                    }
                }
            }
        }
    }
    for (inst, slot) in stores {
        if !observed.contains(&slot) {
            stencil.layout.remove_inst(inst);
        }
    }
}
