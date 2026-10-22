//! Layer-4 smoke shape: valid DDL must EXECUTE through the embedded
//! engine (SDK only, in-memory, no sockets). A sample suite builds on
//! this before releases; the parse oracle cannot see catalog-level
//! rejections.

use surrealdb::Surreal;
use surrealdb::engine::local::Mem;

#[tokio::test]
async fn define_executes_in_embedded_mem() -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query("DEFINE TABLE user;").await?.check()?;
    Ok(())
}

/// The DDL this unit emits must not just parse — it must execute:
/// the namespace lands in the catalog with its COMMENT intact.
#[tokio::test]
async fn emitted_namespace_ddl_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "/// Acme Corporation\nnamespace acme;\n";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    let info: surrealdb::types::Value = db.query("INFO FOR ROOT").await?.take(0)?;
    use surrealdb::types::ToSql;
    let info = info.to_sql();
    assert!(
        info.contains("acme"),
        "namespace must appear in INFO FOR ROOT: {info}"
    );
    assert!(
        info.contains("Acme Corporation"),
        "the COMMENT must survive the round trip: {info}"
    );
    Ok(())
}

/// spec/sequence.md's probed semantics: a bare define starts counting
/// at 0, and the short form starts at its START value. The parse
/// oracle cannot see catalog-level behavior — this is the layer that
/// samples it.
#[tokio::test]
async fn emitted_sequence_ddl_executes_and_counts() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
sequence row_ids;
sequence invoice_number = 1000;
sequence timed {
    timeout: none;
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);
    // The sentinel emits explicitly and must execute (catalog-level
    // acceptance the parse oracle cannot see).
    assert!(ddl.contains("TIMEOUT NONE;"), "{ddl}");

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    // `sequence::nextval(<name>)` continues from the sequence's START
    // (probe-verified spec fact: bare define → 0 then 1; the name is
    // passed as a string at exec time).
    let first: surrealdb::types::Value = db
        .query("RETURN sequence::nextval('row_ids')")
        .await?
        .take(0)?;
    use surrealdb::types::ToSql;
    assert_eq!(first.to_sql(), "0", "bare sequence starts at 0");
    let second: surrealdb::types::Value = db
        .query("RETURN sequence::nextval('row_ids')")
        .await?
        .take(0)?;
    assert_eq!(second.to_sql(), "1", "then it increments");

    let invoice: surrealdb::types::Value = db
        .query("RETURN sequence::nextval('invoice_number')")
        .await?
        .take(0)?;
    assert_eq!(invoice.to_sql(), "1000", "START materializes");
    Ok(())
}

/// Params land in the catalog with their values, comments, and
/// permissions intact — including the define-time evaluation of the
/// value expression (the stored value is the RESULT, spec/param.md).
#[tokio::test]
async fn emitted_param_ddl_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// Default page size for list endpoints
param $default_limit = 50;

param $internal_seed {
    value: 1 + 1;
    permissions: none;
};

param $unset;
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    let info: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    use surrealdb::types::ToSql;
    let info = info.to_sql();
    assert!(info.contains("default_limit"), "{info}");
    assert!(
        info.contains("Default page size for list endpoints"),
        "the COMMENT must survive: {info}"
    );
    assert!(info.contains("internal_seed"), "{info}");
    assert!(
        info.contains("unset"),
        "the bare param stores VALUE NONE: {info}"
    );
    // Define-time evaluation: 1 + 1 is stored as 2.
    let two: surrealdb::types::Value = db.query("RETURN $internal_seed").await?.take(0)?;
    assert_eq!(two.to_sql(), "2");
    // PERMISSIONS NONE hides the param from a non-root scope; from the
    // root session used here it still reads.
    Ok(())
}

/// A mixed batch (the shape a real `surform build` emits) executes as
/// one query: namespace with session USE, sequences, params.
#[tokio::test]
async fn mixed_batch_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// Acme Corporation
namespace acme;

sequence order_number {
    batch: 100;
    start: 5000;
    timeout: 5s;
};

param $endpoints = { read: '/api/read', write: '/api/write' };
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);

    let db = Surreal::new::<Mem>(()).await?;
    // The batch switches namespace itself (`USE NS acme` follows the
    // DEFINE), but a connection needs a database selected first —
    // sequences are DB-level, so they land in acme/scratch.
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;
    Ok(())
}

/// The named exec-smoke batch — namespace + database
/// (strict + changefeed) + sequence + param as one query. The
/// database lands in the catalog with its changefeed, the `USE NS …
/// DB` line puts the session inside it, and `strict` is ACTIVE: a
/// CREATE on an undeclared table errors — while INFO FOR NS shows no
/// STRICT, the preserve-item the normalizer must honor
/// (spec/database.md, "STRICT decoded").
#[tokio::test]
async fn emitted_namespace_database_sequence_param_batch_executes()
-> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// Acme Corporation
namespace acme;

/// Primary application database
database app {
    strict;
    changefeed {
        expiry: 7d;
        include original;
    };
};

sequence invoice_number = 1000;

/// Default page size
param $default_limit = 50;
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);
    assert!(ddl.contains("USE NS acme DB app;"), "{ddl}");

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    // The batch's USE lines apply inside the query but do not persist
    // across SDK query calls: select acme/app explicitly, then the
    // catalog shows the database with its changefeed and the db-level
    // statements that followed the `USE NS … DB` line in the batch.
    db.use_ns("acme").use_db("app").await?;

    use surrealdb::types::ToSql;
    let info: surrealdb::types::Value = db.query("INFO FOR NS").await?.take(0)?;
    let info = info.to_sql();
    assert!(info.contains("app"), "database in INFO FOR NS: {info}");
    assert!(
        info.contains("CHANGEFEED"),
        "the changefeed must survive: {info}"
    );
    let db_info: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    let db_info = db_info.to_sql();
    assert!(
        db_info.contains("invoice_number") && db_info.contains("default_limit"),
        "the batch's sequence and param landed in acme/app: {db_info}"
    );

    // Strict is active: an undefined table cannot be created.
    let strict_probe = db.query("CREATE undefined_table;").await;
    let rejected = match strict_probe {
        Err(_) => true,
        Ok(response) => response.check().is_err(),
    };
    assert!(rejected, "STRICT must reject undefined tables");
    Ok(())
}

/// Buckets need the experimental `files` capability at define time;
/// the SDK exposes it through the engine config, so bucket DDL
/// executes too — this is the catalog-level acceptance the parse
/// oracle cannot see (the backend allowlist is a runtime concern).
#[tokio::test]
async fn emitted_bucket_ddl_executes_with_files_capability()
-> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// Ephemeral test uploads
bucket scratch_uploads {
    backend: 'memory';
    permissions: none;
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);

    use surrealdb::opt::capabilities::Capabilities;
    let config = surrealdb::opt::Config::default()
        .capabilities(Capabilities::default().with_all_experimental_features_allowed());
    let db = Surreal::new::<Mem>(config).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    use surrealdb::types::ToSql;
    let info: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    let info = info.to_sql();
    assert!(
        info.contains("scratch_uploads"),
        "bucket in INFO FOR DB: {info}"
    );
    assert!(
        info.contains("BACKEND 'memory'"),
        "the backend must survive: {info}"
    );
    // PERMISSIONS NONE must survive too (SDL's explicit clause).
    assert!(info.contains("PERMISSIONS NONE"), "{info}");

    // The spec's verified runtime rule: a bucket without an explicit
    // backend requires a server-configured global bucket — none
    // exists here, so the define-time rejection is the catalog-level
    // behavior this layer exists to sample.
    let shared = db.query("DEFINE BUCKET OVERWRITE shared;").await;
    let rejected = match shared {
        Err(_) => true,
        Ok(response) => response.check().is_err(),
    };
    assert!(
        rejected,
        "a clause-less bucket must require a global bucket"
    );
    Ok(())
}

