//! The example verification pipeline.
//!
//! Every `sdl` fence in the docs sources is a claim about the tool —
//! so the site build executes the claim: parse the SDL (clean, or the
//! fence's expected diagnostics), emit DDL, apply it to a fresh
//! in-process SurrealDB engine, read the schema back, and hand the
//! real outputs to the renderer. A fence whose claim does not hold is
//! a BUILD ERROR (`BuildError`), so a drifted doc can never ship.
//!
//! SurrealDB access is embedded-only: the
//! engine runs in-process, never a server. The exec half mirrors
//! tests/common/exec.rs from the compiler's test suite.

use std::path::PathBuf;
use surform::ast::{Database, Namespace, SdlFile, Stmt};
use surform::info::diff::{self, DriftFinding};
use surform::info::normalize::{ReadModel, ScopeTag, merge};
use surform::info::read::{InfoObject, InfoValue, read_info_object};
use surform::info::report::report;
use surform::info::script::convergence_script;
use surform::info::write::write_canonical;
use surform::{codegen, parser};
use surrealdb::Surreal;
use surrealdb::engine::local::Db;

/// The engine version every example is verified against — surfaced in
/// the site's verified badges.
pub const ENGINE_VERSION: &str = "3.3.0";

/// One example fence, decoded from its markdown info string.
#[derive(Debug, Clone)]
pub enum ExampleKind {
    /// `sdl` — a valid example: parse, emit, (default) apply to the
    /// engine. Attrs: `no-exec` (the shape the embedded engine gates,
    /// e.g. Surrealism modules), `info` (add the canonical readback
    /// panel), `no-ddl` (hide the DDL panel — prose-focused fences),
    /// `src=NAME` (the fence body is empty; the example lives in
    /// `docs/examples/NAME.surdl` so terminal sessions can run the very
    /// same file).
    Sdl {
        exec: bool,
        info: bool,
        show_ddl: bool,
        src: Option<String>,
    },
    /// `sdl-error expect=SDL-E1044[,SDL-E1005]` — an invalid example;
    /// the parse must produce EXACTLY the expected diagnostic codes.
    Error { expect: Vec<String> },
    /// `sdl-lint expect=SDL-L0003` — a lintable example; the linter
    /// (file-only mode) must produce EXACTLY the expected codes.
    /// `expect=none` is the suppression flavor: the linter must
    /// produce NOTHING (an allow comment excusing every finding).
    Lint { expect: Vec<String> },
    /// `sdl-fmt` — a formatting example: show the authored text and
    /// the canonical form, asserting the fmt fixed point.
    Fmt,
    /// `sdl-body` — a clause-level fragment (a table block's body, as
    /// the spec's examples use). Verified like `sdl` after wrapping in
    /// a minimal table statement (`table doc schemafull { … };`),
    /// then rendered as a STATIC panel: the fragment's DDL depends on
    /// the host table its prose names, so there is nothing for a
    /// browser editor to check.
    SdlBody,
    /// `adopt ns=acme db=shop` — the fence body is rendered INFO text;
    /// shows `surform normalize` adopting it onto a stub file.
    Adopt { ns: String, db: String },
    /// `drift scenario=stale-table` — apply the fence's SDL, make the
    /// scenario's out-of-band change by hand, then diff.
    Drift { scenario: String },
    /// `session cmd=check src=shop exit=1` — run the REAL CLI binary
    /// against `docs/examples/<src>.surdl` and capture the transcript.
    /// The SDL must parse and emit clean unless `exit` is given; the
    /// panel shows what actually printed.
    Session {
        cmd: String,
        src: String,
        exit: Option<String>,
        cli: PathBuf,
        examples_dir: PathBuf,
    },
}

