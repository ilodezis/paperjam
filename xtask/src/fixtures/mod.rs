//! Synthetic fixture corpus. Everything here is invented: names, numbers and
//! organizations belong to no real person. Output is deterministic.

pub mod mrz;
pub mod pdf;
pub mod raster;

use std::fs;
use std::path::Path;

pub use mrz::{id_card_mrz, passport_mrz};

/// Every file `generate` writes, relative to the output directory.
pub const FILES: &[&str] = &[
    "scan_ru.png",
    "passport_td3.png",
    "passport_td3_badcheck.png",
    "passport_td3_sideways_a4.png",
    "id_card_td1.png",
    "id_card_td1_sideways_a4.png",
    "contract_ru_digital.pdf",
    "contract_ro_digital.pdf",
    "receipt_photo_skewed.jpg",
    "invoice_scan_en.pdf",
    "statement_multi_page.pdf",
    "garbage_text_layer.pdf",
    "stamped_scan.pdf",
    "encrypted.pdf",
    "corrupt.pdf",
    "photo.heic",
];

const STATIC_PHOTO_HEIC: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../fixtures/static/photo.heic"
));

/// Writes the whole corpus into `out_dir` (created if missing).
pub fn generate(out_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(out_dir)?;

    fs::write(out_dir.join("scan_ru.png"), raster::generate_scan_ru()?)?;
    fs::write(
        out_dir.join("passport_td3.png"),
        raster::generate_passport_td3(false)?,
    )?;
    fs::write(
        out_dir.join("passport_td3_badcheck.png"),
        raster::generate_passport_td3(true)?,
    )?;
    fs::write(
        out_dir.join("passport_td3_sideways_a4.png"),
        raster::generate_passport_td3_sideways_a4()?,
    )?;
    fs::write(
        out_dir.join("id_card_td1.png"),
        raster::generate_id_card_td1()?,
    )?;
    fs::write(
        out_dir.join("id_card_td1_sideways_a4.png"),
        raster::generate_id_card_td1_sideways_a4()?,
    )?;

    fs::write(
        out_dir.join("contract_ru_digital.pdf"),
        pdf::generate_contract_ru()?,
    )?;
    fs::write(
        out_dir.join("contract_ro_digital.pdf"),
        pdf::generate_contract_ro()?,
    )?;
    fs::write(
        out_dir.join("receipt_photo_skewed.jpg"),
        raster::generate_receipt_photo_skewed()?,
    )?;
    fs::write(
        out_dir.join("invoice_scan_en.pdf"),
        pdf::generate_invoice_scan()?,
    )?;
    fs::write(
        out_dir.join("statement_multi_page.pdf"),
        pdf::generate_statement_multi_page()?,
    )?;
    fs::write(
        out_dir.join("garbage_text_layer.pdf"),
        pdf::generate_garbage_text_layer()?,
    )?;
    fs::write(
        out_dir.join("stamped_scan.pdf"),
        pdf::generate_stamped_scan()?,
    )?;
    fs::write(out_dir.join("encrypted.pdf"), pdf::generate_encrypted()?)?;
    fs::write(out_dir.join("corrupt.pdf"), pdf::generate_corrupt())?;
    fs::write(out_dir.join("photo.heic"), STATIC_PHOTO_HEIC)?;

    Ok(())
}
