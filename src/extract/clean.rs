//! The clean copy of a scan: each page upright, cut out of the sheet,
//! straightened and trimmed, encoded as JPEG and bound into one PDF.
//!
//! The PDF is written by hand: one image per page, drawn over the whole page,
//! nothing else. That is all a scan needs, and it keeps a PDF library out of
//! the dependency tree.

use std::fmt::Write as _;
use std::io::Cursor;

use ::image::codecs::jpeg::JpegEncoder;
use ::image::{DynamicImage, GrayImage, RgbImage, imageops};

use crate::error::{Result, ServiceError};

/// Longest edge of a clean page: A4 at 300 DPI, as the page is read.
pub const MAX_EDGE: u32 = 3508;
/// JPEG quality of a clean page. Text stays crisp; a page of a few hundred KB.
const QUALITY: u8 = 82;
/// A colour page whose pixels stray at most this far from gray is stored as
/// grayscale, a third of the size.
const GRAY_TOLERANCE: u8 = 16;
/// Share of sampled pixels that may stray further (dust, a coloured stamp
/// speck) on a page still stored as gray.
const COLOUR_SHARE: f32 = 0.002;

/// One page of the clean copy.
pub struct Page {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub gray: bool,
    /// Page size in PDF points (1/72 inch).
    pub width_pt: f32,
    pub height_pt: f32,
}

fn failed() -> ServiceError {
    ServiceError::internal("clean_copy_failed", "could not build the clean copy")
}

/// A render this much denser than the source's own pixels is scaled back down;
/// a few percent over is rounding, not interpolation.
const DENSITY_SLACK: f32 = 1.03;

/// `image` (`pt_per_px` points per pixel) scaled down to the source's own pixel
/// density, `native_px_per_pt` pixels per point, when it was rendered denser.
/// A page drawn at 300 DPI from a 150 DPI scan holds four times the pixels and
/// not a pixel more detail. Never scales up. Returns the image and its new
/// points per pixel.
pub fn at_native_density(
    image: RgbImage,
    pt_per_px: f32,
    native_px_per_pt: f32,
) -> (RgbImage, f32) {
    let density = 1.0 / pt_per_px;
    if !(native_px_per_pt > 0.0 && density > native_px_per_pt * DENSITY_SLACK) {
        return (image, pt_per_px);
    }
    let factor = native_px_per_pt / density;
    let (w, h) = image.dimensions();
    let (nw, nh) = (
        ((w as f32 * factor).round() as u32).max(1),
        ((h as f32 * factor).round() as u32).max(1),
    );
    let scaled = imageops::thumbnail(&image, nw, nh);
    // The page keeps its size in points whatever rounding did to the pixels.
    let pt_per_px = pt_per_px * w as f32 / nw as f32;
    (scaled, pt_per_px)
}

/// Encodes `image`, `pt_per_px` points per pixel, as one clean page; scaled
/// down (never up) to [`MAX_EDGE`].
pub fn page(image: RgbImage, pt_per_px: f32) -> Result<Page> {
    let (w, h) = image.dimensions();
    let longest = w.max(h);
    let (image, pt_per_px) = if longest > MAX_EDGE {
        let scaled = DynamicImage::ImageRgb8(image)
            .thumbnail(MAX_EDGE, MAX_EDGE)
            .to_rgb8();
        let factor = longest as f32 / scaled.width().max(scaled.height()) as f32;
        (scaled, pt_per_px * factor)
    } else {
        (image, pt_per_px)
    };
    let (width, height) = image.dimensions();
    let gray = is_gray(&image);
    let mut jpeg = Vec::new();
    let encoder = JpegEncoder::new_with_quality(Cursor::new(&mut jpeg), QUALITY);
    let encoded = if gray {
        let luma: GrayImage = DynamicImage::ImageRgb8(image).to_luma8();
        luma.write_with_encoder(encoder)
    } else {
        image.write_with_encoder(encoder)
    };
    encoded.map_err(|e| {
        tracing::error!(error = %e, "JPEG encoding failed");
        failed()
    })?;
    Ok(Page {
        jpeg,
        width,
        height,
        gray,
        width_pt: width as f32 * pt_per_px,
        height_pt: height as f32 * pt_per_px,
    })
}

