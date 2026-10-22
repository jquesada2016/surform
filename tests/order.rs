//! The declarative namespace's semantics (src/order.rs): the
//! compiler-owned emission order over the frozen define-time edge
//! table, the duplicate-definition rule (SDL-E1062, every family),
//! and the chain-named cycle diagnostic (SDL-E0015). The engine-fact
//! legs exec the embedded engine — authored order silently mis-stores
//! what the compiler's own order applies correctly, which is the
//! unit's whole point.

use std::path::{Path, PathBuf};

use miette::Diagnostic as _;
use surform::diag::Diag;
use surform::{codegen, lint, load, parser};

#[path = "common/exec.rs"]
mod exec;

fn parse_ok(src: &str) -> surform::ast::SdlFile {
    let (file, diags) = parser::parse_file(src);
    assert!(diags.is_empty(), "{src}: {diags:?}");
    file
}

fn check_diags(src: &str) -> Vec<Diag> {
    let file = parse_ok(src);
    let spans: Vec<surform::lexer::Span> = lint::scan_source(src)
        .statements
        .iter()
        .map(|s| s.span)
        .collect();
    surform::order::check_document(&file.items, &[], &spans)
        .into_iter()
        .map(|(diag, _)| diag)
        .collect()
}

// ---------------------------------------------------------------------
// The emission order — the engine facts it fixes
// ---------------------------------------------------------------------

/// The unit's payoff (frozen-table row 6): a forward param read in
/// AUTHORED order silently stores `none`; the compiler's own emission
/// order stores the value. Probed against the embedded engine, both
/// sides.
#[tokio::test]
async fn forward_param_stores_none_in_authored_order_and_the_value_in_emission() {
    let authored = "DEFINE PARAM OVERWRITE $a VALUE $b;\nDEFINE PARAM OVERWRITE $b VALUE 2;";
    let db = exec::mem_db().await;
    db.use_ns("test").await.unwrap();
    db.use_db("test").await.unwrap();
    db.query(authored).await.unwrap().check().unwrap();
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("VALUE NONE"),
        "the authored order silently stores none: {info}"
    );

    let src = "param $a = $b + 1;\nparam $b = 2;";
    let ddl = codegen::emit(&parse_ok(src));
    assert!(
        ddl.find("DEFINE PARAM OVERWRITE $b").unwrap()
            < ddl.find("DEFINE PARAM OVERWRITE $a").unwrap(),
        "the compiler orders the definition first: {ddl}"
    );
    let db = exec::mem_db().await;
    db.use_ns("test").await.unwrap();
    db.use_db("test").await.unwrap();
    exec::exec_sdl(&db, src).await;
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("VALUE 3"),
        "the emitted order stores the value: {info}"
    );
}

/// Row 1: a forward view source fails the define in authored order
/// (`TbNotFound`) and applies in the compiler's order.
#[tokio::test]
async fn forward_view_source_orders_in_emission() {
    let db = exec::mem_db().await;
    db.use_ns("test").await.unwrap();
    db.use_db("test").await.unwrap();
    let authored = "DEFINE TABLE OVERWRITE v AS SELECT * FROM t;\nDEFINE TABLE OVERWRITE t;";
    let result = db.query(authored).await.unwrap().check();
    assert!(result.is_err(), "the authored order fails TbNotFound");

    let src = "table view v { select * from t; };\ntable t;";
    let ddl = codegen::emit(&parse_ok(src));
    assert!(
        ddl.find("DEFINE TABLE OVERWRITE t").unwrap()
            < ddl.find("DEFINE TABLE OVERWRITE v").unwrap(),
        "the table precedes the view that reads it: {ddl}"
    );
    exec::exec_sdl(&db, src).await;
}

