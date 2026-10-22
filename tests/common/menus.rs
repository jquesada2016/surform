//! The combination oracle's clause menus: ONE home
//! for every statement family's cross-product generator. The
//! per-commit pair suite (`tests/oracle_smoke.rs`) consumes the docs
//! for its parse-oracle legs, the `#[ignore]` full suite
//! (`tests/oracle_full.rs`) adds the end-to-end reverse fixed point
//! (embedded exec → INFO → read → merge convergence), and the audit
//! suite (`tests/oracle_audit.rs`) pins the vacuity guard (exact
//! product counts, distinct docs, terminal coverage).
//!
//! The generators moved here verbatim from the pair suite's inline
//! loops — every count assertion travels with its generator, so a
//! menu edit that drops a terminal breaks the audit before anything
//! else. `Family` carries what only the full suite needs: the exec
//! prerequisites (the tables/functions/analyzers a family's docs
//! reference — applied on the fresh engine first) and the
//! exec-skip markers (docs whose targets the embedded engine cannot
//! define — the surrealism WASM gate's `mod::`).

/// One statement family's menu.
pub struct Family {
    pub name: &'static str,
    /// The full clause cross product, one SDL document each.
    pub docs: fn() -> Vec<String>,
    /// The exact product size (the vacuity guard's pin).
    pub expected_count: usize,
    /// The SDL prepended to every doc, carried in the doc's file AND
    /// executed with it (the scope statements that bind the doc and
    /// the tables/functions/analyzers its references target — the
    /// file and the engine must hold the same definitions or the
    /// merge adopts the difference).
    pub setup: &'static str,
    /// Substring markers for docs the embedded engine cannot execute
    /// (module-adjacent capability gates); each skips with its
    /// reason recorded.
    pub exec_skip: &'static [(&'static str, &'static str)],
    /// The exact count of docs the exec leg runs (the vacuity
    /// guard's second pin — a marker edit that silently grows the
    /// skip set breaks it).
    pub expected_execd: usize,
}

impl Family {
    /// Whether this doc runs the exec leg. Each marker is an
    /// eligibility rule: a plain marker allows docs NOT containing
    /// it (the skip set); a `!`-prefixed marker allows only docs
    /// that DO contain the rest (an absence skip — e.g. a bucket
    /// with no backend clause).
    pub fn execs(&self, sdl: &str) -> bool {
        self.exec_skip.iter().all(|(marker, _)| {
            if let Some(rest) = marker.strip_prefix('!') {
                sdl.contains(rest)
            } else {
                !sdl.contains(marker)
            }
        })
    }
}