/// True when nearly every pixel of `image` is a shade of gray.
fn is_gray(image: &RgbImage) -> bool {
    let (w, h) = image.dimensions();
    // About 40 000 samples, whatever the size.
    let step = ((u64::from(w) * u64::from(h) / 40_000) as f64)
        .sqrt()
        .max(1.0) as u32;
    let (mut seen, mut coloured) = (0u32, 0u32);
    for y in (0..h).step_by(step as usize) {
        for x in (0..w).step_by(step as usize) {
            let [r, g, b] = image.get_pixel(x, y).0;
            let spread = r.max(g).max(b) - r.min(g).min(b);
            seen += 1;
            if spread > GRAY_TOLERANCE {
                coloured += 1;
            }
        }
    }
    seen > 0 && (coloured as f32) <= seen as f32 * COLOUR_SHARE
}

/// `image` turned by `degrees` clockwise around its centre on a canvas of the
/// same size, corners filled with the shade of its own corners; then cropped
/// to `crop` (x, y, width, height). The colour twin of what
/// [`super::preprocess::straighten`] does to the gray copy.
pub fn straighten(image: &RgbImage, degrees: f32, crop: (u32, u32, u32, u32)) -> RgbImage {
    let rotated;
    let source = if degrees == 0.0 {
        image
    } else {
        let (w, h) = image.dimensions();
        let corners =
            [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)].map(|(x, y)| image.get_pixel(x, y).0);
        let fill =
            [0, 1, 2].map(|c| (corners.iter().map(|p| u32::from(p[c])).sum::<u32>() / 4) as u8);
        rotated = rotate(image, degrees, fill);
        &rotated
    };
    let (x, y, w, h) = crop;
    let (sw, sh) = source.dimensions();
    if (x, y, w, h) == (0, 0, sw, sh) {
        return source.clone();
    }
    imageops::crop_imm(
        source,
        x.min(sw),
        y.min(sh),
        w.min(sw - x.min(sw)),
        h.min(sh - y.min(sh)),
    )
    .to_image()
}

fn rotate(image: &RgbImage, degrees: f32, fill: [u8; 3]) -> RgbImage {
    let (w, h) = image.dimensions();
    let pixels = super::preprocess::rotate_pixels(image.as_raw(), w, h, degrees, fill);
    RgbImage::from_raw(w, h, pixels).expect("a buffer of the image's size")
}

/// A PDF of `pages`, each image filling its page.
pub fn pdf(pages: &[Page]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    // Objects: 1 catalog, 2 page tree, 3 info, then per page: page, image, content.
    let count = 3 + pages.len() * 3;
    let mut offsets = vec![0usize; count + 1];
    let page_obj = |i: usize| 4 + i * 3;

    let mut object = |out: &mut Vec<u8>, n: usize, body: &[u8]| {
        offsets[n] = out.len();
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };

    object(&mut out, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let mut kids = String::new();
    for i in 0..pages.len() {
        let _ = write!(kids, "{} 0 R ", page_obj(i));
    }
    object(
        &mut out,
        2,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.trim_end(),
            pages.len()
        )
        .as_bytes(),
    );
    object(&mut out, 3, b"<< /Producer (paperjam) >>");

    for (i, page) in pages.iter().enumerate() {
        let (p, img, content) = (page_obj(i), page_obj(i) + 1, page_obj(i) + 2);
        let (w, h) = (round2(page.width_pt), round2(page.height_pt));
        object(
            &mut out,
            p,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] \
                 /Resources << /XObject << /Im0 {img} 0 R >> >> /Contents {content} 0 R >>"
            )
            .as_bytes(),
        );
        let mut image = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /{} \
             /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            page.width,
            page.height,
            if page.gray { "DeviceGray" } else { "DeviceRGB" },
            page.jpeg.len()
        )
        .into_bytes();
        image.extend_from_slice(&page.jpeg);
        image.extend_from_slice(b"\nendstream");
        object(&mut out, img, &image);
        let draw = format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q");
        object(
            &mut out,
            content,
            format!("<< /Length {} >>\nstream\n{draw}\nendstream", draw.len()).as_bytes(),
        );
    }

    let xref = out.len();
    let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", count + 1);
    for offset in &offsets[1..] {
        let _ = writeln!(table, "{offset:010} 00000 n ");
    }
    let _ = write!(
        table,
        "trailer\n<< /Size {} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        count + 1
    );
    out.extend_from_slice(table.as_bytes());
    out
}

