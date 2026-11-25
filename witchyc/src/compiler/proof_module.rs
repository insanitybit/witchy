//! One emitter for current proofs and a single separately audited body theorem.
use super::{proof::{Goal, AUDIT, IMPORTS}, proof_body::BodyKey};
use crate::diagnostic::Diagnostic;
type Result<T> = std::result::Result<T, Diagnostic>;
fn error() -> Diagnostic { Diagnostic::new("W0402", 0..0, "invalid private proof module plan") }
#[derive(Clone, Copy)]
pub(super) enum Namespace<'a> { Current, Body(&'a BodyKey) }
impl Namespace<'_> {
    pub(super) fn name(&self) -> String {
        match self { Self::Current => "WitchyScalar".into(), Self::Body(key) => format!("WitchyBody_{}", key.as_str()) }
    }
}
pub(super) struct Emitted { pub text: String, entries: Vec<String>, namespace: String }
impl Emitted {
    pub(super) fn audited(mut self, marker: &str) -> Result<String> {
        if marker.is_empty() || marker.len() > 512 || !marker.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b':') { return Err(error()); }
        let audit = if self.namespace == "WitchyScalar" { AUDIT.into() }
            else { AUDIT.replace("`WitchyScalar", &format!("`{}", self.namespace)) };
        self.text.push_str(&audit.replace("__GOALS__", &self.entries.join(", ")).replace("__MARKER__", marker));
        Ok(self.text)
    }
}
pub(super) fn emit(namespace: Namespace<'_>, goals: &[&Goal], application: Option<(usize, &BodyKey)>) -> Result<Emitted> {
    let name = namespace.name();
    if let Some((index, _)) = application {
        if !matches!(namespace, Namespace::Current) || !goals.get(index).is_some_and(|goal| goal.source.as_ref().is_some_and(|s| s.record && s.kind == "function_body")) { return Err(error()); }
    }
    let prelude = include_str!("proof_prelude.lean.in");
    if prelude.matches("namespace WitchyScalar\n").count() != 1 || AUDIT.matches("`WitchyScalar").count() != 1 { return Err(error()); }
    let mut text = String::from(IMPORTS);
    if matches!(namespace, Namespace::Current) { text.push_str(prelude); }
    else { text.push_str(&prelude.replace("namespace WitchyScalar\n", &format!("namespace {name}\n"))); }
    if goals.iter().any(|goal| goal.statement.contains("Witchy.Utf8.")
        || goal.prover.as_deref().is_some_and(|body| body.contains("Witchy.Utf8.")))
    {
        let utf8 = include_str!("../../proof/Witchy/Utf8.lean");
        text.push_str(&format!("end {name}\n"));
        text.push_str(utf8.strip_prefix("import Init\n").ok_or_else(error)?);
        text.push_str(&format!("namespace {name}\n"));
    }

    let mut entries = Vec::new();
    for (index, goal) in goals.iter().enumerate() {
        text.push_str(&format!("def expected_{index} : Prop := {}\ntheorem goal_{index} : expected_{index} := by\n  unfold expected_{index}\n", goal.statement));
        if let Some((_, key)) = application.filter(|(slot, _)| *slot == index) {
            text.push_str(&format!("  exact WitchyBody_{}.goal_0\n", key.as_str()));
        } else if let Some(prover) = &goal.prover { text.push_str(prover); }
        else if goal.statement=="True" { text.push_str("  exact True.intro\n"); }
        else { text.push_str("  intros\n  first | with_reducible assumption | witchy_inline_omega | assumption | simp_all\n"); }
        entries.push(format!("(`{name}.goal_{index}, `{name}.expected_{index})"));
    }
    text.push_str(&format!("end {name}\n"));
    Ok(Emitted { text, entries, namespace: name })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_bytes_and_scoped_auditor_are_preserved() {
        let goal = Goal { source_name:"unused".into(), statement:"True".into(), nodes:1, source:None, prover:None };
        let module = emit(Namespace::Current, &[&goal], None).unwrap();
        let expected = format!("{}{}def expected_0 : Prop := True\ntheorem goal_0 : expected_0 := by\n  unfold expected_0\n  exact True.intro\nend WitchyScalar\n", IMPORTS, include_str!("proof_prelude.lean.in"));
        assert_eq!(module.text, expected);
        assert_eq!(module.audited("TEST:1").unwrap(), expected + &AUDIT.replace("__GOALS__", "(`WitchyScalar.goal_0, `WitchyScalar.expected_0)").replace("__MARKER__", "TEST:1"));
        let key = BodyKey::parse(&"1".repeat(64)).unwrap();
        let body = emit(Namespace::Body(&key), &[&goal], None).unwrap().audited("BODY:1").unwrap();
        assert!(!body.contains("WitchyScalar")); assert!(body.contains(&format!("(`WitchyBody_{}.goal_0", key.as_str())));
        assert!(emit(Namespace::Current, &[&goal], Some((0,&key))).is_err());
    }
}
