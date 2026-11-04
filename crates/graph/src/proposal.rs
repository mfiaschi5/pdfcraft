//! The AI's changes to a map, as a proposal the user previews and accepts one change at a time.
//! Nothing is applied until [`Proposal::apply`] is called with the changes the user kept, and
//! then they are one undo step.
//!
//! New nodes are named by a key the proposal chooses (`"n1"`), so its connections can refer to
//! them before they exist. A connection to a new node the user didn't keep is skipped and said so.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{Anchor, Author, Content, EdgeId, Graph, GraphError, NodeId, Strength};

/// Changes one proposal may hold.
pub const MAX_CHANGES: usize = 200;

/// A node a change refers to: one already in the map, or one the proposal adds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeRef {
    Existing { id: NodeId },
    New { key: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    AddNode { key: String, content: Content, anchor: Option<Anchor>, near: Option<NodeId> },
    AddEdge { from: NodeRef, to: NodeRef, kind: String, strength: Strength, label: Option<String> },
    SetText { id: NodeId, text: String },
    RemoveEdge { id: EdgeId },
}

/// One proposed change and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Proposed {
    pub change: Change,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub changes: Vec<Proposed>,
}

/// What [`Proposal::apply`] did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Applied {
    pub nodes: Vec<NodeId>,
    pub edges: Vec<EdgeId>,
    /// Changes kept but not applied: (index, why).
    pub skipped: Vec<(usize, String)>,
}

impl Proposal {
    /// Add a change, up to [`MAX_CHANGES`]. Returns whether it was kept.
    pub fn push(&mut self, change: Change, reason: &str) -> bool {
        if self.changes.len() >= MAX_CHANGES {
            return false;
        }
        self.changes.push(Proposed { change, reason: crate::capped(reason, crate::MAX_TEXT_BYTES) });
        true
    }

    /// Apply the changes at `accepted` (indices into [`Self::changes`]) to `graph`: new nodes
    /// first, placed near the node they name or in a ring around the map's centre, then the rest
    /// in order. A change that can't apply is skipped and reported; the others still apply.
    pub fn apply(&self, graph: &mut Graph, accepted: &[usize]) -> Applied {
        let mut out = Applied::default();
        let kept = |i: usize| accepted.contains(&i);
        let mut keys: HashMap<&str, NodeId> = HashMap::new();
        let centre = centre(graph);
        let mut ring = 0usize;
        for (i, p) in self.changes.iter().enumerate().filter(|(i, _)| kept(*i)) {
            let Change::AddNode { key, content, anchor, near } = &p.change else { continue };
            let base = near.and_then(|id| graph.node(id)).map_or(centre, |n| n.position);
            let angle = ring as f32 * 2.399_963;
            let radius = if near.is_some() { 260.0 } else { 320.0 + 40.0 * (ring / 8) as f32 };
            ring += 1;
            let at = [base[0] + angle.cos() * radius, base[1] + angle.sin() * radius];
            match graph.add_node(content.clone(), anchor.clone(), at, Author::Assistant) {
                Ok(id) => {
                    keys.insert(key.as_str(), id);
                    out.nodes.push(id);
                }
                Err(e) => out.skipped.push((i, e.to_string())),
            }
        }
        for (i, p) in self.changes.iter().enumerate().filter(|(i, _)| kept(*i)) {
            let result = match &p.change {
                Change::AddNode { .. } => continue,
                Change::AddEdge { from, to, kind, strength, label } => {
                    let resolve = |r: &NodeRef| match r {
                        NodeRef::Existing { id } => graph.node(*id).map(|n| n.id).ok_or(GraphError::NoNode(id.0)),
                        NodeRef::New { key } => {
                            keys.get(key.as_str()).copied().ok_or(GraphError::Unreadable(format!("the new node {key} wasn't kept")))
                        }
                    };
                    let ends = resolve(from).and_then(|f| resolve(to).map(|t| (f, t)));
                    let kind = graph.kind_named(kind).map(|k| k.id).ok_or(GraphError::Unreadable(format!("no kind of connection named {kind}")));
                    match (ends, kind) {
                        (Ok((f, t)), Ok(k)) => graph.add_edge(f, t, k, *strength, label.clone(), Author::Assistant).map(|id| out.edges.push(id)),
                        (Err(e), _) | (_, Err(e)) => Err(e),
                    }
                }
                Change::SetText { id, text } => match graph.node(*id).map(|n| n.content.clone()) {
                    Some(Content::Note { .. }) => graph.set_content(*id, Content::Note { text: text.clone() }),
                    Some(Content::Concept { .. }) => graph.set_content(*id, Content::Concept { label: text.clone() }),
                    // A snippet's text is the document's: the assistant doesn't rewrite it.
                    Some(_) => Err(GraphError::Unreadable("only notes and concepts can be reworded".into())),
                    None => Err(GraphError::NoNode(id.0)),
                },
                Change::RemoveEdge { id } => graph.remove_edge(*id),
            };
            if let Err(e) = result {
                out.skipped.push((i, e.to_string()));
            }
        }
        out
    }
}

fn centre(graph: &Graph) -> [f32; 2] {
    let n = graph.nodes().len();
    if n == 0 {
        return [0.0, 0.0];
    }
    let sum = graph.nodes().iter().fold([0.0f64; 2], |s, node| [s[0] + f64::from(node.position[0]), s[1] + f64::from(node.position[1])]);
    [(sum[0] / n as f64) as f32, (sum[1] / n as f64) as f32]
}
