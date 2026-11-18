//! OCR through the `tesseract` command-line tool (spec §7.2).
//!
//! The binary is run as a child process (image on stdin, results on stdout)
//! so a crash or a hang in Tesseract never takes the server down: the child
//! is killed when the timeout expires.

use std::ffi::OsString;
use std::io::Cursor;
use std::process::Stdio;
use std::time::{Duration, Instant};

use ::image::{DynamicImage, ImageFormat};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::error::{Result, ServiceError};

/// Default wall-clock limit for one Tesseract run.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// One recognised page.
#[derive(Debug, Clone)]
pub struct OcrPage {
    pub text: String,
    /// Tesseract's TSV output (word boxes and confidences), zstd-compressed.
    pub tsv: Vec<u8>,
    /// Mean word confidence in `0..=100` (Tesseract's own scale); `0` when no
    /// word was recognised.
    pub mean_conf: f32,
}

/// Words read with at least this confidence count as legible.
const LEGIBLE_CONFIDENCE: f64 = 80.0;

impl OcrPage {
    /// How much of the page was read upright with confidence: see
    /// [`Reading::legible`].
    pub fn legible_chars(&self) -> usize {
        self.reading().legible
    }

    /// What the page's words say about how it is turned.
    pub fn reading(&self) -> Reading {
        let Ok(tsv) = zstd::decode_all(self.tsv.as_slice()) else {
            return Reading::default();
        };
        reading_of(&String::from_utf8_lossy(&tsv))
    }
}

/// What the words of one reading say about how the page is turned. The
/// automatic page mode reads sideways text too, as vertical lines of words
/// standing on end; text turned upside down comes out as short, doubtful
/// fragments. So of one image turned four ways the upright one is the most
/// legible, and the words of a sideways one stand.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reading {
    /// Characters of the words recognised at [`LEGIBLE_CONFIDENCE`] or better
    /// that lie flat.
    pub legible: usize,
    /// Words of two characters or more, and how many of them stand on end
    /// (taller than wide). A word of one character may be taller than wide
    /// whichever way it is turned.
    pub words: usize,
    pub standing: usize,
    /// The words that lie flat, line by line.
    pub flat_text: String,
}

/// [`Reading`] of a TSV report.
fn reading_of(tsv: &str) -> Reading {
    let mut reading = Reading::default();
    let mut line: Option<(&str, &str, &str)> = None;
    for row in tsv.lines().skip(1) {
        let cols: Vec<&str> = row.splitn(12, '\t').collect();
        if cols.len() < 12 || cols[0] != "5" {
            continue;
        }
        let text = cols[11].trim();
        let (Ok(width), Ok(height), Ok(confidence)) = (
            cols[8].parse::<u32>(),
            cols[9].parse::<u32>(),
            cols[10].parse::<f64>(),
        ) else {
            continue;
        };
        let chars = text.chars().count();
        if chars == 0 {
            continue;
        }
        let flat = chars < 2 || width >= height;
        if chars >= 2 {
            reading.words += 1;
            reading.standing += usize::from(!flat);
        }
        if !flat {
            continue;
        }
        if confidence >= LEGIBLE_CONFIDENCE {
            reading.legible += chars;
        }
        let here = (cols[2], cols[3], cols[4]);
        match line {
            None => {}
            Some(previous) if previous == here => reading.flat_text.push(' '),
            Some(_) => reading.flat_text.push('\n'),
        }
        line = Some(here);
        reading.flat_text.push_str(text);
    }
    reading
}

#[derive(Debug, Clone)]
pub struct Tesseract {
    binary: OsString,
    langs: String,
    timeout: Duration,
}

