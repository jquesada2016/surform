//! SDL→DDL emission: the canonical DDL writer (`OVERWRITE` default;
//! the two define-once families — a secret-less user and a sequence —
//! emit `IF NOT EXISTS`, spec/README.md rule 2), with normalized
//! clause order and expression clauses re-printed from
//! the embedded official parser's AST.
//!
//! Emission mirrors the engine's own writers byte for byte where the
//! two meet: `QuoteStr` + `EscapeWriter` (`fmt/escape.rs`) for
//! string literals, and the ident-escaping rules of `EscapeIdent` /
//! `EscapeKwFreeIdent` — with the spec's codegen-model widening that
//! the backtick set is the supported target's *full keyword set*
//! (every keyword spelling the engine's lexer classifies, not merely
//! the reserved subset), so the same identifier emits the same way
//! wherever it can appear. Byte-identity verified against the v3.2.4
//! writers; re-verified against 3.3.0 at the floor move.
//! Statements emit in the COMPILER's topological order of the frozen
//! define-time edges (rule 7, the declarative namespace — authored
//! order is the tie-break and scope sections never move); each
//! `namespace` emits
//! its own session `USE NS`, and each `database` a `USE NS <ns> DB
//! <db>` bound to the nearest preceding namespace (codegen model
//! item 1, as amended 2026-09-19: binding is best-effort — a database
//! without a preceding namespace emits without the USE line).

use crate::ast::{
    AccessKind, ApiGroupConfig, BearerSubject, Config, Distance, Executable, Family, FieldDefault,
    FieldPath, FieldPathSeg, Filter, FnKind, FnPath, Ident, IndexKind, Introspection, JwtSource,
    JwtVerify, Level, Middleware, OnDelete, OpPermissions, Pass, Permission, Role, SdlFile, Stmt,
    TableKind, Tokenizer,
};
use crate::keywords::{escape_with, ident_needs_escape, word_shaped};
use surrealdb_types::ToSql as _;

/// Emits the canonical DDL for a parsed file. An empty file emits an
/// empty string. Emission is total — no statement requires another:
/// a `database` with no preceding `namespace` emits its `DEFINE`
/// without the `USE NS … DB` line (amended codegen model item 1,
/// 2026-09-19 — convergence and diffing work on definitions, not on
/// session context).
///
/// There is exactly one emission target — 3.3 (README codegen-model
/// item 2, as amended 2026-09-25 when the 3.2 downgrade target and
/// its SDL-E2xxx rejections retired) — and it is diagnostic-free by
/// construction, so no diagnostic list is returned.
///
/// Mixin expansion precedes emission (spec/mixin.md): this is the one
/// entry every DDL consumer goes through, so the lowering rides here
/// and no caller can forget it. The canonical SDL writer is
/// deliberately NOT lowered — spreads round-trip through fmt.
///
/// The emission order is the COMPILER's (README rule 7, the
/// declarative namespace): the lowered stream runs through the
/// stable topological sort of the frozen define-time edge table
/// ([`crate::order::emission_order`]) — authored order breaks ties,
/// so an already-ordered document emits byte-identically, and a true
/// cycle (which no order satisfies) is the order pass's SDL-E0015 at
/// check time, never a silent fold here.
pub fn emit(file: &SdlFile) -> String {
    let mut lowered = crate::expand::expand(file);
    crate::order::emission_order(&mut lowered);
    emit_lowered(&lowered, true)
}

/// The two define-once families (spec/README.md rule 2): a secret-less
/// user and a sequence. Their live state — a stored credential hash, a
/// sequence counter — must survive a re-apply, so emission uses
/// `IF NOT EXISTS` for them instead of `OVERWRITE`; the migration
/// script's Defines region says so where one rides the script.
pub(crate) fn is_define_once(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::User(user) => user.pass.is_none() && user.scram.is_none(),
        Stmt::Sequence(_) => true,
        _ => false,
    }
}

