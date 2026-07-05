//! WebAssembly code generation for witchy.
//!
//! Lowers the type-checked AST to WIR — the structured IR in `witchy_wir::wir` — which
//! `witchy_wir::wir_encode` then encodes to a wasm binary. The entry points are
//! `compile_module_binary` (AST → wasm bytes) and `assemble_wir_module`
//! (AST → `WirModule`).
//!
//! Value model: a universal 8-byte (`i64`) slot. Integers are `i64`; floats are
//! bit-reinterpreted into the slot; pointers and Bools are `i32` widened to it
//! (`to_slot`/`from_slot` convert at typed boundaries). A string is an `i32`
//! pointer to a length-prefixed record in linear memory: `[len: i32][utf8
//! bytes...]`.
//!
//! Capabilities are host imports (`print`, `print_int`, `dir_*`, `net_*`, …) that
//! the runtime links only when granted, so an ungranted compiled module cannot
//! instantiate.
//!
//! Module layout. The `Codegen` struct and the core block/expression/statement
//! lowering live here in `mod.rs`; cohesive groups are split into sibling child
//! modules (each an `impl Codegen` block, or free functions, that shares this
//! module's items via `use super::*`):
//!
//! - [`types`] — type/kind inference (`kind_of`, `val_type_of`, …).
//! - [`helpers`] — per-`EqShape` structural WIR-helper generation (`$eq`, `$ts`
//!   to-string/render, `$rcopy` region-copy).
//! - [`builtins`] — the `lower_call` builtin/stdlib call dispatch.
//! - [`passes`] — pre-lowering AST rewrites (alpha-renaming, concat flip,
//!   try-context rewrite).
//! - [`assembly`] — the compile entry points (`compile_module_binary`,
//!   `assemble_wir_module`, `compile_build_module`) and the wiring that turns
//!   lowered per-function WIR into a finished module (reachability, item
//!   registration, prelude/helper selection, WIR → wasm encode).

mod types;
mod helpers;
mod builtins;
mod passes;
mod assembly;
pub use assembly::*;
use passes::{alpha_rename_module, flip_string_add_module, rewrite_try_ctx_module};

use crate::analysis::{self};
use witchy_syntax::lambda_scan::{collect_pattern_vars, scan_lambda};
use std::collections::{HashMap, HashSet};
use std::fmt;

use witchy_syntax::ast::{
    collect_type_vars, BinOp, Block, Convention, Expr, Function, Item, MatchArm, Module, Param,
    Pattern, Stmt, Type, UnOp,
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

/// One captured variable for a closure: (name, record-type-name,
/// list-element-type-name, slot kind).
type CaptureInfo = (String, Option<String>, Option<String>, Kind);

/// (RFC-0062) A tier-1 elided closure: its lifted THREADED body index (a `$__lamt{i}`
/// taking captures as leading arguments, NO env pointer) plus its ordered captures —
/// each `(local name, slot kind)` — read at every direct call site into i64 argument
/// slots. No heap environment record is ever allocated for such a closure.
type ThreadedClosure = (usize, Vec<(String, Kind)>);

/// (RFC-0062) How a lifted lambda body receives its captures: boxed into a heap
/// environment (the tier-3 default, an env pointer as the implicit first param) or
/// threaded as leading value parameters (the tier-1 elided closure, no env).
#[derive(Clone, Copy, PartialEq)]
enum CapMode {
    Env,
    Threaded,
}

/// Scratch local holding the Result/Option being unwrapped by `?`.
const TRY_TMP: &str = "__witchy_try_tmp";

/// Scratch local holding a `match` scrutinee while arms test it.
const MATCH_TMP: &str = "__witchy_match_tmp";

/// (RFC-0035 step 4) Scratch i64 slot holding a match's RESULT while its dup'd-read
/// scrutinee is `$rc_drop`'d after the arms (`FromSlot` recovers any width). Shared is
/// safe: each match writes-then-reads its result before its parent arm writes.
const MATCH_RES: &str = "__witchy_match_res";

/// (RFC-0035 step 4) Depth of dup'd-read matches whose scrutinee is dropped after the
/// arms; indexes `__witchy_scrut_save_{depth}`. Because an arm body may nest matches that
/// clobber the shared `MATCH_TMP`, the scrutinee is copied into a PER-DEPTH i64 save slot
/// so the post-arm `$rc_drop` reads the right value. Beyond `SCRUT_POOL` the drop is
/// skipped (a sound leak, not a wrong dec).
const SCRUT_POOL: usize = 16;

/// Scratch local holding a `SecretStore.get` handle (the host-table index) so it
/// is fetched once and reused for both the present-test and the `Some` payload.
const SECRET_TMP: &str = "__witchy_secret_tmp";

/// (RFC-0037 §3) Scratch i32 local holding a record pointer under `WITCHY_TYPE_CHECK`, so the
/// type-tag check and the field load share one evaluation of the base.
const TYPECHECK_TMP: &str = "__witchy_typecheck_tmp";

/// (RFC-0037 §3) A stable, stateless 8-bit type id for the type-confusion sanitizer: the same
/// type name always maps to the same NON-ZERO tag (0 means "untagged"), so the WRITE side (a
/// record ctor) and the CHECK side (a `.field` read) agree without threading a registry. FNV-1a
/// hash → 1..=255; a collision (~1/255 per type pair) only misses a confusion, never false-traps.
fn type_tag_of(name: &str) -> u8 {
    let mut h: u32 = 2166136261;
    for byte in name.bytes() {
        h ^= byte as u32;
        h = h.wrapping_mul(16777619);
    }
    (h % 255) as u8 + 1
}

/// One scratch local per nesting level of expression application (`f(x)(y)`),
/// holding the callee pointer while its arguments are evaluated. A nested
/// application inside an argument uses the next level, so the levels never
/// clobber each other. Application nested deeper than this in argument
/// position is rejected (absurd in practice).
const APPLY_POOL: usize = 8;

/// (RFC-0016) Scratch i64 slots for capacity-resizing in-place reuse: a list `var`
/// reassignment `x = [e0, …, e_{k-1}]` evaluates its elements into these once, then
/// either overwrites `x`'s buffer (when it fits) or reallocates — so the elements
/// are not double-evaluated across the branch. A literal with more than this many
/// elements skips the optimization and allocates normally.
const REUSE_POOL: usize = 8;

/// The closure-environment pointer: the implicit first parameter of every
/// lifted lambda, pointing at its `[code_index][cap0]..` heap record.
const ENV_PARAM: &str = "__witchy_env";

/// The WASM representation of a value:
///   * `I64` — `Int`, and the UNIVERSAL representation for type variables /
///     generic values / heap slots. Pointers and bools are zero-extended into
///     i64 and floats are bit-reinterpreted when they enter this representation
///     (see `to_slot`/`from_slot`).
///   * `F64` — `Float`.
///   * `I32` — concrete pointers (strings/lists/records/closures/capabilities)
///     and `Bool`. These are the wasm32 address width.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    I32,
    I64,
    F64,
}

