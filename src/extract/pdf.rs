//! PDF text extraction (`pdf_oxide`) and page rendering (`hayro`).
//!
//! Both crates are young. Every call into them is wrapped so that a panic on
//! a hostile file ends the job instead of the worker thread.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use ::image::RgbImage;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};
use pdf_oxide::PdfDocument;

use super::{PageRenderer, PdfPages, PdfText, TextExtractor};
use crate::error::{Result, ServiceError};

/// PDFs with more pages are refused. A page is held in memory (render, clean
/// copy, text) until the document is stored, so this also bounds a run's memory.
pub const MAX_PAGES: u32 = 300;
/// Rendered pages are capped to this many pixels along the longest edge
/// (hayro works with 16-bit pixel sizes) and this many pixels in total.
const MAX_EDGE_PIXELS: f32 = 16_000.0;
const MAX_TOTAL_PIXELS: f32 = 100_000_000.0;
const POINTS_PER_INCH: f32 = 72.0;
/// Lower bound when capping OCR render resolution to a scan's native density.
pub const MIN_OCR_DPI: u32 = 150;

/// DPI to render a scanned page at: capped to the scan's own pixel density
/// when known, never below [`MIN_OCR_DPI`] (if `ocr_dpi` allows it) and never
/// above `ocr_dpi`.
pub fn effective_ocr_dpi(ocr_dpi: u32, density_px_per_pt: Option<f32>) -> u32 {
    match density_px_per_pt {
        Some(density) if density.is_finite() && density > 0.0 => {
            let native = (density * POINTS_PER_INCH).ceil() as u32;
            native.clamp(MIN_OCR_DPI.min(ocr_dpi), ocr_dpi)
        }
        _ => ocr_dpi,
    }
}

fn encrypted() -> ServiceError {
    ServiceError::invalid("pdf_encrypted", "the PDF is password protected")
}

fn corrupt() -> ServiceError {
    ServiceError::invalid("pdf_corrupt", "the PDF could not be read")
}

fn too_many_pages(count: u32) -> ServiceError {
    ServiceError::invalid(
        "too_many_pages",
        format!("the PDF has {count} pages, the limit is {MAX_PAGES}"),
    )
}

fn page_out_of_range(page: u32, count: u32) -> ServiceError {
    ServiceError::invalid(
        "page_out_of_range",
        format!("pages are numbered 1 to {count}, got {page}"),
    )
}

/// Runs `f`, turning a panic into `pdf_corrupt`.
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        tracing::error!("PDF library panicked");
        Err(corrupt())
    })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OxideExtractor;

struct OxideText {
    document: PdfDocument,
    pages: u32,
}

impl TextExtractor for OxideExtractor {
    fn open(&self, pdf: &[u8]) -> Result<Box<dyn PdfText>> {
        guarded(|| {
            let document = PdfDocument::from_bytes(pdf.to_vec()).map_err(|e| {
                tracing::debug!(error = %e, "pdf_oxide could not open the file");
                if matches!(e, pdf_oxide::Error::EncryptedPdf) {
                    encrypted()
                } else {
                    corrupt()
                }
            })?;
            if document.is_encrypted() && !document.authenticate(b"").unwrap_or(false) {
                return Err(encrypted());
            }
            let pages = document.page_count().map_err(|_| corrupt())?;
            let pages = u32::try_from(pages).map_err(|_| too_many_pages(u32::MAX))?;
            if pages > MAX_PAGES {
                return Err(too_many_pages(pages));
            }
            Ok(Box::new(OxideText { document, pages }) as Box<dyn PdfText>)
        })
    }
}

impl PdfText for OxideText {
    fn page_count(&self) -> u32 {
        self.pages
    }

    fn page_text(&self, page: u32) -> Result<String> {
        if page == 0 || page > self.pages {
            return Err(page_out_of_range(page, self.pages));
        }
        guarded(|| {
            self.document.extract_text(page as usize - 1).map_err(|e| {
                tracing::debug!(error = %e, page, "pdf_oxide text extraction failed");
                corrupt()
            })
        })
    }