/// The emission proper, over an already-expanded file ([`emit`] runs
/// the lowering): `mixin` statements and spread entries never appear.
fn emit_lowered(file: &SdlFile, session_lines: bool) -> String {
    let mut out = String::new();
    // Session context (codegen model item 1): each `USE NS` follows
    // its namespace; a `USE NS <ns> DB <db>` binds to the nearest
    // preceding namespace statement in the file.
    let mut current_ns: Option<&Ident> = None;
    // Scope placement (spec/config.md): `config default` executes at
    // ROOT level and no `USE` can unbind back to root, so the
    // defaults hoist to before the script's FIRST `USE` line (source
    // order among themselves) — emitting one after any `USE` would
    // apply it in the wrong scope. `first_use_at` records where that
    // first `USE` lands; the splice happens once, after the loop.
    let mut first_use_at: Option<usize> = None;
    let mut root_defaults: Vec<String> = Vec::new();
    for stmt in &file.items {
        match stmt {
            Stmt::Namespace(ns) => {
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                head.push_str("DEFINE NAMESPACE OVERWRITE ");
                emit_ident(&mut head, &ns.name);
                sk.push_head_text(&head);
                if let Some(comment) = &ns.comment {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, &comment.joined());
                    sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
                // The session-context follower (codegen model item 1):
                // each `USE NS` follows its namespace. The structured
                // statement carrier ([`emit_statement_alone`]) omits
                // it — the follower is the script's own session
                // machinery, not part of the statement.
                if session_lines {
                    if first_use_at.is_none() {
                        first_use_at = Some(out.len());
                    }
                    out.push_str("USE NS ");
                    emit_ident(&mut out, &ns.name);
                    out.push_str(";\n");
                }
                current_ns = Some(&ns.name);
            }
            Stmt::Database(db) => {
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                head.push_str("DEFINE DATABASE OVERWRITE ");
                emit_ident(&mut head, &db.name);
                sk.push_head_text(&head);
                // The spec's canonical order: CHANGEFEED → STRICT →
                // COMMENT (spec/database.md). The server's own emitter
                // prints STRICT → COMMENT → CHANGEFEED; both parse —
                // the differ compares ASTs, not clause order.
                if let Some(changefeed) = &db.changefeed {
                    let mut parts = vec![crate::pretty::Seg::text(changefeed.expiry.to_sql())];
                    if changefeed.include_original {
                        parts.push(crate::pretty::Seg::text(" INCLUDE ORIGINAL"));
                    }
                    sk.clause(" CHANGEFEED ", parts);
                }
                if db.strict {
                    sk.clause(" STRICT", Vec::new());
                }
                if let Some(comment) = &db.comment {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, &comment.joined());
                    sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
                // The database's session-context follower (`USE NS …
                // DB …` binding to the nearest preceding namespace) —
                // gated on the same switch as the namespace arm's.
                if session_lines && let Some(ns) = current_ns {
                    if first_use_at.is_none() {
                        first_use_at = Some(out.len());
                    }
                    out.push_str("USE NS ");
                    emit_ident(&mut out, ns);
                    out.push_str(" DB ");
                    emit_ident(&mut out, &db.name);
                    out.push_str(";\n");
                }
            }
            Stmt::Bucket(bucket) => {
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                head.push_str("DEFINE BUCKET OVERWRITE ");
                emit_ident(&mut head, &bucket.name);
                sk.push_head_text(&head);
                // The server's own order — READONLY before BACKEND —
                // is the spec's canonical order (spec/bucket.md).
                // Absent permissions elide (rule 9: SurrealQL's FULL
                // default); an explicit `permissions: full;` emits.
                if bucket.readonly {
                    sk.clause(" READONLY", Vec::new());
                }
                if let Some(backend) = &bucket.backend {
                    sk.clause(" BACKEND ", vec![crate::pretty::Seg::value(&backend.canon)]);
                }
                if let Some(permissions) = &bucket.permissions {
                    let mut parts = Vec::new();
                    permission_parts(&mut parts, permissions);
                    sk.clause(" PERMISSIONS ", parts);
                }
                if let Some(comment) = &bucket.comment {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, &comment.joined());
                    sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
            }
            Stmt::Sequence(seq) => {
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                // The second define-once family (spec/sequence.md). A
                // sequence's START is a bootstrap value and its counter is
                // accumulated state: `OVERWRITE` deletes the sequence's
                // stored cursor rows, so re-applying would rewind every
                // mint back to the declared start. `IF NOT EXISTS` seeds a
                // fresh database and leaves an existing sequence — and its
                // counter — untouched.
                head.push_str("DEFINE SEQUENCE IF NOT EXISTS ");
                emit_ident(&mut head, &seq.name);
                sk.push_head_text(&head);
                // SurrealQL's fixed clause order: BATCH → START →
                // TIMEOUT, regardless of SDL order (rule 7: SDL's
                // block form is free-ordered, codegen canonicalizes).
                if let Some(batch) = &seq.batch {
                    sk.clause(" BATCH ", vec![crate::pretty::Seg::value(&batch.canon)]);
                }
                if let Some(start) = &seq.start {
                    sk.clause(" START ", vec![crate::pretty::Seg::value(&start.canon)]);
                }
                if let Some(timeout) = &seq.timeout {
                    sk.clause(" TIMEOUT ", vec![crate::pretty::Seg::value(&timeout.canon)]);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
            }
            Stmt::Param(param) => {
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                head.push_str("DEFINE PARAM OVERWRITE ");
                emit_param_name(&mut head, &param.name);
                sk.push_head_text(&head);
                // The server's own order: VALUE → COMMENT →
                // PERMISSIONS. Absent clauses elide (rule 9): no
                // `value` stores VALUE NONE server-side; absent
                // permissions mean SurrealQL's FULL default.
                if let Some(value) = &param.value {
                    sk.clause(" VALUE ", vec![crate::pretty::Seg::value(&value.canon)]);
                }
                if let Some(comment) = &param.comment {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, &comment.joined());
                    sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
                }
                if let Some(permissions) = &param.permissions {
                    let mut parts = Vec::new();
                    permission_parts(&mut parts, permissions);
                    sk.clause(" PERMISSIONS ", parts);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
            }
            Stmt::User(user) => {
                // One of the codegen model's two `OVERWRITE` exceptions
                // (spec/README.md rule 2; the other is the sequence):
                // a secret-less user is define-once — re-applying with
                // `OVERWRITE` would CLEAR the stored credential
                // (`PASSWORD 'x'` → `PASSHASH ''`); a secret-carrying
                // one converges normally with `OVERWRITE`. A `passscram`
                // verifier is a secret (INFO redacts it) — scram-only
                // users re-apply safely (the hash clears, the verifier
                // updates; probed 2026-09-25).
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                if is_define_once(stmt) {
                    head.push_str("DEFINE USER IF NOT EXISTS ");
                } else {
                    head.push_str("DEFINE USER OVERWRITE ");
                }
                emit_ident(&mut head, &user.name);
                // The spec's canonical order: ON → pass → PASSSCRAM →
                // ROLES → DURATION → COMMENT (the engine emitter's
                // clause order). Absent roles elide (rule 9:
                // upstream's parser seeds `VIEWER`); the durations
                // emit only the present sides, TOKEN first (the
                // server emitter's order).
                head.push_str(" ON ");
                head.push_str(match user.on.value {
                    Level::Root => "ROOT",
                    Level::Namespace => "NAMESPACE",
                    Level::Database => "DATABASE",
                });
                sk.push_head_text(&head);
                match &user.pass {
                    Some(Pass::Password(value)) => {
                        let mut quoted = String::new();
                        emit_string(&mut quoted, value);
                        sk.clause(" PASSWORD ", vec![crate::pretty::Seg::text(quoted)]);
                    }
                    Some(Pass::Hash(value)) => {
                        let mut quoted = String::new();
                        emit_string(&mut quoted, value);
                        sk.clause(" PASSHASH ", vec![crate::pretty::Seg::text(quoted)]);
                    }
                    None => {}
                }
                if let Some(scram) = &user.scram {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, scram);
                    sk.clause(" PASSSCRAM ", vec![crate::pretty::Seg::text(quoted)]);
                }
                if !user.roles.is_empty() {
                    let mut roles = String::new();
                    for (idx, role) in user.roles.iter().enumerate() {
                        if idx != 0 {
                            roles.push_str(", ");
                        }
                        roles.push_str(match role {
                            Role::Owner => "OWNER",
                            Role::Editor => "EDITOR",
                            Role::Viewer => "VIEWER",
                        });
                    }
                    sk.clause(" ROLES ", vec![crate::pretty::Seg::text(roles)]);
                }
                if user.token_duration.is_some() || user.session_duration.is_some() {
                    let mut parts = vec![crate::pretty::Seg::text("DURATION")];
                    if let Some(token) = &user.token_duration {
                        parts.push(crate::pretty::Seg::text(" FOR TOKEN "));
                        parts.push(crate::pretty::Seg::value(&token.canon));
                    }
                    if let Some(session) = &user.session_duration {
                        if user.token_duration.is_some() {
                            parts.push(crate::pretty::Seg::text(","));
                        }
                        parts.push(crate::pretty::Seg::text(" FOR SESSION "));
                        parts.push(crate::pretty::Seg::value(&session.canon));
                    }
                    sk.clause("", parts);
                }
                if let Some(comment) = &user.comment {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, &comment.joined());
                    sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
            }
            Stmt::Analyzer(analyzer) => {
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                head.push_str("DEFINE ANALYZER OVERWRITE ");
                emit_ident(&mut head, &analyzer.name);
                sk.push_head_text(&head);
                // The spec's canonical order: FUNCTION → TOKENIZERS →
                // FILTERS → COMMENT (spec/analyzer.md). List joiners
                // mirror the server's own emitter byte for byte:
                // TOKENIZERS comma-without-space, FILTERS `, `, and
                // the ngrams' arguments without an inner space.
                if let Some(function) = &analyzer.function {
                    let mut path = String::new();
                    emit_fn_path(&mut path, function);
                    sk.clause(" FUNCTION ", vec![crate::pretty::Seg::text(path)]);
                }
                if let Some(tokenizers) = &analyzer.tokenizers {
                    let mut words = String::new();
                    for (idx, tokenizer) in tokenizers.iter().enumerate() {
                        if idx != 0 {
                            words.push(',');
                        }
                        words.push_str(match tokenizer {
                            Tokenizer::Blank => "BLANK",
                            Tokenizer::Camel => "CAMEL",
                            Tokenizer::Class => "CLASS",
                            Tokenizer::Punct => "PUNCT",
                        });
                    }
                    sk.clause(" TOKENIZERS ", vec![crate::pretty::Seg::text(words)]);
                }
                if let Some(filters) = &analyzer.filters {
                    let mut words = String::new();
                    for (idx, filter) in filters.iter().enumerate() {
                        if idx != 0 {
                            words.push_str(", ");
                        }
                        emit_filter(&mut words, filter);
                    }
                    sk.clause(" FILTERS ", vec![crate::pretty::Seg::text(words)]);
                }
                if let Some(comment) = &analyzer.comment {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, &comment.joined());
                    sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
            }
            Stmt::Event(event) => emit_event(&mut out, event),
            Stmt::Module(module) => {
                // The module grammar is the one spelling SDL emits
                // (spec/module.md): `[mod::<name> FROM]`, UNSIGNED
                // mandatory, PERMISSIONS before COMMENT.
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                head.push_str("DEFINE MODULE OVERWRITE ");
                if let Some(name) = &module.name {
                    head.push_str("mod::");
                    emit_word_shaped(&mut head, &name.raw);
                    head.push_str(" FROM ");
                }
                match &module.executable {
                    Executable::File { bucket, key } => {
                        // Parse-time charset validation makes the
                        // print verbatim-safe: the upstream File
                        // emitter's backslash escaping
                        // (`display_inner`) only fires on characters
                        // SDL's file-string grammar cannot produce.
                        head.push_str("f\"");
                        head.push_str(bucket);
                        head.push(':');
                        head.push_str(key);
                        head.push('"');
                    }
                    Executable::Silo {
                        org,
                        pkg,
                        major,
                        minor,
                        patch,
                    } => {
                        head.push_str("silo::");
                        emit_word_shaped(&mut head, org);
                        head.push_str("::");
                        emit_word_shaped(&mut head, pkg);
                        head.push_str(&format!("::<{major}.{minor}.{patch}>"));
                    }
                }
                sk.push_head_text(&head);
                if module.unsigned {
                    sk.clause(" UNSIGNED", Vec::new());
                }
                if let Some(permissions) = &module.permissions {
                    let mut parts = Vec::new();
                    permission_parts(&mut parts, permissions);
                    sk.clause(" PERMISSIONS ", parts);
                }
                if let Some(comment) = &module.comment {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, &comment.joined());
                    sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
            }
            Stmt::Function(function) => {
                // SDL's canonical trailing-clause order (PERMISSIONS →
                // GRAPHQL_* → COMMENT) diverges from the server
                // emitter's (body → COMMENT → GRAPHQL_* →
                // PERMISSIONS) — both parse; the differ
                // compares ASTs (spec/function.md). The body canon is
                // the whole `{ … }` block print, printed verbatim;
                // kinds pass through verbatim per README rule 4 (the
                // source spelling re-parses identically — upstream's
                // printer normalizes `option<T>` to `none | T`, SDL
                // does not adopt that).
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                head.push_str("DEFINE FUNCTION OVERWRITE ");
                emit_fn_path(&mut head, &function.name);
                head.push('(');
                for (i, (name, kind)) in function.args.iter().enumerate() {
                    if i > 0 {
                        head.push_str(", ");
                    }
                    emit_param_name(&mut head, name);
                    head.push_str(": ");
                    head.push_str(&kind.text);
                }
                head.push_str(") ");
                if let Some(returns) = &function.returns {
                    head.push_str("-> ");
                    head.push_str(&returns.text);
                    head.push(' ');
                }
                sk.push_head_text(&head);
                // The body is a head value piece: its block document
                // lays out on its own (a multi-statement body is
                // multi-line no matter what), and the trailing clauses
                // break after its close.
                sk.push_head_value(&function.body.canon);
                if let Some(permissions) = &function.permissions {
                    let mut parts = Vec::new();
                    permission_parts(&mut parts, permissions);
                    sk.clause(" PERMISSIONS ", parts);
                }
                if let Some(graphql) = &function.graphql {
                    emit_graphql_clauses(&mut sk, graphql);
                }
                if let Some(comment) = &function.comment {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, &comment.joined());
                    sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
            }
            Stmt::Config(config) => {
                // Configs are per-variant singletons: OVERWRITE
                // unconditionally (codegen model item 2). `api` and
                // `graphql` are database-level and emit in source
                // order (best-effort placement, like every session
                // line); `default` is the ROOT-level hoist above.
                match config {
                    Config::Api(api) => {
                        let mut sk = crate::pretty::Skeleton::new();
                        sk.push_head_text("DEFINE CONFIG OVERWRITE API");
                        // Upstream's emitter prints MIDDLEWARE before
                        // PERMISSIONS and elides empty middleware; a
                        // permissions clause always emits.
                        if !api.middleware.is_empty() {
                            let mut parts = Vec::new();
                            middleware_entries_parts(&mut parts, &api.middleware);
                            sk.clause(" MIDDLEWARE ", parts);
                        }
                        if let Some(permissions) = &api.permissions {
                            let mut parts = Vec::new();
                            permission_parts(&mut parts, permissions);
                            sk.clause(" PERMISSIONS ", parts);
                        }
                        sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                        out.push('\n');
                    }
                    Config::Graphql(graphql) => {
                        let mut sk = crate::pretty::Skeleton::new();
                        sk.push_head_text("DEFINE CONFIG OVERWRITE GRAPHQL");
                        // Spec-minimal family emission (the
                        // spec/config.md goldens): the all-default
                        // config emits the bare keyword and both-auto
                        // folds to the bare flag's compact form —
                        // upstream's own ToSql ALWAYS prints
                        // `TABLES … FUNCTIONS …` (probe-pinned);
                        // both spellings re-parse identically, and
                        // the differ compares ASTs.
                        match (&graphql.tables, &graphql.functions) {
                            (Family::None, Family::None) => {}
                            (Family::Auto, Family::Auto) => {
                                sk.clause(" AUTO", Vec::new());
                            }
                            (tables, functions) => {
                                let mut parts = Vec::new();
                                family_parts(&mut parts, tables, emit_ident);
                                sk.clause(" TABLES ", parts);
                                let mut parts = Vec::new();
                                family_parts(&mut parts, functions, emit_fn_path);
                                sk.clause(" FUNCTIONS ", parts);
                            }
                        }
                        if let Some(depth) = graphql.depth {
                            sk.clause(" DEPTH ", vec![crate::pretty::Seg::text(depth.to_string())]);
                        }
                        if let Some(complexity) = graphql.complexity {
                            sk.clause(
                                " COMPLEXITY ",
                                vec![crate::pretty::Seg::text(complexity.to_string())],
                            );
                        }
                        // `introspection: auto` is SurrealQL's
                        // default and elides (rule 9); only `none`
                        // prints.
                        if graphql.introspection == Introspection::None {
                            sk.clause(" INTROSPECTION NONE", Vec::new());
                        }
                        sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                        out.push('\n');
                    }
                    Config::Default(default) => {
                        // Both sides always emit — the unset side is
                        // `NONE` (upstream prints both
                        // unconditionally).
                        let mut sk = crate::pretty::Skeleton::new();
                        sk.push_head_text("DEFINE CONFIG OVERWRITE DEFAULT");
                        match &default.namespace {
                            Some(expr) => sk.clause(
                                " NAMESPACE ",
                                vec![crate::pretty::Seg::value(&expr.canon)],
                            ),
                            None => sk.clause(" NAMESPACE NONE", Vec::new()),
                        }
                        match &default.database {
                            Some(expr) => sk
                                .clause(" DATABASE ", vec![crate::pretty::Seg::value(&expr.canon)]),
                            None => sk.clause(" DATABASE NONE", Vec::new()),
                        }
                        let mut line = String::new();
                        sk.write(&mut line, crate::pretty::DEFAULT_MAX_WIDTH);
                        line.push('\n');
                        root_defaults.push(line);
                    }
                }
            }
            Stmt::Index(index) => emit_index(&mut out, index),
            Stmt::Api(api) => {
                // `FOR any` first — canonical placement, matching the
                // emitter's always-materialized clause — then groups
                // in source order. Within each FOR: MIDDLEWARE →
                // PERMISSIONS → THEN (the emitters' order; clause
                // order is free in SDL). Minimal-form emission:
                // PERMISSIONS prints only when the
                // SDL wrote one (upstream's unconditional
                // `PERMISSIONS FULL` is the INFO side's
                // materialization — spec/api.md's reverse section),
                // and an empty `any` block emits bare `FOR ANY`.
                // THEN canon prints verbatim: the action canon is
                // already unwrapped and the fallback CoverStmts-wrapped
                // (each expr.rs slot slices its own print).
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                head.push_str("DEFINE API OVERWRITE ");
                emit_string(&mut head, &api.path);
                sk.push_head_text(&head);
                if let Some(any) = &api.any {
                    let mut parts = vec![crate::pretty::Seg::text("ANY")];
                    api_group_config_parts(&mut parts, &any.config);
                    if let Some(fallback) = &any.fallback {
                        parts.push(crate::pretty::Seg::text(" THEN "));
                        parts.push(crate::pretty::Seg::value(&fallback.canon));
                    }
                    sk.clause(" FOR ", parts);
                }
                for action in &api.actions {
                    let mut methods = String::new();
                    for (i, method) in action.methods.iter().enumerate() {
                        if i > 0 {
                            methods.push_str(", ");
                        }
                        methods.push_str(method.as_str());
                    }
                    let mut parts = vec![crate::pretty::Seg::text(methods)];
                    api_group_config_parts(&mut parts, &action.config);
                    parts.push(crate::pretty::Seg::text(" THEN "));
                    parts.push(crate::pretty::Seg::value(&action.action.canon));
                    sk.clause(" FOR ", parts);
                }
                if let Some(comment) = &api.comment {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, &comment.joined());
                    sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
            }
            Stmt::Access(access) => {
                // Canonical order (spec/access.md): TYPE → SIGNUP/
                // SIGNIN → WITH REFRESH → WITH JWT [WITH ISSUER] →
                // AUTHENTICATE → DURATION → COMMENT — the order the
                // server's own emitter writes and the only one its
                // record grammar accepts (SIGNUP/SIGNIN precede the
                // WITH clauses). `OVERWRITE` rides the head like
                // every statement (the spec examples omit it for
                // readability).
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                head.push_str("DEFINE ACCESS OVERWRITE ");
                emit_ident(&mut head, &access.name);
                head.push_str(" ON ");
                head.push_str(match access.on.value {
                    Level::Root => "ROOT",
                    Level::Namespace => "NAMESPACE",
                    Level::Database => "DATABASE",
                });
                sk.push_head_text(&head);
                match &access.kind {
                    AccessKind::Jwt { source } => {
                        let mut parts = vec![crate::pretty::Seg::text("JWT ")];
                        jwt_source_parts(&mut parts, source);
                        // The whole verify shape rides one clause: its
                        // URL/KEY values are the payload's pieces.
                        sk.clause(" TYPE ", parts);
                    }
                    AccessKind::Record {
                        signup,
                        signin,
                        jwt,
                        refresh,
                    } => {
                        sk.clause(" TYPE RECORD", Vec::new());
                        if let Some(signup) = signup {
                            sk.clause(" SIGNUP ", vec![crate::pretty::Seg::value(&signup.canon)]);
                        }
                        if let Some(signin) = signin {
                            sk.clause(" SIGNIN ", vec![crate::pretty::Seg::value(&signin.canon)]);
                        }
                        if *refresh {
                            sk.clause(" WITH REFRESH", Vec::new());
                        }
                        // Minimal-form issuance:
                        // a record access with no `jwt { … }` emits
                        // none — SurrealQL then auto-issues with a
                        // random HS512 key at define time, which the
                        // differ treats as SDL-managed-only
                        // (spec/access.md's reverse section). The
                        // random keys are never SDL-declared, so
                        // adopting them into the emission would make
                        // the output non-convergent.
                        if let Some(source) = jwt {
                            let mut parts = Vec::new();
                            jwt_source_parts(&mut parts, source);
                            sk.clause(" WITH JWT ", parts);
                        }
                    }
                    AccessKind::Bearer { subject, jwt } => {
                        let mut parts = vec![crate::pretty::Seg::text(match subject {
                            BearerSubject::User => "BEARER FOR USER",
                            BearerSubject::Record => "BEARER FOR RECORD",
                        })];
                        // Emitted even though the server's ToSql (and
                        // INFO) never renders a bearer's jwt — without
                        // it the clause would simply not exist
                        // (spec/access.md, verified).
                        if let Some(source) = jwt {
                            parts.push(crate::pretty::Seg::text(" WITH JWT "));
                            jwt_source_parts(&mut parts, source);
                        }
                        sk.clause(" TYPE ", parts);
                    }
                }
                if let Some(authenticate) = &access.authenticate {
                    sk.clause(
                        " AUTHENTICATE ",
                        vec![crate::pretty::Seg::value(&authenticate.canon)],
                    );
                }
                // CONTEXT prints before DURATION (the printer's own
                // note — a rendered definition must re-parse; 3.3's
                // clause).
                if let Some(context) = &access.context {
                    sk.clause(" CONTEXT ", vec![crate::pretty::Seg::value(&context.canon)]);
                }
                if let Some(duration) = &access.duration {
                    // Only the written sides print (minimal-form: the
                    // server's ToSql always materializes DURATION with
                    // its grant-30d/token-1h/session-none defaults —
                    // the normalizer's adopt/elide business);
                    // GRANT → TOKEN → SESSION, comma-BEFORE-joiner so
                    // no side ever carries a trailing one (the server
                    // emitter appends-then-pops; the joiner position
                    // is the same bytes without the pop).
                    let sides = [
                        ("GRANT", &duration.grant),
                        ("TOKEN", &duration.token),
                        ("SESSION", &duration.session),
                    ];
                    if sides.iter().any(|(_, side)| side.is_some()) {
                        let mut parts = vec![crate::pretty::Seg::text("DURATION")];
                        let mut written = false;
                        for (label, side) in sides {
                            if let Some(expr) = side {
                                if written {
                                    parts.push(crate::pretty::Seg::text(","));
                                }
                                written = true;
                                parts.push(crate::pretty::Seg::text(format!(" FOR {label} ")));
                                parts.push(crate::pretty::Seg::value(&expr.canon));
                            }
                        }
                        sk.clause("", parts);
                    }
                }
                if let Some(comment) = &access.comment {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, &comment.joined());
                    sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
            }
            Stmt::Table(table) => {
                // SDL's canonical order — TYPE → DROP → schema flag →
                // CHANGEFEED → AS SELECT → PERMISSIONS → GRAPHQL_* →
                // INLINE EDGES|REFERENCES → COMMENT — diverges from the
                // server emitter's (TYPE → DROP → schema flag →
                // COMMENT → GRAPHQL_* → INLINE caps → AS → CHANGEFEED,
                // permission groups merged by equal value with
                // delete-Full elided); both parse — the differ compares
                // ASTs, never text (spec/table.md). The kind and the
                // schema flag ALWAYS emit (README rule 9's licensed
                // deviations: bare → `TYPE NORMAL`, the fail-closed
                // default; the flag keeps emitted DDL byte-stable
                // against INFO, which materializes both), and a view's
                // emission is fixed (`TYPE ANY SCHEMALESS AS SELECT …`).
                let mut sk = crate::pretty::Skeleton::new();
                let mut head = String::new();
                head.push_str("DEFINE TABLE OVERWRITE ");
                emit_ident(&mut head, &table.name);
                sk.push_head_text(&head);
                match &table.kind {
                    TableKind::Normal => sk.push_head_text(" TYPE NORMAL"),
                    TableKind::Any => sk.push_head_text(" TYPE ANY"),
                    TableKind::Relation {
                        from,
                        to,
                        enforced,
                        lightweight,
                    } => {
                        sk.push_head_text(" TYPE RELATION");
                        // Only the non-empty sides print (the omitted
                        // side is the unconstrained one); ` | ` is
                        // SurrealQL's endpoint joiner (commas are
                        // rejected there — spec/table.md).
                        for (label, side) in [("IN", from), ("OUT", to)] {
                            if let Some(side) = side {
                                sk.push_head_text(&format!(" {label} "));
                                for (i, endpoint) in side.iter().enumerate() {
                                    if i > 0 {
                                        sk.push_head_text(" | ");
                                    }
                                    let mut ident = String::new();
                                    emit_ident(&mut ident, endpoint);
                                    sk.push_head_text(&ident);
                                }
                            }
                        }
                        if *enforced {
                            sk.push_head_text(" ENFORCED");
                        }
                        // `lightweight` folds `enforced` at parse, but
                        // the emit stays defensive — a hand-built AST
                        // with the implication unapplied still emits
                        // the shape the engine materializes (the
                        // reader's parse-fold rides the same rule).
                        if *lightweight {
                            sk.push_head_text(" LIGHTWEIGHT");
                        }
                    }
                    TableKind::View { query } => {
                        // The whole `TYPE ANY SCHEMALESS AS` spine is
                        // head (the official formatter's header type
                        // clause); the query rides as a head value
                        // piece so its own statement document (the
                        // SELECT skeleton) lays out at its real
                        // column.
                        sk.push_head_text(" TYPE ANY SCHEMALESS AS ");
                        sk.push_head_value(&query.canon);
                    }
                }
                if table.drop {
                    sk.push_head_text(" DROP");
                }
                if table.kind.is_view() {
                    // The view's schema side is fixed (SCHEMALESS,
                    // above) and its changefeed is parse-rejected.
                    debug_assert!(table.schema_full.is_none() && table.changefeed.is_none());
                } else {
                    // Always emitted: `None` = neither flag written —
                    // still SCHEMALESS (the licensed deviation).
                    sk.push_head_text(match table.schema_full {
                        Some(true) => " SCHEMAFULL",
                        _ => " SCHEMALESS",
                    });
                    if let Some(changefeed) = &table.changefeed {
                        let mut parts = vec![crate::pretty::Seg::text(changefeed.expiry.to_sql())];
                        if changefeed.include_original {
                            parts.push(crate::pretty::Seg::text(" INCLUDE ORIGINAL"));
                        }
                        sk.clause(" CHANGEFEED ", parts);
                    }
                }
                if let Some(permissions) = &table.permissions {
                    let mut parts = Vec::new();
                    op_permissions_parts(&mut parts, permissions);
                    sk.clause(" PERMISSIONS ", parts);
                }
                if let Some(graphql) = &table.graphql {
                    emit_graphql_clauses(&mut sk, graphql);
                }
                // The INLINE cache caps print only when written (the
                // server prints only `Some` sides too — bare integers,
                // EDGES before REFERENCES, probe-pinned live 2026-09-25).
                if let Some(cap) = table.inline_edges {
                    sk.clause(
                        " INLINE EDGES ",
                        vec![crate::pretty::Seg::text(cap.to_string())],
                    );
                }
                if let Some(cap) = table.inline_references {
                    sk.clause(
                        " INLINE REFERENCES ",
                        vec![crate::pretty::Seg::text(cap.to_string())],
                    );
                }
                if let Some(comment) = &table.comment {
                    let mut quoted = String::new();
                    emit_string(&mut quoted, &comment.joined());
                    sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
                }
                sk.write(&mut out, crate::pretty::DEFAULT_MAX_WIDTH);
                out.push('\n');
                // The nested definitions in source order (rule 7 —
                // the table's own definition first, then whatever
                // references it: SurrealDB validates field existence
                // at define time).
                for stmt in &table.nested {
                    match stmt {
                        Stmt::Index(index) => emit_index(&mut out, index),
                        Stmt::Event(event) => emit_event(&mut out, event),
                        Stmt::Field(field) => emit_field(&mut out, field),
                        _ => unreachable!(
                            "the parser nests index/event/field/spread, and expansion resolves the spreads"
                        ),
                    }
                }
            }
            Stmt::Field(field) => emit_field(&mut out, field),
            Stmt::Mixin(_) | Stmt::Spread(_) => {
                unreachable!("expansion drops mixin statements and spread entries before emission")
            }
            Stmt::TypeAlias(_) => {
                unreachable!("expansion drops type-alias statements before emission")
            }
            Stmt::Include(_) => unreachable!(
                "composition lowers includes before emission (the loader flattens; \
                 the strict parse entry rejects them)"
            ),
            Stmt::Raw(raw) => {
                // Rule 8: verbatim means verbatim — no re-spelling, no
                // re-indentation, comments included. The parser stored
                // the edges trimmed, so the statement always ends its
                // line cleanly; a whitespace-only body emits nothing.
                if !raw.body.is_empty() {
                    out.push_str(&raw.body);
                    out.push('\n');
                }
            }
        }
    }
    if !root_defaults.is_empty() {
        let at = first_use_at.unwrap_or(0);
        out.insert_str(at, &root_defaults.concat());
    }
    out
}

