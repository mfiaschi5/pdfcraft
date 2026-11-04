//! The AI edits a concept map (ADR "Concept graph" §3.4). The model sees the map and the passages
//! it may quote, and answers by calling tools that *propose* changes: nodes, connections,
//! rewordings, removals. Nothing is applied here. The calls become a
//! [`Proposal`](pdfcraft_graph::proposal::Proposal) that the user previews and accepts change by
//! change.
//!
//! - **Grounded.** A snippet node must quote one of the passages sent, word for word, and name its
//!   source id. Its anchor (document, page, quote) comes from that passage, never from the model's
//!   prose. An id that wasn't sent is refused, and the model is told why so it can try again.
//! - **Bounded.** At most [`MAX_ROUNDS`] request rounds and [`MAX_CHANGES`] changes. The map text
//!   sent is capped.
//! - **No network.** The transport is the caller's (the app's, or the CLI's with
//!   `--allow-ai-network`).

use std::collections::HashSet;
use std::ops::ControlFlow;

use pdfcraft_ai::claude::{self, Message, Options, Passage, ToolCall, ToolDef};
use pdfcraft_ai::event::Event;
use pdfcraft_ai::model::Usage;
use pdfcraft_ai::source::SourceId;
use pdfcraft_ai::transport::{HttpRequest, Secret, Transport};
use pdfcraft_graph::proposal::{Change, NodeRef, Proposal};
use pdfcraft_graph::{Anchor, Content, EdgeId, Graph, NodeId, Strength};
use serde_json::{Value, json};

/// Request rounds one edit may take (each round answers the previous round's tool calls).
pub const MAX_ROUNDS: usize = 6;
/// Changes one edit may propose.
pub const MAX_CHANGES: usize = pdfcraft_graph::proposal::MAX_CHANGES;
/// The map's description sent to the model (bytes).
const MAX_MAP_TEXT: usize = 48 * 1024;
/// A node's text in that description (bytes).
const NODE_TEXT: usize = 240;

const INSTRUCTIONS: &str = "You are helping the user organise a concept map for study and research. The map's nodes and connections are listed below; ids are in brackets. Passages from the user's documents are given as search results, with source ids such as pdfcraft-doc:i3 or pdfcraft-lib:<hash>:i3.

Make changes only by calling the tools: propose_node, propose_connection, propose_rewording and propose_removal. A snippet node must quote a passage word for word and give that passage's source id; use concept nodes for ideas and note nodes for your explanations. Give every change a short reason. Nothing you propose is applied until the user accepts it. When you are done, reply with a short summary of what you proposed and why.";

/// The tools, with the map's own kinds of connection.
pub fn tools(graph: &Graph) -> Vec<ToolDef> {
    let kinds: Vec<&str> = graph.kinds().iter().map(|k| k.name.as_str()).collect();
    vec![
        ToolDef {
            name: "propose_node",
            description: "Propose a new node. type is concept (an idea), note (your explanation) or snippet (a passage quoted word for word; give its source id). key names the new node so connections can refer to it (for example n1). near is the id of an existing node to place it next to.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string", "description": "A short name for the new node, such as n1."},
                    "type": {"type": "string", "enum": ["concept", "note", "snippet"]},
                    "text": {"type": "string", "description": "The concept's name, the note, or the exact quote."},
                    "source": {"type": "string", "description": "For a snippet: the source id of the passage it quotes."},
                    "near": {"type": "integer", "description": "An existing node to place it next to."},
                    "reason": {"type": "string"}
                },
                "required": ["key", "type", "text", "reason"]
            }),
        },
        ToolDef {
            name: "propose_connection",
            description: "Propose a connection between two nodes: existing ones by their id (\"12\") or new ones by their key (\"n1\"). kind is one of the map's kinds; strength is how strongly they are connected.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                    "kind": {"type": "string", "enum": kinds},
                    "strength": {"type": "string", "enum": ["weak", "medium", "strong"]},
                    "label": {"type": "string"},
                    "reason": {"type": "string"}
                },
                "required": ["from", "to", "kind", "strength", "reason"]
            }),
        },
        ToolDef {
            name: "propose_rewording",
            description: "Propose new wording for an existing concept or note (snippets keep the document's words).",
            input_schema: json!({
                "type": "object",
                "properties": {"node": {"type": "integer"}, "text": {"type": "string"}, "reason": {"type": "string"}},
                "required": ["node", "text", "reason"]
            }),
        },
        ToolDef {
            name: "propose_removal",
            description: "Propose removing a connection, by its id (the number after e in the list).",
            input_schema: json!({
                "type": "object",
                "properties": {"connection": {"type": "integer"}, "reason": {"type": "string"}},
                "required": ["connection", "reason"]
            }),
        },
    ]
}

