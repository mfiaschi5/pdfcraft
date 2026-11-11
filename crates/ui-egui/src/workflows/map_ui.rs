//! Concept maps in the app (ADR "Concept graph" G2–G3): a workspace that takes the document area,
//! with snippets of text and pictures from PDFs, notes and concepts, linked by connections of
//! several kinds and three strengths.
//!
//! - **Capture.** *Add to map* in the selection bar, *Add Image to Map* in a picture's page menu.
//!   Snippets follow the assistant's rules: a document that forbids copying gives nothing, and
//!   text under unapplied redaction marks is left out.
//! - **Navigate.** A node's source button (or a double-click) opens its document at the page, its
//!   passage marked while the node stays picked in the view of the map it was picked in.
//!   Selecting a node fades what isn't within two connections of it.
//! - **Edit.** Drag nodes; drag from a node's dot onto another to connect them with the chosen kind
//!   and strength. Each change is one undo step. Tidy up, export to JSON Canvas or GraphML.
//! - **AI.** With the assistant on, a request returns a proposal: the changes with their reasons,
//!   each kept or dropped by the user, applied together as one undo step. Nothing changes before.
//!
//! Maps are `.pdfmap.json` files in the assistant's folder, written atomically after each change.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use pdfcraft_ai::claude::{Options, Passage};
use pdfcraft_ai::source::{Sha256, SourceId};
use pdfcraft_ai::text::{capped, prefix};
use pdfcraft_engine::DocId;
use pdfcraft_engine::assistant::{self, Selected};
use pdfcraft_engine::map_assist::{self, EditRequest, MapEdit};
use pdfcraft_graph::proposal::{Change, NodeRef, Proposal};
use pdfcraft_graph::{Anchor, Author, Content, EdgeId, Graph, History, ImageBytes, KindId, Line, NodeId, Strength, export, layout};

use crate::PdfCraftApp;

mod board;
mod graph_view;
mod window;

use crate::theme::{self, Tokens};
pub(crate) use graph_view::Doc as GraphDoc;
pub(crate) use window::show as show_window;

/// A node's width on the map at 100%.
const NODE_WIDTH: f32 = 220.0;
/// The map new snippets go into when none is open.
const DEFAULT_MAP: &str = "My map";
/// The file name every map ends with.
const EXTENSION: &str = ".pdfmap.json";
/// Map files listed in the picker.
const MAX_LISTED: usize = 500;
/// A snippet's quote kept in its source.
const MAX_QUOTE_BYTES: usize = 2_000;
const STRENGTHS: [Strength; 3] = [Strength::Weak, Strength::Medium, Strength::Strong];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picked {
    Node(NodeId),
    Edge(EdgeId),
}

/// A proposal from the assistant, waiting for the user: which changes are kept.
pub struct Review {
    pub proposal: Proposal,
    pub keep: Vec<bool>,
    pub summary: String,
    pub errors: Vec<String>,
}

struct Running {
    rx: mpsc::Receiver<Result<MapEdit, String>>,
    cancel: Arc<AtomicBool>,
    /// The map it was asked about: a proposal for another map than the one shown is dropped.
    file: Option<PathBuf>,
}

pub struct MapState {
    /// The workspace shows instead of the document.
    pub open: bool,
    pub file: Option<PathBuf>,
    pub graph: Graph,
    history: History,
    pub picked: Option<Picked>,
    pan: egui::Vec2,
    zoom: f32,
    /// The node a connection is being dragged from.
    connecting: Option<NodeId>,
    /// A node was dragged and the map not yet saved.
    moved: bool,
    pub new_kind: KindId,
    pub new_strength: Strength,
    /// Connections weaker than this are hidden.
    pub min_strength: Strength,
    pub note_draft: String,
    /// The panel's search for a node to connect to.
    pub filter: String,
    /// A kind of connection of the user's own, being named.
    pub kind_draft: String,
    pub kind_directed: bool,
    pub request: String,
    pub review: Option<Review>,
    running: Option<Running>,
    /// Where maps are kept: [`maps_dir`] in the desktop app, a folder of their own in tests, and
    /// none on the web, where maps aren't offered.
    pub dir: Option<PathBuf>,
    /// The graph in the panel beside the document, and in the map's own window.
    pub(crate) panel_view: graph_view::GraphView,
    pub(crate) window_view: graph_view::GraphView,
    /// The map shows in a window of its own (another screen), as well as beside the document.
    pub window: bool,
    /// A node's passage to mark once its document is open (it was opening when the node was
    /// clicked).
    pending_mark: Option<PendingMark>,
    /// The node whose passage is marked in the documents, and the view of the map it was picked
    /// in: the mark stays while the node is picked there ([`PdfCraftApp::sync_node_mark`]).
    marked: Option<(NodeId, EditIn)>,
    /// The whole map's board: its preview, glides and pictures.
    pub(crate) board: board::Board,
    /// The node whose text is being edited, and where.
    pub editing: Option<Editing>,
}

struct PendingMark {
    anchor: Anchor,
}

/// A view of the map: where a node is being edited (its editor shows there), and where a node
/// was picked to mark its passage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditIn {
    /// The whole map, over the node's card.
    Board,
    /// The graph beside the document, in the panel under it.
    Panel,
    /// The map's own window, beside its graph.
    Window,
}

/// A node's text being edited.
pub struct Editing {
    pub id: NodeId,
    pub text: String,
    pub place: EditIn,
    /// The field takes the keyboard on its first frame.
    focus: bool,
}

/// The desktop app's folder for maps: `Maps` in the assistant's folder.
pub fn maps_dir() -> Option<PathBuf> {
    crate::assistant_ui::assistant_dir().map(|d| d.join("Maps"))
}

impl Default for MapState {
    fn default() -> Self {
        let graph = Graph::new(DEFAULT_MAP);
        let new_kind = graph.kinds().first().map_or(KindId(1), |k| k.id);
        Self {
            open: false,
            file: None,
            graph,
            history: History::default(),
            picked: None,
            pan: egui::Vec2::ZERO,
            zoom: 1.0,
            connecting: None,
            moved: false,
            new_kind,
            new_strength: Strength::Medium,
            min_strength: Strength::Weak,
            note_draft: String::new(),
            filter: String::new(),
            kind_draft: String::new(),
            kind_directed: true,
            request: String::new(),
            review: None,
            running: None,
            dir: None,
            panel_view: graph_view::GraphView::default(),
            window_view: graph_view::GraphView::default(),
            window: false,
            pending_mark: None,
            marked: None,
            board: board::Board::default(),
            editing: None,
        }
    }
}

impl MapState {
    fn dir(&self) -> Option<PathBuf> {
        self.dir.clone()
    }

    /// Maps can be kept: the desktop app gave them a folder ([`maps_dir`]).
    pub fn available(&self) -> bool {
        self.dir.is_some()
    }

    /// The map files in the folder, by name.
    pub fn files(&self) -> Vec<PathBuf> {
        let Some(dir) = self.dir() else { return Vec::new() };
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|it| it.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(EXTENSION)).take(MAX_LISTED).collect())
            .unwrap_or_default();
        files.sort();
        files
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn can_undo(&self) -> bool {
        self.history.undo_label().is_some()
    }

    pub fn can_redo(&self) -> bool {
        self.history.redo_label().is_some()
    }

    /// Show `graph`, kept in `file`, with nothing selected and no undo steps.
    fn reset(&mut self, graph: Graph, file: Option<PathBuf>) {
        self.new_kind = graph.kinds().first().map_or(KindId(1), |k| k.id);
        self.graph = graph;
        self.file = file;
        self.history = History::default();
        self.picked = None;
        self.review = None;
        self.connecting = None;
        self.moved = false;
        self.editing = None;
        // Another map: fitted to the board when it shows.
        self.board.fit = true;
    }
}

