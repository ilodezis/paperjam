//! Image decoding with EXIF orientation, size guards and thumbnails.

use std::io::Cursor;

use ::image::imageops::FilterType;
use ::image::{DynamicImage, GenericImageView, ImageDecoder, ImageReader};

use crate::error::{Result, ServiceError};

/// Images with more pixels than this are refused before decoding.
pub const MAX_PIXELS: u64 = 100_000_000;

/// Decodes `bytes` (a format the `image` crate reads, or HEIC with the
/// `heic` feature), applying the EXIF orientation so the result is upright.
pub fn decode(bytes: &[u8], mime: &str) -> Result<DynamicImage> {
    if matches!(mime, "image/heic" | "image/heif") {
        return decode_heic(bytes);
    }
    let corrupt = |e: &dyn std::fmt::Display| {
        tracing::debug!(error = %e, "image decode failed");
        ServiceError::invalid("image_corrupt", "the image could not be read")
    };
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| corrupt(&e))?;
    let mut decoder = reader.into_decoder().map_err(|e| corrupt(&e))?;
    let (width, height) = decoder.dimensions();
    check_size(width, height)?;
    let orientation = decoder
        .orientation()
        .unwrap_or(::image::metadata::Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| corrupt(&e))?;
    image.apply_orientation(orientation);
    Ok(image)
}

/// `image` scaled down to at most `max_edge` along its longest side. Area
/// averaging in integers: `resize` would hold the image in 32-bit floats, a
/// quarter of a gigabyte for a 48 MP photo.
pub fn fit(image: DynamicImage, max_edge: u32) -> DynamicImage {
    if image.width().max(image.height()) > max_edge {
        image.thumbnail(max_edge, max_edge)
    } else {
        image
    }
}

fn check_size(width: u32, height: u32) -> Result<()> {
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(ServiceError::invalid(
            "image_too_large",
            "the image has more than 100 megapixels",
        ));
    }
    Ok(())
}

#[cfg(not(feature = "heic"))]
fn decode_heic(_bytes: &[u8]) -> Result<DynamicImage> {
    Err(ServiceError::invalid(
        "heic_unsupported",
        "this build cannot read HEIC images",
    ))
}

#[cfg(feature = "heic")]
fn decode_heic(bytes: &[u8]) -> Result<DynamicImage> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};

    let corrupt = |e: libheif_rs::HeifError| {
        tracing::debug!(error = %e, "heic decode failed");
        ServiceError::invalid("image_corrupt", "the image could not be read")
    };
    let context = HeifContext::read_from_bytes(bytes).map_err(corrupt)?;
    let handle = context.primary_image_handle().map_err(corrupt)?;
    check_size(handle.width(), handle.height())?;
    let heif = LibHeif::new();
    // libheif applies the orientation transforms itself while decoding.
    let decoded = heif
        .decode(&handle, ColorSpace::Rgb(RgbChroma::Rgb), None)
        .map_err(corrupt)?;
    let plane = decoded
        .planes()
        .interleaved
        .ok_or_else(|| ServiceError::invalid("image_corrupt", "the image could not be read"))?;
    let (width, height) = (plane.width, plane.height);
    let row_bytes = width as usize * 3;
    let mut pixels = Vec::with_capacity(row_bytes * height as usize);
    for row in plane.data.chunks(plane.stride).take(height as usize) {
        pixels.extend_from_slice(&row[..row_bytes]);
    }
    ::image::RgbImage::from_raw(width, height, pixels)
        .map(DynamicImage::ImageRgb8)
        .ok_or_else(|| ServiceError::invalid("image_corrupt", "the image could not be read"))
}

/// Longest edge of a thumbnail. The list shows them as small cards.
pub const THUMBNAIL_EDGE: u32 = 320;
/// libwebp qualities (0-100) tried in turn until the result fits its size
/// budget; the first one is what an ordinary page gets. Text stays legible at
/// these, and the renders also feed the LLM and `get_page_image`.
const THUMBNAIL_QUALITIES: [f32; 3] = [60.0, 45.0, 30.0];
const RENDER_QUALITIES: [f32; 4] = [75.0, 65.0, 55.0, 45.0];
/// Sizes in bytes: a thumbnail takes a second or less on a slow link, a page
/// render a few seconds.
const THUMBNAIL_BUDGET: usize = 20_000;
const RENDER_BUDGET: usize = 150_000;

