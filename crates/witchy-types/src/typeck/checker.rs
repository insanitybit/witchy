//! The checker's state. Its methods live in `impl Checker` in `typeck.rs`;
//! defining the struct here gives them their own codegen unit (rustc places
//! an inherent method in the CGU of its self type's module).

use super::*;

/// `StoragePlan::classify` answers over one exact set of type definitions.
pub(super) type StoragePlanMemo =
    std::rc::Rc<std::cell::RefCell<HashMap<ast::Type, witchy_canonical_source::storage::StoragePlan>>>;

thread_local! {
    static SHARED_STORAGE_PLANS: std::cell::RefCell<Option<(std::collections::BTreeMap<String, ast::TypeDef>, StoragePlanMemo)>> =
        const { std::cell::RefCell::new(None) };
}

/// The storage-plan memo for checkers over `definitions`.
///
/// One compile builds a checker per pass (link check, each monomorphization
/// round, each re-annotation: eleven for a small async program), and each
/// used to classify the same types over the same definitions from scratch
/// (6% of the compile). A plan is a pure function of the type and the
/// definitions map, so a memo is reused exactly when the new checker's map
/// is equal to the one it was filled over (`TypeDef` equality is structural
/// over every field). Any other map replaces it.
pub(super) fn shared_storage_plans(definitions: &std::collections::BTreeMap<String, ast::TypeDef>) -> StoragePlanMemo {
    SHARED_STORAGE_PLANS.with_borrow_mut(|shared| {
        if let Some((filled_over, memo)) = shared.as_ref()
            && filled_over == definitions
        {
            return std::rc::Rc::clone(memo);
        }
        let memo = StoragePlanMemo::default();
        *shared = Some((definitions.clone(), std::rc::Rc::clone(&memo)));
        memo
    })
}