/// A file name for a map title: letters, digits, spaces, `-` and `_`, the rest as `_`.
fn file_name(title: &str) -> String {
    let safe: String = title.chars().take(80).map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_') { c } else { '_' }).collect();
    let safe = safe.trim();
    format!("{}{EXTENSION}", if safe.is_empty() { "map" } else { safe })
}

fn colour(c: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(c[0], c[1], c[2])
}

fn strength_name(s: Strength) -> &'static str {
    match s {
        Strength::Weak => tl!("Weak"),
        Strength::Medium => tl!("Medium"),
        Strength::Strong => tl!("Strong"),
    }
}

/// What a source is called on a node: its title and 1-based page.
fn source_label(a: &Anchor) -> String {
    crate::i18n::fmt(tl!("{doc}, p. {n}"), &[("doc", &capped(&a.title, 60)), ("n", &(u64::from(a.page) + 1).to_string())])
}

impl PdfCraftApp {
    /// Show the map workspace, with the first saved map when none is loaded.
    pub(crate) fn open_map_workspace(&mut self) {
        if !self.maps.available() {
            self.notify_tr("Concept maps need the desktop app");
            return;
        }
        self.ensure_map();
        self.maps.open = true;
        self.maps.board.fit = true;
    }

    /// Make sure changes have a map file to go to: the one shown, else the default map, else the
    /// first saved, else a new default map. A map file that can't be read is never written over:
    /// this says so and is false until another map is chosen.
    fn ensure_map(&mut self) -> bool {
        if self.maps.file.is_some() {
            return true;
        }
        let Some(dir) = self.maps.dir() else {
            self.notify_tr("Concept maps need the desktop app");
            return false;
        };
        let default = dir.join(file_name(DEFAULT_MAP));
        let saved = if default.exists() { Some(default.clone()) } else { self.maps.files().into_iter().next() };
        match saved {
            Some(path) => {
                self.load_map(path);
                self.maps.file.is_some()
            }
            None => {
                self.maps.file = Some(default);
                true
            }
        }
    }

