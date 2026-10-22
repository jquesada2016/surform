//! The differ's convergence suite: the comparator over
//! live captured interchanges. Two kinds of leg:
//!
//! - **The convergence property** — clean-apply ⇒ empty diff — over
//!   the exec corpus (one SDL document per statement family, each
//!   applied to a fresh embedded engine and diffed against its own
//!   captured interchange). This is the differ's vacuity guard: a
//!   comparator that flags converged state fails here.
//! - **Drift fixtures** — one live leg per finding class and per
//!   non-comparability arm the comparator owns (the path flip, the
//!   divergent standalone, the auto-issue signature, delete-full,
//!   capability-dropped durations, expected-unmanaged, …).
//!
//! Unit tests per equivalence arm live in `src/info/diff.rs`; this
//! suite pins the arms against the LIVE engine's prints (the merge
//! suite's `converge` shape, applied to the comparator). Module is
//! absent from the corpus by design: the embedded build rejects
//! module DDL, so no exec corpus can carry
//! it.

#[path = "common/mod.rs"]
mod common;
#[path = "common/exec.rs"]
mod exec;

use std::collections::BTreeMap;

use surform::ast::SdlFile;
use surform::info::diff::{DriftClass, DriftFinding, diff};
use surform::info::normalize::{Family, Interchange, ReadModel, TABLE_CHILDREN_KEY};
use surform::info::read::{InfoObject, InfoValue, read_info_object};
use surform::info::report::report;
use surform::info::script::convergence_script;
use surrealdb::Surreal;
use surrealdb::engine::local::Db;

const NS: &str = "test";
const DB: &str = "scratch";

/// The read model over the session's three INFO scopes, with the
/// `INFO FOR TABLE` child maps assembled into the db object's reserved
/// [`TABLE_CHILDREN_KEY`] (the test twin of the interchange files the
/// CLI consumes).
async fn read_model_for(db: &Surreal<Db>, ns: &str, db_name: &str) -> ReadModel {
    let root_text = exec::info_for(db, "INFO FOR ROOT").await;
    let ns_text = exec::info_for(db, "INFO FOR NS").await;
    let db_text = exec::info_for(db, "INFO FOR DB").await;
    let root = read_info_object(&root_text).expect("root INFO walks");
    let ns_obj = read_info_object(&ns_text).expect("ns INFO walks");
    let mut db_obj = read_info_object(&db_text).expect("db INFO walks");
    let mut children = BTreeMap::new();
    if let Some(InfoValue::Object(tables)) = db_obj.0.get("tables") {
        for name in tables.0.keys() {
            // Quoted: a table name may carry any bytes (the hostile
            // name leg below) — the unquoted interpolation would
            // itself be the injection the suite guards against.
            let text = exec::info_for(db, &format!("INFO FOR TABLE `{name}`")).await;
            let obj = read_info_object(&text).expect("table INFO walks");
            children.insert(name.clone(), InfoValue::Object(obj));
        }
    }
    db_obj.0.insert(
        TABLE_CHILDREN_KEY.to_string(),
        InfoValue::Object(InfoObject(children)),
    );
    let namespaces = vec![(ns.to_string(), &ns_obj)];
    let databases = vec![(ns.to_string(), db_name.to_string(), &db_obj)];
    ReadModel::from_interchange(Interchange {
        root: Some(&root),
        namespaces: &namespaces,
        databases: &databases,
    })
    .expect("every scope reads")
}

/// Applies `sdl` to a fresh engine and returns (file, model).
async fn applied(sdl: &str) -> (SdlFile, ReadModel) {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(&db, sdl).await;
    let model = read_model_for(&db, NS, DB).await;
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{sdl}: {diags:?}");
    (file, model)
}

fn assert_empty(findings: &[DriftFinding], sdl: &str) {
    assert!(
        findings.is_empty(),
        "clean-apply must diff clean:\n--- SDL ---\n{sdl}\n--- findings ---\n{findings:#?}"
    );
}

/// The convergence corpus: one SDL document per surface, exercising
/// the comparator's resolution against the live printer's
/// materializations (evaluated-state clauses hold literal terminals
/// only — INFO stores evaluated state).
async fn assert_converges(sdl: &str) {
    let (file, model) = applied(sdl).await;
    let findings = diff(&file, &model);
    assert_empty(&findings, sdl);
    // The renderers agree the state is clean: the report is
    // empty and the convergence script emits nothing.
    assert!(
        report(&findings).is_empty(),
        "clean-apply must report nothing: {sdl}"
    );
    assert!(
        convergence_script(&findings, &file).is_empty(),
        "clean-apply must converge to nothing: {sdl}"
    );
}

fn class_of<'a>(findings: &'a [DriftFinding], family: Family, key: &str) -> Option<&'a DriftClass> {
    findings
        .iter()
        .find(|finding| finding.family == family && finding.key == key)
        .map(|finding| &finding.class)
}

// ---------------------------------------------------------------------
// The convergence corpus: clean-apply ⇒ empty diff
// ---------------------------------------------------------------------

#[tokio::test]
async fn corpus_converges() {
    const DOCS: &[&str] = &[
        // The simple five + scopes.
        "namespace test;
         database scratch;
         bucket att { backend: 'memory'; readonly; };
         sequence s = 5;
         sequence t { start: 5; batch: 100; timeout: 1h30m; };
         param $p = 1;
         param $q { value: 'x'; permissions: $auth.id == 'a'; };",
        // Users at two levels (the secret skips; roles/durations elide).
        "namespace test;
         database scratch;
         user root_owner { on: root; password: 'hunter2'; roles: owner; };
         user db_view { on: db; password: 'hunter2'; };",
        // Analyzer (the snowball alias rides Authored).
        "namespace test;
         database scratch;
         analyzer arts { tokenizers: class, blank; filters: ascii, snowball(eng); };",
        // Functions (single + multi-statement bodies, args, returns).
        "namespace test;
         database scratch;
         fn one($a: int) { return $a; };
         fn multi($a: int) -> int { let $x = $a + 1; return $x; };",
        // Configs (all three variants; the resolved graphql state).
        "namespace test;
         database scratch;
         config default { namespace: 'test'; database: 'scratch'; };
         config graphql { tables { include: t, post; }; depth: 8; };
         config api { permissions: $auth.role == 'owner'; };",
        // The table family: kinds, nested children (object kinds, the
        // synthesized `.*` fold), relations (the in/out fold), a view —
        // plus the 3.3 shapes: a LIGHTWEIGHT relation and the
        // INLINE cache caps (clean-apply ⇒ empty diff through the
        // header's own implication).
        "namespace test;
         database scratch;
         table post;
         table user;
         table t {
           schemaless;
           field x: { a: int, 'b': string, 3: number };
           field tags: array<string>;
           field dotted.chain: int;
           field r: record<user> { reference { on delete: cascade; }; };
           index unique ux { fields: x.a; };
           index count ct;
           event audited { when: x.a > 1; then: (create audited set at = time::now()); };
         };
         table user -> wrote -> post { field weight: number { inline; }; };
         table lw_a;
         table lw_b;
         table lw_a -> likes -> lw_b lightweight { inline edges: 8; };
         table capd { inline references: 16; };
         table view post_stats { select author, count() as posts from post group by author; };",
        // Mixins: the differ expands the SDL side itself —
        // the engine holds the spread members as concrete definitions
        // and the authored file converges against them (index members
        // included — the mixin carries the field its index names;
        // last-wins collision and the explicit overshadow ride the
        // expansion).
        "namespace test;
         database scratch;
         mixin stamp { slug: string; created_at: datetime; index slug_u { fields: slug; concurrently; }; };
         mixin alt { created_at: string; };
         table article schemafull {
           ...stamp;
           ...alt;
         };
         table comment schemafull { ...stamp; };",
        // Index variants at their vector defaults.
        "namespace test;
         database scratch;
         table vec { schemaless;
           field v: array<float>;
           index hnsw h { fields: v; dimension: 4; };
           index diskann d { fields: v; dimension: 4; };
         };",
        // Access: url verify with the capability-dropped TOKEN side, a
        // sentinel key verify, a jwt-less record (the auto-issue
        // signature converges with absence) — and the 3.3 clauses
        // AUDIENCE on the jwt source, CONTEXT on a record.
        "namespace test;
         database scratch;
         access url_access { on: db; type: jwt; url: 'https://u.example/jwks'; audience: 'aud1', 'aud2'; duration { token: 2h; }; };
         access key_access { on: db; type: jwt; algorithm: hs512; key: '[REDACTED]'; };
         access rec { on: db; type: record; signin: select * from type::record('user', $auth.id); context: { tier: 'gold' }; };",
        // Api: groups, the absent-`any` fold, a static + param path —
        // and a GROUP-level `permissions: full` (the printer always
        // materializes it and the reader elides it, so the authored
        // `full` is the spelling of absence; the dogfood tour's
        // catch — the fold rode only the `any` block before).
        "namespace test;
         database scratch;
         api '/health' { any { permissions: full; }; get { then: { status: 200 }; }; };
         api '/user/:id' { post { permissions: full; then: { status: 201 }; }; };",
        // Table delete-full is one-directional by construction.
        "namespace test;
         database scratch;
         table guarded { permissions { delete: full; }; };",
    ];
    for sdl in DOCS {
        assert_converges(sdl).await;
    }
}

/// The 3.3 PASSSCRAM user: the verifier is a secret with NO
/// read-side trace at all — INFO presence-signals the sentinel, the
/// reader strips the clause before validation, the differ skips the
/// slot — so a scram-authored file converges against the live
/// database exactly like a pass-authored one (and a scram + passhash
/// pair converges with both slots skipped).
#[tokio::test]
async fn passscram_user_converges() {
    let kat = common::menus::SCRAM_KAT;
    assert_converges(&format!(
        "namespace test;\n database scratch;\n \
         user provisioned {{ on: root; passscram: '{kat}'; }};\n \
         user migrator {{ on: db; passhash: '$argon2id$'; passscram: '{kat}'; }};"
    ))
    .await;
}

// ---------------------------------------------------------------------
// Drift fixtures — one live leg per class
// ---------------------------------------------------------------------

/// The `config default` none-sides ≡ absence arm (the full-product
/// oracle's catch, pinned per-commit): an all-`none` statement
/// stores nothing in the engine, so it converges against an empty
/// `defaults` map — while a REAL stored default still drifts against
/// a `none`-sided file (the fold never hides genuine drift).
#[tokio::test]
async fn config_default_none_sides_converge_with_absence() {
    let (file, model) =
        applied("namespace test;\n database scratch;\n config default { database: none; };").await;
    assert_empty(&diff(&file, &model), "all-none ≡ nothing stored");

    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test;\n database scratch;\n config default { namespace: 'real'; };",
    )
    .await;
    let model = read_model_for(&db, NS, DB).await;
    let (file, diags) = surform::parser::parse_file(
        "namespace test;\n database scratch;\n config default { database: none; };",
    );
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    assert_eq!(findings.len(), 1, "a real default still drifts");
    assert!(matches!(findings[0].class, DriftClass::Drift { .. }));
}

#[tokio::test]
async fn clause_delta_drifts() {
    // The database's param is 1; the file now says 2.
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(&db, "namespace test; database scratch; param $p = 1;").await;
    let model = read_model_for(&db, NS, DB).await;
    let (file, diags) =
        surform::parser::parse_file("namespace test; database scratch; param $p = 2;");
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    assert!(
        matches!(
            class_of(&findings, Family::Param, "p"),
            Some(DriftClass::Drift { .. })
        ),
        "expected param drift, found {findings:#?}"
    );
}

/// Mixin-carrying files diff at the MEMBER level (spec/mixin.md's
/// reverse-path rule): the differ expands internally, so a
/// diverged member is drift ON THAT STATEMENT — never a mixin-level
/// finding — and a member the database lacks is apply-pending on the
/// member. The convergence script defines that member, not the mixin
/// statement.
#[tokio::test]
async fn spread_members_drift_as_definitions() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         mixin stamp { created_at: datetime; };
         table entry schemafull { ...stamp; title: string; };",
    )
    .await;
    let model = read_model_for(&db, NS, DB).await;
    // A diverged member: drift on the field, nothing at the mixin.
    let (file, diags) = surform::parser::parse_file(
        "namespace test; database scratch;
         mixin stamp { created_at: int; };
         table entry schemafull { ...stamp; title: string; };",
    );
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    assert!(
        matches!(
            class_of(&findings, Family::Field, "created_at"),
            Some(DriftClass::Drift { .. })
        ),
        "expected member drift, found {findings:#?}"
    );
    assert!(
        !findings
            .iter()
            .any(|finding| finding.family == Family::Mixin),
        "never a mixin-level finding: {findings:#?}"
    );
    // An SDL-only member: apply-pending on the field (the spread
    // carries it), and the script defines the member.
    let (file, diags) = surform::parser::parse_file(
        "namespace test; database scratch;
         mixin stamp { created_at: datetime; extra: bool; };
         table entry schemafull { ...stamp; title: string; };",
    );
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    assert!(
        matches!(
            class_of(&findings, Family::Field, "extra"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the member apply-pending, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    assert!(
        script.contains("DEFINE FIELD OVERWRITE extra ON entry TYPE bool;"),
        "script: {script}"
    );
}

#[tokio::test]
async fn cap_and_lightweight_deltas_drift() {
    // The INLINE caps and the LIGHTWEIGHT flag compare
    // through the struct spread (the differ needs no dedicated arm) —
    // a changed cap and a dropped flag both drift.
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         table plain;
         table a -> likes -> b lightweight { inline edges: 8; };",
    )
    .await;
    let model = read_model_for(&db, NS, DB).await;
    let (file, diags) = surform::parser::parse_file(
        "namespace test; database scratch;
         table plain;
         table a -> likes -> b lightweight { inline edges: 16; };",
    );
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "likes"),
            Some(DriftClass::Drift { .. })
        ),
        "expected cap drift on `likes`, found {findings:#?}"
    );
    // The flag itself: a plain relation where the database holds a
    // LIGHTWEIGHT one.
    let (file, diags) = surform::parser::parse_file(
        "namespace test; database scratch;
         table plain;
         table a -> likes -> b { inline edges: 8; enforced; };",
    );
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "likes"),
            Some(DriftClass::Drift { .. })
        ),
        "expected flag drift on `likes`, found {findings:#?}"
    );
}

#[tokio::test]
async fn inline_field_delta_drifts() {
    // The INLINE flag rides the field's struct equality (no
    // dedicated arm) — an un-inlined field where the database holds an
    // inlined one drifts, and the baseline converges (the flag is
    // representable on both sides, unlike the capability-dropped
    // access durations).
    let (file, model) = applied(
        "namespace test; database scratch;
         table a -> likes -> b { field score: number { inline; }; };",
    )
    .await;
    assert_empty(&diff(&file, &model), "the inlined field converges");

    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         table a -> likes -> b { field score: number { inline; }; };",
    )
    .await;
    let model = read_model_for(&db, NS, DB).await;
    let (file, diags) = surform::parser::parse_file(
        "namespace test; database scratch;
         table a -> likes -> b { field score: number; };",
    );
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    assert!(
        matches!(
            class_of(&findings, Family::Field, "score"),
            Some(DriftClass::Drift { .. })
        ),
        "expected inline-flag drift on the `score` field, found {findings:#?}"
    );
}

