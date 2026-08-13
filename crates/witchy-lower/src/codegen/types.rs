//! Type/kind-inference for codegen.
//!
//! A cohesive group of `Codegen` methods that recover the WASM `Kind`, the
//! finer source-level `ValType`, and the record/field/payload types of an
//! expression — to the extent codegen can determine them locally (falling back
//! to typeck's table). Split out of `codegen/mod.rs` as the first slice of an
//! incremental break-up of that file.

use super::{name_kind, promote_kind, valtype_kind, ty_to_valtype};
use super::{Codegen, Kind, ValType};
use witchy_syntax::ast::{BinOp, Block, Expr, Pattern, Stmt, Type, UnOp};
use witchy_syntax::{cap_ops, intrinsics};

impl Codegen<'_> {
    /// The WASM kind a compiled expression evaluates to.
    pub(crate) fn kind_of(&self, e: &Expr) -> Kind {
        if let Some(t) = self.ast_type_of_expr(e) {
            if let k @ (Kind::ExternRef | Kind::GcRef(_)) = self.kind_for_type(&t) {
                return k;
            }
        }
        match e {
            Expr::Int(_) | Expr::Duration(_) => Kind::I64,
            Expr::Float(_) => Kind::F64,
            Expr::Var(n) => self.locals.get(n).copied().unwrap_or(Kind::I32),
            // Compiler-owned packs are inserted after type annotation, so the
            // wrapper node itself has no address-keyed table entry. Its declared
            // runtime representation is nevertheless fixed and never scalar.
            Expr::ExistentialPack { .. } => Kind::GcRef(super::EXISTENTIAL_WRAPPER_ID),
            Expr::ExistentialUpcast { .. } => Kind::GcRef(super::EXISTENTIAL_WRAPPER_ID),
            // Dispatch has already selected its static slot result. The witness
            // chooses only an ABI-identical adapter, so result representation is
            // never discovered from the concrete payload at codegen time.
            Expr::ExistentialCall { result, .. } => self.kind_for_type(result),
            Expr::Unary { op, expr } => match op {
                // `!x` is a bool (i32); negation/complement keep the operand kind.
                UnOp::Not => Kind::I32,
                UnOp::Neg | UnOp::BitNot | UnOp::Move | UnOp::Await | UnOp::Borrow | UnOp::BorrowMut | UnOp::Deref => self.kind_of(expr),
            },
            Expr::Binary { op, lhs, rhs } => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::BitAnd
                | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                    // The common (promoted) kind of the two operands.
                    let (lk, rk) = (self.kind_of(lhs), self.kind_of(rhs));
                    if lk == Kind::F64 || rk == Kind::F64 {
                        Kind::F64
                    } else if lk == Kind::I64 || rk == Kind::I64 {
                        Kind::I64
                    } else {
                        Kind::I32
                    }
                }
                // `a ?? b` carries its payload kind (an Int payload is i64) —
                // matching the `??` emission's branch kind.
                BinOp::Coalesce => self
                    .match_payload_valtype(lhs)
                    .map(valtype_kind)
                    .unwrap_or_else(|| self.kind_of(rhs)),
                // concat (ptr) and comparisons / and / or (bool) are i32.
                _ => Kind::I32,
            },
            Expr::Field { base, field } => {
                if field.parse::<usize>().is_ok() {
                    return valtype_kind(self.val_type_of(e));
                }
                if let Some(bt) = self.record_type_of(base) {
                    if let Some(struct_id) = self.gc_aggregate_ids.get(&bt).copied() {
                        if let Some(fields) = self.record_field_types.get(&bt) {
                            if let Some(names) = self.record_fields.get(&bt) {
                                if let Some(idx) = names.iter().position(|(n, _)| n == field) {
                                    return fields
                                        .get(idx)
                                        .map(|ty| self.kind_for_type(ty))
                                        .unwrap_or(Kind::GcRef(struct_id));
                                }
                            }
                        }
                    }
                    if let Some(fields) = self.record_fields.get(&bt) {
                        if let Some((_, ft)) = fields.iter().find(|(n, _)| n == field) {
                            return name_kind(ft.as_deref());
                        }
                    }
                }
                Kind::I32
            }
            Expr::If {
                then_block,
                else_block,
                ..
            } => {
                let tk = self.block_kind(then_block);
                let ek = else_block.as_ref().map(|b| self.block_kind(b)).unwrap_or(Kind::I32);
                promote_kind(tk, ek)
            }
            Expr::Block(b) => self.block_kind(b),
            Expr::Match { arms, .. } => arms
                .split_first()
                .map(|(first, rest)| {
                    rest.iter()
                        .fold(self.kind_of(&first.body), |acc, a| promote_kind(acc, self.kind_of(&a.body)))
                })
                .unwrap_or(Kind::I32),
            // `get_or(d, k, default)` returns the dict's value at the default's
            // kind (the i64 value slot is recovered to it at the call site).
            Expr::Call { name, args }
                if cap_ops::surface_name(name) == intrinsics::DICT_GET_OR && args.len() == 3 =>
            {
                self.kind_of(&args[2])
            }
            // `at(d, k)` returns the dict's value slot at the value's recovered
            // kind, so `Int` dictionary reads stay i64 instead of the generic i32.
            Expr::Call { name, args }
                if cap_ops::surface_name(name) == intrinsics::DICT_AT && args.len() == 2 =>
            {
                self.dict_value_valtype_of(&args[0]).map(valtype_kind).unwrap_or(Kind::I32)
            }
            // (RFC-0055) `__erase`/`__unerase` are the identity on both value and
            // kind: the message passes through unchanged, so the ctor/slot that
            // stores it uses the ARGUMENT's real kind (a `send`'d `Int` stays i64
            // in its 8-byte slot). The executor then reads the opaque `__Msg` field
            // at the universal i32 width — the same truncation the former generic
            // `m` field took, so both backends stay byte-identical.
            Expr::Call { name, args }
                if intrinsics::is_erasure_bridge(cap_ops::surface_name(name))
                    && args.len() == 1 =>
            {
                self.kind_of(&args[0])
            }
            // RFC-0012/RFC-0005 Stage 2: Dir navigation yields an unforgeable File
            // externref, not an integer handle.
            Expr::Call { name, args }
                if matches!(cap_ops::surface_name(name), "read_file" | "write_file")
                    && args.len() == 2 =>
            {
                Kind::ExternRef
            }
            Expr::Call { name, .. } => match cap_ops::surface_name(name) {
                // (BUG-609) A BINDING IN SCOPE SHADOWS AN INTRINSIC. A closure-typed
                // parameter or local named after a bare intrinsic (`read`, `now`, …)
                // must be typed from its own function type, not from the intrinsic
                // catalog — otherwise the call pushes the intrinsic's result kind and
                // the module fails wasm validation ("expected externref, found i32").
                // This arm has to precede every `intrinsics::lookup` arm below, since
                // name resolution and the interpreter already prefer the local.
                other if self.local_fn_ret_kind.contains_key(other) => {
                    self.local_fn_ret_kind[other]
                }
                name if intrinsics::lookup(name)
                    .is_some_and(|spec| spec.signature.returns_float()) => Kind::F64,
                name if intrinsics::lookup(name)
                    .is_some_and(|spec| spec.signature.returns_int()) => Kind::I64,
                "int_to_duration" | "duration_to_int" | "now" | "now_monotonic" | "rand_u64"
                => Kind::I64,
                name if intrinsics::lookup(name)
                    .is_some_and(|spec| spec.signature.returns_list_element()) =>
                {
                    self.elem_kind_of_list_arg(e)
                }
                render if witchy_syntax::ast::is_render_intrinsic(render) => Kind::I32,
                "int_to_string" | "print" => Kind::I32,
                // A closure-local called by name returns the universal i64 slot,
                // recovered at its tracked return kind (see the call emission).
                other if self.local_fn_ret_kind.contains_key(other) => {
                    self.local_fn_ret_kind[other]
                }
                other => self.fn_ret.get(other).copied().unwrap_or(Kind::I32),
            },
            // `inner?` yields the Ok/Some payload; recover it at the payload's
            // kind (an Int payload as i64) so a big value isn't truncated.
            Expr::Try(inner) => self
                .ast_type_of_expr(e)
                .map(|ty| self.kind_for_type(&ty))
                .or_else(|| self.match_payload_valtype(inner).map(valtype_kind))
                .unwrap_or(Kind::I32),
            // A closure call `f(x)` returns the universal i64 slot; recover it at
            // the closure's declared return kind (an Int-returning closure as i64).
            Expr::Apply { func, .. } => self.apply_ret_kind(func),
            Expr::Ctor { name, args }
                if (name == "Some" && args.len() == 1) || (name == "None" && args.is_empty()) =>
            {
                self.ast_type_of_expr(e)
                    .and_then(|t| self.option_reference_inner(&t).map(|(_, kind)| kind))
                    .unwrap_or(Kind::I32)
            }
            Expr::Ctor { name, .. } if self.transparent_externref_ctors.contains_key(name) => {
                Kind::ExternRef
            }
            Expr::Ctor { name, .. } => self
                .gc_layout_for_ctor(name, self.ast_type_of_expr(e).as_ref())
                .map(|(_, id)| Kind::GcRef(id))
                .unwrap_or(Kind::I32),
            _ => Kind::I32, // Bool, Str, List, Ctor, Spawn
        }
    }

    /// The WASM kind of the element produced by `at(list, i)`: the list's tracked
    /// element kind, or i32 (the generic ABI) when unknown. The `at` *emission*
    /// uses the same `list_elem_kind`, so the typed-expression kind and the loaded
    /// width always agree.
    pub(crate) fn elem_kind_of_list_arg(&self, e: &Expr) -> Kind {
        if let Expr::Call { name, args } = e {
            if cap_ops::surface_name(name) == intrinsics::LIST_AT {
                if let Some(arg) = args.first() {
                    return self.list_elem_kind(arg);
                }
            }
        }
        Kind::I32
    }

    /// The WASM kind of the elements of a list expression, where determinable: a
    /// list variable, list literal, or a `-> List(T)` call (e.g. a monomorphized
    /// `fill__Int`). Used by both `at`'s type and its load, so an Int element of a
    /// call-result list is recovered as i64 rather than truncated to i32.
    pub(crate) fn list_elem_kind(&self, list: &Expr) -> Kind {
        let vt = self.elem_val_type_of(list);
        if vt != ValType::Other {
            return valtype_kind(vt);
        }
        if let Expr::Var(v) = list {
            if let Some(vt) = self.local_list_elem_valtype.get(v) {
                return valtype_kind(*vt);
            }
        }
        Kind::I32
    }

    /// (RFC-0035) Whether `list`'s element is a plain offset-0 `$rc_alloc` heap value —
    /// String, List, Tuple, a closure, or a user record/ADT — as opposed to a Dict (whose
    /// rc region starts at `ptr-4`, not `ptr`), a scalar, or an unresolvable / generic
    /// type-variable element (which under the uniform i32 ABI could be instantiated as a
    /// Dict). Gates the RC-floor `$rc_dup`/`$rc_drop` emission to the elements the plain
    /// `[ptr-8]` refcount word is correct for. Conservative BY CONSTRUCTION: only a KNOWN
    /// concrete offset-0 head returns `true`; every other case (incl. an unresolved element
    /// type or a bare type variable) returns `false`, and a missed dup/drop only leaks — it
    /// never frees a live value (the ⊥-keeps-the-count floor).
    pub(crate) fn list_elem_is_offset0_rc(&self, list: &Expr) -> bool {
        let Some(t) = self.type_table.type_of(list).and_then(witchy_types::typeck::ty_to_ast) else {
            return false;
        };
        let inner = match &t {
            Type::Qualified(_, i) => i.as_ref(),
            other => other,
        };
        if let Type::Named(head, targs) = inner {
            if head == "List" && targs.len() == 1 {
                return self.type_is_offset0_rc(&targs[0]);
            }
        }
        false
    }

    /// The core of [`list_elem_is_offset0_rc`] on a resolved source TYPE: true for
    /// String / List / Tuple / closure / a user record or ADT; false for Dict (rc region at
    /// `ptr-4`), scalars, and bare type variables (uniform-ABI-instantiable as a Dict).
    pub(crate) fn type_is_offset0_rc(&self, ty: &Type) -> bool {
        if matches!(self.kind_for_type(ty), Kind::ExternRef | Kind::GcRef(_)) {
            return false;
        }
        let inner = match ty {
            Type::Qualified(_, inner) => inner.as_ref(),
            other => other,
        };
        match inner {
            Type::Tuple(_) | Type::Fn(_, _, _) => true,
            Type::Named(n, _) => {
                n == "String"
                    || n == "List"
                    || self.adt_variants.contains_key(n)
                    || self.record_fields.contains_key(n)
            }
            _ => false,
        }
    }

    /// (RFC-0035) Whether expression `e`'s OWN type is a plain offset-0 `$rc_alloc` heap
    /// value — used at `list.set_at` to classify the DISPLACED element (which has the same
    /// type as the value being stored). Conservative: an unresolved type yields `false`.
    pub(crate) fn expr_is_offset0_rc(&self, e: &Expr) -> bool {
        self.type_table
            .type_of(e)
            .and_then(witchy_types::typeck::ty_to_ast)
            .is_some_and(|t| self.type_is_offset0_rc(&t))
    }

    pub(crate) fn block_kind(&self, b: &Block) -> Kind {
        match b.stmts.last() {
            Some(Stmt::Expr(e)) => self.kind_of(e),
            _ => Kind::I32,
        }
    }

    /// The source-level value type of an expression, to the extent codegen can
    /// determine it. Used by `to_string`; `Other` means "not distinguished".
    pub(crate) fn val_type_of(&self, e: &Expr) -> ValType {
        match self.val_type_of_inner(e) {
            // The local tracking maps came up empty: ask typeck's table (the
            // typed-lowering keystone) before giving up.
            ValType::Other => self
                .type_table
                .type_of(e)
                .and_then(witchy_types::typeck::ty_to_ast)
                .map(|t| ty_to_valtype(&t))
                .unwrap_or(ValType::Other),
            vt => vt,
        }
    }

    pub(crate) fn val_type_of_inner(&self, e: &Expr) -> ValType {
        match e {
            Expr::Int(_) | Expr::Duration(_) => ValType::Int,
            Expr::Bool(_) => ValType::Bool,
            Expr::Float(_) => ValType::Float,
            Expr::Str(_) => ValType::Str,
            Expr::Unary { op, expr } => match op {
                UnOp::Not => ValType::Bool,
                UnOp::Neg | UnOp::Move | UnOp::Await | UnOp::Borrow | UnOp::BorrowMut | UnOp::Deref => self.val_type_of(expr),
                UnOp::BitNot => ValType::Int,
            },
            Expr::Binary { op, lhs, rhs } => match op {
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
                | BinOp::And | BinOp::Or => ValType::Bool,
                // `a ?? b` yields the unwrapped payload type — the lhs's Option/
                // Result payload when known, else the fallback's type (they agree
                // by the typing rule).
                BinOp::Coalesce => match self.match_payload_valtype(lhs) {
                    Some(vt) if vt != ValType::Other => vt,
                    _ => self.val_type_of(rhs),
                },
                BinOp::Concat => ValType::Str,
                // `+` is concat when either side is a string; otherwise the
                // numeric type rides on the left operand.
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    match self.val_type_of(lhs) {
                        ValType::Other if *op == BinOp::Add => self.val_type_of(rhs),
                        vt => vt,
                    }
                }
                // Bitwise ops are always Int.
                BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                    ValType::Int
                }
            },
            Expr::Var(n) => self.local_val_types.get(n).copied().unwrap_or(ValType::Other),
            Expr::If { then_block, .. } => self.block_val_type(then_block),
            Expr::Block(b) => self.block_val_type(b),
            Expr::Match { arms, .. } => arms
                .first()
                .map(|a| self.val_type_of(&a.body))
                .unwrap_or(ValType::Other),
            // `at(xs, i)` has the list's element type, so a String element
            // compares by content (`$str_eq`) rather than by pointer.
            Expr::Call { name, args }
                if intrinsics::lookup(cap_ops::surface_name(name))
                    .is_some_and(|spec| spec.signature.returns_list_element())
                    && !args.is_empty() =>
            {
                self.elem_val_type_of(&args[0])
            }
            // `get_or(d, k, default)` returns the Dict's value type, which is the
            // default's type — so a `let v = get_or(d, k, 0)` (or a String default)
            // tracks `v`, and `v` can in turn be used as a Dict key.
            Expr::Call { name, args }
                if cap_ops::surface_name(name) == intrinsics::DICT_GET_OR && args.len() == 3 =>
            {
                self.val_type_of(&args[2])
            }
            Expr::Call { name, args }
                if cap_ops::surface_name(name) == intrinsics::DICT_AT && args.len() == 2 =>
            {
                self.dict_value_valtype_of(&args[0]).unwrap_or(ValType::Other)
            }
            Expr::Call { name, .. }
                if witchy_syntax::ast::is_render_intrinsic(cap_ops::surface_name(name)) =>
            {
                ValType::Str
            }
            Expr::Call { name, .. } => match cap_ops::surface_name(name) {
                // (BUG-609) A binding in scope shadows an intrinsic — see the
                // matching arm in `kind_of`. A closure-typed local/parameter named
                // `read`/`now`/… is typed from its own declared function type, so
                // this arm must precede every `intrinsics::lookup` arm below.
                other if self.local_fn_ret_kind.contains_key(other) => self
                    .local_types
                    .get(other)
                    .and_then(|t| match t.unqualified() {
                        Type::Fn(_, ret, _) => Some(ty_to_valtype(ret)),
                        _ => None,
                    })
                    .unwrap_or(ValType::Other),
                name if intrinsics::lookup(name)
                    .is_some_and(|spec| spec.signature.returns_string()) => ValType::Str,
                name if intrinsics::lookup(name)
                    .is_some_and(|spec| spec.signature.returns_bytes()) => ValType::Bytes,
                name if intrinsics::lookup(name)
                    .is_some_and(|spec| spec.signature.returns_bool()) => ValType::Bool,
                name if intrinsics::lookup(name)
                    .is_some_and(|spec| spec.signature.returns_int()) => ValType::Int,
                name if intrinsics::lookup(name)
                    .is_some_and(|spec| spec.signature.returns_float()) => ValType::Float,
                "read" | "read_build" | "exec" | "recv_line" | "recv_all" | "recv_bytes" =>
                    ValType::Str,
                "exists" | "is_dir" => ValType::Bool,
                "int_to_duration" | "duration_to_int" | "now" | "now_monotonic"
                | "rand_u64" => ValType::Int,
                other => self.fn_ret_valtype.get(other).copied().unwrap_or(ValType::Other),
            },
            // `inner?` yields the Ok/Some payload's value type, so `to_string` of
            // a `?`-unwrapped value renders correctly and `==` picks `$str_eq`.
            Expr::Try(inner) => self.match_payload_valtype(inner).unwrap_or(ValType::Other),
            // A record field access (`p.x`): the field's declared value type — so
            // `"${p.x}"` and `==` on a field resolve.
            Expr::Field { base, field } => {
                self.field_type_of(base, field).map(|t| ty_to_valtype(&t)).unwrap_or(ValType::Other)
            }
            _ => ValType::Other,
        }
    }

    /// The record type an expression evaluates to, where codegen can determine
    /// it locally, so a `let x = <expr>` binds `x` to that record and `x.field`
    /// resolves. Recursive: handles constructors, record-typed vars, record-
    /// returning calls, `get_or` (the default's type), `at` (a List(Record)
    /// element), `?` payloads, `update`, and the branches of if/match/block.
    pub(crate) fn record_type_of(&self, e: &Expr) -> Option<String> {
        // Structural resolution is primary — it works even where the type table is
        // silent (e.g. a synthesized node). When it misses, fall back to typeck's
        // annotation for THIS expression: the checker knows the concrete record
        // type of any call result, field projection, or generic-record field —
        // which the local-shape maps cannot see (a generic field's declared type is
        // an opaque type parameter, e.g. `Box(a).value`, but the table has `Inner`).
        self.record_type_structural(e).or_else(|| self.record_type_from_table(e))
    }

    /// Typeck's annotated record type of `e`, if it is a known record — the shape-
    /// independent fallback that closes the "field projection on a call-chain
    /// result" gap (a call result or a generic-record field the local maps miss).
    fn record_type_from_table(&self, e: &Expr) -> Option<String> {
        match self.type_table.type_of(e).and_then(witchy_types::typeck::ty_to_ast) {
            Some(witchy_syntax::ast::Type::Named(n, _)) if self.record_fields.contains_key(&n) => {
                Some(n)
            }
            _ => None,
        }
    }

    /// The record type codegen can determine from an expression's SHAPE alone
    /// (constructors, tracked locals, record-returning calls, `?` payloads,
    /// `update`, and the branches of if/match/block). `record_type_of` layers the
    /// type-table fallback on top.
    fn record_type_structural(&self, e: &Expr) -> Option<String> {
        match e {
            Expr::Ctor { name, .. } if self.record_fields.contains_key(name) => Some(name.clone()),
            // A record-typed variable tracked locally (primary; the table fallback
            // in `record_type_of` covers a binding local tracking missed).
            Expr::Var(v) => self.local_records.get(v).cloned(),
            Expr::Call { name, args } => {
                if let Some(ty) = self.fn_ret_records.get(name) {
                    Some(ty.clone())
                } else if name == intrinsics::DICT_GET_OR {
                    args.get(2).and_then(|d| self.record_type_of(d))
                } else if name == intrinsics::LIST_AT {
                    match args.first() {
                        Some(Expr::Var(v)) => self.local_list_elem.get(v).cloned(),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            Expr::Try(inner) => match inner.as_ref() {
                Expr::Call { name, .. } => self.fn_ret_result_record.get(name).cloned(),
                _ => None,
            },
            Expr::RecordUpdate { base, .. } => self.record_type_of(base),
            // `a.b` is a record when field `b` of `a`'s record type is itself a
            // record (so `a.b.c` resolves).
            Expr::Field { base, field } => {
                let base_ty = self.record_type_of(base)?;
                let names = self.record_fields.get(&base_ty)?;
                let idx = names.iter().position(|(n, _)| n == field)?;
                names[idx]
                    .1
                    .clone()
                    .filter(|t| self.record_fields.contains_key(t))
            }
            Expr::If { then_block, .. } => self.block_record_type(then_block),
            Expr::Match { arms, .. } => arms.first().and_then(|a| self.record_type_of(&a.body)),
            Expr::Block(b) => self.block_record_type(b),
            _ => None,
        }
    }

    /// The declared type of `base.field`, where `base`'s record type is known —
    /// so a field access resolves its value type (`"${p.x}"`) and its structural
    /// shape (`"${p.tags}"`), not just whether it is a record.
    pub(crate) fn field_type_of(&self, base: &Expr, field: &str) -> Option<Type> {
        let rec = self.record_type_of(base)?;
        let names = self.record_fields.get(&rec)?;
        let idx = names.iter().position(|(n, _)| n == field)?;
        self.record_field_types.get(&rec)?.get(idx).cloned()
    }

    /// The element value type of a list-producing expression, where codegen can
    /// determine it (a `split` result, a list literal, or a tracked list local),
    /// so a `for x in <iter>` loop variable's value type — and its use as a Dict
    /// key — can be resolved.
    /// The record type carried by an Option/Result scrutinee's success variant
    /// (`Some(R)` / `Ok(R)`), where codegen can determine it: a call to a
    /// function declared to return `Option(R)`/`Result(R, _)`, or a literal
    /// `Some(r)`/`Ok(r)` over a record. Lets `match f() { Some(a) -> a.field }`
    /// resolve `a`. (A generic payload, e.g. `list.find`'s `Option(a)`, is not
    /// resolvable here — that needs full instantiation tracking.)
    pub(crate) fn match_payload_record(&self, scrutinee: &Expr) -> Option<String> {
        match scrutinee {
            Expr::Var(v) => self.local_payload_records.get(v).cloned(),
            Expr::Call { name, args } => {
                // A declared `-> Option(Record)` return...
                if let Some(rec) = self.fn_ret_result_record.get(name) {
                    return Some(rec.clone());
                }
                // ...or the generic `fn(List(a),..) -> Option(a)` shape, whose
                // payload is the element record type of the given list argument.
                if let Some(&k) = self.fn_ret_option_of_list_arg.get(name) {
                    if let Some(arg) = args.get(k) {
                        return self.elem_record_type_of(arg);
                    }
                }
                None
            }
            Expr::Ctor { name, args } if (name == "Some" || name == "Ok") && args.len() == 1 => {
                self.record_type_of(&args[0])
            }
            _ => None,
        }
    }

    /// Collect `(var, record_type)` for each pattern variable bound to a
    /// record-typed constructor field, recursing through nested patterns. Lets a
    /// `match` arm like `Circle(p) -> p.x` resolve `p`'s record type.
    pub(crate) fn pattern_record_binds(&self, pat: &Pattern, out: &mut Vec<(String, String)>) {
        match pat {
            Pattern::Ctor { name, args } => {
                let field_recs = self.ctor_field_records.get(name);
                for (i, arg) in args.iter().enumerate() {
                    if let Pattern::Var(v) = arg {
                        if let Some(Some(rec)) = field_recs.and_then(|fr| fr.get(i)) {
                            out.push((v.clone(), rec.clone()));
                        }
                    }
                    self.pattern_record_binds(arg, out);
                }
            }
            Pattern::AnonCtor { args, .. } => {
                for arg in args {
                    self.pattern_record_binds(arg, out);
                }
            }
            Pattern::Tuple(args) => {
                for a in args {
                    self.pattern_record_binds(a, out);
                }
            }
            Pattern::List { elems, .. } => {
                for e in elems {
                    self.pattern_record_binds(e, out);
                }
            }
            _ => {}
        }
    }
}