    fn page_image_density(&self, page: u32) -> Option<f32> {
        if page == 0 || page > self.pages {
            return None;
        }
        guarded(|| Ok(self.image_density(page))).ok().flatten()
    }
}

/// An image must cover this share of the page to be taken for the page itself
/// (a scan) and not a logo or a stamp on it.
const FULL_PAGE_SHARE: f32 = 0.8;

impl OxideText {
    fn image_density(&self, page: u32) -> Option<f32> {
        let index = page as usize - 1;
        let (x0, y0, x1, y1) = self.document.get_page_media_box(index).ok()?;
        let page_area = (x1 - x0).abs() * (y1 - y0).abs();
        if page_area <= 0.0 {
            return None;
        }
        let handles = self.document.page_image_handles(index).ok()?;
        // Only metadata is read: the image is never decoded.
        let image = handles.iter().max_by(|a, b| {
            (a.bbox.width * a.bbox.height).total_cmp(&(b.bbox.width * b.bbox.height))
        })?;
        let drawn = image.bbox.width * image.bbox.height;
        if drawn < page_area * FULL_PAGE_SHARE || drawn <= 0.0 {
            return None;
        }
        // Whatever way the image is turned, pixels over area is its density.
        let pixels = image.width as f32 * image.height as f32;
        let density = (pixels / drawn).sqrt();
        (density.is_finite() && density > 0.0).then_some(density)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct HayroRenderer;

/// A PDF opened with `hayro` that allows callers to pass their own
/// thread-local `RenderCache` across pages.
pub struct HayroPdf {
    pdf: Pdf,
    pages: u32,
}

impl HayroPdf {
    pub fn open(bytes: Arc<Vec<u8>>) -> Result<Self> {
        guarded(|| {
            let pdf = Pdf::new(bytes).map_err(|e| match e {
                LoadPdfError::Decryption(_) => encrypted(),
                LoadPdfError::Invalid => corrupt(),
            })?;
            let pages = u32::try_from(pdf.pages().len()).map_err(|_| too_many_pages(u32::MAX))?;
            if pages > MAX_PAGES {
                return Err(too_many_pages(pages));
            }
            Ok(Self { pdf, pages })
        })
    }

    pub fn page_count(&self) -> u32 {
        self.pages
    }

    pub fn render_with_cache<'a>(
        &'a self,
        page: u32,
        dpi: u32,
        cache: &RenderCache<'a>,
    ) -> Result<RgbImage> {
        if page == 0 || page > self.pages {
            return Err(page_out_of_range(page, self.pages));
        }
        render_page(&self.pdf, cache, page, dpi)
    }
}

struct HayroPages {
    pdf: HayroPdf,
}

impl PageRenderer for HayroRenderer {
    fn open(&self, pdf: &[u8]) -> Result<Box<dyn PdfPages>> {
        let pdf = HayroPdf::open(Arc::new(pdf.to_vec()))?;
        Ok(Box::new(HayroPages { pdf }) as Box<dyn PdfPages>)
    }
}

impl PdfPages for HayroPages {
    fn page_count(&self) -> u32 {
        self.pdf.page_count()
    }

