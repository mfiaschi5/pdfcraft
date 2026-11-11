//! The whole map (View ▸ Concept map): the map's nodes as cards on a board.
//!
//! - **Hierarchy.** Cards are told apart by kind with a colour, an icon and a name over each one:
//!   concepts (the map's hubs) tinted in the accent colour with their name large, snippets from
//!   PDFs with a coloured edge and their source under them, notes as warm sticky notes, pictures
//!   with the picture itself. Long text is cut after a few lines.
//! - **Connections** are curved, coloured by kind, as thick as they are strong, with an arrow
//!   where a kind has a direction; their kind shows on them when they are hovered or picked, or
//!   their node is.
//! - **Preview.** Resting the pointer on a card grows its preview beside it, sized to what it
//!   shows and to the board: the passage as the PDF draws it when its document is open, else its
//!   text; the picture large; a note's or concept's whole text when its card cuts it.
//! - **Moving.** The board fits the map when it opens, and glides to a fit (double-click, the
//!   toolbar) or a zoom; drag the board to move it, scroll or pinch to zoom.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::epaint::{QuadraticBezierShape, Shadow};
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align2, Color32, CornerRadius, FontId, Galley, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, TextureHandle, TextureId, Vec2, pos2, vec2};
use pdfcraft_ai::text::{capped, prefix};
use pdfcraft_graph::{Anchor, Author, Content, Line, NodeId, Strength};

use super::graph_view::{Doc, rendered};
use super::{Action, EditIn, MapState, Picked, colour, editor_ui, source_label, strength_name};
use crate::theme::{self, Tokens};

/// How long the pointer rests on a card before its preview shows (seconds), and how long the
/// preview takes to grow.
const PREVIEW_DELAY: f64 = 0.25;
const GROW: f32 = 0.16;
/// How long the view takes to glide to a fit or a zoom (seconds).
const GLIDE: f64 = 0.32;
/// Below this card scale a card shows its kind and one line.
const SMALL_CARD: f32 = 0.62;
/// A picture's texture is no larger than this on a side.
const PICTURE_SIDE: u32 = 1024;
/// Room kept around the map when it is fitted to the board.
const FIT_MARGIN: f32 = 40.0;
/// The most and least a fit zooms.
const FIT_ZOOM: std::ops::RangeInclusive<f32> = 0.3..=1.3;
/// The space between a card and its preview.
const PREVIEW_GAP: f32 = 12.0;

/// What the board keeps between frames.
#[derive(Default)]
pub(crate) struct Board {
    /// The card under the pointer, and since when (seconds).
    hover: Option<(NodeId, f64)>,
    /// The card whose preview shows, kept while it shrinks away.
    shown: Option<NodeId>,
    /// Fit the map to the board on the next frame (opened, loaded, asked).
    pub(crate) fit: bool,
    /// Zoom by this about the board's centre on the next frame (the toolbar).
    pub(crate) zoom_by: Option<f32>,
    glide: Option<Glide>,
    /// Pictures' textures by node, with the picture they were made from (none when it can't be
    /// read, so it isn't tried again).
    pictures: HashMap<NodeId, (usize, Option<TextureHandle>)>,
    /// The side panel shows; `None` leaves it to the board's width.
    pub(crate) side: Option<bool>,
    /// The card being dragged, which stays under the pointer while the others make room.
    dragging: Option<NodeId>,
    /// The view is the last fit (not moved or zoomed since), and the board's size then: a board
    /// that changes size (the side panel, the window) fits the map again.
    fitted: bool,
    size: Option<Vec2>,
}

/// An animated move of the view.
#[derive(Clone, Copy)]
struct Glide {
    from: (Vec2, f32),
    to: (Vec2, f32),
    start: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Concept,
    Snippet,
    Note,
    Picture,
}

impl Kind {
    pub(crate) fn of(c: &Content) -> Self {
        match c {
            Content::Concept { .. } => Self::Concept,
            Content::Text { .. } => Self::Snippet,
            Content::Note { .. } => Self::Note,
            Content::Image { .. } => Self::Picture,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Concept => tl!("Concept"),
            Self::Snippet => tl!("Snippet"),
            Self::Note => tl!("Note"),
            Self::Picture => tl!("Picture"),
        }
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::Concept => "circle-dot",
            Self::Snippet => "message-square-quote",
            Self::Note => "sticky-note",
            Self::Picture => "image",
        }
    }

    /// A card's width at 100%: snippets widest, to be read; concepts narrower, their name large.
    fn width(self) -> f32 {
        match self {
            Self::Concept => 200.0,
            Self::Snippet => 260.0,
            Self::Note | Self::Picture => 220.0,
        }
    }

    /// The lines of text a card shows before cutting it.
    fn rows(self) -> usize {
        match self {
            Self::Concept => 3,
            Self::Snippet => 5,
            Self::Note => 6,
            Self::Picture => 2,
        }
    }

    fn font(self, zoom: f32) -> FontId {
        match self {
            Self::Concept => theme::semibold(15.5 * zoom),
            _ => FontId::proportional(13.0 * zoom),
        }
    }
}

/// A kind's colour, and a card's fill and border made from it.
pub(crate) struct Look {
    pub tint: Color32,
    pub fill: Color32,
    pub border: Color32,
}

pub(crate) fn look(t: &Tokens, kind: Kind) -> Look {
    let dark = t.dark();
    let tint = match kind {
        Kind::Concept => t.accent,
        Kind::Snippet if dark => Color32::from_rgb(0x3C, 0xC4, 0xB0),
        Kind::Snippet => Color32::from_rgb(0x0E, 0x85, 0x77),
        Kind::Note if dark => Color32::from_rgb(0xF2, 0xC2, 0x4B),
        Kind::Note => Color32::from_rgb(0xC9, 0x8A, 0x04),
        Kind::Picture if dark => Color32::from_rgb(0xE8, 0x7C, 0xB8),
        Kind::Picture => Color32::from_rgb(0xB8, 0x3A, 0x83),
    };
    let tinted = match kind {
        Kind::Concept => 0.14,
        Kind::Note if dark => 0.16,
        Kind::Note => 0.2,
        Kind::Snippet | Kind::Picture => 0.05,
    };
    Look { tint, fill: mix(t.card, tint, tinted), border: mix(t.border, tint, if kind == Kind::Concept { 0.75 } else { 0.4 }) }
}