/// A scope no tag presented supports no claim: the SDL's root-level
/// user and its namespace/database statements produce NO apply-pending
/// findings when only the db scope was ingested — while the ingested
/// db scope's own state still compares (an EMPTY ingested scope does
/// classify: `push_scope` tracks scopes even when their INFO was
/// empty, which is what separates "examined and absent" from "never
/// examined").
#[tokio::test]
async fn a_scope_never_ingested_contributes_nothing() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let sdl_text = "namespace test; database scratch;
                    user root_owner { on: root; password: 'hunter2'; roles: owner; };
                    table t;";
    exec::exec_sdl(&db, sdl_text).await;
    // The model ingests ONLY the db scope.
    let db_text = exec::info_for(&db, "INFO FOR DB").await;
    let db_obj = read_info_object(&db_text).expect("db INFO walks");
    let mut children = BTreeMap::new();
    if let Some(InfoValue::Object(tables)) = db_obj.0.get("tables") {
        for name in tables.0.keys() {
            let text = exec::info_for(&db, &format!("INFO FOR TABLE {name}")).await;
            let obj = read_info_object(&text).expect("table INFO walks");
            children.insert(name.clone(), InfoValue::Object(obj));
        }
    }
    let mut db_obj = db_obj;
    db_obj.0.insert(
        TABLE_CHILDREN_KEY.to_string(),
        InfoValue::Object(InfoObject(children)),
    );
    let namespaces: Vec<(String, &InfoObject)> = Vec::new();
    let databases = vec![(NS.to_string(), DB.to_string(), &db_obj)];
    let model = ReadModel::from_interchange(Interchange {
        root: None,
        namespaces: &namespaces,
        databases: &databases,
    })
    .expect("the db scope reads");
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    assert!(
        findings.is_empty(),
        "the un-ingested scopes must contribute nothing: {findings:#?}"
    );
}

#[tokio::test]
async fn database_only_and_sdl_only() {
    // The database holds `extra`; the file holds `pending`.
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(&db, "namespace test; database scratch; table extra;").await;
    let model = read_model_for(&db, NS, DB).await;
    let (file, diags) =
        surform::parser::parse_file("namespace test; database scratch; table pending;");
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    match class_of(&findings, Family::Table, "extra") {
        Some(DriftClass::DatabaseOnly { on_table, .. }) => {
            assert_eq!(on_table.as_deref(), None);
        }
        other => panic!("expected extra database-only, found {other:?} — {findings:#?}"),
    }
    assert!(
        matches!(
            class_of(&findings, Family::Table, "pending"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected pending SDL-only, found {findings:#?}"
    );
}

/// The auto-seeded default namespace — created by the server, never
/// by SDL — is informational, and it is the only finding. The
/// embedded engine's own hop seeds a bare namespace (probe-pinned),
/// so the leg plants the exact fingerprint text the server's
/// auto-seed writes (the `fingerprint_marks_a_defined_namespace`
/// precedent in tests/info_read.rs).
#[tokio::test]
async fn the_auto_seeded_namespace_is_expected_unmanaged() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    db.query("DEFINE NAMESPACE other COMMENT 'Default namespace generated by SurrealDB';")
        .await
        .expect("the query runs")
        .check()
        .expect("the fingerprint namespace defines");
    exec::exec_sdl(&db, "namespace test; database scratch;").await;
    let model = read_model_for(&db, NS, DB).await;
    let (file, diags) = surform::parser::parse_file("namespace test; database scratch;");
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    assert_eq!(
        findings.len(),
        1,
        "expected exactly one finding: {findings:#?}"
    );
    assert!(
        matches!(
            class_of(&findings, Family::Namespace, "other"),
            Some(DriftClass::ExpectedUnmanaged { .. })
        ),
        "expected expected-unmanaged, found {findings:#?}"
    );
}

/// The path flip: the escape-authored api prints flipped, pairs by
/// loose route key, and drifts with the SDL-side path named.
#[tokio::test]
async fn the_path_flip_drifts() {
    let (file, model) = applied(
        "namespace test; database scratch; api '/\\\\:x' { get { then: { status: 200 }; }; };",
    )
    .await;
    let findings = diff(&file, &model);
    assert!(
        matches!(
            class_of(&findings, Family::Api, "/\\:x"),
            Some(DriftClass::Drift { .. })
        ),
        "expected the flip to drift, found {findings:#?}"
    );
}

/// The divergent standalone: a top-level field diverging from its
/// table's nested twin drifts against the twin (a converged standalone
/// never survives placement — the corpus covers the folded form).
#[tokio::test]
async fn the_divergent_standalone_drifts_against_its_twin() {
    let (file, model) = applied(
        "namespace test; database scratch; table t { field x: int; }; field x: string { on: t; };",
    )
    .await;
    let findings = diff(&file, &model);
    assert!(
        matches!(
            class_of(&findings, Family::Field, "x"),
            Some(DriftClass::Drift { .. })
        ),
        "expected the standalone to drift against its twin, found {findings:#?}"
    );
}

/// The sentinel-key access converges live (the read adopts the
/// sentinel; both sides spell `'[REDACTED]'`), and a jwt-less record's
/// auto-issue signature converges with the authored absence — while a
/// real jwt block (an asymmetric URL issuer) still drifts against it.
#[tokio::test]
async fn sentinel_and_auto_issue() {
    assert_converges(
        "namespace test;
         database scratch;
         access key_access { on: db; type: jwt; algorithm: hs512; key: '[REDACTED]'; };
         access rec { on: db; type: record; signin: select * from type::record('user', $auth.id); };",
    )
    .await;
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         access rec { on: db; type: record; signin: select * from type::record('user', $auth.id);
                      jwt { url: 'https://u.example/jwks'; }; };",
    )
    .await;
    let model = read_model_for(&db, NS, DB).await;
    let (file, diags) = surform::parser::parse_file(
        "namespace test; database scratch; access rec { on: db; type: record; };",
    );
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    assert!(
        matches!(
            class_of(&findings, Family::Access, "rec"),
            Some(DriftClass::Drift { .. })
        ),
        "expected the real jwt block to drift against absence, found {findings:#?}"
    );
}

/// A database-only table child carries the host table's ON edge (the
/// REMOVE script's child-before-parent order).
#[tokio::test]
async fn database_only_children_carry_the_on_edge() {
    let (_, model) = applied("namespace test; database scratch; table t { field a: int; };").await;
    // The file's table now omits `a` — the database-only child names t.
    let (file, diags) = surform::parser::parse_file("namespace test; database scratch; table t;");
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    match class_of(&findings, Family::Field, "a") {
        Some(DriftClass::DatabaseOnly { on_table, .. }) => {
            assert_eq!(on_table.as_deref(), Some("t"));
        }
        other => panic!("expected child a database-only ON t, found {other:?} — {findings:#?}"),
    }
}

/// A hand-run untyped field (the real `FieldUntyped` route)
/// surfaces top-level as raw; its database-only finding carries the
/// ON edge the REMOVE script needs (probe-pinned: `raw_dep_names`'s
/// placement subset has no FIELD arm — the differ's own reader does).
#[tokio::test]
async fn raw_children_carry_the_on_edge() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(&db, "namespace test; database scratch; table t;").await;
    db.query("DEFINE FIELD f ON t;")
        .await
        .expect("the query runs")
        .check()
        .expect("the hand-run field defines");
    let model = read_model_for(&db, NS, DB).await;
    let (file, diags) = surform::parser::parse_file("namespace test; database scratch; table t;");
    assert!(diags.is_empty());
    let findings = diff(&file, &model);
    match class_of(&findings, Family::Field, "f") {
        Some(DriftClass::DatabaseOnly { on_table, .. }) => {
            assert_eq!(on_table.as_deref(), Some("t"));
        }
        other => panic!("expected raw field f database-only ON t, found {other:?} — {findings:#?}"),
    }
}

// ---------------------------------------------------------------------
// The renderers: the report, the REMOVE script, and
// the convergence loop they close
// ---------------------------------------------------------------------

/// The layer-2 leg over the script's whole emission vocabulary — the
/// source-verified REMOVE grammar inventory must parse, capability
/// gates included (`REMOVE BUCKET`/`REMOVE MODULE` are gated *inside*
/// the parser). The DEFINE OVERWRITE side is codegen's own output
/// (the full-corpus oracle's fixed point), so this leg pins the
/// removal families.
#[test]
fn layer2_accepts_the_remove_grammar_inventory() {
    let script = "\
REMOVE NAMESPACE IF EXISTS stray;
REMOVE DATABASE IF EXISTS stale;
REMOVE TABLE IF EXISTS `user`;
REMOVE EVENT IF EXISTS e ON `user`;
REMOVE FIELD IF EXISTS a.b ON t;
REMOVE INDEX IF EXISTS ix ON t;
REMOVE ANALYZER IF EXISTS `english`;
REMOVE SEQUENCE IF EXISTS s;
REMOVE PARAM IF EXISTS $x;
REMOVE PARAM IF EXISTS $`weird name`;
REMOVE FUNCTION IF EXISTS fn::a::b;
REMOVE FUNCTION IF EXISTS fn::`a b`::c;
REMOVE API IF EXISTS '/old path';
REMOVE BUCKET IF EXISTS b;
REMOVE USER IF EXISTS legacy ON ROOT;
REMOVE USER IF EXISTS deployer ON NAMESPACE;
REMOVE ACCESS IF EXISTS old ON DATABASE;
REMOVE CONFIG IF EXISTS DEFAULT;
REMOVE CONFIG IF EXISTS GRAPHQL;
REMOVE CONFIG IF EXISTS API;
";
    common::assert_oracle_accepts_files(script);
}

/// `REMOVE MODULE` parses under the surrealism CAPABILITY (the parser
/// setting derives from the capabilities, ours enabled) — unlike
/// DEFINE MODULE's unconditional cargo-feature bail).
/// The script's layer-2 leg covers the family.
#[test]
fn layer2_accepts_remove_module_under_the_capability() {
    common::assert_oracle_accepts_files("REMOVE MODULE IF EXISTS mod::color;");
}

/// The convergence script's whole shape parses: the transaction wrap
/// the transaction probe pinned (DDL rides BEGIN/COMMIT), the define
/// inventory (codegen's own output), the WARNING anchor comments,
/// the REQUIRED slot markers with a FILLED query line between them
/// (what the user's edit produces — an uncommented statement inside
/// the markers), and the removal inventory — one runnable file.
#[test]
fn layer2_accepts_the_convergence_script_shape() {
    common::assert_oracle_accepts_files(
        "\
BEGIN TRANSACTION;
-- generated by `surform migrate generate` - the
--    convergence script.
-- ------------------------------------------------------------
-- ## Removals - objects the schema file no longer declares
--
-- Each removal is idempotent (IF EXISTS): a re-run skips it.
-- >>> SURFORM WARNING: DATA LOSS <<<
-- Removes table extra - every document in it is deleted.
-- >>> SURFORM REQUIRED: DATA MIGRATION <<<
INSERT INTO archive (SELECT * FROM `user`);
-- >>> SURFORM REQUIRED: END <<<
USE NS acme DB shop;
DEFINE TABLE OVERWRITE `user` TYPE NORMAL SCHEMAFULL;
DEFINE FIELD OVERWRITE x ON `user` TYPE int;
DEFINE PARAM OVERWRITE $p VALUE 1;
DEFINE FUNCTION OVERWRITE fn::a::b($x: int) { return $x; };
REMOVE FIELD IF EXISTS gone ON extra;
REMOVE TABLE IF EXISTS extra;
COMMIT;
",
    );
}

/// The convergence loop, end to end against the live engine: drift
/// fixtures → the emitted script defines the SDL side (the drifted
/// param) and removes the database-only extras (children before their
/// table), wrapped in the one transaction (the transaction probe's
/// verdict, pinned here by execution: DDL rides BEGIN/COMMIT) → the re-captured
/// INFO re-diffs clean.
#[tokio::test]
async fn the_convergence_script_converges_the_database() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    // The database state: the SDL's older param, a field the SDL's
    // kept table no longer declares (its own finding — the host
    // matched), and a table the SDL drops entirely (whose children
    // ride inside its own finding — the table removal implies them).
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; param $p = 1;
         table extra { field gone: int; }; table legacy;",
    )
    .await;
    let sdl_text = "namespace test; database scratch; param $p = 2; table extra;";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty());
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Param, "p"),
            Some(DriftClass::Drift { .. })
        ),
        "expected the param to drift: {findings:#?}"
    );
    match (
        class_of(&findings, Family::Field, "gone"),
        class_of(&findings, Family::Table, "legacy"),
    ) {
        (
            Some(DriftClass::DatabaseOnly { on_table, .. }),
            Some(DriftClass::DatabaseOnly { .. }),
        ) => assert_eq!(on_table.as_deref(), Some("extra")),
        other => panic!("expected the extras database-only, found {other:?}"),
    }
    let script = convergence_script(&findings, &file);
    common::assert_oracle_accepts_files(&script);
    // The section order is deterministic: the scope's `USE` above
    // its frame, defines first (the drifted param from the SDL
    // side), removals last (Field before Table, each data-orphaning
    // removal under its anchor cluster — the field the REQUIRED slot
    // alone, the table the WARNING alarm with the REQUIRED slot
    // between it and the statement), one frame around the scope's
    // section.
    assert_eq!(
        script,
        format!(
            "{}\
             {}\
             \n\
             USE NS test DB scratch;\n\
             \n\
             {}\n\
             -- ## Scope db:test/scratch\n\
             -- section: db:test/scratch (1 define, 2 removals)\n\
             \n\
             BEGIN TRANSACTION;\n\
             \n\
             {}\n\
             -- ## Defines - the state the schema file describes\n\
             --\n\
             -- Each define carries OVERWRITE: a re-run redefines it.\n\
             \n\
             DEFINE PARAM OVERWRITE $p VALUE 2;\n\
             \n\
             {}\n\
             -- ## Removals - objects the schema file no longer declares\n\
             --\n\
             -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
             \n\
             -- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n\
             -- Removes field gone on table extra. The stored values remain\n\
             --    on the rows, outside every field definition.\n\
             -- - Write the move query between the markers; it runs before\n\
             --   the removal, in this same transaction.\n\
             -- e.g. UPDATE extra SET moved = gone;\n\
             -- - The values remain after this script; unset them once the\n\
             --   copy is verified (e.g. UPDATE extra UNSET gone;).\n\
             -- >>> SURFORM REQUIRED: END <<<\n\
             REMOVE FIELD IF EXISTS gone ON extra;\n\
             \n\
             -- >>> SURFORM WARNING: DATA LOSS <<<\n\
             -- Removes table legacy - every document in it is deleted.\n\
             \n\
             -- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n\
             -- - Write the export, archive, or move query between the\n\
             --   markers; it runs before the removal, in this same\n\
             --   transaction.\n\
             -- e.g. INSERT INTO archive (SELECT * FROM legacy);\n\
             -- >>> SURFORM REQUIRED: END <<<\n\
             REMOVE TABLE IF EXISTS legacy;\n\
             \n\
             COMMIT;\n",
            surform::info::script::HEADER,
            surform::info::script::summary_block(3, 1, 2, 2, 0, 1, 0),
            surform::info::script::RULE,
            surform::info::script::RULE,
            surform::info::script::RULE,
        ),
        "script: {script}"
    );
    // The UNFILLED script runs clean end-to-end — the slots are
    // text, so the default emission executes against the engine
    // unchanged (the filled-slot legs below).
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the migration applies");
    // The re-captured INFO re-diffs clean: the script CONVERGED the
    // database to the file in one run.
    let findings2 = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        findings2.is_empty(),
        "the loop must converge:\n--- findings ---\n{findings:#?}\n--- script ---\n{script}"
    );
}

