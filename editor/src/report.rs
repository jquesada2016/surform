//! The wasm `check` report: the JSON model over the compiler's
//! diagnostics. Conversion is generic in the one direction that
//! matters — anything that is a `miette::Diagnostic` anchored on the
//! file's source converts (parse diagnostics, the stage-2 rejection,
//! and the linter's findings today).

use serde::Serialize;
use surform::lexer;
use surform::{Diag, codegen, lint, parser};

/// The one-call verdict: a dirty parse carries its diagnostics and no
/// outputs; a clean parse carries both outputs, empty unless the
/// stage-2 oracle rejected the emission (a Surform bug — then `ok`
/// is false and the rejection rides as a diagnostic).
#[derive(Serialize)]
struct CheckResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ddl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    canonical: Option<String>,
    diagnostics: Vec<DiagJson>,
}

/// The `fmt` verdict: the canonical form of a clean parse. Shaped like
/// [`CheckResult`] minus the DDL, so the JS side reuses its rendering.
#[derive(Serialize)]
struct FormatResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    formatted: Option<String>,
    diagnostics: Vec<DiagJson>,
}

/// The `lint` verdict: advisory findings over the authored file (the
/// CLI's no-`--info` path — the merge's resolved state is not
/// available in the browser). A dirty parse rides its parse errors
/// with `ok: false`; a clean parse is `ok: true` even when findings
/// exist — advisories are not errors.
#[derive(Serialize)]
struct LintResult {
    ok: bool,
    diagnostics: Vec<DiagJson>,
}

/// One diagnostic in the report shape. `help` and `label` are
/// optional because the compiler's diagnostics do not always carry
/// them; `start`/`end` are absent only for the file-level diagnostics
/// that have no span to anchor (none reachable from a `check` today —
/// even the stage-2 rejection anchors the file head, its offense
/// being the compiler's, not a position in this file).
#[derive(Serialize)]
struct DiagJson {
    code: String,
    severity: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<Pos>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end: Option<Pos>,
}

/// A 1-indexed (line, char-column) position — [`lexer::line_col`]'s
/// semantics, the ones miette's carets use.
#[derive(Serialize, Clone)]
struct Pos {
    line: usize,
    col: usize,
}

/// Runs the compiler over `text` and serializes the verdict — the
/// one JSON boundary the wasm export calls.
pub(crate) fn check_json(text: &str) -> String {
    serde_json::to_string(&check_result(text)).expect("serializing the check report cannot fail")
}

/// The check-class diagnostics ([`surform::order::check_document`])
/// over one parsed buffer, position-sorted — `None` when clean. The
/// browser has no loader, so this is the playground's whole gate (the
/// CLI's is [`surform::load`]).
fn check_class_diagnostics(ast: &surform::ast::SdlFile, text: &str) -> Option<Vec<DiagJson>> {
    let spans: Vec<lexer::Span> = surform::lint::scan_source(text)
        .statements
        .iter()
        .map(|s| s.span)
        .collect();
    let diags = surform::order::check_document(&ast.items, &[], &spans);
    if diags.is_empty() {
        return None;
    }
    let mut rendered: Vec<DiagJson> = diags
        .iter()
        .map(|(diag, _)| diag_json(diag, text))
        .collect();
    rendered.sort_by_key(|diag| {
        diag.start
            .as_ref()
            .map(|pos| (pos.line, pos.col))
            .unwrap_or((usize::MAX, 0))
    });
    Some(rendered)
}

