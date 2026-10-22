//! The apply machinery's native legs: every pure piece the
//! wasm `apply` flow composes, fed mock carriers — the committed CLI
//! golden script, hand-written rendered captures, the fixture pair —
//! no database involved. The live legs (the real dispatches) run
//! against a throwaway database; the host-call
//! orchestration itself is wasm-only (the SDK's imports are component
//! bindings), so these legs pin everything AROUND the host calls:
//! the exact statements the flow would execute, in order, and every
//! refusal it can produce before and between them.
//!
//! The statements come from the GENERATOR, not a splitter: the
//! convergence emitter captures each statement
//! at emission (`converge_parts` → `ConvergenceScript`), and the flow
//! executes one host call per marked statement. The lexical
//! re-splitter this suite once pinned was retired; the capture
//! ENGINE's own contract now lives with its remaining consumer in the
//! core lexer's tests.

use std::fs;
use std::path::PathBuf;

use surform::info::read::read_info_object;

fn guest_file(path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("{} unreadable: {err}", path.display()))
}

/// The committed convergence golden — the real `migrate generate
/// --info` stdout section for the fixture pair (the converge suite pins its
/// bytes; this suite pins the structured carrier beside it).
fn golden_script() -> String {
    guest_file("../surform-surrealism/tests/goldens/converge.surql")
}

/// The full-fidelity fixture capture — a REAL rendered interchange
/// with the per-table children under `"@tables"` (the converge fixture).
fn fixture_capture() -> String {
    guest_file("../surform-surrealism/tests/fixtures/converge-live-info.txt")
}

/// A bare db-level capture — the fixture's surviving table at header
/// level, no `@tables` children, the shape a caller gets from one
/// `RETURN <string>(INFO FOR DB);`. The SDL's `audit` table is
/// missing and the `legacy` table (the capture-less removal) is not
/// in the capture, so the convergence is the additive class.
fn bare_capture() -> String {
    "{ accesses: {  }, analyzers: {  }, apis: {  }, buckets: {  }, \
     configs: {  }, functions: {  }, models: {  }, modules: {  }, \
     params: {  }, sequences: {  }, tables: { user: 'DEFINE TABLE user \
     TYPE NORMAL SCHEMAFULL PERMISSIONS NONE' }, users: {  } }"
        .to_string()
}

/// A bare db-level capture with the ledger table riding it — the
/// self-record's machinery is real schema every apply creates.
fn bare_capture_with_ledger() -> String {
    "{ accesses: {  }, analyzers: {  }, apis: {  }, buckets: {  }, \
     configs: {  }, functions: {  }, models: {  }, modules: {  }, \
     params: {  }, sequences: {  }, tables: { user: 'DEFINE TABLE user \
     TYPE NORMAL SCHEMAFULL PERMISSIONS NONE', _surform_migrations: \
     'DEFINE TABLE _surform_migrations SCHEMAFULL' }, users: {  } }"
        .to_string()
}

/// A completely empty capture — every section bare, so an SDL
/// converges purely additively (the probe shapes' substrate: no
/// removals, no markers, only the SDL's own defines).
fn empty_capture() -> String {
    "{ accesses: {  }, analyzers: {  }, apis: {  }, buckets: {  }, \
     configs: {  }, functions: {  }, models: {  }, modules: {  }, \
     params: {  }, sequences: {  }, tables: {  }, users: {  } }"
        .to_string()
}

/// The fixture SDL — the desired state the golden converges to.
fn fixture_sdl() -> String {
    guest_file("../surform-surrealism/tests/fixtures/converge.surdl")
}

/// The golden's structured carrier: the script text is byte-identical
/// to the committed golden (the converge suite's pin, re-proven through the parts
/// pipeline), and the marked statements are EXACTLY its five
/// executable statements, in order — the `USE` head, the
/// `BEGIN`/`COMMIT` frame, the header comment block, and the slot
/// comment banners never enter the carrier at all.
#[test]
fn the_golden_carries_its_five_generator_statements() {
    let parts = surform_surrealism_apply::apply::converge_parts(&fixture_sdl(), &fixture_capture())
        .expect("the fixture pair converges");
    assert_eq!(parts.text, golden_script());
    assert_eq!(
        parts.statements,
        [
            "DEFINE PARAM OVERWRITE $retention VALUE 30",
            "DEFINE TABLE OVERWRITE audit TYPE NORMAL SCHEMAFULL",
            "DEFINE FIELD OVERWRITE `at` ON audit TYPE datetime",
            "REMOVE FIELD IF EXISTS note ON `user`",
            "REMOVE TABLE IF EXISTS legacy",
        ],
    );
    assert!(parts.raw_bodies.is_empty());
}