/// The analyzer's catalog-integrity edge, executed: upstream refuses
/// `REMOVE ANALYZER` while a fulltext index still references it
/// (`AzInUse` — `IF EXISTS` does not soften it), so the script must
/// order the analyzer after its referencing table and index. The
/// convergence loop breaks if it does not.
#[tokio::test]
async fn the_analyzer_removes_after_its_references() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         analyzer arts { tokenizers: class; };
         table t { index fulltext ix { fields: title; analyzer: arts; }; };",
    )
    .await;
    // The SDL keeps the table (whose matched host makes the index a
    // separate finding carrying the ON edge) and drops the index and
    // the analyzer.
    let sdl_text = "namespace test; database scratch; table t;";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty());
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    let script = convergence_script(&findings, &file);
    let analyzer_at = script.find("REMOVE ANALYZER").expect("the analyzer remove");
    let index_at = script.find("REMOVE INDEX").expect("the index remove");
    assert!(
        index_at < analyzer_at,
        "the index must remove before the analyzer it references:\n{script}"
    );
    // Pre-fix ordering fails right here with `AzInUse`.
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the removals apply");
    exec::exec_sdl(&db, sdl_text).await;
    let findings2 = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        findings2.is_empty(),
        "the loop must converge: {findings2:#?}"
    );
}

// ---------------------------------------------------------------------
// The data-migration slots, executed
// ---------------------------------------------------------------------

/// The table-destruction slot, executed end to end: the database
/// holds `legacy` with rows; the SDL names `archive` (SDL-only — the
/// script's defines section creates it) and drops `legacy`. The slot
/// rides in the removals tail — AFTER the define it needs (the
/// between-sections placement: a move target the same script defines
/// is already in place) — and filling the slot with its own `e.g.`
/// line (uncommented) moves the rows out before the REMOVE destroys
/// the table. The re-captured INFO shows `legacy` gone and `archive`
/// holding both documents.
#[tokio::test]
async fn a_table_slot_moves_the_data_before_the_destruction() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(&db, "namespace test; database scratch; table legacy;").await;
    db.query("CREATE legacy SET x = 1; CREATE legacy SET x = 2;")
        .await
        .expect("the query runs")
        .check()
        .expect("the rows land");
    // The SDL keeps nothing the database holds and adds `archive` —
    // the migration's defines section creates the move target.
    let sdl_text = "namespace test; database scratch; table archive;";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty());
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "archive"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Table, "legacy"),
            Some(DriftClass::DatabaseOnly { .. })
        ),
        "expected archive SDL-only and legacy database-only: {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let define_at = script
        .find("DEFINE TABLE OVERWRITE archive")
        .expect("the defines section creates the move target");
    let slot_at = script
        .find(">>> SURFORM REQUIRED: DATA MIGRATION")
        .expect("the REQUIRED slot");
    let remove_at = script
        .find("REMOVE TABLE IF EXISTS legacy;")
        .expect("the removal");
    assert!(
        define_at < slot_at && slot_at < remove_at,
        "the slot rides between the sections — its move target defines \
         earlier in the same script:\n{script}"
    );
    assert!(script.contains("WARNING"), "the alarm rides too: {script}");
    // The user fills the slot: the example line, uncommented.
    let filled = script.replace(
        "-- e.g. INSERT INTO archive (SELECT * FROM legacy);",
        "INSERT INTO archive (SELECT * FROM legacy);",
    );
    assert_ne!(filled, script, "the example line is the fill template");
    db.query(&filled)
        .await
        .expect("the script runs")
        .check()
        .expect("the filled migration applies");
    // The move happened before the destruction: archive holds both
    // rows, legacy is gone from the catalog.
    use surrealdb::types::ToSql as _;
    let moved: surrealdb::types::Value = db
        .query("SELECT value x FROM archive ORDER BY x;")
        .await
        .expect("the read runs")
        .take(0)
        .expect("the read takes");
    assert_eq!(moved.to_sql(), "[1, 2]", "the rows moved: {moved:?}");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        !info.contains("legacy"),
        "the removal destroyed the table: {info}"
    );
    // The loop closes: the re-diff is clean.
    let findings2 = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        findings2.is_empty(),
        "the loop must converge: {findings2:#?}"
    );
}

/// The field-residue slot, executed: the database holds `t` with a
/// field `a` and rows in it; the SDL drops the field. The slot's
/// `e.g.` copy runs before the removal (`moved` holds the values),
/// and the engine-side residue claim is PROVEN — `REMOVE FIELD` is
/// catalog-only, so `a`'s values survive on the rows after the
/// migration — and the cleanup hint's `UNSET` query clears them.
#[tokio::test]
async fn a_field_slot_copies_the_values_and_the_residue_survives() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; table t { field a: int; };",
    )
    .await;
    db.query("CREATE t SET a = 7; CREATE t SET a = 9;")
        .await
        .expect("the query runs")
        .check()
        .expect("the rows land");
    let sdl_text = "namespace test; database scratch; table t;";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty());
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    match class_of(&findings, Family::Field, "a") {
        Some(DriftClass::DatabaseOnly { on_table, .. }) => {
            assert_eq!(on_table.as_deref(), Some("t"));
        }
        other => panic!("expected field a database-only ON t, found {other:?}"),
    }
    // The report marks the row and lists the obligation.
    let text = report(&findings);
    assert!(
        text.contains("\u{2212} db:test/scratch field a ON t (required data-migration slot)\n")
            && text.contains("== required data-migration slots =="),
        "the report carries the slot manifest: {text}"
    );
    let script = convergence_script(&findings, &file);
    let slot_at = script
        .find(">>> SURFORM REQUIRED: DATA MIGRATION")
        .expect("the REQUIRED slot");
    let remove_at = script
        .find("REMOVE FIELD IF EXISTS a ON t;")
        .expect("the removal");
    assert!(
        slot_at < remove_at,
        "the slot precedes the removal:\n{script}"
    );
    assert!(
        !script.contains(">>> SURFORM WARNING:"),
        "a catalog-only removal carries no alarm:\n{script}"
    );
    // The user fills the slot: the example copy, uncommented.
    let filled = script.replace("-- e.g. UPDATE t SET moved = a;", "UPDATE t SET moved = a;");
    assert_ne!(filled, script, "the example line is the fill template");
    db.query(&filled)
        .await
        .expect("the script runs")
        .check()
        .expect("the filled migration applies");
    use surrealdb::types::ToSql as _;
    let moved: surrealdb::types::Value = db
        .query("SELECT value moved FROM t ORDER BY moved;")
        .await
        .expect("the read runs")
        .take(0)
        .expect("the read takes");
    assert_eq!(moved.to_sql(), "[7, 9]", "the values copied: {moved:?}");
    // The RESIDUE claim, executed: the removal was catalog-only —
    // the dropped field's values survive on the rows.
    let residue: surrealdb::types::Value = db
        .query("SELECT value a FROM t ORDER BY a;")
        .await
        .expect("the read runs")
        .take(0)
        .expect("the read takes");
    assert_eq!(
        residue.to_sql(),
        "[7, 9]",
        "the values survive as schemaless residue: {residue:?}"
    );
    // The cleanup hint's query shape clears the residue.
    db.query("UPDATE t UNSET a;")
        .await
        .expect("the cleanup runs")
        .check()
        .expect("the cleanup applies");
    let cleaned: surrealdb::types::Value = db
        .query("SELECT value a FROM t;")
        .await
        .expect("the read runs")
        .take(0)
        .expect("the read takes");
    // The projection spells a now-missing field as NONE per row —
    // the residue is gone (the same projection read the plain values
    // above, pre-UNSET).
    assert_eq!(
        cleaned.to_sql(),
        "[NONE, NONE]",
        "the residue unset: {cleaned:?}"
    );
}

/// The vendored lexer's line-comment terminator set
/// (surrealdb-syn-3.3.0 `eat_single_line_comment`, the one eater
/// behind `--`, `//`, AND `#`): LF, CR, NEL — all control
/// characters — plus the Zl/Zp separators U+2028/U+2029, which ride
/// past an `is_control()` check.
fn is_line_comment_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// The executed legs' comment-text invariant: no `--` line ever
/// carries a raw control byte or line-comment terminator, whatever
/// the names hold. (The STATEMENT lines legitimately can — a
/// backtick ident accepts any bytes, and folding there would change
/// the name.)
fn assert_comment_lines_carry_no_controls(script: &str) {
    for line in script.lines().filter(|line| line.starts_with("--")) {
        assert!(
            line.chars()
                .all(|c| !c.is_control() && !is_line_comment_terminator(c)),
            "a comment-breaking character reached a comment line: {line}"
        );
    }
}

/// Executed: a database-side table name carrying
/// a raw newline plus a SHAPED SurrealQL tail. The vendored lexer
/// accepts any bytes inside backtick idents, and the INFO round trip
/// hands the raw name (the unescaped newline included) to the
/// script's comment prose — a raw interpolation would end the `--`
/// comment and execute the tail as SurrealQL inside the script's one
/// transaction. The rendered script must still parse and execute END
/// TO END — the tail stays inert comment text (folded to the visible
/// `\n` spelling) — and the injected `REMOVE TABLE victim` must NOT
/// run: the SDL-declared `victim` survives with its row, while the
/// real removal (backtick-escaped) destroys the hostile table.
#[tokio::test]
async fn a_hostile_name_stays_inert_comment_text() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    // The shaped name: a newline, then the injection,
    // then a trailing `--` that would comment out the prose remnant —
    // all of it ONE table name. It holds a row, so the slot's stakes
    // are real.
    let hostile = "legacy\nREMOVE TABLE victim;--";
    db.query(format!(
        "DEFINE TABLE `{hostile}`; CREATE `{hostile}` SET x = 1;"
    ))
    .await
    .expect("the hostile DDL runs")
    .check()
    .expect("the hostile table lands");
    // The injection's target: SDL-declared (so the migration itself
    // never removes it) and holding a row the injected REMOVE would
    // destroy.
    db.query("CREATE victim SET x = 5;")
        .await
        .expect("the victim lands")
        .check()
        .expect("the victim row lands");
    let sdl_text = "namespace test; database scratch; table victim;";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty());
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    match class_of(&findings, Family::Table, hostile) {
        Some(DriftClass::DatabaseOnly { .. }) => {}
        other => panic!("expected the hostile table database-only: {other:?}"),
    }
    let script = convergence_script(&findings, &file);
    // The prose folded the name: one comment line, the visible
    // `\n` spelling (the two-character form), the tail as inert text
    // (the folded name rides one wrap-word, so it never starts a
    // line).
    assert!(
        script
            .lines()
            .any(|line| line.starts_with("-- Removes table ")
                && line.contains("legacy\\nREMOVE TABLE victim;--")),
        "the slot prose must fold the name:\n{script}"
    );
    // The injected form — the tail starting its own line — never
    // appears, and no comment line carries a comment-breaking
    // character (the vendored terminator set
    // plus every other control byte).
    assert!(
        !script
            .lines()
            .any(|line| line.starts_with("REMOVE TABLE victim")),
        "the tail must stay inert:\n{script}"
    );
    assert_comment_lines_carry_no_controls(&script);
    // The legitimate removal spells the real name backtick-escaped
    // (the same visible `\n` spelling — inside the quotes, where the
    // parser unescapes it back).
    assert!(
        script.contains("REMOVE TABLE IF EXISTS `legacy\\nREMOVE TABLE victim;--`;"),
        "the removal targets the real name:\n{script}"
    );
    // THE PROOF: the whole script — hostile name and all — parses and
    // executes end to end. A broken comment would surface here as a
    // parse error or a failed statement (the transaction rolls back).
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the migration applies");
    // The injected REMOVE never ran: `victim` survives with its row,
    // while the REAL removal destroyed the hostile table.
    use surrealdb::types::ToSql as _;
    let victim: surrealdb::types::Value = db
        .query("SELECT value x FROM victim;")
        .await
        .expect("the read runs")
        .take(0)
        .expect("the read takes");
    assert_eq!(
        victim.to_sql(),
        "[5]",
        "the injection's target survived: {victim:?}"
    );
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        !info.contains("legacy") && info.contains("victim"),
        "the real removal ran, the injected one did not: {info}"
    );
    // The loop closes: the re-diff is clean.
    let findings2 = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        findings2.is_empty(),
        "the loop must converge:\n--- findings ---\n{findings2:#?}\n--- script ---\n{script}"
    );
}

