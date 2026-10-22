//! Build-time syntax highlighting for the site's code panels.
//!
//! A tiny tokenizer (comments, strings, numbers, keywords) applied on
//! the server so pages ship fully highlighted — no client-side
//! flashing, no JavaScript dependency for reading. Misses just render
//! as plain text; highlighting is cosmetic and must never be load-
//! bearing.

/// SurrealQL/SDL vocabulary the highlighter knows. Cosmetic: a word
/// missing from this set still renders (plain).
const KEYWORDS: &[&str] = &[
    // statements
    "define",
    "remove",
    "use",
    "ns",
    "db",
    "namespace",
    "database",
    "table",
    "mixin",
    "field",
    "index",
    "event",
    "function",
    "user",
    "access",
    "analyzer",
    "param",
    "sequence",
    "config",
    "api",
    "bucket",
    "module",
    "raw",
    "overwrite",
    "if",
    "not",
    "exists",
    // table/field clauses
    "type",
    "schemafull",
    "schemaless",
    "flexible",
    "normal",
    "relation",
    "view",
    "any",
    "enforced",
    "lightweight",
    "drop",
    "fields",
    "on",
    "in",
    "out",
    "from",
    "to",
    "value",
    "default",
    "assert",
    "readonly",
    "computed",
    "reference",
    "permissions",
    "comment",
    "inline",
    "edges",
    "references",
    "as",
    "changefeed",
    "expiry",
    "include",
    "original",
    "strict",
    "concurrently",
    // index kinds + clauses
    "unique",
    "count",
    "fulltext",
    "hnsw",
    "diskann",
    "mtree",
    "highlights",
    "tokenizers",
    "filters",
    "class",
    "bm25",
    "doc_ids",
    "az",
    "efc",
    "ef",
    "m",
    "m0",
    "lm",
    "dist",
    "cosine",
    "euclidean",
    "manhattan",
    "minkowski",
    "hamming",
    "sqrt",
    // events / functions
    "when",
    "then",
    "async",
    "retry",
    "maxdepth",
    // access / users
    "jwt",
    "bearer",
    "record",
    "url",
    "issuer",
    "algorithm",
    "audience",
    "context",
    "signin",
    "signup",
    "authenticate",
    "session",
    "grant",
    "token",
    "roles",
    "role",
    "viewer",
    "editor",
    "owner",
    "passhash",
    "password",
    "passscram",
    "hs256",
    "hs384",
    "hs512",
    "es256",
    "es384",
    "es512",
    "ed25519",
    "rs256",
    "rs384",
    "rs512",
    "ps256",
    "ps384",
    "ps512",
    // sequences / params / config / buckets
    "start",
    "batch",
    "timeout",
    "set",
    "returns",
    "storage",
    "apply",
    "graphql",
    "tables",
    "functions",
    "depth",
    "complexity",
    "introspection",
    "middleware",
    "select",
    "create",
    "update",
    "delete",
    "relate",
    "return",
    "none",
    "full",
    "where",
    "group",
    "limit",
    "split",
    "fetch",
    "true",
    "false",
    "null",
    // modules
    "unsigned",
    "capabilities",
    // common kind leaves the panels show
    "string",
    "int",
    "float",
    "number",
    "bool",
    "datetime",
    "duration",
    "object",
    "array",
    "option",
    "geometry",
    "bytes",
    "uuid",
    "decimal",
];

