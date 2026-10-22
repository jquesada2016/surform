//! A small markdown subset renderer for the docs sources.
//!
//! The site's prose needs far less than full markdown, and the example
//! fences are Surform-specific anyway — so `docgen` carries its own
//! renderer instead of a markdown crate. Supported: `##`–`####`
//! headings, paragraphs, `-` bullets and `1.` numbered lists (one
//! nesting level), GFM pipe tables, `>` blockquotes, fenced code
//! blocks (with the fence's info string kept for verification), `---`
//! horizontal rules, and inline code / bold / emphasis / links.
//! Everything else passes through as plain escaped text.

/// One block-level element. Fences keep their info string and source
/// line so the verifier can find and re-anchor them.
#[derive(Debug)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Para {
        text: String,
    },
    List {
        ordered: bool,
        items: Vec<String>,
    },
    Table {
        head: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Code {
        info: String,
        body: String,
        line: usize,
    },
    Quote {
        text: String,
    },
    Hr,
}

/// Splits an optional `---` frontmatter header (`key: value` lines)
/// off the source; returns (attrs, rest).
pub fn split_frontmatter(src: &str) -> (Vec<(String, String)>, &str) {
    let mut lines = src.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (Vec::new(), src);
    }
    let mut attrs = Vec::new();
    let mut consumed = 1;
    for line in lines {
        consumed += 1;
        let trimmed = line.trim();
        if trimmed == "---" {
            let rest_start: usize = src.lines().take(consumed).map(|l| l.len() + 1).sum();
            return (attrs, &src[rest_start..]);
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            attrs.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    (Vec::new(), src)
}

/// Parses the markdown subset into blocks. `line` is the 1-based
/// source line a block starts at (used for build-error reporting).
pub fn parse(src: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let flush_para = |para: &mut Vec<String>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            blocks.push(Block::Para {
                text: para.join(" "),
            });
            para.clear();
        }
    };

    let mut lines = src.lines().enumerate().peekable();
    while let Some((idx, line)) = lines.next() {
        let no = idx + 1;
        let trimmed = line.trim();

        // Fenced code.
        if let Some(rest) = trimmed.strip_prefix("```") {
            flush_para(&mut para, &mut blocks);
            let info = rest.trim().to_string();
            let mut body = Vec::new();
            for (jdx, jline) in lines.by_ref() {
                if jline.trim() == "```" {
                    break;
                }
                let _ = jdx;
                body.push(jline);
            }
            blocks.push(Block::Code {
                info,
                body: body.join("\n"),
                line: no,
            });
            continue;
        }

        // Blank line ends a paragraph.
        if trimmed.is_empty() {
            flush_para(&mut para, &mut blocks);
            continue;
        }

        // Headings.
        if let Some(rest) = trimmed.strip_prefix("#### ") {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block::Heading {
                level: 4,
                text: rest.trim().to_string(),
            });
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("### ") {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block::Heading {
                level: 3,
                text: rest.trim().to_string(),
            });
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block::Heading {
                level: 2,
                text: rest.trim().to_string(),
            });
            continue;
        }

        // Horizontal rule.
        if trimmed == "---" {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block::Hr);
            continue;
        }

        // Blockquote (single-paragraph callouts).
        if let Some(rest) = trimmed.strip_prefix("> ") {
            flush_para(&mut para, &mut blocks);
            let mut text = vec![rest.to_string()];
            while let Some((_, next)) = lines.peek() {
                if let Some(more) = next.trim().strip_prefix("> ") {
                    text.push(more.to_string());
                    lines.next();
                } else {
                    break;
                }
            }
            blocks.push(Block::Quote {
                text: text.join(" "),
            });
            continue;
        }

        // Tables: a header row followed by a `|---|` separator.
        if trimmed.starts_with('|')
            && lines.peek().is_some_and(|(_, next)| {
                let t = next.trim();
                t.starts_with('|')
                    && t.replace('|', "")
                        .trim()
                        .chars()
                        .all(|c| c == '-' || c == ':')
            })
        {
            flush_para(&mut para, &mut blocks);
            let head = split_row(trimmed);
            lines.next(); // consume the separator
            let mut rows = Vec::new();
            while let Some((_, next)) = lines.peek() {
                if next.trim().starts_with('|') {
                    rows.push(split_row(next.trim()));
                    lines.next();
                } else {
                    break;
                }
            }
            blocks.push(Block::Table { head, rows });
            continue;
        }

        // Lists: `- ` bullets and `N. ` numbers. A list continues
        // until a non-item line; an indented item flattens into the
        // list (the renderer has no nesting). An indented,
        // non-blank, non-item line is the current item's wrapped
        // continuation and joins its text — the sources wrap list
        // sentences across lines like any prose.
        if trimmed.starts_with("- ") {
            flush_para(&mut para, &mut blocks);
            let mut items = vec![
                trimmed
                    .strip_prefix("- ")
                    .expect("tested above")
                    .trim()
                    .to_string(),
            ];
            while let Some((_, next)) = lines.peek() {
                let t = next.trim();
                let indented = next.len() - next.trim_start().len() >= 2;
                if let Some(item) = t.strip_prefix("- ") {
                    items.push(item.trim().to_string());
                    lines.next();
                } else if !t.is_empty() && indented {
                    let last = items.last_mut().expect("items non-empty");
                    last.push(' ');
                    last.push_str(t);
                    lines.next();
                } else if t.is_empty() {
                    // A blank line continues a list (markdown's loose
                    // list): skip it — the loop breaks on whatever
                    // follows if it is not another item.
                    lines.next();
                } else {
                    break;
                }
            }
            blocks.push(Block::List {
                ordered: false,
                items,
            });
            continue;
        }
        let numbered = numbered_item(trimmed).is_some();
        if numbered {
            flush_para(&mut para, &mut blocks);
            let mut items = vec![numbered_item(trimmed).expect("checked above")];
            while let Some((_, next)) = lines.peek() {
                let t = next.trim();
                let indented = next.len() - next.trim_start().len() >= 2;
                if numbered_item(t).is_some() {
                    items.push(numbered_item(t).expect("checked above"));
                    lines.next();
                } else if !t.is_empty() && indented {
                    let last = items.last_mut().expect("items non-empty");
                    last.push(' ');
                    last.push_str(t);
                    lines.next();
                } else if t.is_empty() {
                    // Loose list: a blank line does not end it.
                    lines.next();
                } else {
                    break;
                }
            }
            blocks.push(Block::List {
                ordered: true,
                items,
            });
            continue;
        }

        // Otherwise: paragraph text.
        para.push(trimmed.to_string());
    }
    flush_para(&mut para, &mut blocks);
    blocks
}

