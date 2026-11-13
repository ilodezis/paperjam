//! The name a document's file gets when it leaves the archive: its title, so a
//! download reads "Passport — Ivanov I. I.pdf" rather than "IMG_4821.jpg".

use std::path::Path;

/// Longest file name stem, in characters. File systems allow 255 bytes; a
/// Cyrillic title takes two bytes a character, and the extension follows.
const MAX_STEM_CHARS: usize = 120;

/// The file name for a download of the document titled `title`, saved as
/// `extension` (without the dot). Falls back to the uploaded name's stem when
/// the document has no usable title.
pub fn download_name(title: Option<&str>, original_name: &str, extension: &str) -> String {
    let stem = title
        .map(clean_stem)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let original = Path::new(original_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(original_name);
            let cleaned = clean_stem(original);
            if cleaned.is_empty() {
                "document".to_string()
            } else {
                cleaned
            }
        });
    if extension.is_empty() {
        stem
    } else {
        format!("{stem}.{extension}")
    }
}

/// The extension a file of `mime` is saved with: the uploaded name's own one
/// when it has one that fits, else the usual one for the type.
pub fn extension_for(mime: &str, original_name: &str) -> String {
    let own = Path::new(original_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|e| !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()));
    let usual = match mime {
        "application/pdf" => "pdf",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/tiff" => "tiff",
        "image/heic" | "image/heif" => "heic",
        "text/markdown" => "md",
        "text/plain" => "txt",
        _ => "",
    };
    match own {
        // A `.jpeg` stays `.jpeg`; a PDF uploaded as `scan.bin` becomes `.pdf`.
        Some(own) if usual.is_empty() || fits(&own, usual) => own,
        _ => usual.to_string(),
    }
}

fn fits(own: &str, usual: &str) -> bool {
    own == usual
        || matches!(
            (own, usual),
            ("jpeg" | "jpe", "jpg")
                | ("tif", "tiff")
                | ("heif", "heic")
                | ("markdown", "md")
                | ("text", "txt")
        )
}

/// `raw` without the characters file systems refuse, single-spaced, with no
/// leading or trailing dots or spaces, at most [`MAX_STEM_CHARS`] long.
fn clean_stem(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '|' => ' ',
            '*' | '?' | '"' | '<' | '>' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let single: String = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut stem: String = single.chars().take(MAX_STEM_CHARS).collect();
    if single.chars().count() > MAX_STEM_CHARS {
        // Cut at a word when one ends near the limit.
        if let Some(space) = stem.rfind(' ').filter(|i| *i > stem.len() / 2) {
            stem.truncate(space);
        }
    }
    stem.trim_matches(|c: char| c == '.' || c == ' ')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_names_the_file() {
        assert_eq!(
            download_name(Some("Паспорт РФ — Иванов И. И."), "IMG_4821.JPG", "pdf"),
            "Паспорт РФ — Иванов И. И.pdf"
        );
        assert_eq!(
            download_name(Some("Договор аренды"), "scan0003.pdf", "pdf"),
            "Договор аренды.pdf"
        );
    }

    #[test]
    fn characters_file_systems_refuse_are_dropped() {
        assert_eq!(
            download_name(Some("Счёт 12/2026: «ООО Ромашка» *копия*?"), "x.pdf", "pdf"),
            "Счёт 12 2026 «ООО Ромашка» копия.pdf"
        );
        assert_eq!(download_name(Some("a\nb\tc"), "x.pdf", "pdf"), "a b c.pdf");
        assert_eq!(
            download_name(Some("..hidden.."), "x.pdf", "pdf"),
            "hidden.pdf"
        );
    }

    #[test]
    fn without_a_title_the_uploaded_name_stays() {
        assert_eq!(download_name(None, "IMG_4821.JPG", "jpg"), "IMG_4821.jpg");
        assert_eq!(
            download_name(Some("  ///  "), "scan.pdf", "pdf"),
            "scan.pdf"
        );
        assert_eq!(download_name(None, "", "pdf"), "document.pdf");
        assert_eq!(download_name(None, "../../etc/passwd", "txt"), "passwd.txt");
    }

    #[test]
    fn long_titles_are_cut_at_a_word() {
        let title = "слово ".repeat(40);
        let name = download_name(Some(&title), "x.pdf", "pdf");
        let stem = name.strip_suffix(".pdf").unwrap();
        assert!(stem.chars().count() <= MAX_STEM_CHARS, "{stem}");
        assert!(stem.ends_with("слово"), "{stem}");
    }

    #[test]
    fn extensions_follow_the_type() {
        assert_eq!(extension_for("application/pdf", "scan.PDF"), "pdf");
        assert_eq!(extension_for("application/pdf", "scan.bin"), "pdf");
        assert_eq!(extension_for("image/jpeg", "photo.jpeg"), "jpeg");
        assert_eq!(extension_for("image/jpeg", "photo"), "jpg");
        assert_eq!(extension_for("image/png", "photo.jpg"), "png");
        assert_eq!(extension_for("text/plain", "notes.txt"), "txt");
        assert_eq!(extension_for("application/x-unknown", "a.xyz"), "xyz");
    }
}
