//! A concept-map node's passage, marked on its page: the node's quote found in the page's text
//! once that is read, shaded line by line, with a marker where it starts and one where it ends.
//! The view scrolls to its start once it is found. It stays until Escape or another node's; a
//! picture's or a region's node marks its box instead.

use std::ops::Range;

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke, pos2, vec2};
use pdfcraft_render::PageText;

use super::DocView;
use super::xform::PageXform;

/// Words matched at each end when the whole quote isn't found as it is (a hyphenated line break,
/// a ligature, words the extractor joined differently in the middle).
const END_WORDS: usize = 6;
/// The longest quote looked for, in characters.
const MAX_QUOTE_CHARS: usize = 4_000;
/// How long the passage pulses when it is first shown, in seconds.
const PULSE: f32 = 1.4;

/// A node's passage on one page of this view.
#[derive(Clone, Debug)]
pub struct NodeMark {
    pub page: usize,
    pub quote: String,
    /// A region's box (view space), for nodes made from a region or a picture.
    pub rect: Option<[f32; 4]>,
    /// The glyphs of the quote once the page's text is read; `Some(None)` when it isn't there.
    found: Option<Option<Range<usize>>>,
    scrolled: bool,
    since: f64,
}

impl DocView {
    /// Go to `page` and mark `quote` there (or the box `rect`), replacing any mark before.
    pub fn mark_passage(&mut self, page: usize, quote: &str, rect: Option<[f32; 4]>) {
        let page = page.min(self.page_count.saturating_sub(1));
        let quote: String = quote.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_QUOTE_CHARS).collect();
        self.go_to_page(page);
        self.node_mark = Some(NodeMark { page, quote, rect, found: None, scrolled: false, since: 0.0 });
    }

    /// The marked passage: its page and, once the page's text is read, its glyphs (`None` inside
    /// when the quote isn't on the page).
    pub fn marked_passage(&self) -> Option<(usize, Option<Option<Range<usize>>>)> {
        self.node_mark.as_ref().map(|m| (m.page, m.found.clone()))
    }

    /// The marked passage's text as the page has it.
    pub fn marked_text(&self) -> Option<String> {
        let m = self.node_mark.as_ref()?;
        let range = m.found.clone()??;
        Some(self.texts.get(&m.page)?.text_of(range))
    }

    /// Find the quote once its page's text is read, and scroll to where it starts.
    fn resolve_node_mark(&mut self) {
        let Some(m) = self.node_mark.as_mut() else { return };
        if m.found.is_none()
            && let Some(text) = self.texts.get(&m.page)
        {
            m.found = Some(locate(text, &m.quote));
        }
        if m.scrolled {
            return;
        }
        let top = match (&m.found, m.rect) {
            (Some(Some(r)), _) => self.texts.get(&m.page).and_then(|t| t.glyphs.get(r.start)).map(|g| g.rect[1]),
            (_, Some(rect)) => Some(rect[1]),
            (Some(None), None) => None,
            (None, None) => return,
        };
        let (page, height) = (m.page, self.page_sizes.get(m.page).map(|&(_, h)| h));
        m.scrolled = true;
        if let (Some(y), Some(h)) = (top, height.filter(|h| *h > 0.0)) {
            // A little above the start, as the find bar does, so the line before shows too.
            self.goto = Some((page, ((y - 60.0) / h).clamp(0.0, 1.0)));
            self.current = page;
        }
    }

    /// The page's text changed (an edit, a reload): find the quote again.
    pub(super) fn refind_node_mark(&mut self) {
        if let Some(m) = self.node_mark.as_mut() {
            m.found = None;
            m.scrolled = true;
        }
    }
}

/// The glyphs of `quote` on the page: the whole of it (ignoring case, and treating line breaks as
/// spaces), else from where its first words are to where its last words are after them.
pub fn locate(text: &PageText, quote: &str) -> Option<Range<usize>> {
    let words: Vec<&str> = quote.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let whole = words.join(" ");
    if let Some(r) = text.find_opts_limited(&whole, false, false, 1).into_iter().next() {
        return Some(r);
    }
    if words.len() < END_WORDS.saturating_mul(2) {
        return None;
    }
    let head = words.get(..END_WORDS)?.join(" ");
    let tail = words.get(words.len().saturating_sub(END_WORDS)..)?.join(" ");
    // A passage is about as long as its quote: the end is looked for no further than twice that.
    let reach = whole.chars().count().saturating_mul(2);
    let tails = text.find_opts_limited(&tail, false, false, 64);
    text.find_opts_limited(&head, false, false, 64)
        .into_iter()
        .find_map(|h| tails.iter().find(|t| t.start >= h.end && t.end.saturating_sub(h.start) <= reach).map(|t| h.start..t.end))
}

