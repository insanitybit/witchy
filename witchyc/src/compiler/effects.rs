//! The `suspend` effect (docs/design-async.md): which functions may pause their task.
//!
//! Each function gets a summary: whether it may pause whatever its type arguments are, and
//! which methods of its own type parameters it calls that may (`(parameter, trait method)`).
//! A generic body is summarized once, against its bounds: `fn copy<R: Read>(r: &mut R)`
//! pauses exactly when `R`'s `read_some` does, and the summary says so without looking at any
//! type `copy` is later used with. A call substitutes the callee's summary; a call of a trait
//! method on a known type is a call of that type's method. Recursion is a fixed point, from
//! "pauses nowhere" up.
//!
//! Where a summary comes from: a function declared `-> T suspends` may pause. A trait method
//! declared `suspends` lets its implementations pause; one that is not refuses an
//! implementation that may. Checked here:
//! - a `pub` function that may pause whatever its type arguments says `suspends`;
//! - an implementation pauses only where its trait method allows;
//! - a type parameter bound `nosuspend` is given only a type whose methods never pause;
//! - no call in a `nosuspend:` block may pause.
//!
//! Nothing here changes code: a function that may pause runs to completion until the
//! state-machine lowering exists. The check is what makes that lowering safe to add.
use super::logical_body::*;
use crate::diagnostic::Diagnostic;
use std::collections::{BTreeMap, BTreeSet};

type Result<T> = std::result::Result<T, Diagnostic>;

/// What the check reads of each function, from its source.
#[derive(Clone, Debug, Default)]
pub(super) struct Facts {
    pub suspends: bool,
    pub public: bool,
    pub nosuspend: Vec<bool>,
    pub quiet: Vec<(usize, usize)>,
    pub sites: Vec<(CallEdge, (usize, usize))>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Summary {
    /// Pauses whatever the type arguments.
    always: bool,
    /// Pauses when this type parameter's method does: `(parameter index, trait method)`.
    via: BTreeSet<(u32, u32)>,
}
impl Summary {
    fn join(&mut self, other: Summary) { self.always |= other.always; self.via.extend(other.via); }
    fn pauses(&self) -> bool { self.always || !self.via.is_empty() }
}

struct Analysis<'a> {
    program: &'a mut Program,
    by_name: BTreeMap<String, u32>,
    summaries: Vec<Summary>,
    /// Whether `nosuspend` bounds drop what passes through them (the effect), or keep it (to
    /// check what a caller gives such a parameter).
    filter: bool,
}

fn refuse(at: (usize, usize), message: String) -> Diagnostic { Diagnostic::new("W0401", at.0..at.1, message) }
/// The owner name an impl's methods are filed under: the type's, or `#Trait` for a blanket
/// impl (whose recorded target is `#T`, its parameter; adversarial review round 24).
fn filed(trait_name: &str, target: &str) -> String { if target.starts_with('#') { format!("#{trait_name}") } else { target.to_owned() } }
fn shown(name: &str) -> String { name.split('@').next().unwrap_or(name).replace('#', "") }

impl Analysis<'_> {
    /// What calling trait method `m` on a value of type `this` may do, from function `f`.
    fn method(&mut self, f: u32, m: u32, this: Ty) -> Result<Summary> {
        if let Ty::Param { owner, index } = this {
            // One of `f`'s own parameters: its bound decides, and the trait method's permission.
            if owner.0 != f || !self.program.effects[m as usize].suspends { return Ok(Summary::default()); }
            if self.filter && self.program.effects[f as usize].nosuspend.get(index as usize).copied().unwrap_or(false) { return Ok(Summary::default()); }
            return Ok(Summary { always: false, via: BTreeSet::from([(index, m)]) });
        }
        let (implementation, substitutions) = self.implementation(m, this)?;
        self.apply(f, implementation, &substitutions)
    }
    /// The method a type provides for trait method `m`, as `instances.rs` finds it.
    fn implementation(&mut self, m: u32, this: Ty) -> Result<(u32, Vec<Ty>)> {
        let name = self.program.functions[m as usize].name.clone();
        let (trait_name, method) = name.split_once('.').expect("a trait method name");
        let own = self.program.types.nominal(this).and_then(|owner| self.by_name.get(&super::methods::name(owner, method))).copied();
        let blanket = self.by_name.get(&super::methods::name(&format!("#{trait_name}"), method)).copied();
        match (own, blanket) {
            (Some(own), _) => Ok((own, self.program.types.arguments(this))),
            (None, Some(blanket)) => Ok((blanket, vec![this])),
            (None, None) => Err(Diagnostic::new("W0401", 0..0, "a trait method was called on a type with no implementation")),
        }
    }
    /// Function `g` called from `f` with these type arguments, in terms of `f`'s parameters.
    fn apply(&mut self, f: u32, g: u32, substitutions: &[Ty]) -> Result<Summary> {
        let summary = self.summaries[g as usize].clone();
        let mut out = Summary { always: summary.always, via: BTreeSet::new() };
        for (index, m) in summary.via {
            let this = substitutions.get(index as usize).copied().ok_or_else(|| Diagnostic::new("W0401", 0..0, "a call gives a generic function too few type arguments"))?;
            out.join(self.method(f, m, this)?);
        }
        Ok(out)
    }
    fn call(&mut self, f: u32, edge: &CallEdge) -> Result<Summary> {
        let g = edge.definition.0;
        if self.program.functions[g as usize].required { return self.method(f, g, edge.substitutions[0]); }
        self.apply(f, g, &edge.substitutions)
    }
    /// Every function's summary, from "pauses nowhere" up to the least fixed point.
    fn solve(&mut self) -> Result<()> {
        let n = self.program.functions.len();
        self.summaries = vec![Summary::default(); n];
        loop {
            let mut changed = false;
            for f in 0..n {
                if self.program.functions[f].required { continue; }
                let mut summary = Summary { always: self.program.effects[f].suspends, via: BTreeSet::new() };
                let sites = self.program.effects[f].sites.clone();
                for (edge, _) in &sites { summary.join(self.call(f as u32, edge)?); }
                if summary != self.summaries[f] { self.summaries[f] = summary; changed = true; }
            }
            if !changed { return Ok(()); }
        }
    }
}