/// The map as the model reads it: one line per node and per connection, `scope` first (the
/// nodes the request is about), capped.
pub fn map_text(graph: &Graph, scope: Option<&HashSet<NodeId>>) -> String {
    let mut out = format!("Map: {}\n", graph.title);
    let line = |id: NodeId| -> Option<String> {
        let n = graph.node(id)?;
        let kind = match &n.content {
            Content::Text { .. } => "snippet",
            Content::Image { .. } => "picture",
            Content::Note { .. } => "note",
            Content::Concept { .. } => "concept",
        };
        let source = n.anchor.as_ref().map(|a| format!(" ({}, p. {})", a.title, u64::from(a.page) + 1)).unwrap_or_default();
        let text = pdfcraft_ai::text::prefix(n.content.text(), NODE_TEXT).replace('\n', " ");
        Some(format!("[{}] {kind}{source}: {text}\n", id.0))
    };
    let mut order: Vec<NodeId> = graph.nodes().iter().map(|n| n.id).collect();
    if let Some(scope) = scope {
        order.sort_by_key(|id| !scope.contains(id));
    }
    for id in order {
        let Some(l) = line(id) else { continue };
        if out.len() + l.len() > MAX_MAP_TEXT {
            out.push_str("(more nodes left out)\n");
            break;
        }
        out.push_str(&l);
    }
    for e in graph.edges() {
        let kind = graph.kind(e.kind).map_or("is related to", |k| k.name.as_str());
        let l = format!("[e{}] [{}] {kind} ({}) [{}]\n", e.id.0, e.from.0, e.strength.name(), e.to.0);
        if out.len() + l.len() > MAX_MAP_TEXT {
            out.push_str("(more connections left out)\n");
            break;
        }
        out.push_str(&l);
    }
    out
}

/// The first user turn: the passages, the instructions, the map, then the request.
pub fn first_turn(passages: &[Passage], graph: &Graph, scope: Option<&HashSet<NodeId>>, request: &str) -> Result<Message, String> {
    let mut turn = claude::user_turn(passages, None, request).map_err(|e| e.to_string())?;
    let at = turn.content.len().saturating_sub(1);
    turn.content.insert(at, json!({"type": "text", "text": format!("{INSTRUCTIONS}\n\n{}", map_text(graph, scope))}));
    Ok(turn)
}

fn words(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Turn the model's tool calls into proposed changes, and the answers it gets back:
/// (call id, result, is error). `anchor` makes a snippet's anchor from the passage it quotes.
pub fn take_calls(
    calls: &[ToolCall],
    graph: &Graph,
    passages: &[Passage],
    proposal: &mut Proposal,
    anchor: &dyn Fn(&Passage, &str) -> Option<Anchor>,
) -> Vec<(String, String, bool)> {
    let mut results = Vec::new();
    for call in calls {
        let outcome = take_call(call, graph, passages, proposal, anchor);
        let (text, error) = match outcome {
            Ok(text) => (text, false),
            Err(e) => (e, true),
        };
        results.push((call.id.clone(), text, error));
    }
    results
}

fn str_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| format!("{key} is missing"))
}

fn new_keys(proposal: &Proposal) -> HashSet<&str> {
    proposal
        .changes
        .iter()
        .filter_map(|p| match &p.change {
            Change::AddNode { key, .. } => Some(key.as_str()),
            _ => None,
        })
        .collect()
}

