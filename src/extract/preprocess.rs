//! Image clean-up before OCR: grayscale, deskew, border trimming.

use ::image::{DynamicImage, GrayImage, imageops};

/// Largest skew angle (degrees, either direction) that is corrected.
pub const MAX_SKEW_DEGREES: f32 = 10.0;

/// Grayscale, deskew (projection profile, at most ±10°) and trim uniform
/// borders.
pub fn prepare(img: &DynamicImage) -> DynamicImage {
    prepare_with(img).0
}

/// How [`prepare`] straightened a page: the turn it gave it (clockwise
/// degrees, `0` when none) and the box it kept of the turned page, so that a
/// colour copy can be straightened the same way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Straightening {
    pub degrees: f32,
    /// (x, y, width, height) within the turned page.
    pub crop: (u32, u32, u32, u32),
}

impl Straightening {
    /// True when the page was turned or lost a border.
    pub fn changes(&self, width: u32, height: u32) -> bool {
        self.degrees != 0.0 || self.crop != (0, 0, width, height)
    }
}

/// [`prepare`], also telling how the page was straightened.
pub fn prepare_with(img: &DynamicImage) -> (DynamicImage, Straightening) {
    // A grayscale page is read in place, not copied first.
    let converted;
    let gray: &GrayImage = match img {
        DynamicImage::ImageLuma8(gray) => gray,
        other => {
            converted = other.to_luma8();
            &converted
        }
    };
    let skew = estimate_skew(gray);
    let degrees = if skew.abs() >= MIN_CORRECTION_DEGREES {
        -skew
    } else {
        0.0
    };
    let straightened = (degrees != 0.0).then(|| {
        let (w, h) = gray.dimensions();
        let corners = [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)]
            .map(|(x, y)| u32::from(gray.get_pixel(x, y).0[0]));
        let fill = (corners.iter().sum::<u32>() / 4) as u8;
        rotate(gray, degrees, fill)
    });
    let source = straightened.as_ref().unwrap_or(gray);
    let crop = border_box(source);
    let (x, y, w, h) = crop;
    let trimmed = if crop == (0, 0, source.width(), source.height()) {
        source.clone()
    } else {
        imageops::crop_imm(source, x, y, w, h).to_image()
    };
    (
        DynamicImage::ImageLuma8(trimmed),
        Straightening { degrees, crop },
    )
}

/// Skews smaller than this are not worth the resampling blur.
const MIN_CORRECTION_DEGREES: f32 = 0.3;
const ANGLE_STEP: f32 = 0.25;
/// The skew search works on a copy at most this wide.
const ANALYSIS_WIDTH: u32 = 1000;
/// Rows/columns whose shade varies by at most this much count as uniform.
const UNIFORM_TOLERANCE: u8 = 12;

/// Estimated skew of the text lines in degrees: positive when the lines run
/// downhill to the right (clockwise). Rotating the page by `-skew` straightens
/// it. Returns `0` when no clear peak exists inside `±MAX_SKEW_DEGREES` (a
/// peak on the search boundary means the real skew is out of range).
pub fn estimate_skew(gray: &GrayImage) -> f32 {
    let small = if gray.width() > ANALYSIS_WIDTH {
        let height = (u64::from(gray.height()) * u64::from(ANALYSIS_WIDTH)
            / u64::from(gray.width()))
        .max(1) as u32;
        imageops::resize(gray, ANALYSIS_WIDTH, height, imageops::FilterType::Triangle)
    } else {
        gray.clone()
    };
    let threshold = otsu(&small);
    let ink: Vec<(f32, f32)> = small
        .enumerate_pixels()
        .filter(|(_, _, p)| p.0[0] < threshold)
        .map(|(x, y, _)| (x as f32, y as f32))
        .collect();
    if ink.len() < 50 {
        return 0.0;
    }

    let steps = (MAX_SKEW_DEGREES / ANGLE_STEP).round() as i32;
    let bins = (small.width() + small.height()) as usize + 2;
    let offset = small.width() as f32 * MAX_SKEW_DEGREES.to_radians().sin();
    let mut best = (0_i32, f64::MIN);
    for step in -steps..=steps {
        let (sin, cos) = (step as f32 * ANGLE_STEP).to_radians().sin_cos();
        let mut histogram = vec![0_u32; bins];
        for &(x, y) in &ink {
            let row = (y * cos - x * sin + offset) as usize;
            histogram[row.min(bins - 1)] += 1;
        }
        let score: f64 = histogram.iter().map(|&c| f64::from(c) * f64::from(c)).sum();
        if score > best.1 {
            best = (step, score);
        }
    }
    if best.0.abs() == steps {
        return 0.0;
    }
    best.0 as f32 * ANGLE_STEP
}

