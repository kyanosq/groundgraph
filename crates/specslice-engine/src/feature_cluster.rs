//! Deterministic Louvain community detection.
//!
//! P24.2 — "business modules must come from the code graph, not the
//! directory layout". A target repo may be organised by layer
//! (`lib/models`, `lib/services`, `lib/widgets`) or be an outright mess;
//! its folders then say nothing about *business* boundaries. The call /
//! import graph, however, still clusters by feature — a feature's bloc →
//! usecase → repository → api-client call each other densely, while
//! cross-feature calls are sparse. Community detection on that graph
//! recovers business modules regardless of how the files are filed.
//!
//! This module is a small, dependency-free, **deterministic** Louvain
//! implementation over an abstract weighted undirected graph (node =
//! `usize` index). It is intentionally decoupled from the SpecSlice graph
//! types so it is exhaustively unit-testable on tiny synthetic graphs;
//! [`crate::business_pack`] is responsible for lifting the code graph onto
//! file-indices and naming the resulting communities.
//!
//! Determinism: nodes are visited in index order, ties in modularity gain
//! are broken towards the lowest community index (and towards *staying*),
//! and final labels are renumbered by smallest member index. Running it
//! twice on the same input yields byte-identical output — a hard
//! requirement for golden tests and reproducible reports.

use std::collections::HashMap;

/// Modularity gain below this is treated as "no improvement".
const EPSILON: f64 = 1e-9;
/// Safety cap on local-moving passes per level (Louvain converges fast;
/// this only guards against float churn).
const MAX_PASSES_PER_LEVEL: usize = 100;
/// Safety cap on aggregation levels.
const MAX_LEVELS: usize = 50;

/// Detect communities in an undirected weighted graph.
///
/// * `num_nodes` — nodes are `0..num_nodes`.
/// * `edges` — `(u, v, weight)`. May contain duplicates (accumulated),
///   either direction (treated as undirected), and self-loops (`u == v`).
///   Non-positive weights are ignored.
///
/// Returns a `Vec<usize>` of length `num_nodes`, mapping each node to a
/// community label in `0..k` (contiguous, ordered by smallest member).
/// With no usable edges every node is its own community.
pub fn detect_communities(num_nodes: usize, edges: &[(usize, usize, f64)]) -> Vec<usize> {
    detect_communities_with_resolution(num_nodes, edges, 1.0)
}

/// [`detect_communities`] with an explicit modularity **resolution** `γ`.
///
/// The local-moving gain becomes `w_ic - γ · Σ_tot[c] · k_i / 2m`; `γ = 1.0`
/// is standard modularity. `γ > 1.0` penalises large communities, yielding
/// more, smaller communities — the lever against modularity's *resolution
/// limit*, where a single large connected graph (e.g. a 1k-file app with no
/// feature folders) otherwise collapses into one community. Non-finite or
/// non-positive `γ` falls back to `1.0`.
pub fn detect_communities_with_resolution(
    num_nodes: usize,
    edges: &[(usize, usize, f64)],
    resolution: f64,
) -> Vec<usize> {
    if num_nodes == 0 {
        return Vec::new();
    }
    let resolution = if resolution.is_finite() && resolution > 0.0 {
        resolution
    } else {
        1.0
    };
    let mut graph = Graph::from_edges(num_nodes, edges, resolution);
    // node_community[original_node] = community at the current top level.
    let mut node_community: Vec<usize> = (0..num_nodes).collect();

    for _ in 0..MAX_LEVELS {
        let moved = graph.local_moving();
        // Map originals through this level's assignment.
        for c in node_community.iter_mut() {
            *c = graph.community[*c];
        }
        let num_comms = graph.renumber_communities(&mut node_community);
        if !moved || num_comms == graph.n {
            break;
        }
        graph = graph.aggregate(num_comms);
    }

    renumber_by_first_member(node_community)
}