/// The SAME shaped attack as the leg
/// above with U+2028 (LINE SEPARATOR) in place of the newline. The
/// vendored lexer breaks a `--` comment on it too
/// (`eat_single_line_comment`) — and it is Zl/Zp, NOT a control
/// character, so the existing `is_control()` arm let it through
/// verbatim and the tail lexed as SurrealQL inside the transaction.
/// The folded prose keeps the line one comment (the visible
/// `\u{2028}` spelling), the STATEMENT side keeps the raw character
/// inside the backticks (where `lex_surrounded_ident` accepts any
/// byte — folding there would change the name), the script parses
/// and executes END TO END, the injected REMOVE never runs, and the
/// re-diff is clean.
#[tokio::test]
async fn a_unicode_line_separator_name_stays_inert_comment_text() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    // The attack with the separator spelling: U+2028, then
    // the injection, then a trailing `--` that would comment out the
    // prose remnant — all of it ONE table name. It holds a row, so
    // the slot's stakes are real.
    let hostile = "legacy\u{2028}REMOVE TABLE victim;--";
    db.query(format!(
        "DEFINE TABLE `{hostile}`; CREATE `{hostile}` SET x = 1;"
    ))
    .await
    .expect("the hostile DDL runs")
    .check()
    .expect("the hostile table lands");
    // The injection's target: SDL-declared (so the migration itself
    // never removes it) and holding a row the injected REMOVE would
    // destroy.
    db.query("CREATE victim SET x = 5;")
        .await
        .expect("the victim lands")
        .check()
        .expect("the victim row lands");
    let sdl_text = "namespace test; database scratch; table victim;";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty());
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    match class_of(&findings, Family::Table, hostile) {
        Some(DriftClass::DatabaseOnly { .. }) => {}
        other => panic!("expected the hostile table database-only: {other:?}"),
    }
    let script = convergence_script(&findings, &file);
    // The prose folded the separator: one comment line, the visible
    // `\u{2028}` spelling (the literal backtick-u form), the tail as
    // inert text (the folded name rides one wrap-word).
    assert!(
        script
            .lines()
            .any(|line| line.starts_with("-- Removes table ")
                && line.contains("legacy\\u{2028}REMOVE TABLE victim;--")),
        "the slot prose must fold the separator:\n{script}"
    );
    // The injected form — the tail starting its own line — never
    // appears, and no comment line carries a comment-breaking
    // character.
    assert!(
        !script
            .lines()
            .any(|line| line.starts_with("REMOVE TABLE victim")),
        "the tail must stay inert:\n{script}"
    );
    assert_comment_lines_carry_no_controls(&script);
    // The legitimate removal spells the real name backtick-escaped —
    // the raw U+2028 rides VERBATIM inside the quotes, where the
    // surrounded-ident lexer accepts any byte and the parser
    // unescapes nothing (there is nothing to unescape).
    assert!(
        script.contains("REMOVE TABLE IF EXISTS `legacy\u{2028}REMOVE TABLE victim;--`;"),
        "the removal targets the real name:\n{script}"
    );
    // THE PROOF: the whole script — hostile name and all — parses and
    // executes end to end. A separator-broken comment would surface
    // here as a parse error or a failed statement (the transaction
    // rolls back).
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the migration applies");
    // The injected REMOVE never ran: `victim` survives with its row,
    // while the REAL removal destroyed the hostile table.
    use surrealdb::types::ToSql as _;
    let victim: surrealdb::types::Value = db
        .query("SELECT value x FROM victim;")
        .await
        .expect("the read runs")
        .take(0)
        .expect("the read takes");
    assert_eq!(
        victim.to_sql(),
        "[5]",
        "the injection's target survived: {victim:?}"
    );
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        !info.contains("legacy") && info.contains("victim"),
        "the real removal ran, the injected one did not: {info}"
    );
    // The loop closes: the re-diff is clean.
    let findings2 = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        findings2.is_empty(),
        "the loop must converge:\n--- findings ---\n{findings2:#?}\n--- script ---\n{script}"
    );
}

/// Pinned by execution (the reproducer, made permanent): the database
/// holds `likes` as a LIGHTWEIGHT
/// relation; the SDL promotes it to a regular relation carrying a
/// standalone-authored field. The child define must run after the
/// host's `OVERWRITE` (which clears the LIGHTWEIGHT form — vendored
/// define/field.rs refuses a field on a lightweight relation), or the
/// engine refuses and the whole transaction rolls back. The member→host
/// slot puts the child inside the host's emission — after the table
/// head; the script converges.
#[tokio::test]
async fn the_lightweight_promotion_defines_the_host_first() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    // The lightweight relation defines fresh (no edges — a promotion
    // refuses while edges exist).
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         table a; table b;
         table a -> likes -> b lightweight;",
    )
    .await;
    let sdl_text = "namespace test; database scratch;
         table a; table b;
         table a -> likes -> b;
         field x: int { on: likes; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "likes"),
            Some(DriftClass::Drift { .. })
        ),
        "expected the promotion drift, found {findings:#?}"
    );
    assert!(
        matches!(
            class_of(&findings, Family::Field, "x"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the standalone field apply-pending, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let table_at = script
        .find("DEFINE TABLE OVERWRITE likes")
        .expect("the host define");
    let field_at = script
        .find("DEFINE FIELD OVERWRITE x ON likes")
        .expect("the child define");
    assert!(table_at < field_at, "the host must define first:\n{script}");
    common::assert_oracle_accepts_files(&script);
    // Pre-fix ordering fails right here (the lightweight refusal
    // rolls the transaction back). The migration lands: the promoted
    // table and the field are in the catalog.
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the promotion migration applies");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("DEFINE TABLE likes TYPE RELATION IN a OUT b SCHEMALESS"),
        "the host promoted: {info}"
    );
    let table_info = exec::info_for(&db, "INFO FOR TABLE likes").await;
    assert!(
        table_info.contains("DEFINE FIELD x"),
        "the child landed: {table_info}"
    );
    // (The full re-diff-clean loop is the nested leg's job below: a
    // STANDALONE-authored child's clean-apply re-diff trips a
    // pre-existing differ vacuum — compare_children files a database
    // host's children against the SDL TABLE BLOCK only, so a
    // converged standalone folds in classify_unmatched while its twin
    // already went DatabaseOnly; not this test's fix.)
}

/// The same promotion with the field NESTED in the authored table:
/// the host finding's emission carries the member (the
/// dedup — the standalone field finding suppresses), so the script
/// renders the field ONCE, after the table head, and the transaction
/// converges.
#[tokio::test]
async fn the_nested_promotion_renders_the_member_once() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         table a; table b;
         table a -> likes -> b lightweight;",
    )
    .await;
    let sdl_text = "namespace test; database scratch;
         table a; table b;
         table a -> likes -> b { field x: int; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "likes"),
            Some(DriftClass::Drift { .. })
        ) && matches!(
            class_of(&findings, Family::Field, "x"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the host drift and the member apply-pending, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    assert_eq!(
        script.matches("DEFINE FIELD OVERWRITE x ON likes").count(),
        1,
        "the member renders once — inside the host's emission:\n{script}"
    );
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the promotion migration applies");
    let findings2 = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        findings2.is_empty(),
        "the loop must converge:\n--- findings2 ---\n{findings2:#?}\n--- script ---\n{script}"
    );
}

/// The auto-created host, executed: a standalone child whose host
/// carries NO define finding —
/// the table absent entirely — defines against the engine's
/// auto-created schemaless shell (vendored define/field.rs's
/// `get_or_add_tb`). The script carries no table define and executes
/// clean; the field lands on the host the engine created.
#[tokio::test]
async fn a_hostless_child_defines_against_the_auto_created_table() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let sdl_text = "namespace test; database scratch; field x: int { on: fresh; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Field, "x"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the field apply-pending, found {findings:#?}"
    );
    assert!(
        !findings
            .iter()
            .any(|finding| finding.family == Family::Table),
        "no table finding — the host is the engine's business: {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    assert!(
        script.contains("DEFINE FIELD OVERWRITE x ON fresh"),
        "{script}"
    );
    assert!(!script.contains("DEFINE TABLE"), "{script}");
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the hostless child defines");
    let info = exec::info_for(&db, "INFO FOR TABLE fresh").await;
    assert!(
        info.contains("DEFINE FIELD x"),
        "the field landed on the auto-created host: {info}"
    );
}

/// Pinned by execution (the reproducer, made
/// permanent): the database holds plain `t` — schemaless, no children;
/// the SDL nests a fulltext index naming the field in the authored
/// table and authors the field standalone. The table statement pins
/// the index→field edge (order.rs's table arm), and the table's
/// emission is ONE unit — so the field must ride between the head and
/// the nested index (after it, the index defines against a SCHEMAFULL
/// table that lacks the field: vendored define/index.rs hard-fails
/// `FdNotFound` and the transaction rolls back; before the head, the
/// LIGHTWEIGHT refusal of the sibling conjunction leg). The slot
/// places the child inside; the script converges.
#[tokio::test]
async fn a_nested_index_child_slots_inside_the_hosts_emission() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(&db, "namespace test; database scratch; table t;").await;
    let sdl_text = "namespace test; database scratch;
         table t { schemafull; index fulltext ix { fields: x; }; };
         field x: int { on: t; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "t"),
            Some(DriftClass::Drift { .. })
        ) && matches!(
            class_of(&findings, Family::Field, "x"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the host drift and the standalone field, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let table_at = script
        .find("DEFINE TABLE OVERWRITE t")
        .expect("the host head");
    let field_at = script
        .find("DEFINE FIELD OVERWRITE x ON t")
        .expect("the slotted child");
    let index_at = script
        .find("DEFINE INDEX OVERWRITE ix ON t")
        .expect("the nested member");
    assert!(
        table_at < field_at && field_at < index_at,
        "the child rides between the head and the naming index:\n{script}"
    );
    // The nested index renders once — inside the host's emission (the
    // finding is the host emission's, dropped standalone).
    assert_eq!(
        script.matches("DEFINE INDEX OVERWRITE ix").count(),
        1,
        "{script}"
    );
    common::assert_oracle_accepts_files(&script);
    // Pre-fix ordering (child after the whole host emission) fails
    // right here: `FdNotFound` rolls the transaction back.
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the migration applies");
    let info = exec::info_for(&db, "INFO FOR TABLE t").await;
    assert!(
        info.contains("DEFINE FIELD x") && info.contains("DEFINE INDEX ix"),
        "the child and the member landed: {info}"
    );
    // (Re-diff: the nested index converges; the standalone field is
    // the pre-existing differ vacuum —
    // compare_children files the db host's children against the SDL
    // TABLE BLOCK only. Catalog reads above carry the assertion.)
}

/// The compound precondition, executed: the database holds
/// the host as a LIGHTWEIGHT relation AND the SDL's authored table
/// nests an index naming a standalone field. No order of separate
/// statements satisfies both edges (child before the head → the
/// LIGHTWEIGHT refusal; after the head → the nested index's
/// `FdNotFound`), and none is needed: the slot rides the child inside
/// the host's emission, and the migration converges — the compound
/// corner resolves outright.
#[tokio::test]
async fn the_conjunction_lightweight_host_and_nested_index_converges() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         table a; table b;
         table a -> likes -> b lightweight;",
    )
    .await;
    let sdl_text = "namespace test; database scratch;
         table a; table b;
         table a -> likes -> b { schemafull; index fulltext ix { fields: x; }; };
         field x: int { on: likes; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "likes"),
            Some(DriftClass::Drift { .. })
        ) && matches!(
            class_of(&findings, Family::Field, "x"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the promotion drift and the standalone field, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let table_at = script
        .find("DEFINE TABLE OVERWRITE likes")
        .expect("the host head");
    let field_at = script
        .find("DEFINE FIELD OVERWRITE x ON likes")
        .expect("the slotted child");
    let index_at = script
        .find("DEFINE INDEX OVERWRITE ix ON likes")
        .expect("the nested member");
    assert!(
        table_at < field_at && field_at < index_at,
        "head (promotes), then the child, then the naming index:\n{script}"
    );
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the conjunction migration applies");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("DEFINE TABLE likes TYPE RELATION IN a OUT b SCHEMAFULL"),
        "the host promoted: {info}"
    );
    let table_info = exec::info_for(&db, "INFO FOR TABLE likes").await;
    assert!(
        table_info.contains("DEFINE FIELD x") && table_info.contains("DEFINE INDEX ix"),
        "the child and the member landed: {table_info}"
    );
}

