//! WIR, the Witchy IR: the compiled backend's representation. See
//! `rfcs/wir-design.md` for the design rationale.
//!
//! WIR is a structured, witchy-typed value tree (in the style of Binaryen IR),
//! not an SSA/CFG. Control flow is nested `Block`/`Loop`/`If`/`Br` nodes whose
//! branch targets are always lexically-enclosing labels, so lowering to wasm is
//! a direct structural walk with no relooper. Expressions are typed nodes, each
//! carrying a `WirTy`. Scalar values retain the universal i64-slot model while
//! reference kinds remain exact throughout the tree.
//!
//! Locals, labels, and functions are referred to by name; `wir_encode` resolves
//! those names to relative branch depths and wasm indices when it emits the
//! binary. This file is the IR data model plus a `WIR` to WAT text printer used
//! for debugging and test assertions; the runtime-helper library lives in
//! `wir_helpers` and the binary emitter in `wir_encode`.

use std::fmt::Write;

/// The maximum closure arity the static prelude pre-declares (`$clos0..$clos4`).
/// The binary encoder reserves type indices `0..=MAX_CLOS` for these signatures
/// BEFORE any import/func type, because spliced prelude raw bodies bake those
/// `call_indirect (type $closN)` type indices. MUST equal `wir_prelude::MAX_CLOS`.
pub const MAX_CLOS: usize = 4;

/// The wasm-level representation a value is carried as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    I32,
    I64,
    F64,
    /// (RFC-0005) A wasm `externref`: an unforgeable, opaque host reference. It
    /// lives ONLY in locals/params/results/globals/tables and GC struct fields —
    /// never in linear memory, and there is no `i32 -> externref` cast — so a
    /// capability carried as an `externref` cannot be forged or swapped by a
    /// linear-memory corruption. This is the representation the capability core
    /// moves grants onto (File first; see `rfcs/externref-implementation-plan.md`).
    ExternRef,
    /// The abstract wasm GC heap type `(ref null struct)`. This is the uniform
    /// erased type used by the closure wrapper's optional GC environment field;
    /// a lifted lambda casts it back to its statically known payload struct
    /// before reading captures.
    StructRef,
    /// The abstract wasm GC heap type `(ref null any)`. This is the erased
    /// carrier for values that may be either a GC struct or GC array, such as
    /// a channel message whose source type is `List(T)`.
    AnyRef,
    /// (RFC-0005) A typed concrete GC reference `(ref null $t)`, where the `u32`
    /// is the module's GC type-definition index (structs first, then arrays).
    /// This is the representation of a reference-carrying aggregate, so an
    /// `externref` or nested GC reference never decays into linear memory.
    GcRef(u32),
}

/// The access capability carried by an opt-mode place reference.
///
/// This is deliberately separate from [`Kind`]. A place reference can be
/// represented by a pointer, a shadow cell, or a typed GC object; the access
/// capability and the referent's value representation must survive those
/// lowering choices unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlaceAccess {
    Shared,
    Exclusive,
}

/// One runtime projection from a place-reference root.
///
/// Dynamic coordinates name an already-evaluated WIR local. This makes the
/// reference descriptor preserve evaluation order instead of re-running an
/// index expression when a read, write, reborrow, or close occurs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlaceProjection {
    Field(String),
    Tuple(u32),
    Index { coordinate: String },
    Range { lo: String, hi: String, inclusive: bool },
}

/// The runtime identity of a first-class reference after source lifetimes have
/// been checked. Lifetimes have no runtime payload; this descriptor retains the
/// root and evaluated projection needed to read, write, reborrow, or close the
/// reference without treating an interior address as an owning root.
///
/// `root` is a lowering-owned stable location name. Backends choose its physical
/// representation (direct place, owner-backed shadow, or GC cell), while every
/// caller uses the same logical identity and projection path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlaceReference {
    pub root: String,
    pub projections: Vec<PlaceProjection>,
    pub access: PlaceAccess,
}

impl PlaceReference {
    pub fn new(root: impl Into<String>, access: PlaceAccess) -> Self {
        Self { root: root.into(), projections: Vec::new(), access }
    }

    /// Derive a child reference without changing the owner root or access
    /// capability. Checker facts decide whether the requested reborrow is legal;
    /// WIR only preserves the already-proven logical place.
    pub fn reborrow(&self, projections: impl IntoIterator<Item = PlaceProjection>) -> Self {
        let mut child = self.clone();
        child.projections.extend(projections);
        child
    }
}

/// The exact wasm function type used by a closure `call_indirect`.
///
/// Source closures use [`gc_slot_closure_signature`] or a fully typed variant;
/// [`slot_closure_signature`] remains for legacy prelude helpers. Keeping the
/// kinds here is essential because arity alone cannot distinguish `i64`,
/// `externref`, or GC-reference operands.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClosureSignature {
    pub params: Vec<Kind>,
    pub results: Vec<Kind>,
}