/// `a` taken `amount` (0 to 1) of the way to `b`, opaque.
fn mix(a: Color32, b: Color32, amount: f32) -> Color32 {
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * amount.clamp(0.0, 1.0)).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// How large cards are drawn at `zoom`: they grow and shrink more slowly than the distances
/// between them, so a map zoomed out to fit stays readable and one zoomed in keeps its room.
fn card_scale(zoom: f32) -> f32 {
    zoom.max(0.01).sqrt()
}

/// Where the map's coordinates land on the screen.
#[derive(Clone, Copy)]
struct View {
    origin: Pos2,
    zoom: f32,
}

impl View {
    fn to_screen(self, p: [f32; 2]) -> Pos2 {
        pos2(p[0] * self.zoom + self.origin.x, p[1] * self.zoom + self.origin.y)
    }
}

/// A card laid out for this frame.
struct Card {
    id: NodeId,
    kind: Kind,
    rect: Rect,
    /// Its text, cut after its kind's lines, and whether it was cut.
    body: Arc<Galley>,
    elided: bool,
    /// What screen readers (and tests) call it: its text.
    label: String,
    source: Option<String>,
    picture: Option<(TextureId, Vec2)>,
    assistant: bool,
}

/// The board: the cards, their connections, and a card's preview. `doc` is the document being
/// read, for previews rendered from it.
pub(super) fn canvas(ui: &mut egui::Ui, t: &Tokens, maps: &mut MapState, doc: Option<&mut Doc<'_>>, actions: &mut Vec<Action>) {
    let (rect, background) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    background.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Canvas, true, tl!("The whole map")));
    let painter = ui.painter_at(rect);
    paint_dots(&painter, t, rect, maps);
    let now = ui.input(|i| i.time);
    move_the_view(ui, rect, &background, maps, now);
    let view = View { origin: rect.center() + maps.pan, zoom: maps.zoom };
    let near: Option<HashSet<NodeId>> = match maps.picked {
        Some(Picked::Node(id)) => Some(maps.graph.neighbourhood(id, 2)),
        _ => None,
    };
    let mut cards = lay_out(ui, t, maps, view);
    let mut rects: Vec<(NodeId, Rect)> = cards.iter().map(|c| (c.id, c.rect)).collect();
    spread(&mut rects, maps.board.dragging);
    for (card, (_, rect)) in cards.iter_mut().zip(rects) {
        card.rect = rect;
    }
    let hovered = ui
        .input(|i| i.pointer.latest_pos())
        .filter(|p| rect.contains(*p))
        .and_then(|p| cards.iter().rev().find(|c| c.rect.contains(p)).map(|c| c.id));
    edges(ui, &painter, t, maps, &cards, near.as_ref(), hovered, view.zoom);
    nodes(ui, &painter, t, maps, &cards, near.as_ref(), hovered, view.zoom, actions);
    card_editor(ui, t, maps, &cards, rect, actions);
    if maps.graph.nodes().is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            tl!("This map is empty. Add a note, or select text in a document and choose Add to map."),
            FontId::proportional(14.0),
            t.text_faint,
        );
    } else {
        legend(&painter, t, rect);
    }
    // The preview of the card the pointer rests on, unless something is being dragged.
    let busy = maps.connecting.is_some() || maps.editing.is_some() || ui.input(|i| i.pointer.any_down());
    let resting = match (hovered, maps.board.hover) {
        (Some(id), Some((was, since))) if was == id => Some((id, since)),
        (Some(id), _) => Some((id, now)),
        (None, _) => None,
    };
    maps.board.hover = resting;
    let want = resting.filter(|(_, since)| now - since >= PREVIEW_DELAY && !busy).map(|(id, _)| id);
    if let Some((_, since)) = resting
        && want.is_none()
        && !busy
    {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((PREVIEW_DELAY - (now - since)).max(0.01)));
    }
    if want.is_some() {
        maps.board.shown = want;
    }
    if let Some(id) = maps.board.shown {
        let grown = ui.ctx().animate_bool_with_time(ui.scope_id().with(("map-card-preview", id.0)), want == Some(id), GROW);
        match cards.iter().find(|c| c.id == id) {
            Some(card) if grown > 0.01 => preview(ui, t, maps, card, rect, grown, doc),
            _ if want != Some(id) => maps.board.shown = None,
            _ => {}
        }
    }
    // Pictures of nodes gone from the map are let go.
    let ids: HashSet<NodeId> = maps.graph.nodes().iter().map(|n| n.id).collect();
    maps.board.pictures.retain(|id, _| ids.contains(id));
}

/// A faint grid of dots under the map, moving with it.
fn paint_dots(painter: &egui::Painter, t: &Tokens, rect: Rect, maps: &MapState) {
    let step = 28.0 * maps.zoom;
    if !(step.is_finite() && step >= 10.0) {
        return;
    }
    let origin = rect.center() + maps.pan;
    let first = |o: f32, min: f32| o - ((o - min) / step).floor() * step;
    let colour = if t.dark() { Color32::from_white_alpha(16) } else { Color32::from_black_alpha(22) };
    let mut x = first(origin.x, rect.min.x);
    let mut shapes = Vec::new();
    while x <= rect.max.x && shapes.len() < 20_000 {
        let mut y = first(origin.y, rect.min.y);
        while y <= rect.max.y && shapes.len() < 20_000 {
            shapes.push(Shape::circle_filled(pos2(x, y), 1.0, colour));
            y += step;
        }
        x += step;
    }
    painter.extend(shapes);
}

