//! The page a share link opens (`/s/<token>`), and the page images and
//! download it uses (`/api/v1/shared/<token>/…`). No session: the token is
//! the authority, checked again on every request.

use axum::extract::State;
use axum::http::header::{
    ACCEPT_LANGUAGE, CACHE_CONTROL, CONTENT_SECURITY_POLICY, REFERRER_POLICY,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use chrono::{DateTime, Datelike, NaiveDate, Utc};

use crate::error::ServiceError;
use crate::http::api::documents::{derived_image, file_response};
use crate::http::{ApiPath, ApiResult, HttpState};
use crate::model::document::Role;
use crate::model::fields::{FieldValue, Labels};
use crate::oauth::authorize::{html_escape, render};
use crate::service::shares::{self, Shared};

const PAGE_TEMPLATE: &str = include_str!("share.html");
const GONE_TEMPLATE: &str = include_str!("share_gone.html");
const STYLE: &str = concat!(include_str!("base.css"), include_str!("share.css"));
const PAGE_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; font-src 'self'; \
     img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// The words of the page, by language.
struct Strings {
    lang: &'static str,
    /// "<name> shares a document with you"; `{name}` is the sharer.
    shared_by: &'static str,
    shared_anonymously: &'static str,
    untitled: &'static str,
    download_pdf: &'static str,
    /// Followed by the file's extension in capitals.
    download_file: &'static str,
    pages_label: &'static str,
    page: &'static str,
    no_preview: &'static str,
    summary: &'static str,
    details: &'static str,
    people: &'static str,
    footer: &'static str,
    gone_title: &'static str,
    gone_detail: &'static str,
    months: [&'static str; 12],
    kb: &'static str,
    mb: &'static str,
    decimal: char,
}

const EN: Strings = Strings {
    lang: "en",
    shared_by: "{name} shared a document with you",
    shared_anonymously: "A document was shared with you",
    untitled: "Document",
    download_pdf: "Download PDF",
    download_file: "Download",
    pages_label: "Pages",
    page: "Page",
    no_preview: "There is no preview of this file. Download it to open it.",
    summary: "Summary",
    details: "Details",
    people: "People",
    footer: "Shared from a personal paperjam archive. The link stops working when its time is up.",
    gone_title: "This link no longer works",
    gone_detail: "It has expired or was revoked. Ask whoever sent it for a new one.",
    months: [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ],
    kb: "KB",
    mb: "MB",
    decimal: '.',
};

const RU: Strings = Strings {
    lang: "ru",
    shared_by: "{name} делится с вами документом",
    shared_anonymously: "С вами поделились документом",
    untitled: "Документ",
    download_pdf: "Скачать PDF",
    download_file: "Скачать",
    pages_label: "Страницы",
    page: "Страница",
    no_preview: "У этого файла нет предпросмотра. Скачайте его, чтобы открыть.",
    summary: "Кратко",
    details: "Данные",
    people: "Люди",
    footer: "Документ из личного архива paperjam. Ссылка перестанет работать, когда выйдет её срок.",
    gone_title: "Ссылка больше не работает",
    gone_detail: "Её срок истёк или её отозвали. Попросите прислать новую.",
    months: [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ],
    kb: "КБ",
    mb: "МБ",
    decimal: ',',
};

/// Russian when the visitor's browser asks for it before English, else English.
fn strings(headers: &HeaderMap) -> &'static Strings {
    let header = headers
        .get(ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    for tag in header.split(',') {
        let primary = tag.split(';').next().unwrap_or("").trim();
        let primary = primary.split('-').next().unwrap_or("").to_ascii_lowercase();
        match primary.as_str() {
            "ru" | "uk" | "be" => return &RU,
            "en" => return &EN,
            _ => {}
        }
    }
    &EN
}

fn page_response(status: StatusCode, body: String) -> Response {
    let mut response = (status, Html(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(PAGE_CSP));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static("noindex, nofollow"),
    );
    response
}

/// `/s/<token>`: the shared document, or a page saying the link no longer works.
pub async fn page(
    State(state): State<HttpState>,
    headers: HeaderMap,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Response {
    let s = strings(&headers);
    match shares::open(&state.app, &token).await {
        Ok(shared) => page_response(StatusCode::OK, render_page(s, &token, &shared, Utc::now())),
        Err(e) => {
            if !matches!(e.kind, crate::error::ErrorKind::Unauthorized) {
                tracing::warn!(code = e.code, "a shared document could not be shown");
            }
            page_response(StatusCode::NOT_FOUND, render_gone(s))
        }
    }
}

fn render_gone(s: &Strings) -> String {
    render(
        GONE_TEMPLATE,
        &[
            ("lang", s.lang.to_string()),
            ("title", html_escape(s.gone_title)),
            ("detail", html_escape(s.gone_detail)),
            ("style", STYLE.to_string()),
        ],
    )
}

fn render_page(s: &Strings, token: &str, shared: &Shared, now: DateTime<Utc>) -> String {
    let doc = &shared.document;
    let title = doc
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(s.untitled);
    let shared_by = match shared
        .shared_by
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        Some(name) => s.shared_by.replace("{name}", name),
        None => s.shared_anonymously.to_string(),
    };
    let remaining = remaining(s, &shared.expires_at, now);

    let mut meta = Vec::new();
    if let Some(label) = shared
        .type_labels
        .as_ref()
        .and_then(|l| label_in(l, s.lang))
    {
        meta.push(label);
    }
    if let Some(date) = doc.document_date {
        meta.push(format_date(s, date));
    }
    if let Some(count) = doc.file.page_count.filter(|c| *c > 0) {
        meta.push(page_count(s, count));
    }
    let meta = if meta.is_empty() {
        String::new()
    } else {
        format!(
            "<ul class=\"meta\">{}</ul>",
            meta.iter()
                .map(|m| format!("<li>{}</li>", html_escape(m)))
                .collect::<String>()
        )
    };

    let download = &shared.download;
    let download_label = if download.mime == "application/pdf" {
        s.download_pdf.to_string()
    } else {
        let extension = download
            .filename
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_uppercase())
            .unwrap_or_default();
        format!("{} {extension}", s.download_file)
            .trim()
            .to_string()
    };

    let pages = if shared.pages.is_empty() {
        format!(
            "<div class=\"no-preview\"><svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" \
             stroke-width=\"1.6\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\">\
             <path d=\"M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z\"/><path d=\"M14 2v4a2 2 0 0 0 2 2h4\"/>\
             </svg><p>{}</p></div>",
            html_escape(s.no_preview)
        )
    } else {
        let total = shared.pages.len();
        shared
            .pages
            .iter()
            .enumerate()
            .map(|(i, page)| {
                let size = page
                    .size
                    .map(|(w, h)| format!(" width=\"{w}\" height=\"{h}\""))
                    .unwrap_or_default();
                // The first page is in view at once; the rest load as they come.
                let loading = if i == 0 { "eager" } else { "lazy" };
                let caption = if total > 1 {
                    format!("<figcaption>{} / {total}</figcaption>", i + 1)
                } else {
                    String::new()
                };
                format!(
                    "<figure class=\"page\"><img src=\"/api/v1/shared/{token}/pages/{n}.webp\"{size} \
                     loading=\"{loading}\" decoding=\"async\" alt=\"{alt}\">{caption}</figure>",
                    token = html_escape(token),
                    n = page.n,
                    alt = html_escape(&format!("{} {}", s.page, page.n)),
                )
            })
            .collect()
    };

    let details = if shared.show_details {
        render_details(s, shared)
    } else {
        String::new()
    };

    let og_description = format!("{shared_by} · {remaining}");
    render(
        PAGE_TEMPLATE,
        &[
            ("lang", s.lang.to_string()),
            ("title", html_escape(title)),
            ("og_description", html_escape(&og_description)),
            ("style", STYLE.to_string()),
            ("expiry", html_escape(&remaining)),
            ("shared_by", html_escape(&shared_by)),
            ("meta", meta),
            (
                "download_url",
                format!("/api/v1/shared/{}/download", html_escape(token)),
            ),
            ("download_label", html_escape(&download_label)),
            ("download_size", html_escape(&file_size(s, download.size))),
            ("details", details),
            ("pages_label", html_escape(s.pages_label)),
            ("pages", pages),
            ("footer", html_escape(s.footer)),
        ],
    )
}

/// The summary and the extracted values: fields of the type, other facts,
/// organizations by role and people.
fn render_details(s: &Strings, shared: &Shared) -> String {
    let doc = &shared.document;
    let mut rows: Vec<(String, String)> = Vec::new();
    for entry in &doc.fields {
        let label = shared
            .field_defs
            .iter()
            .find(|d| d.key == entry.key)
            .and_then(|d| label_in(&d.labels, s.lang))
            .unwrap_or_else(|| humanize(&entry.key));
        let value = format_value(s, &entry.value);
        if !value.trim().is_empty() {
            rows.push((label, value));
        }
    }
    for fact in &doc.facts {
        let label = fact
            .label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| humanize(&fact.key));
        if !fact.value.trim().is_empty() {
            rows.push((label, fact.value.clone()));
        }
    }
    for organization in &doc.organizations {
        rows.push((
            role_label(s, organization.role).to_string(),
            organization.name.clone(),
        ));
    }
    if !doc.people.is_empty() {
        let names: Vec<&str> = doc.people.iter().map(|p| p.name.as_str()).collect();
        rows.push((s.people.to_string(), names.join(", ")));
    }

    let summary = doc
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    if summary.is_none() && rows.is_empty() {
        return String::new();
    }
    let mut out = String::from("<section class=\"details\">");
    if let Some(summary) = summary {
        out.push_str(&format!(
            "<h2>{}</h2><p class=\"summary\">{}</p>",
            html_escape(s.summary),
            html_escape(summary)
        ));
    }
    if !rows.is_empty() {
        out.push_str(&format!(
            "<h2>{}</h2><dl class=\"facts\">",
            html_escape(s.details)
        ));
        for (label, value) in rows {
            out.push_str(&format!(
                "<div><dt>{}</dt><dd>{}</dd></div>",
                html_escape(&label),
                html_escape(&value)
            ));
        }
        out.push_str("</dl>");
    }
    out.push_str("</section>");
    out
}

