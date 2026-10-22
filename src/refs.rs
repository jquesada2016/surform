//! The reference extractor: walks one parsed
//! SDL file and reports every TABLE / FN / PARAM / ANALYZER / MIXIN
//! reference with its owning top-level statement, plus DYNAMIC-MUTE
//! records for the positions where a reference may hide but cannot be
//! statically resolved.
//!
//! MUTE, NEVER ERROR: SurrealDB's catalog is flat, so a reference is
//! a fact about the source, not a namespace system — this module
//! emits no diagnostics and wires no user-facing behavior. The
//! advisory lints consume the report; anything the
//! extractor cannot statically resolve is muted ([`DynamicWhy`]),
//! never guessed.
//!
//! Two tiers:
//!
//! - **Tier 1** walks the SDL AST: kind surfaces (through
//!   [`crate::kind::parse_kind_ast`]), relation endpoints, index
//!   ANALYZER clauses, graphql families, middleware/analyzer fn
//!   paths, spread names, and the `on:` host tables.
//! - **Tier 2** re-parses each captured expression's canonical text
//!   with the embedded parser ([`canon_tree`]) and walks the
//!   `surrealdb_sql` expression tree by hand — no upstream visitor
//!   exists. View queries are statement-shaped canon the canon tree
//!   reports as opaque, so they ride the dedicated [`view_select`]
//!   reader instead (the one embedded parse this module issues
//!   outside the canon bridge); a view's SELECT walks with the same
//!   Tier-2 walker, its `FROM` targets as table positions.
//!
//! Binding rules mirror the planner's: `LET`, `FOREACH` params,
//! closure args, and fn args introduce LOCAL bindings and shadow
//! param reads; ambient-known params (`$value`, …) are still
//! reported — reporting is not a verdict, the reference lint filters
//! through [`ambient_params`]. Host-table self-references (a nested member's
//! `on`) emit nothing; a fn's own name is a definition, not a ref.
//! The report is positional — no deduplication.

use surrealdb_core::syn;
use surrealdb_sql::kind::KindLiteral;
use surrealdb_sql::lookup::LookupSubject;
use surrealdb_sql::order::Ordering;
use surrealdb_sql::part::{DestructurePart, RecurseInstruction};
use surrealdb_sql::statements::{
    CreateStatement, DeleteStatement, IfelseStatement, InsertStatement, RebuildStatement,
    RelateStatement, SelectStatement, UpdateStatement, UpsertStatement,
};
use surrealdb_sql::{
    Cond, Data, Expr as SqlExpr, Fetchs, Field as SqlField, Fields, Function, FunctionCall, Groups,
    Idiom, Kind as SqlKind, Limit, Literal, Lookup, Mock, Output, Part, PostfixOperator, Splits,
    Start, TableName, TopLevelExpr,
};
use surrealdb_syn::ParserConfig;

use crate::ast::{
    self, AccessKind, Config, FieldDefault, FnPath, IndexKind, JwtVerify, OnDelete, OpPermissions,
    Permission, Stmt, TableKind,
};
use crate::expr::{CanonTree, canon_tree, experimental_capabilities};
use crate::kind::KindAst;

/// One reference: a name of kind [`RefKind`] read at capture site
/// [`Site`] of top-level statement `stmt` (the stream index in
/// [`ast::SdlFile::items`] — nested field/event/index statements
/// attribute to their host table/mixin; [`Site`] distinguishes the
/// capture inside).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub stmt: usize,
    pub site: Site,
    pub kind: RefKind,
    /// Tables as authored (no backticks); fn paths `a::b` (no `fn::`
    /// prefix); params BARE (no `$`); analyzer/mixin idents as
    /// authored.
    pub name: String,
}

/// The class of a reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefKind {
    Table,
    Fn,
    Param,
    Analyzer,
    Mixin,
}

/// The capture site a reference or mute rides — the ambient-environment
/// key ([`ambient_params`]) and the lint message's context ("in this
/// view/default").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Site {
    FieldDefault,
    FieldValue,
    FieldAssert,
    FieldComputed,
    FieldPermission,
    /// A field's `on delete then <expr>` strategy — evaluated at
    /// DELETE/purge time with `$reference` (the current record) and a
    /// CursorDoc for the remote record.
    ReferenceOnDelete,
    TablePermission,
    BucketPermission,
    /// A count index's `where:` filter — evaluated per write/build
    /// against the old and new documents.
    CountFilter,
    /// An index's `fields:` entries — full expressions evaluated per
    /// write/build against the old and new documents.
    IndexFields,
    EventWhen,
    EventThen,
    View,
    FnBody,
    ParamValue,
    /// Permissions on standalone define statements — param, function,
    /// module — plus the api permission shapes (the config-global, an
    /// action group's, and the `any` block's statement-level config),
    /// all evaluated in the session ambient; caller-doc dependent
    /// (see [`ambient_params`]).
    StatementPermission,
    /// An api action's `then` body or the `any` fallback — evaluated
    /// PER REQUEST with `$request` bound (session values ride the
    /// retained parent context).
    ApiAction,
    AccessSignup,
    AccessSignin,
    AccessAuthenticate,
    AccessContext,
    /// Every capture evaluated at DEFINE TIME in the defining query's
    /// context — the session ambient applies, with no site-specific
    /// extras: the structural Tier-1 surfaces (kinds, relation
    /// endpoints, graphql families, spreads, `on:` hosts, analyzer
    /// clauses) and the define-time expression slots (sequence/
    /// bucket/user durations, jwt url/key, config defaults, and the
    /// api middleware ARGUMENTS — stored define-time values,
    /// `api/invocation.rs:334`).
    Other,
}

/// Why a position is muted instead of resolved — the bucketing:
/// env-resolvable vs truly dynamic vs never.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DynamicWhy {
    /// A table-hiding builtin call (`type::table(…)`,
    /// `type::record(…)`) whose relevant argument is not a static
    /// string — the table only exists at runtime.
    DynamicCall,
    /// A table position led by an expression (`FROM $this.a`,
    /// `FROM $param`, a fn call) — the target may resolve once the
    /// ambient environment is typed.
    ExpressionLedTarget,
    /// The capture has no readable tree: a `raw` body, an `include`
    /// statement, or canon both canon-tree wrappers reject.
    Opaque,
}

/// A position where a reference may hide but cannot be statically
/// resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dynamic {
    pub stmt: usize,
    pub site: Site,
    pub why: DynamicWhy,
}

/// The extraction result for one parsed file — positional, ordered by
/// statement-stream order then capture order; no deduplication.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub refs: Vec<Reference>,
    pub dynamics: Vec<Dynamic>,
}

impl Report {
    /// Every reference name of one kind, in report order.
    pub fn names_of(&self, kind: RefKind) -> impl Iterator<Item = &str> {
        self.refs
            .iter()
            .filter(move |reference| reference.kind == kind)
            .map(|reference| reference.name.as_str())
    }
}

/// Extracts the reference report of one parsed (already-flattened)
/// file. Statement indexes are positions in the file's statement
/// stream; composed files ride [`crate::load`]'s flatten, so
/// `include` statements are not expected here and are muted
/// defensively.
pub fn extract_file(file: &ast::SdlFile) -> Report {
    let mut walker = Walker::default();
    for (index, stmt) in file.items.iter().enumerate() {
        walker.walk_stmt(stmt, index);
    }
    Report {
        refs: walker.refs,
        dynamics: walker.dynamics,
    }
}

/// The session ambient — bound at EVERY expression evaluation: every
/// `kvs evaluate` attaches the session (`kvs/ds.rs:5460-5512` →
/// `attach_session` :5501 → `add_values(session.values())`), and
/// `Session::values()` (`dbs/session.rs:308-341`) ALWAYS binds all
/// four, `none`-defaulted when unset — a read of an unset one
/// RESOLVES (to none), which the session-param filter needs to know.
pub const SESSION_PARAMS: &[&str] = &["access", "auth", "token", "session"];

