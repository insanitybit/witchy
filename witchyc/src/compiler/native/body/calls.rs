use super::{Body, InstBuilder, Result, Value, error};
use crate::compiler::logical_body::{Expr, InstanceCall, Value as Logical};
use cranelift_module::Module;

impl Body<'_, '_> {
    pub(super) fn has_plain_arguments(&self, call: &InstanceCall) -> bool {
        call.args
            .iter()
            .all(|arg| self.layout.is_plain_data(arg.ty))
    }

    pub(super) fn emit_call(
        &mut self,
        call: &InstanceCall,
        destination: Option<Value>,
    ) -> Result<Vec<Value>> {
        let values = self.emit_call_carriers(call, destination)?;
        if values.len() == 1 && self.fb.func.dfg.value_type(values[0]) == super::F64X2 {
            return Ok(self.float_pair_words(values[0]));
        }
        Ok(values)
    }

    pub(super) fn emit_call_carriers(
        &mut self,
        call: &InstanceCall,
        destination: Option<Value>,
    ) -> Result<Vec<Value>> {
        let instance = call
            .instance
            .ok_or_else(|| error("unresolved function instance"))?;
        let callee = &self.functions[instance as usize];
        let reference = self.module.declare_func_in_func(callee.id, self.fb.func);
        let result_lanes = callee.output_lanes;
        let result_ty = callee.result;
        let mut args = Vec::new();
        let mut temporary = None;
        if let Some(addr) = destination {
            // A forwarded result still counts toward the caller's logical frame limit.
            self.charge_frame(result_lanes)?;
            args.push(addr);
        } else if callee.indirect_result() {
            let addr = self.scratch(result_ty)?;
            args.push(addr);
            temporary = Some(addr);
        }
        for arg in &call.args {
            if self.layout.float_pair(arg.ty)? {
                args.push(self.float_pair_expr(arg)?);
            } else {
                args.extend(self.abi_lanes(arg)?);
            }
        }
        // Each nested argument has already applied its own write barrier.
        if !self.has_plain_arguments(call) {
            self.reads.clear();
        }
        let inst = self.fb.ins().call(reference, &args);
        Ok(match temporary {
            Some(addr) => self.load_typed(addr, result_ty)?,
            None => self.fb.inst_results(inst).to_vec(),
        })
    }

    pub(super) fn emit_return(&mut self, expr: &Expr) -> Result<()> {
        if self.abi.float_pair_result {
            let vector = self.float_pair_expr(expr)?;
            self.fb.ins().return_(&[vector]);
            return Ok(());
        }
        let Some(slot) = self.return_slot else {
            let lanes = self.abi_lanes(expr)?;
            self.fb.ins().return_(&lanes);
            return Ok(());
        };

        // A call in return position can fill our caller's result slot directly.
        if let Logical::Call(call) = &expr.kind {
            self.emit_call(call, Some(slot))?;
        } else {
            let lanes = self.expr(expr)?;
            self.store_typed(slot, expr.ty, &lanes)?;
        }
        self.fb.ins().return_(&[]);
        Ok(())
    }
}