pub fn slot_closure_signature(arity: usize, result_count: usize) -> ClosureSignature {
    let mut params = vec![Kind::I32];
    params.extend(std::iter::repeat_n(Kind::I64, arity));
    ClosureSignature { params, results: vec![Kind::I64; result_count] }
}

/// The scalar-value signature for a closure carried by the uniform RFC-0005 GC
/// wrapper. Arguments and results retain the established i64 slot ABI; only the
/// implicit environment changes from a forgeable linear-memory pointer to the
/// concrete wrapper reference at GC type index zero.
pub fn gc_slot_closure_signature(arity: usize, result_count: usize) -> ClosureSignature {
    let mut params = vec![Kind::GcRef(0)];
    params.extend(std::iter::repeat_n(Kind::I64, arity));
    ClosureSignature { params, results: vec![Kind::I64; result_count] }
}

impl Kind {
    /// The WAT spelling. Reference kinds are approximate here — the WAT printer is
    /// debug-only and never round-trips the GC type section (the binary encoder in
    /// `wir_encode` is authoritative for reference/GC-struct types).
    pub fn wat(self) -> &'static str {
        match self {
            Kind::I32 => "i32",
            Kind::I64 => "i64",
            Kind::F64 => "f64",
            Kind::ExternRef => "externref",
            Kind::StructRef => "(ref null struct)",
            Kind::AnyRef => "(ref null any)",
            Kind::GcRef(_) => "(ref null $gc)",
        }
    }

    /// True for the reference kinds (`externref`, GC struct refs) — the values
    /// that are NOT in linear memory and cannot be arithmetic'd, loaded/stored, or
    /// boxed into the i64 slot. Callers use this to assert a value never reaches a
    /// scalar-only path (the i64 Slot boundary is a `typeck` reject, so hitting one
    /// of those paths at runtime is a compiler bug, not a program error).
    pub fn is_ref(self) -> bool {
        matches!(self, Kind::ExternRef | Kind::StructRef | Kind::AnyRef | Kind::GcRef(_))
    }
}

/// The witchy-level type, retained alongside the wasm `Kind` so passes stay
/// shape-aware (structural equality, capability handles, slot conversions).
#[derive(Debug, Clone, PartialEq)]
pub enum WirTy {
    Int,
    Float,
    Bool,
    Str,
    Unit,
    Capability,
    List(Box<WirTy>),
    /// The universal untyped i64 slot (generic/monomorphized boundaries).
    Slot,
    /// (RFC-0005) A capability carried as a bare `externref` — the unforgeable
    /// representation a migrated capability (File first) takes in a local, param,
    /// result, or host-import argument. Distinct from the legacy `Capability`
    /// (an i32 handle in linear memory), which stays until its capability's stage.
    Extern,
    /// An erased nullable GC struct reference (`structref`).
    StructRef,
    /// An erased nullable GC reference that may name a struct or array.
    AnyRef,
    /// (RFC-0005) A reference-carrying aggregate lowered to a concrete GC type,
    /// referenced by its module GC-definition index. Named capability-bearing
    /// records use structs; reference-bearing collections use arrays.
    GcRef(u32),
}

impl WirTy {
    /// The wasm representation this type is carried as.
    pub fn kind(&self) -> Kind {
        match self {
            WirTy::Int => Kind::I64,
            WirTy::Float => Kind::F64,
            WirTy::Slot => Kind::I64,
            WirTy::Extern => Kind::ExternRef,
            WirTy::StructRef => Kind::StructRef,
            WirTy::AnyRef => Kind::AnyRef,
            WirTy::GcRef(id) => Kind::GcRef(*id),
            // Bool, Str (ptr), Unit, Capability (handle/placeholder), List (ptr).
            _ => Kind::I32,
        }
    }
}

/// (RFC-0005) A GC struct type declaration for a cap-carrying aggregate. Fields
/// are ordered; each field's `Kind` is its wasm representation (a scalar, an
/// `externref` for a nested capability, or a `GcRef` for a nested aggregate). The
/// encoder lays these after the reserved scalar closure-signature band, so a
/// `Kind::GcRef(i)` resolves to the corresponding concrete GC type index.
/// Aggregate payload structs are mutable; identity wrappers such as closures
/// are immutable after construction.
#[derive(Debug, Clone, PartialEq)]
pub struct WirStructDef {
    pub fields: Vec<Kind>,
    pub mutable: bool,
}

/// A mutable GC array type declaration. Arrays share the concrete GC type-index
/// space with structs: `GcRef(structs.len() + array_id)` names array `array_id`.
/// The element kind may itself be a reference, which is the substrate required
/// for `List(fn(...))` once closures move to the uniform GC wrapper.
#[derive(Debug, Clone, PartialEq)]
pub struct WirArrayDef {
    pub element: Kind,
}