/// Drag the board to move it, scroll or pinch to zoom about the pointer, double-click to fit;
/// and the glides the toolbar and a fit ask for.
fn move_the_view(ui: &egui::Ui, rect: Rect, background: &egui::Response, maps: &mut MapState, now: f64) {
    if background.dragged() && maps.connecting.is_none() {
        maps.pan += background.drag_delta();
        maps.board.glide = None;
        maps.board.fitted = false;
    }
    if background.clicked() {
        maps.picked = None;
    }
    if background.double_clicked() {
        maps.board.fit = true;
    }
    if ui.rect_contains_pointer(rect) {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let pinch = ui.input(|i| i.zoom_delta());
        let factor = (1.0 + scroll * 0.002) * pinch;
        if (factor - 1.0).abs() > 1e-4 && factor.is_finite() {
            maps.board.glide = None;
            maps.board.fitted = false;
            let before = maps.zoom;
            maps.zoom = (maps.zoom * factor).clamp(0.2, 3.0);
            // Keep the point under the pointer where it is.
            if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                let origin = rect.center() + maps.pan;
                maps.pan += (p - origin) * (1.0 - maps.zoom / before);
            }
        }
    }
    // A board that changed size shows the map fitted again, unless it was moved since its fit.
    let size = rect.size();
    if maps.board.fitted && maps.board.size.is_some_and(|before| (before - size).length() > 24.0) {
        maps.board.fit = true;
    }
    maps.board.size = Some(size);
    if let Some(by) = maps.board.zoom_by.take() {
        maps.board.fitted = false;
        let zoom = (maps.zoom * by).clamp(0.2, 3.0);
        let pan = maps.pan * (zoom / maps.zoom.max(1e-3));
        maps.board.glide = Some(Glide { from: (maps.pan, maps.zoom), to: (pan, zoom), start: now });
    }
    if std::mem::take(&mut maps.board.fit) {
        maps.board.fitted = true;
        let to = fit(maps, rect);
        maps.board.glide = Some(Glide { from: (maps.pan, maps.zoom), to, start: now });
    }
    if let Some(g) = maps.board.glide {
        let k = ((now - g.start) / GLIDE).clamp(0.0, 1.0) as f32;
        let ease = 1.0 - (1.0 - k).powi(3);
        maps.pan = g.from.0 + (g.to.0 - g.from.0) * ease;
        maps.zoom = g.from.1 + (g.to.1 - g.from.1) * ease;
        if k >= 1.0 {
            maps.board.glide = None;
        } else {
            ui.ctx().request_repaint();
        }
    }
}

/// The pan and zoom that show the whole map on the board, its cards whole, with a margin.
fn fit(maps: &MapState, rect: Rect) -> (Vec2, f32) {
    let mut centres = Rect::NOTHING;
    let mut widest: f32 = 0.0;
    for n in maps.graph.nodes() {
        let [x, y] = n.position;
        if x.is_finite() && y.is_finite() {
            centres = centres.union(Rect::from_center_size(pos2(x, y), Vec2::ZERO));
            widest = widest.max(Kind::of(&n.content).width());
        }
    }
    // None placed (the empty rectangle's corners are crossed).
    if centres.min.x > centres.max.x {
        return (Vec2::ZERO, 1.0);
    }
    // Cards are drawn at their own scale (card_scale), so the zoom that leaves room for half a
    // card on each side is found in a few steps.
    let span = centres.size().max(Vec2::splat(1.0));
    let mut zoom: f32 = 1.0;
    for _ in 0..6 {
        let card = vec2(widest, 120.0) * card_scale(zoom);
        let room = (rect.size() - Vec2::splat(2.0 * FIT_MARGIN) - card).max(Vec2::splat(1.0));
        zoom = (room.x / span.x).min(room.y / span.y).clamp(*FIT_ZOOM.start(), *FIT_ZOOM.end());
    }
    (-centres.center().to_vec2() * zoom, zoom)
}

/// Each card's place, size and text for this frame.
fn lay_out(ui: &egui::Ui, t: &Tokens, maps: &mut MapState, view: View) -> Vec<Card> {
    let z = card_scale(view.zoom);
    let small = z < SMALL_CARD;
    let pad = 12.0 * z;
    let mut cards = Vec::with_capacity(maps.graph.nodes().len());
    for n in maps.graph.nodes() {
        let kind = Kind::of(&n.content);
        let label = match &n.content {
            Content::Image { alt, .. } => crate::i18n::fmt(tl!("Picture: {alt}"), &[("alt", alt)]),
            Content::Concept { label } => label.clone(),
            other => capped(other.text(), 280),
        };
        let text = match &n.content {
            Content::Image { alt, .. } => alt.clone(),
            Content::Text { text } => format!("\u{201C}{}\u{201D}", capped(text, 600)),
            other => capped(other.text(), 600),
        };
        let width = kind.width() * z;
        let stripe = if matches!(kind, Kind::Snippet | Kind::Note) { 4.0 * z } else { 0.0 };
        let inner = (width - 2.0 * pad - stripe).max(8.0);
        let mut job = LayoutJob::single_section(
            text,
            TextFormat { font_id: kind.font(z), color: t.text, italics: kind == Kind::Snippet, line_height: None, ..Default::default() },
        );
        job.wrap =
            TextWrapping { max_width: inner, max_rows: if small { 1 } else { kind.rows() }, break_anywhere: false, overflow_character: Some('…') };
        let body = ui.fonts_mut(|f| f.layout_job(job));
        let elided = body.elided;
        let picture = match &n.content {
            Content::Image { image, .. } if !small => picture(ui.ctx(), &mut maps.board, n.id, &image.0),
            _ => None,
        };
        let picture_h = picture.map_or(0.0, |(_, size)| inner * (size.y / size.x.max(1.0)).clamp(0.35, 1.0) + 8.0 * z);
        let source = n.anchor.as_ref().filter(|_| !small).map(source_label);
        let header = 18.0 * z;
        let footer = if source.is_some() { 20.0 * z } else { 0.0 };
        let height = pad + header + 4.0 * z + picture_h + body.size().y + footer + pad;
        let rect = Rect::from_center_size(view.to_screen(n.position), vec2(width, height));
        cards.push(Card { id: n.id, kind, rect, body, elided, label, source, picture, assistant: n.author == Author::Assistant });
    }
    cards
}

