//! The map as a graph in the panel beside the document: round nodes fitted to the panel's width
//! (a node's size grows with its connections), its connections drawn by kind and strength, and the
//! nodes from the page being read ringed. Hovering a node grows a preview of it out of the circle;
//! clicking one goes to its passage in the document and marks where it starts and ends.
//! Drag to move the graph, pinch or Ctrl/⌘-scroll to zoom, double-click to fit it again.

use std::collections::{HashMap, HashSet};

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use pdfcraft_ai::text::{capped, prefix};
use pdfcraft_graph::{Author, Content, Graph, Line, NodeId, Strength};

use super::{Action, Here, MapState, PanelView, Picked, colour, source_label, strength_name};
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
const PREVIEW_WIDTH: f32 = 300.0;

/// Where each node is drawn in the panel's graph: its centre and radius, fitted to `rect`.
/// Positions come from the map (as arranged in the workspace); nodes that would overlap are moved
/// apart, and while the graph fits the panel (not zoomed or moved) all of it stays inside.
pub(crate) fn layout(graph: &Graph, rect: Rect, view: &PanelView) -> Vec<(NodeId, Pos2, f32)> {
    let nodes = graph.nodes();
    if nodes.is_empty() || !rect.is_positive() {
        return Vec::new();
    }
    let mut degree: HashMap<NodeId, usize> = HashMap::new();
    for e in graph.edges() {
        *degree.entry(e.from).or_default() += 1;
        *degree.entry(e.to).or_default() += 1;
    }
    let n = nodes.len() as f32;
    let base = (rect.width().min(rect.height()) / (n.sqrt() * 5.0 + 4.0)).clamp(4.0, 12.0);
    let grow = view.zoom.sqrt().clamp(0.75, 2.0);
    let radius = |id: NodeId| base * (1.0 + 0.1 * degree.get(&id).copied().unwrap_or(0).min(5) as f32) * grow;
    let largest = nodes.iter().map(|n| radius(n.id)).fold(0.0f32, f32::max);
    // Room around the edge for the largest circle and a label under it.
    let pad = largest + 14.0;
    let inner = rect.shrink2(vec2(pad.min(rect.width() * 0.45), pad.min(rect.height() * 0.45)));
    let (mut min, mut max) = (pos2(f32::INFINITY, f32::INFINITY), pos2(f32::NEG_INFINITY, f32::NEG_INFINITY));
    for node in nodes {
        let p = pos2(finite(node.position[0]), finite(node.position[1]));
        min = min.min(p);
        max = max.max(p);
    }
    let span = max - min;
    let fit_x = if span.x > 1e-3 { inner.width() / span.x } else { f32::INFINITY };
    let fit_y = if span.y > 1e-3 { inner.height() / span.y } else { f32::INFINITY };
    let fit = fit_x.min(fit_y);
    let fit = if fit.is_finite() { fit } else { 1.0 };
    let centre = min + span * 0.5;
    let zoom = view.zoom.clamp(0.2, 8.0);
    let mut out: Vec<(NodeId, Pos2, f32)> = nodes
        .iter()
        .map(|node| {
            let p = pos2(finite(node.position[0]), finite(node.position[1]));
            (node.id, rect.center() + (p - centre) * fit * zoom + view.pan, radius(node.id))
        })
        .collect();
    let fitted = view.zoom == 1.0 && view.pan == Vec2::ZERO;
    if out.len() <= SPREAD_UP_TO {
        spread(&mut out, fitted.then_some(inner));
    }
    out
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

/// The graph, at the top of the panel.
pub(super) fn show(ui: &mut egui::Ui, t: &Tokens, maps: &mut MapState, here: &Here, actions: &mut Vec<Action>) {
    let width = ui.available_width().max(120.0);
    let height = (width * 0.8).clamp(170.0, 440.0);
    let (rect, bg) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
    bg.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Canvas, true, tl!("Map graph")));
    let painter = ui.painter_at(rect);
    painter.rect(rect, 8.0, t.pasteboard, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    if maps.graph.nodes().is_empty() {
        painter.text(rect.center(), Align2::CENTER_CENTER, tl!("Add a snippet to start the map."), egui::FontId::proportional(12.0), t.text_faint);
        return;
    }
    // Move and zoom; a double-click fits it again.
    if bg.dragged() {
        maps.panel_view.pan += bg.drag_delta();
    }
    if bg.double_clicked() {
        actions.push(Action::FitGraph);
    }
    if bg.hovered() {
        let factor = ui.input(|i| i.zoom_delta());
        if (factor - 1.0).abs() > 1e-4 {
            let old = maps.panel_view.zoom;
            let new = (old * factor).clamp(0.5, 6.0);
            // Zoom about the pointer: the point under it stays there.
            if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                let from_centre = p - rect.center() - maps.panel_view.pan;
                maps.panel_view.pan -= from_centre * (new / old - 1.0);
            }
            maps.panel_view.zoom = new;
        }
    }
    let placed = layout(&maps.graph, rect, &maps.panel_view);
    let at: HashMap<NodeId, (Pos2, f32)> = placed.iter().map(|(id, p, r)| (*id, (*p, *r))).collect();
    edges(&painter, t, maps, &at);
    let on_page: HashSet<NodeId> = maps
        .graph
        .nodes()
        .iter()
        .filter(|n| n.anchor.as_ref().is_some_and(|a| here.holds(a) && usize::try_from(a.page).is_ok_and(|p| p == here.page)))
        .map(|n| n.id)
        .collect();
    let mut hovered: Option<NodeId> = None;
    let few = placed.len() <= LABEL_ALL_UP_TO;
    let mut labels: Vec<(Pos2, String, bool)> = Vec::new();
    for (id, c, r) in &placed {
        let Some(node) = maps.graph.node(*id) else { continue };
        if !rect.expand(*r).contains(*c) {
            continue;
        }
        let (fill, outline) = style(t, &node.content, node.author);
        painter.circle(*c, *r, fill, outline);
        let picked = maps.picked == Some(Picked::Node(*id));
        if on_page.contains(id) {
            painter.circle_stroke(*c, r + 3.0, Stroke::new(1.5, t.accent));
        }
        if picked {
            painter.circle_stroke(*c, r + 5.0, Stroke::new(2.5, t.accent));
        }
        let hit = Rect::from_center_size(*c, Vec2::splat(r * 2.0 + 4.0)).intersect(rect);
        let response = ui.interact(hit, ui.scope_id().with(("map-graph-node", id.0)), Sense::click());
        let name = capped(node.content.text(), 80);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
        let response = theme::hand(response);
        if response.hovered() {
            hovered = Some(*id);
        }
        if response.clicked() {
            actions.push(Action::Reveal(*id));
        }
        if few || picked || on_page.contains(id) || hovered == Some(*id) {
            labels.push((*c + vec2(0.0, r + 3.0), short_label(node.content.text(), LABEL_CHARS), hovered == Some(*id) || picked));
        }
    }
    // Over every circle, so a neighbour never covers a name, and inside the graph's frame.
    for (at, label, strong) in labels {
        let colour = if strong { t.text } else { t.text_muted };
        let galley = painter.layout_no_wrap(label, egui::FontId::proportional(10.5), colour);
        let r = Rect::from_center_size(at + vec2(0.0, galley.size().y * 0.5), galley.size() + vec2(6.0, 2.0));
        let r = inside(r, rect.shrink(3.0));
        painter.rect_filled(r, 3.0, t.pasteboard.gamma_multiply(0.85));
        painter.galley(r.min + vec2(3.0, 1.0), galley, colour);
    }
    // The previews grow out of their node, and shrink back when the pointer leaves.
    for (id, c, r) in &placed {
        let grown = ui.ctx().animate_bool_with_time(ui.scope_id().with(("map-graph-preview", id.0)), hovered == Some(*id), GROW);
        if grown > 0.01 {
            preview(ui, t, &maps.graph, (*id, *c, *r), rect, grown);
        }
    }
    ui.horizontal(|ui| {
        let tip = tl!("Drag to move the graph, pinch or Ctrl-scroll to zoom, double-click to fit it again");
        ui.label(egui::RichText::new(tl!("Click a node to go to its passage.")).small().color(t.text_faint)).on_hover_text(tip);
        if maps.panel_view != PanelView::default() && ui.small_button(tl!("Fit")).clicked() {
            actions.push(Action::FitGraph);
        }
    });
}

