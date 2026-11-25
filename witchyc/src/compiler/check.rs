mod calls;
mod bodies;
use bodies::checked_program;
mod declarations;
mod diagnostics;
use declarations::declarations;
mod resolution;
mod locals;
mod destructuring;
mod arguments;
mod loans;
mod traits;
mod loan_flow;
mod inference;
mod contracts;
mod invariants;
mod operators;
mod rewrites;
mod methods;
mod expressions;
mod places;
mod patterns;
mod statements;

use super::logical_body::*;
use super::{logical_place::{Place, Projection}, flow::Flow};
use super::semantic_types::{Kind, Types};
use super::scalar::Scalar;
use crate::{ast as a, diagnostic::Diagnostic};
use std::collections::{BTreeMap, BTreeSet};
/// Marks a carried or held loan as unique (`&mut`); the rest of the number is the lent root.
const UNIQUE: u32 = 1 << 31;
/// The path step of a loan on a view's own storage (`&v`), as opposed to what it carries.
const VIEW_ITSELF: u32 = u32::MAX;
/// A field path naming what a reference parameter's references point at, outside this frame:
/// a reference read through the parameter depends on it, while the parameter's own fields stay
/// free to use.
const THROUGH: u32 = u32::MAX - 1;
/// Loan ids from here up stand for a field path of a local (`p.left`): two loans conflict only
/// when they are of one local and one path begins the other (`Checker::paths`).
const FIELD_LOAN: u32 = 1 << 30;

