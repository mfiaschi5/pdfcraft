//! The map as a graph: in the panel beside the document, and in a window of its own that can go
//! on another screen. Round nodes fitted to the space they have (larger with more connections),
//! connections curved and drawn by kind and strength, and the nodes from the page being read
//! ringed.
//!
//! - **Hover** grows a preview out of the node to beside the graph: the PDF itself, rendered and
//!   cropped to the node's passage with the passage marked, when its document is the one being
//!   read; otherwise its text.
//! - **Click** goes to the node's passage and marks where it starts and ends.
//! - **Drag** moves a node; **Shift-drag** onto another connects them.
//! - **Keyboard**, once the graph has focus (click it, or ⌘⇧E): arrows or Tab move between nodes,
//!   Enter goes to the passage, Space keeps the preview open, C connects from the node (then
//!   Enter on another), 1/2/3 set the strength, K the kind, + − 0 zoom, Escape lets go.
//! - Drag the background to move the graph, pinch or Ctrl-scroll to zoom, double-click to fit.

use std::collections::{HashMap, HashSet};

use egui::{Align2, Color32, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use pdfcraft_ai::text::{capped, prefix};
use pdfcraft_graph::{Anchor, Author, Content, Graph, Line, NodeId, Strength};
use pdfcraft_render::DocInfo;

use super::{Action, Here, MapState, Picked, colour, source_label, strength_name};
use crate::DocView;
use crate::canvas::node_mark;
use crate::theme::{self, Tokens};

/// Nodes past this many are laid out as they are, without moving them apart.
const SPREAD_UP_TO: usize = 200;
/// Labels show under every node in maps up to this size; in larger ones, under the nodes that
/// matter now (hovered, picked, from the page being read).
const LABEL_ALL_UP_TO: usize = 24;
/// A label's length, in characters, before it is cut at a word.
const LABEL_CHARS: usize = 22;
/// How long the preview takes to grow, in seconds.
const GROW: f32 = 0.18;
/// The preview's width once grown.
const PREVIEW_WIDTH: f32 = 380.0;
/// The tallest a preview's picture of the page is.
const PREVIEW_IMAGE_HEIGHT: f32 = 260.0;
/// Room kept around a passage in its preview (view-space points).
const CONTEXT: f32 = 24.0;
/// The narrowest a preview's crop is, as a part of the page's width: wide enough to read a line
/// around the passage, narrow enough to show its words large.
const MIN_CROP: f32 = 0.38;

/// Where the graph is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Host {
    /// The panel beside the document.
    Panel,
    /// The map's own window.
    Window,
}

/// How map positions become screen positions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fit {
    /// The map position at `origin`.
    pub centre: Pos2,
    /// Screen points per map unit.
    pub scale: f32,
    pub origin: Pos2,
}

impl Fit {
    pub(crate) fn to_screen(self, p: [f32; 2]) -> Pos2 {
        self.origin + (pos2(finite(p[0]), finite(p[1])) - self.centre) * self.scale
    }

    pub(crate) fn to_map(self, s: Pos2) -> [f32; 2] {
        let p = self.centre + (s - self.origin) / self.scale.max(1e-6);
        [finite(p.x), finite(p.y)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// A node follows the pointer; the graph keeps the fit it had when the drag began.
    Move { id: NodeId, fit: Fit, grab: Vec2 },
    /// A connection is drawn from a node to the pointer.
    Connect { from: NodeId, to: Pos2 },
}

/// One host's view of the graph.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GraphView {
    pub zoom: f32,
    pub pan: Vec2,
    /// The node the keyboard is on.
    pub cursor: Option<NodeId>,
    /// Connecting from this node by keyboard: Enter on another connects them.
    pub connect_from: Option<NodeId>,
    /// The node whose preview Space keeps open.
    pub pinned: Option<NodeId>,
    /// Take the keyboard on the next draw (⌘⇧E).
    pub focus_now: bool,
    drag: Option<Drag>,
}

impl Default for GraphView {
    fn default() -> Self {
        Self { zoom: 1.0, pan: Vec2::ZERO, cursor: None, connect_from: None, pinned: None, focus_now: false, drag: None }
    }
}

impl GraphView {
    pub(crate) fn fit_again(&mut self) {
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
    }