    fn load_map(&mut self, path: PathBuf) {
        let read = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| Graph::from_json(&b).map_err(|e| e.to_string()));
        match read {
            Ok((graph, warnings)) => {
                self.maps.reset(graph, Some(path));
                if let Some(w) = warnings.into_iter().next() {
                    self.notify(w);
                }
            }
            // Left as it is, never written over: the map shown stays the one before.
            Err(e) => self.notify_fmt("Couldn't open the map: {e}", &[("e", &e)]),
        }
    }

    fn new_map(&mut self) {
        let Some(dir) = self.maps.dir() else {
            self.notify_tr("Concept maps need the desktop app");
            return;
        };
        let title = tl!("New map");
        let mut name = title.to_owned();
        let mut n = 2;
        while dir.join(file_name(&name)).exists() && n < 1000 {
            name = format!("{title} {n}");
            n += 1;
        }
        self.maps.reset(Graph::new(&name), Some(dir.join(file_name(&name))));
        self.save_map();
    }

    /// Write the map to its file ([`Self::ensure_map`] chose it).
    fn save_map(&mut self) {
        let Some(path) = self.maps.file.clone() else { return };
        let written = self.maps.graph.to_json().map_err(|e| e.to_string()).and_then(|bytes| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            crate::editing::write_atomically(&path.to_string_lossy(), &bytes).map_err(|e| e.to_string())
        });
        if let Err(e) = written {
            self.notify_fmt("Couldn't save the map: {e}", &[("e", &e)]);
        }
    }

    /// Change the map as one undo step, and save it. A change that fails leaves it as it was.
    fn change_map(&mut self, label: &str, change: impl FnOnce(&mut Graph) -> Result<(), String>) -> bool {
        if !self.ensure_map() {
            return false;
        }
        let before = self.maps.graph.clone();
        match change(&mut self.maps.graph) {
            Ok(()) => {
                self.maps.history.record(label, before);
                self.save_map();
                true
            }
            Err(e) => {
                self.maps.graph = before;
                self.notify(e);
                false
            }
        }
    }

    /// Where a new node goes: right of the rightmost node, so it doesn't cover one.
    fn free_spot(&self) -> [f32; 2] {
        let nodes = self.maps.graph.nodes();
        match nodes.iter().max_by(|a, b| a.position[0].total_cmp(&b.position[0])) {
            Some(right) => [(right.position[0] + NODE_WIDTH + 40.0).min(pdfcraft_graph::MAX_COORD), right.position[1]],
            None => [0.0, 0.0],
        }
    }

    fn anchor_for(&self, id: DocId, page: usize, rect: Option<[f32; 4]>, quote: &str) -> Anchor {
        Anchor {
            sha256: self.session.document_sha256(id).map(|h| h.to_hex()).unwrap_or_default(),
            paths: self.session.get(id).and_then(|d| d.path.clone()).into_iter().collect(),
            title: self.session.document_title(id).unwrap_or_default(),
            page: u32::try_from(page).unwrap_or(u32::MAX),
            rect,
            quote: capped(quote, MAX_QUOTE_BYTES),
        }
    }

    /// After a snippet went on the map: it is the node to connect next, in the map's panel beside
    /// the document (or the workspace, when that shows).
    fn added_to_map(&mut self, id: NodeId) {
        self.maps.picked = Some(Picked::Node(id));
        self.maps.filter.clear();
        if !self.maps.open {
            self.choose_right_panel(Some(crate::RightPanel::Map));
        }
        let name = self.maps.graph.title.clone();
        self.notify_fmt("Added to the map “{map}”", &[("map", &name)]);
    }

    /// The selection bar's Add to map: the selected text, with its document and page.
    pub(crate) fn add_selection_to_map(&mut self, index: usize) {
        let Some(view) = self.views.get(index) else { return };
        let id = view.id;
        let Some((page, selected)) = view.assistant_selection() else {
            self.notify_tr("Select some text first.");
            return;
        };
        let ctx = match self.session.assistant_context(id, page, &selected) {
            Ok(ctx) => ctx,
            Err(e) => {
                self.notify(e.to_string());
                return;
            }
        };
        if ctx.text.trim().is_empty() {
            self.notify_tr("There is no text there to add.");
            return;
        }
        let text = capped(&ctx.text, pdfcraft_graph::MAX_TEXT_BYTES);
        let rect = match selected {
            Selected::Region(r) => Some(r),
            _ => None,
        };
        let anchor = self.anchor_for(id, page, rect, &text);
        let at = self.free_spot();
        let mut added = None;
        self.change_map("Add snippet", |g| {
            added = Some(g.add_node(Content::Text { text }, Some(anchor), at, Author::User).map_err(|e| e.to_string())?);
            Ok(())
        });
        if let Some(node) = added {
            self.added_to_map(node);
        }
    }

    /// A picture's page menu: Add Image to Map. The picture's file, when copying is allowed.
    pub(crate) fn add_image_to_map(&mut self, index: usize, page: usize, image: usize) {
        let Some(id) = self.views.get(index).map(|v| v.id) else { return };
        let Some(doc) = self.session.get(id) else { return };
        let bytes = match doc.page_image_for_map(page, image, pdfcraft_graph::MAX_IMAGE_BYTES) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        if !pdfcraft_graph::is_image(&bytes) || bytes.len() > pdfcraft_graph::MAX_IMAGE_BYTES {
            self.notify_tr("That picture can't go on a map: it must be a PNG or JPEG file of at most 2 MB.");
            return;
        }
        let title = self.session.document_title(id).unwrap_or_default();
        let alt = crate::i18n::fmt(tl!("Picture on page {n} of {doc}"), &[("n", &(page + 1).to_string()), ("doc", &title)]);
        let anchor = self.anchor_for(id, page, None, "");
        let at = self.free_spot();
        let content = Content::Image { image: ImageBytes(Arc::new(bytes)), alt: capped(&alt, pdfcraft_graph::MAX_LABEL_BYTES) };
        let mut added = None;
        self.change_map("Add picture", |g| {
            added = Some(g.add_node(content, Some(anchor), at, Author::User).map_err(|e| e.to_string())?);
            Ok(())
        });
        if let Some(node) = added {
            self.added_to_map(node);
        }
    }

    /// Open a node's source: the open document with its bytes, else the one open from its path
    /// (changed since, by an edit), else the file at its last path.
    fn open_anchor(&mut self, anchor: &Anchor) {
        let Some(sha) = Sha256::from_hex(&anchor.sha256) else {
            self.notify_tr("This snippet's document isn't known.");
            return;
        };
        let page = usize::try_from(anchor.page).unwrap_or(usize::MAX);
        let same_bytes = self.views.iter().position(|v| self.session.document_sha256(v.id) == Some(sha));
        let same_file = || {
            self.views
                .iter()
                .position(|v| self.session.get(v.id).and_then(|d| d.path.as_deref()).is_some_and(|p| anchor.paths.iter().any(|a| a == p)))
        };
        if let Some(i) = same_bytes.or_else(same_file) {
            self.maps.open = false;
            self.active = Some(i);
            if let Some(view) = self.views.get_mut(i)
                && page < view.page_count()
            {
                view.go_to_page(page);
            }
            return;
        }
        match anchor.paths.first() {
            Some(path) => {
                self.open_library_document(path.clone(), page, sha);
                // It is opening (and goes to the page once open); a missing file leaves the map.
                if self.assistant.jump_after_open.is_some() {
                    self.maps.open = false;
                }
            }
            None => self.notify_tr("This snippet's document isn't open, and where its file is isn't known."),
        }
    }

    /// Pick a node and go to its passage, marked where it starts and ends; a document that has to
    /// open first is marked once it is. The mark of the node picked before goes, in every
    /// document. `from` is the view of the map it was picked in.
    pub(crate) fn reveal_node(&mut self, id: NodeId, from: EditIn) {
        self.maps.picked = Some(Picked::Node(id));
        self.clear_node_marks();
        let Some(anchor) = self.maps.graph.node(id).and_then(|n| n.anchor.clone()) else { return };
        self.open_anchor(&anchor);
        self.maps.marked = Some((id, from));
        self.maps.pending_mark = Some(PendingMark { anchor });
        self.apply_pending_mark();
    }

    /// Take every document's passage mark away, and the one waiting for its document to open.
    fn clear_node_marks(&mut self) {
        self.maps.marked = None;
        self.maps.pending_mark = None;
        let mut cleared = false;
        for view in &mut self.views {
            cleared |= view.clear_passage_mark();
        }
        // The page drew it this frame: the next frame draws it gone.
        if cleared && let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    /// A node's passage stays marked only while the node is picked and the view of the map it was
    /// picked in is open: the graph beside the document, the map's window, or the whole map (which
    /// gives way to the document to show the passage). Picking another node, a connection or
    /// nothing, removing the node, or closing that view takes the mark away. Run once the frame's
    /// map actions have.
    pub(crate) fn sync_node_mark(&mut self) {
        let Some((id, from)) = self.maps.marked else { return };
        // While its document opens, the panel the document asks for may stand in for the graph
        // ([`Self::apply_pending_mark`] puts it back).
        let opening = self.maps.pending_mark.is_some();
        let open = opening
            || match from {
                EditIn::Panel => self.right == Some(crate::RightPanel::Map),
                EditIn::Window => self.maps.window,
                EditIn::Board => true,
            };
        if !open || self.maps.picked != Some(Picked::Node(id)) || self.maps.graph.node(id).is_none() {
            self.clear_node_marks();
        }
    }

    /// Mark the pending passage once its document is in a tab.
    fn apply_pending_mark(&mut self) {
        let Some(pending) = &self.maps.pending_mark else { return };
        let a = &pending.anchor;
        let sha = Sha256::from_hex(&a.sha256);
        let by_bytes = self.views.iter().position(|v| sha.is_some() && self.session.document_sha256(v.id) == sha);
        let by_path =
            || self.views.iter().position(|v| self.session.get(v.id).and_then(|d| d.path.as_deref()).is_some_and(|p| a.paths.iter().any(|q| q == p)));
        match by_bytes.or_else(by_path) {
            Some(i) => {
                let Some(PendingMark { anchor }) = self.maps.pending_mark.take() else { return };
                // A document that opened for it may have asked for another panel (/PageMode): the
                // graph the node was picked in stays beside it, and so does the mark.
                if matches!(self.maps.marked, Some((_, EditIn::Panel))) && self.right != Some(crate::RightPanel::Map) {
                    self.choose_right_panel(Some(crate::RightPanel::Map));
                }
                self.active = Some(i);
                if let Some(view) = self.views.get_mut(i) {
                    view.mark_passage(usize::try_from(anchor.page).unwrap_or(usize::MAX), &anchor.quote, anchor.rect);
                }
            }
            // Not opening (any more): nothing to mark.
            None if self.assistant.jump_after_open.is_none() => self.maps.pending_mark = None,
            None => {}
        }
    }

    /// Ask the assistant for changes to the map, on a worker thread.
    fn start_map_edit(&mut self) {
        let Some(service) = self.ai_service.clone() else { return };
        if self.maps.running.is_some() {
            return;
        }
        if !self.assistant.prefs.enabled {
            self.notify_tr("Turn the AI assistant on in Preferences first.");
            return;
        }
        // Consent is asked once, in the Assistant panel, with what is sent and where.
        if !self.assistant.prefs.consented {
            self.notify_tr("Ask a first question in the AI assistant panel to allow requests, then try again.");
            return;
        }
        let (transport, key) = match service.route(&self.assistant.prefs) {
            Ok(route) => route,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        let request = self.maps.request.trim().to_owned();
        if request.is_empty() {
            return;
        }
        // What it may quote: the page being read, and the library's best matches.
        let mut passages: Vec<Passage> = Vec::new();
        let mut doc_anchor: Option<Anchor> = None;
        let active = self.active.and_then(|i| self.views.get(i)).map(|v| (v.id, v.current));
        if let Some((id, page)) = active {
            match self.session.assistant_context(id, page, &Selected::Page) {
                Ok(ctx) => {
                    passages.extend(assistant::context_passages(&ctx));
                    doc_anchor = Some(self.anchor_for(id, page, None, ""));
                }
                Err(e) => log::info!("the map request leaves out the open page: {e}"),
            }
        }
        let (library, entries) = self.library_for_map(active.map(|(id, _)| id), &request);
        passages.extend(library);
        passages.truncate(pdfcraft_ai::claude::MAX_PASSAGES);
        let scope: Option<HashSet<NodeId>> = match self.maps.picked {
            Some(Picked::Node(id)) => Some(self.maps.graph.neighbourhood(id, 2)),
            _ => None,
        };
        let graph = self.maps.graph.clone();
        let model = self.assistant.prefs.model();
        let effort = self.assistant.prefs.effort;
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let work = move || {
            let anchor = move |p: &Passage, quote: &str| -> Option<Anchor> {
                let quote = capped(quote, MAX_QUOTE_BYTES);
                match p.id {
                    SourceId::ThisDocument { page } => doc_anchor.clone().map(|a| Anchor { page, quote, ..a }),
                    SourceId::Library { sha256, page } => {
                        let hex = sha256.to_hex();
                        let (_, paths, title) = entries.iter().find(|(h, ..)| *h == hex)?.clone();
                        Some(Anchor { sha256: hex, paths, title, page, rect: None, quote })
                    }
                    // A map snippet quotes a document, not the wiki.
                    SourceId::Wiki { .. } => None,
                }
            };
            let req = EditRequest {
                options: Options { effort, ..Options::new(model) },
                key: &key,
                graph: &graph,
                scope: scope.as_ref(),
                passages,
                request: &request,
                anchor: &anchor,
            };
            let result = pdfcraft_engine::guard(|| {
                map_assist::propose(transport.as_ref(), req, &mut |_| {
                    if stop.load(Ordering::Relaxed) { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
                })
            })
            .unwrap_or_else(|panic| Err(format!("the request failed unexpectedly ({panic})")));
            let _ = tx.send(result);
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        };
        // If the thread can't start, the dropped sender reads as "stopped unexpectedly".
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(e) = std::thread::Builder::new().name("ai-map".into()).spawn(work) {
            log::warn!("couldn't start the map assistant's worker: {e}");
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.maps.running = Some(Running { rx, cancel, file: self.maps.file.clone() });
    }

    /// Take the assistant's proposal once it has come.
    pub(crate) fn poll_map(&mut self) {
        self.apply_pending_mark();
        let Some(running) = &self.maps.running else { return };
        let result = match running.rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(tl!("The request stopped unexpectedly.").to_owned()),
        };
        let cancelled = running.cancel.load(Ordering::Relaxed);
        let same_map = running.file == self.maps.file;
        self.maps.running = None;
        match result {
            Ok(_) if !same_map => self.notify_tr("Another map was opened, so the assistant's proposal was dropped."),
            Ok(edit) if edit.proposal.changes.is_empty() => {
                let said = if edit.summary.trim().is_empty() { tl!("The assistant proposed no changes.").to_owned() } else { edit.summary };
                self.notify(said);
            }
            Ok(edit) => {
                let keep = vec![true; edit.proposal.changes.len()];
                self.maps.review = Some(Review { proposal: edit.proposal, keep, summary: edit.summary, errors: edit.errors });
                self.maps.request.clear();
            }
            Err(_) if cancelled => {}
            Err(e) => self.notify(e),
        }
    }

    /// Apply the changes the user kept, as one undo step.
    fn apply_review(&mut self) {
        if !self.ensure_map() {
            return;
        }
        let Some(review) = self.maps.review.take() else { return };
        let accepted: Vec<usize> = review.keep.iter().enumerate().filter(|(_, k)| **k).map(|(i, _)| i).collect();
        if accepted.is_empty() {
            return;
        }
        let before = self.maps.graph.clone();
        let applied = review.proposal.apply(&mut self.maps.graph, &accepted);
        if applied.skipped.len() < accepted.len() {
            self.maps.history.record("Apply the assistant's changes", before);
            self.save_map();
        }
        if !applied.skipped.is_empty() {
            self.notify_fmt("{n} of the kept changes couldn't be applied.", &[("n", &applied.skipped.len().to_string())]);
        }
    }

    /// Undo (or redo) the map's last change: from the toolbar, Edit ▸ Undo and ⌘Z.
    pub(crate) fn map_history_step(&mut self, undo: bool) {
        let maps = &mut self.maps;
        let stepped = if undo { maps.history.undo(&mut maps.graph) } else { maps.history.redo(&mut maps.graph) };
        if stepped.is_some() {
            maps.picked = None;
            maps.review = None;
            self.save_map();
        }
    }

    /// The map's keys, while it shows: Delete removes what is selected; Escape clears the
    /// selection, then goes back to the document. Typing in a box keeps its keys.
    pub(crate) fn map_keys(&mut self, ctx: &egui::Context) {
        // While a node is edited, its editor has the keys (Escape cancels it there).
        if ctx.egui_wants_keyboard_input() || self.maps.editing.is_some() {
            return;
        }
        let edit = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) || i.consume_key(egui::Modifiers::NONE, egui::Key::F2));
        if edit && let Some(Picked::Node(id)) = self.maps.picked {
            run(self, Action::Edit(id, EditIn::Board));
            return;
        }
        let (delete, escape) =
            ctx.input(|i| (i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace), i.key_pressed(egui::Key::Escape)));
        if delete && let Some(p) = self.maps.picked {
            run(self, Action::Remove(p));
        }
        if escape && self.maps.picked.take().is_none() {
            self.maps.open = false;
        }
    }

    /// Write the map as JSON Canvas or GraphML into a folder the user chooses (a download on the
    /// web).
    fn export_map(&mut self, format: Export) {
        let name = file_name(&self.maps.graph.title);
        let stem = name.strip_suffix(EXTENSION).unwrap_or("map");
        let (out, bytes) = match format {
            Export::Canvas => match serde_json::to_vec_pretty(&export::to_json_canvas(&self.maps.graph)) {
                Ok(bytes) => (format!("{stem}.canvas"), bytes),
                Err(e) => {
                    self.notify_fmt("Couldn't export the map: {e}", &[("e", &e.to_string())]);
                    return;
                }
            },
            Export::GraphMl => (format!("{stem}.graphml"), export::to_graphml(&self.maps.graph).into_bytes()),
        };
        self.write_files(&[(out, Arc::new(bytes))], tl!("Export map"));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Export {
    Canvas,
    GraphMl,
}

/// What the workspace and the panel ask the app to do, after drawing.
pub(crate) enum Action {
    Close,
    New,
    Load(PathBuf),
    Undo,
    Redo,
    Tidy,
    AddNote,
    AddConcept,
    AddKind,
    DragStart,
    Drag(NodeId, [f32; 2]),
    DragEnd,
    Connect(NodeId, NodeId),
    Remove(Picked),
    SetEdge(EdgeId, KindId, Strength),
    Export(Export),
    Ask,
    StopAsk,
    ApplyReview,
    DiscardReview,
    /// The panel: make this node the one to connect from.
    Focus(NodeId),
    Unfocus,
    /// The panel: show the whole map.
    OpenWhole,
    /// The panel: add the document's selection.
    AddSelection,
    /// Pick the node and go to its passage, marked where it starts and ends while the node stays
    /// picked in this view of the map.
    Reveal(NodeId, EditIn),
    /// The map in a window of its own, or not.
    Window(bool),
    /// Edit a node's text, in this view.
    Edit(NodeId, EditIn),
    /// Keep the text being edited, or leave it.
    SaveEdit,
    CancelEdit,
}

/// The map workspace, in the document area.
pub(crate) fn workspace(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let mut actions: Vec<Action> = Vec::new();
    // The side panel shows when there is room for it beside the board, unless asked otherwise.
    let side_open = app.maps.board.side.unwrap_or(ui.available_width() >= 900.0);
    egui::Panel::top("map-toolbar")
        .frame(egui::Frame::NONE.fill(t.panel).inner_margin(egui::Margin::symmetric(10, 6)).stroke(egui::Stroke::new(1.0, t.divider)))
        .show(ui, |ui| toolbar(ui, &t, app, side_open, &mut actions));
    if side_open {
        egui::Panel::right("map-side")
            .resizable(false)
            .exact_size(300.0)
            .frame(egui::Frame::NONE.fill(t.pasteboard).inner_margin(egui::Margin::symmetric(10, 10)))
            .show(ui, |ui| egui::ScrollArea::vertical().show(ui, |ui| side(ui, &t, app, &mut actions)));
    }
    // Previews render from the document being read.
    let active = app.active.and_then(|i| app.views.get(i).map(|v| (i, v.id)));
    let sha256 = active.and_then(|(_, id)| app.session.document_sha256(id)).map(|h| h.to_hex());
    let info = active.and_then(|(_, id)| app.session.get(id)).map(|d| &d.info);
    let view = active.and_then(|(i, _)| app.views.get_mut(i));
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.pasteboard)).show(ui, |ui| {
        let mut doc = match (sha256, info, view) {
            (Some(sha256), Some(info), Some(view)) => Some(GraphDoc { sha256, info, view }),
            _ => None,
        };
        board::canvas(ui, &t, &mut app.maps, doc.as_mut(), &mut actions);
        // The document isn't drawn, so its previews' renders are asked for here.
        if let Some(d) = doc {
            d.view.queue_passage_requests(d.info);
        }
    });
    for action in actions {
        run(app, action);
    }
}

