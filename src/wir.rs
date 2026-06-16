//! WIR — the Witchy IR (the compiled backend's representation). See
//! `docs/wir-design.md`.
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

// --- WIR-native prelude helpers (the #35 migration target) -------------------
//
// The static prelude (`wir_prelude.rs`) ships helpers as RAW wasm bodies that bake
// import/func indices, forcing every binary module to import the full host surface
// (breaking the capability model) and keeping `wat` in the build. Re-expressing
// each helper as a `WirFunc` lets the encoder re-index by name, so a module emits
// only the helpers it reaches and imports only their authority — capability-correct
// AND wat-free. Helpers migrate one at a time; this is the first.

/// `$print_str(s: i32)` — write a witchy string (a `[i32 len][utf-8]` record at
/// `s`) to the host `print` import: `print(s + 4, [s])`. The ONLY authority it
/// needs is `print`, so a module whose only helper is this imports nothing else.
pub fn print_str_helper() -> WirFunc {
    WirFunc {
        name: "print_str".into(),
        params: vec![WirLocal { name: "s".into(), ty: WirTy::Str }],
        ret: vec![],
        locals: vec![],
        body: vec![WirNode::Do(WirExpr::CallHost {
            import: "print".into(),
            args: vec![
                // ptr = s + 4 (skip the 4-byte length header)
                WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::GetLocal("s".into())),
                    rhs: Box::new(WirExpr::ConstI32(4)),
                },
                // len = [s] (the i32 length header)
                WirExpr::Load {
                    ptr: Box::new(WirExpr::GetLocal("s".into())),
                    kind: Kind::I32,
                    offset: 0,
                },
            ],
        })],
        raw_body: None,
    }
}

/// `$ensure(size: i32)` — grow linear memory so `$heap + size` fits. Mirrors the
/// `ENSURE_WAT` helper: `need = heap + size; have = memory.size * 65536; if need
/// >u have: drop(memory.grow(ceil((need-have)/65536)))`. Uses the `$heap` global.
pub fn ensure_helper() -> WirFunc {
    let getl = |n: &str| WirExpr::GetLocal(n.into());
    let i32c = WirExpr::ConstI32;
    let bin = |op: BinOp, l: WirExpr, r: WirExpr| WirExpr::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    WirFunc {
        name: "ensure".into(),
        params: vec![WirLocal { name: "size".into(), ty: WirTy::Bool }],
        ret: vec![],
        locals: vec![
            WirLocal { name: "need".into(), ty: WirTy::Bool },
            WirLocal { name: "have".into(), ty: WirTy::Bool },
        ],
        body: vec![
            WirNode::SetLocal {
                local: "need".into(),
                value: bin(BinOp::Add, WirExpr::GetGlobal("heap".into()), getl("size")),
            },
            WirNode::SetLocal {
                local: "have".into(),
                value: bin(BinOp::Mul, WirExpr::MemorySize, i32c(65536)),
            },
            WirNode::If {
                cond: bin(BinOp::GtU, getl("need"), getl("have")),
                then_: vec![WirNode::Drop(WirExpr::MemoryGrow(Box::new(bin(
                    BinOp::DivU,
                    bin(BinOp::Add, bin(BinOp::Sub, getl("need"), getl("have")), i32c(65535)),
                    i32c(65536),
                ))))],
                els: vec![],
                result: None,
            },
        ],
        raw_body: None,
    }
}

/// The `$mk{n}` allocator for an `n`-field record/tuple/list: bump-allocate
/// `4 + 8n` bytes, store the i32 tag/length header then each i64 field slot,
/// advance `$heap`, return the pointer. Mirrors `wir_prelude::mk_helper` /
/// `codegen::mk_helper`. Calls `$ensure`; uses the `$heap` global.
pub fn mk_helper(n: usize) -> WirFunc {
    let size = 4 + 8 * n;
    let mut params = vec![WirLocal { name: "tag".into(), ty: WirTy::Bool }];
    for i in 0..n {
        params.push(WirLocal { name: format!("f{i}"), ty: WirTy::Int });
    }
    let mut body = vec![
        WirNode::Do(WirExpr::Call {
            func: "ensure".into(),
            args: vec![WirExpr::ConstI32(size as i32)],
        }),
        WirNode::SetLocal { local: "p".into(), value: WirExpr::GetGlobal("heap".into()) },
        // header: store the i32 tag at p+0.
        WirNode::Store {
            ptr: WirExpr::GetLocal("p".into()),
            value: WirExpr::GetLocal("tag".into()),
            kind: Kind::I32,
            offset: 0,
        },
    ];
    for i in 0..n {
        body.push(WirNode::Store {
            ptr: WirExpr::GetLocal("p".into()),
            value: WirExpr::GetLocal(format!("f{i}")),
            kind: Kind::I64,
            offset: (4 + 8 * i) as u32,
        });
    }
    // advance $heap past the allocation, then return the base pointer.
    body.push(WirNode::SetGlobal {
        global: "heap".into(),
        value: WirExpr::Binary {
            op: BinOp::Add,
            kind: Kind::I32,
            lhs: Box::new(WirExpr::GetLocal("p".into())),
            rhs: Box::new(WirExpr::ConstI32(size as i32)),
        },
    });
    body.push(WirNode::Push(WirExpr::GetLocal("p".into())));
    WirFunc {
        name: format!("mk{n}"),
        params,
        ret: vec![WirTy::Bool], // i32 pointer
        locals: vec![WirLocal { name: "p".into(), ty: WirTy::Bool }],
        body,
        raw_body: None,
    }
}

/// `$list_at(list: i32, i: i32) -> i64` — bounds-checked element read: trap on
/// `i < 0 || i >= len`, else load the i64 slot at `(list+4) + i*8`. Mirrors
/// `LIST_AT_WAT`. No heap/import/table.
pub fn list_at_helper() -> WirFunc {
    let getl = |n: &str| WirExpr::GetLocal(n.into());
    let i32c = WirExpr::ConstI32;
    let bin = |op: BinOp, l: WirExpr, r: WirExpr| WirExpr::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    WirFunc {
        name: "list_at".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Int], // i64 slot
        locals: vec![],
        body: vec![
            WirNode::If {
                cond: bin(
                    BinOp::Or,
                    bin(BinOp::Lt, getl("i"), i32c(0)),
                    bin(
                        BinOp::Ge,
                        getl("i"),
                        WirExpr::Load { ptr: Box::new(getl("list")), kind: Kind::I32, offset: 0 },
                    ),
                ),
                then_: vec![WirNode::Unreachable],
                els: vec![],
                result: None,
            },
            WirNode::Push(WirExpr::Load {
                ptr: Box::new(bin(
                    BinOp::Add,
                    bin(BinOp::Add, getl("list"), i32c(4)),
                    bin(BinOp::Mul, getl("i"), i32c(8)),
                )),
                kind: Kind::I64,
                offset: 0,
            }),
        ],
        raw_body: None,
    }
}

/// `$int_to_string(n: i64) -> i32` — render a signed integer to a fresh witchy
/// string (`[i32 len][ascii]`). Mirrors `INT_TO_STRING_WAT`: `0` is a fast path;
/// otherwise count digits (a div-by-10 loop), allocate `[len][digits]`, write the
/// optional `-` then the digits back-to-front (a second div/rem loop). Calls
/// `$ensure`; uses the `$heap` global; byte writes via `Store8`.
pub fn int_to_string_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let bin = |op: BinOp, k: Kind, l: E, r: E| E::Binary {
        op,
        kind: k,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    // n == 0 → the single ascii '0'.
    let then_zero = vec![
        N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
        N::Store { ptr: getl("res"), value: i32c(1), kind: Kind::I32, offset: 0 },
        N::Store8 { ptr: getl("res"), value: i32c(48), offset: 4 },
        N::SetGlobal {
            global: "heap".into(),
            value: bin(BinOp::Add, Kind::I32, getl("res"), i32c(5)),
        },
        N::Push(getl("res")),
    ];
    // Count digits of `t` (mutated to 0): `while t != 0 { ndigits++; t /= 10 }`.
    let count_loop = N::Block {
        label: "b1".into(),
        result: None,
        body: vec![N::Loop {
            label: "l1".into(),
            body: vec![
                N::Br { target: "b1".into(), cond: Some(bin(BinOp::Eq, Kind::I64, getl("t"), i64c(0))) },
                N::SetLocal {
                    local: "ndigits".into(),
                    value: bin(BinOp::Add, Kind::I32, getl("ndigits"), i32c(1)),
                },
                N::SetLocal {
                    local: "t".into(),
                    value: bin(BinOp::DivU, Kind::I64, getl("t"), i64c(10)),
                },
                N::Br { target: "l1".into(), cond: None },
            ],
        }],
    };
    // Write digits back-to-front at `p` (decremented): `store8(p, t%10 + '0')`.
    let write_loop = N::Block {
        label: "b2".into(),
        result: None,
        body: vec![N::Loop {
            label: "l2".into(),
            body: vec![
                N::Br { target: "b2".into(), cond: Some(bin(BinOp::Eq, Kind::I64, getl("t"), i64c(0))) },
                N::Store8 {
                    ptr: getl("p"),
                    value: bin(
                        BinOp::Add,
                        Kind::I32,
                        E::Convert {
                            from: Kind::I64,
                            to: Kind::I32,
                            arg: Box::new(bin(BinOp::RemU, Kind::I64, getl("t"), i64c(10))),
                        },
                        i32c(48),
                    ),
                    offset: 0,
                },
                N::SetLocal {
                    local: "p".into(),
                    value: bin(BinOp::Sub, Kind::I32, getl("p"), i32c(1)),
                },
                N::SetLocal {
                    local: "t".into(),
                    value: bin(BinOp::DivU, Kind::I64, getl("t"), i64c(10)),
                },
                N::Br { target: "l2".into(), cond: None },
            ],
        }],
    };
    let else_nonzero = vec![
        N::SetLocal { local: "neg".into(), value: bin(BinOp::Lt, Kind::I64, getl("n"), i64c(0)) },
        // mag = neg ? -n : n
        N::SetLocal {
            local: "mag".into(),
            value: E::Control(Box::new(N::If {
                cond: getl("neg"),
                then_: vec![N::Push(bin(BinOp::Sub, Kind::I64, i64c(0), getl("n")))],
                els: vec![N::Push(getl("n"))],
                result: Some(WirTy::Int),
            })),
        },
        N::SetLocal { local: "ndigits".into(), value: i32c(0) },
        N::SetLocal { local: "t".into(), value: getl("mag") },
        count_loop,
        N::SetLocal {
            local: "len".into(),
            value: bin(BinOp::Add, Kind::I32, getl("ndigits"), getl("neg")),
        },
        N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
        N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
        N::If {
            cond: getl("neg"),
            then_: vec![N::Store8 { ptr: getl("res"), value: i32c(45), offset: 4 }],
            els: vec![],
            result: None,
        },
        // p = res + 4 + len - 1 (the last digit's byte)
        N::SetLocal {
            local: "p".into(),
            value: bin(
                BinOp::Sub,
                Kind::I32,
                bin(BinOp::Add, Kind::I32, bin(BinOp::Add, Kind::I32, getl("res"), i32c(4)), getl("len")),
                i32c(1),
            ),
        },
        N::SetLocal { local: "t".into(), value: getl("mag") },
        write_loop,
        N::SetGlobal {
            global: "heap".into(),
            value: bin(BinOp::Add, Kind::I32, bin(BinOp::Add, Kind::I32, getl("res"), i32c(4)), getl("len")),
        },
        N::Push(getl("res")),
    ];
    WirFunc {
        name: "int_to_string".into(),
        params: vec![WirLocal { name: "n".into(), ty: WirTy::Int }],
        ret: vec![WirTy::Str], // i32 pointer
        locals: vec![
            WirLocal { name: "mag".into(), ty: WirTy::Int },
            WirLocal { name: "t".into(), ty: WirTy::Int },
            WirLocal { name: "ndigits".into(), ty: WirTy::Bool },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
            WirLocal { name: "p".into(), ty: WirTy::Bool },
            WirLocal { name: "neg".into(), ty: WirTy::Bool },
        ],
        body: vec![N::If {
            cond: bin(BinOp::Eq, Kind::I64, getl("n"), i64c(0)),
            then_: then_zero,
            els: else_nonzero,
            result: Some(WirTy::Str),
        }],
        raw_body: None,
    }
}