fn ty_kind(t: &Type) -> Kind {
    // Concrete `Int` is i64 (matching the interpreter); `Float` is f64. Type
    // variables / generics stay i32 (the existing universal ABI: a generic
    // function compiled once passes values as i32, pointers natively and `Int`
    // narrowed). Heap slots are 8 bytes regardless (see `to_slot`/`from_slot`),
    // so a *concretely*-typed `Int` survives in a list/record; only values that
    // pass through generic code narrow to 32 bits (a monomorphization gap).
    match t {
        Type::Named(n, _) if n == "Float" => Kind::F64,
        Type::Named(n, _) if n == "Int" || n == "Duration" => Kind::I64,
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

/// The element a nested list ultimately bottoms out at: a scalar, or a tuple with
/// the given slot value types. Paired with a nesting depth (`List(Int)` is depth
/// 1 over `Scalar(Int)`; `List(List((Int,Int)))` is depth 2 over
/// `Tuple([Int,Int])`), it lets `at`/`for` peel one list level at a time and
/// recover the bottom element at the right width, at any depth.
#[derive(Clone, PartialEq, Eq)]
enum NestBottom {
    Scalar(ValType),
    Tuple(Vec<ValType>),
}

/// The structural shape of a value, used both for content equality and for
/// `to_string` rendering on WASM. Because runtime slots are untyped, an op on a
/// compound must be specialized to the static shape: `Int`/`Bool` both compare
/// the raw i64 slot (and `Duration` rides along as `Int`) but render differently
/// ("42" vs "true"); `Float` reinterprets and uses `f64.eq`; `Str` calls
/// `$str_eq` on the slot's string pointer; and the compound variants recurse
/// element/field-wise through generated helpers. A shape codegen cannot resolve
/// yields a loud error, never a silent pointer compare or a mis-render.
#[derive(Clone, PartialEq, Eq)]
enum EqShape {
    // `Int` and `Bool` compare identically (`i64.eq`) but RENDER differently
    // (`to_string`: "42" vs "true"), so they are kept distinct rather than a
    // single `Bits`.
    Int,
    Bool,
    Float,
    Str,
    List(Box<EqShape>),
    Tuple(Vec<EqShape>),
    Record(String),
    /// A sum type (its variants and their field types come from `adt_variants`).
    Adt(String),
    /// A generic sum type INSTANTIATED at the comparison site: the resolved
    /// field shapes per variant (by tag). `Some(5) == opt` carries
    /// `AdtInst("Option", [[Bits], []])` — sound for both operands because the
    /// type checker guarantees `==` operands share a type.
    AdtInst(String, Vec<Vec<EqShape>>),
    /// A RECURSIVE generic ADT instantiation (`Stack(Int)`), identified by its
    /// type ARGUMENT shapes rather than an expanded field tree (which would be
    /// infinite). The helper resolves each variant's fields lazily under the
    /// argument substitution; a self-referential field resolves to this same
    /// shape, whose helper name is already reserved — the recursion becomes a
    /// `call` to the helper itself.
    AdtRec(String, Vec<EqShape>),
    /// A Dict with resolved key and value shapes. Equality is insertion-order-
    /// sensitive over the `[count][key slot][value slot]...` entries — exactly
    /// the interpreter's pairwise `Vec<(K, V)>` comparison.
    Dict(Box<EqShape>, Box<EqShape>),
}

impl EqShape {
    /// The scalar shape for a `ValType`, or `None` for `Other` (unresolvable).
    fn scalar(vt: ValType) -> Option<EqShape> {
        match vt {
            ValType::Int => Some(EqShape::Int),
            ValType::Bool => Some(EqShape::Bool),
            ValType::Float => Some(EqShape::Float),
            ValType::Str => Some(EqShape::Str),
            ValType::Other => None,
        }
    }

    /// Whether this is a heap-pointer compound (needs a generated helper) rather
    /// than a slot-level scalar.
    fn is_compound(&self) -> bool {
        matches!(
            self,
            EqShape::List(_)
                | EqShape::Tuple(_)
                | EqShape::Record(_)
                | EqShape::Adt(_)
                | EqShape::AdtInst(..)
                | EqShape::AdtRec(..)
                | EqShape::Dict(..)
        )
    }

    /// A stable identifier used to name and memoize the per-shape eq helper.
    fn id(&self) -> String {
        match self {
            EqShape::Int => "int".into(),
            EqShape::Bool => "bool".into(),
            EqShape::Float => "f64".into(),
            EqShape::Str => "str".into(),
            EqShape::List(e) => format!("list_{}", e.id()),
            EqShape::Tuple(fs) => {
                format!("tup_{}_", fs.iter().map(|f| f.id()).collect::<Vec<_>>().join("_"))
            }
            EqShape::Record(name) => format!("rec_{name}"),
            EqShape::Adt(name) => format!("adt_{name}"),
            EqShape::AdtInst(name, variants) => {
                let vs: Vec<String> = variants
                    .iter()
                    .map(|fs| fs.iter().map(|f| f.id()).collect::<Vec<_>>().join("_"))
                    .collect();
                format!("adti_{name}_{}_", vs.join("__"))
            }
            EqShape::AdtRec(name, args) => {
                let a: Vec<String> = args.iter().map(|s| s.id()).collect();
                format!("adtr_{name}_{}_", a.join("_"))
            }
            EqShape::Dict(k, v) => format!("dict_{}_{}", k.id(), v.id()),
        }
    }
}

/// The common kind two numeric operands/branches promote to: f64 if either is
/// Float, else i64 if either is i64 (a concrete Int), else i32.
fn promote_kind(a: Kind, b: Kind) -> Kind {
    if a == Kind::F64 || b == Kind::F64 {
        Kind::F64
    } else if a == Kind::I64 || b == Kind::I64 {
        Kind::I64
    } else {
        Kind::I32
    }
}

/// The WASM `Kind` for a field/element whose type is known only as an optional
/// type-name string (as `record_fields` stores it). `Int` and type variables and
/// unknown (None) use the universal i64; `Float` is f64; concrete pointer types
/// and `Bool` are i32.
fn name_kind(n: Option<&str>) -> Kind {
    match n {
        Some("Float") => Kind::F64,
        Some("Int") | Some("Duration") => Kind::I64,
        // Concrete pointers, Bool, type variables, and unknown all use i32 (the
        // generic ABI). Only a concrete `Int`/`Float`/`Duration` field is wider.
        _ => Kind::I32,
    }
}

/// The WASM `Kind` a `ValType` is carried as. `Int` is i64; `Float` is f64;
/// `Other` (a generic/undetermined value) uses the universal i64 slot rep;
/// `Bool` and `Str` (a pointer) are i32.
fn valtype_kind(vt: ValType) -> Kind {
    match vt {
        ValType::Int => Kind::I64,
        ValType::Float => Kind::F64,
        // Bool, Str (pointer), and Other (generic/undetermined) use the i32
        // generic ABI.
        ValType::Bool | ValType::Str | ValType::Other => Kind::I32,
    }
}

fn ty_to_valtype(t: &Type) -> ValType {
    match t {
        Type::Named(n, _) if n == "Int" || n == "Duration" => ValType::Int,
        Type::Named(n, _) if n == "Bool" => ValType::Bool,
        Type::Named(n, _) if n == "Float" => ValType::Float,
        Type::Named(n, _) if n == "String" => ValType::Str,
        _ => ValType::Other,
    }
}

/// `(depth, scalar)` for a (possibly nested) `List(...)` type over a scalar
/// element — `List(Int)` is `(1, Int)`, `List(List(Int))` is `(2, Int)`. `None`
/// for non-list or non-scalar-bottomed types. Lets a `List(List(Int))` parameter
/// drive nested-`at` recovery.
fn ty_list_nesting(t: &Type) -> Option<(usize, NestBottom)> {
    if let Type::Named(n, args) = t {
        if n == "List" {
            return match args.first() {
                Some(inner @ Type::Named(in_n, _)) if in_n == "List" => {
                    ty_list_nesting(inner).map(|(d, b)| (d + 1, b))
                }
                Some(Type::Tuple(slots)) => {
                    Some((1, NestBottom::Tuple(slots.iter().map(ty_to_valtype).collect())))
                }
                Some(elem) => match ty_to_valtype(elem) {
                    ValType::Other => None,
                    s => Some((1, NestBottom::Scalar(s))),
                },
                None => None,
            };
        }
    }
    None
}

/// A function's local-type tables, taken out while a nested lambda body compiles
/// and restored afterward. Bundled so `swap_out_scope`/`restore_scope` pass one
/// value instead of a long argument list.
struct SavedScope {
    locals: HashMap<String, Kind>,
    records: HashMap<String, String>,
    list_elem: HashMap<String, String>,
    payload: HashMap<String, String>,
    val_types: HashMap<String, ValType>,
    list_elem_vt: HashMap<String, ValType>,
    list_elem_tuple: HashMap<String, Vec<ValType>>,
    tuple_slots: HashMap<String, Vec<ValType>>,
    shape: HashMap<String, EqShape>,
    payload_vt: HashMap<String, ValType>,
    fn_ret_kind: HashMap<String, Kind>,
    ret: Kind,
    ret_slot: bool,
    var: bool,
    var_params: Vec<String>,
    sroa_candidates: HashSet<String>,
    sroa_active: HashMap<String, usize>,
    view_candidates: HashSet<String>,
    view_active: HashSet<String>,
    packed_candidates: HashSet<String>,
    packed_active: HashMap<String, String>,
    reuse_vars: HashSet<String>,
    rc_floor_vars: HashSet<String>,
    rc_owned_bindings: HashSet<String>,
    devirt_ok: HashSet<String>,
    devirt_index: HashMap<String, usize>,
    thread_index: HashMap<String, ThreadedClosure>,
    closure_elide_called: HashSet<String>,
    closure_elide_reassigned: HashSet<String>,
    elide_index_list: Vec<(String, String)>,
}

struct Codegen {
    strings: Vec<(String, u32)>,
    next_offset: u32,
    uses_int_to_string: bool,
    /// The WIR sink. `compile_function` moves a fully-lowered
    /// body into `wir_funcs` (one `WirFunc` per function whose whole body lowered
    /// to WIR). `compile_module_binary` assembles those + the static prelude into
    /// a binary via `wir_encode`.
    captured_seq: Option<witchy_wir::wir::WirSeq>,
    /// A HARD rejection raised mid-lowering (e.g. a closure that assigns a
    /// captured variable — by-value capture can't write back). Lowering bails to
    /// `None` like any unsupported construct, but `compile_function` turns this
    /// into an `Err` so the program is rejected with a diagnostic, not silently
    /// reported as "unsupported".
    reject_reason: Option<CodegenError>,
    wir_funcs: HashMap<String, witchy_wir::wir::WirFunc>,
    /// Set by `compile_module_binary` to arm WIR capture for the function being
    /// lowered. Left `false` for any scope that doesn't collect WIR, where
    /// `lower_expr`'s call arm stays inert and pays no capture/clone overhead.
    collect_wir: bool,
    /// The exact set of names compiled to real `$name` functions (reachable,
    /// non-intrinsic `Item::Function`s) — populated by `compile_module_binary`.
    /// A call lowers to a direct `WirExpr::Call` only for a member; an intrinsic
    /// or native (`math.sqrt`, `crypto.ed25519_verify`) is NOT one, so it defers.
    emitted_funcs: HashSet<String>,
    /// Parameter conventions per function, so call sites can write back `var`
    /// results (move-in / move-out).
    fn_conventions: HashMap<String, Vec<Convention>>,
    /// Parameters of each top-level function, so a bare function name used as a
    /// value can be materialized as a forwarding closure.
    fn_params: HashMap<String, Vec<Param>>,
    /// Constructor name -> (variant tag, field count). A constructor value is a
    /// heap record `[tag: i32][field: i32]...`.
    ctors: HashMap<String, (u32, usize)>,
    /// Constructor name -> per-field record type name (Some when that field is
    /// a record), so binding `Circle(p)` in a pattern lets `p.field` resolve.
    /// Only concrete (non-generic) field types are known here.
    ctor_field_records: HashMap<String, Vec<Option<String>>>,
    /// Constructor arities for which an allocation helper `$mk{N}` is needed.
    mk_arities: HashSet<usize>,
    /// Counter for unique `match` block labels.
    next_label: u32,
    /// Kinds (i32/f64) of the current function's parameters and locals.
    locals: HashMap<String, Kind>,
    /// Declared return kind per function, for resolving call-result kinds.
    fn_ret: HashMap<String, Kind>,
    /// Function name -> the return kind of the CLOSURE it returns, for a function
    /// declared `-> fn(...) -> RET`. Lets `let f = make(...)` then `f(x)` recover
    /// the closure's result at the right width.
    fn_ret_closure_kind: HashMap<String, Kind>,
    /// Function name -> the element value types of the tuple it returns, so a
    /// `let (a, b) = f(...)` destructures at the right widths (an Int slot as
    /// i64, not the generic i32).
    fn_ret_tuple_slots: HashMap<String, Vec<ValType>>,
    /// Function name -> the slot types of the TUPLE ELEMENTS of the `List((..))`
    /// it returns (e.g. a monomorphized `zip__Int__Int`), so `at(f(...), i)` /
    /// `for t in f(...)` destructure the tuple at the right widths.
    fn_ret_list_elem_tuple_slots: HashMap<String, Vec<ValType>>,
    /// Function name -> per-tuple-slot list-element value type (Some when that
    /// slot is a `List(<scalar>)`), for a tuple-returning fn like `unzip` whose
    /// result is `(List(T), List(U))` — so `let (xs, ys) = unzip(...)` then
    /// `at(xs, i)` recovers an Int element as i64.
    fn_ret_tuple_slot_list_elem: HashMap<String, Vec<Option<ValType>>>,
    /// Whether the list `drop` runtime helper is needed.
    uses_list_drop: bool,
    /// Whether the `starts_with`/`ends_with` string helpers are needed.
    uses_starts_with: bool,
    /// Whether the `crypto.ed25519_verify` host import is needed.
    uses_crypto_ed25519_verify: bool,
    /// Whether the `crypto.sha256` host import + guest helper are needed.
    uses_crypto_sha256: bool,
    /// Whether the `crypto.rune_hash` host import + guest helper are needed.
    uses_crypto_rune_hash: bool,
    /// Variables in the CURRENT function eligible for in-place push
    /// (the analysis's accumulator set); each carries a shadow `${name}__cap`
    /// ownership-token local.
    inplace_push: HashSet<String>,
    /// (RFC-0027/0024) Locals the escape analysis proved frame-confined and used
    /// only via field/index access, so they are scalar-replaced: their fields live
    /// in `${name}$<i>` i64-slot locals instead of a heap object. Populated per
    /// unit in `begin_unit` (only under the `sroa` lever). `sroa_active` is the
    /// subset whose `let` codegen actually replaced (the receiver type resolved),
    /// so `Expr::Field` reads the locals only for those — kept consistent because a
    /// `let` precedes its uses in statement order.
    sroa_candidates: HashSet<String>,
    sroa_active: HashMap<String, usize>,
    /// (RFC-0034 L3) Closure locals eligible for devirtualization in the current
    /// unit: a name bound exactly once and never reassigned (so every `f(x)` reaches
    /// the same lambda). Computed in `begin_unit` only under the `direct-call` lever.
    devirt_ok: HashSet<String>,
    /// (RFC-0034 L3) The subset of `devirt_ok` whose single binding was lowered to a
    /// lambda — mapping the local to that lifted `$__lamw{i}` table index. A call site
    /// `f(x)` with `f` here emits a direct `call $__lamw{i}` instead of `call_indirect`.
    devirt_index: HashMap<String, usize>,
    /// (RFC-0062) Closure locals whose environment is ELIDED: bound once to a lambda,
    /// used ONLY as a direct-call callee (`closure_elide_called`), with every capture
    /// reassignment-free. Maps the local to its threaded lifted body + ordered captures;
    /// a call `f(x)` here becomes a direct `call $__lamt{i}(cap0, .., x)` with NO env
    /// allocation. Checked BEFORE `devirt_index` at every call site.
    thread_index: HashMap<String, ThreadedClosure>,
    /// (RFC-0062) Names `only_directly_called` proved non-escaping this unit (env-elision
    /// candidates), and the names reassigned this unit (a capture in the latter is unsafe
    /// to thread — the interpreter snapshots captures at creation). Both computed in
    /// `begin_unit` only under the `closure-elide` lever; empty otherwise.
    closure_elide_called: HashSet<String>,
    closure_elide_reassigned: HashSet<String>,
    /// (RFC-0034 L2) Active `(index-var, list-var)` pairs whose `list.at(list, index)`
    /// is provably in range — pushed while lowering the body of an eligible
    /// `for index in 0..list.length(list)` loop (see `bounds_elide_pair`), so the
    /// access lowers to a direct unchecked load. A stack: nested eligible loops each
    /// push their own pair (a same-named inner loop disqualifies the outer, so a stale
    /// pair never shadows). Empty ⇒ every `list.at` keeps its trap guard.
    elide_index_list: Vec<(String, String)>,
    /// (RFC-0028) Confined slice *views*: `let w = list.slice(src, lo, hi)` bindings
    /// the escape analysis proved read-only-by-`at`/`length` over an unmutated
    /// source, so the slice copy is elided — `w` keeps `${w}$src`/`${w}$lo`/`${w}$hi`
    /// i32 locals and reads through the source. `view_candidates` is the per-unit
    /// set (under the `views` lever); `view_active` is the subset whose `let` codegen
    /// replaced, so `list.at`/`list.length` lower to the view helpers only for those.
    view_candidates: HashSet<String>,
    view_active: HashSet<String>,
    /// (RFC-0027 packed, inferred) Confined `List` literals of fixed-scalar records
    /// (`let xs = [P(..), ..]`, read only via `list.length` / `list.at(_).field`,
    /// per `escape::confined_record_list_candidates`) stored as one FLAT inline
    /// buffer instead of an array of pointers to boxed records. `packed_candidates`
    /// is the per-unit AST-level set (under the `unbox` lever); `packed_active` maps
    /// the names whose `let` codegen actually packed to their element record type, so
    /// `list.at(xs, i).field` reads the inline slot only for those. Gated, opt-in.
    packed_candidates: HashSet<String>,
    packed_active: HashMap<String, String>,
    /// (RFC-0016 RC-floor reuse) Confined, never-aliased `var`s bound to a list
    /// literal of fixed length L, every reassignment to which is a same-length list
    /// literal (`escape::confined_inplace_reuse_vars`). A reassignment overwrites the
    /// existing L-slot buffer IN PLACE instead of allocating a fresh list, bounding a
    /// build-and-drop loop that would otherwise leak. Per-unit, under the `rc-elide`
    /// lever; the buffer comes from the (normally-lowered) binding.
    reuse_vars: HashSet<String>,
    /// (RFC-0016) RC-floor reclamation: confined, never-aliased `let`/`var` heap
    /// locals (`escape::confined_reassigned_vars`) whose OLD buffer is freed into
    /// the size-classed free-list when it is overwritten by a freshly-allocated one
    /// (`x = f(x, …)`). Per-unit, under the opt-in `rc-floor` lever.
    rc_floor_vars: HashSet<String>,
    /// (RFC-0035 step 3) `let x = list.at(xs, i)` bindings whose read was `$rc_dup`'d
    /// (the SAME per-type gate as the dup site — offset-0 element, `rc-floor` on), so `x`
    /// owns a reference and must be `$rc_drop`'d at its last use. Recording the ownership
    /// here (not re-deriving it at the drop) makes drop-iff-dup'd true by construction —
    /// a never-dup'd binding is never dropped (which would underflow the count → UAF).
    rc_owned_bindings: HashSet<String>,
    /// (RFC-0035 step 4) Nesting depth of dup'd-read matches whose scrutinee is `$rc_drop`'d
    /// after the arms; indexes the `__witchy_scrut_save_{depth}` pool. Balanced inc/dec
    /// within `lower_match`, so it needs no per-unit reset or scope plumbing.
    match_scrut_depth: usize,
    /// The active compile unit's uniqueness facts + (kills consumed, sites
    /// seen) for the post-compile consumption check; units nest via lambdas.
    facts_stack: Vec<(analysis::Facts, usize, usize)>,
    /// (RFC-0035) Per-unit `last_use` drop points (parallel to `facts_stack`): values to
    /// `$rc_free` after their last use, consumed in `lower_block`. Empty unless `rc-floor`.
    drop_facts_stack: Vec<analysis::DropFacts>,
    /// Module-wide function summaries for the uniqueness analysis.
    summaries: analysis::Summaries,
    /// The current function's own-ABI parameter (its ownership token is the
    /// `${name}__cap` PARAM, and the function returns an extra i32 token).
    cur_fn_own_param: Option<String>,
    /// Whether the current function has type-variable parameters (a generic
    /// fallback): unknown-type comparisons there are rejected loudly.
    cur_fn_has_type_vars: bool,
    /// The function being compiled, for error context.
    cur_fn_name: String,
    /// Phase 0 (rfcs/language-evolution.md): typeck's resolved types for the
    /// EXACT module instance being compiled — the authoritative fallback
    /// wherever the local tracking maps come up empty.
    type_table: witchy_types::typeck::TypeTable,
    /// Whether the `$list_push_cap` helper is needed.
    uses_list_push_cap: bool,
    /// (RFC-0033 R2) The `${var}${field}__cap` field-buffer capacity tokens emitted
    /// by the in-place field-path push — declared as i32 locals on the unit being
    /// assembled. Cleared per function.
    field_caps: HashSet<String>,
    /// (RFC-0033 R2) `(var, field)` pairs whose `var.field = list.push(var.field, …)`
    /// may grow the field's list buffer in place — every other read of `var.field` is
    /// absent, so the buffer is never aliased. Recomputed per unit in `begin_unit`.
    field_push_safe: HashSet<(String, String)>,
    /// Whether the `$str_append_cap` helper is needed.
    uses_str_append_cap: bool,
    /// Whether the `$dict_insert_cap` helper is needed.
    uses_dict_insert_cap: bool,
    /// Whether the `$dict_update_cap` helper (the in-place upsert) is needed.
    uses_dict_update_cap: bool,
    /// Memoized per-shape region copy-out helpers (`$rcopy_<shape>`), the
    /// same family pattern as `eq_helpers`/`ts_helpers`.
    rcopy_helpers: std::collections::BTreeMap<String, String>,
    /// Whether any `region:` block emitted reclamation machinery.
    uses_region: bool,
    /// Current loop-watermark nesting depth (see WM_POOL).
    wm_level: usize,
    /// Whether any loop emitted a watermark reset (forces the heap global).
    uses_wm: bool,
    /// Whether the `compiler.footprint` host import + guest helper are needed.
    uses_compiler_footprint: bool,
    /// Whether the `compiler.diff` host import + guest helper are needed.
    uses_compiler_diff: bool,
    /// Whether the `regex.match_spans` host import + guest helper are needed (the
    /// native regex engine; the rest of `std/regex` is witchy built on it).
    uses_regex_spans: bool,
    /// Whether the `float_to_str` host import + guest helper are needed (float
    /// `to_string`).
    uses_float_to_str: bool,
    /// Whether the `string.from_code` host import + guest helper are needed (a
    /// Unicode code point -> its UTF-8 character; powers the JSON `\u` decoder).
    uses_string_from_code: bool,
    /// Whether the `encoding` host import + `$encoding` guest helper are needed
    /// (hex/base64 encode/decode, all `String -> String`).
    uses_encoding: bool,
    /// Whether the NaN-trapping float ordering helpers (`$f_lt`/`$f_le`/`$f_gt`/
    /// `$f_ge`) are needed. Ordering a NaN is a runtime error on the interpreter,
    /// so the compiled comparisons trap rather than silently returning IEEE false.
    uses_float_ord: bool,
    /// Whether the `now` host import is needed (`Clock` capability).
    uses_now: bool,
    /// Whether the `env_len`/`env_fill` host imports + `$get_env` guest helper
    /// are needed (`Env` capability).
    uses_get_env: bool,
    /// Which `Dir` operations the program uses ("read"/"write"/"append"/"exists"/
    /// "is_dir"/"list"/"subdir"/"make_dir"). Each pulls in exactly its own host
    /// import, so a read-only program never imports a write op (and therefore
    /// instantiates under a read-only grant).
    used_dir_ops: std::collections::BTreeSet<&'static str>,
    /// Which build-time host ops a `build` entrypoint uses ("write_out",
    /// "read_build") — gated like the Dir ops so each pulls in only its import.
    used_build_ops: std::collections::BTreeSet<&'static str>,
    /// Which `Net` operations the program uses, gated per verb the same way
    /// ("connect"/"restrict" under Connect; "listen"/"accept" under Listen;
    /// socket I/O under either).
    used_net_ops: std::collections::BTreeSet<&'static str>,
    /// The aws-lc-rs-backed crypto natives beyond the legacy set — `sha512`,
    /// `sha3_256`, `hmac_sha256`, `ecdsa_p256_verify`, `ecdsa_p256_verify_hex`.
    /// Each is bridged to the SAME native registry the interpreter calls, so the
    /// backends agree; tracked as a set rather than a bool-per-op.
    used_crypto_ops: std::collections::BTreeSet<&'static str>,
    /// Whether `main` declares an argv parameter (`args: List(String)`); the
    /// run export then builds the host-provided list via `$build_args`.
    uses_args: bool,
    /// Whether the `crypto.sign` host import + guest helper are needed
    /// (`Secret` capability).
    uses_crypto_sign: bool,
    /// Whether the `crypto.public_key` host import + guest helper are needed.
    uses_crypto_public_key: bool,
    uses_ends_with: bool,
    /// Whether the `split` helper is needed.
    uses_split: bool,
    /// Whether the `$str_chars` helper (string -> list of single-char strings)
    /// is needed.
    uses_str_chars: bool,
    /// Whether the `$substr` allocator is needed (split, substring).
    uses_substr: bool,
    /// Whether the `$ascii_case` helper (ASCII upper/lower-casing) is needed.
    uses_ascii_case: bool,
    /// Whether the `$find_byte` substring search is needed (contains, index_of).
    uses_find_byte: bool,
    /// Whether the char-indexed `index_of` wrapper (+ `$byte_to_char`) is needed.
    uses_index_of: bool,
    /// Whether `$byte_to_char` is needed on its own (char_count).
    uses_byte_to_char: bool,
    /// Whether the char-indexed `substring` wrapper (+ `$char_to_byte`) is needed.
    uses_substring: bool,
    /// Whether `replace` (and its `$match_at` companion) is needed.
    uses_replace: bool,
    /// Whether the `string_to_int` parser (`$str_to_int`) is needed.
    uses_str_to_int: bool,
    /// Whether the `trim` helper (`$trim` + `$is_ws`) is needed.
    uses_trim: bool,
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
    /// Whether the `$dict_update` upsert helper is needed (also implies
    /// `uses_dict`, since it calls `$dict_get_or` + `$dict_insert`).
    uses_dict_update: bool,
    /// Record type name -> ordered fields as `(name, named-type)`, where the
    /// second is the field's type name when it is a `Named` type (so nested
    /// records can be chained, `a.b.c`). For compiling `value.field`.
    record_fields: HashMap<String, Vec<(String, Option<String>)>>,
    /// Record type name -> the full AST type of each field, so structural `==`
    /// can derive an `EqShape` for `List`/`Tuple`/nested-record fields (which the
    /// name-only `record_fields` can't represent).
    record_field_types: HashMap<String, Vec<Type>>,
    /// (RFC-0027 declared `packed`) Type names declared `packed` (`type P packed:`).
    /// A `List` of such a type is stored as ONE flat inline buffer (the same layout
    /// the `unbox` inference uses for confined record lists), GUARANTEED by the
    /// declaration: a confined `let xs = [P(..)..]` packs under `unbox`, and a `List`
    /// of a declared-packed type used in a position the flat layout cannot support is
    /// a clean compile error (`reject_reason`) rather than a silent boxed fall-back.
    /// Module-global (set once at module setup), so it is NOT part of `SavedScope`.
    packed_types: HashSet<String>,
    /// ADT/record type name -> each variant's field types, indexed by tag, for
    /// structural `==` on sum types (`Color`, `Shape`, ...). Generic variant
    /// fields (a type variable) make a type unresolvable here -> a loud error.
    adt_variants: HashMap<String, Vec<Vec<Type>>>,
    /// Constructor name -> its owning type name (so a `Ctor` operand of `==` can
    /// find its variant set).
    ctor_type_name: HashMap<String, String>,
    /// (RFC-0047) Type names with a CUSTOM (non-derived) `PartialEq` impl. A
    /// compound `==` whose element/field type is here calls that type's
    /// `PartialEq__T__eq` instead of recursing structurally, so a custom equality is
    /// honored at every depth (inside List/Option/tuple/Dict/records). Derived
    /// (structural) types are NOT here, so their containers keep the fast structural
    /// helper — byte-identical code to before. A whole-program fact set once at
    /// module setup (module-global; not part of `SavedScope`).
    custom_eq_types: HashSet<String>,
    /// Variables (params / let-bound constructors) known to hold a record of a
    /// given type, so `var.field` can resolve a field index.
    local_records: HashMap<String, String>,
    /// Variables holding a `List(Record)`, mapping to the element record type, so
    /// a `for x in list` loop variable's fields can be resolved.
    local_list_elem: HashMap<String, String>,
    /// Variables holding an `Option(Record)`/`Result(Record, _)`, mapping to the
    /// payload record type, so `match v { Some(a) -> a.field }` resolves `a`.
    local_payload_records: HashMap<String, String>,
    /// Value type of params / let-bound locals, where known, so `to_string` can
    /// pick the right rendering. Absent = `Other`.
    local_val_types: HashMap<String, ValType>,
    /// Element value type of list-typed locals (e.g. a `let words = split(...)`
    /// is `List(String)`), so a `for x in words` loop variable's type — and thus
    /// its use as a Dict key — resolves.
    local_list_elem_valtype: HashMap<String, ValType>,
    /// Element tuple-slot value types of list-of-tuples locals (e.g. a param
    /// `pairs: List((String, Int))`), so a `for p in pairs` loop variable's
    /// slots are known.
    local_list_elem_tuple: HashMap<String, Vec<ValType>>,
    /// Tuple-slot value types of tuple-typed locals (e.g. a for-loop variable
    /// over a list of tuples), so `let (k, v) = p` gives `k`/`v` their types and
    /// `k == key` compiles to string (not pointer) comparison.
    local_tuple_slots: HashMap<String, Vec<ValType>>,
    /// The fully-resolved structural shape of a `let`-bound compound (captured
    /// from its RHS at binding time). Consulted as a `Var` fast-path in
    /// `eq_shape_of` so a tuple/list whose *slots are themselves compound*
    /// (`let v = ([1,2], (3,4))`) resolves — which the scalar-only
    /// `local_tuple_slots` path cannot. Closes the gap for both `==` and
    /// `to_string`.
    local_shape: HashMap<String, EqShape>,
    /// Function name -> the value type it returns, so `__render(f(...))` can be
    /// rendered. Populated from return-type annotations.
    fn_ret_valtype: HashMap<String, ValType>,
    /// Function name -> its DECLARED return type, resolved lazily to an
    /// `EqShape` at `==` sites (lazily so every type is registered by then) —
    /// e.g. `-> Result(Int, String)` makes a call result structurally
    /// comparable. Generic returns (`-> Result(a, e)`) resolve to the by-name
    /// shape, preserving the loud error.
    fn_ret_ty: HashMap<String, Type>,
    /// Function name -> the record type it returns (when it returns one), so a
    /// `let q = f(...)` binds `q` to that record type.
    fn_ret_records: HashMap<String, String>,
    /// Function name -> the record type that is the success payload of its
    /// Result/Option return, so `let q = f(...)?` binds `q` to that record.
    fn_ret_result_record: HashMap<String, String>,
    /// Function name -> the SCALAR value type of the success payload of its
    /// `Option(T)`/`Result(T, _)` return (e.g. `Int` for `parse_int`), so a
    /// `match f(...) { Some(n) -> ... }` and `f(...)?` recover the payload at the
    /// right width (an Int payload as i64, not the generic i32 that truncates a
    /// big value).
    fn_ret_result_valtype: HashMap<String, ValType>,
    /// Variable -> the scalar payload value type of an `Option`/`Result` it holds
    /// (the `let o = f(...)` analog of `fn_ret_result_valtype`).
    local_payload_valtype: HashMap<String, ValType>,
    /// Dict variable -> its value's scalar type (from the `insert` that built it),
    /// so `for v in values(d)` / `at(values(d), i)` recover an Int value as i64.
    local_dict_value_valtype: HashMap<String, ValType>,
    /// Dict variable -> its key's scalar type, so `pairs(d)` destructures the
    /// `(key, value)` tuple at the right widths (an Int key as i64).
    local_dict_key_valtype: HashMap<String, ValType>,
    /// List-of-lists variable -> the INNER list's scalar element type, so
    /// `at(at(xs, i), j)` recovers an Int as i64 (two levels of nesting).
    local_list_elem_list_valtype: HashMap<String, ValType>,
    /// Nested-list variable -> `(depth, bottom)`: the variable is a list nested
    /// `depth` times over a scalar or tuple bottom (`List(Int)` is depth 1 over
    /// `Scalar(Int)`; `List(List((Int,Int)))` is depth 2 over `Tuple([Int,Int])`).
    /// Lets `at`/`for` peel one level at a time so the bottom element recovers at
    /// the right width, at ANY nesting depth.
    local_list_nesting: HashMap<String, (usize, NestBottom)>,
    /// Function name -> the record type of the elements of its `List(_)` return,
    /// so `for x in f(...) { x.field }` resolves x's record type.
    fn_ret_list_elem: HashMap<String, String>,
    /// Functions declared `-> List(<scalar>)` (String/Int/Bool/Float), so a
    /// `let xs = f(...)` records xs's element value type and `at(xs, i)` /
    /// `for x in xs` carry it. Without this an `at(...)`-produced String would
    /// be `Other` and `==` would pointer-compare instead of using `$str_eq`.
    fn_ret_list_elem_valtype: HashMap<String, ValType>,
    /// Function name -> the argument index whose list element type is the payload
    /// of its `Option(a)`/`Result(a, _)` return (the shape `fn(List(a),..)->
    /// Option(a)`, as in list.find/head/min_by). Lets `match f(xs) { Some(r) ->
    /// r.field }` resolve r from xs's element record type, without full inference.
    fn_ret_option_of_list_arg: HashMap<String, usize>,
    /// Like the above but for the shape `fn(List(a),..) -> List(a)` (list.filter/
    /// take/drop/reverse/sort_by/slice/unique): the return's element type is that
    /// argument's element type, so `for x in f(xs) { x.field }` resolves.
    fn_ret_list_of_list_arg: HashMap<String, usize>,
    /// For the `map` shape `fn(.., fn(a) -> b, ..) -> List(b)`: the function-typed
    /// argument index whose *return* is the result's element type. So
    /// `for r in f(xs, fn(x){ Mk(..) }) { r.field }` resolves from the mapper.
    fn_ret_list_of_fn_arg: HashMap<String, usize>,
    /// Return kind of the function currently being compiled (for `return`).
    cur_fn_ret_kind: Kind,
    /// When true (compiling a lambda body), a `return`/tail value is stored into
    /// the universal i64 slot (the closure-result ABI) rather than narrowed to a
    /// fixed kind, so a closure returning a big `Int` keeps its 64 bits.
    cur_fn_ret_slot: bool,
    /// Param/local name -> the WASM kind a function-typed value returns, so a
    /// closure call `f(x)` recovers the result at the right width (an `Int`-
    /// returning closure as i64, not the generic i32).
    local_fn_ret_kind: HashMap<String, Kind>,
    /// Whether the current function has any `var` parameters.
    cur_fn_var: bool,
    /// The current function's `var` parameter names, in declaration order. An
    /// early `return`/`?` must push these (after the primary result) so the
    /// multi-result epilogue is reproduced on every exit path.
    cur_fn_var_params: Vec<String>,
    /// Generated per-shape structural-equality helper functions, keyed by
    /// `EqShape::id` so each shape is emitted once. A `BTreeMap` keeps emission
    /// order deterministic.
    eq_helpers: std::collections::BTreeMap<String, String>,
    /// The WIR-native twin of `eq_helpers` for the binary path: per-shape
    /// structural-equality `WirFunc`s, keyed identically (`eq_{id}`). Populated
    /// only for shapes whose fields are all scalar (Int/Bool/Float) — those
    /// helpers compare i64/f64 slots inline with no calls, so a program with such
    /// a `==` lowers without the eq-helper bail. Str/nested-compound fields still
    /// defer to WAT (their slot compare would need $str_eq / a nested eq call).
    eq_wir_helpers: std::collections::BTreeMap<String, witchy_wir::wir::WirFunc>,
    /// Names of eq helpers currently being built — a cycle guard so a recursive
    /// type's structural eq bails to WAT instead of looping in codegen.
    eq_building: std::collections::HashSet<String>,
    /// WIR-native twin of `ts_helpers` (per-shape `to_string`/`__render`
    /// renderers), keyed identically (`ts_{id}`), for the binary path. Includes
    /// tuples/lists with Int/Bool/String fields (built via `$concat` +
    /// `$int_to_string`); Float/Record fields and enums defer to WAT.
    ts_wir_helpers: std::collections::BTreeMap<String, witchy_wir::wir::WirFunc>,
    /// Cycle guard for `ensure_ts_wir_helper`, mirroring `eq_building`.
    ts_building: std::collections::HashSet<String>,
    /// WIR-native twin of `rcopy_helpers` (per-shape `region:` copy-out deep-copy),
    /// keyed identically (`rcopy_{id}`), for the binary path.
    rcopy_wir_helpers: std::collections::BTreeMap<String, witchy_wir::wir::WirFunc>,
    /// Cycle guard for `ensure_rcopy_wir_helper`, mirroring `eq_building`.
    rcopy_building: std::collections::HashSet<String>,
    /// Lifted lambda bodies for the binary path, in table-index order — the WIR
    /// twin of `lambdas`. Each is a `WirFunc $__lamw{i}`; the closure object
    /// stores `i` as its code index and `CallIndirect` uses it as the table slot.
    lambda_wir_funcs: Vec<witchy_wir::wir::WirFunc>,
    /// Maps a lambda's content hash to its index in `lambda_wir_funcs`, so the
    /// many lowering passes register each lambda exactly once (idempotent).
    lambda_wir_index: std::collections::HashMap<u64, usize>,
    /// (RFC-0062) Maps an ELIDED closure lambda's content hash to its THREADED lifted
    /// body index (a `$__lamt{i}` in `lambda_wir_funcs`), so an identical tier-1 lambda
    /// registers one threaded body across the many lowering passes. A global registry
    /// like `lambda_wir_index` (NOT scope-saved).
    lambda_threaded_index: std::collections::HashMap<u64, usize>,
    /// Generated per-shape `to_string` renderers, keyed by `EqShape::id` (a
    /// `ts_` prefix on the function name). Parallels `eq_helpers`: each compound
    /// shape that flows into `to_string` (or string interpolation) gets one
    /// renderer, emitted once, that builds the interpreter-identical string.
    ts_helpers: std::collections::BTreeMap<String, String>,
    /// Constructor names per sum type, indexed by tag — so a `to_string` ADT
    /// renderer can emit `Some(5)` / `None` (the `eq` path never needs names).
    adt_variant_names: HashMap<String, Vec<String>>,
    /// Closure arities for which a `(type $clos{n})` signature is needed (all
    /// i32 params, i32 result), used by `call_indirect`.
    clos_arities: HashSet<usize>,
    /// Current nesting level of expression application, indexing `APPLY_POOL`.
    apply_level: usize,
    /// Stack of enclosing loops' `(break-target, continue-target)` WASM labels
    /// (innermost last), so `break`/`continue` branch to the right block.
    loop_labels: Vec<(String, String)>,
}

impl Codegen {
    fn new() -> Self {
        Self {
            strings: Vec::new(),
            next_offset: DATA_BASE,
            uses_int_to_string: false,
            captured_seq: None,
            reject_reason: None,
            wir_funcs: HashMap::new(),
            collect_wir: false,
            emitted_funcs: HashSet::new(),
            fn_conventions: HashMap::new(),
            fn_params: HashMap::new(),
            ctors: HashMap::new(),
            ctor_field_records: HashMap::new(),
            mk_arities: HashSet::new(),
            next_label: 0,
            locals: HashMap::new(),
            fn_ret: HashMap::new(),
            fn_ret_closure_kind: HashMap::new(),
            fn_ret_tuple_slots: HashMap::new(),
            fn_ret_list_elem_tuple_slots: HashMap::new(),
            fn_ret_tuple_slot_list_elem: HashMap::new(),
            record_fields: HashMap::new(),
            record_field_types: HashMap::new(),
            custom_eq_types: HashSet::new(),
            packed_types: HashSet::new(),
            adt_variants: HashMap::new(),
            ctor_type_name: HashMap::new(),
            local_records: HashMap::new(),
            local_list_elem: HashMap::new(),
            local_payload_records: HashMap::new(),
            local_val_types: HashMap::new(),
            local_list_elem_valtype: HashMap::new(),
            local_list_elem_tuple: HashMap::new(),
            local_tuple_slots: HashMap::new(),
            local_shape: HashMap::new(),
            fn_ret_valtype: HashMap::new(),
            fn_ret_ty: HashMap::new(),
            fn_ret_records: HashMap::new(),
            fn_ret_result_record: HashMap::new(),
            fn_ret_result_valtype: HashMap::new(),
            local_payload_valtype: HashMap::new(),
            local_dict_value_valtype: HashMap::new(),
            local_dict_key_valtype: HashMap::new(),
            local_list_elem_list_valtype: HashMap::new(),
            local_list_nesting: HashMap::new(),
            fn_ret_list_elem: HashMap::new(),
            fn_ret_list_elem_valtype: HashMap::new(),
            fn_ret_option_of_list_arg: HashMap::new(),
            fn_ret_list_of_list_arg: HashMap::new(),
            fn_ret_list_of_fn_arg: HashMap::new(),
            cur_fn_ret_kind: Kind::I32,
            cur_fn_ret_slot: false,
            local_fn_ret_kind: HashMap::new(),
            cur_fn_var: false,
            cur_fn_var_params: Vec::new(),
            uses_list_drop: false,
            uses_starts_with: false,
            uses_crypto_ed25519_verify: false,
            uses_crypto_sha256: false,
            uses_crypto_rune_hash: false,
            inplace_push: HashSet::new(),
            sroa_candidates: HashSet::new(),
            sroa_active: HashMap::new(),
            devirt_ok: HashSet::new(),
            devirt_index: HashMap::new(),
            thread_index: HashMap::new(),
            closure_elide_called: HashSet::new(),
            closure_elide_reassigned: HashSet::new(),
            elide_index_list: Vec::new(),
            view_candidates: HashSet::new(),
            view_active: HashSet::new(),
            packed_candidates: HashSet::new(),
            packed_active: HashMap::new(),
            reuse_vars: HashSet::new(),
            rc_floor_vars: HashSet::new(),
            rc_owned_bindings: HashSet::new(),
            match_scrut_depth: 0,
            facts_stack: Vec::new(),
            drop_facts_stack: Vec::new(),
            summaries: analysis::Summaries::empty(),
            cur_fn_own_param: None,
            cur_fn_has_type_vars: false,
            cur_fn_name: String::new(),
            type_table: witchy_types::typeck::TypeTable::default(),
            uses_list_push_cap: false,
            field_caps: HashSet::new(),
            field_push_safe: HashSet::new(),
            uses_str_append_cap: false,
            uses_dict_insert_cap: false,
            uses_dict_update_cap: false,
            rcopy_helpers: std::collections::BTreeMap::new(),
            uses_region: false,
            wm_level: 0,
            uses_wm: false,
            uses_compiler_footprint: false,
            uses_compiler_diff: false,
            uses_regex_spans: false,
            uses_float_to_str: false,
            uses_string_from_code: false,
            uses_encoding: false,
            uses_float_ord: false,
            uses_now: false,
            uses_get_env: false,
            used_dir_ops: std::collections::BTreeSet::new(),
            used_build_ops: std::collections::BTreeSet::new(),
            used_net_ops: std::collections::BTreeSet::new(),
            used_crypto_ops: std::collections::BTreeSet::new(),
            uses_args: false,
            uses_crypto_sign: false,
            uses_crypto_public_key: false,
            uses_dict_update: false,
            uses_ends_with: false,
            uses_split: false,
            uses_str_chars: false,
            uses_substr: false,
            uses_ascii_case: false,
            uses_find_byte: false,
            uses_index_of: false,
            uses_byte_to_char: false,
            uses_substring: false,
            uses_replace: false,
            uses_str_to_int: false,
            uses_trim: false,
            uses_str_cmp: false,
            uses_dict: false,
            uses_dict_iter: false,
            eq_helpers: std::collections::BTreeMap::new(),
            eq_wir_helpers: std::collections::BTreeMap::new(),
            eq_building: std::collections::HashSet::new(),
            ts_wir_helpers: std::collections::BTreeMap::new(),
            ts_building: std::collections::HashSet::new(),
            rcopy_wir_helpers: std::collections::BTreeMap::new(),
            rcopy_building: std::collections::HashSet::new(),
            lambda_wir_funcs: Vec::new(),
            lambda_wir_index: std::collections::HashMap::new(),
            lambda_threaded_index: std::collections::HashMap::new(),
            ts_helpers: std::collections::BTreeMap::new(),
            adt_variant_names: HashMap::new(),
            clos_arities: HashSet::new(),
            apply_level: 0,
            loop_labels: Vec::new(),
        }
    }

    /// The WASM kind a closure-valued expression returns: a function-typed
    /// variable's tracked return kind, a lambda's body kind, else i32.
    fn apply_ret_kind(&self, func: &Expr) -> Kind {
        match func {
            Expr::Var(f) => self.local_fn_ret_kind.get(f).copied().unwrap_or(Kind::I32),
            Expr::Lambda { body, .. } => self.block_kind(body),
            _ => Kind::I32,
        }
    }

    /// The call-return kind of a closure VALUE, when determinable: a lambda
    /// literal's body kind, a call to a `-> fn(...) -> RET` function, or another
    /// closure-bound variable. Used to track `let f = <closure>` for later `f(x)`.
    fn closure_ret_kind_of(&self, value: &Expr) -> Option<Kind> {
        match value {
            Expr::Lambda { body, .. } => Some(self.block_kind(body)),
            Expr::Call { name, .. } => self.fn_ret_closure_kind.get(name).copied(),
            Expr::Var(v) => self.local_fn_ret_kind.get(v).copied(),
            _ => None,
        }
    }

    fn block_val_type(&self, b: &Block) -> ValType {
        match b.stmts.last() {
            Some(Stmt::Expr(e)) => self.val_type_of(e),
            _ => ValType::Other,
        }
    }

    fn block_record_type(&self, b: &Block) -> Option<String> {
        match b.stmts.last() {
            Some(Stmt::Expr(e)) => self.record_type_of(e),
            _ => None,
        }
    }

    /// The scalar value type a Dict holds, where determinable: an `insert(d, k,
    /// v)` gives it from `v`; a Dict variable carries its tracked type.
    fn dict_value_valtype_of(&self, value: &Expr) -> Option<ValType> {
        match value {
            Expr::Call { name, args } if name == "dict.insert" && args.len() == 3 => {
                match self.val_type_of(&args[2]) {
                    ValType::Other => None,
                    vt => Some(vt),
                }
            }
            Expr::Var(v) => self.local_dict_value_valtype.get(v).copied(),
            _ => None,
        }
    }

    /// The scalar KEY type a Dict holds (the `insert`'s key, or a Dict variable's
    /// tracked key type), so `pairs(d)` destructures the key slot correctly.
    fn dict_key_valtype_of(&self, value: &Expr) -> Option<ValType> {
        match value {
            Expr::Call { name, args } if name == "dict.insert" && args.len() == 3 => {
                match self.val_type_of(&args[1]) {
                    ValType::Other => None,
                    vt => Some(vt),
                }
            }
            Expr::Var(v) => self.local_dict_key_valtype.get(v).copied(),
            _ => None,
        }
    }

    /// The SCALAR value type of an `Option`/`Result` scrutinee's `Some`/`Ok`
    /// payload, where codegen can determine it: a variable bound to such a value,
    /// a call to a function declared `-> Option(T)`/`Result(T, _)`, or a literal
    /// `Some(x)`/`Ok(x)`. Lets `match` and `?` recover the payload at the right
    /// width so a big `Int` payload isn't truncated to the generic i32.
    fn match_payload_valtype(&self, scrutinee: &Expr) -> Option<ValType> {
        match scrutinee {
            Expr::Var(v) => self.local_payload_valtype.get(v).copied(),
            Expr::Call { name, .. } => self.fn_ret_result_valtype.get(name).copied(),
            Expr::Ctor { name, args } if (name == "Some" || name == "Ok") && args.len() == 1 => {
                match self.val_type_of(&args[0]) {
                    ValType::Other => None,
                    vt => Some(vt),
                }
            }
            _ => None,
        }
    }

    /// The record type of a list expression's elements, where codegen can
    /// determine it: a `List(Record)` variable, a list literal of records, or a
    /// call to a function declared to return `List(Record)`. Lets
    /// `for x in <expr> { x.field }` resolve x's record type for any such expr.
    fn elem_record_type_of(&self, iter: &Expr) -> Option<String> {
        match iter {
            Expr::Var(v) => self.local_list_elem.get(v).cloned(),
            Expr::List(items) => items.first().and_then(|e| self.record_type_of(e)),
            Expr::Call { name, args } => {
                // A declared `-> List(Record)` return...
                if let Some(rec) = self.fn_ret_list_elem.get(name) {
                    return Some(rec.clone());
                }
                // ...or the generic `fn(List(a),..) -> List(a)` shape, whose
                // element type is that of the given list argument.
                if let Some(&k) = self.fn_ret_list_of_list_arg.get(name) {
                    if let Some(arg) = args.get(k) {
                        return self.elem_record_type_of(arg);
                    }
                }
                // ...or the `map` shape `fn(.., fn(a)->b, ..) -> List(b)`, whose
                // element type is the mapper's return record (a lambda body, or
                // a named function declared to return a record).
                if let Some(&k) = self.fn_ret_list_of_fn_arg.get(name) {
                    return match args.get(k) {
                        Some(Expr::Lambda { body, .. }) => self.block_record_type(body),
                        Some(Expr::Var(f)) => self.fn_ret_records.get(f).cloned(),
                        _ => None,
                    };
                }
                None
            }
            _ => None,
        }
    }

    /// The element-tuple slot value types of a list expression, where the
    /// element is a tuple: a list variable (tracked) or a list literal of tuples.
    /// Lets `at(list_of_tuples, i)` and `for t in list_of_tuples` recover the
    /// tuple's slots at the right widths (an Int slot as i64).
    fn list_elem_tuple_slots(&self, list: &Expr) -> Option<Vec<ValType>> {
        match list {
            Expr::Var(v) => self.local_list_elem_tuple.get(v).cloned(),
            Expr::List(items) => match items.first() {
                Some(Expr::Tuple(slots)) => {
                    Some(slots.iter().map(|e| self.val_type_of(e)).collect())
                }
                _ => None,
            },
            // `pairs(d)` yields `(key, value)` tuples; their slot types are the
            // Dict's tracked key and value types.
            Expr::Call { name, args } if name == "dict.pairs" && args.len() == 1 => {
                if let Expr::Var(d) = &args[0] {
                    let k = self.local_dict_key_valtype.get(d).copied().unwrap_or(ValType::Other);
                    let v = self.local_dict_value_valtype.get(d).copied().unwrap_or(ValType::Other);
                    if k != ValType::Other || v != ValType::Other {
                        return Some(vec![k, v]);
                    }
                }
                None
            }
            // A function returning `List((..))` (e.g. a monomorphized `zip`).
            Expr::Call { name, .. } => self.fn_ret_list_elem_tuple_slots.get(name).cloned(),
            _ => None,
        }
        // Fall back to a tracked depth-1 tuple-bottom nesting (e.g. a loop var
        // bound to the inner list of a list-of-lists-of-tuples).
        .or_else(|| match self.list_nesting(list) {
            Some((1, NestBottom::Tuple(slots))) => Some(slots),
            _ => None,
        })
    }

    /// `(depth, scalar)` for a list-valued expression that is a uniform nested
    /// list over a scalar element: a tracked variable, a list literal, or
    /// `at(L, i)` (which peels one level off `L`). `None` when not a uniform
    /// nested-scalar list. Lets `at` recover an Int element at any nesting depth.
    fn list_nesting(&self, e: &Expr) -> Option<(usize, NestBottom)> {
        match e {
            Expr::Var(v) => self
                .local_list_nesting
                .get(v)
                .cloned()
                .or_else(|| {
                    self.local_list_elem_list_valtype
                        .get(v)
                        .map(|s| (2, NestBottom::Scalar(*s)))
                })
                .or_else(|| {
                    self.local_list_elem_tuple
                        .get(v)
                        .map(|slots| (1, NestBottom::Tuple(slots.clone())))
                })
                .or_else(|| {
                    self.local_list_elem_valtype
                        .get(v)
                        .map(|s| (1, NestBottom::Scalar(*s)))
                }),
            Expr::List(_) => self.literal_nesting(e),
            Expr::Call { name, args } if name == "list.at" && args.len() == 2 => {
                match self.list_nesting(&args[0]) {
                    Some((d, b)) if d >= 2 => Some((d - 1, b)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// `(depth, bottom)` of a list LITERAL, computed recursively from its first
    /// element: a nested list adds a level; a tuple or scalar is the bottom.
    fn literal_nesting(&self, e: &Expr) -> Option<(usize, NestBottom)> {
        if let Expr::List(items) = e {
            match items.first() {
                Some(inner @ Expr::List(_)) => {
                    self.literal_nesting(inner).map(|(d, b)| (d + 1, b))
                }
                Some(Expr::Tuple(slots)) => Some((
                    1,
                    NestBottom::Tuple(slots.iter().map(|x| self.val_type_of(x)).collect()),
                )),
                Some(first) => match self.val_type_of(first) {
                    ValType::Other => None,
                    s => Some((1, NestBottom::Scalar(s))),
                },
                None => None,
            }
        } else {
            None
        }
    }

    fn elem_val_type_of(&self, iter: &Expr) -> ValType {
        match iter {
            // Builtins that yield `List(String)` regardless of input. (`list` is
            // the Dir directory listing.)
            Expr::Call { name, .. }
                if name == "string.split"
                    || name == "string.chars"
                    || name == "list" =>
            {
                ValType::Str
            }
            // `values(d)` yields a list of the Dict's values; carry their type so
            // `for v in values(d)` recovers an Int value as i64.
            Expr::Call { name, args } if name == "dict.values" && args.len() == 1 => match &args[0] {
                Expr::Var(d) => self
                    .local_dict_value_valtype
                    .get(d)
                    .copied()
                    .unwrap_or(ValType::Other),
                _ => ValType::Other,
            },
            // `keys(d)` yields a list of the Dict's keys; carry their type so
            // `for k in keys(d)` can in turn use `k` as a Dict key (e.g.
            // `get_or(d, k, 0)`) without the key type going unknown.
            Expr::Call { name, args } if name == "dict.keys" && args.len() == 1 => match &args[0] {
                Expr::Var(d) => self
                    .local_dict_key_valtype
                    .get(d)
                    .copied()
                    .unwrap_or(ValType::Other),
                _ => ValType::Other,
            },
            // `at(L, i)` where `L` is itself a (possibly deeply) nested list: the
            // element is a scalar exactly when the at-result is a depth-1 list.
            // Peeling via `list_nesting` handles any nesting depth.
            Expr::Call { name, args } if name == "list.at" && args.len() == 2 => {
                match self.list_nesting(iter) {
                    Some((1, NestBottom::Scalar(s))) => s,
                    _ => ValType::Other,
                }
            }
            // A function declared `-> List(<scalar>)` carries its element type.
            Expr::Call { name, .. } => self
                .fn_ret_list_elem_valtype
                .get(name)
                .copied()
                .unwrap_or(ValType::Other),
            Expr::List(items) => items
                .first()
                .map(|e| self.val_type_of(e))
                .unwrap_or(ValType::Other),
            Expr::Var(v) => {
                if let Some(s) = self.local_list_elem_valtype.get(v) {
                    *s
                } else if let Some((1, NestBottom::Scalar(s))) = self.local_list_nesting.get(v) {
                    // A nested-list var that is now a depth-1 list of a scalar.
                    *s
                } else {
                    ValType::Other
                }
            }
            _ => ValType::Other,
        }
    }

    /// The WASM kind of the elements iterated by `for x in iter`: a record
    /// element is a pointer (i32); otherwise the element's value type maps via
    /// `valtype_kind` (Int->i64, Float->f64, String/Bool->i32, generic->i64).
    fn iter_elem_kind(&self, iter: &Expr) -> Kind {
        if self.elem_record_type_of(iter).is_some() {
            Kind::I32
        } else {
            valtype_kind(self.elem_val_type_of(iter))
        }
    }

    /// Record the kinds of all `let`/pattern-bound locals in a body.
    fn infer_locals(&mut self, block: &Block) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    // Infer the value's nested bindings FIRST (e.g. a `match`'s
                    // Some/Ok payload vars), so this binding's own kind/type —
                    // computed from `value` below — sees them. Otherwise
                    // `let ok = match f() { Some(n) -> n ... }` would type `ok` as
                    // i32 (n not yet known to be i64) while the match emits i64.
                    self.infer_locals_expr(value);
                    let k = self.kind_of(value);
                    self.locals.insert(name.clone(), k);
                    let vt = self.val_type_of(value);
                    self.local_val_types.insert(name.clone(), vt);
                    let evt = self.elem_val_type_of(value);
                    if evt != ValType::Other {
                        self.local_list_elem_valtype.insert(name.clone(), evt);
                    }
                    // A binding to an `Option`/`Result` of a scalar records the
                    // payload's value type, so `match name { Some(n) -> n }` binds
                    // `n` at the right width.
                    if let Some(pvt) = self.match_payload_valtype(value) {
                        self.local_payload_valtype.insert(name.clone(), pvt);
                    }
                    // A binding to an `insert(...)` (or another Dict var) records
                    // the Dict's value type, so `values(d)`/`for v in values(d)`
                    // recover an Int value as i64.
                    if let Some(vvt) = self.dict_value_valtype_of(value) {
                        self.local_dict_value_valtype.insert(name.clone(), vvt);
                    }
                    if let Some(kvt) = self.dict_key_valtype_of(value) {
                        self.local_dict_key_valtype.insert(name.clone(), kvt);
                    }
                    // A binding to a closure value records its call-return kind, so
                    // `let f = make(...)` then `f(x)` recovers the result width.
                    if let Some(rk) = self.closure_ret_kind_of(value) {
                        self.local_fn_ret_kind.insert(name.clone(), rk);
                    }
                    // A binding to a tuple literal records its element slot value
                    // types, so a later `let (a, b) = name` types `a`/`b` (and
                    // gives Float/Int elements the right kind).
                    if let Expr::Tuple(items) = value {
                        self.local_tuple_slots
                            .insert(name.clone(), items.iter().map(|e| self.val_type_of(e)).collect());
                    } else if let Expr::Call { name: fname, args } = value {
                        if fname == "list.at" && args.len() == 2 {
                            // `at(list_of_tuples, i)`: the result tuple's slots are
                            // the list's element-tuple slot types.
                            if let Expr::Var(list) = &args[0] {
                                if let Some(slots) = self.local_list_elem_tuple.get(list).cloned() {
                                    self.local_tuple_slots.insert(name.clone(), slots);
                                }
                            }
                        } else if let Some(slots) = self.fn_ret_tuple_slots.get(fname) {
                            // A binding to a tuple-returning call records its slots,
                            // so `let (a, b) = name` destructures at i64 for Int.
                            self.local_tuple_slots.insert(name.clone(), slots.clone());
                        }
                    }
                    // A list literal of tuples records its element-tuple slot types,
                    // so `at(name, i)` then `let (a, b) = ...` destructures at the
                    // right widths.
                    // A binding to a list whose elements are tuples (a literal of
                    // tuples, `pairs(d)`, or a `List((..))`-returning call like a
                    // monomorphized `zip`) records the element-tuple slot types, so
                    // `at(name, i)` / `for t in name` destructure at i64 for Int.
                    if let Some(slots) = self.list_elem_tuple_slots(value) {
                        self.local_list_elem_tuple.insert(name.clone(), slots);
                    }
                    // A binding to a nested list records its `(depth, bottom)` (a
                    // literal, a peeled `at(...)`, or another nested-list var), so
                    // `at`/`for` through `name` recover the bottom element at any
                    // depth.
                    if let Some(n) = self.list_nesting(value) {
                        self.local_list_nesting.insert(name.clone(), n);
                    }
                    // A binding to a `List(Record)` (literal, a `List(Record)`-
                    // returning call, or another such variable) records its
                    // element record type, so `for x in name` and `at(name, i)`
                    // resolve fields.
                    if let Some(elem) = self.elem_record_type_of(value) {
                        self.local_list_elem.insert(name.clone(), elem);
                    }
                    // A binding to an `Option(Record)`/`Result(Record, _)` records
                    // its payload type, so `match name { Some(a) -> a.field }`
                    // resolves `a`.
                    if let Some(rec) = self.match_payload_record(value) {
                        self.local_payload_records.insert(name.clone(), rec);
                    }
                    // Remember the binding's record type (if any) so `name.field`
                    // resolves — see `record_type_of` for the cases handled.
                    if let Some(ty) = self.record_type_of(value) {
                        self.local_records.insert(name.clone(), ty);
                    }
                    // Capture the binding's fully-resolved compound shape (from the
                    // RHS), so `eq_shape_of(Var)` resolves a tuple/list whose slots
                    // are themselves compound — which the scalar-only slot tables
                    // cannot. Authoritative: it is exactly `eq_shape_of(rhs)`.
                    if let Some(shape) = self.eq_operand_shape(value) {
                        if shape.is_compound() {
                            self.local_shape.insert(name.clone(), shape);
                        }
                    }
                }
                Stmt::Assign { name, value } => {
                    // `d = insert(d, k, v)` carries the Dict's key/value types forward.
                    if let Some(vvt) = self.dict_value_valtype_of(value) {
                        self.local_dict_value_valtype.insert(name.clone(), vvt);
                    }
                    if let Some(kvt) = self.dict_key_valtype_of(value) {
                        self.local_dict_key_valtype.insert(name.clone(), kvt);
                    }
                    // Refresh the captured compound shape: a reassignment can pin
                    // a payload the original binding could not (`var o = None`
                    // then `o = Some("s")`), and the later, fuller pin is the one
                    // comparisons after the assignment must use.
                    if let Some(shape) = self.eq_operand_shape(value) {
                        if shape.is_compound() {
                            self.local_shape.insert(name.clone(), shape);
                        }
                    }
                    self.infer_locals_expr(value);
                }
                Stmt::LetPattern { pattern, value } => {
                    // The common `let (a, b, …) = e` shape — a flat tuple of plain
                    // variables — gets precise per-slot value types (driving
                    // `to_string`, Dict keys, and the WASM kind Int->i64). Any other
                    // (nested / ctor / list) irrefutable pattern degrades to Other
                    // for its bindings: still correct (the values lower via
                    // `lower_pattern`), just without the slot-type refinement.
                    if let Pattern::Tuple(subs) = pattern {
                        let flat_names: Option<Vec<&String>> = subs
                            .iter()
                            .map(|p| match p {
                                Pattern::Var(n) => Some(n),
                                Pattern::Wildcard => None, // placeholder handled below
                                _ => None,
                            })
                            .collect::<Option<Vec<_>>>()
                            .filter(|_| subs.iter().all(|p| matches!(p, Pattern::Var(_))));
                        if let Some(names) = flat_names {
                            let vts: Vec<ValType> = if let Expr::Tuple(items) = value {
                                if items.len() == names.len() {
                                    items.iter().map(|it| self.val_type_of(it)).collect()
                                } else {
                                    vec![ValType::Other; names.len()]
                                }
                            } else if let Expr::Var(p) = value {
                                self.local_tuple_slots
                                    .get(p)
                                    .filter(|s| s.len() == names.len())
                                    .cloned()
                                    .unwrap_or_else(|| vec![ValType::Other; names.len()])
                            } else if let Expr::Call { name: fname, args } = value {
                                if fname == "list.at" && args.len() == 2 {
                                    self.list_elem_tuple_slots(&args[0])
                                        .filter(|s| s.len() == names.len())
                                        .unwrap_or_else(|| vec![ValType::Other; names.len()])
                                } else {
                                    self.fn_ret_tuple_slots
                                        .get(fname)
                                        .filter(|s| s.len() == names.len())
                                        .cloned()
                                        .unwrap_or_else(|| vec![ValType::Other; names.len()])
                                }
                            } else {
                                vec![ValType::Other; names.len()]
                            };
                            for (n, vt) in names.iter().zip(&vts) {
                                self.local_val_types.insert((*n).clone(), *vt);
                                self.locals.insert((*n).clone(), valtype_kind(*vt));
                            }
                            // `let (xs, ys) = f(...)` returning `(List(T), List(U))`:
                            // record each destructured list var's element type.
                            if let Expr::Call { name: fname, .. } = value {
                                if let Some(elems) = self.fn_ret_tuple_slot_list_elem.get(fname) {
                                    for (n, elem) in names.iter().zip(elems) {
                                        if let Some(vt) = elem {
                                            self.local_list_elem_valtype.insert((*n).clone(), *vt);
                                        }
                                    }
                                }
                            }
                            self.infer_locals_expr(value);
                            continue;
                        }
                    }
                    // General irrefutable pattern: bind every name as Other.
                    let mut names = Vec::new();
                    witchy_syntax::ast::pattern_binds(pattern, &mut names);
                    for n in &names {
                        self.local_val_types.insert(n.clone(), ValType::Other);
                        self.locals.insert(n.clone(), Kind::I32);
                    }
                    self.infer_locals_expr(value);
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => self.infer_locals_expr(e),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
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
                // A range `for` counts: an i64 counter and end bound, and the
                // loop var is the i64 Int counter. No list is materialized.
                if let Expr::Range { lo, hi, .. } = iter.as_ref() {
                    self.locals.insert(format!("__forctr_{var}"), Kind::I64);
                    self.locals.insert(format!("__forend_{var}"), Kind::I64);
                    self.locals.insert(var.clone(), Kind::I64);
                    // The loop var is an Int, so a tuple/record built from it (e.g.
                    // `(a, b)` in a comprehension) stores it as an i64 slot, not i32.
                    self.local_val_types.insert(var.clone(), ValType::Int);
                    self.infer_locals_expr(lo);
                    self.infer_locals_expr(hi);
                    self.infer_locals(body);
                    return;
                }
                // The two scratch locals (list pointer, index) are i32; the loop
                // var takes the element's kind (Int->i64, Float->f64, else i32).
                self.locals.insert(format!("__forlist_{var}"), Kind::I32);
                self.locals.insert(format!("__fori_{var}"), Kind::I32);
                self.locals.insert(var.clone(), self.iter_elem_kind(iter));
                // The loop variable's value type is the iterated list's element
                // type, so e.g. `for w in split(...)` knows `w` is a String.
                let evt = self.elem_val_type_of(iter);
                if evt != ValType::Other {
                    self.local_val_types.insert(var.clone(), evt);
                }
                // Iterating a list of tuples: the loop var is a tuple with the
                // element's slot types, so a `let (k, v) = p` inside can type its
                // bindings (and `k == key` use string, not pointer, comparison).
                if let Some(slots) = self.list_elem_tuple_slots(iter) {
                    self.local_tuple_slots.insert(var.clone(), slots);
                }
                // Iterating a nested list: the loop var is itself a list one level
                // shallower, so an inner `for x in var` / `at(var, i)` recovers the
                // scalar element at any depth.
                if let Some((d, b)) = self.list_nesting(iter) {
                    if d >= 2 {
                        self.local_list_nesting.insert(var.clone(), (d - 1, b));
                    }
                }
                self.infer_locals_expr(iter);
                self.infer_locals(body);
            }
            Expr::Match { scrutinee, arms } => {
                // The record an Option/Result scrutinee's Some/Ok carries, if known.
                let payload = self.match_payload_record(scrutinee);
                // (RFC-0052) A TOP-LEVEL variable/wildcard pattern binds the WHOLE
                // scrutinee, so it takes the scrutinee's kind — crucially F64 for a
                // `match <float>: x -> …`, which otherwise defaulted to i32 and made
                // the WIR ill-typed (the check-passes/codegen-fails Float hole).
                let scrut_kind = self.kind_of(scrutinee);
                for arm in arms {
                    // Pattern-bound vars are i32 (floats aren't stored in records),
                    // except a top-level whole-scrutinee binding (handled below).
                    let mut pvars = Vec::new();
                    collect_pattern_vars(&arm.pattern, &mut pvars);
                    for v in pvars {
                        self.locals.insert(v, Kind::I32);
                    }
                    if let Pattern::Var(v) = &arm.pattern {
                        self.locals.insert(v.clone(), scrut_kind);
                        self.local_val_types.insert(v.clone(), self.val_type_of(scrutinee));
                    }
                    // A var bound to a record-typed constructor field resolves
                    // `.field` in the arm body (concrete field types only).
                    let mut recbinds = Vec::new();
                    self.pattern_record_binds(&arm.pattern, &mut recbinds);
                    for (v, rec) in recbinds {
                        self.local_records.insert(v, rec);
                    }
                    // `Some(a)`/`Ok(a)` over an Option/Result of a record binds
                    // `a` to that record.
                    if let Some(rec) = &payload {
                        if let Pattern::Ctor { name, args } = &arm.pattern {
                            if (name == "Some" || name == "Ok") && args.len() == 1 {
                                if let Pattern::Var(v) = &args[0] {
                                    self.local_records.insert(v.clone(), rec.clone());
                                }
                            }
                        }
                    }
                    // `Some(n)`/`Ok(n)` over an Option/Result of a SCALAR binds `n`
                    // at the payload's kind (an `Int` payload as i64, not the
                    // default i32 that would truncate a big value).
                    if let Some(pvt) = self.match_payload_valtype(scrutinee) {
                        if let Pattern::Ctor { name, args } = &arm.pattern {
                            if (name == "Some" || name == "Ok") && args.len() == 1 {
                                if let Pattern::Var(v) = &args[0] {
                                    self.locals.insert(v.clone(), valtype_kind(pvt));
                                    self.local_val_types.insert(v.clone(), pvt);
                                }
                            }
                        }
                    }
                    // ANY constructor pattern with DECLARED field types binds
                    // its variables at those types (a `JsonFloat(x)` payload is
                    // f64, not the default i32 that would mangle the bits).
                    if let Pattern::Ctor { name, args } = &arm.pattern {
                        let fields = self
                            .ctors
                            .get(name)
                            .map(|&(tag, _)| tag as usize)
                            .and_then(|tag| {
                                let ty = self.ctor_type_name.get(name)?;
                                self.adt_variants.get(ty)?.get(tag).cloned()
                            });
                        if let Some(fields) = fields {
                            for (i, sub) in args.iter().enumerate() {
                                let (Pattern::Var(v), Some(ft)) = (sub, fields.get(i)) else {
                                    continue;
                                };
                                let vt = ty_to_valtype(ft);
                                if vt != ValType::Other
                                    && !self.local_val_types.contains_key(v)
                                {
                                    self.locals.insert(v.clone(), valtype_kind(vt));
                                    self.local_val_types.insert(v.clone(), vt);
                                }
                            }
                        }
                    }
                    self.infer_locals_expr(&arm.body);
                }
            }
            // Recurse into every sub-expression so a desugared range/comprehension
            // Block nested in an argument, operand, etc. still has its loop/let
            // locals inferred (otherwise their kinds default to i32).
            Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
                for a in args {
                    self.infer_locals_expr(a);
                }
            }
            Expr::Apply { func, args } => {
                self.infer_locals_expr(func);
                for a in args {
                    self.infer_locals_expr(a);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.infer_locals_expr(lhs);
                self.infer_locals_expr(rhs);
            }
            Expr::Unary { expr, .. } => self.infer_locals_expr(expr),
            Expr::Tuple(xs) | Expr::List(xs) => {
                for x in xs {
                    self.infer_locals_expr(x);
                }
            }
            Expr::Field { base, .. } => self.infer_locals_expr(base),
            Expr::Try(inner) => self.infer_locals_expr(inner),
            Expr::RecordUpdate { base, fields } => {
                self.infer_locals_expr(base);
                for (_, v) in fields {
                    self.infer_locals_expr(v);
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

    fn compile_function(&mut self, f: &Function) -> Result<(), CodegenError> {
        self.locals.clear();
        self.field_caps.clear();
        self.local_records.clear();
        self.local_list_elem.clear();
        self.local_val_types.clear();
        self.local_list_elem_valtype.clear();
        self.local_list_elem_tuple.clear();
        self.local_tuple_slots.clear();
        self.local_shape.clear();
        self.local_payload_valtype.clear();
        self.local_dict_value_valtype.clear();
        self.local_dict_key_valtype.clear();
        self.local_list_elem_list_valtype.clear();
        self.local_list_nesting.clear();
        self.local_fn_ret_kind.clear();
        for p in &f.params {
            let k = p.ty.as_ref().map(ty_kind).unwrap_or(Kind::I32);
            self.locals.insert(p.name.clone(), k);
            if let Some(t) = &p.ty {
                self.local_val_types.insert(p.name.clone(), ty_to_valtype(t));
            }
            // A function-typed parameter (`f: fn(...) -> RET`): remember RET's kind
            // so a closure call `f(x)` recovers the result at the right width.
            if let Some(Type::Fn(_, ret)) = &p.ty {
                self.local_fn_ret_kind.insert(p.name.clone(), ty_kind(ret));
            }
            // A nested-list parameter (`m: List(List(Int))`): record its
            // `(depth, scalar)` so `at(at(m, i), j)` recovers an Int as i64.
            if let Some(n) = p.ty.as_ref().and_then(ty_list_nesting) {
                if n.0 >= 2 {
                    self.local_list_nesting.insert(p.name.clone(), n);
                }
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
                        // List of tuples: remember the element's slot value types
                        // so `for p in xs` then `let (k, v) = p` types `k`/`v`.
                        if let Type::Tuple(slots) = elem {
                            self.local_list_elem_tuple
                                .insert(p.name.clone(), slots.iter().map(ty_to_valtype).collect());
                        }
                    }
                }
                _ => {}
            }
            // A compound-typed parameter resolves its full structural shape from
            // the declared type (authoritative), so `__render(p)` / `p == q`
            // work even when the slots are themselves compound. Bare type
            // variables resolve to nothing, preserving the loud error.
            if let Some(shape) = p.ty.as_ref().and_then(|t| self.eq_shape_of_type(t)) {
                if shape.is_compound() {
                    self.local_shape.insert(p.name.clone(), shape);
                }
            }
        }
        // Rename shadowing bindings to unique names so function-wide locals
        // don't alias (the interpreter scopes lexically; this preserves that).
        self.cur_fn_name = f.name.clone();
        self.cur_fn_has_type_vars = f.params.iter().any(|p| {
            matches!(&p.ty, Some(Type::Named(n, args))
                if args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()) && !n.contains('.'))
                || matches!(&p.ty, Some(Type::Named(_, args))
                    if args.iter().any(type_has_var))
        });
        // Bodies are pre-renamed at module level (alpha_rename_module), so
        // this is the exact instance the type table and facts are keyed to.
        let renamed = &f.body;
        self.infer_locals(renamed);

        // The own-ABI: a single `own` collection parameter whose buffer may be
        // returned carries the caller's ownership token across the call (an extra
        // i32 cap param + i32 cap result, built into the WirFunc signature by
        // `assemble_wir_func`). Decided from the module summaries, so every compile
        // of this module agrees on the signature.
        self.cur_fn_own_param = self
            .summaries
            .own_abi(&f.name)
            .and_then(|i| f.params.get(i))
            .map(|p| p.name.clone());
        // Result = the normal return value, then one slot per `var` parameter
        // (moved back out to the caller).
        let ret_kind = match &f.ret {
            Some(t) => ty_kind(t),
            None => self.block_kind(renamed),
        };
        self.cur_fn_ret_kind = ret_kind;
        // (RFC-0043) A mutator's `var` receiver (first param) is NOT a
        // procedure-style write-back channel — its write-back is delivered by the
        // `xs = f(xs, …)` statement rewrite, so the receiver is lowered like a plain
        // value param (no extra move-out result). Only a Nil-returning `var`
        // procedure uses the multi-value write-back ABI.
        let mutator_receiver = f.is_mutator();
        self.cur_fn_var_params = f
            .params
            .iter()
            .enumerate()
            .filter(|(i, p)| p.convention == Convention::Var && !(*i == 0 && mutator_receiver))
            .map(|(_, p)| p.name.clone())
            .collect();
        self.cur_fn_var = !self.cur_fn_var_params.is_empty();

        self.begin_unit(renamed);

        self.apply_level = 0;
        self.wm_level = 0;
        // Lower the body straight to WIR (`assemble_wir_module` sets `collect_wir`
        // for the function being compiled). `lower_block` is the block lowering: it
        // walks the statements and produces the `WirSeq` the encoder consumes.
        self.captured_seq = None;
        if let Some(seq) = self.lower_block(renamed) {
            self.captured_seq = Some(seq);
        }
        // A hard rejection raised during lowering (e.g. a closure assigning a
        // captured var) aborts the whole compile with a diagnostic.
        if let Some(e) = self.reject_reason.take() {
            return Err(e);
        }
        let block_kind = self.block_kind(renamed);
        // If the whole body lowered to WIR and the function uses neither the
        // var move-out ABI nor the own-cap ABI (the binary sink models neither
        // yet), keep a `WirFunc` so `compile_module_binary` can encode it.
        if let Some(seq) = self.captured_seq.take() {
            // The whole function lowered + captured (binary path). lower_block
            // deferred facts consumption to here (it is invoked many times per
            // compile, so consuming there over-counts); consume the unit's facts
            // EXACTLY once now — equivalent to the legacy compiling every
            // statement. (The legacy fallback consumes for functions that don't
            // capture; the two are mutually exclusive.)
            if let Some(top) = self.facts_stack.last_mut() {
                let (ke, se) = (top.0.kill_entries, top.0.site_entries);
                top.1 = ke;
                top.2 = se;
            }
            // Var AND own-ABI functions are captured now: the multi-value
            // move-out / own-cap signatures are built in `assemble_wir_func`. A
            // function whose body lowered into a signature the call sites don't
            // match (e.g. an early `return` that can't carry the extra results)
            // is rejected by `wasmparser::validate`, so the whole module falls
            // back to WAT gracefully.
            {
                let seq = Self::convert_block_tail(seq, block_kind, ret_kind);
                let wf = self.assemble_wir_func(f, ret_kind, seq);
                self.wir_funcs.insert(f.name.clone(), wf);
            }
        }
        self.finish_unit(&f.name)?;
        self.cur_fn_own_param = None;
        Ok(())
    }

    /// Build the `WirFunc` for a fully-lowered function: its params, the body
    /// locals (mirroring `compile_function`'s header — the same `let`s and
    /// scratch slots the WIR body may reference), its single result, and the
    /// captured body. `raw_body: None` — this is a node-walked function.
    fn assemble_wir_func(
        &self,
        f: &Function,
        ret_kind: Kind,
        body: witchy_wir::wir::WirSeq,
    ) -> witchy_wir::wir::WirFunc {
        use witchy_wir::wir::{WirFunc, WirLocal, WirTy};
        // `.kind()` is all the encoder reads: `Bool` => i32, `Int` => i64.
        let i32t = || WirTy::Bool;
        let i64t = || WirTy::Int;
        let mut params: Vec<WirLocal> = f
            .params
            .iter()
            .map(|p| WirLocal {
                name: p.name.clone(),
                ty: Self::wir_ty_for_kind(self.locals.get(&p.name).copied().unwrap_or(Kind::I32)),
            })
            .collect();
        // The own-ABI: the owned buffer's caller-supplied ownership token is an
        // EXTRA trailing i32 param (mirroring the WAT header's `$p__cap`), so it
        // is a param here, NOT a local (skipped in the cap-slot loop below).
        if let Some(p) = &self.cur_fn_own_param {
            params.push(WirLocal { name: format!("{p}__cap"), ty: i32t() });
        }
        let mut locals: Vec<WirLocal> = Vec::new();
        let mut lets = Vec::new();
        collect_let_names(&f.body, &mut lets);
        lets.sort();
        lets.dedup();
        for name in &lets {
            let k = self.locals.get(name).copied().unwrap_or(Kind::I32);
            locals.push(WirLocal { name: name.clone(), ty: Self::wir_ty_for_kind(k) });
        }
        // (RFC-0027) Scalar-replaced aggregates: each field lives in a `${name}$<i>`
        // i64-slot local instead of a heap object. (The plain `${name}` local from
        // the loop above is then simply unused.)
        let mut sroa: Vec<(&String, &usize)> = self.sroa_active.iter().collect();
        sroa.sort();
        for (name, count) in sroa {
            for i in 0..*count {
                locals.push(WirLocal { name: format!("{name}${i}"), ty: i64t() });
            }
        }
        // (RFC-0028) Confined slice views: source pointer + raw lo/hi bounds, all
        // i32. (The plain `${name}` local from the loop above is then unused.)
        let mut views: Vec<&String> = self.view_active.iter().collect();
        views.sort();
        for name in views {
            locals.push(WirLocal { name: format!("{name}$src"), ty: i32t() });
            locals.push(WirLocal { name: format!("{name}$lo"), ty: i32t() });
            locals.push(WirLocal { name: format!("{name}$hi"), ty: i32t() });
        }
        // Shadow `${v}__cap` ownership-token slots for the in-place accumulators.
        // The own-ABI parameter's token is a param (above), not a local.
        let mut cap_vars: Vec<&String> = self.inplace_push.iter().collect();
        cap_vars.sort();
        for v in cap_vars {
            if Some(v.as_str()) != self.cur_fn_own_param.as_deref() {
                locals.push(WirLocal { name: format!("{v}__cap"), ty: i32t() });
            }
        }
        // (RFC-0033 R2) field-buffer capacity tokens for in-place field-path pushes.
        let mut field_caps: Vec<&String> = self.field_caps.iter().collect();
        field_caps.sort();
        for fc in field_caps {
            locals.push(WirLocal { name: fc.clone(), ty: i32t() });
        }
        locals.push(WirLocal { name: "__witchy_owncap".into(), ty: i32t() });
        locals.push(WirLocal { name: TUPLE_TMP.into(), ty: i32t() });
        locals.push(WirLocal { name: TRY_TMP.into(), ty: i32t() });
        locals.push(WirLocal { name: TYPECHECK_TMP.into(), ty: i32t() });
        locals.push(WirLocal { name: MATCH_TMP.into(), ty: i64t() });
        locals.push(WirLocal { name: MATCH_RES.into(), ty: i64t() });
        for i in 0..SCRUT_POOL {
            locals.push(WirLocal { name: format!("__witchy_scrut_save_{i}"), ty: i64t() });
        }
        locals.push(WirLocal { name: SECRET_TMP.into(), ty: i32t() });
        // Scratch slots for the inlined in-place `set_at` fast path (index i32,
        // value i64): the common in-bounds + owned case stores directly without a
        // `$list_set_cap` call; the helper is only invoked for OOB / re-own.
        locals.push(WirLocal { name: "__witchy_set_idx".into(), ty: i32t() });
        locals.push(WirLocal { name: "__witchy_set_val".into(), ty: i64t() });
        // (RFC-0016) RC-floor free-at-overwrite scratch: the freshly-allocated
        // buffer (a heap pointer) before the old one is freed and the var rebound.
        locals.push(WirLocal { name: "__rc_new".into(), ty: i32t() });
        for i in 0..WM_POOL {
            locals.push(WirLocal { name: format!("__witchy_wm_{i}"), ty: i32t() });
        }
        for i in 0..APPLY_POOL {
            locals.push(WirLocal { name: format!("__witchy_call_{i}"), ty: i32t() });
        }
        for i in 0..REUSE_POOL {
            locals.push(WirLocal { name: format!("__witchy_reuse_{i}"), ty: i64t() });
        }
        // An `var` function returns its declared value FOLLOWED BY one result per
        // var param (the multi-value move-out ABI, mirroring `var_epilogue` on the
        // WAT path): after the declared tail, push each var param's final value in
        // declaration order. The call site (`CallStoreMulti`) pops them back into the
        // caller's variables.
        let mut ret = vec![Self::wir_ty_for_kind(ret_kind)];
        let mut body = body;
        for name in &self.cur_fn_var_params {
            let k = self.locals.get(name).copied().unwrap_or(Kind::I32);
            ret.push(Self::wir_ty_for_kind(k));
            body.push(witchy_wir::wir::WirNode::Push(witchy_wir::wir::WirExpr::GetLocal(name.clone())));
        }
        // own-ABI: append the returned buffer's ownership token (one i32 result).
        // It is `$p__cap` when the function returns its own buffer AND that buffer
        // is an in-place accumulator; otherwise 0 (the caller re-owns on its next
        // mutation — one copy, never corruption). Mirrors `own_cap_push`.
        if let Some(p) = self.cur_fn_own_param.clone() {
            ret.push(i32t());
            let returns_own = match f.body.stmts.last() {
                Some(Stmt::Expr(Expr::Var(v))) => *v == p,
                Some(Stmt::Expr(Expr::Unary { op: UnOp::Move, expr })) => {
                    matches!(expr.as_ref(), Expr::Var(v) if *v == p)
                }
                _ => false,
            };
            let cap = if returns_own && self.inplace_push.contains(&p) {
                witchy_wir::wir::WirExpr::GetLocal(format!("{p}__cap"))
            } else {
                witchy_wir::wir::WirExpr::ConstI32(0)
            };
            body.push(witchy_wir::wir::WirNode::Push(cap));
        }
        WirFunc {
            name: f.name.clone(),
            params,
            ret,
            locals,
            body,
            raw_body: None,
        }
    }

    /// Begin a compile unit (function/lambda body): run the
    /// uniqueness analysis and install its facts.
    fn begin_unit(&mut self, body: &Block) {
        let facts = if force_copy_mode() {
            analysis::Facts::default()
        } else {
            analysis::analyze(body, &self.summaries)
        };
        self.inplace_push = facts
            .accumulators
            .iter()
            .cloned()
            .collect();
        // (RFC-0033 R2) `(var, field)` pairs whose list buffer is never aliased and may
        // be grown in place. Consumed only inside the in-place RecordUpdate arm, which is
        // itself gated on the InPlace opt, so no separate de-opt lever is needed.
        self.field_push_safe = analysis::field_push_safe_set(body);
        // (RFC-0027) Escape-driven SROA: frame-confined aggregates used only via
        // field access become field-locals. Gated on the `sroa` lever and off in
        // forced-copy mode (so the de-opt differential covers it).
        self.sroa_active.clear();
        self.sroa_candidates = if !force_copy_mode()
            && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::Sroa)
        {
            crate::escape::sroa_candidates_block(body)
        } else {
            HashSet::new()
        };
        // (RFC-0028) Confined slice views: elide the `list.slice` copy for a
        // read-only window over an unmutated source. Gated on the `views` lever and
        // off in forced-copy mode (so the de-opt differential exercises the copy).
        self.view_active.clear();
        self.view_candidates = if !force_copy_mode()
            && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::Views)
        {
            crate::escape::confined_slice_candidates_block(body)
        } else {
            HashSet::new()
        };
        // (RFC-0027) Packed layouts for confined record-list literals. ONE flat
        // inline buffer serves two cases through the SAME codegen path:
        //   * INFERENCE (opt-in `unbox`): any confined list of a packable record.
        //   * DECLARED `packed`: a list of a `type P packed:` — the layout is part
        //     of the type, so it is GUARANTEED whenever `unbox` is on.
        // The confinement query is identical for both; the declared case adds the
        // layout CONTRACT (the "pack or cleanly reject every site" soundness rule):
        // a declared-`packed` list that is NOT confined — used where the flat layout
        // has no representation (passed/returned whole, compared, rendered, iterated
        // with `for`, sent over a channel, or flowed into a generic `List(a)`) — is a
        // clean compile error, never a silent fall-back to the boxed layout the
        // programmer declared away.
        self.packed_active.clear();
        let unbox_on = witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::Unbox);
        let confined_packed = if unbox_on || !self.packed_types.is_empty() {
            crate::escape::confined_record_list_candidates_block(body)
        } else {
            HashSet::new()
        };
        // The declared-`packed` contract is enforced regardless of the lever: the
        // lever chooses the REPRESENTATION (flat vs boxed), never the contract or
        // observable behavior. So a misused declared-`packed` list is rejected even
        // with `unbox` off, where a *confined* one merely stays boxed.
        if !self.packed_types.is_empty() {
            for (name, ctor) in crate::escape::record_list_lets_block(body) {
                let ty = self.ctor_type_name.get(&ctor).cloned().unwrap_or(ctor);
                if self.packed_types.contains(&ty) && !confined_packed.contains(&name) {
                    self.reject_reason.get_or_insert_with(|| CodegenError {
                        message: format!(
                            "the list `{name}` of declared-`packed` type `{ty}` is used in a position the flat \
                             inline layout cannot support — a `packed` list must be a confined local read only via \
                             `list.length` and `list.at(_, i).field` (it may not be passed or returned whole, \
                             compared, rendered, iterated with `for`, sent over a channel, or used as a generic \
                             `List(a)`); drop `packed` from `{ty}` to use the uniform boxed layout there"
                        ),
                    });
                }
            }
        }
        self.packed_candidates = if unbox_on { confined_packed } else { HashSet::new() };
        // (RFC-0016) In-place reuse of a confined, never-aliased list `var` at a
        // same-length reassignment — the never-OOM fix for build-and-drop loops.
        // Gated on `rc-elide`; off in forced-copy mode so the de-opt sweep exercises
        // the leaking path.
        self.reuse_vars = if !force_copy_mode()
            && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::RcElide)
        {
            crate::escape::confined_inplace_reuse_vars_block(body)
        } else {
            HashSet::new()
        };
        // (RFC-0016) RC-floor reclamation: free a confined heap `var`'s OLD buffer
        // when it is overwritten by a freshly-allocated one. Opt-in via `rc-floor`;
        // off in forced-copy mode so the de-opt sweep exercises the leaking path.
        self.rc_floor_vars = if !force_copy_mode()
            && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::RcFloor)
        {
            crate::escape::confined_reassigned_vars_block(body, &self.summaries)
        } else {
            HashSet::new()
        };
        // (RFC-0035 step 3) Populated during this unit's lowering (at each dup-eligible
        // `let x = list.at(...)`); start empty. Nested units save/restore it via SavedScope.
        self.rc_owned_bindings = HashSet::new();
        // (RFC-0034 L3) Closure devirtualization: names bound exactly once and never
        // reassigned, so every call through them reaches the same lambda. The
        // name→index map is filled lazily as each such `let f = <lambda>` lowers.
        // Gated on the `direct-call` lever (off ⇒ empty ⇒ every closure call stays
        // `call_indirect`, which is the de-opt sweep's reference).
        self.devirt_index.clear();
        self.devirt_ok = if witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::DirectCall) {
            collect_devirt_eligible(body)
        } else {
            HashSet::new()
        };
        // (RFC-0062) Closure escape elision: the tier-1 candidate facts. `closure_elide_called`
        // is the general, default-deny non-escape fact (names used ONLY as a direct-call
        // callee this unit); `closure_elide_reassigned` gives the capture-stability guard (a
        // capture reassigned before its call cannot be threaded — the interpreter snapshots
        // captures at creation). A `let f = <lambda>` is elided only when it is in BOTH
        // `devirt_ok` and `closure_elide_called` and none of its captures is reassigned (see
        // the `Stmt::Let` closure-elide branch). Gated on `closure-elide`; empty ⇒ every
        // closure keeps its heap environment (the de-opt reference the differential sweep uses).
        self.thread_index.clear();
        if witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::ClosureElide) {
            self.closure_elide_called = crate::escape::only_directly_called(body);
            self.closure_elide_reassigned = crate::escape::reassigned_names(body);
        } else {
            self.closure_elide_called = HashSet::new();
            self.closure_elide_reassigned = HashSet::new();
        }
        // (RFC-0035) last_use drop points: values proven dead AND freeable (bound to a
        // known heap allocator, never read-again / never aliased / escaped / returned /
        // reassigned / region-confined) get an `$rc_free` after their last use. Gated on
        // `rc-floor`; off ⇒ empty ⇒ no frees (the leak-but-sound reference the differential
        // sweep compares against). The analysis already discharged every double-free /
        // use-after-free obligation, so consuming it here is a direct free of a unique-dead
        // value — no runtime refcount check needed (the RC-elision case, RFC-0016 R2).
        let drops = if !force_copy_mode()
            && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::RcFloor)
        {
            analysis::last_use_drops(body, &self.summaries)
        } else {
            analysis::DropFacts::default()
        };
        self.drop_facts_stack.push(drops);
        self.facts_stack.push((facts, 0, 0));
    }

    /// End a compile unit, asserting every analysis entry was consumed — a
    /// cloned-subtree bug (compiling different AST nodes than were analyzed)
    /// surfaces here as a loud error, never as a lost cap kill.
    fn finish_unit(&mut self, unit: &str) -> Result<(), CodegenError> {
        self.drop_facts_stack.pop();
        let Some((facts, kills, sites)) = self.facts_stack.pop() else {
            return cerr(format!("internal: unbalanced analysis unit in `{unit}`"));
        };
        // The counter assertion is a WAT-path safety net (it proves the compiled
        // statements are the analyzed AST instance, so no cap-kill is lost). On the
        // BINARY path `lower_block` is invoked many times per compile (byte-identity
        // probes, `kind_of`, the legacy fallback's `lower_expr`), so the counters
        // are not a reliable consume-once signal there; the binary path is validated
        // instead by `wasmparser::validate` + the differential oracle tests.
        if !self.collect_wir && (kills != facts.kill_entries || sites != facts.site_entries) {
            return cerr(format!(
                "internal: uniqueness facts for `{unit}` were not fully consumed \
                 ({kills}/{} kills, {sites}/{} sites) — a compiled subtree was not \
                 the analyzed AST instance",
                facts.kill_entries, facts.site_entries
            ));
        }
        Ok(())
    }

    /// The ownership-token kills to emit AFTER a statement (zeroing the cap
    /// of every accumulator the statement may have whole-aliased).
    fn take_kills(&mut self, stmt: &Stmt) -> String {
        let vars: Vec<String> = match self.facts_stack.last_mut() {
            Some((facts, kills, _)) => {
                let vs = facts.kills_after(stmt).to_vec();
                *kills += vs.len();
                vs
            }
            None => return String::new(),
        };
        let mut s = String::new();
        for v in &vars {
            if self.inplace_push.contains(v) {
                s.push_str(&format!("    i32.const 0\n    local.set ${v}__cap\n"));
            }
        }
        s
    }

    /// Lower a SIMPLE block to a `WirSeq`. Only functions without in-place/cap
    /// machinery qualify — no `inplace_push` vars, no `var` params, no own-ABI
    /// param — and only `Let`/`Expr`/`Return` statements; any other shape (the
    /// cap-kill, dict/list fast-path, tuple-destructure, and break/continue cases)
    /// bails to `None`, rejecting the program as not-yet-lowerable.
    ///
    /// Statements are pre-lowered (idempotent `intern`/flag mutations) so that a
    /// non-lowerable expression bails BEFORE any `take_kills` call — `take_kills`
    /// bumps a non-idempotent kill counter, so double-running it would corrupt the
    /// uniqueness accounting.
    /// Lower a block, with TRANSACTIONAL uniqueness-facts accounting: snapshot the
    /// `(kills, sites)` counters on entry and RESTORE them if lowering bails
    /// (`None`). A nested loop-body block may succeed and consume its sites, but if
    /// the enclosing block then bails, the whole tree rolls back so a later attempt
    /// re-consumes from a clean slate (no double-count). Commit (no restore) happens
    /// only on `Some` — the whole block lowered.
    fn lower_block(&mut self, block: &Block) -> Option<witchy_wir::wir::WirSeq> {
        let snap = self.facts_stack.last().map(|(_, k, s)| (*k, *s));
        let result = self.lower_block_inner(block);
        if result.is_none() {
            if let (Some((k, s)), Some(top)) = (snap, self.facts_stack.last_mut()) {
                top.1 = k;
                top.2 = s;
            }
        }
        result
    }

    fn lower_block_inner(&mut self, block: &Block) -> Option<witchy_wir::wir::WirSeq> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        // In-place accumulators lower to the cap ABI (`$list_push_cap` via
        // CallStoreMulti) only in a WIR-collecting scope (`collect_wir`); otherwise
        // this bails. Facts consumption is deferred to `compile_function` on capture
        // (lower_block is invoked many times per compile). The own-ABI never lowers
        // here. `var` lowers ONLY in a WIR-collecting scope (`collect_wir`): the
        // param is a plain mutable local (`n = n + 1` is a `SetLocal`) and its final
        // value is a multi-result at the tail (built by `assemble_wir_func`), the
        // call site writing it back via `CallStoreMulti`. A non-collecting scope
        // can't carry that move-out epilogue (it must run before EVERY early
        // `return`, which the WIR `N::Return` single value can't express), so it
        // bails — leaving the program to be rejected as unsupported.
        if !self.collect_wir
            && (!self.cur_fn_var_params.is_empty()
                || self.cur_fn_own_param.is_some()
                || !self.inplace_push.is_empty())
        {
            return None;
        }
        let mut inplace_sites = 0usize;
        let last = block.stmts.len().saturating_sub(1);
        let mut seq: witchy_wir::wir::WirSeq = Vec::with_capacity(block.stmts.len() + 1);
        let mut tail_is_value = false;
        for (i, stmt) in block.stmts.iter().enumerate() {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    // (RFC-0035 step 3) If this binds a dup-eligible container read
                    // (`let x = list.at(xs, i)` where the element is a provably offset-0 rc
                    // value and rc-floor is on), the read is `$rc_dup`'d in `lower_expr`, so
                    // `x` owns a reference. Record it under the SAME gate as the dup so its
                    // last-use `$rc_drop` (below) fires iff the dup did — a never-dup'd
                    // binding is never dropped (which would underflow the count → UAF).
                    if !force_copy_mode() && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::RcFloor) {
                        if let Expr::Call { name: f, args } = value {
                            if f == "list.at"
                                && args.len() == 2
                                && self.kind_of(value) == Kind::I32
                                && self.list_elem_is_offset0_rc(&args[0])
                            {
                                self.rc_owned_bindings.insert(name.clone());
                            }
                        }
                    }
                    // (RFC-0027) Scalar-replace a frame-confined aggregate: store
                    // each field into a `${name}$<i>` i64-slot local instead of
                    // allocating a heap object. Falls through to the normal path if
                    // any field can't lower (then the name never enters sroa_active,
                    // so its field reads stay heap loads — consistent).
                    // (RFC-0027) Pack a confined list literal of fixed-scalar records
                    // into one flat inline buffer: header = element COUNT (so
                    // `list.length` is unchanged), body = every element's fields in
                    // row-major order. Reuses the checked-heap-correct `$mkN` allocator.
                    let mut packed_done = false;
                    if self.packed_candidates.contains(name) {
                        if let Expr::List(items) = value {
                            if let Some((rec, flat)) = self.packable_record_list(items) {
                                // (RFC-0037 §3) Tag the flat buffer with a DISTINCT `packed:` id
                                // so reading it as a boxed record (or vice versa) is a mismatch.
                                let ptag = type_tag_of(&format!("packed:{rec}"));
                                if let Some(w) = self.lower_aggregate(items.len() as i32, &flat, ptag) {
                                    seq.push(N::SetLocal { local: name.clone(), value: w });
                                    self.packed_active.insert(name.clone(), rec);
                                    packed_done = true;
                                }
                            }
                        }
                    }
                    let mut view_done = false;
                    if !packed_done && self.view_candidates.contains(name) {
                        if let Some((src, lo, hi)) = view_slice_args(value) {
                            // Elide the copy: store the source pointer and the raw
                            // lo/hi bounds (evaluated once, as the materialized slice
                            // would), then read through them. The analysis guarantees
                            // `src` is an unmutated var, so its pointer stays valid.
                            let srcw = self.lower_expr(src)?;
                            let lok = self.kind_of(lo);
                            let low = self.lower_expr(lo)?;
                            let hik = self.kind_of(hi);
                            let hiw = self.lower_expr(hi)?;
                            seq.push(N::SetLocal { local: format!("{name}$src"), value: srcw });
                            seq.push(N::SetLocal {
                                local: format!("{name}$lo"),
                                value: Self::wir_convert(low, lok, Kind::I32),
                            });
                            seq.push(N::SetLocal {
                                local: format!("{name}$hi"),
                                value: Self::wir_convert(hiw, hik, Kind::I32),
                            });
                            // Carry the source's element typing onto the view name so
                            // `list.at(w, _)` recovers the element kind for `FromSlot`.
                            if let Expr::Var(s) = src {
                                if let Some(vt) = self.local_list_elem_valtype.get(s).copied() {
                                    self.local_list_elem_valtype.insert(name.clone(), vt);
                                }
                                if let Some(r) = self.local_list_elem.get(s).cloned() {
                                    self.local_list_elem.insert(name.clone(), r);
                                }
                                if let Some(t) = self.local_list_elem_tuple.get(s).cloned() {
                                    self.local_list_elem_tuple.insert(name.clone(), t);
                                }
                            }
                            self.view_active.insert(name.clone());
                            view_done = true;
                        }
                    }
                    let mut sroa_done = false;
                    if !packed_done && !view_done && self.sroa_candidates.contains(name) {
                        if let Some(args) = sroa_fields(value) {
                            let mut stores = Vec::with_capacity(args.len());
                            let mut ok = true;
                            for (idx, arg) in args.iter().enumerate() {
                                let k = self.kind_of(arg);
                                if let Some(w) = self.lower_expr(arg) {
                                    stores.push(N::SetLocal {
                                        local: format!("{name}${idx}"),
                                        value: W::ToSlot(Box::new(w), Self::wir_kind(k)),
                                    });
                                } else {
                                    ok = false;
                                    break;
                                }
                            }
                            if ok {
                                let n = stores.len();
                                for s in stores {
                                    seq.push(s);
                                }
                                self.sroa_active.insert(name.clone(), n);
                                sroa_done = true;
                            }
                        }
                    }
                    // (RFC-0062 tier-1) Closure escape elision: `let f = <lambda>` where `f`
                    // is bound once (`devirt_ok`) and never escapes (`closure_elide_called`,
                    // i.e. used ONLY as a direct-call callee this unit). `lower_lambda_threaded`
                    // additionally rejects (→ boxed fallback) any lambda whose captures are
                    // reassigned. On success it registers the threaded lifted body and records
                    // the capture list in `thread_index`, and NOTHING is emitted for the binding
                    // — no `mk{n}` env allocation, no `SetLocal name`. The captures stay in their
                    // existing locals and are threaded to each `call $__lamt{i}` (see the call
                    // arms). Comes before the default `SetLocal` and suppresses it.
                    let mut closure_elide_done = false;
                    if !packed_done
                        && !view_done
                        && !sroa_done
                        && self.collect_wir
                        && self.devirt_ok.contains(name)
                        && self.closure_elide_called.contains(name)
                    {
                        if let Expr::Lambda { params, body: lbody, .. } = value {
                            if let Some(caps) = self.lower_lambda_threaded(params, lbody) {
                                self.thread_index.insert(name.clone(), caps);
                                closure_elide_done = true;
                            }
                        }
                    }
                    if !packed_done && !view_done && !sroa_done && !closure_elide_done {
                        let v = self.lower_expr(value)?;
                        seq.push(N::SetLocal { local: name.clone(), value: v });
                        // An accumulator binding starts with a zero ownership token
                        // (the first push re-owns).
                        if self.collect_wir && self.inplace_push.contains(name) {
                            seq.push(N::SetLocal {
                                local: format!("{name}__cap"),
                                value: W::ConstI32(0),
                            });
                        }
                        // (RFC-0034 L3) Record a devirtualizable closure local: `name`
                        // is bound exactly once and never reassigned (`devirt_ok`), and
                        // `lower_expr` just registered the lambda — so recover its lifted
                        // `$__lamw{i}` index. Later `name(x)` calls then emit a direct
                        // `call` instead of `call_indirect` (see the closure-call arms).
                        if self.collect_wir && self.devirt_ok.contains(name) {
                            if let Expr::Lambda { params, body, .. } = value {
                                let key = Self::lambda_content_key(params, body);
                                if let Some(&idx) = self.lambda_wir_index.get(&key) {
                                    self.devirt_index.insert(name.clone(), idx);
                                }
                            }
                        }
                    }
                    tail_is_value = false;
                }
                Stmt::Expr(e) => {
                    let v = self.lower_expr(e)?;
                    if i == last {
                        seq.push(N::Push(v));
                        tail_is_value = true;
                    } else {
                        seq.push(N::Drop(v));
                        tail_is_value = false;
                    }
                }
                Stmt::Return(opt) => {
                    let value = match opt {
                        Some(e) => {
                            let ek = self.kind_of(e);
                            let w = self.lower_expr(e)?;
                            if self.cur_fn_ret_slot {
                                W::ToSlot(Box::new(w), Self::wir_kind(ek))
                            } else {
                                Self::wir_convert(w, ek, self.cur_fn_ret_kind)
                            }
                        }
                        None if self.cur_fn_ret_slot => W::ConstI64(0),
                        None => match self.cur_fn_ret_kind {
                            Kind::I64 => W::ConstI64(0),
                            Kind::F64 => W::ConstF64(0.0),
                            Kind::I32 => W::ConstI32(0),
                        },
                    };
                    if self.cur_fn_var_params.is_empty() && self.cur_fn_own_param.is_none() {
                        seq.push(N::Return(Some(value)));
                    } else {
                        // An `var`/own-ABI function's early `return` must yield the
                        // full multi-result tuple — the declared value, then each
                        // var param's value, then the own-cap — matching
                        // `assemble_wir_func`'s tail ordering. Push them and use a
                        // bare `return` (WIR `N::Return(Some)` carries one value).
                        seq.push(N::Push(value));
                        for name in &self.cur_fn_var_params {
                            seq.push(N::Push(W::GetLocal(name.clone())));
                        }
                        if let Some(p) = self.cur_fn_own_param.clone() {
                            let returns_own = matches!(opt, Some(Expr::Var(v)) if *v == p)
                                || matches!(opt, Some(Expr::Unary { op: UnOp::Move, expr })
                                    if matches!(expr.as_ref(), Expr::Var(v) if *v == p));
                            let cap = if returns_own && self.inplace_push.contains(&p) {
                                W::GetLocal(format!("{p}__cap"))
                            } else {
                                W::ConstI32(0)
                            };
                            seq.push(N::Push(cap));
                        }
                        seq.push(N::Return(None));
                    }
                    tail_is_value = false;
                }
                // `let PAT = e` (RFC-0052): store the value once as an i64 SLOT in
                // MATCH_TMP, then emit the pattern's BINDINGS via the shared
                // `lower_pattern` — the same machinery `match` uses (a `let`
                // pattern is irrefutable, so its test condition is discarded; only
                // the binds run). `lower_pattern` reads the value as an i64 slot
                // (it `FromSlot`s to a pointer for tuples/ctors), so store via
                // `ToSlot` at the value's kind — exactly as `lower_match` does.
                // Handles nested tuples, ctor/record destructures, and list heads
                // uniformly, superseding the old flat-slot-only loop.
                Stmt::LetPattern { pattern, value } => {
                    let vk = self.kind_of(value);
                    let v = self.lower_expr(value)?;
                    seq.push(N::SetLocal {
                        local: MATCH_TMP.to_string(),
                        value: W::ToSlot(Box::new(v), Self::wir_kind(vk)),
                    });
                    let (_cond, binds) =
                        self.lower_pattern(&W::GetLocal(MATCH_TMP.to_string()), pattern)?;
                    seq.extend(binds);
                    tail_is_value = false;
                }
                // `break`/`continue` -> a `br` to the enclosing loop's exit/continue
                // label. Outside a loop -> `None` (legacy emits the loud error).
                Stmt::Break | Stmt::Continue => {
                    let (brk, cont) = {
                        let (b, c) = self.loop_labels.last()?;
                        (b.clone(), c.clone())
                    };
                    let label = if matches!(stmt, Stmt::Break) { brk } else { cont };
                    let target = label.strip_prefix('$').unwrap_or(&label).to_string();
                    seq.push(N::Br { target, cond: None });
                    tail_is_value = false;
                }
                // `x = value` — only the simplest case: a plain LOCAL reassignment
                // that is NOT a self-assign shape (no in-place fast path / site
                // accounting), a string/list state field, or a global. Those keep
                // their bespoke legacy emission.
                Stmt::Assign { name, value } => {
                    // (RFC-0016) In-place reuse: a confined, never-aliased list `var`
                    // reassigned to a same-length list literal OVERWRITES its existing
                    // buffer slot-by-slot instead of allocating a fresh list — so a
                    // build-and-drop loop stays O(1) heap. The escape oracle proved the
                    // buffer is unaliased; we additionally require the RHS to not read
                    // the var (else a slot could be overwritten before a later element
                    // reads it), allocating normally for that one site otherwise.
                    if self.collect_wir
                        && self.reuse_vars.contains(name)
                        && matches!(value, Expr::List(_) | Expr::Ctor { .. })
                        && !expr_reads_var(value, name)
                    {
                        let slot_addr = |i: usize| W::Binary {
                            op: witchy_wir::wir::BinOp::Add,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(W::GetLocal(name.clone())),
                            rhs: Box::new(W::ConstI32((4 + 8 * i) as i32)),
                        };
                        match value {
                            // A record: fixed tag + arity (guaranteed same ctor), so the
                            // buffer always has exactly the right slots — overwrite the
                            // fields directly, tag header untouched.
                            Expr::Ctor { args, .. } => {
                                for (i, item) in args.iter().enumerate() {
                                    let k = self.kind_of(item);
                                    let w = self.lower_expr(item)?;
                                    seq.push(N::Store {
                                        ptr: slot_addr(i),
                                        value: W::ToSlot(Box::new(w), Self::wir_kind(k)),
                                        kind: witchy_wir::wir::Kind::I64,
                                        offset: 0,
                                    });
                                }
                            }
                            // A list: capacity-resizing reuse. Evaluate the elements into
                            // the reuse temp pool ONCE (so the branch does not double-
                            // evaluate side effects), then either overwrite the existing
                            // buffer when its capacity (current count) fits, or
                            // reallocate — so a build-and-drop loop with varying lengths
                            // still stays bounded (the buffer ratchets to the max length).
                            // A literal too large for the pool just allocates (no reuse).
                            Expr::List(items) if items.len() <= REUSE_POOL => {
                                let len = items.len();
                                for (i, item) in items.iter().enumerate() {
                                    let k = self.kind_of(item);
                                    let w = self.lower_expr(item)?;
                                    seq.push(N::SetLocal {
                                        local: format!("__witchy_reuse_{i}"),
                                        value: W::ToSlot(Box::new(w), Self::wir_kind(k)),
                                    });
                                }
                                let mut then_: witchy_wir::wir::WirSeq = (0..len)
                                    .map(|i| N::Store {
                                        ptr: slot_addr(i),
                                        value: W::GetLocal(format!("__witchy_reuse_{i}")),
                                        kind: witchy_wir::wir::Kind::I64,
                                        offset: 0,
                                    })
                                    .collect();
                                then_.push(N::Store {
                                    ptr: W::GetLocal(name.clone()),
                                    value: W::ConstI32(len as i32),
                                    kind: witchy_wir::wir::Kind::I32,
                                    offset: 0,
                                });
                                self.mk_arities.insert(len);
                                let mut mk_args = Vec::with_capacity(len + 1);
                                mk_args.push(W::ConstI32(len as i32));
                                for i in 0..len {
                                    mk_args.push(W::GetLocal(format!("__witchy_reuse_{i}")));
                                }
                                let els = vec![N::SetLocal {
                                    local: name.clone(),
                                    value: W::Call { func: format!("mk{len}"), args: mk_args },
                                }];
                                // reuse when the current count (header at x+0) >= len.
                                let cond = W::Binary {
                                    op: witchy_wir::wir::BinOp::Le,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(W::ConstI32(len as i32)),
                                    rhs: Box::new(W::Load {
                                        ptr: Box::new(W::GetLocal(name.clone())),
                                        kind: witchy_wir::wir::Kind::I32,
                                        offset: 0,
                                    }),
                                };
                                seq.push(N::If { cond, then_, els, result: None });
                            }
                            // A list literal too large for the temp pool: allocate fresh.
                            Expr::List(items) => {
                                let w = self.lower_aggregate(items.len() as i32, items, 0)?;
                                seq.push(N::SetLocal { local: name.clone(), value: w });
                            }
                            _ => unreachable!(),
                        }
                        tail_is_value = false;
                    } else if self.sroa_active.contains_key(name) {
                        let Expr::RecordUpdate { base, fields } = value else {
                            return None;
                        };
                        let tyname = self.record_type_of(base)?;
                        let rec = self.record_fields.get(&tyname)?.clone();
                        for (fname, fval) in fields {
                            let idx = rec.iter().position(|(n, _)| n == fname)?;
                            let k = self.kind_of(fval);
                            let w = self.lower_expr(fval)?;
                            seq.push(N::SetLocal {
                                local: format!("{name}${idx}"),
                                value: W::ToSlot(Box::new(w), Self::wir_kind(k)),
                            });
                        }
                        tail_is_value = false;
                    } else if let Some((callee, _)) = (self.collect_wir
                        && self.inplace_push.contains(name))
                    .then(|| analysis::self_own_call(name, value, &self.summaries))
                    .flatten()
                    {
                        // own-ABI self-call (binary only): `xs = grow(move xs, …)`.
                        // Gated on `inplace_push` — i.e. the `{name}__cap` token IS
                        // declared (an accumulator). Under force-copy (`-inplace`,
                        // no accumulators) this falls through to a plain reassign
                        // (`name = <plain own-ABI call>`, cap = 0), so the cap local
                        // is never referenced when it doesn't exist.
                        // against a callee whose `own` buffer param may be returned.
                        // The callee returns `(value, cap)` and takes the caller's
                        // ownership token as a trailing i32 arg — so thread `xs__cap`
                        // in and capture (value → xs, cap → xs__cap) via CallStoreMulti.
                        let callee = callee.to_string();
                        let Expr::Call { args, .. } = value else { return None };
                        let param_kinds: Vec<Kind> = self
                            .fn_params
                            .get(&callee)
                            .map(|ps| {
                                ps.iter()
                                    .map(|p| p.ty.as_ref().map(ty_kind).unwrap_or(Kind::I32))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mut args_w = Vec::with_capacity(args.len() + 1);
                        for (i, arg) in args.iter().enumerate() {
                            let ak = self.kind_of(arg);
                            let w = self.lower_expr(arg)?;
                            args_w.push(match param_kinds.get(i) {
                                Some(&pk) => Self::wir_convert(w, ak, pk),
                                None => w,
                            });
                        }
                        // The trailing synthetic arg: our current ownership token.
                        args_w.push(W::GetLocal(format!("{name}__cap")));
                        seq.push(N::CallStoreMulti {
                            func: callee,
                            args: args_w,
                            dests: vec![name.clone(), format!("{name}__cap")],
                        });
                        tail_is_value = false;
                    } else if self.collect_wir
                        && self.inplace_push.contains(name)
                        && analysis::self_inplace_op(name, value).is_some()
                    {
                        let op = analysis::self_inplace_op(name, value).expect("guarded Some above");
                        // A dirty site (its RHS embeds an aliasing share of `name`)
                        // forces a zero ownership token → re-own + copy; a clean site
                        // trusts the runtime token. Read-only here; `sites` consumed
                        // at end. Hoisted across all in-place shapes below.
                        let dirty = match self.facts_stack.last() {
                            Some((facts, _, _)) if facts.accumulators.contains(name) => {
                                facts.is_dirty(stmt)
                            }
                            _ => true,
                        };
                        match op {
                            analysis::InPlaceOp::Push(elem) => {
                                // Only the list-push shape has an in-place fast path. A dict/
                                // string self-assign falls through to the plain value-rebind
                                // below — correct value semantics, just without the O(1)
                                // in-place mutation.
                                let xk = self.kind_of(elem);
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                use witchy_wir::wir::BinOp;
                                let e = self.lower_expr(elem)?;
                                self.uses_list_push_cap = true;
                                // Stash length (i32) + value (i64 slot), then APPEND in place
                                // when owned slack remains (cap > len): write the value at slot
                                // `len` and bump the length, leaving the capacity token alone.
                                // Else fall back to `$list_push_cap` (grow / re-own). Eliding
                                // the helper CALL on the hot path is RFC-0016 R2 static elision.
                                seq.push(N::SetLocal {
                                    local: "__witchy_set_idx".into(),
                                    value: W::Load { ptr: Box::new(W::GetLocal(name.clone())), kind: witchy_wir::wir::Kind::I32, offset: 0 },
                                });
                                seq.push(N::SetLocal {
                                    local: "__witchy_set_val".into(),
                                    value: W::ToSlot(Box::new(e), Self::wir_kind(xk)),
                                });
                                let sl = || W::GetLocal("__witchy_set_idx".to_string());
                                let sv = || W::GetLocal("__witchy_set_val".to_string());
                                let bin = |op, l, r| W::Binary { op, kind: witchy_wir::wir::Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
                                let slot_ptr = bin(
                                    BinOp::Add,
                                    bin(BinOp::Add, W::GetLocal(name.clone()), W::ConstI32(4)),
                                    bin(BinOp::Mul, sl(), W::ConstI32(8)),
                                );
                                seq.push(N::If {
                                    cond: bin(BinOp::Gt, cap.clone(), sl()),
                                    then_: vec![
                                        N::Store { ptr: slot_ptr, value: sv(), kind: witchy_wir::wir::Kind::I64, offset: 0 },
                                        N::Store { ptr: W::GetLocal(name.clone()), value: bin(BinOp::Add, sl(), W::ConstI32(1)), kind: witchy_wir::wir::Kind::I32, offset: 0 },
                                    ],
                                    els: vec![N::CallStoreMulti {
                                        func: "list_push_cap".to_string(),
                                        args: vec![W::GetLocal(name.clone()), sv(), cap],
                                        dests: vec![name.clone(), format!("{name}__cap")],
                                    }],
                                    result: None,
                                });
                            }
                            analysis::InPlaceOp::SetAt(iexpr, vexpr) => {
                                // `xs = list.set_at(xs, i, v)`: in-place element store via
                                // `$list_set_cap` (mutate the owned buffer's slot, O(1)),
                                // mirroring the list-push fast path. Without it the plain
                                // rebind rebuilds the whole list each set — O(n²) memory
                                // that traps a large list under the memory cap. A dirty
                                // site forces a zero token (re-own + copy, preserving any
                                // alias); a clean site mutates the owned buffer.
                                let ik = self.kind_of(iexpr);
                                let vk = self.kind_of(vexpr);
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                use witchy_wir::wir::BinOp;
                                let iw = self.lower_expr(iexpr)?;
                                let vw = self.lower_expr(vexpr)?;
                                // Stash index (i32) + value (i64 slot) into scratch locals,
                                // then store IN PLACE inline when in-bounds and owned, else
                                // fall back to `$list_set_cap` (OOB no-op / re-own-and-copy).
                                // Eliding the helper CALL on the hot path is RFC-0016 R2
                                // static elision; the proven helper still covers the cold path.
                                seq.push(N::SetLocal {
                                    local: "__witchy_set_idx".into(),
                                    value: Self::wir_convert(iw, ik, Kind::I32),
                                });
                                seq.push(N::SetLocal {
                                    local: "__witchy_set_val".into(),
                                    value: W::ToSlot(Box::new(vw), Self::wir_kind(vk)),
                                });
                                let si = || W::GetLocal("__witchy_set_idx".to_string());
                                let sv = || W::GetLocal("__witchy_set_val".to_string());
                                let bin = |op, l, r| W::Binary { op, kind: witchy_wir::wir::Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
                                let len = W::Load { ptr: Box::new(W::GetLocal(name.clone())), kind: witchy_wir::wir::Kind::I32, offset: 0 };
                                let cond = bin(
                                    BinOp::And,
                                    bin(BinOp::And, bin(BinOp::Ge, si(), W::ConstI32(0)), bin(BinOp::Lt, si(), len)),
                                    bin(BinOp::Gt, cap.clone(), W::ConstI32(0)),
                                );
                                let slot_ptr = || bin(
                                    BinOp::Add,
                                    bin(BinOp::Add, W::GetLocal(name.clone()), W::ConstI32(4)),
                                    bin(BinOp::Mul, si(), W::ConstI32(8)),
                                );
                                // (RFC-0035 step 2) Drop the element this store DISPLACES: load the
                                // old i32 slot and `$rc_drop` it before overwriting. Sound because
                                // dup-at-read (step 1) already counted every reader — the count is
                                // >1 exactly when a live binding still holds the element (drop just
                                // decrements) and 1 exactly when the slot was its last holder (drop
                                // frees). Gated: the displaced element is a PROVABLY offset-0 rc
                                // value (`expr_is_offset0_rc(vexpr)` — the slot has the same type;
                                // excludes Dict/scalar/type-var), the list var is confined-unique
                                // (`inplace_push` ⇒ never aliased ⇒ its buffer was never copied, so
                                // elements aren't shared through a container copy), we are at
                                // `wm_level==0` (a drop inside an arena-reset scope would double-
                                // free), and `rc-floor` is on. The `els` re-own+copy cold path is
                                // NOT dropped — a copy shares element pointers without a dup, so a
                                // free there could be a UAF (left as a sound leak).
                                let rc_drop_displaced = Self::wir_kind(vk) == witchy_wir::wir::Kind::I32
                                    && self.inplace_push.contains(name)
                                    && self.expr_is_offset0_rc(vexpr)
                                    && self.wm_level == 0
                                    && !force_copy_mode()
                                    && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::RcFloor);
                                let mut then_body = Vec::new();
                                if rc_drop_displaced {
                                    then_body.push(N::Do(W::Call {
                                        func: "rc_drop".into(),
                                        args: vec![W::FromSlot(
                                            Box::new(W::Load { ptr: Box::new(slot_ptr()), kind: witchy_wir::wir::Kind::I64, offset: 0 }),
                                            witchy_wir::wir::Kind::I32,
                                        )],
                                    }));
                                }
                                then_body.push(N::Store { ptr: slot_ptr(), value: sv(), kind: witchy_wir::wir::Kind::I64, offset: 0 });
                                seq.push(N::If {
                                    cond,
                                    then_: then_body,
                                    els: vec![N::CallStoreMulti {
                                        func: "list_set_cap".to_string(),
                                        args: vec![W::GetLocal(name.clone()), si(), sv(), cap],
                                        dests: vec![name.clone(), format!("{name}__cap")],
                                    }],
                                    result: None,
                                });
                            }
                            analysis::InPlaceOp::UpdateAt(iexpr, fexpr) => {
                                // `xs = list.update_at(xs, i, f)`: in-place element update
                                // via `$list_update_cap` (apply the closure to the owned
                                // slot, O(1)), mirroring the set_at fast path. Without it the
                                // plain rebind copies the whole list each update — O(n²)
                                // memory. A dirty site forces a zero token (re-own + copy,
                                // preserving any alias); a clean site mutates in place.
                                let ik = self.kind_of(iexpr);
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                let iw = self.lower_expr(iexpr)?;
                                let fw = self.lower_expr(fexpr)?;
                                seq.push(N::CallStoreMulti {
                                    func: "list_update_cap".to_string(),
                                    args: vec![
                                        W::GetLocal(name.clone()),
                                        Self::wir_convert(iw, ik, Kind::I32),
                                        fw,
                                        cap,
                                    ],
                                    dests: vec![name.clone(), format!("{name}__cap")],
                                });
                            }
                            analysis::InPlaceOp::Insert(kexpr, vexpr) => {
                                // `d = dict.insert(d, k, v)`: the in-place dict upsert via
                                // `$dict_insert_cap` (O(1) amortized into owned entry slack),
                                // mirroring the list-push fast path. Without it the plain
                                // rebind below copies the whole dict each insert — O(n²)
                                // memory that traps a large dict under a tight memory cap.
                                let mode = self.dict_key_mode_wir(kexpr)?;
                                let kk = self.kind_of(kexpr);
                                let vk = self.kind_of(vexpr);
                                if let Some(kvt) = self.dict_key_valtype_of(value) {
                                    self.local_dict_key_valtype.insert(name.clone(), kvt);
                                }
                                if let Some(vvt) = self.dict_value_valtype_of(value) {
                                    self.local_dict_value_valtype.insert(name.clone(), vvt);
                                }
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                let kw = self.lower_expr(kexpr)?;
                                let vw = self.lower_expr(vexpr)?;
                                self.uses_dict_insert_cap = true;
                                seq.push(N::CallStoreMulti {
                                    func: "dict_insert_cap".to_string(),
                                    args: vec![
                                        W::GetLocal(name.clone()),
                                        W::ToSlot(Box::new(kw), Self::wir_kind(kk)),
                                        W::ToSlot(Box::new(vw), Self::wir_kind(vk)),
                                        W::ConstI32(mode as i32),
                                        cap,
                                    ],
                                    dests: vec![name.clone(), format!("{name}__cap")],
                                });
                            }
                            analysis::InPlaceOp::Update(kexpr, dexpr, fexpr) => {
                                // `d = dict.update(d, k, dflt, f)`: the in-place upsert via
                                // `$dict_update_cap` (apply the closure, reinsert into owned
                                // slack), mirroring the dict-insert fast path. Without it the
                                // plain rebind copies the whole dict each update.
                                let mode = self.dict_key_mode_wir(kexpr)?;
                                let kk = self.kind_of(kexpr);
                                let dk = self.kind_of(dexpr);
                                if let Some(kvt) = self.dict_key_valtype_of(value) {
                                    self.local_dict_key_valtype.insert(name.clone(), kvt);
                                }
                                if let Some(vvt) = self.dict_value_valtype_of(value) {
                                    self.local_dict_value_valtype.insert(name.clone(), vvt);
                                }
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                let kw = self.lower_expr(kexpr)?;
                                let dw = self.lower_expr(dexpr)?;
                                let fw = self.lower_expr(fexpr)?;
                                self.clos_arities.insert(1);
                                self.uses_dict_update_cap = true;
                                seq.push(N::CallStoreMulti {
                                    func: "dict_update_cap".to_string(),
                                    args: vec![
                                        W::GetLocal(name.clone()),
                                        W::ToSlot(Box::new(kw), Self::wir_kind(kk)),
                                        W::ToSlot(Box::new(dw), Self::wir_kind(dk)),
                                        W::ConstI32(mode as i32),
                                        fw,
                                        cap,
                                    ],
                                    dests: vec![name.clone(), format!("{name}__cap")],
                                });
                            }
                            analysis::InPlaceOp::Concat(pieces) => {
                                // `s = s + a + b`: the in-place string builder via
                                // `$str_append_cap` (append each piece into owned byte
                                // slack), mirroring the list/dict fast paths. Without it the
                                // plain rebind re-concatenates the whole string each
                                // statement — O(n²) bytes for a growing buffer. A dirty
                                // first piece forces a zero token (re-own → grow-and-copy,
                                // preserving any alias); later pieces reuse the fresh slack.
                                self.uses_str_append_cap = true;
                                for (i, piece) in pieces.into_iter().enumerate() {
                                    let pw = self.lower_expr(piece)?;
                                    let cap = if i == 0 && dirty {
                                        W::ConstI32(0)
                                    } else {
                                        W::GetLocal(format!("{name}__cap"))
                                    };
                                    seq.push(N::CallStoreMulti {
                                        func: "str_append_cap".to_string(),
                                        args: vec![W::GetLocal(name.clone()), pw, cap],
                                        dests: vec![name.clone(), format!("{name}__cap")],
                                    });
                                }
                            }
                            analysis::InPlaceOp::RecordUpdate(fields) => {
                                // (RFC-0033 R1) `s = {...s, f: v, …}`: when `s` is the
                                // uniquely-owned record, write each updated field into s's
                                // existing slots (`s+4+8*idx`) and keep the pointer — O(updated
                                // fields), no alloc, no copy of the un-updated fields. A dirty /
                                // un-owned site re-owns via the `mk{n}` realloc (the existing
                                // copy path), yielding a fresh owned record + a live token.
                                // Records are fixed-shape, so the token is a 0/1 owned flag.
                                // Field values are stashed into the reuse pool BEFORE any store
                                // so a value reading another field sees the pre-update record.
                                use witchy_wir::wir::BinOp;
                                let tyname = self.local_records.get(name).cloned()?;
                                let rnames = self.record_fields.get(&tyname).cloned()?;
                                let &(tag, nfields) = self.ctors.get(&tyname)?;
                                self.mk_arities.insert(nfields);
                                let cap = if dirty {
                                    W::ConstI32(0)
                                } else {
                                    W::GetLocal(format!("{name}__cap"))
                                };
                                let slot = |idx: usize| W::Binary {
                                    op: BinOp::Add,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(W::GetLocal(name.clone())),
                                    rhs: Box::new(W::ConstI32((4 + 8 * idx) as i32)),
                                };
                                let load = |idx: usize| W::Load {
                                    ptr: Box::new(slot(idx)),
                                    kind: witchy_wir::wir::Kind::I64,
                                    offset: 0,
                                };
                                if fields.len() <= REUSE_POOL {
                                    // Stash each updated value into a reuse slot, recording
                                    // (record-field index -> reuse slot).
                                    let mut updated: Vec<(usize, usize)> = Vec::with_capacity(fields.len());
                                    for (j, (fname, vexpr)) in fields.iter().enumerate() {
                                        let idx = rnames.iter().position(|(n, _)| n == fname)?;
                                        // (RFC-0033 R2) `s.field = list.push(s.field, e)`: grow the
                                        // field's list buffer in place instead of copying it each
                                        // update. Two soundness guards:
                                        //   * whole-record aliasing — `eff = field_cap * (record
                                        //     owned)` is 0 unless this record is uniquely owned at
                                        //     the site (R1's `cap`), so an aliased record copies;
                                        //   * field aliasing — `field_push_safe` only holds when
                                        //     `s.field` is read nowhere but this push receiver, so
                                        //     the buffer is never separately aliased.
                                        // Either guard failing falls back to the copying push below.
                                        let push_field = if self
                                            .field_push_safe
                                            .contains(&(name.clone(), (*fname).clone()))
                                        {
                                            match vexpr {
                                                Expr::Call { name: pn, args: pa }
                                                    if pn == "list.push" && pa.len() == 2 =>
                                                {
                                                    match &pa[0] {
                                                        Expr::Field { base, field }
                                                            if field == fname
                                                                && matches!(base.as_ref(), Expr::Var(v) if v == name) =>
                                                        {
                                                            Some(&pa[1])
                                                        }
                                                        _ => None,
                                                    }
                                                }
                                                _ => None,
                                            }
                                        } else {
                                            None
                                        };
                                        if let Some(elem) = push_field {
                                            use witchy_wir::wir::BinOp as B;
                                            let fcap = format!("{name}${fname}__cap");
                                            self.field_caps.insert(fcap.clone());
                                            self.uses_list_push_cap = true;
                                            let ek = self.kind_of(elem);
                                            let ew = self.lower_expr(elem)?;
                                            // The field slot holds an i32 list pointer widened into
                                            // the i64 universal slot; truncate it back to the ptr.
                                            let cur =
                                                Self::wir_convert(load(idx), Kind::I64, Kind::I32);
                                            // eff = field_cap * (record owned): 0 unless the record
                                            // is uniquely owned at this site, forcing a field copy.
                                            let eff = W::Binary {
                                                op: B::Mul,
                                                kind: witchy_wir::wir::Kind::I32,
                                                lhs: Box::new(W::GetLocal(fcap.clone())),
                                                rhs: Box::new(W::Binary {
                                                    op: B::Gt,
                                                    kind: witchy_wir::wir::Kind::I32,
                                                    lhs: Box::new(cap.clone()),
                                                    rhs: Box::new(W::ConstI32(0)),
                                                }),
                                            };
                                            seq.push(N::CallStoreMulti {
                                                func: "list_push_cap".to_string(),
                                                args: vec![
                                                    cur,
                                                    W::ToSlot(Box::new(ew), Self::wir_kind(ek)),
                                                    eff,
                                                ],
                                                dests: vec![TUPLE_TMP.to_string(), fcap],
                                            });
                                            seq.push(N::SetLocal {
                                                local: format!("__witchy_reuse_{j}"),
                                                value: W::ToSlot(
                                                    Box::new(W::GetLocal(TUPLE_TMP.to_string())),
                                                    Self::wir_kind(Kind::I32),
                                                ),
                                            });
                                            updated.push((idx, j));
                                            continue;
                                        }
                                        let vk = self.kind_of(vexpr);
                                        let vw = self.lower_expr(vexpr)?;
                                        seq.push(N::SetLocal {
                                            local: format!("__witchy_reuse_{j}"),
                                            value: W::ToSlot(Box::new(vw), Self::wir_kind(vk)),
                                        });
                                        updated.push((idx, j));
                                    }
                                    // Cold path: a fresh record (updated fields from the reuse
                                    // slots, the rest copied from the old `s`).
                                    let mut mk_args = Vec::with_capacity(nfields + 1);
                                    mk_args.push(W::ConstI32(tag as i32));
                                    for i in 0..nfields {
                                        mk_args.push(match updated.iter().find(|(idx, _)| *idx == i) {
                                            Some((_, j)) => W::GetLocal(format!("__witchy_reuse_{j}")),
                                            None => load(i),
                                        });
                                    }
                                    // Hot path: store each updated value into s's slot in place.
                                    let hot: Vec<N> = updated
                                        .iter()
                                        .map(|(idx, j)| N::Store {
                                            ptr: slot(*idx),
                                            value: W::GetLocal(format!("__witchy_reuse_{j}")),
                                            kind: witchy_wir::wir::Kind::I64,
                                            offset: 0,
                                        })
                                        .collect();
                                    seq.push(N::If {
                                        cond: W::Binary {
                                            op: BinOp::Gt,
                                            kind: witchy_wir::wir::Kind::I32,
                                            lhs: Box::new(cap),
                                            rhs: Box::new(W::ConstI32(0)),
                                        },
                                        then_: hot,
                                        els: vec![
                                            N::SetLocal {
                                                local: name.clone(),
                                                value: W::Call { func: format!("mk{nfields}"), args: mk_args },
                                            },
                                            N::SetLocal { local: format!("{name}__cap"), value: W::ConstI32(1) },
                                        ],
                                        result: None,
                                    });
                                } else {
                                    // >REUSE_POOL updated fields (rare): always realloc + re-own.
                                    let mut upd: Vec<(usize, W)> = Vec::with_capacity(fields.len());
                                    for (fname, vexpr) in fields.iter() {
                                        let idx = rnames.iter().position(|(n, _)| n == fname)?;
                                        let vk = self.kind_of(vexpr);
                                        let vw = self.lower_expr(vexpr)?;
                                        upd.push((idx, W::ToSlot(Box::new(vw), Self::wir_kind(vk))));
                                    }
                                    let mut mk_args = Vec::with_capacity(nfields + 1);
                                    mk_args.push(W::ConstI32(tag as i32));
                                    for i in 0..nfields {
                                        mk_args.push(match upd.iter().position(|(idx, _)| *idx == i) {
                                            Some(p) => upd[p].1.clone(),
                                            None => load(i),
                                        });
                                    }
                                    seq.push(N::SetLocal {
                                        local: name.clone(),
                                        value: W::Call { func: format!("mk{nfields}"), args: mk_args },
                                    });
                                    seq.push(N::SetLocal { local: format!("{name}__cap"), value: W::ConstI32(1) });
                                }
                            }
                        }
                        inplace_sites += 1;
                        tail_is_value = false;
                    } else if self.collect_wir
                        && self.rc_floor_vars.contains(name)
                        && matches!(value, Expr::Call { name: f, args }
                            if matches!(args.first(), Some(Expr::Var(x)) if x == name)
                                && analysis::fresh_heap_builtin_offset(f, args.len()).is_some())
                    {
                        // (RFC-0016) RC-floor free-at-overwrite: `name` is a confined,
                        // never-aliased heap var overwritten by a builtin that allocates
                        // a FRESH buffer while threading the old one through as its
                        // receiver (`d = dict.remove(d, k)`) — a shape the in-place fast
                        // paths above did not claim, so it would otherwise leak the old
                        // buffer every iteration (the cache-eviction leak). The old
                        // buffer is now dead: evaluate the fresh result (which still
                        // reads the old via the receiver), and when it is a genuinely new
                        // allocation (the pointer differs — the guard also makes a callee
                        // that returned its own buffer a safe no-op) free the old region
                        // into the size-classed free-list before rebinding.
                        let Expr::Call { name: f, args } = value else { unreachable!() };
                        let offset = analysis::fresh_heap_builtin_offset(f, args.len())
                            .expect("guarded Some above");
                        let vk = self.kind_of(value);
                        let target = self.locals.get(name).copied().unwrap_or(Kind::I32);
                        let v = self.lower_expr(value)?;
                        seq.push(N::SetLocal {
                            local: "__rc_new".into(),
                            value: Self::wir_convert(v, vk, target),
                        });
                        // The start of the old buffer's `$rc_alloc` region (dicts sit 4
                        // bytes past it, for the hidden index word).
                        let old_region = if offset == 0 {
                            W::GetLocal(name.clone())
                        } else {
                            W::Binary {
                                op: witchy_wir::wir::BinOp::Sub,
                                kind: witchy_wir::wir::Kind::I32,
                                lhs: Box::new(W::GetLocal(name.clone())),
                                rhs: Box::new(W::ConstI32(offset)),
                            }
                        };
                        let cond = W::Binary {
                            op: witchy_wir::wir::BinOp::Ne,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(W::GetLocal("__rc_new".into())),
                            rhs: Box::new(W::GetLocal(name.clone())),
                        };
                        seq.push(N::If {
                            cond,
                            then_: vec![N::Do(W::Call {
                                func: "rc_free".into(),
                                args: vec![old_region],
                            })],
                            els: vec![],
                            result: None,
                        });
                        seq.push(N::SetLocal {
                            local: name.clone(),
                            value: W::GetLocal("__rc_new".into()),
                        });
                        tail_is_value = false;
                    } else {
                        // A plain local reassignment, INCLUDING a self-assign
                        // accumulator (`s = s + x`, `xs = list.push(xs, e)`) that the
                        // in-place fast path above didn't claim: lower it as a
                        // fresh-value rebind. That is exactly the interpreter's value
                        // semantics — the RHS allocates a new value and rebinds the
                        // local; the in-place mutation is only an optimization the
                        // uniqueness/wir_opt pass layers on later. If the RHS itself
                        // can't lower yet, the `?` defers the whole function as before.
                        let vk = self.kind_of(value);
                        let target = self.locals.get(name).copied().unwrap_or(Kind::I32);
                        let v = self.lower_expr(value)?;
                        seq.push(N::SetLocal {
                            local: name.clone(),
                            value: Self::wir_convert(v, vk, target),
                        });
                        tail_is_value = false;
                    }
                }
                // Yield → legacy (rewritten away before codegen anyway).
                _ => return None,
            }
            // Reset the cap of any inplace_push var killed AFTER this statement
            // (binary path), positioned here in the seq. Read-only — the kills
            // counter is consumed once by the `take_kills` loop below.
            if self.collect_wir && !self.inplace_push.is_empty() {
                let killed: Vec<String> = self
                    .facts_stack
                    .last()
                    .map(|(f, _, _)| f.kills_after(stmt).to_vec())
                    .unwrap_or_default();
                for v in &killed {
                    if self.inplace_push.contains(v) {
                        seq.push(N::SetLocal {
                            local: format!("{v}__cap"),
                            value: W::ConstI32(0),
                        });
                    }
                }
            }
            // (RFC-0035) `$rc_free` every value proven dead after this statement:
            // bound to a known heap allocator, read at most once here, never aliased
            // / escaped / returned / reassigned / region-confined (the `last_use`
            // analysis discharged all of that). The value's last use is in the WIR
            // just pushed to `seq`, so its region is now unreachable and free to
            // reclaim — a straight `$rc_free`, no runtime refcount needed (the value
            // is statically unique-and-dead). `drop_facts_stack` is empty unless
            // `rc-floor` is on, so this is a no-op on the reference path. Read-only
            // over the facts, mirroring the kills reset above.
            //
            // SOUNDNESS — the heap-reset boundary. `wm_level > 0` means we are lowering
            // inside an active heap-reset scope: a watermarked loop body (RFC-0030, the
            // per-iteration `$heap`-rewind) or a `region:`/reclaim block. That reset ALSO
            // reclaims this value's buffer, so an `$rc_free` here would be a DOUBLE
            // reclaim — the freed block lands on the free-list, the watermark then rewinds
            // `$heap` below it, and the next bump re-hands-out the same address that is
            // still linked in the free-list → the free-list `next` chain dangles into
            // live data (an out-of-bounds pointer). So rc-floor cedes reclamation to the
            // enclosing reset and only fires where `wm_level == 0` — straight-line code
            // and loops whose body is NOT arena-resettable (something escapes the
            // iteration, the watermark is off), which is exactly rc-floor's niche.
            if self.collect_wir && self.wm_level == 0 {
                let drops: Vec<analysis::Drop> = self
                    .drop_facts_stack
                    .last()
                    .map(|d| d.drops_after(stmt).to_vec())
                    .unwrap_or_default();
                for d in &drops {
                    let region = if d.offset == 0 {
                        W::GetLocal(d.name.clone())
                    } else {
                        W::Binary {
                            op: witchy_wir::wir::BinOp::Sub,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(W::GetLocal(d.name.clone())),
                            rhs: Box::new(W::ConstI32(d.offset)),
                        }
                    };
                    seq.push(N::Do(W::Call {
                        func: "rc_free".into(),
                        args: vec![region],
                    }));
                }
                // (RFC-0035 step 3) `$rc_drop` read-owned heap bindings at their last use.
                // The element was `$rc_dup`'d at the read (step 1), so this releases that
                // reference — freeing at count 0 (the slot's set_at `$rc_drop` or another
                // holder took the rest) and merely decrementing otherwise (a live holder
                // keeps it). Only bindings we recorded as ACTUALLY dup'd (`rc_owned_bindings`
                // — the same per-type gate as the dup, so drop-iff-dup'd holds by
                // construction), all at rc-region offset 0 (Dict elements are excluded there).
                let read_drops: Vec<String> = self
                    .drop_facts_stack
                    .last()
                    .map(|d| d.read_drops_after(stmt).to_vec())
                    .unwrap_or_default();
                for name in &read_drops {
                    if self.rc_owned_bindings.contains(name)
                        && matches!(self.locals.get(name), Some(&Kind::I32))
                    {
                        seq.push(N::Do(W::Call {
                            func: "rc_drop".into(),
                            args: vec![W::GetLocal(name.clone())],
                        }));
                    }
                }
            }
        }
        // The block always leaves one value: the tail expression, or `i32.const 0`.
        if !tail_is_value {
            seq.push(N::Push(W::ConstI32(0)));
        }
        // Facts consumption. In a WIR-collecting scope `lower_block` is invoked many
        // times per compile (`kind_of` probes, nested re-lowering), so consuming
        // here would over-count — instead `compile_function` consumes ONCE on a
        // successful capture. This `!collect_wir` branch is the non-collecting
        // fallback, where this block is the authoritative consumer. The cap-reset
        // nodes are already positioned in `seq` above (read-only `kills_after`).
        if !self.collect_wir {
            for stmt in &block.stmts {
                let _ = self.take_kills(stmt);
            }
            if inplace_sites > 0 {
                if let Some((_, _, sites)) = self.facts_stack.last_mut() {
                    *sites += inplace_sites;
                }
            }
        }
        Some(seq)
    }

    /// Map codegen's `Kind` to the WIR `Kind` (the same three cases).
    fn wir_kind(k: Kind) -> witchy_wir::wir::Kind {
        match k {
            Kind::I32 => witchy_wir::wir::Kind::I32,
            Kind::I64 => witchy_wir::wir::Kind::I64,
            Kind::F64 => witchy_wir::wir::Kind::F64,
        }
    }

    /// A `WirTy` whose `.kind()` is `k` — used for a control node's `result`
    /// block-type, where only the wasm kind matters (`i64`/`f64`/`i32`).
    fn wir_ty_for_kind(k: Kind) -> witchy_wir::wir::WirTy {
        match k {
            Kind::I64 => witchy_wir::wir::WirTy::Int,
            Kind::F64 => witchy_wir::wir::WirTy::Float,
            Kind::I32 => witchy_wir::wir::WirTy::Bool,
        }
    }

    /// Lower an aggregate literal (list/tuple/constructor) to the shared
    /// `$mkN` allocator call: push the i32 `header` (length, `0`, or ctor tag),
    /// then each element in the universal i64 slot, then `call $mkN`. `None` if any
    /// element isn't lowerable.
    fn lower_aggregate(&mut self, header: i32, items: &[Expr], type_tag: u8) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        let n = items.len();
        self.mk_arities.insert(n);
        let mut args = Vec::with_capacity(n + 1);
        // (RFC-0037 §3) Under the type sanitizer, ride the 8-bit type tag in the header's high
        // byte; `mk` masks it off the offset-0 word and stamps it into the alloc header (p-4).
        let header = if type_tag != 0 && witchy_wir::wir_helpers::type_check_enabled() {
            header | ((type_tag as i32) << 24)
        } else {
            header
        };
        args.push(W::ConstI32(header));
        for item in items {
            let k = self.kind_of(item);
            let w = self.lower_expr(item)?;
            args.push(W::ToSlot(Box::new(w), Self::wir_kind(k)));
        }
        Some(W::Call { func: format!("mk{n}"), args })
    }

    /// Whether a record type is PACKABLE: non-empty and every field is a fixed-size
    /// scalar (`Int`/`Float`/`Bool`/`Duration`). A pointer field (`String`, `List`,
    /// a nested record, a sum type) makes the element variable-size / indirected, so
    /// it cannot be stored inline in a flat buffer (RFC-0027 packed layouts).
    fn is_packable_record(&self, name: &str) -> bool {
        match self.record_fields.get(name) {
            Some(fields) if !fields.is_empty() => fields.iter().all(|(_, ty)| {
                matches!(
                    ty.as_deref(),
                    Some("Int") | Some("Float") | Some("Bool") | Some("Duration")
                )
            }),
            _ => false,
        }
    }

    /// A list literal that can be PACKED: every element is a constructor of the SAME
    /// packable record type, each with the full field arity. Returns the record type
    /// name and the elements' fields flattened in row-major order (element 0's
    /// fields, then element 1's, …) — the flat buffer body. `None` otherwise.
    fn packable_record_list(&self, items: &[Expr]) -> Option<(String, Vec<Expr>)> {
        let mut rec: Option<&str> = None;
        let mut flat: Vec<Expr> = Vec::new();
        for it in items {
            let Expr::Ctor { name, args } = it else { return None };
            match rec {
                None => rec = Some(name),
                Some(r) if r == name => {}
                _ => return None, // mixed record types — no single flat layout
            }
            flat.extend(args.iter().cloned());
        }
        let rec = rec?;
        if !self.is_packable_record(rec) {
            return None;
        }
        let nfields = self.record_fields.get(rec)?.len();
        if items.iter().any(|it| matches!(it, Expr::Ctor { args, .. } if args.len() != nfields)) {
            return None; // a partial ctor would break the fixed stride
        }
        Some((rec.to_string(), flat))
    }

    /// Lower a SCALAR pattern test against `value` (the matched value as an i64
    /// slot — `local.get $MATCH_TMP`). Returns `(cond, binds)`: an i32 condition
    /// expression and the binding nodes. `None` for non-scalar patterns
    /// (tuple/list/ctor/string/…), which keep their bespoke legacy emission.
    fn lower_pattern(
        &mut self,
        value: &witchy_wir::wir::WirExpr,
        pat: &Pattern,
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
                    let (sc, sb) = self.lower_pattern(&elem_value, sub)?;
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
                    let (sc, sb) = self.lower_pattern(&elem_value, sub)?;
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
                    let (sc, sb) = self.lower_pattern(&field_value, sub)?;
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
                    let (c, b) = self.lower_pattern(value, alt)?;
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

    /// Lower a `match` to WIR — only when EVERY arm has a scalar pattern (and its
    /// guard/body lower). Store the scrutinee in `$MATCH_TMP`, then an outer
    /// value-`block $d` holding per-arm `block $a` (test → `br_if` skip; binds;
    /// guard; body+convert; `br $d`), then `unreachable`. `next_label` is restored
    /// on a bail.
    fn lower_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        let scrut_kind = self.kind_of(scrutinee);
        let result_kind = arms.iter().fold(Kind::I32, |acc, a| {
            let k = self.kind_of(&a.body);
            if acc == Kind::F64 || k == Kind::F64 {
                Kind::F64
            } else if acc == Kind::I64 || k == Kind::I64 {
                Kind::I64
            } else {
                Kind::I32
            }
        });
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
            && depth < SCRUT_POOL
            && self.wm_level == 0
            && !force_copy_mode()
            && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::RcFloor)
            && match scrutinee {
                Expr::Call { name, args } if name == "list.at" && args.len() == 2 => {
                    self.list_elem_is_offset0_rc(&args[0])
                }
                _ => false,
            };
        let scrut_w = self.lower_expr(scrutinee)?;
        let id = self.next_label;
        self.next_label += 1;
        // Increment AFTER `scrut_w` (whose `?` could bail without a decrement); the arm-lowering
        // bails below restore `depth`, and the success paths decrement — so it stays balanced.
        if drop_scrut {
            self.match_scrut_depth += 1;
        }
        let value = W::GetLocal(MATCH_TMP.to_string());
        let not = |c: W| W::Unary {
            op: witchy_wir::wir::UnOp::Not,
            kind: witchy_wir::wir::Kind::I32,
            arg: Box::new(c),
        };
        let mut arm_blocks: witchy_wir::wir::WirSeq = Vec::with_capacity(arms.len() + 1);
        for (i, arm) in arms.iter().enumerate() {
            let a_label = format!("a{id}_{i}");
            let (cond, binds) = match self.lower_pattern(&value, &arm.pattern) {
                Some(cb) => cb,
                None => {
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

    /// Build the WIR for a plain direct user-function call: each argument lowered
    /// and widened to its parameter's kind, then `call $name`. Returns `None` if
    /// any argument isn't lowerable. ONLY sound from `lower_expr`'s call arm, after
    /// builtins/natives/closures have been excluded, and only for functions WITHOUT
    /// an own-ABI token or `var` writeback.
    fn try_lower_user_call(&mut self, name: &str, args: &[Expr]) -> Option<witchy_wir::wir::WirExpr> {
        let param_kinds: Vec<Kind> = self
            .fn_params
            .get(name)
            .map(|ps| ps.iter().map(|p| p.ty.as_ref().map(ty_kind).unwrap_or(Kind::I32)).collect())
            .unwrap_or_default();
        let mut args_w = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let ak = self.kind_of(arg);
            let w = self.lower_expr(arg)?;
            args_w.push(match param_kinds.get(i) {
                Some(&pk) => Self::wir_convert(w, ak, pk),
                None => w,
            });
        }
        if self.summaries.own_abi(name).is_some() {
            // (RFC-0033 R3) The callee carries the own-ABI: a trailing i32 cap
            // PARAM and an extra i32 cap RESULT. A PLAIN call (not the
            // `x = f(move x)` self-call that `self_own_call` threads) doesn't carry
            // ownership, so pass cap = 0 — the callee re-owns/copies as needed —
            // and discard the returned cap, yielding the declared value via
            // TUPLE_TMP. Without this, a plain call to any own-ABI function bailed.
            use witchy_wir::wir::{WirExpr as W, WirNode as N};
            args_w.push(W::ConstI32(0));
            return Some(W::Seq(vec![
                N::CallStoreMulti {
                    func: name.to_string(),
                    args: args_w,
                    dests: vec![TUPLE_TMP.to_string(), "__witchy_owncap".to_string()],
                },
                N::Push(W::GetLocal(TUPLE_TMP.to_string())),
            ]));
        }
        Some(witchy_wir::wir::WirExpr::Call { func: name.to_string(), args: args_w })
    }

    /// Lower an `var` user call. The callee returns `(declared, var_1, …)`;
    /// `CallStoreMulti` pops the results in reverse into `dests`, so dest[0] is a
    /// scratch holding the declared value and the rest are the caller's var-arg
    /// locals (written back). We then push the scratch — the call's value. Each
    /// var arg must be a non-global local `Var` (CallStoreMulti uses `local.set`);
    /// otherwise we defer to WAT (`None`).
    fn lower_var_call(&mut self, name: &str, args: &[Expr]) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        let convs = self.fn_conventions.get(name).cloned()?;
        let param_kinds: Vec<Kind> = self
            .fn_params
            .get(name)
            .map(|ps| ps.iter().map(|p| p.ty.as_ref().map(ty_kind).unwrap_or(Kind::I32)).collect())
            .unwrap_or_default();
        let mut args_w = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let ak = self.kind_of(arg);
            let w = self.lower_expr(arg)?;
            args_w.push(match param_kinds.get(i) {
                Some(&pk) => Self::wir_convert(w, ak, pk),
                None => w,
            });
        }
        // dest[0] = scratch for the declared return; then each var arg's local.
        let mut dests = vec![TUPLE_TMP.to_string()];
        for (i, conv) in convs.iter().enumerate() {
            if *conv == Convention::Var {
                match args.get(i) {
                    Some(Expr::Var(v)) if self.locals.contains_key(v) =>
                    {
                        dests.push(v.clone());
                    }
                    _ => return None,
                }
            }
        }
        Some(W::Seq(vec![
            N::CallStoreMulti { func: name.to_string(), args: args_w, dests },
            N::Push(W::GetLocal(TUPLE_TMP.to_string())),
        ]))
    }

    /// Convert the value a lowered block leaves on the stack: a block's tail is
    /// always a `Push`, so wrap its value in a `Convert` (a no-op when the kinds
    /// match). Used when a branch block's kind must be promoted to a common kind
    /// shared with its sibling branches.
    fn convert_block_tail(
        mut seq: witchy_wir::wir::WirSeq,
        from: Kind,
        to: Kind,
    ) -> witchy_wir::wir::WirSeq {
        if from != to {
            if let Some(witchy_wir::wir::WirNode::Push(v)) = seq.pop() {
                seq.push(witchy_wir::wir::WirNode::Push(Self::wir_convert(v, from, to)));
            }
        }
        seq
    }

    /// The WIR analogue of `kind_convert`: wrap `arg` in a `Convert` node when the
    /// kinds differ (else return it unchanged).
    fn wir_convert(arg: witchy_wir::wir::WirExpr, from: Kind, to: Kind) -> witchy_wir::wir::WirExpr {
        if from == to {
            arg
        } else {
            witchy_wir::wir::WirExpr::Convert {
                from: Self::wir_kind(from),
                to: Self::wir_kind(to),
                arg: Box::new(arg),
            }
        }
    }

    /// Is `name` a plain function/body local — compiled to a bare `local.get`,
    /// not a top-level function used as a value? `lower_expr`'s `Expr::Var` arm
    /// lowers to `GetLocal` only for names that satisfy this exact predicate.
    fn is_plain_local_var(&self, name: &str) -> bool {
        self.locals.contains_key(name)
    }

    /// Does `e` have a compound (list/tuple/record) equality shape? Such operands
    /// compare structurally (a helper), not by the bare `i32.eq` the numeric path
    /// would emit — so `lower_expr` declines to lower them here.
    fn operand_is_compound(&self, e: &Expr) -> bool {
        self.eq_shape_of(e).is_some_and(|s| s.is_compound())
    }

    /// The generic-reference compare we reject loudly: in a type-variable function,
    /// two `Other`/i32 operands would compare references, which witchy has no notion
    /// of. `lower_expr` consults this to refuse such an equality.
    fn is_generic_ref_compare(&self, lhs: &Expr, rhs: &Expr) -> bool {
        self.cur_fn_has_type_vars
            && self.val_type_of(lhs) == ValType::Other
            && self.val_type_of(rhs) == ValType::Other
            && self.kind_of(lhs) == Kind::I32
            && self.kind_of(rhs) == Kind::I32
    }

    /// Build a `WirExpr` for the lowerable subset of expressions, returning `None`
    /// for any arm — or sub-expression — not yet lowered. A `None` propagates up and
    /// the program is rejected as reaching an unsupported construct; the supported
    /// set is the authoritative codegen for those expression shapes.
    fn lower_expr(&mut self, e: &Expr) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        use witchy_wir::wir::WirNode as N;
        Some(match e {
            // Expanded away by `crate::tagged` during linking, before codegen.
            Expr::TaggedLit { tag, .. } => {
                unreachable!("unexpanded tagged literal `{tag}` reached codegen")
            }
            Expr::Int(n) | Expr::Duration(n) => W::ConstI64(*n),
            Expr::Float(x) => W::ConstF64(*x),
            Expr::Bool(b) => W::ConstI32(if *b { 1 } else { 0 }),
            Expr::Str(s) => W::StrPtr(self.intern(s)),
            Expr::Var(name) if self.is_plain_local_var(name) => W::GetLocal(name.clone()),
            // A bare top-level function name used as a VALUE (`list.filter(xs,
            // is_odd)`): materialize it as a forwarding closure `fn(p..): name(p..)`,
            // reusing the lambda machinery. Only fires for a known function that
            // isn't shadowed by a local; a shadowing local is handled elsewhere.
            Expr::Var(name)
                if self.collect_wir
                    && !self.locals.contains_key(name)
                    && self.fn_params.contains_key(name) =>
            {
                let params = self.fn_params.get(name).cloned()?;
                let args = params.iter().map(|p| Expr::Var(p.name.clone())).collect();
                let body = Block {
                    stmts: vec![Stmt::Expr(Expr::Call { name: name.clone(), args })],
                    lines: vec![0],
                    region: None,
                };
                return self.lower_lambda(&params, &body);
            }
            Expr::Unary { op, expr } => match op {
                // value-neutral on WASM (value semantics): lower the operand.
                UnOp::Move | UnOp::Await => return self.lower_expr(expr),
                UnOp::Not => W::Unary {
                    op: witchy_wir::wir::UnOp::Not,
                    kind: witchy_wir::wir::Kind::I32,
                    arg: Box::new(self.lower_expr(expr)?),
                },
                UnOp::Neg => {
                    let kind = Self::wir_kind(self.kind_of(expr));
                    W::Unary { op: witchy_wir::wir::UnOp::Neg, kind, arg: Box::new(self.lower_expr(expr)?) }
                }
                UnOp::BitNot => {
                    let kind = Self::wir_kind(self.kind_of(expr));
                    W::Unary {
                        op: witchy_wir::wir::UnOp::BitNot,
                        kind,
                        arg: Box::new(self.lower_expr(expr)?),
                    }
                }
            },
            // `e as T` (capability narrowing / type ascription) is value-neutral
            // at codegen — lower the inner expression unchanged.
            Expr::As { expr, .. } => return self.lower_expr(expr),
            // A bare block expression: its `WirSeq` leaves the block's value.
            // (Region blocks keep their bespoke `compile_region` emission.)
            Expr::Block(b) if b.region.is_none() => return Some(W::Seq(self.lower_block(b)?)),
            // A `region:` block on the BINARY path. A SCALAR result (Int/Bool/Float)
            // lives in a register, not the heap, so we reclaim fully: capture the heap
            // watermark, run the body, stash the scalar in the universal i64 slot
            // (`$MATCH_TMP`), reset `$heap` to the watermark (freeing the body's
            // allocations), then recover the scalar. A POINTER result (list/record/…)
            // would be on the reclaimed heap, so it needs the per-shape `$rcopy_*`
            // deep-copy — deferred (a future wir_opt pass) — and lowers as a plain
            // block for now (correct value, no reclaim). WAT-path region blocks
            // (`collect_wir == false`) fall through to `compile_region`.
            Expr::Block(b) if self.collect_wir => {
                let ann = b.region.as_ref().and_then(|r| r.ty.clone());
                let shape = match &ann {
                    Some(t) => self.eq_shape_of_type(t),
                    None => match b.stmts.last() {
                        Some(Stmt::Expr(tail)) => self.eq_operand_shape(tail),
                        _ => None,
                    },
                };
                let is_scalar =
                    matches!(shape, Some(EqShape::Int | EqShape::Bool | EqShape::Float));
                if self.wm_level >= WM_POOL {
                    return Some(W::Seq(self.lower_block(b)?));
                }
                // A POINTER result lives on the reclaimed heap, so it needs its
                // per-shape `$rcopy_*` deep-copy. `ensure_rcopy_wir_helper` returns the
                // helper name (Str/List/Tuple so far) or `None` (an unsupported shape
                // or a recursive-type cycle) → fall back to a plain block (correct
                // value, no reclaim).
                let rcopy_helper: Option<String> = if is_scalar {
                    None
                } else {
                    match &shape {
                        Some(s) => self.ensure_rcopy_wir_helper(s),
                        None => None,
                    }
                };
                if !is_scalar && rcopy_helper.is_none() {
                    return Some(W::Seq(self.lower_block(b)?));
                }
                let wm = format!("__witchy_wm_{}", self.wm_level);
                let body_kind = self.block_kind(b);
                self.wm_level += 1;
                self.uses_wm = true;
                let mut body = self.lower_block(b)?;
                self.wm_level -= 1;
                // Split off the body's tail value. Its `Push(value)` is usually last,
                // but the uniqueness pass appends self-healing `*__cap` token writes
                // after it (block-scope cleanup); drain those, keeping them to run
                // before reclaim (cap tokens are scalars, untouched by the heap slide).
                let mut cap_heals: Vec<N> = vec![];
                while matches!(
                    body.last(),
                    Some(N::SetLocal { local, .. }) if local.ends_with("__cap")
                ) {
                    cap_heals.push(body.pop().unwrap());
                }
                let Some(N::Push(tail)) = body.pop() else {
                    return Some(W::Seq(self.lower_block(b)?));
                };
                cap_heals.reverse();
                body.extend(cap_heals);
                let mut seq = vec![N::SetLocal {
                    local: wm.clone(),
                    value: W::GetGlobal("heap".to_string()),
                }];
                seq.extend(body);
                if is_scalar {
                    seq.push(N::SetLocal {
                        local: MATCH_TMP.to_string(),
                        value: W::ToSlot(Box::new(tail), Self::wir_kind(body_kind)),
                    });
                    seq.push(N::SetGlobal { global: "heap".to_string(), value: W::GetLocal(wm) });
                    // (RFC-0016) The reset reclaims every address at/above the
                    // watermark, so any RC-floor free-list entry freed inside the body
                    // now dangles — drop the list (sound; a no-op when rc-floor is off).
                    seq.push(N::SetGlobal { global: "rc_freelist".to_string(), value: W::ConstI32(0) });
                    seq.push(N::Push(W::FromSlot(
                        Box::new(W::GetLocal(MATCH_TMP.to_string())),
                        Self::wir_kind(body_kind),
                    )));
                    return Some(W::Seq(seq));
                }
                // Pointer reclaim: stash the result ptr, set the rcopy globals (wm /
                // temp base = heap / slide delta), deep-copy it ABOVE the live data
                // (the helper returns a pre-biased ptr), `memory.copy` the finished
                // block down to the watermark, advance `$heap` past it, return the ptr.
                self.uses_region = true;
                let helper = rcopy_helper.expect("guarded pointer shape");
                let i32sub = |l: W, r: W| W::Binary {
                    op: witchy_wir::wir::BinOp::Sub,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                };
                let copied_len =
                    || i32sub(W::GetGlobal("heap".to_string()), W::GetGlobal("rcopy_base".to_string()));
                seq.push(N::SetLocal { local: TUPLE_TMP.to_string(), value: tail });
                seq.push(N::SetGlobal { global: "rcopy_wm".to_string(), value: W::GetLocal(wm.clone()) });
                seq.push(N::SetGlobal {
                    global: "rcopy_base".to_string(),
                    value: W::GetGlobal("heap".to_string()),
                });
                seq.push(N::SetGlobal {
                    global: "rcopy_delta".to_string(),
                    value: i32sub(W::GetGlobal("heap".to_string()), W::GetLocal(wm.clone())),
                });
                seq.push(N::SetLocal {
                    local: TUPLE_TMP.to_string(),
                    value: W::Call { func: helper, args: vec![W::GetLocal(TUPLE_TMP.to_string())] },
                });
                // (RFC-0023) The next `memory.copy` slides the result down over the
                // body's (and the deep-copy's) allocations, reusing every address at or
                // above the watermark — a raw copy `$ensure` never sees. Tell the checked
                // heap to drop those redzones first, so the reuse isn't read as an overrun.
                if witchy_wir::wir_helpers::heap_check_enabled() {
                    seq.push(N::Do(W::Call {
                        func: "__heap_reclaim".to_string(),
                        args: vec![W::GetLocal(wm.clone())],
                    }));
                }
                seq.push(N::MemoryCopy {
                    dest: W::GetLocal(wm.clone()),
                    src: W::GetGlobal("rcopy_base".to_string()),
                    len: copied_len(),
                });
                seq.push(N::SetGlobal {
                    global: "heap".to_string(),
                    value: W::Binary {
                        op: witchy_wir::wir::BinOp::Add,
                        kind: witchy_wir::wir::Kind::I32,
                        lhs: Box::new(W::GetLocal(wm)),
                        rhs: Box::new(copied_len()),
                    },
                });
                // (RFC-0016) As the scalar path: the slide-down reuses every address
                // at/above the watermark, so RC-floor free-list entries freed inside
                // the body dangle — drop the list (a no-op when rc-floor is off).
                seq.push(N::SetGlobal { global: "rc_freelist".to_string(), value: W::ConstI32(0) });
                seq.push(N::Push(W::GetLocal(TUPLE_TMP.to_string())));
                return Some(W::Seq(seq));
            }
            // `match` on scalar patterns; non-scalar arms fall through to legacy.
            Expr::Match { scrutinee, arms } => return self.lower_match(scrutinee, arms),
            // A lambda lowers to its closure-object creation (`$mk{c}`); the lifted
            // body is registered as a `WirFunc` + table entry.
            Expr::Lambda { params, body, .. } => return self.lower_lambda(params, body),
            // Call a closure value: stash the pointer, then `call_indirect` with
            // env (the closure ptr), the i64-slot args, and the code index (the
            // closure's first word).
            Expr::Apply { func, args } => {
                // Only a WIR-collecting scope lowers `Expr::Apply`; otherwise bail
                // so the construct is reported unsupported.
                if !self.collect_wir {
                    return None;
                }
                let level = self.apply_level;
                if level >= APPLY_POOL {
                    return None;
                }
                // (RFC-0062 tier-1) An ELIDED closure applied by name: no closure pointer to
                // stash — thread captures (from their locals) as leading arg slots to a direct
                // `call $__lamt{i}`.
                if let Expr::Var(fname) = func.as_ref() {
                    if let Some((idx, caps)) = self.thread_index.get(fname).cloned() {
                        self.apply_level = level + 1;
                        let mut call_args: Vec<W> = caps
                            .iter()
                            .map(|(cn, ck)| W::ToSlot(Box::new(W::GetLocal(cn.clone())), Self::wir_kind(*ck)))
                            .collect();
                        for a in args {
                            let ak = self.kind_of(a);
                            let av = self.lower_expr(a)?;
                            call_args.push(W::ToSlot(Box::new(av), Self::wir_kind(ak)));
                        }
                        self.apply_level = level;
                        let recover_kind = self.apply_ret_kind(func);
                        let call = W::Call { func: format!("__lamt{idx}"), args: call_args };
                        return Some(W::FromSlot(Box::new(call), Self::wir_kind(recover_kind)));
                    }
                }
                let n = args.len();
                let tmp = format!("__witchy_call_{level}");
                let fcode = self.lower_expr(func)?;
                self.apply_level = level + 1;
                let mut arg_slots: Vec<W> = Vec::new();
                for a in args {
                    let ak = self.kind_of(a);
                    let av = self.lower_expr(a)?;
                    arg_slots.push(W::ToSlot(Box::new(av), Self::wir_kind(ak)));
                }
                self.apply_level = level;
                self.clos_arities.insert(n);
                let recover_kind = self.apply_ret_kind(func);
                let mut ci_args = vec![W::GetLocal(tmp.clone())];
                ci_args.extend(arg_slots);
                // (RFC-0034 L3) Devirtualize an apply whose callee is a single-bound,
                // never-reassigned closure var: a direct `call $__lamw{i}` (env stays
                // the stashed closure pointer), skipping the runtime code-index load.
                let call = match func.as_ref() {
                    Expr::Var(fname) if self.devirt_index.contains_key(fname) => {
                        let idx = self.devirt_index[fname];
                        W::Call { func: format!("__lamw{idx}"), args: ci_args }
                    }
                    _ => W::CallIndirect {
                        type_arity: n,
                        args: ci_args,
                        index: Box::new(W::Load { ptr: Box::new(W::GetLocal(tmp.clone())), kind: witchy_wir::wir::Kind::I32, offset: 0 }),
                    },
                };
                let result = W::FromSlot(Box::new(call), Self::wir_kind(recover_kind));
                return Some(W::Seq(vec![
                    N::SetLocal { local: tmp, value: fcode },
                    N::Push(result),
                ]));
            }
            // `if cond { .. } else { .. }` value-if. CRITICAL: codegen lowers the
            // branch blocks BEFORE the cond in the `else` case (and cond first in
            // the no-`else` case); `intern` assigns string offsets in call order, so
            // the lowering order here must match codegen's exactly or data offsets
            // diverge.
            Expr::If { cond, then_block, else_block } => match else_block {
                Some(eb) => {
                    let tk = self.block_kind(then_block);
                    let ek = self.block_kind(eb);
                    let ck = if tk == Kind::F64 || ek == Kind::F64 {
                        Kind::F64
                    } else if tk == Kind::I64 || ek == Kind::I64 {
                        Kind::I64
                    } else {
                        Kind::I32
                    };
                    let then_ = Self::convert_block_tail(self.lower_block(then_block)?, tk, ck);
                    let els = Self::convert_block_tail(self.lower_block(eb)?, ek, ck);
                    let cond = self.lower_expr(cond)?;
                    W::Control(Box::new(N::If {
                        cond,
                        then_,
                        els,
                        result: Some(Self::wir_ty_for_kind(ck)),
                    }))
                }
                None => {
                    let cond = self.lower_expr(cond)?;
                    let then_ = self.lower_block(then_block)?;
                    W::Control(Box::new(N::If {
                        cond,
                        then_,
                        els: vec![N::Push(W::ConstI32(0))],
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    }))
                }
            },
            // `while cond { body }` — Nil-valued. Only the no-watermark variant
            // (the arena isn't reset around the body) is lowered; the watermark
            // framing (`global.get $heap` save/restore) stays in legacy. `next_label`
            // is allocated in codegen's order (this loop's id BEFORE the body's
            // nested loops), and restored on a bail so the counter never desyncs.
            Expr::While { cond, body } => {
                let saved = self.next_label;
                let id = self.next_label;
                self.next_label += 1;
                let cond_w = match self.lower_expr(cond) {
                    Some(c) => c,
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                // Per-iteration arena reset (the watermark), if the body is
                // resettable — same treatment as the for-loops.
                let wm = self.loop_watermark_wir(body);
                self.loop_labels.push((format!("$we{id}"), format!("$wl{id}")));
                let body_res = self.lower_block(body);
                self.loop_labels.pop();
                if wm.is_some() {
                    self.wm_level -= 1;
                }
                let body_seq = match body_res {
                    Some(b) => b,
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                let not_cond = W::Unary {
                    op: witchy_wir::wir::UnOp::Not,
                    kind: witchy_wir::wir::Kind::I32,
                    arg: Box::new(cond_w),
                };
                let mut loop_body = vec![
                    N::Br { target: format!("we{id}"), cond: Some(not_cond) },
                    N::Drop(W::Seq(body_seq)),
                ];
                // reclaim per-iteration arena garbage before re-testing the cond.
                if let Some((_, reset)) = &wm {
                    loop_body.push(reset.clone());
                }
                loop_body.push(N::Br { target: format!("wl{id}"), cond: None });
                let mut outer: witchy_wir::wir::WirSeq = Vec::new();
                if let Some((capture, _)) = &wm {
                    outer.push(capture.clone());
                }
                outer.push(N::Block {
                    label: format!("we{id}"),
                    result: None,
                    body: vec![N::Loop { label: format!("wl{id}"), body: loop_body }],
                });
                outer.push(N::Push(W::ConstI32(0)));
                W::Seq(outer)
            },
            // `for var in lo..hi { body }` — count without materializing a list.
            // i64 counter + bound in scratch locals; inclusive ranges add a
            // pre-increment `ctr == end` guard so `..=i64::MAX` halts.
            Expr::For { var, iter, body } if matches!(iter.as_ref(), Expr::Range { .. }) => {
                let Expr::Range { lo, hi, inclusive } = iter.as_ref() else { unreachable!() };
                let saved = self.next_label;
                let id = self.next_label;
                self.next_label += 1;
                let ctr = format!("__forctr_{var}");
                let end = format!("__forend_{var}");
                let lo_k = self.kind_of(lo);
                let lo_w = match self.lower_expr(lo) {
                    Some(w) => Self::wir_convert(w, lo_k, Kind::I64),
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                let hi_k = self.kind_of(hi);
                let hi_w = match self.lower_expr(hi) {
                    Some(w) => Self::wir_convert(w, hi_k, Kind::I64),
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                // Per-iteration arena reset (the watermark optimization): save
                // `$heap` before the loop, restore it after each body. `None` when
                // the body isn't resettable — the loop is still correct without it.
                let wm = self.loop_watermark_wir(body);
                // (RFC-0034 L2) Register `(i, xs)` for `for i in 0..list.length(xs)`
                // (xs unmutated in the body) so `list.at(xs, i)` lowers to an unchecked
                // load. Popped right after the body, so the stack stays balanced even on
                // the early `None` bail below.
                let elide_pair = bounds_elide_pair(var, lo, hi, *inclusive, body);
                if let Some(p) = &elide_pair {
                    self.elide_index_list.push(p.clone());
                }
                self.loop_labels.push((format!("$fe{id}"), format!("$fc{id}")));
                let body_res = self.lower_block(body);
                self.loop_labels.pop();
                if elide_pair.is_some() {
                    self.elide_index_list.pop();
                }
                if wm.is_some() {
                    self.wm_level -= 1;
                }
                let body_seq = match body_res {
                    Some(b) => b,
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                let i64k = witchy_wir::wir::Kind::I64;
                let cmp = |op, l: &str, r: &str| W::Binary {
                    op,
                    kind: i64k,
                    lhs: Box::new(W::GetLocal(l.to_string())),
                    rhs: Box::new(W::GetLocal(r.to_string())),
                };
                let exit_op = if *inclusive { witchy_wir::wir::BinOp::Gt } else { witchy_wir::wir::BinOp::Ge };
                let mut loop_body: witchy_wir::wir::WirSeq = vec![
                    N::Br { target: format!("fe{id}"), cond: Some(cmp(exit_op, &ctr, &end)) },
                    N::SetLocal { local: var.clone(), value: W::GetLocal(ctr.clone()) },
                    N::Block {
                        label: format!("fc{id}"),
                        result: None,
                        body: vec![N::Drop(W::Seq(body_seq))],
                    },
                ];
                // reclaim per-iteration arena garbage before the counter advance.
                if let Some((_, reset)) = &wm {
                    loop_body.push(reset.clone());
                }
                if *inclusive {
                    loop_body.push(N::Br {
                        target: format!("fe{id}"),
                        cond: Some(cmp(witchy_wir::wir::BinOp::Eq, &ctr, &end)),
                    });
                }
                loop_body.push(N::SetLocal {
                    local: ctr.clone(),
                    value: W::Binary {
                        op: witchy_wir::wir::BinOp::Add,
                        kind: i64k,
                        lhs: Box::new(W::GetLocal(ctr.clone())),
                        rhs: Box::new(W::ConstI64(1)),
                    },
                });
                loop_body.push(N::Br { target: format!("fl{id}"), cond: None });
                let mut outer: witchy_wir::wir::WirSeq = vec![
                    N::SetLocal { local: ctr, value: lo_w },
                    N::SetLocal { local: end, value: hi_w },
                ];
                if let Some((capture, _)) = &wm {
                    outer.push(capture.clone());
                }
                outer.push(N::Block {
                    label: format!("fe{id}"),
                    result: None,
                    body: vec![N::Loop { label: format!("fl{id}"), body: loop_body }],
                });
                outer.push(N::Push(W::ConstI32(0)));
                W::Seq(outer)
            },
            // `for var in list { body }` — Nil-valued; iterate a `[len][e0]...` list
            // with a pointer + index in scratch locals; an optional per-iteration
            // arena reset (the watermark) reclaims body garbage each time around.
            Expr::For { var, iter, body } if !matches!(iter.as_ref(), Expr::Range { .. }) => {
                let saved = self.next_label;
                let id = self.next_label;
                self.next_label += 1;
                let list_l = format!("__forlist_{var}");
                let idx_l = format!("__fori_{var}");
                let iter_w = match self.lower_expr(iter) {
                    Some(w) => w,
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                if let Some(elem) = self.elem_record_type_of(iter) {
                    self.local_records.insert(var.clone(), elem);
                }
                let elem_kind = self.iter_elem_kind(iter);
                // Watermark AFTER the list is built (`iter_w`), so the list and its
                // elements live below the reset point and survive each iteration.
                let wm = self.loop_watermark_wir(body);
                self.loop_labels.push((format!("$fe{id}"), format!("$fc{id}")));
                let body_res = self.lower_block(body);
                self.loop_labels.pop();
                if wm.is_some() {
                    self.wm_level -= 1;
                }
                let body_seq = match body_res {
                    Some(b) => b,
                    None => {
                        self.next_label = saved;
                        return None;
                    }
                };
                let i32 = witchy_wir::wir::Kind::I32;
                let add = witchy_wir::wir::BinOp::Add;
                // idx >= list.len  ->  br_if $fe
                let exit = N::Br {
                    target: format!("fe{id}"),
                    cond: Some(W::Binary {
                        op: witchy_wir::wir::BinOp::Ge,
                        kind: i32,
                        lhs: Box::new(W::GetLocal(idx_l.clone())),
                        rhs: Box::new(W::Load {
                            ptr: Box::new(W::GetLocal(list_l.clone())),
                            kind: i32,
                            offset: 0,
                        }),
                    }),
                };
                // var = from_slot( load( (list+4) + idx*8 ) )
                let elem_addr = W::Binary {
                    op: add,
                    kind: i32,
                    lhs: Box::new(W::Binary {
                        op: add,
                        kind: i32,
                        lhs: Box::new(W::GetLocal(list_l.clone())),
                        rhs: Box::new(W::ConstI32(4)),
                    }),
                    rhs: Box::new(W::Binary {
                        op: witchy_wir::wir::BinOp::Mul,
                        kind: i32,
                        lhs: Box::new(W::GetLocal(idx_l.clone())),
                        rhs: Box::new(W::ConstI32(8)),
                    }),
                };
                let bind = N::SetLocal {
                    local: var.clone(),
                    value: W::FromSlot(
                        Box::new(W::Load {
                            ptr: Box::new(elem_addr),
                            kind: witchy_wir::wir::Kind::I64,
                            offset: 0,
                        }),
                        Self::wir_kind(elem_kind),
                    ),
                };
                let body_block = N::Block {
                    label: format!("fc{id}"),
                    result: None,
                    body: vec![N::Drop(W::Seq(body_seq))],
                };
                let advance = N::SetLocal {
                    local: idx_l.clone(),
                    value: W::Binary {
                        op: add,
                        kind: i32,
                        lhs: Box::new(W::GetLocal(idx_l.clone())),
                        rhs: Box::new(W::ConstI32(1)),
                    },
                };
                let mut loop_body: witchy_wir::wir::WirSeq = vec![exit, bind, body_block];
                // reclaim per-iteration arena garbage before advancing the index.
                if let Some((_, reset)) = &wm {
                    loop_body.push(reset.clone());
                }
                loop_body.push(advance);
                loop_body.push(N::Br { target: format!("fl{id}"), cond: None });
                let mut outer: witchy_wir::wir::WirSeq = vec![
                    N::SetLocal { local: list_l, value: iter_w },
                    N::SetLocal { local: idx_l, value: W::ConstI32(0) },
                ];
                if let Some((capture, _)) = &wm {
                    outer.push(capture.clone());
                }
                outer.push(N::Block {
                    label: format!("fe{id}"),
                    result: None,
                    body: vec![N::Loop { label: format!("fl{id}"), body: loop_body }],
                });
                outer.push(N::Push(W::ConstI32(0)));
                W::Seq(outer)
            },
            // Aggregate literals: a list is `[len][elems..]`, a tuple is
            // `[0][elems..]`, a constructor is `[tag][fields..]` — all via `$mkN`.
            Expr::List(items) => return self.lower_aggregate(items.len() as i32, items, 0),
            Expr::Tuple(items) => return self.lower_aggregate(0, items, 0),
            Expr::Ctor { name, args } => {
                let &(tag, nfields) = self.ctors.get(name)?;
                if nfields != args.len() {
                    return None; // arity mismatch → legacy emits the loud error
                }
                // (RFC-0037 §3) Tag records (ctor name == type name, so the write here and the
                // `.field` check agree). ADT variants stay untagged (the type name differs from
                // the variant name); the tolerant check skips them.
                let type_tag = if self.record_fields.contains_key(name) { type_tag_of(name) } else { 0 };
                return self.lower_aggregate(tag as i32, args, type_tag);
            }
            // `update rec { field: v }` rebuilds the record: tag, then each field —
            // an overridden value (in a slot) or the base's raw slot copied across.
            // Only the bare-variable base is lowered (the base read directly); a
            // non-`Var` base needs the scratch-local pool, so it stays in legacy.
            Expr::RecordUpdate { base, fields } => {
                let tyname = self.record_type_of(base)?;
                let names = self.record_fields.get(&tyname)?.clone();
                let &(tag, nfields) = self.ctors.get(&tyname)?;
                self.mk_arities.insert(nfields);
                // A Var base is referenced directly; any other expression is
                // evaluated ONCE into the `$TUPLE_TMP` scratch (base-first, as the
                // interpreter does) so each un-updated field reads the same value.
                let (prelude, base_ptr): (Option<witchy_wir::wir::WirNode>, W) =
                    if let Expr::Var(v) = base.as_ref() {
                        (None, W::GetLocal(v.clone()))
                    } else {
                        let bw = self.lower_expr(base)?;
                        (
                            Some(witchy_wir::wir::WirNode::SetLocal { local: TUPLE_TMP.into(), value: bw }),
                            W::GetLocal(TUPLE_TMP.into()),
                        )
                    };
                let mut args = Vec::with_capacity(nfields + 1);
                args.push(W::ConstI32(tag as i32));
                for (i, (fname, _)) in names.iter().enumerate() {
                    if let Some((_, vexpr)) = fields.iter().find(|(n, _)| n == fname) {
                        let k = self.kind_of(vexpr);
                        let w = self.lower_expr(vexpr)?;
                        args.push(W::ToSlot(Box::new(w), Self::wir_kind(k)));
                    } else {
                        args.push(W::Load {
                            ptr: Box::new(W::Binary {
                                op: witchy_wir::wir::BinOp::Add,
                                kind: witchy_wir::wir::Kind::I32,
                                lhs: Box::new(base_ptr.clone()),
                                rhs: Box::new(W::ConstI32((4 + 8 * i) as i32)),
                            }),
                            kind: witchy_wir::wir::Kind::I64,
                            offset: 0,
                        });
                    }
                }
                let call = W::Call { func: format!("mk{nfields}"), args };
                return Some(match prelude {
                    Some(p) => W::Seq(vec![p, witchy_wir::wir::WirNode::Push(call)]),
                    None => call,
                });
            }
            Expr::Binary { op, lhs, rhs } => {
                // `&&`/`||` are short-circuit control flow, not a wasm binary op:
                // lower to a value-`if`.
                //   a && b  ->  if a { b } else { 0 }
                //   a || b  ->  if a { 1 } else { b }
                if matches!(op, BinOp::And | BinOp::Or) {
                    let cond = self.lower_expr(lhs)?;
                    let other = self.lower_expr(rhs)?;
                    let (then_, els) = if matches!(op, BinOp::And) {
                        (vec![witchy_wir::wir::WirNode::Push(other)], vec![
                            witchy_wir::wir::WirNode::Push(W::ConstI32(0)),
                        ])
                    } else {
                        (vec![witchy_wir::wir::WirNode::Push(W::ConstI32(1))], vec![
                            witchy_wir::wir::WirNode::Push(other),
                        ])
                    };
                    return Some(W::Control(Box::new(witchy_wir::wir::WirNode::If {
                        cond,
                        then_,
                        els,
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    })));
                }
                // `a ?? b` (RFC-0048): unwrap `Some`/`Ok` (tag 0, payload at
                // `tmp+4`) or evaluate the fallback — the same store-once/tag-test
                // shape as `?`, minus the early return. The fallback is lowered
                // inside the else branch, so it runs only on `None`/`Err`.
                if *op == BinOp::Coalesce {
                    use witchy_wir::wir::WirNode as N;
                    let k = self
                        .match_payload_valtype(lhs)
                        .map(valtype_kind)
                        .unwrap_or_else(|| self.kind_of(rhs));
                    let lhs_w = self.lower_expr(lhs)?;
                    let rhs_w = Self::wir_convert(self.lower_expr(rhs)?, self.kind_of(rhs), k);
                    let tmp = TRY_TMP.to_string();
                    let cond = W::Unary {
                        op: witchy_wir::wir::UnOp::Not,
                        kind: witchy_wir::wir::Kind::I32,
                        arg: Box::new(W::Load {
                            ptr: Box::new(W::GetLocal(tmp.clone())),
                            kind: witchy_wir::wir::Kind::I32,
                            offset: 0,
                        }),
                    };
                    let payload = W::FromSlot(
                        Box::new(W::Load {
                            ptr: Box::new(W::Binary {
                                op: witchy_wir::wir::BinOp::Add,
                                kind: witchy_wir::wir::Kind::I32,
                                lhs: Box::new(W::GetLocal(tmp.clone())),
                                rhs: Box::new(W::ConstI32(4)),
                            }),
                            kind: witchy_wir::wir::Kind::I64,
                            offset: 0,
                        }),
                        Self::wir_kind(k),
                    );
                    return Some(W::Seq(vec![
                        N::SetLocal { local: tmp.clone(), value: lhs_w },
                        N::If {
                            cond,
                            then_: vec![N::Push(payload)],
                            els: vec![N::Push(rhs_w)],
                            result: Some(Self::wir_ty_for_kind(k)),
                        },
                    ]));
                }
                // String concatenation (`+` flipped to `Concat`) lowers to
                // `$concat` (only in a WIR-collecting scope; otherwise this falls
                // through and the program is rejected as unsupported).
                if self.collect_wir && *op == BinOp::Concat {
                    let a = self.lower_expr(lhs)?;
                    let b = self.lower_expr(rhs)?;
                    return Some(W::Call { func: "concat".to_string(), args: vec![a, b] });
                }
                // String content equality lowers to `$str_eq` (only in a
                // WIR-collecting scope). `!=` is `i32.eqz` of the equality result.
                if self.collect_wir
                    && matches!(op, BinOp::Eq | BinOp::NotEq)
                    && self.val_type_of(lhs) == ValType::Str
                    && self.val_type_of(rhs) == ValType::Str
                {
                    let a = self.lower_expr(lhs)?;
                    let b = self.lower_expr(rhs)?;
                    let eq = W::Call { func: "str_eq".to_string(), args: vec![a, b] };
                    return Some(match op {
                        BinOp::Eq => eq,
                        _ => W::Unary {
                            op: witchy_wir::wir::UnOp::Not,
                            kind: witchy_wir::wir::Kind::I32,
                            arg: Box::new(eq),
                        },
                    });
                }
                // String ordering (`<`/`<=`/`>`/`>=`) lowers to a sign compare of
                // `$str_cmp(a, b)` against 0 (binary path only) — lexicographic, not
                // pointer order. `==`/`!=` were handled by the `$str_eq` block above.
                if self.collect_wir
                    && matches!(op, BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq)
                    && (self.val_type_of(lhs) == ValType::Str
                        || self.val_type_of(rhs) == ValType::Str)
                {
                    self.uses_str_cmp = true;
                    let a = self.lower_expr(lhs)?;
                    let b = self.lower_expr(rhs)?;
                    let cmp = W::Call { func: "str_cmp".to_string(), args: vec![a, b] };
                    let wop = match op {
                        BinOp::Lt => witchy_wir::wir::BinOp::Lt,
                        BinOp::LtEq => witchy_wir::wir::BinOp::Le,
                        BinOp::Gt => witchy_wir::wir::BinOp::Gt,
                        _ => witchy_wir::wir::BinOp::Ge,
                    };
                    return Some(W::Binary {
                        op: wop,
                        kind: witchy_wir::wir::Kind::I32,
                        lhs: Box::new(cmp),
                        rhs: Box::new(W::ConstI32(0)),
                    });
                }
                // Compound (record/tuple/list/enum) equality lowers to the
                // per-shape WIR structural-equality helper (binary path only),
                // gated exactly like the legacy arm (`eq_shape_of(..).is_compound`).
                // A compound `==` MUST be structural, so once the shape is known to
                // be compound we either build the helper or bail the whole function
                // (`?`) — never fall through to a bare pointer compare.
                if self.collect_wir && matches!(op, BinOp::Eq | BinOp::NotEq) {
                    if let Some(shape) = self.eq_shape_of(lhs).or_else(|| self.eq_shape_of(rhs)) {
                        if shape.is_compound() {
                            // A compound `==` MUST be structural; if the shape can't
                            // be built (an unresolved generic payload, e.g. `Ok([])`)
                            // it is a HARD rejection, never a fall-through to a bare
                            // pointer compare.
                            let Some(h) = self.ensure_eq_wir_helper(&shape) else {
                                self.reject_reason.get_or_insert_with(|| CodegenError {
                                    message: "could not resolve the structural-equality shape for a \
                                              compound `==` (an unresolved generic payload, e.g. an \
                                              empty list) — annotate the operands' element type"
                                        .into(),
                                });
                                return None;
                            };
                            let a = self.lower_expr(lhs)?;
                            let b = self.lower_expr(rhs)?;
                            let eq = W::Call { func: h, args: vec![a, b] };
                            return Some(match op {
                                BinOp::Eq => eq,
                                _ => W::Unary { op: witchy_wir::wir::UnOp::Not, kind: witchy_wir::wir::Kind::I32, arg: Box::new(eq) },
                            });
                        }
                    }
                }
                // Float ordering (`<`/`<=`/`>`/`>=` on f64) lowers to the
                // NaN-trapping helper `$f_lt`/`$f_le`/`$f_gt`/`$f_ge` (binary path
                // only); the interpreter errors on ordering a NaN, so a bare
                // `f64.lt` would diverge from the oracle.
                if self.collect_wir
                    && matches!(op, BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq)
                    && (self.kind_of(lhs) == Kind::F64 || self.kind_of(rhs) == Kind::F64)
                {
                    self.uses_float_ord = true;
                    let func = match op {
                        BinOp::Lt => "f_lt",
                        BinOp::LtEq => "f_le",
                        BinOp::Gt => "f_gt",
                        _ => "f_ge",
                    };
                    let a = self.lower_expr(lhs)?;
                    let b = self.lower_expr(rhs)?;
                    return Some(W::Call { func: func.to_string(), args: vec![a, b] });
                }
                // Logical `&&`/`||` lower to a short-circuit value-`If` (binary path
                // only): `a && b` is `if a { b } else { false }`, `a || b` is
                // `if a { true } else { b }`, so the right operand is evaluated only
                // when the left doesn't already decide the result. This matches the
                // interpreter and preserves guards like `i < n && list.at(xs, i)`
                // that depend on the RHS not running when the LHS is false.
                if self.collect_wir && matches!(op, BinOp::And | BinOp::Or) {
                    let a = self.lower_expr(lhs)?;
                    let b = self.lower_expr(rhs)?;
                    let (then_, els) = match op {
                        BinOp::And => (vec![N::Push(b)], vec![N::Push(W::ConstI32(0))]),
                        _ => (vec![N::Push(W::ConstI32(1))], vec![N::Push(b)]),
                    };
                    return Some(W::Control(Box::new(N::If {
                        cond: a,
                        then_,
                        els,
                        result: Some(witchy_wir::wir::WirTy::Bool),
                    })));
                }
                // Common-kind promotion (f64 > i64 > i32), exactly as the legacy
                // numeric path; each operand is then widened to `ck` via a `Convert`
                // node reproducing `kind_convert` (a no-op except i32<->i64). `ck`
                // is computed first because the float-ordering guard below needs it.
                let lk = self.kind_of(lhs);
                let rk = self.kind_of(rhs);
                let ck = if lk == Kind::F64 || rk == Kind::F64 {
                    Kind::F64
                } else if lk == Kind::I64 || rk == Kind::I64 {
                    Kind::I64
                } else {
                    Kind::I32
                };
                // The plain numeric path only. Every special case returns `None` so
                // the legacy arm keeps its exact emission.
                let wop = match op {
                    // `Add` is string concat when either operand is a `Str`.
                    BinOp::Add
                        if self.val_type_of(lhs) == ValType::Str
                            || self.val_type_of(rhs) == ValType::Str =>
                    {
                        return None;
                    }
                    BinOp::Add => witchy_wir::wir::BinOp::Add,
                    BinOp::Sub => witchy_wir::wir::BinOp::Sub,
                    BinOp::Mul => witchy_wir::wir::BinOp::Mul,
                    BinOp::Div => witchy_wir::wir::BinOp::Div,
                    BinOp::Mod => witchy_wir::wir::BinOp::Rem,
                    BinOp::BitAnd => witchy_wir::wir::BinOp::And,
                    BinOp::BitOr => witchy_wir::wir::BinOp::Or,
                    BinOp::BitXor => witchy_wir::wir::BinOp::Xor,
                    BinOp::Shl => witchy_wir::wir::BinOp::Shl,
                    BinOp::Shr => witchy_wir::wir::BinOp::Shr,
                    BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                        // String compares ($str_eq/$str_cmp), the structural eq
                        // helper for compounds, the loud rejects (dict ==, compound
                        // ordering, generic-reference compares), and float *ordering*
                        // ($f_lt/$f_le/$f_gt/$f_ge, NaN-trapping — not f64.lt) all
                        // keep their bespoke legacy emission.
                        if self.val_type_of(lhs) == ValType::Str
                            || self.val_type_of(rhs) == ValType::Str
                            || self.operand_is_compound(lhs)
                            || self.operand_is_compound(rhs)
                            || self.is_dict_operand(lhs)
                            || self.is_dict_operand(rhs)
                            || self.is_generic_ref_compare(lhs, rhs)
                            || (ck == Kind::F64
                                && matches!(op, BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq))
                        {
                            return None;
                        }
                        match op {
                            BinOp::Eq => witchy_wir::wir::BinOp::Eq,
                            BinOp::NotEq => witchy_wir::wir::BinOp::Ne,
                            BinOp::Lt => witchy_wir::wir::BinOp::Lt,
                            BinOp::LtEq => witchy_wir::wir::BinOp::Le,
                            BinOp::Gt => witchy_wir::wir::BinOp::Gt,
                            BinOp::GtEq => witchy_wir::wir::BinOp::Ge,
                            _ => unreachable!(),
                        }
                    }
                    // concat / `&&` / `||` keep their legacy emission.
                    _ => return None,
                };
                let lhs_w = Self::wir_convert(self.lower_expr(lhs)?, lk, ck);
                let rhs_w = Self::wir_convert(self.lower_expr(rhs)?, rk, ck);
                W::Binary { op: wop, kind: Self::wir_kind(ck), lhs: Box::new(lhs_w), rhs: Box::new(rhs_w) }
            }
            // Tuple element (`pair.0`) or record field (`rec.name`): both read an
            // i64 slot at `base + 4 + 8*idx`, recovered at the field's kind. The
            // legacy emission is `base; i32.const off; i32.add; i64.load;
            // from_slot(k)` — reproduced as `FromSlot(Load{Add(base, off)}, k)`.
            Expr::Field { base, field } => {
                // (RFC-0027 packed) `list.at(xs, i).field` on a packed record-list
                // reads the inline slot directly — element `i`, field `j` lives at
                // `xs + 4 + (i*nfields + j)*8`, the same per-field i64-slot rep a
                // boxed record uses, just flattened. One load instead of a pointer
                // deref + a field load. Only for names the `let` actually packed.
                if let Expr::Call { name: at, args } = base.as_ref() {
                    if at == "list.at" && args.len() == 2 {
                        if let Expr::Var(xs) = &args[0] {
                            if let Some(rec) = self.packed_active.get(xs).cloned() {
                                let names = self.record_fields.get(&rec)?;
                                let nfields = names.len();
                                let j = names.iter().position(|(n, _)| n == field)?;
                                let fkind = name_kind(names[j].1.as_deref());
                                let ik = self.kind_of(&args[1]);
                                let idx = Self::wir_convert(self.lower_expr(&args[1])?, ik, Kind::I32);
                                // addr = xs + (4 + j*8) + i*(nfields*8)
                                let row = W::Binary {
                                    op: witchy_wir::wir::BinOp::Mul,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(idx),
                                    rhs: Box::new(W::ConstI32((nfields * 8) as i32)),
                                };
                                let base_off = W::Binary {
                                    op: witchy_wir::wir::BinOp::Add,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(W::GetLocal(xs.clone())),
                                    rhs: Box::new(W::ConstI32((4 + j * 8) as i32)),
                                };
                                let addr = W::Binary {
                                    op: witchy_wir::wir::BinOp::Add,
                                    kind: witchy_wir::wir::Kind::I32,
                                    lhs: Box::new(base_off),
                                    rhs: Box::new(row),
                                };
                                let read = W::FromSlot(
                                    Box::new(W::Load {
                                        ptr: Box::new(addr),
                                        kind: witchy_wir::wir::Kind::I64,
                                        offset: 0,
                                    }),
                                    Self::wir_kind(fkind),
                                );
                                // (RFC-0037 §3) Under WITCHY_TYPE_CHECK, verify the flat buffer's
                                // `packed:` tag before the inline read — catching a boxed/packed
                                // layout confusion at the access site. `xs` is a var, so no temp.
                                if witchy_wir::wir_helpers::type_check_enabled() {
                                    use witchy_wir::wir::{BinOp, Kind as K, WirNode as N};
                                    let expected = type_tag_of(&format!("packed:{rec}"));
                                    let tag = || W::Binary {
                                        op: BinOp::ShrU,
                                        kind: K::I32,
                                        lhs: Box::new(W::Load {
                                            ptr: Box::new(W::Binary { op: BinOp::Sub, kind: K::I32, lhs: Box::new(W::GetLocal(xs.clone())), rhs: Box::new(W::ConstI32(4)) }),
                                            kind: K::I32,
                                            offset: 0,
                                        }),
                                        rhs: Box::new(W::ConstI32(24)),
                                    };
                                    let mismatch = W::Binary {
                                        op: BinOp::And,
                                        kind: K::I32,
                                        lhs: Box::new(W::Binary { op: BinOp::Ne, kind: K::I32, lhs: Box::new(tag()), rhs: Box::new(W::ConstI32(0)) }),
                                        rhs: Box::new(W::Binary { op: BinOp::Ne, kind: K::I32, lhs: Box::new(tag()), rhs: Box::new(W::ConstI32(expected as i32)) }),
                                    };
                                    return Some(W::Seq(vec![
                                        N::If { cond: mismatch, then_: vec![N::Unreachable], els: vec![], result: None },
                                        N::Push(read),
                                    ]));
                                }
                                return Some(read);
                            }
                        }
                    }
                }
                // (RFC-0027) A scalar-replaced aggregate's field is read straight
                // from its `${p}$<i>` slot local — no heap load. Only for names the
                // `let` actually replaced (`sroa_active`); a `let` precedes its uses
                // in statement order, so the set is populated first.
                if let Expr::Var(p) = base.as_ref() {
                    if self.sroa_active.contains_key(p) {
                        let (idx, kind) = if let Ok(i) = field.parse::<usize>() {
                            (i, valtype_kind(self.val_type_of(e)))
                        } else {
                            let base_ty = self.record_type_of(base)?;
                            let names = self.record_fields.get(&base_ty)?;
                            let i = names.iter().position(|(n, _)| n == field)?;
                            (i, name_kind(names[i].1.as_deref()))
                        };
                        return Some(W::FromSlot(
                            Box::new(W::GetLocal(format!("{p}${idx}"))),
                            Self::wir_kind(kind),
                        ));
                    }
                }
                let (offset, kind) = if let Ok(i) = field.parse::<usize>() {
                    (4 + 8 * i, valtype_kind(self.val_type_of(e)))
                } else {
                    let base_ty = self.record_type_of(base)?;
                    let names = self.record_fields.get(&base_ty)?;
                    let idx = names.iter().position(|(n, _)| n == field)?;
                    (4 + 8 * idx, name_kind(names[idx].1.as_deref()))
                };
                // (RFC-0037 §3) Under WITCHY_TYPE_CHECK, verify the record pointer's type tag
                // (the alloc-header high byte at p-4) matches its statically-known type before
                // the field load — trapping a layout / `unbox` confusion AT the access site.
                // Tolerant of an untagged pointer (tag 0), so a value built by a not-yet-tagged
                // path never false-traps; a genuine mismatch (tag != 0 and != expected) traps.
                if witchy_wir::wir_helpers::type_check_enabled() {
                    if let Some(expected) = self.record_type_of(base).map(|t| type_tag_of(&t)) {
                        use witchy_wir::wir::{BinOp, Kind as K, WirNode as N};
                        let base_ptr = self.lower_expr(base)?;
                        let tmp = || W::GetLocal(TYPECHECK_TMP.to_string());
                        let tag = || W::Binary {
                            op: BinOp::ShrU,
                            kind: K::I32,
                            lhs: Box::new(W::Load {
                                ptr: Box::new(W::Binary { op: BinOp::Sub, kind: K::I32, lhs: Box::new(tmp()), rhs: Box::new(W::ConstI32(4)) }),
                                kind: K::I32,
                                offset: 0,
                            }),
                            rhs: Box::new(W::ConstI32(24)),
                        };
                        let mismatch = W::Binary {
                            op: BinOp::And,
                            kind: K::I32,
                            lhs: Box::new(W::Binary { op: BinOp::Ne, kind: K::I32, lhs: Box::new(tag()), rhs: Box::new(W::ConstI32(0)) }),
                            rhs: Box::new(W::Binary { op: BinOp::Ne, kind: K::I32, lhs: Box::new(tag()), rhs: Box::new(W::ConstI32(expected as i32)) }),
                        };
                        let read = W::FromSlot(
                            Box::new(W::Load {
                                ptr: Box::new(W::Binary { op: BinOp::Add, kind: K::I32, lhs: Box::new(tmp()), rhs: Box::new(W::ConstI32(offset as i32)) }),
                                kind: K::I64,
                                offset: 0,
                            }),
                            Self::wir_kind(kind),
                        );
                        return Some(W::Seq(vec![
                            N::SetLocal { local: TYPECHECK_TMP.to_string(), value: base_ptr },
                            N::If { cond: mismatch, then_: vec![N::Unreachable], els: vec![], result: None },
                            N::Push(read),
                        ]));
                    }
                }
                let addr = W::Binary {
                    op: witchy_wir::wir::BinOp::Add,
                    kind: witchy_wir::wir::Kind::I32,
                    lhs: Box::new(self.lower_expr(base)?),
                    rhs: Box::new(W::ConstI32(offset as i32)),
                };
                W::FromSlot(
                    Box::new(W::Load {
                        ptr: Box::new(addr),
                        kind: witchy_wir::wir::Kind::I64,
                        offset: 0,
                    }),
                    Self::wir_kind(kind),
                )
            }
            // `e?`: store the operand once, then a value-`if` on its tag — take the
            // success payload (tag 0, at `tmp+4`) or early-`return` the whole
            // Err/None. The `var` epilogue variant stays in legacy.
            Expr::Try(inner) => {
                use witchy_wir::wir::WirNode as N;
                let payload_kind =
                    self.match_payload_valtype(inner).map(valtype_kind).unwrap_or(Kind::I32);
                let inner_w = self.lower_expr(inner)?;
                let tmp = TRY_TMP.to_string();
                let cond = W::Unary {
                    op: witchy_wir::wir::UnOp::Not,
                    kind: witchy_wir::wir::Kind::I32,
                    arg: Box::new(W::Load {
                        ptr: Box::new(W::GetLocal(tmp.clone())),
                        kind: witchy_wir::wir::Kind::I32,
                        offset: 0,
                    }),
                };
                let payload = W::FromSlot(
                    Box::new(W::Load {
                        ptr: Box::new(W::Binary {
                            op: witchy_wir::wir::BinOp::Add,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(W::GetLocal(tmp.clone())),
                            rhs: Box::new(W::ConstI32(4)),
                        }),
                        kind: witchy_wir::wir::Kind::I64,
                        offset: 0,
                    }),
                    Self::wir_kind(payload_kind),
                );
                let zero = match payload_kind {
                    Kind::I64 => W::ConstI64(0),
                    Kind::F64 => W::ConstF64(0.0),
                    Kind::I32 => W::ConstI32(0),
                };
                // The Err path early-returns the Err Result. In an var/own-ABI
                // fn the return must carry the full multi-result tuple (the Err
                // value, then each var param's current value, then the own-cap),
                // matching `assemble_wir_func`'s tail — so the var writeback still
                // happens on the `?` error path. Then a bare `Return(None)`.
                let mut els: Vec<N> =
                    if self.cur_fn_var_params.is_empty() && self.cur_fn_own_param.is_none() {
                        // `?` early-returns the whole Err/None aggregate (an i32
                        // pointer). Inside a closure the function returns an i64
                        // slot (the call-indirect ABI), so the value must be
                        // slot-widened — mirroring the normal-tail conversion in
                        // `build_lambda_wir_func`. A plain function returns the
                        // pointer directly.
                        let ret_val = if self.cur_fn_ret_slot {
                            W::ToSlot(Box::new(W::GetLocal(tmp.clone())), witchy_wir::wir::Kind::I32)
                        } else {
                            W::GetLocal(tmp.clone())
                        };
                        vec![N::Return(Some(ret_val))]
                    } else {
                        let mut nodes = vec![N::Push(W::GetLocal(tmp.clone()))];
                        for name in &self.cur_fn_var_params {
                            nodes.push(N::Push(W::GetLocal(name.clone())));
                        }
                        if self.cur_fn_own_param.is_some() {
                            nodes.push(N::Push(W::ConstI32(0)));
                        }
                        nodes.push(N::Return(None));
                        nodes
                    };
                // Unreachable, but keeps the `els` branch stack-typed at payload_kind.
                els.push(N::Push(zero));
                W::Seq(vec![
                    N::SetLocal { local: tmp.clone(), value: inner_w },
                    N::If {
                        cond,
                        then_: vec![N::Push(payload)],
                        els,
                        result: Some(Self::wir_ty_for_kind(payload_kind)),
                    },
                ])
            }
            // A call expression. Builtins/natives that WIR can lower flow through
            // `lower_call`; otherwise a plain top-level user call (no own-ABI
            // token, no `var` writeback, not a closure-typed local) lowers via
            // `try_lower_user_call`. The arm precedence here (builtin/native first,
            // then direct user call, then closure-local) is the call dispatch; any
            // other call shape (own-ABI, `var`) returns `None` and is rejected.
            Expr::Call { name, args } => {
                // Only a WIR-collecting scope lowers calls; otherwise bail so the
                // construct is reported unsupported. `lower_call` owns the
                // builtin/native arm precedence (e.g. `math.sqrt` is an intrinsic,
                // not a `$`-func), which this arm does not re-derive.
                if !self.collect_wir {
                    return None;
                }
                if let Some(w) = self.lower_call(name, args) {
                    return Some(w);
                }
                // (RFC-0062 tier-1) An ELIDED closure local `f(x)`: no env exists — thread
                // the captures (from their current locals) as leading i64 arg slots, then
                // the value args, to a direct `call $__lamt{i}`. Checked BEFORE the boxed
                // closure paths.
                if let Some((idx, caps)) = self.thread_index.get(name).cloned() {
                    let mut call_args: Vec<W> = caps
                        .iter()
                        .map(|(cn, ck)| W::ToSlot(Box::new(W::GetLocal(cn.clone())), Self::wir_kind(*ck)))
                        .collect();
                    for a in args {
                        let ak = self.kind_of(a);
                        let av = self.lower_expr(a)?;
                        call_args.push(W::ToSlot(Box::new(av), Self::wir_kind(ak)));
                    }
                    let rk = self.local_fn_ret_kind.get(name).copied().unwrap_or(Kind::I32);
                    let call = W::Call { func: format!("__lamt{idx}"), args: call_args };
                    return Some(W::FromSlot(Box::new(call), Self::wir_kind(rk)));
                }
                // A closure-typed local `f(x)`: pass the closure pointer as the env,
                // the i64-slot args, and `call_indirect` on the code index (the
                // closure record's first word). The pointer is a bare `GetLocal`,
                // so no scratch stash is needed.
                if self.locals.contains_key(name) {
                    let n = args.len();
                    let mut ci_args: Vec<W> = vec![W::GetLocal(name.to_string())];
                    for a in args {
                        let ak = self.kind_of(a);
                        let av = self.lower_expr(a)?;
                        ci_args.push(W::ToSlot(Box::new(av), Self::wir_kind(ak)));
                    }
                    self.clos_arities.insert(n);
                    let rk = self.local_fn_ret_kind.get(name).copied().unwrap_or(Kind::I32);
                    // (RFC-0034 L3) Devirtualize when `name` is a single-bound, never-
                    // reassigned closure local (`devirt_index`): a direct `call
                    // $__lamw{i}` — same env (the closure pointer) and slot args, just
                    // skipping the runtime code-index load — which also lets the
                    // Binaryen pass inline the lambda body into the caller.
                    let call = if let Some(&idx) = self.devirt_index.get(name) {
                        W::Call { func: format!("__lamw{idx}"), args: ci_args }
                    } else {
                        W::CallIndirect {
                            type_arity: n,
                            args: ci_args,
                            index: Box::new(W::Load {
                                ptr: Box::new(W::GetLocal(name.to_string())),
                                kind: witchy_wir::wir::Kind::I32,
                                offset: 0,
                            }),
                        }
                    };
                    return Some(W::FromSlot(Box::new(call), Self::wir_kind(rk)));
                }
                let has_var = self
                    .fn_conventions
                    .get(name)
                    .is_some_and(|cs| cs.contains(&Convention::Var));
                // An `var` user call: the callee returns its declared value plus one
                // result per var param (the multi-value move-out ABI). Lower to a
                // `CallStoreMulti` that writes each var result back into the caller's
                // local var, then yield the declared value.
                if has_var
                    && self.emitted_funcs.contains(name)
                    && !self.locals.contains_key(name)
                    && self.summaries.own_abi(name).is_none()
                {
                    return self.lower_var_call(name, args);
                }
                // Exactly the compiled `$name` user functions — never an
                // intrinsic/native (those have no emitted func to call), never a
                // closure-typed local (that's a `call_indirect`).
                let is_plain_user_fn = self.emitted_funcs.contains(name)
                    && !self.locals.contains_key(name)
                    && !self.local_fn_ret_kind.contains_key(name);
                if is_plain_user_fn && !has_var {
                    return self.try_lower_user_call(name, args);
                }
                return None;
            }
            _ => return None,
        })
    }

    /// Lower a lambda to its closure-object creation expression (the `$mk{c}` call
    /// producing `[code_index][caps..]`), registering the lifted body `WirFunc` in
    /// `lambda_wir_funcs` once (idempotent by content hash). `None` (the program is
    /// then rejected as unsupported) when the lambda assigns a captured var or its
    /// body doesn't fully lower.
    /// The content hash keying a lambda's idempotent registration (and the
    /// `lambda_wir_index` lookup the devirt binding-recorder reuses to recover the
    /// `$__lamw{i}` index a `let f = <lambda>` was assigned).
    fn lambda_content_key(params: &[Param], body: &Block) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{params:?}{body:?}").hash(&mut h);
        h.finish()
    }

    fn lower_lambda(&mut self, params: &[Param], body: &Block) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        // Only a WIR-collecting scope lowers lambdas; otherwise bail so the
        // construct is reported unsupported.
        if !self.collect_wir {
            return None;
        }
        let scan = scan_lambda(params, body);
        let assigns_outer = scan.assigns_outer();
        if !assigns_outer.is_empty() {
            // A hard rejection (by-value capture can't propagate a write back), not
            // an "unsupported" bail: record it so `compile_function` errors with a
            // diagnostic instead of silently reporting the module as unsupported.
            self.reject_reason.get_or_insert_with(|| CodegenError {
                message: format!(
                    "a closure that assigns to a captured variable is not compiled yet (assigns `{}`)",
                    assigns_outer.join("`, `")
                ),
            });
            return None;
        }
        let captures: Vec<String> = scan
            .captures()
            .into_iter()
            .filter(|c| self.locals.contains_key(c))
            .collect();
        let mut cap_info: Vec<CaptureInfo> = Vec::new();
        for c in &captures {
            let kind = self.locals.get(c).copied().unwrap_or(Kind::I32);
            cap_info.push((
                c.clone(),
                self.local_records.get(c).cloned(),
                self.local_list_elem.get(c).cloned(),
                kind,
            ));
        }
        // The capture slots are read at the CREATION site (current scope), before
        // any scope swap, each widened into the universal i64 env slot.
        let cap_slots: Vec<W> = cap_info
            .iter()
            .map(|(name, _, _, kind)| {
                let v = W::GetLocal(name.clone());
                W::ToSlot(Box::new(v), Self::wir_kind(*kind))
            })
            .collect();
        let ncaps = cap_info.len();

        // Idempotent registration: the same lambda (by content) gets one lifted
        // body + one stable table index across the many lowering passes.
        let key = Self::lambda_content_key(params, body);
        let index = if let Some(&i) = self.lambda_wir_index.get(&key) {
            i
        } else {
            let mut func = self.build_lambda_wir_func(params, body, &cap_info, CapMode::Env)?;
            // `build_lambda_wir_func` names itself `__lamw{len}` from the length at
            // its START, but a NESTED lambda lowered during the build pushes to
            // `lambda_wir_funcs` and shifts the length — so the actual push index
            // below differs from that name. Rename to the real push index, or two
            // lambdas collide on the same `__lamw{n}` and the table's name-keyed
            // element segment routes both code indices to one body (a
            // `call_indirect` arity/type mismatch at runtime).
            let i = self.lambda_wir_funcs.len();
            func.name = format!("__lamw{i}");
            self.lambda_wir_funcs.push(func);
            self.lambda_wir_index.insert(key, i);
            self.clos_arities.insert(params.len());
            i
        };

        // Closure object: `$mk{ncaps}(code_index, cap0, ...)` — the code index is
        // the i32 header (tag), captures are the i64 env slots.
        let mut args = vec![W::ConstI32(index as i32)];
        args.extend(cap_slots);
        Some(W::Call { func: format!("mk{ncaps}"), args })
    }

    /// (RFC-0062 tier-1) Register the THREADED lifted body of an ELIDED closure and
    /// return its ordered captures — NOTHING is emitted at the creation site (no `mk`
    /// env allocation), because the captures are threaded to each `call $__lamt{i}` from
    /// their existing locals. `None` (→ the caller falls back to the boxed `lower_lambda`)
    /// when the lambda assigns a captured var (can't thread a write-back), when any
    /// capture is REASSIGNED this unit (the interpreter snapshots captures at creation, so
    /// threading a mutated capture would diverge), or when the body doesn't lower. The
    /// caller has already checked `devirt_ok`/`closure_elide_called` (the escape fact).
    fn lower_lambda_threaded(&mut self, params: &[Param], body: &Block) -> Option<ThreadedClosure> {
        if !self.collect_wir {
            return None;
        }
        let scan = scan_lambda(params, body);
        // A closure assigning a captured var needs the write-back the boxed path rejects;
        // let the boxed fallback raise that diagnostic rather than silently threading.
        if !scan.assigns_outer().is_empty() {
            return None;
        }
        let captures: Vec<String> = scan
            .captures()
            .into_iter()
            .filter(|c| self.locals.contains_key(c))
            .collect();
        // Capture-stability: a reassigned capture is unsafe to thread (parity guard).
        if captures.iter().any(|c| self.closure_elide_reassigned.contains(c)) {
            return None;
        }
        let mut cap_info: Vec<CaptureInfo> = Vec::new();
        for c in &captures {
            let kind = self.locals.get(c).copied().unwrap_or(Kind::I32);
            cap_info.push((
                c.clone(),
                self.local_records.get(c).cloned(),
                self.local_list_elem.get(c).cloned(),
                kind,
            ));
        }
        // Idempotent registration: an identical elided lambda shares one `$__lamt{i}`.
        let key = Self::lambda_content_key(params, body);
        let index = if let Some(&i) = self.lambda_threaded_index.get(&key) {
            i
        } else {
            let mut func = self.build_lambda_wir_func(params, body, &cap_info, CapMode::Threaded)?;
            // Rename to the real push index (a nested lambda lowered during the build may
            // have shifted the length), mirroring `lower_lambda`.
            let i = self.lambda_wir_funcs.len();
            func.name = format!("__lamt{i}");
            self.lambda_wir_funcs.push(func);
            self.lambda_threaded_index.insert(key, i);
            i
        };
        Some((index, cap_info.into_iter().map(|(n, _, _, k)| (n, k)).collect()))
    }

    /// Build the lifted `WirFunc` for a lambda: the capture-passing prefix (per
    /// `cap_mode`) then one i64 value param per lambda param, a prologue recovering each
    /// value param from its slot and each capture, the lowered body, and the tail stored
    /// back into the universal i64 result slot. `None` if the body doesn't lower. Saves
    /// the enclosing scope on entry and restores it on exit so the lifted body lowers in
    /// its own local environment.
    ///
    /// (RFC-0062) `cap_mode` selects how captures reach the body:
    /// - `CapMode::Env` (tier-3, the default): an env-pointer first param `$__lamw{i}`;
    ///   the prologue loads each capture from the heap env record.
    /// - `CapMode::Threaded` (tier-1, elided closure): captures are LEADING value params
    ///   `$__lamt{i}`; the prologue recovers each from its i64 param slot — no env, so the
    ///   creating site allocates nothing.
    fn build_lambda_wir_func(
        &mut self,
        params: &[Param],
        body: &Block,
        cap_info: &[CaptureInfo],
        cap_mode: CapMode,
    ) -> Option<witchy_wir::wir::WirFunc> {
        use witchy_wir::wir::{WirExpr as W, WirFunc, WirLocal, WirNode as N, WirTy};
        let index = self.lambda_wir_funcs.len();
        let saved = self.swap_out_scope();
        self.cur_fn_var = false;
        self.cur_fn_var_params = Vec::new();
        // Lambda params: i32 ABI placeholder + record/list types.
        for p in params {
            self.locals.insert(p.name.clone(), Kind::I32);
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
                        if let Type::Tuple(slots) = elem {
                            self.local_list_elem_tuple
                                .insert(p.name.clone(), slots.iter().map(ty_to_valtype).collect());
                        }
                    }
                }
                _ => {}
            }
        }
        for (name, rec, list_elem, kind) in cap_info {
            self.locals.insert(name.clone(), *kind);
            if let Some(r) = rec {
                self.local_records.insert(name.clone(), r.clone());
            }
            if let Some(e) = list_elem {
                self.local_list_elem.insert(name.clone(), e.clone());
            }
        }
        for p in params {
            let k = p.ty.as_ref().map(ty_kind).unwrap_or(Kind::I32);
            self.locals.insert(p.name.clone(), k);
            if let Some(Type::Fn(_, ret)) = &p.ty {
                self.local_fn_ret_kind.insert(p.name.clone(), ty_kind(ret));
            }
        }
        self.infer_locals(body);
        let saved_inplace = std::mem::take(&mut self.inplace_push);
        let saved_own = self.cur_fn_own_param.take();
        self.begin_unit(body);
        self.cur_fn_ret_kind = Kind::I64;
        self.cur_fn_ret_slot = true;
        let saved_apply = self.apply_level;
        let saved_wm = self.wm_level;
        self.apply_level = 0;
        self.wm_level = 0;
        let body_res = self.lower_block(body);
        // The lambda's OWN in-place accumulators (`var acc = []` + a self-push loop
        // inside the lambda body) — snapshot before restoring the outer function's
        // set, so the cap-shadow `${v}__cap` locals below are declared for the
        // lambda's accumulators, not the enclosing function's.
        let lambda_inplace = self.inplace_push.clone();
        let block_kind = self.block_kind(body);
        self.apply_level = saved_apply;
        self.wm_level = saved_wm;
        let fin = self.finish_unit("lambda");
        self.inplace_push = saved_inplace;
        self.cur_fn_own_param = saved_own;

        let func = match (body_res, fin) {
            (Some(seq), Ok(())) => {
                let i32t = || WirTy::Bool;
                // (RFC-0062) Env mode: a closure-pointer first param. Threaded mode: one
                // i64 capture param per capture (no env pointer), leading the value params.
                let mut func_params = match cap_mode {
                    CapMode::Env => vec![WirLocal { name: ENV_PARAM.into(), ty: i32t() }],
                    CapMode::Threaded => cap_info
                        .iter()
                        .map(|(name, _, _, _)| WirLocal { name: format!("__cap_{name}"), ty: WirTy::Int })
                        .collect(),
                };
                for p in params {
                    func_params.push(WirLocal { name: format!("__lp_{}", p.name), ty: WirTy::Int });
                }
                let mut locals: Vec<WirLocal> = Vec::new();
                for p in params {
                    let k = self.locals.get(&p.name).copied().unwrap_or(Kind::I32);
                    locals.push(WirLocal { name: p.name.clone(), ty: Self::wir_ty_for_kind(k) });
                }
                for (name, _, _, kind) in cap_info {
                    locals.push(WirLocal { name: name.clone(), ty: Self::wir_ty_for_kind(*kind) });
                }
                let mut lets = Vec::new();
                collect_let_names(body, &mut lets);
                lets.sort();
                lets.dedup();
                for name in &lets {
                    let k = self.locals.get(name).copied().unwrap_or(Kind::I32);
                    locals.push(WirLocal { name: name.clone(), ty: Self::wir_ty_for_kind(k) });
                }
                let mut cap_vars: Vec<&String> = lambda_inplace.iter().collect();
                cap_vars.sort();
                for v in cap_vars {
                    locals.push(WirLocal { name: format!("{v}__cap"), ty: i32t() });
                }
                // (RFC-0033 R2) field-buffer capacity tokens for in-place field-path pushes.
                let mut field_caps: Vec<&String> = self.field_caps.iter().collect();
                field_caps.sort();
                for fc in field_caps {
                    locals.push(WirLocal { name: fc.clone(), ty: i32t() });
                }
                locals.push(WirLocal { name: "__witchy_owncap".into(), ty: i32t() });
                locals.push(WirLocal { name: TUPLE_TMP.into(), ty: i32t() });
                locals.push(WirLocal { name: TRY_TMP.into(), ty: i32t() });
                locals.push(WirLocal { name: MATCH_TMP.into(), ty: WirTy::Int });
                locals.push(WirLocal { name: MATCH_RES.into(), ty: WirTy::Int });
                for i in 0..SCRUT_POOL {
                    locals.push(WirLocal { name: format!("__witchy_scrut_save_{i}"), ty: WirTy::Int });
                }
                locals.push(WirLocal { name: SECRET_TMP.into(), ty: i32t() });
                // Scratch slots for the inlined in-place set_at/push fast path (a
                // self-assign accumulator can live inside a lifted lambda body too).
                locals.push(WirLocal { name: "__witchy_set_idx".into(), ty: i32t() });
                locals.push(WirLocal { name: "__witchy_set_val".into(), ty: WirTy::Int });
                locals.push(WirLocal { name: "__rc_new".into(), ty: WirTy::Int });
                for i in 0..WM_POOL {
                    locals.push(WirLocal { name: format!("__witchy_wm_{i}"), ty: i32t() });
                }
                for i in 0..APPLY_POOL {
                    locals.push(WirLocal { name: format!("__witchy_call_{i}"), ty: i32t() });
                }
                for i in 0..REUSE_POOL {
                    locals.push(WirLocal { name: format!("__witchy_reuse_{i}"), ty: WirTy::Int });
                }
                // Prologue: recover each value param from its i64 slot, then each capture
                // — from the env record (`CapMode::Env`, slot j at offset 4 + 8*j) or from
                // its threaded i64 param slot (`CapMode::Threaded`, no env load, RFC-0062).
                let mut nodes: witchy_wir::wir::WirSeq = Vec::new();
                for p in params {
                    let k = self.locals.get(&p.name).copied().unwrap_or(Kind::I32);
                    nodes.push(N::SetLocal {
                        local: p.name.clone(),
                        value: W::FromSlot(Box::new(W::GetLocal(format!("__lp_{}", p.name))), Self::wir_kind(k)),
                    });
                }
                for (j, (name, _, _, kind)) in cap_info.iter().enumerate() {
                    let cap_slot = match cap_mode {
                        CapMode::Env => {
                            let off = (4 + 8 * j) as i32;
                            let addr = W::Binary {
                                op: witchy_wir::wir::BinOp::Add,
                                kind: witchy_wir::wir::Kind::I32,
                                lhs: Box::new(W::GetLocal(ENV_PARAM.into())),
                                rhs: Box::new(W::ConstI32(off)),
                            };
                            W::Load { ptr: Box::new(addr), kind: witchy_wir::wir::Kind::I64, offset: 0 }
                        }
                        CapMode::Threaded => W::GetLocal(format!("__cap_{name}")),
                    };
                    nodes.push(N::SetLocal {
                        local: name.clone(),
                        value: W::FromSlot(Box::new(cap_slot), Self::wir_kind(*kind)),
                    });
                }
                // Body, with the tail value stored into the i64 result slot.
                let mut seq = seq;
                if let Some(N::Push(v)) = seq.pop() {
                    seq.push(N::Push(W::ToSlot(Box::new(v), Self::wir_kind(block_kind))));
                }
                nodes.extend(seq);
                let name = match cap_mode {
                    CapMode::Env => format!("__lamw{index}"),
                    CapMode::Threaded => format!("__lamt{index}"),
                };
                Some(WirFunc {
                    name,
                    params: func_params,
                    ret: vec![WirTy::Int],
                    locals,
                    body: nodes,
                    raw_body: None,
                })
            }
            _ => None,
        };
        self.restore_scope(saved);
        func
    }

    /// Take the current function's local-type tables out (leaving them empty for
    /// a lambda body to populate), returning them for later restoration.
    fn swap_out_scope(&mut self) -> SavedScope {
        SavedScope {
            locals: std::mem::take(&mut self.locals),
            records: std::mem::take(&mut self.local_records),
            list_elem: std::mem::take(&mut self.local_list_elem),
            payload: std::mem::take(&mut self.local_payload_records),
            val_types: std::mem::take(&mut self.local_val_types),
            list_elem_vt: std::mem::take(&mut self.local_list_elem_valtype),
            list_elem_tuple: std::mem::take(&mut self.local_list_elem_tuple),
            tuple_slots: std::mem::take(&mut self.local_tuple_slots),
            shape: std::mem::take(&mut self.local_shape),
            payload_vt: std::mem::take(&mut self.local_payload_valtype),
            fn_ret_kind: std::mem::take(&mut self.local_fn_ret_kind),
            ret: self.cur_fn_ret_kind,
            ret_slot: self.cur_fn_ret_slot,
            var: self.cur_fn_var,
            var_params: std::mem::take(&mut self.cur_fn_var_params),
            sroa_candidates: std::mem::take(&mut self.sroa_candidates),
            sroa_active: std::mem::take(&mut self.sroa_active),
            view_candidates: std::mem::take(&mut self.view_candidates),
            view_active: std::mem::take(&mut self.view_active),
            packed_candidates: std::mem::take(&mut self.packed_candidates),
            packed_active: std::mem::take(&mut self.packed_active),
            reuse_vars: std::mem::take(&mut self.reuse_vars),
            rc_floor_vars: std::mem::take(&mut self.rc_floor_vars),
            rc_owned_bindings: std::mem::take(&mut self.rc_owned_bindings),
            devirt_ok: std::mem::take(&mut self.devirt_ok),
            devirt_index: std::mem::take(&mut self.devirt_index),
            thread_index: std::mem::take(&mut self.thread_index),
            closure_elide_called: std::mem::take(&mut self.closure_elide_called),
            closure_elide_reassigned: std::mem::take(&mut self.closure_elide_reassigned),
            elide_index_list: std::mem::take(&mut self.elide_index_list),
        }
    }

    /// Restore a scope previously taken by `swap_out_scope`.
    fn restore_scope(&mut self, s: SavedScope) {
        self.locals = s.locals;
        self.local_records = s.records;
        self.local_list_elem = s.list_elem;
        self.local_payload_records = s.payload;
        self.local_val_types = s.val_types;
        self.local_list_elem_valtype = s.list_elem_vt;
        self.local_list_elem_tuple = s.list_elem_tuple;
        self.local_tuple_slots = s.tuple_slots;
        self.local_shape = s.shape;
        self.local_payload_valtype = s.payload_vt;
        self.local_fn_ret_kind = s.fn_ret_kind;
        self.cur_fn_ret_kind = s.ret;
        self.cur_fn_ret_slot = s.ret_slot;
        self.cur_fn_var = s.var;
        self.cur_fn_var_params = s.var_params;
        self.sroa_candidates = s.sroa_candidates;
        self.sroa_active = s.sroa_active;
        self.view_candidates = s.view_candidates;
        self.view_active = s.view_active;
        self.packed_candidates = s.packed_candidates;
        self.packed_active = s.packed_active;
        self.reuse_vars = s.reuse_vars;
        self.rc_floor_vars = s.rc_floor_vars;
        self.rc_owned_bindings = s.rc_owned_bindings;
        self.devirt_ok = s.devirt_ok;
        self.devirt_index = s.devirt_index;
        self.thread_index = s.thread_index;
        self.closure_elide_called = s.closure_elide_called;
        self.closure_elide_reassigned = s.closure_elide_reassigned;
        self.elide_index_list = s.elide_index_list;
    }

    /// The `$key_eq` comparison mode for a Dict key expression: 0 for Int/Bool
    /// (i64 bit equality), 1 for String (`$str_eq`), 2 for Float (`f64.eq` on the
    /// reinterpreted slot — matches the interpreter's `==`, so -0.0 == 0.0 and
    /// NaN != NaN). Other key types are rejected.
    fn dict_key_mode(&self, key: &Expr) -> Result<u32, CodegenError> {
        match self.val_type_of(key) {
            ValType::Int | ValType::Bool => Ok(0),
            ValType::Str => Ok(1),
            ValType::Float => Ok(2),
            ValType::Other => cerr(
                "could not determine the Dict key type for WASM; use Int, Float, or String keys (annotate if needed)",
            ),
        }
    }

    /// `dict_key_mode` for the WIR path: an undetermined key type is a HARD
    /// rejection (a dict needs a comparable key), so record it as a `reject_reason`
    /// — `compile_function` turns that into a diagnostic `Err` rather than letting
    /// the function silently bail as "unsupported".
    fn dict_key_mode_wir(&mut self, key: &Expr) -> Option<u32> {
        match self.dict_key_mode(key) {
            Ok(m) => Some(m),
            Err(e) => {
                self.reject_reason.get_or_insert(e);
                None
            }
        }
    }

    /// The structural-equality shape of an expression, where codegen can resolve
    /// it. `None` means the shape is unknown (then compound `==` errors loudly
    /// rather than comparing pointers). Lists (any depth) come from the nesting
    /// tracker, tuples from literals or tracked tuple locals, records from
    /// `record_type_of`.
    /// The WIR form of [`loop_watermark`]: returns the `(capture, reset)` pair as
    /// WIR nodes — `capture` saves `$heap` into a pool slot before the loop, and
    /// `reset` restores it at the end of each iteration so per-iteration arena
    /// garbage is reclaimed. `None` when the loop body isn't arena-resettable or
    /// the pool is exhausted (then the loop simply lowers without the reset, which
    /// is still correct — just less memory-efficient). Bumps `wm_level`; the
    /// caller decrements it once the body is lowered.
    fn loop_watermark_wir(&mut self, body: &Block) -> Option<(witchy_wir::wir::WirNode, witchy_wir::wir::WirNode)> {
        // Gated on the `region` optimization (RFC-0030): `WITCHY_OPT=-region` (or
        // `none`) drops the per-iteration reset so the loop's arena garbage leaks —
        // correct, just unbounded — which is exactly the regression the soak test
        // and the differential sweep guard against.
        if force_copy_mode()
            || !witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::Region)
            || self.wm_level >= WM_POOL
            || !self.loop_arena_resettable(body)
        {
            return None;
        }
        let wm = format!("__witchy_wm_{}", self.wm_level);
        self.wm_level += 1;
        self.uses_wm = true;
        let capture = witchy_wir::wir::WirNode::SetLocal {
            local: wm.clone(),
            value: witchy_wir::wir::WirExpr::GetGlobal("heap".into()),
        };
        let reset = witchy_wir::wir::WirNode::SetGlobal {
            global: "heap".into(),
            value: witchy_wir::wir::WirExpr::GetLocal(wm),
        };
        Some((capture, reset))
    }

    fn loop_arena_resettable(&self, body: &Block) -> bool {
        let mut inner_lets = Vec::new();
        collect_let_names(body, &mut inner_lets);
        let inner: HashSet<String> = inner_lets.into_iter().collect();
        let mut ok = true;
        self.scan_escapes_block(body, &inner, &mut ok);
        ok
    }

    fn scan_escapes_block(&self, b: &Block, inner: &HashSet<String>, ok: &mut bool) {
        for stmt in &b.stmts {
            match stmt {
                Stmt::Assign { name, value } => {
                    if !inner.contains(name) {
                        let scalar_kind = matches!(
                            self.locals.get(name),
                            Some(Kind::I64) | Some(Kind::F64)
                        );
                        let scalar_type = matches!(
                            self.local_val_types.get(name),
                            Some(ValType::Int) | Some(ValType::Bool) | Some(ValType::Float)
                        );
                        if !scalar_kind && !scalar_type {
                            *ok = false;
                        }
                    }
                    self.scan_escapes_expr(value, inner, ok);
                }
                Stmt::Let { value, .. } | Stmt::LetPattern { value, .. } => {
                    self.scan_escapes_expr(value, inner, ok)
                }
                Stmt::Yield(_) => *ok = false,
                Stmt::Return(Some(e)) | Stmt::Expr(e) => self.scan_escapes_expr(e, inner, ok),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn scan_escapes_expr(&self, e: &Expr, inner: &HashSet<String>, ok: &mut bool) {
        match e {
            Expr::If { cond, then_block, else_block } => {
                self.scan_escapes_expr(cond, inner, ok);
                self.scan_escapes_block(then_block, inner, ok);
                if let Some(b) = else_block {
                    self.scan_escapes_block(b, inner, ok);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.scan_escapes_expr(scrutinee, inner, ok);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.scan_escapes_expr(g, inner, ok);
                    }
                    self.scan_escapes_expr(&arm.body, inner, ok);
                }
            }
            Expr::While { cond, body } => {
                self.scan_escapes_expr(cond, inner, ok);
                self.scan_escapes_block(body, inner, ok);
            }
            Expr::For { iter, body, .. } => {
                self.scan_escapes_expr(iter, inner, ok);
                self.scan_escapes_block(body, inner, ok);
            }
            Expr::Lambda { body, .. } => self.scan_escapes_block(body, inner, ok),
            Expr::Block(b) => self.scan_escapes_block(b, inner, ok),
            Expr::Binary { lhs, rhs, .. } => {
                self.scan_escapes_expr(lhs, inner, ok);
                self.scan_escapes_expr(rhs, inner, ok);
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => self.scan_escapes_expr(expr, inner, ok),
            Expr::Call { args, .. }
            | Expr::Ctor { args, .. }
            | Expr::List(args)
            | Expr::Tuple(args) => {
                for a in args {
                    self.scan_escapes_expr(a, inner, ok);
                }
            }
            Expr::Apply { func, args } => {
                self.scan_escapes_expr(func, inner, ok);
                for a in args {
                    self.scan_escapes_expr(a, inner, ok);
                }
            }
            Expr::RecordUpdate { base, fields } => {
                self.scan_escapes_expr(base, inner, ok);
                for (_, v) in fields {
                    self.scan_escapes_expr(v, inner, ok);
                }
            }
            Expr::Range { lo, hi, .. } => {
                self.scan_escapes_expr(lo, inner, ok);
                self.scan_escapes_expr(hi, inner, ok);
            }
            Expr::Index { .. }
            | Expr::WhileLet { .. }
            | Expr::MethodCall { .. }
            | Expr::Record { .. }
            | Expr::LabeledCall { .. }
            | Expr::Var(_)
            | Expr::Int(_)
            | Expr::Duration(_)
            | Expr::Float(_)
            | Expr::Str(_)
            | Expr::TaggedLit { .. }
            | Expr::Bool(_) => {}
        }
    }


    fn eq_shape_of(&self, e: &Expr) -> Option<EqShape> {
        // A `let`-bound compound whose shape was captured at binding time (the
        // authoritative resolution of its RHS) — resolves slots-of-compounds the
        // scalar slot tables miss.
        if let Expr::Var(v) = e {
            if let Some(s) = self.local_shape.get(v) {
                return Some(s.clone());
            }
        }
        // A record field access (`p.tags`): resolve from the field's declared
        // type, so `to_string`/`==` on a compound field works.
        if let Expr::Field { base, field } = e {
            if let Some(shape) = self.field_type_of(base, field).and_then(|t| self.eq_shape_of_type(&t)) {
                return Some(shape);
            }
        }
        // A list literal: the element shape comes from the first element (which
        // recurses, so nested lists / lists of tuples or records resolve). An
        // empty literal never compares elements, so any element shape is safe.
        if let Expr::List(items) = e {
            let elem = match items.first() {
                // An empty literal never accesses an element (eq compares length
                // first; to_string renders "[]"), so any scalar default is safe.
                Some(first) => self.eq_operand_shape(first)?,
                None => EqShape::Int,
            };
            return Some(EqShape::List(Box::new(elem)));
        }
        // A list-typed value (variable / `at(...)`): scalar or tuple bottoms come
        // from the nesting tracker; a list of records from `local_list_elem`.
        if let Some((depth, bottom)) = self.list_nesting(e) {
            let mut shape = match bottom {
                NestBottom::Scalar(vt) => EqShape::scalar(vt)?,
                NestBottom::Tuple(vts) => EqShape::Tuple(
                    vts.into_iter().map(EqShape::scalar).collect::<Option<Vec<_>>>()?,
                ),
            };
            for _ in 0..depth {
                shape = EqShape::List(Box::new(shape));
            }
            return Some(shape);
        }
        if let Expr::Var(v) = e {
            if let Some(rec) = self.local_list_elem.get(v) {
                return Some(EqShape::List(Box::new(EqShape::Record(rec.clone()))));
            }
        }
        if let Expr::Tuple(items) = e {
            return items
                .iter()
                .map(|x| self.eq_operand_shape(x))
                .collect::<Option<Vec<_>>>()
                .map(EqShape::Tuple);
        }
        if let Expr::Var(v) = e {
            if let Some(slots) = self.local_tuple_slots.get(v) {
                return slots
                    .iter()
                    .copied()
                    .map(EqShape::scalar)
                    .collect::<Option<Vec<_>>>()
                    .map(EqShape::Tuple);
            }
        }
        // A Dict with tracked key/value scalar types (a let-bound dict, or an
        // `insert(...)` chain) compares entry-wise in insertion order.
        if let Expr::Var(v) = e {
            if let (Some(k), Some(val)) = (
                self.local_dict_key_valtype.get(v).copied(),
                self.local_dict_value_valtype.get(v).copied(),
            ) {
                return Some(EqShape::Dict(
                    Box::new(EqShape::scalar(k)?),
                    Box::new(EqShape::scalar(val)?),
                ));
            }
        }
        if let Expr::Call { name, args } = e {
            if name == "dict.insert" && args.len() == 3 {
                return Some(EqShape::Dict(
                    Box::new(EqShape::scalar(self.val_type_of(&args[1]))?),
                    Box::new(self.eq_operand_shape(&args[2])?),
                ));
            }
            // A call to a function with a declared compound return type
            // (`-> Result(Int, String)`) resolves from that declaration.
            if let Some(shape) = self.fn_ret_ty.get(name).cloned().and_then(|t| {
                let s = self.eq_shape_of_type(&t)?;
                s.is_compound().then_some(s)
            }) {
                return Some(shape);
            }
        }
        if let Some(rec) = self.record_type_of(e) {
            return Some(EqShape::Record(rec));
        }
        // A constructor of a sum type (`Some(..)`, `Red`, ...). A monomorphic
        // type resolves by name; a SINGLE-parameter generic (Option-like) is
        // instantiated from this constructor's argument — sound for both
        // operands, because the type checker guarantees `==` operands share a
        // type. A nullary constructor of a generic type (None) pins nothing,
        // and any placeholder is sound at this site: the variant carrying the
        // type variable can only match at runtime when this operand is NOT that
        // variant, so its field comparison never executes here. Multi-parameter
        // generics (Result) fall back to the by-name shape, whose unresolvable
        // fields are a loud error when the helper is generated.
        if let Expr::Ctor { name, args } = e {
            if let Some(tyname) = self.ctor_type_name.get(name).cloned() {
                let variants = self.adt_variants.get(&tyname).cloned()?;
                let mut vars: Vec<String> = Vec::new();
                for fs in &variants {
                    for f in fs {
                        collect_type_vars(f, &mut vars);
                    }
                }
                if vars.is_empty() {
                    return Some(EqShape::Adt(tyname));
                }
                // Pin every variable this constructor's own arguments determine
                // (`Ok(3)` pins Ok's `a` = Int); variables only OTHER variants
                // carry (`Err`'s `e`) take a placeholder. Sound for both
                // operands: the type checker guarantees `==` operands share a
                // type, and a placeholder variant can only both-match at runtime
                // when this operand IS that variant — which it is not, so the
                // placeholder field comparison never executes from this site.
                let (tag, _) = *self.ctors.get(name)?;
                let my_fields = variants.get(tag as usize)?;
                let mut subst: HashMap<String, EqShape> = HashMap::new();
                let mut my_vars: Vec<String> = Vec::new();
                for (i, f) in my_fields.iter().enumerate() {
                    let before = my_vars.len();
                    collect_type_vars(f, &mut my_vars);
                    if my_vars.len() > before {
                        let arg_shape = self.eq_operand_shape(args.get(i)?)?;
                        unify_type_vars(f, &arg_shape, &mut subst);
                    }
                }
                // Every variable in THIS variant's fields must be pinned by its
                // arguments (a nested `List(a)` pins through the list shape);
                // otherwise this site can't vouch for its own payload — fall
                // back to the by-name shape (loud later), never a placeholder.
                if my_vars.iter().any(|v| !subst.contains_key(v)) {
                    return Some(EqShape::Adt(tyname));
                }
                for v in &vars {
                    subst.entry(v.clone()).or_insert(EqShape::Int);
                }
                if self.adt_is_self_recursive(&tyname) {
                    let args: Vec<EqShape> =
                        vars.iter().filter_map(|v| subst.get(v).cloned()).collect();
                    return Some(EqShape::AdtRec(tyname, args));
                }
                let inst: Option<Vec<Vec<EqShape>>> = variants
                    .iter()
                    .map(|fs| {
                        fs.iter().map(|f| self.eq_shape_of_type_with(f, &subst)).collect()
                    })
                    .collect();
                return Some(match inst {
                    Some(inst) => EqShape::AdtInst(tyname, inst),
                    None => EqShape::Adt(tyname),
                });
            }
        }
        None
    }

    /// The shape of a value used as a tuple element / general operand: a compound
    /// shape if resolvable, else its scalar value type.
    fn eq_operand_shape(&self, e: &Expr) -> Option<EqShape> {
        self.eq_shape_of(e)
            .or_else(|| EqShape::scalar(self.val_type_of(e)))
            .or_else(|| self.table_shape_of(e))
    }

    /// The structural shape of an expression per typeck's resolved type —
    /// the fallback that makes `${collect(...)}`, ADT-payload bindings, and
    /// every other "the local maps lost it" case render and compare.
    fn table_shape_of(&self, e: &Expr) -> Option<EqShape> {
        let t = witchy_types::typeck::ty_to_ast(self.type_table.type_of(e)?)?;
        self.eq_shape_of_type(&t)
    }

    /// Whether an expression is statically known to be a `Dict` (a tracked dict
    /// local, or a dict-producing builtin call), so `==` can reject it instead of
    /// comparing pointers.
    fn is_dict_operand(&self, e: &Expr) -> bool {
        match e {
            Expr::Var(v) => {
                self.local_dict_key_valtype.contains_key(v)
                    || self.local_dict_value_valtype.contains_key(v)
            }
            Expr::Call { name, .. } => {
                matches!(name.as_str(), "dict.new" | "dict.insert" | "dict.remove" | "dict.update")
            }
            _ => false,
        }
    }

    /// The structural-equality shape of a declared type. Scalars, strings, lists,
    /// tuples, and (nested) record types resolve; `Dict`, function types, and
    /// generic type variables do not (they yield `None`, a loud error at the use
    /// site rather than a silent pointer compare).
    fn eq_shape_of_type(&self, ty: &Type) -> Option<EqShape> {
        self.eq_shape_of_type_with(ty, &HashMap::new())
    }

    /// Whether the ADT's variants reference the ADT itself (directly, or nested
    /// inside a List/Tuple/argument position) — `Push(a, Stack(a))` is.
    fn adt_is_self_recursive(&self, name: &str) -> bool {
        fn mentions(ty: &Type, name: &str) -> bool {
            match ty {
                Type::Qualified(_, inner) => mentions(inner, name),
                Type::Named(n, args) => n == name || args.iter().any(|a| mentions(a, name)),
                Type::Tuple(ts) => ts.iter().any(|t| mentions(t, name)),
                Type::Fn(params, ret) => {
                    params.iter().any(|p| mentions(p, name)) || mentions(ret, name)
                }
            }
        }
        self.adt_variants
            .get(name)
            .is_some_and(|vs| vs.iter().any(|fs| fs.iter().any(|f| mentions(f, name))))
    }

    /// `eq_shape_of_type` under a type-variable substitution. A generic ADT
    /// applied to concrete arguments (`Result(Int, String)`) instantiates to an
    /// `AdtInst` by substituting its parameters (first-appearance order across
    /// the variants' fields — the SAME rule the type checker uses) with the
    /// arguments' shapes; an unresolvable argument falls back to the by-name
    /// `Adt` shape (loud later if a type-variable field is actually compared).
    fn eq_shape_of_type_with(
        &self,
        ty: &Type,
        subst: &HashMap<String, EqShape>,
    ) -> Option<EqShape> {
        self.eq_shape_of_type_rec(ty, subst, &mut Vec::new())
    }

    fn eq_shape_of_type_rec(
        &self,
        ty: &Type,
        subst: &HashMap<String, EqShape>,
        visiting: &mut Vec<String>,
    ) -> Option<EqShape> {
        match ty {
            Type::Qualified(_, inner) => self.eq_shape_of_type_rec(inner, subst, visiting),
            Type::Named(n, args) => match n.as_str() {
                "Int" | "Duration" => Some(EqShape::Int),
                "Bool" => Some(EqShape::Bool),
                "Float" => Some(EqShape::Float),
                "String" => Some(EqShape::Str),
                "List" => args.first().and_then(|inner| {
                    self.eq_shape_of_type_rec(inner, subst, visiting)
                        .map(|s| EqShape::List(Box::new(s)))
                }),
                "Dict" => match args.as_slice() {
                    [k, v] => Some(EqShape::Dict(
                        Box::new(self.eq_shape_of_type_rec(k, subst, visiting)?),
                        Box::new(self.eq_shape_of_type_rec(v, subst, visiting)?),
                    )),
                    _ => None,
                },
                t if self.record_fields.contains_key(t) => Some(EqShape::Record(t.to_string())),
                t if self.adt_variants.contains_key(t) => {
                    if args.is_empty() || visiting.iter().any(|v| v == t) {
                        return Some(EqShape::Adt(t.to_string()));
                    }
                    let variants = self.adt_variants.get(t)?;
                    let mut params: Vec<String> = Vec::new();
                    for fields in variants {
                        for f in fields {
                            collect_type_vars(f, &mut params);
                        }
                    }
                    let mut inner: HashMap<String, EqShape> = HashMap::new();
                    let mut arg_shapes: Vec<EqShape> = Vec::new();
                    for (pn, arg) in params.iter().zip(args) {
                        match self.eq_shape_of_type_rec(arg, subst, visiting) {
                            Some(s) => {
                                arg_shapes.push(s.clone());
                                inner.insert(pn.clone(), s);
                            }
                            None => return Some(EqShape::Adt(t.to_string())),
                        }
                    }
                    // A self-RECURSIVE generic ADT (`Push(a, Stack(a))`) has no
                    // finite expanded shape: identify it by its arguments. The
                    // helper expands one level lazily; the self-reference calls
                    // the same helper.
                    if self.adt_is_self_recursive(t) {
                        return Some(EqShape::AdtRec(t.to_string(), arg_shapes));
                    }
                    visiting.push(t.to_string());
                    let inst: Option<Vec<Vec<EqShape>>> = variants
                        .iter()
                        .map(|fs| {
                            fs.iter()
                                .map(|f| self.eq_shape_of_type_rec(f, &inner, visiting))
                                .collect()
                        })
                        .collect();
                    visiting.pop();
                    match inst {
                        Some(inst) => Some(EqShape::AdtInst(t.to_string(), inst)),
                        None => Some(EqShape::Adt(t.to_string())),
                    }
                }
                v if subst.contains_key(v) => subst.get(v).cloned(),
                _ => None,
            },
            Type::Tuple(items) => items
                .iter()
                .map(|t| self.eq_shape_of_type_rec(t, subst, visiting))
                .collect::<Option<Vec<_>>>()
                .map(EqShape::Tuple),
            Type::Fn(..) => None,
        }
    }


    /// Lower a list of argument expressions, threading `None` if any isn't lowerable.
    fn lower_args(&mut self, args: &[&Expr]) -> Option<Vec<witchy_wir::wir::WirExpr>> {
        let mut v = Vec::with_capacity(args.len());
        for a in args {
            v.push(self.lower_expr(a)?);
        }
        Some(v)
    }

    /// Lower the simple builtin/native `Call` arms to a `WirExpr::Call` (each
    /// `$helper` is a guest module function; the actual host import is `_host`-
    /// suffixed and called from inside the helper). The `uses_*` side-effect flags
    /// are set exactly as the legacy arms do. Returns `None` for unconverted arms.
    /// `vm.par_map` monomorphized over scalar (i64-representable) type arguments —
    /// `vm.par_map__Int__Int`, `__Bool__Bool`, etc. Only these take the native
    /// worker-VM path; the parallel marshaling moves elements as flat i64s, which is
    /// unsound for pointer types (String/List/records) whose data lives in the parent
    /// VM's memory. Such a call falls through to the sequential `list.map` body.
    fn is_scalar_par_map(name: &str) -> bool {
        let Some(suffix) = name.strip_prefix("vm.par_map__") else {
            return false;
        };
        let scalar = |t: &str| matches!(t, "Int" | "Bool" | "Float" | "Duration");
        let parts: Vec<&str> = suffix.split("__").collect();
        parts.len() == 2 && parts.iter().all(|t| scalar(t))
    }

    /// `vm.par_map` monomorphized over a flat BUFFER element type — `String` or `Bytes`
    /// (`vm.par_map__String__String` / `__Bytes__Bytes`). Both are `[len][bytes]` with no
    /// internal pointers, and `List(String)`/`List(Bytes)` share an identical layout, so a
    /// single runtime path (`vm_par_map_bytes`, raw byte copy in/out) serves both — a
    /// witchy `String` is just valid-UTF-8 `Bytes`.
    fn is_buf_par_map(name: &str) -> bool {
        let Some(suffix) = name.strip_prefix("vm.par_map__") else {
            return false;
        };
        let parts: Vec<&str> = suffix.split("__").collect();
        parts.len() == 2 && parts.iter().all(|t| *t == "String" || *t == "Bytes")
    }


}

/// If `ty` is a bare type-parameter (lowercase, argument-less name), return it.
/// Pin type variables in `ty` by structurally matching it against a resolved
/// shape: a bare var takes the whole shape, `List(a)` against a list shape
/// pins `a` to the element, tuples pin pairwise. First pin wins.
fn unify_type_vars(ty: &Type, shape: &EqShape, subst: &mut HashMap<String, EqShape>) {
    if let Some(v) = bare_type_var(ty) {
        subst.entry(v).or_insert_with(|| shape.clone());
        return;
    }
    match (ty, shape) {
        (Type::Named(n, args), EqShape::List(inner)) if n == "List" => {
            if let Some(a) = args.first() {
                unify_type_vars(a, inner, subst);
            }
        }
        (Type::Tuple(ts), EqShape::Tuple(ss)) => {
            for (t, s) in ts.iter().zip(ss) {
                unify_type_vars(t, s, subst);
            }
        }
        _ => {}
    }
}

fn bare_type_var(ty: &Type) -> Option<String> {
    match ty {
        Type::Named(n, args)
            if args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()) && !n.contains('.') =>
        {
            Some(n.clone())
        }
        _ => None,
    }
}

/// If `ret` is `Option(a)` or `Result(a, _)` whose payload `a` is a bare
/// type-parameter, return that parameter's name — used to spot the
/// `fn(List(a),..) -> Option(a)` shape.
fn payload_type_var(ret: &Option<Type>) -> Option<String> {
    if let Some(Type::Named(n, args)) = ret {
        if (n == "Option" || n == "Result") && !args.is_empty() {
            return bare_type_var(&args[0]);
        }
    }
    None
}

/// If `ret` is `List(a)` whose element `a` is a bare type-parameter, return it —
/// used to spot the `fn(List(a),..) -> List(a)` shape.
fn list_elem_type_var(ret: &Option<Type>) -> Option<String> {
    if let Some(Type::Named(n, args)) = ret {
        if n == "List" && args.len() == 1 {
            return bare_type_var(&args[0]);
        }
    }
    None
}

/// The index of the first parameter typed `List(tv)` for the given type-var `tv`.
fn list_param_of_var(params: &[witchy_syntax::ast::Param], tv: &str) -> Option<usize> {
    params.iter().position(|p| {
        matches!(&p.ty, Some(Type::Named(n, targs))
            if n == "List" && targs.len() == 1 && bare_type_var(&targs[0]).as_deref() == Some(tv))
    })
}

/// The index of the first parameter typed `fn(..) -> tv` (a function returning
/// the given type-var `tv`).
fn fn_param_returning_var(params: &[witchy_syntax::ast::Param], tv: &str) -> Option<usize> {
    params.iter().position(|p| {
        matches!(&p.ty, Some(Type::Fn(_, ret)) if bare_type_var(ret).as_deref() == Some(tv))
    })
}

/// Compile a module's functions to WAT. Requires a `main` returning Int or Nil;
/// `main` may take a single capability parameter.
/// Collect every name that could refer to a function — call targets and bare
/// identifiers (first-class function values) — used for reachability/DCE. Over-
/// approximates (also picks up locals), which is safe: non-function names just
/// don't match any function and are ignored.
/// How many nested loops can carry an arena watermark (deeper loops simply
/// skip the reset — a safe fallback).
const WM_POOL: usize = 4;

/// Variables eligible for IN-PLACE push (`xs = push(xs, e)` appends into
/// exclusively-owned slack instead of copying the list): every appearance of
/// the variable in the body must be a self-push reassignment, a read through
/// `at`/`length`, a `for` iteration, or a plain reassignment (which resets
/// the tracked capacity). Anything else — passed to a function, stored in a
/// structure, returned, captured by a lambda, compared — can alias the
/// buffer, so the variable keeps the copying push. This is the linear-update
/// optimization: value semantics are preserved because no one else can
/// observe the mutated block.
/// Does the type mention a bare lowercase type variable anywhere?
fn type_has_var(t: &Type) -> bool {
    match t {
        Type::Qualified(_, inner) => type_has_var(inner),
        Type::Named(n, args) => {
            (args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()) && !n.contains('.'))
                || args.iter().any(type_has_var)
        }
        Type::Tuple(ts) => ts.iter().any(type_has_var),
        Type::Fn(ps, r) => ps.iter().any(type_has_var) || type_has_var(r),
    }
}