/// Most cards moved apart where they overlap, and how many rounds.
const SPREAD_UP_TO: usize = 200;
const SPREAD_ROUNDS: usize = 8;
/// The space kept between cards moved apart.
const SPREAD_GAP: f32 = 10.0;

/// Cards that overlap on the screen moved apart, each half the way along the axis they overlap
/// least on (`fixed`, the one being dragged, stays put): a map laid out at one zoom stays clear at
/// another, as cards grow and shrink more slowly than the distances between them. Only how they
/// are drawn moves; the map keeps its positions.
fn spread(cards: &mut [(NodeId, Rect)], fixed: Option<NodeId>) {
    if cards.len() > SPREAD_UP_TO {
        return;
    }
    for _ in 0..SPREAD_ROUNDS {
        let mut moved = false;
        for i in 0..cards.len() {
            for j in i + 1..cards.len() {
                let (Some(&(ia, a)), Some(&(ib, b))) = (cards.get(i), cards.get(j)) else { continue };
                let (ax, bx) = (a.expand(SPREAD_GAP * 0.5), b.expand(SPREAD_GAP * 0.5));
                if !ax.intersects(bx) {
                    continue;
                }
                let over = ax.intersect(bx).size();
                let push = if over.x < over.y {
                    vec2(over.x * if a.center().x <= b.center().x { -1.0 } else { 1.0 }, 0.0)
                } else {
                    vec2(0.0, over.y * if a.center().y <= b.center().y { -1.0 } else { 1.0 })
                };
                if !push.is_finite() {
                    continue;
                }
                let (fa, fb) = (Some(ia) == fixed, Some(ib) == fixed);
                let (ka, kb) = match (fa, fb) {
                    (true, true) => continue,
                    (true, false) => (0.0, 1.0),
                    (false, true) => (1.0, 0.0),
                    (false, false) => (0.5, 0.5),
                };
                if let Some((_, r)) = cards.get_mut(i) {
                    *r = r.translate(push * ka);
                }
                if let Some((_, r)) = cards.get_mut(j) {
                    *r = r.translate(-push * kb);
                }
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
}

/// A picture node's texture, made once from its picture.
fn picture(ctx: &egui::Context, board: &mut Board, id: NodeId, bytes: &Arc<Vec<u8>>) -> Option<(TextureId, Vec2)> {
    let key = Arc::as_ptr(bytes) as usize;
    if board.pictures.get(&id).is_none_or(|(k, _)| *k != key) {
        let tex = pdfcraft_engine::picture_rgba(bytes, PICTURE_SIDE).ok().map(|(w, h, rgba)| {
            let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            ctx.load_texture(format!("map-picture-{}", id.0), image, egui::TextureOptions::LINEAR)
        });
        board.pictures.insert(id, (key, tex));
    }
    board.pictures.get(&id).and_then(|(_, tex)| tex.as_ref()).map(|tex| (tex.id(), tex.size_vec2()))
}

/// Where a line from inside `r` towards `toward` leaves it.
fn boundary(r: Rect, toward: Pos2) -> Pos2 {
    let c = r.center();
    let d = toward - c;
    if !(d.x.is_finite() && d.y.is_finite()) {
        return c;
    }
    let sx = if d.x.abs() > 1e-4 { r.width() * 0.5 / d.x.abs() } else { f32::INFINITY };
    let sy = if d.y.abs() > 1e-4 { r.height() * 0.5 / d.y.abs() } else { f32::INFINITY };
    let s = sx.min(sy).min(1.0);
    if s.is_finite() { c + d * s } else { c }
}

/// The point at `k` (0 to 1) along the curve from `a` through control `c` to `b`.
fn along(a: Pos2, c: Pos2, b: Pos2, k: f32) -> Pos2 {
    let u = 1.0 - k;
    pos2(u * u * a.x + 2.0 * u * k * c.x + k * k * b.x, u * u * a.y + 2.0 * u * k * c.y + k * k * b.y)
}

/// The connections: curved, by kind and strength, an arrow for a kind with a direction, its kind
/// on it when it or one of its nodes is hovered or picked; a click on its middle picks it.
#[expect(clippy::too_many_arguments, reason = "the board's shared drawing state, passed down once")]
fn edges(
    ui: &egui::Ui,
    painter: &egui::Painter,
    t: &Tokens,
    maps: &mut MapState,
    cards: &[Card],
    near: Option<&HashSet<NodeId>>,
    hovered: Option<NodeId>,
    zoom: f32,
) {
    let rects: HashMap<NodeId, Rect> = cards.iter().map(|c| (c.id, c.rect)).collect();
    let picked_node = match maps.picked {
        Some(Picked::Node(id)) => Some(id),
        _ => None,
    };
    let mut picked = None;
    let mut labels = Vec::new();
    for e in maps.graph.edges() {
        if e.strength < maps.min_strength {
            continue;
        }
        let (Some(ra), Some(rb)) = (rects.get(&e.from), rects.get(&e.to)) else { continue };
        let kind = maps.graph.kind(e.kind);
        let (from, to) = (ra.center(), rb.center());
        let d = to - from;
        if d.length() < 1.0 {
            continue;
        }
        // Bent to one side, the other for the other direction, so two connections between the
        // same cards stay apart.
        let side = if e.from.0 < e.to.0 { 1.0 } else { -1.0 };
        let control = from + d * 0.5 + vec2(-d.y, d.x).normalized() * d.length() * 0.12 * side;
        let (a, b) = (boundary(ra.expand(2.0), control), boundary(rb.expand(2.0), control));
        let touches = |id: Option<NodeId>| id.is_some_and(|n| n == e.from || n == e.to);
        let mut c = kind.map_or(t.text_faint, |k| colour(k.colour));
        if near.is_some_and(|s| !(s.contains(&e.from) && s.contains(&e.to))) {
            c = c.gamma_multiply(0.22);
        }
        let mid = along(a, control, b, 0.5);
        let hit = Rect::from_center_size(mid, vec2(20.0, 20.0));
        let name = format!("{} ({})", kind.map_or("", |k| tl!(&k.name)), strength_name(e.strength));
        let response = ui.interact(hit, ui.scope_id().with(("map-edge", e.id.0)), Sense::click()).on_hover_text(&name);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
        if response.clicked() {
            picked = Some(Picked::Edge(e.id));
        }
        let is_picked = maps.picked == Some(Picked::Edge(e.id));
        if is_picked {
            c = t.accent;
        }
        let lift = if response.hovered() || is_picked { 1.5 } else { 1.0 };
        let width = match e.strength {
            Strength::Weak => 1.2,
            Strength::Medium => 2.2,
            Strength::Strong => 3.6,
        } * zoom.clamp(0.6, 1.4)
            * lift;
        let stroke = Stroke::new(width, c);
        // A kind with a direction ends in an arrow head, the line stopping at its base.
        let directed = kind.is_some_and(|k| k.directed);
        let head = (9.0 + width * 1.6) * zoom.clamp(0.7, 1.3);
        let dir = (b - control).normalized();
        let end = if directed { b - dir * head * 0.8 } else { b };
        let points: Vec<Pos2> = (0..=24).map(|i| along(a, control, end, i as f32 / 24.0)).collect();
        match kind.map_or(Line::Solid, |k| k.line) {
            Line::Solid => {
                painter.add(QuadraticBezierShape::from_points_stroke([a, control, end], false, Color32::TRANSPARENT, stroke));
            }
            Line::Dashed => painter.extend(Shape::dashed_line(&points, stroke, 8.0 * zoom.max(0.5), 5.0 * zoom.max(0.5))),
            Line::Dotted => painter.extend(Shape::dotted_line(&points, c, 6.0 * zoom.max(0.5), (width * 0.6).max(1.0))),
        }
        if directed && dir.is_finite() {
            let normal = vec2(-dir.y, dir.x) * head * 0.45;
            let base = b - dir * head;
            painter.add(Shape::convex_polygon(vec![b, base + normal, base - normal], c, Stroke::NONE));
        }
        if response.hovered() || is_picked || touches(hovered) || touches(picked_node) {
            labels.push((mid, kind.map_or(String::new(), |k| tl!(&k.name).to_owned()), c));
        }
    }
    // The kinds go over every line.
    for (at, text, c) in labels {
        let font = FontId::proportional(11.0 * zoom.clamp(0.8, 1.2));
        let galley = painter.layout_no_wrap(text, font, c);
        let chip = Rect::from_center_size(at, galley.size() + vec2(12.0, 6.0));
        painter.rect(chip, CornerRadius::same(10), t.card, Stroke::new(1.0, c.gamma_multiply(0.7)), StrokeKind::Inside);
        painter.galley(chip.center() - galley.size() * 0.5, galley, c);
    }
    if picked.is_some() {
        maps.picked = picked;
    }
}

/// The cards: click to pick, double-click to open the source, drag to move, and drag from the
/// dot on a card's right edge onto another card to connect them.
#[expect(clippy::too_many_arguments, reason = "the board's shared drawing state, passed down once")]
fn nodes(
    ui: &egui::Ui,
    painter: &egui::Painter,
    t: &Tokens,
    maps: &mut MapState,
    cards: &[Card],
    near: Option<&HashSet<NodeId>>,
    hovered: Option<NodeId>,
    zoom: f32,
    actions: &mut Vec<Action>,
) {
    let pointer = ui.input(|i| i.pointer.latest_pos());
    // The picked and hovered cards go over the others.
    let mut order: Vec<&Card> = cards.iter().collect();
    order.sort_by_key(|c| (maps.picked == Some(Picked::Node(c.id)), hovered == Some(c.id)));
    for card in order {
        let Some(n) = maps.graph.node(card.id) else { continue };
        let response = ui.interact(card.rect, ui.scope_id().with(("map-node", card.id.0)), Sense::click_and_drag());
        // Screen readers (and tests) find a node by its text.
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &card.label));
        let is_picked = maps.picked == Some(Picked::Node(card.id));
        let lift = ui.ctx().animate_bool_with_time(ui.scope_id().with(("map-card-lift", card.id.0)), hovered == Some(card.id) || is_picked, 0.12);
        let faded = near.is_some_and(|s| !s.contains(&card.id));
        paint_card(ui, painter, t, card, lift, is_picked, faded, card_scale(zoom));
        if response.clicked() {
            maps.picked = Some(Picked::Node(card.id));
        }
        // A double-click opens a snippet's or picture's source, and edits a note or a concept.
        if response.double_clicked() {
            match &n.anchor {
                Some(_) => actions.push(Action::Reveal(card.id, EditIn::Board)),
                None => actions.push(Action::Edit(card.id, EditIn::Board)),
            }
        }
        response.context_menu(|ui| card_menu(ui, t, card.id, n.anchor.as_ref(), actions));
        if maps.connecting.is_none() {
            if response.drag_started() {
                maps.board.dragging = Some(card.id);
                actions.push(Action::DragStart);
            }
            if response.dragged() {
                let d = response.drag_delta() / zoom;
                actions.push(Action::Drag(card.id, [n.position[0] + d.x, n.position[1] + d.y]));
            }
            if response.drag_stopped() {
                maps.board.dragging = None;
                actions.push(Action::DragEnd);
            }
        }
        // The dot to connect from: faint, clear when the card is hovered or picked.
        let look = look(t, card.kind);
        let handle = Rect::from_center_size(pos2(card.rect.max.x, card.rect.center().y), vec2(16.0, 16.0));
        let h = ui
            .interact(handle, ui.scope_id().with(("map-handle", card.id.0)), Sense::drag())
            .on_hover_text(tl!("Drag onto another node to connect them"));
        let strong = 0.35 + 0.65 * lift.max(if h.hovered() { 1.0 } else { 0.0 });
        painter.circle(handle.center(), 4.0 + 1.5 * strong, t.card, Stroke::new(2.0, look.tint.gamma_multiply(if faded { 0.3 } else { strong })));
        if h.drag_started() {
            maps.connecting = Some(card.id);
        }
        if h.dragged()
            && let Some(p) = pointer
        {
            painter.add(QuadraticBezierShape::from_points_stroke(
                [handle.center(), pos2((handle.center().x + p.x) * 0.5, handle.center().y), p],
                false,
                Color32::TRANSPARENT,
                Stroke::new(2.0, t.accent),
            ));
            painter.circle_filled(p, 4.0, t.accent);
        }
        if h.drag_stopped() {
            if let Some(to) = pointer.and_then(|p| cards.iter().rev().find(|c| c.id != card.id && c.rect.contains(p)).map(|c| c.id)) {
                actions.push(Action::Connect(card.id, to));
            }
            maps.connecting = None;
        }
    }
}

/// A card's menu: edit it, open its source, remove it.
fn card_menu(ui: &mut egui::Ui, t: &Tokens, id: NodeId, anchor: Option<&Anchor>, actions: &mut Vec<Action>) {
    if ui.add(egui::Button::image_and_text(crate::icons::image("pencil", 14.0, t.icon), tl!("Edit"))).clicked() {
        actions.push(Action::Edit(id, EditIn::Board));
        ui.close();
    }
    if let Some(a) = anchor {
        let label = crate::i18n::fmt(tl!("Open {source}"), &[("source", &source_label(a))]);
        if ui.add(egui::Button::image_and_text(crate::icons::image("file-text", 14.0, t.icon), label)).clicked() {
            actions.push(Action::Reveal(id, EditIn::Board));
            ui.close();
        }
    }
    ui.separator();
    if ui.add(egui::Button::image_and_text(crate::icons::image("trash-2", 14.0, t.icon), tl!("Remove node"))).clicked() {
        actions.push(Action::Remove(Picked::Node(id)));
        ui.close();
    }
}

/// The editor of a card being edited, over the card: at least as wide as a note's card, kept on
/// the board.
fn card_editor(ui: &egui::Ui, t: &Tokens, maps: &mut MapState, cards: &[Card], board: Rect, actions: &mut Vec<Action>) {
    let Some(id) = maps.editing.as_ref().filter(|e| e.place == EditIn::Board).map(|e| e.id) else { return };
    let Some(card) = cards.iter().find(|c| c.id == id) else { return };
    let width = card.rect.width().max(320.0).min(board.width() - 16.0);
    egui::Area::new(ui.scope_id().with(("map-card-editor", id.0)))
        .order(egui::Order::Foreground)
        .fixed_pos(card.rect.min)
        .constrain_to(board.shrink(6.0))
        .show(ui.ctx(), |ui| {
            let look = look(t, card.kind);
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(10)).stroke(Stroke::new(1.5, look.tint)).show(ui, |ui| {
                ui.set_width(width - 20.0);
                editor_ui(ui, t, maps, actions);
            });
        });
}

