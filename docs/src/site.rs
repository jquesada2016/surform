//! Site assembly: the page table, the shared shell (sidebar, TOC,
//! search, footer), and the example-panel renderers that turn
//! verified outputs into HTML.

use crate::hl;
use crate::md::{self, Block};
use crate::verify::{self, ExampleKind, ExampleOutput};

/// The site's page table — the navigation AND the build order. A page
/// listed here must exist as `docs/content/<file>`.
pub struct PageSpec {
    pub slug: &'static str,
    pub file: &'static str,
    pub title: &'static str,
    pub group: &'static str,
}

macro_rules! pages {
    ($(($slug:literal, $file:literal, $title:literal, $group:literal)),+ $(,)?) => {
        pub const PAGES: &[PageSpec] = &[$(PageSpec { slug: $slug, file: $file, title: $title, group: $group }),+];
    };
}

pages![
    ("welcome", "welcome.md", "What is Surform?", "Start"),
    (
        "getting-started",
        "getting-started.md",
        "Getting started",
        "Start"
    ),
    ("basics", "basics.md", "How a schema file works", "Start"),
    ("tables", "tables.md", "Tables", "The language"),
    ("fields", "fields.md", "Fields", "The language"),
    ("mixins", "mixins.md", "Mixins", "The language"),
    (
        "includes",
        "includes.md",
        "Multi-file schemas (include)",
        "The language"
    ),
    ("types", "types.md", "Types (kinds)", "The language"),
    (
        "type-aliases",
        "type-aliases.md",
        "Type aliases",
        "The language"
    ),
    ("relations", "relations.md", "Relations", "The language"),
    ("indexes", "indexes.md", "Indexes", "The language"),
    ("events", "events.md", "Events", "The language"),
    ("functions", "functions.md", "Functions", "The language"),
    ("users", "users.md", "Users & roles", "The language"),
    (
        "access",
        "access.md",
        "Access (sign-in & tokens)",
        "The language"
    ),
    (
        "analyzers",
        "analyzers.md",
        "Fulltext indexes",
        "The language"
    ),
    ("params", "params.md", "Params & sequences", "The language"),
    (
        "config",
        "config.md",
        "Config (GraphQL & middleware)",
        "The language"
    ),
    ("buckets", "buckets.md", "Buckets (files)", "The language"),
    ("api", "api.md", "API routes", "The language"),
    (
        "modules",
        "modules.md",
        "Modules (Surrealism)",
        "The language"
    ),
    ("raw", "raw.md", "The raw escape hatch", "The language"),
    ("cli", "cli.md", "The surform CLI", "Tools"),
    ("fmt", "fmt.md", "Formatting", "Tools"),
    ("normalize", "normalize.md", "Adopting a database", "Tools"),
    ("migrate", "migrate.md", "Schema migration", "Tools"),
    (
        "in-database",
        "in-database.md",
        "Surform inside SurrealDB",
        "Tools"
    ),
    ("lint", "lint.md", "The linter", "Tools"),
    ("vscode", "vscode.md", "The VS Code extension", "Tools"),
    (
        "verification",
        "verification.md",
        "How examples are verified",
        "Behind the scenes"
    ),
    // The language spec itself, rendered from the repo's spec/ (the
    // source of truth) through the same pipeline — every labeled
    // example fence is verified, and complete `sdl` examples go live
    // in the editors. `file` escapes docs/content, so the edit link
    // and the content lookup both treat `../` pages specially.
    (
        "spec",
        "../../spec/README.md",
        "Spec — conventions & rules",
        "Language spec"
    ),
    (
        "spec-access",
        "../../spec/access.md",
        "Spec: DEFINE ACCESS",
        "Language spec"
    ),
    (
        "spec-analyzer",
        "../../spec/analyzer.md",
        "Spec: DEFINE ANALYZER",
        "Language spec"
    ),
    (
        "spec-api",
        "../../spec/api.md",
        "Spec: DEFINE API",
        "Language spec"
    ),
    (
        "spec-bucket",
        "../../spec/bucket.md",
        "Spec: DEFINE BUCKET",
        "Language spec"
    ),
    (
        "spec-config",
        "../../spec/config.md",
        "Spec: DEFINE CONFIG",
        "Language spec"
    ),
    (
        "spec-database",
        "../../spec/database.md",
        "Spec: DEFINE DATABASE",
        "Language spec"
    ),
    (
        "spec-event",
        "../../spec/event.md",
        "Spec: DEFINE EVENT",
        "Language spec"
    ),
    (
        "spec-field",
        "../../spec/field.md",
        "Spec: DEFINE FIELD",
        "Language spec"
    ),
    (
        "spec-function",
        "../../spec/function.md",
        "Spec: DEFINE FUNCTION",
        "Language spec"
    ),
    (
        "spec-include",
        "../../spec/include.md",
        "Spec: INCLUDE",
        "Language spec"
    ),
    (
        "spec-index",
        "../../spec/index.md",
        "Spec: DEFINE INDEX",
        "Language spec"
    ),
    (
        "spec-mixin",
        "../../spec/mixin.md",
        "Spec: MIXIN",
        "Language spec"
    ),
    (
        "spec-module",
        "../../spec/module.md",
        "Spec: DEFINE MODULE",
        "Language spec"
    ),
    (
        "spec-namespace",
        "../../spec/namespace.md",
        "Spec: DEFINE NAMESPACE",
        "Language spec"
    ),
    (
        "spec-param",
        "../../spec/param.md",
        "Spec: DEFINE PARAM",
        "Language spec"
    ),
    (
        "spec-sequence",
        "../../spec/sequence.md",
        "Spec: DEFINE SEQUENCE",
        "Language spec"
    ),
    (
        "spec-table",
        "../../spec/table.md",
        "Spec: DEFINE TABLE",
        "Language spec"
    ),
    (
        "spec-type-alias",
        "../../spec/type-alias.md",
        "Spec: TYPE (type aliases)",
        "Language spec"
    ),
    (
        "spec-user",
        "../../spec/user.md",
        "Spec: DEFINE USER",
        "Language spec"
    ),
];