type Result<T> = std::result::Result<T, Diagnostic>;
fn error(message: impl Into<std::borrow::Cow<'static, str>>) -> Diagnostic {
    Diagnostic::new("W0401", 0..0, message)
}
/// Where a statement is written: its first expression, when it has one of its own.
fn leading(stmt: &a::Statement) -> Option<std::ops::Range<usize>> {
    use a::Statement as S;
    let expr = match stmt {
        S::Assert(e) | S::Expr(e) | S::Return(Some(e)) => e,
        S::Bind { value, .. } | S::Open { value, .. } | S::OpenFields { value, .. } | S::Guard { value, .. }
        | S::Using { value, .. } | S::Match { value, .. } | S::Yield { value, .. } => value,
        S::Assign { target, .. } => target,
        S::While { condition, .. } | S::If { condition, .. } => condition,
        S::For { start, .. } => start,
        S::Dispose { call, .. } => call,
        S::Choose { inner, .. } => return leading(inner),
        _ => return None,
    };
    (expr.span != (0..0)).then(|| expr.span.clone())
}
#[derive(Clone)]
pub(super) struct Signature {
    definition: FunctionId,
    pub(super) generics: Vec<String>,
    any: Vec<bool>,
    /// Per type parameter: the traits it must implement.
    bounds: Vec<Vec<String>>,
    contracted: bool,
    /// It has a `requires`: a caller must show it, so it cannot be a task's entry.
    requires: bool,
    pub(super) params: Vec<Ty>,
    pub(super) result: Ty,
    /// The file it was written in, and whether other files may call it.
    pub(super) module: u32,
    pub(super) public: bool,
    /// The file it is declared in for reach: its own, or for a method a derive wrote, the
    /// deriving file (the recipe's file wrote its code; the type's file asked for it). And where.
    pub(super) home: u32,
    pub(super) at: usize,
    /// The parameters' names, in order: what labels in a call name.
    pub(super) names: Vec<String>,
    /// Per parameter, the literal a call that leaves it out gives it.
    defaults: Vec<Option<a::Expr>>,
    /// A view receiver's fields, in order, with whether each is lent mutably
    /// (`self: Self { &mut count }`, docs/wasm-references.md).
    view: Vec<(String, bool)>,
    /// Per variant of the result: the parameters what it holds may borrow from (`borrows Ok
    /// from left`), or None for any of them. Empty when the function says nothing.
    borrows: Vec<Option<Vec<usize>>>,
    /// `pure fn`: given no capability; a pure trait method is read by contracts as a value.
    pure: bool,
    /// What its source says, read structurally, for `for i in a..v.len(): ... v[i]`
    /// (`counted_container`).
    pub(super) shape: Shape,
}
/// Facts of a function as written, by structure: its body is one `return` of a field path of
/// its parameter (`projection`), or of an element of a slice a method of its first parameter
/// returns, at its second (`slice_index`: that method's name); an `ensures` says its result is
/// a slice as long as a path of its parameter (`result_length`). A path is field names and
/// `m()`, a method called with no argument: `["length"]`, `["items", "len()"]`, `["len()"]`. None of these trusts the function: the body is
/// what runs, and an `ensures` is proved where the function is.
#[derive(Clone, Debug, Default)]
pub(super) struct Shape {
    pub(super) projection: Option<Vec<String>>,
    pub(super) slice_index: Option<String>,
    pub(super) result_length: Option<Vec<String>>,
    /// Its body is one `return` of names, fields, `*`, `&`, literals and `memory.` operations
    /// on them: no loop, no call of a function, no arithmetic, no index.
    pub(super) straight: bool,
    /// Its body is one `return` of a method called with no argument on a field path of its
    /// parameter (`return (*bag).items.as_slice()`): the fields and the method.
    pub(super) forwards: Option<(Vec<String>, String)>,
}
impl Shape {
    pub(super) fn of(f: &a::Function) -> Self {
        let parameter = |k: usize| f.parameters.get(k).map(|(n, _)| n.clone());
        // `p.f.g` or `(*p).f.g`, as the field names; a method called with no argument on the way
        // (`p.items.len()`) is a step `len()`, read as what it returns.
        let path = |e: &a::Expr, of: &str| -> Option<Vec<String>> {
            let mut steps = Vec::new();
            let mut at = e;
            loop {
                match &at.kind {
                    a::Expression::Field(inner, name) => { steps.push(name.clone()); at = inner; }
                    a::Expression::Call(callee, arguments) if arguments.is_empty() && let a::Expression::Field(inner, method) = &callee.kind => { steps.push(format!("{method}()")); at = inner; }
                    _ => break,
                }
            }
            let root = match &at.kind { a::Expression::Unary("*", inner) => inner, _ => at };
            if !matches!(&root.kind, a::Expression::Name(n) if n == of) || steps.is_empty() { return None; }
            steps.reverse();
            Some(steps)
        };
        // A `proof:` block runs nothing: a body is its other statements.
        let runs: Vec<&a::Statement> = f.body.iter().filter(|statement| !matches!(statement, a::Statement::Proof(_))).collect();
        let returned = match runs.as_slice() { [a::Statement::Return(Some(e))] => Some(e), _ => None };
        let first = parameter(0);
        let projection = match (returned, &first) { (Some(e), Some(p)) if f.parameters.len() == 1 => path(e, p), _ => None };
        let slice_index = match (returned, &first, parameter(1)) {
            (Some(e), Some(p), Some(q)) if f.parameters.len() == 2 => match &e.kind {
                a::Expression::Index(sliced, at) if matches!(&at.kind, a::Expression::Name(n) if *n == q) => match &sliced.kind {
                    a::Expression::Call(callee, arguments) if arguments.is_empty() => match &callee.kind {
                        a::Expression::Field(receiver, method) if matches!(&receiver.kind, a::Expression::Name(n) if n == p)
                            || matches!(&receiver.kind, a::Expression::Unary("*", inner) if matches!(&inner.kind, a::Expression::Name(n) if n == p)) => Some(method.clone()),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };
        let length = |e: &a::Expr| matches!(&e.kind, a::Expression::Call(callee, arguments) if arguments.is_empty()
            && matches!(&callee.kind, a::Expression::Field(receiver, method) if method == "len" && matches!(&receiver.kind, a::Expression::Name(n) if n == "result")));
        let measured = |e: &a::Expr, p: &str| path(e, p);
        let result_length = match &first {
            Some(p) if f.parameters.len() == 1 && f.requires.is_empty() => f.ensures.iter().find_map(|clause| match &clause.kind {
                a::PostconditionKind::Predicate(e) => match &e.kind {
                    a::Expression::Binary("==", left, right) if length(left) => measured(right, p),
                    a::Expression::Binary("==", left, right) if length(right) => measured(left, p),
                    _ => None,
                },
                _ => None,
            }),
            _ => None,
        };
        fn plain(e: &a::Expr) -> bool {
            match &e.kind {
                a::Expression::Name(_) | a::Expression::Integer(_) | a::Expression::Bool(_) | a::Expression::Unit => true,
                a::Expression::Field(inner, _) | a::Expression::Unary("*" | "&", inner) => plain(inner),
                a::Expression::Call(callee, arguments) => matches!(&callee.kind, a::Expression::Field(namespace, _) if matches!(&namespace.kind, a::Expression::Name(n) if n == "memory"))
                    && arguments.iter().all(plain),
                _ => false,
            }
        }
        let straight = returned.is_some_and(plain);
        let forwards = match (returned, &first) {
            (Some(e), Some(p)) if f.parameters.len() == 1 => path(e, p).and_then(|mut steps| {
                let method = steps.pop()?.strip_suffix("()")?.to_string();
                (!steps.is_empty() && steps.iter().all(|step| !step.ends_with("()"))).then_some((steps, method))
            }),
            _ => None,
        };
        Shape { projection, slice_index, result_length, straight, forwards }
    }
}
/// What each file sees of the others. A file sees its own declarations, what the files it
/// imports itself offer with `pub`, and what the compiler declares for every file. Nothing is
/// in reach because of how it is named, or because some other file imports it.
///
/// A method or an impl is found by a value's type, not by a name in scope, so it has one more
/// way in: declared in the file that declares the type (or the trait), when that file is of the
/// using file's own package or of the standard library. Another package's file is in reach only
/// by importing it, which names its first file: its other files are its own.
#[derive(Default)]
pub(super) struct Reach {
    /// Per file, the files it imports itself, one bit each (a bundle holds at most 64 files; a
    /// file past that is in no one's reach).
    direct: Vec<u64>,
    /// The file declaring each type, trait and capability, and where.
    homes: BTreeMap<String, (u32, usize)>,
    /// Per file: its package, and whether that is the standard library.
    packages: Vec<(u32, bool)>,
    /// What the compiler declares for every file.
    language: BTreeSet<String>,
    /// (trait, type) -> the file of the impl; a blanket impl is under `#`.
    impls: BTreeMap<(String, String), (u32, usize)>,
}
impl Reach {
    fn of(program: &a::Program) -> Reach {
        let files = program.packages.len().max(program.imports.iter().map(|(from, _, to)| (*from).max(*to) as usize + 1).max().unwrap_or(0));
        let mut direct = vec![0u64; files];
        for (from, to) in program.imports.iter().map(|(from, _, to)| (*from, *to)).chain(program.test_edges.iter().copied()) {
            if (from as usize) < files && to < 64 { direct[from as usize] |= 1 << to; }
        }
        let home = |start: usize, module: u32, target: &str| program.derived.iter().find(|d| d.recipe.contains(&start) && d.recipe_module == module && d.target == target).map_or(module, |d| d.module);
        let impls = program.implements.iter().map(|i| ((i.trait_name.clone(), if i.target.starts_with('#') { "#".to_string() } else { i.target.clone() }), (home(i.at, i.module, &i.target), i.at))).collect();
        Reach { direct, homes: program.type_homes.clone(), packages: program.packages.clone(), language: program.language.clone(), impls }
    }
    /// Whether `file` declares or itself imports `home`.
    fn sees(&self, file: u32, home: u32) -> bool {
        file == home || (home < 64 && self.direct.get(file as usize).is_some_and(|mask| mask >> home & 1 == 1))
    }
    /// Whether `file` is one of the standard library's.
    fn standard(&self, file: u32) -> bool {
        self.packages.get(file as usize).is_some_and(|p| p.1)
    }
    /// Whether `home` is a file of `file`'s own package, or of the standard library.
    fn near(&self, file: u32, home: u32) -> bool {
        // A program read with no bundle (one text, no files) is one package.
        if self.packages.is_empty() { return true; }
        match (self.packages.get(file as usize), self.packages.get(home as usize)) { (Some(a), Some(b)) => b.1 || a.0 == b.0, _ => false }
    }
}
/// How to bring a standard library file's declaration into reach: its module's name.
fn standard_import(reach: &Reach, home: u32, module: Option<String>) -> String {
    match module.filter(|_| reach.standard(home)) { Some(module) => format!(" (`import {module}`)"), None => String::new() }
}
/// (trait, type) -> the traits each of the type's own parameters must implement.
type Implemented = BTreeMap<(String, String), Vec<Vec<String>>>;
/// (trait, type) -> the trait's type arguments in that impl (`impl Equivalent<String> for Text`).
type TraitArguments = BTreeMap<(String, String), Vec<Ty>>;
/// `impl<T: Bounds> Trait<Args> for T`: the trait, its arguments as written, the parameter and its bounds.
type Blankets = Vec<(String, Vec<a::Type>, String, Vec<String>)>;
type Local = LogicalLocal;
struct Checker<'a> {
    test_edges: &'a BTreeSet<(u32, u32)>,
    assertion_result: Option<Ty>,
    /// Most recently visited source expression; locates errors raised without a span.
    at: std::ops::Range<usize>,
    /// Where each local name was last bound (a byte offset): a refusal of the same name bound
    /// again says where the first one is.
    bound_at: BTreeMap<String, usize>,
    /// Where each owner slot was last moved (a byte offset): a use after it says where.
    moved_at: BTreeMap<u32, usize>,
    signatures: &'a BTreeMap<String, Signature>,
    /// For each name, the keys of the functions of that name declared in a file each
    /// (`name@file`), in key order: what `others` gives, without a scan of every signature.
    others: &'a BTreeMap<String, Vec<String>>,
    /// `gen fn`s as written that a `for` may run in place (inline_generator.rs).
    inline_generators: &'a BTreeMap<String, Vec<a::Function>>,
    /// The generators being run in place around the code being checked: never again inside.
    inlining: Vec<String>,
    definition: FunctionId,
    /// The source file this function was written in.
    module: u32,
    /// A method a derive recipe wrote: authority follows each word's origin (`may_reach`).
    derived: Option<a::Derived>,
    /// In a derived method from another file: the named types `For` stands for or reaches
    /// (through references, buffers, slices, type arguments, fields and variants), none of which
    /// the recipe's file's authority covers.
    for_reach: BTreeSet<String>,
    /// And the one it is, past references: of the deriving file's types, only it (and the target)
    /// is the recipe's to reach (review round 3: not a type argument of it).
    for_outer: Option<String>,
    /// Types and traits declared without `pub`, with their files.
    private: &'a BTreeMap<String, u32>,
    /// What each file imports: (importing file, the name it qualifies by, imported file).
    imports: &'a [(u32, String, u32)],
    /// What a file may name and call: see `Reach`.
    reach: &'a Reach,
    /// Set while an operator's call is written (`a + b` is `Add.add(a, b)`): the trait it goes
    /// through is the operator's, not a name this file wrote, and is read once, at that call.
    /// The method it reaches is still found by the operands' type (`finds`).
    operator: bool,
    generics: Vec<String>,
    calls: Vec<CallEdge>,
    /// Every call with where it is written, for the effect check (effects.rs).
    sites: Vec<(CallEdge, (usize, usize))>,
    contract_sites: u32,
    /// Where each call with a contract is written, by its site (`Function::call_spans`).
    call_spans: Vec<(usize, usize)>,
    /// The operand types `method_call` just read for a `+`, `-` or `*` it
    /// left alone, with the expression they belong to. `infer_hint` takes them straight after
    /// asking, instead of reading both operands again: read twice at every level, a chain of
    /// n operators took 2^n steps (`1 + 1 + ...` 24 times, seconds; 40 times, hours).
    operands: Option<(*const a::Expr, Option<Ty>, Option<Ty>)>,
    /// Steps of type inference spent on this function (`infer_hint`), against `HINT_WORK`.
    hint_work: u32,
    /// What `infer_hint` answered in the inference now running (`Hinted`), by span.
    hint_memo: BTreeMap<(usize, usize), Vec<Hinted>>,
    /// How many `infer_hint` and `method_call` calls are open: at none, the next one starts a
    /// new inference and forgets the answers of the last.
    hinting: u32,
    /// The step counter of the `infer_hint` call now open (its address), and the deepest
    /// `depth` reached under it on that counter.
    hint_chain: *const usize,
    hint_reach: usize,
    needs_contract: &'a std::cell::Cell<bool>,
    /// `main`: it has no contract, so nothing in it can be given a proof.
    entry: bool,
    nopanic_body: bool,
    contracted: bool,
    /// The function says `-> T suspends`: it may pause its task (`memory.pause`).
    suspends: bool,
    proof_requests: usize,
    memory_proof: bool,
    /// The value of a `return` is being checked.
    returning: bool,
    /// The contract is over numbers, Bools and fields read through methods: the body may be anything.
    scalar_proof: bool,
    /// The parameters that hold `&` references, by local (`opaque_view`).
    view_parameters: BTreeSet<u32>,
    /// The argument about to be checked is lent to a call whose borrow ends when it returns
    /// (`ends_at_return`). `argument` takes the flag, so nothing inside the argument inherits it.
    brief_lend: bool,
    memory_element: Option<Ty>,
    /// Number of parameters, when the function body is checked by the record model.
    record_proof: Option<(u32, Ty)>,
    layouts: &'a mut Types,
    scope: BTreeMap<String, Local>,
    /// The source name each named local was bound with (`Function::local_names`).
    local_names: BTreeMap<u32, String>,
    locals: Vec<LogicalLocal>,
    flow: Flow,
    /// The loops being checked, innermost last: ownership before the condition and after it,
    /// and the locals that were in scope when the loop began.
    loops: Vec<(Flow, Flow, Vec<u32>)>,
    /// The `match` and `if` statements being checked whose value a `let` binds, innermost last:
    /// the name, whether it is a `var`, its written type, and its local once a path has given a value.
    yields: Vec<(String, bool, Option<Ty>, Option<Local>)>,
    /// Beside each of `yields`: the loans the values given so far depend on. The name keeps them all.
    chosen: Vec<Vec<u32>>,
    /// For the call a `match` is about to take apart: a variant of its result and the type that variant holds.
    held: Option<(String, Ty)>,
    checks: u32,
    element_limits: Vec<Ty>,
    /// Counting-`for` counters (`Function::steps`).
    steps: BTreeSet<u32>,
    /// The hidden locals of owner assignments over a field (`Function::overwrites`).
    overwrites: BTreeSet<u32>,
    /// `while` counters whose every `+= 1` the loop condition bounds (guarded.rs).
    guarded: BTreeSet<String>,
    /// While a call's arguments are read for hints, a call among them leaves a bare number
    /// open instead of making it a U64, so a later argument can settle it (`choose(id(1), byte)`).
    open_numbers: bool,
    result: Ty,
    /// The result type as written, for names a rewrite binds to a value about to be returned.
    written_result: a::Type,
    nodes: usize,
    nesting: usize,
    addressed: BTreeSet<u32>,
    /// For each lent root: how many shared loans and how many unique ones. A new unique loan
    /// needs both to be zero; a second unique count arises only when a reference derived from
    /// a `&mut` local keeps what that local already holds.
    loans: BTreeMap<u32, (usize, usize)>,
    /// The local and field path each field loan id stands for (`FIELD_LOAN + index`).
    paths: Vec<(u32, Vec<u32>)>,
    /// Number of parameters: a returned reference may borrow only from these.
    parameters: u32,
    /// Loans the value being checked still depends on, one entry per loan; `UNIQUE` marks a `&mut` one.
    carried: Vec<u32>,
    /// After a call whose function says what each variant borrows: the function, the loans its
    /// result carries, and those of each variant. A `match` on that call reads it.
    split: Option<(std::ops::Range<usize>, FunctionId, Vec<u32>, Vec<Vec<u32>>)>,
    /// A `let` bound to such a call, before the statement ends: the local, the loans, each variant's.
    pending_parts: Option<(u32, Vec<u32>, Vec<Vec<u32>>)>,
    /// Where the value a `return` gives is written: a call there with `borrows` clauses is
    /// forwarded variant by variant.
    returned_at: std::ops::Range<usize>,
    /// The value inside `return Ok(value)`: a type with an invariant may be built there when the
    /// result is `Result<T, E>` (the synthesised `ensures` covers the `Ok` arm).
    returned_payload_at: Option<std::ops::Range<usize>>,
    /// Locals bound by `let` to such a call: the loans they hold and each variant's share. A later
    /// `match` on the whole local gives each arm its variant's (reference locals are never assigned again).
    local_parts: BTreeMap<u32, (Vec<u32>, Vec<Vec<u32>>)>,
    /// This function's own `borrows` clauses, per variant of its result.
    own_borrows: Vec<Option<Vec<usize>>>,
    /// Its parameters' names, for refusals.
    parameter_names: Vec<String>,
    /// The loans each reference-holding local keeps until its last use.
    holds: BTreeMap<u32, Vec<u32>>,
    /// Inside the failure arm a `?` was rewritten into: a value left holding an owner at its
    /// `return` is named with the `?` that leaves, and `using` is suggested.
    try_exit: Option<std::ops::Range<usize>>,
    /// Match binders that take over the loans of the value matched, for the arm's block to release.
    adopt: Vec<(String, u32)>,
    /// A `let` just checked, whose value's loans become the local's.
    bound: Option<(String, u32)>,
    /// The names an opening just bound to parts that hold a reference (`Two(a, b)` of two
    /// Spans): each holds the opened value's loans until its own last use.
    opened_views: Vec<(String, u32)>,
    /// The traits each of this function's type parameters is declared to implement.
    bounds: Vec<Vec<String>>,
    implemented: &'a Implemented,
    trait_arguments: &'a TraitArguments,
    /// The opening being checked (`brand value as name:`): the value's own call, and the brand new
    /// there. `fresh_for` offers the brand to that call alone; `opened` says it took it.
    opening: Option<(*const a::Expr, Ty)>,
    fresh_for: Option<Ty>,
    opened: bool,
    blankets: &'a Blankets,
}

/// `borrows Variant from a, b`: per variant of the result, the parameters the references it
/// holds may borrow from. Each names a variant that holds a reference, at most once, and
/// parameters that can lend (a reference, or a value that carries one).
fn variant_sources(f: &a::Function, params: &[Ty], result: Ty, layouts: &Types) -> Result<Vec<Option<Vec<usize>>>> {
    if f.borrows.is_empty() { return Ok(Vec::new()); }
    let refuse = |span: &std::ops::Range<usize>, message: String| Diagnostic::new("W0401", span.clone(), message);
    let Some(sum) = layouts.sum(result) else { return Err(refuse(&f.borrows[0].span, "`borrows` names a variant of the result, which is not a sum type".into())); };
    // A variant that holds no reference borrows from nothing. One that may hold a type parameter
    // may hold a reference at a call, which nothing here can see into: it may borrow from any
    // parameter, and no clause can say otherwise (review of `borrows`: `Err(T)` counted as
    // borrowing nothing, and a caller closed what the `T` it passed pointed into).
    let generic = |arm: Ty| !layouts.parameters_of(arm).is_empty();
    let mut sources = sum.arms.iter().map(|arm| if layouts.lends(*arm) || generic(*arm) { None } else { Some(Vec::new()) }).collect::<Vec<_>>();
    for clause in &f.borrows {
        let Some(tag) = sum.names.iter().position(|name| *name == clause.variant) else { return Err(refuse(&clause.span, format!("the result has no variant `{}`", clause.variant))); };
        if generic(sum.arms[tag]) { return Err(refuse(&clause.span, format!("`{}` holds a value of a type parameter, which may be a reference to anything it is given: a clause cannot narrow it", clause.variant))); }
        if !layouts.lends(sum.arms[tag]) { return Err(refuse(&clause.span, format!("`{}` holds no reference: it borrows from nothing", clause.variant))); }
        if f.borrows.iter().filter(|other| other.variant == clause.variant).count() > 1 { return Err(refuse(&clause.span, format!("`borrows {}` is said twice", clause.variant))); }
        let mut from = Vec::new();
        for name in &clause.from {
            let Some(index) = f.parameters.iter().position(|(parameter, _)| parameter == name) else { return Err(refuse(&clause.span, format!("`{name}` is not a parameter"))); };
            if !matches!(params[index], Ty::Borrow { .. }) && !layouts.lends(params[index]) { return Err(refuse(&clause.span, format!("`{name}` lends nothing: it is not a reference and holds none"))); }
            from.push(index);
        }
        sources[tag] = Some(from);
    }
    Ok(sources)
}
/// Whether two lists hold the same loans, in any order.
fn same_loans(a: &[u32], b: &[u32]) -> bool {
    let (mut a, mut b) = (a.to_vec(), b.to_vec());
    a.sort();
    b.sort();
    a == b
}
/// Whether the result may borrow from parameter `index`: always, unless `borrows` clauses
/// cover every variant that holds a reference and none names it.
fn named_by_borrows(borrows: &[Option<Vec<usize>>], index: usize) -> bool {
    borrows.is_empty() || borrows.iter().any(|from| from.as_ref().is_none_or(|from| from.contains(&index)))
}
thread_local! {
    /// The refusals found in functions after the first one refused, in order (`later_refusals`).
    static LATER: std::cell::RefCell<Vec<Diagnostic>> = const { std::cell::RefCell::new(Vec::new()) };
}
/// What the last `program` refused after its first refusal: one for each function after the first
/// that has one, at most 20. The build prints them after the first.
pub(super) fn later_refusals() -> Vec<Diagnostic> { LATER.with(|later| std::mem::take(&mut *later.borrow_mut())) }
pub(super) fn program(program: &a::Program) -> Result<Program> {
    // A refusal with no place of its own, made while checking one function, is placed at it.
    let mut at = None;
    checked_program(program, &mut at).map_err(|mut e| {
        if e.span == (0..0) && let Some(span) = at { e.span = span; }
        e
    })
}
/// The same declaration pass feeds body checking and clients which reflect a signature.
pub(super) struct CheckedDeclarations {
    pub(super) layouts: Types,
    pub(super) signatures: BTreeMap<String, Signature>,
    main: Option<FunctionId>,
    secret_slots: Vec<(String, u8)>,
}
impl CheckedDeclarations {
    /// Resolve a free declaration in one specific module, including a name shared by several files.
    pub(super) fn declared(&self, module: u32, name: &str) -> Option<(&String, &Signature)> {
        self.signatures.get_key_value(&format!("{name}@{module}")).or_else(||
            self.signatures.get_key_value(name).filter(|(_, s)| s.module == module))
    }
}
pub(super) fn reflect(program: &a::Program) -> Result<CheckedDeclarations> {
    let expanded = prepare(program)?;
    let mut at = None;
    declarations(&expanded, &mut at).map_err(|mut e| {
        if e.span == (0..0) && let Some(span) = at { e.span = span; }
        e
    })
}
fn prepare(program: &a::Program) -> Result<std::borrow::Cow<'_, a::Program>> {
    if program.declarations.len() > 4096 { return Err(error("at most 4096 declarations")); }
    // `memory.` and `provider.` are the compiler's own operations wherever they are written,
    // the library's files included: a declaration of either name would be found first there
    // (a user's `fn memory` once made `Vec` unable to allocate). Refused before anything is
    // expanded or resolved. A local or a parameter of the name is its own function's alone.
    for declaration in &program.declarations {
        let (name, span) = match declaration {
            a::Declaration::Function(f) => (&f.name, f.span.clone()),
            a::Declaration::Record { name, span, .. } | a::Declaration::Sum { name, span, .. } | a::Declaration::Trait { name, span, .. } => (name, span.clone()),
            a::Declaration::Alias { name, .. } | a::Declaration::Predicate { name, .. } => (name, 0..0),
            a::Declaration::Lemma(lemma) => (&lemma.name, 0..0),
            _ => continue,
        };
        if matches!(name.as_str(), "memory" | "provider") {
            return Err(Diagnostic::new("W0401", span, format!("`{name}` cannot be declared: it names the compiler's own operations (`{name}.…`) in every file of the program; choose another name")));
        }
    }
    super::spec::prepare(program)
}
/// Steps of type inference one function may take: a backstop. The largest function of the
/// fixtures and the library takes 1856. What used to reach this was a chain of operators on a
/// program's own type, read again at every level (about 3 times the steps for each operator
/// more: 16 took 2 s, 20 took 92 s). `infer_hint` now recalls what it answered within one
/// inference, so such a chain takes steps in proportion to its length (20 operators: 0.08 s),
/// and is bounded first by the depth of one inference (41 operators).
const HINT_WORK: u32 = 1 << 21;
/// An answer `infer_hint` gave in the inference now running: for this expression (compared
/// whole: two expressions the checker wrote may share a span) with this expected type, the
/// type, the steps it added to its counter and how far below its own `depth` it went.
struct Hinted { expr: a::Expr, expected: Option<Ty>, result: Option<Ty>, steps: usize, reach: usize }
const HINT_WORK_MESSAGE: &str = "type inference work limit: this function's expressions take too many steps to type; give the parts of a long chain of operators names with `let`";
/// The type parameters of `definition` a value of type `t` may lead to through trait
/// methods: those a bound on `t` names as its trait's type arguments (`B: Leak<A>` names
/// `A`, whose `leak` can hand out an `&A`), and, since one method's result can be the next
/// one's receiver, those their bounds name in turn (semantic_types `can_hold`).
fn open_parameter(t: Ty, definition: FunctionId, generics: &[String], bounds: &[Vec<String>]) -> Vec<Ty> {
    let Ty::Param { owner, .. } = t else { return Vec::new() };
    if owner != definition { return Vec::new(); }
    let (mut out, mut pending) = (Vec::new(), vec![t]);
    while let Some(Ty::Param { index, .. }) = pending.pop() {
        for bound in bounds.get(index as usize).into_iter().flatten() {
            let Some((_, arguments)) = bound.split_once('<') else { continue };
            for word in arguments.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                let Some(at) = generics.iter().position(|g| g == word) else { continue };
                let reached = Ty::Param { owner: definition, index: at as _ };
                if reached != t && !out.contains(&reached) { out.push(reached); pending.push(reached); }
            }
        }
    }
    out
}