impl Tesseract {
    /// `langs` is Tesseract's `-l` value, e.g. `"rus+ron+eng"`.
    pub fn new(langs: impl Into<String>) -> Self {
        Self {
            binary: OsString::from("tesseract"),
            langs: langs.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The same Tesseract with other `-l` languages.
    #[must_use]
    pub fn with_langs(mut self, langs: impl Into<String>) -> Self {
        self.langs = langs.into();
        self
    }

    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<OsString>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Recognises `img` with the configured languages.
    pub async fn ocr(&self, img: &DynamicImage) -> Result<OcrPage> {
        self.recognise("3", img).await
    }

    /// Recognises sparse or boxed text (`--psm 11`) as a fallback when automatic
    /// page segmentation drops valid text regions.
    pub async fn ocr_sparse(&self, img: &DynamicImage) -> Result<OcrPage> {
        self.recognise("11", img).await
    }

    async fn recognise(&self, psm: &str, img: &DynamicImage) -> Result<OcrPage> {
        let stdout = self
            .run(&["--psm", psm, "-l", &self.langs, "tsv"], img)
            .await?;
        let tsv = String::from_utf8_lossy(&stdout);
        let (text, mean_conf) = parse_tsv(&tsv);
        let tsv = zstd::encode_all(tsv.as_bytes(), 6)
            .map_err(|_| ServiceError::internal("ocr_failed", "could not compress OCR data"))?;
        Ok(OcrPage {
            text,
            tsv,
            mean_conf,
        })
    }

    /// Orientation of the page in degrees: `0`, `90`, `180` or `270`
    /// (clockwise rotation needed to make it upright), via `--psm 0`.
    /// A page with too little text to judge, or an unsure verdict, counts as
    /// upright.
    pub async fn detect_rotation(&self, img: &DynamicImage) -> Result<u16> {
        Ok(self.detect_orientation(img).await?.trusted())
    }

    /// Tesseract's orientation verdict on `img` (`--psm 0`) and how sure it
    /// is; a page with too little text to judge gives `0` with no confidence.
    pub async fn detect_orientation(&self, img: &DynamicImage) -> Result<Orientation> {
        let stdout = match self.run(&["--psm", "0", "-l", "osd"], img).await {
            Ok(out) => out,
            Err(e) if e.code == "ocr_failed" => return Ok(Orientation::default()),
            Err(e) => return Err(e),
        };
        let orientation = parse_osd(&String::from_utf8_lossy(&stdout));
        tracing::debug!(
            degrees = orientation.degrees,
            confidence = orientation.confidence,
            "orientation detected"
        );
        Ok(orientation)
    }

    /// Reads a machine-readable zone: English model, only the characters an
    /// MRZ can contain, treating the image as one block of text. The general
    /// multi-language pass confuses `<` and Latin letters with Cyrillic ones.
    pub async fn ocr_mrz(&self, img: &DynamicImage) -> Result<String> {
        let whitelist = format!("tessedit_char_whitelist={MRZ_CHARS}");
        let stdout = self
            .run(&["--psm", "6", "-l", "eng", "-c", &whitelist], img)
            .await?;
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }

    /// Runs `tesseract stdin stdout <args>` with `img` as PNG on stdin.
    async fn run(&self, args: &[&str], img: &DynamicImage) -> Result<Vec<u8>> {
        let started = Instant::now();
        let mut png = Vec::new();
        img.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .map_err(|_| ServiceError::internal("ocr_failed", "could not encode the page"))?;
        let encoded = started.elapsed();

        let mut child = Command::new(&self.binary)
            .args(["stdin", "stdout"])
            .args(args)
            // Pages are read in parallel, one Tesseract each; its own OpenMP
            // threads would only compete with the other runs for the cores.
            .env("OMP_THREAD_LIMIT", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                tracing::warn!(error = %e, "tesseract could not be started");
                ServiceError::unavailable("ocr_unavailable", "Tesseract is not available")
            })?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let feed = async move {
            // Tesseract may exit before reading everything (e.g. on a bad
            // language); that surfaces through its exit status, not here.
            let _ = stdin.write_all(&png).await;
            let _ = stdin.shutdown().await;
        };

        // Dropping the future on timeout drops the child, which kills it.
        let run = async { tokio::join!(feed, child.wait_with_output()).1 };
        let output = tokio::time::timeout(self.timeout, run)
            .await
            .map_err(|_| ServiceError::unavailable("ocr_timeout", "OCR took too long"))?
            .map_err(|e| {
                tracing::warn!(error = %e, "tesseract run failed");
                ServiceError::internal("ocr_failed", "Tesseract failed")
            })?;
        if !output.status.success() {
            tracing::debug!(
                status = %output.status,
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "tesseract exited with an error"
            );
            return Err(ServiceError::internal("ocr_failed", "Tesseract failed"));
        }
        tracing::debug!(
            args = %args.join(" "),
            width = img.width(),
            height = img.height(),
            encode_ms = encoded.as_millis() as u64,
            ms = started.elapsed().as_millis() as u64,
            "tesseract run"
        );
        Ok(output.stdout)
    }
}

/// Below this orientation confidence the OSD verdict is noise: on sparse
/// pages such as an ID card scanned onto an A4 sheet it confidently turns
/// upright pages over.
const MIN_OSD_CONFIDENCE: f32 = 2.0;

/// What orientation detection says about a page.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Orientation {
    /// The clockwise rotation it asks for: `0`, `90`, `180` or `270`.
    pub degrees: u16,
    /// Tesseract's orientation confidence; `0` when it could not tell.
    pub confidence: f32,
}