/// spec/user.md's secrets protocol, executed: users land at all three
/// levels from the default embedded session (probe-pinned), the
/// define-once `IF NOT EXISTS` re-applies without error, `INFO`
/// redacts set secrets to `PASSHASH '[REDACTED]'` while secret-less
/// users show `PASSHASH ''` — and an `OVERWRITE` without a pass
/// clause CLEARS the credential (the bricking hazard that makes
/// secret-less users define-once in the first place).
#[tokio::test]
async fn emitted_user_ddl_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// CI deployment account
user deployer {
    on: namespace;
    passhash: '$argon2id$v=19$';
    roles: editor, viewer;
    duration {
        token: 1h;
        session: 12h;
    };
};

/// Read-only reporting user (define-once — no secret, `IF NOT
/// EXISTS`).
user analyst {
    on: database;
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);
    assert!(ddl.contains("DEFINE USER OVERWRITE deployer"), "{ddl}");
    assert!(ddl.contains("DEFINE USER IF NOT EXISTS analyst"), "{ddl}");

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;
    // Define-once: the re-apply must not error (and must not clear).
    db.query("DEFINE USER IF NOT EXISTS analyst ON DATABASE;")
        .await?
        .check()?;

    use surrealdb::types::ToSql;
    let root_info: surrealdb::types::Value = db.query("INFO FOR ROOT").await?.take(0)?;
    let root_info = root_info.to_sql();
    let ns_info: surrealdb::types::Value = db.query("INFO FOR NS").await?.take(0)?;
    let ns_info = ns_info.to_sql();
    let db_info: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    let db_info = db_info.to_sql();
    assert!(
        !root_info.contains("deployer"),
        "deployer is a namespace user: {root_info}"
    );
    assert!(
        ns_info.contains("deployer"),
        "deployer in INFO FOR NS users: {ns_info}"
    );
    assert!(
        ns_info.contains("PASSHASH '[REDACTED]'"),
        "the secret redacts: {ns_info}"
    );
    assert!(
        ns_info.contains("ROLES EDITOR, VIEWER"),
        "roles survive: {ns_info}"
    );
    assert!(
        ns_info.contains("DURATION FOR TOKEN 1h, FOR SESSION 12h"),
        "durations survive: {ns_info}"
    );
    assert!(
        db_info.contains("analyst") && db_info.contains("PASSHASH ''"),
        "the secret-less user stores an empty hash: {db_info}"
    );

    // The bricking hazard, demonstrated: OVERWRITE without a pass
    // clause clears the stored credential (SDL's codegen only ever
    // emits that shape for define-once users, never as OVERWRITE).
    db.query("DEFINE USER OVERWRITE deployer ON NAMESPACE;")
        .await?
        .check()?;
    let cleared: surrealdb::types::Value = db.query("INFO FOR NS").await?.take(0)?;
    let cleared = cleared.to_sql();
    assert!(
        cleared.contains("PASSHASH ''"),
        "OVERWRITE without a pass clears the credential: {cleared}"
    );
    Ok(())
}

/// Analyzers execute without any experimental gate (unlike buckets —
/// probe-pinned), land in `INFO FOR DB` with their clauses intact,
/// and the empty analyzer is accepted though inert. The `mod::` fn
/// path needs the surrealism capability at define time, so its
/// engine runs with the experimental capabilities enabled.
#[tokio::test]
async fn emitted_analyzer_ddl_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// English text search
analyzer english {
    tokenizers: class;
    filters: snowball(en), lowercase;
};

analyzer empty;
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    use surrealdb::types::ToSql;
    let info: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    let info = info.to_sql();
    assert!(
        info.contains("SNOWBALL(ENGLISH), LOWERCASE"),
        "the alias canonicalizes to the full name through the pipeline: {info}"
    );
    assert!(
        info.contains("'English text search'"),
        "the COMMENT survives: {info}"
    );
    assert!(info.contains("empty"), "the empty analyzer lands: {info}");

    // `mod::` function paths (capability-gated at parse AND define).
    let (mod_file, diags) =
        surform::parser::parse_file("analyzer m3 { function: mod::demo::some; };");
    assert!(diags.is_empty(), "{diags:?}");
    let mod_ddl = surform::codegen::emit(&mod_file);
    use surrealdb::opt::capabilities::Capabilities;
    let config = surrealdb::opt::Config::default()
        .capabilities(Capabilities::default().with_all_experimental_features_allowed());
    let db2 = Surreal::new::<Mem>(config).await?;
    db2.use_ns("sdl_test").use_db("scratch").await?;
    db2.query(&mod_ddl).await?.check()?;
    let info2: surrealdb::types::Value = db2.query("INFO FOR DB").await?.take(0)?;
    let info2 = info2.to_sql();
    assert!(
        info2.contains("FUNCTION mod::demo::some"),
        "the mod:: path survives: {info2}"
    );
    Ok(())
}

/// Events execute against their table (defined with raw DDL — the
/// `table` statement is not this suite's subject), land in
/// `INFO FOR TABLE`'s events
/// map with the spec's round-trip materializations (`WHEN true` for
/// an absent `when`, `ASYNC RETRY n MAXDEPTH n` always explicit), and
/// the THEN side is live behavior: the `when`-filtered event fires on
/// CREATE only, the unfiltered one on every change.
#[tokio::test]
async fn emitted_event_ddl_executes_and_fires() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// Log every user creation
event user_created {
    on: user;
    when: $event = 'CREATE';
    then: { create log set user = $after.id, at = time::now(); };
};

event touch_counter {
    on: post;
    then: (update counter:posts set total = total + 1);
};

