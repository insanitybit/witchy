use super::*;

impl<'a> MemoryChecker<'a> {
    /// Rename facts through an explicit SSA edge.  A block parameter is a new
    /// definition, so any count, capacity, header value, guard, or object state
    /// carried by an argument must be expressed in terms of that definition.
    pub(crate) fn transfer_edge(
        &mut self,
        target: &BasicBlock,
        arguments: &[ValueId],
    ) -> Result<MemoryState, InitialCheckError> {
        // A managed phi needs the argument's counted extent, which promotion
        // derives lazily from the header loads already in this state. Derive
        // it now from the same facts; a promotion the facts do not justify is
        // left alone and the extent rule below reports it.
        let mut prefix_candidates = SortedSet::new();
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if matches!(parameter.ownership, Some(Ownership::Shared | Ownership::Unique))
                && self.state.objects.contains_key(argument)
            {
                let _ = self.promote_header_extent(*argument);
                let outgoing = &self.state.objects[argument];
                prefix_candidates.extend(self.state.objects.iter().filter_map(|(value, state)| {
                    (state.identity == outgoing.identity
                        && state.layout == outgoing.layout
                        && state.allocation_class == outgoing.allocation_class
                        && state.shared_immutable == outgoing.shared_immutable
                        && matches!(state.ownership, Ownership::Unique | Ownership::Shared))
                        .then_some(*value)
                }));
            }
        }
        // A retained CloneRef is not renamed with its outgoing sibling, but
        // its detailed prefix evidence can still be lost by the later meet.
        // Identity only selects candidates: independently check each token's
        // own extent and all slot/range obligations in this predecessor. Do
        // not infer completeness from overlapping phi origins or another
        // token's summary, and finish every check before publishing summaries.
        let complete_prefixes = prefix_candidates.into_iter().filter(|value| {
            self.require_complete_value_initialized(*value, "managed phi(uninitialized owned slot)").is_ok()
        }).collect::<Vec<_>>();
        for value in complete_prefixes {
            self.state.objects.get_mut(&value).expect("candidate exists").initialized_owned_slots = true;
        }
        let mut renames = SortedMap::new();
        let mut unique_phi_rebases = SortedMap::new();
        let mut shared_phi_rebases = SortedMap::new();
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            // GC owner lanes are proved by the linear GC checker and its
            // owned-origin flow. This rebase is only for linear-memory Ref
            // allocations, which have an authenticated physical identity.
            if parameter.ownership == Some(Ownership::Unique)
                && !matches!(parameter.value_type, ValueType::Gc(_))
            {
                if !matches!(parameter.value_type, ValueType::Ref(_)) {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: parameter.value,
                    });
                }
                let state = self.state.objects.get(argument).ok_or(
                    InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    },
                )?;
                if state.ownership != Ownership::Unique
                    || !self.is_sole_live_token(*argument, state.identity)
                {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!("canonical unique phi refused at {:?}: argument {argument:?} state {state:#?}", target.id);
                    }
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    });
                }
                unique_phi_rebases.insert(*argument, parameter.value);
            }
            if parameter.ownership == Some(Ownership::Shared) && !matches!(parameter.value_type, ValueType::Gc(_)) {
                let ValueType::Ref(parameter_layout) = parameter.value_type else {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: parameter.value,
                    });
                };
                let state = self.state.objects.get(argument).ok_or(
                    InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    },
                )?;
                // A unique reference-counted token may weaken to shared at a
                // shared phi, as a call weakens a unique argument into a
                // shared parameter: the object keeps its one token and loses
                // its write authority. Only a sole live token may weaken.
                let weakenable = state.ownership == Ownership::Unique
                    && state.allocation_class == Some(AllocationClass::ReferenceCounted)
                    && self.is_sole_live_token(*argument, state.identity);
                if !(state.ownership == Ownership::Shared || weakenable) || state.layout != parameter_layout {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!("canonical shared phi refused at {:?}: argument {argument:?} state {state:#?}", target.id);
                    }
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    });
                }
                shared_phi_rebases.insert(*argument, parameter.value);
            }
            if self.state.objects.contains_key(argument)
                && renames.insert(*argument, parameter.value).is_some_and(|previous| previous != parameter.value)
            {
                if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                    eprintln!("canonical edge refused at {:?}: object {argument:?} is passed in two lanes", target.id);
                }
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: *argument,
                });
            }
            renames.insert(*argument, parameter.value);
        }

        // A checked header read can name the same logical count with a fresh
        // SSA definition. Transport that equality, not an optimizer annotation.
        // Capacity header reads are deliberately excluded: a shared meet may
        // retain only a conservative capacity bound rather than physical capacity.
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if parameter.ownership != Some(Ownership::Shared) {
                continue;
            }
            let Some(state) = self.state.objects.get(argument) else { continue };
            let Extent::Counted { count, capacity, .. } = state.extent else { continue };
            for bound in [count, capacity] {
                if renames.contains_key(&bound) {
                    continue;
                }
                let replacement = arguments.iter().zip(&target.parameters).find(|(source, target)| {
                    target.value_type == ValueType::Size
                        && (self.state.constants.get(&bound).zip(self.state.constants.get(source))
                            .is_some_and(|(left, right)| left == right)
                            || (bound == count
                                && self.state.header_values.get(&(*argument, HeaderField::Length)) == Some(*source)))
                });
                if let Some((_, replacement)) = replacement {
                    renames.insert(bound, replacement.value);
                }
            }
        }

        // Object phis may rebase a sole token's allocation identity without
        // representing that change as an ordinary SSA value rename. Owner-
        // generation facts name physical allocation identities, so carry the
        // exact root rebase into those facts as well.
        //
        // A checked shared result carrier can be one of several physical
        // write-back roots. On each predecessor it is another phi for that
        // predecessor's root, but after the CFG meet it intentionally has its
        // own abstract SSA identity and an origin set. It must not acquire
        // either root's mutable-owner facts. When one identity reaches both a
        // direct root lane and such a carrier, preserve facts on the direct
        // root lane only. If the direct identity is not one of the candidate
        // lanes, no exact physical rebase is available and validation remains
        // fail-closed. Multiple Unique candidates remain invalid: a unique
        // physical allocation cannot be represented by more than one live
        // token.
        let mut owner_identity_candidates = SortedMap::<ValueId, (SortedSet<ValueId>, bool)>::new();
        for (argument, parameter) in &unique_phi_rebases {
            let identity = self.state.objects[argument].identity;
            let entry = owner_identity_candidates
                .entry(identity)
                .or_insert_with(|| (SortedSet::new(), false));
            entry.0.insert(*parameter);
            entry.1 = true;
        }
        for (argument, parameter) in &shared_phi_rebases {
            let identity = self.state.objects[argument].identity;
            owner_identity_candidates
                .entry(identity)
                .or_insert_with(|| (SortedSet::new(), false))
                .0
                .insert(*parameter);
        }
        let mut owner_identity_rebases = SortedMap::new();
        for (identity, (candidates, has_unique_candidate)) in owner_identity_candidates {
            let direct = remap_value(&renames, identity);
            if candidates.len() != 1 && has_unique_candidate {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: direct,
                });
            }
            let rebase = if candidates.len() == 1 {
                *candidates.first().expect("one candidate exists")
            } else if candidates.contains(&direct) {
                direct
            } else {
                // The shared value is carried by more than one target lane
                // and no target lane is its direct SSA root. It may be a
                // selected result alongside a write-back replacement. No
                // exact owner-identity rebase exists, so leave all old owner
                // facts stale rather than attaching them to either lane.
                continue;
            };
            owner_identity_rebases.insert(identity, rebase);
        }
        let mut objects = SortedMap::new();
        for (value, state) in &self.state.objects {
            let original_value = *value;
            let mut state = state.clone();
            state.extent = remap_extent(&renames, &state.extent);
            let value = remap_value(&renames, original_value);
            if let Some(identity) = unique_phi_rebases.get(&original_value) {
                state.identity = *identity;
            }
            if let Some(identity) = shared_phi_rebases.get(&original_value) {
                state.identity = *identity;
                if state.ownership == Ownership::Unique {
                    state.ownership = Ownership::Shared;
                }
                // A dynamic shared phi can join an old promoted edge with a
                // HeaderPrefix edge. Give its rebased owner the stable joined
                // generation from the first visit, but carry no proof into
                // that generation. A later header reload is therefore the
                // only way to authenticate a counted access.
                if matches!(self.descriptor(state.layout)?.size(), LayoutSize::Dynamic { .. }) {
                    state.owner_generation = JOINED_OWNER_GENERATION;
                }
                match &state.extent {
                    Extent::Counted { count, capacity, .. }
                        if target.parameters.iter().any(|parameter| {
                            parameter.value == *count && parameter.value_type == ValueType::Size
                        }) && target.parameters.iter().any(|parameter| {
                            parameter.value == *capacity
                                && parameter.value_type == ValueType::Size
                        }) => {}
                    // A header prefix is deliberately not a counted extent:
                    // its checked fact is only that the allocation holds the
                    // dynamic layout's fixed header.  It therefore has no
                    // count or capacity SSA lane to carry across this edge.
                    // Detailed observations are discarded below, so a later
                    // access must reload and authenticate its own bounds.
                    Extent::HeaderPrefix { .. } => {}
                    Extent::FixedBytes(_) => {}
                    extent => {
                        if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                            eprintln!("canonical shared phi refused at {:?}: object {value:?} extent {extent:?} is not carried by the parameters", target.id);
                        }
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: value,
                        });
                    }
                }
            }
            if let Some(previous) = objects.insert(value, state.clone()) {
                if previous != state {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!("canonical edge refused at {:?}: {value:?} renamed from two objects: {previous:#?} vs {state:#?}", target.id);
                    }
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: value,
                    });
                }
            }
        }
        let shared_prefix_phi_values = shared_phi_rebases
            .values()
            .copied()
            .filter(|value| {
                matches!(
                    objects.get(value).map(|state| &state.extent),
                    Some(Extent::HeaderPrefix { .. })
                )
            })
            .collect::<SortedSet<_>>();
        // A counted shared phi has an explicit two-lane transport contract.
        // Its predecessor bounds cannot name the rebased owner generation,
        // but the exact count and capacity lanes themselves may do so.  Keep
        // this deliberately narrower than a general scalar fact transfer:
        // HeaderPrefix phis have no lanes and remain proofless below.
        let shared_counted_phi_lanes = shared_phi_rebases
            .values()
            .filter_map(|value| {
                let state = objects.get(value)?;
                let Extent::Counted { count, capacity, .. } = state.extent else {
                    return None;
                };
                let is_size_parameter = |lane| target.parameters.iter().any(|parameter| {
                    parameter.value == lane && parameter.value_type == ValueType::Size
                });
                (is_size_parameter(count) && is_size_parameter(capacity)).then_some((
                    *value,
                    count,
                    capacity,
                    OwnerGeneration {
                        identity: state.identity,
                        epoch: state.owner_generation,
                    },
                ))
            })
            .collect::<Vec<_>>();
        let canonical_identities = objects
            .iter()
            .map(|(value, state)| (*value, state.identity))
            .collect::<SortedMap<_, _>>();
        let remap_owner_identity = |identity| {
            let token = owner_identity_rebases
                .get(&identity)
                .copied()
                .unwrap_or_else(|| remap_value(&renames, identity));
            canonical_identities.get(&token).copied().unwrap_or(token)
        };
        for parameter in &target.parameters {
            // Only a linear-memory reference carries object state across an
            // edge. Capabilities, GC references, and callables are opaque host
            // or table references with no extent or ownership to rename, so a
            // loop that threads `console` through its header is not a missing
            // object (BUG: capability-in-loop refusal, census B4).
            if matches!(parameter.value_type, ValueType::Ref(_)) && !objects.contains_key(&parameter.value) {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: parameter.value,
                });
            }
        }

        let mut shared_phi_origins = self
            .state
            .shared_phi_origins
            .iter()
            .map(|(value, origins)| (remap_value(&renames, *value), origins.clone()))
            .collect::<SortedMap<_, _>>();
        for (argument, parameter) in &shared_phi_rebases {
            let origins = self
                .state
                .shared_phi_origins
                .get(argument)
                .cloned()
                .unwrap_or_else(|| SortedSet::from([self.state.objects[argument].identity]));
            shared_phi_origins
                .entry(*parameter)
                .or_default()
                .extend(origins);
        }

        let mut edge = MemoryState {
            objects: objects.into(),
            shared_phi_origins: shared_phi_origins.into(),
            // A phi rebase belongs only to the scalar evidence that crosses
            // the edge with that reference. An uncarried `let` snapshot may
            // name the same physical root as a `var` loop lane, but it keeps
            // its own header/load SSA values and must retain their original
            // root identity. Rebinding every fact for the physical identity
            // would make that stable snapshot's current bound appear stale
            // after the loop, even though the mutation path copied before it
            // wrote. No evidence is invented here: carried facts still rebase
            // exactly as before, while uncarried ones remain tied to the
            // unchanged allocation.
            bound_generations: self.state.bound_generations.iter().map(|(value, facts)| {
                let carried = remap_value(&renames, *value) != *value;
                (
                    remap_value(&renames, *value),
                    facts.iter().map(|fact| OwnerGeneration {
                        identity: carried.then(|| remap_owner_identity(fact.identity))
                            .unwrap_or(fact.identity),
                        epoch: fact.epoch,
                    }).collect(),
                )
            }).collect(),
            guard_generations: self.state.guard_generations.iter().map(|((operator, left, right), facts)| {
                let carried = remap_value(&renames, *left) != *left
                    || remap_value(&renames, *right) != *right;
                (
                    (*operator, remap_value(&renames, *left), remap_value(&renames, *right)),
                    facts.iter().map(|fact| OwnerGeneration {
                        identity: carried.then(|| remap_owner_identity(fact.identity))
                            .unwrap_or(fact.identity),
                        epoch: fact.epoch,
                    }).collect(),
                )
            }).collect(),
            fresh_header_lengths: self.state.fresh_header_lengths.iter().map(|(value, facts)| {
                let carried = remap_value(&renames, *value) != *value;
                (
                    remap_value(&renames, *value),
                    facts.iter().map(|fact| OwnerGeneration {
                        identity: carried.then(|| remap_owner_identity(fact.identity))
                            .unwrap_or(fact.identity),
                        epoch: fact.epoch,
                    }).collect(),
                )
            }).collect(),
            initialized_scalar_slots: self
                .state
                .initialized_scalar_slots
                .iter()
                .map(|(container, index)| (remap_value(&renames, *container), match index { SlotIdentity::Constant(n) => SlotIdentity::Constant(*n), SlotIdentity::Dynamic(value) => SlotIdentity::Dynamic(remap_value(&renames, *value)) }))
                .collect(),
            stored_objects: self
                .state
                .stored_objects
                .iter()
                .map(|((container, index), state)| {
                    (
                        (
                            remap_value(&renames, *container),
                            match index {
                                SlotIdentity::Constant(value) => SlotIdentity::Constant(*value),
                                SlotIdentity::Dynamic(value) => {
                                    SlotIdentity::Dynamic(remap_value(&renames, *value))
                                }
                            },
                        ),
                        state.clone(),
                    )
                })
                .collect(),
            nullable_unique_slots: self.state.nullable_unique_slots.iter().map(|(container, slot)| {
                (remap_value(&renames, *container), *slot)
            }).collect(),
            nullable_present_slots: self.state.nullable_present_slots.iter().map(|(container, slot)| {
                (remap_value(&renames, *container), *slot)
            }).collect(),
            affine_plans: self.state.affine_plans.iter().map(|(root, plan)|
                (remap_value(&renames, *root), plan.clone())).collect(),
            affine_list_roots: self.state.affine_list_roots.iter().map(|root| remap_value(&renames, *root)).collect(),
            nullable_absent_slots: self.state.nullable_absent_slots.iter().map(|(container, slot)| {
                (remap_value(&renames, *container), *slot)
            }).collect(),
            range_states: self
                .state
                .range_states
                .iter()
                .map(|(identity, state)| {
                    (
                        RangeIdentity {
                            object: remap_value(&renames, identity.object),
                            start: remap_value(&renames, identity.start),
                            length: remap_value(&renames, identity.length),
                        },
                        *state,
                    )
                })
                .collect(),
            full_current_prefixes: self.state.full_current_prefixes.iter().map(|identity| {
                RangeIdentity {
                    object: remap_value(&renames, identity.object),
                    start: remap_value(&renames, identity.start),
                    length: remap_value(&renames, identity.length),
                }
            }).collect(),
            constants: self
                .state
                .constants
                .iter()
                .map(|(value, constant)| (remap_value(&renames, *value), *constant))
                .collect(),
            exact_additions: self
                .state
                .exact_additions
                .iter()
                .map(|(result, (left, right))| {
                    (
                        remap_value(&renames, *result),
                        (remap_value(&renames, *left), remap_value(&renames, *right)),
                    )
                })
                .collect(),
            comparisons: self
                .state
                .comparisons
                .iter()
                .map(|(value, comparison)| {
                    (
                        remap_value(&renames, *value),
                        Comparison {
                            operator: comparison.operator,
                            left: remap_value(&renames, comparison.left),
                            right: remap_value(&renames, comparison.right),
                        },
                    )
                })
                .collect(),
            guards: self
                .state
                .guards
                .iter()
                .map(|(operator, left, right)| {
                    (*operator, remap_value(&renames, *left), remap_value(&renames, *right))
                })
                .collect(),
            header_values: self
                .state
                .header_values
                .iter()
                .map(|((object, field), value)| {
                    (
                        (remap_value(&renames, *object), *field),
                        remap_value(&renames, *value),
                    )
                })
                .collect(),
            static_data_objects: self
                .state
                .static_data_objects
                .iter()
                .map(|object| remap_value(&renames, *object))
                .collect(),
        };
        // A shared phi rebase never retains predecessor observations: a
        // HeaderPrefix phi has no scalar lanes, while a Counted phi receives
        // only its exact lanes below. Retaining an earlier header observation
        // or bounds guard would make a non-dominating fact look current at
        // the new block parameter.
        let shared_rebased_phi_values = shared_prefix_phi_values
            .iter()
            .chain(shared_counted_phi_lanes.iter().map(|(value, _, _, _)| value))
            .copied()
            .collect::<SortedSet<_>>();
        if !shared_rebased_phi_values.is_empty() {
            let shared_rebased_identities = shared_rebased_phi_values
                .iter()
                .filter_map(|value| edge.objects.get(value).map(|state| state.identity))
                .collect::<SortedSet<_>>();
            edge.header_values.retain(|(object, _), _| !shared_rebased_phi_values.contains(object));
            edge.range_states.retain(|identity, _| !shared_rebased_phi_values.contains(&identity.object));
            edge.full_current_prefixes.retain(|identity| !shared_rebased_phi_values.contains(&identity.object));
            edge.bound_generations.retain(|_, facts| {
                facts.retain(|fact| !shared_rebased_identities.contains(&fact.identity));
                !facts.is_empty()
            });
            edge.guard_generations.retain(|_, facts| {
                facts.retain(|fact| !shared_rebased_identities.contains(&fact.identity));
                !facts.is_empty()
            });
            edge.fresh_header_lengths.retain(|_, facts| {
                facts.retain(|fact| !shared_rebased_identities.contains(&fact.identity));
                !facts.is_empty()
            });
            for (_, count, capacity, generation) in shared_counted_phi_lanes {
                edge.bound_generations.entry(count).or_default().insert(generation);
                edge.bound_generations.entry(capacity).or_default().insert(generation);
            }
        }
        Ok(edge)
    }

    /// A Unique phi may change object identity only when its edge argument is
    /// the sole remaining live representation of that identity.  Container
    /// slots count: rebasing a value while the same object remains stored
    /// elsewhere would leave a physical alias under the old identity.
    pub(super) fn is_sole_live_token(&self, value: ValueId, identity: ValueId) -> bool {
        Self::is_sole_live_token_in(&self.state, value, identity)
    }

    pub(super) fn is_sole_live_token_in(edge: &MemoryState, value: ValueId, identity: ValueId) -> bool {
        edge.objects.iter().all(|(candidate, state)| {
            state.identity != identity || state.ownership == Ownership::Consumed || *candidate == value
        }) && edge.stored_objects.values().all(|state| {
            state.identity != identity || state.ownership == Ownership::Consumed
        })
    }

    /// Meet facts from predecessor edges.  Scalar facts are usable only when
    /// every predecessor establishes the same fact.  An object state is never
    /// widened: different allocation identity, ownership, layout, or extent is
    /// an explicit unsafe join rather than an inferred alias relation.
    pub(crate) fn meet(
        &self,
        block: &BasicBlock,
        existing: &MemoryState,
        incoming: &MemoryState,
    ) -> Result<MemoryState, InitialCheckError> {
        let mut objects = SortedMap::new();
        // A conditional mutation may advance only the owner's epoch while
        // both edges transport the exact same counted-extent lanes.  Those
        // lanes remain facts about the one physical allocation; a subsequent
        // guard must be allowed to authenticate them for the joined epoch.
        // Keep this separate from guard facts, which are path-local evidence.
        let mut reauthenticated_joined_extents = Vec::new();
        // Analysis facts may be intersected, but owned references are release
        // obligations. An incoming edge must not erase a live affine token.
        for (state, other) in [(existing, incoming), (incoming, existing)] {
            for (value, object) in &state.objects {
                if object.ownership != Ownership::Consumed
                    && (object.allocation_class == Some(AllocationClass::ReferenceCounted)
                        || object.shared_immutable)
                    && !other.objects.contains_key(value)
                {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!("canonical memory join at {:?}: live token {value:?} is missing on the other edge: {object:#?}", block.id);
                    }
                    return Err(InitialCheckError::IncompatibleMemoryJoin {
                        function: self.function.id,
                        block: block.id,
                        object: *value,
                    });
                }
            }
            for (slot, object) in &state.stored_objects {
                if other.stored_objects.get(slot) != Some(object) {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!("canonical memory join at {:?}: stored slot {slot:?} differs: {object:#?} vs {:#?}", block.id, other.stored_objects.get(slot));
                    }
                    return Err(InitialCheckError::IncompatibleMemoryJoin {
                        function: self.function.id,
                        block: block.id,
                        object: slot.0,
                    });
                }
            }
        }
        for (value, current) in &existing.objects {
            let Some(next) = incoming.objects.get(value) else {
                continue;
            };
            // Extent promotion is lazy (it happens at the first element or
            // slice access), so a back edge may carry the counted extent the
            // entry edge has not derived yet from the same header loads.
            // Promote both sides before comparing; a promotion that fails on
            // one side leaves that side as it was, so nothing is widened.
            let promoted_current = self.promoted_object(existing, *value).unwrap_or_else(|| current.clone());
            let promoted_next = self.promoted_object(incoming, *value).unwrap_or_else(|| next.clone());
            // Preserve a common logical count before the generic header-only
            // fallback erases it. The shared meet drops capacity precision,
            // never identity or ownership obligations.
            if promoted_current != promoted_next
                && let Some(met) = shared_phi_meet(&promoted_current, &promoted_next)
            {
                if let Some((count, capacity)) =
                    exact_carried_counted_extent(&promoted_current, &promoted_next)
                {
                    reauthenticated_joined_extents.push((
                        count,
                        capacity,
                        OwnerGeneration {
                            identity: met.identity,
                            epoch: met.owner_generation,
                        },
                    ));
                }
                objects.insert(*value, met);
                continue;
            }
            // A back edge may carry a counted extent whose count was loaded
            // inside the loop body, which the entry edge cannot know. The
            // header-prefix form is the weaker fact of the same object, so the
            // join keeps it and the body re-derives the count from its own
            // loads; nothing else about the object may differ.
            let (current, next) = match demoted_pair(&promoted_current, &promoted_next) {
                Some(pair) => {
                    if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                        eprintln!(
                            "canonical memory join at {:?} demotes {value:?}: {:?} vs {:?}",
                            block.id, promoted_current.extent, promoted_next.extent
                        );
                    }
                    pair
                }
                None => (promoted_current, promoted_next),
            };
            let (current, next) = (&current, &next);
            // Predecessors are alternatives. A clone on only one path loses
            // uniqueness, but does not create another token at the join. This
            // rule never identifies different allocations or erases a release
            // obligation; it only drops write/sole-token authority.
            let unique_edge = match (current.ownership, next.ownership) {
                (Ownership::Unique, Ownership::Shared) => Some((existing, current)),
                (Ownership::Shared, Ownership::Unique) => Some((incoming, next)),
                _ => None,
            };
            if let Some((edge, unique)) = unique_edge
                && current.identity == next.identity
                && current.layout == next.layout
                && current.extent == next.extent
                && unique.allocation_class == Some(AllocationClass::ReferenceCounted)
                && Self::is_managed_storage(current)
                && Self::is_managed_storage(next)
                && Self::is_sole_live_token_in(edge, *value, unique.identity)
            {
                let mut met = current.clone();
                met.ownership = Ownership::Shared;
                met.allocation_class = (current.allocation_class == next.allocation_class)
                    .then_some(current.allocation_class).flatten();
                met.shared_immutable = true;
                met.initialized_owned_slots = current.initialized_owned_slots && next.initialized_owned_slots;
                objects.insert(*value, met);
                continue;
            }
            // A shared phi may join a parameter-derived shared reference with
            // a weakened fresh allocation: both are managed storage, so the
            // meet keeps only what both establish (unknown class, immutable,
            // owned slots initialized only if both say so).
            // A consumed token carries no obligation; residual class or
            // immutability facts about the storage it named do not matter.
            if current != next
                && current.ownership == Ownership::Consumed
                && next.ownership == Ownership::Consumed
                && current.identity == next.identity
                && current.layout == next.layout
            {
                objects.insert(*value, current.clone());
                continue;
            }
            // A shared managed object may reach a block through different
            // paths with different class or immutability facts (a phi block,
            // or any block after one across the fixpoint's rounds); the meet
            // keeps only what both establish.
            if current != next
                && let Some(met) = shared_phi_meet(current, next)
            {
                if let Some((count, capacity)) = exact_carried_counted_extent(current, next) {
                    reauthenticated_joined_extents.push((
                        count,
                        capacity,
                        OwnerGeneration {
                            identity: met.identity,
                            epoch: met.owner_generation,
                        },
                    ));
                }
                objects.insert(*value, met);
                continue;
            }
            // A mutation on one predecessor advances this allocation's epoch,
            // even when the other predecessor leaves the exact same owned
            // storage untouched.  The alternatives still carry one physical
            // token, so rejecting the join would conflate stale *bounds
            // evidence* with an alias or release-obligation mismatch. Give
            // the joined owner a dedicated abstract epoch instead. The
            // ordinary fact intersections below retain only common facts, and
            // none can name this joined epoch; a later indexed access must
            // establish fresh bounds evidence before it is admitted. The
            // marker is stable through loop iterations, unlike a synthetic
            // numeric successor which would make dataflow diverge.
            //
            // This is intentionally an exact-state rule.  It never reconciles
            // allocation identity, layout, extent, ownership, class, or slot
            // initialization differences, and it cannot make a consumed token
            // live again.
            if let Some(met) = join_path_mutated_owner(current, next) {
                if let Some((count, capacity)) = exact_carried_counted_extent(current, next) {
                    reauthenticated_joined_extents.push((
                        count,
                        capacity,
                        OwnerGeneration { identity: met.identity, epoch: met.owner_generation },
                    ));
                }
                objects.insert(*value, met);
                continue;
            }
            if current != next {
                if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                    eprintln!(
                        "canonical memory join mismatch at {:?} for {value:?}:\n existing: {current:#?}\n incoming: {next:#?}",
                        block.id
                    );
                }
                return Err(InitialCheckError::IncompatibleMemoryJoin {
                    function: self.function.id,
                    block: block.id,
                    object: *value,
                });
            }
            if let Some((count, capacity)) = exact_carried_counted_extent(current, next) {
                reauthenticated_joined_extents.push((
                    count,
                    capacity,
                    OwnerGeneration { identity: current.identity, epoch: current.owner_generation },
                ));
            }
            objects.insert(*value, current.clone());
        }
        for parameter in &block.parameters {
            // Only a linear-memory reference carries object state across an
            // edge. Capabilities, GC references, and callables are opaque host
            // or table references with no extent or ownership to rename, so a
            // loop that threads `console` through its header is not a missing
            // object (BUG: capability-in-loop refusal, census B4).
            if matches!(parameter.value_type, ValueType::Ref(_)) && !objects.contains_key(&parameter.value) {
                return Err(InitialCheckError::IncompatibleMemoryJoin {
                    function: self.function.id,
                    block: block.id,
                    object: parameter.value,
                });
            }
        }

        let mut range_states = meet_range_states(existing, incoming);
        // Derive this relation before intersecting away predecessor-local
        // scalar facts. HeaderPrefix demotion must not erase a complete
        // prefix's Shared child status merely because its old count ID dies.
        let mut full_current_prefixes = meet_full_current_prefixes(existing, incoming, &range_states);
        self.synchronize_joined_full_prefixes(block, &objects, &mut range_states, &mut full_current_prefixes)?;
        // The met objects already carry the joined epoch, so proofs naming a
        // predecessor's concrete epoch for one of those identities are talking
        // about that same joined owner.
        let joined_identities: SortedSet<ValueId> = objects
            .values()
            .filter(|state| state.owner_generation == JOINED_OWNER_GENERATION)
            .map(|state| state.identity)
            .collect();
        let mut bound_generations = meet_generation_facts(&existing.bound_generations, &incoming.bound_generations);
        for (count, capacity, generation) in reauthenticated_joined_extents {
            bound_generations.entry(count).or_default().insert(generation);
            bound_generations.entry(capacity).or_default().insert(generation);
        }
        Ok(MemoryState {
            objects: objects.into(),
            shared_phi_origins: meet_shared_phi_origins(existing, incoming).into(),
            bound_generations: bound_generations.into(),
            guard_generations: meet_generation_facts(
                &existing.guard_generations, &incoming.guard_generations,
            ).into(),
            // Only the fresh-header observations rebase into the joined epoch;
            // see `rebase_joined_generations` for why the derived proofs above
            // must keep their concrete epochs.
            fresh_header_lengths: meet_generation_facts(
                &rebase_joined_generations(&existing.fresh_header_lengths, &joined_identities),
                &rebase_joined_generations(&incoming.fresh_header_lengths, &joined_identities),
            ).into(),
            stored_objects: meet_map(&existing.stored_objects, &incoming.stored_objects),
            nullable_unique_slots: meet_set(&existing.nullable_unique_slots, &incoming.nullable_unique_slots),
            nullable_present_slots: meet_set(&existing.nullable_present_slots, &incoming.nullable_present_slots),
            affine_plans: meet_map(&existing.affine_plans, &incoming.affine_plans),
            affine_list_roots: meet_set(&existing.affine_list_roots, &incoming.affine_list_roots),
            nullable_absent_slots: meet_set(&existing.nullable_absent_slots, &incoming.nullable_absent_slots),
            initialized_scalar_slots: meet_set(&existing.initialized_scalar_slots, &incoming.initialized_scalar_slots),
            range_states: range_states.into(),
            full_current_prefixes: full_current_prefixes.into(),
            constants: meet_map(&existing.constants, &incoming.constants),
            exact_additions: meet_map(&existing.exact_additions, &incoming.exact_additions),
            comparisons: meet_map(&existing.comparisons, &incoming.comparisons),
            guards: meet_set(&existing.guards, &incoming.guards),
            header_values: meet_map(&existing.header_values, &incoming.header_values),
            static_data_objects: meet_set(&existing.static_data_objects, &incoming.static_data_objects),
        })
    }

    pub(super) fn synchronize_joined_full_prefixes(
        &self,
        block: &BasicBlock,
        objects: &SortedMap<ValueId, ObjectState>,
        ranges: &mut SortedMap<RangeIdentity, RangeState>,
        full_prefixes: &mut SortedSet<RangeIdentity>,
    ) -> Result<(), InitialCheckError> {
        let mut replacements = Vec::new();
        let mut allocations = SortedSet::new();
        for old in full_prefixes.iter().copied() {
            let error = || InitialCheckError::IncompatibleMemoryJoin {
                function: self.function.id, block: block.id, object: old.object,
            };
            let object = objects.get(&old.object).ok_or_else(error)?;
            if !matches!(object.ownership, Ownership::Unique | Ownership::Shared) {
                continue;
            }
            let Extent::Counted { count, .. } = object.extent else { continue; };
            let range = ranges.get(&old).copied().ok_or_else(error)?;
            if !range.initialized || !range.zero_start
                || !matches!(range.ownership, Ownership::Unique | Ownership::Shared)
                || !allocations.insert(object.identity)
            {
                return Err(error());
            }
            let fresh = RangeIdentity { length: count, ..old };
            if fresh != old && ranges.contains_key(&fresh) {
                return Err(error());
            }
            replacements.push((old, fresh, range));
        }
        // Object-only speculative promotion above can select a Counted extent
        // while the range meet retains a predecessor's old length SSA name.
        // Only the full-prefix proof established by BOTH ORIGINAL edges lets
        // us synchronize that range to the checked joined count. No scalar
        // equality is inferred, and partial prefixes are never retagged.
        // Validate every replacement first: a collision or multiple complete
        // prefixes for one allocation must not silently erase an obligation.
        for (old, fresh, range) in replacements {
            if fresh != old {
                ranges.remove(&old);
                ranges.insert(fresh, range);
                full_prefixes.remove(&old);
                full_prefixes.insert(fresh);
            }
        }
        Ok(())
    }

    /// `object`'s state in `state` after header-extent promotion, when the
    /// facts in `state` justify it.
    // (see `demoted_pair` below for the inverse direction at a join)
    pub(super) fn promoted_object(&self, state: &MemoryState, object: ValueId) -> Option<ObjectState> {
        if !matches!(state.objects.get(&object)?.extent, Extent::HeaderPrefix { .. }) {
            return None;
        }
        // Reject missing header/guard evidence against the borrowed edge,
        // before allocating scratch storage. These are the same equations
        // used by ordinary promotion, not facts imported from another edge.
        let (extent, raw_buffer) = self.header_extent_in(state, object).ok()??;
        let current = state.objects.get(&object)?;
        let element = self.descriptor(current.layout).ok()?.kind().packed_element_layout();
        let needs_prefix = !raw_buffer && element.is_some_and(|element| {
            self.descriptor(element).map_or(true, |descriptor| matches!(descriptor.kind(),
                LayoutKind::RuntimePointerValueSlot { .. } | LayoutKind::Scalar(ScalarKind::Unit)))
        });
        if needs_prefix {
            // Managed/Unit consolidation can change range/slot facts and can
            // fail. Keep its existing transactional scratch path intact.
            let mut scratch = MemoryChecker { function: self.function, signature: self.signature, layouts: self.layouts, closure_layouts: self.closure_layouts, affine_abi: self.affine_abi, pointer_fields: Rc::clone(&self.pointer_fields), state: state.clone() };
            scratch.promote_header_extent(object).ok()?;
            return scratch.state.objects.get(&object).cloned();
        }
        // For pointer-free storage consolidation is a no-op. Only the one
        // returned object changes; neither predecessor snapshot is mutated.
        let mut promoted = current.clone();
        promoted.extent = extent;
        Some(promoted)
    }
}
