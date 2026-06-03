//! WebAssembly code generation for witchy.
//!
//! Compiles witchy functions AND actors to WAT modules. Two value
//! representations, both `i32` at the WASM level:
//!   * integers (and capability placeholders) are plain `i32`;
//!   * strings are an `i32` pointer to a length-prefixed record in linear
//!     memory: `[len: i32][utf8 bytes...]`.
//!
//! Capabilities remain host imports (`print`, `print_int`) that the runtime
//! links only when granted, so an ungranted compiled module cannot instantiate.
//!
//! An actor compiles to its own module: each non-capability `Int` field becomes
//! a mutable WASM global (its state, persisting across messages), capability
//! fields are erased (their authority is the host import), and each `on` handler
//! becomes an exported function the host calls to deliver a message.
//!
//! Not yet compiled: floats, lists, ADT constructors, `match`, string/Subject
//! message parameters, and `send` between compiled actors — each errors clearly.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::ast::{
    ActorDef, BinOp, Block, Convention, Expr, Function, Item, MatchArm, Module, Pattern, Stmt, Type,
    UnOp,
};

#[derive(Debug, Clone, PartialEq)]
pub struct CodegenError {
    pub message: String,
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "codegen error: {}", self.message)
    }
}

impl std::error::Error for CodegenError {}

fn cerr<T>(message: impl Into<String>) -> Result<T, CodegenError> {
    Err(CodegenError {
        message: message.into(),
    })
}

const DATA_BASE: u32 = 8;

/// Scratch local holding a tuple pointer while its elements are unpacked.
const TUPLE_TMP: &str = "__witchy_tuple_tmp";

/// Scratch local holding the Result/Option being unwrapped by `?`.
const TRY_TMP: &str = "__witchy_try_tmp";

/// Scratch local holding a `match` scrutinee while arms test it.
const MATCH_TMP: &str = "__witchy_match_tmp";

/// The WASM representation of a value: f64 for floats, i32 for everything else
/// (ints, bools, and pointers to strings/lists/records).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    I32,
    F64,
}

fn wasm_ty(k: Kind) -> &'static str {
    match k {
        Kind::I32 => "i32",
        Kind::F64 => "f64",
    }
}

fn ty_kind(t: &Type) -> Kind {
    match t {
        Type::Named(n, _) if n == "Float" => Kind::F64,
        _ => Kind::I32,
    }
}

struct Codegen {
    strings: Vec<(String, u32)>,
    next_offset: u32,
    uses_print: bool,
    uses_print_int: bool,
    uses_concat: bool,
    uses_int_to_string: bool,
    /// Names that resolve to mutable WASM globals (actor state).
    globals: HashSet<String>,
    /// Capability field names (erased; referencing one yields a placeholder 0).
    cap_fields: HashSet<String>,
    /// Parameter conventions per function, so call sites can write back `inout`
    /// results (move-in / move-out).
    fn_conventions: HashMap<String, Vec<Convention>>,
    /// Constructor name -> (variant tag, field count). A constructor value is a
    /// heap record `[tag: i32][field: i32]...`.
    ctors: HashMap<String, (u32, usize)>,
    /// Constructor arities for which an allocation helper `$mk{N}` is needed.
    mk_arities: HashSet<usize>,
    /// Counter for unique `match` block labels.
    next_label: u32,
    /// Whether the string-equality helper `$str_eq` is needed (string patterns).
    uses_str_eq: bool,
    /// Whether the `print_float` import is needed (a float-returning `main`).
    uses_print_float: bool,
    /// Kinds (i32/f64) of the current function's parameters and locals.
    locals: HashMap<String, Kind>,
    /// Declared return kind per function, for resolving call-result kinds.
    fn_ret: HashMap<String, Kind>,
    /// Message name -> tag, shared across a program's actors so the host can
    /// route a compiled `send` to the target actor's handler.
    message_tags: HashMap<String, u32>,
    /// Whether the inter-actor `send` import is needed.
    uses_send: bool,
    /// Whether the list `push`/`concat`/`drop` runtime helpers are needed.
    uses_list_push: bool,
    uses_list_concat: bool,
    uses_list_drop: bool,
    /// Record type name -> ordered fields as `(name, named-type)`, where the
    /// second is the field's type name when it is a `Named` type (so nested
    /// records can be chained, `a.b.c`). For compiling `value.field`.
    record_fields: HashMap<String, Vec<(String, Option<String>)>>,
    /// Variables (params / let-bound constructors) known to hold a record of a
    /// given type, so `var.field` can resolve a field index.
    local_records: HashMap<String, String>,
    /// Function name -> the record type it returns (when it returns one), so a
    /// `let q = f(...)` binds `q` to that record type.
    fn_ret_records: HashMap<String, String>,
    /// Return kind of the function currently being compiled (for `return`).
    cur_fn_ret_kind: Kind,
    /// Whether the current function has any `inout` parameters.
    cur_fn_inout: bool,
}

impl Codegen {
    fn new() -> Self {
        Self {
            strings: Vec::new(),
            next_offset: DATA_BASE,
            uses_print: false,
            uses_print_int: false,
            uses_concat: false,
            uses_int_to_string: false,
            globals: HashSet::new(),
            cap_fields: HashSet::new(),
            fn_conventions: HashMap::new(),
            ctors: HashMap::new(),
            mk_arities: HashSet::new(),
            next_label: 0,
            uses_str_eq: false,
            uses_print_float: false,
            locals: HashMap::new(),
            fn_ret: HashMap::new(),
            message_tags: HashMap::new(),
            uses_send: false,
            record_fields: HashMap::new(),
            local_records: HashMap::new(),
            fn_ret_records: HashMap::new(),
            cur_fn_ret_kind: Kind::I32,
            cur_fn_inout: false,
            uses_list_push: false,
            uses_list_concat: false,
            uses_list_drop: false,
        }
    }