/// Render-time context: the CLI help captures and the site meta.
pub struct SiteCtx {
    pub cli_help: std::collections::HashMap<String, String>,
    pub commit: String,
    pub repo_url: Option<String>,
    /// The surform binary (session fences run it) and the directory
    /// holding `docs/examples/*.surdl` (their inputs).
    pub cli: std::path::PathBuf,
    pub examples_dir: std::path::PathBuf,
    /// The committed playground bundle (docs/assets/editor/), copied
    /// into the site and required to exist (playground.rs).
    pub editor_assets: std::path::PathBuf,
    /// The compiler's keyword table as a JSON-array island body —
    /// stamped onto every page with interactive examples so the
    /// editors' highlighting cannot drift from the parser.
    pub keywords_island: String,
    /// The content-hashed bundle file name (from the build's
    /// bundle.txt) — stamped as the module script on interactive pages.
    pub bundle_script: String,
}

/// A rendered page: the article HTML plus its table-of-contents.
pub struct PageHtml {
    pub article: String,
    pub toc: Vec<(u8, String, String)>, // (level, anchor, text)
}

/// Renders one page's markdown to HTML, verifying every example fence
/// on the way. `Err` is a build error (bad example) — the caller
/// collects and reports.
pub async fn render_page(
    spec: &PageSpec,
    body: &str,
    ctx: &SiteCtx,
) -> Result<PageHtml, verify::BuildError> {
    let blocks = md::parse(body);
    let mut article = String::new();
    let mut toc = Vec::new();
    let mut seen_heads: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for block in &blocks {
        match block {
            Block::Heading { level, text, .. } => {
                if *level <= 3 {
                    let base = anchor(text);
                    let count = seen_heads.entry(base.clone()).or_insert(0);
                    *count += 1;
                    let anchor = if *count == 1 {
                        base
                    } else {
                        format!("{base}-{}", *count)
                    };
                    toc.push((*level, anchor.clone(), text.clone()));
                    article.push_str(&format!(
                        "<h{level} id=\"{anchor}\">{}</h{level}>\n",
                        md::inline_html(text)
                    ));
                } else {
                    article.push_str(&format!("<h4>{}</h4>\n", md::inline_html(text)));
                }
            }
            Block::Para { text } => {
                article.push_str(&format!("<p>{}</p>\n", md::inline_html(text)));
            }
            Block::List { ordered, items } => {
                let (open, close) = if *ordered { ("ol", "ol") } else { ("ul", "ul") };
                article.push_str(&format!("<{}>", open));
                for item in items {
                    article.push_str(&format!("<li>{}</li>\n", md::inline_html(item)));
                }
                article.push_str(&format!("</{}>\n", close));
            }
            Block::Table { head, rows } => {
                article.push_str("<div class=\"tablewrap\"><table><thead><tr>");
                for cell in head {
                    article.push_str(&format!("<th>{}</th>", md::inline_html(cell)));
                }
                article.push_str("</tr></thead><tbody>");
                for row in rows {
                    article.push_str("<tr>");
                    for cell in row {
                        article.push_str(&format!("<td>{}</td>", md::inline_html(cell)));
                    }
                    article.push_str("</tr>");
                }
                article.push_str("</tbody></table></div>\n");
            }
            Block::Quote { text } => {
                article.push_str(&format!(
                    "<blockquote><p>{}</p></blockquote>\n",
                    md::inline_html(text)
                ));
            }
            Block::Hr => article.push_str("<hr>\n"),
            Block::Code { info, body, line } => {
                article.push_str(&render_fence(info, body, *line, spec, ctx).await?);
            }
        }
    }
    // A page carrying interactive examples pulls the shared editor
    // bundle (the playground's own page ships it inside its article);
    // the keyword island keeps the editors' highlighting on the
    // compiler's table.
    if article.contains("data-interactive=") {
        article.push_str(&format!(
            "<script type=\"application/json\" id=\"surform-keywords\">{}</script>\n",
            ctx.keywords_island,
        ));
        article.push_str(&format!(
            "<script type=\"module\" src=\"assets/editor/{}\"></script>\n",
            ctx.bundle_script,
        ));
    }
    Ok(PageHtml { article, toc })
}

