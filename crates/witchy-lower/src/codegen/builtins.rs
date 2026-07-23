//! Builtin/function call lowering: the `lower_call` dispatch that maps each
//! builtin and stdlib call (crypto, encoding, list/dict/string, file, net, the
//! RFC-0032 vm.* interceptions, …) to its WIR. Split out of `codegen/mod.rs`
//! as the third slice of an incremental break-up of that file.

use super::*;

impl Codegen<'_> {
    pub(crate) fn lower_dynamic_try_decode(
        &mut self,
        call_expr: &Expr,
        args: &[Expr],
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};

        if !self.collect_wir {
            return None;
        }
        let [Expr::Var(dynamic_local), Expr::Int(expected_descriptor)] = args else {
            return None;
        };
        let result = self.ast_type_of_expr(call_expr)?;
        let expected = match result.unqualified() {
            Type::Named(option, arguments)
                if option.rsplit('.').next() == Some("Option") && arguments.len() == 1 =>
            {
                &arguments[0]
            }
            _ => return None,
        };
        let expected_kind = self.kind_for_type(expected);
        let Some(payload_id) = self
            .existential_payload_type_ids
            .get(&self.gc_lookup_type_key(expected))
            .copied()
        else {
            return Some(match expected_kind {
                kind @ (Kind::ExternRef | Kind::GcRef(_)) => {
                    W::RefNull(Self::wir_kind(kind))
                }
                Kind::I32 | Kind::I64 | Kind::F64 => {
                    self.mk_arities.insert(0);
                    W::Call { func: "mk0".into(), args: vec![W::ConstI32(1)] }
                }
            });
        };
        let dynamic_ty = Type::Named("dynamic.Dynamic".into(), Vec::new());
        let (dynamic_layout, dynamic_id) =
            self.gc_layout_for_ctor("Dynamic", Some(&dynamic_ty))?;
        if dynamic_layout.field_types.len() != 2 {
            return None;
        }
        let dynamic_value = || W::GetLocal(dynamic_local.clone());
        let descriptor = W::StructGet {
            struct_id: dynamic_id,
            field: dynamic_layout.field_base,
            base: Box::new(dynamic_value()),
        };
        let actual_descriptor = W::Load {
            ptr: Box::new(descriptor),
            kind: witchy_wir::wir::Kind::I64,
            offset: 4,
        };
        let envelope = W::StructGet {
            struct_id: dynamic_id,
            field: dynamic_layout.field_base + 1,
            base: Box::new(dynamic_value()),
        };
        let erased_payload = W::StructGet {
            struct_id: EXISTENTIAL_WRAPPER_ID,
            field: witchy_wir::wir::EXISTENTIAL_PAYLOAD_FIELD,
            base: Box::new(envelope),
        };
        let payload = W::StructGet {
            struct_id: payload_id,
            field: 0,
            base: Box::new(W::RefCast {
                struct_id: payload_id,
                value: Box::new(erased_payload),
            }),
        };
        let condition = W::Binary {
            op: witchy_wir::wir::BinOp::Eq,
            kind: witchy_wir::wir::Kind::I64,
            lhs: Box::new(actual_descriptor),
            rhs: Box::new(W::ConstI64(*expected_descriptor)),
        };
        let (success, failure, result_ty) = match expected_kind {
            kind @ (Kind::ExternRef | Kind::GcRef(_)) => (
                payload,
                W::RefNull(Self::wir_kind(kind)),
                Self::wir_ty_for_kind(kind),
            ),
            kind @ (Kind::I32 | Kind::I64 | Kind::F64) => {
                self.mk_arities.extend([0, 1]);
                (
                    W::Call {
                        func: "mk1".into(),
                        args: vec![
                            W::ConstI32(0),
                            W::ToSlot(Box::new(payload), Self::wir_kind(kind)),
                        ],
                    },
                    W::Call { func: "mk0".into(), args: vec![W::ConstI32(1)] },
                    witchy_wir::wir::WirTy::Bool,
                )
            }
        };
        Some(W::Control(Box::new(N::If {
            cond: condition,
            then_: vec![N::Push(success)],
            els: vec![N::Push(failure)],
            result: Some(result_ty),
        })))
    }

    /// Adapt a structural `(container, present, old-slot)` helper to a native
    /// `var` operation: write the repaired container back to its receiver local
    /// and yield `Option(old)` as the independent ordinary result. ADT layout
    /// remains in typed lowering; structural helpers only traffic in raw slots.
    fn lower_extract_var(
        &mut self,
        helper: &str,
        receiver: &Expr,
        mut args: Vec<witchy_wir::wir::WirExpr>,
        leaf_biases: &[i32],
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N, WirTy};
        let Expr::Var(root) = receiver else { return None };
        let container = var_scratch("result", 0, Kind::I32);
        let cap_scratch = var_scratch("cap", 0, Kind::I32);
        let tracked = self.inplace_push.contains(root);
        args.push(if tracked {
            W::GetLocal(format!("{root}__cap"))
        } else {
            W::ConstI32(0)
        });
        args.extend(leaf_biases.iter().copied().map(W::ConstI32));
        self.mk_arities.extend([0, 1]);
        let mut seq = vec![
            N::CallStoreMulti {
                func: helper.to_string(),
                args,
                dests: vec![
                    container.clone(),
                    TRY_TMP.to_string(),
                    MATCH_TMP.to_string(),
                    if tracked { format!("{root}__cap") } else { cap_scratch },
                ],
            },
            N::SetLocal { local: root.clone(), value: W::GetLocal(container) },
        ];
        seq.push(N::Push(W::Control(Box::new(N::If {
                cond: W::GetLocal(TRY_TMP.to_string()),
                then_: vec![N::Push(W::Call {
                    func: "mk1".into(),
                    args: vec![W::ConstI32(0), W::GetLocal(MATCH_TMP.to_string())],
                })],
                els: vec![N::Push(W::Call {
                    func: "mk0".into(),
                    args: vec![W::ConstI32(1)],
                })],
                result: Some(WirTy::Bool),
            }))));
        Some(W::Seq(seq))
    }

    pub(crate) fn lower_call(&mut self, name: &str, args: &[Expr]) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        use witchy_wir::wir::WirNode as N;
        use witchy_syntax::ast::is_render_intrinsic;
        use witchy_syntax::intrinsics;
        let name = witchy_syntax::cap_ops::surface_name(name);
        let name = intrinsics::canonical_operation_name(name);
        if let Some((callback_index, diagnostic)) =
            witchy_types::typeck::isolated_vm_callback_contract(name, args.len())
            && !self.is_top_level_fn_ref(&args[callback_index])
        {
            self.reject_reason.get_or_insert_with(|| CodegenError {
                message: diagnostic.to_string(),
            });
            return None;
        }
        let call = |func: &str, a: Vec<W>| W::Call { func: func.to_string(), args: a };
        // A direct host-import call (a `_host` import is the authority surface).
        let host = |import: &str, a: Vec<W>| W::CallHost { import: import.to_string(), args: a };
        let intrinsic_helper = |intrinsic: &str| {
            intrinsics::sole_wir_helper(intrinsic)
                .expect("cataloged builtin has one static WIR helper")
        };
        let intrinsic_helper_variant = |intrinsic: &str, helper: &str| {
            intrinsics::declared_wir_helper(intrinsic, helper)
                .expect("cataloged builtin declares this WIR helper")
        };
        // A void effect that yields Nil: `{inner} ... i32.const 0`.
        let nil0 = |inner: W| W::Seq(vec![N::Do(inner), N::Push(W::ConstI32(0))]);
        Some(match (name, args.len()) {
            (name, 1) if intrinsics::is_list_pop_extract(name) =>
            {
                if self
                    .ast_type_of_expr(&args[0])
                    .as_ref()
                    .and_then(|ty| self.gc_reference_list_layout(ty))
                    .is_some()
                {
                    return self.lower_gc_reference_list_pop(&args[0]);
                }
                let list = self.lower_expr(&args[0])?;
                let bias = self
                    .ast_type_of_expr(&args[0])
                    .as_ref()
                    .and_then(|ty| collection_leaf_bias(ty, "List", 0))?;
                self.lower_extract_var(intrinsic_helper(name), &args[0], vec![list], &[bias])?
            }
            // (RFC-0028) Confined slice view reads: lower to the zero-copy view
            // helpers, reading through the elided slice's source + bounds. Guarded
            // to active views (the `let` replaced the binding), so any other
            // `list.at`/`length` falls through to the materialized-list arms below.
            (intrinsics::LIST_AT, 2)
                if matches!(&args[0], Expr::Var(w) if self.view_active.contains(w)) =>
            {
                let ek = self.list_elem_kind(&args[0]);
                let Expr::Var(w) = &args[0] else { unreachable!() };
                let ik = self.kind_of(&args[1]);
                let inner = vec![
                    W::GetLocal(format!("{w}$src")),
                    W::GetLocal(format!("{w}$lo")),
                    W::GetLocal(format!("{w}$hi")),
                    // i64 index — `$list_at_view` checks in i64 (same i32-wrap fix
                    // as `$list_at`).
                    Self::wir_convert(self.lower_expr(&args[1])?, ik, Kind::I64),
                ];
                W::FromSlot(
                    Box::new(call(
                        intrinsic_helper_variant(intrinsics::LIST_AT, "list_at_view"),
                        inner,
                    )),
                    Self::wir_kind(ek),
                )
            }
            (intrinsics::LIST_LENGTH, 1)
                if self.collect_wir
                    && matches!(&args[0], Expr::Var(w) if self.view_active.contains(w)) =>
            {
                let Expr::Var(w) = &args[0] else { unreachable!() };
                Self::wir_convert(
                    call(
                        intrinsic_helper_variant(intrinsics::LIST_LENGTH, "list_len_view"),
                        vec![
                            W::GetLocal(format!("{w}$src")),
                            W::GetLocal(format!("{w}$lo")),
                            W::GetLocal(format!("{w}$hi")),
                        ],
                    ),
                    Kind::I32,
                    Kind::I64,
                )
            }
            (intrinsics::CRYPTO_ED25519_VERIFY_STATUS, 3) => {
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1], &args[2]])?)
            }
            (intrinsics::CRYPTO_SHA256, 1) => {
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            (intrinsics::CRYPTO_SIGN, 2) => {
                // The Secret bytes stay host-side; the guest passes an opaque
                // externref and the message.
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1]])?)
            }
            (intrinsics::CRYPTO_PUBLIC_KEY, 1) => {
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            (intrinsics::CRYPTO_REVEAL, 1) => {
                // The Secret bytes stay host-side until this explicit reveal path;
                // the guest passes only the opaque externref.
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            (intrinsics::CRYPTO_RUNE_HASH, 2) => {
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1]])?)
            }
            (intrinsics::CRYPTO_ECDSA_P256_VERIFY_STATUS, 3) => {
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1], &args[2]])?)
            }
            (intrinsics::CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS, 3) => {
                call(
                    intrinsic_helper(name),
                    self.lower_args(&[&args[0], &args[1], &args[2]])?,
                )
            }
            (intrinsics::CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS, 3) => {
                call(
                    intrinsic_helper(name),
                    self.lower_args(&[&args[0], &args[1], &args[2]])?,
                )
            }
            (intrinsics::CRYPTO_SHA512, 1) => {
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            (intrinsics::CRYPTO_SHA3_256, 1) => {
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            (intrinsics::CRYPTO_HMAC_SHA256, 2) => {
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1]])?)
            }
            (intrinsics::SECRETSTORE_REQUIRE, 2) => {
                // `SecretStore.require(name)` returns the `Secret` directly (no
                // `Option`): the host returns a nullable externref. An absent secret
                // must fail EAGERLY here — matching the interpreter, which errors at
                // the require site rather than deferring to a later (or never-reached)
                // use of a null Secret. The name is evaluated ONCE into a scratch slot
                // and reused for the lookup and the not-granted message. The store
                // argument carries no guest state — ignored.
                let secret_name = self.lower_expr(&args[1])?;
                let name_of = || W::GetLocal(SECRET_NAME_TMP.to_string());
                let lookup = call(intrinsic_helper(name), vec![name_of()]);
                let secret = || W::GetLocal(SECRET_TMP.to_string());
                let missing = W::RefIsNull(Box::new(secret()));
                let guard = N::If {
                    cond: missing,
                    then_: witchy_wir::wir_helpers::abort_nodes(
                        witchy_syntax::diag::DiagTemplate::SecretRequired,
                        W::ConstI64(0),
                        W::ConstI64(0),
                        name_of(),
                    ),
                    els: vec![],
                    result: None,
                };
                W::Seq(vec![
                    N::SetLocal { local: SECRET_NAME_TMP.to_string(), value: secret_name },
                    N::SetLocal { local: SECRET_TMP.to_string(), value: lookup },
                    guard,
                    N::Push(secret()),
                ])
            }
            (intrinsics::SECRETSTORE_GET, 2) => {
                // `Option(Secret)` is nullable externref: a granted named secret is
                // the `Some` payload, and `None` is `ref.null extern`.
                call(intrinsic_helper(name), vec![self.lower_expr(&args[1])?])
            }
            (intrinsics::COMPILER_FOOTPRINT, 1) => {
                self.uses_compiler_footprint = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            (intrinsics::COMPILER_DIFF, 2) => {
                self.uses_compiler_diff = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1]])?)
            }
            (intrinsics::COMPILER_DOC, 2) => {
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1]])?)
            }
            (intrinsics::COMPILER_DOC_RESULT_JSON, 2) => {
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1]])?)
            }
            (intrinsics::REGEX_MATCH_SPANS, 2) => {
                self.uses_regex_spans = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1]])?)
            }
            // Encoding transforms share one selector-based WIR helper. The catalog
            // owns both the helper and selector so codegen cannot drift from the
            // runtime host dispatch table.
            (name, actual_arity) if intrinsics::lookup(name).is_some_and(|spec| {
                spec.arity == actual_arity
                    && spec.arity == 1
                    && spec.wir_host_call.is_some_and(|host| host.helper == "encoding")
            }) => {
                let host = intrinsics::wir_host_call(name).expect("guarded encoding host call");
                self.uses_encoding = true;
                call(host.helper, vec![W::ConstI32(host.selector), self.lower_expr(&args[0])?])
            }
            // `string.from_code(cp)`: the Int code point travels in the i64 ABI.
            (intrinsics::STRING_FROM_CODE, 1) => {
                self.uses_string_from_code = true;
                let ak = self.kind_of(&args[0]);
                call(
                    intrinsic_helper(name),
                    vec![Self::wir_convert(self.lower_expr(&args[0])?, ak, Kind::I64)],
                )
            }
            // `list.length(xs)` / `string.length(s)` — the i32 count/byte-length
            // header, widened to the Int's i64. A count is non-negative so the
            // signed `Convert` matches an unsigned `i64.extend_i32_u`. Lowers only
            // in a WIR-collecting scope.
            (intrinsics::LIST_LENGTH, 1)
                if self.collect_wir
                    && self
                        .ast_type_of_expr(&args[0])
                        .as_ref()
                        .and_then(|ty| self.gc_reference_list_layout(ty))
                        .is_some() =>
            {
                Self::wir_convert(
                    W::ArrayLen(Box::new(self.lower_expr(&args[0])?)),
                    Kind::I32,
                    Kind::I64,
                )
            }
            (intrinsics::LIST_LENGTH, 1) | (intrinsics::STRING_LENGTH, 1)
                if self.collect_wir =>
            {
                let arg = self.lower_expr(&args[0])?;
                Self::wir_convert(
                    W::Load { ptr: Box::new(arg), kind: witchy_wir::wir::Kind::I32, offset: 0 },
                    Kind::I32,
                    Kind::I64,
                )
            }
            // `string.char_count(s)` — Unicode scalars in `s`, widened to the Int's
            // i64. The `$char_count` helper reads the byte-length header itself, so
            // `s` is evaluated once (binary path only; WAT keeps its legacy arm).
            (intrinsics::STRING_CHAR_COUNT, 1) if self.collect_wir => {
                self.uses_byte_to_char = true;
                let arg = self.lower_expr(&args[0])?;
                Self::wir_convert(
                    W::Call { func: intrinsic_helper(name).to_string(), args: vec![arg] },
                    Kind::I32,
                    Kind::I64,
                )
            }
            // Int <-> Float numeric conversions and `sqrt`, lowered only in a
            // WIR-collecting scope. `to_int` keeps saturating finite/inf behavior
            // but routes NaN through the shared runtime-abort diagnostic.
            (intrinsics::MATH_TO_FLOAT, 1) if self.collect_wir => {
                let ak = self.kind_of(&args[0]);
                let arg = Self::wir_convert(self.lower_expr(&args[0])?, ak, Kind::I64);
                W::Unary { op: witchy_wir::wir::UnOp::ToFloat, kind: witchy_wir::wir::Kind::F64, arg: Box::new(arg) }
            }
            (intrinsics::MATH_TO_INT, 1) if self.collect_wir => {
                let arg = self.lower_expr(&args[0])?;
                W::Call { func: intrinsic_helper(name).to_string(), args: vec![arg] }
            }
            (intrinsics::MATH_SQRT, 1) if self.collect_wir => {
                let arg = self.lower_expr(&args[0])?;
                W::Unary { op: witchy_wir::wir::UnOp::Sqrt, kind: witchy_wir::wir::Kind::F64, arg: Box::new(arg) }
            }
            // Render to a String for the scalar shapes: Str passes through,
            // Int → `$int_to_string`, Bool → an interned "true"/"false" value-if,
            // Bytes → `Bytes(len=N)`. Float and compound shapes route through their
            // dedicated helpers. Gated to a WIR-collecting scope (`collect_wir`).
            (render, 1) if self.collect_wir && is_render_intrinsic(render) => {
                match self.val_type_of(&args[0]) {
                    ValType::Str => return self.lower_expr(&args[0]),
                    ValType::Int => {
                        self.uses_int_to_string = true;
                        let ak = self.kind_of(&args[0]);
                        let arg = self.lower_expr(&args[0])?;
                        call("int_to_string", vec![Self::wir_convert(arg, ak, Kind::I64)])
                    }
                    ValType::Bool => {
                        let t = self.intern("true");
                        let f = self.intern("false");
                        let arg = self.lower_expr(&args[0])?;
                        W::Control(Box::new(witchy_wir::wir::WirNode::If {
                            cond: arg,
                            then_: vec![witchy_wir::wir::WirNode::Push(W::StrPtr(t))],
                            els: vec![witchy_wir::wir::WirNode::Push(W::StrPtr(f))],
                            result: Some(witchy_wir::wir::WirTy::Str),
                        }))
                    }
                    // A scalar Float renders via the `$float_to_str` host-import wrapper
                    // (the same helper the compound `$ts` renderer uses for Float fields,
                    // so it agrees with the oracle).
                    ValType::Float => {
                        self.uses_float_to_str = true;
                        let arg = self.lower_expr(&args[0])?;
                        call("float_to_str", vec![arg])
                    }
                    ValType::Bytes => {
                        self.uses_int_to_string = true;
                        let open = self.intern("Bytes(len=");
                        let close = self.intern(")");
                        let arg = self.lower_expr(&args[0])?;
                        let len = Self::wir_convert(
                            W::Load { ptr: Box::new(arg), kind: witchy_wir::wir::Kind::I32, offset: 0 },
                            Kind::I32,
                            Kind::I64,
                        );
                        let len_s = call("int_to_string", vec![len]);
                        call("concat", vec![call("concat", vec![W::StrPtr(open), len_s]), W::StrPtr(close)])
                    }
                    // Compound (tuple/list/...) rendering builds its string with the
                    // per-shape WIR `$ts` renderer — or bails (`?`) for shapes the
                    // renderer can't build, keeping WAT. `eq_operand_shape` (not just
                    // `eq_shape_of`) so an INLINE expression — e.g. `"${dict.keys(d)}"` —
                    // resolves its shape via typeck's type table, like a let-bound local.
                    _ => {
                        if let Some(shape) = self.eq_operand_shape(&args[0]) {
                            if shape.is_compound() {
                                if let Some(h) = self.ensure_ts_wir_helper(&shape) {
                                    let arg = self.lower_expr(&args[0])?;
                                    return Some(W::Call { func: h, args: vec![arg] });
                                }
                            }
                        }
                        // The structural renderer can't build this shape — most often
                        // a GENERIC RECORD such as `Set(a)` (its field types stay
                        // generic, so the compiled backend has no concrete layout to
                        // walk). Record WHY so the failure names the construct and the
                        // fix instead of the bare "interpreter-only feature?" message.
                        self.reject_reason.get_or_insert_with(|| CodegenError {
                            message: "cannot render this value with `\"${…}\"` on the \
                                      compiled backend — the structural renderer can't \
                                      build this shape. Render through the public \
                                      `Show` protocol instead, e.g. \
                                      `show.render(x)` or `show.say(console, x)`"
                                .into(),
                        });
                        return None;
                    }
                }
            },
            // String helpers over the `[len][bytes]` rep — pure `{args} call $h`.
            (intrinsics::STRING_TO_INT, 1) => {
                self.uses_str_to_int = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            (intrinsics::STRING_STARTS_WITH, 2) => {
                self.uses_starts_with = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1]])?)
            }
            (intrinsics::STRING_ENDS_WITH, 2) => {
                self.uses_ends_with = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1]])?)
            }
            (intrinsics::STRING_SPLIT, 2) => {
                self.uses_split = true;
                self.uses_substr = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1]])?)
            }
            (intrinsics::STRING_CHARS, 1) => {
                self.uses_str_chars = true;
                self.uses_byte_to_char = true;
                self.uses_substring = true;
                self.uses_substr = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            // `clock.now()`: the Clock arg is type-level; the host import is the
            // authority and takes no operands.
            ("now", 1) => {
                self.uses_now = true;
                if self.collect_wir {
                    call("now", vec![])
                } else {
                    W::CallHost { import: "now_host".to_string(), args: vec![] }
                }
            }
            // `clock.now_monotonic()`: monotonic elapsed nanoseconds. Like `now`, the
            // Clock arg is type-level and the host import takes no operands.
            ("now_monotonic", 1) => {
                if self.collect_wir {
                    call("now_monotonic", vec![])
                } else {
                    W::CallHost { import: "now_monotonic_host".to_string(), args: vec![] }
                }
            }
            // `rand.rand_u64()`: like `now`, the Rand arg is type-level; the host import
            // is the authority and takes no operands, returning a fresh i64 draw.
            ("rand_u64", 1) => {
                if self.collect_wir {
                    call("rand_u64", vec![])
                } else {
                    W::CallHost { import: "rand_u64_host".to_string(), args: vec![] }
                }
            }
            // `env.get_env(name)`: only the name travels (the Env grant is the host).
            // `fail(msg)`: a deliberate, loud abort. (RFC-0045) The message is no
            // longer dropped — it is handed to the always-linked, authority-free
            // `__witchy_abort` host import (the `Fail` template passes the string
            // through verbatim), which renders the full runtime diagnostic and
            // traps. Evaluate the message into a scratch first; site propagation
            // inserts the packed global write immediately before the host abort,
            // after any nested calls in the message have returned.
            ("fail", 1) => {
                let msg = self.lower_expr(&args[0])?;
                let mut nodes = vec![N::SetLocal { local: ABORT_STR_TMP.into(), value: msg }];
                nodes.extend(witchy_wir::wir_helpers::abort_nodes(
                    witchy_syntax::diag::DiagTemplate::Fail,
                    W::ConstI64(0),
                    W::ConstI64(0),
                    W::GetLocal(ABORT_STR_TMP.into()),
                ));
                nodes.push(witchy_wir::wir::WirNode::Push(W::ConstI32(0)));
                W::Seq(nodes)
            }
            ("get_env", 2) => {
                self.uses_get_env = true;
                call("get_env", self.lower_args(&[&args[1]])?)
            }
            // `console.print(msg)`: the Console arg is type-level; print the msg
            // (a void host helper), then yield Nil as `i32.const 0`.
            ("print", 2) => {
                W::Seq(vec![
                    witchy_wir::wir::WirNode::Do(W::Call {
                        func: "print_str".to_string(),
                        args: self.lower_args(&[&args[1]])?,
                    }),
                    witchy_wir::wir::WirNode::Push(W::ConstI32(0)),
                ])
            }
            // Duration <-> Int(ms) is a runtime no-op (both i64) — value-neutral.
            ("int_to_duration", 1) | ("duration_to_int", 1) => return self.lower_expr(&args[0]),
            // `contains(s, sub)` == `find_byte(s, sub) != -1`.
            (intrinsics::STRING_CONTAINS, 2) => {
                self.uses_find_byte = true;
                let inner = self.lower_args(&[&args[0], &args[1]])?;
                W::Binary {
                    op: witchy_wir::wir::BinOp::Ne,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(W::Call { func: intrinsic_helper(name).to_string(), args: inner }),
                    rhs: Box::new(W::ConstI32(-1)),
                }
            }
            // `index_of(s, sub)` -> Int: the i32 index, sign-extended to i64.
            (intrinsics::STRING_FIND, 2) => {
                self.uses_find_byte = true;
                self.uses_index_of = true;
                let inner = self.lower_args(&[&args[0], &args[1]])?;
                W::ToSlot(
                    Box::new(W::Call { func: intrinsic_helper(name).to_string(), args: inner }),
                    witchy_wir::wir::Kind::I32,
                )
            }
            // --- guest-helper calls: `{args} call $helper` ---
            (intrinsics::STRING_REPLACE, 3) => {
                self.uses_replace = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1], &args[2]])?)
            }
            (intrinsics::STRING_TRIM, 1) => {
                self.uses_trim = true;
                self.uses_substr = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            (intrinsics::LIST_CONCAT, 2) => {
                if self
                    .ast_type_of_expr(&args[0])
                    .as_ref()
                    .and_then(|ty| self.gc_reference_list_layout(ty))
                    .is_some()
                {
                    self.lower_gc_function_list_concat(&args[0], &args[1])?
                } else {
                    call(intrinsic_helper(name), self.lower_args(&[&args[0], &args[1]])?)
                }
            }
            (intrinsics::DICT_NEW, 0) => {
                self.uses_dict = true;
                call(intrinsic_helper(name), vec![])
            }
            (intrinsics::DICT_KEYS, 1) => {
                self.uses_dict_iter = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            (intrinsics::DICT_VALUES, 1) => {
                self.uses_dict_iter = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            (intrinsics::DICT_PAIRS, 1) => {
                self.uses_dict_iter = true;
                call(intrinsic_helper(name), self.lower_args(&[&args[0]])?)
            }
            ("read", 2) => {
                self.used_dir_ops.insert("read");
                call("dir_read", self.lower_args(&[&args[0], &args[1]])?)
            }
            // RFC-0012 File ops. `read(File)` is arity 1 (a leaf, no path) and goes
            // through the `file_read` WIR helper; `write(File, data)` is arity 2.
            // `open`/`create` navigate a Dir to a confined File externref.
            ("read", 1) => call("file_read", self.lower_args(&[&args[0]])?),
            ("write", 2) => {
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("file_write", a) } else { nil0(host("file_write_host", a)) }
            }
            // RFC-0012 `dir.read_file`/`dir.write_file` navigate a Dir to a confined
            // File externref (the internal host ops keep their `dir_open`/`dir_create` names).
            ("read_file", 2) => {
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("dir_open", a) } else { host("dir_open_host", a) }
            }
            ("write_file", 2) => {
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("dir_create", a) } else { host("dir_create_host", a) }
            }
            // `exec(cap, dir, path, args, stdin) -> String`. The `Exec` cap (arg 0)
            // is a structural placeholder — `caps.exec` gates linking — so it is
            // dropped; the WIR `exec` helper takes (Dir externref, path, args, stdin).
            ("exec", 5) => {
                call("exec", self.lower_args(&[&args[1], &args[2], &args[3], &args[4]])?)
            }
            ("list", 1) => {
                self.used_dir_ops.insert("list");
                call("dir_list", self.lower_args(&[&args[0]])?)
            }
            // (RFC-0032) Intercept `vm.par_map(xs, f)` (monomorphized to
            // `vm.par_map__T__T`) over SCALAR element types, when `f` is a TOP-LEVEL
            // function reference: the host runs the map across OS-thread worker VMs and
            // lays out the result list, invoking `f` via the `__call_idx` export by its
            // table index (NULL environment). The two restrictions keep it sound:
            //  - scalar elements marshal as flat i64s (a pointer element's data lives in
            //    the parent VM's memory, unreachable from a worker's separate memory);
            //  - a top-level `f` is capture-free, so the NULL-env call reads no captured
            //    parent-heap state.
            // Anything else (non-scalar elements, a lambda, a captured closure local)
            // falls through (`_ => None`) to the sequential `list.map` body in
            // std/vm.witchy, which is always correct.
            (_, 2)
                if Self::is_scalar_par_map(name)
                    && self.is_top_level_fn_ref(&args[1]) =>
            {
                call(
                    "vm_par_map",
                    vec![self.lower_expr(&args[0])?, self.lower_closure_code(&args[1])?],
                )
            }
            // (RFC-0032) `String`/`Bytes` variant — flat buffer payloads copied raw across
            // worker VMs (one path; a `String` is valid-UTF-8 `Bytes`).
            (_, 2)
                if Self::is_buf_par_map(name)
                    && self.is_top_level_fn_ref(&args[1]) =>
            {
                call(
                    "vm_par_map_bytes",
                    vec![self.lower_expr(&args[0])?, self.lower_closure_code(&args[1])?],
                )
            }
            // (RFC-0032) Capability-passing: run a top-level `f(Dir, Bytes) -> Bytes` in an
            // isolated worker VM granted exactly `dir`. `f` must be a top-level (capture-free)
            // function, like the par_map variants.
            ("vm.with_dir", 3) => {
                call(
                    "vm_with_dir",
                    vec![
                        self.lower_expr(&args[0])?,
                        self.lower_closure_code(&args[1])?,
                        self.lower_expr(&args[2])?,
                    ],
                )
            }
            // (RFC-0032) `vm.serve(init, requests, handler)` — a stateful service on a
            // long-lived isolated worker VM (the parity-safe cross-VM channel). `handler`
            // must be a top-level (capture-free) function.
            ("vm.serve", 3) => {
                call(
                    "vm_serve",
                    vec![
                        self.lower_expr(&args[0])?,
                        self.lower_expr(&args[1])?,
                        self.lower_closure_code(&args[2])?,
                    ],
                )
            }
            ("read_build", 2) => {
                self.used_build_ops.insert("read_build");
                // Build* caps are zero-representation at the host boundary:
                // typeck requires the receiver, import gating grants authority.
                call("build_read", self.lower_args(&[&args[1]])?)
            }
            ("get_build_env", 2) => {
                self.used_build_ops.insert("get_build_env");
                call("build_get_env", self.lower_args(&[&args[1]])?)
            }
            ("fetch_build", 3) => {
                self.used_build_ops.insert("fetch_build");
                call("build_fetch", self.lower_args(&[&args[1], &args[2]])?)
            }
            ("run_tool", 3) => {
                self.used_build_ops.insert("run_tool");
                call("build_exec", self.lower_args(&[&args[1], &args[2]])?)
            }
            ("recv_line", 1) => {
                self.used_net_ops.insert("recv_line");
                call("net_recv_line", self.lower_args(&[&args[0]])?)
            }
            ("recv_all", 1) => {
                self.used_net_ops.insert("recv_all");
                call("net_recv_all", self.lower_args(&[&args[0]])?)
            }
            // The `Dir` ops: in a WIR-collecting scope, route through a registered
            // host-wrapper helper (so the user body stays free of direct CallHosts
            // and the import is accounted for via `import_deps` — capability-minimal).
            // In a non-collecting scope (e.g. a raw-prelude helper body) emit the
            // inline `$dir_*_host` CallHost, which provides `$dir_*_host` directly
            // rather than the helper.
            // `dir.subtree(path)` narrows a `Dir` to a subtree; lowered to the
            // `dir_subdir` host op (the internal name is historical).
            ("subtree", 2) => {
                self.used_dir_ops.insert("subdir");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("dir_subdir", a) } else { host("dir_subdir_host", a) }
            }
            ("exists", 2) => {
                self.used_dir_ops.insert("exists");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("dir_exists", a) } else { host("dir_exists_host", a) }
            }
            ("is_dir", 2) => {
                self.used_dir_ops.insert("is_dir");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("dir_is_dir", a) } else { host("dir_is_dir_host", a) }
            }
            ("accept", 1) => {
                self.used_net_ops.insert("accept");
                let a = self.lower_args(&[&args[0]])?;
                if self.collect_wir { call("net_accept", a) } else { host("net_accept_host", a) }
            }
            ("fetch", 2) => {
                self.used_net_ops.insert("connect");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("net_fetch", a) } else { host("net_fetch_host", a) }
            }
            // RFC-0011 typed verbs: `only`/`deny` take a policy record; extract its
            // single `pattern` field and feed the host op the same string. `only` is
            // polymorphic on the receiver — a `Dir` narrows its ENTRY policy
            // (`dir_only`), a `Net` narrows its ADDRESS set (`net_restrict`, the host op
            // name is historical — the user-facing verb is `only`).
            ("only", 2) => {
                if matches!(self.type_table.type_of(&args[0]), Some(witchy_types::typeck::Ty::Dir(_))) {
                    self.used_dir_ops.insert("only");
                    let pattern = Expr::Field { base: Box::new(args[1].clone()), field: "pattern".into() };
                    let a = self.lower_args(&[&args[0], &pattern])?;
                    if self.collect_wir { call("dir_only", a) } else { host("dir_only_host", a) }
                } else if matches!(
                    self.type_table.type_of(&args[0]),
                    Some(witchy_types::typeck::Ty::Fetch)
                ) {
                    let a = self.lower_args(&[&args[0], &args[1]])?;
                    if self.collect_wir { call("fetch_only", a) } else { host("fetch_only_host", a) }
                } else {
                    self.used_net_ops.insert("restrict");
                    let pattern = Expr::Field { base: Box::new(args[1].clone()), field: "pattern".into() };
                    let a = self.lower_args(&[&args[0], &pattern])?;
                    if self.collect_wir { call("net_restrict", a) } else { host("net_restrict_host", a) }
                }
            }
            ("send_raw", 5) => {
                let a = self.lower_args(&[&args[0], &args[1], &args[2], &args[3], &args[4]])?;
                if self.collect_wir { call("fetch_send", a) } else { host("fetch_send_host", a) }
            }
            ("deny", 2) => {
                self.used_net_ops.insert("deny");
                let pattern = Expr::Field { base: Box::new(args[1].clone()), field: "pattern".into() };
                let a = self.lower_args(&[&args[0], &pattern])?;
                if self.collect_wir { call("net_deny", a) } else { host("net_deny_host", a) }
            }
            ("connect", 2) => {
                self.used_net_ops.insert("connect");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("net_connect", a) } else { host("net_connect_host", a) }
            }
            // Fallible dial. After RFC-0005's Socket migration, the host returns a
            // nullable externref: non-null is `Some(Socket)`, null is `None`.
            ("try_connect", 2) => {
                self.used_net_ops.insert("try_connect");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir {
                    call("net_try_connect", a)
                } else {
                    host("net_try_connect_host", a)
                }
            }
            // (RFC-0020) `net.net.resolve(host) -> List(String)` — resolved IP literals,
            // via the staged list helper (identical shape to `list`/`dir_list`).
            ("resolve", 2) => {
                self.used_net_ops.insert("resolve");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                call("net_resolve", a)
            }
            // (RFC-0020) `net.connect_pinned(net, ip, host, port, secure) -> Socket` — dial
            // the exact `ip:port` with `host` presented as SNI/Host. Same shape as `connect`.
            ("connect_pinned", 5) => {
                self.used_net_ops.insert("connect");
                let a = self.lower_args(&[&args[0], &args[1], &args[2], &args[3], &args[4]])?;
                if self.collect_wir { call("net_connect_pinned", a) } else { host("net_connect_pinned_host", a) }
            }
            // Fallible pinned dial — nullable-externref `Option(Socket)`, mirroring
            // `try_connect`.
            ("try_connect_pinned", 5) => {
                self.used_net_ops.insert("try_connect");
                let a = self.lower_args(&[&args[0], &args[1], &args[2], &args[3], &args[4]])?;
                if self.collect_wir {
                    call("net_try_connect_pinned", a)
                } else {
                    host("net_try_connect_pinned_host", a)
                }
            }
            ("listen", 2) => {
                self.used_net_ops.insert("listen");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("net_listen", a) } else { host("net_listen_host", a) }
            }
            // (RFC-0060) HTTPS listen: `(net, addr, cert_pem, key) -> Listener`.
            // The `key` argument is an opaque Secret externref; the key bytes never
            // enter guest memory.
            ("listen_tls", 4) => {
                self.used_net_ops.insert("listen");
                let a = self.lower_args(&[&args[0], &args[1], &args[2], &args[3]])?;
                if self.collect_wir { call("net_listen_tls", a) } else { host("net_listen_tls_host", a) }
            }
            // --- void effects yielding Nil: `{args} call $h ... i32.const 0` ---
            ("send_line", 2) => {
                self.used_net_ops.insert("send_line");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("net_send_line", a) } else { nil0(host("net_send_line_host", a)) }
            }
            ("send_bytes", 2) => {
                self.used_net_ops.insert("send_bytes");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("net_send_bytes", a) } else { nil0(host("net_send_bytes_host", a)) }
            }
            ("close", 1) => {
                self.used_net_ops.insert("close");
                let a = self.lower_args(&[&args[0]])?;
                if self.collect_wir { call("net_close", a) } else { nil0(host("net_close_host", a)) }
            }
            // (RFC-0032) `server.serve`'s worker pool: spawn one worker VM per core, all
            // accepting from the shared listener.
            ("serve_pool", 1) => {
                self.used_net_ops.insert("serve_pool");
                let a = self.lower_args(&[&args[0]])?;
                if self.collect_wir { call("serve_pool", a) } else { nil0(host("serve_pool_host", a)) }
            }
            ("write", 3) => {
                self.used_dir_ops.insert("write");
                let a = self.lower_args(&[&args[0], &args[1], &args[2]])?;
                if self.collect_wir { call("dir_write", a) } else { nil0(host("dir_write_host", a)) }
            }
            ("append", 3) => {
                self.used_dir_ops.insert("append");
                let a = self.lower_args(&[&args[0], &args[1], &args[2]])?;
                if self.collect_wir { call("dir_append", a) } else { nil0(host("dir_append_host", a)) }
            }
            ("make_dir", 2) => {
                self.used_dir_ops.insert("make_dir");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("dir_make_dir", a) } else { nil0(host("dir_make_dir_host", a)) }
            }
            ("write_out", 3) => {
                self.used_build_ops.insert("write_out");
                // BuildOut is checked at source level and granted by import
                // linking; the host op needs only the generated path + bytes.
                let a = self.lower_args(&[&args[1], &args[2]])?;
                if self.collect_wir { call("build_out_write", a) } else { nil0(host("build_out_write_host", a)) }
            }
            (intrinsics::TESTING_MOCK_DIR, 1) => {
                call("testing_mock_dir", self.lower_args(&[&args[0]])?)
            }
            // --- calls with a pushed constant / slot conversions ---
            (intrinsics::STRING_TO_UPPER, 1) | (intrinsics::STRING_TO_LOWER, 1) => {
                self.uses_ascii_case = true;
                let up = if name == intrinsics::STRING_TO_UPPER { 1 } else { 0 };
                call(intrinsic_helper(name), vec![self.lower_expr(&args[0])?, W::ConstI32(up)])
            }
            (intrinsics::STRING_SUBSTRING, 3) => {
                self.uses_substring = true;
                self.uses_substr = true;
                let sk = self.kind_of(&args[1]);
                let ek = self.kind_of(&args[2]);
                // (BUG-011) Pass the char indices at full i64 width — `$str_substring`
                // clamps them to `[0, char_count]` before narrowing to byte offsets,
                // exactly like the interpreter. A prior narrow-to-i32 here wrapped huge
                // indices (near the i64 extremes), diverging from the interpreter.
                call(intrinsic_helper(name), vec![
                    self.lower_expr(&args[0])?,
                    Self::wir_convert(self.lower_expr(&args[1])?, sk, Kind::I64),
                    Self::wir_convert(self.lower_expr(&args[2])?, ek, Kind::I64),
                ])
            }
            // (Bytes) `Bytes` shares `String`'s flat `[len][bytes]` layout, so
            // `from_string` is identity — every witchy `String` is already valid
            // UTF-8, so its bytes are the buffer verbatim.
            (intrinsics::BYTES_FROM_STRING, 1) => self.lower_expr(&args[0])?,
            (intrinsics::BYTES_FROM_LIST, 1) => {
                call("bytes_from_list", vec![self.lower_expr(&args[0])?])
            }
            // (parity, SEC-042) `to_string` is NOT identity: `Bytes` has no UTF-8
            // contract, so invalid sequences must be lossily normalized to U+FFFD to
            // match the interpreter's `String::from_utf8_lossy`. Route through the
            // byte-exact `$bytes_to_string` helper (an identity return diverged on
            // bad bytes).
            (intrinsics::BYTES_TO_STRING, 1) => {
                self.uses_encoding = true;
                call("bytes_to_string", vec![self.lower_expr(&args[0])?])
            }
            // (RFC-0055) Channel message erasure. A message already rides the
            // universal slot on the compiled backend (every buffer element, record
            // field, and closure argument is an untyped 8-byte slot), so erasing to
            // `__Msg` and recovering the endpoint's type are both the identity — the
            // value passes through unchanged, exactly as the executor's former
            // generic `m` did.
            (intrinsics::ERASE, 1) | (intrinsics::UNERASE, 1) => self.lower_expr(&args[0])?,
            (intrinsics::BYTES_LENGTH, 1) => {
                let arg = self.lower_expr(&args[0])?;
                Self::wir_convert(
                    W::Load { ptr: Box::new(arg), kind: witchy_wir::wir::Kind::I32, offset: 0 },
                    Kind::I32,
                    Kind::I64,
                )
            }
            (intrinsics::BYTES_AT, 2) => {
                // Bounds-checked byte read via the `$bytes_at` helper: trap on
                // `i < 0 || i >= len`, matching the interpreter's "bytes index out
                // of bounds" error. (An unchecked `load8_u` here used to silently
                // read adjacent heap — a parity/OOB bug, SEC-038.)
                let b = self.lower_expr(&args[0])?;
                let ik = self.kind_of(&args[1]);
                // i64 index — checked in i64 by `$bytes_at` (matches the
                // interpreter's `i as usize`; closes the same i32-wrap hole as list.at).
                let i = Self::wir_convert(self.lower_expr(&args[1])?, ik, Kind::I64);
                call("bytes_at", vec![b, i])
            }
            (intrinsics::BYTES_CONCAT, 2) => {
                call("concat", vec![self.lower_expr(&args[0])?, self.lower_expr(&args[1])?])
            }
            (intrinsics::BYTES_SLICE, 3) => {
                // (parity) `Bytes` is BYTE-indexed with no UTF-8 contract, so this
                // must route through the byte-indexed `$bytes_slice` — NOT the
                // char-indexed `$str_substring`, which mangled multibyte payloads
                // (the backends diverged: interpreter byte-indexed, compiled
                // char-indexed). `$bytes_slice` clamps exactly like the interpreter.
                // The bounds are i64 and clamped in i64 (BUG-392): narrowing to i32
                // first would wrap a large positive bound negative, so
                // `slice(b, 0, 2^31)` compiled to empty while the interpreter
                // returned the full buffer — closing the same hole as `__bytes_at`.
                self.uses_substr = true;
                let sk = self.kind_of(&args[1]);
                let ek = self.kind_of(&args[2]);
                call("bytes_slice", vec![
                    self.lower_expr(&args[0])?,
                    Self::wir_convert(self.lower_expr(&args[1])?, sk, Kind::I64),
                    Self::wir_convert(self.lower_expr(&args[2])?, ek, Kind::I64),
                ])
            }
            (intrinsics::LIST_PUSH | intrinsics::GENERATED_LIST_PUSH, 2) => {
                if self
                    .ast_type_of_expr(&args[0])
                    .as_ref()
                    .and_then(|ty| self.gc_reference_list_layout(ty))
                    .is_some()
                {
                    self.lower_gc_function_list_push(&args[0], &args[1])?
                } else {
                    let xk = self.kind_of(&args[1]);
                    call(
                        intrinsic_helper_variant(name, "list_push"),
                        vec![
                            self.lower_expr(&args[0])?,
                            W::ToSlot(
                                Box::new(self.lower_expr(&args[1])?),
                                Self::wir_kind(xk),
                            ),
                        ],
                    )
                }
            }
            (intrinsics::LIST_SET_AT, 3) => {
                if self
                    .ast_type_of_expr(&args[0])
                    .as_ref()
                    .and_then(|ty| self.gc_reference_list_layout(ty))
                    .is_some()
                {
                    self.lower_gc_function_list_set_at(&args[0], &args[1], &args[2])?
                } else {
                    let level = self.assign_level;
                    if level >= SCRUT_POOL {
                        return None;
                    }
                    let ik = self.kind_of(&args[1]);
                    let vk = self.kind_of(&args[2]);
                    self.assign_level = level + 1;
                    let lowered = (|| {
                        Some((
                            self.lower_expr(&args[0])?,
                            Self::wir_convert(self.lower_expr(&args[1])?, ik, Kind::I64),
                            self.lower_expr(&args[2])?,
                        ))
                    })();
                    self.assign_level = level;
                    let (list, index, value) = lowered?;
                    let list_tmp = assign_scratch("list", level);
                    let index_tmp = assign_scratch("index", level);
                    let value_tmp = assign_scratch("value", level);
                    W::Seq(vec![
                        // Assignment order is destination base, destination
                        // coordinate, RHS, then the checked store. Stage all three
                        // so no source expression is lowered or evaluated twice.
                        N::SetLocal { local: list_tmp.clone(), value: list },
                        N::SetLocal { local: index_tmp.clone(), value: index },
                        N::SetLocal {
                            local: value_tmp.clone(),
                            value: W::ToSlot(Box::new(value), Self::wir_kind(vk)),
                        },
                        N::Drop(W::Call {
                            func: intrinsic_helper_variant(intrinsics::LIST_SET_AT, "list_at")
                                .into(),
                            args: vec![
                                W::GetLocal(list_tmp.clone()),
                                W::GetLocal(index_tmp.clone()),
                            ],
                        }),
                        N::CallStoreMulti {
                            func: intrinsic_helper_variant(
                                intrinsics::LIST_SET_AT,
                                "list_set_cap",
                            )
                            .into(),
                            args: vec![
                                W::GetLocal(list_tmp),
                                Self::wir_convert(
                                    W::GetLocal(index_tmp),
                                    Kind::I64,
                                    Kind::I32,
                                ),
                                W::GetLocal(value_tmp),
                                W::ConstI32(0),
                            ],
                            dests: vec![TUPLE_TMP.to_string(), "__witchy_owncap".to_string()],
                        },
                        N::Push(W::GetLocal(TUPLE_TMP.to_string())),
                    ])
                }
            }
            (intrinsics::LIST_AT, 2) => {
                if self
                    .ast_type_of_expr(&args[0])
                    .as_ref()
                    .and_then(|ty| self.gc_reference_list_layout(ty))
                    .is_some()
                {
                    self.lower_gc_function_list_at(&args[0], &args[1])?
                } else {
                    let ek = self.list_elem_kind(&args[0]);
                    let ik = self.kind_of(&args[1]);
                // (RFC-0034 L2) Bounds-check elision: when the For lowering proved this
                // exact `list.at(xs, i)` is in range (a registered `(i, xs)` pair), emit
                // the unchecked element load — `load_i64( (xs + 4) + i*8 )`, the same
                // address `$list_at` computes, minus the `i < 0 || i >= len` trap guard.
                // Both args are lowered once either way, so string-offset interning is
                // identical to the checked path.
                    let elide = matches!((&args[0], &args[1]), (Expr::Var(lv), Expr::Var(iv))
                    if self.elide_index_list.iter().any(|(i, l)| i == iv && l == lv));
                    let list_w = self.lower_expr(&args[0])?;
                // Lower the index ONCE (it may be a side-effecting call), then widen
                // to the kind the chosen path needs. The elide path does i32 address
                // math directly; the checked `$list_at` now takes the index as i64
                // (so an out-of-i32-range index traps + reports its true value,
                // matching the interpreter's `i as usize` — RFC-0045 message parity
                // and a latent i32-wrap hole this closes).
                    let idx_target = if elide { Kind::I32 } else { Kind::I64 };
                    let idx_w = Self::wir_convert(self.lower_expr(&args[1])?, ik, idx_target);
                    let read = if elide {
                    let wi32 = witchy_wir::wir::Kind::I32;
                    let add = witchy_wir::wir::BinOp::Add;
                    let addr = W::Binary {
                        op: add,
                        kind: wi32,
                        lhs: Box::new(W::Binary {
                            op: add,
                            kind: wi32,
                            lhs: Box::new(list_w),
                            rhs: Box::new(W::ConstI32(4)),
                        }),
                        rhs: Box::new(W::Binary {
                            op: witchy_wir::wir::BinOp::Mul,
                            kind: wi32,
                            lhs: Box::new(idx_w),
                            rhs: Box::new(W::ConstI32(8)),
                        }),
                    };
                    W::FromSlot(
                        Box::new(W::Load {
                            ptr: Box::new(addr),
                            kind: witchy_wir::wir::Kind::I64,
                            offset: 0,
                        }),
                        Self::wir_kind(ek),
                    )
                } else {
                    W::FromSlot(
                        Box::new(call(
                            intrinsic_helper_variant(intrinsics::LIST_AT, "list_at"),
                            vec![list_w, idx_w],
                        )),
                        Self::wir_kind(ek),
                    )
                    };
                // (RFC-0035 step 1) The element read out of the container is now an OWNED
                // reference sharing the object with the slot, so `$rc_dup` it — it returns the
                // pointer, wrapping the read in place. Gated `rc-floor`; only i32-kinded
                // (heap-pointer) elements whose type is a PROVABLY offset-0 `$rc_alloc` value
                // (`list_elem_is_offset0_rc` excludes Dict / scalar / bare type-var — the plain
                // `[ptr-8]` refcount is correct only there). dup-at-read alone only INCREMENTS,
                // so it cannot free live data; a consumer transfers or drops it in later steps.
                    if Self::wir_kind(ek) == witchy_wir::wir::Kind::I32
                    && self.list_elem_is_offset0_rc(&args[0])
                    && !force_copy_mode()
                    && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::RcFloor)
                    {
                        W::Call { func: "rc_dup".into(), args: vec![read] }
                    } else {
                        read
                    }
                }
            }
            ("recv_bytes", 2) => {
                self.used_net_ops.insert("recv_bytes");
                let nk = self.kind_of(&args[1]);
                call("net_recv_bytes", vec![
                    self.lower_expr(&args[0])?,
                    Self::wir_convert(self.lower_expr(&args[1])?, nk, Kind::I64),
                ])
            }
            // `dict.length(d)` -> Int: the i32 count at the header, sign-extended.
            (intrinsics::DICT_LENGTH, 1) => W::ToSlot(
                Box::new(W::Load {
                    ptr: Box::new(self.lower_expr(&args[0])?),
                    kind: witchy_wir::wir::Kind::I32,
                    offset: 0,
                }),
                witchy_wir::wir::Kind::I32,
            ),
            // --- dict family: a key-mode i32 side-operand + slot conversions ---
            (intrinsics::DICT_INSERT, 3) => {
                self.uses_dict = true;
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                let vk = self.kind_of(&args[2]);
                call(
                    intrinsic_helper_variant(name, "dict_insert"),
                    vec![
                        self.lower_expr(&args[0])?,
                        W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                        W::ToSlot(Box::new(self.lower_expr(&args[2])?), Self::wir_kind(vk)),
                        W::ConstI32(mode as i32),
                    ],
                )
            }
            (name, 3) if intrinsics::is_dict_insert_extract(name) =>
            {
                self.uses_dict = true;
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                let vk = self.kind_of(&args[2]);
                let structural_args = vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                    W::ToSlot(Box::new(self.lower_expr(&args[2])?), Self::wir_kind(vk)),
                    W::ConstI32(mode as i32),
                ];
                let dict_ty = self.ast_type_of_expr(&args[0])?;
                let key_bias = collection_leaf_bias(&dict_ty, "Dict", 0)?;
                let value_bias = collection_leaf_bias(&dict_ty, "Dict", 1)?;
                self.lower_extract_var(
                    intrinsic_helper(name),
                    &args[0],
                    structural_args,
                    &[key_bias, value_bias],
                )?
            }
            (intrinsics::DICT_GET_OR, 3) => {
                self.uses_dict = true;
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                let dk = self.kind_of(&args[2]);
                let inner = vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                    W::ToSlot(Box::new(self.lower_expr(&args[2])?), Self::wir_kind(dk)),
                    W::ConstI32(mode as i32),
                ];
                W::FromSlot(Box::new(call(intrinsic_helper(name), inner)), Self::wir_kind(dk))
            }
            (intrinsics::DICT_AT, 2) => {
                self.uses_dict = true;
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                let vk = self
                    .dict_value_valtype_of(&args[0])
                    .map(valtype_kind)
                    .map(Self::wir_kind)
                    .unwrap_or(witchy_wir::wir::Kind::I32);
                let inner = vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                    W::ConstI32(mode as i32),
                ];
                W::FromSlot(Box::new(call(intrinsic_helper(name), inner)), vk)
            }
            (intrinsics::DICT_CONTAINS_KEY, 2) => {
                self.uses_dict = true;
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                call(intrinsic_helper(name), vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                    W::ConstI32(mode as i32),
                ])
            }
            (intrinsics::DICT_REMOVE, 2) => {
                self.uses_dict = true;
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                call(intrinsic_helper(name), vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                    W::ConstI32(mode as i32),
                ])
            }
            (name, 2) if intrinsics::is_dict_remove_extract(name) =>
            {
                self.uses_dict = true;
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                let structural_args = vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                    W::ConstI32(mode as i32),
                ];
                let dict_ty = self.ast_type_of_expr(&args[0])?;
                let key_bias = collection_leaf_bias(&dict_ty, "Dict", 0)?;
                let value_bias = collection_leaf_bias(&dict_ty, "Dict", 1)?;
                self.lower_extract_var(
                    intrinsic_helper(name),
                    &args[0],
                    structural_args,
                    &[key_bias, value_bias],
                )?
            }
            (intrinsics::DICT_UPDATE, 4) => {
                self.uses_dict = true;
                self.uses_dict_update = true;
                self.clos_arities.insert(1);
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                let dk = self.kind_of(&args[2]);
                call(
                    intrinsic_helper_variant(name, "dict_update"),
                    vec![
                        self.lower_expr(&args[0])?,
                        W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                        W::ToSlot(Box::new(self.lower_expr(&args[2])?), Self::wir_kind(dk)),
                        W::ConstI32(mode as i32),
                        self.lower_expr(&args[3])?,
                    ],
                )
            }
            _ => return None,
        })
    }
}
