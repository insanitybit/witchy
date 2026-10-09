use super::*;

#[path = "structured_forward.rs"]
mod forward;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StructuredLabel {
    Block,
    Loop,
    If,
}

/// Immediate dominators of every block reachable from `entry`
/// (Cooper-Harvey-Kennedy over a reverse postorder). Unreachable blocks are
/// absent. Computed once per emitter; both the encoder and the plan mirror
/// answer `dominates` from it, so they keep choosing identical loops.
pub(super) fn immediate_dominators(
    entry: crate::canonical_safewir::BlockId,
    successors_by_block: &BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
) -> BTreeMap<crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId> {
    // Reverse postorder by an explicit DFS: an 11k-block executor must not
    // recurse on the host stack.
    let mut postorder = Vec::new();
    let mut seen = BTreeSet::from([entry]);
    let mut stack = vec![(entry, 0usize)];
    while let Some((block, next)) = stack.last_mut() {
        let successors = successors_by_block.get(block).map(Vec::as_slice).unwrap_or(&[]);
        if *next < successors.len() {
            let successor = successors[*next];
            *next += 1;
            if seen.insert(successor) {
                stack.push((successor, 0));
            }
        } else {
            postorder.push(*block);
            stack.pop();
        }
    }
    let order: BTreeMap<_, _> = postorder.iter().rev().enumerate().map(|(index, block)| (*block, index)).collect();
    let mut predecessors: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for (block, successors) in successors_by_block {
        if !order.contains_key(block) {
            continue;
        }
        for successor in successors {
            predecessors.entry(*successor).or_default().push(*block);
        }
    }
    let mut idom = BTreeMap::from([(entry, entry)]);
    let intersect = |idom: &BTreeMap<_, _>, mut left, mut right| {
        while left != right {
            while order[&left] > order[&right] {
                left = idom[&left];
            }
            while order[&right] > order[&left] {
                right = idom[&right];
            }
        }
        left
    };
    let mut changed = true;
    while changed {
        changed = false;
        for block in postorder.iter().rev().copied() {
            if block == entry {
                continue;
            }
            let mut candidate = None;
            for predecessor in predecessors.get(&block).map(Vec::as_slice).unwrap_or(&[]) {
                if !idom.contains_key(predecessor) {
                    continue;
                }
                candidate = Some(match candidate {
                    None => *predecessor,
                    Some(current) => intersect(&idom, current, *predecessor),
                });
            }
            let Some(candidate) = candidate else { continue };
            if idom.get(&block) != Some(&candidate) {
                idom.insert(block, candidate);
                changed = true;
            }
        }
    }
    idom.remove(&entry);
    idom
}

/// One more than the largest block id an emitter's edge map names: block
/// ids are minted densely by lowering and release placement, so a `Vec`
/// indexed by id is a cheap visited set for the emitter's walks.
pub(super) fn block_slot_count(
    successors_by_block: &BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
) -> usize {
    successors_by_block
        .iter()
        .flat_map(|(block, successors)| std::iter::once(*block).chain(successors.iter().copied()))
        .map(|id| id.get() as usize + 1)
        .max()
        .unwrap_or(0)
}

/// `dominator` lies on every path from `entry` to `node`. A block the entry
/// does not reach has no path at all, so every block dominates it.
pub(super) fn dominates_in(
    idom: &BTreeMap<crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId>,
    entry: crate::canonical_safewir::BlockId,
    dominator: crate::canonical_safewir::BlockId,
    node: crate::canonical_safewir::BlockId,
) -> bool {
    if node != entry && !idom.contains_key(&node) {
        return true;
    }
    let mut current = node;
    loop {
        if current == dominator {
            return true;
        }
        if current == entry {
            return false;
        }
        current = idom[&current];
    }
}

pub(super) fn reverse_loop_members(
    header: crate::canonical_safewir::BlockId,
    back_edge_sources: Vec<crate::canonical_safewir::BlockId>,
    predecessors: &BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
) -> BTreeSet<crate::canonical_safewir::BlockId> {
    let mut members = BTreeSet::new();
    let mut pending = VecDeque::from(back_edge_sources);
    while let Some(block) = pending.pop_front() {
        if block == header || !members.insert(block) {
            continue;
        }
        if let Some(sources) = predecessors.get(&block) {
            pending.extend(sources.iter().copied());
        }
    }
    members
}

pub(super) struct StructuredEmitter<'a> {
    body: &'a mut WasmFunctionEncoder,
    source_function: &'a Function,
    target_function: &'a WasmFunction,
    /// Target blocks by id, for the same reason as the plan emitter's map.
    blocks_by_id: BTreeMap<crate::canonical_safewir::BlockId, &'a WasmBlock>,
    layout: &'a LocalLayout<'a>,
    function_indices: &'a BTreeMap<FunctionId, u32>,
    signature_indices: &'a BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    import_indices: &'a BTreeMap<ImportId, u32>,
    imports: &'a BTreeMap<ImportId, ImportContract>,
    static_data: &'a BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    layouts: &'a LayoutInterner,
    owner_cleanup_function: Option<u32>,
    successors_by_block: BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
    predecessors_by_block: BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
    emitted: BTreeSet<crate::canonical_safewir::BlockId>,
    cloned_blocks: usize,
    labels: Vec<StructuredLabel>,
    loops: Vec<(crate::canonical_safewir::BlockId, usize)>,
    /// Enclosing loops' exit blocks with their `block` labels: a branch to one
    /// leaves the loop with a `br`.
    loop_exits: Vec<(crate::canonical_safewir::BlockId, usize)>,
    /// Memoized `is_loop_header` judgments; the dominance walk is quadratic
    /// and the structuring queries ask about the same blocks repeatedly.
    loop_headers: RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>,
    loop_exit_targets: RefCell<BTreeMap<crate::canonical_safewir::BlockId, Option<crate::canonical_safewir::BlockId>>>,
    back_edge_source_facts: RefCell<BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>>,
    natural_loop_facts: RefCell<BTreeMap<crate::canonical_safewir::BlockId, BTreeSet<crate::canonical_safewir::BlockId>>>,
    reaches_avoiding_facts: RefCell<BTreeMap<(crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId), bool>>,
    immediate_dominators: std::cell::OnceCell<BTreeMap<crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId>>,
    block_slots: std::cell::OnceCell<usize>,
    /// Memoized path judgments keyed by the exact normalized merge set.
    /// Both answers are reusable: a cut cycle proves a path from `start`
    /// avoids every merge and terminal, independent of which caller first
    /// encountered it. Caching refusal is essential for diamond graphs with
    /// a shared cyclic suffix.
    path_facts: RefCell<BTreeMap<Vec<crate::canonical_safewir::BlockId>, std::rc::Rc<RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>>>>,
    /// Blocks from which every path ends in a trap: a failed check's
    /// diagnostic, and the releases on the way to it.
    doomed: BTreeSet<crate::canonical_safewir::BlockId>,
    /// Advisory layout hints for this body, in instruction order.
    hints: Vec<wasm_encoder::BranchHint>,
}