/// Pinned by execution (the reproducers
/// A/J/K, made permanent): the database holds `likes` as a LIGHTWEIGHT
/// relation; the SDL promotes it AND authors the member as a RAW
/// define. The differ files the raw body's header as a typed-family
/// finding — `raw_match_key` reads `DEFINE FIELD …` as (Field, "x"),
/// which sorts before Table — so the unslotting raw kept its sort
/// position and rendered against the LIGHTWEIGHT host: the engine
/// refuses (vendored define/field.rs's lightweight ensure), and the
/// ONE transaction rolled the whole migration back. The raw member now
/// rides its header's `ON <table>` edge into the host's emission —
/// after the head — and the script converges.
#[tokio::test]
async fn a_raw_member_define_slots_inside_the_lightweight_promotion() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    // The lightweight relation defines fresh (no edges — a promotion
    // refuses while edges exist).
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         table a; table b;
         table a -> likes -> b lightweight;",
    )
    .await;
    let sdl_text = "namespace test; database scratch;
         table a; table b;
         table a -> likes -> b;
         raw { DEFINE FIELD OVERWRITE x ON likes TYPE int; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "likes"),
            Some(DriftClass::Drift { .. })
        ),
        "expected the promotion drift, found {findings:#?}"
    );
    assert!(
        matches!(
            class_of(&findings, Family::Field, "x"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the raw header filed as the field, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let table_at = script
        .find("DEFINE TABLE OVERWRITE likes")
        .expect("the host define");
    let raw_at = script
        .find("DEFINE FIELD OVERWRITE x ON likes TYPE int;")
        .expect("the raw member, verbatim");
    assert!(
        table_at < raw_at,
        "the promotion must run before the raw member:\n{script}"
    );
    common::assert_oracle_accepts_files(&script);
    // Pre-fix ordering fails right here (the LIGHTWEIGHT refusal rolls
    // the transaction back). The migration lands: the promoted table
    // and the raw-authored field are in the catalog.
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the raw-member promotion applies");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("DEFINE TABLE likes TYPE RELATION IN a OUT b SCHEMALESS"),
        "the host promoted: {info}"
    );
    let table_info = exec::info_for(&db, "INFO FOR TABLE likes").await;
    assert!(
        table_info.contains("DEFINE FIELD x"),
        "the raw member landed: {table_info}"
    );
}

/// The raw member's SPLICE, pinned by execution: the database holds
/// `t` SCHEMAFULL without the field; the SDL drifts the table
/// (permissions) carrying the field NESTED, and authors the index
/// RAW. The raw header files the index as (Index, "ix") — before
/// Table in the sorted set — so the unslotting raw rendered first and
/// `DEFINE INDEX` refused `FdNotFound` (vendored define/index.rs: the
/// field-existence check bails only `if tb.schemafull`). The slot
/// unfolds the host's emission — head, typed field, then the raw
/// index — so every field it could name is defined.
#[tokio::test]
async fn a_raw_index_member_splices_after_its_hosts_typed_field() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; table t { schemafull; };",
    )
    .await;
    let sdl_text = "namespace test; database scratch;
         table t { schemafull; permissions: full; field a: int; };
         raw { DEFINE INDEX OVERWRITE ix ON t FIELDS a; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "t"),
            Some(DriftClass::Drift { .. })
        ),
        "expected the table drift, found {findings:#?}"
    );
    assert!(
        matches!(
            class_of(&findings, Family::Index, "ix"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the raw header filed as the index, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let table_at = script
        .find("DEFINE TABLE OVERWRITE t")
        .expect("the host head");
    let field_at = script
        .find("DEFINE FIELD OVERWRITE a ON t")
        .expect("the typed member");
    let raw_at = script
        .find("DEFINE INDEX OVERWRITE ix ON t FIELDS a;")
        .expect("the raw index, verbatim");
    assert!(
        table_at < field_at && field_at < raw_at,
        "head, typed field, then the raw index:\n{script}"
    );
    // The nested field renders once — inside the host's emission.
    assert_eq!(
        script.matches("DEFINE FIELD OVERWRITE a ON t").count(),
        1,
        "{script}"
    );
    common::assert_oracle_accepts_files(&script);
    // Pre-fix ordering fails right here (`FdNotFound` rolls the
    // transaction back). The migration lands: field and index.
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the raw-index migration applies");
    let info = exec::info_for(&db, "INFO FOR TABLE t").await;
    assert!(
        info.contains("DEFINE FIELD a") && info.contains("DEFINE INDEX ix"),
        "the member and the raw index landed: {info}"
    );
}

/// Trigger A, pinned by execution (the reproducer, made permanent):
/// a TYPED view whose source table defines
/// as a RAW body. The defines partition runs the view first
/// ("v" < "zzz") and the typed FROM edge cannot resolve against a raw
/// target — order.rs's `def_ident` registers raw bodies as nothing —
/// so the view's `DEFINE TABLE … AS SELECT` rendered first and the
/// engine refused `TbNotFound` walking the view's `source_tables()`
/// (vendored define/table.rs), rolling the ONE transaction back. The
/// settle pass moves the view behind the raw source and the migration
/// lands. The swapped order's refusal is executed too, on a fresh
/// engine: the edge is load-bearing, not decorative.
#[tokio::test]
async fn a_view_defines_after_its_raw_defined_source() {
    // A fresh engine plays the swapped order: the view define precedes
    // its source and the whole transaction rolls back.
    let fresh = exec::mem_db().await;
    fresh.use_ns(NS).use_db(DB).await.ok();
    let swapped = "USE NS test DB scratch; BEGIN TRANSACTION; \
         DEFINE TABLE OVERWRITE v TYPE ANY SCHEMALESS AS SELECT author FROM zzz; \
         DEFINE TABLE OVERWRITE zzz SCHEMALESS; COMMIT;";
    let error = fresh
        .query(swapped)
        .await
        .expect("the query runs")
        .check()
        .expect_err("the view-before-source order refuses");
    let message = error.to_string();
    assert!(
        message.contains("zzz") || message.contains("failed transaction"),
        "the refusal names the missing source or the rollback: {message}"
    );
    let info = exec::info_for(&fresh, "INFO FOR DB").await;
    assert!(
        !info.contains("DEFINE TABLE v") && !info.contains("DEFINE TABLE zzz"),
        "the failed transaction left nothing behind: {info}"
    );

    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let sdl_text = "namespace test; database scratch;
         table view v { select author from zzz; };
         raw { DEFINE TABLE OVERWRITE zzz SCHEMALESS; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "v"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the typed view, found {findings:#?}"
    );
    assert!(
        matches!(
            class_of(&findings, Family::Table, "zzz"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the raw header filed as the table, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let zzz_at = script
        .find("DEFINE TABLE OVERWRITE zzz SCHEMALESS;")
        .expect("the raw source, verbatim");
    let v_at = script
        .find("DEFINE TABLE OVERWRITE v TYPE ANY SCHEMALESS AS SELECT author FROM zzz;")
        .expect("the typed view");
    assert!(zzz_at < v_at, "the source defines first:\n{script}");
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the view migration applies");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("DEFINE TABLE zzz") && info.contains("DEFINE TABLE v"),
        "the source and the view landed: {info}"
    );
}

/// The twin case, pinned by execution: a raw fulltext INDEX
/// and the analyzer it names, in one migration. The settle pass's
/// analyzer arm defends an index-before-analyzer order the assembly
/// cannot produce today (the defines partition puts Analyzer before
/// Index/Table, so the arm is unreachable through the sorted set —
/// the test `the_settle_pass_moves_a_raw_index_behind_its_
/// analyzer` exercises it on a hand-built order). The EXECUTED facts
/// around the pair, probed and narrated here (vendored syn
/// stmt/define.rs gives the body its `FULLTEXT ANALYZER <ident>`
/// spelling):
///
/// - the migration-time pair (fresh table, no records) lands inside
///   the one transaction — the fulltext build is DEFERRED on an empty
///   table (the index stores dangling), so the analyzer define only
///   needs to precede the index, which the family order guarantees;
/// - a record-holding table builds AT define — and the build reads
///   the PRE-TRANSACTION catalog: an analyzer the same transaction
///   defines is invisible to it (`The analyzer 'arts' does not
///   exist`), executed below in both orders. That class is not an
///   ordering failure — no order of the two defines repairs it — and
///   the script's one-transaction wrap carries it as an engine fact:
///   the repair is a converged (pre-existing) analyzer, which this
///   leg's second half pins end to end.
#[tokio::test]
async fn a_raw_fulltext_index_defines_after_its_analyzer() {
    // The same-transaction invisibility, executed in BOTH orders on
    // record-holding tables (the build resolves against the
    // pre-transaction catalog).
    for (label, ddl) in [
        (
            "analyzer-first",
            "BEGIN TRANSACTION; \
             DEFINE ANALYZER OVERWRITE arts TOKENIZERS CLASS; \
             DEFINE INDEX OVERWRITE ix ON t FIELDS a FULLTEXT ANALYZER arts; COMMIT;",
        ),
        (
            "index-first",
            "BEGIN TRANSACTION; \
             DEFINE INDEX OVERWRITE ix ON t FIELDS a FULLTEXT ANALYZER arts; \
             DEFINE ANALYZER OVERWRITE arts TOKENIZERS CLASS; COMMIT;",
        ),
    ] {
        let fresh = exec::mem_db().await;
        fresh.use_ns(NS).use_db(DB).await.ok();
        exec::exec_sdl(
            &fresh,
            "namespace test; database scratch; table t { schemafull; field a: string; };",
        )
        .await;
        fresh
            .query("USE NS test DB scratch; CREATE t SET a = 'hello';")
            .await
            .expect("the query runs")
            .check()
            .expect("the record seeds");
        let error = fresh
            .query(format!("USE NS test DB scratch; {ddl}"))
            .await
            .expect("the query runs")
            .check()
            .expect_err("the build cannot see a same-transaction analyzer");
        let message = error.to_string();
        assert!(
            message.contains("failed transaction") || message.contains("does not exist"),
            "{label}: {message}"
        );
    }

    // The migration-time pair over a FRESH table: the settle order
    // holds (analyzer define first), the transaction lands, the
    // deferred build picks the analyzer up.
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; table t { schemafull; field a: string; };",
    )
    .await;
    let sdl_text = "namespace test; database scratch;
         table t { schemafull; field a: string; };
         analyzer arts { tokenizers: class; };
         raw { DEFINE INDEX OVERWRITE ix ON t FIELDS a FULLTEXT ANALYZER arts; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Analyzer, "arts"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the analyzer, found {findings:#?}"
    );
    assert!(
        matches!(
            class_of(&findings, Family::Index, "ix"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the raw header filed as the index, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let arts_at = script
        .find("DEFINE ANALYZER OVERWRITE arts")
        .expect("the analyzer define");
    let raw_at = script
        .find("DEFINE INDEX OVERWRITE ix ON t FIELDS a FULLTEXT ANALYZER arts;")
        .expect("the raw index, verbatim");
    assert!(arts_at < raw_at, "the analyzer defines first:\n{script}");
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the analyzer migration applies");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("DEFINE ANALYZER arts"),
        "the analyzer landed: {info}"
    );
    // The build resolves a PRE-EXISTING analyzer at define: with the
    // analyzer converged (in the catalog before the transaction), the
    // record-holding table's build succeeds inside the transaction.
    // (Fresh engine: the analyzer and the table seed as TYPED SDL and
    // converge — a converged RAW index would re-diff as the differ's
    // raw-vs-typed twins — the differ's comparison semantics, not
    // this test's.)
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         table t { schemafull; field a: string; };
         analyzer arts { tokenizers: class; };",
    )
    .await;
    db.query("USE NS test DB scratch; CREATE t SET a = 'hello';")
        .await
        .expect("the query runs")
        .check()
        .expect("the record seeds");
    let sdl_text = "namespace test; database scratch;
         table t { schemafull; field a: string; };
         analyzer arts { tokenizers: class; };
         raw { DEFINE INDEX OVERWRITE iy ON t FIELDS a FULLTEXT ANALYZER arts; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Index, "iy"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the second raw index, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    assert!(
        script.contains("DEFINE INDEX OVERWRITE iy ON t FIELDS a FULLTEXT ANALYZER arts;"),
        "the raw index renders: {script}"
    );
    assert!(
        !script.contains("DEFINE ANALYZER OVERWRITE"),
        "the converged analyzer carries no define: {script}"
    );
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the build runs against the pre-existing analyzer");
    let table_info = exec::info_for(&db, "INFO FOR TABLE t").await;
    assert!(
        table_info.contains("DEFINE INDEX iy"),
        "the raw index landed: {table_info}"
    );
}

/// Trigger A, pinned by execution: a TYPED param
/// whose VALUE reads a param that defines as a RAW body. The family
/// partition runs `$a` first ("a" < "b" — same family) and the typed
/// VALUE edge cannot resolve against a raw target (`def_ident`
/// registers raw bodies as nothing), so the reader rendered first and
/// the define-time evaluation of `$b + 1` over the missing `$b`
/// errored — the ONE transaction rolled the whole migration back. The
/// settle pass's value-read arm moves `$a` behind `$b` and the
/// migration lands. The CATALOG VALUE is asserted: `$a` stores the
/// evaluated sum 3, which only the dependency-first order can
/// produce.
#[tokio::test]
async fn a_param_defines_after_its_raw_defined_dependency() {
    // A fresh engine plays the swapped order: the reader defines
    // first, the evaluation of `$b + 1` fails, the transaction rolls
    // back and leaves nothing.
    let fresh = exec::mem_db().await;
    fresh.use_ns(NS).use_db(DB).await.ok();
    let swapped = "USE NS test DB scratch; BEGIN TRANSACTION; \
         DEFINE PARAM OVERWRITE $a VALUE $b + 1; \
         DEFINE PARAM OVERWRITE $b VALUE 2; COMMIT;";
    let error = fresh
        .query(swapped)
        .await
        .expect("the query runs")
        .check()
        .expect_err("the reader-before-dependency order refuses");
    let message = error.to_string();
    assert!(
        message.contains("failed transaction")
            || message.contains("none")
            || message.contains("$b"),
        "the rollback (or the unbound param) is the refusal: {message}"
    );
    let info = exec::info_for(&fresh, "INFO FOR DB").await;
    assert!(
        !info.contains("DEFINE PARAM"),
        "the failed transaction left no param behind: {info}"
    );

    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let sdl_text = "namespace test; database scratch;
         param $a = $b + 1;
         raw { DEFINE PARAM OVERWRITE $b VALUE 2; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Param, "a"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the typed param, found {findings:#?}"
    );
    assert!(
        matches!(
            class_of(&findings, Family::Param, "b"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the raw header filed as the param, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let b_at = script
        .find("DEFINE PARAM OVERWRITE $b VALUE 2;")
        .expect("the raw param, verbatim");
    let a_at = script
        .find("DEFINE PARAM OVERWRITE $a VALUE $b + 1;")
        .expect("the typed param");
    assert!(b_at < a_at, "the dependency defines first:\n{script}");
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the param migration applies");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("VALUE 3"),
        "the catalog stores the EVALUATED sum ($b was 2 when $a defined): {info}"
    );
}

/// Trigger B, pinned by execution — the SILENT
/// class: an access whose jwt `url:` reads a param that defines as a
/// RAW body. Access sorts before Param, the raw body's own slots
/// registered nowhere, so the access rendered first and the
/// transaction COMMITTED — with the URL stored as the literal string
/// `'NONE'` (row 9's recorded behavior). No error, no rollback: the
/// migration "succeeds" wrong, which makes ordering the only defense.
/// The settle pass moves the raw access behind the param, and the
/// catalog is asserted to hold the RESOLVED URL.
#[tokio::test]
async fn an_access_url_resolves_its_param_in_the_catalog() {
    // A fresh engine plays the swapped order — and it LANDS, silently
    // wrong: the URL stores `'NONE'`. This is the class's first
    // silent instance (no rollback announces it).
    let fresh = exec::mem_db().await;
    fresh.use_ns(NS).use_db(DB).await.ok();
    let swapped = "USE NS test DB scratch; BEGIN TRANSACTION; \
         DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $p; \
         DEFINE PARAM OVERWRITE $p VALUE 'https://x/jwks'; COMMIT;";
    fresh
        .query(swapped)
        .await
        .expect("the query runs")
        .check()
        .expect("the reader-before-dependency order SILENTLY lands");
    let info = exec::info_for(&fresh, "INFO FOR DB").await;
    assert!(
        info.contains("URL 'NONE'"),
        "the swapped order stores the literal NONE — the silent wrong value: {info}"
    );

    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let sdl_text = "namespace test; database scratch;
         access ju { on: db; type: jwt; url: $p; };
         raw { DEFINE PARAM OVERWRITE $p VALUE 'https://x/jwks'; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Access, "ju"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Param, "p"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the raw-filed access and the param, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let p_at = script
        .find("DEFINE PARAM OVERWRITE $p")
        .expect("the param define");
    let ju_at = script
        .find("DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $p")
        .expect("the raw access, verbatim");
    assert!(p_at < ju_at, "the param defines first:\n{script}");
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the access migration applies");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("URL 'https://x/jwks'"),
        "the catalog holds the RESOLVED url, not 'NONE': {info}"
    );
    assert!(
        !info.contains("URL 'NONE'"),
        "no silent NONE anywhere: {info}"
    );
}

/// The hard-coercion sibling, pinned by execution: a
/// bucket whose `backend:` reads a param that defines as a RAW body.
/// Bucket sorts before Param and the computed backend
/// (`expr_compute(...).coerce_to::<String>()?` — vendored
/// define/bucket.rs) hard-errors over the missing param, rolling the
/// ONE transaction back. The settle pass moves the bucket behind the
/// param; the catalog is asserted to hold the RESOLVED backend
/// string.
#[tokio::test]
async fn a_bucket_backend_resolves_its_param_in_the_catalog() {
    // A fresh engine plays the swapped order: the coercion over the
    // missing param errors, the transaction rolls back.
    let fresh = exec::mem_db().await;
    fresh.use_ns(NS).use_db(DB).await.ok();
    let swapped = "USE NS test DB scratch; BEGIN TRANSACTION; \
         DEFINE BUCKET OVERWRITE b BACKEND $be; \
         DEFINE PARAM OVERWRITE $be VALUE 'memory'; COMMIT;";
    let error = fresh
        .query(swapped)
        .await
        .expect("the query runs")
        .check()
        .expect_err("the reader-before-dependency order refuses");
    let message = error.to_string();
    assert!(
        message.contains("failed transaction")
            || message.contains("NONE")
            || message.contains("string"),
        "the coercion refusal (or the rollback): {message}"
    );
    let info = exec::info_for(&fresh, "INFO FOR DB").await;
    assert!(
        !info.contains("DEFINE BUCKET"),
        "the failed transaction left no bucket behind: {info}"
    );

    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let sdl_text = "namespace test; database scratch;
         bucket b { backend: $be; };
         raw { DEFINE PARAM OVERWRITE $be VALUE 'memory'; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Bucket, "b"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Param, "be"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the bucket and the param, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let be_at = script
        .find("DEFINE PARAM OVERWRITE $be VALUE 'memory';")
        .expect("the raw param, verbatim");
    let b_at = script
        .find("DEFINE BUCKET OVERWRITE b BACKEND $be")
        .expect("the bucket define");
    assert!(be_at < b_at, "the param defines first:\n{script}");
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the bucket migration applies");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("BACKEND 'memory'"),
        "the catalog holds the RESOLVED backend: {info}"
    );
}

/// The mandated compound scenario, permanent: a raw
/// member + a raw table source + a typed view over it + a LIGHTWEIGHT
/// promotion in ONE migration — slot, unfold, settle, and the
/// promotion order composing. The script's shape asserts every rule
/// at once (promotion head before the slotted child before the
/// unfolded raw member; the raw source before the view), the
/// transaction lands, and the catalog holds all of it.
#[tokio::test]
async fn the_compound_raw_member_raw_source_view_promotion_converges() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    // The lightweight relation defines fresh (no edges — a promotion
    // refuses while edges exist).
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         table a; table b;
         table a -> likes -> b lightweight;",
    )
    .await;
    let sdl_text = "namespace test; database scratch;
         table a; table b;
         table a -> likes -> b;
         field x: int { on: likes; };
         raw { DEFINE FIELD OVERWRITE y ON likes TYPE string; };
         raw { DEFINE TABLE OVERWRITE zzz SCHEMALESS; };
         table view v { select author from zzz; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "likes"),
            Some(DriftClass::Drift { .. })
        ) && matches!(
            class_of(&findings, Family::Field, "x"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Field, "y"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Table, "zzz"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Table, "v"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the promotion, both members, the raw source, and the view, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let likes_at = script
        .find("DEFINE TABLE OVERWRITE likes")
        .expect("the promotion head");
    let x_at = script
        .find("DEFINE FIELD OVERWRITE x ON likes")
        .expect("the slotted typed child");
    let y_at = script
        .find("DEFINE FIELD OVERWRITE y ON likes TYPE string;")
        .expect("the unfolded raw member, verbatim");
    let zzz_at = script
        .find("DEFINE TABLE OVERWRITE zzz SCHEMALESS;")
        .expect("the raw source, verbatim");
    let v_at = script
        .find("DEFINE TABLE OVERWRITE v TYPE ANY SCHEMALESS AS SELECT author FROM zzz;")
        .expect("the typed view");
    assert!(
        likes_at < y_at && y_at < x_at,
        "the promotion head, the raw member (the unfold's raw-leaf slot \
         behind the head), the slotted typed child:\n{script}"
    );
    assert!(zzz_at < v_at, "the raw source before the view:\n{script}");
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the compound migration applies");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("DEFINE TABLE likes TYPE RELATION IN a OUT b SCHEMALESS")
            && info.contains("DEFINE TABLE zzz")
            && info.contains("DEFINE TABLE v"),
        "the promotion, the raw source, and the view landed: {info}"
    );
    let likes_info = exec::info_for(&db, "INFO FOR TABLE likes").await;
    assert!(
        likes_info.contains("DEFINE FIELD x") && likes_info.contains("DEFINE FIELD y"),
        "both members landed: {likes_info}"
    );
}

/// The FLEXIBLE-host case, pinned by execution: a FLEXIBLE member whose
/// host the SDL never declares refuses outright. The engine validates
/// FLEXIBLE BEFORE the auto-create (vendored define/field.rs —
/// `get_or_add_tb` runs after), so the define fails `TbNotFound` on
/// the missing host and the transaction rolls back — nothing lands,
/// and no ordering repairs it (the host define is not in the script;
/// the SDL never declared the table). The module doc's oscillation
/// paragraph scopes its auto-create claim to PLAIN members; this leg
/// pins the FLEXIBLE refusal class that scope names.
#[tokio::test]
async fn a_flexible_hostless_member_refuses_tb_not_found() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let sdl_text =
        "namespace test; database scratch; field x: option<object> { flexible; on: t; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Field, "x"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the field apply-pending, found {findings:#?}"
    );
    assert!(
        !findings
            .iter()
            .any(|finding| finding.family == Family::Table),
        "no table finding — the SDL never declares the host: {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    common::assert_oracle_accepts_files(&script);
    let error = db
        .query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect_err("the FLEXIBLE member refuses the missing host");
    let message = error.to_string();
    assert!(
        message.contains("does not exist") || message.contains("failed transaction"),
        "the TbNotFound class refusal: {message}"
    );
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        !info.contains("DEFINE TABLE t") && !info.contains("DEFINE FIELD x"),
        "nothing landed — the shell is never created: {info}"
    );
}

/// Pinned by execution: a MULTI-statement raw body. All three raw
/// readers consumed only the body's FIRST statement, so a block whose
/// access carried `URL $p` behind another statement read no value
/// edge, rendered before the typed param, and the transaction
/// COMMITTED with the literal `'NONE'` stored — the silent class,
/// executed end-to-end. The union fix (place.rs walks every
/// statement) settles the block behind the param; the catalog is
/// asserted to hold the RESOLVED url, never `'NONE'`.
#[tokio::test]
async fn a_multistatement_raw_access_resolves_its_param_in_the_catalog() {
    // A fresh engine plays the pre-fix shape — the block renders
    // first — and the silent wrong value LANDS: no error announces it.
    let fresh = exec::mem_db().await;
    fresh.use_ns(NS).use_db(DB).await.ok();
    let swapped = "USE NS test DB scratch; BEGIN TRANSACTION; \
         DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $p; \
         DEFINE TABLE OVERWRITE aux SCHEMALESS; \
         DEFINE PARAM OVERWRITE $p VALUE 'https://x/jwks'; COMMIT;";
    fresh
        .query(swapped)
        .await
        .expect("the query runs")
        .check()
        .expect("the reader-before-dependency order SILENTLY lands");
    let info = exec::info_for(&fresh, "INFO FOR DB").await;
    assert!(
        info.contains("URL 'NONE'"),
        "the swapped order stores the literal NONE — the silent wrong value: {info}"
    );

    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let sdl_text = "namespace test; database scratch;
         param $p = 'https://x/jwks';
         raw { DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $p; \
               DEFINE TABLE OVERWRITE aux SCHEMALESS; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Access, "ju"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Param, "p"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the raw-filed access and the param, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let p_at = script
        .find("DEFINE PARAM OVERWRITE $p VALUE 'https://x/jwks';")
        .expect("the typed param");
    let ju_at = script
        .find("DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $p")
        .expect("the raw block, verbatim");
    assert!(p_at < ju_at, "the param defines first:\n{script}");
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the access migration applies");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("URL 'https://x/jwks'"),
        "the catalog holds the RESOLVED url: {info}"
    );
    assert!(
        !info.contains("URL 'NONE'"),
        "no silent NONE anywhere: {info}"
    );
}

/// The visible twin — the rollback class inside a
/// MULTI-statement body: the block's SECOND statement is a param
/// whose VALUE reads a typed param. Pre-fix the block (classified by
/// its first header, Access — sorting before Param) rendered before
/// `$b`, and the define-time evaluation of `$b + 1` over the missing
/// `$b` errored — the ONE transaction rolled back, visibly. The union
/// fix orders the block behind both its dependencies; the migration
/// lands and the catalog holds the evaluated sum and the resolved url.
#[tokio::test]
async fn a_multistatement_body_value_reader_lands_behind_its_dependency() {
    // A fresh engine plays the reader-first order: the evaluation
    // over the missing `$b` fails, the transaction rolls back.
    let fresh = exec::mem_db().await;
    fresh.use_ns(NS).use_db(DB).await.ok();
    let swapped = "USE NS test DB scratch; BEGIN TRANSACTION; \
         DEFINE ACCESS OVERWRITE ja ON DATABASE TYPE JWT URL $p2; \
         DEFINE PARAM OVERWRITE $a VALUE $b + 1; \
         DEFINE PARAM OVERWRITE $b VALUE 2; \
         DEFINE PARAM OVERWRITE $p2 VALUE 'https://y/jwks'; COMMIT;";
    let error = fresh
        .query(swapped)
        .await
        .expect("the query runs")
        .check()
        .expect_err("the reader-before-dependency order refuses");
    let message = error.to_string();
    assert!(
        message.contains("failed transaction")
            || message.contains("none")
            || message.contains("$b"),
        "the rollback (or the unbound param) is the refusal: {message}"
    );
    let info = exec::info_for(&fresh, "INFO FOR DB").await;
    assert!(
        !info.contains("DEFINE PARAM"),
        "the failed transaction left no param behind: {info}"
    );

    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let sdl_text = "namespace test; database scratch;
         param $b = 2; param $p2 = 'https://y/jwks';
         raw { DEFINE ACCESS OVERWRITE ja ON DATABASE TYPE JWT URL $p2; \
               DEFINE PARAM OVERWRITE $a VALUE $b + 1; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Access, "ja"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Param, "b"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Param, "p2"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the raw-filed access and both params, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let b_at = script
        .find("DEFINE PARAM OVERWRITE $b VALUE 2;")
        .expect("the typed dependency");
    let p2_at = script
        .find("DEFINE PARAM OVERWRITE $p2 VALUE 'https://y/jwks';")
        .expect("the typed url param");
    let ja_at = script
        .find("DEFINE ACCESS OVERWRITE ja ON DATABASE TYPE JWT URL $p2")
        .expect("the raw block, verbatim");
    assert!(
        b_at < ja_at && p2_at < ja_at,
        "both dependencies define before the block:\n{script}"
    );
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the migration lands");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("VALUE 3"),
        "the catalog stores the EVALUATED sum ($b was 2 when $a defined): {info}"
    );
    assert!(
        info.contains("URL 'https://y/jwks'") && !info.contains("URL 'NONE'"),
        "the url resolved with the block: {info}"
    );
}

/// The union leg — a multi-statement body carrying the
/// value-read edge in ONE statement (the access's jwt `url:`) and the
/// dep-name edge in ANOTHER (the view's `FROM` source): both extract,
/// and the block settles behind BOTH targets wherever each
/// edge-carrying statement stands (place.rs walks every statement;
/// first-header classification — Access, sorting before Param and
/// Table — gates nothing). Executed: the view's define walks its
/// source at define (it refuses `TbNotFound` otherwise) and the url
/// resolves — both edges held in the catalog.
#[tokio::test]
async fn a_multistatement_raw_body_resolves_its_url_and_dep_edges() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let sdl_text = "namespace test; database scratch;
         param $p = 'https://x/jwks';
         table zzz { schemaless; field author: string; };
         raw { DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $p; \
               DEFINE TABLE OVERWRITE v AS SELECT author FROM zzz; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Access, "ju"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Param, "p"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Table, "zzz"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the raw-filed access, the param, and the source, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let p_at = script
        .find("DEFINE PARAM OVERWRITE $p VALUE 'https://x/jwks';")
        .expect("the typed param");
    let zzz_at = script
        .find("DEFINE TABLE OVERWRITE zzz TYPE NORMAL SCHEMALESS;")
        .expect("the typed source");
    let raw_at = script
        .find("DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $p")
        .expect("the raw block, verbatim");
    assert!(
        p_at < raw_at && zzz_at < raw_at,
        "both targets define before the block:\n{script}"
    );
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the migration applies — the view's source stood before it");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("DEFINE TABLE v TYPE ANY SCHEMALESS AS SELECT author FROM zzz"),
        "the view landed after its source: {info}"
    );
    assert!(
        info.contains("URL 'https://x/jwks'") && !info.contains("URL 'NONE'"),
        "the url resolved with the block: {info}"
    );
}

/// The fail-closed legs on multi-statement bodies. (1) A
/// body the embedded parser rejects ENTIRELY reads no edge — the
/// block keeps its sorted position (never a guess), the script still
/// generates, and the engine refuses the body at parse time, the
/// transaction rolling back with nothing landed (fail-visible). (2) A
/// statement that parses but carries no edge class (a non-DEFINE
/// statement inside the block) leaves its siblings' edges standing —
/// statement one's `url:` edge still settles the block behind `$q`,
/// and the landing resolves.
#[tokio::test]
async fn a_multistatement_body_reads_fail_closed_and_siblings_survive() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    // (1) the unparseable body: fail-closed position, then the
    // engine's visible refusal. (`DEFINE NOPE` is no family — the
    // whole body fails the embedded parse, so every reader reads
    // nothing.)
    let sdl_text = "namespace test; database scratch;
         param $q = 'https://z/jwks';
         raw { DEFINE ACCESS OVERWRITE jm ON DATABASE TYPE JWT URL $q; \
               DEFINE NOPE EVERYTHING; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Access, "jm"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "the scanner's first-header classification still files the block, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let q_at = script
        .find("DEFINE PARAM OVERWRITE $q")
        .expect("the param define");
    let jm_at = script
        .find("DEFINE ACCESS OVERWRITE jm")
        .expect("the raw block, verbatim");
    assert!(
        jm_at < q_at,
        "the unparseable body keeps its sorted position (fail-closed — never a guess):\n{script}"
    );
    let error = db
        .query(&script)
        .await
        .expect_err("the engine refuses the unparseable body at parse time");
    let message = error.to_string();
    assert!(
        message.contains("Parse error"),
        "the visible refusal (the parse error, pre-transaction): {message}"
    );
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        !info.contains("DEFINE PARAM") && !info.contains("DEFINE ACCESS"),
        "the rollback left nothing behind: {info}"
    );

    // (2) the sibling-survival leg: a parseable body whose second
    // statement is no DEFINE — statement one's value edge stands.
    let sdl_text = "namespace test; database scratch;
         param $q = 'https://z/jwks';
         raw { DEFINE ACCESS OVERWRITE jm2 ON DATABASE TYPE JWT URL $q; \
               CREATE probe SET x = 1; };";
    let (file, diags) = surform::parser::parse_file(sdl_text);
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Access, "jm2"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the raw-filed access, found {findings:#?}"
    );
    let script = convergence_script(&findings, &file);
    let q_at = script
        .find("DEFINE PARAM OVERWRITE $q VALUE 'https://z/jwks';")
        .expect("the typed param");
    let jm2_at = script
        .find("DEFINE ACCESS OVERWRITE jm2 ON DATABASE TYPE JWT URL $q")
        .expect("the raw block, verbatim");
    assert!(
        q_at < jm2_at,
        "the unclassifiable statement did not zero its sibling's edge:\n{script}"
    );
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the sibling's edge held and the migration lands");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("URL 'https://z/jwks'") && !info.contains("URL 'NONE'"),
        "the url resolved behind the param: {info}"
    );
}

/// The undeclared owners remove wholesale: a namespace the SDL never
/// declares (INFO FOR ROOT's `namespaces` map) and a database the SDL
/// never declares (INFO FOR NS's `databases` map) are ordinary
/// database-only findings, and the script collapses them to the one
/// `REMOVE DATABASE` / `REMOVE NAMESPACE` — reachable through the
/// library path and the version-pair CLI; the `--info`
/// tag validation still forbids undeclared scopes — the data-loss
/// guard where the database side is real. Executed: the re-captured
/// INFO drops both.
#[tokio::test]
async fn undeclared_owners_remove_wholesale() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    db.query("DEFINE NAMESPACE stray; DEFINE DATABASE extra;")
        .await
        .expect("the query runs")
        .check()
        .expect("the stray owners define");
    exec::exec_sdl(&db, "namespace test; database scratch;").await;
    let (file, diags) = surform::parser::parse_file("namespace test; database scratch;");
    assert!(diags.is_empty());
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    let script = convergence_script(&findings, &file);
    assert_eq!(
        script,
        format!(
            "{}\
             {}\
             \n\
             USE NS test;\n\
             \n\
             {}\n\
             -- ## Removals - retired scopes\n\
             --\n\
             -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
             \n\
             BEGIN TRANSACTION;\n\
             -- section: db:test/extra - retired (the schema file no longer \
             declares this database)\n\
             \n\
             -- >>> SURFORM WARNING: DATA LOSS <<<\n\
             -- Removes database extra - every table and document in it is\n\
             --    deleted.\n\
             REMOVE DATABASE IF EXISTS extra;\n\
             \n\
             COMMIT;\n\
             \n\
             {}\n\
             -- ## Removals - retired scopes\n\
             --\n\
             -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
             \n\
             BEGIN TRANSACTION;\n\
             -- section: ns:stray - retired (the schema file no longer declares \
             this namespace)\n\
             \n\
             -- >>> SURFORM WARNING: DATA LOSS <<<\n\
             -- Removes namespace stray - every database, table, and\n\
             --    document in it is deleted.\n\
             REMOVE NAMESPACE IF EXISTS stray;\n\
             \n\
             COMMIT;\n",
            surform::info::script::HEADER,
            surform::info::script::summary_block(2, 0, 2, 0, 0, 2, 0),
            surform::info::script::RULE,
            surform::info::script::RULE,
        ),
        "script: {script}"
    );
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the removals apply");
    let findings2 = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        findings2.is_empty(),
        "the collapse must converge: {findings2:#?}"
    );
}

/// A redacted access WITH drift carries the report's re-apply note
/// and the convergence script defines the AUTHORED side (sentinel
/// safety: the define renders the user's own text, never the
/// database's redacted form; the migration never re-defines the
/// literal unless the user authored it).
#[tokio::test]
async fn the_redacted_access_drift_carries_the_note() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch;
         access key_access { on: db; type: jwt; algorithm: hs512; key: '[REDACTED]'; };",
    )
    .await;
    let (file, diags) = surform::parser::parse_file(
        "namespace test; database scratch;
         access key_access { on: db; type: jwt; algorithm: hs256; key: '[REDACTED]'; };",
    );
    assert!(diags.is_empty());
    let findings = diff(&file, &read_model_for(&db, NS, DB).await);
    assert!(
        matches!(
            class_of(&findings, Family::Access, "key_access"),
            Some(DriftClass::Drift { .. })
        ),
        "expected the algorithm change to drift: {findings:#?}"
    );
    let text = report(&findings);
    assert!(
        text.contains("note: re-applying the database's form defines the literal '[REDACTED]' key"),
        "report: {text}"
    );
    // The drift defines the authored side — the script's only access
    // statement is the authored define, never the database's form.
    let script = convergence_script(&findings, &file);
    assert!(
        script.contains("DEFINE ACCESS OVERWRITE key_access ON DATABASE"),
        "script: {script}"
    );

    // The removal direction: a redacted access the SDL no longer
    // declares removes sentinel-safely — a plain REMOVE, no define
    // anywhere near it.
    let (file2, diags2) = surform::parser::parse_file("namespace test; database scratch;");
    assert!(diags2.is_empty());
    let findings2 = diff(&file2, &read_model_for(&db, NS, DB).await);
    let script = convergence_script(&findings2, &file2);
    assert_eq!(
        script,
        format!(
            "{}\
             {}\
             \n\
             USE NS test DB scratch;\n\
             \n\
             {}\n\
             -- ## Scope db:test/scratch\n\
             -- section: db:test/scratch (0 defines, 1 removal)\n\
             \n\
             BEGIN TRANSACTION;\n\
             \n\
             {}\n\
             -- ## Removals - objects the schema file no longer declares\n\
             --\n\
             -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
             \n\
             REMOVE ACCESS IF EXISTS key_access ON DATABASE;\n\
             \n\
             COMMIT;\n",
            surform::info::script::HEADER,
            surform::info::script::summary_block(1, 0, 1, 0, 0, 1, 0),
            surform::info::script::RULE,
            surform::info::script::RULE,
        )
    );
}