/// The record-document trio a `fn::` body can read — CONDITIONALLY,
/// unlike [`SESSION_PARAMS`]: `legacy/expr/param.rs:27` resolves
/// `$this`/`$self` (and `$parent` through the row-scope stack) to the
/// caller's `CursorDoc`, so they bind only when the fn is called in a
/// document-bound evaluation and read `none` at top level. They stay
/// out of [`ambient_params`]' site rows by the convention that
/// conditional bindings are not extras; the fn-body consumer (the
/// linter's undefined-reference filter) unions them in, because a
/// read of one can always resolve on some call path.
pub const DOC_SCOPE_PARAMS: &[&str] = &["this", "self", "parent"];

/// The SITE-SPECIFIC ambient extras at one capture site — the pinned
/// core's binding table, SESSION AMBIENT EXCLUDED: consumers union
/// with [`SESSION_PARAMS`] (bound at every evaluation), so no site
/// claims non-binding and the session four never repeat here. The
/// `$this`/`$self` rows are source-verified (`legacy/expr/param.rs:27`
/// resolves both to the `CursorDoc` document): the field
/// default/value/assert/permission processors (`doc/field.rs`
/// :635/:693/:734/:789 — the permission processor binds the FULL
/// write frame, `:834-843`), the computed
/// derivation (`doc/compute.rs:407`), the event when/then
/// processors (`doc/event.rs:110/137`), and the permission
/// evaluations — `doc/check.rs:602` and `doc/field.rs:822` pass
/// `Some(doc)` on the legacy path, and `exec/permission.rs:226` binds
/// the record as the current value and document root on the exec path
/// (`physical_expr/literal.rs:243`, `row_scope.rs:34`) — so those
/// resolve at the table and field permission sites too. The bucket
/// permission row is its own verified set (`buc/controller.rs:290-322`):
/// `$action` always, `$file`/`$target` when the operation carries a
/// key/copy target; the evaluation doc is `Option`, so
/// `$this`/`$self` are NOT site-specific there. A field's
/// `on delete then` strategy rides its own verified set
/// (`doc/purge.rs:628-661`): `$reference` is the current record
/// (:639) and the CursorDoc is the remote record (:646-650), so
/// `$this`/`$self` name the REMOTE side there. A count index's
/// filter and an index's `fields:` entries evaluate against the old
/// and new documents (`doc/index.rs:73-88` and :69-71/:252,
/// `Some(&self.initial)`/`Some(&self.current)`).
/// An api action's
/// `$request` is verified from the invocation chain
/// (`api/invocation.rs:239-240`); session values ride the retained
/// parent context (`ctx/context.rs:271-295`). The standalone-define
/// permission checks pass the CALLER's doc through
/// (`legacy/expr/param.rs:74`, `function.rs:239`), so `$this`/`$self`
/// (and `$parent` when nested) resolve when the param/fn is read
/// inside a document-bound evaluation and bind none at top level —
/// conditional bindings stay out of the extras row; the api shapes
/// are genuinely doc-free (`api/invocation.rs:143`). An fn body's
/// own args are LOCAL bindings, not ambient (they ride the walker's
/// scope stack), so [`Site::FnBody`] is empty.
pub fn ambient_params(site: Site) -> &'static [&'static str] {
    match site {
        // The write-frame four ride the default/value/assert
        // processors; a COMPUTED body's only evaluation is
        // `compute_one_field` (`doc/compute.rs:384-407`), which
        // passes the caller's ctx with `Some(doc)` — they never
        // reach it.
        Site::FieldComputed => &["this", "self"],
        Site::FieldDefault | Site::FieldValue | Site::FieldAssert => {
            &["value", "before", "after", "input", "this", "self"]
        }
        Site::ReferenceOnDelete => &["reference", "this", "self"],
        Site::TablePermission => &["this", "self", "parent"],
        Site::FieldPermission => &[
            "value", "before", "after", "input", "this", "self", "parent",
        ],
        Site::BucketPermission => &["action", "file", "target"],
        Site::CountFilter | Site::IndexFields => &["this", "self"],
        Site::EventWhen | Site::EventThen => {
            &["event", "before", "after", "value", "input", "this", "self"]
        }
        Site::View => &["this", "self"],
        Site::ApiAction => &["request"],
        // The session four ARE the ambient at all three remaining
        // permission shapes (authenticate: the session's own auth and
        // token; context: the access-defined payload riding
        // `$session.data`).
        Site::AccessAuthenticate | Site::AccessContext => &[],
        // The standalone-define permission checks
        // (`legacy/expr/param.rs:60-82`, `function.rs:60-62/107-109/
        // 160-162`, `api/invocation.rs:119-143`) add nothing beyond
        // the session ambient.
        Site::StatementPermission => &[],
        Site::FnBody | Site::ParamValue | Site::AccessSignup | Site::AccessSignin | Site::Other => {
            &[]
        }
    }
}

// ---------------------------------------------------------------------
// The walker
// ---------------------------------------------------------------------

#[derive(Default)]
struct Walker {
    refs: Vec<Reference>,
    dynamics: Vec<Dynamic>,
    /// Module-class calls' in-document identities — a side channel
    /// for the order pass. The report spells a module/silo call as a
    /// [`RefKind::Fn`]
    /// ref (the report's frozen format keeps the sub-path), but a
    /// define-time call is a MODULE dependency (the engine resolves
    /// `mod::<first-segment>` and the silo catalog name through
    /// `get_db_module`), not a fn one — the helper below needs the
    /// split. `mod::` contributes its first segment; a silo call its
    /// `org::pkg::<maj.min.patch>` identity; `ml::` contributes
    /// nothing (no in-document definer exists).
    module_calls: Vec<String>,
    /// When set (the order pass's helper), module/silo receivers
    /// record ONLY here — the fn-class ref is suppressed, since the
    /// engine never resolves a custom fn for them and a same-spelled
    /// fn would phantom-edge.
    split_modules: bool,
}

impl Walker {
    fn record(&mut self, stmt: usize, site: Site, kind: RefKind, name: impl Into<String>) {
        self.refs.push(Reference {
            stmt,
            site,
            kind,
            name: name.into(),
        });
    }

    fn table(&mut self, stmt: usize, site: Site, name: &TableName) {
        self.record(stmt, site, RefKind::Table, name.as_str());
    }

    fn mute(&mut self, stmt: usize, site: Site, why: DynamicWhy) {
        self.dynamics.push(Dynamic { stmt, site, why });
    }

    // -- Tier 1: the SDL statement stream ----------------------------