/// Every family's menu, in spec-file order.
pub fn families() -> Vec<Family> {
    vec![
        Family {
            name: "sequence",
            docs: sequence_docs,
            expected_count: 4 * 3 * 4,
            setup: "namespace test;\ndatabase scratch;\n",
            // START/BATCH/TIMEOUT are EVALUATED at define and INFO
            // stores the evaluated state (the corpus's literal-only
            // rule): the non-literal terminals are parse-coverage
            // spice the exec+convergence layer cannot consume —
            // `time::now()` is a datetime (a duration cast error at
            // exec), and every expression's evaluation replaces the
            // authored spelling in the merge.
            exec_skip: &[
                (
                    "time::now()",
                    "TIMEOUT evaluates it — the datetime cannot cast to duration",
                ),
                ("rand()", "BATCH stores the evaluated number"),
                ("1 + 1", "START stores the evaluated number"),
                ("{ 5s }", "TIMEOUT stores the evaluated duration"),
            ],
            expected_execd: 12,
        },
        Family {
            name: "param",
            docs: param_docs,
            expected_count: 5 * 4,
            setup: "namespace test;\ndatabase scratch;\n",
            // VALUE is evaluated at define (the corpus's literal-only
            // rule); the PERMISSIONS expression is stored verbatim
            // and rides the exec leg.
            exec_skip: &[
                ("string::split", "VALUE stores the evaluated state"),
                ("if $x", "VALUE evaluates it"),
            ],
            expected_execd: 12,
        },
        Family {
            name: "database",
            docs: database_docs,
            expected_count: 2 * 3 * 2,
            // The session's `USE NS test DB scratch` implicitly
            // creates that database on a fresh engine, so the setup
            // must DECLARE it — otherwise it surfaces as an adoptee
            // (and the docs' `database db` rides alongside as the
            // second database of the file).
            setup: "namespace test;\ndatabase scratch;\n",
            exec_skip: &[],
            expected_execd: 12,
        },
        Family {
            name: "user",
            docs: user_docs,
            expected_count: 6 * 3 * 2 * 3 * 4,
            setup: "namespace test;\ndatabase scratch;\n",
            exec_skip: &[],
            expected_execd: 432,
        },
        Family {
            name: "analyzer",
            docs: analyzer_docs,
            expected_count: 4 * 4 * 4,
            setup: "\
                namespace test;\n\
                database scratch;\n\
                fn strip_markup($x: string) { return $x; };\n\
                fn util::class($x: string) { return $x; };\n",
            exec_skip: &[
                ("mod::", "the surrealism WASM gate"),
                (
                    "mapper(",
                    "the runtime file allowlist denies the path at define",
                ),
            ],
            expected_execd: 36,
        },
        Family {
            name: "bucket",
            docs: bucket_docs,
            expected_count: 4 * 2 * 4,
            setup: "namespace test;\ndatabase scratch;\n",
            exec_skip: &[
                (
                    "string::lowercase",
                    "the backend stores the evaluated string",
                ),
                (
                    "'file:",
                    "the file backend needs a server-configured global bucket",
                ),
                (
                    "!backend:",
                    "the absent backend is the engine's default — it needs a \
                     server-configured global bucket",
                ),
            ],
            expected_execd: 8,
        },
        Family {
            name: "event",
            docs: event_docs,
            expected_count: 3 * 6 * 5,
            setup: "namespace test;\ndatabase scratch;\ntable `my table`;\n",
            exec_skip: &[],
            expected_execd: 90,
        },
        Family {
            name: "function",
            docs: function_docs,
            expected_count: 4 * 4 * 4,
            setup: "namespace test;\ndatabase scratch;\ntable log;\n",
            exec_skip: &[],
            expected_execd: 64,
        },
        Family {
            name: "config api",
            docs: config_api_docs,
            expected_count: 4 * 4,
            setup: "namespace test;\ndatabase scratch;\n",
            exec_skip: &[
                ("mod::", "the surrealism WASM gate"),
                (
                    "math::round",
                    "the middleware expression evaluates at define — the engine \
                     checks math::round's arity",
                ),
            ],
            expected_execd: 12,
        },
        Family {
            name: "config graphql",
            docs: config_graphql_docs,
            expected_count: 5 * 4 + 4 + 3 * 2 * 3 - 1,
            setup: "namespace test;\ndatabase scratch;\n",
            exec_skip: &[],
            expected_execd: 41,
        },
        Family {
            name: "config default",
            docs: config_default_docs,
            expected_count: 4 * 4 - 1,
            setup: "namespace test;\ndatabase scratch;\n",
            exec_skip: &[("string::uppercase", "the sides store the evaluated string")],
            expected_execd: 8,
        },
        Family {
            name: "index",
            docs: index_docs,
            expected_count: 4 * 2 * 2 + 3 * 2 + 3 * 3 * 2 + 6 * 3 * 3 * 2 * 4 + 3 * 3 * 3 * 2,
            setup: "\
                namespace test;\n\
                database scratch;\n\
                analyzer english { tokenizers: class; };\n\
                table t {\n\
                  field email: string;\n\
                  field account: string;\n\
                  field thread_id: string;\n\
                  field `index`: string;\n\
                  field a: array<{ b: int }>;\n\
                  field nested: record<nested>;\n\
                  field body: string;\n\
                  field vec: array<float>;\n\
                  field status: string;\n\
                  field n: int;\n\
                };\n\
                table nested;\n",
            exec_skip: &[(
                "->nested",
                "the graph-edge index field is define-rejected (re-probed \
                 live on 3.3.0: \"Expected `field name` but found `NONE`\" \
                 — parse-legal; the pair oracle's acceptance stands; \
                 a known corner)",
            )],
            expected_execd: 522,
        },
        Family {
            name: "api",
            docs: api_docs,
            expected_count: 3 * 4 * 4 + 4 * 4 * 2,
            setup: "namespace test;\ndatabase scratch;\ntable log;\n",
            exec_skip: &[("mod::", "the surrealism WASM gate")],
            expected_execd: 60,
        },
        Family {
            name: "access",
            docs: access_docs,
            expected_count: 4 * 3 * 4 * 4 + 9 + 5 * 4 * 2 * 2 * 2 + 6 * 2 * 3 + 3 + 2 + 3,
            setup: "namespace test;\ndatabase scratch;\ntable user;\n",
            exec_skip: &[],
            expected_execd: 405,
        },
        Family {
            name: "table",
            docs: table_docs,
            // bare/any 2*4*2*3*6*2; relations: the fully-sided header
            // carries the LIGHTWEIGHT dimension (2*2*2*4 = 32), the
            // other four 1*2*2*4 = 16 each (the INLINE caps ride every
            // relation doc); nested 4; views 3*4.
            expected_count: 2 * 4 * 2 * 3 * 6 * 2 + (2 * 2 * 2 * 4 + 4 * 2 * 2 * 4) + 4 + 3 * 4,
            setup: "\
                namespace test;\n\
                database scratch;\n\
                table u1;\n\
                table u2;\n\
                table p1;\n\
                table p2;\n\
                table f1;\n\
                table post { field author: string; field active: bool; };\n\
                table log;\n\
                analyzer english { tokenizers: class; };\n",
            exec_skip: &[],
            expected_execd: 688,
        },
        Family {
            name: "field",
            docs: field_docs,
            expected_count: 11 * 3 * 3 * 2 * 5 * 2 + 4 * 2 + 6 + 6 + 5 + 4 * 5 + 1,
            // The flexible docs demand a SCHEMAFULL host, so `t_x` is
            // schemafull (the schema flag is per-table, never
            // per-field); the computed/nested docs define their own
            // hosts (`c_x`, `n_x`) inline; the INLINE docs ride the
            // relation hosts (the engine's define-time demand).
            setup: "\
                namespace test;\n\
                database scratch;\n\
                table t_x schemafull;\n\
                table person;\n\
                table user;\n\
                table person -> likes -> person;\n\
                table person -> sf_likes -> person { schemafull; };\n",
            exec_skip: &[(
                " reference;",
                "the engine rejects REFERENCE on non-record kinds at define — \
                 SDL-L0016's advisory domain (SDL accepts \
                 per parse parity). Coarse by design: this also skips the \
                 engine-legal record-kind bare-flag docs (the on-delete \
                 action docs cover the round trip).",
            )],
            expected_execd: 1035,
        },
        Family {
            name: "mixin",
            docs: mixin_docs,
            expected_count: 3 * 2 * 3 * 2,
            setup: "namespace test;\ndatabase scratch;\n",
            exec_skip: &[],
            expected_execd: 36,
        },
    ]
}

