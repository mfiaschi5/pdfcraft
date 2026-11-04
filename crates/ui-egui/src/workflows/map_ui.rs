//! Concept maps in the app (ADR "Concept graph" G2–G3): a workspace that takes the document area,
//! with snippets of text and pictures from PDFs, notes and concepts, linked by connections of
//! several kinds and three strengths.
//!
//! - **Capture.** *Add to map* in the selection bar, *Add Image to Map* in a picture's page menu.
//!   Snippets follow the assistant's rules: a document that forbids copying gives nothing, and
//!   text under unapplied redaction marks is left out.
//! - **Navigate.** A node's source button (or a double-click) opens its document at the page.
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
use crate::theme::{self, Tokens};

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
    /// A kind of connection of the user's own, being named.
    pub kind_draft: String,
    pub kind_directed: bool,
    pub request: String,
    pub review: Option<Review>,
    running: Option<Running>,
    /// Where maps are kept: [`maps_dir`] in the desktop app, a folder of their own in tests, and
    /// none on the web, where maps aren't offered.
    pub dir: Option<PathBuf>,
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
            kind_draft: String::new(),
            kind_directed: true,
            request: String::new(),
            review: None,
            running: None,
            dir: None,
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

    fn added_to_map(&mut self) {
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
        if self
            .change_map("Add snippet", |g| g.add_node(Content::Text { text }, Some(anchor), at, Author::User).map(|_| ()).map_err(|e| e.to_string()))
        {
            self.added_to_map();
        }
    }

    /// A picture's page menu: Add Image to Map. The picture's file, when copying is allowed.
    pub(crate) fn add_image_to_map(&mut self, index: usize, page: usize, image: usize) {
        let Some(id) = self.views.get(index).map(|v| v.id) else { return };
        let Some(doc) = self.session.get(id) else { return };
        let bytes = match doc.page_image_file(page, image) {
            Ok((_, bytes)) => bytes,
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
        if self.change_map("Add picture", |g| g.add_node(content, Some(anchor), at, Author::User).map(|_| ()).map_err(|e| e.to_string())) {
            self.added_to_map();
        }
    }

    /// Open a node's source: the open document with its bytes, else the file at its last path.
    fn open_anchor(&mut self, anchor: &Anchor) {
        let Some(sha) = Sha256::from_hex(&anchor.sha256) else {
            self.notify_tr("This snippet's document isn't known.");
            return;
        };
        let page = usize::try_from(anchor.page).unwrap_or(usize::MAX);
        if let Some(i) = self.views.iter().position(|v| self.session.document_sha256(v.id) == Some(sha)) {
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
        let Some(key) = service.key() else {
            self.notify_tr("No API key yet: add one in Preferences ▸ AI assistant.");
            return;
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
        let transport = service.transport.clone();
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
        if ctx.egui_wants_keyboard_input() {
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
enum Export {
    Canvas,
    GraphMl,
}

/// What the workspace asks the app to do, after drawing.
enum Action {
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
    OpenSource(Anchor),
    Export(Export),
    Ask,
    StopAsk,
    ApplyReview,
    DiscardReview,
}

/// The map workspace, in the document area.
pub(crate) fn workspace(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let mut actions: Vec<Action> = Vec::new();
    egui::Panel::top("map-toolbar")
        .frame(egui::Frame::NONE.fill(t.panel).inner_margin(egui::Margin::symmetric(10, 6)))
        .show(ui, |ui| toolbar(ui, &t, app, &mut actions));
    egui::Panel::right("map-side")
        .resizable(false)
        .exact_size(290.0)
        .frame(egui::Frame::NONE.fill(t.panel).inner_margin(egui::Margin::symmetric(10, 6)))
        .show(ui, |ui| egui::ScrollArea::vertical().show(ui, |ui| side(ui, &t, app, &mut actions)));
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.pasteboard)).show(ui, |ui| canvas(ui, &t, &mut app.maps, &mut actions));
    for action in actions {
        run(app, action);
    }
}

fn toolbar(ui: &mut egui::Ui, t: &Tokens, app: &mut PdfCraftApp, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        if ui.button(tl!("Back to the document")).clicked() {
            actions.push(Action::Close);
        }
        ui.separator();
        let current = app.maps.graph.title.clone();
        craft_ui::ComboBox::from_id_salt("map-file").selected_text(current).width(180.0).show_ui(ui, |ui| {
            for path in app.maps.files() {
                let name = path.file_name().map(|n| n.to_string_lossy().trim_end_matches(EXTENSION).to_owned()).unwrap_or_default();
                if ui.selectable_label(app.maps.file.as_ref() == Some(&path), name).clicked() {
                    actions.push(Action::Load(path.clone()));
                }
            }
        });
        if ui.button(tl!("New map")).clicked() {
            actions.push(Action::New);
        }
        ui.separator();
        let undo = app.maps.history.undo_label().map(|l| crate::i18n::fmt(tl!("Undo {what}"), &[("what", tl!(l))]));
        if ui.add_enabled(undo.is_some(), egui::Button::new(tl!("Undo"))).on_hover_text(undo.unwrap_or_default()).clicked() {
            actions.push(Action::Undo);
        }
        if ui.add_enabled(app.maps.history.redo_label().is_some(), egui::Button::new(tl!("Redo"))).clicked() {
            actions.push(Action::Redo);
        }
        if ui.add_enabled(app.maps.graph.nodes().len() > 1, egui::Button::new(tl!("Tidy up"))).clicked() {
            actions.push(Action::Tidy);
        }
        ui.separator();
        ui.label(egui::RichText::new(tl!("New connections:")).color(t.text_muted));
        let kind_name = app.maps.graph.kind(app.maps.new_kind).map(|k| k.name.clone()).unwrap_or_default();
        craft_ui::ComboBox::from_id_salt("map-kind").selected_text(tl!(&kind_name)).width(150.0).show_ui(ui, |ui| {
            for k in app.maps.graph.kinds() {
                ui.selectable_value(&mut app.maps.new_kind, k.id, tl!(&k.name));
            }
        });
        craft_ui::ComboBox::from_id_salt("map-strength").selected_text(strength_name(app.maps.new_strength)).width(90.0).show_ui(ui, |ui| {
            for s in STRENGTHS {
                ui.selectable_value(&mut app.maps.new_strength, s, strength_name(s));
            }
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
    });
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(text).font(theme::semibold(13.0)));
}

fn hint(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.label(egui::RichText::new(text).small().color(t.text_faint));
}

fn side(ui: &mut egui::Ui, t: &Tokens, app: &mut PdfCraftApp, actions: &mut Vec<Action>) {
    // The assistant's proposal comes first while it waits: it is what the user decides on now.
    if let Some(review) = &mut app.maps.review {
        heading(ui, tl!("The assistant's proposal"));
        review_ui(ui, t, &app.maps.graph, review, actions);
        ui.separator();
    }
    heading(ui, tl!("Add"));
    ui.add(
        crate::widgets::block(&mut app.maps.note_draft)
            .hint_text(tl!("A note or a concept"))
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .char_limit(4096),
    );
    ui.horizontal(|ui| {
        let some = !app.maps.note_draft.trim().is_empty();
        if ui.add_enabled(some, egui::Button::new(tl!("Add note"))).clicked() {
            actions.push(Action::AddNote);
        }
        if ui.add_enabled(some, egui::Button::new(tl!("Add concept"))).clicked() {
            actions.push(Action::AddConcept);
        }
    });
    hint(ui, t, tl!("For a snippet, select text in a document and choose Add to map, or right-click a picture."));
    ui.separator();
    match app.maps.picked {
        Some(Picked::Node(id)) => node_ui(ui, t, &app.maps.graph, id, actions),
        Some(Picked::Edge(id)) => edge_ui(ui, &app.maps.graph, id, actions),
        None => hint(ui, t, tl!("Click a node or a connection to see it here. Drag from a node's dot onto another node to connect them.")),
    }
    if app.ai_service.is_some() && app.maps.review.is_none() {
        ui.separator();
        heading(ui, tl!("Ask the assistant"));
        ask_ui(ui, t, app, actions);
    }
    ui.separator();
    heading(ui, tl!("Show connections"));
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
                ui.painter().line_segment([rect.left_center(), rect.right_center()], egui::Stroke::new(2.0, colour(k.colour)));
                ui.label(egui::RichText::new(tl!(&k.name)).small());
            });
        }
    });
}

