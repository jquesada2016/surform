//! The staged-migration suite (issue #65): the engine contract the
//! staging stands on, probe-pinned against the embedded 3.3.0 engine
//! (2026-10-11), and the end-to-end leg — the issue's exact failure
//! shape driven through the real pipeline (apply → INFO → read →
//! diff → script) with the data section filled and executed.
//!
//! The contract, each pinned below: a field kind change NEVER
//! re-validates existing rows at define time; a data step that
//! rewrites the values FAILS while the old kind is in force (the
//! error names the existing row, which reads like a define-time
//! rejection — the issue's false premise); the staged order
//! (relaxed definitions → data step → final definitions) works
//! inside one transaction; a UNIQUE index over duplicate rows is
//! the one row-validating define; permission tightening never
//! validates rows.

#[path = "common/mod.rs"]
mod common;
#[path = "common/exec.rs"]
mod exec;

use std::collections::BTreeMap;
use surform::info::diff::diff;
use surform::info::normalize::{Interchange, ReadModel, TABLE_CHILDREN_KEY};
use surform::info::read::{InfoObject, InfoValue, read_info_object};
use surform::info::script::convergence_script;
use surrealdb::Surreal;
use surrealdb::engine::local::Db;

const NS: &str = "test";
const DB: &str = "scratch";

/// The issue's edge baseline: an `in` field re-keyed from the union
/// kind to the new `record<seat>` while a stale edge row exists.
const BASELINE: &str = "namespace test; database scratch; \
     table member_of { field in: record<user> | record<fleet_account>; field out: record<room>; };";
const TARGET: &str = "namespace test; database scratch; \
     table member_of { field in: record<seat>; field out: record<room>; };";

/// The read model over the session's three INFO scopes (tests/diff.rs's
/// harness, inlined — test crates cannot share imports).
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

async fn seeded_db() -> Surreal<Db> {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(&db, BASELINE).await;
    db.query(
        "USE NS test DB scratch; CREATE member_of:one SET in = fleet_account:abc, out = room:xyz;",
    )
    .await
    .expect("the seed runs")
    .check()
    .expect("the stale edge seeds");
    db
}

/// The engine contract, one leg per pinned fact.
#[tokio::test]
async fn the_engine_contract_behind_the_staging() {
    // A kind change re-validates NOTHING at define time — the define
    // the issue blamed sails over the stale row.
    let db = seeded_db().await;
    db.query("USE NS test DB scratch; DEFINE FIELD OVERWRITE `in` ON member_of TYPE record<seat>;")
        .await
        .expect("the query runs")
        .check()
        .expect("the re-key define accepts the stale row");

    // The trap: the data step UNDER the old kind fails, naming the
    // existing row — the failure the issue attributed to the define.
    let db = seeded_db().await;
    let error = db
        .query("USE NS test DB scratch; UPDATE member_of SET in = seat:r1;")
        .await
        .expect("the query runs")
        .check()
        .expect_err("the old kind rejects the new value");
    assert!(error.to_string().contains("Couldn't coerce"), "{error}");

    // The one row-validating define: a UNIQUE index over duplicate
    // rows fails at build time.
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; table t { field v: string; };",
    )
    .await;
    db.query("USE NS test DB scratch; CREATE t:one SET v = 'a'; CREATE t:two SET v = 'a';")
        .await
        .expect("the seeds run")
        .check()
        .expect("the duplicates seed");
    let error = db
        .query("USE NS test DB scratch; DEFINE INDEX OVERWRITE uq ON t FIELDS v UNIQUE;")
        .await
        .expect("the query runs")
        .check()
        .expect_err("the unique build fails over duplicates");
    assert!(error.to_string().contains("already contains"), "{error}");

    // Permission tightening is catalog-only — never a row validation.
    let db = seeded_db().await;
    db.query(
        "USE NS test DB scratch; DEFINE TABLE OVERWRITE member_of TYPE NORMAL SCHEMALESS \
         PERMISSIONS FOR select, create, update, delete NONE;",
    )
    .await
    .expect("the query runs")
    .check()
    .expect("restrictive permissions apply over rows");
}