event webhook_fanout {
    on: invoice;
    async {
        retry: 5;
        maxdepth: 8;
    };
    then: (create delivery set invoice = $after.id);
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);
    // The skeleton may lay clause-per-line; the ordering claims
    // compare whitespace-collapsed.
    let flat = ddl.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("DEFINE EVENT OVERWRITE user_created ON `user` WHEN"),
        "{ddl}"
    );
    assert!(
        flat.contains("DEFINE EVENT OVERWRITE webhook_fanout ON invoice ASYNC RETRY 5 MAXDEPTH 8"),
        "{ddl}"
    );

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    // The event's target tables: raw DDL, not the `table` statement.
    db.query("DEFINE TABLE user; DEFINE TABLE post; DEFINE TABLE invoice; DEFINE TABLE log;")
        .await?
        .check()?;
    db.query(&ddl).await?.check()?;

    use surrealdb::types::ToSql;
    let user_info: surrealdb::types::Value = db.query("INFO FOR TABLE user").await?.take(0)?;
    let user_info = user_info.to_sql();
    assert!(
        user_info.contains("DEFINE EVENT user_created"),
        "the event lands in its table's INFO: {user_info}"
    );
    assert!(
        user_info.contains("WHEN $event = 'CREATE'"),
        "the when survives: {user_info}"
    );
    let post_info: surrealdb::types::Value = db.query("INFO FOR TABLE post").await?.take(0)?;
    let post_info = post_info.to_sql();
    assert!(
        post_info.contains("DEFINE EVENT touch_counter") && post_info.contains("WHEN true"),
        "the absent `when` materializes in INFO: {post_info}"
    );
    let invoice_info: surrealdb::types::Value =
        db.query("INFO FOR TABLE invoice").await?.take(0)?;
    let invoice_info = invoice_info.to_sql();
    assert!(
        invoice_info.contains("ASYNC RETRY 5 MAXDEPTH 8"),
        "the async parameters survive: {invoice_info}"
    );

    // Live behavior: the counter event fires on CREATE (the record is
    // seeded so `total + 1` is arithmetic, not NONE-folding).
    db.query("CREATE counter:posts SET total = 0;")
        .await?
        .check()?;
    db.query("CREATE post:first SET title = 'hello';")
        .await?
        .check()?;
    let counter: surrealdb::types::Value = db
        .query("SELECT total FROM counter:posts;")
        .await?
        .take(0)?;
    let counter = counter.to_sql();
    assert!(counter.contains("total: 1"), "the THEN fired: {counter}");

    // The when-filter holds: CREATE logs, UPDATE does not.
    db.query("CREATE user:jane SET name = 'Jane';")
        .await?
        .check()?;
    db.query("UPDATE user:jane SET name = 'Janet';")
        .await?
        .check()?;
    let log: surrealdb::types::Value = db.query("SELECT user FROM log;").await?.take(0)?;
    let log = log.to_sql();
    assert!(log.contains("user: user:jane"), "the CREATE logged: {log}");
    // A second entry would mean the UPDATE also logged.
    assert!(!log.contains(", "), "exactly one entry: {log}");
    Ok(())
}

/// The DDL this unit emits must execute: functions land in
/// `INFO FOR DB`, call correctly, and honor the spec's verified
/// optional-parameter call shape (`fn::last_opt(5)` → `5`).
#[tokio::test]
async fn emitted_function_ddl_executes_and_calls() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// Whether the current user may edit this post
fn can_edit($p: record<post>) -> bool {
    return $auth.id = $p.author or $auth.role = 'admin';
} {
    permissions: full;
};

fn tax_total($net: number, $rate: option<number>) {
    let $r = $rate ?? 0.15;
    let $tax = math::round($net * $r);
    $net + $tax
};

fn last_opt($req: number, $opt: option<number>) {
    $opt ?? $req
};

fn util::slugify($s: string) -> string {
    string::lowercase(string::replace($s, ' ', '-'))
} {
    graphql {
        alias: 'slugify';
    };
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);
    let flat = ddl.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("PERMISSIONS FULL COMMENT 'Whether"),
        "explicit full emits: {ddl}"
    );
    assert!(flat.contains("GRAPHQL_ALIAS 'slugify'"), "{ddl}");

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    use surrealdb::types::ToSql;
    let info: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    let info = info.to_sql();
    assert!(
        info.contains("DEFINE FUNCTION fn::can_edit"),
        "the function lands in INFO FOR DB: {info}"
    );
    assert!(
        info.contains("PERMISSIONS FULL"),
        "default/full permissions materialize in INFO: {info}"
    );
    assert!(info.contains("-> bool"), "the return kind survives: {info}");

    // The multi-statement body: the last statement (sans `;`) is the
    // result.
    let mut result = db.query("RETURN fn::tax_total(100, 0.2);").await?.check()?;
    let value: surrealdb::types::Value = result.take(0)?;
    assert_eq!(
        value.to_sql(),
        "120f",
        "100 + round(100 * 0.2): {}",
        value.to_sql()
    );

    // The spec's verified shape: an omitted optional parameter.
    let mut result = db.query("RETURN fn::last_opt(5);").await?.check()?;
    let value: surrealdb::types::Value = result.take(0)?;
    assert_eq!(value.to_sql(), "5", "{}", value.to_sql());

    // Multi-segment names call through `fn::util::slugify`.
    let mut result = db
        .query("RETURN fn::util::slugify('Hello World');")
        .await?
        .check()?;
    let value: surrealdb::types::Value = result.take(0)?;
    assert_eq!(value.to_sql(), "'hello-world'", "{}", value.to_sql());

    // The call succeeds under `PERMISSIONS FULL` with no auth
    // session; the body's comparison folds over the empty $auth
    // (NONE = NONE is true).
    let mut result = db.query("RETURN fn::can_edit(post:1);").await?.check()?;
    let value: surrealdb::types::Value = result.take(0)?;
    assert_eq!(value.to_sql(), "true", "{}", value.to_sql());
    Ok(())
}

/// The DDL this unit emits must execute: all three variants land at
/// their scopes (`config default` hoisted to ROOT; api/graphql at DB
/// level), the middleware arguments are evaluated ONCE at define time
/// (spec/config.md: INFO returns the computed values), and the
/// singleton pin holds — a re-define without `OVERWRITE` errors while
/// `OVERWRITE` converges (spec/config.md's verified facts; the parse
/// oracle cannot see any of this).
#[tokio::test]
async fn emitted_config_ddl_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
config api {
    middleware: timeout(1 + 1);
    permissions: $auth.admin = true;
};

config graphql {
    tables { include: user, post; };
    functions: none;
    depth: 4;
    complexity: 100;
    introspection: none;
};

config default {
    namespace: 'acme';
    database: 'shop';
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);
    // The hoist: with no namespace in the file, the ROOT-level default
    // emits first (the ROOT-region placement).
    assert!(ddl.starts_with("DEFINE CONFIG OVERWRITE DEFAULT"), "{ddl}");

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    use surrealdb::types::ToSql;
    let info: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    let info = info.to_sql();
    assert!(
        info.contains("MIDDLEWARE timeout(2)"),
        "the middleware arg evaluated at define time: {info}"
    );
    assert!(
        info.contains("PERMISSIONS WHERE $auth.admin = true"),
        "the api permissions survive: {info}"
    );
    assert!(
        info.contains(
            "GRAPHQL TABLES INCLUDE user, post FUNCTIONS NONE DEPTH 4 \
                       COMPLEXITY 100 INTROSPECTION NONE"
        ),
        "the graphql fragment in INFO: {info}"
    );

    // The ROOT-level default config (INFO FOR ROOT's structured
    // `defaults` value).
    let root: surrealdb::types::Value = db.query("INFO FOR ROOT").await?.take(0)?;
    let root = root.to_sql();
    assert!(root.contains("acme") && root.contains("shop"), "{root}");

    // The singleton pin: re-defining a variant without OVERWRITE
    // errors (`The config for graphql already exists`).
    let bare = db
        .query("DEFINE CONFIG GRAPHQL TABLES NONE FUNCTIONS NONE;")
        .await;
    let rejected = match bare {
        Err(_) => true,
        Ok(response) => response.check().is_err(),
    };
    assert!(rejected, "a config re-define without OVERWRITE must fail");
    // …while OVERWRITE converges cleanly.
    db.query("DEFINE CONFIG OVERWRITE GRAPHQL TABLES AUTO FUNCTIONS AUTO;")
        .await?
        .check()?;
    let converged: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    let converged = converged.to_sql();
    assert!(
        converged.contains("TABLES AUTO FUNCTIONS AUTO"),
        "OVERWRITE redefined the singleton: {converged}"
    );
    Ok(())
}

