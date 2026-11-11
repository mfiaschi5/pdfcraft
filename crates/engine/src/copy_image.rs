//! Copy Image: find the picture under a point and hand it to the clipboard, as pixels or as a
//! file in its original format. A picture drawn through grouped
//! Form artwork (as Word, InDesign and many scanners write them) is found inside the group.

use std::collections::{HashMap, HashSet};

use pdfcraft_cos::{Dict, Document as Cos, ObjRef, Object};

use crate::Document;

/// Clipboards copy the whole bitmap; past this many pixels a pixel copy is refused.
const MAX_COPY_PIXELS: u64 = 64_000_000;
/// How deep Form artwork is searched for its picture.
const MAX_FORM_DEPTH: usize = 12;
/// The most of grouped artwork's content (all its Forms together) read to tell whether it draws
/// only pictures, and the most Forms read for it.
const MAX_ARTWORK_CONTENT: usize = 8 << 20;
const MAX_ARTWORK_FORMS: usize = 256;
/// Grouped artwork that draws more than pictures is saved as the page draws it, at this
/// resolution, with no side over [`MAX_ARTWORK_SIDE`] pixels.
const ARTWORK_DPI: f64 = 300.0;
const MAX_ARTWORK_SIDE: u32 = 6000;

/// A picture on a page, with its index in [`Document::page_images`].
pub type PagePicture = (usize, pdfcraft_edit::PageImage);

/// The topmost of `pictures` under user-space point `at` (`[x, y]`). `pictures` is bottom first,
/// as [`Document::page_pictures_job`] lists them, so the last one that holds `at` wins.
pub fn picture_at(pictures: &[PagePicture], at: [f64; 2]) -> Option<&PagePicture> {
    pictures.iter().rev().find(|(_, image)| image.contains(at))
}

/// The pictures on `page`, bottom first: raster images, and Form artwork that holds one.
fn pictures(cos: &Cos, page: usize) -> Vec<PagePicture> {
    let images = pdfcraft_edit::page_images(cos, page).unwrap_or_default();
    // Artwork drawn many times is searched once.
    let mut holds_one: HashMap<ObjRef, bool> = HashMap::new();
    images
        .into_iter()
        .enumerate()
        .filter(|(_, i)| match (i.is_form, i.object) {
            (false, Some(_)) => true,
            (true, Some(form)) => *holds_one.entry(form).or_insert_with(|| picture_in_form(cos, form).is_some()),
            _ => false,
        })
        .collect()
}

impl Document {
    /// The topmost picture on `page` under user-space point `at`, as its index in
    /// [`Document::page_images`]: a raster image, or Form artwork that holds one.
    pub fn page_image_at(&self, page: usize, at: [f64; 2]) -> Option<usize> {
        let editor = self.editor.as_ref()?;
        self.page_images(page).iter().rposition(|i| {
            i.contains(at)
                && match (i.is_form, i.object) {
                    (false, Some(_)) => true,
                    (true, Some(form)) => picture_in_form(&editor.cos, form).is_some(),
                    _ => false,
                }
        })
    }

