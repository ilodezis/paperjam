//! Contract tests for the synthetic fixture corpus.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use xtask::fixtures::{FILES, generate, id_card_mrz, passport_mrz};

fn generate_into_temp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = dir.path().join("gen");
    generate(&out).expect("generate fixtures");
    (dir, out)
}

fn sha(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// ICAO Doc 9303 Part 3 §4.9 check digit: weights 7,3,1 over digits
/// (0-9), letters (A=10..Z=35) and the filler `<` (0), modulo 10.
fn icao_check(field: &str) -> u32 {
    const WEIGHTS: [u32; 3] = [7, 3, 1];
    field
        .bytes()
        .enumerate()
        .map(|(i, b)| {
            let v = match b {
                b'0'..=b'9' => u32::from(b - b'0'),
                b'A'..=b'Z' => u32::from(b - b'A') + 10,
                b'<' => 0,
                other => panic!("character {:?} is not allowed in an MRZ", other as char),
            };
            v * WEIGHTS[i % 3]
        })
        .sum::<u32>()
        % 10
}

fn digit(line: &str, at: usize) -> u32 {
    line.as_bytes()[at]
        .checked_sub(b'0')
        .filter(|d| *d < 10)
        .unwrap_or_else(|| panic!("position {at} of {line:?} is not a digit")) as u32
}

/// Returns which of the five TD3 check digits are wrong:
/// document number, birth date, expiry date, personal number, composite.
fn td3_failures(line2: &str) -> Vec<&'static str> {
    let composite = format!("{}{}{}", &line2[0..10], &line2[13..20], &line2[21..43]);
    let checks = [
        ("document number", &line2[0..9], 9),
        ("birth date", &line2[13..19], 19),
        ("expiry date", &line2[21..27], 27),
        ("personal number", &line2[28..42], 42),
        ("composite", composite.as_str(), 43),
    ];
    checks
        .into_iter()
        .filter(|(_, field, at)| icao_check(field) != digit(line2, *at))
        .map(|(name, _, _)| name)
        .collect()
}

#[test]
fn generation_is_deterministic() {
    let (_a, first) = generate_into_temp();
    let (_b, second) = generate_into_temp();
    for name in FILES {
        assert_eq!(
            sha(&first.join(name)),
            sha(&second.join(name)),
            "{name} differs between two runs"
        );
    }
}

#[test]
fn writes_exactly_the_listed_files_and_none_are_empty() {
    let (_dir, out) = generate_into_temp();
    let written: BTreeSet<String> = fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let expected: BTreeSet<String> = FILES.iter().map(|f| f.to_string()).collect();
    assert_eq!(written, expected);
    for name in FILES {
        assert!(
            fs::metadata(out.join(name)).unwrap().len() > 100,
            "{name} is suspiciously small"
        );
    }
}

#[test]
fn passport_mrz_is_a_valid_td3_with_the_agreed_values() {
    let [line1, line2] = passport_mrz(false);
    assert_eq!(line1.len(), 44);
    assert_eq!(line2.len(), 44);
    for l in [&line1, &line2] {
        assert!(
            l.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'<'),
            "{l}"
        );
    }
    assert!(line1.starts_with("P<UTOTESTOV<<IVAN<"), "{line1}");
    assert!(line2.starts_with("TS1234567"), "{line2}");
    assert_eq!(&line2[10..13], "UTO");
    assert_eq!(&line2[13..19], "850312");
    assert_eq!(&line2[20..21], "M");
    assert_eq!(&line2[21..27], "320901");
    assert_eq!(td3_failures(&line2), Vec::<&str>::new());
}

#[test]
fn bad_passport_mrz_differs_by_exactly_one_check_digit() {
    let [good1, good2] = passport_mrz(false);
    let [bad1, bad2] = passport_mrz(true);
    assert_eq!(good1, bad1);
    assert_eq!(bad2.len(), 44);
    assert_eq!(td3_failures(&bad2), vec!["document number"]);
    // Only the document number's check digit was touched.
    let differing: Vec<usize> = good2
        .bytes()
        .zip(bad2.bytes())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i)
        .collect();
    assert!(
        differing.contains(&9),
        "check digit at position 9 must differ"
    );
    assert!(
        differing.iter().all(|i| *i == 9 || *i == 43),
        "{differing:?}"
    );
}