/// `$str_eq(a: i32, b: i32) -> i32` — content equality of two `[len][bytes]`
/// strings: same pointer → 1; different length → 0; else compare bytes. Mirrors
/// `STR_EQ_WAT`. Byte reads via `Load8U`; no heap/import/table.
pub fn str_eq_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let bin = |op: BinOp, l: E, r: E| E::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    let load_i32 = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    // byte a[4+i] vs b[4+i]
    let byte_at = |base: &str| E::Load8U {
        ptr: Box::new(bin(BinOp::Add, bin(BinOp::Add, getl(base), i32c(4)), getl("i"))),
        offset: 0,
    };
    WirFunc {
        name: "str_eq".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Str },
            WirLocal { name: "b".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool], // i32
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
        ],
        body: vec![
            // same pointer → equal
            N::If {
                cond: bin(BinOp::Eq, getl("a"), getl("b")),
                then_: vec![N::Return(Some(i32c(1)))],
                els: vec![],
                result: None,
            },
            // different length → not equal
            N::If {
                cond: bin(BinOp::Ne, load_i32(getl("a")), load_i32(getl("b"))),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            N::SetLocal { local: "len".into(), value: load_i32(getl("a")) },
            N::SetLocal { local: "i".into(), value: i32c(0) },
            N::Block {
                label: "done".into(),
                result: None,
                body: vec![N::Loop {
                    label: "l".into(),
                    body: vec![
                        N::Br {
                            target: "done".into(),
                            cond: Some(bin(BinOp::Ge, getl("i"), getl("len"))),
                        },
                        N::If {
                            cond: bin(BinOp::Ne, byte_at("a"), byte_at("b")),
                            then_: vec![N::Return(Some(i32c(0)))],
                            els: vec![],
                            result: None,
                        },
                        N::SetLocal {
                            local: "i".into(),
                            value: bin(BinOp::Add, getl("i"), i32c(1)),
                        },
                        N::Br { target: "l".into(), cond: None },
                    ],
                }],
            },
            N::Push(i32c(1)),
        ],
        raw_body: None,
    }
}

/// `$concat(a: i32, b: i32) -> i32` — allocate a fresh `[alen+blen][a..b..]`
/// string and `memory.copy` both operands in. Mirrors `CONCAT_WAT`. Calls
/// `$ensure`; uses the `$heap` global.
pub fn concat_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let add = |l: E, r: E| E::Binary {
        op: BinOp::Add,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    let load_i32 = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    WirFunc {
        name: "concat".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Str },
            WirLocal { name: "b".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str], // i32 pointer
        locals: vec![
            WirLocal { name: "alen".into(), ty: WirTy::Bool },
            WirLocal { name: "blen".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "alen".into(), value: load_i32(getl("a")) },
            N::SetLocal { local: "blen".into(), value: load_i32(getl("b")) },
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![add(i32c(4), add(getl("alen"), getl("blen")))],
            }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            // header: total length at res+0
            N::Store {
                ptr: getl("res"),
                value: add(getl("alen"), getl("blen")),
                kind: Kind::I32,
                offset: 0,
            },
            // copy a's bytes to res+4
            N::MemoryCopy {
                dest: add(getl("res"), i32c(4)),
                src: add(getl("a"), i32c(4)),
                len: getl("alen"),
            },
            // copy b's bytes to res+4+alen
            N::MemoryCopy {
                dest: add(add(getl("res"), i32c(4)), getl("alen")),
                src: add(getl("b"), i32c(4)),
                len: getl("blen"),
            },
            // heap = res + 4 + alen + blen
            N::SetGlobal {
                global: "heap".into(),
                value: add(add(getl("res"), i32c(4)), add(getl("alen"), getl("blen"))),
            },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$list_push_cap(list: i32, x: i64, cap: i32) -> (i32, i32)` — the in-place
/// list append: if `cap > len` mutate `list` in place (return it + `cap`), else
/// grow to a doubled buffer (return the new ptr + newcap). Increments the
/// observable `$__witchy_reowns` counter when entered with a zero cap token (the
/// re-own signal). Mirrors `LIST_PUSH_CAP_WAT`; the multi-value early `return` is
/// restructured into `ret_ptr`/`ret_cap` locals + a dual tail `Push` (WIR has no
/// multi-value `If`/`Return`). Calls `$ensure`; uses `$heap` + `$__witchy_reowns`.
pub fn list_push_cap_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b32 = |op: BinOp, l: E, r: E| E::Binary {
        op,
        kind: Kind::I32,
        lhs: Box::new(l),
        rhs: Box::new(r),
    };
    // cap == 0 → bump the re-own counter.
    let reowns_bump = N::If {
        cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("cap")) },
        then_: vec![N::SetGlobal {
            global: "__witchy_reowns".into(),
            value: E::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(E::GetGlobal("__witchy_reowns".into())),
                rhs: Box::new(i64c(1)),
            },
        }],
        els: vec![],
        result: None,
    };
    // cap > len: mutate `list` in place.
    let inplace = vec![
        N::Store {
            ptr: b32(BinOp::Add, getl("list"), b32(BinOp::Mul, getl("len"), i32c(8))),
            value: getl("x"),
            kind: Kind::I64,
            offset: 4,
        },
        N::Store {
            ptr: getl("list"),
            value: b32(BinOp::Add, getl("len"), i32c(1)),
            kind: Kind::I32,
            offset: 0,
        },
        N::SetLocal { local: "ret_ptr".into(), value: getl("list") },
        N::SetLocal { local: "ret_cap".into(), value: getl("cap") },
    ];
    // else: grow to a doubled buffer.
    let grow = vec![
        N::SetLocal {
            local: "newcap".into(),
            value: b32(BinOp::Mul, b32(BinOp::Add, getl("len"), i32c(1)), i32c(2)),
        },
        N::If {
            cond: b32(BinOp::Lt, getl("newcap"), i32c(8)),
            then_: vec![N::SetLocal { local: "newcap".into(), value: i32c(8) }],
            els: vec![],
            result: None,
        },
        N::Do(E::Call {
            func: "ensure".into(),
            args: vec![b32(BinOp::Add, i32c(4), b32(BinOp::Mul, getl("newcap"), i32c(8)))],
        }),
        N::SetLocal { local: "new".into(), value: E::GetGlobal("heap".into()) },
        N::Store {
            ptr: getl("new"),
            value: b32(BinOp::Add, getl("len"), i32c(1)),
            kind: Kind::I32,
            offset: 0,
        },
        N::MemoryCopy {
            dest: b32(BinOp::Add, getl("new"), i32c(4)),
            src: b32(BinOp::Add, getl("list"), i32c(4)),
            len: b32(BinOp::Mul, getl("len"), i32c(8)),
        },
        N::Store {
            ptr: b32(BinOp::Add, getl("new"), b32(BinOp::Mul, getl("len"), i32c(8))),
            value: getl("x"),
            kind: Kind::I64,
            offset: 4,
        },
        N::SetGlobal {
            global: "heap".into(),
            value: b32(
                BinOp::Add,
                b32(BinOp::Add, getl("new"), i32c(4)),
                b32(BinOp::Mul, getl("newcap"), i32c(8)),
            ),
        },
        N::SetLocal { local: "ret_ptr".into(), value: getl("new") },
        N::SetLocal { local: "ret_cap".into(), value: getl("newcap") },
    ];
    WirFunc {
        name: "list_push_cap".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "x".into(), ty: WirTy::Int }, // i64 slot
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool, WirTy::Bool], // (result i32 i32)
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "new".into(), ty: WirTy::Bool },
            WirLocal { name: "newcap".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_ptr".into(), ty: WirTy::Bool },
            WirLocal { name: "ret_cap".into(), ty: WirTy::Bool },
        ],
        body: vec![
            reowns_bump,
            N::SetLocal {
                local: "len".into(),
                value: E::Load { ptr: Box::new(getl("list")), kind: Kind::I32, offset: 0 },
            },
            N::If {
                cond: b32(BinOp::Gt, getl("cap"), getl("len")),
                then_: inplace,
                els: grow,
                result: None,
            },
            N::Push(getl("ret_ptr")),
            N::Push(getl("ret_cap")),
        ],
        raw_body: None,
    }
}

/// `$list_push(list: i32, x: i64) -> i32` — the non-in-place append: always
/// allocates a fresh `(len+1)`-element buffer, copies the existing elements,
/// writes `x` in the new tail slot, and returns the new pointer. (The in-place
/// optimization lives in `$list_push_cap`; this is the plain fallback used by
/// helpers like `$split`/`$str_chars` that build lists internally.)
pub fn list_push_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "list_push".into(),
        params: vec![
            WirLocal { name: "list".into(), ty: WirTy::Bool },
            WirLocal { name: "x".into(), ty: WirTy::Int }, // i64 slot
        ],
        ret: vec![WirTy::Bool], // i32 pointer
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "new".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal {
                local: "len".into(),
                value: E::Load { ptr: Box::new(getl("list")), kind: Kind::I32, offset: 0 },
            },
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, b(BinOp::Add, getl("len"), i32c(1)), i32c(8)))],
            }),
            N::SetLocal { local: "new".into(), value: E::GetGlobal("heap".into()) },
            N::Store {
                ptr: getl("new"),
                value: b(BinOp::Add, getl("len"), i32c(1)),
                kind: Kind::I32,
                offset: 0,
            },
            N::MemoryCopy {
                dest: b(BinOp::Add, getl("new"), i32c(4)),
                src: b(BinOp::Add, getl("list"), i32c(4)),
                len: b(BinOp::Mul, getl("len"), i32c(8)),
            },
            N::Store {
                ptr: b(BinOp::Add, getl("new"), b(BinOp::Mul, getl("len"), i32c(8))),
                value: getl("x"),
                kind: Kind::I64,
                offset: 4,
            },
            N::SetGlobal {
                global: "heap".into(),
                value: b(
                    BinOp::Add,
                    b(BinOp::Add, getl("new"), i32c(4)),
                    b(BinOp::Mul, b(BinOp::Add, getl("len"), i32c(1)), i32c(8)),
                ),
            },
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}

/// `$find_byte(s: i32, sub: i32) -> i32` — index of the first occurrence of
/// `sub` in `s` (byte-wise), or `-1`; empty `sub` → 0. Mirrors `FIND_BYTE_WAT`
/// (a scan loop with an inner byte-compare loop; the inner mismatch `br` lives
/// inside an `if`, which the encoder must count as a branch frame). No
/// heap/import/table.
pub fn find_byte_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), b(BinOp::Add, getl("i"), getl("j")))), offset: 4 };
    let sub_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("sub"), getl("j"))), offset: 4 };
    let cmp_loop = N::Block {
        label: "cmpdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "cmp".into(),
            body: vec![
                N::Br { target: "cmpdone".into(), cond: Some(b(BinOp::Ge, getl("j"), getl("sublen"))) },
                N::If {
                    cond: b(BinOp::Ne, s_byte, sub_byte),
                    then_: vec![setl("match", i32c(0)), N::Br { target: "cmpdone".into(), cond: None }],
                    els: vec![],
                    result: None,
                },
                setl("j", b(BinOp::Add, getl("j"), i32c(1))),
                N::Br { target: "cmp".into(), cond: None },
            ],
        }],
    };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "scan".into(),
            body: vec![
                N::Br {
                    target: "done".into(),
                    cond: Some(b(BinOp::Gt, getl("i"), b(BinOp::Sub, getl("slen"), getl("sublen")))),
                },
                setl("match", i32c(1)),
                setl("j", i32c(0)),
                cmp_loop,
                N::If { cond: getl("match"), then_: vec![N::Return(Some(getl("i")))], els: vec![], result: None },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "scan".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "find_byte".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "sub".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: ["slen", "sublen", "i", "j", "match"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("slen", load(getl("s"))),
            setl("sublen", load(getl("sub"))),
            N::If {
                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("sublen")) },
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            setl("i", i32c(0)),
            scan_loop,
            N::Push(i32c(-1)),
        ],
        raw_body: None,
    }
}

/// `$starts_with(s, p) -> i32` — 1 iff string `s` begins with prefix `p`.
/// Byte-compares `p`'s bytes against `s`'s leading bytes; bails to 0 the moment a
/// byte differs or `p` is longer than `s`.
pub fn starts_with_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("i"))), offset: 4 };
    let p_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("p"), getl("i"))), offset: 4 };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("plen"))) },
                N::If {
                    cond: b(BinOp::Ne, s_byte, p_byte),
                    then_: vec![N::Return(Some(i32c(0)))],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "starts_with".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "p".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: ["plen", "i"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("plen", load(getl("p"))),
            N::If {
                cond: b(BinOp::Gt, getl("plen"), load(getl("s"))),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            setl("i", i32c(0)),
            scan_loop,
            N::Push(i32c(1)),
        ],
        raw_body: None,
    }
}

/// `$ends_with(s, p) -> i32` — 1 iff string `s` ends with suffix `p`.
/// Like `$starts_with`, but the comparison window into `s` is shifted by
/// `off = len(s) - len(p)`; bails to 0 if `p` is longer than `s`.
pub fn ends_with_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::Load8U {
        ptr: Box::new(b(BinOp::Add, getl("s"), b(BinOp::Add, getl("off"), getl("i")))),
        offset: 4,
    };
    let p_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("p"), getl("i"))), offset: 4 };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("plen"))) },
                N::If {
                    cond: b(BinOp::Ne, s_byte, p_byte),
                    then_: vec![N::Return(Some(i32c(0)))],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "ends_with".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "p".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: ["plen", "off", "i"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("plen", load(getl("p"))),
            setl("off", b(BinOp::Sub, load(getl("s")), getl("plen"))),
            N::If {
                cond: b(BinOp::Lt, getl("off"), i32c(0)),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            setl("i", i32c(0)),
            scan_loop,
            N::Push(i32c(1)),
        ],
        raw_body: None,
    }
}

/// `$byte_to_char(s, bytelen) -> i32` — the count of UTF-8 *characters* in the
/// first `bytelen` bytes of `s`. Continuation bytes (`0b10xxxxxx`) don't start a
/// character, so they're skipped; every other byte increments the count.
pub fn byte_to_char_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("i"))), offset: 4 };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("bytelen"))) },
                setl("b", byte),
                N::If {
                    cond: b(BinOp::Ne, b(BinOp::And, getl("b"), i32c(0xc0)), i32c(0x80)),
                    then_: vec![setl("count", b(BinOp::Add, getl("count"), i32c(1)))],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "byte_to_char".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "bytelen".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["i", "count", "b"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![setl("i", i32c(0)), setl("count", i32c(0)), scan_loop, N::Push(getl("count"))],
        raw_body: None,
    }
}

/// `$str_index_of(s, sub) -> i32` — the *character* index where `sub` first
/// occurs in `s`, or -1 if absent. `$find_byte` gives the byte offset; this maps
/// it back to a character index via `$byte_to_char`.
pub fn str_index_of_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    WirFunc {
        name: "str_index_of".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "sub".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![WirLocal { name: "bidx".into(), ty: WirTy::Bool }],
        body: vec![
            setl("bidx", E::Call { func: "find_byte".into(), args: vec![getl("s"), getl("sub")] }),
            N::If {
                cond: b(BinOp::Lt, getl("bidx"), i32c(0)),
                then_: vec![N::Push(i32c(-1))],
                els: vec![N::Push(E::Call {
                    func: "byte_to_char".into(),
                    args: vec![getl("s"), getl("bidx")],
                })],
                result: Some(WirTy::Bool),
            },
        ],
        raw_body: None,
    }
}

/// `$substr(src, start, len) -> i32` — a fresh string holding `len` bytes of
/// `src` starting at *byte* offset `start`. Allocates `4 + len` via `$ensure`,
/// writes the length header, `memory.copy`s the slice, and bumps `$heap`.
pub fn substr_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let add = |l: E, r: E| E::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "substr".into(),
        params: vec![
            WirLocal { name: "src".into(), ty: WirTy::Str },
            WirLocal { name: "start".into(), ty: WirTy::Bool },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Str],
        locals: vec![WirLocal { name: "res".into(), ty: WirTy::Bool }],
        body: vec![
            N::Do(E::Call { func: "ensure".into(), args: vec![add(i32c(4), getl("len"))] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::MemoryCopy {
                dest: add(getl("res"), i32c(4)),
                src: add(add(getl("src"), i32c(4)), getl("start")),
                len: getl("len"),
            },
            N::SetGlobal {
                global: "heap".into(),
                value: add(add(getl("res"), i32c(4)), getl("len")),
            },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$char_to_byte(s, n) -> i32` — the *byte* offset of the `n`-th character of
/// `s` (the inverse of `$byte_to_char`). Walks UTF-8 sequences, stepping the byte
/// cursor by 1/2/3/4 per character based on the lead byte, until `n` chars (or
/// the end) are consumed.
pub fn char_to_byte_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    // seqlen = b<0x80 ? 1 : b<0xe0 ? 2 : b<0xf0 ? 3 : 4 — nested if-statements
    // setting the `seqlen` local (avoids an expression-level conditional).
    let seqlen = N::If {
        cond: b(BinOp::LtU, getl("b"), i32c(0x80)),
        then_: vec![setl("seqlen", i32c(1))],
        els: vec![N::If {
            cond: b(BinOp::LtU, getl("b"), i32c(0xe0)),
            then_: vec![setl("seqlen", i32c(2))],
            els: vec![N::If {
                cond: b(BinOp::LtU, getl("b"), i32c(0xf0)),
                then_: vec![setl("seqlen", i32c(3))],
                els: vec![setl("seqlen", i32c(4))],
                result: None,
            }],
            result: None,
        }],
        result: None,
    };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("slen"))) },
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("count"), getl("n"))) },
                setl("b", E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("i"))), offset: 4 }),
                seqlen,
                setl("i", b(BinOp::Add, getl("i"), getl("seqlen"))),
                setl("count", b(BinOp::Add, getl("count"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "char_to_byte".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "n".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["slen", "i", "count", "b", "seqlen"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("slen", load(getl("s"))),
            setl("i", i32c(0)),
            setl("count", i32c(0)),
            scan_loop,
            N::Push(getl("i")),
        ],
        raw_body: None,
    }
}

/// `$str_substring(s, start, end) -> i32` — the substring of `s` between the
/// *character* indices `start` and `end`. Maps both ends to byte offsets via
/// `$char_to_byte`, then `$substr`s the byte slice; an empty slice when the
/// bounds cross.
pub fn str_substring_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let c2b = |idx: &str| E::Call { func: "char_to_byte".into(), args: vec![getl("s"), getl(idx)] };
    WirFunc {
        name: "str_substring".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "start".into(), ty: WirTy::Bool },
            WirLocal { name: "end".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "lo".into(), ty: WirTy::Bool },
            WirLocal { name: "hi".into(), ty: WirTy::Bool },
        ],
        body: vec![
            setl("lo", c2b("start")),
            setl("hi", c2b("end")),
            N::If {
                cond: b(BinOp::Ge, getl("lo"), getl("hi")),
                then_: vec![N::Push(E::Call {
                    func: "substr".into(),
                    args: vec![getl("s"), i32c(0), i32c(0)],
                })],
                els: vec![N::Push(E::Call {
                    func: "substr".into(),
                    args: vec![getl("s"), getl("lo"), b(BinOp::Sub, getl("hi"), getl("lo"))],
                })],
                result: Some(WirTy::Str),
            },
        ],
        raw_body: None,
    }
}

