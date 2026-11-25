//! Compiler modular-VC rule for sealed, same-allocation owned returns.
//! This descriptor is derived from actual typed execution, never a Same clause.
use super::{logical_body::*, record_contract::{Predicate, Term, Root, RecordContract}, record_value::Symbol,
    storage_contract::error, proof::Goal};
use crate::diagnostic::Diagnostic;
use std::collections::{BTreeMap, BTreeSet};
type Result<T> = std::result::Result<T, Diagnostic>;
pub(super) const MODEL: &str = "witchy.checked-owned-record-post.v2";
pub(super) const MARKER: &str = "-- owned-record-post-dependencies-validated-v1";

#[derive(Clone, Default)]
pub(super) struct OwnedPost { pub arms: BTreeMap<u8, Vec<usize>> }
/// Every allocation a value owns, with the field path to each.
pub(super) fn owners(value: &Symbol) -> Vec<(u32, Vec<usize>)> {
    fn collect(value: &Symbol, path: &mut Vec<usize>, out: &mut Vec<(u32, Vec<usize>)>) {
        match value {
            Symbol::Buffer(id) => out.push((*id, path.clone())),
            Symbol::Record(fields) => for (i, value) in fields.iter().enumerate() {
                path.push(i); collect(value, path, out); path.pop();
            },
            _ => {},
        }
    }
    let mut out = vec![];
    collect(value, &mut vec![], &mut out);
    out
}
/// The one allocation a value owns, where a rule follows exactly one.
pub(super) fn owner(value: &Symbol) -> Result<(u32, Vec<usize>)> {
    let mut out = owners(value);
    if out.len() != 1 { return Err(error("owned post requires exactly one actual allocation owner")); }
    Ok(out.remove(0))
}
impl OwnedPost {
    /// Newly reusable arms need an authored fact about their actual returned
    /// owner. An entry snapshot or another arm's Prefix cannot remove UNKNOWN.
    pub fn returned_prefix(&self, contract: &RecordContract, arm: u8) -> bool {
        let Some(fields) = self.arms.get(&arm) else { return false; };
        for (selected, clause) in &contract.ensures {
            if *selected != Some(arm) { continue; }
            let mut parts = vec![];
            projection(clause, &mut parts);
            if parts.iter().any(|p| matches!(p, Predicate::Prefix(path, _, _) if
                matches!(path.root, Root::Arm(a) if a == arm) && !path.before &&
                matches!(path.ty, Ty::Buffer(_)) && path.fields == *fields)) { return true; }
        }
        false
    }
    pub fn returned(&mut self, arm: u8, value: &Symbol, original: u32) -> Result<()> {
        let (id, path) = owner(value)?;
        if id != original { return Err(error("owned post return changes actual allocation identity")); }
        if self.arms.get(&arm).is_some_and(|previous| *previous != path) {
            return Err(error("owned post arm has inconsistent actual owner paths"));
        }
        self.arms.insert(arm, path);
        Ok(())
    }
}
fn number(t: &Term) -> bool { match t {
    Term::Literal(_) | Term::Capacity(_) => true,
    Term::Path(p) => p.ty == Ty::U64,
    Term::Bound => false,
    Term::Add(a,b) => number(a) && number(b),
}}
/// Drop only entire unsupported conjuncts; never weaken inside Not or Or.
pub(super) fn projection(p: &Predicate, out: &mut Vec<Predicate>) {
    match p {
        Predicate::Binary("∧", a, b) => { projection(a, out); projection(b, out); },
        Predicate::Prefix(_, n, c) if number(n) && number(c) => out.push(p.clone()),
        Predicate::Compare(_, a, b) if number(a) && number(b) => out.push(p.clone()),
        Predicate::Bool(_) => out.push(p.clone()),
        _ => {},
    }
}
/// The caller rule may assume a predecessor post only with its complete body and
/// actual precondition in this same batch. Every dependency is checked, including
/// those introduced on paths later joined at a primary typed site.
pub(super) fn dependencies(
    function: &Function, functions: &[Function], sites: &BTreeMap<u32, super::record_caller::Site>,
    summaries: &BTreeMap<u32, OwnedPost>, goals: &[&Goal],
) -> Result<()> {
    fn visit(id: u32, sites: &BTreeMap<u32, super::record_caller::Site>,
             gray: &mut BTreeSet<u32>, black: &mut BTreeSet<u32>) -> Result<()> {
        if black.contains(&id) { return Ok(()); }
        if !gray.insert(id) { return Err(error("owned post cyclic or self prerequisite")); }
        let site = sites.get(&id).ok_or_else(|| error("owned post missing prerequisite site"))?;
        for predecessor in &site.dependencies {
            let prior = sites.get(predecessor).ok_or_else(|| error("owned post missing predecessor"))?;
            if !prior.reached { return Err(error("owned post prerequisite was not reached")); }
            if prior.order.zip(site.order).is_none_or(|(a,b)| a >= b) {
                return Err(error("owned post future or self prerequisite"));
            }
            visit(*predecessor, sites, gray, black)?;
        }
        gray.remove(&id); black.insert(id); Ok(())
    }
    for (id, site) in sites {
        visit(*id, sites, &mut BTreeSet::new(), &mut BTreeSet::new())?;
        for predecessor in &site.dependencies {
            let prior = &sites[predecessor];
            if !summaries.contains_key(&prior.callee.0) || !goals.iter().any(|g|
                g.source.as_ref().is_some_and(|s| s.record && s.kind == "function_body" &&
                    s.function == functions[prior.callee.0 as usize].name)) {
                return Err(error("owned post prerequisite body absent from atomic batch"));
            }
            if !goals.iter().any(|g| g.source.as_ref().is_some_and(|s|
                s.record && s.kind == "call_precondition" && s.function == function.name &&
                s.site == Some(*predecessor) && s.callee.as_deref() == Some(functions[prior.callee.0 as usize].name.as_str()))) {
                return Err(error("owned post prerequisite precondition absent from atomic batch"));
            }
        }
    }
    Ok(())
}
