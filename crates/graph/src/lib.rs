//! Concept maps (ADR "Concept graph"): snippets of text and pictures taken from PDFs, notes and
//! concepts, linked by connections of several kinds and three strengths, to understand and
//! organise material for study and research.
//!
//! - [`Graph`] is the map: nodes, edges and the edge kinds, with every change checked (caps,
//!   no dangling edges, no duplicates). [`History`] gives undo and redo.
//! - [`Graph::to_json`] / [`Graph::from_json`] are the saved form: versioned, and read
//!   leniently, so a damaged node or edge is dropped with a warning and the rest loads.
//! - [`layout`] tidies positions with a force-directed layout.
//! - [`export`] writes JSON Canvas (Obsidian Canvas) and GraphML (yEd, Gephi), and reads JSON
//!   Canvas.
//! - [`proposal`] is how the AI's changes are previewed and accepted one by one.
//!
//! No I/O: the caller reads and writes files. Every input is untrusted.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod export;
pub mod layout;
pub mod proposal;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Nodes in one map.
pub const MAX_NODES: usize = 10_000;
/// Edges in one map.
pub const MAX_EDGES: usize = 50_000;
/// Edge kinds in one map.
pub const MAX_KINDS: usize = 64;
/// Text of a node (bytes).
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
/// A picture in a node (bytes of PNG or JPEG).
pub const MAX_IMAGE_BYTES: usize = 2 << 20;
/// All pictures in one map.
pub const MAX_IMAGES_BYTES: usize = 256 << 20;
/// Labels, titles, tag and kind names (bytes).
pub const MAX_LABEL_BYTES: usize = 256;
/// Tags on one node.
pub const MAX_TAGS: usize = 16;
/// Largest saved map read back.
pub const MAX_FILE_BYTES: usize = 384 << 20;
/// Positions are kept within this distance of the origin.
pub const MAX_COORD: f32 = 1.0e6;
/// Undo steps kept.
pub const MAX_UNDO: usize = 100;

const FORMAT: &str = "pdfcraft-map";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    #[error("the map is full ({0})")]
    Full(&'static str),
    #[error("{0} is too long")]
    TooLong(&'static str),
    #[error("no node {0}")]
    NoNode(u64),
    #[error("no connection {0}")]
    NoEdge(u64),
    #[error("no kind of connection {0}")]
    NoKind(u32),
    #[error("a node can't be connected to itself")]
    SelfLoop,
    #[error("these nodes are already connected this way")]
    Duplicate,
    #[error("the picture isn't a PNG or JPEG")]
    NotAnImage,
    #[error("{0} is empty")]
    Empty(&'static str),
    #[error("the saved map isn't one PdfCraft can read: {0}")]
    Unreadable(String),
    #[error("the saved map is version {0}; this PdfCraft reads version {FORMAT_VERSION}")]
    WrongVersion(u32),
}

pub type Result<T> = std::result::Result<T, GraphError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EdgeId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KindId(pub u32);

/// A picture's bytes, shared between undo snapshots, saved as base64.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageBytes(pub Arc<Vec<u8>>);

impl Serialize for ImageBytes {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use base64::Engine as _;
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(self.0.as_slice()))
    }
}

impl<'de> Deserialize<'de> for ImageBytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use base64::Engine as _;
        let s = String::deserialize(d)?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(s.as_bytes()).map_err(serde::de::Error::custom)?;
        Ok(Self(Arc::new(bytes)))
    }
}

/// Whether `bytes` start like a PNG or a JPEG.
pub fn is_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    /// A passage taken from a document.
    Text { text: String },
    /// A picture taken from a document (an image, or a region of a page).
    Image { image: ImageBytes, alt: String },
    /// The user's own note.
    Note { text: String },
    /// A named idea, without a source.
    Concept { label: String },
}

impl Content {
    /// The words a node shows, or describes its picture with.
    pub fn text(&self) -> &str {
        match self {
            Self::Text { text } | Self::Note { text } => text,
            Self::Image { alt, .. } => alt,
            Self::Concept { label } => label,
        }
    }