/// The passage's lines as view-space boxes, in reading order.
pub fn line_boxes(text: &PageText, range: Range<usize>) -> Vec<[f32; 4]> {
    let mut out: Vec<(u32, [f32; 4])> = Vec::new();
    for i in range {
        let (Some(g), Some(&line)) = (text.glyphs.get(i), text.line_of.get(i)) else { break };
        match out.last_mut() {
            Some((l, r)) if *l == line => *r = [r[0].min(g.rect[0]), r[1].min(g.rect[1]), r[2].max(g.rect[2]), r[3].max(g.rect[3])],
            _ => out.push((line, g.rect)),
        }
    }
    out.into_iter().map(|(_, r)| r).collect()
}

/// The mark's blue, a passage's shading and its start and end markers.
pub const MARK: Color32 = Color32::from_rgb(0x1B, 0x63, 0xE0);

/// Shade a passage's line boxes (screen space) and mark where it starts, with a bar before the
/// first word and a dot on top, and where it ends, with a bar after the last word and a dot below,
/// as text selection handles are drawn. `size` scales the markers (1 on the page).
pub fn paint_passage(painter: &egui::Painter, boxes: &[Rect], size: f32) {
    let (Some(first), Some(last)) = (boxes.first().copied(), boxes.last().copied()) else { return };
    for b in boxes {
        painter.rect_filled(b.expand2(vec2(1.5, 1.0) * size), CornerRadius::same(2), MARK.gamma_multiply(0.18));
    }
    let bar = |top: Pos2, bottom: Pos2| painter.line_segment([top, bottom], Stroke::new(2.5 * size, MARK));
    let gap = 2.0 * size;
    let start_top = pos2(first.min.x - gap, first.min.y - 3.0 * size);
    bar(start_top, pos2(first.min.x - gap, first.max.y + size));
    painter.circle_filled(start_top - vec2(0.0, 3.5 * size), 4.5 * size, MARK);
    let end_bottom = pos2(last.max.x + gap, last.max.y + 3.0 * size);
    bar(pos2(last.max.x + gap, last.min.y - size), end_bottom);
    painter.circle_filled(end_bottom + vec2(0.0, 3.5 * size), 4.5 * size, MARK);
}

/// Paint the mark on page `page`, if it is there.
pub(super) fn paint(painter: &egui::Painter, xf: &PageXform, page: usize, view: &mut DocView) {
    if view.node_mark.as_ref().is_none_or(|m| m.page != page) {
        return;
    }
    view.resolve_node_mark();
    let ctx = painter.ctx();
    let now = ctx.input(|i| i.time);
    let Some(m) = view.node_mark.as_mut() else { return };
    if m.since == 0.0 {
        m.since = now;
    }
    let age = (now - m.since) as f32;
    let boxes: Vec<Rect> = match (&m.found, m.rect) {
        (Some(Some(r)), _) => {
            view.texts.get(&page).map(|t| line_boxes(t, r.clone())).unwrap_or_default().into_iter().map(|b| xf.view_rect(b)).collect()
        }
        (_, Some(rect)) => vec![xf.view_rect(rect)],
        _ => Vec::new(),
    };
    let Some(first) = boxes.first().copied() else { return };
    paint_passage(painter, &boxes, 1.0);
    // A short pulse around the whole passage when it first shows, so the eye finds it.
    if age < PULSE {
        let all = boxes.iter().fold(first, |a, b| a.union(*b));
        let alpha = ((PULSE - age) / PULSE * 200.0) as u8;
        let grow = 4.0 + age * 10.0;
        painter.rect_stroke(
            all.expand(grow),
            CornerRadius::same(4),
            Stroke::new(2.0, Color32::from_rgba_unmultiplied(0x1B, 0x63, 0xE0, alpha)),
            egui::StrokeKind::Outside,
        );
        ctx.request_repaint();
    }
}