// ---------------------------------------------------------------------
// The generators (moved verbatim from the pair suite's loops)
// ---------------------------------------------------------------------

pub fn sequence_docs() -> Vec<String> {
    let starts: [Option<&str>; 4] = [None, Some("0"), Some("5000"), Some("1 + 1")];
    let batches: [Option<&str>; 3] = [None, Some("1000"), Some("rand() * 2")];
    let timeouts: [Option<&str>; 4] = [None, Some("5s"), Some("time::now()"), Some("{ 5s }")];
    let mut docs = Vec::new();
    for start in starts {
        for batch in batches {
            for timeout in timeouts {
                let mut sdl = String::from("sequence n {");
                if let Some(v) = batch {
                    sdl.push_str(&format!(" batch: {v};"));
                }
                if let Some(v) = start {
                    sdl.push_str(&format!(" start: {v};"));
                }
                if let Some(v) = timeout {
                    sdl.push_str(&format!(" timeout: {v};"));
                }
                sdl.push_str(" };");
                docs.push(sdl);
            }
        }
    }
    docs
}

pub fn param_docs() -> Vec<String> {
    let values: [Option<&str>; 5] = [
        None,
        Some("50"),
        Some("string::split('1.2.3', '.')[0]"),
        Some("{ read: '/api/read', write: '/api/write' }"),
        Some("if $x { 1 } else { 2 }"),
    ];
    let permissions: [Option<&str>; 4] = [
        None,
        Some("none"),
        Some("FULL"),
        Some("$user.role == 'admin'"),
    ];
    let mut docs = Vec::new();
    for value in values {
        for permission in permissions {
            let mut sdl = String::from("param $x {");
            if let Some(v) = value {
                sdl.push_str(&format!(" value: {v};"));
            }
            if let Some(p) = permission {
                sdl.push_str(&format!(" permissions: {p};"));
            }
            sdl.push_str(" };");
            docs.push(sdl);
        }
    }
    docs
}

pub fn database_docs() -> Vec<String> {
    let stricts = [false, true];
    let changefeeds: [Option<(&str, bool)>; 3] = [None, Some(("7d", false)), Some(("1h30m", true))];
    let comments: [Option<&str>; 2] = [None, Some("Primary application database")];
    let mut docs = Vec::new();
    for &strict in &stricts {
        for changefeed in changefeeds {
            for comment in comments {
                let mut sdl = String::new();
                if let Some(line) = comment {
                    sdl.push_str(&format!("/// {line}\n"));
                }
                sdl.push_str("database db {");
                if strict {
                    sdl.push_str(" strict;");
                }
                if let Some((expiry, include_original)) = changefeed {
                    sdl.push_str(&format!(" changefeed {{ expiry: {expiry};"));
                    if include_original {
                        sdl.push_str(" include original;");
                    }
                    sdl.push_str(" };");
                }
                sdl.push_str(" };");
                docs.push(sdl);
            }
        }
    }
    docs
}

/// The upstream known-answer SCRAM verifier (surrealdb-iam's own KAT:
/// password `pencil`, salt `0123456789abcdef`, 4096 iterations) — the
/// same value upstream's `scram.surql` language test imports. Valid
/// per the engine's parse-time verifier check, so every doc carrying
/// it both parses and execs; single-quote-safe by construction
/// (base64 + `$`/`:` only). The PASSSCRAM clause validates at DDL
/// parse, so menus must never carry a hand-bad verifier.
pub const SCRAM_KAT: &str = "SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=";

pub fn user_docs() -> Vec<String> {
    let levels = [
        ("root", "ROOT"),
        ("namespace", "NAMESPACE"),
        ("database", "DATABASE"),
        ("kv", "ROOT"),
        ("ns", "NAMESPACE"),
        ("db", "DATABASE"),
    ];
    let passes: [Option<(&str, &str)>; 3] = [
        None,
        Some(("password", "'change-me'")),
        Some(("passhash", "'$argon2id$'")),
    ];
    let scrams: [Option<&str>; 2] = [None, Some(SCRAM_KAT)];
    let roles: [Option<&str>; 3] = [None, Some("owner"), Some("editor, viewer")];
    let durations: [Option<&str>; 4] = [
        None,
        Some("token: 1h;"),
        Some("session: 12h;"),
        Some("token: 1h; session: 12h;"),
    ];
    let mut docs = Vec::new();
    for (spelling, _) in levels {
        for pass in passes {
            for scram in scrams {
                for role in roles {
                    for duration in durations {
                        let mut sdl = format!("user u {{ on: {spelling};");
                        if let Some((clause, literal)) = pass {
                            sdl.push_str(&format!(" {clause}: {literal};"));
                        }
                        if let Some(verifier) = scram {
                            sdl.push_str(&format!(" passscram: '{verifier}';"));
                        }
                        if let Some(r) = role {
                            sdl.push_str(&format!(" roles: {r};"));
                        }
                        if let Some(duration_clause) = duration {
                            sdl.push_str(&format!(" duration {{ {duration_clause} }};"));
                        }
                        sdl.push_str(" };");
                        docs.push(sdl);
                    }
                }
            }
        }
    }
    docs
}