/// One fenced code block: a Surform example (verified), a captured CLI
/// help screen (real output), or a plain code display.
async fn render_fence(
    info: &str,
    body: &str,
    line: usize,
    spec: &PageSpec,
    ctx: &SiteCtx,
) -> Result<String, verify::BuildError> {
    match verify::decode_fence(info) {
        Err(why) => Err(verify::BuildError::new(spec.file, line, why)),
        Ok(Some(mut kind)) => {
            if let verify::ExampleKind::Session {
                cli, examples_dir, ..
            } = &mut kind
            {
                *cli = ctx.cli.clone();
                *examples_dir = ctx.examples_dir.clone();
            }
            // `src=NAME` fences: the example text lives in one file so
            // a terminal session can run the very same bytes.
            let mut owned = None;
            if let verify::ExampleKind::Sdl { src, .. } = &kind
                && let Some(name) = src
                && body.trim().is_empty()
            {
                let path = ctx.examples_dir.join(format!("{name}.surdl"));
                owned = Some(std::fs::read_to_string(&path).map_err(|reason| {
                    verify::BuildError::new(
                        spec.file,
                        line,
                        format!("the example file {} is missing: {reason}", path.display()),
                    )
                })?);
            }
            let text = owned.as_deref().unwrap_or(body);
            let out = verify::verify(&kind, text, spec.file, line).await?;
            Ok(example_panel(&kind, text, &out))
        }
        Ok(None) => {
            let mut words = info.split_whitespace();
            match words.next() {
                Some("cli-help") => {
                    for attr in words {
                        if let Some(cmd) = attr.strip_prefix("cmd=") {
                            return match ctx.cli_help.get(cmd) {
                                Some(text) => Ok(codebox(
                                    text,
                                    "plain",
                                    Some("captured output — the real thing, not prose"),
                                )),
                                None => Err(verify::BuildError::new(
                                    spec.file,
                                    line,
                                    format!("no captured help for `{cmd}` — is the CLI built?"),
                                )),
                            };
                        }
                    }
                    Err(verify::BuildError::new(
                        spec.file,
                        line,
                        "cli-help needs cmd=<command>".to_string(),
                    ))
                }
                Some(lang) => Ok(codebox(body, lang, None)),
                None => Ok(codebox(body, "plain", None)),
            }
        }
    }
}

