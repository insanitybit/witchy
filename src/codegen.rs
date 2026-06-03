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
    ActorDef, BinOp, Block, Convention, Expr, Function, Item, MatchArm, Module, Param, Pattern,
    Stmt, Type, UnOp,
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

/// The closure-environment pointer: the implicit first parameter of every
/// lifted lambda, pointing at its `[code_index][cap0]..` heap record.
const ENV_PARAM: &str = "__witchy_env";

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
    // `Int` maps to i32 here, not i64 — a deliberate divergence from the
    // interpreter's 64-bit `Int`. Programs whose integers exceed ±2^31 will
    // wrap in compiled code where the interpreter would not; the differential
    // test suite stays within this range.
    match t {
        Type::Named(n, _) if n == "Float" => Kind::F64,
        _ => Kind::I32,
    }
}

/// A finer source-level value type than `Kind`, used where i32 alone is
/// ambiguous — e.g. `to_string` must render an Int, a Bool, and a String
/// differently even though all three are i32 at runtime. `Other` covers
/// everything not (yet) distinguished (lists, records, tuples, ...).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValType {
    Int,
    Bool,
    Float,
    Str,
    Other,
}

fn ty_to_valtype(t: &Type) -> ValType {
    match t {
        Type::Named(n, _) if n == "Int" => ValType::Int,
        Type::Named(n, _) if n == "Bool" => ValType::Bool,
        Type::Named(n, _) if n == "Float" => ValType::Float,
        Type::Named(n, _) if n == "String" => ValType::Str,
        _ => ValType::Other,
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
    /// Whether the `starts_with`/`ends_with` string helpers are needed.
    uses_starts_with: bool,
    uses_ends_with: bool,
    /// Whether the `split` helper is needed.
    uses_split: bool,
    /// Whether the `$substr` allocator is needed (split, substring).
    uses_substr: bool,
    /// Whether the `$find_byte` substring search is needed (contains, index_of).
    uses_find_byte: bool,
    /// Whether the char-indexed `index_of` wrapper (+ `$byte_to_char`) is needed.
    uses_index_of: bool,
    /// Whether the char-indexed `substring` wrapper (+ `$char_to_byte`) is needed.
    uses_substring: bool,
    /// Whether `replace` (and its `$match_at` companion) is needed.
    uses_replace: bool,
    /// Whether the lexicographic string comparison helper `$str_cmp` is needed
    /// (String `<`/`<=`/`>`/`>=`).
    uses_str_cmp: bool,
    /// Whether the Dict helpers (`$dict_new`/`$dict_insert`/`$dict_get_or`/
    /// `$dict_has` + `$key_eq`) are needed.
    uses_dict: bool,
    /// Whether the list-producing Dict ops (`$dict_keys`/`$dict_values`/
    /// `$dict_pairs`) are needed. These don't compare keys, so they need neither
    /// `$key_eq` nor `$str_eq`.
    uses_dict_iter: bool,
    /// Record type name -> ordered fields as `(name, named-type)`, where the
    /// second is the field's type name when it is a `Named` type (so nested
    /// records can be chained, `a.b.c`). For compiling `value.field`.
    record_fields: HashMap<String, Vec<(String, Option<String>)>>,
    /// Variables (params / let-bound constructors) known to hold a record of a
    /// given type, so `var.field` can resolve a field index.
    local_records: HashMap<String, String>,
    /// Variables holding a `List(Record)`, mapping to the element record type, so
    /// a `for x in list` loop variable's fields can be resolved.
    local_list_elem: HashMap<String, String>,
    /// Value type of params / let-bound locals, where known, so `to_string` can
    /// pick the right rendering. Absent = `Other`.
    local_val_types: HashMap<String, ValType>,
    /// Element value type of list-typed locals (e.g. a `let words = split(...)`
    /// is `List(String)`), so a `for x in words` loop variable's type — and thus
    /// its use as a Dict key — resolves.
    local_list_elem_valtype: HashMap<String, ValType>,
    /// Function name -> the value type it returns, so `to_string(f(...))` can be
    /// rendered. Populated from return-type annotations.
    fn_ret_valtype: HashMap<String, ValType>,
    /// Function name -> the record type it returns (when it returns one), so a
    /// `let q = f(...)` binds `q` to that record type.
    fn_ret_records: HashMap<String, String>,
    /// Function name -> the record type that is the success payload of its
    /// Result/Option return, so `let q = f(...)?` binds `q` to that record.
    fn_ret_result_record: HashMap<String, String>,
    /// Return kind of the function currently being compiled (for `return`).
    cur_fn_ret_kind: Kind,
    /// Whether the current function has any `inout` parameters.
    cur_fn_inout: bool,
    /// Lifted lambda functions, indexed by their table slot: a `fn(...) {...}`
    /// expression compiles to a function `$__lam{i}` here and evaluates to the
    /// index `i`. A `call_indirect` through the function table then invokes it.
    lambdas: Vec<String>,
    /// Closure arities for which a `(type $clos{n})` signature is needed (all
    /// i32 params, i32 result), used by `call_indirect`.
    clos_arities: HashSet<usize>,
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
            local_list_elem: HashMap::new(),
            local_val_types: HashMap::new(),
            local_list_elem_valtype: HashMap::new(),
            fn_ret_valtype: HashMap::new(),
            fn_ret_records: HashMap::new(),
            fn_ret_result_record: HashMap::new(),
            cur_fn_ret_kind: Kind::I32,
            cur_fn_inout: false,
            uses_list_push: false,
            uses_list_concat: false,
            uses_list_drop: false,
            uses_starts_with: false,
            uses_ends_with: false,
            uses_split: false,
            uses_substr: false,
            uses_find_byte: false,
            uses_index_of: false,
            uses_substring: false,
            uses_replace: false,
            uses_str_cmp: false,
            uses_dict: false,
            uses_dict_iter: false,
            lambdas: Vec::new(),
            clos_arities: HashSet::new(),
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

    /// The source-level value type of an expression, to the extent codegen can
    /// determine it. Used by `to_string`; `Other` means "not distinguished".
    fn val_type_of(&self, e: &Expr) -> ValType {
        match e {
            Expr::Int(_) => ValType::Int,
            Expr::Bool(_) => ValType::Bool,
            Expr::Float(_) => ValType::Float,
            Expr::Str(_) => ValType::Str,
            Expr::Unary { op, expr } => match op {
                UnOp::Not => ValType::Bool,
                UnOp::Neg => self.val_type_of(expr),
            },
            Expr::Binary { op, lhs, .. } => match op {
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
                | BinOp::And | BinOp::Or => ValType::Bool,
                BinOp::Concat => ValType::Str,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    self.val_type_of(lhs)
                }
            },
            Expr::Var(n) => self.local_val_types.get(n).copied().unwrap_or(ValType::Other),
            Expr::If { then_block, .. } => self.block_val_type(then_block),
            Expr::Block(b) => self.block_val_type(b),
            Expr::Match { arms, .. } => arms
                .first()
                .map(|a| self.val_type_of(&a.body))
                .unwrap_or(ValType::Other),
            Expr::Call { name, .. } => match name.as_str() {
                "int_to_string" | "to_string" | "to_upper" | "to_lower" | "trim" | "replace"
                | "substring" => ValType::Str,
                "starts_with" | "ends_with" | "contains" => ValType::Bool,
                "string_length" | "index_of" | "length" | "float_to_int" | "string_to_int" => {
                    ValType::Int
                }
                "int_to_float" | "sqrt" => ValType::Float,
                other => self.fn_ret_valtype.get(other).copied().unwrap_or(ValType::Other),
            },
            _ => ValType::Other,
        }
    }

    fn block_val_type(&self, b: &Block) -> ValType {
        match b.stmts.last() {
            Some(Stmt::Expr(e)) => self.val_type_of(e),
            _ => ValType::Other,
        }
    }

    /// The record type an expression evaluates to, where codegen can determine
    /// it locally, so a `let x = <expr>` binds `x` to that record and `x.field`
    /// resolves. Recursive: handles constructors, record-typed vars, record-
    /// returning calls, `get_or` (the default's type), `at` (a List(Record)
    /// element), `?` payloads, `update`, and the branches of if/match/block.
    fn record_type_of(&self, e: &Expr) -> Option<String> {
        match e {
            Expr::Ctor { name, .. } if self.record_fields.contains_key(name) => Some(name.clone()),
            Expr::Var(v) => self.local_records.get(v).cloned(),
            Expr::Call { name, args } => {
                if let Some(ty) = self.fn_ret_records.get(name) {
                    Some(ty.clone())
                } else if name == "get_or" {
                    args.get(2).and_then(|d| self.record_type_of(d))
                } else if name == "at" {
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

    fn block_record_type(&self, b: &Block) -> Option<String> {
        match b.stmts.last() {
            Some(Stmt::Expr(e)) => self.record_type_of(e),
            _ => None,
        }
    }

    /// The element value type of a list-producing expression, where codegen can
    /// determine it (a `split` result, a list literal, or a tracked list local),
    /// so a `for x in <iter>` loop variable's value type — and its use as a Dict
    /// key — can be resolved.
    fn elem_val_type_of(&self, iter: &Expr) -> ValType {
        match iter {
            Expr::Call { name, .. } if name == "split" => ValType::Str,
            Expr::List(items) => items
                .first()
                .map(|e| self.val_type_of(e))
                .unwrap_or(ValType::Other),
            Expr::Var(v) => self
                .local_list_elem_valtype
                .get(v)
                .copied()
                .unwrap_or(ValType::Other),
            _ => ValType::Other,
        }
    }

    /// Record the kinds of all `let`/pattern-bound locals in a body.
    fn infer_locals(&mut self, block: &Block) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    let k = self.kind_of(value);
                    self.locals.insert(name.clone(), k);
                    let vt = self.val_type_of(value);
                    self.local_val_types.insert(name.clone(), vt);
                    let evt = self.elem_val_type_of(value);
                    if evt != ValType::Other {
                        self.local_list_elem_valtype.insert(name.clone(), evt);
                    }
                    // A list literal of record constructors records its element
                    // record type, so `for x in items` and `at(items, i)` resolve
                    // fields (the same tracking params already get).
                    if let Expr::List(items) = value {
                        if let Some(Expr::Ctor { name: ctor, .. }) = items.first() {
                            if self.record_fields.contains_key(ctor) {
                                self.local_list_elem.insert(name.clone(), ctor.clone());
                            }
                        }
                    }
                    // Remember the binding's record type (if any) so `name.field`
                    // resolves — see `record_type_of` for the cases handled.
                    if let Some(ty) = self.record_type_of(value) {
                        self.local_records.insert(name.clone(), ty);
                    }
                    self.infer_locals_expr(value);
                }
                Stmt::Assign { value, .. } => self.infer_locals_expr(value),
                Stmt::LetTuple { names, value } => {
                    for n in names {
                        self.locals.insert(n.clone(), Kind::I32);
                    }
                    // Destructuring a tuple literal carries each element's value
                    // type to its binding, so e.g. `let (a, b) = (7, 8)` knows
                    // `a`/`b` are Ints (for `to_string`, Dict keys, ...).
                    if let Expr::Tuple(items) = value {
                        if items.len() == names.len() {
                            for (n, item) in names.iter().zip(items) {
                                let vt = self.val_type_of(item);
                                self.local_val_types.insert(n.clone(), vt);
                            }
                        }
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
                // The loop variable's value type is the iterated list's element
                // type, so e.g. `for w in split(...)` knows `w` is a String.
                let evt = self.elem_val_type_of(iter);
                if evt != ValType::Other {
                    self.local_val_types.insert(var.clone(), evt);
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
            || self.uses_split
            || self.uses_substr
            || self.uses_replace
            || self.uses_dict
            || self.uses_dict_iter
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
            s.push_str(ENSURE_WAT);
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
        if self.uses_starts_with {
            s.push_str(STARTS_WITH_WAT);
        }
        if self.uses_ends_with {
            s.push_str(ENDS_WITH_WAT);
        }
        // `$substr` allocates a string slice (used by `split` and `substring`).
        if self.uses_substr {
            s.push_str(SUBSTR_WAT);
        }
        // `$split` builds its result list with `$list_push` (emitted above via
        // `uses_list_push`, which the split call site also sets).
        if self.uses_split {
            s.push_str(SPLIT_WAT);
        }
        // Substring search (`contains`/`index_of`) and char-indexed slicing.
        if self.uses_find_byte {
            s.push_str(FIND_BYTE_WAT);
        }
        if self.uses_index_of {
            s.push_str(BYTE_TO_CHAR_WAT);
            s.push_str(STR_INDEX_OF_WAT);
        }
        if self.uses_substring {
            s.push_str(CHAR_TO_BYTE_WAT);
            s.push_str(STR_SUBSTRING_WAT);
        }
        if self.uses_replace {
            s.push_str(MATCH_AT_WAT);
            s.push_str(REPLACE_WAT);
        }
        // Dict helpers; `$key_eq` references `$str_eq`, which the dict call sites
        // force on (so it is emitted below via `uses_str_eq`).
        if self.uses_dict {
            s.push_str(DICT_NEW_WAT);
            s.push_str(KEY_EQ_WAT);
            s.push_str(DICT_INSERT_WAT);
            s.push_str(DICT_GET_OR_WAT);
            s.push_str(DICT_HAS_WAT);
            s.push_str(DICT_REMOVE_WAT);
        }
        if self.uses_dict_iter {
            s.push_str(DICT_KEYS_WAT);
            s.push_str(DICT_VALUES_WAT);
            s.push_str(DICT_PAIRS_WAT);
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
        if self.uses_str_cmp {
            s.push_str(STR_CMP_WAT);
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
        self.local_list_elem.clear();
        self.local_val_types.clear();
        self.local_list_elem_valtype.clear();
        for p in &f.params {
            let k = p.ty.as_ref().map(ty_kind).unwrap_or(Kind::I32);
            self.locals.insert(p.name.clone(), k);
            if let Some(t) = &p.ty {
                self.local_val_types.insert(p.name.clone(), ty_to_valtype(t));
            }
            match &p.ty {
                // A record-typed parameter lets `p.field` resolve.
                Some(Type::Named(n, _)) if self.record_fields.contains_key(n) => {
                    self.local_records.insert(p.name.clone(), n.clone());
                }
                // A `List(...)` parameter lets a `for x in p` loop var resolve:
                // record elements for field access, scalar elements (e.g.
                // String) for `to_string` and correct `==`/ordering.
                Some(Type::Named(n, args)) if n == "List" => {
                    if let Some(elem) = args.first() {
                        if let Type::Named(en, _) = elem {
                            if self.record_fields.contains_key(en) {
                                self.local_list_elem.insert(p.name.clone(), en.clone());
                            }
                        }
                        let evt = ty_to_valtype(elem);
                        if evt != ValType::Other {
                            self.local_list_elem_valtype.insert(p.name.clone(), evt);
                        }
                    }
                }
                _ => {}
            }
        }
        // Rename shadowing bindings to unique names so function-wide locals
        // don't alias (the interpreter scopes lexically; this preserves that).
        let renamed = alpha_rename(&f.body, &f.params);
        self.infer_locals(&renamed);

        let mut header = format!("  (func ${} ", f.name);
        for p in &f.params {
            header.push_str(&format!("(param ${} {}) ", p.name, wasm_ty(self.locals[&p.name])));
        }
        // Result = the normal return value, then one slot per `inout` parameter
        // (moved back out to the caller).
        let ret_kind = match &f.ret {
            Some(t) => ty_kind(t),
            None => self.block_kind(&renamed),
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
        collect_let_names(&renamed, &mut lets);
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

        let body = self.compile_block(&renamed)?;
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

    fn compile_expr(&mut self, expr: &Expr) -> Result<String, CodegenError> {
        match expr {
            Expr::Int(n) => {
                // Compiled `Int` is i32. A literal outside the signed 32-bit
                // range would silently wrap (e.g. 3_000_000_000 -> negative) or
                // fail WASM validation, diverging from the i64 interpreter — so
                // reject it explicitly.
                if *n < i32::MIN as i64 || *n > i32::MAX as i64 {
                    return cerr(format!(
                        "integer literal {n} exceeds the 32-bit range of compiled Int"
                    ));
                }
                Ok(format!("    i32.const {n}\n"))
            }
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
                // String comparison is structural / lexicographic, not by pointer.
                // Concrete String operands (per their value type) use $str_eq /
                // $str_cmp; values of unknown type (e.g. a generic `a`) fall back
                // to i32 comparison, as before.
                if matches!(
                    op,
                    BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
                ) && self.val_type_of(lhs) == ValType::Str
                {
                    let l = self.compile_expr(lhs)?;
                    let r = self.compile_expr(rhs)?;
                    match op {
                        BinOp::Eq => {
                            self.uses_str_eq = true;
                            return Ok(format!("{l}{r}    call $str_eq\n"));
                        }
                        BinOp::NotEq => {
                            self.uses_str_eq = true;
                            return Ok(format!("{l}{r}    call $str_eq\n    i32.eqz\n"));
                        }
                        _ => {
                            self.uses_str_cmp = true;
                            let cmp = match op {
                                BinOp::Lt => "i32.lt_s",
                                BinOp::LtEq => "i32.le_s",
                                BinOp::Gt => "i32.gt_s",
                                _ => "i32.ge_s", // GtEq
                            };
                            return Ok(format!("{l}{r}    call $str_cmp\n    i32.const 0\n    {cmp}\n"));
                        }
                    }
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
                // With an `else`, the `if` yields the branches' value, whose kind
                // (i32 or f64) is the result type. Without one it is used for
                // effect (Nil); yield i32 0, matching the i32 tail compile_block
                // leaves for a statement-style branch.
                let (result_ty, else_wat) = match else_block {
                    Some(eb) => (wasm_ty(self.block_kind(then_block)), self.compile_block(eb)?),
                    None => ("i32", "    i32.const 0\n".to_string()),
                };
                Ok(format!(
                    "{}    if (result {result_ty})\n{}    else\n{else_wat}    end\n",
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
                // If iterating a `List(Record)`, the loop var is that record, so
                // `x.field` in the body resolves.
                if let Expr::Var(v) = iter.as_ref() {
                    if let Some(elem) = self.local_list_elem.get(v).cloned() {
                        self.local_records.insert(var.clone(), elem);
                    }
                }
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
                // a field is `*(base + 4 + 4*index)`. The base may be any
                // record-producing expression (a variable, a nested field, an
                // `at`/`get_or` result, a constructor, an if/match, ...); its
                // type comes from `record_type_of` and its pointer from compiling
                // it directly.
                let Some(base_ty) = self.record_type_of(base) else {
                    return cerr(format!(
                        "cannot determine the record type for field access `.{field}`"
                    ));
                };
                let names = &self.record_fields[&base_ty];
                let Some(idx) = names.iter().position(|(n, _)| n == field) else {
                    return cerr(format!("record `{base_ty}` has no field `{field}`"));
                };
                let offset = 4 + 4 * idx;
                let base_wat = self.compile_expr(base)?;
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
            Expr::Lambda { params, body } => self.compile_lambda(params, body),
            Expr::List(items) => {
                // A list is a record [len][elem0..]; reuse the $mk{N} helper with
                // the length as the header slot. Slots are 4 bytes, so f64
                // elements don't fit (same limitation as tuples).
                if items.iter().any(|e| self.kind_of(e) == Kind::F64) {
                    return cerr("lists with Float elements are not compiled to WASM yet");
                }
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
                // Record/ADT fields occupy 4-byte slots, so f64 fields don't fit.
                if args.iter().any(|a| self.kind_of(a) == Kind::F64) {
                    return cerr(format!(
                        "`{name}` has a Float field; Float fields are not compiled to WASM yet"
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

    /// Compile a lambda to a uniform closure value: a heap record
    /// `[code_index][cap0]..[capN]` (built via `$mkN`, the code index as its
    /// tag). The lambda body is lifted to a function `$__lam{i}` whose first
    /// parameter is the closure pointer (`$__env`); a prologue copies each
    /// captured value out of the environment into a local. Captures are taken
    /// by value at creation time — equivalent to the interpreter for the
    /// immutable bindings that dominate, so writing back to a captured variable
    /// is rejected rather than silently diverging.
    fn compile_lambda(
        &mut self,
        params: &[Param],
        body: &Block,
    ) -> Result<String, CodegenError> {
        let scan = scan_lambda(params, body);
        let assigns_outer = scan.assigns_outer();
        if !assigns_outer.is_empty() {
            return cerr(format!(
                "a closure that assigns to a captured variable is not compiled yet (assigns `{}`)",
                assigns_outer.join("`, `")
            ));
        }
        let captures = scan.captures();
        // Resolve each capture against the *enclosing* scope (before the local
        // tables are swapped out for the lambda body).
        let mut cap_info: Vec<(String, bool, Option<String>, Option<String>)> = Vec::new();
        for c in &captures {
            let kind = self.locals.get(c).copied().unwrap_or(Kind::I32);
            if kind != Kind::I32 {
                return cerr(format!("a closure capturing a Float (`{c}`) is not compiled yet"));
            }
            cap_info.push((
                c.clone(),
                self.globals.contains(c),
                self.local_records.get(c).cloned(),
                self.local_list_elem.get(c).cloned(),
            ));
        }

        // Reserve this lambda's table slot *before* compiling the body, so any
        // nested lambdas take the following slots rather than colliding.
        let index = self.lambdas.len();
        self.lambdas.push(String::new());

        // The lambda body compiles in a fresh local scope (its params, the
        // captured locals, and any lets).
        let saved_locals = std::mem::take(&mut self.locals);
        let saved_records = std::mem::take(&mut self.local_records);
        let saved_list_elem = std::mem::take(&mut self.local_list_elem);
        let saved_val_types = std::mem::take(&mut self.local_val_types);
        let saved_list_elem_vt = std::mem::take(&mut self.local_list_elem_valtype);
        let saved_ret = self.cur_fn_ret_kind;
        let saved_inout = self.cur_fn_inout;
        self.cur_fn_inout = false;

        for p in params {
            let k = p.ty.as_ref().map(ty_kind).unwrap_or(Kind::I32);
            if k != Kind::I32 {
                self.restore_locals(saved_locals, saved_records, saved_list_elem, saved_val_types, saved_list_elem_vt, saved_ret, saved_inout);
                return cerr("non-Int lambda parameters are not compiled to WASM yet");
            }
            self.locals.insert(p.name.clone(), k);
            if let Some(t) = &p.ty {
                self.local_val_types.insert(p.name.clone(), ty_to_valtype(t));
            }
            match &p.ty {
                Some(Type::Named(n, _)) if self.record_fields.contains_key(n) => {
                    self.local_records.insert(p.name.clone(), n.clone());
                }
                Some(Type::Named(n, args)) if n == "List" => {
                    if let Some(elem) = args.first() {
                        if let Type::Named(en, _) = elem {
                            if self.record_fields.contains_key(en) {
                                self.local_list_elem.insert(p.name.clone(), en.clone());
                            }
                        }
                        let evt = ty_to_valtype(elem);
                        if evt != ValType::Other {
                            self.local_list_elem_valtype.insert(p.name.clone(), evt);
                        }
                    }
                }
                _ => {}
            }
        }
        // Captured names are locals of the lifted function; carry over their
        // record / list-element types so field and loop resolution still work.
        for (name, _, rec, list_elem) in &cap_info {
            self.locals.insert(name.clone(), Kind::I32);
            if let Some(r) = rec {
                self.local_records.insert(name.clone(), r.clone());
            }
            if let Some(e) = list_elem {
                self.local_list_elem.insert(name.clone(), e.clone());
            }
        }
        self.infer_locals(body);
        let ret_kind = self.block_kind(body);
        if ret_kind != Kind::I32 {
            self.restore_locals(saved_locals, saved_records, saved_list_elem, saved_val_types, saved_list_elem_vt, saved_ret, saved_inout);
            return cerr("non-Int lambda results are not compiled to WASM yet");
        }
        self.cur_fn_ret_kind = ret_kind;

        let mut header = format!("  (func $__lam{index} (param ${ENV_PARAM} i32) ");
        for p in params {
            header.push_str(&format!("(param ${} i32) ", p.name));
        }
        header.push_str("(result i32)\n");
        // Locals: captured values, then `let` bindings, then scratch. Captures
        // are declared even when they shadow nothing, since the prologue stores
        // into them.
        for (name, _, _, _) in &cap_info {
            header.push_str(&format!("    (local ${name} i32)\n"));
        }
        let mut lets = Vec::new();
        collect_let_names(body, &mut lets);
        lets.sort();
        lets.dedup();
        for name in &lets {
            let k = self.locals.get(name).copied().unwrap_or(Kind::I32);
            header.push_str(&format!("    (local ${name} {})\n", wasm_ty(k)));
        }
        header.push_str(&format!("    (local ${TUPLE_TMP} i32)\n"));
        header.push_str(&format!("    (local ${TRY_TMP} i32)\n"));
        header.push_str(&format!("    (local ${MATCH_TMP} i32)\n"));
        // Prologue: copy each capture out of the environment record (slot j is at
        // offset 4 + 4*j, past the code-index header).
        let mut prologue = String::new();
        for (j, (name, _, _, _)) in cap_info.iter().enumerate() {
            let offset = 4 + 4 * j;
            prologue.push_str(&format!(
                "    local.get ${ENV_PARAM}\n    i32.const {offset}\n    i32.add\n    i32.load\n    local.set ${name}\n"
            ));
        }

        let body_wat = self.compile_block(body)?;
        self.lambdas[index] = format!("{header}{prologue}{body_wat}  )\n");
        self.clos_arities.insert(params.len());

        self.restore_locals(saved_locals, saved_records, saved_list_elem, saved_val_types, saved_list_elem_vt, saved_ret, saved_inout);

        // Construction site: allocate `[code_index][cap0]..[capN]` via `$mkN`,
        // pushing the captures from the *enclosing* scope in slot order.
        let n = cap_info.len();
        self.mk_arities.insert(n);
        let mut out = format!("    i32.const {index}\n");
        for (name, is_global, _, _) in &cap_info {
            if *is_global {
                out.push_str(&format!("    global.get ${name}\n"));
            } else {
                out.push_str(&format!("    local.get ${name}\n"));
            }
        }
        out.push_str(&format!("    call $mk{n}\n"));
        Ok(out)
    }

    fn restore_locals(
        &mut self,
        locals: HashMap<String, Kind>,
        records: HashMap<String, String>,
        list_elem: HashMap<String, String>,
        val_types: HashMap<String, ValType>,
        list_elem_vt: HashMap<String, ValType>,
        ret: Kind,
        inout: bool,
    ) {
        self.locals = locals;
        self.local_records = records;
        self.local_list_elem = list_elem;
        self.local_val_types = val_types;
        self.local_list_elem_valtype = list_elem_vt;
        self.cur_fn_ret_kind = ret;
        self.cur_fn_inout = inout;
    }

    /// The `$key_eq` comparison mode for a Dict key expression: 0 for Int/Bool
    /// (i32 equality), 1 for String (`$str_eq`). Other key types are rejected.
    fn dict_key_mode(&self, key: &Expr) -> Result<u32, CodegenError> {
        match self.val_type_of(key) {
            ValType::Int | ValType::Bool => Ok(0),
            ValType::Str => Ok(1),
            ValType::Float => cerr("a Dict with Float keys is not compiled to WASM yet"),
            ValType::Other => cerr(
                "could not determine the Dict key type for WASM; use Int or String keys (annotate if needed)",
            ),
        }
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
            // to_string(x): render by the argument's compile-time value type. A
            // String passes through; an Int reuses `$int_to_string`; a Bool picks
            // an interned "true"/"false". Floats and undetermined types error
            // (rather than silently mis-rendering).
            ("to_string", 1) => match self.val_type_of(&args[0]) {
                ValType::Str => self.compile_expr(&args[0]),
                ValType::Int => {
                    self.uses_int_to_string = true;
                    let arg = self.compile_expr(&args[0])?;
                    Ok(format!("{arg}    call $int_to_string\n"))
                }
                ValType::Bool => {
                    let t = self.intern("true");
                    let f = self.intern("false");
                    let arg = self.compile_expr(&args[0])?;
                    Ok(format!(
                        "{arg}    if (result i32)\n    i32.const {t}\n    else\n    i32.const {f}\n    end\n"
                    ))
                }
                ValType::Float => {
                    cerr("to_string on a Float is not compiled to WASM yet (no float formatting)")
                }
                ValType::Other => cerr(
                    "to_string could not determine the value's type for WASM; convert it explicitly (e.g. int_to_string)",
                ),
            },
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
            // Prefix/suffix tests over the string's bytes (`[len][bytes]`).
            ("starts_with", 2) => {
                self.uses_starts_with = true;
                let s = self.compile_expr(&args[0])?;
                let p = self.compile_expr(&args[1])?;
                Ok(format!("{s}{p}    call $starts_with\n"))
            }
            ("ends_with", 2) => {
                self.uses_ends_with = true;
                let s = self.compile_expr(&args[0])?;
                let p = self.compile_expr(&args[1])?;
                Ok(format!("{s}{p}    call $ends_with\n"))
            }
            // split(text, sep) -> List(String): pieces between separators (the
            // separator dropped); an empty separator yields the whole string.
            ("split", 2) => {
                self.uses_split = true;
                self.uses_substr = true; // each piece is allocated with `$substr`
                self.uses_list_push = true; // `$split` builds its result with it
                let s = self.compile_expr(&args[0])?;
                let sep = self.compile_expr(&args[1])?;
                Ok(format!("{s}{sep}    call $split\n"))
            }
            // contains(s, sub): does `sub` occur in `s`? (UTF-8-safe byte match.)
            ("contains", 2) => {
                self.uses_find_byte = true;
                let s = self.compile_expr(&args[0])?;
                let sub = self.compile_expr(&args[1])?;
                Ok(format!("{s}{sub}    call $find_byte\n    i32.const -1\n    i32.ne\n"))
            }
            // index_of(s, sub): character index of the first occurrence, or -1.
            ("index_of", 2) => {
                self.uses_find_byte = true;
                self.uses_index_of = true;
                let s = self.compile_expr(&args[0])?;
                let sub = self.compile_expr(&args[1])?;
                Ok(format!("{s}{sub}    call $str_index_of\n"))
            }
            // substring(s, start, end): the half-open character range [start, end),
            // clamped to bounds (counted by Unicode scalar).
            ("substring", 3) => {
                self.uses_substring = true;
                self.uses_substr = true;
                let s = self.compile_expr(&args[0])?;
                let start = self.compile_expr(&args[1])?;
                let end = self.compile_expr(&args[2])?;
                Ok(format!("{s}{start}{end}    call $str_substring\n"))
            }
            // replace(s, from, to): all non-overlapping occurrences of `from`.
            ("replace", 3) => {
                self.uses_replace = true;
                let s = self.compile_expr(&args[0])?;
                let from = self.compile_expr(&args[1])?;
                let to = self.compile_expr(&args[2])?;
                Ok(format!("{s}{from}{to}    call $replace\n"))
            }
            ("to_upper", _) | ("to_lower", _) | ("trim", _) => cerr(
                "to_upper/to_lower/trim run in the interpreter; WASM string transforms are future",
            ),
            // --- Dict (immutable association map) ---
            ("dict_new", 0) => {
                self.uses_dict = true;
                self.uses_str_eq = true; // `$key_eq` references `$str_eq`
                Ok("    call $dict_new\n".to_string())
            }
            ("insert", 3) => {
                let mode = self.dict_key_mode(&args[1])?;
                self.uses_dict = true;
                self.uses_str_eq = true;
                let d = self.compile_expr(&args[0])?;
                let k = self.compile_expr(&args[1])?;
                let v = self.compile_expr(&args[2])?;
                Ok(format!("{d}{k}{v}    i32.const {mode}\n    call $dict_insert\n"))
            }
            ("get_or", 3) => {
                let mode = self.dict_key_mode(&args[1])?;
                self.uses_dict = true;
                self.uses_str_eq = true;
                let d = self.compile_expr(&args[0])?;
                let k = self.compile_expr(&args[1])?;
                let default = self.compile_expr(&args[2])?;
                Ok(format!("{d}{k}{default}    i32.const {mode}\n    call $dict_get_or\n"))
            }
            ("has", 2) => {
                let mode = self.dict_key_mode(&args[1])?;
                self.uses_dict = true;
                self.uses_str_eq = true;
                let d = self.compile_expr(&args[0])?;
                let k = self.compile_expr(&args[1])?;
                Ok(format!("{d}{k}    i32.const {mode}\n    call $dict_has\n"))
            }
            // remove(dict, k): a fresh map with `k` (and its value) dropped.
            ("remove", 2) => {
                let mode = self.dict_key_mode(&args[1])?;
                self.uses_dict = true;
                self.uses_str_eq = true;
                let d = self.compile_expr(&args[0])?;
                let k = self.compile_expr(&args[1])?;
                Ok(format!("{d}{k}    i32.const {mode}\n    call $dict_remove\n"))
            }
            // size(dict): the entry count is the map's header word.
            ("size", 1) => {
                let d = self.compile_expr(&args[0])?;
                Ok(format!("{d}    i32.load\n"))
            }
            // keys/values/pairs(dict): a fresh List in insertion order.
            ("keys", 1) => {
                self.uses_dict_iter = true;
                let d = self.compile_expr(&args[0])?;
                Ok(format!("{d}    call $dict_keys\n"))
            }
            ("values", 1) => {
                self.uses_dict_iter = true;
                let d = self.compile_expr(&args[0])?;
                Ok(format!("{d}    call $dict_values\n"))
            }
            ("pairs", 1) => {
                self.uses_dict_iter = true;
                let d = self.compile_expr(&args[0])?;
                Ok(format!("{d}    call $dict_pairs\n"))
            }
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
                // A function-valued local (a closure param/binding) holds a
                // pointer to a `[code_index][caps..]` record. Call it through the
                // table: pass the closure pointer as the environment (first
                // param), then the args, then `call_indirect` on the code index
                // loaded from the record's header.
                if self.locals.contains_key(name) {
                    let n = args.len();
                    let mut out = format!("    local.get ${name}\n");
                    for arg in args {
                        out.push_str(&self.compile_expr(arg)?);
                    }
                    out.push_str(&format!("    local.get ${name}\n    i32.load\n"));
                    out.push_str(&format!("    call_indirect (type $clos{n})\n"));
                    self.clos_arities.insert(n);
                    return Ok(out);
                }
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
                if let Some(t) = &f.ret {
                    cg.fn_ret_valtype.insert(f.name.clone(), ty_to_valtype(t));
                }
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
    // Now that record types are known, note which functions return a record, so
    // `let q = f(...)` resolves `q.field`; and which return a Result/Option whose
    // success payload is a record, so `let q = f(...)?` resolves it too.
    for item in &module.items {
        if let Item::Function(f) = item {
            if let Some(Type::Named(n, args)) = &f.ret {
                if cg.record_fields.contains_key(n) {
                    cg.fn_ret_records.insert(f.name.clone(), n.clone());
                } else if let Some(Type::Named(payload, _)) = args.first() {
                    // e.g. `Result(Account, _)` / `Option(Account)`: `?` yields it.
                    if cg.record_fields.contains_key(payload) {
                        cg.fn_ret_result_record
                            .insert(f.name.clone(), payload.clone());
                    }
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
    // Closure call signatures (env pointer + i32 args, i32 result).
    let mut arities: Vec<usize> = cg.clos_arities.iter().copied().collect();
    arities.sort_unstable();
    for n in &arities {
        // One leading param for the closure environment, then the call's args.
        let params = "(param i32) ".repeat(*n + 1);
        wat.push_str(&format!("  (type $clos{n} (func {params}(result i32)))\n"));
    }
    wat.push_str(&cg.emit_imports());
    wat.push_str("  (memory (export \"memory\") 1)\n");
    // Function table populated with the lifted lambdas; slot i holds `$__lam{i}`.
    // A function table is needed whenever a closure is *called* (`call_indirect`
    // references table 0), even when the program constructs no lambdas — e.g. an
    // imported, never-called closure-taking std function still has its body
    // compiled. The table is then empty; `elem` only lists the actual lambdas.
    if !cg.lambdas.is_empty() || !cg.clos_arities.is_empty() {
        let count = cg.lambdas.len();
        wat.push_str(&format!("  (table {count} funcref)\n"));
        if !cg.lambdas.is_empty() {
            let mut elem = String::from("  (elem (i32.const 0)");
            for i in 0..count {
                elem.push_str(&format!(" $__lam{i}"));
            }
            elem.push_str(")\n");
            wat.push_str(&elem);
        }
    }
    wat.push_str(&cg.emit_data_globals_helpers(""));
    wat.push_str(&func_wat);
    for lam in &cg.lambdas {
        wat.push_str(lam);
    }

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
            // Handler params are all Int (validated above), so `to_string` works.
            cg.local_val_types.insert(p.name.clone(), ValType::Int);
        }
        header.push('\n');
        let renamed = alpha_rename(&h.body, &h.params);
        let mut lets = Vec::new();
        collect_let_names(&renamed, &mut lets);
        lets.sort();
        lets.dedup();
        for name in &lets {
            header.push_str(&format!("    (local ${name} i32)\n"));
        }
        let body = cg.compile_block(&renamed)?;
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
    let mut arities: Vec<usize> = cg.clos_arities.iter().copied().collect();
    arities.sort_unstable();
    for n in &arities {
        // One leading param for the closure environment, then the call's args.
        let params = "(param i32) ".repeat(*n + 1);
        wat.push_str(&format!("  (type $clos{n} (func {params}(result i32)))\n"));
    }
    wat.push_str(&cg.emit_imports());
    wat.push_str("  (memory (export \"memory\") 1)\n");
    // A function table is needed whenever a closure is *called* (`call_indirect`
    // references table 0), even when the program constructs no lambdas — e.g. an
    // imported, never-called closure-taking std function still has its body
    // compiled. The table is then empty; `elem` only lists the actual lambdas.
    if !cg.lambdas.is_empty() || !cg.clos_arities.is_empty() {
        let count = cg.lambdas.len();
        wat.push_str(&format!("  (table {count} funcref)\n"));
        if !cg.lambdas.is_empty() {
            let mut elem = String::from("  (elem (i32.const 0)");
            for i in 0..count {
                elem.push_str(&format!(" $__lam{i}"));
            }
            elem.push_str(")\n");
            wat.push_str(&elem);
        }
    }
    wat.push_str(&cg.emit_data_globals_helpers(&extra_globals));
    for (header, body) in &handlers {
        // Handlers return nothing; discard the block's trailing value.
        wat.push_str(&format!("{header}{reset}{body}    drop\n  )\n"));
    }
    for lam in &cg.lambdas {
        wat.push_str(lam);
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

// str_cmp(a, b): byte-lexicographic comparison, returning a negative/zero/
// positive i32 like Rust's `String::cmp` (and, since UTF-8 preserves code-point
// order, matching the interpreter). At the first differing byte the unsigned
// difference is returned; if one string is a prefix of the other, the shorter
// compares less (the length difference).
const STR_CMP_WAT: &str = r#"  (func $str_cmp (param $a i32) (param $b i32) (result i32)
    (local $alen i32) (local $blen i32) (local $n i32) (local $i i32) (local $ca i32) (local $cb i32)
    (local.set $alen (i32.load (local.get $a)))
    (local.set $blen (i32.load (local.get $b)))
    (local.set $n (select (local.get $alen) (local.get $blen) (i32.lt_s (local.get $alen) (local.get $blen))))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $n)))
        (local.set $ca (i32.load8_u (i32.add (i32.add (local.get $a) (i32.const 4)) (local.get $i))))
        (local.set $cb (i32.load8_u (i32.add (i32.add (local.get $b) (i32.const 4)) (local.get $i))))
        (if (i32.ne (local.get $ca) (local.get $cb))
          (then (return (i32.sub (local.get $ca) (local.get $cb)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.sub (local.get $alen) (local.get $blen)))
"#;

// ensure(size): grow linear memory so the bump allocator has room for `size`
// more bytes past `$heap`. Called by every allocating helper before it bumps
// `$heap`, so compiled programs grow memory on demand instead of trapping once
// the initial page fills. Growing past the limit (true OOM) leaves the
// subsequent store to trap, as before.
const ENSURE_WAT: &str = r#"  (func $ensure (param $size i32)
    (local $need i32) (local $have i32)
    (local.set $need (i32.add (global.get $heap) (local.get $size)))
    (local.set $have (i32.mul (memory.size) (i32.const 65536)))
    (if (i32.gt_u (local.get $need) (local.get $have))
      (then (drop (memory.grow
        (i32.div_u (i32.add (i32.sub (local.get $need) (local.get $have)) (i32.const 65535)) (i32.const 65536)))))))
"#;

/// Allocation helper for an N-field constructor record `[tag][f0..f{N-1}]`.
fn mk_helper(n: usize) -> String {
    let mut params = String::from("(param $tag i32)");
    for i in 0..n {
        params.push_str(&format!(" (param $f{i} i32)"));
    }
    let size = 4 + 4 * n;
    let mut s = format!("  (func $mk{n} {params} (result i32)\n    (local $p i32)\n");
    s.push_str(&format!("    (call $ensure (i32.const {size}))\n"));
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
    (call $ensure (i32.add (i32.const 4) (i32.add (local.get $alen) (local.get $blen))))
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
    (call $ensure (i32.mul (i32.add (local.get $len) (i32.const 2)) (i32.const 4)))
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
    (call $ensure (i32.mul (i32.add (i32.add (local.get $alen) (local.get $blen)) (i32.const 1)) (i32.const 4)))
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
    (call $ensure (i32.mul (i32.add (local.get $newlen) (i32.const 1)) (i32.const 4)))
    global.get $heap local.set $new
    local.get $new local.get $newlen i32.store
    local.get $new i32.const 4 i32.add
    local.get $list i32.const 4 i32.add local.get $k i32.const 4 i32.mul i32.add
    local.get $newlen i32.const 4 i32.mul
    memory.copy
    local.get $new local.get $newlen i32.const 1 i32.add i32.const 4 i32.mul i32.add global.set $heap
    local.get $new)
"#;

// starts_with(s, p): do s's first p.len bytes equal p?
const STARTS_WITH_WAT: &str = r#"  (func $starts_with (param $s i32) (param $p i32) (result i32)
    (local $plen i32) (local $i i32)
    (local.set $plen (i32.load (local.get $p)))
    (if (i32.gt_s (local.get $plen) (i32.load (local.get $s)))
      (then (return (i32.const 0))))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $plen)))
        (if (i32.ne
              (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i)))
              (i32.load8_u (i32.add (i32.add (local.get $p) (i32.const 4)) (local.get $i))))
          (then (return (i32.const 0))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.const 1))
"#;

// substr(src, start, len): a fresh string `[len][src bytes start..start+len]`.
const SUBSTR_WAT: &str = r#"  (func $substr (param $src i32) (param $start i32) (param $len i32) (result i32)
    (local $res i32)
    (call $ensure (i32.add (i32.const 4) (local.get $len)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (memory.copy
      (i32.add (local.get $res) (i32.const 4))
      (i32.add (i32.add (local.get $src) (i32.const 4)) (local.get $start))
      (local.get $len))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;

// match_at(s, from, pos): does `from` occur in `s` starting at byte `pos`?
const MATCH_AT_WAT: &str = r#"  (func $match_at (param $s i32) (param $from i32) (param $pos i32) (result i32)
    (local $flen i32) (local $j i32)
    (local.set $flen (i32.load (local.get $from)))
    (if (i32.gt_s (i32.add (local.get $pos) (local.get $flen)) (i32.load (local.get $s)))
      (then (return (i32.const 0))))
    (local.set $j (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $j) (local.get $flen)))
        (if (i32.ne
              (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (i32.add (local.get $pos) (local.get $j))))
              (i32.load8_u (i32.add (i32.add (local.get $from) (i32.const 4)) (local.get $j))))
          (then (return (i32.const 0))))
        (local.set $j (i32.add (local.get $j) (i32.const 1)))
        (br $l)))
    (i32.const 1))
"#;

// replace(s, from, to): every non-overlapping occurrence of `from` replaced by
// `to` (Rust's str::replace). A non-empty `from` is matched on bytes (UTF-8 safe)
// in two passes — count, then fill. An empty `from` inserts `to` at every UTF-8
// character boundary (and at both ends).
const REPLACE_WAT: &str = r#"  (func $replace (param $s i32) (param $from i32) (param $to i32) (result i32)
    (local $slen i32) (local $flen i32) (local $tlen i32) (local $cnt i32)
    (local $src i32) (local $dst i32) (local $res i32) (local $reslen i32) (local $b i32) (local $clen i32)
    (local.set $slen (i32.load (local.get $s)))
    (local.set $flen (i32.load (local.get $from)))
    (local.set $tlen (i32.load (local.get $to)))
    (call $ensure (i32.add (i32.add (i32.const 4) (local.get $slen))
      (i32.mul (i32.add (local.get $slen) (i32.const 1)) (local.get $tlen))))
    (if (i32.eqz (local.get $flen))
      (then
        (local.set $res (global.get $heap))
        (local.set $dst (i32.add (local.get $res) (i32.const 4)))
        (memory.copy (local.get $dst) (i32.add (local.get $to) (i32.const 4)) (local.get $tlen))
        (local.set $dst (i32.add (local.get $dst) (local.get $tlen)))
        (local.set $src (i32.const 0))
        (block $cdone
          (loop $cl
            (br_if $cdone (i32.ge_s (local.get $src) (local.get $slen)))
            (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $src))))
            (local.set $clen
              (if (result i32) (i32.lt_u (local.get $b) (i32.const 0x80)) (then (i32.const 1))
                (else (if (result i32) (i32.lt_u (local.get $b) (i32.const 0xe0)) (then (i32.const 2))
                  (else (if (result i32) (i32.lt_u (local.get $b) (i32.const 0xf0)) (then (i32.const 3))
                    (else (i32.const 4))))))))
            (memory.copy (local.get $dst) (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $src)) (local.get $clen))
            (local.set $dst (i32.add (local.get $dst) (local.get $clen)))
            (memory.copy (local.get $dst) (i32.add (local.get $to) (i32.const 4)) (local.get $tlen))
            (local.set $dst (i32.add (local.get $dst) (local.get $tlen)))
            (local.set $src (i32.add (local.get $src) (local.get $clen)))
            (br $cl)))
        (local.set $reslen (i32.sub (local.get $dst) (i32.add (local.get $res) (i32.const 4))))
        (i32.store (local.get $res) (local.get $reslen))
        (global.set $heap (local.get $dst))
        (return (local.get $res))))
    (local.set $cnt (i32.const 0))
    (local.set $src (i32.const 0))
    (block $countdone
      (loop $cl2
        (br_if $countdone (i32.gt_s (i32.add (local.get $src) (local.get $flen)) (local.get $slen)))
        (if (call $match_at (local.get $s) (local.get $from) (local.get $src))
          (then
            (local.set $cnt (i32.add (local.get $cnt) (i32.const 1)))
            (local.set $src (i32.add (local.get $src) (local.get $flen))))
          (else
            (local.set $src (i32.add (local.get $src) (i32.const 1)))))
        (br $cl2)))
    (local.set $reslen (i32.add (local.get $slen) (i32.mul (local.get $cnt) (i32.sub (local.get $tlen) (local.get $flen)))))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $reslen))
    (local.set $dst (i32.add (local.get $res) (i32.const 4)))
    (local.set $src (i32.const 0))
    (block $filldone
      (loop $fl
        (br_if $filldone (i32.ge_s (local.get $src) (local.get $slen)))
        (if (call $match_at (local.get $s) (local.get $from) (local.get $src))
          (then
            (memory.copy (local.get $dst) (i32.add (local.get $to) (i32.const 4)) (local.get $tlen))
            (local.set $dst (i32.add (local.get $dst) (local.get $tlen)))
            (local.set $src (i32.add (local.get $src) (local.get $flen))))
          (else
            (i32.store8 (local.get $dst) (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $src))))
            (local.set $dst (i32.add (local.get $dst) (i32.const 1)))
            (local.set $src (i32.add (local.get $src) (i32.const 1)))))
        (br $fl)))
    (global.set $heap (local.get $dst))
    (local.get $res))
"#;

// A Dict is an insertion-ordered association map `[count][k0][v0][k1][v1]...]`:
// entry i has its key at `d + 4 + i*8` and value at `d + 8 + i*8`. Maps are
// immutable values, so `insert` returns a fresh copy. Keys compare by `$key_eq`,
// whose mode (0 = i32 equality for Int/Bool keys, 1 = `$str_eq` for String keys)
// the call site fixes from the key's compile-time type.
const DICT_NEW_WAT: &str = r#"  (func $dict_new (result i32)
    (local $p i32)
    (call $ensure (i32.const 4))
    (local.set $p (global.get $heap))
    (i32.store (local.get $p) (i32.const 0))
    (global.set $heap (i32.add (local.get $p) (i32.const 4)))
    (local.get $p))
"#;

const KEY_EQ_WAT: &str = r#"  (func $key_eq (param $a i32) (param $b i32) (param $mode i32) (result i32)
    (if (result i32) (i32.eqz (local.get $mode))
      (then (i32.eq (local.get $a) (local.get $b)))
      (else (call $str_eq (local.get $a) (local.get $b)))))
"#;

// insert(d, k, v): a fresh map like `d` with `k` set to `v` — the matching
// entry's value replaced, or `(k, v)` appended (count+1) if `k` is absent.
const DICT_INSERT_WAT: &str = r#"  (func $dict_insert (param $d i32) (param $k i32) (param $v i32) (param $mode i32) (result i32)
    (local $count i32) (local $i i32) (local $found i32) (local $new i32) (local $bytes i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 12) (i32.mul (local.get $count) (i32.const 8))))
    (local.set $found (i32.const -1))
    (local.set $i (i32.const 0))
    (block $fdone
      (loop $f
        (br_if $fdone (i32.ge_s (local.get $i) (local.get $count)))
        (if (call $key_eq
              (i32.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8))))
              (local.get $k) (local.get $mode))
          (then (local.set $found (local.get $i)) (br $fdone)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $f)))
    (local.set $bytes (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 8))))
    (local.set $new (global.get $heap))
    (memory.copy (local.get $new) (local.get $d) (local.get $bytes))
    (if (result i32) (i32.ge_s (local.get $found) (i32.const 0))
      (then
        (i32.store (i32.add (i32.add (local.get $new) (i32.const 8)) (i32.mul (local.get $found) (i32.const 8))) (local.get $v))
        (global.set $heap (i32.add (local.get $new) (local.get $bytes)))
        (local.get $new))
      (else
        (i32.store (local.get $new) (i32.add (local.get $count) (i32.const 1)))
        (i32.store (i32.add (local.get $new) (local.get $bytes)) (local.get $k))
        (i32.store (i32.add (i32.add (local.get $new) (local.get $bytes)) (i32.const 4)) (local.get $v))
        (global.set $heap (i32.add (i32.add (local.get $new) (local.get $bytes)) (i32.const 8)))
        (local.get $new))))