pub fn analyzer_docs() -> Vec<String> {
    let functions: [Option<&str>; 4] = [
        None,
        Some("fn::strip_markup"),
        Some("fn::util::class"),
        Some("mod::demo::some"),
    ];
    let tokenizers: [Option<Vec<&str>>; 4] = [
        None,
        Some(vec!["class"]),
        Some(vec!["blank", "punct"]),
        Some(vec!["camel", "class", "blank"]),
    ];
    let filters: [Option<Vec<&str>>; 4] = [
        None,
        Some(vec!["lowercase"]),
        Some(vec!["snowball(en)", "lowercase"]),
        Some(vec!["ascii", "edgengram(2, 10)", "mapper('/tmp/x.txt')"]),
    ];
    let mut docs = Vec::new();
    for function in functions {
        for tokenizers in &tokenizers {
            for filters in &filters {
                let mut sdl = String::from("analyzer a {");
                if let Some(path) = function {
                    sdl.push_str(&format!(" function: {path};"));
                }
                if let Some(list) = tokenizers {
                    sdl.push_str(" tokenizers: ");
                    sdl.push_str(&list.join(", "));
                    sdl.push(';');
                }
                if let Some(list) = filters {
                    sdl.push_str(" filters: ");
                    sdl.push_str(&list.join(", "));
                    sdl.push(';');
                }
                sdl.push_str(" };");
                docs.push(sdl);
            }
        }
    }
    docs
}

pub fn bucket_docs() -> Vec<String> {
    let backends: [Option<&str>; 4] = [
        None,
        Some("'memory'"),
        Some("'file:/srv/uploads'"),
        Some("string::lowercase('MEMORY')"),
    ];
    let readonlies = [false, true];
    let permissions: [Option<&str>; 4] =
        [None, Some("none"), Some("full"), Some("$auth.admin = true")];
    let mut docs = Vec::new();
    for backend in backends {
        for &readonly in &readonlies {
            for permission in permissions {
                let mut sdl = String::from("bucket b {");
                if let Some(v) = backend {
                    sdl.push_str(&format!(" backend: {v};"));
                }
                if readonly {
                    sdl.push_str(" readonly;");
                }
                if let Some(p) = permission {
                    sdl.push_str(&format!(" permissions: {p};"));
                }
                sdl.push_str(" };");
                docs.push(sdl);
            }
        }
    }
    docs
}

pub fn event_docs() -> Vec<String> {
    let whens: [Option<&str>; 3] = [None, Some("$event = 'CREATE'"), Some("time::now() > d.at")];
    let thens: [&str; 6] = [
        "(create log set at = time::now())",
        "{ if $event = 'CREATE' { throw 'nope'; } }",
        "{ create log set a = 1; update b set c = 2; }",
        "(create a set x = 1), (update b set y = y + 1)",
        // The printer's bare word-led THEN shapes (CoverStmts prints
        // value-shaped items unparenthesized) — the read loop's
        // word-led leg.
        "fn::audit::write($event, $after.id ?? $before.id)",
        "throw 'boom'",
    ];
    let asyncts: [Option<&str>; 5] = [
        None,
        Some("async;"),
        Some("async { retry: 3; };"),
        Some("async { maxdepth: 7; };"),
        Some("async { retry: 1; maxdepth: 9; };"),
    ];
    let mut docs = Vec::new();
    for when in whens {
        for then in thens {
            for async_clause in asyncts {
                let mut sdl = String::from("event e { on: `my table`;");
                if let Some(w) = when {
                    sdl.push_str(&format!(" when: {w};"));
                }
                if let Some(a) = async_clause {
                    sdl.push_str(&format!(" {a}"));
                }
                sdl.push_str(&format!(" then: {then};"));
                sdl.push_str(" };");
                docs.push(sdl);
            }
        }
    }
    docs
}

pub fn function_docs() -> Vec<String> {
    let permissions: [Option<&str>; 4] = [
        None,
        Some("permissions: none;"),
        Some("permissions: full;"),
        Some("permissions: $auth.admin = true;"),
    ];
    let graphqls: [Option<&str>; 4] = [
        None,
        Some("graphql { alias: 'f'; };"),
        Some("graphql { deprecated: 'old'; };"),
        Some("graphql { alias: 'f'; deprecated: 'old'; };"),
    ];
    let bodies: [&str; 4] = [
        "{ 'pong' }",
        "{ return $x ?? 1; }",
        "{ let $r = math::round($x, 2); $r + 1; }",
        "{ create log set at = time::now(); }",
    ];
    let mut docs = Vec::new();
    for permission in permissions {
        for graphql in graphqls {
            for body in bodies {
                let mut sdl = String::from("/// doc\nfn combo($x: option<number>) -> number ");
                sdl.push_str(body);
                let mut clauses = String::new();
                if let Some(p) = permission {
                    clauses.push_str(&format!(" {p}"));
                }
                if let Some(g) = graphql {
                    clauses.push_str(&format!(" {g}"));
                }
                if !clauses.is_empty() {
                    sdl.push_str(&format!(" {{{clauses}"));
                    sdl.push_str(" };");
                } else {
                    sdl.push(';');
                }
                docs.push(sdl);
            }
        }
    }
    docs
}

