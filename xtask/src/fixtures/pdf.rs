//! Generation of PDF fixtures (born-digital, scanned, encrypted, corrupt, etc.).

use ab_glyph::{FontRef, PxScale};
use image::{Rgb, RgbImage};
use imageproc::drawing::draw_text_mut;
use krilla::Document;
use krilla::geom::{Point, Size};
use krilla::metadata::{DateTime, Metadata};
use krilla::page::PageSettings;
use krilla::text::{Font, TextDirection};
use lopdf::{Document as LopdfDoc, Object, Stream, dictionary};

use super::raster::{NOTO_SANS_BYTES, NOTO_SANS_MONO_BYTES};

fn default_metadata(title: &str, id: &str) -> Metadata {
    let mut meta = Metadata::new();
    meta = meta.title(title.to_string());
    meta = meta.document_id(id.to_string());
    meta = meta.creator("paperjam".to_string());
    meta = meta.producer("paperjam xtask".to_string());
    meta = meta.creation_date(DateTime::new(2026).month(1).day(1));
    meta
}

/// Generates `contract_ru_digital.pdf`: born-digital 2-page A4 PDF with Cyrillic text.
pub fn generate_contract_ru() -> anyhow::Result<Vec<u8>> {
    let mut doc = Document::new();
    doc.set_metadata(default_metadata(
        "Договор аренды",
        "paperjam-contract-ru-digital",
    ));

    let font = Font::new(NOTO_SANS_BYTES.to_vec().into(), 0)
        .ok_or_else(|| anyhow::anyhow!("failed to load NotoSans font"))?;

    // Page 1
    {
        let mut page = doc.start_page_with(PageSettings::from_wh(595.28, 841.89).unwrap());
        let mut surface = page.surface();

        surface.draw_text(
            Point::from_xy(50.0, 60.0),
            font.clone(),
            18.0,
            "Договор аренды нежилого помещения",
            false,
            TextDirection::Auto,
        );

        surface.draw_text(
            Point::from_xy(50.0, 85.0),
            font.clone(),
            10.0,
            "№ ДА-2026/04                                                    г. Тествилль, 15 января 2026 г.",
            false,
            TextDirection::Auto,
        );

        let p1_lines = [
            "Общество с ограниченной ответственностью «Пример-Инвест», именуемое в дальнейшем «Арендодатель»,",
            "в лице генерального директора Тестова Ивана Сергеевича, действующего на основании Устава, с одной стороны,",
            "и Общество с ограниченной ответственностью «Тест-Ритейл», именуемое в дальнейшем «Арендатор»,",
            "в лице директора Тестовой Анны Павловны, действующей на основании Устава, с другой стороны,",
            "совместно именуемые «Стороны», заключили настоящий Договор о нижеследующем:",
            "",
            "1. Предмет договора",
            "1.1. Арендодатель обязуется предоставить Арендатору за плату во временное владение и пользование",
            "нежилое помещение общей площадью 120,5 кв. м, расположенное по адресу: Республика Тестландия,",
            "г. Тествилль, ул. Примерная, д. 10, пом. II (кадастровый номер 77:01:0001001:1024).",
            "1.2. Помещение передается для использования исключительно под административно-офисные цели.",
            "1.3. Помещение принадлежит Арендодателю на праве собственности согласно выписке из реестра.",
            "",
            "2. Арендная плата и порядок расчетов",
            "2.1. Ежемесячная арендная плата устанавливается в размере 150 000 (сто пятьдесят тысяч) рублей,",
            "включая все применимые налоги и базовые эксплуатационные расходы.",
            "2.2. Оплата производится ежемесячно не позднее 5-го числа расчетного месяца путем перечисления",
            "денежных средств на расчетный счет Арендодателя, указанный в реквизитах настоящего Договора.",
            "2.3. Коммунальные услуги оплачиваются Арендатором отдельно по показаниям приборов учета.",
        ];

        let mut y = 120.0;
        for line in p1_lines {
            if !line.is_empty() {
                surface.draw_text(
                    Point::from_xy(50.0, y),
                    font.clone(),
                    10.5,
                    line,
                    false,
                    TextDirection::Auto,
                );
            }
            y += 18.0;
        }

        surface.finish();
        page.finish();
    }

    // Page 2
    {
        let mut page = doc.start_page_with(PageSettings::from_wh(595.28, 841.89).unwrap());
        let mut surface = page.surface();

        let p2_lines = [
            "3. Права и обязанности сторон",
            "3.1. Арендодатель обязан предоставить помещение по акту приема-передачи в течение 3 дней.",
            "3.2. Арендатор обязан своевременно вносить арендные платежи и поддерживать порядок.",
            "3.3. Арендатор не вправе производить перепланировку без письменного согласия Арендодателя.",
            "",
            "4. Срок действия договора",
            "4.1. Настоящий договор заключается сроком на 11 (одиннадцать) календарных месяцев.",
            "4.2. Продление договора осуществляется по взаимному согласию сторон путем заключения допсоглашения.",
            "",
            "5. Ответственность и разрешение споров",
            "5.1. За нарушение сроков оплаты Арендатор уплачивает пени в размере 0,1% от суммы задолженности.",
            "5.2. Все споры и разногласия стороны разрешают путем конструктивных переговоров.",
            "",
            "6. Адреса, банковские реквизиты и подписи сторон",
            "",
            "Арендодатель: ООО «Пример-Инвест»",
            "ИНН 7701234567, КПП 770101001, ОГРН 1267700012345",
            "Юридический адрес: 101000, г. Тествилль, ул. Примерная, д. 10, оф. 201",
            "Расчетный счет: 40702810900000012345 в Банке Пример",
            "Генеральный директор: ____________________ / И. С. Тестов /",
            "",
            "Арендатор: ООО «Тест-Ритейл»",
            "ИНН 7707654321, КПП 770701001, ОГРН 1267700098765",
            "Юридический адрес: 101000, г. Тествилль, ул. Торговая, д. 5, оф. 104",
            "Расчетный счет: 40702810400000098765 в Банке Пример",
            "Директор: ____________________ / А. П. Тестова /",
        ];

        let mut y = 60.0;
        for line in p2_lines {
            if !line.is_empty() {
                surface.draw_text(
                    Point::from_xy(50.0, y),
                    font.clone(),
                    10.5,
                    line,
                    false,
                    TextDirection::Auto,
                );
            }
            y += 20.0;
        }

        surface.finish();
        page.finish();
    }

    Ok(doc.finish()?)
}