"#;

// get_or(d, k, default): the value for `k`, or `default` if absent.
const DICT_GET_OR_WAT: &str = r#"  (func $dict_get_or (param $d i32) (param $k i32) (param $default i32) (param $mode i32) (result i32)
    (local $count i32) (local $i i32)
    (local.set $count (i32.load (local.get $d)))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (if (call $key_eq
              (i32.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8))))
              (local.get $k) (local.get $mode))
          (then (return (i32.load (i32.add (i32.add (local.get $d) (i32.const 8)) (i32.mul (local.get $i) (i32.const 8)))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.get $default))
"#;

// has(d, k): whether `k` is present.
const DICT_HAS_WAT: &str = r#"  (func $dict_has (param $d i32) (param $k i32) (param $mode i32) (result i32)
    (local $count i32) (local $i i32)
    (local.set $count (i32.load (local.get $d)))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (if (call $key_eq
              (i32.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8))))
              (local.get $k) (local.get $mode))
          (then (return (i32.const 1))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.const 0))
"#;

// remove(d, k): a fresh map with the entry for `k` dropped (unchanged if
// absent). Copies every entry whose key isn't `k` into a new map.
const DICT_REMOVE_WAT: &str = r#"  (func $dict_remove (param $d i32) (param $k i32) (param $mode i32) (result i32)
    (local $count i32) (local $i i32) (local $new i32) (local $n i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 8))))
    (local.set $new (global.get $heap))
    (local.set $i (i32.const 0))
    (local.set $n (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (if (i32.eqz (call $key_eq
              (i32.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8))))
              (local.get $k) (local.get $mode)))
          (then
            (i32.store (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $n) (i32.const 8)))
              (i32.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))))
            (i32.store (i32.add (i32.add (local.get $new) (i32.const 8)) (i32.mul (local.get $n) (i32.const 8)))
              (i32.load (i32.add (i32.add (local.get $d) (i32.const 8)) (i32.mul (local.get $i) (i32.const 8)))))
            (local.set $n (i32.add (local.get $n) (i32.const 1)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.store (local.get $new) (local.get $n))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $n) (i32.const 8))))
    (local.get $new))