pub fn config_api_docs() -> Vec<String> {
    let middlewares: [Option<&str>; 4] = [
        None,
        Some("middleware: timeout();"),
        Some("middleware: api::timeout(5s), api::log();"),
        Some("middleware: fn::util::guard(math::round(1.55, 1), 'a, b', $x);"),
    ];
    let permissions: [Option<&str>; 4] = [
        None,
        Some("permissions: none;"),
        Some("permissions: full;"),
        Some("permissions: $auth.admin = true;"),
    ];
    let mut docs = Vec::new();
    for middleware in middlewares {
        for permission in permissions {
            let mut sdl = String::from("config api {");
            if let Some(m) = middleware {
                sdl.push_str(&format!(" {m}"));
            }
            if let Some(p) = permission {
                sdl.push_str(&format!(" {p}"));
            }
            sdl.push_str(" };");
            docs.push(sdl);
        }
    }
    docs
}

pub fn config_graphql_docs() -> Vec<String> {
    let tables: [&str; 5] = [
        "",
        " tables: auto;",
        " tables: none;",
        " tables { include: user, `order`; };",
        " tables { exclude: `table`; };",
    ];
    let functions: [&str; 4] = [
        "",
        " functions: auto;",
        " functions { include: fn::a::b, fn::c; };",
        " functions { exclude: fn::legacy_report; };",
    ];
    let mut docs = Vec::new();
    for table in tables {
        for function in functions {
            docs.push(format!("config graphql {{{table}{function} }};"));
        }
    }
    // The flag forms fold over the families.
    for flag in ["auto;", "none;"] {
        for family in ["tables: none;", "functions: auto;"] {
            docs.push(format!("config graphql {{ {flag} {family} }};"));
        }
    }
    // The limits trio (the all-empty combo is the family product's
    // empty doc already — the audit's distinctness pin).
    for depth in ["", " depth: 4;", " depth: 1_000;"] {
        for complexity in ["", " complexity: 100;"] {
            for introspection in ["", " introspection: none;", " introspection: auto;"] {
                if depth.is_empty() && complexity.is_empty() && introspection.is_empty() {
                    continue;
                }
                docs.push(format!(
                    "config graphql {{{depth}{complexity}{introspection} }};"
                ));
            }
        }
    }
    docs
}

pub fn config_default_docs() -> Vec<String> {
    let sides: [Option<&str>; 4] = [
        None,
        Some("'acme'"),
        Some("none"),
        Some("string::uppercase('acme')"),
    ];
    let mut docs = Vec::new();
    for namespace in sides {
        for database in sides {
            if namespace.is_none() && database.is_none() {
                continue; // ≥ 1 side required (parse-enforced)
            }
            let mut sdl = String::from("config default {");
            if let Some(n) = namespace {
                sdl.push_str(&format!(" namespace: {n};"));
            }
            if let Some(d) = database {
                sdl.push_str(&format!(" database: {d};"));
            }
            sdl.push_str(" };");
            docs.push(sdl);
        }
    }
    docs
}

pub fn index_docs() -> Vec<String> {
    let mut docs = Vec::new();

    // plain / unique: the shared field vocabulary.
    let scalar_fields: [&str; 4] = [
        "fields: email;",
        "columns: email;",
        "fields: account, thread_id;",
        "fields: `index`, a[0].b, ->nested;",
    ];
    for fields in scalar_fields {
        for concurrently in ["", " concurrently;"] {
            for kind in ["", "unique"] {
                docs.push(format!(
                    "index {kind} idx {{ on: t;{fields}{concurrently} }};"
                ));
            }
        }
    }

    // count: where × concurrently.
    for where_ in ["", " where: status = 'active';", " where: n > 1_000;"] {
        for concurrently in ["", " concurrently;"] {
            docs.push(format!("index count c {{ on: t;{where_}{concurrently} }};"));
        }
    }

    // fulltext: analyzer × bm25 × highlights.
    for analyzer in ["", " analyzer: english;", " analyzer: like;"] {
        for bm25 in ["", " bm25: (1.2, 0.75);", " bm25: (2, 0);"] {
            for highlights in ["", " highlights;"] {
                docs.push(format!(
                    "index fulltext f {{ on: t; fields: body;{analyzer}{bm25}{highlights} }};"
                ));
            }
        }
    }

    // hnsw: distance × type × m × efc × derived.
    let hnsw_distances = [
        "",
        " distance: euclidean;",
        " distance: cosine;",
        " distance: minkowski(3);",
        " distance: inner_product;",
        " distance: hamming;",
    ];
    for distance in hnsw_distances {
        for type_ in ["", " type: f16;", " type: i8;"] {
            for m in ["", " m: 16;", " m: 127;"] {
                for efc in ["", " efc: 200;"] {
                    for derived in ["", " m0: 32;", " lm: 0.5;", " m0: 32; lm: 1.5e-1;"] {
                        docs.push(format!(
                            "index hnsw v {{ on: t; fields: vec; dimension: 768;\
                             {distance}{type_}{m}{efc}{derived} }};"
                        ));
                    }
                }
            }
        }
    }

    // diskann: the unrestricted-legal combos.
    for distance in ["", " distance: cosine;", " distance: inner_product;"] {
        for type_ in ["", " type: f16;", " type: u8;"] {
            for degree in ["", " degree: 32;", " degree: 1;"] {
                for alpha in ["", " alpha: 1.3;"] {
                    docs.push(format!(
                        "index diskann v {{ on: t; fields: vec; dimension: 1536;\
                         {distance}{type_}{degree}{alpha} }};"
                    ));
                }
            }
        }
    }
    docs
}