    fn walk_stmt(&mut self, stmt: &Stmt, index: usize) {
        match stmt {
            // No reference-bearing surface: a namespace or database
            // name, a changefeed expiry, a strict flag.
            Stmt::Namespace(_) | Stmt::Database(_) => {}
            Stmt::Sequence(sequence) => {
                for expr in [&sequence.start, &sequence.batch, &sequence.timeout]
                    .into_iter()
                    .flatten()
                {
                    self.walk_capture(expr, index, Site::Other);
                }
            }
            Stmt::Param(param) => {
                if let Some(value) = &param.value {
                    self.walk_capture(value, index, Site::ParamValue);
                }
                self.walk_permission(param.permissions.as_ref(), index, Site::StatementPermission);
            }
            Stmt::Bucket(bucket) => {
                if let Some(backend) = &bucket.backend {
                    self.walk_capture(backend, index, Site::Other);
                }
                self.walk_permission(bucket.permissions.as_ref(), index, Site::BucketPermission);
            }
            Stmt::User(user) => {
                for expr in [&user.token_duration, &user.session_duration]
                    .into_iter()
                    .flatten()
                {
                    self.walk_capture(expr, index, Site::Other);
                }
            }
            Stmt::Analyzer(analyzer) => {
                // The analyzer's own name is a definition; its
                // function path is an Fn ref.
                if let Some(path) = &analyzer.function {
                    self.record(index, Site::Other, RefKind::Fn, fn_path_name(path));
                }
            }
            Stmt::Event(_) | Stmt::Field(_) | Stmt::Index(_) | Stmt::Spread(_) => {
                // The standalone member forms — `host` `None` lets
                // their `on:` clause emit its host-table ref. Mixin
                // members ride the same walker (their parse-time
                // `on` is the empty placeholder, skipped there).
                self.walk_member(stmt, None, index);
            }
            Stmt::Module(module) => {
                self.walk_permission(
                    module.permissions.as_ref(),
                    index,
                    Site::StatementPermission,
                );
            }
            Stmt::Function(function) => {
                // The fn's own name is a definition, not a ref; its
                // arg and return kinds can still hold table refs.
                for (_, kind) in &function.args {
                    self.walk_kind(&kind.text, index);
                }
                if let Some(returns) = &function.returns {
                    self.walk_kind(&returns.text, index);
                }
                let args = function
                    .args
                    .iter()
                    .map(|(name, _)| name.raw.clone())
                    .collect();
                self.walk_capture_locals(&function.body.canon, index, Site::FnBody, args);
                self.walk_permission(
                    function.permissions.as_ref(),
                    index,
                    Site::StatementPermission,
                );
            }
            Stmt::Config(config) => match config {
                Config::Api(api) => {
                    for middleware in &api.middleware {
                        self.walk_middleware(middleware, index);
                    }
                    // Evaluated per request inside the permission
                    // chain (`api/invocation.rs:119-143`) — the
                    // StatementPermission shape.
                    self.walk_permission(
                        api.permissions.as_ref(),
                        index,
                        Site::StatementPermission,
                    );
                }
                Config::Graphql(graphql) => {
                    self.walk_family_idents(&graphql.tables, index);
                    for path in family_fn_paths(&graphql.functions) {
                        self.record(index, Site::Other, RefKind::Fn, fn_path_name(path));
                    }
                }
                Config::Default(default) => {
                    for expr in [&default.namespace, &default.database]
                        .into_iter()
                        .flatten()
                    {
                        self.walk_capture(expr, index, Site::Other);
                    }
                }
            },
            Stmt::Api(api) => {
                for action in &api.actions {
                    for middleware in &action.config.middleware {
                        self.walk_middleware(middleware, index);
                    }
                    // A matched request evaluates its action group's
                    // permissions in the invocation chain
                    // (`api/invocation.rs:119-143`).
                    self.walk_permission(
                        action.config.permissions.as_ref(),
                        index,
                        Site::StatementPermission,
                    );
                    // The `then` body runs per request with `$request`
                    // bound (`api/invocation.rs:239-249`).
                    self.walk_capture(&action.action, index, Site::ApiAction);
                }
                if let Some(any) = &api.any {
                    for middleware in &any.config.middleware {
                        self.walk_middleware(middleware, index);
                    }
                    // The `any` block compiles to the STATEMENT-level
                    // api config (`FOR ANY <config>` — codegen.rs's
                    // own emission), and the invocation chain
                    // evaluates `api.config.permissions` for EVERY
                    // routed request in both arms
                    // (`api/invocation.rs:119-124`) — the matched
                    // action's slot is what the fallback path lacks,
                    // never the api-level one.
                    self.walk_permission(
                        any.config.permissions.as_ref(),
                        index,
                        Site::StatementPermission,
                    );
                    if let Some(fallback) = &any.fallback {
                        // The fallback `then` body: per request, with
                        // `$request` bound (`api/invocation.rs:239-249`).
                        self.walk_capture(fallback, index, Site::ApiAction);
                    }
                }
            }
            Stmt::Access(access) => {
                self.walk_access_kind(&access.kind, index);
                if let Some(authenticate) = &access.authenticate {
                    self.walk_capture(authenticate, index, Site::AccessAuthenticate);
                }
                if let Some(context) = &access.context {
                    self.walk_capture(context, index, Site::AccessContext);
                }
                if let Some(duration) = &access.duration {
                    for expr in [&duration.grant, &duration.token, &duration.session]
                        .into_iter()
                        .flatten()
                    {
                        self.walk_capture(expr, index, Site::Other);
                    }
                }
            }
            Stmt::Table(table) => self.walk_table(table, index),
            Stmt::Mixin(mixin) => {
                for member in &mixin.members {
                    self.walk_member(member, None, index);
                }
            }
            Stmt::Raw(_) => {
                // The escape hatch rides verbatim — references inside
                // are unreadable by construction.
                self.mute(index, Site::Other, DynamicWhy::Opaque);
            }
            Stmt::Include(_) => {
                // Does not survive the loader's flatten; muted
                // defensively for unflattened parses.
                self.mute(index, Site::Other, DynamicWhy::Opaque);
            }
        }
    }

    fn walk_table(&mut self, table: &ast::Table, index: usize) {
        match &table.kind {
            TableKind::Relation { from, to, .. } => {
                // The endpoints ARE table refs — a self-relation
                // naming its own table still holds them.
                for endpoint in from.iter().flatten().chain(to.iter().flatten()) {
                    self.record(index, Site::Other, RefKind::Table, endpoint.raw.clone());
                }
            }
            TableKind::View { query } => self.walk_view(&query.canon, index),
            TableKind::Any | TableKind::Normal => {}
        }
        self.walk_op_permissions(table.permissions.as_ref(), index, Site::TablePermission);
        let host = table.name.raw.as_str();
        for member in &table.nested {
            self.walk_member(member, Some(host), index);
        }
    }

    /// One hostable body entry — a `table { … }` block's nested
    /// member or a standalone field/index/event statement. `host` is
    /// the enclosing table's name for the nested forms: their `on`
    /// IS the host by parser construction, so it emits nothing (the
    /// self-skip rule). A mixin member's parse-time `on` is the
    /// empty expansion placeholder, which [`Walker::walk_on`]
    /// skips.
    fn walk_member(&mut self, member: &Stmt, host: Option<&str>, index: usize) {
        match member {
            Stmt::Field(field) => self.walk_field(field, host, index),
            Stmt::Index(idx) => {
                if host.is_none() {
                    self.walk_on(&idx.on, index);
                }
                self.walk_index_clauses(idx, index);
            }
            Stmt::Event(event) => {
                if host.is_none() {
                    self.walk_on(&event.on, index);
                }
                if let Some(when) = &event.when {
                    self.walk_capture(when, index, Site::EventWhen);
                }
                for then in &event.then {
                    self.walk_capture(then, index, Site::EventThen);
                }
            }
            Stmt::Spread(spread) => {
                self.record(index, Site::Other, RefKind::Mixin, spread.name.raw.clone());
            }
            // Parser-enforced members only; hand-built ASTs ride the
            // defensive arm.
            _ => {}
        }
    }

    /// The clauses of a fulltext/count index statement, standalone or
    /// nested (the `on` clause is [`Walker::walk_member`]'s business).
    fn walk_index_clauses(&mut self, idx: &ast::Index, index: usize) {
        if let IndexKind::FullText {
            analyzer: Some(analyzer),
            ..
        } = &idx.kind
        {
            self.record(index, Site::Other, RefKind::Analyzer, analyzer.raw.clone());
        }
        if let IndexKind::Count {
            where_: Some(where_),
        } = &idx.kind
        {
            // Evaluated per write/build against the old and new
            // documents.
            self.walk_capture(where_, index, Site::CountFilter);
        }
        // The fields entries are FULL expressions (upstream parses
        // each as a pratt expression), evaluated per write/build like
        // the count filter — params and graph edges are real.
        for field in &idx.fields {
            self.walk_capture(field, index, Site::IndexFields);
        }
    }

