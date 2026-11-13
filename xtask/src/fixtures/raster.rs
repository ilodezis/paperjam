//! Generation of raster image fixtures (PNG, JPEG).

use ab_glyph::{FontRef, PxScale};
use image::{Rgb, RgbImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_hollow_rect_mut, draw_text_mut};
use imageproc::geometric_transformations::{Border, Interpolation, rotate_about_center};
use imageproc::rect::Rect;

use super::mrz::{id_card_mrz, passport_mrz};

pub const NOTO_SANS_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../fixtures/fonts/NotoSans-Regular.ttf"
));

pub const NOTO_SANS_MONO_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../fixtures/fonts/NotoSansMono-Regular.ttf"
));

/// Generates `scan_ru.png`: portrait A4 at 150 dpi (1240x1754).
pub fn generate_scan_ru() -> anyhow::Result<Vec<u8>> {
    let font = FontRef::try_from_slice(NOTO_SANS_BYTES)?;
    let mut img = RgbImage::from_pixel(1240, 1754, Rgb([255, 255, 255]));

    let black = Rgb([0, 0, 0]);

    // Title: >= 56 px
    draw_text_mut(
        &mut img,
        black,
        120,
        140,
        PxScale::from(56.0),
        &font,
        "Паспорт гражданина",
    );

    let lines = [
        "Настоящий документ удостоверяет личность гражданина Республики",
        "Тестландия на территории государства и за его пределами.",
        "Паспорт выдан отделом внутренних дел Центрального района",
        "города Тествилль в соответствии с установленным регламентом.",
        "Владелец паспорта обязан бережно хранить данный документ",
        "и незамедлительно заявлять об утрате в регистрирующий орган.",
        "Записи в паспорте производятся уполномоченными должностными лицами.",
        "Самостоятельное внесение исправлений и отметок не допускается.",
        "Паспорт подлежит замене по достижении установленного возраста,",
        "а также в случае изменения персональных данных или непригодности.",
        "Государственный реестр учета населения ведет архив записей.",
        "Дата выдачи и подпись уполномоченного лица скрепляются печатью.",
        "Действителен для всех официальных юридических процедур.",
        "Серия и номер бланка внесены в единую информационную систему.",
    ];

    let mut y = 260;
    for line in lines {
        draw_text_mut(&mut img, black, 120, y, PxScale::from(36.0), &font, line);
        y += 65;
    }

    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    img.write_with_encoder(encoder)?;
    Ok(buf)
}

/// Generates passport TD3 page (1500x1050), good or bad check digit.
pub fn generate_passport_td3(bad_check: bool) -> anyhow::Result<Vec<u8>> {
    let font_sans = FontRef::try_from_slice(NOTO_SANS_BYTES)?;
    let font_mono = FontRef::try_from_slice(NOTO_SANS_MONO_BYTES)?;

    let mut img = RgbImage::from_pixel(1500, 1050, Rgb([255, 255, 255]));
    let black = Rgb([0, 0, 0]);
    let grey_label = Rgb([80, 80, 80]);
    let photo_bg = Rgb([220, 220, 220]);
    let photo_border = Rgb([160, 160, 160]);

    // Header
    draw_text_mut(
        &mut img,
        black,
        80,
        60,
        PxScale::from(44.0),
        &font_sans,
        "PASSPORT / ПАСПОРТ",
    );
    draw_text_mut(
        &mut img,
        grey_label,
        80,
        120,
        PxScale::from(24.0),
        &font_sans,
        "REPUBLIC OF TESTING / РЕСПУБЛИКА ТЕСТЛАНДИЯ",
    );

    // Photo placeholder box
    let photo_rect = Rect::at(80, 170).of_size(320, 430);
    draw_filled_rect_mut(&mut img, photo_rect, photo_bg);
    draw_hollow_rect_mut(&mut img, photo_rect, photo_border);
    draw_text_mut(
        &mut img,
        grey_label,
        140,
        370,
        PxScale::from(28.0),
        &font_sans,
        "PHOTO / ФОТО",
    );

    // Labelled fields
    let fields = [
        (440, 170, "Type / Тип", "P"),
        (600, 170, "Code / Код", "UTO"),
        (800, 170, "Passport No. / Номер паспорта", "TS1234567"),
        (440, 250, "Surname / Фамилия", "TESTOV / ТЕСТОВ"),
        (440, 330, "Given names / Имена", "IVAN / ИВАН"),
        (440, 410, "Nationality / Гражданство", "UTOPIAN"),
        (880, 410, "Sex / Пол", "M"),
        (440, 490, "Date of birth / Дата рождения", "12 MAR 1985"),
        (880, 490, "Place of birth / Место рождения", "TESTVILLE"),
        (440, 570, "Date of issue / Дата выдачи", "02 SEP 2022"),
        (880, 570, "Date of expiry / Срок действия", "01 SEP 2032"),
        (440, 650, "Authority / Орган выдачи", "MINISTRY OF TESTING"),
    ];

    for (x, y, label, val) in fields {
        draw_text_mut(
            &mut img,
            grey_label,
            x,
            y,
            PxScale::from(18.0),
            &font_sans,
            label,
        );
        draw_text_mut(
            &mut img,
            black,
            x,
            y + 24,
            PxScale::from(26.0),
            &font_sans,
            val,
        );
    }

    // MRZ lines
    let [mrz1, mrz2] = passport_mrz(bad_check);
    draw_text_mut(
        &mut img,
        black,
        100,
        870,
        PxScale::from(42.0),
        &font_mono,
        &mrz1,
    );
    draw_text_mut(
        &mut img,
        black,
        100,
        940,
        PxScale::from(42.0),
        &font_mono,
        &mrz2,
    );

    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    img.write_with_encoder(encoder)?;
    Ok(buf)
}

