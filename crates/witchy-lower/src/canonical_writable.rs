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

    /// An owner-family token the checker's owned-origin flow grants
    /// (`check/owners.rs`): a fresh owner list, a push or set into an owned
    /// list, an owned-GC call result, or a unique lane, that no clone, read,
    /// capture or list element has since aliased. Any other token may be
    /// aliased, so an owned-GC contract cannot accept it.
    pub(super) fn owned_gc_token(&self, value: ValueId) -> bool {
        self.owned_gc_token_via(value, &mut BTreeSet::new())
    }

    /// A use after which the checker no longer counts `value` as owned.
    /// Position is ignored: any such use anywhere disqualifies the token.
    fn gc_token_aliased(&self, value: ValueId) -> bool {
        self.blocks.iter().flat_map(|block| &block.operations).any(|operation| match operation {
            Operation::ClosureOwner(
                OwnerOperation::Clone { owner: used, .. }
                | OwnerOperation::ReadOwner { owner: used, .. }
                | OwnerOperation::CloneList { list: used, .. }
                | OwnerOperation::ListGet { list: used, .. },
            ) => *used == value,
            Operation::ClosureOwner(OwnerOperation::NewList { elements: used, .. }
                | OwnerOperation::Construct { captures: used, .. }) => used.contains(&value),
            _ => false,
        })
    }

    fn owned_gc_token_via(&self, value: ValueId, visited: &mut BTreeSet<ValueId>) -> bool {
        if !visited.insert(value) || self.gc_token_aliased(value) {
            return false;
        }
        let owned_result = |signature: FunctionSignatureId, destinations: &[ValueId]| {
            let Some(index) = destinations.iter().position(|destination| *destination == value) else { return false };
            self.signatures.iter().find(|entry| entry.id == signature).is_some_and(|entry|
                matches!(entry.contract.results.get(index), Some(ResultContract::OwnedGc { .. })))
        };
        for block in &self.blocks {
            if let Some(parameter) = block.parameters.iter().find(|parameter| parameter.value == value) {
                return parameter.ownership == Some(Ownership::Unique);
            }
            for operation in &block.operations {
                match operation {
                    Operation::ClosureOwner(OwnerOperation::NewList { destination, .. }) if *destination == value => {
                        return true;
                    }
                    Operation::ClosureOwner(
                        OwnerOperation::ListPush { destination, list, .. }
                        | OwnerOperation::ListSet { destination, list, .. },
                    ) if *destination == value => {
                        return self.owned_gc_token_via(*list, visited);
                    }
                    Operation::VarCall { destinations, signature, .. } if destinations.contains(&value) => {
                        return owned_result(*signature, destinations);
                    }
                    Operation::Call { destinations, callee, .. } if destinations.contains(&value) => {
                        return self.functions.values().find(|(id, _)| id == callee)
                            .is_some_and(|(_, signature)| owned_result(*signature, destinations));
                    }
                    _ => {}
                }
            }
        }
        false
    }

    /// A non-affine owner list carried through a loop is a unique lane while
    /// every edge into it brings an owned token. Demote each lane that some
    /// edge feeds an aliased token, and repeat: a demoted lane is no longer
    /// owned, so the lanes it feeds may need demoting in turn.
    pub(super) fn demote_owner_list_lanes_fed_aliases(&mut self) {
        loop {
            let mut demote = Vec::new();
            for block in &self.blocks {
                let edges = match &block.terminator {
                    Some(Terminator::Branch { target, arguments }) => vec![(*target, arguments)],
                    Some(Terminator::BranchIf { then_target, then_arguments, else_target, else_arguments, .. }) =>
                        vec![(*then_target, then_arguments), (*else_target, else_arguments)],
                    _ => continue,
                };
                for (target, arguments) in edges {
                    for (index, argument) in arguments.iter().enumerate() {
                        let parameter = &self.blocks[target.get() as usize].parameters[index];
                        if parameter.ownership == Some(Ownership::Unique)
                            && let ValueType::Gc(family) = parameter.value_type
                            && self.owner_list_family(*argument).is_some()
                            && !self.affine_gc_family(family)
                            && !self.owned_gc_token(*argument)
                        {
                            demote.push((target, index));
                        }
                    }
                }
            }
            if demote.is_empty() {
                return;
            }
            for (block, index) in demote {
                self.blocks[block.get() as usize].parameters[index].ownership = Some(Ownership::Shared);
            }
        }
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

    /// A `var x: unique T` parameter writes back the sole token of its
    /// storage, under the same rule as a `unique` result: normal mode
    /// repairs a shared final value (`x = shared_result(x)`) with one copy.
    pub(super) fn unique_write_back_token(&mut self, name: &str, value: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let Some(ty) = self.write_back_parameter_types.get(name) else { return Ok(value) };
        if self.opt_mode
            || !matches!(ty, Type::Qualified(TypeQual::Unique, _))
            || !matches!(self.values[&value], ValueType::Ref(_))
            || self.sole_token(value)
        {
            return Ok(value);
        }
        let ty = ty.clone();
        if Some(self.values[&value]) == self.string_layout_id().ok().map(ValueType::Ref) {
            return self.owned_string_copy(value);
        }
        if matches!(ty.unqualified(), Type::Named(head, arguments) if head == "List" && arguments.len() == 1) {
            return self.clone_list_value_for_storage(value, ty.unqualified());
        }
        if self.is_table_dictionary(&ty)? {
            return self.lower_dict_clone_exact(value);
        }
        // No exact copy for this storage: SafeWIR rejects the shared lane.
        Ok(value)
    }
}