fn toolbar(ui: &mut egui::Ui, t: &Tokens, app: &mut PdfCraftApp, side_open: bool, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if ui.add(egui::Button::image_and_text(crate::icons::image("chevron-left", 14.0, t.icon), tl!("Back to the document"))).clicked() {
            actions.push(Action::Close);
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(crate::icons::image("share-2", 15.0, t.accent));
            let current = app.maps.graph.title.clone();
            craft_ui::ComboBox::from_id_salt("map-file")
                .selected_text(egui::RichText::new(current).font(theme::semibold(13.0)))
                .width(170.0)
                .show_ui(ui, |ui| {
                    for path in app.maps.files() {
                        let name = path.file_name().map(|n| n.to_string_lossy().trim_end_matches(EXTENSION).to_owned()).unwrap_or_default();
                        if ui.selectable_label(app.maps.file.as_ref() == Some(&path), name).clicked() {
                            actions.push(Action::Load(path.clone()));
                        }
                    }
                });
            if crate::icons::button(ui, "file-plus", 26.0, false, tl!("New map")).clicked() {
                actions.push(Action::New);
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            let undo = app.maps.history.undo_label().is_some();
            if ui.add_enabled_ui(undo, |ui| crate::icons::button(ui, "undo-2", 26.0, false, tl!("Undo"))).inner.clicked() {
                actions.push(Action::Undo);
            }
            let redo = app.maps.history.redo_label().is_some();
            if ui.add_enabled_ui(redo, |ui| crate::icons::button(ui, "redo-2", 26.0, false, tl!("Redo"))).inner.clicked() {
                actions.push(Action::Redo);
            }
            let tidy = app.maps.graph.nodes().len() > 1;
            if ui.add_enabled(tidy, egui::Button::image_and_text(crate::icons::image("wand-sparkles", 14.0, t.icon), tl!("Tidy up"))).clicked() {
                actions.push(Action::Tidy);
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            if crate::icons::button(ui, "zoom-out", 26.0, false, tl!("Zoom out")).clicked() {
                app.maps.board.zoom_by = Some(1.0 / 1.25);
            }
            if crate::icons::button(ui, "zoom-in", 26.0, false, tl!("Zoom in")).clicked() {
                app.maps.board.zoom_by = Some(1.25);
            }
            if crate::icons::button(ui, "maximize", 26.0, false, tl!("Fit the map")).clicked() {
                app.maps.board.fit = true;
            }
        });
        ui.separator();
        // The kind and strength of new connections, together so they wrap as one.
        if ui.available_size_before_wrap().x < 360.0 {
            ui.end_row();
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tl!("New connections:")).color(t.text_muted));
            let kind = app.maps.graph.kind(app.maps.new_kind);
            let kind_name = kind.map(|k| k.name.clone()).unwrap_or_default();
            if let Some(k) = kind {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.5, colour(k.colour));
            }
            craft_ui::ComboBox::from_id_salt("map-kind").selected_text(tl!(&kind_name)).width(140.0).show_ui(ui, |ui| {
                for k in app.maps.graph.kinds() {
                    ui.selectable_value(&mut app.maps.new_kind, k.id, tl!(&k.name));
                }
            });
            craft_ui::ComboBox::from_id_salt("map-strength").selected_text(strength_name(app.maps.new_strength)).width(90.0).show_ui(ui, |ui| {
                for s in STRENGTHS {
                    ui.selectable_value(&mut app.maps.new_strength, s, strength_name(s));
                }
            });
        });
        ui.separator();
        ui.menu_button(tl!("Export"), |ui| {
            if ui.button(tl!("JSON Canvas (Obsidian)")).clicked() {
                actions.push(Action::Export(Export::Canvas));
                ui.close();
            }
            if ui.button(tl!("GraphML (yEd, Gephi)")).clicked() {
                actions.push(Action::Export(Export::GraphMl));
                ui.close();
            }
        });
        let tip = if side_open { tl!("Hide the side panel") } else { tl!("Show the side panel") };
        if crate::icons::button(ui, "panel-right", 26.0, side_open, tip).clicked() {
            app.maps.board.side = Some(!side_open);
        }
    });
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(text).font(theme::semibold(13.0)));
}