/// Context lines and trivia never ride the carrier: a namespace
/// declare's `USE NS` follower stays in the TEXT (the bytes `build`
/// prints) but not in the statements; the frames' `BEGIN`/`COMMIT`
/// and the header comments never appear; a `;` inside a string
/// literal stays INSIDE its marked statement (nothing scans for one).
/// (The SDL's own declared database is never drafted — the compare's
/// scope IS that database, the session pre-flight's — so the carrier
/// opens directly with the SDL's own defines, the table first here;
/// owner defines draft only on owner-family findings, which a
/// single-database scope never produces.)
#[test]
fn the_carrier_never_carries_context_lines_or_splits_marked_content() {
    let sdl = "\
         namespace demo;\n\
         database shop;\n\
         table t1 schemafull {\n    \
         comment: 'a;b';\n\
         };\n";
    let parts =
        surform_surrealism_apply::apply::converge_parts(sdl, &empty_capture()).expect("converges");
    assert_eq!(
        parts.statements,
        [
            "DEFINE TABLE OVERWRITE t1 TYPE NORMAL SCHEMAFULL",
            "DEFINE FIELD OVERWRITE `comment` ON t1 TYPE 'a;b'",
        ],
    );
    for statement in &parts.statements {
        let head = statement.split_ascii_whitespace().next().unwrap();
        assert!(
            !matches!(
                head.to_ascii_uppercase().as_str(),
                "USE" | "BEGIN" | "COMMIT"
            ),
            "a context line rode the carrier: {statement}"
        );
    }
    assert!(
        parts.text.contains("USE NS demo DB shop;"),
        "{}",
        parts.text
    );
}

/// Closed structurally: `default always: /a*/` (the
/// compound clause head the position recursion misjudged) plus
/// `value: count / 2` (the keyword-spelled division) compile to a
/// script whose marked statements are EXACT — the emitter cannot
/// merge them, because it never looks for a boundary.
#[test]
fn the_default_always_regex_probe_carries_exact_boundaries() {
    let sdl = "database s3r3;\n\
               table t schemafull {\n    \
               pad: regex { default always: /a*/; };\n    \
               avg: int { value: count / 2; };\n};\n";
    let compiled = surform_surrealism_apply::compile_sdl(sdl).expect("the probe SDL compiles");
    assert_eq!(compiled.matches(';').count(), 4, "{compiled}");
    let parts = surform_surrealism_apply::apply::converge_parts(sdl, &empty_capture())
        .expect("the probe converges");
    assert_eq!(
        parts.statements,
        [
            "DEFINE TABLE OVERWRITE t TYPE NORMAL SCHEMAFULL",
            "DEFINE FIELD OVERWRITE avg ON t TYPE int VALUE count / 2",
            "DEFINE FIELD OVERWRITE pad ON t TYPE regex DEFAULT ALWAYS /a*/",
        ],
        "{:#?}",
        parts.statements
    );
}

/// The whitespace-led regex as a carrier pin: the
/// pattern `/*/`-with-space and a later statement boundary stay exact.
#[test]
fn the_space_led_regex_field_marks_exact_boundaries() {
    let sdl = "database s3r1;\n\
               table t schemafull {\n    \
               pad: regex { default: / */;\n    };\n};\n\
               table t2 schemafull;\n";
    let parts = surform_surrealism_apply::apply::converge_parts(sdl, &empty_capture())
        .expect("the probe SDL converges");
    assert_eq!(
        parts.statements,
        [
            "DEFINE TABLE OVERWRITE t TYPE NORMAL SCHEMAFULL",
            "DEFINE FIELD OVERWRITE pad ON t TYPE regex DEFAULT / */",
            "DEFINE TABLE OVERWRITE t2 TYPE NORMAL SCHEMAFULL",
        ],
    );
}