/// A WebP thumbnail with the longest edge at most [`THUMBNAIL_EDGE`], from the
/// part of the page that holds content: a passport lying in a corner of an A4
/// sheet becomes a picture of the passport.
pub fn thumbnail(image: &DynamicImage) -> Result<Vec<u8>> {
    let cropped;
    let image = match content_box(image) {
        Some((x, y, w, h)) => {
            cropped = image.crop_imm(x, y, w, h);
            &cropped
        }
        None => image,
    };
    Ok(lossy(
        image,
        THUMBNAIL_EDGE,
        &THUMBNAIL_QUALITIES,
        THUMBNAIL_BUDGET,
    )?
    .bytes)
}

/// The margin search looks at a copy at most this long on its longest edge.
const ANALYSIS_EDGE: u32 = 600;
/// A pixel of the analysis copy darker or lighter than the paper by more than
/// this is ink (the copy is averaged, so scanner noise stays far below it).
const INK_CONTRAST: u8 = 10;
/// Ink is grouped into pieces of content in cells this many pixels wide: ink
/// closer together than a cell belongs to one piece (a card, a block of text).
const JOIN_CELL: u32 = 6;
/// A piece with less ink than this share of the biggest piece is dust or a
/// scanner streak, not content, and does not hold the margin open.
const MIN_PIECE_SHARE: f64 = 0.05;
/// Pages whose biggest piece has less ink than this (analysis pixels) are blank.
const MIN_CONTENT_INK: u64 = 40;
/// Margin kept around the content, as a share of its longer side.
const PADDING_SHARE: f32 = 0.03;
const MIN_PADDING: u32 = 12;
/// A box covering more of the page than this is not worth cropping to.
const MAX_BOX_SHARE: f32 = 0.9;

/// `(x, y, width, height)` of the part of `image` that holds anything other
/// than paper, with a small margin, or `None` when there is no margin worth
/// cutting (a blank page, a full-page photo, a box that fills the page).
/// Faint ink counts as content; only the uniform paper around it is dropped.
pub fn content_box(image: &DynamicImage) -> Option<(u32, u32, u32, u32)> {
    content_box_within(image, MAX_BOX_SHARE)
}

/// [`content_box`] with its own limit: `None` unless the content covers at
/// most `max_share` of the page. A card lying on an A4 sheet is cut out with
/// a small limit; the thumbnail only drops plain margins.
pub fn content_box_within(image: &DynamicImage, max_share: f32) -> Option<(u32, u32, u32, u32)> {
    let (width, height) = image.dimensions();
    let longest = width.max(height);
    if longest == 0 {
        return None;
    }
    let scale = (ANALYSIS_EDGE as f32 / longest as f32).min(1.0);
    let small_width = ((width as f32 * scale).round() as u32).max(1);
    let small_height = ((height as f32 * scale).round() as u32).max(1);
    let small = image
        .resize_exact(small_width, small_height, FilterType::Triangle)
        .to_luma8();

    // The paper shade is what the outermost ring of pixels mostly is.
    let mut ring: Vec<u8> = (0..small_width)
        .flat_map(|x| [(x, 0), (x, small_height - 1)])
        .chain((0..small_height).flat_map(|y| [(0, y), (small_width - 1, y)]))
        .map(|(x, y)| small.get_pixel(x, y).0[0])
        .collect();
    ring.sort_unstable();
    let paper = ring[ring.len() / 2];

    let cols = small_width.div_ceil(JOIN_CELL) as usize;
    let rows = small_height.div_ceil(JOIN_CELL) as usize;
    let mut cells = vec![0u64; cols * rows];
    // Bounds of the ink per cell: (left, top, right, bottom).
    let mut bounds = vec![(u32::MAX, u32::MAX, 0u32, 0u32); cols * rows];
    for (x, y, pixel) in small.enumerate_pixels() {
        if pixel.0[0].abs_diff(paper) > INK_CONTRAST {
            let cell = (y / JOIN_CELL) as usize * cols + (x / JOIN_CELL) as usize;
            cells[cell] += 1;
            let b = &mut bounds[cell];
            *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
        }
    }

    // Pieces of content: 8-connected groups of cells that hold ink, each with
    // its amount of ink and bounds.
    let mut seen = vec![false; cells.len()];
    let mut pieces: Vec<(u64, (u32, u32, u32, u32))> = Vec::new();
    for start in 0..cells.len() {
        if cells[start] == 0 || seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![start];
        let mut piece = (0u64, (u32::MAX, u32::MAX, 0u32, 0u32));
        while let Some(cell) = stack.pop() {
            piece.0 += cells[cell];
            let (b, p) = (bounds[cell], &mut piece.1);
            *p = (p.0.min(b.0), p.1.min(b.1), p.2.max(b.2), p.3.max(b.3));
            let (cx, cy) = ((cell % cols) as i64, (cell / cols) as i64);
            for (dx, dy) in [
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ] {
                let (nx, ny) = (cx + dx, cy + dy);
                if nx < 0 || ny < 0 || nx >= cols as i64 || ny >= rows as i64 {
                    continue;
                }
                let next = ny as usize * cols + nx as usize;
                if cells[next] > 0 && !seen[next] {
                    seen[next] = true;
                    stack.push(next);
                }
            }
        }
        pieces.push(piece);
    }
    let biggest = pieces.iter().map(|p| p.0).max()?;
    if biggest < MIN_CONTENT_INK {
        return None;
    }
    let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
    for (ink, b) in &pieces {
        if (*ink as f64) < biggest as f64 * MIN_PIECE_SHARE {
            continue;
        }
        left = left.min(b.0);
        top = top.min(b.1);
        right = right.max(b.2 + 1);
        bottom = bottom.max(b.3 + 1);
    }

    let to_page = |v: u32| (v as f32 / scale).round() as u32;
    let (mut x0, mut x1) = (to_page(left), to_page(right).min(width));
    let (mut y0, mut y1) = (to_page(top), to_page(bottom).min(height));
    let padding = (((x1 - x0).max(y1 - y0) as f32 * PADDING_SHARE) as u32).max(MIN_PADDING);
    x0 = x0.saturating_sub(padding);
    y0 = y0.saturating_sub(padding);
    x1 = (x1 + padding).min(width);
    y1 = (y1 + padding).min(height);

    let area = f64::from(x1 - x0) * f64::from(y1 - y0);
    if area >= f64::from(max_share) * f64::from(width) * f64::from(height) {
        return None;
    }
    Some((x0, y0, x1 - x0, y1 - y0))
}

