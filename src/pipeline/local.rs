//! Local extraction (spec §7.2): embedded text, rendering, orientation,
//! deskew and OCR, page by page. Nothing here touches the database.

use std::borrow::Cow;
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::image::{DynamicImage, RgbImage};
use futures_util::{StreamExt, stream};
use tokio::task::JoinError;

use crate::error::{Result, ServiceError};
use crate::extract::clean;
use crate::extract::image::{self as images, Webp, fit};
use crate::extract::pdf::effective_ocr_dpi;
use crate::extract::preprocess::{self, Straightening};
use crate::extract::quality;
use crate::extract::tesseract::{OcrPage, Reading, Tesseract};
use crate::extract::worker::Isolation;
use crate::service::App;

/// Resolution pages are rendered at before OCR.
pub const OCR_DPI: u32 = 300;
/// Resolution of pages that only need a preview.
pub const PREVIEW_DPI: u32 = 110;
/// Longest edge of the cached page renders.
pub const CACHE_EDGE: u32 = 1600;
/// Longest edge a page is read at: A4 at 300 DPI. A phone photo is larger
/// without reading any better, and every buffer and the OCR time grow with
/// its pixel count.
pub const OCR_MAX_EDGE: u32 = 3508;
/// Longest edge of the copy orientation detection looks at (about 170 DPI
/// for A4): a third of the pixels of a 300 DPI page, the same verdict. It
/// needs no less: at 1400 its confidence on a page of text drops below 2.
const SMALL_EDGE: u32 = 2000;
/// Longest edge of the copy the turn search reads (about 100 DPI for A4). A
/// card cut out of a sheet is smaller already; a full page reads its right
/// turn as well as at 2000 and its wrong turns worse, which tells them apart.
const SEARCH_EDGE: u32 = 1200;
/// Maximum PDF pages processed concurrently per document.
const MAX_PAGES_IN_FLIGHT: usize = 4;

fn pages_in_flight(ocr_workers: usize) -> usize {
    let cores = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2);
    (cores / ocr_workers.max(1)).clamp(2, MAX_PAGES_IN_FLIGHT)
}

/// What the pipeline needs to know about the original.
pub struct SourceFile {
    pub mime: String,
    pub storage_key: String,
}

pub struct PageOut {
    pub n: u32,
    /// Size of the cached render in pixels, when there is one.
    pub size: Option<(u32, u32)>,
    /// Embedded text and its quality score, when the page has any.
    pub embedded: Option<(String, f32)>,
    pub ocr: Option<OcrPage>,
    /// A second, MRZ-specific reading of the bottom of the page, when the
    /// first pass saw filler characters there.
    pub mrz_text: Option<String>,
    /// WebP bytes of the cached page render.
    pub render: Option<Vec<u8>>,
    /// The page as it goes into the clean copy, whether cleaning changed it,
    /// and whether straightening altered the visual geometry relative to `render`.
    /// Only pages of documents building a clean copy have one.
    pub clean: Option<(clean::Page, bool, bool)>,
    /// The clean page as a cached render (and thumbnail, for page 1), present
    /// only when straightening altered the visual geometry relative to `render`.
    pub(crate) clean_render: Option<CleanRender>,
    /// WebP bytes of the page as it was read (upright, not straightened),
    /// kept only when the clean copy took over `render`.
    pub original: Option<Vec<u8>>,
}

/// A clean page encoded for the viewer.
pub(crate) struct CleanRender {
    pub(crate) webp: Webp,
    pub(crate) thumbnail: Option<Vec<u8>>,
}

pub struct LocalOutput {
    pub pages: Vec<PageOut>,
    /// WebP thumbnail of the first page.
    pub thumbnail: Option<Vec<u8>>,
    /// Clean copy pages to stream as `clean.pdf`: built for images, and for
    /// PDFs whose pages were all read and at least one changed meaningfully.
    pub clean: Option<Vec<clean::Page>>,
}

/// Long edge of an A4 sheet in PDF points: an image's page is given this size.
const A4_LONG_PT: f32 = 842.0;

/// The clean copy of `pages`, when every page has its clean page and, unless
/// `always`, cleaning changed at least one of them.
///
/// Once there is a clean copy, any page whose straightening changed its geometry
/// replaces `render` (and `thumbnail` for page 1) with the clean render and
/// keeps the as-read render as `original`.
fn bind_clean_copy(pages: &mut [PageOut], always: bool) -> Option<BoundCopy> {
    let complete = !pages.is_empty() && pages.iter().all(|p| p.clean.is_some());
    let changed = pages
        .iter()
        .any(|p| p.clean.as_ref().is_some_and(|(_, c, _)| *c));
    let bound = complete && (always || changed);
    let mut taken = Vec::new();
    let mut thumbnail = None;
    for page in pages.iter_mut() {
        let clean = page.clean.take();
        let render = page.clean_render.take();
        if !bound {
            continue;
        }
        let mut visual_changed = false;
        if let Some((clean_page, _, visual)) = clean {
            taken.push(clean_page);
            visual_changed = visual;
        }
        if visual_changed && let Some(render) = render {
            page.original = page.render.take();
            page.size = Some((render.webp.width, render.webp.height));
            page.render = Some(render.webp.bytes);
            if page.n == 1 {
                thumbnail = render.thumbnail;
            }
        }
    }
    bound.then_some(BoundCopy {
        pages: taken,
        thumbnail,
    })
}

/// The clean copy's encoded pages, and the thumbnail made from its first page.
struct BoundCopy {
    pages: Vec<clean::Page>,
    pub(crate) thumbnail: Option<Vec<u8>>,
}

/// Files the clean copy into `out`: its pages, and any replacement thumbnail.
fn take_clean_copy(out: &mut LocalOutput, bound: Option<BoundCopy>) {
    if let Some(bound) = bound {
        out.clean = Some(bound.pages);
        if bound.thumbnail.is_some() {
            out.thumbnail = bound.thumbnail;
        }
    }
}

fn joined<T>(result: std::result::Result<Result<T>, JoinError>) -> Result<T> {
    result.unwrap_or_else(|e| {
        tracing::error!(error = %e, "a blocking task failed");
        Err(ServiceError::internal(
            "worker_failed",
            "a processing task failed",
        ))
    })
}

pub async fn extract(app: &App, file: &SourceFile) -> Result<LocalOutput> {
    let started = Instant::now();
    let bytes = read_original(app, &file.storage_key).await?;
    let tesseract = Tesseract::new(app.config.ocr_langs.clone());
    let isolation = Isolation::new(&app.config);
    let out = match file.mime.as_str() {
        "application/pdf" => {
            pdf(
                &isolation,
                &tesseract,
                bytes,
                pages_in_flight(app.config.ocr_workers),
            )
            .await
        }
        "text/plain" | "text/markdown" => Ok(text(&bytes)),
        mime => image(&isolation, &tesseract, bytes, mime.to_string()).await,
    }?;
    tracing::info!(
        pages = out.pages.len(),
        ms = ms(started.elapsed()),
        "local extraction done"
    );
    Ok(out)
}

