//! Logical ownership slots are independent of names and physical register counts.
use crate::diagnostic::Diagnostic;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Availability { Available, Consumed, Maybe }
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(super) struct Flow {
    slots: BTreeMap<u32, Availability>,
}
impl Flow {
    pub fn introduce(&mut self, slot: u32) { self.slots.insert(slot, Availability::Available); }
    pub fn require(&self, slot: u32) -> Result<(), Diagnostic> {
        match self.slots.get(&slot) {
            Some(Availability::Consumed) => return Err(Diagnostic::new("W0401", 0..0, "use of a moved value: it was moved away before this use")),
            Some(Availability::Maybe) => return Err(Diagnostic::new("W0401", 0..0, "use of a moved value: it was moved away on some path that reaches this use")),
            _ => {}
        }
        Ok(())
    }
    pub fn require_owned(&self, slot: u32) -> Result<(), Diagnostic> {
        if !self.slots.contains_key(&slot) {
            return Err(Diagnostic::new("W0401", 0..0, "missing logical owner availability"));
        }
        self.require(slot)
    }
    pub fn consume(&mut self, slot: u32) -> Result<(), Diagnostic> {
        self.require_owned(slot)?;
        let state = self.slots.get_mut(&slot).expect("registered logical owner");
        *state = Availability::Consumed;
        Ok(())
    }
    pub fn reinitialize(&mut self, slot: u32, discardable: bool) -> Result<(), Diagnostic> {
        let state = self.slots.get_mut(&slot).ok_or_else(|| Diagnostic::new("W0401", 0..0, "missing replacement owner slot"))?;
        if !discardable && *state != Availability::Consumed {
            return Err(Diagnostic::new("W0401", 0..0, "linear replacement requires a definitely consumed old slot"));
        }
        *state = Availability::Available;
        Ok(())
    }
    pub fn expire(&mut self, slot: u32, discardable: bool) -> Result<bool, Diagnostic> {
        let state = self.slots.get_mut(&slot).ok_or_else(|| Diagnostic::new("W0401", 0..0, "missing expiring owner slot"))?;
        if *state == Availability::Consumed { return Ok(false); }
        if !discardable { return Err(Diagnostic::new("W0401", 0..0, "live value has no implicit discard permission")); }
        *state = Availability::Consumed;
        Ok(true)
    }
    pub fn remove(&mut self, slot: u32) { self.slots.remove(&slot); }
    /// Whether the value is held here: `Some(true)` on every path, `Some(false)` on none, `None`
    /// when some paths have moved it and others not, or it is not an owner this tracks.
    pub fn held(&self, slot: u32) -> Option<bool> {
        match self.slots.get(&slot)? { Availability::Available => Some(true), Availability::Consumed => Some(false), Availability::Maybe => None }
    }
    /// This flow, keeping only the values that `outer` knows.
    pub fn within(&self, outer: &Self) -> Self {
        Self { slots: self.slots.iter().filter(|(slot, _)| outer.slots.contains_key(slot)).map(|(slot, state)| (*slot, *state)).collect() }
    }
    pub fn join(base: &Self, branches: &[(&Self, bool)]) -> Self {
        let continuing = branches.iter().filter(|(_, exited)| !*exited).collect::<Vec<_>>();
        let selected = if continuing.is_empty() { branches.iter().collect::<Vec<_>>() } else { continuing };
        let mut result = base.clone();
        for (slot, state) in &mut result.slots {
            let mut states = selected.iter().map(|(flow, _)| flow.slots[slot]);
            let first = states.next().expect("control-flow alternatives");
            *state = if states.all(|next| next == first) { first } else { Availability::Maybe };
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maybe_live_is_retained_and_never_acquires_default_discard_permission() {
        let mut start = Flow::default();
        start.introduce(17); // logical ID, unrelated to any physical register
        let mut consumed = start.clone();
        consumed.consume(17).unwrap();
        let mut joined = Flow::join(&start, &[(&start, false), (&consumed, false)]);
        assert_eq!(joined.slots[&17], Availability::Maybe);
        assert!(joined.require_owned(17).is_err());
        assert!(joined.expire(17, false).is_err());
        assert_eq!(joined.slots[&17], Availability::Maybe);
        assert!(joined.expire(17, true).unwrap());
    }
    #[test]
    fn exited_alternative_cannot_poison_or_resurrect_continuing_ownership() {
        let mut start = Flow::default();
        start.introduce(9);
        let mut exited = start.clone();
        exited.consume(9).unwrap();
        let joined = Flow::join(&start, &[(&exited, true), (&start, false)]);
        joined.require_owned(9).unwrap();
        let joined = Flow::join(&start, &[(&start, true), (&exited, false)]);
        assert!(joined.require_owned(9).is_err());
        assert!(Flow::default().require_owned(9).is_err());
    }
}

#[cfg(test)]
mod linear_tests {
    use super::*;
    #[test]
    fn linear_replacement_requires_definite_consumption_and_preserves_failed_state() {
        let mut live = Flow::default();
        live.introduce(71);
        assert!(live.reinitialize(71, false).is_err());
        live.require_owned(71).unwrap();
        let mut consumed = live.clone();
        consumed.consume(71).unwrap();
        let mut maybe = Flow::join(&live, &[(&live, false), (&consumed, false)]);
        assert!(maybe.reinitialize(71, false).is_err());
        assert_eq!(maybe.slots[&71], Availability::Maybe);
        assert!(maybe.expire(71, false).is_err());
        assert_eq!(maybe.slots[&71], Availability::Maybe);
        consumed.reinitialize(71, false).unwrap();
        consumed.require_owned(71).unwrap();
        assert_eq!(consumed, live);
    }
    #[test]
    fn condition_exit_and_backedge_states_are_distinct_without_a_fake_source_consumer() {
        let mut before_condition = Flow::default();
        before_condition.introduce(23);
        let mut after_condition = before_condition.clone();
        after_condition.consume(23).unwrap();
        assert_ne!(after_condition, before_condition); // not a valid continuing backedge
        assert!(after_condition.require_owned(23).is_err()); // false/zero-body exit stays consumed
        let skipped_or_evaluated = Flow::join(&before_condition,
            &[(&before_condition, false), (&after_condition, false)]);
        assert_eq!(skipped_or_evaluated.slots[&23], Availability::Maybe);
        let mut restored_body = after_condition.clone();
        restored_body.reinitialize(23, false).unwrap();
        assert_eq!(restored_body, before_condition);
    }
}
