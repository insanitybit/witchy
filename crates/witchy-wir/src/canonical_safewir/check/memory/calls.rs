use super::*;

impl<'a> MemoryChecker<'a> {
    pub(crate) fn finish_affine_field_borrow(&mut self) {
        for (borrowed, _) in self.state.pending_affine_field_borrow.drain(..) {
            self.state.affine_plans.remove(&borrowed);
            if let Some(object) = self.state.objects.get_mut(&borrowed) {
                object.ownership = Ownership::Consumed;
            }
        }
    }

    pub(crate) fn ensure_no_pending_affine_borrow(&mut self) -> Result<(), InitialCheckError> {
        self.finish_affine_field_borrow();
        if self.state.pending_affine_borrow.is_some() {
            return self.unsupported("Dictionary(borrowed affine child crosses a block edge)");
        }
        Ok(())
    }

    pub(crate) fn is_borrowed_parameter(&self, value: ValueId) -> bool {
        // Keep the existing scalar-result borrow exemption. An aggregate
        // result gets the same treatment only for an authenticated recursive
        // affine parameter proof; the call contract cannot alias that borrow.
        self.function.blocks.iter().find(|block| block.id == self.function.entry)
            .and_then(|entry| entry.parameters.iter().position(|parameter| parameter.value == value))
            .is_some_and(|index| {
                let scalar_result = self.signature.results.iter().all(|result| matches!(result,
                    ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit
                        | ValueType::Size | ValueType::V128));
                let affine_borrow = self.affine_abi
                    .and_then(|proofs| proofs.get(&self.signature.id))
                    .and_then(|proof| proof.parameters.get(index))
                    .and_then(Option::as_ref).is_some();
                // A lent handle is the caller's: this function may clone it but
                // never returns it, unless its contract says a result aliases
                // the parameter (the one-in/one-out alias ABI consumes it).
                let lent = !self.signature.contract.results.iter().any(|result| matches!(result,
                    ResultContract::AliasesParameter { parameter, .. } if usize::try_from(*parameter).ok() == Some(index)));
                (scalar_result || affine_borrow || lent)
                    && self.signature.contract.parameters.get(index).is_some_and(|parameter|
                        matches!(parameter.value_type, ValueType::Ref(_))
                            && parameter.ownership == ParameterOwnership::Value)
            })
    }

    pub(super) fn initialize_parameters(
        &mut self,
        signature: &FunctionSignature,
    ) -> Result<(), InitialCheckError> {
        let entry = self
            .function
            .blocks
            .iter()
            .find(|block| block.id == self.function.entry)
            .ok_or(InitialCheckError::MissingBlock {
                function: self.function.id,
                block: self.function.entry,
            })?;
        for (index, parameter) in entry.parameters.iter().enumerate() {
            let ValueType::Ref(layout) = parameter.value_type else {
                continue;
            };
            let contract = signature
                .contract
                .parameters
                .get(index)
                .ok_or(InitialCheckError::UnsupportedMemorySignature {
                    signature: signature.id,
                })?;
            let ownership = match contract.ownership {
                ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack => Ownership::Unique,
                ParameterOwnership::Shared | ParameterOwnership::WriteBack | ParameterOwnership::Value => Ownership::Shared,
            };
            let extent = self.parameter_extent(layout)?;
            let (allocation_class, shared_immutable) = match contract.ownership {
                ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack => (Some(AllocationClass::ReferenceCounted), false),
                ParameterOwnership::Shared | ParameterOwnership::WriteBack | ParameterOwnership::Value => (None, true),
            };
            let initialized_owned_slots = matches!(
                contract.ownership,
                ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack | ParameterOwnership::Shared | ParameterOwnership::WriteBack | ParameterOwnership::Value
            );
            self.state.objects.insert(
                parameter.value,
                ObjectState {
                    identity: parameter.value,
                    owner_generation: 0,
                    layout,
                    extent,
                    ownership,
                    allocation_class,
                    shared_immutable,
                    initialized_owned_slots,
                },
            );
            if let Some(plan) = self.affine_abi
                .and_then(|proofs| proofs.get(&signature.id))
                .and_then(|proof| proof.parameters.get(index))
                .and_then(Option::as_ref)
            {
                if ownership != Ownership::Unique
                    && !(contract.ownership == ParameterOwnership::Value
                        && self.is_borrowed_parameter(parameter.value))
                {
                    return self.unsupported("Parameter(affine storage requires unique root)");
                }
                self.seed_affine_plan(parameter.value, plan);
            }
            self.bind_extent_generation(parameter.value)?;
        }
        Ok(())
    }

    /// Bind an owner's current counted extent to its scalar lanes.  This is
    /// the source-to-canonical carrier for range facts: it is created only
    /// from checked allocation/ABI state and is refreshed only after the
    /// checker has accepted the new owner state.
    pub(super) fn bind_extent_generation(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(object)?.clone();
        let Extent::Counted { count, capacity, .. } = state.extent else {
            return Ok(());
        };
        let fact = OwnerGeneration { identity: state.identity, epoch: state.owner_generation };
        self.state.bound_generations.entry(count).or_default().insert(fact);
        self.state.bound_generations.entry(capacity).or_default().insert(fact);
        Ok(())
    }

    /// An allocation may be introduced after arithmetic guards over its
    /// already-defined count or capacity lanes. Those guards are valid for
    /// the new allocation, but were necessarily created before its identity
    /// existed, so `Operation::Guard` could not record that identity then.
    /// Bind only this newly created allocation. Extent mutation deliberately
    /// does not use this path: it must leave predecessor guards stale.
    pub(super) fn bind_preallocation_extent_guards(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(object)?.clone();
        let Extent::Counted { count, capacity, .. } = state.extent else {
            return Ok(());
        };
        let generation = self.current_generation(object)?;
        // Only `guard_generations` changes below, so the header-length pairs
        // behind `same_scalar` are fixed: collect them once, not twice per
        // guard (a sorted rebuild each time), and select before mutating.
        let header_lengths = Self::header_length_pairs(&self.state);
        let bound_guards = self.state.guards.iter().copied().filter(|(_, _, right)| {
            Self::same_scalar_with(&self.state, &header_lengths, *right, count)
                || Self::same_scalar_with(&self.state, &header_lengths, *right, capacity)
        }).collect::<Vec<_>>();
        for guard in bound_guards {
            self.state.guard_generations.entry(guard).or_default().insert(generation);
        }
        Ok(())
    }

    pub(super) fn current_generation(&self, object: ValueId) -> Result<OwnerGeneration, InitialCheckError> {
        let state = self.live_object(object)?;
        Ok(OwnerGeneration { identity: state.identity, epoch: state.owner_generation })
    }