    fn fitted(&self) -> bool {
        self.zoom == 1.0 && self.pan == Vec2::ZERO
    }
}

/// The nodes as drawn: centre and radius, and how map positions became screen ones.
pub(crate) struct Placed {
    pub nodes: Vec<(NodeId, Pos2, f32)>,
    pub fit: Fit,
}

/// Where each node is drawn in `rect`: positions come from the map (as arranged), fitted to the
/// space; nodes that would overlap are moved apart, and while the graph fits (not zoomed or moved)
/// all of it stays inside. While a node is dragged, the fit stays as it was when the drag began.
pub(crate) fn layout(graph: &Graph, rect: Rect, view: &GraphView) -> Placed {
    let nodes = graph.nodes();
    let fallback = Fit { centre: Pos2::ZERO, scale: 1.0, origin: rect.center() };
    if nodes.is_empty() || !rect.is_positive() {
        return Placed { nodes: Vec::new(), fit: fallback };
    }
    let mut degree: HashMap<NodeId, usize> = HashMap::new();
    for e in graph.edges() {
        *degree.entry(e.from).or_default() += 1;
        *degree.entry(e.to).or_default() += 1;
    }
    let n = nodes.len() as f32;
    let base = (rect.width().min(rect.height()) / (n.sqrt() * 5.0 + 4.0)).clamp(4.0, 16.0);
    let grow = view.zoom.sqrt().clamp(0.75, 2.0);
    let radius = |id: NodeId| base * (1.0 + 0.1 * degree.get(&id).copied().unwrap_or(0).min(5) as f32) * grow;
    let largest = nodes.iter().map(|n| radius(n.id)).fold(0.0f32, f32::max);
    // Room around the edge for the largest circle and a label under it.
    let pad = largest + 14.0;
    let inner = rect.shrink2(vec2(pad.min(rect.width() * 0.45), pad.min(rect.height() * 0.45)));
    let fit = match view.drag {
        Some(Drag::Move { fit, .. }) => fit,
        _ => fit_to(nodes.iter().map(|n| n.position), inner, view),
    };
    let mut out: Vec<(NodeId, Pos2, f32)> = nodes.iter().map(|node| (node.id, fit.to_screen(node.position), radius(node.id))).collect();
    let moving = match view.drag {
        Some(Drag::Move { id, .. }) => Some(id),
        _ => None,
    };
    if out.len() <= SPREAD_UP_TO && moving.is_none() {
        spread(&mut out, view.fitted().then_some(inner));
    }
    Placed { nodes: out, fit }
}

/// The fit that puts `positions` inside `inner`, then zoomed and moved as `view` says.
fn fit_to(positions: impl Iterator<Item = [f32; 2]>, inner: Rect, view: &GraphView) -> Fit {
    let (mut min, mut max) = (pos2(f32::INFINITY, f32::INFINITY), pos2(f32::NEG_INFINITY, f32::NEG_INFINITY));
    for p in positions {
        let p = pos2(finite(p[0]), finite(p[1]));
        min = min.min(p);
        max = max.max(p);
    }
    let span = max - min;
    let fit_x = if span.x > 1e-3 { inner.width() / span.x } else { f32::INFINITY };
    let fit_y = if span.y > 1e-3 { inner.height() / span.y } else { f32::INFINITY };
    let fit = fit_x.min(fit_y);
    let fit = if fit.is_finite() { fit } else { 1.0 };
    Fit { centre: min + span * 0.5, scale: fit * view.zoom.clamp(0.2, 8.0), origin: inner.center() + view.pan }
}

/// A node's first line, cut after a whole word with "…" when it is longer than `max` characters.
pub(super) fn short_label(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= max {
        return line.to_owned();
    }
    let cut: String = line.chars().take(max).collect();
    // A space found by `rfind` is a character boundary.
    let at_word = cut.rfind(' ').filter(|i| *i >= cut.len() / 2).and_then(|i| cut.get(..i)).unwrap_or(&cut);
    format!("{}…", at_word.trim_end())
}

/// `r` moved inside `bounds` (as far as it fits).
fn inside(r: Rect, bounds: Rect) -> Rect {
    let dx = if r.min.x < bounds.min.x {
        bounds.min.x - r.min.x
    } else if r.max.x > bounds.max.x {
        bounds.max.x - r.max.x
    } else {
        0.0
    };
    let dy = if r.max.y > bounds.max.y { bounds.max.y - r.max.y } else { 0.0 };
    r.translate(vec2(dx, dy))
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v.clamp(-1.0e6, 1.0e6) } else { 0.0 }
}

/// Move overlapping circles apart (a few rounds), keeping them inside `keep` when given.
fn spread(nodes: &mut [(NodeId, Pos2, f32)], keep: Option<Rect>) {
    const GAP: f32 = 4.0;
    for _ in 0..8 {
        let mut moved = false;
        for i in 0..nodes.len() {
            for j in i.saturating_add(1)..nodes.len() {
                let (Some(&(_, a, ra)), Some(&(_, b, rb))) = (nodes.get(i), nodes.get(j)) else { continue };
                let want = ra + rb + GAP;
                let d = b - a;
                let len = d.length();
                if len >= want {
                    continue;
                }
                // Circles on the same spot go apart along a direction of their own.
                let dir = if len > 1e-3 { d / len } else { Vec2::angled(j as f32 * 2.399_963) };
                let push = dir * ((want - len) * 0.5);
                if let Some(n) = nodes.get_mut(i) {
                    n.1 -= push;
                }
                if let Some(n) = nodes.get_mut(j) {
                    n.1 += push;
                }
                moved = true;
            }
        }
        if let Some(keep) = keep {
            for n in nodes.iter_mut() {
                n.1 = n.1.clamp(keep.min, keep.max);
            }
        }
        if !moved {
            break;
        }
    }
}