/// Generates `contract_ro_digital.pdf`: born-digital 2-page A4 PDF with Romanian diacritics.
pub fn generate_contract_ro() -> anyhow::Result<Vec<u8>> {
    let mut doc = Document::new();
    doc.set_metadata(default_metadata(
        "Contractul de închiriere",
        "paperjam-contract-ro-digital",
    ));

    let font = Font::new(NOTO_SANS_BYTES.to_vec().into(), 0)
        .ok_or_else(|| anyhow::anyhow!("failed to load NotoSans font"))?;

    // Page 1
    {
        let mut page = doc.start_page_with(PageSettings::from_wh(595.28, 841.89).unwrap());
        let mut surface = page.surface();

        surface.draw_text(
            Point::from_xy(50.0, 60.0),
            font.clone(),
            18.0,
            "Contractul de închiriere spațiu comercial",
            false,
            TextDirection::Auto,
        );

        surface.draw_text(
            Point::from_xy(50.0, 85.0),
            font.clone(),
            10.0,
            "Nr. 2026/04                                                     mun. Chișinău, 15 ianuarie 2026",
            false,
            TextDirection::Auto,
        );

        let p1_lines = [
            "Societatea Comercială «Exemplu Imobil» SRL, denumită în continuare «Locator»,",
            "reprezentată prin administratorul Ivan Testov, acționând în baza Statutului, pe de o parte,",
            "și Compania «Servicii Moderne» SRL, denumită în continuare «Chiriaș»,",
            "reprezentată prin administratorul Anna Testova, acționând în baza Statutului, pe de altă parte,",
            "au convenit încheierea prezentului contract de locațiune în următoarele condiții:",
            "",
            "1. Obiectul contractului",
            "1.1. Locatorul acordă, iar Chiriașul primește în folosință temporară spațiul comercial",
            "situat la adresa: mun. Chișinău, str. Florilor, nr. 12, având o suprafață de 150 mp.",
            "1.2. Spațiul este destinat exclusiv activităților de birou, instruire și consultanță comercială.",
            "1.3. Locatorul declară pe propria răspundere că imobilul este liber de sarcini și ipoteci.",
            "",
            "2. Chiria lunară și modalitatea de plată",
            "2.1. Chiria lunară constituie suma fixă de 1 200 EUR (una mie două sute euro), plătibilă în lei moldovenești.",
            "2.2. Plata se realizează lunar până la data de 10 a fiecărei luni prin transfer pe contul bancar.",
            "2.3. Cheltuielile pentru utilități (energie electrică, apă, încălzire) se achită separat conform facturilor.",
        ];

        let mut y = 120.0;
        for line in p1_lines {
            if !line.is_empty() {
                surface.draw_text(
                    Point::from_xy(50.0, y),
                    font.clone(),
                    10.5,
                    line,
                    false,
                    TextDirection::Auto,
                );
            }
            y += 18.0;
        }

        surface.finish();
        page.finish();
    }

    // Page 2
    {
        let mut page = doc.start_page_with(PageSettings::from_wh(595.28, 841.89).unwrap());
        let mut surface = page.surface();

        let p2_lines = [
            "3. Termenul și condițiile locațiunii",
            "3.1. Prezentul contract intră în vigoare la data semnării și este valabil pentru o durată de 36 de luni.",
            "3.2. Orice modificare sau completare la prezentul contract se va efectua printr-un act adițional semnat.",
            "",
            "4. Drepturile și obligațiile chiriașului",
            "4.1. Chiriașul se obligă să asigure paza și securitatea antiincendiară a spațiului închiriat.",
            "4.2. Chiriașul garantează menținerea în stare intactă a echipamentelor și finisajelor interioare.",
            "",
            "5. Dispoziții finale și soluționarea litigiilor",
            "5.1. Eventualele litigii apărute pe durata executării se soluționează pe cale amiabilă sau în instanță.",
            "5.2. Prezentul contract a fost redactat în două exemplare originale, câte unul pentru fiecare parte.",
            "",
            "6. Datele de identificare și semnăturile părților",
            "",
            "Locator: SC «Exemplu Imobil» SRL",
            "Cod fiscal (IDNO): 1002600123456, TVA: 0501234",
            "Sediul: mun. Chișinău, str. Florilor, nr. 12",
            "Cont bancar: MD24EX0000001234567890 deschis la Banca Exemplu Chișinău",
            "Administrator: ____________________ / Ivan Testov /",
            "",
            "Chiriaș: SC «Servicii Moderne» SRL",
            "Cod fiscal (IDNO): 1002600654321, TVA: 0507654",
            "Sediul: mun. Chișinău, bd. Ștefan cel Mare, nr. 100",
            "Cont bancar: MD24EX0000008765432109 deschis la Banca Exemplu Chișinău",
            "Administrator: ____________________ / Anna Testova /",
        ];

        let mut y = 60.0;
        for line in p2_lines {
            if !line.is_empty() {
                surface.draw_text(
                    Point::from_xy(50.0, y),
                    font.clone(),
                    10.5,
                    line,
                    false,
                    TextDirection::Auto,
                );
            }
            y += 20.0;
        }

        surface.finish();
        page.finish();
    }

    Ok(doc.finish()?)
}

