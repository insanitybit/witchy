//! Private interpreter evidence and one mathematical-VC lookup hint, never admission authority.
use super::{logical_body::{Function,FunctionId}, proof::Goal, proof_digest as digest};
use crate::diagnostic::Diagnostic;
type Result<T> = std::result::Result<T, Diagnostic>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BodyKey(String);
impl BodyKey {
    pub(super) fn parse(value: &str) -> Result<Self> {
        if value.len()!=64 || value.bytes().all(|b|b==b'0') || !value.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)) {
            return Err(Diagnostic::new("W0402",0..0,"invalid private body namespace key"));
        }
        Ok(Self(value.into()))
    }
    pub(super) fn as_str(&self) -> &str { &self.0 }
}
#[derive(Clone)]
pub(super) struct SiteEvidence {
    caller: FunctionId, callee: FunctionId, caller_name: String, callee_name: String,
    site: u32, reached: bool, generic_call_free: bool,
}
impl SiteEvidence {
    pub(super) fn interpreted(caller: &Function, site: u32, callee: &Function, reached: bool) -> Self {
        Self { caller:caller.definition, callee:callee.definition, caller_name:caller.name.clone(), callee_name:callee.name.clone(), site, reached,
            generic_call_free:callee.type_parameters.len()==1 && callee.calls.is_empty() }
    }
}
#[derive(Clone, Default)]
/// `proved_arithmetic`: definitions whose proof shows every unsigned `+ - *` in range (scalar_vc.rs).
pub(super) struct Prepared { pub goals: Vec<Goal>, pub sites: Vec<SiteEvidence>, pub proved_arithmetic: std::collections::BTreeSet<u32> }
impl Prepared { pub(super) fn goals_only(goals: Vec<Goal>) -> Self { Self { goals, sites:Vec::new(), proved_arithmetic: Default::default() } } }
pub(super) struct Plan { pub index: usize, pub key: BodyKey }
pub(super) fn plan_current(goals: &[Goal], sites: &[SiteEvidence], context: &str) -> Result<Option<Plan>> {
    let bodies = goals.iter().enumerate().filter(|(_,g)|g.source.as_ref().is_some_and(|s|s.kind=="function_body")).collect::<Vec<_>>();
    let [(index, goal)] = bodies.as_slice() else { return Ok(None); };
    let Some(source) = goal.source.as_ref().filter(|s|s.record && !s.storage) else { return Ok(None); };
    let reached = sites.iter().any(|e|e.reached && e.generic_call_free && e.caller!=e.callee && e.callee_name==source.function && goals.iter().any(|g|
        g.source.as_ref().is_some_and(|s|s.record && s.kind=="call_precondition" && s.function==e.caller_name && s.callee.as_deref()==Some(e.callee_name.as_str()) && s.site==Some(e.site))));
    if !reached { return Ok(None); }
    let mut binding = b"witchy.body-interface.lookup.v1".to_vec();
    for value in [source.function.as_bytes(), goal.statement.as_bytes(), goal.prover.as_deref().unwrap_or("").as_bytes(), context.as_bytes()] {
        binding.extend_from_slice(&(value.len() as u64).to_le_bytes()); binding.extend_from_slice(value);
    }
    let hash = digest::bytes(&binding).map_err(|_|Diagnostic::new("W0402",0..0,"body lookup identity I/O refused"))?;
    Ok(Some(Plan { index:*index, key:BodyKey::parse(&hash)? }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn actual(dead: bool) -> Prepared {
        let library = &include_str!("../../library/wasm-vec.wy").replace("PrefixBuffer<","Buffer<").replace("try_alloc_prefix","try_alloc"); // checked cell operations are a Buffer's
        let declarations = &library[..library.find("impl<T: Any> Vec<T>:").unwrap()];
        let checked = include_str!("../../tests/fixtures/ready-vec.wy");
        let body = &checked[checked.find("fn vec_push_spare").unwrap()..checked.find("fn close<").unwrap()];
        let (signature, body) = body.split_once('\n').unwrap();
        let (_, body) = body.split_once("        auto\n").unwrap();
        let clauses = "    requires memory.prefix(input.data, input.length, input.capacity)\n    ensures match result:\n        Ok(out):\n            true\n        Err(back):\n            true\n    proof:\n        auto\n";
        let path = if dead { "    if false:\n        return vec_push_spare(move input, item)\n    return Err(VecPushError(move input, item))\n" }
            else { "    return vec_push_spare(move input, item)\n" };
        let source = format!("{declarations}{signature}\n{clauses}{body}fn helper<T>(input: Vec<T>, item: T) -> Result<Vec<T>, VecPushError<T>>:\n{path}fn main():\n    return\n");
        let syntax = crate::compiler::parse(&source).unwrap();
        let logical = super::super::check::program(&syntax).unwrap();
        let prepared = super::super::source_vc::prepare_with_sites(&logical).unwrap();
        super::super::concrete::program(logical).unwrap();
        prepared
    }
    #[test]
    fn actual_dead_site_presence_is_not_reached_evidence() {
        let dead = actual(true);
        assert_eq!(dead.goals.len(),2);
        assert_eq!(dead.goals[1].source.as_ref().unwrap().kind,"call_precondition");
        assert!(!dead.sites[0].reached);
        assert!(plan_current(&dead.goals,&dead.sites,"context").unwrap().is_none());
        let live = actual(false);
        assert!(live.sites[0].reached);
        assert_eq!(plan_current(&live.goals,&live.sites,"context").unwrap().unwrap().index,0);
    }
    #[test]
    fn exact_statement_prover_and_context_bind_lookup_without_pruning_goals() {
        let mut live = actual(false);
        let first = plan_current(&live.goals,&live.sites,"context").unwrap().unwrap();
        let changed_context = plan_current(&live.goals,&live.sites,"changed").unwrap().unwrap();
        assert_ne!(first.key,changed_context.key);
        live.goals[0].prover.as_mut().unwrap().push_str("  -- changed proof\n");
        let changed_proof = plan_current(&live.goals,&live.sites,"context").unwrap().unwrap();
        assert_ne!(first.key,changed_proof.key);
        live.goals[0].statement.push_str(" ∧ False");
        let changed_body = plan_current(&live.goals,&live.sites,"context").unwrap().unwrap();
        assert_ne!(changed_proof.key,changed_body.key);
        let refs = live.goals.iter().collect::<Vec<_>>();
        let fresh = super::super::proof_module::emit(super::super::proof_module::Namespace::Current,&refs,None).unwrap();
        assert!(!fresh.text.contains("exact WitchyBody_")); assert!(fresh.text.contains("theorem goal_1"));
        let alias = super::super::proof_module::emit(super::super::proof_module::Namespace::Current,&refs,Some((0,&changed_body.key))).unwrap();
        assert!(alias.text.contains("theorem goal_0") && alias.text.contains("theorem goal_1"));
        assert!(BodyKey::parse(&"0".repeat(64)).is_err()); assert!(BodyKey::parse(&"A".repeat(64)).is_err());
    }
}
