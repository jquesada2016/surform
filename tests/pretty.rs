//! The expression layout pass over the clause menus (a sibling of the
//! per-commit pair oracle): every generated document's laid-out
//! emission must still parse through the embedded engine, both
//! writers' output must stay byte-stable through a reparse, and the
//! layout must genuinely fire — a corpus where every value fell back
//! to the flat canon would mean the pass silently died. (The width
//! budget itself is pinned by the exact-shape unit tests in
//! `src/pretty.rs`; corpus lines carry verbatim statement prints whose
//! length the pass does not control.)

mod common;

use common::{assert_oracle_accepts_files, menus};
use surform::codegen::emit;
use surform::info::write::write_canonical;
use surform::parser::parse_file;

/// Every menu family whose documents carry expression clauses. The
/// corpus is the shared one (tests/common/menus.rs) — the same
/// documents the pair oracle exercises.
fn corpus() -> Vec<String> {
    let mut all = Vec::new();
    for docs in [
        menus::field_docs(),
        menus::table_docs(),
        menus::access_docs(),
        menus::event_docs(),
        menus::function_docs(),
        menus::api_docs(),
        menus::index_docs(),
        menus::param_docs(),
        menus::user_docs(),
        menus::config_api_docs(),
        menus::config_default_docs(),
        menus::sequence_docs(),
        menus::bucket_docs(),
    ] {
        all.extend(docs);
    }
    all
}

#[test]
fn laid_out_emissions_parse_stay_stable_and_honor_the_budget() {
    let mut laid = 0;
    for sdl in corpus() {
        let (file, diags) = parse_file(&sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        let ddl = emit(&file);
        // A non-terminator line exists only where the layout broke a
        // value inside a statement (flat emissions end every line at
        // the statement's `;`).
        let is_laid = ddl
            .lines()
            .any(|line| !line.ends_with(';') && !line.trim().is_empty());
        if is_laid {
            laid += 1;
        }
        assert_oracle_accepts_files(&ddl);
        let sdl_once = write_canonical(&file);
        let (sdl_reparsed, diags) = parse_file(&sdl_once);
        assert!(diags.is_empty(), "canonical SDL must reparse: {sdl_once}");
        assert_eq!(
            write_canonical(&sdl_reparsed),
            sdl_once,
            "canonical SDL not byte-stable: {sdl_once}"
        );
    }
    assert!(
        laid > 20,
        "the layout barely fired: {laid} of {} documents",
        corpus().len()
    );
}