/// Scope enforcement is catalog-level (spec/config.md: `config api` /
/// `config graphql` are database-level, `config default` ROOT-level —
/// namespace level rejected): the api define executed from a
/// namespace-only session is the catalog rejection this layer
/// samples.
#[tokio::test]
async fn config_scope_enforcement_executes() -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").await?;
    let define = db
        .query("DEFINE CONFIG OVERWRITE API PERMISSIONS NONE;")
        .await;
    let rejected = match define {
        Err(_) => true,
        Ok(response) => response.check().is_err(),
    };
    assert!(
        rejected,
        "an api config at namespace scope must be rejected by the catalog"
    );
    Ok(())
}

/// spec/index.md's shapes must not just parse — they must execute:
/// indexes land in their tables' catalogs, the bare fulltext
/// materializes `ANALYZER like BM25(1.2,0.75)` server-side, and
/// `CONCURRENTLY` (a transient build directive) is absent from INFO.
/// Tables and fields come FIRST — an index on a missing field fails
/// at exec, not parse, the catalog-level class this layer samples.
#[tokio::test]
async fn emitted_index_ddl_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// Composite lookup for inbox queries
index by_account_thread {
    on: message;
    fields: account, thread_id;
};

index unique one_email {
    on: user;
    fields: email;
    concurrently;
};

index count active_count {
    on: item;
    where: status = 'active';
};

index fulltext body_fts {
    on: post;
    fields: body;
};

index fulltext title_fts {
    on: post;
    fields: title;
    analyzer: english;
    bm25: (1.2, 0.75);
    highlights;
};

index hnsw vec_tuned {
    on: embedding;
    fields: vector;
    dimension: 768;
    distance: minkowski(3);
    type: f16;
    m: 16;
    efc: 200;
    extend_candidates;
    keep_pruned_connections;
};

index diskann big_vec {
    on: document;
    fields: embedding;
    dimension: 1536;
    distance: cosine_normalized;
    type: f16;
    degree: 32;
    l_build: 50;
    alpha: 1.3;
    hashed_vector;
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);
    assert!(
        ddl.contains("DISTANCE MINKOWSKI 3"),
        "the minkowski order emits the bare form: {ddl}"
    );

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    // The indexed tables and their fields, raw DDL rather than the
    // `table`/`field` statements — and the layer-4 point: a missing field would
    // fail HERE, not at parse.
    db.query(
        "DEFINE TABLE message SCHEMAFUL; DEFINE FIELD account ON message TYPE string; \
         DEFINE FIELD thread_id ON message TYPE string; \
         DEFINE TABLE user SCHEMAFUL; DEFINE FIELD email ON user TYPE string; \
         DEFINE TABLE item SCHEMAFUL; DEFINE FIELD status ON item TYPE string; \
         DEFINE TABLE post SCHEMAFUL; DEFINE FIELD body ON post TYPE string; \
         DEFINE FIELD title ON post TYPE string; \
         DEFINE TABLE embedding SCHEMAFUL; DEFINE FIELD vector ON embedding TYPE array; \
         DEFINE TABLE document SCHEMAFUL; DEFINE FIELD embedding ON document TYPE array;",
    )
    .await?
    .check()?;
    db.query(&ddl).await?.check()?;

    use surrealdb::types::ToSql;
    let message_info: surrealdb::types::Value =
        db.query("INFO FOR TABLE message").await?.take(0)?;
    let message_info = message_info.to_sql();
    assert!(
        message_info.contains("DEFINE INDEX by_account_thread"),
        "the index lands in its table's INFO: {message_info}"
    );
    assert!(
        message_info.contains("Composite lookup for inbox queries"),
        "the COMMENT survives: {message_info}"
    );
    let user_info: surrealdb::types::Value = db.query("INFO FOR TABLE user").await?.take(0)?;
    let user_info = user_info.to_sql();
    assert!(
        !user_info.contains("CONCURRENTLY"),
        "the transient build directive never reports in INFO: {user_info}"
    );
    let item_info: surrealdb::types::Value = db.query("INFO FOR TABLE item").await?.take(0)?;
    let item_info = item_info.to_sql();
    assert!(
        item_info.contains("COUNT WHERE status = 'active'"),
        "the count condition survives: {item_info}"
    );
    let post_info: surrealdb::types::Value = db.query("INFO FOR TABLE post").await?.take(0)?;
    let post_info = post_info.to_sql();
    assert!(
        post_info.contains("FULLTEXT ANALYZER like BM25(1.2,0.75)"),
        "the bare fulltext materializes server-side: {post_info}"
    );
    assert!(
        post_info.contains("ANALYZER english"),
        "the tuned analyzer survives: {post_info}"
    );
    let embedding_info: surrealdb::types::Value =
        db.query("INFO FOR TABLE embedding").await?.take(0)?;
    let embedding_info = embedding_info.to_sql();
    assert!(
        embedding_info.contains("HNSW") && embedding_info.contains("MINKOWSKI 3"),
        "the hnsw params survive: {embedding_info}"
    );
    let document_info: surrealdb::types::Value =
        db.query("INFO FOR TABLE document").await?.take(0)?;
    let document_info = document_info.to_sql();
    assert!(
        document_info.contains("DISKANN") && document_info.contains("HASHED_VECTOR"),
        "the diskann params survive: {document_info}"
    );
    Ok(())
}

/// The layer-4 class in its negative form: SDL accepts an index on a
/// field no SDL statement defines (emission is total — no statement
/// requires another), and the rejection happens at exec, not parse —
/// `The field 'nope' does not exist` on the schemafull table
/// (probe-verified: an index on a missing TABLE stores cleanly —
/// indexes may pre-date their table — so the field check is the
/// catalog rejection this layer samples).
#[tokio::test]
async fn index_on_missing_field_fails_at_exec() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "index ghost_idx { on: real; fields: nope; };";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "SDL accepts it: {diags:?}");
    let ddl = surform::codegen::emit(&file);

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query("DEFINE TABLE real SCHEMAFUL; DEFINE FIELD a ON real TYPE string;")
        .await?
        .check()?;
    let executed = db.query(&ddl).await;
    let rejected = match executed {
        Err(_) => true,
        Ok(response) => response.check().is_err(),
    };
    assert!(
        rejected,
        "an index on a missing field must fail at exec: {ddl}"
    );
    Ok(())
}

/// The api DDL this unit emits executes and lands in the catalog:
/// `INFO FOR DB` reports the definitions with the emitter's
/// always-materialized shapes — `FOR any` first on every definition
/// (SDL wrote none on `/health`), `PERMISSIONS …` on every `FOR`
/// clause (SDL wrote none anywhere). Those two shapes are the
/// reverse parser's contract (spec/api.md's reverse section), pinned
/// here where they materialize.
#[tokio::test]
async fn emitted_api_ddl_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// Route health checks
api '/health' {
    get {
        then: { { status: 200, body: { ok: true } } };
    };
};