    /// Work that lists the pictures on `page`, bottom first, for another thread: it reads every
    /// drawing operator on the page, seconds on a dense one, so it must not run on the window
    /// thread. Look a point up with [`picture_at`].
    ///
    /// `None` when the document can't be read. The work reads the document as it is now: later
    /// edits aren't seen (key its answer by [`Document::edit_generation`]). It returns no pictures
    /// for a page that doesn't exist or can't be read.
    pub fn page_pictures_job(&self, page: usize) -> Option<impl FnOnce() -> Vec<PagePicture> + Send + 'static> {
        let cos = self.editor.as_ref()?.cos.clone();
        Some(move || crate::guard(|| pictures(&cos, page)).unwrap_or_default())
    }

    /// Copy Image in Original Format: picture `index` on `page` as a file (extension, bytes):
    /// a JPEG as stored in the PDF, otherwise a lossless PNG.
    pub fn page_image_copy_file(&self, page: usize, index: usize) -> Result<(&'static str, Vec<u8>), String> {
        let editor = self.editor.as_ref().ok_or("the document can't be read")?;
        let img = self.page_images(page).into_iter().nth(index).ok_or_else(|| format!("page {} has no image {}", page + 1, index + 1))?;
        let object = img.object.ok_or("the image has no object")?;
        let picture = if img.is_form { picture_in_form(&editor.cos, object).ok_or("this artwork holds no picture")? } else { object };
        pdfcraft_create::image_file(&editor.cos, picture)
    }

    /// Copy Image: picture `index` on `page` at its own resolution, as straight (unpremultiplied)
    /// RGBA pixels (width, height, pixels) for the clipboard. Soft masks become alpha.
    pub fn page_image_rgba(&self, page: usize, index: usize) -> Result<(u32, u32, Vec<u8>), String> {
        let (_, bytes) = self.page_image_copy_file(page, index)?;
        let format = image::guess_format(&bytes).map_err(|e| e.to_string())?;
        let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes.as_slice()), format);
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(MAX_COPY_PIXELS * 4);
        reader.limits(limits);
        let decoder = reader.into_decoder().map_err(|e| e.to_string())?;
        let (w, h) = image::ImageDecoder::dimensions(&decoder);
        if w == 0 || h == 0 {
            return Err("the image is empty".into());
        }
        if u64::from(w) * u64::from(h) > MAX_COPY_PIXELS {
            return Err(format!("the image is too large to copy ({w} × {h} pixels); use Save Image As"));
        }
        let rgba = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?.into_rgba8();
        Ok((rgba.width(), rgba.height(), rgba.into_raw()))
    }

    /// Edit ▸ Copy of an added image (`index` among `page`'s added items): its picture as a file
    /// (extension, bytes), a JPEG or JPEG 2000 file as stored, otherwise a lossless PNG, for
    /// [`Edit::PasteContent`](crate::Edit::PasteContent). Refused when the document doesn't allow
    /// copying.
    pub fn added_image_file(&self, page: usize, index: usize) -> Result<(&'static str, Vec<u8>), String> {
        self.check_copying().map_err(|e| e.to_string())?;
        let editor = self.editor.as_ref().ok_or("the document can't be read")?;
        let item = self.added().iter().filter(|a| a.page == page).nth(index);
        let Some(pdfcraft_edit::Content::Image(img)) = item.map(|a| &a.content) else {
            return Err(format!("item {} on page {} is not an added image", index + 1, page + 1));
        };
        picture_file(&editor.cos, img.image)
    }
}

/// Add Image to Map: past this many pixels on its longer side a picture is made smaller first.
const MAP_PICTURE_SIDE: u32 = 2048;

impl Document {
    /// Add Image to Map: picture `index` on `page` as a PNG or JPEG file of at most `max_bytes`,
    /// as [`Document::page_image_file`] saves it, made smaller when it is larger: no side over
    /// 2048 pixels, then halved until it fits (a picture with transparency stays a PNG, others
    /// become a JPEG).
    pub fn page_image_for_map(&self, page: usize, index: usize, max_bytes: usize) -> Result<Vec<u8>, String> {
        let (_, bytes) = self.page_image_file(page, index)?;
        let usable = bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.starts_with(&[0xFF, 0xD8, 0xFF]);
        if usable && bytes.len() <= max_bytes {
            return Ok(bytes);
        }
        fit_picture(&bytes, max_bytes)
    }

    /// Save Image As (and Add Image to Map) of grouped Form artwork `img` on `page`: the largest
    /// picture in it as a file when it draws only pictures (as Word, InDesign and scanners wrap a
    /// picture); otherwise (vector drawings, text, a picture with labels drawn over it, as LaTeX
    /// and plotting tools write figures) the part of the page it covers, as the page draws it, as
    /// a PNG.
    pub(crate) fn artwork_file(&self, page: usize, img: &pdfcraft_edit::PageImage) -> Result<(&'static str, Vec<u8>), String> {
        let editor = self.editor.as_ref().ok_or("the document can't be read")?;
        let form = img.object.ok_or("the artwork has no object")?;
        if draws_only_pictures(&editor.cos, form)
            && let Some(picture) = picture_in_form(&editor.cos, form)
        {
            return pdfcraft_create::image_file(&editor.cos, picture);
        }
        let info = self.info.pages.get(page).ok_or_else(|| format!("page {} doesn't exist", page + 1))?;
        let [x0, y0, x1, y1] = img.visible_rect.unwrap_or(img.rect).map(|v| v as f32);
        let corners = [info.user_to_view(x0, y0), info.user_to_view(x1, y0), info.user_to_view(x0, y1), info.user_to_view(x1, y1)];
        if corners.iter().flatten().any(|v| !v.is_finite()) {
            return Err("the artwork has no size".into());
        }
        let fold = |f: fn(f32, f32) -> f32, i: usize, start: f32| corners.iter().map(|c| c[i]).fold(start, f);
        let area = [
            fold(f32::min, 0, f32::INFINITY),
            fold(f32::min, 1, f32::INFINITY),
            fold(f32::max, 0, f32::NEG_INFINITY),
            fold(f32::max, 1, f32::NEG_INFINITY),
        ];
        let png = crate::export::Exporter::new(self).region_png(page, area, ARTWORK_DPI, MAX_ARTWORK_SIDE)?;
        Ok(("png", png))
    }
}