    pub(super) fn bound_is_current(&self, object: ValueId, bound: ValueId) -> bool {
        // Literal bounds are arithmetic facts, not observations of mutable
        // owner state.  Every nonliteral dynamic extent must carry its owner
        // generation explicitly. A fresh length load is also an observation
        // of that owner, but only when it names this exact allocation and
        // generation. This does not trust scalar equivalence: a stale or
        // other-allocation header lane cannot authorize an indexed access.
        self.state.constants.contains_key(&bound)
            || self.current_generation(object).ok().is_some_and(|generation| {
                self.state.bound_generations.get(&bound)
                    .is_some_and(|facts| facts.contains(&generation))
                    || self.fresh_header_generations(bound).contains(&generation)
            })
    }

    /// Return only live current generations established by an exact Length
    /// header load. The scalar lane alone is never enough: its header object
    /// must still be live and at the generation recorded at the load.
    pub(super) fn fresh_header_generations(&self, bound: ValueId) -> SortedSet<OwnerGeneration> {
        self.state.fresh_header_lengths.get(&bound).into_iter()
            .flat_map(|facts| facts.iter().copied())
            .filter(|generation| self.state.header_values.iter().any(|((object, field), header)| {
                *field == HeaderField::Length
                    && *header == bound
                    && self.current_generation(*object).ok() == Some(*generation)
            }))
            .collect()
    }

    /// Advance every live token for this allocation and discard the old
    /// extent authentication.  Use this only when the logical extent itself
    /// can have changed; element initialization and owner-token transport
    /// advance their ownership state through their own fact tables without
    /// changing a checked count or capacity.
    pub(super) fn invalidate_owner_generation(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        self.advance_owner_generation(object, false)
    }

    /// Advance owner state after an extent-preserving operation.  Bounds
    /// evidence is rebound to the new generation only when it already named
    /// this exact allocation; guards, fresh headers, and counted lanes for
    /// other allocations remain untouched.  This retains no element-prefix
    /// or ownership facts, which each operation updates separately.
    pub(super) fn rebase_extent_generation(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        self.advance_owner_generation(object, true)
    }