api '/admin/*rest' {
    any {
        middleware: api::timeout(5s);
        then: { { status: 404, body: { error: 'no such route' } } };
    };
    get, delete {
        permissions: $auth.admin = true;
        then: { { status: 200 } };
    };
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);
    assert!(
        ddl.contains("FOR ANY MIDDLEWARE api::timeout(5s) THEN"),
        "`any` hoists first with its middleware: {ddl}"
    );

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    use surrealdb::types::ToSql;
    let info: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    let info = info.to_sql();
    assert!(info.contains("'/health'"), "{info}");
    assert!(info.contains("'/admin/*rest'"), "{info}");
    assert!(
        info.contains("FOR any PERMISSIONS FULL FOR get PERMISSIONS FULL"),
        "the default config materializes on every definition: {info}"
    );
    assert!(
        info.contains("Route health checks"),
        "the COMMENT survives: {info}"
    );
    Ok(())
}

/// The duplicate-method rule SDL enforces at parse is upstream's
/// define-time error (the parse oracle accepts the folded shape —
/// probe-pinned): the server must agree the shape is invalid, which
/// is the catalog-level rejection this layer samples.
#[tokio::test]
async fn duplicate_api_methods_fail_at_define_time() -> Result<(), Box<dyn std::error::Error>> {
    // SDL-side: eager, one diagnostic — the second group never lands.
    let (file, diags) =
        surform::parser::parse_file("api '/x' { get { then: 1; }; get, post { then: 2; }; };\n");
    assert!(
        matches!(diags.as_slice(), [surform::Diag::DuplicateApiMethod { .. }]),
        "{diags:?}"
    );
    let surform::ast::Stmt::Api(api) = &file.items[0] else {
        panic!("expected the api")
    };
    assert_eq!(api.actions.len(), 1);

    // Server-side: the shape SDL rejected is a define-time error.
    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    let response = db
        .query("DEFINE API '/y' FOR get THEN 1 FOR get, post THEN 2;")
        .await?;
    assert!(
        response.check().is_err(),
        "the server must reject duplicate methods at define time"
    );
    Ok(())
}

/// The access family, every variant, through the engine (layer 4 —
/// the parse oracle cannot see catalog-level behavior). This pins the
/// the reverse-parser contract against the live catalog:
///
/// - `DURATION` always materializes (`FOR SESSION NONE` when unset);
/// - a verify-only access omits `FOR TOKEN` entirely — its declared
///   token duration silently vanishes from round-trips;
/// - a symmetric verify key's mirrored issuer prints (and gains
///   `FOR TOKEN 1h`);
/// - an issuer's `ALGORITHM` never renders (only `WITH ISSUER KEY`);
/// - a jwt-less record access auto-issues HS512 with an issuer
///   present;
/// - **JWT keys render per the catalog's redaction rule**
///   (`catalog/schema/access.rs`, `to_sql_definition` →
///   `AccessType::redacted`): symmetric (hs*) verify keys and every
///   issuer key render `[REDACTED]`; ASYMMETRIC verify keys render
///   verbatim (they are public keys). Key secrets are therefore
///   unrecoverable from INFO — and since SDL itself accepts
///   only the `'[REDACTED]'` sentinel in those slots (SDL-E1056), the
///   engine-side redaction of a REAL key is pinned here through raw
///   SurrealQL, the one spelling still able to carry one.
/// - `FOR GRANT` appears exactly for the grant-capable (record with
///   `refresh`, bearer), with the 30d default spelled `4w2d`;
/// - a bearer's `jwt { … }` is stored but never rendered.
#[tokio::test]
async fn emitted_access_ddl_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// Verify-only JWKS
access sso {
    on: database;
    type: jwt;
    url: 'https://sso.example/jwks';
    duration { token: 2h; };
};

access ops {
    on: database;
    type: jwt;
    algorithm: hs256;
    key: '[REDACTED]';
    duration { session: 8h; };
};

access member {
    on: database;
    type: record;
    refresh;
    signin: (select id from user where email = $email and crypto::argon2::compare(password, $password));
    jwt {
        algorithm: eddsa;
        key: 'member_signing_key';
        issuer { key: '[REDACTED]'; };
    };
};

access guest {
    on: database;
    type: record;
    signin: (select * from user where email = $email);
};

/// CI bearer tokens
access ci_tokens {
    on: database;
    type: bearer;
    for: user;
    jwt { algorithm: hs256; key: '[REDACTED]'; };
    duration { grant: 30d; token: 1d; };
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    use surrealdb::types::ToSql;
    let info: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    let info = info.to_sql();

    // One segment per access definition keeps each pin scoped to its
    // own entry.
    let segment = |name: &str| -> String {
        let start = info
            .find(&format!("DEFINE ACCESS {name} "))
            .unwrap_or_else(|| panic!("`{name}` in INFO: {info}"));
        let rest = &info[start..];
        let end = rest["DEFINE ACCESS ".len()..]
            .find("DEFINE ACCESS")
            .map(|at| at + "DEFINE ACCESS ".len())
            .unwrap_or(rest.len());
        rest[..end].to_string()
    };

    // sso: verify-only — the declared token duration VANISHES (no
    // `FOR TOKEN`), SESSION materializes as NONE, no issuer.
    let sso = segment("sso");
    assert!(sso.contains("TYPE JWT URL"), "{sso}");
    assert!(!sso.contains("FOR TOKEN"), "verify-only omits TOKEN: {sso}");
    assert!(sso.contains("DURATION FOR SESSION NONE"), "{sso}");
    assert!(!sso.contains("WITH ISSUER"), "{sso}");

    // ops: the symmetric key's mirrored issuer prints verbatim (its
    // VALUE redacted like every key) and grants issuing (TOKEN
    // materializes at its 1h default).
    let ops = segment("ops");
    assert!(ops.contains("ALGORITHM HS256 KEY '[REDACTED]'"), "{ops}");
    assert!(ops.contains("WITH ISSUER KEY '[REDACTED]'"), "{ops}");
    assert!(ops.contains("FOR TOKEN 1h"), "{ops}");
    assert!(ops.contains("FOR SESSION 8h"), "{ops}");

    // member: the asymmetric verify key is PUBLIC and renders
    // verbatim; only the issuer key redacts. The issuer's ALGORITHM
    // never renders; refresh makes the access grant-capable.
    let member = segment("member");
    assert!(
        member.contains("WITH JWT ALGORITHM EDDSA KEY 'member_signing_key'"),
        "{member}"
    );
    assert!(member.contains("WITH ISSUER KEY '[REDACTED]'"), "{member}");
    assert!(
        !member.contains("ISSUER ALGORITHM"),
        "issuer algorithm never renders: {member}"
    );
    assert!(member.contains("FOR GRANT"), "refresh grants: {member}");

    // guest: the jwt-less record auto-issues — HS512 with an issuer
    // present. Both key VALUES are redacted (the identical-key fact
    // is pinned from the v3.2.4 source — the Default impl mirrors
    // the random key — since INFO cannot show them).
    let guest = segment("guest");
    assert!(
        guest.contains("WITH JWT ALGORITHM HS512 KEY '[REDACTED]' WITH ISSUER KEY '[REDACTED]'"),
        "{guest}"
    );

    // ci_tokens: bearer jwt stored, never rendered; grant-capable
    // with the 30d default spelled `4w2d`.
    let ci = segment("ci_tokens");
    assert!(ci.contains("TYPE BEARER FOR USER"), "{ci}");
    assert!(
        !ci.contains("WITH JWT"),
        "the bearer jwt never renders: {ci}"
    );
    assert!(ci.contains("FOR GRANT 4w2d"), "30d spells 4w2d: {ci}");
    assert!(ci.contains("FOR TOKEN 1d"), "{ci}");

    // The engine-side redaction of REAL key material, pinned through
    // raw SurrealQL: SDL can no longer author this shape (the
    // SDL-E1056 exposure guard), but the engine still accepts it and
    // must redact the key before INFO ever sees it. (Upstream demands
    // a symmetric issuer key EQUAL the verify key — one value serves
    // both slots; the sentinel rule keeps SDL on the right side of
    // that check by construction.)
    db.query(
        "DEFINE ACCESS OVERWRITE raw_hs ON DATABASE TYPE JWT ALGORITHM HS256 KEY 'a_real_hs_secret' WITH ISSUER KEY 'a_real_hs_secret' DURATION FOR SESSION NONE;",
    )
    .await?
    .check()?;
    let raw_info: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    let raw_info = raw_info.to_sql();
    let start = raw_info
        .find("DEFINE ACCESS raw_hs ")
        .expect("raw_hs in INFO");
    let rest = &raw_info[start..];
    let raw = rest["DEFINE ACCESS ".len()..]
        .find("DEFINE ACCESS")
        .map(|at| &rest[..at + "DEFINE ACCESS ".len()])
        .unwrap_or(rest);
    assert!(
        raw.contains("KEY '[REDACTED]'") && !raw.contains("a_real_"),
        "real keys redact: {raw}"
    );
    Ok(())
}