fn hint(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.label(egui::RichText::new(text).small().color(t.text_faint));
}

/// One of the side panel's sections: a card with an icon and a title in `tint`.
fn section(ui: &mut egui::Ui, t: &Tokens, icon: &str, tint: egui::Color32, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(t.card)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Fields stand out from the card they are on, in either theme.
            ui.visuals_mut().text_edit_bg_color = Some(t.pasteboard);
            ui.horizontal(|ui| {
                ui.add(crate::icons::image(icon, 15.0, tint));
                ui.label(egui::RichText::new(title).font(theme::semibold(13.5)).color(t.text));
            });
            ui.add_space(6.0);
            add(ui);
        });
    ui.add_space(10.0);
}

fn side(ui: &mut egui::Ui, t: &Tokens, app: &mut PdfCraftApp, actions: &mut Vec<Action>) {
    // The assistant's proposal comes first while it waits: it is what the user decides on now.
    if let Some(review) = &mut app.maps.review {
        let graph = &app.maps.graph;
        section(ui, t, "sparkles", t.accent, tl!("The assistant's proposal"), |ui| review_ui(ui, t, graph, review, actions));
    }
    // What is picked, next: it is what the user is looking at.
    match app.maps.picked {
        Some(Picked::Node(id)) => {
            let kind = app.maps.graph.node(id).map(|n| board::Kind::of(&n.content));
            let (icon, tint, title) = kind.map_or(("circle", t.accent, tl!("Node")), |k| (k.icon(), board::look(t, k).tint, k.name()));
            let graph = &app.maps.graph;
            section(ui, t, icon, tint, title, |ui| node_ui(ui, t, graph, id, actions));
        }
        Some(Picked::Edge(id)) => {
            let graph = &app.maps.graph;
            section(ui, t, "spline", t.accent, tl!("Connection"), |ui| edge_ui(ui, graph, id, actions));
        }
        None => {}
    }
    section(ui, t, "plus", t.accent, tl!("Add"), |ui| {
        ui.add(
            crate::widgets::block(&mut app.maps.note_draft)
                .hint_text(tl!("A note or a concept"))
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .char_limit(4096),
        );
        ui.horizontal(|ui| {
            let some = !app.maps.note_draft.trim().is_empty();
            let note = board::look(t, board::Kind::Note).tint;
            if ui.add_enabled(some, egui::Button::image_and_text(crate::icons::image("sticky-note", 14.0, note), tl!("Add note"))).clicked() {
                actions.push(Action::AddNote);
            }
            if ui.add_enabled(some, egui::Button::image_and_text(crate::icons::image("circle-dot", 14.0, t.accent), tl!("Add concept"))).clicked() {
                actions.push(Action::AddConcept);
            }
        });
        hint(ui, t, tl!("For a snippet, select text in a document and choose Add to map, or right-click a picture."));
    });
    if app.maps.picked.is_none() {
        hint(ui, t, tl!("Click a node or a connection to see it here. Drag from a node's dot onto another node to connect them."));
        ui.add_space(10.0);
    }
    if app.ai_service.is_some() && app.maps.review.is_none() {
        let mut inner = Vec::new();
        section(ui, t, "sparkles", t.accent, tl!("Ask the assistant"), |ui| ask_ui(ui, t, app, &mut inner));
        actions.append(&mut inner);
    }
    section(ui, t, "filter", t.text_muted, tl!("Show connections"), |ui| {
        ui.horizontal_wrapped(|ui| {
            for s in STRENGTHS {
                let label = crate::i18n::fmt(tl!("{s} and up"), &[("s", strength_name(s))]);
                ui.selectable_value(&mut app.maps.min_strength, s, label);
            }
        });
        egui::CollapsingHeader::new(tl!("Kinds of connection")).id_salt("map-kinds").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(crate::widgets::line(&mut app.maps.kind_draft).hint_text(tl!("A kind of your own")).desired_width(140.0).char_limit(64));
                ui.add(crate::widgets::checkbox(&mut app.maps.kind_directed, tl!("Has a direction")));
            });
            if ui.add_enabled(!app.maps.kind_draft.trim().is_empty(), egui::Button::new(tl!("Add kind"))).clicked() {
                actions.push(Action::AddKind);
            }
            for k in app.maps.graph.kinds() {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 10.0), egui::Sense::hover());
                    ui.painter().line_segment([rect.left_center(), rect.right_center()], egui::Stroke::new(2.5, colour(k.colour)));
                    ui.label(egui::RichText::new(tl!(&k.name)).small());
                });
            }
        });
    });
}