/// `$is_ws(b) -> i32` — 1 iff byte `b` is ASCII whitespace (space, tab, LF, CR,
/// VT, FF). A pure OR of equalities, no loop.
pub fn is_ws_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let or = |l: E, r: E| E::Binary { op: BinOp::Or, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let eq = |c: i32| E::Binary {
        op: BinOp::Eq,
        kind: Kind::I32,
        lhs: Box::new(getl("b")),
        rhs: Box::new(i32c(c)),
    };
    WirFunc {
        name: "is_ws".into(),
        params: vec![WirLocal { name: "b".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: vec![],
        body: vec![N::Push(or(
            eq(32),
            or(eq(9), or(eq(10), or(eq(13), or(eq(11), eq(12))))),
        ))],
        raw_body: None,
    }
}

/// `$trim(s) -> i32` — `s` with leading and trailing ASCII whitespace removed.
/// Advances `lo` past leading whitespace and pulls `hi` in past trailing
/// whitespace, then `$substr`s the `[lo, hi)` byte window.
pub fn trim_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let not = |e: E| E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(e) };
    let is_ws_at = |idx: E| E::Call {
        func: "is_ws".into(),
        args: vec![E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), idx)), offset: 4 }],
    };
    let lo_loop = N::Block {
        label: "lodone".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "lodone".into(), cond: Some(b(BinOp::Ge, getl("lo"), getl("hi"))) },
                N::Br { target: "lodone".into(), cond: Some(not(is_ws_at(getl("lo")))) },
                setl("lo", b(BinOp::Add, getl("lo"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    let hi_loop = N::Block {
        label: "hidone".into(),
        result: None,
        body: vec![N::Loop {
            label: "h".into(),
            body: vec![
                N::Br { target: "hidone".into(), cond: Some(b(BinOp::Le, getl("hi"), getl("lo"))) },
                N::Br {
                    target: "hidone".into(),
                    cond: Some(not(is_ws_at(b(BinOp::Sub, getl("hi"), i32c(1))))),
                },
                setl("hi", b(BinOp::Sub, getl("hi"), i32c(1))),
                N::Br { target: "h".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "trim".into(),
        params: vec![WirLocal { name: "s".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Str],
        locals: ["len", "lo", "hi"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("len", load(getl("s"))),
            setl("lo", i32c(0)),
            setl("hi", getl("len")),
            lo_loop,
            hi_loop,
            N::Push(E::Call {
                func: "substr".into(),
                args: vec![getl("s"), getl("lo"), b(BinOp::Sub, getl("hi"), getl("lo"))],
            }),
        ],
        raw_body: None,
    }
}

/// `$split(s, sep) -> i32` — a `List(String)` of the pieces of `s` between
/// occurrences of `sep`. Empty `sep` yields `[s]`. Mirrors `$find_byte`'s
/// scan/compare loop nest; on each match it `$substr`s the piece and `$list_push`es
/// it, then `$substr`s the trailing piece after the loop. The substr pointer is
/// zero-extended into the list's i64 slot (a pointer, so the sign of the extend
/// is immaterial — the reader `i32.wrap_i64`s it back).
pub fn split_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let ext = |e: E| E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
    let push_piece = |start: E, len: E| E::Call {
        func: "list_push".into(),
        args: vec![getl("result"), ext(E::Call { func: "substr".into(), args: vec![getl("s"), start, len] })],
    };
    let s_byte = E::Load8U {
        ptr: Box::new(b(BinOp::Add, getl("s"), b(BinOp::Add, getl("i"), getl("j")))),
        offset: 4,
    };
    let sep_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("sep"), getl("j"))), offset: 4 };
    let cmp_loop = N::Block {
        label: "cmpdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "cmp".into(),
            body: vec![
                N::Br { target: "cmpdone".into(), cond: Some(b(BinOp::Ge, getl("j"), getl("seplen"))) },
                N::If {
                    cond: b(BinOp::Ne, s_byte, sep_byte),
                    then_: vec![setl("match", i32c(0)), N::Br { target: "cmpdone".into(), cond: None }],
                    els: vec![],
                    result: None,
                },
                setl("j", b(BinOp::Add, getl("j"), i32c(1))),
                N::Br { target: "cmp".into(), cond: None },
            ],
        }],
    };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "scan".into(),
            body: vec![
                N::Br {
                    target: "done".into(),
                    cond: Some(b(BinOp::Gt, getl("i"), b(BinOp::Sub, getl("slen"), getl("seplen")))),
                },
                setl("match", i32c(1)),
                setl("j", i32c(0)),
                cmp_loop,
                N::If {
                    cond: getl("match"),
                    then_: vec![
                        setl("result", push_piece(getl("start"), b(BinOp::Sub, getl("i"), getl("start")))),
                        setl("i", b(BinOp::Add, getl("i"), getl("seplen"))),
                        setl("start", getl("i")),
                    ],
                    els: vec![setl("i", b(BinOp::Add, getl("i"), i32c(1)))],
                    result: None,
                },
                N::Br { target: "scan".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "split".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "sep".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Bool], // i32 list pointer
        locals: ["slen", "seplen", "result", "start", "i", "j", "match"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("slen", load(getl("s"))),
            setl("seplen", load(getl("sep"))),
            N::Do(E::Call { func: "ensure".into(), args: vec![i32c(4)] }),
            setl("result", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("result"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("result"), i32c(4)) },
            N::If {
                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("seplen")) },
                then_: vec![N::Return(Some(E::Call {
                    func: "list_push".into(),
                    args: vec![getl("result"), ext(getl("s"))],
                }))],
                els: vec![],
                result: None,
            },
            setl("start", i32c(0)),
            setl("i", i32c(0)),
            scan_loop,
            setl("result", push_piece(getl("start"), b(BinOp::Sub, getl("slen"), getl("start")))),
            N::Push(getl("result")),
        ],
        raw_body: None,
    }
}