/// A label in `lang`, else in English, else any.
fn label_in(labels: &Labels, lang: &str) -> Option<String> {
    labels
        .get(lang)
        .or_else(|| labels.get("en"))
        .or_else(|| labels.values().next())
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

/// `issue_date` → `Issue date`.
fn humanize(key: &str) -> String {
    let spaced = key.replace(['_', '-'], " ");
    let mut chars = spaced.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn role_label(s: &Strings, role: Role) -> &'static str {
    let ru = s.lang == "ru";
    match role {
        Role::IssuedBy => {
            if ru {
                "Выдан"
            } else {
                "Issued by"
            }
        }
        Role::ReceivedFrom => {
            if ru {
                "От кого"
            } else {
                "From"
            }
        }
        Role::Party => {
            if ru {
                "Сторона"
            } else {
                "Party"
            }
        }
        Role::Employer => {
            if ru {
                "Работодатель"
            } else {
                "Employer"
            }
        }
        Role::Unspecified => {
            if ru {
                "Организация"
            } else {
                "Organization"
            }
        }
    }
}

fn format_date(s: &Strings, date: NaiveDate) -> String {
    let month = s.months[date.month0() as usize];
    format!("{} {month} {}", date.day(), date.year())
}

fn format_value(s: &Strings, value: &FieldValue) -> String {
    match value {
        FieldValue::Text(t) | FieldValue::Identifier(t) => t.clone(),
        FieldValue::Date(d) => format_date(s, *d),
        FieldValue::Money { amount, currency } => {
            format!("{} {currency}", group_digits(s, &amount.to_string()))
        }
        FieldValue::Number(n) => group_digits(s, &n.to_string()),
        FieldValue::List(items) => items.join(", "),
    }
}

/// `1234567.5` → `1 234 567,5` (Russian) or `1,234,567.5` (English).
fn group_digits(s: &Strings, number: &str) -> String {
    let (sign, digits) = number
        .strip_prefix('-')
        .map_or(("", number), |rest| ("-", rest));
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if !whole.chars().all(|c| c.is_ascii_digit()) {
        return number.to_string();
    }
    let separator = if s.lang == "ru" { '\u{a0}' } else { ',' };
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(separator);
        }
        grouped.push(c);
    }
    if fraction.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}{}{fraction}", s.decimal)
    }
}

