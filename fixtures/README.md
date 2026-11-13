# Synthetic Test Fixtures

This directory contains synthetic fixtures used for testing document ingestion, OCR, MRZ parsing, metadata extraction, text extraction, and error handling in `paperjam`.

## Regeneration

To regenerate all fixtures in `fixtures/gen/`:

```bash
cargo xtask fixtures
```

Generation is completely deterministic: repeated runs produce byte-identical files.

## Synthetic Data Notice

All fixtures in this corpus are strictly synthetic:
- All individuals (e.g., "Ivan Testov", "Anna Testova") and entities ("Magazin Test SRL", "Example Supplies Ltd", "Example Bank") are fictional.
- Machine Readable Zones (MRZ) use ICAO Doc 9303 fictional country code `UTO` ("Utopia").
- Document identifiers, dates, and amounts are simulated for testing purposes only.

## Included Files

- `scan_ru.png`: 150 DPI A4 portrait scan containing Russian Cyrillic document text (`Паспорт гражданина ...`).
- `passport_td3.png`: 1500×1050 passport data page with photo box and valid 2-line TD3 MRZ.
- `passport_td3_badcheck.png`: Same as `passport_td3.png`, but with an invalid document-number check digit.
- `passport_td3_sideways_a4.png`: the passport page of `passport_td3.png` (900×630) lying in the corner of a 150 DPI A4 sheet, scanned a quarter turn clockwise (1754×1240). Tesseract's orientation detection is unsure about it, so the pipeline must find the MRZ by trying the other turns.
- `id_card_td1.png`: 1500×950 ID card with 3-line TD1 MRZ.
- `id_card_td1_sideways_a4.png`: `id_card_td1.png` at card size (1011×640) in the middle of a 300 DPI A4 sheet with specks of dust, scanned a quarter turn counter-clockwise (3508×2480). Orientation detection fails on it; the pipeline must cut the card out, turn it by how it reads and find the MRZ.
- `contract_ru_digital.pdf`: 2-page born-digital A4 contract in Russian with embedded TrueType font and ToUnicode mapping.
- `contract_ro_digital.pdf`: 2-page born-digital A4 contract in Romanian with diacritics (`ă`, `â`, `î`, `ș`, `ț`).
- `receipt_photo_skewed.jpg`: Skewed portrait receipt rotated 90° CCW into landscape with an EXIF Orientation 6 (0x0112) tag.
- `invoice_scan_en.pdf`: 1-page raster image-only PDF invoice (contains `/Image`, no `/Font`).
- `statement_multi_page.pdf`: 12-page PDF bank statement (pages 1–6 born-digital text, pages 7–12 rasterized scanned pages).
- `garbage_text_layer.pdf`: 1-page PDF with readable image underlay and invisible garbage text layer.
- `encrypted.pdf`: Standard RC4-128 encrypted PDF requiring password `secret`.
- `corrupt.pdf`: Malformed PDF truncated mid-stream without cross-reference table or `%%EOF`.
- `photo.heic`: Sample synthetic HEIC image asset.

## Font Attribution

The font files in `fixtures/fonts/`:
- `NotoSans-Regular.ttf`
- `NotoSansMono-Regular.ttf`

are part of the Google Noto font family, licensed under the [SIL Open Font License, Version 1.1](http://scripts.sil.org/OFL).