/// The `table` layer (spec/table.md): the emitted DDL executes
/// through the embedded engine, and the resulting INFO pins the
/// reverse contract:
///
/// - the kind and the schema flag ALWAYS materialize (a bare SDL
///   table INFOs as `TYPE NORMAL SCHEMALESS`, `any` as `TYPE ANY`);
/// - default table permissions are NONE and materialize as the
///   single value;
/// - `FOR delete FULL` is ELIDED from per-op groups while re-parse
///   yields NONE — the preserve rule's data (the SDL wrote
///   `delete: full;` and INFO must not show a delete line);
/// - relation tables materialize their `in`/`out` endpoint fields —
///   the in/out fold's input;
/// - a view INFOs as the fixed `TYPE ANY SCHEMALESS AS SELECT …`;
/// - CHANGEFEED value-normalizes (`30d` → `4w2d`) and DROP renders.
///
/// Exec-class semantics the parse oracle cannot see: RELATE requires
/// a RELATION (or `any`) edge table; `enforced` rejects dangling
/// endpoints at RELATE time, not at define time.
#[tokio::test]
async fn emitted_table_ddl_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
/// Application users
table users schemafull {
  permissions {
    select: full;
    create: full;
    update: $auth.id = id;
    delete: full;
  };
  event audit_email {
    when: $event = 'UPDATE' and $before.email != $after.email;
    then: { create audit_log set at = time::now(); };
  };
};

table any blobby { schemaless; };

// The relation's endpoints and the view's FROM tables — a view
// validates its tables at define time (rule 7, verified), so they
// exist before the definitions that reference them.
table orgs { schemaless; };
table posts { schemaless; };
table comments { schemaless; };

table audit_trail {
  drop;
  schemaless;
  changefeed { expiry: 30d; include original; };
};

table users | orgs -> wrote -> posts | comments { enforced; schemaless; };

/// Posts per author
table view post_stats {
  select author, count() as posts from posts group by author;
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    use surrealdb::types::ToSql;
    let info: surrealdb::types::Value = db.query("INFO FOR DB").await?.take(0)?;
    let info = info.to_sql();

    let segment = |name: &str| -> String {
        let start = info
            .find(&format!("DEFINE TABLE {name} "))
            .unwrap_or_else(|| panic!("`{name}` in INFO: {info}"));
        let rest = &info[start..];
        let end = rest["DEFINE TABLE ".len()..]
            .find("DEFINE TABLE")
            .map(|at| at + "DEFINE TABLE ".len())
            .unwrap_or(rest.len());
        rest[..end].to_string()
    };

    // users: TYPE NORMAL + SCHEMAFULL materialize; per-op permissions
    // render with the ops MERGED by equal value (lowercase, the
    // emitter's grouping — SDL's emission keeps the written groups,
    // the recorded divergence) and the SDL's `delete: full;`
    // VANISHES (the emitter's elision; re-parse yields NONE — never
    // downgrade on retro-apply).
    let users = segment("users");
    assert!(users.contains("TYPE NORMAL SCHEMAFULL"), "{users}");
    assert!(
        users.contains("PERMISSIONS FOR select, create FULL"),
        "{users}"
    );
    assert!(users.contains("FOR update WHERE $auth.id = id"), "{users}");
    assert!(
        !users.to_ascii_lowercase().contains("for delete"),
        "delete-Full elides: {users}"
    );
    // The nested event arrives via INFO FOR TABLE (INFO FOR DB's map
    // carries the table statements only). (A nested INDEX is not
    // exec-probed here; its emission is host-injection-identical to
    // the standalone index, already probed by
    // `emitted_index_ddl_executes`, and the field-first ordering the
    // deferred probe wanted is covered by
    // `emitted_field_ddl_executes`.)
    let users_info: surrealdb::types::Value = db.query("INFO FOR TABLE users").await?.take(0)?;
    let users_info = users_info.to_sql();
    assert!(
        users_info.contains("DEFINE EVENT audit_email ON users"),
        "{users_info}"
    );

    // blobby: the `any` escape hatch keeps TYPE ANY; the bare
    // schemaless flag materializes.
    assert!(segment("blobby").contains("TYPE ANY SCHEMALESS"), "{info}");

    // audit_trail: DROP renders; the changefeed value-normalizes.
    let audit = segment("audit_trail");
    assert!(audit.contains("DROP"), "{audit}");
    assert!(
        audit.contains("CHANGEFEED 4w2d INCLUDE ORIGINAL"),
        "30d spells 4w2d: {audit}"
    );

    // wrote: the full relation shape; the executor materializes the
    // typed endpoint fields (the fold's input — `in`
    // carries the from-list kind).
    assert!(
        segment("wrote").contains("TYPE RELATION IN users | orgs OUT posts | comments ENFORCED"),
        "{info}"
    );
    let wrote_info: surrealdb::types::Value = db.query("INFO FOR TABLE wrote").await?.take(0)?;
    let wrote_info = wrote_info.to_sql();
    assert!(
        wrote_info.contains("DEFINE FIELD in ON wrote TYPE record<"),
        "the in endpoint field materializes: {wrote_info}"
    );
    assert!(
        wrote_info.contains("DEFINE FIELD out ON wrote TYPE record<"),
        "the out endpoint field materializes: {wrote_info}"
    );

    // post_stats: the view's fixed INFO shape (COMMENT rides before
    // AS — the server emitter's own order, the recorded divergence).
    let stats = segment("post_stats");
    assert!(stats.contains("TYPE ANY SCHEMALESS"), "{stats}");
    assert!(
        stats.contains("AS SELECT author, count() AS posts FROM posts GROUP BY author"),
        "{stats}"
    );

    // Exec-class semantics. RELATE requires a relation edge: the
    // NORMAL `users` rejects, the `any` `blobby` accepts.
    let rejected_normal = match db.query("RELATE users:1 -> audit_trail -> posts:1;").await {
        Err(_) => true,
        Ok(response) => response.check().is_err(),
    };
    assert!(rejected_normal, "a NORMAL table rejects RELATE");
    let _ = db.query("CREATE users:1, posts:1;").await?.check()?;
    let accepted_any = match db.query("RELATE users:1 -> blobby -> posts:1;").await {
        Err(_) => false,
        Ok(response) => response.check().is_ok(),
    };
    assert!(accepted_any, "an ANY table accepts RELATE");
    // ENFORCED checks endpoint existence at RELATE time (define time
    // let the dangling shape through).
    let dangling = db.query("RELATE users:1 -> wrote -> posts:99;").await;
    let dangling_rejected = match dangling {
        Err(_) => true,
        Ok(response) => response.check().is_err(),
    };
    assert!(dangling_rejected, "enforced rejects a dangling endpoint");
    let _ = db
        .query("RELATE users:1 -> wrote -> posts:1;")
        .await?
        .check()?;
    Ok(())
}

