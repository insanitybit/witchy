//! One fresh kernel batch for authored ghost mathematics and checked source VCs.
//! No source text is spliced into Lean identifiers or tactics.
use super::{proof_digest as digest, scalar::Scalar};
use crate::{ast as a, diagnostic::Diagnostic};
use std::{collections::{BTreeMap, BTreeSet}, fs, time::Instant};
use super::proof_advice::{self as advice, Unproved};

#[path = "proof_standalone.rs"]
mod standalone;

type Result<T> = std::result::Result<T, Diagnostic>;
fn error(message: &'static str) -> Diagnostic { Diagnostic::new("W0402", 0..0, message) }
fn io<T>(result: std::io::Result<T>) -> Result<T> { result.map_err(|_| error("fresh scalar proof tool or identity I/O refused")) }
const MODEL: &str = "witchy.scalar.ghost.v1: unsigned parameters are Nat with ScalarLayout upper bounds; + is Nat addition; - is saturating Nat subtraction; logical expressions are Prop; no runtime refinement asserted";
pub(super) const AUDIT: &str = include_str!("proof_audit.lean.in");
// Fixed generated-language imports; keep the real audit controls on this closure.
pub(super) const IMPORTS: &str = concat!(
    "import Lean.Elab.BuiltinCommand\n",
    "import Lean.Elab.BuiltinTerm\n",
    "import Lean.Elab.Declaration\n",
    "import Lean.Elab.ElabRules\n",
    "import Lean.Elab.BuiltinEvalCommand\n",
    "import Lean.Elab.StructInst\n",
    "import Lean.Elab.Tactic.Omega\n",
    "import Lean.Elab.Tactic.Match\n",
    "import Lean.Elab.Tactic.Simp\n",
    "import Lean.Elab.Tactic.Unfold\n",
    "import Lean.Util.CollectAxioms\n",
    "import Init.Omega\n",
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty { Unsigned(Scalar), Bool }
struct Value { ty: Ty, lean: String }
struct Resolver { scope: BTreeMap<String, (Ty, usize)>, nodes: usize }
impl Resolver {
    fn hint(&self, expr: &a::Expr) -> Option<Ty> {
        match &expr.kind { a::Expression::Name(name) => self.scope.get(name).map(|v| v.0), a::Expression::Bool(_) => Some(Ty::Bool), _ => None }
    }
    fn value(&mut self, expr: &a::Expr, hint: Option<Ty>, depth: usize) -> Result<Value> {
        self.nodes += 1;
        if depth > 64 || self.nodes > 1024 { return Err(error("scalar lemma expression limit")); }
        let (ty, lean) = match &expr.kind {
            a::Expression::Integer(value) => {
                let ty = hint.unwrap_or(Ty::Unsigned(Scalar::U64));
                let Ty::Unsigned(scalar) = ty else { return Err(error("integer in boolean lemma expression")); };
                if *value > scalar.layout().maximum() { return Err(error("scalar lemma literal exceeds width")); }
                (ty, format!("({value} : Nat)"))
            }
            a::Expression::Bool(value) => (Ty::Bool, if *value { "True" } else { "False" }.into()),
            a::Expression::Name(name) => {
                let (ty, index) = self.scope.get(name).ok_or_else(|| error("unbound scalar lemma name"))?;
                (*ty, if *ty == Ty::Bool { format!("(p{index} = true)") } else { format!("p{index}") })
            }
            a::Expression::Unary("!", value) => {
                let value = self.value(value, Some(Ty::Bool), depth + 1)?;
                (Ty::Bool, format!("(¬ {})", value.lean))
            }
            a::Expression::Binary(op, left, right) => {
                let logical = matches!(*op, "&&" | "||");
                let input = if logical { Some(Ty::Bool) } else { self.hint(left).or_else(|| self.hint(right)).or(hint.filter(|ty| *ty != Ty::Bool)) };
                let left = self.value(left, input, depth + 1)?;
                let right = self.value(right, Some(left.ty), depth + 1)?;
                if left.ty != right.ty { return Err(error("scalar lemma operand width/type mismatch")); }
                let (result, operator) = match (*op, left.ty) {
                    ("+", Ty::Unsigned(_)) => (left.ty, "+"),
                    ("-", Ty::Unsigned(_)) => (left.ty, "-"),
                    ("==", Ty::Unsigned(_)) => (Ty::Bool, "="),
                    ("!=", Ty::Unsigned(_)) => (Ty::Bool, "≠"),
                    ("<", Ty::Unsigned(_)) => (Ty::Bool, "<"),
                    ("<=", Ty::Unsigned(_)) => (Ty::Bool, "≤"),
                    (">", Ty::Unsigned(_)) => (Ty::Bool, ">"),
                    (">=", Ty::Unsigned(_)) => (Ty::Bool, "≥"),
                    ("==", Ty::Bool) => (Ty::Bool, "↔"),
                    ("!=", Ty::Bool) => (Ty::Bool, "≠"),
                    ("&&", Ty::Bool) => (Ty::Bool, "∧"),
                    ("||", Ty::Bool) => (Ty::Bool, "∨"),
                    _ => return Err(error("unsupported scalar lemma logical operation")),
                };
                (result, format!("({} {operator} {})", left.lean, right.lean))
            }
            _ => return Err(error("unsupported scalar lemma formula; only closed scalar logic is admitted")),
        };
        if hint.is_some_and(|expected| expected != ty) { return Err(error("scalar lemma expression type mismatch")); }
        Ok(Value { ty, lean })
    }
}

#[derive(Clone)]
pub(super) struct SourceIdentity {pub kind:&'static str,pub function:String,pub callee:Option<String>,pub site:Option<u32>,pub storage:bool,pub record:bool,pub memory:bool}
#[derive(Clone)]
pub(super) struct Goal { pub source_name: String, pub statement: String, pub nodes: usize, pub source:Option<SourceIdentity>,pub prover:Option<String> }
fn goals(program: &a::Program) -> Result<Vec<Goal>> {
    let mut names = BTreeSet::new();
    let mut result = Vec::new();
    for declaration in &program.declarations {
        match declaration {
            a::Declaration::Function(f) => { if !names.insert(f.name.clone()) { return Err(error("duplicate selected declaration")); } }
            a::Declaration::Record { name, .. } | a::Declaration::Alias { name, .. } | a::Declaration::Sum { name, .. } => { if !names.insert(name.clone()) { return Err(error("duplicate selected declaration")); } }
            a::Declaration::Lemma(lemma) => {
                if !names.insert(lemma.name.clone()) { return Err(error("duplicate selected lemma/declaration")); }
                if lemma.proof.as_slice() != [a::ProofStep::Auto] { return Err(error("scalar lemmas require exactly one auto proof step")); }
                if lemma.parameters.len() > 32 || lemma.requires.len() > 32 { return Err(error("scalar lemma parameter/premise limit")); }
                let mut resolver = Resolver { scope: BTreeMap::new(), nodes: 0 };
                let mut binders = String::new();
                let mut bounds = Vec::new();
                for (index, (name, ty)) in lemma.parameters.iter().enumerate() {
                    let a::Type::Named(name_ty, args) = ty else { return Err(error("scalar lemma parameter must be unsigned or Bool")); };
                    if !args.is_empty() { return Err(error("generic scalar lemma parameter refused")); }
                    let ty = if name_ty == "Bool" { Ty::Bool } else { Ty::Unsigned(Scalar::named(name_ty).ok_or_else(|| error("unknown scalar lemma parameter type"))?) };
                    if resolver.scope.insert(name.clone(), (ty, index)).is_some() { return Err(error("duplicate scalar lemma parameter")); }
                    match ty {
                        Ty::Bool => binders.push_str(&format!("(p{index} : Bool) ")),
                        Ty::Unsigned(scalar) => {
                            binders.push_str(&format!("(p{index} : Nat) "));
                            bounds.push(format!("p{index} ≤ {}", scalar.layout().maximum()));
                        }
                    }
                }
                for premise in &lemma.requires { bounds.push(resolver.value(premise, Some(Ty::Bool), 0)?.lean); }
                let conclusion = resolver.value(&lemma.ensures, Some(Ty::Bool), 0)?.lean;
                bounds.push(conclusion);
                let body = bounds.join(" → ");
                let statement = if binders.is_empty() { body } else { format!("∀ {binders}, {body}") };
                result.push(Goal { source_name: lemma.name.clone(), statement, nodes: resolver.nodes, source:None,prover:None });
            }
            _ => {}
        }
    }
    Ok(result)
}

pub(super) struct Checked { pub inventory: String, pub fresh: String, pub invocations: usize, pub measurements: super::proof_measure::Measurements }
/// Call only after the entire runtime/type checker has succeeded. Every public
/// compile/inspect route uses this same function; there is no unchecked mode.
pub(super) fn check(program: &a::Program, source: &str, source_goals:Vec<Goal>) -> Result<Checked> {
    check_prepared(program, source, super::proof_body::Prepared::goals_only(source_goals), None, &mut Refusal::default())
}
/// A refused obligation, recorded so the build can say what failed: where (a call's site), and
/// how long the request that refused it ran.
struct Failure { function: String, kind: &'static str, callee: Option<String>, site: Option<u32>, message: String, seconds: Option<u64> }

/// Evidence for the current build's refusal. Standalone explanations recheck
/// their isolated goals against the SDK fingerprint captured before admission.
#[derive(Default)]
pub(super) struct Refusal {
    failures: Vec<Failure>,
    sdk: Option<standalone::Sdk>,
    /// The goals of a request that passed the checker's deadline without naming one, and how
    /// long it ran: the explanation finds which function's proof does not finish.
    stalled: Vec<Goal>,
    seconds: Option<u64>,
}
/// The refusals of a request that ran out of time without naming a goal. (The standalone
/// checker's deadline shares its message with output and process failures, so it is not one.)
const DEADLINES: [&str; 1] = ["native diagnostic deadline"];
/// Goal ordinals whose theorem text contains a Lean error, with the first message.
fn failing(module: &str, output: &str, imported_rows: usize) -> Vec<(usize, String)> {
    let mut ranges = Vec::new();
    let mut start = None;
    for (line, text) in module.lines().enumerate() {
        let row = line + 1;
        if (text.starts_with("def expected_") || text.starts_with("end Witchy"))
            && let Some(start) = start.take() { ranges.push(start..row); }
        if text.starts_with("theorem goal_") { start = Some(row); }
    }
    let mut out: Vec<(usize, String)> = Vec::new();
    for line in output.lines() {
        let Some((location, message)) = line.split_once(": error: ") else { continue };
        let mut fields = location.rsplitn(3, ':');
        let (Some(_column), Some(row)) = (fields.next(), fields.next()) else { continue };
        // The resident helper preloads the import header and reports rows of the remaining text.
        let Some(row) = row.parse::<usize>().ok().and_then(|row| row.checked_add(imported_rows)) else { continue };
        let Some(goal) = ranges.iter().position(|range| range.contains(&row)) else { continue };
        if !out.iter().any(|(seen, _)| *seen == goal) { out.push((goal, message.trim().to_owned())); }
    }
    out
}
fn read_failure(module: &str) -> Vec<(usize, String)> {
    let Some(path) = super::proof_session::take_last_failure() else { return Vec::new() };
    fs::read(path.join("stdout")).map(|bytes| failing(module, &String::from_utf8_lossy(&bytes), IMPORTS.lines().count())).unwrap_or_default()
}
fn failure(goal: &Goal, message: String) -> Failure {
    match &goal.source {
        Some(identity) => Failure { function: identity.function.clone(), kind: identity.kind, callee: identity.callee.clone(), site: identity.site, message, seconds: None },
        None => Failure { function: goal.source_name.clone(), kind: "lemma", callee: None, site: None, message, seconds: None },
    }
}
fn call_refusal(function: &str, callee: &str) -> String {
    format!("proof failed: in function `{function}`, a call to `{callee}` does not establish the callee's `requires`")
}
/// A request Lean refused: each goal it named, with its message, is kept for the explanation and
/// (with a cache) for later builds. One it did not name is not kept: it may pass alone.
fn note_failure(failures: &mut Vec<Failure>, module: &str, goals: &[Goal], cache: Option<&super::proof_cache::Cache>,
    originals: &[Goal], theorem_for: &[usize], start: usize) {
    for (index, message) in read_failure(module) {
        let Some(goal) = goals.get(index) else { continue };
        if let Some(cache) = cache { let _ = cache.store_refusal(goal, &message); }
        for (original, &theorem) in originals.iter().zip(theorem_for) {
            if theorem == start + index { failures.push(failure(original, message.clone())); }
        }
    }
}
/// Recheck isolated goals after admission has already failed.
fn probe(goals: &[Goal], source: &str, sdk: Option<&standalone::Sdk>) -> Option<Vec<usize>> {
    if let Some(sdk) = sdk {
        return sdk.refused_goals(goals);
    }
    let binding = super::ready::Binding::configured().ok()??;
    probe_ready(goals, source, binding)
}

fn probe_ready(goals: &[Goal], source: &str, binding: super::ready::Binding) -> Option<Vec<usize>> {
    let mut failed = Vec::new();
    // Cache and check against one binding. A replacement must fail validation
    // instead of recording results from a different checker under this identity.
    let cache = super::proof_cache::Cache::configured(Some(binding.identity.as_str())).ok()?;
    let mut identity = None;
    for range in batches(goals).ok()? {
        let references = goals[range.clone()].iter().collect::<Vec<_>>();
        let emitted = super::proof_module::emit(super::proof_module::Namespace::Current, &references, None).ok()?;
        let text = emitted.text.clone();
        if let Some(found) = cache.as_ref().and_then(|cache| cache.explained(&text, range.len())) {
            failed.extend(found.into_iter().map(|index| range.start + index));
            continue;
        }
        // Only a fresh request needs the compiler context and source fingerprint.
        let (source_id, context) = identity.get_or_insert_with(|| {
            let source_id = digest::bytes(source.as_bytes())?;
            let compiler_id = digest::file(&std::env::current_exe()?)?;
            let imports_id = digest::bytes(IMPORTS.as_bytes())?;
            let audit_id = digest::bytes(AUDIT.as_bytes())?;
            Ok::<_, std::io::Error>((source_id, format!("{compiler_id}:{imports_id}:{audit_id}")))
        }).as_ref().ok()?;
        let mut session = super::proof_session::Session::for_binding(binding.clone()).ok()?;
        let id = digest::bytes(format!("explain:{source_id}:{}", emitted.text).as_bytes()).ok()?;
        let marker = format!("WITCHY_BODY_CHECK_V2:{id}:{}", references.len());
        let module = emitted.audited(&marker).ok()?;
        session.request("explain", source_id);
        session.select_body(None).ok()?;
        match session.run(&module, &marker, context, &mut super::proof_measure::Measurements::default()) {
            Ok(_) => { let _ = session.park(); if let Some(cache) = &cache { cache.store_explained(&text, &[]); } }
            Err(_) => {
                let evidence = super::proof_session::take_last_failure();
                let named = evidence.as_ref().and_then(|path| fs::read(path.join("stdout")).ok())
                    .map(|bytes| failing(&module, &String::from_utf8_lossy(&bytes), IMPORTS.lines().count())).unwrap_or_default();
                drop(session);
                if let Some(path) = evidence { let _ = fs::remove_dir_all(path); }
                // A batch that failed without naming a goal (a time limit under load, a crash)
                // says nothing about which clause fails; claiming none would be false.
                if named.is_empty() { return None; }
                if let Some(cache) = &cache { cache.store_explained(&text, &named.iter().map(|(index, _)| *index).collect::<Vec<_>>()); }
                failed.extend(named.into_iter().map(|(index, _)| range.start + index));
            }
        }
    }
    Some(failed)
}
/// After a refused build, print which function and which authored clause failed, and return
/// where the W0402 points with what it says there (`proof_advice::refused`): the clause,
/// statement, loop or call responsible, what was not proved there, and one next step.
/// A body obligation is checked again a part at a time (each `ensures` at each `return`, each
/// run-time check), so the report names exactly the parts Lean cannot prove. This runs only on
/// failure and changes no admission decision.
pub(super) fn explain(program: &a::Program, source: &str, refusal: Refusal) -> Option<(std::ops::Range<usize>, std::borrow::Cow<'static, str>)> {
    let Refusal { mut failures, sdk, stalled, seconds } = refusal;
    // A request that ran past the checker's deadline names no goal: find the function whose
    // proof does not finish by checking each function of that request alone.
    let mut slow: Option<((String, bool), Option<u64>)> = None;
    if failures.is_empty() && !stalled.is_empty() {
        let (found, named) = stalled_function(&stalled, source, sdk.as_ref());
        failures.extend(named);
        slow = found.map(|found| (found, seconds));
    }
    let function_place = |name: &str| advice::function(program, name).map(|f| f.span.clone()).filter(|at| *at != (0..0));
    let mut reported = BTreeSet::new();
    let mut pointed: Option<advice::Refused> = None;
    if let Some(((name, alone), seconds)) = &slow {
        super::compiler_diagnostic!("{}", crate::diagnostic::source_spelling(&format!("proof failed: function `{name}`")));
        super::compiler_diagnostic!("  the proof checker stopped it at its deadline");
        let after = seconds.map(|s| format!(" after {s} s")).unwrap_or_default();
        let said = if *alone { format!("the proof of `{name}` timed out{after}: the proof checker stopped it at its deadline before Lean finished") }
            else { format!("the proofs checked with `{name}`, the largest of them, timed out{after}: the proof checker stopped them at its deadline before Lean finished") };
        pointed = function_place(name).map(|at| advice::refused(Unproved::Timeout, at, said));
    }
    for failure in &failures {
        if !reported.insert((failure.function.clone(), failure.kind, failure.callee.clone())) { continue; }
        let here = match (failure.kind, &failure.callee) {
            ("call_precondition", Some(callee)) => {
                super::compiler_diagnostic!("{}", call_refusal(&failure.function, callee));
                let clauses = call_parts(program, source, &failure.function, callee, sdk.as_ref()).filter(|c| !c.is_empty()).or_else(|| {
                    // If the call has just one precondition, its own span suffices
                    // even when the diagnostic prover is unavailable.
                    let f = advice::function(program, callee)?;
                    let [clause] = f.requires.as_slice() else { return None };
                    Some(vec![advice::written(source, &clause.span)])
                }).unwrap_or_default();
                for clause in &clauses { super::compiler_diagnostic!("  not proved: requires {clause}"); }
                let at = call_place(program, &failure.function, callee, failure.site).or_else(|| function_place(&failure.function));
                let shown = match clauses.as_slice() {
                    [] => format!("this call to `{callee}` does not establish the callee's `requires`"),
                    [one] => format!("this call to `{callee}` does not establish the callee's `requires {one}`"),
                    many => format!("this call to `{callee}` does not establish the callee's {}", many.iter().map(|c| format!("`requires {c}`")).collect::<Vec<_>>().join(" and ")),
                };
                at.map(|at| advice::refused(Unproved::CallRequires, at, crate::diagnostic::source_spelling(&shown).into_owned()))
            }
            ("lemma", _) => {
                super::compiler_diagnostic!("proof failed: lemma `{}`", failure.function);
                let at = program.declarations.iter().find_map(|d| match d { a::Declaration::Lemma(l) if l.name == failure.function => Some(l.ensures.span.clone()), _ => None });
                at.map(|at| { let said = format!("the lemma `{}` does not follow from its `requires`: `{}` is not proved", failure.function, advice::written(source, &at)); advice::refused(Unproved::Lemma, at, said) })
            }
            _ => {
                let message = format!("proof failed: function `{}`", failure.function);
                super::compiler_diagnostic!("{}", crate::diagnostic::source_spelling(&message));
                let timed = advice::timed_out(&failure.message);
                let parts = isolate(program, source, &failure.function, sdk.as_ref());
                let mut here = None;
                match &parts {
                    Some(Isolated::Parts(parts)) => {
                        for line in listed(source, parts) { super::compiler_diagnostic!("  not proved: {line}"); }
                        if !timed { here = point(program, source, &failure.function, parts); }
                    }
                    Some(Isolated::Combined) => {
                        super::compiler_diagnostic!("  each `ensures` clause is provable alone; the failure needs them combined");
                        if !timed {
                            here = function_place(&failure.function).map(|at| advice::refused(Unproved::Combined, at,
                                format!("each `ensures` clause of `{}` is shown alone, and not all of them together", failure.function)));
                        }
                    }
                    None => {
                        // The parts could not be checked one at a time (that check did not finish,
                        // or no checker was available for it): say what the proof had to show.
                        let mut all = every_part(program, source, &failure.function);
                        if all.is_empty() { all = modelled(program, source, &failure.function).unwrap_or_default(); }
                        if !all.is_empty() { super::compiler_diagnostic!("  the proof must show each of these; the failing ones could not be told apart:"); }
                        for line in listed(source, &all).into_iter().take(12) { super::compiler_diagnostic!("    {line}"); }
                        if all.len() > 12 { super::compiler_diagnostic!("    ... and {} more", all.len() - 12); }
                        if !timed {
                            let among = all.iter().filter_map(|part| part.at.as_ref().map(|at| (part, at))).take(4)
                                .map(|(part, at)| format!("{} ⟪at {}⟫", part.short(source), at.start)).collect::<Vec<_>>();
                            let among = if among.is_empty() { String::new() } else { format!("; it had to show, among others: {}", among.join(", ")) };
                            here = function_place(&failure.function).map(|at| advice::refused(Unproved::Unnarrowed, at,
                                format!("the proof of `{}` was refused, and checking its parts one at a time did not finish, so the failing part is not known{among}", failure.function)));
                        }
                    }
                }
                if timed {
                    let after = failure.seconds.map(|s| format!(" after {s} s")).unwrap_or_default();
                    let showing = match &parts { Some(Isolated::Parts(parts)) => parts.first().map(|p| format!(" while showing {}", p.short(source))).unwrap_or_default(), _ => String::new() };
                    here = function_place(&failure.function).map(|at| advice::refused(Unproved::Timeout, at,
                        format!("the proof of `{}` timed out{after}: Lean ran out of its step budget{showing}", failure.function)));
                }
                here.or_else(|| {
                    // Parts with no place of their own: said at the function, with the step their kind needs.
                    let (named, kind) = match &parts { Some(Isolated::Parts(parts)) if !parts.is_empty() => (format!(": what was not proved is {}", listed(source, parts).join("; ")), parts[0].kind), _ => (String::new(), Unproved::Unnarrowed) };
                    function_place(&failure.function).map(|at| { let mut refusal = advice::refused(kind, at, format!("the proof of `{}` was refused{named}", failure.function)); refusal.tentative = true; refusal })
                })
            }
        };
        super::compiler_diagnostic!("  Lean: {}", failure.message);
        // The first refusal with a place of its own; one that names only the function, last.
        let rank = |r: &advice::Refused| if r.kind == Unproved::Unnarrowed { 2 } else { u8::from(r.tentative) };
        if here.as_ref().is_some_and(|h| pointed.as_ref().is_none_or(|p| rank(h) < rank(p))) { pointed = here; }
    }
    pointed.map(|p| (p.at, p.message))
}
/// Where a call with a contract is written: the call's own place (`Function::call_spans`).
fn call_place(program: &a::Program, function: &str, callee: &str, site: Option<u32>) -> Option<std::ops::Range<usize>> {
    let logical = super::check::program(program).ok()?;
    let f = logical.functions.iter().find(|f| f.name == function)?;
    let at = match site {
        Some(site) => f.call_spans.get(site as usize).map(|(start, end)| *start..*end),
        None => None,
    };
    // A goal shared by several calls of one callee names one site; with none, the call written
    // with that name, when only one is (a method's name alone may be another type's method).
    at.or_else(|| {
        let written = advice::function(program, function)?;
        let short = callee.rsplit('.').next()?;
        let mut found = Vec::new();
        for statement in &mut written.body.clone() {
            statement.visit_exprs_mut(&mut |e| {
                if let a::Expression::Call(c, _) = &e.kind && match &c.kind { a::Expression::Name(n) => n == callee || n == short, a::Expression::Field(_, m) => m == short, _ => false } { found.push(e.span.clone()); }
                false
            });
        }
        match found.as_slice() { [one] => Some(one.clone()), _ => None }
    }).filter(|at| *at != (0..0))
}
/// The deadline case: which function's goals do not finish alone. Groups are checked largest
/// first; the first that runs to the deadline is named. Groups refused with a named goal are
/// kept as failures. At most six groups, and 40 s in all, are spent.
fn stalled_function(stalled: &[Goal], source: &str, sdk: Option<&standalone::Sdk>) -> (Option<(String, bool)>, Vec<Failure>) {
    let mut groups: Vec<(String, Vec<Goal>)> = Vec::new();
    for goal in stalled {
        let name = goal.source.as_ref().map_or_else(|| goal.source_name.clone(), |s| s.function.clone());
        match groups.iter_mut().find(|(n, _)| *n == name) { Some((_, g)) => g.push(goal.clone()), None => groups.push((name, vec![goal.clone()])) }
    }
    if let [(name, _)] = groups.as_slice() { return (Some((name.clone(), true)), Vec::new()); }
    groups.sort_by_key(|(_, g)| std::cmp::Reverse(g.iter().map(|goal| goal.statement.len()).sum::<usize>()));
    let mut named = Vec::new();
    super::proof_session::quiet(true);
    let mut found = None;
    let began = Instant::now();
    for (name, goals) in groups.iter().take(6) {
        if began.elapsed().as_secs() >= 40 { break; }
        let checked = Instant::now();
        match probe(goals, source, sdk) {
            // Only a check that ran to the deadline says the function does not finish; one that
            // failed sooner without naming a goal (no checker, a crash) says nothing.
            None if checked.elapsed().as_secs() >= 9 => { found = Some((name.clone(), true)); break; }
            None => {}
            Some(failed) => for index in failed { if let Some(goal) = goals.get(index) { named.push(failure(goal, "refused when checked alone".into())); } },
        }
    }
    super::proof_session::quiet(false);
    // None times out alone: together they passed the deadline; the largest is named.
    if found.is_none() && named.is_empty() { found = groups.first().map(|(name, _)| (name.clone(), false)); }
    (found, named)
}
/// A part of a function that was not proved: what it checks, where it was written when it has
/// a place of its own, and from the body's diagnostic labels the `return` it is shown at and
/// the loops around it or before it on the way there.
#[derive(Clone)]
struct Part { what: String, at: Option<std::ops::Range<usize>>, returned: Option<std::ops::Range<usize>>, loops: Vec<super::scalar_vc::NotedLoop>, kind: Unproved,
    /// What the refusal says at the part's place, when that is more than `what` is not proved.
    said: Option<String>,
    /// A likely place rather than a certain one (`modelled`).
    tentative: bool }
impl Part {
    fn plain(what: String, at: Option<std::ops::Range<usize>>, kind: Unproved) -> Self { Self { what, at, returned: None, loops: Vec::new(), kind, said: None, tentative: false } }
    fn saying(mut self, said: String) -> Self { self.said = Some(said); self }
    fn guessed(mut self) -> Self { self.tentative = true; self }
    /// The part in a few words, for a list.
    fn short(&self, source: &str) -> String {
        match (&self.kind, &self.at) {
            (Unproved::Ensures, _) => format!("`{}`", self.what),
            (Unproved::Invariant, Some(at)) => format!("the invariant `{}`", advice::written(source, at)),
            (Unproved::Overwrite | Unproved::Discard, Some(at)) => format!("{} (`{}`)", self.what, advice::written(source, at)),
            _ => self.what.clone(),
        }
    }
}
/// What `isolate` found.
enum Isolated { Parts(Vec<Part>), Combined }
/// The lines of a refusal: parts alike are counted, and an owner assignment or a discard names
/// the place whose value may still hold an owner.
fn listed(source: &str, parts: &[Part]) -> Vec<String> {
    let mut counted: Vec<(String, usize)> = Vec::new();
    for part in parts {
        let line = match (&part.kind, &part.at) {
            (Unproved::Overwrite | Unproved::Discard, Some(at)) => format!("{}: `{}`", part.what, advice::written(source, at)),
            (Unproved::Invariant, Some(at)) => format!("the invariant `{}` of a sealed type this function changes or returns, where the function returns", advice::written(source, at)),
            _ => part.what.clone(),
        };
        match counted.iter_mut().find(|(seen, _)| *seen == line) { Some((_, n)) => *n += 1, None => counted.push((line, 1)) }
    }
    counted.into_iter().map(|(what, n)| if n == 1 { what } else { format!("{what} ({n} places)") }).collect()
}
/// Where a refusal points, and what it says there: the first part (in the order the body is
/// walked) with a place, or the loop that changes what that part reads when the loop has no
/// invariant (or too little). The other places are named by their lines (`⟪at N⟫`, files.rs
/// `places`).
fn point(program: &a::Program, source: &str, name: &str, parts: &[Part]) -> Option<advice::Refused> {
    let written = advice::function(program, name);
    // The text a part reads, to match against the names a loop changes.
    let reads = |part: &Part| match (&part.kind, &part.at) {
        (Unproved::Ensures | Unproved::Invariant | Unproved::Check, Some(at)) => Some(advice::written(source, at)),
        (Unproved::Check, None) => advice::check_of(&part.what).1.map(str::to_owned),
        _ => None,
    };
    // The innermost (or most recent) loop that changes a name the part reads, and those names.
    fn changing<'p>(part: &'p Part, text: Option<String>) -> Option<(&'p super::scalar_vc::NotedLoop, Vec<String>)> {
        let text = text?;
        part.loops.iter().find_map(|noted| { let changed = advice::changed_by(&text, &noted.names); (!changed.is_empty()).then_some((noted, changed)) })
    }
    let looped = |part| changing(part, reads(part));
    let part = parts.iter().find(|p| looped(p).is_some() || p.at.is_some() || p.returned.is_some() || (p.kind == Unproved::LoopInvariant && !p.loops.is_empty()))?;
    // The other places not proved, and loops that leave what other parts read unknown.
    let others = |at: &std::ops::Range<usize>| {
        let more = parts.iter().filter_map(|p| p.returned.clone().or_else(|| p.at.clone())).filter(|other| other != at && Some(other) != part.at.as_ref())
            .map(|other| other.start).collect::<BTreeSet<_>>().into_iter().take(4).map(|o| format!("⟪at {o}⟫")).collect::<Vec<_>>();
        let more = if more.is_empty() { String::new() } else { format!(" (not proved either: {})", more.join(", ")) };
        let mut loops: Vec<(std::ops::Range<usize>, bool, Vec<String>)> = Vec::new();
        for (noted, changed) in parts.iter().filter_map(|p| looped(p)) {
            if noted.at == *at { continue; }
            match loops.iter_mut().find(|(seen, _, _)| *seen == noted.at) {
                Some((_, _, names)) => for n in changed { if !names.contains(&n) { names.push(n); } },
                None => loops.push((noted.at.clone(), noted.invariants, changed)),
            }
        }
        let loops = loops.into_iter().take(2).map(|(at, invariants, names)| format!("; the loop at ⟪at {}⟫ {} what holds of {}", at.start,
            if invariants { "has invariants that do not say" } else { "has no invariant saying" }, names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", "))).collect::<String>();
        format!("{more}{loops}")
    };
    let returning = |returned: &Option<std::ops::Range<usize>>| match returned {
        Some(_) => "at this `return`",
        None => "where the function ends",
    };
    let shown = |part: &Part| match (&part.kind, &part.at) {
        (Unproved::Ensures, Some(at)) if part.what.starts_with("ensures, ") => format!("`{}` (⟪at {}⟫)", part.what, at.start),
        (Unproved::Ensures, Some(at)) => format!("`ensures {}` (⟪at {}⟫)", advice::written(source, at), at.start),
        (Unproved::Invariant, Some(at)) => format!("the invariant `{}` (⟪at {}⟫)", advice::written(source, at), at.start),
        (_, Some(at)) => format!("{} (⟪at {}⟫)", part.what, at.start),
        _ => part.what.clone(),
    };
    if let Some((noted, _)) = looped(part) {
        // Every part this loop leaves unknown, by the names they read.
        let mut names: Vec<String> = Vec::new();
        for (other, changed) in parts.iter().filter_map(|p| looped(p)) { if other.at == noted.at { for n in changed { if !names.contains(&n) { names.push(n); } } } }
        let roots = names.iter().filter_map(|n| n.split('.').next()).fold(Vec::<&str>::new(), |mut seen, r| { if !seen.contains(&r) { seen.push(r); } seen });
        let list = |items: &[&str]| items.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
        let names_shown = list(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let whose = if noted.inside { "on a pass" } else { "after it" };
        let at = noted.at.clone();
        return Some(if noted.invariants {
            advice::refused(Unproved::LoopSaysTooLittle, at.clone(), format!("this loop's invariants do not say enough of {names_shown}: the loop changes {}, so {whose} {} is not proved{}", list(&roots), shown(part), others(&at)))
        } else {
            advice::refused(Unproved::LoopNeedsInvariant, at.clone(), format!("this loop needs an invariant stating what holds of {names_shown} on every pass: the loop changes {}, and with no invariant nothing is known of them {whose}, so {} is not proved{}", list(&roots), shown(part), others(&at)))
        });
    }
    if let (Some(said), Some(at)) = (&part.said, &part.at) { let mut refusal = advice::refused(part.kind, at.clone(), format!("{said}{}", others(at))); refusal.tentative = part.tentative; return Some(refusal); }
    Some(match (part.kind, &part.at) {
        (Unproved::Overwrite, Some(at)) => advice::refused(part.kind, at.clone(), format!("`{}` may still hold an owner when this assigns over it, and that owner would be lost{}", advice::written(source, at), others(at))),
        (Unproved::Discard, Some(at)) => advice::refused(part.kind, at.clone(), format!("`{}` may still hold an owner here, and discarding it would lose that owner{}", advice::written(source, at), others(at))),
        (Unproved::Unreachable, Some(at)) => {
            let said = match written.and_then(|f| advice::unreachable_condition(source, &f.body, at)) {
                Some(condition) => format!("some path reaches this `unreachable`: the proof could not rule out that {condition} here"),
                None => "some path reaches this `unreachable`: nothing the proof knows on the way here rules it out".to_owned(),
            };
            advice::refused(part.kind, at.clone(), format!("{said}{}", others(at)))
        }
        (Unproved::Ensures, Some(clause)) => {
            let at = part.returned.clone().unwrap_or_else(|| clause.clone());
            let said = if part.returned.is_some() { format!("{} is not shown {}", shown(part), returning(&part.returned)) }
                else { format!("this clause, {}, is not shown where the function returns", shown(part)) };
            advice::refused(part.kind, at.clone(), format!("{said}{}", others(&at)))
        }
        (Unproved::Invariant, Some(_)) => {
            let at = part.returned.clone().or_else(|| written.map(|f| f.span.clone())).or_else(|| part.at.clone())?;
            let said = format!("{} of a sealed type this function changes or returns may not hold {}", shown(part), returning(&part.returned));
            advice::refused(part.kind, at.clone(), format!("{said}{}", others(&at)))
        }
        (Unproved::LoopInvariant, _) => {
            let at = part.loops.first().map(|l| l.at.clone()).or_else(|| part.at.clone())?;
            advice::refused(part.kind, at.clone(), format!("{} is not proved{}", part.what, others(&at)))
        }
        (_, None) => {
            let at = part.returned.clone()?;
            advice::refused(part.kind, at.clone(), format!("{} is not proved at this `return`{}", part.what, others(&at)))
        }
        (_, Some(at)) => advice::refused(part.kind, at.clone(), format!("{} is not proved{}", part.what, others(at))),
    })
}
fn isolate(program: &a::Program, source: &str, name: &str, sdk: Option<&standalone::Sdk>) -> Option<Isolated> {
    let function = advice::function(program, name)?;
    let body_only = function.ensures.iter().all(|post| matches!(
        &post.kind, a::PostconditionKind::Predicate(a::Expr { kind: a::Expression::Bool(true), .. })
    ));
    // Literal true adds no claim for the diagnostic probe to isolate: each part of the body
    // alone, each run-time check and each invariant at each `return`.
    if body_only && let Some(parts) = body_parts(program, source, name, sdk, None) && !parts.is_empty() {
        return Some(Isolated::Parts(parts));
    }
    let text = |span: &std::ops::Range<usize>| advice::written(source, span);
    let truth = |span: &std::ops::Range<usize>| a::Postcondition { kind: a::PostconditionKind::Predicate(a::Expr { kind: a::Expression::Bool(true), span: span.clone() }), span: span.clone() };
    let baseline = function.ensures.iter().map(|post| match &post.kind {
        a::PostconditionKind::Predicate(_) => truth(&post.span),
        a::PostconditionKind::Match { value, arms } => a::Postcondition { span: post.span.clone(), kind: a::PostconditionKind::Match { value: value.clone(),
            arms: arms.iter().map(|arm| a::PostconditionArm { variant: arm.variant.clone(), binding: arm.binding.clone(), clauses: vec![truth(&post.span)] }).collect() } },
    }).collect::<Vec<_>>();
    let mut variants = vec![(None, "a safety condition inside the body".to_owned(), baseline.clone())];
    for (index, post) in function.ensures.iter().enumerate() {
        match &post.kind {
            a::PostconditionKind::Predicate(_) => { let mut ensures = baseline.clone(); ensures[index] = post.clone(); variants.push((Some(post.span.clone()), format!("ensures {}", text(&post.span)), ensures)); }
            a::PostconditionKind::Match { arms, .. } => for (arm_index, arm) in arms.iter().enumerate() {
                for clause in &arm.clauses {
                    let mut ensures = baseline.clone();
                    if let a::PostconditionKind::Match { arms, .. } = &mut ensures[index].kind { arms[arm_index].clauses = vec![clause.clone()]; }
                    variants.push((Some(clause.span.clone()), format!("ensures, {} arm: {}", arm.variant, text(&clause.span)), ensures));
                }
            },
        }
    }
    let (mut labels, mut goals) = (Vec::new(), Vec::new());
    for (at, label, ensures) in variants {
        let mut variant = program.clone();
        for declaration in &mut variant.declarations { if let a::Declaration::Function(f) = declaration { if f.name == name { f.ensures = ensures.clone(); } } }
        let Ok(logical) = super::check::program(&variant) else { continue };
        let Ok(prepared) = super::source_vc::prepare_with_sites(&logical) else { continue };
        let Some(goal) = prepared.goals.into_iter().find(|g| g.source.as_ref().is_some_and(|s| s.kind == "function_body" && s.function == name)) else { continue };
        labels.push((label, at)); goals.push(goal);
    }
    super::proof_session::quiet(true);
    let failed = probe(&goals, source, sdk);
    super::proof_session::quiet(false);
    let failed = failed?;
    // A failing body condition makes every variant fail; the parts of a body a proof model
    // checks as one statement are named by what they are and where.
    if failed.contains(&0) {
        let parts = if body_only { None } else { body_parts(program, source, name, sdk, None) };
        return Some(Isolated::Parts(match parts {
            Some(parts) if !parts.is_empty() => parts,
            _ => modelled(program, source, name).unwrap_or_else(|| vec![Part::plain(labels[0].0.clone(), None, Unproved::Unnarrowed)]),
        }));
    }
    let clauses: Vec<(String, Option<std::ops::Range<usize>>)> = failed.into_iter().filter_map(|index| labels.get(index).cloned()).collect();
    if clauses.is_empty() { return Some(Isolated::Combined); }
    // Each failing clause again at each `return`, for where it fails and the loops on the way.
    let spans: Vec<std::ops::Range<usize>> = clauses.iter().filter_map(|(_, at)| at.clone()).collect();
    if let Some(mut parts) = body_parts(program, source, name, sdk, Some(&spans)) && !parts.is_empty() {
        // Said as the clause was found: with its arm (`ensures, Err arm: ...`).
        for part in &mut parts {
            if let Some((what, _)) = clauses.iter().find(|(_, at)| at.as_ref().zip(part.at.as_ref()).is_some_and(|(clause, site)| clause.start <= site.start && site.end <= clause.end)) { part.what = what.clone(); }
        }
        return Some(Isolated::Parts(parts));
    }
    Some(Isolated::Parts(clauses.into_iter().map(|(what, at)| Part::plain(what, at, Unproved::Ensures)).collect()))
}
/// What to say of a body condition that failed in a function proved by the memory or record
/// model, whose goal is one statement with no parts: the construct in the body that model does
/// not read (a call of a function with no contract in a model's vocabulary: what such a call
/// was lent is unknown to it afterwards), at the call; or, with none, the operations whose
/// conditions the model checks, each where it is written.
fn modelled(program: &a::Program, source: &str, name: &str) -> Option<Vec<Part>> {
    let logical = super::check::program(program).ok()?;
    let function = logical.functions.iter().find(|f| f.name == name)?;
    let contract = function.contract.as_ref()?;
    let model = if contract.record.is_some() { "record" } else if contract.memory.is_some() { "memory" } else if contract.storage.is_some() { "storage" } else { return None };
    // Calls of functions with no contract in a model's vocabulary: after one, what it was lent
    // is unknown to the model. Of those, the ones with a `requires`, which the model shows at the
    // call with this body's contract in view.
    let mut unread: Vec<String> = Vec::new();
    for edge in &function.calls {
        let Some(callee) = logical.functions.get(edge.definition.0 as usize) else { continue };
        if callee.contract.as_ref().is_some_and(|c| c.record.is_some() || c.memory.is_some() || c.storage.is_some()) { continue; }
        if !unread.contains(&callee.name) { unread.push(callee.name.clone()); }
    }
    let requiring = |full: &str| advice::function(program, full).is_some_and(|f| !f.requires.is_empty());
    // Where each is called, found by the name the call is written with; and each `memory.`
    // operation.
    let written = advice::function(program, name)?;
    let mut calls: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let mut operations: Vec<std::ops::Range<usize>> = Vec::new();
    for statement in &mut written.body.clone() {
        statement.visit_exprs_mut(&mut |e| {
            if let a::Expression::Call(callee, _) = &e.kind {
                let called = match &callee.kind { a::Expression::Name(n) => Some(n.as_str()), a::Expression::Field(_, method) => Some(method.as_str()), _ => None };
                if let Some(called) = called && let Some(full) = unread.iter().find(|full| full.rsplit('.').next() == Some(called))
                    && !calls.iter().any(|(at, _)| *at == e.span) && e.span != (0..0) { calls.push((e.span.clone(), full.clone())); }
                if let a::Expression::Field(base, _) = &callee.kind && matches!(&base.kind, a::Expression::Name(n) if n == "memory") && e.span != (0..0) { operations.push(e.span.clone()); }
            }
            false
        });
    }
    calls.sort_by_key(|(at, _)| at.start);
    operations.sort_by_key(|at| at.start);
    let (asking, unknown): (Vec<_>, Vec<_>) = calls.into_iter().partition(|(_, full)| requiring(full));
    let why = format!("`{name}` is proved by the {model} proof model, not over numbers, because it takes or returns a sealed value over a Buffer (or a record that holds one); that model follows `memory.*` operations and calls of functions a model proves");
    let one = format!("the {model} proof model checks the body as one statement and cannot say which part failed");
    let mut parts = Vec::new();
    if let Some((at, callee)) = unknown.first() {
        parts.push(Part::plain(format!("a fact the {model} proof model cannot follow past this call of `{callee}`, which it does not read"), Some(at.clone()), Unproved::Modelled)
            .guessed().saying(format!("`{}` calls `{callee}`, which the {model} proof model does not read: {why}, and after this call it knows nothing of what the call was lent", advice::written(source, at))));
    }
    for (at, callee) in &asking {
        let clauses = advice::function(program, callee).map(|f| f.requires.iter().map(|r| format!("`requires {}`", advice::written(source, &r.span))).collect::<Vec<_>>().join(" and ")).unwrap_or_default();
        parts.push(Part::plain(format!("the `requires` of this call of `{callee}`"), Some(at.clone()), Unproved::CallRequires)
            .guessed().saying(format!("this call of `{callee}` may not establish its {clauses}; {one}, and this is the first call it checks a `requires` at")));
    }
    for at in &operations {
        parts.push(Part::plain(format!("what `{}` requires of its cells", advice::written(source, at)), Some(at.clone()), Unproved::Check)
            .guessed().saying(format!("what `{}` requires of its cells (in bounds, initialized or empty as it needs) may not be shown here; {one}, and this is the first operation with such a condition", advice::written(source, at))));
    }
    for (at, callee) in unknown.iter().skip(1) { parts.push(Part::plain(format!("(another call the {model} proof model does not read: `{callee}`)"), Some(at.clone()), Unproved::Modelled)); }
    if parts.is_empty() {
        // No call or operation with a condition of its own: what is shown at a `return` (the
        // function's `ensures`, the invariants of the sealed types it takes or returns).
        let mut claims: Vec<String> = written.ensures.iter().map(|post| format!("`ensures {}` (⟪at {}⟫)", advice::written(source, &post.span), post.span.start)).collect();
        claims.extend(advice::invariants_of(program, written).into_iter().map(|at| format!("the invariant `{}` (⟪at {}⟫)", advice::written(source, &at), at.start)));
        let returns = advice::returns_of(written);
        match returns.first() {
            Some(at) if !claims.is_empty() => {
                let more = returns.iter().skip(1).take(4).map(|r| format!("⟪at {}⟫", r.start)).collect::<Vec<_>>();
                let more = if more.is_empty() { String::new() } else { format!(" or at another `return` ({})", more.join(", ")) };
                parts.push(Part::plain(format!("what the function promises where it returns (the {model} proof model checks it as one statement)"), Some(at.clone()), Unproved::Ensures)
                    .guessed().saying(format!("one of {} may not hold at this `return`{more}; {one}", claims.join(", "))));
            }
            _ => parts.push(Part::plain(format!("a condition of the body (the {model} proof model checks it as one statement)"), None, Unproved::Unnarrowed)
                .guessed().saying(format!("a condition of the body of `{name}` is not proved: {why}, and checks the body as one statement"))),
        }
    }
    Some(parts)
}
/// A body part's label, as a refusal says it: its kind, where it was written, its `return` and loops.
fn part(program: &a::Program, source: &str, name: &str, authored: &[std::ops::Range<usize>], label: &str) -> Part {
    let read = super::scalar_vc::PartLabel::read(label);
    let super::scalar_vc::PartLabel { what, site, returned, loops } = read;
    let (what, at, kind) = if what.starts_with(super::scalar_vc::ENSURES) {
        match site {
            Some(at) if authored.iter().any(|clause| clause.start <= at.start && at.end <= clause.end) => (format!("ensures {}", advice::written(source, &at)), Some(at), Unproved::Ensures),
            Some(at) if !advice::written(source, &at).is_empty() => (what, Some(at), Unproved::Invariant),
            _ => (what, None, Unproved::Ensures),
        }
    } else if what.starts_with(super::scalar_vc::OVERWRITE) { (what, site, Unproved::Overwrite) }
    else if what.starts_with(super::scalar_vc::DISCARD) { (what, site, Unproved::Discard) }
    else if what.starts_with(super::scalar_vc::UNREACHED) { (what, site, Unproved::Unreachable) }
    else if what.starts_with("a loop invariant") { (what, site, Unproved::LoopInvariant) }
    else {
        // A run-time check: where it is written, from the local its value is assigned to.
        let at = site.or_else(|| {
            let (operator, local) = advice::check_of(&what);
            advice::assigned_with(&advice::function(program, name)?.body, local?, operator?)
        });
        (what, at, Unproved::Check)
    };
    Part { what, at, returned, loops, kind, said: None, tentative: false }
}
/// Every part of a function's body, unchecked: what a refusal lists when the parts could not
/// be checked one at a time.
fn every_part(program: &a::Program, source: &str, name: &str) -> Vec<Part> {
    let Ok(logical) = super::check::program(program) else { return Vec::new() };
    let Ok(goals) = super::scalar_vc::diagnostic_parts(&logical) else { return Vec::new() };
    let authored = authored(program, name);
    goals.iter().filter(|g| g.source.as_ref().is_some_and(|s| s.kind == "function_body_part" && s.function == name))
        .map(|g| part(program, source, name, &authored, g.source.as_ref().and_then(|s| s.callee.as_deref()).unwrap_or_default())).collect()
}
/// The function's own `ensures` clauses, as written: a clause shown at a return that is not
/// one of them is an invariant of a type the function changes or returns.
fn authored(program: &a::Program, name: &str) -> Vec<std::ops::Range<usize>> {
    advice::function(program, name).map(|f| f.ensures.iter().map(|post| post.span.clone()).collect()).unwrap_or_default()
}
/// The parts of a function's body that do not prove alone, each as what it checks (counted when
/// several are alike): the body's goal again, a part at a time.
/// With `only`, just the clauses shown at a `return` written inside those places.
fn body_parts(program: &a::Program, source: &str, name: &str, sdk: Option<&standalone::Sdk>, only: Option<&[std::ops::Range<usize>]>) -> Option<Vec<Part>> {
    let logical = super::check::program(program).ok()?;
    let wanted = |label: &str| only.is_none_or(|spans| {
        let read = super::scalar_vc::PartLabel::read(label);
        read.what.starts_with(super::scalar_vc::ENSURES) && read.site.is_some_and(|at| spans.iter().any(|clause| clause.start <= at.start && at.end <= clause.end))
    });
    let goals: Vec<Goal> = super::scalar_vc::diagnostic_parts(&logical).ok()?.into_iter().filter(|g| g.source.as_ref().is_some_and(|s| s.kind == "function_body_part" && s.function == name
        && wanted(s.callee.as_deref().unwrap_or_default()))).collect();
    if goals.is_empty() { return None; }
    let labels: Vec<String> = goals.iter().map(|g| g.source.as_ref().and_then(|s| s.callee.clone()).unwrap_or_default()).collect();
    super::proof_session::quiet(true);
    let failed = probe(&goals, source, sdk);
    super::proof_session::quiet(false);
    let authored = authored(program, name);
    Some(failed?.into_iter().filter_map(|index| labels.get(index)).map(|label| part(program, source, name, &authored, label)).collect())
}
/// The `requires` clauses of `callee` that the calls in `name` do not show, as written: the
/// call goals again, a clause at a time.
fn call_parts(program: &a::Program, source: &str, name: &str, callee: &str, sdk: Option<&standalone::Sdk>) -> Option<Vec<String>> {
    let logical = super::check::program(program).ok()?;
    let mut texts = Vec::new();
    let goals: Vec<Goal> = super::scalar_vc::diagnostic_parts(&logical).ok()?.into_iter().filter(|g| g.source.as_ref().is_some_and(|s| s.kind == "call_precondition_part" && s.function == name
        && s.callee.as_deref().and_then(|c| c.split('\u{1}').next()) == Some(callee))).collect();
    for goal in &goals {
        let parts: Vec<&str> = goal.source.as_ref().and_then(|s| s.callee.as_deref()).unwrap_or("").split('\u{1}').collect();
        let span = match parts.as_slice() { [_, start, end] => start.parse::<usize>().ok().zip(end.parse::<usize>().ok()), _ => None };
        texts.push(span.and_then(|(start, end)| source.get(start..end)).map(|t| t.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default());
    }
    if goals.is_empty() { return None; }
    super::proof_session::quiet(true);
    let failed = probe(&goals, source, sdk);
    super::proof_session::quiet(false);
    let mut out: Vec<String> = Vec::new();
    for index in failed? { if let Some(text) = texts.get(index) && !text.is_empty() && !out.contains(text) { out.push(text.clone()); } }
    Some(out)
}
/// Both the compiler's wire check and the installed helper refuse larger requests.
const REQUEST_BYTES: usize = 65536;
// Keep split sequents as independent declarations (and kernel heartbeat budgets).
// Amortize imports and the inventory walk over a small group, without filling
// the entire wire allowance with tactics from a large function.
const SEQUENTS_PER_REQUEST: usize = 32;
/// The proof work a request may carry, in `searches`. Bytes alone let one request hold four
/// Deque bodies, 59 G Lean instructions, against a per-request deadline. At this bound the
/// library's largest request measures 18.4 G with its 6.2 G of imports (docs/decisions.md,
/// "Deque's proofs and a bound on each request's proof work", 2026-11-25).
const SEARCHES_PER_REQUEST: usize = 160;
/// A deterministic stand-in for a goal's proof work: the closing tactics in its proof (each runs
/// omega or `simp` over the goal's context), and one more per 160 bytes of statement, which
/// Lean elaborates and every tactic carries. Fitted to the Lean instructions of the library's
/// requests: about 0.05 G a unit. A tactic under `k` case splits runs `2^k` times but counts
/// once, so the estimate is loose for such proofs; the deadline still bounds them.
fn searches(goal: &Goal) -> usize {
    let closers = match &goal.prover {
        Some(prover) => ["witchy_close", "witchy_omega", "witchy_inline_omega", "simp_all"].iter().map(|tactic| prover.matches(tactic).count()).sum(),
        // The default proof (`proof_module::emit`) tries omega, then `simp_all`.
        None => if goal.statement == "True" { 0 } else { 2 },
    };
    closers + goal.statement.len() / 160
}
/// Contiguous goal ranges whose audited modules fit one request each. One range
/// when everything fits, so existing programs produce byte-identical requests.
fn batches(goals: &[Goal]) -> Result<Vec<std::ops::Range<usize>>> {
    let size = |range: std::ops::Range<usize>| -> Result<usize> {
        let references = goals[range].iter().collect::<Vec<_>>();
        // The marker is a fixed-width hash plus a goal count; reserve its longest form.
        Ok(super::proof_module::emit(super::proof_module::Namespace::Current, &references, None)?.audited(&"M".repeat(112))?.len())
    };
    let sequent = |g: &Goal| g.source.as_ref().is_some_and(|s| s.kind == "function_body_sequent");
    if !goals.iter().any(sequent) && goals.iter().map(searches).sum::<usize>() <= SEARCHES_PER_REQUEST
        && size(0..goals.len())? <= REQUEST_BYTES { return Ok(vec![0..goals.len()]); }
    let (mut out, mut start) = (Vec::new(), 0);
    while start < goals.len() {
        // A large conjunction was split into individually checked sequents.
        // Bound their number per process as well as its ordinary byte limit.
        let mut end = start + 1;
        let mut sequents = usize::from(sequent(&goals[start]));
        let mut search = searches(&goals[start]);
        while end < goals.len() {
            sequents += usize::from(sequent(&goals[end]));
            search += searches(&goals[end]);
            if sequents > SEQUENTS_PER_REQUEST || search > SEARCHES_PER_REQUEST || size(start..end + 1)? > REQUEST_BYTES { break; }
            end += 1;
        }
        out.push(start..end);
        start = end;
    }
    Ok(out)
}
/// Size of each request the goals would be sent in. A program may need several
/// requests; what must hold is that every one of them fits.
#[cfg(test)]
pub(super) fn request_sizes(goals: &[Goal]) -> Result<Vec<usize>> {
    batches(goals)?.into_iter().map(|range| {
        let references = goals[range].iter().collect::<Vec<_>>();
        Ok(super::proof_module::emit(super::proof_module::Namespace::Current, &references, None)?.audited(&"M".repeat(112))?.len())
    }).collect()
}
pub(super) fn check_prepared(program: &a::Program, source: &str, prepared: super::proof_body::Prepared, mut session: Option<&mut super::proof_session::Session>, refusal: &mut Refusal) -> Result<Checked> {
    let super::proof_body::Prepared { goals:source_goals, sites, .. } = prepared;
    let phase = Instant::now();
    let mut measurements = super::proof_measure::Measurements::default();
    let scalar=source_goals.iter().any(|g|g.source.as_ref().is_some_and(|s|!s.storage&&!s.record&&!s.memory));
    let memory=source_goals.iter().any(|g|g.source.as_ref().is_some_and(|s|s.memory));
    let record=source_goals.iter().any(|g|g.source.as_ref().is_some_and(|s|s.record));
    let record_callers=source_goals.iter().any(|g|g.source.as_ref().is_some_and(|s|s.record&&s.kind=="call_precondition"));
    let owned_posts=source_goals.iter().any(|g|g.prover.as_ref().is_some_and(|p|p.starts_with(super::record_post::MARKER)));
    let storage=source_goals.iter().any(|g|g.source.as_ref().is_some_and(|s|s.storage));
    let mut goals = goals(program)?;goals.extend(source_goals);
    measurements.selected_goals = goals.len();
    measurements.statement_bytes = goals.iter().map(|goal| goal.statement.len()).sum();
    measurements.checked_nodes = goals.iter().map(|goal| goal.nodes).sum();
    measurements.goal_resolution_ns = phase.elapsed().as_nanos();
    if goals.is_empty() { return Ok(Checked { inventory: String::new(), fresh: String::new(), invocations: 0, measurements }); }
    let phase = Instant::now();
    let source_id = io(digest::bytes(source.as_bytes()))?;
    let mut model=MODEL.to_string();
    if scalar{model.push_str("; witchy.checked-source-u64.v1: parameter-only requires, distinct actual-return ensures; pure straightline nongeneric U64; mathematical Nat addition with every runtime add range proved for contracted bodies; every actual call precondition and modular checked post/range; normal-return partial correctness, not stack/host/interruption freedom; translation/VC completeness/layout/emission trusted compiler TCB");}
    if memory{model.push_str(&format!("; {}: actual typed CopyStorage reads and exclusive-borrow initialization require kernel-proved bounds plus initialized or empty cells at every unsafe occurrence and current caller precondition after argument evaluation; author Witchy clauses/auto proofs, actual before/after cell updates, fixed allocation keys/capacities and declared modular posts; one- or two-borrow buffer foci with checked same-prefix and unchanged-suffix contracts; source-authored loop invariants checked for initiation, inductive preservation over fresh mutable head state, and exit/early return; pure guards and restricted effects, normal-return partial correctness without a termination claim; guarded safe wrappers; Source Flow remains trusted exclusive-borrow authority; translation/VC completeness, token/layout/metadata/address arithmetic and emitted Wasm correspondence remain trusted compiler/runtime responsibilities",super::memory_model::MODEL));}
    if storage{model.push_str(&format!("; {}: arbitrary CopyStorage alpha and complete allocation key; valid focused entry cells; actual Init bounds/Empty transition determines Result and after-cells; source typed call validity implicit, explicit precondition True; normal returns only; physical identity/metadata/bitmap/count/codec/Wasm correspondence and VC translation trusted compiler TCB",super::storage_model::MODEL));}
    if record{model.push_str(&format!("; {}: generic zero-or-one-entry-owner record paths, independent bounded public headers, declared requirements, actual finite typed moves/stores/checked Init/Take and proof-required InitUnchecked/control/returns; checked Take evaluates receiver then index before its cell snapshot, returns exact old nonempty Option payload and clears only that cell on Ok, preserves state on bounds Err1 or Empty Err2; actual unchecked index bounds and Empty plus supplied payload checked before exact cell update; every Add guarded nonoverflow and every Sub pre-operation no-underflow proved, Nat subtraction never grants missing runtime permission; empty-requires caller sites universally valid without heap or post assumptions, nontrivial sites retain interpreted history/dependencies; normal returns under declared requires; general owned Buffer/record allocation constructors with typed owned Ok/U64 Err and Unit cleanup; zero-entry-owner/mixed-owned Result bodies excluded from legacy same-owner post reuse, demonic allocation failure or fresh empty allocation, logical liveness and exact returned-resource closure; actual helper posts only after arguments/snapshot/havoc and current same-batch body/site proofs with acyclic imports, unsupported/self calls require current-path False without recursive post assumptions; physical allocator freshness/release/count correspondence, identity/layout/codec/emission and VC translation trusted compiler TCB",super::record_model::MODEL));}
    if record{model.push_str(&format!("; {}: opaque returned elements are Option alpha with nonnull only on their own normal arm; Cell and generic equality normalize actual Element to Some and Stored to its option with known path-local nonnull history; Init inserts that option and caller opaque payload nonnull holds only on successful insertion; no synthetic global alpha witness; actual entry element binders remain; concrete erased element arithmetic/comparisons refused",super::record_model::OPTION_MODEL));}
    if record_callers{model.push_str(&format!("; {}: actual typed caller prefixes; per-focus allocation with requested capacity and all-None cells on arbitrary normal Ok; Err creates no owner; opaque payload-insensitive occupancy; unknown call havoc; no callee post reuse; only normal reaching-call preconditions; physical allocator freshness/identity/grants/metadata/codec and VC translation trusted compiler TCB",super::record_caller::MODEL));}
    if owned_posts{model.push_str(&format!("; {}: sealed actual same-owner finite returns and modeled single-focus effects; complete body and predecessor-site preconditions required in this atomic batch; demonic typed result arms; immutable after-all-arguments entry snapshots; path-scoped whole Prefix/U64 conjunct projection; Err reuse additionally requires an explicit active-arm after-Prefix on the sealed returned owner, otherwise history unknown; modular dependency and VC translation trusted compiler TCB",super::record_post::MODEL));}
    if memory||storage||record {model.push_str("; no frame theorem: nothing is claimed about allocations a contract does not name, and no physical heap/component isolation follows; mutable-call cells remain havoced unless actual declared cell posts constrain them; physical footprint correspondence remains trusted compiler/runtime TCB");}
    if goals.iter().any(|g| g.source.as_ref().is_some_and(|s| s.kind == "enclosing_member")) {
        model.push_str("; recursive shared projection v1: finite strict owning descent in the resolved selector, immutable initialized slot-zero focus, variant-specific loan evidence, checked local focus postcondition at each occurrence, exact loaded weak field in the source member, nonnull operand, null outer boundary, matching base and allocation incarnation at an earlier live focus; projection, loan evidence propagation, source-to-VC and Wasm lowering trusted");
    }
    let model_id = io(digest::bytes(model.as_bytes()))?;
    let audit_id = io(digest::bytes(AUDIT.as_bytes()))?;
    let imports_id = io(digest::bytes(IMPORTS.as_bytes()))?;
    // The wire protocol caps one request. A program whose proofs fit is sent exactly
    // as before. A larger one is split; every request must pass before publication.
    // Goals this checker has already accepted are not sent again. Each is described in the
    // inventory as cached; the rest go through exactly the path below.
    let mut cache = super::proof_cache::Cache::configured(session.as_deref().and_then(|s| s.cache_identity()))?;
    if let Some(cache) = &mut cache { cache.bind_libraries(program, source, &model_id)?; }
    let describe = |goal: &Goal| goal.source.as_ref().map(|identity|{
        let call=identity.callee.as_ref().map(|callee|format!(",\"callee\":\"{callee}\",\"site\":{}",identity.site.expect("call site"))).unwrap_or_default();
        let model=if identity.storage||identity.record||identity.memory{format!(",\"source_model\":\"{}\"{}",if identity.memory{super::memory_model::MODEL}else if identity.record{super::record_model::MODEL}else{super::storage_model::MODEL},if identity.kind=="function_body"{if goal.statement.starts_with("∀ (α"){",\"generic_parameters\":1"}else{",\"generic_parameters\":0"}}else{""})}else{String::new()};
        format!(",\"goal_kind\":\"{}\",\"function\":\"{}\"{call}{model}",identity.kind,identity.function)
    }).unwrap_or_default();
    let mut cached: Vec<Option<String>> = Vec::new();
    // Goals Lean refused before, with the same text and checker: refused again without asking it.
    let mut known: Vec<Failure> = Vec::new();
    let _locks;
    let goals = if let Some(cache) = &cache {
        let mut statuses = goals.iter().map(|g| cache.find(g)).collect::<Result<Vec<_>>>()?;
        _locks = cache.reserve(&goals, &mut statuses)?;
        let mut fresh = Vec::new();
        for (goal, status) in goals.into_iter().zip(statuses) {
            match status {
                super::proof_cache::Found::Refused(message) => { known.push(failure(&goal, message)); }
                super::proof_cache::Found::Accepted(axioms) => {
                    let goal_id = io(digest::bytes(goal.statement.as_bytes()))?;
                    cached.push(Some(format!("{{\"declaration\":\"{}\",\"expected_statement\":\"{}\",\"statement_sha256\":\"{goal_id}\",\"checked_nodes\":{},\"outcome\":\"cached_kernel_checked\",\"axioms\":[{axioms}]{}}}", goal.source_name, goal.statement, goal.nodes, describe(&goal))));
                }
                super::proof_cache::Found::Missing => { cached.push(None); fresh.push(goal); }
            }
        }
        fresh
    } else { goals };
    if cached.iter().any(Option::is_some) && let Some(session) = session.as_deref_mut() {
        session.note_cached_proofs();
    }
    let cache_note = |fresh: usize| cache.as_ref().map(|cache| format!(",\"proof_cache\":{{\"schema\":\"witchy.proof-cache.v1\",\"checker_binding_sha256\":\"{}\",\"hits\":{},\"misses\":{fresh},\"trust\":\"cache_directory_integrity\"}}", cache.binding(), cached.iter().flatten().count())).unwrap_or_default();
    // Fresh lines take the places the cache left open, so the inventory keeps source order.
    let merged = |fresh: Vec<String>| -> String {
        if cached.is_empty() { return fresh.join(","); }
        let mut fresh = fresh.into_iter();
        cached.iter().map(|line| line.clone().unwrap_or_else(|| fresh.next().expect("one fresh line per miss"))).collect::<Vec<_>>().join(",")
    };
    let refuse_known = |refusal: &mut Refusal, known: Vec<Failure>| -> Result<Checked> {
        refusal.failures.extend(known);
        Err(error("native diagnostic protocol or proof refusal"))
    };
    if goals.is_empty() {
        if !known.is_empty() { return refuse_known(refusal, known); }
        measurements.module_build_ns = phase.elapsed().as_nanos();
        return Ok(Checked { inventory: merged(Vec::new()), fresh: cache_note(0), invocations: 0, measurements });
    }
    // Only source-independent default proof scripts can be shared. Each source
    // obligation still names its own caller, statement and node count in the
    // receipt, pointing to the exact theorem freshly audited in this build.
    let original = goals;
    let mut unique = BTreeMap::new();
    let mut goals = Vec::new();
    let mut theorem_for = Vec::with_capacity(original.len());
    for goal in &original {
        // Function bodies remain distinct: the retained-body planner uses their
        // source identities independently of propositional equality.
        if goal.prover.is_none() && goal.source.as_ref().is_some_and(|s| s.kind == "call_precondition") {
            // A shared theorem can populate one evidence key. Across library
            // scopes each key must receive its own audited result.
            let scope = cache.as_ref().and_then(|cache| cache.library_scope(goal));
            let key = (goal.statement.as_str(), scope);
            if let Some(&theorem) = unique.get(&key) {
                theorem_for.push(theorem);
                continue;
            }
            unique.insert(key, goals.len());
        }
        theorem_for.push(goals.len());
        goals.push(goal.clone());
    }
    let batches = batches(&goals)?;
    let mut application = None;
    if let Some(session) = session.as_deref_mut().filter(|s|s.body_enabled() && cache.is_none()) {
        let context = format!("{model_id}:{imports_id}:{audit_id}");
        if let Some(mut plan) = if batches.len() == 1 { super::proof_body::plan_current(&original, &sites, &context)? } else { None } {
            plan.index = theorem_for[plan.index];
            if session.installed_body().is_none() {
                let marker = format!("WITCHY_BODY_INSTALL_V2:{}:1",plan.key.as_str());
                let install = super::proof_module::emit(super::proof_module::Namespace::Body(&plan.key), &[&goals[plan.index]], None)?.audited(&marker)?;
                session.queue_body(plan.key.as_str(), install, marker)?;
                application = Some(plan);
            } else if session.installed_body() == Some(plan.key.as_str()) { application = Some(plan); }
        }
        session.select_body(application.as_ref().map(|p|p.key.as_str()))?;
    }
    let mut inventory = Vec::new();
    for (origin, goal) in original.iter().enumerate() {
        let index = theorem_for[origin];
        let batch = batches.iter().position(|range| range.contains(&index)).expect("every goal is batched");
        let goal_id = io(digest::bytes(goal.statement.as_bytes()))?;
        let identity=goal.source.as_ref().map(|identity|{
            let call=identity.callee.as_ref().map(|callee|format!(",\"callee\":\"{callee}\",\"site\":{}",identity.site.expect("call site"))).unwrap_or_default();
            let model=if identity.storage||identity.record||identity.memory{format!(",\"source_model\":\"{}\"{}",if identity.memory{super::memory_model::MODEL}else if identity.record{super::record_model::MODEL}else{super::storage_model::MODEL},if identity.kind=="function_body"{if goal.statement.starts_with("∀ (α"){",\"generic_parameters\":1"}else{",\"generic_parameters\":0"}}else{""})}else{String::new()};
            format!(",\"goal_kind\":\"{}\",\"function\":\"{}\"{call}{model}",identity.kind,identity.function)
        }).unwrap_or_default();
        let outcome = if application.as_ref().is_some_and(|p|p.index==index) { "kernel_typed_body_application" } else { "fresh_kernel_checked" };
        inventory.push(format!("{{\"declaration\":\"{}\",\"theorem\":\"WitchyScalar.goal_{}\"{},\"expected_statement\":\"{}\",\"statement_sha256\":\"{goal_id}\",\"checked_nodes\":{},\"outcome\":\"{outcome}\"{identity}}}", goal.source_name, index - batches[batch].start, if batches.len() > 1 { format!(",\"request\":{batch}") } else { String::new() }, goal.statement, goal.nodes));
    }
    let marker_prefix = if session.as_deref().is_some_and(|s|s.body_enabled()) { "WITCHY_BODY_CHECK_V2" } else { "WITCHY_SCALAR_FRESH_V1" };
    let mut requests = Vec::new();
    for range in &batches {
        let references = goals[range.clone()].iter().collect::<Vec<_>>();
        let applied = application.as_ref().filter(|p| range.contains(&p.index)).map(|p| (p.index - range.start, &p.key));
        let emitted = super::proof_module::emit(super::proof_module::Namespace::Current, &references, applied)?;
        let request_id = io(digest::bytes(format!("{source_id}:{model_id}:{audit_id}:{}", emitted.text).as_bytes()))?;
        let marker = format!("{marker_prefix}:{request_id}:{}", references.len());
        let module = emitted.audited(&marker)?;
        if module.len() > REQUEST_BYTES { return Err(error("one proof obligation exceeds the 64 KiB proof request size")); }
        requests.push((module, marker, references.len()));
    }
    let module_id = if let [(module, _, _)] = requests.as_slice() { io(digest::bytes(module.as_bytes()))? } else {
        let mut ids = Vec::new();
        for (module, _, _) in &requests { ids.push(io(digest::bytes(module.as_bytes()))?); }
        io(digest::bytes(ids.join(":").as_bytes()))?
    };
    measurements.generated_module_bytes = requests.iter().map(|(module, _, _)| module.len()).sum();
    measurements.module_build_ns = phase.elapsed().as_nanos();
    let phase = Instant::now();
    let compiler_path = io(std::env::current_exe())?;
    let compiler_id = io(digest::file(&compiler_path))?;
    measurements.compiler_hash_a_ns = phase.elapsed().as_nanos();
    measurements.compiler_hashes += 1;
    let invocations = requests.len();
    let merge = |all: &mut Vec<String>, request: usize, dependencies: String| {
        let inner = dependencies.trim_start_matches('[').trim_end_matches(']');
        if inner.is_empty() { return; }
        all.push(if invocations > 1 { inner.replace("{\"theorem\":", &format!("{{\"request\":{request},\"theorem\":")) } else { inner.to_owned() });
    };
    let mut all = Vec::new();
    let (installation_id, dependencies, native_id) = if let Some(session) = session {
        let context = format!("{compiler_id}:{imports_id}:{audit_id}");
        let (mut sdk, mut helper) = (String::new(), String::new());
        if application.is_none() { session.queue_independent(&requests); }
        for (request, (module, marker, count)) in requests.iter().enumerate() {
            // A helper serves a bounded number of requests; a healthy exhausted one is replaced.
            if request > 0 && !session.available() { session.renew_within_build()?; }
            let output;
            let began = Instant::now();
            (sdk, output, helper) = match session.run(module, marker, &context, &mut measurements) {
                Ok(reply) => reply,
                Err(error) => {
                    let before = refusal.failures.len();
                    note_failure(&mut refusal.failures, module, &goals[batches[request].clone()], cache.as_ref(), &original, &theorem_for, batches[request].start);
                    let seconds = began.elapsed().as_secs();
                    for failure in &mut refusal.failures[before..] { failure.seconds = Some(seconds); }
                    if refusal.failures.len() == before && DEADLINES.contains(&&*error.message) {
                        refusal.stalled = goals[batches[request].clone()].to_vec();
                        refusal.seconds = Some(seconds);
                    }
                    refusal.failures.extend(known);
                    return Err(error);
                }
            };
            match validate_output(true, &output, "", marker).and_then(|()| audit_dependencies(&output, *count)) {
                Ok(dependencies) => {
                    if let Some(cache) = &cache { cache.store(&goals[batches[request].clone()], &dependencies)?; }
                    merge(&mut all, request, dependencies)
                }
                Err(error) => { session.refuse(); return Err(error); }
            }
        }
        if !known.is_empty() { return refuse_known(refusal, known); }
        (sdk, format!("[{}]", all.join(",")), Some((helper, session.diagnostic_identity(), session.trust_fields(), if session.body_enabled() { session.mode() } else { "UNSEALED_DIAGNOSTIC" })))
    } else {
        let installation = standalone::Sdk::snapshot(&mut measurements)?;
        for (request, (module, marker, count)) in requests.iter().enumerate() {
            let (began, before) = (Instant::now(), refusal.failures.len());
            let dependencies = installation.run_batch(module, marker, *count, &mut measurements, |output| {
                // Standalone Lean reports rows of the complete file. Only a failure
                // inside an actual theorem identifies a source obligation.
                for (index, message) in failing(module, output, 0) {
                    if let Some(goal)=goals[batches[request].clone()].get(index) {
                        refusal.failures.push(failure(goal, message));
                    }
                }
            });
            let dependencies = match dependencies {
                Ok(dependencies) => dependencies,
                Err(error) => {
                    let seconds = began.elapsed().as_secs();
                    for failure in &mut refusal.failures[before..] { failure.seconds = Some(seconds); }
                    if refusal.failures.len() == before && DEADLINES.contains(&&*error.message) {
                        refusal.stalled = goals[batches[request].clone()].to_vec();
                        refusal.seconds = Some(seconds);
                    }
                    if !refusal.failures.is_empty() || !refusal.stalled.is_empty() {
                        refusal.sdk = Some(installation);
                    }
                    return Err(error);
                }
            };
            merge(&mut all, request, dependencies);
        }
        installation.confirm(&mut measurements)?;
        (installation.into_identity(), format!("[{}]", all.join(",")), None)
    };
    let phase = Instant::now();
    let compiler_after = io(digest::file(&compiler_path))?;
    measurements.compiler_hash_b_ns = phase.elapsed().as_nanos();
    measurements.compiler_hashes += 1;
    if compiler_after != compiler_id { return Err(error("compiler changed during fresh proof checking")); }
    if let Some((helper_id, (diagnostic_key, diagnostic_schema), trust, mode)) = native_id {
        let reused = application.is_some();
        return Ok(Checked { inventory: merged(inventory), invocations, measurements,
            fresh: format!(",\"{diagnostic_key}\":{{\"schema\":\"{diagnostic_schema}\",\"mode\":\"{mode}\"{trust},\"child_measurement_semantics\":\"request_round_trip_not_process_lifetime\",\"source_sha256\":\"{source_id}\",\"compiler_sha256\":\"{compiler_id}\",\"helper_sha256\":\"{helper_id}\",\"lean_version\":\"4.36.0\",\"model_sha256\":\"{model_id}\",\"audit_sha256\":\"{audit_id}\",\"generated_module_sha256\":\"{module_id}\",\"lean_installation_sha256\":\"{installation_id}\",\"audited_dependencies\":{dependencies},\"admission_reuse\":{reused}}}{}", cache_note(original.len())) });
    }
    Ok(Checked {
        inventory: merged(inventory), invocations, measurements,
        fresh: format!(",\"fresh_kernel_checks\":{{\"schema\":\"witchy.scalar.fresh.v1\",\"source_sha256\":\"{source_id}\",\"compiler_sha256\":\"{compiler_id}\",\"model_sha256\":\"{model_id}\",\"audit_sha256\":\"{audit_id}\",\"generated_module_sha256\":\"{module_id}\",\"lean_installation_sha256\":\"{installation_id}\",\"lean_version\":\"4.36.0\",\"audited_dependencies\":{dependencies},\"axiom_allowlist\":[\"propext\",\"Classical.choice\",\"Quot.sound\"],\"admission_reuse\":false}}{}", cache_note(original.len())),
    })
}

fn validate_output(success: bool, output: &str, errors: &str, marker: &str) -> Result<()> {
    if !success || !errors.is_empty() || output.lines().filter(|line| *line == marker || line.ends_with(&format!(": info: {marker}"))).count() != 1 {
        let first_error = output.lines().chain(errors.lines()).position(|line| line.contains(": error:") || line.starts_with("error:")).unwrap_or(0);
        let details = output.lines().chain(errors.lines()).skip(first_error).take(8).collect::<Vec<_>>().join("\n")
            .chars().take(2048).collect::<String>();
        super::compiler_diagnostic!("{details}");
        return Err(error("fresh Lean scalar proof or expected-theorem/axiom audit failed"));
    }
    Ok(())
}
pub(super) fn validate_body_install(output: &str, marker: &str, key: &str) -> Result<()> {
    let key = super::proof_body::BodyKey::parse(key)?;
    validate_output(true, output, "", marker)?;
    audit_dependencies_scoped(output, 1, &super::proof_module::Namespace::Body(&key).name())?;
    Ok(())
}
fn audit_dependencies(output: &str, goal_count: usize) -> Result<String> { audit_dependencies_scoped(output,goal_count,"WitchyScalar") }
fn audit_dependencies_scoped(output: &str, goal_count: usize, namespace: &str) -> Result<String> {
    let lines = output.lines().map(|line| line.rsplit_once(": info: ").map_or(line, |(_, message)| message)).collect::<Vec<_>>();
    let mut dependencies = Vec::new();
    for index in 0..goal_count {
        let prefix = format!("WITCHY_SCALAR_AXIOMS:{namespace}.goal_{index}:");
        let records = lines.iter().filter_map(|line| line.strip_prefix(&prefix)).collect::<Vec<_>>();
        if records.len() != 1 || records[0].len() != 3 || !records[0].bytes().all(|b| b == b'0' || b == b'1') {
            return Err(error("missing or malformed per-theorem axiom audit"));
        }
        let actual = ["propext", "Classical.choice", "Quot.sound"].into_iter().zip(records[0].bytes())
            .filter(|(_, flag)| *flag == b'1').map(|(name, _)| format!("\"{name}\""))
            .collect::<Vec<_>>().join(",");
        dependencies.push(format!("{{\"theorem\":\"{namespace}.goal_{index}\",\"axioms\":[{actual}]}}"));
    }
    let dependencies = format!("[{}]", dependencies.join(","));
    Ok(dependencies)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prepare(source: &str) -> Result<Vec<Goal>> { goals(&crate::compiler::parse(source).unwrap()) }
    #[test]
    fn resolves_all_widths_and_total_ghost_arithmetic() {
        let prepared = prepare("lemma narrow(x: U8):\n    ensures x + 1 > x\n    proof:\n        auto\nlemma wide(x: U64):\n    ensures x - x == 0\n    proof:\n        auto\nfn main():\n    return\n").unwrap();
        assert_eq!(prepared.len(), 2);
        assert!(prepared[0].statement.contains("p0 ≤ 255"));
        assert!(prepared[1].statement.contains("p0 ≤ 18446744073709551615"));
        assert!(prepared[1].statement.contains("p0 - p0"));
    }
    #[test]
    fn rejects_duplicate_scope_wrong_width_and_unsupported_proofs() {
        for lemma in [
            "lemma same(x: U8, x: U8):\n    ensures true\n    proof:\n        auto\n",
            "lemma same(x: U8, y: U64):\n    ensures x == y\n    proof:\n        auto\n",
            "lemma same(x: U8):\n    ensures missing == x\n    proof:\n        auto\n",
            "lemma same():\n    ensures true\n    proof:\n        have true\n        auto\n",
        ] { assert!(prepare(&format!("{lemma}fn main():\n    return\n")).is_err()); }
    }
    #[test]
    fn false_unused_is_inventoried_never_filtered() {
        let prepared = prepare("lemma unused():\n    ensures false\n    proof:\n        auto\nfn main():\n    return\n").unwrap();
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].statement, "False");
    }
    #[test]
    fn oversized_proof_sets_split_into_capped_contiguous_requests() {
        let goal = |bytes: usize| Goal { source_name: "f".into(), statement: format!("True{}", " ∧ True".repeat(bytes / 9)), nodes: 1, source: None, prover: None };
        // Everything that fits stays one request, exactly as before splitting existed.
        let small = [goal(100), goal(100), goal(100)];
        assert_eq!(batches(&small).unwrap(), vec![0..3]);
        assert_eq!(batches(&[]).unwrap(), vec![0..0]);
        let large = [goal(25_000), goal(25_000), goal(25_000), goal(100), goal(40_000)];
        let split = batches(&large).unwrap();
        assert!(split.len() >= 3);
        assert_eq!((split.first().unwrap().start, split.last().unwrap().end), (0, large.len()));
        for pair in split.windows(2) { assert_eq!(pair[0].end, pair[1].start); }
        for range in split {
            let references = large[range].iter().collect::<Vec<_>>();
            let module = super::super::proof_module::emit(super::super::proof_module::Namespace::Current, &references, None).unwrap().audited(&"M".repeat(112)).unwrap();
            assert!(module.len() <= REQUEST_BYTES);
        }
    }
    #[test]
    fn split_sequents_keep_independent_theorems_and_bounded_groups() {
        let mut goal = Goal { source_name: "f".into(), statement: "True".into(), nodes: 1,
            source: Some(SourceIdentity { kind: "function_body_sequent", function: "f".into(),
                callee: None, site: None, storage: false, record: false, memory: false }), prover: None };
        let mut goals = vec![goal.clone(); SEQUENTS_PER_REQUEST + 1];
        goal.source = None;
        goals.push(goal);
        assert_eq!(batches(&goals).unwrap(),
            vec![0..SEQUENTS_PER_REQUEST, SEQUENTS_PER_REQUEST..goals.len()]);
    }
    #[test]
    fn proof_search_is_bounded_per_request_as_well_as_bytes() {
        let goal = |closers: usize| Goal { source_name: "f".into(), statement: "False".into(), nodes: 1, source: None,
            prover: Some("  · first | witchy_omega | witchy_close\n".repeat(closers / 2)) };
        // Small enough in bytes for one request, but more search than one request may carry.
        let goals = (0..12).map(|i| goal(40 + 8 * i)).chain([goal(2 * SEARCHES_PER_REQUEST)]).collect::<Vec<_>>();
        let split = batches(&goals).unwrap();
        assert!(split.len() > 1);
        assert_eq!(split.first().unwrap().start, 0);
        assert_eq!(split.last().unwrap().end, goals.len());
        for pair in split.windows(2) { assert_eq!(pair[0].end, pair[1].start); }
        for range in split {
            // A goal heavier than the bound goes alone; no request otherwise exceeds it.
            let search = goals[range.clone()].iter().map(searches).sum::<usize>();
            assert!(range.len() == 1 || search <= SEARCHES_PER_REQUEST, "{range:?} carries {search}");
        }
        let light = (0..4).map(|_| goal(8)).collect::<Vec<_>>();
        assert_eq!(batches(&light).unwrap(), vec![0..light.len()]);
        // Little search but many bytes: the byte bound decides the cut.
        let wordy = |bytes: usize| Goal { source_name: "f".into(), statement: "True".into(), nodes: 1, source: None,
            prover: Some("  exact True.intro\n".repeat(bytes / 19)) };
        let wordy = [wordy(25_000), wordy(25_000), wordy(25_000)];
        assert!(wordy.iter().map(searches).sum::<usize>() <= SEARCHES_PER_REQUEST);
        let split = batches(&wordy).unwrap();
        assert!(split.len() > 1);
        assert_eq!((split.first().unwrap().start, split.last().unwrap().end), (0, wordy.len()));
        for range in split {
            let references = wordy[range].iter().collect::<Vec<_>>();
            let module = super::super::proof_module::emit(super::super::proof_module::Namespace::Current, &references, None).unwrap().audited(&"M".repeat(112)).unwrap();
            assert!(module.len() <= REQUEST_BYTES);
        }
    }
    #[test]
    fn lean_errors_map_to_their_theorem_and_audit_errors_to_none() {
        let goal = |name: &str| Goal { source_name: name.into(), statement: "True".into(), nodes: 1, source: None, prover: None };
        let goals = [goal("first"), goal("second")];
        let module = super::super::proof_module::emit(super::super::proof_module::Namespace::Current, &goals.iter().collect::<Vec<_>>(), None).unwrap().audited("TEST:2").unwrap();
        let row = |text: &str| module.lines().position(|line| line.starts_with(text)).unwrap() + 1 - IMPORTS.lines().count();
        let output = format!("x.lean:{}:4: error: unsolved goals\nx.lean:{}:2: error: later duplicate\nPREFIX:x.lean:{}:0: error: WASM_PROOF: forbidden axiom sorryAx\nx.lean:3:1: warning: unused\n",
            row("theorem goal_1") + 2, row("theorem goal_1") + 3, row("end WitchyScalar") + 2);
        assert_eq!(failing(&module, &output, IMPORTS.lines().count()), vec![(1, "unsolved goals".to_owned())]);
        let expected_row = row("def expected_1") + IMPORTS.lines().count();
        let theorem_row = row("theorem goal_1") + IMPORTS.lines().count();
        let standalone = format!("x.lean:{expected_row}:5: error: bad statement\nx.lean:{theorem_row}:1: error: unsolved goals");
        assert_eq!(failing(&module, &standalone, 0), vec![(1, "unsolved goals".to_owned())]);
        assert!(failing(&module, "", 0).is_empty());
    }
    #[test]
    fn ordinary_program_has_no_proof_inventory() {
        assert!(prepare("fn main():\n    return\n").unwrap().is_empty());
    }
    /// Root must explicitly grant real Lean/host execution and set the trusted
    /// installation. Ordinary focused checks never run this process control.
    #[test]
    #[ignore = "requires explicit trusted Lean installation and host reservation"]
    fn real_audit_rejects_missing_unrelated_sorry_and_new_axiom() {
        let marker = "WITCHY_SCALAR_NEGATIVE_AUDIT_CONTROL";
        let make_module = |declarations: &str| format!("{IMPORTS}namespace WitchyScalar\n{declarations}end WitchyScalar\n{}",
            AUDIT.replace("__GOALS__", "(`WitchyScalar.goal_0, `WitchyScalar.expected_0)").replace("__MARKER__", marker));
        let cases = [
            ("def expected_0 : Prop := True\ntheorem goal_0 : expected_0 := True.intro\n", None),
            ("def expected_0 : Prop := True\n", Some("missing expected theorem")),
            ("theorem goal_0 : True := True.intro\n", Some("missing expected proposition")),
            ("def expected_0 : Prop := True\ntheorem goal_0 : False → False := by intro h; exact h\n", Some("theorem statement mismatch")),
            ("def expected_0 : Prop := True\ntheorem goal_0 : expected_0 := by sorry\n", Some("forbidden axiom sorryAx")),
            ("def expected_0 : Prop := True\naxiom bad : expected_0\ntheorem goal_0 : expected_0 := bad\n", Some("forbidden axiom WitchyScalar.bad")),
        ];
        // The controls exercise audit outcomes inside one SDK identity window.
        // Each has a fresh checker, output limit, and deadline.
        let mut measurements = super::super::proof_measure::Measurements::default();
        let sdk = standalone::Sdk::snapshot(&mut measurements).unwrap();
        let sdk = &sdk;
        let make_module = &make_module;
        std::thread::scope(|scope| {
            for (declarations, expected) in cases {
                scope.spawn(move || {
                    let mut details = String::new();
                    let result = sdk.check(
                        &make_module(declarations),
                        marker,
                        &mut super::super::proof_measure::Measurements::default(),
                        |output| details.push_str(output),
                    );
                    if let Some(expected) = expected {
                        assert!(result.is_err() && details.contains(&format!("WASM_PROOF: {expected}")),
                            "expected {expected}, got {result:?}: {details}");
                    } else {
                        let output = result.unwrap_or_else(|error| panic!("expected a valid audit, got {error:?}: {details}"));
                        assert_eq!(audit_dependencies(&output.text, 1).unwrap(),
                            r#"[{"theorem":"WitchyScalar.goal_0","axioms":[]}]"#);
                    }
                });
            }
        });
        sdk.confirm(&mut measurements).unwrap();
    }
}

/// Every place a proof refusal is made says where, what was not proved, and what to do next.
#[cfg(test)]
mod refusal_sites {
    use super::*;
    /// The kinds named where refusals are made (each `Unproved::` variant in the source of the
    /// explanation), each found by reading the source.
    fn named_kinds(text: &str) -> BTreeSet<String> {
        text.match_indices("Unproved::").map(|(at, _)| text[at + "Unproved::".len()..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>())
            .filter(|kind| kind.starts_with(|c: char| c.is_ascii_uppercase()) && kind != "ALL").collect()
    }
    /// The text between the refusal's opening and its next step: what was not proved, and the step.
    fn said(message: &str) -> (&str, &str) {
        let rest = message.strip_prefix("this function's proof did not go through: ").expect("a refusal's opening");
        rest.rsplit_once("; next: ").expect("a next step")
    }
    #[test]
    fn every_refusal_site_has_a_place_and_a_next_step() {
        // Every kind the explanation names is one this test covers, and every refusal is made by
        // `refused`: no other text of the explanation opens a W0402.
        let sources = [include_str!("proof.rs"), include_str!("proof_advice.rs")];
        let covered: BTreeSet<String> = Unproved::ALL.iter().map(|k| format!("{k:?}")).collect();
        let explanation = &sources[0][..sources[0].find("mod refusal_sites").unwrap()];
        for text in [explanation, sources[1]] {
            for kind in named_kinds(text) { assert!(covered.contains(&kind), "`Unproved::{kind}` is made but not covered here"); }
        }
        assert_eq!(explanation.matches("this function's proof did not go through").count(), 0, "a W0402 refusal is made only by `proof_advice::refused`");
        let sites = explanation.matches("advice::refused(").count();
        assert!(sites >= 15, "the refusal sites were found by reading proof.rs ({sites})");
        // Each kind's refusal: its place as given, what was not proved, and one next step.
        for kind in Unproved::ALL {
            let refusal = advice::refused(kind, 5..9, "the clause `x` (at 3) is not shown".into());
            assert_eq!(refusal.at, 5..9);
            let (what, next) = said(&refusal.message);
            assert!(!what.is_empty() && next.len() > 20 && next == kind.next(), "{kind:?}: {}", refusal.message);
        }
        // Each way `point` places a part: a place in the source, something said, a next step.
        let source = "sealed type Counter:\n    count: U64\n    limit: U64\n    invariant count <= limit\n\nfn f(c: &mut Counter, n: U64) -> U64:\n    requires n < 10\n    ensures result == n\n    proof:\n        auto\n    var i = 0\n    while i < n:\n        i = i + 1\n    let t = n * 2\n    if n == 3:\n        unreachable\n    return i\n\nfn main():\n    return\n";
        let program = crate::compiler::parse(source).unwrap();
        let span = |text: &str| { let at = source.find(text).unwrap(); at..at + text.len() };
        let returned = Some(span("i\n\nfn main").start..span("i\n\nfn main").start + 1);
        let looped = |invariants| vec![super::super::scalar_vc::NotedLoop { inside: false, invariants, at: span("i < n"), names: vec!["i".into()] }];
        let part = |what: &str, at: Option<std::ops::Range<usize>>, kind| Part { what: what.into(), at, returned: returned.clone(), loops: Vec::new(), kind, said: None, tentative: false };
        let cases = vec![
            part("ensures result == n", Some(span("result == n")), Unproved::Ensures),
            Part { returned: None, ..part("ensures result == n", Some(span("result == n")), Unproved::Ensures) },
            part(super::super::scalar_vc::ENSURES, Some(span("count <= limit")), Unproved::Invariant),
            Part { returned: None, ..part(super::super::scalar_vc::ENSURES, Some(span("count <= limit")), Unproved::Invariant) },
            part(super::super::scalar_vc::UNREACHED, Some(span("unreachable")), Unproved::Unreachable),
            part(super::super::scalar_vc::OVERWRITE, Some(span("t = n * 2")), Unproved::Overwrite),
            part(super::super::scalar_vc::DISCARD, Some(span("t = n * 2")), Unproved::Discard),
            part("a `*` that may overflow, in what is assigned to `t`", Some(span("n * 2")), Unproved::Check),
            part("a `-` that may go below zero, in the value returned", None, Unproved::Check),
            Part { loops: looped(false), ..part("a loop invariant after a pass of the loop", None, Unproved::LoopInvariant) },
            Part { loops: looped(false), ..part("a `+` that may overflow, in what is assigned to `i`", Some(span("i + 1")), Unproved::Check) },
            Part { loops: looped(true), ..part("a `+` that may overflow, in what is assigned to `i`", Some(span("i + 1")), Unproved::Check) },
            part("the `requires` of this call of `g`", Some(span("n * 2")), Unproved::CallRequires).saying("this call of `g` may not establish its `requires n < 3`".into()),
            part("a fact the record proof model cannot follow past this call", Some(span("n * 2")), Unproved::Modelled).saying("`g(n)` calls `g`, which the record proof model does not read".into()),
        ];
        let mut kinds = BTreeSet::new();
        for case in cases {
            let refusal = point(&program, source, "f", std::slice::from_ref(&case)).unwrap_or_else(|| panic!("no place for {}", case.what));
            assert!(refusal.at != (0..0) && refusal.at.end <= source.len() && !source[refusal.at.clone()].trim().is_empty(), "{:?}: {:?}", case.kind, refusal.at);
            let (what, next) = said(&refusal.message);
            assert!(!what.is_empty() && !next.is_empty(), "{}", refusal.message);
            kinds.insert(format!("{:?}", refusal.kind));
        }
        // The loop cases are said at the loop.
        assert!(kinds.contains("LoopNeedsInvariant") && kinds.contains("LoopSaysTooLittle"), "{kinds:?}");
        // An `unreachable` names the condition the proof could not rule out.
        let unreachable = point(&program, source, "f", &[part(super::super::scalar_vc::UNREACHED, Some(span("unreachable")), Unproved::Unreachable)]).unwrap();
        assert!(unreachable.message.contains("`n == 3`"), "{}", unreachable.message);
        // A loop with no invariant names what the failing part reads that the loop changes.
        let loop_refusal = point(&program, source, "f", &[Part { loops: looped(false), ..part("a `+` that may overflow, in what is assigned to `i`", Some(span("i + 1")), Unproved::Check) }]).unwrap();
        assert!(loop_refusal.at == span("i < n") && loop_refusal.message.contains("`i`"), "{}", loop_refusal.message);
    }
    #[test]
    fn every_contract_refusal_says_a_next_step() {
        // Every refusal of a contract's vocabulary written in the compiler, found by reading its
        // sources, is given a next step when it is made.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/compiler");
        let mut files = vec![root.clone()];
        let mut messages = BTreeSet::new();
        while let Some(path) = files.pop() {
            if path.is_dir() { files.extend(fs::read_dir(&path).unwrap().map(|e| e.unwrap().path())); continue; }
            if path.extension().is_none_or(|e| e != "rs") { continue; }
            let text = fs::read_to_string(&path).unwrap();
            for family in ["\"function contracts", "\"record contracts", "\"memory contracts", "\"storage contracts"] {
                for (at, _) in text.match_indices(family) {
                    let literal: String = text[at + 1..].chars().take_while(|c| *c != '"').collect();
                    messages.insert(literal);
                }
            }
        }
        assert!(messages.len() > 100, "the contract refusals were found by reading the compiler ({})", messages.len());
        for message in &messages {
            let made = crate::diagnostic::Diagnostic::new("W0401", 0..0, message.clone());
            let next = made.message.rsplit_once("; next: ").map(|(_, next)| next).unwrap_or("");
            assert!(next.len() > 20 || (advice::says_next(message) && made.message == *message), "no next step for `{message}`: {}", made.message);
        }
    }
    #[test]
    fn a_contract_refusal_with_no_place_takes_its_clause_or_function() {
        // `old(...)` of a parameter taken by value: refused while showing the `ensures` at a
        // return, which is placed at that clause.
        let source = "type P:\n    n: U64\nimpl P:\n    fn n(p: &P) -> U64:\n        return (*p).n\nfn f(p: P) -> U64:\n    ensures result == p.n()\n    proof:\n        auto\n    return p.n\nfn main():\n    return\n";
        let syntax = crate::compiler::parse(source).unwrap();
        let refusal = super::super::check::program(&syntax).and_then(|logical| super::super::source_vc::prepare(&logical).map(|_| ())).unwrap_err();
        assert!(refusal.message.starts_with("function contracts") && refusal.message.contains("; next: "), "{}", refusal.message);
        assert!(refusal.span != (0..0) && source[refusal.span.clone()].contains("p.n()"), "{refusal:?}");
    }
}