"#;

// keys(d) / values(d): a fresh List of the keys (or values), in insertion order.
const DICT_KEYS_WAT: &str = r#"  (func $dict_keys (param $d i32) (result i32)
    (local $count i32) (local $i i32) (local $new i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 4))))
    (local.set $new (global.get $heap))
    (i32.store (local.get $new) (local.get $count))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (i32.store
          (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $i) (i32.const 4)))
          (i32.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $count) (i32.const 4))))
    (local.get $new))
"#;

const DICT_VALUES_WAT: &str = r#"  (func $dict_values (param $d i32) (result i32)
    (local $count i32) (local $i i32) (local $new i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 4))))
    (local.set $new (global.get $heap))
    (i32.store (local.get $new) (local.get $count))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (i32.store
          (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $i) (i32.const 4)))
          (i32.load (i32.add (i32.add (local.get $d) (i32.const 8)) (i32.mul (local.get $i) (i32.const 8)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $count) (i32.const 4))))
    (local.get $new))
"#;

// pairs(d): a List of `(key, value)` 2-tuples in insertion order. Each tuple is
// the codegen layout `[0][k][v]`, so `let (k, v) = entry` destructures it.
const DICT_PAIRS_WAT: &str = r#"  (func $dict_pairs (param $d i32) (result i32)
    (local $count i32) (local $i i32) (local $list i32) (local $tup i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 16))))
    (local.set $list (global.get $heap))
    (i32.store (local.get $list) (local.get $count))
    (global.set $heap (i32.add (i32.add (local.get $list) (i32.const 4)) (i32.mul (local.get $count) (i32.const 4))))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (local.set $tup (global.get $heap))
        (i32.store (local.get $tup) (i32.const 0))
        (i32.store (i32.add (local.get $tup) (i32.const 4))
          (i32.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))))
        (i32.store (i32.add (local.get $tup) (i32.const 8))
          (i32.load (i32.add (i32.add (local.get $d) (i32.const 8)) (i32.mul (local.get $i) (i32.const 8)))))
        (global.set $heap (i32.add (local.get $tup) (i32.const 12)))
        (i32.store
          (i32.add (i32.add (local.get $list) (i32.const 4)) (i32.mul (local.get $i) (i32.const 4)))
          (local.get $tup))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.get $list))
