//! Surform in the browser: wasm bindings over the [`surform`]
//! compiler for the docs site's playground.
//!
//! The surface is deliberately small — text in, JSON out — so the JS
//! side stays a thin shim and the logic stays natively testable:
//!
//! - [`check`] — parse SDL text and, when it is clean, emit the DDL
//!   and the canonical SDL form, with the stage-2 parse oracle over
//!   the emission
//! - [`fmt`] — the canonical SDL form of a clean parse
//! - [`lint`] — the advisory findings over a clean parse
//! - [`version`] — the versions the playground's badge displays
//!
//! The embedded ENGINE is not part of this surface: exec and
//! verification stay native (the docs builder and the test suites).
//! Everything reachable from here is pure parsing and emission —
//! the `syn` family compiled to `wasm32-unknown-unknown` probe-clean
//! with no feature flags (2026-09-26).

use wasm_bindgen::prelude::wasm_bindgen;

mod report;

/// Parses SDL `text` and reports the compiler's verdict as one JSON
/// object:
///
/// ```text
/// { ok, ddl?, canonical?,
///   diagnostics: [{ code, severity, message, help?, label?,
///                   start? { line, col }, end? { line, col } }] }
/// ```
///
/// A dirty parse reports its diagnostics with `ok: false` and no
/// outputs; a clean one reports `ok: true` with both outputs and an
/// empty list. A clean parse additionally runs the stage-2 oracle —
/// the emitted DDL must parse through the embedded official parser
/// (the compiler's own output contract). That oracle can only fail
/// on a Surform bug; when it does, `ok` is false and the diagnostic
/// (`SDL-E1060`, embedded render quoted) rides while the outputs
/// stay present. Positions are 1-indexed, columns in Unicode chars —
/// the same semantics as miette's carets
/// ([`surform::lexer::line_col`]); JS consumers map them onto
/// CodeMirror's UTF-16 code units (an astral char before the error
/// desyncs the squiggle otherwise). `message` may be multi-line —
/// embedded-expression rejections carry the embedded render's cause
/// lines and snippet (for clause captures the render's `[line:col]`
/// marker and line number are file-anchored, while the quoted text
/// and caret stay capture-relative; slot-validity and stage-2
/// rejections ride their own texts' coordinates) — anchor squiggles
/// on `start`/`end`, never by parsing the message. Debounce is the
/// caller's job.
#[wasm_bindgen]
pub fn check(text: &str) -> String {
    report::check_json(text)
}

/// Formats SDL `text` into its canonical form (the `surform fmt`
/// pipeline): `{ ok, formatted?, diagnostics }`. A dirty parse
/// reports its diagnostics with `ok: false` and no `formatted`; a
/// clean parse carries the rewritten file.
#[wasm_bindgen]
pub fn fmt(text: &str) -> String {
    report::fmt_json(text)
}

/// Lints SDL `text` (the advisory pass, no `--info` merge): the same
/// diagnostics shape as [`check`], carrying the linter's findings —
/// `SDL-Lxxxx` codes at warning severity — with `ok: true`. A dirty
/// parse reports its parse errors with `ok: false` instead, since
/// lint needs a parsed file.
#[wasm_bindgen]
pub fn lint(text: &str) -> String {
    report::lint_json(text)
}

/// The versions the playground's badge displays: the bindings crate,
/// the compiler, and the targeted SurrealQL grammar generation.
#[wasm_bindgen]
pub fn version() -> String {
    format!(
        "surform-editor {} · surform {} · SurrealQL {}",
        env!("CARGO_PKG_VERSION"),
        surform::VERSION,
        surform::SURREALQL_VERSION,
    )
}
