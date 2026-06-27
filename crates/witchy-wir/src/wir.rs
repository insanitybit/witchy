//! WIR — the Witchy IR (the compiled backend's representation). See
//! `rfcs/wir-design.md`.
//!
//! **Milestone 0**: the data structures + a `WIR → WAT` pretty-printer ONLY. No
//! lowering from the AST yet, and `codegen.rs` is untouched. The point is to
//! validate the IR *shape* against real wasm — hand-write WIR for a few functions,
//! print them, and confirm the WAT assembles and runs correctly.
//!
//! WIR is a **structured, witchy-typed value tree** (à la Binaryen IR), not an
//! SSA/CFG: control flow is nested `Block`/`Loop`/`If`/`Br` nodes whose branch
//! targets are always lexically-enclosing labels, so lowering to wasm is a direct
//! structural walk with no relooper. Expressions are typed nodes (carrying a
//! `WirTy`) over the universal i64-slot value model.
//!
//! M0 uses *names* for locals/labels/funcs (the WAT printer + `wat` crate resolve
//! them); the binary encoder in M3 introduces relative branch depths and indices.

// M0: WIR is not yet wired into codegen, so most of it is exercised only by the
// round-trip tests. Lifted once the lowering (M1+) consumes it.
#![allow(dead_code)]

use std::fmt::Write;

/// The maximum closure arity the static prelude pre-declares (`$clos0..$clos4`).
/// The binary encoder reserves type indices `0..=MAX_CLOS` for these signatures
/// BEFORE any import/func type, because spliced prelude raw bodies bake those
/// `call_indirect (type $closN)` type indices. MUST equal `wir_prelude::MAX_CLOS`.
pub const MAX_CLOS: usize = 4;

/// The wasm-level representation a value is carried as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    I32,
    I64,
    F64,
}

impl Kind {
    pub fn wat(self) -> &'static str {
        match self {
            Kind::I32 => "i32",
            Kind::I64 => "i64",
            Kind::F64 => "f64",
        }
    }
}

/// The witchy-level type, retained so passes are shape-aware. (M0 carries the
/// subset the sample functions need; it grows with the lowering.)
#[derive(Debug, Clone, PartialEq)]
pub enum WirTy {
    Int,
    Float,
    Bool,
    Str,
    Nil,
    Capability,
    List(Box<WirTy>),
    /// The universal untyped i64 slot (generic/monomorphized boundaries).
    Slot,
}

impl WirTy {
    /// The wasm representation this type is carried as.
    pub fn kind(&self) -> Kind {
        match self {
            WirTy::Int => Kind::I64,
            WirTy::Float => Kind::F64,
            WirTy::Slot => Kind::I64,
            // Bool, Str (ptr), Nil, Capability (handle/placeholder), List (ptr).
            _ => Kind::I32,
        }
    }
}

/// A binary operator, abstract over the operand `Kind` (the printer picks the
/// concrete mnemonic, e.g. `i64.add` vs `f64.add`). Comparisons yield an i32 bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    // Unsigned forms — the helper layer's pointer/length arithmetic and bounds
    // checks (the high-level expression layer only ever needs the signed ops).
    DivU,
    RemU,
    ShrU,
    LtU,
    LeU,
    GtU,
    GeU,
}