    fn render(&self, page: u32, dpi: u32) -> Result<RgbImage> {
        let cache = RenderCache::new();
        self.pdf.render_with_cache(page, dpi, &cache)
    }
}

fn render_page<'a>(pdf: &'a Pdf, cache: &RenderCache<'a>, page: u32, dpi: u32) -> Result<RgbImage> {
    guarded(|| {
        let page = &pdf.pages()[page as usize - 1];
        let (width, height) = page.render_dimensions();
        let mut scale = dpi.max(1) as f32 / POINTS_PER_INCH;
        let longest = width.max(height) * scale;
        if longest > MAX_EDGE_PIXELS {
            scale *= MAX_EDGE_PIXELS / longest;
        }
        let total = width * height * scale * scale;
        if total > MAX_TOTAL_PIXELS {
            scale *= (MAX_TOTAL_PIXELS / total).sqrt();
        }
        let pixmap = render(
            page,
            cache,
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: WHITE,
            },
        );
        let (w, h) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
        // Premultiplied RGBA on an opaque white background: drop alpha.
        let src = pixmap.data_as_u8_slice();
        let pixels = (src.len() / 4) * 3;
        let mut rgb = Vec::with_capacity(pixels);
        for px in src.as_chunks::<4>().0 {
            rgb.extend_from_slice(&[px[0], px[1], px[2]]);
        }
        RgbImage::from_raw(w, h, rgb).ok_or_else(corrupt)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{fixture, quality};

    fn bytes(name: &str) -> Vec<u8> {
        std::fs::read(fixture(name)).unwrap()
    }

    fn open_text(name: &str) -> Box<dyn PdfText> {
        OxideExtractor.open(&bytes(name)).unwrap()
    }

    #[test]
    fn born_digital_pdf_text_is_good() {
        let doc = open_text("contract_ru_digital.pdf");
        let text = doc.page_text(1).unwrap();
        assert!(text.contains("Договор аренды"), "{text:?}");
        assert!(
            quality::score(&text) >= 0.6,
            "score {}",
            quality::score(&text)
        );
    }

    #[test]
    fn romanian_born_digital_pdf_keeps_diacritics() {
        let doc = open_text("contract_ro_digital.pdf");
        let text = doc.page_text(1).unwrap();
        assert!(text.contains("Contractul de închiriere"), "{text:?}");
    }

    #[test]
    fn image_only_pdf_has_no_text() {
        let doc = open_text("invoice_scan_en.pdf");
        assert!(doc.page_count() >= 1);
        let text = doc.page_text(1).unwrap();
        assert!(text.trim().is_empty(), "{text:?}");
        assert_eq!(quality::score(&text), 0.0);
    }

    #[test]
    fn a_scanned_page_reports_the_density_of_its_image() {
        // 1240 x 1754 pixels on an A4 sheet: 150 DPI.
        let dpi = open_text("invoice_scan_en.pdf")
            .page_image_density(1)
            .unwrap()
            * 72.0;
        assert!((dpi - 150.0).abs() < 1.0, "{dpi}");

        let page = crate::extract::clean::page(
            ::image::RgbImage::from_pixel(2480, 3508, ::image::Rgb([250, 250, 250])),
            72.0 / 300.0,
        )
        .unwrap();
        let pdf = crate::extract::clean::pdf(&[page]);
        let dpi = OxideExtractor
            .open(&pdf)
            .unwrap()
            .page_image_density(1)
            .unwrap()
            * 72.0;
        assert!((dpi - 300.0).abs() < 1.0, "{dpi}");
    }

    #[test]
    fn a_page_that_is_not_one_image_has_no_density() {
        let doc = open_text("contract_ru_digital.pdf");
        assert_eq!(doc.page_image_density(1), None);
        assert_eq!(doc.page_image_density(0), None);
        assert_eq!(doc.page_image_density(99), None);
    }

    #[test]
    fn multi_page_statement_reports_every_page() {
        let doc = open_text("statement_multi_page.pdf");
        assert_eq!(doc.page_count(), 12);
        assert!(doc.page_text(12).is_ok());
        assert_eq!(doc.page_text(0).unwrap_err().code, "page_out_of_range");
        assert_eq!(doc.page_text(13).unwrap_err().code, "page_out_of_range");
    }

    #[test]
    fn garbage_text_layer_scores_low_while_the_image_stays_readable() {
        let doc = open_text("garbage_text_layer.pdf");
        let text = doc.page_text(1).unwrap();
        assert!(
            !quality::is_good(quality::score(&text)),
            "score {} for {text:?}",
            quality::score(&text)
        );
    }

    #[test]
    fn encrypted_pdf_fails_cleanly() {
        let data = bytes("encrypted.pdf");
        assert_eq!(
            OxideExtractor.open(&data).err().unwrap().code,
            "pdf_encrypted"
        );
        assert_eq!(
            HayroRenderer.open(&data).err().unwrap().code,
            "pdf_encrypted"
        );
    }

    #[test]
    fn corrupt_pdf_fails_cleanly() {
        // Truncated mid-stream: both parsers may legitimately recover the
        // first page, but neither may panic, and what they return is sane.
        let truncated = bytes("corrupt.pdf");
        if let Ok(doc) = OxideExtractor.open(&truncated) {
            let _ = doc.page_text(1);
        }
        if let Ok(doc) = HayroRenderer.open(&truncated) {
            let _ = doc.render(1, 50);
        }

        // Unrecoverable input is a clean `pdf_corrupt`.
        let mut junk = b"%PDF-1.7
"
        .to_vec();
        junk.extend((0..2000u32).map(|i| (i * 31 % 251) as u8));
        for data in [&junk[..], b"", b"%PDF-1.7", b"not a pdf at all"] {
            assert_eq!(OxideExtractor.open(data).err().unwrap().code, "pdf_corrupt");
            assert_eq!(HayroRenderer.open(data).err().unwrap().code, "pdf_corrupt");
        }
    }

    #[test]
    fn render_produces_a_page_sized_bitmap_with_ink() {
        let doc = HayroRenderer
            .open(&bytes("contract_ru_digital.pdf"))
            .unwrap();
        let image = doc.render(1, 100).unwrap();
        // A4 at 100 dpi is about 827 x 1169.
        assert!((780..=880).contains(&image.width()), "{}", image.width());
        assert!(
            (1100..=1250).contains(&image.height()),
            "{}",
            image.height()
        );
        let dark = image.pixels().filter(|p| p.0[0] < 100).count();
        assert!(dark > 500, "page looks blank ({dark} dark pixels)");
    }

    #[test]
    fn render_of_a_scanned_pdf_is_not_blank() {
        let doc = HayroRenderer.open(&bytes("invoice_scan_en.pdf")).unwrap();
        let image = doc.render(1, 72).unwrap();
        let dark = image.pixels().filter(|p| p.0[0] < 100).count();
        assert!(dark > 200, "scan rendered blank ({dark} dark pixels)");
    }

    #[test]
    fn rendering_caps_huge_requests() {
        let doc = HayroRenderer
            .open(&bytes("contract_ru_digital.pdf"))
            .unwrap();
        let image = doc.render(1, 20_000).unwrap();
        assert!(u64::from(image.width()) * u64::from(image.height()) <= 100_000_000);
    }

    /// A valid, blank PDF with `count` pages.
    fn blank_pdf(count: u32) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        let kids: String = (0..count).map(|i| format!("{} 0 R ", 3 + i)).collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Kids [{kids}] /Count {count} >>"),
        ];
        objects.extend(
            (0..count)
                .map(|_| "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_string()),
        );
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        let size = objects.len() + 1;
        out.extend(format!("xref\n0 {size}\n0000000000 65535 f \n").bytes());
        for offset in offsets {
            out.extend(format!("{offset:010} 00000 n \n").bytes());
        }
        out.extend(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").bytes(),
        );
        out
    }

