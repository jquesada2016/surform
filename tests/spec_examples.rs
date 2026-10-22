//! The spec's own example fences, verified — the self-verifying spec
//! (the docs-site pipeline's per-commit twin).
//!
//! Convention (spec/README.md, "Verification workflow"): a runnable
//! example fence in `spec/*.md` carries an info string —
//!
//! - ```sdl — complete statement(s): must parse clean, emit DDL, and
//!   the emitted DDL must parse under the embedded SurrealQL parser
//!   (the stage-2 oracle, `codegen::validate_ddl`).
//! - ```sdl-body — a clause-level fragment (a table block's body):
//!   verified the same way after wrapping in a minimal table
//!   statement, `table doc schemafull { … };`.
//! - ```sdl-error expect=<codes> — a statement the strict pipeline
//!   must reject with EXACTLY those codes (the site verifier's Error
//!   contract, docs/src/verify.rs): the statement class runnable
//!   fences cannot carry (`include` — spec/include.md's file-context
//!   contract).
//!
//! Unlabeled fences are grammar sketches and carry no claim; `surql`
//! fences are expected-OUTPUT illustrations, `sh`/`rust` are shell and
//! AST sketches. The docs site renders the labeled fences through the
//! same pipeline (docgen's `sdl`/`sdl-body` kinds); this test is the
//! cheap per-commit layer that keeps the spec from rotting between
//! site builds. The `field.md` semicolon bug (2026-09-27) is the
//! class this catches.

use miette::Diagnostic as _;
use std::fmt::Write as _;
use std::path::PathBuf;

fn spec_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("spec")
}

/// The fenced code blocks of a markdown source: (1-based line of the
/// opening fence, info string, body).
fn fences(src: &str) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    let mut lines = src.lines().enumerate();
    while let Some((idx, line)) = lines.next() {
        let Some(info) = line.trim().strip_prefix("```") else {
            continue;
        };
        let mut body = Vec::new();
        for (_, inner) in lines.by_ref() {
            if inner.trim() == "```" {
                break;
            }
            body.push(inner);
        }
        out.push((idx + 1, info.trim().to_string(), body.join("\n")));
    }
    out
}

#[test]
fn sdl_body_wrapper_matches_the_site_verifier() {
    // The site's SdlBody path must wrap fragments with the exact
    // literal this test wraps them with — otherwise the site and this
    // test would verify different bytes. Pinned against the verifier's
    // own source.
    const VERIFIER: &str = include_str!("../docs/src/verify.rs");
    // Raw string: the pin matches the verifier's SOURCE bytes, where
    // `\n` is the two-character escape inside its format! literal.
    const WRAPPER: &str = r#"table doc schemafull {{\n{body}\n}};"#;
    assert!(
        VERIFIER.contains(WRAPPER),
        "docs/src/verify.rs no longer carries this test's sdl-body wrapper \
         literal ({WRAPPER:?}) — keep the two in sync"
    );
}

#[test]
fn spec_examples_parse_emit_and_validate() {
    let mut failures = String::new();
    let mut checked = 0usize;

    let mut files: Vec<PathBuf> = std::fs::read_dir(spec_dir())
        .expect("spec/ reads")
        .map(|entry| entry.expect("spec entry reads").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();

    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        let source = std::fs::read_to_string(file).expect("spec file reads");
        let mut labeled = 0usize;

        for (line, info, body) in fences(&source) {
            let head = info.split_whitespace().next().unwrap_or("");
            // `no-exec` and friends are site-render attributes; the
            // per-commit layer always checks parse + emit + oracle.
            let text = match head {
                "sdl" => body.clone(),
                // The wrapper must stay the literal the site's
                // verifier uses (docs/src/verify.rs, SdlBody) — the
                // per-commit layer and the site publish the same
                // claim about the same bytes.
                "sdl-body" => format!("table doc schemafull {{\n{body}\n}};"),
                // The strict pipeline must reject with EXACTLY the
                // expected codes — the site verifier's Error contract
                // (docs/src/verify.rs), kept in sync here.
                "sdl-error" => {
                    labeled += 1;
                    checked += 1;
                    let where_ = format!("{name}:{line}");
                    let Some(expect) = info
                        .split_whitespace()
                        .find_map(|a| a.strip_prefix("expect="))
                    else {
                        let _ = writeln!(failures, "{where_}: sdl-error without `expect=<codes>`");
                        continue;
                    };
                    let expect: Vec<String> =
                        expect.split(',').map(|c| c.trim().to_string()).collect();
                    let (_, diags) = surform::parser::parse_file(&body);
                    let codes: Vec<String> = diags
                        .iter()
                        .filter_map(|d| d.code().map(|c| c.to_string()))
                        .collect();
                    let mut mismatch = String::new();
                    for code in &expect {
                        if !codes.contains(code) {
                            mismatch.push_str(&format!("expected {code} but absent; "));
                        }
                    }
                    for code in &codes {
                        if !expect.contains(code) {
                            mismatch.push_str(&format!("unexpected {code}; "));
                        }
                    }
                    if !mismatch.is_empty() {
                        let _ = writeln!(failures, "{where_}: {mismatch}(codes: {codes:?})");
                    }
                    continue;
                }
                _ => continue,
            };
            labeled += 1;
            checked += 1;
            let where_ = format!("{name}:{line}");

            let (ast, diags) = surform::parser::parse_file(&text);
            if !diags.is_empty() {
                let _ = writeln!(
                    failures,
                    "{where_}: the example does not parse{}:\n{diags:#?}",
                    if head == "sdl-body" {
                        " (inside the table-block wrapper)"
                    } else {
                        ""
                    }
                );
                continue;
            }
            let ddl = surform::codegen::emit(&ast);
            if ddl.trim().is_empty() {
                let _ = writeln!(failures, "{where_}: codegen emitted nothing");
                continue;
            }
            if let Err(render) = surform::codegen::validate_ddl(&ddl) {
                let _ = writeln!(
                    failures,
                    "{where_}: the emitted DDL does not parse:\n{render}"
                );
            }
        }

        // Coverage: a file that teaches by example must carry at least
        // one labeled fence, or the convention has silently rotted.
        if source.contains("## Examples") && labeled == 0 {
            let _ = writeln!(
                failures,
                "{name}: has an Examples section but no ```sdl / ```sdl-body fence"
            );
        }
    }

    assert!(
        failures.is_empty(),
        "spec example fences failed verification ({checked} checked):\n{failures}"
    );
    // The corpus itself must stay substantial — a regression that
    // dropped labels wholesale would otherwise pass vacuously.
    assert!(checked >= 70, "suspiciously few labeled fences: {checked}");
}