    /// The WASM kind a compiled expression evaluates to.
    fn kind_of(&self, e: &Expr) -> Kind {
        match e {
            Expr::Float(_) => Kind::F64,
            Expr::Var(n) => self.locals.get(n).copied().unwrap_or(Kind::I32),
            Expr::Unary { expr, .. } => self.kind_of(expr),
            Expr::Binary { op, lhs, .. } => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => self.kind_of(lhs),
                _ => Kind::I32, // concat (ptr) and comparisons (bool) are i32
            },
            Expr::If { then_block, .. } => self.block_kind(then_block),
            Expr::Block(b) => self.block_kind(b),
            Expr::Match { arms, .. } => {
                arms.first().map(|a| self.kind_of(&a.body)).unwrap_or(Kind::I32)
            }
            Expr::Call { name, .. } => match name.as_str() {
                "int_to_float" => Kind::F64,
                "to_string" | "int_to_string" | "length" | "at" | "print" => Kind::I32,
                other => self.fn_ret.get(other).copied().unwrap_or(Kind::I32),
            },
            _ => Kind::I32, // Int, Bool, Str, List, Ctor, Spawn
        }
    }

    fn block_kind(&self, b: &Block) -> Kind {
        match b.stmts.last() {
            Some(Stmt::Expr(e)) => self.kind_of(e),
            _ => Kind::I32,
        }
    }

    /// Record the kinds of all `let`/pattern-bound locals in a body.
    fn infer_locals(&mut self, block: &Block) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    let k = self.kind_of(value);
                    self.locals.insert(name.clone(), k);
                    // Remember the binding's record type (if any) so `name.field`
                    // resolves: a constructor, a record-returning call, or an
                    // `update` of a known record.
                    let rec_ty = match value {
                        Expr::Ctor { name: ctor, .. }
                            if self.record_fields.contains_key(ctor) =>
                        {
                            Some(ctor.clone())
                        }
                        Expr::Call { name: fname, .. } => {
                            self.fn_ret_records.get(fname).cloned()
                        }
                        Expr::RecordUpdate { base, .. } => match base.as_ref() {
                            Expr::Var(v) => self.local_records.get(v).cloned(),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(ty) = rec_ty {
                        self.local_records.insert(name.clone(), ty);
                    }
                    self.infer_locals_expr(value);
                }
                Stmt::Assign { value, .. } => self.infer_locals_expr(value),
                Stmt::LetTuple { names, value } => {
                    for n in names {
                        self.locals.insert(n.clone(), Kind::I32);
                    }
                    self.infer_locals_expr(value);
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) => self.infer_locals_expr(e),
                Stmt::Return(None) => {}
            }
        }
    }

    fn infer_locals_expr(&mut self, e: &Expr) {
        match e {
            Expr::If {
                then_block,
                else_block,
                ..
            } => {
                self.infer_locals(then_block);
                if let Some(b) = else_block {
                    self.infer_locals(b);
                }
            }
            Expr::Block(b) => self.infer_locals(b),
            Expr::While { cond, body } => {
                self.infer_locals_expr(cond);
                self.infer_locals(body);
            }
            Expr::For { var, iter, body } => {
                // The loop var and the two scratch locals are all i32.
                for n in [var.clone(), format!("__forlist_{var}"), format!("__fori_{var}")] {
                    self.locals.insert(n, Kind::I32);
                }
                self.infer_locals_expr(iter);
                self.infer_locals(body);
            }
            Expr::Match { arms, .. } => {
                for arm in arms {
                    // Pattern-bound vars are i32 (floats aren't stored in records).
                    let mut pvars = Vec::new();
                    collect_pattern_vars(&arm.pattern, &mut pvars);
                    for v in pvars {
                        self.locals.insert(v, Kind::I32);
                    }
                    self.infer_locals_expr(&arm.body);
                }
            }
            _ => {}
        }
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some((_, off)) = self.strings.iter().find(|(t, _)| t == s) {
            return *off;
        }
        let off = self.next_offset;
        self.next_offset += 4 + s.len() as u32;
        self.strings.push((s.to_string(), off));
        off
    }

    fn need_heap(&self) -> bool {
        self.uses_concat
            || self.uses_int_to_string
            || !self.strings.is_empty()
            || !self.mk_arities.is_empty()
            || self.uses_list_push
            || self.uses_list_concat
            || self.uses_list_drop
    }

    fn emit_imports(&self) -> String {
        let mut s = String::new();
        if self.uses_print {
            s.push_str("  (import \"witchy\" \"print\" (func $print (param i32 i32)))\n");
        }
        if self.uses_print_int {
            s.push_str("  (import \"witchy\" \"print_int\" (func $print_int (param i32)))\n");
        }
        if self.uses_print_float {
            s.push_str("  (import \"witchy\" \"print_float\" (func $print_float (param f64)))\n");
        }
        if self.uses_send {
            // send(target_id, message_tag, arg)
            s.push_str("  (import \"witchy\" \"send\" (func $send (param i32 i32 i32)))\n");
        }
        s
    }

    /// Data segments, the heap global, any extra (actor state) globals, and the
    /// runtime helper functions — emitted in valid module-field order (globals
    /// before functions).
    fn emit_data_globals_helpers(&self, extra_globals: &str) -> String {
        let mut s = String::new();
        for (text, off) in &self.strings {
            s.push_str(&data_segment(*off, text));
        }
        if self.need_heap() {
            s.push_str(&format!(
                "  (global $heap (mut i32) (i32.const {}))\n",
                self.next_offset
            ));
        }
        s.push_str(extra_globals);
        if self.need_heap() {
            s.push_str(CONCAT_WAT);
        }
        if self.uses_list_push {
            s.push_str(LIST_PUSH_WAT);
        }
        if self.uses_list_concat {
            s.push_str(LIST_CONCAT_WAT);
        }
        if self.uses_list_drop {
            s.push_str(LIST_DROP_WAT);
        }
        if self.uses_print {
            s.push_str(PRINT_STR_WAT);
        }
        if self.uses_int_to_string {
            s.push_str(INT_TO_STRING_WAT);
        }
        if self.uses_str_eq {
            s.push_str(STR_EQ_WAT);
        }
        let mut arities: Vec<usize> = self.mk_arities.iter().copied().collect();
        arities.sort_unstable();
        for n in arities {
            s.push_str(&mk_helper(n));
        }
        s
    }

    fn compile_function(&mut self, f: &Function) -> Result<String, CodegenError> {
        self.locals.clear();
        self.local_records.clear();
        for p in &f.params {
            let k = p.ty.as_ref().map(ty_kind).unwrap_or(Kind::I32);
            self.locals.insert(p.name.clone(), k);
            // A parameter annotated with a record type lets `p.field` resolve.
            if let Some(Type::Named(n, _)) = &p.ty {
                if self.record_fields.contains_key(n) {
                    self.local_records.insert(p.name.clone(), n.clone());
                }
            }
        }
        self.infer_locals(&f.body);

        let mut header = format!("  (func ${} ", f.name);
        for p in &f.params {
            header.push_str(&format!("(param ${} {}) ", p.name, wasm_ty(self.locals[&p.name])));
        }
        // Result = the normal return value, then one slot per `inout` parameter
        // (moved back out to the caller).
        let ret_kind = match &f.ret {
            Some(t) => ty_kind(t),
            None => self.block_kind(&f.body),
        };
        self.cur_fn_ret_kind = ret_kind;
        self.cur_fn_inout = f.params.iter().any(|p| p.convention == Convention::Inout);
        header.push_str(&format!("(result {}", wasm_ty(ret_kind)));
        for p in &f.params {
            if p.convention == Convention::Inout {
                header.push_str(&format!(" {}", wasm_ty(self.locals[&p.name])));
            }
        }
        header.push_str(")\n");

        let mut lets = Vec::new();
        collect_let_names(&f.body, &mut lets);
        lets.sort();
        lets.dedup();
        for name in &lets {
            let k = self.locals.get(name).copied().unwrap_or(Kind::I32);
            header.push_str(&format!("    (local ${name} {})\n", wasm_ty(k)));
        }
        // Scratch slots: tuple destructuring, `?`, and `match` scrutinees.
        header.push_str(&format!("    (local ${TUPLE_TMP} i32)\n"));
        header.push_str(&format!("    (local ${TRY_TMP} i32)\n"));
        header.push_str(&format!("    (local ${MATCH_TMP} i32)\n"));

        let body = self.compile_block(&f.body)?;
        // Move-out: append each `inout` parameter's final value (declaration order).
        let mut epilogue = String::new();
        for p in &f.params {
            if p.convention == Convention::Inout {
                epilogue.push_str(&format!("    local.get ${}\n", p.name));
            }
        }
        Ok(format!("{header}{body}{epilogue}  )\n"))
    }

    fn compile_block(&mut self, block: &Block) -> Result<String, CodegenError> {
        let mut out = String::new();
        let last = block.stmts.len().saturating_sub(1);
        let mut tail_is_value = false;
        for (i, stmt) in block.stmts.iter().enumerate() {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    out.push_str(&self.compile_expr(value)?);
                    out.push_str(&format!("    local.set ${name}\n"));
                    tail_is_value = false;
                }
                Stmt::Assign { name, value } => {
                    out.push_str(&self.compile_expr(value)?);
                    if self.globals.contains(name) {
                        out.push_str(&format!("    global.set ${name}\n"));
                    } else {
                        out.push_str(&format!("    local.set ${name}\n"));
                    }
                    tail_is_value = false;
                }
                Stmt::LetTuple { names, value } => {
                    // Evaluate the tuple once into a scratch local, then load each
                    // element (at offset 4 + 4*i) into its binding.
                    out.push_str(&self.compile_expr(value)?);
                    out.push_str(&format!("    local.set ${TUPLE_TMP}\n"));
                    for (i, name) in names.iter().enumerate() {
                        let offset = 4 + 4 * i;
                        out.push_str(&format!(
                            "    local.get ${TUPLE_TMP}\n    i32.const {offset}\n    i32.add\n    i32.load\n    local.set ${name}\n"
                        ));
                    }
                    tail_is_value = false;
                }
                Stmt::Return(opt) => {
                    // `inout` functions return extra results; an early return
                    // would have to reproduce them, so disallow that combination.
                    if self.cur_fn_inout {
                        return cerr("`return` is not compiled for functions with `inout` parameters");
                    }
                    let value = match opt {
                        Some(e) => self.compile_expr(e)?,
                        None => format!("    {}.const 0\n", wasm_ty(self.cur_fn_ret_kind)),
                    };
                    out.push_str(&value);
                    out.push_str("    return\n");
                    // Anything after a `return` in this block is unreachable.
                    tail_is_value = false;
                }
                Stmt::Expr(e) => {
                    out.push_str(&self.compile_expr(e)?);
                    if i == last {
                        tail_is_value = true;
                    } else {
                        out.push_str("    drop\n");
                    }
                }
            }
        }
        if !tail_is_value {
            out.push_str("    i32.const 0\n");
        }
        Ok(out)
    }

    /// Compile an expression known to evaluate to a record, returning the WAT
    /// that pushes its heap pointer and the record's type name. Handles a
    /// record-typed variable and (recursively) a field whose own type is a
    /// record, so `a.b.c` resolves.
    fn record_base(&self, e: &Expr) -> Result<(String, String), CodegenError> {
        match e {
            Expr::Var(v) => match self.local_records.get(v) {
                Some(ty) => Ok((format!("    local.get ${v}\n"), ty.clone())),
                None => cerr(format!("cannot determine the record type of `{v}`")),
            },
            Expr::Field { base, field } => {
                let (base_wat, base_ty) = self.record_base(base)?;
                let names = &self.record_fields[&base_ty];
                let Some(idx) = names.iter().position(|(n, _)| n == field) else {
                    return cerr(format!("record `{base_ty}` has no field `{field}`"));
                };
                let field_ty = names[idx].1.clone();
                let Some(rec_ty) = field_ty.filter(|t| self.record_fields.contains_key(t)) else {
                    return cerr(format!("field `{field}` of `{base_ty}` is not a record"));
                };
                let offset = 4 + 4 * idx;
                Ok((
                    format!("{base_wat}    i32.const {offset}\n    i32.add\n    i32.load\n"),
                    rec_ty,
                ))
            }
            _ => cerr("a record base must be a variable or a field access"),
        }
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<String, CodegenError> {
        match expr {
            Expr::Int(n) => Ok(format!("    i32.const {n}\n")),
            Expr::Bool(b) => Ok(format!("    i32.const {}\n", if *b { 1 } else { 0 })),
            Expr::Str(s) => {
                let off = self.intern(s);
                Ok(format!("    i32.const {off}\n"))
            }
            Expr::Var(name) => {
                if self.cap_fields.contains(name) {
                    Ok("    i32.const 0\n".to_string())
                } else if self.globals.contains(name) {
                    Ok(format!("    global.get ${name}\n"))
                } else {
                    Ok(format!("    local.get ${name}\n"))
                }
            }
            Expr::Unary { op, expr } => match op {
                UnOp::Not => Ok(format!("{}    i32.eqz\n", self.compile_expr(expr)?)),
                UnOp::Neg => {
                    if self.kind_of(expr) == Kind::F64 {
                        Ok(format!("{}    f64.neg\n", self.compile_expr(expr)?))
                    } else {
                        Ok(format!("    i32.const 0\n{}    i32.sub\n", self.compile_expr(expr)?))
                    }
                }
            },
            Expr::Binary { op, lhs, rhs } => {
                if *op == BinOp::Concat {
                    self.uses_concat = true;
                    let l = self.compile_expr(lhs)?;
                    let r = self.compile_expr(rhs)?;
                    return Ok(format!("{l}{r}    call $concat\n"));
                }
                // Short-circuit: `a && b` -> if a then b else 0; `a || b` -> if a then 1 else b.
                if *op == BinOp::And {
                    return Ok(format!(
                        "{}    if (result i32)\n{}    else\n    i32.const 0\n    end\n",
                        self.compile_expr(lhs)?,
                        self.compile_expr(rhs)?
                    ));
                }
                if *op == BinOp::Or {
                    return Ok(format!(
                        "{}    if (result i32)\n    i32.const 1\n    else\n{}    end\n",
                        self.compile_expr(lhs)?,
                        self.compile_expr(rhs)?
                    ));
                }
                let float = self.kind_of(lhs) == Kind::F64;
                let l = self.compile_expr(lhs)?;
                let r = self.compile_expr(rhs)?;
                let opcode = match (op, float) {
                    (BinOp::Add, false) => "i32.add",
                    (BinOp::Add, true) => "f64.add",
                    (BinOp::Sub, false) => "i32.sub",
                    (BinOp::Sub, true) => "f64.sub",
                    (BinOp::Mul, false) => "i32.mul",
                    (BinOp::Mul, true) => "f64.mul",
                    (BinOp::Div, false) => "i32.div_s",
                    (BinOp::Div, true) => "f64.div",
                    (BinOp::Mod, _) => "i32.rem_s",
                    (BinOp::Eq, false) => "i32.eq",
                    (BinOp::Eq, true) => "f64.eq",
                    (BinOp::NotEq, false) => "i32.ne",
                    (BinOp::NotEq, true) => "f64.ne",
                    (BinOp::Lt, false) => "i32.lt_s",
                    (BinOp::Lt, true) => "f64.lt",
                    (BinOp::LtEq, false) => "i32.le_s",
                    (BinOp::LtEq, true) => "f64.le",
                    (BinOp::Gt, false) => "i32.gt_s",
                    (BinOp::Gt, true) => "f64.gt",
                    (BinOp::GtEq, false) => "i32.ge_s",
                    (BinOp::GtEq, true) => "f64.ge",
                    (BinOp::Concat | BinOp::And | BinOp::Or, _) => unreachable!("handled above"),
                };
                Ok(format!("{l}{r}    {opcode}\n"))
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                // No `else` means the `if` is used for effect (Nil); yield 0.
                let else_wat = match else_block {
                    Some(eb) => self.compile_block(eb)?,
                    None => "    i32.const 0\n".to_string(),
                };
                Ok(format!(
                    "{}    if (result i32)\n{}    else\n{else_wat}    end\n",
                    self.compile_expr(cond)?,
                    self.compile_block(then_block)?,
                ))
            }
            Expr::Block(b) => self.compile_block(b),
            Expr::While { cond, body } => {
                let id = self.next_label;
                self.next_label += 1;
                let c = self.compile_expr(cond)?;
                let b = self.compile_block(body)?;
                Ok(format!(
                    "    block $we{id}\n    loop $wl{id}\n{c}    i32.eqz\n    br_if $we{id}\n{b}    drop\n    br $wl{id}\n    end\n    end\n    i32.const 0\n"
                ))
            }
            Expr::Call { name, args } => self.compile_call(name, args),
            Expr::Float(x) => Ok(format!("    f64.const {x}\n")),
            Expr::Tuple(items) => {
                // A tuple is a heap record [0][elem0][elem1]...] (a 0 tag, then
                // the elements), reusing the constructor allocator. i32 elements
                // only (the slots are 4 bytes wide).
                if items.iter().any(|e| self.kind_of(e) == Kind::F64) {
                    return cerr("tuples with Float elements are not compiled to WASM yet");
                }
                let n = items.len();
                self.mk_arities.insert(n);
                let mut out = String::from("    i32.const 0\n");
                for item in items {
                    out.push_str(&self.compile_expr(item)?);
                }
                out.push_str(&format!("    call $mk{n}\n"));
                Ok(out)
            }
            Expr::Try(inner) => {
                // The type checker guarantees `inner` is a Result/Option, whose
                // success variant (Ok/Some) is tag 0 carrying one payload. So:
                // if tag==0, take the payload; otherwise early-return the whole
                // value (the Err/None) — which needs the function's `return`.
                if self.cur_fn_inout {
                    return cerr("`?` is not compiled for functions with `inout` parameters");
                }
                let v = self.compile_expr(inner)?;
                Ok(format!(
                    "{v}    local.set ${TRY_TMP}\n    \
                     local.get ${TRY_TMP}\n    i32.load\n    i32.eqz\n    \
                     if (result i32)\n    \
                     local.get ${TRY_TMP}\n    i32.const 4\n    i32.add\n    i32.load\n    \
                     else\n    local.get ${TRY_TMP}\n    return\n    i32.const 0\n    end\n"
                ))
            }
            Expr::For { var, iter, body } => {
                // Iterate a list `[len][e0][e1]...`: keep the list pointer and an
                // index in scratch locals (named after the loop var so they're
                // declared), bind each element, run the body, and drop its value.
                let id = self.next_label;
                self.next_label += 1;
                let list_l = format!("__forlist_{var}");
                let idx_l = format!("__fori_{var}");
                let iter_wat = self.compile_expr(iter)?;
                let body_wat = self.compile_block(body)?;
                Ok(format!(
                    "{iter_wat}    local.set ${list_l}\n    \
                     i32.const 0\n    local.set ${idx_l}\n    \
                     block $fe{id}\n    loop $fl{id}\n    \
                     local.get ${idx_l}\n    local.get ${list_l}\n    i32.load\n    i32.ge_s\n    br_if $fe{id}\n    \
                     local.get ${list_l}\n    i32.const 4\n    i32.add\n    local.get ${idx_l}\n    i32.const 4\n    i32.mul\n    i32.add\n    i32.load\n    local.set ${var}\n\
                     {body_wat}    drop\n    \
                     local.get ${idx_l}\n    i32.const 1\n    i32.add\n    local.set ${idx_l}\n    \
                     br $fl{id}\n    end\n    end\n    i32.const 0\n"
                ))
            }
            Expr::Field { base, field } => {
                // A record value is a heap record `[tag][field0][field1]...`, so
                // a field is `*(base + 4 + 4*index)`. `record_base` resolves the
                // base pointer and its record type (recursively, so `a.b.c`
                // works on nested records).
                let (base_wat, base_ty) = self.record_base(base)?;
                let names = &self.record_fields[&base_ty];
                let Some(idx) = names.iter().position(|(n, _)| n == field) else {
                    return cerr(format!("record `{base_ty}` has no field `{field}`"));
                };
                let offset = 4 + 4 * idx;
                Ok(format!(
                    "{base_wat}    i32.const {offset}\n    i32.add\n    i32.load\n"
                ))
            }
            Expr::RecordUpdate { base, fields } => {
                // Build a fresh record: push the tag, then each field — the
                // override expression where given, else a load from the base.
                let Expr::Var(v) = base.as_ref() else {
                    return cerr("record update in WASM needs a record-typed variable");
                };
                let Some(tyname) = self.local_records.get(v).cloned() else {
                    return cerr(format!("cannot determine the record type of `{v}` to update"));
                };
                let names = self.record_fields[&tyname].clone();
                let (tag, nfields) = self.ctors[&tyname];
                self.mk_arities.insert(nfields);
                let mut out = format!("    i32.const {tag}\n");
                for (i, (fname, _)) in names.iter().enumerate() {
                    if let Some((_, vexpr)) = fields.iter().find(|(n, _)| n == fname) {
                        out.push_str(&self.compile_expr(vexpr)?);
                    } else {
                        let offset = 4 + 4 * i;
                        out.push_str(&format!(
                            "    local.get ${v}\n    i32.const {offset}\n    i32.add\n    i32.load\n"
                        ));
                    }
                }
                out.push_str(&format!("    call $mk{nfields}\n"));
                Ok(out)
            }
            Expr::Lambda { .. } => cerr("lambdas are not compiled to WASM yet"),
            Expr::List(items) => {
                // A list is a record [len][elem0..]; reuse the $mk{N} helper with
                // the length as the header slot.
                let n = items.len();
                self.mk_arities.insert(n);
                let mut out = format!("    i32.const {n}\n");
                for item in items {
                    out.push_str(&self.compile_expr(item)?);
                }
                out.push_str(&format!("    call $mk{n}\n"));
                Ok(out)
            }
            Expr::Ctor { name, args } => {
                let Some(&(tag, nfields)) = self.ctors.get(name) else {
                    return cerr(format!(
                        "unknown constructor `{name}` (declare it with `type`)"
                    ));
                };
                if nfields != args.len() {
                    return cerr(format!(
                        "constructor `{name}` takes {nfields} field(s) but got {}",
                        args.len()
                    ));
                }
                self.mk_arities.insert(nfields);
                let mut out = format!("    i32.const {tag}\n");
                for arg in args {
                    out.push_str(&self.compile_expr(arg)?);
                }
                out.push_str(&format!("    call $mk{nfields}\n"));
                Ok(out)
            }
            Expr::Match { scrutinee, arms } => self.compile_match(scrutinee, arms),
            Expr::Spawn { .. } => cerr("`spawn` is not compiled to WASM yet"),
        }
    }

    /// Compile a `match` on a scalar (`Int`/`Bool`) scrutinee into a chain of
    /// `if`s. The scrutinee must be a variable or literal so it can be safely
    /// re-evaluated per arm without a temporary. Variable/string/constructor
    /// patterns and guards are not compiled yet.
    fn compile_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) -> Result<String, CodegenError> {
        // Patterns re-read the scrutinee, so evaluate it once into a scratch
        // local and use `local.get` (cheap, side-effect-free) as the value. The
        // matching arm's body runs only after its pattern is tested and bound, so
        // a nested match reusing this slot is safe.
        let scrut_setup = format!("{}    local.set ${MATCH_TMP}\n", self.compile_expr(scrutinee)?);
        let scrut = format!("    local.get ${MATCH_TMP}\n");
        let id = self.next_label;
        self.next_label += 1;
        // Each arm is a block: test the pattern (skip on failure), bind, test the
        // guard (skip on failure), run the body and branch out with its value.
        let mut s = scrut_setup;
        s.push_str(&format!("    block $d{id} (result i32)\n"));
        for (i, arm) in arms.iter().enumerate() {
            let (cond, binds) = self.pattern_match(&scrut, &arm.pattern)?;
            s.push_str(&format!("    block $a{id}_{i}\n"));
            s.push_str(&cond);
            s.push_str(&format!("    i32.eqz\n    br_if $a{id}_{i}\n"));
            s.push_str(&binds);
            if let Some(guard) = &arm.guard {
                s.push_str(&self.compile_expr(guard)?);
                s.push_str(&format!("    i32.eqz\n    br_if $a{id}_{i}\n"));
            }
            s.push_str(&self.compile_expr(&arm.body)?);
            s.push_str(&format!("    br $d{id}\n    end\n"));
        }
        s.push_str("    unreachable\n    end\n");
        Ok(s)
    }

    /// Test a pattern against the value produced by `value` (an i32-producing
    /// instruction sequence). Returns the condition instructions (leaving an
    /// i32 on the stack) and the binding instructions to run once it matches.
    /// Conditions short-circuit so a mismatched constructor's fields are never
    /// dereferenced. Recursive, so constructor patterns may nest.
    fn pattern_match(
        &mut self,
        value: &str,
        pat: &Pattern,
    ) -> Result<(String, String), CodegenError> {
        const TRUE: &str = "    i32.const 1\n";
        Ok(match pat {
            Pattern::Wildcard => (TRUE.to_string(), String::new()),
            Pattern::Int(k) => (format!("{value}    i32.const {k}\n    i32.eq\n"), String::new()),
            Pattern::Bool(b) => (
                format!("{value}    i32.const {}\n    i32.eq\n", if *b { 1 } else { 0 }),
                String::new(),
            ),
            Pattern::Var(name) => (
                TRUE.to_string(),
                format!("{value}    local.set ${name}\n"),
            ),
            Pattern::Tuple(pats) => {
                // A tuple is `[0][elem0][elem1]...`; there's no tag to check
                // (tuples always match by shape), so the condition is just the
                // AND of the element-pattern conditions.
                let mut elem_conds = Vec::new();
                let mut binds = String::new();
                for (i, sub) in pats.iter().enumerate() {
                    let elem_value =
                        format!("{value}    i32.const {}\n    i32.add\n    i32.load\n", 4 + 4 * i);
                    let (sub_cond, sub_binds) = self.pattern_match(&elem_value, sub)?;
                    if sub_cond != TRUE {
                        elem_conds.push(sub_cond);
                    }
                    binds.push_str(&sub_binds);
                }
                let cond = if elem_conds.is_empty() {
                    TRUE.to_string()
                } else {
                    and_chain(&elem_conds)
                };
                (cond, binds)
            }
            Pattern::List { elems, rest } => {
                // A list is `[len][e0]...`. Check the length first (exact when
                // there's no `..`, else a minimum), and only then inspect the
                // prefix elements (so a short list never reads out of bounds).
                let n = elems.len();
                let len_cmp = if rest.is_some() { "i32.ge_s" } else { "i32.eq" };
                let len_check = format!("{value}    i32.load\n    i32.const {n}\n    {len_cmp}\n");
                let mut elem_conds = Vec::new();
                let mut binds = String::new();
                for (i, sub) in elems.iter().enumerate() {
                    let elem_value =
                        format!("{value}    i32.const {}\n    i32.add\n    i32.load\n", 4 + 4 * i);
                    let (sc, sb) = self.pattern_match(&elem_value, sub)?;
                    if sc != TRUE {
                        elem_conds.push(sc);
                    }
                    binds.push_str(&sb);
                }
                // `..name` binds the remaining tail as a freshly allocated list.
                if let Some(Some(name)) = rest {
                    self.uses_list_drop = true;
                    binds.push_str(&format!(
                        "{value}    i32.const {n}\n    call $list_drop\n    local.set ${name}\n"
                    ));
                }
                let inner = and_chain(&elem_conds);
                let cond =
                    format!("{len_check}    if (result i32)\n{inner}    else\n    i32.const 0\n    end\n");
                (cond, binds)
            }
            Pattern::Str(s) => {
                self.uses_str_eq = true;
                let off = self.intern(s);
                (
                    format!("{value}    i32.const {off}\n    call $str_eq\n"),
                    String::new(),
                )
            }
            Pattern::Ctor { name, args } => {
                let Some(&(tag, nfields)) = self.ctors.get(name) else {
                    return cerr(format!("unknown constructor `{name}` in pattern"));
                };
                if nfields != args.len() {
                    return cerr(format!(
                        "pattern `{name}` takes {nfields} field(s) but matched {}",
                        args.len()
                    ));
                }
                let mut field_conds = Vec::new();
                let mut binds = String::new();
                for (i, sub) in args.iter().enumerate() {
                    let field_value =
                        format!("{value}    i32.const {}\n    i32.add\n    i32.load\n", 4 + 4 * i);
                    let (sub_cond, sub_binds) = self.pattern_match(&field_value, sub)?;
                    if sub_cond != TRUE {
                        field_conds.push(sub_cond);
                    }
                    binds.push_str(&sub_binds);
                }
                // Only inspect fields once the tag has matched (short-circuit).
                let inner = and_chain(&field_conds);
                let cond = format!(
                    "{value}    i32.load\n    i32.const {tag}\n    i32.eq\n    if (result i32)\n{inner}    else\n    i32.const 0\n    end\n"
                );
                (cond, binds)
            }
        })
    }

    fn compile_call(&mut self, name: &str, args: &[Expr]) -> Result<String, CodegenError> {
        match (name, args.len()) {
            ("print", 2) => {
                self.uses_print = true;
                let msg = self.compile_expr(&args[1])?;
                Ok(format!("{msg}    call $print_str\n    i32.const 0\n"))
            }
            ("int_to_string", 1) => {
                self.uses_int_to_string = true;
                let arg = self.compile_expr(&args[0])?;
                Ok(format!("{arg}    call $int_to_string\n"))
            }
            // The string record's header is its byte length.
            ("string_length", 1) => {
                let arg = self.compile_expr(&args[0])?;
                Ok(format!("{arg}    i32.load\n"))
            }
            ("int_to_float", 1) => {
                Ok(format!("{}    f64.convert_i32_s\n", self.compile_expr(&args[0])?))
            }
            ("float_to_int", 1) => {
                Ok(format!("{}    i32.trunc_f64_s\n", self.compile_expr(&args[0])?))
            }
            ("string_to_int", _) => {
                cerr("string_to_int runs in the interpreter (WASM string parsing is future)")
            }
            ("to_upper", _) | ("to_lower", _) | ("trim", _) | ("starts_with", _) => cerr(
                "string stdlib functions run in the interpreter; WASM string ops are future",
            ),
            // length(list): the record header is the length.
            ("length", 1) => {
                let arg = self.compile_expr(&args[0])?;
                Ok(format!("{arg}    i32.load\n"))
            }
            // at(list, i): load element at ptr + 4 + i*4.
            ("at", 2) => {
                let list = self.compile_expr(&args[0])?;
                let idx = self.compile_expr(&args[1])?;
                Ok(format!(
                    "{list}    i32.const 4\n    i32.add\n{idx}    i32.const 4\n    i32.mul\n    i32.add\n    i32.load\n"
                ))
            }
            // push(list, x) / concat(a, b): allocate a new list (runtime helper).
            ("push", 2) => {
                self.uses_list_push = true;
                let list = self.compile_expr(&args[0])?;
                let x = self.compile_expr(&args[1])?;
                Ok(format!("{list}{x}    call $list_push\n"))
            }
            ("concat", 2) => {
                self.uses_list_concat = true;
                let a = self.compile_expr(&args[0])?;
                let b = self.compile_expr(&args[1])?;
                Ok(format!("{a}{b}    call $list_concat\n"))
            }
            // send(subject, Message(arg)): route to the target actor's handler.
            ("send", 2) => {
                let Expr::Ctor { name: msg, args: fields } = &args[1] else {
                    return cerr("send expects a message constructor as its second argument");
                };
                let Some(&tag) = self.message_tags.get(msg) else {
                    return cerr(format!("send to unknown message `{msg}` (no handler declares it)"));
                };
                if fields.len() > 1 {
                    return cerr("only messages with 0 or 1 fields are compiled yet");
                }
                self.uses_send = true;
                let target = self.compile_expr(&args[0])?;
                let arg = if fields.len() == 1 {
                    self.compile_expr(&fields[0])?
                } else {
                    "    i32.const 0\n".to_string()
                };
                Ok(format!(
                    "{target}    i32.const {tag}\n{arg}    call $send\n    i32.const 0\n"
                ))
            }
            ("spawn", _) => cerr("`spawn` is not compiled to WASM yet (host-driven)"),
            ("read", _) | ("subdir", _) => cerr(
                "filesystem capabilities are not compiled to WASM yet (interpreter only; maps to WASI preopens)",
            ),
            ("connect", _) | ("restrict", _) | ("send_line", _) | ("recv_line", _) => cerr(
                "network capabilities are not compiled to WASM yet (interpreter only; maps to wasi:sockets)",
            ),
            _ => {
                let mut out = String::new();
                for arg in args {
                    out.push_str(&self.compile_expr(arg)?);
                }
                out.push_str(&format!("    call ${name}\n"));
                // Write back `inout` outputs, which sit on top of the stack in
                // reverse declaration order above the normal return value.
                if let Some(convs) = self.fn_conventions.get(name).cloned() {
                    for (i, conv) in convs.iter().enumerate().rev() {
                        if *conv == Convention::Inout {
                            match &args[i] {
                                Expr::Var(v) if self.globals.contains(v) => {
                                    out.push_str(&format!("    global.set ${v}\n"));
                                }
                                Expr::Var(v) => {
                                    out.push_str(&format!("    local.set ${v}\n"));
                                }
                                _ => {
                                    return cerr(format!(
                                        "`inout` argument to `{name}` must be a variable"
                                    ))
                                }
                            }
                        }
                    }
                }
                Ok(out)
            }
        }
    }
}

