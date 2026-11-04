//! Other applications' formats.
//!
//! - **JSON Canvas 1.0** (jsoncanvas.org, MIT): the open format of Obsidian Canvas. Written, and
//!   read back as notes and connections.
//! - **GraphML** (graphml.graphdrawing.org): for yEd, Gephi and other graph tools. Written only.
//!
//! Pictures are not embedded in either: JSON Canvas would need a file for each, and GraphML has
//! no picture type. A picture node is written as its description.

use serde_json::{Value, json};

use crate::{Author, Content, Graph, GraphError, KindId, Result, Strength};

/// A node's size in JSON Canvas (it has no automatic size).
const CANVAS_NODE: [f32; 2] = [280.0, 140.0];
/// Largest JSON Canvas file read.
pub const MAX_CANVAS_BYTES: usize = 64 << 20;

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn node_text(graph: &Graph, n: &crate::Node) -> String {
    let mut text = match &n.content {
        Content::Text { text } => format!("> {}", text.replace('\n', "\n> ")),
        Content::Image { alt, .. } => format!("[picture] {alt}"),
        Content::Note { text } => text.clone(),
        Content::Concept { label } => format!("## {label}"),
    };
    if let Some(a) = &n.anchor {
        text.push_str(&format!("\n\n— {}, p. {}", a.title, u64::from(a.page) + 1));
    }
    let _ = graph;
    text
}

/// The map as a JSON Canvas document.
pub fn to_json_canvas(graph: &Graph) -> Value {
    let nodes: Vec<Value> = graph
        .nodes
        .iter()
        .map(|n| {
            json!({
                "id": format!("n{}", n.id.0),
                "type": "text",
                "text": node_text(graph, n),
                "x": n.position[0].round() as i64,
                "y": n.position[1].round() as i64,
                "width": CANVAS_NODE[0] as i64,
                "height": CANVAS_NODE[1] as i64,
            })
        })
        .collect();
    let edges: Vec<Value> = graph
        .edges
        .iter()
        .map(|e| {
            let kind = graph.kind(e.kind);
            let name = kind.map_or("is related to", |k| k.name.as_str());
            let label = match &e.label {
                Some(l) => format!("{name} ({}): {l}", e.strength.name()),
                None => format!("{name} ({})", e.strength.name()),
            };
            json!({
                "id": format!("e{}", e.id.0),
                "fromNode": format!("n{}", e.from.0),
                "toNode": format!("n{}", e.to.0),
                "toEnd": if kind.is_some_and(|k| k.directed) { "arrow" } else { "none" },
                "color": kind.map(|k| hex(k.colour)),
                "label": label,
            })
        })
        .collect();
    json!({ "nodes": nodes, "edges": edges })
}

/// Read a JSON Canvas document: text nodes become notes, file and link nodes concepts named by
/// them, groups are left out; connections take the kind their label names, else "is related
/// to", and medium strength.
pub fn from_json_canvas(bytes: &[u8], title: &str) -> Result<(Graph, Vec<String>)> {
    if bytes.len() > MAX_CANVAS_BYTES {
        return Err(GraphError::TooLong("the canvas"));
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|e| GraphError::Unreadable(e.to_string()))?;
    let mut graph = Graph::new(title);
    let mut ids = std::collections::HashMap::new();
    let mut skipped = 0usize;
    for n in v.get("nodes").and_then(Value::as_array).into_iter().flatten() {
        let id = n.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let x = n.get("x").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        let y = n.get("y").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        let content = match n.get("type").and_then(Value::as_str) {
            Some("text") => Content::Note { text: crate::capped(n.get("text").and_then(Value::as_str).unwrap_or_default(), crate::MAX_TEXT_BYTES) },
            Some("file") => {
                Content::Concept { label: crate::capped(n.get("file").and_then(Value::as_str).unwrap_or_default(), crate::MAX_LABEL_BYTES) }
            }
            Some("link") => {
                Content::Concept { label: crate::capped(n.get("url").and_then(Value::as_str).unwrap_or_default(), crate::MAX_LABEL_BYTES) }
            }
            _ => {
                skipped += 1;
                continue;
            }
        };
        match graph.add_node(content, None, [x, y], Author::User) {
            Ok(node) => {
                ids.insert(id, node);
            }
            Err(_) => skipped += 1,
        }
    }
    let related = graph.kind_named("is related to").map(|k| k.id).unwrap_or(KindId(1));
    for e in v.get("edges").and_then(Value::as_array).into_iter().flatten() {
        let end = |key: &str| e.get(key).and_then(Value::as_str).and_then(|id| ids.get(id)).copied();
        let (Some(from), Some(to)) = (end("fromNode"), end("toNode")) else {
            skipped += 1;
            continue;
        };
        let label = e.get("label").and_then(Value::as_str).unwrap_or_default();
        let kind = graph.kind_named(label.split(" (").next().unwrap_or_default()).map_or(related, |k| k.id);
        let note = (graph.kind(kind).is_some_and(|k| !label.starts_with(&k.name)) && !label.is_empty())
            .then(|| crate::capped(label, crate::MAX_LABEL_BYTES));
        if graph.add_edge(from, to, kind, Strength::Medium, note, Author::User).is_err() {
            skipped += 1;
        }
    }
    let warnings = if skipped > 0 { vec![format!("{skipped} canvas parts couldn't be read and were left out")] } else { Vec::new() };
    Ok((graph, warnings))
}

