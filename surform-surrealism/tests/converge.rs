//! The converge vectors: the guest's convergence script is
//! BYTE-IDENTICAL to the CLI's `surform migrate generate --info`
//! output on the same inputs.
//!
//! Strategy: the expected script is
//! PINNED — `tests/goldens/converge.surql` is the migration-script
//! section of the real `surform migrate generate` stdout at unit
//! time, run against the committed fixture pair (the desired state
//! `tests/fixtures/converge.surdl`, the live side
//! `tests/fixtures/converge-live-info.txt` — a REAL rendered
//! interchange, captured from the engine's `<string>(INFO FOR DB)`
//! cast plus per-table captures assembled under the reserved
//! `"@tables"` key, the docs' shell-loop recipe). Regenerate after a
//! deliberate comparator/renderer change:
//!
//! ```sh
//! cargo run --bin surform -- migrate generate \
//!   surform-surrealism/tests/fixtures/converge.surdl \
//!   --info db=s2demo/probe=surform-surrealism/tests/fixtures/converge-live-info.txt
//! # pin the `== migration script ==` section's bytes
//! ```
//!
//! The guest's `converge` runs the identical comparator through the
//! same path-depended core (read → ingest → diff → render), so a
//! passing vector is byte-identity; the root crate's migrate suite is
//! the per-commit tripwire for the comparator itself.

use std::fs;
use std::path::PathBuf;

use surform_surrealism::converge_sdl;

fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("file {} unreadable: {err}", path.display()))
}

/// The golden leg: the drifted capture against the desired state —
/// defines (param, table + field), removals (a field with a slot, a
/// table with the warning + slot), and three converged statements the
/// script must NOT carry (the table header, the field, the nested
/// index — the live side was captured with them present).
#[test]
fn the_script_is_byte_identical_to_the_cli_golden() {
    let sdl = repo_file("surform-surrealism/tests/fixtures/converge.surdl");
    let live_info = repo_file("surform-surrealism/tests/fixtures/converge-live-info.txt");
    let golden = repo_file("surform-surrealism/tests/goldens/converge.surql");
    let script =
        converge_sdl(&sdl, &live_info).unwrap_or_else(|err| panic!("unexpected rejection: {err}"));
    assert_eq!(script, golden, "script drifted from the CLI golden");
    // The converged statements ride NOWHERE — byte-identity above
    // covers it; these needles document the intent.
    assert!(!script.contains("email_idx"), "the converged index removes");
    assert!(
        !script.contains("DEFINE FIELD OVERWRITE email"),
        "the converged field redefines"
    );
}

/// The converged capture: the same SDL against a database that
/// already holds the desired state — the empty script (the CLI
/// prints `(converged)` for the same input; the module's empty
/// string IS that verdict).
#[test]
fn the_converged_state_returns_the_empty_script() {
    let sdl = repo_file("surform-surrealism/tests/fixtures/converge.surdl");
    let live_info = repo_file("surform-surrealism/tests/fixtures/converge-converged-info.txt");
    assert_eq!(
        converge_sdl(&sdl, &live_info).expect("the converged state converges"),
        "",
        "a converged database emits no script"
    );
}

/// The ledger drop (the revert precedent): the default ledger table
/// in the capture — both the `tables` section and the `@tables` child
/// map — changes NOTHING. The pipeline's own machinery is never
/// drafted for removal. (The CLI's `--info` mode has no such drop —
/// the module contract is the stronger one; this leg pins the
/// module's, byte-for-byte against the ledger-free golden.)
#[test]
fn the_ledger_table_is_never_converge_material() {
    let sdl = repo_file("surform-surrealism/tests/fixtures/converge.surdl");
    let live_info = repo_file("surform-surrealism/tests/fixtures/converge-live-info.txt");
    let ledger_table = "DEFINE TABLE _surform_migrations SCHEMAFULL";
    let with_ledger = live_info
        .replace(
            "tables: { legacy:",
            &format!("tables: {{ _surform_migrations: '{ledger_table}', legacy:"),
        )
        .replace(
            "\"@tables\": { legacy:",
            "\"@tables\": { _surform_migrations: { events: {  }, fields: {  }, \
             indexes: {  }, lives: {  }, tables: {  } }, legacy:",
        );
    assert!(
        with_ledger.contains("_surform_migrations"),
        "the mutation must land (the anchors track the fixture)"
    );
    let golden = repo_file("surform-surrealism/tests/goldens/converge.surql");
    let script = converge_sdl(&sdl, &with_ledger)
        .unwrap_or_else(|err| panic!("unexpected rejection: {err}"));
    assert_eq!(
        script, golden,
        "the ledger table's presence changed the script"
    );
}

/// Broken SDL rejects under the compile contract verbatim — converge
/// loads the SDL through the SAME pipeline, so the rejection is the
/// compile string (prefix + the CLI-theme render).
#[test]
fn broken_sdl_rejects_under_the_compile_contract() {
    let sdl = repo_file("tests/fixtures/broken.surdl");
    let live_info = repo_file("surform-surrealism/tests/fixtures/converge-live-info.txt");
    let err = converge_sdl(&sdl, &live_info).expect_err("broken SDL rejects");
    assert!(
        err.starts_with("SDL rejected: "),
        "the rejection lost the proven prefix: {err}"
    );
    let expected = repo_file("surform-surrealism/tests/goldens/broken.err");
    assert_eq!(err, expected, "the SDL rejection is the compile contract");
}