/// Compile a module's functions to WAT. Requires a `main` returning Int or Nil;
/// `main` may take a single capability parameter.
pub fn compile_module(module: &Module) -> Result<String, CodegenError> {
    let mut cg = Codegen::new();
    // Collect parameter conventions up front so call sites can resolve `inout`
    // write-back even for forward references.
    for item in &module.items {
        match item {
            Item::Function(f) => {
                cg.fn_conventions
                    .insert(f.name.clone(), f.params.iter().map(|p| p.convention).collect());
                let ret = f.ret.as_ref().map(ty_kind).unwrap_or(Kind::I32);
                cg.fn_ret.insert(f.name.clone(), ret);
            }
            Item::Type(t) => {
                for (tag, variant) in t.variants.iter().enumerate() {
                    cg.ctors
                        .insert(variant.name.clone(), (tag as u32, variant.fields.len()));
                    if !variant.field_names.is_empty() {
                        let fields = variant
                            .field_names
                            .iter()
                            .zip(&variant.fields)
                            .map(|(name, ty)| {
                                let ty_name = match ty {
                                    Type::Named(n, _) => Some(n.clone()),
                                    _ => None,
                                };
                                (name.clone(), ty_name)
                            })
                            .collect();
                        cg.record_fields.insert(t.name.clone(), fields);
                    }
                }
            }
            Item::Actor(_) => {}
        }
    }
    // Now that record types are known, note which functions return one, so a
    // `let q = f(...)` can resolve `q.field`.
    for item in &module.items {
        if let Item::Function(f) = item {
            if let Some(Type::Named(n, _)) = &f.ret {
                if cg.record_fields.contains_key(n) {
                    cg.fn_ret_records.insert(f.name.clone(), n.clone());
                }
            }
        }
    }
    let mut func_wat = String::new();
    let mut main_params = 0usize;
    let mut main_returns_int = false;
    let mut main_returns_float = false;
    let mut has_main = false;

    for item in &module.items {
        match item {
            Item::Function(f) => {
                if f.name == "main" {
                    has_main = true;
                    main_params = f.params.len();
                    main_returns_int = matches!(&f.ret, Some(Type::Named(n, _)) if n == "Int");
                    main_returns_float = matches!(&f.ret, Some(Type::Named(n, _)) if n == "Float");
                    if main_params > 1 {
                        return cerr("codegen `main` may take at most one (capability) argument");
                    }
                }
                func_wat.push_str(&cg.compile_function(f)?);
            }
            Item::Type(_) => {}
            Item::Actor(_) => return cerr("use compile_actor_module to compile an actor"),
        }
    }
    if !has_main {
        return cerr("no `main` function to compile");
    }
    if main_returns_int {
        cg.uses_print_int = true;
    }
    if main_returns_float {
        cg.uses_print_float = true;
    }

    let mut wat = String::from("(module\n");
    wat.push_str(&cg.emit_imports());
    wat.push_str("  (memory (export \"memory\") 1)\n");
    wat.push_str(&cg.emit_data_globals_helpers(""));
    wat.push_str(&func_wat);

    wat.push_str("  (func (export \"run\")\n");
    for _ in 0..main_params {
        wat.push_str("    i32.const 0\n");
    }
    wat.push_str("    call $main\n");
    if main_returns_int {
        wat.push_str("    call $print_int)\n");
    } else if main_returns_float {
        wat.push_str("    call $print_float)\n");
    } else {
        wat.push_str("    drop)\n");
    }
    wat.push_str(")\n");
    Ok(wat)
}