"#;

// split(s, sep): a List(String) of the pieces between (non-overlapping)
// occurrences of `sep`, the separator dropped. Leading/trailing empty pieces
// are kept (matching Rust's str::split); an empty separator yields `[s]`. The
// result list is grown one piece at a time with `$list_push`.
const SPLIT_WAT: &str = r#"  (func $split (param $s i32) (param $sep i32) (result i32)
    (local $slen i32) (local $seplen i32) (local $result i32)
    (local $start i32) (local $i i32) (local $j i32) (local $match i32)
    (local.set $slen (i32.load (local.get $s)))
    (local.set $seplen (i32.load (local.get $sep)))
    (call $ensure (i32.const 4))
    (local.set $result (global.get $heap))
    (i32.store (local.get $result) (i32.const 0))
    (global.set $heap (i32.add (local.get $result) (i32.const 4)))
    (if (i32.eqz (local.get $seplen))
      (then (return (call $list_push (local.get $result) (local.get $s)))))
    (local.set $start (i32.const 0))
    (local.set $i (i32.const 0))
    (block $done
      (loop $scan
        (br_if $done (i32.gt_s (local.get $i) (i32.sub (local.get $slen) (local.get $seplen))))
        (local.set $match (i32.const 1))
        (local.set $j (i32.const 0))
        (block $cmpdone
          (loop $cmp
            (br_if $cmpdone (i32.ge_s (local.get $j) (local.get $seplen)))
            (if (i32.ne
                  (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (i32.add (local.get $i) (local.get $j))))
                  (i32.load8_u (i32.add (i32.add (local.get $sep) (i32.const 4)) (local.get $j))))
              (then (local.set $match (i32.const 0)) (br $cmpdone)))
            (local.set $j (i32.add (local.get $j) (i32.const 1)))
            (br $cmp)))
        (if (local.get $match)
          (then
            (local.set $result
              (call $list_push (local.get $result)
                (call $substr (local.get $s) (local.get $start) (i32.sub (local.get $i) (local.get $start)))))
            (local.set $i (i32.add (local.get $i) (local.get $seplen)))
            (local.set $start (local.get $i)))
          (else
            (local.set $i (i32.add (local.get $i) (i32.const 1)))))
        (br $scan)))
    (local.set $result
      (call $list_push (local.get $result)
        (call $substr (local.get $s) (local.get $start) (i32.sub (local.get $slen) (local.get $start)))))
    (local.get $result))