/// Generates the raster image of an English invoice for `invoice_scan_en.pdf`.
fn generate_invoice_scan_image() -> anyhow::Result<Vec<u8>> {
    let font_sans = FontRef::try_from_slice(NOTO_SANS_BYTES)?;
    let font_mono = FontRef::try_from_slice(NOTO_SANS_MONO_BYTES)?;

    let mut img = RgbImage::from_pixel(1240, 1754, Rgb([255, 255, 255]));
    let black = Rgb([0, 0, 0]);
    let grey = Rgb([90, 90, 90]);

    // Header
    draw_text_mut(
        &mut img,
        black,
        100,
        120,
        PxScale::from(48.0),
        &font_sans,
        "INVOICE",
    );
    draw_text_mut(
        &mut img,
        grey,
        100,
        185,
        PxScale::from(24.0),
        &font_mono,
        "INVOICE No. INV-2026-0042",
    );
    draw_text_mut(
        &mut img,
        grey,
        100,
        220,
        PxScale::from(22.0),
        &font_mono,
        "Date: 2026-10-20",
    );

    // Vendor & Customer
    draw_text_mut(
        &mut img,
        black,
        100,
        290,
        PxScale::from(26.0),
        &font_sans,
        "Vendor:",
    );
    draw_text_mut(
        &mut img,
        grey,
        100,
        325,
        PxScale::from(24.0),
        &font_sans,
        "Example Supplies Ltd",
    );
    draw_text_mut(
        &mut img,
        grey,
        100,
        355,
        PxScale::from(20.0),
        &font_sans,
        "123 Business Parkway, Suite 400, Industrial District",
    );

    draw_text_mut(
        &mut img,
        black,
        700,
        290,
        PxScale::from(26.0),
        &font_sans,
        "Billed To:",
    );
    draw_text_mut(
        &mut img,
        grey,
        700,
        325,
        PxScale::from(24.0),
        &font_sans,
        "Acme Testing Corp",
    );
    draw_text_mut(
        &mut img,
        grey,
        700,
        355,
        PxScale::from(20.0),
        &font_sans,
        "45 Innovation Boulevard, Tech Park",
    );

    // Items table
    let table_lines = [
        "--------------------------------------------------------------------------------",
        "Item Description                                 Qty     Unit Price       Amount",
        "--------------------------------------------------------------------------------",
        "1. Premium Office Desk (Oak finish)                2     250.00 EUR   500.00 EUR",
        "2. Ergonomic Mesh Chair Pro Edition                2     300.00 EUR   600.00 EUR",
        "3. Express Freight & Assembly Handling             1     150.00 EUR   150.00 EUR",
        "--------------------------------------------------------------------------------",
        "                                                    SUBTOTAL:       1,100.00 EUR",
        "                                                    VAT (15%):        150.00 EUR",
        "                                                    TOTAL:          1,250.00 EUR",
        "--------------------------------------------------------------------------------",
    ];

    let mut y = 460;
    for line in table_lines {
        draw_text_mut(
            &mut img,
            black,
            100,
            y,
            PxScale::from(22.0),
            &font_mono,
            line,
        );
        y += 40;
    }

    draw_text_mut(
        &mut img,
        grey,
        100,
        1200,
        PxScale::from(20.0),
        &font_sans,
        "Payment Terms: Net 30 days. Thank you for your business!",
    );

    let mut jpeg_buf = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, 85);
    encoder.encode(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(jpeg_buf)
}