/// Whether `statement` could change `counter`: assigns it (or through it), lends it uniquely,
/// calls a method on it (unless `methods` is false: a slice's `len` changes nothing), or binds
/// the name, anywhere inside.
fn touches(statement: &a::Statement, counter: &str) -> bool { touches_name(statement, counter, true) }
fn touches_name(statement: &a::Statement, counter: &str, methods: bool) -> bool {
    fn rooted(e: &a::Expr, name: &str) -> bool {
        match &e.kind {
            a::Expression::Name(n) => n == name,
            a::Expression::Field(inner, _) | a::Expression::Index(inner, _) | a::Expression::Unary("*", inner) => rooted(inner, name),
            _ => false,
        }
    }
    let mut found = false;
    let mut walk = statement.clone();
    let mut check = |e: &mut a::Expr| {
        match &e.kind {
            a::Expression::Unary("&mut", inner) if rooted(inner, counter) => found = true,
            a::Expression::Call(callee, _) if methods && matches!(&callee.kind, a::Expression::Field(receiver, _) if rooted(receiver, counter)) => found = true,
            _ => {}
        }
        false
    };
    walk.visit_exprs_mut(&mut check);
    fn writes(statements: &[a::Statement], counter: &str) -> bool {
        statements.iter().any(|s| match s {
            a::Statement::Assign { target, .. } => rooted(target, counter),
            a::Statement::Bind { name, .. } | a::Statement::Using { name, .. } | a::Statement::Choose { name, .. } if name == counter => true,
            a::Statement::Open { names, .. } if names.iter().any(|n| n == counter) => true,
            a::Statement::OpenFields { fields, .. } if fields.iter().any(|(_, n)| n == counter) => true,
            a::Statement::Guard { binding, failure, .. } => binding == counter || writes(&failure.body, counter),
            a::Statement::If { yes, no, .. } => writes(yes, counter) || writes(no, counter),
            a::Statement::While { body, .. } | a::Statement::For { body, .. } | a::Statement::Using { body, .. } | a::Statement::Block(body) => writes(body, counter),
            a::Statement::Match { arms, .. } => arms.iter().any(|arm| writes(&arm.body, counter)),
            a::Statement::Choose { inner, .. } => writes(std::slice::from_ref(&**inner), counter),
            // Anything else this does not know is taken to change it.
            a::Statement::Expr(_) | a::Statement::Return(_) | a::Statement::Break | a::Statement::Continue | a::Statement::Unreachable(_)
            | a::Statement::Assert(_) | a::Statement::Bind { .. } | a::Statement::Open { .. } | a::Statement::OpenFields { .. } => false,
            _ => true,
        })
    }
    found || writes(std::slice::from_ref(statement), counter)
}

