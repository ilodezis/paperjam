//! Pure RFC 5322 and MIME extraction for inbound email ingestion.

use std::borrow::Cow;

use mail_parser::{AddressList, DateTime, MessageParser, MessagePart, MessageRef, PartKind};

use crate::error::{Result, ServiceError};
use crate::service::upload::{SNIFF_BYTES, detect_mime};

const MAX_NESTED_DEPTH: usize = 5;
const MAX_ATTACHMENTS: usize = 50;
/// Small inline images outside `multipart/related` without a Content-ID are
/// usually tracking pixels or social icons; real phone photos are much larger.
const MIN_INLINE_IMAGE_BYTES: usize = 4 * 1024;
const MAX_BODY_FILENAME_CHARS: usize = 80;

const SERVICE_MIMES: [&str; 12] = [
    "application/pkcs7-signature",
    "application/x-pkcs7-signature",
    "application/pkcs7-mime",
    "application/pgp-signature",
    "application/ms-tnef",
    "text/calendar",
    "text/vcard",
    "text/x-vcard",
    "message/delivery-status",
    "message/disposition-notification",
    "message/rfc822",
    "message/global",
];

const SERVICE_EXTENSIONS: [&str; 6] = [".p7s", ".p7m", ".asc", ".sig", ".ics", ".vcf"];

/// A file extracted from an email, ready for [`crate::service::upload::upload`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFile {
    pub filename: String,
    pub declared_mime: Option<String>,
    pub bytes: Vec<u8>,
}

/// An attachment or embedded part intentionally not ingested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedAttachment {
    pub filename: String,
    pub reason: &'static str,
}

/// Result of parsing one raw RFC 5322 message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEmail {
    pub from_addresses: Vec<String>,
    pub from_display: Option<String>,
    pub subject: Option<String>,
    pub date: Option<String>,
    pub candidates: Vec<CandidateFile>,
    pub skipped: Vec<SkippedAttachment>,
    pub used_body_fallback: bool,
}

fn invalid_email() -> ServiceError {
    ServiceError::invalid(
        "invalid_email",
        "the request body is not a valid email message",
    )
}

fn has_standard_headers(message: &mail_parser::Message<'_>) -> bool {
    !message.headers().is_empty()
        && (message.from().is_some()
            || message.to().is_some()
            || message.cc().is_some()
            || message.subject().is_some()
            || message.date().is_some()
            || message.message_id().is_some()
            || message.content_type().is_some()
            || message.mime_version().is_some()
            || message.received().is_some())
}

fn clean_single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn collect_from_addresses<'a>(all_from: impl Iterator<Item = AddressList<'a>>) -> Vec<String> {
    let mut out = Vec::new();
    for list in all_from {
        for mailbox in list.mailboxes() {
            if let Some(addr) = mailbox.address() {
                let norm = addr.trim().to_ascii_lowercase();
                if !norm.is_empty() && !out.contains(&norm) {
                    out.push(norm);
                }
            }
        }
    }
    out
}

fn format_from_display(from: Option<AddressList<'_>>) -> Option<String> {
    let mailbox = from?.first()?;
    let addr = mailbox
        .address()
        .map(clean_single_line)
        .filter(|s| !s.is_empty());
    let name = mailbox
        .name()
        .map(clean_single_line)
        .filter(|s| !s.is_empty());
    match (name, addr) {
        (Some(n), Some(a)) if !n.eq_ignore_ascii_case(&a) => Some(format!("{n} <{a}>")),
        (_, Some(a)) => Some(a),
        (Some(n), None) => Some(n),
        (None, None) => None,
    }
}

fn format_utc_date(dt: DateTime) -> Option<String> {
    if !dt.is_valid() {
        return None;
    }
    let utc = DateTime::from_timestamp(dt.to_timestamp());
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        utc.year, utc.month, utc.day, utc.hour, utc.minute
    ))
}

fn is_service_mime(mime: Option<&str>) -> bool {
    mime.is_some_and(|m| SERVICE_MIMES.contains(&m))
}

