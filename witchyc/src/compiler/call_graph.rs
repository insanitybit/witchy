//! Strongly connected components of the compiler's call graphs.

/// The strongly connected component of each node (Tarjan's algorithm, without recursion):
/// linear in nodes and edges.
pub(super) fn components(edges: &[Vec<usize>]) -> Vec<usize> {
    const UNSEEN: usize = usize::MAX;
    let n = edges.len();
    let (mut index, mut low, mut component) = (vec![UNSEEN; n], vec![0; n], vec![UNSEEN; n]);
    let (mut stack, mut on_stack) = (Vec::new(), vec![false; n]);
    let (mut next, mut found) = (0, 0);
    // Each frame holds a node and the number of edges already visited.
    let mut frames = Vec::new();
    for root in 0..n {
        if index[root] != UNSEEN {
            continue;
        }
        frames.push((root, 0usize));
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (node, ref mut done)) = frames.last_mut() {
            if let Some(&to) = edges[node].get(*done) {
                *done += 1;
                if index[to] == UNSEEN {
                    index[to] = next;
                    low[to] = next;
                    next += 1;
                    stack.push(to);
                    on_stack[to] = true;
                    frames.push((to, 0));
                } else if on_stack[to] {
                    low[node] = low[node].min(index[to]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if low[node] == index[node] {
                loop {
                    let member = stack.pop().expect("the component's nodes are on the stack");
                    on_stack[member] = false;
                    component[member] = found;
                    if member == node {
                        break;
                    }
                }
                found += 1;
            }
        }
    }
    component
}

#[cfg(test)]
mod component_tests {
    #[test]
    fn calls_in_one_cycle_share_a_component_and_others_do_not() {
        // 0 -> 1 -> 2 -> 0 is a cycle; 3 calls into it; 4 calls itself; 5 is alone.
        let edges = vec![vec![1], vec![2], vec![0, 3], vec![], vec![4], vec![]];
        let c = super::components(&edges);
        assert!(c[0] == c[1] && c[1] == c[2]);
        assert!(c[3] != c[0] && c[4] != c[0] && c[5] != c[4] && c[3] != c[5]);
        // A long chain does not use the Rust stack.
        let chain: Vec<Vec<usize>> = (0..200_000)
            .map(|i| {
                if i + 1 < 200_000 {
                    vec![i + 1]
                } else {
                    vec![0]
                }
            })
            .collect();
        let c = super::components(&chain);
        assert!(c.iter().all(|x| *x == c[0]));
    }
}