/// One card: its shadow (deeper as it lifts), its fill and border by kind, its kind's colour on
/// its edge and over it, its text, picture and source.
#[expect(clippy::too_many_arguments, reason = "one card's drawing state")]
fn paint_card(ui: &egui::Ui, painter: &egui::Painter, t: &Tokens, card: &Card, lift: f32, picked: bool, faded: bool, z: f32) {
    let look = look(t, card.kind);
    let fade = |c: Color32| if faded { c.gamma_multiply(0.35) } else { c };
    let r = card.rect;
    let radius = CornerRadius::same((10.0 * z).clamp(4.0, 14.0) as u8);
    let shadow_alpha = if t.dark() { 80.0 } else { 20.0 + 22.0 * lift };
    let shadow = Shadow {
        offset: [0, (2.0 + 5.0 * lift) as i8],
        blur: (8.0 + 14.0 * lift) as u8,
        spread: 0,
        color: Color32::from_black_alpha(if faded { 6 } else { shadow_alpha as u8 }),
    };
    painter.add(shadow.as_shape(r, radius));
    let border = if picked {
        Stroke::new(2.0, t.accent)
    } else {
        Stroke::new(if card.kind == Kind::Concept { 1.5 } else { 1.0 }, fade(mix(look.border, look.tint, lift * 0.6)))
    };
    let fill = if faded { mix(look.fill, t.pasteboard, 0.5) } else { look.fill };
    painter.rect(r, radius, fill, border, StrokeKind::Inside);
    let pad = 12.0 * z;
    let mut x = r.min.x + pad;
    if matches!(card.kind, Kind::Snippet | Kind::Note) {
        let stripe = Rect::from_min_max(r.min, pos2(r.min.x + 4.0 * z, r.max.y));
        painter.rect_filled(stripe, CornerRadius { nw: radius.nw, sw: radius.sw, ne: 0, se: 0 }, fade(look.tint));
        x += 4.0 * z;
    }
    // Its kind, over it.
    let mut y = r.min.y + pad;
    let icon = 13.0 * z;
    crate::icons::paint(ui, Rect::from_min_size(pos2(x, y + 1.0 * z), Vec2::splat(icon)), card.kind.icon(), icon, fade(look.tint));
    let kind_font = theme::semibold(10.5 * z);
    painter.text(
        pos2(x + icon + 5.0 * z, y + icon * 0.5 + 1.0 * z),
        Align2::LEFT_CENTER,
        card.kind.name().to_uppercase(),
        kind_font,
        fade(look.tint),
    );
    if card.assistant {
        let at = Rect::from_min_size(pos2(r.max.x - pad - icon, y + 1.0 * z), Vec2::splat(icon));
        crate::icons::paint(ui, at, "sparkles", icon, fade(t.accent));
    }
    y += 18.0 * z + 4.0 * z;
    if let Some((tex, size)) = card.picture {
        let w = r.max.x - pad - x;
        let h = w * (size.y / size.x.max(1.0)).clamp(0.35, 1.0);
        let area = Rect::from_min_size(pos2(x, y), vec2(w, h));
        // Cropped to the area's shape, from the middle.
        let (sw, sh) = (size.x.max(1.0), size.y.max(1.0));
        let (uw, uh) = if sw / sh > w / h { ((w / h) * sh / sw, 1.0) } else { (1.0, (h / w) * sw / sh) };
        let uv = Rect::from_center_size(pos2(0.5, 0.5), vec2(uw, uh));
        painter.image(tex, area, uv, if faded { Color32::from_white_alpha(90) } else { Color32::WHITE });
        painter.rect_stroke(area, CornerRadius::same(4), Stroke::new(1.0, fade(t.border)), StrokeKind::Inside);
        y += h + 8.0 * z;
    }
    let text = match card.kind {
        Kind::Concept => mix(t.text, look.tint, 0.35),
        _ => t.text,
    };
    painter.galley_with_override_text_color(pos2(x, y), card.body.clone(), fade(text));
    if let Some(source) = &card.source {
        let fy = r.max.y - pad - 7.0 * z;
        let small = 11.0 * z;
        crate::icons::paint(ui, Rect::from_center_size(pos2(x + small * 0.5, fy), Vec2::splat(small)), "file-text", small, fade(t.text_faint));
        painter.text(pos2(x + small + 4.0 * z, fy), Align2::LEFT_CENTER, source, FontId::proportional(10.5 * z), fade(t.text_muted));
    }
}