"#;

// find_byte(s, sub): byte offset of the first occurrence of `sub` in `s`, or -1.
// An empty `sub` matches at 0 (like Rust's str::find).
const FIND_BYTE_WAT: &str = r#"  (func $find_byte (param $s i32) (param $sub i32) (result i32)
    (local $slen i32) (local $sublen i32) (local $i i32) (local $j i32) (local $match i32)
    (local.set $slen (i32.load (local.get $s)))
    (local.set $sublen (i32.load (local.get $sub)))
    (if (i32.eqz (local.get $sublen)) (then (return (i32.const 0))))
    (local.set $i (i32.const 0))
    (block $done
      (loop $scan
        (br_if $done (i32.gt_s (local.get $i) (i32.sub (local.get $slen) (local.get $sublen))))
        (local.set $match (i32.const 1))
        (local.set $j (i32.const 0))
        (block $cmpdone
          (loop $cmp
            (br_if $cmpdone (i32.ge_s (local.get $j) (local.get $sublen)))
            (if (i32.ne
                  (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (i32.add (local.get $i) (local.get $j))))
                  (i32.load8_u (i32.add (i32.add (local.get $sub) (i32.const 4)) (local.get $j))))
              (then (local.set $match (i32.const 0)) (br $cmpdone)))
            (local.set $j (i32.add (local.get $j) (i32.const 1)))
            (br $cmp)))
        (if (local.get $match) (then (return (local.get $i))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $scan)))
    (i32.const -1))