/// In-place machinery (linear update and loop watermark resets) is gated on the
/// `inplace` optimization of the single `WITCHY_OPT` lever (RFC-0030). With it
/// off — `WITCHY_OPT=-inplace` or `WITCHY_OPT=none` — the copying paths ARE the
/// semantics, so diffing outputs against an optimized build is a soundness check
/// on the uniqueness analysis.
fn force_copy_mode() -> bool {
    !witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::InPlace)
}

/// Thread-local override of the forced-copy setting so in-process differential
/// tests can compile both ways without racing the process environment. Delegates
/// to the `WITCHY_OPT` lever: `Some(true)` drops `inplace` from the production
/// default, `Some(false)` restores it, `None` falls back to the environment.
///
/// Always compiled (not `#[cfg(test)]`): the `witchy` binary's own tests reach
/// it cross-crate through the `witchy` library, where a `cfg(test)` item would
/// not exist. Inert in production — with no caller the override stays `None`.
#[doc(hidden)]
pub fn set_force_copy_for_tests(v: Option<bool>) {
    use witchy_syntax::opt::{Opt, OptSet};
    witchy_syntax::opt::set_for_tests(v.map(|force_copy| {
        if force_copy {
            OptSet::default_set().without(Opt::InPlace)
        } else {
            OptSet::default_set()
        }
    }));
}