impl Orientation {
    /// The rotation, with an unsure verdict counted as upright.
    pub fn trusted(self) -> u16 {
        if self.confidence < MIN_OSD_CONFIDENCE {
            0
        } else {
            self.degrees
        }
    }
}

/// The verdict of an OSD report.
fn parse_osd(report: &str) -> Orientation {
    let value = |key: &str| {
        report
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .map(str::trim)
    };
    let confidence = value("Orientation confidence:")
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|c| c.is_finite())
        .unwrap_or(0.0);
    let degrees = value("Rotate:")
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|d| matches!(d, 90 | 180 | 270))
        .unwrap_or(0);
    Orientation {
        degrees,
        confidence,
    }
}

/// The characters a machine-readable zone is made of.
const MRZ_CHARS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789<";
/// A line with at least this many `<` is taken for MRZ filler.
const MIN_FILLER: usize = 8;

/// Where a machine-readable zone probably is in the page that produced
/// `tsv`: `(left, top, width, height)` from just above the first line with a
/// lot of `<` filler down to the bottom of the page (the zone is the last
/// thing on a passport page or ID card).
pub fn mrz_region(tsv: &str) -> Option<(u32, u32, u32, u32)> {
    let mut page: Option<(u32, u32)> = None;
    // (top, height, filler count) per line, in reading order.
    let mut lines: Vec<(u32, u32, usize)> = Vec::new();
    let mut current: Option<(&str, &str, &str)> = None;
    for row in tsv.lines().skip(1) {
        let cols: Vec<&str> = row.splitn(12, '\t').collect();
        if cols.len() < 11 {
            continue;
        }
        let number = |i: usize| cols[i].parse::<u32>().ok();
        match cols[0] {
            "1" => page = number(8).zip(number(9)),
            "4" => {
                current = Some((cols[2], cols[3], cols[4]));
                if let (Some(top), Some(height)) = (number(7), number(9)) {
                    lines.push((top, height, 0));
                }
            }
            "5" if cols.len() == 12 && current == Some((cols[2], cols[3], cols[4])) => {
                if let Some(line) = lines.last_mut() {
                    line.2 += cols[11].matches('<').count();
                }
            }
            _ => {}
        }
    }
    let (width, height) = page?;
    let &(top, line_height, _) = lines.iter().find(|l| l.2 >= MIN_FILLER)?;
    // The line above the first filler-heavy one belongs to the zone too: the
    // first line of a three-line ID card zone has hardly any filler.
    let top = top
        .saturating_sub(line_height * 3 / 2)
        .min(height.saturating_sub(1));
    Some((0, top, width, height - top))
}