/// A number for a PDF: at most two decimals, no exponent.
fn round2(v: f32) -> String {
    let s = format!("{:.2}", v.max(1.0));
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{HayroRenderer, PageRenderer};
    use ::image::Rgb;

    fn sheet(w: u32, h: u32, colour: bool) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| {
            if (x / 20 + y / 20) % 2 == 0 {
                if colour {
                    Rgb([200, 30, 30])
                } else {
                    Rgb([40, 40, 40])
                }
            } else {
                Rgb([250, 250, 250])
            }
        })
    }

    #[test]
    fn a_built_pdf_opens_and_renders_with_its_pages() {
        let pages = vec![
            page(sheet(620, 877, false), 72.0 / 75.0).unwrap(),
            page(sheet(400, 250, true), 72.0 / 150.0).unwrap(),
        ];
        assert!(pages[0].gray && !pages[1].gray);
        let bytes = pdf(&pages);
        let opened = HayroRenderer.open(&bytes).unwrap();
        assert_eq!(opened.page_count(), 2);
        // Page 2 is 400 x 250 px at 150 DPI: 192 x 120 pt; at 150 DPI the
        // render is the image's own size again (give or take a pixel of rounding).
        let second = opened.render(2, 150).unwrap();
        let (w, h) = second.dimensions();
        assert!(w.abs_diff(400) <= 1 && h.abs_diff(250) <= 1, "{w} x {h}");
        let px = second.get_pixel(5, 5).0;
        assert!(px[0] > 150 && px[1] < 90, "colour survives: {px:?}");
    }

    #[test]
    fn pages_larger_than_a4_at_300_dpi_are_scaled_down_keeping_their_size() {
        let p = page(sheet(MAX_EDGE * 2, 200, false), 0.5).unwrap();
        assert_eq!((p.width, p.height), (MAX_EDGE, 100));
        assert!((p.width_pt - MAX_EDGE as f32).abs() < 1.0, "{}", p.width_pt);
    }

    #[test]
    fn a_render_denser_than_its_source_is_scaled_back_keeping_the_page_size() {
        // A4 at 300 DPI from a 150 DPI scan.
        let (w, h) = (2480, 3508);
        let (image, pt_per_px) = at_native_density(sheet(w, h, false), 72.0 / 300.0, 150.0 / 72.0);
        assert_eq!(image.dimensions(), (1240, 1754));
        let p = page(image, pt_per_px).unwrap();
        assert!((p.width_pt - 595.2).abs() < 0.5, "{}", p.width_pt);
        assert!((p.height_pt - 841.92).abs() < 0.5, "{}", p.height_pt);
    }

    #[test]
    fn a_render_at_the_sources_density_or_below_is_left_alone() {
        for native_dpi in [300.0, 299.0, 600.0] {
            let (image, pt_per_px) =
                at_native_density(sheet(300, 200, false), 72.0 / 300.0, native_dpi / 72.0);
            assert_eq!(image.dimensions(), (300, 200), "{native_dpi}");
            assert_eq!(pt_per_px, 72.0 / 300.0);
        }
        // An unknown density is no reason to scale.
        let (image, _) = at_native_density(sheet(300, 200, false), 0.24, 0.0);
        assert_eq!(image.dimensions(), (300, 200));
    }

    #[test]
    fn a_tilted_page_is_turned_back_and_cropped() {
        let mut image = RgbImage::from_pixel(300, 200, Rgb([255, 255, 255]));
        for x in 50..250 {
            for y in 90..110 {
                image.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        let same = straighten(&image, 0.0, (0, 0, 300, 200));
        assert_eq!(same, image);
        let cropped = straighten(&image, 3.0, (40, 80, 220, 40));
        assert_eq!(cropped.dimensions(), (220, 40));
        // An out-of-range crop is clamped to the image.
        assert_eq!(
            straighten(&image, 0.0, (250, 150, 500, 500)).dimensions(),
            (50, 50)
        );
    }

    #[test]
    fn numbers_are_written_plainly() {
        assert_eq!(round2(595.276), "595.28");
        assert_eq!(round2(842.0), "842");
        assert_eq!(round2(0.0), "1");
    }
}