/// Row 4: the analyzer precedes the fulltext index that names it,
/// including when the index rides a mixin spread (expansion lands it
/// in the table before the sort sees it).
#[test]
fn analyzer_orders_before_the_fulltext_index_through_expansion() {
    let ddl = codegen::emit(&parse_ok(
        "analyzer ascii { tokenizers: class; };\nmixin text_members {\nindex fulltext fts { fields: title; analyzer: ascii; };\n};\ntable post { ...text_members; };",
    ));
    let analyzer_at = ddl.find("DEFINE ANALYZER OVERWRITE").unwrap();
    let index_at = ddl.find("FULLTEXT ANALYZER").unwrap();
    assert!(
        analyzer_at < index_at,
        "the analyzer precedes its index: {ddl}"
    );
}

/// Row 4b: a table block's nested members reorder — fields before the
/// indexes that name them — and the block still applies on a
/// SCHEMAFULL table (the engine's FdNotFound check would fail the
/// authored order).
#[tokio::test]
async fn nested_field_orders_before_the_index_that_names_it() {
    let db = exec::mem_db().await;
    db.use_ns("test").await.unwrap();
    db.use_db("test").await.unwrap();
    let authored = "DEFINE TABLE OVERWRITE t SCHEMAFULL;\nDEFINE INDEX OVERWRITE ixt ON t FIELDS title;\nDEFINE FIELD OVERWRITE title ON t TYPE string;";
    let result = db.query(authored).await.unwrap().check();
    assert!(
        result.is_err(),
        "the authored order fails FdNotFound on schemafull"
    );

    let ddl = codegen::emit(&parse_ok(
        "table t schemafull {\nindex ixt { fields: title; };\nfield title: string;\n};",
    ));
    assert!(
        ddl.find("DEFINE FIELD OVERWRITE title").unwrap()
            < ddl.find("DEFINE INDEX OVERWRITE ixt").unwrap(),
        "the field precedes the index inside the block: {ddl}"
    );
    db.query(&ddl).await.unwrap().check().unwrap();
}

/// The head run sorts INCLUDING its first statement even when a
/// scope section follows (every pushed run used to be
/// marked leader, freezing the head statement).
#[test]
fn head_run_sorts_even_with_a_later_scope_section() {
    let src = "param $a = $b + 1;\nparam $b = 2;\nnamespace acme;\nparam $z = 1;";
    let ddl = codegen::emit(&parse_ok(src));
    assert!(
        ddl.find("DEFINE PARAM OVERWRITE $b").unwrap()
            < ddl.find("DEFINE PARAM OVERWRITE $a").unwrap(),
        "the head run's first statement sorts: {ddl}"
    );
}

/// A same-scope dependency across a scope section is unplaceable —
/// the honest third failure class (SDL-E0016): emission
/// never reorders across a scope leader, so the document is refused
/// instead of emitting apply-failing DDL.
#[test]
fn cross_section_dependency_errors() {
    let src = "namespace acme;\ntable view v { select * from t; };\ndatabase app;\nnamespace acme;\ntable t;";
    let diags = check_diags(src);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(
        diags[0].code().map(|c| c.to_string()).as_deref(),
        Some("SDL-E0016")
    );
    let message = diags[0].to_string();
    assert!(message.contains("table `t`"), "{message}");
}

/// The same dependency INSIDE one section reorders — the E0016 class
/// is strictly the cross-section shape.
#[tokio::test]
async fn same_section_forward_dependency_reorders() {
    let db = exec::mem_db().await;
    db.use_ns("test").await.unwrap();
    db.use_db("test").await.unwrap();
    exec::exec_sdl(
        &db,
        "namespace acme;\ndatabase app;\ntable view v { select * from t; };\ntable t;",
    )
    .await;
}

/// Backticked NON-word-shaped field names keep their
/// field-before-index edge —
/// the unwrap ran before the word-shape gates, so
/// `` `my-field` `` was rejected and the index emitted first.
#[test]
fn backticked_non_word_field_names_keep_their_edge() {
    let ddl = codegen::emit(&parse_ok(
        "table t schemafull {\nindex ixt { fields: `my-field`; };\nfield `my-field`: string;\n};",
    ));
    assert!(
        ddl.find("DEFINE FIELD OVERWRITE `my-field`").unwrap()
            < ddl.find("DEFINE INDEX OVERWRITE ixt").unwrap(),
        "the field precedes the index that names it: {ddl}"
    );
}