pub fn api_docs() -> Vec<String> {
    let mut docs = Vec::new();
    let permissions = [
        "",
        " permissions: none;",
        " permissions: full;",
        " permissions: $auth.admin = true;",
    ];
    let middleware = [
        "",
        " middleware: api::timeout(5s);",
        " middleware: api::guard(), fn::audit('write');",
        " middleware: mod::pkg::check(1000);",
    ];

    // Method groups. The path uses the engine's dynamic-segment
    // grammar (`:id`) — SDL passes path literals through for the
    // server to validate at define, and the full suite execs.
    for head in ["get", "post, put", "delete, patch, trace"] {
        for permission in permissions {
            for mw in middleware {
                docs.push(format!(
                    "api '/x/:id' {{ {head} {{{permission}{mw} then: \
                     {{ {{ status: 200, body: {{ ok: true }} }} }}; }}; }};"
                ));
            }
        }
    }

    // The `any` block: config clauses × fallback presence.
    for permission in permissions {
        for mw in middleware {
            for fallback in ["", " then: (create log set at = time::now());"] {
                docs.push(format!(
                    "api '/a' {{ any {{{permission}{mw}{fallback} }}; }};"
                ));
            }
        }
    }
    docs
}

pub fn access_docs() -> Vec<String> {
    let mut docs = Vec::new();

    // The jwt variant.
    let sources = [
        (" url: 'https://x/.well-known/jwks.json';", " rs256"),
        (" algorithm: hs256; key: '[REDACTED]';", " hs256"),
        (" algorithm: es256; key: 'k';", " es256"),
        (" algorithm: eddsa; key: 'k';", " eddsa"),
    ];
    let issuers = |alg: &str| -> [String; 3] {
        [
            String::new(),
            " issuer { key: '[REDACTED]'; };".to_string(),
            format!(" issuer {{ algorithm:{alg}; key: '[REDACTED]'; }};"),
        ]
    };
    let durations = [
        "",
        " duration { session: 8h; };",
        " duration { grant: 30d; token: 1d; };",
        " duration { grant: 1h; token: 1h; session: none; };",
    ];
    for (source, issuer_alg) in sources {
        for issuer in issuers(issuer_alg) {
            for duration in durations {
                for level in ["root", "kv", "ns", "database"] {
                    docs.push(format!(
                        "access jwt_x {{ on: {level}; type: jwt;{source}{issuer}{duration} }};"
                    ));
                }
            }
        }
    }

    // The authenticate clause rides any variant.
    for (variant, extra) in [
        ("jwt", " url: 'https://x/jwks';"),
        ("record", ""),
        ("bearer", " for: user;"),
    ] {
        for authenticate in [
            "",
            " authenticate: $auth.active;",
            " authenticate: { if $auth.blocked { throw 'suspended'; } };",
        ] {
            docs.push(format!(
                "access auth_x {{ on: db; type: {variant};{extra}{authenticate} }};"
            ));
        }
    }

    // The record variant.
    let auth_exprs = [
        "",
        " signup: create user set email = $email;",
        " signin: select * from user where email = $email;",
        " signup: create user set email = $email; signin: select * from user;",
        " signup: { create user set email = $email, password = crypto::argon2::generate($password) };",
    ];
    let jwt_blocks = [
        "",
        " jwt { algorithm: hs256; key: '[REDACTED]'; };",
        " jwt { url: 'https://x/jwks'; issuer { key: '[REDACTED]'; }; };",
        " jwt { algorithm: eddsa; key: 's'; issuer { key: '[REDACTED]'; }; };",
    ];
    for auth_expr in auth_exprs {
        for jwt_block in jwt_blocks {
            for refresh in ["", " refresh;"] {
                for duration in ["", " duration { session: 8h; };"] {
                    for level in ["database", "db"] {
                        docs.push(format!(
                            "access rec_x {{ on: {level}; type: record;\
                             {auth_expr}{jwt_block}{refresh}{duration} }};"
                        ));
                    }
                }
            }
        }
    }

    // The bearer variant.
    for (subject, levels) in [
        ("user", &["root", "kv", "ns", "database"][..]),
        ("record", &["database", "db"][..]),
    ] {
        for level in levels {
            for jwt_block in ["", " jwt { algorithm: hs256; key: '[REDACTED]'; };"] {
                for duration in [
                    "",
                    " duration { grant: 30d; };",
                    " duration { token: 12h; };",
                ] {
                    docs.push(format!(
                        "access b_x {{ on: {level}; type: bearer; for: \
                         {subject};{jwt_block}{duration} }};"
                    ));
                }
            }
        }
    }

    // The 3.3 clauses: AUDIENCE rides the jwt source — the
    // variant's direct clauses and record/bearer's `jwt { … }`
    // sub-blocks; CONTEXT rides a record-authenticating access at the
    // database level (the engine's define-time shape demand,
    // probe-pinned; the payload is a read-only literal object).
    for audience in [
        " url: 'https://x/jwks'; audience: 'aud1';",
        " url: 'https://x/jwks'; audience: 'aud1', 'aud2';",
        " algorithm: hs256; key: '[REDACTED]'; audience: 'a';",
    ] {
        docs.push(format!(
            "access aud_x {{ on: db; type: jwt;{audience} duration {{ session: 8h; }}; }};"
        ));
    }
    docs.push(
        "access aud_rec { on: db; type: record; jwt { url: 'https://x/jwks'; \
         audience: 'a'; }; duration { session: 8h; }; };"
            .to_string(),
    );
    docs.push(
        "access aud_bea { on: db; type: bearer; for: user; jwt { url: 'https://x/jwks'; \
         audience: 'a'; }; duration { session: 8h; }; };"
            .to_string(),
    );
    docs.push(
        "access ctx_x { on: db; type: record; signup: create user set email = $email; \
         authenticate: $auth.active; context: { extra: 1, tier: 'gold' }; \
         duration { session: 8h; }; };"
            .to_string(),
    );
    docs.push(
        "access ctx_bea { on: db; type: bearer; for: record; jwt { url: 'https://x/jwks'; }; \
         context: { tier: 'free' }; duration { session: 8h; }; };"
            .to_string(),
    );
    // The payload is a statement position: the subquery spelling rides
    // the paren-wrapped capture/print (CoverStmts) — the slot seam the
    // literal objects above don't exercise.
    docs.push(
        "access ctx_sub { on: db; type: record; signup: create user set email = $email; \
         context: (SELECT * FROM user WHERE email = $email); \
         duration { session: 8h; }; };"
            .to_string(),
    );
    docs
}

