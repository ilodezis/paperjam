//! Markdown to plain text for snippets. The LLM transcription of a page is
//! Markdown (`**bold**`, `# headings`, tables, rules); a snippet is a few
//! words of context and should not show the syntax.

/// The text of `markdown` without block markers (headings, quotes, bullets,
/// rules, table borders), emphasis, code ticks, links (the link text stays)
/// and `<br>`. Line breaks are kept; only the syntax goes.
pub(super) fn plain(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    for line in markdown.lines() {
        let line = line.trim();
        if is_rule(line) {
            continue;
        }
        let line = strip_block_marker(line);
        let line = replace_links(line);
        push_inline(&line, &mut out);
        out.push('\n');
    }
    out
}

/// `---`, `***`, `___`, `===` and table separator rows such as `|---|:--:|`.
fn is_rule(line: &str) -> bool {
    !line.is_empty()
        && line.chars().all(|c| "-*_=|:+ \t".contains(c))
        && line.chars().filter(|c| "-*_=".contains(*c)).count() >= 3
}

fn strip_block_marker(mut line: &str) -> &str {
    while let Some(rest) = line.strip_prefix('>') {
        line = rest.trim_start();
    }
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if rest.is_empty() || rest.starts_with(' ') {
            return rest.trim_start();
        }
    }
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return rest.trim_start();
        }
    }
    line
}

/// `[text](url)` and `![alt](url)` become `text` / `alt`.
fn replace_links(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find("](").map(|i| open + i) else {
            break;
        };
        let Some(end) = rest[close + 2..].find(')').map(|i| close + 2 + i) else {
            break;
        };
        let before = &rest[..open];
        out.push_str(before.strip_suffix('!').unwrap_or(before));
        out.push_str(&rest[open + 1..close]);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn push_inline(line: &str, out: &mut String) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '*' | '`' => {}
            '|' => out.push(' '),
            '~' | '_' if chars.get(i) == Some(&c) => {
                while chars.get(i) == Some(&c) {
                    i += 1;
                }
            }
            '<' => match line_break_len(&chars[i..]) {
                Some(len) => {
                    out.push(' ');
                    i += len;
                }
                None => out.push('<'),
            },
            c => out.push(c),
        }
    }
}

/// Length of `br>`, `br/>` or `br />` (any case) at the start of `rest`.
fn line_break_len(rest: &[char]) -> Option<usize> {
    ["br>", "br/>", "br />"].iter().find_map(|tag| {
        let len = tag.chars().count();
        let head: String = rest.iter().take(len).collect();
        head.eq_ignore_ascii_case(tag).then_some(len)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_emphasis_and_rules_go() {
        assert_eq!(
            plain("# Title\n\n**Name:** Ivan\n---\n> quoted\n- item\n"),
            "Title\n\nName: Ivan\nquoted\nitem\n"
        );
    }

    #[test]
    fn tables_keep_their_cells() {
        let plain = plain("| A | B |\n|---|:-:|\n| 1 | 2 |");
        let words: Vec<&str> = plain.split_whitespace().collect();
        assert_eq!(words, vec!["A", "B", "1", "2"]);
    }

    #[test]
    fn links_images_code_and_breaks() {
        assert_eq!(
            plain("[a link](http://x.test) ![img](p.png) `code` ~~old~~ a<br>b a<br/>b"),
            "a link img code old a b a b\n"
        );
    }

    #[test]
    fn text_that_only_looks_like_markup_stays() {
        assert_eq!(
            plain("No. #12 costs 5 * 3 <ok>"),
            "No. #12 costs 5  3 <ok>\n"
        );
        assert_eq!(
            plain("a_b_c MRZ P<UTO<<ERIKSSON"),
            "a_b_c MRZ P<UTO<<ERIKSSON\n"
        );
        assert_eq!(plain("#hashtag"), "#hashtag\n");
    }
}