impl BinOp {
    /// Does this op produce an i32 boolean regardless of operand kind?
    fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::LtU
                | BinOp::LeU
                | BinOp::GtU
                | BinOp::GeU
        )
    }

    fn mnemonic(self, k: Kind) -> String {
        let p = k.wat();
        match (self, k) {
            (BinOp::Add, _) => format!("{p}.add"),
            (BinOp::Sub, _) => format!("{p}.sub"),
            (BinOp::Mul, _) => format!("{p}.mul"),
            (BinOp::Div, Kind::F64) => "f64.div".into(),
            (BinOp::Div, _) => format!("{p}.div_s"),
            (BinOp::Rem, _) => format!("{p}.rem_s"),
            (BinOp::And, _) => format!("{p}.and"),
            (BinOp::Or, _) => format!("{p}.or"),
            (BinOp::Xor, _) => format!("{p}.xor"),
            (BinOp::Shl, _) => format!("{p}.shl"),
            (BinOp::Shr, _) => format!("{p}.shr_s"),
            (BinOp::Eq, _) => format!("{p}.eq"),
            (BinOp::Ne, _) => format!("{p}.ne"),
            (BinOp::Lt, Kind::F64) => "f64.lt".into(),
            (BinOp::Lt, _) => format!("{p}.lt_s"),
            (BinOp::Le, Kind::F64) => "f64.le".into(),
            (BinOp::Le, _) => format!("{p}.le_s"),
            (BinOp::Gt, Kind::F64) => "f64.gt".into(),
            (BinOp::Gt, _) => format!("{p}.gt_s"),
            (BinOp::Ge, Kind::F64) => "f64.ge".into(),
            (BinOp::Ge, _) => format!("{p}.ge_s"),
            (BinOp::DivU, _) => format!("{p}.div_u"),
            (BinOp::RemU, _) => format!("{p}.rem_u"),
            (BinOp::ShrU, _) => format!("{p}.shr_u"),
            (BinOp::LtU, _) => format!("{p}.lt_u"),
            (BinOp::LeU, _) => format!("{p}.le_u"),
            (BinOp::GtU, _) => format!("{p}.gt_u"),
            (BinOp::GeU, _) => format!("{p}.ge_u"),
        }
    }
}

/// A unary operator. `Not` is i32-only (`eqz`); `Neg`/`BitNot` act on the
/// operand's `Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
    BitNot,
    /// `i64 -> f64` numeric conversion (`f64.convert_i64_s`), for `math.to_float`.
    ToFloat,
    /// `f64 -> i64` saturating truncation (`i64.trunc_sat_f64_s`, non-trapping to
    /// match the interpreter's `as i64`), for `math.to_int`.
    ToInt,
    /// `f64 -> f64` square root (`f64.sqrt`), for `math.sqrt`.
    Sqrt,
}

/// The typed expression layer — witchy-semantic value nodes. Post-order emission
/// leaves exactly one value on the wasm operand stack (except void `CallHost`s).
#[derive(Debug, Clone)]
pub enum WirExpr {
    ConstI64(i64),
    ConstF64(f64),
    ConstI32(i32),
    /// A pointer to an interned `[i32 len][utf-8]` string record (the byte offset).
    StrPtr(u32),
    GetLocal(String),
    /// Read a module global by name (`global.get $g`).
    GetGlobal(String),

    /// value -> universal i64 slot.
    ToSlot(Box<WirExpr>, Kind),
    /// universal i64 slot -> value.
    FromSlot(Box<WirExpr>, Kind),

    Binary {
        op: BinOp,
        kind: Kind,
        lhs: Box<WirExpr>,
        rhs: Box<WirExpr>,
    },
    Unary {
        op: UnOp,
        kind: Kind,
        arg: Box<WirExpr>,
    },
    /// Widen/narrow a value between `Kind`s. Mirrors codegen's `kind_convert`
    /// exactly: only `i32<->i64` emit (`i64.extend_i32_s` / `i32.wrap_i64`); any
    /// other pair (incl. anything touching `f64`) is a no-op, like the original.
    Convert {
        from: Kind,
        to: Kind,
        arg: Box<WirExpr>,
    },

    /// A typed memory load: `<kind>.load offset=<offset>` from `ptr`.
    Load {
        ptr: Box<WirExpr>,
        kind: Kind,
        offset: u32,
    },
    /// `memory.size` — current linear-memory size in 64KiB pages (i32).
    MemorySize,
    /// `memory.grow` — grow memory by `pages` (i32), pushing the PREVIOUS size in
    /// pages (or `-1` on failure). Used by `$ensure`.
    MemoryGrow(Box<WirExpr>),
    /// `i32.load8_u offset=<offset>` — read one byte from `ptr`, zero-extended to
    /// i32. The byte-level read used by string helpers (`$str_eq`, `$find_byte`).
    Load8U { ptr: Box<WirExpr>, offset: u32 },