/// The connections, from circle edge to circle edge, coloured by kind and as thick as they are
/// strong; arrows for the kinds that have a direction.
fn edges(painter: &egui::Painter, t: &Tokens, maps: &MapState, at: &HashMap<NodeId, (Pos2, f32)>) {
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
        let (from, to) = (a + dir * ra, b - dir * rb);
        let kind = maps.graph.kind(e.kind);
        let c = kind.map_or(t.text_faint, |k| colour(k.colour));
        let picked = maps.picked == Some(Picked::Edge(e.id));
        let width = match e.strength {
            Strength::Weak => 1.0,
            Strength::Medium => 1.6,
            Strength::Strong => 2.6,
        } + if picked { 1.0 } else { 0.0 };
        let stroke = Stroke::new(width, if picked { t.accent } else { c });
        match kind.map_or(Line::Solid, |k| k.line) {
            Line::Solid => {
                painter.line_segment([from, to], stroke);
            }
            Line::Dashed => painter.extend(egui::Shape::dashed_line(&[from, to], stroke, 6.0, 4.0)),
            Line::Dotted => painter.extend(egui::Shape::dotted_line(&[from, to], stroke.color, 5.0, width.max(1.0))),
        }
        if kind.is_some_and(|k| k.directed) && len > ra + rb + 10.0 {
            painter.arrow(to - dir * 7.0, dir * 7.0, stroke);
        }
    }
}