/// Decodes a fence's info string into an [`ExampleKind`]. `Ok(None)`
/// when the fence is not a Surform example at all (plain code
/// display); `Err` when it LOOKS like an example but is malformed —
/// a typo'd `sdl*` head, a typo'd attribute, or a missing required
/// attribute must fail the build, never silently downgrade the fence
/// to an unverified code box.
pub fn decode_fence(info: &str) -> Result<Option<ExampleKind>, String> {
    let mut words = info.split_whitespace();
    let bad = |what: &str| {
        format!(
            "malformed `{}` fence: {what}",
            info.split(' ').next().unwrap_or(info)
        )
    };
    match words.next() {
        // The `sdl*` family is fail-closed on the head too: a typo'd
        // `sdl-eror` must not slide through as a plain code box.
        Some(head)
            if head.starts_with("sdl")
                && !matches!(
                    head,
                    "sdl" | "sdl-error" | "sdl-lint" | "sdl-fmt" | "sdl-body"
                ) =>
        {
            Err(bad(
                "unknown example kind (did you mean sdl, sdl-error, sdl-lint, or sdl-fmt?)",
            ))
        }
        Some("sdl") => {
            let mut kind = ExampleKind::Sdl {
                exec: true,
                info: false,
                show_ddl: true,
                src: None,
            };
            for attr in words {
                match attr {
                    "no-exec" => {
                        if let ExampleKind::Sdl { exec, .. } = &mut kind {
                            *exec = false;
                        }
                    }
                    "info" => {
                        if let ExampleKind::Sdl { info, .. } = &mut kind {
                            *info = true;
                        }
                    }
                    "no-ddl" => {
                        if let ExampleKind::Sdl { show_ddl, .. } = &mut kind {
                            *show_ddl = false;
                        }
                    }
                    src if src.starts_with("src=") => {
                        if let ExampleKind::Sdl { src: slot, .. } = &mut kind {
                            *slot = Some(src[4..].to_string());
                        }
                    }
                    other => return Err(bad(&format!("unknown attribute `{other}`"))),
                }
            }
            Ok(Some(kind))
        }
        Some("sdl-error") => {
            let attrs: Vec<&str> = words.collect();
            let expect = parse_expect(attrs.iter().copied(), "expect=")
                .ok_or_else(|| bad("missing `expect=<codes>`"))?;
            if let Some(extra) = attrs.iter().find(|a| !a.starts_with("expect=")) {
                return Err(bad(&format!("unknown attribute `{extra}`")));
            }
            Ok(Some(ExampleKind::Error { expect }))
        }
        Some("sdl-lint") => {
            let attrs: Vec<&str> = words.collect();
            let expect = parse_expect(attrs.iter().copied(), "expect=")
                .ok_or_else(|| bad("missing `expect=<codes>`"))?;
            if let Some(extra) = attrs.iter().find(|a| !a.starts_with("expect=")) {
                return Err(bad(&format!("unknown attribute `{extra}`")));
            }
            Ok(Some(ExampleKind::Lint { expect }))
        }
        Some("sdl-fmt") => match words.next() {
            Some(extra) => Err(bad(&format!("unknown attribute `{extra}`"))),
            None => Ok(Some(ExampleKind::Fmt)),
        },
        Some("sdl-body") => match words.next() {
            Some(extra) => Err(bad(&format!("unknown attribute `{extra}`"))),
            None => Ok(Some(ExampleKind::SdlBody)),
        },
        Some("adopt") => {
            let mut ns = None;
            let mut db = None;
            for attr in words {
                if let Some(v) = attr.strip_prefix("ns=") {
                    ns = Some(v.to_string());
                } else if let Some(v) = attr.strip_prefix("db=") {
                    db = Some(v.to_string());
                } else {
                    return Err(bad(&format!("unknown attribute `{attr}`")));
                }
            }
            match (ns, db) {
                (Some(ns), Some(db)) => Ok(Some(ExampleKind::Adopt { ns, db })),
                _ => Err(bad("needs `ns=<name> db=<name>`")),
            }
        }
        Some("drift") => {
            let mut scenario = None;
            for attr in words {
                if let Some(v) = attr.strip_prefix("scenario=") {
                    scenario = Some(v.to_string());
                } else {
                    return Err(bad(&format!("unknown attribute `{attr}`")));
                }
            }
            Ok(Some(ExampleKind::Drift {
                scenario: scenario.ok_or_else(|| bad("missing `scenario=<name>`"))?,
            }))
        }
        Some("session") => {
            let mut cmd = None;
            let mut src = None;
            let mut exit = None;
            for attr in words {
                if let Some(v) = attr.strip_prefix("cmd=") {
                    cmd = Some(v.to_string());
                } else if let Some(v) = attr.strip_prefix("src=") {
                    src = Some(v.to_string());
                } else if let Some(v) = attr.strip_prefix("exit=") {
                    exit = Some(v.to_string());
                } else {
                    return Err(bad(&format!("unknown attribute `{attr}`")));
                }
            }
            Ok(Some(ExampleKind::Session {
                cmd: cmd.ok_or_else(|| bad("missing `cmd=<command>`"))?,
                src: src.ok_or_else(|| bad("missing `src=<name>`"))?,
                exit,
                cli: PathBuf::new(),
                examples_dir: PathBuf::new(),
            }))
        }
        _ => Ok(None),
    }
}

