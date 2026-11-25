use super::{Body, I64, InstBuilder, LANE_BYTES, Result, Value, error, storage::Runtime};
use crate::compiler::logical_body::{Expr, Ty, Value as Logical};
use cranelift_codegen::ir::{AbiParam, MemFlagsData, StackSlotData, StackSlotKind};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Module};

impl Body<'_, '_> {
    /// Adapt the runtime's single input pointer to the selected function's ABI.
    fn task_entry(&mut self, function: FuncId, lanes: u32) -> Result<Value> {
        let mut context = self.module.make_context();
        context.func.signature.params.push(AbiParam::new(I64));
        let id = self
            .module
            .declare_anonymous_function(&context.func.signature)
            .map_err(error)?;
        let mut builder_context = FunctionBuilderContext::new();
        let target = self
            .module
            .declare_func_in_func(function, &mut context.func);
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let input = builder.block_params(entry)[0];
        let arguments = (0..lanes)
            .map(|lane| {
                builder
                    .ins()
                    .load(I64, MemFlagsData::new(), input, (lane * LANE_BYTES) as i32)
            })
            .collect::<Vec<_>>();
        builder.ins().call(target, &arguments);
        builder.ins().return_(&[]);
        builder.finalize(self.module.isa().frontend_config());
        self.module
            .define_function(id, &mut context)
            .map_err(error)?;
        let reference = self.module.declare_func_in_func(id, self.fb.func);
        Ok(self.fb.ins().func_addr(I64, reference))
    }

    pub(super) fn spawn_task(
        &mut self,
        result: Ty,
        allocator: &Expr,
        bytes: &Expr,
        entry: &Expr,
    ) -> Result<Vec<Value>> {
        let Logical::Call(call) = &entry.kind else {
            return Err(error("task entry must be a direct call"));
        };
        let [input] = call.args.as_slice() else {
            return Err(error("task entry must take one input"));
        };
        let instance = call
            .instance
            .ok_or_else(|| error("unresolved task entry"))?;
        let function = &self.functions[instance as usize];
        if function.result != Ty::Unit {
            return Err(error("task entry must return unit"));
        }
        let function = function.id;
        let grant = self.handle(allocator)?;
        let bytes = self.one(bytes)?;
        let values = self.abi_lanes(input)?;
        let lanes = values.len() as u32;
        let count = self.constant(u64::from(lanes));
        let address = if lanes == 0 {
            self.constant(0)
        } else {
            let size = self.charge_frame(lanes)?;
            let slot = self.fb.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                size,
                3,
            ));
            for (i, value) in values.iter().enumerate() {
                self.fb
                    .ins()
                    .stack_store(I64, *value, slot, (i as u32 * LANE_BYTES) as i32);
            }
            self.fb.ins().stack_addr(I64, slot, 0)
        };
        let entry = self.task_entry(function, lanes)?;
        let handle =
            self.runtime_value(Runtime::TaskSpawn, &[grant, bytes, entry, address, count])?;
        let failed = self.fb.ins().icmp_imm_s(super::IntCC::Equal, handle, 0);
        self.result(result, failed, |_| Ok(vec![handle]), |_| Ok(values))
    }

    pub(super) fn step_task(&mut self, result: Ty, task: &Expr, word: &Expr) -> Result<Vec<Value>> {
        let word = self.one(word)?;
        let handle = self.handle(task)?;
        let token = self.runtime_value(Runtime::TaskStep, &[handle, word])?;
        let status = self.runtime_value(Runtime::TaskStatus, &[handle])?;
        let paused = self.fb.ins().icmp_imm_s(super::IntCC::Equal, status, 1);
        self.result(result, paused, |_| Ok(vec![]), |_| Ok(vec![token]))
    }
}
