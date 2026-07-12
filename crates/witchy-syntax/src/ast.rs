//! Abstract syntax tree for witchy.
//!
//! Convention (Gleam-style): identifiers beginning with an uppercase letter are
//! constructors/variants (`Click`, `Closed`); lowercase identifiers are
//! variables and functions (`greet`, `count`).

// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashSet, HashSetExt as _};

/// Legacy source-spellable structural-render intrinsic.
///
/// This remains accepted as an oracle/test spelling while the in-tree corpus is
/// migrated away from direct compiler-private calls.
pub const LEGACY_RENDER_INTRINSIC: &str = "__render";

/// Lexer-only render intrinsic emitted for string interpolation.
///
/// `@` is not a valid identifier character in witchy source, so users cannot
/// spell this name. It gives the compiler a concrete seam to later reject direct
/// `__render(...)` calls without changing interpolation semantics.
pub const GENERATED_RENDER_INTRINSIC: &str = "@render";

pub fn is_render_intrinsic(name: &str) -> bool {
    name == LEGACY_RENDER_INTRINSIC || name == GENERATED_RENDER_INTRINSIC
}

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    /// The performance mode declared at the top of the file (`mode opt`), or empty
    /// for an ordinary file. The linker copies the entry module's modes onto the
    /// linked module; enforcement (cliffs → errors) then applies to the entry
    /// file's own functions, and an `opt` module may only import other `opt`
    /// modules. See rfcs/performance-modes.md.
    pub modes: Vec<String>,
    /// Names of modules imported (side-effect-free: brings declarations into
    /// scope, runs no code, grants no authority). A `from X import Y` also lists
    /// `X` here (it implies `import X`), so all linker logic keyed on `imports`
    /// sees every imported module.
    pub imports: Vec<String>,
    /// (RFC-0042) `from X import Y, Z` — unqualified bindings of TYPE or function
    /// names into this module: `(module, [name, ...])`. Each listed name becomes
    /// usable bare (a type in annotations, a constructor bare, a function by its
    /// short name); two unqualified bindings of one name are a link-time error.
    /// Plain `import X` contributes nothing here — it binds no unqualified types.
    pub from_imports: Vec<(String, Vec<String>)>,
    pub items: Vec<Item>,
    /// Source line of each import and each top-level item, parallel to `imports`
    /// and `items` — used by the formatter to place comments. Empty when unknown
    /// (e.g. after linking, or when impl merging changed the item count).
    pub import_lines: Vec<u32>,
    pub item_lines: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Function(Function),
    Type(TypeDef),
    /// A trait (interface): a set of method signatures a type can implement.
    /// `trait Show { fn show(self) -> String }`.
    Trait(TraitDef),
    /// An implementation of a trait for a concrete type.
    /// `impl Show for Int { fn show(self) -> String { int_to_string(self) } }`.
    /// Lowered to ordinary functions before type-checking/codegen (see
    /// `crate::traits`), so later stages never see this variant.
    Impl(ImplDef),
    /// A module-level constant: `let MAX = 100`. Inlined at its use sites by
    /// `crate::consts` before type-checking/codegen, so later stages never see
    /// this variant.
    Const { name: String, value: Expr },
    /// A type alias: `type Id = Int` or `type Pair(a) = (a, a)`. Expanded to its target everywhere by
    /// `crate::aliases` before type-checking/codegen, so later stages never see
    /// this variant.
    TypeAlias { name: String, params: Vec<String>, ty: Type },
    /// `comptime:` — a block executed AT COMPILE TIME with no capabilities
    /// (deterministic by construction); everything it prints is parsed as
    /// witchy source and appended to the module as ADDITIVE items before
    /// type-checking and footprint analysis (rfcs/language-evolution.md
    /// Phase 5). Expanded by `crate::comptime` during linking, so later
    /// stages never see this variant.
    Comptime(Block),
}