/// The end-to-end leg: the issue's failure shape through the real
/// pipeline. The generated script carries the staged prelude; the
/// author's filled data section rewrites the stale edge; the final
/// kind lands and governs; the target re-diffs clean.
#[tokio::test]
async fn the_generated_staged_script_migrates_the_stale_edge() {
    let db = seeded_db().await;
    let (to_file, diags) = surform::parser::parse_file(TARGET);
    assert!(diags.is_empty(), "{diags:?}");
    let model = read_model_for(&db, NS, DB).await;
    let findings = diff(&to_file, &model);

    let script = convergence_script(&findings, &to_file);
    // The staged prelude rode: shell, kind relax, the data section.
    let shell_at = script
        .find("DEFINE TABLE OVERWRITE member_of TYPE ANY SCHEMALESS;\n")
        .expect("the staged shell rides");
    let relax_at = script
        .find("DEFINE FIELD OVERWRITE `in` ON member_of TYPE any;\n")
        .expect("the kind relax rides");
    let open_at = script
        .find("-- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n")
        .expect("the data section rides");
    let close_at = script
        .find("-- >>> SURFORM REQUIRED: END <<<\n")
        .expect("the data section closes");
    let final_at = script
        .find("DEFINE FIELD OVERWRITE `in` ON member_of TYPE record<seat>;\n")
        .expect("the final define rides");
    assert!(shell_at < relax_at && relax_at < open_at && open_at < close_at && close_at < final_at);

    // The author fills the section — the staged pattern's data step.
    let mut filled = String::with_capacity(script.len());
    filled.push_str(&script[..open_at + open_marker_len()]);
    filled.push_str("UPDATE member_of SET in = seat:r1;\n");
    filled.push_str(&script[close_at..]);
    // The data step sits between the banners (they stay — permanent
    // documentation on this carrier).
    let step_at = filled
        .find("UPDATE member_of SET in = seat:r1;\n")
        .expect("the fill");
    assert!(open_at < step_at && step_at < close_at);

    // The whole script executes in one call — the frame's transaction
    // lands, the stale edge rewrites under the relaxed kind, the
    // final kind applies over the rewritten row.
    db.query(format!("USE NS test DB scratch; {filled}"))
        .await
        .expect("the script runs")
        .check()
        .expect("the filled staged script applies");

    // The data step landed: the edge's `in` is the new record (`in`
    // is a SurrealQL operator word — the escaped spelling everywhere
    // in probe queries).
    use surrealdb::types::ToSql as _;
    let rows = db
        .query("USE NS test DB scratch; SELECT VALUE `in` FROM member_of:one;")
        .await
        .expect("the probe runs")
        .take::<Vec<surrealdb::types::Value>>(1)
        .expect("the probe takes");
    assert_eq!(rows.len(), 1, "the edge row stands");
    assert!(
        rows[0].to_sql().contains("seat:r1"),
        "the stale edge re-keyed: {}",
        rows[0].to_sql()
    );

    // The final kind governs new writes.
    let error = db
        .query("USE NS test DB scratch; CREATE member_of:two SET in = fleet_account:abc, out = room:xyz;")
        .await
        .expect("the query runs")
        .check()
        .expect_err("the final kind rejects the old shape");
    assert!(error.to_string().contains("Couldn't coerce"), "{error}");

    // And the target converged: re-read, re-diff — empty.
    let model = read_model_for(&db, NS, DB).await;
    let findings = diff(&to_file, &model);
    assert!(
        findings.is_empty(),
        "the staged script converges the target:\n{findings:#?}"
    );
}

fn open_marker_len() -> usize {
    "-- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n".len()
}

/// The un-filled draft's marker pair is un-runnable SurrealQL — the
/// commit gate's contract, pinned here against the staged slot's
/// spellings.
#[test]
fn the_staged_slot_prose_names_the_table_and_field() {
    let (to_file, diags) = surform::parser::parse_file(TARGET);
    assert!(diags.is_empty(), "{diags:?}");
    // A hand-built finding pair mirrors the differ's (the engine legs
    // above carry the pipeline end to end; this pin is text-level).
    use surform::info::diff::{DriftClass, DriftFinding};
    use surform::info::normalize::{Family, ScopeKey};
    let (sdl_field, _) = surform::parser::parse_file("field in: record<seat> { on: member_of; };");
    let (db_field, _) = surform::parser::parse_file(
        "field in: record<user> | record<fleet_account> { on: member_of; };",
    );
    let finding = DriftFinding {
        on_table_exists: true,
        scope: ScopeKey::Database("test".into(), "scratch".into()),
        family: Family::Field,
        key: "in".to_string(),
        class: DriftClass::Drift {
            sdl: Box::new(sdl_field.items.into_iter().next().expect("the sdl side")),
            database: Box::new(db_field.items.into_iter().next().expect("the db side")),
            markers: Box::default(),
        },
    };
    let script = convergence_script(&[finding], &to_file);
    assert!(script.contains("-- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n"));
    assert!(script.contains("-- e.g. UPDATE member_of SET `in` = seat:r1;\n"));
}