/// A malformed capture rejects with the live-INFO render — the same
/// handler and shape, under the `live INFO rejected` prefix naming
/// the failing carrier.
#[test]
fn a_malformed_capture_rejects_with_the_live_info_render() {
    let sdl = repo_file("surform-surrealism/tests/fixtures/converge.surdl");
    let err =
        converge_sdl(&sdl, "{ tables: {").expect_err("a truncated object is not an interchange");
    assert!(
        err.starts_with("live INFO rejected: 1 diagnostics\n"),
        "the rejection lost the INFO prefix: {err}"
    );
    assert!(
        !err.contains("schema.surdl") && err.contains("live-info.txt"),
        "the diagnostic cites the capture, not the SDL: {err}"
    );
    assert!(
        err.contains(",-[live-info.txt:"),
        "the render is the miette graphical: {err}"
    );
}

/// The empty capture rejects the same way (the walk wants the
/// object's opening brace).
#[test]
fn an_empty_capture_rejects() {
    let sdl = repo_file("surform-surrealism/tests/fixtures/converge.surdl");
    let err = converge_sdl(&sdl, "").expect_err("an empty capture is not an interchange");
    assert!(
        err.starts_with("live INFO rejected: 1 diagnostics\n"),
        "{err}"
    );
    assert!(
        err.contains("expected `{` opening the INFO object"),
        "{err}"
    );
}

/// The child-map shape error surfaces through ingestion: a `@tables`
/// child the `tables` section does not hold is the readers' E3002
/// (probed live — the CLI reports the same diagnostic for the same
/// file).
#[test]
fn an_orphan_child_map_entry_rejects_through_ingestion() {
    let sdl = repo_file("surform-surrealism/tests/fixtures/converge.surdl");
    let live_info = repo_file("surform-surrealism/tests/fixtures/converge-converged-info.txt");
    let orphan = live_info.replace(
        "\"@tables\": { audit:",
        "\"@tables\": { ghost: { events: {  }, fields: {  }, indexes: {  }, \
         lives: {  }, tables: {  } }, audit:",
    );
    assert!(orphan.contains("ghost"), "the mutation must land");
    let err = converge_sdl(&sdl, &orphan).expect_err("an orphan child has no table");
    assert!(
        err.starts_with("live INFO rejected: 1 diagnostics\n"),
        "{err}"
    );
    assert!(
        err.contains("the child map names `ghost`"),
        "the readers' own shape diagnostic rides: {err}"
    );
}

/// Scope resolution: the module has no --ns/--db flags — the SDL's
/// declaration IS the scope. Zero declared databases refuse (the
/// compare would silently make no claims); several refuse (one
/// database per call).
#[test]
fn scope_resolution_refuses_undeclared_and_ambiguous_databases() {
    let live_info = repo_file("surform-surrealism/tests/fixtures/converge-live-info.txt");
    let no_database = converge_sdl("namespace s2demo;", &live_info)
        .expect_err("an SDL without a database refuses");
    assert!(
        no_database.starts_with("the SDL never declares a database"),
        "{no_database}"
    );
    assert!(
        no_database.contains("converge compares the live database's rendered INFO"),
        "{no_database}"
    );
    let two_databases = converge_sdl(
        "namespace s2demo;\ndatabase probe;\ndatabase other;",
        &live_info,
    )
    .expect_err("an SDL with two databases refuses");
    assert!(
        two_databases.starts_with("the SDL declares 2 databases (s2demo/other, s2demo/probe)"),
        "{two_databases}"
    );
    assert!(
        two_databases.contains("one database per call"),
        "{two_databases}"
    );
}

/// The namespace-free pseudo-scope: a file with no `namespace`
/// statement but one `database` compares fine — the codegen no-USE
/// posture is the scope's own spelling.
#[test]
fn the_namespace_free_pseudo_scope_compares() {
    // The capture's tables section is scope-agnostic; a pseudo-scope
    // file claiming only `database probe;` (and nothing in it) sees
    // the whole capture as database-only drift — removals, no USE
    // line (the pseudo-scope emits none), and the removals' warning
    // blocks intact.
    let live_info = repo_file("surform-surrealism/tests/fixtures/converge-converged-info.txt");
    let script = converge_sdl("database probe;", &live_info)
        .unwrap_or_else(|err| panic!("the pseudo-scope refuses: {err}"));
    assert!(
        script.contains("REMOVE TABLE IF EXISTS `user`;"),
        "{script}"
    );
    assert!(script.contains("REMOVE TABLE IF EXISTS audit;"), "{script}");
    assert!(
        script.contains("REMOVE PARAM IF EXISTS $retention;"),
        "{script}"
    );
    assert!(script.contains("SURFORM WARNING"), "{script}");
    assert!(
        !script.contains("USE NS"),
        "the pseudo-scope emits no USE: {script}"
    );
}