/// Generates `invoice_scan_en.pdf`: one A4 page, only a raster image, NO fonts, NO text.
pub fn generate_invoice_scan() -> anyhow::Result<Vec<u8>> {
    let jpeg_bytes = generate_invoice_scan_image()?;

    let mut doc = LopdfDoc::with_version("1.5");
    let pages_id = doc.new_object_id();

    let image_stream = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1240,
            "Height" => 1754,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        jpeg_bytes,
    );
    let image_id = doc.add_object(image_stream);

    let content = b"q\n595.28 0 0 841.89 0 0 cm\n/Im0 Do\nQ\n";
    let contents_id = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.28.into(), 841.89.into()],
        "Resources" => dictionary! {
            "XObject" => dictionary! { "Im0" => image_id },
        },
        "Contents" => contents_id,
    });

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut buf = Vec::new();
    doc.save_to(&mut buf)?;
    Ok(buf)
}

/// Generates a raster page image for statement pages 7-12.
fn generate_statement_raster_page(page_num: usize) -> anyhow::Result<Vec<u8>> {
    let font_sans = FontRef::try_from_slice(NOTO_SANS_BYTES)?;
    let font_mono = FontRef::try_from_slice(NOTO_SANS_MONO_BYTES)?;

    let mut img = RgbImage::from_pixel(1240, 1754, Rgb([255, 255, 255]));
    let black = Rgb([0, 0, 0]);
    let grey = Rgb([80, 80, 80]);

    draw_text_mut(
        &mut img,
        black,
        100,
        100,
        PxScale::from(36.0),
        &font_sans,
        "Example Bank / Банк Пример",
    );
    draw_text_mut(
        &mut img,
        grey,
        100,
        150,
        PxScale::from(24.0),
        &font_sans,
        &format!("Statement Archive Appendix - Page {page_num} of 12 (Scanned Record)"),
    );

    let lines = [
        "--------------------------------------------------------------------------------",
        "TX ID    | Date       | Transaction Details / Описание             | Amount EUR ",
        "--------------------------------------------------------------------------------",
        "TX900142 | 2026-01-20 | Покупка оборудования / Tech hardware store |    -640.00 ",
        "TX900143 | 2026-01-21 | Подписка на ПО / Software cloud licensing  |    -120.00 ",
        "TX900144 | 2026-01-22 | Возврат средств / Merchant charge refund   |     +45.00 ",
        "TX900145 | 2026-01-23 | Оплата хостинга / Dedicated server hosting |    -250.00 ",
        "TX900146 | 2026-01-24 | Бухгалтерский аудит / Accounting services  |    -800.00 ",
        "TX900147 | 2026-01-25 | Консультация юриста / Legal advisory       |    -350.00 ",
        "TX900148 | 2026-01-26 | Оплата связи и интернета / Telecom billing |     -85.00 ",
        "TX900149 | 2026-01-27 | Доставка корреспонденции / Courier express |     -30.00 ",
        "TX900150 | 2026-01-28 | Проценты по депозиту / Deposit interest    |    +115.40 ",
        "--------------------------------------------------------------------------------",
    ];

    let mut y = 230;
    for line in lines {
        draw_text_mut(
            &mut img,
            black,
            100,
            y,
            PxScale::from(22.0),
            &font_mono,
            line,
        );
        y += 45;
    }

    let mut jpeg_buf = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, 80);
    encoder.encode(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(jpeg_buf)
}