impl StructuredEmitter<'_> {
    /// Hint the `if` about to be emitted when exactly one arm is doomed, so
    /// the engine lays that arm out of line and a passing check falls
    /// through. A hint never changes which arm executes.
    pub(super) fn hint_if(
        &mut self,
        then_target: crate::canonical_safewir::BlockId,
        else_target: crate::canonical_safewir::BlockId,
    ) {
        let taken = match (self.doomed.contains(&then_target), self.doomed.contains(&else_target)) {
            (false, true) => 1,
            (true, false) => 0,
            _ => return,
        };
        self.hint(taken);
    }

    /// Hint the `br_if` about to be emitted as not taken: its destination
    /// is a doomed arm.
    pub(super) fn hint_cold_branch(&mut self) { self.hint(0); }

    fn hint(&mut self, taken: u32) {
        if let Ok(branch_func_offset) = u32::try_from(self.body.byte_len()) {
            self.hints.push(wasm_encoder::BranchHint { branch_func_offset, branch_hint_value: taken });
        }
    }

    fn block(&self, id: crate::canonical_safewir::BlockId) -> Result<&WasmBlock, EncodeError> {
        self.blocks_by_id
            .get(&id)
            .copied()
            .ok_or(EncodeError::MissingBlock {
                function: self.source_function.id,
                block: id,
            })
    }

    fn successors(
        &self,
        id: crate::canonical_safewir::BlockId,
    ) -> Result<Vec<crate::canonical_safewir::BlockId>, EncodeError> {
        self.successors_by_block.get(&id).cloned().ok_or(EncodeError::MissingBlock {
            function: self.source_function.id,
            block: id,
        })
    }

    fn successor_slice(&self, id: crate::canonical_safewir::BlockId) -> Result<&[crate::canonical_safewir::BlockId], EncodeError> {
        self.successors_by_block.get(&id).map(Vec::as_slice).ok_or(EncodeError::MissingBlock {
            function: self.source_function.id,
            block: id,
        })
    }

    /// `distances_from` stopping at `stops` (which are never entered).
    fn distances_until(
        &self,
        start: crate::canonical_safewir::BlockId,
        stops: &[crate::canonical_safewir::BlockId],
    ) -> Result<foldhash::HashMap<crate::canonical_safewir::BlockId, usize>, EncodeError> {
        // Hashed rather than ordered: callers only probe and rank the
        // entries, and on an executor-sized function this walk runs per
        // branch (the ordered map and a successor-list clone per step were
        // most of its cost).
        let mut distances = foldhash::HashMap::default();
        let mut pending = VecDeque::from([(start, 0)]);
        while let Some((id, distance)) = pending.pop_front() {
            if stops.contains(&id) || distances.contains_key(&id) {
                continue;
            }
            // The edge map is keyed by exactly the function's blocks, so
            // this lookup also rejects a missing block.
            let successors = self.successor_slice(id)?;
            distances.insert(id, distance);
            pending.extend(successors.iter().map(|successor| (*successor, distance + 1)));
        }
        Ok(distances)
    }

    fn distances_from(
        &self,
        start: crate::canonical_safewir::BlockId,
    ) -> Result<BTreeMap<crate::canonical_safewir::BlockId, usize>, EncodeError> {
        let mut distances = BTreeMap::new();
        let mut pending = VecDeque::from([(start, 0)]);
        while let Some((id, distance)) = pending.pop_front() {
            if distances.contains_key(&id) {
                continue;
            }
            self.block(id)?;
            distances.insert(id, distance);
            pending.extend(
                self.successors(id)?
                    .into_iter()
                    .map(|successor| (successor, distance + 1)),
            );
        }
        Ok(distances)
    }

    fn reaches(
        &self,
        start: crate::canonical_safewir::BlockId,
        goal: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        // encode_function checks every successor while constructing the
        // predecessor map, before any query can succeed early. Keep distance
        // maps for join scores; this query needs only graph membership.
        let mut visited = BTreeSet::new();
        let mut pending = VecDeque::from([start]);
        while let Some(id) = pending.pop_front() {
            if !visited.insert(id) { continue; }
            self.block(id)?;
            if id == goal { return Ok(true); }
            let successors = self.successors_by_block.get(&id).ok_or(EncodeError::MissingBlock {
                function: self.source_function.id, block: id,
            })?;
            pending.extend(successors.iter().copied());
        }
        Ok(false)
    }

    /// One slot per block id, for dense visited marks in the walks below.
    fn block_slots(&self) -> usize {
        *self.block_slots.get_or_init(|| block_slot_count(&self.successors_by_block))
    }

    /// A side only terminates when every path from it ends in a terminal
    /// block (structured loops included: their bodies return to their
    /// headers or leave through their exits, and the exits terminate) and it
    /// shares no block with the other side, where the other side is what
    /// `other` reaches without passing back through `avoid` (the branching
    /// block: a loop back edge would otherwise make a returning arm look
    /// reachable from the continuing side). Such a side never reaches a
    /// merge and needs none.
    fn only_terminates_avoiding(
        &self,
        start: crate::canonical_safewir::BlockId,
        other: crate::canonical_safewir::BlockId,
        avoid: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        let slots = self.block_slots();
        let mut visited = vec![false; slots];
        let mut side = Vec::new();
        let mut pending = VecDeque::from([start]);
        while let Some(id) = pending.pop_front() {
            if std::mem::replace(&mut visited[id.get() as usize], true) {
                continue;
            }
            self.block(id)?;
            side.push(id);
            pending.extend(self.successors(id)?);
        }
        // The sides share a block exactly when `other` reaches one of this
        // side's blocks without passing through `avoid`: walk that path
        // backwards from the side. The side is small (a terminating arm);
        // the forward sweep from `other` this replaces covered the rest of
        // the function on every call -- 13.5M block visits in one async
        // compile, against 47k for the side itself.
        if other != avoid {
            let mut reached = vec![false; slots];
            let mut backward = VecDeque::from(side);
            while let Some(id) = backward.pop_front() {
                if id == avoid || std::mem::replace(&mut reached[id.get() as usize], true) {
                    continue;
                }
                if id == other {
                    return Ok(false);
                }
                backward.extend(self.predecessors_by_block.get(&id).into_iter().flatten().copied());
            }
        }
        self.all_paths_reach_or_terminate(start, &[], &mut BTreeSet::new())
    }

    fn is_terminal(
        &self,
        id: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        Ok(matches!(
            &self.block(id)?.terminator.operation,
            ScalarTerminator::Return { .. }
                | ScalarTerminator::TailCall { .. }
                | ScalarTerminator::VarTailCall { .. }
                | ScalarTerminator::Trap
                | ScalarTerminator::Unreachable
        ))
    }

    /// True only for a loop-leaving tail that can be emitted inside the loop:
    /// it cannot return to `header`, and the structural walk accepts only
    /// terminal paths or recognized nested loops with recursively checked
    /// exits; this is not a proof of runtime termination. No block, including
    /// a terminal sink, may be shared with code outside that loop and tail.
    /// Shared sinks remain continuation candidates for single emission.
    fn is_exclusive_terminal_loop_tail(
        &self,
        start: crate::canonical_safewir::BlockId,
        header: crate::canonical_safewir::BlockId,
        members: &[crate::canonical_safewir::BlockId],
    ) -> Result<bool, EncodeError> {
        if self.reaches(start, header)? {
            return Ok(false);
        }
        let reach = self.reachable_until(start, &[header])?;
        if !self.all_paths_reach_or_terminate(start, &[], &mut BTreeSet::new())? {
            return Ok(false);
        }
        for reached in &reach {
            // A terminal sink shared with another continuation must remain a
            // merge, otherwise structured emission can visit its block twice.
            let predecessors = self.predecessors(*reached)?;
            let mut external = predecessors.iter().filter(|predecessor| !reach.contains(predecessor));
            if *reached == start {
                let Some(entry) = external.next() else { return Ok(false) };
                if !members.contains(entry) || external.next().is_some() {
                    return Ok(false);
                }
            } else if external.next().is_some() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Check finite paths to a proposed merge (any of `merges`).  A terminal
    /// path is allowed to bypass a merge, but a cycle that can avoid it is
    /// not.  Recognized loop headers are treated as structured boundaries:
    /// every path through the body must return to the header, leave through
    /// the loop's exit, or terminate, and the exit must still reach the merge.
    fn all_paths_reach_or_terminate(
        &self,
        start: crate::canonical_safewir::BlockId,
        merges: &[crate::canonical_safewir::BlockId],
        active: &mut BTreeSet<crate::canonical_safewir::BlockId>,
    ) -> Result<bool, EncodeError> {
        if merges.contains(&start) || self.is_terminal(start)? {
            return Ok(true);
        }
        let mut stops = merges.to_vec();
        stops.sort();
        let facts = self.path_query_facts(&stops);
        self.all_paths_with_facts(start, &stops, active, &facts)
    }

    fn path_query_facts(
        &self,
        sorted_stops: &[crate::canonical_safewir::BlockId],
    ) -> std::rc::Rc<RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>> {
        // Intern only the old exact sorted key, including duplicates. The
        // query borrows this group's node memo throughout recursion; no
        // ancestor-set judgment or success for another stop set is reused.
        let mut groups = self.path_facts.borrow_mut();
        if let Some(facts) = groups.get(sorted_stops) {
            return std::rc::Rc::clone(facts);
        }
        let facts = std::rc::Rc::new(RefCell::new(BTreeMap::new()));
        groups.insert(sorted_stops.to_vec(), std::rc::Rc::clone(&facts));
        facts
    }

    fn all_paths_with_facts(
        &self,
        start: crate::canonical_safewir::BlockId,
        merges: &[crate::canonical_safewir::BlockId],
        active: &mut BTreeSet<crate::canonical_safewir::BlockId>,
        facts: &RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>,
    ) -> Result<bool, EncodeError> {
        if merges.contains(&start) || self.is_terminal(start)? {
            return Ok(true);
        }
        if let Some(result) = facts.borrow().get(&start) {
            return Ok(*result);
        }
        if !active.insert(start) {
            return Ok(false);
        }
        let result = if self.is_loop_header(start)? {
            let exit = self.loop_exit(start)?;
            let mut body_merges = vec![start];
            body_merges.extend(exit);
            body_merges.sort();
            let body_facts = self.path_query_facts(&body_merges);
            // The body walk keeps the caller's active set: body blocks are
            // dominated by the header, so an active ancestor is reachable
            // only by leaving the loop somewhere other than its exit, which
            // must fail rather than walk the enclosing loop again.
            let mut result = true;
            for successor in self.successors(start)? {
                if !self.all_paths_with_facts(successor, &body_merges, active, &body_facts)? {
                    result = false;
                    break;
                }
            }
            result
                && match exit {
                    Some(exit) => self.all_paths_with_facts(exit, merges, active, facts)?,
                    None => true,
                }
        } else {
            let mut result = true;
            for successor in self.successors(start)? {
                if !self.all_paths_with_facts(successor, merges, active, facts)? {
                    result = false;
                    break;
                }
            }
            result
        };
        active.remove(&start);
        facts.borrow_mut().insert(start, result);
        Ok(result)
    }

    fn is_loop_header(
        &self,
        id: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        if let Some(known) = self.loop_headers.borrow().get(&id) {
            return Ok(*known);
        }
        // A loop header is the target of a back edge: a predecessor it
        // dominates. Plain reachability is not enough: an inner loop's exit
        // reaches the inner header again through the enclosing loop's back
        // edge, which is not a cycle of the inner loop.
        let result = !self.back_edge_sources(id)?.is_empty();
        self.loop_headers.borrow_mut().insert(id, result);
        Ok(result)
    }

    /// The block a structured loop continues at: the one target outside the
    /// header's natural loop that the loop's blocks branch to.  `None` when
    /// the loop only leaves by terminating; several distinct exits are not a
    /// shape the structured emitter accepts.
    fn loop_exit(
        &self,
        header: crate::canonical_safewir::BlockId,
    ) -> Result<Option<crate::canonical_safewir::BlockId>, EncodeError> {
        if let Some(known) = self.loop_exit_targets.borrow().get(&header) {
            return Ok(*known);
        }
        // One natural-loop set, read once: probing it per block per header
        // and scanning `members` per successor was quadratic in loop size.
        // Every member is a block of the function, so `is_member` answers
        // exactly what `members.contains` did.
        let natural = if self.target_function.blocks.iter().any(|block| block.id != header) {
            self.natural_loop_members(header)?
        } else {
            BTreeSet::new()
        };
        let mut members = vec![header];
        members.extend(self.target_function.blocks.iter().map(|block| block.id).filter(|id| *id != header && natural.contains(id)));
        let is_member = |block: &crate::canonical_safewir::BlockId| *block == header || natural.contains(block);
        // The canonical `while` header names an exit directly: its `else`
        // side leaves the loop. Every other edge out of the loop is an exit
        // too unless all its paths terminate (a `return` inside the body,
        // emitted inline). With one exit the loop continues there; with
        // several (drop blocks on the edges out of a byte scan) it continues
        // at the nearest block all of them reach, and the edge blocks are
        // emitted inside the loop ahead of their `br`.
        let classic_exit = match &self.block(header)?.terminator.operation {
            ScalarTerminator::BranchIf { else_target, .. } if !is_member(else_target) => Some(*else_target),
            _ => None,
        };
        let mut leaving = BTreeSet::new();
        leaving.extend(classic_exit);
        for member in &members {
            for successor in self.successors(*member)? {
                if !is_member(&successor) {
                    leaving.insert(successor);
                }
            }
        }
        // An exclusive terminal tail is emitted inline; every other leaving
        // edge is a continuation. Shared terminal sinks are harmless, but a
        // shared cleanup block would duplicate an observable operation.
        let mut candidates = BTreeSet::new();
        for block in &leaving {
            if Some(*block) == classic_exit
                || !self.is_exclusive_terminal_loop_tail(*block, header, &members)?
            {
                candidates.insert(*block);
            }
        }
        let exit = if candidates.len() <= 1 {
            candidates.into_iter().next()
        } else {
            let mut common: Option<(usize, crate::canonical_safewir::BlockId)> = None;
            let distances = candidates
                .iter()
                .map(|candidate| self.distances_from(*candidate))
                .collect::<Result<Vec<_>, _>>()?;
            for block in distances[0].keys() {
                let Some(farthest) = distances
                    .iter()
                    .map(|reach| reach.get(block).copied())
                    .collect::<Option<Vec<_>>>()
                    .and_then(|reaches| reaches.into_iter().max())
                else {
                    continue;
                };
                let score = (farthest, *block);
                if common.is_none_or(|previous| score < previous) {
                    common = Some(score);
                }
            }
            let Some((_, block)) = common else {
                return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                    function: self.source_function.id,
                    operation: "loop exits that never converge",
                }));
            };
            Some(block)
        };
        self.loop_exit_targets.borrow_mut().insert(header, exit);
        Ok(exit)
    }

    /// A branch that leaves structured emission with a plain `br`: back to an
    /// enclosing loop's header (its loop label) or out to that loop's exit
    /// (its block label).
    fn escape_label(&self, target: crate::canonical_safewir::BlockId) -> Option<usize> {
        self.loops
            .iter()
            .rev()
            .find(|(header, _)| *header == target)
            .or_else(|| self.loop_exits.iter().rev().find(|(exit, _)| *exit == target))
            .map(|(_, label_index)| *label_index)
    }

    fn predecessors(&self, block: crate::canonical_safewir::BlockId) -> Result<Vec<crate::canonical_safewir::BlockId>, EncodeError> {
        self.predecessors_by_block.get(&block).cloned().ok_or(EncodeError::MissingBlock {
            function: self.source_function.id,
            block,
        })
    }

    fn reachable_until(&self, start: crate::canonical_safewir::BlockId, stops: &[crate::canonical_safewir::BlockId]) -> Result<foldhash::HashSet<crate::canonical_safewir::BlockId>, EncodeError> {
        // Hashed: callers only test membership, disjointness and all/any.
        let mut seen = foldhash::HashSet::default();
        let mut pending = VecDeque::from([start]);
        while let Some(id) = pending.pop_front() {
            if stops.contains(&id) || !seen.insert(id) {
                continue;
            }
            pending.extend(self.successor_slice(id)?);
        }
        Ok(seen)
    }

    /// A branch side that only leaves: its target is an escape, or every
    /// path from it ends in an escape or a terminal block (the drop block on
    /// a loop's exit edge) and it shares no block with the other side.
    fn side_escapes(&self, current: crate::canonical_safewir::BlockId, side: crate::canonical_safewir::BlockId, other: crate::canonical_safewir::BlockId, stop: Option<crate::canonical_safewir::BlockId>) -> Result<bool, EncodeError> {
        if self.escape_label(side).is_some() {
            return Ok(true);
        }
        let mut stops = self.escape_targets();
        if stops.is_empty() || !self.all_paths_reach_or_terminate(side, &stops, &mut BTreeSet::new())? {
            return Ok(false);
        }
        stops.push(current);
        let reach = self.reachable_until(side, &stops)?;
        // A side that reaches the enclosing merge flows into it rather than
        // escaping: emitting it inline here would emit the merge twice.
        if stop.is_some_and(|stop| reach.contains(&stop)) {
            return Ok(false);
        }
        // Every path must end at an escape: a side that can also return is
        // a terminal side for `only_terminates` to judge, not an escape.
        for block in &reach {
            if self.is_terminal(*block)? {
                return Ok(false);
            }
        }
        let other_reach = self.reachable_until(other, &stops)?;
        Ok(reach.is_disjoint(&other_reach))
    }

    fn escape_targets(&self) -> Vec<crate::canonical_safewir::BlockId> {
        self.loops
            .iter()
            .map(|(header, _)| *header)
            .chain(self.loop_exits.iter().map(|(exit, _)| *exit))
            .collect()
    }

    fn reaches_avoiding(
        &self,
        start: crate::canonical_safewir::BlockId,
        goal: crate::canonical_safewir::BlockId,
        avoid: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        let key = (start, goal, avoid);
        if let Some(known) = self.reaches_avoiding_facts.borrow().get(&key) {
            return Ok(*known);
        }
        let mut seen = BTreeSet::new();
        let mut pending = VecDeque::from([start]);
        let mut result = false;
        while let Some(id) = pending.pop_front() {
            if id == avoid || !seen.insert(id) {
                continue;
            }
            if id == goal {
                result = true;
                break;
            }
            pending.extend(self.successors(id)?);
        }
        self.reaches_avoiding_facts.borrow_mut().insert(key, result);
        Ok(result)
    }

    /// `dominator` lies on every path from the entry block to `node`.
    fn dominates(
        &self,
        dominator: crate::canonical_safewir::BlockId,
        node: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        let entry = self.target_function.entry;
        let idom = self
            .immediate_dominators
            .get_or_init(|| immediate_dominators(entry, &self.successors_by_block));
        Ok(dominates_in(idom, entry, dominator, node))
    }

    /// Predecessors of `header` that it dominates: the sources of its back edges.
    fn back_edge_sources(
        &self,
        header: crate::canonical_safewir::BlockId,
    ) -> Result<Vec<crate::canonical_safewir::BlockId>, EncodeError> {
        if let Some(known) = self.back_edge_source_facts.borrow().get(&header) {
            return Ok(known.clone());
        }
        let mut sources = Vec::new();
        for predecessor in self.predecessors(header)? {
            if predecessor != header && self.dominates(header, predecessor)? {
                sources.push(predecessor);
            }
        }
        self.back_edge_source_facts.borrow_mut().insert(header, sources.clone());
        Ok(sources)
    }

    fn natural_loop_members(
        &self,
        header: crate::canonical_safewir::BlockId,
    ) -> Result<BTreeSet<crate::canonical_safewir::BlockId>, EncodeError> {
        if let Some(known) = self.natural_loop_facts.borrow().get(&header) {
            return Ok(known.clone());
        }
        let members = reverse_loop_members(
            header,
            self.back_edge_sources(header)?,
            &self.predecessors_by_block,
        );
        self.natural_loop_facts.borrow_mut().insert(header, members.clone());
        Ok(members)
    }

    /// `block` belongs to the natural loop of `header`: it reaches one of the
    /// header's back-edge sources without passing through the header.
    fn in_natural_loop(
        &self,
        header: crate::canonical_safewir::BlockId,
        block: crate::canonical_safewir::BlockId,
    ) -> Result<bool, EncodeError> {
        if block == header {
            return Ok(false);
        }
        if let Some(members) = self.natural_loop_facts.borrow().get(&header) {
            return Ok(members.contains(&block));
        }
        Ok(self.natural_loop_members(header)?.contains(&block))
    }

    fn find_join(
        &self,
        current: crate::canonical_safewir::BlockId,
        then_target: crate::canonical_safewir::BlockId,
        else_target: crate::canonical_safewir::BlockId,
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<Option<crate::canonical_safewir::BlockId>, EncodeError> {
        if then_target == else_target {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "non-distinct structured branches",
            }));
        }
        // A side that escapes to an enclosing loop's header or exit is a plain
        // `br`: it never reaches a merge, and the other side's first block is
        // where emission continues.
        match (
            self.side_escapes(current, then_target, else_target, stop)?,
            self.side_escapes(current, else_target, then_target, stop)?,
        ) {
            (true, true) => return Ok(None),
            (true, false) => return Ok(Some(else_target)),
            (false, true) => return Ok(Some(then_target)),
            (false, false) => {}
        }
        // Candidates come from paths that stay in the current loop region: a
        // block reached only through an enclosing loop's back edge or exit is
        // the next iteration's, not this branch's merge.
        let stops = self.escape_targets();
        // A merge lies before the enclosing continuation: a block reached only
        // through `stop` belongs to the code after this branch's region, and
        // choosing it would emit `stop` inside one side and again after it.
        let mut bounded = stops.clone();
        bounded.extend(stop);
        let then_distances = self.distances_until(then_target, &bounded)?;
        let else_distances = self.distances_until(else_target, &bounded)?;
        // A side whose every path terminates (an exhaustive `match` whose
        // fallthrough traps, `if c: a else: return`) never reaches the merge;
        // `all_paths_reach_or_terminate` already permits that bypass, so such
        // a side contributes distance zero instead of vetoing every candidate.
        // The other side's reach is taken without passing back through the
        // branching block: a loop back edge would otherwise make a returning
        // arm look reachable from the continuing side.
        // Reaching an enclosing continuation is not terminal within this
        // region, even if execution eventually returns beyond that boundary.
        let then_reaches_stop = match stop { Some(stop) => self.reaches(then_target, stop)?, None => false };
        let else_reaches_stop = match stop { Some(stop) => self.reaches(else_target, stop)?, None => false };
        let then_terminal = !then_reaches_stop && self.only_terminates_avoiding(then_target, else_target, current)?;
        let else_terminal = !else_reaches_stop && self.only_terminates_avoiding(else_target, then_target, current)?;
        // Rank every candidate by the score the selection below minimizes
        // before running the path check, so the check runs cheapest-first and
        // stops at the first admissible candidate: that is the minimum, and
        // an executor-sized region no longer walks every reachable block per
        // candidate.
        let mut scored = Vec::new();
        for candidate in then_distances.keys().chain(else_distances.keys()).copied() {
            // A side's own first block can be the merge: when the other side
            // only terminates (`if c: return x` then fall through) or flows
            // into it (a length test guarding a scan loop that exits there),
            // the structured form is `if c { ... } else {} end` followed by
            // that block. An enclosing loop's header or exit is never a
            // merge: emission reaches those only through `br`.
            if candidate == current || self.escape_label(candidate).is_some() {
                continue;
            }
            let then_distance = match then_distances.get(&candidate) {
                Some(distance) => *distance,
                None if then_terminal => 0,
                None => continue,
            };
            let else_distance = match else_distances.get(&candidate) {
                Some(distance) => *distance,
                None if else_terminal => 0,
                None => continue,
            };
            scored.push((then_distance + else_distance, candidate));
        }
        scored.sort_unstable();
        scored.dedup();
        let mut best = None;
        for score @ (_, candidate) in scored {
            // Inside a loop a path may also leave through the loop's exit or
            // back edge (a `br`) instead of reaching the merge.
            let mut merges = self.escape_targets();
            merges.push(candidate);
            if self.all_paths_reach_or_terminate(then_target, &merges, &mut BTreeSet::new())?
                && self.all_paths_reach_or_terminate(else_target, &merges, &mut BTreeSet::new())?
                && !forward_cfg::bypasses_join_continuation(then_target, else_target, candidate,
                    &self.successors_by_block, &stops).map_err(|_| EncodeError::Lowering(
                    LoweringError::UnsupportedControlFlow { function: self.source_function.id,
                        operation: "invalid join continuation" }))?
            {
                best = Some(score);
                break;
            }
        }
        if best.is_none() && let Some(stop) = stop && self.escape_label(stop).is_none() {
            let mut merges = self.escape_targets();
            merges.push(stop);
            if self.all_paths_reach_or_terminate(then_target, &merges, &mut BTreeSet::new())?
                && self.all_paths_reach_or_terminate(else_target, &merges, &mut BTreeSet::new())?
                && !forward_cfg::bypasses_join_continuation(then_target, else_target, stop,
                    &self.successors_by_block, &stops).map_err(|_| EncodeError::Lowering(
                    LoweringError::UnsupportedControlFlow { function: self.source_function.id,
                        operation: "invalid join continuation" }))?
            {
                best = Some((usize::MAX, stop));
            }
        }
        if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
            eprintln!("find_join current={current:?} then={then_target:?} else={else_target:?} then_terminal={then_terminal} else_terminal={else_terminal} best={best:?}");
        }
        best.map(|(_, block)| Some(block)).ok_or(EncodeError::Lowering(
            LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "irreducible or non-convergent BranchIf",
            },
        ))
    }

    fn label_depth(&self, index: usize) -> u32 {
        (self.labels.len() - 1 - index) as u32
    }

    fn emit_branch_arguments(
        &mut self,
        target: crate::canonical_safewir::BlockId,
        arguments: &[ValueId],
    ) -> Result<(), EncodeError> {
        self.block(target)?;
        let source_block = self
            .source_function
            .blocks
            .iter()
            .find(|block| block.id == target)
            .ok_or(EncodeError::MissingBlock {
                function: self.source_function.id,
                block: target,
            })?;
        if source_block.parameters.len() != arguments.len() {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "branch arity",
            }));
        }
        // Evaluate every edge operand before assigning any destination.  This
        // is the SSA phi transfer rule and also handles a valid parallel swap.
        for argument in arguments {
            emit_get(self.body, self.layout, self.source_function.id, *argument)?;
        }
        for parameter in source_block.parameters.iter().rev() {
            emit_set(
                self.body,
                self.layout,
                self.source_function.id,
                parameter.value,
            )?;
        }
        Ok(())
    }

    fn emit_path(
        &mut self,
        start: crate::canonical_safewir::BlockId,
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<(), EncodeError> {
        if let Some(label_index) = self.escape_label(start) {
            self.body
                .instruction(&wasm_encoder::Instruction::Br(self.label_depth(label_index)));
            return Ok(());
        }
        if stop == Some(start) {
            return Ok(());
        }
        if self.is_loop_header(start)? {
            return self.emit_loop(start, stop);
        }
        if !self.loops.is_empty() && stop.is_none_or(|target| self.escape_label(target).is_some()) {
            let order = forward_cfg::region(start, &self.successors_by_block, &self.escape_targets())
                .map_err(|_| EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                    function: self.source_function.id, operation: "invalid forward loop region",
                }))?;
            // A region already entered once (a copied tail) is emitted block
            // by block, where a second entry takes the bounded copy.
            if let Some(order) = order && !order.iter().any(|block| self.emitted.contains(block)) {
                return self.emit_forward(&order);
            }
        }
        if !self.emitted.insert(start) {
            // A shared continuation may be entered from two nested branch
            // paths. Copy only its already emitted acyclic portion, stopping
            // at the enclosing join (or the function's exits when there is
            // none); never turn a loop into duplicated code.
            let cloneable = forward_cfg::repeatable_region(start, stop,
                &self.successors_by_block, &self.escape_targets(), &self.emitted,
                64usize.saturating_sub(self.cloned_blocks)).ok().flatten().is_some();
            if !cloneable {
                return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                    function: self.source_function.id,
                    operation: "block emitted twice",
                }));
            }
            self.cloned_blocks += 1;
        }
        let block = self.block(start)?.clone();
        for instruction in &block.instructions {
            emit_instruction(
                self.body,
                self.source_function,
                self.layout,
                &instruction.operation,
                self.function_indices,
                self.signature_indices,
                self.import_indices,
                self.imports,
                self.static_data,
                self.layouts,
                self.owner_cleanup_function,
            )?;
        }
        self.emit_block_terminator(start, block.terminator.operation, stop)
    }

    fn emit_block_terminator(
        &mut self,
        start: crate::canonical_safewir::BlockId,
        terminator: ScalarTerminator,
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<(), EncodeError> {
        match terminator {
            ScalarTerminator::Branch { target, arguments } => {
                self.emit_branch_arguments(target, &arguments)?;
                self.emit_path(target, stop)
            }
            ScalarTerminator::BranchIf {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } => self.emit_if(
                start,
                condition,
                then_target,
                &then_arguments,
                else_target,
                &else_arguments,
                stop,
            ),
            terminator => emit_terminator(
                self.body,
                self.source_function,
                self.layout,
                &terminator,
                self.function_indices,
                self.signature_indices,
            ),
        }
    }

    fn emit_if(
        &mut self,
        current: crate::canonical_safewir::BlockId,
        condition: ValueId,
        then_target: crate::canonical_safewir::BlockId,
        then_arguments: &[ValueId],
        else_target: crate::canonical_safewir::BlockId,
        else_arguments: &[ValueId],
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<(), EncodeError> {
        if self.is_loop_header(current)? && !self.loops.iter().any(|(header, _)| *header == current) {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "loop header outside structured loop",
            }));
        }
        let join = self.find_join(current, then_target, else_target, stop)?;
        emit_get(self.body, self.layout, self.source_function.id, condition)?;
        // Cranelift lays a conditional's first successor out last. When the
        // `else` side leaves the innermost loop, test the negation so the
        // exit is that successor and the loop's own path stays inline.
        let (then_target, then_arguments, else_target, else_arguments) =
            if self.leaves_innermost_loop(else_target) && !self.leaves_innermost_loop(then_target) {
                self.body.instruction(&wasm_encoder::Instruction::I32Eqz);
                (else_target, else_arguments, then_target, then_arguments)
            } else {
                (then_target, then_arguments, else_target, else_arguments)
            };
        self.hint_if(then_target, else_target);
        self.body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        self.labels.push(StructuredLabel::If);
        self.emit_branch_arguments(then_target, then_arguments)?;
        self.emit_path(then_target, join)?;
        self.body.instruction(&wasm_encoder::Instruction::Else);
        self.emit_branch_arguments(else_target, else_arguments)?;
        self.emit_path(else_target, join)?;
        self.body.instruction(&wasm_encoder::Instruction::End);
        self.labels.pop();
        match join {
            Some(join) => self.emit_path(join, stop),
            None => Ok(()),
        }
    }

    fn leaves_innermost_loop(&self, target: crate::canonical_safewir::BlockId) -> bool {
        self.loop_exits.last().is_some_and(|(exit, _)| *exit == target)
    }

    fn emit_loop(
        &mut self,
        header: crate::canonical_safewir::BlockId,
        stop: Option<crate::canonical_safewir::BlockId>,
    ) -> Result<(), EncodeError> {
        if !self.emitted.insert(header) {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "loop header emitted twice",
            }));
        }
        if !self.is_loop_header(header)? {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
                function: self.source_function.id,
                operation: "non-canonical loop shape",
            }));
        }
        let block = self.block(header)?.clone();
        let exit = self.loop_exit(header)?;
        if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
            eprintln!("emit_loop header={header:?} exit={exit:?} stop={stop:?}");
        }

        self.body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
        let block_label = self.labels.len();
        self.labels.push(StructuredLabel::Block);
        self.body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
        let loop_label = self.labels.len();
        self.labels.push(StructuredLabel::Loop);
        for instruction in &block.instructions {
            emit_instruction(
                self.body,
                self.source_function,
                self.layout,
                &instruction.operation,
                self.function_indices,
                self.signature_indices,
                self.import_indices,
                self.imports,
                self.static_data,
                self.layouts,
                self.owner_cleanup_function,
            )?;
        }
        self.loops.push((header, loop_label));
        if let Some(exit) = exit {
            self.loop_exits.push((exit, block_label));
        }
        match block.terminator.operation {
            // The canonical `while` shape (the header tests the condition and
            // its `else` is the loop's exit) leaves under a negated test and
            // runs the body as the header's fall-through. Cranelift lays out a
            // conditional's first successor last, so a body under `if` would
            // sit after the rest of the function and every iteration would
            // take two branches instead of one.
            ScalarTerminator::BranchIf {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } if Some(else_target) == exit => {
                emit_get(self.body, self.layout, self.source_function.id, condition)?;
                self.body.instruction(&wasm_encoder::Instruction::I32Eqz);
                self.body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
                self.labels.push(StructuredLabel::If);
                self.emit_branch_arguments(else_target, &else_arguments)?;
                self.body
                    .instruction(&wasm_encoder::Instruction::Br(self.label_depth(block_label)));
                self.body.instruction(&wasm_encoder::Instruction::End);
                self.labels.pop();
                self.emit_branch_arguments(then_target, &then_arguments)?;
                self.emit_path(then_target, Some(header))?;
            }
            // Any other header (a short-circuit condition that decides the
            // exit in a later block, a header that only leaves by returning)
            // is emitted structurally inside the loop: branches back to the
            // header and out to the exit become `br`.
            terminator => self.emit_block_terminator(header, terminator, Some(header))?,
        }
        if exit.is_some() {
            self.loop_exits.pop();
        }
        self.loops.pop();
        self.body.instruction(&wasm_encoder::Instruction::End);
        self.labels.pop();
        self.body.instruction(&wasm_encoder::Instruction::End);
        self.labels.pop();
        match exit {
            Some(exit) => self.emit_path(exit, stop),
            // A loop that only leaves by terminating never falls out of its
            // block: the code after it is unreachable, and saying so keeps
            // the function's result stack valid.
            None => {
                self.body.instruction(&wasm_encoder::Instruction::Unreachable);
                Ok(())
            }
        }
    }
}