/// (RFC-0034 L3) Names eligible for closure devirtualization in a unit: a name
/// introduced by EXACTLY ONE `let` and never otherwise re-introduced or reassigned,
/// so every call through it provably reaches the same value. A devirt site only ever
/// fires for a name whose single `let` bound a lambda (the binding-recorder checks
/// that), so this need not inspect the RHS — it only has to guarantee the name is not
/// MUTABLE OR SHADOWED: any reassignment (`f = …`), a second `let`, a tuple/pattern/
/// for-var/lambda-param binding of the same name, all disqualify it. Conservative by
/// construction (default ineligible); the walk is exhaustive (no wildcard arm) so a
/// future `Expr`/`Stmt` variant that could rebind a name is a compile error, not a
/// silent unsound devirt.
#[derive(Default)]
struct DevirtScan {
    /// `let name = …` occurrences, by name (a count, so a second `let` excludes it).
    let_bind: HashMap<String, u32>,
    /// Names introduced by any NON-`let` binder (tuple destructure, `for` var, lambda
    /// param, match/while-let pattern) — a single one disqualifies the name.
    other_bind: HashSet<String>,
    /// Names reassigned via `name = …` — a single one disqualifies the name.
    reassigned: HashSet<String>,
}

impl DevirtScan {
    fn walk_block(&mut self, b: &Block) {
        for stmt in &b.stmts {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    *self.let_bind.entry(name.clone()).or_insert(0) += 1;
                    self.walk_expr(value);
                }
                Stmt::Assign { name, value } => {
                    self.reassigned.insert(name.clone());
                    self.walk_expr(value);
                }
                Stmt::LetPattern { pattern, value } => {
                    let mut names = Vec::new();
                    witchy_syntax::ast::pattern_binds(pattern, &mut names);
                    for n in names {
                        self.other_bind.insert(n);
                    }
                    self.walk_expr(value);
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => self.walk_expr(e),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn walk_expr(&mut self, e: &Expr) {
        match e {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_)
            | Expr::TaggedLit { .. } => {}
            Expr::List(xs) | Expr::Tuple(xs) => xs.iter().for_each(|x| self.walk_expr(x)),
            Expr::Call { args, .. } | Expr::Ctor { args, .. } => {
                args.iter().for_each(|a| self.walk_expr(a))
            }
            Expr::LabeledCall { args, .. } => {
                args.iter().for_each(|(_, a)| self.walk_expr(a))
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.walk_expr(receiver);
                args.iter().for_each(|a| self.walk_expr(a));
            }
            Expr::Apply { func, args } => {
                self.walk_expr(func);
                args.iter().for_each(|a| self.walk_expr(a));
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
                self.walk_expr(expr)
            }
            Expr::Field { base, .. } => self.walk_expr(base),
            Expr::Lambda { params, body, .. } => {
                for p in params {
                    self.other_bind.insert(p.name.clone());
                }
                self.walk_block(body);
            }
            Expr::RecordUpdate { base, fields } => {
                self.walk_expr(base);
                fields.iter().for_each(|(_, v)| self.walk_expr(v));
            }
            Expr::Record { fields, spread, .. } => {
                fields.iter().for_each(|(_, v)| self.walk_expr(v));
                if let Some(s) = spread {
                    self.walk_expr(s);
                }
            }
            Expr::Binary { lhs, rhs, .. }
            | Expr::Index { base: lhs, index: rhs }
            | Expr::Range { lo: lhs, hi: rhs, .. } => {
                self.walk_expr(lhs);
                self.walk_expr(rhs);
            }
            Expr::If { cond, then_block, else_block } => {
                self.walk_expr(cond);
                self.walk_block(then_block);
                if let Some(eb) = else_block {
                    self.walk_block(eb);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    collect_pattern_vars(&arm.pattern, &mut self.other_bind);
                    if let Some(g) = &arm.guard {
                        self.walk_expr(g);
                    }
                    self.walk_expr(&arm.body);
                }
            }
            Expr::Block(b) => self.walk_block(b),
            Expr::While { cond, body } => {
                self.walk_expr(cond);
                self.walk_block(body);
            }
            Expr::For { var, iter, body } => {
                self.other_bind.insert(var.clone());
                self.walk_expr(iter);
                self.walk_block(body);
            }
            Expr::WhileLet { pattern, scrutinee, body } => {
                collect_pattern_vars(pattern, &mut self.other_bind);
                self.walk_expr(scrutinee);
                self.walk_block(body);
            }
        }
    }
}

fn collect_devirt_eligible(body: &Block) -> HashSet<String> {
    let mut s = DevirtScan::default();
    s.walk_block(body);
    let DevirtScan { let_bind, other_bind, reassigned } = s;
    let_bind
        .into_iter()
        .filter(|(n, c)| *c == 1 && !other_bind.contains(n) && !reassigned.contains(n))
        .map(|(n, _)| n)
        .collect()
}

/// (RFC-0034 L2) Is `for var in lo..hi` the bounds-elidable pattern
/// `for i in 0..list.length(xs)`, with `xs` and the loop var unshadowed and
/// unreassigned in `body`? If so, returns the `(index-var, list-var)` pair to register
/// while lowering the body, so a `list.at(xs, i)` there lowers to an unchecked load.
///
/// Soundness: the for-counter is compiler-managed (set to the counter each iteration,
/// advancing `lo, lo+1, …`), so inside the body `lo ≤ i < hi`. With `lo ≥ 0` and
/// `hi = list.length(xs)`, that is exactly `0 ≤ i < length(xs)` — in range — PROVIDED
/// the length we proved cannot change: `xs` must not be reassigned (which would rebind
/// it, possibly to a shorter list) nor re-bound by a shadowing `let`/tuple/for/param/
/// pattern (which would make `xs` at the access a different value than the one whose
/// length bounds the loop). `i` likewise must not be reassigned/shadowed in the body
/// (it would no longer equal the counter). The walk that proves this (`DevirtScan`) is
/// exhaustive. Half-open only: an inclusive `0..=length(xs)` would let `i == length`
/// (OOB), so it is rejected. Conservative everywhere — any deviation keeps the checked
/// access. Gated on `bounds-elide`; off ⇒ None ⇒ the access keeps its trap guard (the
/// de-opt reference the differential sweep compares against).
fn bounds_elide_pair(var: &str, lo: &Expr, hi: &Expr, inclusive: bool, body: &Block) -> Option<(String, String)> {
    if inclusive || !witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::BoundsElide) {
        return None;
    }
    match lo {
        Expr::Int(k) if *k >= 0 => {}
        _ => return None,
    }
    let xs = match hi {
        Expr::Call { name, args } if name == "list.length" && args.len() == 1 => match &args[0] {
            Expr::Var(x) => x.clone(),
            _ => return None,
        },
        _ => return None,
    };
    let mut scan = DevirtScan::default();
    scan.walk_block(body);
    let stable = |n: &str| {
        !scan.let_bind.contains_key(n) && !scan.other_bind.contains(n) && !scan.reassigned.contains(n)
    };
    (stable(&xs) && stable(var)).then_some((var.to_string(), xs))
}