/// Field indices and layout for the uniform first-class closure wrapper used by
/// RFC-0005 Stage 4. Every source function value uses this wrapper. `linear_env`
/// is reserved as zero for ABI stability; boxed captures live in a per-lambda
/// typed GC payload behind the erased `gc_env` reference.
pub const CLOSURE_CODE_FIELD: u32 = 0;
pub const CLOSURE_LINEAR_ENV_FIELD: u32 = 1;
pub const CLOSURE_GC_ENV_FIELD: u32 = 2;

pub fn closure_wrapper_struct() -> WirStructDef {
    WirStructDef {
        fields: vec![Kind::I32, Kind::I32, Kind::StructRef],
        mutable: false,
    }
}

/// RFC-0081's backend-neutral existential envelope. The payload is an erased
/// reference to a separately generated, concretely typed GC struct; the witness
/// is a closed-program table index. Payload fields never cross the scalar slot
/// ABI merely because the envelope erases their concrete struct identity.
pub const EXISTENTIAL_PAYLOAD_FIELD: u32 = 0;
pub const EXISTENTIAL_WITNESS_FIELD: u32 = 1;

pub fn existential_wrapper_struct() -> WirStructDef {
    WirStructDef {
        fields: vec![Kind::StructRef, Kind::I32],
        mutable: false,
    }
}

/// Erased channel-message envelope. Source endpoint types recover the exact
/// field statically, while the scheduler carries one stable GC reference ABI
/// across channels with different scalar, host-reference, and GC-reference
/// message types.
pub const MESSAGE_I32_FIELD: u32 = 0;
pub const MESSAGE_I64_FIELD: u32 = 1;
pub const MESSAGE_F64_FIELD: u32 = 2;
pub const MESSAGE_EXTERNREF_FIELD: u32 = 3;
pub const MESSAGE_ANYREF_FIELD: u32 = 4;

pub fn message_wrapper_struct() -> WirStructDef {
    WirStructDef {
        fields: vec![
            Kind::I32,
            Kind::I64,
            Kind::F64,
            Kind::ExternRef,
            Kind::AnyRef,
        ],
        mutable: false,
    }
}

/// The executable representation of an `&'a Int` / `&'a mut Int` in opt mode.
/// The cell owns the current scalar payload; a reference is the typed GC handle
/// itself, so it survives calls and `call_indirect` without relying on a
/// caller-local address being recoverable after lowering.
pub fn reference_i64_cell_struct() -> WirStructDef {
    WirStructDef {
        fields: vec![Kind::I64],
        mutable: true,
    }
}

/// Uniform ABI wrapper for an executable opt-mode reference. `root` is erased
/// so one reference type can cross a function-value boundary regardless of the
/// owner's concrete representation; `projection` selects the checked path.
pub fn place_reference_struct() -> WirStructDef {
    WirStructDef {
        fields: vec![Kind::StructRef, Kind::I32],
        mutable: false,
    }
}

