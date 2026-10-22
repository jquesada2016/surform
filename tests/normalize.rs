//! The normalizer's convergence tests: SDL → DDL →
//! embedded exec → INFO → read → merge → SDL″, where SDL″ is the
//! canonical form — byte-equal to fmt(SDL) for materialization-free
//! inputs and carrying exactly the pinned adopt/elide/preserve/fold
//! delta otherwise.
//!
//! The engine harness rides `tests/common` (the shared
//! home: `#[path = "common/exec.rs"]` — each exec suite compiles the
//! trio once; `common/mod.rs`'s oracle helpers stay the layer-2
//! suites' home). `read_model_for`, `canon`, and `converge` are this
//! suite's own fixtures; the fmt fixed point below is the owned copy
//! (tests/info_read.rs's precedent — see its header).

#[path = "common/mod.rs"]
mod common;
#[path = "common/exec.rs"]
mod exec;

use std::collections::BTreeMap;

use surform::info::normalize::{Interchange, ReadModel, TABLE_CHILDREN_KEY, merge};
use surform::info::read::{InfoObject, InfoValue, read_info_object};
use surform::info::write::write_canonical;
use surrealdb::Surreal;
use surrealdb::engine::local::Db;

// -- the embedded engine (Layer 4) rides tests/common --

/// The fmt fixed point, over the merge's output.
/// (Owned copy — see the file header.)
fn assert_fmt_fixed_point(sdl: &str) {
    let (ast, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{sdl}: {diags:?}");
    let once = surform::info::write::write_canonical(&ast);
    let (reparsed, diags) = surform::parser::parse_file(&once);
    assert!(diags.is_empty(), "fmt output stopped parsing:\n{once}");
    assert!(
        ast.same_definition(&reparsed),
        "fmt changed the definition:\n--- SDL ---\n{sdl}\n--- fmt ---\n{once}"
    );
    let twice = surform::info::write::write_canonical(&reparsed);
    assert_eq!(twice, once, "fmt is not idempotent:\n{once}");
}

/// The full read model over the session's three INFO scopes, with the
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
            let text = exec::info_for(db, &format!("INFO FOR TABLE {name}")).await;
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

/// Dedents a literal block to the expected canonical bytes: strips
/// the common margin and applies the writer's own top-level
/// blank-line rule, so test literals stay tight and readable while
/// the comparison stays byte-exact. Like the writer, the break falls
/// before the whole group — a `//`/`///` comment run leading a
/// statement gets the blank, so comment-led literals mirror the
/// output too. The separator blanks themselves are pinned by the CLI
/// expectations and the info_read SDL snapshots; this normalization
/// only keeps them out of the literals' way. (Known limit,
/// fail-loud: a
/// file-trailing comment after the LAST statement prints tight in
/// real output but gains a blank here — a literal that wants that
/// shape writes the bytes by hand.)
fn canon(s: &str) -> String {
    let lines: Vec<&str> = s.trim_matches('\n').lines().collect();
    let margin = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let dedented: Vec<&str> = lines
        .iter()
        .map(|line| &line[margin.min(line.len())..])
        .collect();
    let mut out = String::new();
    for (i, line) in dedented.iter().enumerate() {
        // The statement heads, at margin 0 in the dedented literal —
        // clause and nested lines carry the literal's indent and
        // never match.
        let head = line.split(|c: char| !c.is_ascii_alphanumeric()).next();
        let is_head = matches!(
            head,
            Some("namespace")
                | Some("database")
                | Some("table")
                | Some("user")
                | Some("access")
                | Some("sequence")
                | Some("param")
                | Some("analyzer")
                | Some("bucket")
                | Some("config")
                | Some("event")
                | Some("module")
                | Some("raw")
                | Some("field")
                | Some("index")
                | Some("api")
                | Some("fn")
                | Some("ns")
                | Some("db")
        );
        let is_comment = line.starts_with("//");
        if i > 0 && (is_head || is_comment) {
            let prev = dedented[i - 1].trim_end();
            if prev.ends_with(';') && !prev.starts_with("//") {
                out.push('\n');
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The convergence leg: exec the SDL, read all three INFO scopes,
/// merge, and hold the written output byte-equal to `expected` (the
/// canonical form carrying exactly the pinned delta) and fmt-stable.
async fn converge(sdl: &str, expected: &str) {
    const NS: &str = "test";
    const DB: &str = "scratch";
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(&db, sdl).await;
    let model = read_model_for(&db, NS, DB).await;
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{sdl}: {diags:?}");
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(out, canon(expected), "merge output diverged");
    assert_fmt_fixed_point(&out);
}

// ---------------------------------------------------------------------
// ELIDE — the materialized defaults fold back to minimal SDL
// ---------------------------------------------------------------------

#[tokio::test]
async fn sequence_defaults_elide_to_the_short_form() {
    converge(
        "namespace test;
         database scratch;
         sequence s = 5;
         sequence t { start: 5; batch: 100; timeout: 1h30m; };",
        "
         namespace test;
         database scratch;
         sequence s = 5;
         sequence t {
           start: 5;
           batch: 100;
           timeout: 1h30m;
         };",
    )
    .await;
}

#[tokio::test]
async fn param_adopts_the_evaluated_value_and_elides_full() {
    converge(
        "namespace test;
         database scratch;
         param $p = 1 + 1;
         param $q { value: [1, 2]; permissions: full; };",
        "
         namespace test;
         database scratch;
         param $p = 2;
         param $q = [1, 2];",
    )
    .await;
}

#[tokio::test]
async fn user_defaults_elide_and_secrets_preserve() {
    converge(
        "namespace test;
         database scratch;
         user admin { on: root; password: 'secret'; roles: owner, editor; };
         user watcher { on: db; roles: viewer; };",
        "
         namespace test;
         database scratch;
         user admin {
           on: root;
           password: 'secret';
           roles: owner, editor;
         };
         user watcher {
           on: db;
         };",
    )
    .await;
}

/// The 3.3 `passscram:` slot preserves like the pass clauses (the
/// merge carries the SDL value; the read never produces one — INFO's
/// `PASSSCRAM '[REDACTED]'` strips before validation). A scram-only
/// user merges with its empty hash untouched.
#[tokio::test]
async fn user_passscram_preserves() {
    let kat = common::menus::SCRAM_KAT;
    let sdl = format!(
        "namespace test;
         database scratch;
         user provisioned {{ on: root; passscram: '{kat}'; }};"
    );
    let expected = format!(
        "
         namespace test;
         database scratch;
         user provisioned {{
           on: root;
           passscram: '{kat}';
         }};"
    );
    converge(&sdl, &expected).await;
}

#[tokio::test]
async fn event_materializations_elide() {
    converge(
        "namespace test;
         database scratch;
         table t { schemaless;
           event ev { then: CREATE logged; async; };
         };
         table logged { schemaless; };",
        "
         namespace test;
         database scratch;
         table t {
           schemaless;
           event ev {
             then: (CREATE logged);
             async;
           };
         };
         table logged {
           schemaless;
         };",
    )
    .await;
}

#[tokio::test]
async fn index_materializations_elide_at_defaults() {
    converge(
        "namespace test;
         database scratch;
         table fold { schemaless;
           index fulltext ft { fields: t; };
           index hnsw hn { fields: v; dimension: 4; };
           index hnsw hn30 { fields: v; dimension: 4; m: 30; };
           index diskann dk { fields: v; dimension: 4; };
         };",
        "
         namespace test;
         database scratch;
         table fold {
           schemaless;
           index diskann dk {
             fields: v;
             dimension: 4;
           };
           index fulltext ft {
             fields: t;
           };
           index hnsw hn {
             fields: v;
             dimension: 4;
           };
           index hnsw hn30 {
             fields: v;
             dimension: 4;
             m: 30;
           };
         };",
    )
    .await;
}

#[tokio::test]
async fn field_permissions_elide_and_respell() {
    converge(
        "namespace test;
         database scratch;
         table t { schemaless;
           field f_full: int { permissions: full; };
           field f_none: int { permissions: none; };
         };",
        "
         namespace test;
         database scratch;
         table t {
           schemaless;
           field f_full: int;
           field f_none: int {
             permissions: none;
           };
         };",
    )
    .await;
}

#[tokio::test]
async fn mixed_table_permission_groups_elide_their_default_sides() {
    // The engine materializes every operation's side; groups at the
    // table's SurrealQL default (NONE) drop, leaving the SDL's own
    // shape byte-identical.
    converge(
        "namespace test;
         database scratch;
         table mixed { schemafull; permissions { select: full; }; };",
        "
         namespace test;
         database scratch;
         table mixed {
           schemafull;
           permissions {
             select: full;
           };
         };",
    )
    .await;
}

// ---------------------------------------------------------------------
// FOLD — the synthesized shapes fold away
// ---------------------------------------------------------------------

#[tokio::test]
async fn dot_star_subfields_fold_deepest_first() {
    converge(
        "namespace test;
         database scratch;
         table t { schemaless;
           field matrix: array<array<int>>;
           field f: int | array<string>;
           field optel: array<option<int>>;
           field sized: set<float, 4>;
           field bare: array;
           field tags: array;
           field tags.*: int;
           field a.b: array<int>;
         };",
        "
         namespace test;
         database scratch;
         table t {
           schemaless;
           field a.b: array<int>;
           field bare: array;
           field f: int | array<string>;
           field matrix: array<array<int>>;
           field optel: array<option<int>>;
           field sized: set<float, 4>;
           field tags: array;
           field tags.*: int;
         };",
    )
    .await;
}

#[tokio::test]
async fn relation_endpoints_fold_but_customized_sides_stay() {
    converge(
        "namespace test;
         database scratch;
         table users2 { schemaless; };
         table orgs { schemaless; };
         table users2 -> likes -> orgs { schemafull; enforced; };
         table users2 -> custom -> orgs {
           schemafull;
           field in: record<users2> { readonly; };
         };",
        "
         namespace test;
         database scratch;
         table users2 {
           schemaless;
         };
         table orgs {
           schemaless;
         };
         table users2->likes->orgs {
           schemafull;
           enforced;
         };
         table users2->custom->orgs {
           schemafull;
           field `in`: record<users2> {
             readonly;
           };
         };",
    )
    .await;
}

// The 3.3 shapes: a LIGHTWEIGHT relation merges through the
// header's own implication (no in/out children exist at read — the
// reader drops the engine's auto-installed fields) and the INLINE
// caps adopt verbatim (the printer materializes only written caps, so
// there is nothing to elide).
#[tokio::test]
async fn lightweight_and_caps_converge() {
    converge(
        "namespace test;
         database scratch;
         table lw_users { schemaless; };
         table lw_posts { schemaless; };
         table lw_users -> likes -> lw_posts lightweight { inline edges: 8; };
         table capd { inline references: 16; inline edges: 0; };",
        "
         namespace test;
         database scratch;
         table lw_users {
           schemaless;
         };
         table lw_posts {
           schemaless;
         };
         table lw_users->likes->lw_posts lightweight {
           schemaless;
           inline edges: 8;
         };
         table capd {
           schemaless;
           inline edges: 0;
           inline references: 16;
         };",
    )
    .await;
}

// ---------------------------------------------------------------------
// PRESERVE — what INFO drops but SDL means
// ---------------------------------------------------------------------

#[tokio::test]
async fn table_delete_full_never_downgrades() {
    converge(
        "namespace test;
         database scratch;
         table guarded { schemafull; permissions { delete: full; select: none; }; };
         table open { schemafull; permissions: full; };",
        "
         namespace test;
         database scratch;
         table guarded {
           schemafull;
           permissions {
             delete: full;
           };
         };
         table open {
           schemafull;
           permissions: full;
         };",
    )
    .await;
}

#[tokio::test]
async fn strict_and_concurrently_preserve() {
    converge(
        "namespace test;
         database scratch { strict; };
         table t { schemaless;
           index unique i { fields: a; concurrently; };
         };",
        "
         namespace test;
         database scratch {
           strict;
         };
         table t {
           schemaless;
           index unique i {
             fields: a;
             concurrently;
           };
         };",
    )
    .await;
}

#[tokio::test]
async fn access_durations_elide_and_the_url_algorithm_preserves() {
    // `refresh;` makes the access grant-capable — INFO materializes
    // DURATION FOR GRANT 4w2d (the value-normalized 30d) alongside the
    // token/session sides, and all three elide at their defaults.
    converge(
        "namespace test;
         database scratch;
         access record_auth {
           on: db;
           type: record;
           signin: SELECT * FROM user;
           refresh;
         };
         access sso {
           on: db;
           type: jwt;
           url: 'https://sso.example/jwks';
           issuer { algorithm: rs256; key: '[REDACTED]'; };
         };",
        "
         namespace test;
         database scratch;
         access record_auth {
           on: db;
           type: record;
           signin: (SELECT * FROM user);
           jwt {
             algorithm: hs512;
             key: '[REDACTED]';
           };
           refresh;
         };
         access sso {
           on: db;
           type: jwt;
           url: 'https://sso.example/jwks';
           issuer {
             algorithm: rs256;
             key: '[REDACTED]';
           };
         };",
    )
    .await;
}

// ---------------------------------------------------------------------
// ADOPT — entries with no SDL counterpart surface
// ---------------------------------------------------------------------

#[tokio::test]
async fn unmanaged_definitions_adopt() {
    let db = exec::mem_db().await;
    db.use_ns("test").use_db("scratch").await.ok();
    // The first apply is the database state the second file knows
    // nothing about.
    exec::exec_sdl(
        &db,
        "namespace test;
         database scratch;
         sequence s = 5;
         table t { schemaless;
           field a: int;
           field m: array<int>;
         };",
    )
    .await;
    // The second file declares only the namespace/database: everything
    // else adopts (elided form, `m.*` folded away). The synthesized
    // `m.*` field folds into `m` under the kind algebra; `m`'s field
    // name escapes (a reserved word in SurrealQL's kind positions).
    let sdl = "namespace test;\ndatabase scratch;\n";
    let model = read_model_for(&db, "test", "scratch").await;
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(
        out,
        canon(
            "
             namespace test;
             database scratch;
             sequence s = 5;
             table t {
               schemaless;
               field a: int;
               field `m`: array<int>;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
}

#[tokio::test]
async fn dot_star_entry_with_own_clauses_is_user_meaningful_and_kept() {
    // An entry whose kind matches the parent's inner kind but carries
    // its own clauses (a `default`) is user-meaningful and stays (a
    // divergent kind is engine-rejected at define time — the fold's
    // divergent branch guards hand-fed interchange only).
    converge(
        "namespace test;
         database scratch;
         table t { schemaless;
           field matrix: array<int>;
           field matrix.*: int { default: 5; };
         };",
        "
         namespace test;
         database scratch;
         table t {
           schemaless;
           field matrix: array<int>;
           field matrix.*: int {
             default: 5;
           };
         };",
    )
    .await;
}

// ---------------------------------------------------------------------
// Layer 3 — the proptest panic guard over the merge
// ---------------------------------------------------------------------

// The live-captured interchange fixture and the `corrupt` mutation
// helper live in `tests/common` (the differ's totality
// proptest mutates the same interchange).

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(512))]

    /// Mutated interchange × a fixture SDL: reading may diagnose (a
    /// well-formed interchange rejection only), but a successful read
    /// merges without panicking and the merged file always re-parses
    /// clean.
    #[test]
    fn mutated_interchange_merges_without_panics(
        cut in 0usize..400,
        edits in proptest::collection::vec(
            (0usize..400, proptest::sample::select(vec![
                b'{', b'}', b':', b',', b'\'', b'"', b';', b'\\', b'a', b' ',
            ])),
            0..12,
        ),
    ) {
        let text = common::corrupt(common::INTERCHANGE_FIXTURE, cut, &edits);
        let read = read_info_object(&text);
        if let Ok(obj) = read {
            let namespaces = Vec::new();
            let databases = vec![("test".to_string(), "scratch".to_string(), &obj)];
            if let Ok(model) = ReadModel::from_interchange(Interchange {
                root: None,
                namespaces: &namespaces,
                databases: &databases,
            }) {
                let (file, _) = surform::parser::parse_file(
                    "namespace test;\ndatabase scratch;\n",
                );
                let merged = merge(&file, &model);
                let out = write_canonical(&merged);
                let (reparsed, diags) = surform::parser::parse_file(&out);
                proptest::prop_assert!(diags.is_empty(), "merge output stopped parsing:\n{out}");
                proptest::prop_assert_eq!(
                    write_canonical(&reparsed),
                    out,
                    "merge output is not fmt-stable"
                );
            }
        }
    }

    /// Arbitrary text into the merge's inputs never panics: the read
    /// diagnoses or the merge runs over whatever read.
    #[test]
    fn arbitrary_interchange_text_never_panics(text in ".*") {
        let _ = read_info_object(&text).map(|obj| {
            let namespaces = Vec::new();
            let databases = vec![("test".to_string(), "scratch".to_string(), &obj)];
            if let Ok(model) = ReadModel::from_interchange(Interchange {
                root: None,
                namespaces: &namespaces,
                databases: &databases,
            }) {
                let (file, _) = surform::parser::parse_file("");
                let _ = merge(&file, &model);
            }
        });
    }
}

// ---------------------------------------------------------------------
// The scope model — multi-scope regressions
// ---------------------------------------------------------------------

/// The read model over EXPLICIT scope lists: the session hops to every
/// scope to capture its INFO (the queries are session-scoped).
async fn read_model_for_scopes(
    db: &Surreal<Db>,
    namespaces: &[&str],
    databases: &[(&str, &str)],
) -> ReadModel {
    let root_text = exec::info_for(db, "INFO FOR ROOT").await;
    let root = read_info_object(&root_text).expect("root INFO walks");
    let mut ns_objs: Vec<(String, InfoObject)> = Vec::new();
    for ns in namespaces {
        db.use_ns(*ns).await.ok();
        let text = exec::info_for(db, "INFO FOR NS").await;
        ns_objs.push((
            ns.to_string(),
            read_info_object(&text).expect("ns INFO walks"),
        ));
    }
    let mut db_objs: Vec<(String, String, InfoObject)> = Vec::new();
    for (ns, db_name) in databases {
        db.use_ns(*ns).use_db(*db_name).await.ok();
        let text = exec::info_for(db, "INFO FOR DB").await;
        let mut obj = read_info_object(&text).expect("db INFO walks");
        let mut children = BTreeMap::new();
        if let Some(InfoValue::Object(tables)) = obj.0.get("tables") {
            for name in tables.0.keys() {
                let t = exec::info_for(db, &format!("INFO FOR TABLE {name}")).await;
                let o = read_info_object(&t).expect("table INFO walks");
                children.insert(name.clone(), InfoValue::Object(o));
            }
        }
        obj.0.insert(
            TABLE_CHILDREN_KEY.to_string(),
            InfoValue::Object(InfoObject(children)),
        );
        db_objs.push((ns.to_string(), db_name.to_string(), obj));
    }
    let ns_refs: Vec<(String, &InfoObject)> = ns_objs.iter().map(|(n, o)| (n.clone(), o)).collect();
    let db_refs: Vec<(String, String, &InfoObject)> = db_objs
        .iter()
        .map(|(n, d, o)| (n.clone(), d.clone(), o))
        .collect();
    ReadModel::from_interchange(Interchange {
        root: Some(&root),
        namespaces: &ns_refs,
        databases: &db_refs,
    })
    .expect("every scope reads")
}

#[tokio::test]
async fn multi_scope_merge_keeps_every_scope_separate() {
    let db = exec::mem_db().await;
    db.use_ns("test").use_db("scratch").await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; table x { schemaless; };",
    )
    .await;
    exec::exec_sdl(
        &db,
        "namespace test; database second; table y { schemaless; };",
    )
    .await;
    exec::exec_sdl(
        &db,
        "namespace other; database solo; table z { schemaless; };",
    )
    .await;
    // The SDL declares both `test` databases (the second matches in the
    // NS scope even after the first opened a db scope) and nothing of
    // `other`: its database + table adopt under a fresh tail anchor.
    // The second database declares a changefeed the database never
    // had: the NS-scope lookup still matches (the override under
    // test), and adopt wins — the drift drops.
    let sdl = "namespace test;\ndatabase scratch;\ntable x { schemaless; };\ndatabase second { changefeed { expiry: 1h; }; };\n";
    let model = read_model_for_scopes(
        &db,
        &["test", "other"],
        &[("test", "scratch"), ("test", "second"), ("other", "solo")],
    )
    .await;
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(
        out,
        canon(
            "
             namespace test;
             database scratch;
             table x {
               schemaless;
             };
             database second;
             table y {
               schemaless;
             };
             namespace other;
             database solo;
             table z {
               schemaless;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
}

#[tokio::test]
async fn root_entries_adopt_when_the_file_declares_no_root_statements() {
    let db = exec::mem_db().await;
    db.use_ns("test").use_db("scratch").await.ok();
    // A root user defined outside SDL's file: the ROOT scope's
    // earliest slot is the file head (the earliest-gap rule — the
    // user carries `on: root`, position-safe anywhere).
    db.query("DEFINE USER rooty ON ROOT PASSWORD 'secret';")
        .await
        .expect("root user defines")
        .check()
        .expect("root user executes");
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; table t { schemaless; };",
    )
    .await;
    let sdl = "namespace test;\ndatabase scratch;\ntable t { schemaless; };\n";
    let model = read_model_for(&db, "test", "scratch").await;
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(
        out,
        canon(
            "
             user rooty {
               on: root;
             };
             namespace test;
             database scratch;
             table t {
               schemaless;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
}

// ---------------------------------------------------------------------
// The remaining families — adopt + elide converge to authored form
// ---------------------------------------------------------------------

#[tokio::test]
async fn remaining_families_converge() {
    converge(
        "namespace test;
         database scratch;
         analyzer eng { tokenizers: class, punct; filters: lowercase, snowball(english); };
         bucket docs { backend: 'memory'; };
         fn g::h($a: int) { RETURN $a * 2; };
         api '/x' {
           get { then: CREATE log:x; };
         };
         config graphql { tables: auto; functions: auto; };",
        "
         namespace test;
         database scratch;
         analyzer `eng` {
           tokenizers: class, punct;
           filters: lowercase, snowball(english);
         };
         bucket docs {
           backend: 'memory';
         };
         fn g::h($a: int) { RETURN $a * 2 };
         api '/x' {
           get {
             then: CREATE log:x;
           };
         };
         config graphql {
           tables: auto;
           functions: auto;
         };",
    )
    .await;
}

// ---------------------------------------------------------------------
// The unrepresentable engine state — the m=1 HNSW index raw-routes
// ---------------------------------------------------------------------

#[tokio::test]
async fn diverged_lm_index_adopts_as_raw() {
    converge(
        "namespace test;
         database scratch;
         table t { schemaless;
           field v: array<float>;
           index hnsw i { fields: v; dimension: 3; m: 1; };
         };",
        "
         namespace test;
         database scratch;
         table t {
           schemaless;
           field v: array<float>;
         };
         raw {
         DEFINE INDEX i ON t FIELDS v HNSW DIMENSION 3 DIST EUCLIDEAN TYPE F32 EFC 150 M 1 M0 2 LM Infinity;
         };",
    )
    .await;
}

#[tokio::test]
async fn root_listed_namespace_with_a_db_tag_still_adopts_content() {
    // ROOT's map lists every namespace; an interchange that tags
    // `db=ghost/gdb` without an `ns=ghost` section must still adopt the
    // database's content under the ROOT-tail's anchor (never dropped) —
    // and TWO db tags under the section-less ns adopt once each (the
    // tail pass is per distinct ns, never per tag).
    let db = exec::mem_db().await;
    db.use_ns("ghost").use_db("gdb").await.ok();
    exec::exec_sdl(
        &db,
        "namespace ghost; database gdb; table z { schemaless; };",
    )
    .await;
    exec::exec_sdl(
        &db,
        "namespace ghost; database gdb2; table z2 { schemaless; };",
    )
    .await;
    db.use_ns("real").use_db("d").await.ok();
    exec::exec_sdl(&db, "namespace real; database d; table t { schemaless; };").await;
    let sdl = "namespace real;\ndatabase d;\ntable t { schemaless; };\n";
    // No `ns=ghost` section passed — only the db tags.
    let model = read_model_for_scopes(
        &db,
        &[],
        &[("ghost", "gdb"), ("ghost", "gdb2"), ("real", "d")],
    )
    .await;
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(
        out,
        canon(
            "
             namespace real;
             database d;
             table t {
               schemaless;
             };
             namespace ghost;
             database gdb;
             table z {
               schemaless;
             };
             database gdb2;
             table z2 {
               schemaless;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
}

#[tokio::test]
async fn an_empty_ns_section_still_anchors_its_namespace() {
    // A tagged ns section with no content anchors the bare `namespace`
    // statement for an SDL that never declares it — synthesis keys on
    // the SDL section's absence, not the scope's content.
    let db = exec::mem_db().await;
    db.use_ns("hollow").await.ok();
    db.use_ns("real").use_db("d").await.ok();
    exec::exec_sdl(&db, "namespace real; database d; table t { schemaless; };").await;
    let sdl = "namespace real;\ndatabase d;\ntable t { schemaless; };\n";
    let model = read_model_for_scopes(&db, &["hollow"], &[("real", "d")]).await;
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(
        out,
        canon(
            "
             namespace real;
             database d;
             table t {
               schemaless;
             };
             namespace hollow;"
        )
    );
    assert_fmt_fixed_point(&out);
}

#[tokio::test]
async fn an_empty_file_synthesizes_everything_from_the_read_model() {
    // The ROOT synthesized arm's shape: no walk positions exist, so
    // every scope tails — the merge is total over an empty pre-merge
    // source.
    let db = exec::mem_db().await;
    db.use_ns("test").use_db("scratch").await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; table t { schemaless; };",
    )
    .await;
    let model = read_model_for(&db, "test", "scratch").await;
    let (file, diags) = surform::parser::parse_file("");
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(
        out,
        canon(
            "
             namespace test;
             database scratch;
             table t {
               schemaless;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
}

#[tokio::test]
async fn tail_groups_keep_anchor_and_content_adjacent() {
    // The anchor-ordering regression shape: an unanchored ROOT listing,
    // a tagged-but-empty section, and two section-less ns tags — every
    // group emits its anchor and content ADJACENTLY, so no database
    // ever re-binds to another group's namespace.
    let db = exec::mem_db().await;
    db.use_ns("ghost").use_db("gdb").await.ok();
    exec::exec_sdl(
        &db,
        "namespace ghost; database gdb; table z { schemaless; };",
    )
    .await;
    db.use_ns("hollow").await.ok();
    db.use_ns("alpha").use_db("ad").await.ok();
    exec::exec_sdl(
        &db,
        "namespace alpha; database ad; table a1 { schemaless; };",
    )
    .await;
    db.use_ns("beta").use_db("bd").await.ok();
    exec::exec_sdl(
        &db,
        "namespace beta; database bd; table b1 { schemaless; };",
    )
    .await;
    // An empty pre-merge source: every scope tails.
    let model = read_model_for_scopes(
        &db,
        &["hollow"],
        &[("ghost", "gdb"), ("alpha", "ad"), ("beta", "bd")],
    )
    .await;
    let (file, diags) = surform::parser::parse_file("");
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(
        out,
        canon(
            "
             namespace alpha;
             database ad;
             table a1 {
               schemaless;
             };
             namespace beta;
             database bd;
             table b1 {
               schemaless;
             };
             namespace ghost;
             database gdb;
             table z {
               schemaless;
             };
             namespace hollow;"
        )
    );
    assert_fmt_fixed_point(&out);
}

#[tokio::test]
async fn ns_scope_adoptees_insert_after_their_namespace() {
    // An ns-level user with no SDL counterpart adopts directly after
    // the SDL's namespace statement (the ns scope's anchor) — ahead of
    // the database statement, never dropped and never re-anchored.
    let db = exec::mem_db().await;
    db.use_ns("test").await.ok();
    db.query("DEFINE USER nsu ON NAMESPACE PASSWORD 'secret';")
        .await
        .expect("ns user defines")
        .check()
        .expect("ns user executes");
    db.use_ns("test").use_db("scratch").await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; table t { schemaless; };",
    )
    .await;
    let sdl = "namespace test;\ndatabase scratch;\ntable t { schemaless; };\n";
    let model = read_model_for(&db, "test", "scratch").await;
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(
        out,
        canon(
            "
             namespace test;
             user nsu {
               on: namespace;
             };
             database scratch;
             table t {
               schemaless;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
}

// ---------------------------------------------------------------------
// Canonical placement: the dependency-bounded topological rule
// ---------------------------------------------------------------------

#[tokio::test]
async fn adopted_view_places_after_the_tables_its_query_reads() {
    let db = exec::mem_db().await;
    db.use_ns("test").use_db("scratch").await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; table user { schemaless; }; \
         table post { schemaless; };",
    )
    .await;
    // The INFO map is alphabetical (post, recent, user) — family/map
    // order alone would place the view first; its query reads user and
    // post, so the dependency bound carries it behind both.
    db.query("DEFINE TABLE recent AS SELECT id, title FROM user, post;")
        .await
        .expect("view defines")
        .check()
        .expect("view executes");
    let sdl = "namespace test;\ndatabase scratch;\ntable user { schemaless; };\ntable post { schemaless; };\n";
    let model = read_model_for(&db, "test", "scratch").await;
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(
        out,
        canon(
            "
             namespace test;
             database scratch;
             table `user` {
               schemaless;
             };
             table `post` {
               schemaless;
             };
             table view recent {
               SELECT id, title FROM user, post;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
}

#[tokio::test]
async fn fulltext_index_lifts_when_its_analyzer_places_after_the_table() {
    // The analyzer is authored after the table: nested placement would
    // define the fulltext index before its analyzer (rule 7 — SDL's
    // only guard), so the index lifts to top level right after it.
    converge(
        "namespace test;
         database scratch;
         table doc {
           schemaless;
           index fulltext body_idx { fields: body; analyzer: sprache; };
         };
         analyzer sprache { tokenizers: class; };",
        "
         namespace test;
         database scratch;
         table doc {
           schemaless;
         };
         analyzer sprache {
           tokenizers: class;
         };
         index fulltext body_idx {
           on: doc;
           fields: body;
           analyzer: sprache;
         };",
    )
    .await;
}

#[tokio::test]
async fn fulltext_index_stays_nested_when_its_analyzer_places_before_the_table() {
    converge(
        "namespace test;
         database scratch;
         analyzer sprache {
           tokenizers: class;
         };
         table doc {
           schemaless;
           index fulltext body_idx {
             fields: body;
             analyzer: sprache;
           };
         };",
        "
         namespace test;
         database scratch;
         analyzer sprache {
           tokenizers: class;
         };
         table doc {
           schemaless;
           index fulltext body_idx {
             fields: body;
             analyzer: sprache;
           };
         };",
    )
    .await;
}

#[tokio::test]
async fn standalone_field_folds_into_its_table_when_identical() {
    // A top-level `field` merges nothing (INFO carries fields only as
    // table children), so the authored standalone survives the merge —
    // and canonical placement folds it away when the table carries the
    // same definition nested (the duplication family).
    converge(
        "namespace test;
         database scratch;
         table user { schemaless; };
         field email: string { on: user; };",
        "
         namespace test;
         database scratch;
         table `user` {
           schemaless;
           field email: string;
         };",
    )
    .await;
}

#[tokio::test]
async fn analyzer_places_after_the_function_its_function_clause_names() {
    let db = exec::mem_db().await;
    db.use_ns("test").use_db("scratch").await.ok();
    exec::exec_sdl(&db, "namespace test; database scratch;").await;
    // Family order alone puts the analyzer (A) before the function
    // (F) — the analyzer's `function:` fn binds it after instead.
    db.query("DEFINE FUNCTION fn::stem($t: string) { RETURN $t; };")
        .await
        .expect("fn defines")
        .check()
        .expect("fn executes");
    db.query("DEFINE ANALYZER sprache FUNCTION fn::stem FILTERS LOWERCASE;")
        .await
        .expect("analyzer defines")
        .check()
        .expect("analyzer executes");
    let sdl = "namespace test;\ndatabase scratch;\n";
    let model = read_model_for(&db, "test", "scratch").await;
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(
        out,
        canon(
            "
             namespace test;
             database scratch;
             fn stem($t: string) { RETURN $t };
             analyzer sprache {
               function: fn::stem;
               filters: lowercase;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
}

#[tokio::test]
async fn no_dependency_adoptees_take_their_sections_earliest_slot() {
    let db = exec::mem_db().await;
    db.use_ns("test").use_db("scratch").await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; table user { schemaless; };",
    )
    .await;
    db.query("DEFINE PARAM $limit VALUE 10;")
        .await
        .expect("param defines")
        .check()
        .expect("param executes");
    let sdl = "namespace test;\ndatabase scratch;\ntable user { schemaless; };\n";
    let model = read_model_for(&db, "test", "scratch").await;
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    // The earliest slot after the db section's opener — not the tail
    // (the interim posture, replaced by the spec's rule).
    assert_eq!(
        out,
        canon(
            "
             namespace test;
             database scratch;
             param $limit = 10;
             table `user` {
               schemaless;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
}

/// Hand-fed (a real engine round trip cannot produce these shapes):
/// the interchange literals go straight through
/// `ReadModel::from_interchange`, the way a partially-applied file's
/// INFO would arrive.
fn hand_fed_merge(sdl: &str, db_text: &str, db_children_text: Option<&str>) -> String {
    let mut db_obj = read_info_object(db_text).expect("db INFO walks");
    if let Some(children) = db_children_text {
        let obj = read_info_object(children).expect("table INFO walks");
        db_obj
            .0
            .insert(TABLE_CHILDREN_KEY.to_string(), InfoValue::Object(obj));
    }
    let model = ReadModel::from_interchange(Interchange {
        root: None,
        namespaces: &[],
        databases: &[("test".to_string(), "scratch".to_string(), &db_obj)],
    })
    .expect("the interchange reads");
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty());
    write_canonical(&merge(&file, &model))
}

#[test]
fn surfaced_view_children_order_after_their_view_table() {
    // A view's grammar has no nested-define spelling: its children
    // surface top-level as standalone entries. The index's `on:` table
    // is the view itself — the dependency bound must beat family order
    // (Index sorts before Table), or the index would precede the view.
    let out = hand_fed_merge(
        "namespace test;\ndatabase scratch;\n",
        "{ tables: { recent: 'DEFINE TABLE recent TYPE ANY SCHEMALESS AS \
         SELECT id FROM user', user: 'DEFINE TABLE user TYPE ANY \
         SCHEMALESS PERMISSIONS NONE' } }",
        Some(
            "{ recent: { fields: {}, indexes: { idx: 'DEFINE INDEX idx ON \
             recent FIELDS id' }, events: {} } }",
        ),
    );
    assert_eq!(
        out,
        canon(
            "
             namespace test;
             database scratch;
             table any `user` {
               schemaless;
             };
             table view recent {
               SELECT id FROM user;
             };
             index idx {
               on: recent;
               fields: id;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
}

#[test]
fn divergent_standalone_stays_for_the_differ() {
    // The standalone carries clauses the table's nested twin lacks:
    // folding it away would hide the drift, so both survive (the
    // differ reports; the linter flags the non-nested form).
    let out = hand_fed_merge(
        "namespace test;
         database scratch;
         table user { schemaless; };
         field email: string { assert: self != none; on: user; };\n",
        "{ tables: { user: 'DEFINE TABLE user TYPE ANY SCHEMALESS \
         PERMISSIONS NONE' } }",
        Some(
            "{ user: { fields: { email: 'DEFINE FIELD email ON user TYPE \
             string PERMISSIONS FULL' }, indexes: {}, events: {} } }",
        ),
    );
    assert_eq!(
        out,
        canon(
            "
             namespace test;
             database scratch;
             table any `user` {
               schemaless;
               field email: string;
             };
             field email: string {
               on: `user`;
               assert: self != NONE;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
}

/// The INLINE flag rides the twin fold's value equality: a
/// standalone `inline;` field folds into its table's identical nested
/// twin, and the twin with the flag set stays (flag-difference is the
/// differ's drift).
#[test]
fn inline_flag_rides_the_twin_fold() {
    let out = hand_fed_merge(
        "namespace test;
         database scratch;
         table a -> likes -> b { enforced; field score: number { inline; }; };",
        "{ tables: { likes: 'DEFINE TABLE likes TYPE RELATION IN a OUT b ENFORCED \
         PERMISSIONS NONE' } }",
        Some(
            "{ likes: { fields: { score: 'DEFINE FIELD score ON likes TYPE number \
             INLINE PERMISSIONS FULL' }, indexes: {}, events: {} } }",
        ),
    );
    // The identical nested twin folds the standalone away.
    assert!(!out.contains("on:"));
    assert!(out.contains("inline;"));
    assert_fmt_fixed_point(&out);
}

// ---------------------------------------------------------------------
// The multi-scope enclosure property (adoptees merge per
// ns/db section, never across a `database` boundary, property-tested)
// ---------------------------------------------------------------------

/// Whether a merged statement is scope-bound (binds to the walk's open
/// scope, so it must sit inside a database section) — users/accesses
/// carry their `on:` level, `config default` is ROOT-region by design,
/// and raw bodies ride verbatim.
fn is_scope_bound(stmt: &surform::ast::Stmt) -> bool {
    use surform::ast::Stmt as S;
    !matches!(
        stmt,
        S::Namespace(_)
            | S::Database(_)
            | S::User(_)
            | S::Access(_)
            | S::Raw(_)
            | S::Config(surform::ast::Config::Default(_))
    )
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(512))]

    /// Two mutated db interchanges (separate namespaces, both named
    /// `scratch` so the fixture's own `ON scratch` strings stay
    /// coherent) plus fixed ROOT and NS scopes against a declared
    /// ns+db: every scope-bound statement of the merged output sits
    /// inside a database section — its own, or the synthesized tail
    /// section for the unvisited scope — never across a `database`
    /// boundary, and the file stays fmt-stable.
    #[test]
    fn multi_scope_merge_keeps_section_enclosure(
        cut in 0usize..400,
        edits in proptest::collection::vec(
            (0usize..400, proptest::sample::select(vec![
                b'{', b'}', b':', b',', b'\'', b'"', b';', b'\\', b'a', b' ',
            ])),
            0..12,
        ),
        cut2 in 0usize..400,
        edits2 in proptest::collection::vec(
            (0usize..400, proptest::sample::select(vec![
                b'{', b'}', b':', b',', b'\'', b'"', b';', b'\\', b'a', b' ',
            ])),
            0..12,
        ),
    ) {
        let root = read_info_object(
            "{ users: { r: 'DEFINE USER r ON ROOT PASSHASH \\'x\\'' } }",
        )
        .ok();
        let ns = read_info_object(
            "{ users: { u: 'DEFINE USER u ON NAMESPACE PASSHASH \\'x\\'' } }",
        )
        .ok();
        let dbs: Vec<(String, String, Option<surform::info::read::InfoObject>)> = vec![
            (
                "test".to_string(),
                "scratch".to_string(),
                read_info_object(&common::corrupt(
                    common::INTERCHANGE_FIXTURE,
                    cut,
                    &edits,
                ))
                .ok(),
            ),
            (
                "test2".to_string(),
                "scratch".to_string(),
                read_info_object(&common::corrupt(
                    common::INTERCHANGE_FIXTURE,
                    cut2,
                    &edits2,
                ))
                .ok(),
            ),
        ];
        if let (Some(root), Some(ns)) = (&root, &ns)
            && dbs.iter().all(|(_, _, obj)| obj.is_some())
        {
            let mut model = ReadModel::default();
            let mut clean = true;
            clean &= model.push_scope(
                surform::info::normalize::ScopeTag::Root,
                root,
            ).is_ok();
            clean &= model.push_scope(
                surform::info::normalize::ScopeTag::Namespace("test".to_string()),
                ns,
            ).is_ok();
            for (ns_name, db_name, obj) in &dbs {
                clean &= model
                    .push_scope(
                        surform::info::normalize::ScopeTag::Database(
                            ns_name.clone(),
                            db_name.clone(),
                        ),
                        obj.as_ref().expect("gated above"),
                    )
                    .is_ok();
            }
            if clean {
                let (file, _) = surform::parser::parse_file(
                    "namespace test;\ndatabase scratch;\n",
                );
                let out = write_canonical(&merge(&file, &model));
                let (reparsed, diags) = surform::parser::parse_file(&out);
                proptest::prop_assert!(diags.is_empty(), "merge output stopped parsing:\n{out}");
                let restated = write_canonical(&reparsed);
                proptest::prop_assert_eq!(restated, out.clone(), "merge output is not fmt-stable");
                let mut in_db = false;
                for stmt in &reparsed.items {
                    match stmt {
                        surform::ast::Stmt::Namespace(_) => in_db = false,
                        surform::ast::Stmt::Database(_) => in_db = true,
                        other => {
                            if is_scope_bound(other) {
                                proptest::prop_assert!(
                                    in_db,
                                    "a scope-bound statement sits outside a \
                                     database section:\n{out}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A `namespace` statement authored after a
/// `database` statement keeps its authored point — hoisting it to the
/// file head would re-bind `database shop` (and its whole section) to
/// the wrong namespace, and the in-place workflow would then brick the
/// file against its own interchange (E3005 on re-validation).
#[test]
fn namespaces_keep_their_authored_points() {
    let sdl = "namespace acme;\ndatabase shop;\ntable x { schemaless; };\n\
               namespace other;\ndatabase solo;\ntable z { schemaless; };\n";
    let shop = read_info_object(
        "{ tables: { x: 'DEFINE TABLE x TYPE NORMAL SCHEMALESS \
         PERMISSIONS NONE' } }",
    )
    .expect("walks");
    let solo = read_info_object(
        "{ params: { p: 'DEFINE PARAM $p VALUE 2 \
         PERMISSIONS FULL' } }",
    )
    .expect("walks");
    let dbs = vec![
        ("acme".to_string(), "shop".to_string(), &shop),
        ("other".to_string(), "solo".to_string(), &solo),
    ];
    let model = ReadModel::from_interchange(Interchange {
        root: None,
        namespaces: &[],
        databases: &dbs,
    })
    .expect("the interchange reads");
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty());
    let out = write_canonical(&merge(&file, &model));
    assert_eq!(
        out,
        canon(
            "
             namespace acme;
             database shop;
             table x {
               schemaless;
             };
             namespace other;
             database solo;
             param $p = 2;
             table z {
               schemaless;
             };"
        )
    );
    assert_fmt_fixed_point(&out);
    // The second in-place run must still validate: the reordered
    // output keeps every declared scope the tags name.
    let (out_file, _) = surform::parser::parse_file(&out);
    assert!(
        surform::info::normalize::validate_scope_tag(
            &surform::info::normalize::ScopeTag::Database("acme".to_string(), "shop".to_string()),
            &out_file,
        )
        .is_ok()
    );
    assert!(
        surform::info::normalize::validate_scope_tag(
            &surform::info::normalize::ScopeTag::Database("other".to_string(), "solo".to_string()),
            &out_file,
        )
        .is_ok()
    );
}