/// `let x = *call(...)`: the reference the call gives is bound first, then read, as
/// `let deref#N = call(...)` and `let x = *deref#N`, which is how it is written by hand; the call
/// runs where it did, and its loan ends at that read. Only a whole `let` value: inside a larger
/// expression the call would move ahead of what runs before it.
fn deref_lets(body: &mut Vec<a::Statement>) {
    let mut out = Vec::with_capacity(body.len());
    for mut statement in std::mem::take(body) {
        match &mut statement {
            a::Statement::Using { body, .. } | a::Statement::Block(body) | a::Statement::While { body, .. } | a::Statement::For { body, .. } => deref_lets(body),
            a::Statement::If { yes, no, .. } => { deref_lets(yes); deref_lets(no); }
            a::Statement::Match { arms, .. } => for arm in arms { deref_lets(&mut arm.body); },
            a::Statement::Guard { failure, .. } => deref_lets(&mut failure.body),
            _ => {}
        }
        if let a::Statement::Bind { mutable, name, ty, value, proof } = &statement
            && let a::Expression::Unary("*", inner) = &value.kind && matches!(inner.kind, a::Expression::Call(..)) {
            let hidden = format!("deref#{}", value.span.start);
            out.push(a::Statement::Bind { mutable: false, name: hidden.clone(), ty: None, value: (**inner).clone(), proof: None });
            let read = a::Expr { kind: a::Expression::Unary("*", Box::new(a::Expr { kind: a::Expression::Name(hidden), span: inner.span.clone() })), span: value.span.clone() };
            out.push(a::Statement::Bind { mutable: *mutable, name: name.clone(), ty: ty.clone(), value: read, proof: proof.clone() });
            continue;
        }
        out.push(statement);
    }
    *body = out;
}