/// fmt never reorders (the never-reorder contract survives the
/// compiler-owned emission order): a forward-referencing file formats
/// in its authored order.
#[test]
fn fmt_keeps_authored_order() {
    use surform::info::write::write_canonical;
    let formatted = write_canonical(&parse_ok("param $a = $b + 1;\nparam $b = 2;"));
    assert!(
        formatted.find("$b = 2").unwrap() > formatted.find("$a = $b + 1").unwrap(),
        "fmt is presentation, not semantics: {formatted}"
    );
}

// ---------------------------------------------------------------------
// True cycles — the chain named (SDL-E0015)
// ---------------------------------------------------------------------

#[test]
fn param_cycle_errors_with_the_chain() {
    let diags = check_diags("param $a = $b + 1;\nparam $b = $a + 1;");
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(
        diags[0].code().map(|c| c.to_string()).as_deref(),
        Some("SDL-E0015")
    );
    let message = diags[0].to_string();
    assert!(
        message.contains("param `$a`") && message.contains("param `$b`"),
        "the chain names both participants: {message}"
    );
}

#[test]
fn view_cycle_errors_with_the_chain() {
    let diags =
        check_diags("table view a { select * from b; };\ntable view b { select * from a; };");
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(
        diags[0].code().map(|c| c.to_string()).as_deref(),
        Some("SDL-E0015")
    );
    let message = diags[0].to_string();
    assert!(
        message.contains("view `a`") && message.contains("view `b`"),
        "the chain names both views: {message}"
    );
}

#[test]
fn a_longer_cycle_reports_once() {
    let diags =
        check_diags("param $a = $b + 1;\nparam $b = $c + 1;\nparam $c = $a + 1;\nparam $z = 1;");
    assert_eq!(diags.len(), 1, "{diags:?}");
    let message = diags[0].to_string();
    assert!(
        message.contains("param `$c`"),
        "the chain spans the loop: {message}"
    );
}

/// A param reading ITSELF is a length-1 cycle — the
/// self-edge only drops for views (the engine inserts the table
/// before the FROM check), never for params.
#[test]
fn param_self_read_is_a_cycle() {
    let diags = check_diags("param $a = $a + 1;");
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(
        diags[0].code().map(|c| c.to_string()).as_deref(),
        Some("SDL-E0015")
    );
    assert!(
        diags[0].to_string().contains("param `$a` → param `$a`"),
        "{}",
        diags[0]
    );
}

/// A view reading itself is define-legal (the engine inserts the
/// table first) — no cycle, no diagnostic.
#[test]
fn view_self_read_is_legal() {
    assert!(check_diags("table view v { select * from v; };").is_empty());
}

/// A `mod::` call in a define-time slot is a MODULE dependency — the
/// engine resolves `mod::<first-segment>` at define.
#[test]
fn module_call_orders_before_the_param_that_calls_it() {
    let src = "param $x = mod::m1::bar();\nmodule m1 { from: f\"bucket:/x.wasm\"; unsigned; };";
    let ddl = codegen::emit(&parse_ok(src));
    assert!(
        ddl.find("DEFINE MODULE OVERWRITE mod::m1").unwrap()
            < ddl.find("DEFINE PARAM OVERWRITE $x").unwrap(),
        "the module precedes the param that calls it: {ddl}"
    );
}

/// Two silo modules (self-named, `name: None`) key by their
/// executable identity, not by the empty name — they were
/// falsely refused as duplicates.
#[test]
fn anonymous_silo_modules_are_distinct_definitions() {
    let src = "module { from: silo::a::b::<1.0.0>; unsigned; };\nmodule { from: silo::c::d::<2.0.0>; unsigned; };";
    assert!(check_diags(src).is_empty());
}

/// A silo call in a define-time slot is a MODULE dependency on the
/// silo module whose executable carries the same org::pkg::version —
/// the call filed fn-class and never reordered.
#[test]
fn silo_call_orders_before_the_param_that_calls_it() {
    let src = "param $x = silo::acme::utils::<1.2.0>::greet();\nmodule { from: silo::acme::utils::<1.2.0>; unsigned; };";
    let ddl = codegen::emit(&parse_ok(src));
    assert!(
        ddl.find("DEFINE MODULE OVERWRITE silo::acme").unwrap()
            < ddl.find("DEFINE PARAM OVERWRITE $x").unwrap(),
        "the silo module precedes the param that calls it: {ddl}"
    );
}