/// Compile a single actor to its own WASM module. Int fields with literal
/// initializers become mutable globals (state); capability fields are erased;
/// each handler becomes an exported function.
pub fn compile_actor_module(actor: &ActorDef) -> Result<String, CodegenError> {
    compile_actor_with_tags(actor, &HashMap::new())
}

/// Compile every actor in a module, assigning each distinct handler message a
/// shared tag so the host can route inter-actor sends. Returns (actor name,
/// WAT) pairs and the tag -> message-name table.
pub fn compile_program(
    module: &Module,
) -> Result<(Vec<(String, String)>, Vec<String>), CodegenError> {
    let mut tag_of: HashMap<String, u32> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    for item in &module.items {
        if let Item::Actor(a) = item {
            for h in &a.handlers {
                if !tag_of.contains_key(&h.message) {
                    tag_of.insert(h.message.clone(), names.len() as u32);
                    names.push(h.message.clone());
                }
            }
        }
    }
    let mut actors = Vec::new();
    for item in &module.items {
        if let Item::Actor(a) = item {
            actors.push((a.name.clone(), compile_actor_with_tags(a, &tag_of)?));
        }
    }
    Ok((actors, names))
}

fn compile_actor_with_tags(
    actor: &ActorDef,
    tags: &HashMap<String, u32>,
) -> Result<String, CodegenError> {
    let mut cg = Codegen::new();
    cg.message_tags = tags.clone();

    let mut state_globals = String::new();
    for field in &actor.fields {
        let tname = match &field.ty {
            Type::Named(n, _) => n.as_str(),
            Type::Tuple(_) => {
                return cerr(format!(
                    "actor field `{}`: tuple-typed fields are not compiled yet",
                    field.name
                ))
            }
            Type::Fn(..) => {
                return cerr(format!(
                    "actor field `{}`: function-typed fields are not compiled yet",
                    field.name
                ))
            }
        };
        // Console is erased (its authority is the linked `print` import).
        if tname == "Console" {
            cg.cap_fields.insert(field.name.clone());
            continue;
        }
        // A Subject is a real i32 (the target's id), exported so the host can
        // set it at spawn.
        if tname == "Subject" {
            cg.globals.insert(field.name.clone());
            state_globals.push_str(&format!(
                "  (global ${0} (export \"{0}\") (mut i32) (i32.const 0))\n",
                field.name
            ));
            continue;
        }
        if tname != "Int" {
            return cerr(format!(
                "actor field `{}`: only Int state and capability fields compile yet",
                field.name
            ));
        }
        let init = match &field.init {
            Some(Expr::Int(n)) => *n,
            Some(_) => return cerr(format!("field `{}`: initializer must be an Int literal", field.name)),
            None => {
                return cerr(format!(
                    "field `{}`: Int state needs an initializer in codegen",
                    field.name
                ))
            }
        };
        cg.globals.insert(field.name.clone());
        state_globals.push_str(&format!(
            "  (global ${} (mut i32) (i32.const {init}))\n",
            field.name
        ));
    }

    let mut handlers: Vec<(String, String)> = Vec::new();
    for h in &actor.handlers {
        for p in &h.params {
            if !matches!(&p.ty, Some(Type::Named(t, _)) if t == "Int") {
                return cerr(format!(
                    "handler `{}` param `{}`: only Int message parameters compile yet",
                    h.message, p.name
                ));
            }
        }
        let mut header = format!("  (func (export \"{}\") ", h.message);
        for p in &h.params {
            header.push_str(&format!("(param ${} i32) ", p.name));
        }
        header.push('\n');
        let mut lets = Vec::new();
        collect_let_names(&h.body, &mut lets);
        lets.sort();
        lets.dedup();
        for name in &lets {
            header.push_str(&format!("    (local ${name} i32)\n"));
        }
        let body = cg.compile_block(&h.body)?;
        handlers.push((header, body));
    }

    // No-GC for actors: reset the heap arena at the start of each message, since
    // a handler's heap allocations never escape (state lives in globals; sends
    // copy). This bounds memory for long-running actors without a collector.
    let mut extra_globals = state_globals;
    let reset = if cg.need_heap() {
        extra_globals.push_str(&format!(
            "  (global $heap_base i32 (i32.const {}))\n",
            cg.next_offset
        ));
        "    global.get $heap_base\n    global.set $heap\n"
    } else {
        ""
    };

    let mut wat = String::from("(module\n");
    wat.push_str(&cg.emit_imports());
    wat.push_str("  (memory (export \"memory\") 1)\n");
    wat.push_str(&cg.emit_data_globals_helpers(&extra_globals));
    for (header, body) in &handlers {
        // Handlers return nothing; discard the block's trailing value.
        wat.push_str(&format!("{header}{reset}{body}    drop\n  )\n"));
    }
    wat.push_str(")\n");
    Ok(wat)
}

