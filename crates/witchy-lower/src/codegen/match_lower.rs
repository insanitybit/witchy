//! Pattern-match lowering: the `Codegen` methods that lower a `match`
//! expression and its patterns to WIR — scalar pattern tests
//! (`lower_pattern`), externref and GC-struct pattern destructuring
//! (`lower_externref_pattern`, `lower_gc_field_patterns`,
//! `lower_gc_struct_pattern`), and the `match` arm dispatch itself
//! (`lower_match`). Split out of `codegen/mod.rs` as a further slice of an
//! incremental break-up of that file.

use super::*;

impl Codegen<'_> {
    /// Lower a SCALAR pattern test against `value` (the matched value as an i64
    /// slot — `local.get $MATCH_TMP`). Returns `(cond, binds)`: an i32 condition
    /// expression and the binding nodes. `None` for non-scalar patterns
    /// (tuple/list/ctor/string/…), which keep their bespoke legacy emission.
    pub(crate) fn lower_pattern(
        &mut self,
        value: &witchy_wir::wir::WirExpr,
        pat: &Pattern,
        expected: Option<&Type>,
    ) -> Option<(witchy_wir::wir::WirExpr, witchy_wir::wir::WirSeq)> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        let eq_i64 = |v: i64| W::Binary {
            op: witchy_wir::wir::BinOp::Eq,
            kind: witchy_wir::wir::Kind::I64,
            lhs: Box::new(value.clone()),
            rhs: Box::new(W::ConstI64(v)),
        };
        Some(match pat {
            Pattern::Wildcard => (W::ConstI32(1), vec![]),
            Pattern::Int(k) => (eq_i64(*k), vec![]),
            Pattern::Bool(b) => (eq_i64(if *b { 1 } else { 0 }), vec![]),
            Pattern::Var(name) => {
                let k = self.locals.get(name).copied().unwrap_or(Kind::I32);
                (
                    W::ConstI32(1),
                    vec![N::SetLocal {
                        local: name.clone(),
                        value: W::FromSlot(Box::new(value.clone()), Self::wir_kind(k)),
                    }],
                )
            }
            // A tuple `[0][e0][e1]...`: no tag, so the condition is the AND of the
            // element-pattern conditions; element `i` is the i64 slot at `ptr+4+8*i`
            // (ptr = value wrapped to i32). Recurses into sub-patterns.
            Pattern::Tuple(pats) => {
                let ptr = W::FromSlot(Box::new(value.clone()), witchy_wir::wir::Kind::I32);
                let slot_types = match expected.map(Type::unqualified) {
                    Some(Type::Tuple(items)) => Some(items.as_slice()),
                    Some(Type::Named(name, items)) if name.starts_with("Tuple") => {
                        Some(items.as_slice())
                    }
                    _ => None,
                };
                let mut elem_conds: Vec<W> = Vec::new();
                let mut binds: witchy_wir::wir::WirSeq = Vec::new();
                for (i, sub) in pats.iter().enumerate() {
                    let elem_value = W::Load {
                        ptr: Box::new(W::Binary {
                            op: witchy_wir::wir::BinOp::Add,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(ptr.clone()),
                            rhs: Box::new(W::ConstI32((4 + 8 * i) as i32)),
                        }),
                        kind: witchy_wir::wir::Kind::I64,
                        offset: 0,
                    };
                    let (sc, sb) =
                        self.lower_pattern(&elem_value, sub, slot_types.and_then(|tys| tys.get(i)))?;
                    if !matches!(sc, W::ConstI32(1)) {
                        elem_conds.push(sc);
                    }
                    binds.extend(sb);
                }
                let cond = if elem_conds.is_empty() {
                    W::ConstI32(1)
                } else {
                    wir_and_chain(&elem_conds)
                };
                (cond, binds)
            }
            // A list `[len][e0]...`: check the length first (exact, or a minimum
            // when there's a `..` tail), then — short-circuited under the length
            // check so a short list never reads out of bounds — match each prefix
            // element (the i64 slot at `ptr+4+8*i`). `..name` binds the tail as a
            // freshly-allocated list via `$list_drop`.
            Pattern::List { elems, rest } => {
                let ptr = W::FromSlot(Box::new(value.clone()), witchy_wir::wir::Kind::I32);
                let elem_ty = match expected.map(Type::unqualified) {
                    Some(Type::Named(name, args)) if name == "List" => args.first(),
                    _ => None,
                };
                let n = elems.len() as i32;
                let len_op = if rest.is_some() { witchy_wir::wir::BinOp::Ge } else { witchy_wir::wir::BinOp::Eq };
                let len_check = W::Binary {
                    op: len_op,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(W::Load { ptr: Box::new(ptr.clone()), kind: witchy_wir::wir::Kind::I32, offset: 0 }),
                    rhs: Box::new(W::ConstI32(n)),
                };
                let mut elem_conds: Vec<W> = Vec::new();
                let mut binds: witchy_wir::wir::WirSeq = Vec::new();
                for (i, sub) in elems.iter().enumerate() {
                    let elem_value = W::Load {
                        ptr: Box::new(W::Binary {
                            op: witchy_wir::wir::BinOp::Add,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(ptr.clone()),
                            rhs: Box::new(W::ConstI32((4 + 8 * i) as i32)),
                        }),
                        kind: witchy_wir::wir::Kind::I64,
                        offset: 0,
                    };
                    let (sc, sb) = self.lower_pattern(&elem_value, sub, elem_ty)?;
                    if !matches!(sc, W::ConstI32(1)) {
                        elem_conds.push(sc);
                    }
                    binds.extend(sb);
                }
                if let Some(Some(name)) = rest {
                    self.uses_list_drop = true;
                    binds.push(N::SetLocal {
                        local: name.clone(),
                        value: W::Call { func: "list_drop".into(), args: vec![ptr.clone(), W::ConstI32(n)] },
                    });
                }
                let cond = if elem_conds.is_empty() {
                    len_check
                } else {
                    W::Control(Box::new(N::If {
                        cond: len_check,
                        then_: vec![N::Push(wir_and_chain(&elem_conds))],
                        els: vec![N::Push(W::ConstI32(0))],
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    }))
                };
                (cond, binds)
            }
            // A string literal: structural compare against the interned literal.
            // Sound as a field pattern too — `$str_eq` reads the length header
            // first, so a wrong-variant garbage pointer is bounded by its claimed
            // length rather than dereferenced unboundedly (and field conditions are
            // short-circuited under the tag check below anyway).
            Pattern::Str(s) => {
                let off = self.intern(s);
                (
                    W::Call {
                        func: "str_eq".into(),
                        args: vec![W::FromSlot(Box::new(value.clone()), witchy_wir::wir::Kind::I32), W::StrPtr(off)],
                    },
                    vec![],
                )
            }
            // An ADT constructor `[tag][f0][f1]...`: the condition is `tag == k`,
            // and each field is the i64 slot at `ptr+4+8*i` (ptr = value wrapped to
            // i32). Field conditions are evaluated under a short-circuit `if tag ==
            // k` so a field is never loaded-and-inspected for the wrong variant
            // (which could deref a garbage pointer for a nested ctor). Binds run in
            // the arm body only after the whole condition passes, so they're safe.
            Pattern::Ctor { name, args } => {
                let &(tag, nfields) = self.ctors.get(name)?;
                if nfields != args.len() {
                    return None;
                }
                let field_types = self.ctor_pattern_field_types(name, expected);
                let ptr = W::FromSlot(Box::new(value.clone()), witchy_wir::wir::Kind::I32);
                let mut field_conds: Vec<W> = Vec::new();
                let mut binds: witchy_wir::wir::WirSeq = Vec::new();
                for (i, sub) in args.iter().enumerate() {
                    let field_value = W::Load {
                        ptr: Box::new(W::Binary {
                            op: witchy_wir::wir::BinOp::Add,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(ptr.clone()),
                            rhs: Box::new(W::ConstI32((4 + 8 * i) as i32)),
                        }),
                        kind: witchy_wir::wir::Kind::I64,
                        offset: 0,
                    };
                    let (sc, sb) = self.lower_pattern(
                        &field_value,
                        sub,
                        field_types.as_ref().and_then(|tys| tys.get(i)),
                    )?;
                    if !matches!(sc, W::ConstI32(1)) {
                        field_conds.push(sc);
                    }
                    binds.extend(sb);
                }
                let tag_eq = W::Binary {
                    op: witchy_wir::wir::BinOp::Eq,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(W::Load { ptr: Box::new(ptr), kind: witchy_wir::wir::Kind::I32, offset: 0 }),
                    rhs: Box::new(W::ConstI32(tag as i32)),
                };
                let cond = if field_conds.is_empty() {
                    tag_eq
                } else {
                    // `if tag == k: (field0 && field1 && …) else: 0` — fields are
                    // only touched when the tag matches.
                    W::Control(Box::new(N::If {
                        cond: tag_eq,
                        then_: vec![N::Push(wir_and_chain(&field_conds))],
                        els: vec![N::Push(W::ConstI32(0))],
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    }))
                };
                (cond, binds)
            }
            Pattern::AnonCtor { tag, args } => {
                let Type::Named(name, union_args) = expected?.unqualified() else {
                    return None;
                };
                let variants = witchy_types::typeck::anon_union_synthetic_variants(name)?;
                let mut offset = 0usize;
                let mut found = None;
                for (variant, arity) in variants {
                    let end = offset.checked_add(arity)?;
                    if end > union_args.len() {
                        return None;
                    }
                    if variant == *tag && arity == args.len() {
                        found = Some((
                            self.anon_union_tag_code(&variant, arity),
                            union_args[offset..end].to_vec(),
                        ));
                        break;
                    }
                    offset = end;
                }
                let (tag_code, field_types) = found?;
                let ptr = W::FromSlot(Box::new(value.clone()), witchy_wir::wir::Kind::I32);
                let mut field_conds: Vec<W> = Vec::new();
                let mut binds: witchy_wir::wir::WirSeq = Vec::new();
                for (i, sub) in args.iter().enumerate() {
                    let field_value = W::Load {
                        ptr: Box::new(W::Binary {
                            op: witchy_wir::wir::BinOp::Add,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(ptr.clone()),
                            rhs: Box::new(W::ConstI32((4 + 8 * i) as i32)),
                        }),
                        kind: witchy_wir::wir::Kind::I64,
                        offset: 0,
                    };
                    let (sc, sb) =
                        self.lower_pattern(&field_value, sub, field_types.get(i))?;
                    if !matches!(sc, W::ConstI32(1)) {
                        field_conds.push(sc);
                    }
                    binds.extend(sb);
                }
                let tag_eq = W::Binary {
                    op: witchy_wir::wir::BinOp::Eq,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(W::Load { ptr: Box::new(ptr), kind: witchy_wir::wir::Kind::I32, offset: 0 }),
                    rhs: Box::new(W::ConstI32(tag_code)),
                };
                let cond = if field_conds.is_empty() {
                    tag_eq
                } else {
                    W::Control(Box::new(N::If {
                        cond: tag_eq,
                        then_: vec![N::Push(wir_and_chain(&field_conds))],
                        els: vec![N::Push(W::ConstI32(0))],
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    }))
                };
                (cond, binds)
            }
            // (RFC-0052) A Duration literal is whole milliseconds and a Duration
            // value is an i64 slot of ms, so it is exact i64 equality — identical
            // to an Int literal (and to the interpreter's `Pattern::Duration` arm).
            Pattern::Duration(ms) => (eq_i64(*ms), vec![]),
            // (RFC-0052) `lo..hi` (half-open) / `lo..=hi` (inclusive):
            // `v >= lo && v (< | <=) hi` on the i64 scrutinee, mirroring the
            // interpreter's IntRange arm. No bindings.
            Pattern::IntRange { lo, hi, inclusive } => {
                let ge_lo = W::Binary {
                    op: witchy_wir::wir::BinOp::Ge,
                    kind: witchy_wir::wir::Kind::I64,
                    lhs: Box::new(value.clone()),
                    rhs: Box::new(W::ConstI64(*lo)),
                };
                let below_hi = W::Binary {
                    op: if *inclusive {
                        witchy_wir::wir::BinOp::Le
                    } else {
                        witchy_wir::wir::BinOp::Lt
                    },
                    kind: witchy_wir::wir::Kind::I64,
                    lhs: Box::new(value.clone()),
                    rhs: Box::new(W::ConstI64(*hi)),
                };
                (wir_and_chain(&[ge_lo, below_hi]), vec![])
            }
            // (RFC-0052) `p1 | p2 | …`: OR the alternative conditions. A binding
            // alternative (e.g. `Some(a) | Wrap(a)`) contributes its binds GUARDED
            // by its own re-tested condition (`if ci: binds_i`) — every alternative
            // binds the SAME names (checker-enforced), so exactly one guard fires
            // and the arm body sees the matched alternative's values, matching the
            // interpreter, which binds through the first matching alternative.
            Pattern::Or(alts) => {
                let mut conds: Vec<W> = Vec::new();
                let mut binds: witchy_wir::wir::WirSeq = Vec::new();
                for alt in alts {
                    let (c, b) = self.lower_pattern(value, alt, expected)?;
                    if !b.is_empty() {
                        binds.push(N::If {
                            cond: c.clone(),
                            then_: b,
                            els: vec![],
                            result: None,
                        });
                    }
                    conds.push(c);
                }
                if conds.is_empty() {
                    return None;
                }
                (wir_or_chain(&conds), binds)
            }
        })
    }

    pub(crate) fn lower_externref_pattern(
        &mut self,
        value: &witchy_wir::wir::WirExpr,
        pat: &Pattern,
        expected: Option<&Type>,
    ) -> Option<(witchy_wir::wir::WirExpr, witchy_wir::wir::WirSeq)> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        Some(match pat {
            Pattern::Wildcard => (W::ConstI32(1), vec![]),
            Pattern::Var(name) => (
                W::ConstI32(1),
                vec![N::SetLocal {
                    local: name.clone(),
                    value: value.clone(),
                }],
            ),
            Pattern::Ctor { name, args } if name == "Some" && args.len() == 1 => {
                let (inner, kind) = expected.and_then(|t| self.option_reference_inner(t))?;
                let (sub_cond, sub_binds) = match kind {
                    Kind::ExternRef => {
                        self.lower_externref_pattern(value, &args[0], Some(inner))?
                    }
                    Kind::GcRef(struct_id) => {
                        self.lower_gc_struct_pattern(
                            value,
                            &args[0],
                            struct_id,
                            Some(inner),
                        )?
                    }
                    _ => return None,
                };
                let non_null = W::Unary {
                    op: witchy_wir::wir::UnOp::Not,
                    kind: witchy_wir::wir::Kind::I32,
                    arg: Box::new(W::RefIsNull(Box::new(value.clone()))),
                };
                let cond = if matches!(sub_cond, W::ConstI32(1)) {
                    non_null
                } else {
                    W::Control(Box::new(N::If {
                        cond: non_null,
                        then_: vec![N::Push(sub_cond)],
                        els: vec![N::Push(W::ConstI32(0))],
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    }))
                };
                (cond, sub_binds)
            }
            Pattern::Ctor { name, args } if name == "None" && args.is_empty() => {
                expected.and_then(|t| self.option_reference_inner(t))?;
                (W::RefIsNull(Box::new(value.clone())), vec![])
            }
            Pattern::Ctor { name, args }
                if args.len() == 1 && self.transparent_externref_ctors.contains_key(name) =>
            {
                let field_ty = self.transparent_externref_ctors.get(name).cloned();
                self.lower_externref_pattern(value, &args[0], field_ty.as_ref().or(expected))?
            }
            _ => return None,
        })
    }

    fn lower_gc_field_patterns(
        &mut self,
        value: &witchy_wir::wir::WirExpr,
        args: &[Pattern],
        field_types: &[Type],
        struct_id: u32,
        field_base: u32,
    ) -> Option<(witchy_wir::wir::WirExpr, witchy_wir::wir::WirSeq)> {
        use witchy_wir::wir::WirExpr as W;
        if field_types.len() != args.len() {
            return None;
        }
        let mut field_conds: Vec<W> = Vec::new();
        let mut binds: witchy_wir::wir::WirSeq = Vec::new();
        for (i, sub) in args.iter().enumerate() {
            let field_ty = field_types.get(i)?;
            let field_kind = self.gc_field_storage_kind(field_ty);
            let field = W::StructGet {
                struct_id,
                field: field_base + i as u32,
                base: Box::new(value.clone()),
            };
            let (cond, sub_binds) = match field_kind {
                Kind::ExternRef => {
                    self.lower_externref_pattern(&field, sub, Some(field_ty))?
                }
                Kind::GcRef(nested) => {
                    self.lower_gc_struct_pattern(&field, sub, nested, Some(field_ty))?
                }
                _ => {
                    let slot = W::ToSlot(Box::new(field), Self::wir_kind(field_kind));
                    self.lower_pattern(&slot, sub, Some(field_ty))?
                }
            };
            if !matches!(cond, W::ConstI32(1)) {
                field_conds.push(cond);
            }
            binds.extend(sub_binds);
        }
        let cond = if field_conds.is_empty() {
            W::ConstI32(1)
        } else {
            wir_and_chain(&field_conds)
        };
        Some((cond, binds))
    }

    pub(crate) fn lower_gc_struct_pattern(
        &mut self,
        value: &witchy_wir::wir::WirExpr,
        pat: &Pattern,
        struct_id: u32,
        expected: Option<&Type>,
    ) -> Option<(witchy_wir::wir::WirExpr, witchy_wir::wir::WirSeq)> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        Some(match pat {
            Pattern::Wildcard => (W::ConstI32(1), vec![]),
            Pattern::Var(name) => (
                W::ConstI32(1),
                vec![N::SetLocal {
                    local: name.clone(),
                    value: value.clone(),
                }],
            ),
            Pattern::Ctor { name, .. }
                if matches!(name.as_str(), "Some" | "None")
                    && expected
                        .and_then(|ty| self.option_reference_inner(ty))
                        .is_some_and(|(_, kind)| kind == Kind::GcRef(struct_id)) =>
            {
                self.lower_externref_pattern(value, pat, expected)?
            }
            Pattern::List { elems, rest } => {
                let expected = expected?;
                let Type::Named(name, args) = expected.unqualified() else {
                    return None;
                };
                if name != "List" {
                    return None;
                }
                let elem_ty = args.first()?;
                let (type_id, array_id, element_kind) =
                    self.gc_reference_list_layout(expected)?;
                if type_id != struct_id {
                    return None;
                }
                let len_op = if rest.is_some() {
                    witchy_wir::wir::BinOp::Ge
                } else {
                    witchy_wir::wir::BinOp::Eq
                };
                let len_check = W::Binary {
                    op: len_op,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(W::ArrayLen(Box::new(value.clone()))),
                    rhs: Box::new(W::ConstI32(elems.len() as i32)),
                };
                let mut elem_conds = Vec::new();
                let mut binds = Vec::new();
                for (index, sub) in elems.iter().enumerate() {
                    let element = W::ArrayGet {
                        array_id,
                        array: Box::new(value.clone()),
                        index: Box::new(W::ConstI32(index as i32)),
                    };
                    let (cond, sub_binds) = match element_kind {
                        Kind::ExternRef => {
                            self.lower_externref_pattern(&element, sub, Some(elem_ty))?
                        }
                        Kind::GcRef(nested) => {
                            self.lower_gc_struct_pattern(
                                &element,
                                sub,
                                nested,
                                Some(elem_ty),
                            )?
                        }
                        Kind::I32 | Kind::I64 | Kind::F64 => return None,
                    };
                    if !matches!(cond, W::ConstI32(1)) {
                        elem_conds.push(cond);
                    }
                    binds.extend(sub_binds);
                }
                if let Some(Some(name)) = rest {
                    binds.push(N::SetLocal {
                        local: name.clone(),
                        value: self.lower_gc_reference_list_tail(
                            value.clone(),
                            type_id,
                            array_id,
                            element_kind,
                            elems.len() as i32,
                        )?,
                    });
                }
                let cond = if elem_conds.is_empty() {
                    len_check
                } else {
                    W::Control(Box::new(N::If {
                        cond: len_check,
                        then_: vec![N::Push(wir_and_chain(&elem_conds))],
                        els: vec![N::Push(W::ConstI32(0))],
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    }))
                };
                (cond, binds)
            }
            Pattern::Tuple(args) => {
                let Type::Tuple(field_types) = expected?.unqualified() else {
                    return None;
                };
                let shape = self.gc_tuple_shape(expected?)?;
                if self.gc_tuple_ids.get(&shape).copied()? != struct_id {
                    return None;
                }
                self.lower_gc_field_patterns(value, args, field_types, struct_id, 0)?
            }
            Pattern::Ctor { name, args } => {
                let (layout, id) = self.gc_layout_for_ctor(name, expected)?;
                if id != struct_id {
                    return None;
                }
                let (payload_cond, binds) = self.lower_gc_field_patterns(
                    value,
                    args,
                    &layout.field_types,
                    struct_id,
                    layout.field_base,
                )?;
                let cond = if let Some(tag) = layout.tag {
                    let tag_check = W::Binary {
                        op: witchy_wir::wir::BinOp::Eq,
                        kind: witchy_wir::wir::Kind::I32,
                        lhs: Box::new(W::StructGet {
                            struct_id,
                            field: 0,
                            base: Box::new(value.clone()),
                        }),
                        rhs: Box::new(W::ConstI32(tag as i32)),
                    };
                    if matches!(payload_cond, W::ConstI32(1)) {
                        tag_check
                    } else {
                        W::Control(Box::new(N::If {
                            cond: tag_check,
                            then_: vec![N::Push(payload_cond)],
                            els: vec![N::Push(W::ConstI32(0))],
                            result: Some(witchy_wir::wir::WirTy::Bool),
                        }))
                    }
                } else {
                    payload_cond
                };
                (cond, binds)
            }
            Pattern::Or(alts) => {
                let mut conds: Vec<W> = Vec::new();
                let mut binds: witchy_wir::wir::WirSeq = Vec::new();
                for alt in alts {
                    let (c, b) =
                        self.lower_gc_struct_pattern(value, alt, struct_id, expected)?;
                    if !b.is_empty() {
                        binds.push(N::If {
                            cond: c.clone(),
                            then_: b,
                            els: vec![],
                            result: None,
                        });
                    }
                    conds.push(c);
                }
                if conds.is_empty() {
                    return None;
                }
                (wir_or_chain(&conds), binds)
            }
            _ => return None,
        })
    }

    /// Lower a `match` to WIR — only when EVERY arm has a scalar pattern (and its
    /// guard/body lower). Store the scrutinee in `$MATCH_TMP`, then an outer
    /// value-`block $d` holding per-arm `block $a` (test → `br_if` skip; binds;
    /// guard; body+convert; `br $d`), then `unreachable`. `next_label` is restored
    /// on a bail.
    pub(crate) fn lower_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        let scrut_kind = self.kind_of(scrutinee);
        let result_kind = arms
            .split_first()
            .map(|(first, rest)| {
                rest.iter()
                    .fold(self.kind_of(&first.body), |acc, a| promote_kind(acc, self.kind_of(&a.body)))
            })
            .unwrap_or(Kind::I32);
        let saved = self.next_label;
        // (RFC-0035 step 4) When the scrutinee is a `list.at` READ of a provably offset-0
        // element, it was `$rc_dup`'d at the read (step 1) and is DEAD after the match — an
        // owned value with no binding — so `$rc_drop` it once, after the arms. SAME per-type
        // gate as the dup (Dict/scalar/type-var excluded ⇒ rc-region offset 0). A bare-var
        // scrutinee is a BORROW (still owned by its var), not dropped here. Because an arm
        // body may nest matches that clobber the shared MATCH_TMP, the scrutinee is copied
        // into a PER-DEPTH save slot; beyond SCRUT_POOL the drop is skipped (a sound leak).
        let depth = self.match_scrut_depth;
        let drop_scrut = scrut_kind == Kind::I32
            && !matches!(result_kind, Kind::ExternRef | Kind::GcRef(_))
            && depth < SCRUT_POOL
            && self.wm_level == 0
            && !force_copy_mode()
            && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::RcFloor)
            && match scrutinee {
                Expr::Call { name, args } if name == intrinsics::LIST_AT && args.len() == 2 => {
                    self.list_elem_is_offset0_rc(&args[0])
                }
                _ => false,
            };
        let scrut_ty = self.ast_type_of_expr(scrutinee);
        let scrut_w = self.lower_expr(scrutinee)?;
        let id = self.next_label;
        self.next_label += 1;
        // Increment AFTER `scrut_w` (whose `?` could bail without a decrement); the arm-lowering
        // bails below restore `depth`, and the success paths decrement — so it stays balanced.
        if drop_scrut {
            self.match_scrut_depth += 1;
        }
        let value = if scrut_kind == Kind::ExternRef {
            W::GetLocal(MATCH_REF_TMP.to_string())
        } else if let Kind::GcRef(id) = scrut_kind {
            W::GetLocal(match_gc_tmp(id))
        } else {
            W::GetLocal(MATCH_TMP.to_string())
        };
        let not = |c: W| W::Unary {
            op: witchy_wir::wir::UnOp::Not,
            kind: witchy_wir::wir::Kind::I32,
            arg: Box::new(c),
        };
        let mut arm_blocks: witchy_wir::wir::WirSeq = Vec::with_capacity(arms.len() + 1);
        for (i, arm) in arms.iter().enumerate() {
            let a_label = format!("a{id}_{i}");
            let (cond, binds) = match if scrut_kind == Kind::ExternRef {
                self.lower_externref_pattern(&value, &arm.pattern, scrut_ty.as_ref())
            } else if let Kind::GcRef(id) = scrut_kind {
                self.lower_gc_struct_pattern(&value, &arm.pattern, id, scrut_ty.as_ref())
            } else {
                self.lower_pattern(&value, &arm.pattern, scrut_ty.as_ref())
            } {
                Some(cb) => cb,
                None => {
                    if std::env::var_os("WIRDIAG").is_some() {
                        eprintln!(
                            "WIRBAIL lower-match-pattern: pattern={:?} scrut_ty={:?}",
                            arm.pattern, scrut_ty
                        );
                    }
                    self.next_label = saved;
                    self.match_scrut_depth = depth;
                    return None;
                }
            };
            let mut arm_body: witchy_wir::wir::WirSeq = Vec::new();
            arm_body.push(N::Br { target: a_label.clone(), cond: Some(not(cond)) });
            arm_body.extend(binds);
            if let Some(guard) = &arm.guard {
                let g = match self.lower_expr(guard) {
                    Some(w) => w,
                    None => {
                        if std::env::var_os("WIRDIAG").is_some() {
                            eprintln!("WIRBAIL lower-match-guard: guard={guard:?}");
                        }
                        self.next_label = saved;
                        self.match_scrut_depth = depth;
                        return None;
                    }
                };
                arm_body.push(N::Br { target: a_label.clone(), cond: Some(not(g)) });
            }
            let body_kind = self.kind_of(&arm.body);
            let b = match self.lower_expr(&arm.body) {
                Some(w) => w,
                None => {
                    if std::env::var_os("WIRDIAG").is_some() {
                        eprintln!("WIRBAIL lower-match-body: body={:?}", arm.body);
                    }
                    self.next_label = saved;
                    self.match_scrut_depth = depth;
                    return None;
                }
            };
            // (RFC-0035 step 4) On the drop path, stash the result into MATCH_RES so the
            // d-block yields nothing and the scrutinee `$rc_drop` can run after it.
            let arm_result = Self::wir_convert(b, body_kind, result_kind);
            if drop_scrut {
                arm_body.push(N::SetLocal {
                    local: MATCH_RES.to_string(),
                    value: W::ToSlot(Box::new(arm_result), Self::wir_kind(result_kind)),
                });
            } else {
                arm_body.push(N::Push(arm_result));
            }
            arm_body.push(N::Br { target: format!("d{id}"), cond: None });
            arm_blocks.push(N::Block { label: a_label, result: None, body: arm_body });
        }
        arm_blocks.push(N::Unreachable);
        if drop_scrut {
            self.match_scrut_depth -= 1;
            // The arms stashed their result into MATCH_RES; the `d` block yields nothing.
            // After it, `$rc_drop` the dead scrutinee — read from the PER-DEPTH save slot
            // (not MATCH_TMP, which a nested match may have clobbered), at rc-region offset 0
            // (Dict/scalar/type-var scrutinees are excluded above) — then push the result.
            let save = format!("__witchy_scrut_save_{depth}");
            let scrut_ptr =
                W::FromSlot(Box::new(W::GetLocal(save.clone())), witchy_wir::wir::Kind::I32);
            return Some(W::Seq(vec![
                N::SetLocal {
                    local: MATCH_TMP.to_string(),
                    value: W::ToSlot(Box::new(scrut_w), Self::wir_kind(scrut_kind)),
                },
                // Copy the scrutinee into its save slot BEFORE the arms run.
                N::SetLocal { local: save, value: W::GetLocal(MATCH_TMP.to_string()) },
                N::Block { label: format!("d{id}"), result: None, body: arm_blocks },
                N::Do(W::Call { func: "rc_drop".into(), args: vec![scrut_ptr] }),
                N::Push(W::FromSlot(
                    Box::new(W::GetLocal(MATCH_RES.to_string())),
                    Self::wir_kind(result_kind),
                )),
            ]));
        }
        if scrut_kind == Kind::ExternRef {
            return Some(W::Seq(vec![
                N::SetLocal {
                    local: MATCH_REF_TMP.to_string(),
                    value: scrut_w,
                },
                N::Block {
                    label: format!("d{id}"),
                    result: Some(Self::wir_ty_for_kind(result_kind)),
                    body: arm_blocks,
                },
            ]));
        }
        if let Kind::GcRef(struct_id) = scrut_kind {
            return Some(W::Seq(vec![
                N::SetLocal {
                    local: match_gc_tmp(struct_id),
                    value: scrut_w,
                },
                N::Block {
                    label: format!("d{id}"),
                    result: Some(Self::wir_ty_for_kind(result_kind)),
                    body: arm_blocks,
                },
            ]));
        }
        Some(W::Seq(vec![
            N::SetLocal {
                local: MATCH_TMP.to_string(),
                value: W::ToSlot(Box::new(scrut_w), Self::wir_kind(scrut_kind)),
            },
            N::Block {
                label: format!("d{id}"),
                result: Some(Self::wir_ty_for_kind(result_kind)),
                body: arm_blocks,
            },
        ]))
    }
}
