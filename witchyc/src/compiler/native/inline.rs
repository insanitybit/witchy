//! Copy finalized CLIF. Proofs, loans, and nopanic have already been checked in the
//! source definition, and its concrete instance has already selected its checks.
use super::{Result, body::MAX_FRAME_BYTES, error};
use cranelift_codegen::{
    Context,
    inline::{Inline, InlineCommand},
    ir::{self, ExternalName},
};
use cranelift_module::FuncId;
use std::{borrow::Cow, collections::BTreeMap};

mod stack;
use stack::{forward_stack_loads, remove_unobserved_stack_stores, stack_address};

const SMALL_LEAF: usize = 32;
const SMALL_WRAPPER: usize = 16;
const EXPOSURE_ALLOWANCE: usize = 16;
const CALLER_BUDGET: usize = 128;

pub(super) struct Candidates {
    bodies: BTreeMap<FuncId, Candidate>,
    selective: bool,
    enabled: bool,
}

struct Candidate {
    body: ir::Function,
    frame: u32,
    limit: usize,
    indirect_result: bool,
}

impl Default for Candidates {
    fn default() -> Self {
        let mode = std::env::var("WITCHY_NATIVE_INLINING").ok();
        Self {
            bodies: BTreeMap::new(),
            enabled: mode.as_deref() != Some("off"),
            selective: mode.as_deref() != Some("baseline"),
        }
    }
}

impl Candidates {
    pub(super) fn consider(
        &mut self,
        id: FuncId,
        body: &ir::Function,
        frame: u32,
        indirect_result: bool,
    ) {
        if !self.enabled {
            return;
        }
        // Calls can be remapped by the CLIF inliner. Keep call-bearing
        // wrappers smaller than leaves to bound duplicated call sites.
        let limit = if body.dfg.ext_funcs.is_empty() {
            SMALL_LEAF
        } else {
            SMALL_WRAPPER
        };
        // A global value is the runtime heap's symbol alone (body.rs makes it each function's
        // first name); `Caller::inline` checks the caller names the same symbol at that index.
        let symbols_only = body.global_values.values().all(|gv| {
            matches!(
                gv,
                ir::GlobalValueData::Symbol {
                    name: ExternalName::User(_),
                    ..
                }
            )
        });
        let allowance = if self.selective {
            EXPOSURE_ALLOWANCE
        } else {
            0
        };
        if body.dfg.num_insts() <= limit + allowance && symbols_only {
            self.bodies.insert(
                id,
                Candidate {
                    body: body.clone(),
                    frame,
                    limit,
                    indirect_result,
                },
            );
        }
    }

    pub(super) fn inline(&self, context: &mut Context, frame: u32, function: FuncId) -> Result<()> {
        if !self.bodies.is_empty() && !context.func.dfg.ext_funcs.is_empty() {
            let mut exposed_stack = false;
            context
                .inline(Caller {
                    exposed_stack: &mut exposed_stack,
                    candidates: self,
                    function,
                    instructions: CALLER_BUDGET,
                    // Copied stack slots now live in the caller. Charge each
                    // inline site the original callee's full logical frame.
                    frame: MAX_FRAME_BYTES - frame,
                })
                .map_err(error)?;
            if exposed_stack {
                forward_stack_loads(&mut context.func);
                remove_unobserved_stack_stores(&mut context.func);
            }
        }
        Ok(())
    }
}

struct Caller<'a> {
    candidates: &'a Candidates,
    exposed_stack: &'a mut bool,
    function: FuncId,
    instructions: usize,
    frame: u32,
}

impl Inline for Caller<'_> {
    fn inline(
        &mut self,
        caller: &ir::Function,
        _: ir::Inst,
        _: ir::Opcode,
        callee: ir::FuncRef,
        arguments: &[ir::Value],
    ) -> InlineCommand<'_> {
        let ExternalName::User(reference) = caller.dfg.ext_funcs[callee].name else {
            return InlineCommand::KeepCall;
        };
        // cranelift-module uses namespace zero for function IDs.
        let name = &caller.params.user_named_funcs()[reference];
        if name.namespace != 0 {
            return InlineCommand::KeepCall;
        }
        let callee = FuncId::from_u32(name.index);
        if callee == self.function {
            return InlineCommand::KeepCall;
        }
        let Some(Candidate {
            body,
            frame,
            limit,
            indirect_result,
        }) = self.candidates.bodies.get(&callee)
        else {
            return InlineCommand::KeepCall;
        };
        // The inliner copies a symbol's name reference unchanged: it must name the same symbol
        // in the caller.
        for gv in body.global_values.values() {
            let ir::GlobalValueData::Symbol {
                name: ExternalName::User(reference),
                ..
            } = gv
            else {
                return InlineCommand::KeepCall;
            };
            let wanted = body.params.user_named_funcs().get(*reference);
            if wanted.is_none() || caller.params.user_named_funcs().get(*reference) != wanted {
                return InlineCommand::KeepCall;
            }
        }
        let cost = body.dfg.num_insts();
        if cost > self.instructions || *frame > self.frame {
            return InlineCommand::KeepCall;
        }
        // Admit slightly larger bodies when inlining exposes a caller stack
        // argument or indirect result to stack promotion.
        let stack_argument = self.candidates.selective
            && arguments
                .iter()
                .any(|&value| stack_address(&caller.dfg, value).is_some());
        let exposes_stack = self.candidates.selective && (*indirect_result || stack_argument);
        if cost > *limit && !exposes_stack {
            return InlineCommand::KeepCall;
        }
        *self.exposed_stack |= exposes_stack;
        self.instructions -= cost;
        self.frame -= frame;
        InlineCommand::Inline {
            callee: Cow::Borrowed(body),
            visit_callee: false,
        }
    }
}