/// A PNG or JPEG picture (a concept map's) as straight RGBA pixels (width, height, pixels), made
/// smaller to no side over `max_side` pixels: for drawing it.
pub fn picture_rgba(bytes: &[u8], max_side: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_COPY_PIXELS * 4);
    reader.limits(limits);
    let picture = reader.decode().map_err(|e| e.to_string())?;
    let side = max_side.max(1);
    let picture =
        if picture.width() > side || picture.height() > side { picture.resize(side, side, image::imageops::FilterType::Triangle) } else { picture };
    let rgba = picture.into_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Picture file `bytes` re-encoded to at most `max_bytes`: no side over [`MAP_PICTURE_SIDE`],
/// then halved, at most four times.
fn fit_picture(bytes: &[u8], max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_COPY_PIXELS * 4);
    reader.limits(limits);
    let picture = reader.decode().map_err(|e| e.to_string())?;
    let alpha = picture.color().has_alpha() && picture.to_rgba8().pixels().any(|p| p.0[3] < 255);
    let mut side = picture.width().max(picture.height()).min(MAP_PICTURE_SIDE);
    for _ in 0..5 {
        let smaller = picture.resize(side, side, image::imageops::FilterType::Triangle);
        let mut out = std::io::Cursor::new(Vec::new());
        if alpha {
            smaller.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        } else {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&smaller.to_rgb8()).map_err(|e| e.to_string())?;
        }
        let out = out.into_inner();
        if out.len() <= max_bytes {
            return Ok(out);
        }
        side = (side / 2).max(1);
    }
    Err("the picture is too large even made smaller".into())
}

/// Whether Form `form` draws only pictures: no path painted, no text, shading or inline image,
/// in it or in the Forms it draws. Content that can't be read, or is past what is read for it,
/// counts as drawing more.
fn draws_only_pictures(cos: &Cos, form: ObjRef) -> bool {
    let mut budget = pdfcraft_cos::DecodeBudget::new(MAX_ARTWORK_CONTENT, MAX_ARTWORK_FORMS);
    let mut seen = HashSet::from([form]);
    only_pictures(cos, form, 0, &mut budget, &mut seen)
}