fn node_ui(ui: &mut egui::Ui, t: &Tokens, graph: &Graph, id: NodeId, actions: &mut Vec<Action>) {
    let Some(n) = graph.node(id) else { return };
    heading(ui, tl!("Node"));
    ui.label(prefix(n.content.text(), 2_000));
    if n.author == Author::Assistant {
        hint(ui, t, tl!("Added by the assistant"));
    }
    if let Some(a) = &n.anchor {
        let label = crate::i18n::fmt(tl!("Open {source}"), &[("source", &source_label(a))]);
        if ui.button(label).clicked() {
            actions.push(Action::OpenSource(a.clone()));
        }
    }
    let links = graph.edges().iter().filter(|e| e.from == id || e.to == id).count();
    hint(ui, t, &crate::i18n::fmt(tl!("Connections: {n}"), &[("n", &links.to_string())]));
    if ui.button(tl!("Remove node")).clicked() {
        actions.push(Action::Remove(Picked::Node(id)));
    }
}

fn edge_ui(ui: &mut egui::Ui, graph: &Graph, id: EdgeId, actions: &mut Vec<Action>) {
    let Some(e) = graph.edge(id) else { return };
    heading(ui, tl!("Connection"));
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

/// Where the map's world coordinates land on the screen.
#[derive(Clone, Copy)]
struct View {
    origin: egui::Pos2,
    zoom: f32,
}

impl View {
    fn to_screen(self, p: [f32; 2]) -> egui::Pos2 {
        egui::pos2(p[0] * self.zoom + self.origin.x, p[1] * self.zoom + self.origin.y)
    }
}

/// A node's box on the screen and its laid-out text.
type NodeBox = (NodeId, egui::Rect, Arc<egui::Galley>);

fn canvas(ui: &mut egui::Ui, t: &Tokens, maps: &mut MapState, actions: &mut Vec<Action>) {
    let (rect, background) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    pan_and_zoom(ui, rect, &background, maps);
    let view = View { origin: rect.center() + maps.pan, zoom: maps.zoom };
    let font = egui::FontId::proportional(12.5 * view.zoom.clamp(0.6, 1.6));
    let near: Option<HashSet<NodeId>> = match maps.picked {
        Some(Picked::Node(id)) => Some(maps.graph.neighbourhood(id, 2)),
        _ => None,
    };
    // Each node's box, laid out first: the connections meet them.
    let mut boxes: Vec<NodeBox> = Vec::with_capacity(maps.graph.nodes().len());
    for n in maps.graph.nodes() {
        let text = match &n.content {
            Content::Image { alt, .. } => format!("🖼 {alt}"),
            Content::Concept { label } => label.clone(),
            other => capped(other.text(), 280),
        };
        let galley = painter.layout(text, font.clone(), t.text, ((NODE_WIDTH - 20.0) * view.zoom).max(10.0));
        let source_h = if n.anchor.is_some() { 16.0 * view.zoom } else { 0.0 };
        let size = egui::vec2(NODE_WIDTH * view.zoom, galley.size().y + 16.0 * view.zoom + source_h);
        boxes.push((n.id, egui::Rect::from_center_size(view.to_screen(n.position), size), galley));
    }
    edges(ui, &painter, t, maps, near.as_ref(), &boxes, view.zoom);
    nodes(ui, &painter, t, maps, near.as_ref(), &boxes, view.zoom, actions);
    if maps.graph.nodes().is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            tl!("This map is empty. Add a note, or select text in a document and choose Add to map."),
            egui::FontId::proportional(14.0),
            t.text_faint,
        );
    }
}