/// `$str_chars(s) -> i32` — a `List(String)` of `s`'s individual characters.
/// Counts characters via `$byte_to_char`, then `$str_substring`s each single-char
/// `[i, i+1)` window and `$list_push`es it (the substring handles multibyte
/// characters correctly).
pub fn str_chars_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let ext = |e: E| E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(e) };
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("n"))) },
                setl(
                    "result",
                    E::Call {
                        func: "list_push".into(),
                        args: vec![
                            getl("result"),
                            ext(E::Call {
                                func: "str_substring".into(),
                                args: vec![getl("s"), getl("i"), b(BinOp::Add, getl("i"), i32c(1))],
                            }),
                        ],
                    },
                ),
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "str_chars".into(),
        params: vec![WirLocal { name: "s".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Bool], // i32 list pointer
        locals: ["n", "i", "result"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("n", E::Call { func: "byte_to_char".into(), args: vec![getl("s"), load(getl("s"))] }),
            N::Do(E::Call { func: "ensure".into(), args: vec![i32c(4)] }),
            setl("result", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("result"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("result"), i32c(4)) },
            setl("i", i32c(0)),
            scan_loop,
            N::Push(getl("result")),
        ],
        raw_body: None,
    }
}

/// `$list_concat(a, b) -> i32` — a fresh list holding `a`'s elements followed by
/// `b`'s. Like the string `$concat`, but elements are 8-byte slots: allocate
/// `(alen+blen)` slots, `memory.copy` each source array in turn, bump `$heap`.
pub fn list_concat_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let total = b(BinOp::Add, getl("alen"), getl("blen"));
    WirFunc {
        name: "list_concat".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Bool },
            WirLocal { name: "b".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool], // i32 list pointer
        locals: ["alen", "blen", "new"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("alen", load(getl("a"))),
            setl("blen", load(getl("b"))),
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, total.clone(), i32c(8)))],
            }),
            setl("new", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("new"), value: total.clone(), kind: Kind::I32, offset: 0 },
            // a's elements → new+4
            N::MemoryCopy {
                dest: b(BinOp::Add, getl("new"), i32c(4)),
                src: b(BinOp::Add, getl("a"), i32c(4)),
                len: b(BinOp::Mul, getl("alen"), i32c(8)),
            },
            // b's elements → new+4 + alen*8
            N::MemoryCopy {
                dest: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), b(BinOp::Mul, getl("alen"), i32c(8))),
                src: b(BinOp::Add, getl("b"), i32c(4)),
                len: b(BinOp::Mul, getl("blen"), i32c(8)),
            },
            N::SetGlobal {
                global: "heap".into(),
                value: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), b(BinOp::Mul, total, i32c(8))),
            },
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}