/// The effect check; it changes nothing.
pub(super) fn check(program: &mut Program) -> Result<()> {
    if program.effects.len() != program.functions.len() { return Err(Diagnostic::new("W0401", 0..0, "effect facts do not match the functions")); }
    let by_name = program.functions.iter().enumerate().map(|(i, f)| (f.name.clone(), i as u32)).collect();
    let mut analysis = Analysis { program, by_name, summaries: Vec::new(), filter: true };
    analysis.solve()?;
    let filtered = analysis.summaries.clone();
    // What passes through each `nosuspend` parameter, kept: what a caller must not give it.
    analysis.filter = false;
    analysis.solve()?;
    let raw = std::mem::replace(&mut analysis.summaries, filtered);
    analysis.filter = true;
    let n = analysis.program.functions.len();
    // Each trait's required methods, in function order: looked up per implementation below,
    // rather than every function scanned for each implementation (a profile's hot spot).
    let mut required: std::collections::BTreeMap<String, Vec<(usize, String)>> = std::collections::BTreeMap::new();
    for m in 0..n {
        let function = &analysis.program.functions[m];
        if let Some((owner, method)) = function.name.split_once('.') && function.required {
            required.entry(owner.to_string()).or_default().push((m, method.to_string()));
        }
    }
    let methods_of = |trait_name: &str| required.get(trait_name).map(Vec::as_slice).unwrap_or(&[]);
    // The implementations of trait methods: their trait's permission governs them, below.
    let mut implementing = BTreeSet::new();
    for (trait_name, target) in analysis.program.implements.clone() {
        for (_, method) in methods_of(&trait_name) {
            if let Some(&h) = analysis.by_name.get(&super::methods::name(&filed(&trait_name, &target), method)) { implementing.insert(h as usize); }
        }
    }
    for f in 0..n {
        if analysis.program.functions[f].required { continue; }
        let facts = analysis.program.effects[f].clone();
        for (edge, at) in &facts.sites {
            let effect = analysis.call(f as u32, edge)?;
            // Names as the source wrote them, made for a refusal only.
            let callee = |program: &Program| shown(&program.functions[edge.definition.0 as usize].name);
            // A `nosuspend:` block: nothing in it may pause.
            if effect.pauses() && facts.quiet.iter().any(|(start, end)| *start <= at.0 && at.1 <= *end) {
                let why = if effect.always { format!("`{}` may pause", callee(analysis.program)) } else { format!("`{}` may pause through a type parameter's method: bound it `nosuspend`", callee(analysis.program)) };
                return Err(refuse(*at, format!("a call inside `nosuspend:` that may pause its task: {why}")));
            }
            // A `pub` function says it may pause.
            if effect.always && facts.public && !facts.suspends && !implementing.contains(&f) {
                return Err(refuse(*at, format!("`{}` is public and may pause here (`{}` may): its signature says so, `-> T suspends`", shown(&analysis.program.functions[f].name), callee(analysis.program))));
            }
            // A `nosuspend` parameter of the callee gets a type whose methods never pause.
            let g = edge.definition.0 as usize;
            if analysis.program.functions[g].required { continue; }
            let quiet = analysis.program.effects[g].nosuspend.clone();
            for (index, is_quiet) in quiet.iter().enumerate() {
                if !is_quiet { continue; }
                let Some(&given) = edge.substitutions.get(index) else { continue };
                for (parameter, m) in raw[g].via.iter().filter(|(p, _)| *p as usize == index) {
                    let _ = parameter;
                    if analysis.method(f as u32, *m, given)?.pauses() {
                        let method = shown(&analysis.program.functions[*m as usize].name);
                        return Err(refuse(*at, format!("`{}` needs a type whose methods never pause (`nosuspend`), and this one's `{method}` may", callee(analysis.program))));
                    }
                }
            }
        }
    }
    // An implementation pauses only where its trait allows.
    let implements = analysis.program.implements.clone();
    for (trait_name, target) in &implements {
        for (m, method) in methods_of(trait_name) {
            let (m, method) = (*m, method.as_str());
            let method_name = format!("{trait_name}.{method}");
            if analysis.program.effects[m].suspends { continue; }
            let Some(&h) = analysis.by_name.get(&super::methods::name(&filed(trait_name, target), method)) else { continue };
            if analysis.summaries[h as usize].pauses() {
                let at = analysis.program.effects[h as usize].sites.iter().map(|(_, at)| *at).next().unwrap_or((0, 0));
                return Err(refuse(at, format!("`{}` may pause, but `{}` does not allow its implementations to: declare it `suspends` in the trait", shown(&format!("{target}.{method}")), shown(&method_name))));
            }
        }
    }
    // Last, so a more specific refusal comes first. `main` runs on no task, so nothing could resume it: pausing code runs in a task
    // (`Task.spawn`), and `step` is not a pause of whatever calls it.
    let main = analysis.program.main.0 as usize;
    for (edge, at) in &analysis.program.effects[main].sites.clone() {
        if analysis.call(main as u32, edge)?.pauses() {
            let callee = shown(&analysis.program.functions[edge.definition.0 as usize].name);
            return Err(refuse(*at, format!("`main` may pause here (`{callee}` may), but nothing would resume it: run pausing code in a task, `Task.spawn`")));
        }
    }
    Ok(())
}