fn only_pictures(cos: &Cos, form: ObjRef, depth: usize, budget: &mut pdfcraft_cos::DecodeBudget, seen: &mut HashSet<ObjRef>) -> bool {
    if depth > MAX_FORM_DEPTH || seen.len() > MAX_ARTWORK_FORMS {
        return false;
    }
    let obj = cos.get(form);
    let Object::Stream(s) = &*obj else { return false };
    let Ok(data) = budget.decode_within(s, MAX_ARTWORK_CONTENT) else { return false };
    let xobjects = s
        .dict
        .get(b"Resources")
        .map(|r| cos.resolve(r))
        .and_then(|r| r.as_dict().cloned())
        .and_then(|r| r.get(b"XObject").map(|x| cos.resolve(x)).and_then(|x| x.as_dict().cloned()));
    for op in pdfcraft_content::ops(&data) {
        match op.op.as_slice() {
            // Painting a path, text, a shading or an inline image.
            b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"Tj" | b"TJ" | b"'" | b"\"" | b"sh" | b"BI" => return false,
            b"Do" => {
                let Some(name) = op.operands.first().and_then(|o| o.as_name()) else { continue };
                let Some(r) = xobjects.as_ref().and_then(|x| x.get(name)).and_then(|v| v.as_ref()) else { continue };
                let is_form = matches!(&*cos.get(r), Object::Stream(inner) if inner.dict.name(b"Subtype") == Some(b"Form"));
                if is_form && seen.insert(r) && !only_pictures(cos, r, depth + 1, budget, seen) {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// An added image's picture as a file: [`pdfcraft_create::image_file`], except that a JPEG 2000
/// picture, which Add Image stores as the file it was given, is that file.
fn picture_file(cos: &Cos, image: ObjRef) -> Result<(&'static str, Vec<u8>), String> {
    match &*cos.get(image) {
        Object::Stream(s) if s.dict.name(b"Subtype") == Some(b"Image") && s.dict.name(b"Filter") == Some(b"JPXDecode") => Ok(("jp2", s.raw.to_vec())),
        _ => pdfcraft_create::image_file(cos, image),
    }
}

/// Whether image object `image` in `cos` is the picture `file` (as [`picture_file`] writes it).
/// The sizes are compared first, so an unrelated image is not re-encoded to find out.
pub(crate) fn is_picture(cos: &Cos, image: ObjRef, file: &[u8]) -> bool {
    let size = |s: &pdfcraft_cos::Stream| Some((u32::try_from(s.dict.int(b"Width")?).ok()?, u32::try_from(s.dict.int(b"Height")?).ok()?));
    // The object at that number now, not one of an earlier generation of it.
    if cos.generation(image.num) != image.generation {
        return false;
    }
    let stored = match &*cos.get(image) {
        // A JPEG 2000 picture is stored as its file.
        Object::Stream(s) if s.dict.name(b"Subtype") == Some(b"Image") && s.dict.name(b"Filter") == Some(b"JPXDecode") => return s.raw[..] == *file,
        Object::Stream(s) if s.dict.name(b"Subtype") == Some(b"Image") => size(s),
        _ => return false,
    };
    let given = image::ImageReader::new(std::io::Cursor::new(file)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok());
    stored.is_some() && stored == given && picture_file(cos, image).is_ok_and(|(_, data)| data == file)
}

/// The image object an added image in `cos` already draws picture `file` from, if any: where a
/// second paste from another document finds the first one's.
pub(crate) fn added_picture(cos: &Cos, file: &[u8]) -> Option<ObjRef> {
    let mut seen = std::collections::HashSet::new();
    pdfcraft_edit::list_added(cos).into_iter().find_map(|a| match a.content {
        pdfcraft_edit::Content::Image(img) if seen.insert(img.image) && is_picture(cos, img.image, file) => Some(img.image),
        _ => None,
    })
}

/// The largest raster image Form `form` draws through its resources (nested Forms included).
pub(crate) fn picture_in_form(cos: &Cos, form: ObjRef) -> Option<ObjRef> {
    let mut seen = HashSet::from([form]);
    let mut best: Option<(u64, ObjRef)> = None;
    let res = resources(cos, form)?;
    collect(cos, &res, 0, &mut seen, &mut best);
    best.map(|(_, r)| r)
}

fn resources(cos: &Cos, stream: ObjRef) -> Option<Dict> {
    match &*cos.get(stream) {
        Object::Stream(s) => s.dict.get(b"Resources").map(|r| cos.resolve(r)).and_then(|r| r.as_dict().cloned()),
        _ => None,
    }
}

fn collect(cos: &Cos, res: &Dict, depth: usize, seen: &mut HashSet<ObjRef>, best: &mut Option<(u64, ObjRef)>) {
    if depth > MAX_FORM_DEPTH {
        return;
    }
    let Some(xobjects) = res.get(b"XObject").map(|x| cos.resolve(x)).and_then(|x| x.as_dict().cloned()) else { return };
    for (_, v) in xobjects.iter() {
        let Some(r) = v.as_ref() else { continue };
        if !seen.insert(r) {
            continue;
        }
        let obj = cos.get(r);
        let Object::Stream(s) = &*obj else { continue };
        match s.dict.name(b"Subtype") {
            Some(b"Image") => {
                let px = |k: &[u8]| u64::try_from(s.dict.int(k).unwrap_or(0)).unwrap_or(0);
                let size = px(b"Width").saturating_mul(px(b"Height"));
                if size > 0 && best.is_none_or(|(b, _)| size > b) {
                    *best = Some((size, r));
                }
            }
            Some(b"Form") => {
                if let Some(inner) = resources(cos, r) {
                    collect(cos, &inner, depth + 1, seen, best);
                }
            }
            _ => {}
        }
    }
}