/// The nearest node from `from` in direction `dir` (within 60° of it), nearer ones and ones
/// straighter ahead first.
pub(crate) fn neighbour(nodes: &[(NodeId, Pos2, f32)], from: Pos2, dir: Vec2, except: Option<NodeId>) -> Option<NodeId> {
    let dir = dir.normalized();
    nodes
        .iter()
        .filter(|(id, ..)| Some(*id) != except)
        .filter_map(|(id, p, _)| {
            let d = *p - from;
            let len = d.length();
            if len < 1e-3 {
                return None;
            }
            let cos = d.dot(dir) / len;
            (cos > 0.5).then_some((*id, len * (1.0 + 2.0 * (1.0 - cos))))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

/// The part of a page a preview shows, in view space: the passage's lines with room around them,
/// at least [`MIN_CROP`] of the page wide, no taller than `max_aspect` times its width (the passage's start
/// kept in view), all inside the page.
pub(crate) fn preview_crop(lines: &[Rect], page: Vec2, max_aspect: f32) -> Rect {
    let page_rect = Rect::from_min_size(Pos2::ZERO, page.max(Vec2::splat(1.0)));
    let Some(first) = lines.first().copied() else { return page_rect };
    let passage = lines.iter().fold(first, |a, b| a.union(*b));
    let width = (passage.width() + 2.0 * CONTEXT).max(page_rect.width() * MIN_CROP).min(page_rect.width());
    let mut height = (passage.height() + 2.0 * CONTEXT).min(page_rect.height());
    let mut top = passage.min.y - CONTEXT;
    if height > width * max_aspect {
        height = width * max_aspect;
        // A long passage: its start, with a little above it.
        top = passage.min.y - CONTEXT * 0.5;
    }
    let left = (passage.center().x - width * 0.5).clamp(0.0, (page_rect.width() - width).max(0.0));
    let top = top.clamp(0.0, (page_rect.height() - height).max(0.0));
    Rect::from_min_size(pos2(left, top), vec2(width, height))
}

/// The node's fill and outline: snippets in the accent colour, pictures darker, concepts tinted,
/// notes plain.
fn style(t: &Tokens, content: &Content, author: Author) -> (Color32, Stroke) {
    let outline = if author == Author::Assistant { t.accent.gamma_multiply(0.7) } else { t.border };
    match content {
        Content::Text { .. } => (t.accent, Stroke::new(1.2, t.accent.gamma_multiply(0.6))),
        Content::Image { .. } => (t.accent.gamma_multiply(0.65), Stroke::new(1.2, t.accent)),
        Content::Concept { .. } => (t.accent_soft, Stroke::new(1.5, t.accent)),
        Content::Note { .. } => (t.card, Stroke::new(1.2, outline)),
    }
}

fn kind_name(content: &Content) -> &'static str {
    match content {
        Content::Text { .. } => tl!("Snippet"),
        Content::Image { .. } => tl!("Picture"),
        Content::Concept { .. } => tl!("Concept"),
        Content::Note { .. } => tl!("Note"),
    }
}

/// The document being read, for previews rendered from it.
pub(crate) struct Doc<'a> {
    pub sha256: String,
    pub info: &'a DocInfo,
    pub view: &'a mut DocView,
}

impl MapState {
    pub(crate) fn graph_view(&mut self, host: Host) -> &mut GraphView {
        match host {
            Host::Panel => &mut self.panel_view,
            Host::Window => &mut self.window_view,
        }
    }

    /// The node the keyboard is on in the panel's graph.
    pub fn graph_cursor(&self) -> Option<NodeId> {
        self.panel_view.cursor
    }

    fn graph_view_ref(&self, host: Host) -> &GraphView {
        match host {
            Host::Panel => &self.panel_view,
            Host::Window => &self.window_view,
        }
    }
}

/// The keyboard shortcuts, as the help says them.
fn shortcuts_help(ctx: &egui::Context) -> String {
    let focus = crate::commands::command_shortcut_label(ctx, "map.focus_graph");
    crate::i18n::fmt(
        tl!(
            "Click a node: go to its passage\nDrag: move it · Shift-drag onto another: connect them\nArrows or Tab: move between nodes · Enter: go to the passage\nSpace: keep the preview open · C: connect from the node, then Enter on another\n1, 2, 3: weak, medium, strong · K: next kind of connection\n+ - 0: zoom · Escape: let go · {focus}: go to the graph"
        ),
        &[("focus", &focus)],
    )
}

/// The graph's own toolbar: zoom, fit, the map in a window, and the shortcuts.
fn toolbar(ui: &mut egui::Ui, maps: &mut MapState, host: Host, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let view = maps.graph_view(host);
        if crate::icons::button(ui, "zoom-out", 24.0, false, tl!("Zoom out")).clicked() {
            view.zoom = (view.zoom / 1.25).clamp(0.5, 6.0);
        }
        if crate::icons::button(ui, "zoom-in", 24.0, false, tl!("Zoom in")).clicked() {
            view.zoom = (view.zoom * 1.25).clamp(0.5, 6.0);
        }
        if crate::icons::button(ui, "maximize", 24.0, view.fitted(), tl!("Fit the graph")).clicked() {
            view.fit_again();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            crate::icons::button(ui, "circle-help", 24.0, false, &shortcuts_help(ui.ctx()));
            let (icon, tip) = match host {
                Host::Panel if maps.window => ("app-window", tl!("The map is open in its own window; bring it back")),
                Host::Panel => ("app-window", tl!("Open the map in its own window, to put it on another screen")),
                Host::Window => ("app-window", tl!("Close this window; the map stays beside the document")),
            };
            if crate::icons::button(ui, icon, 24.0, maps.window, tip).clicked() {
                actions.push(Action::Window(!maps.window));
            }
        });
    });
}

