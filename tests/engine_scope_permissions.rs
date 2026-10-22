//! The engine's scope-permission shapes, pinned in-process (issue
//! #103's ground). The embedded engine runs the same code the server
//! runs, so these are the wire refusals' source: a db-level
//! credential — exactly the audience the bearer-token transport
//! (#99) serves — reads its own database and nothing above it, an
//! ns-level credential stops one scope shorter, and every refusal
//! carries the stable `NotAllowed` kind (details.kind `Auth`, the
//! IAM "not enough permissions" verdict). Pull's scopes leg branches
//! on that kind — never on message text — when it degrades to the
//! readable scopes (issue #103); these pins are the branch key's
//! evidence, re-verified against the engine on every run.

use surrealdb::Surreal;
use surrealdb_types::{AuthError, ErrorDetails, NotAllowedError};

/// The in-process engine with the experimental capabilities enabled,
/// holding one namespace (`test`), one database (`scratch`) with a
/// table, and three users: a root user, an ns-level OWNER, and a
/// db-level OWNER.
async fn engine_with_users() -> Surreal<surrealdb::engine::local::Db> {
    let config = surrealdb::opt::Config::default().capabilities(
        surrealdb::opt::capabilities::Capabilities::default()
            .with_all_experimental_features_allowed(),
    );
    let db = Surreal::new::<surrealdb::engine::local::Mem>(config)
        .await
        .expect("the embedded engine starts");
    db.use_ns("test").await.expect("the ns selects");
    db.use_db("scratch").await.expect("the db selects");
    db.query(
        "DEFINE TABLE user TYPE NORMAL SCHEMALESS; \
         DEFINE FIELD email ON user TYPE string; \
         DEFINE USER probe_root ON ROOT PASSWORD 'rootpass' ROLES OWNER; \
         DEFINE USER nspull ON NAMESPACE PASSWORD 'nspass' ROLES OWNER; \
         DEFINE USER dbpull ON DATABASE PASSWORD 'dbpass' ROLES OWNER;",
    )
    .await
    .expect("the defines run")
    .check()
    .expect("the defines check");
    db
}

/// The permission verdict's shape: the stable `NotAllowed` kind with
/// the `Auth` reason carrying the IAM verdict — never matched on
/// message text anywhere downstream.
fn assert_permission_refused(error: surrealdb_types::Error) {
    assert_eq!(error.kind_str(), "NotAllowed", "{error:?}");
    assert!(
        matches!(
            error.details(),
            ErrorDetails::NotAllowed(Some(NotAllowedError::Auth(AuthError::NotAllowed { .. })))
        ),
        "{error:?}"
    );
}

/// A db-level credential (ROLES OWNER on DATABASE) reads its own
/// database and nothing above it: ROOT and NS answer the permission
/// refusal, DB and the per-table reads are OK. The scopes leg's
/// degrade-on-NotAllowed branch (issue #103) rests on exactly this.
#[tokio::test]
async fn a_db_level_credential_reads_its_database_only() {
    let db = engine_with_users().await;
    db.signin(surrealdb::opt::auth::Database {
        namespace: "test".into(),
        database: "scratch".into(),
        username: "dbpull".into(),
        password: "dbpass".into(),
    })
    .await
    .expect("the db-level signin");
    for refused in ["INFO FOR ROOT", "INFO FOR NS"] {
        let error = db
            .query(refused)
            .await
            .expect("the query dispatches")
            .check()
            .expect_err("the scope above the credential is refused");
        assert_permission_refused(error);
    }
    for readable in ["INFO FOR DB", "INFO FOR TABLE user"] {
        db.query(readable)
            .await
            .expect("the query dispatches")
            .check()
            .unwrap_or_else(|error| panic!("{readable} must read: {error:?}"));
    }
}

/// An ns-level credential reads through its namespace: ROOT answers
/// the permission refusal, NS and DB are OK — the permission model
/// is a strict prefix, so the scopes leg's degrade ladder is
/// {root, ns, db} / {ns, db} / {db}.
#[tokio::test]
async fn an_ns_level_credential_reads_through_its_namespace() {
    let db = engine_with_users().await;
    db.signin(surrealdb::opt::auth::Namespace {
        namespace: "test".into(),
        username: "nspull".into(),
        password: "nspass".into(),
    })
    .await
    .expect("the ns-level signin");
    let error = db
        .query("INFO FOR ROOT")
        .await
        .expect("the query dispatches")
        .check()
        .expect_err("the scope above the credential is refused");
    assert_permission_refused(error);
    for readable in ["INFO FOR NS", "INFO FOR DB"] {
        db.query(readable)
            .await
            .expect("the query dispatches")
            .check()
            .unwrap_or_else(|error| panic!("{readable} must read: {error:?}"));
    }
}
