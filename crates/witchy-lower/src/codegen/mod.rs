//! WebAssembly code generation for witchy.
//!
//! Lowers the type-checked AST to WIR — the structured IR in `witchy_wir::wir` — which
//! `witchy_wir::wir_encode` then encodes to a wasm binary. The entry points are
//! `compile_checked_module_binary` (checked AST → wasm bytes). Raw AST entry
//! points exist only for lowerer and synthetic-module tests.
//!
//! Value model: a universal 8-byte (`i64`) slot. Integers are `i64`; floats are
//! bit-reinterpreted into the slot; pointers and Bools are `i32` widened to it
//! (`to_slot`/`from_slot` convert at typed boundaries). A string is an `i32`
//! pointer to a length-prefixed record in linear memory: `[len: i32][utf8
//! bytes...]`. The backend-wide representation map is documented in
//! `spec/value-model.md`.
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
mod loans;
mod type_vars;
mod host_layout;
mod callable_layout;
mod expr_lower;
mod match_lower;
mod block_lower;
mod header_elision;
mod specialization;
mod glamour_metadata;
pub use glamour_metadata::{
    checked_glamour_island_execution_module, checked_glamour_islands,
    checked_glamour_worker_execution_module,
    checked_glamour_static_evaluation_module,
    checked_glamour_templates, GlamourBrowserPolicyMetadata, GlamourIslandMetadata, GlamourTemplateAttributeMetadata,
    GlamourTemplateMetadata, GlamourTemplateNodeMetadata, GlamourTemplateOriginMetadata,
    GlamourHostPortMetadata, GlamourMappedWorkMetadata, GlamourTemplateSlotMetadata, GlamourWorkerTaskMetadata,
    GlamourWorkMetadata,
};
pub use assembly::{
    assemble_checked_optimized_wir_module, compile_checked_build_module,
    compile_checked_development_module, compile_checked_glamour_island_binary,
    compile_checked_glamour_island_execution_binary, compile_checked_module_binary,
    CompiledDevelopmentModule, GlamourDevelopmentField, GlamourDevelopmentMetadata,
};
#[cfg(any(test, feature = "raw-module-test-api"))]
pub use assembly::{
    assemble_optimized_wir_module, assemble_wir_module, compile_build_module,
    compile_module_binary,
};
use passes::{alpha_rename_module, flip_string_add_module, rewrite_try_ctx_module};
use loans::{collect_loan_event_keys, collect_loan_roots};
use type_vars::*;

use crate::analysis::{self};
use witchy_syntax::lambda_scan::{collect_pattern_vars, scan_lambda};
use witchy_syntax::intrinsics;
use witchy_wir::layout::{
    type_tag_of, CallableLayoutSignature, FieldKind, HeaderLayout,
    LayoutId, LayoutInterner, LayoutKind, LayoutSize, RcHeader, ScalarKind, DATA_BASE,
};
// foldhash (not SipHash): all keys are compiler-internal names/ids, never
// attacker-chosen collections — see the note in witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use witchy_syntax::ast::{
    collect_type_vars, BinOp, Block, Convention, Expr, Function, Item, MatchArm, Module, Param,
    Pattern, Stmt, Type, UnOp,
};
use witchy_types::storage::externref_cap_name;
use specialization::{CallableSpecializationKey, GenericCallableInstances};

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

#[derive(Debug, Clone, PartialEq)]
pub struct UnsupportedLowering {
    pub message: String,
}

impl fmt::Display for UnsupportedLowering {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported lowering: {}", self.message)
    }
}

/// The explicit result of crossing the checked-AST to WIR boundary.
///
/// `Unsupported` is reserved for source constructs that are valid but do not
/// yet have a capability-correct compiled lowering. Broken compiler output and
/// invalid input to this stage are `Rejected`, never disguised as a miss.
/// Callers must supply the linked, lowered, type-checked module promised by the
/// compiler pipeline; classification of an arbitrary unchecked AST is outside
/// this contract.
#[must_use]
#[derive(Debug, Clone, PartialEq)]
pub enum LoweringOutcome<T> {
    Lowered(T),
    Unsupported(UnsupportedLowering),
    Rejected(CodegenError),
}

impl<T> LoweringOutcome<T> {
    pub fn expect_lowered(self, message: &str) -> T {
        match self {
            Self::Lowered(value) => value,
            Self::Unsupported(reason) => panic!("{message}: {reason}"),
            Self::Rejected(error) => panic!("{message}: {error}"),
        }
    }

    pub fn expect_rejected(self, message: &str) -> CodegenError {
        match self {
            Self::Rejected(error) => error,
            Self::Lowered(_) => panic!("{message}: lowering succeeded"),
            Self::Unsupported(reason) => panic!("{message}: {reason}"),
        }
    }

    pub fn expect_unsupported(self, message: &str) -> UnsupportedLowering {
        match self {
            Self::Unsupported(reason) => reason,
            Self::Lowered(_) => panic!("{message}: lowering succeeded"),
            Self::Rejected(error) => panic!("{message}: {error}"),
        }
    }
}

fn cerr<T>(message: impl Into<String>) -> Result<T, CodegenError> {
    Err(CodegenError {
        message: message.into(),
    })
}

/// Scratch local holding a tuple pointer while its elements are unpacked.
const TUPLE_TMP: &str = "__witchy_tuple_tmp";
const CALL_RESULT_I32_TMP: &str = "__witchy_call_result_i32";
const CALL_RESULT_I64_TMP: &str = "__witchy_call_result_i64";
const CALL_RESULT_F64_TMP: &str = "__witchy_call_result_f64";
const CALL_RESULT_EXTERN_TMP: &str = "__witchy_call_result_extern";
const UNIQUE_RESULT_CAP_TMP: &str = "__witchy_unique_result_cap";
const DESTINATION_PARAM: &str = "__witchy_destination";
const DESTINATION_RESULT_TMP: &str = "__witchy_destination_result";

fn assign_scratch(component: &str, level: usize) -> String {
    format!("__witchy_assign_{component}_{level}")
}

fn scalar_sum_tag_local(name: &str) -> String {
    format!("{name}__witchy_sum_tag")
}

fn scalar_sum_payload_local(name: &str, index: usize) -> String {
    format!("{name}__witchy_sum_payload_{index}")
}

fn call_result_gc_tmp(struct_id: u32) -> String {
    format!("__witchy_call_result_gc_{struct_id}")
}

fn call_result_tmp(kind: Kind) -> String {
    match kind {
        Kind::I32 => CALL_RESULT_I32_TMP.to_string(),
        Kind::I64 => CALL_RESULT_I64_TMP.to_string(),
        Kind::F64 => CALL_RESULT_F64_TMP.to_string(),
        Kind::ExternRef => CALL_RESULT_EXTERN_TMP.to_string(),
        Kind::GcRef(id) => call_result_gc_tmp(id),
    }
}

fn var_scratch(prefix: &str, index: usize, kind: Kind) -> String {
    let suffix = match kind {
        Kind::I32 => "i32".to_string(),
        Kind::I64 => "i64".to_string(),
        Kind::F64 => "f64".to_string(),
        Kind::ExternRef => "extern".to_string(),
        Kind::GcRef(id) => format!("gc_{id}"),
    };
    format!("__witchy_var_{prefix}_{suffix}_{index}")
}

#[derive(Clone)]
enum CodegenPlace {
    Root(String),
    Field { base: Box<CodegenPlace>, field: String },
    Index {
        base: Box<CodegenPlace>,
        coordinate: String,
        coordinate_kind: Kind,
        coordinate_type: ValType,
        dict: bool,
    },
}

type ClosureWriteback = (CodegenPlace, Kind, String);

#[derive(Clone, Debug, Default)]
struct ClosureOwnershipEnvelope {
    own_capacity_param: Option<usize>,
    var_capacity_params: Vec<usize>,
    unique_capacity_result: bool,
}

impl ClosureOwnershipEnvelope {
    fn has_state(&self) -> bool {
        self.unique_capacity_result
            || self.own_capacity_param.is_some()
            || !self.var_capacity_params.is_empty()
    }
}

#[derive(Clone, Copy)]
struct LambdaContract<'a> {
    result_ty: Option<&'a Type>,
    access: Option<&'a witchy_types::access::AccessSignature>,
    ownership: &'a ClosureOwnershipEnvelope,
}

type LoweredClosureArgs = (
    Vec<witchy_wir::wir::WirExpr>,
    Vec<ClosureWriteback>,
    Vec<String>,
);

/// One captured variable plus every local refinement lowering may consult in
/// the lifted body. Capture metadata is copied deliberately; it must not arrive
/// by leaking the enclosing function's whole local-type scope through a lambda.
#[derive(Clone)]
struct CaptureInfo {
    name: String,
    kind: Kind,
    record: Option<String>,
    list_elem: Option<String>,
    payload_record: Option<String>,
    val_type: Option<ValType>,
    ty: Option<Type>,
    list_elem_vt: Option<ValType>,
    list_elem_tuple: Option<Vec<ValType>>,
    tuple_slots: Option<Vec<ValType>>,
    shape: Option<EqShape>,
    payload_vt: Option<ValType>,
    dict_value_vt: Option<ValType>,
    dict_key_vt: Option<ValType>,
    list_elem_list_vt: Option<ValType>,
    list_nesting: Option<(usize, NestBottom)>,
    fn_ret_kind: Option<Kind>,
    fn_ownership: Option<ClosureOwnershipEnvelope>,
}

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
    Env(Option<u32>),
    Threaded,
}

#[derive(Clone, Copy)]
enum ReferenceTryShape {
    Nullable {
        payload_kind: Kind,
    },
    Tagged {
        struct_id: u32,
        success_tag: u32,
        payload_field: u32,
        payload_kind: Kind,
        failure_field: Option<u32>,
        failure_kind: Option<Kind>,
    },
}

/// Scratch local holding the Result/Option being unwrapped by `?`.
const TRY_TMP: &str = "__witchy_try_tmp";

/// Scratch local holding a `match` scrutinee while arms test it.
const MATCH_TMP: &str = "__witchy_match_tmp";
/// Scratch local holding an externref `match` scrutinee; externrefs cannot pass
/// through the i64 slot used by MATCH_TMP.
const MATCH_REF_TMP: &str = "__witchy_match_ref_tmp";
/// Scratch local holding a GC-struct `match` scrutinee for cap-carrying records.
fn match_gc_tmp(struct_id: u32) -> String {
    format!("__witchy_match_gc_{struct_id}")
}

/// (RFC-0005 stage 4) Scratch local holding a GC-struct spread base
/// (`T(field: v, ..base)`) so a non-variable base is evaluated exactly once.
/// Separate from `match_gc_tmp` — a spread inside a match arm must not clobber
/// the scrutinee.
fn update_gc_tmp(struct_id: u32) -> String {
    format!("__witchy_update_gc_{struct_id}")
}

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

/// Scratch local holding a `SecretStore.require` externref so it is fetched once
/// and reused for both the present-test and returned `Secret`.
const SECRET_TMP: &str = "__witchy_secret_tmp";

/// Scratch i32 slot holding a `SecretStore.require` name-string pointer, so the
/// name is evaluated ONCE and reused for both the `secretstore_lookup` and the
/// eager not-granted abort message (BUG-394).
const SECRET_NAME_TMP: &str = "__witchy_secret_name_tmp";

/// Scratch i32 slot holding `fail`'s evaluated string pointer. The source site
/// is published only after the message expression returns, so a nested call in
/// that expression cannot overwrite the outer abort's location.
const ABORT_STR_TMP: &str = "__witchy_abort_str_tmp";

/// (RFC-0037 §3) Scratch i32 local holding a record pointer under `WITCHY_TYPE_CHECK`, so the
/// type-tag check and the field load share one evaluation of the base.
const TYPECHECK_TMP: &str = "__witchy_typecheck_tmp";

/// One scratch local per nesting level of expression application (`f(x)(y)`),
/// holding the callee pointer while its arguments are evaluated. A nested
/// application inside an argument uses the next level, so the levels never
/// clobber each other. Application nested deeper than this in argument
/// position is rejected (absurd in practice).
const APPLY_POOL: usize = 8;

/// Scratch envelopes for nested RFC-0081 dynamic calls. The receiver must be
/// evaluated once before its arguments, and those arguments may themselves
/// dispatch dynamically, so each nesting level needs an independent local.
const EXISTENTIAL_CALL_POOL: usize = 8;

fn existential_call_scratch(level: usize) -> String {
    format!("__witchy_dyn_receiver_{level}")
}

/// (RFC-0016) Scratch i64 slots for capacity-resizing in-place reuse: a list `var`
/// reassignment `x = [e0, …, e_{k-1}]` evaluates its elements into these once, then
/// either overwrites `x`'s buffer (when it fits) or reallocates — so the elements
/// are not double-evaluated across the branch. A literal with more than this many
/// elements skips the optimization and allocates normally.
const REUSE_POOL: usize = 8;

/// The uniform GC closure wrapper: the implicit first parameter of every lifted
/// lambda. GC type zero is reserved for this wrapper before all payload types.
const ENV_PARAM: &str = "__witchy_env";
const CLOSURE_WRAPPER_ID: u32 = 0;
/// The erased RFC-0081 envelope follows the closure wrapper in every module.
/// Its payload is a `structref`; each witness gets a separate concrete box.
const EXISTENTIAL_WRAPPER_ID: u32 = 1;
const GC_LIST_SRC_TMP: &str = "__witchy_gc_list_src";
const GC_LIST_RIGHT_TMP: &str = "__witchy_gc_list_right";
const GC_LIST_DST_TMP: &str = "__witchy_gc_list_dst";
const GC_LIST_VALUE_TMP: &str = "__witchy_gc_list_value";
const GC_LIST_LEN_TMP: &str = "__witchy_gc_list_len";
const GC_LIST_LEFT_LEN_TMP: &str = "__witchy_gc_list_left_len";
const GC_LIST_INDEX_TMP: &str = "__witchy_gc_list_index";
const GC_LIST_TARGET_TMP: &str = "__witchy_gc_list_target";
const GC_LIST_RAW_INDEX_TMP: &str = "__witchy_gc_list_raw_index";

fn gc_list_scratch(prefix: &str, level: usize, type_id: u32) -> String {
    format!("{prefix}_{type_id}_{level}")
}

/// The WASM representation of a value:
///   * `I64` — `Int`, and the UNIVERSAL representation for type variables /
///     generic values / heap slots. Pointers and bools are zero-extended into
///     i64 and floats are bit-reinterpreted when they enter this representation
///     (see `to_slot`/`from_slot`).
///   * `F64` — `Float`.
///   * `I32` — concrete linear-memory pointers (strings and scalar-only
///     lists/records), future unmigrated handles, and `Bool`. These are the
///     wasm32 address width.
///   * `ExternRef` — migrated unforgeable capabilities (`Dir`/`File`/`Net`,
///     `Socket`/`Listener`, and `Secret`). These must not cross the universal
///     slot or linear-memory heap.
///   * `GcRef` — a typed GC reference for closure wrappers, capture payloads,
///     fixed reference-bearing aggregates, and function-valued arrays.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Kind {
    I32,
    I64,
    F64,
    ExternRef,
    GcRef(u32),
}

impl Kind {
    fn is_ref(self) -> bool {
        matches!(self, Kind::ExternRef | Kind::GcRef(_))
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
        Type::Named(n, _) if externref_cap_name(n).is_some() => {
            Kind::ExternRef
        }
        _ => Kind::I32,
    }
}

