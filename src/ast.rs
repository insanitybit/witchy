//! Abstract syntax tree for witchy.
//!
//! Convention (Gleam-style): identifiers beginning with an uppercase letter are
//! constructors/variants (`Click`, `Closed`); lowercase identifiers are
//! variables and functions (`greet`, `count`).

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    /// The performance mode declared at the top of the file (`mode opt`), or empty
    /// for an ordinary file. The linker copies the entry module's modes onto the
    /// linked module; enforcement (cliffs → errors) then applies to the entry
    /// file's own functions, and an `opt` module may only import other `opt`
    /// modules. See rfcs/performance-modes.md.
    pub modes: Vec<String>,
    /// Names of modules imported (side-effect-free: brings declarations into
    /// scope, runs no code, grants no authority).
    pub imports: Vec<String>,
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
    /// A type alias: `type Id = Int`. Expanded to its target everywhere by
    /// `crate::aliases` before type-checking/codegen, so later stages never see
    /// this variant.
    TypeAlias { name: String, ty: Type },
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
    /// `false` for an ordinary `type`.
    pub sealed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Type>,
    /// For a *record* type (`type Point { x: Int, y: Int }`), the name of each
    /// field, parallel to `fields`. Empty for ordinary positional variants
    /// (`Circle(Int)`). A record is a single constructor with named fields.
    pub field_names: Vec<String>,
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
    /// `async fn` — a function that may `await`. Phase 1: surface + coloring only
    /// (the body runs sequentially; `await e` is parsed as `e`). Later phases
    /// lower an async fn to a resumable state machine driven by the executor.
    pub is_async: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
    pub convention: Convention,
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Source line of each statement (parallel to `stmts`), for diagnostics.
    pub lines: Vec<u32>,
    /// A `retain`/`without` capability firewall on this block: inside it, only
    /// the named capabilities stay in scope (`retain`) or the named ones are
    /// dropped (`without`). Purely a compile-time scoping restriction — the type
    /// checker hides the bindings so the block is sealed against capabilities the
    /// outer scope might gain; every backend runs the block normally (capabilities
    /// are erased at runtime). `None` for an ordinary block.
    pub restrict: Option<CapRestrict>,
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

/// A block-level capability restriction introduced by `retain`/`without`.
#[derive(Debug, Clone, PartialEq)]
pub struct CapRestrict {
    pub mode: RestrictMode,
    /// The capability variables named: kept (for `retain`) or dropped (for
    /// `without`).
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestrictMode {
    /// `retain a, b:` — only `a` and `b` stay in scope; every other capability
    /// is hidden inside the block.
    Retain,
    /// `without a, b:` — `a` and `b` are dropped; everything else stays.
    Without,
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
    /// `let (a, b) = e` — destructure a tuple into immutable bindings.
    LetTuple {
        names: Vec<String>,
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
    /// is not mutated. Produced by lowering an `Expr::Record` whose `spread` is
    /// set (`Point(x: 5, ..p)`); no longer has surface syntax of its own.
    RecordUpdate {
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
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard,
    Var(String),
    Int(i64),
    Str(String),
    Bool(bool),
    Ctor { name: String, args: Vec<Pattern> },
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
}