/// Extracts plain text and the mean word confidence from Tesseract TSV.
pub fn parse_tsv(tsv: &str) -> (String, f32) {
    const WORD_LEVEL: &str = "5";
    let mut text = String::new();
    let mut previous: Option<(&str, &str, &str)> = None;
    let (mut conf_sum, mut words) = (0.0_f64, 0_u32);

    for row in tsv.lines().skip(1) {
        let cols: Vec<&str> = row.splitn(12, '\t').collect();
        if cols.len() < 12 || cols[0] != WORD_LEVEL || cols[11].trim().is_empty() {
            continue;
        }
        let (block, par, line) = (cols[2], cols[3], cols[4]);
        match previous {
            None => {}
            Some((b, p, _)) if (b, p) != (block, par) => text.push_str("\n\n"),
            Some((_, _, l)) if l != line => text.push('\n'),
            Some(_) => text.push(' '),
        }
        previous = Some((block, par, line));
        text.push_str(cols[11].trim());
        if let Ok(conf) = cols[10].parse::<f64>()
            && conf >= 0.0
        {
            conf_sum += conf;
            words += 1;
        }
    }
    let mean = if words == 0 {
        0.0
    } else {
        (conf_sum / f64::from(words)) as f32
    };
    (text, mean)
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Instant;

    use super::*;
    use crate::extract::fixture;

    /// True when `tesseract` and every language in `langs` is installed;
    /// otherwise prints a one-line skip message.
    fn have(langs: &[&str]) -> bool {
        let out = Command::new("tesseract").arg("--list-langs").output();
        let Ok(out) = out else {
            eprintln!("skipped: tesseract is not installed");
            return false;
        };
        let listed = String::from_utf8_lossy(&out.stdout).into_owned();
        for lang in langs {
            if !listed.lines().any(|l| l.trim() == *lang) {
                eprintln!("skipped: tesseract language pack `{lang}` is missing");
                return false;
            }
        }
        true
    }

    fn scan() -> DynamicImage {
        ::image::open(fixture("scan_ru.png")).unwrap()
    }

    #[test]
    fn parse_tsv_joins_words_into_lines_and_paragraphs() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
1\t1\t0\t0\t0\t0\t0\t0\t100\t100\t-1\t\n\
5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t90.0\tHello\n\
5\t1\t1\t1\t1\t2\t0\t0\t10\t10\t70.0\tworld\n\
5\t1\t1\t1\t2\t1\t0\t0\t10\t10\t80.0\tsecond\n\
5\t1\t1\t2\t1\t1\t0\t0\t10\t10\t60.0\tpara\n\
5\t1\t2\t1\t1\t1\t0\t0\t10\t10\t-1\t\n";
        let (text, conf) = parse_tsv(tsv);
        assert_eq!(text, "Hello world\nsecond\n\npara");
        assert!((conf - 75.0).abs() < 0.01, "mean conf {conf}");
    }

    #[test]
    fn only_confident_words_lying_flat_are_legible() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t1\t1\t1\t1\t0\t0\t90\t20\t91.0\tPassport\n\