    pub(super) fn advance_owner_generation(
        &mut self,
        object: ValueId,
        preserve_extent_authentication: bool,
    ) -> Result<(), InitialCheckError> {
        let identity = self.live_object(object)?.identity;
        let maximum = self.state.objects.values()
            .filter(|state| state.identity == identity)
            .map(|state| state.owner_generation)
            .max()
            .unwrap_or(0);
        let next = if maximum == JOINED_OWNER_GENERATION {
            JOINED_OWNER_GENERATION
        } else {
            maximum.checked_add(1).ok_or(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            })?
        };
        for state in self.state.objects.values_mut().filter(|state| state.identity == identity) {
            state.owner_generation = next;
        }
        let rebase = |facts: &mut SortedSet<OwnerGeneration>| {
            if facts.iter().any(|fact| fact.identity == identity) {
                facts.retain(|fact| fact.identity != identity);
                facts.insert(OwnerGeneration { identity, epoch: next });
            }
        };
        if preserve_extent_authentication {
            for facts in self.state.bound_generations.values_mut() {
                rebase(facts);
            }
            for facts in self.state.fresh_header_lengths.values_mut() {
                rebase(facts);
            }
            for facts in self.state.guard_generations.values_mut() {
                rebase(facts);
            }
        } else {
            self.state.bound_generations.retain(|_, facts| {
                facts.retain(|fact| fact.identity != identity);
                !facts.is_empty()
            });
            self.state.fresh_header_lengths.retain(|_, facts| {
                facts.retain(|fact| fact.identity != identity);
                !facts.is_empty()
            });
        }
        let aliases = self.state.objects.iter().filter_map(|(value, state)| {
            (state.identity == identity && state.ownership != Ownership::Consumed).then_some(*value)
        }).collect::<Vec<_>>();
        for alias in aliases {
            self.bind_extent_generation(alias)?;
        }
        Ok(())
    }

    pub(super) fn parameter_extent(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<Extent, InitialCheckError> {
        match self.descriptor(layout)?.size() {
            LayoutSize::Fixed(bytes) => Ok(Extent::FixedBytes(u64::from(bytes))),
            LayoutSize::Dynamic { base, .. } => {
                Ok(Extent::HeaderPrefix { minimum_bytes: base })
            }
        }
    }

    /// Whether a callee with this contract can keep anything it was lent
    /// past the call. Retention needs somewhere to put a reference: a result,
    /// a write-back lane, or a callable, capability, or GC carrier that could
    /// hand one on. A reference lane can carry a share of a lent object only
    /// if its layout holds a reference or is reachable from a reference
    /// parameter's layout. A contract with no such lane keeps nothing, so the
    /// caller's token keeps its exclusive authority.
    pub(super) fn cannot_retain_borrow(&self, parameters: &[ParameterContract], results: &[ResultContract]) -> bool {
        results.iter().all(|result| match result {
            ResultContract::Value(value_type) => self.lane_cannot_retain(*value_type, parameters),
            ResultContract::CheckedValue { layout }
            | ResultContract::Fresh { layout, .. }
            | ResultContract::Shared { layout }
            | ResultContract::Unique { layout }
            | ResultContract::StaticData { layout } => self.lane_cannot_retain(ValueType::Ref(*layout), parameters),
            _ => false,
        }) && parameters.iter().all(|parameter| match parameter.ownership {
            ParameterOwnership::WriteBack | ParameterOwnership::UniqueWriteBack =>
                self.lane_cannot_retain(parameter.value_type, parameters),
            _ => !matches!(parameter.value_type,
                ValueType::Gc(_) | ValueType::Capability(_) | ValueType::NullableCapability(_) | ValueType::Callable(_)),
        })
    }

    fn lane_cannot_retain(&self, value_type: ValueType, parameters: &[ParameterContract]) -> bool {
        match value_type {
            ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit | ValueType::Size | ValueType::V128 => true,
            ValueType::InlineAggregate(layout) => self.layouts.holds_no_reference(layout),
            ValueType::Ref(layout) => self.layouts.holds_no_reference(layout)
                && !parameters.iter().any(|parameter| matches!(parameter.value_type,
                    ValueType::Ref(lent) if self.layouts.may_reach(lent, layout))),
            ValueType::Gc(_) | ValueType::Capability(_) | ValueType::NullableCapability(_) | ValueType::Callable(_) => false,
        }
    }

    pub(crate) fn check_call(
        &mut self,
        callee_signature: &FunctionSignature,
        arguments: &[CallArgument],
        parameters: &[ParameterContract],
        destinations: &[ValueId],
        results: &[ResultContract],
    ) -> Result<(), InitialCheckError> {
        if callee_signature.contract.parameters.as_slice() != parameters
            || callee_signature.contract.results.as_slice() != results
        {
            return Err(InitialCheckError::CallableContractMismatch {
                signature: callee_signature.id,
            });
        }
        let mut argument_states = Vec::with_capacity(arguments.len());
        // `check::exact_moved_value_alias_parameter_transport` proves the
        // whole one-in/one-out alias ABI. Keep this separate from ordinary
        // `Value` calls: only that contract can consume a sole managed token
        // and rebind it through its matching alias result below.
        let exact_moved_value_alias = arguments.len() == 1
            && arguments.first().is_some_and(|argument| {
                super::super::exact_moved_value_alias_parameter_transport(argument, callee_signature)
            });
        let exact_indirect_moved_value_alias = arguments.iter().enumerate().any(|(index, argument)| {
            super::super::exact_indirect_moved_value_alias_parameter_transport(index, argument, callee_signature)
        });
        // A fixed-field returned exclusive handle may carry a Bump-allocated
        // aggregate root across exactly one authenticated write-back call.
        // The ordinary result and hidden root lane establish that this is a
        // relay, not a general Bump-memory ABI boundary.
        let projected_bump_write_back =
            authenticated_projected_bump_write_back_parameter(results, parameters);
        let affine_proof = self.affine_abi.and_then(|proofs| proofs.get(&callee_signature.id));
        let pending_borrow = self.state.pending_affine_borrow;
        let pending_field = self.state.pending_affine_field_borrow.last().copied();
        if let Some((borrowed, parent)) = pending_field {
            let borrowed_index = arguments.iter().position(|argument| matches!(argument,
                CallArgument::Value(value) if *value == borrowed));
            if borrowed_index.is_none()
                || arguments.iter().filter(|argument| matches!(argument,
                    CallArgument::Value(value) if *value == borrowed)).count() != 1
                || arguments.iter().any(|argument| matches!(argument,
                    CallArgument::Value(value) | CallArgument::Move(value)
                        if self.same_allocation(*value, parent) || self.state.pending_affine_field_borrow.iter()
                            .any(|(ancestor, _)| value == ancestor && *value != borrowed)))
                || !results.iter().all(|result| matches!(result,
                    ResultContract::Value(ValueType::Int | ValueType::Float | ValueType::Bool
                        | ValueType::Unit | ValueType::Size | ValueType::V128)))
                || borrowed_index.and_then(|index| affine_proof
                    .and_then(|proof| proof.parameters.get(index)).and_then(Option::as_ref))
                    != self.state.affine_plans.get(&borrowed)
            {
                return self.unsupported("Call(borrowed affine field boundary)");
            }
            self.live_object(parent)?;
        }
        if let Some((borrowed, dict)) = pending_borrow {
            let borrowed_index = arguments.iter().position(|argument| matches!(argument,
                CallArgument::Value(value) if *value == borrowed));
            if borrowed_index.is_none()
                || arguments.iter().filter(|argument| matches!(argument,
                    CallArgument::Value(value) if *value == borrowed)).count() != 1
                || arguments.iter().any(|argument| matches!(argument,
                    CallArgument::Value(value) | CallArgument::Move(value) if *value == dict))
                || borrowed_index.and_then(|index| affine_proof
                    .and_then(|proof| proof.parameters.get(index)).and_then(Option::as_ref))
                    != self.state.affine_plans.get(&borrowed)
            {
                return self.unsupported("Dictionary(borrowed affine key call boundary)");
            }
            self.live_object(dict)?;
        }
        for (parameter_index, (argument, parameter)) in arguments.iter().zip(parameters).enumerate() {
            let value = match argument {
                CallArgument::Value(value) | CallArgument::Move(value) => *value,
            };
            let actual = value_type(self.function, value)?;
            if matches!(actual, ValueType::Gc(_)) {
                argument_states.push(None);
                // The separate opaque-token CFG judgment consumes this lane.
                // It has no linear address, extent, or allocation class.
                if !matches!(argument, CallArgument::Move(_))
                    || !matches!(parameter.ownership, ParameterOwnership::Shared | ParameterOwnership::WriteBack | ParameterOwnership::Unique)
                {
                    return self.unsupported("Call(opaque ownership)");
                }
                continue;
            }
            if !actual.is_linear_memory_ref() {
                argument_states.push(None);
                if !matches!(argument, CallArgument::Value(_)) {
                    return self.unsupported("Call(non-memory ownership)");
                }
                continue;
            }
            if matches!(argument, CallArgument::Move(_)) {
                // A call boundary is opaque to a future range pass unless it
                // establishes a new checked result carrier.  Advance before
                // consumption so any alias result receives the new generation
                // while every pre-call guard remains stale.
                self.invalidate_owner_generation(value)?;
            }
            argument_states.push(Some(self.live_object(value)?.clone()));
            match argument {
                // A recursive affine proof permits a call-scoped borrow even
                // when the result is an aggregate, provided the result does
                // not claim an alias of this borrowed parameter.
                CallArgument::Value(value) if parameter.ownership == ParameterOwnership::Value
                    && (results.iter().all(|result| matches!(result,
                        ResultContract::Value(ValueType::Int | ValueType::Float | ValueType::Bool
                            | ValueType::Unit | ValueType::Size | ValueType::V128)))
                        || !results.iter().any(|result| matches!(result,
                            ResultContract::AliasesParameter { parameter, .. } if usize::try_from(*parameter).ok() == Some(parameter_index)))
                        || affine_proof.and_then(|proof| proof.parameters.get(parameter_index))
                            .and_then(Option::as_ref).is_some()
                            && !results.iter().any(|result| matches!(result,
                                ResultContract::AliasesParameter { parameter, .. } if usize::try_from(*parameter).ok() == Some(parameter_index)))) =>
                {
                    self.require_complete_value_initialized(*value, "Call(uninitialized borrowed slot)")?;
                    if let Some(plan) = affine_proof.and_then(|proof|
                        proof.parameters.get(parameter_index)).and_then(Option::as_ref)
                    {
                        let borrowed_affine = (pending_borrow.is_some_and(|(borrowed, _)| borrowed == *value)
                            || pending_field.is_some_and(|(borrowed, _)| borrowed == *value)
                            || self.is_borrowed_parameter(*value))
                            && self.state.affine_plans.get(value) == Some(plan);
                        if !borrowed_affine && !self.require_affine_plan(*value, plan)? {
                            return self.unsupported("Call(borrowed affine argument without recursive proof)");
                        }
                    } else if !self.cannot_retain_borrow(parameters, results) {
                        // An ordinary borrowed read may retain a clone. The
                        // caller keeps its token but loses exclusive authority.
                        self.share_allocation_tokens(*value)?;
                    }
                }
                CallArgument::Value(_) => return self.unsupported("Call(Value memory)"),
                CallArgument::Move(value) => {
                    self.require_complete_value_initialized(*value, "Call(uninitialized owned slot)")?;
                    if let Some(plan) = affine_proof.and_then(|proof| proof.parameters.get(parameter_index)).and_then(Option::as_ref)
                        && !self.require_affine_plan(*value, plan)?
                    {
                        return self.unsupported("Call(affine argument without recursive proof)");
                    }
                    // A complete scalar aggregate can be proven from its
                    // individual stored-slot facts.  A write-back call's
                    // hidden replacement carrier intentionally drops the
                    // old extent lanes, so retain this checked completeness
                    // summary on its pre-consumption snapshot.  This grants
                    // initialization only for a subsequently authenticated
                    // current prefix; it does not retain a stale bound.
                    if let Some(state) = argument_states
                        .get_mut(parameter_index)
                        .and_then(Option::as_mut)
                    {
                        state.initialized_owned_slots = true;
                    }
                    let authenticated_bump_relay = projected_bump_write_back == Some(parameter_index)
                        && self.live_object(*value)?.allocation_class == Some(AllocationClass::Bump);
                    if authenticated_bump_relay {
                        self.consume_authenticated_projected_bump_write_back(*value)?;
                    } else if (exact_moved_value_alias && parameter_index == 0
                        || exact_indirect_moved_value_alias && parameter_index == 1)
                        && parameter.ownership == ParameterOwnership::Value
                    {
                        self.consume_exact_moved_value_alias(*value)?;
                    } else {
                        self.consume_call_move(*value, parameter.ownership)?;
                    }
                }
            }
        }
        for (result_index, (destination, result)) in destinations.iter().zip(results).enumerate() {
            let affine_result = affine_proof.and_then(|proof| proof.results.get(result_index)).and_then(Option::as_ref);
            if affine_result.is_some() {
                if !matches!(result, ResultContract::Unique { .. }) {
                    return self.unsupported("Call(affine result without unique contract)");
                }
            }
            match result {
                ResultContract::Value(value_type) if !value_type.is_linear_memory_ref() => {}
                ResultContract::OwnedGc { .. } => {}
                ResultContract::CheckedValue { layout } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            owner_generation: 0,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Shared,
                            allocation_class: None,
                            shared_immutable: false,
                            initialized_owned_slots: true,
                        },
                    );
                }
                ResultContract::Fresh { layout, class, .. } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            owner_generation: 0,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Unique,
                            allocation_class: Some(*class),
                            shared_immutable: false,
                            initialized_owned_slots: true,
                        },
                    );
                }
                ResultContract::Unique { layout } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(*destination, ObjectState {
                        identity: *destination, owner_generation: 0, layout: *layout, extent,
                        ownership: Ownership::Unique,
                        allocation_class: Some(AllocationClass::ReferenceCounted),
                        shared_immutable: false, initialized_owned_slots: true,
                    });
                    if let Some(plan) = affine_result {
                        self.seed_affine_plan(*destination, plan);
                    } else {
                        self.attach_unique_result_leaf(*destination, *layout);
                    }
                }
                ResultContract::ReallocatesParameter { parameter, layout } => {
                    let index = usize::try_from(*parameter).map_err(|_| {
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id }
                    })?;
                    if !matches!(parameters.get(index), Some(ParameterContract {
                        value_type: ValueType::Ref(parameter_layout),
                        ownership: ParameterOwnership::WriteBack,
                    }) if *parameter_layout == *layout)
                        || write_back_hidden_lane(results, *parameter, parameters) != Some(result_index)
                    {
                        return self.unsupported("Call(reallocated write-back contract)");
                    }
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(*destination, ObjectState {
                        identity: *destination,
                        owner_generation: 0,
                        layout: *layout,
                        extent,
                        ownership: Ownership::Unique,
                        allocation_class: Some(AllocationClass::ReferenceCounted),
                        shared_immutable: false,
                        initialized_owned_slots: true,
                    });
                }
                ResultContract::Shared { layout } => {
                    if let Some((parameter, alias_layout)) =
                        write_back_alias_parameter_for_hidden_lane(results, result_index, parameters)
                    {
                        if *layout != alias_layout {
                            return self.unsupported("Call(exclusive alias/write-back hidden layout)");
                        }
                        let index = usize::try_from(parameter).map_err(|_| {
                            InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id }
                        })?;
                        let contract = parameters.get(index).ok_or(
                            InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id },
                        )?;
                        if contract.ownership != ParameterOwnership::WriteBack
                            || contract.value_type != ValueType::Ref(*layout)
                        {
                            return self.unsupported("Call(exclusive alias/write-back hidden contract)");
                        }
                        let mut state = argument_states.get(index).and_then(Option::as_ref).cloned()
                            .ok_or(InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id })?;
                        if state.layout != *layout {
                            return self.unsupported("Call(exclusive alias/write-back hidden identity)");
                        }
                        self.require_ref_type(*destination, *layout)?;
                        // A `var` call may replace its argument rather than
                        // mutate its old allocation. Its hidden replacement
                        // lane may therefore not retain the input's counted
                        // lanes: they name a pre-call owner generation and
                        // could otherwise authorize a later element access.
                        // Keep the carrier identity, but require a fresh
                        // guarded header observation for its extent.
                        state.extent = self.parameter_extent(*layout)?;
                        state.ownership = Ownership::Shared;
                        // The callee may mutate or replace any part of the
                        // write-back value.  The checked call admission above
                        // establishes that the returned logical value is
                        // complete, but predecessor-local slot and range
                        // records describe the pre-call representation and
                        // must not survive.  Keep only the authenticated
                        // completeness summary and require fresh header lanes
                        // before indexing the returned carrier.
                        let identity = state.identity;
                        let aliases = self
                            .state
                            .objects
                            .iter()
                            .filter_map(|(value, candidate)| {
                                (candidate.identity == identity).then_some(*value)
                            })
                            .collect::<SortedSet<_>>();
                        self.state
                            .initialized_scalar_slots
                            .retain(|(container, _)| !aliases.contains(container));
                        self.state
                            .stored_objects
                            .retain(|(container, _), _| !aliases.contains(container));
                        self.state
                            .nullable_unique_slots
                            .retain(|(container, _)| !aliases.contains(container));
                        self.state
                            .nullable_present_slots
                            .retain(|(container, _)| !aliases.contains(container));
                        self.state
                            .nullable_absent_slots
                            .retain(|(container, _)| !aliases.contains(container));
                        self.state
                            .range_states
                            .retain(|range, _| !aliases.contains(&range.object));
                        self.state
                            .full_current_prefixes
                            .retain(|range| !aliases.contains(&range.object));
                        self.state.objects.insert(*destination, state);
                        continue;
                    }
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            owner_generation: 0,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Shared,
                            allocation_class: None,
                            shared_immutable: true,
                            // A checked Shared result is a complete logical
                            // value at the ABI boundary. This is distinct from
                            // locally allocated or Fresh storage, which needs
                            // slot/range evidence before it can escape.
                            initialized_owned_slots: true,
                        },
                    );
                }
                ResultContract::AliasesParameter { parameter, layout } => {
                    let index = usize::try_from(*parameter).map_err(|_| {
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id }
                    })?;
                    let contract = parameters.get(index).ok_or(
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id },
                    )?;
                    if contract.ownership != ParameterOwnership::Value
                        || contract.value_type != ValueType::Ref(*layout)
                    {
                        return self.unsupported("Call(borrowed alias contract)");
                    }
                    let mut state = argument_states.get(index).and_then(Option::as_ref).cloned()
                        .ok_or(InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id })?;
                    if state.layout != *layout {
                        return self.unsupported("Call(borrowed alias layout)");
                    }
                    self.require_ref_type(*destination, *layout)?;
                    state.ownership = Ownership::Shared;
                    self.state.objects.insert(*destination, state);
                    if arguments.get(index).is_some_and(|argument| {
                        let value = match argument {
                            CallArgument::Value(value) | CallArgument::Move(value) => value,
                        };
                        self.state.static_data_objects.contains(value)
                    }) {
                        self.state.static_data_objects.insert(*destination);
                    }
                }
                ResultContract::AliasesWriteBackParameter { parameter, layout, projection } => {
                    let index = usize::try_from(*parameter).map_err(|_| {
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id }
                    })?;
                    let contract = parameters.get(index).ok_or(
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id },
                    )?;
                    let claimed_projected = projection.is_some();
                    let projected = authenticated_projected_bump_write_back_parameter(results, parameters)
                        == Some(index);
                    if claimed_projected && !projected {
                        return self.unsupported("Call(projected exclusive alias/write-back contract)");
                    }
                    if contract.ownership != ParameterOwnership::WriteBack
                        || (!projected && contract.value_type != ValueType::Ref(*layout))
                        || (projected && !matches!(contract.value_type, ValueType::Ref(_)))
                        || !has_distinct_write_back_lane(results, result_index, *parameter, *layout, parameters)
                    {
                        return self.unsupported("Call(exclusive alias/write-back contract)");
                    }
                    let mut state = argument_states.get(index).and_then(Option::as_ref).cloned()
                        .ok_or(InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id })?;
                    if !projected && state.layout != *layout {
                        return self.unsupported("Call(exclusive alias/write-back layout)");
                    }
                    self.require_ref_type(*destination, *layout)?;
                    if projected {
                        state = ObjectState {
                            identity: *destination,
                            owner_generation: 0,
                            layout: *layout,
                            extent: self.parameter_extent(*layout)?,
                            ownership: Ownership::Shared,
                            // This class is authenticated by the callee's
                            // derived `CloneShared(field) -> CloneRef`
                            // projected carrier history. It is never taken
                            // from the Bump owner or inferred from a bare
                            // result contract.
                            allocation_class: Some(AllocationClass::ReferenceCounted),
                            shared_immutable: false,
                            initialized_owned_slots: true,
                        };
                        // This is a capability marker, not initialization
                        // inferred from layout: only the checked projected
                        // result contract installs its exact fixed Int field
                        // on the freshly named result token.  `store` below
                        // consumes it only alongside this synthetic result
                        // state, so an ordinary shared object cannot borrow
                        // write authority from a scalar-slot fact.
                        let Some(field) = projection else {
                            return self.unsupported("Call(projected exclusive alias without field)");
                        };
                        self.state.initialized_scalar_slots.insert(
                            (*destination, SlotIdentity::Constant(u64::from(*field))),
                        );
                    } else {
                        // The ordinary alias is paired with the hidden
                        // replacement lane above. A write-back call is an
                        // extent boundary even when it preserves the source
                        // storage identity, so stale input count/capacity
                        // facts cannot cross it.
                        state.extent = self.parameter_extent(*layout)?;
                        state.ownership = Ownership::Shared;
                    }
                    self.state.objects.insert(*destination, state);
                }
                ResultContract::AliasesOneOfWriteBackParameters { parameters: candidates, layout } => {
                    if candidates.len() < 2
                        || !candidates.windows(2).all(|pair| pair[0] < pair[1])
                    {
                        return self.unsupported("Call(disjunctive exclusive alias candidate set)");
                    }
                    let mut states = candidates.iter().map(|parameter| {
                        let index = usize::try_from(*parameter).map_err(|_| {
                            InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id }
                        })?;
                        let contract = parameters.get(index).ok_or(
                            InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id },
                        )?;
                        if contract.ownership != ParameterOwnership::WriteBack
                            || contract.value_type != ValueType::Ref(*layout)
                            || !has_distinct_write_back_lane(results, result_index, *parameter, *layout, parameters)
                        {
                            return self.unsupported("Call(disjunctive exclusive alias contract)");
                        }
                        argument_states.get(index).and_then(Option::as_ref).cloned().ok_or(
                            InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id },
                        )
                    });
                    let Some(mut state) = states.next().transpose()? else {
                        return self.unsupported("Call(disjunctive exclusive alias empty candidate set)");
                    };
                    for candidate in states {
                        let candidate = candidate?;
                        if candidate.layout != state.layout {
                            return self.unsupported("Call(disjunctive exclusive alias layout)");
                        }
                    }
                    self.require_ref_type(*destination, *layout)?;
                    // This SSA value is a checked one-of carrier, not a claim
                    // that either particular argument was chosen statically.
                    // The candidate set was authenticated above from the
                    // exact call contract and all of its hidden lanes.
                    state.identity = *destination;
                    // The candidates can have different dynamic extents.  A
                    // chosen carrier therefore cannot inherit candidate zero's
                    // counted header fact: subsequent use must load and guard
                    // the current selected root's extent before a slice read.
                    state.extent = self.parameter_extent(*layout)?;
                    state.ownership = Ownership::Shared;
                    self.state.objects.insert(*destination, state);
                }
                ResultContract::DisjunctiveWriteBackSelector {
                    carrier_result,
                    parameters: candidates,
                    selector_parameter,
                } => {
                    let Ok(carrier) = usize::try_from(*carrier_result) else {
                        return self.unsupported("Call(disjunctive selector result index)");
                    };
                    if self.function.value_types.get(destination) != Some(&ValueType::Bool)
                        || candidates.len() != 2
                        || !candidates.windows(2).all(|pair| pair[0] < pair[1])
                        || !matches!(parameters.get(*selector_parameter as usize), Some(ParameterContract {
                            value_type: ValueType::Bool,
                            ownership: ParameterOwnership::Value,
                        }))
                        || !matches!(results.get(carrier), Some(ResultContract::AliasesOneOfWriteBackParameters {
                            parameters: carrier_candidates, ..
                        }) if carrier_candidates == candidates)
                    {
                        return self.unsupported("Call(disjunctive selector contract)");
                    }
                }
                ResultContract::StaticData { layout } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            owner_generation: 0,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Shared,
                            allocation_class: None,
                            shared_immutable: false,
                            initialized_owned_slots: false,
                        },
                    );
                    self.state.static_data_objects.insert(*destination);
                }
                _ => return self.unsupported("Call(memory result)"),
            }
        }
        if let Some((borrowed, _)) = pending_borrow {
            self.state.pending_affine_borrow = None;
            self.state.affine_plans.remove(&borrowed);
            self.state.objects.get_mut(&borrowed).expect("checked borrowed slot").ownership = Ownership::Consumed;
        }
        if pending_field.is_some() {
            self.finish_affine_field_borrow();
        }
        Ok(())
    }

    pub(crate) fn check_exit(&self, returned: &[ValueId]) -> Result<(), InitialCheckError> {
        for (object, state) in &self.state.objects {
            if state.ownership != Ownership::Consumed
                && !self.is_borrowed_parameter(*object)
                && (state.allocation_class == Some(AllocationClass::ReferenceCounted)
                    || state.shared_immutable)
                && !returned.contains(object)
            {
                return Err(InitialCheckError::UnconsumedReference {
                    function: self.function.id,
                    object: *object,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn check_return(
        &self,
        values: &[ValueId],
        results: &[ResultContract],
    ) -> Result<(), InitialCheckError> {
        let mut transferred = SortedSet::new();
        for (result_index, (value, result)) in values.iter().zip(results).enumerate() {
            if let Some(plan) = self.affine_abi
                .and_then(|proofs| proofs.get(&self.signature.id))
                .and_then(|proof| proof.results.get(result_index))
                .and_then(Option::as_ref)
            {
                if !matches!(result, ResultContract::Unique { .. }) || !self.require_affine_plan(*value, plan)? {
                    return self.unsupported("Return(affine result without recursive proof)");
                }
            }
            // Each result lane transfers one token. Repeating an SSA reference
            // does not execute CloneRef, even when both lanes claim Shared.
            if matches!(value_type(self.function, *value)?, ValueType::Ref(_))
                && !matches!(result, ResultContract::StaticData { .. })
                && !transferred.insert(*value)
            {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: *value,
                });
            }
            match result {
                ResultContract::Value(value_type) if !value_type.is_linear_memory_ref() => {}
                ResultContract::OwnedGc { .. } => {}
                ResultContract::DisjunctiveWriteBackSelector {
                    carrier_result,
                    parameters: candidates,
                    selector_parameter,
                } => {
                    let selector = usize::try_from(*selector_parameter).ok();
                    let carrier = usize::try_from(*carrier_result).ok();
                    let entry = self.function.blocks.iter().find(|block| block.id == self.function.entry)
                        .ok_or(InitialCheckError::MissingBlock {
                            function: self.function.id,
                            block: self.function.entry,
                        })?;
                    let selector_value = selector
                        .and_then(|index| entry.parameters.get(index))
                        .map(|parameter| parameter.value);
                    if self.function.value_types.get(value) != Some(&ValueType::Bool)
                        || selector_value != Some(*value)
                        || candidates.len() != 2
                        || !candidates.windows(2).all(|pair| pair[0] < pair[1])
                        || !matches!(self.signature.contract.parameters.get(*selector_parameter as usize), Some(ParameterContract {
                            value_type: ValueType::Bool,
                            ownership: ParameterOwnership::Value,
                        }))
                        || !matches!(carrier.and_then(|index| results.get(index)), Some(ResultContract::AliasesOneOfWriteBackParameters {
                            parameters: carrier_candidates, ..
                        }) if carrier_candidates == candidates)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                }
                ResultContract::CheckedValue { layout } => {
                    let state = self.live_object(*value)?;
                    if state.layout != *layout
                        || !matches!(state.ownership, Ownership::Unique | Ownership::Shared)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                    self.require_complete_value_initialized(*value, "Return(uninitialized owned slot)")?;
                }
                ResultContract::Fresh { layout, class, extent } => {
                    let state = self.live_object(*value)?;
                    if state.layout != *layout
                        || state.ownership != Ownership::Unique
                        || state.allocation_class != Some(*class)
                        || !returned_extent_matches(&state.extent, extent)
                        || !self.is_sole_live_token(*value, state.identity)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                    self.require_complete_value_initialized(*value, "Return(uninitialized owned slot)")?;
                }
                ResultContract::StaticData { layout } => {
                    let state = self.live_object(*value)?;
                    if state.layout != *layout
                        || state.ownership != Ownership::Shared
                        || !self.state.static_data_objects.contains(value)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                }
                ResultContract::Unique { layout } => {
                    let state = self.live_object(*value)?;
                    if state.layout != *layout || state.ownership != Ownership::Unique
                        || state.allocation_class != Some(AllocationClass::ReferenceCounted)
                        || !self.is_sole_live_token(*value, state.identity)
                    {
                        return Err(InitialCheckError::InvalidOwnership { function: self.function.id, object: *value });
                    }
                    self.require_complete_value_initialized(*value, "Return(uninitialized unique write-back)")?;
                }
                ResultContract::ReallocatesParameter { parameter, layout } => {
                    let index = usize::try_from(*parameter).map_err(|_| {
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id }
                    })?;
                    let Some(ParameterContract {
                        value_type: ValueType::Ref(parameter_layout),
                        ownership: ParameterOwnership::WriteBack,
                    }) = self.signature.contract.parameters.get(index) else {
                        return Err(InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id });
                    };
                    let state = self.live_object(*value)?;
                    if *parameter_layout != *layout
                        || write_back_hidden_lane(results, *parameter, &self.signature.contract.parameters)
                            != Some(result_index)
                        || state.layout != *layout
                        || state.ownership != Ownership::Unique
                        || state.allocation_class != Some(AllocationClass::ReferenceCounted)
                        || !self.is_sole_live_token(*value, state.identity)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                    self.require_complete_value_initialized(*value, "Return(uninitialized reallocated write-back)")?;
                }
                ResultContract::Shared { layout } => {
                    let state = self.live_object(*value)?;
                    // A procedure with no source reference result returns its
                    // write-back replacement in the terminal ABI lane.  That
                    // lane has an exact parameter ordinal and root layout,
                    // but (unlike an alias carrier) is allowed to replace the
                    // parameter's allocation identity.
                    let terminal_write_back = terminal_write_back_parameter_for_hidden_lane(
                        results,
                        result_index,
                        &self.signature.contract.parameters,
                    );
                    if let Some((parameter, alias_layout)) = write_back_alias_parameter_for_hidden_lane(
                        results,
                        result_index,
                        &self.signature.contract.parameters,
                    ) {
                        let index = usize::try_from(parameter).map_err(|_| {
                            InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id }
                        })?;
                        let contract = self.signature.contract.parameters.get(index).ok_or(
                            InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id },
                        )?;
                        let entry = self.function.blocks.iter().find(|block| block.id == self.function.entry)
                            .ok_or(InitialCheckError::MissingBlock {
                                function: self.function.id,
                                block: self.function.entry,
                            })?;
                        let parameter_value = entry.parameters.get(index).ok_or(
                            InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id },
                        )?.value;
                        let direct_identity = self.state.objects.get(&parameter_value)
                            .is_some_and(|parameter_state| state.identity == parameter_state.identity);
                        // The two-root carrier retains a separate hidden
                        // write-back lane for every candidate. A CFG join may
                        // rename that lane, but only the exact singleton
                        // origin of its declared input is accepted; a union,
                        // unknown origin, or unrelated Shared result remains
                        // invalid.
                        let disjunctive_hidden_lane = results.iter().enumerate().any(|(carrier_index, result)| {
                            let ResultContract::AliasesOneOfWriteBackParameters {
                                parameters: candidates,
                                ..
                            } = result else { return false };
                            candidates.binary_search(&parameter).is_ok()
                                && matches!(results.get(carrier_index + 1), Some(ResultContract::DisjunctiveWriteBackSelector {
                                    carrier_result,
                                    parameters: selector_candidates,
                                    ..
                                }) if *carrier_result as usize == carrier_index
                                    && selector_candidates == candidates)
                        });
                        let rebound_exact_identity = disjunctive_hidden_lane
                            && self.state.shared_phi_origins.get(value)
                                == Some(&SortedSet::from([parameter_value]));
                        if *layout != alias_layout
                            || contract.ownership != ParameterOwnership::WriteBack
                            || contract.value_type != ValueType::Ref(*layout)
                            || !(direct_identity || rebound_exact_identity || terminal_write_back.is_some())
                        {
                            return Err(InitialCheckError::InvalidOwnership {
                                function: self.function.id,
                                object: *value,
                            });
                        }
                    }
                    // Shared owns either an RC token or immutable static data.
                    // The union never grants mutation or reclamation authority.
                    if state.layout != *layout
                        || !matches!(state.ownership, Ownership::Unique | Ownership::Shared)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                    if !self.is_shared_immutable_storage(*value, state) {
                        return Err(InitialCheckError::InvalidAllocationClass {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                    self.require_complete_value_initialized(*value, "Return(uninitialized owned slot)")?;
                }
                ResultContract::AliasesParameter { parameter, layout } => {
                    let index = usize::try_from(*parameter).map_err(|_| {
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id }
                    })?;
                    let contract = self.signature.contract.parameters.get(index).ok_or(
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id },
                    )?;
                    let entry = self.function.blocks.iter().find(|block| block.id == self.function.entry)
                        .ok_or(InitialCheckError::MissingBlock {
                            function: self.function.id,
                            block: self.function.entry,
                        })?;
                    let parameter_value = entry.parameters.get(index).ok_or(
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id },
                    )?.value;
                    let state = self.live_object(*value)?;
                    let parameter_state = self.state.objects.get(&parameter_value).ok_or(
                        InitialCheckError::InvalidOwnership { function: self.function.id, object: *value },
                    )?;
                    if contract.ownership != ParameterOwnership::Value
                        || contract.value_type != ValueType::Ref(*layout)
                        || state.layout != *layout
                        || state.identity != parameter_state.identity
                        || !matches!(state.ownership, Ownership::Unique | Ownership::Shared)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                    self.require_complete_value_initialized(*value, "Return(borrowed alias)")?;
                }
                ResultContract::AliasesWriteBackParameter { parameter, layout, projection } => {
                    let index = usize::try_from(*parameter).map_err(|_| {
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id }
                    })?;
                    let contract = self.signature.contract.parameters.get(index).ok_or(
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id },
                    )?;
                    let entry = self.function.blocks.iter().find(|block| block.id == self.function.entry)
                        .ok_or(InitialCheckError::MissingBlock {
                            function: self.function.id,
                            block: self.function.entry,
                        })?;
                    let parameter_value = entry.parameters.get(index).ok_or(
                        InitialCheckError::UnsupportedMemorySignature { signature: self.signature.id },
                    )?.value;
                    let state = self.live_object(*value)?;
                    let parameter_state = self.state.objects.get(&parameter_value).ok_or(
                        InitialCheckError::InvalidOwnership { function: self.function.id, object: *value },
                    )?;
                    let projected = projection.is_some();
                    if contract.ownership != ParameterOwnership::WriteBack
                        || (!projected && contract.value_type != ValueType::Ref(*layout))
                        || (projected && !matches!(contract.value_type, ValueType::Ref(_)))
                        || !has_distinct_write_back_lane(results, result_index, *parameter, *layout, &self.signature.contract.parameters)
                        || state.layout != *layout
                        || (!projected && state.identity != parameter_state.identity)
                        || !matches!(state.ownership, Ownership::Unique | Ownership::Shared)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                    self.require_complete_value_initialized(*value, "Return(exclusive alias)")?;
                }
                ResultContract::AliasesOneOfWriteBackParameters { parameters: candidates, layout } => {
                    if candidates.len() < 2
                        || !candidates.windows(2).all(|pair| pair[0] < pair[1])
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                    let entry = self.function.blocks.iter().find(|block| block.id == self.function.entry)
                        .ok_or(InitialCheckError::MissingBlock {
                            function: self.function.id,
                            block: self.function.entry,
                    })?;
                    let state = self.live_object(*value)?;
                    // A disjunctive carrier is a CFG phi, so its current SSA
                    // identity is deliberately the phi definition rather than
                    // either physical root.  The checker-owned origin set is
                    // the only authority that can recover those roots: it
                    // must be exactly the declaration's authenticated
                    // candidate set, never merely overlap it.
                    let mut candidate_identities = SortedSet::new();
                    let candidates_match = candidates.iter().all(|parameter| {
                        let Ok(index) = usize::try_from(*parameter) else { return false };
                        let Some(ParameterContract {
                            value_type: ValueType::Ref(parameter_layout),
                            ownership: ParameterOwnership::WriteBack,
                        }) = self.signature.contract.parameters.get(index) else { return false };
                        let Some(parameter_value) = entry.parameters.get(index) else {
                            return false;
                        };
                        if *parameter_layout != *layout
                            || parameter_value.value_type != ValueType::Ref(*layout)
                            || !has_distinct_write_back_lane(
                                results,
                                result_index,
                                *parameter,
                                *layout,
                                &self.signature.contract.parameters,
                            )
                        {
                            return false;
                        }
                        // CFG rebinding can rename the live write-back
                        // values, but a phi origin is always the immutable
                        // entry definition.  Compare the contract's roots at
                        // that same stable identity rather than treating a
                        // later token name as a third candidate.
                        candidate_identities.insert(parameter_value.value)
                    });
                    let origins = self.state.shared_phi_origins.get(value);
                    if !candidates_match
                        || candidate_identities.len() != candidates.len()
                        || origins != Some(&candidate_identities)
                        || state.layout != *layout
                        || state.ownership != Ownership::Shared
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                    self.require_complete_value_initialized(*value, "Return(disjunctive exclusive alias)")?;
                }
                _ => return self.unsupported("Return(memory result contract)"),
            }
        }
        Ok(())
    }

    pub(crate) fn check_host_buffer(
        &mut self,
        place: &Place,
        extent: &crate::canonical_safewir::HostBufferExtent,
        import: crate::canonical_safewir::ImportId,
        parameter: usize,
    ) -> Result<(), InitialCheckError> {
        let Projection::Slice { start, length } = place.projection() else {
            return Err(InitialCheckError::UnsupportedHostBuffer {
                function: self.function.id,
                import,
                parameter,
            });
        };
        self.promote_header_extent(place.object())?;
        let object = self.live_object(place.object())?.clone();
        self.require_ref_type(place.object(), object.layout)?;
        if place.value_type() != ValueType::Ref(object.layout) {
            return Err(InitialCheckError::TypeMismatch {
                function: self.function.id,
                value: place.object(),
                expected: ValueType::Ref(object.layout),
                actual: place.value_type(),
            });
        }
        if place.access() == AccessMode::Write {
            if object.ownership != Ownership::Unique
                || self.state.static_data_objects.contains(&place.object())
            {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: place.object(),
                });
            }
        }
        self.require_type(*start, ValueType::Size)?;
        self.require_type(*length, ValueType::Size)?;
        let Extent::Counted { count, capacity, stride_bytes, .. } = object.extent else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        };
        if !((stride_bytes == 1 && self.is_raw_byte_buffer(object.layout)?)
            || (stride_bytes == 8 && self.is_int_buffer(object.layout)?)) {
            return Err(InitialCheckError::UnsupportedHostBuffer {
                function: self.function.id,
                import,
                parameter,
            });
        }
        // A host read stays inside the logical length; a host write lands
        // inside the capacity, and the guest publishes the written length
        // afterwards under its own capacity guard.
        let logical_count = if place.access() == AccessMode::Write {
            self.state
                .header_values
                .get(&(place.object(), HeaderField::Capacity))
                .copied()
                .unwrap_or(capacity)
        } else {
            self.state
                .header_values
                .get(&(place.object(), HeaderField::Length))
                .copied()
                .unwrap_or(count)
        };
        let starts_at_zero = self.state.constants.get(start) == Some(&0);
        let whole_logical_body = starts_at_zero && self.same_scalar(*length, logical_count);
        let constant_range = self
            .state
            .constants
            .get(start)
            .zip(self.state.constants.get(length))
            .zip(self.state.constants.get(&logical_count))
            .is_some_and(|((start, length), count)| {
                start.checked_add(*length).is_some_and(|end| end <= *count)
            });
        if !whole_logical_body && !constant_range {
            return Err(InitialCheckError::MissingBoundsGuard {
                function: self.function.id,
                index: *start,
                count: logical_count,
            });
        }
        let constant_bytes = self
            .state
            .constants
            .get(length)
            .and_then(|length| length.checked_mul(u64::from(stride_bytes)));
        match extent {
            crate::canonical_safewir::HostBufferExtent::Slice => {}
            crate::canonical_safewir::HostBufferExtent::Exact(bytes)
                if constant_bytes == Some(*bytes) => {}
            crate::canonical_safewir::HostBufferExtent::AtLeast(bytes)
                if constant_bytes.is_some_and(|actual| actual >= *bytes) => {}
            crate::canonical_safewir::HostBufferExtent::Exact(_)
            | crate::canonical_safewir::HostBufferExtent::AtLeast(_)
            | crate::canonical_safewir::HostBufferExtent::Unknown => {
                return Err(InitialCheckError::HostBufferExtentMismatch {
                    function: self.function.id,
                    import,
                    parameter,
                });
            }
        }
        if place.access() == AccessMode::Read {
            let element = self.descriptor(object.layout)?.kind().packed_element_layout()
                .ok_or(InitialCheckError::UnsupportedHostBuffer {
                    function: self.function.id,
                    import,
                    parameter,
                })?;
            self.require_initialized_scalar_range(
                CheckedRange {
                    object: place.object(), start: *start, length: *length,
                    layout: object.layout, element, identity: object.identity,
                    ownership: object.ownership, allocation_class: object.allocation_class,
                },
                "HostCall(read uninitialized scalar range)",
            )?;
        }
        Ok(())
    }

    /// A write buffer grants bounded mutation, not initialized capacity. The
    /// authenticated size result names the exact initialized prefix; a later
    /// checked UpdateCount publishes it to guest reads.
    pub(crate) fn record_host_written_prefix(
        &mut self,
        place: &Place,
        written: ValueId,
    ) -> Result<(), InitialCheckError> {
        self.require_type(written, ValueType::Size)?;
        let Projection::Slice { start, length: _ } = place.projection() else {
            return self.unsupported("HostCall(write prefix projection)");
        };
        let object = self.live_object(place.object())?.clone();
        let Extent::Counted { .. } = object.extent else {
            return self.unsupported("HostCall(write prefix extent)");
        };
        if !self.is_raw_byte_buffer(object.layout)? && !self.is_int_buffer(object.layout)? {
            return self.unsupported("HostCall(write prefix contract)");
        }
        let element = self.descriptor(object.layout)?.kind().packed_element_layout()
            .ok_or(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            })?;
        // The authenticated write contract defines its sole Size result as
        // the number of bytes initialized from the supplied slice's start.
        // Do not turn the slice length or backing capacity into initialized
        // storage: only the host-published result receives that authority.
        self.mark_initialized_scalar_range(CheckedRange {
            object: place.object(), start: *start, length: written,
            layout: object.layout, element, identity: object.identity,
            ownership: object.ownership, allocation_class: object.allocation_class,
        });
        Ok(())
    }
}