fn data_segment(off: u32, s: &str) -> String {
    let mut bytes = (s.len() as u32).to_le_bytes().to_vec();
    bytes.extend_from_slice(s.as_bytes());
    let escaped: String = bytes.iter().map(|b| format!("\\{b:02x}")).collect();
    format!("  (data (i32.const {off}) \"{escaped}\")\n")
}

/// Short-circuiting AND of a list of i32-producing condition sequences.
fn and_chain(conds: &[String]) -> String {
    match conds.split_first() {
        None => "    i32.const 1\n".to_string(),
        Some((first, rest)) => {
            let rest = and_chain(rest);
            format!("{first}    if (result i32)\n{rest}    else\n    i32.const 0\n    end\n")
        }
    }
}

/// String equality over two length-prefixed records `[len][bytes]`.
const STR_EQ_WAT: &str = r#"  (func $str_eq (param $a i32) (param $b i32) (result i32)
    (local $len i32) (local $i i32)
    (if (i32.ne (i32.load (local.get $a)) (i32.load (local.get $b)))
      (then (return (i32.const 0))))
    (local.set $len (i32.load (local.get $a)))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $len)))
        (if (i32.ne
              (i32.load8_u (i32.add (i32.add (local.get $a) (i32.const 4)) (local.get $i)))
              (i32.load8_u (i32.add (i32.add (local.get $b) (i32.const 4)) (local.get $i))))
          (then (return (i32.const 0))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.const 1))
