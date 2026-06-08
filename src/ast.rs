//! Abstract syntax tree for witchy.
//!
//! Convention (Gleam-style): identifiers beginning with an uppercase letter are
//! constructors/variants (`Click`, `Closed`); lowercase identifiers are
//! variables and functions (`greet`, `count`).

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    /// Names of modules imported (side-effect-free: brings declarations into
    /// scope, runs no code, grants no authority).
    pub imports: Vec<String>,
    pub items: Vec<Item>,
    /// Source line of each import and each top-level item, parallel to `imports`
    /// and `items` — used by the formatter to place comments. Empty when unknown
    /// (e.g. after linking, or when actor-impl merging changed the item count).
    pub import_lines: Vec<u32>,
    pub item_lines: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Function(Function),
    Actor(ActorDef),
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
}

/// A trait declaration: named method signatures (no bodies). The receiver is the
/// first parameter, conventionally named `self`, whose type is the implementing
/// type at each `impl`.
#[derive(Debug, Clone, PartialEq)]
pub struct TraitDef {
    pub name: String,
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
    pub type_name: String,
    pub methods: Vec<Function>,
    /// Message handlers (`on Msg(...) { ... }`) for an inherent impl on an actor
    /// type. Merged into the target `ActorDef` right after parsing, so later
    /// stages see them on the actor; empty for ordinary trait/struct impls.
    pub handlers: Vec<Handler>,
}

/// A sum type: `type Event { Click(Int, Int) Closed }`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDef {
    pub name: String,
    pub variants: Vec<Variant>,
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
pub struct ActorDef {
    pub name: String,
    pub fields: Vec<Field>,
    pub handlers: Vec<Handler>,
}

/// Actor state. A field with an initializer (`var count: Int = 0`) defaults at
/// spawn; a field without one (`console: Console`) must be supplied at spawn —
/// this is how capabilities are granted to an actor.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub mutable: bool,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    pub message: String,
    pub params: Vec<Param>,
    pub body: Block,
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
    pub bounds: Vec<(String, String)>,
    /// `gen fn` — a generator whose `yield`s build a lazy `iter.Iter`. Lowered to
    /// ordinary functions by `crate::generators` before any later stage.
    pub is_gen: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
    pub convention: Convention,
}

/// Hylo-style parameter passing conventions (mutable value semantics).
/// `let` borrows immutably (default), `inout` mutates in place and writes back,
/// `sink` consumes (takes ownership / moves the value in).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Convention {
    #[default]
    Let,
    Inout,
    Sink,
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
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `let x = e` or `var x = e`.
    Let {
        name: String,
        mutable: bool,
        value: Expr,
    },
    /// `x = e` — reassign an existing binding (e.g. actor state).
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
    /// An anonymous function (closure): `fn(x: Int) { x + 1 }`. Captures the
    /// environment it is created in.
    Lambda { params: Vec<Param>, body: Block },
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
    /// `spawn ActorName(args)` — create an actor, granting it the args as its
    /// initial (non-defaulted) fields. Evaluates to a `Subject`.
    Spawn {
        actor: String,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
    /// `move x` — a use-site ownership transfer. Value-neutral (it evaluates to
    /// its operand); on the native backend it forces a move (no clone) so the
    /// caller relinquishes the binding. Carried as a unary op so every AST walker
    /// that already recurses through `Unary` handles it transparently.
    Move,
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