fn ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Where the time of one page went; logged when the page is done.
#[derive(Debug, Default, Clone, Copy)]
struct PageTimes {
    /// Rendering the PDF page (ahead of the reading, on the renderer thread).
    render: Duration,
    /// Fitting, cutting out and the grayscale copies.
    prepare: Duration,
    /// Orientation detection.
    osd: Duration,
    /// Trying turns to find the one the page reads at, and how many were.
    search: Duration,
    turns_tried: u32,
    /// Deskewing and full OCR passes, and how many there were.
    read: Duration,
    reads: u32,
    /// The second, MRZ-specific passes.
    mrz: Duration,
    /// Turning and straightening the colour page for the clean copy.
    straighten: Duration,
    /// Encoding the cached render, the thumbnail and the clean page.
    encode: Duration,
}

impl PageTimes {
    fn log(&self, page: u32, turn: Option<u16>, total: Duration) {
        tracing::info!(
            page,
            turn,
            render_ms = ms(self.render),
            prepare_ms = ms(self.prepare),
            osd_ms = ms(self.osd),
            search_ms = ms(self.search),
            turns_tried = self.turns_tried,
            read_ms = ms(self.read),
            reads = self.reads,
            mrz_ms = ms(self.mrz),
            straighten_ms = ms(self.straighten),
            encode_ms = ms(self.encode),
            total_ms = ms(total),
            "page processed"
        );
    }
}

async fn read_original(app: &App, key: &str) -> Result<Vec<u8>> {
    let files = app.files.clone();
    let key = key.to_string();
    let read = tokio::task::spawn_blocking(move || {
        let mut file = files.open_original(&key)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    })
    .await;
    match read {
        Ok(Ok(bytes)) => Ok(bytes),
        _ => Err(ServiceError::internal(
            "storage_error",
            "the original file could not be read",
        )),
    }
}

/// Plain text and Markdown: the text is exact, there is nothing to render.
fn text(bytes: &[u8]) -> LocalOutput {
    let text = String::from_utf8_lossy(bytes).trim().to_string();
    LocalOutput {
        pages: vec![PageOut {
            n: 1,
            size: None,
            embedded: (!text.is_empty()).then_some((text, 1.0)),
            ocr: None,
            mrz_text: None,
            render: None,
            clean: None,
            clean_render: None,
            original: None,
        }],
        thumbnail: None,
        clean: None,
    }
}

async fn image(
    isolation: &Isolation,
    tesseract: &Tesseract,
    bytes: Vec<u8>,
    mime: String,
) -> Result<LocalOutput> {
    let started = Instant::now();
    // Decoded and scaled down in the worker, so the full-size decode never
    // exists in this process.
    let decoded =
        DynamicImage::ImageRgb8(isolation.decode_image(bytes, &mime, OCR_MAX_EDGE).await?);
    let decoding = started.elapsed();
    // The sheet an image shows is taken to be A4.
    let pt_per_px = A4_LONG_PT / decoded.width().max(decoded.height()).max(1) as f32;
    // An image is its own source: its pixels are already the native ones.
    let read = ocr_page(tesseract, decoded, true, pt_per_px, None, true).await?;
    let mut times = read.times;
    times.render = decoding;
    let encoding = Instant::now();
    let encoded = encode_read(read.upright, read.clean, true).await?;
    times.encode = encoding.elapsed();
    times.log(1, Some(read.turn), started.elapsed());
    let thumbnail = encoded.thumbnail;
    let mut pages = vec![PageOut {
        n: 1,
        size: Some((encoded.render.width, encoded.render.height)),
        embedded: None,
        ocr: Some(read.page),
        mrz_text: read.mrz_text,
        render: Some(encoded.render.bytes),
        clean: encoded.clean,
        clean_render: encoded.clean_render,
        original: None,
    }];
    // An image always gets its clean copy: a scan is wanted as a PDF.
    let bound = bind_clean_copy(&mut pages, true);
    let mut out = LocalOutput {
        pages,
        thumbnail,
        clean: None,
    };
    take_clean_copy(&mut out, bound);
    Ok(out)
}

async fn pdf(
    isolation: &Isolation,
    tesseract: &Tesseract,
    bytes: Vec<u8>,
    in_flight: usize,
) -> Result<LocalOutput> {
    // The worker reads the embedded text of every page first (this also
    // rejects encrypted, corrupt and over-long documents), with the density of
    // the scan behind each page that goes to OCR; then it renders the pages
    // while they are read.
    let run = isolation
        .open_pdf(Arc::new(bytes), OCR_DPI, PREVIEW_DPI)
        .await?;
    let (embedded, native): (Vec<EmbeddedText>, Vec<Option<f32>>) = run
        .texts
        .into_iter()
        .map(|page| (page.text.map(|text| (text, page.score)), page.density))
        .unzip();
    let receiver = run.pages;
    let needs_ocr: Vec<bool> = embedded.iter().map(quality::needs_ocr).collect();
    let all_need_ocr = !needs_ocr.is_empty() && needs_ocr.iter().all(|&b| b);

    // Renders are taken in page order; up to `in_flight` pages are read
    // at once, and the results come back in page order.
    let pages = embedded
        .into_iter()
        .zip(needs_ocr)
        .zip(native)
        .map(|((embedded, needs_ocr), native)| (embedded, needs_ocr, native))
        .enumerate();
    let inputs = stream::unfold(
        (receiver, pages),
        move |(mut receiver, mut pages)| async move {
            let (i, (embedded, needs_ocr, native)) = pages.next()?;
            let n = i as u32 + 1;
            let rendered = match receiver.recv().await {
                Some((got, result, took)) if got == n => (result, took),
                _ => (
                    Err(ServiceError::invalid(
                        "pdf_corrupt",
                        "the PDF could not be rendered",
                    )),
                    Duration::ZERO,
                ),
            };
            Some((
                (n, embedded, needs_ocr, native, rendered),
                (receiver, pages),
            ))
        },
    );
    let read = inputs
        .map(|(n, embedded, needs_ocr, native, (rendered, took))| {
            let tesseract = tesseract.clone();
            async move {
                pdf_page(
                    &tesseract,
                    n,
                    (embedded, needs_ocr, native),
                    rendered,
                    took,
                    all_need_ocr,
                )
                .await
            }
        })
        .buffered(in_flight);
    let mut read = std::pin::pin!(read);

    let mut out = LocalOutput {
        pages: Vec::new(),
        thumbnail: None,
        clean: None,
    };
    while let Some(result) = read.next().await {
        let (page, thumbnail) = result?;
        if thumbnail.is_some() {
            out.thumbnail = thumbnail;
        }
        out.pages.push(page);
    }
    let bound = bind_clean_copy(&mut out.pages, false);
    take_clean_copy(&mut out, bound);
    Ok(out)
}