/// Otsu's threshold: pixels darker than the result are ink.
fn otsu(gray: &GrayImage) -> u8 {
    let mut histogram = [0_u64; 256];
    for p in gray.pixels() {
        histogram[usize::from(p.0[0])] += 1;
    }
    let total: u64 = histogram.iter().sum();
    let weighted: f64 = histogram
        .iter()
        .enumerate()
        .map(|(i, &c)| i as f64 * c as f64)
        .sum();
    let (mut below, mut below_sum) = (0_u64, 0.0_f64);
    let (mut best, mut best_var) = (128_u8, -1.0_f64);
    for (level, &count) in histogram.iter().enumerate() {
        below += count;
        if below == 0 {
            continue;
        }
        let above = total - below;
        if above == 0 {
            break;
        }
        below_sum += level as f64 * count as f64;
        let mean_below = below_sum / below as f64;
        let mean_above = (weighted - below_sum) / above as f64;
        let variance = below as f64 * above as f64 * (mean_below - mean_above).powi(2);
        if variance > best_var {
            best_var = variance;
            best = (level + 1).min(255) as u8;
        }
    }
    best
}

/// Rotates by `degrees` clockwise around the centre, keeping the canvas
/// size and filling the corners with `fill`.
pub fn rotate(gray: &GrayImage, degrees: f32, fill: u8) -> GrayImage {
    let (w, h) = gray.dimensions();
    let pixels = rotate_pixels(gray.as_raw(), w, h, degrees, [fill]);
    GrayImage::from_raw(w, h, pixels).expect("a buffer of the image's size")
}

/// Rotates an image of `width` x `height` pixels of `N` interleaved 8-bit
/// channels by `degrees` clockwise around its centre, on a canvas of the same
/// size: bilinear, with positions outside the image given `fill`. The gray
/// copy read by OCR and the colour clean page are both turned by it, so they
/// stay alike. Weights are in 1/256ths and pixels are read straight from the
/// buffer: a page at 300 DPI turns in a fraction of the time per-pixel float
/// sampling takes, a shade off at most.
pub(crate) fn rotate_pixels<const N: usize>(
    src: &[u8],
    width: u32,
    height: u32,
    degrees: f32,
    fill: [u8; N],
) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0_u8; w * h * N];
    if w == 0 || h == 0 {
        return out;
    }
    let (sin, cos) = degrees.to_radians().sin_cos();
    let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
    let (last_x, last_y) = (width as f32 - 0.5, height as f32 - 0.5);
    let row_len = w * N;
    let column = |v: f32| (v.max(0.0) as usize).min(w - 1);
    let line = |v: f32| (v.max(0.0) as usize).min(h - 1) * row_len;
    for (y, row) in out.chunks_exact_mut(row_len).enumerate() {
        let dy = y as f32 + 0.5 - cy;
        for (x, pixel) in row.as_chunks_mut::<N>().0.iter_mut().enumerate() {
            let dx = x as f32 + 0.5 - cx;
            let sx = cx + cos * dx + sin * dy - 0.5;
            let sy = cy - sin * dx + cos * dy - 0.5;
            if sx < -0.5 || sy < -0.5 || sx > last_x || sy > last_y {
                *pixel = fill;
                continue;
            }
            let (x0, y0) = (sx.floor(), sy.floor());
            let fx = ((sx - x0) * 256.0).round() as u32;
            let fy = ((sy - y0) * 256.0).round() as u32;
            let (left, right) = (column(x0) * N, column(x0 + 1.0) * N);
            let (top, bottom) = (line(y0), line(y0 + 1.0));
            for (c, value) in pixel.iter_mut().enumerate() {
                let at = |row: usize, col: usize| u32::from(src[row + col + c]);
                let upper = at(top, left) * (256 - fx) + at(top, right) * fx;
                let lower = at(bottom, left) * (256 - fx) + at(bottom, right) * fx;
                *value = ((upper * (256 - fy) + lower * fy + (1 << 15)) >> 16) as u8;
            }
        }
    }
    out
}

/// Crops rows and columns at the edges that are one uniform shade. A page
/// that is uniform throughout is returned unchanged.
pub fn trim_borders(gray: &GrayImage) -> GrayImage {
    let (x, y, w, h) = border_box(gray);
    imageops::crop_imm(gray, x, y, w, h).to_image()
}

/// The box (x, y, width, height) [`trim_borders`] keeps: the page without the
/// rows and columns at its edges that are one uniform shade; the whole page
/// when it is uniform throughout.
pub fn border_box(gray: &GrayImage) -> (u32, u32, u32, u32) {
    let (w, h) = gray.dimensions();
    let row_uniform = |y: u32| spread((0..w).map(|x| gray.get_pixel(x, y).0[0]));
    let col_uniform = |x: u32| spread((0..h).map(|y| gray.get_pixel(x, y).0[0]));

    let Some(top) = (0..h).find(|&y| !row_uniform(y)) else {
        return (0, 0, w, h);
    };
    let bottom = (0..h).rev().find(|&y| !row_uniform(y)).unwrap_or(top);
    let Some(left) = (0..w).find(|&x| !col_uniform(x)) else {
        return (0, 0, w, h);
    };
    let right = (0..w).rev().find(|&x| !col_uniform(x)).unwrap_or(left);
    (left, top, right - left + 1, bottom - top + 1)
}

