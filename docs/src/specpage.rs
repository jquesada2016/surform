//! Prepares the repo's `spec/*.md` for the site pipeline.
//!
//! Spec pages render through the same verifier as docs/content, but
//! their sources are written for the repository reader: they open with
//! an `#` h1 (the page shell renders its own) and link each other by
//! repo-relative paths (`table.md`, `README.md`). This
//! module rewrites those two shapes; the example fences pass through
//! untouched — they carry the shared fence vocabulary and are
//! verified exactly like the docs pages' own.

/// The statement pages' slugs are derived from these file stems.
const STATEMENTS: &[&str] = &[
    "access",
    "analyzer",
    "api",
    "bucket",
    "config",
    "database",
    "event",
    "field",
    "function",
    "include",
    "index",
    "mixin",
    "module",
    "namespace",
    "param",
    "sequence",
    "table",
    "type-alias",
    "user",
];

/// Drops the leading `#` title line and rewrites the spec's relative
/// links to their site targets. `repo_url` is the origin remote's
/// browse URL; links to files outside `spec/` become repository blob
/// links when it is known and pass through unchanged when it is not.
///
/// The scan is fence-aware and the fence rules mirror `md::parse`
/// exactly (open on any line starting with ``` outside a fence, close
/// on an exact ``` line): fenced example bytes must reach the
/// verifier and the published page exactly as the repo holds them —
/// a link-shaped comment inside an SDL example is content, not a
/// link.
pub fn prepare(body: &str, repo_url: &Option<String>) -> String {
    let body = drop_leading_title(body);
    let body = body.as_str();
    let mut out = String::with_capacity(body.len());
    let mut in_fence = false;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim();
        if !in_fence && trimmed.starts_with("```") {
            in_fence = true;
            out.push_str(line);
        } else if in_fence && trimmed == "```" {
            in_fence = false;
            out.push_str(line);
        } else if in_fence {
            out.push_str(line);
        } else {
            out.push_str(&rewrite_links(line, repo_url));
        }
    }
    out
}

fn rewrite_links(line: &str, repo_url: &Option<String>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find("](") {
        let target_start = open + 2;
        out.push_str(&rest[..target_start]);
        match rest[target_start..].find(')') {
            Some(end) => {
                let target = &rest[target_start..target_start + end];
                out.push_str(&rewrite_target(target, repo_url));
                out.push(')');
                rest = &rest[target_start + end + 1..];
            }
            None => {
                out.push_str(&rest[target_start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn rewrite_target(target: &str, repo_url: &Option<String>) -> String {
    if let Some(repo_path) = target.strip_prefix("../") {
        return match repo_url {
            Some(url) => format!("{url}/blob/main/{repo_path}"),
            None => target.to_string(),
        };
    }
    if target == "README.md" {
        return "spec.html".to_string();
    }
    if let Some(stem) = target.strip_suffix(".md")
        && STATEMENTS.contains(&stem)
    {
        return format!("spec-{stem}.html");
    }
    target.to_string()
}

fn drop_leading_title(body: &str) -> String {
    match body.split_once('\n') {
        Some((first, rest)) if first.starts_with("# ") => rest.to_string(),
        _ => body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_spec_links() {
        let url = Some("https://github.com/x/surform".to_string());
        let out = prepare(
            "# DEFINE TABLE\n\nSee [field.md](field.md) and [README.md](README.md); \
             start with [CONTRIBUTING.md](../CONTRIBUTING.md).\n\n\
             Unrelated (parenthetical) and [text](other.md) stay.\n",
            &url,
        );
        assert!(out.contains("[field.md](spec-field.html)"), "{out}");
        assert!(out.contains("[README.md](spec.html)"), "{out}");
        assert!(
            out.contains(
                "[CONTRIBUTING.md](https://github.com/x/surform/blob/main/CONTRIBUTING.md)"
            ),
            "{out}"
        );
        assert!(out.contains("[text](other.md)"), "{out}");
        assert!(!out.starts_with("# DEFINE TABLE"), "{out}");
    }

    #[test]
    fn fenced_bodies_pass_through_untouched() {
        // A link-shaped comment inside an example is content: the
        // published fence must stay byte-identical to the repo file
        // the per-commit test verifies.
        let url = Some("https://github.com/x/surform".to_string());
        let out = prepare(
            "# T\n\nprose [field.md](field.md) link\n\n```sdl\n// see [table.md](table.md)\ntable t;\n```\n",
            &url,
        );
        assert!(out.contains("prose [field.md](spec-field.html)"), "{out}");
        assert!(out.contains("// see [table.md](table.md)"), "{out}");
    }

    #[test]
    fn statements_table_matches_the_spec_pages() {
        let mut pages: Vec<&str> = crate::site::PAGES
            .iter()
            .filter_map(|p| p.slug.strip_prefix("spec-"))
            .collect();
        pages.sort_unstable();
        let mut statements = STATEMENTS.to_vec();
        statements.sort_unstable();
        assert_eq!(pages, statements);
    }

    #[test]
    fn outside_links_pass_without_a_remote() {
        let out = prepare("x [m](../CONTRIBUTING.md) y", &None);
        assert!(out.contains("(../CONTRIBUTING.md)"), "{out}");
    }
}