#[test]
fn id_card_mrz_is_a_valid_td1() {
    let [l1, l2, l3] = id_card_mrz();
    for l in [&l1, &l2, &l3] {
        assert_eq!(l.len(), 30, "{l}");
        assert!(
            l.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'<'),
            "{l}"
        );
    }
    assert!(l1.starts_with("I<UTOTS7654321"), "{l1}");
    assert_eq!(&l2[0..6], "900704");
    assert_eq!(&l2[7..8], "F");
    assert_eq!(&l2[8..14], "310420");
    assert_eq!(&l2[15..18], "UTO");
    assert!(l3.starts_with("TESTOVA<<ANNA<"), "{l3}");

    assert_eq!(icao_check(&l1[5..14]), digit(&l1, 14), "document number");
    assert_eq!(icao_check(&l2[0..6]), digit(&l2, 6), "birth date");
    assert_eq!(icao_check(&l2[8..14]), digit(&l2, 14), "expiry date");
    let composite = format!("{}{}{}{}", &l1[5..30], &l2[0..7], &l2[8..15], &l2[18..29]);
    assert_eq!(icao_check(&composite), digit(&l2, 29), "composite");
}

#[test]
fn images_have_usable_dimensions() {
    let (_dir, out) = generate_into_temp();
    for name in [
        "scan_ru.png",
        "passport_td3.png",
        "passport_td3_badcheck.png",
        "id_card_td1.png",
    ] {
        let (w, h) = image::image_dimensions(out.join(name)).unwrap();
        assert!(w >= 800 && h >= 500, "{name} is only {w}x{h}");
    }
    let (w, h) = image::image_dimensions(out.join("scan_ru.png")).unwrap();
    assert!(h > w, "scan_ru.png is a portrait page");
}

#[test]
fn receipt_is_stored_landscape_with_exif_orientation_6() {
    let (_dir, out) = generate_into_temp();
    let path = out.join("receipt_photo_skewed.jpg");
    let bytes = fs::read(&path).unwrap();
    assert_eq!(&bytes[..3], [0xFF, 0xD8, 0xFF], "JPEG signature");
    // Stored pixels are landscape; orientation 6 turns them into a portrait page.
    let (w, h) = image::image_dimensions(&path).unwrap();
    assert!(w > h, "stored as {w}x{h}");
    assert!(
        bytes.windows(6).any(|w| w == b"Exif\0\0"),
        "EXIF APP1 segment"
    );
    // Little-endian IFD entry: tag 0x0112 (Orientation), type SHORT, count 1, value 6.
    let entry = [0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x00];
    assert!(
        bytes.windows(entry.len()).any(|w| w == entry),
        "orientation = 6"
    );
}

#[test]
fn pdf_fixtures_have_the_expected_structure() {
    let (_dir, out) = generate_into_temp();
    let read = |name: &str| fs::read(out.join(name)).unwrap();
    let has =
        |bytes: &[u8], needle: &str| bytes.windows(needle.len()).any(|w| w == needle.as_bytes());

    for name in FILES.iter().filter(|f| f.ends_with(".pdf")) {
        assert!(read(name).starts_with(b"%PDF-"), "{name} starts with %PDF-");
    }
    let encrypted = read("encrypted.pdf");
    assert!(
        has(&encrypted, "/Encrypt"),
        "encrypted.pdf has an /Encrypt entry"
    );
    assert!(has(&encrypted, "%%EOF"));

    let corrupt = read("corrupt.pdf");
    assert!(
        !has(&corrupt, "%%EOF") && !has(&corrupt, "startxref"),
        "corrupt.pdf is truncated"
    );

    // Image-only: a picture and no fonts, hence no text layer.
    let scan = read("invoice_scan_en.pdf");
    assert!(has(&scan, "/Image"), "invoice_scan_en.pdf embeds an image");
    assert!(!has(&scan, "/Font"), "invoice_scan_en.pdf has no text");

    // Born digital: fonts present.
    assert!(has(&read("contract_ru_digital.pdf"), "/Font"));
    assert!(has(&read("contract_ro_digital.pdf"), "/Font"));
    // A 12-page statement is not tiny.
    assert!(read("statement_multi_page.pdf").len() > 20_000);
    // Readable image underneath the broken text layer.
    let garbage = read("garbage_text_layer.pdf");
    assert!(has(&garbage, "/Image") && has(&garbage, "/Font"));
}

#[test]
fn heic_fixture_is_the_committed_static_file() {
    let (_dir, out) = generate_into_temp();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures/static/photo.heic");
    assert_eq!(sha(&out.join("photo.heic")), sha(&source));
    let bytes = fs::read(out.join("photo.heic")).unwrap();
    assert_eq!(&bytes[4..8], b"ftyp");
}