/// A spread-carried index's field edge rides the expansion: the
/// cross-section shape refuses (E0016) exactly as the standalone form
/// does — the authored stream hid the edge.
#[test]
fn spread_carried_field_edge_is_visible_to_the_check() {
    let src = "namespace acme;\ntable post schemafull { ...m; };\nmixin m { index ixt { fields: title; }; };\ndatabase app;\nnamespace acme;\nfield title: string { on: post; };";
    let diags = check_diags(src);
    assert!(
        diags
            .iter()
            .any(|diag| diag.code().map(|c| c.to_string()).as_deref() == Some("SDL-E0016")),
        "{diags:?}"
    );
}

/// …and the same-section shape reorders through the expansion (the
/// field lands before the spread-carried index in the emitted DDL).
#[tokio::test]
async fn spread_carried_field_edge_reorders_in_emission() {
    let db = exec::mem_db().await;
    db.use_ns("test").await.unwrap();
    db.use_db("test").await.unwrap();
    exec::exec_sdl(
        &db,
        "namespace acme;\ndatabase app;\nmixin m { index ixt { fields: title; }; };\ntable post schemafull { ...m; field title: string; };",
    )
    .await;
}

/// Backticked and non-ASCII field names keep their
/// field-before-index edge — the
/// canon filter dropped them, emitting index-before-field
/// on schemafull.
#[test]
fn backticked_and_non_ascii_field_names_keep_their_edge() {
    let ddl = codegen::emit(&parse_ok(
        "table t schemafull {\nindex ixt { fields: `t\u{ed}tulo`; };\nfield `t\u{ed}tulo`: string;\n};",
    ));
    assert!(
        ddl.find("DEFINE FIELD OVERWRITE `t\u{ed}tulo`").unwrap()
            < ddl.find("DEFINE INDEX OVERWRITE ixt").unwrap(),
        "the field precedes the index that names it: {ddl}"
    );
}

/// A `mod::` call files ONLY the module edge — a same-spelled fn
/// never phantom-edges (the engine resolves the module, not the fn).
/// A fn named exactly like the call's path in a LATER
/// section must not refuse the document.
#[test]
fn mod_call_files_no_phantom_fn_edge() {
    let src = "namespace acme;\nparam $x = mod::m1::bar();\ndatabase app;\nfn m1::bar() { return 1; };\nmodule m1 { from: f\"bucket:/x.wasm\"; unsigned; };";
    assert!(
        check_diags(src).is_empty(),
        "the module is satisfied in the first section; the fn is not a dependency: {diags:?}",
        diags = check_diags(src)
    );
}

/// A module-class call in a TABLE position files the module edge —
/// `walk_target` filed it fn-class, so the dependency
/// never reordered (and a same-spelled fn phantom-edged).
#[test]
fn table_position_module_call_orders_and_never_phantom_edges() {
    let src = "param $p = SELECT * FROM mod::m1::t();\nmodule m1 { from: f\"bucket:/x.wasm\"; unsigned; };";
    let ddl = codegen::emit(&parse_ok(src));
    assert!(
        ddl.find("DEFINE MODULE OVERWRITE mod::m1").unwrap()
            < ddl.find("DEFINE PARAM OVERWRITE $p").unwrap(),
        "the module precedes the param whose table call resolves it: {ddl}"
    );
    // A same-spelled fn in a LATER section never phantom-edges.
    let src = "namespace acme;\nparam $p = SELECT * FROM mod::m1::t();\nmodule m1 { from: f\"bucket:/x.wasm\"; unsigned; };\ndatabase app;\nfn m1::t() { return 1; };";
    assert!(check_diags(src).is_empty());
}

