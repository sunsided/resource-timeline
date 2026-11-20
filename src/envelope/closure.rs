//! Maximum-weight closure by minimum cut.
//!
//! A closure of a directed graph is a node set that contains every successor of its nodes.
//! The heaviest closure is found with the classic reduction (Picard, 1976): connect a source to
//! every node of positive weight and every node of negative weight to a sink, with capacities
//! equal to the absolute weights, and give every arc infinite capacity. The heaviest closure
//! weighs the sum of the positive weights minus the minimum cut. The graphs here are small
//! (the pending events at one tick), so a dense Edmonds-Karp suffices.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::Error;

/// The weight of the heaviest closure of the graph on `weights.len()` nodes in which
/// `arc(x, y)` means "`x` in the closure requires `y` in the closure".
///
/// The empty set is a closure, so the result is never negative. Costs `O(n^2)` memory and
/// `O(n^5)` time in the worst case for `n` nodes.
///
/// # Errors
///
/// - [`Error::Overflow`] if the sum of the absolute weights exceeds `i128`.
/// - [`Error::OutOfMemory`] if the capacity matrix cannot be allocated.
pub(crate) fn max_weight_closure(
    weights: &[i128],
    arc: impl Fn(usize, usize) -> bool,
) -> Result<i128, Error> {
    let positive = weights
        .iter()
        .filter(|&&weight| weight > 0)
        .try_fold(0_i128, |sum, &weight| sum.checked_add(weight))
        .ok_or(Error::Overflow)?;
    if positive == 0 {
        return Ok(0);
    }

    let infinite = weights
        .iter()
        .try_fold(1_i128, |sum, &weight| sum.checked_add(weight.abs()))
        .ok_or(Error::Overflow)?;
    let mut network = Network::new(weights.len() + 2)?;
    let (source, sink) = (weights.len(), weights.len() + 1);
    for (node, &weight) in weights.iter().enumerate() {
        if weight > 0 {
            network.set(source, node, weight);
        } else if weight < 0 {
            network.set(node, sink, -weight);
        }

        for other in (0..weights.len()).filter(|&other| other != node && arc(node, other)) {
            network.set(node, other, infinite);
        }
    }

    Ok(positive - network.max_flow(source, sink)?)
}

/// A dense residual network.
struct Network {
    /// The number of nodes.
    nodes: usize,
    /// `residual[x * nodes + y]`: the remaining capacity from `x` to `y`.
    residual: Vec<i128>,
}

impl Network {
    /// A network on `nodes` nodes without capacity.
    fn new(nodes: usize) -> Result<Self, Error> {
        let mut residual = Vec::new();
        residual
            .try_reserve_exact(nodes * nodes)
            .map_err(|_| Error::OutOfMemory)?;
        residual.resize(nodes * nodes, 0);
        Ok(Self { nodes, residual })
    }

    /// Sets the capacity from `x` to `y`.
    fn set(&mut self, x: usize, y: usize, capacity: i128) {
        self.residual[x * self.nodes + y] = capacity;
    }

    /// The value of a maximum flow from `source` to `sink`, by shortest augmenting paths.
    fn max_flow(&mut self, source: usize, sink: usize) -> Result<i128, Error> {
        let mut flow: i128 = 0;
        while let Some(parents) = self.augmenting_path(source, sink)? {
            let mut bottleneck = i128::MAX;
            let mut node = sink;
            while node != source {
                let parent = parents[node];
                bottleneck = bottleneck.min(self.residual[parent * self.nodes + node]);
                node = parent;
            }

            let mut node = sink;
            while node != source {
                let parent = parents[node];
                self.residual[parent * self.nodes + node] -= bottleneck;
                self.residual[node * self.nodes + parent] += bottleneck;
                node = parent;
            }

            flow = flow.checked_add(bottleneck).ok_or(Error::Overflow)?;
        }

        Ok(flow)
    }

    /// The parent of every node on a shortest residual path from `source`, if `sink` is
    /// reachable.
    fn augmenting_path(&self, source: usize, sink: usize) -> Result<Option<Vec<usize>>, Error> {
        let mut parents = Vec::new();
        parents
            .try_reserve_exact(self.nodes)
            .map_err(|_| Error::OutOfMemory)?;
        parents.resize(self.nodes, usize::MAX);
        parents[source] = source;
        let mut queue = VecDeque::new();
        queue
            .try_reserve(self.nodes)
            .map_err(|_| Error::OutOfMemory)?;
        queue.push_back(source);
        while let Some(node) = queue.pop_front() {
            for next in 0..self.nodes {
                if parents[next] == usize::MAX && self.residual[node * self.nodes + next] > 0 {
                    parents[next] = node;
                    if next == sink {
                        return Ok(Some(parents));
                    }

                    queue.push_back(next);
                }
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    //! The closure value against enumeration of all node subsets.

    use super::max_weight_closure;

    /// Every small graph: the min-cut value equals the best closed subset.
    #[test]
    fn matches_enumeration() {
        type Graph<'a> = (&'a [i128], &'a [(usize, usize)]);
        let graphs: [Graph<'_>; 4] = [
            (&[3, -2, 4, -5], &[(0, 1), (2, 3)]),
            (&[5, -1, -1, 2], &[(0, 1), (0, 2), (3, 2)]),
            (&[-4, 6, -3, 1, 2], &[(1, 0), (1, 2), (3, 4), (4, 3)]),
            (&[-1, -2], &[]),
        ];
        for (weights, arcs) in graphs {
            let arc = |x: usize, y: usize| arcs.contains(&(x, y));
            let best = (0_u32..1 << weights.len())
                .filter(|set| {
                    arcs.iter()
                        .all(|&(x, y)| set & (1 << x) == 0 || set & (1 << y) != 0)
                })
                .map(|set| {
                    (0..weights.len())
                        .filter(|&node| set & (1 << node) != 0)
                        .map(|node| weights[node])
                        .sum::<i128>()
                })
                .max()
                .expect("the empty set is closed");
            assert_eq!(max_weight_closure(weights, arc), Ok(best));
        }
    }
}