struct Graph {
    n: usize,
    /// adjacency[i] = list of (neighbour, weight), self-loops excluded.
    adjacency: Vec<Vec<(usize, f64)>>,
    /// self_loop[i] = weight of edges from i to itself (counted once).
    self_loop: Vec<f64>,
    /// degree[i] = sum of incident edge weights, self-loop counted twice.
    degree: Vec<f64>,
    /// Total edge weight m (each undirected edge once, self-loops once).
    m: f64,
    /// community[i] = current community of node i (a node index).
    community: Vec<usize>,
    /// sigma_tot[c] = sum of degrees of nodes currently in community c.
    sigma_tot: Vec<f64>,
    /// Modularity resolution γ (1.0 = standard). Carried across aggregation
    /// levels so granularity stays consistent.
    resolution: f64,
}

impl Graph {
    fn from_edges(n: usize, edges: &[(usize, usize, f64)], resolution: f64) -> Graph {
        // Accumulate undirected weights into a per-node neighbour map so
        // duplicate / reversed edges combine deterministically.
        let mut neigh: Vec<HashMap<usize, f64>> = vec![HashMap::new(); n];
        let mut self_loop = vec![0.0f64; n];
        for &(u, v, w) in edges {
            if w <= 0.0 || u >= n || v >= n {
                continue;
            }
            if u == v {
                self_loop[u] += w;
            } else {
                *neigh[u].entry(v).or_insert(0.0) += w;
                *neigh[v].entry(u).or_insert(0.0) += w;
            }
        }
        let mut adjacency: Vec<Vec<(usize, f64)>> = Vec::with_capacity(n);
        let mut degree = vec![0.0f64; n];
        let mut m = 0.0f64;
        for i in 0..n {
            let mut list: Vec<(usize, f64)> = neigh[i].iter().map(|(&k, &w)| (k, w)).collect();
            list.sort_by_key(|&(k, _)| k);
            let mut deg = self_loop[i] * 2.0;
            for &(_, w) in &list {
                deg += w;
            }
            degree[i] = deg;
            adjacency.push(list);
            m += self_loop[i];
        }
        // Each undirected edge counted twice across the two endpoints' maps.
        let mut edge_sum = 0.0;
        for list in &adjacency {
            for &(_, w) in list {
                edge_sum += w;
            }
        }
        m += edge_sum / 2.0;

        let community: Vec<usize> = (0..n).collect();
        let sigma_tot = degree.clone();
        Graph {
            n,
            adjacency,
            self_loop,
            degree,
            m,
            community,
            sigma_tot,
            resolution,
        }
    }

    /// One Louvain level: repeatedly move nodes to the neighbouring
    /// community that maximises modularity gain. Returns whether any node
    /// moved at all.
    fn local_moving(&mut self) -> bool {
        if self.m <= 0.0 {
            return false;
        }
        let two_m = 2.0 * self.m;
        let mut any_moved = false;
        for _pass in 0..MAX_PASSES_PER_LEVEL {
            let mut moved_this_pass = false;
            for i in 0..self.n {
                let ci = self.community[i];
                let ki = self.degree[i];
                // weight from i into each neighbouring community
                let mut w_to_comm: HashMap<usize, f64> = HashMap::new();
                for &(j, w) in &self.adjacency[i] {
                    *w_to_comm.entry(self.community[j]).or_insert(0.0) += w;
                }
                // remove i from its own community
                self.sigma_tot[ci] -= ki;
                let w_i_old = w_to_comm.get(&ci).copied().unwrap_or(0.0);

                // baseline gain of staying in (now i-less) ci
                let g = self.resolution;
                let mut best_comm = ci;
                let mut best_gain = w_i_old - g * self.sigma_tot[ci] * ki / two_m;

                // consider neighbour communities in deterministic order
                let mut candidates: Vec<usize> = w_to_comm.keys().copied().collect();
                candidates.sort_unstable();
                for c in candidates {
                    if c == ci {
                        continue;
                    }
                    let w_ic = w_to_comm.get(&c).copied().unwrap_or(0.0);
                    let gain = w_ic - g * self.sigma_tot[c] * ki / two_m;
                    if gain > best_gain + EPSILON {
                        best_gain = gain;
                        best_comm = c;
                    }
                }
                // re-insert into chosen community
                self.sigma_tot[best_comm] += ki;
                if best_comm != ci {
                    self.community[i] = best_comm;
                    moved_this_pass = true;
                    any_moved = true;
                }
            }
            if !moved_this_pass {
                break;
            }
        }
        any_moved
    }