/// True when the values lie within `UNIFORM_TOLERANCE` of each other.
fn spread(values: impl Iterator<Item = u8>) -> bool {
    let (mut lo, mut hi) = (u8::MAX, u8::MIN);
    for v in values {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    hi - lo <= UNIFORM_TOLERANCE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::fixture;
    use ::image::Luma;

    fn scan_gray() -> GrayImage {
        ::image::open(fixture("scan_ru.png")).unwrap().to_luma8()
    }

    /// Bilinear sampling in floats, pixel by pixel: what [`rotate_pixels`]
    /// must match.
    fn rotated_in_floats(gray: &GrayImage, degrees: f32, fill: u8) -> GrayImage {
        let (w, h) = gray.dimensions();
        let (sin, cos) = degrees.to_radians().sin_cos();
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        GrayImage::from_fn(w, h, |x, y| {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let sx = cx + cos * dx + sin * dy - 0.5;
            let sy = cy - sin * dx + cos * dy - 0.5;
            if sx < -0.5 || sy < -0.5 || sx > w as f32 - 0.5 || sy > h as f32 - 0.5 {
                return Luma([fill]);
            }
            let (x0, y0) = (sx.floor(), sy.floor());
            let (fx, fy) = (sx - x0, sy - y0);
            let at = |px: f32, py: f32| {
                let px = (px.max(0.0) as u32).min(w - 1);
                let py = (py.max(0.0) as u32).min(h - 1);
                f32::from(gray.get_pixel(px, py).0[0])
            };
            let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1.0, y0) * fx;
            let bottom = at(x0, y0 + 1.0) * (1.0 - fx) + at(x0 + 1.0, y0 + 1.0) * fx;
            Luma([(top * (1.0 - fy) + bottom * fy).round() as u8])
        })
    }

    #[test]
    fn turning_matches_float_sampling_within_a_shade() {
        let page = GrayImage::from_fn(301, 203, |x, y| {
            Luma([((x * 7 + y * 13 + (x * y) % 29) % 256) as u8])
        });
        for degrees in [0.0_f32, 0.4, -3.7, 9.5] {
            let fast = rotate(&page, degrees, 250);
            let slow = rotated_in_floats(&page, degrees, 250);
            let worst = fast
                .pixels()
                .zip(slow.pixels())
                .map(|(a, b)| a.0[0].abs_diff(b.0[0]))
                .max()
                .unwrap();
            assert!(worst <= 1, "{degrees}°: off by {worst}");
        }
        assert_eq!(rotate(&page, 0.0, 0), page);
    }

    #[test]
    fn straight_page_has_no_skew() {
        let skew = estimate_skew(&scan_gray());
        assert!(skew.abs() <= 0.5, "skew {skew}");
    }

    #[test]
    fn skew_of_a_rotated_page_is_estimated() {
        for degrees in [-7.0_f32, -3.0, 4.0, 8.0] {
            let tilted = rotate(&scan_gray(), degrees, 255);
            let skew = estimate_skew(&tilted);
            assert!(
                (skew - degrees).abs() <= 0.75,
                "rotated by {degrees}, estimated {skew}"
            );
        }
    }

    #[test]
    fn prepare_straightens_a_tilted_page() {
        let tilted = DynamicImage::ImageLuma8(rotate(&scan_gray(), 5.0, 255));
        let fixed = prepare(&tilted).to_luma8();
        let skew = estimate_skew(&fixed);
        assert!(skew.abs() <= 0.75, "still skewed by {skew}");
    }

    #[test]
    fn skew_beyond_the_limit_is_left_alone() {
        let img = rotate(&scan_gray(), 25.0, 255);
        let fixed = prepare(&DynamicImage::ImageLuma8(img.clone())).to_luma8();
        assert_eq!(fixed.dimensions(), trim_borders(&img).dimensions());
    }

    #[test]
    fn uniform_borders_are_trimmed() {
        let mut img = GrayImage::from_pixel(200, 100, Luma([255]));
        for y in 30..70 {
            for x in 50..150 {
                img.put_pixel(x, y, Luma([20]));
            }
        }
        let trimmed = trim_borders(&img);
        assert_eq!(trimmed.dimensions(), (100, 40));
    }

    #[test]
    fn a_gray_page_prepares_like_its_colour_original() {
        let tilted = rotate(&scan_gray(), 4.0, 255);
        let colour = DynamicImage::ImageRgb8(DynamicImage::ImageLuma8(tilted.clone()).to_rgb8());
        assert_eq!(
            prepare(&DynamicImage::ImageLuma8(tilted)).to_luma8(),
            prepare(&colour).to_luma8()
        );
    }

    #[test]
    fn blank_page_is_not_trimmed_to_nothing() {
        let img = GrayImage::from_pixel(50, 40, Luma([255]));
        assert_eq!(trim_borders(&img).dimensions(), (50, 40));
    }
}