    /// A direct call to a module function by name.
    Call {
        func: String,
        args: Vec<WirExpr>,
    },
    /// A call to a host capability import by name (the authority surface).
    CallHost {
        import: String,
        args: Vec<WirExpr>,
    },
    /// An indirect call through table 0 (`call_indirect (type $clos{N})`), used
    /// for closures. `type_arity` is the closure arity `N` — it resolves to the
    /// `$clos{N}` signature `(param i32) (param i64)*N (result i64)` (one i32 env
    /// pointer, then N i64 slot args, one i64 slot result). `args` are pushed
    /// first (the env pointer then the slot args, in order); `index` (the code
    /// index loaded from the closure record) is pushed last, matching wasm's
    /// `call_indirect` operand order.
    CallIndirect {
        type_arity: usize,
        args: Vec<WirExpr>,
        index: Box<WirExpr>,
    },
    /// A statement-shaped construct sitting in value position: a value-`If`
    /// (`&&`/`||`, `if`/`else` expression) or value-`Block`/`Loop` whose node
    /// leaves exactly one value on the stack. The wrapped node carries its own
    /// `result` type.
    Control(Box<WirNode>),
    /// A sequence of nodes whose execution leaves exactly one value on the stack
    /// (the last node is value-producing). Models an expression with a statement
    /// side-effect before it — e.g. `?` stores its operand in a scratch local,
    /// then a value-`if` extracts the payload or early-returns.
    Seq(WirSeq),
}

/// A statement-level node: executes for effect and/or leaves a typed value.
/// Control flow is nested here; branch targets are lexically-enclosing labels.
#[derive(Debug, Clone)]
pub enum WirNode {
    /// Bind/rebind a local: evaluate `value`, `local.set $local`.
    SetLocal {
        local: String,
        value: WirExpr,
    },
    /// Set a module global: evaluate `value`, `global.set $global`.
    SetGlobal {
        global: String,
        value: WirExpr,
    },
    /// A typed memory store: evaluate `ptr` then `value`, `<kind>.store
    /// offset=<offset>`.
    Store {
        ptr: WirExpr,
        value: WirExpr,
        kind: Kind,
        offset: u32,
    },
    /// Call a MULTI-result function and store each result into a local. Evaluate
    /// `args`, `call $func` (which leaves `dests.len()` values on the stack), then
    /// `local.set` each `dest` — popped in REVERSE, matching wasm stack order
    /// (the last result is on top). This is how the in-place/ownership cap ABI
    /// (`$list_push_cap`/`$str_append_cap`/`$dict_*_cap`, all `(result i32 i32)`)
    /// writes its `(new_ptr, new_cap)` back into the accumulator + its cap slot.
    CallStoreMulti {
        func: String,
        args: Vec<WirExpr>,
        dests: Vec<String>,
    },
    /// `memory.copy` — copy `len` bytes from `src` to `dest` (operands pushed in
    /// the order dest, src, len). Used by `$concat` / `$list_push` / ….
    MemoryCopy {
        dest: WirExpr,
        src: WirExpr,
        len: WirExpr,
    },
    /// `memory.fill` — set `len` bytes at `dest` to the low byte of `value`.
    MemoryFill {
        dest: WirExpr,
        value: WirExpr,
        len: WirExpr,
    },
    /// `i32.store8 offset=<offset>` — write the low byte of `value` to `ptr`. The
    /// byte-level write used by `$int_to_string` and the string builders.
    Store8 {
        ptr: WirExpr,
        value: WirExpr,
        offset: u32,
    },
    /// `if (cond) then else els`. `result` is the value type (None = statement if).
    If {
        cond: WirExpr,
        then_: WirSeq,
        els: WirSeq,
        result: Option<WirTy>,
    },
    /// A labelled `block` — the target of a forward `Br`.
    Block {
        label: String,
        result: Option<WirTy>,
        body: WirSeq,
    },
    /// A `loop` — the target of a back-edge `Br` (`continue`/`while`).
    Loop {
        label: String,
        body: WirSeq,
    },
    /// `br`/`br_if` to an enclosing label.
    Br {
        target: String,
        cond: Option<WirExpr>,
    },
    /// Evaluate an expression that leaves one value, then drop it.
    Drop(WirExpr),
    /// Evaluate a void expression (e.g. a `print` host call) for its effect.
    Do(WirExpr),
    /// Evaluate an expression and LEAVE its value on the stack — the value a
    /// value-`If`/`Block` or a function tail produces (no `drop`, no `return`).
    Push(WirExpr),
    /// `return` the value (or bare).
    Return(Option<WirExpr>),
    /// `unreachable` — a trap. Used as the fall-through after an exhaustive
    /// `match`'s arms (satisfies the result type of a value-producing block).
    Unreachable,
}

pub type WirSeq = Vec<WirNode>;