/// Owner cell for an aggregate still represented by a linear-memory pointer.
/// It shares the uniform PlaceReference wrapper with scalar and GC roots.
pub fn reference_i32_cell_struct() -> WirStructDef {
    WirStructDef {
        fields: vec![Kind::I32],
        mutable: true,
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
    /// `f64 -> i64` saturating truncation (`i64.trunc_sat_f64_s`). `math.to_int`
    /// uses the `float_to_int` helper so NaN can abort before this conversion.
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
    /// An indirect call through table 0, used for closures. `signature` is the
    /// exact wasm function type. `args` are pushed first (the environment then
    /// source arguments, in order); `index` is pushed last, matching wasm's
    /// `call_indirect` operand order.
    CallIndirect {
        signature: ClosureSignature,
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

    /// (RFC-0005) `struct.new $s` — allocate a cap-carrying GC struct. `args` are
    /// pushed in field order (each already the field's `Kind`), leaving a
    /// `(ref $s)` on the stack. `struct_id` is the module struct-definition index.
    StructNew {
        struct_id: u32,
        args: Vec<WirExpr>,
    },
    /// (RFC-0005) `struct.get $s $field` — read field `field` of the GC struct
    /// `base` (a `(ref null $s)`), leaving the field's value. Replaces a linear
    /// `Load` (byte offset) with a GC field index for cap-carrying aggregates.
    StructGet {
        struct_id: u32,
        field: u32,
        base: Box<WirExpr>,
    },
    /// Allocate an array of `len` elements, each initialized to `value`.
    ArrayNew {
        array_id: u32,
        value: Box<WirExpr>,
        len: Box<WirExpr>,
    },
    /// Allocate an array initialized from the fixed element sequence.
    ArrayNewFixed {
        array_id: u32,
        items: Vec<WirExpr>,
    },
    /// Read `array[index]`. Wasm performs the bounds check and traps on failure.
    ArrayGet {
        array_id: u32,
        array: Box<WirExpr>,
        index: Box<WirExpr>,
    },
    /// Return an array's length as `i32`.
    ArrayLen(Box<WirExpr>),
    /// Cast an erased `structref` to a concrete non-null GC struct reference.
    /// The compiler emits this only for the payload layout assigned to the
    /// lifted lambda, so a trap means an internal closure-layout mismatch.
    RefCast {
        struct_id: u32,
        value: Box<WirExpr>,
    },
    /// Cast a reference to a concrete nullable GC struct reference. This is
    /// used at nullable carrier boundaries such as `Option(Some(value))`,
    /// where construction produces `(ref $t)` but the source-level value is
    /// represented as `(ref null $t)`.
    RefCastNullable {
        struct_id: u32,
        value: Box<WirExpr>,
    },
    /// (RFC-0005) `ref.null` of a reference kind (`externref` or a concrete GC
    /// struct ref). The null initializer for a not-yet-populated cap slot.
    RefNull(Kind),
    /// (RFC-0005) `ref.is_null`, used by nullable-externref encodings such as
    /// `Option(Dir)` while Dir is represented as an externref.
    RefIsNull(Box<WirExpr>),
}

/// A statement-level node: executes for effect and/or leaves a typed value.
/// Control flow is nested here; branch targets are lexically-enclosing labels.
#[derive(Debug, Clone)]
pub enum WirNode {
    /// Compiler-owned source attribution for the enclosed WIR sequence. The
    /// wrapper emits no Wasm instruction; the development encoder records the
    /// exact instruction-ordinal interval produced by `body`. A zero line is
    /// never constructed for source code.
    Source {
        line: u32,
        body: WirSeq,
    },
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
    /// The indirect-call counterpart of `CallStoreMulti`.
    CallIndirectStoreMulti {
        signature: ClosureSignature,
        args: Vec<WirExpr>,
        index: WirExpr,
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
    /// (RFC-0005) `struct.set $s $field` — write `value` into field `field` of the
    /// GC struct `base` (a `(ref null $s)`). The GC-field analogue of `Store`.
    StructSet {
        struct_id: u32,
        field: u32,
        base: WirExpr,
        value: WirExpr,
    },
    /// Write `value` to `array[index]`. Wasm performs the bounds check.
    ArraySet {
        array_id: u32,
        array: WirExpr,
        index: WirExpr,
        value: WirExpr,
    },
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
    F64(f64),
}

impl GlobalInit {
    /// The `Kind` (wasm representation) of this initializer.
    pub fn kind(self) -> Kind {
        match self {
            GlobalInit::I32(_) => Kind::I32,
            GlobalInit::I64(_) => Kind::I64,
            GlobalInit::F64(_) => Kind::F64,
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

impl WirFunc {
    /// Prune declared body locals that are never read or written in the function body.
    /// Parameters are always preserved.
    pub fn prune_unused_locals(&mut self) {
        if self.raw_body.is_some() {
            return;
        }
        let mut used = std::collections::HashSet::new();
        collect_used_locals_seq(&self.body, &mut used);
        self.locals.retain(|l| used.contains(l.name.as_str()));
    }
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

impl WirModule {
    /// Prune unused locals across all defined functions in the module.
    pub fn prune_unused_locals(&mut self) {
        for func in &mut self.funcs {
            func.prune_unused_locals();
        }
    }
}

/// Render a module to WAT text, for debugging and test assertions (`witchy
/// emit-wat`). This is not the build path: `wir_encode` emits the wasm binary
/// directly. Scalar modules are kept instruction-for-instruction aligned. GC
/// reference kinds remain approximate because this API does not receive the
/// module's external `WirStructDef` list; the binary encoder is authoritative
/// for GC modules.
pub fn to_wat(module: &WirModule) -> String {
    let mut module_clone;
    let module = if module.funcs.iter().any(|f| f.raw_body.is_none()) {
        module_clone = module.clone();
        module_clone.prune_unused_locals();
        &module_clone
    } else {
        module
    };
    let mut s = String::new();
    s.push_str("(module\n");

    // Closure type declarations for any `call_indirect` in the body.
    let mut clos_signatures: Vec<ClosureSignature> = Vec::new();
    for f in &module.funcs {
        collect_clos_signatures_seq(&f.body, &mut clos_signatures);
    }
    clos_signatures.sort_unstable();
    for signature in &clos_signatures {
        let params: String = signature
            .params
            .iter()
            .map(|kind| format!("(param {}) ", kind.wat()))
            .collect();
        let results: String = signature
            .results
            .iter()
            .map(|kind| format!("(result {})", kind.wat()))
            .collect();
        let _ = writeln!(
            s,
            "  (type ${} (func {params}{results}))",
            clos_type_name(signature)
        );
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
        GlobalInit::F64(n) => format!("(f64.const {n})"),
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
// keywords: a flat 4-space layout where the wasm structure comes from the
// keywords, not the indentation.
fn print_node(s: &mut String, node: &WirNode, depth: usize) {
    match node {
        WirNode::Source { body, .. } => print_seq(s, body, depth),
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
        WirNode::CallIndirectStoreMulti {
            signature,
            args,
            index,
            dests,
        } => {
            for a in args {
                print_expr(s, a, depth);
            }
            print_expr(s, index, depth);
            indent(s, depth);
            let _ = writeln!(
                s,
                "call_indirect (type ${})",
                clos_type_name(signature)
            );
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
        WirNode::Drop(e) => match e {
            WirExpr::ConstI32(_)
            | WirExpr::ConstI64(_)
            | WirExpr::ConstF64(_)
            | WirExpr::StrPtr(_)
            | WirExpr::RefNull(_) => {}
            _ => {
                print_expr(s, e, depth);
                indent(s, depth);
                s.push_str("drop\n");
            }
        },
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
        WirNode::StructSet { struct_id, field, base, value } => {
            print_expr(s, base, depth);
            print_expr(s, value, depth);
            indent(s, depth);
            let _ = writeln!(s, "struct.set {struct_id} {field}");
        }
        WirNode::ArraySet { array_id, array, index, value } => {
            print_expr(s, array, depth);
            print_expr(s, index, depth);
            print_expr(s, value, depth);
            indent(s, depth);
            let _ = writeln!(s, "array.set {array_id}");
        }
    }
}

fn print_expr(s: &mut String, e: &WirExpr, depth: usize) {
    match e {
        WirExpr::ConstI64(n) => emit(s, depth, &format!("i64.const {n}")),
        WirExpr::ConstI32(n) => emit(s, depth, &format!("i32.const {n}")),
        // Plain `{x}` Display: WAT infers f64 from the `f64.const` mnemonic, so a
        // whole-number `5` needs no `.0`. (The binary encoder writes the bits
        // directly and does not go through this text path.)
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
            signature,
            args,
            index,
        } => {
            // Args pushed first, then the code index, then `call_indirect` —
            // byte-identical to codegen.
            for a in args {
                print_expr(s, a, depth);
            }
            print_expr(s, index, depth);
            emit(
                s,
                depth,
                &format!(
                    "call_indirect (type ${})",
                    clos_type_name(signature)
                ),
            );
        }
        WirExpr::Control(node) => print_node(s, node, depth),
        WirExpr::Seq(nodes) => print_seq(s, nodes, depth),
        WirExpr::StructNew { struct_id, args } => {
            for a in args {
                print_expr(s, a, depth);
            }
            indent(s, depth);
            let _ = writeln!(s, "struct.new {struct_id}");
        }
        WirExpr::StructGet { struct_id, field, base } => {
            print_expr(s, base, depth);
            indent(s, depth);
            let _ = writeln!(s, "struct.get {struct_id} {field}");
        }
        WirExpr::ArrayNew { array_id, value, len } => {
            print_expr(s, value, depth);
            print_expr(s, len, depth);
            emit(s, depth, &format!("array.new {array_id}"));
        }
        WirExpr::ArrayNewFixed { array_id, items } => {
            for item in items {
                print_expr(s, item, depth);
            }
            emit(s, depth, &format!("array.new_fixed {array_id} {}", items.len()));
        }
        WirExpr::ArrayGet { array_id, array, index } => {
            print_expr(s, array, depth);
            print_expr(s, index, depth);
            emit(s, depth, &format!("array.get {array_id}"));
        }
        WirExpr::ArrayLen(array) => {
            print_expr(s, array, depth);
            emit(s, depth, "array.len");
        }
        WirExpr::RefCast { struct_id, value } => {
            print_expr(s, value, depth);
            emit(s, depth, &format!("ref.cast (ref {struct_id})"));
        }
        WirExpr::RefCastNullable { struct_id, value } => {
            print_expr(s, value, depth);
            emit(s, depth, &format!("ref.cast (ref null {struct_id})"));
        }
        WirExpr::RefNull(kind) => {
            let heap = match kind {
                Kind::ExternRef => "extern".to_string(),
                Kind::StructRef => "struct".to_string(),
                Kind::AnyRef => "any".to_string(),
                Kind::GcRef(id) => format!("{id}"),
                _ => "extern".to_string(),
            };
            emit(s, depth, &format!("ref.null {heap}"));
        }
        WirExpr::RefIsNull(expr) => {
            print_expr(s, expr, depth);
            emit(s, depth, "ref.is_null");
        }
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
        // (RFC-0005) A reference has no i64 bit-pattern, so it cannot enter the
        // universal slot. Reaching here means the i64 Slot-boundary `typeck`
        // reject (§4.4) was bypassed — a compiler bug, not a program error.
        Kind::ExternRef | Kind::StructRef | Kind::AnyRef | Kind::GcRef(_) => {
            panic!("cannot box a reference-typed value (a capability) into the i64 slot")
        }
    }
}

/// from-slot conversion: universal i64 slot back to a value of `kind`.
fn from_slot_op(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::I64 => None,
        Kind::I32 => Some("i32.wrap_i64"),
        Kind::F64 => Some("f64.reinterpret_i64"),
        Kind::ExternRef | Kind::StructRef | Kind::AnyRef | Kind::GcRef(_) => {
            panic!("cannot recover a reference-typed value (a capability) from the i64 slot")
        }
    }
}

fn clos_type_name(signature: &ClosureSignature) -> String {
    if signature.params.first() == Some(&Kind::I32)
        && signature.params[1..].iter().all(|kind| *kind == Kind::I64)
        && signature.results.iter().all(|kind| *kind == Kind::I64)
    {
        let arity = signature.params.len() - 1;
        return if signature.results.len() == 1 {
            format!("clos{arity}")
        } else {
            format!("clos{arity}r{}", signature.results.len())
        };
    }

    fn token(kind: Kind) -> String {
        match kind {
            Kind::I32 => "i32".into(),
            Kind::I64 => "i64".into(),
            Kind::F64 => "f64".into(),
            Kind::ExternRef => "externref".into(),
            Kind::StructRef => "structref".into(),
            Kind::AnyRef => "anyref".into(),
            Kind::GcRef(id) => format!("gc{id}"),
        }
    }
    let params = signature.params.iter().map(|kind| token(*kind)).collect::<Vec<_>>().join("_");
    let results = signature.results.iter().map(|kind| token(*kind)).collect::<Vec<_>>().join("_");
    format!("clos_t_{params}_r_{results}")
}

/// Collect the distinct closure signatures referenced by indirect calls, for
/// the type declarations both emitters synthesize: the WAT printer's type
/// section and the binary encoder's (one walker, two callers — they must
/// never diverge).
pub(crate) fn collect_clos_signatures_seq(seq: &WirSeq, out: &mut Vec<ClosureSignature>) {
    fn push(out: &mut Vec<ClosureSignature>, signature: &ClosureSignature) {
        if !out.contains(signature) {
            out.push(signature.clone());
        }
    }
    fn walk_expr(e: &WirExpr, out: &mut Vec<ClosureSignature>) {
        match e {
            WirExpr::CallIndirect {
                signature,
                args,
                index,
            } => {
                push(out, signature);
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
            WirExpr::Seq(nodes) => collect_clos_signatures_seq(nodes, out),
            WirExpr::StructNew { args, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
            }
            WirExpr::ArrayNew { value, len, .. } => {
                walk_expr(value, out);
                walk_expr(len, out);
            }
            WirExpr::ArrayNewFixed { items, .. } => {
                for item in items {
                    walk_expr(item, out);
                }
            }
            WirExpr::ArrayGet { array, index, .. } => {
                walk_expr(array, out);
                walk_expr(index, out);
            }
            WirExpr::StructGet { base, .. }
            | WirExpr::RefCast { value: base, .. }
            | WirExpr::RefCastNullable { value: base, .. }
            | WirExpr::ArrayLen(base)
            | WirExpr::RefIsNull(base) => walk_expr(base, out),
            WirExpr::ConstI64(_)
            | WirExpr::ConstF64(_)
            | WirExpr::ConstI32(_)
            | WirExpr::StrPtr(_)
            | WirExpr::MemorySize
            | WirExpr::GetLocal(_)
            | WirExpr::GetGlobal(_)
            | WirExpr::RefNull(_) => {}
        }
    }
    fn walk_node(node: &WirNode, out: &mut Vec<ClosureSignature>) {
        match node {
            WirNode::Source { body, .. } => collect_clos_signatures_seq(body, out),
            WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => {
                walk_expr(value, out)
            }
            WirNode::StructSet { base, value, .. } => {
                walk_expr(base, out);
                walk_expr(value, out);
            }
            WirNode::ArraySet { array, index, value, .. } => {
                walk_expr(array, out);
                walk_expr(index, out);
                walk_expr(value, out);
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
            WirNode::CallIndirectStoreMulti {
                signature,
                args,
                index,
                dests: _,
            } => {
                push(out, signature);
                for a in args {
                    walk_expr(a, out);
                }
                walk_expr(index, out);
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
                collect_clos_signatures_seq(then_, out);
                collect_clos_signatures_seq(els, out);
            }
            WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
                collect_clos_signatures_seq(body, out)
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

/// Collect all local variable names read or written in `seq`.
pub(crate) fn collect_used_locals_seq(seq: &WirSeq, out: &mut std::collections::HashSet<String>) {
    fn walk_expr(e: &WirExpr, out: &mut std::collections::HashSet<String>) {
        match e {
            WirExpr::GetLocal(name) => {
                out.insert(name.clone());
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
            WirExpr::CallIndirect { args, index, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
                walk_expr(index, out);
            }
            WirExpr::Control(node) => walk_node(node, out),
            WirExpr::Seq(nodes) => collect_used_locals_seq(nodes, out),
            WirExpr::StructNew { args, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
            }
            WirExpr::ArrayNew { value, len, .. } => {
                walk_expr(value, out);
                walk_expr(len, out);
            }
            WirExpr::ArrayNewFixed { items, .. } => {
                for item in items {
                    walk_expr(item, out);
                }
            }
            WirExpr::ArrayGet { array, index, .. } => {
                walk_expr(array, out);
                walk_expr(index, out);
            }
            WirExpr::StructGet { base, .. }
            | WirExpr::RefCast { value: base, .. }
            | WirExpr::RefCastNullable { value: base, .. }
            | WirExpr::ArrayLen(base)
            | WirExpr::RefIsNull(base) => walk_expr(base, out),
            WirExpr::ConstI64(_)
            | WirExpr::ConstF64(_)
            | WirExpr::ConstI32(_)
            | WirExpr::StrPtr(_)
            | WirExpr::MemorySize
            | WirExpr::GetGlobal(_)
            | WirExpr::RefNull(_) => {}
        }
    }
    fn walk_node(node: &WirNode, out: &mut std::collections::HashSet<String>) {
        match node {
            WirNode::Source { body, .. } => collect_used_locals_seq(body, out),
            WirNode::SetLocal { local, value } => {
                out.insert(local.clone());
                walk_expr(value, out);
            }
            WirNode::SetGlobal { value, .. } => {
                walk_expr(value, out);
            }
            WirNode::StructSet { base, value, .. } => {
                walk_expr(base, out);
                walk_expr(value, out);
            }
            WirNode::ArraySet { array, index, value, .. } => {
                walk_expr(array, out);
                walk_expr(index, out);
                walk_expr(value, out);
            }
            WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
                walk_expr(ptr, out);
                walk_expr(value, out);
            }
            WirNode::CallStoreMulti { args, dests, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
                for d in dests {
                    out.insert(d.clone());
                }
            }
            WirNode::CallIndirectStoreMulti {
                signature: _,
                args,
                index,
                dests,
            } => {
                for a in args {
                    walk_expr(a, out);
                }
                walk_expr(index, out);
                for d in dests {
                    out.insert(d.clone());
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
                collect_used_locals_seq(then_, out);
                collect_used_locals_seq(els, out);
            }
            WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
                collect_used_locals_seq(body, out);
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

/// (RFC-0051 I2) The single-allocator structural invariant: exactly ONE construct
/// may ADVANCE the `$heap` global — `$bump_alloc`, which is `$ensure`-prefixed by
/// construction. Walk every function body of `module` and return the names of
/// functions containing a `SetGlobal { global: "heap" }` outside the allowed set:
///
/// - `bump_alloc` itself (the one allocator), and
/// - watermark REWINDS: a `SetGlobal("heap", ...)` whose value is a plain
///   `GetLocal`/`GetLocal+expr` restore of a previously captured watermark
///   (`__witchy_wm_*` locals — the RFC-0016 region reclaim resets, which move
///   `$heap` DOWN to a value it already held, never past unensured memory). The
///   codegen region copy-out's `heap = wm + copied_len` advance is also a rewind
///   in this sense: `copied_len <= heap - wm` (the copy slides finished data DOWN
///   below the old, already-ensured frontier).
///
/// Everything else — a raw ensure+bump pair, an unensured bump, any hand-written
/// helper writing `$heap` — is a violation: it either forgot `ensure()` (the
/// `int_to_string` OOB class) or will forget it on the next edit. The test that
/// wraps this fn is the enforcement; it cannot be forgotten the way `ensure()` can.
pub fn heap_write_violations(module: &WirModule) -> Vec<String> {
    fn value_is_watermark_rewind(e: &WirExpr) -> bool {
        // The rewind shapes codegen emits: `heap = wm` and `heap = wm + delta`
        // where `wm` is a `__witchy_wm_*` watermark local captured from `$heap`.
        match e {
            WirExpr::GetLocal(n) => n.starts_with("__witchy_wm_"),
            WirExpr::Binary { lhs, .. } => value_is_watermark_rewind(lhs),
            _ => false,
        }
    }
    fn node_violates(n: &WirNode, hits: &mut bool) {
        match n {
            WirNode::Source { body, .. } => {
                for node in body {
                    node_violates(node, hits);
                }
            }
            WirNode::SetGlobal { global, value } => {
                if global == "heap" && !value_is_watermark_rewind(value) {
                    *hits = true;
                }
                expr_violates(value, hits);
            }
            WirNode::SetLocal { value, .. } => expr_violates(value, hits),
            // StructSet writes a GC field, not the `$heap` bump pointer — it can
            // never advance `$heap`, so only its subexpressions are scanned.
            WirNode::StructSet { base, value, .. } => {
                expr_violates(base, hits);
                expr_violates(value, hits);
            }
            WirNode::ArraySet { array, index, value, .. } => {
                expr_violates(array, hits);
                expr_violates(index, hits);
                expr_violates(value, hits);
            }
            WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
                expr_violates(ptr, hits);
                expr_violates(value, hits);
            }
            WirNode::CallStoreMulti { args, .. } => {
                for a in args {
                    expr_violates(a, hits);
                }
            }
            WirNode::CallIndirectStoreMulti { args, index, .. } => {
                for a in args {
                    expr_violates(a, hits);
                }
                expr_violates(index, hits);
            }
            WirNode::MemoryCopy { dest, src, len } => {
                expr_violates(dest, hits);
                expr_violates(src, hits);
                expr_violates(len, hits);
            }
            WirNode::MemoryFill { dest, value, len } => {
                expr_violates(dest, hits);
                expr_violates(value, hits);
                expr_violates(len, hits);
            }
            WirNode::If { cond, then_, els, .. } => {
                expr_violates(cond, hits);
                for x in then_.iter().chain(els.iter()) {
                    node_violates(x, hits);
                }
            }
            WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
                for x in body {
                    node_violates(x, hits);
                }
            }
            WirNode::Br { cond, .. } => {
                if let Some(c) = cond {
                    expr_violates(c, hits);
                }
            }
            WirNode::Drop(e) | WirNode::Do(e) | WirNode::Push(e) | WirNode::Return(Some(e)) => {
                expr_violates(e, hits);
            }
            WirNode::Return(None) | WirNode::Unreachable => {}
        }
    }
    fn expr_violates(e: &WirExpr, hits: &mut bool) {
        match e {
            WirExpr::ToSlot(i, _)
            | WirExpr::FromSlot(i, _)
            | WirExpr::Unary { arg: i, .. }
            | WirExpr::Convert { arg: i, .. }
            | WirExpr::Load { ptr: i, .. }
            | WirExpr::Load8U { ptr: i, .. }
            | WirExpr::MemoryGrow(i) => expr_violates(i, hits),
            WirExpr::Binary { lhs, rhs, .. } => {
                expr_violates(lhs, hits);
                expr_violates(rhs, hits);
            }
            WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } => {
                for a in args {
                    expr_violates(a, hits);
                }
            }
            WirExpr::CallIndirect { args, index, .. } => {
                for a in args {
                    expr_violates(a, hits);
                }
                expr_violates(index, hits);
            }
            WirExpr::Control(n) => node_violates(n, hits),
            WirExpr::Seq(s) => {
                for x in s {
                    node_violates(x, hits);
                }
            }
            WirExpr::StructNew { args, .. } => {
                for a in args {
                    expr_violates(a, hits);
                }
            }
            WirExpr::ArrayNew { value, len, .. } => {
                expr_violates(value, hits);
                expr_violates(len, hits);
            }
            WirExpr::ArrayNewFixed { items, .. } => {
                for item in items {
                    expr_violates(item, hits);
                }
            }
            WirExpr::ArrayGet { array, index, .. } => {
                expr_violates(array, hits);
                expr_violates(index, hits);
            }
            WirExpr::StructGet { base, .. }
            | WirExpr::RefCast { value: base, .. }
            | WirExpr::RefCastNullable { value: base, .. }
            | WirExpr::ArrayLen(base)
            | WirExpr::RefIsNull(base) => expr_violates(base, hits),
            WirExpr::ConstI64(_)
            | WirExpr::ConstF64(_)
            | WirExpr::ConstI32(_)
            | WirExpr::StrPtr(_)
            | WirExpr::GetLocal(_)
            | WirExpr::GetGlobal(_)
            | WirExpr::MemorySize
            | WirExpr::RefNull(_) => {}
        }
    }
    let mut out = Vec::new();
    for f in &module.funcs {
        if f.name == "bump_alloc" {
            continue; // the one allocator
        }
        let mut hits = false;
        for n in &f.body {
            node_violates(n, &mut hits);
        }
        if hits {
            out.push(f.name.clone());
        }
    }
    out
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
