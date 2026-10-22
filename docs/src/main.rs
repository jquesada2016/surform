//! `docgen` — builds the Surform documentation site.
//!
//! Every example fence in `docs/content/*.md` is verified against the
//! compiler and the embedded SurrealDB engine while the site builds
//! (see [`verify`]); any broken example is a build error, so the
//! published site can never drift from the tool. Output is a static
//! bundle (default `docs/_site/`) ready for GitHub Pages — see
//! docs/README.md.

mod hl;
mod md;
mod playground;
mod site;
mod specpage;
mod verify;

use site::SiteCtx;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command as Proc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    // Paths anchor on the repo root (this crate lives in docs/), so
    // docgen behaves the same from any working directory.
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the docs crate sits in the repo")
        .to_path_buf();
    let content_dir = base.join("docs/content");
    let out_dir = flag("--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| base.join("docs/_site"));

    let cli = flag("--cli").map(PathBuf::from).unwrap_or_else(|| {
        let mut path = base.join("target");
        path.push("debug");
        path.push("surform");
        path
    });

    let mut errors: Vec<verify::BuildError> = Vec::new();

    let cli_help = capture_cli_help(&cli, &mut errors);

    let commit = Proc::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&base)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let repo_url = git_remote(&base);

    // The committed bundle's hashed name comes from the build's own
    // manifest — checked (and failed loudly) before anything renders,
    // since every interactive page stamps it into its HTML.
    let editor_assets = base.join("docs/assets/editor");
    let bundle_script = playground::require_bundle(&editor_assets, &mut errors);

    let ctx = SiteCtx {
        cli_help,
        commit,
        repo_url,
        cli: cli.clone(),
        examples_dir: base.join("docs/examples"),
        editor_assets,
        keywords_island: playground::keywords_island(),
        bundle_script,
    };

    // The playground: not a markdown page — its article is built from
    // the verified example files and the compiler's keyword table.
    let playground_article = playground::build(&ctx, &mut errors);

    let rt = tokio::runtime::Runtime::new().expect("the tokio runtime starts");
    let mut built: Vec<(&'static site::PageSpec, site::PageHtml, String)> = Vec::new();
    let mut examples = 0usize;
    rt.block_on(async {
        for spec in site::PAGES {
            let path = content_dir.join(spec.file);
            let source = match std::fs::read_to_string(&path) {
                Ok(source) => source,
                Err(reason) => {
                    errors.push(verify::BuildError {
                        file: spec.file.to_string(),
                        line: 0,
                        message: format!("content file missing: {reason}"),
                    });
                    continue;
                }
            };
            let (_, body) = md::split_frontmatter(&source);
            // Spec pages (their `file` escapes docs/content) get the
            // repo-reader shapes rewritten for the site first; fences
            // pass through and verify like any other page's.
            let body = if spec.file.starts_with("../") {
                specpage::prepare(body.trim_start(), &ctx.repo_url)
            } else {
                body.to_string()
            };
            match site::render_page(spec, &body, &ctx).await {
                Ok(page) => {
                    examples += count_examples(&body);
                    built.push((spec, page, body));
                }
                Err(error) => errors.push(error),
            }
        }
    });

    // The site writes even with errors (partial output helps a broken
    // build's author), but the exit is nonzero: a drifted example
    // never ships.
    write_site(&out_dir, &ctx, &built, &playground_article);

    if !errors.is_empty() {
        eprintln!("\ndocgen: {} example error(s):", errors.len());
        for error in &errors {
            eprintln!("  {}:{} — {}", error.file, error.line, error.message);
        }
        std::process::exit(1);
    }
    println!(
        "docgen: {} pages, {examples} verified examples → {}",
        built.len(),
        out_dir.display()
    );
}

fn count_examples(body: &str) -> usize {
    md::parse(body)
        .iter()
        .filter(|b| {
            matches!(
                b,
                md::Block::Code { info, .. }
                    if matches!(verify::decode_fence(info), Ok(Some(_)))
            )
        })
        .count()
}