"#;

/// Allocation helper for an N-field constructor record `[tag][f0..f{N-1}]`.
fn mk_helper(n: usize) -> String {
    let mut params = String::from("(param $tag i32)");
    for i in 0..n {
        params.push_str(&format!(" (param $f{i} i32)"));
    }
    let size = 4 + 4 * n;
    let mut s = format!("  (func $mk{n} {params} (result i32)\n    (local $p i32)\n");
    s.push_str("    global.get $heap local.set $p\n");
    s.push_str("    local.get $p local.get $tag i32.store\n");
    for i in 0..n {
        s.push_str(&format!(
            "    local.get $p i32.const {} i32.add local.get $f{i} i32.store\n",
            4 + 4 * i
        ));
    }
    s.push_str(&format!("    local.get $p i32.const {size} i32.add global.set $heap\n"));
    s.push_str("    local.get $p)\n");
    s
}

const CONCAT_WAT: &str = r#"  (func $concat (param $a i32) (param $b i32) (result i32)
    (local $alen i32) (local $blen i32) (local $res i32)
    local.get $a i32.load local.set $alen
    local.get $b i32.load local.set $blen
    global.get $heap local.set $res
    local.get $res local.get $alen local.get $blen i32.add i32.store
    local.get $res i32.const 4 i32.add
    local.get $a i32.const 4 i32.add
    local.get $alen
    memory.copy
    local.get $res i32.const 4 i32.add local.get $alen i32.add
    local.get $b i32.const 4 i32.add
    local.get $blen
    memory.copy
    local.get $res i32.const 4 i32.add local.get $alen i32.add local.get $blen i32.add
    global.set $heap
    local.get $res)