/// Generates `passport_td3_sideways_a4.png`: the passport page of
/// `passport_td3.png` lying in the corner of an A4 sheet (150 dpi), the whole
/// sheet scanned a quarter turn clockwise (1754x1240 landscape).
pub fn generate_passport_td3_sideways_a4() -> anyhow::Result<Vec<u8>> {
    let passport = image::load_from_memory(&generate_passport_td3(false)?)?.to_rgb8();
    let passport = image::imageops::resize(&passport, 900, 630, image::imageops::Lanczos3);

    let mut sheet = RgbImage::from_pixel(1240, 1754, Rgb([255, 255, 255]));
    image::imageops::replace(&mut sheet, &passport, 70, 90);
    draw_hollow_rect_mut(
        &mut sheet,
        Rect::at(70, 90).of_size(900, 630),
        Rgb([120, 120, 120]),
    );

    let sideways = image::imageops::rotate90(&sheet);
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    sideways.write_with_encoder(encoder)?;
    Ok(buf)
}

/// Generates `id_card_td1_sideways_a4.png`: `id_card_td1.png` shrunk to a
/// real card's size (1011x640 at 300 dpi) in the middle of an A4 sheet with a
/// few specks of dust, the sheet scanned a quarter turn counter-clockwise
/// (3508x2480 landscape), as a flatbed scan of a card laid down sideways.
pub fn generate_id_card_td1_sideways_a4() -> anyhow::Result<Vec<u8>> {
    let card = image::load_from_memory(&generate_id_card_td1()?)?.to_rgb8();
    let card = image::imageops::resize(&card, 1011, 640, image::imageops::Lanczos3);

    let mut sheet = RgbImage::from_pixel(2480, 3508, Rgb([252, 252, 252]));
    image::imageops::replace(&mut sheet, &card, 600, 1300);
    draw_hollow_rect_mut(
        &mut sheet,
        Rect::at(600, 1300).of_size(1011, 640),
        Rgb([150, 150, 150]),
    );
    // Dust on the glass: small dark flecks far from the card.
    for (x, y) in [(240, 280), (1960, 3000), (420, 3240), (2200, 520)] {
        for (dx, dy) in [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1), (1, 2)] {
            sheet.put_pixel(x + dx, y + dy, Rgb([40, 40, 40]));
        }
    }

    let sideways = image::imageops::rotate270(&sheet);
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    sideways.write_with_encoder(encoder)?;
    Ok(buf)
}

/// Generates `id_card_td1.png` (1500x950).
pub fn generate_id_card_td1() -> anyhow::Result<Vec<u8>> {
    let font_sans = FontRef::try_from_slice(NOTO_SANS_BYTES)?;
    let font_mono = FontRef::try_from_slice(NOTO_SANS_MONO_BYTES)?;

    let mut img = RgbImage::from_pixel(1500, 950, Rgb([255, 255, 255]));
    let black = Rgb([0, 0, 0]);
    let grey_label = Rgb([80, 80, 80]);
    let photo_bg = Rgb([220, 220, 220]);
    let photo_border = Rgb([160, 160, 160]);

    // Header
    draw_text_mut(
        &mut img,
        black,
        80,
        50,
        PxScale::from(40.0),
        &font_sans,
        "ID CARD / УДОСТОВЕРЕНИЕ ЛИЧНОСТИ",
    );
    draw_text_mut(
        &mut img,
        grey_label,
        80,
        105,
        PxScale::from(22.0),
        &font_sans,
        "REPUBLIC OF TESTING / РЕСПУБЛИКА ТЕСТЛАНДИЯ",
    );

    // Photo placeholder
    let photo_rect = Rect::at(80, 150).of_size(280, 360);
    draw_filled_rect_mut(&mut img, photo_rect, photo_bg);
    draw_hollow_rect_mut(&mut img, photo_rect, photo_border);
    draw_text_mut(
        &mut img,
        grey_label,
        130,
        320,
        PxScale::from(24.0),
        &font_sans,
        "PHOTO / ФОТО",
    );

    // Fields
    let fields = [
        (400, 150, "Document No. / Номер документа", "TS7654321"),
        (400, 220, "Surname / Фамилия", "TESTOVA / ТЕСТОВА"),
        (400, 290, "Given names / Имя", "ANNA / АННА"),
        (400, 360, "Date of birth / Дата рождения", "04 JUL 1990"),
        (850, 360, "Sex / Пол", "F"),
        (400, 430, "Nationality / Гражданство", "UTO"),
        (850, 430, "Date of expiry / Срок действия", "20 APR 2031"),
    ];

    for (x, y, label, val) in fields {
        draw_text_mut(
            &mut img,
            grey_label,
            x,
            y,
            PxScale::from(16.0),
            &font_sans,
            label,
        );
        draw_text_mut(
            &mut img,
            black,
            x,
            y + 20,
            PxScale::from(24.0),
            &font_sans,
            val,
        );
    }

    // 3 MRZ lines
    let [l1, l2, l3] = id_card_mrz();
    draw_text_mut(
        &mut img,
        black,
        100,
        650,
        PxScale::from(42.0),
        &font_mono,
        &l1,
    );
    draw_text_mut(
        &mut img,
        black,
        100,
        720,
        PxScale::from(42.0),
        &font_mono,
        &l2,
    );
    draw_text_mut(
        &mut img,
        black,
        100,
        790,
        PxScale::from(42.0),
        &font_mono,
        &l3,
    );

    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    img.write_with_encoder(encoder)?;
    Ok(buf)
}