/// One statement's canonical DDL — the exact printer [`emit_lowered`]
/// walks, over a one-statement file. No session lines and no
/// root-defaults hoist beyond the statement itself (`config default`
/// renders its own line); the sole session line a single statement can
/// carry is a `namespace` define's trailing `USE NS` (codegen model
/// item 1). The migration script's define arm
/// ([`crate::info::script::convergence_script`]) renders each define
/// finding through this — the same bytes `build` would emit for the
/// statement — with the script's own `USE` lines carrying scope
/// context between them.
pub fn emit_statement(stmt: &Stmt) -> String {
    emit_lowered(
        &SdlFile {
            items: vec![stmt.clone()],
            trailing_comments: Vec::new(),
        },
        true,
    )
}

/// [`emit_statement`] with the session-context line suppressed — the
/// one difference is a `namespace` define's trailing `USE NS`
/// follower (a `database` define's follower only exists when a
/// namespace precedes, which a one-statement emission never has). The
/// migration script's structured statement carrier
/// ([`crate::info::script::ConvergenceScript`]) captures statement
/// boundaries AT EMISSION through this — the carrier's elements are
/// exactly the statements, never the script's own `USE` machinery —
/// so an executor that runs one element per engine call can never
/// ride a context line into a first-expression-only host import.
pub fn emit_statement_alone(stmt: &Stmt) -> String {
    emit_lowered(
        &SdlFile {
            items: vec![stmt.clone()],
            trailing_comments: Vec::new(),
        },
        false,
    )
}

/// One statement's canonical pieces — usually one, but a table whose
/// `nested` members ride it (the script walk slots standalone
/// children into their host's emission) prints the head PLUS one
/// statement per member, so the pieces are the true statement
/// boundaries the one emission produces. The concatenation is
/// byte-identical to [`emit_statement_alone`] (the head's clauses
/// never read the members; each member's emission is the same call
/// the table arm makes). The structured carrier's other boundary
/// source — with [`emit_statement_alone`], the generator's complete
/// "where a statement starts and ends" knowledge.
pub fn emit_statement_pieces(stmt: &Stmt) -> Vec<String> {
    match stmt {
        Stmt::Table(table) if !table.nested.is_empty() => {
            let mut head = table.clone();
            head.nested = Vec::new();
            let mut pieces = vec![emit_statement_alone(&Stmt::Table(head))];
            pieces.extend(table.nested.iter().map(emit_statement_alone));
            pieces
        }
        _ => vec![emit_statement_alone(stmt)],
    }
}

/// The stage-2 oracle: validates DDL through the embedded official
/// parser (capabilities on — the same experimental set every other
/// embed site uses). `emit`'s output parses by construction — the
/// full-corpus oracle holds the fixed point — so `Err` means a Surform
/// bug, and callers surface it as [`crate::diag::Diag::EmbeddedDdlRejected`].
/// `Err` carries the upstream render verbatim (the DDL text is its
/// own coordinate space). This is the one public home of the
/// whole-DDL probe: the wasm bindings' `check` and any future native
/// self-check share it.
pub fn validate_ddl(ddl: &str) -> Result<(), String> {
    crate::expr::probe_statements(ddl)
}

/// A graphql family value: the bare enums inline; the structured
/// list comma-separated (`INCLUDE user, post`), items through the
/// caller's per-entry writer (idents for tables, fn paths for
/// functions).
fn family_parts<T>(
    out: &mut Vec<crate::pretty::Seg>,
    family: &Family<T>,
    item: impl Fn(&mut String, &T),
) {
    match family {
        Family::None => out.push(crate::pretty::Seg::text("NONE")),
        Family::Auto => out.push(crate::pretty::Seg::text("AUTO")),
        Family::Include(items) => {
            comma_separated_parts(out, items, item, "INCLUDE ");
        }
        Family::Exclude(items) => {
            comma_separated_parts(out, items, item, "EXCLUDE ");
        }
    }
}

/// The comma-separated list behind a family's INCLUDE/EXCLUDE: a
/// leading label piece, then `, `-joined items through the caller's
/// writer.
fn comma_separated_parts<T>(
    out: &mut Vec<crate::pretty::Seg>,
    items: &[T],
    mut item: impl FnMut(&mut String, &T),
    label: &str,
) {
    out.push(crate::pretty::Seg::text(label));
    let mut joined = String::new();
    for (i, value) in items.iter().enumerate() {
        if i > 0 {
            joined.push_str(", ");
        }
        item(&mut joined, value);
    }
    out.push(crate::pretty::Seg::text(joined));
}

fn emit_comma_separated<T>(out: &mut String, items: &[T], mut item: impl FnMut(&mut String, &T)) {
    for (i, value) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        item(out, value);
    }
}

/// The GRAPHQL_ALIAS / GRAPHQL_DEPRECATED clause pair shared by
/// `field`, `table`, and `function`.
fn emit_graphql_clauses(sk: &mut crate::pretty::Skeleton, graphql: &crate::ast::Graphql) {
    if let Some(alias) = &graphql.alias {
        let mut quoted = String::new();
        emit_string(&mut quoted, alias);
        sk.clause(" GRAPHQL_ALIAS ", vec![crate::pretty::Seg::text(quoted)]);
    }
    if let Some(deprecated) = &graphql.deprecated {
        let mut quoted = String::new();
        emit_string(&mut quoted, deprecated);
        sk.clause(
            " GRAPHQL_DEPRECATED ",
            vec![crate::pretty::Seg::text(quoted)],
        );
    }
}

/// A `MIDDLEWARE` list's entries — the comma-separated `path(args)`
/// shapes over the args' canon, without the keyword (the keyword is
/// either the clause key or a continuation piece, whichever position
/// calls for).
fn middleware_entries_parts(out: &mut Vec<crate::pretty::Seg>, middleware: &[Middleware]) {
    for (i, entry) in middleware.iter().enumerate() {
        if i > 0 {
            out.push(crate::pretty::Seg::text(", "));
        }
        let mut path = String::new();
        emit_fn_path(&mut path, &entry.path);
        out.push(crate::pretty::Seg::text(path));
        out.push(crate::pretty::Seg::text("("));
        for (j, arg) in entry.args.iter().enumerate() {
            if j > 0 {
                out.push(crate::pretty::Seg::text(", "));
            }
            out.push(crate::pretty::Seg::value(&arg.canon));
        }
        out.push(crate::pretty::Seg::text(")"));
    }
}

/// The `MIDDLEWARE` keyword plus its entries, as a continuation
/// payload inside an api FOR group (`ANY MIDDLEWARE …`).
fn middleware_parts(out: &mut Vec<crate::pretty::Seg>, middleware: &[Middleware]) {
    if middleware.is_empty() {
        return;
    }
    out.push(crate::pretty::Seg::text(" MIDDLEWARE "));
    middleware_entries_parts(out, middleware);
}

/// An api sub-block's config — `MIDDLEWARE …` then `PERMISSIONS …`
/// (the emitters' canonical order). Minimal-form: the permissions
/// clause prints only when the SDL wrote one (upstream's
/// unconditional ` PERMISSIONS FULL` print is the INFO
/// side's materialization — spec/api.md's reverse section).
fn api_group_config_parts(out: &mut Vec<crate::pretty::Seg>, config: &ApiGroupConfig) {
    middleware_parts(out, &config.middleware);
    if let Some(permissions) = &config.permissions {
        out.push(crate::pretty::Seg::text(" PERMISSIONS "));
        permission_parts(out, permissions);
    }
}

/// A JWT verification + issuing source (spec/access.md): the verify
/// shape verbatim, then ` WITH ISSUER …` only when the SDL wrote an
/// `issuer { … }` block (minimal-form: a symmetric verify key's
/// implicit mirror and the jwt-less record's random auto-issue are
/// SurrealQL-side materializations, never SDL emission). A declared
/// issuer algorithm prints only on the URL-verified path: by local
/// key it is forced equal to the verification algorithm (elided as
/// forced-equal — rule 6), while on the URL path the server's own
/// ToSql never prints it, and a re-parse of its output would silently
/// materialize HS512 — so this is the one place SDL emits MORE than
/// the server emitter (spec/access.md's issuer note). A key-less
/// issuer (parse-validated to be the symmetric-mirrored case) emits
/// the verify key — the value SurrealQL itself mirrors. The
/// skeleton-payload form: the verify/issuer expressions are value
/// pieces.
fn jwt_source_parts(out: &mut Vec<crate::pretty::Seg>, source: &JwtSource) {
    let mut verify_key_canon: Option<&str> = None;
    match &source.verify {
        JwtVerify::Url(expr) => {
            out.push(crate::pretty::Seg::text("URL "));
            out.push(crate::pretty::Seg::value(&expr.canon));
        }
        JwtVerify::Key { algorithm, key } => {
            out.push(crate::pretty::Seg::text("ALGORITHM "));
            out.push(crate::pretty::Seg::text(algorithm.as_str()));
            out.push(crate::pretty::Seg::text(" KEY "));
            out.push(crate::pretty::Seg::value(&key.canon));
            verify_key_canon = Some(&key.canon);
        }
    }
    if let Some(audience) = &source.audience {
        out.push(crate::pretty::Seg::text(" AUDIENCE "));
        let mut joined = String::new();
        emit_comma_separated(&mut joined, audience, |out, value| emit_string(out, value));
        out.push(crate::pretty::Seg::text(joined));
    }
    if let Some(issuer) = &source.issuer {
        out.push(crate::pretty::Seg::text(" WITH ISSUER"));
        if let (Some(algorithm), JwtVerify::Url(_)) = (issuer.algorithm, &source.verify) {
            out.push(crate::pretty::Seg::text(" ALGORITHM "));
            out.push(crate::pretty::Seg::text(algorithm.as_str()));
        }
        out.push(crate::pretty::Seg::text(" KEY "));
        match &issuer.key {
            Some(key) => out.push(crate::pretty::Seg::value(&key.canon)),
            None => out.push(crate::pretty::Seg::text(
                verify_key_canon
                    .expect(
                        "a key-less issuer parses only when \
                     the verify key exists to mirror",
                    )
                    .to_string(),
            )),
        }
    }
}

/// A single-value permission: the keywords uppercase, an expression
/// as `WHERE <canon>` (the expression canon is the clause text
/// without the `WHERE`, sliced at parse time). The skeleton-payload
/// form — the expression rides as a value piece, laid out at its
/// clause column when the statement breaks.
fn permission_parts(out: &mut Vec<crate::pretty::Seg>, permissions: &Permission) {
    match permissions {
        Permission::None => out.push(crate::pretty::Seg::text("NONE")),
        Permission::Full => out.push(crate::pretty::Seg::text("FULL")),
        Permission::Expr(expr) => {
            out.push(crate::pretty::Seg::text("WHERE "));
            out.push(crate::pretty::Seg::value(&expr.canon));
        }
    }
}

/// The per-operation permissions shared by `table` and `field`
/// (spec/table.md, spec/field.md): the single-value shorthand, or the
/// written `FOR` groups — source order, ops comma-joined, values
/// spelled `NONE`/`FULL`/`WHERE <canon>` (minimal-form: unlisted
/// operations are the engine's seed — NONE for tables, FULL for
/// fields — so nothing is invented here either way). An empty
/// `permissions { };` folds to the all-NONE default and emits
/// nothing (rule 9 — SurrealQL's own zero-`FOR` parse). The
/// skeleton-payload form: each group's value is a value piece, and
/// the groups separate at a break point — flat `, `, broken one
/// group per line at the clause's indent.
fn op_permissions_parts(out: &mut Vec<crate::pretty::Seg>, permissions: &OpPermissions) {
    match permissions {
        OpPermissions::None => out.push(crate::pretty::Seg::text("NONE")),
        OpPermissions::Full => out.push(crate::pretty::Seg::text("FULL")),
        OpPermissions::Groups(groups) => {
            for (i, group) in groups.iter().enumerate() {
                if i > 0 {
                    out.push(crate::pretty::Seg::text(","));
                    out.push(crate::pretty::Seg::line());
                }
                out.push(crate::pretty::Seg::text("FOR "));
                for (j, op) in group.ops.iter().enumerate() {
                    if j > 0 {
                        out.push(crate::pretty::Seg::text(", "));
                    }
                    out.push(crate::pretty::Seg::text(op.as_str()));
                }
                out.push(crate::pretty::Seg::text(" "));
                permission_parts(out, &group.value);
            }
        }
    }
}

/// One `index` definition — shared by the standalone statement and
/// its nested placement inside a `table` block (the emission is
/// identical: the nested form's host table rides the AST's `on`).
/// Minimal-form kind emission (spec/index.md's goldens): only the
/// params the SDL source specified print — the server's own ToSql
/// materializes defaults (bare FULLTEXT reprints
/// `ANALYZER like BM25(1.2,0.75)`, HNSW reprints derived `M0`/`LM`).
/// Both spellings re-parse to identical ASTs; the differ
/// compares ASTs. `OVERWRITE` rides the head like every statement
/// (the spec examples omit it for readability).
fn emit_index(out: &mut String, index: &crate::ast::Index) {
    let mut sk = crate::pretty::Skeleton::new();
    let mut head = String::new();
    head.push_str("DEFINE INDEX OVERWRITE ");
    emit_ident(&mut head, &index.name);
    head.push_str(" ON ");
    emit_ident(&mut head, &index.on);
    sk.push_head_text(&head);
    // The field expressions ride as head value pieces — their
    // laid-out form must see the column they really render at, so
    // they cannot be pre-printed into a text run.
    if !index.fields.is_empty() {
        sk.push_head_text(" FIELDS ");
        for (i, expr) in index.fields.iter().enumerate() {
            if i > 0 {
                sk.push_head_text(", ");
            }
            sk.push_head_value(&expr.canon);
        }
    }
    let mut parts = Vec::new();
    index_kind_parts(&mut parts, &index.kind);
    if !parts.is_empty() {
        sk.clause("", parts);
    }
    if let Some(comment) = &index.comment {
        let mut quoted = String::new();
        emit_string(&mut quoted, &comment.joined());
        sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
    }
    if index.concurrently {
        sk.clause(" CONCURRENTLY", Vec::new());
    }
    sk.write(out, crate::pretty::DEFAULT_MAX_WIDTH);
    out.push('\n');
}

/// One `event` definition — standalone and nested share the shape
/// (see [`emit_index`]). The spec's canonical order: ON → ASYNC →
/// WHEN → THEN → COMMENT (spec/event.md — the v3.2.4 emitter's own
/// order, `ASYNC RETRY n MAXDEPTH n` glued). Absent `when` emits
/// nothing (rule 9: upstream materializes `WHEN true` internally;
/// the adopt/elide decision is the normalizer's); a partial async block
/// emits only the sides written.
fn emit_event(out: &mut String, event: &crate::ast::Event) {
    let mut sk = crate::pretty::Skeleton::new();
    let mut head = String::new();
    head.push_str("DEFINE EVENT OVERWRITE ");
    emit_ident(&mut head, &event.name);
    head.push_str(" ON ");
    emit_ident(&mut head, &event.on);
    sk.push_head_text(&head);
    if let Some(params) = &event.r#async {
        let mut parts = vec![crate::pretty::Seg::text("ASYNC")];
        if let Some(retry) = params.retry {
            parts.push(crate::pretty::Seg::text(format!(" RETRY {retry}")));
        }
        if let Some(maxdepth) = params.maxdepth {
            parts.push(crate::pretty::Seg::text(format!(" MAXDEPTH {maxdepth}")));
        }
        sk.clause("", parts);
    }
    if let Some(when) = &event.when {
        sk.clause(" WHEN ", vec![crate::pretty::Seg::value(&when.canon)]);
    }
    // The server emitter's comma joiner (`Fmt::comma_separated`) —
    // the items separate at a break point (flat `, `, broken one item
    // per line at the clause's indent).
    let mut parts = Vec::new();
    for (idx, item) in event.then.iter().enumerate() {
        if idx != 0 {
            parts.push(crate::pretty::Seg::text(","));
            parts.push(crate::pretty::Seg::line());
        }
        parts.push(crate::pretty::Seg::value(&item.canon));
    }
    sk.clause(" THEN ", parts);
    if let Some(comment) = &event.comment {
        let mut quoted = String::new();
        emit_string(&mut quoted, &comment.joined());
        sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
    }
    sk.write(out, crate::pretty::DEFAULT_MAX_WIDTH);
    out.push('\n');
}

