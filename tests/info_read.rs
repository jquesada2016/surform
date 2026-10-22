//! The INFO ingestion + DDL readers over LIVE engine output: reader
//! goldens captured from the embedded engine's INFO
//! results — never hand-written — and the reverse fixed-point leg per
//! statement: SDL → DDL → exec → INFO → read → SDL′, structure-equal
//! modulo the pinned materializations (the engine defaults that
//! INFO always reports and the reader therefore carries).
//!
//! The reverse cross products use literal terminals only: INFO
//! reports the define-time EVALUATED state, so an SDL expression
//! like `1 + 1` cannot appear in it (the one non-literal case is
//! pinned by the param golden instead). The forward oracle's richer
//! expression terminals (tests/oracle_smoke.rs) stay parse-level for
//! exactly that reason. No `mod common`: this suite needs the fmt
//! fixed point but not the parse-oracle helpers, and each test
//! binary compiles `common` separately (fmt_property.rs's precedent
//! — it keeps its own copy too).

//! The engine harness rides `tests/common` (the shared
//! home: `#[path = "common/exec.rs"]`); this suite's golden machinery
//! (`reverse_leg` and friends) stays local, as does the owned fmt
//! fixed-point copy the header notes.

#[path = "common/exec.rs"]
mod exec;

use std::collections::BTreeMap;

use surform::ast::{
    Access, AccessDuration, AccessKind, Algorithm, Analyzer, Api, ApiAction, ApiAnyBlock,
    ApiConfig, ApiGroupConfig, ApiMethod, Async, Authored, BearerSubject, Bm25, Bucket, Carried,
    ChangeFeed, Config, Database, DefaultConfig, Distance, DocComment, Expr, Family, Field,
    FieldDefault, FieldPath, FieldPathSeg, Filter, Float, FnKind, FnPath, Function, Graphql,
    GraphqlConfig, Ident, Index, IndexKind, Introspection, JwtIssuer, JwtSource, JwtVerify, Kind,
    Language, Level, Middleware, Namespace, OnDelete, OpPermissions, Param, ParamName, Permission,
    PermissionGroup, PermissionOp, SdlFile, Sequence, Stmt, Table, TableKind, Tokenizer, User,
    VectorType,
};
use surform::diag::Diag;
use surform::info::read::{
    InfoObject, InfoValue, Markers, RawRoute, ReadEntry, read_accesses, read_analyzers, read_apis,
    read_buckets, read_configs, read_databases, read_default_config, read_events, read_fields,
    read_functions, read_indexes, read_info_object, read_namespaces, read_params, read_sequences,
    read_tables, read_users,
};
use surform::info::write::write_canonical;
use surrealdb::Surreal;
use surrealdb::engine::local::Db;

/// The fmt fixed point, over the written SDL of read
/// entries: canonical SDL re-parses clean, holds the same definition,
/// and formats to itself byte for byte. (Owned copy — see above.)
fn assert_fmt_fixed_point(sdl: &str) {
    let (ast, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{sdl}: {diags:?}");
    let once = write_canonical(&ast);
    let (reparsed, diags) = surform::parser::parse_file(&once);
    assert!(diags.is_empty(), "fmt output stopped parsing:\n{once}");
    assert!(
        ast.same_definition(&reparsed),
        "fmt changed the definition:\n--- SDL ---\n{sdl}\n--- fmt ---\n{once}"
    );
    let twice = write_canonical(&reparsed);
    assert_eq!(twice, once, "fmt is not idempotent:\n{once}");
}

/// One reverse-leg case: the SDL statement, the expectation the
/// engine's materializations pin, and the markers that read must
/// carry (the secret sentinels are facts of the round trip — a
/// user's `[REDACTED]` hash never reads as a literal).
struct Combo {
    name: String,
    sdl: String,
    expected: Stmt,
    markers: Markers,
}

// -- the embedded engine (Layer 4) rides tests/common --

/// The live-captured golden for one section: `name: DDL` lines, in
/// the map's order.
fn section_ddl(info: &InfoObject, section: &str) -> String {
    let Some(InfoValue::Object(map)) = info.0.get(section) else {
        return String::new();
    };
    let mut out = String::new();
    for (name, value) in &map.0 {
        if let InfoValue::Ddl(ddl) = value {
            out.push_str(name);
            out.push_str(": ");
            out.push_str(ddl);
            out.push('\n');
        }
    }
    out
}

/// The canonical SDL written from the read entries.
fn written(entries: &[ReadEntry]) -> String {
    write_canonical(&SdlFile {
        items: entries.iter().map(|e| e.stmt.clone()).collect(),
        trailing_comments: Vec::new(),
    })
}

/// The reverse fixed-point leg: exec every combo, capture the INFO
/// section, read it back, and hold each result `same_definition`
/// against the combo's materialized expectation. `extras` names the
/// definitions the engine auto-seeds in the scope (the session's own
/// ns/db) — they read like anything else but have no combo. Golden-
/// snaps the section's live DDL text and the written SDL.
async fn reverse_leg(
    db: &Surreal<Db>,
    combos: &[Combo],
    info_query: &str,
    section: &str,
    read: fn(&InfoObject) -> Vec<Result<ReadEntry, Diag>>,
    snapshot_tag: &str,
    extras: &[&str],
) {
    for combo in combos {
        exec::exec_sdl(db, &combo.sdl).await;
    }
    let text = exec::info_for(db, info_query).await;
    let info = read_info_object(&text).expect("real INFO must walk");
    insta::assert_snapshot!(format!("{snapshot_tag}_ddl"), section_ddl(&info, section));
    let entries = read(&info);
    assert_eq!(
        entries.len(),
        combos.len() + extras.len(),
        "every definition must read back"
    );
    let mut read_out = Vec::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|diag| panic!("entry must read: {diag:?}"));
        read_out.push(entry.clone());
        let Some(combo) = combos.iter().find(|c| c.name == entry.name) else {
            assert!(
                extras.contains(&entry.name.as_str()),
                "read name `{}` is neither a combo nor a known auto-seed",
                entry.name
            );
            continue;
        };
        assert!(
            SdlFile {
                items: vec![entry.stmt.clone()],
                trailing_comments: Vec::new(),
            }
            .same_definition(&SdlFile {
                items: vec![combo.expected.clone()],
                trailing_comments: Vec::new(),
            }),
            "reverse round trip changed `{}`:\nexpected: {:?}\nread: {:?}",
            combo.name,
            combo.expected,
            entry.stmt,
        );
        assert_eq!(
            entry.markers, combo.markers,
            "the read markers must match the materialization facts"
        );
    }
    insta::assert_snapshot!(format!("{snapshot_tag}_sdl"), written(&read_out));
    // The written canonical SDL of the read entries rides the fmt
    // fixed point: it re-parses and formats to itself.
    assert_fmt_fixed_point(&written(&read_out));
}

// ---------------------------------------------------------------------
// namespace — the ROOT tier
// ---------------------------------------------------------------------

fn namespace_combos() -> Vec<Combo> {
    let mut combos = Vec::new();
    for (name, comment) in [
        ("acme", None),
        ("app", Some("Acme Corporation")),
        ("`table`", None),
        ("staging", Some("two words")),
    ] {
        let mut sdl = String::new();
        if let Some(line) = comment {
            sdl.push_str(&format!("/// {line}\n"));
        }
        sdl.push_str(&format!("namespace {name};"));
        combos.push(Combo {
            name: name.trim_matches('`').to_string(),
            sdl,
            markers: Markers::default(),
            expected: Stmt::Namespace(Namespace {
                name: Ident::new(name.trim_matches('`')),
                keyword: surform::ast::Carried::default(),
                comment: comment.map(|line| DocComment {
                    lines: vec![line.to_string()],
                }),
                comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
        });
    }
    combos
}

#[tokio::test]
async fn namespaces_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u17").use_db("scratch").await.ok();
    reverse_leg(
        &db,
        &namespace_combos(),
        "INFO FOR ROOT",
        "namespaces",
        read_namespaces,
        "namespaces",
        &["sdl_u17"],
    )
    .await;
}

/// The auto-seed fingerprint (spec/namespace.md's reverse section),
/// through the live engine: a namespace whose comment IS the
/// fingerprint reads back with the marker set — this is how the
/// differ will report a genuinely auto-seeded `main` expected-
/// unmanaged. (A real server's `main` carries exactly this comment;
/// the embedded engine never auto-seeds one, so the fingerprint is
/// exercised on a defined namespace instead.)
#[tokio::test]
async fn fingerprint_marks_a_defined_namespace() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u17f").use_db("scratch").await.ok();
    exec::exec_sdl(
        &db,
        "/// Default namespace generated by SurrealDB\nnamespace marked;",
    )
    .await;
    let text = exec::info_for(&db, "INFO FOR ROOT").await;
    let info = read_info_object(&text).expect("real INFO must walk");
    insta::assert_snapshot!("fingerprint_ddl", section_ddl(&info, "namespaces"));
    let entries = read_namespaces(&info);
    let entry = entries
        .into_iter()
        .find(|e| e.as_ref().map(|e| e.name == "marked").unwrap_or(false))
        .expect("the namespace must read")
        .expect("it must read cleanly");
    let Stmt::Namespace(ns) = entry.stmt else {
        panic!("must be a namespace");
    };
    assert_eq!(
        ns.comment,
        Some(DocComment {
            lines: vec!["Default namespace generated by SurrealDB".to_string()]
        })
    );
    assert!(entry.markers.auto_seeded_default_namespace);
}

// ---------------------------------------------------------------------
// database — the NS tier
// ---------------------------------------------------------------------

