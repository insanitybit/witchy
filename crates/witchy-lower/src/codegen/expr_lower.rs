//! Expression lowering: the `lower_expr` dispatch that builds a `WirExpr` for
//! the lowerable subset of AST expressions (returning `None` for any arm — or
//! sub-expression — not yet lowered). Split out of `codegen/mod.rs` as a
//! continuation of the `Codegen` impl; behavior is unchanged.

use super::*;

impl<'types> Codegen<'types> {
    /// Build a `WirExpr` for the lowerable subset of expressions, returning `None`
    /// for any arm — or sub-expression — not yet lowered. A `None` propagates up and
    /// the program is rejected as reaching an unsupported construct; the supported
    /// set is the authoritative codegen for those expression shapes.
    pub(crate) fn lower_expr(&mut self, e: &Expr) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        use witchy_wir::wir::WirNode as N;
        if self.reject_unsupported_specialized_boundary(e) {
            return None;
        }
        Some(match e {
            // Expanded away by `crate::tagged` during linking, before codegen.
            Expr::TaggedLit { tag, .. } => {
                unreachable!("unexpanded tagged literal `{tag}` reached codegen")
            }
            Expr::Call { name, args }
                if witchy_syntax::intrinsics::canonical_operation_name(
                    witchy_syntax::cap_ops::surface_name(name),
                ) == witchy_syntax::intrinsics::DYNAMIC_TRY_DECODE_TYPED =>
            {
                return self.lower_dynamic_try_decode(e, args);
            }
            Expr::Int(n) | Expr::Duration(n) => W::ConstI64(*n),
            Expr::Float(x) => W::ConstF64(*x),
            Expr::Bool(b) => W::ConstI32(if *b { 1 } else { 0 }),
            Expr::Str(s) => W::StrPtr(self.intern(s)),
            Expr::Var(name) if self.is_plain_local_var(name) => W::GetLocal(name.clone()),
            // A bare top-level function name used as a VALUE (`list.filter(xs,
            // is_odd)`): materialize it as a forwarding closure `fn(p..): name(p..)`,
            // reusing the lambda machinery. Only fires for a known function that
            // isn't shadowed by a local; a shadowing local is handled elsewhere.
            Expr::Var(name)
                if self.collect_wir
                    && !self.locals.contains_key(name)
                    && self.fn_params.contains_key(name) =>
            {
                let params = self.fn_params.get(name).cloned()?;
                let args = params.iter().map(|p| Expr::Var(p.name.clone())).collect();
                let body = Block {
                    stmts: vec![Stmt::Expr(Expr::Call { name: name.clone(), args })],
                    lines: vec![0],
                    region: None,
                };
                // This forwarding body is synthesized after type annotation, so
                // `e` need not have a TypeTable row. Recover the exact ABI from
                // the declaration tables instead of silently falling back to
                // scalar slots (which would erase an externref parameter).
                let param_kinds = params
                    .iter()
                    .map(|param| {
                        param
                            .ty
                            .as_ref()
                            .map(|ty| self.kind_for_type(ty))
                            .unwrap_or(Kind::I32)
                    })
                    .collect();
                let signature = (
                    param_kinds,
                    self.fn_ret
                        .get(name)
                        .copied()
                        .unwrap_or_else(|| self.apply_ret_kind(e)),
                );
                let result_ty = self
                    .fn_ret_ty
                    .get(name)
                    .cloned()
                    .or_else(|| self.closure_result_type(e));
                let access = self.closure_access_signature(e)?;
                let ownership = Self::ownership_envelope_for_signature(&access);
                let call = match body.stmts.first() {
                    Some(Stmt::Expr(call @ Expr::Call { .. })) => call,
                    _ => return None,
                };
                let call_key = call as *const Expr as usize;
                self.synthesized_call_access.insert(call_key, access.clone());
                let lowered = self.lower_lambda(
                    &params,
                    &body,
                    &signature,
                    result_ty.as_ref(),
                    Some(&access),
                    &ownership,
                );
                let removed = self.synthesized_call_access.remove(&call_key);
                debug_assert!(removed.is_some(), "registered forwarding call must be consumed");
                return lowered;
            }
            Expr::Unary { op, expr } => match op {
                // value-neutral on WASM (value semantics): lower the operand.
                UnOp::Move | UnOp::Await => return self.lower_expr(expr),
                UnOp::Not => W::Unary {
                    op: witchy_wir::wir::UnOp::Not,
                    kind: witchy_wir::wir::Kind::I32,
                    arg: Box::new(self.lower_expr(expr)?),
                },
                UnOp::Neg => {
                    let kind = Self::wir_kind(self.kind_of(expr));
                    W::Unary { op: witchy_wir::wir::UnOp::Neg, kind, arg: Box::new(self.lower_expr(expr)?) }
                }
                UnOp::BitNot => {
                    let kind = Self::wir_kind(self.kind_of(expr));
                    W::Unary {
                        op: witchy_wir::wir::UnOp::BitNot,
                        kind,
                        arg: Box::new(self.lower_expr(expr)?),
                    }
                }
            },
            // Compiler-owned RFC-0081 construction: preserve the payload's exact
            // representation inside its own GC box, then erase only the outer
            // reference in the fixed `{structref, witness}` envelope.
            Expr::ExistentialPack { expr, witness, .. } => {
                let payload_id = *self.existential_payload_ids.get(witness)?;
                let payload = W::StructNew {
                    struct_id: payload_id,
                    args: vec![self.lower_expr(expr)?],
                };
                return Some(W::StructNew {
                    struct_id: EXISTENTIAL_WRAPPER_ID,
                    args: vec![payload, W::ConstI32(i32::try_from(*witness).ok()?)],
                });
            }
            Expr::ExistentialUpcast { expr, ty } => {
                let level = self.existential_call_level;
                if level >= EXISTENTIAL_CALL_POOL {
                    return None;
                }
                self.existential_call_level += 1;
                let source = self.lower_expr(expr);
                self.existential_call_level = level;
                let source = source?;
                let local = existential_call_scratch(level);
                let source_witness = || W::StructGet {
                    struct_id: EXISTENTIAL_WRAPPER_ID,
                    field: 1,
                    base: Box::new(W::GetLocal(local.clone())),
                };
                let mut transitions = self
                    .existential_upcasts
                    .iter()
                    .filter(|(_, target, _)| target == ty)
                    .cloned()
                    .collect::<Vec<_>>();
                if transitions.is_empty() {
                    return None;
                }
                transitions.sort_by_key(|(source, _, _)| *source);
                let mut selected = None;
                for (from, _, to) in transitions.into_iter().rev() {
                    let fallback = selected.take().unwrap_or_else(
                        || W::Control(Box::new(N::Block {
                            label: "__witchy_bad_existential_upcast".into(),
                            result: Some(witchy_wir::wir::WirTy::Bool),
                            body: vec![N::Unreachable],
                        })),
                    );
                    selected = Some(W::Control(Box::new(N::If {
                        cond: W::Binary {
                            op: witchy_wir::wir::BinOp::Eq,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(source_witness()),
                            rhs: Box::new(W::ConstI32(i32::try_from(from).ok()?)),
                        },
                        then_: vec![N::Push(W::ConstI32(i32::try_from(to).ok()?))],
                        els: vec![N::Push(fallback)],
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    })));
                }
                let payload = W::StructGet {
                    struct_id: EXISTENTIAL_WRAPPER_ID,
                    field: 0,
                    base: Box::new(W::GetLocal(local.clone())),
                };
                W::Seq(vec![
                    N::SetLocal { local, value: source },
                    N::Push(W::StructNew {
                        struct_id: EXISTENTIAL_WRAPPER_ID,
                        args: vec![payload, selected?],
                    }),
                ])
            }
            // RFC-0081 dispatch selects a compiler-owned adapter from the dense
            // witness table. This is deliberately not a source-name fallback:
            // trait lowering already authenticated the static owner and slot.
            Expr::ExistentialCall {
                receiver,
                args,
                slot,
                result,
                ..
            } => {
                let access = self.call_access_signature(e)?.clone();
                if !self.collect_wir
                    || access.params().iter().skip(1).any(|param| {
                        !matches!(
                            param.kind(),
                            witchy_types::access::AccessKind::OwnedImmutable
                                | witchy_types::access::AccessKind::ExclusiveWriteback
                        )
                    })
                    || self.existential_dispatch_stride == 0
                {
                    return None;
                }
                let level = self.existential_call_level;
                if level >= EXISTENTIAL_CALL_POOL {
                    return None;
                }
                if access.params().len() != args.len() + 1 {
                    return None;
                }
                let mut operands = Vec::with_capacity(args.len() + 1);
                operands.push(receiver.as_ref().clone());
                operands.extend(args.iter().cloned());
                let mut operand_kinds = Vec::with_capacity(args.len() + 1);
                operand_kinds.push(Kind::GcRef(EXISTENTIAL_WRAPPER_ID));
                operand_kinds.extend(args.iter().map(|arg| self.kind_of(arg)));
                let ownership = Self::ownership_envelope_for_signature(&access);
                self.existential_call_level = level + 1;
                let lowered = self.lower_closure_args(
                    &operands,
                    &access,
                    &operand_kinds,
                    true,
                    &ownership,
                );
                self.existential_call_level = level;
                let (mut lowered_operands, writebacks, capacity_dests) = lowered?;
                let receiver_value = lowered_operands.remove(0);
                let receiver_tmp = existential_call_scratch(level);
                let result_kind = self.kind_for_type(result);
                let mut call_args = vec![W::GetLocal(receiver_tmp.clone())];
                call_args.extend(lowered_operands);
                let mut signature_params = vec![witchy_wir::wir::Kind::StructRef];
                signature_params.extend(args.iter().map(|arg| Self::wir_kind(self.kind_of(arg))));
                signature_params.extend(
                    ownership
                        .var_capacity_params
                        .iter()
                        .map(|_| witchy_wir::wir::Kind::I32),
                );
                let witness_id = W::StructGet {
                    struct_id: EXISTENTIAL_WRAPPER_ID,
                    field: 1,
                    base: Box::new(W::GetLocal(receiver_tmp.clone())),
                };
                let table_index = W::Binary {
                    op: witchy_wir::wir::BinOp::Add,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(W::Binary {
                        op: witchy_wir::wir::BinOp::Mul,
                        kind: witchy_wir::wir::Kind::I32,
                        lhs: Box::new(witness_id),
                        rhs: Box::new(W::ConstI32(i32::try_from(self.existential_dispatch_stride).ok()?)),
                    }),
                    rhs: Box::new(W::ConstI32(i32::try_from(*slot).ok()?)),
                };
                let mut seq = vec![N::SetLocal { local: receiver_tmp, value: receiver_value }];
                if !writebacks.is_empty() || ownership.has_state() {
                    let mut results = vec![Self::wir_kind(result_kind)];
                    if ownership.unique_capacity_result {
                        results.push(witchy_wir::wir::Kind::I32);
                    }
                    results.extend(
                        writebacks
                            .iter()
                            .map(|(_, kind, _)| Self::wir_kind(*kind)),
                    );
                    results.extend(
                        ownership
                            .var_capacity_params
                            .iter()
                            .map(|_| witchy_wir::wir::Kind::I32),
                    );
                    let dests = Self::closure_call_dests(
                        result_kind,
                        true,
                        &writebacks,
                        &capacity_dests,
                        &ownership,
                    );
                    let call = N::CallIndirectStoreMulti {
                        signature: witchy_wir::wir::ClosureSignature {
                            params: signature_params,
                            results,
                        },
                        args: call_args,
                        index: table_index,
                        dests,
                    };
                    let result = self.finish_closure_multi_call(
                        call,
                        writebacks,
                        result_kind,
                        true,
                        true,
                    )?;
                    seq.push(N::Push(result));
                } else if access.params().first().is_some_and(|receiver| {
                    matches!(
                        receiver.kind(),
                        witchy_types::access::AccessKind::OwnedImmutable
                            | witchy_types::access::AccessKind::SharedBorrow
                            | witchy_types::access::AccessKind::Consuming
                    )
                }) {
                    seq.push(N::Push(W::CallIndirect {
                        signature: witchy_wir::wir::ClosureSignature {
                            params: signature_params,
                            results: vec![Self::wir_kind(result_kind)],
                        },
                        args: call_args,
                        index: Box::new(table_index),
                    }));
                } else {
                    return None;
                }
                return Some(W::Seq(seq));
            }
            // `e as T` (capability narrowing / type ascription) is value-neutral
            // at codegen — lower the inner expression unchanged.
            Expr::As { expr, .. } => return self.lower_expr(expr),
            // A bare block expression: its `WirSeq` leaves the block's value.
            // (Region blocks keep their bespoke `compile_region` emission.)
            Expr::Block(b) if b.region.is_none() => return Some(W::Seq(self.lower_block(b)?)),
            // A `region:` block on the BINARY path. A SCALAR result (Int/Bool/Float)
            // lives in a register, not the heap, so we reclaim fully: capture the heap
            // watermark, run the body, stash the scalar in the universal i64 slot
            // (`$MATCH_TMP`), reset `$heap` to the watermark (freeing the body's
            // allocations), then recover the scalar. A POINTER result (list/record/…)
            // would be on the reclaimed heap, so it needs the per-shape `$rcopy_*`
            // deep-copy — deferred (a future wir_opt pass) — and lowers as a plain
            // block for now (correct value, no reclaim). WAT-path region blocks
            // (`collect_wir == false`) fall through to `compile_region`.
            Expr::Block(b) if self.collect_wir => {
                let ann = b.region.as_ref().and_then(|r| r.ty.clone());
                let shape = match &ann {
                    Some(t) => self.eq_shape_of_type(t),
                    None => match b.stmts.last() {
                        Some(Stmt::Expr(tail)) => self.eq_operand_shape(tail),
                        _ => None,
                    },
                };
                let is_scalar =
                    matches!(shape, Some(EqShape::Int | EqShape::Bool | EqShape::Float));
                if self.wm_level >= WM_POOL {
                    return Some(W::Seq(self.lower_block(b)?));
                }
                // A POINTER result lives on the reclaimed heap, so it needs its
                // per-shape `$rcopy_*` deep-copy. `ensure_rcopy_wir_helper` returns the
                // helper name (Str/List/Tuple so far) or `None` (an unsupported shape
                // or a recursive-type cycle) → fall back to a plain block (correct
                // value, no reclaim).
                let rcopy_helper: Option<String> = if is_scalar {
                    None
                } else {
                    match &shape {
                        Some(s) => self.ensure_rcopy_wir_helper(s),
                        None => None,
                    }
                };
                if !is_scalar && rcopy_helper.is_none() {
                    return Some(W::Seq(self.lower_block(b)?));
                }
                let wm = format!("__witchy_wm_{}", self.wm_level);
                let body_kind = self.block_kind(b);
                self.wm_level += 1;
                self.uses_wm = true;
                let mut body = self.lower_block(b)?;
                self.wm_level -= 1;
                // Split off the body's tail value. Its `Push(value)` is usually last,
                // but the uniqueness pass appends self-healing `*__cap` token writes
                // after it (block-scope cleanup); drain those, keeping them to run
                // before reclaim (cap tokens are scalars, untouched by the heap slide).
                let mut cap_heals: Vec<N> = vec![];
                while matches!(
                    body.last(),
                    Some(N::SetLocal { local, .. }) if local.ends_with("__cap")
                ) {
                    cap_heals.push(body.pop().unwrap());
                }
                let Some(N::Push(tail)) = body.pop() else {
                    return Some(W::Seq(self.lower_block(b)?));
                };
                cap_heals.reverse();
                body.extend(cap_heals);
                let mut seq = vec![N::SetLocal {
                    local: wm.clone(),
                    value: W::GetGlobal("heap".to_string()),
                }];
                seq.extend(body);
                if is_scalar {
                    seq.push(N::SetLocal {
                        local: MATCH_TMP.to_string(),
                        value: W::ToSlot(Box::new(tail), Self::wir_kind(body_kind)),
                    });
                    seq.push(Self::increment_counter("__witchy_region_rewind_calls"));
                    seq.push(N::SetGlobal { global: "heap".to_string(), value: W::GetLocal(wm) });
                    // (RFC-0016) The reset reclaims every address at/above the
                    // watermark, so any RC-floor free-list entry freed inside the body
                    // now dangles — drop the list (sound; a no-op when rc-floor is off).
                    seq.push(N::SetGlobal { global: "rc_freelist".to_string(), value: W::ConstI32(0) });
                    seq.push(N::Push(W::FromSlot(
                        Box::new(W::GetLocal(MATCH_TMP.to_string())),
                        Self::wir_kind(body_kind),
                    )));
                    return Some(W::Seq(seq));
                }
                // Pointer reclaim: stash the result ptr, set the rcopy globals (wm /
                // temp base = heap / slide delta), deep-copy it ABOVE the live data
                // (the helper returns a pre-biased ptr), `memory.copy` the finished
                // block down to the watermark, advance `$heap` past it, return the ptr.
                self.uses_region = true;
                let helper = rcopy_helper.expect("guarded pointer shape");
                let i32sub = |l: W, r: W| W::Binary {
                    op: witchy_wir::wir::BinOp::Sub,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                };
                let copied_len =
                    || i32sub(W::GetGlobal("heap".to_string()), W::GetGlobal("rcopy_base".to_string()));
                seq.push(N::SetLocal { local: TUPLE_TMP.to_string(), value: tail });
                seq.push(N::SetGlobal { global: "rcopy_wm".to_string(), value: W::GetLocal(wm.clone()) });
                seq.push(N::SetGlobal {
                    global: "rcopy_base".to_string(),
                    value: W::GetGlobal("heap".to_string()),
                });
                seq.push(N::SetGlobal {
                    global: "rcopy_delta".to_string(),
                    value: i32sub(W::GetGlobal("heap".to_string()), W::GetLocal(wm.clone())),
                });
                seq.push(N::SetLocal {
                    local: TUPLE_TMP.to_string(),
                    value: W::Call { func: helper, args: vec![W::GetLocal(TUPLE_TMP.to_string())] },
                });
                // (RFC-0023) The next `memory.copy` slides the result down over the
                // body's (and the deep-copy's) allocations, reusing every address at or
                // above the watermark — a raw copy `$ensure` never sees. Tell the checked
                // heap to drop those redzones first, so the reuse isn't read as an overrun.
                if witchy_wir::wir_helpers::heap_check_enabled() {
                    seq.push(N::Do(W::Call {
                        func: "__heap_reclaim".to_string(),
                        args: vec![W::GetLocal(wm.clone())],
                    }));
                }
                seq.push(N::MemoryCopy {
                    dest: W::GetLocal(wm.clone()),
                    src: W::GetGlobal("rcopy_base".to_string()),
                    len: copied_len(),
                });
                seq.push(Self::increment_counter("__witchy_region_rewind_calls"));
                seq.push(N::SetGlobal {
                    global: "heap".to_string(),
                    value: W::Binary {
                        op: witchy_wir::wir::BinOp::Add,
                        kind: witchy_wir::wir::Kind::I32,
                        lhs: Box::new(W::GetLocal(wm)),
                        rhs: Box::new(copied_len()),
                    },
                });
                // (RFC-0016) As the scalar path: the slide-down reuses every address
                // at/above the watermark, so RC-floor free-list entries freed inside
                // the body dangle — drop the list (a no-op when rc-floor is off).
                seq.push(N::SetGlobal { global: "rc_freelist".to_string(), value: W::ConstI32(0) });
                seq.push(N::Push(W::GetLocal(TUPLE_TMP.to_string())));
                return Some(W::Seq(seq));
            }
            // `match` on scalar patterns; non-scalar arms fall through to legacy.
            Expr::Match { scrutinee, arms } => return self.lower_match(scrutinee, arms),
            // A lambda lowers to a uniform GC wrapper plus its typed environment;
            // the lifted body is registered as a `WirFunc` + table entry.
            Expr::Lambda { params, body, .. } => {
                let signature = (self.closure_param_kinds(e), self.apply_ret_kind(e));
                let result_ty = self.closure_result_type(e);
                let access = self.closure_access_signature(e);
                let ownership = access
                    .as_ref()
                    .map(Self::ownership_envelope_for_signature)
                    .unwrap_or_default();
                return self.lower_lambda(
                    params,
                    body,
                    &signature,
                    result_ty.as_ref(),
                    access.as_ref(),
                    &ownership,
                );
            }
            // Call a closure value: stash the wrapper, then `call_indirect` with
            // that wrapper, signature-shaped args, and its immutable code field.
            Expr::Apply { func, args } => {
                // Only a WIR-collecting scope lowers `Expr::Apply`; otherwise bail
                // so the construct is reported unsupported.
                if !self.collect_wir {
                    return None;
                }
                let level = self.apply_level;
                if level >= APPLY_POOL {
                    return None;
                }
                let access = self.call_access_signature(e)?.clone();
                let param_kinds = access
                    .params()
                    .iter()
                    .map(|param| self.kind_for_type(param.ty()))
                    .collect::<Vec<_>>();
                let recover_kind = self.kind_for_type(access.result().ty());
                let typed_abi = Self::closure_uses_typed_abi(&param_kinds, recover_kind);
                let ownership = Self::ownership_envelope_for_signature(&access);
                // (RFC-0062 tier-1) An ELIDED closure applied by name: no closure pointer to
                // stash — thread captures (from their locals) as leading arg slots to a direct
                // `call $__lamt{i}`.
                if let Expr::Var(fname) = func.as_ref() {
                    if let Some((idx, caps)) = self.thread_index.get(fname).cloned() {
                        self.apply_level = level + 1;
                        let mut call_args: Vec<W> = caps
                            .iter()
                            .map(|(cn, ck)| {
                                let value = W::GetLocal(cn.clone());
                                if ck.is_ref() {
                                    value
                                } else {
                                    W::ToSlot(Box::new(value), Self::wir_kind(*ck))
                                }
                            })
                            .collect();
                        let (arg_slots, writebacks, capacity_dests) = self.lower_closure_args(
                            args,
                            &access,
                            &param_kinds,
                            typed_abi,
                            &ownership,
                        )?;
                        call_args.extend(arg_slots);
                        self.apply_level = level;
                        if writebacks.is_empty() && !ownership.has_state() {
                            let call = W::Call { func: format!("__lamt{idx}"), args: call_args };
                            return Some(if typed_abi {
                                call
                            } else {
                                W::FromSlot(Box::new(call), Self::wir_kind(recover_kind))
                            });
                        }
                        let dests = Self::closure_call_dests(
                            recover_kind,
                            typed_abi,
                            &writebacks,
                            &capacity_dests,
                            &ownership,
                        );
                        let call = N::CallStoreMulti {
                            func: format!("__lamt{idx}"),
                            args: call_args,
                            dests,
                        };
                        return self.finish_closure_multi_call(
                            call,
                            writebacks,
                            recover_kind,
                            typed_abi,
                            false,
                        );
                    }
                }
                let n = args.len();
                let tmp = format!("__witchy_call_{level}");
                let fcode = self.lower_expr(func)?;
                self.apply_level = level + 1;
                let (arg_slots, writebacks, capacity_dests) = self.lower_closure_args(
                    args,
                    &access,
                    &param_kinds,
                    typed_abi,
                    &ownership,
                )?;
                self.apply_level = level;
                self.clos_arities.insert(n);
                let mut ci_args = vec![W::GetLocal(tmp.clone())];
                ci_args.extend(arg_slots);
                // (RFC-0034 L3) Devirtualize an apply whose callee is a single-bound,
                // never-reassigned closure var: a direct `call $__lamw{i}` (env stays
                // the stashed closure pointer), skipping the runtime code-index load.
                if !writebacks.is_empty() || ownership.has_state() {
                    let dests = Self::closure_call_dests(
                        recover_kind,
                        typed_abi,
                        &writebacks,
                        &capacity_dests,
                        &ownership,
                    );
                    let signature = Self::closure_signature(
                        n,
                        &param_kinds,
                        recover_kind,
                        &writebacks,
                        typed_abi,
                        &ownership,
                    );
                    let indirect_ownership = !matches!(
                        func.as_ref(),
                        Expr::Var(fname) if self.devirt_index.contains_key(fname)
                    );
                    let call = match func.as_ref() {
                        Expr::Var(fname) if self.devirt_index.contains_key(fname) => {
                            N::CallStoreMulti {
                                func: format!("__lamw{}", self.devirt_index[fname]),
                                args: ci_args,
                                dests,
                            }
                        }
                        _ => N::CallIndirectStoreMulti {
                            signature,
                            args: ci_args,
                            index: W::StructGet {
                                struct_id: CLOSURE_WRAPPER_ID,
                                field: witchy_wir::wir::CLOSURE_CODE_FIELD,
                                base: Box::new(W::GetLocal(tmp.clone())),
                            },
                            dests,
                        },
                    };
                    let result = self.finish_closure_multi_call(
                        call,
                        writebacks,
                        recover_kind,
                        typed_abi,
                        indirect_ownership,
                    )?;
                    return Some(W::Seq(vec![N::SetLocal { local: tmp, value: fcode }, N::Push(result)]));
                }
                let call = match func.as_ref() {
                    Expr::Var(fname) if self.devirt_index.contains_key(fname) => {
                        let idx = self.devirt_index[fname];
                        W::Call { func: format!("__lamw{idx}"), args: ci_args }
                    }
                    _ => W::CallIndirect {
                        signature: Self::closure_signature(
                            n,
                            &param_kinds,
                            recover_kind,
                            &[],
                            typed_abi,
                            &ownership,
                        ),
                        args: ci_args,
                        index: Box::new(W::StructGet {
                            struct_id: CLOSURE_WRAPPER_ID,
                            field: witchy_wir::wir::CLOSURE_CODE_FIELD,
                            base: Box::new(W::GetLocal(tmp.clone())),
                        }),
                    },
                };
                let result = if typed_abi {
                    call
                } else {
                    W::FromSlot(Box::new(call), Self::wir_kind(recover_kind))
                };
                return Some(W::Seq(vec![
                    N::SetLocal { local: tmp, value: fcode },
                    N::Push(result),
                ]));
            }
            // `if cond { .. } else { .. }` value-if. CRITICAL: codegen lowers the
            // branch blocks BEFORE the cond in the `else` case (and cond first in
            // the no-`else` case); `intern` assigns string offsets in call order, so
            // the lowering order here must match codegen's exactly or data offsets
            // diverge.
            Expr::If { cond, then_block, else_block } => match else_block {
                Some(eb) => {
                    let tk = self.block_kind(then_block);
                    let ek = self.block_kind(eb);
                    // Use the type checker's kind for the whole value. A branch
                    // ending in `return` has no stack result, so treating its
                    // fallback block kind as a peer would erase a reference kind.
                    let ck = self.kind_of(e);
                    let then_ = Self::convert_block_tail(self.lower_block(then_block)?, tk, ck);
                    let els = Self::convert_block_tail(self.lower_block(eb)?, ek, ck);
                    let cond = self.lower_expr(cond)?;
                    W::Control(Box::new(N::If {
                        cond,
                        then_,
                        els,
                        result: Some(Self::wir_ty_for_kind(ck)),
                    }))
                }
                None => {
                    let cond = self.lower_expr(cond)?;
                    let then_ = self.lower_block(then_block)?;
                    W::Control(Box::new(N::If {
                        cond,
                        then_,
                        els: vec![N::Push(W::ConstI32(0))],
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    }))
                }
            },
            // `while cond { body }` — Nil-valued. Only the no-watermark variant
            // (the arena isn't reset around the body) is lowered; the watermark
            // framing (`global.get $heap` save/restore) stays in legacy. `next_label`
            // is allocated in codegen's order (this loop's id BEFORE the body's
            // nested loops), and restored on a bail so the counter never desyncs.
            Expr::While { cond, body } => {
                let saved = self.next_label;
                let id = self.next_label;
                self.next_label += 1;
                let cond_w = match self.lower_expr(cond) {
                    Some(c) => c,
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                // Per-iteration arena reset (the watermark), if the body is
                // resettable — same treatment as the for-loops.
                let wm = self.loop_watermark_wir(body);
                self.loop_labels.push((format!("$we{id}"), format!("$wl{id}")));
                let body_res = self.lower_block(body);
                self.loop_labels.pop();
                if wm.is_some() {
                    self.wm_level -= 1;
                }
                let body_seq = match body_res {
                    Some(b) => b,
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                let not_cond = W::Unary {
                    op: witchy_wir::wir::UnOp::Not,
                    kind: witchy_wir::wir::Kind::I32,
                    arg: Box::new(cond_w),
                };
                let mut loop_body = vec![
                    N::Br { target: format!("we{id}"), cond: Some(not_cond) },
                    N::Drop(W::Seq(body_seq)),
                ];
                // reclaim per-iteration arena garbage before re-testing the cond.
                if let Some((_, reset)) = &wm {
                    loop_body.extend(reset.clone());
                }
                loop_body.push(N::Br { target: format!("wl{id}"), cond: None });
                let mut outer: witchy_wir::wir::WirSeq = Vec::new();
                if let Some((capture, _)) = &wm {
                    outer.push(capture.clone());
                }
                outer.push(N::Block {
                    label: format!("we{id}"),
                    result: None,
                    body: vec![N::Loop { label: format!("wl{id}"), body: loop_body }],
                });
                outer.push(N::Push(W::ConstI32(0)));
                W::Seq(outer)
            },
            // `for var in lo..hi { body }` — count without materializing a list.
            // i64 counter + bound in scratch locals; inclusive ranges add a
            // pre-increment `ctr == end` guard so `..=i64::MAX` halts.
            Expr::For { var, iter, body } if matches!(iter.as_ref(), Expr::Range { .. }) => {
                let Expr::Range { lo, hi, inclusive } = iter.as_ref() else { unreachable!() };
                let saved = self.next_label;
                let id = self.next_label;
                self.next_label += 1;
                let ctr = format!("__forctr_{var}");
                let end = format!("__forend_{var}");
                let lo_k = self.kind_of(lo);
                let lo_w = match self.lower_expr(lo) {
                    Some(w) => Self::wir_convert(w, lo_k, Kind::I64),
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                let hi_k = self.kind_of(hi);
                let hi_w = match self.lower_expr(hi) {
                    Some(w) => Self::wir_convert(w, hi_k, Kind::I64),
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                // Per-iteration arena reset (the watermark optimization): save
                // `$heap` before the loop, restore it after each body. `None` when
                // the body isn't resettable — the loop is still correct without it.
                let wm = self.loop_watermark_wir(body);
                // (RFC-0034 L2) Register `(i, xs)` for `for i in 0..list.length(xs)`
                // (xs unmutated in the body) so `list.at(xs, i)` lowers to an unchecked
                // load. Popped right after the body, so the stack stays balanced even on
                // the early `None` bail below.
                let elide_pair = bounds_elide_pair(var, lo, hi, *inclusive, body);
                if let Some(p) = &elide_pair {
                    self.elide_index_list.push(p.clone());
                }
                self.loop_labels.push((format!("$fe{id}"), format!("$fc{id}")));
                let body_res = self.lower_block(body);
                self.loop_labels.pop();
                if elide_pair.is_some() {
                    self.elide_index_list.pop();
                }
                if wm.is_some() {
                    self.wm_level -= 1;
                }
                let body_seq = match body_res {
                    Some(b) => b,
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                let i64k = witchy_wir::wir::Kind::I64;
                let cmp = |op, l: &str, r: &str| W::Binary {
                    op,
                    kind: i64k,
                    lhs: Box::new(W::GetLocal(l.to_string())),
                    rhs: Box::new(W::GetLocal(r.to_string())),
                };
                let exit_op = if *inclusive { witchy_wir::wir::BinOp::Gt } else { witchy_wir::wir::BinOp::Ge };
                let lanes = if self.loop_unroll_safe(body) { 4 } else { 1 };
                let mut loop_body: witchy_wir::wir::WirSeq = Vec::new();
                for _ in 0..lanes {
                    // Guard every logical iteration. This preserves empty and
                    // reversed ranges and supplies the exact 0..3 remainder
                    // without a speculative body evaluation.
                    loop_body.push(N::Br {
                        target: format!("fe{id}"),
                        cond: Some(cmp(exit_op, &ctr, &end)),
                    });
                    loop_body.push(N::SetLocal {
                        local: var.clone(),
                        value: W::GetLocal(ctr.clone()),
                    });
                    loop_body.push(N::Block {
                        label: format!("fc{id}"),
                        result: None,
                        body: vec![N::Drop(W::Seq(body_seq.clone()))],
                    });
                    // Reclaim after each source iteration, never once per
                    // unrolled group, so allocation/free ordering is unchanged.
                    if let Some((_, reset)) = &wm {
                        loop_body.extend(reset.clone());
                    }
                    if *inclusive {
                        // Avoid overflowing the induction variable at i64::MAX.
                        loop_body.push(N::Br {
                            target: format!("fe{id}"),
                            cond: Some(cmp(witchy_wir::wir::BinOp::Eq, &ctr, &end)),
                        });
                    }
                    loop_body.push(N::SetLocal {
                        local: ctr.clone(),
                        value: W::Binary {
                            op: witchy_wir::wir::BinOp::Add,
                            kind: i64k,
                            lhs: Box::new(W::GetLocal(ctr.clone())),
                            rhs: Box::new(W::ConstI64(1)),
                        },
                    });
                }
                loop_body.push(N::Br { target: format!("fl{id}"), cond: None });
                let mut outer: witchy_wir::wir::WirSeq = vec![
                    N::SetLocal { local: ctr, value: lo_w },
                    N::SetLocal { local: end, value: hi_w },
                ];
                if let Some((capture, _)) = &wm {
                    outer.push(capture.clone());
                }
                outer.push(N::Block {
                    label: format!("fe{id}"),
                    result: None,
                    body: vec![N::Loop { label: format!("fl{id}"), body: loop_body }],
                });
                outer.push(N::Push(W::ConstI32(0)));
                W::Seq(outer)
            },
            // `for var in list { body }` — Nil-valued; iterate a `[len][e0]...` list
            // with a pointer + index in scratch locals; an optional per-iteration
            // arena reset (the watermark) reclaims body garbage each time around.
            Expr::For { var, iter, body } if !matches!(iter.as_ref(), Expr::Range { .. }) => {
                let saved = self.next_label;
                let id = self.next_label;
                self.next_label += 1;
                let list_l = format!("__forlist_{var}");
                let idx_l = format!("__fori_{var}");
                let iter_w = match self.lower_expr(iter) {
                    Some(w) => w,
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                if let Some(elem) = self.elem_record_type_of(iter) {
                    self.local_records.insert(var.clone(), elem);
                }
                let elem_kind = self.iter_elem_kind(iter);
                // Watermark AFTER the list is built (`iter_w`), so the list and its
                // elements live below the reset point and survive each iteration.
                let wm = self.loop_watermark_wir(body);
                self.loop_labels.push((format!("$fe{id}"), format!("$fc{id}")));
                let body_res = self.lower_block(body);
                self.loop_labels.pop();
                if wm.is_some() {
                    self.wm_level -= 1;
                }
                let body_seq = match body_res {
                    Some(b) => b,
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                let i32 = witchy_wir::wir::Kind::I32;
                let add = witchy_wir::wir::BinOp::Add;
                let gc_reference_list = self
                    .ast_type_of_expr(iter)
                    .as_ref()
                    .and_then(|ty| self.gc_reference_list_layout(ty));
                let specialized_list = self
                    .specialized_layout_of_expr(iter)
                    .and_then(|id| self.specialized_layouts.get(id))
                    .and_then(|descriptor| match (descriptor.header(), descriptor.size()) {
                        (
                            HeaderLayout::PackedList { data_offset, .. },
                            LayoutSize::Dynamic { stride, .. },
                        ) => Some((data_offset, stride)),
                        _ => None,
                    });
                // idx >= list.len  ->  br_if $fe
                let exit = N::Br {
                    target: format!("fe{id}"),
                    cond: Some(W::Binary {
                        op: witchy_wir::wir::BinOp::Ge,
                        kind: i32,
                        lhs: Box::new(W::GetLocal(idx_l.clone())),
                        rhs: Box::new(if gc_reference_list.is_some() {
                            W::ArrayLen(Box::new(W::GetLocal(list_l.clone())))
                        } else {
                            W::Load {
                                ptr: Box::new(W::GetLocal(list_l.clone())),
                                kind: i32,
                                offset: 0,
                            }
                        }),
                    }),
                };
                // var = from_slot( load( (list+4) + idx*8 ) )
                let (data_offset, stride) = specialized_list.unwrap_or((4, 8));
                let elem_addr = W::Binary {
                    op: add,
                    kind: i32,
                    lhs: Box::new(W::Binary {
                        op: add,
                        kind: i32,
                        lhs: Box::new(W::GetLocal(list_l.clone())),
                        rhs: Box::new(W::ConstI32(data_offset as i32)),
                    }),
                    rhs: Box::new(W::Binary {
                        op: witchy_wir::wir::BinOp::Mul,
                        kind: i32,
                        lhs: Box::new(W::GetLocal(idx_l.clone())),
                        rhs: Box::new(W::ConstI32(stride as i32)),
                    }),
                };
                let bind = N::SetLocal {
                    local: var.clone(),
                    value: if specialized_list.is_some() {
                        elem_addr
                    } else if let Some((_, array_id, _)) = gc_reference_list {
                        W::ArrayGet {
                            array_id,
                            array: Box::new(W::GetLocal(list_l.clone())),
                            index: Box::new(W::GetLocal(idx_l.clone())),
                        }
                    } else {
                        W::FromSlot(
                            Box::new(W::Load {
                                ptr: Box::new(elem_addr),
                                kind: witchy_wir::wir::Kind::I64,
                                offset: 0,
                            }),
                            Self::wir_kind(elem_kind),
                        )
                    },
                };
                let body_block = N::Block {
                    label: format!("fc{id}"),
                    result: None,
                    body: vec![N::Drop(W::Seq(body_seq))],
                };
                let advance = N::SetLocal {
                    local: idx_l.clone(),
                    value: W::Binary {
                        op: add,
                        kind: i32,
                        lhs: Box::new(W::GetLocal(idx_l.clone())),
                        rhs: Box::new(W::ConstI32(1)),
                    },
                };
                let mut loop_body: witchy_wir::wir::WirSeq = vec![exit, bind, body_block];
                // reclaim per-iteration arena garbage before advancing the index.
                if let Some((_, reset)) = &wm {
                    loop_body.extend(reset.clone());
                }
                loop_body.push(advance);
                loop_body.push(N::Br { target: format!("fl{id}"), cond: None });
                let mut outer: witchy_wir::wir::WirSeq = vec![
                    N::SetLocal { local: list_l, value: iter_w },
                    N::SetLocal { local: idx_l, value: W::ConstI32(0) },
                ];
                if let Some((capture, _)) = &wm {
                    outer.push(capture.clone());
                }
                outer.push(N::Block {
                    label: format!("fe{id}"),
                    result: None,
                    body: vec![N::Loop { label: format!("fl{id}"), body: loop_body }],
                });
                outer.push(N::Push(W::ConstI32(0)));
                W::Seq(outer)
            },
            // Aggregate literals: ordinary tuples retain the linear-memory
            // `$mkN` layout. A concrete cap-carrying tuple uses its interned GC
            // struct, so no reference field crosses the universal i64 slot.
            Expr::List(items)
                if self.gc_reference_list_literal_layout(e, items).is_some() =>
            {
                let (_, array_id, _) = self.gc_reference_list_literal_layout(e, items)?;
                let mut lowered = Vec::with_capacity(items.len());
                for item in items {
                    lowered.push(self.lower_expr(item)?);
                }
                return Some(W::ArrayNewFixed { array_id, items: lowered });
            }
            Expr::List(items) => {
                if self.specialized_layout_of_expr(e).is_some() {
                    return self.lower_packed_list_literal(e, items);
                }
                return self.lower_aggregate(items.len() as i32, items, 0);
            }
            Expr::Tuple(items) => {
                if self.specialized_layout_of_expr(e).is_some() {
                    return self.lower_packed_record_ctor(e, items);
                }
                if let Some(ty) = self.ast_type_of_expr(e)
                    && let Some(shape) = self.gc_tuple_shape(&ty)
                    && let Some(struct_id) = self.gc_tuple_ids.get(&shape).copied()
                {
                    let mut lowered = Vec::with_capacity(items.len());
                    for item in items {
                        lowered.push(self.lower_expr(item)?);
                    }
                    return Some(W::StructNew { struct_id, args: lowered });
                }
                return self.lower_aggregate(0, items, 0);
            }
            Expr::AnonCtor { tag, args } => {
                let ty = self
                    .type_table
                    .type_of(e)
                    .and_then(witchy_types::typeck::ty_to_ast)?;
                let Type::Named(name, _) = ty else {
                    return None;
                };
                let variants = witchy_types::typeck::anon_union_synthetic_variants(&name)?;
                let (_, arity) = variants
                    .iter()
                    .find(|(variant, arity)| variant == tag && *arity == args.len())?;
                let tag_code = self.anon_union_tag_code(tag, *arity);
                return self.lower_aggregate(tag_code, args, 0);
            }
            Expr::Ctor { name, args } => {
                if self.specialized_layout_of_expr(e).is_some() {
                    return self.lower_packed_record_ctor(e, args);
                }
                if let Some(ty) = self.ast_type_of_expr(e)
                    && let Some((_, option_kind)) = self.option_reference_inner(&ty)
                {
                    if name == "Some" && args.len() == 1 {
                        return self.lower_expr(&args[0]);
                    }
                    if name == "None" && args.is_empty() {
                        return Some(W::RefNull(Self::wir_kind(option_kind)));
                    }
                }
                if self.transparent_externref_ctors.contains_key(name) {
                    if args.len() != 1 {
                        return None;
                    }
                    return self.lower_expr(&args[0]);
                }
                let owner_ty = self.ast_type_of_expr(e);
                if let Some((layout, struct_id)) =
                    self.gc_layout_for_ctor(name, owner_ty.as_ref())
                {
                    if layout.field_types.len() != args.len() {
                        return None;
                    }
                    if layout.tag.is_none() {
                        let mut lowered = Vec::with_capacity(args.len());
                        for (arg, field_ty) in args.iter().zip(&layout.field_types) {
                            lowered.push(self.lower_gc_ctor_arg(arg, field_ty)?);
                        }
                        return Some(W::StructNew { struct_id, args: lowered });
                    }
                    let zero = |kind: witchy_wir::wir::Kind| match kind {
                        witchy_wir::wir::Kind::I32 => W::ConstI32(0),
                        witchy_wir::wir::Kind::I64 => W::ConstI64(0),
                        witchy_wir::wir::Kind::F64 => W::ConstF64(0.0),
                        ref_kind @ (witchy_wir::wir::Kind::ExternRef
                        | witchy_wir::wir::Kind::StructRef
                        | witchy_wir::wir::Kind::GcRef(_)) => W::RefNull(ref_kind),
                    };
                    let mut lowered: Vec<W> = self
                        .gc_structs
                        .get(struct_id as usize)?
                        .fields
                        .iter()
                        .copied()
                        .map(zero)
                        .collect();
                    lowered[0] = W::ConstI32(layout.tag? as i32);
                    for (i, arg) in args.iter().enumerate() {
                        lowered[layout.field_base as usize + i] =
                            self.lower_gc_ctor_arg(arg, &layout.field_types[i])?;
                    }
                    return Some(W::StructNew { struct_id, args: lowered });
                }
                if name == "Nil" && args.is_empty() {
                    return Some(W::ConstI32(0));
                }
                let &(tag, nfields) = self.ctors.get(name)?;
                if nfields != args.len() {
                    return None; // arity mismatch → legacy emits the loud error
                }
                // (RFC-0037 §3) Tag records (ctor name == type name, so the write here and the
                // `.field` check agree). ADT variants stay untagged (the type name differs from
                // the variant name); the tolerant check skips them.
                let type_tag = if self.record_fields.contains_key(name) { type_tag_of(name) } else { 0 };
                return self.lower_aggregate(tag as i32, args, type_tag);
            }
            // `update rec { field: v }` rebuilds the record: tag, then each field —
            // an overridden value (in a slot) or the base's raw slot copied across.
            // Only the bare-variable base is lowered (the base read directly); a
            // non-`Var` base needs the scratch-local pool, so it stays in legacy.
            Expr::RecordUpdate { name: _, base, fields } => {
                let tyname = self.record_type_of(base)?;
                let names = self.record_fields.get(&tyname)?.clone();
                // (RFC-0005 stage 4) A GC-lowered cap-carrying record spreads via
                // `StructNew`, reading each un-updated field from the base struct
                // with `StructGet` — the GC analog of the linear `mk{N}` below.
                if let Some(struct_id) =
                    self.ast_type_of_expr(base).and_then(|ty| self.gc_struct_id_for_type(&ty))
                {
                    let (prelude, base_ref): (Option<witchy_wir::wir::WirNode>, W) =
                        if let Expr::Var(v) = base.as_ref() {
                            (None, W::GetLocal(v.clone()))
                        } else {
                            let bw = self.lower_expr(base)?;
                            (
                                Some(witchy_wir::wir::WirNode::SetLocal {
                                    local: update_gc_tmp(struct_id),
                                    value: bw,
                                }),
                                W::GetLocal(update_gc_tmp(struct_id)),
                            )
                        };
                    let mut args = Vec::with_capacity(names.len());
                    for (i, (fname, _)) in names.iter().enumerate() {
                        if let Some((_, vexpr)) = fields.iter().find(|(n, _)| n == fname) {
                            args.push(self.lower_expr(vexpr)?);
                        } else {
                            args.push(W::StructGet {
                                struct_id,
                                field: i as u32,
                                base: Box::new(base_ref.clone()),
                            });
                        }
                    }
                    let new = W::StructNew { struct_id, args };
                    return Some(match prelude {
                        Some(p) => W::Seq(vec![p, witchy_wir::wir::WirNode::Push(new)]),
                        None => new,
                    });
                }
                let &(tag, nfields) = self.ctors.get(&tyname)?;
                self.mk_arities.insert(nfields);
                // A Var base is referenced directly; any other expression is
                // evaluated ONCE into the `$TUPLE_TMP` scratch (base-first, as the
                // interpreter does) so each un-updated field reads the same value.
                let (prelude, base_ptr): (Option<witchy_wir::wir::WirNode>, W) =
                    if let Expr::Var(v) = base.as_ref() {
                        (None, W::GetLocal(v.clone()))
                    } else {
                        let bw = self.lower_expr(base)?;
                        (
                            Some(witchy_wir::wir::WirNode::SetLocal { local: TUPLE_TMP.into(), value: bw }),
                            W::GetLocal(TUPLE_TMP.into()),
                        )
                    };
                let mut args = Vec::with_capacity(nfields + 1);
                args.push(W::ConstI32(tag as i32));
                for (i, (fname, _)) in names.iter().enumerate() {
                    if let Some((_, vexpr)) = fields.iter().find(|(n, _)| n == fname) {
                        let k = self.kind_of(vexpr);
                        let w = self.lower_expr(vexpr)?;
                        args.push(W::ToSlot(Box::new(w), Self::wir_kind(k)));
                    } else {
                        args.push(W::Load {
                            ptr: Box::new(W::Binary {
                                op: witchy_wir::wir::BinOp::Add,
                                kind: witchy_wir::wir::Kind::I32,
                                lhs: Box::new(base_ptr.clone()),
                                rhs: Box::new(W::ConstI32((4 + 8 * i) as i32)),
                            }),
                            kind: witchy_wir::wir::Kind::I64,
                            offset: 0,
                        });
                    }
                }
                let call = W::Call { func: format!("mk{nfields}"), args };
                return Some(match prelude {
                    Some(p) => W::Seq(vec![p, witchy_wir::wir::WirNode::Push(call)]),
                    None => call,
                });
            }
            Expr::Binary { op, lhs, rhs } => {
                // `&&`/`||` are short-circuit control flow, not a wasm binary op:
                // lower to a value-`if`.
                //   a && b  ->  if a { b } else { 0 }
                //   a || b  ->  if a { 1 } else { b }
                if matches!(op, BinOp::And | BinOp::Or) {
                    let cond = self.lower_expr(lhs)?;
                    let other = self.lower_expr(rhs)?;
                    let (then_, els) = if matches!(op, BinOp::And) {
                        (vec![witchy_wir::wir::WirNode::Push(other)], vec![
                            witchy_wir::wir::WirNode::Push(W::ConstI32(0)),
                        ])
                    } else {
                        (vec![witchy_wir::wir::WirNode::Push(W::ConstI32(1))], vec![
                            witchy_wir::wir::WirNode::Push(other),
                        ])
                    };
                    return Some(W::Control(Box::new(witchy_wir::wir::WirNode::If {
                        cond,
                        then_,
                        els,
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    })));
                }
                // `a ?? b` (RFC-0048): unwrap `Some`/`Ok` (tag 0, payload at
                // `tmp+4`) or evaluate the fallback — the same store-once/tag-test
                // shape as `?`, minus the early return. The fallback is lowered
                // inside the else branch, so it runs only on `None`/`Err`.
                if *op == BinOp::Coalesce {
                    use witchy_wir::wir::WirNode as N;
                    if let Some(shape) = self.reference_try_shape(lhs) {
                        let (aggregate_kind, payload_kind, cond, payload) = match shape {
                            ReferenceTryShape::Nullable { payload_kind } => {
                                let tmp = Self::reference_try_tmp(payload_kind)?;
                                (
                                    payload_kind,
                                    payload_kind,
                                    W::Unary {
                                        op: witchy_wir::wir::UnOp::Not,
                                        kind: witchy_wir::wir::Kind::I32,
                                        arg: Box::new(W::RefIsNull(Box::new(
                                            W::GetLocal(tmp.clone()),
                                        ))),
                                    },
                                    W::GetLocal(tmp),
                                )
                            }
                            ReferenceTryShape::Tagged {
                                struct_id,
                                success_tag,
                                payload_field,
                                payload_kind,
                                ..
                            } => {
                                let tmp = Self::reference_try_tmp(Kind::GcRef(struct_id))?;
                                (
                                    Kind::GcRef(struct_id),
                                    payload_kind,
                                    W::Binary {
                                        op: witchy_wir::wir::BinOp::Eq,
                                        kind: witchy_wir::wir::Kind::I32,
                                        lhs: Box::new(W::StructGet {
                                            struct_id,
                                            field: 0,
                                            base: Box::new(W::GetLocal(tmp.clone())),
                                        }),
                                        rhs: Box::new(W::ConstI32(success_tag as i32)),
                                    },
                                    W::StructGet {
                                        struct_id,
                                        field: payload_field,
                                        base: Box::new(W::GetLocal(tmp)),
                                    },
                                )
                            }
                        };
                        let tmp = Self::reference_try_tmp(aggregate_kind)?;
                        let lhs_w = self.lower_expr(lhs)?;
                        let rhs_kind = self.kind_of(rhs);
                        let rhs_w =
                            Self::wir_convert(self.lower_expr(rhs)?, rhs_kind, payload_kind);
                        return Some(W::Seq(vec![
                            N::SetLocal { local: tmp, value: lhs_w },
                            N::If {
                                cond,
                                then_: vec![N::Push(payload)],
                                els: vec![N::Push(rhs_w)],
                                result: Some(Self::wir_ty_for_kind(payload_kind)),
                            },
                        ]));
                    }
                    let k = self
                        .match_payload_valtype(lhs)
                        .map(valtype_kind)
                        .unwrap_or_else(|| self.kind_of(rhs));
                    let lhs_w = self.lower_expr(lhs)?;
                    let rhs_w = Self::wir_convert(self.lower_expr(rhs)?, self.kind_of(rhs), k);
                    let tmp = TRY_TMP.to_string();
                    let cond = W::Unary {
                        op: witchy_wir::wir::UnOp::Not,
                        kind: witchy_wir::wir::Kind::I32,
                        arg: Box::new(W::Load {
                            ptr: Box::new(W::GetLocal(tmp.clone())),
                            kind: witchy_wir::wir::Kind::I32,
                            offset: 0,
                        }),
                    };
                    let payload = W::FromSlot(
                        Box::new(W::Load {
                            ptr: Box::new(W::Binary {
                                op: witchy_wir::wir::BinOp::Add,
                                kind: witchy_wir::wir::Kind::I32,
                                lhs: Box::new(W::GetLocal(tmp.clone())),
                                rhs: Box::new(W::ConstI32(4)),
                            }),
                            kind: witchy_wir::wir::Kind::I64,
                            offset: 0,
                        }),
                        Self::wir_kind(k),
                    );
                    return Some(W::Seq(vec![
                        N::SetLocal { local: tmp.clone(), value: lhs_w },
                        N::If {
                            cond,
                            then_: vec![N::Push(payload)],
                            els: vec![N::Push(rhs_w)],
                            result: Some(Self::wir_ty_for_kind(k)),
                        },
                    ]));
                }
                // String concatenation (`+` flipped to `Concat`) lowers to
                // `$concat` (only in a WIR-collecting scope; otherwise this falls
                // through and the program is rejected as unsupported).
                if self.collect_wir && *op == BinOp::Concat {
                    let a = self.lower_expr(lhs)?;
                    let b = self.lower_expr(rhs)?;
                    return Some(W::Call { func: "concat".to_string(), args: vec![a, b] });
                }
                // String content equality lowers to `$str_eq` (only in a
                // WIR-collecting scope). `!=` is `i32.eqz` of the equality result.
                if self.collect_wir
                    && matches!(op, BinOp::Eq | BinOp::NotEq)
                    && self.val_type_of(lhs) == ValType::Str
                    && self.val_type_of(rhs) == ValType::Str
                {
                    let a = self.lower_expr(lhs)?;
                    let b = self.lower_expr(rhs)?;
                    let eq = W::Call { func: "str_eq".to_string(), args: vec![a, b] };
                    return Some(match op {
                        BinOp::Eq => eq,
                        _ => W::Unary {
                            op: witchy_wir::wir::UnOp::Not,
                            kind: witchy_wir::wir::Kind::I32,
                            arg: Box::new(eq),
                        },
                    });
                }
                // String ordering (`<`/`<=`/`>`/`>=`) lowers to a sign compare of
                // `$str_cmp(a, b)` against 0 (binary path only) — lexicographic, not
                // pointer order. `==`/`!=` were handled by the `$str_eq` block above.
                if self.collect_wir
                    && matches!(op, BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq)
                    && (self.val_type_of(lhs) == ValType::Str
                        || self.val_type_of(rhs) == ValType::Str)
                {
                    self.uses_str_cmp = true;
                    let a = self.lower_expr(lhs)?;
                    let b = self.lower_expr(rhs)?;
                    let cmp = W::Call { func: "str_cmp".to_string(), args: vec![a, b] };
                    let wop = match op {
                        BinOp::Lt => witchy_wir::wir::BinOp::Lt,
                        BinOp::LtEq => witchy_wir::wir::BinOp::Le,
                        BinOp::Gt => witchy_wir::wir::BinOp::Gt,
                        _ => witchy_wir::wir::BinOp::Ge,
                    };
                    return Some(W::Binary {
                        op: wop,
                        kind: witchy_wir::wir::Kind::I32,
                        lhs: Box::new(cmp),
                        rhs: Box::new(W::ConstI32(0)),
                    });
                }
                // Compound (record/tuple/list/enum) equality lowers to the
                // per-shape WIR structural-equality helper (binary path only),
                // gated exactly like the legacy arm (`eq_shape_of(..).is_compound`).
                // A compound `==` MUST be structural, so once the shape is known to
                // be compound we either build the helper or bail the whole function
                // (`?`) — never fall through to a bare pointer compare.
                if self.collect_wir && matches!(op, BinOp::Eq | BinOp::NotEq) {
                    if let Some(shape) = self.eq_shape_of(lhs).or_else(|| self.eq_shape_of(rhs)) {
                        if shape.is_compound() {
                            // A compound `==` MUST be structural; if the shape can't
                            // be built (an unresolved generic payload, e.g. `Ok([])`)
                            // it is a HARD rejection, never a fall-through to a bare
                            // pointer compare.
                            let Some(h) = self.ensure_eq_wir_helper(&shape) else {
                                self.reject_reason.get_or_insert_with(|| CodegenError {
                                    message: "could not resolve the structural-equality shape for a \
                                              compound `==` (an unresolved generic payload, e.g. an \
                                              empty list) — annotate the operands' element type"
                                        .into(),
                                });
                                return None;
                            };
                            let a = self.lower_expr(lhs)?;
                            let b = self.lower_expr(rhs)?;
                            let eq = W::Call { func: h, args: vec![a, b] };
                            return Some(match op {
                                BinOp::Eq => eq,
                                _ => W::Unary { op: witchy_wir::wir::UnOp::Not, kind: witchy_wir::wir::Kind::I32, arg: Box::new(eq) },
                            });
                        }
                    }
                }
                // Float ordering (`<`/`<=`/`>`/`>=` on f64) lowers to the
                // NaN-trapping helper `$f_lt`/`$f_le`/`$f_gt`/`$f_ge` (binary path
                // only); the interpreter errors on ordering a NaN, so a bare
                // `f64.lt` would diverge from the oracle.
                if self.collect_wir
                    && matches!(op, BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq)
                    && (self.kind_of(lhs) == Kind::F64 || self.kind_of(rhs) == Kind::F64)
                {
                    self.uses_float_ord = true;
                    let func = match op {
                        BinOp::Lt => "f_lt",
                        BinOp::LtEq => "f_le",
                        BinOp::Gt => "f_gt",
                        _ => "f_ge",
                    };
                    let a = self.lower_expr(lhs)?;
                    let b = self.lower_expr(rhs)?;
                    return Some(W::Call { func: func.to_string(), args: vec![a, b] });
                }
                // Logical `&&`/`||` lower to a short-circuit value-`If` (binary path
                // only): `a && b` is `if a { b } else { false }`, `a || b` is
                // `if a { true } else { b }`, so the right operand is evaluated only
                // when the left doesn't already decide the result. This matches the
                // interpreter and preserves guards like `i < n && list.at(xs, i)`
                // that depend on the RHS not running when the LHS is false.
                if self.collect_wir && matches!(op, BinOp::And | BinOp::Or) {
                    let a = self.lower_expr(lhs)?;
                    let b = self.lower_expr(rhs)?;
                    let (then_, els) = match op {
                        BinOp::And => (vec![N::Push(b)], vec![N::Push(W::ConstI32(0))]),
                        _ => (vec![N::Push(W::ConstI32(1))], vec![N::Push(b)]),
                    };
                    return Some(W::Control(Box::new(N::If {
                        cond: a,
                        then_,
                        els,
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    })));
                }
                // Common-kind promotion (f64 > i64 > i32), exactly as the legacy
                // numeric path; each operand is then widened to `ck` via a `Convert`
                // node reproducing `kind_convert` (a no-op except i32<->i64). `ck`
                // is computed first because the float-ordering guard below needs it.
                let lk = self.kind_of(lhs);
                let rk = self.kind_of(rhs);
                let ck = if lk == Kind::F64 || rk == Kind::F64 {
                    Kind::F64
                } else if lk == Kind::I64 || rk == Kind::I64 {
                    Kind::I64
                } else {
                    Kind::I32
                };
                // A literal divisor proves the Wasm op cannot trap in the common
                // cases: remainder needs only nonzero; signed division also has
                // the `Int::MIN / -1` overflow edge. Keep those proven-safe ops
                // raw so tight arithmetic/index loops do not pay a helper call.
                let rhs_proves_nontrapping = match (*op, rhs.as_ref()) {
                    (BinOp::Mod, Expr::Int(n)) => *n != 0,
                    (BinOp::Div, Expr::Int(n)) => *n != 0 && *n != -1,
                    _ => false,
                };
                if self.collect_wir
                    && ck == Kind::I64
                    && matches!(op, BinOp::Div | BinOp::Mod)
                    && !rhs_proves_nontrapping
                {
                    let func = if *op == BinOp::Div { "int_div" } else { "int_rem" };
                    let lhs_w = Self::wir_convert(self.lower_expr(lhs)?, lk, ck);
                    let rhs_w = Self::wir_convert(self.lower_expr(rhs)?, rk, ck);
                    return Some(W::Call { func: func.into(), args: vec![lhs_w, rhs_w] });
                }
                // The plain numeric path only. Every special case returns `None` so
                // the legacy arm keeps its exact emission.
                let wop = match op {
                    // `Add` is string concat when either operand is a `Str`.
                    BinOp::Add
                        if self.val_type_of(lhs) == ValType::Str
                            || self.val_type_of(rhs) == ValType::Str =>
                    {
                        return None;
                    }
                    BinOp::Add => witchy_wir::wir::BinOp::Add,
                    BinOp::Sub => witchy_wir::wir::BinOp::Sub,
                    BinOp::Mul => witchy_wir::wir::BinOp::Mul,
                    BinOp::Div => witchy_wir::wir::BinOp::Div,
                    BinOp::Mod => witchy_wir::wir::BinOp::Rem,
                    BinOp::BitAnd => witchy_wir::wir::BinOp::And,
                    BinOp::BitOr => witchy_wir::wir::BinOp::Or,
                    BinOp::BitXor => witchy_wir::wir::BinOp::Xor,
                    BinOp::Shl => witchy_wir::wir::BinOp::Shl,
                    BinOp::Shr => witchy_wir::wir::BinOp::Shr,
                    BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                        // String compares ($str_eq/$str_cmp), the structural eq
                        // helper for compounds, the loud rejects (dict ==, compound
                        // ordering, generic-reference compares), and float *ordering*
                        // ($f_lt/$f_le/$f_gt/$f_ge, NaN-trapping — not f64.lt) all
                        // keep their bespoke legacy emission.
                        if self.val_type_of(lhs) == ValType::Str
                            || self.val_type_of(rhs) == ValType::Str
                            || self.operand_is_compound(lhs)
                            || self.operand_is_compound(rhs)
                            || self.is_dict_operand(lhs)
                            || self.is_dict_operand(rhs)
                            || self.is_generic_ref_compare(lhs, rhs)
                            || (ck == Kind::F64
                                && matches!(op, BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq))
                        {
                            return None;
                        }
                        match op {
                            BinOp::Eq => witchy_wir::wir::BinOp::Eq,
                            BinOp::NotEq => witchy_wir::wir::BinOp::Ne,
                            BinOp::Lt => witchy_wir::wir::BinOp::Lt,
                            BinOp::LtEq => witchy_wir::wir::BinOp::Le,
                            BinOp::Gt => witchy_wir::wir::BinOp::Gt,
                            BinOp::GtEq => witchy_wir::wir::BinOp::Ge,
                            _ => unreachable!(),
                        }
                    }
                    // concat / `&&` / `||` keep their legacy emission.
                    _ => return None,
                };
                let lhs_w = Self::wir_convert(self.lower_expr(lhs)?, lk, ck);
                let rhs_w = Self::wir_convert(self.lower_expr(rhs)?, rk, ck);
                W::Binary { op: wop, kind: Self::wir_kind(ck), lhs: Box::new(lhs_w), rhs: Box::new(rhs_w) }
            }
            // Tuple element (`pair.0`) or record field (`rec.name`): both read an
            // i64 slot at `base + 4 + 8*idx`, recovered at the field's kind. The
            // legacy emission is `base; i32.const off; i32.add; i64.load;
            // from_slot(k)` — reproduced as `FromSlot(Load{Add(base, off)}, k)`.
            Expr::Field { base, field } => {
                if self.specialized_layout_of_expr(base).is_some()
                    || matches!(base.as_ref(), Expr::Call { name, args }
                        if name == intrinsics::LIST_AT
                            && args.first().and_then(|arg| self.specialized_layout_of_expr(arg)).is_some())
                {
                    return self.lower_specialized_field(base, field);
                }
                if let Ok(index) = field.parse::<usize>()
                    && let Some(ty) = self.ast_type_of_expr(base)
                    && let Some(shape) = self.gc_tuple_shape(&ty)
                    && let Some(struct_id) = self.gc_tuple_ids.get(&shape).copied()
                {
                    let Type::Tuple(items) = ty.unqualified() else {
                        return None;
                    };
                    if index >= items.len() {
                        return None;
                    }
                    return Some(W::StructGet {
                        struct_id,
                        field: index as u32,
                        base: Box::new(self.lower_expr(base)?),
                    });
                }
                // (RFC-0027 packed) `list.at(xs, i).field` on a packed record-list
                // reads the inline slot directly — element `i`, field `j` lives at
                // `xs + 4 + (i*nfields + j)*8`, the same per-field i64-slot rep a
                // boxed record uses, just flattened. One load instead of a pointer
                // deref + a field load. Only for names the `let` actually packed.
                if let Expr::Call { name: at, args } = base.as_ref() {
                    if at == intrinsics::LIST_AT && args.len() == 2 {
                        if let Expr::Var(xs) = &args[0] {
                            if let Some(rec) = self.packed_active.get(xs).cloned() {
                                let names = self.record_fields.get(&rec)?;
                                let nfields = names.len();
                                let j = names.iter().position(|(n, _)| n == field)?;
                                let fkind = name_kind(names[j].1.as_deref());
                                let ik = self.kind_of(&args[1]);
                                let idx = Self::wir_convert(self.lower_expr(&args[1])?, ik, Kind::I32);
                                // addr = xs + (4 + j*8) + i*(nfields*8)
                                let row = W::Binary {
                                    op: witchy_wir::wir::BinOp::Mul,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(idx),
                                    rhs: Box::new(W::ConstI32((nfields * 8) as i32)),
                                };
                                let base_off = W::Binary {
                                    op: witchy_wir::wir::BinOp::Add,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(W::GetLocal(xs.clone())),
                                    rhs: Box::new(W::ConstI32((4 + j * 8) as i32)),
                                };
                                let addr = W::Binary {
                                    op: witchy_wir::wir::BinOp::Add,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(base_off),
                                    rhs: Box::new(row),
                                };
                                let read = W::FromSlot(
                                    Box::new(W::Load {
                                        ptr: Box::new(addr),
                                        kind: witchy_wir::wir::Kind::I64,
                                        offset: 0,
                                    }),
                                    Self::wir_kind(fkind),
                                );
                                // (RFC-0037 §3) Under WITCHY_TYPE_CHECK, verify the flat buffer's
                                // `packed:` tag before the inline read — catching a boxed/packed
                                // layout confusion at the access site. `xs` is a var, so no temp.
                                if witchy_wir::wir_helpers::type_check_enabled() {
                                    use witchy_wir::wir::{BinOp, Kind as K, WirNode as N};
                                    let expected = type_tag_of(&format!("packed:{rec}"));
                                    let tag = || W::Binary {
                                        op: BinOp::ShrU,
                                        kind: K::I32,
                                        lhs: Box::new(W::Load {
                                            ptr: Box::new(W::Binary { op: BinOp::Sub, kind: K::I32, lhs: Box::new(W::GetLocal(xs.clone())), rhs: Box::new(W::ConstI32(4)) }),
                                            kind: K::I32,
                                            offset: 0,
                                        }),
                                        rhs: Box::new(W::ConstI32(24)),
                                    };
                                    let mismatch = W::Binary {
                                        op: BinOp::And,
                                        kind: K::I32,
                                        lhs: Box::new(W::Binary { op: BinOp::Ne, kind: K::I32, lhs: Box::new(tag()), rhs: Box::new(W::ConstI32(0)) }),
                                        rhs: Box::new(W::Binary { op: BinOp::Ne, kind: K::I32, lhs: Box::new(tag()), rhs: Box::new(W::ConstI32(expected as i32)) }),
                                    };
                                    return Some(W::Seq(vec![
                                        N::If { cond: mismatch, then_: vec![N::Unreachable], els: vec![], result: None },
                                        N::Push(read),
                                    ]));
                                }
                                return Some(read);
                            }
                        }
                    }
                }
                // (RFC-0027) A scalar-replaced aggregate's field is read straight
                // from its `${p}$<i>` slot local — no heap load. Only for names the
                // `let` actually replaced (`sroa_active`); a `let` precedes its uses
                // in statement order, so the set is populated first.
                if let Expr::Var(p) = base.as_ref() {
                    if self.sroa_active.contains_key(p) {
                        let (idx, kind) = if let Ok(i) = field.parse::<usize>() {
                            (i, valtype_kind(self.val_type_of(e)))
                        } else {
                            let base_ty = self.record_type_of(base)?;
                            let names = self.record_fields.get(&base_ty)?;
                            let i = names.iter().position(|(n, _)| n == field)?;
                            (i, name_kind(names[i].1.as_deref()))
                        };
                        return Some(W::FromSlot(
                            Box::new(W::GetLocal(format!("{p}${idx}"))),
                            Self::wir_kind(kind),
                        ));
                    }
                }
                if let Some(base_ty) = self.record_type_of(base) {
                    if let Some(struct_id) =
                        self.ast_type_of_expr(base).and_then(|ty| self.gc_struct_id_for_type(&ty))
                    {
                        let names = self.record_fields.get(&base_ty)?;
                        let idx = names.iter().position(|(n, _)| n == field)?;
                        return Some(W::StructGet {
                            struct_id,
                            field: idx as u32,
                            base: Box::new(self.lower_expr(base)?),
                        });
                    }
                }
                let (offset, kind) = if let Ok(i) = field.parse::<usize>() {
                    (4 + 8 * i, valtype_kind(self.val_type_of(e)))
                } else {
                    let base_ty = self.record_type_of(base)?;
                    let names = self.record_fields.get(&base_ty)?;
                    let idx = names.iter().position(|(n, _)| n == field)?;
                    (4 + 8 * idx, name_kind(names[idx].1.as_deref()))
                };
                // (RFC-0037 §3) Under WITCHY_TYPE_CHECK, verify the record pointer's type tag
                // (the alloc-header high byte at p-4) matches its statically-known type before
                // the field load — trapping a layout / `unbox` confusion AT the access site.
                // Tolerant of an untagged pointer (tag 0), so a value built by a not-yet-tagged
                // path never false-traps; a genuine mismatch (tag != 0 and != expected) traps.
                if witchy_wir::wir_helpers::type_check_enabled() {
                    if let Some(expected) = self.record_type_of(base).map(|t| type_tag_of(&t)) {
                        use witchy_wir::wir::{BinOp, Kind as K, WirNode as N};
                        let base_ptr = self.lower_expr(base)?;
                        let tmp = || W::GetLocal(TYPECHECK_TMP.to_string());
                        let tag = || W::Binary {
                            op: BinOp::ShrU,
                            kind: K::I32,
                            lhs: Box::new(W::Load {
                                ptr: Box::new(W::Binary { op: BinOp::Sub, kind: K::I32, lhs: Box::new(tmp()), rhs: Box::new(W::ConstI32(4)) }),
                                kind: K::I32,
                                offset: 0,
                            }),
                            rhs: Box::new(W::ConstI32(24)),
                        };
                        let mismatch = W::Binary {
                            op: BinOp::And,
                            kind: K::I32,
                            lhs: Box::new(W::Binary { op: BinOp::Ne, kind: K::I32, lhs: Box::new(tag()), rhs: Box::new(W::ConstI32(0)) }),
                            rhs: Box::new(W::Binary { op: BinOp::Ne, kind: K::I32, lhs: Box::new(tag()), rhs: Box::new(W::ConstI32(expected as i32)) }),
                        };
                        let read = W::FromSlot(
                            Box::new(W::Load {
                                ptr: Box::new(W::Binary { op: BinOp::Add, kind: K::I32, lhs: Box::new(tmp()), rhs: Box::new(W::ConstI32(offset as i32)) }),
                                kind: K::I64,
                                offset: 0,
                            }),
                            Self::wir_kind(kind),
                        );
                        return Some(W::Seq(vec![
                            N::SetLocal { local: TYPECHECK_TMP.to_string(), value: base_ptr },
                            N::If { cond: mismatch, then_: vec![N::Unreachable], els: vec![], result: None },
                            N::Push(read),
                        ]));
                    }
                }
                let addr = W::Binary {
                    op: witchy_wir::wir::BinOp::Add,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(self.lower_expr(base)?),
                    rhs: Box::new(W::ConstI32(offset as i32)),
                };
                W::FromSlot(
                    Box::new(W::Load {
                        ptr: Box::new(addr),
                        kind: witchy_wir::wir::Kind::I64,
                        offset: 0,
                    }),
                    Self::wir_kind(kind),
                )
            }
            // `e?`: store the operand once, then a value-`if` on its tag — take the
            // success payload (tag 0, at `tmp+4`) or early-`return` the whole
            // Err/None. The `var` epilogue variant stays in legacy.
            Expr::Try(inner) => {
                use witchy_wir::wir::WirNode as N;
                if let Some(shape) = self.reference_try_shape(inner) {
                    let source_ty = self.ast_type_of_expr(inner)?;
                    let Type::Named(family, _) = source_ty.unqualified() else {
                        return None;
                    };
                    let (aggregate_kind, payload_kind, tmp, cond, payload, error) = match shape {
                        ReferenceTryShape::Nullable { payload_kind } => {
                            let tmp = Self::reference_try_tmp(payload_kind)?;
                            (
                                payload_kind,
                                payload_kind,
                                tmp.clone(),
                                W::Unary {
                                    op: witchy_wir::wir::UnOp::Not,
                                    kind: witchy_wir::wir::Kind::I32,
                                    arg: Box::new(W::RefIsNull(Box::new(W::GetLocal(
                                        tmp.clone(),
                                    )))),
                                },
                                W::GetLocal(tmp),
                                None,
                            )
                        }
                        ReferenceTryShape::Tagged {
                            struct_id,
                            success_tag,
                            payload_field,
                            payload_kind,
                            failure_field,
                            failure_kind,
                        } => {
                            let tmp = Self::reference_try_tmp(Kind::GcRef(struct_id))?;
                            let error = failure_field.zip(failure_kind).map(
                                |(field, kind)| {
                                    (
                                        W::StructGet {
                                            struct_id,
                                            field,
                                            base: Box::new(W::GetLocal(tmp.clone())),
                                        },
                                        kind,
                                    )
                                },
                            );
                            (
                                Kind::GcRef(struct_id),
                                payload_kind,
                                tmp.clone(),
                                W::Binary {
                                    op: witchy_wir::wir::BinOp::Eq,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(W::StructGet {
                                        struct_id,
                                        field: 0,
                                        base: Box::new(W::GetLocal(tmp.clone())),
                                    }),
                                    rhs: Box::new(W::ConstI32(success_tag as i32)),
                                },
                                W::StructGet {
                                    struct_id,
                                    field: payload_field,
                                    base: Box::new(W::GetLocal(tmp.clone())),
                                },
                                error,
                            )
                        }
                    };
                    let inner_w = self.lower_expr(inner)?;
                    let zero = match payload_kind {
                        Kind::I64 => W::ConstI64(0),
                        Kind::F64 => W::ConstF64(0.0),
                        Kind::I32 => W::ConstI32(0),
                        Kind::ExternRef => {
                            W::RefNull(witchy_wir::wir::Kind::ExternRef)
                        }
                        Kind::GcRef(id) => {
                            W::RefNull(witchy_wir::wir::Kind::GcRef(id))
                        }
                    };
                    let (failure_value, failure_kind) = self
                        .try_failure_value(family, error)
                        .or_else(|| {
                            (aggregate_kind == self.cur_fn_ret_kind).then(|| {
                                (W::GetLocal(tmp.clone()), aggregate_kind)
                            })
                        })?;
                    let mut failure =
                        self.try_early_return_nodes(failure_value, failure_kind);
                    failure.push(N::Push(zero));
                    return Some(W::Seq(vec![
                        N::SetLocal { local: tmp, value: inner_w },
                        N::If {
                            cond,
                            then_: vec![N::Push(payload)],
                            els: failure,
                            result: Some(Self::wir_ty_for_kind(payload_kind)),
                        },
                    ]));
                }
                let payload_kind = self
                    .ast_type_of_expr(e)
                    .map(|ty| self.kind_for_type(&ty))
                    .or_else(|| self.match_payload_valtype(inner).map(valtype_kind))
                    .unwrap_or(Kind::I32);
                let inner_w = self.lower_expr(inner)?;
                let tmp = TRY_TMP.to_string();
                let cond = W::Unary {
                    op: witchy_wir::wir::UnOp::Not,
                    kind: witchy_wir::wir::Kind::I32,
                    arg: Box::new(W::Load {
                        ptr: Box::new(W::GetLocal(tmp.clone())),
                        kind: witchy_wir::wir::Kind::I32,
                        offset: 0,
                    }),
                };
                let payload = W::FromSlot(
                    Box::new(W::Load {
                        ptr: Box::new(W::Binary {
                            op: witchy_wir::wir::BinOp::Add,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(W::GetLocal(tmp.clone())),
                            rhs: Box::new(W::ConstI32(4)),
                        }),
                        kind: witchy_wir::wir::Kind::I64,
                        offset: 0,
                    }),
                    Self::wir_kind(payload_kind),
                );
                let zero = match payload_kind {
                    Kind::I64 => W::ConstI64(0),
                    Kind::F64 => W::ConstF64(0.0),
                    Kind::I32 => W::ConstI32(0),
                    Kind::ExternRef => W::RefNull(witchy_wir::wir::Kind::ExternRef),
                    Kind::GcRef(id) => W::RefNull(witchy_wir::wir::Kind::GcRef(id)),
                };
                // The Err path carries the whole aggregate and every var/own
                // write-back through the function's existing result envelope.
                let source_ty = self.ast_type_of_expr(inner)?;
                let Type::Named(family, args) = source_ty.unqualified() else {
                    return None;
                };
                let error = if family == "Result" {
                    let error_kind = self.kind_for_type(args.get(1)?);
                    Some((
                        W::FromSlot(
                            Box::new(W::Load {
                                ptr: Box::new(W::Binary {
                                    op: witchy_wir::wir::BinOp::Add,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(W::GetLocal(tmp.clone())),
                                    rhs: Box::new(W::ConstI32(4)),
                                }),
                                kind: witchy_wir::wir::Kind::I64,
                                offset: 0,
                            }),
                            Self::wir_kind(error_kind),
                        ),
                        error_kind,
                    ))
                } else {
                    None
                };
                let (failure_value, failure_kind) = self
                    .try_failure_value(family, error)
                    .or_else(|| {
                        (self.cur_fn_ret_kind == Kind::I32).then(|| {
                            (W::GetLocal(tmp.clone()), Kind::I32)
                        })
                    })?;
                let mut els =
                    self.try_early_return_nodes(failure_value, failure_kind);
                // Unreachable, but keeps the `els` branch stack-typed at payload_kind.
                els.push(N::Push(zero));
                W::Seq(vec![
                    N::SetLocal { local: tmp.clone(), value: inner_w },
                    N::If {
                        cond,
                        then_: vec![N::Push(payload)],
                        els,
                        result: Some(Self::wir_ty_for_kind(payload_kind)),
                    },
                ])
            }
            // A call expression. Builtins/natives that WIR can lower flow through
            // `lower_call`; otherwise a plain top-level user call (no own-ABI
            // token, no `var` writeback, not a closure-typed local) lowers via
            // `try_lower_user_call`. The arm precedence here (builtin/native first,
            // then direct user call, then closure-local) is the call dispatch; any
            // other call shape (own-ABI, `var`) returns `None` and is rejected.
            Expr::Call { name, args } => {
                // Only a WIR-collecting scope lowers calls; otherwise bail so the
                // construct is reported unsupported. `lower_call` owns the
                // builtin/native arm precedence (e.g. `math.sqrt` is an intrinsic,
                // not a `$`-func), which this arm does not re-derive.
                if !self.collect_wir {
                    return None;
                }
                if let Some(w) = self.lower_call(name, args) {
                    return Some(w);
                }
                // (RFC-0062 tier-1) An ELIDED closure local `f(x)`: no env exists — thread
                // the captures (from their current locals) as leading i64 arg slots, then
                // the value args, to a direct `call $__lamt{i}`. Checked BEFORE the boxed
                // closure paths.
                if let Some((idx, caps)) = self.thread_index.get(name).cloned() {
                    let access = self.call_access_signature(e)?.clone();
                    let param_kinds = access
                        .params()
                        .iter()
                        .map(|param| self.kind_for_type(param.ty()))
                        .collect::<Vec<_>>();
                    let rk = self.kind_for_type(access.result().ty());
                    let typed_abi = Self::closure_uses_typed_abi(&param_kinds, rk);
                    let ownership = Self::ownership_envelope_for_signature(&access);
                    let mut call_args: Vec<W> = caps
                        .iter()
                        .map(|(cn, ck)| {
                            let value = W::GetLocal(cn.clone());
                            if ck.is_ref() {
                                value
                            } else {
                                W::ToSlot(Box::new(value), Self::wir_kind(*ck))
                            }
                        })
                        .collect();
                    let (arg_slots, writebacks, capacity_dests) = self.lower_closure_args(
                        args,
                        &access,
                        &param_kinds,
                        typed_abi,
                        &ownership,
                    )?;
                    call_args.extend(arg_slots);
                    if writebacks.is_empty() && !ownership.has_state() {
                        let call = W::Call { func: format!("__lamt{idx}"), args: call_args };
                        return Some(if typed_abi {
                            call
                        } else {
                            W::FromSlot(Box::new(call), Self::wir_kind(rk))
                        });
                    }
                    let dests = Self::closure_call_dests(
                        rk,
                        typed_abi,
                        &writebacks,
                        &capacity_dests,
                        &ownership,
                    );
                    let call = N::CallStoreMulti {
                        func: format!("__lamt{idx}"),
                        args: call_args,
                        dests,
                    };
                    return self.finish_closure_multi_call(
                        call,
                        writebacks,
                        rk,
                        typed_abi,
                        false,
                    );
                }
                // A closure-typed local `f(x)`: pass the wrapper as the implicit
                // environment, then signature-shaped args, and call indirectly on
                // its immutable code field. The wrapper is a bare `GetLocal`, so
                // no scratch stash is needed.
                if self.locals.contains_key(name) {
                    let n = args.len();
                    let access = self.call_access_signature(e)?.clone();
                    let param_kinds = access
                        .params()
                        .iter()
                        .map(|param| self.kind_for_type(param.ty()))
                        .collect::<Vec<_>>();
                    let rk = self.kind_for_type(access.result().ty());
                    let typed_abi = Self::closure_uses_typed_abi(&param_kinds, rk);
                    let ownership = Self::ownership_envelope_for_signature(&access);
                    let mut ci_args: Vec<W> = vec![W::GetLocal(name.to_string())];
                    let (arg_slots, writebacks, capacity_dests) = self.lower_closure_args(
                        args,
                        &access,
                        &param_kinds,
                        typed_abi,
                        &ownership,
                    )?;
                    ci_args.extend(arg_slots);
                    self.clos_arities.insert(n);
                    // (RFC-0034 L3) Devirtualize when `name` is a single-bound, never-
                    // reassigned closure local (`devirt_index`): a direct `call
                    // $__lamw{i}` — same env (the closure pointer) and args, just
                    // skipping the runtime code-index load — which also lets the
                    // Binaryen pass inline the lambda body into the caller.
                    if !writebacks.is_empty() || ownership.has_state() {
                        let dests = Self::closure_call_dests(
                            rk,
                            typed_abi,
                            &writebacks,
                            &capacity_dests,
                            &ownership,
                        );
                        let signature = Self::closure_signature(
                            n,
                            &param_kinds,
                            rk,
                            &writebacks,
                            typed_abi,
                            &ownership,
                        );
                        let indirect_ownership = !self.devirt_index.contains_key(name);
                        let call = if let Some(&idx) = self.devirt_index.get(name) {
                            N::CallStoreMulti {
                                func: format!("__lamw{idx}"),
                                args: ci_args,
                                dests,
                            }
                        } else {
                            N::CallIndirectStoreMulti {
                                signature,
                                args: ci_args,
                                index: W::StructGet {
                                    struct_id: CLOSURE_WRAPPER_ID,
                                    field: witchy_wir::wir::CLOSURE_CODE_FIELD,
                                    base: Box::new(W::GetLocal(name.to_string())),
                                },
                                dests,
                            }
                        };
                        return self.finish_closure_multi_call(
                            call,
                            writebacks,
                            rk,
                            typed_abi,
                            indirect_ownership,
                        );
                    }
                    let call = if let Some(&idx) = self.devirt_index.get(name) {
                        W::Call { func: format!("__lamw{idx}"), args: ci_args }
                    } else {
                        W::CallIndirect {
                            signature: Self::closure_signature(
                                n,
                                &param_kinds,
                                rk,
                                &[],
                                typed_abi,
                                &ownership,
                            ),
                            args: ci_args,
                            index: Box::new(W::StructGet {
                                struct_id: CLOSURE_WRAPPER_ID,
                                field: witchy_wir::wir::CLOSURE_CODE_FIELD,
                                base: Box::new(W::GetLocal(name.to_string())),
                            }),
                        }
                    };
                    return Some(if typed_abi {
                        call
                    } else {
                        W::FromSlot(Box::new(call), Self::wir_kind(rk))
                    });
                }
                let call_access = self.call_access_signature(e).cloned();
                let has_var = call_access.as_ref().is_some_and(|signature| {
                    signature.params().iter().any(|param| {
                        param.kind() == witchy_types::access::AccessKind::ExclusiveWriteback
                    })
                });
                // An `var` user call: the callee returns its declared value plus one
                // result per var param (the multi-value move-out ABI). Lower to a
                // `CallStoreMulti` that writes each var result back into the caller's
                // local var, then yield the declared value.
                if has_var
                    && self.emitted_funcs.contains(name)
                    && !self.locals.contains_key(name)
                    && self.summaries.own_abi(name).is_none()
                {
                    let result_kind = self
                        .ast_type_of_expr(e)
                        .map(|ty| self.kind_for_type(&ty))
                        .unwrap_or_else(|| self.kind_of(e));
                    return self.lower_var_call(name, args, result_kind, call_access.as_ref()?);
                }
                // Exactly the compiled `$name` user functions — never an
                // intrinsic/native (those have no emitted func to call), never a
                // closure-typed local (that's a `call_indirect`).
                let is_plain_user_fn = self.emitted_funcs.contains(name)
                    && !self.locals.contains_key(name)
                    && !self.local_fn_ret_kind.contains_key(name);
                if is_plain_user_fn && !has_var {
                    return self.try_lower_user_call(name, args, call_access.as_ref()?);
                }
                return None;
            }
            _ => return None,
        })
    }
}