/// `$ascii_case(s, up) -> i32` — `s` with ASCII letters cased: `up != 0`
/// uppercases (`a`–`z` → `A`–`Z`), else lowercases. Non-letters and non-ASCII
/// bytes copy through unchanged (byte-wise, so multibyte UTF-8 is preserved).
pub fn ascii_case_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let in_range = |lo: i32, hi: i32| b(BinOp::And, b(BinOp::GeU, getl("b"), i32c(lo)), b(BinOp::LeU, getl("b"), i32c(hi)));
    let scan_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("len"))) },
                setl("b", E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("i"))), offset: 4 }),
                N::If {
                    cond: getl("up"),
                    then_: vec![N::If {
                        cond: in_range(97, 122),
                        then_: vec![setl("b", b(BinOp::Sub, getl("b"), i32c(32)))],
                        els: vec![],
                        result: None,
                    }],
                    els: vec![N::If {
                        cond: in_range(65, 90),
                        then_: vec![setl("b", b(BinOp::Add, getl("b"), i32c(32)))],
                        els: vec![],
                        result: None,
                    }],
                    result: None,
                },
                N::Store8 { ptr: b(BinOp::Add, getl("res"), getl("i")), value: getl("b"), offset: 4 },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "ascii_case".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "up".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Str],
        locals: ["len", "i", "res", "b"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("len", load(getl("s"))),
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(4), getl("len"))] }),
            setl("res", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            setl("i", i32c(0)),
            scan_loop,
            N::SetGlobal {
                global: "heap".into(),
                value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("len")),
            },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$str_to_int(s) -> i64` — parse a (optionally signed) decimal integer,
/// tolerating leading/trailing ASCII whitespace. Traps (like Rust's checked
/// parse) on overflow, on no digits, or on trailing non-whitespace garbage —
/// matching the interpreter oracle, which errors on the same inputs.
pub fn str_to_int_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b32 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let b64 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I64, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let byte = || E::Load8U { ptr: Box::new(b32(BinOp::Add, getl("s"), getl("i"))), offset: 4 };
    let not = |e: E| E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(e) };
    let is_ws_b = || not(E::Call { func: "is_ws".into(), args: vec![getl("b")] });
    let inc_i = || setl("i", b32(BinOp::Add, getl("i"), i32c(1)));
    // digit magnitude (b - '0') widened to i64.
    let digit = || E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(b32(BinOp::Sub, getl("b"), i32c(48))) };
    let ws_skip = |done: &str, l: &str| N::Block {
        label: done.into(),
        result: None,
        body: vec![N::Loop {
            label: l.into(),
            body: vec![
                N::Br { target: done.into(), cond: Some(b32(BinOp::Ge, getl("i"), getl("len"))) },
                setl("b", byte()),
                N::Br { target: done.into(), cond: Some(is_ws_b()) },
                inc_i(),
                N::Br { target: l.into(), cond: None },
            ],
        }],
    };
    let digit_loop = N::Block {
        label: "digdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "dig".into(),
            body: vec![
                N::Br { target: "digdone".into(), cond: Some(b32(BinOp::Ge, getl("i"), getl("len"))) },
                setl("b", byte()),
                N::Br {
                    target: "digdone".into(),
                    cond: Some(b32(BinOp::Or, b32(BinOp::LtU, getl("b"), i32c(48)), b32(BinOp::GtU, getl("b"), i32c(57)))),
                },
                // overflow: acc >u (limit - d) / 10  ->  trap.
                N::If {
                    cond: b64(
                        BinOp::GtU,
                        getl("acc"),
                        b64(BinOp::DivU, b64(BinOp::Sub, getl("limit"), digit()), i64c(10)),
                    ),
                    then_: vec![N::Unreachable],
                    els: vec![],
                    result: None,
                },
                setl("acc", b64(BinOp::Add, b64(BinOp::Mul, getl("acc"), i64c(10)), digit())),
                setl("got", i32c(1)),
                inc_i(),
                N::Br { target: "dig".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "str_to_int".into(),
        params: vec![WirLocal { name: "s".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Int],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "b".into(), ty: WirTy::Bool },
            WirLocal { name: "acc".into(), ty: WirTy::Int },
            WirLocal { name: "neg".into(), ty: WirTy::Bool },
            WirLocal { name: "got".into(), ty: WirTy::Bool },
            WirLocal { name: "limit".into(), ty: WirTy::Int },
        ],
        body: vec![
            setl("len", load(getl("s"))),
            setl("i", i32c(0)),
            setl("acc", i64c(0)),
            setl("neg", i32c(0)),
            setl("got", i32c(0)),
            ws_skip("wsdone", "ws"),
            // optional sign
            N::If {
                cond: b32(BinOp::Lt, getl("i"), getl("len")),
                then_: vec![
                    setl("b", byte()),
                    N::If {
                        cond: b32(BinOp::Eq, getl("b"), i32c(45)),
                        then_: vec![setl("neg", i32c(1)), inc_i()],
                        els: vec![N::If {
                            cond: b32(BinOp::Eq, getl("b"), i32c(43)),
                            then_: vec![inc_i()],
                            els: vec![],
                            result: None,
                        }],
                        result: None,
                    },
                ],
                els: vec![],
                result: None,
            },
            // magnitude bound: |i64::MIN| for negatives, i64::MAX otherwise.
            N::If {
                cond: getl("neg"),
                then_: vec![setl("limit", i64c(i64::MIN))],
                els: vec![setl("limit", i64c(i64::MAX))],
                result: None,
            },
            digit_loop,
            ws_skip("twsdone", "tws"),
            // must have consumed at least one digit and reached the end.
            N::If {
                cond: b32(BinOp::Or, not(getl("got")), b32(BinOp::Lt, getl("i"), getl("len"))),
                then_: vec![N::Unreachable],
                els: vec![],
                result: None,
            },
            N::If {
                cond: getl("neg"),
                then_: vec![N::Push(b64(BinOp::Sub, i64c(0), getl("acc")))],
                els: vec![N::Push(getl("acc"))],
                result: Some(WirTy::Int),
            },
        ],
        raw_body: None,
    }
}

// --- Dict helpers ------------------------------------------------------------
// A Dict pointer `d` addresses an i32 `count` at offset 0, then `count` 16-byte
// entries (i64 key at entry+0, i64 value at entry+8); entry i is at d+4+i*16.
// A hidden word at d-4 is 0 (linear scan) or an open-addressing index pointer.
// On the binary path only the non-`_cap` helpers are migrated, and none of them
// build an index, so d-4 stays 0 and `$dict_find` always takes the linear path —
// but the hash path is ported faithfully anyway so the helper is correct if a
// future cap-insert migration starts hanging an index.

/// `$key_eq(a, b, mode) -> i32` — slot equality under the key's compile-time
/// type: mode 0 = raw i64 (Int/Bool), 1 = `$str_eq` on the pointers (String),
/// else f64 (the slots reinterpreted as doubles).
pub fn key_eq_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let wrap = |n: &str| E::FromSlot(Box::new(getl(n)), Kind::I32);
    WirFunc {
        name: "key_eq".into(),
        params: vec![
            WirLocal { name: "a".into(), ty: WirTy::Int },
            WirLocal { name: "b".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![],
        body: vec![N::If {
            cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("mode")) },
            then_: vec![N::Push(E::Binary {
                op: BinOp::Eq,
                kind: Kind::I64,
                lhs: Box::new(getl("a")),
                rhs: Box::new(getl("b")),
            })],
            els: vec![N::If {
                cond: E::Binary { op: BinOp::Eq, kind: Kind::I32, lhs: Box::new(getl("mode")), rhs: Box::new(i32c(1)) },
                then_: vec![N::Push(E::Call { func: "str_eq".into(), args: vec![wrap("a"), wrap("b")] })],
                els: vec![N::Push(E::Binary {
                    op: BinOp::Eq,
                    kind: Kind::F64,
                    lhs: Box::new(E::FromSlot(Box::new(getl("a")), Kind::F64)),
                    rhs: Box::new(E::FromSlot(Box::new(getl("b")), Kind::F64)),
                })],
                result: Some(WirTy::Bool),
            }],
            result: Some(WirTy::Bool),
        }],
        raw_body: None,
    }
}

/// `$dict_hash(k, mode) -> i32` — a 64-bit bit-mix for scalar keys (mode 0),
/// FNV-1a over the bytes for string keys (mode 1, `k` = string pointer). Only
/// consulted by `$dict_find`'s (binary-path-dormant) hash probe.
pub fn dict_hash_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let i64c = E::ConstI64;
    let b32 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let b64 = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I64, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let fnv_loop = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b32(BinOp::Ge, getl("i"), getl("len"))) },
                setl("h", b32(BinOp::Xor, getl("h"), E::Load8U { ptr: Box::new(b32(BinOp::Add, getl("p"), getl("i"))), offset: 4 })),
                setl("h", b32(BinOp::Mul, getl("h"), i32c(16777619))),
                setl("i", b32(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "dict_hash".into(),
        params: vec![
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "x".into(), ty: WirTy::Int },
            WirLocal { name: "p".into(), ty: WirTy::Bool },
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "i".into(), ty: WirTy::Bool },
            WirLocal { name: "h".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::If {
                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("mode")) },
                then_: vec![
                    setl("x", getl("k")),
                    setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(33)))),
                    setl("x", b64(BinOp::Mul, getl("x"), i64c(-49064778989728563))),
                    setl("x", b64(BinOp::Xor, getl("x"), b64(BinOp::ShrU, getl("x"), i64c(33)))),
                    N::Return(Some(E::FromSlot(Box::new(getl("x")), Kind::I32))),
                ],
                els: vec![],
                result: None,
            },
            setl("p", E::FromSlot(Box::new(getl("k")), Kind::I32)),
            setl("len", E::Load { ptr: Box::new(getl("p")), kind: Kind::I32, offset: 0 }),
            setl("h", i32c(-2128831035)),
            setl("i", i32c(0)),
            fnv_loop,
            N::Push(getl("h")),
        ],
        raw_body: None,
    }
}

/// `$dict_find(d, k, mode) -> i32` — the entry index of key `k`, or -1. Linear
/// scan when the hidden index word is 0 (always, on the binary path); otherwise
/// an open-addressing probe over the hash table.
pub fn dict_find_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E, off: u32| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: off };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    // key slot of entry `e`: d + 4 + e*16.
    let key_at = |e: E| E::Load { ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, e, i32c(16)))), kind: Kind::I64, offset: 4 };
    let keq = |e: E| E::Call { func: "key_eq".into(), args: vec![key_at(e), getl("k"), getl("mode")] };
    let linear = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("count"))) },
                N::If { cond: keq(getl("i")), then_: vec![N::Return(Some(getl("i")))], els: vec![], result: None },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    // slot value at index table position h: idx + 4 + h*4.
    let slot_at_h = load(b(BinOp::Add, b(BinOp::Add, getl("idx"), i32c(4)), b(BinOp::Mul, getl("h"), i32c(4))), 0);
    let probe = N::Block {
        label: "miss".into(),
        result: None,
        body: vec![N::Loop {
            label: "p".into(),
            body: vec![
                setl("e", slot_at_h),
                N::Br { target: "miss".into(), cond: Some(E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("e")) }) },
                N::If {
                    cond: keq(b(BinOp::Sub, getl("e"), i32c(1))),
                    then_: vec![N::Return(Some(b(BinOp::Sub, getl("e"), i32c(1))))],
                    els: vec![],
                    result: None,
                },
                setl("h", b(BinOp::And, b(BinOp::Add, getl("h"), i32c(1)), b(BinOp::Sub, getl("slots"), i32c(1)))),
                N::Br { target: "p".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "dict_find".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["idx", "count", "i", "slots", "h", "e"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("idx", load(b(BinOp::Sub, getl("d"), i32c(4)), 0)),
            N::If {
                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("idx")) },
                then_: vec![
                    setl("count", load(getl("d"), 0)),
                    setl("i", i32c(0)),
                    linear,
                    N::Return(Some(i32c(-1))),
                ],
                els: vec![],
                result: None,
            },
            setl("slots", load(getl("idx"), 0)),
            setl("h", b(BinOp::And, E::Call { func: "dict_hash".into(), args: vec![getl("k"), getl("mode")] }, b(BinOp::Sub, getl("slots"), i32c(1)))),
            probe,
            N::Push(i32c(-1)),
        ],
        raw_body: None,
    }
}

/// `$dict_new() -> i32` — an empty dict: 8 reserved bytes holding a zero hidden
/// word (at p-4) and a zero count (at p), with `p` returned.
pub fn dict_new_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "dict_new".into(),
        params: vec![],
        ret: vec![WirTy::Bool],
        locals: vec![WirLocal { name: "p".into(), ty: WirTy::Bool }],
        body: vec![
            N::Do(E::Call { func: "ensure".into(), args: vec![i32c(8)] }),
            N::SetLocal { local: "p".into(), value: b(BinOp::Add, E::GetGlobal("heap".into()), i32c(4)) },
            N::Store { ptr: b(BinOp::Sub, getl("p"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Store { ptr: getl("p"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("p"), i32c(4)) },
            N::Push(getl("p")),
        ],
        raw_body: None,
    }
}

/// `$dict_insert(d, k, v, mode) -> i32` — a fresh dict like `d` with `k` set to
/// `v`: the matching entry's value replaced, or `(k, v)` appended. Copies the
/// existing block (resetting the hidden index word to 0), then writes in place.
pub fn dict_insert_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    WirFunc {
        name: "dict_insert".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "v".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["count", "found", "new", "bytes"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("count", E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 }),
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, i32c(24), b(BinOp::Mul, getl("count"), i32c(16)))],
            }),
            setl("found", E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] }),
            setl("bytes", b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(16)))),
            setl("new", b(BinOp::Add, E::GetGlobal("heap".into()), i32c(4))),
            N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::MemoryCopy { dest: getl("new"), src: getl("d"), len: getl("bytes") },
            N::If {
                cond: b(BinOp::Ge, getl("found"), i32c(0)),
                then_: vec![
                    // replace value slot of the found entry: new + 12 + found*16.
                    N::Store {
                        ptr: b(BinOp::Add, getl("new"), b(BinOp::Mul, getl("found"), i32c(16))),
                        value: getl("v"),
                        kind: Kind::I64,
                        offset: 12,
                    },
                    N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("new"), getl("bytes")) },
                    N::Push(getl("new")),
                ],
                els: vec![
                    N::Store { ptr: getl("new"), value: b(BinOp::Add, getl("count"), i32c(1)), kind: Kind::I32, offset: 0 },
                    N::Store { ptr: b(BinOp::Add, getl("new"), getl("bytes")), value: getl("k"), kind: Kind::I64, offset: 0 },
                    N::Store { ptr: b(BinOp::Add, getl("new"), getl("bytes")), value: getl("v"), kind: Kind::I64, offset: 8 },
                    N::SetGlobal {
                        global: "heap".into(),
                        value: b(BinOp::Add, b(BinOp::Add, getl("new"), getl("bytes")), i32c(16)),
                    },
                    N::Push(getl("new")),
                ],
                result: Some(WirTy::Bool),
            },
        ],
        raw_body: None,
    }
}

/// `$dict_get_or(d, k, default, mode) -> i64` — the value slot for `k`, or
/// `default` when absent.
pub fn dict_get_or_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "dict_get_or".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "default".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Int],
        locals: vec![WirLocal { name: "found".into(), ty: WirTy::Bool }],
        body: vec![
            N::SetLocal { local: "found".into(), value: E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] } },
            N::If {
                cond: b(BinOp::Lt, getl("found"), i32c(0)),
                then_: vec![N::Return(Some(getl("default")))],
                els: vec![],
                result: None,
            },
            // value slot: d + 12 + found*16.
            N::Push(E::Load {
                ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, getl("found"), i32c(16)))),
                kind: Kind::I64,
                offset: 12,
            }),
        ],
        raw_body: None,
    }
}

/// `$dict_has(d, k, mode) -> i32` — whether `k` is present.
pub fn dict_has_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    WirFunc {
        name: "dict_has".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: vec![],
        body: vec![N::Push(E::Binary {
            op: BinOp::Ge,
            kind: Kind::I32,
            lhs: Box::new(E::Call { func: "dict_find".into(), args: vec![getl("d"), getl("k"), getl("mode")] }),
            rhs: Box::new(E::ConstI32(0)),
        })],
        raw_body: None,
    }
}

/// Shared body for `$dict_keys` / `$dict_values`: copy each entry's slot at
/// `entry_off` (4 = key, 12 = value) into a fresh `count`-element list.
pub fn dict_project_helper(name: &str, entry_off: u32) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let src = E::Load {
        ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, getl("i"), i32c(16)))),
        kind: Kind::I64,
        offset: entry_off,
    };
    let scan = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("count"))) },
                N::Store { ptr: b(BinOp::Add, getl("new"), b(BinOp::Mul, getl("i"), i32c(8))), value: src, kind: Kind::I64, offset: 4 },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: name.into(),
        params: vec![WirLocal { name: "d".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: ["count", "i", "new"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            setl("count", E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 }),
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(8)))] }),
            setl("new", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("new"), value: getl("count"), kind: Kind::I32, offset: 0 },
            setl("i", i32c(0)),
            scan,
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), b(BinOp::Mul, getl("count"), i32c(8))) },
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}

/// `$dict_pairs(d) -> i32` — a `List((K, V))`: one `[0][key][value]` tuple per
/// entry (20 bytes: i32 tag + two i64 slots), with the list holding the tuple
/// pointers. Reserves the list slots first, then allocates tuples after it.
pub fn dict_pairs_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let entry = |off: u32| E::Load {
        ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, getl("i"), i32c(16)))),
        kind: Kind::I64,
        offset: off,
    };
    let scan = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("count"))) },
                setl("tup", E::GetGlobal("heap".into())),
                N::Store { ptr: getl("tup"), value: i32c(0), kind: Kind::I32, offset: 0 },
                N::Store { ptr: getl("tup"), value: entry(4), kind: Kind::I64, offset: 4 },
                N::Store { ptr: getl("tup"), value: entry(12), kind: Kind::I64, offset: 12 },
                N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("tup"), i32c(20)) },
                // list slot i ← tuple pointer (zero-extended into the i64 slot).
                N::Store {
                    ptr: b(BinOp::Add, getl("list"), b(BinOp::Mul, getl("i"), i32c(8))),
                    value: E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("tup")) },
                    kind: Kind::I64,
                    offset: 4,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "dict_pairs".into(),
        params: vec![WirLocal { name: "d".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: ["count", "i", "list", "tup"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            setl("count", E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 }),
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, b(BinOp::Add, i32c(4), b(BinOp::Mul, getl("count"), i32c(8))), b(BinOp::Mul, getl("count"), i32c(20)))],
            }),
            setl("list", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("list"), value: getl("count"), kind: Kind::I32, offset: 0 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("list"), i32c(4)), b(BinOp::Mul, getl("count"), i32c(8))) },
            setl("i", i32c(0)),
            scan,
            N::Push(getl("list")),
        ],
        raw_body: None,
    }
}

/// `$dict_remove(d, k, mode) -> i32` — a fresh dict with the entry for `k`
/// dropped (unchanged if absent). Copies every entry whose key isn't `k`.
pub fn dict_remove_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let entry = |off: u32| E::Load {
        ptr: Box::new(b(BinOp::Add, getl("d"), b(BinOp::Mul, getl("i"), i32c(16)))),
        kind: Kind::I64,
        offset: off,
    };
    let dst = b(BinOp::Add, getl("new"), b(BinOp::Mul, getl("n"), i32c(16)));
    let scan = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("i"), getl("count"))) },
                N::If {
                    cond: E::Unary {
                        op: UnOp::Not,
                        kind: Kind::I32,
                        arg: Box::new(E::Call { func: "key_eq".into(), args: vec![entry(4), getl("k"), getl("mode")] }),
                    },
                    then_: vec![
                        N::Store { ptr: dst.clone(), value: entry(4), kind: Kind::I64, offset: 4 },
                        N::Store { ptr: dst.clone(), value: entry(12), kind: Kind::I64, offset: 12 },
                        setl("n", b(BinOp::Add, getl("n"), i32c(1))),
                    ],
                    els: vec![],
                    result: None,
                },
                setl("i", b(BinOp::Add, getl("i"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "dict_remove".into(),
        params: vec![
            WirLocal { name: "d".into(), ty: WirTy::Bool },
            WirLocal { name: "k".into(), ty: WirTy::Int },
            WirLocal { name: "mode".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["count", "i", "new", "n"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            setl("count", E::Load { ptr: Box::new(getl("d")), kind: Kind::I32, offset: 0 }),
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, i32c(8), b(BinOp::Mul, getl("count"), i32c(16)))] }),
            setl("new", b(BinOp::Add, E::GetGlobal("heap".into()), i32c(4))),
            N::Store { ptr: b(BinOp::Sub, getl("new"), i32c(4)), value: i32c(0), kind: Kind::I32, offset: 0 },
            setl("i", i32c(0)),
            setl("n", i32c(0)),
            scan,
            N::Store { ptr: getl("new"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("new"), i32c(4)), b(BinOp::Mul, getl("n"), i32c(16))) },
            N::Push(getl("new")),
        ],
        raw_body: None,
    }
}

/// `$match_at(s, from, pos) -> i32` — 1 iff `from` occurs in `s` starting at
/// byte offset `pos`. Bails to 0 if `from` would run off the end or any byte
/// differs.
pub fn match_at_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), b(BinOp::Add, getl("pos"), getl("j")))), offset: 4 };
    let from_byte = E::Load8U { ptr: Box::new(b(BinOp::Add, getl("from"), getl("j"))), offset: 4 };
    let scan = N::Block {
        label: "done".into(),
        result: None,
        body: vec![N::Loop {
            label: "l".into(),
            body: vec![
                N::Br { target: "done".into(), cond: Some(b(BinOp::Ge, getl("j"), getl("flen"))) },
                N::If { cond: b(BinOp::Ne, s_byte, from_byte), then_: vec![N::Return(Some(i32c(0)))], els: vec![], result: None },
                setl("j", b(BinOp::Add, getl("j"), i32c(1))),
                N::Br { target: "l".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "match_at".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "from".into(), ty: WirTy::Str },
            WirLocal { name: "pos".into(), ty: WirTy::Bool },
        ],
        ret: vec![WirTy::Bool],
        locals: ["flen", "j"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            setl("flen", load(getl("from"))),
            N::If {
                cond: b(BinOp::Gt, b(BinOp::Add, getl("pos"), getl("flen")), load(getl("s"))),
                then_: vec![N::Return(Some(i32c(0)))],
                els: vec![],
                result: None,
            },
            setl("j", i32c(0)),
            scan,
            N::Push(i32c(1)),
        ],
        raw_body: None,
    }
}

/// `$replace(s, from, to) -> i32` — `s` with every occurrence of `from` replaced
/// by `to`. Empty `from` inserts `to` between every character (and at both ends),
/// stepping by UTF-8 sequence length. Otherwise counts matches via `$match_at`,
/// allocates the exact result, then copies through replacing each match.
pub fn replace_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let load = |p: E| E::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
    let setl = |n: &str, v: E| N::SetLocal { local: n.into(), value: v };
    let s_off = |off: E| b(BinOp::Add, b(BinOp::Add, getl("s"), i32c(4)), off);
    let to_bytes = b(BinOp::Add, getl("to"), i32c(4));
    let match_here = || E::Call { func: "match_at".into(), args: vec![getl("s"), getl("from"), getl("src")] };
    // seqlen(b) into `clen` — UTF-8 lead-byte classification.
    let seqlen = N::If {
        cond: b(BinOp::LtU, getl("b"), i32c(0x80)),
        then_: vec![setl("clen", i32c(1))],
        els: vec![N::If {
            cond: b(BinOp::LtU, getl("b"), i32c(0xe0)),
            then_: vec![setl("clen", i32c(2))],
            els: vec![N::If {
                cond: b(BinOp::LtU, getl("b"), i32c(0xf0)),
                then_: vec![setl("clen", i32c(3))],
                els: vec![setl("clen", i32c(4))],
                result: None,
            }],
            result: None,
        }],
        result: None,
    };
    // --- empty-`from` branch: insert `to` around every character. ---
    let empty_loop = N::Block {
        label: "cdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "cl".into(),
            body: vec![
                N::Br { target: "cdone".into(), cond: Some(b(BinOp::Ge, getl("src"), getl("slen"))) },
                setl("b", E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("src"))), offset: 4 }),
                seqlen,
                N::MemoryCopy { dest: getl("dst"), src: s_off(getl("src")), len: getl("clen") },
                setl("dst", b(BinOp::Add, getl("dst"), getl("clen"))),
                N::MemoryCopy { dest: getl("dst"), src: to_bytes.clone(), len: getl("tlen") },
                setl("dst", b(BinOp::Add, getl("dst"), getl("tlen"))),
                setl("src", b(BinOp::Add, getl("src"), getl("clen"))),
                N::Br { target: "cl".into(), cond: None },
            ],
        }],
    };
    let empty_branch = vec![
        setl("res", E::GetGlobal("heap".into())),
        setl("dst", b(BinOp::Add, getl("res"), i32c(4))),
        N::MemoryCopy { dest: getl("dst"), src: to_bytes.clone(), len: getl("tlen") },
        setl("dst", b(BinOp::Add, getl("dst"), getl("tlen"))),
        setl("src", i32c(0)),
        empty_loop,
        setl("reslen", b(BinOp::Sub, getl("dst"), b(BinOp::Add, getl("res"), i32c(4)))),
        N::Store { ptr: getl("res"), value: getl("reslen"), kind: Kind::I32, offset: 0 },
        N::SetGlobal { global: "heap".into(), value: getl("dst") },
        N::Return(Some(getl("res"))),
    ];
    // --- non-empty `from`: count matches, then fill. ---
    let count_loop = N::Block {
        label: "countdone".into(),
        result: None,
        body: vec![N::Loop {
            label: "cl2".into(),
            body: vec![
                N::Br { target: "countdone".into(), cond: Some(b(BinOp::Gt, b(BinOp::Add, getl("src"), getl("flen")), getl("slen"))) },
                N::If {
                    cond: match_here(),
                    then_: vec![setl("cnt", b(BinOp::Add, getl("cnt"), i32c(1))), setl("src", b(BinOp::Add, getl("src"), getl("flen")))],
                    els: vec![setl("src", b(BinOp::Add, getl("src"), i32c(1)))],
                    result: None,
                },
                N::Br { target: "cl2".into(), cond: None },
            ],
        }],
    };
    let fill_loop = N::Block {
        label: "filldone".into(),
        result: None,
        body: vec![N::Loop {
            label: "fl".into(),
            body: vec![
                N::Br { target: "filldone".into(), cond: Some(b(BinOp::Ge, getl("src"), getl("slen"))) },
                N::If {
                    cond: match_here(),
                    then_: vec![
                        N::MemoryCopy { dest: getl("dst"), src: to_bytes.clone(), len: getl("tlen") },
                        setl("dst", b(BinOp::Add, getl("dst"), getl("tlen"))),
                        setl("src", b(BinOp::Add, getl("src"), getl("flen"))),
                    ],
                    els: vec![
                        N::Store8 { ptr: getl("dst"), value: E::Load8U { ptr: Box::new(b(BinOp::Add, getl("s"), getl("src"))), offset: 4 }, offset: 0 },
                        setl("dst", b(BinOp::Add, getl("dst"), i32c(1))),
                        setl("src", b(BinOp::Add, getl("src"), i32c(1))),
                    ],
                    result: None,
                },
                N::Br { target: "fl".into(), cond: None },
            ],
        }],
    };
    WirFunc {
        name: "replace".into(),
        params: vec![
            WirLocal { name: "s".into(), ty: WirTy::Str },
            WirLocal { name: "from".into(), ty: WirTy::Str },
            WirLocal { name: "to".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str],
        locals: ["slen", "flen", "tlen", "cnt", "src", "dst", "res", "reslen", "b", "clen"]
            .iter()
            .map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool })
            .collect(),
        body: vec![
            setl("slen", load(getl("s"))),
            setl("flen", load(getl("from"))),
            setl("tlen", load(getl("to"))),
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, b(BinOp::Add, i32c(4), getl("slen")), b(BinOp::Mul, b(BinOp::Add, getl("slen"), i32c(1)), getl("tlen")))],
            }),
            N::If {
                cond: E::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(getl("flen")) },
                then_: empty_branch,
                els: vec![],
                result: None,
            },
            setl("cnt", i32c(0)),
            setl("src", i32c(0)),
            count_loop,
            setl("reslen", b(BinOp::Add, getl("slen"), b(BinOp::Mul, getl("cnt"), b(BinOp::Sub, getl("tlen"), getl("flen"))))),
            setl("res", E::GetGlobal("heap".into())),
            N::Store { ptr: getl("res"), value: getl("reslen"), kind: Kind::I32, offset: 0 },
            setl("dst", b(BinOp::Add, getl("res"), i32c(4))),
            setl("src", i32c(0)),
            fill_loop,
            N::SetGlobal { global: "heap".into(), value: getl("dst") },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$encoding(op, in) -> i32` — a thin wrapper over the host `encoding` import,