/// The kinds of card, in the board's corner.
fn legend(painter: &egui::Painter, t: &Tokens, rect: Rect) {
    let font = FontId::proportional(11.5);
    let mut x = rect.min.x + 12.0;
    let y = rect.max.y - 16.0;
    let items: Vec<(Kind, Arc<Galley>)> = [Kind::Concept, Kind::Snippet, Kind::Note, Kind::Picture]
        .into_iter()
        .map(|k| (k, painter.layout_no_wrap(k.name().to_owned(), font.clone(), t.text_muted)))
        .collect();
    let width = items.iter().map(|(_, g)| g.size().x + 30.0).sum::<f32>() + 8.0;
    let back = Rect::from_min_max(pos2(x - 6.0, y - 12.0), pos2(x - 6.0 + width, y + 12.0));
    painter.rect(back, CornerRadius::same(12), t.card.gamma_multiply(0.92), Stroke::new(1.0, t.border), StrokeKind::Inside);
    for (kind, galley) in items {
        let look = look(t, kind);
        painter.circle(pos2(x + 6.0, y), 5.0, look.fill, Stroke::new(1.5, look.tint));
        let w = galley.size().x;
        painter.galley(pos2(x + 16.0, y - galley.size().y * 0.5), galley, t.text_muted);
        x += w + 30.0;
    }
    let hint = tl!("Scroll to zoom · drag the board to move it · double-click to fit");
    let galley = painter.layout_no_wrap(hint.to_owned(), FontId::proportional(11.0), t.text_faint);
    if back.max.x + galley.size().x + 24.0 < rect.max.x {
        painter.galley(pos2(rect.max.x - 12.0 - galley.size().x, y - galley.size().y * 0.5), galley, t.text_faint);
    }
}