// ---------------------------------------------------------------------
// The version pair — the FROM file lowered into the
// read-side slot, the ONE comparator over both input modes
// ---------------------------------------------------------------------

/// The version-pair front end, the CLI's library shape: v1 lowers
/// into the read-side slot, v2 diffs against it.
fn version_findings(v1: &str, v2: &str) -> Vec<DriftFinding> {
    let (from, diags) = surform::parser::parse_file(v1);
    assert!(diags.is_empty(), "{v1}: {diags:?}");
    let (to, diags) = surform::parser::parse_file(v2);
    assert!(diags.is_empty(), "{v2}: {diags:?}");
    let model = surform::info::normalize::ReadModel::version_pair_model(&from, &to);
    diff(&to, &model)
}

fn version_script(v1: &str, v2: &str) -> String {
    let (to, diags) = surform::parser::parse_file(v2);
    assert!(diags.is_empty(), "{v2}: {diags:?}");
    convergence_script(&version_findings(v1, v2), &to)
}

/// Identical versions are converged end to end — no findings, an
/// empty report, an empty script — over a document that rides every
/// input shape the synthesis must carry (typed table with nested
/// members, a standalone field, a param, a raw block).
#[test]
fn identical_versions_converge() {
    let sdl = "namespace test;\ndatabase scratch;\n\
               param $p = 1;\n\
               table user { schemafull; field name: string; \
               index unique ix { fields: name; }; };\n\
               field email: string { on: user; };\n\
               raw { DEFINE ANALYZER arts TOKENIZERS class; };\n";
    let findings = version_findings(sdl, sdl);
    assert_empty(&findings, sdl);
    assert!(
        report(&findings).is_empty(),
        "identical versions must report nothing"
    );
    assert!(
        version_script(sdl, sdl).is_empty(),
        "identical versions must converge to nothing"
    );
}