"#;

// push(list, x): a fresh list `[len+1][elems...][x]`. Elements are 4-byte i32s,
// so the element block is copied with `memory.copy`.
const LIST_PUSH_WAT: &str = r#"  (func $list_push (param $list i32) (param $x i32) (result i32)
    (local $len i32) (local $new i32)
    local.get $list i32.load local.set $len
    global.get $heap local.set $new
    local.get $new local.get $len i32.const 1 i32.add i32.store
    local.get $new i32.const 4 i32.add
    local.get $list i32.const 4 i32.add
    local.get $len i32.const 4 i32.mul
    memory.copy
    local.get $new i32.const 4 i32.add local.get $len i32.const 4 i32.mul i32.add
    local.get $x i32.store
    local.get $new local.get $len i32.const 2 i32.add i32.const 4 i32.mul i32.add global.set $heap
    local.get $new)
"#;

// concat(a, b): a fresh list `[alen+blen][a elems][b elems]`.
const LIST_CONCAT_WAT: &str = r#"  (func $list_concat (param $a i32) (param $b i32) (result i32)
    (local $alen i32) (local $blen i32) (local $new i32)
    local.get $a i32.load local.set $alen
    local.get $b i32.load local.set $blen
    global.get $heap local.set $new
    local.get $new local.get $alen local.get $blen i32.add i32.store
    local.get $new i32.const 4 i32.add
    local.get $a i32.const 4 i32.add
    local.get $alen i32.const 4 i32.mul
    memory.copy
    local.get $new i32.const 4 i32.add local.get $alen i32.const 4 i32.mul i32.add
    local.get $b i32.const 4 i32.add
    local.get $blen i32.const 4 i32.mul
    memory.copy
    local.get $new local.get $alen local.get $blen i32.add i32.const 1 i32.add i32.const 4 i32.mul i32.add global.set $heap
    local.get $new)
"#;

// drop(list, k): the sublist `[len-k][elem_k...]` (used by `[h, ..t]` patterns).
const LIST_DROP_WAT: &str = r#"  (func $list_drop (param $list i32) (param $k i32) (result i32)
    (local $newlen i32) (local $new i32)
    local.get $list i32.load local.get $k i32.sub local.set $newlen
    global.get $heap local.set $new
    local.get $new local.get $newlen i32.store
    local.get $new i32.const 4 i32.add
    local.get $list i32.const 4 i32.add local.get $k i32.const 4 i32.mul i32.add
    local.get $newlen i32.const 4 i32.mul
    memory.copy
    local.get $new local.get $newlen i32.const 1 i32.add i32.const 4 i32.mul i32.add global.set $heap
    local.get $new)
"#;

const PRINT_STR_WAT: &str = r#"  (func $print_str (param $s i32)
    local.get $s i32.const 4 i32.add
    local.get $s i32.load
    call $print)
"#;

/// Non-negative integer to a string record. Negative inputs are out of scope.
const INT_TO_STRING_WAT: &str = r#"  (func $int_to_string (param $n i32) (result i32)
    (local $tmp i32) (local $ndigits i32) (local $res i32) (local $p i32)
    local.get $n
    i32.eqz
    if (result i32)
      global.get $heap local.set $res
      local.get $res i32.const 1 i32.store
      local.get $res i32.const 4 i32.add i32.const 48 i32.store8
      local.get $res i32.const 5 i32.add global.set $heap
      local.get $res
    else
      local.get $n local.set $tmp
      i32.const 0 local.set $ndigits
      block $b1
        loop $l1
          local.get $tmp i32.eqz br_if $b1
          local.get $ndigits i32.const 1 i32.add local.set $ndigits
          local.get $tmp i32.const 10 i32.div_s local.set $tmp
          br $l1
        end
      end
      global.get $heap local.set $res
      local.get $res local.get $ndigits i32.store
      local.get $res i32.const 4 i32.add local.get $ndigits i32.add i32.const 1 i32.sub local.set $p
      local.get $n local.set $tmp
      block $b2
        loop $l2
          local.get $tmp i32.eqz br_if $b2
          local.get $p
          local.get $tmp i32.const 10 i32.rem_s i32.const 48 i32.add
          i32.store8
          local.get $p i32.const 1 i32.sub local.set $p
          local.get $tmp i32.const 10 i32.div_s local.set $tmp
          br $l2
        end
      end
      local.get $res i32.const 4 i32.add local.get $ndigits i32.add global.set $heap
      local.get $res
    end)
"#;

fn collect_let_names(block: &Block, out: &mut Vec<String>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                out.push(name.clone());
                collect_let_names_expr(value, out);
            }
            Stmt::Assign { value, .. } => collect_let_names_expr(value, out),
            Stmt::LetTuple { names, value } => {
                for n in names {
                    out.push(n.clone());
                }
                collect_let_names_expr(value, out);
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) => collect_let_names_expr(e, out),
            Stmt::Return(None) => {}
        }
    }
}

fn collect_let_names_expr(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::If {
            then_block,
            else_block,
            ..
        } => {
            collect_let_names(then_block, out);
            if let Some(b) = else_block {
                collect_let_names(b, out);
            }
        }
        Expr::Block(b) => collect_let_names(b, out),
        Expr::While { cond, body } => {
            collect_let_names_expr(cond, out);
            collect_let_names(body, out);
        }
        Expr::For { var, iter, body } => {
            out.push(var.clone());
            out.push(format!("__forlist_{var}"));
            out.push(format!("__fori_{var}"));
            collect_let_names_expr(iter, out);
            collect_let_names(body, out);
        }
        Expr::Match { scrutinee, arms } => {
            collect_let_names_expr(scrutinee, out);
            for arm in arms {
                collect_pattern_vars(&arm.pattern, out);
                if let Some(g) = &arm.guard {
                    collect_let_names_expr(g, out);
                }
                collect_let_names_expr(&arm.body, out);
            }
        }
        _ => {}
    }
}