/// The graph, with its toolbar, in `host`.
pub(super) fn show(ui: &mut egui::Ui, t: &Tokens, maps: &mut MapState, host: Host, here: &Here, mut doc: Option<Doc<'_>>, actions: &mut Vec<Action>) {
    toolbar(ui, maps, host, actions);
    let width = ui.available_width().max(120.0);
    let height = match host {
        Host::Panel => (width * 0.8).clamp(170.0, 440.0),
        Host::Window => ui.available_height().max(200.0) - 22.0,
    };
    let (rect, bg) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
    let label = match host {
        Host::Panel => tl!("Map graph"),
        Host::Window => tl!("Map graph in its window"),
    };
    bg.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Canvas, true, label));
    let painter = ui.painter_at(rect);
    painter.rect(rect, 8.0, t.pasteboard, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    if maps.graph.nodes().is_empty() {
        painter.text(rect.center(), Align2::CENTER_CENTER, tl!("Add a snippet to start the map."), egui::FontId::proportional(12.0), t.text_faint);
        return;
    }
    // The keyboard: the graph takes it when clicked or asked to (⌘⇧E), and keeps the arrows,
    // Tab and Escape while it has it.
    let view = maps.graph_view(host);
    if std::mem::take(&mut view.focus_now) || bg.clicked() {
        bg.request_focus();
    }
    let focused = bg.has_focus();
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(bg.id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true })
        });
    }
    move_and_zoom(ui, &bg, rect, view);
    let placed = layout(&maps.graph, rect, maps.graph_view_ref(host));
    if focused {
        keys(ui, maps, host, &placed, &bg, actions);
    }
    let on_page: HashSet<NodeId> = maps
        .graph
        .nodes()
        .iter()
        .filter(|n| n.anchor.as_ref().is_some_and(|a| here.holds(a) && usize::try_from(a.page).is_ok_and(|p| p == here.page)))
        .map(|n| n.id)
        .collect();
    let hovered = nodes(ui, &painter, t, maps, host, &placed, &on_page, rect, actions);
    if focused {
        painter.rect_stroke(rect.shrink(1.0), 8.0, Stroke::new(1.5, t.accent.gamma_multiply(0.6)), egui::StrokeKind::Inside);
    }
    // The previews grow out of their node, and shrink back when the pointer leaves.
    let shown = hovered.or(maps.graph_view(host).pinned);
    for (id, c, r) in &placed.nodes {
        let grown = ui.ctx().animate_bool_with_time(ui.scope_id().with(("map-graph-preview", id.0)), shown == Some(*id), GROW);
        if grown > 0.01 {
            preview(ui, t, &maps.graph, (*id, *c, *r), rect, grown, doc.as_mut());
        }
    }
    footer(ui, t, maps);
}

/// Drag the background to move the graph, pinch or Ctrl-scroll to zoom about the pointer,
/// double-click to fit it again.
fn move_and_zoom(ui: &egui::Ui, bg: &egui::Response, rect: Rect, view: &mut GraphView) {
    if bg.dragged() && view.drag.is_none() {
        view.pan += bg.drag_delta();
    }
    if bg.double_clicked() {
        view.fit_again();
    }
    if bg.hovered() || ui.rect_contains_pointer(rect) {
        let factor = ui.input(|i| i.zoom_delta());
        if (factor - 1.0).abs() > 1e-4 {
            let old = view.zoom;
            let new = (old * factor).clamp(0.5, 6.0);
            if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                let from_centre = p - rect.center() - view.pan;
                view.pan -= from_centre * (new / old - 1.0);
            }
            view.zoom = new;
        }
    }
}

/// The keyboard, while the graph has it.
fn keys(ui: &egui::Ui, maps: &mut MapState, host: Host, placed: &Placed, bg: &egui::Response, actions: &mut Vec<Action>) {
    let pressed = |k: Key| ui.input_mut(|i| i.consume_key(Modifiers::NONE, k));
    let kinds: Vec<_> = maps.graph.kinds().iter().map(|k| k.id).collect();
    if pressed(Key::Num1) {
        maps.new_strength = Strength::Weak;
    }
    if pressed(Key::Num2) {
        maps.new_strength = Strength::Medium;
    }
    if pressed(Key::Num3) {
        maps.new_strength = Strength::Strong;
    }
    if pressed(Key::K)
        && let Some(i) = kinds.iter().position(|k| *k == maps.new_kind)
    {
        maps.new_kind = kinds.get(i.saturating_add(1) % kinds.len().max(1)).copied().unwrap_or(maps.new_kind);
    }
    let view = maps.graph_view(host);
    // The order Tab follows: top to bottom, left to right.
    let mut order: Vec<(NodeId, Pos2)> = placed.nodes.iter().map(|(id, p, _)| (*id, *p)).collect();
    order.sort_by(|a, b| a.1.y.total_cmp(&b.1.y).then(a.1.x.total_cmp(&b.1.x)));
    let at = |id: Option<NodeId>| id.and_then(|c| placed.nodes.iter().find(|(n, ..)| *n == c)).map(|(_, p, _)| *p);
    let first = || order.first().map(|(id, _)| *id);
    for (k, dir) in
        [(Key::ArrowLeft, vec2(-1.0, 0.0)), (Key::ArrowRight, vec2(1.0, 0.0)), (Key::ArrowUp, vec2(0.0, -1.0)), (Key::ArrowDown, vec2(0.0, 1.0))]
    {
        if pressed(k) {
            view.cursor = match at(view.cursor) {
                Some(p) => neighbour(&placed.nodes, p, dir, view.cursor).or(view.cursor),
                None => first(),
            };
        }
    }
    let shift_tab = ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Tab));
    if shift_tab || pressed(Key::Tab) {
        let i = view.cursor.and_then(|c| order.iter().position(|(id, _)| *id == c));
        let n = order.len().max(1);
        let next = match i {
            Some(i) if shift_tab => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None => 0,
        };
        view.cursor = order.get(next).map(|(id, _)| *id);
    }
    if pressed(Key::C) {
        view.connect_from = if view.connect_from.is_some() { None } else { view.cursor };
    }
    if pressed(Key::Space) {
        view.pinned = if view.pinned.is_some() { None } else { view.cursor };
    }
    if pressed(Key::Enter)
        && let Some(cursor) = view.cursor
    {
        match view.connect_from.take() {
            Some(from) if from != cursor => actions.push(Action::Connect(from, cursor)),
            Some(_) => {}
            None => actions.push(Action::Reveal(cursor)),
        }
    }
    if pressed(Key::Plus) || pressed(Key::Equals) || ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Equals)) {
        view.zoom = (view.zoom * 1.25).clamp(0.5, 6.0);
    }
    if pressed(Key::Minus) {
        view.zoom = (view.zoom / 1.25).clamp(0.5, 6.0);
    }
    if pressed(Key::Num0) {
        view.fit_again();
    }
    // Escape: stop connecting, else close the kept preview, else let the keyboard go.
    if pressed(Key::Escape) && view.connect_from.take().is_none() && view.pinned.take().is_none() {
        ui.memory_mut(|m| m.surrender_focus(bg.id));
    }
}