/// The keyword-spelled operand's division, as a
/// carrier pin: a field named `count` (backticked at the DEFINE
/// position — the codegen's full-keyword escape) and the bare
/// `count / 2` operand both stay inside their marked statements.
#[test]
fn the_keyword_spelled_operand_marks_exact_boundaries() {
    let sdl = "database s3r2;\n\
               table metrics schemafull {\n    \
               count: int;\n    \
               avg2: int { value: count / 2; };\n};\n";
    let parts = surform_surrealism_apply::apply::converge_parts(sdl, &empty_capture())
        .expect("the probe SDL converges");
    assert_eq!(
        parts.statements,
        [
            "DEFINE TABLE OVERWRITE metrics TYPE NORMAL SCHEMAFULL",
            "DEFINE FIELD OVERWRITE avg2 ON metrics TYPE int VALUE count / 2",
            "DEFINE FIELD OVERWRITE `count` ON metrics TYPE int",
        ],
    );
}

/// The data-loss gate: the committed golden carries the REQUIRED and
/// WARNING banners (its removals) and refuses — five marker lines,
/// the first the field-removal slot's REQUIRED banner; a marker-free
/// script passes. The markers are the generator's COMMENT anchors —
/// text beside the statement, never carrier elements — so this gate
/// alone reads the script text, and reads it LINE-PREFIX EXACT over
/// the tool's marker spellings (a substring scan misreads statement
/// CONTENT carrying the banner mid-line).
#[test]
fn the_marker_gate_refuses_the_golden_and_passes_additive_scripts() {
    let error = surform_surrealism_apply::apply::refuse_surform_markers(&golden_script())
        .expect_err("the golden carries slot markers");
    assert!(error.contains("5 SURFORM marker line(s)"), "{error}");
    assert!(
        error.contains("first marker: -- >>> SURFORM REQUIRED: DATA MIGRATION <<<"),
        "{error}"
    );
    surform_surrealism_apply::apply::refuse_surform_markers(
        "USE NS demo DB shop;\nBEGIN TRANSACTION;\nDEFINE TABLE OVERWRITE t1;\nCOMMIT;\n",
    )
    .expect("marker-free passes");
}

/// End to end at the pure level: statement CONTENT
/// carrying the banner substring is not a marker. The exact SDL — a
/// purely ADDITIVE schema whose field comment's value
/// carries `>>> SURFORM WARNING: DATA LOSS <<<` — is the shape a
/// substring scan falsely refuses (it would raise the
/// destructive-drift message "the convergence script
/// carries 1 SURFORM marker line(s)") on a comment string 13
/// characters into the line; a substring scan rides any line
/// CONTAINING the banner. The scan is LINE-PREFIX EXACT (the
/// CLI `classify_marker_line`'s semantics, mirrored): the value rides
/// a `DEFINE` line mid-line, and the schema applies clean. The
/// printer escapes newlines inside strings and idents (surrealdb-types
/// 3.3.0 `EscapeWriter`, SingleLine), so a value can never continue
/// onto its own line — a line BEGINNING with a marker spelling can
/// only be a generator anchor.
#[test]
fn a_banner_substring_inside_statement_content_is_not_a_marker() {
    let sdl = "database s3h1;\n\
               table t schemafull {\n\
               \x20   comment: 'see >>> SURFORM WARNING: DATA LOSS <<< docs';\n\
               };\n";
    let parts =
        surform_surrealism_apply::apply::converge_parts(sdl, &empty_capture()).expect("converges");
    assert!(
        parts
            .statements
            .iter()
            .any(|statement| statement.contains(">>> SURFORM WARNING: DATA LOSS <<<")),
        "the banner rides a statement's content: {:?}",
        parts.statements
    );
    surform_surrealism_apply::apply::refuse_surform_markers(&parts.text)
        .expect("content mid-line is not a marker line — the additive schema applies");
}

