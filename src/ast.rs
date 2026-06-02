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
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Function(Function),
    Actor(ActorDef),
    Type(TypeDef),
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
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    /// A bare lowercase identifier — a variable or function reference.
    Var(String),
    /// A call to a named function: `f(a, b)`.
    Call { name: String, args: Vec<Expr> },
    /// A constructor application: `Click(x, y)` or nullary `Closed`.
    Ctor { name: String, args: Vec<Expr> },
    Unary { op: UnOp, expr: Box<Expr> },
    /// Record field access: `point.x`. (Module-qualified calls `mod.func(...)`
    /// are parsed as `Call`, not `Field`.)
    Field { base: Box<Expr>, field: String },
    /// An anonymous function (closure): `fn(x: Int) { x + 1 }`. Captures the
    /// environment it is created in.
    Lambda { params: Vec<Param>, body: Block },
    /// `e?` — propagate a `Result`/`Option`: unwrap `Ok`/`Some` to its payload,
    /// or short-circuit, returning the `Err`/`None` from the enclosing function.
    Try(Box<Expr>),
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
