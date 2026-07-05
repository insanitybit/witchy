//! Builtin/function call lowering: the `lower_call` dispatch that maps each
//! builtin and stdlib call (crypto, encoding, list/dict/string, file, net, the
//! RFC-0032 vm.* interceptions, …) to its WIR. Split out of `codegen/mod.rs`
//! as the third slice of an incremental break-up of that file.

use super::*;

impl Codegen {
    pub(crate) fn lower_call(&mut self, name: &str, args: &[Expr]) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        use witchy_wir::wir::WirNode as N;
        let call = |func: &str, a: Vec<W>| W::Call { func: func.to_string(), args: a };
        // A direct host-import call (a `_host` import is the authority surface).
        let host = |import: &str, a: Vec<W>| W::CallHost { import: import.to_string(), args: a };
        // A void effect that yields Nil: `{inner} ... i32.const 0`.
        let nil0 = |inner: W| W::Seq(vec![N::Do(inner), N::Push(W::ConstI32(0))]);
        Some(match (name, args.len()) {
            // (RFC-0028) Confined slice view reads: lower to the zero-copy view
            // helpers, reading through the elided slice's source + bounds. Guarded
            // to active views (the `let` replaced the binding), so any other
            // `list.at`/`length` falls through to the materialized-list arms below.
            ("list.at", 2)
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
                W::FromSlot(Box::new(call("list_at_view", inner)), Self::wir_kind(ek))
            }
            ("list.length", 1)
                if self.collect_wir
                    && matches!(&args[0], Expr::Var(w) if self.view_active.contains(w)) =>
            {
                let Expr::Var(w) = &args[0] else { unreachable!() };
                Self::wir_convert(
                    call("list_len_view", vec![
                        W::GetLocal(format!("{w}$src")),
                        W::GetLocal(format!("{w}$lo")),
                        W::GetLocal(format!("{w}$hi")),
                    ]),
                    Kind::I32,
                    Kind::I64,
                )
            }
            ("crypto.ed25519_verify", 3) => {
                self.uses_crypto_ed25519_verify = true;
                call("crypto_ed25519_verify", self.lower_args(&[&args[0], &args[1], &args[2]])?)
            }
            ("crypto.sha256", 1) => {
                self.uses_crypto_sha256 = true;
                call("crypto_sha256", self.lower_args(&[&args[0]])?)
            }
            ("crypto.sign", 2) => {
                // The Secret bytes stay host-side; the guest passes the key HANDLE
                // (an i32 index into the host secret table) and the message.
                self.uses_crypto_sign = true;
                call("crypto_sign", self.lower_args(&[&args[0], &args[1]])?)
            }
            ("crypto.public_key", 1) => {
                self.uses_crypto_public_key = true;
                call("crypto_public_key", self.lower_args(&[&args[0]])?)
            }
            ("crypto.reveal", 1) => {
                // The Secret bytes stay host-side; the guest passes the key HANDLE
                // and the host stages the revealed bytes as a fresh String.
                call("crypto_reveal", self.lower_args(&[&args[0]])?)
            }
            ("crypto.rune_hash", 2) => {
                self.uses_crypto_rune_hash = true;
                call("crypto_rune_hash", self.lower_args(&[&args[0], &args[1]])?)
            }
            ("crypto.ecdsa_p256_verify", 3) => {
                self.used_crypto_ops.insert("ecdsa_p256_verify");
                call("crypto_ecdsa_p256_verify", self.lower_args(&[&args[0], &args[1], &args[2]])?)
            }
            ("crypto.rsa_pkcs1_sha256_verify", 3) => {
                self.used_crypto_ops.insert("rsa_pkcs1_sha256_verify");
                call(
                    "crypto_rsa_pkcs1_sha256_verify",
                    self.lower_args(&[&args[0], &args[1], &args[2]])?,
                )
            }
            ("crypto.ecdsa_p256_verify_hex", 3) => {
                self.used_crypto_ops.insert("ecdsa_p256_verify_hex");
                call(
                    "crypto_ecdsa_p256_verify_hex",
                    self.lower_args(&[&args[0], &args[1], &args[2]])?,
                )
            }
            ("crypto.sha512", 1) => {
                self.used_crypto_ops.insert("sha512");
                call("crypto_sha512", self.lower_args(&[&args[0]])?)
            }
            ("crypto.sha3_256", 1) => {
                self.used_crypto_ops.insert("sha3_256");
                call("crypto_sha3_256", self.lower_args(&[&args[0]])?)
            }
            ("crypto.hmac_sha256", 2) => {
                self.used_crypto_ops.insert("hmac_sha256");
                call("crypto_hmac_sha256", self.lower_args(&[&args[0], &args[1]])?)
            }
            ("secretstore.require", 2) => {
                // `SecretStore.require(name)` returns the `Secret` directly (no
                // `Option`): the host-table handle IS the Secret's guest value. An
                // absent secret yields -1, which the crypto host ops reject loudly.
                // The store argument (handle 0) carries no guest state — ignored.
                call("secretstore_lookup", vec![self.lower_expr(&args[1])?])
            }
            ("secretstore.get", 2) => {
                // `SecretStore.get(name)` builds `Option(Secret)` on the guest:
                //   let h = secretstore_lookup(name)
                //   if h >= 0 { Some(h) } else { None }
                // The handle is the host secret-table index (an i32) — which IS the
                // `Secret`'s guest representation, so `Some(h)` is `Some(Secret)`. The
                // handle is fetched ONCE into a scratch local and reused, so the name
                // string is allocated once. The store argument (handle 0) carries no
                // guest state, so it is ignored.
                let lookup = call("secretstore_lookup", vec![self.lower_expr(&args[1])?]);
                let handle = || W::GetLocal(SECRET_TMP.to_string());
                let cond = W::Binary {
                    op: witchy_wir::wir::BinOp::Ge,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(handle()),
                    rhs: Box::new(W::ConstI32(0)),
                };
                self.mk_arities.insert(1);
                self.mk_arities.insert(0);
                let some = W::Call {
                    func: "mk1".into(),
                    args: vec![W::ConstI32(0), W::ToSlot(Box::new(handle()), witchy_wir::wir::Kind::I32)],
                };
                let none = W::Call { func: "mk0".into(), args: vec![W::ConstI32(1)] };
                let choose = W::Control(Box::new(N::If {
                    cond,
                    then_: vec![N::Push(some)],
                    els: vec![N::Push(none)],
                    result: Some(witchy_wir::wir::WirTy::Str),
                }));
                W::Seq(vec![
                    N::SetLocal { local: SECRET_TMP.to_string(), value: lookup },
                    N::Push(choose),
                ])
            }
            ("compiler.footprint", 1) => {
                self.uses_compiler_footprint = true;
                call("compiler_footprint", self.lower_args(&[&args[0]])?)
            }
            ("compiler.diff", 2) => {
                self.uses_compiler_diff = true;
                call("compiler_diff", self.lower_args(&[&args[0], &args[1]])?)
            }
            ("compiler.doc", 2) => call("compiler_doc", self.lower_args(&[&args[0], &args[1]])?),
            ("regex.match_spans", 2) => {
                self.uses_regex_spans = true;
                call("regex_match_spans", self.lower_args(&[&args[0], &args[1]])?)
            }
            // The `encoding` transforms share one `$encoding` helper, selected by an
            // i32 op pushed *before* the argument. The `*_lossy` decoders are the raw
            // byte-level primitives; the public `encoding.*decode` wrappers (pure
            // witchy in `std/encoding.witchy`) validate the alphabet and return
            // `Result`, so they lower as ordinary function calls, not intercepts.
            ("encoding.hex_encode", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(0), self.lower_expr(&args[0])?])
            }
            ("encoding.hex_decode_lossy", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(1), self.lower_expr(&args[0])?])
            }
            ("encoding.base64_encode", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(2), self.lower_expr(&args[0])?])
            }
            ("encoding.base64_decode_lossy", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(3), self.lower_expr(&args[0])?])
            }
            ("encoding.hex_to_base64url", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(4), self.lower_expr(&args[0])?])
            }
            ("encoding.base64url_decode_lossy", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(5), self.lower_expr(&args[0])?])
            }
            ("encoding.base64url_to_hex_lossy", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(6), self.lower_expr(&args[0])?])
            }
            // `string.from_code(cp)`: the Int code point travels in the i64 ABI.
            ("string.from_code", 1) => {
                self.uses_string_from_code = true;
                let ak = self.kind_of(&args[0]);
                call(
                    "string_from_code",
                    vec![Self::wir_convert(self.lower_expr(&args[0])?, ak, Kind::I64)],
                )
            }
            // `list.length(xs)` / `string.length(s)` — the i32 count/byte-length
            // header, widened to the Int's i64. A count is non-negative so the
            // signed `Convert` matches an unsigned `i64.extend_i32_u`. Lowers only
            // in a WIR-collecting scope.
            ("list.length", 1) | ("string.length", 1) if self.collect_wir => {
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
            ("string.char_count", 1) if self.collect_wir => {
                self.uses_byte_to_char = true;
                let arg = self.lower_expr(&args[0])?;
                Self::wir_convert(
                    W::Call { func: "char_count".to_string(), args: vec![arg] },
                    Kind::I32,
                    Kind::I64,
                )
            }
            // Int <-> Float numeric conversions and `sqrt`, lowered only in a
            // WIR-collecting scope to `f64.convert_i64_s` / `i64.trunc_sat_f64_s` /
            // `f64.sqrt`. `to_int` is SATURATING to match the interpreter's `as i64`
            // (NaN -> 0, ±inf clamp), not the trapping trunc.
            ("math.to_float", 1) if self.collect_wir => {
                let ak = self.kind_of(&args[0]);
                let arg = Self::wir_convert(self.lower_expr(&args[0])?, ak, Kind::I64);
                W::Unary { op: witchy_wir::wir::UnOp::ToFloat, kind: witchy_wir::wir::Kind::F64, arg: Box::new(arg) }
            }
            ("math.to_int", 1) if self.collect_wir => {
                let arg = self.lower_expr(&args[0])?;
                W::Unary { op: witchy_wir::wir::UnOp::ToInt, kind: witchy_wir::wir::Kind::I64, arg: Box::new(arg) }
            }
            ("math.sqrt", 1) if self.collect_wir => {
                let arg = self.lower_expr(&args[0])?;
                W::Unary { op: witchy_wir::wir::UnOp::Sqrt, kind: witchy_wir::wir::Kind::F64, arg: Box::new(arg) }
            }
            // `__render` to a String for the scalar shapes: Str passes through,
            // Int → `$int_to_string`, Bool → an interned "true"/"false" value-if.
            // Float and compound shapes bail (handled by their dedicated render
            // helpers). Gated to a WIR-collecting scope (`collect_wir`).
            ("__render", 1) if self.collect_wir => match self.val_type_of(&args[0]) {
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
                // Compound (tuple/list/...) `__render` builds its string with the
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
                                  build this shape (typically a generic record such \
                                  as `Set`); call the type's own renderer instead, \
                                  e.g. `set.show(s)`"
                            .into(),
                    });
                    return None;
                }
            },
            // String helpers over the `[len][bytes]` rep — pure `{args} call $h`.
            ("string.to_int", 1) => {
                self.uses_str_to_int = true;
                call("str_to_int", self.lower_args(&[&args[0]])?)
            }
            ("string.starts_with", 2) => {
                self.uses_starts_with = true;
                call("starts_with", self.lower_args(&[&args[0], &args[1]])?)
            }
            ("string.ends_with", 2) => {
                self.uses_ends_with = true;
                call("ends_with", self.lower_args(&[&args[0], &args[1]])?)
            }
            ("string.split", 2) => {
                self.uses_split = true;
                self.uses_substr = true;
                call("split", self.lower_args(&[&args[0], &args[1]])?)
            }
            ("string.chars", 1) => {
                self.uses_str_chars = true;
                self.uses_byte_to_char = true;
                self.uses_substring = true;
                self.uses_substr = true;
                call("str_chars", self.lower_args(&[&args[0]])?)
            }
            // `now(clock)`: the Clock arg is type-level; the host import is the
            // authority and takes no operands.
            ("now", 1) => {
                self.uses_now = true;
                if self.collect_wir {
                    call("now", vec![])
                } else {
                    W::CallHost { import: "now_host".to_string(), args: vec![] }
                }
            }
            // `now_monotonic(clock)`: monotonic elapsed nanoseconds. Like `now`, the
            // Clock arg is type-level and the host import takes no operands.
            ("now_monotonic", 1) => {
                if self.collect_wir {
                    call("now_monotonic", vec![])
                } else {
                    W::CallHost { import: "now_monotonic_host".to_string(), args: vec![] }
                }
            }
            // `rand_u64(rand)`: like `now`, the Rand arg is type-level; the host import
            // is the authority and takes no operands, returning a fresh i64 draw.
            ("rand_u64", 1) => {
                if self.collect_wir {
                    call("rand_u64", vec![])
                } else {
                    W::CallHost { import: "rand_u64_host".to_string(), args: vec![] }
                }
            }
            // `get_env(env, name)`: only the name travels (the Env grant is the host).
            // `fail(msg)`: a deliberate, loud abort. (RFC-0045) The message is no
            // longer dropped — it is handed to the always-linked, authority-free
            // `__witchy_abort` host import (the `Fail` template passes the string
            // through verbatim), which renders `runtime error: <msg>` and traps.
            // The `unreachable` after keeps the Seq stack-typed (the call never
            // returns); the trailing `i32.const 0` is dead code satisfying the type.
            ("fail", 1) => {
                let msg = self.lower_expr(&args[0])?;
                let mut nodes = witchy_wir::wir_helpers::abort_nodes(
                    witchy_syntax::diag::DiagTemplate::Fail,
                    W::ConstI64(0),
                    W::ConstI64(0),
                    msg,
                );
                nodes.push(witchy_wir::wir::WirNode::Push(W::ConstI32(0)));
                W::Seq(nodes)
            }
            ("get_env", 2) => {
                self.uses_get_env = true;
                call("get_env", self.lower_args(&[&args[1]])?)
            }
            // `print(console, msg)`: the Console arg is type-level; print the msg
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
            ("string.contains", 2) => {
                self.uses_find_byte = true;
                let inner = self.lower_args(&[&args[0], &args[1]])?;
                W::Binary {
                    op: witchy_wir::wir::BinOp::Ne,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(W::Call { func: "find_byte".to_string(), args: inner }),
                    rhs: Box::new(W::ConstI32(-1)),
                }
            }
            // `index_of(s, sub)` -> Int: the i32 index, sign-extended to i64.
            ("string.find", 2) => {
                self.uses_find_byte = true;
                self.uses_index_of = true;
                let inner = self.lower_args(&[&args[0], &args[1]])?;
                W::ToSlot(
                    Box::new(W::Call { func: "str_index_of".to_string(), args: inner }),
                    witchy_wir::wir::Kind::I32,
                )
            }
            // --- guest-helper calls: `{args} call $helper` ---
            ("string.replace", 3) => {
                self.uses_replace = true;
                call("replace", self.lower_args(&[&args[0], &args[1], &args[2]])?)
            }
            ("string.trim", 1) => {
                self.uses_trim = true;
                self.uses_substr = true;
                call("trim", self.lower_args(&[&args[0]])?)
            }
            ("list.concat", 2) => {
                call("list_concat", self.lower_args(&[&args[0], &args[1]])?)
            }
            ("dict.new", 0) => {
                self.uses_dict = true;
                call("dict_new", vec![])
            }
            ("dict.keys", 1) => {
                self.uses_dict_iter = true;
                call("dict_keys", self.lower_args(&[&args[0]])?)
            }
            ("dict.values", 1) => {
                self.uses_dict_iter = true;
                call("dict_values", self.lower_args(&[&args[0]])?)
            }
            ("dict.pairs", 1) => {
                self.uses_dict_iter = true;
                call("dict_pairs", self.lower_args(&[&args[0]])?)
            }
            ("read", 2) => {
                self.used_dir_ops.insert("read");
                call("dir_read", self.lower_args(&[&args[0], &args[1]])?)
            }
            // RFC-0012 File ops. `read(File)` is arity 1 (a leaf, no path) and goes
            // through the `file_read` WIR helper; `write(File, data)` is arity 2.
            // `open`/`create` navigate a Dir to a confined File handle.
            ("read", 1) => call("file_read", self.lower_args(&[&args[0]])?),
            ("write", 2) => {
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("file_write", a) } else { nil0(host("file_write_host", a)) }
            }
            // RFC-0012 `dir.read_file`/`dir.write_file` navigate a Dir to a confined
            // File handle (the internal host ops keep their `dir_open`/`dir_create` names).
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
            // dropped; the WIR `exec` helper takes (dir handle, path, args, stdin).
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
                    && matches!(&args[1], Expr::Var(f) if !self.locals.contains_key(f)) =>
            {
                call("vm_par_map", self.lower_args(&[&args[0], &args[1]])?)
            }
            // (RFC-0032) `String`/`Bytes` variant — flat buffer payloads copied raw across
            // worker VMs (one path; a `String` is valid-UTF-8 `Bytes`).
            (_, 2)
                if Self::is_buf_par_map(name)
                    && matches!(&args[1], Expr::Var(f) if !self.locals.contains_key(f)) =>
            {
                call("vm_par_map_bytes", self.lower_args(&[&args[0], &args[1]])?)
            }
            // (RFC-0032) Capability-passing: run a top-level `f(Dir, Bytes) -> Bytes` in an
            // isolated worker VM granted exactly `dir`. `f` must be a top-level (capture-free)
            // function, like the par_map variants.
            ("vm.with_dir", 3)
                if matches!(&args[1], Expr::Var(f) if !self.locals.contains_key(f)) =>
            {
                call("vm_with_dir", self.lower_args(&[&args[0], &args[1], &args[2]])?)
            }
            // (RFC-0032) `vm.serve(init, requests, handler)` — a stateful service on a
            // long-lived isolated worker VM (the parity-safe cross-VM channel). `handler`
            // must be a top-level (capture-free) function.
            ("vm.serve", 3)
                if matches!(&args[2], Expr::Var(f) if !self.locals.contains_key(f)) =>
            {
                call("vm_serve", self.lower_args(&[&args[0], &args[1], &args[2]])?)
            }
            ("read_build", 2) => {
                self.used_build_ops.insert("read_build");
                call("build_read", self.lower_args(&[&args[0], &args[1]])?)
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
            // RFC-0011 typed verbs: `only`/`deny` take a policy record; extract its
            // single `pattern` field and feed the host op the same string. `only` is
            // polymorphic on the receiver — a `Dir` narrows its ENTRY policy
            // (`dir_only`), a `Net` narrows its ADDRESS set (`net_restrict`, the host op
            // name is historical — the user-facing verb is `only`).
            ("only", 2) => {
                let pattern = Expr::Field { base: Box::new(args[1].clone()), field: "pattern".into() };
                if matches!(self.type_table.type_of(&args[0]), Some(witchy_types::typeck::Ty::Dir(_))) {
                    self.used_dir_ops.insert("only");
                    let a = self.lower_args(&[&args[0], &pattern])?;
                    if self.collect_wir { call("dir_only", a) } else { host("dir_only_host", a) }
                } else {
                    self.used_net_ops.insert("restrict");
                    let a = self.lower_args(&[&args[0], &pattern])?;
                    if self.collect_wir { call("net_restrict", a) } else { host("net_restrict_host", a) }
                }
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
            // Fallible dial. The host `net_try_connect` returns the socket handle
            // or the `-1` sentinel; wrap it as `Option(Socket)`: handle the dial
            // ONCE into a scratch local, then `h >= 0 ? Some(h) : None`. Mirrors
            // the `secretstore.lookup` Option construction (Some=tag-0/None=tag-1).
            ("try_connect", 2) => {
                self.used_net_ops.insert("try_connect");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                let dial = if self.collect_wir {
                    call("net_try_connect", a)
                } else {
                    host("net_try_connect_host", a)
                };
                let handle = || W::GetLocal(SECRET_TMP.to_string());
                let cond = W::Binary {
                    op: witchy_wir::wir::BinOp::Ge,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(handle()),
                    rhs: Box::new(W::ConstI32(0)),
                };
                self.mk_arities.insert(1);
                self.mk_arities.insert(0);
                let some = W::Call {
                    func: "mk1".into(),
                    args: vec![W::ConstI32(0), W::ToSlot(Box::new(handle()), witchy_wir::wir::Kind::I32)],
                };
                let none = W::Call { func: "mk0".into(), args: vec![W::ConstI32(1)] };
                let choose = W::Control(Box::new(N::If {
                    cond,
                    then_: vec![N::Push(some)],
                    els: vec![N::Push(none)],
                    result: Some(witchy_wir::wir::WirTy::Str),
                }));
                W::Seq(vec![
                    N::SetLocal { local: SECRET_TMP.to_string(), value: dial },
                    N::Push(choose),
                ])
            }
            // (RFC-0020) `net.resolve(net, host) -> List(String)` — resolved IP literals,
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
            // Fallible pinned dial — `Option(Socket)`, mirroring `try_connect`'s handle-or-`-1`
            // sentinel wrapping (Some=tag-0/None=tag-1).
            ("try_connect_pinned", 5) => {
                self.used_net_ops.insert("try_connect");
                let a = self.lower_args(&[&args[0], &args[1], &args[2], &args[3], &args[4]])?;
                let dial = if self.collect_wir {
                    call("net_try_connect_pinned", a)
                } else {
                    host("net_try_connect_pinned_host", a)
                };
                let handle = || W::GetLocal(SECRET_TMP.to_string());
                let cond = W::Binary {
                    op: witchy_wir::wir::BinOp::Ge,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(handle()),
                    rhs: Box::new(W::ConstI32(0)),
                };
                self.mk_arities.insert(1);
                self.mk_arities.insert(0);
                let some = W::Call {
                    func: "mk1".into(),
                    args: vec![W::ConstI32(0), W::ToSlot(Box::new(handle()), witchy_wir::wir::Kind::I32)],
                };
                let none = W::Call { func: "mk0".into(), args: vec![W::ConstI32(1)] };
                let choose = W::Control(Box::new(N::If {
                    cond,
                    then_: vec![N::Push(some)],
                    els: vec![N::Push(none)],
                    result: Some(witchy_wir::wir::WirTy::Str),
                }));
                W::Seq(vec![
                    N::SetLocal { local: SECRET_TMP.to_string(), value: dial },
                    N::Push(choose),
                ])
            }
            ("listen", 2) => {
                self.used_net_ops.insert("listen");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("net_listen", a) } else { host("net_listen_host", a) }
            }
            // (RFC-0060) HTTPS listen: `(net, addr, cert_pem, key) -> Listener`. The
            // `key` argument is a Secret, whose guest value IS its host-table handle,
            // so it lowers like any i32 — the key bytes never enter guest memory.
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
                let a = self.lower_args(&[&args[0], &args[1], &args[2]])?;
                if self.collect_wir { call("build_out_write", a) } else { nil0(host("build_out_write_host", a)) }
            }
            // --- calls with a pushed constant / slot conversions ---
            ("string.to_upper", 1) | ("string.to_lower", 1) => {
                self.uses_ascii_case = true;
                let up = if name == "string.to_upper" { 1 } else { 0 };
                call("ascii_case", vec![self.lower_expr(&args[0])?, W::ConstI32(up)])
            }
            ("string.substring", 3) => {
                self.uses_substring = true;
                self.uses_substr = true;
                let sk = self.kind_of(&args[1]);
                let ek = self.kind_of(&args[2]);
                // (BUG-011) Pass the char indices at full i64 width — `$str_substring`
                // clamps them to `[0, char_count]` before narrowing to byte offsets,
                // exactly like the interpreter. A prior narrow-to-i32 here wrapped huge
                // indices (near the i64 extremes), diverging from the interpreter.
                call("str_substring", vec![
                    self.lower_expr(&args[0])?,
                    Self::wir_convert(self.lower_expr(&args[1])?, sk, Kind::I64),
                    Self::wir_convert(self.lower_expr(&args[2])?, ek, Kind::I64),
                ])
            }
            // (Bytes) `Bytes` shares `String`'s flat `[len][bytes]` layout, so
            // `from_string` is identity — every witchy `String` is already valid
            // UTF-8, so its bytes are the buffer verbatim.
            ("__bytes_from_string", 1) => self.lower_expr(&args[0])?,
            // (parity, SEC-042) `to_string` is NOT identity: `Bytes` has no UTF-8
            // contract, so invalid sequences must be lossily normalized to U+FFFD to
            // match the interpreter's `String::from_utf8_lossy`. Route through the
            // byte-exact `$bytes_to_string` helper (an identity return diverged on
            // bad bytes).
            ("__bytes_to_string", 1) => {
                self.uses_encoding = true;
                call("bytes_to_string", vec![self.lower_expr(&args[0])?])
            }
            // (RFC-0055) Channel message erasure. A message already rides the
            // universal slot on the compiled backend (every buffer element, record
            // field, and closure argument is an untyped 8-byte slot), so erasing to
            // `__Msg` and recovering the endpoint's type are both the identity — the
            // value passes through unchanged, exactly as the executor's former
            // generic `m` did.
            ("__erase", 1) | ("__unerase", 1) => self.lower_expr(&args[0])?,
            ("__bytes_length", 1) => {
                let arg = self.lower_expr(&args[0])?;
                Self::wir_convert(
                    W::Load { ptr: Box::new(arg), kind: witchy_wir::wir::Kind::I32, offset: 0 },
                    Kind::I32,
                    Kind::I64,
                )
            }
            ("__bytes_at", 2) => {
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
            ("__bytes_concat", 2) => {
                call("concat", vec![self.lower_expr(&args[0])?, self.lower_expr(&args[1])?])
            }
            ("__bytes_slice", 3) => {
                // (parity) `Bytes` is BYTE-indexed with no UTF-8 contract, so this
                // must route through the byte-indexed `$bytes_slice` — NOT the
                // char-indexed `$str_substring`, which mangled multibyte payloads
                // (the backends diverged: interpreter byte-indexed, compiled
                // char-indexed). `$bytes_slice` clamps exactly like the interpreter.
                self.uses_substr = true;
                let sk = self.kind_of(&args[1]);
                let ek = self.kind_of(&args[2]);
                call("bytes_slice", vec![
                    self.lower_expr(&args[0])?,
                    Self::wir_convert(self.lower_expr(&args[1])?, sk, Kind::I32),
                    Self::wir_convert(self.lower_expr(&args[2])?, ek, Kind::I32),
                ])
            }
            ("list.push", 2) => {
                let xk = self.kind_of(&args[1]);
                call("list_push", vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(xk)),
                ])
            }
            ("list.at", 2) => {
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
                    W::FromSlot(Box::new(call("list_at", vec![list_w, idx_w])), Self::wir_kind(ek))
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
            ("recv_bytes", 2) => {
                self.used_net_ops.insert("recv_bytes");
                let nk = self.kind_of(&args[1]);
                call("net_recv_bytes", vec![
                    self.lower_expr(&args[0])?,
                    Self::wir_convert(self.lower_expr(&args[1])?, nk, Kind::I64),
                ])
            }
            // `dict.length(d)` -> Int: the i32 count at the header, sign-extended.
            ("dict.length", 1) => W::ToSlot(
                Box::new(W::Load {
                    ptr: Box::new(self.lower_expr(&args[0])?),
                    kind: witchy_wir::wir::Kind::I32,
                    offset: 0,
                }),
                witchy_wir::wir::Kind::I32,
            ),
            // --- dict family: a key-mode i32 side-operand + slot conversions ---
            ("dict.insert", 3) => {
                self.uses_dict = true;
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                let vk = self.kind_of(&args[2]);
                call("dict_insert", vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                    W::ToSlot(Box::new(self.lower_expr(&args[2])?), Self::wir_kind(vk)),
                    W::ConstI32(mode as i32),
                ])
            }
            ("dict.get_or", 3) => {
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
                W::FromSlot(Box::new(call("dict_get_or", inner)), Self::wir_kind(dk))
            }
            ("dict.contains_key", 2) => {
                self.uses_dict = true;
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                call("dict_has", vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                    W::ConstI32(mode as i32),
                ])
            }
            ("dict.remove", 2) => {
                self.uses_dict = true;
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                call("dict_remove", vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                    W::ConstI32(mode as i32),
                ])
            }
            ("dict.update", 4) => {
                self.uses_dict = true;
                self.uses_dict_update = true;
                self.clos_arities.insert(1);
                let mode = self.dict_key_mode_wir(&args[1])?;
                let kk = self.kind_of(&args[1]);
                let dk = self.kind_of(&args[2]);
                call("dict_update", vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(kk)),
                    W::ToSlot(Box::new(self.lower_expr(&args[2])?), Self::wir_kind(dk)),
                    W::ConstI32(mode as i32),
                    self.lower_expr(&args[3])?,
                ])
            }
            _ => return None,
        })
    }
}