fn xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Characters XML 1.0 can't carry are left out.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// The map as GraphML: node text, type and source; edge kind, strength and label.
pub fn to_graphml(graph: &Graph) -> String {
    let mut out = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\">\n",
        "  <key id=\"text\" for=\"node\" attr.name=\"text\" attr.type=\"string\"/>\n",
        "  <key id=\"type\" for=\"node\" attr.name=\"type\" attr.type=\"string\"/>\n",
        "  <key id=\"source\" for=\"node\" attr.name=\"source\" attr.type=\"string\"/>\n",
        "  <key id=\"page\" for=\"node\" attr.name=\"page\" attr.type=\"int\"/>\n",
        "  <key id=\"x\" for=\"node\" attr.name=\"x\" attr.type=\"double\"/>\n",
        "  <key id=\"y\" for=\"node\" attr.name=\"y\" attr.type=\"double\"/>\n",
        "  <key id=\"kind\" for=\"edge\" attr.name=\"kind\" attr.type=\"string\"/>\n",
        "  <key id=\"strength\" for=\"edge\" attr.name=\"strength\" attr.type=\"string\"/>\n",
        "  <key id=\"label\" for=\"edge\" attr.name=\"label\" attr.type=\"string\"/>\n",
    ));
    out.push_str(&format!("  <graph id=\"{}\" edgedefault=\"undirected\">\n", xml(&graph.title)));
    for n in &graph.nodes {
        let kind = match n.content {
            Content::Text { .. } => "text",
            Content::Image { .. } => "picture",
            Content::Note { .. } => "note",
            Content::Concept { .. } => "concept",
        };
        out.push_str(&format!("    <node id=\"n{}\">\n", n.id.0));
        out.push_str(&format!("      <data key=\"text\">{}</data>\n      <data key=\"type\">{kind}</data>\n", xml(n.content.text())));
        if let Some(a) = &n.anchor {
            out.push_str(&format!(
                "      <data key=\"source\">{}</data>\n      <data key=\"page\">{}</data>\n",
                xml(&a.title),
                u64::from(a.page) + 1
            ));
        }
        out.push_str(&format!("      <data key=\"x\">{}</data>\n      <data key=\"y\">{}</data>\n    </node>\n", n.position[0], n.position[1]));
    }
    for e in &graph.edges {
        let kind = graph.kind(e.kind);
        let directed = kind.is_some_and(|k| k.directed);
        out.push_str(&format!("    <edge id=\"e{}\" source=\"n{}\" target=\"n{}\" directed=\"{directed}\">\n", e.id.0, e.from.0, e.to.0));
        out.push_str(&format!("      <data key=\"kind\">{}</data>\n", xml(kind.map_or("", |k| k.name.as_str()))));
        out.push_str(&format!("      <data key=\"strength\">{}</data>\n", e.strength.name()));
        if let Some(l) = &e.label {
            out.push_str(&format!("      <data key=\"label\">{}</data>\n", xml(l)));
        }
        out.push_str("    </edge>\n");
    }
    out.push_str("  </graph>\n</graphml>\n");
    out
}
