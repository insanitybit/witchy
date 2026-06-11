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
//! An actor compiles to its own module: an `Int` field becomes a mutable WASM
//! global, a `String` field a host-side cell (the per-message arena reset would
//! clobber a guest-heap string), capability fields are erased (their authority
//! is the host import), and each `on` handler becomes an exported function the
//! host calls to deliver a message.
//!
//! `send` between compiled actors crosses the VM boundary by value: Int, Float,
//! and Subject fields are copied (passing a Subject delegates send authority);
//! String, List(Int), and List(String) fields are read out of the sender by
//! content and re-laid out in the receiver (`__msg_alloc`). Not yet compiled:
//! tuple/record message parameters and `spawn` from compiled code — each
//! errors clearly.

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

/// One captured variable for a closure: (name, is-global-actor-field,
/// record-type-name, list-element-type-name, slot kind).
type CaptureInfo = (String, bool, Option<String>, Option<String>, Kind);

/// Scratch local holding the Result/Option being unwrapped by `?`.
const TRY_TMP: &str = "__witchy_try_tmp";

/// Scratch local holding a `match` scrutinee while arms test it.
const MATCH_TMP: &str = "__witchy_match_tmp";

/// One scratch local per nesting level of expression application (`f(x)(y)`),
/// holding the callee pointer while its arguments are evaluated. A nested
/// application inside an argument uses the next level, so the levels never
/// clobber each other. Application nested deeper than this in argument
/// position is rejected (absurd in practice).
const APPLY_POOL: usize = 8;

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

fn wasm_ty(k: Kind) -> &'static str {
    match k {
        Kind::I32 => "i32",
        Kind::I64 => "i64",
        Kind::F64 => "f64",
    }
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

/// Emit the conversion that puts a value of WASM kind `k` into the universal i64
/// slot representation (for storing in a list/tuple/record/closure slot, or
/// passing where a type variable is expected).
fn to_slot(k: Kind) -> &'static str {
    match k {
        Kind::I64 => "",
        Kind::F64 => "    i64.reinterpret_f64\n",
        // Sign-extend (matching `kind_convert`'s i32->i64): a generic slot may
        // carry a negative `Int` that entered through the i32 ABI, and a concrete
        // `Int` reader loads the slot as i64 directly — zero-extension turned -1
        // into 4294967295. Pointers and Bools are always < 2^31 (high bit clear),
        // so sign-extension leaves them unchanged.
        Kind::I32 => "    i64.extend_i32_s\n",
    }
}

