//! Automatic checks that decide whether a document "needs attention"
//! (spec §7.7).
//!
//! Two kinds of flags exist. *Derived* flags are recomputed from the stored
//! values every time: `checksum_failed` and `ocr_llm_disagree`, so editing the
//! affected field clears them. *Carried* flags record something that happened
//! during processing and stay until the next reprocess: `llm_failed`,
//! `llm_budget`, `processing_failed`, `mrz_mismatch`.

use std::collections::BTreeSet;

use sqlx::SqliteConnection;

use crate::error::Result;
use crate::ids::DocumentId;
use crate::model::document::Provenance;
use crate::model::fields::{FieldDef, FieldValue};
use crate::service::App;
use crate::validators::{self, Validation};

pub const MRZ_MISMATCH: &str = "mrz_mismatch";
pub const CHECKSUM_FAILED: &str = "checksum_failed";
pub const OCR_LLM_DISAGREE: &str = "ocr_llm_disagree";
pub const LLM_FAILED: &str = "llm_failed";
pub const LLM_BUDGET: &str = "llm_budget";
pub const PROCESSING_FAILED: &str = "processing_failed";
/// Key of the fact recording that a scanned MRZ failed its check digits.
pub const MRZ_CHECK_FACT: &str = "mrz_check_digits";

/// Canonical order of flags in the stored list.
const ORDER: [&str; 6] = [
    MRZ_MISMATCH,
    CHECKSUM_FAILED,
    OCR_LLM_DISAGREE,
    LLM_FAILED,
    LLM_BUDGET,
    PROCESSING_FAILED,
];
const CARRIED: [&str; 4] = [MRZ_MISMATCH, LLM_FAILED, LLM_BUDGET, PROCESSING_FAILED];

/// The flags the document should carry now: the carried ones it already has,
/// `extra` flags found by the caller, and the derived ones recomputed.
pub async fn run(app: &App, id: DocumentId) -> Result<Vec<String>> {
    let mut conn = app.db.read.acquire().await?;
    run_in(&mut conn, id, &[]).await
}