fn collect_fn_refs_block(b: &Block, out: &mut HashSet<String>) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
                collect_fn_refs_expr(value, out)
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => collect_fn_refs_expr(e, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_fn_refs_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        // A range survives only inside a `for` iterator; scan its bounds for
        // referenced functions (e.g. `0..len(xs)`). The other sugar nodes are
        // fully lowered before codegen.
        Expr::Range { lo, hi, .. } => {
            collect_fn_refs_expr(lo, out);
            collect_fn_refs_expr(hi, out);
        }
        Expr::Index { .. }
        | Expr::WhileLet { .. }
        | Expr::MethodCall { .. }
        | Expr::Record { .. }
        | Expr::LabeledCall { .. } => {
            unreachable!("range/index sugar is lowered before codegen (parser::lower_sugar_module)")
        }
        Expr::Call { name, args } => {
            out.insert(name.clone());
            for a in args {
                collect_fn_refs_expr(a, out);
            }
        }
        Expr::Var(name) => {
            out.insert(name.clone());
        }
        Expr::Apply { func, args } => {
            collect_fn_refs_expr(func, out);
            for a in args {
                collect_fn_refs_expr(a, out);
            }
        }
        Expr::Ctor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => {
            for a in args {
                collect_fn_refs_expr(a, out);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            collect_fn_refs_expr(expr, out)
        }
        Expr::RecordUpdate { base, fields } => {
            collect_fn_refs_expr(base, out);
            for (_, v) in fields {
                collect_fn_refs_expr(v, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_fn_refs_expr(lhs, out);
            collect_fn_refs_expr(rhs, out);
        }
        Expr::If { cond, then_block, else_block } => {
            collect_fn_refs_expr(cond, out);
            collect_fn_refs_block(then_block, out);
            if let Some(b) = else_block {
                collect_fn_refs_block(b, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_fn_refs_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_fn_refs_expr(g, out);
                }
                collect_fn_refs_expr(&arm.body, out);
            }
        }
        Expr::While { cond, body } => {
            collect_fn_refs_expr(cond, out);
            collect_fn_refs_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_fn_refs_expr(iter, out);
            collect_fn_refs_block(body, out);
        }
        Expr::Lambda { body, .. } => collect_fn_refs_block(body, out),
        Expr::Block(b) => collect_fn_refs_block(b, out),
        Expr::Int(_)
        | Expr::Duration(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
    }
}

/// The set of functions reachable from `main` (transitively). Only these need
/// compiling — importing a std module no longer drags its whole API into the
/// output.
/// The naming convention that designates a JS-callable string export: a `pub fn`
/// whose name starts with this prefix and has the `(String) -> String` shape.
pub(crate) const STRING_EXPORT_PREFIX: &str = "export_";

/// Is `f` a JS-callable string export? — a `pub fn export_*(s: String) -> String`.
/// Such a function gets a stable `(in_ptr, in_len) -> ptr` export wrapper
/// (`__export_<name>`) plus the `__galloc` allocator, so a host (the browser
/// pure-compute shim, the glamour DOM shell) can drive it across the WASM boundary
/// with a JSON string in and a JSON string out.
///
/// The marker is the `export_` name PREFIX rather than the bare `(String)->String`
/// shape, because after linking the stdlib's own `pub fn`s (`string.to_upper`,
/// `trim`, …) are flattened into the same item list and would otherwise all become
/// roots/exports. An explicit, opt-in prefix scopes the host surface to functions
/// the author intends to expose — and is also the better security posture: the
/// JS-callable boundary is named, not implicit. It adds no import and no authority;
/// the wrapper only reads/writes guest memory (RFC-0007 §"Data marshaling",
/// RFC-0008's run loop).
pub(crate) fn is_string_export(f: &Function, grantable: &std::collections::HashSet<&str>) -> bool {
    let is_string = |t: &Option<Type>| matches!(t, Some(Type::Named(n, a)) if n == "String" && a.is_empty());
    // After linking a function is named `{module}.{name}` (the entry module's
    // `main` is the one exception). Match the unqualified tail against the prefix.
    let unqualified = f.name.rsplit('.').next().unwrap_or(&f.name);
    if !(f.public && unqualified.starts_with(STRING_EXPORT_PREFIX) && f.bounds.is_empty() && is_string(&f.ret)) {
        return false;
    }
    match f.params.as_slice() {
        // `pub fn export_*(String) -> String`.
        [p] => is_string(&p.ty),
        // (RFC-0040) `pub fn export_*(cap: <bare grantable>, String) -> String` — a
        // browser app root: the leading grantable cap is host-minted per call.
        [cap, s] => is_string(&s.ty) && export_cap_name(cap).is_some_and(|n| grantable.contains(n)),
        _ => false,
    }
}

/// (RFC-0040) The leading grantable-capability parameter's type name, if a
/// string-export function has one (`export_*(cap, String) -> String`).
pub(crate) fn export_cap_name(param: &Param) -> Option<&str> {
    match &param.ty {
        Some(Type::Named(n, _)) => Some(n.as_str()),
        _ => None,
    }
}

/// The JS export name for a string-export function: `__export_<unqualified>`. The
/// linker's `{module}.` prefix is dropped so a host calls a stable, source-named
/// export (`__export_step`) regardless of the rune's file/module name.
pub(crate) fn string_export_name(linked_name: &str) -> String {
    let unqualified = linked_name.rsplit('.').next().unwrap_or(linked_name);
    format!("__export_{unqualified}")
}


/// A short-circuit AND of i32 conditions, built as nested value-`if`s
/// (`c0 ? (c1 ? … : 0) : 0`).
fn wir_and_chain(conds: &[witchy_wir::wir::WirExpr]) -> witchy_wir::wir::WirExpr {
    use witchy_wir::wir::{WirExpr as W, WirNode as N};
    match conds.split_first() {
        None => W::ConstI32(1),
        Some((first, rest)) => W::Control(Box::new(N::If {
            cond: first.clone(),
            then_: vec![N::Push(wir_and_chain(rest))],
            els: vec![N::Push(W::ConstI32(0))],
            result: Some(witchy_wir::wir::WirTy::Bool),
        })),
    }
}

/// Short-circuit OR of i32 boolean conditions — `if first: 1 else: (rest…)`.
/// The dual of `wir_and_chain`; used for or-patterns (`1 | 2 | 3`).
fn wir_or_chain(conds: &[witchy_wir::wir::WirExpr]) -> witchy_wir::wir::WirExpr {
    use witchy_wir::wir::{WirExpr as W, WirNode as N};
    match conds.split_first() {
        None => W::ConstI32(0),
        Some((first, rest)) => W::Control(Box::new(N::If {
            cond: first.clone(),
            then_: vec![N::Push(W::ConstI32(1))],
            els: vec![N::Push(wir_or_chain(rest))],
            result: Some(witchy_wir::wir::WirTy::Bool),
        })),
    }
}

/// The fields of an aggregate literal (record `Ctor` or tuple), positionally, for
/// scalar replacement — `None` for any other expression.
fn sroa_fields(e: &Expr) -> Option<&[Expr]> {
    match e {
        Expr::Ctor { args, .. } | Expr::Tuple(args) => Some(args),
        _ => None,
    }
}

/// Whether expression `e` mentions the variable `name` ANYWHERE (even inside an
/// element read like `name.at(0)`). Used by RC-floor in-place reuse to bail on a
/// self-referential reassignment (`x = [x.at(0), …]`), where overwriting a slot
/// before a later element reads it would corrupt the value — that one site
/// allocates a fresh list instead.
fn expr_reads_var(e: &Expr, name: &str) -> bool {
    match e {
        Expr::Var(n) => n == name,
        _ => {
            let mut found = false;
            crate::escape::for_each_immediate_subexpr(e, &mut |s| {
                found = found || expr_reads_var(s, name);
            });
            found
        }
    }
}

/// The `(source, lo, hi)` of a `list.slice(source, lo, hi)` call — the binding a
/// confined slice view elides — or `None` for any other expression.
fn view_slice_args(e: &Expr) -> Option<(&Expr, &Expr, &Expr)> {
    match e {
        Expr::Call { name, args }
            if crate::escape::is_list_slice(name) && args.len() == 3 =>
        {
            Some((&args[0], &args[1], &args[2]))
        }
        _ => None,
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
            Stmt::LetPattern { pattern, value } => {
                witchy_syntax::ast::pattern_binds(pattern, out);
                collect_let_names_expr(value, out);
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => collect_let_names_expr(e, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_let_names_expr(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        // A range survives only inside a `for` iterator; recurse its bounds.
        // (A range bound has no `let`, but a bound expression could nest one.)
        // The other sugar nodes are fully lowered before codegen.
        Expr::Range { lo, hi, .. } => {
            collect_let_names_expr(lo, out);
            collect_let_names_expr(hi, out);
        }
        Expr::Index { .. }
        | Expr::WhileLet { .. }
        | Expr::MethodCall { .. }
        | Expr::Record { .. }
        | Expr::LabeledCall { .. } => {
            unreachable!("range/index sugar is lowered before codegen (parser::lower_sugar_module)")
        }
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
            // A range `for` declares an i64 counter + end bound; a list `for`
            // declares the list pointer + index. The kinds come from
            // `infer_locals`; here we only name the locals so they're declared.
            if matches!(iter.as_ref(), Expr::Range { .. }) {
                out.push(format!("__forctr_{var}"));
                out.push(format!("__forend_{var}"));
            } else {
                out.push(format!("__forlist_{var}"));
                out.push(format!("__fori_{var}"));
            }
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
        // Value-position sub-expressions may hold a nested block with `let`s
        // (e.g. a list comprehension as a call argument), so recurse into them.
        // A lambda body is compiled as its own function, so its lets are not
        // this function's locals.
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => {
            for a in args {
                collect_let_names_expr(a, out);
            }
        }
        Expr::Apply { func, args } => {
            collect_let_names_expr(func, out);
            for a in args {
                collect_let_names_expr(a, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_let_names_expr(lhs, out);
            collect_let_names_expr(rhs, out);
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            collect_let_names_expr(expr, out)
        }
        Expr::RecordUpdate { base, fields } => {
            collect_let_names_expr(base, out);
            for (_, v) in fields {
                collect_let_names_expr(v, out);
            }
        }
        Expr::Var(_)
        | Expr::Int(_)
        | Expr::Duration(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. }
        | Expr::Lambda { .. } => {}
    }
}


#[cfg(test)]
#[path = "../codegen_tests.rs"]
mod tests;