    /// A standalone or nested field. `host` (`Some`) suppresses the
    /// `on` clause — a nested field's host table.
    fn walk_field(&mut self, field: &ast::Field, host: Option<&str>, index: usize) {
        if host.is_none() {
            self.walk_on(&field.on, index);
        }
        if let Some(kind) = &field.kind {
            self.walk_kind(&kind.text, index);
        }
        if let Some(default) = &field.default {
            let expr = match default {
                FieldDefault::Set(expr) | FieldDefault::Always(expr) => expr,
            };
            self.walk_capture(expr, index, Site::FieldDefault);
        }
        if let Some(value) = &field.value {
            self.walk_capture(value, index, Site::FieldValue);
        }
        if let Some(assert) = &field.assert {
            self.walk_capture(assert, index, Site::FieldAssert);
        }
        if let Some(computed) = &field.computed {
            self.walk_capture(computed, index, Site::FieldComputed);
        }
        self.walk_op_permissions(field.permissions.as_ref(), index, Site::FieldPermission);
        if let Some(OnDelete::Then(expr)) = &field.reference {
            // A custom delete strategy evaluates at DELETE/purge time
            // with `$reference` and the remote record as the doc.
            self.walk_capture(expr, index, Site::ReferenceOnDelete);
        }
    }

    /// A standalone member's `on:` host table. An empty name is the
    /// mixin placeholder ([`Walker::walk_member`]) — skipped.
    fn walk_on(&mut self, on: &ast::Ident, index: usize) {
        if !on.raw.is_empty() {
            self.record(index, Site::Other, RefKind::Table, on.raw.clone());
        }
    }

    fn walk_middleware(&mut self, middleware: &ast::Middleware, index: usize) {
        self.record(
            index,
            Site::Other,
            RefKind::Fn,
            fn_path_name(&middleware.path),
        );
        for arg in &middleware.args {
            self.walk_capture(arg, index, Site::Other);
        }
    }

    fn walk_access_kind(&mut self, kind: &AccessKind, index: usize) {
        match kind {
            ast::AccessKind::Jwt { source } => self.walk_jwt(source, index),
            ast::AccessKind::Record {
                signup,
                signin,
                jwt,
                ..
            } => {
                if let Some(signup) = signup {
                    self.walk_capture(signup, index, Site::AccessSignup);
                }
                if let Some(signin) = signin {
                    self.walk_capture(signin, index, Site::AccessSignin);
                }
                if let Some(source) = jwt {
                    self.walk_jwt(source, index);
                }
            }
            // The bearer subject (`for: user | record`) is a
            // discriminator, not a reference — SDL's own grammar; its
            // `jwt { … }` block is a real JwtSource.
            ast::AccessKind::Bearer { jwt, .. } => {
                if let Some(source) = jwt {
                    self.walk_jwt(source, index);
                }
            }
        }
    }

    fn walk_jwt(&mut self, source: &ast::JwtSource, index: usize) {
        match &source.verify {
            JwtVerify::Url(expr) => self.walk_capture(expr, index, Site::Other),
            JwtVerify::Key { key, .. } => self.walk_capture(key, index, Site::Other),
        }
        if let Some(issuer) = &source.issuer
            && let Some(key) = &issuer.key
        {
            self.walk_capture(key, index, Site::Other);
        }
    }

    fn walk_permission(&mut self, permission: Option<&Permission>, index: usize, site: Site) {
        if let Some(Permission::Expr(expr)) = permission {
            self.walk_capture(expr, index, site);
        }
    }

    fn walk_op_permissions(
        &mut self,
        permissions: Option<&OpPermissions>,
        index: usize,
        site: Site,
    ) {
        if let Some(OpPermissions::Groups(groups)) = permissions {
            for group in groups {
                self.walk_permission(Some(&group.value), index, site);
            }
        }
    }

    /// The graphql table family's idents — Table refs.
    fn walk_family_idents(&mut self, family: &ast::Family<ast::Ident>, index: usize) {
        let names = match family {
            ast::Family::Include(names) | ast::Family::Exclude(names) => names,
            ast::Family::None | ast::Family::Auto => return,
        };
        for name in names {
            self.record(index, Site::Other, RefKind::Table, name.raw.clone());
        }
    }

    /// The embedded kind AST's table names — `record<…>` / `table<…>`
    /// slots inside captured expressions (a `LET`'s kind, a closure's
    /// arg/return kinds). `file<…>` names a bucket, and the report
    /// has no bucket kind.
    fn walk_kind_sql(&mut self, kind: &SqlKind, stmt: usize, site: Site) {
        match kind {
            SqlKind::Table(names) | SqlKind::Record(names) => {
                for name in names {
                    self.table(stmt, site, name);
                }
            }
            SqlKind::Either(arms) => {
                for arm in arms {
                    self.walk_kind_sql(arm, stmt, site);
                }
            }
            SqlKind::Set(inner, _) | SqlKind::Array(inner, _) => {
                self.walk_kind_sql(inner, stmt, site);
            }
            SqlKind::Function(args, returns) => {
                for arg in args.iter().flatten() {
                    self.walk_kind_sql(arg, stmt, site);
                }
                if let Some(returns) = returns {
                    self.walk_kind_sql(returns, stmt, site);
                }
            }
            SqlKind::Literal(KindLiteral::Array(kinds)) => {
                for kind in kinds {
                    self.walk_kind_sql(kind, stmt, site);
                }
            }
            SqlKind::Literal(KindLiteral::Object(entries)) => {
                for kind in entries.values() {
                    self.walk_kind_sql(kind, stmt, site);
                }
            }
            _ => {}
        }
    }

    /// The structured kind's table names — `record<…>` / `table<…>`.
    /// `file<…>` names a bucket, and the report has no bucket kind.
    fn walk_kind(&mut self, text: &str, index: usize) {
        let Some(kind) = crate::kind::parse_kind_ast(text) else {
            return;
        };
        self.walk_kind_ast(&kind, index);
    }

    fn walk_kind_ast(&mut self, kind: &KindAst, index: usize) {
        match kind {
            KindAst::Record(names) | KindAst::Table(names) => {
                for name in names {
                    self.record(index, Site::Other, RefKind::Table, name.clone());
                }
            }
            KindAst::File(_) => {}
            KindAst::Sized { inner, .. } => {
                if let Some(inner) = inner {
                    self.walk_kind_ast(inner, index);
                }
            }
            KindAst::Option(inner) => self.walk_kind_ast(inner, index),
            KindAst::Union(arms) | KindAst::ArrayLit(arms) => {
                for arm in arms {
                    self.walk_kind_ast(arm, index);
                }
            }
            KindAst::Object(entries) => {
                for (_, kind) in entries {
                    self.walk_kind_ast(kind, index);
                }
            }
            KindAst::Any
            | KindAst::Word(_)
            | KindAst::Bool(_)
            | KindAst::Nan
            | KindAst::Infinity
            | KindAst::Geometry(_)
            | KindAst::Str(_)
            | KindAst::Num(_) => {}
        }
    }

    // -- The view reader ---------------------------------------------

    /// A view's canonical query: statement-shaped canon the canon
    /// tree reports as opaque, so it rides its own embedded parse.
    /// The SELECT's `FROM` targets are table positions; projections,
    /// `WHERE`, and every other clause ride the Tier-2 walker — the
    /// engine's own view dependency set is the FROM list, everything
    /// else is references the view's body still carries.
    fn walk_view(&mut self, canon: &str, index: usize) {
        let Some(select) = view_select(canon) else {
            self.mute(index, Site::View, DynamicWhy::Opaque);
            return;
        };
        self.walk_select(&select, index, Site::View, &mut Vec::new());
    }

    // -- Tier 2: the captured-expression walker ----------------------

    /// One canonical capture, no local bindings in scope.
    fn walk_capture(&mut self, expr: &ast::Expr, stmt: usize, site: Site) {
        self.walk_capture_locals(&expr.canon, stmt, site, Vec::new());
    }