"#;

// byte_to_char(s, bytelen): number of Unicode scalars in the first `bytelen`
// bytes of `s` (counts non-continuation bytes, i.e. those not 0b10xxxxxx).
const BYTE_TO_CHAR_WAT: &str = r#"  (func $byte_to_char (param $s i32) (param $bytelen i32) (result i32)
    (local $i i32) (local $count i32) (local $b i32)
    (local.set $i (i32.const 0))
    (local.set $count (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $bytelen)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (if (i32.ne (i32.and (local.get $b) (i32.const 0xc0)) (i32.const 0x80))
          (then (local.set $count (i32.add (local.get $count) (i32.const 1)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.get $count))
"#;

// index_of(s, sub): the character index of the first occurrence, or -1.
const STR_INDEX_OF_WAT: &str = r#"  (func $str_index_of (param $s i32) (param $sub i32) (result i32)
    (local $b i32)
    (local.set $b (call $find_byte (local.get $s) (local.get $sub)))
    (if (result i32) (i32.lt_s (local.get $b) (i32.const 0))
      (then (i32.const -1))
      (else (call $byte_to_char (local.get $s) (local.get $b)))))
"#;

// char_to_byte(s, n): byte offset of character `n`. A negative `n` clamps to 0
// and an `n` past the end clamps to the byte length, so callers need not bound
// it themselves. Walks one UTF-8 scalar at a time using its lead byte's length.
const CHAR_TO_BYTE_WAT: &str = r#"  (func $char_to_byte (param $s i32) (param $n i32) (result i32)
    (local $slen i32) (local $i i32) (local $count i32) (local $b i32)
    (local.set $slen (i32.load (local.get $s)))
    (local.set $i (i32.const 0))
    (local.set $count (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $slen)))
        (br_if $done (i32.ge_s (local.get $count) (local.get $n)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (local.set $i (i32.add (local.get $i)
          (if (result i32) (i32.lt_u (local.get $b) (i32.const 0x80)) (then (i32.const 1))
            (else (if (result i32) (i32.lt_u (local.get $b) (i32.const 0xe0)) (then (i32.const 2))
              (else (if (result i32) (i32.lt_u (local.get $b) (i32.const 0xf0)) (then (i32.const 3))
                (else (i32.const 4)))))))))
        (local.set $count (i32.add (local.get $count) (i32.const 1)))
        (br $l)))
    (local.get $i))