/// Drag the background to pan; scroll to zoom about the pointer.
fn pan_and_zoom(ui: &egui::Ui, rect: egui::Rect, background: &egui::Response, maps: &mut MapState) {
    if background.dragged() && maps.connecting.is_none() {
        maps.pan += background.drag_delta();
    }
    if background.clicked() {
        maps.picked = None;
    }
    if !ui.rect_contains_pointer(rect) {
        return;
    }
    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
    if scroll != 0.0 && scroll.is_finite() {
        let before = maps.zoom;
        maps.zoom = (maps.zoom * (1.0 + scroll * 0.002)).clamp(0.2, 3.0);
        // Keep the point under the pointer where it is.
        if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
            let origin = rect.center() + maps.pan;
            maps.pan += (p - origin) * (1.0 - maps.zoom / before);
        }
    }
}

/// The connections, drawn by kind and strength; a click near one's middle picks it.
fn edges(ui: &egui::Ui, painter: &egui::Painter, t: &Tokens, maps: &mut MapState, near: Option<&HashSet<NodeId>>, boxes: &[NodeBox], zoom: f32) {
    let rects: std::collections::HashMap<NodeId, egui::Rect> = boxes.iter().map(|(id, r, _)| (*id, *r)).collect();
    let mut picked = None;
    for e in maps.graph.edges() {
        if e.strength < maps.min_strength {
            continue;
        }
        let (Some(a), Some(b)) = (rects.get(&e.from), rects.get(&e.to)) else { continue };
        let kind = maps.graph.kind(e.kind);
        let mut c = kind.map_or(t.text_faint, |k| colour(k.colour));
        if near.is_some_and(|s| !(s.contains(&e.from) && s.contains(&e.to))) {
            c = c.gamma_multiply(0.25);
        }
        if maps.picked == Some(Picked::Edge(e.id)) {
            c = t.accent;
        }
        let width = match e.strength {
            Strength::Weak => 1.0,
            Strength::Medium => 2.0,
            Strength::Strong => 3.5,
        } * zoom.clamp(0.6, 1.5);
        let stroke = egui::Stroke::new(width, c);
        let (from, to) = (a.center(), b.center());
        match kind.map_or(Line::Solid, |k| k.line) {
            Line::Solid => {
                painter.line_segment([from, to], stroke);
            }
            Line::Dashed => painter.extend(egui::Shape::dashed_line(&[from, to], stroke, 8.0, 5.0)),
            Line::Dotted => painter.extend(egui::Shape::dotted_line(&[from, to], c, 6.0, width.max(1.0))),
        }
        let along = to - from;
        if kind.is_some_and(|k| k.directed) && along.length() > 1.0 {
            // The arrow head where the line meets the target's box.
            let dir = along.normalized();
            let tip = to - dir * (b.width().min(b.height()) * 0.5).min(along.length() * 0.5);
            painter.arrow(tip - dir * 14.0 * zoom, dir * 14.0 * zoom, stroke);
        }
        let hit = egui::Rect::from_center_size(from + along * 0.5, egui::vec2(18.0, 18.0));
        let name = format!("{} ({})", kind.map_or("", |k| tl!(&k.name)), strength_name(e.strength));
        let response = ui.interact(hit, ui.scope_id().with(("map-edge", e.id.0)), egui::Sense::click()).on_hover_text(&name);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
        if response.clicked() {
            picked = Some(Picked::Edge(e.id));
        }
    }
    if picked.is_some() {
        maps.picked = picked;
    }
}