fn take_call(
    call: &ToolCall,
    graph: &Graph,
    passages: &[Passage],
    proposal: &mut Proposal,
    anchor: &dyn Fn(&Passage, &str) -> Option<Anchor>,
) -> Result<String, String> {
    if proposal.changes.len() >= MAX_CHANGES {
        return Err("no more changes can be proposed in one go".into());
    }
    let input = &call.input;
    let reason = input.get("reason").and_then(Value::as_str).unwrap_or_default();
    let change = match call.name.as_str() {
        "propose_node" => {
            let key = str_arg(input, "key")?;
            if key.len() > 32 || key.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
                return Err("key must be a short name that doesn't start with a digit, such as n1".into());
            }
            if new_keys(proposal).contains(key) {
                return Err(format!("the key {key} is taken; choose another"));
            }
            let text = str_arg(input, "text")?;
            if text.len() > pdfcraft_graph::MAX_TEXT_BYTES {
                return Err("the text is too long".into());
            }
            let near = match input.get("near").and_then(Value::as_u64) {
                Some(id) if graph.node(NodeId(id)).is_none() => return Err(format!("there is no node {id}")),
                Some(id) => Some(NodeId(id)),
                None => None,
            };
            let (content, node_anchor) = match str_arg(input, "type")? {
                "concept" => (Content::Concept { label: pdfcraft_ai::text::capped(text, pdfcraft_graph::MAX_LABEL_BYTES) }, None),
                "note" => (Content::Note { text: text.to_owned() }, None),
                "snippet" => {
                    let source = str_arg(input, "source")?;
                    let id = SourceId::parse(source).ok_or_else(|| format!("{source} isn't a source id"))?;
                    let passage = passages
                        .iter()
                        .filter(|p| p.id == id)
                        .find(|p| words(&p.text).contains(&words(text)))
                        .ok_or_else(|| format!("the quote isn't word for word in a passage with the source id {source}"))?;
                    let a = anchor(passage, text).ok_or_else(|| "that passage's document can't be cited".to_owned())?;
                    (Content::Text { text: text.to_owned() }, Some(a))
                }
                other => return Err(format!("type {other} isn't concept, note or snippet")),
            };
            Change::AddNode { key: key.to_owned(), content, anchor: node_anchor, near }
        }
        "propose_connection" => {
            let keys = new_keys(proposal);
            let end = |name: &str| -> Result<NodeRef, String> {
                let raw = str_arg(input, name)?;
                match raw.parse::<u64>() {
                    Ok(id) if graph.node(NodeId(id)).is_some() => Ok(NodeRef::Existing { id: NodeId(id) }),
                    Ok(id) => Err(format!("there is no node {id}")),
                    Err(_) if keys.contains(raw) => Ok(NodeRef::New { key: raw.to_owned() }),
                    Err(_) => Err(format!("{raw} is neither a node id nor the key of a node you proposed")),
                }
            };
            let (from, to) = (end("from")?, end("to")?);
            if from == to {
                return Err("a node can't be connected to itself".into());
            }
            let kind = str_arg(input, "kind")?;
            if graph.kind_named(kind).is_none() {
                return Err(format!("{kind} isn't one of the map's kinds of connection"));
            }
            let strength = Strength::parse(str_arg(input, "strength")?).ok_or("strength is weak, medium or strong")?;
            let label = input.get("label").and_then(Value::as_str).map(|l| pdfcraft_ai::text::capped(l.trim(), pdfcraft_graph::MAX_LABEL_BYTES));
            Change::AddEdge { from, to, kind: kind.to_owned(), strength, label }
        }
        "propose_rewording" => {
            let id = input.get("node").and_then(Value::as_u64).ok_or("node is missing")?;
            match graph.node(NodeId(id)).map(|n| &n.content) {
                Some(Content::Note { .. } | Content::Concept { .. }) => {}
                Some(_) => return Err("only concepts and notes can be reworded; snippets keep the document's words".into()),
                None => return Err(format!("there is no node {id}")),
            }
            Change::SetText { id: NodeId(id), text: str_arg(input, "text")?.to_owned() }
        }
        "propose_removal" => {
            let id = input.get("connection").and_then(Value::as_u64).ok_or("connection is missing")?;
            if graph.edge(EdgeId(id)).is_none() {
                return Err(format!("there is no connection e{id}"));
            }
            Change::RemoveEdge { id: EdgeId(id) }
        }
        other => return Err(format!("there is no tool {other}")),
    };
    proposal.push(change, reason);
    Ok(format!("proposed as change {}", proposal.changes.len()))
}

/// What one edit came back with.
#[derive(Debug, Default)]
pub struct MapEdit {
    pub proposal: Proposal,
    /// The model's closing words.
    pub summary: String,
    pub usage: Usage,
    pub errors: Vec<String>,
}

/// Everything [`propose`] needs besides the transport.
pub struct EditRequest<'a> {
    pub options: Options<'a>,
    pub key: &'a Secret,
    pub graph: &'a Graph,
    pub scope: Option<&'a HashSet<NodeId>>,
    pub passages: Vec<Passage>,
    pub request: &'a str,
    pub anchor: &'a dyn Fn(&Passage, &str) -> Option<Anchor>,
}

/// The first request of an edit, as a dry run shows it.
pub fn first_request(req: &EditRequest<'_>, tools: &[ToolDef]) -> Result<HttpRequest, String> {
    let turn = first_turn(&req.passages, req.graph, req.scope, req.request)?;
    let options = Options { tools, ..req.options.clone() };
    claude::request(&options, req.key, &[], &turn).map_err(|e| e.to_string())
}

/// Ask the model for changes, answering its tool calls until it is done (or [`MAX_ROUNDS`]).
/// `on_event` sees every event (the answer streaming in); `Break` cancels.
pub fn propose(transport: &dyn Transport, req: EditRequest<'_>, on_event: &mut dyn FnMut(&Event) -> ControlFlow<()>) -> Result<MapEdit, String> {
    let tools = tools(req.graph);
    let options = Options { tools: &tools, ..req.options.clone() };
    let mut out = MapEdit::default();
    let mut history: Vec<Message> = Vec::new();
    let mut turn = first_turn(&req.passages, req.graph, req.scope, req.request)?;
    for _ in 0..MAX_ROUNDS {
        let request = claude::request(&options, req.key, &history, &turn).map_err(|e| e.to_string())?;
        let mut all = history.clone();
        all.push(turn.clone());
        let mut text = String::new();
        let reply = claude::send(transport, &request, claude::sent_sources(&all), &mut |e| {
            match e {
                Event::Text { text: t, .. } => text.push_str(t),
                Event::ProviderError { kind, message } => out.errors.push(format!("{kind}: {message}")),
                _ => {}
            }
            on_event(e)
        })
        .map_err(|e| e.to_string())?;
        out.usage = out.usage.plus(&reply.usage);
        if !text.trim().is_empty() {
            out.summary = text;
        }
        if reply.status != 200 {
            break;
        }
        let calls = claude::tool_calls(&reply.message);
        history.push(turn);
        history.push(reply.message);
        if calls.is_empty() {
            break;
        }
        let results = take_calls(&calls, req.graph, &req.passages, &mut out.proposal, req.anchor);
        turn = claude::tool_results(&results);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
