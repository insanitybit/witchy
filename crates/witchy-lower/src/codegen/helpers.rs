//! Structural WIR-helper generation for codegen: the equality (`$eq`),
//! to-string/render (`$ts`), and region-copy (`$rcopy`) runtime helpers, all
//! synthesized per `EqShape`. Split out of `codegen/mod.rs` as the second
//! slice of an incremental break-up of that file.

use super::*;

fn is_generated_anon_name(name: &str) -> bool {
    name.strip_prefix("__anon")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

impl Codegen {
    /// WIR twin of [`slot_cmp`] for SCALAR slots only: the comparison of two
    /// 8-byte slots at addresses `aa`/`bb`. `None` for Str/compound shapes (whose
    /// compare would need `$str_eq` or a nested eq call) so the caller bails.
    pub(crate) fn slot_cmp_wir(
        &mut self,
        shape: &EqShape,
        aa: witchy_wir::wir::WirExpr,
        bb: witchy_wir::wir::WirExpr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{BinOp, Kind, WirExpr as W};
        let load = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I64, offset: 0 };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        Some(match shape {
            EqShape::Int | EqShape::Bool => {
                W::Binary { op: BinOp::Eq, kind: Kind::I64, lhs: Box::new(load(aa)), rhs: Box::new(load(bb)) }
            }
            EqShape::Float => W::Binary {
                op: BinOp::Eq,
                kind: Kind::F64,
                lhs: Box::new(W::FromSlot(Box::new(load(aa)), Kind::F64)),
                rhs: Box::new(W::FromSlot(Box::new(load(bb)), Kind::F64)),
            },
            // String content equality: load each slot's i32 pointer and `$str_eq`.
            EqShape::Str => {
                W::Call { func: "str_eq".into(), args: vec![load_i32(aa), load_i32(bb)] }
            }
            // (RFC-0047) A field whose type has a CUSTOM PartialEq impl: call it,
            // so a custom equality is honored at every depth. The slot holds a
            // pointer to the value; the user `eq(self, other) -> Bool` takes two
            // i32 pointers and returns an i32 bool — the same ABI as `str_eq` and
            // the structural helpers.
            compound if self.custom_eq_type_of_shape(compound).is_some() => {
                let ty = self.custom_eq_type_of_shape(compound).unwrap();
                W::Call { func: format!("PartialEq__{ty}__eq"), args: vec![load_i32(aa), load_i32(bb)] }
            }
            // A compound field: the slot holds a pointer to the nested value;
            // recurse into that shape's eq helper (None → the parent bails to WAT,
            // e.g. an Adt/Dict field, or a recursive type via the cycle guard).
            compound => {
                let h = self.ensure_eq_wir_helper(compound)?;
                W::Call { func: h, args: vec![load_i32(aa), load_i32(bb)] }
            }
        })
    }

    /// (RFC-0047) The custom-eq type name for a compound shape, if its type has a
    /// user (non-derived) `PartialEq` impl — then a container comparing it calls
    /// that impl. Only Record/Adt shapes name a concrete type; List/Tuple/Dict are
    /// structural containers (their ELEMENTS may still be custom-eq, handled by the
    /// recursive `slot_cmp_wir`).
    pub(crate) fn custom_eq_type_of_shape(&self, shape: &EqShape) -> Option<String> {
        let name = match shape {
            EqShape::Record(n)
            | EqShape::RecInst(n, _)
            | EqShape::Adt(n)
            | EqShape::AdtInst(n, _)
            | EqShape::AdtRec(n, _) => n,
            _ => return None,
        };
        if self.custom_eq_types.contains(name) {
            Some(name.clone())
        } else {
            None
        }
    }

    /// WIR twin of [`ensure_eq_helper`], for shapes whose fields are all scalar
    /// (so the body has no calls and no cycles). Builds the `WirFunc` into
    /// `eq_wir_helpers` and returns its name; `None` (→ the caller bails to WAT)
    /// for any shape or field `slot_cmp_wir` can't handle.
    pub(crate) fn ensure_eq_wir_helper(&mut self, shape: &EqShape) -> Option<String> {
        let name = format!("eq_{}", shape.id());
        if self.eq_wir_helpers.contains_key(&name) {
            return Some(name);
        }
        if !self.eq_building.insert(name.clone()) {
            return None;
        }
        // Reserve the name with a placeholder BEFORE building, so a recursive
        // ADT's self-referential field compares via a `call $eq_…` back to this
        // helper (tying the knot) instead of looping forever in codegen.
        self.eq_wir_helpers.insert(name.clone(), witchy_wir::wir::WirFunc {
            name: name.clone(),
            params: vec![
                witchy_wir::wir::WirLocal { name: "a".into(), ty: witchy_wir::wir::WirTy::Bool },
                witchy_wir::wir::WirLocal { name: "b".into(), ty: witchy_wir::wir::WirTy::Bool },
            ],
            ret: vec![witchy_wir::wir::WirTy::Bool],
            locals: vec![],
            body: vec![witchy_wir::wir::WirNode::Push(witchy_wir::wir::WirExpr::ConstI32(1))],
            raw_body: None,
        });
        let built = self.build_eq_wir_body(shape);
        self.eq_building.remove(&name);
        let Some((body, locals)) = built else {
            self.eq_wir_helpers.remove(&name);
            return None;
        };
        let func = witchy_wir::wir::WirFunc {
            name: name.clone(),
            params: vec![
                witchy_wir::wir::WirLocal { name: "a".into(), ty: witchy_wir::wir::WirTy::Bool },
                witchy_wir::wir::WirLocal { name: "b".into(), ty: witchy_wir::wir::WirTy::Bool },
            ],
            ret: vec![witchy_wir::wir::WirTy::Bool],
            locals,
            body,
            raw_body: None,
        };
        self.eq_wir_helpers.insert(name.clone(), func);
        Some(name)
    }

    /// A program-specific `$key_eq` replacement for Dicts whose key type is a
    /// compound `Eq` shape. The static prelude helper handles only scalar modes
    /// 0..=2; this keeps those fast paths and appends shape-mode dispatch arms.
    pub(crate) fn dict_key_eq_wir_helper(&self) -> Option<witchy_wir::wir::WirFunc> {
        if self.dict_key_shapes.is_empty() {
            return None;
        }
        use witchy_wir::wir::{BinOp, Kind, UnOp, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |n: &str| W::GetLocal(n.into());
        let i32c = W::ConstI32;
        let wrap = |n: &str| W::FromSlot(Box::new(getl(n)), Kind::I32);
        let mode_is = |m: i32| W::Binary {
            op: BinOp::Eq,
            kind: Kind::I32,
            lhs: Box::new(getl("mode")),
            rhs: Box::new(i32c(m)),
        };
        let mut body = vec![
            N::If {
                cond: W::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("mode")) },
                then_: vec![N::Return(Some(W::Binary {
                    op: BinOp::Eq,
                    kind: Kind::I64,
                    lhs: Box::new(getl("a")),
                    rhs: Box::new(getl("b")),
                }))],
                els: vec![],
                result: None,
            },
            N::If {
                cond: mode_is(1),
                then_: vec![N::Return(Some(W::Call {
                    func: "str_eq".into(),
                    args: vec![wrap("a"), wrap("b")],
                }))],
                els: vec![],
                result: None,
            },
            N::If {
                cond: mode_is(2),
                then_: vec![N::Return(Some(W::Binary {
                    op: BinOp::Eq,
                    kind: Kind::F64,
                    lhs: Box::new(W::FromSlot(Box::new(getl("a")), Kind::F64)),
                    rhs: Box::new(W::FromSlot(Box::new(getl("b")), Kind::F64)),
                }))],
                els: vec![],
                result: None,
            },
        ];
        for (mode, shape) in &self.dict_key_shapes {
            let func = self
                .custom_eq_type_of_shape(shape)
                .map(|ty| format!("PartialEq__{ty}__eq"))
                .unwrap_or_else(|| format!("eq_{}", shape.id()));
            body.push(N::If {
                cond: mode_is(*mode as i32),
                then_: vec![N::Return(Some(W::Call {
                    func,
                    args: vec![wrap("a"), wrap("b")],
                }))],
                els: vec![],
                result: None,
            });
        }
        body.push(N::Unreachable);
        Some(witchy_wir::wir::WirFunc {
            name: "key_eq".into(),
            params: vec![
                WirLocal { name: "a".into(), ty: WirTy::Int },
                WirLocal { name: "b".into(), ty: WirTy::Int },
                WirLocal { name: "mode".into(), ty: WirTy::Bool },
            ],
            ret: vec![WirTy::Bool],
            locals: vec![],
            body,
            raw_body: None,
        })
    }

    /// The i64 a copied-out slot holds, given the SOURCE slot ADDRESS `src`: a scalar
    /// verbatim (`i64.load`), a pointer shape through its (biased) rcopy helper
    /// (`i64.extend_i32_u(rcopy_h(i32.wrap_i64(i64.load src)))`). Mirrors `slot_rcopy`.
    pub(crate) fn slot_rcopy_wir(
        &mut self,
        shape: &EqShape,
        src: witchy_wir::wir::WirExpr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{Kind as WK, WirExpr as W};
        let load = W::Load { ptr: Box::new(src), kind: WK::I64, offset: 0 };
        Some(match shape {
            EqShape::Int | EqShape::Bool | EqShape::Float => load,
            compound => {
                let h = self.ensure_rcopy_wir_helper(compound)?;
                W::Convert {
                    from: WK::I32,
                    to: WK::I64,
                    arg: Box::new(W::Call {
                        func: h,
                        args: vec![W::Convert { from: WK::I64, to: WK::I32, arg: Box::new(load) }],
                    }),
                }
            }
        })
    }

    /// Ensure the WIR rcopy helper for `shape` exists, returning its name. `Str` uses
    /// the registered `$rcopy_str`; scalars never get one. Compound shapes generate a
    /// `WirFunc` into `rcopy_wir_helpers`. The name is reserved before building,
    /// so a recursive field becomes a call back to the helper being defined.
    pub(crate) fn ensure_rcopy_wir_helper(&mut self, shape: &EqShape) -> Option<String> {
        if matches!(shape, EqShape::Str) {
            return Some("rcopy_str".to_string());
        }
        let name = format!("rcopy_{}", shape.id());
        if self.rcopy_wir_helpers.contains_key(&name) {
            return Some(name);
        }
        if !self.rcopy_building.insert(name.clone()) {
            return None;
        }
        self.rcopy_wir_helpers.insert(name.clone(), witchy_wir::wir::WirFunc {
            name: name.clone(),
            params: vec![witchy_wir::wir::WirLocal {
                name: "p".into(),
                ty: witchy_wir::wir::WirTy::Str,
            }],
            ret: vec![witchy_wir::wir::WirTy::Bool],
            locals: vec![],
            body: vec![witchy_wir::wir::WirNode::Push(
                witchy_wir::wir::WirExpr::GetLocal("p".into()),
            )],
            raw_body: None,
        });
        let built = self.build_rcopy_wir_body(shape);
        self.rcopy_building.remove(&name);
        let Some((body, locals)) = built else {
            self.rcopy_wir_helpers.remove(&name);
            return None;
        };
        let func = witchy_wir::wir::WirFunc {
            name: name.clone(),
            params: vec![witchy_wir::wir::WirLocal { name: "p".into(), ty: witchy_wir::wir::WirTy::Str }],
            ret: vec![witchy_wir::wir::WirTy::Bool],
            locals,
            body,
            raw_body: None,
        };
        self.rcopy_wir_helpers.insert(name.clone(), func);
        Some(name)
    }

    /// Allocate one region-copy payload through the RC allocator and account for
    /// the copied bytes. Every compound shape uses this exact header/counter path.
    fn rcopy_alloc_wir(size: witchy_wir::wir::WirExpr) -> witchy_wir::wir::WirSeq {
        use witchy_wir::wir::{BinOp, Kind, WirExpr as W, WirNode as N};
        let getl = |name: &str| W::GetLocal(name.into());
        let getg = |name: &str| W::GetGlobal(name.into());
        vec![
            N::SetLocal {
                local: "size".into(),
                value: size,
            },
            // `$rc_alloc` preserves the `[rc][size]` header required by reuse and
            // returns the object base expected by the slide-down bias.
            N::SetLocal {
                local: "n".into(),
                value: W::Call {
                    func: "rc_alloc".into(),
                    args: vec![getl("size")],
                },
            },
            N::SetGlobal {
                global: "__region_copy_bytes".into(),
                value: W::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(getg("__region_copy_bytes")),
                    rhs: Box::new(W::Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(getl("size")),
                    }),
                },
            },
        ]
    }

    /// Build a per-shape rcopy `WirFunc` body. Each: parent short-circuit, then
    /// allocate above the live data, fill (recursing per slot), and return the ptr
    /// pre-biased by `$rcopy_delta`.
    pub(crate) fn build_rcopy_wir_body(
        &mut self,
        shape: &EqShape,
    ) -> Option<(witchy_wir::wir::WirSeq, Vec<witchy_wir::wir::WirLocal>)> {
        use witchy_wir::wir::{BinOp, Kind as WK, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |n: &str| W::GetLocal(n.into());
        let getg = |n: &str| W::GetGlobal(n.into());
        let i32c = W::ConstI32;
        let bin = |op: BinOp, l: W, r: W| W::Binary { op, kind: WK::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: WK::I32, offset: 0 };
        let i32l = || WirTy::Bool;
        // `if p < rcopy_wm: return p` (parent-side value, shared not copied).
        let prologue = N::If {
            cond: bin(BinOp::LtU, getl("p"), getg("rcopy_wm")),
            then_: vec![N::Return(Some(getl("p")))],
            els: vec![],
            result: None,
        };
        let ret_biased = N::Push(bin(BinOp::Sub, getl("n"), getg("rcopy_delta")));
        match shape {
            EqShape::List(elem) => {
                // size = 4 + 8*len; len = list length header.
                let size = bin(BinOp::Add, i32c(4), bin(BinOp::Mul, load_i32(getl("p")), i32c(8)));
                let mut body = vec![prologue, N::SetLocal { local: "len".into(), value: load_i32(getl("p")) }];
                body.extend(Self::rcopy_alloc_wir(size));
                if matches!(**elem, EqShape::Int | EqShape::Bool | EqShape::Float) {
                    // Scalar payload: one straight copy of `[len][payload]`.
                    body.push(N::MemoryCopy { dest: getl("n"), src: getl("p"), len: getl("size") });
                } else {
                    // Compound payload: copy the length header, then rcopy each slot.
                    let slot_src = bin(
                        BinOp::Add,
                        bin(BinOp::Add, getl("p"), i32c(4)),
                        bin(BinOp::Mul, getl("i"), i32c(8)),
                    );
                    let slot_dst = bin(
                        BinOp::Add,
                        bin(BinOp::Add, getl("n"), i32c(4)),
                        bin(BinOp::Mul, getl("i"), i32c(8)),
                    );
                    let slot_val = self.slot_rcopy_wir(elem, slot_src)?;
                    body.push(N::Store { ptr: getl("n"), value: getl("len"), kind: WK::I32, offset: 0 });
                    body.push(N::SetLocal { local: "i".into(), value: i32c(0) });
                    body.push(N::Block {
                        label: "done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "l".into(),
                            body: vec![
                                N::Br { target: "done".into(), cond: Some(bin(BinOp::Ge, getl("i"), getl("len"))) },
                                N::Store { ptr: slot_dst, value: slot_val, kind: WK::I64, offset: 0 },
                                N::SetLocal { local: "i".into(), value: bin(BinOp::Add, getl("i"), i32c(1)) },
                                N::Br { target: "l".into(), cond: None },
                            ],
                        }],
                    });
                }
                body.push(ret_biased);
                Some((
                    body,
                    vec![
                        WirLocal { name: "n".into(), ty: i32l() },
                        WirLocal { name: "size".into(), ty: i32l() },
                        WirLocal { name: "i".into(), ty: i32l() },
                        WirLocal { name: "len".into(), ty: i32l() },
                    ],
                ))
            }
            EqShape::Tuple(shapes) => {
                let nslots = shapes.len();
                let mut body = vec![prologue];
                body.extend(Self::rcopy_alloc_wir(i32c((4 + 8 * nslots) as i32)));
                // Copy the tag word (slot 0), then rcopy each field slot.
                body.push(N::Store { ptr: getl("n"), value: load_i32(getl("p")), kind: WK::I32, offset: 0 });
                for (i, fs) in shapes.iter().enumerate() {
                    let off = (4 + 8 * i) as i32;
                    let slot_val = self.slot_rcopy_wir(fs, bin(BinOp::Add, getl("p"), i32c(off)))?;
                    body.push(N::Store { ptr: bin(BinOp::Add, getl("n"), i32c(off)), value: slot_val, kind: WK::I64, offset: 0 });
                }
                body.push(ret_biased);
                Some((
                    body,
                    vec![
                        WirLocal { name: "n".into(), ty: i32l() },
                        WirLocal { name: "size".into(), ty: i32l() },
                    ],
                ))
            }
            // (BUG-407) A record has the same flat `[tag i32][slot i64]…` layout as a
            // tuple, so its copy-out is the tuple body over its RESOLVED field shapes —
            // deep-copying each slot through `slot_rcopy_wir`. This closes the shape
            // dependence for the common `region -> SomeRecord:` result; recursive
            // fields call the helper name reserved by `ensure_rcopy_wir_helper`.
            EqShape::Record(tyname) => {
                let field_tys = self.record_field_types.get(tyname).cloned()?;
                let mut shapes = Vec::new();
                for fty in &field_tys {
                    shapes.push(self.eq_shape_of_type(fty)?);
                }
                self.build_rcopy_wir_body(&EqShape::Tuple(shapes))
            }
            // A GENERIC record instantiation (`Box(Int)`): resolve each field type UNDER
            // the argument substitution, the record analogue of `AdtRec` — matching the
            // `RecInst` eq arm (BUG-319), so a generic-record region result copies out.
            EqShape::RecInst(tyname, args) => {
                let field_tys = self.record_field_types.get(tyname).cloned()?;
                let subst = self.record_field_subst(tyname, args);
                let mut shapes = Vec::new();
                for fty in &field_tys {
                    shapes.push(self.eq_shape_of_type_with(fty, &subst)?);
                }
                self.build_rcopy_wir_body(&EqShape::Tuple(shapes))
            }
            EqShape::Adt(tyname) => {
                let variants = self.adt_variants.get(tyname).cloned()?;
                let mut all = Vec::new();
                for fields in &variants {
                    let mut shapes = Vec::new();
                    for field in fields {
                        shapes.push(self.eq_shape_of_type(field)?);
                    }
                    all.push(shapes);
                }
                self.build_variant_rcopy_wir(&all)
            }
            EqShape::AdtInst(_, variants) => self.build_variant_rcopy_wir(variants),
            EqShape::AdtRec(tyname, args) => {
                let variants = self.adt_variants.get(tyname).cloned()?;
                let mut params = Vec::new();
                for fields in &variants {
                    for field in fields {
                        collect_type_vars(field, &mut params);
                    }
                }
                let subst: HashMap<String, EqShape> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();
                let mut all = Vec::new();
                for fields in &variants {
                    let mut shapes = Vec::new();
                    for field in fields {
                        shapes.push(self.eq_shape_of_type_with(field, &subst)?);
                    }
                    all.push(shapes);
                }
                self.build_variant_rcopy_wir(&all)
            }
            EqShape::Dict(key, value) => {
                let mut body = vec![
                    prologue,
                    N::SetLocal {
                        local: "len".into(),
                        value: load_i32(getl("p")),
                    },
                ];
                let size = bin(
                    BinOp::Add,
                    i32c(8),
                    bin(BinOp::Mul, getl("len"), i32c(16)),
                );
                body.extend(Self::rcopy_alloc_wir(size));
                body.push(N::Store {
                    ptr: getl("n"),
                    value: i32c(0),
                    kind: WK::I32,
                    offset: 0,
                });
                body.push(N::Store {
                    ptr: getl("n"),
                    value: getl("len"),
                    kind: WK::I32,
                    offset: 4,
                });
                body.push(N::SetLocal {
                    local: "i".into(),
                    value: i32c(0),
                });
                let entry_off = bin(BinOp::Mul, getl("i"), i32c(16));
                let key_value = self.slot_rcopy_wir(
                    key,
                    bin(
                        BinOp::Add,
                        bin(BinOp::Add, getl("p"), i32c(4)),
                        entry_off.clone(),
                    ),
                )?;
                let value_value = self.slot_rcopy_wir(
                    value,
                    bin(
                        BinOp::Add,
                        bin(BinOp::Add, getl("p"), i32c(12)),
                        entry_off.clone(),
                    ),
                )?;
                let dest = bin(BinOp::Add, getl("n"), entry_off);
                body.push(N::Block {
                    label: "done".into(),
                    result: None,
                    body: vec![N::Loop {
                        label: "l".into(),
                        body: vec![
                            N::Br {
                                target: "done".into(),
                                cond: Some(bin(BinOp::Ge, getl("i"), getl("len"))),
                            },
                            N::Store {
                                ptr: dest.clone(),
                                value: key_value,
                                kind: WK::I64,
                                offset: 8,
                            },
                            N::Store {
                                ptr: dest,
                                value: value_value,
                                kind: WK::I64,
                                offset: 16,
                            },
                            N::SetLocal {
                                local: "i".into(),
                                value: bin(BinOp::Add, getl("i"), i32c(1)),
                            },
                            N::Br {
                                target: "l".into(),
                                cond: None,
                            },
                        ],
                    }],
                });
                body.push(N::Push(bin(
                    BinOp::Sub,
                    bin(BinOp::Add, getl("n"), i32c(4)),
                    getg("rcopy_delta"),
                )));
                Some((
                    body,
                    vec![
                        WirLocal {
                            name: "n".into(),
                            ty: i32l(),
                        },
                        WirLocal {
                            name: "size".into(),
                            ty: i32l(),
                        },
                        WirLocal {
                            name: "i".into(),
                            ty: i32l(),
                        },
                        WirLocal {
                            name: "len".into(),
                            ty: i32l(),
                        },
                    ],
                ))
            }
            EqShape::Int | EqShape::Bool | EqShape::Float | EqShape::Str => None,
        }
    }

    /// Build a tag-dispatched region copy-out for an ADT. Each variant allocates
    /// exactly its active payload size, copies the tag, and recursively copies its
    /// fields. Recursive fields call the placeholder reserved by
    /// `ensure_rcopy_wir_helper`.
    pub(crate) fn build_variant_rcopy_wir(
        &mut self,
        variants: &[Vec<EqShape>],
    ) -> Option<(witchy_wir::wir::WirSeq, Vec<witchy_wir::wir::WirLocal>)> {
        use witchy_wir::wir::{BinOp, Kind as WK, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |name: &str| W::GetLocal(name.into());
        let getg = |name: &str| W::GetGlobal(name.into());
        let i32c = W::ConstI32;
        let bin = |op: BinOp, lhs: W, rhs: W| W::Binary {
            op,
            kind: WK::I32,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        let mut body = vec![N::If {
            cond: bin(BinOp::LtU, getl("p"), getg("rcopy_wm")),
            then_: vec![N::Return(Some(getl("p")))],
            els: vec![],
            result: None,
        }];
        body.push(N::SetLocal {
            local: "tag".into(),
            value: W::Load {
                ptr: Box::new(getl("p")),
                kind: WK::I32,
                offset: 0,
            },
        });
        for (tag, fields) in variants.iter().enumerate() {
            let size = (4 + 8 * fields.len()) as i32;
            let mut copy = Self::rcopy_alloc_wir(i32c(size));
            copy.push(N::Store {
                ptr: getl("n"),
                value: getl("tag"),
                kind: WK::I32,
                offset: 0,
            });
            for (index, field) in fields.iter().enumerate() {
                let offset = (4 + 8 * index) as i32;
                let value = self.slot_rcopy_wir(
                    field,
                    bin(BinOp::Add, getl("p"), i32c(offset)),
                )?;
                copy.push(N::Store {
                    ptr: getl("n"),
                    value,
                    kind: WK::I64,
                    offset: offset as u32,
                });
            }
            copy.push(N::Return(Some(bin(
                BinOp::Sub,
                getl("n"),
                getg("rcopy_delta"),
            ))));
            body.push(N::If {
                cond: bin(BinOp::Eq, getl("tag"), i32c(tag as i32)),
                then_: copy,
                els: vec![],
                result: None,
            });
        }
        body.push(N::Unreachable);
        Some((
            body,
            vec![
                WirLocal {
                    name: "n".into(),
                    ty: WirTy::Bool,
                },
                WirLocal {
                    name: "size".into(),
                    ty: WirTy::Bool,
                },
                WirLocal {
                    name: "tag".into(),
                    ty: WirTy::Bool,
                },
            ],
        ))
    }

    /// Build the `(body, locals)` of a structural-eq helper for `shape`. `None`
    /// for shapes/fields not yet handled (Adt, Dict, or a non-buildable nested
    /// field). Recurses through `slot_cmp_wir` for compound fields.
    pub(crate) fn build_eq_wir_body(&mut self, shape: &EqShape) -> Option<(witchy_wir::wir::WirSeq, Vec<witchy_wir::wir::WirLocal>)> {
        use witchy_wir::wir::{BinOp, Kind, UnOp, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |n: &str| W::GetLocal(n.into());
        let i32c = W::ConstI32;
        let add = |l: W, r: W| W::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let not = |e: W| W::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(e) };
        let check = |cmp: W| N::If { cond: not(cmp), then_: vec![N::Return(Some(i32c(0)))], els: vec![], result: None };
        let bool_local = |n: &str| WirLocal { name: n.into(), ty: WirTy::Bool };

        // Build the per-field checks for a flat record/tuple/variant whose field
        // shapes are `fields`, reading slots at `base+4+8*i`. None if any non-scalar.
        let (body, locals): (witchy_wir::wir::WirSeq, Vec<WirLocal>) = match shape {
            EqShape::Tuple(fields) => {
                let mut b: witchy_wir::wir::WirSeq = Vec::new();
                for (i, f) in fields.iter().enumerate() {
                    let off = i32c((4 + 8 * i) as i32);
                    let cmp = self.slot_cmp_wir(f, add(getl("a"), off.clone()), add(getl("b"), off))?;
                    b.push(check(cmp));
                }
                b.push(N::Push(i32c(1)));
                (b, vec![])
            }
            EqShape::Record(tyname) => {
                let fields = self.record_field_types.get(tyname).cloned()?;
                let mut b: witchy_wir::wir::WirSeq = Vec::new();
                for (i, fty) in fields.iter().enumerate() {
                    let fshape = self.eq_shape_of_type(fty)?;
                    let off = i32c((4 + 8 * i) as i32);
                    let cmp = self.slot_cmp_wir(&fshape, add(getl("a"), off.clone()), add(getl("b"), off))?;
                    b.push(check(cmp));
                }
                b.push(N::Push(i32c(1)));
                (b, vec![])
            }
            // A GENERIC record instantiation (`Box(Int)`): resolve each field type
            // UNDER the argument substitution — the record analogue of `AdtRec`. The
            // plain `Record` arm resolves fields with an EMPTY subst, so a generic
            // field (`item: a`) came back `None` and the whole `==` was rejected even
            // when fully annotated (BUG-319). A self-referential field resolves back
            // to this same shape, whose helper name is already reserved.
            EqShape::RecInst(tyname, args) => {
                let fields = self.record_field_types.get(tyname).cloned()?;
                let subst = self.record_field_subst(tyname, args);
                let mut b: witchy_wir::wir::WirSeq = Vec::new();
                for (i, fty) in fields.iter().enumerate() {
                    let fshape = self.eq_shape_of_type_with(fty, &subst)?;
                    let off = i32c((4 + 8 * i) as i32);
                    let cmp = self.slot_cmp_wir(&fshape, add(getl("a"), off.clone()), add(getl("b"), off))?;
                    b.push(check(cmp));
                }
                b.push(N::Push(i32c(1)));
                (b, vec![])
            }
            EqShape::List(elem) => {
                let idx_off = |base: &str| {
                    add(
                        add(getl(base), i32c(4)),
                        W::Binary { op: BinOp::Mul, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(8)) },
                    )
                };
                let cmp = self.slot_cmp_wir(elem, idx_off("a"), idx_off("b"))?;
                let b: witchy_wir::wir::WirSeq = vec![
                    // lengths differ → not equal
                    N::If {
                        cond: W::Binary { op: BinOp::Ne, kind: Kind::I32, lhs: Box::new(load_i32(getl("a"))), rhs: Box::new(load_i32(getl("b"))) },
                        then_: vec![N::Return(Some(i32c(0)))],
                        els: vec![],
                        result: None,
                    },
                    N::SetLocal { local: "n".into(), value: load_i32(getl("a")) },
                    N::SetLocal { local: "i".into(), value: i32c(0) },
                    N::Block {
                        label: "done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "l".into(),
                            body: vec![
                                N::Br { target: "done".into(), cond: Some(W::Binary { op: BinOp::Ge, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(getl("n")) }) },
                                check(cmp),
                                N::SetLocal { local: "i".into(), value: add(getl("i"), i32c(1)) },
                                N::Br { target: "l".into(), cond: None },
                            ],
                        }],
                    },
                    N::Push(i32c(1)),
                ];
                (b, vec![bool_local("n"), bool_local("i")])
            }
            // Enum (Adt) structural `==`: tag-dispatch then per-variant field
            // compares, mirroring the WAT `ensure_eq_helper` arms (which are also
            // structural, so binary == WAT == interpreter all agree). A generic
            // payload that the comparison site couldn't resolve to a concrete shape
            // (a bare type variable) makes `eq_shape_of_type` return None below → the
            // `?` bails to WAT. Recursive ADTs bail via the `ensure_eq_wir_helper`
            // cycle guard.
            EqShape::Adt(tyname) => {
                let variants = self.adt_variants.get(tyname).cloned()?;
                let mut all: Vec<Vec<EqShape>> = Vec::new();
                for fs in &variants {
                    let mut shapes = Vec::new();
                    for f in fs {
                        shapes.push(self.eq_shape_of_type(f)?);
                    }
                    all.push(shapes);
                }
                return self.build_variant_eq_wir(&all);
            }
            EqShape::AdtInst(_, variant_shapes) => {
                let all = variant_shapes.clone();
                return self.build_variant_eq_wir(&all);
            }
            EqShape::AdtRec(tyname, args) => {
                let variants = self.adt_variants.get(tyname).cloned()?;
                let mut params: Vec<String> = Vec::new();
                for fields in &variants {
                    for f in fields {
                        collect_type_vars(f, &mut params);
                    }
                }
                let subst: HashMap<String, EqShape> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();
                let mut all: Vec<Vec<EqShape>> = Vec::new();
                for fs in &variants {
                    let mut shapes = Vec::new();
                    for f in fs {
                        shapes.push(self.eq_shape_of_type_with(f, &subst)?);
                    }
                    all.push(shapes);
                }
                return self.build_variant_eq_wir(&all);
            }
            // Dict `==`: insertion-order pairwise compare over the
            // `[count][key slot, value slot]…` entries (16-byte stride), exactly
            // the interpreter's `Vec<(K, V)>` equality and the WAT `$eq_dict_*`.
            EqShape::Dict(k, v) => {
                let entry = |base: &str, off: i32| {
                    add(
                        add(getl(base), i32c(off)),
                        W::Binary { op: BinOp::Mul, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(16)) },
                    )
                };
                let kcmp = self.slot_cmp_wir(k, entry("a", 4), entry("b", 4))?;
                let vcmp = self.slot_cmp_wir(v, entry("a", 12), entry("b", 12))?;
                let b: witchy_wir::wir::WirSeq = vec![
                    // counts differ → not equal
                    N::If {
                        cond: W::Binary { op: BinOp::Ne, kind: Kind::I32, lhs: Box::new(load_i32(getl("a"))), rhs: Box::new(load_i32(getl("b"))) },
                        then_: vec![N::Return(Some(i32c(0)))],
                        els: vec![],
                        result: None,
                    },
                    N::SetLocal { local: "n".into(), value: load_i32(getl("a")) },
                    N::SetLocal { local: "i".into(), value: i32c(0) },
                    N::Block {
                        label: "done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "l".into(),
                            body: vec![
                                N::Br { target: "done".into(), cond: Some(W::Binary { op: BinOp::Ge, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(getl("n")) }) },
                                check(kcmp),
                                check(vcmp),
                                N::SetLocal { local: "i".into(), value: add(getl("i"), i32c(1)) },
                                N::Br { target: "l".into(), cond: None },
                            ],
                        }],
                    },
                    N::Push(i32c(1)),
                ];
                (b, vec![bool_local("n"), bool_local("i")])
            }
            // Scalars never reach here (compared inline by `slot_cmp_wir`, never
            // via a helper).
            EqShape::Int | EqShape::Bool | EqShape::Float | EqShape::Str => return None,
        };
        Some((body, locals))
    }

    /// The tag-dispatch body of an enum `==`: tags differ → return 0, else load the
    /// shared tag and, for the matching variant, compare its fields (slot `4+8*i`)
    /// via `slot_cmp_wir`; a nullary or fully-equal variant → 1. `all` is the
    /// per-variant resolved field shapes. Mirrors the WAT `ensure_eq_helper` Adt arm.
    pub(crate) fn build_variant_eq_wir(
        &mut self,
        all: &[Vec<EqShape>],
    ) -> Option<(witchy_wir::wir::WirSeq, Vec<witchy_wir::wir::WirLocal>)> {
        use witchy_wir::wir::{BinOp, Kind, UnOp, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |n: &str| W::GetLocal(n.into());
        let i32c = W::ConstI32;
        let add = |l: W, r: W| W::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let not = |e: W| W::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(e) };
        let eqi = |l: W, r: W| W::Binary { op: BinOp::Eq, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let nei = |l: W, r: W| W::Binary { op: BinOp::Ne, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let mut b: witchy_wir::wir::WirSeq = Vec::new();
        b.push(N::If {
            cond: nei(load_i32(getl("a")), load_i32(getl("b"))),
            then_: vec![N::Return(Some(i32c(0)))],
            els: vec![],
            result: None,
        });
        b.push(N::SetLocal { local: "t".into(), value: load_i32(getl("a")) });
        for (tag, fields) in all.iter().enumerate() {
            if fields.is_empty() {
                continue;
            }
            let mut checks: witchy_wir::wir::WirSeq = Vec::new();
            for (i, fshape) in fields.iter().enumerate() {
                let off = i32c((4 + 8 * i) as i32);
                let cmp = self.slot_cmp_wir(fshape, add(getl("a"), off.clone()), add(getl("b"), off))?;
                checks.push(N::If { cond: not(cmp), then_: vec![N::Return(Some(i32c(0)))], els: vec![], result: None });
            }
            checks.push(N::Return(Some(i32c(1))));
            b.push(N::If {
                cond: eqi(getl("t"), i32c(tag as i32)),
                then_: checks,
                els: vec![],
                result: None,
            });
        }
        b.push(N::Push(i32c(1)));
        Some((b, vec![WirLocal { name: "t".into(), ty: WirTy::Bool }]))
    }

    /// WIR twin of [`render_slot`]: the String pointer rendering the 8-byte slot
    /// at `addr`. Int → `$int_to_string`, Bool → an interned "true"/"false"
    /// value-if, Str → the pointer, compound → that shape's `$ts` helper. `None`
    /// for Float (needs the `$float_to_str` host import) or an unbuildable nested.
    pub(crate) fn slot_render_wir(&mut self, shape: &EqShape, addr: witchy_wir::wir::WirExpr) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{Kind, WirExpr as W, WirNode as N, WirTy};
        let load_i64 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I64, offset: 0 };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        Some(match shape {
            EqShape::Int => {
                self.uses_int_to_string = true;
                W::Call { func: "int_to_string".into(), args: vec![load_i64(addr)] }
            }
            EqShape::Bool => {
                let t = self.intern("true");
                let f = self.intern("false");
                W::Control(Box::new(N::If {
                    cond: W::FromSlot(Box::new(load_i64(addr)), Kind::I32),
                    then_: vec![N::Push(W::StrPtr(t))],
                    els: vec![N::Push(W::StrPtr(f))],
                    result: Some(WirTy::Str),
                }))
            }
            EqShape::Str => load_i32(addr),
            EqShape::Float => {
                self.uses_float_to_str = true;
                W::Call { func: "float_to_str".into(), args: vec![W::FromSlot(Box::new(load_i64(addr)), Kind::F64)] }
            }
            compound => {
                let h = self.ensure_ts_wir_helper(compound)?;
                W::Call { func: h, args: vec![load_i32(addr)] }
            }
        })
    }

    /// WIR twin of [`ensure_ts_helper`], for Tuple/List shapes whose fields all
    /// render via `slot_render_wir`. Cycle-guarded like the eq helpers.
    pub(crate) fn ensure_ts_wir_helper(&mut self, shape: &EqShape) -> Option<String> {
        let name = format!("ts_{}", shape.id());
        if self.ts_wir_helpers.contains_key(&name) {
            return Some(name);
        }
        if !self.ts_building.insert(name.clone()) {
            return None;
        }
        // Reserve the name with a placeholder BEFORE building the body, so a
        // self-referential field (a recursive ADT like JsonValue, whose
        // JsonArray(List(JsonValue)) field renders this very shape) ties back
        // through the `contains_key` check above — the recursion becomes a
        // `call $ts_…` to this helper rather than an infinite inline expansion.
        let empty = self.intern("");
        self.ts_wir_helpers.insert(name.clone(), witchy_wir::wir::WirFunc {
            name: name.clone(),
            params: vec![witchy_wir::wir::WirLocal { name: "p".into(), ty: witchy_wir::wir::WirTy::Bool }],
            ret: vec![witchy_wir::wir::WirTy::Str],
            locals: vec![],
            body: vec![witchy_wir::wir::WirNode::Push(witchy_wir::wir::WirExpr::StrPtr(empty))],
            raw_body: None,
        });
        let built = self.build_ts_wir_body(shape);
        self.ts_building.remove(&name);
        let Some((body, locals)) = built else {
            // A nested shape couldn't be built — drop the placeholder so the
            // whole render bails cleanly (and a later re-attempt rebuilds).
            self.ts_wir_helpers.remove(&name);
            return None;
        };
        let func = witchy_wir::wir::WirFunc {
            name: name.clone(),
            params: vec![witchy_wir::wir::WirLocal { name: "p".into(), ty: witchy_wir::wir::WirTy::Bool }],
            ret: vec![witchy_wir::wir::WirTy::Str],
            locals,
            body,
            raw_body: None,
        };
        self.ts_wir_helpers.insert(name.clone(), func);
        Some(name)
    }

    /// Build the `(body, locals)` of a `$ts` renderer: a tuple `(f0, f1)` or a
    /// list `[e0, e1]`, accumulating with `$concat`. `None` for Record/Adt/etc.
    pub(crate) fn build_ts_wir_body(&mut self, shape: &EqShape) -> Option<(witchy_wir::wir::WirSeq, Vec<witchy_wir::wir::WirLocal>)> {
        use witchy_wir::wir::{BinOp, Kind, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |n: &str| W::GetLocal(n.into());
        let i32c = W::ConstI32;
        let add = |l: W, r: W| W::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let concat = |a: W, b: W| W::Call { func: "concat".into(), args: vec![a, b] };
        let setl = |n: &str, v: W| N::SetLocal { local: n.into(), value: v };
        let bool_local = |n: &str| WirLocal { name: n.into(), ty: WirTy::Bool };
        match shape {
            EqShape::Tuple(fields) => {
                let (open, close, comma) = (self.intern("("), self.intern(")"), self.intern(", "));
                let mut body: witchy_wir::wir::WirSeq = vec![setl("acc", W::StrPtr(open))];
                for (i, f) in fields.iter().enumerate() {
                    let render = self.slot_render_wir(f, add(getl("p"), i32c((4 + 8 * i) as i32)))?;
                    if i > 0 {
                        body.push(setl("acc", concat(getl("acc"), W::StrPtr(comma))));
                    }
                    body.push(setl("acc", concat(getl("acc"), render)));
                }
                body.push(N::Push(concat(getl("acc"), W::StrPtr(close))));
                Some((body, vec![bool_local("acc")]))
            }
            EqShape::List(elem) => {
                let (open, close, comma) = (self.intern("["), self.intern("]"), self.intern(", "));
                let render = self.slot_render_wir(
                    elem,
                    add(add(getl("p"), i32c(4)), W::Binary { op: BinOp::Mul, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(8)) }),
                )?;
                let body: witchy_wir::wir::WirSeq = vec![
                    setl("n", W::Load { ptr: Box::new(getl("p")), kind: Kind::I32, offset: 0 }),
                    setl("acc", W::StrPtr(open)),
                    setl("i", i32c(0)),
                    N::Block {
                        label: "done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "l".into(),
                            body: vec![
                                N::Br { target: "done".into(), cond: Some(W::Binary { op: BinOp::Ge, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(getl("n")) }) },
                                N::If {
                                    cond: W::Binary { op: BinOp::Gt, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(0)) },
                                    then_: vec![setl("acc", concat(getl("acc"), W::StrPtr(comma)))],
                                    els: vec![],
                                    result: None,
                                },
                                setl("acc", concat(getl("acc"), render)),
                                setl("i", add(getl("i"), i32c(1))),
                                N::Br { target: "l".into(), cond: None },
                            ],
                        }],
                    },
                    N::Push(concat(getl("acc"), W::StrPtr(close))),
                ];
                Some((body, vec![bool_local("n"), bool_local("i"), bool_local("acc")]))
            }
            // A record renders as `Name(f0, f1, ...)` — like a tuple with the type
            // name as the opening token (matching the single ctor it lowers to).
            EqShape::Record(tyname) => {
                let fields = self.record_field_types.get(tyname).cloned()?;
                let names = self.record_fields.get(tyname).cloned()?;
                let anonymous = is_generated_anon_name(tyname);
                // (RFC-0042) Render the unqualified type name (`P`), not the
                // canonical `main.P` — matching the interpreter's Display.
                let shown = tyname.rsplit_once('.').map_or(tyname.as_str(), |(_, c)| c);
                let header_text = if anonymous { ".{".to_string() } else { format!("{shown}(") };
                let header = self.intern(&header_text);
                let close = self.intern(if anonymous { "}" } else { ")" });
                let (comma, colon) = (self.intern(", "), self.intern(": "));
                let mut body: witchy_wir::wir::WirSeq = vec![setl("acc", W::StrPtr(header))];
                for (i, fty) in fields.iter().enumerate() {
                    let fshape = self.eq_shape_of_type(fty)?;
                    let render = self.slot_render_wir(&fshape, add(getl("p"), i32c((4 + 8 * i) as i32)))?;
                    if i > 0 {
                        body.push(setl("acc", concat(getl("acc"), W::StrPtr(comma))));
                    }
                    if anonymous {
                        let label = self.intern(&names.get(i)?.0);
                        body.push(setl("acc", concat(getl("acc"), W::StrPtr(label))));
                        body.push(setl("acc", concat(getl("acc"), W::StrPtr(colon))));
                    }
                    body.push(setl("acc", concat(getl("acc"), render)));
                }
                body.push(N::Push(concat(getl("acc"), W::StrPtr(close))));
                Some((body, vec![bool_local("acc")]))
            }
            // A generic record instantiation renders `Name(f0, f1, …)` like a plain
            // record, but resolves each field UNDER the argument substitution (so a
            // generic field `item: a` renders per `Box(Int)`). Mirrors the eq
            // `RecInst` arm and the render `AdtRec` arm.
            EqShape::RecInst(tyname, args) => {
                let fields = self.record_field_types.get(tyname).cloned()?;
                let names = self.record_fields.get(tyname).cloned()?;
                let subst = self.record_field_subst(tyname, args);
                let anonymous = is_generated_anon_name(tyname);
                let shown = tyname.rsplit_once('.').map_or(tyname.as_str(), |(_, c)| c);
                let header_text = if anonymous { ".{".to_string() } else { format!("{shown}(") };
                let header = self.intern(&header_text);
                let close = self.intern(if anonymous { "}" } else { ")" });
                let (comma, colon) = (self.intern(", "), self.intern(": "));
                let mut body: witchy_wir::wir::WirSeq = vec![setl("acc", W::StrPtr(header))];
                for (i, fty) in fields.iter().enumerate() {
                    let fshape = self.eq_shape_of_type_with(fty, &subst)?;
                    let render = self.slot_render_wir(&fshape, add(getl("p"), i32c((4 + 8 * i) as i32)))?;
                    if i > 0 {
                        body.push(setl("acc", concat(getl("acc"), W::StrPtr(comma))));
                    }
                    if anonymous {
                        let label = self.intern(&names.get(i)?.0);
                        body.push(setl("acc", concat(getl("acc"), W::StrPtr(label))));
                        body.push(setl("acc", concat(getl("acc"), W::StrPtr(colon))));
                    }
                    body.push(setl("acc", concat(getl("acc"), render)));
                }
                body.push(N::Push(concat(getl("acc"), W::StrPtr(close))));
                Some((body, vec![bool_local("acc")]))
            }
            // Enum (Adt) render: dispatch on the tag to `Name` (nullary) or
            // `Name(f0, f1, ...)`. Matches the interpreter's `Value::Ctor` Display
            // (records are also Ctors, rendered positionally) AND the WAT path.
            EqShape::Adt(tyname) => {
                let variants = self.adt_variants.get(tyname).cloned()?;
                let names = self.adt_variant_names.get(tyname).cloned()?;
                let mut all: Vec<Vec<EqShape>> = Vec::new();
                for fs in &variants {
                    let mut shapes = Vec::new();
                    for f in fs {
                        shapes.push(self.eq_shape_of_type(f)?);
                    }
                    all.push(shapes);
                }
                self.build_variant_ts_wir(&names, &all)
            }
            EqShape::AdtInst(tyname, variant_shapes) => {
                let names = self.adt_variant_names.get(tyname).cloned()?;
                let all = variant_shapes.clone();
                self.build_variant_ts_wir(&names, &all)
            }
            // A dict renders as `{k: v, ...}` over its `[count][key slot, value slot]…`
            // entries (16-byte stride), matching the interpreter's `Value::Dict` order.
            EqShape::Dict(k, v) => {
                let (open, close, comma, colon) =
                    (self.intern("{"), self.intern("}"), self.intern(", "), self.intern(": "));
                let stride = |off: i32| {
                    add(
                        add(getl("p"), i32c(off)),
                        W::Binary { op: BinOp::Mul, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(16)) },
                    )
                };
                let krender = self.slot_render_wir(k, stride(4))?;
                let vrender = self.slot_render_wir(v, stride(12))?;
                let body: witchy_wir::wir::WirSeq = vec![
                    setl("n", W::Load { ptr: Box::new(getl("p")), kind: Kind::I32, offset: 0 }),
                    setl("acc", W::StrPtr(open)),
                    setl("i", i32c(0)),
                    N::Block {
                        label: "done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "l".into(),
                            body: vec![
                                N::Br { target: "done".into(), cond: Some(W::Binary { op: BinOp::Ge, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(getl("n")) }) },
                                N::If {
                                    cond: W::Binary { op: BinOp::Gt, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(0)) },
                                    then_: vec![setl("acc", concat(getl("acc"), W::StrPtr(comma)))],
                                    els: vec![],
                                    result: None,
                                },
                                setl("acc", concat(getl("acc"), krender)),
                                setl("acc", concat(getl("acc"), W::StrPtr(colon))),
                                setl("acc", concat(getl("acc"), vrender)),
                                setl("i", add(getl("i"), i32c(1))),
                                N::Br { target: "l".into(), cond: None },
                            ],
                        }],
                    },
                    N::Push(concat(getl("acc"), W::StrPtr(close))),
                ];
                Some((body, vec![bool_local("n"), bool_local("i"), bool_local("acc")]))
            }
            // A generic self-recursive ADT instantiation (`Stack(Int)`): expand
            // one level of each variant's fields under the argument substitution.
            // A self-referential field resolves back to this shape, whose `$ts`
            // helper name is reserved (the placeholder in `ensure_ts_wir_helper`),
            // so it renders via a recursive `call`. Mirrors the eq `AdtRec` arm.
            EqShape::AdtRec(tyname, args) => {
                let variants = self.adt_variants.get(tyname).cloned()?;
                let names = self.adt_variant_names.get(tyname).cloned()?;
                let mut params: Vec<String> = Vec::new();
                for fields in &variants {
                    for f in fields {
                        collect_type_vars(f, &mut params);
                    }
                }
                let subst: HashMap<String, EqShape> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();
                let mut all: Vec<Vec<EqShape>> = Vec::new();
                for fs in &variants {
                    let mut shapes = Vec::new();
                    for f in fs {
                        shapes.push(self.eq_shape_of_type_with(f, &subst)?);
                    }
                    all.push(shapes);
                }
                self.build_variant_ts_wir(&names, &all)
            }
            EqShape::Int | EqShape::Bool | EqShape::Float | EqShape::Str => None,
        }
    }

    /// The tag-dispatch body of an enum `__render`: load the tag, and for the
    /// matching variant emit `Name` (nullary) or `Name(f0, f1, ...)` accumulating
    /// each field's `slot_render_wir` with `$concat`. `ctor_names`/`all` are the
    /// per-variant names and resolved field shapes. Mirrors the WAT `ts_adt_body`.
    pub(crate) fn build_variant_ts_wir(
        &mut self,
        ctor_names: &[String],
        all: &[Vec<EqShape>],
    ) -> Option<(witchy_wir::wir::WirSeq, Vec<witchy_wir::wir::WirLocal>)> {
        use witchy_wir::wir::{BinOp, Kind, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |n: &str| W::GetLocal(n.into());
        let i32c = W::ConstI32;
        let add = |l: W, r: W| W::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let concat = |a: W, b: W| W::Call { func: "concat".into(), args: vec![a, b] };
        let setl = |n: &str, v: W| N::SetLocal { local: n.into(), value: v };
        let eqi = |l: W, r: W| W::Binary { op: BinOp::Eq, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let (open, close, comma) = (self.intern("("), self.intern(")"), self.intern(", "));
        let mut b: witchy_wir::wir::WirSeq = vec![setl("t", load_i32(getl("p")))];
        for (tag, fields) in all.iter().enumerate() {
            // (RFC-0042) Render the unqualified variant name (`Item`), not the
            // canonical `module.Ctor` the tag table carries — matching the
            // interpreter's `Value::Ctor` Display so both backends print alike.
            let raw = ctor_names.get(tag).map(|s| s.as_str()).unwrap_or("?");
            let label = self.intern(raw.rsplit_once('.').map_or(raw, |(_, c)| c));
            if fields.is_empty() {
                b.push(N::If {
                    cond: eqi(getl("t"), i32c(tag as i32)),
                    then_: vec![N::Return(Some(W::StrPtr(label)))],
                    els: vec![],
                    result: None,
                });
                continue;
            }
            let mut arm: witchy_wir::wir::WirSeq = vec![
                setl("acc", W::StrPtr(label)),
                setl("acc", concat(getl("acc"), W::StrPtr(open))),
            ];
            for (i, fshape) in fields.iter().enumerate() {
                let render = self.slot_render_wir(fshape, add(getl("p"), i32c((4 + 8 * i) as i32)))?;
                if i > 0 {
                    arm.push(setl("acc", concat(getl("acc"), W::StrPtr(comma))));
                }
                arm.push(setl("acc", concat(getl("acc"), render)));
            }
            arm.push(N::Return(Some(concat(getl("acc"), W::StrPtr(close)))));
            b.push(N::If { cond: eqi(getl("t"), i32c(tag as i32)), then_: arm, els: vec![], result: None });
        }
        // For valid data the tag always matches a variant above; this tail is the
        // unreachable fallback that keeps the function stack-typed.
        let q = self.intern("?");
        b.push(N::Push(W::StrPtr(q)));
        Some((
            b,
            vec![
                WirLocal { name: "t".into(), ty: WirTy::Bool },
                WirLocal { name: "acc".into(), ty: WirTy::Bool },
            ],
        ))
    }
}
