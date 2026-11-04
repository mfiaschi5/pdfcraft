//! Tidy up: a force-directed layout (Fruchterman and Reingold, "Graph Drawing by Force-directed
//! Placement", 1991). Connected nodes pull together, all nodes push apart, and the moves cool
//! down step by step. Deterministic, and bounded: the work never passes [`MAX_WORK`].

use std::collections::{HashMap, HashSet};

use crate::{Graph, MAX_COORD, NodeId, Strength};

/// Most pair visits one layout makes (nodes² × steps).
pub const MAX_WORK: u64 = 200_000_000;
/// The distance two connected nodes settle at.
const IDEAL: f64 = 220.0;

/// Move the nodes of `graph` (or only those in `only`; the others stay and still push and pull)
/// for up to `steps` steps. Returns how many steps ran.
pub fn tidy(graph: &mut Graph, only: Option<&HashSet<NodeId>>, steps: u32) -> u32 {
    let n = graph.nodes.len();
    if n < 2 {
        return 0;
    }
    let pairs = (n as u64).saturating_mul(n as u64).max(1);
    let steps = u64::from(steps).min(MAX_WORK / pairs) as u32;
    let index: HashMap<NodeId, usize> = graph.nodes.iter().enumerate().map(|(i, node)| (node.id, i)).collect();
    let mut pos: Vec<[f64; 2]> = graph.nodes.iter().map(|node| node.position.map(f64::from)).collect();
    let movable: Vec<bool> = graph.nodes.iter().map(|node| only.is_none_or(|o| o.contains(&node.id))).collect();
    // Nodes stacked on one point get a deterministic nudge, so the forces have a direction.
    for i in 0..n {
        for j in 0..i {
            if let (Some(a), Some(b)) = (pos.get(i).copied(), pos.get(j).copied())
                && (a[0] - b[0]).abs() < 1e-3
                && (a[1] - b[1]).abs() < 1e-3
                && movable.get(i).copied().unwrap_or(false)
                && let Some(p) = pos.get_mut(i)
            {
                let angle = i as f64 * 2.399_963; // the golden angle
                p[0] += angle.cos() * IDEAL * 0.5;
                p[1] += angle.sin() * IDEAL * 0.5;
            }
        }
    }
    let springs: Vec<(usize, usize, f64)> = graph
        .edges
        .iter()
        .filter_map(|e| {
            let weight = match e.strength {
                Strength::Weak => 0.5,
                Strength::Medium => 1.0,
                Strength::Strong => 1.6,
            };
            Some((*index.get(&e.from)?, *index.get(&e.to)?, weight))
        })
        .collect();
    let mut temperature = IDEAL;
    for step in 0..steps {
        let mut moves = vec![[0.0f64; 2]; n];
        for i in 0..n {
            for j in (i + 1)..n {
                let (Some(a), Some(b)) = (pos.get(i), pos.get(j)) else { continue };
                let d = [a[0] - b[0], a[1] - b[1]];
                let dist = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1.0);
                let push = IDEAL * IDEAL / dist;
                let f = [d[0] / dist * push, d[1] / dist * push];
                if let Some(m) = moves.get_mut(i) {
                    m[0] += f[0];
                    m[1] += f[1];
                }
                if let Some(m) = moves.get_mut(j) {
                    m[0] -= f[0];
                    m[1] -= f[1];
                }
            }
        }
        for &(i, j, weight) in &springs {
            let (Some(a), Some(b)) = (pos.get(i), pos.get(j)) else { continue };
            let d = [a[0] - b[0], a[1] - b[1]];
            let dist = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1.0);
            let pull = dist * dist / IDEAL * weight;
            let f = [d[0] / dist * pull, d[1] / dist * pull];
            if let Some(m) = moves.get_mut(i) {
                m[0] -= f[0];
                m[1] -= f[1];
            }
            if let Some(m) = moves.get_mut(j) {
                m[0] += f[0];
                m[1] += f[1];
            }
        }
        for i in 0..n {
            if !movable.get(i).copied().unwrap_or(false) {
                continue;
            }
            let (Some(m), Some(p)) = (moves.get(i).copied(), pos.get_mut(i)) else { continue };
            let len = (m[0] * m[0] + m[1] * m[1]).sqrt();
            if len > 1e-9 && len.is_finite() {
                let step_len = len.min(temperature);
                p[0] = (p[0] + m[0] / len * step_len).clamp(-f64::from(MAX_COORD), f64::from(MAX_COORD));
                p[1] = (p[1] + m[1] / len * step_len).clamp(-f64::from(MAX_COORD), f64::from(MAX_COORD));
            }
        }
        temperature = (IDEAL * (1.0 - f64::from(step + 1) / f64::from(steps.max(1)))).max(1.0);
    }
    for (node, p) in graph.nodes.iter_mut().zip(pos) {
        if p[0].is_finite() && p[1].is_finite() {
            node.position = [p[0] as f32, p[1] as f32];
        }
    }
    steps
}