/// One `field` definition — standalone and nested share the shape
/// (see [`emit_index`]; the nested form's host table rides the AST's
/// `on`). The spec's canonical order (spec/field.md): `TYPE [FLEXIBLE]
/// → READONLY → VALUE → ASSERT → DEFAULT [ALWAYS] → COMPUTED →
/// REFERENCE [ON DELETE …] → PERMISSIONS → GRAPHQL_* → COMMENT` —
/// SDL's own choice, which the server emitter's order diverges from
/// (DEFAULT before ASSERT, COMMENT/GRAPHQL near the end); both parse,
/// and the differ compares ASTs. Minimal-form emission:
/// absent clauses elide — the server's
/// unconditional `PERMISSIONS FULL` is the INFO side's
/// materialization — and `reference`'s engine default prints bare
/// `REFERENCE` for every ignore spelling (rule 6). `OVERWRITE` rides
/// the head like every statement (the spec examples omit it for
/// readability).
fn emit_field(out: &mut String, field: &crate::ast::Field) {
    let mut sk = crate::pretty::Skeleton::new();
    let mut head = String::new();
    head.push_str("DEFINE FIELD OVERWRITE ");
    emit_field_path(&mut head, &field.name);
    head.push_str(" ON ");
    emit_ident(&mut head, &field.on);
    if let Some(kind) = &field.kind {
        head.push_str(" TYPE ");
        // Kinds pass through verbatim per README rule 4 (the source
        // spelling re-parses identically — upstream's printer
        // normalizes `option<T>` to `none | T`, SDL does not adopt
        // that). FLEXIBLE glues after TYPE (the engine's own fixed
        // position).
        head.push_str(&kind.text);
        if field.flexible {
            head.push_str(" FLEXIBLE");
        }
    }
    if field.readonly {
        head.push_str(" READONLY");
    }
    // INLINE glues after READONLY (upstream's fixed relative order —
    // the printer the reverse reader walks).
    if field.inline {
        head.push_str(" INLINE");
    }
    sk.push_head_text(&head);
    if let Some(value) = &field.value {
        sk.clause(" VALUE ", vec![crate::pretty::Seg::value(&value.canon)]);
    }
    if let Some(assert) = &field.assert {
        sk.clause(" ASSERT ", vec![crate::pretty::Seg::value(&assert.canon)]);
    }
    match &field.default {
        Some(FieldDefault::Set(expr)) => {
            sk.clause(" DEFAULT ", vec![crate::pretty::Seg::value(&expr.canon)]);
        }
        Some(FieldDefault::Always(expr)) => {
            sk.clause(
                " DEFAULT ALWAYS ",
                vec![crate::pretty::Seg::value(&expr.canon)],
            );
        }
        None => {}
    }
    if let Some(computed) = &field.computed {
        sk.clause(
            " COMPUTED ",
            vec![crate::pretty::Seg::value(&computed.canon)],
        );
    }
    if let Some(on_delete) = &field.reference {
        let mut parts = vec![crate::pretty::Seg::text("REFERENCE")];
        match on_delete {
            OnDelete::Ignore => {}
            OnDelete::Reject => parts.push(crate::pretty::Seg::text(" ON DELETE REJECT")),
            OnDelete::Cascade => parts.push(crate::pretty::Seg::text(" ON DELETE CASCADE")),
            OnDelete::Unset => parts.push(crate::pretty::Seg::text(" ON DELETE UNSET")),
            OnDelete::Then(expr) => {
                parts.push(crate::pretty::Seg::text(" ON DELETE THEN "));
                parts.push(crate::pretty::Seg::value(&expr.canon));
            }
        }
        // The bare `REFERENCE` head is part of the clause's own
        // spelling, not its key.
        sk.clause("", parts);
    }
    if let Some(permissions) = &field.permissions {
        let mut parts = Vec::new();
        op_permissions_parts(&mut parts, permissions);
        sk.clause(" PERMISSIONS ", parts);
    }
    if let Some(graphql) = &field.graphql {
        emit_graphql_clauses(&mut sk, graphql);
    }
    if let Some(comment) = &field.comment {
        let mut quoted = String::new();
        emit_string(&mut quoted, &comment.joined());
        sk.clause(" COMMENT ", vec![crate::pretty::Seg::text(quoted)]);
    }
    sk.write(out, crate::pretty::DEFAULT_MAX_WIDTH);
    out.push('\n');
}

/// A field name path: `.`-joined segments, each through the ordinary
/// ident rule, the wildcard bare (`tags.*` — SurrealQL's own wildcard
/// print).
fn emit_field_path(out: &mut String, path: &FieldPath) {
    for (i, segment) in path.segments.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        match segment {
            FieldPathSeg::Name(name) => emit_ident(out, name),
            FieldPathSeg::Wildcard => out.push('*'),
        }
    }
}

/// A SurrealQL string literal, after v3.2.4 `QuoteStr` — see
/// [`crate::keywords::quoted_string`], the shared home the SDL writer
/// rides too.
fn emit_string(out: &mut String, value: &str) {
    crate::keywords::quoted_string(out, value);
}

/// An identifier: bare when it is a plain non-keyword word, else
/// backtick-escaped (with the [`escape_with`] table, backtick as the
/// escaped character). See the keyword table below for what counts as
/// a collision.
fn emit_ident(out: &mut String, ident: &Ident) {
    if ident_needs_escape(&ident.raw) {
        out.push('`');
        escape_with(out, &ident.raw, '`');
        out.push('`');
    } else {
        out.push_str(&ident.raw);
    }
}

/// A `$`-parameter name: the `$` plus the interior (the shape rule —
/// see [`emit_word_shaped`]).
fn emit_param_name(out: &mut String, name: &crate::ast::ParamName) {
    out.push('$');
    emit_word_shaped(out, &name.raw);
}

/// A word position governed by upstream's `EscapeKwFreeIdent` shape
/// rule only (param names after `$`, `<fn-path>` segments): a plain
/// word emits bare — keyword collisions stay bare (`$table`,
/// `fn::class`, probe-verified) — anything else backtick-escaped,
/// through the shared [`crate::keywords::word_shaped`] predicate.
fn emit_word_shaped(out: &mut String, raw: &str) {
    if word_shaped(raw) {
        out.push_str(raw);
    } else {
        out.push('`');
        escape_with(out, raw, '`');
        out.push('`');
    }
}

/// An `<fn-path>`: the `fn`/`mod` prefix (absent on the bare heads
/// only api `middleware:` takes), then the `::`-joined segments, each
/// through the shape rule (upstream prints segments with
/// `EscapeKwFreeIdent`).
fn emit_fn_path(out: &mut String, path: &FnPath) {
    let prefix = match path.kind {
        FnKind::Fn => Some("fn"),
        FnKind::Mod => Some("mod"),
        FnKind::Bare => None,
    };
    if let Some(prefix) = prefix {
        out.push_str(prefix);
        out.push_str("::");
    }
    for (i, segment) in path.segments.iter().enumerate() {
        if i > 0 {
            out.push_str("::");
        }
        emit_word_shaped(out, segment);
    }
}

/// A filter atom, after the server emitter's own spellings: the
/// ngrams' arguments without an inner space (`EDGENGRAM(2,10)`), the
/// language as its uppercased full name, the mapper path re-quoted.
fn emit_filter(out: &mut String, filter: &Filter) {
    match filter {
        Filter::Ascii => out.push_str("ASCII"),
        Filter::Lowercase => out.push_str("LOWERCASE"),
        Filter::Uppercase => out.push_str("UPPERCASE"),
        Filter::EdgeNgram(min, max) => {
            out.push_str(&format!("EDGENGRAM({min},{max})"));
        }
        Filter::Ngram(min, max) => {
            out.push_str(&format!("NGRAM({min},{max})"));
        }
        Filter::Snowball(language) => {
            out.push_str("SNOWBALL(");
            out.push_str(language.value.as_str());
            out.push(')');
        }
        Filter::Mapper(path) => {
            out.push_str("MAPPER(");
            emit_string(out, path);
            out.push(')');
        }
    }
}

/// An `index` kind clause payload, minimal form (spec/index.md's Emit
/// block): kind params in the spec grammar's own declared order —
/// `HNSW DIMENSION … DISTANCE … TYPE … M … M0 … LM … EFC …` — with
/// the absent params eliding. The codegen model's spellings:
/// `DISTANCE` (the server's printed alias is `DIST`; both parse, per
/// the spec emit paragraph) and bare floats (`1.3`, `MINKOWSKI 3`)
/// where the server's `Number` print carries an `f` suffix (`1.3f`) —
/// both spellings re-parse to identical ASTs; the differ compares
/// ASTs. Empty output for the plain kind (the caller skips the
/// boundary).
fn index_kind_parts(out: &mut Vec<crate::pretty::Seg>, kind: &IndexKind) {
    match kind {
        IndexKind::Plain => {}
        IndexKind::Unique => out.push(crate::pretty::Seg::text("UNIQUE")),
        IndexKind::Count { where_ } => {
            out.push(crate::pretty::Seg::text("COUNT"));
            if let Some(expr) = where_ {
                out.push(crate::pretty::Seg::text(" WHERE "));
                out.push(crate::pretty::Seg::value(&expr.canon));
            }
        }
        IndexKind::FullText {
            analyzer,
            bm25,
            highlights,
        } => {
            out.push(crate::pretty::Seg::text("FULLTEXT"));
            if let Some(analyzer) = analyzer {
                let mut quoted = String::new();
                emit_ident(&mut quoted, analyzer);
                out.push(crate::pretty::Seg::text(format!(" ANALYZER {quoted}")));
            }
            if let Some(pair) = bm25 {
                // The server emitter's own `BM25({},{})` shape — comma
                // without a space — over f32 values (upstream's
                // `Scoring` storage). The spec example's `BM25(1.2,
                // 0.75)` spacing is illustrative, like its omitted
                // OVERWRITE; byte parity is with the server writer.
                out.push(crate::pretty::Seg::text(format!(
                    " BM25({},{})",
                    pair.0, pair.1
                )));
            }
            if *highlights {
                out.push(crate::pretty::Seg::text(" HIGHLIGHTS"));
            }
        }
        IndexKind::Hnsw {
            dimension,
            distance,
            vector_type,
            m,
            m0,
            lm,
            efc,
            extend_candidates,
            keep_pruned_connections,
            hashed_vector,
        } => {
            out.push(crate::pretty::Seg::text(format!(
                "HNSW DIMENSION {dimension}"
            )));
            distance_parts(out, *distance);
            if let Some(vector_type) = vector_type {
                out.push(crate::pretty::Seg::text(format!(
                    " TYPE {}",
                    vector_type.as_str()
                )));
            }
            if let Some(m) = m {
                out.push(crate::pretty::Seg::text(format!(" M {m}")));
            }
            if let Some(m0) = m0 {
                out.push(crate::pretty::Seg::text(format!(" M0 {m0}")));
            }
            if let Some(lm) = lm {
                out.push(crate::pretty::Seg::text(format!(" LM {}", lm.0)));
            }
            if let Some(efc) = efc {
                out.push(crate::pretty::Seg::text(format!(" EFC {efc}")));
            }
            if *extend_candidates {
                out.push(crate::pretty::Seg::text(" EXTEND_CANDIDATES"));
            }
            if *keep_pruned_connections {
                out.push(crate::pretty::Seg::text(" KEEP_PRUNED_CONNECTIONS"));
            }
            if *hashed_vector {
                out.push(crate::pretty::Seg::text(" HASHED_VECTOR"));
            }
        }
        IndexKind::DiskAnn {
            dimension,
            distance,
            vector_type,
            degree,
            l_build,
            alpha,
            hashed_vector,
        } => {
            out.push(crate::pretty::Seg::text(format!(
                "DISKANN DIMENSION {dimension}"
            )));
            distance_parts(out, *distance);
            if let Some(vector_type) = vector_type {
                out.push(crate::pretty::Seg::text(format!(
                    " TYPE {}",
                    vector_type.as_str()
                )));
            }
            if let Some(degree) = degree {
                out.push(crate::pretty::Seg::text(format!(" DEGREE {degree}")));
            }
            if let Some(l_build) = l_build {
                out.push(crate::pretty::Seg::text(format!(" L_BUILD {l_build}")));
            }
            if let Some(alpha) = alpha {
                out.push(crate::pretty::Seg::text(format!(" ALPHA {}", alpha.0)));
            }
            if *hashed_vector {
                out.push(crate::pretty::Seg::text(" HASHED_VECTOR"));
            }
        }
    }
}

