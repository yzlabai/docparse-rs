//! Image XObject collection for the OCR enhancer path (plan N3 / P4 route).
//!
//! Scanned pages are embedded raster images — no rendering is needed to OCR
//! them, only the *original* image bytes (the "structure extraction, not
//! rasterization" identity holds). This module resolves a page's image
//! XObjects up-front (like fonts) so interpretation can run on worker threads,
//! but keeps the streams *undecoded*: pixels are only materialized at `Do`
//! time for page-covering images (scan candidates), so image-heavy digital
//! documents pay nothing.
//!
//! Supported payloads (MVP): DCTDecode passthrough (JPEG bytes as-is) and
//! Flate/ASCII85 raw bitmaps with 8 bpc, 1 or 3 components (Gray8/Rgb8).
//! TODO: JBIG2/CCITT/JPX scans are recorded position-only (`ImageKind::None`)
//! — affected pages keep an auditable Image element but can't be OCR'd yet.
//! Form XObjects are resolved too (G4): each form carries its own content
//! stream, /Matrix and resources (fonts/images/nested forms, resolved up
//! front with a depth cap against cycles) — the interpreter executes them
//! recursively so text and scans inside forms are no longer missed.

use crate::font::{build_fonts_from_resources, FontInfo};
use crate::matrix::Matrix;
use docparse_core::ir::ImageKind;
use lopdf::{Dictionary, Document as PdfDocument, Object, ObjectId, Stream};
use std::collections::HashMap;

/// Maximum Form XObject nesting resolved at build time (cycle guard).
pub const MAX_FORM_DEPTH: usize = 4;

/// A Form XObject with its own content stream and pre-resolved resources.
pub struct FormX {
    pub content: Vec<u8>,
    pub matrix: Matrix,
    pub fonts: HashMap<String, FontInfo>,
    pub images: HashMap<String, XImage>,
    pub forms: HashMap<String, FormX>,
}

/// An undecoded image XObject, resolved off the shared document.
pub struct XImage {
    pub width: u32,
    pub height: u32,
    /// The raw stream (filters unapplied) — decoded lazily via [`XImage::decode`].
    stream: Stream,
}

impl XImage {
    /// Materialize the pixel payload. Returns the kind and bytes, or
    /// `ImageKind::None` with empty bytes for unsupported encodings.
    pub fn decode(&self) -> (ImageKind, Vec<u8>) {
        let filters: Vec<String> = self
            .stream
            .filters()
            .map(|fs| {
                fs.iter()
                    .map(|f| String::from_utf8_lossy(f).into_owned())
                    .collect()
            })
            .unwrap_or_default();

        // JPEG passthrough: the common scan encoding. Only the bare chain —
        // a DCT behind ASCII85/Flate pre-filters is rare; TODO if ever seen.
        if filters.last().map(String::as_str) == Some("DCTDecode") {
            if filters.len() == 1 {
                return (ImageKind::Jpeg, self.stream.content.clone());
            }
            return (ImageKind::None, Vec::new());
        }

        // Raw bitmap behind Flate/ASCII85/etc.: let lopdf apply the filters,
        // then infer components from the byte count (covers ICCBased RGB too).
        let Ok(pixels) = self.stream.decompressed_content() else {
            return (ImageKind::None, Vec::new());
        };
        let px = (self.width as usize) * (self.height as usize);
        if px == 0 {
            return (ImageKind::None, Vec::new());
        }
        match pixels.len() / px {
            3 if pixels.len() == px * 3 => (ImageKind::Rgb8, pixels),
            1 if pixels.len() == px => (ImageKind::Gray8, pixels),
            _ => (ImageKind::None, Vec::new()),
        }
    }
}

/// Resolve image XObjects from a page's resources, keyed by `Do` name.
pub fn build_page_images(doc: &PdfDocument, page_id: ObjectId) -> HashMap<String, XImage> {
    match page_resources(doc, page_id) {
        Some(res) => build_images_from_resources(doc, &res),
        None => HashMap::new(),
    }
}