pub(super) struct Checker {
    /// The module's type definitions in the canonical form, so a view capture
    /// is admitted by exactly the record-root and list-root carrier rules that
    /// lowering and authority apply, with the refusal reported here.
    pub(super) canonical_definitions: std::collections::BTreeMap<String, ast::TypeDef>,
    /// RFC-0164 Phase 3: memoizes `resolve`, invalidated by `bind_subst_var`/
    /// `restore_subst` (the only two ways `subst` changes) rather than on
    /// every read, so it survives across the many `resolve` calls between
    /// mutations. `RefCell` because `resolve` takes `&self` -- most of its
    /// callers hold other borrows of `self` alongside it.
    pub(super) resolve_cache: std::cell::RefCell<HashMap<u32, Ty>>,
    /// Memoizes `StoragePlan::classify` over `canonical_definitions`, which
    /// never changes after construction. The plan is a pure function of the
    /// source type and that map; recomputing it re-walked every reachable
    /// definition (capability-authority and view searches) for each probed
    /// binding, ~a third of an async compile. Shared by every checker on
    /// this thread built over equal definitions ([`shared_storage_plans`]).
    pub(super) storage_plans: StoragePlanMemo,
    /// Memoizes `ty_carries_once_callable` for variable-free types. The
    /// answer then reads only the type and the declaration tables
    /// (`record_fields`, `adt_variants`, `ctor_sigs`, `stored_obligations`),
    /// which are filled before any body is checked; re-walking every nominal
    /// payload with fresh substitutions per probe was ~7% of an async compile.
    pub(super) once_callable_types: std::cell::RefCell<HashMap<Ty, bool>>,
    /// Root unique-storage queries, evaluated with an empty recursion guard.
    /// Declaration tables are fixed before body checking; both substitution
    /// mutations clear this memo because the judgment resolves their fields.
    pub(super) unique_storage_types: std::cell::RefCell<HashMap<Ty, bool>>,
    /// When annotating (see `annotate`): expression identity -> inferred type,
    /// finalized against the ending substitution. Key = `&Expr as *const _`.
    // The boolean is checked non-completion, recorded in the same session so
    // infer_transient suppresses both facts for temporary AST allocations.
    pub(super) type_record: Option<HashMap<usize, (Ty, bool, Option<Ty>)>>,
    /// Exact retained expressions checked as the compiler-known `.owned()`
    /// operation. Method spelling and receiver shape alone cannot distinguish
    /// this operation from a user-declared method.
    pub(super) compiler_owned_materializations: HashSet<usize>,
    /// A persistent bare declaration expression checked as a function value.
    /// Local bindings and transient desugar nodes never mint this origin.
    pub(super) declared_function_values: HashMap<usize, String>,
    /// Directed concrete-to-existential coercions discovered while checking.
    /// The types remain checker types until the ending substitution is known.
    /// [`TypeTable`] preserves unresolved requests so final existential
    /// preparation rejects them loudly instead of silently omitting a pack.
    pub(super) existential_pack_record: Option<HashMap<usize, (Ty, Ty)>>,
    /// Directed existential supertrait conversions. The pair is `(target,
    /// source)`; lowering converts the source witness to the target witness
    /// without exposing the concrete payload.
    pub(super) existential_upcast_record: Option<HashMap<usize, (Ty, Ty)>>,
    /// Directed richer-to-poorer anonymous-record conversions discovered at
    /// explicit expected-type sites. The pair is `(target, source)` and is
    /// finalized only after the ending substitution is known.
    pub(super) record_projection_record: Option<HashMap<usize, (Ty, Ty)>>,
    pub(super) fn_sigs: HashMap<String, (Vec<Ty>, Ty)>,
    /// Source-level `async fn` declarations expose `Task(T)` to callers while
    /// their own body returns the declared `T`.  Keeping the body contract
    /// separate prevents the type checker from depending on CPS lowering to
    /// explain `await`.
    pub(super) async_body_returns: HashMap<String, Ty>,
    /// Callable contract promised by each named reusable function.
    pub(super) fn_qualifiers: HashMap<String, CallableQualifiers>,
    /// Functions selected by trait lowering as actual `From.from` impls.
    /// Generated function names are an ABI detail, not semantic evidence.
    pub(super) from_conversion_fns: HashSet<String>,
    pub(super) ctor_sigs: HashMap<String, (Vec<Ty>, Ty)>,
    /// Type-parameter var ids per constructor, so a generic ADT's constructors
    /// are instantiated fresh at each use (e.g. `Some(1)` vs `Some("x")`).
    pub(super) ctor_typarams: HashMap<String, HashSet<u32>>,
    /// Record types: name -> (type-parameter var ids in order, fields). A field
    /// type may mention the parameters, which are instantiated with the value's
    /// actual type arguments on access.
    pub(super) record_fields: HashMap<String, RecordInfo>,
    /// RFC-0112 stage-1 nominal shells. Their syntax and callable identities are
    /// checked, but values may not yet be projected, destructured, updated, or
    /// transported through executable calls until owner-root lowering lands.
    pub(super) borrowed_nominal_types: HashSet<String>,
    /// Fields whose declared type contributes a borrowed relation to a lifetime
    /// nominal. The AST declaration is authoritative here because `Ty` erases a
    /// `View` qualifier after checking.
    pub(super) borrowed_nominal_relation_fields: HashMap<String, HashSet<String>>,
    /// Nominal aggregates whose fields use the executable RFC-0122 `&` or
    /// `&mut` relation. Legacy `View` shells remain runtime-guarded.
    pub(super) explicit_reference_nominal_types: HashSet<String>,
    /// The assignment target currently authorized to infer a borrowed-shell
    /// record update. A standalone update expression remains an untracked copy
    /// and is rejected.
    pub(super) borrowed_shell_update_target: Option<String>,
    /// The assignment target of a checker-recognized mutation that preserves
    /// every existing must-consume element (currently list push). Its private
    /// rebuilding intrinsic may temporarily own the list before the assignment
    /// writes the rebuilt value back to the same borrowed place.
    pub(super) must_self_update_target: Option<String>,
    /// Mutable locals whose initializer created a checked borrowed shell. This
    /// is deliberately separate from type identity: a borrowed shell received
    /// as an ordinary parameter has no compiler-owned root companions at this
    /// boundary, so it must retain the RFC-0112 stage-1 runtime guard.
    pub(super) borrowed_shell_bindings: Vec<HashSet<String>>,
    /// Direct references erase to their payload in [`Ty`], so retain their
    /// source relation for closure-capture checks.
    pub(super) explicit_reference_bindings: Vec<HashSet<String>>,
    /// Bindings declared with `frozen`. Qualifiers erase from [`Ty`], but
    /// exclusive borrowing must still respect the source storage contract.
    pub(super) frozen_bindings: Vec<HashSet<String>>,
    /// Bindings whose value handle was declared `unique`.  This is distinct
    /// from a shell that merely stores a nested unique payload: projections of
    /// the latter must retain field-local provenance rather than freezing every
    /// sibling by root name.
    pub(super) unique_value_bindings: Vec<HashSet<String>>,
    /// Bindings known to carry unique storage, including inferred list/tuple/
    /// nominal shells.  The common `consumed` set records their affine flow;
    /// this set only retains the erased source qualifier provenance.
    pub(super) unique_storage_bindings: Vec<HashSet<String>>,
    /// In opt mode, a source `&place` may leave a retained root-plus-projection
    /// carrier. Keep every observed root for this function, including across
    /// branches and carrier copies: without a liveness proof a later unique
    /// write-back cannot silently clone the old root to repair that alias.
    pub(super) opt_viewed_roots: HashSet<String>,
    /// Original declared types for bindings carrying unique storage. Pattern
    /// extraction needs these because `Ty` has already erased nested qualifiers.
    pub(super) unique_binding_types: Vec<HashMap<String, ast::Type>>,
    /// Checked local ascriptions whose recursive affine storage plan must
    /// remain visible to the canonical layout and SafeWIR authority stages.
    pub(super) checked_storage_declared_types: HashSet<ast::Type>,
    /// Expression identities whose source storage qualifiers follow a
    /// checked binding/projection origin rather than an HM type rendering.
    pub(super) affine_expression_types: HashMap<usize, ast::Type>,
    /// Source callable contracts keyed by local binding. Runtime callable
    /// identity erases qualifiers, so indirect source calls recover both the
    /// parameter and result facts rather than silently repairing aliases in
    /// opt.
    pub(super) callable_unique_contracts: Vec<HashMap<String, UniqueCallableContract>>,
    /// Source-level opt callable provenance for local function-value aliases.
    /// This is deliberately separate from `Ty::Fn`: the ordinary function ABI
    /// remains usable by normal callers, while opt higher-order entries demand
    /// the checked fact explicitly.
    pub(super) callable_opt_provenance: Vec<HashMap<String, OptCallableContract>>,
    /// Structural checked value provenance for callback-bearing aggregate
    /// bindings. Function ABI still does not itself encode opt authority.
    pub(super) container_opt_provenance: Vec<HashMap<String, StoredOptProof>>,
    /// Proofs captured at source sites before block and pattern scopes close.
    pub(super) opt_expression_proofs: HashMap<usize, StoredOptProof>,
    /// Sealed record capabilities (`capability X:` with named fields). Their
    /// fields are opaque: `.field` access is rejected so the only way to reach a
    /// carried capability is `match`, which the linker confines to the home
    /// module — otherwise an alias would leak the underlying authority.
    pub(super) sealed_types: HashSet<String>,
    /// Every `sealed type` and `capability` record. Rebuilding one through a
    /// record update is construction, so it is legal only while checking a
    /// function in the type's defining module.
    pub(super) construction_sealed_types: HashSet<String>,
    /// Single-field brands whose compiled representation is the same externref
    /// as their single underlying migrated host capability.
    pub(super) transparent_externref_brands: HashMap<String, String>,
    /// Named non-generic aggregates that the compiled backend lowers to typed GC
    /// structs, so they may directly carry migrated externref capabilities.
    pub(super) gc_cap_aggregates: HashSet<String>,
    pub(super) adt_variants: HashMap<String, Vec<String>>,
    pub(super) fn_conventions: HashMap<String, Vec<Convention>>,
    /// Whether each source parameter is an explicit `&mut` reference. This is
    /// deliberately tracked beside conventions: `&mut T` keeps the public
    /// `let` convention while still reserving an exclusive caller place.
    pub(super) fn_exclusive_reference_params: HashMap<String, Vec<bool>>,
    /// Whether each source parameter is an outer `frozen` value. Qualifiers
    /// erase from `Ty`, so this preserves the write-back contract needed to
    /// distinguish `var frozen T` (replacement) from `var T` (deep mutation).
    pub(super) fn_frozen_value_params: HashMap<String, Vec<bool>>,
    /// Source callable contracts for named functions. Callable identity remains
    /// erased at runtime, so this is checker-only provenance.
    pub(super) fn_unique_contracts: HashMap<String, UniqueCallableContract>,
    /// Exact checked result storage for named source calls. `Ty` erases
    /// qualifiers, so consuming patterns recover their field provenance here.
    pub(super) fn_source_result_types: HashMap<String, ast::Type>,
    /// (RFC-0167) Named functions whose every result is freshly allocated
    /// storage, inferred from their bodies by `infer_fresh_result_fns`. A
    /// `unique` result cannot be declared on the canonical route yet, so
    /// `dict.new()` is typed `Dict(k, v)` while its value is a fresh owner;
    /// binding it with `var` may hand it to a `unique` parameter in `mode opt`.
    /// Checker-only provenance: lowering never reads it.
    pub(super) fresh_result_fns: HashSet<String>,
    /// Bindings whose owner proof came from `fresh_result_fns` rather than a
    /// declared `unique` type. Only these demote on an ordinary alias; a
    /// declared unique value stays affine (RFC-0167 §4).
    pub(super) inferred_owner_bindings: Vec<HashSet<String>>,
    /// Named function origins retained from the linker mode markers.  This is
    /// signature/source metadata, never inferred from an implementation body.
    pub(super) fn_opt_provenance: HashMap<String, OptCallableContract>,
    /// Named record fields declared as outer `frozen` values. This preserves
    /// storage immutability below an otherwise mutable aggregate after `Ty`
    /// erases qualifiers.
    pub(super) frozen_record_fields: HashMap<String, HashSet<String>>,
    /// Record fields declared with an outer `unique` qualifier.  This is the
    /// checked declaration fact used for projection-aware affine transfer.
    pub(super) unique_record_fields: HashMap<String, HashSet<String>>,
    /// Source record-field types and their nominal parameter order.  Used only
    /// to recover qualifier provenance for field projections after `Ty` erasure.
    pub(super) unique_source_record_fields: HashMap<String, HashMap<String, ast::Type>>,
    pub(super) unique_source_record_params: HashMap<String, Vec<String>>,
    /// Source positional constructor payload types. ADT field `Ty`s erase
    /// qualifiers, so pattern extraction follows these checked declarations
    /// rather than marking every binding below a moved aggregate affine.
    pub(super) unique_ctor_field_types: HashMap<String, Vec<ast::Type>>,
    pub(super) ctor_source_type_params: HashMap<String, Vec<String>>,
    /// Source parameter names paired with [`Self::fn_conventions`]. Names are
    /// diagnostic metadata only; function-type identity remains positional.
    pub(super) fn_param_names: HashMap<String, Vec<String>>,
    /// Per-function type parameters (name, var id), from lowercase type names in
    /// signatures. Generalized: instantiated fresh at each call site.
    pub(super) fn_typarams: HashMap<String, Vec<(String, u32)>>,
    /// Generic view and replay-capture storage judgments rechecked after
    /// direct-call instantiation. Function-value transport remains refused.
    pub(super) fn_storage_obligations: HashMap<String, Vec<StorageObligation>>,
    pub(super) current_storage_obligations: Vec<StorageObligation>,
    /// Concrete executable functions on this checked source. Templates and
    /// unused linked helpers receive their capture proof after specialization.
    pub(super) replay_capture_scope: std::sync::Arc<std::collections::BTreeSet<String>>,
    pub(super) check_replay_captures: bool,
    /// Local inference variables can resolve in later declarations.
    pub(super) pending_replay_captures: Vec<(Ty, String)>,
    /// Per-function call obligations from `where` clauses, keyed to the same
    /// signature type-parameter var ids as `fn_typarams`. These are checked at a
    /// generic call site so wrapper functions cannot erase a callee's public
    /// protocol bounds.
    pub(super) fn_bounds: HashMap<String, Vec<(u32, String)>>,
    /// Bare trait method names declared in the linked module. Trait methods do
    /// not have one first-class function value; value-position references need a
    /// targeted RFC-0050 diagnostic instead of the generic unbound-variable one.
    pub(super) trait_method_names: HashSet<String>,
    /// The source trait graph survives trait lowering only through this checker
    /// context. It lets source checking reject an unrelated `dyn` conversion
    /// before feature staging; final existential preparation validates the same
    /// edge again before either runtime sees a compiler-owned upcast node.
    pub(super) trait_supertraits: HashMap<String, Vec<String>>,
    /// (BUG-308) The type parameters (name -> var id) of the function whose body is
    /// currently being checked, so a body `let`/`var` ascription's lowercase name
    /// (`let out: List(a) = …`) resolves to the SAME type-parameter var as the
    /// signature rather than a fresh concrete `Named("a")` — which would pin the
    /// generic parameter and trip the "isn't generic" soundness check.
    pub(super) current_typarams: HashMap<String, u32>,
    /// Trait bounds declared on the function currently being checked.
    pub(super) current_bounds: Vec<(String, String)>,
    pub(super) subst: HashMap<u32, Ty>,
    pub(super) next_var: u32,
    /// Each binding carries its type and whether it is mutable.
    pub(super) scopes: Vec<HashMap<String, (Ty, bool)>>,
    /// Bindings that have been consumed (moved out via an `own` parameter) and
    /// may not be used again until reassigned. Flow-sensitive within a body.
    pub(super) consumed: HashSet<(usize, String)>,
    /// Consumed bindings whose consuming edge was invocation of a `once fn`,
    /// retained separately so diagnostics and CFG joins do not misreport an
    /// affine call as an `own` move.
    pub(super) once_invoked: HashSet<(usize, String)>,
    /// Checked stored-parameter and transitive must-containment judgment.
    /// All source storage consumers read one canonical-source product.
    pub(super) stored_obligations: StoredObligationCatalog,
    /// Owned must-consume bindings that still require disposition on at least
    /// one control-flow path. Branch joins use union: a value is discharged
    /// only when every path consumes or returns it.
    pub(super) must_live: HashSet<(usize, String)>,
    /// Lexical scope floor exited by the innermost break/continue. Owners in
    /// outer scopes survive that edge; iteration-local obligations do not.
    pub(super) loop_exit_scopes: Vec<usize>,
    /// Completed owned operands awaiting transfer to their enclosing operation.
    /// The scope index distinguishes an abandoned loop-body expression from an
    /// outer operand that remains live while an inner loop runs.
    pub(super) pending_operands: Vec<(usize, Vec<Ty>)>,
    /// Must-consume values received through `let`/`var` borrowing conventions.
    /// The caller retains their obligation, so the callee may inspect or mutate
    /// through the borrow but may not move, return, replace, or pass them to an
    /// `own` boundary.
    pub(super) must_borrowed: HashSet<(usize, String)>,
    /// Affine callable bindings received through a shared `borrow` parameter.
    /// They may be inspected or forwarded to another borrow, but invocation or
    /// transfer would consume the caller-owned value.
    pub(super) once_borrowed: HashSet<(usize, String)>,
    /// One entry per ACTIVE `region:` block, holding the names declared
    /// inside it — an assignment to a name outside the innermost region must
    /// be scalar (a region's only pointer-escape is its value).
    pub(super) region_locals: Vec<HashSet<String>>,
    /// Exact checked contracts for `region:` blocks.  Canonical lowering may
    /// consume only this retained, AST-address-keyed product; a naked region
    /// node is never permission to erase the source boundary.
    pub(super) region_promotions: HashMap<usize, RegionPromotionFact>,
    /// The only packed records that canonical v1 can carry inline.  This is
    /// source-derived layout identity, not a lowerer-supplied permission bit.
    pub(super) region_packed_layouts: HashMap<String, RegionPackedLayoutDescriptor>,
    /// The declared return type of the function currently being checked, so `?`
    /// can require the enclosing function to return a matching Result/Option.
    pub(super) current_ret: Option<Ty>,
    /// Whether the callable body currently being checked promised `pure`.
    /// Ordinary nested lambdas replace (rather than inherit) this context: their
    /// bodies are latent behavior, while their construction is checked at the
    /// enclosing expression site.
    pub(super) current_pure: bool,
    /// Binding identities for `var` parameters of the current callable. A write
    /// to one of these places is observable caller write-back; mutable locals and
    /// owned parameters remain ordinary internal computation.
    pub(super) pure_var_parameters: HashSet<(usize, String)>,
    /// The callback parameter of an isolated-worker stdlib reference body. The
    /// wrapper may invoke this parameter directly; the worker adapter still has
    /// its own capability-bearing callback boundary.
    pub(super) current_isolated_callback: Option<String>,
    /// (BUG-395 / RFC-0047) The key types of the generic `Dict` key operations
    /// (`dict.insert`/`get_or`/`update`/`contains_key`/`remove`) invoked in the
    /// body currently being checked, with the source line. A dict key is hashed and
    /// compared, so it must be `Eq`; validated once the body is fully inferred so
    /// an unbounded generic key (a type parameter with no `where k: Eq` bound)
    /// is rejected rather than silently accepted.
    pub(super) dict_key_ops: Vec<(Ty, u32)>,
    /// Source line of the statement currently being checked, attached to errors
    /// so diagnostics point at a location. 0 means "no line known".
    pub(super) cur_line: u32,
    /// The module (file stem) of the function currently being checked, so a
    /// diagnostic can render a home-module type/variant with its bare name — the
    /// spelling the reader wrote — while keeping cross-module qualifiers that
    /// disambiguate (RFC-0042; BUG-292). Empty means "unknown".
    pub(super) cur_module: String,
    /// The entry module's name (the home of the unqualified `main`), used as the
    /// home for a bare function whose name carries no `module.` prefix (BUG-292).
    pub(super) entry_module: String,
    /// True only while checking the isolated program used to execute a
    /// `comptime:` block; ordinary runtime modules cannot traffic in compiler
    /// syntax values such as `meta.ItemSyntax`.
    pub(super) compiler_syntax_allowed: bool,
    /// The checked declaration's own mode, reset before every function body;
    /// imported opt bodies cannot inherit the entry module's normal mode.
    pub(super) opt_mode: bool,
    /// The source callable currently being checked may be imported from an opt
    /// module even when the linked entry module is normal.
    pub(super) current_opt_callable: bool,
}