fn is_service_filename(name: Option<&str>) -> bool {
    let Some(name) = name else {
        return false;
    };
    let lower = name.trim().to_ascii_lowercase();
    lower == "winmail.dat" || SERVICE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

fn is_text_attachment_allowed(name: Option<&str>, declared_mime: Option<&str>) -> bool {
    let Some(raw_name) = name.map(str::trim).filter(|s| !s.is_empty()) else {
        return matches!(declared_mime, Some("text/plain" | "text/markdown"));
    };
    let lower = raw_name.to_ascii_lowercase();
    if lower.ends_with(".txt") || lower.ends_with(".md") || lower.ends_with(".markdown") {
        return true;
    }
    let has_other_extension = raw_name.rsplit_once('.').is_some_and(|(_, ext)| {
        !ext.is_empty() && ext.len() <= 10 && ext.chars().all(|c| c.is_ascii_alphanumeric())
    });
    !has_other_extension && matches!(declared_mime, Some("text/plain" | "text/markdown"))
}

fn is_document_binary_mime(mime: &str) -> bool {
    matches!(
        mime,
        "application/pdf" | "image/jpeg" | "image/png" | "image/webp" | "image/tiff" | "image/heic"
    )
}

fn ext_for_mime(mime: &str) -> &'static str {
    match mime {
        "application/pdf" => "pdf",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/tiff" => "tif",
        "image/heic" => "heic",
        "text/markdown" => "md",
        "text/plain" => "txt",
        _ => "bin",
    }
}

struct WalkState {
    candidates: Vec<CandidateFile>,
    skipped: Vec<SkippedAttachment>,
    next_index: usize,
}

impl WalkState {
    fn next_filename(&mut self, raw_name: Option<&str>, ext: &str) -> String {
        if let Some(name) = raw_name.map(str::trim).filter(|s| !s.is_empty()) {
            return name.to_string();
        }
        let idx = self.next_index;
        self.next_index += 1;
        format!("attachment-{idx}.{ext}")
    }

    fn push_candidate(&mut self, filename: String, declared_mime: Option<String>, bytes: Vec<u8>) {
        if self.candidates.len() >= MAX_ATTACHMENTS {
            self.skipped.push(SkippedAttachment {
                filename,
                reason: "too_many_attachments",
            });
            return;
        }
        self.candidates.push(CandidateFile {
            filename,
            declared_mime,
            bytes,
        });
    }
}

fn walk_message(message: MessageRef<'_>, depth: usize, state: &mut WalkState) {
    walk_part(message.root_part(), false, depth, state);
}

fn walk_part(part: MessagePart<'_>, in_related: bool, depth: usize, state: &mut WalkState) {
    match part.kind() {
        PartKind::Multipart => {
            let related_here = in_related || part.is_content_type("multipart", "related");
            for child in part.children() {
                walk_part(child, related_here, depth, state);
            }
        }
        PartKind::Message(nested) => {
            if depth < MAX_NESTED_DEPTH {
                walk_message(nested, depth + 1, state);
            }
        }
        PartKind::Text | PartKind::Html | PartKind::Binary | PartKind::InlineBinary => {
            inspect_leaf(part, in_related, state);
        }
    }
}

