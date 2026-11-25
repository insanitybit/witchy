//! Syntax only: parsing a program does not certify ownership or authorize execution.
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Unit,
    Named(String, Vec<Type>),
    Borrow { mutable: bool, inner: Box<Type> },
    Array { element: Box<Type>, length: u64 },
    Slice(Box<Type>),
    /// `.{label: Type, ...}`: a record identified by its labelled fields alone.
    Record(Vec<(String, Type)>),
    /// `.[Tag | Tag(Type)]`: a closed sum identified by its tags and what they hold.
    Union(Vec<(String, Option<Type>)>),
}

impl Type {
    /// The type as written. A bound with a trait argument (`Q: Equivalent<K>`) is kept as this text.
    pub fn render(&self) -> String {
        let list = |types: &[Type]| types.iter().map(Type::render).collect::<Vec<_>>().join(", ");
        match self {
            Type::Unit => "()".into(),
            Type::Named(name, arguments) if arguments.is_empty() => name.clone(),
            Type::Named(name, arguments) => format!("{name}<{}>", list(arguments)),
            Type::Borrow { mutable, inner } => format!("&{}{}", if *mutable { "mut " } else { "" }, inner.render()),
            Type::Array { element, length } => format!("[{}; {length}]", element.render()),
            Type::Slice(inner) => format!("[{}]", inner.render()),
            Type::Record(fields) => format!(".{{{}}}", fields.iter().map(|(n, t)| format!("{n}: {}", t.render())).collect::<Vec<_>>().join(", ")),
            Type::Union(variants) => format!(".[{}]", variants.iter().map(|(n, t)| match t { Some(t) => format!("{n}({})", t.render()), None => n.clone() }).collect::<Vec<_>>().join(" | ")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expr {
    pub kind: Expression,
    pub span: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expression {
    Unit,
    Integer(u64),
    /// `1.5`: the 64 bits of an F64.
    Float(u64),
    Bool(bool),
    /// Caller position, resolved when checking a direct call to a source-site parameter.
    SourceSite(String),
    Name(String),
    /// Ghost-only bounded universal quantification; never a runtime iterator.
    Forall { variable: String, lower: Box<Expr>, upper: Box<Expr>, body: Box<Expr> },
    /// A specification-only match; a guarded clause has one arm and is vacuous elsewhere.
    SpecCase { value: Box<Expr>, arms: Vec<(String, Option<String>, Expr)>, guarded: bool },
    Array(Vec<Expr>),
    Repeat(Box<Expr>, u64),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    /// `name: value` as a call's argument: the parameter (or record field) it is given to. The
    /// checker verifies it and takes it away before anything else reads the call.
    Labelled(String, Box<Expr>),
    /// `if let Variant(x) = value: a else: b` on one line: the condition of that `Conditional`,
    /// the value and the one arm (with no body). Taken out with the `Conditional` before its
    /// statement and made a `match` (wasm/early_exit.rs); valid nowhere else.
    Matches(Box<Expr>, Box<MatchArm>),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Try(Box<Expr>),
    /// `if condition: yes else: no` on one line: a value. Lowered to a `Choose` before the
    /// statement that holds it (wasm/early_exit.rs).
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `.{label: value, ...}`.
    Record(Vec<(String, Expr)>),
    /// `.Tag` or `.Tag(value)`: a value of an anonymous sum, whose type comes from the context.
    Inject(String, Option<Box<Expr>>),
}

/// A proof justification attached to a binding. Parsing records it; only the
/// lowering decides whether a rule binds to emitted operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Justification {
    /// A named checked rule that Lean must discharge against the emitted code.
    Rule(String),
}

/// A declared representation invariant naming record fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invariant {
    pub predicate: String,
    pub fields: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProofStep {
    Auto,
    Have(Expr),
    NamedHave { name: String, claim: Expr },
    Apply(Expr),
    ApplyUsing { application: Expr, facts: Option<Vec<String>>, binding: Option<String> },
    Exact { name: String, span: Range<usize> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lemma {
    pub name: String,
    pub parameters: Vec<(String, Type)>,
    pub requires: Vec<Expr>,
    pub ensures: Expr,
    pub proof: Vec<ProofStep>,
}

/// Where a path that must not be taken was written, and what wrote it: a refusal of its proof
/// points here. Only the parser makes `Written`; the others are the checker's own lowerings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site { pub span: std::ops::Range<usize>, pub why: Why }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// `unreachable`, as written.
    Written,
    /// The owner arm of `place = value` over a field (check.rs `vacant_assign`): the span is the place's.
    Overwrite,
    /// The owner arm of `memory.vacant(value)` (check.rs `vacant_discard`): the span is the value's.
    Discard,
    /// A step the checker wrote out for another statement (an element assignment's missing index).
    Lowered,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Statement {
    Proof(Vec<ProofStep>),
    Assert(Expr),
    Bind {
        mutable: bool,
        name: String,
        ty: Option<Type>,
        value: Expr,
        proof: Option<Justification>,
    },
    /// `let Record(a, b) = value`: take a whole record apart, binding every field.
    Open { record: String, names: Vec<String>, value: Expr },
    /// `let .{label: name, ...} = value`: the same for an anonymous record, by label.
    /// `let .{a, b} = v`, `let (a, b) = v`, `var (a, b): (U64, U64) = v`: each label's value
    /// bound to its name, mutable for `var`, the value checked against the type when one is given.
    OpenFields { fields: Vec<(String, String)>, value: Expr, mutable: bool, ty: Option<Type> },
    /// Refutable binding; the failure arm must exit with explicit disposition.
    /// `let Ok(name): Type = value else Err(e): ...`; the written type says what `name` is.
    Guard { value: Expr, variant: String, binding: String, ty: Option<Type>, failure: MatchArm },
    Assign {
        target: Expr,
        operation: &'static str,
        value: Expr,
    },
    Return(Option<Expr>),
    /// Leave the innermost loop.
    Break,
    /// Go on to the innermost loop's next step.
    Continue,
    /// Stop the run with a trap: a path the program holds cannot be taken.
    Unreachable(Site),
    /// `using name = value:` an owner discarded on every way out of the block (`src/compiler/using.rs`).
    Using { mutable: bool, name: String, ty: Option<Type>, value: Expr, body: Vec<Statement> },
    /// A scope: its names end with it. Only made by rewrites, never written.
    Block(Vec<Statement>),
    /// `call` (a `name.discard()`) if `name` is still held here, nothing if it has been moved
    /// away. Made by the `using` rewrite; the checker decides from what it knows of `name`.
    Dispose { name: String, call: Expr },
    While {
        condition: Expr,
        invariants: Vec<Expr>,
        body: Vec<Statement>,
    },
    /// `for name in start..end:` counts; `for name in container:` walks by reference.
    /// `index` names the position of each element in a `for index, item in container:`.
    For { index: Option<String>, binding: String, start: Expr, end: Option<Expr>, invariants: Vec<Expr>, body: Vec<Statement> },
    If {
        condition: Expr,
        yes: Vec<Statement>,
        no: Vec<Statement>,
    },
    Match {
        value: Expr,
        arms: Vec<MatchArm>,
    },
    /// `let name = match ...:` and `let name = if ...:`. `inner` is that `match` or `if`;
    /// every path through it ends in a `Yield` of the name or leaves.
    Choose { mutable: bool, name: String, ty: Option<Type>, inner: Box<Statement> },
    /// The value the enclosing `Choose` of `name` takes on this path.
    Yield { name: String, value: Expr },
    Expr(Expr),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchArm {
    pub variant: String,
    pub binding: Option<String>,
    pub body: Vec<Statement>,
    /// `Variant(Inner(x))`: the pattern the variant's own value must match in turn.
    pub nested: Option<Box<Pattern>>,
    /// `Variant(a, b)`: a name for each of the values the variant holds; empty otherwise.
    pub open: Vec<String>,
}

/// A pattern inside a match arm's parentheses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    pub variant: String,
    pub binding: Option<String>,
    pub nested: Option<Box<Pattern>>,
    pub open: Vec<String>,
}

/// Pure postconditions have their own pattern scopes and never emit runtime code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Postcondition {
    pub kind: PostconditionKind,
    pub span: Range<usize>,
}

impl Postcondition {
    pub(crate) fn truth(span: Range<usize>) -> Self {
        Self {
            kind: PostconditionKind::Predicate(Expr { kind: Expression::Bool(true), span: span.clone() }),
            span,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostconditionKind {
    Predicate(Expr),
    Match { value: Expr, arms: Vec<PostconditionArm> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostconditionArm {
    pub variant: String,
    pub binding: Option<String>,
    pub clauses: Vec<Postcondition>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub generics: Vec<String>,
    /// Per type parameter: declared `T: Any`, so the body may not copy or drop a `T`.
    pub any: Vec<bool>,
    /// Per type parameter: the traits it must implement.
    pub bounds: Vec<Vec<String>>,
    /// A trait's method: a signature for implementations to match, with no body.
    pub required: bool,
    /// Index of the source file the declaration was written in.
    pub module: u32,
    /// `pub fn`: callable from other modules. Trait methods are public with their trait.
    pub public: bool,
    /// `pure fn`: checked to be given no capability, so calling it has no effect.
    pub pure: bool,
    /// `nopanic fn`: checked, once its proofs are accepted, to have no way to trap in what is
    /// emitted, through what it calls too (src/compiler/traps.rs). The keyword's place.
    pub nopanic: Option<std::ops::Range<usize>>,
    /// Where its name is written: refusals of the function as a whole are placed there.
    pub span: std::ops::Range<usize>,
    /// `-> T suspends`: it may pause its task (docs/design-async.md). On a trait method, its
    /// implementations may.
    pub suspends: bool,
    /// Per type parameter: `T: Read nosuspend`, so no method of `T` the body calls may pause.
    pub nosuspend: Vec<bool>,
    /// The source ranges of the body's `nosuspend:` blocks: no call inside may pause.
    pub quiet: Vec<(usize, usize)>,
    pub parameters: Vec<(String, Type)>,
    /// Per parameter: `by: U64 = 10`, the literal a call that leaves it out gives it.
    pub defaults: Vec<Option<Expr>>,
    pub result: Type,
    pub body: Vec<Statement>,
    pub requires: Vec<Expr>,
    pub ensures: Vec<Postcondition>,
    /// `borrows Ok from left`: the references a result of that variant holds borrow only from
    /// these parameters (docs/wasm-references.md, "Where a variant borrows from").
    pub borrows: Vec<Borrows>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Borrows {
    pub variant: String,
    pub from: Vec<String>,
    pub span: std::ops::Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Declaration {
    Lemma(Lemma),
    Predicate { module: u32, public: bool, name: String, generics: Vec<String>, parameters: Vec<(String, Type)>, body: Expr },
    Function(Function),
    Impl {
        generics: Vec<String>,
        /// Per type parameter: declared `T: Any`.
        any: Vec<bool>,
        /// Per type parameter: the traits it must implement.
        bounds: Vec<Vec<String>>,
        /// `impl Trait for Type`: the trait these methods implement.
        implements: Option<String>,
        /// `impl Equivalent<String> for Text`: the trait's type arguments.
        trait_arguments: Vec<Type>,
        target: Type,
        methods: Vec<(Function, Range<usize>)>,
        span: Range<usize>,
    },
    /// `trait Name:` followed by method signatures. `Self` is the implementing type.
    Trait { name: String, generics: Vec<String>, methods: Vec<(Function, Range<usize>)>, span: Range<usize> },
    /// `type Name<T> = Type`: another spelling of a type, not a new one.
    Alias { name: String, generics: Vec<String>, ty: Type },
    /// `type Name<T>:` followed by variants: a value is exactly one of them.
    Sum {
        name: String,
        generics: Vec<String>,
        /// Each variant carries one value or nothing.
        variants: Vec<(String, Option<Type>)>,
        /// Where its name is written (0..0 for a type the compiler makes).
        span: Range<usize>,
        /// Index of the source file it was written in.
        module: u32,
    },
    Record {
        name: String,
        generics: Vec<String>,
        fields: Vec<(String, Type)>,
        invariants: Vec<Invariant>,
        /// `invariant <expression over the fields>`: what every value of a sealed type satisfies.
        conditions: Vec<Expr>,
        /// Only the defining file may construct the type, write its fields or take its owner.
        sealed: bool,
        module: u32,
        /// Where its name is written (0..0 for a type the compiler makes).
        span: Range<usize>,
    },
}

/// Methods a derive recipe wrote for a type: they belong to the recipe's file (its names are
/// the recipe's), and the words the derive put in carry the derive site's span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    /// Where the recipe is (header to the end of its body: every token it wrote carries a span
    /// in it, the line it was written from) and its file.
    pub recipe: std::ops::Range<usize>,
    pub recipe_module: u32,
    /// The derive site (the span the derive's words carry), its file, and the type's name.
    pub site: std::ops::Range<usize>,
    pub module: u32,
    pub target: String,
    /// The `for:` type as the derive wrote it, its names as the program's types are named
    /// (`Name@file` where shared): the type a recipe's `For` stands for.
    pub for_type: Option<Type>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub(crate) text_literals: Vec<String>,
    /// Resolved function keys of marked declarations and their explicitly selected providers.
    pub test_cases: std::collections::BTreeMap<String, Option<(u32, String)>>,
    /// Checked reporter declaration whose first parameter must be handled by test members.
    pub test_reporter: Option<String>,
    pub test_members: std::collections::BTreeSet<u32>,
    pub test_edges: std::collections::BTreeSet<(u32, u32)>,
    pub test_adapter: Option<u32>,
    pub coverage_modules: std::collections::BTreeSet<u32>,
    pub declarations: Vec<Declaration>,
    /// Shared projection methods a predicate mentions, including unused definitions.
    pub proof_views: std::collections::BTreeMap<u32, Range<usize>>,
    /// Each derive's expansion (`Derived`).
    pub derived: Vec<Derived>,
    /// Which types implement which traits, once impl blocks have been lifted.
    pub implements: Vec<Implements>,
    /// Types and traits declared without `pub`, with the file of each.
    pub private: std::collections::BTreeMap<String, u32>,
    /// What each file imports: (importing file, the name it qualifies by, imported file). The
    /// name is empty for a file that cannot be qualified (its stem is not a plain name).
    pub imports: Vec<(u32, String, u32)>,
    /// The file that declares each type, trait and capability, `pub` or not.
    pub type_homes: std::collections::BTreeMap<String, (u32, usize)>,
    /// Where each bundled file starts.
    pub starts: Vec<usize>,
    /// Per file: a number the files of one package share, and whether it is the standard library.
    pub packages: Vec<(u32, bool)>,
    /// Declarations the compiler itself writes for every file (`Text`'s methods, the call traits).
    pub language: std::collections::BTreeSet<String>,
    /// Each name written where only a type or trait can be (annotations, parameters, results,
    /// bounds, `impl Trait for Type`, type arguments), with where: each must be in its file's reach.
    pub type_uses: Vec<(String, std::ops::Range<usize>)>,
    /// `capability X from U`: each such X, which must hold authority.
    pub capabilities: Vec<String>,
    /// `grantable capability` names: policy data the run grants to `main` (parser/grants.rs).
    pub grantable: Vec<String>,
    /// The records `gen fn`s became, with where each was declared: checked to hold no
    /// authority (parser/generator.rs).
    pub generators: Vec<(String, std::ops::Range<usize>)>,
    /// `gen fn`s as written, by their function's name (`Owner.method` for a method), that a `for`
    /// over a call of one may run in place of stepping a record (generator.rs `inlinable`).
    pub inline_generators: std::collections::BTreeMap<String, Vec<Function>>,
    /// The library types the key law trusted (wasm/key_law.rs), with the file declaring each:
    /// a build refuses one not declared in a standard file (wasm/files.rs).
    pub key_roots: Vec<(String, u32)>,
}

/// `impl Trait for Type`, after its methods became functions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Implements {
    pub trait_name: String,
    /// The trait's type arguments, for a trait that has type parameters.
    pub arguments: Vec<Type>,
    pub target: String,
    /// The traits each of the type's own parameters must implement for this to hold.
    pub bounds: Vec<Vec<String>>,
    /// The file the impl is written in, and where.
    pub module: u32,
    pub at: usize,
    /// Its own type parameters (`impl<T> Walk<T> for Vec<T>`), none for most: its trait arguments
    /// may name them, so they are not types the checker can read on their own.
    pub parameters: Vec<String>,
}

impl Expr {
    /// Every expression in this one, outermost first; `visit` returning true stops the descent
    /// below the one it was given.
    pub fn visit_mut(&mut self, visit: &mut dyn FnMut(&mut Expr) -> bool) {
        if visit(self) { return; }
        match &mut self.kind {
            Expression::Forall { lower, upper, body, .. } => { lower.visit_mut(visit); upper.visit_mut(visit); body.visit_mut(visit); }
            Expression::SpecCase { value, arms, .. } => { value.visit_mut(visit); for (_, _, e) in arms { e.visit_mut(visit); } }
            Expression::Array(items) => for item in items { item.visit_mut(visit); },
            Expression::Repeat(inner, _) | Expression::Unary(_, inner) | Expression::Field(inner, _) | Expression::Try(inner) | Expression::Labelled(_, inner) | Expression::Matches(inner, _) => inner.visit_mut(visit),
            Expression::Binary(_, a, b) | Expression::Index(a, b) => { a.visit_mut(visit); b.visit_mut(visit); }
            Expression::Call(callee, arguments) => { callee.visit_mut(visit); for a in arguments { a.visit_mut(visit); } }
            Expression::Conditional(c, a, b) => { c.visit_mut(visit); a.visit_mut(visit); b.visit_mut(visit); }
            Expression::Record(fields) => for (_, value) in fields { value.visit_mut(visit); },
            Expression::Inject(_, Some(inner)) => inner.visit_mut(visit),
            _ => {}
        }
    }
}

impl Statement {
    /// `Expr::visit_mut` over every expression of this statement, at any depth.
    pub fn visit_exprs_mut(&mut self, visit: &mut dyn FnMut(&mut Expr) -> bool) {
        let all = |statements: &mut Vec<Statement>, visit: &mut dyn FnMut(&mut Expr) -> bool| for s in statements { s.visit_exprs_mut(visit); };
        match self {
            Statement::Expr(e) | Statement::Assert(e) | Statement::Return(Some(e)) | Statement::Yield { value: e, .. } => e.visit_mut(visit),
            Statement::Bind { value, .. } | Statement::Open { value, .. } | Statement::OpenFields { value, .. } => value.visit_mut(visit),
            Statement::Assign { target, value, .. } => { target.visit_mut(visit); value.visit_mut(visit); }
            Statement::Guard { value, failure, .. } => { value.visit_mut(visit); all(&mut failure.body, visit); }
            Statement::If { condition, yes, no } => { condition.visit_mut(visit); all(yes, visit); all(no, visit); }
            Statement::While { condition, body, .. } => { condition.visit_mut(visit); all(body, visit); }
            Statement::For { start, end, body, .. } => { start.visit_mut(visit); if let Some(end) = end { end.visit_mut(visit); } all(body, visit); }
            Statement::Match { value, arms } => { value.visit_mut(visit); for arm in arms { all(&mut arm.body, visit); } }
            Statement::Using { value, body, .. } => { value.visit_mut(visit); all(body, visit); }
            Statement::Block(body) => all(body, visit),
            Statement::Choose { inner, .. } => inner.visit_exprs_mut(visit),
            _ => {}
        }
    }
}

/// Visit executable expressions in lexical order, carrying the local names introduced
/// by the containing blocks. Used before lowering when resolving labelled calls.
pub(crate) fn visit_scoped(body: &mut [Statement], names: &mut Vec<String>,
    visit: &mut impl FnMut(&mut Expr, &[String]) -> bool) {
    fn arm(arm: &mut MatchArm, names: &mut Vec<String>, visit: &mut impl FnMut(&mut Expr, &[String]) -> bool) {
        let start=names.len();
        names.extend(arm.binding.iter().chain(&arm.open).cloned());
        let mut nested=arm.nested.as_deref();
        while let Some(pattern)=nested {
            names.extend(pattern.binding.iter().chain(&pattern.open).cloned());
            nested=pattern.nested.as_deref();
        }
        visit_scoped(&mut arm.body,names,visit);
        names.truncate(start);
    }
    let outer=names.len();
    for statement in body {
        let mut value=|e: &mut Expr|e.visit_mut(&mut |e| visit(e,names));
        match statement {
            Statement::Bind { name,value:e,.. } => { value(e); names.push(name.clone()); }
            Statement::Open { names:bound,value:e,.. } => { value(e); names.extend(bound.iter().cloned()); }
            Statement::OpenFields { fields,value:e,.. } => { value(e); names.extend(fields.iter().map(|(_,n)|n.clone())); }
            Statement::Guard { value:e,binding,failure,.. } => { value(e); arm(failure,names,visit); names.push(binding.clone()); }
            Statement::Using { value:e,name,body,.. } => { value(e); names.push(name.clone()); visit_scoped(body,names,visit); names.pop(); }
            Statement::For { index,binding,start,end,body,.. } => {
                value(start);
                if let Some(end)=end { value(end); }
                let n=names.len();
                names.extend(index.iter().cloned());
                names.push(binding.clone());
                visit_scoped(body,names,visit);
                names.truncate(n);
            }
            Statement::While { condition,body,.. } => { value(condition); visit_scoped(body,names,visit); }
            Statement::If { condition,yes,no } => { value(condition); visit_scoped(yes,names,visit); visit_scoped(no,names,visit); }
            Statement::Match { value:e,arms } => { value(e); for a in arms {arm(a,names,visit);} }
            Statement::Block(inner) => visit_scoped(inner,names,visit),
            Statement::Choose { name,inner,.. } => {visit_scoped(std::slice::from_mut(inner),names,visit);names.push(name.clone());}
            stmt => stmt.visit_exprs_mut(&mut |e|visit(e,names)),
        }
    }
    names.truncate(outer);
}