fn parse_expect<'a>(mut words: impl Iterator<Item = &'a str>, prefix: &str) -> Option<Vec<String>> {
    let raw = words.find_map(|a| a.strip_prefix(prefix))?;
    Some(raw.split(',').map(str::trim).map(String::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The fail-closed contract: a malformed example fence must be a
    // build error, never a silent downgrade to an unverified code box.
    #[test]
    fn malformed_fences_fail_decoding() {
        assert!(decode_fence("sdl-fmt bogus").is_err());
        assert!(decode_fence("sdl-eror expect=SDL-E1003").is_err());
        assert!(decode_fence("sdl no-exe").is_err());
        assert!(decode_fence("sdl wat").is_err());
        assert!(decode_fence("sdl-error").is_err());
        assert!(decode_fence("sdl-error expect=SDL-E1003 extra").is_err());
        assert!(decode_fence("adopt ns=acme").is_err());
        assert!(decode_fence("drift").is_err());
        assert!(decode_fence("session src=shop").is_err());
        assert!(decode_fence("sdl-body wrap=table").is_err());
    }

    #[test]
    fn non_examples_stay_plain() {
        assert!(decode_fence("sh").unwrap().is_none());
        assert!(decode_fence("text").unwrap().is_none());
        assert!(decode_fence("").unwrap().is_none());
    }

    #[test]
    fn well_formed_fences_decode() {
        assert!(matches!(
            decode_fence("sdl no-exec info"),
            Ok(Some(ExampleKind::Sdl {
                exec: false,
                info: true,
                ..
            }))
        ));
        assert!(decode_fence("sdl-error expect=SDL-E1044,SDL-E1003").is_ok());
        assert!(decode_fence("sdl-lint expect=SDL-L0003").is_ok());
        // The suppression flavor — decoded like any code list; the
        // VERIFY leg owns its empty-findings meaning (and rejects
        // `none` mixed with codes at verification time).
        assert!(matches!(
            decode_fence("sdl-lint expect=none"),
            Ok(Some(ExampleKind::Lint { expect })) if expect == ["none"]
        ));
        assert!(decode_fence("session cmd=check src=shop exit=1").is_ok());
        assert!(matches!(
            decode_fence("sdl-body"),
            Ok(Some(ExampleKind::SdlBody))
        ));
    }
}

/// A fence whose claim failed: the site build reports these and exits
/// nonzero, so the docs can never drift from the tool.
#[derive(Debug)]
pub struct BuildError {
    pub file: String,
    pub line: usize,
    pub message: String,
}

impl BuildError {
    pub fn new(file: &str, line: usize, message: impl Into<String>) -> Self {
        BuildError {
            file: file.to_string(),
            line,
            message: message.into(),
        }
    }
}

/// What verification produced for one fence — the renderer's inputs.
#[derive(Debug, Default)]
pub struct ExampleOutput {
    /// The SurrealQL DDL `surform build` emits.
    pub ddl: Option<String>,
    /// The canonical SDL the engine's own INFO readback produces.
    pub canonical: Option<String>,
    /// Rendered diagnostics (error fences).
    pub diags: Option<String>,
    /// Rendered lint findings (lint fences).
    pub lint: Option<String>,
    /// The canonical form (fmt fences).
    pub fmt: Option<String>,
    /// The adopted canonical SDL (adopt fences).
    pub adopt: Option<String>,
    /// Drift walkthrough (drift fences).
    pub drift: Option<DriftWalkthrough>,
    /// A real terminal transcript (session fences): the prompt line
    /// and the command's combined output.
    pub session: Option<(String, String)>,
    /// The verified badge's text.
    pub badge: String,
}

#[derive(Debug)]
pub struct DriftWalkthrough {
    /// The out-of-band SurrealQL somebody ran by hand.
    pub mutation: String,
    /// The `surform migrate generate` drift report.
    pub report: String,
    /// The convergence script (per-scope sections: defines +
    /// removals, one frame each, the USE outside).
    pub script: String,
}

/// The namespace/database names an example's SDL declares (the last
/// of each, matching the session the emitted USE lines land in).
fn declared_scope(file: &SdlFile) -> (Option<String>, Option<String>) {
    let mut ns = None;
    let mut db = None;
    for item in &file.items {
        match item {
            Stmt::Namespace(Namespace { name, .. }) => ns = Some(name.raw.clone()),
            Stmt::Database(Database { name, .. }) => db = Some(name.raw.clone()),
            _ => {}
        }
    }
    (ns, db)
}

/// The merge base: the authored file, plus synthesized
/// `namespace doc; database doc;` statements when it declares none —
/// the engine session the exec half applies in, and the scope the
/// readback merges under. Returns (base, ns, db, needs_use_prefix).
fn merge_base(file: &SdlFile) -> (SdlFile, String, String, bool) {
    let (ns, db) = declared_scope(file);
    match (ns, db) {
        (Some(ns), Some(db)) => (file.clone(), ns, db, false),
        (ns, db) => {
            let mut items = Vec::new();
            let ns_name = ns.unwrap_or_else(|| "doc".to_string());
            let db_name = db.unwrap_or_else(|| "doc".to_string());
            // Synthesize only the missing statement(s), preserving any
            // the file did declare (and their order relative to the
            // body — the synthesized ones go first, which is also the
            // only legal position for a namespace).
            let (ast_ns, ast_db) = declared_scope(file);
            if ast_ns.is_none() {
                items.push(Stmt::Namespace(Namespace {
                    name: surform::ast::Ident::new(ns_name.clone()),
                    keyword: Default::default(),
                    comment: None,
                    comments: Vec::new(),
                    clause_comments: Vec::new(),
                }));
            }
            if ast_db.is_none() {
                items.push(Stmt::Database(Database {
                    name: surform::ast::Ident::new(db_name.clone()),
                    keyword: Default::default(),
                    strict: false,
                    changefeed: None,
                    comment: None,
                    comments: Vec::new(),
                    trailing_comments: Vec::new(),
                    clause_comments: Vec::new(),
                }));
            }
            items.extend(file.items.iter().cloned());
            (
                SdlFile {
                    items,
                    trailing_comments: Vec::new(),
                },
                ns_name,
                db_name,
                ast_ns.is_none() || ast_db.is_none(),
            )
        }
    }
}

/// The full in-process apply: fresh engine, run the DDL batch, return
/// the complete read model — `INFO FOR DB` plus each table's
/// `INFO FOR TABLE` injected under the reserved `"@tables"` key (the
/// db interchange shape; the fields/indexes/events live only in the
/// per-table reports). The session for the INFO queries is set with
/// the SDK's own `use_ns`/`use_db` — an in-text `USE` binds inside
/// its query and does not carry across `query()` calls.
async fn apply_and_interchange(ddl: &str, ns: &str, db_name: &str) -> Result<InfoObject, String> {
    let db = mem_db().await;
    db.query(ddl)
        .await
        .map_err(|e| format!("the query does not run: {e}"))?
        .check()
        .map_err(|e| format!("the DDL does not execute: {e}"))?;
    db.use_ns(ns)
        .use_db(db_name)
        .await
        .map_err(|e| format!("the session does not bind: {e}"))?;
    db_interchange(&db).await
}

/// `INFO FOR DB` + per-table `INFO FOR TABLE`, assembled into the
/// interchange shape the readers ingest.
async fn db_interchange(db: &Surreal<Db>) -> Result<InfoObject, String> {
    let db_text = info_for(db, "INFO FOR DB").await.map_err(|e| e.message)?;
    let mut obj = read_info_object(&db_text).map_err(|d| format!("INFO does not parse: {d:?}"))?;
    let table_names: Vec<String> = match obj.0.get("tables") {
        Some(InfoValue::Object(t)) => t.0.keys().cloned().collect(),
        _ => Vec::new(),
    };
    let mut children = InfoObject(Default::default());
    for name in table_names {
        // Backticks are safe on any name, keyword or not.
        let text = info_for(db, &format!("INFO FOR TABLE `{name}`"))
            .await
            .map_err(|e| e.message)?;
        let child = read_info_object(&text)
            .map_err(|d| format!("INFO FOR TABLE {name} does not parse: {d:?}"))?;
        children.0.insert(name, InfoValue::Object(child));
    }
    if !children.0.is_empty() {
        obj.0
            .insert("@tables".to_string(), InfoValue::Object(children));
    }
    Ok(obj)
}

/// The in-process engine with every experimental capability enabled —
/// the same harness the compiler's exec suites use.
async fn mem_db() -> Surreal<Db> {
    use surrealdb::opt::capabilities::Capabilities;
    let config = surrealdb::opt::Config::default()
        .capabilities(Capabilities::default().with_all_experimental_features_allowed());
    Surreal::new::<surrealdb::engine::local::Mem>(config)
        .await
        .expect("the embedded engine starts")
}

/// The parse oracle: the embedded official SurrealQL parser with the
/// experimental capabilities enabled — the same DDL check the
/// compiler's own test suites run (module and bucket statements are
/// gated inside the parser, so the capabilities-on variant matches a
/// capability-enabled server's acceptance). One home: the compiler
/// lib's stage-2 wrapper (`codegen::validate_ddl`) — the wasm
/// bindings' `check` runs the same probe.
fn oracle_accepts(ddl: &str) -> Result<(), String> {
    surform::codegen::validate_ddl(ddl)
        .map_err(|render| format!("the emitted DDL does not parse: {render}"))
}

/// Renders diagnostics the way the CLI shows them (the plain theme —
/// no colors — so the docs match piped terminal output).
pub fn render_diags<D>(items: &[D], source: &str) -> String
where
    D: miette::Diagnostic + Clone + Send + Sync + 'static,
{
    let mut out = String::new();
    for item in items {
        let report = miette::Report::new(item.clone()).with_source_code(source.to_owned());
        miette::GraphicalReportHandler::new_themed(miette::GraphicalTheme::none())
            .with_links(false)
            .render_report(&mut out, report.as_ref())
            .expect("diagnostic rendering into a String cannot fail");
        out.push('\n');
    }
    out
}

/// Diagnostic codes of a parsed file's diagnostics (e.g. SDL-E1044).
fn diag_codes(diags: &[surform::Diag]) -> Vec<String> {
    diags.iter().filter_map(use_miette_code).collect()
}

fn use_miette_code(d: &surform::Diag) -> Option<String> {
    use miette::Diagnostic as _;
    d.code().map(|c| c.to_string())
}

/// Verifies one fence. `file`/`line` anchor build errors.
pub async fn verify(
    kind: &ExampleKind,
    body: &str,
    file: &str,
    line: usize,
) -> Result<ExampleOutput, BuildError> {
    let err = |m: String| BuildError::new(file, line, m);
    let mut out = ExampleOutput::default();
    match kind {
        ExampleKind::Sdl {
            exec,
            info,
            show_ddl,
            ..
        } => {
            let (ast, diags) = parser::parse_file(body);
            if !diags.is_empty() {
                return Err(err(format!(
                    "the example does not parse:\n{}",
                    render_diags(&diags, body)
                )));
            }
            // The check-class contract (the declarative namespace): a
            // spread naming no mixin of the document is an error, so a
            // fence must not silently emit without its members.
            let unresolved = parser::resolve_spreads(&ast);
            if !unresolved.is_empty() {
                return Err(err(format!(
                    "the example does not resolve:\n{}",
                    render_diags(&unresolved, body)
                )));
            }
            let ddl = codegen::emit(&ast);
            if ddl.trim().is_empty() {
                return Err(err("codegen emitted nothing".to_string()));
            }
            if *show_ddl {
                out.ddl = Some(ddl.clone());
            }
            let (base, ns, db, needs_use) = merge_base(&ast);
            if *exec {
                let prefix = if needs_use {
                    format!("USE NS {ns} DB {db};\n")
                } else {
                    String::new()
                };
                let interchange = apply_and_interchange(&format!("{prefix}{ddl}"), &ns, &db)
                    .await
                    .map_err(&err)?;
                out.badge = format!("applied to SurrealDB {ENGINE_VERSION}");
                if *info {
                    let mut model = ReadModel::default();
                    model
                        .push_scope(ScopeTag::Database(ns.clone(), db.clone()), &interchange)
                        .map_err(|ds| err(format!("the INFO readback does not ingest: {ds:?}")))?;
                    let merged = merge(&base, &model);
                    out.canonical = Some(write_canonical(&merged));
                }
            } else {
                oracle_accepts(&ddl).map_err(&err)?;
                out.badge = format!(
                    "compiles clean (engine-gated shape; SurrealDB {ENGINE_VERSION} parses the DDL)"
                );
            }
        }
        ExampleKind::Error { expect } => {
            let (ast, diags) = parser::parse_file(body);
            // The E1061 class reports at the resolution pass now (the
            // declarative namespace) — an error fence demonstrates it
            // through the same pass, so a clean parse still resolves.
            let diags = if diags.is_empty() {
                parser::resolve_spreads(&ast)
            } else {
                diags
            };
            if diags.is_empty() {
                return Err(err(
                    "expected diagnostics, but the example parses clean".to_string()
                ));
            }
            let codes = diag_codes(&diags);
            for code in expect {
                if !codes.contains(code) {
                    return Err(err(format!(
                        "expected diagnostic {code} but the example reports {codes:?}"
                    )));
                }
            }
            for code in &codes {
                if !expect.contains(code) {
                    return Err(err(format!(
                        "unexpected diagnostic {code} (expected exactly {expect:?})"
                    )));
                }
            }
            out.diags = Some(render_diags(&diags, body));
            out.badge = "real Surform diagnostics".to_string();
        }
        ExampleKind::Lint { expect } => {
            let (ast, diags) = parser::parse_file(body);
            if !diags.is_empty() {
                return Err(err(format!(
                    "the example does not parse:\n{}",
                    render_diags(&diags, body)
                )));
            }
            let findings = surform::lint::lint(&ast, &ast, body);
            let codes: Vec<String> = findings
                .iter()
                .filter_map(|f| {
                    use miette::Diagnostic as _;
                    f.code().map(|c| c.to_string())
                })
                .collect();
            if expect.first().map(String::as_str) == Some("none") {
                if expect.len() > 1 {
                    return Err(err("`expect=none` names no codes".to_string()));
                }
                if !findings.is_empty() {
                    return Err(err(format!(
                        "expected NO lint findings, the example reports {codes:?}"
                    )));
                }
                out.badge = "verified: the linter reports nothing".to_string();
                return Ok(out);
            }
            for code in expect {
                if !codes.contains(code) {
                    return Err(err(format!(
                        "expected lint {code} but the example reports {codes:?}"
                    )));
                }
            }
            for code in &codes {
                if !expect.contains(code) {
                    return Err(err(format!(
                        "unexpected lint {code} (expected exactly {expect:?})"
                    )));
                }
            }
            if findings.is_empty() {
                return Err(err("expected lint findings, got none".to_string()));
            }
            out.lint = Some(render_diags(&findings, body));
            out.badge = "real Surform lint findings".to_string();
        }
        ExampleKind::Fmt => {
            // The composable entry (spec/include.md): fmt fences may
            // carry `include` statements — fmt round-trips them
            // without resolving, exactly as the CLI does.
            let (ast, diags) = parser::parse_file_composable(body);
            if !diags.is_empty() {
                return Err(err(format!(
                    "the example does not parse:\n{}",
                    render_diags(&diags, body)
                )));
            }
            let canonical = write_canonical(&ast);
            // The fmt fixed point: re-parse the canonical text, require
            // a clean parse and a stable second print.
            let (ast2, diags2) = parser::parse_file_composable(&canonical);
            if !diags2.is_empty() {
                return Err(err(format!(
                    "the canonical form does not re-parse:\n{}",
                    render_diags(&diags2, &canonical)
                )));
            }
            if write_canonical(&ast2) != canonical {
                return Err(err("fmt is not idempotent on this example".to_string()));
            }
            out.fmt = Some(canonical);
            out.badge = "canonical form verified (re-parses, stable)".to_string();
        }
        ExampleKind::SdlBody => {
            // The wrapper must stay the literal the per-commit spec
            // test uses (tests/spec_examples.rs) — the site and the
            // test publish the same claim about the same bytes.
            let wrapped = format!("table doc schemafull {{\n{body}\n}};");
            let (ast, diags) = parser::parse_file(&wrapped);
            if !diags.is_empty() {
                return Err(err(format!(
                    "the fragment does not parse inside the table-block wrapper:\n{}",
                    render_diags(&diags, &wrapped)
                )));
            }
            let ddl = codegen::emit(&ast);
            if ddl.trim().is_empty() {
                return Err(err("codegen emitted nothing".to_string()));
            }
            oracle_accepts(&ddl).map_err(&err)?;
            out.badge =
                "compiles clean inside a table block (SurrealDB 3.3 parses the DDL)".to_string();
        }
        ExampleKind::Adopt { ns, db } => {
            // The fence body is rendered INFO text; adopt it onto a
            // stub file exactly like `surform normalize` does.
            let read = read_info_object(body)
                .map_err(|d| err(format!("the INFO text does not parse: {d:?}")))?;
            let stub_src = format!("namespace {ns};\ndatabase {db};\n");
            let (stub, diags) = parser::parse_file(&stub_src);
            if !diags.is_empty() {
                return Err(err("the synthesized stub does not parse".to_string()));
            }
            let mut model = ReadModel::default();
            model
                .push_scope(ScopeTag::Database(ns.clone(), db.clone()), &read)
                .map_err(|ds| err(format!("the INFO text does not ingest: {ds:?}")))?;
            out.adopt = Some(write_canonical(&merge(&stub, &model)));
            out.badge = "adoption verified through the normalize pipeline (read → merge → write)"
                .to_string();
        }
        ExampleKind::Drift { scenario } => {
            let (ast, diags) = parser::parse_file(body);
            if !diags.is_empty() {
                return Err(err(format!(
                    "the example does not parse:\n{}",
                    render_diags(&diags, body)
                )));
            }
            let ddl = codegen::emit(&ast);
            let (base, ns, db, needs_use) = merge_base(&ast);
            let prefix = if needs_use {
                format!("USE NS {ns} DB {db};\n")
            } else {
                String::new()
            };
            let mutation = match scenario.as_str() {
                "stale-table" => "DEFINE TABLE archived_orders SCHEMALESS;".to_string(),
                "changed-kind" => {
                    "DEFINE FIELD OVERWRITE name ON `user` TYPE option<string>;".to_string()
                }
                // The walkthrough (docs/content/mixins.md): an
                // out-of-band default on a SPREAD member — the finding
                // is the member's own drift, never a mixin-level one
                // (the differ expands the SDL side itself).
                "member-drift" => "DEFINE FIELD OVERWRITE created_at ON `post` TYPE datetime \
                     DEFAULT time::now();"
                    .to_string(),
                other => return Err(err(format!("unknown drift scenario `{other}`"))),
            };
            // Apply, then diff BEFORE the mutation: a clean baseline is
            // what makes the drift findings meaningful (and pins the
            // differ's convergence on this example).
            let db_handle = mem_db().await;
            db_handle
                .query(format!("{prefix}{ddl}"))
                .await
                .map_err(|e| err(format!("the query does not run: {e}")))?
                .check()
                .map_err(|e| err(format!("the DDL does not execute: {e}")))?;
            db_handle
                .use_ns(&ns)
                .use_db(&db)
                .await
                .map_err(|e| err(format!("the session does not bind: {e}")))?;
            let baseline_info = db_interchange(&db_handle).await.map_err(&err)?;
            let baseline = drift_findings(&base, &ns, &db, &baseline_info)?;
            if !report(&baseline).is_empty() {
                return Err(err(format!(
                    "the example's baseline state must converge before the \
                     hand-made drift, but it reports:\n{}",
                    report(&baseline)
                )));
            }
            // The out-of-band change, then the real diff.
            db_handle
                .query(mutation.as_str())
                .await
                .map_err(|e| err(format!("the drift mutation does not run: {e}")))?
                .check()
                .map_err(|e| err(format!("the drift mutation does not execute: {e}")))?;
            let info_text = db_interchange(&db_handle).await.map_err(&err)?;
            let findings = drift_findings(&base, &ns, &db, &info_text)?;
            let script = convergence_script(&findings, &base);
            out.drift = Some(DriftWalkthrough {
                mutation,
                report: report(&findings),
                script,
            });
            if report(&findings).is_empty() {
                return Err(err(
                    "the drift scenario produced no findings — the mutation did not register"
                        .to_string(),
                ));
            }
            out.badge = format!("live drift against SurrealDB {ENGINE_VERSION}");
        }
        ExampleKind::Session {
            cmd,
            src,
            exit,
            cli,
            examples_dir,
        } => {
            let example_path = examples_dir.join(format!("{src}.surdl"));
            let source = std::fs::read_to_string(&example_path).map_err(|reason| {
                err(format!(
                    "the session's example file {} is missing: {reason}",
                    example_path.display()
                ))
            })?;
            if exit.is_none() {
                // A success-session's file must be clean SDL (a
                // failure-session — `exit=` given — may show anything;
                // the captured transcript is the point).
                let (ast, diags) = parser::parse_file(&source);
                if !diags.is_empty() {
                    return Err(err(format!(
                        "the session's example file does not parse:\n{}",
                        render_diags(&diags, &source)
                    )));
                }
                let _ = ast;
            }
            // Run the real binary against a temp copy (the CLI reads a
            // path; the docs' copy stays untouched).
            let temp = std::env::temp_dir().join(format!("surform-docs-{src}.surdl"));
            std::fs::write(&temp, &source)
                .map_err(|e| err(format!("the temp copy does not write: {e}")))?;
            let ran = std::process::Command::new(cli)
                .arg(cmd)
                .arg(&temp)
                .output()
                .map_err(|e| err(format!("the CLI does not run: {e}")))?;
            // The temp copy did its job; don't leave it behind (on any
            // path — the transcript checks below may fail).
            let _ = std::fs::remove_file(&temp);
            let code = ran.status.code().unwrap_or(-1).to_string();
            if let Some(expect) = exit {
                if &code != expect {
                    return Err(err(format!(
                        "expected exit {expect}, the CLI exited {code}"
                    )));
                }
            } else if !ran.status.success() {
                return Err(err(format!(
                    "the CLI exited {code}:\n{}",
                    String::from_utf8_lossy(&ran.stderr)
                )));
            }
            let mut transcript = String::from_utf8_lossy(&ran.stdout).to_string();
            let stderr = String::from_utf8_lossy(&ran.stderr);
            if !stderr.is_empty() {
                if !transcript.is_empty() {
                    transcript.push('\n');
                }
                transcript.push_str(&stderr);
            }
            out.session = Some((format!("$ surform {cmd} {src}.surdl"), transcript));
            out.badge = "captured from the real binary".to_string();
        }
    }
    Ok(out)
}

async fn info_for(db: &Surreal<Db>, query: &str) -> Result<String, BuildError> {
    let info: surrealdb::types::Value = db
        .query(query)
        .await
        .map_err(|e| BuildError::new("", 0, format!("{query} does not run: {e}")))?
        .take(0)
        .map_err(|e| BuildError::new("", 0, format!("{query} does not take: {e}")))?;
    use surrealdb::types::ToSql as _;
    Ok(info.to_sql())
}

fn drift_findings(
    base: &SdlFile,
    ns: &str,
    db: &str,
    interchange: &InfoObject,
) -> Result<Vec<DriftFinding>, BuildError> {
    let mut model = ReadModel::default();
    model
        .push_scope(
            ScopeTag::Database(ns.to_string(), db.to_string()),
            interchange,
        )
        .map_err(|ds| {
            BuildError::new("", 0, format!("the INFO readback does not ingest: {ds:?}"))
        })?;
    Ok(diff::diff(base, &model))
}