/// A trait declaration: named method signatures (no bodies). The receiver is the
/// first parameter, conventionally named `self`, whose type is the implementing
/// type at each `impl`.
#[derive(Debug, Clone, PartialEq)]
pub struct TraitDef {
    pub name: String,
    /// `trait FromIterator(e):` — the trait's type parameters. Empty for the
    /// plain `trait Show:` form.
    pub typarams: Vec<String>,
    /// `trait Ord: Eq + PartialOrd:` — the direct supertraits. A type implementing
    /// this trait must also implement each of these, and a `where a: Ord` bound
    /// brings the supertraits' methods into scope.
    pub supertraits: Vec<String>,
    pub methods: Vec<MethodSig>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodSig {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    /// A default body (`fn m(self) -> T { ... }` inside the trait). Impls that
    /// don't provide this method inherit the default.
    pub default: Option<Block>,
}

/// `impl Trait for Type { <methods> }`, or an inherent `impl Type { <methods> }`
/// (`trait_name` is `None`). Each method is a full function whose first parameter
/// (`self`) stands for a value of `type_name`. Inherent methods are dispatched by
/// receiver type just like trait methods, but belong to no trait.
#[derive(Debug, Clone, PartialEq)]
pub struct ImplDef {
    pub origin: ImplOrigin,
    pub trait_name: Option<String>,
    /// `impl FromIterator(a) for …` — the trait's type arguments at this
    /// impl. Empty for unparameterized traits.
    pub trait_args: Vec<Type>,
    pub type_name: String,
    /// The target's type arguments — `[a]` for `impl … for List(a)`, `[a, b]` for a
    /// tuple impl, empty for a concrete target. They type the method `self` (so a
    /// generic impl's `self` is `List(a)`, not bare `List`) and pair with `bounds`.
    pub target_args: Vec<Type>,
    /// A `where` clause on the impl head — a CONDITIONAL impl that applies only
    /// when its target's type variables satisfy these bounds (`impl FromIterator(a)
    /// for Set(a) where a: Eq`). Each bound is `(var, trait, trait_args)`, the same
    /// shape as `Function::bounds`; the bounds are copied onto every generated
    /// method so its body's bounded calls type-check and monomorphize.
    pub bounds: Vec<(String, String, Vec<Type>)>,
    pub methods: Vec<Function>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImplOrigin {
    Source,
    CompilerGenerated,
}

/// A sum type: `type Event { Click(Int, Int) Closed }`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDef {
    pub name: String,
    /// Explicit type parameters: `type Pair(m, a):` is `["m", "a"]`. When present,
    /// these FIX the parameter order; otherwise the order is inferred from the
    /// variants' field types (order of first appearance). Explicit params matter
    /// when a constructor omits some of them (e.g. `Done(a)` for `Step(m, a)`) —
    /// inference can't recover the intended position of the omitted one.
    pub params: Vec<String>,
    pub variants: Vec<Variant>,
    /// `type T derive(Show, Eq, Ord):` — traits whose impls the compiler
    /// generates (additively, before checking; rfcs/language-evolution.md
    /// Phase 4). Empty for an undecorated type.
    pub derives: Vec<String>,
    /// `capability X from U` (RFC-0002): a SEALED one-variant brand over a host
    /// capability. Behaves like an ordinary single-variant type EXCEPT its
    /// constructor and pattern may only be used in the module that declares it
    /// (enforced at link time) — so a value of `X` can only be minted by `X`'s
    /// own module, making it un-forgeable like the host capability it refines.
    /// `false` for an ordinary `type`. Set to `true` by BOTH `capability X …`
    /// (RFC-0002) and `sealed type X …` (RFC-0065): the two share one enforcement
    /// mechanism — a value may be CONSTRUCTED/DESTRUCTURED only in its home module.
    /// `is_capability` distinguishes which surface produced it (for rendering and
    /// the diagnostic noun); the sealing check treats them identically.
    pub sealed: bool,
    /// `true` when the `sealed` flag came from a `capability` declaration (RFC-0002),
    /// `false` when it came from `sealed type` (RFC-0065). Only meaningful when
    /// `sealed` is set. Drives the formatter (`capability X` vs `sealed type X`) and
    /// the sealing diagnostic's noun ("sealed capability" vs "sealed type").
    pub is_capability: bool,
    /// `grantable capability X:` (RFC-0038): a sealed capability the host may mint
    /// at a root entrypoint (a parameter of `main`) from a grant document's
    /// `[user_caps]` section. Valid ONLY on a BARE capability — one carrying zero
    /// transitive host-capability authority — enforced at check time, so a
    /// grantable cap can never disguise a built-in host cap. `false` otherwise.
    pub grantable: bool,
    /// `type Point packed:` (RFC-0027): the type's values are stored INLINE inside
    /// containers (a flat `[f0,f1,…]` buffer) instead of behind a pointer — a
    /// cache-dense layout for fixed-scalar records. Requires all fields be packable
    /// (scalars or other `packed` types); enforced at check time. `false` for an
    /// ordinary `type`. The layout change is gated (opt-mode); the surface + the
    /// packability check are representation-neutral.
    pub packed: bool,
    /// (RFC-0047) Set by `derive::expand` when the type derives `PartialEq` (or
    /// `Eq`, which refines it): its equality impl is the STRUCTURAL default. A type
    /// with a hand-written `impl PartialEq for T` leaves this `false`. The whole-
    /// program `==`-through-PartialEq rule reads it: a container comparing elements
    /// of type `T` recurses structurally (the fast path) unless `T` has a *custom*
    /// (non-derived) impl, in which case that impl is called at every depth. This
    /// flag persists across the idempotent re-runs of `derive::expand` (it is set,
    /// not consumed, so a later empty-`derives` pass never clears it).
    pub partial_eq_derived: bool,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    /// Source line for the variant/record constructor header. Used by the formatter
    /// to attach comments to the right variant instead of flushing all comments at
    /// the top of the type body.
    pub line: u32,
    pub fields: Vec<Type>,
    /// For a *record* type (`type Point { x: Int, y: Int }`), the name of each
    /// field, parallel to `fields`. Empty for ordinary positional variants
    /// (`Circle(Int)`). A record is a single constructor with named fields.
    pub field_names: Vec<String>,
    /// Source lines for record fields, parallel to `field_names`/`fields`.
    pub field_lines: Vec<u32>,
}

impl PartialEq for Variant {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.fields == other.fields
            && self.field_names == other.field_names
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub public: bool,
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Block,
    /// Trait bounds from a `where` clause: `(type variable, trait)` pairs, e.g.
    /// `where a: Ord` is `("a", "Ord")`. Such a function is a generic template;
    /// `crate::traits` monomorphizes it per concrete instantiation.
    /// `where a: Ord` / `where c: FromIterator(a)` — (variable, trait,
    /// trait type-arguments).
    pub bounds: Vec<(String, String, Vec<Type>)>,
    /// `gen fn` — a generator whose `yield`s build a lazy `iter.Iter`. Lowered to
    /// ordinary functions by `crate::generators` before any later stage.
    pub is_gen: bool,
    /// `async fn` — a function that may `await`. Lowered to a resumable state
    /// machine driven by the executor by `crate::async_lower` (before type
    /// checking), so `await` genuinely suspends — not run sequentially.
    pub is_async: bool,
}

impl Function {
    /// (RFC-0043) Whether this function is a **mutator**: its first parameter has
    /// the `var` convention *and* the function returns that parameter's declared
    /// type. Its statement form (`xs.push(1)`) writes back the result; its
    /// expression form (`let ys = xs.push(1)`) is a pure value call — the `var`
    /// receiver does *not* trigger the procedure-style write-back that a
    /// Nil-returning `var` parameter does.
    ///
    /// Write-back is a DECLARED property here, not inferred from a whole-program
    /// name census: the fact lives in the signature, and resolution is per
    /// receiver type after method resolution.
    pub fn is_mutator(&self) -> bool {
        let Some(first) = self.params.first() else {
            return false;
        };
        if first.convention != Convention::Var {
            return false;
        }
        matches!(
            (first.ty.as_ref(), self.ret.as_ref()),
            (Some(pt), Some(rt)) if pt.unqualified() == rt.unqualified()
        )
    }