/// Highlights SurrealQL/SDL code to HTML. Input is raw text; every
/// emitted fragment is HTML-escaped.
pub fn sdl(code: &str) -> String {
    let mut out = String::with_capacity(code.len() + 64);
    let mut rest = code;
    while !rest.is_empty() {
        // Comments: `//`, `#`, and `--` to end of line, `/* … */` blocks.
        if rest.starts_with("//") || rest.starts_with("--") || rest.starts_with('#') {
            let end = rest.find('\n').unwrap_or(rest.len());
            push(&mut out, "tok-c", &rest[..end]);
            rest = &rest[end..];
            continue;
        }
        if rest.starts_with("/*") {
            match rest[2..].find("*/") {
                Some(end) => {
                    push(&mut out, "tok-c", &rest[..2 + end + 2]);
                    rest = &rest[2 + end + 2..];
                }
                None => {
                    push(&mut out, "tok-c", rest);
                    break;
                }
            }
            continue;
        }
        // Strings: '…' / "…" with backslash escapes, optional f prefix.
        let f_prefixed = rest.starts_with("f\"") || rest.starts_with("f'");
        let quote = if f_prefixed {
            rest.as_bytes()[1]
        } else if rest.starts_with('\'') || rest.starts_with('"') {
            rest.as_bytes()[0]
        } else {
            0
        };
        if quote != 0 {
            let skip = if f_prefixed { 2 } else { 1 };
            let mut end = skip;
            let bytes = rest.as_bytes();
            while end < bytes.len() {
                if bytes[end] == b'\\' && end + 1 < bytes.len() {
                    end += 2;
                    continue;
                }
                if bytes[end] == quote {
                    end += 1;
                    break;
                }
                end += 1;
            }
            push(&mut out, "tok-s", &rest[..end.min(rest.len())]);
            rest = &rest[end.min(rest.len())..];
            continue;
        }
        // Numbers (with `_` and `.` separators, duration units ride).
        if rest.starts_with(|c: char| c.is_ascii_digit()) {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
                .unwrap_or(rest.len());
            push(&mut out, "tok-n", &rest[..end]);
            rest = &rest[end..];
            continue;
        }
        // Words: `$variables` and keywords vs plain. The scan starts
        // AFTER the lead character so `find` can never return 0 —
        // a zero-length slice would spin forever.
        let lead_word = rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '$');
        if lead_word {
            let dollar = rest.starts_with('$');
            let scan_from = if dollar { 1 } else { 0 };
            let end = scan_from
                + rest[scan_from..]
                    .find(|c: char| {
                        !(c.is_ascii_alphanumeric() || c == '_' || (!dollar && c == ':'))
                    })
                    .unwrap_or(rest.len() - scan_from);
            let word = &rest[..end];
            let class = if dollar {
                "tok-v"
            } else {
                let bare = word.trim_end_matches(':');
                if KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(bare)) {
                    "tok-k"
                } else {
                    ""
                }
            };
            push(&mut out, class, word);
            rest = &rest[end..];
            continue;
        }
        // Everything else: one char, escaped, plain.
        let ch = rest.chars().next().expect("non-empty rest");
        push(&mut out, "", &rest[..ch.len_utf8()]);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// Shell blocks: dim comments (whole-line or trailing), leave the
/// rest plain.
pub fn sh(code: &str) -> String {
    let mut out = String::with_capacity(code.len() + 32);
    for (idx, line) in code.lines().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let starts_comment = line.trim_start().starts_with('#');
        // A trailing comment: `#` preceded by whitespace (a `#` inside
        // a word or path is left alone — cosmetic either way).
        let trailing = line.match_indices(" #").next().map(|(i, _)| i);
        match if starts_comment { Some(0) } else { trailing } {
            Some(at) => {
                push(&mut out, "", &line[..at]);
                push(&mut out, "tok-c", &line[at..]);
            }
            None => push(&mut out, "", line),
        }
    }
    out
}

fn push(out: &mut String, class: &str, text: &str) {
    let escaped = crate::md::escape(text);
    if class.is_empty() {
        out.push_str(&escaped);
    } else {
        out.push_str(&format!("<span class=\"{class}\">{escaped}</span>"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_keywords_strings_comments_and_escapes() {
        let html = sdl("DEFINE TABLE `user`;\n// note\nname: 'a<b';");
        assert!(html.contains("<span class=\"tok-k\">DEFINE</span>"));
        assert!(html.contains("<span class=\"tok-k\">TABLE</span>"));
        assert!(html.contains("<span class=\"tok-c\">// note</span>"));
        assert!(
            html.contains("<span class=\"tok-s\">&#39;a&lt;b&#39;</span>")
                || html.contains("a&lt;b")
        );
        assert!(html.contains("tok-s"));
        assert!(!html.contains("<b>")); // everything escaped
    }

    #[test]
    fn f_strings_and_variables() {
        let html = sdl("FROM f\"a{x}\" SET $p = 1");
        assert!(html.contains("tok-s"));
        assert!(html.contains("tok-v"));
        assert!(html.contains("tok-n"));
    }

    #[test]
    fn variables_terminate_and_highlight() {
        // A `$` used to spin forever: the word scan's `find` matched
        // the non-word lead character at offset 0 and never advanced.
        let html = sdl("assert: string::is_email($value);");
        assert!(html.contains("tok-v"));
        assert!(html.contains("tok-k"));
        assert!(html.contains("value"));
    }

    #[test]
    fn lone_dollar_terminates() {
        assert!(!sdl("$").is_empty());
    }

    #[test]
    fn sh_dims_comments_only() {
        let html = sh("surform build x.surdl # note\nsurform check x.surdl");
        assert!(html.contains("tok-c"));
        assert_eq!(html.matches('\n').count(), 1);
    }
}