/// Resolve Form XObjects (with their own resources, recursively) from a page.
pub fn build_page_forms(doc: &PdfDocument, page_id: ObjectId) -> HashMap<String, FormX> {
    match page_resources(doc, page_id) {
        Some(res) => build_forms_from_resources(doc, &res, 0),
        None => HashMap::new(),
    }
}

fn page_resources(doc: &PdfDocument, page_id: ObjectId) -> Option<Dictionary> {
    let page = doc.get_dictionary(page_id).ok()?;
    page.get(b"Resources")
        .ok()
        .and_then(|o| doc.dereference(o).ok())
        .and_then(|(_, o)| o.as_dict().ok())
        .cloned()
}

fn xobject_streams(doc: &PdfDocument, res: &Dictionary) -> Vec<(String, Stream)> {
    let Some(xobjs) = res
        .get(b"XObject")
        .ok()
        .and_then(|o| doc.dereference(o).ok())
        .and_then(|(_, o)| o.as_dict().ok())
    else {
        return Vec::new();
    };
    xobjs
        .iter()
        .filter_map(|(name, obj)| match doc.dereference(obj) {
            Ok((_, Object::Stream(s))) => {
                Some((String::from_utf8_lossy(name).into_owned(), s.clone()))
            }
            _ => None,
        })
        .collect()
}

fn build_images_from_resources(doc: &PdfDocument, res: &Dictionary) -> HashMap<String, XImage> {
    let mut out = HashMap::new();
    for (name, s) in xobject_streams(doc, res) {
        if s.dict
            .get(b"Subtype")
            .and_then(|o| o.as_name())
            .unwrap_or(b"?")
            != b"Image"
        {
            continue;
        }
        let width = s.dict.get(b"Width").and_then(|o| o.as_i64()).unwrap_or(0);
        let height = s.dict.get(b"Height").and_then(|o| o.as_i64()).unwrap_or(0);
        let bpc = s
            .dict
            .get(b"BitsPerComponent")
            .and_then(|o| o.as_i64())
            .unwrap_or(8);
        if width <= 0 || height <= 0 || bpc != 8 {
            continue; // TODO: 1-bit (CCITT/JBIG2) scans — position-only for now
        }
        out.insert(
            name,
            XImage {
                width: width as u32,
                height: height as u32,
                stream: s,
            },
        );
    }
    out
}

fn build_forms_from_resources(
    doc: &PdfDocument,
    res: &Dictionary,
    depth: usize,
) -> HashMap<String, FormX> {
    let mut out = HashMap::new();
    if depth >= MAX_FORM_DEPTH {
        return out;
    }
    for (name, s) in xobject_streams(doc, res) {
        if s.dict
            .get(b"Subtype")
            .and_then(|o| o.as_name())
            .unwrap_or(b"?")
            != b"Form"
        {
            continue;
        }
        let matrix = match s.dict.get(b"Matrix").ok().and_then(|o| o.as_array().ok()) {
            Some(arr) if arr.len() == 6 => {
                let v: Vec<f64> = arr
                    .iter()
                    .map(|o| match o {
                        Object::Integer(i) => *i as f64,
                        Object::Real(r) => *r as f64,
                        _ => 0.0,
                    })
                    .collect();
                Matrix {
                    a: v[0],
                    b: v[1],
                    c: v[2],
                    d: v[3],
                    e: v[4],
                    f: v[5],
                }
            }
            _ => Matrix::identity(),
        };
        let Ok(content) = s.decompressed_content() else {
            continue;
        };
        // A form's own resources; fall back to the parent's when absent
        // (allowed by the spec for legacy files).
        let form_res = s
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|o| doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_dict().ok())
            .cloned()
            .unwrap_or_else(|| res.clone());
        out.insert(
            name,
            FormX {
                content,
                matrix,
                fonts: build_fonts_from_resources(doc, &form_res),
                images: build_images_from_resources(doc, &form_res),
                forms: build_forms_from_resources(doc, &form_res, depth + 1),
            },
        );
    }
    out
}