/// An example card: the badge plus labelled code panels. Interactive
/// kinds (`sdl`, `sdl-error`, `sdl-lint`, `sdl-fmt`) are also stamped
/// for the browser: a `data-interactive` attribute, a toolbar (status
/// badge + Reset/Format, hidden until hydration), `data-pane` hooks on
/// the panels the editors drive, and a JSON island carrying the
/// verified original text (what Reset restores). The walkthrough kinds
/// (adopt/drift/session) stay static transcripts.
fn example_panel(kind: &ExampleKind, body: &str, out: &ExampleOutput) -> String {
    let badge = format!(
        "<span class=\"badge\"><svg width=\"11\" height=\"11\" viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path fill=\"currentColor\" d=\"M6.5 12.5 2 8l1.5-1.5 3 3 6-6L14 5z\"/></svg>{}</span>",
        md::escape(&out.badge)
    );
    let interactive = match kind {
        ExampleKind::Sdl { .. } => Some("sdl"),
        ExampleKind::Error { .. } => Some("error"),
        ExampleKind::Lint { .. } => Some("lint"),
        ExampleKind::Fmt => Some("fmt"),
        _ => None,
    };
    let mut html = match interactive {
        Some(kind_slug) => {
            format!(
                "<figure class=\"example\" data-interactive=\"{kind_slug}\">{badge}\
                 <div class=\"ex-toolbar\" hidden><span class=\"badge ex-status\" \
                 role=\"status\"></span><button class=\"ex-btn\" type=\"button\" \
                 data-act=\"reset\">Reset</button><button class=\"ex-btn\" type=\"button\" \
                 data-act=\"format\">Format</button></div>"
            )
        }
        None => format!("<figure class=\"example\">{badge}"),
    };
    match kind {
        ExampleKind::Sdl { show_ddl, .. } => {
            html.push_str(&pane("schema.surdl", body, "sdl", Some("editor")));
            if *show_ddl && let Some(ddl) = &out.ddl {
                html.push_str(&pane("surform build", ddl, "sql", Some("ddl")));
            }
            if let Some(canonical) = &out.canonical {
                html.push_str(&pane(
                    "the engine's own record of it (canonical form)",
                    canonical,
                    "sdl",
                    Some("canonical"),
                ));
            }
        }
        ExampleKind::Error { .. } => {
            html.push_str(&pane(
                "this schema is wrong on purpose",
                body,
                "sdl",
                Some("editor"),
            ));
            if let Some(diags) = &out.diags {
                html.push_str(&pane("what Surform reports", diags, "plain", Some("diags")));
            }
        }
        ExampleKind::Lint { .. } => {
            html.push_str(&pane("this file lints", body, "sdl", Some("editor")));
            if let Some(lint) = &out.lint {
                html.push_str(&pane("what Surform suggests", lint, "plain", Some("lint")));
            }
        }
        ExampleKind::Fmt => {
            html.push_str(&pane("as you wrote it", body, "sdl", Some("editor")));
            if let Some(fmt) = &out.fmt {
                html.push_str(&pane("surform fmt", fmt, "sdl", Some("fmt")));
            }
        }
        ExampleKind::SdlBody => {
            html.push_str(&panel(
                "a clause-level fragment — lives inside a table block",
                body,
                "sdl",
            ));
        }
        ExampleKind::Adopt { .. } => {
            html.push_str(&panel(
                "INFO FOR DB, as the engine prints it",
                body,
                "plain",
            ));
            if let Some(adopt) = &out.adopt {
                html.push_str(&panel("surform normalize → your schema file", adopt, "sdl"));
            }
        }
        ExampleKind::Drift { .. } => {
            html.push_str(&panel("your schema file", body, "sdl"));
            if let Some(drift) = &out.drift {
                html.push_str(&panel("someone ran this by hand", &drift.mutation, "sql"));
                html.push_str(&panel(
                    "surform migrate generate — the drift report",
                    &drift.report,
                    "plain",
                ));
                html.push_str(&panel(
                    "the convergence script (one transaction — SURFORM WARNING marks the data-destroying lines)",
                    &drift.script,
                    "sql",
                ));
            }
        }
        ExampleKind::Session { .. } => {
            if let Some((prompt, transcript)) = &out.session {
                html.push_str(&panel(prompt, transcript, "plain"));
            }
        }
    }
    if let Some(kind_slug) = interactive {
        html.push_str(&example_island(kind_slug, body));
    }
    html.push_str("</figure>\n");
    html
}