/// Runs the compiler over `text`: parse, and on a clean parse, the
/// DDL emission, the canonical SDL form, and the stage-2 oracle —
/// the emitted DDL must parse through the embedded official parser
/// ([`codegen::validate_ddl`], the compiler's own output contract).
/// A stage-2 rejection is a Surform bug, never an authored-file
/// problem: the outputs still ride (they exist), `ok` is false, and
/// the diagnostic ([`Diag::EmbeddedDdlRejected`]) carries the
/// embedded render anchored at the file head.
fn check_result(text: &str) -> CheckResult {
    let (ast, diags) = parser::parse_file(text);
    if !diags.is_empty() {
        return CheckResult {
            ok: false,
            ddl: None,
            canonical: None,
            diagnostics: diags.iter().map(|diag| diag_json(diag, text)).collect(),
        };
    }
    // The check-class contract in one call (the declarative
    // namespace): a spread naming no mixin of the document is an
    // error, never a silent empty expansion, and so are a duplicate
    // definition (SDL-E1062) and a define-time cycle (SDL-E0015).
    if let Some(diagnostics) = check_class_diagnostics(&ast, text) {
        return CheckResult {
            ok: false,
            ddl: None,
            canonical: None,
            diagnostics,
        };
    }
    let ddl = codegen::emit(&ast);
    let mut diagnostics = Vec::new();
    let ok = match codegen::validate_ddl(&ddl) {
        Ok(()) => true,
        Err(message) => {
            diagnostics.push(diag_json(
                &Diag::EmbeddedDdlRejected {
                    span: lexer::Span::point(0),
                    message,
                },
                text,
            ));
            false
        }
    };
    CheckResult {
        ok,
        ddl: Some(ddl),
        canonical: Some(surform::info::write::write_canonical(&ast)),
        diagnostics,
    }
}

/// The JSON boundary the wasm `fmt` export calls: parse, and on a
/// clean parse, the canonical SDL form. No stage-2 oracle here — the
/// canonical form is SDL, not DDL, so there is nothing to re-parse.
pub(crate) fn fmt_json(text: &str) -> String {
    serde_json::to_string(&fmt_result(text)).expect("serializing the fmt report cannot fail")
}

fn fmt_result(text: &str) -> FormatResult {
    // The composable entry (spec/include.md): fmt round-trips
    // `include` statements without resolving them, exactly as the CLI
    // does — the playground has no file system behind it, but a
    // composition statement is still formattable text.
    let (ast, diags) = parser::parse_file_composable(text);
    if !diags.is_empty() {
        return FormatResult {
            ok: false,
            formatted: None,
            diagnostics: diags.iter().map(|diag| diag_json(diag, text)).collect(),
        };
    }
    FormatResult {
        ok: true,
        formatted: Some(surform::info::write::write_canonical(&ast)),
        diagnostics: Vec::new(),
    }
}

/// The JSON boundary the wasm `lint` export calls: the advisory
/// findings over the authored file. A dirty parse reports its parse
/// errors (`ok: false`); a clean parse reports the findings, if any
/// (`ok: true` — advisories are not errors).
pub(crate) fn lint_json(text: &str) -> String {
    serde_json::to_string(&lint_result(text)).expect("serializing the lint report cannot fail")
}

fn lint_result(text: &str) -> LintResult {
    let (ast, diags) = parser::parse_file(text);
    if !diags.is_empty() {
        return LintResult {
            ok: false,
            diagnostics: diags.iter().map(|diag| diag_json(diag, text)).collect(),
        };
    }
    // The CLI's lint is load-gated, and the load runs the whole
    // check-class contract — mirror it here ([`check_document`]) so a
    // typo'd spread, a duplicate definition, or a define-time cycle
    // reports its error instead of the advisory noise it would
    // otherwise trip.
    if let Some(diagnostics) = check_class_diagnostics(&ast, text) {
        return LintResult {
            ok: false,
            diagnostics,
        };
    }
    // The CLI's no-`--info` resolved view: the expanded form (a spread
    // member is a field the database would hold), with the authored
    // file kept for the authored-only findings.
    let resolved = surform::expand::expand(&ast);
    let findings = lint::lint(&ast, &resolved, text);
    LintResult {
        ok: true,
        diagnostics: findings.iter().map(|f| diag_json(f, text)).collect(),
    }
}