fn split_row(row: &str) -> Vec<String> {
    let inner = row.strip_prefix('|').unwrap_or(row);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// `N. text` with a SHORT all-digit number — the ordered-item shape.
/// Deliberately strict: a wrapped prose line that merely starts with
/// a digit and contains ". " somewhere (e.g. `3.3.0*, *real …*) must
/// not become a list item.
fn numbered_item(t: &str) -> Option<String> {
    let d = t.find(". ")?;
    if d == 0 || d > 3 || !t[..d].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(t[d + 2..].trim().to_string())
}

/// Renders inline markdown (code spans, bold, emphasis, links) to
/// HTML, escaping everything else.
pub fn inline_html(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let bytes = md.as_bytes();
    let mut i = 0usize;
    // Scan char-wise with byte offsets; the sources are ASCII except
    // the odd typographic character, all of which pass through.
    while i < bytes.len() {
        let rest = &md[i..];
        if rest.starts_with('`')
            && let Some(end) = rest[1..].find('`')
        {
            let code = &rest[1..1 + end];
            out.push_str(&format!("<code>{}</code>", escape(code)));
            i += 1 + end + 1;
            continue;
        }
        if rest.starts_with('[')
            && let Some(close) = rest.find(']')
            && rest[close + 1..].starts_with('(')
            && let Some(paren) = rest[close + 2..].find(')')
        {
            let text = &rest[1..close];
            let url = &rest[close + 2..close + 2 + paren];
            out.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                escape(url),
                inline_html(text)
            ));
            i += close + 2 + paren + 1;
            continue;
        }
        if rest.starts_with("**")
            && let Some(end) = rest[2..].find("**")
        {
            out.push_str(&format!(
                "<strong>{}</strong>",
                inline_html(&rest[2..2 + end])
            ));
            i += 2 + end + 2;
            continue;
        }
        if rest.starts_with('*')
            && let Some(end) = rest[1..].find('*')
        {
            out.push_str(&format!("<em>{}</em>", inline_html(&rest[1..1 + end])));
            i += 1 + end + 1;
            continue;
        }
        // Consume one char (UTF-8 safe).
        let ch = rest.chars().next().expect("non-empty rest");
        out.push_str(&escape(&ch.to_string()));
        i += ch.len_utf8();
    }
    out
}