/// Partially-quoted dotted paths and wildcard paths keep their
/// field-before-index edge — the candidate gate took word-shaped paths
/// only, so `a.`b c`` and `tags.*` lost the edge.
#[test]
fn mixed_quoted_and_wildcard_field_names_keep_their_edge() {
    let ddl = codegen::emit(&parse_ok(
        "table t schemafull {\nindex ixt { fields: a.`b c`; };\nfield a.`b c`: string;\n};",
    ));
    assert!(
        ddl.find("DEFINE FIELD OVERWRITE a.`b c`").unwrap()
            < ddl.find("DEFINE INDEX OVERWRITE ixt").unwrap(),
        "the field precedes the index that names it: {ddl}"
    );
    let ddl = codegen::emit(&parse_ok(
        "table t schemafull {\nindex ixt { fields: tags.*; };\nfield tags.*: array;\n};",
    ));
    assert!(
        ddl.find("DEFINE FIELD OVERWRITE tags.*").unwrap()
            < ddl.find("DEFINE INDEX OVERWRITE ixt").unwrap(),
        "the wildcard field precedes the index: {ddl}"
    );
}

/// A nested member and a standalone member of one host are ONE
/// definition — the registry walked top-level statements
/// only, so the pair folded last-wins silently.
/// A standalone member and its table's nested twin are the twin-fold
/// workflow's INPUT (normalize folds the exact twin; L0007 nudges the
/// nesting) — legal SDL, not a duplicate. The registry keys top-level
/// statements only (issue #48's ledgered residue); two STANDALONE
/// members of one host still collide.
#[test]
fn nested_vs_standalone_members_stay_legal_and_standalones_collide() {
    assert!(
        check_diags("table t { field x: string; };\nfield x: int { on: t; };").is_empty(),
        "the twin-fold input shape loads"
    );
    let diags = check_diags("field x: int { on: t; };\nfield x: string { on: t; };\ntable t;");
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(
        diags[0].code().map(|c| c.to_string()).as_deref(),
        Some("SDL-E1062")
    );
    // Different hosts stay legal.
    assert!(
        check_diags("table a { field x: string; };\nfield x: int { on: b; };\ntable b;").is_empty()
    );
}

/// A user/access define-time slot evaluates in the SECTION's scope
/// (the USE lines), not the statement's catalog level —
/// `on: ns` keyed the resolve to namespace scope, so a db-section
/// param/fn read never edged and the emitted DDL failed at apply.
#[tokio::test]
async fn user_and_access_slots_resolve_at_the_section_scope() {
    let src = "namespace acme;\ndatabase app;\nuser u { on: ns; password: 'x'; duration { session: $p; }; };\nparam $p = 2h;";
    let ddl = codegen::emit(&parse_ok(src));
    assert!(
        ddl.find("DEFINE PARAM OVERWRITE $p").unwrap()
            < ddl.find("DEFINE USER OVERWRITE u").unwrap(),
        "the param precedes the user whose duration reads it: {ddl}"
    );
    let db = exec::mem_db().await;
    db.use_ns("test").await.unwrap();
    db.use_db("test").await.unwrap();
    exec::exec_sdl(&db, src).await;
    // The session keeps neither the DDL query's in-query USE lines
    // nor a raw USE query — re-bind through the method API.
    db.use_ns("acme").await.unwrap();
    db.use_db("app").await.unwrap();
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("2h"),
        "the duration stores the value, not none: {info}"
    );

    let src = "namespace acme;\ndatabase app;\naccess a { on: ns; type: jwt; url: 'https://x'; duration { token: fn::later(); }; };\nfn later() { return 1h; };";
    let ddl = codegen::emit(&parse_ok(src));
    assert!(
        ddl.find("DEFINE FUNCTION OVERWRITE fn::later()").unwrap()
            < ddl.find("DEFINE ACCESS OVERWRITE a").unwrap(),
        "the fn precedes the access whose duration calls it: {ddl}"
    );
    exec::exec_sdl(&db, src).await;
}