/// The last statement of a body that yields the function's result: an expression becomes its
/// `return`, and an `if` with an `else`, a `match`, a `using` or a block passes the rule on to
/// each of its own last statements.
fn tail(mut body: Vec<a::Statement>) -> Vec<a::Statement> {
    match body.pop() {
        Some(a::Statement::Expr(value)) => body.push(a::Statement::Return(Some(value))),
        Some(a::Statement::If { condition, yes, no }) if !no.is_empty() => body.push(a::Statement::If { condition, yes: tail(yes), no: tail(no) }),
        Some(a::Statement::Match { value, arms }) => body.push(a::Statement::Match { value, arms: arms.into_iter().map(|mut arm| { arm.body = tail(arm.body); arm }).collect() }),
        Some(a::Statement::Using { mutable, name, ty, value, body: inner }) => body.push(a::Statement::Using { mutable, name, ty, value, body: tail(inner) }),
        Some(a::Statement::Block(inner)) => body.push(a::Statement::Block(tail(inner))),
        Some(other) => body.push(other),
        None => {}
    }
    body
}

/// Whether a written type names the type parameter `name` anywhere in it.
fn type_mentions(t: &crate::ast::Type, name: &str) -> bool {
    use crate::ast::Type as T;
    match t {
        T::Unit => false,
        T::Named(n, args) => n == name || args.iter().any(|a| type_mentions(a, name)),
        T::Borrow { inner, .. } | T::Slice(inner) => type_mentions(inner, name),
        T::Array { element, .. } => type_mentions(element, name),
        T::Record(fields) => fields.iter().any(|(_, t)| type_mentions(t, name)),
        T::Union(tags) => tags.iter().any(|(_, t)| t.as_ref().is_some_and(|t| type_mentions(t, name))),
    }
}