pub(super) fn emit_terminator(
    body: &mut wasm_encoder::Function,
    source_function: &Function,
    layout: &LocalLayout,
    terminator: &ScalarTerminator,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
) -> Result<(), EncodeError> {
    match terminator {
        ScalarTerminator::Branch { .. } => return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow { function: source_function.id, operation: "Branch" })),
        ScalarTerminator::BranchIf { .. } => return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow { function: source_function.id, operation: "BranchIf" })),
        ScalarTerminator::Return { values } => {
            for value in values { emit_get(body, layout, source_function.id, *value)?; }
            body.instruction(&wasm_encoder::Instruction::Return);
        }
        ScalarTerminator::TailCall { callee, arguments, .. } => {
            emit_call_arguments(body, layout, source_function.id, arguments)?;
            let index = function_indices.get(callee).copied().ok_or(EncodeError::MissingFunction(*callee))?;
            body.instruction(&wasm_encoder::Instruction::ReturnCall(index));
        }
        ScalarTerminator::VarTailCall { callee, signature, arguments, .. } => {
            emit_call_arguments(body, layout, source_function.id, arguments)?;
            emit_get(body, layout, source_function.id, *callee)?;
            let type_index = signature_indices.get(signature).copied().ok_or(
                EncodeError::UnsupportedSignature { signature: *signature },
            )?;
            body.instruction(&wasm_encoder::Instruction::ReturnCallIndirect {
                type_index,
                table_index: 0,
            });
        }
        ScalarTerminator::Trap | ScalarTerminator::Unreachable => { body.instruction(&wasm_encoder::Instruction::Unreachable); }
    }
    Ok(())
}