fn panel(label: &str, code: &str, lang: &str) -> String {
    pane(label, code, lang, None)
}

/// A labelled code panel; `pane` names the slot the example editors
/// drive (`editor`, `ddl`, `canonical`, `diags`, `lint`, `fmt`).
fn pane(label: &str, code: &str, lang: &str, pane: Option<&str>) -> String {
    let attr = pane
        .map(|p| format!(" data-pane=\"{p}\""))
        .unwrap_or_default();
    format!(
        "<div class=\"ex-block\"{attr}><div class=\"ex-label\">{}</div>{}</div>\n",
        md::escape(label),
        codebox(code, lang, None)
    )
}

/// The per-example JSON island: the verified fence text the editor
/// starts from and what Reset restores. `<` is escaped so a literal
/// can never close the script tag (same contract as the playground's
/// islands).
fn example_island(kind: &str, body: &str) -> String {
    format!(
        "<script type=\"application/json\" class=\"surform-example\">{{\"kind\":\"{kind}\",\"body\":{}}}</script>\n",
        crate::playground::island_json(body),
    )
}

/// A plain code box (the copy button is attached client-side).
pub fn codebox(code: &str, lang: &str, note: Option<&str>) -> String {
    let note = note
        .map(|n| format!("<span class=\"code-note\">{}</span>", md::escape(n)))
        .unwrap_or_default();
    let highlighted = match lang {
        "sdl" | "sql" => hl::sdl(code),
        "sh" => hl::sh(code),
        _ => md::escape(code),
    };
    format!(
        "<div class=\"codebox\"><span class=\"lang-chip\">{}</span>{note}<button class=\"copy\" type=\"button\" aria-label=\"Copy code\">Copy</button><pre><code>{highlighted}</code></pre></div>\n",
        md::escape(if lang == "plain" { "text" } else { lang })
    )
}

/// The full HTML document for a page. The playground (`docs/src/
/// playground.rs`) rides the same shell: its link injects at the head
/// of the "Start" group, and its synthetic spec carries no content
/// file (so no edit link).
pub fn shell(spec: &PageSpec, page: &PageHtml, ctx: &SiteCtx) -> String {
    let mut nav = String::from("<nav class=\"sidebar\" id=\"sidebar\">\n");
    let mut last_group = "";
    for p in PAGES {
        if p.group != last_group {
            if !last_group.is_empty() {
                nav.push_str("</div>\n");
            }
            nav.push_str(&format!(
                "<div class=\"nav-group\"><div class=\"nav-head\">{}</div>\n",
                md::escape(p.group)
            ));
            last_group = p.group;
            if p.group == crate::playground::SPEC.group {
                let active = if spec.slug == crate::playground::SPEC.slug {
                    " class=\"active\""
                } else {
                    ""
                };
                nav.push_str(&format!(
                    "<a href=\"{}.html\"{active}>{}</a>\n",
                    crate::playground::SPEC.slug,
                    md::escape(crate::playground::SPEC.title),
                ));
            }
        }
        let active = if p.slug == spec.slug {
            " class=\"active\""
        } else {
            ""
        };
        let href = if p.slug == "welcome" {
            "index.html"
        } else {
            &format!("{}.html", p.slug)
        };
        nav.push_str(&format!(
            "<a href=\"{href}\"{active}>{}</a>\n",
            md::escape(p.title)
        ));
    }
    nav.push_str("</div>\n</nav>\n");

    let mut toc_html = String::new();
    if !page.toc.is_empty() {
        toc_html.push_str("<aside class=\"toc\"><div class=\"toc-head\">On this page</div>\n");
        for (level, anchor, text) in &page.toc {
            let class = if *level == 2 { "toc-2" } else { "toc-3" };
            toc_html.push_str(&format!(
                "<a class=\"{class}\" href=\"#{anchor}\">{}</a>\n",
                md::escape(text)
            ));
        }
        toc_html.push_str("</aside>");
    }

    let mut breadcrumb = String::new();
    breadcrumb.push_str(&format!(
        "<span class=\"crumb-group\">{}</span><span class=\"crumb-title\">{}</span>",
        md::escape(spec.group),
        md::escape(spec.title)
    ));

    // The page title renders as the article's h1 (content starts at
    // h2); the breadcrumb above it is the group.
    let article_html = format!(
        "<h1 class=\"page-title\">{}</h1>\n{}",
        md::escape(spec.title),
        page.article
    );

    // Spec pages keep their content outside docs/content, so the edit
    // link derives from the file path: `../spec/x.md` is already
    // repo-relative once the leading `../` is stripped.
    let edit = match (&ctx.repo_url, spec.file.is_empty()) {
        (Some(url), false) => {
            let path = if spec.file.starts_with("../") {
                spec.file.trim_start_matches("../").to_string()
            } else {
                format!("docs/content/{}", spec.file)
            };
            format!("<a class=\"edit\" href=\"{url}/blob/main/{path}\">Edit this page</a>")
        }
        _ => String::new(),
    };

    let canonical = if spec.slug == "welcome" {
        "index.html"
    } else {
        &format!("{}.html", spec.slug)
    };
    // Plain placeholder replacement (not format!): the template's own
    // braces — inline JS — must survive untouched.
    let template = include_str!("../assets/page.template.html");
    let title = md::escape(spec.title);
    [
        ("{title}", title.as_str()),
        ("{description}", title.as_str()),
        ("{canonical}", canonical),
        ("{nav}", &nav),
        ("{breadcrumb}", &breadcrumb),
        ("{article}", &article_html),
        ("{toc}", &toc_html),
        ("{commit}", &ctx.commit),
        ("{version}", verify::ENGINE_VERSION),
        ("{edit}", &edit),
    ]
    .into_iter()
    .fold(template.to_string(), |acc, (key, value)| {
        acc.replace(key, value)
    })
}