/// Russian plural: `forms` for 1, 2–4 and 5+ (`минута`, `минуты`, `минут`).
fn plural_ru(n: i64, forms: [&str; 3]) -> &str {
    let (tens, ones) = (n % 100, n % 10);
    if (11..=14).contains(&tens) {
        forms[2]
    } else if ones == 1 {
        forms[0]
    } else if (2..=4).contains(&ones) {
        forms[1]
    } else {
        forms[2]
    }
}

fn page_count(s: &Strings, n: u32) -> String {
    let n64 = i64::from(n);
    if s.lang == "ru" {
        format!(
            "{n} {}",
            plural_ru(n64, ["страница", "страницы", "страниц"])
        )
    } else if n == 1 {
        "1 page".to_string()
    } else {
        format!("{n} pages")
    }
}

/// How long the link still works: "15 more minutes", "ещё 3 часа".
fn remaining(s: &Strings, expires_at: &str, now: DateTime<Utc>) -> String {
    let left = DateTime::parse_from_rfc3339(expires_at)
        .map(|t| t.with_timezone(&Utc) - now)
        .unwrap_or_default();
    let minutes = (left.num_seconds().max(0) + 59) / 60;
    let ru = s.lang == "ru";
    if minutes >= 90 {
        let hours = (minutes + 30) / 60;
        if ru {
            format!(
                "Ссылка работает ещё {hours} {}",
                plural_ru(hours, ["час", "часа", "часов"])
            )
        } else if hours == 1 {
            "Link works for 1 more hour".to_string()
        } else {
            format!("Link works for {hours} more hours")
        }
    } else {
        let minutes = minutes.max(1);
        if ru {
            format!(
                "Ссылка работает ещё {minutes} {}",
                plural_ru(minutes, ["минуту", "минуты", "минут"])
            )
        } else if minutes == 1 {
            "Link works for 1 more minute".to_string()
        } else {
            format!("Link works for {minutes} more minutes")
        }
    }
}