/// The three directions file with the DB pair's exact classes and
/// report shape: a table v2 adds is SdlOnly (apply-pending), a table
/// v2 drops is DatabaseOnly (the removal carrying its ON-free head),
/// a field both hold with a different kind drifts — the report
/// carrying the kind note and the slot manifest, the script carrying
/// the define, the slots, and the removal.
#[test]
fn the_version_pair_files_each_direction() {
    let v1 = "namespace test;\ndatabase scratch;\n\
              table kept { field x: int; };\n\
              table legacy;\n";
    let v2 = "namespace test;\ndatabase scratch;\n\
              table kept { field x: string; field added: int; };\n\
              table archive;\n";
    let findings = version_findings(v1, v2);
    assert!(
        matches!(
            class_of(&findings, Family::Table, "archive"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected archive SDL-only: {findings:#?}"
    );
    assert!(
        matches!(
            class_of(&findings, Family::Table, "legacy"),
            Some(DriftClass::DatabaseOnly { on_table: None, .. })
        ),
        "expected legacy database-only: {findings:#?}"
    );
    assert!(
        matches!(
            class_of(&findings, Family::Field, "x"),
            Some(DriftClass::Drift { .. })
        ) && matches!(
            class_of(&findings, Family::Field, "added"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected x drifted and added SDL-only: {findings:#?}"
    );
    let text = report(&findings);
    assert!(
        text.contains(
            "note: the kind changes (db `int` -> sdl `string`); existing rows keep \
             their values (the engine validates no rows at define time), but a \
             data step that rewrites them fails while the old kind is in \
             force; run the data step after the definitions - the script stages \
             this table (relaxed shell -> REQUIRED data section -> final \
             definitions)\n"
        ),
        "the kind note rides the version pair's drift: {text}"
    );
    assert!(
        text.contains("== required data-migration slots =="),
        "{text}"
    );
    // The kind change stages `kept` — the staged manifest names it.
    assert!(
        text.contains(
            "== staged migrations ==\n  db:test/scratch table kept - the data \
             section re-keys these fields: `x`; fill the section (or delete \
             it) before commit\n"
        ),
        "{text}"
    );
    let script = version_script(v1, v2);
    // The staged prelude rides FIRST (shell → kind relax → the staged
    // data section), then the defines (the added table), then the
    // removals under their anchors — the removal slot is the LAST
    // REQUIRED region now.
    let shell_at = script
        .find("DEFINE TABLE OVERWRITE kept TYPE ANY SCHEMALESS;\n")
        .expect("the staged shell rides");
    let relax_at = script
        .find("DEFINE FIELD OVERWRITE x ON kept TYPE any;\n")
        .expect("the kind relax rides");
    let staged_banner_at = script
        .find("-- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n")
        .expect("the staged data section rides");
    let define_at = script
        .find("DEFINE TABLE OVERWRITE archive")
        .expect("the added table defines");
    let slot_at = script
        .rfind(">>> SURFORM REQUIRED: DATA MIGRATION")
        .expect("the removal's REQUIRED slot rides");
    let warning_at = script.find("SURFORM WARNING").expect("the alarm rides");
    let remove_at = script
        .find("REMOVE TABLE IF EXISTS legacy;")
        .expect("the dropped table removes");
    assert!(
        shell_at < relax_at && relax_at < staged_banner_at && staged_banner_at < define_at,
        "the staged prelude precedes the defines:\n{script}"
    );
    assert!(define_at < slot_at && warning_at < slot_at && slot_at < remove_at);
    // The slot prose is state-neutral — the WARNING names the removal
    // and the table, never a side.
    assert!(
        script.contains("-- Removes table legacy - every document in it is deleted.\n"),
        "{script}"
    );
}

/// Raw-authored statements on the from side match by the raw
/// machinery exactly as a database's raw definitions would: same
/// body converges, a changed body drifts with the whole-body delta,
/// a raw-vs-structured cross drifts, and a multi-statement block is
/// one finding (its body the delta).
#[test]
fn raw_authored_versions_pair_by_the_raw_machinery() {
    let table = "namespace test;\ndatabase scratch;\ntable t;\n";
    // Same body: converged.
    let raw = "raw { DEFINE INDEX i ON t FIELDS a UNIQUE; };\n";
    let findings = version_findings(&format!("{table}{raw}"), &format!("{table}{raw}"));
    assert!(
        findings.is_empty(),
        "identical raw bodies converge: {findings:#?}"
    );
    // Changed body: drift, the whole body the delta.
    let findings = version_findings(
        &format!("{table}{raw}"),
        &format!("{table}raw {{ DEFINE INDEX i ON t FIELDS a,b UNIQUE; }};\n"),
    );
    match class_of(&findings, Family::Index, "i") {
        Some(DriftClass::Drift { sdl, database, .. }) => {
            assert!(matches!(sdl.as_ref(), surform::ast::Stmt::Raw(_)));
            assert!(matches!(database.as_ref(), surform::ast::Stmt::Raw(_)));
        }
        other => panic!("expected the raw body to drift, found {other:?}"),
    }
    // The structured cross: v1 raw, v2 typed — drift with the route
    // pairing by the raw machinery (the script defines the typed v2
    // side).
    let findings = version_findings(
        &format!("{table}{raw}"),
        &format!("{table}index unique i {{ on: t; fields: a; }};\n"),
    );
    assert!(
        matches!(
            class_of(&findings, Family::Index, "i"),
            Some(DriftClass::Drift { .. })
        ),
        "expected the raw-vs-structured cross to drift: {findings:#?}"
    );
    // A multi-statement block is ONE finding: v2's edited tail
    // statement drifts the whole body.
    let multi = "raw { DEFINE INDEX i ON t FIELDS a UNIQUE; \
                 DEFINE FIELD b ON t TYPE int; };\n";
    let findings = version_findings(
        &format!("{table}{multi}"),
        &format!(
            "{table}raw {{ DEFINE INDEX i ON t FIELDS a UNIQUE; \
                  DEFINE FIELD b ON t TYPE string; }};\n"
        ),
    );
    let finding_count = findings.len();
    assert_eq!(
        finding_count, 1,
        "one raw block, one finding: {findings:#?}"
    );
    // The identical multi-statement block converges.
    let findings = version_findings(&format!("{table}{multi}"), &format!("{table}{multi}"));
    assert!(findings.is_empty(), "{findings:#?}");
}

/// Mixin spreads on either side resolve through the SAME expansion
/// the authored side runs: the from side's spreads materialize to
/// their members, so a v2 edit of one member is drift ON THAT
/// MEMBER — both directions, and identical spreads converge.
#[test]
fn spread_members_pair_at_the_member_level() {
    // v1 authors the mixin + spread; v2 authors the concrete member,
    // changed — drift on the member statement.
    let v1 = "namespace test;\ndatabase scratch;\n\
              mixin base { field x: int; };\n\
              table t schemafull { ...base; };\n";
    let v2 = "namespace test;\ndatabase scratch;\ntable t schemafull { field x: string; };\n";
    let findings = version_findings(v1, v2);
    assert!(
        matches!(
            class_of(&findings, Family::Field, "x"),
            Some(DriftClass::Drift { .. })
        ),
        "expected the spread member to drift: {findings:#?}"
    );
    // The mirror: v1 concrete, v2 the spread with the member changed
    // — the same finding (the expansion is one mechanism).
    let mirrored = version_findings(v2, v1);
    assert_eq!(mirrored.len(), 1, "{mirrored:#?}");
    // A member the from side's spread holds that v2 dropped is
    // database-only (the removal class).
    let v2_drop = "namespace test;\ndatabase scratch;\ntable t schemafull;\n";
    let findings = version_findings(v1, v2_drop);
    match class_of(&findings, Family::Field, "x") {
        Some(DriftClass::DatabaseOnly { on_table, .. }) => {
            assert_eq!(on_table.as_deref(), Some("t"));
        }
        other => panic!("expected the dropped member database-only on its host: {other:?}"),
    }
    // Identical spreads converge (no finding, no mixin-level noise).
    let findings = version_findings(v1, v1);
    assert!(findings.is_empty(), "{findings:#?}");
}

/// The scope universe is the union: a scope the TO version dropped is
/// from-only content and the script's collapse logic retires it under
/// the owner REMOVEs (WARNING-anchored); a scope only the TO version
/// declares is genuinely apply-pending — the script defines its
/// owners first. The version pair makes the collapse CLI-reachable
/// (the DB pair's collapse is library-only behind the E3005 gate).
#[test]
fn the_scope_universe_is_the_union() {
    // The drop: everything in db test/scratch is from-only; the TO
    // file declares neither, so the scope collapses.
    let v1 = "namespace test;\ndatabase scratch;\nparam $p = 1;\ntable t;\n";
    let findings = version_findings(v1, "");
    match (
        class_of(&findings, Family::Namespace, "test"),
        class_of(&findings, Family::Database, "scratch"),
    ) {
        (Some(DriftClass::DatabaseOnly { .. }), Some(DriftClass::DatabaseOnly { .. })) => {}
        other => panic!("expected the owners database-only, found {other:?}"),
    }
    // The content findings are absent — the collapsed scope's single
    // owner REMOVE implies them (no param/table lines).
    let script = version_script(v1, "");
    assert!(script.contains("USE NS test;\n"), "{script}");
    assert!(
        script.contains("REMOVE DATABASE IF EXISTS scratch;\n"),
        "{script}"
    );
    assert!(
        script.contains("REMOVE NAMESPACE IF EXISTS test;\n"),
        "{script}"
    );
    assert!(script.contains("SURFORM WARNING"), "{script}");
    assert!(
        !script.contains("REMOVE PARAM"),
        "the collapse implies the content: {script}"
    );
    assert!(
        !script.contains("REMOVE TABLE"),
        "the collapse implies the content: {script}"
    );
    // The database removal precedes the namespace removal.
    let db_at = script.find("REMOVE DATABASE").expect("the db remove");
    let ns_at = script.find("REMOVE NAMESPACE").expect("the ns remove");
    assert!(db_at < ns_at);
    // The add: only the TO file declares the scope — its owners and
    // content are apply-pending, the owners define first.
    let v2 = "namespace fresh;\ndatabase main;\ntable t;\n";
    let findings = version_findings("", v2);
    assert!(
        matches!(
            class_of(&findings, Family::Namespace, "fresh"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Database, "main"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Table, "t"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the new scope apply-pending: {findings:#?}"
    );
    let script = version_script("", v2);
    let ns_at = script
        .find("DEFINE NAMESPACE OVERWRITE fresh")
        .expect("the ns define");
    let db_at = script
        .find("DEFINE DATABASE OVERWRITE main")
        .expect("the db define");
    let t_at = script
        .find("DEFINE TABLE OVERWRITE t")
        .expect("the table define");
    assert!(
        ns_at < db_at && db_at < t_at,
        "owners define first:\n{script}"
    );
}

/// The findings (and so the script) are a function of the versions'
/// CONTENT, not their authored order: the same two documents with a
/// scope's statements reordered produce byte-identical scripts (the
/// findings' multiset is unchanged; the renderers sort).
#[test]
fn the_version_pair_is_authored_order_insensitive() {
    let v1a = "namespace test;\ndatabase scratch;\nparam $p = 1;\ntable t;\n";
    let v1b = "namespace test;\ndatabase scratch;\ntable t;\nparam $p = 1;\n";
    let v2 = "namespace test;\ndatabase scratch;\nparam $p = 2;\n";
    assert_eq!(version_script(v1a, v2), version_script(v1b, v2));
}

/// THE convergence property, executed end to end:
/// v1 builds into the embedded engine, the v1→v2 script runs, and
/// the engine's INFO re-diffs clean against v2 through the standard
/// DB-pair path — the script lands the database exactly at the to
/// state. The fixture rides both directions: a drifted param, a
/// field v2 adds to the kept table, a table v2 drops (its slot
/// riding), a table v2 adds.
#[tokio::test]
async fn the_version_pair_converges_a_live_database() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let v1 = "namespace test;\ndatabase scratch;\nparam $p = 1;\n\
              table extra { field gone: int; };\ntable legacy;\n";
    let v2 = "namespace test;\ndatabase scratch;\nparam $p = 2;\n\
              table extra { field gone: int; field fresh: string; };\ntable archive;\n";
    exec::exec_sdl(&db, v1).await;
    let (to, diags) = surform::parser::parse_file(v2);
    assert!(diags.is_empty());
    let findings = version_findings(v1, v2);
    assert!(
        matches!(
            class_of(&findings, Family::Param, "p"),
            Some(DriftClass::Drift { .. })
        ) && matches!(
            class_of(&findings, Family::Field, "fresh"),
            Some(DriftClass::SdlOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Table, "legacy"),
            Some(DriftClass::DatabaseOnly { .. })
        ) && matches!(
            class_of(&findings, Family::Table, "archive"),
            Some(DriftClass::SdlOnly { .. })
        ),
        "expected the four-direction fixture: {findings:#?}"
    );
    let script = convergence_script(&findings, &to);
    common::assert_oracle_accepts_files(&script);
    assert!(
        script.contains(">>> SURFORM REQUIRED: DATA MIGRATION"),
        "{script}"
    );
    // The migration runs clean, end to end.
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the migration applies");
    // THE property: the engine's INFO re-diffs clean against v2
    // through the standard DB-pair path — the database landed exactly
    // at the to state.
    let findings2 = diff(&to, &read_model_for(&db, NS, DB).await);
    assert!(
        findings2.is_empty(),
        "the version pair must land v2 exactly:\n--- findings ---\n{findings2:#?}\n--- script ---\n{script}"
    );
}

/// The version pair's slot moves data between versions, executed: v1
/// holds `legacy` with rows; v2 renames the world around it (the
/// move target `archive` is v2-only, so the defines section creates
/// it before the slot rides). Filling the slot with its own `e.g.`
/// line moves the rows out before the REMOVE destroys the table; the
/// re-diff against v2 is clean.
#[tokio::test]
async fn the_version_pair_slot_moves_the_data_between_versions() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let v1 = "namespace test;\ndatabase scratch;\ntable legacy;\n";
    let v2 = "namespace test;\ndatabase scratch;\ntable archive;\n";
    exec::exec_sdl(&db, v1).await;
    db.query("CREATE legacy SET x = 1; CREATE legacy SET x = 2;")
        .await
        .expect("the query runs")
        .check()
        .expect("the rows land");
    let (to, diags) = surform::parser::parse_file(v2);
    assert!(diags.is_empty());
    let findings = version_findings(v1, v2);
    let script = convergence_script(&findings, &to);
    let define_at = script
        .find("DEFINE TABLE OVERWRITE archive")
        .expect("the defines section creates the move target");
    let slot_at = script
        .find(">>> SURFORM REQUIRED: DATA MIGRATION")
        .expect("the REQUIRED slot");
    let remove_at = script
        .find("REMOVE TABLE IF EXISTS legacy;")
        .expect("the removal");
    assert!(
        define_at < slot_at && slot_at < remove_at,
        "the slot rides between the sections:\n{script}"
    );
    // The user fills the slot: the example line, uncommented.
    let filled = script.replace(
        "-- e.g. INSERT INTO archive (SELECT * FROM legacy);",
        "INSERT INTO archive (SELECT * FROM legacy);",
    );
    assert_ne!(filled, script, "the example line is the fill template");
    db.query(&filled)
        .await
        .expect("the script runs")
        .check()
        .expect("the filled migration applies");
    use surrealdb::types::ToSql as _;
    let moved: surrealdb::types::Value = db
        .query("SELECT value x FROM archive ORDER BY x;")
        .await
        .expect("the read runs")
        .take(0)
        .expect("the read takes");
    assert_eq!(moved.to_sql(), "[1, 2]", "the rows moved: {moved:?}");
    let findings2 = diff(&to, &read_model_for(&db, NS, DB).await);
    assert!(
        findings2.is_empty(),
        "the loop must converge: {findings2:#?}"
    );
}

/// The scope drop executes: a database at the v1 state (namespace,
/// database, a table holding a row) migrates to an empty v2 — the
/// collapsed owner REMOVEs run, and the engine's root INFO no longer
/// holds the namespace.
#[tokio::test]
async fn the_version_pair_scope_drop_executes() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let v1 = "namespace test;\ndatabase scratch;\ntable t;\n";
    exec::exec_sdl(&db, v1).await;
    db.query("CREATE t SET x = 1;")
        .await
        .expect("the query runs")
        .check()
        .expect("the row lands");
    let script = version_script(v1, "");
    common::assert_oracle_accepts_files(&script);
    db.query(&script)
        .await
        .expect("the script runs")
        .check()
        .expect("the collapse applies");
    let info = exec::info_for(&db, "INFO FOR ROOT").await;
    assert!(
        !info.contains("test"),
        "the collapsed namespace is gone from the root: {info}"
    );
}

/// The header-less raw body keeps its EMPTY match key (the
/// renderers grew the hint, the pairing home did not): the
/// from-side entry is filed under the empty key like any raw body,
/// the finding carries key "", and the script's manual-removal note
/// quotes the body's first line, so the removal still names itself.
#[test]
fn the_headerless_raw_keeps_its_empty_key_and_names_itself() {
    let v1 = "namespace test;\ndatabase scratch;\nraw { DEFINE THING weird ON t; };\n";
    let v2 = "namespace test;\ndatabase scratch;\n";
    let findings = version_findings(v1, v2);
    let raw = findings
        .iter()
        .find(|f| f.family == Family::Raw)
        .expect("the raw finding");
    assert_eq!(raw.key, "", "the match key stays empty (the pairing home)");
    assert!(
        matches!(raw.class, DriftClass::DatabaseOnly { .. }),
        "{raw:#?}"
    );
    let script = version_script(v1, v2);
    assert!(
        script.contains(
            "-- manual removal required: db:test/scratch raw - DEFINE THING weird ON t;\n"
        ),
        "{script}"
    );
}

// The totality proptest (Layer 3): mutated interchange × the
// comparator — the drift model is total and both renderers are pure
// functions of it, whatever the interchange's bytes were.
proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(512))]

    /// Mutated interchange × the comparator: total (no panics), the
    /// renderers' structural invariants hold whatever the
    /// interchange's bytes were.
    #[test]
    fn mutated_interchange_diffs_and_renders_without_panics(
        cut in 0usize..400,
        edits in proptest::collection::vec(
            (0usize..400, proptest::sample::select(vec![
                b'{', b'}', b':', b',', b'\'', b'"', b';', b'\\', b'a', b' ',
            ])),
            0..12,
        ),
    ) {
        let text = common::corrupt(common::INTERCHANGE_FIXTURE, cut, &edits);
        if let Ok(obj) = read_info_object(&text) {
            let namespaces = Vec::new();
            let databases = vec![("test".to_string(), "scratch".to_string(), &obj)];
            if let Ok(model) = ReadModel::from_interchange(Interchange {
                root: None,
                namespaces: &namespaces,
                databases: &databases,
            }) {
                let (file, _) = surform::parser::parse_file(
                    "namespace test;\ndatabase scratch;\ntable t;\nparam $p = 1;\n",
                );
                let findings = diff(&file, &model);
                let script = convergence_script(&findings, &file);
                // The script's standing invariant: every REMOVE is
                // idempotent (IF EXISTS), whatever produced the
                // finding.
                proptest::prop_assert!(
                    script
                        .lines()
                        .filter(|line| line.starts_with("REMOVE"))
                        .all(|line| line.contains(" IF EXISTS ")),
                    "a REMOVE without IF EXISTS:\n{script}"
                );
                // The renderers are total and pure: the same findings
                // render identically on a re-run.
                let findings2 = diff(&file, &model);
                proptest::prop_assert_eq!(
                    convergence_script(&findings2, &file),
                    script,
                    "the script is not a pure function of the model"
                );
                proptest::prop_assert_eq!(
                    report(&findings2),
                    report(&findings),
                    "the report is not a pure function of the model"
                );
            }
        }
    }
}

// -------------------------------------------------------------------
// The standalone twin fold (issue #51): a standalone child authored
// against an existing converged table must re-diff clean after a
// clean apply — compare_children's database-side arm defers the
// converged child to the SDL standalone's own twin path instead of
// filing a phantom database-only finding.
// -------------------------------------------------------------------

/// The issue's exact probe: standalone-authored child, clean apply,
/// re-diff — EMPTY (nested-authored already converged; standalone
/// yielded one bogus database-only finding).
#[tokio::test]
async fn a_standalone_child_of_a_converged_table_re_diffs_clean() {
    let sdl = "namespace test; database scratch; table t; field x: int { on: t; };";
    let (file, model) = applied(sdl).await;
    let findings = diff(&file, &model);
    assert_empty(&findings, sdl);
}

/// Source order is irrelevant: the standalone BEFORE its host table
/// folds the same way.
#[tokio::test]
async fn a_standalone_child_before_its_host_re_diffs_clean() {
    let sdl = "namespace test; database scratch; field x: int { on: t; }; table t;";
    let (file, model) = applied(sdl).await;
    let findings = diff(&file, &model);
    assert_empty(&findings, sdl);
}

/// A DIVERGED standalone files exactly ONE finding — the Drift the
/// SDL side's twin path carries — never a double (DatabaseOnly plus
/// Drift) and never the phantom.
#[tokio::test]
async fn a_diverged_standalone_files_one_drift() {
    let (_file, model) =
        applied("namespace test; database scratch; table t; field x: int { on: t; };").await;
    let (drifted, diags) = surform::parser::parse_file(
        "namespace test; database scratch; table t; field x: string { on: t; };",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&drifted, &model);
    assert_eq!(
        findings.len(),
        1,
        "exactly one finding for the diverged standalone: {findings:#?}"
    );
    assert!(
        matches!(findings[0].class, DriftClass::Drift { .. }),
        "the finding is the Drift: {findings:#?}"
    );
    assert_eq!(findings[0].family, Family::Field);
    assert_eq!(findings[0].key, "x");
}

/// The SDL-only host keeps today's classes: an apply-pending table
/// and its apply-pending standalone child are both SdlOnly — no
/// database-only phantom (there is no database side at all).
#[tokio::test]
async fn an_apply_pending_host_keeps_sdl_only_classes() {
    let (_file, model) = applied("namespace test; database scratch;").await;
    let (fresh, diags) = surform::parser::parse_file(
        "namespace test; database scratch; table fresh; field y: int { on: fresh; };",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let findings = diff(&fresh, &model);
    assert!(!findings.is_empty(), "the fresh pair is apply-pending");
    for finding in &findings {
        assert!(
            matches!(finding.class, DriftClass::SdlOnly { .. }),
            "every finding is SdlOnly: {finding:#?}"
        );
    }
}