    fn check(&self) -> Result<()> {
        match self {
            Self::Text { text } | Self::Note { text } if text.len() > MAX_TEXT_BYTES => Err(GraphError::TooLong("the text")),
            Self::Concept { label } if label.len() > MAX_LABEL_BYTES => Err(GraphError::TooLong("the label")),
            Self::Image { image, alt } => {
                if image.0.len() > MAX_IMAGE_BYTES {
                    return Err(GraphError::TooLong("the picture"));
                }
                if !is_image(&image.0) {
                    return Err(GraphError::NotAnImage);
                }
                if alt.len() > MAX_TEXT_BYTES { Err(GraphError::TooLong("the description")) } else { Ok(()) }
            }
            _ => Ok(()),
        }
    }

    fn image_bytes(&self) -> usize {
        match self {
            Self::Image { image, .. } => image.0.len(),
            _ => 0,
        }
    }
}

/// Where a snippet came from: the document by its SHA-256 (so a moved file is found again), the
/// paths it was last seen at, the page (0-based), the region in view space, and the words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub sha256: String,
    pub paths: Vec<String>,
    pub title: String,
    pub page: u32,
    pub rect: Option<[f32; 4]>,
    pub quote: String,
}

impl Anchor {
    fn check(&self) -> Result<()> {
        let hex = self.sha256.len() == 64 && self.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !hex {
            return Err(GraphError::Unreadable("a source's document hash isn't 64 lowercase hex digits".into()));
        }
        if self.paths.len() > 8 || self.paths.iter().any(|p| p.len() > 4096) || self.title.len() > MAX_LABEL_BYTES {
            return Err(GraphError::TooLong("the source"));
        }
        if self.quote.len() > MAX_TEXT_BYTES {
            return Err(GraphError::TooLong("the quote"));
        }
        if self.rect.is_some_and(|r| r.iter().any(|v| !v.is_finite())) {
            return Err(GraphError::Unreadable("a source's region isn't finite".into()));
        }
        Ok(())
    }
}

/// Who made a node or an edge: the user, or the assistant (whose additions the user accepted).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    #[default]
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub content: Content,
    pub anchor: Option<Anchor>,
    pub position: [f32; 2],
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub author: Author,
}

/// How strongly two snippets are connected: drawn as the line's weight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strength {
    Weak,
    #[default]
    Medium,
    Strong,
}