#[test]
fn runtime_references_stay_edge_free() {
    // The representability guarantee: fn bodies, permissions, event
    // slots, field defaults — a fn referencing a table referencing a
    // fn, in any arrangement, is no error.
    let src = "\
fn uses_table() { return SELECT * FROM later_table; };
table later_table {
    event e { when: true; then: fn::uses_table(); };
    field f: string { default: fn::uses_table(); value: $input; permissions: full; };
};
param $p = fn::uses_table() != none;
";
    let diags = check_diags(src);
    assert!(
        diags.is_empty(),
        "no define-time edge points back: {diags:?}"
    );
}

/// A cycle across files — the loader's order pass sees the composed
/// document.
#[test]
fn cross_file_cycle_fails_the_load() {
    let dir = tempdir();
    write(
        &dir.join("root.surdl"),
        "include \"a.surdl\";\ninclude \"b.surdl\";\n",
    );
    write(&dir.join("a.surdl"), "table view a { select * from b; };\n");
    write(&dir.join("b.surdl"), "table view b { select * from a; };\n");
    let failure = load::load(
        &dir.join("root.surdl"),
        "root.surdl",
        std::fs::read_to_string(dir.join("root.surdl")).unwrap(),
    )
    .expect_err("a cross-file view cycle fails the load");
    assert!(
        failure
            .errors
            .iter()
            .any(|(diag, _)| matches!(diag, Diag::DefineCycle { .. })),
        "{failure:?}"
    );
}

// ---------------------------------------------------------------------
// Duplicates — every family (SDL-E1062)
// ---------------------------------------------------------------------

#[test]
fn duplicate_definitions_error_for_every_family() {
    for (src, word) in [
        ("table t;\ntable t;", "table `t`"),
        ("param $x = 1;\nparam $x = 2;", "param `$x`"),
        (
            "analyzer a { filters: ascii; };\nanalyzer a { filters: ascii; };",
            "analyzer `a`",
        ),
        (
            "fn f() { return 1; };\nfn f() { return 2; };",
            "function `f`",
        ),
        (
            "user jose { on: root; password: 'x'; };\nuser jose { on: root; password: 'y'; };",
            "user `jose`",
        ),
        ("sequence s = 1000;\nsequence s = 2000;", "sequence `s`"),
        (
            "bucket b { backend: 'file://x'; };\nbucket b { backend: 'file://x'; };",
            "bucket `b`",
        ),
    ] {
        let diags = check_diags(src);
        assert_eq!(diags.len(), 1, "{src}: {diags:?}");
        assert_eq!(
            diags[0].code().map(|c| c.to_string()).as_deref(),
            Some("SDL-E1062"),
            "{src}"
        );
        assert!(
            diags[0]
                .to_string()
                .contains(&format!("{word} is already defined")),
            "{src}: {}",
            diags[0]
        );
    }
}

#[test]
fn same_name_in_different_scopes_is_two_definitions() {
    // A root user and a db user named alike coexist; a table in two
    // databases coexists; two members named alike on different hosts
    // coexist.
    let src = "namespace acme;\ndatabase one;\nuser jose { on: root; password: 'x'; };\nuser jose { on: ns; password: 'y'; };\ntable t;\ndatabase two;\ntable t;";
    assert!(
        check_diags(src).is_empty(),
        "different scopes are different definitions"
    );
}

#[test]
fn scope_reopening_stays_legal() {
    let src = "namespace acme;\ndatabase one;\nnamespace acme;\ndatabase two;\nnamespace acme;";
    assert!(
        check_diags(src).is_empty(),
        "scope anchors are not definitions"
    );
}

#[test]
fn cross_file_duplicate_table_fails_the_load() {
    let dir = tempdir();
    write(
        &dir.join("root.surdl"),
        "include \"a.surdl\";\ninclude \"b.surdl\";\n",
    );
    write(&dir.join("a.surdl"), "table users { field name: string; };");
    write(
        &dir.join("b.surdl"),
        "table users { field email: string; };",
    );
    let failure = load::load(
        &dir.join("root.surdl"),
        "root.surdl",
        std::fs::read_to_string(dir.join("root.surdl")).unwrap(),
    )
    .expect_err("the same table in two files fails the load");
    assert!(
        failure
            .errors
            .iter()
            .any(|(diag, _)| matches!(diag, Diag::DuplicateDefinition { .. })),
        "{failure:?}"
    );
}