/// A page's embedded text and its quality score, when it has any.
type EmbeddedText = Option<(String, f32)>;

/// Reads (when needed) and encodes one rendered PDF page; returns the page
/// and, for page 1, the thumbnail.
async fn pdf_page(
    tesseract: &Tesseract,
    n: u32,
    (embedded, needs_ocr, native): (EmbeddedText, bool, Option<f32>),
    rendered: Result<RgbImage>,
    rendering: Duration,
    build_clean: bool,
) -> Result<(PageOut, Option<Vec<u8>>)> {
    let started = Instant::now();
    let mut times = PageTimes {
        render: rendering,
        ..PageTimes::default()
    };
    let mut turn = None;
    let mut page = PageOut {
        n,
        size: None,
        embedded,
        ocr: None,
        mrz_text: None,
        render: None,
        clean: None,
        clean_render: None,
        original: None,
    };
    let mut thumbnail = None;
    match rendered {
        Ok(rgb) => {
            let image = DynamicImage::ImageRgb8(rgb);
            let (render, thumb) = if needs_ocr {
                let render_dpi = effective_ocr_dpi(OCR_DPI, native);
                let pt_per_px = 72.0 / render_dpi as f32;
                let read = ocr_page(
                    tesseract,
                    image,
                    n <= SEARCHED_PAGES,
                    pt_per_px,
                    native,
                    build_clean,
                )
                .await?;
                times = PageTimes {
                    render: rendering,
                    ..read.times
                };
                turn = Some(read.turn);
                // A stamp is kept as text but no longer stands for the page:
                // the page goes to OCR's text and the LLM's transcription.
                if let Some((text, score)) = page.embedded.as_mut()
                    && quality::ocr_says_more(text, &read.page.text)
                {
                    *score = score.min(quality::PARTIAL_SCORE);
                }
                page.ocr = Some(read.page);
                page.mrz_text = read.mrz_text;
                let encoding = Instant::now();
                let encoded = encode_read(read.upright, read.clean, n == 1).await?;
                times.encode = encoding.elapsed();
                page.clean = encoded.clean;
                page.clean_render = encoded.clean_render;
                (encoded.render, encoded.thumbnail)
            } else {
                let encoding = Instant::now();
                let encoded = encode(image, n == 1).await?;
                times.encode = encoding.elapsed();
                encoded
            };
            page.size = Some((render.width, render.height));
            page.render = Some(render.bytes);
            thumbnail = thumb;
            // The render was made ahead, while earlier pages were read.
            times.log(n, turn, started.elapsed());
        }
        // A page that only needs a preview may fail to render without
        // failing the document.
        Err(e) if !needs_ocr => {
            tracing::warn!(
                page = n,
                code = e.code,
                "page preview could not be rendered"
            );
        }
        Err(e) => return Err(e),
    }
    Ok((page, thumbnail))
}

/// Pages with fewer OCR words than this may be an identity document lying on
/// an otherwise empty sheet; denser pages are never turned by search.
const SPARSE_WORDS: usize = 200;
/// Only the first pages of a document are searched; an ID card or a passport
/// data page is among them.
const SEARCHED_PAGES: u32 = 2;
/// The four ways a page can lie.
const TURNS: [u16; 4] = [0, 90, 270, 180];
/// An orientation verdict at least this sure is right: pages of text get 6
/// and more, while an ID card or a passport page (little text, a photo, a
/// patterned background) gets 0.3 to 4.5, wrong as often as one time in three.
const SURE_OSD_CONFIDENCE: f32 = 6.0;
/// A reading with this many legible characters is turned right; the turns
/// after it are not tried. On the search copy, a card, a passport page or a
/// page of text reads 46 to 770 at its right turn, at most 57 upside down
/// and 24 sideways; read in full, a wrong turn gives at most 54.
const LEGIBLE_ENOUGH: usize = 100;
/// A reading below this says nothing about the turn.
const MIN_LEGIBLE: usize = 8;
/// Of the two turns along the line a page lies on, the better is right when
/// it reads at least this much (and twice its opposite): more than any
/// sideways reading, less than the poorest right one.
const MIN_LINE_SCORE: usize = 30;
/// A reading with fewer words than this does not tell whether they stand.
/// With more, a sideways one has three in four of them standing on end, a
/// right or upside-down one at most one in four.
const MIN_WORDS_FOR_AXIS: usize = 6;
/// What a line that looks like a machine-readable zone adds to a reading: a
/// zone read upright is the surest sign of the right turn, even when its
/// characters are not read with full confidence.
const ZONE_LINE_WEIGHT: usize = 150;

/// [`fit`] for a shared image: the same one when it is small enough already.
fn shrunk(image: &Arc<DynamicImage>, max_edge: u32) -> Arc<DynamicImage> {
    if image.width().max(image.height()) > max_edge {
        Arc::new(image.thumbnail(max_edge, max_edge))
    } else {
        Arc::clone(image)
    }
}

/// `image` turned clockwise by `degrees`, borrowed when it stays as it is.
fn turned(image: &DynamicImage, degrees: u16) -> Cow<'_, DynamicImage> {
    match degrees {
        90 => Cow::Owned(image.rotate90()),
        180 => Cow::Owned(image.rotate180()),
        270 => Cow::Owned(image.rotate270()),
        _ => Cow::Borrowed(image),
    }
}

/// A page whose content covers at most this share of the sheet is a card or a
/// small document lying on it (an ID card scanned onto A4): the page becomes
/// the content, so that it is turned, read and shown at a useful size.
const CUT_OUT_SHARE: f32 = 0.6;

#[cfg(test)]
fn cut_out_content(image: DynamicImage) -> DynamicImage {
    cut_out(image).0
}

/// [`cut_out_content`], also telling whether anything was cut.
fn cut_out(image: DynamicImage) -> (DynamicImage, bool) {
    match images::content_box_within(&image, CUT_OUT_SHARE) {
        Some((x, y, w, h)) => (image.crop_imm(x, y, w, h), true),
        None => (image, false),
    }
}

fn is_sparse(page: &OcrPage) -> bool {
    page.text.split_whitespace().count() < SPARSE_WORDS
}

/// Lines of `text` that look like one line of a machine-readable zone: 28 to
/// 46 characters, nearly all of them `A-Z`, `0-9` and `<`, with filler, and
/// either a run of it (names, an empty field) or the digits of numbers and
/// dates. An upside-down zone reads as capitals too, with a stray `<`.
fn zone_lines(text: &str) -> usize {
    text.lines()
        .map(|line| line.split_whitespace().collect::<String>())
        .filter(|line| {
            let length = line.chars().count();
            let zone = line
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '<')
                .count();
            let digits = line.chars().filter(char::is_ascii_digit).count();
            (28..=46).contains(&length)
                && zone * 100 >= length * 85
                && line.contains('<')
                && (line.contains("<<") || digits >= ZONE_DIGITS)
        })
        .count()
}