    /// Compress `community` labels to `0..k` and rewrite `self.community`
    /// in place. Also remaps the externally-held `node_community` mapping
    /// (originals → current community node) through the same table.
    /// Returns `k`.
    fn renumber_communities(&mut self, node_community: &mut [usize]) -> usize {
        let mut remap: HashMap<usize, usize> = HashMap::new();
        // Deterministic: assign new ids in ascending old-community order.
        let mut present: Vec<usize> = self.community.clone();
        present.sort_unstable();
        present.dedup();
        for old in present {
            let next = remap.len();
            remap.insert(old, next);
        }
        for c in self.community.iter_mut() {
            *c = remap[c];
        }
        for c in node_community.iter_mut() {
            // node_community currently holds "current community node"; map it.
            *c = remap[c];
        }
        remap.len()
    }

    /// Build the aggregated graph where each community becomes one node.
    fn aggregate(&self, num_comms: usize) -> Graph {
        let mut neigh: Vec<HashMap<usize, f64>> = vec![HashMap::new(); num_comms];
        let mut self_loop = vec![0.0f64; num_comms];
        // intra-community self loops carry over
        for i in 0..self.n {
            let ci = self.community[i];
            self_loop[ci] += self.self_loop[i];
        }
        // edges between current nodes → edges/self-loops between communities
        for i in 0..self.n {
            let ci = self.community[i];
            for &(j, w) in &self.adjacency[i] {
                if j < i {
                    continue; // each undirected edge once
                }
                let cj = self.community[j];
                if ci == cj {
                    self_loop[ci] += w;
                } else {
                    *neigh[ci].entry(cj).or_insert(0.0) += w;
                    *neigh[cj].entry(ci).or_insert(0.0) += w;
                }
            }
        }
        let mut adjacency: Vec<Vec<(usize, f64)>> = Vec::with_capacity(num_comms);
        let mut degree = vec![0.0f64; num_comms];
        let mut m = 0.0f64;
        for c in 0..num_comms {
            let mut list: Vec<(usize, f64)> = neigh[c].iter().map(|(&k, &w)| (k, w)).collect();
            list.sort_by_key(|&(k, _)| k);
            let mut deg = self_loop[c] * 2.0;
            for &(_, w) in &list {
                deg += w;
            }
            degree[c] = deg;
            adjacency.push(list);
            m += self_loop[c];
        }
        let mut edge_sum = 0.0;
        for list in &adjacency {
            for &(_, w) in list {
                edge_sum += w;
            }
        }
        m += edge_sum / 2.0;
        Graph {
            n: num_comms,
            adjacency,
            self_loop,
            m,
            community: (0..num_comms).collect(),
            sigma_tot: degree.clone(),
            degree,
            resolution: self.resolution,
        }
    }
}

