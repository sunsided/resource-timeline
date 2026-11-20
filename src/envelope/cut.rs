//! Exact maximum of a separable function over integer difference constraints, by minimum cut.
//!
//! Given integer variables `x_p` with domains `[lo_p, hi_p]`, arbitrary values `f_p(x_p)` and
//! constraints `x_q - x_p <= c_pq`, the maximum of `sum f_p(x_p)` is a minimum cut in a layered
//! graph (Ishikawa, 2003): one chain of nodes per variable, node `(p, k)` on the source side
//! meaning `x_p >= lo_p + k`; the chain edge leaving the last source-side node of a chain
//! carries the cost `M_p - f_p(x_p)`; infinite reverse edges keep each chain a prefix; and an
//! infinite edge from `(q, k)` to `(p, k - c_pq)` encodes "`x_q >= k` forces `x_p >= k - c`".
//! Difference constraints are convex pairwise terms, so the construction is exact and needs no
//! LP. The graph has one node per domain value, so the cost grows with the window lengths.

use alloc::vec::Vec;

use crate::Error;

/// One variable: its domain and its value at every domain point.
#[derive(Debug, Clone)]
pub(crate) struct Variable {
    /// The smallest domain value.
    pub(crate) lo: i128,
    /// `values[k]` is the value at `lo + k`. Never empty.
    pub(crate) values: Vec<i128>,
}

impl Variable {
    /// The largest domain value.
    fn hi(&self) -> i128 {
        self.lo + i128::try_from(self.values.len()).unwrap_or(i128::MAX) - 1
    }
}

/// The number of graph nodes a problem over `variables` needs.
pub(crate) fn nodes_for(domains: impl Iterator<Item = usize>) -> usize {
    domains.fold(2, |sum, size| sum.saturating_add(size.saturating_sub(1)))
}

/// The maximum of `sum f_p(x_p)` over the domains subject to `x_q - x_p <= bound(p, q)` for
/// every pair with a finite bound.
///
/// # Errors
///
/// - [`Error::InconsistentPrecedences`] if no assignment satisfies the constraints.
/// - [`Error::Overflow`] if a capacity sum exceeds `i128`.
/// - [`Error::OutOfMemory`] if the graph cannot be allocated.
pub(crate) fn max_separable(
    variables: &[Variable],
    bound: impl Fn(usize, usize) -> Option<i128>,
) -> Result<i128, Error> {
    let mut graph = Graph::default();
    let (source, sink) = (graph.node()?, graph.node()?);
    let mut first = Vec::new();
    first
        .try_reserve_exact(variables.len())
        .map_err(|_| Error::OutOfMemory)?;
    let mut ceiling: i128 = 0;
    let mut finite: i128 = 1;
    for variable in variables {
        let top = variable.values.iter().copied().max().unwrap_or(0);
        ceiling = ceiling.checked_add(top).ok_or(Error::Overflow)?;
        for &value in &variable.values {
            finite = finite.checked_add(top - value).ok_or(Error::Overflow)?;
        }

        first.push(graph.nodes);
        for _ in 1..variable.values.len() {
            graph.node()?;
        }
    }

    let infinite = finite;
    let node = |p: usize, k: usize| first[p] + k - 1;
    for (p, variable) in variables.iter().enumerate() {
        let top = variable.values.iter().copied().max().unwrap_or(0);
        let count = variable.values.len();
        for (k, &value) in variable.values.iter().enumerate() {
            let from = if k == 0 { source } else { node(p, k) };
            let to = if k + 1 == count { sink } else { node(p, k + 1) };
            graph.edge(from, to, top - value)?;
            if k > 0 && k + 1 < count {
                graph.edge(to, from, infinite)?;
            }
        }
    }

    for (q, later) in variables.iter().enumerate() {
        for (p, earlier) in variables.iter().enumerate() {
            let Some(limit) = bound(p, q).filter(|_| p != q) else {
                continue;
            };

            for k in 0..later.values.len() {
                // x_q >= later.lo + k forces x_p >= later.lo + k - limit.
                let needed = later.lo + i128::try_from(k).map_err(|_| Error::Overflow)? - limit;
                if needed <= earlier.lo {
                    continue;
                }

                let target = if needed > earlier.hi() {
                    sink
                } else {
                    let step = usize::try_from(needed - earlier.lo).map_err(|_| Error::Overflow)?;
                    node(p, step)
                };
                let from = if k == 0 { source } else { node(q, k) };
                graph.edge(from, target, infinite)?;
            }
        }
    }

    let cut = graph.max_flow(source, sink)?;
    if cut >= infinite {
        return Err(Error::InconsistentPrecedences);
    }

    Ok(ceiling - cut)
}

/// A flow network with adjacency lists, for Dinic's algorithm.
#[derive(Debug, Default)]
struct Graph {
    /// The number of nodes.
    nodes: usize,
    /// Edges as `(to, residual capacity)`; edge `2 i + 1` is the reverse of edge `2 i`.
    edges: Vec<(usize, i128)>,
    /// `adjacency[node]`: the indices of the edges leaving `node`.
    adjacency: Vec<Vec<usize>>,
}