/// Variables bound by a pattern (these become function locals).
fn collect_pattern_vars(pat: &Pattern, out: &mut Vec<String>) {
    match pat {
        Pattern::Var(name) => out.push(name.clone()),
        Pattern::Ctor { args, .. } | Pattern::Tuple(args) => {
            for sub in args {
                collect_pattern_vars(sub, out);
            }
        }
        Pattern::List { elems, rest } => {
            for sub in elems {
                collect_pattern_vars(sub, out);
            }
            if let Some(Some(name)) = rest {
                out.push(name.clone());
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Item;
    use crate::parser::parse_module;
    use std::sync::{Arc, Mutex};
    use wasmtime::{Caller, Engine, Linker, Module as WtModule, Store};

    fn run_int(src: &str) -> i32 {
        let module = parse_module(src).expect("parse");
        let wat = compile_module(&module).expect("compile");
        let engine = Engine::default();
        let wt = WtModule::new(&engine, &wat).expect("valid wat");
        let captured = Arc::new(Mutex::new(None));
        let mut linker = Linker::new(&engine);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap("witchy", "print_int", move |n: i32| {
                *sink.lock().unwrap() = Some(n);
            })
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().take().expect("printed a value")
    }

    /// Run a float program with a capturing `print_float`.
    fn run_float(src: &str) -> f64 {
        let module = parse_module(src).expect("parse");
        let wat = compile_module(&module).expect("compile");
        let engine = Engine::default();
        let wt = WtModule::new(&engine, &wat).expect("valid wat");
        let captured = Arc::new(Mutex::new(None));
        let mut linker = Linker::new(&engine);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap("witchy", "print_float", move |x: f64| {
                *sink.lock().unwrap() = Some(x);
            })
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().take().expect("printed a float")
    }

    #[test]
    fn compiles_floats() {
        let src = r#"
            fn half(x: Float) -> Float { x / 2.0 }
            fn main() -> Float { half(7.0) + 1.5 }
        "#;
        assert_eq!(run_float(src), 5.0); // 3.5 + 1.5
    }

    /// Build a wasmtime instance whose `print` captures strings from memory.
    fn instantiate_with_print(
        wat: &str,
    ) -> (Store<()>, wasmtime::Instance, Arc<Mutex<Vec<String>>>) {
        let engine = Engine::default();
        let wt = WtModule::new(&engine, wat).expect("valid wat");
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut linker = Linker::new(&engine);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap(
                "witchy",
                "print",
                move |mut caller: Caller<'_, ()>, ptr: i32, len: i32| {
                    let mem = caller.get_export("memory").unwrap().into_memory().unwrap();
                    let data = mem.data(&caller);
                    let bytes = &data[ptr as usize..(ptr + len) as usize];
                    sink.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(bytes).into_owned());
                },
            )
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        (store, instance, captured)
    }

    fn run_str(src: &str) -> Vec<String> {
        let module = parse_module(src).expect("parse");
        let wat = compile_module(&module).expect("compile");
        let (mut store, instance, captured) = instantiate_with_print(&wat);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        let out = captured.lock().unwrap().clone();
        out
    }

    #[test]
    fn compiles_arithmetic() {
        assert_eq!(run_int("fn main() -> Int { 1 + 2 * 3 }"), 7);
    }

    #[test]
    fn full_int_program() {
        let src = r#"
            fn double(n: Int) -> Int { n * 2 }
            fn fib(n: Int) -> Int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
            fn main() -> Int { let a = double(21) let b = fib(10) a + b }
        "#;
        assert_eq!(run_int(src), 97);
    }

    #[test]
    fn compiles_int_float_conversions() {
        // int_to_float(7) / 2.0 = 3.5; float_to_int(3.5) = 3
        assert_eq!(
            run_int("fn main() -> Int { float_to_int(int_to_float(7) / 2.0) }"),
            3
        );
    }

    #[test]
    fn compiles_string_length() {
        assert_eq!(run_int(r#"fn main() -> Int { string_length("hello") }"#), 5);
    }

    #[test]
    fn compiles_while_and_mod() {
        // sum of multiples of 3 below 10: 0 + 3 + 6 + 9
        let src = r#"
            fn main() -> Int {
              var i = 0
              var total = 0
              while i < 10 {
                if i % 3 == 0 { total = total + i }
                i = i + 1
              }
              total
            }
        "#;
        assert_eq!(run_int(src), 18);
    }

    #[test]
    fn compiles_boolean_ops() {
        let src = r#"
            fn in_range(n: Int) -> Int { if n > 0 && n < 10 { 1 } else { 0 } }
            fn main() -> Int { in_range(5) + in_range(50) + in_range(-3) }
        "#;
        assert_eq!(run_int(src), 1); // 1 + 0 + 0
    }

    #[test]
    fn compiles_boolean_not() {
        assert_eq!(run_int("fn main() -> Int { if !(1 == 2) { 7 } else { 0 } }"), 7);
    }

    #[test]
    fn compiles_match_with_guards() {
        let src = r#"
            fn sign(n: Int) -> Int {
              match n {
                0 -> 0
                m if m > 0 -> 1
                _ -> 0 - 1
              }
            }
            fn main() -> Int { sign(5) + sign(-3) + sign(0) }
        "#;
        assert_eq!(run_int(src), 0); // 1 + (-1) + 0
    }

    #[test]
    fn compiles_adts_and_constructor_patterns() {
        // Constructors become heap records [tag][fields...]; ctor patterns load
        // the tag and bind fields.
        let src = r#"
            type Shape { Circle(Int) Square(Int) }
            fn area(s: Shape) -> Int {
              match s {
                Circle(r) -> 3 * r * r
                Square(w) -> w * w
              }
            }
            fn main() -> Int { area(Circle(10)) + area(Square(5)) }
        "#;
        assert_eq!(run_int(src), 325);
    }

    #[test]
    fn compiles_lists() {
        let src = r#"
            fn main() -> Int {
              let xs = [10, 20, 30]
              length(xs) + at(xs, 0) + at(xs, 2)
            }
        "#;
        assert_eq!(run_int(src), 43); // 3 + 10 + 30
    }

    #[test]
    fn compiles_nested_constructor_patterns() {
        let src = r#"
            type Point { Point(Int, Int) }
            type Shape { Dot(Point) Pair(Point, Point) }
            fn x_of(s: Shape) -> Int {
              match s {
                Dot(Point(x, _)) -> x
                Pair(Point(x, _), _) -> x
              }
            }
            fn main() -> Int {
              x_of(Dot(Point(7, 9))) + x_of(Pair(Point(3, 0), Point(0, 0)))
            }
        "#;
        assert_eq!(run_int(src), 10); // 7 + 3
    }

    #[test]
    fn compiles_string_patterns() {
        let src = r#"
            fn classify(s: String) -> Int {
              match s {
                "yes" -> 1
                "no" -> 0
                _ -> 0 - 1
              }
            }
            fn main() -> Int {
              classify("yes") + classify("no") + classify("maybe")
            }
        "#;
        assert_eq!(run_int(src), 0); // 1 + 0 + (-1)
    }

    #[test]
    fn compiles_match_and_recursion() {
        let src = r#"
            fn fact(n: Int) -> Int {
              match n {
                0 -> 1
                _ -> n * fact(n - 1)
              }
            }
            fn main() -> Int { fact(5) }
        "#;
        assert_eq!(run_int(src), 120);
    }

    #[test]
    fn compiles_inout_writeback() {
        // `inout` compiles to move-in / move-out: bump returns the updated n,
        // and the caller writes it back into x.
        let src = r#"
            fn bump(inout n: Int) { n = n + 1 }
            fn main() -> Int {
              var x = 41
              bump(x)
              bump(x)
              x
            }
        "#;
        assert_eq!(run_int(src), 43);
    }

    #[test]
    fn actor_arena_is_reset_each_message() {
        let src = r#"
            actor Counter {
              console: Console
              var count: Int = 0
              on Tick() {
                count = count + 1
                print(console, "n=" <> int_to_string(count))
              }
            }
        "#;
        let module = parse_module(src).unwrap();
        let Item::Actor(actor) = &module.items[0] else {
            panic!("expected actor");
        };
        let wat = compile_actor_module(actor).unwrap();
        let engine = Engine::default();
        let wt = WtModule::new(&engine, &wat).unwrap();
        let captured: Arc<Mutex<Vec<(i32, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let mut linker = Linker::new(&engine);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap(
                "witchy",
                "print",
                move |mut caller: Caller<'_, ()>, ptr: i32, len: i32| {
                    let mem = caller.get_export("memory").unwrap().into_memory().unwrap();
                    let data = mem.data(&caller);
                    let s = String::from_utf8_lossy(&data[ptr as usize..(ptr + len) as usize])
                        .into_owned();
                    sink.lock().unwrap().push((ptr, s));
                },
            )
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        let tick = instance.get_typed_func::<(), ()>(&mut store, "Tick").unwrap();
        tick.call(&mut store, ()).unwrap();
        tick.call(&mut store, ()).unwrap();
        let c = captured.lock().unwrap();
        // State persists (count is a global); the heap arena is reset, so the
        // second message reuses the same addresses.
        assert_eq!(c[0].1, "n=1");
        assert_eq!(c[1].1, "n=2");
        assert_eq!(c[0].0, c[1].0, "arena should be reset, reusing heap addresses");
    }

    #[test]
    fn compiles_string_concatenation() {
        let src = r#"
            fn shout(name: String) -> String { "hello, " <> name }
            fn main(console: Console) { print(console, shout("witchy")) }
        "#;
        assert_eq!(run_str(src), vec!["hello, witchy"]);
    }

    #[test]
    fn compiles_int_to_string() {
        let src = r#"fn main(console: Console) { print(console, int_to_string(12345)) }"#;
        assert_eq!(run_str(src), vec!["12345"]);
    }

    #[test]
    fn int_to_string_handles_zero() {
        let src = r#"fn main(console: Console) { print(console, int_to_string(0)) }"#;
        assert_eq!(run_str(src), vec!["0"]);
    }

    /// The headline: an actor compiled to its own WASM VM, with Int state in a
    /// global and a Console capability, handling messages run-to-completion.
    #[test]
    fn compiles_a_stateful_actor() {
        let src = r#"
            actor Counter {
              console: Console
              var count: Int = 0
              on Tick() {
                count = count + 1
                print(console, "count is " <> int_to_string(count))
              }
            }
        "#;
        let module = parse_module(src).unwrap();
        let Item::Actor(actor) = &module.items[0] else {
            panic!("expected actor");
        };
        let wat = compile_actor_module(actor).unwrap();
        let (mut store, instance, captured) = instantiate_with_print(&wat);
        let tick = instance.get_typed_func::<(), ()>(&mut store, "Tick").unwrap();
        tick.call(&mut store, ()).unwrap();
        tick.call(&mut store, ()).unwrap();
        tick.call(&mut store, ()).unwrap();
        assert_eq!(
            *captured.lock().unwrap(),
            vec!["count is 1", "count is 2", "count is 3"]
        );
    }
}
