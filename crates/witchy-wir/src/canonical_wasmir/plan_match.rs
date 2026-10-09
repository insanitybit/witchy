use super::*;

#[path = "plan_forward.rs"]
mod forward;

pub(super) struct PlanEmitter<'a> {
    source_function: &'a Function,
    /// References this function proves static (`static_values`).
    static_values: BTreeSet<ValueId>,
    /// Blocks by id: the structured walk looks blocks up on every path,
    /// join, and reachability step, so the lookup must not scan the body.
    blocks_by_id: BTreeMap<crate::canonical_safewir::BlockId, &'a BasicBlock>,
    layout: &'a LocalLayout<'a>,
    layouts: &'a LayoutInterner,
    function_indices: &'a BTreeMap<FunctionId, u32>,
    signature_indices: &'a BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    imports: &'a BTreeMap<ImportId, ImportContract>,
    import_indices: &'a BTreeMap<ImportId, u32>,
    static_data_offsets: &'a BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    owner_cleanup_function: Option<u32>,
    successors_by_block: BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
    predecessors_by_block: BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>,
    plan: Vec<PlanToken>,
    emitted: BTreeSet<crate::canonical_safewir::BlockId>,
    cloned_blocks: usize,
    labels: Vec<StructuredLabel>,
    loops: Vec<(crate::canonical_safewir::BlockId, usize)>,
    /// Inline aggregates are SSA markers over field locals, so a field read
    /// is valid anywhere the aggregate dominates: the map is function-wide,
    /// as in the encoder.
    aggregates: BTreeMap<ValueId, Vec<ValueId>>,
    loop_exits: Vec<(crate::canonical_safewir::BlockId, usize)>,
    loop_headers: RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>,
    loop_exit_targets: RefCell<BTreeMap<crate::canonical_safewir::BlockId, Option<crate::canonical_safewir::BlockId>>>,
    back_edge_source_facts: RefCell<BTreeMap<crate::canonical_safewir::BlockId, Vec<crate::canonical_safewir::BlockId>>>,
    natural_loop_facts: RefCell<BTreeMap<crate::canonical_safewir::BlockId, BTreeSet<crate::canonical_safewir::BlockId>>>,
    reaches_avoiding_facts: RefCell<BTreeMap<(crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId), bool>>,
    immediate_dominators: std::cell::OnceCell<BTreeMap<crate::canonical_safewir::BlockId, crate::canonical_safewir::BlockId>>,
    block_slots: std::cell::OnceCell<usize>,
    /// Memoized path judgments keyed by the exact normalized merge set. This
    /// mirrors the encoder's bounded graph decision, including cached cyclic
    /// refusals, without weakening the independent plan comparison.
    path_facts: RefCell<BTreeMap<Vec<crate::canonical_safewir::BlockId>, std::rc::Rc<RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>>>>,
    /// Blocks that only reach a trap, derived from the source CFG.
    doomed: BTreeSet<crate::canonical_safewir::BlockId>,
}