/// A zone line without a run of filler carries at least this many digits: a
/// number, its check digit and two dates.
const ZONE_DIGITS: usize = 10;

/// What reading one page gives.
struct ReadPage {
    /// The page turned upright and cut out of the sheet, in colour, for the
    /// cached render.
    upright: DynamicImage,
    page: OcrPage,
    mrz_text: Option<String>,
    /// The page as it goes into the clean copy (absent when no clean copy
    /// will be built, e.g. on a mixed PDF).
    clean: Option<CleanSource>,
    /// The clockwise turn the page was read at.
    turn: u16,
    times: PageTimes,
}

/// A page for the clean copy before it is encoded.
struct CleanSource {
    /// Upright, cut out, straightened and trimmed, in colour.
    image: RgbImage,
    /// PDF points per pixel of `image`.
    pt_per_px: f32,
    /// Whether cleaning changed the page meaningfully (cut, turn, or visual).
    changed: bool,
    /// Whether straightening altered the geometry relative to `upright`.
    visual_changed: bool,
    /// Pixels per point the source itself has, when it is known (a scanned
    /// PDF page: its one image). The clean page is not kept denser than that.
    native_px_per_pt: Option<f32>,
}

/// Rotates the page upright, then deskews and trims a copy for OCR. Returns
/// the upright colour image (for the cached render), the OCR result and the
/// page for the clean copy: the upright image straightened the way the OCR
/// copy was. `pt_per_px` is the size of a pixel of `image` in PDF points;
/// `native_px_per_pt` is the density of the source's own pixels, when it is
/// not the image's (a PDF page rendered denser than the scan inside it).
///
/// Tesseract's orientation detection is a guess on a sparse page (an ID card
/// scanned onto A4, a passport page): it gives up on small text and is
/// confidently wrong on others. So among the first pages of a document, a
/// page it is unsure about is turned every way on a small copy before it is
/// read ([`choose_turn`]), and a page it is sure about that still reads
/// poorly is searched the same way after.
///
/// Everything after the cut-out works on one grayscale copy; the colour image
/// is only turned at the end, for the render and the clean copy.
async fn ocr_page(
    tesseract: &Tesseract,
    image: DynamicImage,
    search: bool,
    pt_per_px: f32,
    native_px_per_pt: Option<f32>,
    build_clean: bool,
) -> Result<ReadPage> {
    let mut times = PageTimes::default();
    let started = Instant::now();
    let prepared = joined(
        tokio::task::spawn_blocking(move || {
            let before = image.width().max(image.height()).max(1);
            let fitted = fit(image, OCR_MAX_EDGE);
            let pt_per_px =
                pt_per_px * before as f32 / fitted.width().max(fitted.height()).max(1) as f32;
            let (image, cut) = cut_out(fitted);
            let gray = Arc::new(DynamicImage::ImageLuma8(image.to_luma8()));
            // Orientation, and how each turn reads, need no full resolution.
            let small = shrunk(&gray, SMALL_EDGE);
            // Made from the page itself: scaled down twice, it reads worse.
            let searched = search.then(|| shrunk(&gray, SEARCH_EDGE));
            Ok((image, gray, small, searched, cut, pt_per_px))
        })
        .await,
    )?;
    let (image, gray, small, searched, cut, pt_per_px) = prepared;
    times.prepare = started.elapsed();
    let started = Instant::now();
    let orientation = tesseract.detect_orientation(&small).await?;
    times.osd = started.elapsed();
    drop(small);
    let detected = orientation.trusted();
    let unsure = orientation.confidence < SURE_OSD_CONFIDENCE;
    let (mut turn, mut page, mut mrz_text, mut straightening) = match searched.as_ref() {
        Some(searched) if unsure => {
            // Upright first: most pages are.
            let hint = orientation.degrees;
            let turn = choose_turn(tesseract, searched, 0, hint, &mut times).await?;
            let (page, mrz_text, straightening) =
                read_page(tesseract, &gray, turn, &mut times).await?;
            (turn, page, mrz_text, straightening)
        }
        _ => {
            let (page, mrz_text, straightening) =
                read_page(tesseract, &gray, detected, &mut times).await?;
            (detected, page, mrz_text, straightening)
        }
    };
    // A sure verdict on a sparse page that still reads poorly: other turns
    // are tried on the search copy (a full reading is not comparable).
    if let Some(searched) = searched.as_ref()
        && !unsure
        && is_sparse(&page)
        && page.legible_chars() < LEGIBLE_ENOUGH
    {
        let better = choose_turn(tesseract, searched, detected, detected, &mut times).await?;
        if better != turn {
            turn = better;
            (page, mrz_text, straightening) = read_page(tesseract, &gray, turn, &mut times).await?;
        }
    }
    if turn != detected {
        tracing::info!(from = detected, to = turn, "page turned by what it reads");
    }
    drop((gray, searched));
    let started = Instant::now();
    let (upright, clean) = joined(
        tokio::task::spawn_blocking(move || {
            let upright = match turn {
                90 => image.rotate90(),
                180 => image.rotate180(),
                270 => image.rotate270(),
                _ => image,
            };
            if !build_clean {
                return Ok((upright, None));
            }
            let converted;
            let rgb = match upright.as_rgb8() {
                Some(rgb) => rgb,
                None => {
                    converted = upright.to_rgb8();
                    &converted
                }
            };
            let visual_changed = straightening.meaningful_change(rgb.width(), rgb.height());
            let changed = cut || turn != 0 || visual_changed;
            let clean = if visual_changed {
                clean::straighten(rgb, straightening.degrees, straightening.crop)
            } else {
                rgb.clone()
            };
            Ok((
                upright,
                Some(CleanSource {
                    image: clean,
                    pt_per_px,
                    changed,
                    visual_changed,
                    native_px_per_pt,
                }),
            ))
        })
        .await,
    )?;
    times.straighten = started.elapsed();
    Ok(ReadPage {
        upright,
        page,
        mrz_text,
        clean,
        turn,
        times,
    })
}

/// A page that was read, encoded.
struct EncodedRead {
    /// The cached render of the page as read, and its thumbnail if asked.
    render: Webp,
    thumbnail: Option<Vec<u8>>,
    /// The clean page, whether cleaning changed it, and whether it changed visually.
    clean: Option<(clean::Page, bool, bool)>,
    /// The render the viewer shows once the clean copy exists (only when
    /// `visual_changed` is true).
    clean_render: Option<CleanRender>,
}