/// A node's Edit and Remove buttons, in `place`.
fn node_buttons(ui: &mut egui::Ui, t: &Tokens, id: NodeId, place: EditIn, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::image_and_text(crate::icons::image("pencil", 13.0, t.icon), tl!("Edit")))
            .on_hover_text(tl!("Edit what the node says"))
            .clicked()
        {
            actions.push(Action::Edit(id, place));
        }
        if ui
            .add(egui::Button::image_and_text(crate::icons::image("trash-2", 13.0, t.icon), tl!("Remove node")))
            .on_hover_text(tl!("Remove the node and its connections (Undo brings it back)"))
            .clicked()
        {
            actions.push(Action::Remove(Picked::Node(id)));
        }
    });
}

/// The editor of the node being edited: what it says, Save and Cancel. ⌘↩ (and ↩ for a concept's
/// one-line name) saves, Escape cancels.
pub(crate) fn editor_ui(ui: &mut egui::Ui, t: &Tokens, maps: &mut MapState, actions: &mut Vec<Action>) {
    let Some(e) = maps.editing.as_mut() else { return };
    let Some(node) = maps.graph.node(e.id) else {
        maps.editing = None;
        return;
    };
    let kind = board::Kind::of(&node.content);
    let look = board::look(t, kind);
    ui.horizontal(|ui| {
        ui.add(crate::icons::image("pencil", 13.0, look.tint));
        let title = crate::i18n::fmt(tl!("Edit {kind}"), &[("kind", &kind.name().to_lowercase())]);
        ui.label(egui::RichText::new(title).font(theme::semibold(12.5)).color(look.tint));
    });
    let one_line = kind == board::Kind::Concept;
    let (limit, hint_text) = match kind {
        board::Kind::Concept => (pdfcraft_graph::MAX_LABEL_BYTES, tl!("Its name")),
        board::Kind::Picture => (pdfcraft_graph::MAX_TEXT_BYTES, tl!("What the picture shows")),
        _ => (pdfcraft_graph::MAX_TEXT_BYTES, tl!("Its text")),
    };
    let field = if one_line { crate::widgets::line(&mut e.text) } else { crate::widgets::block(&mut e.text).desired_rows(4) };
    let response = ui.add(field.hint_text(hint_text).desired_width(f32::INFINITY).char_limit(limit));
    if std::mem::take(&mut e.focus) {
        response.request_focus();
    }
    let empty = e.text.trim().is_empty() && kind != board::Kind::Picture;
    let enter = one_line && response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let save_key = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)) || enter;
    let cancel_key = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let mut save = false;
    let mut cancel = false;
    ui.horizontal(|ui| {
        let button = egui::Button::new(egui::RichText::new(tl!("Save")).color(egui::Color32::WHITE)).fill(t.accent);
        save = ui.add_enabled(!empty, button).clicked();
        cancel = ui.button(tl!("Cancel")).clicked();
        // A one-line name is saved with Enter; longer text, where Enter is a new line, with ⌘↩.
        let save_with = if one_line {
            crate::i18n::key_name("Enter").to_owned()
        } else {
            crate::commands::shortcut_label(ui.ctx(), pdfcraft_engine::commands::Shortcut::cmd("Enter"))
        };
        let keys = crate::i18n::fmt(tl!("{save} saves · {esc} cancels"), &[("save", &save_with), ("esc", crate::i18n::key_name("Esc"))]);
        ui.label(egui::RichText::new(keys).small().color(t.text_faint));
    });
    if (save || (save_key && !empty)) && !cancel {
        actions.push(Action::SaveEdit);
    } else if cancel || cancel_key {
        actions.push(Action::CancelEdit);
    }
}

fn node_ui(ui: &mut egui::Ui, t: &Tokens, graph: &Graph, id: NodeId, actions: &mut Vec<Action>) {
    let Some(n) = graph.node(id) else { return };
    ui.label(egui::RichText::new(prefix(n.content.text(), 2_000)).size(13.5));
    if n.author == Author::Assistant {
        hint(ui, t, tl!("Added by the assistant"));
    }
    if let Some(a) = &n.anchor {
        let label = crate::i18n::fmt(tl!("Open {source}"), &[("source", &source_label(a))]);
        if ui.button(label).clicked() {
            actions.push(Action::Reveal(id, EditIn::Board));
        }
    }
    let links = graph.edges().iter().filter(|e| e.from == id || e.to == id).count();
    hint(ui, t, &crate::i18n::fmt(tl!("Connections: {n}"), &[("n", &links.to_string())]));
    node_buttons(ui, t, id, EditIn::Board, actions);
}

fn edge_ui(ui: &mut egui::Ui, graph: &Graph, id: EdgeId, actions: &mut Vec<Action>) {
    let Some(e) = graph.edge(id) else { return };
    let (mut kind, mut strength) = (e.kind, e.strength);
    let kind_name = graph.kind(kind).map(|k| k.name.clone()).unwrap_or_default();
    craft_ui::ComboBox::from_id_salt("map-edge-kind").selected_text(tl!(&kind_name)).width(200.0).show_ui(ui, |ui| {
        for k in graph.kinds() {
            ui.selectable_value(&mut kind, k.id, tl!(&k.name));
        }
    });
    craft_ui::ComboBox::from_id_salt("map-edge-strength").selected_text(strength_name(strength)).width(200.0).show_ui(ui, |ui| {
        for s in STRENGTHS {
            ui.selectable_value(&mut strength, s, strength_name(s));
        }
    });
    if kind != e.kind || strength != e.strength {
        actions.push(Action::SetEdge(id, kind, strength));
    }
    if ui.button(tl!("Remove connection")).clicked() {
        actions.push(Action::Remove(Picked::Edge(id)));
    }
}

fn ask_ui(ui: &mut egui::Ui, t: &Tokens, app: &mut PdfCraftApp, actions: &mut Vec<Action>) {
    if app.maps.is_running() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tl!("Working on the map…"));
        });
        if ui.button(tl!("Stop")).clicked() {
            actions.push(Action::StopAsk);
        }
        return;
    }
    if !app.assistant.prefs.enabled {
        hint(ui, t, tl!("The AI assistant is off. Turn it on in Preferences ▸ AI assistant."));
        return;
    }
    ui.add(
        crate::widgets::block(&mut app.maps.request)
            .hint_text(tl!("What should change? For example: connect the selected node to related ideas"))
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .char_limit(8192),
    );
    for preset in [
        tl!("Extract the key concepts from this page and connect them."),
        tl!("Connect the selected node to related ideas."),
        tl!("What is missing from this map?"),
    ] {
        if ui.small_button(preset).clicked() {
            app.maps.request = preset.to_owned();
        }
    }
    if app.maps.picked.is_some_and(|p| matches!(p, Picked::Node(_))) {
        hint(ui, t, tl!("Only the selected node and what is within two connections of it are sent."));
    }
    if ui.add_enabled(!app.maps.request.trim().is_empty(), egui::Button::new(tl!("Propose changes"))).clicked() {
        actions.push(Action::Ask);
    }
}

