//! Deterministic forward-label schedule, derived only from CFG edges.
use std::collections::{BTreeMap, BTreeSet};
use crate::canonical_safewir::BlockId;

/// A cycle selects the existing loop encoder. Missing or unreachable blocks
/// are errors, never permission to omit part of a checked function.
pub(super) fn schedule(
    entry: BlockId,
    edges: &BTreeMap<BlockId, Vec<BlockId>>,
) -> Result<Option<Vec<BlockId>>, ()> {
    let mut reachable = BTreeSet::new();
    let mut pending = vec![entry];
    while let Some(block) = pending.pop() {
        let successors = edges.get(&block).ok_or(())?;
        if reachable.insert(block) { pending.extend(successors); }
    }
    if reachable.len() != edges.len() { return Err(()); }
    let mut incoming: BTreeMap<_, usize> = edges.keys().map(|block| (*block, 0)).collect();
    for successors in edges.values() {
        for target in successors { *incoming.get_mut(target).ok_or(())? += 1; }
    }
    let mut ready: BTreeSet<_> = incoming.iter()
        .filter_map(|(block, count)| (*count == 0).then_some(*block)).collect();
    let mut order = Vec::with_capacity(edges.len());
    while let Some(block) = ready.pop_first() {
        order.push(block);
        for target in &edges[&block] {
            let count = incoming.get_mut(target).ok_or(())?;
            *count -= 1;
            if *count == 0 { ready.insert(*target); }
        }
    }
    if order.len() != edges.len() { return Ok(None); }
    if order.first() != Some(&entry) { return Err(()); }
    Ok(Some(order))
}

/// Remaining labels nest from the last scheduled block to the next one.
/// A conditional edge has one additional enclosing Wasm `if` label.
pub(super) fn depth(current: usize, target: usize, conditional: bool) -> Option<u32> {
    let depth = target.checked_sub(current)?.checked_sub(1)?;
    u32::try_from(depth.checked_add(usize::from(conditional))?).ok()
}

/// An acyclic portion of an existing structured loop. Boundaries are only
/// the caller's active loop header/exit labels, not guessed forward joins.
pub(super) fn region(
    entry: BlockId,
    edges: &BTreeMap<BlockId, Vec<BlockId>>,
    boundaries: &[BlockId],
) -> Result<Option<Vec<BlockId>>, ()> {
    if boundaries.contains(&entry) { return Err(()); }
    let mut portion = BTreeMap::new();
    let mut pending = vec![entry];
    while let Some(block) = pending.pop() {
        if portion.contains_key(&block) { continue; }
        let mut successors = Vec::new();
        for target in edges.get(&block).ok_or(())? {
            if !edges.contains_key(target) { return Err(()); }
            if !boundaries.contains(target) { successors.push(*target); }
        }
        pending.extend(&successors);
        portion.insert(block, successors);
    }
    schedule(entry, &portion)
}
