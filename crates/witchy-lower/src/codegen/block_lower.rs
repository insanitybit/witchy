//! Block statement lowering: the `Codegen` methods that lower an AST `Block`
//! to a WIR sequence — `lower_block` (the transactional, uniqueness-facts-
//! accounting entry point) and its `lower_block_inner` workhorse. Split out of
//! `codegen/mod.rs` as a continuation of the `Codegen` impl; behavior is
//! unchanged.

use super::*;

impl<'types> Codegen<'types> {
    /// Lower a SIMPLE block to a `WirSeq`. Only functions without in-place/cap
    /// machinery qualify — no `inplace_push` vars, no `var` params, no own-ABI
    /// param — and only `Let`/`Expr`/`Return` statements; any other shape (the
    /// cap-kill, dict/list fast-path, tuple-destructure, and break/continue cases)
    /// bails to `None`, rejecting the program as not-yet-lowerable.
    ///
    /// Statements are pre-lowered (idempotent `intern`/flag mutations) so that a
    /// non-lowerable expression bails BEFORE any `take_kills` call — `take_kills`
    /// bumps a non-idempotent kill counter, so double-running it would corrupt the
    /// uniqueness accounting.
    /// Lower a block, with TRANSACTIONAL uniqueness-facts accounting: snapshot the
    /// `(kills, sites)` counters on entry and RESTORE them if lowering bails
    /// (`None`). A nested loop-body block may succeed and consume its sites, but if
    /// the enclosing block then bails, the whole tree rolls back so a later attempt
    /// re-consumes from a clean slate (no double-count). Commit (no restore) happens
    /// only on `Some` — the whole block lowered.
    pub(crate) fn lower_block(&mut self, block: &Block) -> Option<witchy_wir::wir::WirSeq> {
        let snap = self.facts_stack.last().map(|(_, k, s)| (*k, *s));
        let saved_loans = std::mem::take(&mut self.active_loan_events);
        let result = self.lower_block_inner(block);
        self.active_loan_events = saved_loans;
        if result.is_none() {
            if let (Some((k, s)), Some(top)) = (snap, self.facts_stack.last_mut()) {
                top.1 = k;
                top.2 = s;
            }
        }
        result
    }