fn review_ui(ui: &mut egui::Ui, t: &Tokens, graph: &Graph, review: &mut Review, actions: &mut Vec<Action>) {
    hint(ui, t, tl!("Nothing has changed yet. Keep the changes you want, then apply them."));
    if !review.summary.trim().is_empty() {
        ui.label(egui::RichText::new(prefix(&review.summary, 4_000)).small());
    }
    for e in &review.errors {
        ui.label(egui::RichText::new(e).small().color(t.text_muted));
    }
    let name = |r: &NodeRef| match r {
        NodeRef::Existing { id } => graph.node(*id).map(|n| capped(n.content.text(), 40)).unwrap_or_default(),
        NodeRef::New { key } => key.clone(),
    };
    for (i, (p, keep)) in review.proposal.changes.iter().zip(review.keep.iter_mut()).enumerate() {
        let what = match &p.change {
            Change::AddNode { key, content, anchor, .. } => {
                let source = anchor.as_ref().map(|a| format!(" ({})", source_label(a))).unwrap_or_default();
                crate::i18n::fmt(tl!("Add {key}: {text}{source}"), &[("key", key), ("text", &capped(content.text(), 80)), ("source", &source)])
            }
            Change::AddEdge { from, to, kind, strength, .. } => crate::i18n::fmt(
                tl!("Connect {from} → {to}: {kind}, {strength}"),
                &[("from", &name(from)), ("to", &name(to)), ("kind", tl!(kind)), ("strength", strength_name(*strength))],
            ),
            Change::SetText { id, text } => crate::i18n::fmt(
                tl!("Reword {node}: {text}"),
                &[("node", &graph.node(*id).map(|n| capped(n.content.text(), 40)).unwrap_or_default()), ("text", &capped(text, 80))],
            ),
            Change::RemoveEdge { id } => {
                let ends =
                    graph.edge(*id).map(|e| (name(&NodeRef::Existing { id: e.from }), name(&NodeRef::Existing { id: e.to }))).unwrap_or_default();
                crate::i18n::fmt(tl!("Remove the connection {from} → {to}"), &[("from", &ends.0), ("to", &ends.1)])
            }
        };
        ui.add(crate::widgets::checkbox(keep, format!("{}. {what}", i + 1)));
        if !p.reason.is_empty() {
            hint(ui, t, prefix(&p.reason, 600));
        }
    }
    let kept = review.keep.iter().filter(|k| **k).count();
    ui.horizontal(|ui| {
        let label = crate::i18n::fmt(tl!("Apply {n} changes"), &[("n", &kept.to_string())]);
        if ui.add_enabled(kept > 0, egui::Button::new(label)).clicked() {
            actions.push(Action::ApplyReview);
        }
        if ui.button(tl!("Discard")).clicked() {
            actions.push(Action::DiscardReview);
        }
    });
}

fn run(app: &mut PdfCraftApp, action: Action) {
    match action {
        Action::Close => app.maps.open = false,
        Action::New => app.new_map(),
        Action::Load(path) => app.load_map(path),
        Action::Undo => app.map_history_step(true),
        Action::Redo => app.map_history_step(false),
        Action::Tidy => {
            app.change_map("Tidy up", |g| {
                layout::tidy(g, None, 300);
                Ok(())
            });
        }
        Action::AddNote | Action::AddConcept => {
            let text = std::mem::take(&mut app.maps.note_draft).trim().to_owned();
            let content = match action {
                Action::AddNote => Content::Note { text: capped(&text, pdfcraft_graph::MAX_TEXT_BYTES) },
                _ => Content::Concept { label: capped(&text, pdfcraft_graph::MAX_LABEL_BYTES) },
            };
            let at = app.free_spot();
            app.change_map("Add node", |g| g.add_node(content, None, at, Author::User).map(|_| ()).map_err(|e| e.to_string()));
        }
        Action::AddKind => {
            let name = std::mem::take(&mut app.maps.kind_draft).trim().to_owned();
            let directed = app.maps.kind_directed;
            let mut added = None;
            app.change_map("Add kind of connection", |g| {
                let colour = g.next_kind_colour();
                added = Some(g.add_kind(&name, directed, Line::Solid, colour).map_err(|e| e.to_string())?);
                Ok(())
            });
            // New connections take the kind just added.
            if let Some(id) = added {
                app.maps.new_kind = id;
            }
        }
        // A drag is one undo step, saved when it ends.
        Action::DragStart => {
            let before = app.maps.graph.clone();
            app.maps.history.record("Move node", before);
        }
        Action::Drag(id, at) => {
            if app.maps.graph.move_node(id, at).is_ok() {
                app.maps.moved = true;
            }
        }
        Action::DragEnd => {
            if std::mem::take(&mut app.maps.moved) {
                app.save_map();
            }
        }
        Action::Connect(from, to) => {
            let (kind, strength) = (app.maps.new_kind, app.maps.new_strength);
            app.change_map("Connect", |g| g.add_edge(from, to, kind, strength, None, Author::User).map(|_| ()).map_err(|e| e.to_string()));
        }
        Action::Remove(Picked::Node(id)) => {
            app.maps.picked = None;
            if app.maps.editing.as_ref().is_some_and(|e| e.id == id) {
                app.maps.editing = None;
            }
            app.change_map("Remove node", |g| g.remove_node(id).map_err(|e| e.to_string()));
        }
        Action::Edit(id, place) => {
            if let Some(n) = app.maps.graph.node(id) {
                app.maps.editing = Some(Editing { id, text: n.content.text().to_owned(), place, focus: true });
                app.maps.picked = Some(Picked::Node(id));
            }
        }
        Action::SaveEdit => {
            if let Some(e) = app.maps.editing.take() {
                let same = app.maps.graph.node(e.id).is_some_and(|n| n.content.text() == e.text.trim());
                // Refused (empty, too long): the editor stays, with what was typed.
                if !same && !app.change_map("Edit node", |g| g.set_text(e.id, &e.text).map_err(|x| x.to_string())) {
                    app.maps.editing = Some(e);
                }
            }
        }
        Action::CancelEdit => app.maps.editing = None,
        Action::Remove(Picked::Edge(id)) => {
            app.maps.picked = None;
            app.change_map("Remove connection", |g| g.remove_edge(id).map_err(|e| e.to_string()));
        }
        Action::SetEdge(id, kind, strength) => {
            app.change_map("Change connection", |g| {
                let label = g.edge(id).and_then(|e| e.label.clone());
                g.set_edge(id, kind, strength, label).map_err(|e| e.to_string())
            });
        }
        Action::Reveal(id, from) => app.reveal_node(id, from),
        Action::Window(open) => app.set_map_window(open),
        Action::Export(format) => app.export_map(format),
        Action::Ask => app.start_map_edit(),
        Action::StopAsk => {
            if let Some(r) = &app.maps.running {
                r.cancel.store(true, Ordering::Relaxed);
            }
        }
        Action::ApplyReview => app.apply_review(),
        Action::DiscardReview => app.maps.review = None,
        Action::Focus(id) => {
            app.maps.picked = Some(Picked::Node(id));
            app.maps.filter.clear();
        }
        Action::Unfocus => app.maps.picked = None,
        Action::OpenWhole => app.open_map_workspace(),
        Action::AddSelection => {
            if let Some(i) = app.active {
                app.add_selection_to_map(i);
            }
        }
    }
}

