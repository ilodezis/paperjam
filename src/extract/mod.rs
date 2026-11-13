//! Text extraction from PDFs and decoding of images (spec §7.2).
//!
//! PDF parsing and rendering sit behind traits so the young pure-Rust crates
//! can be swapped for pdfium without touching the pipeline.

pub mod clean;
pub mod image;
pub mod pdf;
pub mod preprocess;
pub mod quality;
pub mod tesseract;
pub mod worker;

use ::image::RgbImage;

use crate::error::Result;

/// A PDF opened for text extraction. Pages are numbered from 1.
pub trait PdfText {
    fn page_count(&self) -> u32;
    fn page_text(&self, page: u32) -> Result<String>;
    /// Pixels per point of the image that covers the page, when the page is
    /// one full-page image (a scan): its own pixel size over the size it is
    /// drawn at. `None` for any other page, and when it cannot be told.
    fn page_image_density(&self, page: u32) -> Option<f32>;
}

/// Opens PDFs for text extraction.
pub trait TextExtractor: Send + Sync {
    fn open(&self, pdf: &[u8]) -> Result<Box<dyn PdfText>>;
}

/// A PDF opened for rendering. Pages are numbered from 1.
pub trait PdfPages {
    fn page_count(&self) -> u32;
    fn render(&self, page: u32, dpi: u32) -> Result<RgbImage>;
}

/// Opens PDFs for rendering pages to bitmaps.
pub trait PageRenderer: Send + Sync {
    fn open(&self, pdf: &[u8]) -> Result<Box<dyn PdfPages>>;
}

pub use self::pdf::{HayroRenderer, OxideExtractor};

#[cfg(test)]
pub(crate) fn fixture(name: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("gen")
        .join(name);
    assert!(
        path.exists(),
        "fixture {name} is missing: run `cargo xtask fixtures` first"
    );
    path
}
