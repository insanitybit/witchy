//! Emit acyclic CFGs once, including shared forward continuations.
use super::*;

impl StructuredEmitter<'_> {
    pub(super) fn emit_forward(&mut self, order: &[crate::canonical_safewir::BlockId]) -> Result<(), EncodeError> {
        let positions: BTreeMap<_, _> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        for _ in order.iter().skip(1) {
            self.body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
        }
        for (position, id) in order.iter().enumerate() {
            if position > 0 { self.body.instruction(&wasm_encoder::Instruction::End); }
            self.emitted.insert(*id);
            let block = self.block(*id)?.clone();
            for instruction in &block.instructions {
                emit_instruction(self.body, self.source_function, self.layout,
                    &instruction.operation, self.function_indices, self.signature_indices,
                    self.import_indices, self.imports, self.static_data, self.layouts,
                    self.owner_cleanup_function)?;
            }
            let depth = |target, conditional| {
                positions.get(&target).and_then(|target| forward_cfg::depth(position, *target, conditional))
                    .ok_or(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                        function: self.source_function.id, operation: "non-forward scheduled edge",
                    }))
            };
            match &block.terminator.operation {
                ScalarTerminator::Branch { target, arguments } => {
                    let depth = depth(*target, false)?;
                    self.emit_branch_arguments(*target, arguments)?;
                    self.body.instruction(&wasm_encoder::Instruction::Br(depth));
                }
                ScalarTerminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments } => {
                    let then_depth = depth(*then_target, true)?;
                    let else_depth = depth(*else_target, true)?;
                    emit_get(self.body, self.layout, self.source_function.id, *condition)?;
                    self.body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
                    self.emit_branch_arguments(*then_target, then_arguments)?;
                    self.body.instruction(&wasm_encoder::Instruction::Br(then_depth));
                    self.body.instruction(&wasm_encoder::Instruction::Else);
                    self.emit_branch_arguments(*else_target, else_arguments)?;
                    self.body.instruction(&wasm_encoder::Instruction::Br(else_depth));
                    self.body.instruction(&wasm_encoder::Instruction::End);
                }
                terminator => emit_terminator(self.body, self.source_function, self.layout,
                    terminator, self.function_indices, self.signature_indices)?,
            }
        }
        Ok(())
    }
}