/// Generates `statement_multi_page.pdf`: 12 pages.
/// Pages 1-6: born-digital text (embedded font, Cyrillic + Latin).
/// Pages 7-12: image-only pages (no text layer).
pub fn generate_statement_multi_page() -> anyhow::Result<Vec<u8>> {
    let mut doc = Document::new();
    doc.set_metadata(default_metadata(
        "Account Statement",
        "paperjam-statement-multi-page",
    ));

    let font = Font::new(NOTO_SANS_BYTES.to_vec().into(), 0)
        .ok_or_else(|| anyhow::anyhow!("failed to load font"))?;

    // Pages 1 to 6: born-digital text
    for p in 1..=6 {
        let mut page = doc.start_page_with(PageSettings::from_wh(595.28, 841.89).unwrap());
        let mut surface = page.surface();

        surface.draw_text(
            Point::from_xy(50.0, 50.0),
            font.clone(),
            16.0,
            "Example Bank / Банк Пример — Выписка по банковскому счету",
            false,
            TextDirection::Auto,
        );

        surface.draw_text(
            Point::from_xy(50.0, 75.0),
            font.clone(),
            10.0,
            &format!("Клиент: Иван Тестов / Ivan Testov | Счет IBAN: UT56EXBK12345678901234 | Лист {p} из 12"),
            false,
            TextDirection::Auto,
        );

        let table_header = [
            "Дата       | Назначение платежа / Operation description          | Сумма (EUR) | Остаток",
            "---------------------------------------------------------------------------------------",
        ];
        let mut y = 110.0;
        for line in table_header {
            surface.draw_text(
                Point::from_xy(50.0, y),
                font.clone(),
                9.5,
                line,
                false,
                TextDirection::Auto,
            );
            y += 16.0;
        }

        for row in 1..=24 {
            let row_text = format!(
                "2026-01-{:02} | Расчетная операция #{p}{row:02} / Service settlement | -{:3}.00 EUR | 1{:02},{:03}.00 EUR",
                row % 28 + 1,
                (row * 17) % 500 + 20,
                row,
                (row * 43) % 1000
            );
            surface.draw_text(
                Point::from_xy(50.0, y),
                font.clone(),
                9.0,
                &row_text,
                false,
                TextDirection::Auto,
            );
            y += 18.0;
        }

        surface.finish();
        page.finish();
    }

    // Pages 7 to 12: image-only pages
    for p in 7..=12 {
        let jpeg_bytes = generate_statement_raster_page(p)?;
        let krilla_img = krilla::image::Image::from_jpeg(jpeg_bytes.into(), false)
            .map_err(|e| anyhow::anyhow!("krilla image error: {e}"))?;

        let mut page = doc.start_page_with(PageSettings::from_wh(595.28, 841.89).unwrap());
        let mut surface = page.surface();
        surface.draw_image(krilla_img, Size::from_wh(595.28, 841.89).unwrap());
        surface.finish();
        page.finish();
    }

    Ok(doc.finish()?)
}