/// What a card's preview shows.
enum Show<'a> {
    Passage(&'a Anchor),
    Text(String, bool),
    Picture(TextureId, Vec2, String),
}

/// A card's preview, grown `grown` (0 to 1) beside it on the side with room, sized to what it
/// shows and kept on the board.
fn preview(ui: &egui::Ui, t: &Tokens, maps: &MapState, card: &Card, board: Rect, grown: f32, doc: Option<&mut Doc<'_>>) {
    let Some(n) = maps.graph.node(card.id) else { return };
    let open = |a: &Anchor| doc.as_ref().is_some_and(|d| d.sha256 == a.sha256);
    let show = match (&n.content, &n.anchor) {
        (Content::Text { .. }, Some(a)) if open(a) => Show::Passage(a),
        (Content::Text { text }, _) => Show::Text(prefix(text, 900).to_owned(), true),
        (Content::Image { alt, .. }, _) => match card.picture {
            Some((tex, size)) => Show::Picture(tex, size, alt.clone()),
            None => Show::Text(alt.clone(), false),
        },
        (other, _) if card.elided => Show::Text(prefix(other.text(), 1_500).to_owned(), false),
        _ => return,
    };
    let ease = 1.0 - (1.0 - grown).powi(3);
    let right = board.max.x - card.rect.max.x - PREVIEW_GAP - 12.0;
    let left = card.rect.min.x - board.min.x - PREVIEW_GAP - 12.0;
    let room = right.max(left);
    let tall = (board.height() - 24.0).max(120.0);
    let width = match &show {
        Show::Passage(_) => room.clamp(300.0, 560.0),
        Show::Text(..) => room.clamp(260.0, 440.0),
        Show::Picture(_, size, _) => {
            let scale = (room.min(560.0) / size.x.max(1.0)).min((tall.min(440.0) - 40.0) / size.y.max(1.0)).min(2.0);
            (size.x * scale).max(200.0)
        }
    };
    let on_right = right >= left;
    let x = if on_right { card.rect.max.x + PREVIEW_GAP } else { card.rect.min.x - PREVIEW_GAP - width };
    // It slides out a little from the card as it grows.
    let slide = (1.0 - ease) * 10.0 * if on_right { -1.0 } else { 1.0 };
    let at = pos2(x + slide, card.rect.min.y);
    egui::Area::new(ui.scope_id().with(("map-card-preview-area", card.id.0)))
        .order(egui::Order::Tooltip)
        .fixed_pos(at)
        .interactable(false)
        .constrain_to(board.shrink(6.0))
        .show(ui.ctx(), |ui| {
            ui.set_opacity(ease);
            let look = look(t, card.kind);
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(8)).stroke(Stroke::new(1.0, look.border)).show(ui, |ui| {
                ui.set_width(width - 16.0);
                ui.set_max_height(tall);
                match show {
                    Show::Passage(anchor) => {
                        if let Some(doc) = doc {
                            rendered(ui, t, anchor, doc, width - 16.0, width - 16.0);
                        }
                    }
                    Show::Text(text, quote) => {
                        let text = if quote { format!("\u{201C}{text}\u{201D}") } else { text };
                        egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
                            ui.label(egui::RichText::new(text).size(15.0).color(t.text).italics());
                        });
                    }
                    Show::Picture(tex, size, alt) => {
                        let w = width - 16.0;
                        let h = (w * size.y / size.x.max(1.0)).min(tall - 40.0);
                        ui.add(egui::Image::new((tex, vec2(w, h))).fit_to_exact_size(vec2(w, h)).corner_radius(4));
                        if !alt.is_empty() {
                            ui.label(egui::RichText::new(alt).size(12.5).color(t.text_muted));
                        }
                    }
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_leaves_a_box_on_its_edge() {
        let r = Rect::from_center_size(pos2(0.0, 0.0), vec2(100.0, 40.0));
        let near = |a: Pos2, b: Pos2| (a - b).length() < 1e-3;
        assert!(near(boundary(r, pos2(500.0, 0.0)), pos2(50.0, 0.0)));
        assert!(near(boundary(r, pos2(0.0, -300.0)), pos2(0.0, -20.0)));
        let corner = boundary(r, pos2(100.0, 100.0));
        assert!((corner.y - 20.0).abs() < 1e-3 && corner.x <= 50.0, "{corner:?}");
        // From the centre, or towards nowhere: the centre.
        assert_eq!(boundary(r, pos2(0.0, 0.0)), pos2(0.0, 0.0));
        assert_eq!(boundary(r, pos2(f32::NAN, 0.0)), pos2(0.0, 0.0));
    }

    fn card(id: u64, rect: Rect) -> (NodeId, Rect) {
        (NodeId(id), rect)
    }

    #[test]
    fn overlapping_cards_are_moved_apart_and_a_dragged_one_stays_put() {
        let mut cards = vec![
            card(1, Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 80.0))),
            card(2, Rect::from_min_size(pos2(150.0, 20.0), vec2(200.0, 80.0))),
            card(3, Rect::from_min_size(pos2(160.0, 30.0), vec2(200.0, 80.0))),
            card(4, Rect::from_min_size(pos2(900.0, 900.0), vec2(200.0, 80.0))),
        ];
        let far = cards[3].1;
        spread(&mut cards, Some(NodeId(1)));
        assert_eq!(cards[0].1, Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 80.0)), "the dragged card stays under the pointer");
        assert_eq!(cards[3].1, far, "a card with room isn't moved");
        for (i, a) in cards.iter().enumerate() {
            for b in cards.iter().skip(i + 1) {
                assert!(!a.1.intersects(b.1), "{:?} and {:?}", a.1, b.1);
            }
        }
        // Cards on top of each other, NaN or past the most moved: nothing breaks.
        let mut stacked: Vec<(NodeId, Rect)> = (0..5).map(|i| card(i, Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 40.0)))).collect();
        stacked.push(card(9, Rect::from_min_size(pos2(f32::NAN, 0.0), vec2(100.0, 40.0))));
        spread(&mut stacked, None);
        let mut many: Vec<(NodeId, Rect)> =
            (0..(SPREAD_UP_TO as u64 + 1)).map(|i| card(i, Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0)))).collect();
        spread(&mut many, None);
        assert!(many.iter().all(|c| c.1.min == pos2(0.0, 0.0)), "past the most, left as they are");
    }

    #[test]
    fn fitting_shows_every_card_on_the_board() {
        let mut maps = MapState::default();
        for (i, at) in [[-900.0, -300.0], [1200.0, 40.0], [100.0, 800.0], [f32::NAN, 0.0]].into_iter().enumerate() {
            let content = if i % 2 == 0 { Content::Concept { label: format!("c{i}") } } else { Content::Note { text: format!("n{i}") } };
            maps.graph.add_node(content, None, at, Author::User).unwrap();
        }
        let board = Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 500.0));
        let (pan, zoom) = fit(&maps, board);
        assert!(FIT_ZOOM.contains(&zoom), "{zoom}");
        let view = View { origin: board.center() + pan, zoom };
        for n in maps.graph.nodes().iter().filter(|n| n.position.iter().all(|v| v.is_finite())) {
            let p = view.to_screen(n.position);
            assert!(board.contains(p), "{:?} at {p:?} off {board:?}", n.position);
        }
        // An empty map: as it was made.
        assert_eq!(fit(&MapState::default(), board), (Vec2::ZERO, 1.0));
    }

    #[test]
    fn kinds_have_their_own_colours_in_both_themes() {
        for dark in [false, true] {
            let t = Tokens::for_kind(if dark { theme::ThemeKind::Dark } else { theme::ThemeKind::Light });
            let tints: HashSet<[u8; 4]> =
                [Kind::Concept, Kind::Snippet, Kind::Note, Kind::Picture].into_iter().map(|k| look(&t, k).tint.to_array()).collect();
            assert_eq!(tints.len(), 4, "four colours");
        }
        assert_eq!(mix(Color32::BLACK, Color32::WHITE, 0.5), Color32::from_rgb(128, 128, 128));
        assert_eq!(mix(Color32::BLACK, Color32::WHITE, 7.0), Color32::WHITE);
    }
}