/// Emit the conversion that recovers a value of WASM kind `k` from the universal
/// i64 slot representation (after loading a slot, or receiving a generic value).
fn from_slot(k: Kind) -> &'static str {
    match k {
        Kind::I64 => "",
        Kind::F64 => "    f64.reinterpret_i64\n",
        Kind::I32 => "    i32.wrap_i64\n",
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

/// Convert a value of WASM kind `from` to kind `to` at an ABI boundary (a call
/// argument or a return). The only crossings that occur in practice are between
/// a concrete `Int` (i64) and the generic i32 ABI; matching kinds need nothing.
fn kind_convert(from: Kind, to: Kind) -> &'static str {
    match (from, to) {
        (Kind::I64, Kind::I32) => "    i32.wrap_i64\n",
        (Kind::I32, Kind::I64) => "    i64.extend_i32_s\n",
        _ => "",
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
    inout: bool,
    inout_params: Vec<String>,
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
    /// Whether the string-equality helper `$str_eq` is needed (string patterns).
    uses_str_eq: bool,
    /// Whether the `print_float` import is needed (a float-returning `main`).
    uses_print_float: bool,
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
    /// Message name -> tag, shared across a program's actors so the host can
    /// route a compiled `send` to the target actor's handler.
    message_tags: HashMap<String, u32>,
    /// Whether the inter-actor `send` import is needed.
    uses_send: bool,
    /// Whether the bounds-checked `$list_at` helper is needed (list indexing).
    uses_list_at: bool,
    /// Whether the list `push`/`concat`/`drop` runtime helpers are needed.
    uses_list_push: bool,
    uses_list_concat: bool,
    uses_list_drop: bool,
    /// Whether the `starts_with`/`ends_with` string helpers are needed.
    uses_starts_with: bool,
    /// Whether the `crypto.ed25519_verify` host import is needed.
    uses_crypto_ed25519_verify: bool,
    /// Whether the `crypto.sha256` host import + guest helper are needed.
    uses_crypto_sha256: bool,
    /// Whether the `crypto.rune_hash` host import + guest helper are needed.
    uses_crypto_rune_hash: bool,
    /// Whether the actor needs the `__msg_alloc` export (a String message
    /// parameter the host re-allocates into this actor's memory).
    uses_msg_alloc: bool,
    /// String state fields of the actor being compiled -> host cell index.
    /// String state lives in HOST cells (not guest globals): the per-message
    /// arena reset would clobber a guest-heap string between messages.
    str_fields: HashMap<String, u32>,
    /// Whether any String state field exists (links the field_str host pair).
    uses_str_field: bool,
    /// Whether the `compiler.footprint` host import + guest helper are needed.
    uses_compiler_footprint: bool,
    /// Whether the `compiler.diff` host import + guest helper are needed.
    uses_compiler_diff: bool,
    /// Whether the `float_to_str` host import + guest helper are needed (float
    /// `to_string`).
    uses_float_to_str: bool,
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
    /// Which `Dir` operations the program uses ("read"/"write"/"exists"/
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
    /// ADT/record type name -> each variant's field types, indexed by tag, for
    /// structural `==` on sum types (`Color`, `Shape`, ...). Generic variant
    /// fields (a type variable) make a type unresolvable here -> a loud error.
    adt_variants: HashMap<String, Vec<Vec<Type>>>,
    /// Constructor name -> its owning type name (so a `Ctor` operand of `==` can
    /// find its variant set).
    ctor_type_name: HashMap<String, String>,
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
    /// Function name -> the value type it returns, so `to_string(f(...))` can be
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
    /// Whether the current function has any `inout` parameters.
    cur_fn_inout: bool,
    /// The current function's `inout` parameter names, in declaration order. An
    /// early `return`/`?` must push these (after the primary result) so the
    /// multi-result epilogue is reproduced on every exit path.
    cur_fn_inout_params: Vec<String>,
    /// Lifted lambda functions, indexed by their table slot: a `fn(...) {...}`
    /// expression compiles to a function `$__lam{i}` here and evaluates to the
    /// index `i`. A `call_indirect` through the function table then invokes it.
    lambdas: Vec<String>,
    /// Generated per-shape structural-equality helper functions, keyed by
    /// `EqShape::id` so each shape is emitted once. A `BTreeMap` keeps emission
    /// order deterministic.
    eq_helpers: std::collections::BTreeMap<String, String>,
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
            uses_print: false,
            uses_print_int: false,
            uses_concat: false,
            uses_int_to_string: false,
            globals: HashSet::new(),
            cap_fields: HashSet::new(),
            fn_conventions: HashMap::new(),
            fn_params: HashMap::new(),
            ctors: HashMap::new(),
            ctor_field_records: HashMap::new(),
            mk_arities: HashSet::new(),
            next_label: 0,
            uses_str_eq: false,
            uses_print_float: false,
            locals: HashMap::new(),
            fn_ret: HashMap::new(),
            fn_ret_closure_kind: HashMap::new(),
            fn_ret_tuple_slots: HashMap::new(),
            fn_ret_list_elem_tuple_slots: HashMap::new(),
            fn_ret_tuple_slot_list_elem: HashMap::new(),
            message_tags: HashMap::new(),
            uses_send: false,
            record_fields: HashMap::new(),
            record_field_types: HashMap::new(),
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
            cur_fn_inout: false,
            cur_fn_inout_params: Vec::new(),
            uses_list_at: false,
            uses_list_push: false,
            uses_list_concat: false,
            uses_list_drop: false,
            uses_starts_with: false,
            uses_crypto_ed25519_verify: false,
            uses_crypto_sha256: false,
            uses_crypto_rune_hash: false,
            uses_msg_alloc: false,
            str_fields: HashMap::new(),
            uses_str_field: false,
            uses_compiler_footprint: false,
            uses_compiler_diff: false,
            uses_float_to_str: false,
            uses_encoding: false,
            uses_float_ord: false,
            uses_now: false,
            uses_get_env: false,
            used_dir_ops: std::collections::BTreeSet::new(),
            used_build_ops: std::collections::BTreeSet::new(),
            used_net_ops: std::collections::BTreeSet::new(),
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
            lambdas: Vec::new(),
            eq_helpers: std::collections::BTreeMap::new(),
            ts_helpers: std::collections::BTreeMap::new(),
            adt_variant_names: HashMap::new(),
            clos_arities: HashSet::new(),
            apply_level: 0,
            loop_labels: Vec::new(),
        }
    }

    /// The WASM kind a compiled expression evaluates to.
    fn kind_of(&self, e: &Expr) -> Kind {
        match e {
            Expr::Int(_) | Expr::Duration(_) => Kind::I64,
            Expr::Float(_) => Kind::F64,
            Expr::Var(n) => self.locals.get(n).copied().unwrap_or(Kind::I32),
            Expr::Unary { op, expr } => match op {
                // `!x` is a bool (i32); negation/complement keep the operand kind.
                UnOp::Not => Kind::I32,
                UnOp::Neg | UnOp::BitNot | UnOp::Move => self.kind_of(expr),
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
                // concat (ptr) and comparisons / and / or (bool) are i32.
                _ => Kind::I32,
            },
            Expr::Field { base, field } => {
                if let Some(bt) = self.record_type_of(base) {
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
                .iter()
                .fold(Kind::I32, |acc, a| promote_kind(acc, self.kind_of(&a.body))),
            // `get_or(d, k, default)` returns the dict's value at the default's
            // kind (the i64 value slot is recovered to it at the call site).
            Expr::Call { name, args } if name == "get_or" && args.len() == 3 => {
                self.kind_of(&args[2])
            }
            Expr::Call { name, .. } => match name.as_str() {
                "int_to_float" => Kind::F64,
                "float_to_int" | "string_length" | "char_count" | "index_of" | "length"
                | "string_to_int" | "int_to_duration" | "duration_to_int" | "now" => Kind::I64,
                "at" => self.elem_kind_of_list_arg(e),
                "to_string" | "int_to_string" | "print" => Kind::I32,
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
                .match_payload_valtype(inner)
                .map(valtype_kind)
                .unwrap_or(Kind::I32),
            // A closure call `f(x)` returns the universal i64 slot; recover it at
            // the closure's declared return kind (an Int-returning closure as i64).
            Expr::Apply { func, .. } => self.apply_ret_kind(func),
            _ => Kind::I32, // Bool, Str, List, Ctor, Spawn
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

    /// The WASM kind of the element produced by `at(list, i)`: the list's tracked
    /// element kind, or i32 (the generic ABI) when unknown. The `at` *emission*
    /// uses the same `list_elem_kind`, so the typed-expression kind and the loaded
    /// width always agree.
    fn elem_kind_of_list_arg(&self, e: &Expr) -> Kind {
        if let Expr::Call { name, args } = e {
            if name == "at" {
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
    fn list_elem_kind(&self, list: &Expr) -> Kind {
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
            Expr::Int(_) | Expr::Duration(_) => ValType::Int,
            Expr::Bool(_) => ValType::Bool,
            Expr::Float(_) => ValType::Float,
            Expr::Str(_) => ValType::Str,
            Expr::Unary { op, expr } => match op {
                UnOp::Not => ValType::Bool,
                UnOp::Neg | UnOp::Move => self.val_type_of(expr),
                UnOp::BitNot => ValType::Int,
            },
            Expr::Binary { op, lhs, .. } => match op {
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
                | BinOp::And | BinOp::Or => ValType::Bool,
                BinOp::Concat => ValType::Str,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    self.val_type_of(lhs)
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
            Expr::Call { name, args } if name == "at" && !args.is_empty() => {
                self.elem_val_type_of(&args[0])
            }
            // `get_or(d, k, default)` returns the Dict's value type, which is the
            // default's type — so a `let v = get_or(d, k, 0)` (or a String default)
            // tracks `v`, and `v` can in turn be used as a Dict key.
            Expr::Call { name, args } if name == "get_or" && args.len() == 3 => {
                self.val_type_of(&args[2])
            }
            Expr::Call { name, .. } => match name.as_str() {
                "int_to_string" | "to_string" | "to_upper" | "to_lower" | "trim" | "replace"
                | "substring" | "crypto.sha256" | "crypto.sign" | "crypto.public_key" | "read"
                | "crypto.rune_hash" | "compiler.footprint" | "compiler.diff"
                | "recv_line" | "recv_all" | "recv_bytes" => ValType::Str,
                "starts_with" | "ends_with" | "contains" | "has" | "exists" | "is_dir"
                | "crypto.ed25519_verify" => ValType::Bool,
                "string_length" | "char_count" | "index_of" | "length" | "size" | "float_to_int"
                | "string_to_int" | "int_to_duration" | "duration_to_int" | "now" => ValType::Int,
                "int_to_float" | "sqrt" => ValType::Float,
                other => self.fn_ret_valtype.get(other).copied().unwrap_or(ValType::Other),
            },
            // `inner?` yields the Ok/Some payload's value type, so `to_string` of
            // a `?`-unwrapped value renders correctly and `==` picks `$str_eq`.
            Expr::Try(inner) => self.match_payload_valtype(inner).unwrap_or(ValType::Other),
            // A record field access (`p.x`): the field's declared value type — so
            // `"${p.x}"` / `to_string(p.x)` and `==` on a field resolve.
            Expr::Field { base, field } => {
                self.field_type_of(base, field).map(|t| ty_to_valtype(&t)).unwrap_or(ValType::Other)
            }
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

    /// The declared type of `base.field`, where `base`'s record type is known —
    /// so a field access resolves its value type (`to_string(p.x)`) and its
    /// structural shape (`to_string(p.tags)`), not just whether it is a record.
    fn field_type_of(&self, base: &Expr, field: &str) -> Option<Type> {
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
    fn match_payload_record(&self, scrutinee: &Expr) -> Option<String> {
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

    /// The scalar value type a Dict holds, where determinable: an `insert(d, k,
    /// v)` gives it from `v`; a Dict variable carries its tracked type.
    fn dict_value_valtype_of(&self, value: &Expr) -> Option<ValType> {
        match value {
            Expr::Call { name, args } if name == "insert" && args.len() == 3 => {
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
            Expr::Call { name, args } if name == "insert" && args.len() == 3 => {
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

    /// Collect `(var, record_type)` for each pattern variable bound to a
    /// record-typed constructor field, recursing through nested patterns. Lets a
    /// `match` arm like `Circle(p) -> p.x` resolve `p`'s record type.
    fn pattern_record_binds(&self, pat: &Pattern, out: &mut Vec<(String, String)>) {
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
            Expr::Call { name, args } if name == "pairs" && args.len() == 1 => {
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
            Expr::Call { name, args } if name == "at" && args.len() == 2 => {
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
                if name == "split"
                    || name == "to_chars"
                    || name == "string_chars"
                    || name == "list" =>
            {
                ValType::Str
            }
            // `values(d)` yields a list of the Dict's values; carry their type so
            // `for v in values(d)` recovers an Int value as i64.
            Expr::Call { name, args } if name == "values" && args.len() == 1 => match &args[0] {
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
            Expr::Call { name, args } if name == "keys" && args.len() == 1 => match &args[0] {
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
            Expr::Call { name, args } if name == "at" && args.len() == 2 => {
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
                        if fname == "at" && args.len() == 2 {
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
                Stmt::LetTuple { names, value } => {
                    // The value type of each binding: from a tuple literal's
                    // elements, or a tuple-typed variable's tracked slot types,
                    // else Other. This drives both the binding's value type (for
                    // `to_string`, Dict keys, ...) and its WASM kind (Int->i64).
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
                        if fname == "at" && args.len() == 2 {
                            // `let (a, b) = at(list_of_tuples, i)`: the element-tuple
                            // slot types of the list (variable or literal).
                            self.list_elem_tuple_slots(&args[0])
                                .filter(|s| s.len() == names.len())
                                .unwrap_or_else(|| vec![ValType::Other; names.len()])
                        } else {
                            // A tuple-returning call: destructure at its slot types.
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
                        self.local_val_types.insert(n.clone(), *vt);
                        self.locals.insert(n.clone(), valtype_kind(*vt));
                    }
                    // `let (xs, ys) = f(...)` where f returns `(List(T), List(U))`:
                    // record each destructured list var's element type, so a later
                    // `at(xs, i)` recovers an Int element as i64.
                    if let Expr::Call { name: fname, .. } = value {
                        if let Some(elems) = self.fn_ret_tuple_slot_list_elem.get(fname) {
                            for (n, elem) in names.iter().zip(elems) {
                                if let Some(vt) = elem {
                                    self.local_list_elem_valtype.insert(n.clone(), *vt);
                                }
                            }
                        }
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
                for arm in arms {
                    // Pattern-bound vars are i32 (floats aren't stored in records).
                    let mut pvars = Vec::new();
                    collect_pattern_vars(&arm.pattern, &mut pvars);
                    for v in pvars {
                        self.locals.insert(v, Kind::I32);
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

    fn need_heap(&self) -> bool {
        self.uses_concat
            || self.uses_int_to_string
            || !self.strings.is_empty()
            || !self.mk_arities.is_empty()
            || self.uses_list_push
            || self.uses_list_concat
            || self.uses_list_drop
            || self.uses_split
            || self.uses_str_chars
            || self.uses_substr
            || self.uses_ascii_case
            || self.uses_replace
            || self.uses_dict
            || self.uses_dict_iter
            || self.uses_crypto_sha256
            || self.uses_crypto_rune_hash
            || self.uses_msg_alloc
            || self.uses_str_field
            || self.uses_compiler_footprint
            || self.uses_compiler_diff
            || self.uses_float_to_str
            || self.uses_encoding
            || self.uses_get_env
            || self.used_dir_ops.contains("read")
            || self.used_dir_ops.contains("list")
            || self.used_net_ops.contains("recv_line")
            || self.used_net_ops.contains("recv_all")
            || self.used_net_ops.contains("recv_bytes")
            || self.uses_args
            || self.uses_crypto_sign
            || self.uses_crypto_public_key
    }

    fn emit_imports(&self) -> String {
        let mut s = String::new();
        if self.uses_print {
            s.push_str("  (import \"witchy\" \"print\" (func $print (param i32 i32)))\n");
        }
        if self.uses_print_int {
            s.push_str("  (import \"witchy\" \"print_int\" (func $print_int (param i64)))\n");
        }
        if self.uses_print_float {
            s.push_str("  (import \"witchy\" \"print_float\" (func $print_float (param f64)))\n");
        }
        if self.uses_send {
            // send(target_id, message_tag, arg)
            s.push_str("  (import \"witchy\" \"send\" (func $send (param i32 i32 i32)))\n");
        }
        if self.uses_crypto_ed25519_verify {
            // crypto.ed25519_verify(pk_ptr, msg_ptr, sig_ptr) -> bool; each arg is
            // a string header pointer, the result an i32 bool.
            s.push_str("  (import \"witchy\" \"crypto.ed25519_verify\" (func $crypto_ed25519_verify (param i32 i32 i32) (result i32)))\n");
        }
        if self.uses_crypto_sha256 {
            // crypto.sha256(in_header_ptr, out_data_ptr): the host writes 64 hex
            // bytes at out_data_ptr (the guest pre-allocates the result string).
            s.push_str("  (import \"witchy\" \"crypto.sha256\" (func $crypto_sha256_host (param i32 i32)))\n");
        }
        if self.uses_crypto_rune_hash {
            // crypto.rune_hash(paths_ptr, contents_ptr, out_data_ptr): the host
            // walks both guest string lists and writes the fixed 71-byte
            // `sha256:<hex>` store hash at out_data_ptr.
            s.push_str("  (import \"witchy\" \"crypto.rune_hash\" (func $crypto_rune_hash_host (param i32 i32 i32)))\n");
        }
        if self.uses_compiler_footprint {
            // compiler_footprint_len(src_ptr) -> JSON byte length: the host
            // computes and stages the footprint JSON; `fill_pending` writes it
            // (the dir_read staging protocol).
            s.push_str("  (import \"witchy\" \"compiler_footprint_len\" (func $compiler_footprint_len_host (param i32) (result i32)))\n");
        }
        if self.uses_compiler_diff {
            // compiler_diff_len(old_ptr, new_ptr) -> JSON byte length: staged like
            // compiler_footprint_len.
            s.push_str("  (import \"witchy\" \"compiler_diff_len\" (func $compiler_diff_len_host (param i32 i32) (result i32)))\n");
        }
        if self.uses_str_field {
            // String actor state: field_str_set(idx, str_ptr) copies the value
            // OUT to its host cell; field_str_len(idx) stages the cell's bytes
            // for `fill_pending` (the dir_read staging protocol).
            s.push_str("  (import \"witchy\" \"field_str_set\" (func $field_str_set_host (param i32 i32)))\n");
            s.push_str("  (import \"witchy\" \"field_str_len\" (func $field_str_len_host (param i32) (result i32)))\n");
        }
        if self.uses_float_to_str {
            // float_to_str(x, out_data_ptr) -> byte length: the host formats `x`
            // (Rust Display) into the guest's pre-allocated buffer.
            s.push_str("  (import \"witchy\" \"float_to_str\" (func $float_to_str_host (param f64 i32) (result i32)))\n");
        }
        if self.uses_encoding {
            // encoding(op, in_header_ptr, out_data_ptr) -> byte length. op selects
            // hex/base64 encode/decode; the host runs the same native transform the
            // interpreter does and writes the result into the guest's buffer.
            s.push_str("  (import \"witchy\" \"encoding\" (func $encoding_host (param i32 i32 i32) (result i32)))\n");
        }
        if self.uses_now {
            // now() -> epoch milliseconds. Capability-gated: linked only when the
            // actor holds a Clock grant; an ungranted module fails instantiation.
            s.push_str("  (import \"witchy\" \"now\" (func $now_host (result i64)))\n");
        }
        if self.uses_crypto_sign {
            // crypto.sign(msg_ptr, out_data_ptr): the host signs with the GRANTED
            // key and writes the 128 hex signature bytes. Secret-gated.
            s.push_str("  (import \"witchy\" \"crypto.sign\" (func $crypto_sign_host (param i32 i32)))\n");
        }
        if self.uses_crypto_public_key {
            // crypto.public_key(out_data_ptr): the granted key's 64 hex public bytes.
            s.push_str("  (import \"witchy\" \"crypto.public_key\" (func $crypto_public_key_host (param i32)))\n");
        }
        if self.uses_get_env {
            // env_len(name) -> value byte length or -1 (unset); env_fill(name, out)
            // writes the bytes. Capability-gated on an Env grant.
            s.push_str("  (import \"witchy\" \"env_len\" (func $env_len_host (param i32) (result i32)))\n");
            s.push_str("  (import \"witchy\" \"env_fill\" (func $env_fill_host (param i32 i32)))\n");
        }
        // The Dir family: a guest Dir value is an i32 handle into the host's
        // path table; each operation is its own capability-gated import, so the
        // module's import list IS its filesystem footprint.
        if self.used_dir_ops.contains("subdir") {
            s.push_str("  (import \"witchy\" \"dir_subdir\" (func $dir_subdir_host (param i32 i32) (result i32)))\n");
        }
        if self.used_dir_ops.contains("read") {
            s.push_str("  (import \"witchy\" \"dir_read_len\" (func $dir_read_len_host (param i32 i32) (result i32)))\n");
        }
        if self.used_dir_ops.contains("exists") {
            s.push_str("  (import \"witchy\" \"dir_exists\" (func $dir_exists_host (param i32 i32) (result i32)))\n");
        }
        if self.used_dir_ops.contains("is_dir") {
            s.push_str("  (import \"witchy\" \"dir_is_dir\" (func $dir_is_dir_host (param i32 i32) (result i32)))\n");
        }
        if self.used_dir_ops.contains("list") {
            s.push_str("  (import \"witchy\" \"dir_list_size\" (func $dir_list_size_host (param i32) (result i32)))\n");
        }
        if self.uses_args {
            s.push_str("  (import \"witchy\" \"args_size\" (func $args_size_host (result i32)))\n");
        }
        if self.used_dir_ops.contains("list") || self.uses_args {
            s.push_str("  (import \"witchy\" \"write_pending_list\" (func $write_pending_list_host (param i32)))\n");
        }
        if self.used_dir_ops.contains("write") {
            s.push_str("  (import \"witchy\" \"dir_write\" (func $dir_write_host (param i32 i32 i32)))\n");
        }
        if self.used_dir_ops.contains("make_dir") {
            s.push_str("  (import \"witchy\" \"dir_make_dir\" (func $dir_make_dir_host (param i32 i32)))\n");
        }
        // Build-time host ops: write generated source into the confined output
        // sandbox, and read from the confined read roots (staged like `dir_read`).
        if self.used_build_ops.contains("write_out") {
            s.push_str("  (import \"witchy\" \"build_out_write\" (func $build_out_write_host (param i32 i32 i32)))\n");
        }
        if self.used_build_ops.contains("read_build") {
            s.push_str("  (import \"witchy\" \"build_read_len\" (func $build_read_len_host (param i32 i32) (result i32)))\n");
        }
        // The Net family: a guest Net/Socket/Listener is an i32 handle into the
        // host's tables; the import list is the program's network footprint.
        if self.used_net_ops.contains("restrict") {
            s.push_str("  (import \"witchy\" \"net_restrict\" (func $net_restrict_host (param i32 i32) (result i32)))\n");
        }
        if self.used_net_ops.contains("connect") {
            s.push_str("  (import \"witchy\" \"net_connect\" (func $net_connect_host (param i32 i32) (result i32)))\n");
        }
        if self.used_net_ops.contains("listen") {
            s.push_str("  (import \"witchy\" \"net_listen\" (func $net_listen_host (param i32 i32) (result i32)))\n");
        }
        if self.used_net_ops.contains("accept") {
            s.push_str("  (import \"witchy\" \"net_accept\" (func $net_accept_host (param i32) (result i32)))\n");
        }
        if self.used_net_ops.contains("send_line") {
            s.push_str("  (import \"witchy\" \"net_send_line\" (func $net_send_line_host (param i32 i32)))\n");
        }
        if self.used_net_ops.contains("send_bytes") {
            s.push_str("  (import \"witchy\" \"net_send_bytes\" (func $net_send_bytes_host (param i32 i32)))\n");
        }
        if self.used_net_ops.contains("recv_line") {
            s.push_str("  (import \"witchy\" \"net_recv_line_len\" (func $net_recv_line_len_host (param i32) (result i32)))\n");
        }
        if self.used_net_ops.contains("recv_all") {
            s.push_str("  (import \"witchy\" \"net_recv_all_len\" (func $net_recv_all_len_host (param i32) (result i32)))\n");
        }
        if self.used_net_ops.contains("recv_bytes") {
            s.push_str("  (import \"witchy\" \"net_recv_bytes_len\" (func $net_recv_bytes_len_host (param i32 i64) (result i32)))\n");
        }
        if self.used_net_ops.contains("close") {
            s.push_str("  (import \"witchy\" \"net_close\" (func $net_close_host (param i32)))\n");
        }
        // `fill_pending` is the shared, authority-free transfer primitive for
        // every staged read (Dir read, Net recv, compiler JSON) — emitted once.
        if self.used_dir_ops.contains("read")
            || self.used_build_ops.contains("read_build")
            || self.used_net_ops.contains("recv_line")
            || self.used_net_ops.contains("recv_all")
            || self.used_net_ops.contains("recv_bytes")
            || self.uses_compiler_footprint
            || self.uses_compiler_diff
            || self.uses_str_field
        {
            s.push_str("  (import \"witchy\" \"fill_pending\" (func $fill_pending_host (param i32)))\n");
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
        if self.uses_list_at {
            s.push_str(LIST_AT_WAT);
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
        if self.uses_ascii_case {
            s.push_str(ASCII_CASE_WAT);
        }
        // `$crypto_sha256` allocates the 68-byte result string, then the host
        // import fills its 64 hex bytes.
        if self.uses_crypto_sha256 {
            s.push_str(CRYPTO_SHA256_WAT);
        }
        if self.uses_crypto_rune_hash {
            s.push_str(CRYPTO_RUNE_HASH_WAT);
        }
        if self.uses_str_field {
            s.push_str(FIELD_STR_GET_WAT);
        }
        if self.uses_compiler_footprint {
            s.push_str(COMPILER_FOOTPRINT_WAT);
        }
        if self.uses_compiler_diff {
            s.push_str(COMPILER_DIFF_WAT);
        }
        if self.uses_float_to_str {
            s.push_str(FLOAT_TO_STR_WAT);
        }
        if self.uses_encoding {
            s.push_str(ENCODING_WAT);
        }
        if self.uses_get_env {
            s.push_str(GET_ENV_WAT);
        }
        if self.used_dir_ops.contains("read") {
            s.push_str(DIR_READ_WAT);
        }
        if self.used_build_ops.contains("read_build") {
            s.push_str(BUILD_READ_WAT);
        }
        if self.used_dir_ops.contains("list") {
            s.push_str(DIR_LIST_WAT);
        }
        if self.used_net_ops.contains("recv_line") {
            s.push_str(NET_RECV_LINE_WAT);
        }
        if self.used_net_ops.contains("recv_all") {
            s.push_str(NET_RECV_ALL_WAT);
        }
        if self.used_net_ops.contains("recv_bytes") {
            s.push_str(NET_RECV_BYTES_WAT);
        }
        if self.uses_args {
            s.push_str(BUILD_ARGS_WAT);
        }
        if self.uses_crypto_sign {
            s.push_str(CRYPTO_SIGN_WAT);
        }
        if self.uses_crypto_public_key {
            s.push_str(CRYPTO_PUBLIC_KEY_WAT);
        }
        if self.uses_float_ord {
            s.push_str(FLOAT_ORD_WAT);
        }
        // `$split` builds its result list with `$list_push` (emitted above via
        // `uses_list_push`, which the split call site also sets).
        if self.uses_split {
            s.push_str(SPLIT_WAT);
        }
        // `$str_chars` builds a list of single-char strings; it uses `$byte_to_char`
        // (char count), `$str_substring` (each char), and `$list_push` — all forced
        // on by the call site.
        if self.uses_str_chars {
            s.push_str(STR_CHARS_WAT);
        }
        // Substring search (`contains`/`index_of`) and char-indexed slicing.
        if self.uses_find_byte {
            s.push_str(FIND_BYTE_WAT);
        }
        // `$byte_to_char` backs both index_of (char index) and char_count;
        // emit it once if either needs it.
        if self.uses_index_of || self.uses_byte_to_char {
            s.push_str(BYTE_TO_CHAR_WAT);
        }
        if self.uses_index_of {
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
        if self.uses_str_to_int {
            s.push_str(STR_TO_INT_WAT);
        }
        if self.uses_trim {
            s.push_str(IS_WS_WAT);
            s.push_str(TRIM_WAT);
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
        // `$dict_update` calls `$dict_get_or` + `$dict_insert` (emitted above via
        // `uses_dict`, which the update call site forces on) and the closure ABI.
        if self.uses_dict_update {
            s.push_str(DICT_UPDATE_WAT);
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
            // the declared type (authoritative), so `to_string(p)` / `p == q`
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
        self.cur_fn_inout_params = f
            .params
            .iter()
            .filter(|p| p.convention == Convention::Inout)
            .map(|p| p.name.clone())
            .collect();
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
        header.push_str(&format!("    (local ${MATCH_TMP} i64)\n"));
        for i in 0..APPLY_POOL {
            header.push_str(&format!("    (local $__witchy_call_{i} i32)\n"));
        }

        self.apply_level = 0;
        let body = self.compile_block(&renamed)?;
        // The body's tail value must match the declared result kind (a generic
        // i32 body returned from an `-> Int` function is widened, etc.).
        let body = format!("{body}{}", kind_convert(self.block_kind(&renamed), ret_kind));
        // Move-out: append each `inout` parameter's final value (declaration order).
        let epilogue = self.inout_epilogue();
        Ok(format!("{header}{body}{epilogue}  )\n"))
    }

    /// The move-out epilogue for an `inout` function: push each inout param's
    /// current value (declaration order) so the function yields its declared
    /// result followed by one result per inout param. Empty for non-inout
    /// functions. Used both at the function tail and before every early exit.
    fn inout_epilogue(&self) -> String {
        let mut s = String::new();
        for name in &self.cur_fn_inout_params {
            s.push_str(&format!("    local.get ${name}\n"));
        }
        s
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
                    if let Some(&idx) = self.str_fields.get(name) {
                        // Assigning a String state field copies the content OUT
                        // to the host cell, so it survives the arena reset.
                        out.push_str(&format!("    i32.const {idx}\n"));
                        out.push_str(&self.compile_expr(value)?);
                        out.push_str("    call $field_str_set_host\n");
                        tail_is_value = false;
                        continue;
                    }
                    let vk = self.kind_of(value);
                    out.push_str(&self.compile_expr(value)?);
                    if self.globals.contains(name) {
                        // An Int/Subject global is i32; a Float field's f64 kind
                        // is registered in `locals`.
                        let target = self.locals.get(name).copied().unwrap_or(Kind::I32);
                        out.push_str(kind_convert(vk, target));
                        out.push_str(&format!("    global.set ${name}\n"));
                    } else {
                        let target = self.locals.get(name).copied().unwrap_or(Kind::I32);
                        out.push_str(kind_convert(vk, target));
                        out.push_str(&format!("    local.set ${name}\n"));
                    }
                    tail_is_value = false;
                }
                Stmt::LetTuple { names, value } => {
                    // Evaluate the tuple once into a scratch local, then load each
                    // 8-byte slot (at offset 4 + 8*i) and recover each binding's
                    // kind from the universal i64 slot rep.
                    out.push_str(&self.compile_expr(value)?);
                    out.push_str(&format!("    local.set ${TUPLE_TMP}\n"));
                    for (i, name) in names.iter().enumerate() {
                        let offset = 4 + 8 * i;
                        let k = self.locals.get(name).copied().unwrap_or(Kind::I32);
                        out.push_str(&format!(
                            "    local.get ${TUPLE_TMP}\n    i32.const {offset}\n    i32.add\n    i64.load\n{}    local.set ${name}\n",
                            from_slot(k)
                        ));
                    }
                    tail_is_value = false;
                }
                Stmt::Return(opt) => {
                    let value = match opt {
                        Some(e) => {
                            let ek = self.kind_of(e);
                            let conv = if self.cur_fn_ret_slot {
                                to_slot(ek).to_string()
                            } else {
                                kind_convert(ek, self.cur_fn_ret_kind).to_string()
                            };
                            format!("{}{conv}", self.compile_expr(e)?)
                        }
                        None if self.cur_fn_ret_slot => "    i64.const 0\n".to_string(),
                        None => format!("    {}.const 0\n", wasm_ty(self.cur_fn_ret_kind)),
                    };
                    out.push_str(&value);
                    // `inout` functions return extra results (one per inout param);
                    // reproduce the epilogue here so an early return is well-formed.
                    out.push_str(&self.inout_epilogue());
                    out.push_str("    return\n");
                    // Anything after a `return` in this block is unreachable.
                    tail_is_value = false;
                }
                Stmt::Break | Stmt::Continue => {
                    let Some((brk, cont)) = self.loop_labels.last() else {
                        return cerr("`break`/`continue` outside a loop");
                    };
                    let label = if matches!(stmt, Stmt::Break) { brk } else { cont };
                    out.push_str(&format!("    br {label}\n"));
                    tail_is_value = false;
                }
                Stmt::Expr(e) | Stmt::Yield(e) => {
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
            Expr::Range { .. } | Expr::Index { .. } | Expr::WhileLet { .. } | Expr::MethodCall { .. } | Expr::Record { .. } => {
                unreachable!("range/index sugar is lowered before codegen (parser::lower_sugar_module)")
            }
            Expr::Int(n) | Expr::Duration(n) => Ok(format!("    i64.const {n}\n")),
            Expr::Bool(b) => Ok(format!("    i32.const {}\n", if *b { 1 } else { 0 })),
            Expr::Str(s) => {
                let off = self.intern(s);
                Ok(format!("    i32.const {off}\n"))
            }
            Expr::Var(name) => {
                if self.cap_fields.contains(name) {
                    Ok("    i32.const 0\n".to_string())
                } else if let Some(&idx) = self.str_fields.get(name) {
                    // A String state field: a fresh arena copy of the host cell.
                    Ok(format!("    i32.const {idx}\n    call $field_str_get\n"))
                } else if self.globals.contains(name) {
                    Ok(format!("    global.get ${name}\n"))
                } else if !self.locals.contains_key(name) {
                    // A bare top-level function name used as a value: materialize
                    // it as a forwarding closure `fn(p..) { name(p..) }`, reusing
                    // the lambda machinery (a fresh table slot + `[code_index]`
                    // record). Locals shadow functions, so this only fires when
                    // `name` is not a local binding.
                    let Some(params) = self.fn_params.get(name).cloned() else {
                        return Ok(format!("    local.get ${name}\n"));
                    };
                    let args = params.iter().map(|p| Expr::Var(p.name.clone())).collect();
                    let body = Block {
                        stmts: vec![Stmt::Expr(Expr::Call {
                            name: name.clone(),
                            args,
                        })],
                        lines: vec![0],
                        restrict: None,
                    };
                    self.compile_lambda(&params, &body)
                } else {
                    Ok(format!("    local.get ${name}\n"))
                }
            }
            Expr::Unary { op, expr } => match op {
                // `move x` is value-neutral on WASM (value semantics throughout) —
                // just compile the operand.
                UnOp::Move => self.compile_expr(expr),
                UnOp::Not => Ok(format!("{}    i32.eqz\n", self.compile_expr(expr)?)),
                UnOp::Neg => match self.kind_of(expr) {
                    Kind::F64 => Ok(format!("{}    f64.neg\n", self.compile_expr(expr)?)),
                    Kind::I64 => {
                        Ok(format!("    i64.const 0\n{}    i64.sub\n", self.compile_expr(expr)?))
                    }
                    Kind::I32 => {
                        Ok(format!("    i32.const 0\n{}    i32.sub\n", self.compile_expr(expr)?))
                    }
                },
                // ~x == x ^ -1 (all bits set); Int is i64.
                UnOp::BitNot => {
                    let p = wasm_ty(self.kind_of(expr));
                    Ok(format!("{}    {p}.const -1\n    {p}.xor\n", self.compile_expr(expr)?))
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
                // to i32 comparison, as before. We check BOTH operands — the type
                // checker guarantees they share a type, so a literal on either
                // side (e.g. `at(to_chars(s), i) == " "`, where the element type
                // isn't tracked locally) is enough to pick structural equality.
                if matches!(
                    op,
                    BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
                ) && (self.val_type_of(lhs) == ValType::Str
                    || self.val_type_of(rhs) == ValType::Str)
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
                // Compound (`List`/`Tuple`/record) operands: a bare `i32.eq` would
                // compare heap pointers, not contents. Equality is done with a
                // structural helper specialized to the operands' shape; ordering a
                // compound is a runtime error on the interpreter, so reject it.
                if let Some(shape) = self.eq_shape_of(lhs).or_else(|| self.eq_shape_of(rhs)) {
                    if shape.is_compound() {
                        match op {
                            BinOp::Eq | BinOp::NotEq => {
                                let h = self.ensure_eq_helper(&shape)?;
                                let l = self.compile_expr(lhs)?;
                                let r = self.compile_expr(rhs)?;
                                let eq = format!("{l}{r}    call ${h}\n");
                                return Ok(if matches!(op, BinOp::NotEq) {
                                    format!("{eq}    i32.eqz\n")
                                } else {
                                    eq
                                });
                            }
                            _ => {
                                return cerr(
                                    "ordering (`<`/`<=`/`>`/`>=`) is not defined for compound values (lists, tuples, records) — the interpreter errors on it too",
                                );
                            }
                        }
                    }
                }
                // A `Dict` operand would otherwise compare heap pointers; structural
                // dict equality (order-sensitive over key/value slots) isn't compiled
                // yet, so reject it loudly rather than diverge silently.
                if matches!(op, BinOp::Eq | BinOp::NotEq)
                    && (self.is_dict_operand(lhs) || self.is_dict_operand(rhs))
                {
                    return cerr(
                        "`==` on a Dict is not yet compiled to WASM (structural dict equality) — compare `pairs(d)` or specific keys instead",
                    );
                }
                // Promote both operands to a common kind so a concrete i64 Int
                // and an i32 (generic/narrowed) operand don't clash: f64 if either
                // is Float, else i64 if either is i64, else i32. Comparisons still
                // produce an i32 bool.
                let lk = self.kind_of(lhs);
                let rk = self.kind_of(rhs);
                let ck = if lk == Kind::F64 || rk == Kind::F64 {
                    Kind::F64
                } else if lk == Kind::I64 || rk == Kind::I64 {
                    Kind::I64
                } else {
                    Kind::I32
                };
                let p = wasm_ty(ck);
                let float = ck == Kind::F64;
                let l = format!("{}{}", self.compile_expr(lhs)?, kind_convert(lk, ck));
                let r = format!("{}{}", self.compile_expr(rhs)?, kind_convert(rk, ck));
                let opcode: String = match op {
                    BinOp::Add => format!("{p}.add"),
                    BinOp::Sub => format!("{p}.sub"),
                    BinOp::Mul => format!("{p}.mul"),
                    BinOp::Div => {
                        if float {
                            "f64.div".to_string()
                        } else {
                            format!("{p}.div_s")
                        }
                    }
                    BinOp::Mod => format!("{p}.rem_s"),
                    BinOp::Eq => format!("{p}.eq"),
                    BinOp::NotEq => format!("{p}.ne"),
                    // Float ordering goes through a NaN-trapping helper (the
                    // interpreter errors on a NaN comparison); integer ordering is
                    // a plain signed compare.
                    BinOp::Lt => {
                        if float {
                            self.uses_float_ord = true;
                            "call $f_lt".to_string()
                        } else {
                            format!("{p}.lt_s")
                        }
                    }
                    BinOp::LtEq => {
                        if float {
                            self.uses_float_ord = true;
                            "call $f_le".to_string()
                        } else {
                            format!("{p}.le_s")
                        }
                    }
                    BinOp::Gt => {
                        if float {
                            self.uses_float_ord = true;
                            "call $f_gt".to_string()
                        } else {
                            format!("{p}.gt_s")
                        }
                    }
                    BinOp::GtEq => {
                        if float {
                            self.uses_float_ord = true;
                            "call $f_ge".to_string()
                        } else {
                            format!("{p}.ge_s")
                        }
                    }
                    // Bitwise ops are Int-only -> i64.
                    BinOp::BitAnd => format!("{p}.and"),
                    BinOp::BitOr => format!("{p}.or"),
                    BinOp::BitXor => format!("{p}.xor"),
                    BinOp::Shl => format!("{p}.shl"),
                    BinOp::Shr => format!("{p}.shr_s"),
                    BinOp::Concat | BinOp::And | BinOp::Or => unreachable!("handled above"),
                };
                Ok(format!("{l}{r}    {opcode}\n"))
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                // With an `else`, the `if` yields the branches' value. The two
                // branches can compile to different kinds for one source type, so
                // promote to their common kind and convert each. Without an else
                // it is used for effect (Nil); yield i32 0.
                match else_block {
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
                        let then_wat =
                            format!("{}{}", self.compile_block(then_block)?, kind_convert(tk, ck));
                        let else_wat = format!("{}{}", self.compile_block(eb)?, kind_convert(ek, ck));
                        Ok(format!(
                            "{}    if (result {})\n{then_wat}    else\n{else_wat}    end\n",
                            self.compile_expr(cond)?,
                            wasm_ty(ck),
                        ))
                    }
                    None => Ok(format!(
                        "{}    if (result i32)\n{}    else\n    i32.const 0\n    end\n",
                        self.compile_expr(cond)?,
                        self.compile_block(then_block)?,
                    )),
                }
            }
            Expr::Block(b) => self.compile_block(b),
            Expr::While { cond, body } => {
                let id = self.next_label;
                self.next_label += 1;
                let c = self.compile_expr(cond)?;
                // `break` exits to $we{id}; `continue` re-enters $wl{id}, which
                // re-checks the condition.
                self.loop_labels.push((format!("$we{id}"), format!("$wl{id}")));
                let b = self.compile_block(body)?;
                self.loop_labels.pop();
                Ok(format!(
                    "    block $we{id}\n    loop $wl{id}\n{c}    i32.eqz\n    br_if $we{id}\n{b}    drop\n    br $wl{id}\n    end\n    end\n    i32.const 0\n"
                ))
            }
            Expr::Apply { func, args } => {
                // Call a function value produced by an expression. Stash the
                // closure pointer in this level's scratch local, then build the
                // indirect-call stack: env, args..., code index. Arguments use
                // the next level so a nested application can't clobber the
                // pointer between its two reads.
                let level = self.apply_level;
                if level >= APPLY_POOL {
                    return cerr("function application nested too deeply in arguments to compile");
                }
                let n = args.len();
                let tmp = format!("__witchy_call_{level}");
                let fcode = self.compile_expr(func)?;
                self.apply_level = level + 1;
                let mut argcode = String::new();
                for a in args {
                    let ak = self.kind_of(a);
                    argcode.push_str(&self.compile_expr(a)?);
                    // Pass each arg in the universal i64 slot (the closure ABI).
                    argcode.push_str(to_slot(ak));
                }
                self.apply_level = level;
                self.clos_arities.insert(n);
                // The call returns the universal i64 slot; recover it at the
                // closure's return kind.
                let recover = from_slot(self.apply_ret_kind(func));
                Ok(format!(
                    "{fcode}    local.set ${tmp}\n    local.get ${tmp}\n{argcode}    local.get ${tmp}\n    i32.load\n    call_indirect (type $clos{n})\n{recover}"
                ))
            }
            Expr::Call { name, args } => self.compile_call(name, args),
            Expr::Float(x) => Ok(format!("    f64.const {x}\n")),
            Expr::Tuple(items) => {
                // A tuple is a heap record [0][elem0][elem1]...] (a 0 tag, then
                // the elements), reusing the constructor allocator. Each element
                // occupies an 8-byte slot holding the universal i64 rep.
                let n = items.len();
                self.mk_arities.insert(n);
                let mut out = String::from("    i32.const 0\n");
                for item in items {
                    let k = self.kind_of(item);
                    out.push_str(&self.compile_expr(item)?);
                    out.push_str(to_slot(k));
                }
                out.push_str(&format!("    call $mk{n}\n"));
                Ok(out)
            }
            // `e as T` narrows a capability (type-level only); capabilities are
            // interpreter-only, so this compiles its operand unchanged.
            Expr::As { expr, .. } => self.compile_expr(expr),
            Expr::Try(inner) => {
                // The type checker guarantees `inner` is a Result/Option, whose
                // success variant (Ok/Some) is tag 0 carrying one payload. So:
                // if tag==0, take the payload; otherwise early-return the whole
                // value (the Err/None) — which needs the function's `return`.
                // The payload occupies the first 8-byte slot (at +4), stored in the
                // universal i64 rep. Recover it at the payload's kind: an Int
                // payload stays i64 (no truncation), a pointer payload wraps to
                // i32. The success branch yields that kind; the error branch
                // early-returns the whole value, then a typed zero satisfies the
                // block's result type (dead after `return`).
                let payload_kind = self
                    .match_payload_valtype(inner)
                    .map(valtype_kind)
                    .unwrap_or(Kind::I32);
                let result_ty = wasm_ty(payload_kind);
                let recover = from_slot(payload_kind);
                let zero = match payload_kind {
                    Kind::I64 => "i64.const 0",
                    Kind::F64 => "f64.const 0",
                    Kind::I32 => "i32.const 0",
                };
                let v = self.compile_expr(inner)?;
                // In an `inout` function the early return must also push each inout
                // param (after the error value) to match the multi-result epilogue.
                let epilogue = self.inout_epilogue();
                Ok(format!(
                    "{v}    local.set ${TRY_TMP}\n    \
                     local.get ${TRY_TMP}\n    i32.load\n    i32.eqz\n    \
                     if (result {result_ty})\n    \
                     local.get ${TRY_TMP}\n    i32.const 4\n    i32.add\n    i64.load\n{recover}    \
                     else\n    local.get ${TRY_TMP}\n{epilogue}    return\n    {zero}\n    end\n"
                ))
            }
            Expr::For { var, iter, body } if matches!(iter.as_ref(), Expr::Range { .. }) => {
                // Iterate a range by counting — never materialize a list. An i64
                // counter and end bound live in scratch locals; each step binds
                // the loop var to the counter, runs the body (its value dropped),
                // then advances. `break` -> $fe (exit), `continue` -> $fc (the
                // block around the body; falls through to the advance, so a
                // continued iteration still advances). For an inclusive range we
                // break when the counter reaches the end *before* incrementing,
                // so `0..=i64::MAX` halts instead of overflowing/looping forever;
                // exclusive ranges never reach an overflowing counter.
                let Expr::Range { lo, hi, inclusive } = iter.as_ref() else {
                    unreachable!("guarded by the match arm")
                };
                let id = self.next_label;
                self.next_label += 1;
                let ctr = format!("__forctr_{var}");
                let end = format!("__forend_{var}");
                let lo_wat = self.compile_expr(lo)?;
                let hi_wat = self.compile_expr(hi)?;
                let exit_cmp = if *inclusive { "i64.gt_s" } else { "i64.ge_s" };
                let guard_max = if *inclusive {
                    format!(
                        "    local.get ${ctr}\n    local.get ${end}\n    i64.eq\n    br_if $fe{id}\n"
                    )
                } else {
                    String::new()
                };
                self.loop_labels.push((format!("$fe{id}"), format!("$fc{id}")));
                let body_wat = self.compile_block(body)?;
                self.loop_labels.pop();
                Ok(format!(
                    "{lo_wat}    local.set ${ctr}\n\
                     {hi_wat}    local.set ${end}\n    \
                     block $fe{id}\n    loop $fl{id}\n    \
                     local.get ${ctr}\n    local.get ${end}\n    {exit_cmp}\n    br_if $fe{id}\n    \
                     local.get ${ctr}\n    local.set ${var}\n    \
                     block $fc{id}\n{body_wat}    drop\n    end\n\
                     {guard_max}    \
                     local.get ${ctr}\n    i64.const 1\n    i64.add\n    local.set ${ctr}\n    \
                     br $fl{id}\n    end\n    end\n    i32.const 0\n"
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
                // If iterating a `List(Record)` (a variable, a list literal, or a
                // call returning one), the loop var is that record, so `x.field`
                // in the body resolves.
                if let Some(elem) = self.elem_record_type_of(iter) {
                    self.local_records.insert(var.clone(), elem);
                }
                let elem_from = from_slot(self.iter_elem_kind(iter));
                // `break` branches to $fe{id} (loop exit); `continue` to $fc{id}
                // (an inner block around the body, after which the index advances).
                self.loop_labels.push((format!("$fe{id}"), format!("$fc{id}")));
                let body_wat = self.compile_block(body)?;
                self.loop_labels.pop();
                Ok(format!(
                    "{iter_wat}    local.set ${list_l}\n    \
                     i32.const 0\n    local.set ${idx_l}\n    \
                     block $fe{id}\n    loop $fl{id}\n    \
                     local.get ${idx_l}\n    local.get ${list_l}\n    i32.load\n    i32.ge_s\n    br_if $fe{id}\n    \
                     local.get ${list_l}\n    i32.const 4\n    i32.add\n    local.get ${idx_l}\n    i32.const 8\n    i32.mul\n    i32.add\n    i64.load\n{elem_from}    local.set ${var}\n\
                     block $fc{id}\n{body_wat}    drop\n    end\n    \
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
                let field_kind = name_kind(names[idx].1.as_deref());
                let offset = 4 + 8 * idx;
                let base_wat = self.compile_expr(base)?;
                Ok(format!(
                    "{base_wat}    i32.const {offset}\n    i32.add\n    i64.load\n{}",
                    from_slot(field_kind)
                ))
            }
            Expr::RecordUpdate { base, fields } => {
                // Build a fresh record: push the tag, then each field — the
                // override expression where given, else a load from the base.
                let Some(tyname) = self.record_type_of(base) else {
                    return cerr("cannot determine the record type for this `update`");
                };
                let names = self.record_fields[&tyname].clone();
                let (tag, nfields) = self.ctors[&tyname];
                self.mk_arities.insert(nfields);
                // The base is read once per non-overridden field. A bare variable
                // is re-read directly; any other base expression is evaluated once
                // into a level-scoped scratch local (the same pool `Apply` uses),
                // with override expressions compiled at the next level so a nested
                // update can't clobber it.
                let prelude;
                let load_base;
                let mut restore_level = None;
                if let Expr::Var(v) = base.as_ref() {
                    prelude = String::new();
                    load_base = format!("    local.get ${v}\n");
                } else {
                    let level = self.apply_level;
                    if level >= APPLY_POOL {
                        return cerr("record update nested too deeply to compile");
                    }
                    let tmp = format!("__witchy_call_{level}");
                    prelude = format!("{}    local.set ${tmp}\n", self.compile_expr(base)?);
                    load_base = format!("    local.get ${tmp}\n");
                    self.apply_level = level + 1;
                    restore_level = Some(level);
                }
                let mut out = format!("{prelude}    i32.const {tag}\n");
                for (i, (fname, _)) in names.iter().enumerate() {
                    if let Some((_, vexpr)) = fields.iter().find(|(n, _)| n == fname) {
                        let k = self.kind_of(vexpr);
                        out.push_str(&self.compile_expr(vexpr)?);
                        out.push_str(to_slot(k));
                    } else {
                        // Preserved field: copy the raw 8-byte slot straight across
                        // (already in the universal i64 rep).
                        let offset = 4 + 8 * i;
                        out.push_str(&format!(
                            "{load_base}    i32.const {offset}\n    i32.add\n    i64.load\n"
                        ));
                    }
                }
                if let Some(level) = restore_level {
                    self.apply_level = level;
                }
                out.push_str(&format!("    call $mk{nfields}\n"));
                Ok(out)
            }
            Expr::Lambda { params, body } => self.compile_lambda(params, body),
            Expr::List(items) => {
                // A list is a record [len][elem0..]; reuse the $mk{N} helper with
                // the length as the (i32) header. Each element is an 8-byte slot
                // holding the universal i64 rep, so floats now fit too.
                let n = items.len();
                self.mk_arities.insert(n);
                let mut out = format!("    i32.const {n}\n");
                for item in items {
                    let k = self.kind_of(item);
                    out.push_str(&self.compile_expr(item)?);
                    out.push_str(to_slot(k));
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
                    let k = self.kind_of(arg);
                    out.push_str(&self.compile_expr(arg)?);
                    out.push_str(to_slot(k));
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
        // The scrutinee is stored in the universal i64 rep (MATCH_TMP is i64);
        // each pattern recovers the kind it needs from it.
        let scrut_kind = self.kind_of(scrutinee);
        let scrut_setup = format!(
            "{}{}    local.set ${MATCH_TMP}\n",
            self.compile_expr(scrutinee)?,
            to_slot(scrut_kind)
        );
        let scrut = format!("    local.get ${MATCH_TMP}\n");
        let id = self.next_label;
        self.next_label += 1;
        // Arms can compile to different kinds for the same source type (an Int
        // built from a literal is i64; one built only from narrowed i32 pattern
        // vars is i32), so the block result is their promoted common kind and
        // each arm body is converted to it.
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
        let result_ty = wasm_ty(result_kind);
        // Each arm is a block: test the pattern (skip on failure), bind, test the
        // guard (skip on failure), run the body and branch out with its value.
        let mut s = scrut_setup;
        s.push_str(&format!("    block $d{id} (result {result_ty})\n"));
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
            let body_kind = self.kind_of(&arm.body);
            s.push_str(&self.compile_expr(&arm.body)?);
            s.push_str(kind_convert(body_kind, result_kind));
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
        // `value` always produces the matched value in the universal i64 rep.
        // Scalar patterns (Int/Bool) compare as i64; pointer patterns first wrap
        // it to an i32 address (`ptr`); field/element reads load the raw i64 slot
        // and let the recursive sub-pattern recover its kind.
        let ptr = format!("{value}    i32.wrap_i64\n");
        Ok(match pat {
            Pattern::Wildcard => (TRUE.to_string(), String::new()),
            Pattern::Int(k) => (format!("{value}    i64.const {k}\n    i64.eq\n"), String::new()),
            Pattern::Bool(b) => (
                format!("{value}    i64.const {}\n    i64.eq\n", if *b { 1 } else { 0 }),
                String::new(),
            ),
            Pattern::Var(name) => {
                let k = self.locals.get(name).copied().unwrap_or(Kind::I32);
                (
                    TRUE.to_string(),
                    format!("{value}{}    local.set ${name}\n", from_slot(k)),
                )
            }
            Pattern::Tuple(pats) => {
                // A tuple is `[0][elem0][elem1]...`; there's no tag to check
                // (tuples always match by shape), so the condition is just the
                // AND of the element-pattern conditions.
                let mut elem_conds = Vec::new();
                let mut binds = String::new();
                for (i, sub) in pats.iter().enumerate() {
                    let elem_value =
                        format!("{ptr}    i32.const {}\n    i32.add\n    i64.load\n", 4 + 8 * i);
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
                let len_check = format!("{ptr}    i32.load\n    i32.const {n}\n    {len_cmp}\n");
                let mut elem_conds = Vec::new();
                let mut binds = String::new();
                for (i, sub) in elems.iter().enumerate() {
                    let elem_value =
                        format!("{ptr}    i32.const {}\n    i32.add\n    i64.load\n", 4 + 8 * i);
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
                        "{ptr}    i32.const {n}\n    call $list_drop\n    local.set ${name}\n"
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
                    format!("{ptr}    i32.const {off}\n    call $str_eq\n"),
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
                        format!("{ptr}    i32.const {}\n    i32.add\n    i64.load\n", 4 + 8 * i);
                    let (sub_cond, sub_binds) = self.pattern_match(&field_value, sub)?;
                    if sub_cond != TRUE {
                        field_conds.push(sub_cond);
                    }
                    binds.push_str(&sub_binds);
                }
                // Only inspect fields once the tag has matched (short-circuit).
                let inner = and_chain(&field_conds);
                let cond = format!(
                    "{ptr}    i32.load\n    i32.const {tag}\n    i32.eq\n    if (result i32)\n{inner}    else\n    i32.const 0\n    end\n"
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
        // Keep only names bound in the enclosing scope (locals or actor-state
        // globals). Called names that are top-level functions or builtins are
        // not captured — they compile to direct calls.
        let captures: Vec<String> = scan
            .captures()
            .into_iter()
            .filter(|c| self.locals.contains_key(c) || self.globals.contains(c))
            .collect();
        // Resolve each capture against the *enclosing* scope (before the local
        // tables are swapped out for the lambda body).
        let mut cap_info: Vec<CaptureInfo> = Vec::new();
        for c in &captures {
            // A capture keeps its registered kind: an Int local survives as
            // i64 in its 8-byte env slot, a Float field (a global whose f64
            // kind lives in `locals`) as f64; other globals are i32.
            let kind = self.locals.get(c).copied().unwrap_or(Kind::I32);
            cap_info.push((
                c.clone(),
                self.globals.contains(c),
                self.local_records.get(c).cloned(),
                self.local_list_elem.get(c).cloned(),
                kind,
            ));
        }

        // Reserve this lambda's table slot *before* compiling the body, so any
        // nested lambdas take the following slots rather than colliding.
        let index = self.lambdas.len();
        self.lambdas.push(String::new());

        // The lambda body compiles in a fresh local scope (its params, the
        // captured locals, and any lets).
        let saved = self.swap_out_scope();
        self.cur_fn_inout = false;
        self.cur_fn_inout_params = Vec::new();

        for p in params {
            // Closures use the i32 generic ABI for every parameter (an Int arg
            // is narrowed at the call_indirect boundary), so the body sees the
            // param as i32.
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
        }
        // Captured names are locals of the lifted function; carry over their
        // record / list-element types so field and loop resolution still work.
        for (name, _, rec, list_elem, kind) in &cap_info {
            // A capture keeps its real kind (an Int capture stays i64): it is
            // stored into and recovered from the env record's universal i64 slot.
            self.locals.insert(name.clone(), *kind);
            if let Some(r) = rec {
                self.local_records.insert(name.clone(), r.clone());
            }
            if let Some(e) = list_elem {
                self.local_list_elem.insert(name.clone(), e.clone());
            }
        }
        // Lambda parameters: type them so the body uses the right width (an Int
        // param is i64), and track a fn-typed param's return kind.
        for p in params {
            let k = p.ty.as_ref().map(ty_kind).unwrap_or(Kind::I32);
            self.locals.insert(p.name.clone(), k);
            if let Some(t) = &p.ty {
                self.local_val_types.insert(p.name.clone(), ty_to_valtype(t));
            }
            if let Some(Type::Fn(_, ret)) = &p.ty {
                self.local_fn_ret_kind.insert(p.name.clone(), ty_kind(ret));
            }
        }
        self.infer_locals(body);
        // The closure result is the universal i64 slot: the body's tail value (and
        // any `return`) is stored via `to_slot`, and each `Apply` recovers it at
        // the closure's return kind. This keeps a big `Int` return from truncating
        // to the old i32 closure-result ABI. (Params stay i32.)
        self.cur_fn_ret_kind = Kind::I64;
        self.cur_fn_ret_slot = true;

        // Value params arrive in the universal i64 slot (the closure-call ABI);
        // a prologue recovers each into its named local at the right width.
        let mut header = format!("  (func $__lam{index} (param ${ENV_PARAM} i32) ");
        for p in params {
            header.push_str(&format!("(param $__lp_{} i64) ", p.name));
        }
        header.push_str("(result i64)\n");
        // Locals: params, captured values, `let` bindings, then scratch.
        for p in params {
            let k = self.locals.get(&p.name).copied().unwrap_or(Kind::I32);
            header.push_str(&format!("    (local ${} {})\n", p.name, wasm_ty(k)));
        }
        for (name, _, _, _, kind) in &cap_info {
            header.push_str(&format!("    (local ${name} {})\n", wasm_ty(*kind)));
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
        header.push_str(&format!("    (local ${MATCH_TMP} i64)\n"));
        for i in 0..APPLY_POOL {
            header.push_str(&format!("    (local $__witchy_call_{i} i32)\n"));
        }
        // Prologue: copy each capture out of the environment record (slot j is an
        // 8-byte slot at offset 4 + 8*j, past the i32 code-index header), then
        // recover its kind from the universal i64 slot rep.
        let mut prologue = String::new();
        // Recover each value param from its i64 slot into the named local.
        for p in params {
            let k = self.locals.get(&p.name).copied().unwrap_or(Kind::I32);
            prologue.push_str(&format!(
                "    local.get $__lp_{}\n{}    local.set ${}\n",
                p.name,
                from_slot(k),
                p.name
            ));
        }
        for (j, (name, _, _, _, kind)) in cap_info.iter().enumerate() {
            let offset = 4 + 8 * j;
            prologue.push_str(&format!(
                "    local.get ${ENV_PARAM}\n    i32.const {offset}\n    i32.add\n    i64.load\n{}    local.set ${name}\n",
                from_slot(*kind)
            ));
        }

        // The lifted body is its own function: application nesting restarts at 0.
        let saved_apply_level = self.apply_level;
        self.apply_level = 0;
        let body_wat = self.compile_block(body)?;
        // Store the body's tail value into the universal i64 closure-result slot.
        let body_wat = format!("{body_wat}{}", to_slot(self.block_kind(body)));
        self.apply_level = saved_apply_level;
        self.lambdas[index] = format!("{header}{prologue}{body_wat}  )\n");
        self.clos_arities.insert(params.len());

        self.restore_scope(saved);

        // Construction site: allocate `[code_index][cap0]..[capN]` via `$mkN`,
        // pushing the captures from the *enclosing* scope in slot order.
        let n = cap_info.len();
        self.mk_arities.insert(n);
        let mut out = format!("    i32.const {index}\n");
        for (name, is_global, _, _, kind) in &cap_info {
            if *is_global {
                out.push_str(&format!("    global.get ${name}\n"));
            } else {
                out.push_str(&format!("    local.get ${name}\n"));
            }
            out.push_str(to_slot(*kind));
        }
        out.push_str(&format!("    call $mk{n}\n"));
        Ok(out)
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
            inout: self.cur_fn_inout,
            inout_params: std::mem::take(&mut self.cur_fn_inout_params),
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
        self.cur_fn_inout = s.inout;
        self.cur_fn_inout_params = s.inout_params;
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

    /// The structural-equality shape of an expression, where codegen can resolve
    /// it. `None` means the shape is unknown (then compound `==` errors loudly
    /// rather than comparing pointers). Lists (any depth) come from the nesting
    /// tracker, tuples from literals or tracked tuple locals, records from
    /// `record_type_of`.
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
            if name == "insert" && args.len() == 3 {
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
        self.eq_shape_of(e).or_else(|| EqShape::scalar(self.val_type_of(e)))
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
                matches!(name.as_str(), "dict_new" | "insert" | "remove" | "update")
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
            Type::Named(n, args) => match n.as_str() {
                "Int" | "Duration" => Some(EqShape::Int),
                "Bool" => Some(EqShape::Bool),
                "Float" => Some(EqShape::Float),
                "String" => Some(EqShape::Str),
                "List" => args.first().and_then(|inner| {
                    self.eq_shape_of_type_rec(inner, subst, visiting)
                        .map(|s| EqShape::List(Box::new(s)))
                }),
                t if self.record_fields.contains_key(t) => Some(EqShape::Record(t.to_string())),
                t if self.adt_variants.contains_key(t) => {
                    // A RECURSIVE generic ADT (`Push(a, Stack(a))`) has no finite
                    // instantiated shape — its self-reference falls back to the
                    // by-name shape (loud if a type-variable field is compared),
                    // exactly the pre-instantiation behavior.
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
                    for (pn, arg) in params.iter().zip(args) {
                        match self.eq_shape_of_type_rec(arg, subst, visiting) {
                            Some(s) => {
                                inner.insert(pn.clone(), s);
                            }
                            None => return Some(EqShape::Adt(t.to_string())),
                        }
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

    /// An S-expression yielding an i32 bool: are the two 8-byte slots at the given
    /// addresses equal under `shape`? Scalars compare inline; compounds load the
    /// slot's heap pointer and call that shape's generated helper.
    fn slot_cmp(&mut self, shape: &EqShape, aa: &str, bb: &str) -> Result<String, CodegenError> {
        Ok(match shape {
            EqShape::Int | EqShape::Bool => format!("(i64.eq (i64.load {aa}) (i64.load {bb}))"),
            EqShape::Float => format!(
                "(f64.eq (f64.reinterpret_i64 (i64.load {aa})) (f64.reinterpret_i64 (i64.load {bb})))"
            ),
            EqShape::Str => {
                self.uses_str_eq = true;
                format!("(call $str_eq (i32.load {aa}) (i32.load {bb}))")
            }
            compound => {
                let h = self.ensure_eq_helper(compound)?;
                format!("(call ${h} (i32.load {aa}) (i32.load {bb}))")
            }
        })
    }

    /// Ensure a structural-equality helper exists for a compound `shape`, emitting
    /// it (and any helpers it depends on) once, and return its function name. The
    /// slot reserves its name before generating the body so a recursive record
    /// type (e.g. a tree) refers to the same helper without looping.
    fn ensure_eq_helper(&mut self, shape: &EqShape) -> Result<String, CodegenError> {
        let name = format!("eq_{}", shape.id());
        if self.eq_helpers.contains_key(&name) {
            return Ok(name);
        }
        self.eq_helpers.insert(name.clone(), String::new()); // reserve (break cycles)
        let body = match shape {
            EqShape::List(elem) => {
                let cmp = self.slot_cmp(
                    elem,
                    "(i32.add (i32.add (local.get $a) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))",
                    "(i32.add (i32.add (local.get $b) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))",
                )?;
                format!(
                    "  (func ${name} (param $a i32) (param $b i32) (result i32)\n    \
                     (local $n i32) (local $i i32)\n    \
                     (if (i32.ne (i32.load (local.get $a)) (i32.load (local.get $b))) (then (return (i32.const 0))))\n    \
                     (local.set $n (i32.load (local.get $a)))\n    \
                     (block $done (loop $l\n      \
                     (br_if $done (i32.ge_s (local.get $i) (local.get $n)))\n      \
                     (if (i32.eqz {cmp}) (then (return (i32.const 0))))\n      \
                     (local.set $i (i32.add (local.get $i) (i32.const 1)))\n      (br $l)))\n    \
                     (i32.const 1))\n"
                )
            }
            EqShape::Tuple(fields) => {
                let mut checks = String::new();
                for (i, f) in fields.iter().enumerate() {
                    let off = 4 + 8 * i;
                    let cmp = self.slot_cmp(
                        f,
                        &format!("(i32.add (local.get $a) (i32.const {off}))"),
                        &format!("(i32.add (local.get $b) (i32.const {off}))"),
                    )?;
                    checks.push_str(&format!("    (if (i32.eqz {cmp}) (then (return (i32.const 0))))\n"));
                }
                format!("  (func ${name} (param $a i32) (param $b i32) (result i32)\n{checks}    (i32.const 1))\n")
            }
            EqShape::Record(tyname) => {
                let fields = self
                    .record_field_types
                    .get(tyname)
                    .cloned()
                    .ok_or_else(|| CodegenError { message: format!("unknown record `{tyname}` in `==`") })?;
                let mut checks = String::new();
                for (i, fty) in fields.iter().enumerate() {
                    let off = 4 + 8 * i;
                    let fshape = self.eq_shape_of_type(fty).ok_or_else(|| CodegenError {
                        message: format!(
                            "`==` on `{tyname}` needs a comparable field type; field {} is not yet structurally compared on WASM — compare the fields directly or use the `Eq` trait",
                            i + 1
                        ),
                    })?;
                    let cmp = self.slot_cmp(
                        &fshape,
                        &format!("(i32.add (local.get $a) (i32.const {off}))"),
                        &format!("(i32.add (local.get $b) (i32.const {off}))"),
                    )?;
                    checks.push_str(&format!("    (if (i32.eqz {cmp}) (then (return (i32.const 0))))\n"));
                }
                format!("  (func ${name} (param $a i32) (param $b i32) (result i32)\n{checks}    (i32.const 1))\n")
            }
            EqShape::Adt(tyname) => {
                let variants = self
                    .adt_variants
                    .get(tyname)
                    .cloned()
                    .ok_or_else(|| CodegenError { message: format!("unknown type `{tyname}` in `==`") })?;
                // Tags differ -> not equal. Otherwise dispatch on the (shared) tag
                // to compare that variant's fields; nullary variants need no check.
                let mut arms = String::new();
                for (tag, fields) in variants.iter().enumerate() {
                    if fields.is_empty() {
                        continue;
                    }
                    let mut checks = String::new();
                    for (i, fty) in fields.iter().enumerate() {
                        let off = 4 + 8 * i;
                        let fshape = self.eq_shape_of_type(fty).ok_or_else(|| CodegenError {
                            message: format!(
                                "`==` on `{tyname}` needs comparable fields; a field of variant {tag} is not structurally compared on WASM (e.g. a generic payload like `Option`/`Result`) — match on it or use the `Eq` trait"
                            ),
                        })?;
                        let cmp = self.slot_cmp(
                            &fshape,
                            &format!("(i32.add (local.get $a) (i32.const {off}))"),
                            &format!("(i32.add (local.get $b) (i32.const {off}))"),
                        )?;
                        checks.push_str(&format!("      (if (i32.eqz {cmp}) (then (return (i32.const 0))))\n"));
                    }
                    arms.push_str(&format!(
                        "    (if (i32.eq (local.get $t) (i32.const {tag})) (then\n{checks}      (return (i32.const 1))))\n"
                    ));
                }
                format!(
                    "  (func ${name} (param $a i32) (param $b i32) (result i32)\n    \
                     (local $t i32)\n    \
                     (if (i32.ne (i32.load (local.get $a)) (i32.load (local.get $b))) (then (return (i32.const 0))))\n    \
                     (local.set $t (i32.load (local.get $a)))\n{arms}    \
                     (i32.const 1))\n"
                )
            }
            EqShape::AdtInst(_, variant_shapes) => {
                // Like `Adt`, but the per-variant field shapes were resolved at
                // the comparison site (a generic payload instantiated there).
                let mut arms = String::new();
                for (tag, fields) in variant_shapes.iter().enumerate() {
                    if fields.is_empty() {
                        continue;
                    }
                    let mut checks = String::new();
                    for (i, fshape) in fields.iter().enumerate() {
                        let off = 4 + 8 * i;
                        let cmp = self.slot_cmp(
                            fshape,
                            &format!("(i32.add (local.get $a) (i32.const {off}))"),
                            &format!("(i32.add (local.get $b) (i32.const {off}))"),
                        )?;
                        checks.push_str(&format!("      (if (i32.eqz {cmp}) (then (return (i32.const 0))))\n"));
                    }
                    arms.push_str(&format!(
                        "    (if (i32.eq (local.get $t) (i32.const {tag})) (then\n{checks}      (return (i32.const 1))))\n"
                    ));
                }
                format!(
                    "  (func ${name} (param $a i32) (param $b i32) (result i32)\n    \
                     (local $t i32)\n    \
                     (if (i32.ne (i32.load (local.get $a)) (i32.load (local.get $b))) (then (return (i32.const 0))))\n    \
                     (local.set $t (i32.load (local.get $a)))\n{arms}    \
                     (i32.const 1))\n"
                )
            }
            EqShape::Dict(k, v) => {
                // A dict is `[count][16-byte entries: key slot, value slot]`.
                // Insertion-order-sensitive pairwise comparison, matching the
                // interpreter's Vec<(K, V)> equality.
                let kcmp = self.slot_cmp(
                    k,
                    "(i32.add (i32.add (local.get $a) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16)))",
                    "(i32.add (i32.add (local.get $b) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16)))",
                )?;
                let vcmp = self.slot_cmp(
                    v,
                    "(i32.add (i32.add (local.get $a) (i32.const 12)) (i32.mul (local.get $i) (i32.const 16)))",
                    "(i32.add (i32.add (local.get $b) (i32.const 12)) (i32.mul (local.get $i) (i32.const 16)))",
                )?;
                format!(
                    "  (func ${name} (param $a i32) (param $b i32) (result i32)\n    \
                     (local $n i32) (local $i i32)\n    \
                     (if (i32.ne (i32.load (local.get $a)) (i32.load (local.get $b))) (then (return (i32.const 0))))\n    \
                     (local.set $n (i32.load (local.get $a)))\n    \
                     (block $done (loop $l\n      \
                     (br_if $done (i32.ge_s (local.get $i) (local.get $n)))\n      \
                     (if (i32.eqz {kcmp}) (then (return (i32.const 0))))\n      \
                     (if (i32.eqz {vcmp}) (then (return (i32.const 0))))\n      \
                     (local.set $i (i32.add (local.get $i) (i32.const 1)))\n      (br $l)))\n    \
                     (i32.const 1))\n"
                )
            }
            _ => unreachable!("scalar shapes have no helper"),
        };
        self.eq_helpers.insert(name.clone(), body);
        Ok(name)
    }

    /// Render the value in an 8-byte slot at `addr` (a WAT i32 address
    /// expression) to a String pointer, byte-identical to the interpreter's
    /// `Display`. Scalars format inline; compounds load the slot's pointer and
    /// call the per-shape `to_string` helper.
    fn render_slot(&mut self, shape: &EqShape, addr: &str) -> Result<String, CodegenError> {
        Ok(match shape {
            EqShape::Int => {
                self.uses_int_to_string = true;
                format!("(call $int_to_string (i64.load {addr}))")
            }
            EqShape::Bool => {
                let t = self.intern("true");
                let f = self.intern("false");
                format!("(select (i32.const {t}) (i32.const {f}) (i32.wrap_i64 (i64.load {addr})))")
            }
            EqShape::Float => {
                self.uses_float_to_str = true;
                format!("(call $float_to_str (f64.reinterpret_i64 (i64.load {addr})))")
            }
            // A string slot already holds the string pointer; render unquoted, as
            // the interpreter's Display does inside a compound (`[a, b]`).
            EqShape::Str => format!("(i32.load {addr})"),
            compound => {
                let h = self.ensure_ts_helper(compound)?;
                format!("(call ${h} (i32.load {addr}))")
            }
        })
    }

    /// Ensure a `to_string` renderer exists for compound `shape`, emitting it
    /// (and any nested-shape renderers it needs) once, and return its function
    /// name. The reserve-before-body trick mirrors `ensure_eq_helper`, so a
    /// recursive type refers to the same helper without looping.
    fn ensure_ts_helper(&mut self, shape: &EqShape) -> Result<String, CodegenError> {
        self.uses_concat = true;
        let name = format!("ts_{}", shape.id());
        if self.ts_helpers.contains_key(&name) {
            return Ok(name);
        }
        self.ts_helpers.insert(name.clone(), String::new()); // reserve (break cycles)
        let body = match shape {
            EqShape::List(elem) => {
                let open = self.intern("[");
                let close = self.intern("]");
                let comma = self.intern(", ");
                let render = self.render_slot(
                    elem,
                    "(i32.add (i32.add (local.get $p) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))",
                )?;
                format!(
                    "  (func ${name} (param $p i32) (result i32)\n    \
                     (local $n i32) (local $i i32) (local $acc i32)\n    \
                     (local.set $n (i32.load (local.get $p)))\n    \
                     (local.set $acc (i32.const {open}))\n    \
                     (block $done (loop $l\n      \
                     (br_if $done (i32.ge_s (local.get $i) (local.get $n)))\n      \
                     (if (i32.gt_s (local.get $i) (i32.const 0)) (then (local.set $acc (call $concat (local.get $acc) (i32.const {comma})))))\n      \
                     (local.set $acc (call $concat (local.get $acc) {render}))\n      \
                     (local.set $i (i32.add (local.get $i) (i32.const 1)))\n      (br $l)))\n    \
                     (call $concat (local.get $acc) (i32.const {close})))\n"
                )
            }
            EqShape::Tuple(fields) => {
                let open = self.intern("(");
                let close = self.intern(")");
                let comma = self.intern(", ");
                let mut parts = String::new();
                for (i, f) in fields.iter().enumerate() {
                    let off = 4 + 8 * i;
                    let render =
                        self.render_slot(f, &format!("(i32.add (local.get $p) (i32.const {off}))"))?;
                    if i > 0 {
                        parts.push_str(&format!(
                            "    (local.set $acc (call $concat (local.get $acc) (i32.const {comma})))\n"
                        ));
                    }
                    parts.push_str(&format!(
                        "    (local.set $acc (call $concat (local.get $acc) {render}))\n"
                    ));
                }
                format!(
                    "  (func ${name} (param $p i32) (result i32)\n    \
                     (local $acc i32)\n    \
                     (local.set $acc (i32.const {open}))\n{parts}    \
                     (call $concat (local.get $acc) (i32.const {close})))\n"
                )
            }
            EqShape::Record(tyname) => {
                // A record renders as `Name(f1, f2, ...)`, exactly like the
                // single ctor it lowers to at runtime.
                let fields = self.record_field_types.get(tyname).cloned().ok_or_else(|| {
                    CodegenError { message: format!("unknown record `{tyname}` in `to_string`") }
                })?;
                let header = self.intern(&format!("{tyname}("));
                let close = self.intern(")");
                let comma = self.intern(", ");
                let mut parts = String::new();
                for (i, fty) in fields.iter().enumerate() {
                    let off = 4 + 8 * i;
                    let fshape = self.eq_shape_of_type(fty).ok_or_else(|| CodegenError {
                        message: format!(
                            "`to_string` on `{tyname}` needs a renderable field type; field {} is not yet structurally rendered on WASM — implement `Show` for `{tyname}` or interpolate the fields directly",
                            i + 1
                        ),
                    })?;
                    let render = self
                        .render_slot(&fshape, &format!("(i32.add (local.get $p) (i32.const {off}))"))?;
                    if i > 0 {
                        parts.push_str(&format!(
                            "    (local.set $acc (call $concat (local.get $acc) (i32.const {comma})))\n"
                        ));
                    }
                    parts.push_str(&format!(
                        "    (local.set $acc (call $concat (local.get $acc) {render}))\n"
                    ));
                }
                format!(
                    "  (func ${name} (param $p i32) (result i32)\n    \
                     (local $acc i32)\n    \
                     (local.set $acc (i32.const {header}))\n{parts}    \
                     (call $concat (local.get $acc) (i32.const {close})))\n"
                )
            }
            EqShape::Adt(tyname) => {
                let variants = self.adt_variants.get(tyname).cloned().ok_or_else(|| {
                    CodegenError { message: format!("unknown type `{tyname}` in `to_string`") }
                })?;
                let names = self.adt_variant_names.get(tyname).cloned().ok_or_else(|| {
                    CodegenError { message: format!("unknown type `{tyname}` in `to_string`") }
                })?;
                let resolved: Vec<Vec<EqShape>> = variants
                    .iter()
                    .enumerate()
                    .map(|(tag, fields)| {
                        fields
                            .iter()
                            .map(|fty| {
                                self.eq_shape_of_type(fty).ok_or_else(|| CodegenError {
                                    message: format!(
                                        "`to_string` on `{tyname}` needs renderable fields; a field of variant `{}` is generic on WASM — implement `Show` for `{tyname}` or match and interpolate the fields",
                                        names.get(tag).map(|s| s.as_str()).unwrap_or("?")
                                    ),
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.ts_adt_body(&name, &names, &resolved)?
            }
            EqShape::AdtInst(tyname, variant_shapes) => {
                let names = self.adt_variant_names.get(tyname).cloned().ok_or_else(|| {
                    CodegenError { message: format!("unknown type `{tyname}` in `to_string`") }
                })?;
                self.ts_adt_body(&name, &names, variant_shapes)?
            }
            EqShape::Dict(k, v) => {
                // A dict renders as `{k: v, ...}` over its `[count][key slot,
                // value slot]...` entries, matching the interpreter's order.
                let open = self.intern("{");
                let close = self.intern("}");
                let comma = self.intern(", ");
                let colon = self.intern(": ");
                let krender = self.render_slot(
                    k,
                    "(i32.add (i32.add (local.get $p) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16)))",
                )?;
                let vrender = self.render_slot(
                    v,
                    "(i32.add (i32.add (local.get $p) (i32.const 12)) (i32.mul (local.get $i) (i32.const 16)))",
                )?;
                format!(
                    "  (func ${name} (param $p i32) (result i32)\n    \
                     (local $n i32) (local $i i32) (local $acc i32)\n    \
                     (local.set $n (i32.load (local.get $p)))\n    \
                     (local.set $acc (i32.const {open}))\n    \
                     (block $done (loop $l\n      \
                     (br_if $done (i32.ge_s (local.get $i) (local.get $n)))\n      \
                     (if (i32.gt_s (local.get $i) (i32.const 0)) (then (local.set $acc (call $concat (local.get $acc) (i32.const {comma})))))\n      \
                     (local.set $acc (call $concat (local.get $acc) {krender}))\n      \
                     (local.set $acc (call $concat (local.get $acc) (i32.const {colon})))\n      \
                     (local.set $acc (call $concat (local.get $acc) {vrender}))\n      \
                     (local.set $i (i32.add (local.get $i) (i32.const 1)))\n      (br $l)))\n    \
                     (call $concat (local.get $acc) (i32.const {close})))\n"
                )
            }
            _ => unreachable!("scalar shapes are rendered inline, not via a helper"),
        };
        self.ts_helpers.insert(name.clone(), body);
        Ok(name)
    }

    /// Build a sum-type `to_string` renderer body: dispatch on the tag (slot 0)
    /// to `Name` (nullary) or `Name(f1, f2, ...)`. Shared by `Adt` (field shapes
    /// from the declaration) and `AdtInst` (shapes resolved at the use site).
    fn ts_adt_body(
        &mut self,
        name: &str,
        ctor_names: &[String],
        variant_shapes: &[Vec<EqShape>],
    ) -> Result<String, CodegenError> {
        let open = self.intern("(");
        let close = self.intern(")");
        let comma = self.intern(", ");
        let mut arms = String::new();
        for (tag, fields) in variant_shapes.iter().enumerate() {
            let label = self.intern(ctor_names.get(tag).map(|s| s.as_str()).unwrap_or("?"));
            if fields.is_empty() {
                arms.push_str(&format!(
                    "    (if (i32.eq (local.get $t) (i32.const {tag})) (then (return (i32.const {label}))))\n"
                ));
                continue;
            }
            let mut parts = String::new();
            parts.push_str(&format!("      (local.set $acc (i32.const {label}))\n"));
            parts.push_str(&format!(
                "      (local.set $acc (call $concat (local.get $acc) (i32.const {open})))\n"
            ));
            for (i, fshape) in fields.iter().enumerate() {
                let off = 4 + 8 * i;
                let render =
                    self.render_slot(fshape, &format!("(i32.add (local.get $p) (i32.const {off}))"))?;
                if i > 0 {
                    parts.push_str(&format!(
                        "      (local.set $acc (call $concat (local.get $acc) (i32.const {comma})))\n"
                    ));
                }
                parts.push_str(&format!(
                    "      (local.set $acc (call $concat (local.get $acc) {render}))\n"
                ));
            }
            arms.push_str(&format!(
                "    (if (i32.eq (local.get $t) (i32.const {tag})) (then\n{parts}      (return (call $concat (local.get $acc) (i32.const {close})))))\n"
            ));
        }
        Ok(format!(
            "  (func ${name} (param $p i32) (result i32)\n    \
             (local $t i32) (local $acc i32)\n    \
             (local.set $t (i32.load (local.get $p)))\n{arms}    \
             (unreachable))\n"
        ))
    }

    /// Compile an `encoding.*` call (op-coded hex/base64 transform) to a call to
    /// the `$encoding` guest helper: push the op, then the string-argument
    /// pointer, then call.
    fn compile_encoding(&mut self, op: u32, arg: &Expr) -> Result<String, CodegenError> {
        self.uses_encoding = true;
        let s = self.compile_expr(arg)?;
        Ok(format!("    i32.const {op}\n{s}    call $encoding\n"))
    }

    fn compile_call(&mut self, name: &str, args: &[Expr]) -> Result<String, CodegenError> {
        match (name, args.len()) {
            // `crypto.ed25519_verify(pk, msg, sig) -> Bool`: a native-module
            // function bridged into the sandbox as a host import. Each string arg
            // is a single header pointer (the host reads `[len][bytes]`); the
            // result is an i32 bool. The host calls the SAME `native::lookup`
            // implementation the interpreter uses, so the backends agree.
            ("crypto.ed25519_verify", 3) => {
                self.uses_crypto_ed25519_verify = true;
                let a = self.compile_expr(&args[0])?;
                let b = self.compile_expr(&args[1])?;
                let c = self.compile_expr(&args[2])?;
                Ok(format!("{a}{b}{c}    call $crypto_ed25519_verify\n"))
            }
            // `crypto.sha256(s) -> String`: the guest helper bump-allocates the
            // fixed 68-byte result header (`[len=64][64 hex bytes]`), then the host
            // import fills the 64 bytes — returning a normal witchy string.
            ("crypto.sha256", 1) => {
                self.uses_crypto_sha256 = true;
                let s = self.compile_expr(&args[0])?;
                Ok(format!("{s}    call $crypto_sha256\n"))
            }
            // `crypto.sign(key, msg)` / `crypto.public_key(key)`: the Secret
            // argument is type-level only — the granted host key IS the key, so
            // only the message travels. Fixed-size results (128/64 hex bytes).
            ("crypto.sign", 2) => {
                self.uses_crypto_sign = true;
                let msg = self.compile_expr(&args[1])?;
                Ok(format!("{msg}    call $crypto_sign\n"))
            }
            ("crypto.public_key", 1) => {
                self.uses_crypto_public_key = true;
                Ok("    call $crypto_public_key\n".to_string())
            }
            // The `encoding` module's hex/base64 transforms (all `String ->
            // String`) bridge to the SAME native registry the interpreter uses, via
            // a host import. `op` selects the transform; the guest reserves the
            // result buffer, the host fills it and returns the length.
            ("encoding.hex_encode", 1) => self.compile_encoding(0, &args[0]),
            ("encoding.hex_decode", 1) => self.compile_encoding(1, &args[0]),
            ("encoding.base64_encode", 1) => self.compile_encoding(2, &args[0]),
            ("encoding.base64_decode", 1) => self.compile_encoding(3, &args[0]),
            // `crypto.rune_hash(paths, contents) -> String`: both args are guest
            // string lists; the host walks them and writes the fixed 71-byte
            // `sha256:<hex>` store hash into the guest-allocated result.
            ("crypto.rune_hash", 2) => {
                self.uses_crypto_rune_hash = true;
                let paths = self.compile_expr(&args[0])?;
                let contents = self.compile_expr(&args[1])?;
                Ok(format!("{paths}{contents}    call $crypto_rune_hash\n"))
            }
            // `compiler.footprint(src)` / `compiler.diff(old, new)`: pure
            // toolchain analyses returning JSON of unpredictable size — the host
            // computes and stages the result at the `_len` call, the guest
            // allocates `[len][bytes]`, and `fill_pending` writes the bytes.
            ("compiler.footprint", 1) => {
                self.uses_compiler_footprint = true;
                let src = self.compile_expr(&args[0])?;
                Ok(format!("{src}    call $compiler_footprint\n"))
            }
            ("compiler.diff", 2) => {
                self.uses_compiler_diff = true;
                let old = self.compile_expr(&args[0])?;
                let new = self.compile_expr(&args[1])?;
                Ok(format!("{old}{new}    call $compiler_diff\n"))
            }
            // Safety net: every registered native is bridged above; a future
            // native added without a bridge fails loudly here instead of
            // miscompiling as an unknown user function.
            (n, _) if crate::native::is_native(n) => {
                cerr(format!("`{n}` is not bridged into WASM yet"))
            }
            // `now(clock)`: the Clock capability argument is type-level only (like
            // print's Console); the host import is the authority, linked only when
            // the actor holds a Clock grant.
            ("now", 1) => {
                self.uses_now = true;
                Ok("    call $now_host\n".to_string())
            }
            // `get_env(env, name) -> Option(String)`: the guest helper sizes a
            // buffer from `env_len`, fills it with `env_fill`, and builds the
            // Some/None record — all under an Env grant.
            ("get_env", 2) => {
                self.uses_get_env = true;
                let name = self.compile_expr(&args[1])?;
                Ok(format!("{name}    call $get_env\n"))
            }
            // `fail(msg)`: a deliberate, loud abort — the interpreter raises the
            // message as a runtime error; compiled code traps. (Both fail.)
            ("fail", 1) => {
                let msg = self.compile_expr(&args[0])?;
                Ok(format!("{msg}    drop\n    unreachable\n"))
            }
            ("print", 2) => {
                self.uses_print = true;
                let msg = self.compile_expr(&args[1])?;
                Ok(format!("{msg}    call $print_str\n    i32.const 0\n"))
            }
            ("int_to_string", 1) => {
                self.uses_int_to_string = true;
                let ak = self.kind_of(&args[0]);
                let arg = self.compile_expr(&args[0])?;
                Ok(format!("{arg}{}    call $int_to_string\n", kind_convert(ak, Kind::I64)))
            }
            // to_string(x): render by the argument's compile-time value type. A
            // String passes through; an Int reuses `$int_to_string`; a Bool picks
            // an interned "true"/"false". Floats and undetermined types error
            // (rather than silently mis-rendering).
            ("to_string", 1) => match self.val_type_of(&args[0]) {
                ValType::Str => self.compile_expr(&args[0]),
                ValType::Int => {
                    self.uses_int_to_string = true;
                    let ak = self.kind_of(&args[0]);
                    let arg = self.compile_expr(&args[0])?;
                    Ok(format!("{arg}{}    call $int_to_string\n", kind_convert(ak, Kind::I64)))
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
                    // Format in the host (Rust `Display`), byte-identical to the
                    // interpreter; no float formatter in hand-written WAT.
                    self.uses_float_to_str = true;
                    let ak = self.kind_of(&args[0]);
                    let arg = self.compile_expr(&args[0])?;
                    Ok(format!("{arg}{}    call $float_to_str\n", kind_convert(ak, Kind::F64)))
                }
                // A compound (list/tuple/record/ADT/dict) renders via a generated
                // per-shape helper, byte-identical to the interpreter's Display —
                // so `"${xs}"` works on WASM too. Shapes the structural machinery
                // can't resolve (a generic payload) still error loudly.
                ValType::Other => match self.eq_shape_of(&args[0]) {
                    Some(shape) if shape.is_compound() => {
                        let h = self.ensure_ts_helper(&shape)?;
                        let arg = self.compile_expr(&args[0])?;
                        Ok(format!("{arg}    call ${h}\n"))
                    }
                    _ => cerr(
                        "to_string could not determine the value's type for WASM; convert it explicitly (e.g. int_to_string) or implement `Show`",
                    ),
                },
            },
            // The string record's header is its byte length (i32) -> Int (i64).
            ("string_length", 1) => {
                let arg = self.compile_expr(&args[0])?;
                Ok(format!("{arg}    i32.load\n    i64.extend_i32_u\n"))
            }
            // char_count(s): Unicode scalars in `s`. Evaluate `s` once into a
            // scratch slot, then `$byte_to_char(s, byte_length(s))`.
            ("char_count", 1) => {
                self.uses_byte_to_char = true;
                let level = self.apply_level;
                if level >= APPLY_POOL {
                    return cerr("char_count nested too deeply to compile");
                }
                let tmp = format!("__witchy_call_{level}");
                let arg = self.compile_expr(&args[0])?;
                Ok(format!(
                    "{arg}    local.set ${tmp}\n    local.get ${tmp}\n    local.get ${tmp}\n    i32.load\n    call $byte_to_char\n    i64.extend_i32_u\n"
                ))
            }
            ("int_to_float", 1) => {
                Ok(format!("{}    f64.convert_i64_s\n", self.compile_expr(&args[0])?))
            }
            ("float_to_int", 1) => {
                // Saturating (non-trapping) truncation to match the interpreter's
                // Rust `as i64`: NaN -> 0, +inf -> i64::MAX, -inf -> i64::MIN, and
                // out-of-range floats clamp. Plain `i64.trunc_f64_s` would instead
                // trap on those, diverging from the interpreter.
                Ok(format!("{}    i64.trunc_sat_f64_s\n", self.compile_expr(&args[0])?))
            }
            // Duration <-> Int(ms) is a no-op at runtime (both are i64).
            ("int_to_duration", 1) | ("duration_to_int", 1) => self.compile_expr(&args[0]),
            // sqrt(x): WASM has a native f64 square root.
            ("sqrt", 1) => Ok(format!("{}    f64.sqrt\n", self.compile_expr(&args[0])?)),
            // string_to_int(s): parse a well-formed decimal integer — optional
            // surrounding ASCII whitespace, an optional sign, then digits. The
            // interpreter trims and rejects malformed input; the compiled parser
            // matches the interpreter's `trim().parse::<i64>()` (i64 accumulation,
            // strict: traps on junk / no digits), so the backends agree.
            ("string_to_int", 1) => {
                self.uses_str_to_int = true;
                let s = self.compile_expr(&args[0])?;
                Ok(format!("{s}    call $str_to_int\n"))
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
            // string_chars(s): list of single-character strings. Built by walking
            // the chars via `$str_substring`; reuses the existing char-correct
            // helpers (no new UTF-8 logic).
            ("string_chars", 1) => {
                self.uses_str_chars = true;
                self.uses_byte_to_char = true; // counts chars
                self.uses_substring = true; // emits $char_to_byte + $str_substring
                self.uses_substr = true; // $str_substring allocates each char
                self.uses_list_push = true; // result list built with it
                let s = self.compile_expr(&args[0])?;
                Ok(format!("{s}    call $str_chars\n"))
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
                Ok(format!("{s}{sub}    call $str_index_of\n    i64.extend_i32_s\n"))
            }
            // substring(s, start, end): the half-open character range [start, end),
            // clamped to bounds (counted by Unicode scalar).
            ("substring", 3) => {
                self.uses_substring = true;
                self.uses_substr = true;
                // start/end are Int (i64) but the helper indexes with i32.
                let sk = self.kind_of(&args[1]);
                let ek = self.kind_of(&args[2]);
                let s = self.compile_expr(&args[0])?;
                let start = self.compile_expr(&args[1])?;
                let end = self.compile_expr(&args[2])?;
                Ok(format!(
                    "{s}{start}{}{end}{}    call $str_substring\n",
                    kind_convert(sk, Kind::I32),
                    kind_convert(ek, Kind::I32)
                ))
            }
            // replace(s, from, to): all non-overlapping occurrences of `from`.
            ("replace", 3) => {
                self.uses_replace = true;
                let s = self.compile_expr(&args[0])?;
                let from = self.compile_expr(&args[1])?;
                let to = self.compile_expr(&args[2])?;
                Ok(format!("{s}{from}{to}    call $replace\n"))
            }
            // trim(s): drop leading/trailing ASCII whitespace. A byte scan is
            // safe for UTF-8 since whitespace bytes never appear inside a
            // multi-byte scalar; this matches the interpreter on ASCII edges
            // (Unicode-whitespace trimming remains interpreter-only).
            ("trim", 1) => {
                self.uses_trim = true;
                self.uses_substr = true;
                let s = self.compile_expr(&args[0])?;
                Ok(format!("{s}    call $trim\n"))
            }
            // ASCII case mapping, matching the interpreter's ASCII fold.
            ("to_upper", 1) | ("to_lower", 1) => {
                self.uses_ascii_case = true;
                let up = if name == "to_upper" { 1 } else { 0 };
                let s = self.compile_expr(&args[0])?;
                Ok(format!("{s}    i32.const {up}\n    call $ascii_case\n"))
            }
            // --- Dict (immutable association map) ---
            ("dict_new", 0) => {
                self.uses_dict = true;
                self.uses_str_eq = true; // `$key_eq` references `$str_eq`
                Ok("    call $dict_new\n".to_string())
            }
            // Dicts use the i32 ABI for keys and values (a concrete i64 Int key
            // or value is narrowed at the boundary).
            ("insert", 3) => {
                let mode = self.dict_key_mode(&args[1])?;
                self.uses_dict = true;
                self.uses_str_eq = true;
                let kk = self.kind_of(&args[1]);
                let vk = self.kind_of(&args[2]);
                let d = self.compile_expr(&args[0])?;
                let k = self.compile_expr(&args[1])?;
                let v = self.compile_expr(&args[2])?;
                // Key and value go into the dict's universal i64 slots.
                Ok(format!(
                    "{d}{k}{}{v}{}    i32.const {mode}\n    call $dict_insert\n",
                    to_slot(kk),
                    to_slot(vk)
                ))
            }
            ("get_or", 3) => {
                let mode = self.dict_key_mode(&args[1])?;
                self.uses_dict = true;
                self.uses_str_eq = true;
                let kk = self.kind_of(&args[1]);
                let dk = self.kind_of(&args[2]);
                let d = self.compile_expr(&args[0])?;
                let k = self.compile_expr(&args[1])?;
                let default = self.compile_expr(&args[2])?;
                // Key + default go in as i64 slots; the i64 result is recovered at
                // the value kind (the default's kind, which shares the value type).
                Ok(format!(
                    "{d}{k}{}{default}{}    i32.const {mode}\n    call $dict_get_or\n{}",
                    to_slot(kk),
                    to_slot(dk),
                    from_slot(dk)
                ))
            }
            ("has", 2) => {
                let mode = self.dict_key_mode(&args[1])?;
                self.uses_dict = true;
                self.uses_str_eq = true;
                let kk = self.kind_of(&args[1]);
                let d = self.compile_expr(&args[0])?;
                let k = self.compile_expr(&args[1])?;
                Ok(format!(
                    "{d}{k}{}    i32.const {mode}\n    call $dict_has\n",
                    to_slot(kk)
                ))
            }
            // The single-lookup upsert: read the current value (or `default` when
            // absent), apply the updater closure, and reinsert. The `$dict_update`
            // helper takes all four pieces (each evaluated once at the call site)
            // and runs the read + `call_indirect` + write in its own frame, so the
            // mid-op closure call composes with the existing closure ABI. Matches
            // the interpreter's `insert(d, k, f(get_or(d, k, default)))` exactly.
            ("update", 4) => {
                let mode = self.dict_key_mode(&args[1])?;
                self.uses_dict = true;
                self.uses_str_eq = true; // `$key_eq` references `$str_eq`
                self.uses_dict_update = true;
                self.clos_arities.insert(1); // `$clos1` type + the function table
                let kk = self.kind_of(&args[1]);
                let dk = self.kind_of(&args[2]);
                let d = self.compile_expr(&args[0])?;
                let k = self.compile_expr(&args[1])?;
                let default = self.compile_expr(&args[2])?;
                let f = self.compile_expr(&args[3])?;
                // Stack: dict, key slot, default slot, mode, closure ptr.
                Ok(format!(
                    "{d}{k}{}{default}{}    i32.const {mode}\n{f}    call $dict_update\n",
                    to_slot(kk),
                    to_slot(dk),
                ))
            }
            // remove(dict, k): a fresh map with `k` (and its value) dropped.
            ("remove", 2) => {
                let mode = self.dict_key_mode(&args[1])?;
                self.uses_dict = true;
                self.uses_str_eq = true;
                let kk = self.kind_of(&args[1]);
                let d = self.compile_expr(&args[0])?;
                let k = self.compile_expr(&args[1])?;
                Ok(format!(
                    "{d}{k}{}    i32.const {mode}\n    call $dict_remove\n",
                    to_slot(kk)
                ))
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
            // The list header is its element count (i32) -> Int (i64).
            ("length", 1) => {
                let arg = self.compile_expr(&args[0])?;
                Ok(format!("{arg}    i32.load\n    i64.extend_i32_u\n"))
            }
            // at(list, i): load the 8-byte slot at ptr + 4 + i*8 (index is an
            // i64 Int, wrapped to an i32 address offset), then recover the
            // element's kind from the universal i64 slot rep.
            ("at", 2) => {
                // Recover the element at its kind, via the SAME `list_elem_kind`
                // that types the `at` expression — otherwise codegen would expect
                // one width and load another.
                let ek = self.list_elem_kind(&args[0]);
                // The index is normally an i64 Int, but a tuple-destructured Int
                // can already be narrowed to i32; convert to the i32 address kind.
                let ik = self.kind_of(&args[1]);
                let list = self.compile_expr(&args[0])?;
                let idx = self.compile_expr(&args[1])?;
                // `$list_at` bounds-checks and traps on an out-of-range index,
                // matching the interpreter's "index out of bounds" error (instead
                // of silently reading adjacent heap, which returned 0 or garbage).
                self.uses_list_at = true;
                Ok(format!(
                    "{list}{idx}{}    call $list_at\n{}",
                    kind_convert(ik, Kind::I32),
                    from_slot(ek)
                ))
            }
            // push(list, x) / concat(a, b): allocate a new list (runtime helper).
            ("push", 2) => {
                self.uses_list_push = true;
                let xk = self.kind_of(&args[1]);
                let list = self.compile_expr(&args[0])?;
                let x = self.compile_expr(&args[1])?;
                Ok(format!("{list}{x}{}    call $list_push\n", to_slot(xk)))
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
                self.uses_send = true;
                let target = self.compile_expr(&args[0])?;
                // Pack the message fields as a `[count][f0]..` record (the list
                // layout) and pass its pointer; the host copies the values out.
                let payload = self.compile_expr(&Expr::List(fields.clone()))?;
                Ok(format!(
                    "{target}    i32.const {tag}\n{payload}    call $send\n    i32.const 0\n"
                ))
            }
            ("spawn", _) => cerr("`spawn` is not compiled to WASM yet (host-driven)"),
            // --- the Dir capability family. A Dir value is an i32 handle into
            // the host's confined path table; each op is its own gated import. ---
            ("subdir", 2) => {
                self.used_dir_ops.insert("subdir");
                let d = self.compile_expr(&args[0])?;
                let name = self.compile_expr(&args[1])?;
                Ok(format!("{d}{name}    call $dir_subdir_host\n"))
            }
            ("read", 2) => {
                self.used_dir_ops.insert("read");
                let d = self.compile_expr(&args[0])?;
                let rel = self.compile_expr(&args[1])?;
                Ok(format!("{d}{rel}    call $dir_read\n"))
            }
            ("exists", 2) => {
                self.used_dir_ops.insert("exists");
                let d = self.compile_expr(&args[0])?;
                let rel = self.compile_expr(&args[1])?;
                Ok(format!("{d}{rel}    call $dir_exists_host\n"))
            }
            ("is_dir", 2) => {
                self.used_dir_ops.insert("is_dir");
                let d = self.compile_expr(&args[0])?;
                let rel = self.compile_expr(&args[1])?;
                Ok(format!("{d}{rel}    call $dir_is_dir_host\n"))
            }
            ("list", 1) => {
                self.used_dir_ops.insert("list");
                let d = self.compile_expr(&args[0])?;
                Ok(format!("{d}    call $dir_list\n"))
            }
            ("write", 3) => {
                self.used_dir_ops.insert("write");
                let d = self.compile_expr(&args[0])?;
                let rel = self.compile_expr(&args[1])?;
                let contents = self.compile_expr(&args[2])?;
                Ok(format!("{d}{rel}{contents}    call $dir_write_host\n    i32.const 0\n"))
            }
            ("make_dir", 2) => {
                self.used_dir_ops.insert("make_dir");
                let d = self.compile_expr(&args[0])?;
                let name = self.compile_expr(&args[1])?;
                Ok(format!("{d}{name}    call $dir_make_dir_host\n    i32.const 0\n"))
            }
            // --- build-time host ops (only reachable from a `build` entrypoint
            // compiled via `compile_build_module`). The BuildOut/BuildRead handle
            // is an i32 like a Dir handle; the host confines via its own tables. ---
            ("write_out", 3) => {
                self.used_build_ops.insert("write_out");
                let h = self.compile_expr(&args[0])?;
                let name = self.compile_expr(&args[1])?;
                let contents = self.compile_expr(&args[2])?;
                Ok(format!("{h}{name}{contents}    call $build_out_write_host\n    i32.const 0\n"))
            }
            ("read_build", 2) => {
                self.used_build_ops.insert("read_build");
                let h = self.compile_expr(&args[0])?;
                let rel = self.compile_expr(&args[1])?;
                Ok(format!("{h}{rel}    call $build_read\n"))
            }
            // --- the Net capability family. Net/Socket/Listener values are i32
            // handles into the host's tables; each op is its own gated import. ---
            ("restrict", 2) | ("connect", 2) | ("listen", 2) => {
                let op: &'static str = match name {
                    "restrict" => "restrict",
                    "connect" => "connect",
                    _ => "listen",
                };
                self.used_net_ops.insert(op);
                let net = self.compile_expr(&args[0])?;
                let addr = self.compile_expr(&args[1])?;
                Ok(format!("{net}{addr}    call $net_{op}_host\n"))
            }
            ("accept", 1) => {
                self.used_net_ops.insert("accept");
                let l = self.compile_expr(&args[0])?;
                Ok(format!("{l}    call $net_accept_host\n"))
            }
            ("send_line", 2) | ("send_bytes", 2) => {
                let op: &'static str = if name == "send_line" { "send_line" } else { "send_bytes" };
                self.used_net_ops.insert(op);
                let sock = self.compile_expr(&args[0])?;
                let payload = self.compile_expr(&args[1])?;
                Ok(format!("{sock}{payload}    call $net_{op}_host\n    i32.const 0\n"))
            }
            ("recv_line", 1) => {
                self.used_net_ops.insert("recv_line");
                let sock = self.compile_expr(&args[0])?;
                Ok(format!("{sock}    call $net_recv_line\n"))
            }
            ("recv_all", 1) => {
                self.used_net_ops.insert("recv_all");
                let sock = self.compile_expr(&args[0])?;
                Ok(format!("{sock}    call $net_recv_all\n"))
            }
            ("recv_bytes", 2) => {
                self.used_net_ops.insert("recv_bytes");
                let nk = self.kind_of(&args[1]);
                let sock = self.compile_expr(&args[0])?;
                let n = self.compile_expr(&args[1])?;
                Ok(format!(
                    "{sock}{n}{}    call $net_recv_bytes\n",
                    kind_convert(nk, Kind::I64)
                ))
            }
            ("close", 1) => {
                self.used_net_ops.insert("close");
                let sock = self.compile_expr(&args[0])?;
                Ok(format!("{sock}    call $net_close_host\n    i32.const 0\n"))
            }
            _ => {
                // A function-valued local (a closure param/binding) holds a
                // pointer to a `[code_index][caps..]` record. Call it through the
                // table: pass the closure pointer as the environment (first
                // param), then the args, then `call_indirect` on the code index
                // loaded from the record's header.
                if self.locals.contains_key(name) {
                    // Closures use the generic i32 ABI for every parameter, so a
                    // concrete i64 Int argument is narrowed to i32.
                    let n = args.len();
                    let mut out = format!("    local.get ${name}\n");
                    for arg in args {
                        let ak = self.kind_of(arg);
                        out.push_str(&self.compile_expr(arg)?);
                        // Pass each arg in the universal i64 slot (the closure ABI).
                        out.push_str(to_slot(ak));
                    }
                    out.push_str(&format!("    local.get ${name}\n    i32.load\n"));
                    out.push_str(&format!("    call_indirect (type $clos{n})\n"));
                    // The call returns the universal i64 slot; recover at the
                    // closure's return kind (matches `kind_of` for this call).
                    let rk = self.local_fn_ret_kind.get(name).copied().unwrap_or(Kind::I32);
                    out.push_str(from_slot(rk));
                    self.clos_arities.insert(n);
                    return Ok(out);
                }
                // Convert each argument to its parameter's kind (the only real
                // crossing is a concrete i64 Int meeting a generic i32 param).
                let param_kinds: Vec<Kind> = self
                    .fn_params
                    .get(name)
                    .map(|ps| {
                        ps.iter()
                            .map(|p| p.ty.as_ref().map(ty_kind).unwrap_or(Kind::I32))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut out = String::new();
                for (i, arg) in args.iter().enumerate() {
                    let ak = self.kind_of(arg);
                    out.push_str(&self.compile_expr(arg)?);
                    if let Some(&pk) = param_kinds.get(i) {
                        out.push_str(kind_convert(ak, pk));
                    }
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

/// Collect a type's variables (lowercase, argument-less names) in order of
/// first appearance — the same parameter-ordering rule the type checker's
/// `collect_type_params` applies to a type declaration.
fn collect_type_vars(ty: &Type, acc: &mut Vec<String>) {
    match ty {
        Type::Tuple(ts) => {
            for t in ts {
                collect_type_vars(t, acc);
            }
        }
        Type::Fn(params, ret) => {
            for p in params {
                collect_type_vars(p, acc);
            }
            collect_type_vars(ret, acc);
        }
        Type::Named(name, args) => {
            if args.is_empty() && name.chars().next().is_some_and(|c| c.is_lowercase()) {
                if !acc.contains(name) {
                    acc.push(name.clone());
                }
            } else {
                for a in args {
                    collect_type_vars(a, acc);
                }
            }
        }
    }
}

fn bare_type_var(ty: &Type) -> Option<String> {
    match ty {
        Type::Named(n, args)
            if args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()) =>
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
fn list_param_of_var(params: &[crate::ast::Param], tv: &str) -> Option<usize> {
    params.iter().position(|p| {
        matches!(&p.ty, Some(Type::Named(n, targs))
            if n == "List" && targs.len() == 1 && bare_type_var(&targs[0]).as_deref() == Some(tv))
    })
}

/// The index of the first parameter typed `fn(..) -> tv` (a function returning
/// the given type-var `tv`).
fn fn_param_returning_var(params: &[crate::ast::Param], tv: &str) -> Option<usize> {
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
fn collect_fn_refs_block(b: &Block, out: &mut HashSet<String>) {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetTuple { value, .. } => {
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
        Expr::Index { .. } | Expr::WhileLet { .. } | Expr::MethodCall { .. } | Expr::Record { .. } => {
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
        | Expr::Tuple(args)
        | Expr::Spawn { args, .. } => {
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
        Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
    }
}

/// The set of functions reachable from `main` (transitively). Only these need
/// compiling — importing a std module no longer drags its whole API into the
/// output.
fn reachable_functions(module: &Module) -> HashSet<String> {
    let mut bodies: HashMap<&str, &Block> = HashMap::new();
    for item in &module.items {
        if let Item::Function(f) = item {
            bodies.insert(f.name.as_str(), &f.body);
        }
    }
    let mut reachable: HashSet<String> = HashSet::new();
    let mut work: Vec<String> = Vec::new();
    if bodies.contains_key("main") {
        reachable.insert("main".to_string());
        work.push("main".to_string());
    }
    while let Some(name) = work.pop() {
        if let Some(body) = bodies.get(name.as_str()) {
            let mut refs = HashSet::new();
            collect_fn_refs_block(body, &mut refs);
            for r in refs {
                if bodies.contains_key(r.as_str()) && reachable.insert(r.clone()) {
                    work.push(r);
                }
            }
        }
    }
    reachable
}

pub fn compile_module(module: &Module) -> Result<String, CodegenError> {
    // Desugar traits/impls to ordinary functions (no-op for trait-free modules)
    // so codegen, like the interpreter, only ever sees plain functions. Then
    // lower ranges to their list-building blocks once, so the local-collection
    // and emission passes below agree on the synthetic loop-variable names.
    let recs = crate::records::lower(module.clone())
        .map_err(|message| CodegenError { message })?;
    let mut lowered = crate::traits::lower_for_wasm(recs);
    crate::parser::lower_sugar_module(&mut lowered);
    let module = &lowered;
    let mut cg = Codegen::new();
    // Collect parameter conventions up front so call sites can resolve `inout`
    // write-back even for forward references.
    for item in &module.items {
        match item {
            Item::Function(f) => {
                cg.fn_conventions
                    .insert(f.name.clone(), f.params.iter().map(|p| p.convention).collect());
                cg.fn_params.insert(f.name.clone(), f.params.clone());
                let ret = f.ret.as_ref().map(ty_kind).unwrap_or(Kind::I32);
                cg.fn_ret.insert(f.name.clone(), ret);
                if let Some(t) = &f.ret {
                    cg.fn_ret_valtype.insert(f.name.clone(), ty_to_valtype(t));
                    cg.fn_ret_ty.insert(f.name.clone(), t.clone());
                }
                // A function returning a closure (`-> fn(...) -> RET`): record the
                // closure's return kind so a `let f = make(...)` then `f(x)` call
                // recovers the result at the right width.
                if let Some(Type::Fn(_, cret)) = &f.ret {
                    cg.fn_ret_closure_kind.insert(f.name.clone(), ty_kind(cret));
                }
                // A function returning a tuple: record its slot value types so a
                // `let (a, b) = f(...)` destructures each at the right width.
                if let Some(Type::Tuple(slots)) = &f.ret {
                    cg.fn_ret_tuple_slots
                        .insert(f.name.clone(), slots.iter().map(ty_to_valtype).collect());
                    // Per slot, the element type if the slot is `List(<scalar>)`
                    // (e.g. unzip's `(List(Int), List(Int))`), so a destructure
                    // binds each list var's element type.
                    let elems: Vec<Option<ValType>> = slots
                        .iter()
                        .map(|t| match t {
                            Type::Named(n, a) if n == "List" => a.first().and_then(|e| {
                                match ty_to_valtype(e) {
                                    ValType::Other => None,
                                    vt => Some(vt),
                                }
                            }),
                            _ => None,
                        })
                        .collect();
                    if elems.iter().any(|e| e.is_some()) {
                        cg.fn_ret_tuple_slot_list_elem.insert(f.name.clone(), elems);
                    }
                }
            }
            Item::Type(t) => {
                cg.adt_variants
                    .insert(t.name.clone(), t.variants.iter().map(|v| v.fields.clone()).collect());
                cg.adt_variant_names
                    .insert(t.name.clone(), t.variants.iter().map(|v| v.name.clone()).collect());
                for (tag, variant) in t.variants.iter().enumerate() {
                    cg.ctor_type_name.insert(variant.name.clone(), t.name.clone());
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
                        cg.record_field_types.insert(t.name.clone(), variant.fields.clone());
                    }
                }
            }
            Item::Actor(_) => {}
            Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } => {}
        }
    }
    // Now that all record types are known, record which constructor fields are
    // records, so binding `Circle(p)` in a pattern lets `p.field` resolve.
    for item in &module.items {
        if let Item::Type(t) = item {
            for variant in &t.variants {
                let field_recs: Vec<Option<String>> = variant
                    .fields
                    .iter()
                    .map(|ty| match ty {
                        Type::Named(n, _) if cg.record_fields.contains_key(n) => Some(n.clone()),
                        _ => None,
                    })
                    .collect();
                if field_recs.iter().any(|r| r.is_some()) {
                    cg.ctor_field_records.insert(variant.name.clone(), field_recs);
                }
            }
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
                } else if n == "List" {
                    // `List(Account)`: `for x in f(...)` binds x to that record.
                    if let Some(Type::Named(elem, _)) = args.first() {
                        if cg.record_fields.contains_key(elem) {
                            cg.fn_ret_list_elem.insert(f.name.clone(), elem.clone());
                        }
                    }
                    // `List(String)` etc.: record the scalar element value type so
                    // `at(f(...), i)` is typed (e.g. a String element compares by
                    // content). Skips `Other` (generic / non-scalar elements).
                    if let Some(elem) = args.first() {
                        let evt = ty_to_valtype(elem);
                        if evt != ValType::Other {
                            cg.fn_ret_list_elem_valtype.insert(f.name.clone(), evt);
                        }
                        // `List((T, U))` (e.g. zip): record the element tuple's
                        // slot types so a destructure of `at(f(...), i)` is typed.
                        if let Type::Tuple(slots) = elem {
                            cg.fn_ret_list_elem_tuple_slots
                                .insert(f.name.clone(), slots.iter().map(ty_to_valtype).collect());
                        }
                    }
                } else if let Some(payload) = args.first() {
                    // e.g. `Result(Account, _)` / `Option(Account)`: `?` yields it.
                    if let Type::Named(rec, _) = payload {
                        if cg.record_fields.contains_key(rec) {
                            cg.fn_ret_result_record.insert(f.name.clone(), rec.clone());
                        }
                    }
                    // A scalar success payload (e.g. `Option(Int)` from parse_int,
                    // or a user `R(Int, _)`): record it so a `match`/`?` recovers
                    // the Some/Ok value at the right width instead of truncating a
                    // big Int to the generic i32. The success payload is the first
                    // type argument (true for Option/Result and result-like sum
                    // types); only ever consulted at a Some/Ok/`?` site, so a
                    // non-result type's first arg is harmless.
                    let pvt = ty_to_valtype(payload);
                    if pvt != ValType::Other {
                        cg.fn_ret_result_valtype.insert(f.name.clone(), pvt);
                    }
                }
            }
            // Generic shapes over a `List(a)` argument: `-> Option(a)/Result(a,_)`
            // (find/head/min_by) and `-> List(a)` (filter/take/reverse/sort_by).
            // Record which argument carries `a` so a call's payload / element
            // record type resolves from that argument, without full inference.
            if let Some(tv) = payload_type_var(&f.ret) {
                if let Some(k) = list_param_of_var(&f.params, &tv) {
                    cg.fn_ret_option_of_list_arg.insert(f.name.clone(), k);
                }
            }
            if let Some(tv) = list_elem_type_var(&f.ret) {
                if let Some(k) = list_param_of_var(&f.params, &tv) {
                    cg.fn_ret_list_of_list_arg.insert(f.name.clone(), k);
                } else if let Some(k) = fn_param_returning_var(&f.params, &tv) {
                    // `map`: result element type is the mapper's return type.
                    cg.fn_ret_list_of_fn_arg.insert(f.name.clone(), k);
                }
            }
        }
    }
    let mut func_wat = String::new();
    let mut main_params = 0usize;
    let mut main_param_is_args: Vec<bool> = Vec::new();
    let mut main_returns_int = false;
    let mut main_returns_float = false;
    let mut has_main = false;

    // Dead-code elimination: compile only functions reachable from `main`.
    let reachable = reachable_functions(module);
    for item in &module.items {
        match item {
            Item::Function(f) => {
                if f.name == "main" {
                    has_main = true;
                    main_params = f.params.len();
                    main_returns_int = matches!(&f.ret, Some(Type::Named(n, _)) if n == "Int");
                    main_returns_float = matches!(&f.ret, Some(Type::Named(n, _)) if n == "Float");
                    // Capability parameters are type-level only (the authority is
                    // the linked host imports), so each gets a dummy slot in the
                    // `run` export — which happens to be handle 0, the granted
                    // root, for the handle-backed Dir/Net. An argv parameter is
                    // materialized by `$build_args` from the host-provided list.
                    for p in &f.params {
                        let is_args = matches!(&p.ty, Some(t) if crate::typeck::is_args_type(t));
                        if is_args {
                            cg.uses_args = true;
                        }
                        main_param_is_args.push(is_args);
                    }
                }
                if reachable.contains(&f.name) {
                    func_wat.push_str(&cg.compile_function(f)?);
                }
            }
            Item::Type(_) => {}
            Item::Actor(_) => return cerr("use compile_actor_module to compile an actor"),
            Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } => {}
        }
    }
    if !has_main {
        return cerr("no `main` function to compile");
    }
    // In the sandbox there is no process to exit, so a value-returning `main`
    // surfaces its result (an `Int` via `print_int`, a `Float` via `print_float`)
    // — the REPL-style value display the codegen tests rely on. At the *process*
    // boundary (the CLI/interpreter), an `Int` return is instead the exit code.
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
        let params = format!("(param i32) {}", "(param i64) ".repeat(*n));
        wat.push_str(&format!("  (type $clos{n} (func {params}(result i64)))\n"));
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
    for body in cg.eq_helpers.values() {
        wat.push_str(body);
    }
    for body in cg.ts_helpers.values() {
        wat.push_str(body);
    }

    wat.push_str("  (func (export \"run\")\n");
    for i in 0..main_params {
        if main_param_is_args.get(i).copied().unwrap_or(false) {
            // The argv parameter: a real List(String) built from the host.
            wat.push_str("    call $build_args\n");
        } else {
            // A capability parameter: type-level only; 0 is the root handle.
            wat.push_str("    i32.const 0\n");
        }
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

/// Compile a rune's build step to a WASM module that runs in the zero-ambient
/// build sandbox. The `build` entrypoint is renamed to `main` so the whole
/// `compile_module` pipeline (the `run` export, marshaling, helpers) is reused
/// verbatim — its capability parameters lower to handle 0 exactly like `main`'s,
/// and the only build-specific code is the `write_out`/`read_build` host calls,
/// which never appear in an ordinary program (so parity is untouched). The host
/// links only `build_out_write`/`build_read_len`, confined to the granted output
/// sandbox and read roots — nothing else exists for the guest to call.
pub fn compile_build_module(module: &Module) -> Result<String, CodegenError> {
    let mut m = module.clone();
    // A build module ships no `main`; promote its `build` entrypoint to `main`.
    m.items.retain(|it| !matches!(it, Item::Function(f) if f.name == "main"));
    for item in &mut m.items {
        if let Item::Function(f) = item {
            if f.name.rsplit('.').next() == Some("build") {
                f.name = "main".to_string();
            }
        }
    }
    compile_module(&m)
}

/// Compile a single actor to its own WASM module. Int fields with literal
/// initializers become mutable globals (state); capability fields are erased;
/// each handler becomes an exported function.
pub fn compile_actor_module(actor: &ActorDef) -> Result<String, CodegenError> {
    compile_actor_with_tags(actor, &HashMap::new())
}

/// The host-visible type of one message field: how the actor system reads it
/// out of the sender's memory and delivers it into the receiver's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MsgField {
    /// A scalar copied by value.
    Int,
    /// A string: the slot is a pointer in the SENDER's memory; the host copies
    /// the content and re-allocates it in the receiver before delivery.
    Str,
    /// An actor id copied by value — passing a Subject in a message hands the
    /// receiver the authority to message that actor (capability delegation).
    Subject,
    /// An f64 copied by value (the full 8-byte slot).
    Float,
    /// A `List(Int)`: the host walks the sender's `[count][i64 slots]` and
    /// re-lays it out in the receiver.
    IntList,
    /// A `List(String)`: walked by content like `Str`, re-laid out in the
    /// receiver with absolute pointers (the `write_pending_list` layout).
    StrList,
}

/// One routable message: its name and per-field wire types.
pub type MessageSig = (String, Vec<MsgField>);

/// Compile every actor in a module, assigning each distinct handler message a
/// shared tag so the host can route inter-actor sends. Returns (actor name,
/// WAT) pairs and the tag -> message-signature table.
pub type CompiledActors = (Vec<(String, String)>, Vec<MessageSig>);

fn message_sig_of(h: &crate::ast::Handler) -> Result<Vec<MsgField>, CodegenError> {
    h.params
        .iter()
        .map(|p| match &p.ty {
            Some(Type::Named(t, _)) if t == "Int" => Ok(MsgField::Int),
            Some(Type::Named(t, _)) if t == "String" => Ok(MsgField::Str),
            Some(Type::Named(t, _)) if t == "Subject" => Ok(MsgField::Subject),
            Some(Type::Named(t, _)) if t == "Float" => Ok(MsgField::Float),
            Some(Type::Named(t, args)) if t == "List" => match args.first() {
                Some(Type::Named(e, _)) if e == "Int" => Ok(MsgField::IntList),
                Some(Type::Named(e, _)) if e == "String" => Ok(MsgField::StrList),
                _ => cerr(format!(
                    "handler `{}` param `{}`: only List(Int) and List(String) list parameters compile yet",
                    h.message, p.name
                )),
            },
            _ => cerr(format!(
                "handler `{}` param `{}`: only Int, Float, String, Subject, List(Int), and List(String) message parameters compile yet",
                h.message, p.name
            )),
        })
        .collect()
}

pub fn compile_program(module: &Module) -> Result<CompiledActors, CodegenError> {
    let mut tag_of: HashMap<String, u32> = HashMap::new();
    let mut sigs: Vec<MessageSig> = Vec::new();
    for item in &module.items {
        if let Item::Actor(a) = item {
            for h in &a.handlers {
                let sig = message_sig_of(h)?;
                match tag_of.get(&h.message) {
                    None => {
                        tag_of.insert(h.message.clone(), sigs.len() as u32);
                        sigs.push((h.message.clone(), sig));
                    }
                    // The host decodes a message by its tag's signature, so every
                    // actor declaring the message must agree on its field types.
                    Some(&tag) if sigs[tag as usize].1 != sig => {
                        return cerr(format!(
                            "message `{}` is declared with different parameter types by different actors",
                            h.message
                        ));
                    }
                    Some(_) => {}
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
    Ok((actors, sigs))
}

fn compile_actor_with_tags(
    actor: &ActorDef,
    tags: &HashMap<String, u32>,
) -> Result<String, CodegenError> {
    // Lower ranges first (see compile_module) so the passes below see plain
    // blocks with consistent synthetic names.
    let mut actor_owned = actor.clone();
    crate::parser::lower_sugar_actor(&mut actor_owned);
    let actor = &actor_owned;
    let mut cg = Codegen::new();
    cg.message_tags = tags.clone();

    let mut state_globals = String::new();
    let mut str_field_inits: Vec<(u32, u32)> = Vec::new();
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
        // String state lives in host cells (the per-message arena reset would
        // clobber a guest-heap string): reads stage through `field_str_len` +
        // `fill_pending`, writes copy content out via `field_str_set`, and a
        // start function sets the declared initializers at instantiation.
        if tname == "String" {
            let init_off = match &field.init {
                Some(Expr::Str(s)) => {
                    let s = s.clone();
                    cg.intern(&s)
                }
                Some(_) => {
                    return cerr(format!(
                        "field `{}`: initializer must be a String literal",
                        field.name
                    ))
                }
                None => {
                    return cerr(format!(
                        "field `{}`: String state needs an initializer in codegen",
                        field.name
                    ))
                }
            };
            let idx = cg.str_fields.len() as u32;
            cg.str_fields.insert(field.name.clone(), idx);
            cg.uses_str_field = true;
            cg.local_val_types.insert(field.name.clone(), ValType::Str);
            str_field_inits.push((idx, init_off));
            continue;
        }
        // A Float field is a real (mut f64) global — floats are values, so no
        // host cell is needed. Its kind registers in `locals` so reads,
        // assignments, and captures convert at f64.
        if tname == "Float" {
            let init = match &field.init {
                Some(Expr::Float(x)) => *x,
                Some(_) => {
                    return cerr(format!(
                        "field `{}`: initializer must be a Float literal",
                        field.name
                    ))
                }
                None => {
                    return cerr(format!(
                        "field `{}`: Float state needs an initializer in codegen",
                        field.name
                    ))
                }
            };
            cg.globals.insert(field.name.clone());
            cg.locals.insert(field.name.clone(), Kind::F64);
            cg.local_val_types.insert(field.name.clone(), ValType::Float);
            state_globals.push_str(&format!(
                "  (global ${} (mut f64) (f64.const {init}))\n",
                field.name
            ));
            continue;
        }
        if tname != "Int" {
            return cerr(format!(
                "actor field `{}`: only Int, Float, String, and capability fields compile yet",
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
    // Field kinds (e.g. a Float field's f64) persist across handlers; each
    // handler's own params/lets start from this snapshot.
    let field_kinds = cg.locals.clone();
    for h in &actor.handlers {
        let sig = message_sig_of(h)?;
        cg.locals = field_kinds.clone();
        let mut header = format!("  (func (export \"{}\") ", h.message);
        for (p, kind) in h.params.iter().zip(&sig) {
            // An Int or Subject travels by value; a Float as the full f64; a
            // String param is a pointer to a `[len][bytes]` string the host
            // re-allocated in THIS actor's memory (`__msg_alloc`) before
            // delivery.
            let wasm_ty = if *kind == MsgField::Float { "f64" } else { "i32" };
            header.push_str(&format!("(param ${} {wasm_ty}) ", p.name));
            match kind {
                MsgField::Int => {
                    cg.local_val_types.insert(p.name.clone(), ValType::Int);
                }
                MsgField::Float => {
                    cg.locals.insert(p.name.clone(), Kind::F64);
                    cg.local_val_types.insert(p.name.clone(), ValType::Float);
                }
                MsgField::Str => {
                    cg.uses_msg_alloc = true;
                    cg.local_val_types.insert(p.name.clone(), ValType::Str);
                }
                MsgField::IntList => {
                    cg.uses_msg_alloc = true;
                    cg.local_list_elem_valtype.insert(p.name.clone(), ValType::Int);
                }
                MsgField::StrList => {
                    cg.uses_msg_alloc = true;
                    cg.local_list_elem_valtype.insert(p.name.clone(), ValType::Str);
                }
                // A Subject is an opaque actor id; it is a send target, not a
                // printable value, so it gets no value type.
                MsgField::Subject => {}
            }
        }
        header.push('\n');
        let renamed = alpha_rename(&h.body, &h.params);
        // Infer each let's kind (as `compile_function` does) so e.g. an i64
        // list element flowing into a local declares that local at i64.
        cg.infer_locals(&renamed);
        let mut lets = Vec::new();
        collect_let_names(&renamed, &mut lets);
        lets.sort();
        lets.dedup();
        for name in &lets {
            let k = cg.locals.get(name).copied().unwrap_or(Kind::I32);
            header.push_str(&format!("    (local ${name} {})\n", wasm_ty(k)));
        }
        let body = cg.compile_block(&renamed)?;
        handlers.push((header, body));
    }

    // No-GC for actors: the host calls `__msg_prep` before each delivery to
    // reset the heap arena, since a handler's heap allocations never escape
    // (state lives in globals; sends copy). This bounds memory for long-running
    // actors without a collector. `__msg_alloc` then re-allocates any String
    // message fields in THIS actor's memory before the handler runs.
    let mut extra_globals = state_globals;
    let mut msg_helpers = String::new();
    if cg.need_heap() {
        extra_globals.push_str(&format!(
            "  (global $heap_base i32 (i32.const {}))\n",
            cg.next_offset
        ));
        msg_helpers.push_str(
            "  (func (export \"__msg_prep\")\n    global.get $heap_base\n    global.set $heap)\n",
        );
        if cg.uses_msg_alloc {
            msg_helpers.push_str(
                "  (func (export \"__msg_alloc\") (param $n i32) (result i32)\n    \
                 (local $p i32)\n    \
                 (call $ensure (i32.add (local.get $n) (i32.const 4)))\n    \
                 (local.set $p (global.get $heap))\n    \
                 (global.set $heap (i32.add (local.get $p) (i32.add (local.get $n) (i32.const 4))))\n    \
                 (local.get $p))\n",
            );
        }
    }

    let mut wat = String::from("(module\n");
    let mut arities: Vec<usize> = cg.clos_arities.iter().copied().collect();
    arities.sort_unstable();
    for n in &arities {
        // One leading param for the closure environment, then the call's args.
        let params = format!("(param i32) {}", "(param i64) ".repeat(*n));
        wat.push_str(&format!("  (type $clos{n} (func {params}(result i64)))\n"));
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
        wat.push_str(&format!("{header}{body}    drop\n  )\n"));
    }
    wat.push_str(&msg_helpers);
    if !str_field_inits.is_empty() {
        // Set each String state cell to its declared initializer (an interned
        // literal) at instantiation, before any message is delivered.
        wat.push_str("  (func $__init_str_fields\n");
        for (idx, off) in &str_field_inits {
            wat.push_str(&format!(
                "    (call $field_str_set_host (i32.const {idx}) (i32.const {off}))\n"
            ));
        }
        wat.push_str("  )\n  (start $__init_str_fields)\n");
    }
    for lam in &cg.lambdas {
        wat.push_str(lam);
    }
    for body in cg.eq_helpers.values() {
        wat.push_str(body);
    }
    for body in cg.ts_helpers.values() {
        wat.push_str(body);
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

/// Allocation helper for an N-field constructor record `[tag: i32][f0..f{N-1}]`.
/// The tag header is a 4-byte i32; every field is an 8-byte slot holding the
/// universal i64 representation (callers convert via `to_slot`).
fn mk_helper(n: usize) -> String {
    let mut params = String::from("(param $tag i32)");
    for i in 0..n {
        params.push_str(&format!(" (param $f{i} i64)"));
    }
    let size = 4 + 8 * n;
    let mut s = format!("  (func $mk{n} {params} (result i32)\n    (local $p i32)\n");
    s.push_str(&format!("    (call $ensure (i32.const {size}))\n"));
    s.push_str("    global.get $heap local.set $p\n");
    s.push_str("    local.get $p local.get $tag i32.store\n");
    for i in 0..n {
        s.push_str(&format!(
            "    local.get $p i32.const {} i32.add local.get $f{i} i64.store\n",
            4 + 8 * i
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

// at(list, i): the i-th element slot, bounds-checked. A list is `[len:i32]` then
// `len` 8-byte slots, so element i is at `list + 4 + 8*i`. An out-of-range index
// (negative or >= len) traps — matching the interpreter's "index out of bounds"
// error instead of silently reading adjacent heap.
const LIST_AT_WAT: &str = r#"  (func $list_at (param $list i32) (param $i i32) (result i64)
    (if (i32.or
          (i32.lt_s (local.get $i) (i32.const 0))
          (i32.ge_s (local.get $i) (i32.load (local.get $list))))
      (then (unreachable)))
    (i64.load
      (i32.add (i32.add (local.get $list) (i32.const 4))
               (i32.mul (local.get $i) (i32.const 8)))))
"#;

// push(list, x): a fresh list `[len+1][elems...][x]`. Elements are 4-byte i32s,
// so the element block is copied with `memory.copy`.
const LIST_PUSH_WAT: &str = r#"  (func $list_push (param $list i32) (param $x i64) (result i32)
    (local $len i32) (local $new i32)
    local.get $list i32.load local.set $len
    (call $ensure (i32.add (i32.const 4) (i32.mul (i32.add (local.get $len) (i32.const 1)) (i32.const 8))))
    global.get $heap local.set $new
    local.get $new local.get $len i32.const 1 i32.add i32.store
    local.get $new i32.const 4 i32.add
    local.get $list i32.const 4 i32.add
    local.get $len i32.const 8 i32.mul
    memory.copy
    local.get $new i32.const 4 i32.add local.get $len i32.const 8 i32.mul i32.add
    local.get $x i64.store
    local.get $new i32.const 4 i32.add local.get $len i32.const 1 i32.add i32.const 8 i32.mul i32.add global.set $heap
    local.get $new)
"#;

// concat(a, b): a fresh list `[alen+blen][a elems][b elems]` (8-byte slots).
const LIST_CONCAT_WAT: &str = r#"  (func $list_concat (param $a i32) (param $b i32) (result i32)
    (local $alen i32) (local $blen i32) (local $new i32)
    local.get $a i32.load local.set $alen
    local.get $b i32.load local.set $blen
    (call $ensure (i32.add (i32.const 4) (i32.mul (i32.add (local.get $alen) (local.get $blen)) (i32.const 8))))
    global.get $heap local.set $new
    local.get $new local.get $alen local.get $blen i32.add i32.store
    local.get $new i32.const 4 i32.add
    local.get $a i32.const 4 i32.add
    local.get $alen i32.const 8 i32.mul
    memory.copy
    local.get $new i32.const 4 i32.add local.get $alen i32.const 8 i32.mul i32.add
    local.get $b i32.const 4 i32.add
    local.get $blen i32.const 8 i32.mul
    memory.copy
    local.get $new i32.const 4 i32.add local.get $alen local.get $blen i32.add i32.const 8 i32.mul i32.add global.set $heap
    local.get $new)
"#;

// drop(list, k): the sublist `[len-k][elem_k...]` (used by `[h, ..t]` patterns).
const LIST_DROP_WAT: &str = r#"  (func $list_drop (param $list i32) (param $k i32) (result i32)
    (local $newlen i32) (local $new i32)
    local.get $list i32.load local.get $k i32.sub local.set $newlen
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $newlen) (i32.const 8))))
    global.get $heap local.set $new
    local.get $new local.get $newlen i32.store
    local.get $new i32.const 4 i32.add
    local.get $list i32.const 4 i32.add local.get $k i32.const 8 i32.mul i32.add
    local.get $newlen i32.const 8 i32.mul
    memory.copy
    local.get $new i32.const 4 i32.add local.get $newlen i32.const 8 i32.mul i32.add global.set $heap
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
// ascii_case(s, up): a fresh string with each ASCII letter cased (`up`=1 ->
// upper, 0 -> lower); other bytes are copied unchanged. Matches the
// interpreter's `to_ascii_uppercase`/`to_ascii_lowercase` byte-for-byte.
const ASCII_CASE_WAT: &str = r#"  (func $ascii_case (param $s i32) (param $up i32) (result i32)
    (local $len i32) (local $i i32) (local $res i32) (local $b i32)
    (local.set $len (i32.load (local.get $s)))
    (call $ensure (i32.add (i32.const 4) (local.get $len)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $len)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (if (local.get $up)
          (then
            (if (i32.and (i32.ge_u (local.get $b) (i32.const 97)) (i32.le_u (local.get $b) (i32.const 122)))
              (then (local.set $b (i32.sub (local.get $b) (i32.const 32))))))
          (else
            (if (i32.and (i32.ge_u (local.get $b) (i32.const 65)) (i32.le_u (local.get $b) (i32.const 90)))
              (then (local.set $b (i32.add (local.get $b) (i32.const 32)))))))
        (i32.store8 (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $i)) (local.get $b))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;

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

// crypto.sha256(in): allocate the fixed 68-byte result string `[len=64][64
// bytes]`, set its length, then call the host import to fill the 64 hex bytes at
// `res+4`. The hash length is a compile-time constant, so no size negotiation is
// needed.
// float_to_str(x): a fresh string of `x` formatted by the host (Rust Display).
// Reserve a generous body buffer (an f64's decimal form is well under 512
// bytes), let the host write into it and return the length, then set the header.
const FLOAT_TO_STR_WAT: &str = r#"  (func $float_to_str (param $x f64) (result i32)
    (local $res i32) (local $n i32)
    (call $ensure (i32.const 516))
    (local.set $res (global.get $heap))
    (local.set $n (call $float_to_str_host (local.get $x) (i32.add (local.get $res) (i32.const 4))))
    (i32.store (local.get $res) (local.get $n))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $n)))
    (local.get $res))
"#;

// encoding(op, in): a fresh string holding the hex/base64 transform of `in`,
// computed by the host. Every output is at most 2x the input bytes (hex_encode
// is exactly 2x; base64_encode ~1.33x; both decodes shrink), so reserve
// `2*len + slack` for the body, let the host fill it and return the length, then
// set the header.
const ENCODING_WAT: &str = r#"  (func $encoding (param $op i32) (param $in i32) (result i32)
    (local $res i32) (local $n i32)
    (call $ensure (i32.add (i32.mul (i32.load (local.get $in)) (i32.const 2)) (i32.const 20)))
    (local.set $res (global.get $heap))
    (local.set $n (call $encoding_host (local.get $op) (local.get $in) (i32.add (local.get $res) (i32.const 4))))
    (i32.store (local.get $res) (local.get $n))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $n)))
    (local.get $res))
"#;

// get_env(name): an Option(String). `env_len` reports the value's byte length
// (or -1 when unset), the guest allocates the string and `env_fill` writes the
// bytes; the result is a Some record ([tag=0][string-ptr slot]) or a bare None
// ([tag=1]) — the same tags `std/option` declares.
const GET_ENV_WAT: &str = r#"  (func $get_env (param $name i32) (result i32)
    (local $len i32) (local $str i32) (local $res i32)
    (local.set $len (call $env_len_host (local.get $name)))
    (if (i32.lt_s (local.get $len) (i32.const 0))
      (then
        (call $ensure (i32.const 4))
        (local.set $res (global.get $heap))
        (i32.store (local.get $res) (i32.const 1))
        (global.set $heap (i32.add (local.get $res) (i32.const 4)))
        (return (local.get $res))))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $str (global.get $heap))
    (i32.store (local.get $str) (local.get $len))
    (call $env_fill_host (local.get $name) (i32.add (local.get $str) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $str) (i32.const 4)) (local.get $len)))
    (call $ensure (i32.const 12))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (i32.const 0))
    (i64.store (i32.add (local.get $res) (i32.const 4)) (i64.extend_i32_s (local.get $str)))
    (global.set $heap (i32.add (local.get $res) (i32.const 12)))
    (local.get $res))
"#;

// read(dir, rel): a fresh string of the confined file's contents. The host
// reads and stages the file at the `dir_read_len` call (no read/fill race),
// the guest allocates `[len][bytes]`, and `fill_pending` writes the bytes.
const DIR_READ_WAT: &str = r#"  (func $dir_read (param $h i32) (param $rel i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $dir_read_len_host (local.get $h) (local.get $rel)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;

// read_build(h, rel): the confined file's contents as a fresh string — identical
// staging to `dir_read`, but the host resolves against the build read roots.
const BUILD_READ_WAT: &str = r#"  (func $build_read (param $h i32) (param $rel i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $build_read_len_host (local.get $h) (local.get $rel)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;

// list(dir): a List(String) of the directory's sorted entry names. The host
// stages the listing at `dir_list_size` and then lays the COMPLETE list
// structure out at the reserved base (header, slots, string objects) — the
// guest only reserves and bumps.
const DIR_LIST_WAT: &str = r#"  (func $dir_list (param $h i32) (result i32)
    (local $size i32) (local $res i32)
    (local.set $size (call $dir_list_size_host (local.get $h)))
    (call $ensure (local.get $size))
    (local.set $res (global.get $heap))
    (call $write_pending_list_host (local.get $res))
    (global.set $heap (i32.add (local.get $res) (local.get $size)))
    (local.get $res))
"#;

// crypto.sign(msg) / crypto.public_key(): fixed-size hex strings (128 / 64
// bytes) filled by the host with the GRANTED key — the seed never enters guest
// memory.
const CRYPTO_SIGN_WAT: &str = r#"  (func $crypto_sign (param $msg i32) (result i32)
    (local $res i32)
    (call $ensure (i32.const 132))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (i32.const 128))
    (call $crypto_sign_host (local.get $msg) (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (local.get $res) (i32.const 132)))
    (local.get $res))
"#;

const CRYPTO_PUBLIC_KEY_WAT: &str = r#"  (func $crypto_public_key (result i32)
    (local $res i32)
    (call $ensure (i32.const 68))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (i32.const 64))
    (call $crypto_public_key_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (local.get $res) (i32.const 68)))
    (local.get $res))
"#;

// build_args(): the host-provided argv as a List(String), built exactly like a
// directory listing — the host stages it at `args_size` and lays the complete
// structure out via `write_pending_list`.
const BUILD_ARGS_WAT: &str = r#"  (func $build_args (result i32)
    (local $size i32) (local $res i32)
    (local.set $size (call $args_size_host))
    (call $ensure (local.get $size))
    (local.set $res (global.get $heap))
    (call $write_pending_list_host (local.get $res))
    (global.set $heap (i32.add (local.get $res) (local.get $size)))
    (local.get $res))
"#;

// recv_line / recv_all / recv_bytes: a fresh string of staged socket data. The
// host performs the read at the `_len` call and stages the bytes (newline
// trimming / lossy UTF-8 exactly as the interpreter does); the guest allocates
// `[len][bytes]` and `fill_pending` writes them.
const NET_RECV_LINE_WAT: &str = r#"  (func $net_recv_line (param $s i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $net_recv_line_len_host (local.get $s)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;

const NET_RECV_ALL_WAT: &str = r#"  (func $net_recv_all (param $s i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $net_recv_all_len_host (local.get $s)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;

const NET_RECV_BYTES_WAT: &str = r#"  (func $net_recv_bytes (param $s i32) (param $n i64) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $net_recv_bytes_len_host (local.get $s) (local.get $n)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;

// Float ordering with the interpreter's NaN semantics: comparing a NaN is a
// runtime error (NaN has no order), so each helper traps when either operand is
// NaN (`x != x`), otherwise it performs the IEEE comparison. Equality (`==`/`!=`)
// needs no helper — IEEE false/true for NaN already matches the interpreter.
const FLOAT_ORD_WAT: &str = r#"  (func $f_nan_guard (param $a f64) (param $b f64)
    (if (i32.or (f64.ne (local.get $a) (local.get $a)) (f64.ne (local.get $b) (local.get $b)))
      (then (unreachable))))
  (func $f_lt (param $a f64) (param $b f64) (result i32)
    (call $f_nan_guard (local.get $a) (local.get $b))
    (f64.lt (local.get $a) (local.get $b)))
  (func $f_le (param $a f64) (param $b f64) (result i32)
    (call $f_nan_guard (local.get $a) (local.get $b))
    (f64.le (local.get $a) (local.get $b)))
  (func $f_gt (param $a f64) (param $b f64) (result i32)
    (call $f_nan_guard (local.get $a) (local.get $b))
    (f64.gt (local.get $a) (local.get $b)))
  (func $f_ge (param $a f64) (param $b f64) (result i32)
    (call $f_nan_guard (local.get $a) (local.get $b))
    (f64.ge (local.get $a) (local.get $b)))
"#;

const CRYPTO_SHA256_WAT: &str = r#"  (func $crypto_sha256 (param $in i32) (result i32)
    (local $res i32)
    (call $ensure (i32.const 68))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (i32.const 64))
    (call $crypto_sha256_host (local.get $in) (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (local.get $res) (i32.const 68)))
    (local.get $res))
"#;

// rune_hash(paths, contents): the store's content hash of a rune's files — a
// fixed 71-byte `sha256:<64 hex>` string. The host walks both guest lists and
// runs the same native implementation the interpreter (and `src/pm/store.rs`)
// uses, so all three agree byte-for-byte.
const CRYPTO_RUNE_HASH_WAT: &str = r#"  (func $crypto_rune_hash (param $paths i32) (param $contents i32) (result i32)
    (local $res i32)
    (call $ensure (i32.const 75))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (i32.const 71))
    (call $crypto_rune_hash_host (local.get $paths) (local.get $contents) (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (local.get $res) (i32.const 75)))
    (local.get $res))
"#;

// footprint(src): the capability-footprint JSON of witchy source. The host
// computes and stages the JSON at the `_len` call (no compute/fill race), the
// guest allocates `[len][bytes]`, and `fill_pending` writes the bytes — the
// same staging protocol as `dir_read`.
const COMPILER_FOOTPRINT_WAT: &str = r#"  (func $compiler_footprint (param $src i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $compiler_footprint_len_host (local.get $src)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;

// diff(old, new): the footprint-diff JSON of two witchy sources — staged like
// `compiler_footprint`.
// field_str_get(idx): a fresh arena copy of a String state field's host cell —
// staged by `field_str_len`, written by `fill_pending` (the dir_read protocol).
const FIELD_STR_GET_WAT: &str = r#"  (func $field_str_get (param $idx i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $field_str_len_host (local.get $idx)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
    (global.set $heap (i32.add (i32.add (local.get $res) (i32.const 4)) (local.get $len)))
    (local.get $res))
"#;

const COMPILER_DIFF_WAT: &str = r#"  (func $compiler_diff (param $old i32) (param $new i32) (result i32)
    (local $len i32) (local $res i32)
    (local.set $len (call $compiler_diff_len_host (local.get $old) (local.get $new)))
    (call $ensure (i32.add (local.get $len) (i32.const 4)))
    (local.set $res (global.get $heap))
    (i32.store (local.get $res) (local.get $len))
    (call $fill_pending_host (i32.add (local.get $res) (i32.const 4)))
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

// A Dict is `[count:i32]` then `count` entries of 16 bytes each: an i64 key slot
// (at entry+0) and an i64 value slot (at entry+8), entry i at base + 4 + i*16.
// Keys and values use the universal i64 slot (a String/list key/value is a
// pointer in the low 32 bits), so a big `Int` key or value keeps its 64 bits.
const KEY_EQ_WAT: &str = r#"  (func $key_eq (param $a i64) (param $b i64) (param $mode i32) (result i32)
    (if (result i32) (i32.eqz (local.get $mode))
      (then (i64.eq (local.get $a) (local.get $b)))
      (else (if (result i32) (i32.eq (local.get $mode) (i32.const 1))
        (then (call $str_eq (i32.wrap_i64 (local.get $a)) (i32.wrap_i64 (local.get $b))))
        (else (f64.eq (f64.reinterpret_i64 (local.get $a)) (f64.reinterpret_i64 (local.get $b))))))))
"#;

// insert(d, k, v): a fresh map like `d` with `k` set to `v` — the matching
// entry's value replaced, or `(k, v)` appended (count+1) if `k` is absent.
const DICT_INSERT_WAT: &str = r#"  (func $dict_insert (param $d i32) (param $k i64) (param $v i64) (param $mode i32) (result i32)
    (local $count i32) (local $i i32) (local $found i32) (local $new i32) (local $bytes i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 20) (i32.mul (local.get $count) (i32.const 16))))
    (local.set $found (i32.const -1))
    (local.set $i (i32.const 0))
    (block $fdone
      (loop $f
        (br_if $fdone (i32.ge_s (local.get $i) (local.get $count)))
        (if (call $key_eq
              (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16))))
              (local.get $k) (local.get $mode))
          (then (local.set $found (local.get $i)) (br $fdone)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $f)))
    (local.set $bytes (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 16))))
    (local.set $new (global.get $heap))
    (memory.copy (local.get $new) (local.get $d) (local.get $bytes))
    (if (result i32) (i32.ge_s (local.get $found) (i32.const 0))
      (then
        (i64.store (i32.add (i32.add (local.get $new) (i32.const 12)) (i32.mul (local.get $found) (i32.const 16))) (local.get $v))
        (global.set $heap (i32.add (local.get $new) (local.get $bytes)))
        (local.get $new))
      (else
        (i32.store (local.get $new) (i32.add (local.get $count) (i32.const 1)))
        (i64.store (i32.add (local.get $new) (local.get $bytes)) (local.get $k))
        (i64.store (i32.add (i32.add (local.get $new) (local.get $bytes)) (i32.const 8)) (local.get $v))
        (global.set $heap (i32.add (i32.add (local.get $new) (local.get $bytes)) (i32.const 16)))
        (local.get $new))))
"#;

// get_or(d, k, default): the value for `k`, or `default` if absent.
const DICT_GET_OR_WAT: &str = r#"  (func $dict_get_or (param $d i32) (param $k i64) (param $default i64) (param $mode i32) (result i64)
    (local $count i32) (local $i i32)
    (local.set $count (i32.load (local.get $d)))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (if (call $key_eq
              (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16))))
              (local.get $k) (local.get $mode))
          (then (return (i64.load (i32.add (i32.add (local.get $d) (i32.const 12)) (i32.mul (local.get $i) (i32.const 16)))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.get $default))
"#;

// has(d, k): whether `k` is present.
const DICT_HAS_WAT: &str = r#"  (func $dict_has (param $d i32) (param $k i64) (param $mode i32) (result i32)
    (local $count i32) (local $i i32)
    (local.set $count (i32.load (local.get $d)))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (if (call $key_eq
              (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16))))
              (local.get $k) (local.get $mode))
          (then (return (i32.const 1))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.const 0))
"#;

// remove(d, k): a fresh map with the entry for `k` dropped (unchanged if
// absent). Copies every entry whose key isn't `k` into a new map.
const DICT_REMOVE_WAT: &str = r#"  (func $dict_remove (param $d i32) (param $k i64) (param $mode i32) (result i32)
    (local $count i32) (local $i i32) (local $new i32) (local $n i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 16))))
    (local.set $new (global.get $heap))
    (local.set $i (i32.const 0))
    (local.set $n (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (if (i32.eqz (call $key_eq
              (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16))))
              (local.get $k) (local.get $mode)))
          (then
            (i64.store (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $n) (i32.const 16)))
              (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16)))))
            (i64.store (i32.add (i32.add (local.get $new) (i32.const 12)) (i32.mul (local.get $n) (i32.const 16)))
              (i64.load (i32.add (i32.add (local.get $d) (i32.const 12)) (i32.mul (local.get $i) (i32.const 16)))))
            (local.set $n (i32.add (local.get $n) (i32.const 1)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (i32.store (local.get $new) (local.get $n))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $n) (i32.const 16))))
    (local.get $new))
"#;

// update(d, k, default, f): the upsert. Read the current value (or `default` if
// `k` is absent) as a universal i64 slot, apply the updater closure `f` to it
// (env = the closure pointer, code index = its first word), then reinsert under
// `k`. Equivalent to `insert(d, k, f(get_or(d, k, default)))`, but the closure
// call lives in this helper's own frame so call-site arg evaluation stays
// single-shot and nests cleanly.
const DICT_UPDATE_WAT: &str = r#"  (func $dict_update (param $d i32) (param $k i64) (param $default i64) (param $mode i32) (param $clos i32) (result i32)
    (local $new i64)
    (local.set $new
      (call_indirect (type $clos1)
        (local.get $clos)
        (call $dict_get_or (local.get $d) (local.get $k) (local.get $default) (local.get $mode))
        (i32.load (local.get $clos))))
    (call $dict_insert (local.get $d) (local.get $k) (local.get $new) (local.get $mode)))
"#;

// keys(d) / values(d): a fresh List (8-byte i64 slots) of the keys (or values),
// in insertion order — copied straight across (both are i64 slots already).
const DICT_KEYS_WAT: &str = r#"  (func $dict_keys (param $d i32) (result i32)
    (local $count i32) (local $i i32) (local $new i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 8))))
    (local.set $new (global.get $heap))
    (i32.store (local.get $new) (local.get $count))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (i64.store
          (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))
          (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $count) (i32.const 8))))
    (local.get $new))
"#;

const DICT_VALUES_WAT: &str = r#"  (func $dict_values (param $d i32) (result i32)
    (local $count i32) (local $i i32) (local $new i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 8))))
    (local.set $new (global.get $heap))
    (i32.store (local.get $new) (local.get $count))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (i64.store
          (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))
          (i64.load (i32.add (i32.add (local.get $d) (i32.const 12)) (i32.mul (local.get $i) (i32.const 16)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (global.set $heap (i32.add (i32.add (local.get $new) (i32.const 4)) (i32.mul (local.get $count) (i32.const 8))))
    (local.get $new))
"#;

// pairs(d): a List of `(key, value)` 2-tuples in insertion order. Each tuple is
// the codegen layout `[0][k][v]` with 8-byte slots, so `let (k, v) = entry`
// destructures it; the list itself holds the tuple pointers in 8-byte slots.
const DICT_PAIRS_WAT: &str = r#"  (func $dict_pairs (param $d i32) (result i32)
    (local $count i32) (local $i i32) (local $list i32) (local $tup i32)
    (local.set $count (i32.load (local.get $d)))
    (call $ensure (i32.add (i32.add (i32.const 4) (i32.mul (local.get $count) (i32.const 8))) (i32.mul (local.get $count) (i32.const 20))))
    (local.set $list (global.get $heap))
    (i32.store (local.get $list) (local.get $count))
    (global.set $heap (i32.add (i32.add (local.get $list) (i32.const 4)) (i32.mul (local.get $count) (i32.const 8))))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (local.set $tup (global.get $heap))
        (i32.store (local.get $tup) (i32.const 0))
        (i64.store (i32.add (local.get $tup) (i32.const 4))
          (i64.load (i32.add (i32.add (local.get $d) (i32.const 4)) (i32.mul (local.get $i) (i32.const 16)))))
        (i64.store (i32.add (local.get $tup) (i32.const 12))
          (i64.load (i32.add (i32.add (local.get $d) (i32.const 12)) (i32.mul (local.get $i) (i32.const 16)))))
        (global.set $heap (i32.add (local.get $tup) (i32.const 20)))
        (i64.store
          (i32.add (i32.add (local.get $list) (i32.const 4)) (i32.mul (local.get $i) (i32.const 8)))
          (i64.extend_i32_u (local.get $tup)))
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
      (then (return (call $list_push (local.get $result) (i64.extend_i32_u (local.get $s))))))
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
                (i64.extend_i32_u (call $substr (local.get $s) (local.get $start) (i32.sub (local.get $i) (local.get $start))))))
            (local.set $i (i32.add (local.get $i) (local.get $seplen)))
            (local.set $start (local.get $i)))
          (else
            (local.set $i (i32.add (local.get $i) (i32.const 1)))))
        (br $scan)))
    (local.set $result
      (call $list_push (local.get $result)
        (i64.extend_i32_u (call $substr (local.get $s) (local.get $start) (i32.sub (local.get $slen) (local.get $start))))))
    (local.get $result))
"#;

// str_chars(s): a list of the characters of `s`, each a single-char string.
// Counts chars via `$byte_to_char` of the byte length, then pushes each
// `$str_substring(s, i, i+1)`. Reuses the char-correct helpers (no new UTF-8
// decoding); builds the result list the same way `$split` does.
const STR_CHARS_WAT: &str = r#"  (func $str_chars (param $s i32) (result i32)
    (local $n i32) (local $i i32) (local $result i32)
    (local.set $n (call $byte_to_char (local.get $s) (i32.load (local.get $s))))
    (call $ensure (i32.const 4))
    (local.set $result (global.get $heap))
    (i32.store (local.get $result) (i32.const 0))
    (global.set $heap (i32.add (local.get $result) (i32.const 4)))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_s (local.get $i) (local.get $n)))
        (local.set $result
          (call $list_push (local.get $result)
            (i64.extend_i32_u (call $str_substring (local.get $s) (local.get $i) (i32.add (local.get $i) (i32.const 1))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
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

// str_to_int(s): parse a decimal integer from `s`, mirroring the interpreter's
// `s.trim().parse::<i64>()`: skip leading/trailing ASCII whitespace, accept an
// optional +/- sign then one or more digits, and accumulate in i64. Anything
// else — no digits, or leftover non-whitespace after the number — traps, matching
// the interpreter's parse error (rather than silently parsing a prefix). The
// safe wrapper `std/string.parse_int` pre-validates, so it never reaches a trap.
const STR_TO_INT_WAT: &str = r#"  (func $str_to_int (param $s i32) (result i64)
    (local $len i32) (local $i i32) (local $b i32) (local $acc i64) (local $neg i32) (local $got i32) (local $limit i64)
    (local.set $len (i32.load (local.get $s)))
    (local.set $i (i32.const 0))
    (local.set $acc (i64.const 0))
    (local.set $neg (i32.const 0))
    (local.set $got (i32.const 0))
    (block $wsdone
      (loop $ws
        (br_if $wsdone (i32.ge_s (local.get $i) (local.get $len)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (br_if $wsdone (i32.eqz (i32.or
          (i32.eq (local.get $b) (i32.const 32))
          (i32.or (i32.eq (local.get $b) (i32.const 9))
          (i32.or (i32.eq (local.get $b) (i32.const 10))
          (i32.or (i32.eq (local.get $b) (i32.const 13))
          (i32.or (i32.eq (local.get $b) (i32.const 11))
                  (i32.eq (local.get $b) (i32.const 12)))))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $ws)))
    (if (i32.lt_s (local.get $i) (local.get $len))
      (then
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (if (i32.eq (local.get $b) (i32.const 45))
          (then (local.set $neg (i32.const 1)) (local.set $i (i32.add (local.get $i) (i32.const 1))))
          (else (if (i32.eq (local.get $b) (i32.const 43))
            (then (local.set $i (i32.add (local.get $i) (i32.const 1)))))))))
    ;; Magnitude bound (unsigned): 2^63 for a negative value (|i64::MIN|), else
    ;; 2^63 - 1 (i64::MAX). The digit loop traps past it, matching Rust's checked
    ;; parse rather than silently wrapping.
    (local.set $limit (if (result i64) (local.get $neg)
      (then (i64.const -9223372036854775808))
      (else (i64.const 9223372036854775807))))
    (block $digdone
      (loop $dig
        (br_if $digdone (i32.ge_s (local.get $i) (local.get $len)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (br_if $digdone (i32.or
          (i32.lt_u (local.get $b) (i32.const 48))
          (i32.gt_u (local.get $b) (i32.const 57))))
        ;; Overflow if acc*10 + d would exceed `limit` (unsigned magnitude), i.e.
        ;; acc > (limit - d) / 10.
        (if (i64.gt_u (local.get $acc)
              (i64.div_u
                (i64.sub (local.get $limit) (i64.extend_i32_u (i32.sub (local.get $b) (i32.const 48))))
                (i64.const 10)))
          (then (unreachable)))
        (local.set $acc (i64.add (i64.mul (local.get $acc) (i64.const 10))
          (i64.extend_i32_u (i32.sub (local.get $b) (i32.const 48)))))
        (local.set $got (i32.const 1))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $dig)))
    (block $twsdone
      (loop $tws
        (br_if $twsdone (i32.ge_s (local.get $i) (local.get $len)))
        (local.set $b (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $i))))
        (br_if $twsdone (i32.eqz (i32.or
          (i32.eq (local.get $b) (i32.const 32))
          (i32.or (i32.eq (local.get $b) (i32.const 9))
          (i32.or (i32.eq (local.get $b) (i32.const 10))
          (i32.or (i32.eq (local.get $b) (i32.const 13))
          (i32.or (i32.eq (local.get $b) (i32.const 11))
                  (i32.eq (local.get $b) (i32.const 12)))))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $tws)))
    (if (i32.or (i32.eqz (local.get $got)) (i32.lt_s (local.get $i) (local.get $len)))
      (then (unreachable)))
    (if (result i64) (local.get $neg)
      (then (i64.sub (i64.const 0) (local.get $acc)))
      (else (local.get $acc))))
"#;

// is_ws(b): is byte `b` one of the six ASCII whitespace characters?
const IS_WS_WAT: &str = r#"  (func $is_ws (param $b i32) (result i32)
    (i32.or
      (i32.eq (local.get $b) (i32.const 32))
      (i32.or (i32.eq (local.get $b) (i32.const 9))
      (i32.or (i32.eq (local.get $b) (i32.const 10))
      (i32.or (i32.eq (local.get $b) (i32.const 13))
      (i32.or (i32.eq (local.get $b) (i32.const 11))
              (i32.eq (local.get $b) (i32.const 12))))))))
"#;

// trim(s): a fresh string with leading/trailing ASCII whitespace removed.
const TRIM_WAT: &str = r#"  (func $trim (param $s i32) (result i32)
    (local $len i32) (local $lo i32) (local $hi i32)
    (local.set $len (i32.load (local.get $s)))
    (local.set $lo (i32.const 0))
    (local.set $hi (local.get $len))
    (block $lodone
      (loop $l
        (br_if $lodone (i32.ge_s (local.get $lo) (local.get $hi)))
        (br_if $lodone (i32.eqz (call $is_ws
          (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (local.get $lo))))))
        (local.set $lo (i32.add (local.get $lo) (i32.const 1)))
        (br $l)))
    (block $hidone
      (loop $h
        (br_if $hidone (i32.le_s (local.get $hi) (local.get $lo)))
        (br_if $hidone (i32.eqz (call $is_ws
          (i32.load8_u (i32.add (i32.add (local.get $s) (i32.const 4)) (i32.sub (local.get $hi) (i32.const 1)))))))
        (local.set $hi (i32.sub (local.get $hi) (i32.const 1)))
        (br $h)))
    (call $substr (local.get $s) (local.get $lo) (i32.sub (local.get $hi) (local.get $lo))))
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
const INT_TO_STRING_WAT: &str = r#"  (func $int_to_string (param $n i64) (result i32)
    (local $mag i64) (local $t i64) (local $ndigits i32) (local $len i32) (local $res i32) (local $p i32) (local $neg i32)
    (call $ensure (i32.const 28))
    (if (result i32) (i64.eqz (local.get $n))
      (then
        (local.set $res (global.get $heap))
        (i32.store (local.get $res) (i32.const 1))
        (i32.store8 (i32.add (local.get $res) (i32.const 4)) (i32.const 48))
        (global.set $heap (i32.add (local.get $res) (i32.const 5)))
        (local.get $res))
      (else
        (local.set $neg (i64.lt_s (local.get $n) (i64.const 0)))
        (local.set $mag
          (if (result i64) (local.get $neg)
            (then (i64.sub (i64.const 0) (local.get $n)))
            (else (local.get $n))))
        (local.set $ndigits (i32.const 0))
        (local.set $t (local.get $mag))
        (block $b1
          (loop $l1
            (br_if $b1 (i64.eqz (local.get $t)))
            (local.set $ndigits (i32.add (local.get $ndigits) (i32.const 1)))
            (local.set $t (i64.div_u (local.get $t) (i64.const 10)))
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
            (br_if $b2 (i64.eqz (local.get $t)))
            (i32.store8 (local.get $p) (i32.add (i32.wrap_i64 (i64.rem_u (local.get $t) (i64.const 10))) (i32.const 48)))
            (local.set $p (i32.sub (local.get $p) (i32.const 1)))
            (local.set $t (i64.div_u (local.get $t) (i64.const 10)))
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
/// Names a lambda assigns but does not bind internally — i.e. writes to a
/// captured/outer variable. By-value capture cannot propagate these out, so every
/// backend rejects them; the type checker calls this so the rejection is uniform
/// (and identical to what codegen would detect) rather than backend-specific.
pub(crate) fn lambda_outer_assigns(params: &[Param], body: &Block) -> Vec<String> {
    scan_lambda(params, body).assigns_outer()
}

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
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => fv_expr(e, s),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn fv_expr(e: &Expr, s: &mut LambdaScan) {
    match e {
        // A range survives only inside a `for` iterator (its loop is lowered in
        // codegen, not the parser); scan its bounds for free variables. The
        // other sugar nodes are fully lowered before codegen.
        Expr::Range { lo, hi, .. } => {
            fv_expr(lo, s);
            fv_expr(hi, s);
        }
        Expr::Index { .. } | Expr::WhileLet { .. } | Expr::MethodCall { .. } | Expr::Record { .. } => {
            unreachable!("range/index sugar is lowered before codegen (parser::lower_sugar_module)")
        }
        Expr::Var(n) => {
            s.reads.insert(n.clone());
        }
        Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
        // A `Call` name is a function/builtin (or a closure local, caught at WASM
        // validation), never an outer value capture — only its args matter here.
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                fv_expr(x, s);
            }
        }
        // The callee name matters: it may be a captured function-valued local
        // (which must be pulled into the closure), not only a top-level
        // function. Non-local names are filtered out where captures are built.
        Expr::Call { name, args } => {
            s.reads.insert(name.clone());
            for a in args {
                fv_expr(a, s);
            }
        }
        Expr::Ctor { args, .. } | Expr::Spawn { args, .. } => {
            for a in args {
                fv_expr(a, s);
            }
        }
        Expr::Apply { func, args } => {
            fv_expr(func, s);
            for a in args {
                fv_expr(a, s);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => fv_expr(expr, s),
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
        Expr::Index { .. } | Expr::WhileLet { .. } | Expr::MethodCall { .. } | Expr::Record { .. } => {
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
        | Expr::Tuple(args)
        | Expr::Spawn { args, .. } => {
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
        | Expr::Lambda { .. } => {}
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
    // Every source name ever bound in this function. A WASM local has a single
    // type, so two *disjoint* scopes that reuse a name must still get distinct
    // locals — they can differ in kind (e.g. an i64 range loop var in one branch
    // and an i32 tuple destructure in another).
    seen: HashSet<String>,
}

impl Renamer {
    fn new() -> Self {
        Self { scopes: Vec::new(), counter: 0, seen: HashSet::new() }
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
        // First use of a name keeps it; any later binding of the same name (a
        // shadow, or a reuse in a sibling scope) gets a fresh unique local.
        let unique = if self.seen.insert(name.to_string()) {
            name.to_string()
        } else {
            self.counter += 1;
            format!("{name}__shadow{}", self.counter)
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
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Yield(e) => self.rename_expr(e),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }

    fn rename_expr(&mut self, e: &mut Expr) {
        match e {
            // A range survives only inside a `for` iterator; rename vars in its
            // bounds (e.g. a captured `n` in `0..n`). The other sugar nodes are
            // fully lowered before codegen.
            Expr::Range { lo, hi, .. } => {
                self.rename_expr(lo);
                self.rename_expr(hi);
            }
            Expr::Index { .. } | Expr::WhileLet { .. } | Expr::MethodCall { .. } | Expr::Record { .. } => {
                unreachable!("range/index sugar is lowered before codegen (parser::lower_sugar_module)")
            }
            Expr::Var(n) => *n = self.resolve(n),
            Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
            // Call / Ctor / Spawn names are functions / constructors / actors,
            // not locals — only the arguments are renamed.
            Expr::List(xs) | Expr::Tuple(xs) => {
                for x in xs {
                    self.rename_expr(x);
                }
            }
            Expr::Apply { func, args } => {
                self.rename_expr(func);
                for a in args {
                    self.rename_expr(a);
                }
            }
            Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::Spawn { args, .. } => {
                for a in args {
                    self.rename_expr(a);
                }
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => self.rename_expr(expr),
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

    #[test]
    fn build_module_is_zero_ambient() {
        // A compiled build step imports ONLY its build host functions — none of
        // the runtime authority. That's the structural zero-ambient guarantee:
        // the dangerous host functions don't exist for the guest to call.
        let module = parse_module(
            "fn build(out: BuildOut, schema: BuildRead):\n    write_out(out, \"x.witchy\", read_build(schema, \"a.proto\"))\n",
        )
        .expect("parse");
        let wat = compile_build_module(&module).expect("compile build module");
        assert!(wat.contains("(export \"run\")"), "build entrypoint becomes the run export");
        assert!(wat.contains("build_out_write"), "write_out import present");
        assert!(wat.contains("build_read_len"), "read_build import present");
        // No runtime-authority imports leaked in.
        for forbidden in ["dir_write", "dir_read_len", "net_connect", "net_listen", "\"print\"", "\"now\"", "crypto.sign"] {
            assert!(!wat.contains(forbidden), "build module must not import `{forbidden}`:\n{wat}");
        }
    }

    fn run_int(src: &str) -> i64 {
        let module = parse_module(src).expect("parse");
        let wat = compile_module(&module).expect("compile");
        let engine = Engine::default();
        let wt = WtModule::new(&engine, &wat).expect("valid wat");
        let captured = Arc::new(Mutex::new(None));
        let mut linker = Linker::new(&engine);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap("witchy", "print_int", move |n: i64| {
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
fn half(x: Float) -> Float:
    (x / 2.0)

fn main() -> Float:
    (half(7.0) + 1.5)
"#;
        assert_eq!(run_float(src), 5.0); // 3.5 + 1.5
    }

    #[test]
    fn float_valued_if_compiles() {
        // An `if/else` whose branches are Float must yield an f64 result (the
        // `if` result type follows the branch kind, not a hardcoded i32).
        let src = r#"
fn pick(a: Float, b: Float) -> Float:
    if (a < b):
        a
    else:
        b

fn main() -> Float:
    (pick(2.5, 7.5) + pick(9.0, 1.0))
"#;
        assert_eq!(run_float(src), 3.5); // min(2.5,7.5)=2.5 + min(9.0,1.0)=1.0
    }

    #[test]
    fn large_int_literals_compile() {
        // Compiled Int is i64, so a literal beyond the 32-bit range round-trips
        // (it no longer wraps or is rejected), matching the interpreter.
        assert_eq!(run_int("fn main() -> Int:\n    3000000000\n"), 3_000_000_000);
        assert_eq!(
            run_int("fn main() -> Int:\n    9000000000000\n"),
            9_000_000_000_000
        );
    }

    #[test]
    fn float_record_field_compiles() {
        // 8-byte heap slots hold an f64 field; float_to_int reads it back.
        let src = r#"
type Vec2:
    x: Float
    y: Float

fn main() -> Int:
    let v = Vec2(1.5, 2.5)
    float_to_int((v).x)
"#;
        assert_eq!(run_int(src), 1);
    }

    #[test]
    fn float_list_element_compiles() {
        // 8-byte heap slots hold an f64, so floats now live in lists.
        let src = r#"
fn main() -> Int:
    let xs = [1.5, 2.5]
    length(xs)
"#;
        assert_eq!(run_int(src), 2);
    }

    #[test]
    fn compiles_non_capturing_closure() {
        // A non-capturing lambda passed to a higher-order function: lifted to a
        // table slot, then invoked via `call_indirect`.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    apply(fn(n: Int): (n * n), 9)
"#;
        assert_eq!(run_int(src), 81);
    }

    #[test]
    fn compiles_multiple_closures() {
        // Two distinct lambdas take distinct table slots and call_indirect each.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    let a = apply(fn(n: Int): (n + 1), 10)
    let b = apply(fn(n: Int): (n * 3), 10)
    (a + b)
"#;
        assert_eq!(run_int(src), 41); // 11 + 30
    }

    #[test]
    fn closure_can_call_global_function() {
        // A lambda calling a top-level function is still non-capturing.
        let src = r#"
fn dbl(x: Int) -> Int:
    (x * 2)

fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    apply(fn(n: Int): (dbl(n) + 1), 4)
"#;
        assert_eq!(run_int(src), 9); // dbl(4) + 1
    }

    #[test]
    fn compiles_capturing_closure() {
        // The lambda reads `k` from the enclosing scope: captured by value into
        // the closure's heap environment, then read back via the env prologue.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    let k = 100
    apply(fn(n: Int): (n + k), 5)
"#;
        assert_eq!(run_int(src), 105);
    }

    #[test]
    fn closure_captures_multiple_vars() {
        // Several captures land in distinct environment slots.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    let a = 3
    let b = 7
    let c = 11
    apply(fn(n: Int): (((n * a) + b) - c), 10)
"#;
        assert_eq!(run_int(src), 26); // 10*3 + 7 - 11
    }

    #[test]
    fn closure_captures_record_field() {
        // Capturing a record value: the env carries the heap pointer, and field
        // access still resolves inside the lambda.
        let src = r#"
type Point:
    x: Int
    y: Int

fn apply(f: fn(Int) -> Int, n: Int) -> Int:
    f(n)

fn main() -> Int:
    let p = Point(4, 9)
    apply(fn(n: Int): (n + ((p).x * (p).y)), 1)
"#;
        assert_eq!(run_int(src), 37); // 1 + 4*9
    }

    #[test]
    fn closure_assigning_captured_var_is_rejected() {
        // By-value capture cannot propagate a write back to the outer binding, so
        // assigning a captured variable is rejected rather than diverging.
        let src = r#"
fn run(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)
fn main() -> Int:
    var total = 0
    let add = fn(n: Int):
        total = total + n
    run(add, 5)
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
        captured.lock().unwrap().clone()
    }

    #[test]
    fn compiles_arithmetic() {
        assert_eq!(run_int(r#"
fn main() -> Int:
    (1 + (2 * 3))
"#), 7);
    }

    #[test]
    fn full_int_program() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn fib(n: Int) -> Int:
    if (n < 2):
        n
    else:
        (fib((n - 1)) + fib((n - 2)))

fn main() -> Int:
    let a = double(21)
    let b = fib(10)
    (a + b)
"#;
        assert_eq!(run_int(src), 97);
    }

    #[test]
    fn compiles_int_float_conversions() {
        // int_to_float(7) / 2.0 = 3.5; float_to_int(3.5) = 3
        assert_eq!(
            run_int("fn main() -> Int:\n    float_to_int(int_to_float(7) / 2.0)\n"),
            3
        );
    }

    #[test]
    fn compiles_string_length() {
        assert_eq!(run_int(r#"
fn main() -> Int:
    string_length("hello")
"#), 5);
    }

    #[test]
    fn compiles_while_and_mod() {
        // sum of multiples of 3 below 10: 0 + 3 + 6 + 9
        let src = r#"
fn main() -> Int:
    var i = 0
    var total = 0
    while (i < 10):
        if ((i % 3) == 0):
            total = (total + i)
        i = (i + 1)
    total
"#;
        assert_eq!(run_int(src), 18);
    }

    #[test]
    fn compiles_boolean_ops() {
        let src = r#"
fn in_range(n: Int) -> Int:
    if ((n > 0) && (n < 10)):
        1
    else:
        0

fn main() -> Int:
    ((in_range(5) + in_range(50)) + in_range((-3)))
"#;
        assert_eq!(run_int(src), 1); // 1 + 0 + 0
    }

    #[test]
    fn compiles_boolean_not() {
        assert_eq!(run_int("fn main() -> Int:\n    if !(1 == 2): 7 else: 0\n"), 7);
    }

    #[test]
    fn compiles_match_with_guards() {
        let src = r#"
fn sign(n: Int) -> Int:
    match n:
        0 -> 0
        m if (m > 0) -> 1
        _ -> (0 - 1)

fn main() -> Int:
    ((sign(5) + sign((-3))) + sign(0))
"#;
        assert_eq!(run_int(src), 0); // 1 + (-1) + 0
    }

    #[test]
    fn compiles_adts_and_constructor_patterns() {
        // Constructors become heap records [tag][fields...]; ctor patterns load
        // the tag and bind fields.
        let src = r#"
type Shape:
    Circle(Int)
    Square(Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> ((3 * r) * r)
        Square(w) -> (w * w)

fn main() -> Int:
    (area(Circle(10)) + area(Square(5)))
"#;
        assert_eq!(run_int(src), 325);
    }

    #[test]
    fn compiles_lists() {
        let src = r#"
fn main() -> Int:
    let xs = [10, 20, 30]
    ((length(xs) + at(xs, 0)) + at(xs, 2))
"#;
        assert_eq!(run_int(src), 43); // 3 + 10 + 30
    }

    #[test]
    fn compiles_nested_constructor_patterns() {
        let src = r#"
type Point:
    Point(Int, Int)

type Shape:
    Dot(Point)
    Pair(Point, Point)

fn x_of(s: Shape) -> Int:
    match s:
        Dot(Point(x, _)) -> x
        Pair(Point(x, _), _) -> x

fn main() -> Int:
    (x_of(Dot(Point(7, 9))) + x_of(Pair(Point(3, 0), Point(0, 0))))
"#;
        assert_eq!(run_int(src), 10); // 7 + 3
    }

    #[test]
    fn compiles_string_patterns() {
        let src = r#"
fn classify(s: String) -> Int:
    match s:
        "yes" -> 1
        "no" -> 0
        _ -> (0 - 1)

fn main() -> Int:
    ((classify("yes") + classify("no")) + classify("maybe"))
"#;
        assert_eq!(run_int(src), 0); // 1 + 0 + (-1)
    }

    #[test]
    fn compiles_match_and_recursion() {
        let src = r#"
fn fact(n: Int) -> Int:
    match n:
        0 -> 1
        _ -> (n * fact((n - 1)))

fn main() -> Int:
    fact(5)
"#;
        assert_eq!(run_int(src), 120);
    }

    #[test]
    fn compiles_inout_writeback() {
        // `inout` compiles to move-in / move-out: bump returns the updated n,
        // and the caller writes it back into x.
        let src = r#"
fn bump(inout n: Int):
    n = (n + 1)

fn main() -> Int:
    var x = 41
    bump(x)
    bump(x)
    x
"#;
        assert_eq!(run_int(src), 43);
    }

    #[test]
    fn actor_arena_is_reset_each_message() {
        let src = r#"
actor Counter:
    console: Console
    var count: Int = 0

impl Counter:
    on Tick():
        count = (count + 1)
        print(console, ("n=" <> int_to_string(count)))
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
        // The host resets the arena via `__msg_prep` before each delivery
        // (exactly what `actor_system::System::invoke` does).
        let prep = instance.get_typed_func::<(), ()>(&mut store, "__msg_prep").unwrap();
        let tick = instance.get_typed_func::<(), ()>(&mut store, "Tick").unwrap();
        prep.call(&mut store, ()).unwrap();
        tick.call(&mut store, ()).unwrap();
        prep.call(&mut store, ()).unwrap();
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
fn shout(name: String) -> String:
    ("hello, " <> name)

fn main(console: Console):
    print(console, shout("witchy"))
"#;
        assert_eq!(run_str(src), vec!["hello, witchy"]);
    }

    #[test]
    fn compiles_int_to_string() {
        let src = r#"
fn main(console: Console):
    print(console, int_to_string(12345))
"#;
        assert_eq!(run_str(src), vec!["12345"]);
    }

    #[test]
    fn int_to_string_handles_zero() {
        let src = r#"
fn main(console: Console):
    print(console, int_to_string(0))
"#;
        assert_eq!(run_str(src), vec!["0"]);
    }

    /// The headline: an actor compiled to its own WASM VM, with Int state in a
    /// global and a Console capability, handling messages run-to-completion.
    #[test]
    fn compiles_a_stateful_actor() {
        let src = r#"
actor Counter:
    console: Console
    var count: Int = 0

impl Counter:
    on Tick():
        count = (count + 1)
        print(console, ("count is " <> int_to_string(count)))
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
