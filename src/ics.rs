//! Writing an iCalendar (RFC 5545) feed of all-day events.

use chrono::{DateTime, NaiveDate, Utc};

/// Content lines are folded at this many octets, the line break excluded.
const LINE_OCTETS: usize = 75;

/// How often calendars are asked to poll, as an ISO 8601 duration.
const REFRESH: &str = "PT12H";

/// One all-day event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub uid: String,
    pub date: NaiveDate,
    pub summary: String,
    pub url: Option<String>,
}

/// TEXT escaping: backslash, semicolon, comma and line breaks.
pub fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// Folds one content line: no physical line is longer than 75 octets and a
/// continuation starts with one space. A character is never split.
pub fn fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + line.len() / LINE_OCTETS * 3 + 2);
    let mut used = 0;
    for c in line.chars() {
        let width = c.len_utf8();
        if used + width > LINE_OCTETS {
            out.push_str("\r\n ");
            // The leading space counts against the continuation line.
            used = 1;
        }
        out.push(c);
        used += width;
    }
    out.push_str("\r\n");
    out
}

/// A complete calendar named `name`, with `now` as every event's DTSTAMP.
pub fn calendar(name: &str, events: &[Event], now: DateTime<Utc>) -> String {
    let mut out = String::new();
    let mut line = |text: String| out.push_str(&fold(&text));
    line("BEGIN:VCALENDAR".into());
    line("VERSION:2.0".into());
    line("PRODID:-//paperjam//due dates//EN".into());
    line("CALSCALE:GREGORIAN".into());
    line("METHOD:PUBLISH".into());
    line(format!("X-WR-CALNAME:{}", escape_text(name)));
    line(format!("REFRESH-INTERVAL;VALUE=DURATION:{REFRESH}"));
    line(format!("X-PUBLISHED-TTL:{REFRESH}"));
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    for event in events {
        line("BEGIN:VEVENT".into());
        line(format!("UID:{}", event.uid));
        line(format!("DTSTAMP:{stamp}"));
        line(format!(
            "DTSTART;VALUE=DATE:{}",
            event.date.format("%Y%m%d")
        ));
        // The end of an all-day event is the day after it.
        if let Some(next) = event.date.succ_opt() {
            line(format!("DTEND;VALUE=DATE:{}", next.format("%Y%m%d")));
        }
        line(format!("SUMMARY:{}", escape_text(&event.summary)));
        if let Some(url) = &event.url {
            line(format!("URL:{url}"));
        }
        line("TRANSP:TRANSPARENT".into());
        line("END:VEVENT".into());
    }
    line("END:VCALENDAR".into());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 9, 8, 30, 0).unwrap()
    }

    fn event(summary: &str) -> Event {
        Event {
            uid: "doc-1-due@paperjam".into(),
            date: "2031-02-03".parse().unwrap(),
            summary: summary.into(),
            url: None,
        }
    }

    #[test]
    fn text_escapes_backslash_semicolon_comma_and_newlines() {
        assert_eq!(escape_text(r"a\b;c,d"), r"a\\b\;c\,d");
        assert_eq!(
            escape_text("one\ntwo\r\nthree\rfour"),
            r"one\ntwo\nthree\nfour"
        );
        assert_eq!(escape_text("plain Привет"), "plain Привет");
    }

    #[test]
    fn short_lines_are_not_folded() {
        assert_eq!(fold("SUMMARY:short"), "SUMMARY:short\r\n");
    }

    #[test]
    fn folded_lines_stay_within_75_octets_and_unfold_to_the_original() {
        let line = format!(
            "SUMMARY:{}",
            "Паспорт гражданина, срок действия; ".repeat(8)
        );
        let folded = fold(&line);
        let physical: Vec<&str> = folded.strip_suffix("\r\n").unwrap().split("\r\n").collect();
        assert!(physical.len() > 1);
        for (i, part) in physical.iter().enumerate() {
            assert!(part.len() <= 75, "line {i} is {} octets", part.len());
            assert_eq!(part.starts_with(' '), i > 0);
        }
        // Every piece is valid UTF-8 by construction; unfolding restores the line.
        assert_eq!(
            folded.strip_suffix("\r\n").unwrap().replace("\r\n ", ""),
            line
        );
    }

    #[test]
    fn folding_a_multibyte_character_at_the_boundary_moves_it_whole() {
        // 74 ASCII octets then a 2-octet character: it does not fit on the line.
        let line = format!("{}Я", "x".repeat(74));
        assert_eq!(fold(&line), format!("{}\r\n Я\r\n", "x".repeat(74)));
        // A 4-octet character at the start of a continuation line.
        let line = format!("{}😀{}", "x".repeat(75), "y".repeat(70));
        let folded = fold(&line);
        assert!(folded.split("\r\n").all(|l| l.len() <= 75));
    }

    #[test]
    fn the_calendar_has_crlf_endings_and_the_required_properties() {
        let text = calendar("paperjam", &[event("Passport expires")], at());
        assert!(text.ends_with("END:VCALENDAR\r\n"));
        assert!(!text.replace("\r\n", "").contains(['\r', '\n']));
        for expected in [
            "BEGIN:VCALENDAR\r\n",
            "VERSION:2.0\r\n",
            "PRODID:-//paperjam//due dates//EN\r\n",
            "CALSCALE:GREGORIAN\r\n",
            "X-WR-CALNAME:paperjam\r\n",
            "REFRESH-INTERVAL;VALUE=DURATION:PT12H\r\n",
            "X-PUBLISHED-TTL:PT12H\r\n",
            "UID:doc-1-due@paperjam\r\n",
            "DTSTAMP:20261109T083000Z\r\n",
            "DTSTART;VALUE=DATE:20310203\r\n",
            "DTEND;VALUE=DATE:20310204\r\n",
            "SUMMARY:Passport expires\r\n",
            "END:VEVENT\r\n",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
        assert!(!text.contains("URL:"));
    }

    #[test]
    fn a_summary_is_escaped_and_a_url_is_written_verbatim() {
        let mut e = event("Lease; flat, 2");
        e.url = Some("https://docs.example.org/documents/abc".into());
        let text = calendar("paperjam", &[e], at());
        assert!(text.contains("SUMMARY:Lease\\; flat\\, 2\r\n"));
        assert!(text.contains("URL:https://docs.example.org/documents/abc\r\n"));
    }
}