/// The document the map's panel sits beside: what its snippets are matched against.
pub(crate) struct Here {
    pub sha256: Option<String>,
    pub path: Option<String>,
    pub page: usize,
    /// The document has a selection that may be copied.
    pub can_add: bool,
}

impl Here {
    fn holds(&self, a: &Anchor) -> bool {
        self.sha256.as_deref() == Some(a.sha256.as_str()) || self.path.as_deref().is_some_and(|p| a.paths.iter().any(|q| q == p))
    }
}

/// Nodes listed to connect to, at most.
const PANEL_MATCHES: usize = 20;

/// The map beside the document, for reading: add the selection, connect the node just added
/// (or chosen) to others without leaving the page, follow its connections, and see what is on
/// the map from this page.
pub(crate) fn panel(ui: &mut egui::Ui, t: &Tokens, maps: &mut MapState, here: &Here, doc: Option<GraphDoc<'_>>) -> Vec<Action> {
    let mut actions = Vec::new();
    if !maps.available() {
        ui.label(egui::RichText::new(tl!("Concept maps need the desktop app")).color(t.text_muted));
        return actions;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(&maps.graph.title).font(theme::semibold(13.0)));
        if ui.small_button(tl!("Whole map")).on_hover_text(tl!("Show the whole map instead of the document")).clicked() {
            actions.push(Action::OpenWhole);
        }
        let undo = maps.history.undo_label().map(|l| crate::i18n::fmt(tl!("Undo {what}"), &[("what", tl!(l))]));
        if let Some(undo) = undo
            && ui.small_button(tl!("Undo")).on_hover_text(undo).clicked()
        {
            actions.push(Action::Undo);
        }
    });
    graph_view::show(ui, t, maps, graph_view::Host::Panel, here, doc, &mut actions);
    let tip = crate::commands::command_tip(ui.ctx(), tl!("Add the selected text to the map ({key})"), "map.add_selection");
    if ui.add_enabled(here.can_add, egui::Button::new(tl!("Add the selection"))).on_hover_text(tip).clicked() {
        actions.push(Action::AddSelection);
    }
    if !here.can_add {
        hint(ui, t, tl!("Select text to add it, or right-click a picture."));
    }
    ui.separator();
    match maps.picked {
        Some(Picked::Node(id)) if maps.graph.node(id).is_some() => focus_ui(ui, t, maps, id, EditIn::Panel, &mut actions),
        _ => hint(ui, t, tl!("Add a snippet, or pick a node below, to connect it to others.")),
    }
    ui.separator();
    heading(ui, tl!("On the map from this page"));
    let mut any = false;
    for n in maps
        .graph
        .nodes()
        .iter()
        .filter(|n| n.anchor.as_ref().is_some_and(|a| here.holds(a) && usize::try_from(a.page).is_ok_and(|p| p == here.page)))
    {
        any = true;
        let picked = maps.picked == Some(Picked::Node(n.id));
        if ui.selectable_label(picked, capped(n.content.text(), 70)).clicked() {
            actions.push(Action::Focus(n.id));
        }
    }
    if !any {
        hint(ui, t, tl!("Nothing yet."));
    }
    actions
}

/// The node to connect from: its text and source, its connections (each opens the other end's
/// source), and the nodes it can be connected to, found by their words.
fn focus_ui(ui: &mut egui::Ui, t: &Tokens, maps: &mut MapState, id: NodeId, place: EditIn, actions: &mut Vec<Action>) {
    let Some(n) = maps.graph.node(id).cloned() else { return };
    ui.horizontal(|ui| {
        heading(ui, tl!("Connect"));
        if ui.small_button(tl!("Done")).on_hover_text(tl!("Stop connecting this node")).clicked() {
            actions.push(Action::Unfocus);
        }
    });
    if maps.editing.as_ref().is_some_and(|e| e.id == id && e.place == place) {
        editor_ui(ui, t, maps, actions);
    } else {
        ui.label(egui::RichText::new(prefix(n.content.text(), 300)).small());
        node_buttons(ui, t, id, place, actions);
    }
    if let Some(a) = &n.anchor
        && ui.small_button(source_label(a)).on_hover_text(tl!("Go to the source")).clicked()
    {
        actions.push(Action::Reveal(id, place));
    }
    let name = |g: &Graph, other: NodeId| g.node(other).map(|o| capped(o.content.text(), 50)).unwrap_or_default();
    for e in maps.graph.edges().iter().filter(|e| e.from == id || e.to == id) {
        let other = if e.from == id { e.to } else { e.from };
        let kind = maps.graph.kind(e.kind).map_or("", |k| tl!(&k.name));
        let arrow = if e.from == id { "→" } else { "←" };
        let label = format!("{arrow} {kind} ({}) · {}", strength_name(e.strength), name(&maps.graph, other));
        let response = ui.selectable_label(false, egui::RichText::new(label).small());
        if response.clicked() {
            actions.push(Action::Focus(other));
            if maps.graph.node(other).is_some_and(|o| o.anchor.is_some()) {
                actions.push(Action::Reveal(other, place));
            }
        }
    }
    ui.horizontal_wrapped(|ui| {
        let kind_name = maps.graph.kind(maps.new_kind).map(|k| k.name.clone()).unwrap_or_default();
        craft_ui::ComboBox::from_id_salt("map-panel-kind").selected_text(tl!(&kind_name)).width(130.0).show_ui(ui, |ui| {
            for k in maps.graph.kinds() {
                ui.selectable_value(&mut maps.new_kind, k.id, tl!(&k.name));
            }
        });
        craft_ui::ComboBox::from_id_salt("map-panel-strength").selected_text(strength_name(maps.new_strength)).width(80.0).show_ui(ui, |ui| {
            for s in STRENGTHS {
                ui.selectable_value(&mut maps.new_strength, s, strength_name(s));
            }
        });
    });
    ui.add(crate::widgets::line(&mut maps.filter).hint_text(tl!("Find a node to connect to")).desired_width(f32::INFINITY).char_limit(200));
    let words: Vec<String> = maps.filter.split_whitespace().map(str::to_lowercase).collect();
    let connected: HashSet<NodeId> =
        maps.graph.edges().iter().filter(|e| e.from == id || e.to == id).map(|e| if e.from == id { e.to } else { e.from }).collect();
    let mut shown = 0;
    for other in maps.graph.nodes().iter().rev() {
        if shown >= PANEL_MATCHES {
            break;
        }
        if other.id == id || connected.contains(&other.id) {
            continue;
        }
        let text = other.content.text().to_lowercase();
        if !words.iter().all(|w| text.contains(w.as_str())) {
            continue;
        }
        shown += 1;
        let label = crate::i18n::fmt(tl!("Connect to: {node}"), &[("node", &capped(other.content.text(), 60))]);
        if ui.selectable_label(false, egui::RichText::new(label).small()).clicked() {
            actions.push(Action::Connect(id, other.id));
        }
    }
    if shown == 0 {
        hint(ui, t, if words.is_empty() { tl!("Nothing else on the map yet.") } else { tl!("No node has those words.") });
    }
}

/// Run what the panel asked for, once the panel is drawn.
pub(crate) fn run_panel(app: &mut PdfCraftApp, actions: Vec<Action>) {
    for action in actions {
        run(app, action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_keep_to_safe_characters() {
        assert_eq!(file_name("Biology: cells/2"), "Biology_ cells_2.pdfmap.json");
        assert_eq!(file_name("  "), "map.pdfmap.json");
        assert_eq!(file_name("../../etc"), "______etc.pdfmap.json");
        assert!(file_name(&"é".repeat(500)).chars().count() <= 80 + EXTENSION.len());
    }
}