/// HTML-escapes text content.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headings_paras_and_fences() {
        let src = "## Hello\n\nSome *text* with `code`.\n\n```sdl\nDEFINE TABLE x;\n```\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 3);
        match &blocks[0] {
            Block::Heading { level: 2, text } => assert_eq!(text, "Hello"),
            other => panic!("unexpected {other:?}"),
        }
        match &blocks[1] {
            Block::Para { text } => assert!(text.contains("*text*")),
            other => panic!("unexpected {other:?}"),
        }
        match &blocks[2] {
            Block::Code { info, body, .. } => {
                assert_eq!(info, "sdl");
                assert_eq!(body, "DEFINE TABLE x;");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_tables() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
        match &parse(src)[0] {
            Block::Table { head, rows } => {
                assert_eq!(head, &["a", "b"]);
                assert_eq!(rows.len(), 2);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn inline_renders_code_bold_links_and_escapes() {
        assert_eq!(
            inline_html("see `a<b` and **bold** [x](y.html)"),
            "see <code>a&lt;b</code> and <strong>bold</strong> <a href=\"y.html\">x</a>"
        );
    }

    #[test]
    fn frontmatter_splits() {
        let (attrs, rest) = split_frontmatter("---\ntitle: Hi\nslug: hi\n---\nbody");
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0], ("title".into(), "Hi".into()));
        assert_eq!(rest, "body");
    }

    #[test]
    fn list_items_absorb_wrapped_continuation_lines() {
        // A sentence that wraps across source lines stays inside its
        // item, and a blank line between items keeps one list
        // (markdown's loose list).
        let blocks = parse("- first line\n  continues here\n\n- second\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::List {
                ordered: false,
                items,
            } => assert_eq!(
                items,
                &[
                    "first line continues here".to_string(),
                    "second".to_string()
                ]
            ),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn numbered_items_absorb_wrapped_continuation_lines() {
        let blocks = parse("1. one\n2. two\n   still two\n3. three\n");
        match &blocks[0] {
            Block::List {
                ordered: true,
                items,
            } => assert_eq!(
                items,
                &[
                    "one".to_string(),
                    "two still two".to_string(),
                    "three".to_string()
                ]
            ),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn a_paragraph_after_a_list_stays_outside_it() {
        // An unindented non-item line ends the list: the paragraph is
        // its own block, not swallowed into the last item.
        let blocks = parse("- item\ntrailing paragraph\n");
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[0], Block::List { .. }));
        assert!(matches!(blocks[1], Block::Para { .. }));
    }

    #[test]
    fn a_version_number_cannot_start_a_numbered_list() {
        // `3.3.0*, *real …` is digit-led and contains ". " (after
        // `verified*`), but it is prose mid-paragraph — the strict
        // short-number check must keep it a paragraph.
        let blocks = parse("badges — *applied to SurrealDB\n3.3.0*, *real ones*. They are\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Para { text } => assert!(text.contains("3.3.0*, *real ones*. They are")),
            other => panic!("unexpected {other:?}"),
        }
    }
}