/// The node under `p`, if any.
fn node_at(placed: &Placed, p: Pos2) -> Option<NodeId> {
    placed.nodes.iter().rev().find(|(_, c, r)| (p - *c).length() <= r + 3.0).map(|(id, ..)| *id)
}

/// The connections and the nodes, with what the pointer and keyboard do to them. Returns the
/// node under the pointer.
#[expect(clippy::too_many_arguments, reason = "the graph's shared drawing state, passed down once")]
fn nodes(
    ui: &egui::Ui,
    painter: &egui::Painter,
    t: &Tokens,
    maps: &mut MapState,
    host: Host,
    placed: &Placed,
    on_page: &HashSet<NodeId>,
    rect: Rect,
    actions: &mut Vec<Action>,
) -> Option<NodeId> {
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let hovered = pointer.filter(|p| rect.contains(*p)).and_then(|p| node_at(placed, p));
    let view = maps.graph_view(host).clone();
    let picked = match maps.picked {
        Some(Picked::Node(id)) => Some(id),
        _ => None,
    };
    // What the eye is on: its connections show, the rest steps back.
    let focus = hovered.or(view.cursor).or(picked);
    let near: Option<HashSet<NodeId>> = focus.map(|f| {
        let mut s: HashSet<NodeId> = maps.graph.edges().iter().filter(|e| e.from == f || e.to == f).flat_map(|e| [e.from, e.to]).collect();
        s.insert(f);
        s
    });
    let at: HashMap<NodeId, (Pos2, f32)> = placed.nodes.iter().map(|(id, p, r)| (*id, (*p, *r))).collect();
    edges(painter, t, maps, &at, focus);
    let few = placed.nodes.len() <= LABEL_ALL_UP_TO;
    let mut labels: Vec<(Pos2, String, bool)> = Vec::new();
    for (id, c, r) in &placed.nodes {
        let Some(node) = maps.graph.node(*id) else { continue };
        if !rect.expand(*r).contains(*c) {
            continue;
        }
        let grow = ui.ctx().animate_bool_with_time(ui.scope_id().with(("map-graph-hover", id.0)), hovered == Some(*id), 0.1);
        let r_draw = r * (1.0 + 0.18 * grow);
        let faded = near.as_ref().is_some_and(|s| !s.contains(id));
        let (fill, outline) = style(t, &node.content, node.author);
        let fill = if faded { fill.gamma_multiply(0.45) } else { fill };
        // A soft shadow under the circle, a ring for the page being read, a glow when picked.
        painter.circle_filled(*c + vec2(0.0, 1.5), r_draw + 1.0, Color32::from_black_alpha(if faded { 14 } else { 34 }));
        if picked == Some(*id) {
            painter.circle_filled(*c, r_draw + 7.0, t.accent.gamma_multiply(0.18));
        }
        painter.circle(*c, r_draw, fill, outline);
        if matches!(node.content, Content::Text { .. }) && r_draw >= 9.0 {
            painter.text(
                *c + vec2(0.0, r_draw * 0.12),
                Align2::CENTER_CENTER,
                "“",
                egui::FontId::proportional(r_draw * 1.3),
                Color32::from_white_alpha(220),
            );
        }
        if on_page.contains(id) {
            painter.circle_stroke(*c, r_draw + 3.0, Stroke::new(1.5, t.accent));
        }
        if view.cursor == Some(*id) {
            painter.extend(egui::Shape::dashed_line(&circle_points(*c, r_draw + 6.0), Stroke::new(1.5, t.accent_text), 4.0, 3.0));
        }
        if view.connect_from == Some(*id) {
            painter.circle_stroke(*c, r_draw + 5.0, Stroke::new(2.0, t.accent));
        }
        let label = short_label(node.content.text(), LABEL_CHARS);
        node_input(ui, maps, host, placed, (*id, *c, *r), rect, actions);
        if few || picked == Some(*id) || on_page.contains(id) || hovered == Some(*id) || view.cursor == Some(*id) {
            labels.push((*c + vec2(0.0, r_draw + 3.0), label, hovered == Some(*id) || picked == Some(*id)));
        }
    }
    connecting(ui, painter, t, maps, host, placed, pointer);
    // Over every circle, so a neighbour never covers a name, and inside the graph's frame.
    for (at, label, strong) in labels {
        let colour = if strong { t.text } else { t.text_muted };
        let galley = painter.layout_no_wrap(label, egui::FontId::proportional(10.5), colour);
        let r = Rect::from_center_size(at + vec2(0.0, galley.size().y * 0.5), galley.size() + vec2(6.0, 2.0));
        let r = inside(r, rect.shrink(3.0));
        painter.rect_filled(r, 3.0, t.pasteboard.gamma_multiply(0.85));
        painter.galley(r.min + vec2(3.0, 1.0), galley, colour);
    }
    hovered
}