/// Generates `garbage_text_layer.pdf`:
/// One page: raster image of clear English text, plus invisible text layer (3 Tr) with garbage extraction.
pub fn generate_garbage_text_layer() -> anyhow::Result<Vec<u8>> {
    // Hex string for garbage bytes: ÿþ§¶%%¤ÐÑ¢£ repeated
    // In WinAnsiEncoding:
    // 0xFF: ÿ, 0xFE: þ, 0xA7: §, 0xB6: ¶, 0x25: %, 0x25: %, 0xA4: ¤, 0xD0: Ð, 0xD1: Ñ, 0xA2: ¢, 0xA3: £
    let garbage_hex = "FFFEA7B62525A4D0D1A2A3".repeat(6);
    // Invisible text mode: 3 Tr
    report_scan_with_text(&format!(
        "BT\n3 Tr\n/F1 12 Tf\n72 750 Td\n<{garbage_hex}> Tj\nET\n"
    ))
}

/// Generates `stamped_scan.pdf`: the same scanned report, whose only text is
/// the stamp a phone scanner app prints in the corner. The stamp reads as
/// perfectly good text, yet it says nothing of the page.
pub fn generate_stamped_scan() -> anyhow::Result<Vec<u8>> {
    report_scan_with_text("BT\n/F1 9 Tf\n440 20 Td\n(Scanned with PocketScan) Tj\nET\n")
}

/// One A4 page: a raster image of a short English report, then `text_ops`
/// (content stream operators that may use the font `/F1`).
fn report_scan_with_text(text_ops: &str) -> anyhow::Result<Vec<u8>> {
    // 1. Create raster image with clear English text
    let font_sans = FontRef::try_from_slice(NOTO_SANS_BYTES)?;
    let mut img = RgbImage::from_pixel(1240, 1754, Rgb([255, 255, 255]));
    let black = Rgb([0, 0, 0]);
    let grey = Rgb([70, 70, 70]);

    draw_text_mut(
        &mut img,
        black,
        100,
        120,
        PxScale::from(40.0),
        &font_sans,
        "Quarterly Business Review - Q3 2026",
    );

    let report_lines = [
        "Executive Summary:",
        "During the third quarter of 2026, total operating revenue reached 4,250,000 EUR,",
        "representing an 18% increase year over year across all commercial departments.",
        "Operating expenses remained stable and strictly within the allocated budget envelope.",
        "Customer satisfaction ratings improved to 96.4% following infrastructure modernization.",
        "The organization maintains a strong balance sheet with positive cash flows from operations.",
        "Key performance indicators remain on track to achieve annual corporate targets.",
        "Management recommends continuing the planned expansion into neighboring territories.",
    ];

    let mut y = 200;
    for line in report_lines {
        draw_text_mut(
            &mut img,
            grey,
            100,
            y,
            PxScale::from(24.0),
            &font_sans,
            line,
        );
        y += 45;
    }

    let mut jpeg_bytes = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_bytes, 85);
    encoder.encode(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )?;

    // 2. Build PDF using lopdf
    let mut doc = LopdfDoc::with_version("1.5");
    let pages_id = doc.new_object_id();

    let image_stream = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1240,
            "Height" => 1754,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        jpeg_bytes,
    );
    let image_id = doc.add_object(image_stream);

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });

    let content = format!("q\n595.28 0 0 841.89 0 0 cm\n/Im0 Do\nQ\n{text_ops}");
    let contents_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.28.into(), 841.89.into()],
        "Resources" => dictionary! {
            "XObject" => dictionary! { "Im0" => image_id },
            "Font" => dictionary! { "F1" => font_id },
        },
        "Contents" => contents_id,
    });

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut buf = Vec::new();
    doc.save_to(&mut buf)?;
    Ok(buf)
}