fn inspect_leaf(part: MessagePart<'_>, in_related: bool, state: &mut WalkState) {
    let declared_mime = part.content_type().map(|ct| match ct.subtype() {
        Some(sub) => format!(
            "{}/{}",
            ct.ctype().to_ascii_lowercase(),
            sub.to_ascii_lowercase()
        ),
        None => ct.ctype().to_ascii_lowercase(),
    });
    let is_explicit_attachment = part
        .content_disposition()
        .is_some_and(|d| d.is_attachment());
    let raw_name = part.attachment_name();

    if is_service_mime(declared_mime.as_deref()) || is_service_filename(raw_name) {
        if is_explicit_attachment || raw_name.is_some() {
            let filename = state.next_filename(raw_name, "bin");
            state.skipped.push(SkippedAttachment {
                filename,
                reason: "unsupported_type",
            });
        }
        return;
    }

    if is_explicit_attachment {
        let bytes = part.decoded();
        if bytes.is_empty() {
            let filename = state.next_filename(raw_name, "bin");
            state.skipped.push(SkippedAttachment {
                filename,
                reason: "empty_file",
            });
            return;
        }
        let head = &bytes[..bytes.len().min(SNIFF_BYTES)];
        let name_hint = raw_name.unwrap_or("");
        match detect_mime(head, name_hint, declared_mime.as_deref()) {
            Ok(mime) if is_document_binary_mime(mime) => {
                let filename = state.next_filename(raw_name, ext_for_mime(mime));
                state.push_candidate(filename, Some(mime.to_string()), bytes.into_owned());
            }
            Ok(mime) if is_text_attachment_allowed(raw_name, declared_mime.as_deref()) => {
                let filename = state.next_filename(raw_name, ext_for_mime(mime));
                state.push_candidate(filename, Some(mime.to_string()), bytes.into_owned());
            }
            _ => {
                let filename = state.next_filename(raw_name, "bin");
                state.skipped.push(SkippedAttachment {
                    filename,
                    reason: "unsupported_type",
                });
            }
        }
        return;
    }

    // Inline or disposition-less text/HTML parts are email bodies or inline
    // disclaimers, never file attachments.
    if matches!(declared_mime.as_deref(), Some("text/plain" | "text/html"))
        || (declared_mime.is_none() && matches!(part.kind(), PartKind::Text | PartKind::Html))
    {
        return;
    }

    let bytes = part.decoded();
    if bytes.is_empty() {
        if raw_name.is_some() {
            let filename = state.next_filename(raw_name, "bin");
            state.skipped.push(SkippedAttachment {
                filename,
                reason: "empty_file",
            });
        }
        return;
    }

    let head = &bytes[..bytes.len().min(SNIFF_BYTES)];
    let name_hint = raw_name.unwrap_or("");
    match detect_mime(head, name_hint, declared_mime.as_deref()) {
        Ok("application/pdf") => {
            let filename = state.next_filename(raw_name, "pdf");
            state.push_candidate(
                filename,
                Some("application/pdf".to_string()),
                bytes.into_owned(),
            );
        }
        Ok(mime) if mime.starts_with("image/") => {
            if in_related || part.content_id().is_some() || bytes.len() < MIN_INLINE_IMAGE_BYTES {
                let filename = state.next_filename(raw_name, ext_for_mime(mime));
                state.skipped.push(SkippedAttachment {
                    filename,
                    reason: "inline_asset",
                });
                return;
            }
            // Inline photos outside `multipart/related` without a Content-ID
            // (for example, iPhone Mail photo attachments) are ingested.
            let filename = state.next_filename(raw_name, ext_for_mime(mime));
            state.push_candidate(filename, Some(mime.to_string()), bytes.into_owned());
        }
        _ => {
            if raw_name.is_some() || !matches!(part.kind(), PartKind::Text | PartKind::Html) {
                let filename = state.next_filename(raw_name, "bin");
                state.skipped.push(SkippedAttachment {
                    filename,
                    reason: "unsupported_type",
                });
            }
        }
    }
}

fn extract_body_text(message: &mail_parser::Message<'_>) -> Option<String> {
    let joined = join_non_empty(message.text_bodies());
    if !joined.is_empty() {
        return Some(joined);
    }
    let from_html = join_non_empty(
        message
            .html_bodies()
            .map(|html| Cow::Owned(mail_parser::html_to_text(&html))),
    );
    (!from_html.is_empty()).then_some(from_html)
}

fn join_non_empty<'a>(parts: impl Iterator<Item = Cow<'a, str>>) -> String {
    let mut chunks = Vec::new();
    for part in parts {
        let cleaned = normalize_body_text(&part);
        if !cleaned.is_empty() {
            chunks.push(cleaned);
        }
    }
    chunks.join("\n\n")
}