/// Captures `--help` for every CLI command the docs reference. These
/// panels are real program output, not hand-written prose.
fn capture_cli_help(
    cli: &PathBuf,
    errors: &mut Vec<verify::BuildError>,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for cmd in ["", "check", "build", "fmt", "normalize", "migrate", "lint"] {
        let mut invocation = Proc::new(cli);
        if !cmd.is_empty() {
            invocation.arg(cmd);
        }
        invocation.arg("--help");
        match invocation.output() {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                map.insert(cmd.to_string(), text.trim_end().to_string());
            }
            Ok(out) => errors.push(verify::BuildError {
                file: "cli-help".to_string(),
                line: 0,
                message: format!(
                    "`surform {cmd} --help` failed ({}): {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr)
                ),
            }),
            Err(reason) => errors.push(verify::BuildError {
                file: "cli-help".to_string(),
                line: 0,
                message: format!(
                    "cannot run the surform binary at {} (build it first: \
                     `cargo build --bin surform`): {reason}",
                    cli.display()
                ),
            }),
        }
    }
    map
}

fn git_remote(base: &PathBuf) -> Option<String> {
    Proc::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(base)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
            // git@github.com:owner/repo.git → the https browse URL.
            if let Some(rest) = url.strip_prefix("git@github.com:") {
                format!("https://github.com/{}", rest.trim_end_matches(".git"))
            } else {
                url.trim_end_matches(".git").to_string()
            }
        })
}

fn write_site(
    out_dir: &PathBuf,
    ctx: &SiteCtx,
    built: &[(&'static site::PageSpec, site::PageHtml, String)],
    playground_article: &str,
) {
    std::fs::create_dir_all(out_dir).expect("the output directory creates");
    std::fs::create_dir_all(out_dir.join("assets")).expect("assets/ creates");
    std::fs::write(out_dir.join(".nojekyll"), "").expect(".nojekyll writes");
    std::fs::write(
        out_dir.join("assets/style.css"),
        include_str!("../assets/style.css"),
    )
    .expect("style.css writes");
    std::fs::write(
        out_dir.join("assets/site.js"),
        include_str!("../assets/site.js"),
    )
    .expect("site.js writes");
    copy_editor_assets(ctx, out_dir);

    let mut index = Vec::new();
    for (spec, page, body) in built {
        let file_name = if spec.slug == "welcome" {
            "index.html".to_string()
        } else {
            format!("{}.html", spec.slug)
        };
        let html = site::shell(spec, page, ctx);
        std::fs::write(out_dir.join(&file_name), html).expect("page writes");
        index.push(site::search_entry(spec, body));
    }
    let search = format!("window.SURFORM_SEARCH=[{}];\n", index.join(",\n"));
    std::fs::write(out_dir.join("search-index.js"), search).expect("search index writes");

    // The playground rides the shell too — synthetic spec, this
    // module's article, no TOC.
    let page = site::PageHtml {
        article: playground_article.to_string(),
        toc: Vec::new(),
    };
    let html = site::shell(&playground::SPEC, &page, ctx);
    std::fs::write(out_dir.join(format!("{}.html", playground::SLUG)), html)
        .expect("the playground page writes");
}

/// Copies the committed playground bundle (docs/assets/editor/) into
/// the site. Presence was already checked by `playground::require_
/// bundle`; this walks whatever is there (the bindgen glue, the
/// wasm, the app bundle).
fn copy_editor_assets(ctx: &SiteCtx, out_dir: &std::path::Path) {
    let target = out_dir.join("assets/editor");
    std::fs::create_dir_all(&target).expect("assets/editor/ creates");
    for entry in std::fs::read_dir(&ctx.editor_assets).expect("the bundle dir reads") {
        let entry = entry.expect("the bundle entry reads");
        if entry.file_type().expect("the entry type reads").is_file() {
            std::fs::copy(entry.path(), target.join(entry.file_name()))
                .expect("the bundle file copies");
        }
    }
}