    /// (RFC-0043) Whether this function is a **procedure channel**: it has a `var`
    /// parameter and returns `Nil` (or nothing). Today's `var` semantics — the
    /// call-site argument must be a mutable `var` and the caller's variable is
    /// written back through the parameter. Distinct from a mutator by return shape.
    pub fn is_var_procedure(&self) -> bool {
        if !self.params.iter().any(|p| p.convention == Convention::Var) {
            return false;
        }
        match self.ret.as_ref() {
            None => true,
            Some(t) => matches!(t.unqualified(), Type::Named(n, _) if n == "Nil"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
    pub convention: Convention,
    /// (RFC-0056) A closed-constant default value: `port: Int = 443`. Omitting
    /// this argument at a *direct* call site splices the default in. `None` for a
    /// required parameter. Defaults never enter the function's type or value — a
    /// function value ignores them — so `crate::keyword_args::resolve` erases them
    /// (splices the constant) before typeck or either backend sees the call
    /// (parity by construction). A `var` parameter may not carry one (there is no
    /// caller variable to write back), and a defaulted parameter must be a suffix
    /// of the list.
    pub default: Option<Expr>,
}

/// Hylo-style parameter passing conventions (mutable value semantics).
/// `let` borrows immutably (default), `var` mutates in place and writes back,
/// `own` consumes (takes ownership / moves the value in).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Convention {
    /// The default (no keyword): an owned value, observably immutable to the
    /// caller (value semantics).
    #[default]
    Let,
    /// An explicit `let` keyword on a parameter: an immutable *borrow*. Same
    /// observable semantics as the default; the no-escape contract (a borrowed
    /// parameter may not be returned) is enforced by the type checker.
    Borrow,
    Var,
    Own,
}

impl Convention {
    /// Whether a parameter with this convention binds a *mutable* local (you may
    /// assign it in the body). `var` and `own` give a mutable value; `let` and
    /// an explicit borrow are read-only.
    pub fn binds_mutable(self) -> bool {
        matches!(self, Convention::Var | Convention::Own)
    }
}

/// Types are parsed but not yet checked. `Named("Result", [Int, Error])`.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Named(String, Vec<Type>),
    Tuple(Vec<Type>),
    /// A function type: `fn(Int, String) -> Bool`.
    Fn(Vec<Type>, Box<Type>),
    /// (RFC-0025/0026) An ownership/immutability qualifier on a type: `frozen T`,
    /// `unique T`, `local unique T`. A compile-time CONTRACT — typeck enforces it
    /// (a `frozen` value may not be mutated; a `unique` value must be the sole
    /// reference) but it carries no runtime representation: both backends lower the
    /// inner type identically, so it is invisible to codegen and parity-neutral.
    /// Use [`Type::unqualified`] to see through it where the qualifier is irrelevant.
    Qualified(TypeQual, Box<Type>),
}

/// An ownership/immutability qualifier (RFC-0025 `frozen`, RFC-0026 `unique`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeQual {
    /// `frozen` — deeply immutable; sharing is safe, mutation is a check-time error.
    Frozen,
    /// `unique` — the sole reference; may be returned as `unique`.
    Unique,
    /// `local unique` — unique within this activation only; may not escape.
    LocalUnique,
}

impl TypeQual {
    /// The source keyword(s) for this qualifier — for formatting / reflection.
    pub fn as_str(self) -> &'static str {
        match self {
            TypeQual::Frozen => "frozen",
            TypeQual::Unique => "unique",
            TypeQual::LocalUnique => "local unique",
        }
    }
}