/// The node's preview, grown `grown` (0 to 1) out of its circle to beside the graph, so it covers
/// the document rather than the graph: what the node holds, where it comes from, and what it is
/// connected to.
fn preview(ui: &egui::Ui, t: &Tokens, graph: &Graph, (id, c, r): (NodeId, Pos2, f32), graph_rect: Rect, grown: f32) {
    let Some(node) = graph.node(id) else { return };
    let ease = 1.0 - (1.0 - grown).powi(3);
    let width = egui::lerp((r * 2.0).max(24.0)..=PREVIEW_WIDTH, ease);
    let screen = ui.ctx().content_rect();
    // Outside the graph on the side with room: left of a panel docked at the window's right.
    let left = graph_rect.min.x - 10.0 - PREVIEW_WIDTH >= screen.min.x;
    let (target, pivot) =
        if left { (pos2(graph_rect.min.x - 10.0, c.y - r), Align2::RIGHT_TOP) } else { (pos2(graph_rect.max.x + 10.0, c.y - r), Align2::LEFT_TOP) };
    // It starts at the node and travels out as it grows.
    let start = pos2(if left { c.x + r } else { c.x - r }, c.y - r);
    let pos = start + (target - start) * ease;
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
                ui.set_max_height(egui::lerp((r * 2.0)..=360.0, ease));
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(kind_name(&node.content)).small().strong().color(t.accent_text));
                    if node.author == Author::Assistant {
                        ui.label(egui::RichText::new(tl!("from the assistant")).small().color(t.text_faint));
                    }
                });
                let text = prefix(node.content.text(), 700);
                match node.content {
                    Content::Text { .. } => ui.label(egui::RichText::new(format!("“{text}”")).italics()),
                    Content::Image { .. } => ui.label(crate::i18n::fmt(tl!("Picture: {alt}"), &[("alt", text)])),
                    _ => ui.label(text),
                };
                if let Some(a) = &node.anchor {
                    ui.label(egui::RichText::new(source_label(a)).small().color(t.text_muted));
                }
                for (shown, e) in graph.edges().iter().filter(|e| e.from == id || e.to == id).enumerate() {
                    if shown == 4 {
                        ui.label(egui::RichText::new("…").small().color(t.text_faint));
                        break;
                    }
                    let other = if e.from == id { e.to } else { e.from };
                    let kind = graph.kind(e.kind).map_or("", |k| tl!(&k.name));
                    let arrow = if e.from == id { "→" } else { "←" };
                    let name = graph.node(other).map(|o| short_label(o.content.text(), 44)).unwrap_or_default();
                    ui.label(egui::RichText::new(format!("{arrow} {kind} ({}) · {name}", strength_name(e.strength))).small());
                }
                let hint = if node.anchor.is_some() { tl!("Click to go to the passage") } else { tl!("Click to pick it, to connect it") };
                ui.label(egui::RichText::new(hint).small().color(t.text_faint));
            });
        });
}

#[cfg(test)]
mod tests;
