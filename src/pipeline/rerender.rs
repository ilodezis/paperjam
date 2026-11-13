//! Rebuilds the cached page renders and thumbnails of a document in the
//! current format (`paperjam rerender`).
//!
//! Older versions cached lossless WebP: hundreds of kilobytes per page and
//! per thumbnail. The source here is the cached render itself, not the
//! original file: its pixels are exactly what the pipeline produced after
//! turning the page upright, a decision that is not stored anywhere else.
//! Only lossless files are converted, so running it again (or on a document
//! processed by the current version) changes nothing.
//!
//! A scan with a clean copy is shown the way that copy shows it. Documents
//! processed before that was so are brought along here, from `clean.pdf` (no
//! reprocess, no database migration): the old page renders move to
//! `original-N.webp`, the clean pages take their place, and the thumbnail is
//! made from the first of them.

use std::path::Path;

use crate::error::{Result, ServiceError};
use crate::extract::image as images;
use crate::extract::{HayroRenderer, PageRenderer};
use crate::pipeline::local::{CACHE_EDGE, OCR_DPI};
use crate::service::documents::CLEAN_COPY;

/// What a run did.
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// Page renders rewritten.
    pub pages: u32,
    /// Thumbnails rewritten or created.
    pub thumbnails: u32,
    /// Bytes of the rewritten files, before and after.
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// Pages whose render was rebuilt from the clean copy, with their new
    /// size: (n, width, height). The pages table follows these.
    pub cleaned: Vec<(u32, u32, u32)>,
}

impl Report {
    pub fn add(&mut self, other: Report) {
        self.pages += other.pages;
        self.thumbnails += other.thumbnails;
        self.bytes_before += other.bytes_before;
        self.bytes_after += other.bytes_after;
        self.cleaned.extend(other.cleaned);
    }
}

/// True when the WebP in `bytes` holds a lossless (`VP8L`) image.
pub fn is_lossless(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return false;
    }
    let mut chunks = &bytes[12..];
    while chunks.len() >= 8 {
        let size = u32::from_le_bytes([chunks[4], chunks[5], chunks[6], chunks[7]]) as usize;
        if &chunks[..4] == b"VP8L" {
            return true;
        }
        // Chunks are padded to an even size.
        let next = 8 + size + (size & 1);
        chunks = chunks.get(next..).unwrap_or_default();
    }
    false
}

fn storage(error: std::io::Error) -> ServiceError {
    tracing::warn!(%error, "could not rewrite a cached image");
    ServiceError::internal("storage_error", "could not rewrite the page renders")
}

fn unreadable(error: impl std::fmt::Display) -> ServiceError {
    tracing::warn!(%error, "could not decode a cached image");
    ServiceError::internal("image_corrupt", "a cached image could not be read")
}

/// Writes `bytes` so that a reader never sees a half-written file.
fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension("webp.tmp");
    std::fs::write(&temp, bytes).map_err(storage)?;
    std::fs::rename(&temp, path).map_err(storage)
}

/// Brings a document's derived directory up to date: lossless renders become
/// lossy, and a scan with a clean copy is shown from it. A missing directory
/// is an empty one.
pub fn rerender_dir(dir: &Path) -> Result<Report> {
    let mut report = convert_lossless(dir)?;
    show_clean_copy(dir, &mut report)?;
    Ok(report)
}

/// Shows the pages of `clean.pdf` in place of the renders in `dir`, unless
/// that has been done (`original-1.webp` is there) or there is no clean copy.
fn show_clean_copy(dir: &Path, report: &mut Report) -> Result<()> {
    let Ok(pdf) = std::fs::read(dir.join(CLEAN_COPY)) else {
        return Ok(());
    };
    if dir.join("original-1.webp").exists() {
        return Ok(());
    }
    let pages = HayroRenderer.open(&pdf)?;
    let mut first = None;
    for n in 1..=pages.page_count() {
        // Pages as they are in the clean copy: the same pixels it was built
        // from, at the resolution it was read at.
        let image = ::image::DynamicImage::ImageRgb8(pages.render(n, OCR_DPI)?);
        let new = images::webp(&image, CACHE_EDGE)?;
        let page = dir.join(format!("page-{n}.webp"));
        // What was shown until now is kept as the original.
        if page.exists() {
            std::fs::rename(&page, dir.join(format!("original-{n}.webp"))).map_err(storage)?;
        }
        replace(&page, &new.bytes)?;
        report.cleaned.push((n, new.width, new.height));
        if n == 1 {
            first = Some(image);
        }
    }
    if let Some(image) = first {
        replace(&dir.join("thumbnail.webp"), &images::thumbnail(&image)?)?;
        report.thumbnails += 1;
    }
    Ok(())
}