/// Generates `encrypted.pdf`: valid 1-page PDF encrypted with standard handler,
/// user password "secret".
pub fn generate_encrypted() -> anyhow::Result<Vec<u8>> {
    let mut doc = LopdfDoc::with_version("1.5");
    doc.trailer.set(
        "ID",
        Object::Array(vec![
            Object::string_literal(b"0123456789abcdef"),
            Object::string_literal(b"fedcba9876543210"),
        ]),
    );

    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let content = b"BT\n/F1 14 Tf\n100 700 Td\n(Confidential synthetic document - password required) Tj\nET\n";
    let contents_id = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.28.into(), 841.89.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
        "Contents" => contents_id,
    });

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let state = lopdf::EncryptionState::try_from(lopdf::EncryptionVersion::V2 {
        document: &doc,
        owner_password: "secret",
        user_password: "secret",
        key_length: 128,
        permissions: lopdf::Permissions::all(),
    })
    .map_err(|e| anyhow::anyhow!("encryption setup failed: {e:?}"))?;

    doc.encrypt(&state)?;

    // Fix the 16 bytes of arbitrary padding in the /U entry of the /Encrypt dictionary
    // to ensure deterministic output across runs.
    if let Ok(Object::Reference(id)) = doc.trailer.get(b"Encrypt")
        && let Some(Object::Dictionary(dict)) = doc.objects.get_mut(id)
        && let Ok(Object::String(u_bytes, _)) = dict.get_mut(b"U")
        && let Some(tail) = u_bytes.get_mut(16..)
    {
        tail.fill(0);
    }

    let mut buf = Vec::new();
    doc.save_to(&mut buf)?;
    Ok(buf)
}

/// Generates `corrupt.pdf`: valid header and couple of objects, cut off mid-object, no startxref/EOF.
pub fn generate_corrupt() -> Vec<u8> {
    let corrupt_bytes: &[u8] = b"%PDF-1.4\n\
1 0 obj\n\
<< /Type /Catalog /Pages 2 0 R >>\n\
endobj\n\
2 0 obj\n\
<< /Type /Pages /Kids [3 0 R] /Count 1 >>\n\
endobj\n\
3 0 obj\n\
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R >>\n\
endobj\n\
4 0 obj\n\
<< /Length 300 >>\n\
stream\n\
BT\n/F1 12 Tf\n72 700 Td\n(This stream was truncated abruptly \
\xDE\xAD\xBE\xEF\xCA\xFE\xBA\xBE\x00\xFF\xAA\x55\x12\x34\x56\x78\x9A\xBC\xDE\xF0\
\xDE\xAD\xBE\xEF\xCA\xFE\xBA\xBE\x00\xFF\xAA\x55\x12\x34\x56\x78\x9A\xBC\xDE\xF0\
\xDE\xAD\xBE\xEF\xCA\xFE\xBA\xBE\x00\xFF\xAA\x55\x12\x34\x56\x78\x9A\xBC\xDE\xF0";
    corrupt_bytes.to_vec()
}