fn circle_points(c: Pos2, r: f32) -> Vec<Pos2> {
    (0..=48).map(|i| c + Vec2::angled(i as f32 / 48.0 * std::f32::consts::TAU) * r).collect()
}

/// What a node does under the pointer: click to go to its passage, drag to move it, Shift-drag
/// onto another to connect them.
fn node_input(
    ui: &egui::Ui,
    maps: &mut MapState,
    host: Host,
    placed: &Placed,
    (id, c, r): (NodeId, Pos2, f32),
    rect: Rect,
    actions: &mut Vec<Action>,
) {
    let Some(node) = maps.graph.node(id) else { return };
    let hit = Rect::from_center_size(c, Vec2::splat(r * 2.0 + 4.0)).intersect(rect);
    let response = ui.interact(hit, ui.scope_id().with(("map-graph-node", id.0)), Sense::click_and_drag());
    let name = capped(node.content.text(), 80);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
    let response = theme::hand(response);
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let view = maps.graph_view(host);
    if response.clicked() {
        view.cursor = Some(id);
        view.focus_now = true;
        actions.push(Action::Reveal(id));
    }
    if response.drag_started()
        && let Some(p) = pointer
    {
        if ui.input(|i| i.modifiers.shift) {
            view.drag = Some(Drag::Connect { from: id, to: p });
        } else {
            view.drag = Some(Drag::Move { id, fit: placed.fit, grab: p - c });
            actions.push(Action::DragStart);
        }
    }
    if response.dragged()
        && let Some(p) = pointer
    {
        match &mut view.drag {
            Some(Drag::Move { id, fit, grab }) => actions.push(Action::Drag(*id, fit.to_map(p - *grab))),
            Some(Drag::Connect { to, .. }) => *to = p,
            None => {}
        }
    }
    if response.drag_stopped() {
        match view.drag.take() {
            Some(Drag::Move { .. }) => actions.push(Action::DragEnd),
            Some(Drag::Connect { from, to }) => {
                if let Some(target) = node_at(placed, to).filter(|t| *t != from) {
                    actions.push(Action::Connect(from, target));
                }
            }
            None => {}
        }
    }
}

/// The connection being made: from its node to the pointer (Shift-drag), or to the keyboard's
/// node (C, then Enter), in the kind it will have.
fn connecting(ui: &egui::Ui, painter: &egui::Painter, t: &Tokens, maps: &mut MapState, host: Host, placed: &Placed, pointer: Option<Pos2>) {
    let view = maps.graph_view(host).clone();
    let (from, to) = match view.drag {
        Some(Drag::Connect { from, to }) => (from, pointer.unwrap_or(to)),
        _ => match (view.connect_from, view.cursor) {
            (Some(from), Some(cursor)) if from != cursor => match placed.nodes.iter().find(|(n, ..)| *n == cursor) {
                Some((_, p, _)) => (from, *p),
                None => return,
            },
            (Some(from), _) => match pointer {
                Some(p) => (from, p),
                None => return,
            },
            _ => return,
        },
    };
    let Some((_, a, _)) = placed.nodes.iter().find(|(n, ..)| *n == from) else { return };
    let colour = maps.graph.kind(maps.new_kind).map_or(t.accent, |k| colour(k.colour));
    painter.extend(egui::Shape::dashed_line(&[*a, to], Stroke::new(2.0, colour), 6.0, 4.0));
    painter.circle_filled(to, 4.0, colour);
    let kind = maps.graph.kind(maps.new_kind).map_or("", |k| tl!(&k.name));
    let label = crate::i18n::fmt(tl!("Connect: {kind}, {strength}"), &[("kind", kind), ("strength", strength_name(maps.new_strength))]);
    let galley = painter.layout_no_wrap(label, egui::FontId::proportional(11.0), t.text);
    let r = Rect::from_min_size(to + vec2(8.0, -8.0 - galley.size().y), galley.size() + vec2(8.0, 4.0));
    let r = inside(r, painter.clip_rect().shrink(3.0));
    painter.rect(r, 4.0, t.card, Stroke::new(1.0, colour), egui::StrokeKind::Inside);
    painter.galley(r.min + vec2(4.0, 2.0), galley, t.text);
    ui.ctx().request_repaint();
}