/// Generates `receipt_photo_skewed.jpg`: portrait receipt, rotated 7° with background,
/// rotated 90° CCW into landscape, saved with EXIF Orientation 6 tag.
pub fn generate_receipt_photo_skewed() -> anyhow::Result<Vec<u8>> {
    let font_mono = FontRef::try_from_slice(NOTO_SANS_MONO_BYTES)?;

    // Canvas in portrait: 1000 x 1400
    let bg_color = Rgb([235, 230, 225]);
    let mut portrait = RgbImage::from_pixel(1000, 1400, bg_color);

    // Draw receipt paper in the center
    let paper_rect = Rect::at(180, 100).of_size(640, 1180);
    draw_filled_rect_mut(&mut portrait, paper_rect, Rgb([255, 255, 255]));
    draw_hollow_rect_mut(&mut portrait, paper_rect, Rgb([210, 205, 200]));

    let black = Rgb([0, 0, 0]);
    let lines = [
        (220, 160, "MAGAZIN TEST SRL"),
        (220, 200, "str. Testivil 42, mun. Chisinau"),
        (220, 240, "Cod fiscal: 1002600123456"),
        (220, 290, "Data: 2026-10-15   Ora: 14:32"),
        (220, 330, "BON FISCAL: #0004291"),
        (220, 370, "--------------------------------"),
        (220, 420, "Cafea espresso            35.00"),
        (220, 460, "Croissant cu unt          28.50"),
        (220, 500, "Apa minerala 0.5L         19.95"),
        (220, 540, "Ciocolata neagra          40.00"),
        (220, 590, "--------------------------------"),
        (220, 640, "TOTAL                     123.45 MDL"),
        (220, 690, "--------------------------------"),
        (220, 740, "TVA 20%                   20.58 MDL"),
        (220, 780, "CARD BANCAR               123.45 MDL"),
        (220, 840, "MULTUMIM PENTRU VIZITA!"),
        (220, 880, "VA MAI ASTEPTAM!"),
    ];

    for (x, y, text) in lines {
        draw_text_mut(
            &mut portrait,
            black,
            x,
            y,
            PxScale::from(24.0),
            &font_mono,
            text,
        );
    }

    // Rotate 7 degrees clockwise about center
    let theta = 7.0f32.to_radians();
    let rotated = rotate_about_center(
        &portrait,
        theta,
        Interpolation::Bilinear,
        Border::Constant(bg_color),
    );

    // Rotate 90 degrees counter-clockwise (rotate270 clockwise) -> landscape (1400x1000)
    let landscape = image::imageops::rotate270(&rotated);

    // Encode landscape image as JPEG
    let mut jpeg_buf = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, 85);
    encoder.encode(
        landscape.as_raw(),
        landscape.width(),
        landscape.height(),
        image::ExtendedColorType::Rgb8,
    )?;

    // EXIF APP1 segment payload:
    // Little-endian TIFF ("II*\0"), IFD0 entry tag 0x0112 (Orientation), type SHORT (3), count 1, value 6.
    let exif_payload = [
        // Exif\0\0
        b'E', b'x', b'i', b'f', 0x00, 0x00, // TIFF header: little-endian
        b'I', b'I', 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, // Offset to IFD0
        // IFD0: 1 entry
        0x01, 0x00, // Entry: tag 0x0112, type 0x0003 (SHORT), count 1, value 6
        0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,
        // Next IFD offset: 0
        0x00, 0x00, 0x00, 0x00,
    ];

    let app1_len = (exif_payload.len() + 2) as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&[0xFF, 0xD8]); // SOI marker
    out.extend_from_slice(&[0xFF, 0xE1]); // APP1 marker
    out.extend_from_slice(&app1_len.to_be_bytes()); // Length field
    out.extend_from_slice(&exif_payload);
    out.extend_from_slice(&jpeg_buf[2..]); // The rest of the JPEG data after SOI

    Ok(out)
}