/// The mixin registry keeps its parse-time in-file rejection (the
/// same E1062), and the order pass's cross-file arm catches what the
/// parse cannot see.
#[test]
fn mixin_duplicates_keep_their_parse_rejection_and_gain_the_cross_file_arm() {
    // In-file: the parse itself rejects (E1062), so the order pass
    // never sees a clean parse to re-fire on.
    let (_, diags) = parser::parse_file_composable("mixin m { x: int; };\nmixin m { y: int; };");
    assert!(
        diags
            .iter()
            .any(|diag| diag.code().map(|c| c.to_string()).as_deref() == Some("SDL-E1062")),
        "parse rejects the in-file duplicate: {diags:?}"
    );
    let dir = tempdir();
    write(
        &dir.join("root.surdl"),
        "include \"a.surdl\";\ninclude \"b.surdl\";\n",
    );
    write(&dir.join("a.surdl"), "mixin m { x: int; };");
    write(&dir.join("b.surdl"), "mixin m { y: int; };");
    let failure = load::load(
        &dir.join("root.surdl"),
        "root.surdl",
        std::fs::read_to_string(dir.join("root.surdl")).unwrap(),
    )
    .expect_err("the same mixin in two files fails the load");
    assert!(
        failure.errors.iter().any(|(diag, _)| matches!(
            diag,
            Diag::DuplicateDefinition {
                family: "mixin",
                ..
            }
        )),
        "{failure:?}"
    );
}

// ---------------------------------------------------------------------
// The CLI surfaces
// ---------------------------------------------------------------------

#[test]
fn check_reports_the_cycle_and_exits_1() {
    let dir = tempdir();
    let root = dir.join("cycle.surdl");
    write(&root, "param $a = $b + 1;\nparam $b = $a + 1;");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_surform"))
        .arg("check")
        .arg(&root)
        .output()
        .expect("the binary runs");
    assert!(!output.status.success(), "a cycle exits 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("SDL-E0015"), "{stderr}");
    assert!(
        stderr.contains("param `$a` → param `$b` → param `$a`"),
        "{stderr}"
    );
}

#[test]
fn check_reports_the_cross_file_duplicate_against_its_own_file() {
    let dir = tempdir();
    write(
        &dir.join("root.surdl"),
        "include \"a.surdl\";\ninclude \"b.surdl\";\n",
    );
    write(&dir.join("a.surdl"), "table users;");
    write(&dir.join("b.surdl"), "table users;");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_surform"))
        .arg("check")
        .arg(dir.join("root.surdl"))
        .output()
        .expect("the binary runs");
    assert!(!output.status.success(), "a duplicate exits 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("SDL-E1062"), "{stderr}");
    assert!(
        stderr.contains("b.surdl"),
        "the second definition's file renders: {stderr}"
    );
}

#[test]
fn build_emits_the_compilers_order() {
    let dir = tempdir();
    let root = dir.join("order.surdl");
    write(
        &root,
        "param $a = $b + 1;\nparam $b = 2;\nanalyzer ascii { tokenizers: class; };\n\
         table t { index fulltext fts { fields: title; analyzer: ascii; }; };",
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_surform"))
        .arg("build")
        .arg(&root)
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ddl = String::from_utf8_lossy(&output.stdout).to_string();
    let positions = [
        ddl.find("DEFINE PARAM OVERWRITE $b").unwrap(),
        ddl.find("DEFINE PARAM OVERWRITE $a").unwrap(),
        ddl.find("DEFINE ANALYZER OVERWRITE").unwrap(),
        ddl.find("DEFINE INDEX OVERWRITE fts").unwrap(),
    ];
    let mut sorted = positions;
    sorted.sort_unstable();
    assert_eq!(
        positions, sorted,
        "definitions precede their readers: {ddl}"
    );
}

// ---------------------------------------------------------------------
// The tempdir/write harness (tests/include.rs's shape)
// ---------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "surform-order-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("tempdir creates");
    TempDir(dir)
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("fixture writes");
}