/// Encodes a page that was read: its render, and (when requested) its clean
/// page with a replacement viewer render only if straightening visually
/// changed the geometry. Page 1 never encodes two thumbnails.
async fn encode_read(
    upright: DynamicImage,
    clean: Option<CleanSource>,
    thumbnail: bool,
) -> Result<EncodedRead> {
    let Some(clean) = clean else {
        let (render, thumb) = encode(upright, thumbnail).await?;
        return Ok(EncodedRead {
            render,
            thumbnail: thumb,
            clean: None,
            clean_render: None,
        });
    };
    if !clean.visual_changed {
        let changed = clean.changed;
        let encode_page = async {
            joined(
                tokio::task::spawn_blocking(move || {
                    let (rgb, pt_per_px) = match clean.native_px_per_pt {
                        Some(native) => {
                            clean::at_native_density(clean.image, clean.pt_per_px, native)
                        }
                        None => (clean.image, clean.pt_per_px),
                    };
                    clean::page(rgb, pt_per_px)
                })
                .await,
            )
        };
        let ((render, thumb), clean_page) =
            tokio::try_join!(encode(upright, thumbnail), encode_page)?;
        return Ok(EncodedRead {
            render,
            thumbnail: thumb,
            clean: Some((clean_page, changed, false)),
            clean_render: None,
        });
    }
    let ((render, _), (clean, clean_render)) =
        tokio::try_join!(encode(upright, false), encode_clean(clean, thumbnail))?;
    Ok(EncodedRead {
        render,
        thumbnail: clean_render.thumbnail.clone(),
        clean: Some((clean.0, clean.1, true)),
        clean_render: Some(clean_render),
    })
}

/// Encodes a page for the clean copy; returns it (and whether cleaning changed
/// it) together with the render the viewer shows once the copy exists. The
/// render is made from a copy scaled down first, so that the full page goes
/// to the JPEG encoder at the same time.
async fn encode_clean(
    source: CleanSource,
    thumbnail: bool,
) -> Result<((clean::Page, bool), CleanRender)> {
    let CleanSource {
        image,
        pt_per_px,
        changed,
        visual_changed: _,
        native_px_per_pt,
    } = source;
    let (image, preview) = joined(
        tokio::task::spawn_blocking(move || {
            let image = DynamicImage::ImageRgb8(image);
            let preview = if image.width().max(image.height()) > CACHE_EDGE {
                image.thumbnail(CACHE_EDGE, CACHE_EDGE)
            } else {
                image.clone()
            };
            Ok((image.into_rgb8(), preview))
        })
        .await,
    )?;
    let page = async {
        joined(
            tokio::task::spawn_blocking(move || {
                let started = Instant::now();
                let (rgb, pt_per_px) = match native_px_per_pt {
                    Some(native) => clean::at_native_density(image, pt_per_px, native),
                    None => (image, pt_per_px),
                };
                let page = clean::page(rgb, pt_per_px)?;
                tracing::debug!(
                    width = page.width,
                    height = page.height,
                    gray = page.gray,
                    bytes = page.jpeg.len(),
                    ms = ms(started.elapsed()),
                    "clean page encoded"
                );
                Ok(page)
            })
            .await,
        )
    };
    let render = async {
        joined(
            tokio::task::spawn_blocking(move || {
                Ok(CleanRender {
                    webp: images::webp(&preview, CACHE_EDGE)?,
                    thumbnail: if thumbnail {
                        Some(images::thumbnail(&preview)?)
                    } else {
                        None
                    },
                })
            })
            .await,
        )
    };
    let (page, render) = tokio::try_join!(page, render)?;
    Ok(((page, changed), render))
}

/// How `gray` turned by `degrees` reads: its legible characters, with the
/// lines of a machine-readable zone counting extra, and whether its words
/// stand on end (the page lies sideways at this turn), when there are enough
/// of them to tell.
fn reading_score(reading: &Reading) -> usize {
    reading.legible + zone_lines(&reading.flat_text) * ZONE_LINE_WEIGHT
}

/// Runs default automatic page segmentation first (preserving word bounding
/// boxes for sideways-block detection); when it drops layout blocks and yields
/// fewer than [`MIN_WORDS_FOR_AXIS`] words, falls back to sparse mode
/// (`--psm 11`) and keeps whichever reading scores higher.
async fn ocr_with_sparse_fallback(tesseract: &Tesseract, img: &DynamicImage) -> Result<OcrPage> {
    let page = tesseract.ocr(img).await?;
    let reading = page.reading();
    if reading.words >= MIN_WORDS_FOR_AXIS {
        return Ok(page);
    }
    let sparse = tesseract.ocr_sparse(img).await?;
    let sparse_reading = sparse.reading();
    if (reading_score(sparse_reading), sparse_reading.words)
        > (reading_score(reading), reading.words)
    {
        Ok(sparse)
    } else {
        Ok(page)
    }
}

async fn turn_score(
    tesseract: &Tesseract,
    gray: &Arc<DynamicImage>,
    degrees: u16,
) -> Result<(usize, Option<bool>)> {
    let (prepared, _) = prepare(gray, degrees).await?;
    let ocr = ocr_with_sparse_fallback(tesseract, &prepared).await?;
    let reading = ocr.reading();
    let score = reading_score(reading);
    let sideways =
        (reading.words >= MIN_WORDS_FOR_AXIS).then_some(reading.standing * 2 >= reading.words);
    Ok((score, sideways))
}

/// The turns tried so far and how they read.
struct Tried(Vec<(u16, usize)>);

impl Tried {
    fn score(&self, degrees: u16) -> Option<usize> {
        self.0.iter().find(|(d, _)| *d == degrees).map(|(_, s)| *s)
    }
}

/// The turn (clockwise degrees) at which `gray` reads best, starting from
/// `first`. A turn that reads well enough is taken at once. Otherwise how the
/// first turn read says whether the page lies along it or across it: the
/// better of the two turns on that line is taken when it reads clearly better
/// than its opposite (`hint`, orientation detection's verdict, is tried
/// first). Only a page that says neither is read every way; `first` then stays
/// unless another turn reads clearly better.
async fn choose_turn(
    tesseract: &Tesseract,
    gray: &Arc<DynamicImage>,
    first: u16,
    hint: u16,
    times: &mut PageTimes,
) -> Result<u16> {
    let started = Instant::now();
    let chosen = search_turns(tesseract, gray, first, hint, times).await;
    times.search += started.elapsed();
    chosen
}