impl<'a> PlanEmitter<'a> {
    fn block(&self, id: crate::canonical_safewir::BlockId) -> Result<&'a BasicBlock, CanonicalAdmissionError> {
        self.blocks_by_id.get(&id).copied().ok_or(CanonicalAdmissionError::UnsupportedPlan)
    }

    fn successors(&self, id: crate::canonical_safewir::BlockId) -> Result<Vec<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        self.successors_by_block.get(&id).cloned().ok_or(CanonicalAdmissionError::UnsupportedPlan)
    }

    fn successor_slice(&self, id: crate::canonical_safewir::BlockId) -> Result<&[crate::canonical_safewir::BlockId], CanonicalAdmissionError> {
        self.successors_by_block.get(&id).map(Vec::as_slice).ok_or(CanonicalAdmissionError::UnsupportedPlan)
    }

    /// `distances_from` stopping at `stops` (which are never entered).
    fn distances_until(
        &self,
        start: crate::canonical_safewir::BlockId,
        stops: &[crate::canonical_safewir::BlockId],
    ) -> Result<foldhash::HashMap<crate::canonical_safewir::BlockId, usize>, CanonicalAdmissionError> {
        // Hashed: `find_join` only probes and ranks the entries.
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

    fn distances_from(&self, start: crate::canonical_safewir::BlockId) -> Result<BTreeMap<crate::canonical_safewir::BlockId, usize>, CanonicalAdmissionError> {
        let mut distances = BTreeMap::new();
        let mut pending = VecDeque::from([(start, 0)]);
        while let Some((id, distance)) = pending.pop_front() {
            if distances.contains_key(&id) {
                continue;
            }
            self.block(id)?;
            distances.insert(id, distance);
            pending.extend(self.successors(id)?.into_iter().map(|successor| (successor, distance + 1)));
        }
        Ok(distances)
    }

    fn reaches(&self, start: crate::canonical_safewir::BlockId, goal: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
        // Independently walk the checked-source graph. The complete edge
        // census in expected_function_plan rejects missing successors before
        // traversal; finding the goal cannot hide a malformed sibling edge.
        let mut pending = vec![start];
        let mut seen = BTreeSet::new();
        while let Some(current) = pending.pop() {
            if !seen.insert(current) { continue; }
            self.block(current)?;
            if current == goal { return Ok(true); }
            let successors = self.successors_by_block.get(&current)
                .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            pending.extend(successors.iter().rev().copied());
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
    ) -> Result<bool, CanonicalAdmissionError> {
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

    fn is_terminal(&self, id: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
        Ok(matches!(
            self.block(id)?.terminator,
            Terminator::Return { .. }
                | Terminator::TailCall { .. }
                | Terminator::VarTailCall { .. }
                | Terminator::Trap
                | Terminator::Unreachable
        ))
    }

    /// Mirror of the encoder's exclusive terminal-tail judgment.
    fn is_exclusive_terminal_loop_tail(
        &self,
        start: crate::canonical_safewir::BlockId,
        header: crate::canonical_safewir::BlockId,
        members: &[crate::canonical_safewir::BlockId],
    ) -> Result<bool, CanonicalAdmissionError> {
        if self.reaches(start, header)? {
            return Ok(false);
        }
        let reach = self.reachable_until(start, &[header])?;
        if !self.all_paths_reach_or_terminate(start, &[], &mut BTreeSet::new())? {
            return Ok(false);
        }
        for reached in &reach {
            // Match the encoder's exclusivity rule for terminal sinks too.
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

    fn all_paths_reach_or_terminate(
        &self,
        start: crate::canonical_safewir::BlockId,
        merges: &[crate::canonical_safewir::BlockId],
        active: &mut BTreeSet<crate::canonical_safewir::BlockId>,
    ) -> Result<bool, CanonicalAdmissionError> {
        if merges.contains(&start) || self.is_terminal(start)? {
            return Ok(true);
        }
        let mut region = merges.to_vec();
        region.sort_unstable();
        let facts = self.path_query_facts(&region);
        self.all_paths_with_facts(start, &region, active, &facts)
    }

    fn path_query_facts(
        &self,
        sorted_stops: &[crate::canonical_safewir::BlockId],
    ) -> std::rc::Rc<RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>> {
        // Independent plan-side storage for the same exact query keys. A
        // group stays live across nested queries without holding a borrow of
        // the outer table, and its results never come from encoder decisions.
        let mut table = self.path_facts.borrow_mut();
        if let Some(existing) = table.get(sorted_stops) {
            return std::rc::Rc::clone(existing);
        }
        let nodes = std::rc::Rc::new(RefCell::new(BTreeMap::new()));
        table.insert(sorted_stops.to_vec(), std::rc::Rc::clone(&nodes));
        nodes
    }

    fn all_paths_with_facts(
        &self,
        start: crate::canonical_safewir::BlockId,
        merges: &[crate::canonical_safewir::BlockId],
        active: &mut BTreeSet<crate::canonical_safewir::BlockId>,
        facts: &RefCell<BTreeMap<crate::canonical_safewir::BlockId, bool>>,
    ) -> Result<bool, CanonicalAdmissionError> {
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
            body_merges.sort_unstable();
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

    fn is_loop_header(&self, id: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
        if let Some(known) = self.loop_headers.borrow().get(&id) {
            return Ok(*known);
        }
        // Mirror of the encoder's rule: a loop header is the target of a back
        // edge, a predecessor it dominates.
        let result = !self.back_edge_sources(id)?.is_empty();
        self.loop_headers.borrow_mut().insert(id, result);
        Ok(result)
    }

    /// Mirror of the encoder's `loop_exit`: the one target outside the natural
    /// loop, `None` for a loop that only leaves by terminating.
    fn loop_exit(&self, header: crate::canonical_safewir::BlockId) -> Result<Option<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        if let Some(known) = self.loop_exit_targets.borrow().get(&header) {
            return Ok(*known);
        }
        // One natural-loop set, read once: probing it per block per header
        // and scanning `members` per successor was quadratic in loop size.
        // Every member is a block of the function, so `is_member` answers
        // exactly what `members.contains` did.
        let natural = if self.source_function.blocks.iter().any(|block| block.id != header) {
            self.natural_loop_members(header)?
        } else {
            BTreeSet::new()
        };
        let mut members = vec![header];
        members.extend(self.source_function.blocks.iter().map(|block| block.id).filter(|id| *id != header && natural.contains(id)));
        let is_member = |block: &crate::canonical_safewir::BlockId| *block == header || natural.contains(block);
        // Mirror of the encoder's rule: the `while` header's `else` side and
        // every non-terminating edge out of the loop are exits; one exit is
        // the continuation, several continue at the nearest block they all
        // reach.
        let classic_exit = match &self.block(header)?.terminator {
            Terminator::BranchIf { else_target, .. } if !is_member(else_target) => Some(*else_target),
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
        // Mirror of the encoder: only an exclusive terminal tail is emitted
        // inline; shared terminal sinks need no duplicated cleanup.
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
                return Err(CanonicalAdmissionError::UnsupportedPlan);
            };
            Some(block)
        };
        self.loop_exit_targets.borrow_mut().insert(header, exit);
        Ok(exit)
    }

    fn escape_label(&self, target: crate::canonical_safewir::BlockId) -> Option<usize> {
        self.loops
            .iter()
            .rev()
            .find(|(header, _)| *header == target)
            .or_else(|| self.loop_exits.iter().rev().find(|(exit, _)| *exit == target))
            .map(|(_, label_index)| *label_index)
    }

    fn predecessors(&self, block: crate::canonical_safewir::BlockId) -> Result<Vec<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        self.predecessors_by_block.get(&block).cloned().ok_or(CanonicalAdmissionError::UnsupportedPlan)
    }

    fn reachable_until(&self, start: crate::canonical_safewir::BlockId, stops: &[crate::canonical_safewir::BlockId]) -> Result<foldhash::HashSet<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
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
    fn side_escapes(&self, current: crate::canonical_safewir::BlockId, side: crate::canonical_safewir::BlockId, other: crate::canonical_safewir::BlockId, stop: Option<crate::canonical_safewir::BlockId>) -> Result<bool, CanonicalAdmissionError> {
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

    fn reaches_avoiding(&self, start: crate::canonical_safewir::BlockId, goal: crate::canonical_safewir::BlockId, avoid: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
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

    fn dominates(&self, dominator: crate::canonical_safewir::BlockId, node: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
        let entry = self.source_function.entry;
        let idom = self
            .immediate_dominators
            .get_or_init(|| immediate_dominators(entry, &self.successors_by_block));
        Ok(dominates_in(idom, entry, dominator, node))
    }

    fn back_edge_sources(&self, header: crate::canonical_safewir::BlockId) -> Result<Vec<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
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

    fn natural_loop_members(&self, header: crate::canonical_safewir::BlockId) -> Result<BTreeSet<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
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

    fn in_natural_loop(&self, header: crate::canonical_safewir::BlockId, block: crate::canonical_safewir::BlockId) -> Result<bool, CanonicalAdmissionError> {
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
    ) -> Result<Option<crate::canonical_safewir::BlockId>, CanonicalAdmissionError> {
        if then_target == else_target {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        // Mirror of the encoder's rule: an escaping side is a plain `br` and
        // the other side's first block is where emission continues.
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
        // Mirror of the encoder's rule: a terminal side bypasses the merge and
        // contributes distance zero.
        // The other side's reach is taken without passing back through the
        // branching block: a loop back edge would otherwise make a returning
        // arm look reachable from the continuing side.
        // Mirror the enclosing-continuation boundary used by the encoder.
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
            // Mirror of the encoder's rule: a side's own first block can be
            // the merge; loop headers and exits never are.
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
                    &self.successors_by_block, &stops).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?
            {
                best = Some(score);
                break;
            }
        }
        // Mirror of the encoder: the enclosing continuation is the merge when
        // no earlier block is and every path reaches it or terminates.
        if best.is_none() && let Some(stop) = stop && self.escape_label(stop).is_none() {
            let mut merges = self.escape_targets();
            merges.push(stop);
            if self.all_paths_reach_or_terminate(then_target, &merges, &mut BTreeSet::new())?
                && self.all_paths_reach_or_terminate(else_target, &merges, &mut BTreeSet::new())?
                && !forward_cfg::bypasses_join_continuation(then_target, else_target, stop,
                    &self.successors_by_block, &stops).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?
            {
                best = Some((usize::MAX, stop));
            }
        }
        best.map(|(_, block)| Some(block)).ok_or(CanonicalAdmissionError::UnsupportedPlan)
    }

    fn label_depth(&self, index: usize) -> u32 {
        (self.labels.len() - 1 - index) as u32
    }

    fn push_branch_arguments(&mut self, target: crate::canonical_safewir::BlockId, arguments: &[ValueId]) -> Result<(), CanonicalAdmissionError> {
        let parameters = self.block(target)?.parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
        if parameters.len() != arguments.len() {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        for argument in arguments {
            push_get(&mut self.plan, self.layout, *argument)?;
        }
        for parameter in parameters.into_iter().rev() {
            push_set(&mut self.plan, self.layout, parameter)?;
        }
        Ok(())
    }

    fn push_terminator(&mut self, terminator: &Terminator) -> Result<(), CanonicalAdmissionError> {
        match terminator {
            Terminator::Return { values } => {
                for value in values {
                    push_get(&mut self.plan, self.layout, *value)?;
                }
                self.plan.push(PlanToken::Return);
            }
            Terminator::TailCall { callee, arguments, .. } => {
                for argument in arguments {
                    push_get(&mut self.plan, self.layout, plan_call_argument(argument)?)?;
                }
                self.plan.push(PlanToken::ReturnCall(self.function_indices.get(callee).copied().ok_or(CanonicalAdmissionError::UnsupportedPlan)?));
            }
            Terminator::VarTailCall { callee, signature, arguments, .. } => {
                for argument in arguments {
                    push_get(&mut self.plan, self.layout, plan_call_argument(argument)?)?;
                }
                push_get(&mut self.plan, self.layout, *callee)?;
                self.plan.push(PlanToken::ReturnCallIndirect {
                    type_index: *self
                        .signature_indices
                        .get(signature)
                        .ok_or(CanonicalAdmissionError::UnsupportedPlan)?,
                    table_index: 0,
                });
            }
            Terminator::Trap | Terminator::Unreachable => self.plan.push(PlanToken::Unreachable),
            Terminator::Branch { .. } | Terminator::BranchIf { .. } => {
                return Err(CanonicalAdmissionError::UnsupportedPlan);
            }
        }
        Ok(())
    }

    fn emit_path(&mut self, start: crate::canonical_safewir::BlockId, stop: Option<crate::canonical_safewir::BlockId>) -> Result<(), CanonicalAdmissionError> {
        if let Some(label_index) = self.escape_label(start) {
            self.plan.push(PlanToken::Br(self.label_depth(label_index)));
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
                .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            // A region already entered once (a copied tail) is emitted block
            // by block, where a second entry takes the bounded copy.
            if let Some(order) = order && !order.iter().any(|block| self.emitted.contains(block)) {
                return self.emit_forward(&order);
            }
        }
        if !self.emitted.insert(start) {
            // Match the encoder's bounded shared-continuation clone exactly.
            let cloneable = forward_cfg::repeatable_region(start, stop,
                &self.successors_by_block, &self.escape_targets(), &self.emitted,
                64usize.saturating_sub(self.cloned_blocks)).ok().flatten().is_some();
            if !cloneable { return Err(CanonicalAdmissionError::UnsupportedPlan); }
            self.cloned_blocks += 1;
        }
        let block = self.block(start)?;
        self.plan.extend(expected_block_operations_plan(
            self.source_function,
            block,
            self.layout,
            self.layouts,
        self.function_indices,
        self.signature_indices,
            self.imports,
            self.import_indices,
            self.static_data_offsets,
            &mut self.aggregates,
            self.owner_cleanup_function,
            &self.static_values,
        )?);
        self.emit_block_terminator(start, &block.terminator, stop)
    }

    fn emit_block_terminator(&mut self, start: crate::canonical_safewir::BlockId, terminator: &Terminator, stop: Option<crate::canonical_safewir::BlockId>) -> Result<(), CanonicalAdmissionError> {
        match terminator {
            Terminator::Branch { target, arguments } => {
                self.push_branch_arguments(*target, arguments)?;
                self.emit_path(*target, stop)
            }
            Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments } => {
                self.emit_if(start, *condition, *then_target, then_arguments, *else_target, else_arguments, stop)
            }
            terminator => self.push_terminator(terminator),
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
    ) -> Result<(), CanonicalAdmissionError> {
        if self.is_loop_header(current)? && !self.loops.iter().any(|(header, _)| *header == current) {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        let join = self.find_join(current, then_target, else_target, stop)?;
        push_get(&mut self.plan, self.layout, condition)?;
        // Mirror of the encoder: an `else` that leaves the innermost loop is
        // tested first under a negated condition.
        let (then_target, then_arguments, else_target, else_arguments) =
            if self.leaves_innermost_loop(else_target) && !self.leaves_innermost_loop(then_target) {
                self.plan.push(PlanToken::I32Eqz);
                (else_target, else_arguments, then_target, then_arguments)
            } else {
                (then_target, then_arguments, else_target, else_arguments)
            };
        self.plan.push(PlanToken::If("Empty".to_owned()));
        self.labels.push(StructuredLabel::If);
        self.push_branch_arguments(then_target, then_arguments)?;
        self.emit_path(then_target, join)?;
        self.plan.push(PlanToken::Else);
        self.push_branch_arguments(else_target, else_arguments)?;
        self.emit_path(else_target, join)?;
        self.plan.push(PlanToken::End);
        self.labels.pop();
        match join {
            Some(join) => self.emit_path(join, stop),
            None => Ok(()),
        }
    }

    fn leaves_innermost_loop(&self, target: crate::canonical_safewir::BlockId) -> bool {
        self.loop_exits.last().is_some_and(|(exit, _)| *exit == target)
    }

    fn emit_loop(&mut self, header: crate::canonical_safewir::BlockId, stop: Option<crate::canonical_safewir::BlockId>) -> Result<(), CanonicalAdmissionError> {
        if !self.emitted.insert(header) {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        if !self.is_loop_header(header)? {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        let block = self.block(header)?;
        let exit = self.loop_exit(header)?;
        self.plan.push(PlanToken::Block("Empty".to_owned()));
        let block_label = self.labels.len();
        self.labels.push(StructuredLabel::Block);
        self.plan.push(PlanToken::Loop("Empty".to_owned()));
        let loop_label = self.labels.len();
        self.labels.push(StructuredLabel::Loop);
        self.plan.extend(expected_block_operations_plan(self.source_function, block, self.layout, self.layouts, self.function_indices, self.signature_indices, self.imports, self.import_indices, self.static_data_offsets, &mut self.aggregates, self.owner_cleanup_function, &self.static_values)?);
        self.loops.push((header, loop_label));
        if let Some(exit) = exit {
            self.loop_exits.push((exit, block_label));
        }
        match &block.terminator {
            // Mirror of the encoder's canonical `while` form.
            Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments } if Some(*else_target) == exit => {
                push_get(&mut self.plan, self.layout, *condition)?;
                self.plan.push(PlanToken::I32Eqz);
                self.plan.push(PlanToken::If("Empty".to_owned()));
                self.labels.push(StructuredLabel::If);
                self.push_branch_arguments(*else_target, else_arguments)?;
                self.plan.push(PlanToken::Br(self.label_depth(block_label)));
                self.plan.push(PlanToken::End);
                self.labels.pop();
                self.push_branch_arguments(*then_target, then_arguments)?;
                self.emit_path(*then_target, Some(header))?;
            }
            terminator => self.emit_block_terminator(header, terminator, Some(header))?,
        }
        if exit.is_some() {
            self.loop_exits.pop();
        }
        self.loops.pop();
        self.plan.push(PlanToken::End);
        self.labels.pop();
        self.plan.push(PlanToken::End);
        self.labels.pop();
        match exit {
            Some(exit) => self.emit_path(exit, stop),
            None => {
                self.plan.push(PlanToken::Unreachable);
                Ok(())
            }
        }
    }
}

/// One canonical spelling for a local's value type so the independently
/// decoded body and the planned body compare kinds, not two libraries' `Debug`
/// renderings (which agree for numerics and differ for reference types).
/// The plan's comparison table. It must be the exact mirror of `emit_compare`:
/// `Int` compares as i64, `Size` and `Bool` as i32, `Float` as f64; every
/// operator is covered so the plan never refuses a comparison the encoder
/// emits.
/// The plan's arithmetic table, the exact mirror of `emit_arithmetic`.
pub(super) fn planned_arithmetic_token(
    value_type: crate::canonical_safewir::ValueType,
    operator: ArithmeticOperator,
) -> Result<PlanToken, CanonicalAdmissionError> {
    use crate::canonical_safewir::ValueType as V;
    use ArithmeticOperator as A;
    Ok(match value_type {
        V::Int => match operator {
            A::Add => PlanToken::I64Add,
            A::Subtract => PlanToken::I64Sub,
            A::Multiply => PlanToken::I64Mul,
            A::DivideSigned => PlanToken::I64DivS,
            A::DivideUnsigned => PlanToken::I64DivU,
            A::RemainderSigned => PlanToken::I64RemS,
            A::RemainderUnsigned => PlanToken::I64RemU,
            A::BitAnd => PlanToken::I64And,
            A::BitOr => PlanToken::I64Or,
            A::BitXor => PlanToken::I64Xor,
            A::ShiftLeft => PlanToken::I64Shl,
            A::ShiftRightSigned => PlanToken::I64ShrS,
            A::ShiftRightUnsigned => PlanToken::I64ShrU,
        },
        V::Size => match operator {
            A::Add => PlanToken::I32Add,
            A::Subtract => PlanToken::I32Sub,
            A::Multiply => PlanToken::I32Mul,
            A::DivideSigned => PlanToken::I32DivS,
            A::DivideUnsigned => PlanToken::I32DivU,
            A::RemainderSigned => PlanToken::I32RemS,
            A::RemainderUnsigned => PlanToken::I32RemU,
            A::BitAnd => PlanToken::I32And,
            A::BitOr => PlanToken::I32Or,
            A::BitXor => PlanToken::I32Xor,
            A::ShiftLeft => PlanToken::I32Shl,
            A::ShiftRightSigned => PlanToken::I32ShrS,
            A::ShiftRightUnsigned => PlanToken::I32ShrU,
        },
        V::Float => match operator {
            A::Add => PlanToken::F64Add,
            A::Subtract => PlanToken::F64Sub,
            A::Multiply => PlanToken::F64Mul,
            A::DivideSigned | A::DivideUnsigned => PlanToken::F64Div,
            _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
        },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    })
}

pub(super) fn planned_compare_token(
    value_type: crate::canonical_safewir::ValueType,
    operator: ComparisonOperator,
) -> Result<PlanToken, CanonicalAdmissionError> {
    use crate::canonical_safewir::ValueType as V;
    Ok(match value_type {
        V::Int => match operator {
            ComparisonOperator::Equal => PlanToken::I64Eq,
            ComparisonOperator::NotEqual => PlanToken::I64Ne,
            ComparisonOperator::LessSigned => PlanToken::I64LtS,
            ComparisonOperator::LessUnsigned => PlanToken::I64LtU,
            ComparisonOperator::LessEqualSigned => PlanToken::I64LeS,
            ComparisonOperator::LessEqualUnsigned => PlanToken::I64LeU,
            ComparisonOperator::GreaterSigned => PlanToken::I64GtS,
            ComparisonOperator::GreaterUnsigned => PlanToken::I64GtU,
            ComparisonOperator::GreaterEqualSigned => PlanToken::I64GeS,
            ComparisonOperator::GreaterEqualUnsigned => PlanToken::I64GeU,
        },
        V::Size | V::Bool => match operator {
            ComparisonOperator::Equal => PlanToken::I32Eq,
            ComparisonOperator::NotEqual => PlanToken::I32Ne,
            ComparisonOperator::LessSigned => PlanToken::I32LtS,
            ComparisonOperator::LessUnsigned => PlanToken::I32LtU,
            ComparisonOperator::LessEqualSigned => PlanToken::I32LeS,
            ComparisonOperator::LessEqualUnsigned => PlanToken::I32LeU,
            ComparisonOperator::GreaterSigned => PlanToken::I32GtS,
            ComparisonOperator::GreaterUnsigned => PlanToken::I32GtU,
            ComparisonOperator::GreaterEqualSigned => PlanToken::I32GeS,
            ComparisonOperator::GreaterEqualUnsigned => PlanToken::I32GeU,
        },
        V::Float => match operator {
            ComparisonOperator::Equal => PlanToken::F64Eq,
            ComparisonOperator::NotEqual => PlanToken::F64Ne,
            ComparisonOperator::LessSigned | ComparisonOperator::LessUnsigned => PlanToken::F64Lt,
            ComparisonOperator::LessEqualSigned | ComparisonOperator::LessEqualUnsigned => PlanToken::F64Le,
            ComparisonOperator::GreaterSigned | ComparisonOperator::GreaterUnsigned => PlanToken::F64Gt,
            ComparisonOperator::GreaterEqualSigned | ComparisonOperator::GreaterEqualUnsigned => PlanToken::F64Ge,
        },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    })
}

pub(super) fn planned_local_kind(ty: ValType) -> String {
    match ty {
        ValType::I32 => "i32".to_owned(),
        ValType::I64 => "i64".to_owned(),
        ValType::F32 => "f32".to_owned(),
        ValType::F64 => "f64".to_owned(),
        ValType::V128 => "v128".to_owned(),
        ValType::Ref(reference) if reference == wasm_encoder::RefType::EXTERNREF => "externref".to_owned(),
        ValType::Ref(reference) if reference == wasm_encoder::RefType::FUNCREF => "funcref".to_owned(),
        ValType::Ref(reference) if reference == wasm_encoder::RefType::ANYREF => "anyref".to_owned(),
        other => format!("{other:?}"),
    }
}

pub(super) fn decoded_local_kind(ty: wasmparser::ValType) -> String {
    match ty {
        wasmparser::ValType::I32 => "i32".to_owned(),
        wasmparser::ValType::I64 => "i64".to_owned(),
        wasmparser::ValType::F32 => "f32".to_owned(),
        wasmparser::ValType::F64 => "f64".to_owned(),
        wasmparser::ValType::V128 => "v128".to_owned(),
        wasmparser::ValType::Ref(reference) if reference.is_extern_ref() => "externref".to_owned(),
        wasmparser::ValType::Ref(reference) if reference.is_func_ref() => "funcref".to_owned(),
        wasmparser::ValType::Ref(reference) if reference == wasmparser::RefType::ANYREF => "anyref".to_owned(),
        other => format!("{other:?}"),
    }
}

pub(super) fn expected_function_plan(
    function: &Function,
    signature: &FunctionSignature,
    layouts: &LayoutInterner,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    imports: &BTreeMap<ImportId, ImportContract>,
    import_indices: &BTreeMap<ImportId, u32>,
    static_data_offsets: &BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    owner_cleanup_function: Option<u32>,
    owner_bridges: &[AdmittedOwnerBridge],
    memory_helpers: &std::sync::Arc<outlined_memory::Helpers>,
) -> Result<(Vec<(u32, String)>, Vec<PlanToken>), CanonicalAdmissionError> {
    // The planned local declarations come from this same layout, read before
    // planning, rather than from a second `local_layout` of the function by
    // the caller (the layout's slot verification was ~7% of admission).
    let mut layout = local_layout(function, signature, layouts).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    layout.owner_bridges = owner_bridges.iter().collect();
    layout.memory_helpers = Some(memory_helpers.clone());
    layout.adopt_refs(memory_helpers.refs, signature.parameters.len());
    let locals = layout.extra_types.iter().map(|ty| (1, planned_local_kind(*ty))).collect();
    let successors_by_block: BTreeMap<_, _> = function
        .blocks
        .iter()
        .map(|block| {
            let successors = match &block.terminator {
                Terminator::Branch { target, .. } => vec![*target],
                Terminator::BranchIf { then_target, else_target, .. } => {
                    vec![*then_target, *else_target]
                }
                Terminator::Return { .. }
                | Terminator::TailCall { .. }
                | Terminator::VarTailCall { .. }
                | Terminator::Trap
                | Terminator::Unreachable => Vec::new(),
            };
            (block.id, successors)
        })
        .collect();
    let mut predecessors_by_block: BTreeMap<_, Vec<_>> = function
        .blocks
        .iter()
        .map(|block| (block.id, Vec::new()))
        .collect();
    for (source, successors) in &successors_by_block {
        for successor in successors {
            let predecessors = predecessors_by_block
                .get_mut(successor)
                .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            if !predecessors.contains(source) {
                predecessors.push(*source);
            }
        }
    }
    let doomed = forward_cfg::doomed(&successors_by_block, function.blocks.iter()
        .filter(|block| matches!(block.terminator, Terminator::Trap | Terminator::Unreachable))
        .map(|block| block.id));
    let mut emitter = PlanEmitter {
        source_function: function,
        static_values: static_values::static_values(function),
        blocks_by_id: function.blocks.iter().map(|block| (block.id, block)).collect(),
        layout: &layout,
        layouts,
        function_indices,
        signature_indices,
        imports,
        import_indices,
        static_data_offsets,
        owner_cleanup_function,
        successors_by_block,
        predecessors_by_block,
        plan: Vec::new(),
        emitted: BTreeSet::new(),
        cloned_blocks: 0,
        labels: Vec::new(),
        loops: Vec::new(),
        loop_exits: Vec::new(),
        aggregates: BTreeMap::new(),
        loop_headers: RefCell::new(BTreeMap::new()),
        loop_exit_targets: RefCell::new(BTreeMap::new()),
        back_edge_source_facts: RefCell::new(BTreeMap::new()),
        natural_loop_facts: RefCell::new(BTreeMap::new()),
        reaches_avoiding_facts: RefCell::new(BTreeMap::new()),
        immediate_dominators: std::cell::OnceCell::new(),
        block_slots: std::cell::OnceCell::new(),
        path_facts: RefCell::new(BTreeMap::new()),
        doomed,
    };
    match forward_cfg::schedule(function.entry, &emitter.successors_by_block)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?
    {
        Some(order) => emitter.emit_forward(&order)?,
        None => emitter.emit_path(function.entry, None)?,
    }
    if emitter.emitted.len() != function.blocks.len() {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    emitter.plan.push(PlanToken::End);
    Ok((locals, emitter.plan))
}

pub(super) fn expected_metadata_prefix(
    source: &CanonicalModule<InitialValid>,
    target: &WasmIr,
) -> Result<Vec<u8>, CanonicalAdmissionError> {
    let refs = ref_encoding::RefEncoding::for_heap(
        witchy_canonical_source::capabilities::HeapMode::from_launch_contract(source.launch_contract_payload()));
    let mut types = TypeSection::new();
    for signature in source.signatures() {
        let parameters = signature.parameters.iter().enumerate()
            .map(|(index, value_type)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(index as u32), *value_type))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        let results = signature.results.iter().enumerate()
            .map(|(index, value_type)| wasm_value_type(FunctionId::new(u32::MAX), ValueId::new(index as u32), *value_type))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        types.ty().function(parameters, results);
    }
    let signature_indices: BTreeMap<_, _> = source.signatures().iter().enumerate()
        .map(|(index, signature)| (signature.id, index as u32)).collect();
    // Both maps are functions of the module. Built inside the loop they made
    // this quadratic in the module's imports and signatures: every signature
    // was cloned once per import, and each contract came from a linear scan.
    let signatures_by_id: BTreeMap<_, _> = source.signatures().iter()
        .map(|signature| (signature.id, signature.clone())).collect();
    let contracts_by_id: BTreeMap<_, _> = source.imports().iter()
        .map(|contract| (contract.id, contract)).collect();
    let mut import_type_indices = BTreeMap::new();
    for (index, import) in target.imports.iter().enumerate() {
        let contract = *contracts_by_id.get(&import.id)
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        let (parameters, results) = import_wasm_signature(FunctionId::new(u32::MAX), contract, &signatures_by_id, refs)
            .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        let type_index = u32::try_from(source.signatures().len() + index)
            .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        types.ty().function(parameters, results);
        import_type_indices.insert(import.id, type_index);
    }
    owner_bridges::append_types(&mut types, &target.owner_bridges).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let owner_cleanup_bridge = owner_cleanup_bridge(&target.owner_bridges);
    let owner_cleanup_type = owner_cleanup_bridge.map(|_| {
        let index = types.len();
        types.ty().function([ValType::Ref(RefType::ANYREF)], []);
        index
    });
    let address_type = types.len();
    types.ty().function([ValType::I32; 3], [ValType::I32]);
    let drop_type = types.len();
    types.ty().function([ValType::I32], []);
    let retain_type = types.len();
    types.ty().function([ValType::I32], [ValType::I32]);
    let memory_helpers = outlined_memory::Helpers::new(source,
        (target.imports.len() + target.owner_bridges.len() + target.functions.len()) as u32 + u32::from(owner_cleanup_bridge.is_some()), refs)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let mut imports = ImportSection::new();
    for import in &target.imports {
        let type_index = import_type_indices.get(&import.id).copied()
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        // `contracts_by_id` above already indexes exactly this; scanning
        // here was the O(imports^2) half of the same lookup.
        let contract = *contracts_by_id.get(&import.id)
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        imports.import(&contract.module, &contract.name, EntityType::Function(type_index));
    }
    owner_bridges::append_imports(&mut imports, &target.owner_bridges, (source.signatures().len() + source.imports().len()) as u32);
    let mut functions = FunctionSection::new();
    let mut tables = TableSection::new();
    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    // The allocator's three globals are exported read-only for the host's
    // heap audit (a launcher may check that an admitted program released
    // every object); they grant no authority the module does not already
    // exercise on its own memory.
    exports.export(HEAP_TOP_EXPORT, ExportKind::Global, 0);
    exports.export(HEAP_FREE_EXPORT, ExportKind::Global, 1);
    exports.export(HEAP_BASE_EXPORT, ExportKind::Global, 2);
    memory_op_counters::declare_exports(&mut exports);
    let import_count = u32::try_from(target.imports.len() + target.owner_bridges.len()).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    for (index, function) in target.functions.iter().enumerate() {
        let type_index = signature_indices.get(&function.signature).copied()
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        functions.function(type_index);
        exports.export(&format!("f{}", function.id.get()), ExportKind::Func, import_count + index as u32);
    }
    if let Some(type_index) = owner_cleanup_type {
        functions.function(type_index);
    }
    functions.function(address_type);
    for _ in memory_helpers.drops.keys() { functions.function(drop_type); }
    functions.function(retain_type);
    functions.function(retain_type);
    if let Some(table) = &target.table {
        tables.table(TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: table.functions.len() as u64,
            maximum: None,
            shared: false,
        });
    }
    let mut memory = MemorySection::new();
    memory.memory(MemoryType { minimum: initial_memory_pages(target.heap_start), maximum: Some(max_memory_pages(source.launch_contract_payload())), memory64: true, shared: false, page_size_log2: None });
    let mut globals = GlobalSection::new();
    globals.global(GlobalType { val_type: ValType::I32, mutable: true, shared: false }, &ConstExpr::i32_const(refs.reference(target.heap_start) as i32));
    globals.global(GlobalType { val_type: ValType::I32, mutable: true, shared: false }, &ConstExpr::i32_const(0));
    globals.global(GlobalType { val_type: ValType::I32, mutable: false, shared: false }, &ConstExpr::i32_const(refs.reference(target.heap_start) as i32));
    memory_op_counters::declare_globals(&mut globals);
    let mut module = WasmModule::new();
    module.section(&types).section(&imports).section(&functions);
    if target.table.is_some() {
        module.section(&tables);
    }
    module.section(&memory).section(&globals).section(&exports);
    Ok(module.finish())
}

pub(super) fn decode_and_match_plan(
    source: &CanonicalModule<InitialValid>,
    target: &WasmIr,
    bytes: &[u8],
) -> Result<TargetWasmWitness, CanonicalAdmissionError> {
    wasmparser::Validator::new_with_features(
        wasmparser::WasmFeatures::default() | wasmparser::WasmFeatures::TAIL_CALL,
    )
    .validate_all(bytes)
    .map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
    let metadata = expected_metadata_prefix(source, target)?;
    if bytes.len() < metadata.len() || bytes[..metadata.len()] != metadata[..] {
        return Err(CanonicalAdmissionError::PlanMismatch("module metadata"));
    }
    let refs = ref_encoding::RefEncoding::for_heap(
        witchy_canonical_source::capabilities::HeapMode::from_launch_contract(source.launch_contract_payload()));
    let (expected_static_data, expected_heap_start) = static_data_plan(source.static_data_segments(), refs)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if target.heap_start != expected_heap_start || target.static_data != expected_static_data {
        return Err(CanonicalAdmissionError::PlanMismatch("static-data lowering"));
    }
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&source.layouts().canonical_bytes())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let import_count = u32::try_from(target.imports.len() + target.owner_bridges.len()).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let owner_cleanup_bridge = owner_cleanup_bridge(&target.owner_bridges);
    let owner_cleanup_function = owner_cleanup_bridge
        .map(|_| import_count + target.functions.len() as u32);
    let memory_helpers = outlined_memory::Helpers::new(source, import_count + target.functions.len() as u32 + u32::from(owner_cleanup_bridge.is_some()), refs)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let function_indices: BTreeMap<_, _> = target.functions.iter().enumerate()
        .map(|(index, function)| (function.id, import_count + index as u32))
        .collect();
    let signature_indices: BTreeMap<_, _> = source.signatures().iter().enumerate()
        .map(|(index, signature)| (signature.id, index as u32))
        .collect();
    let mut import_indices: BTreeMap<_, _> = target.imports.iter().enumerate()
        .map(|(index, import)| (import.id, index as u32)).collect();
    for (index, bridge) in target.owner_bridges.iter().enumerate() { import_indices.insert(bridge.id(), target.imports.len() as u32 + index as u32); }
    let static_data_offsets: BTreeMap<_, _> = expected_static_data.iter()
        .map(|segment| (segment.id, refs.reference(segment.offset)))
        .collect();
    // Both of these are functions of the module, not of one function in it.
    // Built inside the per-function closure they were quadratic: the import
    // map cloned every ImportContract (two Strings and two Vecs each) once per
    // function, and the signature came from a linear scan of every signature.
    let imports_by_id: BTreeMap<_, _> = source.imports().iter()
        .map(|import| (import.id, import.clone())).collect();
    let signatures_by_id: BTreeMap<_, _> = source.signatures().iter()
        .map(|signature| (signature.id, signature)).collect();
    let plan = |function: &Function| {
        let signature = *signatures_by_id.get(&function.signature)
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        expected_function_plan(function, signature, &layouts, &function_indices, &signature_indices, &imports_by_id, &import_indices, &static_data_offsets, owner_cleanup_function, &target.owner_bridges, &memory_helpers)
    };
    // Each function's plan reads only the shared module inputs above.
    let workers = crate::parallel::workers(source.functions().len());
    let mut expected = if workers > 1 {
        crate::parallel::map_ordered(source.functions(), workers, plan)
            .into_iter().collect::<Result<Vec<_>, _>>()?
    } else {
        source.functions().iter().map(plan).collect::<Result<Vec<_>, _>>()?
    };
    if let Some(bridge) = owner_cleanup_bridge {
        expected.push(owner_operation_kernel::plan_cleanup_helper(&layouts, bridge, refs)?);
    }
    expected.push(outlined_memory::plan_address());
    for root in memory_helpers.drops.keys() {
        expected.push(outlined_memory::plan_drop(&layouts, *root, refs)?);
    }
    expected.push(outlined_memory::plan_retain(refs));
    expected.push(outlined_memory::plan_alloc(refs));
    let mut actual = Vec::new();
    let mut code_count = None;
    let mut section_order = Vec::new();
    let mut code_started = false;
    let mut actual_data = Vec::new();
    let mut actual_table = None;
    let mut actual_elements = None;
    let mut actual_launch_contract = None;
    let mut actual_entry: Option<Vec<u8>> = None;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload.map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))? {
            wasmparser::Payload::Version { .. } => {}
            wasmparser::Payload::TypeSection(_) => section_order.push(0),
            wasmparser::Payload::ImportSection(_) => section_order.push(1),
            wasmparser::Payload::FunctionSection(_) => section_order.push(2),
            wasmparser::Payload::TableSection(reader) => {
                if actual_table.is_some() {
                    return Err(CanonicalAdmissionError::PlanMismatch("duplicate table section"));
                }
                let tables = reader.into_iter().collect::<Result<Vec<_>, _>>().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                if tables.len() != 1 {
                    return Err(CanonicalAdmissionError::PlanMismatch("table count"));
                }
                let table = &tables[0];
                actual_table = Some((
                    table.ty.element_type == wasmparser::RefType::FUNCREF,
                    table.ty.table64,
                    table.ty.initial,
                    table.ty.maximum,
                    table.ty.shared,
                ));
                section_order.push(3);
            }
            wasmparser::Payload::MemorySection(_) => section_order.push(4),
            wasmparser::Payload::GlobalSection(_) => section_order.push(5),
            wasmparser::Payload::ExportSection(_) => section_order.push(6),
            wasmparser::Payload::ElementSection(reader) => {
                if actual_elements.is_some() {
                    return Err(CanonicalAdmissionError::PlanMismatch("duplicate element section"));
                }
                let elements = reader.into_iter().collect::<Result<Vec<_>, _>>().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                if elements.len() != 1 {
                    return Err(CanonicalAdmissionError::PlanMismatch("element count"));
                }
                let element = &elements[0];
                let wasmparser::ElementKind::Active { table_index, offset_expr } = &element.kind else {
                    return Err(CanonicalAdmissionError::PlanMismatch("element kind"));
                };
                if *table_index != None {
                    return Err(CanonicalAdmissionError::PlanMismatch("element table"));
                }
                let mut offset = offset_expr.get_operators_reader();
                if !matches!(offset.read().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?, wasmparser::Operator::I32Const { value: 0 })
                    || !matches!(offset.read().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?, wasmparser::Operator::End)
                {
                    return Err(CanonicalAdmissionError::PlanMismatch("element offset"));
                }
                let wasmparser::ElementItems::Functions(reader) = &element.items else {
                    return Err(CanonicalAdmissionError::PlanMismatch("element expressions"));
                };
                actual_elements = Some(reader.clone().into_iter().collect::<Result<Vec<_>, _>>().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?);
                section_order.push(7);
            }
            wasmparser::Payload::CodeSectionStart { count, .. } => {
                if code_count.replace(count).is_some() {
                    return Err(CanonicalAdmissionError::PlanMismatch("duplicate code section"));
                }
                section_order.push(8);
                code_started = true;
            }
            wasmparser::Payload::CodeSectionEntry(body) => {
                if !code_started {
                    return Err(CanonicalAdmissionError::PlanMismatch("code body before section"));
                }
                let mut locals_reader = body.get_locals_reader().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                let mut locals = Vec::new();
                for _ in 0..locals_reader.get_count() {
                    let (count, ty) = locals_reader.read().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                    locals.push((count, decoded_local_kind(ty)));
                }
                let mut operators = body.get_operators_reader().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                let mut plan = Vec::new();
                while !operators.eof() {
                    plan.push(decode_plan_token(operators.read().map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?)?);
                }
                actual.push((locals, plan));
            }
            wasmparser::Payload::DataSection(reader) => {
                section_order.push(9);
                for data in reader {
                    let data = data.map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                    let wasmparser::DataKind::Active { memory_index, offset_expr } = data.kind else {
                        return Err(CanonicalAdmissionError::PlanMismatch("passive data segment"));
                    };
                    if memory_index != 0 {
                        return Err(CanonicalAdmissionError::PlanMismatch("nonzero data memory"));
                    }
                    let mut operators = offset_expr.get_operators_reader();
                    // The 64-bit memory takes an i64 offset; static data
                    // still lies below 4 GiB.
                    let wasmparser::Operator::I64Const { value } = operators
                        .read()
                        .map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?
                    else {
                        return Err(CanonicalAdmissionError::PlanMismatch("dynamic data offset"));
                    };
                    let offset = u32::try_from(value)
                        .map_err(|_| CanonicalAdmissionError::PlanMismatch("data offset outside 32-bit static data"))?;
                    actual_data.push((offset, data.data.to_vec()));
                }
            }
            // Branch hints are advisory layout metadata: an engine may only
            // move a hinted arm out of line, never change which arm runs. They
            // must decode completely so none is silently discarded.
            wasmparser::Payload::CustomSection(section) if section.name() == "metadata.code.branch_hint" => {
                let wasmparser::KnownCustom::BranchHints(reader) = section.as_known() else {
                    return Err(CanonicalAdmissionError::PlanMismatch("branch hint section"));
                };
                for function in reader {
                    let function = function.map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                    for hint in function.hints {
                        hint.map_err(|error| CanonicalAdmissionError::WasmValidation(error.to_string()))?;
                    }
                }
                if section_order.contains(&12) {
                    return Err(CanonicalAdmissionError::PlanMismatch("duplicate branch hint section"));
                }
                section_order.push(12);
            }
            wasmparser::Payload::CustomSection(section) => match section.name() {
                "witchy.launch" if actual_launch_contract.replace(section.data().to_vec()).is_none() => {
                    section_order.push(10);
                }
                ENTRY_SECTION if actual_entry.replace(section.data().to_vec()).is_none() => {
                    section_order.push(11);
                }
                _ => return Err(CanonicalAdmissionError::PlanMismatch("launch contract section")),
            },
            wasmparser::Payload::End(_) => {}
            _ => return Err(CanonicalAdmissionError::PlanMismatch("unexpected Wasm section")),
        }
    }
    let expected_count = u32::try_from(expected.len()).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    // The canonical encoder deliberately emits every fixed metadata section,
    // including an empty import section. Match that exact byte policy rather
    // than inferring section presence from the logical import count.
    let mut expected_sections = vec![0, 1, 2];
    if target.table.is_some() {
        expected_sections.push(3);
    }
    expected_sections.extend([4, 5, 6]);
    if target.table.is_some() {
        expected_sections.push(7);
    }
    // Optional: present exactly when some emitted branch has a doomed arm.
    if section_order.contains(&12) {
        expected_sections.push(12);
    }
    expected_sections.push(8);
    if !target.static_data.is_empty() {
        expected_sections.push(9);
    }
    expected_sections.push(10);
    if target.entry_payload.is_some() {
        expected_sections.push(11);
    }
    let expected_data = expected_static_data.iter().map(|segment| (segment.offset, segment.bytes.clone())).collect::<Vec<_>>();
    if section_order != expected_sections {
        return Err(CanonicalAdmissionError::SectionOrderMismatch {
            expected: expected_sections,
            actual: section_order,
        });
    }
    if code_count != Some(expected_count) {
        return Err(CanonicalAdmissionError::PlanMismatch("function count"));
    }
    if actual != expected {
        // Development diagnostic only: name the first token where the emitted
        // body and the independently derived plan diverge. Nothing is admitted
        // on this path; the refusal below is unchanged.
        if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
            for (index, (emitted, planned)) in actual.iter().zip(&expected).enumerate() {
                if emitted == planned {
                    continue;
                }
                if emitted.0 != planned.0 {
                    eprintln!("canonical plan mismatch: function {index} locals differ");
                    eprintln!("  actual:   {:?}", emitted.0);
                    eprintln!("  expected: {:?}", planned.0);
                }
                let divergence = emitted.1.iter().zip(&planned.1).position(|(a, e)| a != e).unwrap_or(emitted.1.len().min(planned.1.len()));
                let window = |tokens: &[PlanToken]| tokens.iter().skip(divergence.saturating_sub(6)).take(14).map(|t| format!("{t:?}")).collect::<Vec<_>>().join(" ");
                eprintln!("canonical plan mismatch: function {index} token {divergence} (actual {} / expected {})", emitted.1.len(), planned.1.len());
                eprintln!("  actual:   {}", window(emitted.1.as_slice()));
                eprintln!("  expected: {}", window(planned.1.as_slice()));
                break;
            }
            if actual.len() != expected.len() {
                eprintln!("canonical plan mismatch: {} functions emitted, {} planned", actual.len(), expected.len());
            }
        }
        return Err(CanonicalAdmissionError::PlanMismatch("function body"));
    }
    if actual_data != expected_data {
        return Err(CanonicalAdmissionError::PlanMismatch("data contents"));
    }
    if actual_launch_contract.as_deref() != Some(target.launch_contract_payload.as_slice()) {
        return Err(CanonicalAdmissionError::PlanMismatch("launch contract payload"));
    }
    if actual_entry != target.entry_payload {
        return Err(CanonicalAdmissionError::PlanMismatch("entry section payload"));
    }
    match &target.table {
        Some(table) => {
            let expected_table = (true, false, table.functions.len() as u64, None, false);
            if actual_table != Some(expected_table) {
                return Err(CanonicalAdmissionError::PlanMismatch("table definition"));
            }
            let expected_elements = table.functions.iter().map(|function| function_indices.get(function).copied().ok_or(CanonicalAdmissionError::UnsupportedPlan)).collect::<Result<Vec<_>, _>>()?;
            if actual_elements != Some(expected_elements) {
                return Err(CanonicalAdmissionError::PlanMismatch("table elements"));
            }
        }
        None => {
            if actual_table.is_some() || actual_elements.is_some() {
                return Err(CanonicalAdmissionError::PlanMismatch("unexpected table"));
            }
        }
    }
    Ok(TargetWasmWitness::from_decoded(
        bytes,
        &section_order,
        &actual,
        &actual_data,
    ))
}
