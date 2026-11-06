//! Rasters for the concept map's previews of a node's passage. A preview shows the part of a page
//! around the passage, so the graph asks for that crop ([`DocView::want_passage_preview`]) at the
//! preview's size in device pixels. The crop is rendered as a tile of the page at that scale, so its
//! words stay sharp however large the preview is, and the page's text layer is requested with it.
//! The last few crops are kept.

use egui::{ColorImage, Pos2, Rect, TextureHandle, TextureId, TextureOptions, Vec2, pos2, vec2};
use pdfcraft_render::{DocInfo, RenderRequest, RequestKind, Tile};

use super::DocView;
use super::cache::{TEXT_TAG, scale_tag, tag_scale};

/// The tag bit of a preview's raster.
pub(super) const PASSAGE_TAG: u64 = 1 << 58;
/// Crops kept, most recent first.
const KEPT: usize = 6;
/// The most device pixels across or down a crop.
const MAX_SIDE_PX: f32 = 4096.0;
/// The scales a crop is rendered at, in steps of 1/16, so a small change in the preview's width
/// doesn't ask for it again.
const SCALE_STEPS: f32 = 16.0;

/// A crop of a page, rendered: the part of the page it covers (view-space points) and its texture.
pub(super) struct PassageRaster {
    page: usize,
    tag: u64,
    tile: Tile,
    covers: Rect,
    tex: TextureHandle,
}

impl DocView {
    /// A preview shows `crop` (view-space points) of `page`, `width_px` device pixels wide: ask
    /// for that crop and for the page's text.
    pub fn want_passage_preview(&mut self, page: usize, crop: Rect, width_px: f32) {
        if page < self.page_count && crop.is_finite() && width_px.is_finite() && width_px > 0.0 {
            self.passage_wanted = Some((page, crop, width_px));
        }
    }

    /// What a preview of `crop` of `page` (a page `page_size` points large) draws from: the
    /// sharpest kept crop that covers it, else the page as it is on screen. Returns the texture and
    /// the part of the page it covers.
    pub fn passage_raster(&self, page: usize, crop: Rect, page_size: Vec2) -> Option<(TextureId, Rect)> {
        self.passage_rasters
            .iter()
            .filter(|r| r.page == page && r.covers.expand(0.5).contains_rect(crop))
            .max_by(|a, b| tag_scale(a.tag & !PASSAGE_TAG).total_cmp(&tag_scale(b.tag & !PASSAGE_TAG)))
            .map(|r| (r.tex.id(), r.covers))
            .or_else(|| self.page_raster(page).map(|tex| (tex, Rect::from_min_size(Pos2::ZERO, page_size))))
    }

    /// Whether a crop of `page` has been rendered for a preview (tests).
    pub fn has_passage_raster(&self, page: usize) -> bool {
        self.passage_rasters.iter().any(|r| r.page == page)
    }

    /// The requests for the crop a preview wants, once per frame it is wanted.
    pub(super) fn passage_requests(&mut self, info: &DocInfo) -> Vec<RenderRequest> {
        let Some((page, crop, width_px)) = self.passage_wanted.take() else { return Vec::new() };
        let Some(p) = info.pages.get(page) else { return Vec::new() };
        let mut out = Vec::new();
        if !self.errors.contains_key(&page)
            && let Some((scale, tile)) = passage_tile(crop, vec2(p.width, p.height), width_px)
        {
            let tag = scale_tag(scale) | PASSAGE_TAG;
            if !self.passage_rasters.iter().any(|r| r.page == page && r.tag == tag && r.tile == tile) {
                out.push(RenderRequest { page, kind: RequestKind::Pixels, tile: Some(tile), scale, tag, preview: false });
            }
        }
        if !self.texts.contains_key(&page) && !self.text_failed.contains(&page) {
            out.push(RenderRequest { page, kind: RequestKind::Text, tile: None, scale: 1.0, tag: TEXT_TAG, preview: false });
        }
        out
    }

    /// The document isn't drawn (the whole map shows instead of it): ask for what a preview
    /// wants on its own, with the frame's other work.
    pub(crate) fn queue_passage_requests(&mut self, info: &DocInfo) {
        let requests = self.passage_requests(info);
        self.frame_queue.extend(requests);
    }