"#;

// substring(s, start, end): the [start, end) character range as a fresh string.
const STR_SUBSTRING_WAT: &str = r#"  (func $str_substring (param $s i32) (param $start i32) (param $end i32) (result i32)
    (local $lo i32) (local $hi i32)
    (local.set $lo (call $char_to_byte (local.get $s) (local.get $start)))
    (local.set $hi (call $char_to_byte (local.get $s) (local.get $end)))
    (if (result i32) (i32.ge_s (local.get $lo) (local.get $hi))
      (then (call $substr (local.get $s) (i32.const 0) (i32.const 0)))
      (else (call $substr (local.get $s) (local.get $lo) (i32.sub (local.get $hi) (local.get $lo))))))
"#;

// ends_with(s, p): do s's last p.len bytes equal p?
const ENDS_WITH_WAT: &str = r#"  (func $ends_with (param $s i32) (param $p i32) (result i32)
    (local $plen i32) (local $off i32) (local $i i32)
    (local.set $plen (i32.load (local.get $p)))
    (local.set $off (i32.sub (i32.load (local.get $s)) (local.get $plen)))
    (if (i32.lt_s (local.get $off) (i32.const 0))
      (then (return (i32.const 0))))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $plen)))
        (if (i32.ne
              (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (i32.add (local.get $off) (local.get $i))))
              (i32.load8_u (i32.add (i32.add (local.get $p) (i32.const 4)) (local.get $i))))
          (then (return (i32.const 0))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.const 1))
"#;

const PRINT_STR_WAT: &str = r#"  (func $print_str (param $s i32)
    local.get $s i32.const 4 i32.add
    local.get $s i32.load
    call $print)
"#;

// int_to_string(n): the decimal text of `n`, with a leading '-' for negatives.
// Digits are extracted from the magnitude with unsigned div/rem (so a negative
// `n` works), written back-to-front after the optional sign. 15 bytes covers
// any i32 ("-2147483648" plus the 4-byte header).
const INT_TO_STRING_WAT: &str = r#"  (func $int_to_string (param $n i32) (result i32)
    (local $mag i32) (local $t i32) (local $ndigits i32) (local $len i32) (local $res i32) (local $p i32) (local $neg i32)
    (call $ensure (i32.const 15))
    (if (result i32) (i32.eqz (local.get $n))
      (then
        (local.set $res (global.get $heap))
        (i32.store (local.get $res) (i32.const 1))
        (i32.store8 (i32.add (local.get $res) (i32.const 4)) (i32.const 48))
        (global.set $heap (i32.add (local.get $res) (i32.const 5)))
        (local.get $res))
      (else
        (local.set $neg (i32.lt_s (local.get $n) (i32.const 0)))
        (local.set $mag
          (if (result i32) (local.get $neg)
            (then (i32.sub (i32.const 0) (local.get $n)))
            (else (local.get $n))))
        (local.set $ndigits (i32.const 0))
        (local.set $t (local.get $mag))
        (block $b1
          (loop $l1
            (br_if $b1 (i32.eqz (local.get $t)))
            (local.set $ndigits (i32.add (local.get $ndigits) (i32.const 1)))
            (local.set $t (i32.div_u (local.get $t) (i32.const 10)))
            (br $l1)))
        (local.set $len (i32.add (local.get $ndigits) (local.get $neg)))
        (local.set $res (global.get $heap))
        (i32.store (local.get $res) (local.get $len))
        (if (local.get $neg)
          (then (i32.store8 (i32.add (local.get $res) (i32.const 4)) (i32.const 45))))
        (local.set $p (i32.sub (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)) (i32.const 1)))
        (local.set $t (local.get $mag))
        (block $b2
          (loop $l2
            (br_if $b2 (i32.eqz (local.get $t)))
            (i32.store8 (local.get $p) (i32.add (i32.rem_u (local.get $t) (i32.const 10)) (i32.const 48)))
            (local.set $p (i32.sub (local.get $p) (i32.const 1)))
            (local.set $t (i32.div_u (local.get $t) (i32.const 10)))
            (br $l2)))
        (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
        (local.get $res))))
"#;

/// The result of scanning a lambda body: the variables it reads and the
/// variables it assigns, alongside the names it binds internally.
#[derive(Default)]
struct LambdaScan {
    reads: HashSet<String>,
    assigns: HashSet<String>,
    bound: HashSet<String>,
}

impl LambdaScan {
    /// Variables read from the enclosing scope (the closure's captures), sorted
    /// for a deterministic capture-slot order.
    fn captures(&self) -> Vec<String> {
        let mut free: Vec<String> = self.reads.difference(&self.bound).cloned().collect();
        free.sort();
        free
    }

    /// Variables assigned that are not bound within the lambda — i.e. writes to
    /// an outer binding. By-value capture cannot propagate these back out.
    fn assigns_outer(&self) -> Vec<String> {
        let mut a: Vec<String> = self.assigns.difference(&self.bound).cloned().collect();
        a.sort();
        a
    }
}

/// Scan a lambda for captures and outer assignments. `bound` is seeded with the
/// params and grows with every internal binder (lets, loop vars, match
/// patterns, nested lambda params). The bound set is an over-approximation
/// (binders apply to the whole body), sound for these checks on all but
/// pathological shadowing.
fn scan_lambda(params: &[Param], body: &Block) -> LambdaScan {
    let mut s = LambdaScan::default();
    for p in params {
        s.bound.insert(p.name.clone());
    }
    fv_block(body, &mut s);
    s
}

fn fv_block(block: &Block, s: &mut LambdaScan) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                fv_expr(value, s);
                s.bound.insert(name.clone());
            }
            Stmt::Assign { name, value } => {
                s.assigns.insert(name.clone());
                fv_expr(value, s);
            }
            Stmt::LetTuple { names, value } => {
                fv_expr(value, s);
                for n in names {
                    s.bound.insert(n.clone());
                }
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) => fv_expr(e, s),
            Stmt::Return(None) => {}
        }
    }
}