fn normalize_body_text(text: &str) -> String {
    let mut lines = Vec::new();
    let mut blank_run = 0usize;
    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 && !lines.is_empty() {
                lines.push("");
            }
        } else {
            blank_run = 0;
            lines.push(line);
        }
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn markdown_filename(subject: Option<&str>) -> String {
    let cleaned = subject
        .unwrap_or("")
        .chars()
        .map(|c| match c {
            c if c.is_control() => ' ',
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            other => other,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let truncated: String = cleaned.chars().take(MAX_BODY_FILENAME_CHARS).collect();
    let trimmed = truncated.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        return "email.md".to_string();
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".markdown") {
        trimmed.to_string()
    } else {
        format!("{trimmed}.md")
    }
}

fn format_markdown_body(
    subject: Option<&str>,
    from_display: Option<&str>,
    date: Option<&str>,
    body: &str,
) -> String {
    let mut out = String::new();
    let heading = subject
        .map(clean_single_line)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Email".to_string());
    out.push_str("# ");
    out.push_str(&heading);
    out.push('\n');
    let mut meta = Vec::new();
    if let Some(from) = from_display {
        meta.push(format!("From: {from}"));
    }
    if let Some(date) = date {
        meta.push(format!("Date: {date}"));
    }
    if !meta.is_empty() {
        out.push('\n');
        out.push_str(&meta.join("\n"));
        out.push('\n');
    }
    out.push('\n');
    out.push_str(body);
    out.push('\n');
    out
}

/// Parses raw RFC 5322 email bytes into uploadable attachments (or a Markdown
/// rendering of the body when `body_fallback` is true and no attachments exist).
pub fn parse_email(raw: &[u8], body_fallback: bool) -> Result<ParsedEmail> {
    let Some(message) = MessageParser::default().parse(raw) else {
        return Err(invalid_email());
    };
    if !has_standard_headers(&message) {
        return Err(invalid_email());
    }

    let from_addresses = collect_from_addresses(message.all_from());
    let from_display = format_from_display(message.from());
    let subject = message
        .subject()
        .map(clean_single_line)
        .filter(|s| !s.is_empty());
    let date = message
        .date()
        .or_else(|| message.resent_date())
        .and_then(format_utc_date);

    let mut state = WalkState {
        candidates: Vec::new(),
        skipped: Vec::new(),
        next_index: 1,
    };
    walk_message(message.root(), 0, &mut state);

    let mut used_body_fallback = false;
    if state.candidates.is_empty()
        && body_fallback
        && let Some(body) = extract_body_text(&message)
    {
        let md = format_markdown_body(
            subject.as_deref(),
            from_display.as_deref(),
            date.as_deref(),
            &body,
        );
        let filename = markdown_filename(subject.as_deref());
        state.candidates.push(CandidateFile {
            filename,
            declared_mime: Some("text/markdown".to_string()),
            bytes: md.into_bytes(),
        });
        used_body_fallback = true;
    }

    Ok(ParsedEmail {
        from_addresses,
        from_display,
        subject,
        date,
        candidates: state.candidates,
        skipped: state.skipped,
        used_body_fallback,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as B64;

    const PNG_1X1: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, b'I',
        b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
    ];

    fn fake_pdf(tag: &str) -> Vec<u8> {
        format!("%PDF-1.4\n% {tag}\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n").into_bytes()
    }

    fn fake_jpeg(len: usize) -> Vec<u8> {
        let mut out = vec![
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00,
        ];
        out.resize(len.max(out.len() + 2), 0x42);
        let n = out.len();
        out[n - 2] = 0xff;
        out[n - 1] = 0xd9;
        out
    }

    #[test]
    fn multipart_pdf_and_jpeg_attachments_are_extracted() {
        let pdf = fake_pdf("invoice");
        let jpg = fake_jpeg(512);
        let raw = format!(
            "From: Alice <Alice@Example.org>\r\n\
             Subject: Invoice and receipt\r\n\
             Date: Sat, 21 Nov 2026 12:30:00 +0300\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
             \r\n\
             --mix\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             \r\n\
             Please find attached.\r\n\
             --mix\r\n\
             Content-Type: application/pdf; name=\"invoice.pdf\"\r\n\
             Content-Disposition: attachment; filename=\"invoice.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --mix\r\n\
             Content-Type: image/jpeg; name=\"receipt.jpg\"\r\n\
             Content-Disposition: attachment; filename=\"receipt.jpg\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --mix--\r\n",
            B64.encode(&pdf),
            B64.encode(&jpg)
        );

        let parsed = parse_email(raw.as_bytes(), true).unwrap();
        assert_eq!(parsed.from_addresses, vec!["alice@example.org"]);
        assert_eq!(
            parsed.from_display.as_deref(),
            Some("Alice <Alice@Example.org>")
        );
        assert_eq!(parsed.subject.as_deref(), Some("Invoice and receipt"));
        assert_eq!(parsed.date.as_deref(), Some("2026-11-21 09:30 UTC"));
        assert!(!parsed.used_body_fallback);
        assert_eq!(parsed.candidates.len(), 2);
        assert_eq!(parsed.candidates[0].filename, "invoice.pdf");
        assert_eq!(parsed.candidates[0].bytes, pdf);
        assert_eq!(parsed.candidates[1].filename, "receipt.jpg");
        assert_eq!(parsed.candidates[1].bytes, jpg);
        assert!(parsed.skipped.is_empty());
    }

    #[test]
    fn related_cid_logos_are_skipped_unless_explicit_attachment() {
        let pdf = fake_pdf("bill");
        let raw = format!(
            "From: billing@example.org\r\n\
             Subject: Monthly statement\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
             \r\n\
             --mix\r\n\
             Content-Type: multipart/related; boundary=\"rel\"\r\n\
             \r\n\
             --rel\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             \r\n\
             <p>See bill</p><img src=\"cid:logo1\">\r\n\
             --rel\r\n\
             Content-Type: image/png; name=\"logo.png\"\r\n\
             Content-ID: <logo1>\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --rel\r\n\
             Content-Type: image/png; name=\"scan.png\"\r\n\
             Content-Disposition: attachment; filename=\"scan.png\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --rel--\r\n\
             --mix\r\n\
             Content-Type: application/pdf; name=\"bill.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --mix--\r\n",
            B64.encode(PNG_1X1),
            B64.encode(PNG_1X1),
            B64.encode(&pdf)
        );

        let parsed = parse_email(raw.as_bytes(), true).unwrap();
        assert_eq!(parsed.candidates.len(), 2);
        assert_eq!(parsed.candidates[0].filename, "scan.png");
        assert_eq!(parsed.candidates[1].filename, "bill.pdf");
        assert_eq!(
            parsed.skipped,
            vec![SkippedAttachment {
                filename: "logo.png".to_string(),
                reason: "inline_asset",
            }]
        );
    }

    #[test]
    fn iphone_inline_photos_are_kept_and_tiny_pixels_are_skipped() {
        let big_photo = fake_jpeg(6000);
        let tiny_pixel = PNG_1X1;
        let raw = format!(
            "From: alice@example.org\r\n\
             Subject: Photos\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
             \r\n\
             --mix\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             Sent from my phone\r\n\
             --mix\r\n\
             Content-Type: image/jpeg; name=\"IMG_0001.JPG\"\r\n\
             Content-Disposition: inline; filename=\"IMG_0001.JPG\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --mix\r\n\
             Content-Type: image/png; name=\"tracker.png\"\r\n\
             Content-Disposition: inline; filename=\"tracker.png\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --mix--\r\n",
            B64.encode(&big_photo),
            B64.encode(tiny_pixel)
        );

        let parsed = parse_email(raw.as_bytes(), true).unwrap();
        assert_eq!(parsed.candidates.len(), 1);
        assert_eq!(parsed.candidates[0].filename, "IMG_0001.JPG");
        assert_eq!(parsed.candidates[0].bytes, big_photo);
        assert_eq!(
            parsed.skipped,
            vec![SkippedAttachment {
                filename: "tracker.png".to_string(),
                reason: "inline_asset",
            }]
        );
    }

    #[test]
    fn service_and_office_attachments_are_rejected_and_txt_md_kept() {
        let raw = format!(
            "From: alice@example.org\r\n\
             Subject: Mixed attachments\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
             \r\n\
             --mix\r\n\
             Content-Type: text/calendar; name=\"invite.ics\"\r\n\
             Content-Disposition: attachment; filename=\"invite.ics\"\r\n\
             \r\n\
             BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n\
             --mix\r\n\
             Content-Type: text/plain; name=\"contact.vcf\"\r\n\
             Content-Disposition: attachment; filename=\"contact.vcf\"\r\n\
             \r\n\
             BEGIN:VCARD\r\nEND:VCARD\r\n\
             --mix\r\n\
             Content-Type: application/pkcs7-signature; name=\"smime.p7s\"\r\n\
             Content-Disposition: attachment; filename=\"smime.p7s\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --mix\r\n\
             Content-Type: application/octet-stream; name=\"winmail.dat\"\r\n\
             Content-Disposition: attachment; filename=\"winmail.dat\"\r\n\
             \r\n\
             plain lookalike\r\n\
             --mix\r\n\
             Content-Type: application/vnd.openxmlformats-officedocument.wordprocessingml.document; name=\"doc.docx\"\r\n\
             Content-Disposition: attachment; filename=\"doc.docx\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --mix\r\n\
             Content-Type: text/csv; name=\"data.csv\"\r\n\
             Content-Disposition: attachment; filename=\"data.csv\"\r\n\
             \r\n\
             a,b\r\n1,2\r\n\
             --mix\r\n\
             Content-Type: text/plain; name=\"notes.txt\"\r\n\
             Content-Disposition: attachment; filename=\"notes.txt\"\r\n\
             \r\n\
             Remember to renew.\r\n\
             --mix\r\n\
             Content-Type: text/markdown; name=\"summary.md\"\r\n\
             Content-Disposition: attachment; filename=\"summary.md\"\r\n\
             \r\n\
             # Summary\r\n\
             --mix--\r\n",
            B64.encode(b"sigbytes"),
            B64.encode(b"PK\x03\x04\x14\0\0\0\x08\0"),
        );

        let parsed = parse_email(raw.as_bytes(), true).unwrap();
        let kept: Vec<&str> = parsed
            .candidates
            .iter()
            .map(|c| c.filename.as_str())
            .collect();
        assert_eq!(kept, vec!["notes.txt", "summary.md"]);
        assert_eq!(
            parsed.skipped,
            vec![
                SkippedAttachment {
                    filename: "invite.ics".to_string(),
                    reason: "unsupported_type",
                },
                SkippedAttachment {
                    filename: "contact.vcf".to_string(),
                    reason: "unsupported_type",
                },
                SkippedAttachment {
                    filename: "smime.p7s".to_string(),
                    reason: "unsupported_type",
                },
                SkippedAttachment {
                    filename: "winmail.dat".to_string(),
                    reason: "unsupported_type",
                },
                SkippedAttachment {
                    filename: "doc.docx".to_string(),
                    reason: "unsupported_type",
                },
                SkippedAttachment {
                    filename: "data.csv".to_string(),
                    reason: "unsupported_type",
                },
            ]
        );
    }

    #[test]
    fn nested_rfc822_attachment_is_extracted_and_depth_is_capped() {
        let pdf = fake_pdf("forwarded");
        let inner = format!(
            "From: bank@example.org\r\n\
             Subject: Inner\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"inner\"\r\n\
             \r\n\
             --inner\r\n\
             Content-Type: application/pdf; name=\"statement.pdf\"\r\n\
             Content-Disposition: attachment; filename=\"statement.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --inner--\r\n",
            B64.encode(&pdf)
        );
        let mut wrapped = inner;
        // Wrap deeper than MAX_NESTED_DEPTH to verify bounded termination.
        for level in 0..=(MAX_NESTED_DEPTH + 1) {
            wrapped = format!(
                "From: alice@example.org\r\n\
                 Subject: Fwd {level}\r\n\
                 MIME-Version: 1.0\r\n\
                 Content-Type: multipart/mixed; boundary=\"b{level}\"\r\n\
                 \r\n\
                 --b{level}\r\n\
                 Content-Type: message/rfc822\r\n\
                 \r\n\
                 {wrapped}\r\n\
                 --b{level}--\r\n"
            );
        }
        let deep_parsed = parse_email(wrapped.as_bytes(), false).unwrap();
        assert!(deep_parsed.candidates.is_empty());

        let single_fwd = format!(
            "From: alice@example.org\r\n\
             Subject: Fwd statement\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
             \r\n\
             --outer\r\n\
             Content-Type: message/rfc822\r\n\
             \r\n\
             From: bank@example.org\r\n\
             Subject: Statement\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"in\"\r\n\
             \r\n\
             --in\r\n\
             Content-Type: application/pdf; name=\"statement.pdf\"\r\n\
             Content-Disposition: attachment; filename=\"statement.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --in--\r\n\
             --outer--\r\n",
            B64.encode(&pdf)
        );
        let parsed = parse_email(single_fwd.as_bytes(), true).unwrap();
        assert_eq!(parsed.candidates.len(), 1);
        assert_eq!(parsed.candidates[0].filename, "statement.pdf");
        assert_eq!(parsed.candidates[0].bytes, pdf);
        assert!(!parsed.used_body_fallback);
    }

    #[test]
    fn html_body_fallback_generates_markdown_only_without_attachments() {
        let html_email = "From: Shop <shop@example.org>\r\n\
             Subject: Receipt #42 / Order\r\n\
             Date: Sat, 21 Nov 2026 10:00:00 +0000\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             \r\n\
             <html><body><h1>Thank you</h1><p>Total: 49.90 EUR</p></body></html>\r\n";

        let with_fallback = parse_email(html_email.as_bytes(), true).unwrap();
        assert!(with_fallback.used_body_fallback);
        assert_eq!(with_fallback.candidates.len(), 1);
        assert_eq!(with_fallback.candidates[0].filename, "Receipt #42 Order.md");
        assert_eq!(
            with_fallback.candidates[0].declared_mime.as_deref(),
            Some("text/markdown")
        );
        let md = String::from_utf8(with_fallback.candidates[0].bytes.clone()).unwrap();
        assert!(md.starts_with("# Receipt #42 / Order\n\n"));
        assert!(md.contains("From: Shop <shop@example.org>\n"));
        assert!(md.contains("Date: 2026-11-21 10:00 UTC\n"));
        assert!(md.contains("Thank you"));
        assert!(md.contains("49.90 EUR"));
        assert!(!md.contains("<html>"));

        let without_fallback = parse_email(html_email.as_bytes(), false).unwrap();
        assert!(!without_fallback.used_body_fallback);
        assert!(without_fallback.candidates.is_empty());

        let pdf = fake_pdf("with-html");
        let with_pdf = format!(
            "From: shop@example.org\r\n\
             Subject: With PDF\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"m\"\r\n\
             \r\n\
             --m\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             \r\n\
             <p>Ignored HTML body</p>\r\n\
             --m\r\n\
             Content-Type: application/pdf; name=\"receipt.pdf\"\r\n\
             Content-Disposition: attachment; filename=\"receipt.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --m--\r\n",
            B64.encode(&pdf)
        );
        let parsed = parse_email(with_pdf.as_bytes(), true).unwrap();
        assert!(!parsed.used_body_fallback);
        assert_eq!(parsed.candidates.len(), 1);
        assert_eq!(parsed.candidates[0].filename, "receipt.pdf");
    }

    #[test]
    fn cyrillic_subjects_filenames_and_windows1251_bodies_decode() {
        let subj_b64 = B64.encode("Договор аренды".as_bytes());
        let name_b64 = B64.encode("договор.pdf".as_bytes());
        let pdf = fake_pdf("cyrillic");
        let raw = format!(
            "From: alice@example.org\r\n\
             Subject: =?UTF-8?B?{subj_b64}?=\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             Content-Type: application/pdf\r\n\
             Content-Disposition: attachment; filename=\"=?UTF-8?B?{name_b64}?=\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --b--\r\n",
            B64.encode(&pdf)
        );
        let parsed = parse_email(raw.as_bytes(), true).unwrap();
        assert_eq!(parsed.subject.as_deref(), Some("Договор аренды"));
        assert_eq!(parsed.candidates[0].filename, "договор.pdf");

        // `windows-1251` plain-text email without attachments ("Счёт на оплату").
        let encoded = b"\xD1\xF7\xB8\xF2 \xED\xE0 \xEE\xEF\xEB\xE0\xF2\xF3";
        let mut cp1251_mail = b"From: alice@example.org\r\n\
             Subject: =?UTF-8?B?"
            .to_vec();
        cp1251_mail.extend_from_slice(subj_b64.as_bytes());
        cp1251_mail
            .extend_from_slice(b"?=\r\nContent-Type: text/plain; charset=windows-1251\r\n\r\n");
        cp1251_mail.extend_from_slice(encoded);
        let parsed = parse_email(&cp1251_mail, true).unwrap();
        let md = String::from_utf8(parsed.candidates[0].bytes.clone()).unwrap();
        assert!(md.contains("Счёт на оплату"), "{md}");
    }

    #[test]
    fn garbage_and_empty_inputs_are_rejected() {
        for raw in [&b""[..], b"not an email at all", PNG_1X1] {
            let err = parse_email(raw, true).unwrap_err();
            assert_eq!(err.code, "invalid_email");
        }
    }
}