fn database_combos() -> Vec<Combo> {
    let mut combos = Vec::new();
    let mut i = 0;
    let changefeeds: [Option<(&str, bool)>; 3] = [None, Some(("7d", false)), Some(("1h30m", true))];
    for strict in [false, true] {
        for changefeed in changefeeds {
            for comment in [None, Some("Primary application database")] {
                i += 1;
                let name = format!("db_{i}");
                let mut sdl = String::new();
                if let Some(line) = comment {
                    sdl.push_str(&format!("/// {line}\n"));
                }
                sdl.push_str(&format!("database {name} {{"));
                if strict {
                    sdl.push_str(" strict;");
                }
                if let Some((expiry, include)) = changefeed {
                    sdl.push_str(&format!(" changefeed {{ expiry: {expiry};"));
                    if include {
                        sdl.push_str(" include original;");
                    }
                    sdl.push_str(" };");
                }
                sdl.push_str(" };");
                // STRICT is active but never reported — the read
                // result carries none of it (preserve is the
                // normalizer's verb).
                combos.push(Combo {
                    name: name.clone(),
                    sdl,
                    markers: Markers::default(),
                    expected: Stmt::Database(Database {
                        name: Ident::new(name),
                        keyword: surform::ast::Carried::default(),
                        strict: false,
                        changefeed: changefeed.map(|(text, include)| ChangeFeed {
                            expiry: fixture_duration(text),
                            include_original: include,
                        }),
                        comment: comment.map(|line| DocComment {
                            lines: vec![line.to_string()],
                        }),
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                });
            }
        }
    }
    combos
}

/// Parses a duration literal through SDL's own parser — the same
/// value grammar the reader uses, so `7d` and `1w` hold one value.
fn fixture_duration(text: &str) -> surrealdb_types::Duration {
    let (file, diags) = surform::parser::parse_file(&format!(
        "database d {{ changefeed {{ expiry: {text}; }}; }}"
    ));
    assert!(diags.is_empty(), "{diags:?}");
    match &file.items[0] {
        Stmt::Database(db) => {
            db.changefeed
                .as_ref()
                .expect("the fixture has an expiry")
                .expiry
        }
        other => unreachable!("unexpected statement {other:?}"),
    }
}

#[tokio::test]
async fn databases_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u17db").use_db("scratch").await.ok();
    reverse_leg(
        &db,
        &database_combos(),
        "INFO FOR NS",
        "databases",
        read_databases,
        "databases",
        &["scratch"],
    )
    .await;
}

// ---------------------------------------------------------------------
// sequence — the DB tier
// ---------------------------------------------------------------------

fn sequence_combos() -> Vec<Combo> {
    let mut combos = Vec::new();
    let mut i = 0;
    for start in [None, Some("0"), Some("5000")] {
        for batch in [None, Some("1000"), Some("250")] {
            for timeout in [None, Some("5s")] {
                i += 1;
                let name = format!("seq_{i}");
                let mut sdl = format!("sequence {name} {{");
                if let Some(batch) = batch {
                    sdl.push_str(&format!(" batch: {batch};"));
                }
                if let Some(start) = start {
                    sdl.push_str(&format!(" start: {start};"));
                }
                if let Some(timeout) = timeout {
                    sdl.push_str(&format!(" timeout: {timeout};"));
                }
                sdl.push_str(" };");
                // BATCH/START always materialize (the defaults ride
                // the read result); TIMEOUT NONE stores as absent.
                combos.push(Combo {
                    name: name.clone(),
                    sdl,
                    markers: Markers::default(),
                    expected: Stmt::Sequence(Sequence {
                        name: Ident::new(name),
                        start: Some(Expr {
                            canon: start.unwrap_or("0").to_string(),
                        }),
                        batch: Some(Expr {
                            canon: batch.unwrap_or("1000").to_string(),
                        }),
                        timeout: timeout.map(|t| Expr {
                            canon: t.to_string(),
                        }),
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                });
            }
        }
    }
    combos
}

#[tokio::test]
async fn sequences_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u17seq").use_db("scratch").await.ok();
    reverse_leg(
        &db,
        &sequence_combos(),
        "INFO FOR DB",
        "sequences",
        read_sequences,
        "sequences",
        &[],
    )
    .await;
}

// ---------------------------------------------------------------------
// param — the DB tier
// ---------------------------------------------------------------------

fn param_combos() -> Vec<Combo> {
    let mut combos = Vec::new();
    let mut i = 0;
    for value in [
        None,
        Some("50"),
        Some("'text'"),
        Some("{ a: 1 }"),
        Some("true"),
    ] {
        for permission in [
            None,
            Some("none"),
            Some("full"),
            Some("$user.role == 'admin'"),
        ] {
            i += 1;
            let name = format!("param_{i}");
            let mut sdl = format!("param ${name} {{");
            if let Some(value) = value {
                sdl.push_str(&format!(" value: {value};"));
            }
            if let Some(permission) = permission {
                sdl.push_str(&format!(" permissions: {permission};"));
            }
            sdl.push_str(" };");
            // PERMISSIONS always materializes (FULL for the absent
            // clause); the value is the define-time result — here
            // the literals themselves.
            combos.push(Combo {
                name: name.clone(),
                sdl,
                markers: Markers::default(),
                expected: Stmt::Param(Param {
                    name: ParamName::new(name),
                    value: value.map(|v| Expr {
                        canon: v.to_string(),
                    }),
                    permissions: Some(match permission {
                        Some("none") => Permission::None,
                        Some("full") | None => Permission::Full,
                        Some(expr) => Permission::Expr(Expr {
                            canon: expr.to_string(),
                        }),
                    }),
                    comment: None,
                    comments: Vec::new(),
                    trailing_comments: Vec::new(),
                    clause_comments: Vec::new(),
                }),
            });
        }
    }
    combos
}

/// The define-time evaluation fact (spec/param.md): an SDL
/// expression VALUE stores its RESULT — `1 + 1` reads back as `2`.
#[tokio::test]
async fn param_value_reads_the_evaluated_result() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u17param").use_db("scratch").await.ok();
    exec::exec_sdl(&db, "param $seed { value: 1 + 1; };").await;
    let text = exec::info_for(&db, "INFO FOR DB").await;
    let info = read_info_object(&text).expect("real INFO must walk");
    insta::assert_snapshot!("param_eval_ddl", section_ddl(&info, "params"));
    let entries = read_params(&info);
    let entry = entries
        .into_iter()
        .find(|e| e.as_ref().map(|e| e.name == "seed").unwrap_or(false))
        .expect("the param must read")
        .expect("it must read cleanly");
    let Stmt::Param(param) = entry.stmt else {
        panic!("must be a param");
    };
    assert_eq!(
        param.value,
        Some(Expr {
            canon: "2".to_string()
        }),
        "the stored value is the define-time result"
    );
}

#[tokio::test]
async fn params_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u17params").use_db("scratch").await.ok();
    reverse_leg(
        &db,
        &param_combos(),
        "INFO FOR DB",
        "params",
        read_params,
        "params",
        &[],
    )
    .await;
}

// ---------------------------------------------------------------------
// bucket — the DB tier
// ---------------------------------------------------------------------