/// which does the actual hex/base64 transform (op 0 hex-encode, 1 hex-decode,
/// 2 base64-encode, 3 base64-decode, 4 base64url-of-hex). Reserves a worst-case
/// `2*len + 20` result buffer, lets the host write into `res+4`, and caps the
/// length header to what it returned. The first migrated host-import helper.
pub fn encoding_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "encoding".into(),
        params: vec![
            WirLocal { name: "op".into(), ty: WirTy::Bool },
            WirLocal { name: "in".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "res".into(), ty: WirTy::Bool },
            WirLocal { name: "n".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::Do(E::Call {
                func: "ensure".into(),
                args: vec![b(BinOp::Add, b(BinOp::Mul, E::Load { ptr: Box::new(getl("in")), kind: Kind::I32, offset: 0 }, i32c(2)), i32c(20))],
            }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::SetLocal {
                local: "n".into(),
                value: E::CallHost { import: "encoding".into(), args: vec![getl("op"), getl("in"), b(BinOp::Add, getl("res"), i32c(4))] },
            },
            N::Store { ptr: getl("res"), value: getl("n"), kind: Kind::I32, offset: 0 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("n")) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// Shared body for the fixed-length crypto digests: reserve `hexlen+4` bytes,
/// write the length header, hand the inputs + `res+4` to the host `import`, and
/// bump `$heap`. `inputs` are the string-pointer params (one for the plain
/// hashes, two — key, msg — for HMAC). The crypto imports are host-provided
/// unconditionally (hashing needs no capability).
fn crypto_hash_helper(name: &str, import: &str, hexlen: i32, inputs: &[&str]) -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    let mut host_args: Vec<E> = inputs.iter().map(|n| getl(n)).collect();
    host_args.push(b(BinOp::Add, getl("res"), i32c(4)));
    WirFunc {
        name: name.into(),
        params: inputs.iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Str }).collect(),
        ret: vec![WirTy::Str],
        locals: vec![WirLocal { name: "res".into(), ty: WirTy::Bool }],
        body: vec![
            N::Do(E::Call { func: "ensure".into(), args: vec![i32c(hexlen + 4)] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: i32c(hexlen), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: import.into(), args: host_args }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("res"), i32c(hexlen + 4)) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$dir_read(h, rel) -> i32` — the contents of file `rel` under dir handle `h`,
/// as a String. Two-phase host protocol: `dir_read_len` reads the file and
/// reports its byte length (staging the bytes host-side), then `fill_pending`
/// copies the staged bytes into `res+4`. Needs the Dir(Read) capability.
pub fn dir_read_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "dir_read".into(),
        params: vec![
            WirLocal { name: "h".into(), ty: WirTy::Bool },
            WirLocal { name: "rel".into(), ty: WirTy::Str },
        ],
        ret: vec![WirTy::Str],
        locals: vec![
            WirLocal { name: "len".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "dir_read_len".into(), args: vec![getl("h"), getl("rel")] } },
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "fill_pending".into(), args: vec![b(BinOp::Add, getl("res"), i32c(4))] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("res"), i32c(4)), getl("len")) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$dir_list(h) -> i32` — the entries of directory handle `h`, as a
/// `List(String)`. The host reports the total byte size of the marshaled list
/// (`dir_list_size`), then writes the whole `[count][ptr..]` + payload structure
/// into the reserved block (`write_pending_list`). Needs the Dir(Read) capability.
pub fn dir_list_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "dir_list".into(),
        params: vec![WirLocal { name: "h".into(), ty: WirTy::Bool }],
        ret: vec![WirTy::Bool],
        locals: vec![
            WirLocal { name: "size".into(), ty: WirTy::Bool },
            WirLocal { name: "res".into(), ty: WirTy::Bool },
        ],
        body: vec![
            N::SetLocal { local: "size".into(), value: E::CallHost { import: "dir_list_size".into(), args: vec![getl("h")] } },
            N::Do(E::Call { func: "ensure".into(), args: vec![getl("size")] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Do(E::CallHost { import: "write_pending_list".into(), args: vec![getl("res")] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("res"), getl("size")) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// `$get_env(name) -> i32` — the value of env var `name` as an `Option(String)`
/// (`[tag][payload]`: tag 0 = Some with the string pointer in the i64 slot at +4,
/// tag 1 = None). `env_len` reports the value's length (or <0 if absent); on
/// presence `env_fill` copies the bytes. Needs the Env capability. (Reachable on
/// the binary path now that `match` on its Option result lowers via the
/// constructor-pattern arm.)
pub fn get_env_helper() -> WirFunc {
    use WirExpr as E;
    use WirNode as N;
    let getl = |n: &str| E::GetLocal(n.into());
    let i32c = E::ConstI32;
    let b = |op: BinOp, l: E, r: E| E::Binary { op, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
    WirFunc {
        name: "get_env".into(),
        params: vec![WirLocal { name: "name".into(), ty: WirTy::Str }],
        ret: vec![WirTy::Bool],
        locals: ["len", "str", "res"].iter().map(|n| WirLocal { name: (*n).into(), ty: WirTy::Bool }).collect(),
        body: vec![
            N::SetLocal { local: "len".into(), value: E::CallHost { import: "env_len".into(), args: vec![getl("name")] } },
            N::If {
                cond: b(BinOp::Lt, getl("len"), i32c(0)),
                then_: vec![
                    N::Do(E::Call { func: "ensure".into(), args: vec![i32c(4)] }),
                    N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
                    N::Store { ptr: getl("res"), value: i32c(1), kind: Kind::I32, offset: 0 },
                    N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("res"), i32c(4)) },
                    N::Return(Some(getl("res"))),
                ],
                els: vec![],
                result: None,
            },
            N::Do(E::Call { func: "ensure".into(), args: vec![b(BinOp::Add, getl("len"), i32c(4))] }),
            N::SetLocal { local: "str".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("str"), value: getl("len"), kind: Kind::I32, offset: 0 },
            N::Do(E::CallHost { import: "env_fill".into(), args: vec![getl("name"), b(BinOp::Add, getl("str"), i32c(4))] }),
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, b(BinOp::Add, getl("str"), i32c(4)), getl("len")) },
            N::Do(E::Call { func: "ensure".into(), args: vec![i32c(12)] }),
            N::SetLocal { local: "res".into(), value: E::GetGlobal("heap".into()) },
            N::Store { ptr: getl("res"), value: i32c(0), kind: Kind::I32, offset: 0 },
            N::Store { ptr: getl("res"), value: E::Convert { from: Kind::I32, to: Kind::I64, arg: Box::new(getl("str")) }, kind: Kind::I64, offset: 4 },
            N::SetGlobal { global: "heap".into(), value: b(BinOp::Add, getl("res"), i32c(12)) },
            N::Push(getl("res")),
        ],
        raw_body: None,
    }
}

/// A WIR-native prelude helper plus the module-level resources it needs (so a
/// pruned module declares only the imports/globals/table its reached helpers
/// actually touch — capability-minimal).
pub struct WirHelperSpec {
    pub func: WirFunc,
    /// Other prelude helpers this one calls (transitively pulled in).
    pub helper_deps: &'static [&'static str],
    /// Host imports (the `witchy` field names) this helper calls directly.
    pub import_deps: &'static [&'static str],
    /// Whether it reads/writes the `$heap` / `$__witchy_reowns` globals.
    pub uses_heap: bool,
    /// Whether it does a `call_indirect` (needs table 0).
    pub uses_table: bool,
}

/// The WIR-native prelude registry: the helpers migrated off the raw-body blob
/// so far. `None` for a helper not yet migrated — `assemble_wir_module` then
/// falls back to the raw-body prelude for any program that reaches it. Helpers
/// migrate one at a time; each is a green step that grows binary-path coverage.
pub fn wir_helper(name: &str) -> Option<WirHelperSpec> {
    match name {
        "print_str" => Some(WirHelperSpec {
            func: print_str_helper(),
            helper_deps: &[],
            import_deps: &["print"],
            uses_heap: false,
            uses_table: false,
        }),
        "ensure" => Some(WirHelperSpec {
            func: ensure_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_at" => Some(WirHelperSpec {
            func: list_at_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "int_to_string" => Some(WirHelperSpec {
            func: int_to_string_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "str_eq" => Some(WirHelperSpec {
            func: str_eq_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "find_byte" => Some(WirHelperSpec {
            func: find_byte_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "starts_with" => Some(WirHelperSpec {
            func: starts_with_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "ends_with" => Some(WirHelperSpec {
            func: ends_with_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "byte_to_char" => Some(WirHelperSpec {
            func: byte_to_char_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "substr" => Some(WirHelperSpec {
            func: substr_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "char_to_byte" => Some(WirHelperSpec {
            func: char_to_byte_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "str_substring" => Some(WirHelperSpec {
            func: str_substring_helper(),
            helper_deps: &["char_to_byte", "substr"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "is_ws" => Some(WirHelperSpec {
            func: is_ws_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "trim" => Some(WirHelperSpec {
            func: trim_helper(),
            helper_deps: &["is_ws", "substr"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "str_index_of" => Some(WirHelperSpec {
            func: str_index_of_helper(),
            helper_deps: &["find_byte", "byte_to_char"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "concat" => Some(WirHelperSpec {
            func: concat_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_push_cap" => Some(WirHelperSpec {
            func: list_push_cap_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_push" => Some(WirHelperSpec {
            func: list_push_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "split" => Some(WirHelperSpec {
            func: split_helper(),
            helper_deps: &["ensure", "substr", "list_push"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "str_chars" => Some(WirHelperSpec {
            func: str_chars_helper(),
            helper_deps: &["ensure", "byte_to_char", "str_substring", "list_push"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "list_concat" => Some(WirHelperSpec {
            func: list_concat_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "ascii_case" => Some(WirHelperSpec {
            func: ascii_case_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "match_at" => Some(WirHelperSpec {
            func: match_at_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "encoding" => Some(WirHelperSpec {
            func: encoding_helper(),
            helper_deps: &["ensure"],
            import_deps: &["encoding"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sha256" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_sha256", "crypto.sha256", 64, &["in"]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.sha256"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sha512" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_sha512", "crypto.sha512", 128, &["in"]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.sha512"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sha3_256" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_sha3_256", "crypto.sha3_256", 64, &["in"]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.sha3_256"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_hmac_sha256" => Some(WirHelperSpec {
            func: crypto_hash_helper("crypto_hmac_sha256", "crypto.hmac_sha256", 64, &["key", "msg"]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.hmac_sha256"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_rune_hash" => Some(WirHelperSpec {
            // paths + contents are List(String) pointers; the host hashes them
            // into a fixed 71-char digest.
            func: crypto_hash_helper("crypto_rune_hash", "crypto.rune_hash", 71, &["paths", "contents"]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.rune_hash"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_sign" => Some(WirHelperSpec {
            // The Secret capability: the host signs `msg` with the never-exposed
            // seed and writes a 128-char hex signature.
            func: crypto_hash_helper("crypto_sign", "crypto.sign", 128, &["msg"]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.sign"],
            uses_heap: true,
            uses_table: false,
        }),
        "crypto_public_key" => Some(WirHelperSpec {
            // No input — the host writes the seed's 64-char hex public key.
            func: crypto_hash_helper("crypto_public_key", "crypto.public_key", 64, &[]),
            helper_deps: &["ensure"],
            import_deps: &["crypto.public_key"],
            uses_heap: true,
            uses_table: false,
        }),
        "dir_read" => Some(WirHelperSpec {
            func: dir_read_helper(),
            helper_deps: &["ensure"],
            import_deps: &["dir_read_len", "fill_pending"],
            uses_heap: true,
            uses_table: false,
        }),
        "dir_list" => Some(WirHelperSpec {
            func: dir_list_helper(),
            helper_deps: &["ensure"],
            import_deps: &["dir_list_size", "write_pending_list"],
            uses_heap: true,
            uses_table: false,
        }),
        "get_env" => Some(WirHelperSpec {
            func: get_env_helper(),
            helper_deps: &["ensure"],
            import_deps: &["env_len", "env_fill"],
            uses_heap: true,
            uses_table: false,
        }),
        "replace" => Some(WirHelperSpec {
            func: replace_helper(),
            helper_deps: &["ensure", "match_at"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "str_to_int" => Some(WirHelperSpec {
            func: str_to_int_helper(),
            helper_deps: &["is_ws"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "key_eq" => Some(WirHelperSpec {
            func: key_eq_helper(),
            helper_deps: &["str_eq"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_hash" => Some(WirHelperSpec {
            func: dict_hash_helper(),
            helper_deps: &[],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_find" => Some(WirHelperSpec {
            func: dict_find_helper(),
            helper_deps: &["key_eq", "dict_hash"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_new" => Some(WirHelperSpec {
            func: dict_new_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_insert" => Some(WirHelperSpec {
            func: dict_insert_helper(),
            helper_deps: &["ensure", "dict_find"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_get_or" => Some(WirHelperSpec {
            func: dict_get_or_helper(),
            helper_deps: &["dict_find"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_has" => Some(WirHelperSpec {
            func: dict_has_helper(),
            helper_deps: &["dict_find"],
            import_deps: &[],
            uses_heap: false,
            uses_table: false,
        }),
        "dict_keys" => Some(WirHelperSpec {
            func: dict_project_helper("dict_keys", 4),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_values" => Some(WirHelperSpec {
            func: dict_project_helper("dict_values", 12),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_pairs" => Some(WirHelperSpec {
            func: dict_pairs_helper(),
            helper_deps: &["ensure"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        "dict_remove" => Some(WirHelperSpec {
            func: dict_remove_helper(),
            helper_deps: &["ensure", "key_eq"],
            import_deps: &[],
            uses_heap: true,
            uses_table: false,
        }),
        _ => {
            // `$mk0` … `$mk8`: the aggregate allocators (each calls `$ensure`).
            if let Some(rest) = name.strip_prefix("mk") {
                if let Ok(n) = rest.parse::<usize>() {
                    if n <= 8 {
                        return Some(WirHelperSpec {
                            func: mk_helper(n),
                            helper_deps: &["ensure"],
                            import_deps: &[],
                            uses_heap: true,
                            uses_table: false,
                        });
                    }
                }
            }
            None
        }
    }
}

// --- WIR → WAT pretty-printer (the only lowering in M0) ----------------------

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
/// matching `codegen.rs`'s emission style) — the bridge used during the M1
/// in-place conversion, where a `compile_expr` arm builds a `WirExpr` and splices
/// its printed instructions into the surrounding (still-string) WAT stream.
pub fn expr_to_wat(e: &WirExpr) -> String {
    let mut s = String::new();
    print_expr(&mut s, e, 2); // depth 2 == 4 spaces, codegen's flat indent
    s
}

/// Print a sequence of statement nodes as a flat instruction fragment (the M2
/// analogue of `expr_to_wat`): the bridge used while `compile_block` is being
/// converted in place — it builds a `WirSeq` and splices its printed instructions
/// into the surrounding (still-string) WAT stream.
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
mod tests {
    use super::*;

    fn local(name: &str, ty: WirTy) -> WirLocal {
        WirLocal { name: name.into(), ty }
    }

    /// Assemble a WIR module's WAT and run its `run` export, capturing `print_int`
    /// and `print` output as ordered lines.
    fn run_capture(module: &WirModule) -> Vec<String> {
        use std::sync::{Arc, Mutex};
        let wat = to_wat(module);
        let binary = wat::parse_str(&wat)
            .unwrap_or_else(|e| panic!("WIR→WAT did not assemble: {e}\n---\n{wat}"));
        let engine = wasmtime::Engine::default();
        let m = wasmtime::Module::new(&engine, &binary)
            .unwrap_or_else(|e| panic!("assembled module invalid: {e}\n---\n{wat}"));
        let out: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut linker = wasmtime::Linker::new(&engine);
        let o = out.clone();
        linker
            .func_wrap("witchy", "print_int", move |n: i64| {
                o.lock().unwrap().push(n.to_string());
            })
            .unwrap();
        let o = out.clone();
        linker
            .func_wrap(
                "witchy",
                "print",
                move |mut caller: wasmtime::Caller<'_, ()>, ptr: i32, len: i32| {
                    let mem = caller.get_export("memory").and_then(|e| e.into_memory()).unwrap();
                    let data = mem.data(&caller);
                    let s =
                        String::from_utf8_lossy(&data[ptr as usize..(ptr + len) as usize]).into_owned();
                    o.lock().unwrap().push(s);
                },
            )
            .unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let inst = linker.instantiate(&mut store, &m).expect("instantiate");
        let run = inst.get_typed_func::<(), ()>(&mut store, "run").expect("run export");
        run.call(&mut store, ()).expect("run");
        let v = out.lock().unwrap().clone();
        v
    }

    /// Module with one Int-returning func + a `run` that prints its result.
    fn int_demo(f: WirFunc, call: WirExpr) -> WirModule {
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![call],
            })],
            raw_body: None,
        };
        WirModule {
            imports: vec![
                WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] },
                WirImport {
                    name: "print".into(),
                    params: vec![Kind::I32, Kind::I32],
                    results: vec![],
                },
            ],
            funcs: vec![f, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        }
    }

    #[test]
    fn arithmetic_roundtrips() {
        // fn add() -> Int: (2 + 3) * 4   == 20
        let add = WirFunc {
            name: "add".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Binary {
                op: BinOp::Mul,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::ConstI64(2)),
                    rhs: Box::new(WirExpr::ConstI64(3)),
                }),
                rhs: Box::new(WirExpr::ConstI64(4)),
            }))],
            raw_body: None,
        };
        let m = int_demo(add, WirExpr::Call { func: "add".into(), args: vec![] });
        assert_eq!(run_capture(&m), vec!["20"]);
    }

    #[test]
    fn if_with_result_roundtrips() {
        // fn pick(b: Bool) -> Int: if b: 10 else: 20  (each arm returns)
        let pick = WirFunc {
            name: "pick".into(),
            params: vec![local("b", WirTy::Bool)],
            ret: vec![WirTy::Int],
            locals: vec![],
            // value-`if`: each branch leaves an i64; the if's value is the result.
            body: vec![WirNode::If {
                cond: WirExpr::GetLocal("b".into()),
                then_: vec![WirNode::Push(WirExpr::ConstI64(10))],
                els: vec![WirNode::Push(WirExpr::ConstI64(20))],
                result: Some(WirTy::Int),
            }],
            raw_body: None,
        };
        let m_true = int_demo(
            pick.clone(),
            WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(1)] },
        );
        assert_eq!(run_capture(&m_true), vec!["10"]);
        let m_false =
            int_demo(pick, WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(0)] });
        assert_eq!(run_capture(&m_false), vec!["20"]);
    }

    #[test]
    fn loop_spine_roundtrips() {
        // fn sum_to(n: Int) -> Int:   (sum of 0..n)
        //   var total = 0; var i = 0
        //   block $exit: loop $head:
        //     br $exit if !(i < n); total += i; i += 1; br $head
        //   total
        let i_lt_n = WirExpr::Binary {
            op: BinOp::Lt,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("i".into())),
            rhs: Box::new(WirExpr::GetLocal("n".into())),
        };
        let not_i_lt_n = WirExpr::Binary {
            op: BinOp::Eq,
            kind: Kind::I32,
            lhs: Box::new(i_lt_n),
            rhs: Box::new(WirExpr::ConstI32(0)),
        };
        let sum_to = WirFunc {
            name: "sum_to".into(),
            params: vec![local("n", WirTy::Int)],
            ret: vec![WirTy::Int],
            locals: vec![local("total", WirTy::Int), local("i", WirTy::Int)],
            body: vec![
                WirNode::SetLocal { local: "total".into(), value: WirExpr::ConstI64(0) },
                WirNode::SetLocal { local: "i".into(), value: WirExpr::ConstI64(0) },
                WirNode::Block {
                    label: "exit".into(),
                    result: None,
                    body: vec![WirNode::Loop {
                        label: "head".into(),
                        body: vec![
                            WirNode::Br { target: "exit".into(), cond: Some(not_i_lt_n) },
                            WirNode::SetLocal {
                                local: "total".into(),
                                value: WirExpr::Binary {
                                    op: BinOp::Add,
                                    kind: Kind::I64,
                                    lhs: Box::new(WirExpr::GetLocal("total".into())),
                                    rhs: Box::new(WirExpr::GetLocal("i".into())),
                                },
                            },
                            WirNode::SetLocal {
                                local: "i".into(),
                                value: WirExpr::Binary {
                                    op: BinOp::Add,
                                    kind: Kind::I64,
                                    lhs: Box::new(WirExpr::GetLocal("i".into())),
                                    rhs: Box::new(WirExpr::ConstI64(1)),
                                },
                            },
                            WirNode::Br { target: "head".into(), cond: None },
                        ],
                    }],
                },
                WirNode::Return(Some(WirExpr::GetLocal("total".into()))),
            ],
            raw_body: None,
        };
        // sum 0..5 = 0+1+2+3+4 = 10
        let m = int_demo(
            sum_to,
            WirExpr::Call { func: "sum_to".into(), args: vec![WirExpr::ConstI64(5)] },
        );
        assert_eq!(run_capture(&m), vec!["10"]);
    }

    #[test]
    fn slot_conversions_roundtrip() {
        // FromSlot(ToSlot(x, k), k) == x for each Kind — the conversion nodes the
        // headline optimization (§3.2) will cancel.
        for (kind, value, expect) in [
            (Kind::I64, WirExpr::ConstI64(42), "42"),
            // F64 5.0 reinterpreted both ways, then truncated to i64 for print.
        ] {
            let f = WirFunc {
                name: "rt".into(),
                params: vec![],
                ret: vec![WirTy::Int],
                locals: vec![],
                body: vec![WirNode::Return(Some(WirExpr::FromSlot(
                    Box::new(WirExpr::ToSlot(Box::new(value), kind)),
                    kind,
                )))],
                raw_body: None,
            };
            let m = int_demo(f, WirExpr::Call { func: "rt".into(), args: vec![] });
            assert_eq!(run_capture(&m), vec![expect.to_string()]);
        }
    }

    #[test]
    fn unary_ops_roundtrip() {
        // fn neg() -> Int: -(5) == -5  (exercises the `0 - x` operand ordering)
        let neg = WirFunc {
            name: "neg".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Unary {
                op: UnOp::Neg,
                kind: Kind::I64,
                arg: Box::new(WirExpr::ConstI64(5)),
            }))],
            raw_body: None,
        };
        let m = int_demo(neg, WirExpr::Call { func: "neg".into(), args: vec![] });
        assert_eq!(run_capture(&m), vec!["-5"]);

        // fn bnot() -> Int: ~0 == -1  (x ^ -1)
        let bnot = WirFunc {
            name: "bnot".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Unary {
                op: UnOp::BitNot,
                kind: Kind::I64,
                arg: Box::new(WirExpr::ConstI64(0)),
            }))],
            raw_body: None,
        };
        let m = int_demo(bnot, WirExpr::Call { func: "bnot".into(), args: vec![] });
        assert_eq!(run_capture(&m), vec!["-1"]);
    }

    #[test]
    fn control_value_if_roundtrips() {
        // A value-`if` in *expression* position (the `&&`/`||` and if-expr shape):
        // fn pick(b) -> Int: return (if b { 10 } else { 20 })
        let pick = WirFunc {
            name: "pick".into(),
            params: vec![local("b", WirTy::Bool)],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Control(Box::new(WirNode::If {
                cond: WirExpr::GetLocal("b".into()),
                then_: vec![WirNode::Push(WirExpr::ConstI64(10))],
                els: vec![WirNode::Push(WirExpr::ConstI64(20))],
                result: Some(WirTy::Int),
            }))))],
            raw_body: None,
        };
        let m_true = int_demo(
            pick.clone(),
            WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(1)] },
        );
        assert_eq!(run_capture(&m_true), vec!["10"]);
        let m_false =
            int_demo(pick, WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(0)] });
        assert_eq!(run_capture(&m_false), vec!["20"]);
    }

    #[test]
    fn string_print_roundtrips() {
        // A self-contained `print(console, "hi")`: data `[2,0,0,0,'h','i']` at
        // offset 8; print is called with (ptr+4, load_len). Exercises StrPtr,
        // Load, and a void CallHost.
        let mut bytes = (2u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(b"hi");
        let print_call = WirExpr::CallHost {
            import: "print".into(),
            args: vec![
                WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::StrPtr(8)),
                    rhs: Box::new(WirExpr::ConstI32(4)),
                },
                WirExpr::Load { ptr: Box::new(WirExpr::StrPtr(8)), kind: Kind::I32, offset: 0 },
            ],
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(print_call)],
            raw_body: None,
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print".into(),
                params: vec![Kind::I32, Kind::I32],
                results: vec![],
            }],
            funcs: vec![run],
            memory_pages: 1,
            data: vec![DataSegment { offset: 8, bytes }],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_eq!(run_capture(&m), vec!["hi"]);
    }
}