async fn search_turns(
    tesseract: &Tesseract,
    gray: &Arc<DynamicImage>,
    first: u16,
    hint: u16,
    times: &mut PageTimes,
) -> Result<u16> {
    let mut tried = Tried(Vec::with_capacity(TURNS.len()));
    let mut try_turn = async |tried: &mut Tried, degrees: u16| -> Result<(usize, Option<bool>)> {
        times.turns_tried += 1;
        let (score, sideways) = turn_score(tesseract, gray, degrees).await?;
        tracing::debug!(degrees, score, ?sideways, "turn tried");
        tried.0.push((degrees, score));
        Ok((score, sideways))
    };
    let (score, sideways) = try_turn(&mut tried, first).await?;
    if score >= LEGIBLE_ENOUGH {
        return Ok(first);
    }
    let turn = |quarters: u16| (first + quarters * 90) % 360;
    let line = match sideways {
        Some(true) if hint == turn(3) => Some([turn(3), turn(1)]),
        Some(true) => Some([turn(1), turn(3)]),
        Some(false) => Some([first, turn(2)]),
        None => None,
    };
    if let Some(line) = line {
        for degrees in line {
            if tried.score(degrees).is_none() {
                let (score, _) = try_turn(&mut tried, degrees).await?;
                if score >= LEGIBLE_ENOUGH {
                    return Ok(degrees);
                }
            }
        }
        let [a, b] = line.map(|d| (d, tried.score(d).unwrap_or(0)));
        let (best, opposite) = if b.1 > a.1 { (b, a) } else { (a, b) };
        if best.1 >= MIN_LINE_SCORE && best.1 >= opposite.1 * 2 {
            return Ok(best.0);
        }
    }
    for degrees in TURNS {
        if tried.score(degrees).is_none() {
            let (score, _) = try_turn(&mut tried, degrees).await?;
            if score >= LEGIBLE_ENOUGH {
                return Ok(degrees);
            }
        }
    }
    Ok(pick_fallback(&tried, first))
}

/// Picks between `first` and the strongest turn in `tried`: stays at `first`
/// unless another turn both meets [`MIN_LEGIBLE`] and beats `first` by more
/// than a quarter (avoiding low-confidence flips on ambiguous or empty scans).
fn pick_fallback(tried: &Tried, first: u16) -> u16 {
    let baseline = tried.score(first).unwrap_or(0);
    // The first of equal scores, in the order they were tried.
    let best = tried
        .0
        .iter()
        .copied()
        .reduce(|best, next| if next.1 > best.1 { next } else { best })
        .unwrap_or((first, 0));
    if best.1 >= MIN_LEGIBLE && best.1 * 4 > baseline * 5 {
        best.0
    } else {
        first
    }
}

/// `gray` turned by `degrees`, deskewed and trimmed for OCR, and how it was
/// straightened.
async fn prepare(gray: &Arc<DynamicImage>, degrees: u16) -> Result<(DynamicImage, Straightening)> {
    let gray = Arc::clone(gray);
    joined(
        tokio::task::spawn_blocking(move || Ok(preprocess::prepare_with(&turned(&gray, degrees))))
            .await,
    )
}

/// Reads `gray` turned by `degrees`, with the second MRZ pass.
async fn read_page(
    tesseract: &Tesseract,
    gray: &Arc<DynamicImage>,
    degrees: u16,
    times: &mut PageTimes,
) -> Result<(OcrPage, Option<String>, Straightening)> {
    let started = Instant::now();
    let (prepared, straightening) = prepare(gray, degrees).await?;
    let page = ocr_with_sparse_fallback(tesseract, &prepared).await?;
    times.read += started.elapsed();
    times.reads += 1;
    let started = Instant::now();
    let mrz_text = read_mrz(tesseract, &prepared, &page).await;
    times.mrz += started.elapsed();
    Ok((page, mrz_text, straightening))
}

/// Second pass over the bottom of a page that has MRZ-like filler lines.
/// Best effort: a failure here only means the MRZ is not read.
async fn read_mrz(
    tesseract: &Tesseract,
    prepared: &DynamicImage,
    page: &OcrPage,
) -> Option<String> {
    let (x, y, w, h) = page.mrz_region?;
    let region = prepared.crop_imm(x, y, w, h);
    match tesseract.ocr_mrz(&region).await {
        Ok(text) => Some(text),
        Err(e) => {
            tracing::debug!(code = e.code, "the MRZ pass failed");
            None
        }
    }
}