/// The connections: curved from circle edge to circle edge, coloured by kind and as thick as they
/// are strong; arrows for the kinds that have a direction. Those of `focus` show, others step back.
fn edges(painter: &egui::Painter, t: &Tokens, maps: &MapState, at: &HashMap<NodeId, (Pos2, f32)>, focus: Option<NodeId>) {
    for e in maps.graph.edges() {
        if e.strength < maps.min_strength {
            continue;
        }
        let (Some(&(a, ra)), Some(&(b, rb))) = (at.get(&e.from), at.get(&e.to)) else { continue };
        let along = b - a;
        let len = along.length();
        if len <= ra + rb {
            continue;
        }
        let dir = along / len;
        // A slight bend, alternating side by connection, so lines between the same nodes part.
        let bend = if e.id.0 % 2 == 0 { 0.12 } else { -0.12 };
        let ctrl = a + along * 0.5 + dir.rot90() * len * bend;
        let from = a + (ctrl - a).normalized() * ra;
        let to = b + (ctrl - b).normalized() * rb;
        let kind = maps.graph.kind(e.kind);
        let picked = maps.picked == Some(Picked::Edge(e.id));
        let lit = focus.is_none_or(|f| e.from == f || e.to == f);
        let mut c = kind.map_or(t.text_faint, |k| colour(k.colour));
        if picked {
            c = t.accent;
        } else if !lit {
            c = c.gamma_multiply(0.25);
        }
        let width = match e.strength {
            Strength::Weak => 1.0,
            Strength::Medium => 1.7,
            Strength::Strong => 2.8,
        } + if picked || (focus.is_some() && lit) { 0.6 } else { 0.0 };
        let stroke = Stroke::new(width, c);
        let curve: Vec<Pos2> = (0..=20)
            .map(|i| {
                let s = i as f32 / 20.0;
                let u = 1.0 - s;
                pos2(u * u * from.x + 2.0 * u * s * ctrl.x + s * s * to.x, u * u * from.y + 2.0 * u * s * ctrl.y + s * s * to.y)
            })
            .collect();
        match kind.map_or(Line::Solid, |k| k.line) {
            Line::Solid => {
                painter.add(egui::Shape::line(curve, stroke));
            }
            Line::Dashed => painter.extend(egui::Shape::dashed_line(&curve, stroke, 6.0, 4.0)),
            Line::Dotted => painter.extend(egui::Shape::dotted_line(&curve, stroke.color, 5.0, width.max(1.0))),
        }
        if kind.is_some_and(|k| k.directed) && len > ra + rb + 10.0 {
            let tip_dir = (to - ctrl).normalized();
            painter.arrow(to - tip_dir * 7.0, tip_dir * 7.0, stroke);
        }
    }
}

/// Below the graph: what a click does, what a new connection will be, and the kinds of connection
/// on the map, wrapping to the space there is.
fn footer(ui: &mut egui::Ui, t: &Tokens, maps: &MapState) {
    let kind = maps.graph.kind(maps.new_kind).map_or_else(String::new, |k| tl!(&k.name).to_owned());
    let new = crate::i18n::fmt(tl!("New connections: {kind}, {strength}"), &[("kind", &kind), ("strength", strength_name(maps.new_strength))]);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(10.0, 2.0);
        ui.label(egui::RichText::new(tl!("Click a node to go to its passage.")).small().color(t.text_faint));
        ui.label(egui::RichText::new(new).small().color(t.text_muted))
            .on_hover_text(tl!("K and 1, 2, 3 change them while the graph has the keyboard"));
        let used: HashSet<_> = maps.graph.edges().iter().map(|e| e.kind).collect();
        let font = egui::TextStyle::Small.resolve(ui.style());
        for k in maps.graph.kinds().iter().filter(|k| used.contains(&k.id)).take(10) {
            // Sized before it is placed, so the row wraps before it rather than running past.
            let galley = ui.painter().layout_no_wrap(tl!(&k.name).to_owned(), font.clone(), t.text_muted);
            let (r, _) = ui.allocate_exact_size(vec2(14.0 + galley.size().x, galley.size().y.max(10.0)), Sense::hover());
            let mid = r.min.y + r.height() * 0.5;
            ui.painter().line_segment([pos2(r.min.x, mid), pos2(r.min.x + 10.0, mid)], Stroke::new(2.5, colour(k.colour)));
            ui.painter().galley(pos2(r.min.x + 14.0, r.min.y), galley, t.text_muted);
        }
    });
}

/// The node's preview, grown `grown` (0 to 1) out of its circle to beside the graph, so it covers
/// the document rather than the graph: the passage rendered from the PDF when its document is the
/// one being read, else its text; where it comes from, and what it is connected to.
fn preview(ui: &egui::Ui, t: &Tokens, graph: &Graph, (id, c, r): (NodeId, Pos2, f32), graph_rect: Rect, grown: f32, doc: Option<&mut Doc<'_>>) {
    let Some(node) = graph.node(id) else { return };
    let ease = 1.0 - (1.0 - grown).powi(3);
    let width = egui::lerp((r * 2.0).max(24.0)..=PREVIEW_WIDTH, ease);
    let screen = ui.ctx().content_rect();
    // Outside the graph on the side with room: left of a panel docked at the window's right.
    let left = graph_rect.min.x - 10.0 - PREVIEW_WIDTH >= screen.min.x;
    let right = graph_rect.max.x + 10.0 + PREVIEW_WIDTH <= screen.max.x;
    let (target, pivot) = if left {
        (pos2(graph_rect.min.x - 10.0, c.y - r), Align2::RIGHT_TOP)
    } else if right {
        (pos2(graph_rect.max.x + 10.0, c.y - r), Align2::LEFT_TOP)
    } else {
        // No room beside the graph (its own window filling the screen): over it, beside the node.
        (pos2(c.x + r + 10.0, c.y - r), Align2::LEFT_TOP)
    };
    // It starts at the node and travels out as it grows.
    let start = pos2(if pivot == Align2::RIGHT_TOP { c.x + r } else { c.x - r }, c.y - r);
    let pos = start + (target - start) * ease;
    let page = node.anchor.as_ref().and_then(|a| doc.filter(|d| d.sha256 == a.sha256).map(|d| (a, d)));
    egui::Area::new(ui.scope_id().with(("map-graph-preview-area", id.0)))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos)
        .pivot(pivot)
        .interactable(false)
        .constrain(true)
        .show(ui.ctx(), |ui| {
            ui.set_opacity(ease);
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(width - 16.0);
                ui.set_max_height(egui::lerp((r * 2.0)..=520.0, ease));
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(kind_name(&node.content)).small().strong().color(t.accent_text));
                    if node.author == Author::Assistant {
                        ui.label(egui::RichText::new(tl!("from the assistant")).small().color(t.text_faint));
                    }
                    if let Some(a) = &node.anchor {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(source_label(a)).small().color(t.text_muted));
                        });
                    }
                });
                let text = prefix(node.content.text(), 700);
                match page {
                    Some((anchor, doc)) => rendered(ui, t, anchor, doc, width - 16.0),
                    None => {
                        match node.content {
                            Content::Text { .. } => ui.label(egui::RichText::new(format!("“{text}”")).italics()),
                            Content::Image { .. } => ui.label(crate::i18n::fmt(tl!("Picture: {alt}"), &[("alt", text)])),
                            _ => ui.label(text),
                        };
                        if node.anchor.is_some() {
                            ui.label(egui::RichText::new(tl!("Open its document to see the page here.")).small().color(t.text_faint));
                        }
                    }
                }
                connections(ui, graph, id);
                let hint = if node.anchor.is_some() { tl!("Click to go to the passage") } else { tl!("Click to pick it, to connect it") };
                ui.label(egui::RichText::new(hint).small().color(t.text_faint));
            });
        });
}