    #[test]
    fn a_pdf_over_the_page_limit_is_refused() {
        let at_limit = blank_pdf(MAX_PAGES);
        assert!(OxideExtractor.open(&at_limit).is_ok());
        assert!(HayroRenderer.open(&at_limit).is_ok());
        let over = blank_pdf(MAX_PAGES + 1);
        assert_eq!(
            OxideExtractor.open(&over).err().unwrap().code,
            "too_many_pages"
        );
        assert_eq!(
            HayroRenderer.open(&over).err().unwrap().code,
            "too_many_pages"
        );
    }

    #[test]
    fn effective_ocr_dpi_caps_to_native_density_with_floor() {
        assert_eq!(effective_ocr_dpi(300, None), 300);
        assert_eq!(effective_ocr_dpi(300, Some(0.0)), 300);
        assert_eq!(effective_ocr_dpi(300, Some(f32::NAN)), 300);
        assert_eq!(effective_ocr_dpi(300, Some(150.0 / 72.0)), 150);
        assert_eq!(effective_ocr_dpi(300, Some(100.0 / 72.0)), 150);
        assert_eq!(effective_ocr_dpi(300, Some(220.0 / 72.0)), 220);
        assert_eq!(effective_ocr_dpi(300, Some(600.0 / 72.0)), 300);
        assert_eq!(effective_ocr_dpi(110, Some(72.0 / 72.0)), 110);
    }
}