/// The gate's classification table, pinned shape by shape. REFUSE:
/// the generator's three banner spellings (the CommentBanners
/// emission) and the CLI's slot pair (the same pub constants
/// migrate.rs's `classify_marker_line` classifies — one home, the
/// guest references them). PASS: the banner substring mid-line, a
/// space-led banner (line-PREFIX exact), a closer with a suffix
/// (the CLI demands exact equality), an opener without the ` (`
/// (prefix-exact), and the generator's own hint lines. The CLI's
/// git-stray classes (any `<<<<<<<`/`>>>>>>>`-leading line) are
/// deliberately NOT carried: this script is generator output, never
/// hand-carried draft text, so those shapes are unrepresentable —
/// and a raw body carrying one refuses at the raw gate regardless.
#[test]
fn the_marker_gate_classifies_lines_prefix_exact() {
    use surform_surrealism_apply::apply::refuse_surform_markers;

    for marker in [
        "-- >>> SURFORM REQUIRED: DATA MIGRATION <<<",
        "-- >>> SURFORM REQUIRED: END <<<",
        "-- >>> SURFORM WARNING: DATA LOSS <<<",
        "-- >>> SURFORM WARNING: DATA LOSS <<< (trailing prose still refuses)",
        "<<<<<<< SURFORM SLOT (db: demo/shop)",
        ">>>>>>> SURFORM SLOT END",
    ] {
        refuse_surform_markers(marker).expect_err("a tool marker spelling at line start refuses");
    }
    for content in [
        "DEFINE FIELD OVERWRITE `comment` ON t TYPE string \
         COMMENT 'see >>> SURFORM WARNING: DATA LOSS <<< docs'",
        " -- >>> SURFORM WARNING: DATA LOSS <<<",
        ">>>>>>> SURFORM SLOT ENDING SOON",
        "<<<<<<< SURFORM SLOT without the paren",
        "-- e.g. the WARNING banner marks data loss",
    ] {
        refuse_surform_markers(content).expect("content is not a marker line");
    }
}

/// The additive-only gate: ANY `REMOVE` statement in
/// the marked statements refuses, naming it — the live probe
/// (`REMOVE USER IF EXISTS low ON DATABASE;`, which executed
/// in-guest before this gate existed) refuses here; an additive list passes;
/// a `REMOVE` inside a string is content, not a head (a statement is
/// never an expression's text — the head test is syntactic over
/// generator-guaranteed boundaries); the head test folds case and
/// trims the `;` whole-word (a `REMOVEX` head is an identifier).
#[test]
fn the_remove_gate_refuses_any_marked_remove_and_passes_additive() {
    use surform_surrealism_apply::apply::refuse_removals;

    let error = refuse_removals(&["REMOVE USER IF EXISTS low ON DATABASE".to_string()])
        .expect_err("the probe shape refuses");
    assert!(error.contains("1 REMOVE statement(s)"), "{error}");
    assert!(
        error.contains("  REMOVE USER IF EXISTS low ON DATABASE"),
        "{error}"
    );
    // Additive lists pass whole.
    refuse_removals(&[
        "DEFINE TABLE OVERWRITE t1".to_string(),
        "DEFINE FIELD OVERWRITE `at` ON t1 TYPE datetime".to_string(),
    ])
    .expect("defines pass");
    // Not a head: a REMOVE inside a string literal is expression
    // content; a whole-word lookalike is an identifier.
    refuse_removals(&[
        "DEFINE FIELD OVERWRITE v ON t TYPE string VALUE 'REMOVE USER x'".to_string(),
        "DEFINE TABLE OVERWRITE removex".to_string(),
    ])
    .expect("non-heads pass");
    // Case-folded head, `;` trimmed.
    let error = refuse_removals(&["remove table if exists x;".to_string()])
        .expect_err("the folded head refuses");
    assert!(error.contains("remove table if exists x;"), "{error}");
}

/// End to end at the pure level: a database carrying
/// a db user the SDL never declares converges to an UNMARKED
/// `REMOVE USER` — the marker gate passes it (that is the defect
/// class), and the additive-only gate is what refuses, from the
/// carrier. Before the gate existed, the script executed in-guest.
#[test]
fn an_unmarked_user_removal_passes_the_marker_gate_and_refuses_at_the_remove_gate() {
    let capture = "{ accesses: {  }, analyzers: {  }, apis: {  }, buckets: {  }, \
                   configs: {  }, functions: {  }, models: {  }, modules: {  }, \
                   params: {  }, sequences: {  }, tables: { user: 'DEFINE TABLE user \
                   TYPE NORMAL SCHEMAFULL PERMISSIONS NONE' }, users: { low: \
                   \"DEFINE USER low ON DATABASE PASSHASH 'h' ROLES VIEWER DURATION \
                   FOR TOKEN 1h, FOR SESSION NONE\" } }";
    let parts = surform_surrealism_apply::apply::converge_parts(&fixture_sdl(), capture)
        .expect("converges");
    assert!(
        parts
            .text
            .contains("REMOVE USER IF EXISTS low ON DATABASE;"),
        "the unmarked removal drafts: {}",
        parts.text
    );
    surform_surrealism_apply::apply::refuse_surform_markers(&parts.text)
        .expect("the defect: an unmarked removal carries NO marker");
    let error = surform_surrealism_apply::apply::refuse_removals(&parts.statements)
        .expect_err("the additive-only gate refuses it");
    assert!(
        error.contains("REMOVE USER IF EXISTS low ON DATABASE"),
        "{error}"
    );
}