    /// A capture with seed local bindings — an fn body's own args.
    fn walk_capture_locals(&mut self, canon: &str, stmt: usize, site: Site, locals: Vec<String>) {
        let mut locals = locals;
        match canon_tree(canon) {
            CanonTree::Expr(expr) => self.walk_expr(&expr, stmt, site, &mut locals),
            CanonTree::Block(exprs) => self.walk_block(&exprs, stmt, site, &mut locals),
            CanonTree::Opaque => self.mute(stmt, site, DynamicWhy::Opaque),
        }
    }

    /// A braced block's statements in order, threading one binding
    /// scope: a `LET` registers for the statements AFTER it, and the
    /// whole layer ends at the block's close.
    fn walk_block(&mut self, exprs: &[SqlExpr], stmt: usize, site: Site, locals: &mut Vec<String>) {
        for expr in exprs {
            self.walk_expr(expr, stmt, site, locals);
        }
    }

    fn walk_expr(&mut self, expr: &SqlExpr, stmt: usize, site: Site, locals: &mut Vec<String>) {
        match expr {
            SqlExpr::Literal(literal) => self.walk_literal(literal, stmt, site, locals),
            SqlExpr::Param(param) => {
                let name = param.as_str();
                if !locals.iter().any(|local| local == name) {
                    self.record(stmt, site, RefKind::Param, name);
                }
            }
            SqlExpr::Idiom(idiom) => self.walk_idiom(idiom, stmt, site, locals),
            // The core bet: `Expr::Table` nodes appear only in table
            // positions. Should one appear here anyway, it is still
            // a table.
            SqlExpr::Table(name) => self.table(stmt, site, name),
            SqlExpr::Mock(mock) => self.walk_mock(mock, stmt, site),
            SqlExpr::Block(block) => {
                let depth = locals.len();
                self.walk_block(&block.0, stmt, site, locals);
                locals.truncate(depth);
            }
            SqlExpr::Constant(_) | SqlExpr::Break | SqlExpr::Continue => {}
            SqlExpr::Prefix { expr, .. } => self.walk_expr(expr, stmt, site, locals),
            SqlExpr::Postfix { expr, op } => {
                self.walk_expr(expr, stmt, site, locals);
                // A postfix call/method-call carries argument
                // expressions (the closure-invoke shape).
                match op {
                    PostfixOperator::Call(args) | PostfixOperator::MethodCall(_, args) => {
                        for arg in args {
                            self.walk_expr(arg, stmt, site, locals);
                        }
                    }
                    PostfixOperator::Range | PostfixOperator::RangeSkip => {}
                }
            }
            SqlExpr::Binary { left, right, .. } => {
                self.walk_expr(left, stmt, site, locals);
                self.walk_expr(right, stmt, site, locals);
            }
            SqlExpr::FunctionCall(call) => self.walk_call(call, stmt, site, locals),
            SqlExpr::Closure(closure) => {
                let depth = locals.len();
                for (param, kind) in &closure.args {
                    self.walk_kind_sql(kind, stmt, site);
                    locals.push(param.as_str().to_owned());
                }
                if let Some(returns) = &closure.returns {
                    self.walk_kind_sql(returns, stmt, site);
                }
                self.walk_expr(&closure.body, stmt, site, locals);
                locals.truncate(depth);
            }
            SqlExpr::Throw(thrown) => self.walk_expr(thrown, stmt, site, locals),
            SqlExpr::Return(output) => {
                self.walk_expr(&output.what, stmt, site, locals);
                self.walk_fetch(output.fetch.as_ref(), stmt, site, locals);
            }
            SqlExpr::IfElse(ifelse) => self.walk_ifelse(ifelse, stmt, site, locals),
            SqlExpr::Select(select) => self.walk_select(select, stmt, site, locals),
            SqlExpr::Create(create) => self.walk_dml(create.as_ref(), stmt, site, locals),
            SqlExpr::Update(update) => self.walk_dml(update.as_ref(), stmt, site, locals),
            SqlExpr::Upsert(upsert) => self.walk_dml(upsert.as_ref(), stmt, site, locals),
            SqlExpr::Delete(delete) => self.walk_dml(delete.as_ref(), stmt, site, locals),
            // A nested DDL statement's names are definitions, not
            // references, and its slots are outside the pinned Tier-2
            // surface (`DEFINE FUNCTION` bodies arrive through the
            // canon bridge's block wrapper, never through this arm).
            SqlExpr::Define(_) | SqlExpr::Remove(_) | SqlExpr::Alter(_) => {}
            SqlExpr::Relate(relate) => self.walk_relate(relate, stmt, site, locals),
            SqlExpr::Insert(insert) => self.walk_insert(insert, stmt, site, locals),
            SqlExpr::Rebuild(rebuild) => match rebuild.as_ref() {
                RebuildStatement::Index(index) => self.table(stmt, site, &index.what),
            },
            SqlExpr::Info(_) | SqlExpr::Sleep(_) => {}
            SqlExpr::Foreach(foreach) => {
                let depth = locals.len();
                // The range evaluates BEFORE the param binds
                // (`legacy/expr/statements/foreach.rs:23` computes,
                // the per-iteration child context binds after) — the
                // LET arm's ordering, mirrored.
                self.walk_expr(&foreach.range, stmt, site, locals);
                locals.push(foreach.param.as_str().to_owned());
                self.walk_block(&foreach.block.0, stmt, site, locals);
                locals.truncate(depth);
            }
            SqlExpr::Let(sets) => {
                if let Some(kind) = &sets.kind {
                    self.walk_kind_sql(kind, stmt, site);
                }
                self.walk_expr(&sets.what, stmt, site, locals);
                // Registered AFTER the value walk: `LET $x = $x`
                // reads the outer binding.
                locals.push(sets.name.as_str().to_owned());
            }
            SqlExpr::Explain { statement, .. } => self.walk_expr(statement, stmt, site, locals),
        }
    }

    fn walk_literal(
        &mut self,
        literal: &Literal,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        match literal {
            // A record-id literal references its table from any
            // position; its key can carry expressions.
            Literal::RecordId(rid) => {
                self.table(stmt, site, &rid.table);
                self.walk_record_key(&rid.key, stmt, site, locals);
            }
            Literal::Array(exprs) | Literal::Set(exprs) => {
                for expr in exprs {
                    self.walk_expr(expr, stmt, site, locals);
                }
            }
            Literal::Object(entries) => {
                for entry in entries {
                    self.walk_expr(&entry.value, stmt, site, locals);
                }
            }
            _ => {}
        }
    }

    fn walk_record_key(
        &mut self,
        key: &surrealdb_sql::RecordIdKeyLit,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        match key {
            surrealdb_sql::RecordIdKeyLit::Array(exprs) => {
                for expr in exprs {
                    self.walk_expr(expr, stmt, site, locals);
                }
            }
            surrealdb_sql::RecordIdKeyLit::Object(entries) => {
                for entry in entries {
                    self.walk_expr(&entry.value, stmt, site, locals);
                }
            }
            surrealdb_sql::RecordIdKeyLit::Range(range) => {
                // A range's bounds are key literals, whose array and
                // object forms carry expressions.
                self.walk_record_range(range, stmt, site, locals);
            }
            surrealdb_sql::RecordIdKeyLit::Number(_)
            | surrealdb_sql::RecordIdKeyLit::String(_)
            | surrealdb_sql::RecordIdKeyLit::Uuid(_)
            | surrealdb_sql::RecordIdKeyLit::Generate(_) => {}
        }
    }

