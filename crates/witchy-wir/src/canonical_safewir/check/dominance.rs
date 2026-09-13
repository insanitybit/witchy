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

#[cfg(test)]
mod tests {
    use super::*;

    // Independent set representation of the prior production algorithm. Keep
    // its empty-predecessor and unreachable-cycle rules, not just DAG cases.
    fn reference(entry: usize, predecessors: &[Vec<usize>]) -> Vec<BTreeSet<usize>> {
        let all: BTreeSet<_> = (0..predecessors.len()).collect();
        let mut result: Vec<_> = (0..predecessors.len()).map(|block| {
            if block == entry { BTreeSet::from([block]) } else { all.clone() }
        }).collect();
        loop {
            let mut changed = false;
            for (block, incoming) in predecessors.iter().enumerate() {
                if block == entry { continue; }
                let mut updated = incoming.iter().map(|predecessor| result[*predecessor].clone())
                    .reduce(|left, right| left.intersection(&right).copied().collect())
                    .unwrap_or_default();
                updated.insert(block);
                if result[block] != updated { result[block] = updated; changed = true; }
            }
            if !changed { return result; }
        }
    }

    fn compare(entry: usize, predecessors: &[Vec<usize>]) {
        let expected = reference(entry, predecessors);
        let actual = Dominators {
            // Deliberately sparse IDs: bit positions are not BlockId values.
            indices: (0..predecessors.len()).map(|index| (BlockId::new((index * 7 + 11) as u32), index)).collect(),
            rows: solve(entry, predecessors),
        };
        for (block, expected) in expected.iter().enumerate() {
            for definition in 0..predecessors.len() {
                assert_eq!(actual.dominates(BlockId::new((definition * 7 + 11) as u32), BlockId::new((block * 7 + 11) as u32)),
                    expected.contains(&definition), "entry {entry}, def {definition}, use {block}, graph {predecessors:?}");
            }
        }
        assert!(!actual.dominates(BlockId::new(u32::MAX), BlockId::new(11)));
        assert!(!actual.dominates(BlockId::new(11), BlockId::new(u32::MAX)));
    }

    #[test]
    fn dense_dominators_match_sets_for_every_three_block_graph() {
        for count in 1..=3 {
            for edges in 0..(1usize << (count * count)) {
                let predecessors: Vec<Vec<usize>> = (0..count).map(|block| {
                    (0..count).filter(|predecessor| edges & (1 << (block * count + predecessor)) != 0).collect()
                }).collect();
                for entry in 0..count { compare(entry, &predecessors); }
            }
        }
    }

    #[test]
    fn dense_dominators_match_sets_across_word_boundaries_and_unreachable_cycles() {
        for count in [63, 64, 65, 129, 130] {
            let mut graph: Vec<Vec<usize>> = (0..count).map(|block| {
                if block == 0 { vec![] } else { vec![block - 1] }
            }).collect();
            compare(0, &graph);
            graph[0] = vec![count - 1];
            graph[count / 2].push(count - 1);
            compare(count / 2, &graph);
            graph[count / 2].clear();
            graph[count - 1] = vec![count - 2];
            graph[count - 2] = vec![count - 1];
            compare(count / 2, &graph);
        }
    }
}