/// The home page uses the same shell; callers pass the welcome spec.
/// The search index builder: one entry per page with stripped text.
pub fn search_entry(spec: &PageSpec, body: &str) -> String {
    let mut text = String::new();
    for block in md::parse(body) {
        match block {
            Block::Heading { text: piece, .. }
            | Block::Para { text: piece }
            | Block::Quote { text: piece } => {
                text.push_str(&strip_inline(&piece));
                text.push(' ');
            }
            Block::List { items, .. } => {
                for item in items {
                    text.push_str(&strip_inline(&item));
                    text.push(' ');
                }
            }
            Block::Table { head, rows } => {
                for cell in head.iter().chain(rows.iter().flatten()) {
                    text.push_str(&strip_inline(cell));
                    text.push(' ');
                }
            }
            _ => {}
        }
    }
    let url = if spec.slug == "welcome" {
        "index.html"
    } else {
        &format!("{}.html", spec.slug)
    };
    format!(
        "{{\"t\":{},\"u\":{},\"g\":{},\"b\":{}}}",
        json(&format!("{} — {}", spec.title, spec.group)),
        json(url),
        json(spec.group),
        json(&text)
    )
}

fn strip_inline(md_text: &str) -> String {
    md_text.replace(['`', '*', '[', ']'], "").replace('(', " (")
}

/// Minimal JSON string escaping (content is UTF-8 prose). Also the
/// escaper behind the playground's JSON islands.
pub(crate) fn json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn anchor(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut out = String::new();
    for c in lower.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if (c == ' ' || c == '-' || c == '/' || c == '(' || c == ')')
            && !out.ends_with('-')
            && !out.is_empty()
        {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_slugify() {
        assert_eq!(anchor("Quick start"), "quick-start");
        assert_eq!(anchor("Types (kinds)"), "types-kinds");
        assert_eq!(anchor("What is Surform?"), "what-is-surform");
    }

    #[test]
    fn json_escapes() {
        assert_eq!(json("a\"b\n"), "\"a\\\"b\\n\"");
    }

    #[test]
    fn page_table_groups_are_contiguous() {
        // The nav builder closes each group div on a group change;
        // the table must therefore keep groups contiguous.
        let mut seen = std::collections::HashSet::new();
        let mut current = "";
        for p in PAGES {
            if p.group != current {
                assert!(!seen.contains(p.group), "group {} re-opened", p.group);
                seen.insert(p.group);
                current = p.group;
            }
        }
    }
}