/// The cached page render and, if asked, the thumbnail.
async fn encode(image: DynamicImage, thumbnail: bool) -> Result<(Webp, Option<Vec<u8>>)> {
    joined(
        tokio::task::spawn_blocking(move || {
            let thumb = if thumbnail {
                Some(images::thumbnail(&image)?)
            } else {
                None
            };
            Ok((images::webp(&image, CACHE_EDGE)?, thumb))
        })
        .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::fixture;
    use crate::extract::{HayroRenderer, OxideExtractor, PageRenderer, TextExtractor};
    use crate::mrz;
    use ::image::GenericImageView;

    fn have_tesseract() -> bool {
        let listed = std::process::Command::new("tesseract")
            .arg("--list-langs")
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_default();
        let have = ["eng", "rus", "ron", "osd"]
            .iter()
            .all(|lang| listed.lines().any(|l| l.trim() == *lang));
        if !have {
            eprintln!("skipped: tesseract or a language pack is missing");
        }
        have
    }

    fn image(name: &str) -> DynamicImage {
        ::image::open(fixture(name)).unwrap()
    }

    fn gray(image: DynamicImage) -> Arc<DynamicImage> {
        Arc::new(DynamicImage::ImageLuma8(image.to_luma8()))
    }

    fn verified_zone(page: &OcrPage, mrz_text: &Option<String>) -> bool {
        [mrz_text.as_deref(), Some(page.text.as_str())]
            .into_iter()
            .flatten()
            .filter_map(mrz::find)
            .any(|zone| zone.checks_ok)
    }

    #[tokio::test]
    async fn a_sideways_sheet_is_turned_upright_and_cut_to_its_content() {
        if !have_tesseract() {
            return;
        }
        // The page was left as scanned: a quarter turn clockwise.
        let tesseract = Tesseract::new("rus+ron+eng");
        let sheet = image("passport_td3_sideways_a4.png");
        let read = ocr_page(&tesseract, sheet.clone(), true, 72.0 / 300.0, None, true)
            .await
            .unwrap();
        let (upright, page, mrz_text) = (read.upright, read.page, read.mrz_text);
        assert!(verified_zone(&page, &mrz_text), "{mrz_text:?}");
        let clean_source = read.clean.expect("clean source built");
        assert!(
            clean_source.changed,
            "turning and cutting out change the page"
        );
        let clean = clean_source.image;
        assert!(
            clean.height() < clean.width(),
            "the clean passport page lies landscape like the upright one: {:?}",
            clean.dimensions()
        );
        assert!(page.text.contains("PASSPORT"), "{}", page.text);
        assert!(
            u64::from(upright.width()) * u64::from(upright.height())
                < u64::from(sheet.width()) * u64::from(sheet.height()) * 6 / 10,
            "the passport is cut out of the sheet: {:?}",
            upright.dimensions()
        );
    }

    #[tokio::test]
    async fn an_upright_page_is_not_turned_by_a_wrong_verdict() {
        if !have_tesseract() {
            return;
        }
        // Orientation detection said 90 on a page that is already upright.
        let tesseract = Tesseract::new("rus+ron+eng");
        let upright = gray(cut_out_content(image("passport_td3.png")));
        assert_eq!(
            choose_turn(&tesseract, &upright, 90, 90, &mut PageTimes::default())
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_passport_page_is_turned_upright_whichever_way_it_lies() {
        if !have_tesseract() {
            return;
        }
        let tesseract = Tesseract::new("rus+ron+eng");
        let upright = gray(cut_out_content(image("passport_td3.png")));
        for lying in [0_u16, 90, 180, 270] {
            let laid = Arc::new(turned(&upright, lying).into_owned());
            let right = (360 - lying) % 360;
            // Orientation detection's usual mistake: the opposite turn.
            let wrong = (right + 180) % 360;
            let mut times = PageTimes::default();
            let turn = choose_turn(&tesseract, &laid, 0, wrong, &mut times)
                .await
                .unwrap();
            assert_eq!(turn, right, "lying {lying}");
            // Upright at once; upside down after its opposite; sideways after
            // the first reading showed the line and the wrong end of it.
            let tried = match lying {
                0 => 1,
                180 => 2,
                _ => 3,
            };
            assert_eq!(times.turns_tried, tried, "lying {lying}");
        }
    }

    #[tokio::test]
    async fn a_page_of_text_keeps_the_detected_turn() {
        if !have_tesseract() {
            return;
        }
        let tesseract = Tesseract::new("rus+ron+eng");
        let page = gray(image("scan_ru.png"));
        let mut times = PageTimes::default();
        assert_eq!(
            choose_turn(&tesseract, &page, 0, 0, &mut times)
                .await
                .unwrap(),
            0
        );
        assert_eq!(times.turns_tried, 1);
    }

    #[tokio::test]
    async fn a_page_of_text_is_read_once_and_not_searched() {
        if !have_tesseract() {
            return;
        }
        let tesseract = Tesseract::new("rus+ron+eng");
        let read = ocr_page(
            &tesseract,
            image("scan_ru.png"),
            true,
            72.0 / 300.0,
            None,
            true,
        )
        .await
        .unwrap();
        assert_eq!(read.turn, 0);
        assert_eq!((read.times.reads, read.times.turns_tried), (1, 0));
        assert!(read.page.text.contains("Паспорт"), "{}", read.page.text);
    }

    #[tokio::test]
    async fn an_upside_down_passport_page_is_read_once_upright() {
        if !have_tesseract() {
            return;
        }
        let tesseract = Tesseract::new("rus+ron+eng");
        let read = ocr_page(
            &tesseract,
            image("passport_td3.png").rotate180(),
            true,
            72.0 / 300.0,
            None,
            true,
        )
        .await
        .unwrap();
        assert_eq!(read.turn, 180);
        assert!(
            verified_zone(&read.page, &read.mrz_text),
            "{:?}",
            read.mrz_text
        );
        // Turns are tried on the small copy; the page is read in full once.
        assert_eq!(read.times.reads, 1);
    }

    /// For tuning the turn search: each page of the `PAPERJAM_BENCH` files
    /// (the first pages of a PDF) laid every way, with the orientation verdict
    /// and how every turn of its search copy reads.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "benchmark: needs PAPERJAM_BENCH"]
    async fn bench_turns() {
        let tesseract = Tesseract::new("eng+rus+ron");
        let files = std::env::var("PAPERJAM_BENCH").unwrap_or_default();
        for path in files.split(';').filter(|p| !p.trim().is_empty()) {
            let path = std::path::Path::new(path.trim());
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let bytes = std::fs::read(path).unwrap();
            let pages: Vec<DynamicImage> = if name.ends_with(".pdf") {
                let renderer = HayroRenderer.open(&bytes).unwrap();
                (1..=renderer.page_count().min(SEARCHED_PAGES))
                    .map(|n| DynamicImage::ImageRgb8(renderer.render(n, OCR_DPI).unwrap()))
                    .collect()
            } else {
                vec![::image::load_from_memory(&bytes).unwrap()]
            };
            for (i, page) in pages.into_iter().enumerate() {
                let (cut, _) = cut_out(fit(page, OCR_MAX_EDGE));
                let gray = DynamicImage::ImageLuma8(cut.to_luma8());
                for lying in TURNS {
                    let laid = Arc::new(turned(&gray, lying).into_owned());
                    let osd = tesseract
                        .detect_orientation(&shrunk(&laid, SMALL_EDGE))
                        .await
                        .unwrap();
                    let searched = shrunk(&laid, SEARCH_EDGE);
                    let mut line = format!(
                        "TURNS {name} p{} lying={lying}: osd={}@{:.2} |",
                        i + 1,
                        osd.degrees,
                        osd.confidence
                    );
                    for turn in TURNS {
                        let started = Instant::now();
                        let (score, sideways) =
                            turn_score(&tesseract, &searched, turn).await.unwrap();
                        line.push_str(&format!(
                            " {turn}={score}{}/{}ms",
                            match sideways {
                                Some(true) => "s",
                                Some(false) => "f",
                                None => "?",
                            },
                            started.elapsed().as_millis()
                        ));
                    }
                    eprintln!("{line}");
                }
            }
        }
    }

    /// Times the local pipeline on files outside the corpus, page by page;
    /// not part of the gate:
    /// `PAPERJAM_BENCH_WORKER=target/release/paperjam PAPERJAM_BENCH="a.pdf;b.png" cargo test --release --lib bench_local -- --ignored --nocapture`
    /// (`RUST_LOG=paperjam=debug` adds every Tesseract run).
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "benchmark: needs PAPERJAM_BENCH"]
    async fn bench_local() {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("paperjam=info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
        let tesseract = Tesseract::new("eng+rus+ron");
        // The test binary is not `paperjam`: point at the release binary.
        let worker = std::env::var("PAPERJAM_BENCH_WORKER").expect("PAPERJAM_BENCH_WORKER");
        let isolation = Isolation::command(
            worker.into(),
            vec!["extract-worker".into()],
            Duration::from_secs(120),
            Some(1536 << 20),
        );
        let files = std::env::var("PAPERJAM_BENCH").unwrap_or_default();
        for path in files.split(';').filter(|p| !p.trim().is_empty()) {
            let path = std::path::Path::new(path.trim());
            let bytes = std::fs::read(path).unwrap();
            let extension = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let started = Instant::now();
            let out = match extension.as_str() {
                "pdf" => pdf(&isolation, &tesseract, bytes, pages_in_flight(1)).await,
                "jpg" | "jpeg" => {
                    super::image(&isolation, &tesseract, bytes, "image/jpeg".into()).await
                }
                other => {
                    super::image(&isolation, &tesseract, bytes, format!("image/{other}")).await
                }
            }
            .unwrap();
            let took = started.elapsed();
            let name = path.file_name().unwrap().to_string_lossy();
            for page in &out.pages {
                let Some(ocr) = &page.ocr else {
                    eprintln!("BENCH {name} p{}: not read", page.n);
                    continue;
                };
                eprintln!(
                    "BENCH {name} p{}: words={} legible={} conf={:.1} mrz_ok={} size={:?}",
                    page.n,
                    ocr.text.split_whitespace().count(),
                    ocr.legible_chars(),
                    ocr.mean_conf,
                    verified_zone(ocr, &page.mrz_text),
                    page.size,
                );
            }
            let clean_bytes = out.clean.as_deref().map_or(0, |pages| {
                clean::write_pdf(pages, &mut std::io::sink()).unwrap_or(0)
            });
            eprintln!(
                "BENCH {name}: {} ms, clean copy {} bytes",
                took.as_millis(),
                clean_bytes
            );
        }
    }

    #[test]
    fn zone_lines_are_told_from_prose() {
        // The ICAO 9303 specimen ID card.
        let zone = "I<UTOD231458907<<<<<<<<<<<<<<<\n7408122F1204159UTO<<<<<<<<<<<6\nERIKSSON<<ANNA<MARIA<<<<<<<<<<";
        assert_eq!(zone_lines(zone), 3);
        assert_eq!(
            zone_lines("Plain words on a line of an ordinary document page\nsecond"),
            0
        );
        // A zone read upside down: capitals, a few stray marks and one `<`.
        assert_eq!(
            zone_lines("te HOXUMOHSNH-AHXON NOSOXAWSOHIH \u{fffd}04. <"),
            0
        );
        // A passport's second line with the personal number filled in.
        assert_eq!(
            zone_lines("TS12345675UT08503127M32090112001234567890<16"),
            1
        );
    }

    #[test]
    fn photos_are_read_at_a4_resolution_at_most() {
        let photo = DynamicImage::new_rgb8(6048, 8064);
        let fitted = fit(photo, OCR_MAX_EDGE);
        assert_eq!(fitted.height(), OCR_MAX_EDGE);
        assert_eq!(fitted.width(), 2631);
        let scan = DynamicImage::new_rgb8(1240, 1754);
        assert_eq!(fit(scan, OCR_MAX_EDGE).dimensions(), (1240, 1754));
    }

    #[test]
    fn an_upright_page_is_borrowed_not_copied() {
        let page = DynamicImage::new_luma8(30, 20);
        assert!(matches!(turned(&page, 0), Cow::Borrowed(_)));
        assert_eq!(turned(&page, 90).dimensions(), (20, 30));
    }

    #[test]
    fn only_sparse_pages_are_searched() {
        let page = |words: usize| OcrPage {
            text: vec!["word"; words].join(" "),
            mean_conf: 80.0,
            reading: Reading::default(),
            mrz_region: None,
        };
        assert!(is_sparse(&page(40)));
        assert!(!is_sparse(&page(600)));
    }

    #[test]
    fn fallback_keeps_first_turn_when_all_turns_tie_or_barely_differ() {
        // Low scores (< MIN_LEGIBLE) never trigger a turn flip.
        let empty = Tried(vec![(0, 0), (180, 0), (90, 0), (270, 0)]);
        assert_eq!(pick_fallback(&empty, 0), 0);

        let weak = Tried(vec![(0, 0), (180, 7), (90, 0), (270, 0)]);
        assert_eq!(pick_fallback(&weak, 0), 0);

        // A small margin over `first` (< 25%) keeps `first`.
        let narrow = Tried(vec![(0, 40), (180, 48), (90, 5), (270, 5)]);
        assert_eq!(pick_fallback(&narrow, 0), 0);

        // A clear winner (>= MIN_LEGIBLE and > 1.25x baseline) wins.
        let clear = Tried(vec![(0, 20), (180, 40), (90, 5), (270, 5)]);
        assert_eq!(pick_fallback(&clear, 0), 180);
    }

    #[test]
    fn pages_in_flight_is_clamped() {
        assert!((2..=MAX_PAGES_IN_FLIGHT).contains(&pages_in_flight(0)));
        assert!((2..=MAX_PAGES_IN_FLIGHT).contains(&pages_in_flight(1)));
        assert_eq!(pages_in_flight(64), 2);
    }

    /// The clean page of `pdf`'s first page as the pipeline builds it for a
    /// scan: rendered at the OCR resolution, then encoded with the density the
    /// PDF's own image has.
    async fn clean_page_of(pdf: &[u8]) -> clean::Page {
        let native = OxideExtractor.open(pdf).unwrap().page_image_density(1);
        let render = HayroRenderer.open(pdf).unwrap().render(1, OCR_DPI).unwrap();
        let source = CleanSource {
            image: render,
            pt_per_px: 72.0 / OCR_DPI as f32,
            changed: false,
            visual_changed: false,
            native_px_per_pt: native,
        };
        encode_clean(source, false).await.unwrap().0.0
    }

    #[tokio::test]
    async fn the_clean_page_of_a_150_dpi_scan_is_kept_at_150_dpi() {
        let bytes = std::fs::read(fixture("invoice_scan_en.pdf")).unwrap();
        let page = clean_page_of(&bytes).await;
        // The scan is 1240 x 1754 pixels; the 300 DPI render is twice that.
        let dpi = page.width as f32 / page.width_pt * 72.0;
        assert!(
            (dpi - 150.0).abs() < 2.0,
            "{dpi} DPI, {} px wide",
            page.width
        );
        assert!((page.width_pt - 595.28).abs() < 1.0, "{}", page.width_pt);

        // Against the copy made from the same render without the limit.
        let render = HayroRenderer
            .open(&bytes)
            .unwrap()
            .render(1, OCR_DPI)
            .unwrap();
        let old = clean::page(render, 72.0 / OCR_DPI as f32).unwrap();
        eprintln!(
            "invoice_scan_en.pdf clean page: {} px / {} bytes before, {} px / {} bytes now",
            old.width,
            old.jpeg.len(),
            page.width,
            page.jpeg.len()
        );
        assert!(page.jpeg.len() < old.jpeg.len());
    }

    #[tokio::test]
    async fn the_clean_page_of_a_300_dpi_scan_is_unchanged() {
        let scan = clean::page(
            ::image::RgbImage::from_fn(2480, 3508, |x, y| {
                let v = if (x / 40 + y / 40) % 2 == 0 { 30 } else { 240 };
                ::image::Rgb([v, v, v])
            }),
            72.0 / 300.0,
        )
        .unwrap();
        let page = clean_page_of(&clean::pdf(&[scan])).await;
        assert!(page.width.abs_diff(2480) <= 2 && page.height.abs_diff(3508) <= 2);
        assert!((page.width_pt - 595.2).abs() < 1.0, "{}", page.width_pt);
    }
}