fn file_size(s: &Strings, bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        let mb = bytes as f64 / (1024.0 * 1024.0);
        let text = format!("{mb:.1}").replace('.', &s.decimal.to_string());
        format!("{text}\u{a0}{}", s.mb)
    } else {
        format!("{}\u{a0}{}", bytes.div_ceil(1024).max(1), s.kb)
    }
}

/// Page `n` of a shared document as a cached WebP render.
#[utoipa::path(
    get,
    path = "/shared/{token}/pages/{file}",
    tag = "shares",
    summary = "Page n of a shared document as a WebP render; the share token is the authority.",
    params(
        ("token" = String, Path, description = "Share token"),
        ("file" = String, Path, description = "Page file name (e.g. 1.webp)"),
    ),
    responses(
        (status = 200, description = "Rendered WebP page image", body = Vec<u8>, content_type = "image/webp"),
        (status = 401, description = "The link is invalid, revoked or has expired", body = crate::http::ErrorResponse),
        (status = 404, description = "The page has no image", body = crate::http::ErrorResponse),
    )
)]
pub async fn page_image(
    State(state): State<HttpState>,
    ApiPath((token, file)): ApiPath<(String, String)>,
    request: HeaderMap,
) -> ApiResult<Response> {
    let id = shares::document_of(&state.app, &token).await?;
    let n: u32 = file
        .strip_suffix(".webp")
        .and_then(|n| n.parse().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| ServiceError::not_found("page_not_found", "pages are named <n>.webp"))?;
    let mut response = derived_image(&state, id, &format!("page-{n}.webp"), &request).await?;
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    Ok(response)
}