    /// A record-key range's bounds — `Bound<RecordIdKeyLit>` on both
    /// sides; the bounded sides walk as key literals.
    fn walk_record_range(
        &mut self,
        range: &surrealdb_sql::RecordIdKeyRangeLit,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        for bound in [&range.start, &range.end] {
            match bound {
                std::ops::Bound::Included(key) | std::ops::Bound::Excluded(key) => {
                    self.walk_record_key(key, stmt, site, locals);
                }
                std::ops::Bound::Unbounded => {}
            }
        }
    }

    fn walk_mock(&mut self, mock: &Mock, stmt: usize, site: Site) {
        match mock {
            Mock::Count(table, _) | Mock::Range(table, _) => self.table(stmt, site, table),
        }
    }

    fn walk_idiom(&mut self, idiom: &Idiom, stmt: usize, site: Site, locals: &mut Vec<String>) {
        for part in &idiom.0 {
            match part {
                Part::Graph(lookup) => self.walk_lookup(lookup, stmt, site, locals),
                Part::Start(expr) | Part::Where(expr) | Part::Value(expr) => {
                    self.walk_expr(expr, stmt, site, locals);
                }
                Part::Method(_, args) => {
                    for arg in args {
                        self.walk_expr(arg, stmt, site, locals);
                    }
                }
                Part::Destructure(parts) => self.walk_destructure(parts, stmt, site, locals),
                // The recurse's nest idiom and shortest-path target
                // both lead with full idioms (graphs included).
                Part::Recurse(_, nest, instruction) => {
                    if let Some(nest) = nest {
                        self.walk_idiom(nest, stmt, site, locals);
                    }
                    if let Some(RecurseInstruction::Shortest { expects, .. }) = instruction {
                        self.walk_expr(expects, stmt, site, locals);
                    }
                }
                _ => {}
            }
        }
    }

    /// A `.{ field: … }` destructure — the aliased parts lead with
    /// full idioms (graphs included); nested destructures recurse.
    fn walk_destructure(
        &mut self,
        parts: &[DestructurePart],
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        for part in parts {
            match part {
                DestructurePart::Aliased(_, idiom) => self.walk_idiom(idiom, stmt, site, locals),
                DestructurePart::Destructure(_, nested) => {
                    self.walk_destructure(nested, stmt, site, locals);
                }
                DestructurePart::All(_) | DestructurePart::Field(_) => {}
            }
        }
    }

    /// One graph/`<~` edge: every lookup subject is a table ref (the
    /// multi form `->a,b` yields one each; `?` has none; the alias is
    /// not a target); a range subject's bounds carry key literals
    /// whose array/object forms hold expressions; the inner SELECT,
    /// WHERE, and the tail clauses recurse.
    fn walk_lookup(&mut self, lookup: &Lookup, stmt: usize, site: Site, locals: &mut Vec<String>) {
        for subject in &lookup.what {
            self.table(stmt, site, lookup_subject_table(subject));
            if let LookupSubject::Range { range, .. } = subject {
                self.walk_record_range(range, stmt, site, locals);
            }
        }
        if let Some(fields) = &lookup.expr {
            self.walk_fields(fields, stmt, site, locals);
        }
        if let Some(cond) = &lookup.cond {
            self.walk_expr(&cond.0, stmt, site, locals);
        }
        self.walk_splits(lookup.split.as_ref(), stmt, site, locals);
        self.walk_groups(lookup.group.as_ref(), stmt, site, locals);
        self.walk_order(lookup.order.as_ref(), stmt, site, locals);
        self.walk_limit(lookup.limit.as_ref(), stmt, site, locals);
        self.walk_start(lookup.start.as_ref(), stmt, site, locals);
        // The `AS` alias leads with a full idiom (graphs included) —
        // it is not a TARGET, but its edges are still references.
        if let Some(alias) = &lookup.alias {
            self.walk_idiom(alias, stmt, site, locals);
        }
    }

    fn walk_call(
        &mut self,
        call: &FunctionCall,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        match &call.receiver {
            // A table-hiding builtin recovers a static table from a
            // string-literal argument, else mutes the call.
            Function::Normal(name) if table_hiding_arg(name).is_some() => {
                match recover_table_call(name, &call.arguments) {
                    Some(table) => self.record(stmt, site, RefKind::Table, table),
                    None => self.mute(stmt, site, DynamicWhy::DynamicCall),
                }
            }
            receiver => {
                // The module-class receivers: an in-document MODULE
                // dependency (recorded for the order pass), never the
                // custom-fn resolution the Fn ref implies.
                let module_class = matches!(receiver, Function::Module(..) | Function::Silo { .. });
                if let Function::Module(module, _) = receiver {
                    self.module_calls.push(module.clone());
                }
                if let Function::Silo {
                    org,
                    pkg,
                    major,
                    minor,
                    patch,
                    ..
                } = receiver
                {
                    self.module_calls
                        .push(format!("{org}::{pkg}::<{major}.{minor}.{patch}>"));
                }
                // In split mode the module-class call records ONLY as
                // a module dependency — the engine never resolves a
                // custom fn for it, and a same-spelled fn would
                // phantom-edge.
                if let Some(name) = receiver_fn_name(receiver)
                    && !(self.split_modules && module_class)
                {
                    self.record(stmt, site, RefKind::Fn, name);
                }
            }
        }
        for arg in &call.arguments {
            self.walk_expr(arg, stmt, site, locals);
        }
    }

    fn walk_ifelse(
        &mut self,
        ifelse: &IfelseStatement,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        for (cond, body) in &ifelse.exprs {
            self.walk_expr(cond, stmt, site, locals);
            self.walk_expr(body, stmt, site, locals);
        }
        if let Some(close) = &ifelse.close {
            self.walk_expr(close, stmt, site, locals);
        }
    }

    fn walk_select(
        &mut self,
        select: &SelectStatement,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        self.walk_fields(&select.fields, stmt, site, locals);
        for omit in &select.omit {
            self.walk_expr(omit, stmt, site, locals);
        }
        for target in &select.what {
            self.walk_target(target, stmt, site, locals);
        }
        if let Some(cond) = &select.cond {
            self.walk_expr(&cond.0, stmt, site, locals);
        }
        self.walk_splits(select.split.as_ref(), stmt, site, locals);
        self.walk_groups(select.group.as_ref(), stmt, site, locals);
        self.walk_order(select.order.as_ref(), stmt, site, locals);
        self.walk_limit(select.limit.as_ref(), stmt, site, locals);
        self.walk_start(select.start.as_ref(), stmt, site, locals);
        self.walk_fetch(select.fetch.as_ref(), stmt, site, locals);
        self.walk_expr(&select.version, stmt, site, locals);
        self.walk_expr(&select.timeout, stmt, site, locals);
    }

    /// A table position — the `what`/`into`/relate slot: static
    /// targets resolve, expression-led ones mute and still walk (the
    /// idiom's graph edges, the param read, the subquery are real
    /// references).
    fn walk_target(&mut self, expr: &SqlExpr, stmt: usize, site: Site, locals: &mut Vec<String>) {
        match expr {
            SqlExpr::Table(name) => self.table(stmt, site, name),
            SqlExpr::Mock(mock) => self.walk_mock(mock, stmt, site),
            SqlExpr::Literal(Literal::RecordId(rid)) => {
                self.table(stmt, site, &rid.table);
                self.walk_record_key(&rid.key, stmt, site, locals);
            }
            SqlExpr::FunctionCall(call) => {
                // The table-position classification runs ONCE here —
                // the argument walk below must not re-run recovery.
                match &call.receiver {
                    Function::Normal(name) if table_hiding_arg(name).is_some() => {
                        match recover_table_call(name, &call.arguments) {
                            Some(table) => self.record(stmt, site, RefKind::Table, table),
                            None => self.mute(stmt, site, DynamicWhy::DynamicCall),
                        }
                    }
                    receiver => {
                        self.mute(stmt, site, DynamicWhy::ExpressionLedTarget);
                        // The module-class classification rides here
                        // too — a `mod::`/`silo::` call in a TABLE
                        // position is the same in-document dependency
                        // (the engine resolves the module, not a fn).
                        let module_class = module_identity(receiver);
                        if let Some(identity) = &module_class {
                            self.module_calls.push(identity.clone());
                        }
                        if let Some(name) = receiver_fn_name(receiver)
                            && !(self.split_modules && module_class.is_some())
                        {
                            self.record(stmt, site, RefKind::Fn, name);
                        }
                    }
                }
                for arg in &call.arguments {
                    self.walk_expr(arg, stmt, site, locals);
                }
            }
            other => {
                self.mute(stmt, site, DynamicWhy::ExpressionLedTarget);
                self.walk_expr(other, stmt, site, locals);
            }
        }
    }