impl Strength {
    pub fn name(self) -> &'static str {
        match self {
            Self::Weak => "weak",
            Self::Medium => "medium",
            Self::Strong => "strong",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "weak" => Some(Self::Weak),
            "medium" => Some(Self::Medium),
            "strong" => Some(Self::Strong),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Line {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

impl Line {
    pub fn name(self) -> &'static str {
        match self {
            Self::Solid => "solid",
            Self::Dashed => "dashed",
            Self::Dotted => "dotted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "solid" => Some(Self::Solid),
            "dashed" => Some(Self::Dashed),
            "dotted" => Some(Self::Dotted),
            _ => None,
        }
    }
}

/// The colours of kinds the user adds, taken in turn.
const OWN_COLOURS: [[u8; 3]; 6] = [[210, 90, 150], [90, 160, 60], [30, 120, 180], [180, 120, 40], [120, 80, 160], [40, 150, 120]];

/// A kind of connection: its name, whether it has a direction, and how it is drawn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeKind {
    pub id: KindId,
    pub name: String,
    pub directed: bool,
    pub line: Line,
    pub colour: [u8; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub from: NodeId,
    pub to: NodeId,
    pub kind: KindId,
    #[serde(default)]
    pub strength: Strength,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub author: Author,
}

/// The kinds a new map starts with. They can be renamed and more added.
pub fn default_kinds() -> Vec<EdgeKind> {
    let kind =
        |id: u32, name: &str, directed: bool, line: Line, colour: [u8; 3]| EdgeKind { id: KindId(id), name: name.into(), directed, line, colour };
    vec![
        kind(1, "is related to", false, Line::Solid, [120, 120, 130]),
        kind(2, "supports", true, Line::Solid, [46, 139, 87]),
        kind(3, "contradicts", true, Line::Dashed, [200, 60, 60]),
        kind(4, "is an example of", true, Line::Solid, [70, 110, 200]),
        kind(5, "defines", true, Line::Solid, [140, 90, 190]),
        kind(6, "causes", true, Line::Solid, [220, 140, 30]),
        kind(7, "is part of", true, Line::Dotted, [60, 150, 160]),
        kind(8, "comes before", true, Line::Dashed, [110, 110, 110]),
        kind(9, "see also", false, Line::Dotted, [150, 150, 150]),
    ]
}

/// A concept map.
#[derive(Clone, Debug, PartialEq)]
pub struct Graph {
    pub title: String,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    kinds: Vec<EdgeKind>,
    next_node: u64,
    next_edge: u64,
    next_kind: u32,
}

impl Default for Graph {
    fn default() -> Self {
        Self::new("Untitled map")
    }
}

fn clamp_position(p: [f32; 2]) -> [f32; 2] {
    p.map(|v| if v.is_finite() { v.clamp(-MAX_COORD, MAX_COORD) } else { 0.0 })
}

fn capped(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or_default().to_owned()
}

impl Graph {
    pub fn new(title: &str) -> Self {
        let kinds = default_kinds();
        let next_kind = kinds.iter().map(|k| k.id.0).max().unwrap_or(0).saturating_add(1);
        Self { title: capped(title.trim(), MAX_LABEL_BYTES), nodes: Vec::new(), edges: Vec::new(), kinds, next_node: 1, next_edge: 1, next_kind }
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }
    pub fn kinds(&self) -> &[EdgeKind] {
        &self.kinds
    }
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
    pub fn edge(&self, id: EdgeId) -> Option<&Edge> {
        self.edges.iter().find(|e| e.id == id)
    }
    pub fn kind(&self, id: KindId) -> Option<&EdgeKind> {
        self.kinds.iter().find(|k| k.id == id)
    }
    /// The kind with this name (case-insensitive), as the AI and the tools name kinds.
    pub fn kind_named(&self, name: &str) -> Option<&EdgeKind> {
        let name = name.trim();
        self.kinds.iter().find(|k| k.name.eq_ignore_ascii_case(name))
    }

    fn images_bytes(&self) -> usize {
        self.nodes.iter().fold(0usize, |n, node| n.saturating_add(node.content.image_bytes()))
    }

    /// Add a node. Its text, picture and source are checked against the caps.
    pub fn add_node(&mut self, content: Content, anchor: Option<Anchor>, position: [f32; 2], author: Author) -> Result<NodeId> {
        if self.nodes.len() >= MAX_NODES {
            return Err(GraphError::Full("nodes"));
        }
        content.check()?;
        if let Some(a) = &anchor {
            a.check()?;
        }
        if self.images_bytes().saturating_add(content.image_bytes()) > MAX_IMAGES_BYTES {
            return Err(GraphError::Full("pictures"));
        }
        let id = NodeId(self.next_node);
        self.next_node = self.next_node.saturating_add(1);
        self.nodes.push(Node { id, content, anchor, position: clamp_position(position), tags: Vec::new(), author });
        Ok(id)
    }

    /// Connect two nodes. A kind without a direction connects a pair once, whichever way round.
    pub fn add_edge(&mut self, from: NodeId, to: NodeId, kind: KindId, strength: Strength, label: Option<String>, author: Author) -> Result<EdgeId> {
        let id = EdgeId(self.next_edge);
        self.insert_edge(Edge { id, from, to, kind, strength, label, author })?;
        Ok(id)
    }

    /// Add `edge` with its own id, after the same checks as [`Self::add_edge`].
    fn insert_edge(&mut self, edge: Edge) -> Result<()> {
        if self.edges.len() >= MAX_EDGES {
            return Err(GraphError::Full("connections"));
        }
        if edge.from == edge.to {
            return Err(GraphError::SelfLoop);
        }
        for end in [edge.from, edge.to] {
            if self.node(end).is_none() {
                return Err(GraphError::NoNode(end.0));
            }
        }
        let directed = self.kind(edge.kind).ok_or(GraphError::NoKind(edge.kind.0))?.directed;
        let same =
            |e: &Edge| e.kind == edge.kind && ((e.from == edge.from && e.to == edge.to) || (!directed && e.from == edge.to && e.to == edge.from));
        if self.edges.iter().any(same) || self.edge(edge.id).is_some() {
            return Err(GraphError::Duplicate);
        }
        if edge.label.as_ref().is_some_and(|l| l.len() > MAX_LABEL_BYTES) {
            return Err(GraphError::TooLong("the label"));
        }
        self.next_edge = self.next_edge.max(edge.id.0.saturating_add(1));
        let label = edge.label.filter(|l| !l.trim().is_empty());
        self.edges.push(Edge { label, ..edge });
        Ok(())
    }

    /// Remove a node and its connections.
    pub fn remove_node(&mut self, id: NodeId) -> Result<()> {
        let before = self.nodes.len();
        self.nodes.retain(|n| n.id != id);
        if self.nodes.len() == before {
            return Err(GraphError::NoNode(id.0));
        }
        self.edges.retain(|e| e.from != id && e.to != id);
        Ok(())
    }

    pub fn remove_edge(&mut self, id: EdgeId) -> Result<()> {
        let before = self.edges.len();
        self.edges.retain(|e| e.id != id);
        if self.edges.len() == before { Err(GraphError::NoEdge(id.0)) } else { Ok(()) }
    }

    pub fn move_node(&mut self, id: NodeId, position: [f32; 2]) -> Result<()> {
        let node = self.nodes.iter_mut().find(|n| n.id == id).ok_or(GraphError::NoNode(id.0))?;
        node.position = clamp_position(position);
        Ok(())
    }

    pub fn set_content(&mut self, id: NodeId, content: Content) -> Result<()> {
        content.check()?;
        let old = self.node(id).ok_or(GraphError::NoNode(id.0))?.content.image_bytes();
        if self.images_bytes().saturating_sub(old).saturating_add(content.image_bytes()) > MAX_IMAGES_BYTES {
            return Err(GraphError::Full("pictures"));
        }
        let node = self.nodes.iter_mut().find(|n| n.id == id).ok_or(GraphError::NoNode(id.0))?;
        node.content = content;
        Ok(())
    }

    /// Change what node `id` says, keeping what it is: a snippet's or note's text, a concept's
    /// name, a picture's description. Trimmed; only a picture's description may be empty.
    pub fn set_text(&mut self, id: NodeId, text: &str) -> Result<()> {
        let text = text.trim().to_owned();
        let content = match &self.node(id).ok_or(GraphError::NoNode(id.0))?.content {
            Content::Image { image, .. } => Content::Image { image: image.clone(), alt: text },
            _ if text.is_empty() => return Err(GraphError::Empty("the text")),
            Content::Text { .. } => Content::Text { text },
            Content::Note { .. } => Content::Note { text },
            Content::Concept { .. } => Content::Concept { label: text },
        };
        self.set_content(id, content)
    }

    pub fn set_tags(&mut self, id: NodeId, tags: Vec<String>) -> Result<()> {
        if tags.len() > MAX_TAGS || tags.iter().any(|t| t.len() > MAX_LABEL_BYTES) {
            return Err(GraphError::TooLong("the tags"));
        }
        let node = self.nodes.iter_mut().find(|n| n.id == id).ok_or(GraphError::NoNode(id.0))?;
        node.tags = tags.into_iter().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()).collect();
        Ok(())
    }

    pub fn set_edge(&mut self, id: EdgeId, kind: KindId, strength: Strength, label: Option<String>) -> Result<()> {
        if self.kind(kind).is_none() {
            return Err(GraphError::NoKind(kind.0));
        }
        if label.as_ref().is_some_and(|l| l.len() > MAX_LABEL_BYTES) {
            return Err(GraphError::TooLong("the label"));
        }
        let edge = self.edges.iter_mut().find(|e| e.id == id).ok_or(GraphError::NoEdge(id.0))?;
        edge.kind = kind;
        edge.strength = strength;
        edge.label = label.filter(|l| !l.trim().is_empty());
        Ok(())
    }

    /// Add a kind of connection, or rename one (by id).
    pub fn add_kind(&mut self, name: &str, directed: bool, line: Line, colour: [u8; 3]) -> Result<KindId> {
        let name = name.trim();
        if name.is_empty() || name.len() > MAX_LABEL_BYTES {
            return Err(GraphError::TooLong("the kind's name"));
        }
        if let Some(k) = self.kind_named(name) {
            return Ok(k.id);
        }
        if self.kinds.len() >= MAX_KINDS {
            return Err(GraphError::Full("kinds of connection"));
        }
        let id = KindId(self.next_kind);
        self.next_kind = self.next_kind.saturating_add(1);
        self.kinds.push(EdgeKind { id, name: name.to_owned(), directed, line, colour });
        Ok(id)
    }

    /// The colour for the next kind the user adds.
    pub fn next_kind_colour(&self) -> [u8; 3] {
        OWN_COLOURS.get(self.kinds.len() % OWN_COLOURS.len()).copied().unwrap_or([120, 120, 130])
    }

    pub fn rename_kind(&mut self, id: KindId, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() || name.len() > MAX_LABEL_BYTES {
            return Err(GraphError::TooLong("the kind's name"));
        }
        let kind = self.kinds.iter_mut().find(|k| k.id == id).ok_or(GraphError::NoKind(id.0))?;
        kind.name = name.to_owned();
        Ok(())
    }

    /// The nodes within `depth` connections of `id`, itself included, in no particular order.
    pub fn neighbourhood(&self, id: NodeId, depth: usize) -> HashSet<NodeId> {
        let mut seen = HashSet::new();
        if self.node(id).is_none() {
            return seen;
        }
        seen.insert(id);
        let mut frontier = vec![id];
        for _ in 0..depth.min(16) {
            let mut next = Vec::new();
            for e in &self.edges {
                for (a, b) in [(e.from, e.to), (e.to, e.from)] {
                    if frontier.contains(&a) && seen.insert(b) {
                        next.push(b);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        seen
    }

    /// Nodes whose text, quote, title or tags contain `query` (case-insensitive).
    pub fn search(&self, query: &str) -> Vec<NodeId> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        self.nodes
            .iter()
            .filter(|n| {
                n.content.text().to_lowercase().contains(&q)
                    || n.anchor.as_ref().is_some_and(|a| a.quote.to_lowercase().contains(&q) || a.title.to_lowercase().contains(&q))
                    || n.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .map(|n| n.id)
            .collect()
    }

    pub fn to_json(&self) -> Result<Vec<u8>> {
        let saved =
            SavedRef { format: FORMAT, version: FORMAT_VERSION, title: &self.title, kinds: &self.kinds, nodes: &self.nodes, edges: &self.edges };
        serde_json::to_vec(&saved).map_err(|e| GraphError::Unreadable(e.to_string()))
    }

    /// Read a saved map. A wrong format or version is an error; damaged nodes, edges and kinds
    /// are dropped one by one, each counted in a warning, and the map is checked as if built.
    pub fn from_json(bytes: &[u8]) -> Result<(Self, Vec<String>)> {
        if bytes.len() > MAX_FILE_BYTES {
            return Err(GraphError::TooLong("the saved map"));
        }
        let saved: Saved = serde_json::from_slice(bytes).map_err(|e| GraphError::Unreadable(e.to_string()))?;
        if saved.format != FORMAT {
            return Err(GraphError::Unreadable("not a PdfCraft map".into()));
        }
        if saved.version != FORMAT_VERSION {
            return Err(GraphError::WrongVersion(saved.version));
        }
        let mut warnings = Vec::new();
        let mut graph = Graph::new(&saved.title);
        graph.kinds.clear();
        let mut dropped = 0usize;
        for Lenient(kind) in saved.kinds.into_iter().take(MAX_KINDS) {
            match kind {
                Some(k) if !k.name.trim().is_empty() && k.name.len() <= MAX_LABEL_BYTES && graph.kind(k.id).is_none() => {
                    graph.next_kind = graph.next_kind.max(k.id.0.saturating_add(1));
                    graph.kinds.push(k);
                }
                _ => dropped += 1,
            }
        }
        if graph.kinds.is_empty() {
            graph.kinds = default_kinds();
            graph.next_kind = graph.kinds.iter().map(|k| k.id.0).max().unwrap_or(0).saturating_add(1);
        }
        let mut ids: HashMap<NodeId, ()> = HashMap::new();
        for Lenient(node) in saved.nodes {
            let Some(node) = node else {
                dropped += 1;
                continue;
            };
            let fits = graph.nodes.len() < MAX_NODES
                && !ids.contains_key(&node.id)
                && node.content.check().is_ok()
                && node.anchor.as_ref().is_none_or(|a| a.check().is_ok())
                && node.tags.len() <= MAX_TAGS
                && graph.images_bytes().saturating_add(node.content.image_bytes()) <= MAX_IMAGES_BYTES;
            if !fits {
                dropped += 1;
                continue;
            }
            ids.insert(node.id, ());
            graph.next_node = graph.next_node.max(node.id.0.saturating_add(1));
            graph.nodes.push(Node { position: clamp_position(node.position), ..node });
        }
        for Lenient(edge) in saved.edges {
            match edge.map(|e| graph.insert_edge(e)) {
                Some(Ok(())) => {}
                _ => dropped += 1,
            }
        }
        if dropped > 0 {
            warnings.push(format!("{dropped} damaged or impossible parts of the map were left out"));
        }
        Ok((graph, warnings))
    }
}

/// Undo and redo for a map: whole-map snapshots (pictures are shared, so a snapshot is cheap).
#[derive(Default)]
pub struct History {
    undo: Vec<(String, Graph)>,
    redo: Vec<(String, Graph)>,
}

impl History {
    /// Record `before` as the state to go back to, labelled with what the change was.
    pub fn record(&mut self, label: &str, before: Graph) {
        self.undo.push((capped(label, MAX_LABEL_BYTES), before));
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|(l, _)| l.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|(l, _)| l.as_str())
    }

    /// Go back one step: the map before the last change, which `current` becomes.
    pub fn undo(&mut self, current: &mut Graph) -> Option<String> {
        let (label, before) = self.undo.pop()?;
        let now = std::mem::replace(current, before);
        self.redo.push((label.clone(), now));
        Some(label)
    }

    pub fn redo(&mut self, current: &mut Graph) -> Option<String> {
        let (label, after) = self.redo.pop()?;
        let now = std::mem::replace(current, after);
        self.undo.push((label.clone(), now));
        Some(label)
    }
}

/// One element of a saved list, or `None` if damaged: read through a `Value` of its own.
struct Lenient<T>(Option<T>);

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for Lenient<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        Ok(Self(serde_json::from_value(value).ok()))
    }
}

#[derive(Deserialize)]
struct Saved {
    format: String,
    version: u32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    kinds: Vec<Lenient<EdgeKind>>,
    #[serde(default)]
    nodes: Vec<Lenient<Node>>,
    #[serde(default)]
    edges: Vec<Lenient<Edge>>,
}

#[derive(Serialize)]
struct SavedRef<'a> {
    format: &'static str,
    version: u32,
    title: &'a str,
    kinds: &'a [EdgeKind],
    nodes: &'a [Node],
    edges: &'a [Edge],
}

#[cfg(test)]
mod tests;