/// A declared local (param or body local).
#[derive(Debug, Clone)]
pub struct WirLocal {
    pub name: String,
    pub ty: WirTy,
}

/// A host capability import (`(import "witchy" "<name>" (func ...))`).
#[derive(Debug, Clone)]
pub struct WirImport {
    pub name: String,
    pub params: Vec<Kind>,
    pub results: Vec<Kind>,
}

/// The constant initializer of a `WirGlobal` (a wasm const-expr leaf). Only the
/// shapes codegen emits are needed: an i32 or i64 immediate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GlobalInit {
    I32(i32),
    I64(i64),
}

impl GlobalInit {
    /// The `Kind` (wasm representation) of this initializer.
    pub fn kind(self) -> Kind {
        match self {
            GlobalInit::I32(_) => Kind::I32,
            GlobalInit::I64(_) => Kind::I64,
        }
    }
}

/// A module global (`$heap`, `$__witchy_reowns`, the region watermarks, …). The
/// global's `Kind` is the kind of its `init` value; mutable globals are
/// `local`-like cells the helpers read and write.
#[derive(Debug, Clone)]
pub struct WirGlobal {
    pub name: String,
    pub kind: Kind,
    pub mutable: bool,
    pub init: GlobalInit,
    /// An optional export name (`$__witchy_reowns` / `$__region_copy_bytes` are
    /// exported for the soundness nets); `None` for plain internal globals.
    pub export: Option<String>,
}