/// An encoded WebP image and its pixel size.
pub struct Webp {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Encodes `image` as lossy WebP, scaled down (never up) so that the longest
/// edge is at most `max_edge`.
pub fn webp(image: &DynamicImage, max_edge: u32) -> Result<Webp> {
    lossy(image, max_edge, &RENDER_QUALITIES, RENDER_BUDGET)
}

/// Encodes at the first of `qualities` whose output fits `budget` (the last
/// one is used when none does).
fn lossy(image: &DynamicImage, max_edge: u32, qualities: &[f32], budget: usize) -> Result<Webp> {
    let failed = |what: &str| {
        tracing::error!(what, "WebP encoding failed");
        ServiceError::internal("thumbnail_failed", "could not create a thumbnail")
    };
    let resized;
    let fitted = if image.width().max(image.height()) > max_edge {
        resized = image.thumbnail(max_edge, max_edge);
        &resized
    } else {
        image
    };
    // The encoder takes 8-bit RGB; borrow directly when already RGB8.
    let converted;
    let rgb = match fitted.as_rgb8() {
        Some(rgb) => rgb,
        None => {
            converted = fitted.to_rgb8();
            &converted
        }
    };
    let started = std::time::Instant::now();
    let mut bytes = Vec::new();
    let mut tried = 0_u32;
    for (idx, &quality) in qualities.iter().enumerate() {
        tried += 1;
        let mut config = ::webp::WebPConfig::new().map_err(|()| failed("config"))?;
        config.quality = quality;
        // Fast encoding handles normal pages; the last-resort step uses stronger
        // compression so noisy scans still fit within the byte budget.
        config.method = if idx + 1 == qualities.len() { 4 } else { 2 };
        config.use_sharp_yuv = 1;
        bytes = ::webp::Encoder::from_rgb(rgb.as_raw(), rgb.width(), rgb.height())
            .encode_advanced(&config)
            .map_err(|e| failed(&format!("{e:?}")))?
            .to_vec();
        if bytes.len() <= budget {
            break;
        }
    }
    tracing::debug!(
        width = rgb.width(),
        height = rgb.height(),
        tried,
        bytes = bytes.len(),
        ms = started.elapsed().as_millis() as u64,
        "webp encoded"
    );
    Ok(Webp {
        bytes,
        width: rgb.width(),
        height: rgb.height(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::fixture;
    use ::image::{ImageFormat, RgbImage};

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut crc = 0xFFFF_FFFFu32;
            for &byte in data {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = if crc & 1 == 1 {
                        (crc >> 1) ^ 0xEDB8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            let mut out = (data.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc32(&body).to_be_bytes());
            out
        }
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend(chunk(b"IHDR", &ihdr));
        // An empty (but valid) zlib stream: the header parses, no pixels follow.
        png.extend(chunk(
            b"IDAT",
            &[0x78, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        ));
        png.extend(chunk(b"IEND", &[]));
        png
    }

    #[test]
    fn image_bomb_rejected() {
        let bomb = png_header(30_000, 30_000);
        let err = decode(&bomb, "image/png").unwrap_err();
        assert_eq!(err.code, "image_too_large");
    }

    #[test]
    fn exactly_the_limit_is_not_a_bomb_but_corrupt_data_is_reported() {
        let header_only = png_header(10_000, 10_000);
        let err = decode(&header_only, "image/png").unwrap_err();
        assert_eq!(err.code, "image_corrupt");
    }

    #[test]
    fn garbage_bytes_are_corrupt() {
        let err = decode(b"definitely not an image", "image/png").unwrap_err();
        assert_eq!(err.code, "image_corrupt");
    }

    #[cfg(not(feature = "heic"))]
    #[test]
    fn heic_without_the_feature_fails_cleanly() {
        let err = decode(b"whatever", "image/heic").unwrap_err();
        assert_eq!(err.code, "heic_unsupported");
    }

    #[cfg(feature = "heic")]
    #[test]
    fn heic_decodes() {
        let bytes = std::fs::read(fixture("photo.heic")).unwrap();
        let image = decode(&bytes, "image/heic").unwrap();
        assert!(image.width() > 0 && image.height() > 0);
    }

    #[test]
    fn exif_rotation_applied() {
        // The camera stored the receipt sideways (EXIF orientation 6); after
        // decoding it must be upright, i.e. portrait.
        let bytes = std::fs::read(fixture("receipt_photo_skewed.jpg")).unwrap();
        let image = decode(&bytes, "image/jpeg").unwrap();
        let (w, h) = image.dimensions();
        assert!(h > w, "expected portrait, got {w}x{h}");
    }

    #[test]
    fn thumbnails_are_webp_and_bounded() {
        let big = DynamicImage::ImageRgb8(RgbImage::from_pixel(
            2000,
            1000,
            ::image::Rgb([200, 30, 30]),
        ));
        let bytes = thumbnail(&big).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
        let decoded = ::image::load_from_memory_with_format(&bytes, ImageFormat::WebP).unwrap();
        assert_eq!(decoded.dimensions(), (THUMBNAIL_EDGE, THUMBNAIL_EDGE / 2));
    }

    /// A deterministic pseudo-random byte stream for scanner noise.
    struct Noise(u64);

    impl Noise {
        fn next(&mut self, spread: i32) -> i32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % (2 * spread as u64 + 1)) as i32 - spread
        }
    }

    /// A white A4 sheet (150 dpi) with scanner noise and a block of text-like
    /// stripes (grey `ink`) in `block` = `(x, y, width, height)`.
    fn sheet_with_block(block: (u32, u32, u32, u32), ink: u8) -> DynamicImage {
        let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
        let image = RgbImage::from_fn(1240, 1754, |x, y| {
            let (bx, by, bw, bh) = block;
            let inside = (bx..bx + bw).contains(&x) && (by..by + bh).contains(&y);
            let base = if inside && (x / 3 + y / 5) % 2 == 0 {
                i32::from(ink)
            } else {
                250
            };
            let v = (base + noise.next(3)).clamp(0, 255) as u8;
            ::image::Rgb([v, v, v])
        });
        DynamicImage::ImageRgb8(image)
    }

    #[test]
    fn margins_around_a_document_are_cropped_without_cutting_into_it() {
        let block = (700, 150, 420, 300);
        let (x, y, w, h) = content_box(&sheet_with_block(block, 20)).expect("a margin to crop");
        assert!(x <= block.0 && y <= block.1, "box starts at {x},{y}");
        assert!(x + w >= block.0 + block.2 && y + h >= block.1 + block.3);
        // Tight: padding only, not the sheet.
        assert!(w <= block.2 + 100 && h <= block.3 + 100, "box is {w}x{h}");
    }

    #[test]
    fn faint_ink_is_content_too() {
        // Light grey text, 25 levels darker than the paper.
        let block = (200, 1200, 600, 200);
        let (x, y, w, h) = content_box(&sheet_with_block(block, 225)).expect("a margin to crop");
        assert!(x <= block.0 && y <= block.1);
        assert!(x + w >= block.0 + block.2 && y + h >= block.1 + block.3);
    }

    #[test]
    fn dust_does_not_hold_the_margins_open() {
        let mut sheet = sheet_with_block((700, 150, 420, 300), 20).to_rgb8();
        for (x, y) in [(5, 5), (1230, 1740), (20, 1700)] {
            for (dx, dy) in [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)] {
                sheet.put_pixel(x + dx, y + dy, ::image::Rgb([0, 0, 0]));
            }
        }
        let (_, _, w, h) = content_box(&DynamicImage::ImageRgb8(sheet)).expect("a margin");
        assert!(w < 600 && h < 450, "box is {w}x{h}");
    }

    #[test]
    fn a_page_without_margins_or_without_content_is_left_alone() {
        let full = sheet_with_block((10, 10, 1220, 1734), 20);
        assert_eq!(content_box(&full), None);
        let blank = DynamicImage::ImageRgb8(RgbImage::from_pixel(600, 800, ::image::Rgb([255; 3])));
        assert_eq!(content_box(&blank), None);
    }

    /// The passport lying in the corner of an A4 sheet, turned upright.
    fn scanned_sheet() -> DynamicImage {
        ::image::open(fixture("passport_td3_sideways_a4.png"))
            .unwrap()
            .rotate270()
    }

    #[test]
    fn a_thumbnail_of_a_scan_is_a_few_kilobytes() {
        let bytes = thumbnail(&scanned_sheet()).unwrap();
        assert!(bytes.len() <= 20_000, "thumbnail is {} bytes", bytes.len());
        let decoded = ::image::load_from_memory_with_format(&bytes, ImageFormat::WebP).unwrap();
        // The corner of the sheet, not the whole sheet.
        assert!(
            decoded.width() > decoded.height(),
            "{:?}",
            decoded.dimensions()
        );
    }

    #[test]
    fn a_page_render_is_light_and_keeps_the_text_readable() {
        let sheet = scanned_sheet();
        let render = webp(&sheet, 1600).unwrap();
        assert!(
            render.bytes.len() <= 150_000,
            "{} bytes",
            render.bytes.len()
        );
        assert_eq!((render.width, render.height), (1131, 1600));
        // Quality loss stays small: the mean error is a couple of levels.
        let decoded = ::image::load_from_memory_with_format(&render.bytes, ImageFormat::WebP)
            .unwrap()
            .to_luma8();
        let original = sheet.thumbnail(1600, 1600).to_luma8();
        let total: u64 = decoded
            .pixels()
            .zip(original.pixels())
            .map(|(a, b)| u64::from(a.0[0].abs_diff(b.0[0])))
            .sum();
        let mean = total as f64 / f64::from(original.width() * original.height());
        assert!(mean < 3.0, "mean error {mean}");
    }

    /// A page that compresses badly: a dense guilloche-like pattern with a
    /// photo-like blob and scanner noise over the whole sheet.
    fn busy_page() -> DynamicImage {
        let mut noise = Noise(777);
        DynamicImage::ImageRgb8(RgbImage::from_fn(1754, 1240, |x, y| {
            let (fx, fy) = (x as f32, y as f32);
            let wave =
                ((fx * 0.35).sin() * (fy * 0.21).cos() + (fx * 0.11 + fy * 0.17).sin()) * 28.0;
            let blob = (-(((fx - 500.0).powi(2) + (fy - 500.0).powi(2)) / 40_000.0)).exp() * 90.0;
            let v = ((170.0 + wave - blob) as i32 + noise.next(9)).clamp(0, 255) as u8;
            ::image::Rgb([v, v.saturating_sub(10), v.saturating_sub(25)])
        }))
    }

    #[test]
    fn even_a_busy_page_stays_within_the_size_budgets() {
        let page = busy_page();
        let thumb = thumbnail(&page).unwrap();
        assert!(thumb.len() <= 20_000, "thumbnail is {} bytes", thumb.len());
        let render = webp(&page, 1600).unwrap();
        assert!(
            render.bytes.len() <= 150_000,
            "render is {} bytes",
            render.bytes.len()
        );
    }
}