/// Whether every path through `body` ends by leaving the function: `return`, or a trap, and
/// no `break` or `continue` in it reaches a loop around it (review: a branch ending in `return`
/// that could `break` first was taken as leaving, and the loop went on with its loans ended).
fn leaves_function(body: &[a::Statement]) -> bool {
    ends_in_return(body) && !escapes(body)
}
/// Whether a `break` or `continue` in `body`, outside any loop within it, leaves it for a loop
/// around it.
fn escapes(body: &[a::Statement]) -> bool {
    body.iter().any(|statement| match statement {
        a::Statement::Break | a::Statement::Continue => true,
        a::Statement::If { yes, no, .. } => escapes(yes) || escapes(no),
        a::Statement::Match { arms, .. } => arms.iter().any(|arm| escapes(&arm.body)),
        a::Statement::Block(inner) | a::Statement::Using { body: inner, .. } => escapes(inner),
        a::Statement::Guard { failure, .. } => escapes(&failure.body),
        a::Statement::Choose { inner, .. } => escapes(std::slice::from_ref(&**inner)),
        // A loop's own `break` and `continue` stay in it.
        a::Statement::While { .. } | a::Statement::For { .. } => false,
        _ => false,
    })
}
fn ends_in_return(body: &[a::Statement]) -> bool {
    match body.last() {
        Some(a::Statement::Return(_) | a::Statement::Unreachable(_)) => true,
        Some(a::Statement::If { yes, no, .. }) => !no.is_empty() && ends_in_return(yes) && ends_in_return(no),
        Some(a::Statement::Match { arms, .. }) => !arms.is_empty() && arms.iter().all(|arm| ends_in_return(&arm.body)),
        Some(a::Statement::Block(inner)) => ends_in_return(inner),
        _ => false,
    }
}

/// The hidden local an owner assignment over a field exchanges the old value into (`vacant_assign`).
pub(super) const OVERWRITTEN: &str = "overwritten#";
/// The binder of an owner arm of `memory.vacant` (`vacant_discard`): no source can name it.
const VACANT: &str = "vacant#";