/// The single function table (table 0) and its element segment 0. The element
/// segment places `funcs[i]` at offset `i`; the table is sized to `funcs.len()`.
/// (An empty `funcs` with a present `WirTable` still emits a 0-sized table — the
/// `call_indirect (type $closN)` form references table 0 even when no lambda is
/// ever constructed.)
#[derive(Debug, Clone)]
pub struct WirTable {
    /// Function names placed in element segment 0 at offset 0, in order.
    pub funcs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct WirFunc {
    pub name: String,
    pub params: Vec<WirLocal>,
    pub ret: Vec<WirTy>,
    pub locals: Vec<WirLocal>,
    pub body: WirSeq,
    /// An externally pre-compiled function body (the already-encoded wasm body
    /// bytes — locals + instructions including the trailing `End` — exactly as
    /// `wasm_encoder::Function::raw` consumes). When `Some`, the binary encoder
    /// splices these bytes into the Code section instead of walking `body`; the
    /// func still contributes its type (from `params`/`ret`) and name→index. The
    /// WAT printer ignores `raw_body` (it has no text form), so a raw-body func
    /// is binary-encoder-only. `None` for normal node-walked funcs.
    pub raw_body: Option<Vec<u8>>,
}

/// An active data segment: bytes placed at a fixed memory offset.
#[derive(Debug, Clone)]
pub struct DataSegment {
    pub offset: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct WirModule {
    pub imports: Vec<WirImport>,
    pub funcs: Vec<WirFunc>,
    pub memory_pages: u32,
    pub data: Vec<DataSegment>,
    /// Module globals (`$heap`, `$__witchy_reowns`, region watermarks, …).
    pub globals: Vec<WirGlobal>,
    /// The function table + element segment for `call_indirect` (closures).
    /// `None` when the module constructs and calls no closures.
    pub table: Option<WirTable>,
    /// `(export name, function name)`.
    pub exports: Vec<(String, String)>,
}

/// Render a module to WAT text (`wat::parse_str`-assemblable, runtime-runnable).
/// This is the migration differential during M1–M2 and the `emit-wat` debug view
/// thereafter — never the binary path (that's `wir::encode` in M3).
pub fn to_wat(module: &WirModule) -> String {
    let mut s = String::new();
    s.push_str("(module\n");

    // Closure `$clos{N}` type declarations for any `call_indirect` in the body
    // (the WAT names a type the `call_indirect` references). One i32 env param,
    // then N i64 slot args, one i64 slot result — matching codegen's shape.
    let mut clos_arities: Vec<usize> = Vec::new();
    for f in &module.funcs {
        collect_clos_arities_seq(&f.body, &mut clos_arities);
    }
    clos_arities.sort_unstable();
    for n in &clos_arities {
        let params = format!("(param i32) {}", "(param i64) ".repeat(*n));
        let _ = writeln!(s, "  (type $clos{n} (func {params}(result i64)))");
    }

    for imp in &module.imports {
        let params: String = imp
            .params
            .iter()
            .map(|k| format!(" (param {})", k.wat()))
            .collect();
        let results: String = imp
            .results
            .iter()
            .map(|k| format!(" (result {})", k.wat()))
            .collect();
        let _ = writeln!(
            s,
            "  (import \"witchy\" \"{}\" (func ${}{params}{results}))",
            imp.name, imp.name
        );
    }

    let _ = writeln!(s, "  (memory (export \"memory\") {})", module.memory_pages);

    // Function table + element segment 0 (closures): `(table N funcref)` then
    // `(elem (i32.const 0) $f0 $f1 …)`. Matches codegen's table/elem shape.
    if let Some(table) = &module.table {
        let _ = writeln!(s, "  (table {} funcref)", table.funcs.len());
        if !table.funcs.is_empty() {
            s.push_str("  (elem (i32.const 0)");
            for name in &table.funcs {
                let _ = write!(s, " ${name}");
            }
            s.push_str(")\n");
        }
    }

    // Globals (`$heap`, `$__witchy_reowns`, region watermarks, …). A global with
    // an `export` renders the inline `(export "…")` form codegen uses.
    for g in &module.globals {
        print_global(&mut s, g);
    }

    for seg in &module.data {
        let _ = writeln!(
            s,
            "  (data (i32.const {}) \"{}\")",
            seg.offset,
            escape_data(&seg.bytes)
        );
    }

    for f in &module.funcs {
        print_func(&mut s, f);
    }

    for (export, func) in &module.exports {
        let _ = writeln!(s, "  (export \"{export}\" (func ${func}))");
    }

    s.push_str(")\n");
    s
}

/// Print a single expression as a flat instruction fragment (4-space indent,
/// matching `codegen.rs`'s emission style). A leftover from the now-retired
/// string-bridge era when WIR fragments were spliced into a WAT text stream;
/// codegen no longer round-trips through WAT.
pub fn expr_to_wat(e: &WirExpr) -> String {
    let mut s = String::new();
    print_expr(&mut s, e, 2); // depth 2 == 4 spaces, codegen's flat indent
    s
}

/// Print a sequence of statement nodes as a flat instruction fragment (the
/// statement-level analogue of `expr_to_wat`). Like `expr_to_wat`, a leftover
/// from the retired string-bridge era; codegen no longer round-trips through WAT.
pub fn seq_to_wat(seq: &WirSeq) -> String {
    let mut s = String::new();
    print_seq(&mut s, seq, 2);
    s
}

/// Print a global field: `(global $name [(export "e")] (mut? <kind>) (<init>))`.
fn print_global(s: &mut String, g: &WirGlobal) {
    let _ = write!(s, "  (global ${}", g.name);
    if let Some(e) = &g.export {
        let _ = write!(s, " (export \"{e}\")");
    }
    let ty = if g.mutable {
        format!("(mut {})", g.kind.wat())
    } else {
        g.kind.wat().to_string()
    };
    let init = match g.init {
        GlobalInit::I32(n) => format!("(i32.const {n})"),
        GlobalInit::I64(n) => format!("(i64.const {n})"),
    };
    let _ = writeln!(s, " {ty} {init})");
}

fn print_func(s: &mut String, f: &WirFunc) {
    let _ = write!(s, "  (func ${}", f.name);
    for p in &f.params {
        let _ = write!(s, " (param ${} {})", p.name, p.ty.kind().wat());
    }
    for r in &f.ret {
        let _ = write!(s, " (result {})", r.kind().wat());
    }
    s.push('\n');
    for l in &f.locals {
        let _ = writeln!(s, "    (local ${} {})", l.name, l.ty.kind().wat());
    }
    print_seq(s, &f.body, 2);
    s.push_str("  )\n");
}

fn print_seq(s: &mut String, seq: &WirSeq, depth: usize) {
    for node in seq {
        print_node(s, node, depth);
    }
}

fn indent(s: &mut String, depth: usize) {
    for _ in 0..depth {
        s.push_str("  ");
    }
}

// Control bodies print at the SAME `depth` as their `if`/`block`/`loop`/`end`
// keywords — a flat 4-space layout matching codegen's string emission exactly
// (wasm structure comes from the keywords, not indentation). This flat style is
// what makes the M1/M2 byte-identity diff against legacy WAT possible.
fn print_node(s: &mut String, node: &WirNode, depth: usize) {
    match node {
        WirNode::SetLocal { local, value } => {
            print_expr(s, value, depth);
            indent(s, depth);
            let _ = writeln!(s, "local.set ${local}");
        }
        WirNode::SetGlobal { global, value } => {
            print_expr(s, value, depth);
            indent(s, depth);
            let _ = writeln!(s, "global.set ${global}");
        }
        WirNode::Store { ptr, value, kind, offset } => {
            print_expr(s, ptr, depth);
            print_expr(s, value, depth);
            indent(s, depth);
            if *offset == 0 {
                let _ = writeln!(s, "{}.store", kind.wat());
            } else {
                let _ = writeln!(s, "{}.store offset={offset}", kind.wat());
            }
        }
        WirNode::CallStoreMulti { func, args, dests } => {
            for a in args {
                print_expr(s, a, depth);
            }
            indent(s, depth);
            let _ = writeln!(s, "call ${func}");
            for d in dests.iter().rev() {
                indent(s, depth);
                let _ = writeln!(s, "local.set ${d}");
            }
        }
        WirNode::MemoryCopy { dest, src, len } => {
            print_expr(s, dest, depth);
            print_expr(s, src, depth);
            print_expr(s, len, depth);
            indent(s, depth);
            s.push_str("memory.copy\n");
        }
        WirNode::MemoryFill { dest, value, len } => {
            print_expr(s, dest, depth);
            print_expr(s, value, depth);
            print_expr(s, len, depth);
            indent(s, depth);
            s.push_str("memory.fill\n");
        }
        WirNode::Store8 { ptr, value, offset } => {
            print_expr(s, ptr, depth);
            print_expr(s, value, depth);
            indent(s, depth);
            if *offset == 0 {
                let _ = writeln!(s, "i32.store8");
            } else {
                let _ = writeln!(s, "i32.store8 offset={offset}");
            }
        }
        WirNode::If {
            cond,
            then_,
            els,
            result,
        } => {
            print_expr(s, cond, depth);
            indent(s, depth);
            match result {
                Some(t) => {
                    let _ = writeln!(s, "if (result {})", t.kind().wat());
                }
                None => s.push_str("if\n"),
            }
            print_seq(s, then_, depth);
            indent(s, depth);
            s.push_str("else\n");
            print_seq(s, els, depth);
            indent(s, depth);
            s.push_str("end\n");
        }
        WirNode::Block {
            label,
            result,
            body,
        } => {
            indent(s, depth);
            match result {
                Some(t) => {
                    let _ = writeln!(s, "block ${label} (result {})", t.kind().wat());
                }
                None => {
                    let _ = writeln!(s, "block ${label}");
                }
            }
            print_seq(s, body, depth);
            indent(s, depth);
            s.push_str("end\n");
        }
        WirNode::Loop { label, body } => {
            indent(s, depth);
            let _ = writeln!(s, "loop ${label}");
            print_seq(s, body, depth);
            indent(s, depth);
            s.push_str("end\n");
        }
        WirNode::Br { target, cond } => match cond {
            Some(c) => {
                print_expr(s, c, depth);
                indent(s, depth);
                let _ = writeln!(s, "br_if ${target}");
            }
            None => {
                indent(s, depth);
                let _ = writeln!(s, "br ${target}");
            }
        },
        WirNode::Drop(e) => {
            print_expr(s, e, depth);
            indent(s, depth);
            s.push_str("drop\n");
        }
        WirNode::Do(e) => {
            print_expr(s, e, depth);
        }
        WirNode::Push(e) => {
            print_expr(s, e, depth);
        }
        WirNode::Return(Some(e)) => {
            print_expr(s, e, depth);
            indent(s, depth);
            s.push_str("return\n");
        }
        WirNode::Return(None) => {
            indent(s, depth);
            s.push_str("return\n");
        }
        WirNode::Unreachable => {
            indent(s, depth);
            s.push_str("unreachable\n");
        }
    }
}

fn print_expr(s: &mut String, e: &WirExpr, depth: usize) {
    match e {
        WirExpr::ConstI64(n) => emit(s, depth, &format!("i64.const {n}")),
        WirExpr::ConstI32(n) => emit(s, depth, &format!("i32.const {n}")),
        // Plain `{x}` Display, matching codegen's `Expr::Float` emission exactly
        // (the `wat` crate infers f64 from the `f64.const` mnemonic, so a
        // whole-number `5` needs no `.0`). The M3 binary encoder writes the bits
        // directly and won't use this text path.
        WirExpr::ConstF64(x) => emit(s, depth, &format!("f64.const {x}")),
        WirExpr::StrPtr(off) => emit(s, depth, &format!("i32.const {off}")),
        WirExpr::GetLocal(name) => emit(s, depth, &format!("local.get ${name}")),
        WirExpr::GetGlobal(name) => emit(s, depth, &format!("global.get ${name}")),
        WirExpr::ToSlot(inner, kind) => {
            print_expr(s, inner, depth);
            if let Some(op) = to_slot_op(*kind) {
                emit(s, depth, op);
            }
        }
        WirExpr::FromSlot(inner, kind) => {
            print_expr(s, inner, depth);
            if let Some(op) = from_slot_op(*kind) {
                emit(s, depth, op);
            }
        }
        WirExpr::Binary { op, kind, lhs, rhs } => {
            print_expr(s, lhs, depth);
            print_expr(s, rhs, depth);
            emit(s, depth, &op.mnemonic(*kind));
        }
        WirExpr::Unary { op, kind, arg } => match op {
            UnOp::Not => {
                print_expr(s, arg, depth);
                emit(s, depth, "i32.eqz");
            }
            UnOp::Neg => match kind {
                Kind::F64 => {
                    print_expr(s, arg, depth);
                    emit(s, depth, "f64.neg");
                }
                // `-x` == `0 - x`: the zero is pushed *before* the operand.
                _ => {
                    emit(s, depth, &format!("{}.const 0", kind.wat()));
                    print_expr(s, arg, depth);
                    emit(s, depth, &format!("{}.sub", kind.wat()));
                }
            },
            // `~x` == `x ^ -1` (all bits set).
            UnOp::BitNot => {
                print_expr(s, arg, depth);
                emit(s, depth, &format!("{}.const -1", kind.wat()));
                emit(s, depth, &format!("{}.xor", kind.wat()));
            }
            UnOp::ToFloat => {
                print_expr(s, arg, depth);
                emit(s, depth, "f64.convert_i64_s");
            }
            UnOp::ToInt => {
                print_expr(s, arg, depth);
                emit(s, depth, "i64.trunc_sat_f64_s");
            }
            UnOp::Sqrt => {
                print_expr(s, arg, depth);
                emit(s, depth, "f64.sqrt");
            }
        },
        WirExpr::Convert { from, to, arg } => {
            print_expr(s, arg, depth);
            match (from, to) {
                (Kind::I64, Kind::I32) => emit(s, depth, "i32.wrap_i64"),
                (Kind::I32, Kind::I64) => emit(s, depth, "i64.extend_i32_s"),
                _ => {}
            }
        }
        WirExpr::Load { ptr, kind, offset } => {
            print_expr(s, ptr, depth);
            if *offset == 0 {
                emit(s, depth, &format!("{}.load", kind.wat()));
            } else {
                emit(s, depth, &format!("{}.load offset={offset}", kind.wat()));
            }
        }
        WirExpr::MemorySize => emit(s, depth, "memory.size"),
        WirExpr::MemoryGrow(pages) => {
            print_expr(s, pages, depth);
            emit(s, depth, "memory.grow");
        }
        WirExpr::Load8U { ptr, offset } => {
            print_expr(s, ptr, depth);
            if *offset == 0 {
                emit(s, depth, "i32.load8_u");
            } else {
                emit(s, depth, &format!("i32.load8_u offset={offset}"));
            }
        }
        WirExpr::Call { func, args } => {
            for a in args {
                print_expr(s, a, depth);
            }
            emit(s, depth, &format!("call ${func}"));
        }
        WirExpr::CallHost { import, args } => {
            for a in args {
                print_expr(s, a, depth);
            }
            emit(s, depth, &format!("call ${import}"));
        }
        WirExpr::CallIndirect {
            type_arity,
            args,
            index,
        } => {
            // Args (env ptr then slot args) pushed first, then the code index,
            // then `call_indirect (type $clos{N})` — byte-identical to codegen.
            for a in args {
                print_expr(s, a, depth);
            }
            print_expr(s, index, depth);
            emit(s, depth, &format!("call_indirect (type $clos{type_arity})"));
        }
        WirExpr::Control(node) => print_node(s, node, depth),
        WirExpr::Seq(nodes) => print_seq(s, nodes, depth),
    }
}

fn emit(s: &mut String, depth: usize, instr: &str) {
    indent(s, depth);
    s.push_str(instr);
    s.push('\n');
}

/// to-slot conversion for a value of `kind` into the universal i64 slot.
fn to_slot_op(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::I64 => None, // already a slot
        // Sign-extend, matching codegen's `to_slot` / `kind_convert`: a generic
        // slot may carry a negative Int that entered via the i32 ABI. Pointers and
        // Bools have the high bit clear, so sign-extension leaves them unchanged.
        Kind::I32 => Some("i64.extend_i32_s"),
        Kind::F64 => Some("i64.reinterpret_f64"),
    }
}

/// from-slot conversion: universal i64 slot back to a value of `kind`.
fn from_slot_op(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::I64 => None,
        Kind::I32 => Some("i32.wrap_i64"),
        Kind::F64 => Some("f64.reinterpret_i64"),
    }
}