/// Renumber arbitrary community labels so the label of each community is
/// the rank (by smallest member node index) — deterministic and stable.
fn renumber_by_first_member(labels: Vec<usize>) -> Vec<usize> {
    let mut first_seen: Vec<(usize, usize)> = Vec::new(); // (label, first_index)
    let mut seen: HashMap<usize, usize> = HashMap::new();
    for (idx, &lab) in labels.iter().enumerate() {
        seen.entry(lab).or_insert(idx);
    }
    for (&lab, &idx) in &seen {
        first_seen.push((lab, idx));
    }
    first_seen.sort_by_key(|&(_, idx)| idx);
    let mut remap: HashMap<usize, usize> = HashMap::new();
    for (new_id, &(lab, _)) in first_seen.iter().enumerate() {
        remap.insert(lab, new_id);
    }
    labels.into_iter().map(|l| remap[&l]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_graph_returns_empty() {
        assert!(detect_communities(0, &[]).is_empty());
    }

    #[test]
    fn no_edges_each_node_isolated() {
        let labels = detect_communities(4, &[]);
        // every node its own community
        assert_eq!(labels.len(), 4);
        let mut uniq = labels.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), 4);
    }

    #[test]
    fn two_triangles_bridged_split_into_two_communities() {
        // triangle A: 0-1-2 ; triangle B: 3-4-5 ; weak bridge 2-3
        let edges = vec![
            (0, 1, 1.0),
            (1, 2, 1.0),
            (0, 2, 1.0),
            (3, 4, 1.0),
            (4, 5, 1.0),
            (3, 5, 1.0),
            (2, 3, 1.0), // bridge
        ];
        let labels = detect_communities(6, &edges);
        assert_eq!(labels[0], labels[1]);
        assert_eq!(labels[1], labels[2]);
        assert_eq!(labels[3], labels[4]);
        assert_eq!(labels[4], labels[5]);
        assert_ne!(
            labels[0], labels[3],
            "the two triangles are separate communities"
        );
        // contiguous labels starting at 0
        assert_eq!(labels[0], 0);
        assert_eq!(labels[3], 1);
    }

    #[test]
    fn clique_is_single_community() {
        let mut edges = Vec::new();
        for i in 0..5 {
            for j in (i + 1)..5 {
                edges.push((i, j, 1.0));
            }
        }
        let labels = detect_communities(5, &edges);
        assert!(labels.iter().all(|&l| l == labels[0]));
        assert!(labels.iter().all(|&l| l == 0));
    }

    #[test]
    fn weighted_pull_overrides_sparse_directory_layout() {
        // Simulate a "messy" repo: node 0 (a model) is weakly linked to a
        // hub (5) but strongly coupled to its real feature {0,1,2}.
        let edges = vec![
            (0, 1, 5.0),
            (1, 2, 5.0),
            (0, 2, 5.0),
            (3, 4, 5.0),
            (4, 5, 5.0),
            (3, 5, 5.0),
            (0, 5, 1.0), // weak cross-feature link
            (2, 3, 1.0),
        ];
        let labels = detect_communities(6, &edges);
        assert_eq!(labels[0], labels[1]);
        assert_eq!(labels[1], labels[2]);
        assert_eq!(labels[3], labels[4]);
        assert_eq!(labels[4], labels[5]);
        assert_ne!(labels[0], labels[3]);
    }

    #[test]
    fn resolution_controls_granularity_against_the_limit() {
        // Canonical resolution-limit example: a ring of N triangles, adjacent
        // ones joined by a single bridge. Standard modularity (γ=1) merges
        // adjacent triangles into fewer, larger communities; a higher γ
        // penalises large communities and recovers the finer structure.
        const T: usize = 12;
        let n = T * 3;
        let mut edges = Vec::new();
        for t in 0..T {
            let (a, b, c) = (3 * t, 3 * t + 1, 3 * t + 2);
            edges.push((a, b, 1.0));
            edges.push((b, c, 1.0));
            edges.push((a, c, 1.0));
            // bridge to the next triangle in the ring
            let next = 3 * ((t + 1) % T);
            edges.push((c, next, 1.0));
        }
        let count = |labels: &[usize]| {
            let mut u = labels.to_vec();
            u.sort_unstable();
            u.dedup();
            u.len()
        };
        let low = count(&detect_communities_with_resolution(n, &edges, 1.0));
        let high = count(&detect_communities_with_resolution(n, &edges, 2.0));
        assert!(
            low < T,
            "γ=1 should merge adjacent triangles: {low} communities"
        );
        assert!(
            high > low,
            "a higher resolution must yield more communities: {high} vs {low}"
        );
        // Default keeps the resolution-1.0 behaviour for back-compat.
        assert_eq!(
            detect_communities(n, &edges),
            detect_communities_with_resolution(n, &edges, 1.0)
        );
    }

    #[test]
    fn output_is_deterministic() {
        let edges = vec![
            (0, 1, 1.0),
            (1, 2, 1.0),
            (0, 2, 1.0),
            (3, 4, 1.0),
            (4, 5, 1.0),
            (3, 5, 1.0),
            (2, 3, 1.0),
            (5, 6, 1.0),
            (6, 7, 1.0),
            (5, 7, 1.0),
        ];
        let a = detect_communities(8, &edges);
        let b = detect_communities(8, &edges);
        assert_eq!(a, b);
    }
}