pub fn table_docs() -> Vec<String> {
    let mut docs = Vec::new();

    // The bare and `any` kinds.
    let schema_spellings = ["", " schemafull", " schemaless", " schemafull schemaless"];
    let drops = ["", " drop;"];
    let changefeeds = [
        "",
        " changefeed { expiry: 1d; };",
        " changefeed { expiry: 12h; include original; };",
    ];
    let permission_forms = [
        "",
        " permissions: none;",
        " permissions: full;",
        " permissions { select: full; delete: none; };",
        " permissions { select, create: $auth.id = id; };",
        " permissions { select: full; create: full; update: $auth.admin; delete: $auth.admin; };",
    ];
    let graphqls = ["", " graphql { deprecated: 'gone'; };"];
    for kind_word in ["", "any"] {
        for schema in schema_spellings {
            for drop in drops {
                for changefeed in changefeeds {
                    for permissions in permission_forms {
                        for graphql in graphqls {
                            let head = if kind_word.is_empty() {
                                "table t_x".to_string()
                            } else {
                                format!("table {kind_word} t_x")
                            };
                            docs.push(format!(
                                "{head}{schema}{{{drop}{changefeed}{permissions}{graphql}}};"
                            ));
                        }
                    }
                }
            }
        }
    }

    // The relation kinds. The fully-sided header grows the 3.3
    // LIGHTWEIGHT dimension: the flag trails the header and
    // implies `enforced` — the explicit `enforced;` spelling folds to
    // the same AST (the fold rides the menu too). The other headers
    // are unchanged: a lightweight flag needs both sides named, and
    // the greedy to-list read would take the word as an endpoint. The
    // schema dimension stays `schemaless`-or-absent: SCHEMAFULL is
    // define-rejected on a lightweight relation (probe-pinned), and
    // the menu generates legal docs only.
    for header in [
        "table u1 | u2 -> r_x -> p1 | p2",
        "table f1 -> r_x ->",
        "table -> r_x -> p1",
        "table -> r_x ->",
        "table relation r_x",
    ] {
        let lightweights: &[&str] = if header.starts_with("table u1") {
            &["", " lightweight"]
        } else {
            &[""]
        };
        for lightweight in lightweights {
            for enforced in ["", " enforced;"] {
                for schema in ["", "schemaless; "] {
                    // The INLINE cache caps ride every relation doc
                    // (kind-independent — the engine accepts them on
                    // any shape, probe-pinned), in one of four
                    // spellings; the block collapses when empty.
                    for caps in [
                        "",
                        " inline edges: 8;",
                        " inline references: 16;",
                        " inline edges: 8; inline references: 16;",
                    ] {
                        let inner = format!("{schema}{enforced}{caps}");
                        let body = if inner.trim().is_empty() {
                            ";".to_string()
                        } else {
                            format!(" {{ {inner} }};")
                        };
                        docs.push(format!("{header}{lightweight}{body}"));
                    }
                }
            }
        }
    }

    // The nested definitions — the doc's host carries the fields its
    // indexes/events reference (exec validates field existence at
    // define time; the pair suite's parse leg never needed them, but
    // one menu feeds both).
    for nested in [
        " index unique u_x { fields: a; };",
        " index fulltext f_x { fields: a; analyzer: english; };",
        " event e_x { when: true; then: (create log set id = $after.id); };",
        " event e_x { then: 1; }; index count c_x { where: a > 1; };",
    ] {
        docs.push(format!("table n_x schemafull {{ field a: int;{nested}}}"));
    }

    // The view.
    for query in [
        "select author from post",
        "select author, count() as posts from post group by author",
        "select * from post where active = true",
    ] {
        for trailing in [
            "",
            " permissions: none;",
            " permissions: full;",
            " graphql { alias: 'v'; };",
        ] {
            docs.push(format!("table view v_x {{ {query};{trailing} }};"));
        }
    }
    docs
}