/// The raw-body gate: a raw-authored statement (rule 8's escape
/// hatch) rides the script text but NEVER the marked statements — its
/// boundaries are the author's, which the generator does not mark —
/// and refuses loud, naming the body. The other gates pass it (the
/// raw arm is its own refusal, not the removal gate's business).
#[test]
fn a_raw_authored_statement_refuses_the_apply_flow_loud() {
    let sdl = "database s3raw;\n\
               table keep schemafull;\n\
               raw { DEFINE ANALYZER legacy_ver VS/classic; };\n";
    let parts =
        surform_surrealism_apply::apply::converge_parts(sdl, &empty_capture()).expect("converges");
    assert!(
        parts
            .text
            .contains("DEFINE ANALYZER legacy_ver VS/classic;"),
        "{}",
        parts.text
    );
    assert_eq!(
        parts.statements,
        ["DEFINE TABLE OVERWRITE keep TYPE NORMAL SCHEMAFULL"]
    );
    assert_eq!(parts.raw_bodies, ["DEFINE ANALYZER legacy_ver VS/classic;"]);
    surform_surrealism_apply::apply::refuse_surform_markers(&parts.text).expect("no markers");
    surform_surrealism_apply::apply::refuse_removals(&parts.statements)
        .expect("the raw arm never enters the removal gate");
    let error = surform_surrealism_apply::apply::refuse_raw_bodies(&parts.raw_bodies)
        .expect_err("the raw body refuses");
    assert!(error.contains("1 raw-authored statement(s)"), "{error}");
    assert!(
        error.contains("DEFINE ANALYZER legacy_ver VS/classic;"),
        "{error}"
    );
}

/// The session pre-flight: the SDL's declared database must be the
/// session's — the namespace-free pseudo-scope matches any namespace,
/// the database name never; an unscoped session refuses.
#[test]
fn the_session_pre_flight_pins_the_scope() {
    use surform_surrealism_apply::apply::session_scope_matches;

    session_scope_matches(Some("acme"), Some("shop"), "acme", "shop").expect("exact match");
    session_scope_matches(Some("other"), Some("shop"), "", "shop")
        .expect("the pseudo-scope matches any namespace");
    let error = session_scope_matches(Some("acme"), Some("other"), "acme", "shop")
        .expect_err("database mismatch");
    assert!(
        error.contains("acme/shop") && error.contains("acme/other"),
        "{error}"
    );
    let error = session_scope_matches(Some("other"), Some("shop"), "acme", "shop")
        .expect_err("namespace mismatch");
    assert!(
        error.contains("acme/shop") && error.contains("other/shop"),
        "{error}"
    );
    let error = session_scope_matches(None, None, "", "shop").expect_err("no scope");
    assert!(error.contains("the session names none"), "{error}");
}

/// The per-table capture statements — the shared ident rule spells
/// the table (keyword names backtick; `user` is one).
#[test]
fn table_capture_statements_spell_the_table() {
    use surform::info::script::ident_text;
    use surform_surrealism_apply::apply::table_capture_statement;
    assert_eq!(
        table_capture_statement("legacy"),
        "RETURN <string>(INFO FOR TABLE legacy);"
    );
    assert_eq!(
        table_capture_statement("user"),
        "RETURN <string>(INFO FOR TABLE `user`);"
    );
    // The spellings ride the core's one ident rule, byte-for-byte.
    for name in ["legacy", "user", "table", "a-b"] {
        assert_eq!(
            table_capture_statement(name),
            format!("RETURN <string>(INFO FOR TABLE {});", ident_text(name))
        );
    }
}

/// The capture's table names: the fixture's two tables, and a ledger
/// table riding the capture is excluded (never read for children,
/// never enters the compare).
#[test]
fn table_names_exclude_the_ledger_table() {
    let capture = read_info_object(&fixture_capture()).expect("the committed capture parses");
    assert_eq!(
        surform_surrealism_apply::apply::table_names(&capture),
        ["legacy", "user"]
    );
    let with_ledger = read_info_object(&bare_capture_with_ledger()).expect("parses");
    assert_eq!(
        surform_surrealism_apply::apply::table_names(&with_ledger),
        ["user"]
    );
}

