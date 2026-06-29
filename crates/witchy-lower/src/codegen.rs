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

use crate::analysis::{self, is_self_assign_shape, self_concat_pieces, self_insert_args, self_push_elem, self_set_at, self_update_args, self_update_at};
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

/// Scratch local holding the Result/Option being unwrapped by `?`.
const TRY_TMP: &str = "__witchy_try_tmp";

/// Scratch local holding a `match` scrutinee while arms test it.
const MATCH_TMP: &str = "__witchy_match_tmp";

/// Scratch local holding a `SecretStore.get` handle (the host-table index) so it
/// is fetched once and reused for both the present-test and the `Some` payload.
const SECRET_TMP: &str = "__witchy_secret_tmp";

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
}

struct Codegen {
    strings: Vec<(String, u32)>,
    next_offset: u32,
    uses_print: bool,
    uses_print_int: bool,
    uses_concat: bool,
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
    /// The active compile unit's uniqueness facts + (kills consumed, sites
    /// seen) for the post-compile consumption check; units nest via lambdas.
    facts_stack: Vec<(analysis::Facts, usize, usize)>,
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
            uses_str_eq: false,
            uses_print_float: false,
            locals: HashMap::new(),
            fn_ret: HashMap::new(),
            fn_ret_closure_kind: HashMap::new(),
            fn_ret_tuple_slots: HashMap::new(),
            fn_ret_list_elem_tuple_slots: HashMap::new(),
            fn_ret_tuple_slot_list_elem: HashMap::new(),
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
            cur_fn_var: false,
            cur_fn_var_params: Vec::new(),
            uses_list_at: false,
            uses_list_push: false,
            uses_list_concat: false,
            uses_list_drop: false,
            uses_starts_with: false,
            uses_crypto_ed25519_verify: false,
            uses_crypto_sha256: false,
            uses_crypto_rune_hash: false,
            inplace_push: HashSet::new(),
            sroa_candidates: HashSet::new(),
            sroa_active: HashMap::new(),
            facts_stack: Vec::new(),
            summaries: analysis::Summaries::empty(),
            cur_fn_own_param: None,
            cur_fn_has_type_vars: false,
            cur_fn_name: String::new(),
            type_table: witchy_types::typeck::TypeTable::default(),
            uses_list_push_cap: false,
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
                UnOp::Neg | UnOp::BitNot | UnOp::Move | UnOp::Await => self.kind_of(expr),
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
                if field.parse::<usize>().is_ok() {
                    return valtype_kind(self.val_type_of(e));
                }
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
            Expr::Call { name, args } if name == "dict.get_or" && args.len() == 3 => {
                self.kind_of(&args[2])
            }
            Expr::Call { name, .. } => match name.as_str() {
                "math.to_float" => Kind::F64,
                "math.to_int" | "string.length" | "string.char_count" | "string.index_of"
                | "list.length" | "dict.length" | "string.to_int" | "int_to_duration"
                | "duration_to_int" | "now" | "rand_u64" => Kind::I64,
                "list.at" => self.elem_kind_of_list_arg(e),
                "__render" | "int_to_string" | "print" => Kind::I32,
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
            if name == "list.at" {
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

    fn val_type_of_inner(&self, e: &Expr) -> ValType {
        match e {
            Expr::Int(_) | Expr::Duration(_) => ValType::Int,
            Expr::Bool(_) => ValType::Bool,
            Expr::Float(_) => ValType::Float,
            Expr::Str(_) => ValType::Str,
            Expr::Unary { op, expr } => match op {
                UnOp::Not => ValType::Bool,
                UnOp::Neg | UnOp::Move | UnOp::Await => self.val_type_of(expr),
                UnOp::BitNot => ValType::Int,
            },
            Expr::Binary { op, lhs, rhs } => match op {
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
                | BinOp::And => ValType::Bool,
                // Non-Bool `||` is the truthy fallback, so it yields its operand type.
                BinOp::Or => self.val_type_of(lhs),
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
            Expr::Call { name, args } if name == "list.at" && !args.is_empty() => {
                self.elem_val_type_of(&args[0])
            }
            // `get_or(d, k, default)` returns the Dict's value type, which is the
            // default's type — so a `let v = get_or(d, k, 0)` (or a String default)
            // tracks `v`, and `v` can in turn be used as a Dict key.
            Expr::Call { name, args } if name == "dict.get_or" && args.len() == 3 => {
                self.val_type_of(&args[2])
            }
            Expr::Call { name, .. } => match name.as_str() {
                "__render" | "string.to_upper" | "string.to_lower" | "string.trim"
                | "string.replace" | "string.substring" | "crypto.sha256" | "crypto.sign"
                | "crypto.public_key" | "crypto.reveal" | "read" | "read_build" | "crypto.rune_hash"
                | "exec"
                | "compiler.footprint"
                | "compiler.diff" | "compiler.doc" | "regex.match_spans" | "recv_line" | "recv_all"
                | "crypto.sha512" | "crypto.sha3_256" | "crypto.hmac_sha256"
                | "recv_bytes" => ValType::Str,
                "string.starts_with" | "string.ends_with" | "string.contains" | "dict.contains_key"
                | "exists" | "is_dir" | "crypto.ed25519_verify"
                | "crypto.ecdsa_p256_verify" | "crypto.ecdsa_p256_verify_hex"
                | "crypto.rsa_pkcs1_sha256_verify" => ValType::Bool,
                "string.length" | "string.char_count" | "string.index_of" | "list.length"
                | "dict.length" | "math.to_int" | "string.to_int" | "int_to_duration"
                | "duration_to_int" | "now" | "rand_u64" => ValType::Int,
                "math.to_float" | "math.sqrt" => ValType::Float,
                other => self.fn_ret_valtype.get(other).copied().unwrap_or(ValType::Other),
            },
            // `inner?` yields the Ok/Some payload's value type, so `to_string` of
            // a `?`-unwrapped value renders correctly and `==` picks `$str_eq`.
            Expr::Try(inner) => self.match_payload_valtype(inner).unwrap_or(ValType::Other),
            // A record field access (`p.x`): the field's declared value type — so
            // `"${p.x}"` / `__render(p.x)` and `==` on a field resolve.
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
            // A record-typed variable. Local tracking is primary; when it misses
            // (e.g. a `match` binding whose scrutinee is a closure-parameter call,
            // whose return shape codegen can't infer locally) fall back to typeck's
            // annotation, which knows the binding's record type.
            Expr::Var(v) => self.local_records.get(v).cloned().or_else(|| {
                match self.type_table.type_of(e).and_then(witchy_types::typeck::ty_to_ast) {
                    Some(witchy_syntax::ast::Type::Named(n, _)) if self.record_fields.contains_key(&n) => Some(n),
                    _ => None,
                }
            }),
            Expr::Call { name, args } => {
                if let Some(ty) = self.fn_ret_records.get(name) {
                    Some(ty.clone())
                } else if name == "dict.get_or" {
                    args.get(2).and_then(|d| self.record_type_of(d))
                } else if name == "list.at" {
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
    /// so a field access resolves its value type (`__render(p.x)`) and its
    /// structural shape (`__render(p.tags)`), not just whether it is a record.
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
                        if fname == "list.at" && args.len() == 2 {
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
                if args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()))
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
        self.cur_fn_var = f.params.iter().any(|p| p.convention == Convention::Var);
        self.cur_fn_var_params = f
            .params
            .iter()
            .filter(|p| p.convention == Convention::Var)
            .map(|p| p.name.clone())
            .collect();

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
        // Shadow `${v}__cap` ownership-token slots for the in-place accumulators.
        // The own-ABI parameter's token is a param (above), not a local.
        let mut cap_vars: Vec<&String> = self.inplace_push.iter().collect();
        cap_vars.sort();
        for v in cap_vars {
            if Some(v.as_str()) != self.cur_fn_own_param.as_deref() {
                locals.push(WirLocal { name: format!("{v}__cap"), ty: i32t() });
            }
        }
        locals.push(WirLocal { name: "__witchy_owncap".into(), ty: i32t() });
        locals.push(WirLocal { name: TUPLE_TMP.into(), ty: i32t() });
        locals.push(WirLocal { name: TRY_TMP.into(), ty: i32t() });
        locals.push(WirLocal { name: MATCH_TMP.into(), ty: i64t() });
        locals.push(WirLocal { name: SECRET_TMP.into(), ty: i32t() });
        // Scratch slots for the inlined in-place `set_at` fast path (index i32,
        // value i64): the common in-bounds + owned case stores directly without a
        // `$list_set_cap` call; the helper is only invoked for OOB / re-own.
        locals.push(WirLocal { name: "__witchy_set_idx".into(), ty: i32t() });
        locals.push(WirLocal { name: "__witchy_set_val".into(), ty: i64t() });
        for i in 0..WM_POOL {
            locals.push(WirLocal { name: format!("__witchy_wm_{i}"), ty: i32t() });
        }
        for i in 0..APPLY_POOL {
            locals.push(WirLocal { name: format!("__witchy_call_{i}"), ty: i32t() });
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
        self.facts_stack.push((facts, 0, 0));
    }

    /// End a compile unit, asserting every analysis entry was consumed — a
    /// cloned-subtree bug (compiling different AST nodes than were analyzed)
    /// surfaces here as a loud error, never as a lost cap kill.
    fn finish_unit(&mut self, unit: &str) -> Result<(), CodegenError> {
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
                    // (RFC-0027) Scalar-replace a frame-confined aggregate: store
                    // each field into a `${name}$<i>` i64-slot local instead of
                    // allocating a heap object. Falls through to the normal path if
                    // any field can't lower (then the name never enters sroa_active,
                    // so its field reads stay heap loads — consistent).
                    let mut sroa_done = false;
                    if self.sroa_candidates.contains(name) {
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
                    if !sroa_done {
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
                // `let (a, b, ..) = tuple`: store once, then load each 8-byte slot.
                Stmt::LetTuple { names, value } => {
                    let v = self.lower_expr(value)?;
                    seq.push(N::SetLocal { local: TUPLE_TMP.to_string(), value: v });
                    for (i, name) in names.iter().enumerate() {
                        let k = self.locals.get(name).copied().unwrap_or(Kind::I32);
                        let addr = W::Binary {
                            op: witchy_wir::wir::BinOp::Add,
                            kind: witchy_wir::wir::Kind::I32,
                            lhs: Box::new(W::GetLocal(TUPLE_TMP.to_string())),
                            rhs: Box::new(W::ConstI32((4 + 8 * i) as i32)),
                        };
                        seq.push(N::SetLocal {
                            local: name.clone(),
                            value: W::FromSlot(
                                Box::new(W::Load {
                                    ptr: Box::new(addr),
                                    kind: witchy_wir::wir::Kind::I64,
                                    offset: 0,
                                }),
                                Self::wir_kind(k),
                            ),
                        });
                    }
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
                    // own-ABI self-call (binary only): `xs = grow(move xs, …)`
                    // against a callee whose `own` buffer param may be returned.
                    // The callee returns `(value, cap)` and takes the caller's
                    // ownership token as a trailing i32 arg — so thread `xs__cap`
                    // in and capture (value → xs, cap → xs__cap) via CallStoreMulti.
                    if let Some((callee, _)) = self
                        .collect_wir
                        .then(|| analysis::self_own_call(name, value, &self.summaries))
                        .flatten()
                    {
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
                        && is_self_assign_shape(name, value, &self.summaries)
                        && self_push_elem(name, value).is_some()
                    {
                        // Only the list-push shape has an in-place fast path. A dict/
                        // string self-assign (`self_push_elem` is None) falls through
                        // to the plain value-rebind below — correct value semantics,
                        // just without the O(1) in-place mutation.
                        let elem = self_push_elem(name, value).expect("guarded Some above");
                        let xk = self.kind_of(elem);
                        // A dirty site (its RHS embeds an aliasing share of `name`)
                        // forces a zero token → re-own + copy; a clean site trusts
                        // the runtime token. Read-only here; `sites` consumed at end.
                        let dirty = match self.facts_stack.last() {
                            Some((facts, _, _)) if facts.accumulators.contains(name) => {
                                facts.is_dirty(stmt)
                            }
                            _ => true,
                        };
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
                        inplace_sites += 1;
                        tail_is_value = false;
                    } else if self.collect_wir
                        && self.inplace_push.contains(name)
                        && is_self_assign_shape(name, value, &self.summaries)
                        && self_set_at(name, value).is_some()
                    {
                        // `xs = list.set_at(xs, i, v)`: in-place element store via
                        // `$list_set_cap` (mutate the owned buffer's slot, O(1)),
                        // mirroring the list-push fast path. Without it the plain
                        // rebind rebuilds the whole list each set — O(n²) memory
                        // that traps a large list under the memory cap. A dirty
                        // site forces a zero token (re-own + copy, preserving any
                        // alias); a clean site mutates the owned buffer.
                        let (iexpr, vexpr) = self_set_at(name, value).expect("guarded Some above");
                        let ik = self.kind_of(iexpr);
                        let vk = self.kind_of(vexpr);
                        let dirty = match self.facts_stack.last() {
                            Some((facts, _, _)) if facts.accumulators.contains(name) => {
                                facts.is_dirty(stmt)
                            }
                            _ => true,
                        };
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
                        let slot_ptr = bin(
                            BinOp::Add,
                            bin(BinOp::Add, W::GetLocal(name.clone()), W::ConstI32(4)),
                            bin(BinOp::Mul, si(), W::ConstI32(8)),
                        );
                        seq.push(N::If {
                            cond,
                            then_: vec![N::Store { ptr: slot_ptr, value: sv(), kind: witchy_wir::wir::Kind::I64, offset: 0 }],
                            els: vec![N::CallStoreMulti {
                                func: "list_set_cap".to_string(),
                                args: vec![W::GetLocal(name.clone()), si(), sv(), cap],
                                dests: vec![name.clone(), format!("{name}__cap")],
                            }],
                            result: None,
                        });
                        inplace_sites += 1;
                        tail_is_value = false;
                    } else if self.collect_wir
                        && self.inplace_push.contains(name)
                        && is_self_assign_shape(name, value, &self.summaries)
                        && self_update_at(name, value).is_some()
                    {
                        // `xs = list.update_at(xs, i, f)`: in-place element update
                        // via `$list_update_cap` (apply the closure to the owned
                        // slot, O(1)), mirroring the set_at fast path. Without it the
                        // plain rebind copies the whole list each update — O(n²)
                        // memory. A dirty site forces a zero token (re-own + copy,
                        // preserving any alias); a clean site mutates in place.
                        let (iexpr, fexpr) = self_update_at(name, value).expect("guarded Some above");
                        let ik = self.kind_of(iexpr);
                        let dirty = match self.facts_stack.last() {
                            Some((facts, _, _)) if facts.accumulators.contains(name) => {
                                facts.is_dirty(stmt)
                            }
                            _ => true,
                        };
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
                        inplace_sites += 1;
                        tail_is_value = false;
                    } else if self.collect_wir
                        && self.inplace_push.contains(name)
                        && is_self_assign_shape(name, value, &self.summaries)
                        && self_insert_args(name, value).is_some()
                    {
                        // `d = dict.insert(d, k, v)`: the in-place dict upsert via
                        // `$dict_insert_cap` (O(1) amortized into owned entry slack),
                        // mirroring the list-push fast path. Without it the plain
                        // rebind below copies the whole dict each insert — O(n²)
                        // memory that traps a large dict under a tight memory cap.
                        let (kexpr, vexpr) = self_insert_args(name, value).expect("guarded Some above");
                        let mode = self.dict_key_mode_wir(kexpr)?;
                        let kk = self.kind_of(kexpr);
                        let vk = self.kind_of(vexpr);
                        if let Some(kvt) = self.dict_key_valtype_of(value) {
                            self.local_dict_key_valtype.insert(name.clone(), kvt);
                        }
                        if let Some(vvt) = self.dict_value_valtype_of(value) {
                            self.local_dict_value_valtype.insert(name.clone(), vvt);
                        }
                        let dirty = match self.facts_stack.last() {
                            Some((facts, _, _)) if facts.accumulators.contains(name) => {
                                facts.is_dirty(stmt)
                            }
                            _ => true,
                        };
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
                        inplace_sites += 1;
                        tail_is_value = false;
                    } else if self.collect_wir
                        && self.inplace_push.contains(name)
                        && is_self_assign_shape(name, value, &self.summaries)
                        && self_concat_pieces(name, value).is_some()
                    {
                        // `s = s + a + b`: the in-place string builder via
                        // `$str_append_cap` (append each piece into owned byte
                        // slack), mirroring the list/dict fast paths. Without it the
                        // plain rebind re-concatenates the whole string each
                        // statement — O(n²) bytes for a growing buffer. A dirty
                        // first piece forces a zero token (re-own → grow-and-copy,
                        // preserving any alias); later pieces reuse the fresh slack.
                        let pieces = self_concat_pieces(name, value).expect("guarded Some above");
                        let dirty = match self.facts_stack.last() {
                            Some((facts, _, _)) if facts.accumulators.contains(name) => {
                                facts.is_dirty(stmt)
                            }
                            _ => true,
                        };
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
                        inplace_sites += 1;
                        tail_is_value = false;
                    } else if self.collect_wir
                        && self.inplace_push.contains(name)
                        && is_self_assign_shape(name, value, &self.summaries)
                        && self_update_args(name, value).is_some()
                    {
                        // `d = dict.update(d, k, dflt, f)`: the in-place upsert via
                        // `$dict_update_cap` (apply the closure, reinsert into owned
                        // slack), mirroring the dict-insert fast path. Without it the
                        // plain rebind copies the whole dict each update.
                        let (kexpr, dexpr, fexpr) =
                            self_update_args(name, value).expect("guarded Some above");
                        let mode = self.dict_key_mode_wir(kexpr)?;
                        let kk = self.kind_of(kexpr);
                        let dk = self.kind_of(dexpr);
                        if let Some(kvt) = self.dict_key_valtype_of(value) {
                            self.local_dict_key_valtype.insert(name.clone(), kvt);
                        }
                        if let Some(vvt) = self.dict_value_valtype_of(value) {
                            self.local_dict_value_valtype.insert(name.clone(), vvt);
                        }
                        let dirty = match self.facts_stack.last() {
                            Some((facts, _, _)) if facts.accumulators.contains(name) => {
                                facts.is_dirty(stmt)
                            }
                            _ => true,
                        };
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
                        inplace_sites += 1;
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
    fn lower_aggregate(&mut self, header: i32, items: &[Expr]) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        let n = items.len();
        self.mk_arities.insert(n);
        let mut args = Vec::with_capacity(n + 1);
        args.push(W::ConstI32(header));
        for item in items {
            let k = self.kind_of(item);
            let w = self.lower_expr(item)?;
            args.push(W::ToSlot(Box::new(w), Self::wir_kind(k)));
        }
        Some(W::Call { func: format!("mk{n}"), args })
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
                self.uses_str_eq = true;
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
        let scrut_w = self.lower_expr(scrutinee)?;
        let id = self.next_label;
        self.next_label += 1;
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
                    return None;
                }
            };
            arm_body.push(N::Push(Self::wir_convert(b, body_kind, result_kind)));
            arm_body.push(N::Br { target: format!("d{id}"), cond: None });
            arm_blocks.push(N::Block { label: a_label, result: None, body: arm_body });
        }
        arm_blocks.push(N::Unreachable);
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
                let call = W::CallIndirect {
                    type_arity: n,
                    args: ci_args,
                    index: Box::new(W::Load { ptr: Box::new(W::GetLocal(tmp.clone())), kind: witchy_wir::wir::Kind::I32, offset: 0 }),
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
            Expr::List(items) => return self.lower_aggregate(items.len() as i32, items),
            Expr::Tuple(items) => return self.lower_aggregate(0, items),
            Expr::Ctor { name, args } => {
                let &(tag, nfields) = self.ctors.get(name)?;
                if nfields != args.len() {
                    return None; // arity mismatch → legacy emits the loud error
                }
                return self.lower_aggregate(tag as i32, args);
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
                    // Non-Bool `||` is the truthy fallback `a || b` ≡ `if truthy(a):
                    // a else: b`, evaluating `a` once. Every emptyable value is a
                    // pointer whose first word is a length (String/List) or variant
                    // tag (Option); "" / [] / None all have a zero first word, so
                    // "truthy" is a single `load i32` — no per-type branching.
                    if *op == BinOp::Or && self.val_type_of(lhs) != ValType::Bool {
                        use witchy_wir::wir::WirNode as N;
                        // Option is truthy when present: `Some` is the tag-0 (success)
                        // variant and `None` is non-zero — the inverse of String/List,
                        // whose first word is a length that is zero only when empty. So
                        // the Option predicate is `header == 0`, the rest `header != 0`.
                        let is_option = matches!(lhs.as_ref(), Expr::Ctor { name, .. } if name == "None" || name == "Some")
                            || matches!(
                                self.type_table.type_of(lhs).and_then(witchy_types::typeck::ty_to_ast),
                                Some(witchy_syntax::ast::Type::Named(ref n, _)) if n == "Option"
                            );
                        let tmp = TRY_TMP.to_string();
                        let lhs_w = self.lower_expr(lhs)?;
                        let rhs_w = self.lower_expr(rhs)?;
                        let header = W::Load {
                            ptr: Box::new(W::GetLocal(tmp.clone())),
                            kind: witchy_wir::wir::Kind::I32,
                            offset: 0,
                        };
                        let cond = if is_option {
                            W::Unary {
                                op: witchy_wir::wir::UnOp::Not,
                                kind: witchy_wir::wir::Kind::I32,
                                arg: Box::new(header),
                            }
                        } else {
                            header
                        };
                        return Some(W::Seq(vec![
                            N::SetLocal { local: tmp.clone(), value: lhs_w },
                            N::If {
                                cond,
                                then_: vec![N::Push(W::GetLocal(tmp.clone()))],
                                els: vec![N::Push(rhs_w)],
                                result: Some(witchy_wir::wir::WirTy::Bool),
                            },
                        ]));
                    }
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
                // String concatenation (`+` flipped to `Concat`) lowers to
                // `$concat` (only in a WIR-collecting scope; otherwise this falls
                // through and the program is rejected as unsupported).
                if self.collect_wir && *op == BinOp::Concat {
                    self.uses_concat = true;
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
                    self.uses_str_eq = true;
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
                    let call = W::CallIndirect {
                        type_arity: n,
                        args: ci_args,
                        index: Box::new(W::Load {
                            ptr: Box::new(W::GetLocal(name.to_string())),
                            kind: witchy_wir::wir::Kind::I32,
                            offset: 0,
                        }),
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
                if is_plain_user_fn && self.summaries.own_abi(name).is_none() && !has_var {
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
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            format!("{params:?}{body:?}").hash(&mut h);
            h.finish()
        };
        let index = if let Some(&i) = self.lambda_wir_index.get(&key) {
            i
        } else {
            let mut func = self.build_lambda_wir_func(params, body, &cap_info)?;
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

    /// Build the lifted `WirFunc $__lamw{i}` for a lambda: env-pointer param then
    /// one i64 value param per lambda param, a prologue recovering each value
    /// param from its slot and each capture from the env record, the lowered body,
    /// and the tail stored back into the universal i64 result slot. `None` if the
    /// body doesn't lower. Saves the enclosing scope on entry and restores it on
    /// exit so the lifted body lowers in its own local environment.
    fn build_lambda_wir_func(
        &mut self,
        params: &[Param],
        body: &Block,
        cap_info: &[CaptureInfo],
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
                let mut func_params = vec![WirLocal { name: ENV_PARAM.into(), ty: i32t() }];
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
                locals.push(WirLocal { name: "__witchy_owncap".into(), ty: i32t() });
                locals.push(WirLocal { name: TUPLE_TMP.into(), ty: i32t() });
                locals.push(WirLocal { name: TRY_TMP.into(), ty: i32t() });
                locals.push(WirLocal { name: MATCH_TMP.into(), ty: WirTy::Int });
                locals.push(WirLocal { name: SECRET_TMP.into(), ty: i32t() });
                // Scratch slots for the inlined in-place set_at/push fast path (a
                // self-assign accumulator can live inside a lifted lambda body too).
                locals.push(WirLocal { name: "__witchy_set_idx".into(), ty: i32t() });
                locals.push(WirLocal { name: "__witchy_set_val".into(), ty: WirTy::Int });
                for i in 0..WM_POOL {
                    locals.push(WirLocal { name: format!("__witchy_wm_{i}"), ty: i32t() });
                }
                for i in 0..APPLY_POOL {
                    locals.push(WirLocal { name: format!("__witchy_call_{i}"), ty: i32t() });
                }
                // Prologue: recover each value param from its i64 slot, then each
                // capture from the env record (slot j at offset 4 + 8*j).
                let mut nodes: witchy_wir::wir::WirSeq = Vec::new();
                for p in params {
                    let k = self.locals.get(&p.name).copied().unwrap_or(Kind::I32);
                    nodes.push(N::SetLocal {
                        local: p.name.clone(),
                        value: W::FromSlot(Box::new(W::GetLocal(format!("__lp_{}", p.name))), Self::wir_kind(k)),
                    });
                }
                for (j, (name, _, _, kind)) in cap_info.iter().enumerate() {
                    let off = (4 + 8 * j) as i32;
                    let addr = W::Binary {
                        op: witchy_wir::wir::BinOp::Add,
                        kind: witchy_wir::wir::Kind::I32,
                        lhs: Box::new(W::GetLocal(ENV_PARAM.into())),
                        rhs: Box::new(W::ConstI32(off)),
                    };
                    nodes.push(N::SetLocal {
                        local: name.clone(),
                        value: W::FromSlot(Box::new(W::Load { ptr: Box::new(addr), kind: witchy_wir::wir::Kind::I64, offset: 0 }), Self::wir_kind(*kind)),
                    });
                }
                // Body, with the tail value stored into the i64 result slot.
                let mut seq = seq;
                if let Some(N::Push(v)) = seq.pop() {
                    seq.push(N::Push(W::ToSlot(Box::new(v), Self::wir_kind(block_kind))));
                }
                nodes.extend(seq);
                Some(WirFunc {
                    name: format!("__lamw{index}"),
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
                Stmt::Let { value, .. } | Stmt::LetTuple { value, .. } => {
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

    /// WIR twin of [`slot_cmp`] for SCALAR slots only: the comparison of two
    /// 8-byte slots at addresses `aa`/`bb`. `None` for Str/compound shapes (whose
    /// compare would need `$str_eq` or a nested eq call) so the caller bails.
    fn slot_cmp_wir(
        &mut self,
        shape: &EqShape,
        aa: witchy_wir::wir::WirExpr,
        bb: witchy_wir::wir::WirExpr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{BinOp, Kind, WirExpr as W};
        let load = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I64, offset: 0 };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        Some(match shape {
            EqShape::Int | EqShape::Bool => {
                W::Binary { op: BinOp::Eq, kind: Kind::I64, lhs: Box::new(load(aa)), rhs: Box::new(load(bb)) }
            }
            EqShape::Float => W::Binary {
                op: BinOp::Eq,
                kind: Kind::F64,
                lhs: Box::new(W::FromSlot(Box::new(load(aa)), Kind::F64)),
                rhs: Box::new(W::FromSlot(Box::new(load(bb)), Kind::F64)),
            },
            // String content equality: load each slot's i32 pointer and `$str_eq`.
            EqShape::Str => {
                self.uses_str_eq = true;
                W::Call { func: "str_eq".into(), args: vec![load_i32(aa), load_i32(bb)] }
            }
            // A compound field: the slot holds a pointer to the nested value;
            // recurse into that shape's eq helper (None → the parent bails to WAT,
            // e.g. an Adt/Dict field, or a recursive type via the cycle guard).
            compound => {
                let h = self.ensure_eq_wir_helper(compound)?;
                W::Call { func: h, args: vec![load_i32(aa), load_i32(bb)] }
            }
        })
    }

    /// WIR twin of [`ensure_eq_helper`], for shapes whose fields are all scalar
    /// (so the body has no calls and no cycles). Builds the `WirFunc` into
    /// `eq_wir_helpers` and returns its name; `None` (→ the caller bails to WAT)
    /// for any shape or field `slot_cmp_wir` can't handle.
    fn ensure_eq_wir_helper(&mut self, shape: &EqShape) -> Option<String> {
        let name = format!("eq_{}", shape.id());
        if self.eq_wir_helpers.contains_key(&name) {
            return Some(name);
        }
        if !self.eq_building.insert(name.clone()) {
            return None;
        }
        // Reserve the name with a placeholder BEFORE building, so a recursive
        // ADT's self-referential field compares via a `call $eq_…` back to this
        // helper (tying the knot) instead of looping forever in codegen.
        self.eq_wir_helpers.insert(name.clone(), witchy_wir::wir::WirFunc {
            name: name.clone(),
            params: vec![
                witchy_wir::wir::WirLocal { name: "a".into(), ty: witchy_wir::wir::WirTy::Bool },
                witchy_wir::wir::WirLocal { name: "b".into(), ty: witchy_wir::wir::WirTy::Bool },
            ],
            ret: vec![witchy_wir::wir::WirTy::Bool],
            locals: vec![],
            body: vec![witchy_wir::wir::WirNode::Push(witchy_wir::wir::WirExpr::ConstI32(1))],
            raw_body: None,
        });
        let built = self.build_eq_wir_body(shape);
        self.eq_building.remove(&name);
        let Some((body, locals)) = built else {
            self.eq_wir_helpers.remove(&name);
            return None;
        };
        let func = witchy_wir::wir::WirFunc {
            name: name.clone(),
            params: vec![
                witchy_wir::wir::WirLocal { name: "a".into(), ty: witchy_wir::wir::WirTy::Bool },
                witchy_wir::wir::WirLocal { name: "b".into(), ty: witchy_wir::wir::WirTy::Bool },
            ],
            ret: vec![witchy_wir::wir::WirTy::Bool],
            locals,
            body,
            raw_body: None,
        };
        self.eq_wir_helpers.insert(name.clone(), func);
        Some(name)
    }

    /// The i64 a copied-out slot holds, given the SOURCE slot ADDRESS `src`: a scalar
    /// verbatim (`i64.load`), a pointer shape through its (biased) rcopy helper
    /// (`i64.extend_i32_u(rcopy_h(i32.wrap_i64(i64.load src)))`). Mirrors `slot_rcopy`.
    fn slot_rcopy_wir(
        &mut self,
        shape: &EqShape,
        src: witchy_wir::wir::WirExpr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{Kind as WK, WirExpr as W};
        let load = W::Load { ptr: Box::new(src), kind: WK::I64, offset: 0 };
        Some(match shape {
            EqShape::Int | EqShape::Bool | EqShape::Float => load,
            compound => {
                let h = self.ensure_rcopy_wir_helper(compound)?;
                W::Convert {
                    from: WK::I32,
                    to: WK::I64,
                    arg: Box::new(W::Call {
                        func: h,
                        args: vec![W::Convert { from: WK::I64, to: WK::I32, arg: Box::new(load) }],
                    }),
                }
            }
        })
    }

    /// Ensure the WIR rcopy helper for `shape` exists, returning its name. `Str` uses
    /// the registered `$rcopy_str`; scalars never get one. Compound shapes generate a
    /// `WirFunc` into `rcopy_wir_helpers`. A recursive type mid-build (cycle) returns
    /// `None` → the region arm falls back to a plain block (correct value, no reclaim).
    fn ensure_rcopy_wir_helper(&mut self, shape: &EqShape) -> Option<String> {
        if matches!(shape, EqShape::Str) {
            return Some("rcopy_str".to_string());
        }
        let name = format!("rcopy_{}", shape.id());
        if self.rcopy_wir_helpers.contains_key(&name) {
            return Some(name);
        }
        if !self.rcopy_building.insert(name.clone()) {
            return None;
        }
        let built = self.build_rcopy_wir_body(shape);
        self.rcopy_building.remove(&name);
        let (body, locals) = built?;
        let func = witchy_wir::wir::WirFunc {
            name: name.clone(),
            params: vec![witchy_wir::wir::WirLocal { name: "p".into(), ty: witchy_wir::wir::WirTy::Str }],
            ret: vec![witchy_wir::wir::WirTy::Bool],
            locals,
            body,
            raw_body: None,
        };
        self.rcopy_wir_helpers.insert(name.clone(), func);
        Some(name)
    }

    /// Build a per-shape rcopy `WirFunc` body (List/Tuple so far; other compound
    /// shapes return `None` → plain-block fallback). Each: parent short-circuit, then
    /// allocate above the live data, fill (recursing per slot), and return the ptr
    /// pre-biased by `$rcopy_delta`. Mirrors `ensure_rcopy_helper`'s WAT emission.
    fn build_rcopy_wir_body(
        &mut self,
        shape: &EqShape,
    ) -> Option<(witchy_wir::wir::WirSeq, Vec<witchy_wir::wir::WirLocal>)> {
        use witchy_wir::wir::{BinOp, Kind as WK, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |n: &str| W::GetLocal(n.into());
        let getg = |n: &str| W::GetGlobal(n.into());
        let i32c = W::ConstI32;
        let bin = |op: BinOp, l: W, r: W| W::Binary { op, kind: WK::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: WK::I32, offset: 0 };
        let i32l = || WirTy::Bool;
        // `if p < rcopy_wm: return p` (parent-side value, shared not copied).
        let prologue = N::If {
            cond: bin(BinOp::LtU, getl("p"), getg("rcopy_wm")),
            then_: vec![N::Return(Some(getl("p")))],
            els: vec![],
            result: None,
        };
        // Allocate `$size` bytes above the live data; record `$n` and count the bytes.
        let alloc = |size_expr: W| -> Vec<N> {
            vec![
                N::SetLocal { local: "size".into(), value: size_expr },
                N::Do(W::Call { func: "ensure".into(), args: vec![getl("size")] }),
                N::SetLocal { local: "n".into(), value: getg("heap") },
                N::SetGlobal { global: "heap".into(), value: bin(BinOp::Add, getl("n"), getl("size")) },
                N::SetGlobal {
                    global: "__region_copy_bytes".into(),
                    value: W::Binary {
                        op: BinOp::Add,
                        kind: WK::I64,
                        lhs: Box::new(getg("__region_copy_bytes")),
                        rhs: Box::new(W::Convert { from: WK::I32, to: WK::I64, arg: Box::new(getl("size")) }),
                    },
                },
            ]
        };
        let ret_biased = N::Push(bin(BinOp::Sub, getl("n"), getg("rcopy_delta")));
        match shape {
            EqShape::List(elem) => {
                // size = 4 + 8*len; len = list length header.
                let size = bin(BinOp::Add, i32c(4), bin(BinOp::Mul, load_i32(getl("p")), i32c(8)));
                let mut body = vec![prologue, N::SetLocal { local: "len".into(), value: load_i32(getl("p")) }];
                body.extend(alloc(size));
                if matches!(**elem, EqShape::Int | EqShape::Bool | EqShape::Float) {
                    // Scalar payload: one straight copy of `[len][payload]`.
                    body.push(N::MemoryCopy { dest: getl("n"), src: getl("p"), len: getl("size") });
                } else {
                    // Compound payload: copy the length header, then rcopy each slot.
                    let slot_src = bin(
                        BinOp::Add,
                        bin(BinOp::Add, getl("p"), i32c(4)),
                        bin(BinOp::Mul, getl("i"), i32c(8)),
                    );
                    let slot_dst = bin(
                        BinOp::Add,
                        bin(BinOp::Add, getl("n"), i32c(4)),
                        bin(BinOp::Mul, getl("i"), i32c(8)),
                    );
                    let slot_val = self.slot_rcopy_wir(elem, slot_src)?;
                    body.push(N::Store { ptr: getl("n"), value: getl("len"), kind: WK::I32, offset: 0 });
                    body.push(N::SetLocal { local: "i".into(), value: i32c(0) });
                    body.push(N::Block {
                        label: "done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "l".into(),
                            body: vec![
                                N::Br { target: "done".into(), cond: Some(bin(BinOp::Ge, getl("i"), getl("len"))) },
                                N::Store { ptr: slot_dst, value: slot_val, kind: WK::I64, offset: 0 },
                                N::SetLocal { local: "i".into(), value: bin(BinOp::Add, getl("i"), i32c(1)) },
                                N::Br { target: "l".into(), cond: None },
                            ],
                        }],
                    });
                }
                body.push(ret_biased);
                Some((
                    body,
                    vec![
                        WirLocal { name: "n".into(), ty: i32l() },
                        WirLocal { name: "size".into(), ty: i32l() },
                        WirLocal { name: "i".into(), ty: i32l() },
                        WirLocal { name: "len".into(), ty: i32l() },
                    ],
                ))
            }
            EqShape::Tuple(shapes) => {
                let nslots = shapes.len();
                let mut body = vec![prologue];
                body.extend(alloc(i32c((4 + 8 * nslots) as i32)));
                // Copy the tag word (slot 0), then rcopy each field slot.
                body.push(N::Store { ptr: getl("n"), value: load_i32(getl("p")), kind: WK::I32, offset: 0 });
                for (i, fs) in shapes.iter().enumerate() {
                    let off = (4 + 8 * i) as i32;
                    let slot_val = self.slot_rcopy_wir(fs, bin(BinOp::Add, getl("p"), i32c(off)))?;
                    body.push(N::Store { ptr: bin(BinOp::Add, getl("n"), i32c(off)), value: slot_val, kind: WK::I64, offset: 0 });
                }
                body.push(ret_biased);
                Some((
                    body,
                    vec![
                        WirLocal { name: "n".into(), ty: i32l() },
                        WirLocal { name: "size".into(), ty: i32l() },
                    ],
                ))
            }
            _ => None,
        }
    }

    /// Build the `(body, locals)` of a structural-eq helper for `shape`. `None`
    /// for shapes/fields not yet handled (Adt, Dict, or a non-buildable nested
    /// field). Recurses through `slot_cmp_wir` for compound fields.
    fn build_eq_wir_body(&mut self, shape: &EqShape) -> Option<(witchy_wir::wir::WirSeq, Vec<witchy_wir::wir::WirLocal>)> {
        use witchy_wir::wir::{BinOp, Kind, UnOp, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |n: &str| W::GetLocal(n.into());
        let i32c = W::ConstI32;
        let add = |l: W, r: W| W::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let not = |e: W| W::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(e) };
        let check = |cmp: W| N::If { cond: not(cmp), then_: vec![N::Return(Some(i32c(0)))], els: vec![], result: None };
        let bool_local = |n: &str| WirLocal { name: n.into(), ty: WirTy::Bool };

        // Build the per-field checks for a flat record/tuple/variant whose field
        // shapes are `fields`, reading slots at `base+4+8*i`. None if any non-scalar.
        let (body, locals): (witchy_wir::wir::WirSeq, Vec<WirLocal>) = match shape {
            EqShape::Tuple(fields) => {
                let mut b: witchy_wir::wir::WirSeq = Vec::new();
                for (i, f) in fields.iter().enumerate() {
                    let off = i32c((4 + 8 * i) as i32);
                    let cmp = self.slot_cmp_wir(f, add(getl("a"), off.clone()), add(getl("b"), off))?;
                    b.push(check(cmp));
                }
                b.push(N::Push(i32c(1)));
                (b, vec![])
            }
            EqShape::Record(tyname) => {
                let fields = self.record_field_types.get(tyname).cloned()?;
                let mut b: witchy_wir::wir::WirSeq = Vec::new();
                for (i, fty) in fields.iter().enumerate() {
                    let fshape = self.eq_shape_of_type(fty)?;
                    let off = i32c((4 + 8 * i) as i32);
                    let cmp = self.slot_cmp_wir(&fshape, add(getl("a"), off.clone()), add(getl("b"), off))?;
                    b.push(check(cmp));
                }
                b.push(N::Push(i32c(1)));
                (b, vec![])
            }
            EqShape::List(elem) => {
                let idx_off = |base: &str| {
                    add(
                        add(getl(base), i32c(4)),
                        W::Binary { op: BinOp::Mul, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(8)) },
                    )
                };
                let cmp = self.slot_cmp_wir(elem, idx_off("a"), idx_off("b"))?;
                let b: witchy_wir::wir::WirSeq = vec![
                    // lengths differ → not equal
                    N::If {
                        cond: W::Binary { op: BinOp::Ne, kind: Kind::I32, lhs: Box::new(load_i32(getl("a"))), rhs: Box::new(load_i32(getl("b"))) },
                        then_: vec![N::Return(Some(i32c(0)))],
                        els: vec![],
                        result: None,
                    },
                    N::SetLocal { local: "n".into(), value: load_i32(getl("a")) },
                    N::SetLocal { local: "i".into(), value: i32c(0) },
                    N::Block {
                        label: "done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "l".into(),
                            body: vec![
                                N::Br { target: "done".into(), cond: Some(W::Binary { op: BinOp::Ge, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(getl("n")) }) },
                                check(cmp),
                                N::SetLocal { local: "i".into(), value: add(getl("i"), i32c(1)) },
                                N::Br { target: "l".into(), cond: None },
                            ],
                        }],
                    },
                    N::Push(i32c(1)),
                ];
                (b, vec![bool_local("n"), bool_local("i")])
            }
            // Enum (Adt) structural `==`: tag-dispatch then per-variant field
            // compares, mirroring the WAT `ensure_eq_helper` arms (which are also
            // structural, so binary == WAT == interpreter all agree). A generic
            // payload that the comparison site couldn't resolve to a concrete shape
            // (a bare type variable) makes `eq_shape_of_type` return None below → the
            // `?` bails to WAT. Recursive ADTs bail via the `ensure_eq_wir_helper`
            // cycle guard.
            EqShape::Adt(tyname) => {
                let variants = self.adt_variants.get(tyname).cloned()?;
                let mut all: Vec<Vec<EqShape>> = Vec::new();
                for fs in &variants {
                    let mut shapes = Vec::new();
                    for f in fs {
                        shapes.push(self.eq_shape_of_type(f)?);
                    }
                    all.push(shapes);
                }
                return self.build_variant_eq_wir(&all);
            }
            EqShape::AdtInst(_, variant_shapes) => {
                let all = variant_shapes.clone();
                return self.build_variant_eq_wir(&all);
            }
            EqShape::AdtRec(tyname, args) => {
                let variants = self.adt_variants.get(tyname).cloned()?;
                let mut params: Vec<String> = Vec::new();
                for fields in &variants {
                    for f in fields {
                        collect_type_vars(f, &mut params);
                    }
                }
                let subst: std::collections::HashMap<String, EqShape> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();
                let mut all: Vec<Vec<EqShape>> = Vec::new();
                for fs in &variants {
                    let mut shapes = Vec::new();
                    for f in fs {
                        shapes.push(self.eq_shape_of_type_with(f, &subst)?);
                    }
                    all.push(shapes);
                }
                return self.build_variant_eq_wir(&all);
            }
            // Dict `==`: insertion-order pairwise compare over the
            // `[count][key slot, value slot]…` entries (16-byte stride), exactly
            // the interpreter's `Vec<(K, V)>` equality and the WAT `$eq_dict_*`.
            EqShape::Dict(k, v) => {
                let entry = |base: &str, off: i32| {
                    add(
                        add(getl(base), i32c(off)),
                        W::Binary { op: BinOp::Mul, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(16)) },
                    )
                };
                let kcmp = self.slot_cmp_wir(k, entry("a", 4), entry("b", 4))?;
                let vcmp = self.slot_cmp_wir(v, entry("a", 12), entry("b", 12))?;
                let b: witchy_wir::wir::WirSeq = vec![
                    // counts differ → not equal
                    N::If {
                        cond: W::Binary { op: BinOp::Ne, kind: Kind::I32, lhs: Box::new(load_i32(getl("a"))), rhs: Box::new(load_i32(getl("b"))) },
                        then_: vec![N::Return(Some(i32c(0)))],
                        els: vec![],
                        result: None,
                    },
                    N::SetLocal { local: "n".into(), value: load_i32(getl("a")) },
                    N::SetLocal { local: "i".into(), value: i32c(0) },
                    N::Block {
                        label: "done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "l".into(),
                            body: vec![
                                N::Br { target: "done".into(), cond: Some(W::Binary { op: BinOp::Ge, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(getl("n")) }) },
                                check(kcmp),
                                check(vcmp),
                                N::SetLocal { local: "i".into(), value: add(getl("i"), i32c(1)) },
                                N::Br { target: "l".into(), cond: None },
                            ],
                        }],
                    },
                    N::Push(i32c(1)),
                ];
                (b, vec![bool_local("n"), bool_local("i")])
            }
            // Scalars never reach here (compared inline by `slot_cmp_wir`, never
            // via a helper).
            EqShape::Int | EqShape::Bool | EqShape::Float | EqShape::Str => return None,
        };
        Some((body, locals))
    }

    /// The tag-dispatch body of an enum `==`: tags differ → return 0, else load the
    /// shared tag and, for the matching variant, compare its fields (slot `4+8*i`)
    /// via `slot_cmp_wir`; a nullary or fully-equal variant → 1. `all` is the
    /// per-variant resolved field shapes. Mirrors the WAT `ensure_eq_helper` Adt arm.
    fn build_variant_eq_wir(
        &mut self,
        all: &[Vec<EqShape>],
    ) -> Option<(witchy_wir::wir::WirSeq, Vec<witchy_wir::wir::WirLocal>)> {
        use witchy_wir::wir::{BinOp, Kind, UnOp, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |n: &str| W::GetLocal(n.into());
        let i32c = W::ConstI32;
        let add = |l: W, r: W| W::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        let not = |e: W| W::Unary { op: UnOp::Not, kind: Kind::I32, arg: Box::new(e) };
        let eqi = |l: W, r: W| W::Binary { op: BinOp::Eq, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let nei = |l: W, r: W| W::Binary { op: BinOp::Ne, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let mut b: witchy_wir::wir::WirSeq = Vec::new();
        b.push(N::If {
            cond: nei(load_i32(getl("a")), load_i32(getl("b"))),
            then_: vec![N::Return(Some(i32c(0)))],
            els: vec![],
            result: None,
        });
        b.push(N::SetLocal { local: "t".into(), value: load_i32(getl("a")) });
        for (tag, fields) in all.iter().enumerate() {
            if fields.is_empty() {
                continue;
            }
            let mut checks: witchy_wir::wir::WirSeq = Vec::new();
            for (i, fshape) in fields.iter().enumerate() {
                let off = i32c((4 + 8 * i) as i32);
                let cmp = self.slot_cmp_wir(fshape, add(getl("a"), off.clone()), add(getl("b"), off))?;
                checks.push(N::If { cond: not(cmp), then_: vec![N::Return(Some(i32c(0)))], els: vec![], result: None });
            }
            checks.push(N::Return(Some(i32c(1))));
            b.push(N::If {
                cond: eqi(getl("t"), i32c(tag as i32)),
                then_: checks,
                els: vec![],
                result: None,
            });
        }
        b.push(N::Push(i32c(1)));
        Some((b, vec![WirLocal { name: "t".into(), ty: WirTy::Bool }]))
    }

    /// WIR twin of [`render_slot`]: the String pointer rendering the 8-byte slot
    /// at `addr`. Int → `$int_to_string`, Bool → an interned "true"/"false"
    /// value-if, Str → the pointer, compound → that shape's `$ts` helper. `None`
    /// for Float (needs the `$float_to_str` host import) or an unbuildable nested.
    fn slot_render_wir(&mut self, shape: &EqShape, addr: witchy_wir::wir::WirExpr) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{Kind, WirExpr as W, WirNode as N, WirTy};
        let load_i64 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I64, offset: 0 };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        Some(match shape {
            EqShape::Int => {
                self.uses_int_to_string = true;
                W::Call { func: "int_to_string".into(), args: vec![load_i64(addr)] }
            }
            EqShape::Bool => {
                let t = self.intern("true");
                let f = self.intern("false");
                W::Control(Box::new(N::If {
                    cond: W::FromSlot(Box::new(load_i64(addr)), Kind::I32),
                    then_: vec![N::Push(W::StrPtr(t))],
                    els: vec![N::Push(W::StrPtr(f))],
                    result: Some(WirTy::Str),
                }))
            }
            EqShape::Str => load_i32(addr),
            EqShape::Float => {
                self.uses_float_to_str = true;
                W::Call { func: "float_to_str".into(), args: vec![W::FromSlot(Box::new(load_i64(addr)), Kind::F64)] }
            }
            compound => {
                let h = self.ensure_ts_wir_helper(compound)?;
                W::Call { func: h, args: vec![load_i32(addr)] }
            }
        })
    }

    /// WIR twin of [`ensure_ts_helper`], for Tuple/List shapes whose fields all
    /// render via `slot_render_wir`. Cycle-guarded like the eq helpers.
    fn ensure_ts_wir_helper(&mut self, shape: &EqShape) -> Option<String> {
        self.uses_concat = true;
        let name = format!("ts_{}", shape.id());
        if self.ts_wir_helpers.contains_key(&name) {
            return Some(name);
        }
        if !self.ts_building.insert(name.clone()) {
            return None;
        }
        // Reserve the name with a placeholder BEFORE building the body, so a
        // self-referential field (a recursive ADT like JsonValue, whose
        // JsonArray(List(JsonValue)) field renders this very shape) ties back
        // through the `contains_key` check above — the recursion becomes a
        // `call $ts_…` to this helper rather than an infinite inline expansion.
        let empty = self.intern("");
        self.ts_wir_helpers.insert(name.clone(), witchy_wir::wir::WirFunc {
            name: name.clone(),
            params: vec![witchy_wir::wir::WirLocal { name: "p".into(), ty: witchy_wir::wir::WirTy::Bool }],
            ret: vec![witchy_wir::wir::WirTy::Str],
            locals: vec![],
            body: vec![witchy_wir::wir::WirNode::Push(witchy_wir::wir::WirExpr::StrPtr(empty))],
            raw_body: None,
        });
        let built = self.build_ts_wir_body(shape);
        self.ts_building.remove(&name);
        let Some((body, locals)) = built else {
            // A nested shape couldn't be built — drop the placeholder so the
            // whole render bails cleanly (and a later re-attempt rebuilds).
            self.ts_wir_helpers.remove(&name);
            return None;
        };
        let func = witchy_wir::wir::WirFunc {
            name: name.clone(),
            params: vec![witchy_wir::wir::WirLocal { name: "p".into(), ty: witchy_wir::wir::WirTy::Bool }],
            ret: vec![witchy_wir::wir::WirTy::Str],
            locals,
            body,
            raw_body: None,
        };
        self.ts_wir_helpers.insert(name.clone(), func);
        Some(name)
    }

    /// Build the `(body, locals)` of a `$ts` renderer: a tuple `(f0, f1)` or a
    /// list `[e0, e1]`, accumulating with `$concat`. `None` for Record/Adt/etc.
    fn build_ts_wir_body(&mut self, shape: &EqShape) -> Option<(witchy_wir::wir::WirSeq, Vec<witchy_wir::wir::WirLocal>)> {
        use witchy_wir::wir::{BinOp, Kind, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |n: &str| W::GetLocal(n.into());
        let i32c = W::ConstI32;
        let add = |l: W, r: W| W::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let concat = |a: W, b: W| W::Call { func: "concat".into(), args: vec![a, b] };
        let setl = |n: &str, v: W| N::SetLocal { local: n.into(), value: v };
        let bool_local = |n: &str| WirLocal { name: n.into(), ty: WirTy::Bool };
        match shape {
            EqShape::Tuple(fields) => {
                let (open, close, comma) = (self.intern("("), self.intern(")"), self.intern(", "));
                let mut body: witchy_wir::wir::WirSeq = vec![setl("acc", W::StrPtr(open))];
                for (i, f) in fields.iter().enumerate() {
                    let render = self.slot_render_wir(f, add(getl("p"), i32c((4 + 8 * i) as i32)))?;
                    if i > 0 {
                        body.push(setl("acc", concat(getl("acc"), W::StrPtr(comma))));
                    }
                    body.push(setl("acc", concat(getl("acc"), render)));
                }
                body.push(N::Push(concat(getl("acc"), W::StrPtr(close))));
                Some((body, vec![bool_local("acc")]))
            }
            EqShape::List(elem) => {
                let (open, close, comma) = (self.intern("["), self.intern("]"), self.intern(", "));
                let render = self.slot_render_wir(
                    elem,
                    add(add(getl("p"), i32c(4)), W::Binary { op: BinOp::Mul, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(8)) }),
                )?;
                let body: witchy_wir::wir::WirSeq = vec![
                    setl("n", W::Load { ptr: Box::new(getl("p")), kind: Kind::I32, offset: 0 }),
                    setl("acc", W::StrPtr(open)),
                    setl("i", i32c(0)),
                    N::Block {
                        label: "done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "l".into(),
                            body: vec![
                                N::Br { target: "done".into(), cond: Some(W::Binary { op: BinOp::Ge, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(getl("n")) }) },
                                N::If {
                                    cond: W::Binary { op: BinOp::Gt, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(0)) },
                                    then_: vec![setl("acc", concat(getl("acc"), W::StrPtr(comma)))],
                                    els: vec![],
                                    result: None,
                                },
                                setl("acc", concat(getl("acc"), render)),
                                setl("i", add(getl("i"), i32c(1))),
                                N::Br { target: "l".into(), cond: None },
                            ],
                        }],
                    },
                    N::Push(concat(getl("acc"), W::StrPtr(close))),
                ];
                Some((body, vec![bool_local("n"), bool_local("i"), bool_local("acc")]))
            }
            // A record renders as `Name(f0, f1, ...)` — like a tuple with the type
            // name as the opening token (matching the single ctor it lowers to).
            EqShape::Record(tyname) => {
                let fields = self.record_field_types.get(tyname).cloned()?;
                let header = self.intern(&format!("{tyname}("));
                let (close, comma) = (self.intern(")"), self.intern(", "));
                let mut body: witchy_wir::wir::WirSeq = vec![setl("acc", W::StrPtr(header))];
                for (i, fty) in fields.iter().enumerate() {
                    let fshape = self.eq_shape_of_type(fty)?;
                    let render = self.slot_render_wir(&fshape, add(getl("p"), i32c((4 + 8 * i) as i32)))?;
                    if i > 0 {
                        body.push(setl("acc", concat(getl("acc"), W::StrPtr(comma))));
                    }
                    body.push(setl("acc", concat(getl("acc"), render)));
                }
                body.push(N::Push(concat(getl("acc"), W::StrPtr(close))));
                Some((body, vec![bool_local("acc")]))
            }
            // Enum (Adt) render: dispatch on the tag to `Name` (nullary) or
            // `Name(f0, f1, ...)`. Matches the interpreter's `Value::Ctor` Display
            // (records are also Ctors, rendered positionally) AND the WAT path.
            EqShape::Adt(tyname) => {
                let variants = self.adt_variants.get(tyname).cloned()?;
                let names = self.adt_variant_names.get(tyname).cloned()?;
                let mut all: Vec<Vec<EqShape>> = Vec::new();
                for fs in &variants {
                    let mut shapes = Vec::new();
                    for f in fs {
                        shapes.push(self.eq_shape_of_type(f)?);
                    }
                    all.push(shapes);
                }
                self.build_variant_ts_wir(&names, &all)
            }
            EqShape::AdtInst(tyname, variant_shapes) => {
                let names = self.adt_variant_names.get(tyname).cloned()?;
                let all = variant_shapes.clone();
                self.build_variant_ts_wir(&names, &all)
            }
            // A dict renders as `{k: v, ...}` over its `[count][key slot, value slot]…`
            // entries (16-byte stride), matching the interpreter's `Value::Dict` order.
            EqShape::Dict(k, v) => {
                let (open, close, comma, colon) =
                    (self.intern("{"), self.intern("}"), self.intern(", "), self.intern(": "));
                let stride = |off: i32| {
                    add(
                        add(getl("p"), i32c(off)),
                        W::Binary { op: BinOp::Mul, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(16)) },
                    )
                };
                let krender = self.slot_render_wir(k, stride(4))?;
                let vrender = self.slot_render_wir(v, stride(12))?;
                let body: witchy_wir::wir::WirSeq = vec![
                    setl("n", W::Load { ptr: Box::new(getl("p")), kind: Kind::I32, offset: 0 }),
                    setl("acc", W::StrPtr(open)),
                    setl("i", i32c(0)),
                    N::Block {
                        label: "done".into(),
                        result: None,
                        body: vec![N::Loop {
                            label: "l".into(),
                            body: vec![
                                N::Br { target: "done".into(), cond: Some(W::Binary { op: BinOp::Ge, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(getl("n")) }) },
                                N::If {
                                    cond: W::Binary { op: BinOp::Gt, kind: Kind::I32, lhs: Box::new(getl("i")), rhs: Box::new(i32c(0)) },
                                    then_: vec![setl("acc", concat(getl("acc"), W::StrPtr(comma)))],
                                    els: vec![],
                                    result: None,
                                },
                                setl("acc", concat(getl("acc"), krender)),
                                setl("acc", concat(getl("acc"), W::StrPtr(colon))),
                                setl("acc", concat(getl("acc"), vrender)),
                                setl("i", add(getl("i"), i32c(1))),
                                N::Br { target: "l".into(), cond: None },
                            ],
                        }],
                    },
                    N::Push(concat(getl("acc"), W::StrPtr(close))),
                ];
                Some((body, vec![bool_local("n"), bool_local("i"), bool_local("acc")]))
            }
            // A generic self-recursive ADT instantiation (`Stack(Int)`): expand
            // one level of each variant's fields under the argument substitution.
            // A self-referential field resolves back to this shape, whose `$ts`
            // helper name is reserved (the placeholder in `ensure_ts_wir_helper`),
            // so it renders via a recursive `call`. Mirrors the eq `AdtRec` arm.
            EqShape::AdtRec(tyname, args) => {
                let variants = self.adt_variants.get(tyname).cloned()?;
                let names = self.adt_variant_names.get(tyname).cloned()?;
                let mut params: Vec<String> = Vec::new();
                for fields in &variants {
                    for f in fields {
                        collect_type_vars(f, &mut params);
                    }
                }
                let subst: std::collections::HashMap<String, EqShape> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();
                let mut all: Vec<Vec<EqShape>> = Vec::new();
                for fs in &variants {
                    let mut shapes = Vec::new();
                    for f in fs {
                        shapes.push(self.eq_shape_of_type_with(f, &subst)?);
                    }
                    all.push(shapes);
                }
                self.build_variant_ts_wir(&names, &all)
            }
            EqShape::Int | EqShape::Bool | EqShape::Float | EqShape::Str => None,
        }
    }

    /// The tag-dispatch body of an enum `__render`: load the tag, and for the
    /// matching variant emit `Name` (nullary) or `Name(f0, f1, ...)` accumulating
    /// each field's `slot_render_wir` with `$concat`. `ctor_names`/`all` are the
    /// per-variant names and resolved field shapes. Mirrors the WAT `ts_adt_body`.
    fn build_variant_ts_wir(
        &mut self,
        ctor_names: &[String],
        all: &[Vec<EqShape>],
    ) -> Option<(witchy_wir::wir::WirSeq, Vec<witchy_wir::wir::WirLocal>)> {
        use witchy_wir::wir::{BinOp, Kind, WirExpr as W, WirLocal, WirNode as N, WirTy};
        let getl = |n: &str| W::GetLocal(n.into());
        let i32c = W::ConstI32;
        let add = |l: W, r: W| W::Binary { op: BinOp::Add, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let concat = |a: W, b: W| W::Call { func: "concat".into(), args: vec![a, b] };
        let setl = |n: &str, v: W| N::SetLocal { local: n.into(), value: v };
        let eqi = |l: W, r: W| W::Binary { op: BinOp::Eq, kind: Kind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let load_i32 = |p: W| W::Load { ptr: Box::new(p), kind: Kind::I32, offset: 0 };
        self.uses_concat = true;
        let (open, close, comma) = (self.intern("("), self.intern(")"), self.intern(", "));
        let mut b: witchy_wir::wir::WirSeq = vec![setl("t", load_i32(getl("p")))];
        for (tag, fields) in all.iter().enumerate() {
            let label = self.intern(ctor_names.get(tag).map(|s| s.as_str()).unwrap_or("?"));
            if fields.is_empty() {
                b.push(N::If {
                    cond: eqi(getl("t"), i32c(tag as i32)),
                    then_: vec![N::Return(Some(W::StrPtr(label)))],
                    els: vec![],
                    result: None,
                });
                continue;
            }
            let mut arm: witchy_wir::wir::WirSeq = vec![
                setl("acc", W::StrPtr(label)),
                setl("acc", concat(getl("acc"), W::StrPtr(open))),
            ];
            for (i, fshape) in fields.iter().enumerate() {
                let render = self.slot_render_wir(fshape, add(getl("p"), i32c((4 + 8 * i) as i32)))?;
                if i > 0 {
                    arm.push(setl("acc", concat(getl("acc"), W::StrPtr(comma))));
                }
                arm.push(setl("acc", concat(getl("acc"), render)));
            }
            arm.push(N::Return(Some(concat(getl("acc"), W::StrPtr(close)))));
            b.push(N::If { cond: eqi(getl("t"), i32c(tag as i32)), then_: arm, els: vec![], result: None });
        }
        // For valid data the tag always matches a variant above; this tail is the
        // unreachable fallback that keeps the function stack-typed.
        let q = self.intern("?");
        b.push(N::Push(W::StrPtr(q)));
        Some((
            b,
            vec![
                WirLocal { name: "t".into(), ty: WirTy::Bool },
                WirLocal { name: "acc".into(), ty: WirTy::Bool },
            ],
        ))
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
    fn lower_call(&mut self, name: &str, args: &[Expr]) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        use witchy_wir::wir::WirNode as N;
        let call = |func: &str, a: Vec<W>| W::Call { func: func.to_string(), args: a };
        // A direct host-import call (a `_host` import is the authority surface).
        let host = |import: &str, a: Vec<W>| W::CallHost { import: import.to_string(), args: a };
        // A void effect that yields Nil: `{inner} ... i32.const 0`.
        let nil0 = |inner: W| W::Seq(vec![N::Do(inner), N::Push(W::ConstI32(0))]);
        Some(match (name, args.len()) {
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
            // i32 op pushed *before* the argument.
            ("encoding.hex_encode", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(0), self.lower_expr(&args[0])?])
            }
            ("encoding.hex_decode", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(1), self.lower_expr(&args[0])?])
            }
            ("encoding.base64_encode", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(2), self.lower_expr(&args[0])?])
            }
            ("encoding.base64_decode", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(3), self.lower_expr(&args[0])?])
            }
            ("encoding.base64url_of_hex", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(4), self.lower_expr(&args[0])?])
            }
            ("encoding.base64url_decode", 1) => {
                self.uses_encoding = true;
                call("encoding", vec![W::ConstI32(5), self.lower_expr(&args[0])?])
            }
            ("encoding.base64url_to_hex", 1) => {
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
                self.uses_list_push = true;
                call("split", self.lower_args(&[&args[0], &args[1]])?)
            }
            ("string.chars", 1) => {
                self.uses_str_chars = true;
                self.uses_byte_to_char = true;
                self.uses_substring = true;
                self.uses_substr = true;
                self.uses_list_push = true;
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
            // `fail(msg)`: a deliberate, loud abort — evaluate (and drop) the
            // message, then `unreachable` traps. The trailing `i32.const 0` is dead
            // code after the trap, present only so the Seq is stack-typed.
            ("fail", 1) => {
                let msg = self.lower_expr(&args[0])?;
                W::Seq(vec![
                    witchy_wir::wir::WirNode::Drop(msg),
                    witchy_wir::wir::WirNode::Unreachable,
                    witchy_wir::wir::WirNode::Push(W::ConstI32(0)),
                ])
            }
            ("get_env", 2) => {
                self.uses_get_env = true;
                call("get_env", self.lower_args(&[&args[1]])?)
            }
            // `print(console, msg)`: the Console arg is type-level; print the msg
            // (a void host helper), then yield Nil as `i32.const 0`.
            ("print", 2) => {
                self.uses_print = true;
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
            ("string.index_of", 2) => {
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
                self.uses_list_concat = true;
                call("list_concat", self.lower_args(&[&args[0], &args[1]])?)
            }
            ("dict.new", 0) => {
                self.uses_dict = true;
                self.uses_str_eq = true;
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
            ("restrict", 2) => {
                self.used_net_ops.insert("restrict");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("net_restrict", a) } else { host("net_restrict_host", a) }
            }
            // RFC-0011 typed verbs: `only`/`deny` take a policy record; extract its
            // single `pattern` field and feed the host op the same string. `only` is
            // polymorphic on the receiver — a `Dir` narrows its ENTRY policy
            // (`dir_only`), a `Net` narrows its ADDRESS set (`net_restrict`).
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
            ("listen", 2) => {
                self.used_net_ops.insert("listen");
                let a = self.lower_args(&[&args[0], &args[1]])?;
                if self.collect_wir { call("net_listen", a) } else { host("net_listen_host", a) }
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
                call("str_substring", vec![
                    self.lower_expr(&args[0])?,
                    Self::wir_convert(self.lower_expr(&args[1])?, sk, Kind::I32),
                    Self::wir_convert(self.lower_expr(&args[2])?, ek, Kind::I32),
                ])
            }
            ("list.push", 2) => {
                self.uses_list_push = true;
                let xk = self.kind_of(&args[1]);
                call("list_push", vec![
                    self.lower_expr(&args[0])?,
                    W::ToSlot(Box::new(self.lower_expr(&args[1])?), Self::wir_kind(xk)),
                ])
            }
            ("list.at", 2) => {
                self.uses_list_at = true;
                let ek = self.list_elem_kind(&args[0]);
                let ik = self.kind_of(&args[1]);
                let inner = vec![
                    self.lower_expr(&args[0])?,
                    Self::wir_convert(self.lower_expr(&args[1])?, ik, Kind::I32),
                ];
                W::FromSlot(Box::new(call("list_at", inner)), Self::wir_kind(ek))
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
                self.uses_str_eq = true;
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
                self.uses_str_eq = true;
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
                self.uses_str_eq = true;
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
                self.uses_str_eq = true;
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
                self.uses_str_eq = true;
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
        Type::Named(n, args) => {
            (args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()))
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
pub(crate) fn is_string_export(f: &Function) -> bool {
    let is_string = |t: &Option<Type>| matches!(t, Some(Type::Named(n, a)) if n == "String" && a.is_empty());
    // After linking a function is named `{module}.{name}` (the entry module's
    // `main` is the one exception). Match the unqualified tail against the prefix.
    let unqualified = f.name.rsplit('.').next().unwrap_or(&f.name);
    f.public
        && unqualified.starts_with(STRING_EXPORT_PREFIX)
        && f.bounds.is_empty()
        && f.params.len() == 1
        && is_string(&f.params[0].ty)
        && is_string(&f.ret)
}

/// The JS export name for a string-export function: `__export_<unqualified>`. The
/// linker's `{module}.` prefix is dropped so a host calls a stable, source-named
/// export (`__export_step`) regardless of the rune's file/module name.
pub(crate) fn string_export_name(linked_name: &str) -> String {
    let unqualified = linked_name.rsplit('.').next().unwrap_or(linked_name);
    format!("__export_{unqualified}")
}

/// The names of every JS-callable string export in declaration order (`__export_*`
/// wrappers are emitted for these and they are extra reachability roots).
fn string_export_functions(module: &Module) -> Vec<String> {
    module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f) if is_string_export(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect()
}

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
    // String exports (`pub fn f(String) -> String`) are additional roots: the host
    // calls them directly through their `__export_*` wrapper, so they must be
    // compiled and kept even when `main` never reaches them.
    for name in string_export_functions(module) {
        if reachable.insert(name.clone()) {
            work.push(name);
        }
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

/// Register every item's compile-time metadata (parameter conventions,
/// return kinds/types, record fields, generic shape hints, ...) on `cg`.
fn register_module_items(cg: &mut Codegen, module: &Module) {
    // `Option`/`Result` are language-level (`?`, `Some`/`Ok` literals, the
    // interpreter evaluates them natively): their constructors exist for
    // patterns whether or not std/option / std/result are linked. Tags match
    // the std declarations (Some=0/None=1, Ok=0/Err=1); if the modules ARE
    // linked, the Item::Type pass below re-registers identical values.
    for (ty, variants) in [
        ("Option", [("Some", 1usize), ("None", 0)]),
        ("Result", [("Ok", 1), ("Err", 1)]),
    ] {
        cg.adt_variant_names
            .insert(ty.to_string(), variants.iter().map(|(n, _)| n.to_string()).collect());
        for (tag, (name, nfields)) in variants.iter().enumerate() {
            cg.ctor_type_name.insert(name.to_string(), ty.to_string());
            cg.ctors.insert(name.to_string(), (tag as u32, *nfields));
        }
    }
    // Collect parameter conventions up front so call sites can resolve `var`
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
            Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
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
                    // `list.at(f(...), i)` is typed (e.g. a String element compares by
                    // content). Skips `Other` (generic / non-scalar elements).
                    if let Some(elem) = args.first() {
                        let evt = ty_to_valtype(elem);
                        if evt != ValType::Other {
                            cg.fn_ret_list_elem_valtype.insert(f.name.clone(), evt);
                        }
                        // `List((T, U))` (e.g. zip): record the element tuple's
                        // slot types so a destructure of `list.at(f(...), i)` is typed.
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
}

/// Compile a module straight to a wasm **binary** via WIR + `wir_encode::encode`.
/// Returns `Ok(Some(bytes))` only when the whole module assembles to WIR (see
/// `assemble_wir_module`); otherwise `Ok(None)`, which the caller treats as a
/// hard "cannot compile" error (there is no WAT fallback). The `wir_opt`
/// slot-elimination pass runs before encoding, and the assembled binary is
/// wasm-validated — an assembly slip returns `Ok(None)` rather than shipping a
/// malformed module.
pub fn compile_module_binary(
    module: &Module,
) -> Result<Option<Vec<u8>>, CodegenError> {
    let Some(mut wir_module) = assemble_wir_module(module)? else {
        return Ok(None);
    };
    witchy_wir::wir_opt::optimize(&mut wir_module);
    // Robustness net: if any reached `Call` names a func that didn't make it into
    // the module — an unregistered guest helper like `$string_from_code`, which
    // `assemble`'s prelude/wir-helper resolution doesn't account for — bail with
    // `Ok(None)` rather than panic in the encoder's func-index lookup.
    {
        let mut defined: std::collections::HashSet<String> = std::collections::HashSet::new();
        for imp in &wir_module.imports {
            defined.insert(imp.name.clone());
        }
        for f in &wir_module.funcs {
            defined.insert(f.name.clone());
        }
        let mut called: std::collections::HashSet<String> = std::collections::HashSet::new();
        for f in &wir_module.funcs {
            collect_called_funcs(&f.body, &mut called);
        }
        if !called.iter().all(|c| defined.contains(c)) {
            if std::env::var_os("WIRDIAG").is_some() {
                let missing: Vec<&String> = called.iter().filter(|c| !defined.contains(*c)).collect();
                eprintln!("WIRBAIL called-undefined-func: {missing:?}");
            }
            return Ok(None);
        }
    }
    let bytes = witchy_wir::wir_encode::encode(&wir_module);
    // Validate before committing; a malformed assembly returns `Ok(None)`.
    if let Err(e) = wasmparser::validate(&bytes) {
        if std::env::var_os("WIRDIAG").is_some() {
            eprintln!("WIRBAIL validate-failed: {e}");
        }
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// Assemble the complete pre-optimization `WirModule` for a program — the static
/// prelude raw-body helpers + the lowered user functions + the `run` export +
/// imports/globals/data/table — or `Ok(None)` when any reachable function does
/// not fully lower to WIR or the program needs something outside the static
/// prelude. Split out from `compile_module_binary` so tests can compare the
/// optimized vs. unoptimized encoding (the slot-elimination differential).
pub fn assemble_wir_module(
    module: &Module,
) -> Result<Option<witchy_wir::wir::WirModule>, CodegenError> {
    use witchy_wir::wir::{
        DataSegment, GlobalInit, Kind as WK, WirExpr, WirFunc, WirGlobal, WirImport, WirModule,
        WirNode, WirTable,
    };
    use witchy_wir::wir_prelude::WasmTy;
    // Front-end, identical to `compile_module_with`.
    let recs = witchy_syntax::records::lower(module.clone()).map_err(|message| CodegenError { message })?;
    let mut lowered = witchy_types::traits::lower_for_wasm(recs);
    witchy_syntax::parser::lower_sugar_module(&mut lowered);
    alpha_rename_module(&mut lowered);
    let mut cg = Codegen::new();
    cg.collect_wir = true;
    cg.type_table = witchy_types::typeck::annotate(&lowered);
    // `e ? "msg"` desugar (`__try_ctx`) is type-directed: an `Option` operand lowers
    // via `option.ok_or`, a `Result` via `result.map_err`. Rewrite it here — after
    // annotation (so the operand's type is known) and before the string-`+` flip +
    // lowering (so the synthesized `map_err` lambda's `+` flips to `Concat` and its
    // nodes get typed). Re-annotate so the freshly minted calls/lambda are in the
    // type table.
    if rewrite_try_ctx_module(&mut lowered, &cg.type_table) {
        cg.type_table = witchy_types::typeck::annotate(&lowered);
    }
    flip_string_add_module(&mut lowered, &cg.type_table);
    let module = &lowered;
    register_module_items(&mut cg, module);
    cg.summaries = analysis::Summaries::of_module(module);

    let reachable = reachable_functions(module);
    // The exact `$name` functions this module emits — the discriminator
    // `lower_expr`'s call arm uses to tell a user call from an intrinsic/native.
    cg.emitted_funcs = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f)
                if reachable.contains(&f.name) && !witchy_types::typeck::intrinsic(&f.name) =>
            {
                Some(f.name.clone())
            }
            _ => None,
        })
        .collect();
    let mut has_main = false;
    let mut main_params = 0usize;
    let mut main_param_is_args: Vec<bool> = Vec::new();
    let mut main_param_is_dir: Vec<bool> = Vec::new();
    let mut main_param_is_file: Vec<bool> = Vec::new();
    let mut main_returns_int = false;
    let mut main_returns_float = false;
    let mut user_order: Vec<String> = Vec::new();
    // The JS-callable string exports (`pub fn f(String) -> String`); each gets an
    // `__export_f` wrapper and is an extra reachability root (above).
    let string_exports = string_export_functions(module);
    for item in &module.items {
        if let Item::Function(f) = item {
            if f.name == "main" {
                has_main = true;
                main_params = f.params.len();
                main_returns_int = matches!(&f.ret, Some(Type::Named(n, _)) if n == "Int");
                main_returns_float = matches!(&f.ret, Some(Type::Named(n, _)) if n == "Float");
                for p in &f.params {
                    let is_args = matches!(&p.ty, Some(t) if witchy_types::typeck::is_args_type(t));
                    if is_args {
                        cg.uses_args = true;
                    }
                    main_param_is_args.push(is_args);
                    main_param_is_dir
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "Dir"));
                    main_param_is_file
                        .push(matches!(&p.ty, Some(Type::Named(n, _)) if n == "File"));
                }
            }
            if reachable.contains(&f.name) && !witchy_types::typeck::intrinsic(&f.name) {
                // Compiled for its side effects: stashes a `WirFunc` in
                // `cg.wir_funcs` iff the whole body lowered, and sets the
                // `uses_*` import-gating flags.
                cg.compile_function(f)?;
                user_order.push(f.name.clone());
            }
        }
    }
    // A module needs an entry: either a `main` (the `run` export) or at least one
    // string export (a `__export_*` host entry). A library with neither has nothing
    // to instantiate against.
    if !has_main && string_exports.is_empty() {
        if std::env::var_os("WIRDIAG").is_some() { eprintln!("WIRBAIL no-main"); }
        return Ok(None);
    }
    if main_returns_int {
        cg.uses_print_int = true;
    }
    if main_returns_float {
        cg.uses_print_float = true;
    }

    // Every reachable function must have fully lowered to WIR.
    if !user_order.iter().all(|n| cg.wir_funcs.contains_key(n)) {
        // Migration aid: `WIRDIAG=1` names the function(s) that didn't lower, so the
        // remaining WAT-fallback surface can be bisected. Inert otherwise.
        if std::env::var_os("WIRDIAG").is_some() {
            let missing: Vec<&String> =
                user_order.iter().filter(|n| !cg.wir_funcs.contains_key(*n)).collect();
            eprintln!("WIRBAIL user-fn-incomplete: {missing:?}");
        }
        return Ok(None);
    }
    // Bail if the program needs program-specific helpers (not in the prelude) or
    // closure types beyond the reserved band. An Int/Float `main` is fine now —
    // the prelude declares `print_int`/`print_float` and the `run` wrapper prints
    // the result.
    // Structural `==` / `__render` are fine when every legacy eq/ts helper has a
    // WIR twin; a shape the WIR generator couldn't build leaves its key without a
    // twin → bail to WAT.
    let eq_all_wir = cg.eq_helpers.keys().all(|k| cg.eq_wir_helpers.contains_key(k));
    let ts_all_wir = cg.ts_helpers.keys().all(|k| cg.ts_wir_helpers.contains_key(k));
    // Lambdas/closures are fine now: each lifted body is in `lambda_wir_funcs` and
    // the closure types are synthesized by the encoder from the `CallIndirect`
    // nodes. A lambda the WIR couldn't lower already bailed its enclosing function
    // at the lower stage (so the user_order check below catches it).
    if !eq_all_wir || !ts_all_wir || !cg.rcopy_helpers.is_empty() {
        if std::env::var_os("WIRDIAG").is_some() {
            eprintln!("WIRBAIL eq_ts_rcopy: eq={eq_all_wir} ts={ts_all_wir} rcopy={}", cg.rcopy_helpers.len());
        }
        return Ok(None);
    }
    let prelude = witchy_wir::wir_prelude::prelude();

    let wasmty_kind = |t: WasmTy| -> WK {
        match t {
            WasmTy::I32 => WK::I32,
            WasmTy::I64 => WK::I64,
            WasmTy::F64 | WasmTy::F32 => WK::F64,
        }
    };

    // --- Capability-minimal WIR-helper path (#35) -------------------------------
    // If every prelude helper the program reaches has a WIR-native form (the
    // `wir_helper` registry), build a PRUNED module that declares only those
    // helpers and imports only their authority — instead of splicing the full
    // "all features on" raw-body prelude (which would over-import and break the
    // capability model). Falls through to the raw-body path otherwise.
    {
        let helper_names: std::collections::HashSet<&str> =
            prelude.funcs.iter().map(|f| f.name.as_str()).collect();
        let mut called = std::collections::HashSet::new();
        let mut user_host_imports = std::collections::HashSet::new();
        for name in &user_order {
            if let Some(wf) = cg.wir_funcs.get(name) {
                collect_called_funcs(&wf.body, &mut called);
                collect_called_host_imports(&wf.body, &mut user_host_imports);
            }
        }
        // The generated structural-eq / render helpers (included below) call
        // prelude helpers themselves — a Str field eq via `$str_eq`, a renderer via
        // `$concat`/`$int_to_string`. Pull those (and nested eq_*/ts_* calls) into
        // the reached set so the resolution loop declares them.
        for f in cg.eq_wir_helpers.values() {
            collect_called_funcs(&f.body, &mut called);
        }
        for f in cg.ts_wir_helpers.values() {
            collect_called_funcs(&f.body, &mut called);
        }
        // Generated rcopy helpers call `$ensure`, `$rcopy_str`, and each other.
        // Only when a region actually reclaimed (so the `$rcopy_*` globals are
        // declared); a helper generated for a region that then fell back to a plain
        // block is an orphan and must not enter the module.
        if cg.uses_region {
            for f in cg.rcopy_wir_helpers.values() {
                collect_called_funcs(&f.body, &mut called);
            }
        }
        // Lifted lambda bodies call `$mkN`/`$ensure`/prelude helpers and each
        // other; pull their reached helpers into the resolution set.
        for f in &cg.lambda_wir_funcs {
            collect_called_funcs(&f.body, &mut called);
        }
        // A direct host call in user code (e.g. `now`, `dir.subdir`, `recv_*`)
        // needs authority the capability-minimal helper registry can't account
        // for — give up on such programs (`Ok(None)`). (Host access that goes
        // THROUGH a migrated helper is fine; its imports come from import_deps.)
        let no_direct_host =
            !called.iter().any(|n| n.starts_with("host:")) && user_host_imports.is_empty();
        if cg.uses_args {
            called.insert("build_args".to_string());
        }
        // The `__galloc` allocator the string-export wrappers expose calls `$ensure`
        // and bumps `$heap`, so pull `ensure` into the reached set (it brings the
        // `$heap` global via `uses_heap` below). Harmless if a string-export body
        // already reaches it.
        if !string_exports.is_empty() {
            called.insert("ensure".to_string());
        }
        // Resolve every reached helper through the registry (transitively).
        let mut resolved: std::collections::BTreeMap<String, witchy_wir::wir_helpers::WirHelperSpec> =
            std::collections::BTreeMap::new();
        let mut all_registered = true;
        // A called name is a prelude helper to pull in if the static prelude
        // declares it OR the WIR registry resolves it — the latter covers helpers
        // migrated to WIR that have no static-prelude body (e.g. crypto_sha512).
        let mut queue: Vec<String> = called
            .iter()
            .filter(|n| helper_names.contains(n.as_str()) || witchy_wir::wir_helpers::wir_helper(n).is_some())
            .cloned()
            .collect();
        while let Some(h) = queue.pop() {
            if resolved.contains_key(&h) {
                continue;
            }
            match witchy_wir::wir_helpers::wir_helper(&h) {
                Some(spec) => {
                    for d in spec.helper_deps {
                        queue.push((*d).to_string());
                    }
                    resolved.insert(h, spec);
                }
                None => {
                    all_registered = false;
                    if std::env::var_os("WIRDIAG").is_some() { eprintln!("WIRBAIL unregistered-helper: {h}"); }
                    break;
                }
            }
        }
        if std::env::var_os("WIRDIAG").is_some() && !(no_direct_host && all_registered) {
            let hosts: Vec<&String> = called.iter().filter(|n| n.starts_with("host:")).collect();
            eprintln!("WIRBAIL prune-fail: no_direct_host={no_direct_host} all_registered={all_registered} user_host={user_host_imports:?} hosts={hosts:?}");
        }
        if no_direct_host && all_registered {
            let mut import_names: std::collections::BTreeSet<&str> =
                std::collections::BTreeSet::new();
            let mut uses_heap = false;
            let mut uses_table = false;
            for spec in resolved.values() {
                for i in spec.import_deps {
                    import_names.insert(i);
                }
                uses_heap |= spec.uses_heap;
                uses_table |= spec.uses_table;
            }
            // A watermarked loop in user code reads/writes `$heap` even when no
            // reached helper allocates, so the global must still be declared.
            uses_heap |= cg.uses_wm;
            // An Int/Float-returning `main` prints its result in the `run`
            // wrapper, so the corresponding host import must be declared.
            if main_returns_int {
                import_names.insert("print_int");
            } else if main_returns_float {
                import_names.insert("print_float");
            }
            let pruned_imports: Vec<WirImport> = import_names
                .iter()
                .map(|iname| {
                    let pi = prelude
                        .imports
                        .iter()
                        .find(|p| p.name.as_str() == *iname)
                        .expect("a helper's import_dep must be a prelude import");
                    WirImport {
                        name: pi.name.clone(),
                        params: pi.params.iter().copied().map(wasmty_kind).collect(),
                        results: pi.results.iter().copied().map(wasmty_kind).collect(),
                    }
                })
                .collect();
            let mut pruned_funcs: Vec<WirFunc> = resolved.into_values().map(|s| s.func).collect();
            // The program-specific structural-equality / render helpers reached by
            // user `==` / `__render`.
            for f in cg.eq_wir_helpers.values() {
                pruned_funcs.push(f.clone());
            }
            for f in cg.ts_wir_helpers.values() {
                pruned_funcs.push(f.clone());
            }
            // Generated per-shape region copy-out helpers reached by a pointer
            // `region:` reclaim. Gated on `uses_region` so a helper generated for a
            // region that then fell back to a plain block stays out of the module
            // (it references `$rcopy_*` globals only declared when `uses_region`).
            if cg.uses_region {
                for f in cg.rcopy_wir_helpers.values() {
                    pruned_funcs.push(f.clone());
                }
            }
            // Lifted lambda bodies, in table-index order (so `$__lamw{i}` lands at
            // table slot i, matching the code index baked into each closure object).
            for f in &cg.lambda_wir_funcs {
                pruned_funcs.push(f.clone());
            }
            for name in &user_order {
                pruned_funcs.push(cg.wir_funcs.get(name).expect("lowered above").clone());
            }
            // Each `Dir` param maps to a distinct host handle in declaration order
            // (0, 1, 2, …) so a `main` taking several `Dir`s gets several grants;
            // each `File` param maps to a file handle in declaration order (the host
            // pre-populates the files table from `--file` grants, RFC-0012); every
            // other cap is a right-less placeholder (handle 0).
            let mut dir_handle = 0i32;
            let mut file_handle = 0i32;
            let mut main_args: Vec<WirExpr> = Vec::with_capacity(main_params);
            for i in 0..main_params {
                if main_param_is_args.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::Call { func: "build_args".into(), args: vec![] });
                } else if main_param_is_dir.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::ConstI32(dir_handle));
                    dir_handle += 1;
                } else if main_param_is_file.get(i).copied().unwrap_or(false) {
                    main_args.push(WirExpr::ConstI32(file_handle));
                    file_handle += 1;
                } else {
                    main_args.push(WirExpr::ConstI32(0));
                }
            }
            // The `run` export calls `main`; an Int/Float result is printed (the
            // exit-code convention), anything else is dropped — matching the WAT
            // sink's `run` tail. Only synthesized when the module has a `main`; a
            // pure string-export library (no `main`) exports only `__galloc` + the
            // `__export_*` wrappers.
            if has_main {
                let main_call = WirExpr::Call { func: "main".into(), args: main_args };
                let run_body = if main_returns_int {
                    vec![WirNode::Do(WirExpr::CallHost { import: "print_int".into(), args: vec![main_call] })]
                } else if main_returns_float {
                    vec![WirNode::Do(WirExpr::CallHost { import: "print_float".into(), args: vec![main_call] })]
                } else {
                    vec![WirNode::Drop(main_call)]
                };
                pruned_funcs.push(WirFunc {
                    name: "run".into(),
                    params: Vec::new(),
                    ret: Vec::new(),
                    locals: Vec::new(),
                    body: run_body,
                    raw_body: None,
                });
            }
            // String-export glue (RFC-0007 §"Data marshaling" / RFC-0008 run loop):
            // a JS host writes a witchy `String` header `[i32 len][bytes]` into guest
            // memory at a `__galloc`-returned pointer, then calls `__export_f(ptr,
            // len)`; the wrapper passes the pointer straight to the witchy fn (whose
            // single `String` param IS that header) and returns the result String
            // pointer. No import, no authority — only guest-memory reads/writes.
            if !string_exports.is_empty() {
                // __galloc(len) -> ptr : ensure(len); p = heap; heap = heap + len; p
                pruned_funcs.push(WirFunc {
                    name: "__galloc".into(),
                    params: vec![witchy_wir::wir::WirLocal {
                        name: "len".into(),
                        ty: witchy_wir::wir::WirTy::Bool, // i32
                    }],
                    ret: vec![witchy_wir::wir::WirTy::Bool], // i32 pointer
                    locals: vec![witchy_wir::wir::WirLocal {
                        name: "p".into(),
                        ty: witchy_wir::wir::WirTy::Bool,
                    }],
                    body: vec![
                        WirNode::Do(WirExpr::Call {
                            func: "ensure".into(),
                            args: vec![WirExpr::GetLocal("len".into())],
                        }),
                        WirNode::SetLocal {
                            local: "p".into(),
                            value: WirExpr::GetGlobal("heap".into()),
                        },
                        WirNode::SetGlobal {
                            global: "heap".into(),
                            value: WirExpr::Binary {
                                op: witchy_wir::wir::BinOp::Add,
                                kind: WK::I32,
                                lhs: Box::new(WirExpr::GetGlobal("heap".into())),
                                rhs: Box::new(WirExpr::GetLocal("len".into())),
                            },
                        },
                        WirNode::Push(WirExpr::GetLocal("p".into())),
                    ],
                    raw_body: None,
                });
                // One `__export_f(in_ptr, in_len) -> out_ptr` per string export. The
                // `in_len` param is accepted for ABI symmetry (and a future bounds
                // check) but the String header is self-describing, so the wrapper
                // forwards `in_ptr` to the witchy fn directly.
                for name in &string_exports {
                    pruned_funcs.push(WirFunc {
                        name: string_export_name(name),
                        params: vec![
                            witchy_wir::wir::WirLocal { name: "in_ptr".into(), ty: witchy_wir::wir::WirTy::Bool },
                            witchy_wir::wir::WirLocal { name: "in_len".into(), ty: witchy_wir::wir::WirTy::Bool },
                        ],
                        ret: vec![witchy_wir::wir::WirTy::Bool], // i32 result String pointer
                        locals: vec![],
                        body: vec![WirNode::Push(WirExpr::Call {
                            func: name.clone(),
                            args: vec![WirExpr::GetLocal("in_ptr".into())],
                        })],
                        raw_body: None,
                    });
                }
            }
            let mut pruned_globals = if uses_heap {
                vec![
                    WirGlobal {
                        name: "heap".into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(cg.next_offset as i32),
                        // Exported so a long-lived host (the glamour MVU run loop, which calls a
                        // `String -> String` export once per event) can RESET the bump allocator to
                        // its base after each call. Every `export_*` call is pure — its input,
                        // working, and output allocations are all dead once the host has read the
                        // result String out — so without a reset the never-freeing bump allocator
                        // leaks one call's allocations forever and eventually exhausts memory
                        // (`__galloc` returns an out-of-bounds pointer). The host reads the global's
                        // initial value as the base and restores it; see witchy-runtime.mjs.
                        export: Some("__heap".into()),
                    },
                    WirGlobal {
                        name: "__witchy_reowns".into(),
                        kind: WK::I64,
                        mutable: true,
                        init: GlobalInit::I64(0),
                        export: Some("__witchy_reowns".into()),
                    },
                ]
            } else {
                Vec::new()
            };
            // Region copy-out scratch globals: the watermark / temp base / slide delta
            // the `$rcopy_*` helpers read, and the exported `$__region_copy_bytes`
            // counter. Declared only when a pointer `region:` reclaim is reached.
            if cg.uses_region {
                for (name, ex) in
                    [("rcopy_wm", false), ("rcopy_base", false), ("rcopy_delta", false)]
                {
                    pruned_globals.push(WirGlobal {
                        name: name.into(),
                        kind: WK::I32,
                        mutable: true,
                        init: GlobalInit::I32(0),
                        export: if ex { Some(name.into()) } else { None },
                    });
                }
                pruned_globals.push(WirGlobal {
                    name: "__region_copy_bytes".into(),
                    kind: WK::I64,
                    mutable: true,
                    init: GlobalInit::I64(0),
                    export: Some("__region_copy_bytes".into()),
                });
            }
            let data: Vec<DataSegment> = cg
                .strings
                .iter()
                .map(|(text, off)| {
                    let mut bytes = (text.len() as u32).to_le_bytes().to_vec();
                    bytes.extend_from_slice(text.as_bytes());
                    DataSegment { offset: *off, bytes }
                })
                .collect();
            return Ok(Some(WirModule {
                imports: pruned_imports,
                funcs: pruned_funcs,
                memory_pages: 1,
                data,
                globals: pruned_globals,
                table: if cg.lambda_wir_funcs.is_empty() {
                    if uses_table { Some(WirTable { funcs: Vec::new() }) } else { None }
                } else {
                    // Slot i = `$__lamw{i}`, so a closure object's code index
                    // resolves to its lifted body through the element segment.
                    Some(WirTable { funcs: cg.lambda_wir_funcs.iter().map(|f| f.name.clone()).collect() })
                },
                exports: {
                    let mut exports: Vec<(String, String)> = Vec::new();
                    if has_main {
                        exports.push(("run".into(), "run".into()));
                    }
                    if !string_exports.is_empty() {
                        exports.push(("__galloc".into(), "__galloc".into()));
                        for name in &string_exports {
                            let ex = string_export_name(name);
                            exports.push((ex.clone(), ex));
                        }
                    }
                    exports
                },
            }));
        }
    }

    // Otherwise the program reaches a prelude helper not yet migrated to a
    // WIR-native form (or directly calls a host import), so no capability-correct
    // binary can be built yet → return `Ok(None)`. The old raw-body
    // "all features on" splice path is RETIRED: it over-imported the full host
    // surface (incl. authority like crypto.sign/dir/net), which a minimal program
    // cannot instantiate under its real grant — the opposite of witchy's
    // capability model. Coverage grows by migrating helpers into `wir_helper`.
    Ok(None)
}

/// Collect every function name a `WirSeq` calls directly (`Call{func}`),
/// recursively. Used by `assemble_wir_module` to find which prelude helpers a
/// program reaches.
fn collect_called_funcs(seq: &witchy_wir::wir::WirSeq, out: &mut std::collections::HashSet<String>) {
    use witchy_wir::wir::{WirExpr as E, WirNode as N};
    fn expr(e: &E, out: &mut std::collections::HashSet<String>) {
        match e {
            E::Call { func, args } => {
                out.insert(func.clone());
                for a in args {
                    expr(a, out);
                }
            }
            E::CallHost { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            E::CallIndirect { args, index, .. } => {
                for a in args {
                    expr(a, out);
                }
                expr(index, out);
            }
            E::ToSlot(i, _)
            | E::FromSlot(i, _)
            | E::Unary { arg: i, .. }
            | E::Convert { arg: i, .. }
            | E::Load { ptr: i, .. }
            | E::Load8U { ptr: i, .. }
            | E::MemoryGrow(i) => expr(i, out),
            E::Binary { lhs, rhs, .. } => {
                expr(lhs, out);
                expr(rhs, out);
            }
            E::Control(n) => node(n, out),
            E::Seq(s) => collect_called_funcs(s, out),
            E::ConstI64(_) | E::ConstF64(_) | E::ConstI32(_) | E::StrPtr(_) | E::MemorySize
            | E::GetLocal(_) | E::GetGlobal(_) => {}
        }
    }
    fn node(n: &N, out: &mut std::collections::HashSet<String>) {
        match n {
            N::SetLocal { value, .. } | N::SetGlobal { value, .. } => expr(value, out),
            N::Store { ptr, value, .. } | N::Store8 { ptr, value, .. } => {
                expr(ptr, out);
                expr(value, out);
            }
            N::CallStoreMulti { func, args, .. } => {
                out.insert(func.clone());
                for a in args {
                    expr(a, out);
                }
            }
            N::MemoryCopy { dest, src, len } => {
                expr(dest, out);
                expr(src, out);
                expr(len, out);
            }
            N::MemoryFill { dest, value, len } => {
                expr(dest, out);
                expr(value, out);
                expr(len, out);
            }
            N::If { cond, then_, els, .. } => {
                expr(cond, out);
                collect_called_funcs(then_, out);
                collect_called_funcs(els, out);
            }
            N::Block { body, .. } | N::Loop { body, .. } => collect_called_funcs(body, out),
            N::Br { cond: Some(c), .. } => expr(c, out),
            N::Drop(e) | N::Do(e) | N::Push(e) | N::Return(Some(e)) => expr(e, out),
            N::Br { cond: None, .. } | N::Return(None) | N::Unreachable => {}
        }
    }
    for n in seq {
        node(n, out);
    }
}

/// Collect every host import a `WirSeq` calls directly (`CallHost{import}`),
/// recursively. Used by `assemble_wir_module` to detect direct host-authority
/// calls in USER code (e.g. `dir.subdir`, `now`, `recv_*`) — which the pruned
/// path can't account for, so such programs return `Ok(None)`. (Helper
/// host calls are accounted for via the registry's `import_deps` instead.)
fn collect_called_host_imports(seq: &witchy_wir::wir::WirSeq, out: &mut std::collections::HashSet<String>) {
    use witchy_wir::wir::{WirExpr as E, WirNode as N};
    fn expr(e: &E, out: &mut std::collections::HashSet<String>) {
        match e {
            E::CallHost { import, args } => {
                out.insert(import.clone());
                for a in args {
                    expr(a, out);
                }
            }
            E::Call { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            E::CallIndirect { args, index, .. } => {
                for a in args {
                    expr(a, out);
                }
                expr(index, out);
            }
            E::ToSlot(i, _)
            | E::FromSlot(i, _)
            | E::Unary { arg: i, .. }
            | E::Convert { arg: i, .. }
            | E::Load { ptr: i, .. }
            | E::Load8U { ptr: i, .. }
            | E::MemoryGrow(i) => expr(i, out),
            E::Binary { lhs, rhs, .. } => {
                expr(lhs, out);
                expr(rhs, out);
            }
            E::Control(n) => node(n, out),
            E::Seq(s) => collect_called_host_imports(s, out),
            E::ConstI64(_) | E::ConstF64(_) | E::ConstI32(_) | E::StrPtr(_) | E::MemorySize
            | E::GetLocal(_) | E::GetGlobal(_) => {}
        }
    }
    fn node(n: &N, out: &mut std::collections::HashSet<String>) {
        match n {
            N::SetLocal { value, .. } | N::SetGlobal { value, .. } => expr(value, out),
            N::Store { ptr, value, .. } | N::Store8 { ptr, value, .. } => {
                expr(ptr, out);
                expr(value, out);
            }
            N::CallStoreMulti { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            N::MemoryCopy { dest, src, len } => {
                expr(dest, out);
                expr(src, out);
                expr(len, out);
            }
            N::MemoryFill { dest, value, len } => {
                expr(dest, out);
                expr(value, out);
                expr(len, out);
            }
            N::If { cond, then_, els, .. } => {
                expr(cond, out);
                collect_called_host_imports(then_, out);
                collect_called_host_imports(els, out);
            }
            N::Block { body, .. } | N::Loop { body, .. } => collect_called_host_imports(body, out),
            N::Br { cond: Some(c), .. } => expr(c, out),
            N::Drop(e) | N::Do(e) | N::Push(e) | N::Return(Some(e)) => expr(e, out),
            N::Br { cond: None, .. } | N::Return(None) | N::Unreachable => {}
        }
    }
    for n in seq {
        node(n, out);
    }
}

/// Compile a rune's build step to a WASM binary that runs in the zero-ambient
/// build sandbox. The `build` entrypoint is renamed to `main` so the whole
/// `compile_module_binary` pipeline (the `run` export, marshaling, helpers) is
/// reused verbatim — its capability parameters lower to handle 0 exactly like
/// `main`'s, and the only build-specific code is the `write_out`/`read_build`
/// host calls (the `build_out_write`/`build_read` WIR helpers), which never
/// appear in an ordinary program (so parity is untouched). The host links only
/// `build_out_write`/`build_read_len`, confined to the granted output sandbox
/// and read roots — nothing else exists for the guest to call.
pub fn compile_build_module(module: &Module) -> Result<Vec<u8>, CodegenError> {
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
    compile_module_binary(&m)?.ok_or_else(|| CodegenError {
        message: "build step uses a construct the binary backend does not support".into(),
    })
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

/// The fields of an aggregate literal (record `Ctor` or tuple), positionally, for
/// scalar replacement — `None` for any other expression.
fn sroa_fields(e: &Expr) -> Option<&[Expr]> {
    match e {
        Expr::Ctor { args, .. } | Expr::Tuple(args) => Some(args),
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

/// Variables bound by a pattern (these become function locals).
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
            Expr::Int(_)
        | Expr::Duration(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::TaggedLit { .. } => {}
            // Call / Ctor names are functions / constructors, not locals —
            // only the arguments are renamed.
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
            // A `Call` name may be a LOCAL closure variable (`cont(x)` where
            // `cont` was bound by a `let`/parameter/match pattern), which
            // lexically shadows any global of the same name — exactly as the
            // type checker resolves it. Rename it like any other use: `resolve`
            // is a no-op for a true global (never bound in a scope), so this
            // only rewrites calls to a renamed local. Without this, a local
            // closure that gets alpha-renamed (e.g. a `cont` reused across
            // sibling match arms) loses its call sites. A `Ctor` name is always
            // a global constructor, never a local.
            Expr::Call { name, args } => {
                *name = self.resolve(name);
                for a in args {
                    self.rename_expr(a);
                }
            }
            Expr::Ctor { args, .. } => {
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
            Expr::Lambda { params, body, .. } => {
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

/// Alpha-rename a function body so shadowing bindings get unique names.
/// `params` are bound in the outermost scope (never renamed themselves).
/// Alpha-rename every function body IN PLACE, once, at module
/// level — BEFORE `typeck::annotate` runs — so the annotated AST instance is
/// the very one codegen compiles (the type table and uniqueness facts are
/// keyed by node identity). `compile_function` compiles bodies as-given.
/// Flip string `+` to the internal `Concat` op, in place — AFTER annotation
/// (the table's node-identity keys survive a field mutation) and BEFORE the
/// ownership analysis (whose accumulator shapes match `Concat`). Detection is
/// the type table plus string literals; anything it misses still compiles
/// correctly through the val-type net in the `Add` arm, just unoptimized.
fn flip_string_add_module(m: &mut Module, table: &witchy_types::typeck::TypeTable) {
    fn stringy(e: &Expr, table: &witchy_types::typeck::TypeTable) -> bool {
        // A `Concat` is always a String — recognize it structurally so a nested
        // chain whose intermediate levels lack a literal operand (and whose other
        // operand the type table didn't resolve, e.g. a build-time `read_build`)
        // still flips the whole chain once the innermost level is anchored.
        matches!(e, Expr::Str(_))
            || matches!(e, Expr::Binary { op: BinOp::Concat, .. })
            || matches!(
                table.type_of(e).and_then(witchy_types::typeck::ty_to_ast),
                Some(Type::Named(n, _)) if n == "String"
            )
    }
    fn walk_expr(e: &mut Expr, table: &witchy_types::typeck::TypeTable) {
        match e {
            Expr::Binary { op, lhs, rhs } => {
                walk_expr(lhs, table);
                walk_expr(rhs, table);
                if *op == BinOp::Add && (stringy(lhs, table) || stringy(rhs, table)) {
                    *op = BinOp::Concat;
                }
            }
            Expr::List(xs) | Expr::Tuple(xs) | Expr::Ctor { args: xs, .. }
            | Expr::Call { args: xs, .. } => {
                for x in xs {
                    walk_expr(x, table);
                }
            }
            Expr::Apply { func, args } => {
                walk_expr(func, table);
                for a in args {
                    walk_expr(a, table);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                walk_expr(receiver, table);
                for a in args {
                    walk_expr(a, table);
                }
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => walk_expr(expr, table),
            Expr::Range { lo, hi, .. } => {
                walk_expr(lo, table);
                walk_expr(hi, table);
            }
            Expr::Index { base, index } => {
                walk_expr(base, table);
                walk_expr(index, table);
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    walk_expr(v, table);
                }
                if let Some(sp) = spread {
                    walk_expr(sp, table);
                }
            }
            Expr::RecordUpdate { base, fields } => {
                walk_expr(base, table);
                for (_, v) in fields {
                    walk_expr(v, table);
                }
            }
            Expr::If { cond, then_block, else_block } => {
                walk_expr(cond, table);
                walk_block(then_block, table);
                if let Some(b) = else_block {
                    walk_block(b, table);
                }
            }
            Expr::Match { scrutinee, arms } => {
                walk_expr(scrutinee, table);
                for a in arms {
                    if let Some(g) = &mut a.guard {
                        walk_expr(g, table);
                    }
                    walk_expr(&mut a.body, table);
                }
            }
            Expr::While { cond, body } => {
                walk_expr(cond, table);
                walk_block(body, table);
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                walk_expr(scrutinee, table);
                walk_block(body, table);
            }
            Expr::For { iter, body, .. } => {
                walk_expr(iter, table);
                walk_block(body, table);
            }
            Expr::Lambda { body, .. } => walk_block(body, table),
            Expr::Block(b) => walk_block(b, table),
            Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_)
            | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
        }
    }
    fn walk_block(b: &mut Block, table: &witchy_types::typeck::TypeTable) {
        for st in &mut b.stmts {
            match st {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetTuple { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value)
                | Stmt::Yield(value) => walk_expr(value, table),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }
    for item in &mut m.items {
        match item {
            Item::Function(f) => walk_block(&mut f.body, table),
            Item::Impl(im) => {
                for f in &mut im.methods {
                    walk_block(&mut f.body, table);
                }
            }
            Item::Trait(t) => {
                for msig in &mut t.methods {
                    if let Some(b) = &mut msig.default {
                        walk_block(b, table);
                    }
                }
            }
            Item::Const { value, .. } => walk_expr(value, table),
            Item::Type(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
}

fn alpha_rename_module(m: &mut Module) {
    for item in &mut m.items {
        if let Item::Function(f) = item {
            f.body = alpha_rename(&f.body, &f.params);
        }
    }
}

/// The `e ? "msg"` desugar (`__try_ctx(operand, msg)`) rewritten to a concrete
/// std call by the operand's type: `Option` -> `option.ok_or(operand, msg)`,
/// `Result` -> `result.map_err(operand, fn(__ctx_err): msg + ": " + __ctx_err)`.
/// The `+` stays `Add`; the later `flip_string_add_module` turns it into `Concat`.
/// Returns true if any node was rewritten (so the caller re-annotates, since
/// moved/new nodes change the address-keyed `TypeTable`).
fn rewrite_try_ctx_module(m: &mut Module, table: &witchy_types::typeck::TypeTable) -> bool {
    fn replacement(is_option: bool, operand: Expr, msg: Expr) -> Expr {
        if is_option {
            return Expr::Call { name: "option.ok_or".into(), args: vec![operand, msg] };
        }
        Expr::Call {
            name: "result.map_err".into(),
            args: vec![
                operand,
                Expr::Lambda {
                    params: vec![Param {
                        name: "__ctx_err".into(),
                        ty: None,
                        convention: Convention::default(),
                    }],
                    body: Block {
                        stmts: vec![Stmt::Expr(Expr::Binary {
                            op: BinOp::Add,
                            lhs: Box::new(Expr::Binary {
                                op: BinOp::Add,
                                lhs: Box::new(msg),
                                rhs: Box::new(Expr::Str(": ".into())),
                            }),
                            rhs: Box::new(Expr::Var("__ctx_err".into())),
                        })],
                        lines: vec![0],
                        region: None,
                    },
                    ret: None,
                },
            ],
        }
    }
    fn walk_expr(e: &mut Expr, table: &witchy_types::typeck::TypeTable, changed: &mut bool) {
        match e {
            Expr::List(xs) | Expr::Tuple(xs) | Expr::Ctor { args: xs, .. }
            | Expr::Call { args: xs, .. } => {
                for x in xs {
                    walk_expr(x, table, changed);
                }
            }
            Expr::Apply { func, args } => {
                walk_expr(func, table, changed);
                for a in args {
                    walk_expr(a, table, changed);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                walk_expr(receiver, table, changed);
                for a in args {
                    walk_expr(a, table, changed);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                walk_expr(lhs, table, changed);
                walk_expr(rhs, table, changed);
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => walk_expr(expr, table, changed),
            Expr::Range { lo, hi, .. } => {
                walk_expr(lo, table, changed);
                walk_expr(hi, table, changed);
            }
            Expr::Index { base, index } => {
                walk_expr(base, table, changed);
                walk_expr(index, table, changed);
            }
            Expr::Record { fields, spread, .. } => {
                for (_, v) in fields {
                    walk_expr(v, table, changed);
                }
                if let Some(sp) = spread {
                    walk_expr(sp, table, changed);
                }
            }
            Expr::RecordUpdate { base, fields } => {
                walk_expr(base, table, changed);
                for (_, v) in fields {
                    walk_expr(v, table, changed);
                }
            }
            Expr::If { cond, then_block, else_block } => {
                walk_expr(cond, table, changed);
                walk_block(then_block, table, changed);
                if let Some(b) = else_block {
                    walk_block(b, table, changed);
                }
            }
            Expr::Match { scrutinee, arms } => {
                walk_expr(scrutinee, table, changed);
                for a in arms {
                    if let Some(g) = &mut a.guard {
                        walk_expr(g, table, changed);
                    }
                    walk_expr(&mut a.body, table, changed);
                }
            }
            Expr::While { cond, body } => {
                walk_expr(cond, table, changed);
                walk_block(body, table, changed);
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                walk_expr(scrutinee, table, changed);
                walk_block(body, table, changed);
            }
            Expr::For { iter, body, .. } => {
                walk_expr(iter, table, changed);
                walk_block(body, table, changed);
            }
            Expr::Lambda { body, .. } => walk_block(body, table, changed),
            Expr::Block(b) => walk_block(b, table, changed),
            Expr::Int(_) | Expr::Duration(_) | Expr::Float(_) | Expr::Str(_)
            | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
        }
        // After recursing into children, rewrite this node if it is `__try_ctx`.
        // Read the operand type BEFORE moving (the table is keyed by node address).
        let is_try = matches!(e, Expr::Call { name, args } if name == "__try_ctx" && args.len() == 2);
        if is_try {
            let is_option = if let Expr::Call { args, .. } = &*e {
                matches!(
                    table.type_of(&args[0]).and_then(witchy_types::typeck::ty_to_ast),
                    Some(Type::Named(n, _)) if n == "Option"
                )
            } else {
                false
            };
            if let Expr::Call { args, .. } = std::mem::replace(e, Expr::Bool(false)) {
                let mut it = args.into_iter();
                let operand = it.next().unwrap();
                let msg = it.next().unwrap();
                *e = replacement(is_option, operand, msg);
                *changed = true;
            }
        }
    }
    fn walk_block(b: &mut Block, table: &witchy_types::typeck::TypeTable, changed: &mut bool) {
        for st in &mut b.stmts {
            match st {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetTuple { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value)
                | Stmt::Yield(value) => walk_expr(value, table, changed),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }
    let mut changed = false;
    for item in &mut m.items {
        match item {
            Item::Function(f) => walk_block(&mut f.body, table, &mut changed),
            Item::Impl(im) => {
                for f in &mut im.methods {
                    walk_block(&mut f.body, table, &mut changed);
                }
            }
            Item::Trait(t) => {
                for msig in &mut t.methods {
                    if let Some(b) = &mut msig.default {
                        walk_block(b, table, &mut changed);
                    }
                }
            }
            Item::Const { value, .. } => walk_expr(value, table, &mut changed),
            Item::Type(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    changed
}

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
#[path = "codegen_tests.rs"]
mod tests;
