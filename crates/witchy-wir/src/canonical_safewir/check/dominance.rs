//! The original all-block dominator fixed point, represented by dense bits.
//! This changes set storage, not the checker theory or CFG reachability policy.
use super::*;

pub(super) struct Dominators {
    indices: BTreeMap<BlockId, usize>,
    rows: Vec<Vec<u64>>,
}

impl Dominators {
    pub(super) fn dominates(&self, definition: BlockId, use_block: BlockId) -> bool {
        let (Some(&definition), Some(&use_block)) = (self.indices.get(&definition), self.indices.get(&use_block)) else {
            return false;
        };
        self.rows[use_block][definition / 64] & (1u64 << (definition % 64)) != 0
    }
}

pub(super) fn dominators(
    function: &Function,
    blocks: &BTreeMap<BlockId, &BasicBlock>,
    predecessors: &BTreeMap<BlockId, BTreeSet<BlockId>>,
) -> Result<Dominators, InitialCheckError> {
    // Retain the entry/reachable-successor validation performed by the old
    // implementation. The fixed point below still includes ALL blocks.
    let mut reachable = BTreeSet::new();
    let mut pending = vec![function.entry];
    while let Some(block) = pending.pop() {
        if reachable.insert(block) {
            let body = blocks.get(&block).copied().ok_or(InitialCheckError::MissingBlock {
                function: function.id, block,
            })?;
            pending.extend(successors(&body.terminator));
        }
    }
    let indices: BTreeMap<_, _> = blocks.keys().copied().enumerate()
        .map(|(index, block)| (block, index)).collect();
    let mut incoming: Vec<Vec<usize>> = Vec::with_capacity(indices.len());
    for block in blocks.keys() {
        let predecessors = predecessors.get(block).expect("all blocks have predecessor sets");
        incoming.push(predecessors.iter().map(|predecessor| {
            indices.get(predecessor).copied().ok_or(InitialCheckError::MissingBlock {
                function: function.id, block: *predecessor,
            })
        }).collect::<Result<Vec<_>, _>>()?);
    }
    let rows = solve(indices[&function.entry], &incoming);
    Ok(Dominators { indices, rows })
}

fn solve(entry: usize, predecessors: &[Vec<usize>]) -> Vec<Vec<u64>> {
    let count = predecessors.len();
    let mut all = vec![u64::MAX; count.div_ceil(64)];
    if count % 64 != 0 {
        *all.last_mut().expect("nonempty CFG") = (1u64 << (count % 64)) - 1;
    }
    let mut rows = vec![all; count];
    rows[entry].fill(0);
    rows[entry][entry / 64] = 1u64 << (entry % 64);
    let mut updated = vec![0; rows[entry].len()];
    loop {
        let mut changed = false;
        for (block, incoming) in predecessors.iter().enumerate() {
            if block == entry { continue; }
            if let Some((&first, rest)) = incoming.split_first() {
                updated.copy_from_slice(&rows[first]);
                for &predecessor in rest {
                    for (word, incoming_word) in updated.iter_mut().zip(&rows[predecessor]) {
                        *word &= *incoming_word;
                    }
                }
            } else {
                updated.fill(0);
            }
            updated[block / 64] |= 1u64 << (block % 64);
            if rows[block] != updated {
                rows[block].copy_from_slice(&updated);
                changed = true;
            }
        }
        if !changed { return rows; }
    }
}