/// The nodes: click to pick, double-click to open the source, drag to move, and drag from the dot
/// on the right edge onto another node to connect them.
#[expect(clippy::too_many_arguments, reason = "the canvas's shared drawing state, passed down once")]
fn nodes(
    ui: &egui::Ui,
    painter: &egui::Painter,
    t: &Tokens,
    maps: &mut MapState,
    near: Option<&HashSet<NodeId>>,
    boxes: &[NodeBox],
    zoom: f32,
    actions: &mut Vec<Action>,
) {
    let pointer = ui.input(|i| i.pointer.latest_pos());
    for (id, r, galley) in boxes {
        let Some(n) = maps.graph.node(*id) else { continue };
        let faded = near.is_some_and(|s| !s.contains(id));
        let fill = if faded { t.card.gamma_multiply(0.6) } else { t.card };
        let border = if maps.picked == Some(Picked::Node(*id)) {
            egui::Stroke::new(2.0, t.accent)
        } else if n.author == Author::Assistant {
            egui::Stroke::new(1.0, t.accent.gamma_multiply(0.6))
        } else {
            egui::Stroke::new(1.0, t.border)
        };
        painter.rect(*r, 6.0, fill, border, egui::StrokeKind::Inside);
        painter.galley(r.min + egui::vec2(10.0, 8.0) * zoom, galley.clone(), if faded { t.text_faint } else { t.text });
        if let Some(a) = &n.anchor {
            let at = egui::pos2(r.min.x + 10.0 * zoom, r.max.y - 6.0 * zoom);
            painter.text(at, egui::Align2::LEFT_BOTTOM, source_label(a), egui::FontId::proportional(10.5 * zoom.clamp(0.6, 1.6)), t.text_faint);
        }
        let response =
            ui.interact(*r, ui.scope_id().with(("map-node", id.0)), egui::Sense::click_and_drag()).on_hover_text(prefix(n.content.text(), 600));
        // Screen readers (and tests) find a node by its text.
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, galley.text()));
        if response.clicked() {
            maps.picked = Some(Picked::Node(*id));
        }
        if response.double_clicked()
            && let Some(a) = &n.anchor
        {
            actions.push(Action::OpenSource(a.clone()));
        }
        if maps.connecting.is_none() {
            if response.drag_started() {
                actions.push(Action::DragStart);
            }
            if response.dragged() {
                let d = response.drag_delta() / zoom;
                actions.push(Action::Drag(*id, [n.position[0] + d.x, n.position[1] + d.y]));
            }
            if response.drag_stopped() {
                actions.push(Action::DragEnd);
            }
        }
        let handle = egui::Rect::from_center_size(egui::pos2(r.max.x, r.center().y), egui::vec2(14.0, 14.0));
        painter.circle_filled(handle.center(), 5.0, t.accent);
        let h = ui
            .interact(handle, ui.scope_id().with(("map-handle", id.0)), egui::Sense::drag())
            .on_hover_text(tl!("Drag onto another node to connect them"));
        if h.drag_started() {
            maps.connecting = Some(*id);
        }
        if h.dragged()
            && let Some(p) = pointer
        {
            painter.line_segment([handle.center(), p], egui::Stroke::new(1.5, t.accent));
        }
        if h.drag_stopped() {
            if let Some(to) = pointer.and_then(|p| boxes.iter().find(|(other, rr, _)| other != id && rr.contains(p)).map(|(o, ..)| *o)) {
                actions.push(Action::Connect(*id, to));
            }
            maps.connecting = None;
        }
    }
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
            app.change_map("Remove node", |g| g.remove_node(id).map_err(|e| e.to_string()));
        }
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
        Action::OpenSource(anchor) => app.open_anchor(&anchor),
        Action::Export(format) => app.export_map(format),
        Action::Ask => app.start_map_edit(),
        Action::StopAsk => {
            if let Some(r) = &app.maps.running {
                r.cancel.store(true, Ordering::Relaxed);
            }
        }
        Action::ApplyReview => app.apply_review(),
        Action::DiscardReview => app.maps.review = None,
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