5\t1\t1\t1\t1\t2\t0\t0\t20\t90\t93.0\tstanding\n\
5\t1\t1\t1\t1\t3\t0\t0\t10\t20\t95.0\tI\n\
5\t1\t1\t1\t1\t4\t0\t0\t90\t20\t40.0\tdoubtful\n\
5\t1\t1\t1\t2\t1\t0\t0\t90\t20\t85.0\tsecond\n";
        let reading = reading_of(tsv);
        // "Passport", "I" and "second"; the word on end was read off sideways
        // text, the doubtful one is not legible.
        assert_eq!(reading.legible, 15);
        assert_eq!((reading.words, reading.standing), (4, 1));
        assert_eq!(reading.flat_text, "Passport I doubtful\nsecond");
        assert_eq!(reading_of("garbage"), Reading::default());
    }

    #[test]
    fn mrz_region_starts_at_the_first_filler_heavy_line() {
        let row = |level: u32, block: u32, line: u32, word: u32, top: u32, text: &str| {
            format!("{level}\t1\t{block}\t1\t{line}\t{word}\t10\t{top}\t900\t40\t90\t{text}\n")
        };
        let mut tsv = String::from(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n",
        );
        tsv.push_str("1\t1\t0\t0\t0\t0\t0\t0\t1500\t1050\t-1\t\n");
        tsv.push_str(&row(4, 1, 1, 0, 100, ""));
        tsv.push_str(&row(5, 1, 1, 1, 100, "Surname"));
        tsv.push_str(&row(4, 2, 1, 0, 900, ""));
        tsv.push_str(&row(
            5,
            2,
            1,
            1,
            900,
            "P<UTOTESTOV<<IVAN<<<<<<<<<<<<<<<<<<<<<<<<<<<",
        ));
        tsv.push_str(&row(4, 2, 2, 0, 950, ""));
        tsv.push_str(&row(
            5,
            2,
            2,
            1,
            950,
            "TS12345675UTO8503127M3209011<<<<<<<<<<<<<<04",
        ));
        let (left, top, width, height) = mrz_region(&tsv).unwrap();
        assert_eq!((left, width), (0, 1500));
        assert!((830..=860).contains(&top), "top {top}");
        assert_eq!(top + height, 1050);
        assert_eq!(
            mrz_region("level\n1\t1\t0\t0\t0\t0\t0\t0\t10\t10\t-1\t\n"),
            None
        );
    }

    #[test]
    fn parse_tsv_of_nothing_is_empty() {
        let (text, conf) = parse_tsv("");
        assert_eq!(text, "");
        assert_eq!(conf, 0.0);
    }

    #[tokio::test]
    async fn ocr_reads_russian_fixture() {
        if !have(&["rus", "eng"]) {
            return;
        }
        let page = Tesseract::new("rus+eng").ocr(&scan()).await.unwrap();
        assert!(page.text.contains("Паспорт"), "got: {}", page.text);
        assert!(page.mean_conf > 60.0, "conf {}", page.mean_conf);
        let tsv = zstd::decode_all(page.tsv.as_slice()).unwrap();
        assert!(String::from_utf8(tsv).unwrap().starts_with("level\t"));
    }

    #[tokio::test]
    async fn tilted_page_is_readable_after_preparation() {
        if !have(&["rus", "eng"]) {
            return;
        }
        let gray = scan().to_luma8();
        let tilted = DynamicImage::ImageLuma8(crate::extract::preprocess::rotate(&gray, 6.0, 255));
        let prepared = crate::extract::preprocess::prepare(&tilted);
        let page = Tesseract::new("rus+eng").ocr(&prepared).await.unwrap();
        assert!(page.text.contains("Паспорт"), "got: {}", page.text);
    }

    #[tokio::test]
    async fn upside_down_page_is_detected() {
        if !have(&["osd"]) {
            return;
        }
        let tess = Tesseract::new("rus+eng");
        assert_eq!(tess.detect_rotation(&scan()).await.unwrap(), 0);
        let flipped = scan().rotate180();
        assert_eq!(tess.detect_rotation(&flipped).await.unwrap(), 180);
    }

    #[tokio::test]
    async fn sideways_page_turns_the_right_way() {
        if !have(&["osd"]) {
            return;
        }
        let tess = Tesseract::new("rus+eng");
        // Turned a quarter clockwise, it needs three more quarters clockwise.
        assert_eq!(tess.detect_rotation(&scan().rotate90()).await.unwrap(), 270);
        assert_eq!(tess.detect_rotation(&scan().rotate270()).await.unwrap(), 90);
    }

    #[test]
    fn osd_report_is_trusted_only_when_confident() {
        let report = |rotate: u16, conf: f32| {
            format!(
                "Page number: 0\nOrientation in degrees: 0\nRotate: {rotate}\nOrientation confidence: {conf}\n"
            )
        };
        let trusted = |report: &str| parse_osd(report).trusted();
        assert_eq!(trusted(&report(270, 22.1)), 270);
        assert_eq!(trusted(&report(90, 4.5)), 90);
        assert_eq!(trusted(&report(180, 9.0)), 180);
        // An upright ID card on a mostly blank A4 scan, read as upside down.
        assert_eq!(trusted(&report(180, 0.7)), 0);
        assert_eq!(
            parse_osd(&report(180, 0.7)),
            Orientation {
                degrees: 180,
                confidence: 0.7
            }
        );
        assert_eq!(trusted("garbage"), 0);
        assert_eq!(parse_osd("garbage"), Orientation::default());
    }

    #[tokio::test]
    async fn timeout_kills_process() {
        if !have(&["eng"]) {
            return;
        }
        let tess = Tesseract::new("eng").with_timeout(Duration::from_millis(1));
        let started = Instant::now();
        let err = tess.ocr(&scan()).await.unwrap_err();
        assert_eq!(err.code, "ocr_timeout");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn missing_binary_is_unavailable() {
        let tess = Tesseract::new("eng").with_binary("paperjam-no-such-binary");
        let err = tess.ocr(&scan_small()).await.unwrap_err();
        assert_eq!(err.code, "ocr_unavailable");
    }

    fn scan_small() -> DynamicImage {
        DynamicImage::new_luma8(8, 8)
    }
}