/// The `field` layer (spec/field.md): the emitted DDL executes
/// through the embedded engine — including a nested index whose host
/// field the same file defines first (the deferred probe:
/// an index over a missing field fails at EXEC, never at parse, and
/// rule 7's field-before-index order is what makes nesting work) —
/// and the resulting INFO pins the reverse contract:
///
/// - PERMISSIONS materializes UNCONDITIONALLY (an unpermissioned
///   field INFOs `PERMISSIONS FULL` — the adopt/elide input);
/// - bare `REFERENCE` reprints as `REFERENCE ON DELETE IGNORE`
///   (never bare — mapped back to the `reference;` flag);
/// - `option<T>` desugars to `none | T`; the differ compares kind
///   ASTs under that normalization;
/// - the server SYNTHESIZES `.*` subfields for inner-kind fields
///   (`matrix: array<array<int>>` → `matrix.*` and `matrix.*.*`,
///   flexible inherited) — the fold-back input;
/// - a typeless computed INFOs typeless (COMPUTED without TYPE).
///
/// Exec-class semantics the parse oracle cannot see: flexible needs
/// a SCHEMAFULL table; the `id` field rejects VALUE; reference needs
/// a top-level record-shaped field; the reference-type predicate is
/// looser than "record" but rejects `record<user> | string`; the
/// ASSERT rejection and the computed/default write paths behave.
#[tokio::test]
async fn emitted_field_ddl_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
table account schemafull {
  name: string;
  email: string {
    assert: string::is_email($value);
    default: 'unknown@example.com';
  };
  secret: string { permissions: none; };
  author: record<user> { reference; };
  matrix: array<array<int>>;
  opt: option<string>;
  shout: string = string::uppercase(name);
  lower = string::lowercase(name);
  index email_idx { fields: email; };
};

table loose { schemaless; };

// The write-path table (account's remaining typed fields would
// demand their own values at write time).
table scratch schemafull {
  name: string;
  note: string { default: 'n/a'; };
  shout: string = string::uppercase(name);
  email: string {
    assert: string::is_email($value);
    default: 'unknown@example.com';
  };
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    // The nested `index email_idx` defines only because its host
    // field `email` lands first in the emitted order (rule 7) — the
    // exec-class check the parse oracle cannot make.
    db.query(&ddl).await?.check()?;

    use surrealdb::types::ToSql;
    let info: surrealdb::types::Value = db.query("INFO FOR TABLE account").await?.take(0)?;
    let info = info.to_sql();

    // PERMISSIONS materializes unconditionally; the unpermissioned
    // `matrix` INFOs the all-FULL single value.
    assert!(info.contains("PERMISSIONS FULL"), "{info}");
    // Bare REFERENCE reprints with its engine default.
    assert!(
        info.contains(
            "DEFINE FIELD author ON account TYPE record<user> REFERENCE ON DELETE IGNORE"
        ),
        "{info}"
    );
    // option<T> desugars to the union.
    assert!(info.contains("TYPE none | string"), "{info}");
    // The synthesized wildcard subfields: deepest-first recursion
    // over inner kinds.
    assert!(info.contains("matrix.*"), "{info}");
    assert!(info.contains("matrix.*.*"), "{info}");
    // The typeless computed INFOs typeless (`lower` was written
    // without a type); the typed one keeps its TYPE — both forms
    // round-trip stable, field.md's Option<Kind> note.
    assert!(
        info.contains("DEFINE FIELD lower ON account COMPUTED string::lowercase(name)"),
        "{info}"
    );
    assert!(
        info.contains("DEFINE FIELD shout ON account TYPE string COMPUTED string::uppercase(name)"),
        "{info}"
    );

    // Write paths: the default fills, the computed applies, the
    // assert rejects.
    let created: surrealdb::types::Value = db
        .query("CREATE scratch SET name = 'Bob';")
        .await?
        .take(0)?;
    let created = created.to_sql();
    assert!(
        created.contains("note: 'n/a'"),
        "the default fills: {created}"
    );
    assert!(created.contains("shout: 'BOB'"), "{created}");
    assert!(
        created.contains("email: 'unknown@example.com'"),
        "{created}"
    );
    // The rejection rides the per-statement result (the request
    // itself succeeds — the engine reports the failed CREATE inside).
    let mut response = db
        .query("CREATE scratch SET name = 'Eve', email = 'nope';")
        .await
        .expect("the request itself succeeds");
    let taken: Result<surrealdb::types::Value, _> = response.take(0);
    assert!(taken.is_err(), "the assert must reject: {taken:?}");

    // Exec-class define rejections.
    // flexible needs a SCHEMAFULL table.
    let flexible_on_schemaless =
        surform::parser::parse_file("field m: object { on: loose; flexible; };");
    let (file, diags) = flexible_on_schemaless;
    assert!(diags.is_empty(), "{diags:?}");
    let rejected = db.query(surform::codegen::emit(&file)).await?.check();
    assert!(rejected.is_err(), "flexible on schemaless must reject");
    // The `id` field rejects VALUE at define time — SDL eagerly.
    let (_, diags) =
        surform::parser::parse_file("field id: record<user> { on: account; value: 1; };");
    assert!(
        diags
            .iter()
            .any(|d| matches!(d, surform::diag::Diag::InvalidFieldId { .. })),
        "SDL rejects id+VALUE eagerly: {diags:?}"
    );
    // (the engine's own rejection of the shape SDL never emits)
    let rejected = db
        .query("DEFINE FIELD OVERWRITE id ON loose VALUE 1;")
        .await?
        .check();
    assert!(rejected.is_err(), "id+VALUE must reject at define");
    // reference on a nested path (SDL rejects eagerly; the engine's
    // message is the spec's ground truth).
    let rejected = db
        .query("DEFINE FIELD OVERWRITE a.b ON loose TYPE option<record<user>> REFERENCE;")
        .await?
        .check();
    assert!(rejected.is_err(), "reference on a nested path must reject");
    // The reference-type predicate: a non-record field rejects, the
    // every-arm-record union passes, mixed arms reject.
    let rejected = db
        .query("DEFINE FIELD OVERWRITE stray_ref ON loose TYPE string REFERENCE ON DELETE IGNORE;")
        .await?
        .check();
    assert!(rejected.is_err(), "reference on a string field must reject");
    let ok = db
        .query("DEFINE FIELD OVERWRITE refs ON loose TYPE record<user> | none REFERENCE ON DELETE IGNORE;")
        .await?
        .check();
    assert!(ok.is_ok(), "record|none reference must define: {ok:?}");
    let rejected = db
        .query("DEFINE FIELD OVERWRITE mixed_ref ON loose TYPE record<user> | string REFERENCE ON DELETE IGNORE;")
        .await?
        .check();
    assert!(rejected.is_err(), "record|string reference must reject");
    // An index over a missing field fails at EXEC, not parse — the
    // class the ordering discipline above guards. (On the schemaless
    // `loose` the define even succeeds — the SCHEMAFULL table is
    // where the catalog rejects it.)
    let rejected = db
        .query("DEFINE INDEX OVERWRITE ghost_idx ON account COLUMNS ghost;")
        .await?
        .check();
    assert!(rejected.is_err(), "index over a missing field must reject");
    Ok(())
}