fn fv_expr(e: &Expr, s: &mut LambdaScan) {
    match e {
        Expr::Var(n) => {
            s.reads.insert(n.clone());
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
        // A `Call` name is a function/builtin (or a closure local, caught at WASM
        // validation), never an outer value capture — only its args matter here.
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                fv_expr(x, s);
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::Spawn { args, .. } => {
            for a in args {
                fv_expr(a, s);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) => fv_expr(expr, s),
        Expr::Field { base, .. } => fv_expr(base, s),
        Expr::RecordUpdate { base, fields } => {
            fv_expr(base, s);
            for (_, v) in fields {
                fv_expr(v, s);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            fv_expr(lhs, s);
            fv_expr(rhs, s);
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            fv_expr(cond, s);
            fv_block(then_block, s);
            if let Some(b) = else_block {
                fv_block(b, s);
            }
        }
        Expr::Match { scrutinee, arms } => {
            fv_expr(scrutinee, s);
            for arm in arms {
                let mut pv = Vec::new();
                collect_pattern_vars(&arm.pattern, &mut pv);
                for v in pv {
                    s.bound.insert(v);
                }
                if let Some(g) = &arm.guard {
                    fv_expr(g, s);
                }
                fv_expr(&arm.body, s);
            }
        }
        Expr::Block(b) => fv_block(b, s),
        Expr::While { cond, body } => {
            fv_expr(cond, s);
            fv_block(body, s);
        }
        Expr::For { var, iter, body } => {
            fv_expr(iter, s);
            s.bound.insert(var.clone());
            fv_block(body, s);
        }
        Expr::Lambda { params, body } => {
            for p in params {
                s.bound.insert(p.name.clone());
            }
            fv_block(body, s);
        }
    }
}

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

/// Give shadowing bindings unique names before codegen. The compiled backend
/// declares one WASM local per distinct local *name* across a whole function,
/// so an inner binding that reuses an outer name would otherwise alias the
/// same local and clobber the outer value once the inner scope ends. This pass
/// walks the body with a scope stack and renames any binding (let, lettuple,
/// loop var, match-pattern var, lambda param) that shadows a name already in
/// scope, rewriting the references that resolve to it. Names that don't shadow
/// are left untouched, so output is unchanged for the common case.
struct Renamer {
    scopes: Vec<HashMap<String, String>>,
    counter: u32,
}

impl Renamer {
    fn new() -> Self {
        Self { scopes: Vec::new(), counter: 0 }
    }

    fn resolve(&self, name: &str) -> String {
        for s in self.scopes.iter().rev() {
            if let Some(n) = s.get(name) {
                return n.clone();
            }
        }
        name.to_string()
    }

    /// Bind `name` in the current scope, renaming it if it's already in scope.
    fn declare(&mut self, name: &str) -> String {
        let shadows = self.scopes.iter().any(|s| s.contains_key(name));
        let unique = if shadows {
            self.counter += 1;
            format!("{name}__shadow{}", self.counter)
        } else {
            name.to_string()
        };
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string(), unique.clone());
        unique
    }

    fn rename_block(&mut self, b: &mut Block) {
        self.scopes.push(HashMap::new());
        for stmt in &mut b.stmts {
            self.rename_stmt(stmt);
        }
        self.scopes.pop();
    }

    fn rename_stmt(&mut self, s: &mut Stmt) {
        match s {
            // The value is evaluated in the scope *before* the binding exists.
            Stmt::Let { name, value, .. } => {
                self.rename_expr(value);
                *name = self.declare(name);
            }
            Stmt::Assign { name, value } => {
                self.rename_expr(value);
                *name = self.resolve(name);
            }
            Stmt::LetTuple { names, value } => {
                self.rename_expr(value);
                for n in names {
                    *n = self.declare(n);
                }
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) => self.rename_expr(e),
            Stmt::Return(None) => {}
        }
    }

    fn rename_expr(&mut self, e: &mut Expr) {
        match e {
            Expr::Var(n) => *n = self.resolve(n),
            Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
            // Call / Ctor / Spawn names are functions / constructors / actors,
            // not locals — only the arguments are renamed.
            Expr::List(xs) | Expr::Tuple(xs) => {
                for x in xs {
                    self.rename_expr(x);
                }
            }
            Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::Spawn { args, .. } => {
                for a in args {
                    self.rename_expr(a);
                }
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) => self.rename_expr(expr),
            // The field name is not a local.
            Expr::Field { base, .. } => self.rename_expr(base),
            Expr::RecordUpdate { base, fields } => {
                self.rename_expr(base);
                for (_, v) in fields {
                    self.rename_expr(v);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.rename_expr(lhs);
                self.rename_expr(rhs);
            }
            Expr::If { cond, then_block, else_block } => {
                self.rename_expr(cond);
                self.rename_block(then_block);
                if let Some(b) = else_block {
                    self.rename_block(b);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.rename_expr(scrutinee);
                for arm in arms {
                    self.scopes.push(HashMap::new());
                    self.rename_pattern(&mut arm.pattern);
                    if let Some(g) = &mut arm.guard {
                        self.rename_expr(g);
                    }
                    self.rename_expr(&mut arm.body);
                    self.scopes.pop();
                }
            }
            Expr::Block(b) => self.rename_block(b),
            Expr::While { cond, body } => {
                self.rename_expr(cond);
                self.rename_block(body);
            }
            // The loop variable is bound in the same scope as the body.
            Expr::For { var, iter, body } => {
                self.rename_expr(iter);
                self.scopes.push(HashMap::new());
                *var = self.declare(var);
                for stmt in &mut body.stmts {
                    self.rename_stmt(stmt);
                }
                self.scopes.pop();
            }
            Expr::Lambda { params, body } => {
                self.scopes.push(HashMap::new());
                for p in params {
                    p.name = self.declare(&p.name);
                }
                for stmt in &mut body.stmts {
                    self.rename_stmt(stmt);
                }
                self.scopes.pop();
            }
        }
    }

    fn rename_pattern(&mut self, p: &mut Pattern) {
        match p {
            Pattern::Var(n) => *n = self.declare(n),
            Pattern::Ctor { args, .. } | Pattern::Tuple(args) => {
                for a in args {
                    self.rename_pattern(a);
                }
            }
            Pattern::List { elems, rest } => {
                for e in elems {
                    self.rename_pattern(e);
                }
                if let Some(Some(n)) = rest {
                    *n = self.declare(n);
                }
            }
            _ => {}
        }
    }
}

/// Alpha-rename a function/handler body so shadowing bindings get unique names.
/// `params` are bound in the outermost scope (never renamed themselves).
fn alpha_rename(body: &Block, params: &[Param]) -> Block {
    let mut r = Renamer::new();
    r.scopes.push(HashMap::new());
    for p in params {
        r.declare(&p.name);
    }
    let mut b = body.clone();
    r.rename_block(&mut b);
    b
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

    #[test]
    fn float_valued_if_compiles() {
        // An `if/else` whose branches are Float must yield an f64 result (the
        // `if` result type follows the branch kind, not a hardcoded i32).
        let src = r#"
            fn pick(a: Float, b: Float) -> Float { if a < b { a } else { b } }
            fn main() -> Float { pick(2.5, 7.5) + pick(9.0, 1.0) }
        "#;
        assert_eq!(run_float(src), 3.5); // min(2.5,7.5)=2.5 + min(9.0,1.0)=1.0
    }

    #[test]
    fn out_of_range_int_literal_rejected_clearly() {
        // Compiled Int is i32; a literal in [2^31, 2^32-1] would silently wrap to
        // a negative, so it's rejected rather than diverging from the i64
        // interpreter. An in-range literal still compiles.
        let big = parse_module("fn main() -> Int { 3000000000 }").expect("parse");
        let err = compile_module(&big).expect_err("out-of-range literal should be rejected");
        assert!(err.to_string().contains("32-bit range"), "unexpected error: {err}");
        assert_eq!(run_int("fn main() -> Int { 2000000000 }"), 2000000000);
    }

    #[test]
    fn float_record_field_rejected_clearly() {
        // Heap slots are 4 bytes, so an f64 field can't be stored; reject with a
        // clear message rather than a cryptic WASM type mismatch.
        let src = r#"
            type Vec2 { x: Float, y: Float }
            fn main() -> Int { let v = Vec2(1.5, 2.5)  0 }
        "#;
        let module = parse_module(src).expect("parse");
        let err = compile_module(&module).expect_err("Float field should be rejected");
        assert!(err.to_string().contains("Float field"), "unexpected error: {err}");
    }

    #[test]
    fn float_list_element_rejected_clearly() {
        let src = r#"
            fn main() -> Int { let xs = [1.5, 2.5]  0 }
        "#;
        let module = parse_module(src).expect("parse");
        let err = compile_module(&module).expect_err("Float list should be rejected");
        assert!(err.to_string().contains("Float elements"), "unexpected error: {err}");
    }

    #[test]
    fn compiles_non_capturing_closure() {
        // A non-capturing lambda passed to a higher-order function: lifted to a
        // table slot, then invoked via `call_indirect`.
        let src = r#"
            fn apply(f: fn(Int) -> Int, x: Int) -> Int { f(x) }
            fn main() -> Int { apply(fn(n: Int) { n * n }, 9) }
        "#;
        assert_eq!(run_int(src), 81);
    }

    #[test]
    fn compiles_multiple_closures() {
        // Two distinct lambdas take distinct table slots and call_indirect each.
        let src = r#"
            fn apply(f: fn(Int) -> Int, x: Int) -> Int { f(x) }
            fn main() -> Int {
                let a = apply(fn(n: Int) { n + 1 }, 10)
                let b = apply(fn(n: Int) { n * 3 }, 10)
                a + b
            }
        "#;
        assert_eq!(run_int(src), 41); // 11 + 30
    }

    #[test]
    fn closure_can_call_global_function() {
        // A lambda calling a top-level function is still non-capturing.
        let src = r#"
            fn dbl(x: Int) -> Int { x * 2 }
            fn apply(f: fn(Int) -> Int, x: Int) -> Int { f(x) }
            fn main() -> Int { apply(fn(n: Int) { dbl(n) + 1 }, 4) }
        "#;
        assert_eq!(run_int(src), 9); // dbl(4) + 1
    }

    #[test]
    fn compiles_capturing_closure() {
        // The lambda reads `k` from the enclosing scope: captured by value into
        // the closure's heap environment, then read back via the env prologue.
        let src = r#"
            fn apply(f: fn(Int) -> Int, x: Int) -> Int { f(x) }
            fn main() -> Int {
                let k = 100
                apply(fn(n: Int) { n + k }, 5)
            }
        "#;
        assert_eq!(run_int(src), 105);
    }

    #[test]
    fn closure_captures_multiple_vars() {
        // Several captures land in distinct environment slots.
        let src = r#"
            fn apply(f: fn(Int) -> Int, x: Int) -> Int { f(x) }
            fn main() -> Int {
                let a = 3
                let b = 7
                let c = 11
                apply(fn(n: Int) { n * a + b - c }, 10)
            }
        "#;
        assert_eq!(run_int(src), 26); // 10*3 + 7 - 11
    }

    #[test]
    fn closure_captures_record_field() {
        // Capturing a record value: the env carries the heap pointer, and field
        // access still resolves inside the lambda.
        let src = r#"
            type Point { x: Int, y: Int }
            fn apply(f: fn(Int) -> Int, n: Int) -> Int { f(n) }
            fn main() -> Int {
                let p = Point(4, 9)
                apply(fn(n: Int) { n + p.x * p.y }, 1)
            }
        "#;
        assert_eq!(run_int(src), 37); // 1 + 4*9
    }

    #[test]
    fn closure_assigning_captured_var_is_rejected() {
        // By-value capture cannot propagate a write back to the outer binding, so
        // assigning a captured variable is rejected rather than diverging.
        let src = r#"
            fn run(f: fn(Int) -> Int, x: Int) -> Int { f(x) }
            fn main() -> Int {
                var total = 0
                run(fn(n: Int) { total = total + n }, 5)
            }
        "#;
        let module = parse_module(src).expect("parse");
        let err = compile_module(&module).expect_err("should reject outer assignment");
        assert!(
            err.to_string().contains("assigns `total`"),
            "unexpected error: {err}"
        );
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
