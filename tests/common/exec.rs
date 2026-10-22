//! The embedded-engine harness (Layer 4) shared by the exec suites:
//! `normalize`, `info_read`, and `diff`. Included per suite with
//! `#[path = "common/exec.rs"] mod exec;` — `common/mod.rs`'s oracle
//! helpers stay the non-exec suites' home, so nothing compiles helpers
//! it never uses.
//!
//! SurrealDB access is embedded-only: the
//! engine runs in-process with every experimental capability enabled
//! (`files` + `surrealism` — the bucket gate sits inside the
//! parser), never a server.

use surrealdb::Surreal;
use surrealdb::engine::local::Db;

/// The in-process engine with the experimental capabilities enabled.
pub async fn mem_db() -> Surreal<Db> {
    use surrealdb::opt::capabilities::Capabilities;
    let config = surrealdb::opt::Config::default()
        .capabilities(Capabilities::default().with_all_experimental_features_allowed());
    Surreal::new::<surrealdb::engine::local::Mem>(config)
        .await
        .expect("the embedded engine starts")
}

/// One INFO query's rendered object text (the engine's own `ToSql` —
/// the printer dialect the readers walk).
pub async fn info_for(db: &Surreal<Db>, query: &str) -> String {
    let info: surrealdb::types::Value = db
        .query(query)
        .await
        .expect("the INFO query runs")
        .take(0)
        .expect("the INFO result takes");
    use surrealdb::types::ToSql as _;
    info.to_sql()
}

/// Parses SDL, emits DDL, and executes it — the suite's apply step
/// (a failure is a test failure: these legs exec clean inputs; the
/// message carries the SDL so a family-menu gap points at its doc).
pub async fn exec_sdl(db: &Surreal<Db>, sdl: &str) {
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{sdl}: {diags:?}");
    let ddl = surform::codegen::emit(&file);
    let result = db.query(&ddl).await.expect("the query runs").check();
    if let Err(error) = result {
        panic!("the DDL does not execute: {error}\n--- SDL ---\n{sdl}\n--- DDL ---\n{ddl}");
    }
}