/// The flow's own scaffolding drops from its capture: a module
/// registration (whose `org::name` INFO keys refuse the readers — the
/// live leg's first-round failure) and the module's bucket (whose
/// database-only drift drafts an UNMARKED `REMOVE BUCKET` — the
/// module would delete its own bytes). The ledger table rides the
/// same drop.
#[test]
fn the_scaffolding_drops_from_the_flow_capture() {
    use surform_surrealism_apply::apply::drop_scaffolding;
    let mut info = read_info_object(
        "{ accesses: {  }, buckets: { modules: \"DEFINE BUCKET modules \
         BACKEND 'file:/x' PERMISSIONS FULL\" }, modules: { \
         \"mod::surform_apply\": 'DEFINE MODULE mod::surform_apply FROM \
         f\"modules:/surform-apply.surli\" UNSIGNED PERMISSIONS FULL' }, \
         params: {  }, tables: { user: 'DEFINE TABLE user', \
         _surform_migrations: 'DEFINE TABLE _surform_migrations' } }",
    )
    .expect("parses");
    drop_scaffolding(&mut info);
    assert!(
        !info.0.contains_key("modules") && !info.0.contains_key("buckets"),
        "the scaffolding sections must drop"
    );
    // The ledger table's drop rides converge_model in the flow; the
    // scaffolding drop is additive, not a replacement.
    assert!(info.0.contains_key("tables"));
}

/// The ledger numbering: one past the highest, 1 for an empty ledger,
/// refused past the CLI's draft ceiling.
#[test]
fn the_ledger_number_walks_and_refuses_the_ceiling() {
    use surform_surrealism_apply::apply::next_ledger_number;
    assert_eq!(next_ledger_number(None).unwrap(), 1);
    assert_eq!(next_ledger_number(Some(0)).unwrap(), 1);
    assert_eq!(next_ledger_number(Some(41)).unwrap(), 42);
    assert_eq!(next_ledger_number(Some(9998)).unwrap(), 9999);
    let error = next_ledger_number(Some(9999)).expect_err("ceiling");
    assert!(error.contains("9999"), "{error}");
    let error = next_ledger_number(Some(u32::MAX)).expect_err("saturated");
    assert!(error.contains("full"), "{error}");
}

/// The self-record: the pipeline's ledger block in its
/// generator-marked form — the idempotent defines first, the INSERT
/// last with the number and hash riding, `time::now()` stamping
/// applied_at — and the TEXT form byte-identical to the banner plus
/// the marked statements one per line (the CLI's committed-file
/// bytes never moved).
#[test]
fn the_ledger_ride_is_the_pipeline_block() {
    use surform::migrate::{ledger_statement_list, ledger_statements};
    let ride = surform_surrealism_apply::apply::ledger_ride(
        12,
        &"a".repeat(64),
        "namespace acme;\ndatabase shop;\n",
    );
    assert_eq!(ride.len(), 8, "{ride:#?}");
    assert_eq!(
        ride[0],
        "DEFINE TABLE IF NOT EXISTS _surform_migrations SCHEMAFULL;"
    );
    assert_eq!(
        ride[6],
        "DEFINE FIELD IF NOT EXISTS sdl ON _surform_migrations TYPE string;"
    );
    assert!(ride[7].starts_with(
        "INSERT IGNORE INTO _surform_migrations (id, number, name, hash, message, \
         applied_at, sdl) VALUES (12, 12, 'apply', "
    ));
    assert!(ride[7].contains("time::now()"), "{}", ride[7]);
    assert!(
        ride[7].ends_with("'namespace acme;\\ndatabase shop;\\n');"),
        "{}",
        ride[7]
    );
    // The text form: banner + the marked statements, one per line —
    // the exact bytes the CLI splices (the committed-file hash reads
    // them back).
    let text = ledger_statements(
        "_surform_migrations",
        12,
        "apply",
        &"a".repeat(64),
        "msg",
        "namespace acme;\ndatabase shop;\n",
    );
    let mut expected = String::from(
        "-- surform: migration ledger — appended by `surform migrate commit`; do not edit.\n",
    );
    for statement in ledger_statement_list(
        "_surform_migrations",
        12,
        "apply",
        &"a".repeat(64),
        "msg",
        "namespace acme;\ndatabase shop;\n",
    ) {
        expected.push_str(&statement);
        expected.push('\n');
    }
    assert_eq!(text, expected);
}