/// Converts one diagnostic anchored on `source` into the report
/// shape: the first label carries the span (the compiler's
/// diagnostics have exactly one primary label), byte offsets become
/// 1-indexed line/char columns, and a diagnostic without a miette
/// `severity` is an error (miette's own rendering default).
fn diag_json<D: miette::Diagnostic + ?Sized>(diag: &D, source: &str) -> DiagJson {
    let (label, start, end) = diag
        .labels()
        .and_then(|labels| labels.into_iter().next())
        .map(|span| {
            (
                span.label().map(str::to_string),
                Some(pos(source, span.offset())),
                Some(pos(source, span.offset() + span.len())),
            )
        })
        .unwrap_or((None, None, None));
    DiagJson {
        code: diag.code().map(|code| code.to_string()).unwrap_or_default(),
        severity: severity(diag.severity()),
        message: diag.to_string(),
        help: diag.help().map(|help| help.to_string()),
        label,
        start,
        end,
    }
}

fn severity(severity: Option<miette::Severity>) -> &'static str {
    match severity {
        Some(miette::Severity::Warning) => "warning",
        Some(miette::Severity::Advice) => "advice",
        Some(miette::Severity::Error) | None => "error",
    }
}

fn pos(source: &str, offset: usize) -> Pos {
    let (line, col) = lexer::line_col(source, offset);
    Pos { line, col }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The report the JS side parses for a clean file: both outputs
    /// present, no diagnostics, and the wire shape carries the keys
    /// the playground renders (`ok`, `ddl`, `canonical`,
    /// `diagnostics`).
    #[test]
    fn clean_parse_reports_both_outputs() {
        let value =
            serde_json::to_value(check_result("namespace app;")).expect("the report serializes");
        assert_eq!(value["ok"], serde_json::json!(true));
        assert!(
            value["ddl"]
                .as_str()
                .is_some_and(|ddl| ddl.contains("DEFINE NAMESPACE") && ddl.contains("app"))
        );
        assert!(
            value["canonical"]
                .as_str()
                .is_some_and(|sdl| sdl.contains("namespace app"))
        );
        assert_eq!(value["diagnostics"], serde_json::json!([]));
    }

    /// A dirty parse: `ok` is false, the outputs are ABSENT keys (not
    /// nulls — `skip_serializing_if`), and the diagnostic carries the
    /// full shape with its span anchored on the offense.
    #[test]
    fn dirty_parse_reports_diagnostics_only() {
        let value = serde_json::to_value(check_result("bogus;")).expect("the report serializes");
        assert_eq!(value["ok"], serde_json::json!(false));
        assert!(value.get("ddl").is_none());
        assert!(value.get("canonical").is_none());
        let diag = &value["diagnostics"][0];
        assert!(
            diag["code"]
                .as_str()
                .is_some_and(|code| code.starts_with("SDL-E"))
        );
        assert_eq!(diag["severity"], serde_json::json!("error"));
        assert!(diag["message"].as_str().is_some_and(|m| !m.is_empty()));
        assert_eq!(diag["start"], serde_json::json!({ "line": 1, "col": 1 }));
        assert_eq!(diag["end"]["line"], serde_json::json!(1));
    }

    /// Columns count chars, not bytes: the offense sits after a
    /// 2-byte char inside a quoted ident, so a byte-counting column
    /// would read one too far. The head diagnostic is the point-span
    /// `ExpectedStatementEnd` at the `@` — end == start, the caret
    /// under the label. (The trailing `;` yields a second,
    /// IllegalCharacter, diagnostic — recovery's own.)
    #[test]
    fn positions_count_chars_not_bytes() {
        let result = check_result("namespace `é` @;");
        assert_eq!(result.diagnostics.len(), 2);
        let diag = &result.diagnostics[0];
        assert_eq!(diag.code, "SDL-E1005");
        let start = diag.start.clone().expect("the diag anchors");
        let end = diag.end.clone().expect("the diag anchors");
        assert_eq!((start.line, start.col), (1, 15));
        assert_eq!((end.line, end.col), (1, 15));
    }

    /// The compiler's fix suggestions ride through as `help`, and the
    /// label text (the squiggle's own words) rides through as
    /// `label`.
    #[test]
    fn help_and_label_ride_through() {
        let result = check_result("bogus;");
        let diag = &result.diagnostics[0];
        assert!(diag.help.as_deref().is_some_and(|h| !h.is_empty()));
        assert!(diag.label.as_deref().is_some_and(|l| !l.is_empty()));
    }

    /// The stage-2 rejection rides the report shape: `SDL-E1060`, the
    /// embedded render inside the message, anchored at the file head
    /// (the offense is the compiler's, not a position in this file).
    /// The full `check` wiring's failure arm cannot be triggered
    /// without a compiler bug — that is the invariant the oracle
    /// guards — so the conversion is pinned directly on the diagnostic.
    #[test]
    fn stage2_rejection_converts() {
        let diag = Diag::EmbeddedDdlRejected {
            span: lexer::Span::point(0),
            message: String::from("something\n --> [1:5]\n  |\n1 | DEFINE BOGUS"),
        };
        let json = diag_json(&diag, "namespace app;");
        assert_eq!(json.code, "SDL-E1060");
        assert_eq!(json.severity, "error");
        assert!(json.message.contains("rejected Surform's own emitted DDL"));
        assert_eq!(json.label.as_deref(), Some("the compiled DDL"));
        assert!(
            json.help
                .as_deref()
                .is_some_and(|h| h.contains("Surform bug"))
        );
        let start = json.start.expect("the file head anchors");
        let end = json.end.expect("the file head anchors");
        assert_eq!((start.line, start.col), (1, 1));
        assert_eq!((end.line, end.col), (1, 1));
    }

    /// The `fmt` surface: a clean parse carries the canonical form; a
    /// dirty parse carries its diagnostics and no output.
    #[test]
    fn fmt_reports_canonical_form() {
        let clean =
            serde_json::to_value(fmt_result("namespace  app;")).expect("the report serializes");
        assert_eq!(clean["ok"], serde_json::json!(true));
        assert_eq!(clean["formatted"], serde_json::json!("namespace app;\n"));
        let dirty = serde_json::to_value(fmt_result("bogus;")).expect("the report serializes");
        assert_eq!(dirty["ok"], serde_json::json!(false));
        assert!(dirty.get("formatted").is_none());
        assert_eq!(
            dirty["diagnostics"][0]["severity"],
            serde_json::json!("error")
        );
    }

    /// The `lint` surface: findings ride at warning severity with
    /// their `SDL-L` codes; a clean file with no findings reports an
    /// empty list with `ok` true.
    #[test]
    fn lint_reports_findings_as_warnings() {
        let result =
            lint_result("namespace app;\nuser analyst {\n  on: database;\n  roles: viewer;\n};\n");
        assert!(result.ok, "the file parses, so lint ran");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "SDL-L0003" && d.severity == "warning"),
            "the secret-less user is flagged as a warning"
        );
        assert!(
            result.diagnostics.iter().all(|d| d.severity != "error"),
            "advisories are never errors"
        );
    }

    /// Lint on a dirty parse defers to the parse errors (`ok` false,
    /// `SDL-E` codes) — no findings without a parsed file.
    #[test]
    fn lint_reports_parse_errors_first() {
        let result = lint_result("bogus;");
        assert!(!result.ok);
        assert!(
            result.diagnostics[0].code.starts_with("SDL-E"),
            "parse errors ride instead of findings"
        );
    }

    /// The version string names all three components.
    #[test]
    fn version_names_components() {
        let version = crate::version();
        assert!(version.contains("surform-editor"));
        // The leading separator, not bare "surform" — the bindings
        // crate's own name prefixes the string.
        assert!(version.contains("· surform "));
        assert!(version.contains(surform::SURREALQL_VERSION));
    }
}