    fn walk_relate(
        &mut self,
        relate: &RelateStatement,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        self.walk_target(&relate.through, stmt, site, locals);
        self.walk_target(&relate.from, stmt, site, locals);
        self.walk_target(&relate.to, stmt, site, locals);
        if let Some(data) = &relate.data {
            self.walk_data(data, stmt, site, locals);
        }
        self.walk_output(relate.output.as_ref(), stmt, site, locals);
        self.walk_expr(&relate.timeout, stmt, site, locals);
    }

    fn walk_insert(
        &mut self,
        insert: &InsertStatement,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        if let Some(into) = &insert.into {
            self.walk_target(into, stmt, site, locals);
        }
        self.walk_data(&insert.data, stmt, site, locals);
        if let Some(update) = &insert.update {
            self.walk_data(update, stmt, site, locals);
        }
        self.walk_output(insert.output.as_ref(), stmt, site, locals);
        self.walk_expr(&insert.timeout, stmt, site, locals);
    }

    fn walk_fields(&mut self, fields: &Fields, stmt: usize, site: Site, locals: &mut Vec<String>) {
        match fields {
            Fields::Value(selector) => self.walk_selector(selector, stmt, site, locals),
            Fields::Select(items) => {
                for item in items {
                    if let SqlField::Single(selector) = item {
                        self.walk_selector(selector, stmt, site, locals);
                    }
                }
            }
        }
    }

    /// One projection: its expression plus its `AS` alias — the
    /// alias leads with a full idiom (graphs included).
    fn walk_selector(
        &mut self,
        selector: &surrealdb_sql::field::Selector,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        self.walk_expr(&selector.expr, stmt, site, locals);
        if let Some(alias) = &selector.alias {
            self.walk_idiom(alias, stmt, site, locals);
        }
    }

    fn walk_data(&mut self, data: &Data, stmt: usize, site: Site, locals: &mut Vec<String>) {
        match data {
            Data::SetExpression(assignments) | Data::UpdateExpression(assignments) => {
                for assignment in assignments {
                    self.walk_idiom(&assignment.place, stmt, site, locals);
                    self.walk_expr(&assignment.value, stmt, site, locals);
                }
            }
            Data::UnsetExpression(idioms) => {
                for idiom in idioms {
                    self.walk_idiom(idiom, stmt, site, locals);
                }
            }
            Data::PatchExpression(expr)
            | Data::MergeExpression(expr)
            | Data::ReplaceExpression(expr)
            | Data::ContentExpression(expr)
            | Data::SingleExpression(expr) => self.walk_expr(expr, stmt, site, locals),
            Data::ValuesExpression(rows) => {
                for row in rows {
                    // The column side is a full idiom (graph parts and
                    // `[value]`/`[WHERE …]` parts included), exactly
                    // like the index-fields precedent.
                    for (column, expr) in row {
                        self.walk_idiom(column, stmt, site, locals);
                        self.walk_expr(expr, stmt, site, locals);
                    }
                }
            }
            Data::EmptyExpression => {}
        }
    }

    fn walk_output(
        &mut self,
        output: Option<&Output>,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        if let Some(Output::Fields(fields)) = output {
            self.walk_fields(fields, stmt, site, locals);
        }
    }

    fn walk_splits(
        &mut self,
        splits: Option<&Splits>,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        if let Some(splits) = splits {
            for split in &splits.0 {
                self.walk_idiom(&split.0, stmt, site, locals);
            }
        }
    }

    fn walk_groups(
        &mut self,
        groups: Option<&Groups>,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        if let Some(groups) = groups {
            for group in &groups.0 {
                self.walk_idiom(&group.0, stmt, site, locals);
            }
        }
    }

    fn walk_order(
        &mut self,
        order: Option<&Ordering>,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        if let Some(Ordering::Order(list)) = order {
            for order in list.iter() {
                self.walk_idiom(&order.value, stmt, site, locals);
            }
        }
    }

    fn walk_limit(
        &mut self,
        limit: Option<&Limit>,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        if let Some(limit) = limit {
            self.walk_expr(&limit.0, stmt, site, locals);
        }
    }

    fn walk_start(
        &mut self,
        start: Option<&Start>,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        if let Some(start) = start {
            self.walk_expr(&start.0, stmt, site, locals);
        }
    }

    fn walk_fetch(
        &mut self,
        fetch: Option<&Fetchs>,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        if let Some(fetch) = fetch {
            for fetch in &fetch.0 {
                self.walk_expr(&fetch.0, stmt, site, locals);
            }
        }
    }
}

/// The clause shape CREATE / UPDATE / UPSERT / DELETE share —
/// [`Walker::walk_dml`]'s input.
trait DmlShape {
    fn what(&self) -> &[SqlExpr];
    fn tail(&self) -> DmlTail<'_>;
}

impl DmlShape for CreateStatement {
    fn what(&self) -> &[SqlExpr] {
        &self.what
    }
    fn tail(&self) -> DmlTail<'_> {
        DmlTail {
            cond: None,
            data: self.data.as_ref(),
            output: self.output.as_ref(),
            split: None,
            group: None,
            order: None,
            limit: None,
            start: None,
            fetch: None,
            timeout: &self.timeout,
        }
    }
}

impl DmlShape for UpdateStatement {
    fn what(&self) -> &[SqlExpr] {
        &self.what
    }
    fn tail(&self) -> DmlTail<'_> {
        DmlTail {
            cond: self.cond.as_ref(),
            data: self.data.as_ref(),
            output: self.output.as_ref(),
            split: None,
            group: None,
            order: None,
            limit: None,
            start: None,
            fetch: None,
            timeout: &self.timeout,
        }
    }
}

impl DmlShape for UpsertStatement {
    fn what(&self) -> &[SqlExpr] {
        &self.what
    }
    fn tail(&self) -> DmlTail<'_> {
        DmlTail {
            cond: self.cond.as_ref(),
            data: self.data.as_ref(),
            output: self.output.as_ref(),
            split: None,
            group: None,
            order: None,
            limit: None,
            start: None,
            fetch: None,
            timeout: &self.timeout,
        }
    }
}

impl DmlShape for DeleteStatement {
    fn what(&self) -> &[SqlExpr] {
        &self.what
    }
    fn tail(&self) -> DmlTail<'_> {
        DmlTail {
            cond: self.cond.as_ref(),
            data: None,
            output: self.output.as_ref(),
            split: None,
            group: None,
            order: None,
            limit: None,
            start: None,
            fetch: None,
            timeout: &self.timeout,
        }
    }
}

struct DmlTail<'a> {
    cond: Option<&'a Cond>,
    data: Option<&'a Data>,
    output: Option<&'a Output>,
    split: Option<&'a Splits>,
    group: Option<&'a Groups>,
    order: Option<&'a Ordering>,
    limit: Option<&'a Limit>,
    start: Option<&'a Start>,
    fetch: Option<&'a Fetchs>,
    timeout: &'a SqlExpr,
}