pub async fn run_in(
    conn: &mut SqliteConnection,
    id: DocumentId,
    extra: &[&str],
) -> Result<Vec<String>> {
    let stored: Option<String> = sqlx::query_scalar("SELECT attention FROM documents WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    let stored: Vec<String> = stored
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut flags: BTreeSet<&str> = ORDER
        .iter()
        .copied()
        .filter(|f| CARRIED.contains(f) && stored.iter().any(|s| s == f))
        .collect();
    flags.extend(extra.iter().copied());
    if !mrz_mismatch_fields(conn, id).await?.is_empty() {
        flags.insert(MRZ_MISMATCH);
    }
    if has_bad_mrz(conn, id).await? || !failed_checksum_fields(conn, id).await?.is_empty() {
        flags.insert(CHECKSUM_FAILED);
    }
    if !fields_disagreeing_with_ocr(conn, id).await?.is_empty() {
        flags.insert(OCR_LLM_DISAGREE);
    }
    Ok(ORDER
        .iter()
        .filter(|f| flags.contains(*f))
        .map(|f| (*f).to_string())
        .collect())
}

/// Recomputes the flags and stores them.
pub async fn recompute_in(
    conn: &mut SqliteConnection,
    id: DocumentId,
    extra: &[&str],
) -> Result<()> {
    let flags = run_in(conn, id, extra).await?;
    store_in(conn, id, &flags).await
}

pub async fn store_in(conn: &mut SqliteConnection, id: DocumentId, flags: &[String]) -> Result<()> {
    sqlx::query("UPDATE documents SET attention = ? WHERE id = ?")
        .bind(serde_json::json!(flags).to_string())
        .bind(id)
        .execute(conn)
        .await?;
    Ok(())
}

/// Adds one carried flag to the stored list (used by pipeline steps that fail
/// softly, such as the LLM steps).
pub async fn add_flag(conn: &mut SqliteConnection, id: DocumentId, flag: &str) -> Result<()> {
    let stored: Option<String> = sqlx::query_scalar("SELECT attention FROM documents WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    let mut flags: Vec<String> = stored
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if !flags.iter().any(|f| f == flag) {
        flags.push(flag.to_string());
        let ordered: Vec<String> = ORDER
            .iter()
            .filter(|f| flags.iter().any(|s| s == *f))
            .map(|f| (*f).to_string())
            .collect();
        store_in(conn, id, &ordered).await?;
    }
    Ok(())
}

/// Removes carried flags from the stored list: a run that redoes the work a
/// flag records starts without it.
pub async fn remove_flags(
    conn: &mut SqliteConnection,
    id: DocumentId,
    remove: &[&str],
) -> Result<()> {
    let stored: Option<String> = sqlx::query_scalar("SELECT attention FROM documents WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    let flags: Vec<String> = stored
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let kept: Vec<String> = flags
        .iter()
        .filter(|f| !remove.contains(&f.as_str()))
        .cloned()
        .collect();
    if kept.len() != flags.len() {
        store_in(conn, id, &kept).await?;
    }
    Ok(())
}

/// Whether `flag` is one of the known attention flag codes.
pub fn is_known_flag(flag: &str) -> bool {
    ORDER.contains(&flag)
}

/// Clears the per-field MRZ mismatch markers on a document.
pub async fn clear_mrz_mismatch_fields(conn: &mut SqliteConnection, id: DocumentId) -> Result<()> {
    sqlx::query(
        "UPDATE field_values SET mrz_mismatch = 0 WHERE document_id = ? AND mrz_mismatch != 0",
    )
    .bind(id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Dismisses the requested attention flags that a user confirmed, returning the
/// flags that were actually cleared.
pub async fn dismiss_in(
    conn: &mut SqliteConnection,
    id: DocumentId,
    flags: &[String],
) -> Result<Vec<String>> {
    let stored: Option<String> = sqlx::query_scalar("SELECT attention FROM documents WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    let before: Vec<String> = stored
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut remove: Vec<&str> = Vec::new();
    for flag in flags {
        match flag.as_str() {
            MRZ_MISMATCH => {
                clear_mrz_mismatch_fields(conn, id).await?;
                remove.push(MRZ_MISMATCH);
            }
            LLM_FAILED => remove.push(LLM_FAILED),
            LLM_BUDGET => remove.push(LLM_BUDGET),
            PROCESSING_FAILED => remove.push(PROCESSING_FAILED),
            CHECKSUM_FAILED if failed_checksum_fields(conn, id).await?.is_empty() => {
                sqlx::query(
                    "DELETE FROM facts WHERE document_id = ? AND key = ? AND provenance = 'mrz'",
                )
                .bind(id)
                .bind(MRZ_CHECK_FACT)
                .execute(&mut *conn)
                .await?;
            }
            _ => {}
        }
    }
    if !remove.is_empty() {
        remove_flags(conn, id, &remove).await?;
    }
    recompute_in(conn, id, &[]).await?;
    let after = run_in(conn, id, &[]).await?;
    Ok(before
        .into_iter()
        .filter(|f| flags.iter().any(|wanted| wanted == f) && !after.iter().any(|a| a == f))
        .collect())
}

/// Fields whose value disagreed with the passport's machine-readable zone and
/// have not yet been confirmed by hand (`manual`).
pub async fn mrz_mismatch_fields(
    conn: &mut SqliteConnection,
    id: DocumentId,
) -> Result<Vec<String>> {
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT field_key FROM field_values \
         WHERE document_id = ? AND mrz_mismatch = 1 AND provenance != 'manual' \
         ORDER BY field_key",
    )
    .bind(id)
    .fetch_all(conn)
    .await?;
    Ok(keys)
}

async fn field_defs(conn: &mut SqliteConnection, id: DocumentId) -> Result<Vec<FieldDef>> {
    let fields: Option<String> = sqlx::query_scalar(
        "SELECT t.fields FROM documents d JOIN document_types t ON t.id = d.type_id WHERE d.id = ?",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(fields
        .and_then(|f| serde_json::from_str(&f).ok())
        .unwrap_or_default())
}

/// The fields whose current value raises an issue, as `(field key, flag)`, so
/// the reader can see which value to check. A field may appear under more than
/// one flag.
pub async fn field_issues(
    conn: &mut SqliteConnection,
    id: DocumentId,
) -> Result<Vec<(String, &'static str)>> {
    let mut issues: Vec<(String, &'static str)> = mrz_mismatch_fields(conn, id)
        .await?
        .into_iter()
        .map(|key| (key, MRZ_MISMATCH))
        .collect();
    issues.extend(
        failed_checksum_fields(conn, id)
            .await?
            .into_iter()
            .map(|key| (key, CHECKSUM_FAILED)),
    );
    issues.extend(
        fields_disagreeing_with_ocr(conn, id)
            .await?
            .into_iter()
            .map(|key| (key, OCR_LLM_DISAGREE)),
    );
    Ok(issues)
}

/// A scanned MRZ whose check digits fail fills no field, so the verdict is
/// kept as an `mrz`-provenance fact ([`MRZ_CHECK_FACT`]) that every later run
/// can see.
async fn has_bad_mrz(conn: &mut SqliteConnection, id: DocumentId) -> Result<bool> {
    let bad_mrz: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM facts WHERE document_id = ? AND key = ? AND provenance = 'mrz' LIMIT 1",
    )
    .bind(id)
    .bind(MRZ_CHECK_FACT)
    .fetch_optional(conn)
    .await?;
    Ok(bad_mrz.is_some())
}

/// Identifiers with a known checksum that fail it. Values the user typed
/// (`manual`) are trusted: the user decides what the document says.
async fn failed_checksum_fields(
    conn: &mut SqliteConnection,
    id: DocumentId,
) -> Result<Vec<String>> {
    let defs = field_defs(conn, id).await?;
    let values: Vec<(String, String)> = sqlx::query_as(
        "SELECT field_key, value FROM field_values WHERE document_id = ? AND provenance != 'manual'          ORDER BY field_key",
    )
    .bind(id)
    .fetch_all(conn)
    .await?;
    let mut failed = Vec::new();
    for (key, value) in values {
        let Some(validator) = defs
            .iter()
            .find(|d| d.key == key)
            .and_then(|d| d.validator.as_deref())
        else {
            continue;
        };
        let text = match serde_json::from_str::<FieldValue>(&value) {
            Ok(FieldValue::Identifier(s) | FieldValue::Text(s)) => s,
            _ => continue,
        };
        if validators::validate(validator, &text) == Validation::Invalid {
            failed.push(key);
        }
    }
    Ok(failed)
}

/// Digits of money and identifier values the LLM extracted must appear
/// (ignoring spaces and separators) in the Tesseract text of the page they
/// came from. Pages without a Tesseract layer cannot be checked.
///
/// A value disagrees only when Tesseract printed a near miss of it: a number
/// a few digits off, so the two readings of the same print differ. A value
/// Tesseract printed nothing like was not read by it at all (a decorative
/// certificate, a poor scan whose OCR is noise) and cannot be checked either.
async fn fields_disagreeing_with_ocr(
    conn: &mut SqliteConnection,
    id: DocumentId,
) -> Result<Vec<String>> {
    let values: Vec<(String, String, Option<i64>)> = sqlx::query_as(
        "SELECT field_key, value, page FROM field_values WHERE document_id = ? AND provenance = ?          ORDER BY field_key",
    )
    .bind(id)
    .bind(Provenance::Llm)
    .fetch_all(&mut *conn)
    .await?;
    if values.is_empty() {
        return Ok(Vec::new());
    }
    let layers: Vec<(i64, String)> = sqlx::query_as(
        "SELECT page, text FROM text_layers WHERE document_id = ? AND source = 'tesseract'",
    )
    .bind(id)
    .fetch_all(conn)
    .await?;
    let mut disagreeing = Vec::new();
    for (key, value, page) in values {
        let Ok(value) = serde_json::from_str::<FieldValue>(&value) else {
            continue;
        };
        let forms = digit_forms(&value);
        if forms.is_empty() {
            continue;
        }
        let texts: Vec<&str> = layers
            .iter()
            .filter(|(p, _)| page.is_none_or(|wanted| wanted == *p))
            .map(|(_, text)| text.as_str())
            .collect();
        if texts.is_empty() {
            continue;
        }
        let found = texts.iter().any(|text| {
            let all = digits(text);
            forms.iter().any(|form| all.contains(form.as_str()))
        });
        let misread = || {
            texts
                .iter()
                .flat_map(|text| digit_runs(text))
                .any(|run| forms.iter().any(|form| near_miss(form, &run)))
        };
        if !found && misread() {
            disagreeing.push(key);
        }
    }
    Ok(disagreeing)
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// The numbers printed in `text`, as digit strings: digits joined across
/// single separators ("1 800,00", "77 4423337", "160-009").
fn digit_runs(text: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut run = String::new();
    let mut gap = 0;
    for c in text.chars() {
        if c.is_ascii_digit() {
            run.push(c);
            gap = 0;
        } else if !run.is_empty() && gap == 0 && matches!(c, ' ' | '-' | '.' | ',' | '/') {
            gap = 1;
        } else if !run.is_empty() {
            runs.push(std::mem::take(&mut run));
            gap = 0;
        }
    }
    if !run.is_empty() {
        runs.push(run);
    }
    runs
}

/// Whether some stretch of `run` is `form` misread: within a third of its
/// digits (at least one) by edit distance, but not equal.
fn near_miss(form: &str, run: &str) -> bool {
    if run.contains(form) {
        return false;
    }
    let form: Vec<char> = form.chars().collect();
    let run: Vec<char> = run.chars().collect();
    let allowed = (form.len() / 3).max(1);
    if form.len() < 3 {
        return false;
    }
    let lens = form.len().saturating_sub(1)..=form.len() + 1;
    lens.filter(|len| *len > 0 && *len <= run.len())
        .flat_map(|len| run.windows(len))
        .map(|window| edit_distance(&form, window))
        .any(|d| d > 0 && d <= allowed)
}

fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The digit strings under which a value may legitimately appear in text.
/// Only money and identifiers are compared.
fn digit_forms(value: &FieldValue) -> Vec<String> {
    match value {
        FieldValue::Money { amount, .. } => {
            let mut forms = vec![
                digits(&amount.normalize().to_string()),
                digits(&format!("{:.2}", amount.round_dp(2))),
            ];
            forms.retain(|f| !f.is_empty());
            forms.dedup();
            forms
        }
        FieldValue::Identifier(s) => {
            let d = digits(s);
            if d.is_empty() { Vec::new() } else { vec![d] }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;
    use crate::service::testing::{insert_document, test_app};

    #[test]
    fn money_matches_with_or_without_decimals() {
        let money = |s: &str| FieldValue::Money {
            amount: s.parse::<Decimal>().unwrap(),
            currency: "EUR".into(),
        };
        assert_eq!(digit_forms(&money("1500.00")), vec!["1500", "150000"]);
        assert_eq!(digit_forms(&money("12.5")), vec!["125", "1250"]);
        assert!(digit_forms(&FieldValue::Text("1500".into())).is_empty());
    }

    async fn add_value(
        app: &App,
        id: DocumentId,
        key: &str,
        value: &str,
        provenance: &str,
        page: i64,
    ) {
        sqlx::query(
            "INSERT INTO field_values (document_id, field_key, value, provenance, page, updated_at) \
             VALUES (?, ?, ?, ?, ?, '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(key)
        .bind(value)
        .bind(provenance)
        .bind(page)
        .execute(&app.db.write)
        .await
        .unwrap();
    }

    async fn add_layer(app: &App, id: DocumentId, page: i64, source: &str, text: &str) {
        sqlx::query(
            "INSERT INTO text_layers (document_id, page, source, text, created_at) \
             VALUES (?, ?, ?, ?, '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(page)
        .bind(source)
        .bind(text)
        .execute(&app.db.write)
        .await
        .unwrap();
    }

    const TOTAL: &str = r#"{"kind":"money","value":{"amount":"1500.00","currency":"EUR"}}"#;

    #[tokio::test]
    async fn ocr_llm_disagree_when_digits_are_not_in_the_page_text() {
        let (_dir, app) = test_app().await;
        let id = insert_document(&app).await;
        add_value(&app, id, "total", TOTAL, "llm", 1).await;
        add_layer(&app, id, 1, "tesseract", "Итого к оплате 1 800,00 EUR").await;
        assert_eq!(run(&app, id).await.unwrap(), vec![OCR_LLM_DISAGREE]);
        let mut conn = app.db.read.acquire().await.unwrap();
        assert_eq!(
            field_issues(&mut conn, id).await.unwrap(),
            vec![("total".to_string(), OCR_LLM_DISAGREE)]
        );
    }

    #[tokio::test]
    async fn digits_found_ignoring_separators_do_not_disagree() {
        let (_dir, app) = test_app().await;
        let id = insert_document(&app).await;
        add_value(&app, id, "total", TOTAL, "llm", 1).await;
        add_layer(&app, id, 1, "tesseract", "Итого к оплате 1 500,00 EUR").await;
        assert!(run(&app, id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_value_the_ocr_did_not_read_at_all_cannot_disagree() {
        let (_dir, app) = test_app().await;
        let id = insert_document(&app).await;
        let number = r#"{"kind":"identifier","value":"I-ПН № 512346"}"#;
        add_value(&app, id, "certificate_number", number, "llm", 1).await;
        // Tesseract on a decorative certificate: noise and an unrelated code.
        add_layer(
            &app,
            id,
            1,
            "tesseract",
            "СВИДЕТ ЕЛЬ = о ае 12000034 Примерный",
        )
        .await;
        assert!(run(&app, id).await.unwrap().is_empty());

        // The same print read one digit off is a real disagreement.
        let misread = insert_document(&app).await;
        add_value(&app, misread, "certificate_number", number, "llm", 1).await;
        add_layer(&app, misread, 1, "tesseract", "I-ПН № 512340").await;
        assert_eq!(run(&app, misread).await.unwrap(), vec![OCR_LLM_DISAGREE]);
    }

    #[test]
    fn numbers_are_read_across_separators() {
        assert_eq!(
            digit_runs("Итого 1 800,00 EUR, код 160-009, 77 4423337"),
            vec!["180000", "160009", "774423337"]
        );
        assert!(near_miss("512346", "512340"));
        assert!(near_miss("150000", "180000"));
        assert!(!near_miss("512346", "512346"));
        assert!(!near_miss("345678", "09000000008800000055"));
        assert!(!near_miss("60000", "62026"));
    }

    #[tokio::test]
    async fn pages_without_a_tesseract_layer_cannot_disagree() {
        let (_dir, app) = test_app().await;
        let id = insert_document(&app).await;
        add_value(&app, id, "total", TOTAL, "llm", 1).await;
        add_layer(&app, id, 1, "embedded", "Итого к оплате 1 800,00 EUR").await;
        assert!(run(&app, id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn carried_flags_survive_and_derived_ones_do_not() {
        let (_dir, app) = test_app().await;
        let id = insert_document(&app).await;
        sqlx::query("UPDATE documents SET attention = ? WHERE id = ?")
            .bind(r#"["checksum_failed","llm_failed"]"#)
            .bind(id)
            .execute(&app.db.write)
            .await
            .unwrap();
        // checksum_failed is derived (nothing fails now); llm_failed is carried.
        assert_eq!(run(&app, id).await.unwrap(), vec![LLM_FAILED]);
    }

    #[tokio::test]
    async fn add_flag_keeps_canonical_order_and_is_idempotent() {
        let (_dir, app) = test_app().await;
        let id = insert_document(&app).await;
        let mut conn = app.db.write.acquire().await.unwrap();
        add_flag(&mut conn, id, LLM_FAILED).await.unwrap();
        add_flag(&mut conn, id, MRZ_MISMATCH).await.unwrap();
        add_flag(&mut conn, id, LLM_FAILED).await.unwrap();
        let stored: String = sqlx::query_scalar("SELECT attention FROM documents WHERE id = ?")
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(stored, r#"["mrz_mismatch","llm_failed"]"#);
    }

    #[tokio::test]
    async fn dismiss_in_clears_mrz_mismatch_and_bad_mrz_checksum_fact() {
        let (_dir, app) = test_app().await;
        let id = insert_document(&app).await;
        add_value(
            &app,
            id,
            "surname",
            r#"{"kind":"text","value":"ТЕСТОВА"}"#,
            "llm",
            1,
        )
        .await;
        sqlx::query("UPDATE field_values SET mrz_mismatch = 1 WHERE document_id = ? AND field_key = 'surname'")
            .bind(id)
            .execute(&app.db.write)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO facts (document_id, key, value, provenance, page) VALUES (?, ?, 'failed', 'mrz', 1)",
        )
        .bind(id)
        .bind(MRZ_CHECK_FACT)
        .execute(&app.db.write)
        .await
        .unwrap();

        let mut conn = app.db.write.acquire().await.unwrap();
        recompute_in(&mut conn, id, &[]).await.unwrap();
        assert_eq!(
            run_in(&mut conn, id, &[]).await.unwrap(),
            vec![MRZ_MISMATCH, CHECKSUM_FAILED]
        );
        assert_eq!(
            field_issues(&mut conn, id).await.unwrap(),
            vec![("surname".to_string(), MRZ_MISMATCH)]
        );

        let cleared = dismiss_in(
            &mut conn,
            id,
            &[MRZ_MISMATCH.to_string(), CHECKSUM_FAILED.to_string()],
        )
        .await
        .unwrap();
        assert_eq!(cleared, vec![MRZ_MISMATCH, CHECKSUM_FAILED]);
        assert!(run_in(&mut conn, id, &[]).await.unwrap().is_empty());
        assert!(field_issues(&mut conn, id).await.unwrap().is_empty());
    }
}
