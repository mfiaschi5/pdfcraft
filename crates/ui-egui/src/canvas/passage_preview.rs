//! Rasters of whole pages for the concept map's previews of a node's passage. The graph asks for
//! a page while a preview of it shows ([`DocView::want_passage_preview`]); the page's raster (at
//! a size a preview crops sharply) and its text layer are then requested with the frame's other
//! work, and the last few rasters are kept.

use egui::{ColorImage, TextureHandle, TextureId, TextureOptions};
use pdfcraft_render::{DocInfo, RenderRequest, RequestKind};

use super::DocView;
use super::cache::{TEXT_TAG, scale_tag};

/// The tag bit of a passage preview's raster.
pub(super) const PASSAGE_TAG: u64 = 1 << 58;
/// Rasters kept, most recent first.
const KEPT: usize = 4;
/// Device pixels across a preview's page raster: a column of text cropped from it stays sharp at
/// a preview's width on a high-density screen.
const WIDTH_PX: f32 = 1400.0;

impl DocView {
    /// A preview of a passage on `page` is showing: ask for the page's raster and text.
    pub fn want_passage_preview(&mut self, page: usize) {
        if page < self.page_count {
            self.passage_wanted = Some(page);
        }
    }

    /// The raster a preview of `page` crops: its own, else the one on screen.
    pub fn passage_raster(&self, page: usize) -> Option<TextureId> {
        self.passage_rasters.iter().find(|(p, ..)| *p == page).map(|(_, _, tex)| tex.id()).or_else(|| self.page_raster(page))
    }

    /// Whether a preview's own raster of `page` has arrived (tests).
    pub fn has_passage_raster(&self, page: usize) -> bool {
        self.passage_rasters.iter().any(|(p, ..)| *p == page)
    }

    /// The requests for the page a preview wants, once per frame it is wanted.
    pub(super) fn passage_requests(&mut self, info: &DocInfo) -> Vec<RenderRequest> {
        let Some(page) = self.passage_wanted.take() else { return Vec::new() };
        let Some(p) = info.pages.get(page) else { return Vec::new() };
        let mut out = Vec::new();
        if p.width > 0.0 && !self.errors.contains_key(&page) {
            let scale = (WIDTH_PX / p.width).clamp(0.25, 4.0);
            let tag = scale_tag(scale) | PASSAGE_TAG;
            if !self.passage_rasters.iter().any(|(q, t, _)| *q == page && *t == tag) {
                out.push(RenderRequest { page, kind: RequestKind::Pixels, tile: None, scale, tag, preview: false });
            }
        }
        if !self.texts.contains_key(&page) && !self.text_failed.contains(&page) {
            out.push(RenderRequest { page, kind: RequestKind::Text, tile: None, scale: 1.0, tag: TEXT_TAG, preview: false });
        }
        out
    }

    /// A preview's raster arrived.
    pub(super) fn receive_passage(&mut self, ctx: &egui::Context, page: usize, tag: u64, img: ColorImage) {
        let tex: TextureHandle = ctx.load_texture(format!("passage-{:?}-{page}", self.id), img, TextureOptions::LINEAR);
        self.passage_rasters.retain(|(p, ..)| *p != page);
        self.passage_rasters.insert(0, (page, tag, tex));
        self.passage_rasters.truncate(KEPT);
    }

    /// The page changed: its preview raster is out of date.
    pub(super) fn drop_passage_raster(&mut self, page: Option<usize>) {
        match page {
            Some(page) => self.passage_rasters.retain(|(p, ..)| *p != page),
            None => self.passage_rasters.clear(),
        }
    }
}