/// The raw statement's DDL executes verbatim — the raw-defined
/// param is live in the catalog, proving the passthrough reached the
/// engine unharmed (comments and interior spelling included).
#[tokio::test]
async fn emitted_raw_ddl_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
raw {
  // interior comments ride along
  DEFINE PARAM OVERWRITE $smoke VALUE 42;
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    let value: surrealdb::types::Value = db.query("RETURN $smoke").await?.take(0)?;
    use surrealdb::types::ToSql;
    assert_eq!(value.to_sql(), "42");
    Ok(())
}

/// The mixin expansion's DDL executes and the members are live fields
/// on every spreading table (spec/mixin.md): one mixin, two tables,
/// the overshadowing explicit field winning on its table.
#[tokio::test]
async fn emitted_mixin_expansion_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
mixin audited {
  created_at: datetime { default: time::now(); };
  updated_at: datetime { default: time::now(); };
};

table posts schemafull {
  ...audited;
  title: string;
};

table invoices schemafull {
  ...audited;
  created_at: datetime;
  amount: decimal;
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    // Both tables carry the member — and the explicit field's
    // definition WHOLLY won on invoices: no DEFAULT there (the
    // member's default is gone with the member, the overshadow rule —
    // a clause merge would have kept it).
    use surrealdb::types::ToSql;
    for table in ["posts", "invoices"] {
        let info: surrealdb::types::Value = db
            .query(format!("INFO FOR TABLE {table}").as_str())
            .await?
            .take(0)?;
        let info = info.to_sql();
        assert!(info.contains("created_at"), "{table}: {info}");
    }
    let posts: surrealdb::types::Value = db.query("INFO FOR TABLE posts").await?.take(0)?;
    assert!(
        posts
            .to_sql()
            .contains("created_at ON posts TYPE datetime DEFAULT"),
        "{}",
        posts.to_sql()
    );
    let invoices: surrealdb::types::Value = db.query("INFO FOR TABLE invoices").await?.take(0)?;
    let invoices_sql = invoices.to_sql();
    assert!(
        !invoices_sql.contains("created_at ON invoices TYPE datetime DEFAULT"),
        "the overshadow dropped the member's default: {invoices_sql}"
    );
    // The member's default is live where the spread defines it.
    db.query("CREATE posts CONTENT { title: 'first' };")
        .await?
        .check()?;
    let titles: Vec<String> = db.query("SELECT VALUE title FROM posts").await?.take(0)?;
    assert_eq!(titles, ["first"]);
    Ok(())
}

/// The full member model executes (spec/mixin.md): index and
/// event members define on the spreading table at exec, and a
/// collision between two spreads resolves last-occurrence-wins —
/// the engine holds the LATER spread's index (the loser never
/// executed at all, so no CONCURRENTLY side rides anywhere).
#[tokio::test]
async fn emitted_mixin_member_kinds_execute_last_wins() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
mixin base {
  slug: string;
  index slug_u { fields: slug; concurrently; };
  event touched { then: 1; };
};

mixin alt {
  index slug_u { fields: slug; };
};

table posts schemafull {
  ...base;
  ...alt;
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    let ddl = surform::codegen::emit(&file);

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;

    // The catalog holds all three member kinds on the spreading
    // table — and the later spread's plain index won the collision.
    let info: surrealdb::types::Value = db.query("INFO FOR TABLE posts").await?.take(0)?;
    use surrealdb::types::ToSql;
    let info = info.to_sql();
    assert!(info.contains("slug_u"), "the index member: {info}");
    assert!(info.contains("touched"), "the event member: {info}");
    assert!(
        !info.contains("CONCURRENTLY"),
        "the loser (base's concurrently index) never executed: {info}"
    );
    Ok(())
}

/// The canonical layout is define-order safe: a spread carrying an
/// index that names an EXPLICIT field of
/// the host must still build after fmt — the fields group prints
/// before the spreads, so the formatted file's DDL defines the field
/// first. The fixture holds every safe crossing the layout performs:
/// (a) a spread's index naming an explicit field,
/// (b) an explicit index naming a spread-supplied field, (c) one
/// spread's index naming an EARLIER spread's field (relative spread
/// order preserved). Executed, not parsed: the engine's schemafull
/// field-existence check is a define-time catalog rejection the
/// parse oracle cannot see.
#[tokio::test]
async fn fmtted_spread_index_crossings_execute() -> Result<(), Box<dyn std::error::Error>> {
    let sdl = "\
mixin idx {
  index by_email { fields: email; };
};

mixin person {
  name: string;
};

mixin late {
  index by_name { fields: name; };
};

table t schemafull {
  email: string;
  ...idx;
  ...person;
  ...late;
  index by_both { fields: email, name; };
};
";
    let (file, diags) = surform::parser::parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    // fmt's output re-parses; its DDL is what must execute.
    let formatted = surform::info::write::write_canonical(&file);
    let (refmt, diags) = surform::parser::parse_file(&formatted);
    assert!(diags.is_empty(), "fmt output parses: {diags:?}");
    let ddl = surform::codegen::emit(&refmt);
    for (field, index) in [
        ("email", "by_email"),
        ("name", "by_name"),
        ("name", "by_both"),
        ("email", "by_both"),
    ] {
        let at = |needle: &str| {
            ddl.find(needle)
                .unwrap_or_else(|| panic!("{needle} missing: {ddl}"))
        };
        assert!(
            at(&format!("DEFINE FIELD OVERWRITE {field}"))
                < at(&format!("DEFINE INDEX OVERWRITE {index}")),
            "{index} must define after {field}: {ddl}"
        );
    }

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("sdl_test").use_db("scratch").await?;
    db.query(&ddl).await?.check()?;
    Ok(())
}