impl Type {
    /// See through any ownership/immutability qualifiers to the underlying type —
    /// for the many sites where `frozen T` / `unique T` behave exactly as `T`
    /// (shape, lowering, generic structure). Recurses through stacked qualifiers.
    pub fn unqualified(&self) -> &Type {
        match self {
            Type::Qualified(_, inner) => inner.unqualified(),
            other => other,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Source line of each statement (parallel to `stmts`), for diagnostics.
    pub lines: Vec<u32>,
    /// A `region:` allocation scope: everything allocated inside is reclaimed
    /// at the block's end and the block's VALUE is what escapes (deep-copied
    /// out on the WASM tier). Purely a reclamation annotation — a region
    /// never changes observable behavior, only when memory is freed; the
    /// interpreter runs the block normally. `None` for an ordinary block.
    pub region: Option<RegionAnn>,
}

/// A `region:` / `region -> T:` annotation. The optional type guarantees the
/// copy-out shape at check time instead of inferring it from the tail.
#[derive(Debug, Clone, PartialEq)]
pub struct RegionAnn {
    pub ty: Option<Type>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `let x = e` or `var x = e`, optionally ascribed: `let x: T = e`.
    /// The ascription is a unification constraint — it can PIN type variables
    /// the right-hand side leaves open (an empty `[]`, a return-position
    /// type variable), and it fails loudly when the RHS disagrees.
    Let {
        name: String,
        /// `let x: T = …` — None for the inferred form.
        ty: Option<Type>,
        mutable: bool,
        value: Expr,
    },
    /// `x = e` — reassign an existing binding (e.g. a `var` accumulator).
    Assign {
        name: String,
        value: Expr,
    },
    /// `let PAT = e` — destructure via an irrefutable pattern into immutable
    /// bindings (RFC-0052). `PAT` is any pattern the refutability checker proves
    /// always matches: a tuple (any nesting), a single-variant record/ctor with
    /// irrefutable fields, a wildcard, a variable. A refutable pattern here is a
    /// check-time error pointing at `if let`. Replaces the old flat-names
    /// `LetTuple` — every binding position now shares one grammar.
    LetPattern {
        pattern: Pattern,
        value: Expr,
    },
    /// `return e` (or bare `return`) — exit the enclosing function early with a
    /// value (or Nil).
    Return(Option<Expr>),
    /// `break` — exit the innermost enclosing loop.
    Break,
    /// `continue` — skip to the next iteration of the innermost enclosing loop.
    Continue,
    /// `yield e` — produce a value from a `gen fn`. Only valid inside a generator;
    /// `crate::generators::lower` rewrites it (and the enclosing `gen fn`) into a
    /// lazy `iter.Iter`, so later stages never see it.
    Yield(Expr),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    /// A duration literal (`30s`, `2hr`, ...), carried as whole milliseconds and
    /// typed as the distinct `Duration` type.
    Duration(i64),
    Str(String),
    Bool(bool),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    /// A bare lowercase identifier — a variable or function reference.
    Var(String),
    /// A call to a named function: `f(a, b)`.
    Call { name: String, args: Vec<Expr> },
    /// (RFC-0056) A *direct* call whose arguments carry keyword labels:
    /// `substring(s, start: 2, end: 7)`. A positional argument has label `None`;
    /// a labeled one carries `Some(param_name)`. Produced by the parser ONLY when
    /// at least one argument is labeled (an all-positional call stays `Call`).
    /// `crate::keyword_args::resolve` rewrites it — validating and reordering the
    /// arguments against the callee's declared parameter list, binding each written
    /// argument to a temp in SOURCE order to preserve evaluation order, and
    /// splicing any defaults — into a positional `Call` (optionally wrapped in a
    /// temp-binding `Block`), so labels never reach typeck or either backend
    /// (parity by construction). Labels are a property of the *declaration*, not of
    /// a function value: only calls with a statically-known callee may carry them
    /// (method calls and value calls may not — the parser rejects those).
    LabeledCall { name: String, args: Vec<(Option<String>, Expr)> },
    /// `receiver.method(args)` — UFCS method-call sugar for `method(receiver,
    /// args)`. Kept as a node (rather than flattened at parse time) so the
    /// formatter can print it back; every other consumer lowers it via
    /// `parser::desugar_method`. (Module-qualified calls like `json.decode(x)`
    /// are plain `Call`s whose name carries the `.`, so they are unaffected.)
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        args: Vec<Expr>,
    },
    /// Application of an arbitrary expression that evaluates to a function:
    /// `make_adder(3)(4)`, `(fn(x){x})(1)`. (A bare-name call is `Call`.)
    Apply { func: Box<Expr>, args: Vec<Expr> },
    /// A constructor application: `Click(x, y)` or nullary `Closed`.
    Ctor { name: String, args: Vec<Expr> },
    /// An anonymous tagged-union injection: `.NotFound` or `.BadPort(70000)`.
    /// The concrete union type is supplied by an expected type at check time.
    AnonCtor { tag: String, args: Vec<Expr> },
    Unary { op: UnOp, expr: Box<Expr> },
    /// Record field access: `point.x`. (Module-qualified calls `mod.func(...)`
    /// are parsed as `Call`, not `Field`.)
    Field { base: Box<Expr>, field: String },
    /// An anonymous function (closure): `fn(x: Int): x + 1`. Captures the
    /// environment it is created in. `ret` is the optional declared return type
    /// (`fn(x: Int) -> Bool: ...`); when present it makes the closure a `?`
    /// boundary with that exact `Result`/`Option` type.
    Lambda { params: Vec<Param>, body: Block, ret: Option<Type> },
    /// Record update: a new record like `p` with the named fields replaced. `p`
    /// is not mutated. `name` is `Some(Point)` when produced by lowering the
    /// surface spread form `Point(x: 5, ..p)`, and `None` for internal updates
    /// such as field assignment desugaring (`p.x = v`).
    RecordUpdate {
        name: Option<String>,
        base: Box<Expr>,
        fields: Vec<(String, Expr)>,
    },
    /// Named-field record construction: `Point(x: 1, y: 2)`, optionally with a
    /// spread base `Point(x: 5, ..p)` (fields not named come from `p`). Kept as a
    /// node so the formatter can print it; `crate::records` lowers it to a
    /// positional `Ctor` (no spread) or a `RecordUpdate` (spread) using the
    /// type's declared field order, so later stages never see it.
    Record {
        name: String,
        fields: Vec<(String, Expr)>,
        spread: Option<Box<Expr>>,
    },
    /// `e?` — propagate a `Result`/`Option`: unwrap `Ok`/`Some` to its payload,
    /// or short-circuit, returning the `Err`/`None` from the enclosing function.
    Try(Box<Expr>),
    /// `e as T` — a capability *narrowing* ascription: re-type a capability to a
    /// subset of its rights (`net as Net[Connect]`). Checked statically (the
    /// target rights must be a subset of the source's); identity at runtime.
    As { expr: Box<Expr>, ty: Type },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    If {
        cond: Box<Expr>,
        then_block: Block,
        else_block: Option<Block>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    Block(Block),
    /// `while cond { body }` — iterate while `cond` holds; evaluates to Nil.
    While {
        cond: Box<Expr>,
        body: Block,
    },
    /// `for x in list { body }` — bind each element of `list` to `x` in turn;
    /// evaluates to Nil.
    For {
        var: String,
        iter: Box<Expr>,
        body: Block,
    },
    /// `lo..hi` (half-open) or `lo..=hi` (inclusive) — an integer range that
    /// evaluates to the `List(Int)` it spans. Kept as a node (rather than
    /// desugared at parse time) so the formatter can print it back as `lo..hi`;
    /// every other consumer lowers it via `parser::desugar_range`.
    Range {
        lo: Box<Expr>,
        hi: Box<Expr>,
        inclusive: bool,
    },
    /// `base[index]` — list subscript, sugar for `at(base, index)`. Kept as a
    /// node (rather than desugared at parse time) so the formatter can print it
    /// back as `base[index]`; every other consumer lowers it via
    /// `parser::desugar_index`.
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    /// `while let PAT = SCRUT: body` — loop while the scrutinee keeps matching
    /// `PAT`, binding its variables in the body each iteration. Kept as a node
    /// (rather than desugared at parse time) so the formatter can print it back;
    /// every other consumer lowers it via `parser::desugar_while_let` to a
    /// `while true` over a match whose wildcard arm breaks.
    WhileLet {
        pattern: Pattern,
        scrutinee: Box<Expr>,
        body: Block,
    },
    /// `tag"a${x}b"` — a compile-time tagged literal (RFC-0006). `tag` is an
    /// ordinary compile-time function `fn(parts: List(String), holes: List(String))
    /// -> String` that returns witchy EXPRESSION SOURCE. `parts` are the static
    /// fragments (`["a", "b"]`); `holes` are each interpolation's SOURCE TEXT
    /// (`["x"]`); `hole_spans` is each hole's `(line, col)` start position in the
    /// ORIGINAL source. `crate::tagged::expand` passes the tag an OPAQUE MARKER
    /// (`__witchy_hole_N`) per hole — the tag places markers where the hole's value
    /// goes — runs `tag(parts, markers)` at compile time, parses the returned
    /// string as an expression, then SUBSTITUTES the real hole expression (parsed
    /// once from `holes[N]` and STAMPED with `hole_spans[N]`) at each marker. This
    /// is RFC-0006's hygiene split: tag-emitted nodes carry the tag's scope (the
    /// tag emits qualified `glamour.text(…)` etc.), hole nodes carry the call site.
    /// The result is spliced in place of this node before type-checking and codegen,
    /// so both backends consume the same expanded AST. UNREACHABLE after expansion:
    /// typeck, the interpreter, and both codegen backends panic if they ever see one.
    TaggedLit {
        tag: String,
        parts: Vec<String>,
        holes: Vec<String>,
        hole_spans: Vec<(u32, u32)>,
        line: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
    /// `move x` — a use-site ownership transfer. Value-neutral (it evaluates to
    /// its operand); the caller relinquishes the binding (use-after-move is a
    /// compile error). Carried as a unary op so every AST walker that already
    /// recurses through `Unary` handles it transparently.
    Move,
    /// `await e` — a suspension point inside an `async fn`. Value-neutral like
    /// `move` (Phase 1 has no executor, so it evaluates to its operand and runs
    /// sequentially); carried as a unary op so it survives to `fmt` and so every
    /// `Unary` walker handles it transparently until the executor lands.
    Await,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Concat,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    /// `a ?? b` (RFC-0048): the fallback operator. `Option(T) ?? T -> T` /
    /// `Result(T, e) ?? T -> T` — unwrap `Some`/`Ok`, or evaluate the right
    /// side on `None`/`Err` (lazily; the error value is discarded).
    /// Right-associative, one level looser than `||`.
    Coalesce,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    /// Source line for the arm pattern. Used by the formatter for comment
    /// attachment inside match bodies.
    pub line: u32,
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}

impl PartialEq for MatchArm {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern && self.guard == other.guard && self.body == other.body
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard,
    Var(String),
    Int(i64),
    Str(String),
    Bool(bool),
    Ctor { name: String, args: Vec<Pattern> },
    /// An anonymous tagged-union pattern: `.NotFound` or `.BadPort(p)`.
    /// The scrutinee type supplies the closed tag set during type checking.
    AnonCtor { tag: String, args: Vec<Pattern> },
    Tuple(Vec<Pattern>),
    /// A list pattern. `elems` are matched positionally against the front of the
    /// list. `rest` controls the tail: `None` requires an exact-length match
    /// (`[a, b]`); `Some(None)` matches any remaining tail and ignores it
    /// (`[a, ..]`); `Some(Some(name))` binds the remaining tail as a list
    /// (`[a, ..rest]`).
    List {
        elems: Vec<Pattern>,
        rest: Option<Option<String>>,
    },
    /// A duration literal pattern (`1s`, `-1s`), carried as whole milliseconds —
    /// exact `i64` equality against a `Duration` scrutinee (RFC-0052). No float
    /// hazard, since a Duration is an exact millisecond count.
    Duration(i64),
    /// An integer range pattern (RFC-0052): `lo..hi` (half-open) or `lo..=hi`
    /// (inclusive), usable at any depth. A real AST node (not the old
    /// guard-desugar) so exhaustiveness can reason about it — though the
    /// classifier still treats ranges as refutable (no numeric-coverage
    /// analysis), so a range match still needs a final `_`/binding arm.
    IntRange { lo: i64, hi: i64, inclusive: bool },
    /// An or-pattern (RFC-0052): `p1 | p2 | …`, a real AST node usable at any
    /// depth (`Some(1 | 2)`, `[1 | 2, ..rest]`). Every alternative must bind the
    /// same names at the same types (the checker enforces binding-consistency).
    Or(Vec<Pattern>),
}

/// Collect the variables an irrefutable pattern binds, in appearance order.
/// (Or-patterns/ranges/literals bind nothing here; the checker rejects those in
/// irrefutable position, and for or-patterns all alternatives bind the same set,
/// so the first alternative's names would suffice — but they never reach the
/// irrefutable binding paths that call this.)
pub fn pattern_binds(p: &Pattern, out: &mut Vec<String>) {
    match p {
        Pattern::Var(n) if n != "_" => out.push(n.clone()),
        Pattern::Wildcard | Pattern::Var(_) => {}
        Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. } => {
            args.iter().for_each(|a| pattern_binds(a, out));
        }
        Pattern::Tuple(ps) => ps.iter().for_each(|q| pattern_binds(q, out)),
        Pattern::List { elems, rest } => {
            elems.iter().for_each(|q| pattern_binds(q, out));
            if let Some(Some(name)) = rest {
                out.push(name.clone());
            }
        }
        Pattern::Or(alts) => {
            // All alternatives bind the same names (checker-enforced); take the
            // first so callers that only need the *set* of names are correct.
            if let Some(first) = alts.first() {
                pattern_binds(first, out);
            }
        }
        Pattern::Int(_) | Pattern::Str(_) | Pattern::Bool(_) | Pattern::Duration(_)
        | Pattern::IntRange { .. } => {}
    }
}

/// Collect every type *name* (`Type::Named` head) in `t` into any `Extend<String>`
/// sink — a `Vec` to keep appearance order, a `HashSet` for set membership.
/// Recurses through tuple/function components and generic arguments.
pub fn collect_type_names<S: Extend<String>>(t: &Type, out: &mut S) {
    match t {
        Type::Qualified(_, inner) => collect_type_names(inner, out),
        Type::Named(name, args) => {
            out.extend([name.clone()]);
            for a in args {
                collect_type_names(a, out);
            }
        }
        Type::Tuple(ts) => {
            for t in ts {
                collect_type_names(t, out);
            }
        }
        Type::Fn(params, ret) => {
            for p in params {
                collect_type_names(p, out);
            }
            collect_type_names(ret, out);
        }
    }
}

/// (RFC-0047) The set of type names that have a CUSTOM (hand-written, non-derived)
/// `PartialEq` impl in the linked module — a whole-program fact both backends
/// consume so `==`/`!=` honor a custom impl at every depth (inside a `List`,
/// `Option`, tuple, `Dict`, record, …). A type whose `PartialEq` comes from
/// `derive(PartialEq)` or an Eq-implied structural PartialEq is the STRUCTURAL
/// default and is NOT in the set, so containers of it keep the fast structural
/// compare (identical code to today). Called on the single linked module, so
/// `derive::expand` has already run and stamped `TypeDef::partial_eq_derived`.
///
/// A type is "custom-eq" when there is an `impl PartialEq for T` AND `T`'s
/// declaration did not generate a structural PartialEq. (A type with a hand impl
/// plus marker `derive(Eq)` has `partial_eq_derived == false`; a derived
/// structural PartialEq has it `true`.)
pub fn custom_partial_eq_types(module: &Module) -> HashSet<String> {
    // Types that DERIVED their PartialEq — their generated impl is structural.
    let mut derived: HashSet<&str> = HashSet::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            if t.partial_eq_derived {
                derived.insert(t.name.as_str());
            }
        }
    }
    let mut custom: HashSet<String> = HashSet::new();
    for item in &module.items {
        if let Item::Impl(im) = item {
            let is_partial_eq = im.trait_name.as_deref() == Some("PartialEq");
            // The std/cmp primitive impls (`impl PartialEq for Int`, `String`, …)
            // ARE the structural/native default expressed as an impl — never route a
            // container of them through a call; both backends compare primitives
            // inline. Exclude those names so only genuinely user-defined equalities
            // (records/enums with a hand impl) enter the set.
            let is_primitive = matches!(
                im.type_name.as_str(),
                "Int" | "Bool" | "Float" | "String" | "Duration" | "Bytes"
            );
            if is_partial_eq && !is_primitive && !derived.contains(im.type_name.as_str()) {
                custom.insert(im.type_name.clone());
            }
        }
    }
    custom
}

/// Collect the type *variables* in `t` — argument-less, lowercase-initial
/// `Type::Named` heads — in first-appearance order, without duplicates (the
/// parameters a generic signature is implicitly quantified over).
pub fn collect_type_vars(t: &Type, out: &mut Vec<String>) {
    match t {
        Type::Qualified(_, inner) => collect_type_vars(inner, out),
        Type::Named(name, args) => {
            if args.is_empty()
                && name.chars().next().is_some_and(char::is_lowercase)
                && !name.contains('.')
                && !out.iter().any(|v| v == name)
            {
                out.push(name.clone());
            }
            for a in args {
                collect_type_vars(a, out);
            }
        }
        Type::Tuple(ts) => {
            for t in ts {
                collect_type_vars(t, out);
            }
        }
        Type::Fn(params, ret) => {
            for p in params {
                collect_type_vars(p, out);
            }
            collect_type_vars(ret, out);
        }
    }
}