fn is_builtin_externref_type(n: &str) -> bool {
    externref_cap_name(n).is_some()
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
    Bytes,
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
/// ("42" vs "true"); `Float` reinterprets and uses `f64.eq`; `Str` and `Bytes`
/// call `$str_eq` on their identical `[len][bytes]` representation; and compounds recurse
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
    Bytes,
    List(Box<EqShape>),
    Tuple(Vec<EqShape>),
    Record(String),
    /// A GENERIC record INSTANTIATED at the comparison site (`Box(Int)`),
    /// identified by its type-ARGUMENT shapes — the record analogue of `AdtRec`.
    /// The `Record` variant carries no arguments, so a generic record's field
    /// types (`item: a`) could not be resolved (the record arm dropped the type
    /// args, unlike the ADT arm — BUG-319). The helper resolves each field lazily
    /// under the argument substitution, so `Box(Int) == Box(Int)` and std
    /// `Set(a) == Set(a)` compile like the interpreter runs them.
    RecInst(String, Vec<EqShape>),
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
            ValType::Bytes => Some(EqShape::Bytes),
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
                | EqShape::RecInst(..)
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
            EqShape::Bytes => "bytes".into(),
            EqShape::List(e) => format!("list_{}", e.id()),
            EqShape::Tuple(fs) => {
                format!("tup_{}_", fs.iter().map(|f| f.id()).collect::<Vec<_>>().join("_"))
            }
            EqShape::Record(name) => format!("rec_{name}"),
            EqShape::RecInst(name, args) => {
                let a: Vec<String> = args.iter().map(|s| s.id()).collect();
                format!("reci_{name}_{}_", a.join("_"))
            }
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

fn anon_union_variant_names(name: &str) -> Option<Vec<String>> {
    witchy_types::typeck::anon_union_synthetic_variants(name).map(|variants| {
        variants
            .into_iter()
            .map(|(tag, _)| format!(".{tag}"))
            .collect()
    })
}

fn anon_union_tag_key(tag: &str, arity: usize) -> String {
    format!("{tag}/{arity}")
}

fn anon_union_variant_types(name: &str, args: &[Type]) -> Option<Vec<Vec<Type>>> {
    let variants = witchy_types::typeck::anon_union_synthetic_variants(name)?;
    let mut out = Vec::with_capacity(variants.len());
    let mut offset = 0usize;
    for (_, arity) in variants {
        let end = offset.checked_add(arity)?;
        if end > args.len() {
            return None;
        }
        out.push(args[offset..end].to_vec());
        offset = end;
    }
    (offset == args.len()).then_some(out)
}

/// The common kind two numeric operands/branches promote to: f64 if either is
/// Float, else i64 if either is i64 (a concrete Int), else i32. An externref can
/// only merge with another externref; typeck should prevent mixed scalar/ref arms.
fn promote_kind(a: Kind, b: Kind) -> Kind {
    if a == Kind::ExternRef && b == Kind::ExternRef {
        Kind::ExternRef
    } else if let (Kind::GcRef(x), Kind::GcRef(y)) = (a, b) {
        if x == y { Kind::GcRef(x) } else { Kind::I32 }
    } else if a == Kind::F64 || b == Kind::F64 {
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
/// `Bool`, `Str`, and `Bytes` (pointers) are i32.
fn valtype_kind(vt: ValType) -> Kind {
    match vt {
        ValType::Int => Kind::I64,
        ValType::Float => Kind::F64,
        // Bool, Str/Bytes pointers, and Other (generic/undetermined) use the i32
        // generic ABI.
        ValType::Bool | ValType::Str | ValType::Bytes | ValType::Other => Kind::I32,
    }
}

fn shape_val_type(shape: &EqShape) -> Option<ValType> {
    match shape {
        EqShape::Int => Some(ValType::Int),
        EqShape::Bool => Some(ValType::Bool),
        EqShape::Float => Some(ValType::Float),
        EqShape::Str => Some(ValType::Str),
        EqShape::Bytes => Some(ValType::Bytes),
        _ => None,
    }
}

fn ty_to_valtype(t: &Type) -> ValType {
    match t {
        Type::Named(n, _) if n == "Int" || n == "Duration" => ValType::Int,
        Type::Named(n, _) if n == "Bool" => ValType::Bool,
        Type::Named(n, _) if n == "Float" => ValType::Float,
        Type::Named(n, _) if n == "String" => ValType::Str,
        Type::Named(n, _) if n == "Bytes" => ValType::Bytes,
        _ => ValType::Other,
    }
}

/// RC-region bias for a value stored in a universal i64 collection slot.
/// `None` is an unresolved generic shape and disables ownership-sensitive
/// extraction; -1 is trivial/non-RC, 0 is an ordinary object base, and 4 is a
/// Dict pointer following its hidden index word.
fn rc_leaf_bias(t: &Type) -> Option<i32> {
    match t {
        Type::Qualified(_, inner) => rc_leaf_bias(inner),
        // (RFC-0081) A dyn value's representation is unresolved here; disable
        // ownership-sensitive extraction like an unresolved generic shape.
        Type::Dyn(_, _) => None,
        Type::Tuple(_) => Some(0),
        Type::Fn(_, _, _) => Some(-1),
        Type::Named(name, args)
            if args.is_empty()
                && !name.contains('.')
                && name.chars().next().is_some_and(char::is_lowercase) =>
        {
            None
        }
        Type::Named(name, _) if name == "Dict" => Some(4),
        Type::Named(name, _)
            if matches!(
                name.as_str(),
                "Int" | "Duration" | "Bool" | "Float" | "Nil" | "Console" | "Rand"
            ) || is_builtin_externref_type(name) =>
        {
            Some(-1)
        }
        Type::Named(_, _) => Some(0),
        Type::RecordCompose { .. } => unreachable!(
            "compiler invariant violated: record composition must be normalized before Wasm ownership classification"
        ),
    }
}

fn collection_leaf_bias(t: &Type, collection: &str, index: usize) -> Option<i32> {
    match t {
        Type::Qualified(_, inner) => collection_leaf_bias(inner, collection, index),
        Type::Named(name, args) if name == collection => args.get(index).and_then(rc_leaf_bias),
        _ => None,
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
    field_caps: HashSet<String>,
    field_push_safe: HashSet<(String, String)>,
    records: HashMap<String, String>,
    list_elem: HashMap<String, String>,
    payload: HashMap<String, String>,
    val_types: HashMap<String, ValType>,
    types: HashMap<String, Type>,
    list_elem_vt: HashMap<String, ValType>,
    list_elem_tuple: HashMap<String, Vec<ValType>>,
    tuple_slots: HashMap<String, Vec<ValType>>,
    shape: HashMap<String, EqShape>,
    payload_vt: HashMap<String, ValType>,
    dict_value_vt: HashMap<String, ValType>,
    dict_key_vt: HashMap<String, ValType>,
    list_elem_list_vt: HashMap<String, ValType>,
    list_nesting: HashMap<String, (usize, NestBottom)>,
    fn_ret_kind: HashMap<String, Kind>,
    fn_ownership: HashMap<String, ClosureOwnershipEnvelope>,
    ret: Kind,
    ret_ty: Option<Type>,
    ret_slot: bool,
    unique_ret: bool,
    destination_forward_vars: HashSet<String>,
    destination_scratch_sites: HashMap<usize, (String, LayoutId)>,
    var: bool,
    var_params: Vec<String>,
    var_cap_params: Vec<String>,
    sroa_candidates: HashSet<String>,
    sroa_active: HashMap<String, usize>,
    scalar_sum_candidates: HashSet<String>,
    scalar_sum_active: HashMap<String, ScalarSumLayout>,
    scalar_sum_fused_values: HashMap<String, Expr>,
    scalar_record_call_candidates: HashMap<String, LayoutId>,
    direct_list_builder_lets: HashMap<usize, DirectListBuilderPlan>,
    direct_list_builder_loops: HashMap<usize, DirectListBuilderPlan>,
    active_direct_list_builder: Option<DirectListBuilderPlan>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct LoanRoot {
    local: String,
    value: String,
    bias: i32,
}

#[derive(Clone)]
struct GcCtorLayout {
    owner_key: String,
    tag: Option<u32>,
    field_base: u32,
    field_types: Vec<Type>,
}

#[derive(Clone, Copy)]
struct ScalarSumLayout {
    id: LayoutId,
    max_arity: usize,
}

#[derive(Clone)]
struct ScalarRecordProducer {
    layout: LayoutId,
    field_count: usize,
}

#[derive(Clone)]
struct DirectListBuilderPlan {
    list: String,
    counter: String,
    lower: i64,
    capacity: i32,
    data_offset: i32,
    stride: i32,
    packed_field_offsets: Vec<u32>,
    rc_header: RcHeader,
}

/// A stable, representation-only key for an element of a GC-lowered tuple.
/// Nominal references use names rather than assigned numeric IDs so tuple IDs
/// can be sorted and reserved deterministically before layouts are materialized.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum GcFieldShape {
    I32,
    I64,
    F64,
    ExternRef,
    Function,
    ReferenceList(String),
    Nominal(String),
    Tuple(GcTupleShape),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct GcTupleShape(Vec<GcFieldShape>);

struct Codegen<'types> {
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
    /// Descriptor-derived constructor helpers for specialized values. These
    /// are separate from source functions but enter the same WIR reachability
    /// walk, so allocator/checked-heap dependencies remain explicit.
    layout_wir_funcs: BTreeMap<String, witchy_wir::wir::WirFunc>,
    /// The canonical physical descriptors selected for this closed module.
    /// Every offset, stride, and helper below is read back from this interner.
    specialized_layouts: LayoutInterner,
    /// Closed logical types paired with their canonical physical IDs. This is
    /// identity plumbing only: it deliberately stores no duplicate shape data.
    specialized_type_ids: Vec<(Type, LayoutId)>,
    /// Exact physical signatures for direct source callables. Ownership/access
    /// facts remain independently supplied by RFC-0110.
    callable_layouts: HashMap<String, CallableLayoutSignature>,
    /// Physical instances and per-emitted-caller direct-call targets for
    /// logically monomorphized generic functions.
    generic_callable_instances: GenericCallableInstances,
    /// Emitted function currently being compiled. This distinguishes the same
    /// source call expression reached through two physical caller instances.
    cur_emitted_fn_name: String,
    /// Per-instance physical layouts for logical types in the current generic
    /// body. These override the module-global default layout map.
    current_specialized_type_ids: Vec<(Type, LayoutId)>,
    /// Set by `compile_module_binary` to arm WIR capture for the function being
    /// lowered. Left `false` for any scope that doesn't collect WIR, where
    /// `lower_expr`'s call arm stays inert and pays no capture/clone overhead.
    collect_wir: bool,
    /// Retain source statement wrappers for the authenticated development
    /// instruction map. Production WIR remains source-neutral.
    collect_source_map: bool,
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
    /// Single-field brands over migrated host externrefs that compile as the
    /// underlying externref instead of a heap record.
    transparent_externref_brands: HashSet<String>,
    /// Constructor name -> its single underlying field type for transparent
    /// externref brands. The constructor lowers to its sole argument.
    transparent_externref_ctors: HashMap<String, Type>,
    /// Closed nominal instances lowered as typed GC structs, keyed by canonical
    /// semantic type identity (`Task(Int)` and `Task(String)` never collide).
    gc_aggregate_ids: HashMap<String, u32>,
    /// Bare/qualified nominal spelling -> linked canonical declaration name.
    gc_nominal_names: HashMap<String, String>,
    /// Fully concrete cap-carrying tuple layouts, interned by representation.
    gc_tuple_ids: HashMap<GcTupleShape, u32>,
    /// `(canonical owner type, constructor)` -> optional sum tag and payload band.
    gc_ctor_layouts: HashMap<(String, String), GcCtorLayout>,
    /// The WIR struct type declarations for `gc_aggregate_ids`, indexed by id.
    gc_structs: Vec<witchy_wir::wir::WirStructDef>,
    /// Reference-bearing list storage. Struct IDs precede these array IDs in the
    /// shared concrete GC type-index space.
    gc_arrays: Vec<witchy_wir::wir::WirArrayDef>,
    /// Closed `List(T)` instances whose element is a GC reference. Each exact
    /// element representation gets a typed GC array; direct externref
    /// collections remain rejected.
    gc_reference_list_ids: HashMap<String, u32>,
    /// Closed witness ID -> concrete one-field payload box. The field uses the
    /// payload's actual WIR kind, never the scalar slot ABI.
    existential_payload_ids: HashMap<u32, u32>,
    /// Canonical concrete type key -> its one-field existential payload box.
    /// Dynamic decode selects this from the inferred result type before emitting
    /// a `ref.cast`; runtime descriptor strings never choose a representation.
    existential_payload_type_ids: HashMap<String, u32>,
    /// Closed-plan authenticated `(source witness, target existential, result
    /// witness)` transitions for compiler-owned existential upcasts.
    existential_upcasts: Vec<(u32, Type, u32)>,
    /// Source lambda identity -> pre-reserved typed GC capture payload. All
    /// lambda structs are reserved before arrays so concrete GC type IDs never
    /// shift while function bodies lower.
    lambda_gc_env_ids: HashMap<u64, u32>,
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
    /// Direct functions returning `unique` fixed-layout values accept one hidden
    /// optional destination pointer. The LayoutId is the ABI compatibility key.
    fn_destination_layouts: HashMap<String, LayoutId>,
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
    /// Closed-sum locals proven to be consumed only as direct match
    /// scrutinees. Their tag and payload arguments remain in scalar locals.
    scalar_sum_candidates: HashSet<String>,
    scalar_sum_active: HashMap<String, ScalarSumLayout>,
    /// Adjacent, pure closed-sum constructors whose sole match can consume the
    /// constructor branches directly. The value is retained by AST name so the
    /// match lowers at its original statement (and loan/source-site) boundary.
    scalar_sum_fused_values: HashMap<String, Expr>,
    scalar_record_producers: HashMap<String, ScalarRecordProducer>,
    scalar_record_call_candidates: HashMap<String, LayoutId>,
    /// Exact-capacity, direct-store list builders proven by adjacent empty-list
    /// bindings and literal counted loops. AST identities prevent a later loop
    /// over the same binding from inheriting the builder privilege.
    direct_list_builder_lets: HashMap<usize, DirectListBuilderPlan>,
    direct_list_builder_loops: HashMap<usize, DirectListBuilderPlan>,
    active_direct_list_builder: Option<DirectListBuilderPlan>,
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
    /// Reassigned locals whose old value never escapes as a whole. An exact
    /// LayoutId match lets a `unique` result initialize that dead storage.
    destination_forward_vars: HashSet<String>,
    /// Immediate nonescaping fixed-layout producer arguments reuse one
    /// caller-owned scratch object per AST call site.
    destination_scratch_sites: HashMap<usize, (String, LayoutId)>,
    /// Active counted-range counter-batch slots, innermost last.
    counter_batch_stack: Vec<usize>,
    /// `(destination, rewind)` counters actually touched by each active batch.
    counter_batch_used: Vec<(bool, bool)>,
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
    /// Per-unit RFC-0083 statement identities: expected facts from the checked
    /// AST and the identities actually encountered by lowering.
    loan_fact_stack: Vec<(HashSet<usize>, HashSet<usize>)>,
    /// (RFC-0035) Per-unit `last_use` drop points (parallel to `facts_stack`): values to
    /// `$rc_free` after their last use, consumed in `lower_block`. Empty unless `rc-floor`.
    drop_facts_stack: Vec<analysis::DropFacts>,
    /// Module-wide function summaries for the uniqueness analysis.
    summaries: analysis::Summaries,
    /// RFC-0083's authoritative checked loan events for this exact typed AST.
    loan_facts: witchy_types::loans::LoanFacts,
    /// The exact typed AST which owns every address-keyed semantic fact.
    checked_module: &'types Module,
    /// RFC-0110's canonical direct, indirect, closure, and witness access
    /// contracts for `checked_module`. Physical ABI selection must consume
    /// these facts instead of reconstructing access from surface syntax.
    access_facts: witchy_types::access::CheckedAccessFacts<'types>,
    /// (RFC-0110 step 5) Call-node pointers (`*const Expr as usize`, into
    /// `checked_module`) that are normal-mode one-copy repair sites — an unproven
    /// `unique` argument re-owned at the call boundary. Lever-INDEPENDENT (derived
    /// from the checked access graph, not the `InPlace`/`inplace_push` lever), so
    /// `__witchy_boundary_reown_copies` counts the same under every `WITCHY_OPT`.
    /// Populated in `register_module_items`; empty if the access graph is absent.
    boundary_repair_sites: foldhash::HashSet<usize>,
    /// Exact access rows for the tiny compiler-owned forwarding calls created
    /// after type annotation. Source calls must be present in `access_facts`;
    /// only an address explicitly registered here may use a declaration row.
    synthesized_call_access: HashMap<usize, witchy_types::access::AccessSignature>,
    /// Loans active at the statement currently being lowered. `Expr::Try` uses
    /// this to release roots before its structured early return.
    active_loan_events: Vec<witchy_types::loans::LoanEvent>,
    /// The current function's own-ABI parameter (its ownership token is the
    /// `${name}__cap` PARAM, and the function returns an extra i32 token).
    cur_fn_own_param: Option<String>,
    /// Whether the current function has type-variable parameters (a generic
    /// fallback): unknown-type comparisons there are rejected loudly.
    cur_fn_has_type_vars: bool,
    /// The function being compiled, for error context.
    cur_fn_name: String,
    /// Whether any lowered statement propagates a source site to a host-backed
    /// operation. The assembler uses this to declare the failure-only global.
    uses_diagnostic_sites: bool,
    /// Phase 0 (rfcs/language-evolution.md): typeck's resolved types for the
    /// EXACT module instance being compiled — the authoritative fallback
    /// wherever the local tracking maps come up empty.
    type_table: &'types witchy_types::typeck::TypeTable,
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
    /// (hex/base64 encode/decode over flat `String`/`Bytes` buffers).
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
    /// Whether `main` declares an argv parameter (`args: List(String)`); the
    /// run export then builds the host-provided list via `$build_args`.
    uses_args: bool,
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
    /// Record type name -> its DECLARED type parameters, in order (`Pair(a, b)` ->
    /// `["a", "b"]`). A generic record's `RecInst` maps use-site type arguments to
    /// these parameters positionally; using declared order (not field-occurrence
    /// order) keeps the field substitution correct when fields are declared out of
    /// parameter order (`Rev(a, b): second: b, first: a`) — BUG-319.
    record_generics: HashMap<String, Vec<String>>,
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
    /// Type names with a visible `Eq` impl in the source program after derive
    /// expansion but before trait lowering erases marker impls. Dict keys use
    /// this as a backend acceptance guard so a plain record does not become a
    /// valid key merely because codegen can synthesize structural comparison.
    eq_types: HashSet<String>,
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
    /// Full type of params / let-bound locals, where known. This is deliberately
    /// small and local: it fills gaps where address-keyed type facts cannot
    /// recover a bare `Expr::Var` node's type during backend lowering.
    local_types: HashMap<String, Type>,
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
    /// Function name -> the value type it returns, so generated render of
    /// `f(...)` can be lowered. Populated from return-type annotations.
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
    /// Declared or checker-resolved result type of the current function/lambda.
    /// `?` uses it to rebuild the destination failure representation when the
    /// source and destination success payloads have different GC layouts.
    cur_fn_ret_ty: Option<Type>,
    /// When true (compiling a lambda body), a `return`/tail value is stored into
    /// the universal i64 slot (the closure-result ABI) rather than narrowed to a
    /// fixed kind, so a closure returning a big `Int` keeps its 64 bits.
    cur_fn_ret_slot: bool,
    cur_fn_unique_ret: bool,
    /// Param/local name -> the WASM kind a function-typed value returns, so a
    /// closure call `f(x)` recovers the result at the right width (an `Int`-
    /// returning closure as i64, not the generic i32).
    local_fn_ret_kind: HashMap<String, Kind>,
    /// Ownership-sensitive ABI facts for local function values. The checker
    /// type table can erase a result qualifier while resolving a bare function
    /// name, so these facts are captured when the value is bound and carried
    /// independently of the scalar call signature.
    local_fn_ownership: HashMap<String, ClosureOwnershipEnvelope>,
    /// Whether the current function has any `var` parameters.
    cur_fn_var: bool,
    /// The current function's `var` parameter names, in declaration order. An
    /// early `return`/`?` must push these (after the primary result) so the
    /// multi-result epilogue is reproduced on every exit path.
    cur_fn_var_params: Vec<String>,
    /// Capacity-bearing `var` params whose ownership token is threaded as an
    /// additional ABI input/result. A zero token is the conservative CoW path.
    cur_fn_var_cap_params: Vec<String>,
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
    /// Structural Dict key modes, keyed by the numeric mode passed to `$key_eq`.
    /// Modes 0..=2 are reserved by the scalar prelude helper; modes >=3 dispatch
    /// to per-shape equality helpers in a program-specific `$key_eq` override.
    dict_key_shapes: std::collections::BTreeMap<u32, EqShape>,
    /// Reverse index for `dict_key_shapes`, keyed by `EqShape::id`.
    dict_key_shape_modes: std::collections::BTreeMap<String, u32>,
    /// Names of eq helpers currently being built — a cycle guard so a recursive
    /// type's structural eq bails to WAT instead of looping in codegen.
    eq_building: HashSet<String>,
    /// WIR-native twin of `ts_helpers` (per-shape structural renderers), keyed
    /// identically (`ts_{id}`), for the binary path. Includes
    /// tuples/lists with Int/Bool/String fields (built via `$concat` +
    /// `$int_to_string`); Float/Record fields and enums defer to WAT.
    ts_wir_helpers: std::collections::BTreeMap<String, witchy_wir::wir::WirFunc>,
    /// Cycle guard for `ensure_ts_wir_helper`, mirroring `eq_building`.
    ts_building: HashSet<String>,
    /// WIR-native twin of `rcopy_helpers` (per-shape `region:` copy-out deep-copy),
    /// keyed identically (`rcopy_{id}`), for the binary path.
    rcopy_wir_helpers: std::collections::BTreeMap<String, witchy_wir::wir::WirFunc>,
    /// Cycle guard for `ensure_rcopy_wir_helper`, mirroring `eq_building`.
    rcopy_building: HashSet<String>,
    /// Lifted lambda bodies for the binary path, in table-index order — the WIR
    /// twin of `lambdas`. Each is a `WirFunc $__lamw{i}`; the uniform GC wrapper
    /// stores `i` as its code index and `CallIndirect` uses it as the table slot.
    lambda_wir_funcs: Vec<witchy_wir::wir::WirFunc>,
    /// Leading table cells reserved for RFC-0081 witness adapters. Lambda
    /// bodies retain their local vector indices, while closure objects carry
    /// `existential_table_len + lambda_index` as the shared table address.
    existential_table_len: u32,
    /// Width of the dense `(witness_id, static_slot)` adapter table. This is
    /// captured from the frontend witness plan during module registration.
    existential_dispatch_stride: u32,
    /// Current nesting level of an RFC-0081 dynamic call, indexing
    /// `EXISTENTIAL_CALL_POOL` receiver scratch locals.
    existential_call_level: usize,
    /// Maps a lambda's source-owner/content hash to its index in
    /// `lambda_wir_funcs`, so the many lowering passes register each lambda
    /// exactly once (idempotent).
    lambda_wir_index: HashMap<u64, usize>,
    /// (RFC-0062) Maps an ELIDED closure lambda's owner/content hash to its
    /// THREADED lifted body index (a `$__lamt{i}` in `lambda_wir_funcs`), so an
    /// identical tier-1 lambda registers one threaded body across the many
    /// lowering passes. A global registry like `lambda_wir_index` (NOT
    /// scope-saved).
    lambda_threaded_index: HashMap<u64, usize>,
    /// Generated per-shape `to_string` renderers, keyed by `EqShape::id` (a
    /// `ts_` prefix on the function name). Parallels `eq_helpers`: each compound
    /// shape that flows into `to_string` (or string interpolation) gets one
    /// renderer, emitted once, that builds the interpreter-identical string.
    ts_helpers: std::collections::BTreeMap<String, String>,
    /// Constructor names per sum type, indexed by tag — so a `to_string` ADT
    /// renderer can emit `Some(5)` / `None` (the `eq` path never needs names).
    adt_variant_names: HashMap<String, Vec<String>>,
    /// Anonymous union runtime tag codes, keyed by tag spelling and arity. Unlike
    /// declared ADTs, anonymous unions can widen across different closed sets, so
    /// their runtime tag word must not be the variant's index within one set.
    anon_union_tag_codes: HashMap<String, i32>,
    next_anon_union_tag_code: i32,
    /// Closure arities for which a `(type $clos{n})` signature is needed (all
    /// i32 params, i32 result), used by `call_indirect`.
    clos_arities: HashSet<usize>,
    /// Current nesting level of expression application, indexing `APPLY_POOL`.
    apply_level: usize,
    /// Nesting depth for assignment-place staging. A RHS block may contain
    /// another assignment, so each live captured destination needs its own slots.
    assign_level: usize,
    /// Stack of enclosing loops' `(break-target, continue-target)` WASM labels
    /// (innermost last), so `break`/`continue` branch to the right block.
    loop_labels: Vec<(String, String)>,
}

impl<'types> Codegen<'types> {
    fn new(
        checked_module: &'types Module,
        type_table: &'types witchy_types::typeck::TypeTable,
        loan_facts: witchy_types::loans::LoanFacts,
        access_facts: witchy_types::access::CheckedAccessFacts<'types>,
    ) -> Self {
        Self {
            strings: Vec::new(),
            next_offset: DATA_BASE,
            uses_int_to_string: false,
            captured_seq: None,
            reject_reason: None,
            wir_funcs: HashMap::new(),
            layout_wir_funcs: BTreeMap::new(),
            specialized_layouts: LayoutInterner::new(),
            specialized_type_ids: Vec::new(),
            callable_layouts: HashMap::new(),
            generic_callable_instances: GenericCallableInstances::default(),
            cur_emitted_fn_name: String::new(),
            current_specialized_type_ids: Vec::new(),
            collect_wir: false,
            collect_source_map: false,
            emitted_funcs: HashSet::new(),
            fn_conventions: HashMap::new(),
            fn_params: HashMap::new(),
            ctors: HashMap::new(),
            transparent_externref_brands: HashSet::new(),
            transparent_externref_ctors: HashMap::new(),
            gc_aggregate_ids: HashMap::new(),
            gc_nominal_names: HashMap::new(),
            gc_tuple_ids: HashMap::new(),
            gc_ctor_layouts: HashMap::new(),
            gc_structs: Vec::new(),
            gc_arrays: Vec::new(),
            gc_reference_list_ids: HashMap::new(),
            existential_payload_ids: HashMap::new(),
            existential_payload_type_ids: HashMap::new(),
            existential_upcasts: Vec::new(),
            lambda_gc_env_ids: HashMap::new(),
            ctor_field_records: HashMap::new(),
            mk_arities: HashSet::new(),
            next_label: 0,
            locals: HashMap::new(),
            fn_ret: HashMap::new(),
            fn_destination_layouts: HashMap::new(),
            fn_ret_closure_kind: HashMap::new(),
            fn_ret_tuple_slots: HashMap::new(),
            fn_ret_list_elem_tuple_slots: HashMap::new(),
            fn_ret_tuple_slot_list_elem: HashMap::new(),
            record_fields: HashMap::new(),
            record_field_types: HashMap::new(),
            record_generics: HashMap::new(),
            custom_eq_types: HashSet::new(),
            eq_types: HashSet::new(),
            packed_types: HashSet::new(),
            adt_variants: HashMap::new(),
            ctor_type_name: HashMap::new(),
            local_records: HashMap::new(),
            local_list_elem: HashMap::new(),
            local_payload_records: HashMap::new(),
            local_val_types: HashMap::new(),
            local_types: HashMap::new(),
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
            cur_fn_ret_ty: None,
            cur_fn_ret_slot: false,
            cur_fn_unique_ret: false,
            local_fn_ret_kind: HashMap::new(),
            local_fn_ownership: HashMap::new(),
            cur_fn_var: false,
            cur_fn_var_params: Vec::new(),
            cur_fn_var_cap_params: Vec::new(),
            uses_list_drop: false,
            uses_starts_with: false,
            inplace_push: HashSet::new(),
            sroa_candidates: HashSet::new(),
            sroa_active: HashMap::new(),
            scalar_sum_candidates: HashSet::new(),
            scalar_sum_active: HashMap::new(),
            scalar_sum_fused_values: HashMap::new(),
            scalar_record_producers: HashMap::new(),
            scalar_record_call_candidates: HashMap::new(),
            direct_list_builder_lets: HashMap::new(),
            direct_list_builder_loops: HashMap::new(),
            active_direct_list_builder: None,
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
            destination_forward_vars: HashSet::new(),
            destination_scratch_sites: HashMap::new(),
            counter_batch_stack: Vec::new(),
            counter_batch_used: Vec::new(),
            rc_owned_bindings: HashSet::new(),
            match_scrut_depth: 0,
            facts_stack: Vec::new(),
            loan_fact_stack: Vec::new(),
            drop_facts_stack: Vec::new(),
            summaries: analysis::Summaries::empty(),
            loan_facts,
            checked_module,
            access_facts,
            boundary_repair_sites: foldhash::HashSet::default(),
            synthesized_call_access: HashMap::new(),
            active_loan_events: Vec::new(),
            cur_fn_own_param: None,
            cur_fn_has_type_vars: false,
            cur_fn_name: String::new(),
            uses_diagnostic_sites: false,
            type_table,
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
            uses_args: false,
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
            dict_key_shapes: std::collections::BTreeMap::new(),
            dict_key_shape_modes: std::collections::BTreeMap::new(),
            eq_building: HashSet::new(),
            ts_wir_helpers: std::collections::BTreeMap::new(),
            ts_building: HashSet::new(),
            rcopy_wir_helpers: std::collections::BTreeMap::new(),
            rcopy_building: HashSet::new(),
            lambda_wir_funcs: Vec::new(),
            existential_table_len: 0,
            existential_dispatch_stride: 0,
            existential_call_level: 0,
            lambda_wir_index: HashMap::new(),
            lambda_threaded_index: HashMap::new(),
            ts_helpers: std::collections::BTreeMap::new(),
            adt_variant_names: HashMap::new(),
            anon_union_tag_codes: HashMap::new(),
            next_anon_union_tag_code: 0,
            clos_arities: HashSet::new(),
            apply_level: 0,
            assign_level: 0,
            loop_labels: Vec::new(),
        }
    }

    fn anon_union_tag_code(&mut self, tag: &str, arity: usize) -> i32 {
        let key = anon_union_tag_key(tag, arity);
        if let Some(code) = self.anon_union_tag_codes.get(&key) {
            return *code;
        }
        let code = self.next_anon_union_tag_code;
        self.next_anon_union_tag_code += 1;
        self.anon_union_tag_codes.insert(key, code);
        code
    }

    fn anon_union_tag_codes_for(&mut self, name: &str) -> Option<Vec<i32>> {
        let variants = witchy_types::typeck::anon_union_synthetic_variants(name)?;
        Some(
            variants
                .into_iter()
                .map(|(tag, arity)| self.anon_union_tag_code(&tag, arity))
                .collect(),
        )
    }

    fn kind_for_type(&self, t: &Type) -> Kind {
        let t = t.unqualified();
        match t {
            Type::Dyn(_, _) => Kind::GcRef(EXISTENTIAL_WRAPPER_ID),
            Type::Fn(_, _, _) => Kind::GcRef(CLOSURE_WRAPPER_ID),
            Type::Tuple(_) => self
                .gc_tuple_shape(t)
                .and_then(|shape| self.gc_tuple_ids.get(&shape).copied())
                .map(Kind::GcRef)
                .unwrap_or(Kind::I32),
            Type::Named(n, _) if self.transparent_externref_brands.contains(n) => Kind::ExternRef,
            Type::Named(n, _) if n == "List" => self
                .gc_reference_list_layout(t)
                .map(|(type_id, _, _)| Kind::GcRef(type_id))
                .unwrap_or(Kind::I32),
            Type::Named(n, args)
                if n == "Option"
                    && args.len() == 1
                    && !matches!(args[0].unqualified(), Type::Named(inner, _) if inner == "Option") =>
            {
                match self.kind_for_type(&args[0]) {
                    kind @ (Kind::ExternRef | Kind::GcRef(_)) => kind,
                    _ => ty_kind(t),
                }
            }
            Type::Named(_, _) => self
                .gc_struct_id_for_type(t)
                .map(Kind::GcRef)
                .unwrap_or_else(|| ty_kind(t)),
            _ => ty_kind(t),
        }
    }

    fn gc_struct_id_for_type(&self, ty: &Type) -> Option<u32> {
        let Type::Named(name, _) = ty.unqualified() else {
            return None;
        };
        let key = self.gc_lookup_type_key(ty);
        if let Some(id) = self.gc_aggregate_ids.get(&key).copied() {
            return Some(id);
        }
        if !type_has_var(ty) {
            return None;
        }

        // Result-only generic builders can retain an open source spelling even
        // when every reachable call selects one closed reference layout. Use
        // that layout only when the linked module proves it unambiguous.
        let owner = self.gc_nominal_names.get(name).unwrap_or(name);
        let prefix = format!("N{}:{owner}[", owner.len());
        let mut matches = self
            .gc_aggregate_ids
            .iter()
            .filter_map(|(candidate, id)| candidate.starts_with(&prefix).then_some(*id));
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    /// Storage kind for a field in a GC aggregate whose declaration is generic.
    /// Scalar type variables keep the existing universal-slot representation inside
    /// the typed struct; concrete reference substitutions are rejected by typeck.
    fn gc_field_storage_kind(&self, ty: &Type) -> Kind {
        let kind = self.kind_for_type(ty);
        if type_has_var(ty) && kind == Kind::I32 {
            Kind::I64
        } else {
            kind
        }
    }

    fn lower_gc_ctor_arg(
        &mut self,
        arg: &Expr,
        field_ty: &Type,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        let value = self.lower_expr(arg)?;
        if type_has_var(field_ty) && self.gc_field_storage_kind(field_ty) == Kind::I64 {
            let concrete = self.kind_of(arg);
            debug_assert!(
                !concrete.is_ref(),
                "typeck must reject reference substitutions in generic GC fields"
            );
            if concrete.is_ref() {
                return None;
            }
            Some(W::ToSlot(Box::new(value), Self::wir_kind(concrete)))
        } else {
            Some(value)
        }
    }

    fn gc_lookup_type_key(&self, ty: &Type) -> String {
        match ty.unqualified() {
            Type::Named(name, args) => {
                let name = self.gc_nominal_names.get(name).unwrap_or(name);
                format!(
                    "N{}:{}[{}]",
                    name.len(),
                    name,
                    args.iter()
                        .map(|arg| self.gc_lookup_type_key(arg))
                        .collect::<Vec<_>>()
                        .join(";")
                )
            }
            Type::Dyn(name, args) => format!(
                "D{}:{}[{}]",
                name.len(),
                name,
                args.iter()
                    .map(|arg| self.gc_lookup_type_key(arg))
                    .collect::<Vec<_>>()
                    .join(";")
            ),
            Type::Tuple(items) => format!(
                "T[{}]",
                items
                    .iter()
                    .map(|item| self.gc_lookup_type_key(item))
                    .collect::<Vec<_>>()
                    .join(";")
            ),
            Type::Fn(params, result, conventions) => {
                let conventions = conventions
                    .iter()
                    .map(|convention| match convention {
                        Convention::Let => 'l',
                        Convention::Borrow => 'b',
                        Convention::Var => 'v',
                        Convention::Own => 'o',
                    })
                    .collect::<String>();
                format!(
                    "F{conventions}[{}]->{}",
                    params
                        .iter()
                        .map(|param| self.gc_lookup_type_key(param))
                        .collect::<Vec<_>>()
                        .join(";"),
                    self.gc_lookup_type_key(result)
                )
            }
            Type::RecordCompose { .. } => unreachable!(
                "compiler invariant violated: record composition must be normalized before Wasm layout key generation"
            ),
            Type::Qualified(_, _) => unreachable!("unqualified above"),
        }
    }

    fn type_is_reference_list_candidate(&self, ty: &Type) -> bool {
        matches!(ty.unqualified(), Type::Named(name, args)
            if name == "List"
                && args.first().is_some_and(|element| {
                    matches!(
                        self.kind_for_type(element),
                        Kind::ExternRef | Kind::GcRef(_)
                    )
                }))
    }

    fn gc_reference_list_layout(&self, ty: &Type) -> Option<(u32, u32, Kind)> {
        let Type::Named(name, args) = ty.unqualified() else {
            return None;
        };
        if name != "List" {
            return None;
        }
        let element = args.first()?;
        let element_kind = self.kind_for_type(element);
        if !matches!(element_kind, Kind::ExternRef | Kind::GcRef(_)) {
            return None;
        }
        let type_id = if let Some(type_id) = self
            .gc_reference_list_ids
            .get(&self.gc_lookup_type_key(ty))
            .copied()
        {
            type_id
        } else if type_has_var(ty) {
            let mut matches = self
                .gc_reference_list_layouts()
                .into_iter()
                .filter_map(|(type_id, kind)| (kind == element_kind).then_some(type_id));
            let first = matches.next()?;
            if matches.next().is_some() {
                return None;
            }
            first
        } else {
            return None;
        };
        let array_id = type_id.checked_sub(self.gc_structs.len() as u32)?;
        Some((type_id, array_id, element_kind))
    }

    /// The type table predates compiler-owned existential packing, so a list
    /// literal may still be recorded as `List(Concrete)` even after each
    /// element has become an `ExistentialPack`. Recover the rewritten element
    /// type for this one literal; the registered GC layout remains keyed by the
    /// ordinary source-level `List(T)` type, never by a runtime special case.
    fn gc_reference_list_literal_layout(
        &self,
        list: &Expr,
        items: &[Expr],
    ) -> Option<(u32, u32, Kind)> {
        self.ast_type_of_expr(list)
            .as_ref()
            .and_then(|ty| self.gc_reference_list_layout(ty))
            .or_else(|| {
                let element = items.first().and_then(|item| self.ast_type_of_expr(item))?;
                self.gc_reference_list_layout(&Type::Named("List".to_string(), vec![element.clone()]))
                    .or_else(|| {
                        // The source annotation and its resolved compiler-owned
                        // element can have different identity spellings. Arrays
                        // are representation-typed, so a matching exact GC kind
                        // is sufficient and remains type-safe at the Wasm layer.
                        let element_kind = self.kind_for_type(&element);
                        self.gc_reference_list_layouts().into_iter().find_map(
                            |(type_id, candidate_kind)| {
                                if candidate_kind != element_kind {
                                    return None;
                                }
                                let array_id = type_id.checked_sub(self.gc_structs.len() as u32)?;
                                Some((type_id, array_id, element_kind))
                            },
                        )
                    })
            })
    }

    fn gc_reference_list_layouts(&self) -> Vec<(u32, Kind)> {
        let mut layouts = self
            .gc_reference_list_ids
            .values()
            .copied()
            .filter_map(|type_id| {
                let array_id = type_id.checked_sub(self.gc_structs.len() as u32)?;
                let element = self.gc_arrays.get(array_id as usize)?.element;
                let kind = match element {
                    witchy_wir::wir::Kind::I32 => Kind::I32,
                    witchy_wir::wir::Kind::I64 => Kind::I64,
                    witchy_wir::wir::Kind::F64 => Kind::F64,
                    witchy_wir::wir::Kind::ExternRef => Kind::ExternRef,
                    witchy_wir::wir::Kind::StructRef => return None,
                    witchy_wir::wir::Kind::GcRef(id) => Kind::GcRef(id),
                };
                Some((type_id, kind))
            })
            .collect::<Vec<_>>();
        layouts.sort_by_key(|(type_id, _)| *type_id);
        layouts.dedup_by_key(|(type_id, _)| *type_id);
        layouts
    }

    fn collect_unit_gc_ids_type(&self, ty: &Type, ids: &mut BTreeSet<u32>) {
        if let Kind::GcRef(id) = self.kind_for_type(ty) {
            ids.insert(id);
        }
        match ty.unqualified() {
            Type::Named(_, args) | Type::Dyn(_, args) | Type::Tuple(args) => {
                for arg in args {
                    self.collect_unit_gc_ids_type(arg, ids);
                }
            }
            Type::Fn(params, result, _) => {
                for param in params {
                    self.collect_unit_gc_ids_type(param, ids);
                }
                self.collect_unit_gc_ids_type(result, ids);
            }
            Type::RecordCompose { .. } => unreachable!(
                "compiler invariant violated: record composition must be normalized before Wasm GC layout collection"
            ),
            Type::Qualified(_, _) => unreachable!("unqualified above"),
        }
    }

    fn collect_unit_gc_ids_expr(&self, expr: &Expr, ids: &mut BTreeSet<u32>) {
        if let Some(ty) = self.ast_type_of_expr(expr) {
            self.collect_unit_gc_ids_type(&ty, ids);
        }
        crate::escape::for_each_immediate_subexpr(expr, &mut |child| {
            self.collect_unit_gc_ids_expr(child, ids);
        });
    }

    fn collect_unit_gc_ids_block(&self, block: &Block, ids: &mut BTreeSet<u32>) {
        for stmt in &block.stmts {
            let expr = match stmt {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Yield(value)
                | Stmt::Expr(value) => Some(value),
                Stmt::Return(value) => value.as_ref(),
                Stmt::Break | Stmt::Continue => None,
            };
            if let Some(expr) = expr {
                self.collect_unit_gc_ids_expr(expr, ids);
            }
        }
    }

    fn unit_gc_ids(
        &self,
        params: impl IntoIterator<Item = Type>,
        result: Option<Type>,
        body: &Block,
    ) -> BTreeSet<u32> {
        let mut ids = BTreeSet::new();
        for param in params {
            self.collect_unit_gc_ids_type(&param, &mut ids);
        }
        if let Some(result) = result {
            self.collect_unit_gc_ids_type(&result, &mut ids);
        }
        self.collect_unit_gc_ids_block(body, &mut ids);
        ids
    }

    fn gc_tuple_shape(&self, ty: &Type) -> Option<GcTupleShape> {
        let Type::Tuple(items) = ty.unqualified() else {
            return None;
        };
        if type_has_var(ty) {
            return None;
        }
        let fields: Vec<GcFieldShape> =
            items.iter().map(|item| self.gc_field_shape(item)).collect::<Option<_>>()?;
        fields
            .iter()
            .any(|field| {
                matches!(
                    field,
                    GcFieldShape::ExternRef
                        | GcFieldShape::Function
                        | GcFieldShape::ReferenceList(_)
                        | GcFieldShape::Nominal(_)
                        | GcFieldShape::Tuple(_)
                )
            })
            .then_some(GcTupleShape(fields))
    }

    fn gc_field_shape(&self, ty: &Type) -> Option<GcFieldShape> {
        let ty = ty.unqualified();
        Some(match ty {
            Type::Fn(_, _, _) => GcFieldShape::Function,
            Type::Tuple(_) => self
                .gc_tuple_shape(ty)
                .map(GcFieldShape::Tuple)
                .unwrap_or(GcFieldShape::I32),
            Type::Named(_, _) if self.type_is_direct_externref(ty) => {
                GcFieldShape::ExternRef
            }
            Type::Named(_, _) if self.type_is_reference_list_candidate(ty) => {
                GcFieldShape::ReferenceList(self.gc_lookup_type_key(ty))
            }
            Type::Named(name, args)
                if name == "Option"
                    && args.len() == 1
                    && self.option_reference_inner(ty).is_some() =>
            {
                self.option_reference_inner(ty)
                    .and_then(|(inner, _)| self.gc_field_shape(inner))
                    .unwrap_or(GcFieldShape::I32)
            }
            Type::Named(_, _) if self.gc_struct_id_for_type(ty).is_some() => {
                GcFieldShape::Nominal(self.gc_lookup_type_key(ty))
            }
            _ => match ty_kind(ty) {
                Kind::I64 => GcFieldShape::I64,
                Kind::F64 => GcFieldShape::F64,
                Kind::I32 => GcFieldShape::I32,
                Kind::ExternRef | Kind::GcRef(_) => return None,
            },
        })
    }

    fn type_is_direct_externref(&self, t: &Type) -> bool {
        match t.unqualified() {
            Type::Named(n, _) if is_builtin_externref_type(n) => true,
            Type::Named(n, args) if args.is_empty() => self.transparent_externref_brands.contains(n),
            _ => false,
        }
    }

    fn option_reference_inner<'a>(&self, t: &'a Type) -> Option<(&'a Type, Kind)> {
        let Type::Named(n, args) = t.unqualified() else {
            return None;
        };
        let inner = args.first()?;
        if n != "Option"
            || args.len() != 1
            || matches!(inner.unqualified(), Type::Named(name, _) if name == "Option")
        {
            return None;
        }
        let kind = self.kind_for_type(inner);
        matches!(kind, Kind::ExternRef | Kind::GcRef(_)).then_some((inner, kind))
    }

    fn reference_try_shape(&self, expr: &Expr) -> Option<ReferenceTryShape> {
        let owner = self.ast_type_of_expr(expr)?;
        if let Some((_, payload_kind)) = self.option_reference_inner(&owner) {
            return Some(ReferenceTryShape::Nullable { payload_kind });
        }
        let Type::Named(name, _) = owner.unqualified() else {
            return None;
        };
        let (success, failure) = match name.as_str() {
            "Option" => ("Some", "None"),
            "Result" => ("Ok", "Err"),
            _ => return None,
        };
        let (layout, struct_id) = self.gc_layout_for_ctor(success, Some(&owner))?;
        let (failure_layout, failure_id) =
            self.gc_layout_for_ctor(failure, Some(&owner))?;
        if failure_id != struct_id {
            return None;
        }
        let payload_kind = self.kind_for_type(layout.field_types.first()?);
        let failure_kind = failure_layout
            .field_types
            .first()
            .map(|ty| self.kind_for_type(ty));
        Some(ReferenceTryShape::Tagged {
            struct_id,
            success_tag: layout.tag?,
            payload_field: layout.field_base,
            payload_kind,
            failure_field: failure_kind.map(|_| failure_layout.field_base),
            failure_kind,
        })
    }

    fn reference_try_tmp(kind: Kind) -> Option<String> {
        match kind {
            Kind::ExternRef => Some(MATCH_REF_TMP.to_string()),
            Kind::GcRef(id) => Some(call_result_gc_tmp(id)),
            Kind::I32 | Kind::I64 | Kind::F64 => None,
        }
    }

    fn gc_layout_for_ctor(
        &self,
        name: &str,
        owner_ty: Option<&Type>,
    ) -> Option<(GcCtorLayout, u32)> {
        let owner_key = owner_ty.map(|ty| self.gc_lookup_type_key(ty));
        let bare_name = name.rsplit('.').next().unwrap_or(name);
        let layout = owner_key
            .as_ref()
            .and_then(|owner_key| {
                self.gc_ctor_layouts
                    .get(&(owner_key.clone(), name.to_string()))
                    .or_else(|| {
                        self.gc_ctor_layouts.iter().find_map(
                            |((owner, ctor), layout)| {
                                (owner == owner_key
                                    && ctor.rsplit('.').next().unwrap_or(ctor) == bare_name)
                                    .then_some(layout)
                            },
                        )
                    })
            })
            .or_else(|| {
                let owner_ty = owner_ty?.unqualified();
                let Type::Named(owner, _) = owner_ty else {
                    return None;
                };
                if !type_has_var(owner_ty) {
                    return None;
                }
                let owner = self.gc_nominal_names.get(owner).unwrap_or(owner);
                let prefix = format!("N{}:{owner}[", owner.len());
                let mut matches = self.gc_ctor_layouts.iter().filter_map(
                    |((candidate_owner, ctor), layout)| {
                        (candidate_owner.starts_with(&prefix)
                            && ctor.rsplit('.').next().unwrap_or(ctor) == bare_name)
                            .then_some(layout)
                    },
                );
                let first = matches.next()?;
                matches.next().is_none().then_some(first)
            })
            .or_else(|| {
                if owner_ty.is_some() {
                    return None;
                }
                let mut matches = self.gc_ctor_layouts.iter().filter_map(
                    |((_owner, ctor), layout)| {
                        (ctor.rsplit('.').next().unwrap_or(ctor) == bare_name)
                            .then_some(layout)
                    },
                );
                let first = matches.next()?;
                matches.next().is_none().then_some(first)
            });
        let layout = layout?.clone();
        let id = self.gc_aggregate_ids.get(&layout.owner_key).copied()?;
        Some((layout, id))
    }

    /// The WASM kind a closure-valued expression returns: a function-typed
    /// variable's tracked return kind, a lambda's body kind, else i32.
    fn apply_ret_kind(&self, func: &Expr) -> Kind {
        self.closure_ret_kind_of(func).unwrap_or(Kind::I32)
    }

    fn closure_param_kinds(&self, func: &Expr) -> Vec<Kind> {
        let Some(ty) = self.ast_type_of_expr(func) else {
            return Vec::new();
        };
        match ty.unqualified() {
            Type::Fn(params, _, _) => params.iter().map(|ty| self.kind_for_type(ty)).collect(),
            _ => Vec::new(),
        }
    }

    fn closure_result_type(&self, func: &Expr) -> Option<Type> {
        if let Expr::Lambda { ret, .. } = func
            && let Some(result) = ret
        {
            return Some(result.clone());
        }
        let ty = self.ast_type_of_expr(func)?;
        let Type::Fn(_, result, _) = ty.unqualified() else {
            return None;
        };
        Some(result.as_ref().clone())
    }

    fn ownership_envelope_for_signature(
        signature: &witchy_types::access::AccessSignature,
    ) -> ClosureOwnershipEnvelope {
        let fact = analysis::call_ownership_fact(signature);
        ClosureOwnershipEnvelope {
            // Generic and uniform callables retain the compatibility state
            // channel. Named exact-layout callables refine it below.
            own_capacity_param: fact.consuming_state_param(),
            var_capacity_params: fact.var_capacity_params().to_vec(),
            unique_capacity_result: fact.unique_capacity_result(),
        }
    }

    fn ownership_envelope_for_named_signature(
        &self,
        name: &str,
        signature: &witchy_types::access::AccessSignature,
    ) -> ClosureOwnershipEnvelope {
        let mut envelope = Self::ownership_envelope_for_signature(signature);
        let Some(layout) = self.callable_layouts.get(name) else {
            return envelope;
        };
        if envelope.own_capacity_param.is_some_and(|index| {
            layout.parameters().get(index).is_some_and(Option::is_some)
        }) {
            envelope.own_capacity_param = None;
        }
        envelope.var_capacity_params.retain(|index| {
            !layout
                .parameters()
                .get(*index)
                .is_some_and(Option::is_some)
        });
        if layout.result().is_some() {
            envelope.unique_capacity_result = false;
        }
        envelope
    }

    fn signature_has_unique_layout_result(
        signature: &witchy_types::access::AccessSignature,
    ) -> bool {
        signature
            .result()
            .qualifiers()
            .contains(&witchy_types::access::AccessQualifier::Unique)
            && matches!(
                signature.result().ownership_output(),
                Some(witchy_types::access::OwnershipStateClass::LayoutDependent { .. })
            )
    }

    fn ownership_envelope_for_type(ty: &Type) -> ClosureOwnershipEnvelope {
        witchy_types::access::AccessSignature::from_function_type(ty)
            .ok()
            .as_ref()
            .map(Self::ownership_envelope_for_signature)
            .unwrap_or_default()
    }

    fn call_access_signature(
        &self,
        call: &Expr,
    ) -> Option<&witchy_types::access::AccessSignature> {
        self.access_facts
            .call_at(self.checked_module, call)
            .or_else(|| {
                self.synthesized_call_access
                    .get(&(call as *const Expr as usize))
            })
    }

    fn call_ownership_envelope(&self, call: &Expr) -> ClosureOwnershipEnvelope {
        self.call_access_signature(call)
            .map(Self::ownership_envelope_for_signature)
            .unwrap_or_default()
    }

    fn expression_returns_unique_capacity(&self, expression: &Expr) -> bool {
        match expression {
            Expr::Unary { op: UnOp::Move, expr } => {
                self.expression_returns_unique_capacity(expr)
            }
            _ => self.call_ownership_envelope(expression).unique_capacity_result,
        }
    }

    fn closure_access_signature(
        &self,
        func: &Expr,
    ) -> Option<witchy_types::access::AccessSignature> {
        if let Some(signature) = self.access_facts.callable_at(self.checked_module, func) {
            return Some(signature.clone());
        }
        if let Expr::Var(name) = func {
            if let Some(signature) = self.access_facts.declaration(name) {
                return Some(signature.clone());
            }
        }
        // A type-only signature lacks the module's nominal borrow catalog and
        // therefore carries only conservative root relations. Source callable
        // values must use their exact checked fact; generated direct function
        // values are covered by the declaration query above.
        None
    }

    fn closure_ownership_envelope(&self, func: &Expr) -> ClosureOwnershipEnvelope {
        self.closure_access_signature(func)
            .as_ref()
            .map(Self::ownership_envelope_for_signature)
            .or_else(|| {
                let Expr::Var(name) = func else { return None };
                self.local_fn_ownership.get(name).cloned()
            })
            .unwrap_or_default()
    }

    fn closure_uses_typed_abi(param_kinds: &[Kind], result_kind: Kind) -> bool {
        matches!(result_kind, Kind::ExternRef | Kind::GcRef(_))
            || param_kinds
                .iter()
                .any(|kind| matches!(kind, Kind::ExternRef | Kind::GcRef(_)))
    }

    fn closure_signature(
        arity: usize,
        param_kinds: &[Kind],
        result_kind: Kind,
        writebacks: &[ClosureWriteback],
        typed_abi: bool,
        ownership: &ClosureOwnershipEnvelope,
    ) -> witchy_wir::wir::ClosureSignature {
        if !typed_abi && !ownership.has_state() {
            return witchy_wir::wir::gc_slot_closure_signature(
                arity,
                1 + writebacks.len(),
            );
        }
        let mut params = vec![witchy_wir::wir::Kind::GcRef(CLOSURE_WRAPPER_ID)];
        params.extend(param_kinds.iter().copied().map(|kind| {
            if typed_abi {
                Self::wir_kind(kind)
            } else {
                witchy_wir::wir::Kind::I64
            }
        }));
        params.extend(
            ownership
                .own_capacity_param
                .iter()
                .map(|_| witchy_wir::wir::Kind::I32),
        );
        params.extend(
            ownership
                .var_capacity_params
                .iter()
                .map(|_| witchy_wir::wir::Kind::I32),
        );
        let mut results = vec![if typed_abi {
            Self::wir_kind(result_kind)
        } else {
            witchy_wir::wir::Kind::I64
        }];
        if ownership.unique_capacity_result {
            results.push(witchy_wir::wir::Kind::I32);
        }
        results.extend(writebacks.iter().map(|(_, kind, _)| {
            if typed_abi {
                Self::wir_kind(*kind)
            } else {
                witchy_wir::wir::Kind::I64
            }
        }));
        results.extend(
            ownership
                .var_capacity_params
                .iter()
                .map(|_| witchy_wir::wir::Kind::I32),
        );
        if ownership.own_capacity_param.is_some() {
            results.push(witchy_wir::wir::Kind::I32);
        }
        witchy_wir::wir::ClosureSignature { params, results }
    }

    /// Lower closure arguments and capture every `var` place. Scalar signatures use
    /// universal slots; reference-bearing signatures preserve their exact WIR kinds.
    /// Coordinates are evaluated into typed scratch locals before the call, and final
    /// values are rebuilt after the call.
    fn lower_closure_args(
        &mut self,
        args: &[Expr],
        access: &witchy_types::access::AccessSignature,
        param_kinds: &[Kind],
        typed_abi: bool,
        ownership: &ClosureOwnershipEnvelope,
    ) -> Option<LoweredClosureArgs> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        let mut slots = Vec::with_capacity(args.len());
        let mut writebacks = Vec::new();
        let mut next_coordinate = 0;
        for (index, arg) in args.iter().enumerate() {
            let is_var = access.params().get(index).is_some_and(|param| {
                param.kind() == witchy_types::access::AccessKind::ExclusiveWriteback
            });
            let kind = if is_var {
                param_kinds.get(index).copied().unwrap_or_else(|| self.kind_of(arg))
            } else {
                self.kind_of(arg)
            };
            let value = if is_var {
                let mut prelude = Vec::new();
                let place = self.capture_codegen_place(arg, &mut next_coordinate, &mut prelude)?;
                if writebacks.len() >= SCRUT_POOL {
                    return None;
                }
                let mut coordinates = Vec::new();
                Self::codegen_place_coordinates(&place, &mut coordinates);
                for (coordinate, coordinate_kind, value_type) in &coordinates {
                    self.locals.insert(coordinate.clone(), *coordinate_kind);
                    self.local_val_types.insert(coordinate.clone(), *value_type);
                }
                let read = self.lower_codegen_place_read(&place, kind);
                for (coordinate, _, _) in &coordinates {
                    self.locals.remove(coordinate);
                    self.local_val_types.remove(coordinate);
                }
                let read = read?;
                writebacks.push((
                    place,
                    kind,
                    format!("__witchy_scrut_save_{}", writebacks.len()),
                ));
                if prelude.is_empty() {
                    read
                } else {
                    prelude.push(N::Push(read));
                    W::Seq(prelude)
                }
            } else {
                self.lower_expr(arg)?
            };
            slots.push(if typed_abi {
                value
            } else {
                W::ToSlot(Box::new(value), Self::wir_kind(kind))
            });
        }
        let mut capacity_dests = Vec::with_capacity(ownership.var_capacity_params.len());
        for (ordinal, index) in ownership.var_capacity_params.iter().copied().enumerate() {
            let tracked_root = match args.get(index) {
                Some(Expr::Var(root)) if self.inplace_push.contains(root) => Some(root),
                _ => None,
            };
            slots.push(match tracked_root {
                Some(root) => W::GetLocal(format!("{root}__cap")),
                None => W::ConstI32(0),
            });
            capacity_dests.push(match tracked_root {
                Some(root) => format!("{root}__cap"),
                None => var_scratch("cap", ordinal, Kind::I32),
            });
        }
        if let Some(index) = ownership.own_capacity_param {
            let capacity = args
                .get(index)
                .map(|arg| self.owned_argument_cap(arg))
                .unwrap_or(W::ConstI32(0));
            let insert_at = slots.len() - ownership.var_capacity_params.len();
            slots.insert(insert_at, capacity);
        }
        Some((slots, writebacks, capacity_dests))
    }

    fn closure_call_dests(
        result_kind: Kind,
        typed_abi: bool,
        writebacks: &[ClosureWriteback],
        capacity_dests: &[String],
        ownership: &ClosureOwnershipEnvelope,
    ) -> Vec<String> {
        let mut dests = vec![if typed_abi {
            call_result_tmp(result_kind)
        } else {
            MATCH_TMP.to_string()
        }];
        if ownership.unique_capacity_result {
            dests.push(UNIQUE_RESULT_CAP_TMP.to_string());
        }
        dests.extend(writebacks.iter().enumerate().map(|(index, (_, kind, scratch))| {
            if typed_abi {
                var_scratch("result", index, *kind)
            } else {
                scratch.clone()
            }
        }));
        dests.extend(capacity_dests.iter().cloned());
        if ownership.own_capacity_param.is_some() {
            dests.push("__witchy_owncap".to_string());
        }
        dests
    }

    fn finish_closure_multi_call(
        &mut self,
        call: witchy_wir::wir::WirNode,
        writebacks: Vec<ClosureWriteback>,
        result_kind: Kind,
        typed_abi: bool,
        count_indirect_ownership: bool,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        let mut seq = Vec::new();
        if count_indirect_ownership {
            seq.push(N::SetGlobal {
                global: "__witchy_indirect_ownership_calls".to_string(),
                value: W::Binary {
                    op: witchy_wir::wir::BinOp::Add,
                    kind: witchy_wir::wir::Kind::I64,
                    lhs: Box::new(W::GetGlobal(
                        "__witchy_indirect_ownership_calls".to_string(),
                    )),
                    rhs: Box::new(W::ConstI64(1)),
                },
            });
        }
        seq.push(call);
        if !typed_abi {
            for (index, (_, kind, scratch)) in writebacks.iter().enumerate() {
                seq.push(N::SetLocal {
                    local: var_scratch("result", index, *kind),
                    value: W::FromSlot(
                        Box::new(W::GetLocal(scratch.clone())),
                        Self::wir_kind(*kind),
                    ),
                });
            }
        }

        let mut groups: Vec<(String, Kind, Expr)> = Vec::new();
        let mut coordinates = Vec::new();
        for (index, (place, value_kind, _)) in writebacks.iter().enumerate() {
            let result = var_scratch("result", index, *value_kind);
            let root = Self::codegen_place_root(place).to_string();
            let root_kind = self.locals.get(&root).copied()?;
            Self::codegen_place_coordinates(place, &mut coordinates);
            let root_value = groups
                .iter()
                .find(|(candidate, _, _)| candidate == &root)
                .map(|(_, _, value)| value.clone())
                .unwrap_or_else(|| Expr::Var(root.clone()));
            let update =
                Self::codegen_place_update_from(place, Expr::Var(result.clone()), &root_value);
            if let Some((_, _, value)) =
                groups.iter_mut().find(|(candidate, _, _)| candidate == &root)
            {
                *value = update;
            } else {
                groups.push((root, root_kind, update));
            }
        }
        coordinates.sort_by(|left, right| left.0.cmp(&right.0));
        coordinates.dedup_by(|left, right| left.0 == right.0);
        for (coordinate, kind, value_type) in &coordinates {
            self.locals.insert(coordinate.clone(), *kind);
            self.local_val_types.insert(coordinate.clone(), *value_type);
        }
        for (index, (_, value_kind, _)) in writebacks.iter().enumerate() {
            self.locals
                .insert(var_scratch("result", index, *value_kind), *value_kind);
        }
        let mut commits = Vec::with_capacity(groups.len());
        for (index, (root, root_kind, update)) in groups.iter().enumerate() {
            let root_scratch = var_scratch("root", index, *root_kind);
            let update_w = self.lower_expr(update)?;
            seq.push(N::SetLocal { local: root_scratch.clone(), value: update_w });
            commits.push((root.clone(), root_scratch));
        }
        for (index, (_, value_kind, _)) in writebacks.iter().enumerate() {
            self.locals.remove(&var_scratch("result", index, *value_kind));
        }
        for (coordinate, _, _) in &coordinates {
            self.locals.remove(coordinate);
            self.local_val_types.remove(coordinate);
        }
        for (root, scratch) in commits {
            seq.push(N::SetLocal { local: root, value: W::GetLocal(scratch) });
        }
        let result = if typed_abi {
            W::GetLocal(call_result_tmp(result_kind))
        } else {
            W::FromSlot(
                Box::new(W::GetLocal(MATCH_TMP.to_string())),
                Self::wir_kind(result_kind),
            )
        };
        seq.push(N::Push(result));
        Some(W::Seq(seq))
    }

    /// The call-return kind of a closure VALUE, when determinable: a lambda
    /// literal's body kind, a call to a `-> fn(...) -> RET` function, or another
    /// closure-bound variable. Used to track `let f = <closure>` for later `f(x)`.
    fn closure_ret_kind_of(&self, value: &Expr) -> Option<Kind> {
        if let Some(ty) = self.ast_type_of_expr(value)
            && let Type::Fn(_, ret, _) = ty.unqualified()
        {
            return Some(self.kind_for_type(ret));
        }
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

    fn ast_type_of_expr(&self, e: &Expr) -> Option<Type> {
        // These compiler-owned nodes are introduced after annotation. Their
        // carried type must win over a structural TypeTable lookup, which can
        // still match the rewritten node's pre-pack concrete expression.
        match e {
            Expr::ExistentialPack { ty, .. } => Some(ty.clone()),
            Expr::ExistentialUpcast { ty, .. } => Some(ty.clone()),
            Expr::ExistentialCall { result, .. } => Some(result.clone()),
            Expr::Var(name) => self
                .local_types
                .get(name)
                .cloned()
                .or_else(|| {
                    self.type_table
                        .type_of(e)
                        .and_then(witchy_types::typeck::ty_to_ast)
                }),
            _ => self
                .type_table
                .type_of(e)
                .and_then(witchy_types::typeck::ty_to_ast),
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
            Expr::Call { name, args }
                if matches!(name.as_str(), "dict.insert" | intrinsics::DICT_INSERT)
                    && args.len() == 3 =>
            {
                match self.val_type_of(&args[2]) {
                    ValType::Other => None,
                    vt => Some(vt),
                }
            }
            Expr::Var(v) => self.local_dict_value_valtype.get(v).copied(),
            _ => None,
        }
        .or_else(|| match self.type_table.type_of(value).and_then(witchy_types::typeck::ty_to_ast) {
            Some(Type::Named(n, args)) if n == "Dict" && args.len() == 2 => {
                match ty_to_valtype(&args[1]) {
                    ValType::Other => None,
                    vt => Some(vt),
                }
            }
            _ => None,
        })
    }

    /// The scalar KEY type a Dict holds (the `insert`'s key, or a Dict variable's
    /// tracked key type), so `pairs(d)` destructures the key slot correctly.
    fn dict_key_valtype_of(&self, value: &Expr) -> Option<ValType> {
        match value {
            Expr::Call { name, args }
                if matches!(name.as_str(), "dict.insert" | intrinsics::DICT_INSERT)
                    && args.len() == 3 =>
            {
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
            Expr::Call { name, args } if name == intrinsics::DICT_PAIRS && args.len() == 1 => {
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
            Expr::Call { name, args } if name == intrinsics::LIST_AT && args.len() == 2 => {
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
                if name == intrinsics::STRING_SPLIT
                    || name == intrinsics::STRING_CHARS
                    || name == "list" =>
            {
                ValType::Str
            }
            // `values(d)` yields a list of the Dict's values; carry their type so
            // `for v in values(d)` recovers an Int value as i64.
            Expr::Call { name, args } if name == intrinsics::DICT_VALUES && args.len() == 1 => match &args[0] {
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
            Expr::Call { name, args } if name == intrinsics::DICT_KEYS && args.len() == 1 => match &args[0] {
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
            Expr::Call { name, args } if name == intrinsics::LIST_AT && args.len() == 2 => {
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
                } else if let Some(Type::Named(name, args)) =
                    self.local_types.get(v).map(Type::unqualified)
                    && name == "List"
                {
                    args.first().map(ty_to_valtype).unwrap_or(ValType::Other)
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
        if let Some(Type::Named(name, args)) = self.ast_type_of_expr(iter).as_ref().map(Type::unqualified)
            && name == "List"
            && let Some(element) = args.first()
        {
            return self.kind_for_type(element);
        }
        if self.elem_record_type_of(iter).is_some() {
            Kind::I32
        } else {
            valtype_kind(self.elem_val_type_of(iter))
        }
    }

    fn substitute_pattern_type(ty: &Type, bindings: &HashMap<String, Type>) -> Type {
        match ty {
            Type::Named(name, args) if args.is_empty() && bindings.contains_key(name) => {
                bindings[name].clone()
            }
            Type::Named(name, args) => Type::Named(
                name.clone(),
                args.iter()
                    .map(|arg| Self::substitute_pattern_type(arg, bindings))
                    .collect(),
            ),
            Type::Dyn(name, args) => Type::Dyn(
                name.clone(),
                args.iter()
                    .map(|arg| Self::substitute_pattern_type(arg, bindings))
                    .collect(),
            ),
            Type::Tuple(items) => Type::Tuple(
                items
                    .iter()
                    .map(|item| Self::substitute_pattern_type(item, bindings))
                    .collect(),
            ),
            Type::Fn(params, result, conventions) => Type::Fn(
                params
                    .iter()
                    .map(|param| Self::substitute_pattern_type(param, bindings))
                    .collect(),
                Box::new(Self::substitute_pattern_type(result, bindings)),
                conventions.clone(),
            ),
            Type::Qualified(qualifier, inner) => Type::Qualified(
                qualifier.clone(),
                Box::new(Self::substitute_pattern_type(inner, bindings)),
            ),
            Type::RecordCompose { base, fields } => Type::RecordCompose {
                base: Box::new(Self::substitute_pattern_type(base, bindings)),
                fields: fields
                    .iter()
                    .map(|(name, field)| {
                        (name.clone(), Self::substitute_pattern_type(field, bindings))
                    })
                    .collect(),
            },
        }
    }

    fn ctor_pattern_field_types(&self, name: &str, expected: Option<&Type>) -> Option<Vec<Type>> {
        if let Some((layout, _)) = self.gc_layout_for_ctor(name, expected) {
            return Some(layout.field_types);
        }
        match (name, expected.map(Type::unqualified)) {
            ("Some", Some(Type::Named(owner, args))) if owner == "Option" => {
                return args.first().cloned().map(|ty| vec![ty]);
            }
            ("Ok", Some(Type::Named(owner, args))) if owner == "Result" => {
                return args.first().cloned().map(|ty| vec![ty]);
            }
            ("Err", Some(Type::Named(owner, args))) if owner == "Result" => {
                return args.get(1).cloned().map(|ty| vec![ty]);
            }
            _ => {}
        }
        let fields = self
            .ctors
            .get(name)
            .map(|&(tag, _)| tag as usize)
            .and_then(|tag| {
                let ty = self.ctor_type_name.get(name)?;
                self.adt_variants.get(ty)?.get(tag).cloned()
            })?;
        let owner = self.ctor_type_name.get(name)?;
        let Some(Type::Named(expected_owner, args)) = expected.map(Type::unqualified) else {
            return Some(fields);
        };
        let Some(params) = self
            .record_generics
            .get(owner)
            .filter(|params| expected_owner == owner.as_str() && params.len() == args.len())
        else {
            return Some(fields);
        };
        let bindings = params
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect::<HashMap<_, _>>();
        Some(
            fields
                .iter()
                .map(|field| Self::substitute_pattern_type(field, &bindings))
                .collect(),
        )
    }

    fn anon_union_pattern_field_types(
        expected: Option<&Type>,
        tag: &str,
        arity: usize,
    ) -> Option<Vec<Type>> {
        let Type::Named(name, union_args) = expected?.unqualified() else {
            return None;
        };
        let variants = witchy_types::typeck::anon_union_synthetic_variants(name)?;
        let mut offset = 0usize;
        for (variant, variant_arity) in variants {
            let end = offset.checked_add(variant_arity)?;
            if end > union_args.len() {
                return None;
            }
            if variant == tag && variant_arity == arity {
                return Some(union_args[offset..end].to_vec());
            }
            offset = end;
        }
        None
    }

    fn bind_pattern_value_types(&mut self, pat: &Pattern, expected: Option<&Type>) {
        match pat {
            Pattern::Var(name) if name != "_" => {
                if let Some(ty) = expected {
                    let vt = ty_to_valtype(ty);
                    self.local_types.insert(name.clone(), ty.clone());
                    self.local_val_types.insert(name.clone(), vt);
                    self.locals.insert(name.clone(), self.kind_for_type(ty));
                    if let Type::Fn(_, ret, _) = ty.unqualified() {
                        self.local_fn_ret_kind.insert(name.clone(), self.kind_for_type(ret));
                        let envelope = Self::ownership_envelope_for_type(ty);
                        if envelope.has_state() {
                            self.local_fn_ownership.insert(name.clone(), envelope);
                        }
                    }
                }
            }
            Pattern::Tuple(parts) => {
                let slots = match expected.map(Type::unqualified) {
                    Some(Type::Tuple(slots)) => Some(slots.as_slice()),
                    Some(Type::Named(name, slots)) if name.starts_with("Tuple") => Some(slots.as_slice()),
                    _ => None,
                };
                for (index, part) in parts.iter().enumerate() {
                    self.bind_pattern_value_types(part, slots.and_then(|items| items.get(index)));
                }
            }
            Pattern::List { elems, rest } => {
                let elem_ty = match expected.map(Type::unqualified) {
                    Some(Type::Named(name, args)) if name == "List" => args.first(),
                    _ => None,
                };
                for elem in elems {
                    self.bind_pattern_value_types(elem, elem_ty);
                }
                if let (Some(Some(name)), Some(ty)) = (rest, expected) {
                    self.local_types.insert(name.clone(), ty.clone());
                    self.local_val_types.insert(name.clone(), ty_to_valtype(ty));
                    self.locals.insert(name.clone(), self.kind_for_type(ty));
                }
            }
            Pattern::Ctor { name, args } => {
                let fields = self.ctor_pattern_field_types(name, expected);
                for (index, arg) in args.iter().enumerate() {
                    self.bind_pattern_value_types(arg, fields.as_ref().and_then(|items| items.get(index)));
                }
            }
            Pattern::AnonCtor { tag, args } => {
                let fields = Self::anon_union_pattern_field_types(expected, tag, args.len());
                for (index, arg) in args.iter().enumerate() {
                    self.bind_pattern_value_types(arg, fields.as_ref().and_then(|items| items.get(index)));
                }
            }
            Pattern::Or(alts) => {
                if let Some(first) = alts.first() {
                    self.bind_pattern_value_types(first, expected);
                }
            }
            Pattern::Wildcard
            | Pattern::Var(_)
            | Pattern::Int(_)
            | Pattern::Str(_)
            | Pattern::Bool(_)
            | Pattern::Duration(_)
            | Pattern::IntRange { .. } => {}
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
                    let fn_ownership = self.closure_ownership_envelope(value);
                    // The legacy shape inference cannot recover generic `?`
                    // payloads or an elided result from a value-returning `var`
                    // call. Use the authoritative table for those two shapes;
                    // retain the established inference elsewhere because several
                    // specialized lowerings intentionally choose their local ABI.
                    let needs_resolved_type = matches!(value, Expr::Try(_))
                        || matches!(value, Expr::Call { name, .. }
                            if self.fn_conventions.get(name).is_some_and(|cs|
                                cs.contains(&Convention::Var)));
                    let resolved_type = self
                        .type_table
                        .type_of(value)
                        .and_then(witchy_types::typeck::ty_to_ast);
                    let inferred_type = if needs_resolved_type
                        || matches!(resolved_type.as_ref().map(Type::unqualified), Some(Type::Fn(..)))
                    {
                        resolved_type.clone()
                    } else {
                        None
                    };
                    let k = inferred_type
                        .as_ref()
                        .map(|ty| self.kind_for_type(ty))
                        .unwrap_or_else(|| self.kind_of(value));
                    self.locals.insert(name.clone(), k);
                    let vt = inferred_type
                        .as_ref()
                        .map(ty_to_valtype)
                        .unwrap_or_else(|| self.val_type_of(value));
                    self.local_val_types.insert(name.clone(), vt);
                    // Preserve the full checker-resolved type even when the
                    // legacy scalar-kind inference remains authoritative for
                    // this binding's ABI. Later generic operations need the
                    // arguments (`List(Int)`, `Iter(Int)`, ...) rather than only
                    // the container's i32 runtime kind. In particular, a value
                    // returned through a local function call must not make a
                    // subsequent `list.at` fall back to an i32 element.
                    if let Some(t) = resolved_type {
                        self.local_types.insert(name.clone(), t);
                    }
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
                    if fn_ownership.has_state() {
                        self.local_fn_ownership.insert(name.clone(), fn_ownership);
                    } else {
                        self.local_fn_ownership.remove(name);
                    }
                    // A binding to a tuple literal records its element slot value
                    // types, so a later `let (a, b) = name` types `a`/`b` (and
                    // gives Float/Int elements the right kind).
                    if let Expr::Tuple(items) = value {
                        self.local_tuple_slots
                            .insert(name.clone(), items.iter().map(|e| self.val_type_of(e)).collect());
                    } else if let Expr::Call { name: fname, args } = value {
                        if fname == intrinsics::LIST_AT && args.len() == 2 {
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
                                if fname == intrinsics::LIST_AT && args.len() == 2 {
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
                            // The legacy slot table classifies reference fields as
                            // `Other`/i32. Re-apply the checked tuple type so a
                            // GC-tuple destructure declares externref and nested
                            // GC-ref locals at their real Wasm kinds.
                            let pat_ty = self.ast_type_of_expr(value);
                            self.bind_pattern_value_types(pattern, pat_ty.as_ref());
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
                    let pat_ty = self.ast_type_of_expr(value);
                    self.bind_pattern_value_types(pattern, pat_ty.as_ref());
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
                let iter_kind = self.kind_of(iter);
                let reference_list = self
                    .ast_type_of_expr(iter)
                    .as_ref()
                    .and_then(|ty| self.gc_reference_list_layout(ty))
                    .is_some();
                self.locals.insert(
                    format!("__forlist_{var}"),
                    if reference_list {
                        iter_kind
                    } else {
                        Kind::I32
                    },
                );
                self.locals.insert(format!("__fori_{var}"), Kind::I32);
                self.locals.insert(format!("__forptr_{var}"), Kind::I32);
                self.locals.insert(format!("__forendptr_{var}"), Kind::I32);
                self.locals.insert(var.clone(), self.iter_elem_kind(iter));
                // Keep the full element type, not only its Wasm kind. A loop
                // variable holding a higher-order function needs its exact
                // parameter/result signature so `mw(h)` selects the typed
                // closure ABI instead of attempting to slot-box `h`.
                if let Some(Type::Named(name, args)) =
                    self.ast_type_of_expr(iter).as_ref().map(Type::unqualified)
                    && name == "List"
                    && let Some(element) = args.first()
                {
                    self.local_types.insert(var.clone(), element.clone());
                    if let Type::Fn(_, ret, _) = element.unqualified() {
                        self.local_fn_ret_kind
                            .insert(var.clone(), self.kind_for_type(ret));
                        let envelope = Self::ownership_envelope_for_type(element);
                        if envelope.has_state() {
                            self.local_fn_ownership.insert(var.clone(), envelope);
                        }
                    }
                }
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
                let scrut_ty = self.ast_type_of_expr(scrutinee);
                for arm in arms {
                    // Pattern-bound vars are i32 (floats aren't stored in records),
                    // except a top-level whole-scrutinee binding (handled below).
                    let mut pvars = Vec::new();
                    collect_pattern_vars(&arm.pattern, &mut pvars);
                    for v in pvars {
                        self.locals.insert(v, Kind::I32);
                    }
                    self.bind_pattern_value_types(&arm.pattern, scrut_ty.as_ref());
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
                        let scrut_shape = self.eq_shape_of(scrutinee);
                        if let (
                            Some((tag, _)),
                            Some(EqShape::AdtInst(_, variant_shapes)),
                        ) = (self.ctors.get(name).copied(), scrut_shape)
                        {
                            if let Some(field_shapes) = variant_shapes.get(tag as usize) {
                                for (sub, shape) in args.iter().zip(field_shapes) {
                                    self.bind_pattern_eq_shape(sub, shape);
                                }
                            }
                        }
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
            Expr::ExistentialCall { receiver, args, .. } => {
                self.infer_locals_expr(receiver);
                for arg in args {
                    self.infer_locals_expr(arg);
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
            Expr::Unary { expr, .. }
            | Expr::As { expr, .. }
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. } => self.infer_locals_expr(expr),
            Expr::Tuple(xs) | Expr::List(xs) => {
                for x in xs {
                    self.infer_locals_expr(x);
                }
            }
            Expr::Field { base, .. } => self.infer_locals_expr(base),
            Expr::Try(inner) => self.infer_locals_expr(inner),
            Expr::RecordUpdate { name: _, base, fields } => {
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
        self.compile_function_as(f, &f.name, None)
    }

    fn compile_function_as(
        &mut self,
        f: &Function,
        emitted_name: &str,
        specialization: Option<&CallableSpecializationKey>,
    ) -> Result<(), CodegenError> {
        self.with_callable_specialization(f, emitted_name, specialization, |codegen| {
            codegen.compile_function_instance(f, emitted_name)
        })
    }

    fn compile_function_instance(
        &mut self,
        f: &Function,
        emitted_name: &str,
    ) -> Result<(), CodegenError> {
        let access_signature = self.access_facts.declaration(&f.name).cloned();
        let mut resolved_params = f.params.clone();
        if let Some(signature) = &access_signature {
            for (param, access) in resolved_params.iter_mut().zip(signature.params()) {
                param.ty = Some(access.ty().clone());
            }
        }
        let resolved_ret = access_signature
            .as_ref()
            .map(|signature| signature.result().ty().clone())
            .or_else(|| f.ret.clone());
        self.locals.clear();
        self.field_caps.clear();
        self.local_records.clear();
        self.local_list_elem.clear();
        self.local_payload_records.clear();
        self.local_val_types.clear();
        self.local_types.clear();
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
        self.local_fn_ownership.clear();
        for p in &resolved_params {
            let k = p.ty.as_ref().map(|t| self.kind_for_type(t)).unwrap_or(Kind::I32);
            self.locals.insert(p.name.clone(), k);
            if let Some(t) = &p.ty {
                self.local_val_types.insert(p.name.clone(), ty_to_valtype(t));
                self.local_types.insert(p.name.clone(), t.clone());
            }
            // A function-typed parameter (`f: fn(...) -> RET`): remember RET's kind
            // so a closure call `f(x)` recovers the result at the right width.
            if let Some(ty) = &p.ty
                && let Type::Fn(_, ret, _) = ty.unqualified()
            {
                self.local_fn_ret_kind.insert(p.name.clone(), self.kind_for_type(ret));
                let envelope = Self::ownership_envelope_for_type(ty);
                if envelope.has_state() {
                    self.local_fn_ownership.insert(p.name.clone(), envelope);
                }
            }
            // A nested-list parameter (`m: List(List(Int))`): record its
            // `(depth, scalar)` so `at(at(m, i), j)` recovers an Int as i64.
            if let Some(n) = p.ty.as_ref().and_then(ty_list_nesting) {
                if n.0 >= 2 {
                    self.local_list_nesting.insert(p.name.clone(), n);
                }
            }
            match p.ty.as_ref().map(Type::unqualified) {
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
            // the declared type (authoritative), so interpolation render / `p == q`
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
        self.cur_fn_has_type_vars = resolved_params.iter().any(|p| {
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
        let access_envelope = access_signature
            .as_ref()
            .map(|signature| self.ownership_envelope_for_named_signature(&f.name, signature))
            .unwrap_or_default();
        self.cur_fn_own_param = self
            .summaries
            .own_abi(&f.name)
            .filter(|index| access_envelope.own_capacity_param == Some(*index))
            .and_then(|i| resolved_params.get(i))
            .map(|p| p.name.clone());
        // Result = the normal return value, then one slot per `var` parameter
        // (moved back out to the caller).
        let ret_kind = match &resolved_ret {
            Some(t) => self.kind_for_type(t),
            None => self.block_kind(renamed),
        };
        self.cur_fn_ret_kind = ret_kind;
        self.cur_fn_ret_ty = resolved_ret.clone().or_else(|| {
            let Stmt::Expr(tail) = renamed.stmts.last()? else {
                return None;
            };
            self.ast_type_of_expr(tail)
        });
        self.cur_fn_unique_ret = access_envelope.unique_capacity_result;
        self.cur_fn_var_params = resolved_params
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                access_signature.as_ref().is_some_and(|signature| {
                    signature.params().get(*index).is_some_and(|param| {
                        param.kind() == witchy_types::access::AccessKind::ExclusiveWriteback
                    })
                })
            })
            .map(|(_, p)| p.name.clone())
            .collect();
        self.cur_fn_var_cap_params = access_envelope
            .var_capacity_params
            .iter()
            .filter_map(|index| resolved_params.get(*index).map(|param| param.name.clone()))
            .collect();
        self.cur_fn_var = !self.cur_fn_var_params.is_empty();

        self.begin_unit(renamed);

        self.apply_level = 0;
        self.existential_call_level = 0;
        self.assign_level = 0;
        self.wm_level = 0;
        self.counter_batch_stack.clear();
        self.counter_batch_used.clear();
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
            // Unit fact stacks are per sequential compile invocation; this name
            // is diagnostic context from the checked declaration, not an
            // emitted-artifact key. Physical instances cannot collide here.
            self.abort_unit(&f.name)?;
            return Err(e);
        }
        let block_kind = self.block_kind(renamed);
        // If the whole body lowered to WIR and the function uses neither the
        // var move-out ABI nor the own-cap ABI (the binary sink models neither
        // yet), keep a `WirFunc` so `compile_module_binary` can encode it.
        let captured_seq = self.captured_seq.take();
        let capture_failed = self.collect_wir && captured_seq.is_none();
        if let Some(seq) = captured_seq {
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
                let mut wf = self.assemble_wir_func(
                    f,
                    &resolved_params,
                    resolved_ret.as_ref(),
                    ret_kind,
                    seq,
                )?;
                wf.name = emitted_name.to_string();
                self.wir_funcs.insert(emitted_name.to_string(), wf);
            }
        }
        // WIR lowering is deliberately all-or-nothing. A `None` after visiting
        // part of the function means the binary sink will report the function
        // as unsupported and the caller may select its fallback backend. The
        // visited loan/uniqueness identities are therefore not a completed
        // compilation and must not be checked as though they were. Successful
        // WIR capture (and the legacy path) retain the strict identity check.
        if capture_failed {
            self.abort_unit(&f.name)?;
        } else {
            // Companions are emitted artifacts and therefore use emitted_name;
            // finish_unit consumes only this invocation's LIFO fact frames and
            // retains the logical name solely for diagnostics.
            self.install_scalar_record_companion(f, emitted_name)?;
            self.finish_unit(&f.name)?;
        }
        self.cur_fn_own_param = None;
        self.cur_fn_var_cap_params.clear();
        self.cur_fn_unique_ret = false;
        Ok(())
    }

    fn scalar_record_companion_name(name: &str) -> String {
        format!("{name}$scalar_result")
    }

    fn install_scalar_record_companion(
        &mut self,
        function: &Function,
        emitted_name: &str,
    ) -> Result<(), CodegenError> {
        use witchy_wir::wir::{WirFunc, WirLocal, WirNode as N, WirTy};
        let Some(producer) = self.scalar_record_producers.get(&function.name).cloned() else {
            return Ok(());
        };
        let [Stmt::Expr(Expr::Ctor { args, .. })] = function.body.stmts.as_slice() else {
            return Ok(());
        };
        if args.len() != producer.field_count {
            return Ok(());
        }
        let mut body = Vec::with_capacity(args.len());
        for field in args {
            let kind = self.kind_of(field);
            let Some(value) = self.lower_expr(field) else {
                return Ok(());
            };
            body.push(N::Push(witchy_wir::wir::WirExpr::ToSlot(
                Box::new(value),
                Self::wir_kind(kind),
            )));
        }
        let params = function
            .params
            .iter()
            .map(|param| WirLocal {
                name: param.name.clone(),
                ty: Self::wir_ty_for_kind(
                    self.locals.get(&param.name).copied().unwrap_or(Kind::I32),
                ),
            })
            .collect();
        let name = Self::scalar_record_companion_name(emitted_name);
        self.layout_wir_funcs.insert(
            name.clone(),
            WirFunc {
                name,
                params,
                ret: vec![WirTy::Int; producer.field_count],
                locals: Vec::new(),
                body,
                raw_body: None,
            },
        );
        Ok(())
    }

    /// Build the `WirFunc` for a fully-lowered function: its params, the body
    /// locals (mirroring `compile_function`'s header — the same `let`s and
    /// scratch slots the WIR body may reference), its single result, and the
    /// captured body. `raw_body: None` — this is a node-walked function.
    fn assemble_wir_func(
        &self,
        f: &Function,
        params: &[Param],
        result: Option<&Type>,
        ret_kind: Kind,
        body: witchy_wir::wir::WirSeq,
    ) -> Result<witchy_wir::wir::WirFunc, CodegenError> {
        use witchy_wir::wir::{WirFunc, WirLocal, WirTy};
        // `.kind()` is all the encoder reads: `Bool` => i32, `Int` => i64.
        let i32t = || WirTy::Bool;
        let i64t = || WirTy::Int;
        let unit_gc_ids = self.unit_gc_ids(
            params.iter().filter_map(|param| param.ty.clone()),
            result.cloned(),
            &f.body,
        );
        let mut params: Vec<WirLocal> = params
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
        for p in &self.cur_fn_var_cap_params {
            params.push(WirLocal { name: format!("{p}__cap"), ty: i32t() });
        }
        if self.fn_destination_layouts.contains_key(&f.name) {
            params.push(WirLocal { name: DESTINATION_PARAM.into(), ty: i32t() });
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
        // RFC-0083 owner roots are explicit i32 rc-region pointers. They are
        // retained after the view-producing binding and released at last use or
        // on every structured return path.
        let mut loan_roots = Vec::new();
        collect_loan_roots(&f.body, &self.loan_facts, &mut loan_roots)?;
        loan_roots.sort_by(|a, b| a.local.cmp(&b.local));
        loan_roots.dedup_by(|a, b| a.local == b.local);
        for root in loan_roots {
            locals.push(WirLocal { name: root.local, ty: i32t() });
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
        let mut scalar_sums: Vec<(&String, &ScalarSumLayout)> =
            self.scalar_sum_active.iter().collect();
        scalar_sums.sort_by(|left, right| left.0.cmp(right.0));
        for (name, layout) in scalar_sums {
            locals.push(WirLocal {
                name: scalar_sum_tag_local(name),
                ty: i32t(),
            });
            for index in 0..layout.max_arity {
                locals.push(WirLocal {
                    name: scalar_sum_payload_local(name, index),
                    ty: i64t(),
                });
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
            if Some(v.as_str()) != self.cur_fn_own_param.as_deref()
                && !self.cur_fn_var_cap_params.contains(v)
            {
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
        locals.push(WirLocal { name: UNIQUE_RESULT_CAP_TMP.into(), ty: i32t() });
        locals.push(WirLocal { name: TUPLE_TMP.into(), ty: i32t() });
        locals.push(WirLocal { name: CALL_RESULT_I32_TMP.into(), ty: i32t() });
        locals.push(WirLocal { name: CALL_RESULT_I64_TMP.into(), ty: i64t() });
        locals.push(WirLocal { name: CALL_RESULT_F64_TMP.into(), ty: WirTy::Float });
        locals.push(WirLocal { name: CALL_RESULT_EXTERN_TMP.into(), ty: WirTy::Extern });
        locals.push(WirLocal { name: TRY_TMP.into(), ty: i32t() });
        locals.push(WirLocal { name: TYPECHECK_TMP.into(), ty: i32t() });
        locals.push(WirLocal { name: MATCH_TMP.into(), ty: i64t() });
        locals.push(WirLocal { name: MATCH_REF_TMP.into(), ty: witchy_wir::wir::WirTy::Extern });
        for &id in &unit_gc_ids {
            locals.push(WirLocal {
                name: call_result_gc_tmp(id),
                ty: witchy_wir::wir::WirTy::GcRef(id),
            });
            locals.push(WirLocal {
                name: match_gc_tmp(id),
                ty: witchy_wir::wir::WirTy::GcRef(id),
            });
            locals.push(WirLocal {
                name: update_gc_tmp(id),
                ty: witchy_wir::wir::WirTy::GcRef(id),
            });
        }
        locals.push(WirLocal { name: MATCH_RES.into(), ty: i64t() });
        for i in 0..SCRUT_POOL {
            locals.push(WirLocal { name: format!("__witchy_scrut_save_{i}"), ty: i64t() });
            locals.push(WirLocal { name: assign_scratch("list", i), ty: i32t() });
            locals.push(WirLocal { name: assign_scratch("index", i), ty: i64t() });
            locals.push(WirLocal { name: assign_scratch("value", i), ty: i64t() });
            for prefix in ["coord", "result", "root", "cap"] {
                locals.push(WirLocal { name: var_scratch(prefix, i, Kind::I32), ty: i32t() });
                locals.push(WirLocal { name: var_scratch(prefix, i, Kind::I64), ty: i64t() });
                locals.push(WirLocal {
                    name: var_scratch(prefix, i, Kind::F64),
                    ty: WirTy::Float,
                });
                locals.push(WirLocal {
                    name: var_scratch(prefix, i, Kind::ExternRef),
                    ty: WirTy::Extern,
                });
                for &id in &unit_gc_ids {
                    locals.push(WirLocal {
                        name: var_scratch(prefix, i, Kind::GcRef(id)),
                        ty: witchy_wir::wir::WirTy::GcRef(id),
                    });
                }
            }
        }
        locals.push(WirLocal { name: SECRET_TMP.into(), ty: witchy_wir::wir::WirTy::Extern });
        locals.push(WirLocal { name: SECRET_NAME_TMP.into(), ty: i32t() });
        locals.push(WirLocal { name: ABORT_STR_TMP.into(), ty: i32t() });
        // Scratch slots for the inlined in-place `set_at` fast path (index i32,
        // value i64): the common in-bounds + owned case stores directly without a
        // `$list_set_cap` call; the helper is only invoked for OOB / re-own.
        locals.push(WirLocal { name: "__witchy_set_idx".into(), ty: i32t() });
        locals.push(WirLocal { name: "__witchy_set_val".into(), ty: i64t() });
        // (RFC-0016) RC-floor free-at-overwrite scratch: the freshly-allocated
        // buffer (a heap pointer) before the old one is freed and the var rebound.
        locals.push(WirLocal { name: "__rc_new".into(), ty: i32t() });
        locals.push(WirLocal {
            name: DESTINATION_RESULT_TMP.into(),
            ty: i32t(),
        });
        for i in 0..WM_POOL {
            locals.push(WirLocal { name: format!("__witchy_wm_{i}"), ty: i32t() });
            locals.push(WirLocal {
                name: Self::counter_batch_local("destination", i),
                ty: i64t(),
            });
            locals.push(WirLocal {
                name: Self::counter_batch_local("rewind", i),
                ty: i64t(),
            });
        }
        for i in 0..APPLY_POOL {
            locals.push(WirLocal {
                name: format!("__witchy_call_{i}"),
                ty: WirTy::GcRef(CLOSURE_WRAPPER_ID),
            });
        }
        for i in 0..EXISTENTIAL_CALL_POOL {
            locals.push(WirLocal {
                name: existential_call_scratch(i),
                ty: WirTy::GcRef(EXISTENTIAL_WRAPPER_ID),
            });
        }
        for (id, element_kind) in self
            .gc_reference_list_layouts()
            .into_iter()
            .filter(|(id, _)| unit_gc_ids.contains(id))
        {
            for level in 0..APPLY_POOL {
                locals.push(WirLocal {
                    name: gc_list_scratch(GC_LIST_SRC_TMP, level, id),
                    ty: WirTy::GcRef(id),
                });
                locals.push(WirLocal {
                    name: gc_list_scratch(GC_LIST_RIGHT_TMP, level, id),
                    ty: WirTy::GcRef(id),
                });
                locals.push(WirLocal {
                    name: gc_list_scratch(GC_LIST_DST_TMP, level, id),
                    ty: WirTy::GcRef(id),
                });
                locals.push(WirLocal {
                    name: gc_list_scratch(GC_LIST_VALUE_TMP, level, id),
                    ty: Self::wir_ty_for_kind(element_kind),
                });
                locals.push(WirLocal {
                    name: gc_list_scratch(GC_LIST_LEN_TMP, level, id),
                    ty: i32t(),
                });
                locals.push(WirLocal {
                    name: gc_list_scratch(GC_LIST_LEFT_LEN_TMP, level, id),
                    ty: i32t(),
                });
                locals.push(WirLocal {
                    name: gc_list_scratch(GC_LIST_INDEX_TMP, level, id),
                    ty: i32t(),
                });
                locals.push(WirLocal {
                    name: gc_list_scratch(GC_LIST_TARGET_TMP, level, id),
                    ty: i32t(),
                });
                locals.push(WirLocal {
                    name: gc_list_scratch(GC_LIST_RAW_INDEX_TMP, level, id),
                    ty: i64t(),
                });
            }
        }
        for i in 0..REUSE_POOL {
            locals.push(WirLocal { name: format!("__witchy_reuse_{i}"), ty: i64t() });
        }
        let mut destination_scratches: Vec<(String, LayoutId)> = self
            .destination_scratch_sites
            .values()
            .cloned()
            .collect();
        destination_scratches.sort_by(|left, right| left.0.cmp(&right.0));
        destination_scratches.dedup();
        for (name, _) in &destination_scratches {
            locals.push(WirLocal {
                name: name.clone(),
                ty: i32t(),
            });
        }
        // An `var` function returns its declared value FOLLOWED BY one result per
        // var param (the multi-value move-out ABI, mirroring `var_epilogue` on the
        // WAT path): after the declared tail, push each var param's final value in
        // declaration order. The call site (`CallStoreMulti`) pops them back into the
        // caller's variables.
        let mut ret = vec![Self::wir_ty_for_kind(ret_kind)];
        let mut body = body;
        if !destination_scratches.is_empty() {
            let mut initialized = destination_scratches
                .into_iter()
                .map(|(local, id)| witchy_wir::wir::WirNode::SetLocal {
                    local,
                    value: witchy_wir::wir::WirExpr::Call {
                        func: Self::layout_helper_name("destination_scratch", id, None),
                        args: Vec::new(),
                    },
                })
                .collect::<witchy_wir::wir::WirSeq>();
            initialized.append(&mut body);
            body = initialized;
        }
        if self.cur_fn_unique_ret {
            ret.push(i32t());
            let cap = f
                .body
                .stmts
                .last()
                .and_then(|stmt| match stmt {
                    Stmt::Expr(expr) => Some(self.return_capacity_expr(expr)),
                    _ => None,
                })
                .unwrap_or(witchy_wir::wir::WirExpr::ConstI32(0));
            body.push(witchy_wir::wir::WirNode::Push(cap));
        }
        for name in &self.cur_fn_var_params {
            let k = self.locals.get(name).copied().unwrap_or(Kind::I32);
            ret.push(Self::wir_ty_for_kind(k));
            body.push(witchy_wir::wir::WirNode::Push(witchy_wir::wir::WirExpr::GetLocal(name.clone())));
        }
        for name in &self.cur_fn_var_cap_params {
            ret.push(i32t());
            body.push(witchy_wir::wir::WirNode::Push(
                witchy_wir::wir::WirExpr::GetLocal(format!("{name}__cap")),
            ));
        }
        // own-ABI: append the returned buffer's ownership token (one i32 result).
        // It is `$p__cap` whenever the function returns its owned parameter;
        // mutation is not required to preserve ownership. A forwarded self call
        // uses the cap returned by that call; every other result returns zero.
        if let Some(p) = self.cur_fn_own_param.clone() {
            ret.push(i32t());
            let returns_own = match f.body.stmts.last() {
                Some(Stmt::Expr(Expr::Var(v))) => *v == p,
                Some(Stmt::Expr(Expr::Unary { op: UnOp::Move, expr })) => {
                    matches!(expr.as_ref(), Expr::Var(v) if *v == p)
                }
                _ => false,
            };
            let forwards_own = matches!(f.body.stmts.last(), Some(Stmt::Expr(Expr::Call { name, args }))
                if name == &f.name
                    && self.summaries.own_abi(name).and_then(|index| args.get(index)).is_some_and(|arg|
                        matches!(arg, Expr::Var(v) if v == &p)
                            || matches!(arg, Expr::Unary { op: UnOp::Move, expr }
                                if matches!(expr.as_ref(), Expr::Var(v) if v == &p))));
            let cap = if returns_own {
                witchy_wir::wir::WirExpr::GetLocal(format!("{p}__cap"))
            } else if forwards_own {
                witchy_wir::wir::WirExpr::GetLocal("__witchy_owncap".to_string())
            } else {
                witchy_wir::wir::WirExpr::ConstI32(0)
            };
            body.push(witchy_wir::wir::WirNode::Push(cap));
        }
        Ok(WirFunc {
            name: f.name.clone(),
            params,
            ret,
            locals,
            body,
            raw_body: None,
        })
    }

    fn return_capacity_expr(&self, expr: &Expr) -> witchy_wir::wir::WirExpr {
        use witchy_wir::wir::WirExpr as W;
        match expr {
            Expr::List(items) => W::ConstI32(items.len() as i32),
            Expr::Var(name) if self.cur_fn_own_param.as_deref() == Some(name) => {
                W::GetLocal(format!("{name}__cap"))
            }
            Expr::Var(name) if self.inplace_push.contains(name) => {
                W::GetLocal(format!("{name}__cap"))
            }
            expression if self.expression_returns_unique_capacity(expression) => {
                W::GetLocal(UNIQUE_RESULT_CAP_TMP.to_string())
            }
            _ => W::ConstI32(0),
        }
    }

    /// Begin a compile unit (function/lambda body): run the
    /// uniqueness analysis and install its facts.
    fn begin_unit(&mut self, body: &Block) {
        let mut expected_loan_keys = HashSet::new();
        collect_loan_event_keys(body, &self.loan_facts, &mut expected_loan_keys);
        self.loan_fact_stack.push((expected_loan_keys, HashSet::new()));
        let mut facts = if force_copy_mode() {
            analysis::Facts::default()
        } else {
            analysis::analyze_with_access(
                body,
                &self.summaries,
                self.checked_module,
                &self.access_facts,
            )
        };
        if !force_copy_mode() {
            facts.merge_loan_kills(body, &self.loan_facts);
        }
        self.inplace_push = facts
            .accumulators
            .iter()
            .cloned()
            .collect();
        if !force_copy_mode() {
            self.inplace_push.extend(self.cur_fn_var_cap_params.iter().cloned());
        }
        self.install_direct_list_builder_plans(body);
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
        let nested_var_roots = nested_var_place_roots(body, &self.fn_conventions);
        self.sroa_candidates.retain(|name| !nested_var_roots.contains(name));
        self.scalar_record_call_candidates = if !force_copy_mode()
            && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::Sroa)
        {
            scalar_record_call_candidates_block(
                body,
                &self.scalar_record_producers,
                &self.local_types,
                &self.specialized_type_ids,
            )
        } else {
            HashMap::new()
        };
        self.scalar_record_call_candidates
            .retain(|name, _| !nested_var_roots.contains(name));
        self.scalar_sum_active.clear();
        self.scalar_sum_fused_values.clear();
        self.scalar_sum_candidates = if !force_copy_mode()
            && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::Sroa)
        {
            crate::escape::confined_match_sum_candidates_block(body)
        } else {
            HashSet::new()
        };
        self.scalar_sum_candidates
            .retain(|name| !nested_var_roots.contains(name));
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
        // Declared-packed values now use their canonical descriptor across
        // direct boundaries. Keep the predecessor's name-based path only for
        // best-effort inferred unboxing; otherwise it would construct the old
        // `[len][i64 slots...]` layout while descriptor-based consumers expect
        // `[len][capacity][stride elements...]`.
        self.packed_candidates = if unbox_on {
            let declared: HashSet<String> = crate::escape::record_list_lets_block(body)
                .into_iter()
                .filter_map(|(name, ctor)| {
                    let ty = self.ctor_type_name.get(&ctor).cloned().unwrap_or(ctor);
                    self.packed_types.contains(&ty).then_some(name)
                })
                .collect();
            confined_packed
                .into_iter()
                .filter(|name| !declared.contains(name))
                .collect()
        } else {
            HashSet::new()
        };
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
        self.destination_forward_vars = if !force_copy_mode() {
            crate::escape::confined_reassigned_vars_block(body, &self.summaries)
        } else {
            HashSet::new()
        };
        self.destination_scratch_sites.clear();
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

    /// Recognize only the closed builder shape
    /// `var xs = []; for i in LO..<HI>: list.push(xs, scalar)` where the binding
    /// and loop are adjacent, bounds are non-empty exclusive integer literals,
    /// and uniqueness already proved `xs` has no observable alias. Two AST-keyed
    /// maps keep the reservation and direct-store privileges pinned to those exact
    /// statements; a later loop over the same local is never conflated with it.
    fn install_direct_list_builder_plans(&mut self, body: &Block) {
        self.direct_list_builder_lets.clear();
        self.direct_list_builder_loops.clear();
        self.active_direct_list_builder = None;
        for pair in body.stmts.windows(2) {
            let Stmt::Let {
                name,
                value: Expr::List(items),
                ..
            } = &pair[0]
            else {
                continue;
            };
            if !items.is_empty() || !self.inplace_push.contains(name) {
                continue;
            }
            let Stmt::Expr(loop_expr @ Expr::For { var, iter, body: loop_body }) = &pair[1]
            else {
                continue;
            };
            let Expr::Range {
                lo,
                hi,
                inclusive: false,
            } = iter.as_ref()
            else {
                continue;
            };
            let (Expr::Int(lower), Expr::Int(upper)) = (lo.as_ref(), hi.as_ref()) else {
                continue;
            };
            let Some(length) = upper.checked_sub(*lower) else {
                continue;
            };
            if length <= 0 {
                continue;
            }
            let push = match loop_body.stmts.as_slice() {
                [Stmt::Expr(push)] => push,
                [Stmt::Assign { name: target, value }] if target == name => value,
                _ => continue,
            };
            let Expr::Call { args, .. } = push else {
                continue;
            };
            if args.len() != 2
                || !matches!(
                    analysis::self_inplace_op(name, push),
                    Some(analysis::InPlaceOp::Push(_))
                )
                || expr_reads_var(&args[1], name)
            {
                continue;
            }
            if self.locals.get(name) != Some(&Kind::I32) {
                continue;
            }
            let specialized = self
                .local_types
                .get(name)
                .and_then(|list_type| self.specialized_layout_id(list_type));
            let shape = if let Some(list_id) = specialized {
                let Some(list_layout) = self.specialized_layouts.get(list_id) else {
                    continue;
                };
                let (
                    HeaderLayout::PackedList { rc, data_offset, .. },
                    LayoutSize::Dynamic { stride, .. },
                    LayoutKind::PackedList { element, .. },
                ) = (list_layout.header(), list_layout.size(), list_layout.kind())
                else {
                    continue;
                };
                let Expr::Ctor { args: fields, .. } = &args[1] else {
                    continue;
                };
                let Some(element_layout) = self.specialized_layouts.get(*element) else {
                    continue;
                };
                let LayoutKind::PackedRecord { fields: layouts } = element_layout.kind() else {
                    continue;
                };
                if self.specialized_layout_of_expr(&args[1]) != Some(*element)
                    || fields.len() != layouts.len()
                    || fields.iter().any(|field| self.kind_of(field).is_ref())
                {
                    continue;
                }
                (
                    data_offset as i32,
                    stride as i32,
                    element_layout
                        .fields()
                        .iter()
                        .map(|field| field.offset())
                        .collect(),
                    rc,
                )
            } else {
                if self.kind_of(&args[1]).is_ref() {
                    continue;
                }
                (4, 8, Vec::new(), RcHeader::Required)
            };
            // `$rc_alloc` records its byte size in a 24-bit header. Keep the
            // exact reservation within that representable range.
            if length > ((0x00ff_ffff_i64 - i64::from(shape.0)) / i64::from(shape.1)) {
                continue;
            }
            let plan = DirectListBuilderPlan {
                list: name.clone(),
                counter: format!("__forctr_{var}"),
                lower: *lower,
                capacity: length as i32,
                data_offset: shape.0,
                stride: shape.1,
                packed_field_offsets: shape.2,
                rc_header: shape.3,
            };
            self.direct_list_builder_lets
                .insert((&pair[0] as *const Stmt) as usize, plan.clone());
            self.direct_list_builder_loops
                .insert((loop_expr as *const Expr) as usize, plan);
        }
    }

    /// End a compile unit, asserting every analysis entry was consumed — a
    /// cloned-subtree bug (compiling different AST nodes than were analyzed)
    /// surfaces here as a loud error, never as a lost cap kill.
    fn finish_unit(&mut self, unit: &str) -> Result<(), CodegenError> {
        self.drop_facts_stack.pop();
        let Some((expected_loans, seen_loans)) = self.loan_fact_stack.pop() else {
            return cerr(format!("internal: unbalanced loan-fact unit in `{unit}`"));
        };
        if expected_loans != seen_loans {
            return cerr(format!(
                "internal: loan facts for `{unit}` were not fully consumed \
                 ({}/{} statement identities) — a compiled subtree was not the checked AST instance",
                seen_loans.len(),
                expected_loans.len(),
            ));
        }
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

    /// Balance a compile unit whose whole-unit WIR capture failed.
    ///
    /// Partial WIR walks may consume only a prefix of the checked facts before
    /// discovering a valid construct that this sink does not support. Those
    /// facts were not used to emit a function, so only the stack balance is
    /// required here; [`Self::finish_unit`] remains the strict path for every
    /// function or lambda that was actually captured.
    fn abort_unit(&mut self, unit: &str) -> Result<(), CodegenError> {
        if self.drop_facts_stack.pop().is_none() {
            return cerr(format!("internal: unbalanced drop-fact unit in `{unit}`"));
        }
        if self.loan_fact_stack.pop().is_none() {
            return cerr(format!("internal: unbalanced loan-fact unit in `{unit}`"));
        }
        if self.facts_stack.pop().is_none() {
            return cerr(format!("internal: unbalanced analysis unit in `{unit}`"));
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

    fn loan_root(
        event: &witchy_types::loans::LoanEvent,
    ) -> Result<Option<LoanRoot>, CodegenError> {
        let owner = event.owner_root();
        let owner_type = owner.direct_storage_type.as_ref().ok_or_else(|| CodegenError {
            message: format!(
                "internal: loan root `{}` for view `{}` has no exact checked root-local type",
                owner.local, event.view
            ),
        })?;
        let bias = rc_leaf_bias(owner_type).ok_or_else(|| CodegenError {
            message: format!(
                "internal: loan root `{}` for view `{}` has unresolved checked type `{:?}`",
                owner.local, event.view, owner_type
            ),
        })?;
        if bias < 0 {
            return Ok(None);
        }
        Ok(Some(LoanRoot {
            local: format!("__loan_root_{}__{}", event.view, owner.local),
            value: owner.local,
            bias,
        }))
    }

    fn checked_loan_roots(
        &mut self,
        events: &[witchy_types::loans::LoanEvent],
    ) -> Vec<LoanRoot> {
        let mut roots = Vec::new();
        for event in events {
            match Self::loan_root(event) {
                Ok(Some(root)) => roots.push(root),
                Ok(None) => {}
                Err(error) => {
                    self.reject_reason.get_or_insert(error);
                }
            }
        }
        roots
    }

    fn loan_region(root: &LoanRoot) -> witchy_wir::wir::WirExpr {
        use witchy_wir::wir::{BinOp, Kind as K, WirExpr as W};
        if root.bias == 0 {
            W::GetLocal(root.value.clone())
        } else {
            W::Binary {
                op: BinOp::Sub,
                kind: K::I32,
                lhs: Box::new(W::GetLocal(root.value.clone())),
                rhs: Box::new(W::ConstI32(root.bias)),
            }
        }
    }

    fn open_loan_nodes(
        &mut self,
        events: &[witchy_types::loans::LoanEvent],
    ) -> witchy_wir::wir::WirSeq {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for root in self.checked_loan_roots(events) {
            if !seen.insert(root.local.clone()) {
                continue;
            }
            let region = Self::loan_region(&root);
            out.push(N::SetLocal {
                local: root.local,
                value: W::Call { func: "rc_dup".into(), args: vec![region] },
            });
        }
        out
    }

    fn close_loan_nodes(
        &mut self,
        events: &[witchy_types::loans::LoanEvent],
    ) -> witchy_wir::wir::WirSeq {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for root in self.checked_loan_roots(events) {
            if !seen.insert(root.local.clone()) {
                continue;
            }
            out.push(N::Do(W::Call {
                func: "rc_drop".into(),
                args: vec![W::GetLocal(root.local.clone())],
            }));
            // Zeroing makes cleanup idempotent across a statement's normal close
            // and any enclosing structured-return path.
            out.push(N::SetLocal { local: root.local, value: W::ConstI32(0) });
        }
        out
    }

    /// Map codegen's `Kind` to the exact WIR kind, preserving reference types.
    fn wir_kind(k: Kind) -> witchy_wir::wir::Kind {
        match k {
            Kind::I32 => witchy_wir::wir::Kind::I32,
            Kind::I64 => witchy_wir::wir::Kind::I64,
            Kind::F64 => witchy_wir::wir::Kind::F64,
            Kind::ExternRef => witchy_wir::wir::Kind::ExternRef,
            Kind::GcRef(id) => witchy_wir::wir::Kind::GcRef(id),
        }
    }

    /// A `WirTy` whose `.kind()` is `k` — used for a control node's `result`
    /// block-type, where only the wasm kind matters (`i64`/`f64`/`i32`).
    fn wir_ty_for_kind(k: Kind) -> witchy_wir::wir::WirTy {
        match k {
            Kind::I64 => witchy_wir::wir::WirTy::Int,
            Kind::F64 => witchy_wir::wir::WirTy::Float,
            Kind::I32 => witchy_wir::wir::WirTy::Bool,
            Kind::ExternRef => witchy_wir::wir::WirTy::Extern,
            Kind::GcRef(id) => witchy_wir::wir::WirTy::GcRef(id),
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

    fn specialized_layout_id(&self, ty: &Type) -> Option<LayoutId> {
        self.current_specialized_type_ids
            .iter()
            .chain(&self.specialized_type_ids)
            .find(|(known, _)| known.unqualified() == ty.unqualified())
            .map(|(_, id)| *id)
    }

    fn local_has_elided_rc_header(&self, name: &str) -> bool {
        let Some(descriptor) = self
            .local_types
            .get(name)
            .and_then(|ty| self.specialized_layout_id(ty))
            .and_then(|id| self.specialized_layouts.get(id))
        else {
            return false;
        };
        matches!(descriptor.header(), HeaderLayout::PackedList { rc: RcHeader::Elided, .. })
    }

    fn specialized_layout_of_expr(&self, expr: &Expr) -> Option<LayoutId> {
        let ty = self.ast_type_of_expr(expr)?;
        self.specialized_layout_id(&ty)
    }

    fn specialized_boundary_result_layout(&self, expr: &Expr) -> Option<LayoutId> {
        self.specialized_layout_of_expr(expr)
            .or_else(|| {
                self.call_access_signature(expr)
                    .and_then(|signature| self.specialized_layout_id(signature.result().ty()))
            })
            .or_else(|| match expr {
                Expr::Apply { func, .. } => self
                    .closure_result_type(func)
                    .and_then(|ty| self.specialized_layout_id(&ty)),
                _ => None,
            })
    }

    fn scalar_layout_kind(kind: ScalarKind) -> Kind {
        match kind {
            ScalarKind::Int | ScalarKind::Duration => Kind::I64,
            ScalarKind::Float => Kind::F64,
            ScalarKind::Bool | ScalarKind::U32 | ScalarKind::Tag8
            | ScalarKind::Tag16 | ScalarKind::Tag32 => Kind::I32,
        }
    }

    fn layout_field_kind(&self, kind: FieldKind) -> Option<Kind> {
        match kind {
            FieldKind::Scalar(scalar) => Some(Self::scalar_layout_kind(scalar)),
            FieldKind::Inline(id) => match self.specialized_layouts.get(id)?.size() {
                LayoutSize::Fixed(_) => Some(Kind::I32),
                LayoutSize::Dynamic { .. } => None,
            },
        }
    }

    fn layout_helper_name(prefix: &str, id: LayoutId, count: Option<usize>) -> String {
        match count {
            Some(count) => format!("__witchy_{prefix}_{}_n{count}", id.to_hex()),
            None => format!("__witchy_{prefix}_{}", id.to_hex()),
        }
    }

    fn layout_alloc_nodes(
        &self,
        size: u32,
    ) -> (Vec<witchy_wir::wir::WirNode>, witchy_wir::wir::WirExpr) {
        self.layout_alloc_expr_nodes_into_with_header(
            witchy_wir::wir::WirExpr::ConstI32(size as i32),
            "p",
            None,
        )
    }

    fn layout_alloc_nodes_with_header(
        &self,
        size: u32,
        rc: RcHeader,
    ) -> (Vec<witchy_wir::wir::WirNode>, witchy_wir::wir::WirExpr) {
        self.layout_alloc_expr_nodes_into_with_header(
            witchy_wir::wir::WirExpr::ConstI32(size as i32),
            "p",
            Some(rc),
        )
    }

    fn layout_alloc_expr_nodes_with_header(
        &self,
        size: witchy_wir::wir::WirExpr,
        rc: RcHeader,
    ) -> (Vec<witchy_wir::wir::WirNode>, witchy_wir::wir::WirExpr) {
        self.layout_alloc_expr_nodes_into_with_header(size, "p", Some(rc))
    }

    fn layout_alloc_expr_nodes_into(
        &self,
        size: witchy_wir::wir::WirExpr,
        local: &str,
    ) -> (Vec<witchy_wir::wir::WirNode>, witchy_wir::wir::WirExpr) {
        self.layout_alloc_expr_nodes_into_with_header(size, local, None)
    }

    fn layout_alloc_expr_nodes_into_with_header(
        &self,
        size: witchy_wir::wir::WirExpr,
        local: &str,
        rc: Option<RcHeader>,
    ) -> (Vec<witchy_wir::wir::WirNode>, witchy_wir::wir::WirExpr) {
        use witchy_wir::wir::{BinOp as WB, Kind as WK, WirExpr as W, WirNode as N};
        let checked = witchy_wir::wir_helpers::heap_check_enabled();
        let reserve = if checked {
            W::Binary {
                op: WB::Add,
                kind: WK::I32,
                lhs: Box::new(size.clone()),
                rhs: Box::new(W::ConstI32(witchy_wir::layout::HEAP_REDZONE as i32)),
            }
        } else {
            size.clone()
        };
        let mut nodes = vec![N::SetLocal {
            local: local.into(),
            value: W::Call {
                func: match rc {
                    Some(RcHeader::Elided) => "bump_alloc".into(),
                    Some(RcHeader::Required) | None => "rc_alloc".into(),
                },
                args: vec![reserve],
            },
        }];
        if let Some(rc) = rc {
            nodes.push(Self::increment_counter(match rc {
                RcHeader::Required => "__witchy_rc_headers_emitted",
                RcHeader::Elided => "__witchy_rc_headers_elided",
            }));
        }
        nodes.push(Self::increment_counter("__witchy_packed_alloc_calls"));
        nodes.push(N::SetGlobal {
            global: "__witchy_packed_alloc_bytes".into(),
            value: W::Binary {
                op: WB::Add,
                kind: WK::I64,
                lhs: Box::new(W::GetGlobal("__witchy_packed_alloc_bytes".into())),
                // Count descriptor payload bytes. Debug redzones are deliberately
                // excluded so instrumentation does not change the physical metric.
                rhs: Box::new(W::Convert {
                    from: WK::I32,
                    to: WK::I64,
                    arg: Box::new(size.clone()),
                }),
            },
        });
        if checked {
            nodes.push(N::Do(W::CallHost {
                import: "heap_register".into(),
                args: vec![
                    W::GetLocal(local.into()),
                    W::Binary {
                        op: WB::Add,
                        kind: WK::I32,
                        lhs: Box::new(W::GetLocal(local.into())),
                        rhs: Box::new(size),
                    },
                ],
            }));
        }
        (nodes, W::GetLocal(local.into()))
    }

    fn push_layout_store(
        &self,
        nodes: &mut Vec<witchy_wir::wir::WirNode>,
        base: witchy_wir::wir::WirExpr,
        field: witchy_wir::layout::LayoutField,
        value: witchy_wir::wir::WirExpr,
    ) -> Option<()> {
        use witchy_wir::wir::{Kind as WK, WirExpr as W, WirNode as N};
        match field.kind() {
            FieldKind::Scalar(ScalarKind::Bool | ScalarKind::Tag8) => {
                nodes.push(N::Store8 { ptr: base, value, offset: field.offset() });
            }
            FieldKind::Scalar(ScalarKind::Tag16) => {
                nodes.push(N::Store8 {
                    ptr: base.clone(),
                    value: value.clone(),
                    offset: field.offset(),
                });
                nodes.push(N::Store8 {
                    ptr: base,
                    value: W::Binary {
                        op: witchy_wir::wir::BinOp::ShrU,
                        kind: WK::I32,
                        lhs: Box::new(value),
                        rhs: Box::new(W::ConstI32(8)),
                    },
                    offset: field.offset().checked_add(1)?,
                });
            }
            FieldKind::Scalar(scalar) => {
                nodes.push(N::Store {
                    ptr: base,
                    value,
                    kind: match scalar {
                        ScalarKind::Int | ScalarKind::Duration => WK::I64,
                        ScalarKind::Float => WK::F64,
                        ScalarKind::U32 | ScalarKind::Tag32 => WK::I32,
                        ScalarKind::Bool | ScalarKind::Tag8 | ScalarKind::Tag16 => unreachable!(),
                    },
                    offset: field.offset(),
                });
            }
            FieldKind::Inline(child) => {
                let LayoutSize::Fixed(size) = self.specialized_layouts.get(child)?.size() else {
                    return None;
                };
                nodes.push(N::MemoryCopy {
                    dest: W::Binary {
                        op: witchy_wir::wir::BinOp::Add,
                        kind: WK::I32,
                        lhs: Box::new(base),
                        rhs: Box::new(W::ConstI32(field.offset() as i32)),
                    },
                    src: value,
                    len: W::ConstI32(size as i32),
                });
            }
        }
        Some(())
    }

    fn ensure_packed_record_helper(&mut self, id: LayoutId) -> Option<String> {
        use witchy_wir::wir::{WirFunc, WirLocal, WirNode as N, WirTy};
        let name = Self::layout_helper_name("packed_record", id, None);
        if self.layout_wir_funcs.contains_key(&name) {
            return Some(name);
        }
        let descriptor = self.specialized_layouts.get(id)?.clone();
        if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }) {
            return None;
        }
        let LayoutSize::Fixed(size) = descriptor.size() else { return None };
        let mut params = Vec::with_capacity(descriptor.fields().len());
        for (index, field) in descriptor.fields().iter().enumerate() {
            let kind = self.layout_field_kind(field.kind())?;
            params.push(WirLocal {
                name: format!("f{index}"),
                ty: Self::wir_ty_for_kind(kind),
            });
        }
        let (mut body, base) = self.layout_alloc_nodes(size);
        for (index, field) in descriptor.fields().iter().copied().enumerate() {
            self.push_layout_store(
                &mut body,
                base.clone(),
                field,
                witchy_wir::wir::WirExpr::GetLocal(format!("f{index}")),
            )?;
        }
        body.push(N::Push(base));
        self.layout_wir_funcs.insert(name.clone(), WirFunc {
            name: name.clone(),
            params,
            ret: vec![WirTy::Bool],
            locals: vec![WirLocal { name: "p".into(), ty: WirTy::Bool }],
            body,
            raw_body: None,
        });
        Some(name)
    }

    fn ensure_packed_sum_ctor_helper(
        &mut self,
        id: LayoutId,
        tag: usize,
    ) -> Option<String> {
        use witchy_wir::wir::{WirExpr as W, WirFunc, WirLocal, WirNode as N, WirTy};
        let name = Self::layout_helper_name("packed_sum", id, Some(tag));
        if self.layout_wir_funcs.contains_key(&name) {
            return Some(name);
        }
        let descriptor = self.specialized_layouts.get(id)?.clone();
        let LayoutKind::ClosedSum { variants } = descriptor.kind() else { return None };
        let variant = descriptor.variant_layouts().get(tag)?.clone();
        if variants.get(tag)?.len() != variant.fields().len() {
            return None;
        }
        let LayoutSize::Fixed(size) = descriptor.size() else { return None };
        let tag_field = *descriptor.fields().first()?;
        let mut params = Vec::with_capacity(variant.fields().len());
        for (index, field) in variant.fields().iter().enumerate() {
            params.push(WirLocal {
                name: format!("f{index}"),
                ty: Self::wir_ty_for_kind(self.layout_field_kind(field.kind())?),
            });
        }
        let (mut body, base) = self.layout_alloc_nodes(size);
        self.push_layout_store(&mut body, base.clone(), tag_field, W::ConstI32(tag as i32))?;
        for (index, field) in variant.fields().iter().copied().enumerate() {
            self.push_layout_store(
                &mut body,
                base.clone(),
                field,
                W::GetLocal(format!("f{index}")),
            )?;
        }
        body.push(N::Push(base));
        self.layout_wir_funcs.insert(name.clone(), WirFunc {
            name: name.clone(),
            params,
            ret: vec![WirTy::Bool],
            locals: vec![WirLocal { name: "p".into(), ty: WirTy::Bool }],
            body,
            raw_body: None,
        });
        Some(name)
    }

    fn ensure_layout_destination_scratch_helper(&mut self, id: LayoutId) -> Option<String> {
        use witchy_wir::wir::{WirFunc, WirLocal, WirNode as N, WirTy};
        let name = Self::layout_helper_name("destination_scratch", id, None);
        if self.layout_wir_funcs.contains_key(&name) {
            return Some(name);
        }
        let descriptor = self.specialized_layouts.get(id)?;
        let LayoutSize::Fixed(size) = descriptor.size() else {
            return None;
        };
        let (mut body, base) = self.layout_alloc_nodes(size);
        body.push(N::Push(base));
        self.layout_wir_funcs.insert(
            name.clone(),
            WirFunc {
                name: name.clone(),
                params: Vec::new(),
                ret: vec![WirTy::Bool],
                locals: vec![WirLocal {
                    name: "p".into(),
                    ty: WirTy::Bool,
                }],
                body,
                raw_body: None,
            },
        );
        Some(name)
    }

    fn ensure_packed_list_push_helper(&mut self, id: LayoutId) -> Option<String> {
        use witchy_wir::wir::{BinOp as WB, Kind as WK, WirExpr as W, WirFunc, WirLocal, WirNode as N, WirTy};
        let name = Self::layout_helper_name("packed_list_push", id, None);
        if self.layout_wir_funcs.contains_key(&name) {
            return Some(name);
        }
        let descriptor = self.specialized_layouts.get(id)?.clone();
        let LayoutKind::PackedList { element, .. } = descriptor.kind() else { return None };
        let element_descriptor = self.specialized_layouts.get(*element)?.clone();
        let HeaderLayout::PackedList {
            rc,
            length_offset,
            capacity_offset,
            data_offset,
            ..
        } = descriptor.header() else { return None };
        let LayoutSize::Dynamic { base, stride } = descriptor.size() else { return None };
        let fields = element_descriptor.fields();
        let mut params = vec![
            WirLocal { name: "root".into(), ty: WirTy::Bool },
            WirLocal { name: "cap".into(), ty: WirTy::Bool },
        ];
        for (index, field) in fields.iter().enumerate() {
            params.push(WirLocal {
                name: format!("f{index}"),
                ty: Self::wir_ty_for_kind(self.layout_field_kind(field.kind())?),
            });
        }
        let binary = |op, lhs, rhs| W::Binary {
            op,
            kind: WK::I32,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        let element_base = |root: W| {
            binary(
                WB::Add,
                binary(WB::Add, root, W::ConstI32(data_offset as i32)),
                binary(
                    WB::Mul,
                    W::GetLocal("len".into()),
                    W::ConstI32(stride as i32),
                ),
            )
        };
        let mut hot = Vec::new();
        for (index, field) in fields.iter().copied().enumerate() {
            self.push_layout_store(
                &mut hot,
                element_base(W::GetLocal("root".into())),
                field,
                W::GetLocal(format!("f{index}")),
            )?;
        }
        hot.push(N::Store {
            ptr: W::GetLocal("root".into()),
            value: binary(WB::Add, W::GetLocal("len".into()), W::ConstI32(1)),
            kind: WK::I32,
            offset: length_offset,
        });
        hot.push(N::SetLocal {
            local: "out_root".into(),
            value: W::GetLocal("root".into()),
        });
        hot.push(N::SetLocal {
            local: "out_cap".into(),
            value: W::GetLocal("cap".into()),
        });

        let mut cold = vec![
            N::SetLocal {
                local: "new_cap".into(),
                value: binary(
                    WB::Mul,
                    binary(WB::Add, W::GetLocal("len".into()), W::ConstI32(1)),
                    W::ConstI32(2),
                ),
            },
            N::SetLocal {
                local: "logical_size".into(),
                value: binary(
                    WB::Add,
                    W::ConstI32(base as i32),
                    binary(
                        WB::Mul,
                        W::GetLocal("new_cap".into()),
                        W::ConstI32(stride as i32),
                    ),
                ),
            },
        ];
        let (allocation, new_root) = self.layout_alloc_expr_nodes_with_header(
            W::GetLocal("logical_size".into()),
            rc,
        );
        cold.extend(allocation);
        cold.push(N::SetLocal {
            local: "new_root".into(),
            value: new_root,
        });
        cold.push(N::MemoryCopy {
            dest: binary(
                WB::Add,
                W::GetLocal("new_root".into()),
                W::ConstI32(data_offset as i32),
            ),
            src: binary(
                WB::Add,
                W::GetLocal("root".into()),
                W::ConstI32(data_offset as i32),
            ),
            len: binary(
                WB::Mul,
                W::GetLocal("len".into()),
                W::ConstI32(stride as i32),
            ),
        });
        for (index, field) in fields.iter().copied().enumerate() {
            self.push_layout_store(
                &mut cold,
                element_base(W::GetLocal("new_root".into())),
                field,
                W::GetLocal(format!("f{index}")),
            )?;
        }
        cold.push(N::Store {
            ptr: W::GetLocal("new_root".into()),
            value: binary(WB::Add, W::GetLocal("len".into()), W::ConstI32(1)),
            kind: WK::I32,
            offset: length_offset,
        });
        cold.push(N::Store {
            ptr: W::GetLocal("new_root".into()),
            value: W::GetLocal("new_cap".into()),
            kind: WK::I32,
            offset: capacity_offset,
        });
        cold.push(N::SetLocal {
            local: "out_root".into(),
            value: W::GetLocal("new_root".into()),
        });
        cold.push(N::SetLocal {
            local: "out_cap".into(),
            value: W::GetLocal("new_cap".into()),
        });

        let body = vec![
            N::SetLocal {
                local: "len".into(),
                value: W::Load {
                    ptr: Box::new(W::GetLocal("root".into())),
                    kind: WK::I32,
                    offset: length_offset,
                },
            },
            N::If {
                cond: binary(WB::Gt, W::GetLocal("cap".into()), W::GetLocal("len".into())),
                then_: hot,
                els: cold,
                result: None,
            },
            N::Push(W::GetLocal("out_root".into())),
            N::Push(W::GetLocal("out_cap".into())),
        ];
        self.layout_wir_funcs.insert(name.clone(), WirFunc {
            name: name.clone(),
            params,
            ret: vec![WirTy::Bool, WirTy::Bool],
            locals: vec![
                WirLocal { name: "p".into(), ty: WirTy::Bool },
                WirLocal { name: "len".into(), ty: WirTy::Bool },
                WirLocal { name: "new_cap".into(), ty: WirTy::Bool },
                WirLocal { name: "logical_size".into(), ty: WirTy::Bool },
                WirLocal { name: "new_root".into(), ty: WirTy::Bool },
                WirLocal { name: "out_root".into(), ty: WirTy::Bool },
                WirLocal { name: "out_cap".into(), ty: WirTy::Bool },
            ],
            body,
            raw_body: None,
        });
        Some(name)
    }

    fn lower_packed_list_push_call(
        &mut self,
        root: &str,
        elem: &Expr,
        cap: witchy_wir::wir::WirExpr,
    ) -> Option<(String, Vec<witchy_wir::wir::WirExpr>)> {
        let list_ty = self.local_types.get(root)?.clone();
        let id = self.specialized_layout_id(&list_ty)?;
        let LayoutKind::PackedList { element, .. } = self.specialized_layouts.get(id)?.kind()
        else {
            return None;
        };
        let element_descriptor = self.specialized_layouts.get(*element)?.clone();
        let fields = match elem {
            Expr::Ctor { args: fields, .. } | Expr::Tuple(fields) => {
                if fields.len() != element_descriptor.fields().len() {
                    return None;
                }
                fields
                    .iter()
                    .map(|field| self.lower_expr(field))
                    .collect::<Option<Vec<_>>>()?
            }
            // A physically specialized generic may append a packed-record
            // parameter. Restrict this path to an exact local with the same
            // descriptor; arbitrary expressions remain fail-closed.
            Expr::Var(name) => {
                let element_ty = self.local_types.get(name)?.clone();
                let element_id = self.specialized_layout_id(&element_ty)?;
                if element_id != *element {
                    return None;
                }
                element_descriptor
                    .fields()
                    .iter()
                    .copied()
                    .map(|field| {
                        self.lower_layout_field_read(
                            witchy_wir::wir::WirExpr::GetLocal(name.clone()),
                            field,
                        )
                    })
                    .collect::<Option<Vec<_>>>()?
            }
            _ => return None,
        };
        let helper = self.ensure_packed_list_push_helper(id)?;
        let mut args = vec![witchy_wir::wir::WirExpr::GetLocal(root.into()), cap];
        args.extend(fields);
        Some((helper, args))
    }

    fn ensure_packed_list_helper(&mut self, id: LayoutId, count: usize) -> Option<String> {
        use witchy_wir::wir::{Kind as WK, WirExpr as W, WirFunc, WirLocal, WirNode as N, WirTy};
        let name = Self::layout_helper_name("packed_list", id, Some(count));
        if self.layout_wir_funcs.contains_key(&name) {
            return Some(name);
        }
        let descriptor = self.specialized_layouts.get(id)?.clone();
        let LayoutKind::PackedList { element, .. } = descriptor.kind() else { return None };
        let element = *element;
        let element_descriptor = self.specialized_layouts.get(element)?.clone();
        let HeaderLayout::PackedList {
            rc,
            length_offset,
            capacity_offset,
            data_offset,
            ..
        } = descriptor.header() else { return None };
        let LayoutSize::Dynamic { base, stride } = descriptor.size() else { return None };
        let size = base.checked_add(stride.checked_mul(count as u32)?)?;
        let fields = element_descriptor.fields();
        let mut params = Vec::with_capacity(fields.len() * count);
        for index in 0..count {
            for (field_index, field) in fields.iter().enumerate() {
                let kind = self.layout_field_kind(field.kind())?;
                params.push(WirLocal {
                    name: format!("e{index}f{field_index}"),
                    ty: Self::wir_ty_for_kind(kind),
                });
            }
        }
        let (mut body, root) = self.layout_alloc_nodes_with_header(size, rc);
        body.push(N::Store {
            ptr: root.clone(),
            value: W::ConstI32(count as i32),
            kind: WK::I32,
            offset: length_offset,
        });
        body.push(N::Store {
            ptr: root.clone(),
            value: W::ConstI32(count as i32),
            kind: WK::I32,
            offset: capacity_offset,
        });
        for index in 0..count {
            for (field_index, field) in fields.iter().copied().enumerate() {
                let element_base = W::Binary {
                    op: witchy_wir::wir::BinOp::Add,
                    kind: WK::I32,
                    lhs: Box::new(root.clone()),
                    rhs: Box::new(W::ConstI32((data_offset + stride * index as u32) as i32)),
                };
                self.push_layout_store(
                    &mut body,
                    element_base,
                    field,
                    W::GetLocal(format!("e{index}f{field_index}")),
                )?;
            }
        }
        body.push(N::Push(root));
        self.layout_wir_funcs.insert(name.clone(), WirFunc {
            name: name.clone(),
            params,
            ret: vec![WirTy::Bool],
            locals: vec![WirLocal { name: "p".into(), ty: WirTy::Bool }],
            body,
            raw_body: None,
        });
        Some(name)
    }

    fn lower_packed_record_ctor(
        &mut self,
        expr: &Expr,
        args: &[Expr],
    ) -> Option<witchy_wir::wir::WirExpr> {
        let id = self.specialized_layout_of_expr(expr)?;
        let descriptor = self.specialized_layouts.get(id)?;
        if self.fn_destination_layouts.get(&self.cur_fn_name) == Some(&id) {
            return self.lower_packed_destination_ctor_inline(id, expr, args);
        }
        let helper = match descriptor.kind() {
            LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }
                if descriptor.fields().len() == args.len() =>
            {
                self.ensure_packed_record_helper(id)?
            }
            LayoutKind::ClosedSum { .. } => {
                let Expr::Ctor { name, .. } = expr else { return None };
                let (tag, arity) = self.ctors.get(name).copied()?;
                if arity != args.len()
                    || self
                        .specialized_layouts
                        .get(id)?
                        .variant_layouts()
                        .get(tag as usize)?
                        .fields()
                        .len()
                        != args.len()
                {
                    return None;
                }
                self.ensure_packed_sum_ctor_helper(id, tag as usize)?
            }
            _ => return None,
        };
        let lowered = args
            .iter()
            .map(|arg| self.lower_expr(arg))
            .collect::<Option<Vec<_>>>()?;
        Some(witchy_wir::wir::WirExpr::Call { func: helper, args: lowered })
    }

    fn scalar_sum_layout_for_binding(
        &self,
        name: &str,
        value: &Expr,
    ) -> Option<ScalarSumLayout> {
        let id = self
            .local_types
            .get(name)
            .and_then(|ty| self.specialized_layout_id(ty))?;
        let descriptor = self.specialized_layouts.get(id)?;
        if !matches!(descriptor.kind(), LayoutKind::ClosedSum { .. })
            || !matches!(descriptor.size(), LayoutSize::Fixed(_))
            || !self.scalar_sum_value_matches_layout(id, value)
        {
            return None;
        }
        let max_arity = descriptor
            .variant_layouts()
            .iter()
            .map(|variant| variant.fields().len())
            .max()
            .unwrap_or(0);
        Some(ScalarSumLayout { id, max_arity })
    }

    /// An adjacent confined sum binding and its sole match may bypass even the
    /// scalar tag/payload representation when the constructor decision is pure.
    /// Keep this proof deliberately smaller than general SROA: scalar arithmetic
    /// only, exact constructor arms, no guards, and no intervening statement.
    /// Calls, pointer-shaped locals, wildcard dispatch, and nested patterns retain
    /// the ordinary scalarized or materialized path.
    fn scalar_sum_fusion_layout(
        &self,
        name: &str,
        value: &Expr,
        next: Option<&Stmt>,
    ) -> Option<ScalarSumLayout> {
        let layout = self.scalar_sum_layout_for_binding(name, value)?;
        let Stmt::Expr(Expr::Match { scrutinee, arms }) = next? else {
            return None;
        };
        if !matches!(scrutinee.as_ref(), Expr::Var(local) if local == name)
            || arms.is_empty()
            || arms.iter().any(|arm| arm.guard.is_some())
            || !self.scalar_sum_value_has_fusable_arms(value, arms)
        {
            return None;
        }
        Some(layout)
    }

    fn scalar_sum_value_has_fusable_arms(&self, value: &Expr, arms: &[MatchArm]) -> bool {
        match value {
            Expr::Ctor { name, args } => {
                if !args
                    .iter()
                    .all(|argument| self.scalar_sum_fusion_pure_scalar(argument))
                {
                    return false;
                }
                let mut matching = arms.iter().filter(|arm| {
                    matches!(&arm.pattern, Pattern::Ctor { name: arm_name, .. }
                        if arm_name == name)
                });
                let Some(arm) = matching.next() else {
                    return false;
                };
                matching.next().is_none()
                    && matches!(&arm.pattern, Pattern::Ctor { args: patterns, .. }
                        if patterns.len() == args.len()
                            && patterns.iter().all(|pattern| {
                                matches!(pattern, Pattern::Var(_) | Pattern::Wildcard)
                            }))
            }
            Expr::If {
                cond,
                then_block,
                else_block: Some(else_block),
            } => {
                self.scalar_sum_fusion_pure_scalar(cond)
                    && self.scalar_sum_tail_has_fusable_arms(then_block, arms)
                    && self.scalar_sum_tail_has_fusable_arms(else_block, arms)
            }
            _ => false,
        }
    }

    fn scalar_sum_tail_has_fusable_arms(&self, block: &Block, arms: &[MatchArm]) -> bool {
        block.region.is_none()
            && matches!(block.stmts.as_slice(), [Stmt::Expr(value)]
                if self.scalar_sum_value_has_fusable_arms(value, arms))
    }

    /// The fused value is cloned into the per-unit plan, but the loan ledger is
    /// keyed by the checked AST's statement addresses. Account for every pure
    /// constructor tail at plan installation while those original identities are
    /// still in hand; the fusion proof excludes expressions that could carry an
    /// actual loan event into the delayed match.
    fn mark_scalar_sum_fusion_tail_loan_keys(&mut self, value: &Expr) {
        let Expr::If {
            then_block,
            else_block: Some(else_block),
            ..
        } = value
        else {
            return;
        };
        for block in [then_block, else_block] {
            let [statement @ Stmt::Expr(tail)] = block.stmts.as_slice() else {
                continue;
            };
            if let Some(key) = self.loan_facts.event_key(statement)
                && let Some((_, seen)) = self.loan_fact_stack.last_mut()
            {
                seen.insert(key);
            }
            self.mark_scalar_sum_fusion_tail_loan_keys(tail);
        }
    }

    fn scalar_sum_fusion_pure_scalar(&self, value: &Expr) -> bool {
        match value {
            Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Bool(_) => true,
            Expr::Var(name) => self
                .locals
                .get(name)
                .is_some_and(|kind| matches!(*kind, Kind::I64 | Kind::F64)),
            Expr::Unary { op, expr } => {
                matches!(op, UnOp::Neg | UnOp::Not | UnOp::BitNot)
                    && self.scalar_sum_fusion_pure_scalar(expr)
            }
            Expr::Binary { op, lhs, rhs } => {
                !matches!(op, BinOp::Concat | BinOp::Coalesce)
                    && self.scalar_sum_fusion_pure_scalar(lhs)
                    && self.scalar_sum_fusion_pure_scalar(rhs)
            }
            _ => false,
        }
    }

    fn scalar_sum_value_matches_layout(&self, id: LayoutId, value: &Expr) -> bool {
        if self.specialized_layout_of_expr(value) != Some(id) {
            return false;
        }
        match value {
            Expr::Ctor { name, args } => {
                let Some((tag, arity)) = self.ctors.get(name).copied() else {
                    return false;
                };
                let Some(descriptor) = self.specialized_layouts.get(id) else {
                    return false;
                };
                arity == args.len()
                    && descriptor
                        .variant_layouts()
                        .get(tag as usize)
                        .is_some_and(|variant| variant.fields().len() == args.len())
                    && args.iter().all(|argument| !self.kind_of(argument).is_ref())
            }
            Expr::If {
                then_block,
                else_block: Some(else_block),
                ..
            } => {
                self.scalar_sum_tail_matches_layout(id, then_block)
                    && self.scalar_sum_tail_matches_layout(id, else_block)
            }
            _ => false,
        }
    }

    fn scalar_sum_tail_matches_layout(&self, id: LayoutId, block: &Block) -> bool {
        block.region.is_none()
            && matches!(block.stmts.as_slice(), [Stmt::Expr(value)]
                if self.scalar_sum_value_matches_layout(id, value))
    }

    fn lower_confined_scalar_sum_binding(
        &mut self,
        name: &str,
        value: &Expr,
    ) -> Option<(ScalarSumLayout, witchy_wir::wir::WirSeq)> {
        let layout = self.scalar_sum_layout_for_binding(name, value)?;
        let nodes = self.lower_confined_scalar_sum_value(name, layout.id, value)?;
        Some((layout, nodes))
    }

    fn lower_confined_scalar_sum_value(
        &mut self,
        local: &str,
        id: LayoutId,
        value: &Expr,
    ) -> Option<witchy_wir::wir::WirSeq> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        if !self.scalar_sum_value_matches_layout(id, value) {
            return None;
        }
        match value {
            Expr::Ctor { name, args } => {
                let (tag, _) = self.ctors.get(name).copied()?;
                let mut nodes = Vec::with_capacity(args.len() + 1);
                for (index, argument) in args.iter().enumerate() {
                    let kind = self.kind_of(argument);
                    let lowered = self.lower_expr(argument)?;
                    nodes.push(N::SetLocal {
                        local: scalar_sum_payload_local(local, index),
                        value: W::ToSlot(Box::new(lowered), Self::wir_kind(kind)),
                    });
                }
                nodes.push(N::SetLocal {
                    local: scalar_sum_tag_local(local),
                    value: W::ConstI32(tag as i32),
                });
                Some(nodes)
            }
            Expr::If {
                cond,
                then_block,
                else_block: Some(else_block),
            } => {
                let condition = self.lower_expr(cond)?;
                let [Stmt::Expr(then_value)] = then_block.stmts.as_slice() else {
                    return None;
                };
                let [Stmt::Expr(else_value)] = else_block.stmts.as_slice() else {
                    return None;
                };
                Some(vec![N::If {
                    cond: condition,
                    then_: self.lower_confined_scalar_sum_tail(
                        local,
                        id,
                        &then_block.stmts[0],
                        then_value,
                    )?,
                    els: self.lower_confined_scalar_sum_tail(
                        local,
                        id,
                        &else_block.stmts[0],
                        else_value,
                    )?,
                    result: None,
                }])
            }
            _ => None,
        }
    }

    /// A scalarized `if` consumes its constructor tail without invoking normal
    /// block lowering (which would materialize it). Preserve the checked AST's
    /// per-statement loan identity and active-event context explicitly.
    fn lower_confined_scalar_sum_tail(
        &mut self,
        local: &str,
        id: LayoutId,
        statement: &Stmt,
        value: &Expr,
    ) -> Option<witchy_wir::wir::WirSeq> {
        let saved_events = std::mem::replace(
            &mut self.active_loan_events,
            self.loan_facts.active_at(statement).to_vec(),
        );
        if let Some(key) = self.loan_facts.event_key(statement)
            && let Some((_, seen)) = self.loan_fact_stack.last_mut()
        {
            seen.insert(key);
        }
        let lowered = self.lower_confined_scalar_sum_value(local, id, value);
        self.active_loan_events = saved_events;
        lowered
    }

    fn lower_packed_destination_ctor_inline(
        &mut self,
        id: LayoutId,
        expr: &Expr,
        args: &[Expr],
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{BinOp as WB, Kind as WK, WirExpr as W, WirNode as N};
        let descriptor = self.specialized_layouts.get(id)?.clone();
        let LayoutSize::Fixed(size) = descriptor.size() else {
            return None;
        };
        let (fields, tag) = match descriptor.kind() {
            LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }
                if descriptor.fields().len() == args.len() =>
            {
                (descriptor.fields().to_vec(), None)
            }
            LayoutKind::ClosedSum { .. } => {
                let Expr::Ctor { name, .. } = expr else {
                    return None;
                };
                let (tag, arity) = self.ctors.get(name).copied()?;
                let fields = descriptor
                    .variant_layouts()
                    .get(tag as usize)?
                    .fields()
                    .to_vec();
                if arity != args.len() || fields.len() != args.len() {
                    return None;
                }
                (fields, Some(tag as usize))
            }
            _ => return None,
        };
        let lowered = args
            .iter()
            .map(|argument| self.lower_expr(argument))
            .collect::<Option<Vec<_>>>()?;
        let destination = W::GetLocal(DESTINATION_PARAM.into());
        let (allocation, _) = self.layout_alloc_expr_nodes_into(
            W::ConstI32(size as i32),
            DESTINATION_RESULT_TMP,
        );
        let mut nodes = vec![N::If {
            cond: W::Binary {
                op: WB::Ne,
                kind: WK::I32,
                lhs: Box::new(destination.clone()),
                rhs: Box::new(W::ConstI32(0)),
            },
            then_: vec![N::SetLocal {
                local: DESTINATION_RESULT_TMP.into(),
                value: destination,
            }],
            els: allocation,
            result: None,
        }];
        if let Some(tag) = tag {
            self.push_layout_store(
                &mut nodes,
                W::GetLocal(DESTINATION_RESULT_TMP.into()),
                *descriptor.fields().first()?,
                W::ConstI32(tag as i32),
            )?;
        }
        for (field, value) in fields.into_iter().zip(lowered) {
            self.push_layout_store(
                &mut nodes,
                W::GetLocal(DESTINATION_RESULT_TMP.into()),
                field,
                value,
            )?;
        }
        nodes.push(N::Push(W::GetLocal(DESTINATION_RESULT_TMP.into())));
        Some(W::Seq(nodes))
    }

    fn lower_packed_list_literal(
        &mut self,
        expr: &Expr,
        items: &[Expr],
    ) -> Option<witchy_wir::wir::WirExpr> {
        let id = self.specialized_layout_of_expr(expr)?;
        let list_descriptor = self.specialized_layouts.get(id)?.clone();
        let LayoutKind::PackedList { element, .. } = list_descriptor.kind() else {
            return None;
        };
        let element_id = *element;
        let element_descriptor = self.specialized_layouts.get(element_id)?.clone();
        let mut fields = Vec::new();
        for item in items {
            match item {
                Expr::Ctor { args, .. } | Expr::Tuple(args) => {
                    if args.len() != element_descriptor.fields().len() {
                        return None;
                    }
                    fields.extend(
                        args.iter()
                            .map(|field| self.lower_expr(field))
                            .collect::<Option<Vec<_>>>()?,
                    );
                }
                // Preserve the exact packed descriptor when a literal contains
                // an already-materialized local element. Arbitrary expressions
                // remain rejected rather than being reinterpreted as slots.
                Expr::Var(name) => {
                    let element_ty = self.local_types.get(name)?.clone();
                    if self.specialized_layout_id(&element_ty)? != element_id {
                        return None;
                    }
                    fields.extend(
                        element_descriptor
                            .fields()
                            .iter()
                            .copied()
                            .map(|field| {
                                self.lower_layout_field_read(
                                    witchy_wir::wir::WirExpr::GetLocal(name.clone()),
                                    field,
                                )
                            })
                            .collect::<Option<Vec<_>>>()?,
                    );
                }
                _ => return None,
            }
        }
        let helper = self.ensure_packed_list_helper(id, items.len())?;
        Some(witchy_wir::wir::WirExpr::Call { func: helper, args: fields })
    }

    fn lower_layout_field_read(
        &mut self,
        base: witchy_wir::wir::WirExpr,
        field: witchy_wir::layout::LayoutField,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{BinOp as WB, Kind as WK, WirExpr as W};
        match field.kind() {
            FieldKind::Scalar(ScalarKind::Bool | ScalarKind::Tag8) => {
                Some(W::Load8U { ptr: Box::new(base), offset: field.offset() })
            }
            FieldKind::Scalar(ScalarKind::Tag16) => Some(W::Binary {
                op: WB::Or,
                kind: WK::I32,
                lhs: Box::new(W::Load8U {
                    ptr: Box::new(base.clone()),
                    offset: field.offset(),
                }),
                rhs: Box::new(W::Binary {
                    op: WB::Shl,
                    kind: WK::I32,
                    lhs: Box::new(W::Load8U {
                        ptr: Box::new(base),
                        offset: field.offset().checked_add(1)?,
                    }),
                    rhs: Box::new(W::ConstI32(8)),
                }),
            }),
            FieldKind::Scalar(scalar) => Some(W::Load {
                ptr: Box::new(base),
                kind: match scalar {
                    ScalarKind::Int | ScalarKind::Duration => WK::I64,
                    ScalarKind::Float => WK::F64,
                    ScalarKind::U32 | ScalarKind::Tag32 => WK::I32,
                    ScalarKind::Bool | ScalarKind::Tag8 | ScalarKind::Tag16 => unreachable!(),
                },
                offset: field.offset(),
            }),
            FieldKind::Inline(child) => {
                let LayoutSize::Fixed(_) = self.specialized_layouts.get(child)?.size() else {
                    return None;
                };
                Some(W::Binary {
                    op: WB::Add,
                    kind: WK::I32,
                    lhs: Box::new(base),
                    rhs: Box::new(W::ConstI32(field.offset() as i32)),
                })
            }
        }
    }

    /// Address of packed-list element `index` — `list + data_offset + index *
    /// stride`, the row base a packed record occupies inline. Only fires when
    /// `list` is an exact packed-list descriptor; a mismatched or reference list
    /// returns `None` and the caller falls back to the slot path. `list` and
    /// `index` are each lowered exactly once (the index may side-effect). This
    /// is the shared addressing for both the direct `list.at(xs, i).field` read
    /// and a materialized `let e = list.at(xs, i)` binding, so the two agree on
    /// layout and on the pinned RFC-0027 inline-read trap behavior.
    fn lower_packed_list_element_address(
        &mut self,
        list: &Expr,
        index: &Expr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{BinOp as WB, Kind as WK, WirExpr as W};
        let list_id = self.specialized_layout_of_expr(list)?;
        let list_descriptor = self.specialized_layouts.get(list_id)?.clone();
        let LayoutKind::PackedList { .. } = list_descriptor.kind() else {
            return None;
        };
        let HeaderLayout::PackedList { data_offset, .. } = list_descriptor.header() else {
            return None;
        };
        let LayoutSize::Dynamic { stride, .. } = list_descriptor.size() else {
            return None;
        };
        let index_kind = self.kind_of(index);
        let index = Self::wir_convert(self.lower_expr(index)?, index_kind, Kind::I32);
        let root = self.lower_expr(list)?;
        Some(W::Binary {
            op: WB::Add,
            kind: WK::I32,
            lhs: Box::new(root),
            rhs: Box::new(W::Binary {
                op: WB::Add,
                kind: WK::I32,
                lhs: Box::new(W::ConstI32(data_offset as i32)),
                rhs: Box::new(W::Binary {
                    op: WB::Mul,
                    kind: WK::I32,
                    lhs: Box::new(index),
                    rhs: Box::new(W::ConstI32(stride as i32)),
                }),
            }),
        })
    }

    /// Materialized `list.at(xs, i)` on an exact packed-list whose element is an
    /// inline aggregate (packed record or tuple): the whole element is a row of
    /// fields, so its value is the row address, matching how a packed local is
    /// stored and later read field-by-field. Returns `None` for scalar packed
    /// elements (their value is the slot itself) and for non-packed lists, so
    /// those keep the ordinary slot read. Shares `lower_packed_list_element_address`
    /// with the direct `.field` path, so addressing and trap behavior agree.
    fn lower_packed_list_element_read(
        &mut self,
        list: &Expr,
        index: &Expr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        let list_id = self.specialized_layout_of_expr(list)?;
        let list_descriptor = self.specialized_layouts.get(list_id)?.clone();
        let LayoutKind::PackedList { element, .. } = list_descriptor.kind() else {
            return None;
        };
        let element_descriptor = self.specialized_layouts.get(*element)?.clone();
        if !matches!(
            element_descriptor.kind(),
            LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }
        ) {
            return None;
        }
        self.lower_packed_list_element_address(list, index)
    }

    fn lower_specialized_field(
        &mut self,
        base: &Expr,
        field_name: &str,
    ) -> Option<witchy_wir::wir::WirExpr> {
        if let Expr::Call { name, args } = base
            && name == intrinsics::LIST_AT
            && args.len() == 2
        {
            let list_id = self.specialized_layout_of_expr(&args[0])?;
            let list_descriptor = self.specialized_layouts.get(list_id)?.clone();
            let LayoutKind::PackedList { element, .. } = list_descriptor.kind() else {
                return None;
            };
            let element_descriptor = self.specialized_layouts.get(*element)?.clone();
            let element_ty = match self.ast_type_of_expr(&args[0])?.unqualified() {
                Type::Named(name, arguments) if name == "List" => arguments.first()?.clone(),
                _ => return None,
            };
            let field_index = match element_ty.unqualified() {
                Type::Tuple(_) => field_name.parse::<usize>().ok()?,
                Type::Named(name, _) => self
                    .record_fields
                    .get(name)?
                    .iter()
                    .position(|(candidate, _)| candidate == field_name)?,
                _ => return None,
            };
            let field = *element_descriptor.fields().get(field_index)?;
            let element_base = self.lower_packed_list_element_address(&args[0], &args[1])?;
            return self.lower_layout_field_read(element_base, field);
        }

        let id = self.specialized_layout_of_expr(base)?;
        let descriptor = self.specialized_layouts.get(id)?.clone();
        if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }) {
            return None;
        }
        let field_index = match self.ast_type_of_expr(base)?.unqualified() {
            Type::Tuple(_) => field_name.parse::<usize>().ok()?,
            Type::Named(name, _) => self
                .record_fields
                .get(name)?
                .iter()
                .position(|(candidate, _)| candidate == field_name)?,
            _ => return None,
        };
        let field = *descriptor.fields().get(field_index)?;
        let root = self.lower_expr(base)?;
        self.lower_layout_field_read(root, field)
    }

    /// Bounds-checked read for a reference-backed list. The language index is
    /// i64, so validate it before narrowing to Wasm GC's i32 array index.
    fn lower_gc_function_list_at(
        &mut self,
        list: &Expr,
        target: &Expr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{BinOp, Kind as WK, WirExpr as W, WirNode as N};
        let list_ty = self.ast_type_of_expr(list)?;
        let (type_id, array_id, _) = self.gc_reference_list_layout(&list_ty)?;
        let level = self.assign_level;
        if level >= APPLY_POOL {
            return None;
        }
        let target_kind = self.kind_of(target);
        self.assign_level = level + 1;
        let lowered = (|| {
            Some((
                self.lower_expr(list)?,
                Self::wir_convert(self.lower_expr(target)?, target_kind, Kind::I64),
            ))
        })();
        self.assign_level = level;
        let (list, target) = lowered?;
        let src = gc_list_scratch(GC_LIST_SRC_TMP, level, type_id);
        let len = gc_list_scratch(GC_LIST_LEN_TMP, level, type_id);
        let raw_index = gc_list_scratch(GC_LIST_RAW_INDEX_TMP, level, type_id);
        let len_i64 = || Self::wir_convert(
            W::GetLocal(len.clone()),
            Kind::I32,
            Kind::I64,
        );
        let invalid = W::Binary {
            op: BinOp::Or,
            kind: WK::I32,
            lhs: Box::new(W::Binary {
                op: BinOp::Lt,
                kind: WK::I64,
                lhs: Box::new(W::GetLocal(raw_index.clone())),
                rhs: Box::new(W::ConstI64(0)),
            }),
            rhs: Box::new(W::Binary {
                op: BinOp::Ge,
                kind: WK::I64,
                lhs: Box::new(W::GetLocal(raw_index.clone())),
                rhs: Box::new(len_i64()),
            }),
        };
        let abort = witchy_wir::wir_helpers::abort_nodes(
            witchy_syntax::diag::DiagTemplate::ListIndexOob,
            W::GetLocal(raw_index.clone()),
            len_i64(),
            W::ConstI32(0),
        );
        Some(W::Seq(vec![
            N::SetLocal { local: src.clone(), value: list },
            N::SetLocal { local: raw_index.clone(), value: target },
            N::SetLocal {
                local: len,
                value: W::ArrayLen(Box::new(W::GetLocal(src.clone()))),
            },
            N::If { cond: invalid, then_: abort, els: vec![], result: None },
            N::Push(W::ArrayGet {
                array_id,
                array: Box::new(W::GetLocal(src)),
                index: Box::new(Self::wir_convert(
                    W::GetLocal(raw_index),
                    Kind::I64,
                    Kind::I32,
                )),
            }),
        ]))
    }

    /// Persistent append for the GC-backed `List(fn(...))` lane. A fresh array
    /// preserves value semantics and keeps aliases on the old array; nested
    /// appends use distinct scratch levels so evaluation remains left-to-right.
    fn lower_gc_function_list_push(
        &mut self,
        list: &Expr,
        value: &Expr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{BinOp, Kind as WK, WirExpr as W, WirNode as N};
        let list_ty = self.ast_type_of_expr(list)?;
        let (type_id, array_id, element_kind) =
            self.gc_reference_list_layout(&list_ty)?;
        let level = self.assign_level;
        if level >= APPLY_POOL {
            return None;
        }
        self.assign_level = level + 1;
        let lowered = (|| Some((self.lower_expr(list)?, self.lower_expr(value)?)))();
        self.assign_level = level;
        let (list, value) = lowered?;
        let src = gc_list_scratch(GC_LIST_SRC_TMP, level, type_id);
        let dst = gc_list_scratch(GC_LIST_DST_TMP, level, type_id);
        let item = gc_list_scratch(GC_LIST_VALUE_TMP, level, type_id);
        let len = gc_list_scratch(GC_LIST_LEN_TMP, level, type_id);
        let index = gc_list_scratch(GC_LIST_INDEX_TMP, level, type_id);
        let label = self.next_label;
        self.next_label += 1;
        let exit = format!("gcle{label}");
        let loop_label = format!("gcll{label}");
        let add = |lhs, rhs| W::Binary {
            op: BinOp::Add,
            kind: WK::I32,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        Some(W::Seq(vec![
            N::SetLocal { local: src.clone(), value: list },
            N::SetLocal { local: item.clone(), value },
            N::SetLocal {
                local: len.clone(),
                value: W::ArrayLen(Box::new(W::GetLocal(src.clone()))),
            },
            N::SetLocal {
                local: dst.clone(),
                value: W::ArrayNew {
                    array_id,
                    value: Box::new(W::RefNull(Self::wir_kind(element_kind))),
                    len: Box::new(add(W::GetLocal(len.clone()), W::ConstI32(1))),
                },
            },
            N::SetLocal { local: index.clone(), value: W::ConstI32(0) },
            N::Block {
                label: exit.clone(),
                result: None,
                body: vec![N::Loop {
                    label: loop_label.clone(),
                    body: vec![
                        N::Br {
                            target: exit,
                            cond: Some(W::Binary {
                                op: BinOp::Ge,
                                kind: WK::I32,
                                lhs: Box::new(W::GetLocal(index.clone())),
                                rhs: Box::new(W::GetLocal(len.clone())),
                            }),
                        },
                        N::ArraySet {
                            array_id,
                            array: W::GetLocal(dst.clone()),
                            index: W::GetLocal(index.clone()),
                            value: W::ArrayGet {
                                array_id,
                                array: Box::new(W::GetLocal(src)),
                                index: Box::new(W::GetLocal(index.clone())),
                            },
                        },
                        N::SetLocal {
                            local: index.clone(),
                            value: add(W::GetLocal(index), W::ConstI32(1)),
                        },
                        N::Br { target: loop_label, cond: None },
                    ],
                }],
            },
            N::ArraySet {
                array_id,
                array: W::GetLocal(dst.clone()),
                index: W::GetLocal(len),
                value: W::GetLocal(item),
            },
            N::Push(W::GetLocal(dst)),
        ]))
    }

    fn lower_gc_function_list_set_at(
        &mut self,
        list: &Expr,
        target: &Expr,
        value: &Expr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{BinOp, Kind as WK, WirExpr as W, WirNode as N};
        let list_ty = self.ast_type_of_expr(list)?;
        let (type_id, array_id, element_kind) =
            self.gc_reference_list_layout(&list_ty)?;
        let level = self.assign_level;
        if level >= APPLY_POOL {
            return None;
        }
        let target_kind = self.kind_of(target);
        self.assign_level = level + 1;
        let lowered = (|| {
            Some((
                self.lower_expr(list)?,
                Self::wir_convert(self.lower_expr(target)?, target_kind, Kind::I64),
                self.lower_expr(value)?,
            ))
        })();
        self.assign_level = level;
        let (list, target, value) = lowered?;
        let src = gc_list_scratch(GC_LIST_SRC_TMP, level, type_id);
        let dst = gc_list_scratch(GC_LIST_DST_TMP, level, type_id);
        let item = gc_list_scratch(GC_LIST_VALUE_TMP, level, type_id);
        let len = gc_list_scratch(GC_LIST_LEN_TMP, level, type_id);
        let index = gc_list_scratch(GC_LIST_INDEX_TMP, level, type_id);
        let target_local = gc_list_scratch(GC_LIST_TARGET_TMP, level, type_id);
        let raw_index = gc_list_scratch(GC_LIST_RAW_INDEX_TMP, level, type_id);
        let label = self.next_label;
        self.next_label += 1;
        let exit = format!("gcse{label}");
        let loop_label = format!("gcsl{label}");
        let add_one = |value| W::Binary {
            op: BinOp::Add,
            kind: WK::I32,
            lhs: Box::new(value),
            rhs: Box::new(W::ConstI32(1)),
        };
        let len_i64 = || Self::wir_convert(
            W::GetLocal(len.clone()),
            Kind::I32,
            Kind::I64,
        );
        let invalid = W::Binary {
            op: BinOp::Or,
            kind: WK::I32,
            lhs: Box::new(W::Binary {
                op: BinOp::Lt,
                kind: WK::I64,
                lhs: Box::new(W::GetLocal(raw_index.clone())),
                rhs: Box::new(W::ConstI64(0)),
            }),
            rhs: Box::new(W::Binary {
                op: BinOp::Ge,
                kind: WK::I64,
                lhs: Box::new(W::GetLocal(raw_index.clone())),
                rhs: Box::new(len_i64()),
            }),
        };
        let abort = witchy_wir::wir_helpers::abort_nodes(
            witchy_syntax::diag::DiagTemplate::ListIndexOob,
            W::GetLocal(raw_index.clone()),
            len_i64(),
            W::ConstI32(0),
        );
        Some(W::Seq(vec![
            N::SetLocal { local: src.clone(), value: list },
            N::SetLocal { local: raw_index.clone(), value: target },
            N::SetLocal { local: item.clone(), value },
            N::SetLocal {
                local: len.clone(),
                value: W::ArrayLen(Box::new(W::GetLocal(src.clone()))),
            },
            N::If { cond: invalid, then_: abort, els: vec![], result: None },
            N::SetLocal {
                local: target_local.clone(),
                value: Self::wir_convert(
                    W::GetLocal(raw_index),
                    Kind::I64,
                    Kind::I32,
                ),
            },
            N::SetLocal {
                local: dst.clone(),
                value: W::ArrayNew {
                    array_id,
                    value: Box::new(W::RefNull(Self::wir_kind(element_kind))),
                    len: Box::new(W::GetLocal(len.clone())),
                },
            },
            N::SetLocal { local: index.clone(), value: W::ConstI32(0) },
            N::Block {
                label: exit.clone(),
                result: None,
                body: vec![N::Loop {
                    label: loop_label.clone(),
                    body: vec![
                        N::Br {
                            target: exit,
                            cond: Some(W::Binary {
                                op: BinOp::Ge,
                                kind: WK::I32,
                                lhs: Box::new(W::GetLocal(index.clone())),
                                rhs: Box::new(W::GetLocal(len)),
                            }),
                        },
                        N::ArraySet {
                            array_id,
                            array: W::GetLocal(dst.clone()),
                            index: W::GetLocal(index.clone()),
                            value: W::ArrayGet {
                                array_id,
                                array: Box::new(W::GetLocal(src.clone())),
                                index: Box::new(W::GetLocal(index.clone())),
                            },
                        },
                        N::SetLocal {
                            local: index.clone(),
                            value: add_one(W::GetLocal(index)),
                        },
                        N::Br { target: loop_label, cond: None },
                    ],
                }],
            },
            N::ArraySet {
                array_id,
                array: W::GetLocal(dst.clone()),
                index: W::GetLocal(target_local),
                value: W::GetLocal(item),
            },
            N::Push(W::GetLocal(dst)),
        ]))
    }

    fn lower_gc_function_list_concat(
        &mut self,
        left: &Expr,
        right: &Expr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{BinOp, Kind as WK, WirExpr as W, WirNode as N};
        let list_ty = self.ast_type_of_expr(left)?;
        let (type_id, array_id, element_kind) =
            self.gc_reference_list_layout(&list_ty)?;
        let right_ty = self.ast_type_of_expr(right)?;
        if self.gc_reference_list_layout(&right_ty)?.0 != type_id {
            return None;
        }
        let level = self.assign_level;
        if level >= APPLY_POOL {
            return None;
        }
        self.assign_level = level + 1;
        let lowered = (|| Some((self.lower_expr(left)?, self.lower_expr(right)?)))();
        self.assign_level = level;
        let (left, right) = lowered?;
        let src = gc_list_scratch(GC_LIST_SRC_TMP, level, type_id);
        let rhs = gc_list_scratch(GC_LIST_RIGHT_TMP, level, type_id);
        let dst = gc_list_scratch(GC_LIST_DST_TMP, level, type_id);
        let left_len = gc_list_scratch(GC_LIST_LEFT_LEN_TMP, level, type_id);
        let len = gc_list_scratch(GC_LIST_LEN_TMP, level, type_id);
        let index = gc_list_scratch(GC_LIST_INDEX_TMP, level, type_id);
        let label = self.next_label;
        self.next_label += 1;
        let exit = format!("gcce{label}");
        let loop_label = format!("gccl{label}");
        let binary = |op, lhs, rhs| W::Binary {
            op,
            kind: WK::I32,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        Some(W::Seq(vec![
            N::SetLocal { local: src.clone(), value: left },
            N::SetLocal { local: rhs.clone(), value: right },
            N::SetLocal {
                local: left_len.clone(),
                value: W::ArrayLen(Box::new(W::GetLocal(src.clone()))),
            },
            N::SetLocal {
                local: len.clone(),
                value: binary(
                    BinOp::Add,
                    W::GetLocal(left_len.clone()),
                    W::ArrayLen(Box::new(W::GetLocal(rhs.clone()))),
                ),
            },
            N::SetLocal {
                local: dst.clone(),
                value: W::ArrayNew {
                    array_id,
                    value: Box::new(W::RefNull(Self::wir_kind(element_kind))),
                    len: Box::new(W::GetLocal(len.clone())),
                },
            },
            N::SetLocal { local: index.clone(), value: W::ConstI32(0) },
            N::Block {
                label: exit.clone(),
                result: None,
                body: vec![N::Loop {
                    label: loop_label.clone(),
                    body: vec![
                        N::Br {
                            target: exit,
                            cond: Some(binary(
                                BinOp::Ge,
                                W::GetLocal(index.clone()),
                                W::GetLocal(len),
                            )),
                        },
                        N::ArraySet {
                            array_id,
                            array: W::GetLocal(dst.clone()),
                            index: W::GetLocal(index.clone()),
                            value: W::Control(Box::new(N::If {
                                cond: binary(
                                    BinOp::Lt,
                                    W::GetLocal(index.clone()),
                                    W::GetLocal(left_len.clone()),
                                ),
                                then_: vec![N::Push(W::ArrayGet {
                                    array_id,
                                    array: Box::new(W::GetLocal(src.clone())),
                                    index: Box::new(W::GetLocal(index.clone())),
                                })],
                                els: vec![N::Push(W::ArrayGet {
                                    array_id,
                                    array: Box::new(W::GetLocal(rhs.clone())),
                                    index: Box::new(binary(
                                        BinOp::Sub,
                                        W::GetLocal(index.clone()),
                                        W::GetLocal(left_len.clone()),
                                    )),
                                })],
                                result: Some(Self::wir_ty_for_kind(element_kind)),
                            })),
                        },
                        N::SetLocal {
                            local: index.clone(),
                            value: binary(
                                BinOp::Add,
                                W::GetLocal(index),
                                W::ConstI32(1),
                            ),
                        },
                        N::Br { target: loop_label, cond: None },
                    ],
                }],
            },
            N::Push(W::GetLocal(dst)),
        ]))
    }

    fn lower_gc_reference_list_tail(
        &mut self,
        value: witchy_wir::wir::WirExpr,
        type_id: u32,
        array_id: u32,
        element_kind: Kind,
        dropped: i32,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{BinOp, Kind as WK, WirExpr as W, WirNode as N};
        let level = self.assign_level;
        if level >= APPLY_POOL {
            return None;
        }
        self.assign_level = level + 1;
        let src = gc_list_scratch(GC_LIST_SRC_TMP, level, type_id);
        let dst = gc_list_scratch(GC_LIST_DST_TMP, level, type_id);
        let len = gc_list_scratch(GC_LIST_LEN_TMP, level, type_id);
        let index = gc_list_scratch(GC_LIST_INDEX_TMP, level, type_id);
        let label = self.next_label;
        self.next_label += 1;
        self.assign_level = level;
        let binary = |op, lhs, rhs| W::Binary {
            op,
            kind: WK::I32,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        let exit = format!("gcte{label}");
        let loop_label = format!("gctl{label}");
        Some(W::Seq(vec![
            N::SetLocal { local: src.clone(), value },
            N::SetLocal {
                local: len.clone(),
                value: binary(
                    BinOp::Sub,
                    W::ArrayLen(Box::new(W::GetLocal(src.clone()))),
                    W::ConstI32(dropped),
                ),
            },
            N::SetLocal {
                local: dst.clone(),
                value: W::ArrayNew {
                    array_id,
                    value: Box::new(W::RefNull(Self::wir_kind(element_kind))),
                    len: Box::new(W::GetLocal(len.clone())),
                },
            },
            N::SetLocal { local: index.clone(), value: W::ConstI32(0) },
            N::Block {
                label: exit.clone(),
                result: None,
                body: vec![N::Loop {
                    label: loop_label.clone(),
                    body: vec![
                        N::Br {
                            target: exit,
                            cond: Some(binary(
                                BinOp::Ge,
                                W::GetLocal(index.clone()),
                                W::GetLocal(len.clone()),
                            )),
                        },
                        N::ArraySet {
                            array_id,
                            array: W::GetLocal(dst.clone()),
                            index: W::GetLocal(index.clone()),
                            value: W::ArrayGet {
                                array_id,
                                array: Box::new(W::GetLocal(src.clone())),
                                index: Box::new(binary(
                                    BinOp::Add,
                                    W::GetLocal(index.clone()),
                                    W::ConstI32(dropped),
                                )),
                            },
                        },
                        N::SetLocal {
                            local: index.clone(),
                            value: binary(
                                BinOp::Add,
                                W::GetLocal(index),
                                W::ConstI32(1),
                            ),
                        },
                        N::Br { target: loop_label, cond: None },
                    ],
                }],
            },
            N::Push(W::GetLocal(dst)),
        ]))
    }

    fn lower_gc_reference_list_pop(
        &mut self,
        receiver: &Expr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{BinOp, Kind as WK, WirExpr as W, WirNode as N};
        let Expr::Var(root) = receiver else {
            return None;
        };
        let list_ty = self.ast_type_of_expr(receiver)?;
        let Type::Named(name, args) = list_ty.unqualified() else {
            return None;
        };
        if name != "List" {
            return None;
        }
        let element_ty = args.first()?.clone();
        let (type_id, array_id, element_kind) =
            self.gc_reference_list_layout(&list_ty)?;
        let option_ty = Type::Named("Option".into(), vec![element_ty]);
        let option_kind = self.kind_for_type(&option_ty);
        let level = self.assign_level;
        if level >= APPLY_POOL {
            return None;
        }
        self.assign_level = level + 1;
        let source = self.lower_expr(receiver)?;
        self.assign_level = level;
        let src = gc_list_scratch(GC_LIST_SRC_TMP, level, type_id);
        let dst = gc_list_scratch(GC_LIST_DST_TMP, level, type_id);
        let item = gc_list_scratch(GC_LIST_VALUE_TMP, level, type_id);
        let len = gc_list_scratch(GC_LIST_LEN_TMP, level, type_id);
        let index = gc_list_scratch(GC_LIST_INDEX_TMP, level, type_id);
        let label = self.next_label;
        self.next_label += 1;
        let exit = format!("gcpe{label}");
        let loop_label = format!("gcpl{label}");
        let binary = |op, lhs, rhs| W::Binary {
            op,
            kind: WK::I32,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        let some = if self.option_reference_inner(&option_ty).is_some() {
            W::GetLocal(item.clone())
        } else {
            self.gc_ctor_value(
                "Some",
                &option_ty,
                vec![(W::GetLocal(item.clone()), element_kind)],
            )?
        };
        let none = if self.option_reference_inner(&option_ty).is_some() {
            W::RefNull(Self::wir_kind(option_kind))
        } else {
            self.gc_ctor_value("None", &option_ty, Vec::new())?
        };
        Some(W::Seq(vec![
            N::SetLocal { local: src.clone(), value: source },
            N::SetLocal {
                local: len.clone(),
                value: W::ArrayLen(Box::new(W::GetLocal(src.clone()))),
            },
            N::If {
                cond: binary(
                    BinOp::Gt,
                    W::GetLocal(len.clone()),
                    W::ConstI32(0),
                ),
                then_: vec![
                    N::SetLocal {
                        local: item,
                        value: W::ArrayGet {
                            array_id,
                            array: Box::new(W::GetLocal(src.clone())),
                            index: Box::new(binary(
                                BinOp::Sub,
                                W::GetLocal(len.clone()),
                                W::ConstI32(1),
                            )),
                        },
                    },
                    N::SetLocal {
                        local: dst.clone(),
                        value: W::ArrayNew {
                            array_id,
                            value: Box::new(W::RefNull(Self::wir_kind(
                                element_kind,
                            ))),
                            len: Box::new(binary(
                                BinOp::Sub,
                                W::GetLocal(len.clone()),
                                W::ConstI32(1),
                            )),
                        },
                    },
                    N::SetLocal {
                        local: index.clone(),
                        value: W::ConstI32(0),
                    },
                    N::Block {
                        label: exit.clone(),
                        result: None,
                        body: vec![N::Loop {
                            label: loop_label.clone(),
                            body: vec![
                                N::Br {
                                    target: exit,
                                    cond: Some(binary(
                                        BinOp::Ge,
                                        W::GetLocal(index.clone()),
                                        binary(
                                            BinOp::Sub,
                                            W::GetLocal(len.clone()),
                                            W::ConstI32(1),
                                        ),
                                    )),
                                },
                                N::ArraySet {
                                    array_id,
                                    array: W::GetLocal(dst.clone()),
                                    index: W::GetLocal(index.clone()),
                                    value: W::ArrayGet {
                                        array_id,
                                        array: Box::new(W::GetLocal(src.clone())),
                                        index: Box::new(W::GetLocal(index.clone())),
                                    },
                                },
                                N::SetLocal {
                                    local: index.clone(),
                                    value: binary(
                                        BinOp::Add,
                                        W::GetLocal(index),
                                        W::ConstI32(1),
                                    ),
                                },
                                N::Br { target: loop_label, cond: None },
                            ],
                        }],
                    },
                    N::SetLocal {
                        local: root.clone(),
                        value: W::GetLocal(dst),
                    },
                    N::Push(some),
                ],
                els: vec![N::Push(none)],
                result: Some(Self::wir_ty_for_kind(option_kind)),
            },
        ]))
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

    /// Build the WIR for a plain direct user-function call: each argument lowered
    /// and widened to its parameter's kind, then `call $name`. Returns `None` if
    /// any argument isn't lowerable. ONLY sound from `lower_expr`'s call arm, after
    /// builtins/natives/closures have been excluded, and only for functions WITHOUT
    /// an own-ABI token or `var` writeback.
    fn try_lower_user_call(
        &mut self,
        name: &str,
        emitted_name: &str,
        args: &[Expr],
        access: &witchy_types::access::AccessSignature,
    ) -> Option<witchy_wir::wir::WirExpr> {
        let ownership = self.ownership_envelope_for_named_signature(name, access);
        let param_kinds: Vec<Kind> = self
            .fn_params
            .get(name)
            .map(|ps| ps.iter().map(|p| p.ty.as_ref().map(|t| self.kind_for_type(t)).unwrap_or(Kind::I32)).collect())
            .unwrap_or_default();
        let mut args_w = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let ak = self.kind_of(arg);
            let transient = self.transient_destination_call(name, i, args.len(), arg);
            let w = match transient {
                Some((producer, producer_args, id, producer_access)) => {
                    let emitted_producer = self.generic_call_target(arg, producer).to_string();
                    let site = arg as *const Expr as usize;
                    let scratch = if let Some((scratch, known_id)) =
                        self.destination_scratch_sites.get(&site)
                    {
                        if *known_id != id {
                            return None;
                        }
                        scratch.clone()
                    } else {
                        self.ensure_layout_destination_scratch_helper(id)?;
                        let scratch = format!(
                            "__witchy_destination_scratch_{}",
                            self.destination_scratch_sites.len()
                        );
                        self.destination_scratch_sites
                            .insert(site, (scratch.clone(), id));
                        scratch
                    };
                    self.lower_destination_user_call(
                        producer,
                        &emitted_producer,
                        producer_args,
                        witchy_wir::wir::WirExpr::GetLocal(scratch),
                        &producer_access,
                    )?
                }
                None => match self.lower_expr(arg) {
                    Some(value) => value,
                    None => {
                        if std::env::var_os("WIRDIAG").is_some() {
                            eprintln!(
                                "WIRBAIL user-call-arg: callee={name} index={i} arg={arg:?}"
                            );
                        }
                        return None;
                    }
                },
            };
            args_w.push(match param_kinds.get(i) {
                Some(&pk) => Self::wir_convert(w, ak, pk),
                None => w,
            });
        }
        if let Some(own_index) = self
            .summaries
            .own_abi(name)
            .filter(|index| ownership.own_capacity_param == Some(*index))
        {
            // (RFC-0033 R3) The callee carries the own-ABI: a trailing i32 cap
            // PARAM and an extra i32 cap RESULT. A PLAIN call (not the
            // `x = f(move x)` self-call that `self_own_call` threads) derives a
            // token from the argument: tracked owners and fresh aggregates carry
            // capacity; unknown values pass zero and safely re-own in the callee.
            // The returned token is retained only for a declared unique result.
            // The declared value uses a kind-correct scratch because a closed
            // reference-bearing `List(T)` is a concrete GC array reference.
            use witchy_wir::wir::{WirExpr as W, WirNode as N};
            let cap = args
                .get(own_index)
                .map(|arg| self.owned_argument_cap(arg))
                .unwrap_or(W::ConstI32(0));
            args_w.push(cap);
            let result_kind = self.fn_ret.get(name).copied().unwrap_or(Kind::I32);
            let result_tmp = call_result_tmp(result_kind);
            let mut dests = vec![result_tmp.clone()];
            if ownership.unique_capacity_result {
                dests.push(UNIQUE_RESULT_CAP_TMP.to_string());
            }
            dests.push("__witchy_owncap".to_string());
            return Some(W::Seq(vec![
                N::CallStoreMulti {
                    func: emitted_name.to_string(),
                    args: args_w,
                    dests,
                },
                N::Push(W::GetLocal(result_tmp)),
            ]));
        }
        if ownership.unique_capacity_result {
            use witchy_wir::wir::{WirExpr as W, WirNode as N};
            if self.fn_destination_layouts.contains_key(name) {
                // The checked destination candidates above exclude own/var
                // capacity parameters, so the hidden destination follows the
                // complete source parameter list and precedes no state inputs.
                args_w.push(W::ConstI32(0));
            }
            let result_kind = self.fn_ret.get(name).copied().unwrap_or(Kind::I32);
            let result_tmp = call_result_tmp(result_kind);
            return Some(W::Seq(vec![
                N::CallStoreMulti {
                    func: emitted_name.to_string(),
                    args: args_w,
                    dests: vec![
                        result_tmp.clone(),
                        UNIQUE_RESULT_CAP_TMP.to_string(),
                    ],
                },
                N::Push(W::GetLocal(result_tmp)),
            ]));
        }
        if self.fn_destination_layouts.contains_key(name) {
            args_w.push(witchy_wir::wir::WirExpr::ConstI32(0));
        }
        Some(witchy_wir::wir::WirExpr::Call {
            func: emitted_name.to_string(),
            args: args_w,
        })
    }

    fn transient_destination_call<'expr>(
        &self,
        consumer: &str,
        index: usize,
        argc: usize,
        argument: &'expr Expr,
    ) -> Option<(
        &'expr str,
        &'expr [Expr],
        LayoutId,
        witchy_types::access::AccessSignature,
    )> {
        let Expr::Call {
            name: producer,
            args,
        } = argument
        else {
            return None;
        };
        let id = self.fn_destination_layouts.get(producer).copied()?;
        let access = self.call_access_signature(argument)?.clone();
        let ownership = self.ownership_envelope_for_named_signature(producer, &access);
        if !matches!(
            self.specialized_layouts.get(id)?.kind(),
            LayoutKind::ClosedSum { .. }
        ) || self.summaries.arg_leaks(consumer, index, argc)
            || self.summaries.arg_may_alias_out(consumer, index)
            || ownership.own_capacity_param.is_some()
            || !ownership.var_capacity_params.is_empty()
        {
            return None;
        }
        let consumer_id = self
            .callable_layouts
            .get(consumer)?
            .parameters()
            .get(index)
            .copied()
            .flatten()?;
        (consumer_id == id).then_some((producer.as_str(), args.as_slice(), id, access))
    }

    /// Lower a direct call that initializes a proven-dead exact-layout local.
    /// This is intentionally separate from ordinary calls so the fallback ABI
    /// always supplies zero and therefore allocates normally.
    fn lower_destination_user_call(
        &mut self,
        name: &str,
        emitted_name: &str,
        args: &[Expr],
        destination: witchy_wir::wir::WirExpr,
        access: &witchy_types::access::AccessSignature,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        let ownership = self.ownership_envelope_for_named_signature(name, access);
        if ownership.own_capacity_param.is_some()
            || !ownership.var_capacity_params.is_empty()
        {
            return None;
        }
        let param_kinds: Vec<Kind> = self
            .fn_params
            .get(name)?
            .iter()
            .map(|p| {
                p.ty.as_ref()
                    .map(|t| self.kind_for_type(t))
                    .unwrap_or(Kind::I32)
            })
            .collect();
        let mut lowered = Vec::with_capacity(args.len() + 1);
        for (index, arg) in args.iter().enumerate() {
            let source_kind = self.kind_of(arg);
            let value = self.lower_expr(arg)?;
            lowered.push(match param_kinds.get(index) {
                Some(&parameter_kind) => Self::wir_convert(value, source_kind, parameter_kind),
                None => value,
            });
        }
        lowered.push(destination);
        let result_kind = self.fn_ret.get(name).copied().unwrap_or(Kind::I32);
        let result_tmp = call_result_tmp(result_kind);
        let mut nodes = vec![self.increment_hot_counter(
            "__witchy_destination_candidates_forwarded",
        )];
        if ownership.unique_capacity_result {
            nodes.push(N::CallStoreMulti {
                func: emitted_name.into(),
                args: lowered,
                dests: vec![
                    result_tmp.clone(),
                    UNIQUE_RESULT_CAP_TMP.to_string(),
                ],
            });
            nodes.push(N::Push(W::GetLocal(result_tmp)));
        } else {
            nodes.push(N::Push(W::Call {
                func: emitted_name.into(),
                args: lowered,
            }));
        }
        Some(W::Seq(nodes))
    }

    fn lower_scalar_record_call(
        &mut self,
        producer: &str,
        emitted_producer: &str,
        args: &[Expr],
        destination: &str,
        count_forwarding: bool,
    ) -> Option<witchy_wir::wir::WirSeq> {
        use witchy_wir::wir::WirNode as N;
        let plan = self.scalar_record_producers.get(producer)?.clone();
        let param_kinds: Vec<Kind> = self
            .fn_params
            .get(producer)?
            .iter()
            .map(|param| {
                param
                    .ty
                    .as_ref()
                    .map(|ty| self.kind_for_type(ty))
                    .unwrap_or(Kind::I32)
            })
            .collect();
        let mut lowered = Vec::with_capacity(args.len());
        for (index, argument) in args.iter().enumerate() {
            let source_kind = self.kind_of(argument);
            let value = self.lower_expr(argument)?;
            lowered.push(match param_kinds.get(index) {
                Some(&parameter_kind) => {
                    Self::wir_convert(value, source_kind, parameter_kind)
                }
                None => value,
            });
        }
        let mut nodes = Vec::with_capacity(2);
        if count_forwarding {
            nodes.push(self.increment_hot_counter(
                "__witchy_destination_candidates_forwarded",
            ));
        }
        nodes.push(N::CallStoreMulti {
            func: Self::scalar_record_companion_name(emitted_producer),
            args: lowered,
            dests: (0..plan.field_count)
                .map(|index| format!("{destination}${index}"))
                .collect(),
        });
        Some(nodes)
    }

    fn owned_argument_cap(&self, arg: &Expr) -> witchy_wir::wir::WirExpr {
        use witchy_wir::wir::WirExpr as W;
        match arg {
            Expr::Var(name) if self.inplace_push.contains(name) => {
                W::GetLocal(format!("{name}__cap"))
            }
            Expr::Var(name) if self.cur_fn_own_param.as_deref() == Some(name) => {
                W::GetLocal(format!("{name}__cap"))
            }
            Expr::List(items) => W::ConstI32(items.len() as i32),
            Expr::Ctor { .. } | Expr::AnonCtor { .. } | Expr::Record { .. } => W::ConstI32(1),
            Expr::Unary { op: UnOp::Move, expr } => self.owned_argument_cap(expr),
            _ => W::ConstI32(0),
        }
    }

    fn capture_codegen_place(
        &mut self,
        expr: &Expr,
        next_coordinate: &mut usize,
        prelude: &mut witchy_wir::wir::WirSeq,
    ) -> Option<CodegenPlace> {
        use witchy_wir::wir::WirNode as N;
        match expr {
            Expr::Var(root) if self.locals.contains_key(root) => {
                Some(CodegenPlace::Root(root.clone()))
            }
            Expr::Field { base, field } => Some(CodegenPlace::Field {
                base: Box::new(self.capture_codegen_place(base, next_coordinate, prelude)?),
                field: field.clone(),
            }),
            Expr::Index { base, index } => {
                let captured_base = self.capture_codegen_place(base, next_coordinate, prelude)?;
                if *next_coordinate >= SCRUT_POOL {
                    return None;
                }
                let index_kind = self.kind_of(index);
                let coordinate = var_scratch("coord", *next_coordinate, index_kind);
                *next_coordinate += 1;
                let index_value = self.lower_expr(index)?;
                prelude.push(N::SetLocal {
                    local: coordinate.clone(),
                    value: index_value,
                });
                Some(CodegenPlace::Index {
                    base: Box::new(captured_base),
                    coordinate,
                    coordinate_kind: index_kind,
                    coordinate_type: self.val_type_of(index),
                    dict: self.is_dict_operand(base),
                })
            }
            Expr::Call { name, args }
                if matches!(name.as_str(), intrinsics::LIST_AT | intrinsics::DICT_AT)
                    && args.len() == 2 =>
            {
                let captured_base =
                    self.capture_codegen_place(&args[0], next_coordinate, prelude)?;
                if *next_coordinate >= SCRUT_POOL {
                    return None;
                }
                let index_kind = self.kind_of(&args[1]);
                let coordinate = var_scratch("coord", *next_coordinate, index_kind);
                *next_coordinate += 1;
                let index_value = self.lower_expr(&args[1])?;
                prelude.push(N::SetLocal {
                    local: coordinate.clone(),
                    value: index_value,
                });
                Some(CodegenPlace::Index {
                    base: Box::new(captured_base),
                    coordinate,
                    coordinate_kind: index_kind,
                    coordinate_type: self.val_type_of(&args[1]),
                    dict: name == intrinsics::DICT_AT,
                })
            }
            _ => None,
        }
    }

    fn codegen_place_read(place: &CodegenPlace) -> Expr {
        let root = Expr::Var(Self::codegen_place_root(place).to_string());
        Self::codegen_place_read_from(place, &root)
    }

    fn codegen_place_root(place: &CodegenPlace) -> &str {
        match place {
            CodegenPlace::Root(root) => root,
            CodegenPlace::Field { base, .. } | CodegenPlace::Index { base, .. } => {
                Self::codegen_place_root(base)
            }
        }
    }

    fn codegen_place_read_from(place: &CodegenPlace, root: &Expr) -> Expr {
        match place {
            CodegenPlace::Root(_) => root.clone(),
            CodegenPlace::Field { base, field } => Expr::Field {
                base: Box::new(Self::codegen_place_read_from(base, root)),
                field: field.clone(),
            },
            CodegenPlace::Index { base, coordinate, dict, .. } => Expr::Call {
                name: if *dict { intrinsics::DICT_AT } else { intrinsics::LIST_AT }.to_string(),
                args: vec![
                    Self::codegen_place_read_from(base, root),
                    Expr::Var(coordinate.clone()),
                ],
            },
        }
    }

    fn codegen_place_update_from(
        place: &CodegenPlace,
        replacement: Expr,
        root: &Expr,
    ) -> Expr {
        match place {
            CodegenPlace::Root(_) => replacement,
            CodegenPlace::Field { base, field } => {
                let updated = Expr::RecordUpdate {
                    name: None,
                    base: Box::new(Self::codegen_place_read_from(base, root)),
                    fields: vec![(field.clone(), replacement)],
                };
                Self::codegen_place_update_from(base, updated, root)
            }
            CodegenPlace::Index { base, coordinate, dict, .. } => {
                let updated = Expr::Call {
                    name: (if *dict {
                        intrinsics::DICT_INSERT
                    } else {
                        intrinsics::LIST_SET_AT
                    })
                    .to_string(),
                    args: vec![
                        Self::codegen_place_read_from(base, root),
                        Expr::Var(coordinate.clone()),
                        replacement,
                    ],
                };
                Self::codegen_place_update_from(base, updated, root)
            }
        }
    }

    fn codegen_place_coordinates(
        place: &CodegenPlace,
        out: &mut Vec<(String, Kind, ValType)>,
    ) {
        match place {
            CodegenPlace::Root(_) => {}
            CodegenPlace::Field { base, .. } => Self::codegen_place_coordinates(base, out),
            CodegenPlace::Index {
                base,
                coordinate,
                coordinate_kind,
                coordinate_type,
                ..
            } => {
                Self::codegen_place_coordinates(base, out);
                out.push((coordinate.clone(), *coordinate_kind, *coordinate_type));
            }
        }
    }

    fn lower_codegen_place_read(
        &mut self,
        place: &CodegenPlace,
        expected: Kind,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        match place {
            CodegenPlace::Root(root) => {
                let actual = self.locals.get(root).copied()?;
                Some(Self::wir_convert(W::GetLocal(root.clone()), actual, expected))
            }
            CodegenPlace::Field { .. } => {
                let expr = Self::codegen_place_read(place);
                let actual = self.kind_of(&expr);
                Some(Self::wir_convert(self.lower_expr(&expr)?, actual, expected))
            }
            CodegenPlace::Index {
                base,
                coordinate,
                coordinate_kind,
                dict: false,
                ..
            } => {
                let base_expr = Self::codegen_place_read(base);
                if let Some((_, _, element_kind)) = self
                    .ast_type_of_expr(&base_expr)
                    .as_ref()
                    .and_then(|ty| self.gc_reference_list_layout(ty))
                {
                    if element_kind != expected {
                        return None;
                    }
                    let target = Expr::Var(coordinate.clone());
                    self.locals.insert(coordinate.clone(), *coordinate_kind);
                    return self.lower_gc_function_list_at(&base_expr, &target);
                }
                let base = self.lower_codegen_place_read(base, Kind::I32)?;
                let value = W::Call {
                    func: "list_at".into(),
                    args: vec![base, W::GetLocal(coordinate.clone())],
                };
                Some(W::FromSlot(Box::new(value), Self::wir_kind(expected)))
            }
            CodegenPlace::Index { .. } => {
                let expr = Self::codegen_place_read(place);
                let actual = self.kind_of(&expr);
                Some(Self::wir_convert(self.lower_expr(&expr)?, actual, expected))
            }
        }
    }

    /// Lower an `var` user call. The callee returns `(declared, var_1, …)`;
    /// `CallStoreMulti` pops the results in reverse into `dests`, so dest[0] is a
    /// scratch holding the declared value and the rest are staged var results.
    /// Nested places rebuild into separate root scratches before any caller local
    /// commits, preserving all-or-nothing write-back when a projection traps.
    fn lower_var_call(
        &mut self,
        name: &str,
        emitted_name: &str,
        args: &[Expr],
        result_kind: Kind,
        access: &witchy_types::access::AccessSignature,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        let ownership = self.ownership_envelope_for_named_signature(name, access);
        let param_kinds: Vec<Kind> = self
            .fn_params
            .get(name)
            .map(|ps| {
                ps.iter()
                    .map(|p| p.ty.as_ref().map(|t| self.kind_for_type(t)).unwrap_or(Kind::I32))
                    .collect()
            })
            .unwrap_or_default();
        let mut args_w = Vec::with_capacity(args.len());
        let mut places = Vec::new();
        let mut next_coordinate = 0;
        for (i, arg) in args.iter().enumerate() {
            let is_var = access.params().get(i).is_some_and(|param| {
                param.kind() == witchy_types::access::AccessKind::ExclusiveWriteback
            });
            let ak = if is_var {
                param_kinds.get(i).copied().unwrap_or_else(|| self.kind_of(arg))
            } else {
                self.kind_of(arg)
            };
            let w = if is_var {
                let mut prelude = Vec::new();
                let place = self.capture_codegen_place(arg, &mut next_coordinate, &mut prelude)?;
                let mut coordinates = Vec::new();
                Self::codegen_place_coordinates(&place, &mut coordinates);
                for (coordinate, kind, value_type) in &coordinates {
                    self.locals.insert(coordinate.clone(), *kind);
                    self.local_val_types.insert(coordinate.clone(), *value_type);
                }
                let read = self.lower_codegen_place_read(&place, ak);
                for (coordinate, _, _) in &coordinates {
                    self.locals.remove(coordinate);
                    self.local_val_types.remove(coordinate);
                }
                let read = match read {
                    Some(read) => read,
                    None => {
                        if std::env::var_os("WIRDIAG").is_some() {
                            eprintln!("WIRBAIL var-place-read: callee={name} arg={arg:?}");
                        }
                        return None;
                    }
                };
                places.push((place, ak));
                if prelude.is_empty() {
                    read
                } else {
                    prelude.push(N::Push(read));
                    W::Seq(prelude)
                }
            } else {
                self.lower_expr(arg)?
            };
            args_w.push(match param_kinds.get(i) {
                Some(&pk) => Self::wir_convert(w, ak, pk),
                None => w,
            });
        }
        let cap_param_indices = ownership.var_capacity_params.clone();
        let mut cap_dests = Vec::with_capacity(cap_param_indices.len());
        for (ordinal, index) in cap_param_indices.iter().copied().enumerate() {
            let tracked_root = match args.get(index) {
                Some(Expr::Var(root)) if self.inplace_push.contains(root) => Some(root),
                _ => None,
            };
            args_w.push(match tracked_root {
                Some(root) => W::GetLocal(format!("{root}__cap")),
                None => W::ConstI32(0),
            });
            cap_dests.push(match tracked_root {
                Some(root) => format!("{root}__cap"),
                None => var_scratch("cap", ordinal, Kind::I32),
            });
        }
        // dest[0] = a kind-correct scratch for the declared return; then each var
        // arg's local. A single i32 tuple scratch is insufficient now that RFC-0087
        // admits independent scalar and reference returns from a `var` function.
        let result_tmp = call_result_tmp(result_kind);
        if places.len() > SCRUT_POOL {
            return None;
        }
        let mut dests = vec![result_tmp.clone()];
        if ownership.unique_capacity_result {
            dests.push(UNIQUE_RESULT_CAP_TMP.to_string());
        }
        for (index, (_, kind)) in places.iter().enumerate() {
            dests.push(var_scratch("result", index, *kind));
        }
        dests.extend(cap_dests);
        let mut seq = vec![N::CallStoreMulti {
            func: emitted_name.to_string(),
            args: args_w,
            dests,
        }];
        let mut groups: Vec<(String, Kind, Expr)> = Vec::new();
        let mut coordinates = Vec::new();
        for (index, (place, value_kind)) in places.iter().enumerate() {
            let result = var_scratch("result", index, *value_kind);
            let root = Self::codegen_place_root(place).to_string();
            let root_kind = self.locals.get(&root).copied()?;
            Self::codegen_place_coordinates(place, &mut coordinates);
            let root_value = groups
                .iter()
                .find(|(candidate, _, _)| candidate == &root)
                .map(|(_, _, value)| value.clone())
                .unwrap_or_else(|| Expr::Var(root.clone()));
            let update =
                Self::codegen_place_update_from(place, Expr::Var(result.clone()), &root_value);
            if let Some((_, _, value)) =
                groups.iter_mut().find(|(candidate, _, _)| candidate == &root)
            {
                *value = update;
            } else {
                groups.push((root, root_kind, update));
            }
        }
        coordinates.sort_by(|left, right| left.0.cmp(&right.0));
        coordinates.dedup_by(|left, right| left.0 == right.0);
        for (coordinate, kind, value_type) in &coordinates {
            self.locals.insert(coordinate.clone(), *kind);
            self.local_val_types.insert(coordinate.clone(), *value_type);
        }
        for (index, (_, value_kind)) in places.iter().enumerate() {
            self.locals
                .insert(var_scratch("result", index, *value_kind), *value_kind);
        }
        let mut commits = Vec::with_capacity(groups.len());
        for (index, (root, root_kind, update)) in groups.iter().enumerate() {
            let root_scratch = var_scratch("root", index, *root_kind);
            let update_w = self.lower_expr(update);
            let update_w = match update_w {
                Some(update) => update,
                None => {
                    if std::env::var_os("WIRDIAG").is_some() {
                        eprintln!("WIRBAIL var-place-update: callee={name} update={update:?}");
                    }
                    return None;
                }
            };
            seq.push(N::SetLocal { local: root_scratch.clone(), value: update_w });
            commits.push((root.clone(), root_scratch));
        }
        for (index, (_, value_kind)) in places.iter().enumerate() {
            self.locals.remove(&var_scratch("result", index, *value_kind));
        }
        for (coordinate, _, _) in &coordinates {
            self.locals.remove(coordinate);
            self.local_val_types.remove(coordinate);
        }
        for (root, scratch) in commits {
            seq.push(N::SetLocal { local: root, value: W::GetLocal(scratch) });
        }
        seq.push(N::Push(W::GetLocal(result_tmp)));
        Some(W::Seq(seq))
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
            if let Some(witchy_wir::wir::WirNode::Push(value)) = seq.last_mut() {
                let original =
                    std::mem::replace(value, witchy_wir::wir::WirExpr::ConstI32(0));
                *value = Self::wir_convert(original, from, to);
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

    fn zero_for_kind(kind: witchy_wir::wir::Kind) -> witchy_wir::wir::WirExpr {
        use witchy_wir::wir::WirExpr as W;
        match kind {
            witchy_wir::wir::Kind::I32 => W::ConstI32(0),
            witchy_wir::wir::Kind::I64 => W::ConstI64(0),
            witchy_wir::wir::Kind::F64 => W::ConstF64(0.0),
            reference @ (witchy_wir::wir::Kind::ExternRef
            | witchy_wir::wir::Kind::StructRef
            | witchy_wir::wir::Kind::GcRef(_)) => W::RefNull(reference),
        }
    }

    fn gc_ctor_value(
        &self,
        name: &str,
        owner: &Type,
        values: Vec<(witchy_wir::wir::WirExpr, Kind)>,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        let (layout, struct_id) = self.gc_layout_for_ctor(name, Some(owner))?;
        if layout.field_types.len() != values.len() {
            return None;
        }
        let mut fields = self
            .gc_structs
            .get(struct_id as usize)?
            .fields
            .iter()
            .copied()
            .map(Self::zero_for_kind)
            .collect::<Vec<_>>();
        if let Some(tag) = layout.tag {
            fields[0] = W::ConstI32(tag as i32);
        }
        for (index, ((value, actual), expected_ty)) in values
            .into_iter()
            .zip(&layout.field_types)
            .enumerate()
        {
            let expected = self.kind_for_type(expected_ty);
            if matches!(actual, Kind::ExternRef | Kind::GcRef(_))
                && actual != expected
            {
                return None;
            }
            fields[layout.field_base as usize + index] =
                Self::wir_convert(value, actual, expected);
        }
        Some(W::StructNew { struct_id, args: fields })
    }

    fn linear_ctor_value(
        &mut self,
        name: &str,
        values: Vec<(witchy_wir::wir::WirExpr, Kind)>,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        let &(tag, fields) = self.ctors.get(name)?;
        if fields != values.len() {
            return None;
        }
        self.mk_arities.insert(fields);
        let mut args = Vec::with_capacity(fields + 1);
        args.push(W::ConstI32(tag as i32));
        args.extend(values.into_iter().map(|(value, kind)| {
            W::ToSlot(Box::new(value), Self::wir_kind(kind))
        }));
        Some(W::Call { func: format!("mk{fields}"), args })
    }

    fn try_failure_value(
        &mut self,
        family: &str,
        error: Option<(witchy_wir::wir::WirExpr, Kind)>,
    ) -> Option<(witchy_wir::wir::WirExpr, Kind)> {
        use witchy_wir::wir::WirExpr as W;
        let destination = self.cur_fn_ret_ty.clone()?;
        let Type::Named(name, _) = destination.unqualified() else {
            return None;
        };
        if name != family {
            return None;
        }
        let destination_kind = self.kind_for_type(&destination);
        let value = match family {
            "Option" => {
                if self.option_reference_inner(&destination).is_some() {
                    W::RefNull(Self::wir_kind(destination_kind))
                } else if self
                    .gc_layout_for_ctor("None", Some(&destination))
                    .is_some()
                {
                    self.gc_ctor_value("None", &destination, Vec::new())?
                } else {
                    self.linear_ctor_value("None", Vec::new())?
                }
            }
            "Result" => {
                let error = error?;
                if self
                    .gc_layout_for_ctor("Err", Some(&destination))
                    .is_some()
                {
                    self.gc_ctor_value("Err", &destination, vec![error])?
                } else {
                    self.linear_ctor_value("Err", vec![error])?
                }
            }
            _ => return None,
        };
        Some((value, destination_kind))
    }

    fn try_early_return_nodes(
        &mut self,
        value: witchy_wir::wir::WirExpr,
        aggregate_kind: Kind,
    ) -> witchy_wir::wir::WirSeq {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        let active = self.active_loan_events.clone();
        if self.cur_fn_var_params.is_empty()
            && self.cur_fn_own_param.is_none()
            && !self.cur_fn_unique_ret
        {
            let result = if self.cur_fn_ret_slot {
                W::ToSlot(
                    Box::new(value),
                    Self::wir_kind(aggregate_kind),
                )
            } else {
                value
            };
            let mut nodes = self.close_loan_nodes(&active);
            nodes.push(N::Return(Some(result)));
            return nodes;
        }

        let result = if self.cur_fn_ret_slot {
            W::ToSlot(
                Box::new(value),
                Self::wir_kind(aggregate_kind),
            )
        } else {
            value
        };
        let mut nodes = self.close_loan_nodes(&active);
        nodes.push(N::Push(result));
        if self.cur_fn_unique_ret {
            nodes.push(N::Push(W::ConstI32(0)));
        }
        for name in &self.cur_fn_var_params {
            let var = W::GetLocal(name.clone());
            let var = if self.cur_fn_ret_slot {
                let kind = self.locals.get(name).copied().unwrap_or(Kind::I32);
                W::ToSlot(Box::new(var), Self::wir_kind(kind))
            } else {
                var
            };
            nodes.push(N::Push(var));
        }
        for name in &self.cur_fn_var_cap_params {
            nodes.push(N::Push(W::GetLocal(format!("{name}__cap"))));
        }
        if self.cur_fn_own_param.is_some() {
            nodes.push(N::Push(W::ConstI32(0)));
        }
        nodes.push(N::Return(None));
        nodes
    }

    /// Is `name` a plain function/body local — compiled to a bare `local.get`,
    /// not a top-level function used as a value? `lower_expr`'s `Expr::Var` arm
    /// lowers to `GetLocal` only for names that satisfy this exact predicate.
    fn is_plain_local_var(&self, name: &str) -> bool {
        self.locals.contains_key(name)
    }

    /// (BUG-414) Whether `e` is a bare reference to a top-level, capture-free
    /// function — the ONLY argument shape the `vm.*` worker-VM intercepts
    /// (`vm_par_map`/`vm_with_dir`/`vm_serve`) may take, because the host invokes it
    /// via the `__call_idx` export by TABLE INDEX with a null environment. A name is
    /// a top-level ref only when it is an actually-emitted function (`emitted_funcs`
    /// — it therefore has a table entry) AND is not shadowed by a local holding a
    /// function value. Any other argument (a local/param, a lambda, a call result, a
    /// non-function name) falls back to the sequential `vm.par_map` reference body;
    /// `vm.with_dir` and `vm.serve` reject it through the shared isolation contract.
    fn is_top_level_fn_ref(&self, e: &Expr) -> bool {
        matches!(e, Expr::Var(f) if self.emitted_funcs.contains(f) && !self.locals.contains_key(f))
    }

    fn lower_closure_code(&mut self, expr: &Expr) -> Option<witchy_wir::wir::WirExpr> {
        use witchy_wir::wir::WirExpr as W;
        Some(W::StructGet {
            struct_id: CLOSURE_WRAPPER_ID,
            field: witchy_wir::wir::CLOSURE_CODE_FIELD,
            base: Box::new(self.lower_expr(expr)?),
        })
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

    /// Lower a lambda to its uniform GC wrapper and typed capture payload,
    /// registering the lifted body `WirFunc` in `lambda_wir_funcs` once
    /// (idempotent by owner/content hash). `None` (the
    /// program is then rejected as unsupported) when the lambda assigns a
    /// captured var or its body doesn't fully lower.
    /// The source-owner/content hash keying a lambda's idempotent registration
    /// (and the `lambda_wir_index` lookup the devirt binding-recorder reuses to recover the
    /// `$__lamw{i}` index a `let f = <lambda>` was assigned). The owner is
    /// diagnostic identity: identical bodies in two functions carry different
    /// source function names and therefore cannot share one lifted body.
    fn lambda_content_key(owner: &str, params: &[Param], body: &Block) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        owner.hash(&mut h);
        format!("{params:?}{body:?}").hash(&mut h);
        h.finish()
    }

    fn capture_info(&self, name: &str) -> CaptureInfo {
        CaptureInfo {
            name: name.to_string(),
            kind: self.locals.get(name).copied().unwrap_or(Kind::I32),
            record: self.local_records.get(name).cloned(),
            list_elem: self.local_list_elem.get(name).cloned(),
            payload_record: self.local_payload_records.get(name).cloned(),
            val_type: self.local_val_types.get(name).copied(),
            ty: self.local_types.get(name).cloned(),
            list_elem_vt: self.local_list_elem_valtype.get(name).copied(),
            list_elem_tuple: self.local_list_elem_tuple.get(name).cloned(),
            tuple_slots: self.local_tuple_slots.get(name).cloned(),
            shape: self.local_shape.get(name).cloned(),
            payload_vt: self.local_payload_valtype.get(name).copied(),
            dict_value_vt: self.local_dict_value_valtype.get(name).copied(),
            dict_key_vt: self.local_dict_key_valtype.get(name).copied(),
            list_elem_list_vt: self.local_list_elem_list_valtype.get(name).copied(),
            list_nesting: self.local_list_nesting.get(name).cloned(),
            fn_ret_kind: self.local_fn_ret_kind.get(name).copied().or_else(|| {
                let Type::Fn(_, result, _) = self.local_types.get(name)?.unqualified() else {
                    return None;
                };
                Some(self.kind_for_type(result))
            }),
            fn_ownership: self.local_fn_ownership.get(name).cloned(),
        }
    }

    fn install_capture_info(&mut self, capture: &CaptureInfo) {
        let name = capture.name.clone();
        self.locals.insert(name.clone(), capture.kind);
        if let Some(value) = &capture.record {
            self.local_records.insert(name.clone(), value.clone());
        }
        if let Some(value) = &capture.list_elem {
            self.local_list_elem.insert(name.clone(), value.clone());
        }
        if let Some(value) = &capture.payload_record {
            self.local_payload_records.insert(name.clone(), value.clone());
        }
        if let Some(value) = capture.val_type {
            self.local_val_types.insert(name.clone(), value);
        }
        if let Some(value) = &capture.ty {
            self.local_types.insert(name.clone(), value.clone());
        }
        if let Some(value) = capture.list_elem_vt {
            self.local_list_elem_valtype.insert(name.clone(), value);
        }
        if let Some(value) = &capture.list_elem_tuple {
            self.local_list_elem_tuple.insert(name.clone(), value.clone());
        }
        if let Some(value) = &capture.tuple_slots {
            self.local_tuple_slots.insert(name.clone(), value.clone());
        }
        if let Some(value) = &capture.shape {
            self.local_shape.insert(name.clone(), value.clone());
        }
        if let Some(value) = capture.payload_vt {
            self.local_payload_valtype.insert(name.clone(), value);
        }
        if let Some(value) = capture.dict_value_vt {
            self.local_dict_value_valtype.insert(name.clone(), value);
        }
        if let Some(value) = capture.dict_key_vt {
            self.local_dict_key_valtype.insert(name.clone(), value);
        }
        if let Some(value) = capture.list_elem_list_vt {
            self.local_list_elem_list_valtype.insert(name.clone(), value);
        }
        if let Some(value) = &capture.list_nesting {
            self.local_list_nesting.insert(name.clone(), value.clone());
        }
        if let Some(value) = capture.fn_ret_kind {
            self.local_fn_ret_kind.insert(name.clone(), value);
        }
        if let Some(value) = &capture.fn_ownership {
            self.local_fn_ownership.insert(name, value.clone());
        }
    }

    fn lower_lambda(
        &mut self,
        params: &[Param],
        body: &Block,
        signature: &(Vec<Kind>, Kind),
        result_ty: Option<&Type>,
        access: Option<&witchy_types::access::AccessSignature>,
        ownership: &ClosureOwnershipEnvelope,
    ) -> Option<witchy_wir::wir::WirExpr> {
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
        let cap_info: Vec<CaptureInfo> = captures.iter().map(|c| self.capture_info(c)).collect();
        // Idempotent registration: the same lambda (by source owner and content)
        // gets one lifted body + one stable table index across lowering passes.
        let key = Self::lambda_content_key(&self.cur_fn_name, params, body);
        let env_struct_id = self.lambda_gc_env_ids.get(&key).copied();
        if !cap_info.is_empty() {
            let env_struct_id = env_struct_id?;
            let fields: Vec<_> = cap_info
                .iter()
                .map(|capture| Self::wir_kind(capture.kind))
                .collect();
            let slot = self.gc_structs.get_mut(env_struct_id as usize)?;
            if slot.fields.is_empty() {
                slot.fields = fields;
            } else if slot.fields != fields {
                self.reject_reason.get_or_insert_with(|| CodegenError {
                    message: "one closure source resolved to inconsistent GC capture layouts".into(),
                });
                return None;
            }
        }
        let index = if let Some(&i) = self.lambda_wir_index.get(&key) {
            i
        } else {
            let mut func = self.build_lambda_wir_func(
                params,
                body,
                &cap_info,
                CapMode::Env(env_struct_id),
                signature,
                LambdaContract { result_ty, access, ownership },
            )?;
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

        let gc_env = if cap_info.is_empty() {
            W::RefNull(witchy_wir::wir::Kind::StructRef)
        } else {
            W::StructNew {
                struct_id: env_struct_id?,
                args: cap_info
                    .iter()
                    .map(|capture| W::GetLocal(capture.name.clone()))
                    .collect(),
            }
        };
        Some(W::StructNew {
            struct_id: CLOSURE_WRAPPER_ID,
            args: vec![
                W::ConstI32(i32::try_from(
                    self.existential_table_len.checked_add(u32::try_from(index).ok()?)?
                ).ok()?),
                W::ConstI32(0),
                gc_env,
            ],
        })
    }

    /// (RFC-0062 tier-1) Register the THREADED lifted body of an ELIDED closure and
    /// return its ordered captures — NOTHING is emitted at the creation site (no `mk`
    /// env allocation), because the captures are threaded to each `call $__lamt{i}` from
    /// their existing locals. `None` (→ the caller falls back to the boxed `lower_lambda`)
    /// when the lambda assigns a captured var (can't thread a write-back), when any
    /// capture is REASSIGNED this unit (the interpreter snapshots captures at creation, so
    /// threading a mutated capture would diverge), or when the body doesn't lower. The
    /// caller has already checked `devirt_ok`/`closure_elide_called` (the escape fact).
    fn lower_lambda_threaded(
        &mut self,
        params: &[Param],
        body: &Block,
        signature: &(Vec<Kind>, Kind),
        result_ty: Option<&Type>,
        access: Option<&witchy_types::access::AccessSignature>,
        ownership: &ClosureOwnershipEnvelope,
    ) -> Option<ThreadedClosure> {
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
        let cap_info: Vec<CaptureInfo> = captures.iter().map(|c| self.capture_info(c)).collect();
        // Idempotent registration: an identical, same-owner elided lambda shares
        // one `$__lamt{i}`.
        let key = Self::lambda_content_key(&self.cur_fn_name, params, body);
        let index = if let Some(&i) = self.lambda_threaded_index.get(&key) {
            i
        } else {
            let mut func = self.build_lambda_wir_func(
                params,
                body,
                &cap_info,
                CapMode::Threaded,
                signature,
                LambdaContract { result_ty, access, ownership },
            )?;
            // Rename to the real push index (a nested lambda lowered during the build may
            // have shifted the length), mirroring `lower_lambda`.
            let i = self.lambda_wir_funcs.len();
            func.name = format!("__lamt{i}");
            self.lambda_wir_funcs.push(func);
            self.lambda_threaded_index.insert(key, i);
            i
        };
        Some((index, cap_info.into_iter().map(|c| (c.name, c.kind)).collect()))
    }

    /// Build the lifted `WirFunc` for a lambda: the capture-passing prefix (per
    /// `cap_mode`) then one value parameter per lambda parameter. Scalar signatures use
    /// universal slots; reference-bearing signatures preserve exact WIR kinds. `None`
    /// means the body does not lower. The enclosing scope is restored after lowering.
    ///
    /// (RFC-0062) `cap_mode` selects how captures reach the body:
    /// - `CapMode::Env` (tier-3, the default): an env-pointer first param `$__lamw{i}`;
    ///   the prologue loads each capture from the heap env record.
    /// - `CapMode::Threaded` (tier-1, elided closure): captures are leading value
    ///   params to `$__lamt{i}`; reference captures retain their exact kind and
    ///   scalar captures use slots. The creating site allocates no environment.
    fn build_lambda_wir_func(
        &mut self,
        params: &[Param],
        body: &Block,
        cap_info: &[CaptureInfo],
        cap_mode: CapMode,
        signature: &(Vec<Kind>, Kind),
        contract: LambdaContract<'_>,
    ) -> Option<witchy_wir::wir::WirFunc> {
        use witchy_wir::wir::{WirExpr as W, WirFunc, WirLocal, WirNode as N, WirTy};
        let LambdaContract { result_ty, access, ownership } = contract;
        let index = self.lambda_wir_funcs.len();
        let saved = self.swap_out_scope();
        self.cur_fn_var_params = params
            .iter()
            .enumerate()
            .filter(|(index, param)| {
                access.map_or(param.convention == Convention::Var, |signature| {
                    signature.params().get(*index).is_some_and(|access| {
                        access.kind()
                            == witchy_types::access::AccessKind::ExclusiveWriteback
                    })
                })
            })
            .map(|(_, p)| p.name.clone())
            .collect();
        self.cur_fn_var_cap_params = ownership
            .var_capacity_params
            .iter()
            .filter_map(|index| params.get(*index).map(|param| param.name.clone()))
            .collect();
        let lambda_own_param = ownership
            .own_capacity_param
            .and_then(|index| params.get(index))
            .map(|param| param.name.clone());
        self.cur_fn_var = !self.cur_fn_var_params.is_empty();
        // Install declared parameter shape metadata. Exact runtime kinds are
        // replaced from the checker-resolved function signature below.
        for (index, p) in params.iter().enumerate() {
            let resolved_type = access
                .and_then(|signature| signature.params().get(index))
                .map(witchy_types::access::AccessParam::ty)
                .or(p.ty.as_ref());
            self.locals.insert(p.name.clone(), Kind::I32);
            if let Some(t) = resolved_type {
                self.local_val_types.insert(p.name.clone(), ty_to_valtype(t));
                self.local_types.insert(p.name.clone(), t.clone());
            }
            match resolved_type.map(Type::unqualified) {
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
        for capture in cap_info {
            self.install_capture_info(capture);
        }
        for (index, p) in params.iter().enumerate() {
            let resolved_type = access
                .and_then(|signature| signature.params().get(index))
                .map(witchy_types::access::AccessParam::ty)
                .or(p.ty.as_ref());
            let k = resolved_type
                .map(|ty| self.kind_for_type(ty))
                .unwrap_or(Kind::I32);
            self.locals.insert(p.name.clone(), k);
            if let Some(ty) = resolved_type
                && let Type::Fn(_, ret, _) = ty.unqualified()
            {
                self.local_fn_ret_kind.insert(p.name.clone(), self.kind_for_type(ret));
                let envelope = Self::ownership_envelope_for_type(ty);
                if envelope.has_state() {
                    self.local_fn_ownership.insert(p.name.clone(), envelope);
                }
            }
        }
        self.infer_locals(body);
        let block_kind = signature.1;
        let param_kinds = &signature.0;
        for (param, kind) in params.iter().zip(param_kinds) {
            self.locals.insert(param.name.clone(), *kind);
        }
        let typed_abi = Self::closure_uses_typed_abi(param_kinds, block_kind);
        let saved_inplace = std::mem::take(&mut self.inplace_push);
        let saved_own = self.cur_fn_own_param.take();
        self.cur_fn_own_param = lambda_own_param.clone();
        self.begin_unit(body);
        self.cur_fn_ret_kind = if typed_abi { block_kind } else { Kind::I64 };
        self.cur_fn_ret_ty = access
            .map(|signature| signature.result().ty().clone())
            .or_else(|| result_ty.cloned());
        self.cur_fn_ret_slot = !typed_abi;
        self.cur_fn_unique_ret = ownership.unique_capacity_result;
        let saved_apply = self.apply_level;
        let saved_existential_call = self.existential_call_level;
        let saved_assign = self.assign_level;
        let saved_wm = self.wm_level;
        self.apply_level = 0;
        self.existential_call_level = 0;
        self.assign_level = 0;
        self.wm_level = 0;
        let body_res = self.lower_block(body);
        // The lambda's OWN in-place accumulators (`var acc = []` + a self-push loop
        // inside the lambda body) — snapshot before restoring the outer function's
        // set, so the cap-shadow `${v}__cap` locals below are declared for the
        // lambda's accumulators, not the enclosing function's.
        let lambda_inplace = self.inplace_push.clone();
        let lambda_tail_capacity = self.cur_fn_unique_ret.then(|| {
            body.stmts
                .last()
                .and_then(|stmt| match stmt {
                    Stmt::Expr(expr) => Some(self.return_capacity_expr(expr)),
                    _ => None,
                })
                .unwrap_or(W::ConstI32(0))
        });
        let lambda_own_tail_capacity = lambda_own_param.as_ref().map(|own| {
            match body.stmts.last() {
                Some(Stmt::Expr(Expr::Var(value))) if value == own => {
                    W::GetLocal(format!("{own}__cap"))
                }
                Some(Stmt::Expr(Expr::Unary { op: UnOp::Move, expr }))
                    if matches!(expr.as_ref(), Expr::Var(value) if value == own) =>
                {
                    W::GetLocal(format!("{own}__cap"))
                }
                Some(Stmt::Expr(Expr::Call { .. })) => {
                    W::GetLocal("__witchy_owncap".to_string())
                }
                _ => W::ConstI32(0),
            }
        });
        self.apply_level = saved_apply;
        self.existential_call_level = saved_existential_call;
        self.assign_level = saved_assign;
        self.wm_level = saved_wm;
        let fin = if self.collect_wir && body_res.is_none() {
            self.abort_unit("lambda")
        } else {
            self.finish_unit("lambda")
        };
        self.inplace_push = saved_inplace;
        self.cur_fn_own_param = saved_own;

        let func = match (body_res, fin) {
            (Some(seq), Ok(())) => {
                let i32t = || WirTy::Bool;
                let mut unit_gc_ids = self.unit_gc_ids(
                    params.iter().filter_map(|param| param.ty.clone()),
                    None,
                    body,
                );
                for kind in param_kinds
                    .iter()
                    .copied()
                    .chain(std::iter::once(block_kind))
                    .chain(cap_info.iter().map(|capture| capture.kind))
                {
                    if let Kind::GcRef(id) = kind {
                        unit_gc_ids.insert(id);
                    }
                }
                // Env mode receives the uniform wrapper. Threaded mode keeps scalar
                // captures in slots but carries reference captures at their exact kind.
                let mut func_params = match cap_mode {
                    CapMode::Env(_) => vec![WirLocal {
                        name: ENV_PARAM.into(),
                        ty: WirTy::GcRef(CLOSURE_WRAPPER_ID),
                    }],
                    CapMode::Threaded => cap_info
                        .iter()
                        .map(|capture| WirLocal {
                            name: format!("__cap_{}", capture.name),
                            ty: if capture.kind.is_ref() {
                                Self::wir_ty_for_kind(capture.kind)
                            } else {
                                WirTy::Int
                            },
                        })
                        .collect(),
                };
                for p in params {
                    let kind = self.locals.get(&p.name).copied().unwrap_or(Kind::I32);
                    func_params.push(WirLocal {
                        name: format!("__lp_{}", p.name),
                        ty: if typed_abi {
                            Self::wir_ty_for_kind(kind)
                        } else {
                            WirTy::Int
                        },
                    });
                }
                if let Some(p) = &lambda_own_param {
                    func_params.push(WirLocal {
                        name: format!("{p}__cap"),
                        ty: i32t(),
                    });
                }
                for p in &self.cur_fn_var_cap_params {
                    func_params.push(WirLocal {
                        name: format!("{p}__cap"),
                        ty: i32t(),
                    });
                }
                let mut locals: Vec<WirLocal> = Vec::new();
                for p in params {
                    let k = self.locals.get(&p.name).copied().unwrap_or(Kind::I32);
                    locals.push(WirLocal { name: p.name.clone(), ty: Self::wir_ty_for_kind(k) });
                }
                for capture in cap_info {
                    locals.push(WirLocal {
                        name: capture.name.clone(),
                        ty: Self::wir_ty_for_kind(capture.kind),
                    });
                }
                let mut lets = Vec::new();
                collect_let_names(body, &mut lets);
                lets.sort();
                lets.dedup();
                for name in &lets {
                    let k = self.locals.get(name).copied().unwrap_or(Kind::I32);
                    locals.push(WirLocal { name: name.clone(), ty: Self::wir_ty_for_kind(k) });
                }
                let mut scalar_sums: Vec<(&String, &ScalarSumLayout)> =
                    self.scalar_sum_active.iter().collect();
                scalar_sums.sort_by(|left, right| left.0.cmp(right.0));
                for (name, layout) in scalar_sums {
                    locals.push(WirLocal {
                        name: scalar_sum_tag_local(name),
                        ty: i32t(),
                    });
                    for index in 0..layout.max_arity {
                        locals.push(WirLocal {
                            name: scalar_sum_payload_local(name, index),
                            ty: WirTy::Int,
                        });
                    }
                }
                let mut loan_roots = Vec::new();
                if let Err(error) = collect_loan_roots(body, &self.loan_facts, &mut loan_roots) {
                    self.reject_reason.get_or_insert(error);
                }
                loan_roots.sort_by(|a, b| a.local.cmp(&b.local));
                loan_roots.dedup_by(|a, b| a.local == b.local);
                for root in loan_roots {
                    locals.push(WirLocal { name: root.local, ty: i32t() });
                }
                let mut cap_vars: Vec<&String> = lambda_inplace.iter().collect();
                cap_vars.sort();
                for v in cap_vars {
                    if self.cur_fn_var_cap_params.contains(v)
                        || lambda_own_param.as_ref() == Some(v)
                    {
                        continue;
                    }
                    locals.push(WirLocal { name: format!("{v}__cap"), ty: i32t() });
                }
                // (RFC-0033 R2) field-buffer capacity tokens for in-place field-path pushes.
                let mut field_caps: Vec<&String> = self.field_caps.iter().collect();
                field_caps.sort();
                for fc in field_caps {
                    locals.push(WirLocal { name: fc.clone(), ty: i32t() });
                }
                locals.push(WirLocal { name: "__witchy_owncap".into(), ty: i32t() });
                locals.push(WirLocal { name: UNIQUE_RESULT_CAP_TMP.into(), ty: i32t() });
                locals.push(WirLocal { name: TUPLE_TMP.into(), ty: i32t() });
                locals.push(WirLocal { name: CALL_RESULT_I32_TMP.into(), ty: i32t() });
                locals.push(WirLocal { name: CALL_RESULT_I64_TMP.into(), ty: WirTy::Int });
                locals.push(WirLocal { name: CALL_RESULT_F64_TMP.into(), ty: WirTy::Float });
                locals.push(WirLocal { name: CALL_RESULT_EXTERN_TMP.into(), ty: WirTy::Extern });
                for &id in &unit_gc_ids {
                    locals.push(WirLocal { name: call_result_gc_tmp(id), ty: WirTy::GcRef(id) });
                    locals.push(WirLocal { name: match_gc_tmp(id), ty: WirTy::GcRef(id) });
                    locals.push(WirLocal { name: update_gc_tmp(id), ty: WirTy::GcRef(id) });
                }
                locals.push(WirLocal { name: TRY_TMP.into(), ty: i32t() });
                locals.push(WirLocal { name: MATCH_TMP.into(), ty: WirTy::Int });
                locals.push(WirLocal { name: MATCH_REF_TMP.into(), ty: WirTy::Extern });
                locals.push(WirLocal { name: MATCH_RES.into(), ty: WirTy::Int });
                for i in 0..SCRUT_POOL {
                    locals.push(WirLocal { name: format!("__witchy_scrut_save_{i}"), ty: WirTy::Int });
                    locals.push(WirLocal { name: assign_scratch("list", i), ty: i32t() });
                    locals.push(WirLocal { name: assign_scratch("index", i), ty: WirTy::Int });
                    locals.push(WirLocal { name: assign_scratch("value", i), ty: WirTy::Int });
                    for prefix in ["coord", "result", "root", "cap"] {
                        locals.push(WirLocal {
                            name: var_scratch(prefix, i, Kind::I32),
                            ty: i32t(),
                        });
                        locals.push(WirLocal {
                            name: var_scratch(prefix, i, Kind::I64),
                            ty: WirTy::Int,
                        });
                        locals.push(WirLocal {
                            name: var_scratch(prefix, i, Kind::F64),
                            ty: WirTy::Float,
                        });
                        locals.push(WirLocal {
                            name: var_scratch(prefix, i, Kind::ExternRef),
                            ty: WirTy::Extern,
                        });
                        for &id in &unit_gc_ids {
                            locals.push(WirLocal {
                                name: var_scratch(prefix, i, Kind::GcRef(id)),
                                ty: WirTy::GcRef(id),
                            });
                        }
                    }
                }
                locals.push(WirLocal { name: SECRET_TMP.into(), ty: WirTy::Extern });
                locals.push(WirLocal { name: SECRET_NAME_TMP.into(), ty: i32t() });
                locals.push(WirLocal { name: ABORT_STR_TMP.into(), ty: i32t() });
                // Scratch slots for the inlined in-place set_at/push fast path (a
                // self-assign accumulator can live inside a lifted lambda body too).
                locals.push(WirLocal { name: "__witchy_set_idx".into(), ty: i32t() });
                locals.push(WirLocal { name: "__witchy_set_val".into(), ty: WirTy::Int });
                locals.push(WirLocal { name: "__rc_new".into(), ty: WirTy::Int });
                locals.push(WirLocal {
                    name: DESTINATION_RESULT_TMP.into(),
                    ty: i32t(),
                });
                for i in 0..WM_POOL {
                    locals.push(WirLocal { name: format!("__witchy_wm_{i}"), ty: i32t() });
                    locals.push(WirLocal {
                        name: Self::counter_batch_local("destination", i),
                        ty: WirTy::Int,
                    });
                    locals.push(WirLocal {
                        name: Self::counter_batch_local("rewind", i),
                        ty: WirTy::Int,
                    });
                }
                for i in 0..APPLY_POOL {
                    locals.push(WirLocal {
                        name: format!("__witchy_call_{i}"),
                        ty: WirTy::GcRef(CLOSURE_WRAPPER_ID),
                    });
                }
                for i in 0..EXISTENTIAL_CALL_POOL {
                    locals.push(WirLocal {
                        name: existential_call_scratch(i),
                        ty: WirTy::GcRef(EXISTENTIAL_WRAPPER_ID),
                    });
                }
                for (id, element_kind) in self
                    .gc_reference_list_layouts()
                    .into_iter()
                    .filter(|(id, _)| unit_gc_ids.contains(id))
                {
                    for level in 0..APPLY_POOL {
                        locals.push(WirLocal {
                            name: gc_list_scratch(GC_LIST_SRC_TMP, level, id),
                            ty: WirTy::GcRef(id),
                        });
                        locals.push(WirLocal {
                            name: gc_list_scratch(GC_LIST_RIGHT_TMP, level, id),
                            ty: WirTy::GcRef(id),
                        });
                        locals.push(WirLocal {
                            name: gc_list_scratch(GC_LIST_DST_TMP, level, id),
                            ty: WirTy::GcRef(id),
                        });
                        locals.push(WirLocal {
                            name: gc_list_scratch(GC_LIST_VALUE_TMP, level, id),
                            ty: Self::wir_ty_for_kind(element_kind),
                        });
                        locals.push(WirLocal {
                            name: gc_list_scratch(GC_LIST_LEN_TMP, level, id),
                            ty: i32t(),
                        });
                        locals.push(WirLocal {
                            name: gc_list_scratch(GC_LIST_LEFT_LEN_TMP, level, id),
                            ty: i32t(),
                        });
                        locals.push(WirLocal {
                            name: gc_list_scratch(GC_LIST_INDEX_TMP, level, id),
                            ty: i32t(),
                        });
                        locals.push(WirLocal {
                            name: gc_list_scratch(GC_LIST_TARGET_TMP, level, id),
                            ty: i32t(),
                        });
                        locals.push(WirLocal {
                            name: gc_list_scratch(GC_LIST_RAW_INDEX_TMP, level, id),
                            ty: WirTy::Int,
                        });
                    }
                }
                for i in 0..REUSE_POOL {
                    locals.push(WirLocal { name: format!("__witchy_reuse_{i}"), ty: WirTy::Int });
                }
                let mut destination_scratches: Vec<(String, LayoutId)> = self
                    .destination_scratch_sites
                    .values()
                    .cloned()
                    .collect();
                destination_scratches.sort_by(|left, right| left.0.cmp(&right.0));
                destination_scratches.dedup();
                for (name, _) in &destination_scratches {
                    locals.push(WirLocal {
                        name: name.clone(),
                        ty: i32t(),
                    });
                }
                // Prologue: recover parameters, then captures from the lambda's typed
                // GC payload or from direct threaded parameters.
                let mut nodes: witchy_wir::wir::WirSeq = Vec::new();
                for p in params {
                    let k = self.locals.get(&p.name).copied().unwrap_or(Kind::I32);
                    nodes.push(N::SetLocal {
                        local: p.name.clone(),
                        value: if typed_abi {
                            W::GetLocal(format!("__lp_{}", p.name))
                        } else {
                            W::FromSlot(
                                Box::new(W::GetLocal(format!("__lp_{}", p.name))),
                                Self::wir_kind(k),
                            )
                        },
                    });
                }
                for (j, capture) in cap_info.iter().enumerate() {
                    let value = match cap_mode {
                        CapMode::Env(env_struct_id) => {
                            let env_struct_id = env_struct_id?;
                            let erased = W::StructGet {
                                struct_id: CLOSURE_WRAPPER_ID,
                                field: witchy_wir::wir::CLOSURE_GC_ENV_FIELD,
                                base: Box::new(W::GetLocal(ENV_PARAM.into())),
                            };
                            W::StructGet {
                                struct_id: env_struct_id,
                                field: j as u32,
                                base: Box::new(W::RefCast {
                                    struct_id: env_struct_id,
                                    value: Box::new(erased),
                                }),
                            }
                        }
                        CapMode::Threaded if capture.kind.is_ref() => {
                            W::GetLocal(format!("__cap_{}", capture.name))
                        }
                        CapMode::Threaded => W::FromSlot(
                            Box::new(W::GetLocal(format!("__cap_{}", capture.name))),
                            Self::wir_kind(capture.kind),
                        ),
                    };
                    nodes.push(N::SetLocal {
                        local: capture.name.clone(),
                        value,
                    });
                }
                nodes.extend(destination_scratches.into_iter().map(|(local, id)| {
                    N::SetLocal {
                        local,
                        value: W::Call {
                            func: Self::layout_helper_name("destination_scratch", id, None),
                            args: Vec::new(),
                        },
                    }
                }));
                // Body, with the declared result and every `var` final value emitted
                // in the closure signature's selected representation.
                let mut seq = seq;
                // An explicit terminal return already emitted the complete
                // multi-result tuple and a bare WIR return. Appending only the
                // var/cap suffix afterward leaves a partial function-end stack
                // and makes validation expect the missing primary result.
                let terminal_return = matches!(body.stmts.last(), Some(Stmt::Return(_)));
                if !terminal_return {
                    if !typed_abi && let Some(N::Push(v)) = seq.pop() {
                        seq.push(N::Push(W::ToSlot(Box::new(v), Self::wir_kind(block_kind))));
                    }
                    if self.cur_fn_unique_ret {
                        seq.push(N::Push(
                            lambda_tail_capacity
                                .clone()
                                .unwrap_or(W::ConstI32(0)),
                        ));
                    }
                    for name in &self.cur_fn_var_params {
                        let kind = self.locals.get(name).copied().unwrap_or(Kind::I32);
                        let value = W::GetLocal(name.clone());
                        seq.push(N::Push(if typed_abi {
                            value
                        } else {
                            W::ToSlot(Box::new(value), Self::wir_kind(kind))
                        }));
                    }
                    for name in &self.cur_fn_var_cap_params {
                        seq.push(N::Push(W::GetLocal(format!("{name}__cap"))));
                    }
                    if lambda_own_param.is_some() {
                        seq.push(N::Push(
                            lambda_own_tail_capacity
                                .clone()
                                .unwrap_or(W::ConstI32(0)),
                        ));
                    }
                }
                nodes.extend(seq);
                let name = match cap_mode {
                    CapMode::Env(_) => format!("__lamw{index}"),
                    CapMode::Threaded => format!("__lamt{index}"),
                };
                Some(WirFunc {
                    name,
                    params: func_params,
                    ret: if typed_abi {
                        {
                            let mut ret = vec![Self::wir_ty_for_kind(block_kind)];
                            if self.cur_fn_unique_ret {
                                ret.push(i32t());
                            }
                            ret.extend(self.cur_fn_var_params.iter().map(|name| {
                                Self::wir_ty_for_kind(
                                    self.locals.get(name).copied().unwrap_or(Kind::I32),
                                )
                            }));
                            ret.extend(self.cur_fn_var_cap_params.iter().map(|_| i32t()));
                            if lambda_own_param.is_some() {
                                ret.push(i32t());
                            }
                            ret
                        }
                    } else {
                        let mut ret = vec![WirTy::Int];
                        if self.cur_fn_unique_ret {
                            ret.push(i32t());
                        }
                        ret.extend(std::iter::repeat_n(
                            WirTy::Int,
                            self.cur_fn_var_params.len(),
                        ));
                        ret.extend(self.cur_fn_var_cap_params.iter().map(|_| i32t()));
                        if lambda_own_param.is_some() {
                            ret.push(i32t());
                        }
                        ret
                    },
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
            field_caps: std::mem::take(&mut self.field_caps),
            field_push_safe: std::mem::take(&mut self.field_push_safe),
            records: std::mem::take(&mut self.local_records),
            list_elem: std::mem::take(&mut self.local_list_elem),
            payload: std::mem::take(&mut self.local_payload_records),
            val_types: std::mem::take(&mut self.local_val_types),
            types: std::mem::take(&mut self.local_types),
            list_elem_vt: std::mem::take(&mut self.local_list_elem_valtype),
            list_elem_tuple: std::mem::take(&mut self.local_list_elem_tuple),
            tuple_slots: std::mem::take(&mut self.local_tuple_slots),
            shape: std::mem::take(&mut self.local_shape),
            payload_vt: std::mem::take(&mut self.local_payload_valtype),
            dict_value_vt: std::mem::take(&mut self.local_dict_value_valtype),
            dict_key_vt: std::mem::take(&mut self.local_dict_key_valtype),
            list_elem_list_vt: std::mem::take(&mut self.local_list_elem_list_valtype),
            list_nesting: std::mem::take(&mut self.local_list_nesting),
            fn_ret_kind: std::mem::take(&mut self.local_fn_ret_kind),
            fn_ownership: std::mem::take(&mut self.local_fn_ownership),
            ret: self.cur_fn_ret_kind,
            ret_ty: self.cur_fn_ret_ty.take(),
            ret_slot: self.cur_fn_ret_slot,
            unique_ret: self.cur_fn_unique_ret,
            destination_forward_vars: std::mem::take(&mut self.destination_forward_vars),
            destination_scratch_sites: std::mem::take(&mut self.destination_scratch_sites),
            var: self.cur_fn_var,
            var_params: std::mem::take(&mut self.cur_fn_var_params),
            var_cap_params: std::mem::take(&mut self.cur_fn_var_cap_params),
            sroa_candidates: std::mem::take(&mut self.sroa_candidates),
            sroa_active: std::mem::take(&mut self.sroa_active),
            scalar_sum_candidates: std::mem::take(&mut self.scalar_sum_candidates),
            scalar_sum_active: std::mem::take(&mut self.scalar_sum_active),
            scalar_sum_fused_values: std::mem::take(&mut self.scalar_sum_fused_values),
            scalar_record_call_candidates: std::mem::take(
                &mut self.scalar_record_call_candidates,
            ),
            direct_list_builder_lets: std::mem::take(&mut self.direct_list_builder_lets),
            direct_list_builder_loops: std::mem::take(&mut self.direct_list_builder_loops),
            active_direct_list_builder: self.active_direct_list_builder.take(),
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
        self.field_caps = s.field_caps;
        self.field_push_safe = s.field_push_safe;
        self.local_records = s.records;
        self.local_list_elem = s.list_elem;
        self.local_payload_records = s.payload;
        self.local_val_types = s.val_types;
        self.local_types = s.types;
        self.local_list_elem_valtype = s.list_elem_vt;
        self.local_list_elem_tuple = s.list_elem_tuple;
        self.local_tuple_slots = s.tuple_slots;
        self.local_shape = s.shape;
        self.local_payload_valtype = s.payload_vt;
        self.local_dict_value_valtype = s.dict_value_vt;
        self.local_dict_key_valtype = s.dict_key_vt;
        self.local_list_elem_list_valtype = s.list_elem_list_vt;
        self.local_list_nesting = s.list_nesting;
        self.local_fn_ret_kind = s.fn_ret_kind;
        self.local_fn_ownership = s.fn_ownership;
        self.cur_fn_ret_kind = s.ret;
        self.cur_fn_ret_ty = s.ret_ty;
        self.cur_fn_ret_slot = s.ret_slot;
        self.cur_fn_unique_ret = s.unique_ret;
        self.destination_forward_vars = s.destination_forward_vars;
        self.destination_scratch_sites = s.destination_scratch_sites;
        self.cur_fn_var = s.var;
        self.cur_fn_var_params = s.var_params;
        self.cur_fn_var_cap_params = s.var_cap_params;
        self.sroa_candidates = s.sroa_candidates;
        self.sroa_active = s.sroa_active;
        self.scalar_sum_candidates = s.scalar_sum_candidates;
        self.scalar_sum_active = s.scalar_sum_active;
        self.scalar_sum_fused_values = s.scalar_sum_fused_values;
        self.scalar_record_call_candidates = s.scalar_record_call_candidates;
        self.direct_list_builder_lets = s.direct_list_builder_lets;
        self.direct_list_builder_loops = s.direct_list_builder_loops;
        self.active_direct_list_builder = s.active_direct_list_builder;
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

    /// The scalar `$key_eq` comparison mode for a Dict key expression: 0 for
    /// Int/Bool (i64 bit equality), 1 for String/Bytes (`$str_eq`), 2 for Float
    /// (`f64.eq` on the reinterpreted slot). Float reaches this only in
    /// already-lowered/internal code; the public checker rejects Float dict keys
    /// because Float is not Eq.
    fn scalar_dict_key_mode(&self, key: &Expr) -> Option<u32> {
        match self.val_type_of(key) {
            ValType::Int | ValType::Bool => Some(0),
            ValType::Str | ValType::Bytes => Some(1),
            ValType::Float => Some(2),
            ValType::Other => None,
        }
    }

    fn dict_key_mode_error() -> CodegenError {
        CodegenError {
            message: "could not determine the Dict key type for WASM; use Int, Bool, Duration, String, Bytes, or a resolved Eq compound key (annotate if needed)".to_string(),
        }
    }

    fn shape_has_eq_impl(&self, shape: &EqShape) -> bool {
        match shape {
            EqShape::Int | EqShape::Bool | EqShape::Str | EqShape::Bytes => true,
            EqShape::Float => false,
            EqShape::Record(name)
            | EqShape::RecInst(name, _)
            | EqShape::Adt(name)
            | EqShape::AdtInst(name, _)
            | EqShape::AdtRec(name, _) => self.eq_types.contains(name),
            EqShape::List(elem) => self.eq_types.contains("List") && self.shape_has_eq_impl(elem),
            EqShape::Dict(k, v) => {
                self.eq_types.contains("Dict")
                    && self.shape_has_eq_impl(k)
                    && self.shape_has_eq_impl(v)
            }
            EqShape::Tuple(fields) => fields.iter().all(|f| self.shape_has_eq_impl(f)),
        }
    }

    /// `dict_key_mode` for the WIR path: an undetermined key type is a HARD
    /// rejection (a dict needs a comparable key), so record it as a `reject_reason`
    /// — `compile_function` turns that into a diagnostic `Err` rather than letting
    /// the function silently bail as "unsupported".
    fn dict_key_mode_wir(&mut self, key: &Expr) -> Option<u32> {
        if let Some(mode) = self.scalar_dict_key_mode(key) {
            return Some(mode);
        }
        let Some(shape) = self.eq_operand_shape(key) else {
            self.reject_reason.get_or_insert_with(Self::dict_key_mode_error);
            return None;
        };
        match shape {
            EqShape::Int | EqShape::Bool => Some(0),
            EqShape::Str | EqShape::Bytes => Some(1),
            EqShape::Float => Some(2),
            shape => {
                if !self.shape_has_eq_impl(&shape) {
                    self.reject_reason.get_or_insert_with(Self::dict_key_mode_error);
                    return None;
                }
                if self.custom_eq_type_of_shape(&shape).is_none()
                    && self.ensure_eq_wir_helper(&shape).is_none()
                {
                    self.reject_reason.get_or_insert_with(Self::dict_key_mode_error);
                    return None;
                }
                let id = shape.id();
                if let Some(mode) = self.dict_key_shape_modes.get(&id).copied() {
                    return Some(mode);
                }
                let mode = 3 + self.dict_key_shapes.len() as u32;
                self.dict_key_shapes.insert(mode, shape);
                self.dict_key_shape_modes.insert(id, mode);
                Some(mode)
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
    /// garbage is reclaimed. `None` when the whole body is proven allocation-free,
    /// the body isn't arena-resettable, or the pool is exhausted. In each case the
    /// loop lowers without a reset; the allocation-free case also avoids all
    /// per-iteration watermark and counter traffic. Bumps `wm_level`; the caller
    /// decrements it once the body is lowered.
    fn loop_watermark_wir(
        &mut self,
        body: &Block,
    ) -> Option<(witchy_wir::wir::WirNode, witchy_wir::wir::WirSeq)> {
        // Gated on the `region` optimization (RFC-0030): `WITCHY_OPT=-region` (or
        // `none`) drops the per-iteration reset so the loop's arena garbage leaks —
        // correct, just unbounded — which is exactly the regression the soak test
        // and the differential sweep guard against.
        if force_copy_mode()
            || !witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::Region)
            || self.wm_level >= WM_POOL
            || !self.loop_arena_resettable(body)
            || !self.summaries.block_may_allocate(body)
            || self.destination_allocation_free_block(body)
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
        let reset = vec![
            self.increment_hot_counter("__witchy_region_rewind_calls"),
            witchy_wir::wir::WirNode::SetGlobal {
                global: "heap".into(),
                value: witchy_wir::wir::WirExpr::GetLocal(wm),
            },
        ];
        Some((capture, reset))
    }

    fn increment_counter(name: &str) -> witchy_wir::wir::WirNode {
        use witchy_wir::wir::{BinOp, Kind, WirExpr as W, WirNode as N};
        N::SetGlobal {
            global: name.into(),
            value: W::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(W::GetGlobal(name.into())),
                rhs: Box::new(W::ConstI64(1)),
            },
        }
    }

    /// (RFC-0110 step 5) If `call` is a normal-mode one-copy repair site, wrap
    /// its lowered value so the boundary-reown + ownership-token counters
    /// increment once, at runtime, exactly when the repaired call executes (a
    /// repair inside an unexecuted branch does not count — the increment sits in
    /// the value's own sequence). Non-repair calls are returned unchanged. The
    /// membership is lever-independent (`boundary_repair_sites` is derived from
    /// the checked access graph), satisfying criterion 9's lever invariance.
    fn count_boundary_repair(
        &self,
        call: &Expr,
        lowered: witchy_wir::wir::WirExpr,
    ) -> witchy_wir::wir::WirExpr {
        use witchy_wir::wir::{WirExpr as W, WirNode as N};
        if !self.boundary_repair_sites.contains(&(call as *const Expr as usize)) {
            return lowered;
        }
        W::Seq(vec![
            Self::increment_counter("__witchy_boundary_reown_copies"),
            Self::increment_counter("__witchy_ownership_token_repairs"),
            N::Push(lowered),
        ])
    }

    fn counter_batch_local(kind: &str, level: usize) -> String {
        format!("__witchy_counter_batch_{kind}_{level}")
    }

    fn increment_hot_counter(&mut self, name: &str) -> witchy_wir::wir::WirNode {
        use witchy_wir::wir::{BinOp, Kind, WirExpr as W, WirNode as N};
        let kind = match name {
            "__witchy_destination_candidates_forwarded" => "destination",
            "__witchy_region_rewind_calls" => "rewind",
            _ => return Self::increment_counter(name),
        };
        let Some(level) = self.counter_batch_stack.last().copied() else {
            return Self::increment_counter(name);
        };
        if let Some((destination, rewind)) = self.counter_batch_used.last_mut() {
            match kind {
                "destination" => *destination = true,
                "rewind" => *rewind = true,
                _ => unreachable!(),
            }
        }
        let local = Self::counter_batch_local(kind, level);
        N::SetLocal {
            local: local.clone(),
            value: W::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(W::GetLocal(local)),
                rhs: Box::new(W::ConstI64(1)),
            },
        }
    }

    fn commit_counter_batch(name: &str, local: String) -> witchy_wir::wir::WirNode {
        use witchy_wir::wir::{BinOp, Kind, WirExpr as W, WirNode as N};
        N::SetGlobal {
            global: name.into(),
            value: W::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(W::GetGlobal(name.into())),
                rhs: Box::new(W::GetLocal(local)),
            },
        }
    }

    fn loop_arena_resettable(&self, body: &Block) -> bool {
        let mut inner_lets = Vec::new();
        collect_let_names(body, &mut inner_lets);
        let inner: HashSet<String> = inner_lets.into_iter().collect();
        let mut ok = true;
        self.scan_escapes_block(body, &inner, &mut ok);
        ok
    }

    /// A second, narrower allocation proof that understands immediate
    /// destination forwarding and confined closed sums scalar-replaced by WIR.
    /// It never blesses a bare constructor, collection/index boundary, unknown
    /// call, or escaping consumer.
    fn destination_allocation_free_block(&self, body: &Block) -> bool {
        body.region.is_none()
            && body.stmts.iter().all(|statement| match statement {
                Stmt::Let { name, value, .. }
                    if self.scalar_sum_candidates.contains(name)
                        && self.scalar_sum_layout_for_binding(name, value).is_some()
                        && self.scalar_sum_value_allocation_free(value) =>
                {
                    true
                }
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value) => self.destination_allocation_free_expr(value),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => true,
                Stmt::Yield(_) => false,
            })
    }

    /// Allocation behavior after removing only the outer closed-sum object.
    /// Payload expressions still count normally: a nested packed record, list,
    /// string operation, or allocating call keeps the loop watermark.
    fn scalar_sum_value_allocation_free(&self, value: &Expr) -> bool {
        match value {
            Expr::Ctor { args, .. } => args
                .iter()
                .all(|argument| self.destination_allocation_free_expr(argument)),
            Expr::If {
                cond,
                then_block,
                else_block: Some(else_block),
            } => {
                self.destination_allocation_free_expr(cond)
                    && self.scalar_sum_tail_allocation_free(then_block)
                    && self.scalar_sum_tail_allocation_free(else_block)
            }
            _ => false,
        }
    }

    fn scalar_sum_tail_allocation_free(&self, block: &Block) -> bool {
        block.region.is_none()
            && matches!(block.stmts.as_slice(), [Stmt::Expr(value)]
                if self.scalar_sum_value_allocation_free(value))
    }

    fn destination_allocation_free_expr(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_) => true,
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::Field { base: expr, .. } => self.destination_allocation_free_expr(expr),
            Expr::Binary { op, lhs, rhs } => {
                !matches!(op, BinOp::Concat)
                    && self.destination_allocation_free_expr(lhs)
                    && self.destination_allocation_free_expr(rhs)
            }
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                self.destination_allocation_free_expr(cond)
                    && self.destination_allocation_free_block(then_block)
                    && else_block
                        .as_ref()
                        .is_none_or(|block| self.destination_allocation_free_block(block))
            }
            Expr::Match { scrutinee, arms } => {
                self.destination_allocation_free_expr(scrutinee)
                    && arms.iter().all(|arm| {
                        arm.guard
                            .as_ref()
                            .is_none_or(|guard| self.destination_allocation_free_expr(guard))
                            && self.destination_allocation_free_expr(&arm.body)
                    })
            }
            Expr::Block(block) => self.destination_allocation_free_block(block),
            Expr::Range { lo, hi, .. } => {
                self.destination_allocation_free_expr(lo)
                    && self.destination_allocation_free_expr(hi)
            }
            Expr::Call { name, args } => {
                !self.summaries.call_may_allocate(name)
                    && args.iter().enumerate().all(|(index, argument)| {
                        self.transient_destination_call(name, index, args.len(), argument)
                            .is_some()
                            || self.destination_allocation_free_expr(argument)
                    })
            }
            Expr::While { .. }
            | Expr::For { .. }
            | Expr::WhileLet { .. }
            | Expr::List(_)
            | Expr::Tuple(_)
            | Expr::Ctor { .. }
            | Expr::AnonCtor { .. }
            | Expr::RecordUpdate { .. }
            | Expr::Record { .. }
            | Expr::Index { .. }
            | Expr::Lambda { .. }
            | Expr::ExistentialPack { .. }
            | Expr::Apply { .. }
            | Expr::MethodCall { .. }
            | Expr::ExistentialCall { .. }
            | Expr::LabeledCall { .. }
            | Expr::LabeledMethodCall { .. }
            | Expr::TaggedLit { .. } => false,
        }
    }

    /// Counted-range unrolling clones only bodies whose control remains inside
    /// one logical iteration. Direct source functions are preserved as calls in
    /// the same order; unknown/host/intrinsic and indirect boundaries decline.
    fn loop_unroll_safe(&self, body: &Block) -> bool {
        if body.region.is_some() {
            return false;
        }
        body.stmts.iter().all(|statement| match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Expr(value) => self.loop_unroll_safe_expr(value),
            Stmt::Return(_) | Stmt::Yield(_) | Stmt::Break | Stmt::Continue => false,
        })
    }

    fn loop_unroll_safe_expr(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_) => true,
            Expr::Unary {
                op: UnOp::Await, ..
            }
            | Expr::Try(_)
            | Expr::Lambda { .. }
            | Expr::ExistentialPack { .. }
            | Expr::Apply { .. }
            | Expr::MethodCall { .. }
            | Expr::ExistentialCall { .. }
            | Expr::LabeledCall { .. }
            | Expr::LabeledMethodCall { .. }
            | Expr::While { .. }
            | Expr::For { .. }
            | Expr::WhileLet { .. }
            | Expr::TaggedLit { .. } => false,
            Expr::Unary { expr, .. }
            | Expr::As { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::Field { base: expr, .. } => self.loop_unroll_safe_expr(expr),
            Expr::Binary { lhs, rhs, .. }
            | Expr::Range { lo: lhs, hi: rhs, .. }
            | Expr::Index {
                base: lhs,
                index: rhs,
            } => self.loop_unroll_safe_expr(lhs) && self.loop_unroll_safe_expr(rhs),
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                self.loop_unroll_safe_expr(cond)
                    && self.loop_unroll_safe(then_block)
                    && else_block
                        .as_ref()
                        .is_none_or(|block| self.loop_unroll_safe(block))
            }
            Expr::Match { scrutinee, arms } => {
                self.loop_unroll_safe_expr(scrutinee)
                    && arms.iter().all(|arm| {
                        arm.guard
                            .as_ref()
                            .is_none_or(|guard| self.loop_unroll_safe_expr(guard))
                            && self.loop_unroll_safe_expr(&arm.body)
                    })
            }
            Expr::Block(block) => self.loop_unroll_safe(block),
            Expr::Call { name, args } => {
                self.emitted_funcs.contains(name)
                    && !self.locals.contains_key(name)
                    && args.iter().all(|argument| self.loop_unroll_safe_expr(argument))
            }
            Expr::List(items)
            | Expr::Tuple(items)
            | Expr::Ctor { args: items, .. }
            | Expr::AnonCtor { args: items, .. } => {
                items.iter().all(|item| self.loop_unroll_safe_expr(item))
            }
            Expr::RecordUpdate { base, fields, .. } => {
                self.loop_unroll_safe_expr(base)
                    && fields
                        .iter()
                        .all(|(_, value)| self.loop_unroll_safe_expr(value))
            }
            Expr::Record { fields, spread, .. } => {
                fields
                    .iter()
                    .all(|(_, value)| self.loop_unroll_safe_expr(value))
                    && spread
                        .as_ref()
                        .is_none_or(|value| self.loop_unroll_safe_expr(value))
            }
        }
    }

    fn outer_write_can_escape_heap(&self, name: &str, inner: &HashSet<String>) -> bool {
        if inner.contains(name) {
            return false;
        }
        let scalar_kind = matches!(self.locals.get(name), Some(Kind::I64) | Some(Kind::F64));
        let scalar_type = matches!(
            self.local_val_types.get(name),
            Some(ValType::Int) | Some(ValType::Bool) | Some(ValType::Float)
        );
        !scalar_kind && !scalar_type
    }

    fn call_writes_outer_heap(&self, name: &str, args: &[Expr], inner: &HashSet<String>) -> bool {
        fn place_root(expr: &Expr) -> Option<&str> {
            match expr {
                Expr::Var(root) => Some(root),
                Expr::Field { base, .. } | Expr::Index { base, .. } => place_root(base),
                Expr::Call { name, args }
                    if matches!(name.as_str(), intrinsics::LIST_AT | intrinsics::DICT_AT)
                        && args.len() == 2 =>
                {
                    place_root(&args[0])
                }
                _ => None,
            }
        }

        let Some(convs) = self.fn_conventions.get(name) else {
            return false;
        };
        convs.iter().enumerate().any(|(i, conv)| {
            *conv == Convention::Var
                && args
                    .get(i)
                    .and_then(place_root)
                    .is_some_and(|root| self.outer_write_can_escape_heap(root, inner))
        })
    }

    fn bind_pattern_eq_shape(&mut self, pat: &Pattern, shape: &EqShape) {
        match pat {
            Pattern::Var(v) => {
                self.local_shape.insert(v.clone(), shape.clone());
                if let Some(vt) = shape_val_type(shape) {
                    self.locals.insert(v.clone(), valtype_kind(vt));
                    self.local_val_types.insert(v.clone(), vt);
                }
            }
            Pattern::Tuple(parts) => {
                if let EqShape::Tuple(shapes) = shape {
                    for (part, subshape) in parts.iter().zip(shapes) {
                        self.bind_pattern_eq_shape(part, subshape);
                    }
                }
            }
            _ => {}
        }
    }

    fn scan_escapes_block(&self, b: &Block, inner: &HashSet<String>, ok: &mut bool) {
        for stmt in &b.stmts {
            match stmt {
                Stmt::Assign { name, value } => {
                    if self.outer_write_can_escape_heap(name, inner) {
                        *ok = false;
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
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::Field { base: expr, .. } => self.scan_escapes_expr(expr, inner, ok),
            Expr::Call { name, args } => {
                if self.call_writes_outer_heap(name, args, inner) {
                    *ok = false;
                }
                for a in args {
                    self.scan_escapes_expr(a, inner, ok);
                }
            }
            Expr::ExistentialCall { receiver, args, .. } => {
                self.scan_escapes_expr(receiver, inner, ok);
                for arg in args {
                    self.scan_escapes_expr(arg, inner, ok);
                }
            }
            Expr::Ctor { args, .. }
            | Expr::AnonCtor { args, .. }
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
            Expr::RecordUpdate { name: _, base, fields } => {
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
            | Expr::LabeledMethodCall { .. }
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
                // A GENERIC record element (`List(Box(Int))`) has no name-only shape —
                // its type arguments live only in the type table, so fall through to
                // `table_shape_of` rather than dropping them (BUG-319).
                if !self.record_is_generic(rec) {
                    return Some(EqShape::List(Box::new(EqShape::Record(rec.clone()))));
                }
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
            if matches!(name.as_str(), "dict.insert" | intrinsics::DICT_INSERT)
                && args.len() == 3
            {
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
            // A GENERIC record (`Box(Int)`) drops its type arguments in the name-only
            // `Record` shape, so resolve the fully-typed shape (`RecInst`) from
            // typeck's type table instead — the arguments are what let the eq/render
            // helper resolve a generic field (`item: a`) (BUG-319). A non-generic
            // record keeps the fast, name-only path.
            if self.record_is_generic(&rec) {
                if let Some(shape) = self.table_shape_of(e) {
                    return Some(shape);
                }
            }
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
                matches!(
                    name.as_str(),
                    intrinsics::DICT_NEW | "dict.insert" | intrinsics::DICT_INSERT | "dict.remove"
                        | intrinsics::DICT_REMOVE | "dict.update" | intrinsics::DICT_UPDATE
                )
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
                // (RFC-0081) The dyn head is a trait name, never the ADT's name.
                Type::Dyn(_, args) => args.iter().any(|a| mentions(a, name)),
                Type::Tuple(ts) => ts.iter().any(|t| mentions(t, name)),
                Type::Fn(params, ret, _) => {
                    params.iter().any(|p| mentions(p, name)) || mentions(ret, name)
                }
                Type::RecordCompose { base, fields } => {
                    mentions(base, name)
                        || fields.iter().any(|(_, field)| mentions(field, name))
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
            // (RFC-0081) A dyn type has no structural-equality shape — `None` is
            // a loud error at the use site, like Dict/fn/type variables.
            Type::Dyn(_, _) => None,
            Type::Named(n, args) => match n.as_str() {
                "Int" | "Duration" => Some(EqShape::Int),
                "Bool" => Some(EqShape::Bool),
                "Float" => Some(EqShape::Float),
                "String" => Some(EqShape::Str),
                "Bytes" => Some(EqShape::Bytes),
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
                t if witchy_types::typeck::anon_union_synthetic_variants(t).is_some() => {
                    let variants = anon_union_variant_types(t, args)?;
                    let inst: Option<Vec<Vec<EqShape>>> = variants
                        .iter()
                        .map(|fs| {
                            fs.iter()
                                .map(|f| self.eq_shape_of_type_rec(f, subst, visiting))
                                .collect()
                        })
                        .collect();
                    inst.map(|shapes| EqShape::AdtInst(t.to_string(), shapes))
                }
                t if self.record_fields.contains_key(t) => {
                    // A GENERIC record instantiation (`Box(Int)`, std `Set(a)`) must
                    // carry its type-ARGUMENT shapes, so the eq/render helper can
                    // resolve a generic field type (`item: a`) under the argument
                    // substitution — exactly as the ADT arm below does (BUG-319). The
                    // plain `Record` arm dropped the args, so a fully annotated
                    // `Box(Int) == Box(Int)` was rejected on the compiled backend.
                    // A non-generic use (no type args) OR a record whose fields use
                    // no type variable (a phantom generic like `Wrap(a): count: Int`):
                    // the arg-free `Record` shape resolves every field concretely.
                    if args.is_empty() || !self.record_is_generic(t) {
                        return Some(EqShape::Record(t.to_string()));
                    }
                    // Generic record instantiation: map the record's DECLARED type
                    // parameters (in order) to the use-site argument shapes, so a
                    // generic field (`item: a`) resolves under the substitution.
                    let params = self.record_generics.get(t).cloned().unwrap_or_default();
                    let mut arg_shapes: Vec<EqShape> = Vec::new();
                    for (_, arg) in params.iter().zip(args) {
                        arg_shapes.push(self.eq_shape_of_type_rec(arg, subst, visiting)?);
                    }
                    Some(EqShape::RecInst(t.to_string(), arg_shapes))
                }
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
            Type::RecordCompose { .. } => unreachable!(
                "compiler invariant violated: record composition must be normalized before Wasm equality-shape lowering"
            ),
        }
    }

    /// The field-resolution substitution for a `RecInst(tyname, args)`: the record's
    /// distinct field type variables (first-occurrence order, matching how the
    /// argument shapes were built in `eq_shape_of_type_rec`) mapped to `args`. Used
    /// by the eq/render helper builders to resolve a generic field under the
    /// instantiation — the record analogue of `AdtRec`'s subst.
    pub(crate) fn record_field_subst(
        &self,
        tyname: &str,
        args: &[EqShape],
    ) -> HashMap<String, EqShape> {
        self.record_generics
            .get(tyname)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .zip(args.iter().cloned())
            .collect()
    }

    /// Whether a record type is GENERIC for equality purposes — i.e. one of its
    /// field types mentions a type variable (`Box(a): item: a`). Such a record's
    /// shape can only be resolved with its type arguments (via the type table),
    /// so the name-only `EqShape::Record` fast path must be skipped for it.
    pub(crate) fn record_is_generic(&self, tyname: &str) -> bool {
        self.record_field_types.get(tyname).is_some_and(|fields| {
            let mut params: Vec<String> = Vec::new();
            for f in fields {
                collect_type_vars(f, &mut params);
            }
            !params.is_empty()
        })
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

/// Compile a module's functions to WAT. Requires a `main` returning Int or Nil;
/// `main` may take a single capability parameter.
/// Collect every name that could refer to a function — call targets and bare
/// identifiers (first-class function values) — used for reachability/DCE. Over-
/// approximates (also picks up locals), which is safe: non-function names just
/// don't match any function and are ignored.
/// How many nested loops can carry an arena watermark (deeper loops simply
/// skip the reset — a safe fallback).
const WM_POOL: usize = 4;

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

fn nested_var_place_roots(
    block: &Block,
    conventions: &HashMap<String, Vec<Convention>>,
) -> HashSet<String> {
    fn place_root(expr: &Expr) -> Option<&str> {
        match expr {
            Expr::Var(root) => Some(root),
            Expr::Field { base, .. } | Expr::Index { base, .. } => place_root(base),
            Expr::Call { name, args }
                if matches!(name.as_str(), intrinsics::LIST_AT | intrinsics::DICT_AT)
                    && args.len() == 2 =>
            {
                place_root(&args[0])
            }
            _ => None,
        }
    }

    fn scan_block(
        block: &Block,
        conventions: &HashMap<String, Vec<Convention>>,
        roots: &mut HashSet<String>,
    ) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value)
                | Stmt::Yield(value) => scan_expr(value, conventions, roots),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn scan_expr(
        expr: &Expr,
        conventions: &HashMap<String, Vec<Convention>>,
        roots: &mut HashSet<String>,
    ) {
        match expr {
            Expr::Call { name, args } => {
                if let Some(parameter_conventions) = conventions.get(name) {
                    for (argument, convention) in args.iter().zip(parameter_conventions) {
                        if *convention == Convention::Var
                            && !matches!(argument, Expr::Var(_))
                            && let Some(root) = place_root(argument)
                        {
                            roots.insert(root.to_string());
                        }
                    }
                } else {
                    // A closure-valued local is represented as `Call` after
                    // parsing, but has no declaration entry in this map. Its
                    // function type may carry `var`, so protect every nested
                    // argument root from record scalar replacement.
                    for argument in args {
                        if !matches!(argument, Expr::Var(_))
                            && let Some(root) = place_root(argument)
                        {
                            roots.insert(root.to_string());
                        }
                    }
                }
                for argument in args {
                    scan_expr(argument, conventions, roots);
                }
            }
            Expr::Apply { func, args } => {
                scan_expr(func, conventions, roots);
                for argument in args {
                    // Indirect calls carry conventions in the function value's
                    // type, which this name-only scan cannot inspect. Disqualify
                    // any nested argument root conservatively so a later `var`
                    // write-back cannot race a scalar-replaced record field.
                    if !matches!(argument, Expr::Var(_))
                        && let Some(root) = place_root(argument)
                    {
                        roots.insert(root.to_string());
                    }
                    scan_expr(argument, conventions, roots);
                }
            }
            Expr::Ctor { args, .. }
            | Expr::AnonCtor { args, .. }
            | Expr::List(args)
            | Expr::Tuple(args) => {
                for argument in args {
                    scan_expr(argument, conventions, roots);
                }
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::Field { base: expr, .. } => scan_expr(expr, conventions, roots),
            Expr::RecordUpdate { base, fields, .. } => {
                scan_expr(base, conventions, roots);
                for (_, value) in fields {
                    scan_expr(value, conventions, roots);
                }
            }
            Expr::Binary { lhs, rhs, .. }
            | Expr::Index { base: lhs, index: rhs }
            | Expr::Range { lo: lhs, hi: rhs, .. } => {
                scan_expr(lhs, conventions, roots);
                scan_expr(rhs, conventions, roots);
            }
            Expr::If { cond, then_block, else_block } => {
                scan_expr(cond, conventions, roots);
                scan_block(then_block, conventions, roots);
                if let Some(block) = else_block {
                    scan_block(block, conventions, roots);
                }
            }
            Expr::Match { scrutinee, arms } => {
                scan_expr(scrutinee, conventions, roots);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        scan_expr(guard, conventions, roots);
                    }
                    scan_expr(&arm.body, conventions, roots);
                }
            }
            Expr::While { cond, body } => {
                scan_expr(cond, conventions, roots);
                scan_block(body, conventions, roots);
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                scan_expr(scrutinee, conventions, roots);
                scan_block(body, conventions, roots);
            }
            Expr::For { iter, body, .. } => {
                scan_expr(iter, conventions, roots);
                scan_block(body, conventions, roots);
            }
            Expr::Lambda { body, .. } | Expr::Block(body) => {
                scan_block(body, conventions, roots)
            }
        Expr::ExistentialCall { receiver, args, .. } => {
            scan_expr(receiver, conventions, roots);
            for argument in args {
                scan_expr(argument, conventions, roots);
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            scan_expr(receiver, conventions, roots);
            for (_, argument) in args {
                scan_expr(argument, conventions, roots);
            }
        }
        Expr::MethodCall { .. }
        | Expr::Record { .. }
        | Expr::LabeledCall { .. }
        | Expr::Int(_)
        | Expr::Duration(_)
            | Expr::Float(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_)
            | Expr::TaggedLit { .. } => {}
        }
    }

    let mut roots = HashSet::new();
    scan_block(block, conventions, &mut roots);
    roots
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
pub(crate) fn is_string_export(f: &Function, grantable: &HashSet<&str>) -> bool {
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

fn scalar_record_call_candidates_block(
    body: &Block,
    producers: &HashMap<String, ScalarRecordProducer>,
    local_types: &HashMap<String, Type>,
    specialized_types: &[(Type, LayoutId)],
) -> HashMap<String, LayoutId> {
    let mut bindings = HashMap::new();
    let mut candidates = HashMap::new();
    for statement in &body.stmts {
        let Stmt::Let {
            name,
            value: Expr::Call { name: producer, .. },
            ..
        } = statement
        else {
            continue;
        };
        let Some(producer) = producers.get(producer) else {
            continue;
        };
        let local_layout = local_types.get(name).and_then(|ty| {
            specialized_types
                .iter()
                .find_map(|(known, id)| {
                    (known.unqualified() == ty.unqualified()).then_some(*id)
                })
        });
        if local_layout == Some(producer.layout) {
            bindings.insert(name.clone(), (statement as *const Stmt) as usize);
            candidates.insert(name.clone(), producer.layout);
        }
    }
    if candidates.is_empty() {
        return candidates;
    }
    let mut disqualified = HashSet::new();
    scan_scalar_record_block(
        body,
        &candidates,
        producers,
        &bindings,
        &mut disqualified,
    );
    candidates.retain(|name, _| !disqualified.contains(name));
    candidates
}

fn scan_scalar_record_block(
    block: &Block,
    candidates: &HashMap<String, LayoutId>,
    producers: &HashMap<String, ScalarRecordProducer>,
    bindings: &HashMap<String, usize>,
    disqualified: &mut HashSet<String>,
) {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { name, value, .. } => {
                if candidates.contains_key(name)
                    && bindings.get(name).copied() != Some((statement as *const Stmt) as usize)
                {
                    disqualified.insert(name.clone());
                }
                scan_scalar_record_expr(value, candidates, producers, bindings, disqualified);
            }
            Stmt::Assign { name, value } if candidates.contains_key(name) => {
                let compatible = matches!(value,
                    Expr::Call { name: producer, .. }
                        if producers.get(producer).map(|producer| producer.layout)
                            == candidates.get(name).copied());
                if !compatible {
                    disqualified.insert(name.clone());
                }
                scan_scalar_record_expr(value, candidates, producers, bindings, disqualified);
            }
            Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Yield(value)
            | Stmt::Expr(value) => {
                scan_scalar_record_expr(value, candidates, producers, bindings, disqualified);
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn scan_scalar_record_expr(
    expr: &Expr,
    candidates: &HashMap<String, LayoutId>,
    producers: &HashMap<String, ScalarRecordProducer>,
    bindings: &HashMap<String, usize>,
    disqualified: &mut HashSet<String>,
) {
    match expr {
        Expr::Field { base, .. }
            if matches!(base.as_ref(), Expr::Var(name) if candidates.contains_key(name)) => {}
        Expr::Var(name) if candidates.contains_key(name) => {
            disqualified.insert(name.clone());
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            scan_scalar_record_expr(cond, candidates, producers, bindings, disqualified);
            scan_scalar_record_block(
                then_block,
                candidates,
                producers,
                bindings,
                disqualified,
            );
            if let Some(block) = else_block {
                scan_scalar_record_block(
                    block,
                    candidates,
                    producers,
                    bindings,
                    disqualified,
                );
            }
        }
        Expr::Block(block) | Expr::Lambda { body: block, .. } => scan_scalar_record_block(
            block,
            candidates,
            producers,
            bindings,
            disqualified,
        ),
        Expr::While { cond, body } => {
            scan_scalar_record_expr(cond, candidates, producers, bindings, disqualified);
            scan_scalar_record_block(body, candidates, producers, bindings, disqualified);
        }
        Expr::For { iter, body, .. } => {
            scan_scalar_record_expr(iter, candidates, producers, bindings, disqualified);
            scan_scalar_record_block(body, candidates, producers, bindings, disqualified);
        }
        Expr::WhileLet {
            scrutinee, body, ..
        } => {
            scan_scalar_record_expr(scrutinee, candidates, producers, bindings, disqualified);
            scan_scalar_record_block(body, candidates, producers, bindings, disqualified);
        }
        Expr::Match { scrutinee, arms } => {
            scan_scalar_record_expr(scrutinee, candidates, producers, bindings, disqualified);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    scan_scalar_record_expr(guard, candidates, producers, bindings, disqualified);
                }
                scan_scalar_record_expr(
                    &arm.body,
                    candidates,
                    producers,
                    bindings,
                    disqualified,
                );
            }
        }
        _ => crate::escape::for_each_immediate_subexpr(expr, &mut |inner| {
            scan_scalar_record_expr(inner, candidates, producers, bindings, disqualified)
        }),
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

fn block_reads_var(block: &Block, name: &str) -> bool {
    block.stmts.iter().any(|statement| match statement {
        Stmt::Assign { name: target, value } => target == name || expr_reads_var(value, name),
        Stmt::Let { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Return(Some(value))
        | Stmt::Yield(value)
        | Stmt::Expr(value) => expr_reads_var(value, name),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => false,
    })
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
        | Expr::LabeledCall { .. }
        | Expr::LabeledMethodCall { .. } => {
            unreachable!("range/index sugar is lowered before codegen (parser::lower_sugar_module)")
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            collect_let_names_expr(receiver, out);
            for arg in args {
                collect_let_names_expr(arg, out);
            }
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
                out.push(format!("__forptr_{var}"));
                out.push(format!("__forendptr_{var}"));
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
        | Expr::AnonCtor { args, .. }
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
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => {
            collect_let_names_expr(expr, out)
        }
        Expr::RecordUpdate { name: _, base, fields } => {
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

#[cfg(test)]
mod callable_layout_tests;

#[cfg(test)]
mod host_layout_tests;