impl Graph {
    /// Adds a node and returns its index.
    fn node(&mut self) -> Result<usize, Error> {
        self.adjacency
            .try_reserve(1)
            .map_err(|_| Error::OutOfMemory)?;
        self.adjacency.push(Vec::new());
        self.nodes += 1;
        Ok(self.nodes - 1)
    }

    /// Adds an edge of `capacity` from `from` to `to` with its zero reverse edge.
    fn edge(&mut self, from: usize, to: usize, capacity: i128) -> Result<(), Error> {
        self.edges.try_reserve(2).map_err(|_| Error::OutOfMemory)?;
        let index = self.edges.len();
        self.edges.push((to, capacity));
        self.edges.push((from, 0));
        for (node, edge) in [(from, index), (to, index + 1)] {
            self.adjacency[node]
                .try_reserve(1)
                .map_err(|_| Error::OutOfMemory)?;
            self.adjacency[node].push(edge);
        }

        Ok(())
    }

    /// The value of a maximum flow from `source` to `sink` (Dinic).
    fn max_flow(&mut self, source: usize, sink: usize) -> Result<i128, Error> {
        let mut flow: i128 = 0;
        let mut level = Vec::new();
        level
            .try_reserve_exact(self.nodes)
            .map_err(|_| Error::OutOfMemory)?;
        let mut next = Vec::new();
        next.try_reserve_exact(self.nodes)
            .map_err(|_| Error::OutOfMemory)?;
        while self.levels(source, sink, &mut level)? {
            next.clear();
            next.resize(self.nodes, 0);
            loop {
                let pushed = self.augment(source, sink, i128::MAX, &level, &mut next);
                if pushed == 0 {
                    break;
                }

                flow = flow.checked_add(pushed).ok_or(Error::Overflow)?;
            }
        }

        Ok(flow)
    }

    /// Breadth-first levels from `source`; whether `sink` is reachable.
    fn levels(&self, source: usize, sink: usize, level: &mut Vec<usize>) -> Result<bool, Error> {
        level.clear();
        level.resize(self.nodes, usize::MAX);
        level[source] = 0;
        let mut queue = alloc::collections::VecDeque::new();
        queue
            .try_reserve(self.nodes)
            .map_err(|_| Error::OutOfMemory)?;
        queue.push_back(source);
        while let Some(node) = queue.pop_front() {
            for &edge in &self.adjacency[node] {
                let (to, capacity) = self.edges[edge];
                if capacity > 0 && level[to] == usize::MAX {
                    level[to] = level[node] + 1;
                    queue.push_back(to);
                }
            }
        }

        Ok(level[sink] != usize::MAX)
    }

    /// Pushes up to `limit` along level-increasing paths from `node`; returns the amount.
    ///
    /// The recursion depth is bounded by the number of levels, at most the node count.
    fn augment(
        &mut self,
        node: usize,
        sink: usize,
        limit: i128,
        level: &[usize],
        next: &mut [usize],
    ) -> i128 {
        if node == sink {
            return limit;
        }

        while next[node] < self.adjacency[node].len() {
            let edge = self.adjacency[node][next[node]];
            let (to, capacity) = self.edges[edge];
            if capacity > 0 && level[to] == level[node] + 1 {
                let pushed = self.augment(to, sink, limit.min(capacity), level, next);
                if pushed > 0 {
                    self.edges[edge].1 -= pushed;
                    self.edges[edge ^ 1].1 += pushed;
                    return pushed;
                }
            }

            next[node] += 1;
        }

        0
    }
}

#[cfg(test)]
mod tests {
    //! The cut value against enumeration of every assignment.

    use alloc::vec;

    use super::{Variable, max_separable};

    /// Small problems: the cut agrees with brute force over all assignments.
    #[test]
    fn matches_enumeration() {
        let variables = [
            Variable {
                lo: 0,
                values: vec![3, -1, 4, 0],
            },
            Variable {
                lo: 2,
                values: vec![-2, 5, 1],
            },
            Variable {
                lo: -1,
                values: vec![0, 2, -3, 6, 1],
            },
        ];
        let bounds: [[Option<i128>; 3]; 3] = [
            [Some(0), Some(1), None],
            [Some(-1), Some(0), Some(0)],
            [Some(2), None, Some(0)],
        ];
        let closed = |p: usize, q: usize| bounds[p][q];

        let mut best = i128::MIN;
        for a in 0..4_i128 {
            for b in 2..5_i128 {
                for c in -1..4_i128 {
                    let x = [a, b, c];
                    let feasible = (0..3).all(|p| {
                        (0..3).all(|q| closed(p, q).is_none_or(|limit| x[q] - x[p] <= limit))
                    });
                    if feasible {
                        let value: i128 = variables
                            .iter()
                            .zip(x)
                            .map(|(v, x)| v.values[usize::try_from(x - v.lo).expect("in domain")])
                            .sum();
                        best = best.max(value);
                    }
                }
            }
        }

        assert!(best > i128::MIN, "the example is feasible");
        assert_eq!(max_separable(&variables, closed), Ok(best));
    }
}