/// Collect the distinct closure arities referenced by `CallIndirect` nodes in a
/// node sequence — for the `$clos{N}` type declarations the WAT printer emits.
/// (Mirrors the binary encoder's `collect_clos_arities`.)
fn collect_clos_arities_seq(seq: &WirSeq, out: &mut Vec<usize>) {
    fn push(out: &mut Vec<usize>, n: usize) {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    fn walk_expr(e: &WirExpr, out: &mut Vec<usize>) {
        match e {
            WirExpr::CallIndirect { type_arity, args, index } => {
                push(out, *type_arity);
                for a in args {
                    walk_expr(a, out);
                }
                walk_expr(index, out);
            }
            WirExpr::ToSlot(inner, _)
            | WirExpr::FromSlot(inner, _)
            | WirExpr::Unary { arg: inner, .. }
            | WirExpr::Convert { arg: inner, .. } => walk_expr(inner, out),
            WirExpr::Binary { lhs, rhs, .. } => {
                walk_expr(lhs, out);
                walk_expr(rhs, out);
            }
            WirExpr::Load { ptr, .. } | WirExpr::Load8U { ptr, .. } => walk_expr(ptr, out),
            WirExpr::MemoryGrow(pages) => walk_expr(pages, out),
            WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
            }
            WirExpr::Control(node) => walk_node(node, out),
            WirExpr::Seq(nodes) => collect_clos_arities_seq(nodes, out),
            WirExpr::ConstI64(_)
            | WirExpr::ConstF64(_)
            | WirExpr::ConstI32(_)
            | WirExpr::StrPtr(_)
            | WirExpr::MemorySize
            | WirExpr::GetLocal(_)
            | WirExpr::GetGlobal(_) => {}
        }
    }
    fn walk_node(node: &WirNode, out: &mut Vec<usize>) {
        match node {
            WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => {
                walk_expr(value, out)
            }
            WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
                walk_expr(ptr, out);
                walk_expr(value, out);
            }
            WirNode::CallStoreMulti { args, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
            }
            WirNode::MemoryCopy { dest, src, len } => {
                walk_expr(dest, out);
                walk_expr(src, out);
                walk_expr(len, out);
            }
            WirNode::MemoryFill { dest, value, len } => {
                walk_expr(dest, out);
                walk_expr(value, out);
                walk_expr(len, out);
            }
            WirNode::If { cond, then_, els, .. } => {
                walk_expr(cond, out);
                collect_clos_arities_seq(then_, out);
                collect_clos_arities_seq(els, out);
            }
            WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
                collect_clos_arities_seq(body, out)
            }
            WirNode::Br { cond: Some(c), .. } => walk_expr(c, out),
            WirNode::Drop(e) | WirNode::Do(e) | WirNode::Push(e) | WirNode::Return(Some(e)) => {
                walk_expr(e, out)
            }
            WirNode::Br { cond: None, .. } | WirNode::Return(None) | WirNode::Unreachable => {}
        }
    }
    for node in seq {
        walk_node(node, out);
    }
}

/// Escape data-segment bytes for a WAT string literal.
fn escape_data(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        let _ = write!(s, "\\{b:02x}");
    }
    s
}

#[cfg(test)]
#[cfg(feature = "native")]
#[path = "wir_tests.rs"]
mod tests;