pub(super) fn encode_function(
    source_function: &Function,
    signature: &FunctionSignature,
    target_function: &WasmFunction,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    import_indices: &BTreeMap<ImportId, u32>,
    imports: &BTreeMap<ImportId, ImportContract>,
    static_data: &BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    layouts: &LayoutInterner,
    owner_bridges: &[AdmittedOwnerBridge],
    owner_cleanup_function: Option<u32>,
    memory_helpers: &std::sync::Arc<outlined_memory::Helpers>,
) -> Result<(WasmFunctionEncoder, Vec<wasm_encoder::BranchHint>), EncodeError> {
    let mut layout = local_layout(source_function, signature, layouts)?;
    layout.owner_bridges = owner_bridges.iter().collect();
    layout.memory_helpers = Some(memory_helpers.clone());
    layout.adopt_refs(memory_helpers.refs, signature.parameters.len());
    let local_declarations: Vec<_> = layout.extra_types.iter().copied().map(|ty| (1, ty)).collect();
    let mut body = WasmFunctionEncoder::new(local_declarations);
    if target_function.entry != source_function.entry {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
            function: source_function.id,
            operation: "entry mismatch",
        }));
    }
    let entry = target_function.entry;
    let successors_by_block: BTreeMap<_, _> = target_function
        .blocks
        .iter()
        .map(|block| {
            let successors = match &block.terminator.operation {
                ScalarTerminator::Branch { target, .. } => vec![*target],
                ScalarTerminator::BranchIf { then_target, else_target, .. } => {
                    vec![*then_target, *else_target]
                }
                ScalarTerminator::Return { .. }
                | ScalarTerminator::TailCall { .. }
                | ScalarTerminator::VarTailCall { .. }
                | ScalarTerminator::Trap
                | ScalarTerminator::Unreachable => Vec::new(),
            };
            (block.id, successors)
        })
        .collect();
    let mut predecessors_by_block: BTreeMap<_, Vec<_>> = target_function
        .blocks
        .iter()
        .map(|block| (block.id, Vec::new()))
        .collect();
    for (source, successors) in &successors_by_block {
        for successor in successors {
            let predecessors = predecessors_by_block
                .get_mut(successor)
                .ok_or(EncodeError::MissingBlock {
                    function: source_function.id,
                    block: *successor,
                })?;
            if !predecessors.contains(source) {
                predecessors.push(*source);
            }
        }
    }
    let doomed = forward_cfg::doomed(&successors_by_block, target_function.blocks.iter()
        .filter(|block| matches!(block.terminator.operation, ScalarTerminator::Trap | ScalarTerminator::Unreachable))
        .map(|block| block.id));
    let mut emitter = StructuredEmitter {
        body: &mut body,
        source_function,
        target_function,
        blocks_by_id: target_function.blocks.iter().map(|block| (block.id, block)).collect(),
        layout: &layout,
        function_indices,
        signature_indices,
        import_indices,
        imports,
        static_data,
        layouts,
        owner_cleanup_function,
        successors_by_block,
        predecessors_by_block,
        emitted: BTreeSet::new(),
        cloned_blocks: 0,
        labels: Vec::new(),
        loops: Vec::new(),
        loop_exits: Vec::new(),
        loop_headers: RefCell::new(BTreeMap::new()),
        loop_exit_targets: RefCell::new(BTreeMap::new()),
        back_edge_source_facts: RefCell::new(BTreeMap::new()),
        natural_loop_facts: RefCell::new(BTreeMap::new()),
        reaches_avoiding_facts: RefCell::new(BTreeMap::new()),
        immediate_dominators: std::cell::OnceCell::new(),
        block_slots: std::cell::OnceCell::new(),
        path_facts: RefCell::new(BTreeMap::new()),
        doomed,
        hints: Vec::new(),
    };
    match forward_cfg::schedule(entry, &emitter.successors_by_block).map_err(|_| {
        EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
            function: source_function.id, operation: "invalid forward CFG",
        })
    })? {
        Some(order) => emitter.emit_forward(&order)?,
        None => emitter.emit_path(entry, None)?,
    }
    if emitter.emitted.len() != target_function.blocks.len() {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedControlFlow {
            function: source_function.id,
            operation: "unstructured or unreachable block",
        }));
    }
    let hints = std::mem::take(&mut emitter.hints);
    body.instruction(&wasm_encoder::Instruction::End);
    Ok((body, hints))
}