/// Converts the lossless `page-N.webp` files in `dir` (a document's derived
/// directory) to the current lossy format and rebuilds the thumbnail from the
/// first page when it is lossless or missing.
fn convert_lossless(dir: &Path) -> Result<Report> {
    let mut report = Report::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(report);
    };
    let mut pages: Vec<u32> = entries
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            name.strip_prefix("page-")?
                .strip_suffix(".webp")?
                .parse()
                .ok()
        })
        .collect();
    pages.sort_unstable();

    let mut first = None;
    for n in pages {
        let path = dir.join(format!("page-{n}.webp"));
        let old = std::fs::read(&path).map_err(storage)?;
        if !is_lossless(&old) {
            continue;
        }
        let image = ::image::load_from_memory(&old).map_err(unreadable)?;
        let new = images::webp(&image, CACHE_EDGE)?;
        replace(&path, &new.bytes)?;
        report.pages += 1;
        report.bytes_before += old.len() as u64;
        report.bytes_after += new.bytes.len() as u64;
        if n == 1 {
            first = Some(image);
        }
    }

    let thumbnail = dir.join("thumbnail.webp");
    let old = std::fs::read(&thumbnail).ok();
    if old.as_deref().is_some_and(|bytes| !is_lossless(bytes)) {
        return Ok(report);
    }
    let source = match first {
        Some(image) => Some(image),
        None => std::fs::read(dir.join("page-1.webp"))
            .ok()
            .and_then(|bytes| ::image::load_from_memory(&bytes).ok()),
    };
    if let Some(image) = source {
        let new = images::thumbnail(&image)?;
        replace(&thumbnail, &new)?;
        report.thumbnails += 1;
        report.bytes_before += old.map_or(0, |bytes| bytes.len() as u64);
        report.bytes_after += new.len() as u64;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ::image::{DynamicImage, GenericImageView, ImageFormat};

    use super::*;
    use crate::extract::fixture;

    /// What the pipeline used to write: lossless WebP.
    fn lossless(image: &DynamicImage, edge: u32) -> Vec<u8> {
        let fitted = image.thumbnail(edge, edge);
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(fitted.to_rgb8())
            .write_to(&mut out, ImageFormat::WebP)
            .unwrap();
        out.into_inner()
    }

    fn sheet() -> DynamicImage {
        ::image::open(fixture("passport_td3_sideways_a4.png"))
            .unwrap()
            .rotate270()
    }

    /// A derived directory as an older version left it.
    fn old_directory(pages: u32) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let image = sheet();
        for n in 1..=pages {
            std::fs::write(
                dir.path().join(format!("page-{n}.webp")),
                lossless(&image, 1600),
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("thumbnail.webp"), lossless(&image, 512)).unwrap();
        dir
    }

    fn read(dir: &Path, name: &str) -> Vec<u8> {
        std::fs::read(dir.join(name)).unwrap()
    }

    #[test]
    fn old_lossless_files_are_rebuilt_small() {
        let dir = old_directory(2);
        let before = read(dir.path(), "page-1.webp");
        assert!(is_lossless(&before));

        let report = rerender_dir(dir.path()).unwrap();

        assert_eq!((report.pages, report.thumbnails), (2, 1));
        for name in ["page-1.webp", "page-2.webp"] {
            let bytes = read(dir.path(), name);
            assert!(!is_lossless(&bytes), "{name} is still lossless");
            assert!(bytes.len() <= 150_000, "{name}: {} bytes", bytes.len());
            let decoded = ::image::load_from_memory(&bytes).unwrap();
            assert_eq!(decoded.dimensions(), (1131, 1600), "{name}");
        }
        let thumbnail = read(dir.path(), "thumbnail.webp");
        assert!(thumbnail.len() <= 20_000, "{} bytes", thumbnail.len());
        // Cropped to the passport, not the whole sheet.
        let (w, h) = ::image::load_from_memory(&thumbnail).unwrap().dimensions();
        assert!(w > h, "{w}x{h}");
        assert!(report.bytes_after < report.bytes_before, "{report:?}");
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let dir = old_directory(1);
        rerender_dir(dir.path()).unwrap();
        let page = read(dir.path(), "page-1.webp");
        let thumbnail = read(dir.path(), "thumbnail.webp");

        let report = rerender_dir(dir.path()).unwrap();

        assert_eq!((report.pages, report.thumbnails), (0, 0));
        assert_eq!(read(dir.path(), "page-1.webp"), page);
        assert_eq!(read(dir.path(), "thumbnail.webp"), thumbnail);
    }

    #[test]
    fn a_missing_thumbnail_is_made_from_the_first_page() {
        let dir = old_directory(1);
        std::fs::remove_file(dir.path().join("thumbnail.webp")).unwrap();
        let report = rerender_dir(dir.path()).unwrap();
        assert_eq!(report.thumbnails, 1);
        assert!(dir.path().join("thumbnail.webp").exists());
    }

    /// A clean copy of the sheet, tall and narrow, as the pipeline builds it.
    fn clean_copy(dir: &Path) -> (u32, u32) {
        let page = ::image::open(fixture("passport_td3_sideways_a4.png"))
            .unwrap()
            .rotate270()
            .to_rgb8();
        let page = crate::extract::clean::page(page, 72.0 / 300.0).unwrap();
        let size = (page.width, page.height);
        std::fs::write(dir.join(CLEAN_COPY), crate::extract::clean::pdf(&[page])).unwrap();
        size
    }

    #[test]
    fn an_old_scan_is_shown_from_its_clean_copy_and_keeps_its_original() {
        let dir = old_directory(1);
        let (w, h) = clean_copy(dir.path());
        // What was shown until now: a different shape than the clean page.
        let shown = ::image::open(fixture("passport_td3_sideways_a4.png"))
            .unwrap()
            .thumbnail(1600, 1600);
        std::fs::write(
            dir.path().join("page-1.webp"),
            images::webp(&shown, 1600).unwrap().bytes,
        )
        .unwrap();
        let before = read(dir.path(), "page-1.webp");

        let report = rerender_dir(dir.path()).unwrap();

        assert_eq!(read(dir.path(), "original-1.webp"), before);
        let page = ::image::load_from_memory(&read(dir.path(), "page-1.webp")).unwrap();
        let aspect = |w: u32, h: u32| w as f32 / h as f32;
        assert!((aspect(page.width(), page.height()) - aspect(w, h)).abs() < 0.02);
        assert_eq!(report.cleaned, vec![(1, page.width(), page.height())]);
        // The thumbnail is made from the clean page, not from the old render.
        let thumbnail = read(dir.path(), "thumbnail.webp");
        assert!(!is_lossless(&thumbnail));
        assert_ne!(thumbnail, images::thumbnail(&shown).unwrap());

        // Done once: a second run leaves it alone.
        let page = read(dir.path(), "page-1.webp");
        let report = rerender_dir(dir.path()).unwrap();
        assert!(report.cleaned.is_empty());
        assert_eq!(read(dir.path(), "page-1.webp"), page);
        assert_eq!(read(dir.path(), "original-1.webp"), before);
    }

    #[test]
    fn a_directory_without_renders_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let report = rerender_dir(dir.path()).unwrap();
        assert_eq!((report.pages, report.thumbnails), (0, 0));
        // A document that never had a derived directory.
        let report = rerender_dir(&dir.path().join("nope")).unwrap();
        assert_eq!((report.pages, report.thumbnails), (0, 0));
    }
}