pub fn field_docs() -> Vec<String> {
    let mut docs = Vec::new();

    // The regular fields.
    let kinds = [
        "string",
        "option<array<record<user>>>",
        "int | string",
        "'small' | 'large' | int",
        "array<int, 5>",
        "{ a: string, b: number }",
        "geometry<point | line>",
        "set<float>",
        "record<`NaN` | `Infinity`>",
        "{ `NaN`: int, nan: Infinity }",
        "record<NAN>",
    ];
    let defaults = ["", " default: 1;", " default always: 'x';"];
    let value_asserts = ["", " value: 1;", " assert: $value != none;"];
    let references = ["", " reference;"];
    let permission_forms = [
        "",
        " permissions: none;",
        " permissions: full;",
        " permissions { select: none; };",
        " permissions { select, update: $auth.admin = true; };",
    ];
    let flags = ["", " readonly;"];
    for kind in kinds {
        for default in defaults {
            for value_assert in value_asserts {
                for reference in references {
                    for permissions in permission_forms {
                        for flag in flags {
                            docs.push(format!(
                                "field f_x: {kind} {{\n  on: t_x;{default}{value_assert}\
                                 {reference}{permissions}{flag}\n}};"
                            ));
                        }
                    }
                }
            }
        }
    }

    // `flexible` over the object-bearing shapes only.
    for kind in [
        "object",
        "{ a: int }",
        "array<object>",
        "set<{ x: string }>",
    ] {
        for flexible in ["", " flexible;"] {
            docs.push(format!("field m_x: {kind} {{\n  on: t_x;{flexible}\n}};"));
        }
    }

    // The reference actions.
    for action in [
        " reference;",
        " reference { on delete: reject; };",
        " reference { on delete: cascade; };",
        " reference { on delete: ignore; };",
        " reference { on delete: unset; };",
        " reference { on delete: { create log set at = time::now() }; };",
    ] {
        docs.push(format!(
            "field r_x: record<user> {{\n  on: t_x;{action}\n}};"
        ));
    }

    // The computed variants (the short forms nested-only, the block
    // form standalone).
    for computed in [
        " shout: string = string::uppercase(name);",
        " derived = <~person;",
        " derived = $after.id;",
    ] {
        docs.push(format!("table c_x {{{computed}}};"));
    }
    for computed in ["string::uppercase(name)", "$after.id", "<~person"] {
        docs.push(format!(
            "field c_x: string {{\n  on: t_x;\n  computed: {computed};\n}};"
        ));
    }

    // The nested placements.
    for nested in [
        " email: string;",
        " field drop: string;",
        " meta: object { flexible; }; meta.trusted: bool;",
        " tags: array<string>; tags.*: string;",
        " email: string; index unique u_x { fields: email; };",
    ] {
        docs.push(format!("table n_x schemafull{{{nested}}}"));
    }

    // The INLINE fields (3.3): a `type relation` host is the
    // engine's define-time demand, so these ride their own relation
    // doc (`likes` from the setup) — no kind constraint, every
    // orthogonal clause combines (probe-pinned: DEFAULT/READONLY ride
    // with INLINE in the printer's fixed order). The middle segments
    // keep the canonical block order (inline before permissions).
    let inline_kinds = ["number", "option<string>", "object", "array<int>"];
    let inline_middles = [
        " inline;",
        " default: 1; inline;",
        " readonly; inline;",
        " value: 1; inline;",
        " inline; permissions: none;",
    ];
    for kind in inline_kinds {
        for middle in inline_middles {
            docs.push(format!("field i_x: {kind} {{\n  on: likes;{middle}\n}};"));
        }
    }
    // `flexible` + `inline` coexist on a SCHEMAFULL relation host
    // (probe-pinned; the flexible/schema pairing keeps its own doc).
    docs.push("field fi_x: object {\n  on: sf_likes;\n  flexible; inline;\n};".to_string());
    docs
}

/// The mixin family: the MECHANISM is the menu's subject —
/// spreads, per-kind members, the explicit overshadow, and the
/// last-wins collision between spreads (the field clause grammar has
/// its own family; a's fields vary only enough to make the collision
/// and override REAL — every shape distinct from b's and the explicit
/// override's). The docs are files (the mixin statement and its
/// spreaders read top-to-bottom like the corpus's); every doc
/// executes. Define-order discipline (the cross-spread corner):
/// emission keeps authored order,
/// so the INDEX rides a's OWN field `h` — a member that always lands
/// from THIS spread (nothing collides with it), never the collision
/// key `f`, whose winner may land from the LATER spread (b's `f`
/// after a's index — `FdNotFound` at define on schemafull tables).
pub fn mixin_docs() -> Vec<String> {
    let mut docs = Vec::new();
    // a's field `f` — three clause shapes (a plain kind, a literal
    // DEFAULT, a non-default PERMISSIONS group: all round-trip INFO)
    // — plus `h`, the field a's own index names.
    let a_fields = [
        "f: int;\n  h: int;",
        "f: string { default: 'x'; };\n  h: int;",
        "f: int { permissions { select: none; }; };\n  h: int;",
    ];
    // b — collides on `f` (last-wins decides) or carries a distinct
    // key (no collision).
    let b_bodies = ["f: bool;", "g: float;"];
    // The table's explicit `f` — present (overshadows every spread's
    // `f` position-independently) or absent.
    let overrides = ["", "  f: decimal;\n"];
    // Extra member kinds on a — an index (on a's own `h`) and an
    // event (a constant body — events define against nothing).
    let extra_kinds = [
        "",
        "  index h_idx { fields: h; };\n",
        "  event f_evt { then: 1; };\n",
    ];
    for a_field in a_fields {
        for b_body in b_bodies {
            for extra in extra_kinds {
                for override_f in overrides {
                    docs.push(format!(
                        "mixin a {{\n  {a_field}\n{extra}}};\n\
                         mixin b {{ {b_body} }};\n\
                         table m_x schemafull {{\n{override_f}  ...a;\n  ...b;\n}};"
                    ));
                }
            }
        }
    }
    docs
}
