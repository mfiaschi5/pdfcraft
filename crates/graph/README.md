# pdfcraft-graph

Concept maps for study and research (M13, the ADR "Concept graph"): snippets of text and pictures
taken from PDFs, notes and concepts, linked by kinds of connection of three strengths. Each
snippet keeps its source (the document's SHA-256, its last paths, title, page, region and quote),
so the app can open the document at that page again, even after the file has moved.

- **Layer:** L4, standalone: it depends on no other PdfCraft crate. `engine::map_assist` asks the
  AI for changes, `automation` has the `graph_*` tools, `ui-egui` the map workspace.
- **No I/O:** maps are read and written as bytes ([`Graph::to_json`], [`Graph::from_json`]); the
  callers own the files. The crate builds for `wasm32-unknown-unknown`.
- **Licence:** MIT OR Apache-2.0. Written from the JSON Canvas 1.0 specification (jsoncanvas.org,
  MIT), the GraphML primer (graphml.graphdrawing.org) and Fruchterman and Reingold, "Graph Drawing
  by Force-directed Placement" (1991). No other implementation was read.

## API

```rust
// The map (lib.rs)
Graph::new(title)
Graph::{nodes, edges, kinds, node, edge, kind, kind_named}
Graph::add_node(Content, Option<Anchor>, [x, y], Author) -> Result<NodeId>
Graph::add_edge(from, to, KindId, Strength, label, Author) -> Result<EdgeId>
Graph::{remove_node, remove_edge, move_node, set_content, set_tags, set_edge}
Graph::{add_kind, rename_kind, next_kind_colour}   // the user's own kinds of connection
Graph::neighbourhood(id, depth) -> HashSet<NodeId> // what an AI request about one node sends
Graph::search(query) -> Vec<NodeId>
Graph::to_json() / Graph::from_json(bytes) -> (Graph, warnings)
Content::{Text, Image, Note, Concept}   Anchor   Strength::{Weak, Medium, Strong}   Line
History::{record, undo, redo, undo_label, redo_label}   // whole-map steps, MAX_UNDO deep

// Elsewhere
layout::tidy(&mut Graph, only, steps)             // force-directed, bounded work
export::{to_json_canvas, from_json_canvas, to_graphml}
proposal::{Proposal, Change, NodeRef, Applied}     // the AI's changes, applied only as kept
```

## Rules

- **Untrusted input.** A map file, a JSON Canvas and an AI proposal are all untrusted. Reading
  keeps what is sound and says what was left out (a dangling connection, a node past
  [`MAX_NODES`], a picture that isn't PNG or JPEG); nothing panics. Text, labels, pictures, nodes,
  connections, kinds, tags, coordinates and undo steps are capped (`MAX_*`).
- **Connections.** No connection from a node to itself; a kind without a direction connects a pair
  once, a directed kind once each way. Removing a node removes its connections.
- **The document's words.** A proposal can reword notes and concepts, never a snippet.
- **Ids** are not reused by later changes, so a proposal's ids name the nodes it was made for. An
  undo brings back the earlier counters too, so the app drops a proposal made before an undo.

## Tests

`cargo test -p pdfcraft-graph`: unit tests for the checks, undo, saved maps (round trip and
damaged files), layout, both export formats and proposals, and property tests that arbitrary
bytes never panic the readers and random edits keep every connection's ends in the map.