/// A shared document's file: the clean copy of a scan, else the original.
#[utoipa::path(
    get,
    path = "/shared/{token}/download",
    tag = "shares",
    summary = "A shared document's file (the clean copy of a scan, else the original); the share token is the authority.",
    params(
        ("token" = String, Path, description = "Share token"),
    ),
    responses(
        (status = 200, description = "File contents", body = Vec<u8>, content_type = "application/octet-stream"),
        (status = 401, description = "The link is invalid, revoked or has expired", body = crate::http::ErrorResponse),
    )
)]
pub async fn download(
    State(state): State<HttpState>,
    ApiPath(token): ApiPath<String>,
) -> ApiResult<Response> {
    let id = shares::document_of(&state.app, &token).await?;
    let file = crate::service::documents::download_file(
        &state.app.ctx(crate::service::Actor::system()),
        id,
    )
    .await?;
    let mut response = file_response(&state.app, file, false)?;
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn the_visitor_language_picks_the_words() {
        let mut headers = HeaderMap::new();
        assert_eq!(strings(&headers).lang, "en");
        headers.insert(
            ACCEPT_LANGUAGE,
            HeaderValue::from_static("ru-RU,ru;q=0.9,en;q=0.8"),
        );
        assert_eq!(strings(&headers).lang, "ru");
        headers.insert(
            ACCEPT_LANGUAGE,
            HeaderValue::from_static("de-DE, en-US;q=0.8, ru;q=0.5"),
        );
        assert_eq!(strings(&headers).lang, "en");
    }

    #[test]
    fn russian_plurals_and_remaining_time() {
        assert_eq!(plural_ru(1, ["минуту", "минуты", "минут"]), "минуту");
        assert_eq!(plural_ru(3, ["минуту", "минуты", "минут"]), "минуты");
        assert_eq!(plural_ru(11, ["минуту", "минуты", "минут"]), "минут");
        assert_eq!(plural_ru(21, ["час", "часа", "часов"]), "час");
        let now = Utc.with_ymd_and_hms(2026, 11, 5, 12, 0, 0).unwrap();
        assert_eq!(
            remaining(&RU, "2026-11-05T12:15:00.000Z", now),
            "Ссылка работает ещё 15 минут"
        );
        assert_eq!(
            remaining(&RU, "2026-11-06T12:00:00.000Z", now),
            "Ссылка работает ещё 24 часа"
        );
        assert_eq!(
            remaining(&EN, "2026-11-05T12:00:20.000Z", now),
            "Link works for 1 more minute"
        );
        assert_eq!(
            remaining(&EN, "2026-11-05T13:00:00.000Z", now),
            "Link works for 60 more minutes"
        );
    }

    #[test]
    fn values_read_naturally() {
        use rust_decimal::Decimal;
        use std::str::FromStr;
        let money = FieldValue::Money {
            amount: Decimal::from_str("1234567.50").unwrap(),
            currency: "RUB".into(),
        };
        assert_eq!(format_value(&RU, &money), "1\u{a0}234\u{a0}567,50 RUB");
        assert_eq!(format_value(&EN, &money), "1,234,567.50 RUB");
        let date = FieldValue::Date(NaiveDate::from_ymd_opt(2026, 3, 1).unwrap());
        assert_eq!(format_value(&RU, &date), "1 марта 2026");
        assert_eq!(format_value(&EN, &date), "1 March 2026");
        assert_eq!(humanize("issue_date"), "Issue date");
        assert_eq!(file_size(&RU, 1_258_291), "1,2\u{a0}МБ");
        assert_eq!(file_size(&EN, 2_000), "2\u{a0}KB");
        assert_eq!(page_count(&RU, 2), "2 страницы");
        assert_eq!(page_count(&EN, 1), "1 page");
    }

    #[test]
    fn templates_have_every_slot_filled() {
        // Each slot the templates name is one the renderers fill.
        for slot in [
            "lang",
            "title",
            "og_description",
            "style",
            "expiry",
            "shared_by",
            "meta",
            "download_url",
            "download_label",
            "download_size",
            "details",
            "pages_label",
            "pages",
            "footer",
        ] {
            assert!(PAGE_TEMPLATE.contains(&format!("{{{{{slot}}}}}")), "{slot}");
        }
        assert!(!STYLE.contains("{{"));
        let gone = render_gone(&RU);
        assert!(gone.contains("Ссылка больше не работает") && !gone.contains("{{"));
    }

    #[test]
    fn the_policy_lets_the_page_draw_its_own_style() {
        // The paper grain is an inline SVG image: a policy without `data:`
        // images blocks it and leaves an error in the visitor's console.
        assert!(STYLE.contains("url(\"data:image/svg+xml"));
        assert!(PAGE_CSP.contains("img-src 'self' data:"));
    }
}
