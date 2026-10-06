//! The one copy-on-write rule for in-place updates. Normal mode repairs a
//! missing uniqueness proof with a copy (RFC-0167 §4): an update writes its
//! operand in place only when that operand is the sole token of the binding
//! the update replaces, and otherwise writes one fresh copy. Every in-place
//! capable operation asks here instead of deciding for itself.
use super::*;

impl FunctionLowerer<'_> {
    /// `source` is a token no other binding or shared alias can observe.
    /// Two bindings of one SSA value are one checker token, so the checker
    /// cannot tell them apart; that alias is excluded here.
    pub(super) fn sole_token(&self, source: ValueId) -> bool {
        self.carries_token(source)
            && !self.known_shared_value(source)
            && self.binding_holders(source) <= 1
    }

    /// How many bindings hold `value`, counting an outer binding a nested
    /// `let` has shadowed: it is restored when the block ends and still
    /// observes the value.
    pub(super) fn binding_holders(&self, value: ValueId) -> usize {
        self.bindings.values().filter(|bound| **bound == value).count()
            + self.binding_scopes.iter()
                .flat_map(|scope| scope.values())
                .filter(|(saved, _)| *saved == Some(value))
                .count()
    }

    /// Lower `x = value`, recording that `value` replaces `x` so an update
    /// whose first operand is `x` may write it in place.
    pub(super) fn lower_rebinding_value(&mut self, name: &str, value: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let rebinds = matches!(value, Expr::Call { args, .. }
            if matches!(args.first(), Some(Expr::Var(operand)) if operand == name));
        let previous = std::mem::replace(
            &mut self.rebinding,
            rebinds.then(|| (name.to_owned(), std::ptr::from_ref(value))),
        );
        let lowered = self.lower_expr(value);
        self.rebinding = previous;
        lowered
    }

    /// Whether the update `expression`, whose evaluated first operand is
    /// `source`, may write `source` in place. Only the statement's own
    /// right-hand side qualifies: a nested update of the same name is read
    /// again by the outer one and must not observe its write.
    pub(super) fn updates_in_place(&self, expression: &Expr, source: ValueId) -> bool {
        self.rebinding.as_ref().is_some_and(|(name, target)| {
            std::ptr::eq(*target, expression)
                && self.bindings.get(name) == Some(&source)
                && self.sole_token(source)
        })
    }

    /// The byte buffer an update of `source` writes: `source` itself, or a
    /// fresh copy.
    pub(super) fn writable_byte_buffer(&mut self, expression: &Expr, source: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        if !self.updates_in_place(expression, source) {
            return self.owned_string_copy(source);
        }
        self.ensure_string_extent(source)?;
        self.recover_raw_byte_capacity(source)?;
        Ok(source)
    }

    /// A result declared `unique` leaves as the sole token of its storage.
    /// Opt mode proved that in source; normal mode repairs a shared result
    /// with one copy. The checker verifies the returned token either way.
    pub(super) fn unique_result_token(&mut self, value: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let Some(ty) = self.unique_result.clone() else { return Ok(value) };
        if self.opt_mode || self.sole_token(value) {
            return Ok(value);
        }
        if Some(self.values[&value]) == self.string_layout_id().ok().map(ValueType::Ref) {
            return self.owned_string_copy(value);
        }
        if matches!(ty.unqualified(), Type::Named(head, arguments) if head == "List" && arguments.len() == 1) {
            return self.clone_list_value_for_storage(value, ty.unqualified());
        }
        Ok(value)
    }
}