/// A `DISTANCE` value when present (rule 9: the kind's default
/// elides); minkowski emits the bare order — `MINKOWSKI 3`, no
/// parens (spec/index.md, "Emit").
fn distance_parts(out: &mut Vec<crate::pretty::Seg>, distance: Option<Distance>) {
    let Some(distance) = distance else { return };
    if let Distance::Minkowski(order) = distance {
        out.push(crate::pretty::Seg::text(format!(
            " DISTANCE MINKOWSKI {}",
            order.0
        )));
    } else {
        out.push(crate::pretty::Seg::text(format!(
            " DISTANCE {}",
            distance.as_str()
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Authored, Carried, DocComment, Namespace};
    use surrealdb_core::syn;

    /// The skeleton model's load-bearing property: a BROKEN emission
    /// (the one-line spelling past the budget lays clause-per-line) is
    /// a fixed point of emit ∘ parse — the embedded parser reads the
    /// clause-per-line DDL back to the same AST, and the re-emission is
    /// byte-identical. Every clause family that breaks rides this test.
    #[test]
    fn broken_skeletons_are_emit_parse_emit_fixed_points() {
        for sdl in [
            // field: VALUE + ASSERT + COMMENT over the budget — the
            // issue-#1 repro shape (the value's chains break under
            // their clauses).
            "table invoice {\n\
             \x20 field status: string {\n\
             \x20   value: $value OR 'pending submission pipeline stage' OR 'approved by regional manager' OR 'rejected';\n\
             \x20   assert: $value = NONE OR $value INSIDE ['pending submission pipeline stage', 'approved', 'rejected'];\n\
             \x20 };\n\
             };",
            // user: PASSHASH/ROLES/DURATION.
            "user deployer { on: ns; passhash: '$argon2id$v=19$'; \
             roles: editor, viewer; duration { token: 1h; session: 12h; }; };",
            // access: TYPE/BEARER/DURATION.
            "access ci { on: db; type: bearer; for: user; \
             duration { grant: 4w; token: 1d; }; };",
            // analyzer: TOKENIZERS/FILTERS.
            "analyzer english { tokenizers: class; \
             filters: snowball(english), lowercase; };",
            // table: PERMISSIONS groups.
            "table `user` { schemafull; permissions { select: full; \
             update: $auth.id = id; }; };",
        ] {
            let (ast, diags) = crate::parser::parse_file(sdl);
            assert!(diags.is_empty(), "{sdl}: {diags:?}");
            let once = emit(&ast);
            assert!(
                once.contains('\n'),
                "the fixture must actually break: {once}"
            );
            // The embedded parser reads the clause-per-line DDL (the
            // breaks are whitespace); the full DDL→SDL→DDL fixed point
            // is the oracle layers'.
            validate_ddl(&once).expect("the broken DDL parses");
            let twice = emit(&ast);
            assert_eq!(once, twice, "the emission must be deterministic: {sdl}");
        }
    }

    /// The stage-2 oracle accepts what `emit` produces for a
    /// representative file — every statement family's emission rides
    /// the full corpus; this pins the wrapper itself end to end.
    #[test]
    fn validate_ddl_accepts_emitted_ddl() {
        let (ast, diags) = crate::parser::parse_file(
            "namespace acme;\n\
             database shop;\n\
             table person {\n\
             \x20 schemafull;\n\
             \x20 field name: string;\n\
             \x20 field email: string { assert: string::is_email($value); };\n\
             \x20 index unique email_unique {\n\
             \x20   fields: email;\n\
             \x20 };\n\
             };\n",
        );
        assert!(diags.is_empty(), "the source parses");
        let ddl = emit(&ast);
        validate_ddl(&ddl).expect("the emitted DDL parses");
    }

    /// And rejects DDL the embedded parser cannot read, with the
    /// upstream render as the message.
    #[test]
    fn validate_ddl_rejects_broken_ddl() {
        let err = validate_ddl("DEFINE BOGUS thing;").expect_err("rejects");
        assert!(!err.is_empty());
    }

    #[test]
    fn escape_rules_match_upstream_quote_str() {
        let mut out = String::new();
        emit_string(&mut out, "plain");
        assert_eq!(out, "'plain'");
        // Apostrophe switches the quote character, not escaped.
        let mut out = String::new();
        emit_string(&mut out, "it's");
        assert_eq!(out, "\"it's\"");
        // The active quote is backslashed; the other is verbatim.
        let mut out = String::new();
        emit_string(&mut out, "say \"hi\" 'ok'");
        assert_eq!(out, "\"say \\\"hi\\\" 'ok'\"");
        let mut out = String::new();
        emit_string(&mut out, "it\\'s");
        assert_eq!(out, "\"it\\\\'s\"");
        // The control-character table.
        let mut out = String::new();
        emit_string(&mut out, "a\u{0}b\rc\td\ne\u{8}f\u{c}g");
        assert_eq!(out, "'a\\0b\\rc\\td\\ne\\u{8}f\\fg'");
    }

    #[test]
    fn ident_emission_backticks_on_collision_and_shape() {
        let case = |raw: &str| {
            let mut out = String::new();
            emit_ident(&mut out, &Ident::new(raw));
            out
        };
        assert_eq!(case("acme"), "acme");
        assert_eq!(case("table"), "`table`");
        assert_eq!(case("SELECT"), "`SELECT`");
        assert_eq!(case("NaN"), "`NaN`");
        assert_eq!(case("9lives"), "`9lives`");
        assert_eq!(case("weird name"), "`weird name`");
        assert_eq!(case("a`b"), "`a\\`b`");
        assert_eq!(case("a\\b"), "`a\\\\b`");
    }

    /// Every golden emission must also parse through the embedded
    /// v3.2.4 parser — the layer-2 oracle, inline where the DDL is
    /// born.
    #[test]
    fn emitted_namespace_ddl_parses() {
        let file = SdlFile {
            items: vec![Stmt::Namespace(Namespace {
                name: Ident::new("acme"),
                keyword: Carried::default(),
                comment: Some(DocComment {
                    lines: vec!["Acme Corporation".into()],
                }),
                comments: Vec::new(),
                clause_comments: Vec::new(),
            })],
            trailing_comments: Vec::new(),
        };
        let ddl = emit(&file);
        assert_eq!(
            ddl,
            "DEFINE NAMESPACE OVERWRITE acme COMMENT 'Acme Corporation';\nUSE NS acme;\n"
        );
        syn::parse(&ddl).expect("emitted DDL must parse");
    }

    /// Insta goldens for the spec's examples, OVERWRITE/USE-corrected
    /// to the ground truth. Inputs are hand-built ASTs so
    /// a golden failure attributes to codegen alone; the full
    /// SDL→DDL→oracle chain is covered by the CLI and oracle tests.
    fn golden(name: &str, name_raw: &str, lines: &[&str]) {
        let file = SdlFile {
            items: vec![Stmt::Namespace(Namespace {
                name: Ident::new(name_raw),
                keyword: Carried::default(),
                comment: (!lines.is_empty()).then(|| DocComment {
                    lines: lines.iter().map(|l| l.to_string()).collect(),
                }),
                comments: Vec::new(),
                clause_comments: Vec::new(),
            })],
            trailing_comments: Vec::new(),
        };
        let ddl = emit(&file);
        syn::parse(&ddl).expect("emitted DDL must parse");
        insta::assert_snapshot!(name, ddl);
    }

    /// spec/namespace.md: the doc-comment example.
    #[test]
    fn golden_doc_comment_namespace() {
        golden("codegen-namespace-doc", "acme", &["Acme Corporation"]);
    }

    /// spec/namespace.md: the bare example (the empty-block form
    /// parses to the same AST).
    #[test]
    fn golden_bare_namespace() {
        golden("codegen-namespace-bare", "staging", &[]);
    }

    /// Multi-line docs join with `\n`; escaping corpus on one line
    /// (apostrophe flips the quote character; controls use the table).
    #[test]
    fn golden_escaping_corpus() {
        golden(
            "codegen-namespace-escaping",
            "acme",
            &["Acme's \"premier\" line", "second\tline — with a ' quote"],
        );
    }

    /// A keyword-colliding name emits backticked — in both the DEFINE
    /// and the session USE.
    #[test]
    fn golden_keyword_collision() {
        golden("codegen-namespace-collision", "table", &[]);
    }

    // ---- sequence and param ----

    use crate::ast::{Expr, Param, ParamName, Permission, Sequence};

    /// Insta goldens for the spec's examples, OVERWRITE-corrected to
    /// the ground truth. Hand-built ASTs so a failure
    /// attributes to codegen alone.
    fn sequence(
        name: &str,
        start: Option<&str>,
        batch: Option<&str>,
        timeout: Option<&str>,
    ) -> Stmt {
        Stmt::Sequence(Sequence {
            name: Ident::new(name),
            start: start.map(|canon| Expr {
                canon: canon.to_string(),
            }),
            batch: batch.map(|canon| Expr {
                canon: canon.to_string(),
            }),
            timeout: timeout.map(|canon| Expr {
                canon: canon.to_string(),
            }),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })
    }

    fn param(name: &str, value: Option<&str>, permissions: Option<Permission>) -> Stmt {
        Stmt::Param(Param {
            name: ParamName::new(name),
            value: value.map(|canon| Expr {
                canon: canon.to_string(),
            }),
            permissions,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })
    }

    fn golden_stmts(name: &str, items: Vec<Stmt>) {
        let file = SdlFile {
            items,
            trailing_comments: Vec::new(),
        };
        let ddl = emit(&file);
        syn::parse(&ddl).expect("emitted DDL must parse");
        insta::assert_snapshot!(name, ddl);
    }

    /// spec/sequence.md: `sequence invoice_number = 1000;` — the
    /// short form emits START (the assignment's target clause).
    #[test]
    fn golden_sequence_short_form() {
        golden_stmts(
            "codegen-sequence-short",
            vec![sequence("invoice_number", Some("1000"), None, None)],
        );
    }

    /// spec/sequence.md: bare form — all defaults elide entirely.
    #[test]
    fn golden_sequence_bare() {
        golden_stmts(
            "codegen-sequence-bare",
            vec![sequence("row_ids", None, None, None)],
        );
    }

    /// spec/sequence.md: the block example — BATCH → START → TIMEOUT
    /// regardless of SDL clause order.
    #[test]
    fn golden_sequence_block() {
        golden_stmts(
            "codegen-sequence-block",
            vec![sequence(
                "order_number",
                Some("5000"),
                Some("100"),
                Some("5s"),
            )],
        );
    }

    /// spec/param.md: the doc-comment example (VALUE → COMMENT).
    #[test]
    fn golden_param_short_with_doc() {
        golden_stmts(
            "codegen-param-short-doc",
            vec![Stmt::Param(Param {
                name: ParamName::new("default_limit"),
                value: Some(Expr {
                    canon: "50".to_string(),
                }),
                permissions: None,
                comment: Some(DocComment {
                    lines: vec!["Default page size for list endpoints".into()],
                }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })],
        );
    }

    /// spec/param.md: block example — VALUE, then PERMISSIONS NONE.
    #[test]
    fn golden_param_block_permissions_none() {
        golden_stmts(
            "codegen-param-internal-seed",
            vec![param("internal_seed", Some("42"), Some(Permission::None))],
        );
    }

    /// The permission expression arm emits `PERMISSIONS WHERE`; the
    /// explicit `full` keyword arm emits `FULL`.
    #[test]
    fn golden_param_permissions() {
        golden_stmts(
            "codegen-param-permissions",
            vec![
                param(
                    "editor_only",
                    Some("true"),
                    Some(Permission::Expr(Expr {
                        canon: "level > 10".to_string(),
                    })),
                ),
                param("explicit_full", None, Some(Permission::Full)),
            ],
        );
    }

    /// A bare param (no value, no permissions) emits VALUE-less —
    /// SurrealQL stores VALUE NONE server-side (rule 9: omission
    /// parity). A keyword-colliding name emits bare after `$`,
    /// mirroring upstream's `EscapeKwFreeIdent`.
    #[test]
    fn golden_param_bare_and_keyword_name() {
        golden_stmts(
            "codegen-param-bare",
            vec![param("x", None, None), param("table", None, None)],
        );
    }

    /// The constant-literal spellings `NaN`/`Infinity` emit
    /// backticked after `$` (upstream's shape rule), while keyword
    /// collisions stay bare.
    #[test]
    fn param_name_constant_spellings_emit_backticked() {
        let file = SdlFile {
            items: vec![param("NaN", None, None), param("Infinity", None, None)],
            trailing_comments: Vec::new(),
        };
        let ddl = emit(&file);
        assert_eq!(
            ddl,
            "DEFINE PARAM OVERWRITE $`NaN`;\nDEFINE PARAM OVERWRITE $`Infinity`;\n"
        );
        syn::parse(&ddl).expect("emitted DDL must parse");
    }

    /// Re-print normalization from *parsed* SDL: comments inside
    /// captures dropped, whitespace canonicalized, statement-like
    /// expressions wrapped in `CoverStmts` parens, keyword casing
    /// normalized — the embedded printer is the only authority.
    #[test]
    fn golden_reprint_normalization() {
        let cases = [
            (
                "codegen-reprint-sequence-comment",
                "sequence n = 1  /* one */  +\n1;",
            ),
            ("codegen-reprint-param-create", "param $x = create user;"),
            (
                "codegen-reprint-param-select",
                "param $x = select * from user;",
            ),
            (
                "codegen-reprint-param-braced",
                "param $x = { string::split('1.2.3', '.')[0] };",
            ),
        ];
        for (name, sdl) in cases {
            let (file, diags) = crate::parser::parse_file(sdl);
            assert!(diags.is_empty(), "{sdl}: {diags:?}");
            let ddl = emit(&file);
            syn::parse(&ddl).expect("emitted DDL must parse");
            insta::assert_snapshot!(name, ddl);
        }
    }

    // ---- database and bucket ----

    use crate::ast::{Bucket, ChangeFeed, Database};

    /// `DEFINE BUCKET` is gated *inside* the parser (`files_enabled`;
    /// plain `syn::parse` rejects it), so bucket
    /// emissions are validated with the experimental capabilities
    /// enabled — the one construction
    /// [`crate::expr::experimental_capabilities`] owns.
    fn syn_parse_with_files(ddl: &str) {
        use surrealdb_syn::ParserConfig;
        let capabilities = crate::expr::experimental_capabilities();
        surrealdb_core::syn::parse_with_capabilities(ddl, &capabilities, &ParserConfig::default())
            .expect("emitted DDL must parse");
    }

    fn database(name: &str, strict: bool, changefeed: Option<ChangeFeed>, lines: &[&str]) -> Stmt {
        Stmt::Database(Database {
            name: Ident::new(name),
            keyword: Carried::default(),
            strict,
            changefeed,
            comment: (!lines.is_empty()).then(|| DocComment {
                lines: lines.iter().map(|l| l.to_string()).collect(),
            }),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })
    }

    fn bucket(
        name: &str,
        backend: Option<&str>,
        readonly: bool,
        permissions: Option<Permission>,
        lines: &[&str],
    ) -> Stmt {
        Stmt::Bucket(Bucket {
            name: Ident::new(name),
            backend: backend.map(|canon| Expr {
                canon: canon.to_string(),
            }),
            readonly,
            permissions,
            comment: (!lines.is_empty()).then(|| DocComment {
                lines: lines.iter().map(|l| l.to_string()).collect(),
            }),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })
    }

    fn golden_stmts_files(name: &str, items: Vec<Stmt>) {
        let file = SdlFile {
            items,
            trailing_comments: Vec::new(),
        };
        let ddl = emit(&file);
        syn_parse_with_files(&ddl);
        insta::assert_snapshot!(name, ddl);
    }

    /// spec/database.md: the doc-comment example.
    #[test]
    fn golden_database_doc() {
        golden_stmts(
            "codegen-database-doc",
            vec![database(
                "blog",
                false,
                None,
                &["Primary application database"],
            )],
        );
    }

    /// spec/database.md: the strict example.
    #[test]
    fn golden_database_strict() {
        golden_stmts(
            "codegen-database-strict",
            vec![database("blog", true, None, &[])],
        );
    }

    /// spec/database.md's changefeed example — the spec's canonical
    /// emit order CHANGEFEED → STRICT → COMMENT (the server's own
    /// printer orders STRICT, COMMENT, CHANGEFEED; both parse). The
    /// stored duration prints canonically: SDL's `7d` is one week of
    /// seconds, and the greedy y→w→d decomposition prints `1w` —
    /// value-preserving, exactly the re-decomposition the spec's
    /// reverse-parser section describes.
    #[test]
    fn golden_database_changefeed() {
        golden_stmts(
            "codegen-database-changefeed",
            vec![database(
                "blog",
                false,
                Some(ChangeFeed {
                    expiry: surrealdb_types::Duration::from_secs(7 * 86_400),
                    include_original: true,
                }),
                &[],
            )],
        );
    }

    /// The stored duration prints the server's own canonical compound
    /// form through `surrealdb_types::Duration`'s ToSql — the printer
    /// the server itself runs: `90m` → `1h30m`, `370d` → `1y5d`,
    /// `2500ns` → `2µs500ns`, zero → `0ns`.
    #[test]
    fn golden_database_duration_canonical_print() {
        golden_stmts(
            "codegen-database-duration-canonical",
            vec![
                database(
                    "d1",
                    false,
                    Some(ChangeFeed {
                        expiry: surrealdb_types::Duration::from_secs(5_400),
                        include_original: false,
                    }),
                    &[],
                ),
                database(
                    "d2",
                    false,
                    Some(ChangeFeed {
                        expiry: surrealdb_types::Duration::from_secs(370 * 86_400),
                        include_original: false,
                    }),
                    &[],
                ),
                database(
                    "d3",
                    false,
                    Some(ChangeFeed {
                        expiry: surrealdb_types::Duration::from_nanos(2_500),
                        include_original: false,
                    }),
                    &[],
                ),
                database(
                    "d4",
                    false,
                    Some(ChangeFeed {
                        expiry: surrealdb_types::Duration::ZERO,
                        include_original: false,
                    }),
                    &[],
                ),
            ],
        );
    }

    /// Session context (codegen model item 1): a database emits
    /// `USE NS <ns> DB <db>;` bound to the nearest preceding
    /// namespace; two ns/db pairs each bind to their own.
    #[test]
    fn golden_database_binds_to_nearest_namespace() {
        golden_stmts(
            "codegen-database-use-ns-db",
            vec![
                Stmt::Namespace(Namespace {
                    name: Ident::new("one"),
                    keyword: Carried::default(),
                    comment: None,
                    comments: Vec::new(),
                    clause_comments: Vec::new(),
                }),
                database("first", false, None, &[]),
                Stmt::Namespace(Namespace {
                    name: Ident::new("two"),
                    keyword: Carried::default(),
                    comment: None,
                    comments: Vec::new(),
                    clause_comments: Vec::new(),
                }),
                database("second", true, None, &[]),
            ],
        );
    }

    /// No preceding namespace — the DEFINE emits alone, with no USE
    /// line and no diagnostic (amended codegen model item 1:
    /// definitions stand alone; convergence and diffing do not
    /// require session context).
    #[test]
    fn golden_database_without_namespace_emits_without_use() {
        let ddl = emit(&SdlFile {
            items: vec![database("standalone", false, None, &[])],
            trailing_comments: Vec::new(),
        });
        assert_eq!(ddl, "DEFINE DATABASE OVERWRITE standalone;\n");
        syn::parse(&ddl).expect("emitted DDL must parse");
    }

    /// Keyword-colliding names backtick in the DEFINE and in the
    /// session USE line alike.
    #[test]
    fn golden_database_keyword_collision() {
        golden_stmts(
            "codegen-database-collision",
            vec![
                Stmt::Namespace(Namespace {
                    name: Ident::new("table"),
                    keyword: Carried::default(),
                    comment: None,
                    comments: Vec::new(),
                    clause_comments: Vec::new(),
                }),
                database("select", false, None, &[]),
            ],
        );
    }

    /// spec/bucket.md: the memory-backend example with its doc
    /// comment.
    #[test]
    fn golden_bucket_backend_doc() {
        golden_stmts_files(
            "codegen-bucket-backend-doc",
            vec![bucket(
                "scratch_uploads",
                Some("'memory'"),
                false,
                None,
                &["Ephemeral test uploads"],
            )],
        );
    }

    /// spec/bucket.md: readonly + backend — the canonical order
    /// READONLY before BACKEND (the server's own order).
    #[test]
    fn golden_bucket_readonly_backend() {
        golden_stmts_files(
            "codegen-bucket-readonly-backend",
            vec![bucket(
                "published_reports",
                Some("'memory'"),
                true,
                None,
                &[],
            )],
        );
    }

    /// spec/bucket.md: the permissions expression arm emits
    /// `PERMISSIONS WHERE`; an explicit `full` emits `FULL`. Absent
    /// permissions elide (SurrealQL's FULL default, rule 9).
    #[test]
    fn golden_bucket_permissions_and_elision() {
        golden_stmts_files(
            "codegen-bucket-permissions",
            vec![
                bucket(
                    "admin_dumps",
                    None,
                    false,
                    Some(Permission::Expr(Expr {
                        canon: "$auth.admin = true".to_string(),
                    })),
                    &[],
                ),
                bucket("explicit_full", None, false, Some(Permission::Full), &[]),
                bucket("shared", None, false, None, &[]),
            ],
        );
    }

    /// A file-backend spelling, and a keyword-colliding bucket name.
    #[test]
    fn golden_bucket_file_backend_and_collision() {
        golden_stmts_files(
            "codegen-bucket-file-backend",
            vec![
                bucket("user_files", Some("'file:/srv/uploads'"), false, None, &[]),
                bucket("select", None, true, None, &[]),
            ],
        );
    }

    // ---- user and analyzer ----

    use crate::ast::{
        Analyzer as AnalyzerStmt, Async, Event as EventStmt, Filter, FnKind, FnPath, Language,
        Level, Pass, Role, Tokenizer, User as UserStmt,
    };

    fn user(
        name: &str,
        on: Level,
        pass: Option<Pass>,
        roles: Vec<Role>,
        token: Option<&str>,
        session: Option<&str>,
        lines: &[&str],
    ) -> Stmt {
        Stmt::User(UserStmt {
            name: Ident::new(name),
            on: Authored::canonical(on),
            pass,
            scram: None,
            roles,
            token_duration: token.map(|canon| Expr {
                canon: canon.to_string(),
            }),
            session_duration: session.map(|canon| Expr {
                canon: canon.to_string(),
            }),
            comment: (!lines.is_empty()).then(|| DocComment {
                lines: lines.iter().map(|l| l.to_string()).collect(),
            }),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })
    }

    fn analyzer(
        name: &str,
        function: Option<FnPath>,
        tokenizers: Option<Vec<Tokenizer>>,
        filters: Option<Vec<Filter>>,
        lines: &[&str],
    ) -> Stmt {
        Stmt::Analyzer(AnalyzerStmt {
            name: Ident::new(name),
            function,
            tokenizers,
            filters,
            comment: (!lines.is_empty()).then(|| DocComment {
                lines: lines.iter().map(|l| l.to_string()).collect(),
            }),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })
    }

    fn event(
        name: &str,
        on: &str,
        when: Option<&str>,
        then: &[&str],
        r#async: Option<Async>,
        lines: &[&str],
    ) -> Stmt {
        Stmt::Event(EventStmt {
            name: Ident::new(name),
            on: Ident::new(on),
            when: when.map(|canon| Expr {
                canon: canon.to_string(),
            }),
            then: then
                .iter()
                .map(|canon| Expr {
                    canon: canon.to_string(),
                })
                .collect(),
            r#async,
            comment: (!lines.is_empty()).then(|| DocComment {
                lines: lines.iter().map(|l| l.to_string()).collect(),
            }),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })
    }

    /// spec/user.md: the passhash example — secret-carrying users
    /// converge with `OVERWRITE`; roles uppercased; the canonical
    /// order ON → PASSHASH → ROLES → DURATION → COMMENT with the
    /// comma joining the duration sides.
    #[test]
    fn golden_user_passhash() {
        golden_stmts(
            "codegen-user-passhash",
            vec![user(
                "deployer",
                Level::Namespace,
                Some(Pass::Hash("$argon2id$v=19$".to_string())),
                vec![Role::Editor, Role::Viewer],
                Some("1h"),
                Some("12h"),
                &["CI deployment account"],
            )],
        );
    }

    /// spec/user.md: the root owner with a plaintext password (the
    /// warning is the linter's; emission is `PASSWORD`).
    #[test]
    fn golden_user_password() {
        golden_stmts(
            "codegen-user-password",
            vec![user(
                "admin",
                Level::Root,
                Some(Pass::Password("change-me-immediately".to_string())),
                vec![Role::Owner],
                None,
                None,
                &[],
            )],
        );
    }

    /// spec/user.md: the define-once example — one of the codegen
    /// model's two `OVERWRITE` exceptions (the other is the sequence,
    /// spec/sequence.md): no secret ⇒ `IF NOT EXISTS`
    /// between `USER` and the name.
    #[test]
    fn golden_user_define_once() {
        golden_stmts(
            "codegen-user-define-once",
            vec![user(
                "analyst",
                Level::Database,
                None,
                vec![],
                None,
                None,
                &[],
            )],
        );
    }

    /// spec/user.md: the provisioned-SCRAM example — the 3.3
    /// additive clause. A scram-only user is secret-carrying
    /// (`OVERWRITE`, not define-once); PASSSCRAM prints after the
    /// pass clause, before ROLES (the engine emitter's order).
    #[test]
    fn golden_user_passscram() {
        // surrealdb-iam's KAT verifier — the value upstream's own
        // scram.surql test imports (single-quote-safe by construction).
        let scram =
            "SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20="
                .to_string();
        golden_stmts(
            "codegen-user-passscram",
            vec![
                Stmt::User(UserStmt {
                    name: Ident::new("provisioned"),
                    on: Authored::canonical(Level::Root),
                    pass: None,
                    scram: Some(scram.clone()),
                    roles: vec![],
                    token_duration: None,
                    session_duration: None,
                    comment: None,
                    comments: Vec::new(),
                    trailing_comments: Vec::new(),
                    clause_comments: Vec::new(),
                }),
                Stmt::User(UserStmt {
                    name: Ident::new("migrator"),
                    on: Authored::canonical(Level::Database),
                    pass: Some(Pass::Hash("$argon2id$v=19$abc".to_string())),
                    scram: Some(scram),
                    roles: vec![],
                    token_duration: None,
                    session_duration: None,
                    comment: None,
                    comments: Vec::new(),
                    trailing_comments: Vec::new(),
                    clause_comments: Vec::new(),
                }),
            ],
        );
    }

    /// Absent roles and durations elide (rule 9 — upstream's parser
    /// seeds `VIEWER` and the `1h`/`none` defaults); a secret-less
    /// user WITH roles still emits `IF NOT EXISTS`; `on: kv;`
    /// emits the canonical full word.
    #[test]
    fn golden_user_elision_and_alias() {
        golden_stmts(
            "codegen-user-elision",
            vec![
                user(
                    "bot",
                    Level::Root,
                    None,
                    vec![Role::Owner],
                    None,
                    // The canon spelling the embedded printer
                    // produces (`none` parses to the NONE sentinel).
                    Some("NONE"),
                    &[],
                ),
                user(
                    "deployer",
                    Level::Root,
                    Some(Pass::Hash("h".into())),
                    vec![],
                    Some("30m"),
                    None,
                    &[],
                ),
            ],
        );
    }

    /// spec/analyzer.md: the english example — the canonical order
    /// FUNCTION → TOKENIZERS → FILTERS → COMMENT, the full-name
    /// language, and the server emitter's `", "` filter joiner.
    #[test]
    fn golden_analyzer_english() {
        golden_stmts(
            "codegen-analyzer-english",
            vec![analyzer(
                "english",
                None,
                Some(vec![Tokenizer::Class]),
                Some(vec![
                    Filter::Snowball(Authored::canonical(Language::English)),
                    Filter::Lowercase,
                ]),
                &["English text search"],
            )],
        );
    }

    /// spec/analyzer.md: the parameterized atoms (the server prints
    /// `EDGENGRAM(2,10)` without an inner space) and the
    /// comma-without-space tokenizer joiner.
    #[test]
    fn golden_analyzer_parameterized_filters() {
        golden_stmts(
            "codegen-analyzer-filters",
            vec![
                analyzer(
                    "autocomplete",
                    None,
                    None,
                    Some(vec![Filter::Lowercase, Filter::EdgeNgram(2, 10)]),
                    &[],
                ),
                analyzer(
                    "fuzzy",
                    None,
                    None,
                    Some(vec![Filter::Ascii, Filter::Ngram(3, 5)]),
                    &[],
                ),
                analyzer(
                    "domain",
                    None,
                    None,
                    Some(vec![Filter::Mapper("/etc/surreal/domain-map.txt".into())]),
                    &[],
                ),
                analyzer(
                    "raw_split",
                    None,
                    Some(vec![Tokenizer::Camel, Tokenizer::Punct]),
                    None,
                    &[],
                ),
            ],
        );
    }

    /// The `<fn-path>` arms: multi-segment `fn::`, keyword-like
    /// segments bare, shape-escaped segments backticked — and the
    /// `mod::` prefix, which is capability-gated at parse time like
    /// `DEFINE BUCKET`, so this golden validates through the
    /// capabilities-enabled oracle.
    #[test]
    fn golden_analyzer_fn_paths() {
        golden_stmts_files(
            "codegen-analyzer-fn-paths",
            vec![
                analyzer(
                    "preprocessed",
                    Some(FnPath {
                        kind: FnKind::Fn,
                        segments: vec!["strip_markup".to_string()],
                    }),
                    Some(vec![Tokenizer::Blank]),
                    Some(vec![Filter::Lowercase]),
                    &[],
                ),
                analyzer(
                    "pathed",
                    Some(FnPath {
                        kind: FnKind::Fn,
                        segments: vec![
                            "util".to_string(),
                            "class".to_string(),
                            "9lives".to_string(),
                        ],
                    }),
                    None,
                    None,
                    &[],
                ),
                analyzer(
                    "moduled",
                    Some(FnPath {
                        kind: FnKind::Mod,
                        segments: vec!["demo".to_string(), "some".to_string()],
                    }),
                    None,
                    None,
                    &[],
                ),
            ],
        );
    }

    /// A keyword-colliding analyzer name backticks like any name
    /// position; the empty analyzer emits bare.
    #[test]
    fn golden_analyzer_collision_and_empty() {
        golden_stmts(
            "codegen-analyzer-collision",
            vec![
                analyzer("snowball", None, None, None, &[]),
                analyzer("empty", None, None, None, &[]),
            ],
        );
    }

    /// spec/event.md, example 1: when + then + doc comment — the
    /// canonical order ON → WHEN → THEN → COMMENT, the braced-then
    /// canon as the pipeline produces it (keywords uppercased; a
    /// single-statement block prints without a trailing `;`).
    #[test]
    fn golden_event_when_then_doc() {
        golden_stmts(
            "codegen-event-when-then-doc",
            vec![event(
                "user_created",
                "user",
                Some("$event = 'CREATE'"),
                &["{ CREATE log SET at = time::now(), user = $after.id }"],
                None,
                &["Log every user creation"],
            )],
        );
    }

    /// spec/event.md, example 2: then only — an absent `when` emits
    /// nothing (rule 9; upstream materializes `WHEN true`
    /// internally), and the statement-like item rides the
    /// CoverStmts parens.
    #[test]
    fn golden_event_then_only() {
        golden_stmts(
            "codegen-event-then-only",
            vec![event(
                "touch_counter",
                "post",
                None,
                &["(UPDATE counter:posts SET total = total + 1)"],
                None,
                &[],
            )],
        );
    }

    /// spec/event.md, example 3: the configured async block —
    /// `ASYNC RETRY 5 MAXDEPTH 8` glued after ON, ahead of WHEN. The
    /// canon is the pipeline's print: `rand` is a reserved keyword
    /// upstream, so the embedded printer backticks the fn-path head.
    #[test]
    fn golden_event_async_block() {
        golden_stmts(
            "codegen-event-async-block",
            vec![event(
                "webhook_fanout",
                "invoice",
                None,
                &["{ IF `rand`::bool() { THROW 'flaky downstream' } }"],
                Some(Async {
                    retry: Some(5),
                    maxdepth: Some(8),
                }),
                &[],
            )],
        );
    }

    /// spec/event.md, example 4: the multi-expression then —
    /// SurrealQL's `THEN a, b` form with the server emitter's
    /// `", "` joiner.
    #[test]
    fn golden_event_multi_then() {
        golden_stmts(
            "codegen-event-multi-then",
            vec![event(
                "double_bookkeeping",
                "payment",
                Some("$event = 'UPDATE' AND $before.status != $after.status"),
                &[
                    "(CREATE ledger_entry SET payment = $after.id)",
                    "(UPDATE stats:payments SET changes = changes + 1)",
                ],
                None,
                &[],
            )],
        );
    }

    /// The async group's faithful emission: the bare flag is a bare
    /// `ASYNC`, a partial block emits only the sides written
    /// (rule 9 — the unwritten side is SurrealQL's default), and
    /// both sides compress to the glued pair.
    #[test]
    fn golden_event_async_flag_and_partial() {
        golden_stmts(
            "codegen-event-async-flag-partial",
            vec![
                event(
                    "flagged",
                    "t",
                    None,
                    &["1"],
                    Some(Async {
                        retry: None,
                        maxdepth: None,
                    }),
                    &[],
                ),
                event(
                    "retry_only",
                    "t",
                    None,
                    &["1"],
                    Some(Async {
                        retry: Some(5),
                        maxdepth: None,
                    }),
                    &[],
                ),
                event(
                    "maxdepth_only",
                    "t",
                    None,
                    &["1"],
                    Some(Async {
                        retry: None,
                        maxdepth: Some(8),
                    }),
                    &[],
                ),
                event("sync", "t", None, &["1"], None, &[]),
            ],
        );
    }

    // ---- module ----

    use crate::ast::{Executable, Module};

    /// Module goldens ride the parse oracle since the 3.3.0 floor
    /// move: the module grammar is real in the embed (capability-gated
    /// at parse time — `syn_parse_with_files` runs the experimental
    /// capabilities), so the emissions validate like every other
    /// family's. The EXEC side and the reverse reader stay blind
    /// (that half stays open).
    fn golden_stmts_module(name: &str, items: Vec<Stmt>) {
        let ddl = emit(&SdlFile {
            items,
            trailing_comments: Vec::new(),
        });
        syn_parse_with_files(&ddl);
        insta::assert_snapshot!(name, ddl);
    }

    fn module(
        name: Option<&str>,
        executable: Executable,
        permissions: Option<Permission>,
        lines: &[&str],
    ) -> Stmt {
        Stmt::Module(Module {
            name: name.map(Ident::new),
            executable,
            unsigned: true,
            permissions,
            comment: (!lines.is_empty()).then(|| DocComment {
                lines: lines.iter().map(|l| l.to_string()).collect(),
            }),
            // Hand-built ASTs carry no source; `from_span` rides the
            // AST shape only.
            from_span: crate::lexer::Span::point(0),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })
    }

    fn file_executable(path: &str) -> Executable {
        let (bucket, key) = path.split_once(':').expect("bucket:/key");
        Executable::File {
            bucket: bucket.to_string(),
            key: key.to_string(),
        }
    }

    /// spec/module.md, example 1: the named file module with its doc
    /// comment — `[mod::<name> FROM]`, the f-string in the server
    /// emitter's double-quoted spelling, UNSIGNED, COMMENT.
    #[test]
    fn golden_module_file_doc() {
        golden_stmts_module(
            "codegen-module-file-doc",
            vec![module(
                Some("color"),
                file_executable("modules:/color.surli"),
                None,
                &["Colour helpers"],
            )],
        );
    }

    /// spec/module.md, example 2: the self-named silo package — no
    /// alias, the 3.3 `::<version>` spelling, UNSIGNED, all optional
    /// clauses elided.
    #[test]
    fn golden_module_silo() {
        golden_stmts_module(
            "codegen-module-silo",
            vec![module(
                None,
                Executable::Silo {
                    org: "acme".to_string(),
                    pkg: "utils".to_string(),
                    major: 1,
                    minor: 2,
                    patch: 0,
                },
                None,
                &[],
            )],
        );
    }

    /// spec/module.md, example 3: the permissions expression arm —
    /// UNSIGNED then `PERMISSIONS WHERE` (the spec's canonical emit
    /// order; the 3.2.4 emitter prints PERMISSIONS unconditionally and
    /// last, but SDL's 3.3 output follows its own Emit table).
    #[test]
    fn golden_module_permissions() {
        golden_stmts_module(
            "codegen-module-permissions",
            vec![module(
                Some("admin_tools"),
                file_executable("modules:/admin.surli"),
                Some(Permission::Expr(Expr {
                    canon: "$auth.admin = true".to_string(),
                })),
                &[],
            )],
        );
    }

    /// Emission spellings: keyword-colliding module names and silo
    /// segments stay bare after `mod::`/`silo::` (upstream
    /// `parse_ident` positions, `EscapeKwFreeIdent` semantics);
    /// non-word shapes backtick; `permissions: none` spells out.
    #[test]
    fn golden_module_spellings() {
        golden_stmts_module(
            "codegen-module-spellings",
            vec![
                module(
                    Some("table"),
                    file_executable("b:/k"),
                    Some(Permission::None),
                    &[],
                ),
                module(
                    None,
                    Executable::Silo {
                        org: "9lives".to_string(),
                        pkg: "pkg".to_string(),
                        major: 0,
                        minor: 1,
                        patch: 2,
                    },
                    None,
                    &[],
                ),
            ],
        );
    }

    // ---- raw ----

    use crate::ast::Raw;

    fn raw_stmt(body: &str) -> Stmt {
        Stmt::Raw(Raw {
            body: body.to_string(),
            comments: Vec::new(),
            clause_comments: Vec::new(),
        })
    }

    /// Rule 8: verbatim means verbatim — the parsable class rides
    /// `golden_stmts` (the layer-2 oracle checks the emission).
    #[test]
    fn golden_raw_verbatim() {
        golden_stmts(
            "codegen-raw-verbatim",
            vec![raw_stmt(
                "// interior comments ride\nDEFINE PARAM OVERWRITE $weird VALUE '}no \
                 close{'; /* tail */",
            )],
        );
    }

    /// Raw statements keep their source-order slot between
    /// definitions; a whitespace-only body emits nothing.
    #[test]
    fn raw_keeps_source_order_and_elides_when_empty() {
        let file = SdlFile {
            items: vec![
                Stmt::Sequence(Sequence {
                    name: Ident::new("order_number"),
                    start: None,
                    batch: None,
                    timeout: None,
                    comments: Vec::new(),
                    trailing_comments: Vec::new(),
                    clause_comments: Vec::new(),
                }),
                raw_stmt(""),
                raw_stmt("DEFINE PARAM OVERWRITE $after VALUE 1;"),
            ],
            trailing_comments: Vec::new(),
        };
        assert_eq!(
            emit(&file),
            "DEFINE SEQUENCE IF NOT EXISTS order_number;\nDEFINE PARAM OVERWRITE $after VALUE 1;\n"
        );
    }

    // ---- function ----

    use crate::ast::{Function, Graphql, Kind};

    fn function(
        name: &str,
        args: &[(&str, &str)],
        returns: Option<&str>,
        body: &str,
        permissions: Option<Permission>,
        graphql: Option<(Option<&str>, Option<&str>)>,
        lines: &[&str],
    ) -> Stmt {
        Stmt::Function(Function {
            name: FnPath {
                kind: FnKind::Fn,
                segments: name.split("::").map(str::to_string).collect(),
            },
            args: args
                .iter()
                .map(|(param, kind)| (ParamName::new(*param), Kind::new(*kind)))
                .collect(),
            returns: returns.map(Kind::new),
            body: Expr {
                canon: body.to_string(),
            },
            permissions,
            graphql: graphql.map(|(alias, deprecated)| Graphql {
                alias: alias.map(str::to_string),
                deprecated: deprecated.map(str::to_string),
            }),
            comment: (!lines.is_empty()).then(|| DocComment {
                lines: lines.iter().map(|l| l.to_string()).collect(),
            }),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })
    }

    /// spec/function.md: the doc-comment example — the signature with
    /// return type, the single-statement body (canon without a
    /// trailing `;`), and SDL's canonical clause order PERMISSIONS →
    /// COMMENT (diverging from the server emitter's, both parse).
    #[test]
    fn golden_function_doc_permissions() {
        golden_stmts(
            "codegen-function-doc-permissions",
            vec![function(
                "can_edit",
                &[("p", "record<post>")],
                Some("bool"),
                "{ RETURN $auth.id = $p.author OR $auth.role = 'admin' }",
                Some(Permission::Expr(Expr {
                    canon: "$auth.admin = true".to_string(),
                })),
                None,
                &["Whether the current user may edit this post"],
            )],
        );
    }

    /// spec/function.md: the multi-statement body — one `;` after
    /// every statement (the embedded block print); the `option<T>`
    /// spelling passes through verbatim (rule 4).
    #[test]
    fn golden_function_multi_statement_body() {
        golden_stmts(
            "codegen-function-multi-body",
            vec![function(
                "tax_total",
                &[("net", "number"), ("rate", "option<number>")],
                None,
                "{ LET $r = $rate ?? 0.15f; LET $tax = math::round($net * $r, 2); $net + $tax; }",
                None,
                None,
                &[],
            )],
        );
    }

    /// spec/function.md: `permissions: none` spells out; absent
    /// clauses elide entirely (no options block in the source).
    #[test]
    fn golden_function_permissions_none() {
        golden_stmts(
            "codegen-function-permissions-none",
            vec![function(
                "purge_logs",
                &[("before", "datetime")],
                None,
                "{ DELETE log WHERE at < $before }",
                Some(Permission::None),
                None,
                &[],
            )],
        );
    }

    /// spec/function.md: zero params; an explicit `permissions: full`
    /// emits (`FULL`), unlike the absent clause.
    #[test]
    fn golden_function_zero_param() {
        golden_stmts(
            "codegen-function-zero-param",
            vec![function(
                "ping",
                &[],
                Some("string"),
                "{ 'pong' }",
                Some(Permission::Full),
                None,
                &[],
            )],
        );
    }

    /// spec/function.md: the trailing `option<number>` parameter and
    /// the multi-segment name path with GRAPHQL_ALIAS.
    #[test]
    fn golden_function_graphql() {
        golden_stmts(
            "codegen-function-graphql",
            vec![
                function(
                    "last_opt",
                    &[("req", "number"), ("opt", "option<number>")],
                    None,
                    "{ $opt ?? $req }",
                    None,
                    None,
                    &[],
                ),
                function(
                    "util::slugify",
                    &[("s", "string")],
                    Some("string"),
                    "{ string::lowercase(string::replace($s, ' ', '-')) }",
                    None,
                    Some((Some("slugify"), None)),
                    &[],
                ),
            ],
        );
    }

    /// spec/function.md: the empty body canonizes to upstream's `{;}`
    /// block print; GRAPHQL_DEPRECATED with COMMENT (COMMENT after
    /// GRAPHQL_* in SDL's order).
    #[test]
    fn golden_function_empty_body() {
        golden_stmts(
            "codegen-function-empty-body",
            vec![function(
                "edge",
                &[("a", "array<float, 10>")],
                None,
                "{;}",
                None,
                Some((None, Some("use v2"))),
                &["graphql deprecated"],
            )],
        );
    }

    // ---- config ----

    use crate::ast::{
        ApiConfig, Config, DefaultConfig, Family, GraphqlConfig, Introspection, Middleware,
    };

    fn middleware(kind: FnKind, path: &str, args: &[&str]) -> Middleware {
        Middleware {
            path: FnPath {
                kind,
                segments: path.split("::").map(str::to_string).collect(),
            },
            args: args
                .iter()
                .map(|canon| Expr {
                    canon: (*canon).to_string(),
                })
                .collect(),
        }
    }

    fn config_api(middleware: Vec<Middleware>, permissions: Option<Permission>) -> Stmt {
        Stmt::Config(Config::Api(ApiConfig {
            middleware,
            permissions,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }))
    }

    fn config_graphql(config: GraphqlConfig) -> Stmt {
        Stmt::Config(Config::Graphql(config))
    }

    fn config_default(namespace: Option<&str>, database: Option<&str>) -> Stmt {
        let expr = |canon: &str| {
            Some(Expr {
                canon: canon.to_string(),
            })
        };
        Stmt::Config(Config::Default(DefaultConfig {
            namespace: namespace.and_then(expr),
            database: database.and_then(expr),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }))
    }

    /// spec/config.md: the api example — bare-head middleware with a
    /// duration argument and an empty-argument entry, then the
    /// permissions expression. SDL's clause order (middleware →
    /// permissions) is the block's source order; upstream's loop is
    /// positional and prints the same order.
    #[test]
    fn golden_config_api() {
        golden_stmts(
            "codegen-config-api",
            vec![config_api(
                vec![
                    middleware(FnKind::Bare, "api::timeout", &["5s"]),
                    middleware(FnKind::Bare, "api::log", &[]),
                ],
                Some(Permission::Expr(Expr {
                    canon: "$auth.admin = true".to_string(),
                })),
            )],
        );
    }

    /// Middleware heads in every SDL spelling: bare keyword-colliding
    /// segments stay bare (the `EscapeKwFreeIdent` shape rule), the
    /// `fn::`/`mod::` prefixes print, statement-like arguments carry
    /// the printer's paren wrap. Absent permissions elides (rule 9:
    /// SurrealQL's FULL default applies). The `mod::` head is gated
    /// *inside* the parser (surrealism), so this one parses through
    /// the experimental capabilities like bucket DDL.
    #[test]
    fn golden_config_api_spellings() {
        golden_stmts_files(
            "codegen-config-api-spellings",
            vec![
                config_api(vec![middleware(FnKind::Bare, "timeout", &[])], None),
                config_api(
                    vec![
                        middleware(FnKind::Fn, "util::guard", &["(CREATE audit)"]),
                        middleware(FnKind::Mod, "pkg::check", &["1000"]),
                    ],
                    Some(Permission::None),
                ),
            ],
        );
    }

    /// spec/config.md: the three graphql examples — the bare `auto;`
    /// flag's compact form, the include/exclude sub-blocks (table
    /// idents escape per the union keyword set; fn paths re-prefixed),
    /// and depth/complexity/introspection-none over elided default
    /// families.
    #[test]
    fn golden_config_graphql_examples() {
        golden_stmts(
            "codegen-config-graphql-examples",
            vec![
                config_graphql(GraphqlConfig {
                    tables: Family::Auto,
                    functions: Family::Auto,
                    ..GraphqlConfig::default()
                }),
                config_graphql(GraphqlConfig {
                    tables: Family::Include(vec![Ident::new("user"), Ident::new("post")]),
                    functions: Family::Exclude(vec![FnPath {
                        kind: FnKind::Fn,
                        segments: vec!["legacy_report".to_string()],
                    }]),
                    ..GraphqlConfig::default()
                }),
                config_graphql(GraphqlConfig {
                    depth: Some(4),
                    complexity: Some(100),
                    introspection: Introspection::None,
                    ..GraphqlConfig::default()
                }),
            ],
        );
    }

    /// The spec-minimal shapes (diverging from
    /// upstream ToSql's unconditional `TABLES … FUNCTIONS …`): the
    /// all-default config, an explicit `none;` flag, and an explicit
    /// `introspection: auto` all resolve to the bare keyword; a mixed
    /// family pair prints both sides; `TABLES NONE` spells out inside
    /// a structured pair.
    #[test]
    fn golden_config_graphql_minimal() {
        golden_stmts(
            "codegen-config-graphql-minimal",
            vec![
                config_graphql(GraphqlConfig::default()),
                config_graphql(GraphqlConfig {
                    tables: Family::None,
                    functions: Family::None,
                    ..GraphqlConfig::default()
                }),
                config_graphql(GraphqlConfig {
                    introspection: Introspection::Auto,
                    ..GraphqlConfig::default()
                }),
                config_graphql(GraphqlConfig {
                    tables: Family::Auto,
                    functions: Family::None,
                    ..GraphqlConfig::default()
                }),
                config_graphql(GraphqlConfig {
                    tables: Family::Exclude(vec![Ident::new("audit_log")]),
                    functions: Family::Include(vec![FnPath {
                        kind: FnKind::Fn,
                        segments: vec!["a".to_string(), "b".to_string()],
                    }]),
                    depth: Some(1_000),
                    ..GraphqlConfig::default()
                }),
            ],
        );
    }

    /// spec/config.md: the default examples — both sides, and the
    /// namespace-only form whose unset side emits `DATABASE NONE`.
    #[test]
    fn golden_config_default() {
        golden_stmts(
            "codegen-config-default",
            vec![
                config_default(Some("'acme'"), Some("'blog'")),
                config_default(Some("'acme'"), None),
            ],
        );
    }

    /// Scope placement (spec/config.md): the ROOT-level `config
    /// default` hoists to before the script's first `USE` line —
    /// here out of source order, between the namespace define and
    /// its `USE` — while the database-level `config graphql` stays
    /// at its source position (best-effort placement, codegen model
    /// item 1).
    #[test]
    fn golden_config_default_hoists_before_first_use() {
        golden_stmts(
            "codegen-config-default-hoists",
            vec![
                Stmt::Namespace(Namespace {
                    name: Ident::new("acme"),
                    keyword: Carried::default(),
                    comment: None,
                    comments: Vec::new(),
                    clause_comments: Vec::new(),
                }),
                config_default(Some("'acme'"), None),
                Stmt::Database(Database {
                    name: Ident::new("shop"),
                    keyword: Carried::default(),
                    strict: false,
                    changefeed: None,
                    comment: None,
                    comments: Vec::new(),
                    trailing_comments: Vec::new(),
                    clause_comments: Vec::new(),
                }),
                config_graphql(GraphqlConfig {
                    tables: Family::Auto,
                    functions: Family::Auto,
                    ..GraphqlConfig::default()
                }),
            ],
        );
    }

    // ---- index ----

    use crate::ast::{Bm25, Distance, Float, Index, IndexKind, VectorType};

    /// spec/index.md: hand-built ASTs (a failure attributes to
    /// codegen alone) over the shared shape — the per-kind params
    /// ride in as [`IndexKind`].
    fn index(name: &str, on: &str, fields: &[&str], kind: IndexKind) -> Index {
        Index {
            name: Ident::new(name),
            kind,
            on: Ident::new(on),
            fields: fields
                .iter()
                .map(|canon| Expr {
                    canon: canon.to_string(),
                })
                .collect(),
            fields_key: Carried::default(),
            concurrently: false,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }
    }

    fn index_stmt(name: &str, on: &str, fields: &[&str], kind: IndexKind) -> Stmt {
        Stmt::Index(index(name, on, fields, kind))
    }

    fn doc(lines: &[&str]) -> Option<crate::ast::DocComment> {
        Some(crate::ast::DocComment {
            lines: lines.iter().map(|line| line.to_string()).collect(),
        })
    }

    /// spec/index.md: plain multi-field with a doc comment —
    /// NAME → ON → FIELDS → COMMENT.
    #[test]
    fn golden_index_plain_comment() {
        let mut stmt = index_stmt(
            "by_account_thread",
            "message",
            &["account", "thread_id"],
            IndexKind::Plain,
        );
        if let Stmt::Index(index) = &mut stmt {
            index.comment = doc(&["Composite lookup for inbox queries"]);
        }
        golden_stmts("codegen-index-plain-comment", vec![stmt]);
    }

    /// spec/index.md: unique — the kind clause between FIELDS and
    /// CONCURRENTLY.
    #[test]
    fn golden_index_unique_concurrently() {
        let mut index = index("one_email", "user", &["email"], IndexKind::Unique);
        index.concurrently = true;
        golden_stmts(
            "codegen-index-unique-concurrently",
            vec![Stmt::Index(index)],
        );
    }

    /// spec/index.md: count — bare and filtered (`COUNT` prints
    /// unconditionally, `WHERE` only when written).
    #[test]
    fn golden_index_count() {
        golden_stmts(
            "codegen-index-count",
            vec![
                index_stmt(
                    "row_count",
                    "reading",
                    &[],
                    IndexKind::Count { where_: None },
                ),
                index_stmt(
                    "active_count",
                    "item",
                    &[],
                    IndexKind::Count {
                        where_: Some(Expr {
                            canon: "status = 'active'".to_string(),
                        }),
                    },
                ),
            ],
        );
    }

    /// spec/index.md: fulltext — minimal emits bare `FULLTEXT`
    /// (minimal-form emission; the server materializes `ANALYZER like
    /// BM25(1.2,0.75)`), the tuned one carries the written clauses
    /// (`BM25(k1,b)` in the server emitter's own comma-without-space
    /// shape — the spec example's spacing is illustrative, like its
    /// omitted OVERWRITE).
    #[test]
    fn golden_index_fulltext() {
        golden_stmts(
            "codegen-index-fulltext-minimal",
            vec![index_stmt(
                "body_fts",
                "post",
                &["body"],
                IndexKind::FullText {
                    analyzer: None,
                    bm25: None,
                    highlights: false,
                },
            )],
        );
        golden_stmts(
            "codegen-index-fulltext-full",
            vec![index_stmt(
                "title_fts",
                "post",
                &["title"],
                IndexKind::FullText {
                    analyzer: Some(Ident::new("english")),
                    bm25: Some(Bm25(1.2, 0.75)),
                    highlights: true,
                },
            )],
        );
    }

    /// spec/index.md: hnsw — minimal (DIMENSION + DISTANCE only) and
    /// tuned (minkowski's bare order, the underscore flags; M before
    /// EFC per the spec grammar's declared order — the server's own
    /// print orders EFC first, both parse).
    #[test]
    fn golden_index_hnsw() {
        golden_stmts(
            "codegen-index-hnsw-minimal",
            vec![index_stmt(
                "vec",
                "embedding",
                &["vector"],
                IndexKind::Hnsw {
                    dimension: 768,
                    distance: Some(Distance::Cosine),
                    vector_type: None,
                    m: None,
                    m0: None,
                    lm: None,
                    efc: None,
                    extend_candidates: false,
                    keep_pruned_connections: false,
                    hashed_vector: false,
                },
            )],
        );
        golden_stmts(
            "codegen-index-hnsw-tuned",
            vec![index_stmt(
                "vec_tuned",
                "embedding",
                &["vector"],
                IndexKind::Hnsw {
                    dimension: 768,
                    distance: Some(Distance::Minkowski(Float(3.0))),
                    vector_type: Some(VectorType::F16),
                    m: Some(16),
                    m0: None,
                    lm: None,
                    efc: Some(200),
                    extend_candidates: true,
                    keep_pruned_connections: true,
                    hashed_vector: false,
                },
            )],
        );
    }

    /// spec/index.md: diskann — the full tuned set, including the
    /// float ALPHA.
    #[test]
    fn golden_index_diskann() {
        golden_stmts(
            "codegen-index-diskann",
            vec![index_stmt(
                "big_vec",
                "document",
                &["embedding"],
                IndexKind::DiskAnn {
                    dimension: 1536,
                    distance: Some(Distance::CosineNormalized),
                    vector_type: Some(VectorType::F16),
                    degree: Some(32),
                    l_build: Some(50),
                    alpha: Some(Float(1.3)),
                    hashed_vector: true,
                },
            )],
        );
    }

    // ---- api ----

    /// One api statement, parsed from SDL — the goldens ride the real
    /// parse (the folded config, the THEN canon) so the snapshot is
    /// the surform output, not a hand-built AST.
    fn api_stmt(sdl: &str) -> Stmt {
        let (file, diags) = crate::parser::parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        assert_eq!(file.items.len(), 1, "{sdl}");
        file.items.into_iter().next().expect("len checked")
    }

    /// spec/api.md's first three examples — the minimal doc-commented
    /// group, the `$request` echo over a comma-grouped head, and the
    /// dynamic path parameter. Emission: OVERWRITE, groups in source
    /// order, THEN canon verbatim (the action slot is unwrapped),
    /// COMMENT last.
    #[test]
    fn golden_api_examples() {
        golden_stmts(
            "codegen-api-examples",
            vec![
                api_stmt(
                    "/// Health check\napi '/health' {\n  get {\n    then: \
                     { { status: 200, body: { ok: true } } };\n  };\n};\n",
                ),
                api_stmt(
                    "api '/echo' {\n  post, put {\n    then: { { status: 200, body: \
                     { you_sent: $request.body, method: $request.method } } };\n  };\n};\n",
                ),
                api_stmt(
                    "api '/users/:id' {\n  get {\n    then: { { status: 200, body: \
                     (type::record('user', $request.params.id)) } };\n  };\n};\n",
                ),
            ],
        );
    }

    /// spec/api.md's permission forms (all three) and the middleware
    /// example: PERMISSIONS prints only where the SDL wrote one, the
    /// expression arm as `WHERE <canon>`, MIDDLEWARE before
    /// PERMISSIONS regardless of SDL order.
    #[test]
    fn golden_api_permissions_and_middleware() {
        golden_stmts(
            "codegen-api-permissions-middleware",
            vec![
                api_stmt(
                    "api '/mixed' {\n  get {\n    permissions: full;\n    then: \
                     { { status: 200 } };\n  };\n  delete {\n    permissions: none;\n    then: \
                     { { status: 204 } };\n  };\n  trace {\n    permissions: \
                     $auth.admin = true;\n    then: { { status: 200 } };\n  };\n};\n",
                ),
                api_stmt(
                    "api '/slow' {\n  get {\n    middleware: api::timeout(1s);\n    then: \
                     { { status: 200 } };\n  };\n};\n",
                ),
            ],
        );
    }

    /// spec/api.md's wildcard + any + endpoint-wide middleware shape:
    /// `FOR ANY` hoists first (canonical, matching the emitter), the
    /// fallback keeps its CoverStmts wrap. The bare and empty-body
    /// forms emit the zero-`FOR` statement, an empty `any` block the
    /// bare `FOR ANY`; the path string re-emits escaped (the decoded
    /// backslash survives via the string table).
    #[test]
    fn golden_api_any_and_bare_forms() {
        golden_stmts(
            "codegen-api-any-and-bare",
            vec![
                api_stmt(
                    "api '/admin/*rest' {\n  any {\n    middleware: api::timeout(5s);\n    then: \
                     { { status: 404, body: { error: 'no such route' } } };\n  };\n  get, delete {\n    \
                     permissions: $auth.admin = true;\n    then: { { status: 200 } };\n  };\n};\n",
                ),
                api_stmt("api '/x' { any { }; };"),
                api_stmt("api '/bare';"),
                api_stmt("api '/e\\\\:x' { get { then: 1; }; };"),
            ],
        );
    }

    // ---- access ----

    /// One access statement, parsed from SDL — the goldens ride the
    /// real parse (the folded kind, the source shape, the canon) so
    /// the snapshot is the surform output, not a hand-built AST.
    fn access_stmt(sdl: &str) -> Stmt {
        let (file, diags) = crate::parser::parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        assert_eq!(file.items.len(), 1, "{sdl}");
        file.items.into_iter().next().expect("len checked")
    }

    fn table_stmt(sdl: &str) -> Stmt {
        let (file, diags) = crate::parser::parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        assert_eq!(file.items.len(), 1, "{sdl}");
        file.items.into_iter().next().expect("len checked")
    }

    /// spec/table.md's workhorse form — schemafull + per-op
    /// permissions + a doc comment, with the nested definitions the
    /// statement carries (fields, index, event). The
    /// permission groups print as written (op words lowercase — the
    /// server emitter's own spelling, probe-pinned; the expression
    /// value `WHERE <canon>`, `none` spelled out) — the server's own
    /// ToSql additionally merges groups by equal value, which SDL
    /// does not adopt (minimal-form, rule 9).
    #[test]
    fn golden_table_workhorse() {
        golden_stmts(
            "codegen-table-workhorse",
            vec![table_stmt(
                "/// Application users\n\ntable user schemafull {\n  permissions {\n    \
                 select: full;\n    create: full;\n    update: $auth.id = id;\n    delete: \
                 none;\n  };\n\n  email: string {\n    assert: string::is_email($value);\n    \
                 default: 'unknown@example.com';\n  };\n\n  index unique email_unique { \
                 fields: email; };\n\n  event audit_email {\n    when: $event = 'UPDATE' and \
                 $before.email != $after.email;\n    then: { create audit set at = \
                 time::now(); };\n  };\n};\n",
            )],
        );
    }

    /// spec/table.md's audit example — drop + schemaless + changefeed.
    /// The expiry emits value-normalized (`30d` → `4w2d` — the
    /// embedded duration canon, spec/access.md's verified fact; the
    /// spec example's `CHANGEFEED 30d` golden predates that
    /// correction).
    #[test]
    fn golden_table_audit() {
        golden_stmts(
            "codegen-table-audit",
            vec![table_stmt(
                "table audit {\n  drop;\n  schemaless;\n  changefeed {\n    expiry: 30d;\n    \
                 include original;\n  };\n};",
            )],
        );
    }

    /// spec/table.md's relation family — pipe endpoints + enforced
    /// (with its nested index), the one-sided forms, the bare
    /// skeleton, and the relation-word spelling. Only the non-empty
    /// sides print; ` | ` joins endpoints (commas are rejected at
    /// parse).
    #[test]
    fn golden_table_relations() {
        golden_stmts(
            "codegen-table-relations",
            vec![
                table_stmt(
                    "table user | org -> wrote -> post | comment {\n  enforced;\n  \
                     schemaless;\n\n  index unique wrote_id { fields: id; };\n};",
                ),
                table_stmt("table follower -> watched ->;"),
                table_stmt("table -> tagged -> post;"),
                table_stmt("table -> follows ->;"),
                table_stmt("table relation generic_edge;"),
            ],
        );
    }

    /// The 3.3 shapes: the LIGHTWEIGHT header word (enforced
    /// folds — the emission always carries `ENFORCED LIGHTWEIGHT` in
    /// the TYPE, the shape INFO materializes, probe-pinned live) and
    /// the INLINE cache caps (only-when-written, EDGES before
    /// REFERENCES, kind-independent).
    #[test]
    fn golden_table_lightweight_caps() {
        golden_stmts(
            "codegen-table-lightweight-caps",
            vec![
                table_stmt("table person -> likes -> post lightweight;"),
                table_stmt(
                    "table person -> likes -> post lightweight {\n  inline edges: 8;\n  \
                     inline references: 16;\n};",
                ),
                table_stmt("table t { inline edges: 0; };"),
                table_stmt("table t { inline references: 256; };"),
            ],
        );
    }

    /// spec/table.md's view example — the block IS the query; the
    /// emission is the fixed `TYPE ANY SCHEMALESS AS SELECT …` shape,
    /// and the restricted select canonizes through the embedded
    /// parser (AS posts, GROUP BY).
    #[test]
    fn golden_table_view() {
        golden_stmts(
            "codegen-table-view",
            vec![table_stmt(
                "/// Posts per author\n\ntable view post_stats {\n  select author, count() \
                 as posts from post group by author;\n};",
            )],
        );
    }

    /// spec/table.md's graphql example, and the minimal forms: the
    /// `any` escape hatch, the bare table emitting `TYPE NORMAL`
    /// (fail-closed), the empty permissions block folding to the
    /// all-NONE default (nothing emitted), and a view's trailing
    /// permission.
    #[test]
    fn golden_table_minimal_forms() {
        golden_stmts(
            "codegen-table-minimal-forms",
            vec![
                table_stmt(
                    "table legacy_reports {\n  graphql {\n    deprecated: 'use \
                 reports';\n  };\n};",
                ),
                table_stmt("table blobby { schemaless; };"),
                table_stmt("table plain;"),
                table_stmt("table empty_perms { permissions { }; };"),
                table_stmt(
                    "table view signed_in { select author from user where active = true; \
                     permissions: full; };",
                ),
            ],
        );
    }

    // ---- field ----

    fn field_stmt(sdl: &str) -> Stmt {
        let (file, diags) = crate::parser::parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        assert_eq!(file.items.len(), 1, "{sdl}");
        file.items.into_iter().next().expect("len checked")
    }

    /// spec/field.md's workhorse — doc comment + type + assert +
    /// default (standalone form), and the value + readonly example.
    /// The SDL canonical clause order puts DEFAULT after ASSERT (the
    /// server's ToSql puts DEFAULT first — both parse, the differ
    /// compares ASTs).
    #[test]
    fn golden_field_workhorse() {
        golden_stmts(
            "codegen-field-workhorse",
            vec![
                field_stmt(
                    "/// Contact address for the account owner\nfield email: string {\n  \
                     on: account;\n  assert: string::is_email($value);\n  default: \
                     'unknown@example.com';\n};",
                ),
                field_stmt(
                    "field locked: bool {\n  on: account;\n  readonly;\n  value: \
                     true;\n};",
                ),
            ],
        );
    }

    /// spec/field.md's computed forms — the typed header assignment,
    /// the typeless nested head, and the block form with clauses.
    /// COMPUTED prints where SDL's order puts it (after DEFAULT — the
    /// server's ToSql prints it after ASSERT).
    #[test]
    fn golden_field_computed() {
        golden_stmts(
            "codegen-field-computed",
            vec![
                table_stmt(
                    "table shout_board {\n  shout: string = string::uppercase(name);\n  \
                     derived = string::uppercase(name);\n};",
                ),
                field_stmt(
                    "field secret_shout: string {\n  on: shout_board;\n  computed: \
                     string::uppercase(name);\n  permissions {\n    select: none;\n  \
                     };\n};",
                ),
            ],
        );
    }

    /// spec/field.md's reference family — the bare flag (the engine's
    /// `ON DELETE IGNORE` default prints bare `REFERENCE`), the
    /// cascade strategy, and the custom THEN block (its canon is the
    /// emitter's block print).
    #[test]
    fn golden_field_reference() {
        golden_stmts(
            "codegen-field-reference",
            vec![
                field_stmt(
                    "field comics: option<array<record<comic_book>>> {\n  on: \
                     person;\n  reference;\n};",
                ),
                field_stmt(
                    "field author: record<user> {\n  on: post;\n  reference {\n    on \
                     delete: cascade;\n  };\n};",
                ),
                field_stmt(
                    "field parent: record<page> {\n  on: page;\n  reference {\n    on delete: \
                     { create cleanup_log set at = time::now() };\n  };\n};",
                ),
            ],
        );
    }

    /// spec/field.md's permissions — the single-value shorthand and
    /// the per-op block without delete (op words lowercase; unlisted
    /// operations are FULL server-side, never SDL emission).
    #[test]
    fn golden_field_permissions() {
        golden_stmts(
            "codegen-field-permissions",
            vec![
                field_stmt(
                    "field secret: string {\n  on: account;\n  permissions: \
                     none;\n};",
                ),
                field_stmt(
                    "field audited: string {\n  on: account;\n  permissions {\n    select: \
                     none;\n    create, update: $auth.admin = true;\n  };\n};",
                ),
            ],
        );
    }

    /// The kind grammar through the header verbatim — unions,
    /// literals, brace-bearing inline objects, fixed-size arrays,
    /// geometry — plus the flexible example and its synthesized
    /// subfield shape, and the nested/wildcard paths.
    #[test]
    fn golden_field_kinds() {
        golden_stmts(
            "codegen-field-kinds",
            vec![
                field_stmt("field mixed: int | string {\n  on: t;\n};"),
                field_stmt("field size: 'small' | 'large' | int {\n  on: t;\n};"),
                field_stmt("field inline_obj: { a: string, b: number } {\n  on: t;\n};"),
                field_stmt("field matrix: array<array<int>> {\n  on: t;\n};"),
                field_stmt("field district: geometry<polygon> {\n  on: t;\n};"),
                table_stmt(
                    "table metrics {\n  meta: object {\n    flexible;\n  };\n  \
                     meta.user_id: int;\n  tags: array<string>;\n  tags.*: string;\n};",
                ),
            ],
        );
    }

    /// spec/field.md's remaining examples — default always, graphql
    /// alias/deprecation, and the `id` field with its allowed clauses.
    #[test]
    fn golden_field_examples() {
        golden_stmts(
            "codegen-field-examples",
            vec![
                field_stmt("field tags: array<string> {\n  on: post;\n  default always: [];\n};"),
                field_stmt(
                    "field display: string {\n  on: user;\n  graphql {\n    alias: \
                     'displayName';\n  };\n};",
                ),
                field_stmt(
                    "field legacy: string {\n  on: user;\n  graphql {\n    deprecated: \
                     'use display';\n  };\n};",
                ),
                field_stmt(
                    "field id: record<user> {\n  on: user;\n  default: \
                     rand::id();\n};",
                ),
            ],
        );
    }

    /// spec/access.md's jwt examples — the JWKS endpoint with its doc
    /// comment, and the local-key form with a duration. URL →
    /// ALGORITHM/KEY, declared sides only in DURATION (minimal-form;
    /// the server's ToSql materializes the full clause).
    #[test]
    fn golden_access_jwt() {
        golden_stmts(
            "codegen-access-jwt",
            vec![
                access_stmt(
                    "/// Tokens issued by Acme SSO\naccess corp_sso {\n  on: database;\n  \
                     type: jwt;\n  url: 'https://sso.acme.com/.well-known/jwks.json';\n};\n",
                ),
                access_stmt(
                    "access ops {\n  on: root;\n  type: jwt;\n  algorithm: es256;\n  key: \
                     'ops_public_key_pem';\n  duration { session: 8h; };\n};\n",
                ),
            ],
        );
    }

    /// spec/access.md's record examples — the braced signup/signin
    /// pair (block canon, statement-like values untouched inside
    /// blocks) and the braceless-signin + authenticate-block shape.
    /// No `jwt { … }` emits no WITH JWT (minimal-form issuance).
    #[test]
    fn golden_access_record() {
        golden_stmts(
            "codegen-access-record",
            vec![
                access_stmt(
                    "access account {\n  on: database;\n  type: record;\n  signup: {\n    \
                     create user set\n      email = $email,\n      password = \
                     crypto::argon2::generate($password)\n  };\n  signin: {\n    select * from \
                     user\n      where email = $email\n      and \
                     crypto::argon2::compare(password, $password)\n  };\n};\n",
                ),
                access_stmt(
                    "access gated {\n  on: database;\n  type: record;\n  signin: select * \
                     from user where email = $email;\n  authenticate: {\n    if $auth.blocked \
                     { throw 'account suspended'; }\n  };\n};\n",
                ),
            ],
        );
    }

    /// spec/access.md's canonical-order example — SIGNIN before the
    /// WITH clauses regardless of SDL order (the signin: clause comes
    /// LAST in the SDL source).
    #[test]
    fn golden_access_record_refresh_jwt() {
        golden_stmts(
            "codegen-access-record-refresh-jwt",
            vec![access_stmt(
                "access member {\n  on: database;\n  type: record;\n  refresh;\n  signin: { \
                 select id from user where email = $email and \
                 crypto::argon2::compare(password, $password) };\n  jwt {\n    algorithm: \
                 eddsa;\n    key: 'member_signing_key';\n    issuer {\n      key: \
                 '[REDACTED]';\n    };\n  };\n};\n",
            )],
        );
    }

    /// spec/access.md's jwt-with-issuer example: the declared issuer
    /// algorithm elides by local key (forced-equal), the issuer key
    /// prints. Duration sides normalize through the embedded printer
    /// (`1h` stays `1h`).
    #[test]
    fn golden_access_jwt_issuer() {
        golden_stmts(
            "codegen-access-jwt-issuer",
            vec![access_stmt(
                "access partner_api {\n  on: database;\n  type: jwt;\n  algorithm: es256;\n  \
                 key: 'partners_public_key';\n  issuer {\n    key: \
                 '[REDACTED]';\n  };\n  duration { token: 1h; };\n};\n",
            )],
        );
    }

    /// spec/access.md's bearer examples — the grant duration (whose
    /// `30d` canonizes to `4w2d`, the embedded printer's weeks+days
    /// spelling) and the record-subject bearer with its `jwt { … }`
    /// block (emitted even though INFO never renders it).
    #[test]
    fn golden_access_bearer() {
        golden_stmts(
            "codegen-access-bearer",
            vec![
                access_stmt(
                    "access ci_tokens {\n  on: database;\n  type: bearer;\n  for: user;\n  \
                     duration {\n    grant: 30d;\n    token: 1d;\n  };\n};\n",
                ),
                access_stmt(
                    "access device_keys {\n  on: database;\n  type: bearer;\n  for: record;\n  \
                     jwt {\n    algorithm: hs256;\n    key: '[REDACTED]';\n  };\n  duration { \
                     token: 365d; };\n};\n",
                ),
            ],
        );
    }

    /// Minimal forms: a bare jwt with neither issuer nor duration, a
    /// duration block with two sides (the `,` joiner, no trailing
    /// comma), the empty duration block folding away entirely, and
    /// the symmetric-verify mirrored issuer written explicitly (the
    /// mirrored `key:` rides the verify key's canon).
    #[test]
    fn golden_access_minimal_forms() {
        golden_stmts(
            "codegen-access-minimal-forms",
            vec![
                access_stmt("access bare { on: root; type: jwt; url: 'https://x/jwks'; };"),
                access_stmt(
                    "access two_sides { on: root; type: bearer; for: user; duration { grant: \
                     12h; session: none; }; };",
                ),
                access_stmt(
                    "access empty_dur { on: ns; type: jwt; url: 'https://x/jwks'; duration { \
                     }; };",
                ),
                access_stmt(
                    "access mirrored { on: db; type: jwt; algorithm: hs256; key: '[REDACTED]'; \
                     issuer { algorithm: hs256; }; };",
                ),
            ],
        );
    }

    /// The mixin expansion goldens (spec/mixin.md): emission runs the
    /// lowering itself, so these parse the authored form (spread and
    /// all) and snapshot the expanded DDL — the spread position, the
    /// whole-field overshadow, the multi-table reuse, and the unused
    /// mixin emitting nothing.
    #[test]
    fn golden_mixin_expansion() {
        let parse_items = |sdl: &str| {
            let (file, diags) = crate::parser::parse_file(sdl);
            assert!(diags.is_empty(), "{sdl}: {diags:?}");
            file.items
        };
        golden_stmts(
            "codegen-mixin-expansion",
            parse_items(
                "mixin timestamped {\n  created_at: datetime;\n  updated_at: datetime;\n};\n\
                 \ntable post schemafull {\n  ...timestamped;\n  title: string;\n  \
                 created_at: datetime { default: time::now(); };\n};\n\ntable account \
                 schemafull {\n  ...timestamped;\n  email: string;\n};\n\nmixin unused {\n  \
                 dead: int;\n};",
            ),
        );
    }

    /// The full member model's expansion goldens (spec/mixin.md):
    /// index and event members emit `ON` the spreading table, and a
    /// collision between two spreads resolves LAST-OCCURRENCE-WINS —
    /// the loser is skipped, never emitted-then-overwritten (the
    /// later spread's `slug_u` and `x` are the ones the DDL carries).
    #[test]
    fn golden_mixin_member_kinds_and_last_wins() {
        let parse_items = |sdl: &str| {
            let (file, diags) = crate::parser::parse_file(sdl);
            assert!(diags.is_empty(), "{sdl}: {diags:?}");
            file.items
        };
        golden_stmts(
            "codegen-mixin-member-kinds-and-last-wins",
            parse_items(
                "mixin base {\n  slug: string;\n  x: int;\n  index slug_u { fields: slug; \
                 concurrently; };\n  event touched { when: slug != ''; then: 1; };\n};\n\
                 \nmixin alt {\n  x: string;\n  index slug_u { fields: slug; };\n};\n\n\
                 table post schemafull {\n  ...base;\n  ...alt;\n};\n\n\
                 table note schemafull {\n  ...base;\n  index slug_u { fields: slug; };\n};",
            ),
        );
    }
}