    fn lower_block_inner(&mut self, block: &Block) -> Option<witchy_wir::wir::WirSeq> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        // In-place accumulators lower to the cap ABI (`$list_push_cap` via
        // CallStoreMulti) only in a WIR-collecting scope (`collect_wir`); otherwise
        // this bails. Facts consumption is deferred to `compile_function` on capture
        // (lower_block is invoked many times per compile). The own-ABI never lowers
        // here. `var` lowers ONLY in a WIR-collecting scope (`collect_wir`): the
        // param is a plain mutable local (`n = n + 1` is a `SetLocal`) and its final
        // value is a multi-result at the tail (built by `assemble_wir_func`), the
        // call site writing it back via `CallStoreMulti`. A non-collecting scope
        // can't carry that move-out epilogue (it must run before EVERY early
        // `return`, which the WIR `N::Return` single value can't express), so it
        // bails — leaving the program to be rejected as unsupported.
        if !self.collect_wir
            && (!self.cur_fn_var_params.is_empty()
                || self.cur_fn_own_param.is_some()
                || !self.inplace_push.is_empty())
        {
            return None;
        }
        let mut inplace_sites = 0usize;
        let last = block.stmts.len().saturating_sub(1);
        let mut seq: witchy_wir::wir::WirSeq = Vec::with_capacity(block.stmts.len() + 1);
        let mut tail_is_value = false;
        let tail_is_terminal = matches!(block.stmts.last(), Some(Stmt::Return(_)));
        for (i, stmt) in block.stmts.iter().enumerate() {
            // Public collection intrinsics still lower to their value-producing
            // implementation internally. In RFC-0087 statement form, feed that
            // value into the existing self-assignment machinery so the receiver
            // is written back instead of dropping the updated collection.
            let assignment_name = match stmt {
                Stmt::Assign { name, .. } => Some(name.as_str()),
                Stmt::Expr(value) => {
                    let uniform_var_call = matches!(value, Expr::Call { name, .. }
                        if self.fn_conventions.get(name).is_some_and(|conventions|
                            conventions.contains(&Convention::Var)));
                    (!uniform_var_call)
                        .then(|| analysis::direct_inplace_root(value))
                        .flatten()
                }
                _ => None,
            };
            let analyzed_stmt = stmt;
            let stmt_start = seq.len();
            self.active_loan_events = self.loan_facts.active_at(analyzed_stmt).to_vec();
            if let Some(key) = self.loan_facts.event_key(analyzed_stmt)
                && let Some((_, seen)) = self.loan_fact_stack.last_mut()
            {
                seen.insert(key);
            }
            match stmt {
                Stmt::Let { name, value, .. } => {
                    // (RFC-0035 step 3) If this binds a dup-eligible container read
                    // (`let x = list.at(xs, i)` where the element is a provably offset-0 rc
                    // value and rc-floor is on), the read is `$rc_dup`'d in `lower_expr`, so
                    // `x` owns a reference. Record it under the SAME gate as the dup so its
                    // last-use `$rc_drop` (below) fires iff the dup did — a never-dup'd
                    // binding is never dropped (which would underflow the count → UAF).
                    if !force_copy_mode() && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::RcFloor) {
                        if let Expr::Call { name: f, args } = value {
                            if f == intrinsics::LIST_AT
                                && args.len() == 2
                                && self.kind_of(value) == Kind::I32
                                && self.list_elem_is_offset0_rc(&args[0])
                            {
                                self.rc_owned_bindings.insert(name.clone());
                            }
                        }
                    }
                    // (RFC-0027) Scalar-replace a frame-confined aggregate: store
                    // each field into a `${name}$<i>` i64-slot local instead of
                    // allocating a heap object. Falls through to the normal path if
                    // any field can't lower (then the name never enters sroa_active,
                    // so its field reads stay heap loads — consistent).
                    // (RFC-0027) Pack a confined list literal of fixed-scalar records
                    // into one flat inline buffer: header = element COUNT (so
                    // `list.length` is unchanged), body = every element's fields in
                    // row-major order. Reuses the checked-heap-correct `$mkN` allocator.
                    let mut scalar_sum_done = false;
                    if self.scalar_sum_candidates.contains(name)
                        && let Some((layout, nodes)) =
                            self.lower_confined_scalar_sum_binding(name, value)
                    {
                        seq.extend(nodes);
                        self.scalar_sum_active.insert(name.clone(), layout);
                        scalar_sum_done = true;
                    }
                    let mut packed_done = false;
                    if !scalar_sum_done && self.packed_candidates.contains(name) {
                        if let Expr::List(items) = value {
                            if let Some((rec, flat)) = self.packable_record_list(items) {
                                // (RFC-0037 §3) Tag the flat buffer with a DISTINCT `packed:` id
                                // so reading it as a boxed record (or vice versa) is a mismatch.
                                let ptag = type_tag_of(&format!("packed:{rec}"));
                                if let Some(w) = self.lower_aggregate(items.len() as i32, &flat, ptag) {
                                    seq.push(N::SetLocal { local: name.clone(), value: w });
                                    self.packed_active.insert(name.clone(), rec);
                                    packed_done = true;
                                }
                            }
                        }
                    }
                    let mut view_done = false;
                    if !scalar_sum_done && !packed_done && self.view_candidates.contains(name) {
                        if let Some((src, lo, hi)) = view_slice_args(value) {
                            // Elide the copy: store the source pointer and the raw
                            // lo/hi bounds (evaluated once, as the materialized slice
                            // would), then read through them. The analysis guarantees
                            // `src` is an unmutated var, so its pointer stays valid.
                            let srcw = self.lower_expr(src)?;
                            let lok = self.kind_of(lo);
                            let low = self.lower_expr(lo)?;
                            let hik = self.kind_of(hi);
                            let hiw = self.lower_expr(hi)?;
                            seq.push(N::SetLocal { local: format!("{name}$src"), value: srcw });
                            seq.push(N::SetLocal {
                                local: format!("{name}$lo"),
                                value: Self::wir_convert(low, lok, Kind::I32),
                            });
                            seq.push(N::SetLocal {
                                local: format!("{name}$hi"),
                                value: Self::wir_convert(hiw, hik, Kind::I32),
                            });
                            // Carry the source's element typing onto the view name so
                            // `list.at(w, _)` recovers the element kind for `FromSlot`.
                            if let Expr::Var(s) = src {
                                if let Some(vt) = self.local_list_elem_valtype.get(s).copied() {
                                    self.local_list_elem_valtype.insert(name.clone(), vt);
                                }
                                if let Some(r) = self.local_list_elem.get(s).cloned() {
                                    self.local_list_elem.insert(name.clone(), r);
                                }
                                if let Some(t) = self.local_list_elem_tuple.get(s).cloned() {
                                    self.local_list_elem_tuple.insert(name.clone(), t);
                                }
                            }
                            self.view_active.insert(name.clone());
                            view_done = true;
                        }
                    }
                    let mut sroa_done = false;
                    if !scalar_sum_done
                        && !packed_done
                        && !view_done
                        && self.sroa_candidates.contains(name)
                    {
                        // (RFC-0005 stage 4) A cap-carrying (GC-lowered) record has
                        // reference-typed fields with no i64 slot form — never SROA
                        // it; the plain path binds it as one `GcRef` local.
                        let ref_field = sroa_fields(value).is_some_and(|args| {
                            args.iter()
                                .any(|a| matches!(self.kind_of(a), Kind::ExternRef | Kind::GcRef(_)))
                        });
                        if !ref_field && let Some(args) = sroa_fields(value) {
                            let mut stores = Vec::with_capacity(args.len());
                            let mut ok = true;
                            for (idx, arg) in args.iter().enumerate() {
                                let k = self.kind_of(arg);
                                if let Some(w) = self.lower_expr(arg) {
                                    stores.push(N::SetLocal {
                                        local: format!("{name}${idx}"),
                                        value: W::ToSlot(Box::new(w), Self::wir_kind(k)),
                                    });
                                } else {
                                    ok = false;
                                    break;
                                }
                            }
                            if ok {
                                let n = stores.len();
                                for s in stores {
                                    seq.push(s);
                                }
                                self.sroa_active.insert(name.clone(), n);
                                sroa_done = true;
                            }
                        }
                    }
                    // (RFC-0062 tier-1) Closure escape elision: `let f = <lambda>` where `f`
                    // is bound once (`devirt_ok`) and never escapes (`closure_elide_called`,
                    // i.e. used ONLY as a direct-call callee this unit). `lower_lambda_threaded`
                    // additionally rejects (→ boxed fallback) any lambda whose captures are
                    // reassigned. On success it registers the threaded lifted body and records
                    // the capture list in `thread_index`, and NOTHING is emitted for the binding
                    // — no `mk{n}` env allocation, no `SetLocal name`. The captures stay in their
                    // existing locals and are threaded to each `call $__lamt{i}` (see the call
                    // arms). Comes before the default `SetLocal` and suppresses it.
                    let mut closure_elide_done = false;
                    if !scalar_sum_done
                        && !packed_done
                        && !view_done
                        && !sroa_done
                        && self.collect_wir
                        && self.devirt_ok.contains(name)
                        && self.closure_elide_called.contains(name)
                    {
                        if let Expr::Lambda { params, body: lbody, .. } = value {
                            let signature = (
                                self.closure_param_kinds(value),
                                self.apply_ret_kind(value),
                            );
                            let result_ty = self.closure_result_type(value);
                            let access = self.closure_access_signature(value);
                            let ownership = access
                                .as_ref()
                                .map(Self::ownership_envelope_for_signature)
                                .unwrap_or_default();
                            if let Some(caps) =
                                self.lower_lambda_threaded(
                                    params,
                                    lbody,
                                    &signature,
                                    result_ty.as_ref(),
                                    access.as_ref(),
                                    &ownership,
                                )
                            {
                                self.thread_index.insert(name.clone(), caps);
                                closure_elide_done = true;
                            }
                        }
                    }
                    if !scalar_sum_done
                        && !packed_done
                        && !view_done
                        && !sroa_done
                        && !closure_elide_done
                    {
                        let v = self.lower_expr(value)?;
                        seq.push(N::SetLocal { local: name.clone(), value: v });
                        // A fresh non-empty list literal is already uniquely owned
                        // with exactly its current length as capacity. Other
                        // accumulator bindings start at zero and re-own on their
                        // first structural update.
                        if self.collect_wir && self.inplace_push.contains(name) {
                            let initial_cap = match value {
                                Expr::List(items) => items.len() as i32,
                                _ => 0,
                            };
                            let initial_cap = if self.expression_returns_unique_capacity(value) {
                                W::GetLocal(UNIQUE_RESULT_CAP_TMP.to_string())
                            } else {
                                W::ConstI32(initial_cap)
                            };
                            seq.push(N::SetLocal {
                                local: format!("{name}__cap"),
                                value: initial_cap,
                            });
                        }
                        // (RFC-0034 L3) Record a devirtualizable closure local: `name`
                        // is bound exactly once and never reassigned (`devirt_ok`), and
                        // `lower_expr` just registered the lambda — so recover its lifted
                        // `$__lamw{i}` index. Later `name(x)` calls then emit a direct
                        // `call` instead of `call_indirect` (see the closure-call arms).
                        if self.collect_wir && self.devirt_ok.contains(name) {
                            if let Expr::Lambda { params, body, .. } = value {
                                let key = Self::lambda_content_key(&self.cur_fn_name, params, body);
                                if let Some(&idx) = self.lambda_wir_index.get(&key) {
                                    self.devirt_index.insert(name.clone(), idx);
                                }
                            }
                        }
                    }
                    tail_is_value = false;
                }
                Stmt::Expr(e) if assignment_name.is_none() => {
                    let v = self.lower_expr(e)?;
                    if i == last {
                        seq.push(N::Push(v));
                        tail_is_value = true;
                    } else {
                        seq.push(N::Drop(v));
                        tail_is_value = false;
                    }
                }
                Stmt::Return(opt) => {
                    let value = match opt {
                        Some(e) => {
                            let ek = self.kind_of(e);
                            let w = self.lower_expr(e)?;
                            if self.cur_fn_ret_slot {
                                W::ToSlot(Box::new(w), Self::wir_kind(ek))
                            } else {
                                Self::wir_convert(w, ek, self.cur_fn_ret_kind)
                            }
                        }
                        None if self.cur_fn_ret_slot => W::ConstI64(0),
                        None => match self.cur_fn_ret_kind {
                            Kind::I64 => W::ConstI64(0),
                            Kind::F64 => W::ConstF64(0.0),
                            Kind::I32 => W::ConstI32(0),
                            Kind::ExternRef => W::RefNull(witchy_wir::wir::Kind::ExternRef),
                            Kind::GcRef(id) => W::RefNull(witchy_wir::wir::Kind::GcRef(id)),
                        },
                    };
                    let active = self.active_loan_events.clone();
                    let cleanup = self.close_loan_nodes(&active);
                    let value = if cleanup.is_empty() {
                        // Keep the direct return expression intact so recursive
                        // calls remain visible to the WIR tail-call pass.
                        value
                    } else {
                        // Evaluate the return value while every referenced view is
                        // still rooted, then release roots before transferring
                        // control. Cleanup must never run before this evaluation.
                        let return_kind = if self.cur_fn_ret_slot {
                            Kind::I64
                        } else {
                            self.cur_fn_ret_kind
                        };
                        let return_tmp = call_result_tmp(return_kind);
                        seq.push(N::SetLocal { local: return_tmp.clone(), value });
                        seq.extend(cleanup);
                        W::GetLocal(return_tmp)
                    };
                    if self.cur_fn_var_params.is_empty()
                        && self.cur_fn_own_param.is_none()
                        && !self.cur_fn_unique_ret
                    {
                        seq.push(N::Return(Some(value)));
                    } else {
                        // An `var`/own-ABI function's early `return` must yield the
                        // full multi-result tuple — the declared value, then each
                        // var param's value, then the own-cap — matching
                        // `assemble_wir_func`'s tail ordering. Push them and use a
                        // bare `return` (WIR `N::Return(Some)` carries one value).
                        seq.push(N::Push(value));
                        if self.cur_fn_unique_ret {
                            let cap = opt
                                .as_ref()
                                .map(|expr| self.return_capacity_expr(expr))
                                .unwrap_or_else(|| W::ConstI32(0));
                            seq.push(N::Push(cap));
                        }
                        for name in &self.cur_fn_var_params {
                            let var = W::GetLocal(name.clone());
                            let var = if self.cur_fn_ret_slot {
                                let kind = self.locals.get(name).copied().unwrap_or(Kind::I32);
                                W::ToSlot(Box::new(var), Self::wir_kind(kind))
                            } else {
                                var
                            };
                            seq.push(N::Push(var));
                        }
                        for name in &self.cur_fn_var_cap_params {
                            seq.push(N::Push(W::GetLocal(format!("{name}__cap"))));
                        }
                        if let Some(p) = self.cur_fn_own_param.clone() {
                            let returns_own = matches!(opt, Some(Expr::Var(v)) if *v == p)
                                || matches!(opt, Some(Expr::Unary { op: UnOp::Move, expr })
                                    if matches!(expr.as_ref(), Expr::Var(v) if *v == p));
                            let cap = if returns_own {
                                W::GetLocal(format!("{p}__cap"))
                            } else {
                                W::ConstI32(0)
                            };
                            seq.push(N::Push(cap));
                        }
                        seq.push(N::Return(None));
                    }
                    tail_is_value = false;
                }
                // `let PAT = e` (RFC-0052): store the value once as an i64 SLOT in
                // MATCH_TMP, then emit the pattern's BINDINGS via the shared
                // `lower_pattern` — the same machinery `match` uses (a `let`
                // pattern is irrefutable, so its test condition is discarded; only
                // the binds run). `lower_pattern` reads the value as an i64 slot
                // (it `FromSlot`s to a pointer for tuples/ctors), so store via
                // `ToSlot` at the value's kind — exactly as `lower_match` does.
                // Handles nested tuples, ctor/record destructures, and list heads
                // uniformly, superseding the old flat-slot-only loop.
                Stmt::LetPattern { pattern, value } => {
                    let vk = self.kind_of(value);
                    let v = self.lower_expr(value)?;
                    let pat_ty = self.ast_type_of_expr(value);
                    let (_cond, binds) = if let Kind::GcRef(struct_id) = vk {
                        let local = match_gc_tmp(struct_id);
                        seq.push(N::SetLocal { local: local.clone(), value: v });
                        self.lower_gc_struct_pattern(
                            &W::GetLocal(local),
                            pattern,
                            struct_id,
                            pat_ty.as_ref(),
                        )?
                    } else if vk == Kind::ExternRef {
                        seq.push(N::SetLocal {
                            local: MATCH_REF_TMP.to_string(),
                            value: v,
                        });
                        self.lower_externref_pattern(
                            &W::GetLocal(MATCH_REF_TMP.to_string()),
                            pattern,
                            pat_ty.as_ref(),
                        )?
                    } else {
                        seq.push(N::SetLocal {
                            local: MATCH_TMP.to_string(),
                            value: W::ToSlot(Box::new(v), Self::wir_kind(vk)),
                        });
                        self.lower_pattern(
                            &W::GetLocal(MATCH_TMP.to_string()),
                            pattern,
                            pat_ty.as_ref(),
                        )?
                    };
                    seq.extend(binds);
                    tail_is_value = false;
                }
                // `break`/`continue` -> a `br` to the enclosing loop's exit/continue
                // label. Outside a loop -> `None` (legacy emits the loud error).
                Stmt::Break | Stmt::Continue => {
                    let (brk, cont) = {
                        let (b, c) = self.loop_labels.last()?;
                        (b.clone(), c.clone())
                    };
                    let label = if matches!(stmt, Stmt::Break) { brk } else { cont };
                    let target = label.strip_prefix('$').unwrap_or(&label).to_string();
                    seq.push(N::Br { target, cond: None });
                    tail_is_value = false;
                }
                // `x = value` — only the simplest case: a plain LOCAL reassignment
                // that is NOT a self-assign shape (no in-place fast path / site
                // accounting), a string/list state field, or a global. Those keep
                // their bespoke legacy emission.
                Stmt::Assign { value, .. } | Stmt::Expr(value) => {
                    let name = assignment_name?.to_string();
                    let name = &name;
                    // RFC-0111 unique-result destination passing. The escape
                    // proof says the old whole value cannot be observed after
                    // this explicit reassignment, the exact LayoutId comparison
                    // says caller and callee agree on every physical byte, and
                    // the checked access signature supplies the complete physical
                    // ownership envelope. Statement-form mutators and any call
                    // with own/var state retain the ordinary path.
                    let destination_call = match value {
                        Expr::Call { name: callee, args }
                            if self.collect_wir
                                && matches!(analyzed_stmt, Stmt::Assign { .. })
                                && self.destination_forward_vars.contains(name) =>
                        {
                            let local_layout = self
                                .local_types
                                .get(name)
                                .and_then(|ty| self.specialized_layout_id(ty));
                            let callee_layout = self.fn_destination_layouts.get(callee).copied();
                            self.call_access_signature(value).cloned().and_then(|access| {
                                let ownership =
                                    Self::ownership_envelope_for_signature(&access);
                                (local_layout.is_some()
                                    && local_layout == callee_layout
                                    && ownership.unique_capacity_result
                                    && ownership.own_capacity_param.is_none()
                                    && ownership.var_capacity_params.is_empty())
                                    .then(|| (callee.clone(), args, access))
                            })
                        }
                        _ => None,
                    };
                    if let Some((callee, args, access)) = destination_call {
                        let result = self.lower_destination_user_call(
                            &callee,
                            args,
                            W::GetLocal(name.clone()),
                            &access,
                        )?;
                        seq.push(N::SetLocal {
                            local: name.clone(),
                            value: result,
                        });
                        tail_is_value = false;
                        // (RFC-0016) In-place reuse: a confined, never-aliased list `var`
                        // reassigned to a same-length list literal OVERWRITES its existing
                        // buffer slot-by-slot instead of allocating a fresh list — so a
                        // build-and-drop loop stays O(1) heap. The escape oracle proved the
                        // buffer is unaliased; we additionally require the RHS to not read
                        // the var (else a slot could be overwritten before a later element
                        // reads it), allocating normally for that one site otherwise.
                    } else if self.collect_wir
                        && self.reuse_vars.contains(name)
                        && matches!(value, Expr::List(_) | Expr::Ctor { .. })
                        // RFC-0111 specialized aggregates are descriptor-shaped, not
                        // the legacy `[header][i64 slots...]` buffer this optimization
                        // rewrites. A normal rebind below constructs the new descriptor
                        // layout safely; never reinterpret it as reusable legacy slots.
                        && self
                            .local_types
                            .get(name)
                            .and_then(|ty| self.specialized_layout_id(ty))
                            .is_none()
                        // Reference-backed aggregates have no linear slot buffer.
                        // Their persistent GC lowering is already copy-correct;
                        // never route them through the RFC-0016 i64 reuse path.
                        && !self.locals.get(name).is_some_and(|kind| kind.is_ref())
                        && !expr_reads_var(value, name)
                    {
                        let slot_addr = |i: usize| W::Binary {
                            op: witchy_wir::wir::BinOp::Add,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(W::GetLocal(name.clone())),
                            rhs: Box::new(W::ConstI32((4 + 8 * i) as i32)),
                        };
                        match value {
                            // A record: fixed tag + arity (guaranteed same ctor), so the
                            // buffer always has exactly the right slots — overwrite the
                            // fields directly, tag header untouched.
                            Expr::Ctor { args, .. } => {
                                for (i, item) in args.iter().enumerate() {
                                    let k = self.kind_of(item);
                                    let w = self.lower_expr(item)?;
                                    seq.push(N::Store {
                                        ptr: slot_addr(i),
                                        value: W::ToSlot(Box::new(w), Self::wir_kind(k)),
                                        kind: witchy_wir::wir::Kind::I64,
                                        offset: 0,
                                    });
                                }
                            }
                            // A list: capacity-resizing reuse. Evaluate the elements into
                            // the reuse temp pool ONCE (so the branch does not double-
                            // evaluate side effects), then either overwrite the existing
                            // buffer when its capacity (current count) fits, or
                            // reallocate — so a build-and-drop loop with varying lengths
                            // still stays bounded (the buffer ratchets to the max length).
                            // A literal too large for the pool just allocates (no reuse).
                            Expr::List(items) if items.len() <= REUSE_POOL => {
                                let len = items.len();
                                for (i, item) in items.iter().enumerate() {
                                    let k = self.kind_of(item);
                                    let w = self.lower_expr(item)?;
                                    seq.push(N::SetLocal {
                                        local: format!("__witchy_reuse_{i}"),
                                        value: W::ToSlot(Box::new(w), Self::wir_kind(k)),
                                    });
                                }
                                let mut then_: witchy_wir::wir::WirSeq = (0..len)
                                    .map(|i| N::Store {
                                        ptr: slot_addr(i),
                                        value: W::GetLocal(format!("__witchy_reuse_{i}")),
                                        kind: witchy_wir::wir::Kind::I64,
                                        offset: 0,
                                    })
                                    .collect();
                                then_.push(N::Store {
                                    ptr: W::GetLocal(name.clone()),
                                    value: W::ConstI32(len as i32),
                                    kind: witchy_wir::wir::Kind::I32,
                                    offset: 0,
                                });
                                self.mk_arities.insert(len);
                                let mut mk_args = Vec::with_capacity(len + 1);
                                mk_args.push(W::ConstI32(len as i32));
                                for i in 0..len {
                                    mk_args.push(W::GetLocal(format!("__witchy_reuse_{i}")));
                                }
                                let els = vec![N::SetLocal {
                                    local: name.clone(),
                                    value: W::Call { func: format!("mk{len}"), args: mk_args },
                                }];
                                // reuse when the current count (header at x+0) >= len.
                                let cond = W::Binary {
                                    op: witchy_wir::wir::BinOp::Le,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(W::ConstI32(len as i32)),
                                    rhs: Box::new(W::Load {
                                        ptr: Box::new(W::GetLocal(name.clone())),
                                        kind: witchy_wir::wir::Kind::I32,
                                        offset: 0,
                                    }),
                                };
                                seq.push(N::If { cond, then_, els, result: None });
                            }
                            // A list literal too large for the temp pool: allocate fresh.
                            Expr::List(items) => {
                                let w = self.lower_aggregate(items.len() as i32, items, 0)?;
                                seq.push(N::SetLocal { local: name.clone(), value: w });
                            }
                            _ => unreachable!(),
                        }
                        tail_is_value = false;
                    } else if self.sroa_active.contains_key(name) {
                        let Expr::RecordUpdate { name: _, base, fields } = value else {
                            return None;
                        };
                        let tyname = self.record_type_of(base)?;
                        let rec = self.record_fields.get(&tyname)?.clone();
                        for (fname, fval) in fields {
                            let idx = rec.iter().position(|(n, _)| n == fname)?;
                            let k = self.kind_of(fval);
                            let w = self.lower_expr(fval)?;
                            seq.push(N::SetLocal {
                                local: format!("{name}${idx}"),
                                value: W::ToSlot(Box::new(w), Self::wir_kind(k)),
                            });
                        }
                        tail_is_value = false;
                    } else if let Some((callee, _)) = (self.collect_wir
                        && self.inplace_push.contains(name))
                    .then(|| analysis::self_own_call(name, value, &self.summaries))
                    .flatten()
                    {
                        // own-ABI self-call (binary only): `xs = grow(move xs, …)`.
                        // Gated on `inplace_push` — i.e. the `{name}__cap` token IS
                        // declared (an accumulator). Under force-copy (`-inplace`,
                        // no accumulators) this falls through to a plain reassign
                        // (`name = <plain own-ABI call>`, cap = 0), so the cap local
                        // is never referenced when it doesn't exist.
                        // against a callee whose `own` buffer param may be returned.
                        // The callee returns `(value, cap)` and takes the caller's
                        // ownership token as a trailing i32 arg — so thread `xs__cap`
                        // in and capture (value → xs, cap → xs__cap) via CallStoreMulti.
                        let callee = callee.to_string();
                        let Expr::Call { args, .. } = value else { return None };
                        let param_kinds: Vec<Kind> = self
                            .fn_params
                            .get(&callee)
                            .map(|ps| {
                                ps.iter()
                                    .map(|p| p.ty.as_ref().map(|t| self.kind_for_type(t)).unwrap_or(Kind::I32))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mut args_w = Vec::with_capacity(args.len() + 1);
                        for (i, arg) in args.iter().enumerate() {
                            let ak = self.kind_of(arg);
                            let w = self.lower_expr(arg)?;
                            args_w.push(match param_kinds.get(i) {
                                Some(&pk) => Self::wir_convert(w, ak, pk),
                                None => w,
                            });
                        }
                        // The trailing synthetic arg: our current ownership token.
                        args_w.push(W::GetLocal(format!("{name}__cap")));
                        seq.push(N::CallStoreMulti {
                            func: callee,
                            args: args_w,
                            dests: vec![name.clone(), format!("{name}__cap")],
                        });
                        tail_is_value = false;
                    } else if self.collect_wir
                        && self.inplace_push.contains(name)
                        && matches!(
                            analysis::self_inplace_op(name, value),
                            Some(analysis::InPlaceOp::Push(_))
                        )
                        && self
                            .local_types
                            .get(name)
                            .and_then(|ty| self.specialized_layout_id(ty))
                            .is_some()
                        && !matches!(self.locals.get(name), Some(Kind::GcRef(_) | Kind::ExternRef))
                    {
                        let analysis::InPlaceOp::Push(elem) =
                            analysis::self_inplace_op(name, value).expect("guarded packed push")
                        else {
                            unreachable!("guarded packed push shape")
                        };
                        // Preserve value semantics at an alias-dirty site by passing
                        // no usable capacity token. The descriptor helper then copies
                        // into a fresh packed buffer; a clean unique site appends into
                        // descriptor slack when `cap > len`.
                        let dirty = match self.facts_stack.last() {
                            Some((facts, _, _)) if facts.accumulators.contains(name) => {
                                facts.is_dirty(analyzed_stmt)
                            }
                            _ => true,
                        };
                        let cap = if dirty {
                            W::ConstI32(0)
                        } else {
                            W::GetLocal(format!("{name}__cap"))
                        };
                        let Some((helper, args)) =
                            self.lower_packed_list_push_call(name, elem, cap)
                        else {
                            self.reject_reason.get_or_insert_with(|| CodegenError {
                                message: "declared packed layout cannot cross unsupported mutation `list.push` with a non-constructor element; this boundary requires an exact RFC-0111 LayoutId adapter and cannot box or reshape".into(),
                            });
                            return None;
                        };
                        seq.push(N::CallStoreMulti {
                            func: helper,
                            args,
                            dests: vec![name.clone(), format!("{name}__cap")],
                        });
                        inplace_sites += 1;
                        tail_is_value = false;
                    } else if self.collect_wir
                        && self.inplace_push.contains(name)
                        && analysis::self_inplace_op(name, value).is_some()
                        // (RFC-0005 stage 4) A GC-lowered cap-carrying record has no
                        // linear-memory buffer to mutate in place — fall through to
                        // the plain rebind (`StructNew` via the RecordUpdate GC path).
                        && !matches!(self.locals.get(name), Some(Kind::GcRef(_) | Kind::ExternRef))
                    {
                        let op = analysis::self_inplace_op(name, value).expect("guarded Some above");
                        if self
                            .local_types
                            .get(name)
                            .and_then(|ty| self.specialized_layout_id(ty))
                            .is_some()
                        {
                            let boundary = match &op {
                                analysis::InPlaceOp::Push(_) => "list.push",
                                analysis::InPlaceOp::SetAt(_, _) => "list.set_at",
                                analysis::InPlaceOp::UpdateAt(_, _) => "list.update_at",
                                analysis::InPlaceOp::Insert(_, _) => "dict.insert",
                                analysis::InPlaceOp::Update(_, _, _) => "dict.update",
                                analysis::InPlaceOp::Concat(_) => "string concatenation",
                                analysis::InPlaceOp::RecordUpdate(_) => "record update",
                            };
                            self.reject_reason.get_or_insert_with(|| CodegenError {
                                message: format!(
                                    "declared packed layout cannot cross unsupported mutation `{boundary}`; \
                                     this boundary requires an exact RFC-0111 LayoutId adapter and cannot box or reshape"
                                ),
                            });
                            return None;
                        }
                        // A dirty site (its RHS embeds an aliasing share of `name`)
                        // forces a zero ownership token → re-own + copy; a clean site
                        // trusts the runtime token. Read-only here; `sites` consumed
                        // at end. Hoisted across all in-place shapes below.
                        let dirty = match self.facts_stack.last() {
                            Some((facts, _, _)) if facts.accumulators.contains(name) => {
                                facts.is_dirty(analyzed_stmt)
                            }
                            _ => true,
                        };
                        match op {
                            analysis::InPlaceOp::Push(elem) => {
                                // Only the list-push shape has an in-place fast path. A dict/
                                // string self-assign falls through to the plain value-rebind
                                // below — correct value semantics, just without the O(1)
                                // in-place mutation.
                                let xk = self.kind_of(elem);
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                use witchy_wir::wir::BinOp;
                                let e = self.lower_expr(elem)?;
                                self.uses_list_push_cap = true;
                                // Stash length (i32) + value (i64 slot), then APPEND in place
                                // when owned slack remains (cap > len): write the value at slot
                                // `len` and bump the length, leaving the capacity token alone.
                                // Else fall back to `$list_push_cap` (grow / re-own). Eliding
                                // the helper CALL on the hot path is RFC-0016 R2 static elision.
                                seq.push(N::SetLocal {
                                    local: "__witchy_set_idx".into(),
                                    value: W::Load { ptr: Box::new(W::GetLocal(name.clone())), kind: witchy_wir::wir::Kind::I32, offset: 0 },
                                });
                                seq.push(N::SetLocal {
                                    local: "__witchy_set_val".into(),
                                    value: W::ToSlot(Box::new(e), Self::wir_kind(xk)),
                                });
                                let sl = || W::GetLocal("__witchy_set_idx".to_string());
                                let sv = || W::GetLocal("__witchy_set_val".to_string());
                                let bin = |op, l, r| W::Binary { op, kind: witchy_wir::wir::Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
                                let slot_ptr = bin(
                                    BinOp::Add,
                                    bin(BinOp::Add, W::GetLocal(name.clone()), W::ConstI32(4)),
                                    bin(BinOp::Mul, sl(), W::ConstI32(8)),
                                );
                                seq.push(N::If {
                                    cond: bin(BinOp::Gt, cap.clone(), sl()),
                                    then_: vec![
                                        N::Store { ptr: slot_ptr, value: sv(), kind: witchy_wir::wir::Kind::I64, offset: 0 },
                                        N::Store { ptr: W::GetLocal(name.clone()), value: bin(BinOp::Add, sl(), W::ConstI32(1)), kind: witchy_wir::wir::Kind::I32, offset: 0 },
                                    ],
                                    els: vec![N::CallStoreMulti {
                                        func: "list_push_cap".to_string(),
                                        args: vec![W::GetLocal(name.clone()), sv(), cap],
                                        dests: vec![name.clone(), format!("{name}__cap")],
                                    }],
                                    result: None,
                                });
                            }
                            analysis::InPlaceOp::SetAt(iexpr, vexpr) => {
                                // `list.set_at(xs, i, v)`: in-place element store via
                                // `$list_set_cap` (mutate the owned buffer's slot, O(1)),
                                // mirroring the list-push fast path. Without it the plain
                                // rebind rebuilds the whole list each set — O(n²) memory
                                // that traps a large list under the memory cap. A dirty
                                // site forces a zero token (re-own + copy, preserving any
                                // alias); a clean site mutates the owned buffer.
                                let ik = self.kind_of(iexpr);
                                let vk = self.kind_of(vexpr);
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                use witchy_wir::wir::BinOp;
                                let iw = self.lower_expr(iexpr)?;
                                let vw = self.lower_expr(vexpr)?;
                                // Stash index (i32) + value (i64 slot) into scratch locals,
                                // then store IN PLACE inline when in-bounds and owned, else
                                // fall back to `$list_set_cap` (OOB no-op / re-own-and-copy).
                                // Eliding the helper CALL on the hot path is RFC-0016 R2
                                // static elision; the proven helper still covers the cold path.
                                seq.push(N::SetLocal {
                                    local: "__witchy_set_idx".into(),
                                    value: Self::wir_convert(iw, ik, Kind::I32),
                                });
                                seq.push(N::SetLocal {
                                    local: "__witchy_set_val".into(),
                                    value: W::ToSlot(Box::new(vw), Self::wir_kind(vk)),
                                });
                                let si = || W::GetLocal("__witchy_set_idx".to_string());
                                let sv = || W::GetLocal("__witchy_set_val".to_string());
                                let bin = |op, l, r| W::Binary { op, kind: witchy_wir::wir::Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
                                let len = W::Load { ptr: Box::new(W::GetLocal(name.clone())), kind: witchy_wir::wir::Kind::I32, offset: 0 };
                                let cond = bin(
                                    BinOp::And,
                                    bin(BinOp::And, bin(BinOp::Ge, si(), W::ConstI32(0)), bin(BinOp::Lt, si(), len)),
                                    bin(BinOp::Gt, cap.clone(), W::ConstI32(0)),
                                );
                                let slot_ptr = || bin(
                                    BinOp::Add,
                                    bin(BinOp::Add, W::GetLocal(name.clone()), W::ConstI32(4)),
                                    bin(BinOp::Mul, si(), W::ConstI32(8)),
                                );
                                // (RFC-0035 step 2) Drop the element this store DISPLACES: load the
                                // old i32 slot and `$rc_drop` it before overwriting. Sound because
                                // dup-at-read (step 1) already counted every reader — the count is
                                // >1 exactly when a live binding still holds the element (drop just
                                // decrements) and 1 exactly when the slot was its last holder (drop
                                // frees). Gated: the displaced element is a PROVABLY offset-0 rc
                                // value (`expr_is_offset0_rc(vexpr)` — the slot has the same type;
                                // excludes Dict/scalar/type-var), the list var is confined-unique
                                // (`inplace_push` ⇒ never aliased ⇒ its buffer was never copied, so
                                // elements aren't shared through a container copy), we are at
                                // `wm_level==0` (a drop inside an arena-reset scope would double-
                                // free), and `rc-floor` is on. The `els` re-own+copy cold path is
                                // NOT dropped — a copy shares element pointers without a dup, so a
                                // free there could be a UAF (left as a sound leak).
                                // (BUG-315) An out-of-range (or negative) `set_at` is a
                                // runtime error on both backends — symmetric with the
                                // `xs[i]` READ trap — never a silent no-op (the cold
                                // `$list_set_cap` path used to swallow it). Route the OOB
                                // case through `$list_at`, which aborts with the identical
                                // `list index {i} out of bounds (length {len})` diagnostic
                                // (and carries the `__witchy_abort` import); the result is
                                // unreachable (the call always traps here) so it is dropped.
                                let set_len = || W::Load { ptr: Box::new(W::GetLocal(name.clone())), kind: witchy_wir::wir::Kind::I32, offset: 0 };
                                seq.push(N::If {
                                    cond: bin(BinOp::Or, bin(BinOp::Lt, si(), W::ConstI32(0)), bin(BinOp::Ge, si(), set_len())),
                                    then_: vec![N::Drop(W::Call {
                                        func: "list_at".into(),
                                        args: vec![W::GetLocal(name.clone()), Self::wir_convert(si(), Kind::I32, Kind::I64)],
                                    })],
                                    els: vec![],
                                    result: None,
                                });
                                let rc_drop_displaced = Self::wir_kind(vk) == witchy_wir::wir::Kind::I32
                                    && self.inplace_push.contains(name)
                                    && self.expr_is_offset0_rc(vexpr)
                                    && self.wm_level == 0
                                    && !force_copy_mode()
                                    && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::RcFloor);
                                let mut then_body = Vec::new();
                                if rc_drop_displaced {
                                    then_body.push(N::Do(W::Call {
                                        func: "rc_drop".into(),
                                        args: vec![W::FromSlot(
                                            Box::new(W::Load { ptr: Box::new(slot_ptr()), kind: witchy_wir::wir::Kind::I64, offset: 0 }),
                                            witchy_wir::wir::Kind::I32,
                                        )],
                                    }));
                                }
                                then_body.push(N::Store { ptr: slot_ptr(), value: sv(), kind: witchy_wir::wir::Kind::I64, offset: 0 });
                                seq.push(N::If {
                                    cond,
                                    then_: then_body,
                                    els: vec![N::CallStoreMulti {
                                        func: "list_set_cap".to_string(),
                                        args: vec![W::GetLocal(name.clone()), si(), sv(), cap],
                                        dests: vec![name.clone(), format!("{name}__cap")],
                                    }],
                                    result: None,
                                });
                            }
                            analysis::InPlaceOp::UpdateAt(iexpr, fexpr) => {
                                // `list.update_at(xs, i, f)`: in-place element update
                                // via `$list_update_cap` (apply the closure to the owned
                                // slot, O(1)), mirroring the set_at fast path. Without it the
                                // plain rebind copies the whole list each update — O(n²)
                                // memory. A dirty site forces a zero token (re-own + copy,
                                // preserving any alias); a clean site mutates in place.
                                let ik = self.kind_of(iexpr);
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                let iw = self.lower_expr(iexpr)?;
                                let fw = self.lower_expr(fexpr)?;
                                use witchy_wir::wir::BinOp;
                                let bin = |op, l, r| W::Binary { op, kind: witchy_wir::wir::Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
                                // Stash the index so the bounds check and the helper call
                                // share it (the index expr is lowered once).
                                seq.push(N::SetLocal {
                                    local: "__witchy_set_idx".into(),
                                    value: Self::wir_convert(iw, ik, Kind::I32),
                                });
                                let si = || W::GetLocal("__witchy_set_idx".to_string());
                                // (BUG-315) An out-of-range (or negative) `update_at` is a
                                // runtime error on both backends — symmetric with the
                                // `list.at` READ trap — never a silent no-op. See the
                                // `SetAt` arm; `$list_at` carries the identical diagnostic.
                                let upd_len = || W::Load { ptr: Box::new(W::GetLocal(name.clone())), kind: witchy_wir::wir::Kind::I32, offset: 0 };
                                seq.push(N::If {
                                    cond: bin(BinOp::Or, bin(BinOp::Lt, si(), W::ConstI32(0)), bin(BinOp::Ge, si(), upd_len())),
                                    then_: vec![N::Drop(W::Call {
                                        func: "list_at".into(),
                                        args: vec![W::GetLocal(name.clone()), Self::wir_convert(si(), Kind::I32, Kind::I64)],
                                    })],
                                    els: vec![],
                                    result: None,
                                });
                                seq.push(N::CallStoreMulti {
                                    func: "list_update_cap".to_string(),
                                    args: vec![
                                        W::GetLocal(name.clone()),
                                        si(),
                                        fw,
                                        cap,
                                    ],
                                    dests: vec![name.clone(), format!("{name}__cap")],
                                });
                            }
                            analysis::InPlaceOp::Insert(kexpr, vexpr) => {
                                // `dict.insert(d, k, v)`: the in-place dict upsert via
                                // `$dict_insert_cap` (O(1) amortized into owned entry slack),
                                // mirroring the list-push fast path. Without it the plain
                                // rebind below copies the whole dict each insert — O(n²)
                                // memory that traps a large dict under a tight memory cap.
                                let mode = self.dict_key_mode_wir(kexpr)?;
                                let kk = self.kind_of(kexpr);
                                let vk = self.kind_of(vexpr);
                                if let Some(kvt) = self.dict_key_valtype_of(value) {
                                    self.local_dict_key_valtype.insert(name.clone(), kvt);
                                }
                                if let Some(vvt) = self.dict_value_valtype_of(value) {
                                    self.local_dict_value_valtype.insert(name.clone(), vvt);
                                }
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                let kw = self.lower_expr(kexpr)?;
                                let vw = self.lower_expr(vexpr)?;
                                self.uses_dict_insert_cap = true;
                                seq.push(N::CallStoreMulti {
                                    func: intrinsics::declared_wir_helper(
                                        intrinsics::DICT_INSERT,
                                        "dict_insert_cap",
                                    )
                                    .expect("dict insert catalog declares optimized helper")
                                    .to_string(),
                                    args: vec![
                                        W::GetLocal(name.clone()),
                                        W::ToSlot(Box::new(kw), Self::wir_kind(kk)),
                                        W::ToSlot(Box::new(vw), Self::wir_kind(vk)),
                                        W::ConstI32(mode as i32),
                                        cap,
                                    ],
                                    dests: vec![name.clone(), format!("{name}__cap")],
                                });
                            }
                            analysis::InPlaceOp::Update(kexpr, dexpr, fexpr) => {
                                // `dict.update(d, k, dflt, f)`: the in-place upsert via
                                // `$dict_update_cap` (apply the closure, reinsert into owned
                                // slack), mirroring the dict-insert fast path. Without it the
                                // plain rebind copies the whole dict each update.
                                let mode = self.dict_key_mode_wir(kexpr)?;
                                let kk = self.kind_of(kexpr);
                                let dk = self.kind_of(dexpr);
                                if let Some(kvt) = self.dict_key_valtype_of(value) {
                                    self.local_dict_key_valtype.insert(name.clone(), kvt);
                                }
                                if let Some(vvt) = self.dict_value_valtype_of(value) {
                                    self.local_dict_value_valtype.insert(name.clone(), vvt);
                                }
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                let kw = self.lower_expr(kexpr)?;
                                let dw = self.lower_expr(dexpr)?;
                                let fw = self.lower_expr(fexpr)?;
                                self.clos_arities.insert(1);
                                self.uses_dict_update_cap = true;
                                seq.push(N::CallStoreMulti {
                                    func: intrinsics::declared_wir_helper(
                                        intrinsics::DICT_UPDATE,
                                        "dict_update_cap",
                                    )
                                    .expect("dict update catalog declares optimized helper")
                                    .to_string(),
                                    args: vec![
                                        W::GetLocal(name.clone()),
                                        W::ToSlot(Box::new(kw), Self::wir_kind(kk)),
                                        W::ToSlot(Box::new(dw), Self::wir_kind(dk)),
                                        W::ConstI32(mode as i32),
                                        fw,
                                        cap,
                                    ],
                                    dests: vec![name.clone(), format!("{name}__cap")],
                                });
                            }
                            analysis::InPlaceOp::Concat(pieces) => {
                                // `s = s + a + b`: the in-place string builder via
                                // `$str_append_cap` (append each piece into owned byte
                                // slack), mirroring the list/dict fast paths. Without it the
                                // plain rebind re-concatenates the whole string each
                                // statement — O(n²) bytes for a growing buffer. A dirty
                                // first piece forces a zero token (re-own → grow-and-copy,
                                // preserving any alias); later pieces reuse the fresh slack.
                                self.uses_str_append_cap = true;
                                for (i, piece) in pieces.into_iter().enumerate() {
                                    let pw = self.lower_expr(piece)?;
                                    let cap = if i == 0 && dirty {
                                        W::ConstI32(0)
                                    } else {
                                        W::GetLocal(format!("{name}__cap"))
                                    };
                                    seq.push(N::CallStoreMulti {
                                        func: "str_append_cap".to_string(),
                                        args: vec![W::GetLocal(name.clone()), pw, cap],
                                        dests: vec![name.clone(), format!("{name}__cap")],
                                    });
                                }
                            }
                            analysis::InPlaceOp::RecordUpdate(fields) => {
                                // (RFC-0033 R1) `s = {...s, f: v, …}`: when `s` is the
                                // uniquely-owned record, write each updated field into s's
                                // existing slots (`s+4+8*idx`) and keep the pointer — O(updated
                                // fields), no alloc, no copy of the un-updated fields. A dirty /
                                // un-owned site re-owns via the `mk{n}` realloc (the existing
                                // copy path), yielding a fresh owned record + a live token.
                                // Records are fixed-shape, so the token is a 0/1 owned flag.
                                // Field values are stashed into the reuse pool BEFORE any store
                                // so a value reading another field sees the pre-update record.
                                use witchy_wir::wir::BinOp;
                                let tyname = self.local_records.get(name).cloned()?;
                                let rnames = self.record_fields.get(&tyname).cloned()?;
                                let &(tag, nfields) = self.ctors.get(&tyname)?;
                                self.mk_arities.insert(nfields);
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                let slot = |idx: usize| W::Binary {
                                    op: BinOp::Add,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(W::GetLocal(name.clone())),
                                    rhs: Box::new(W::ConstI32((4 + 8 * idx) as i32)),
                                };
                                let load = |idx: usize| W::Load {
                                    ptr: Box::new(slot(idx)),
                                    kind: witchy_wir::wir::Kind::I64,
                                    offset: 0,
                                };
                                if fields.len() <= REUSE_POOL {
                                    // Stash each updated value into a reuse slot, recording
                                    // (record-field index -> reuse slot).
                                    let mut updated: Vec<(usize, usize)> = Vec::with_capacity(fields.len());
                                    for (j, (fname, vexpr)) in fields.iter().enumerate() {
                                        let idx = rnames.iter().position(|(n, _)| n == fname)?;
                                        // (RFC-0033 R2) `s.field = list.push(s.field, e)`: grow the
                                        // field's list buffer in place instead of copying it each
                                        // update. Two soundness guards:
                                        //   * whole-record aliasing — `eff = field_cap * (record
                                        //     owned)` is 0 unless this record is uniquely owned at
                                        //     the site (R1's `cap`), so an aliased record copies;
                                        //   * field aliasing — `field_push_safe` only holds when
                                        //     `s.field` is read nowhere but this push receiver, so
                                        //     the buffer is never separately aliased.
                                        // Either guard failing falls back to the copying push below.
                                        let push_field = if self
                                            .field_push_safe
                                            .contains(&(name.clone(), (*fname).clone()))
                                        {
                                            match vexpr {
                                                Expr::Call { name: pn, args: pa }
                                                    if matches!(pn.as_str(), "list.push" | intrinsics::LIST_PUSH) && pa.len() == 2 =>
                                                {
                                                    match &pa[0] {
                                                        Expr::Field { base, field }
                                                            if field == fname
                                                                && matches!(base.as_ref(), Expr::Var(v) if v == name) =>
                                                        {
                                                            Some(&pa[1])
                                                        }
                                                        _ => None,
                                                    }
                                                }
                                                _ => None,
                                            }
                                        } else {
                                            None
                                        };
                                        if let Some(elem) = push_field {
                                            use witchy_wir::wir::BinOp as B;
                                            let fcap = format!("{name}${fname}__cap");
                                            self.field_caps.insert(fcap.clone());
                                            self.uses_list_push_cap = true;
                                            let ek = self.kind_of(elem);
                                            let ew = self.lower_expr(elem)?;
                                            // The field slot holds an i32 list pointer widened into
                                            // the i64 universal slot; truncate it back to the ptr.
                                            let cur =
                                                Self::wir_convert(load(idx), Kind::I64, Kind::I32);
                                            // eff = field_cap * (record owned): 0 unless the record
                                            // is uniquely owned at this site, forcing a field copy.
                                            let eff = W::Binary {
                                                op: B::Mul,
                                                kind: witchy_wir::wir::Kind::I32,
                                                lhs: Box::new(W::GetLocal(fcap.clone())),
                                                rhs: Box::new(W::Binary {
                                                    op: B::Gt,
                                                    kind: witchy_wir::wir::Kind::I32,
                                                    lhs: Box::new(cap.clone()),
                                                    rhs: Box::new(W::ConstI32(0)),
                                                }),
                                            };
                                            seq.push(N::CallStoreMulti {
                                                func: "list_push_cap".to_string(),
                                                args: vec![
                                                    cur,
                                                    W::ToSlot(Box::new(ew), Self::wir_kind(ek)),
                                                    eff,
                                                ],
                                                dests: vec![TUPLE_TMP.to_string(), fcap],
                                            });
                                            seq.push(N::SetLocal {
                                                local: format!("__witchy_reuse_{j}"),
                                                value: W::ToSlot(
                                                    Box::new(W::GetLocal(TUPLE_TMP.to_string())),
                                                    Self::wir_kind(Kind::I32),
                                                ),
                                            });
                                            updated.push((idx, j));
                                            continue;
                                        }
                                        let vk = self.kind_of(vexpr);
                                        let vw = self.lower_expr(vexpr)?;
                                        seq.push(N::SetLocal {
                                            local: format!("__witchy_reuse_{j}"),
                                            value: W::ToSlot(Box::new(vw), Self::wir_kind(vk)),
                                        });
                                        updated.push((idx, j));
                                    }
                                    // Cold path: a fresh record (updated fields from the reuse
                                    // slots, the rest copied from the old `s`).
                                    let mut mk_args = Vec::with_capacity(nfields + 1);
                                    mk_args.push(W::ConstI32(tag as i32));
                                    for i in 0..nfields {
                                        mk_args.push(match updated.iter().find(|(idx, _)| *idx == i) {
                                            Some((_, j)) => W::GetLocal(format!("__witchy_reuse_{j}")),
                                            None => load(i),
                                        });
                                    }
                                    // Hot path: store each updated value into s's slot in place.
                                    let hot: Vec<N> = updated
                                        .iter()
                                        .map(|(idx, j)| N::Store {
                                            ptr: slot(*idx),
                                            value: W::GetLocal(format!("__witchy_reuse_{j}")),
                                            kind: witchy_wir::wir::Kind::I64,
                                            offset: 0,
                                        })
                                        .collect();
                                    seq.push(N::If {
                                        cond: W::Binary {
                                            op: BinOp::Gt,
                                            kind: witchy_wir::wir::Kind::I32,
                                            lhs: Box::new(cap),
                                            rhs: Box::new(W::ConstI32(0)),
                                        },
                                        then_: hot,
                                        els: vec![
                                            N::SetLocal {
                                                local: name.clone(),
                                                value: W::Call { func: format!("mk{nfields}"), args: mk_args },
                                            },
                                            N::SetLocal { local: format!("{name}__cap"), value: W::ConstI32(1) },
                                        ],
                                        result: None,
                                    });
                                } else {
                                    // >REUSE_POOL updated fields (rare): always realloc + re-own.
                                    let mut upd: Vec<(usize, W)> = Vec::with_capacity(fields.len());
                                    for (fname, vexpr) in fields.iter() {
                                        let idx = rnames.iter().position(|(n, _)| n == fname)?;
                                        let vk = self.kind_of(vexpr);
                                        let vw = self.lower_expr(vexpr)?;
                                        upd.push((idx, W::ToSlot(Box::new(vw), Self::wir_kind(vk))));
                                    }
                                    let mut mk_args = Vec::with_capacity(nfields + 1);
                                    mk_args.push(W::ConstI32(tag as i32));
                                    for i in 0..nfields {
                                        mk_args.push(match upd.iter().position(|(idx, _)| *idx == i) {
                                            Some(p) => upd[p].1.clone(),
                                            None => load(i),
                                        });
                                    }
                                    seq.push(N::SetLocal {
                                        local: name.clone(),
                                        value: W::Call { func: format!("mk{nfields}"), args: mk_args },
                                    });
                                    seq.push(N::SetLocal { local: format!("{name}__cap"), value: W::ConstI32(1) });
                                }
                            }
                        }
                        inplace_sites += 1;
                        tail_is_value = false;
                    } else if self.collect_wir
                        && self.rc_floor_vars.contains(name)
                        && !self.locals.get(name).is_some_and(|kind| kind.is_ref())
                        && matches!(value, Expr::Call { name: f, args }
                            if matches!(args.first(), Some(Expr::Var(x)) if x == name)
                                && analysis::fresh_heap_builtin_offset(f, args.len()).is_some())
                    {
                        // (RFC-0016) RC-floor free-at-overwrite: `name` is a confined,
                        // never-aliased heap var overwritten by a builtin that allocates
                        // a FRESH buffer while threading the old one through as its
                        // receiver (`dict.remove(d, k)`) — a shape the in-place fast
                        // paths above did not claim, so it would otherwise leak the old
                        // buffer every iteration (the cache-eviction leak). The old
                        // buffer is now dead: evaluate the fresh result (which still
                        // reads the old via the receiver), and when it is a genuinely new
                        // allocation (the pointer differs — the guard also makes a callee
                        // that returned its own buffer a safe no-op) free the old region
                        // into the size-classed free-list before rebinding.
                        let Expr::Call { name: f, args } = value else { unreachable!() };
                        let offset = analysis::fresh_heap_builtin_offset(f, args.len())
                            .expect("guarded Some above");
                        let vk = self.kind_of(value);
                        let target = self.locals.get(name).copied().unwrap_or(Kind::I32);
                        let v = self.lower_expr(value)?;
                        seq.push(N::SetLocal {
                            local: "__rc_new".into(),
                            value: Self::wir_convert(v, vk, target),
                        });
                        // The start of the old buffer's `$rc_alloc` region (dicts sit 4
                        // bytes past it, for the hidden index word).
                        let old_region = if offset == 0 {
                            W::GetLocal(name.clone())
                        } else {
                            W::Binary {
                                op: witchy_wir::wir::BinOp::Sub,
                                kind: witchy_wir::wir::Kind::I32,
                                lhs: Box::new(W::GetLocal(name.clone())),
                                rhs: Box::new(W::ConstI32(offset)),
                            }
                        };
                        let cond = W::Binary {
                            op: witchy_wir::wir::BinOp::Ne,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(W::GetLocal("__rc_new".into())),
                            rhs: Box::new(W::GetLocal(name.clone())),
                        };
                        seq.push(N::If {
                            cond,
                            then_: vec![N::Do(W::Call {
                                func: "rc_free".into(),
                                args: vec![old_region],
                            })],
                            els: vec![],
                            result: None,
                        });
                        seq.push(N::SetLocal {
                            local: name.clone(),
                            value: W::GetLocal("__rc_new".into()),
                        });
                        tail_is_value = false;
                    } else {
                        // A plain local reassignment, INCLUDING a self-assign
                        // accumulator (`s = s + x`, `list.push(xs, e)`) that the
                        // in-place fast path above didn't claim: lower it as a
                        // fresh-value rebind. That is exactly the interpreter's value
                        // semantics — the RHS allocates a new value and rebinds the
                        // local; the in-place mutation is only an optimization the
                        // uniqueness/wir_opt pass layers on later. If the RHS itself
                        // can't lower yet, the `?` defers the whole function as before.
                        let vk = self.kind_of(value);
                        let target = self.locals.get(name).copied().unwrap_or(Kind::I32);
                        let v = self.lower_expr(value)?;
                        seq.push(N::SetLocal {
                            local: name.clone(),
                            value: Self::wir_convert(v, vk, target),
                        });
                        // A plain rebind replaces the allocation represented by
                        // this shadow token. Carrying the old capacity into the
                        // new value would make the next in-place write trust
                        // storage it does not own. Fresh list literals seed their
                        // exact capacity, and a direct `unique` collection result
                        // supplies the token returned by its compiled ABI.
                        if self.collect_wir && self.inplace_push.contains(name) {
                            let cap = if self.expression_returns_unique_capacity(value) {
                                W::GetLocal(UNIQUE_RESULT_CAP_TMP.to_string())
                            } else {
                                match value {
                                    Expr::List(items) => W::ConstI32(items.len() as i32),
                                    _ => W::ConstI32(0),
                                }
                            };
                            seq.push(N::SetLocal {
                                local: format!("{name}__cap"),
                                value: cap,
                            });
                        }
                        tail_is_value = false;
                    }
                }
                // Yield → legacy (rewritten away before codegen anyway).
                _ => return None,
            }
            // Derive source-site propagation from the lowered artifact, not from
            // a second list of language operations. Host-backed helpers receive
            // the packed site as a final argument and publish it only at their
            // host edge; successful nested calls therefore cannot stale an outer
            // operation's location.
            if assembly::wir_seq_needs_diagnostic_site(&seq[stmt_start..]) {
                let func = self.cur_fn_name.clone();
                let func_ptr = self.intern(&func);
                let line = block.lines.get(i).copied().filter(|line| *line != u32::MAX).unwrap_or(0);
                let site = witchy_syntax::diag::pack_site(func_ptr, line);
                let mut stmt_seq = seq.split_off(stmt_start);
                let attached = assembly::attach_diagnostic_sites(&mut stmt_seq, site);
                debug_assert!(attached, "detected host path must accept a source site");
                seq.extend(stmt_seq);
                self.uses_diagnostic_sites = true;
            }
            if !matches!(stmt, Stmt::Return(_)) {
                let opens = self.loan_facts.opens_after(analyzed_stmt).to_vec();
                let closes = self.loan_facts.closes_after(analyzed_stmt).to_vec();
                seq.extend(self.open_loan_nodes(&opens));
                seq.extend(self.close_loan_nodes(&closes));
            }
            // Reset the cap of any inplace_push var killed AFTER this statement
            // (binary path), positioned here in the seq. Read-only — the kills
            // counter is consumed once by the `take_kills` loop below.
            if self.collect_wir && !self.inplace_push.is_empty() {
                let killed: Vec<String> = self
                    .facts_stack
                    .last()
                    .map(|(f, _, _)| f.kills_after(analyzed_stmt).to_vec())
                    .unwrap_or_default();
                for v in &killed {
                    if self.inplace_push.contains(v) {
                        seq.push(N::SetLocal {
                            local: format!("{v}__cap"),
                            value: W::ConstI32(0),
                        });
                    }
                }
            }
            // (RFC-0035) `$rc_free` every value proven dead after this statement:
            // bound to a known heap allocator, read at most once here, never aliased
            // / escaped / returned / reassigned / region-confined (the `last_use`
            // analysis discharged all of that). The value's last use is in the WIR
            // just pushed to `seq`, so its region is now unreachable and free to
            // reclaim — a straight `$rc_free`, no runtime refcount needed (the value
            // is statically unique-and-dead). `drop_facts_stack` is empty unless
            // `rc-floor` is on, so this is a no-op on the reference path. Read-only
            // over the facts, mirroring the kills reset above.
            //
            // SOUNDNESS — the heap-reset boundary. `wm_level > 0` means we are lowering
            // inside an active heap-reset scope: a watermarked loop body (RFC-0030, the
            // per-iteration `$heap`-rewind) or a `region:`/reclaim block. That reset ALSO
            // reclaims this value's buffer, so an `$rc_free` here would be a DOUBLE
            // reclaim — the freed block lands on the free-list, the watermark then rewinds
            // `$heap` below it, and the next bump re-hands-out the same address that is
            // still linked in the free-list → the free-list `next` chain dangles into
            // live data (an out-of-bounds pointer). So rc-floor cedes reclamation to the
            // enclosing reset and only fires where `wm_level == 0` — straight-line code
            // and loops whose body is NOT arena-resettable (something escapes the
            // iteration, the watermark is off), which is exactly rc-floor's niche.
            if self.collect_wir && self.wm_level == 0 {
                let drops: Vec<analysis::Drop> = self
                    .drop_facts_stack
                    .last()
                    .map(|d| d.drops_after(stmt).to_vec())
                    .unwrap_or_default();
                for d in &drops {
                    let region = if d.offset == 0 {
                        W::GetLocal(d.name.clone())
                    } else {
                        W::Binary {
                            op: witchy_wir::wir::BinOp::Sub,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(W::GetLocal(d.name.clone())),
                            rhs: Box::new(W::ConstI32(d.offset)),
                        }
                    };
                    seq.push(N::Do(W::Call {
                        func: "rc_free".into(),
                        args: vec![region],
                    }));
                }
                // (RFC-0035 step 3) `$rc_drop` read-owned heap bindings at their last use.
                // The element was `$rc_dup`'d at the read (step 1), so this releases that
                // reference — freeing at count 0 (the slot's set_at `$rc_drop` or another
                // holder took the rest) and merely decrementing otherwise (a live holder
                // keeps it). Only bindings we recorded as ACTUALLY dup'd (`rc_owned_bindings`
                // — the same per-type gate as the dup, so drop-iff-dup'd holds by
                // construction), all at rc-region offset 0 (Dict elements are excluded there).
                let read_drops: Vec<String> = self
                    .drop_facts_stack
                    .last()
                    .map(|d| d.read_drops_after(stmt).to_vec())
                    .unwrap_or_default();
                for name in &read_drops {
                    if self.rc_owned_bindings.contains(name)
                        && matches!(self.locals.get(name), Some(&Kind::I32))
                    {
                        seq.push(N::Do(W::Call {
                            func: "rc_drop".into(),
                            args: vec![W::GetLocal(name.clone())],
                        }));
                    }
                }
            }
        }
        // A fallthrough block always leaves one value: the tail expression, or
        // `i32.const 0`. A terminal `return` needs no synthetic value; adding an
        // i32 after it makes reference-returning functions fail Wasm validation.
        if !tail_is_value && !tail_is_terminal {
            seq.push(N::Push(W::ConstI32(0)));
        }
        // Facts consumption. In a WIR-collecting scope `lower_block` is invoked many
        // times per compile (`kind_of` probes, nested re-lowering), so consuming
        // here would over-count — instead `compile_function` consumes ONCE on a
        // successful capture. This `!collect_wir` branch is the non-collecting
        // fallback, where this block is the authoritative consumer. The cap-reset
        // nodes are already positioned in `seq` above (read-only `kills_after`).
        if !self.collect_wir {
            for stmt in &block.stmts {
                let _ = self.take_kills(stmt);
            }
            if inplace_sites > 0 {
                if let Some((_, _, sites)) = self.facts_stack.last_mut() {
                    *sites += inplace_sites;
                }
            }
        }
        Some(seq)
    }
}