impl Walker {
    /// The shared body of CREATE / UPDATE / UPSERT / DELETE: table
    /// positions plus the clause tail.
    fn walk_dml<S: DmlShape>(
        &mut self,
        shape: &S,
        stmt: usize,
        site: Site,
        locals: &mut Vec<String>,
    ) {
        for target in shape.what() {
            self.walk_target(target, stmt, site, locals);
        }
        let tail = shape.tail();
        if let Some(cond) = tail.cond {
            self.walk_expr(&cond.0, stmt, site, locals);
        }
        if let Some(data) = tail.data {
            self.walk_data(data, stmt, site, locals);
        }
        self.walk_output(tail.output, stmt, site, locals);
        self.walk_splits(tail.split, stmt, site, locals);
        self.walk_groups(tail.group, stmt, site, locals);
        self.walk_order(tail.order, stmt, site, locals);
        self.walk_limit(tail.limit, stmt, site, locals);
        self.walk_start(tail.start, stmt, site, locals);
        self.walk_fetch(tail.fetch, stmt, site, locals);
        self.walk_expr(tail.timeout, stmt, site, locals);
    }
}

// ---------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------

/// The Param and Fn reads of one canonical capture — the define-time
/// dependency surface. Only the [`Site::Other`] and [`Site::ParamValue`]
/// slots call this (the engine evaluates those at define; every other
/// site is runtime-evaluated and edge-free — see the module doc's
/// site map). Params are bare names (no `$`), fns `a::b` paths, both
/// deduplicated in report order; an unreadable capture yields empty
/// vectors (fail-closed: no edge, never a guess). The order pass
/// ([`crate::order`]) consumes this — the per-statement report above
/// cannot separate a define-time capture's reads from a statement's
/// structural paths (an api's middleware names vs its middleware
/// args' calls share a site), and splits the module-class calls
/// (`mod::`/`silo::`) from the fn-class ones the report lumps
/// together.
pub(crate) fn capture_param_fn_refs(canon: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut walker = Walker {
        split_modules: true,
        ..Walker::default()
    };
    walker.walk_capture_locals(canon, 0, Site::ParamValue, Vec::new());
    let mut params = Vec::new();
    let mut fns = Vec::new();
    for reference in walker.refs {
        match reference.kind {
            RefKind::Param => {
                if !params.contains(&reference.name) {
                    params.push(reference.name);
                }
            }
            RefKind::Fn => {
                if !fns.contains(&reference.name) {
                    fns.push(reference.name);
                }
            }
            RefKind::Table | RefKind::Analyzer | RefKind::Mixin => {}
        }
    }
    // The module-class calls are fn refs in the report but MODULE
    // deps for the order pass (the engine resolves
    // `mod::<first-segment>` and the silo catalog name at define);
    // identities, deduplicated.
    let mut modules = Vec::new();
    for module in walker.module_calls {
        if !modules.contains(&module) {
            modules.push(module);
        }
    }
    (params, fns, modules)
}

/// An [`ast::FnPath`] as the report spells fn names: the segments
/// `::`-joined, no `fn::`/`mod::` prefix.
fn fn_path_name(path: &FnPath) -> String {
    path.segments.join("::")
}

fn family_fn_paths(family: &ast::Family<FnPath>) -> &[FnPath] {
    match family {
        ast::Family::Include(paths) | ast::Family::Exclude(paths) => paths,
        ast::Family::None | ast::Family::Auto => &[],
    }
}

/// A call receiver as the report spells fn names — the path without
/// its family prefix, matching the module's Tier-1 [`ast::FnPath`]
/// convention (`fn::a::b`/`mod::a::b`/bare heads all lose their
/// prefix; silo keeps its org/pkg/version shape, model its
/// `name::<version>` shape). `None` for the receivers that name no
/// reportable fn (builtins and script bodies).
fn receiver_fn_name(receiver: &Function) -> Option<String> {
    match receiver {
        Function::Custom(name) => Some(name.clone()),
        Function::Module(module, sub) => Some(match sub {
            Some(sub) => format!("{module}::{sub}"),
            None => module.clone(),
        }),
        Function::Silo {
            org,
            pkg,
            major,
            minor,
            patch,
            sub,
        } => Some(match sub {
            Some(sub) => format!("{org}::{pkg}::<{major}.{minor}.{patch}>::{sub}"),
            None => format!("{org}::{pkg}::<{major}.{minor}.{patch}>"),
        }),
        Function::Model(model) => Some(format!(
            "{}::<{}>",
            model.name.as_str(),
            model.version.as_str()
        )),
        Function::Normal(_) | Function::Script(_) => None,
    }
}

/// A call receiver's in-document MODULE identity — `mod::` names its
/// first segment (the engine resolves `mod::<first-segment>`), a
/// `silo::` call its `org::pkg::<maj.min.patch>` catalog name. `None`
/// for the receivers no in-document statement defines (`fn::` customs
/// file fn-class; `ml::`/builtins/scripts have no definer at all).
/// Shared by [`Walker::walk_call`] and [`Walker::walk_target`] — a
/// module-class call in a TABLE position is the same dependency.
fn module_identity(receiver: &Function) -> Option<String> {
    match receiver {
        Function::Module(module, _) => Some(module.clone()),
        Function::Silo {
            org,
            pkg,
            major,
            minor,
            patch,
            ..
        } => Some(format!("{org}::{pkg}::<{major}.{minor}.{patch}>")),
        _ => None,
    }
}

/// A graph lookup subject's table — both the plain and the range
/// form name one.
fn lookup_subject_table(subject: &LookupSubject) -> &TableName {
    match subject {
        LookupSubject::Table { table, .. } => table,
        LookupSubject::Range { table, .. } => table,
    }
}

/// The table-hiding builtins — the pinned 3.3.0 set (the `fnc/mod.rs`
/// dispatch and the `exec/function/builtin/type.rs` registry):
/// `type::table(value)` and `type::record(value, ?table)`. Upstream's
/// `type::thing` / `type::edge` do not exist in 3.3.0. The value is
/// the argument index the table name comes from.
fn table_hiding_arg(name: &str) -> Option<usize> {
    match name {
        "type::table" | "type::record" => Some(0),
        _ => None,
    }
}

/// Recovers a static table from a table-hiding call whose relevant
/// argument is a string literal: `type::table('person')` → `person`;
/// two-arg `type::record('user', key)` → `user`; one-arg
/// `type::record('user:123')` → the prefix before the first `:`.
/// Anything else (non-literal, empty, colon-less one-arg record) is
/// `None` — the caller mutes the call.
fn recover_table_call(name: &str, args: &[SqlExpr]) -> Option<String> {
    let index = table_hiding_arg(name)?;
    let SqlExpr::Literal(Literal::String(value)) = args.get(index)? else {
        return None;
    };
    let value = value.as_str();
    if value.is_empty() {
        return None;
    }
    match (name, args.len()) {
        ("type::record", 1) => {
            let (table, _) = value.split_once(':')?;
            (!table.is_empty()).then(|| table.to_string())
        }
        _ => Some(value.to_string()),
    }
}

/// Parses a view's canonical query (`SELECT …`, the embedded
/// printer's restricted select) into its statement AST — the reader
/// the Tier-2 view walk rides. `None` = the canon does not read back
/// as a single SELECT (the caller mutes). Crate-public for the
/// consumers that need exactly the view's top `FROM` targets — the
/// engine's define-validated source set (`view.source_tables()` is
/// the stored `what` list; the order pass's hard edge rows ride the
/// same list), not every table position the body carries.
pub(crate) fn view_select(canon: &str) -> Option<SelectStatement> {
    let mut query = String::with_capacity(canon.len() + 1);
    query.push_str(canon);
    if !query.ends_with(';') {
        query.push(';');
    }
    let ast = syn::parse_with_capabilities(
        &query,
        &experimental_capabilities(),
        &ParserConfig::default(),
    )
    .ok()?;
    match ast.expressions.as_slice() {
        [TopLevelExpr::Expr(SqlExpr::Select(select))] => Some(select.as_ref().clone()),
        _ => None,
    }
}