fn bucket_combos() -> Vec<Combo> {
    let mut combos = Vec::new();
    let mut i = 0;
    // One exec-able backend terminal: the engine validates the
    // backend against its own allowlist at define time (the backend
    // literal's passthrough is pinned by the unit tests instead).
    for backend in ["'memory'"] {
        for readonly in [false, true] {
            for permission in [None, Some("none"), Some("full"), Some("$auth.admin = true")] {
                i += 1;
                let name = format!("bucket_{i}");
                let mut sdl = format!("bucket {name} {{");
                if readonly {
                    sdl.push_str(" readonly;");
                }
                sdl.push_str(&format!(" backend: {backend};"));
                if let Some(permission) = permission {
                    sdl.push_str(&format!(" permissions: {permission};"));
                }
                sdl.push_str(" };");
                // PERMISSIONS always materializes (FULL for the
                // absent clause).
                combos.push(Combo {
                    name: name.clone(),
                    sdl,
                    markers: Markers::default(),
                    expected: Stmt::Bucket(Bucket {
                        name: Ident::new(name),
                        backend: Some(Expr {
                            canon: backend.to_string(),
                        }),
                        readonly,
                        permissions: Some(match permission {
                            Some("none") => Permission::None,
                            Some("full") | None => Permission::Full,
                            Some(expr) => Permission::Expr(Expr {
                                canon: expr.to_string(),
                            }),
                        }),
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                });
            }
        }
    }
    combos
}

#[tokio::test]
async fn buckets_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u17bucket").use_db("scratch").await.ok();
    reverse_leg(
        &db,
        &bucket_combos(),
        "INFO FOR DB",
        "buckets",
        read_buckets,
        "buckets",
        &[],
    )
    .await;
}

// ---------------------------------------------------------------------
// user — the medium tier
// ---------------------------------------------------------------------

fn user_combos() -> Vec<Combo> {
    let mut combos = Vec::new();
    let mut i = 0;
    // The pass clause drives the round trip's secret facts: a bare
    // user stores `PASSHASH ''` (the secretless sentinel), any set
    // secret reads back `[REDACTED]` — both keep `pass` at `None` and
    // feed their markers (spec/user.md: secrets never round-trip).
    for pass in [None, Some("password"), Some("passhash")] {
        // ROLES VIEWER and both durations always materialize.
        for roles in [None, Some("viewer"), Some("owner, editor")] {
            for durations in [None, Some(("1h", "none")), Some(("2h", "30m"))] {
                i += 1;
                let name = format!("user_{i}");
                let mut sdl = format!("user {name} {{ on: db;");
                match pass {
                    None => {}
                    Some("password") => sdl.push_str(" password: 'hunter2hunter2';"),
                    Some("passhash") => sdl.push_str(" passhash: 'argon2hash';"),
                    Some(_) => unreachable!(),
                }
                if let Some(roles) = roles {
                    sdl.push_str(&format!(" roles: {roles};"));
                }
                if let Some((token, session)) = durations {
                    sdl.push_str(&format!(
                        " duration {{ token: {token}; session: {session}; }};"
                    ));
                }
                sdl.push_str(" };");
                // INFO always reports PASSHASH — the sentinels feed
                // the markers, the pass clause reads as absent.
                let markers = match pass {
                    None => Markers {
                        secretless_passhash: true,
                        ..Markers::default()
                    },
                    Some(_) => Markers {
                        redacted: true,
                        ..Markers::default()
                    },
                };
                combos.push(Combo {
                    name: name.clone(),
                    sdl,
                    markers,
                    expected: Stmt::User(User {
                        name: Ident::new(name),
                        on: Authored::canonical(Level::Database),
                        pass: None,
                        scram: None,
                        roles: match roles {
                            // ROLES VIEWER always materializes — the
                            // absent clause reads as the default role.
                            None | Some("viewer") => vec![surform::ast::Role::Viewer],
                            Some(_) => vec![surform::ast::Role::Owner, surform::ast::Role::Editor],
                        },
                        token_duration: durations
                            .map(|(token, _)| Expr {
                                canon: token.to_string(),
                            })
                            .or(Some(Expr {
                                canon: "1h".to_string(),
                            })),
                        session_duration: Some(match durations {
                            // `session: none;` canonizes to the
                            // printer's NONE — the same spelling INFO
                            // carries.
                            Some((_, "none")) | None => Expr {
                                canon: "NONE".to_string(),
                            },
                            Some((_, session)) => Expr {
                                canon: session.to_string(),
                            },
                        }),
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                });
            }
        }
    }
    // A keyword-shaped name and a doc comment ride the header escape
    // and the COMMENT clause.
    let sdl = "/// db worker\nuser `table` { on: db; passhash: 'h'; roles: editor; duration \
               { token: 1h; session: none; }; };";
    combos.push(Combo {
        name: "table".to_string(),
        sdl: sdl.to_string(),
        markers: Markers {
            redacted: true,
            ..Markers::default()
        },
        expected: Stmt::User(User {
            name: Ident::new("table"),
            on: Authored::canonical(Level::Database),
            pass: None,
            scram: None,
            roles: vec![surform::ast::Role::Editor],
            token_duration: Some(Expr {
                canon: "1h".to_string(),
            }),
            session_duration: Some(Expr {
                canon: "NONE".to_string(),
            }),
            comment: Some(DocComment {
                lines: vec!["db worker".to_string()],
            }),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    combos
}

#[tokio::test]
async fn users_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u18user").use_db("scratch").await.ok();
    reverse_leg(
        &db,
        &user_combos(),
        "INFO FOR DB",
        "users",
        read_users,
        "users",
        &[],
    )
    .await;
}

/// `ON <LEVEL>` rides the DDL text at every tier (probe-pinned full
/// words) — the same `users` section name reads at ROOT and NS, and
/// the level word is what the read carries.
#[tokio::test]
async fn users_read_at_every_tier() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u18tier").use_db("scratch").await.ok();
    exec::exec_sdl(
        &db,
        "user root_worker { on: root; roles: owner; duration { token: 1h; session: none; }; };",
    )
    .await;
    exec::exec_sdl(
        &db,
        "user ns_worker { on: ns; roles: editor; duration { token: 1h; session: none; }; };",
    )
    .await;
    for (query, expected_name, expected_level) in [
        (
            "INFO FOR ROOT",
            "root_worker",
            Authored::canonical(Level::Root),
        ),
        (
            "INFO FOR NS",
            "ns_worker",
            Authored::canonical(Level::Namespace),
        ),
    ] {
        let text = exec::info_for(&db, query).await;
        let info = read_info_object(&text).expect("real INFO must walk");
        let entries = read_users(&info);
        let entry = entries
            .into_iter()
            .find(|e| e.as_ref().map(|e| e.name == expected_name).unwrap_or(false))
            .unwrap_or_else(|| panic!("{expected_name} must read"))
            .expect("it must read cleanly");
        let Stmt::User(user) = entry.stmt else {
            panic!("must be a user");
        };
        assert_eq!(user.on, expected_level);
    }
}

/// The 3.3 PASSSCRAM clause live: an SDL-authored verifier
/// execs; INFO presence-signals it as `PASSSCRAM '[REDACTED]'`; the
/// reader strips the clause before validation (the sentinel is
/// define-rejected) and the read user carries `scram: None` — the
/// secret stays SDL-side only. Both secret-shape markers pin per the
/// pass clause present.
#[tokio::test]
async fn users_passscram_strips_and_never_round_trips() {
    const KAT: &str = "SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=";
    let db = exec::mem_db().await;
    db.use_ns("sdl_u_g_user").use_db("scratch").await.ok();
    // Scram-only: INFO shows `PASSHASH ''` (empty, not redacted) plus
    // the scram sentinel — the read feeds secretless_passhash.
    exec::exec_sdl(
        &db,
        &format!("user scramonly {{ on: db; passscram: '{KAT}'; }};"),
    )
    .await;
    // passhash + scram: both secrets redacted — the read feeds
    // redacted. The coexistence itself is the additive grammar pin.
    exec::exec_sdl(
        &db,
        &format!("user hashy {{ on: db; passhash: '$argon2id$'; passscram: '{KAT}'; }};"),
    )
    .await;
    let text = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        text.matches("PASSSCRAM '[REDACTED]'").count() == 2,
        "both users presence-signal the verifier: {text}"
    );
    let info = read_info_object(&text).expect("real INFO must walk");
    let entries = read_users(&info);
    let mut by_name = std::collections::BTreeMap::new();
    for entry in entries {
        let entry = entry.expect("it must read cleanly");
        let Stmt::User(user) = entry.stmt else {
            panic!("must be a user");
        };
        by_name.insert(user.name.raw.clone(), (user, entry.markers));
    }
    let (scramonly, markers) = by_name.remove("scramonly").expect("scramonly reads");
    assert!(scramonly.pass.is_none() && scramonly.scram.is_none());
    assert!(markers.secretless_passhash && !markers.redacted);
    let (hashy, markers) = by_name.remove("hashy").expect("hashy reads");
    // The sentinel never adopts as a literal — the marker carries the
    // redaction fact, the AST slot stays empty (the medium-tier
    // protocol).
    assert!(hashy.pass.is_none() && hashy.scram.is_none());
    assert!(markers.redacted && !markers.secretless_passhash);
}

// ---------------------------------------------------------------------
// analyzer — the medium tier
// ---------------------------------------------------------------------

fn analyzer_combos() -> Vec<Combo> {
    let mut combos = Vec::new();
    let mut i = 0;
    // Nothing materializes for analyzers — the reverse mapping is 1:1
    // and the list order is preserved and significant.
    for function in [None, Some("fn::strip")] {
        for tokenizers in [None, Some("class, punct")] {
            for filters in [
                None,
                Some("lowercase, edgengram(2,10)"),
                Some("snowball(english), ascii"),
            ] {
                i += 1;
                let name = format!("an_{i}");
                let mut sdl = format!("analyzer {name} {{");
                if let Some(function) = function {
                    sdl.push_str(&format!(" function: {function};"));
                }
                if let Some(tokenizers) = tokenizers {
                    sdl.push_str(&format!(" tokenizers: {tokenizers};"));
                }
                if let Some(filters) = filters {
                    sdl.push_str(&format!(" filters: {filters};"));
                }
                sdl.push_str(" };");
                combos.push(Combo {
                    name: name.clone(),
                    sdl,
                    markers: Markers::default(),
                    expected: Stmt::Analyzer(Analyzer {
                        name: Ident::new(name),
                        function: function.map(|path| FnPath {
                            kind: FnKind::Fn,
                            segments: path
                                .strip_prefix("fn::")
                                .expect("the fixture path")
                                .split("::")
                                .map(str::to_string)
                                .collect(),
                        }),
                        tokenizers: tokenizers.map(|list| {
                            list.split(", ")
                                .map(|atom| match atom {
                                    "class" => Tokenizer::Class,
                                    "punct" => Tokenizer::Punct,
                                    other => unreachable!("unexpected tokenizer {other}"),
                                })
                                .collect()
                        }),
                        filters: filters.map(|list| {
                            list.split(", ")
                                .map(|atom| match atom {
                                    "lowercase" => Filter::Lowercase,
                                    "ascii" => Filter::Ascii,
                                    "edgengram(2,10)" => Filter::EdgeNgram(2, 10),
                                    // The alias canonicalizes to the
                                    // language value (value, never
                                    // spelling).
                                    "snowball(english)" => {
                                        Filter::Snowball(Authored::canonical(Language::English))
                                    }
                                    other => unreachable!("unexpected filter {other}"),
                                })
                                .collect()
                        }),
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                });
            }
        }
    }
    // The keyword-shaped name rides the header escape (SDL's own
    // keyword table backticks `english` at parse; the engine prints
    // it bare — the golden pins the engine's spelling); the
    // multi-tokenizer list rides comma-without-space.
    combos.push(Combo {
        name: "english".to_string(),
        sdl: "analyzer `english` { tokenizers: class; filters: snowball(english); };".to_string(),
        markers: Markers::default(),
        expected: Stmt::Analyzer(Analyzer {
            name: Ident::new("english"),
            function: None,
            tokenizers: Some(vec![Tokenizer::Class]),
            filters: Some(vec![Filter::Snowball(Authored::canonical(
                Language::English,
            ))]),
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    // The COMMENT clause.
    combos.push(Combo {
        name: "noted".to_string(),
        sdl: "/// English text search\nanalyzer noted { filters: lowercase; };".to_string(),
        markers: Markers::default(),
        expected: Stmt::Analyzer(Analyzer {
            name: Ident::new("noted"),
            function: None,
            tokenizers: None,
            filters: Some(vec![Filter::Lowercase]),
            comment: Some(DocComment {
                lines: vec!["English text search".to_string()],
            }),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    combos
}

#[tokio::test]
async fn analyzers_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u18an").use_db("scratch").await.ok();
    reverse_leg(
        &db,
        &analyzer_combos(),
        "INFO FOR DB",
        "analyzers",
        read_analyzers,
        "analyzers",
        &[],
    )
    .await;
}

// ---------------------------------------------------------------------
// event — the medium tier
// ---------------------------------------------------------------------

fn event_combos() -> Vec<Combo> {
    let mut combos = Vec::new();
    let mut i = 0;
    // WHEN true and ASYNC RETRY 1 MAXDEPTH 3 always materialize (the
    // defaults ride the read result); the THEN items are the
    // printer's own statement texts.
    for when in [None, Some("$event = 'CREATE'")] {
        for then in [
            "(CREATE log SET at = time::now())",
            "(CREATE l2 SET x = 1), (UPDATE stats SET n += 1)",
            // The printer's bare word-led shapes (the promoted half
            // of accepted-corner #22): value-shaped THEN items print
            // unparenthesized — CoverStmts covers only statement
            // forms.
            "fn::audit::write($event, $after.id ?? $before.id)",
            "THROW 'boom'",
            "RETURN $after.id",
            "BREAK",
            "CONTINUE",
            "time::now()",
            "x.y = 1",
            "fn::a(), THROW 'b'",
        ] {
            for r#async in [
                None,
                Some("async"),
                Some("async { retry: 5; maxdepth: 8; }"),
            ] {
                i += 1;
                let name = format!("ev_{i}");
                let mut sdl = format!("event {name} {{ on: ev_host;");
                if let Some(r#async) = r#async {
                    sdl.push_str(&format!(" {async};"));
                }
                if let Some(when) = when {
                    sdl.push_str(&format!(" when: {when};"));
                }
                sdl.push_str(&format!(" then: {then};"));
                sdl.push_str(" };");
                let then_items: Vec<Expr> = match then {
                    "(CREATE log SET at = time::now())" => vec![Expr {
                        canon: then.to_string(),
                    }],
                    "(CREATE l2 SET x = 1), (UPDATE stats SET n += 1)" => vec![
                        Expr {
                            canon: "(CREATE l2 SET x = 1)".to_string(),
                        },
                        Expr {
                            canon: "(UPDATE stats SET n += 1)".to_string(),
                        },
                    ],
                    // The word-led fixtures print exactly as authored
                    // (value-shaped CoverStmts arms re-print bare), so
                    // the read canon is the SDL text; the multi-item
                    // form splits at its depth-0 comma.
                    "fn::a(), THROW 'b'" => vec![
                        Expr {
                            canon: "fn::a()".to_string(),
                        },
                        Expr {
                            canon: "THROW 'b'".to_string(),
                        },
                    ],
                    other => vec![Expr {
                        canon: other.to_string(),
                    }],
                };
                combos.push(Combo {
                    name: name.clone(),
                    sdl,
                    markers: Markers::default(),
                    expected: Stmt::Event(surform::ast::Event {
                        name: Ident::new(name),
                        on: Ident::new("ev_host"),
                        when: Some(match when {
                            Some(expr) => Expr {
                                canon: expr.to_string(),
                            },
                            // The materialized default rides the
                            // read result.
                            None => Expr {
                                canon: "true".to_string(),
                            },
                        }),
                        then: then_items,
                        r#async: match r#async {
                            None => None,
                            // Bare async materializes RETRY 1
                            // MAXDEPTH 3.
                            Some("async") => Some(Async {
                                retry: Some(1),
                                maxdepth: Some(3),
                            }),
                            Some(_) => Some(Async {
                                retry: Some(5),
                                maxdepth: Some(8),
                            }),
                        },
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                });
            }
        }
    }
    // The braced THEN form and the COMMENT clause.
    combos.push(Combo {
        name: "braced".to_string(),
        sdl: "/// update log\nevent braced { on: ev_host; then: { CREATE b SET x = 1; }; };"
            .to_string(),
        markers: Markers::default(),
        expected: Stmt::Event(surform::ast::Event {
            name: Ident::new("braced"),
            on: Ident::new("ev_host"),
            when: Some(Expr {
                canon: "true".to_string(),
            }),
            then: vec![Expr {
                canon: "{ CREATE b SET x = 1 }".to_string(),
            }],
            r#async: None,
            comment: Some(DocComment {
                lines: vec!["update log".to_string()],
            }),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    combos
}

#[tokio::test]
async fn events_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u18ev").use_db("scratch").await.ok();
    exec::exec_sdl(&db, "table ev_host;").await;
    reverse_leg(
        &db,
        &event_combos(),
        "INFO FOR TABLE ev_host",
        "events",
        read_events,
        "events",
        &[],
    )
    .await;
}

// ---------------------------------------------------------------------
// function — the medium tier
// ---------------------------------------------------------------------

fn function_combos() -> Vec<Combo> {
    let mut combos = Vec::new();
    let mut i = 0;
    // PERMISSIONS FULL always materializes (the absent clause's
    // default rides the read result); the option kind arrives as the
    // printer's `none | int` normalization (same_definition compares
    // parsed kind ASTs).
    for params in [
        Vec::new(),
        vec![("$a", "int")],
        vec![("$a", "int"), ("$b", "array<float, 10>")],
        vec![("$x", "option<int>")],
    ] {
        for returns in [None, Some("bool")] {
            for body in ["{ RETURN 1; }", "{ LET $y = 1; RETURN $y; }", "{;}"] {
                i += 1;
                let name = format!("fn_{i}");
                let mut sdl = format!("fn {name}(");
                let param_sdl = params
                    .iter()
                    .map(|(param, kind)| format!("{param}: {kind}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                sdl.push_str(&param_sdl);
                sdl.push(')');
                if let Some(kind) = returns {
                    sdl.push_str(&format!(" -> {kind}"));
                }
                sdl.push(' ');
                sdl.push_str(body);
                sdl.push(';');
                let body_canon = match body {
                    // The single-statement body prints without its
                    // trailing `;`; multi-statement bodies keep one
                    // after every statement; empty canonizes to `{;}`.
                    "{ RETURN 1; }" => "{ RETURN 1 }".to_string(),
                    other => other.to_string(),
                };
                combos.push(Combo {
                    name: name.clone(),
                    sdl,
                    markers: Markers::default(),
                    expected: Stmt::Function(Function {
                        name: FnPath {
                            kind: FnKind::Fn,
                            segments: vec![name],
                        },
                        args: params
                            .iter()
                            .map(|(param, kind)| {
                                (
                                    ParamName::new(param.trim_start_matches('$')),
                                    Kind::new(*kind),
                                )
                            })
                            .collect(),
                        returns: returns.map(Kind::new),
                        body: Expr { canon: body_canon },
                        permissions: Some(Permission::Full),
                        graphql: None,
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                });
            }
        }
    }
    // The server's trailing order (body → COMMENT → GRAPHQL_* →
    // PERMISSIONS) reads the graphql options into the sub-block; the
    // WHERE-permissions expression rides the capture.
    combos.push(Combo {
        name: "full_shape".to_string(),
        sdl: "/// whether the user may edit\nfn full_shape($p: record<post>) -> bool { RETURN \
              $auth.id = $p.author } { permissions: $auth.admin = true; graphql { alias: 'edit'; \
              deprecated: 'use v2'; }; };"
            .to_string(),
        markers: Markers::default(),
        expected: Stmt::Function(Function {
            name: FnPath {
                kind: FnKind::Fn,
                segments: vec!["full_shape".to_string()],
            },
            args: vec![(ParamName::new("p"), Kind::new("record<post>"))],
            returns: Some(Kind::new("bool")),
            body: Expr {
                canon: "{ RETURN $auth.id = $p.author }".to_string(),
            },
            permissions: Some(Permission::Expr(Expr {
                canon: "$auth.admin = true".to_string(),
            })),
            graphql: Some(Graphql {
                alias: Some("edit".to_string()),
                deprecated: Some("use v2".to_string()),
            }),
            comment: Some(DocComment {
                lines: vec!["whether the user may edit".to_string()],
            }),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    // A multi-segment path: the INFO key is `d1::d2` (the path sans
    // `fn::`, quoted in the renderer's map — probe-pinned), riding
    // walk + read + same_definition end to end.
    combos.push(Combo {
        name: "d1::d2".to_string(),
        sdl: "fn d1::d2() { RETURN 1; };".to_string(),
        markers: Markers::default(),
        expected: Stmt::Function(Function {
            name: FnPath {
                kind: FnKind::Fn,
                segments: vec!["d1".to_string(), "d2".to_string()],
            },
            args: vec![],
            returns: None,
            body: Expr {
                canon: "{ RETURN 1 }".to_string(),
            },
            permissions: Some(Permission::Full),
            graphql: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    combos
}

#[tokio::test]
async fn functions_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u18fn").use_db("scratch").await.ok();
    reverse_leg(
        &db,
        &function_combos(),
        "INFO FOR DB",
        "functions",
        read_functions,
        "functions",
        &[],
    )
    .await;
}

// ---------------------------------------------------------------------
// config — the medium tier
// ---------------------------------------------------------------------

/// INFO FOR DB's `configs` map is keyed by VARIANT (`API`,
/// `GraphQL`) and holds one definition per variant (per-variant
/// singletons) — so the leg carries exactly one api and one graphql
/// combo, and the remaining shapes ride dedicated tests below.
#[tokio::test]
async fn configs_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u18cfg").use_db("scratch").await.ok();
    let combos = vec![
        Combo {
            name: "api".to_string(),
            sdl: "config api { middleware: api::timeout(5s), api::log(); permissions: none; };"
                .to_string(),
            markers: Markers::default(),
            expected: Stmt::Config(Config::Api(ApiConfig {
                middleware: vec![
                    Middleware {
                        path: FnPath {
                            kind: FnKind::Bare,
                            segments: vec!["api".to_string(), "timeout".to_string()],
                        },
                        args: vec![Expr {
                            canon: "5s".to_string(),
                        }],
                    },
                    Middleware {
                        path: FnPath {
                            kind: FnKind::Bare,
                            segments: vec!["api".to_string(), "log".to_string()],
                        },
                        args: vec![],
                    },
                ],
                permissions: Some(Permission::None),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })),
        },
        Combo {
            name: "graphql".to_string(),
            sdl: "config graphql { tables { include: t1, t2; }; functions: none; depth: 4; \
                  complexity: 100; introspection: none; };"
                .to_string(),
            markers: Markers::default(),
            expected: Stmt::Config(Config::Graphql(GraphqlConfig {
                tables: Family::Include(vec![Ident::new("t1"), Ident::new("t2")]),
                functions: Family::None,
                depth: Some(4),
                complexity: Some(100),
                introspection: Introspection::None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })),
        },
    ];
    reverse_leg(
        &db,
        &combos,
        "INFO FOR DB",
        "configs",
        read_configs,
        "configs",
        &[],
    )
    .await;
}

/// The singleton variants' remaining shapes: the minimal api fragment
/// (PERMISSIONS FULL materializes) and the `auto;` flag resolving to
/// the per-family spellings. Each variant holds ONE definition — the
/// graphql shapes ride separate scopes' assertions on the single
/// survivor.
#[tokio::test]
async fn config_shapes_read_back() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u18cfgs").use_db("scratch").await.ok();
    exec::exec_sdl(&db, "config api;").await;
    // The `auto;` flag writes both families — the fragment arrives as
    // the resolved spellings; INTROSPECTION elides (auto is default).
    exec::exec_sdl(&db, "config graphql { auto; };").await;
    let text = exec::info_for(&db, "INFO FOR DB").await;
    let info = read_info_object(&text).expect("real INFO must walk");
    let entries = read_configs(&info);
    assert_eq!(entries.len(), 2, "one api and one graphql survive");
    for entry in entries {
        let entry = entry.expect("it must read cleanly");
        assert_eq!(entry.markers, Markers::default());
        match entry.name.as_str() {
            "api" => {
                let Stmt::Config(Config::Api(api)) = entry.stmt else {
                    panic!("must be the api config");
                };
                assert!(api.middleware.is_empty());
                assert_eq!(api.permissions, Some(Permission::Full));
            }
            "graphql" => {
                let Stmt::Config(Config::Graphql(graphql)) = entry.stmt else {
                    panic!("must be the graphql config");
                };
                assert_eq!(graphql.tables, Family::Auto);
                assert_eq!(graphql.functions, Family::Auto);
                assert_eq!(graphql.introspection, Introspection::Auto);
            }
            other => panic!("unexpected config variant {other}"),
        }
    }
}

/// Unset families ride NONE (`tables: none; functions: none` is
/// representation-equal to absence in the AST) with introspection
/// elided.
#[tokio::test]
async fn config_none_families_read_back() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u18cfgn").use_db("scratch").await.ok();
    exec::exec_sdl(&db, "config graphql { tables: none; functions: none; };").await;
    let text = exec::info_for(&db, "INFO FOR DB").await;
    let info = read_info_object(&text).expect("real INFO must walk");
    let entries = read_configs(&info);
    let entry = entries.into_iter().next().expect("the graphql survives");
    let entry = entry.expect("it must read cleanly");
    let Stmt::Config(Config::Graphql(graphql)) = entry.stmt else {
        panic!("must be the graphql config");
    };
    assert_eq!(graphql.tables, Family::None);
    assert_eq!(graphql.functions, Family::None);
    assert_eq!(graphql.introspection, Introspection::Auto);
}

/// The `default` config's structured `defaults` value (INFO FOR
/// ROOT): the set sides only, `{}` reading as none — and the written
/// SDL rides the fmt fixed point.
#[tokio::test]
async fn default_config_reads_the_structured_value() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u18def").use_db("scratch").await.ok();
    // No default config defined yet.
    let text = exec::info_for(&db, "INFO FOR ROOT").await;
    let info = read_info_object(&text).expect("real INFO must walk");
    assert!(read_default_config(&info).expect("reads").is_none());
    // Both sides, then one side (the singleton overwrite).
    exec::exec_sdl(
        &db,
        "config default { namespace: 'acme'; database: 'blog'; };",
    )
    .await;
    exec::exec_sdl(&db, "config default { database: 'solo'; };").await;
    let text = exec::info_for(&db, "INFO FOR ROOT").await;
    let info = read_info_object(&text).expect("real INFO must walk");
    insta::assert_snapshot!("defaults_ddl", section_ddl(&info, "defaults"));
    let entry = read_default_config(&info)
        .expect("reads")
        .expect("the default config is set");
    assert_eq!(entry.name, "default");
    assert_eq!(entry.markers, Markers::default());
    let Stmt::Config(Config::Default(DefaultConfig {
        namespace,
        database,
        ..
    })) = &entry.stmt
    else {
        panic!("must be the default config");
    };
    assert_eq!(*namespace, None);
    assert_eq!(
        *database,
        Some(Expr {
            canon: "'solo'".to_string()
        })
    );
    // The written SDL rides the fmt fixed point.
    let sdl = write_canonical(&SdlFile {
        items: vec![entry.stmt],
        trailing_comments: Vec::new(),
    });
    insta::assert_snapshot!("defaults_sdl", sdl);
    assert_fmt_fixed_point(&sdl);
}

// ---------------------------------------------------------------------
// The tripwire class, through the public entry points
// ---------------------------------------------------------------------

#[tokio::test]
async fn shape_and_drift_diagnose_through_the_public_readers() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u17trip").use_db("scratch").await.ok();

    // Kind word vs section.
    let info = read_info_object("{ namespaces: { x: 'DEFINE TABLE x;' } }").expect("walks");
    let entries = read_namespaces(&info);
    assert!(matches!(entries[0], Err(Diag::InfoUnknownShape { .. })),);

    // Embedded-unparseable DDL (the decided disposition:
    // diagnose, never raw-route — no simple statement's reverse
    // section licenses an unparseable shape).
    let info = read_info_object("{ params: { x: 'DEFINE PARAM BOGUS' } }").expect("walks");
    let entries = read_params(&info);
    assert!(matches!(entries[0], Err(Diag::InfoUnparsable { .. })),);

    // Drifted trailing text still parses upstream (unknown trailing
    // tokens are a parse error, so this rides the same E3001 class).
    let info =
        read_info_object("{ params: { x: 'DEFINE PARAM $x VALUE NONE PERMISSIONS FULL JUNK' } }")
            .expect("walks");
    let entries = read_params(&info);
    assert!(matches!(entries[0], Err(Diag::InfoUnparsable { .. })),);
    // NOTE: for PRINTER-SHAPED input (what live INFO carries),
    // `DdlTripwire` is deliberately unreachable through the public
    // readers: the
    // canonicalize step re-prints everything into the pinned dialect
    // first, so the only runtime source of a splitter mismatch is
    // printer drift itself. HAND-FED canonical text that upstream
    // accepts can still tripwire (the default-config fragment's
    // grammar is not INFO-reachable in 3.2.4, and hand-fed values can
    // sit outside the pinned shapes) — those diagnose fail-closed.
    // The tripwire shapes are exercised directly on the splitters in
    // the unit tests.
}

// ---------------------------------------------------------------------
// The high tier: index, field, table (live goldens + the
// reverse fixed-point legs)
// ---------------------------------------------------------------------

/// A `.`-path from its dotted spelling (`matrix.*.*`), the reverse of
/// the reader's rejoin.
fn field_path(path: &str) -> FieldPath {
    FieldPath {
        segments: path
            .split('.')
            .map(|segment| {
                if segment == "*" {
                    FieldPathSeg::Wildcard
                } else {
                    FieldPathSeg::Name(Ident::new(segment))
                }
            })
            .collect(),
    }
}

/// One full-permission field clause: the engine prints PERMISSIONS
/// unconditionally (FULL is the default), so every expected field
/// carries it.
fn full_permissions() -> Option<OpPermissions> {
    Some(OpPermissions::Full)
}

fn index_combos() -> Vec<Combo> {
    vec![
        Combo {
            name: "by_thread".into(),
            sdl: "/// Composite lookup\nindex by_thread { on: doc; fields: account, thread_id; };".into(),
            expected: Stmt::Index(Index {
                name: Ident::new("by_thread"),
                kind: IndexKind::Plain,
                on: Ident::new("doc"),
                fields: vec![Expr { canon: "account".into() }, Expr { canon: "thread_id".into() }],
                fields_key: Carried::default(),
                concurrently: false,
                comment: Some(DocComment { lines: vec!["Composite lookup".into()] }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // `concurrently;` is transient — the read cannot see it (the
        // preserve list's, spec/index.md).
        Combo {
            name: "one_email".into(),
            sdl: "index unique one_email { on: doc; fields: email; concurrently; };".into(),
            expected: Stmt::Index(Index {
                name: Ident::new("one_email"),
                kind: IndexKind::Unique,
                on: Ident::new("doc"),
                fields: vec![Expr { canon: "email".into() }],
                fields_key: Carried::default(),
                concurrently: false,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "row_count".into(),
            sdl: "index count row_count { on: doc; };".into(),
            expected: Stmt::Index(Index {
                name: Ident::new("row_count"),
                kind: IndexKind::Count { where_: None },
                on: Ident::new("doc"),
                fields: vec![],
                fields_key: Carried::default(),
                concurrently: false,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "active_count".into(),
            sdl: "index count active_count { on: doc; where: status = 'active'; };".into(),
            expected: Stmt::Index(Index {
                name: Ident::new("active_count"),
                kind: IndexKind::Count {
                    where_: Some(Expr { canon: "status = 'active'".into() }),
                },
                on: Ident::new("doc"),
                fields: vec![],
                fields_key: Carried::default(),
                concurrently: false,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // A bare SDL FULLTEXT reads with the materialized defaults —
        // the faithful posture (elision is the normalizer's).
        Combo {
            name: "body_fts".into(),
            sdl: "index fulltext body_fts { on: doc; fields: body; };".into(),
            expected: Stmt::Index(Index {
                name: Ident::new("body_fts"),
                kind: IndexKind::FullText {
                    analyzer: Some(Ident::new("like")),
                    bm25: Some(Bm25(1.2, 0.75)),
                    highlights: false,
                },
                on: Ident::new("doc"),
                fields: vec![Expr { canon: "body".into() }],
                fields_key: Carried::default(),
                concurrently: false,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "title_fts".into(),
            sdl: "index fulltext title_fts { on: doc; fields: title; analyzer: english; bm25: (1.2, 0.9); highlights; };".into(),
            expected: Stmt::Index(Index {
                name: Ident::new("title_fts"),
                kind: IndexKind::FullText {
                    analyzer: Some(Ident::new("english")),
                    bm25: Some(Bm25(1.2, 0.9)),
                    highlights: true,
                },
                on: Ident::new("doc"),
                fields: vec![Expr { canon: "title".into() }],
                fields_key: Carried::default(),
                concurrently: false,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // HNSW minimal: every parameter materializes, M0/LM derived.
        Combo {
            name: "vec_min".into(),
            sdl: "index hnsw vec_min { on: doc; fields: vector; dimension: 4; };".into(),
            expected: Stmt::Index(Index {
                name: Ident::new("vec_min"),
                kind: IndexKind::Hnsw {
                    dimension: 4,
                    distance: Some(Distance::Euclidean),
                    vector_type: Some(VectorType::F32),
                    m: Some(12),
                    m0: Some(24),
                    lm: Some(Float(1.0 / (12.0f64).ln())),
                    efc: Some(150),
                    extend_candidates: false,
                    keep_pruned_connections: false,
                    hashed_vector: false,
                },
                on: Ident::new("doc"),
                fields: vec![Expr { canon: "vector".into() }],
                fields_key: Carried::default(),
                concurrently: false,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "vec_tuned".into(),
            sdl: "index hnsw vec_tuned { on: doc; fields: vector; dimension: 4; distance: minkowski(3); type: f16; m: 16; efc: 200; extend_candidates; keep_pruned_connections; };".into(),
            expected: Stmt::Index(Index {
                name: Ident::new("vec_tuned"),
                kind: IndexKind::Hnsw {
                    dimension: 4,
                    distance: Some(Distance::Minkowski(Float(3.0))),
                    vector_type: Some(VectorType::F16),
                    m: Some(16),
                    m0: Some(32),
                    lm: Some(Float(1.0 / (16.0f64).ln())),
                    efc: Some(200),
                    extend_candidates: true,
                    keep_pruned_connections: true,
                    hashed_vector: false,
                },
                on: Ident::new("doc"),
                fields: vec![Expr { canon: "vector".into() }],
                fields_key: Carried::default(),
                concurrently: false,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "big_vec".into(),
            sdl: "index diskann big_vec { on: doc; fields: embedding; dimension: 8; distance: cosine_normalized; type: f16; degree: 32; l_build: 50; alpha: 1.3; hashed_vector; };".into(),
            expected: Stmt::Index(Index {
                name: Ident::new("big_vec"),
                kind: IndexKind::DiskAnn {
                    dimension: 8,
                    distance: Some(Distance::CosineNormalized),
                    vector_type: Some(VectorType::F16),
                    degree: Some(32),
                    l_build: Some(50),
                    alpha: Some(Float(1.3)),
                    hashed_vector: true,
                },
                on: Ident::new("doc"),
                fields: vec![Expr { canon: "embedding".into() }],
                fields_key: Carried::default(),
                concurrently: false,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "disk_min".into(),
            sdl: "index diskann disk_min { on: doc; fields: embedding; dimension: 8; };".into(),
            expected: Stmt::Index(Index {
                name: Ident::new("disk_min"),
                kind: IndexKind::DiskAnn {
                    dimension: 8,
                    distance: Some(Distance::Euclidean),
                    vector_type: Some(VectorType::F32),
                    degree: Some(64),
                    l_build: Some(100),
                    alpha: Some(Float(1.2)),
                    hashed_vector: false,
                },
                on: Ident::new("doc"),
                fields: vec![Expr { canon: "embedding".into() }],
                fields_key: Carried::default(),
                concurrently: false,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
    ]
}

#[tokio::test]
async fn indexes_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u19i").use_db("scratch").await.ok();
    // The single index host and its fields (an index over a missing
    // field fails at exec on a SCHEMAFULL host; schemaless keeps the
    // probe unconstrained either way).
    exec::exec_sdl(
        &db,
        "table doc { schemaless; };
         field account: string { on: doc; };
         field thread_id: string { on: doc; };
         field email: string { on: doc; };
         field status: string { on: doc; };
         field body: string { on: doc; };
         field title: string { on: doc; };
         field vector: array<float> { on: doc; };
         field embedding: array<float> { on: doc; };",
    )
    .await;
    reverse_leg(
        &db,
        &index_combos(),
        "INFO FOR TABLE doc",
        "indexes",
        read_indexes,
        "indexes",
        &[],
    )
    .await;
}

fn field_combos() -> Vec<Combo> {
    vec![
        Combo {
            name: "contact".into(),
            sdl: "/// The contact address\nfield contact: string { on: account; assert: string::is_email($value); default: 'unknown@example.com'; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("contact"),
                kind: Some(Kind::new("string")),
                on: Ident::new("account"),
                default: Some(FieldDefault::Set(Expr { canon: "'unknown@example.com'".into() })),
                value: None,
                assert: Some(Expr { canon: "string::is_email($value)".into() }),
                computed: None,
                readonly: false,
                flexible: false,
                inline: false,
                reference: None,
                permissions: full_permissions(),
                graphql: None,
                comment: Some(DocComment { lines: vec!["The contact address".into()] }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "locked".into(),
            sdl: "field locked: bool { on: account; readonly; value: true; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("locked"),
                kind: Some(Kind::new("bool")),
                on: Ident::new("account"),
                default: None,
                value: Some(Expr { canon: "true".into() }),
                assert: None,
                computed: None,
                readonly: true,
                flexible: false,
                inline: false,
                reference: None,
                permissions: full_permissions(),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // Typed and typeless computed both read (the type is optional
        // exactly when COMPUTED rides).
        Combo {
            name: "shout".into(),
            sdl: "field shout: string { on: account; computed: string::uppercase(name); };".into(),
            expected: Stmt::Field(Field {
                name: field_path("shout"),
                kind: Some(Kind::new("string")),
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: Some(Expr { canon: "string::uppercase(name)".into() }),
                readonly: false,
                flexible: false,
                inline: false,
                reference: None,
                permissions: full_permissions(),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "derived".into(),
            sdl: "field derived { on: account; computed: string::uppercase(name); };".into(),
            expected: Stmt::Field(Field {
                name: field_path("derived"),
                kind: None,
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: Some(Expr { canon: "string::uppercase(name)".into() }),
                readonly: false,
                flexible: false,
                inline: false,
                reference: None,
                permissions: full_permissions(),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "da".into(),
            sdl: "field da: array<string> { on: account; default always: []; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("da"),
                kind: Some(Kind::new("array<string>")),
                on: Ident::new("account"),
                default: Some(FieldDefault::Always(Expr { canon: "[]".into() })),
                value: None,
                assert: None,
                computed: None,
                readonly: false,
                flexible: false,
                inline: false,
                reference: None,
                permissions: full_permissions(),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "meta2".into(),
            sdl: "field meta2: object { on: account; flexible; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("meta2"),
                kind: Some(Kind::new("object")),
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: None,
                readonly: false,
                flexible: true,
                inline: false,
                reference: None,
                permissions: full_permissions(),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // REFERENCE: the printer always spells the action; IGNORE is
        // the bare flag's spelling (live: owner).
        Combo {
            name: "author".into(),
            sdl: "field author: record<user> { on: account; reference { on delete: cascade; }; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("author"),
                kind: Some(Kind::new("record<user>")),
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: None,
                readonly: false,
                flexible: false,
                inline: false,
                reference: Some(OnDelete::Cascade),
                permissions: full_permissions(),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "owner".into(),
            sdl: "field owner: record<user> { on: account; reference; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("owner"),
                kind: Some(Kind::new("record<user>")),
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: None,
                readonly: false,
                flexible: false,
                inline: false,
                reference: Some(OnDelete::Ignore),
                permissions: full_permissions(),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "parent".into(),
            sdl: "field parent: record<page> { on: account; reference { on delete: (create cleanup_log set at = time::now()); }; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("parent"),
                kind: Some(Kind::new("record<page>")),
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: None,
                readonly: false,
                flexible: false,
                inline: false,
                reference: Some(OnDelete::Then(Expr {
                    canon: "(CREATE cleanup_log SET at = time::now())".into(),
                })),
                permissions: full_permissions(),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // The all-NONE shorthand prints as the merged group (the
        // printer's grouping, live-probed on `secret`).
        Combo {
            name: "secret".into(),
            sdl: "field secret: string { on: account; permissions: none; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("secret"),
                kind: Some(Kind::new("string")),
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: None,
                readonly: false,
                flexible: false,
                inline: false,
                reference: None,
                permissions: Some(OpPermissions::Groups(vec![PermissionGroup {
                    ops: vec![
                        PermissionOp::Select,
                        PermissionOp::Create,
                        PermissionOp::Update,
                    ],
                    value: Permission::None,
                }])),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "audited".into(),
            sdl: "field audited: string { on: account; permissions { select: none; create, update: $auth.admin = true; }; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("audited"),
                kind: Some(Kind::new("string")),
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: None,
                readonly: false,
                flexible: false,
                inline: false,
                reference: None,
                permissions: Some(OpPermissions::Groups(vec![
                    PermissionGroup {
                        ops: vec![PermissionOp::Select],
                        value: Permission::None,
                    },
                    PermissionGroup {
                        ops: vec![PermissionOp::Create, PermissionOp::Update],
                        value: Permission::Expr(Expr { canon: "$auth.admin = true".into() }),
                    },
                ])),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "display".into(),
            sdl: "field display: string { on: account; graphql { alias: 'displayName'; }; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("display"),
                kind: Some(Kind::new("string")),
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: None,
                readonly: false,
                flexible: false,
                inline: false,
                reference: None,
                permissions: full_permissions(),
                graphql: Some(Graphql {
                    alias: Some("displayName".into()),
                    deprecated: None,
                }),
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // The option desugar rides as printed (rule 4: kinds compare
        // by parsed AST; same_definition equates).
        Combo {
            name: "optf".into(),
            sdl: "field optf: option<string> { on: account; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("optf"),
                kind: Some(Kind::new("none | string")),
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: None,
                readonly: false,
                flexible: false,
                inline: false,
                reference: None,
                permissions: full_permissions(),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // The printer's fmt arm order: DEFAULT before VALUE before
        // ASSERT (the DEFAULT capture once swallowed the VALUE clause).
        Combo {
            name: "fvd".into(),
            sdl: "field fvd: string { on: account; default: 2; value: 1; assert: 3 > 2; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("fvd"),
                kind: Some(Kind::new("string")),
                on: Ident::new("account"),
                default: Some(FieldDefault::Set(Expr { canon: "2".into() })),
                value: Some(Expr { canon: "1".into() }),
                assert: Some(Expr { canon: "3 > 2".into() }),
                computed: None,
                readonly: false,
                flexible: false,
                inline: false,
                reference: None,
                permissions: full_permissions(),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // Group PERMISSIONS after a captured COMPUTED expression (the
        // Perm follow must confirm the FOR form).
        Combo {
            name: "fcp".into(),
            sdl: "field fcp: string { on: account; computed: 1 + 1; permissions { select: meta = 1; create, update: full; }; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("fcp"),
                kind: Some(Kind::new("string")),
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: Some(Expr { canon: "1 + 1".into() }),
                readonly: false,
                flexible: false,
                inline: false,
                reference: None,
                permissions: Some(OpPermissions::Groups(vec![
                    PermissionGroup {
                        ops: vec![PermissionOp::Select],
                        value: Permission::Expr(Expr { canon: "meta = 1".into() }),
                    },
                    PermissionGroup {
                        ops: vec![PermissionOp::Create, PermissionOp::Update],
                        value: Permission::Full,
                    },
                ])),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // The id field reads like any field (the engine prints it
        // with TYPE and PERMISSIONS FULL).
        Combo {
            name: "id".into(),
            sdl: "field id: string { on: account; };".into(),
            expected: Stmt::Field(Field {
                name: field_path("id"),
                kind: Some(Kind::new("string")),
                on: Ident::new("account"),
                default: None,
                value: None,
                assert: None,
                computed: None,
                readonly: false,
                flexible: false,
                inline: false,
                reference: None,
                permissions: full_permissions(),
                graphql: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
    ]
}

#[tokio::test]
async fn fields_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u19f").use_db("scratch").await.ok();
    // The synthesis parents (matrix / flx / un) sit on a separate
    // table: their synthesized `.*` subfields land in the account
    // table's fields section only when authored there — this leg
    // keeps the section exactly combos + the known extras.
    exec::exec_sdl(
        &db,
        "table account { schemafull; };
         field name: string { on: account; };
         table synth { schemafull; };
         field matrix: array<array<int>> { on: synth; };
         field flx: array<object> { on: synth; flexible; };
         field un: array<int> | array<string> { on: synth; };",
    )
    .await;
    // The engine synthesizes the inner-kind subfields (the
    // field-probe contract): `matrix.*`/`matrix.*.*`, inherited-flexible `flx.*`,
    // and the union's union-of-arms `un.*` — read as the ordinary
    // fields they print as (the deepest-first fold is the
    // normalizer's).
    let text = exec::info_for(&db, "INFO FOR TABLE synth").await;
    assert!(
        text.contains(
            r#""matrix.*": 'DEFINE FIELD matrix.* ON synth TYPE array<int> PERMISSIONS FULL'"#
        ),
        "{text}"
    );
    assert!(
        text.contains(
            r#""matrix.*.*": 'DEFINE FIELD matrix.*.* ON synth TYPE int PERMISSIONS FULL'"#
        ),
        "{text}"
    );
    assert!(
        text.contains(
            r#""flx.*": 'DEFINE FIELD flx.* ON synth TYPE object FLEXIBLE PERMISSIONS FULL'"#
        ),
        "{text}"
    );
    assert!(
        text.contains(r#""un.*": 'DEFINE FIELD un.* ON synth TYPE int | string PERMISSIONS FULL'"#),
        "{text}"
    );
    // The `name` field the assertions above lean on, read like any
    // field (the account table's auto-asserted companion).
    reverse_leg(
        &db,
        &field_combos(),
        "INFO FOR TABLE account",
        "fields",
        read_fields,
        "fields",
        // The assertion companion `name`, and the engine's synthesized
        // `.*` subfield of the array-typed `da` — read as the ordinary
        // fields they print as (the fold is the normalizer's).
        &["name", "da.*"],
    )
    .await;
}

/// The 3.3 INLINE field: the flag rides the live INFO print
/// between READONLY and VALUE, reads back into the AST, and the
/// authored definition round-trips (`same_definition`). The
/// engine's auto `in`/`out` fields read as the ordinary fields they
/// print as — a non-lightweight relation keeps them (the drop
/// is lightweight-only).
#[tokio::test]
async fn inline_field_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_inline").use_db("scratch").await.ok();
    exec::exec_sdl(
        &db,
        "table person -> likes -> person { permissions: full; };\n\
         field score: number { on: likes; readonly; inline; default: 1; };",
    )
    .await;
    let text = exec::info_for(&db, "INFO FOR TABLE likes").await;
    assert!(
        text.contains("TYPE number DEFAULT 1 READONLY INLINE"),
        "the printer's fixed INLINE position: {text}"
    );
    let info = read_info_object(&text).expect("real INFO must walk");
    let entries = read_fields(&info);
    assert_eq!(entries.len(), 3, "score plus the auto in/out: {entries:?}");
    let (file, diags) = surform::parser::parse_file(
        "table person -> likes -> person { permissions: full; };\n\
         field score: number { on: likes; readonly; inline; default: 1; };",
    );
    assert!(diags.is_empty());
    let Stmt::Field(expected) = &file.items[1] else {
        panic!()
    };
    // Faithful reads carry the printer's materializations: PERMISSIONS
    // FULL always prints (elision is the normalizer's), so the
    // expectation is hand-built to the read shape (the leg contract).
    let mut expected = expected.clone();
    expected.permissions = full_permissions();
    let mut score = None;
    let mut extras = 0;
    for entry in entries {
        let entry = entry.unwrap_or_else(|diag| panic!("entry must read: {diag:?}"));
        if entry.name == "score" {
            score = Some(entry);
        } else {
            extras += 1;
        }
    }
    assert_eq!(extras, 2, "the auto in/out pair");
    let score = score.expect("the inline field reads back");
    let Stmt::Field(field) = &score.stmt else {
        panic!()
    };
    assert!(field.inline && field.readonly);
    assert!(
        SdlFile {
            items: vec![score.stmt.clone()],
            trailing_comments: Vec::new(),
        }
        .same_definition(&SdlFile {
            items: vec![Stmt::Field(expected.clone())],
            trailing_comments: Vec::new(),
        }),
        "the inline field must round-trip"
    );
    assert_fmt_fixed_point(&written(std::slice::from_ref(&score)));
}

fn table_combos() -> Vec<Combo> {
    vec![
        Combo {
            name: "users".into(),
            sdl: "/// App users\ntable users { schemafull; permissions { select: full; create: full; update: $auth.id = id; delete: none; }; };".into(),
            expected: Stmt::Table(Table {
                name: Ident::new("users"),
                kind: TableKind::Normal,
                schema_full: Some(true),
                drop: false,
                inline_edges: None,
                inline_references: None,
                changefeed: None,
                permissions: Some(OpPermissions::Groups(vec![
                    // The printer merges equal-value groups (the
                    // recorded divergence — SDL's emission keeps the
                    // written groups, the read carries the merge).
                    PermissionGroup {
                        ops: vec![PermissionOp::Select, PermissionOp::Create],
                        value: Permission::Full,
                    },
                    PermissionGroup {
                        ops: vec![PermissionOp::Update],
                        value: Permission::Expr(Expr { canon: "$auth.id = id".into() }),
                    },
                    PermissionGroup { ops: vec![PermissionOp::Delete], value: Permission::None },
                ])),
                graphql: None,
                nested: vec![],
                comment: Some(DocComment { lines: vec!["App users".into()] }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // delete-Full elides in INFO (the emitter's merging): the
        // read carries the three remaining ops as printed.
        Combo {
            name: "audit".into(),
            sdl: "table audit { drop; schemaless; changefeed { expiry: 30d; include original; }; permissions { select: full; create: full; update: full; delete: full; }; };".into(),
            expected: Stmt::Table(Table {
                name: Ident::new("audit"),
                kind: TableKind::Normal,
                schema_full: Some(false),
                drop: true,
                inline_edges: None,
                inline_references: None,
                changefeed: Some(ChangeFeed {
                    // 30d value-normalizes to the printer's 4w2d.
                    expiry: surrealdb_types::Duration::from_micros(
                        30u64 * 24 * 60 * 60 * 1_000_000,
                    ),
                    include_original: true,
                }),
                // The all-FULL payload prints as the single value (the
                // printer collapses the merged group), delete-Full
                // elided.
                permissions: Some(OpPermissions::Full),
                graphql: None,
                nested: vec![],
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // The bare form reads back TYPE NORMAL (the fail-closed bare
        // emission — the `any` escape hatch is its own statement).
        Combo {
            name: "blobby".into(),
            sdl: "table blobby { schemaless; };".into(),
            expected: Stmt::Table(Table {
                name: Ident::new("blobby"),
                kind: TableKind::Normal,
                schema_full: Some(false),
                drop: false,
                inline_edges: None,
                inline_references: None,
                changefeed: None,
                permissions: Some(OpPermissions::None),
                graphql: None,
                nested: vec![],
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // The relation's endpoint fields materialize in the child map
        // and nest (fields alphabetical: at, in, out).
        Combo {
            name: "wrote".into(),
            sdl: "table users2 | orgs -> wrote -> posts { enforced; schemaless; };
         field at: datetime { on: wrote; default: time::now(); };".into(),
            expected: Stmt::Table(Table {
                name: Ident::new("wrote"),
                kind: TableKind::Relation {
                    from: Some(vec![Ident::new("users2"), Ident::new("orgs")]),
                    to: Some(vec![Ident::new("posts")]),
                    enforced: true,
                    lightweight: false,
                },
                schema_full: Some(false),
                drop: false,
                inline_edges: None,
                inline_references: None,
                changefeed: None,
                permissions: Some(OpPermissions::None),
                graphql: None,
                nested: vec![
                    Stmt::Field(Field {
                        name: field_path("at"),
                        kind: Some(Kind::new("datetime")),
                        on: Ident::new("wrote"),
                        default: Some(FieldDefault::Set(Expr { canon: "time::now()".into() })),
                        value: None,
                        assert: None,
                        computed: None,
                        readonly: false,
                        flexible: false,
                        inline: false,
                        reference: None,
                        permissions: full_permissions(),
                        graphql: None,
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                    Stmt::Field(Field {
                        name: field_path("in"),
                        kind: Some(Kind::new("record<users2 | orgs>")),
                        on: Ident::new("wrote"),
                        default: None,
                        value: None,
                        assert: None,
                        computed: None,
                        readonly: false,
                        flexible: false,
                        inline: false,
                        reference: None,
                        permissions: full_permissions(),
                        graphql: None,
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                    Stmt::Field(Field {
                        name: field_path("out"),
                        kind: Some(Kind::new("record<posts>")),
                        on: Ident::new("wrote"),
                        default: None,
                        value: None,
                        assert: None,
                        computed: None,
                        readonly: false,
                        flexible: false,
                        inline: false,
                        reference: None,
                        permissions: full_permissions(),
                        graphql: None,
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                ],
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // The unconstrained relation's endpoints read bare `record`.
        Combo {
            name: "generic_edge".into(),
            sdl: "table relation generic_edge;".into(),
            expected: Stmt::Table(Table {
                name: Ident::new("generic_edge"),
                kind: TableKind::Relation {
                    from: None,
                    to: None,
                    enforced: false,
                    lightweight: false,
                },
                schema_full: Some(false),
                drop: false,
                inline_edges: None,
                inline_references: None,
                changefeed: None,
                permissions: Some(OpPermissions::None),
                graphql: None,
                nested: vec![
                    Stmt::Field(Field {
                        name: field_path("in"),
                        kind: Some(Kind::new("record")),
                        on: Ident::new("generic_edge"),
                        default: None,
                        value: None,
                        assert: None,
                        computed: None,
                        readonly: false,
                        flexible: false,
                        inline: false,
                        reference: None,
                        permissions: full_permissions(),
                        graphql: None,
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                    Stmt::Field(Field {
                        name: field_path("out"),
                        kind: Some(Kind::new("record")),
                        on: Ident::new("generic_edge"),
                        default: None,
                        value: None,
                        assert: None,
                        computed: None,
                        readonly: false,
                        flexible: false,
                        inline: false,
                        reference: None,
                        permissions: full_permissions(),
                        graphql: None,
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                ],
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // An endpoint named `out` — a keyword-shaped table name in
        // list position reads as the endpoint, and its materialized
        // endpoint fields carry the `record<out>` kind (pinning the
        // write-side round trip of keyword-named endpoints).
        Combo {
            name: "rel_out".into(),
            sdl: "table out -> rel_out -> posts { schemaless; };".into(),
            expected: Stmt::Table(Table {
                name: Ident::new("rel_out"),
                kind: TableKind::Relation {
                    from: Some(vec![Ident::new("out")]),
                    to: Some(vec![Ident::new("posts")]),
                    enforced: false,
                    lightweight: false,
                },
                schema_full: Some(false),
                drop: false,
                inline_edges: None,
                inline_references: None,
                changefeed: None,
                permissions: Some(OpPermissions::None),
                graphql: None,
                nested: vec![
                    Stmt::Field(Field {
                        name: field_path("in"),
                        kind: Some(Kind::new("record<out>")),
                        on: Ident::new("rel_out"),
                        default: None,
                        value: None,
                        assert: None,
                        computed: None,
                        readonly: false,
                        flexible: false,
                        inline: false,
                        reference: None,
                        permissions: full_permissions(),
                        graphql: None,
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                    Stmt::Field(Field {
                        name: field_path("out"),
                        kind: Some(Kind::new("record<posts>")),
                        on: Ident::new("rel_out"),
                        default: None,
                        value: None,
                        assert: None,
                        computed: None,
                        readonly: false,
                        flexible: false,
                        inline: false,
                        reference: None,
                        permissions: full_permissions(),
                        graphql: None,
                        comment: None,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    }),
                ],
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "post_stats".into(),
            sdl: "/// Posts per author\ntable view post_stats { select author, count() as posts from posts group by author; };".into(),
            expected: Stmt::Table(Table {
                name: Ident::new("post_stats"),
                kind: TableKind::View {
                    query: Expr {
                        canon: "SELECT author, count() AS posts FROM posts GROUP BY author"
                            .into(),
                    },
                },
                schema_full: None,
                drop: false,
                inline_edges: None,
                inline_references: None,
                changefeed: None,
                permissions: Some(OpPermissions::None),
                graphql: None,
                nested: vec![],
                comment: Some(DocComment { lines: vec!["Posts per author".into()] }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // The 3.3 shapes, live-captured: the LIGHTWEIGHT
        // relation's INFO always prints `ENFORCED LIGHTWEIGHT` (the
        // engine's define normalization — authored without the
        // explicit flag), and the caps print only-when-written after
        // the schema flag (EDGES before REFERENCES).
        Combo {
            name: "lw_likes".into(),
            sdl: "table lw_users { schemaless; };\ntable lw_posts { schemaless; };\ntable lw_users -> lw_likes -> lw_posts lightweight;".into(),
            expected: Stmt::Table(Table {
                name: Ident::new("lw_likes"),
                kind: TableKind::Relation {
                    from: Some(vec![Ident::new("lw_users")]),
                    to: Some(vec![Ident::new("lw_posts")]),
                    enforced: true,
                    lightweight: true,
                },
                schema_full: Some(false),
                drop: false,
                inline_edges: None,
                inline_references: None,
                changefeed: None,
                permissions: Some(OpPermissions::None),
                graphql: None,
                // The engine auto-installs `in`/`out` from the header
                // and rejects any DEFINE FIELD on a lightweight
                // relation — the reader drops the implied fields
                // (nesting them would print SDL the engine itself
                // rejects at define).
                nested: vec![],
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        Combo {
            name: "cap_host".into(),
            sdl: "table cap_host { inline edges: 64; inline references: 16; };".into(),
            expected: Stmt::Table(Table {
                name: Ident::new("cap_host"),
                kind: TableKind::Normal,
                schema_full: Some(false),
                drop: false,
                inline_edges: Some(64),
                inline_references: Some(16),
                changefeed: None,
                permissions: Some(OpPermissions::None),
                graphql: None,
                nested: vec![],
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
        // Lightweight + a cap coexist in one print (caps last before
        // PERMISSIONS — the printer's clause order).
        Combo {
            name: "lw_cap".into(),
            sdl: "table lw_users -> lw_cap -> lw_posts lightweight { inline edges: 8; };"
                .into(),
            expected: Stmt::Table(Table {
                name: Ident::new("lw_cap"),
                kind: TableKind::Relation {
                    from: Some(vec![Ident::new("lw_users")]),
                    to: Some(vec![Ident::new("lw_posts")]),
                    enforced: true,
                    lightweight: true,
                },
                schema_full: Some(false),
                drop: false,
                inline_edges: Some(8),
                inline_references: None,
                changefeed: None,
                permissions: Some(OpPermissions::None),
                graphql: None,
                nested: vec![],
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers::default(),
        },
    ]
}

/// The table leg rides [`read_tables`]: INFO FOR DB's `tables` map
/// plus an `INFO FOR TABLE` child map per table (the child maps ride
/// their DB input).
#[tokio::test]
async fn tables_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u19t").use_db("scratch").await.ok();
    // The relation and the view name their endpoint/source tables;
    // the view's FROM must exist at define time.
    exec::exec_sdl(
        &db,
        "table users2 { schemaless; };
         table orgs { schemaless; };
         table posts { schemaless; };
         table out { schemaless; };",
    )
    .await;
    let combos = table_combos();
    for combo in &combos {
        exec::exec_sdl(&db, &combo.sdl).await;
    }
    let db_text = exec::info_for(&db, "INFO FOR DB").await;
    let db_info = read_info_object(&db_text).expect("real INFO must walk");
    let mut children = BTreeMap::new();
    let InfoValue::Object(tables) = &db_info.0["tables"] else {
        panic!("the tables section is an object");
    };
    for name in tables.0.keys() {
        let text = exec::info_for(&db, &format!("INFO FOR TABLE {name}")).await;
        children.insert(name.clone(), read_info_object(&text).expect("walks"));
    }
    insta::assert_snapshot!("tables_ddl", section_ddl(&db_info, "tables"));
    let entries = read_tables(&db_info, &children);
    // The preamble hosts (users2/orgs/posts/out, and the
    // lightweight endpoints) read like any table but have no combo.
    let extras = ["users2", "orgs", "posts", "out", "lw_users", "lw_posts"];
    assert_eq!(
        entries.len(),
        combos.len() + extras.len(),
        "every table reads back"
    );
    let mut read_out = Vec::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|diag| panic!("entry must read: {diag:?}"));
        read_out.push(entry.clone());
        let Some(combo) = combos.iter().find(|c| c.name == entry.name) else {
            assert!(
                extras.contains(&entry.name.as_str()),
                "read name `{}` is neither a combo nor a preamble host",
                entry.name
            );
            continue;
        };
        assert!(
            SdlFile {
                items: vec![entry.stmt.clone()],
                trailing_comments: Vec::new(),
            }
            .same_definition(&SdlFile {
                items: vec![combo.expected.clone()],
                trailing_comments: Vec::new(),
            }),
            "reverse round trip changed `{}`:\nexpected: {:?}\nread: {:?}",
            combo.name,
            combo.expected,
            entry.stmt,
        );
        assert_eq!(entry.markers, combo.markers);
    }
    insta::assert_snapshot!("tables_sdl", written(&read_out));
    assert_fmt_fixed_point(&written(&read_out));
}

// ---------------------------------------------------------------------
// access — the high tier's discriminator statement
// ---------------------------------------------------------------------

/// The auto-issued HS512 block as every jwt-less record access prints
/// (live-probed `KEY '[REDACTED]' WITH ISSUER KEY '[REDACTED]'`): the
/// read adopts the sentinel and folds the printed mirror to the
/// key-less issuer — the authored-mirror shape.
fn auto_issued_hs512() -> Option<JwtSource> {
    Some(JwtSource {
        verify: JwtVerify::Key {
            algorithm: Algorithm::Hs512,
            key: Expr {
                canon: "'[REDACTED]'".to_string(),
            },
        },
        audience: None,
        issuer: Some(JwtIssuer {
            algorithm: None,
            key: None,
        }),
    })
}

fn access_duration(
    grant: Option<&str>,
    token: Option<&str>,
    session: Option<&str>,
) -> Option<AccessDuration> {
    let canon = |text: &str| {
        Some(Expr {
            canon: text.to_string(),
        })
    };
    Some(AccessDuration {
        grant: grant.and_then(canon),
        token: token.and_then(canon),
        session: session.and_then(canon),
    })
}

fn access_combos() -> Vec<Combo> {
    let mut combos = Vec::new();
    let redacted = Markers {
        redacted: true,
        ..Markers::default()
    };
    // jwt + url, verify-only: TOKEN omits entirely, SESSION carries.
    combos.push(Combo {
        name: "a_url".to_string(),
        sdl: "access a_url { on: db; type: jwt; url: 'https://sso.example/jwks'; duration { \
              session: 8h; }; };"
            .to_string(),
        markers: Markers::default(),
        expected: Stmt::Access(Access {
            name: Ident::new("a_url"),
            on: Authored::canonical(Level::Database),
            kind: AccessKind::Jwt {
                source: JwtSource {
                    verify: JwtVerify::Url(Expr {
                        canon: "'https://sso.example/jwks'".to_string(),
                    }),
                    audience: None,
                    issuer: None,
                },
            },
            duration: access_duration(None, None, Some("8h")),
            authenticate: None,
            context: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    // jwt + symmetric key: the sentinel is SDL's only spelling; INFO
    // redacts the key and prints the mirrored issuer, which reads as
    // the key-less mirror.
    combos.push(Combo {
        name: "a_hs".to_string(),
        sdl: "access a_hs { on: db; type: jwt; algorithm: hs256; key: '[REDACTED]'; duration { \
              token: 2h; }; };"
            .to_string(),
        markers: redacted.clone(),
        expected: Stmt::Access(Access {
            name: Ident::new("a_hs"),
            on: Authored::canonical(Level::Database),
            kind: AccessKind::Jwt {
                source: JwtSource {
                    verify: JwtVerify::Key {
                        algorithm: Algorithm::Hs256,
                        key: Expr {
                            canon: "'[REDACTED]'".to_string(),
                        },
                    },
                    audience: None,
                    issuer: Some(JwtIssuer {
                        algorithm: None,
                        key: None,
                    }),
                },
            },
            duration: access_duration(None, Some("2h"), Some("NONE")),
            authenticate: None,
            context: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    // jwt + asymmetric key: the public verify key renders verbatim;
    // only the issuer key redacts (and never its algorithm). Issuing
    // gains TOKEN at its 1h default.
    combos.push(Combo {
        name: "a_es".to_string(),
        sdl: "access a_es { on: db; type: jwt; algorithm: es256; key: 'public_verify_pem'; \
              issuer { key: '[REDACTED]'; }; };"
            .to_string(),
        markers: redacted.clone(),
        expected: Stmt::Access(Access {
            name: Ident::new("a_es"),
            on: Authored::canonical(Level::Database),
            kind: AccessKind::Jwt {
                source: JwtSource {
                    verify: JwtVerify::Key {
                        algorithm: Algorithm::Es256,
                        key: Expr {
                            canon: "'public_verify_pem'".to_string(),
                        },
                    },
                    audience: None,
                    issuer: Some(JwtIssuer {
                        algorithm: None,
                        key: Some(Expr {
                            canon: "'[REDACTED]'".to_string(),
                        }),
                    }),
                },
            },
            duration: access_duration(None, Some("1h"), Some("NONE")),
            authenticate: None,
            context: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    // jwt + URL + issuer: the issuer key rides beside a URL verify
    // (its authored ALGORITHM never renders — live-probed), the key
    // redacts, and issuing gains TOKEN at its 1h default.
    combos.push(Combo {
        name: "a_urli".to_string(),
        sdl: "access a_urli { on: db; type: jwt; url: 'https://u.example/jwks'; issuer { key: \
              '[REDACTED]'; }; };"
            .to_string(),
        markers: redacted.clone(),
        expected: Stmt::Access(Access {
            name: Ident::new("a_urli"),
            on: Authored::canonical(Level::Database),
            kind: AccessKind::Jwt {
                source: JwtSource {
                    verify: JwtVerify::Url(Expr {
                        canon: "'https://u.example/jwks'".to_string(),
                    }),
                    audience: None,
                    issuer: Some(JwtIssuer {
                        algorithm: None,
                        key: Some(Expr {
                            canon: "'[REDACTED]'".to_string(),
                        }),
                    }),
                },
            },
            duration: access_duration(None, Some("1h"), Some("NONE")),
            authenticate: None,
            context: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    // record: braced signup/signin, no `jwt { … }` — the auto-issued
    // HS512 block reads through the ordinary WITH JWT arm.
    combos.push(Combo {
        name: "a_rec".to_string(),
        sdl: "access a_rec { on: db; type: record; signup: { create user set email = $email \
              }; signin: { select * from user where email = $email }; };"
            .to_string(),
        markers: redacted.clone(),
        expected: Stmt::Access(Access {
            name: Ident::new("a_rec"),
            on: Authored::canonical(Level::Database),
            kind: AccessKind::Record {
                signup: Some(Expr {
                    canon: "{ CREATE user SET email = $email }".to_string(),
                }),
                signin: Some(Expr {
                    canon: "{ SELECT * FROM user WHERE email = $email }".to_string(),
                }),
                jwt: auto_issued_hs512(),
                refresh: false,
            },
            duration: access_duration(None, Some("1h"), Some("NONE")),
            authenticate: None,
            context: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    // record + refresh + an asymmetric jwt block with an issuer: the
    // refresh makes the access grant-capable (FOR GRANT materializes
    // at the 4w2d default), the issuer key redacts, its EDDSA
    // algorithm never renders.
    combos.push(Combo {
        name: "a_recjwt".to_string(),
        sdl: "access a_recjwt { on: db; type: record; refresh; signin: { select id from user \
              where email = $email }; jwt { algorithm: eddsa; key: 'rec_signing'; issuer { \
              key: '[REDACTED]'; }; }; };"
            .to_string(),
        markers: redacted.clone(),
        expected: Stmt::Access(Access {
            name: Ident::new("a_recjwt"),
            on: Authored::canonical(Level::Database),
            kind: AccessKind::Record {
                signup: None,
                signin: Some(Expr {
                    canon: "{ SELECT id FROM user WHERE email = $email }".to_string(),
                }),
                jwt: Some(JwtSource {
                    verify: JwtVerify::Key {
                        algorithm: Algorithm::EdDsa,
                        key: Expr {
                            canon: "'rec_signing'".to_string(),
                        },
                    },
                    audience: None,
                    issuer: Some(JwtIssuer {
                        algorithm: None,
                        key: Some(Expr {
                            canon: "'[REDACTED]'".to_string(),
                        }),
                    }),
                }),
                refresh: true,
            },
            duration: access_duration(Some("4w2d"), Some("1h"), Some("NONE")),
            authenticate: None,
            context: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    // bearer for user, grant-capable: the 30d default prints `4w2d`.
    combos.push(Combo {
        name: "a_buser".to_string(),
        sdl: "access a_buser { on: db; type: bearer; for: user; duration { grant: 30d; token: \
              1d; }; };"
            .to_string(),
        markers: Markers::default(),
        expected: Stmt::Access(Access {
            name: Ident::new("a_buser"),
            on: Authored::canonical(Level::Database),
            kind: AccessKind::Bearer {
                subject: BearerSubject::User,
                jwt: None,
            },
            duration: access_duration(Some("4w2d"), Some("1d"), Some("NONE")),
            authenticate: None,
            context: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    // bearer for record with a `jwt { … }`: stored but NEVER rendered
    // (live-probed) — the read carries absence, and nothing was
    // adopted, so no redacted marker.
    combos.push(Combo {
        name: "a_brec".to_string(),
        sdl: "access a_brec { on: db; type: bearer; for: record; jwt { algorithm: hs256; key: \
              '[REDACTED]'; }; };"
            .to_string(),
        markers: Markers::default(),
        expected: Stmt::Access(Access {
            name: Ident::new("a_brec"),
            on: Authored::canonical(Level::Database),
            kind: AccessKind::Bearer {
                subject: BearerSubject::Record,
                jwt: None,
            },
            duration: access_duration(Some("4w2d"), Some("1h"), Some("NONE")),
            authenticate: None,
            context: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    // authenticate prints bare, before DURATION; a verify-only access
    // carries SESSION NONE alone.
    combos.push(Combo {
        name: "a_auth".to_string(),
        sdl: "access a_auth { on: db; type: jwt; url: 'https://x.example/jwks'; authenticate: \
              $auth.admin = true; };"
            .to_string(),
        markers: Markers::default(),
        expected: Stmt::Access(Access {
            name: Ident::new("a_auth"),
            on: Authored::canonical(Level::Database),
            kind: AccessKind::Jwt {
                source: JwtSource {
                    verify: JwtVerify::Url(Expr {
                        canon: "'https://x.example/jwks'".to_string(),
                    }),
                    audience: None,
                    issuer: None,
                },
            },
            duration: access_duration(None, None, Some("NONE")),
            authenticate: Some(Expr {
                canon: "$auth.admin = true".to_string(),
            }),
            context: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    // The 3.3 clauses: AUDIENCE adopts decoded on the jwt
    // source; CONTEXT adopts on a record access — its canon is the
    // whole-statement re-print of the payload (INFO prints the
    // expression unevaluated).
    combos.push(Combo {
        name: "a_aud".to_string(),
        sdl: "access a_aud { on: db; type: jwt; url: 'https://sso.example/jwks'; audience: \
              'aud1', 'aud2'; duration { session: 8h; }; };"
            .to_string(),
        markers: Markers::default(),
        expected: Stmt::Access(Access {
            name: Ident::new("a_aud"),
            on: Authored::canonical(Level::Database),
            kind: AccessKind::Jwt {
                source: JwtSource {
                    verify: JwtVerify::Url(Expr {
                        canon: "'https://sso.example/jwks'".to_string(),
                    }),
                    audience: Some(vec!["aud1".to_string(), "aud2".to_string()]),
                    issuer: None,
                },
            },
            duration: access_duration(None, None, Some("8h")),
            authenticate: None,
            context: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    combos.push(Combo {
        name: "a_ctx".to_string(),
        sdl: "access a_ctx { on: db; type: record; signup: { create user set email = $email \
              }; context: { extra: 1, tier: 'gold' }; };"
            .to_string(),
        markers: redacted.clone(),
        expected: Stmt::Access(Access {
            name: Ident::new("a_ctx"),
            on: Authored::canonical(Level::Database),
            kind: AccessKind::Record {
                signup: Some(Expr {
                    canon: "{ CREATE user SET email = $email }".to_string(),
                }),
                signin: None,
                jwt: auto_issued_hs512(),
                refresh: false,
            },
            duration: access_duration(None, Some("1h"), Some("NONE")),
            authenticate: None,
            context: Some(Expr {
                canon: "{ extra: 1, tier: 'gold' }".to_string(),
            }),
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
    });
    combos
}

#[tokio::test]
async fn accesses_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u19bacc").use_db("scratch").await.ok();
    reverse_leg(
        &db,
        &access_combos(),
        "INFO FOR DB",
        "accesses",
        read_accesses,
        "accesses",
        &[],
    )
    .await;
}

/// `ON <LEVEL>` rides the DDL text at every tier — the same
/// `accesses` section name reads at ROOT and NS (`users`'s shape).
#[tokio::test]
async fn accesses_read_at_every_tier() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u19btier").use_db("scratch").await.ok();
    exec::exec_sdl(
        &db,
        "access root_sso { on: root; type: jwt; url: 'https://r.example/jwks'; };",
    )
    .await;
    exec::exec_sdl(
        &db,
        "access ns_sso { on: ns; type: jwt; url: 'https://n.example/jwks'; };",
    )
    .await;
    for (query, expected_name, expected_level) in [
        (
            "INFO FOR ROOT",
            "root_sso",
            Authored::canonical(Level::Root),
        ),
        (
            "INFO FOR NS",
            "ns_sso",
            Authored::canonical(Level::Namespace),
        ),
    ] {
        let text = exec::info_for(&db, query).await;
        let info = read_info_object(&text).expect("real INFO must walk");
        let entries = read_accesses(&info);
        let entry = entries
            .into_iter()
            .find(|e| e.as_ref().map(|e| e.name == expected_name).unwrap_or(false))
            .unwrap_or_else(|| panic!("{expected_name} must read"))
            .expect("it must read cleanly");
        let Stmt::Access(access) = entry.stmt else {
            panic!("must be an access");
        };
        assert_eq!(access.on, expected_level);
    }
}

// ---------------------------------------------------------------------
// api — the structural-sub-block statement's reverse
// ---------------------------------------------------------------------

fn api_middleware(path: &str, args: &[&str]) -> Middleware {
    let (kind, segments) = match path.strip_prefix("fn::") {
        Some(rest) => (FnKind::Fn, vec![rest.to_string()]),
        None => {
            let parts: Vec<String> = path.split("::").map(str::to_string).collect();
            (FnKind::Bare, parts)
        }
    };
    Middleware {
        path: FnPath { kind, segments },
        args: args
            .iter()
            .map(|arg| Expr {
                canon: (*arg).to_string(),
            })
            .collect(),
    }
}

fn group_config(middleware: Vec<Middleware>, permissions: Option<Permission>) -> ApiGroupConfig {
    ApiGroupConfig {
        middleware,
        permissions,
    }
}

fn api_action(methods: &[ApiMethod], config: ApiGroupConfig, then: &str) -> ApiAction {
    ApiAction {
        methods: methods.to_vec(),
        config,
        action: Expr {
            canon: then.to_string(),
        },
        comments: Vec::new(),
        trailing_comments: Vec::new(),
        clause_comments: Vec::new(),
    }
}

fn apis_combos() -> Vec<Combo> {
    // Minimal group + doc comment; the printer's always-materialized
    // `FOR any PERMISSIONS FULL` folds to an absent `any`, and the
    // group's `PERMISSIONS FULL` elides.
    let combos = vec![
        Combo {
            name: "/health".to_string(),
            sdl: "/// Health check\napi '/health' { get { then: { { status: 200, body: { ok: \
                  true } } }; }; };"
                .to_string(),
            markers: Markers::default(),
            expected: Stmt::Api(Api {
                path: "/health".to_string(),
                actions: vec![api_action(
                    &[ApiMethod::Get],
                    group_config(Vec::new(), None),
                    "{ { status: 200, body: { ok: true } } }",
                )],
                any: None,
                comment: Some(DocComment {
                    lines: vec!["Health check".to_string()],
                }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
        },
        // Method comma list.
        Combo {
            name: "/echo".to_string(),
            sdl: "api '/echo' { post, put { then: { { status: 200, body: { you_sent: \
              $request.body } } }; }; };"
                .to_string(),
            markers: Markers::default(),
            expected: Stmt::Api(Api {
                path: "/echo".to_string(),
                actions: vec![api_action(
                    &[ApiMethod::Post, ApiMethod::Put],
                    group_config(Vec::new(), None),
                    "{ { status: 200, body: { you_sent: $request.body } } }",
                )],
                any: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
        },
        // Wildcard path + any with middleware and a fallback + a group
        // permissions expression.
        Combo {
            name: "/admin/*rest".to_string(),
            sdl: "api '/admin/*rest' { any { middleware: api::timeout(5s); then: { { status: 404, \
              body: { error: 'no such route' } } }; }; get, delete { permissions: \
              $auth.admin = true; then: { { status: 200 } }; }; };"
                .to_string(),
            markers: Markers::default(),
            expected: Stmt::Api(Api {
                path: "/admin/*rest".to_string(),
                actions: vec![api_action(
                    &[ApiMethod::Get, ApiMethod::Delete],
                    group_config(
                        Vec::new(),
                        Some(Permission::Expr(Expr {
                            canon: "$auth.admin = true".to_string(),
                        })),
                    ),
                    "{ { status: 200 } }",
                )],
                any: Some(ApiAnyBlock {
                    config: group_config(vec![api_middleware("api::timeout", &["5s"])], None),
                    fallback: Some(Expr {
                        canon: "{ { status: 404, body: { error: 'no such route' } } }".to_string(),
                    }),
                    comments: Vec::new(),
                    trailing_comments: Vec::new(),
                    clause_comments: Vec::new(),
                }),
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
        },
        // `any` with middleware and NO fallback: the middleware keeps the
        // block present (the absent-`any` rule needs all three absences).
        Combo {
            name: "/anymw".to_string(),
            sdl: "api '/anymw' { any { middleware: api::timeout(5s); }; get { then: { { status: \
              200 } }; }; };"
                .to_string(),
            markers: Markers::default(),
            expected: Stmt::Api(Api {
                path: "/anymw".to_string(),
                actions: vec![api_action(
                    &[ApiMethod::Get],
                    group_config(Vec::new(), None),
                    "{ { status: 200 } }",
                )],
                any: Some(ApiAnyBlock {
                    config: group_config(vec![api_middleware("api::timeout", &["5s"])], None),
                    fallback: None,
                    comments: Vec::new(),
                    trailing_comments: Vec::new(),
                    clause_comments: Vec::new(),
                }),
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
        },
        // An authored `any { permissions: full; }` is representation-equal
        // to absence once the default elides — the read carries the fold.
        Combo {
            name: "/anyfull".to_string(),
            sdl: "api '/anyfull' { any { permissions: full; }; get { then: { { status: 200 } }; \
              }; };"
                .to_string(),
            markers: Markers::default(),
            expected: Stmt::Api(Api {
                path: "/anyfull".to_string(),
                actions: vec![api_action(
                    &[ApiMethod::Get],
                    group_config(Vec::new(), None),
                    "{ { status: 200 } }",
                )],
                any: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
        },
        // All three permission forms across groups: FULL elides, NONE and
        // the WHERE expression ride.
        Combo {
            name: "/perms".to_string(),
            sdl: "api '/perms' { get { permissions: full; then: { { status: 200 } }; }; delete { \
              permissions: none; then: { { status: 204 } }; }; trace { permissions: \
              $auth.admin = true; then: { { status: 200 } }; }; };"
                .to_string(),
            markers: Markers::default(),
            expected: Stmt::Api(Api {
                path: "/perms".to_string(),
                actions: vec![
                    api_action(
                        &[ApiMethod::Get],
                        group_config(Vec::new(), None),
                        "{ { status: 200 } }",
                    ),
                    api_action(
                        &[ApiMethod::Delete],
                        group_config(Vec::new(), Some(Permission::None)),
                        "{ { status: 204 } }",
                    ),
                    api_action(
                        &[ApiMethod::Trace],
                        group_config(
                            Vec::new(),
                            Some(Permission::Expr(Expr {
                                canon: "$auth.admin = true".to_string(),
                            })),
                        ),
                        "{ { status: 200 } }",
                    ),
                ],
                any: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
        },
        // A two-entry middleware list with fn::-prefixed and computed
        // argument shapes, plus a NONE permission.
        Combo {
            name: "/mid".to_string(),
            sdl: "api '/mid' { get { middleware: api::timeout(1s), fn::rate_limit(100); \
              permissions: none; then: { { status: 200 } }; }; };"
                .to_string(),
            markers: Markers::default(),
            expected: Stmt::Api(Api {
                path: "/mid".to_string(),
                actions: vec![api_action(
                    &[ApiMethod::Get],
                    group_config(
                        vec![
                            api_middleware("api::timeout", &["1s"]),
                            api_middleware("fn::rate_limit", &["100"]),
                        ],
                        Some(Permission::None),
                    ),
                    "{ { status: 200 } }",
                )],
                any: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
        },
        // The bare form: `FOR any PERMISSIONS FULL` only — everything
        // folds to the empty statement.
        Combo {
            name: "/empty".to_string(),
            sdl: "api '/empty';".to_string(),
            markers: Markers::default(),
            expected: Stmt::Api(Api {
                path: "/empty".to_string(),
                actions: Vec::new(),
                any: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
        },
        // The path flip, live-probed: the catalog re-prints its PARSED
        // path, so an escape-authored static segment arrives as a dynamic
        // segment and the read carries the printed text verbatim. (The
        // escape is only storable segment-initially — SDL
        // `'/\\:x'`, value `/\:x` — and prints flipped as `'/:x'`.)
        Combo {
            name: "/:x".to_string(),
            sdl: "api '/\\\\:x' { get { then: { { status: 200 } }; }; };".to_string(),
            markers: Markers::default(),
            expected: Stmt::Api(Api {
                path: "/:x".to_string(),
                actions: vec![api_action(
                    &[ApiMethod::Get],
                    group_config(Vec::new(), None),
                    "{ { status: 200 } }",
                )],
                any: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
        },
    ];
    combos
}

#[tokio::test]
async fn apis_reverse_leg() {
    let db = exec::mem_db().await;
    db.use_ns("sdl_u19bapi").use_db("scratch").await.ok();
    reverse_leg(
        &db,
        &apis_combos(),
        "INFO FOR DB",
        "apis",
        read_apis,
        "apis",
        &[],
    )
    .await;
}

/// A second `FOR any` clause — upstream folds it config-last-wins at
/// define time, so live INFO never carries one; hand-fed interchange
/// text routes to verbatim `raw` (SDL's single-`any` rule is a parse
/// error, so the shape is inexpressible).
#[test]
fn api_duplicate_any_routes_raw() {
    let text = r#"{ apis: { "/dup": "DEFINE API '/dup' FOR any PERMISSIONS FULL FOR any PERMISSIONS NONE THEN { { status: 500 } }" } }"#;
    let info = read_info_object(text).expect("the interchange walks");
    let entries = read_apis(&info);
    let [entry] = entries.as_slice() else {
        panic!("one entry: {entries:?}");
    };
    let entry = entry.as_ref().expect("a raw route is never an error");
    assert_eq!(entry.markers.raw_routed, Some(RawRoute::ApiDuplicateAny));
    assert_eq!(entry.name, "/dup");
    match &entry.stmt {
        Stmt::Raw(raw) => {
            assert!(raw.body.starts_with("DEFINE API"));
            assert!(raw.body.ends_with(';'), "the terminator normalizes on");
        }
        other => panic!("expected a raw statement: {other:?}"),
    }
}
