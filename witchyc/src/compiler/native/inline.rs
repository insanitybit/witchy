//! Copy finalized CLIF. Proofs, loans, and nopanic have already been checked in the
//! source definition, and its concrete instance has already selected its checks.
use super::{Result, body::MAX_FRAME_BYTES, error};
use cranelift_codegen::{
    Context,
    inline::{Inline, InlineCommand},
    ir::{self, ExternalName},
};
use cranelift_module::FuncId;
use std::{borrow::Cow, collections::{BTreeMap, BTreeSet}};

mod stack;
use stack::{forward_stack_loads, remove_unobserved_stack_stores, stack_address};

// A checked accessor (`Vec.at`: bounds test, lookup, a Result of a reference) is 47
// instructions and calls nothing; at 32 it stayed a call in every loop that reads a vector.
const SMALL_LEAF: usize = 48;
const SMALL_WRAPPER: usize = 16;
const EXPOSURE_ALLOWANCE: usize = 16;
// What one function may take in, over all its call sites. At 128 the first few sites of a
// long function spent it and the loops further down kept their calls.
const CALLER_BUDGET: usize = 512;

pub(super) struct Candidates {
    bodies: BTreeMap<FuncId, Candidate>,
    /// Every function some candidate's body calls, a candidate or not.
    called: BTreeSet<FuncId>,
    /// The candidates that can reach themselves through the bodies of candidates.
    recursive: BTreeSet<FuncId>,
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
            called: BTreeSet::new(),
            recursive: BTreeSet::new(),
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
        // Calls can be remapped by the CLIF inliner. A body that calls is copied with its
        // calls, and those are considered in turn: keep such wrappers smaller than leaves.
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
            self.registered(id);
        }
    }

    /// Note which candidates the new one `id` puts on a cycle. Only a candidate that an
    /// earlier body already calls (or that calls itself) can close one, so most cost nothing.
    fn registered(&mut self, id: FuncId) {
        let calls: Vec<FuncId> = Self::calls(&self.bodies[&id].body).collect();
        self.called.extend(calls);
        if !self.called.contains(&id) {
            return;
        }
        // What `id` reaches through candidates, and who calls whom among them.
        let (mut reached, mut work) = (BTreeSet::from([id]), vec![id]);
        let mut callers = BTreeMap::<FuncId, Vec<FuncId>>::new();
        while let Some(next) = work.pop() {
            for callee in Self::calls(&self.bodies[&next].body).filter(|callee| self.bodies.contains_key(callee)) {
                callers.entry(callee).or_default().push(next);
                if reached.insert(callee) {
                    work.push(callee);
                }
            }
        }
        // Of those, the ones that reach `id` back: with `id`, the new cycles.
        let (mut back, mut work) = (BTreeSet::new(), vec![id]);
        while let Some(next) = work.pop() {
            for &caller in callers.get(&next).into_iter().flatten() {
                if back.insert(caller) {
                    work.push(caller);
                }
            }
        }
        self.recursive.extend(back);
    }

    /// The functions `body` calls.
    fn calls(body: &ir::Function) -> impl Iterator<Item = FuncId> + '_ {
        body.dfg.ext_funcs.values().filter_map(|function| match function.name {
            ExternalName::User(reference) => Some(&body.params.user_named_funcs()[reference]),
            _ => None,
        })
        .filter(|name| name.namespace == 0)
        .map(|name| FuncId::from_u32(name.index))
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
        // A copied body's calls are considered in turn, and nothing here knows how deep a
        // copy is: a candidate that can reach itself through candidates would be unrolled
        // until the budget was gone. It stays a call (`registered` keeps the set).
        if self.candidates.recursive.contains(&callee) {
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
            // A candidate's body is registered as lowered, with its own calls standing, so the
            // calls a copied body brings are considered here, against the same budgets.
            visit_callee: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_codegen::ir::{self, AbiParam, ExtFuncData, InstBuilder, Signature, UserExternalName, UserFuncName, types::I64};
    use cranelift_codegen::cursor::Cursor;
    use cranelift_codegen::isa::CallConv;

    /// A small function `id` that calls each of `calls` once.
    fn body(id: u32, calls: &[u32]) -> ir::Function {
        let mut signature = Signature::new(CallConv::SystemV);
        signature.returns.push(AbiParam::new(I64));
        let mut function = ir::Function::with_name_signature(UserFuncName::user(0, id), signature.clone());
        let block = function.dfg.make_block();
        function.layout.append_block(block);
        let mut pos = cranelift_codegen::cursor::FuncCursor::new(&mut function).at_bottom(block);
        let mut sum = pos.ins().iconst(I64, 1);
        for &callee in calls {
            let name = pos.func.declare_imported_user_function(UserExternalName::new(0, callee));
            let signature = pos.func.import_signature(signature.clone());
            let callee = pos.func.import_function(ExtFuncData { name: ExternalName::User(name), signature, colocated: true, patchable: false });
            let call = pos.ins().call(callee, &[]);
            let result = pos.func.dfg.inst_results(call)[0];
            sum = pos.ins().iadd(sum, result);
        }
        pos.ins().return_(&[sum]);
        function
    }

    fn candidates(bodies: &[(u32, &[u32])]) -> Candidates {
        let mut candidates = Candidates { bodies: BTreeMap::new(), called: BTreeSet::new(), recursive: BTreeSet::new(), selective: true, enabled: true };
        for &(id, calls) in bodies {
            candidates.consider(FuncId::from_u32(id), &body(id, calls), 0, false);
            assert!(candidates.bodies.contains_key(&FuncId::from_u32(id)), "{id} is small enough to be a candidate");
        }
        candidates
    }

    #[test]
    fn a_candidate_that_reaches_itself_is_recursive() {
        // 1 calls itself; 2 and 3 call each other; 4 calls 5 calls 6, and 7 calls the pair.
        let all = candidates(&[(1, &[1]), (2, &[3]), (3, &[2]), (4, &[5]), (5, &[6]), (6, &[]), (7, &[2, 6])]);
        let recursive = |all: &Candidates| all.recursive.iter().map(|id| id.as_u32()).collect::<Vec<u32>>();
        assert_eq!(recursive(&all), [1, 2, 3]);
        // Whatever order they are met in: a cycle of three closed by its first, middle or last.
        for order in [[1, 2, 3], [3, 2, 1], [2, 3, 1], [1, 3, 2]] {
            let calls = |id: u32| -> &[u32] { [&[2][..], &[3], &[1]][id as usize - 1] };
            let ring = candidates(&order.map(|id| (id, calls(id))));
            assert_eq!(recursive(&ring), [1, 2, 3], "met in the order {order:?}");
        }
        // A callee that is no candidate is never copied, so a cycle through it unrolls nothing.
        assert!(recursive(&candidates(&[(2, &[3])])).is_empty());
    }
}