/// The node's passage as the PDF draws it: the page rendered and cropped around the passage,
/// marked from where it starts to where it ends.
fn rendered(ui: &mut egui::Ui, t: &Tokens, anchor: &Anchor, doc: &mut Doc<'_>, width: f32) {
    let Some(page) = usize::try_from(anchor.page).ok().filter(|p| *p < doc.info.pages.len()) else {
        ui.label(egui::RichText::new(tl!("That page isn't in the document any more.")).small().color(t.text_faint));
        return;
    };
    doc.view.want_passage_preview(page);
    let Some(p) = doc.info.pages.get(page) else { return };
    let page_size = vec2(p.width, p.height);
    let text = doc.view.page_text(page);
    let lines: Vec<Rect> = match (&text, anchor.rect) {
        (_, Some(r)) => vec![Rect::from_min_max(pos2(r[0], r[1]), pos2(r[2], r[3]))],
        (Some(text), None) => node_mark::locate(text, &anchor.quote)
            .map(|range| node_mark::line_boxes(text, range).into_iter().map(|b| Rect::from_min_max(pos2(b[0], b[1]), pos2(b[2], b[3]))).collect())
            .unwrap_or_default(),
        (None, None) => Vec::new(),
    };
    let crop = preview_crop(&lines, page_size, PREVIEW_IMAGE_HEIGHT / width.max(1.0));
    let scale = width / crop.width().max(1.0);
    let size = vec2(width, (crop.height() * scale).min(PREVIEW_IMAGE_HEIGHT));
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let alt = crate::i18n::fmt(tl!("Page {n} of {doc}, rendered"), &[("n", &(page + 1).to_string()), ("doc", &anchor.title)]);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &alt));
    let painter = ui.painter_at(rect);
    match doc.view.passage_raster(page) {
        Some(tex) => {
            let uv = Rect::from_min_max(
                pos2(crop.min.x / page_size.x, crop.min.y / page_size.y),
                pos2(crop.max.x / page_size.x, crop.max.y / page_size.y),
            );
            painter.rect_filled(rect, 3.0, Color32::WHITE);
            painter.image(tex, rect, uv, Color32::WHITE);
            let to_screen = |b: &Rect| Rect::from_min_max(rect.min + (b.min - crop.min) * scale, rect.min + (b.max - crop.min) * scale);
            let boxes: Vec<Rect> = lines.iter().map(to_screen).collect();
            node_mark::paint_passage(&painter, &boxes, 0.8);
            if text.is_some() && lines.is_empty() {
                ui.label(egui::RichText::new(tl!("The passage wasn't found on the page; the page is shown.")).small().color(t.text_faint));
            }
        }
        None => {
            painter.rect_filled(rect, 3.0, t.card);
            painter.text(rect.center(), Align2::CENTER_CENTER, tl!("Rendering the page…"), egui::FontId::proportional(12.0), t.text_faint);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
    ui.painter().rect_stroke(rect, 3.0, Stroke::new(1.0, t.border), egui::StrokeKind::Outside);
}

/// Up to four of the node's connections.
fn connections(ui: &mut egui::Ui, graph: &Graph, id: NodeId) {
    for (shown, e) in graph.edges().iter().filter(|e| e.from == id || e.to == id).enumerate() {
        if shown == 4 {
            ui.label(egui::RichText::new("…").small());
            break;
        }
        let other = if e.from == id { e.to } else { e.from };
        let kind = graph.kind(e.kind).map_or("", |k| tl!(&k.name));
        let arrow = if e.from == id { "→" } else { "←" };
        let name = graph.node(other).map(|o| short_label(o.content.text(), 44)).unwrap_or_default();
        ui.label(egui::RichText::new(format!("{arrow} {kind} ({}) · {name}", strength_name(e.strength))).small());
    }
}

#[cfg(test)]
mod tests;
