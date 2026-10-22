//! The full-product oracle (the testing policy's
//! manual/nightly layer): every clause cross-product doc through the
//! WHOLE pipeline — SDL → codegen → embedded parse → embedded EXEC →
//! INFO capture → read → merge. `#[ignore]` by design (the exec
//! engine makes this minutes, not milliseconds):
//!
//! ```sh
//! cargo test --test oracle_full -- --ignored --test-threads=4
//! ```
//!
//! The fixed point, per doc, is IDEMPOTENCE — the honest form:
//! applying the loop to its own output (a fresh engine executes the
//! merged SDL, INFO reads back, the merge re-runs) reproduces the
//! merged output byte-for-byte, the differ holds that state clean
//! (the clean-apply property over the whole corpus), and the
//! output is fmt-stable. Byte-equality to the INPUT is deliberately
//! NOT asserted: the merge elides clauses whose authored value is
//! the SurrealQL default (canonical SDL is minimal — `batch: 1000;`
//! merges to `sequence n;`, the oracle's own second catch), so only
//! already-minimal inputs converge in one round — idempotence is
//! the one-round-later property that holds for every input. Docs
//! whose evaluated clauses hold non-literal terminals (the
//! parse-coverage spice) skip the exec leg with their reason
//! recorded in the menu; a skipped count is asserted per family so
//! the skip set cannot silently grow.

#[path = "common/mod.rs"]
mod common;
#[path = "common/exec.rs"]
mod exec;

use std::collections::BTreeMap;

use common::menus;
use surform::info::normalize::{Interchange, ReadModel, TABLE_CHILDREN_KEY, merge};
use surform::info::read::{InfoObject, InfoValue, read_info_object};
use surform::info::write::write_canonical;
use surrealdb::Surreal;
use surrealdb::engine::local::Db;

const NS: &str = "test";
const DB: &str = "scratch";

/// The read model over the session's three INFO scopes, with the
/// `INFO FOR TABLE` child maps assembled into the db object's
/// reserved [`TABLE_CHILDREN_KEY`] (the interchange files' test
/// twin — `read_model_for`'s shape, shared by the exec suites).
async fn read_model_for(db: &Surreal<Db>) -> ReadModel {
    let root_text = exec::info_for(db, "INFO FOR ROOT").await;
    let ns_text = exec::info_for(db, "INFO FOR NS").await;
    let db_text = exec::info_for(db, "INFO FOR DB").await;
    let root = read_info_object(&root_text).expect("root INFO walks");
    let ns_obj = read_info_object(&ns_text).expect("ns INFO walks");
    let mut db_obj = read_info_object(&db_text).expect("db INFO walks");
    let mut children = BTreeMap::new();
    if let Some(InfoValue::Object(tables)) = db_obj.0.get("tables") {
        for name in tables.0.keys() {
            // Backtick-escaped: the menus exercise keyword-colliding
            // and spaced table names (`my table`).
            let text = exec::info_for(db, &format!("INFO FOR TABLE `{name}`")).await;
            let obj = read_info_object(&text).expect("table INFO walks");
            children.insert(name.clone(), InfoValue::Object(obj));
        }
    }
    db_obj.0.insert(
        TABLE_CHILDREN_KEY.to_string(),
        InfoValue::Object(InfoObject(children)),
    );
    let namespaces = vec![(NS.to_string(), &ns_obj)];
    let databases = vec![(NS.to_string(), DB.to_string(), &db_obj)];
    ReadModel::from_interchange(Interchange {
        root: Some(&root),
        namespaces: &namespaces,
        databases: &databases,
    })
    .expect("every scope reads")
}

/// One doc's end-to-end leg. The doc's FILE is the family's setup
/// prepended to the generated doc — the engine executes exactly
/// that file, so INFO and the file hold the same definitions and
/// convergence is the honest claim (a setup the file did not carry
/// would surface as adoptees in the merge, the oracle's own first
/// catch). Returns whether the exec hop ran (the caller pins the
/// skip counts).
async fn one_doc(sdl: &str, setup: &str, exec_doc: bool, family: &str) -> bool {
    let full = format!("{setup}{sdl}");

    // The parse-side legs run for every doc.
    let (file, diags) = surform::parser::parse_file(&full);
    assert!(diags.is_empty(), "{family}: {full}: {diags:?}");
    let ddl = surform::codegen::emit(&file);
    common::assert_oracle_accepts_files(&ddl);

    if !exec_doc {
        return false;
    }

    // The exec hop: a fresh engine, the whole file, then the loop
    // again on the merged output — idempotence is the fixed point.
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(&db, &full).await;
    let model = read_model_for(&db).await;
    let merged = write_canonical(&merge(&file, &model));

    // The second round: the merged output executes cleanly on a
    // fresh engine, and the loop reproduces it byte-for-byte.
    let db2 = exec::mem_db().await;
    db2.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(&db2, &merged).await;
    let model2 = read_model_for(&db2).await;
    let (reparsed, diags) = surform::parser::parse_file(&merged);
    assert!(
        diags.is_empty(),
        "{family}: merged output stopped parsing:\n{merged}"
    );
    let merged2 = write_canonical(&merge(&reparsed, &model2));
    assert_eq!(
        merged2, merged,
        "{family}: the loop is not idempotent\n--- SDL ---\n{full}\n--- round 1 \
         ---\n{merged}\n--- round 2 ---\n{merged2}"
    );
    // The differ holds the merged state clean (the clean-apply
    // property, corpus-wide).
    let findings = surform::info::diff::diff(&reparsed, &model2);
    assert!(
        findings.is_empty(),
        "{family}: the merged state does not diff clean\n--- SDL ---\n{full}\n--- \
         merged ---\n{merged}\n--- findings ---\n{findings:#?}"
    );
    // fmt-stability of the merged output.
    assert_eq!(
        write_canonical(&reparsed),
        merged,
        "{family}: merged output is not fmt-stable:\n{merged}"
    );
    true
}

/// Every family, every doc. One `#[tokio::test]` for the whole layer
/// (per-family parallelism buys little against engine startup; the
/// recipe below parallelizes at the process level when wanted).
#[tokio::test]
#[ignore = "the manual/nightly full-product layer (see the module doc for the invocation)"]
async fn every_full_product_doc_holds_the_reverse_fixed_point() {
    for family in menus::families() {
        let docs = (family.docs)();
        let mut execd = 0;
        for sdl in &docs {
            if one_doc(sdl, family.setup, family.execs(sdl), family.name).await {
                execd += 1;
            }
        }
        // The absolute pin (the audit holds the per-commit copy): a
        // marker edit that silently grows the skip set breaks it.
        assert_eq!(
            execd, family.expected_execd,
            "{}: the exec count moved — update the pin WITH the marker edit",
            family.name
        );
        // Every family exercises the exec hop somewhere (a family
        // whose docs all skip belongs in the golden-only set with a
        // recorded reason, like module).
        assert!(execd > 0, "{}: no doc exercised the exec hop", family.name);
    }
}