    /// A preview's crop arrived.
    pub(super) fn receive_passage(&mut self, ctx: &egui::Context, req: &RenderRequest, img: ColorImage) {
        let Some(tile) = req.tile else { return };
        if !(req.scale.is_finite() && req.scale > 0.0) {
            return;
        }
        // What the image covers, from its own size: the renderer may clip a tile at the page's edge.
        let [w, h] = img.size;
        let covers =
            Rect::from_min_size(pos2(tile.x as f32 / req.scale, tile.y as f32 / req.scale), vec2(w as f32 / req.scale, h as f32 / req.scale));
        let tex = ctx.load_texture(format!("passage-{:?}-{}", self.id, req.page), img, TextureOptions::LINEAR);
        self.passage_rasters.retain(|r| !(r.page == req.page && r.tag == req.tag && r.tile == tile));
        self.passage_rasters.insert(0, PassageRaster { page: req.page, tag: req.tag, tile, covers, tex });
        self.passage_rasters.truncate(KEPT);
    }

    /// The page changed: its crops are out of date.
    pub(super) fn drop_passage_raster(&mut self, page: Option<usize>) {
        match page {
            Some(page) => self.passage_rasters.retain(|r| r.page != page),
            None => self.passage_rasters.clear(),
        }
    }
}

/// The scale and the tile (device pixels at that scale) that render `crop` of a page `page`
/// points large, `width_px` device pixels wide: on the page, at least a pixel each way, and no
/// side over [`MAX_SIDE_PX`]. `None` for a crop with nothing of the page in it.
pub(super) fn passage_tile(crop: Rect, page: Vec2, width_px: f32) -> Option<(f32, Tile)> {
    if !crop.is_finite() {
        return None;
    }
    let crop = crop.intersect(Rect::from_min_size(Pos2::ZERO, page));
    if !(crop.is_finite() && crop.width() >= 1.0 && crop.height() >= 1.0 && width_px.is_finite() && width_px > 0.0) {
        return None;
    }
    let fitting = (width_px / crop.width()).min(MAX_SIDE_PX / crop.width()).min(MAX_SIDE_PX / crop.height());
    let scale = ((fitting * SCALE_STEPS).floor() / SCALE_STEPS).clamp(0.25, 8.0);
    // Whole device pixels, inside the page as rendered at this scale. The casts saturate, and every
    // value is finite and not negative here.
    let x0 = (crop.min.x * scale).floor();
    let y0 = (crop.min.y * scale).floor();
    let x1 = (crop.max.x * scale).ceil().min((page.x * scale).floor()).max(x0 + 1.0);
    let y1 = (crop.max.y * scale).ceil().min((page.y * scale).floor()).max(y0 + 1.0);
    Some((scale, Tile { x: x0 as u32, y: y0 as u32, w: (x1 - x0) as u32, h: (y1 - y0) as u32 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crop_is_rendered_as_a_tile_of_the_page_at_the_previews_width() {
        let page = vec2(600.0, 800.0);
        let crop = Rect::from_min_max(pos2(100.0, 300.0), pos2(400.0, 420.0));
        let (scale, tile) = passage_tile(crop, page, 1200.0).unwrap();
        assert!((scale - 4.0).abs() < 1e-6, "1200 px across 300 points: {scale}");
        assert_eq!((tile.x, tile.y, tile.w, tile.h), (400, 1200, 1200, 480));
        // A width that changes a little asks for the same tile.
        assert_eq!(passage_tile(crop, page, 1203.0), Some((scale, tile)));
    }

    #[test]
    fn a_tile_stays_on_the_page_and_within_its_size() {
        let page = vec2(600.0, 800.0);
        // Partly off the page: only the part on it.
        let (scale, tile) = passage_tile(Rect::from_min_max(pos2(500.0, -50.0), pos2(700.0, 100.0)), page, 400.0).unwrap();
        assert!((tile.x + tile.w) as f32 <= page.x * scale + 0.5 && (tile.y + tile.h) as f32 <= page.y * scale + 0.5);
        assert_eq!(tile.y, 0);
        // Asked far too wide: no side over the most a crop may be.
        let (_, tile) = passage_tile(Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 600.0)), page, 1.0e9).unwrap();
        assert!(tile.w as f32 <= MAX_SIDE_PX && tile.h as f32 <= MAX_SIDE_PX, "{tile:?}");
        // Nothing of the page, or nothing to fill: no tile.
        assert_eq!(passage_tile(Rect::from_min_max(pos2(700.0, 0.0), pos2(800.0, 10.0)), page, 400.0), None);
        assert_eq!(passage_tile(Rect::from_min_max(pos2(0.0, 0.0), pos2(f32::NAN, 10.0)), page, 400.0), None);
        assert_eq!(passage_tile(Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0)), page, f32::INFINITY), None);
        assert_eq!(passage_tile(Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0)), page, 0.0), None);
    }
}