/// The outcome summaries — the exact strings the flow returns.
#[test]
fn the_summaries_are_pinned() {
    use surform_surrealism_apply::apply::{applied_summary, converged_summary, failed_summary};
    assert_eq!(
        converged_summary(),
        "(converged) — nothing to apply, the ledger untouched"
    );
    assert_eq!(
        applied_summary(5, 4),
        "applied 5 statement(s); recorded 0004_apply in the ledger"
    );
    let failed = failed_summary(3, 5, "The table 'legacy' does not exist");
    assert_eq!(
        failed,
        "statement 4 of 5 failed: The table 'legacy' does not exist\n\
         the apply is one transaction at the dispatch boundary: the engine \
         rolled back everything this call did, and no ledger row was written. \
         Resolve the failure and re-run — the next convergence drafts only what \
         still differs"
    );
}

/// The script hash: sha256 over the executed script's bytes — the
/// CLI's committed-file definition (the filled script, ledger region
/// drained) applied to the module's shape, where the ledger never
/// splices into the script text.
#[test]
fn the_script_hash_is_sha256_over_the_script_bytes() {
    use surform_surrealism_apply::apply::script_hash;
    assert_eq!(
        script_hash("DEFINE TABLE t;"),
        surform::migrate::sha256_hex(b"DEFINE TABLE t;")
    );
}

/// The mock-side full pipeline, gates and all: converge the fixture
/// pair the way `converge` does, then run the apply flow's gates over
/// the carriers. The committed fixture pair's script carries slot
/// markers (its removals) — the flow refuses before executing
/// anything; against the bare capture (no children, no ledger
/// machinery) the same SDL converges to the additive statements the
/// flow WOULD execute, in generator order.
#[test]
fn the_mock_pipeline_gates_and_executes_the_marked_statements() {
    let sdl = fixture_sdl();

    // Full-fidelity capture: the golden script — and the gates refuse
    // it (the destructive class): the markers first (precedence), the
    // additive-only scan second (the golden's REMOVE statements
    // refuse there too).
    let parts = surform_surrealism_apply::apply::converge_parts(&sdl, &fixture_capture())
        .expect("converges");
    assert_eq!(parts.text, golden_script());
    surform_surrealism_apply::apply::refuse_surform_markers(&parts.text)
        .expect_err("the fixture's removals carry slots — the destructive class refuses");
    assert_eq!(parts.statements.len(), 5);
    surform_surrealism_apply::apply::refuse_removals(&parts.statements)
        .expect_err("the golden's REMOVE statements refuse the additive-only gate");

    // Bare capture: the capture cannot claim ANY children, so the
    // SDL's children for `user` surface as defines alongside the
    // missing `audit` table — additive here (the `legacy` removal is
    // invisible without a capture that claims the table), so the
    // gates pass and the carrier names the defines in the renderer's
    // order.
    let parts =
        surform_surrealism_apply::apply::converge_parts(&sdl, &bare_capture()).expect("converges");
    surform_surrealism_apply::apply::refuse_surform_markers(&parts.text)
        .expect("a define-only script carries no markers");
    surform_surrealism_apply::apply::refuse_removals(&parts.statements)
        .expect("a define-only script carries no removals");
    assert_eq!(
        parts.statements,
        [
            "DEFINE FIELD OVERWRITE email ON `user` TYPE string",
            "DEFINE INDEX OVERWRITE email_idx ON `user` FIELDS email UNIQUE",
            "DEFINE PARAM OVERWRITE $retention VALUE 30",
            "DEFINE TABLE OVERWRITE audit TYPE NORMAL SCHEMAFULL",
            "DEFINE FIELD OVERWRITE `at` ON audit TYPE datetime",
        ],
    );

    // The ledger machinery riding a capture is dropped before the
    // compare: the SDL above doesn't declare it, and the script never
    // drafts its removal (the revert precedent, re-proven through the
    // apply-side capture shape).
    let parts = surform_surrealism_apply::apply::converge_parts(&sdl, &bare_capture_with_ledger())
        .expect("converges");
    assert!(
        !parts.text.contains("_surform_migrations"),
        "the ledger table is never converge material: {}",
        parts.text
    );
}
