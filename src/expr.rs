//! The embedded-expression bridge: a
//! rule-3 capture becomes canonical DDL text through the embedded
//! official parser — raw source is never echoed.
//!
//! Why a wrapper statement: the embed's public API has no expression
//! entry point (`syn::expr` and `Parser::parse_expr_field` are
//! `pub(crate)`; verified against the vendored source), so a capture
//! is canonicalized by parsing a minimal `DEFINE` statement whose
//! clause slot uses the exact grammatical position the server itself
//! uses, then slicing the slot's text back out of the statement's
//! `ToSql` print. The slice is what the server's own emitter prints
//! for that clause — `CoverStmts` paren-wrapping of statement-like
//! expressions included — so [`crate::codegen`] prints it verbatim.
//! (The `ToSql` trait lives in `surrealdb-types`, which the embed
//! does not re-export; hence the direct `=3.3.0` dependency.)
//!
//! Two-stage validation:
//!
//! 1. The capture must parse as a complete top-level query on its
//!    own. This is what rejects clause-keyword leaks — `1 COMMENT 'x'`
//!    would otherwise ride the wrapper's clause loop as a `COMMENT`
//!    clause and silently change the statement's meaning — and it
//!    forwards the embedded error's rendered snippet with its marker
//!    re-anchored to the capture's file position (the quoted line
//!    stays capture-relative). One shape needs a retry:
//!    a bare-variable assignment (`$event = 'CREATE'`, event
//!    `when:`'s idiom) reads as the deprecated parameter-assignment
//!    STATEMENT at the top level, while the same text is a valid
//!    comparison EXPRESSION — the retry parses `RETURN <capture>;`,
//!    still a complete query (leaks die just the same) with the
//!    capture in expression context.
//! 2. The wrapper must accept the capture in its slot; its `ToSql`
//!    output is sliced to the clause text. A slice mismatch is
//!    [`EmbeddedError::Internal`] — diagnosed, never panicked (the
//!    proptest layer relies on every failure being a well-formed
//!    diagnostic).

use std::collections::HashSet;

use crate::lexer::repair_module_call_spaces;
use surrealdb_core::syn;
use surrealdb_rpc::capabilities::{Capabilities, ExperimentalTarget, Targets};
use surrealdb_syn::{ParseError, ParserConfig};
use surrealdb_types::ToSql as _;

/// The parser capabilities SDL runs the embed with: the experimental
/// targets enabled (`files`, `surrealism`), per README codegen model
/// item 4 — SDL does not model server capabilities, so
/// capability-gated constructs (`DEFINE BUCKET`, `mod::` paths) must
/// canonicalize like everything else. Plain [`syn::parse`] enables
/// none of them (`Capabilities::all()` still leaves experiments
/// denied), and `DEFINE BUCKET` is gated *inside the parser* —
/// verified against the v3.2.4 source (`parse_define_bucket`).
pub(crate) fn experimental_capabilities() -> Capabilities {
    Capabilities::none().with_experimental(Targets::Some(HashSet::from([
        ExperimentalTarget::Files,
        ExperimentalTarget::Surrealism,
    ])))
}

/// A clause slot an expression capture feeds, with the wrapper
/// statement and slice bounds that canonicalize it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    /// `param`'s `value:` / `= <expr>` — `DEFINE PARAM … VALUE <T>`.
    ParamValue,
    /// `param`'s `permissions: <expr>` arm — `PERMISSIONS WHERE <T>`.
    ParamPermissions,
    /// `module`'s `permissions: <expr>` arm — its own grammar position
    /// since the 3.3.0 embed (`DEFINE MODULE … UNSIGNED PERMISSIONS
    /// WHERE <T>` parses under the experimental capabilities; the
    /// 3.2.4 cargo-feature bail arm that forced param's stand-in is
    /// gone as of
    /// 2026-09-25). The printer materializes `PERMISSIONS`
    /// unconditionally on modules (probe-pinned), so the prefix is
    /// stable; absent permissions elides at SDL emission (rule 9).
    ModulePermissions,
    /// `sequence`'s `start:` / `= <expr>` — `… START <T>`.
    SequenceStart,
    /// `sequence`'s `batch:` — `… BATCH <T>`.
    SequenceBatch,
    /// `sequence`'s `timeout:` — `… TIMEOUT <T>`.
    SequenceTimeout,
    /// `bucket`'s `backend:` — `DEFINE BUCKET … BACKEND <T>`
    /// (spec/bucket.md; the conventional values are the literals
    /// `'memory'` and `'file:/…'`).
    BucketBackend,
    /// `bucket`'s `permissions: <expr>` arm —
    /// `PERMISSIONS WHERE <T>` (the single-value form, param's
    /// grammar with bucket's variables `$action`, `$file`, `$target`).
    BucketPermissions,
    /// `user`'s `duration { token: <expr>; }` side —
    /// `… DURATION FOR TOKEN <T>` (spec/user.md).
    UserDurationToken,
    /// `user`'s `duration { session: <expr>; }` side —
    /// `… DURATION FOR SESSION <T>`.
    UserDurationSession,
    /// `event`'s `when:` — `DEFINE EVENT sdl ON t WHEN <T> THEN 1;`
    /// (spec/event.md; the dummy `THEN 1` satisfies THEN's ≥1
    /// requirement). `when` prints unconditionally — the statement's
    /// AST defaults it to `true` — so there is no elision fallback.
    EventWhen,
    /// `event`'s `then:`, one item of its comma list —
    /// `DEFINE EVENT sdl ON t THEN <T>;`. The absent `when`
    /// materializes as `WHEN true` in every print (v3.2.4 emitter).
    EventThen,
    /// `function`'s permissions expression arm —
    /// `DEFINE FUNCTION fn::sdl() { 1 } PERMISSIONS WHERE <T>;` (the
    /// statement's own grammar position — it parses gate-free on the
    /// embed, so it needs no shared-slot
    /// stand-in — probe-pinned that the print is the plain
    /// `PERMISSIONS WHERE <canon>` tail).
    FunctionPermissions,
    /// `function`'s body block (spec/function.md) — the whole
    /// braced capture, canon as the complete `{ … }` print:
    /// `DEFINE FUNCTION fn::sdl() { <T> };` prints
    /// `DEFINE FUNCTION fn::sdl() <block> PERMISSIONS FULL;`
    /// (PERMISSIONS prints unconditionally; the parse default is
    /// FULL — probe-pinned against v3.2.4 `ToSql`). Element-counted
    /// block shapes: a single-statement body prints without a
    /// trailing `;`, a multi-statement body with one after every
    /// statement, and an empty or comment-only body canonizes to
    /// upstream's `{;}` — stage 1 accepts `{ }` standalone (an empty
    /// object), and the wrapper's `parse_block` reads it as an empty
    /// block, so no special case is needed.
    FunctionBody,
    /// `config api`'s `permissions:` expression arm —
    /// `DEFINE CONFIG OVERWRITE API PERMISSIONS WHERE <T>;` (the
    /// statement's own grammar position; config has no capability
    /// gate, so the wrapper parses gate-free on the embed — probe-pinned
    /// that PERMISSIONS prints unconditionally with the parse default
    /// FULL, which is exactly the tail this slot's sibling arms sit
    /// behind).
    ConfigApiPermissions,
    /// One `middleware:` argument (spec/config.md) — the slot is a
    /// single-argument call `MIDDLEWARE sdl(<T>)`, so the printed
    /// interior is exactly the argument's canonical text:
    /// `DEFINE CONFIG OVERWRITE API MIDDLEWARE sdl(<T>) PERMISSIONS
    /// FULL;` (PERMISSIONS prints unconditionally). Statement-like
    /// arguments paren-wrap through the printer's `CoverStmts`, like
    /// every embedded print.
    ConfigMiddlewareArg,
    /// `config default`'s `namespace:` — `DEFINE CONFIG OVERWRITE
    /// DEFAULT NAMESPACE <T> DATABASE NONE;`. The print carries TWO
    /// spaces after `OVERWRITE` (upstream quirk: the kind writes
    /// nothing, then the inner emitter starts with a space of its
    /// own) — the slice prefix pins it.
    ConfigDefaultNamespace,
    /// `config default`'s `database:` — `DEFINE CONFIG OVERWRITE
    /// DEFAULT NAMESPACE NONE DATABASE <T>;` (same double space).
    ConfigDefaultDatabase,
    /// One `index`'s `fields:`/`columns:` item — `DEFINE INDEX sdl ON
    /// t FIELDS <T>;` (spec/index.md; the server's FIELDS print, item
    /// comma-joined by codegen; statement-like items CoverStmts-wrap
    /// like every embedded print).
    IndexFields,
    /// `index count`'s `where:` — `DEFINE INDEX sdl ON t COUNT WHERE
    /// <T>;` (the COUNT kind clause prints unconditionally, so the
    /// WHERE sits at the statement tail with nothing after it).
    IndexWhere,
    /// One `api` method group's `then:` action (spec/api.md) —
    /// `DEFINE API OVERWRITE '/probe' FOR get THEN <T>;`. The wrapper
    /// parses gate-free on the embed (the statement has no capability
    /// gate), and the emitter's always-materialized shapes ride the
    /// print: `FOR any PERMISSIONS FULL` ahead of the action, the
    /// group's own `PERMISSIONS FULL` behind it (both probe-pinned).
    /// The action slot prints UNWRAPPED — no `CoverStmts`
    /// paren-wrapping of statement-like values, unlike every other
    /// statement-like slot — so the slice is the bare canonical text
    /// (a statement-like action canonizes unwrapped, e.g. `CREATE
    /// audit`, and re-parses in the THEN position).
    ApiAction,
    /// `api`'s `any`-block `then:` fallback (spec/api.md) —
    /// `DEFINE API OVERWRITE '/probe' FOR any THEN <T>;` parses as the
    /// fallback-only form (zero actions, probe-pinned). Unlike the
    /// action slot, the fallback prints `CoverStmts`-wrapped like the
    /// other statement-like values, and the slice carries that wrap.
    ApiFallback,
    /// `access`'s `signup:` (spec/access.md) — `DEFINE ACCESS sdl ON
    /// DATABASE TYPE RECORD SIGNUP (<T>) WITH JWT ALGORITHM HS256 KEY
    /// 'sdl';`. Every access slot buffers the capture in parentheses:
    /// the clause grammar positions here have TRAILING tokens (the
    /// jwt suffix, the materialized DURATION), and an unbuffered
    /// statement-like capture glues with them (`SIGNUP select * from
    /// user WITH JWT …` reads `WITH` as a SELECT index hint — pinned
    /// by the tripwire). The buffer is transparent to the canon:
    /// CoverStmts keeps parens only where the print needs them. The
    /// wrapper's explicit jwt makes the record print deterministic:
    /// the jwt-less record's default is a RANDOM HS512 key
    /// (probe-pinned — no static suffix could slice it), while the
    /// symmetric HS256 mirrors a known issuer. The print's tail adds
    /// the issuer mirror plus the ALWAYS-materialized `DURATION` (the
    /// jwt-bearing record issues tokens, so TOKEN defaults to `1h` —
    /// probe-pinned).
    AccessSignup,
    /// `access`'s `signin:` — the SIGNIN sibling of [`Slot::AccessSignup`]
    /// (same wrapper shape, SIGNIN position).
    AccessSignin,
    /// `access`'s `authenticate:` (spec/access.md) — `DEFINE ACCESS
    /// sdl ON ROOT TYPE JWT URL 'sdl' AUTHENTICATE <T>;`. The jwt
    /// URL-verify wrapper parses at ROOT (jwt is legal at every
    /// level), so the authenticate position is variant-independent;
    /// the print's tail is the emitter's ALWAYS-materialized
    /// `DURATION` clause (session defaults to `NONE`; the verify-only
    /// wrapper issues nothing, so no TOKEN or GRANT sides appear —
    /// probe-pinned).
    AccessAuthenticate,
    /// `access`'s `context:` (3.3, spec/access.md) — the
    /// authorization payload that rides `$session.data`. The wrapper
    /// mirrors the engine's define-time shape demand (a
    /// record-authenticating access); the value prints covered like
    /// AUTHENTICATE's.
    AccessContext,
    /// `access`'s `url:` (spec/access.md) — `DEFINE ACCESS sdl ON ROOT
    /// TYPE JWT URL <T>;` (the JWKS position; jwt needs a source, so
    /// the URL IS the minimal wrapper). Same materialized-DURATION
    /// tail as every jwt-URL print.
    AccessUrl,
    /// `access`'s `key:` (verify side; spec/access.md) — `DEFINE
    /// ACCESS sdl ON ROOT TYPE JWT ALGORITHM ES256 KEY <T>;`. The
    /// asymmetric ES256 keeps the print issuer-less (a symmetric
    /// algorithm would mirror the capture itself into a
    /// capture-dependent ` WITH ISSUER KEY <canon>` tail); the KEY
    /// expression position is algorithm-independent, so one slot
    /// serves every `algorithm:` pairing. Same materialized-DURATION
    /// tail.
    AccessKey,
    /// `access`'s issuer `key:` (spec/access.md) — `DEFINE ACCESS sdl
    /// ON ROOT TYPE JWT ALGORITHM ES256 KEY 'sdl' WITH ISSUER KEY
    /// <T>;` (the WITH ISSUER KEY position after a fixed asymmetric
    /// verify). Same materialized-DURATION tail.
    AccessIssuerKey,
    /// `access`'s `duration { grant: …; }` side (spec/access.md) —
    /// `DEFINE ACCESS sdl ON ROOT TYPE BEARER FOR USER DURATION FOR
    /// GRANT <T>;`. The bearer wrapper is grant-capable, so GRANT
    /// prints; TOKEN and SESSION materialize their defaults behind it
    /// (`1h` / `NONE`), pinning the static suffix (the trailing comma
    /// after GRANT is the emitter's unconditional shape).
    AccessDurationGrant,
    /// `access`'s `duration { token: …; }` side — the bearer wrapper
    /// prints GRANT's default `30d` ahead and SESSION's `NONE` behind.
    AccessDurationToken,
    /// `access`'s `duration { session: …; }` side — behind both
    /// materialized defaults (`30d`, `1h`).
    AccessDurationSession,
    /// `table view`'s query (spec/table.md) — `DEFINE TABLE sdl TYPE
    /// ANY SCHEMALESS AS <T>;` (the view's fixed emission shape). The
    /// capture is statement-final, and the AS position demands the
    /// `SELECT` spelling itself — upstream's `parse_view` is where
    /// SurrealQL's restricted-select grammar (fields, FROM, WHERE,
    /// GROUP) enforces itself, so no paren buffer and no other clause
    /// can follow in the wrapper. The print's tail is the emitter's
    /// ALWAYS-materialized `PERMISSIONS NONE` (a table permission
    /// prints unconditionally; with none written it is the all-NONE
    /// single value — probe-pinned), pinning the static suffix.
    TableViewQuery,
    /// One `permissions { … }` per-op value's expression arm
    /// (spec/table.md) — `PERMISSIONS FOR select WHERE <T>` in the
    /// wrapper. The capture is paren-buffered like every clause
    /// position whose statement-like values could glue with a
    /// trailing statement (CoverStmts keeps the parens only where the
    /// print needs them); the buffer is transparent to simple values.
    /// The print's tail is the emitter's own materialization of the
    /// three unlisted operations — with only `select` written, they
    /// group as `FOR create, update, delete NONE` (the emitter prints
    /// the full op set once any per-op value is written, grouped by
    /// equal value with delete-Full elided — probe-pinned), pinning
    /// the static suffix.
    TablePermissionValue,
    /// `field`'s `default:` (spec/field.md) — `DEFINE FIELD sdl ON t
    /// TYPE string DEFAULT <T>;`. The print's tail is the emitter's
    /// ALWAYS-materialized `PERMISSIONS FULL` (fields print PERMISSIONS
    /// unconditionally; the parse default is all-FULL — probe-pinned),
    /// which is also why the capture is paren-buffered: a statement-
    /// like value would otherwise glue with the trailing clause.
    FieldDefault,
    /// `field`'s `default always:` — the ALWAYS sibling of
    /// [`Slot::FieldDefault`], same materialized tail.
    FieldDefaultAlways,
    /// `field`'s `value:` (spec/field.md) — `DEFINE FIELD sdl ON t
    /// TYPE string VALUE <T>;` with the same ALWAYS-materialized
    /// `PERMISSIONS FULL` tail (ASSERT/COMPUTED/REFERENCE sit at
    /// their absent defaults between VALUE and PERMISSIONS).
    FieldValue,
    /// `field`'s `assert:` (spec/field.md) — same wrapper shape,
    /// ASSERT position (VALUE absent, PERMISSIONS tail).
    FieldAssert,
    /// `field`'s `computed:` (spec/field.md) — same wrapper shape,
    /// COMPUTED position (the last expression clause before
    /// REFERENCE/COMMENT/PERMISSIONS at their defaults).
    FieldComputed,
    /// `field`'s `on delete: <expr>;` (spec/field.md) — the
    /// non-enum reference action, `DEFINE FIELD sdl ON t TYPE string
    /// REFERENCE ON DELETE THEN <T>;`. The THEN position prints
    /// statement-like values as their braced block (the emitter's own
    /// ON DELETE THEN print — a block capture canonizes to its block
    /// print, no CoverStmts parens survive around braces), with the
    /// same PERMISSIONS tail.
    FieldOnDelete,
    /// One `permissions { … }` per-op value's expression arm for a
    /// `field` (spec/field.md) — `PERMISSIONS FOR select WHERE <T>`.
    /// Same paren-buffer as the table slot; the print's tail is the
    /// emitter's materialization of the two unlisted operations, which
    /// fields seed FULL (`FOR create, update FULL` — the opposite of
    /// the table's NONE-grouped tail — probe-pinned).
    FieldPermissionValue,
}

impl Slot {
    /// The minimal wrapper statement with the capture at the slot
    /// position (kind `Default`, every other clause at its parsed
    /// default), plus the exact prefix/suffix the v3.2.4 `ToSql`
    /// print wraps the slot's canonical text with:
    ///
    /// - `DEFINE PARAM $sdl VALUE <T>;` prints
    ///   `DEFINE PARAM $sdl VALUE <canon> PERMISSIONS FULL;`
    ///   (`PERMISSIONS` unconditionally, default `FULL`).
    /// - `DEFINE PARAM $sdl PERMISSIONS WHERE <T>;` prints
    ///   `DEFINE PARAM $sdl VALUE NONE PERMISSIONS WHERE <canon>;`
    ///   (a clause-less param stores `VALUE NONE`).
    /// - `DEFINE SEQUENCE sdl START <T>;` prints
    ///   `DEFINE SEQUENCE sdl BATCH 1000 START <canon>;` (BATCH
    ///   defaults materialize; fixed order BATCH → START → TIMEOUT).
    /// - `DEFINE SEQUENCE sdl BATCH <T>;` prints
    ///   `DEFINE SEQUENCE sdl BATCH <canon> START 0;`.
    /// - `DEFINE SEQUENCE sdl TIMEOUT <T>;` prints
    ///   `DEFINE SEQUENCE sdl BATCH 1000 START 0 TIMEOUT <canon>;`.
    /// - `DEFINE BUCKET sdl BACKEND <T>;` prints
    ///   `DEFINE BUCKET sdl BACKEND <canon> PERMISSIONS FULL;`
    ///   (`PERMISSIONS` unconditionally, default `FULL`).
    /// - `DEFINE BUCKET sdl PERMISSIONS WHERE <T>;` prints
    ///   `DEFINE BUCKET sdl PERMISSIONS WHERE <canon>;` (nothing
    ///   follows: READONLY and BACKEND are absent, COMMENT elides at
    ///   its default).
    /// - `DEFINE USER sdl ON ROOT DURATION FOR TOKEN <T>;` prints
    ///   `DEFINE USER sdl ON ROOT ROLES VIEWER DURATION FOR TOKEN
    ///   <canon>, FOR SESSION NONE;` — the emitter prints ROLES
    ///   (parse-seeded `VIEWER`) and DURATION (defaults `1h`/`none`)
    ///   unconditionally (probe-verified against v3.2.4 `ToSql`).
    /// - `DEFINE USER sdl ON ROOT DURATION FOR SESSION <T>;` prints
    ///   `DEFINE USER sdl ON ROOT ROLES VIEWER DURATION FOR TOKEN 1h,
    ///   FOR SESSION <canon>;`.
    /// - `DEFINE EVENT sdl ON t WHEN <T> THEN 1;` prints
    ///   `DEFINE EVENT sdl ON t WHEN <canon> THEN 1;` — WHEN prints
    ///   unconditionally (probe-verified against v3.2.4 `ToSql`: the
    ///   emitter has no `when` elision; absent defaults to `true`).
    /// - `DEFINE EVENT sdl ON t THEN <T>;` prints
    ///   `DEFINE EVENT sdl ON t WHEN true THEN <canon>;` — the
    ///   absent `when` materializes in every print (probe-verified).
    ///   The dummy table `t` rides the same `ON <ident>` shape the
    ///   emitter prints (the grammar's optional `TABLE` keyword is
    ///   eaten at parse, never printed).
    ///
    /// Each set of bounds is pinned by a test against the embedded
    /// parser — they are the bump tripwires (re-pinned at the 3.3.0
    /// floor move).
    fn harness(&self, capture: &str) -> (String, &'static str, &'static str) {
        match self {
            Slot::ParamValue => (
                format!("DEFINE PARAM $sdl VALUE {capture};"),
                "DEFINE PARAM $sdl VALUE ",
                " PERMISSIONS FULL;",
            ),
            Slot::ParamPermissions => (
                format!("DEFINE PARAM $sdl PERMISSIONS WHERE {capture};"),
                "DEFINE PARAM $sdl VALUE NONE PERMISSIONS WHERE ",
                ";",
            ),
            Slot::ModulePermissions => (
                // The file executable's name coupling: the header name
                // is required for file modules, so the wrapper carries
                // one; the printer's `PERMISSIONS` is unconditional.
                format!(
                    "DEFINE MODULE OVERWRITE mod::sdl FROM f\"modules:/sdl.surli\" \
                     UNSIGNED PERMISSIONS WHERE {capture};"
                ),
                "DEFINE MODULE OVERWRITE mod::sdl FROM f\"modules:/sdl.surli\" UNSIGNED \
                 PERMISSIONS WHERE ",
                ";",
            ),
            Slot::SequenceStart => (
                format!("DEFINE SEQUENCE sdl START {capture};"),
                "DEFINE SEQUENCE sdl BATCH 1000 START ",
                ";",
            ),
            Slot::SequenceBatch => (
                format!("DEFINE SEQUENCE sdl BATCH {capture};"),
                "DEFINE SEQUENCE sdl BATCH ",
                " START 0;",
            ),
            Slot::SequenceTimeout => (
                format!("DEFINE SEQUENCE sdl TIMEOUT {capture};"),
                "DEFINE SEQUENCE sdl BATCH 1000 START 0 TIMEOUT ",
                ";",
            ),
            Slot::BucketBackend => (
                format!("DEFINE BUCKET sdl BACKEND {capture};"),
                "DEFINE BUCKET sdl BACKEND ",
                " PERMISSIONS FULL;",
            ),
            Slot::BucketPermissions => (
                format!("DEFINE BUCKET sdl PERMISSIONS WHERE {capture};"),
                "DEFINE BUCKET sdl PERMISSIONS WHERE ",
                ";",
            ),
            Slot::UserDurationToken => (
                format!("DEFINE USER sdl ON ROOT DURATION FOR TOKEN {capture};"),
                "DEFINE USER sdl ON ROOT ROLES VIEWER DURATION FOR TOKEN ",
                ", FOR SESSION NONE;",
            ),
            Slot::UserDurationSession => (
                format!("DEFINE USER sdl ON ROOT DURATION FOR SESSION {capture};"),
                "DEFINE USER sdl ON ROOT ROLES VIEWER DURATION FOR TOKEN 1h, FOR SESSION ",
                ";",
            ),
            Slot::EventWhen => (
                format!("DEFINE EVENT sdl ON t WHEN {capture} THEN 1;"),
                "DEFINE EVENT sdl ON t WHEN ",
                " THEN 1;",
            ),
            Slot::EventThen => (
                format!("DEFINE EVENT sdl ON t THEN {capture};"),
                "DEFINE EVENT sdl ON t WHEN true THEN ",
                ";",
            ),
            Slot::FunctionBody => (
                format!("DEFINE FUNCTION fn::sdl() {capture};"),
                "DEFINE FUNCTION fn::sdl() ",
                " PERMISSIONS FULL;",
            ),
            Slot::FunctionPermissions => (
                format!("DEFINE FUNCTION fn::sdl() {{ 1 }} PERMISSIONS WHERE {capture};"),
                "DEFINE FUNCTION fn::sdl() { 1 } PERMISSIONS WHERE ",
                ";",
            ),
            Slot::ConfigApiPermissions => (
                format!("DEFINE CONFIG OVERWRITE API PERMISSIONS WHERE {capture};"),
                "DEFINE CONFIG OVERWRITE API PERMISSIONS WHERE ",
                ";",
            ),
            Slot::ConfigMiddlewareArg => (
                format!("DEFINE CONFIG OVERWRITE API MIDDLEWARE sdl({capture}) PERMISSIONS FULL;"),
                "DEFINE CONFIG OVERWRITE API MIDDLEWARE sdl(",
                ") PERMISSIONS FULL;",
            ),
            Slot::ConfigDefaultNamespace => (
                format!("DEFINE CONFIG OVERWRITE DEFAULT NAMESPACE {capture} DATABASE NONE;"),
                "DEFINE CONFIG OVERWRITE  DEFAULT NAMESPACE ",
                " DATABASE NONE;",
            ),
            Slot::ConfigDefaultDatabase => (
                format!("DEFINE CONFIG OVERWRITE DEFAULT NAMESPACE NONE DATABASE {capture};"),
                "DEFINE CONFIG OVERWRITE  DEFAULT NAMESPACE NONE DATABASE ",
                ";",
            ),
            Slot::IndexFields => (
                format!("DEFINE INDEX sdl ON t FIELDS {capture};"),
                "DEFINE INDEX sdl ON t FIELDS ",
                ";",
            ),
            Slot::IndexWhere => (
                format!("DEFINE INDEX sdl ON t COUNT WHERE {capture};"),
                "DEFINE INDEX sdl ON t COUNT WHERE ",
                ";",
            ),
            Slot::ApiAction => (
                format!("DEFINE API OVERWRITE '/probe' FOR get THEN {capture};"),
                "DEFINE API OVERWRITE '/probe' FOR any PERMISSIONS FULL \
                 FOR get PERMISSIONS FULL THEN ",
                ";",
            ),
            Slot::ApiFallback => (
                format!("DEFINE API OVERWRITE '/probe' FOR any THEN {capture};"),
                "DEFINE API OVERWRITE '/probe' FOR any PERMISSIONS FULL THEN ",
                ";",
            ),
            Slot::AccessSignup => (
                format!(
                    "DEFINE ACCESS sdl ON DATABASE TYPE RECORD SIGNUP ({capture}) \
                     WITH JWT ALGORITHM HS256 KEY 'sdl';"
                ),
                "DEFINE ACCESS sdl ON DATABASE TYPE RECORD SIGNUP ",
                " WITH JWT ALGORITHM HS256 KEY 'sdl' WITH ISSUER KEY 'sdl' \
                 DURATION FOR TOKEN 1h, FOR SESSION NONE;",
            ),
            Slot::AccessSignin => (
                format!(
                    "DEFINE ACCESS sdl ON DATABASE TYPE RECORD SIGNIN ({capture}) \
                     WITH JWT ALGORITHM HS256 KEY 'sdl';"
                ),
                "DEFINE ACCESS sdl ON DATABASE TYPE RECORD SIGNIN ",
                " WITH JWT ALGORITHM HS256 KEY 'sdl' WITH ISSUER KEY 'sdl' \
                 DURATION FOR TOKEN 1h, FOR SESSION NONE;",
            ),
            Slot::AccessAuthenticate => (
                format!("DEFINE ACCESS sdl ON ROOT TYPE JWT URL 'sdl' AUTHENTICATE ({capture});"),
                "DEFINE ACCESS sdl ON ROOT TYPE JWT URL 'sdl' AUTHENTICATE ",
                " DURATION FOR SESSION NONE;",
            ),
            // The CONTEXT payload rides a record-authenticating
            // access (the engine's define-time demand — the harness
            // mirrors the legal shape); the value prints covered like
            // AUTHENTICATE's.
            Slot::AccessContext => (
                format!(
                    "DEFINE ACCESS sdl ON DATABASE TYPE RECORD \
                     SIGNUP (CREATE user SET email = $email) \
                     WITH JWT ALGORITHM HS256 KEY 'sdl' CONTEXT ({capture});"
                ),
                "DEFINE ACCESS sdl ON DATABASE TYPE RECORD SIGNUP \
                 (CREATE user SET email = $email) WITH JWT ALGORITHM HS256 KEY 'sdl' \
                 WITH ISSUER KEY 'sdl' CONTEXT ",
                " DURATION FOR TOKEN 1h, FOR SESSION NONE;",
            ),
            Slot::AccessUrl => (
                format!("DEFINE ACCESS sdl ON ROOT TYPE JWT URL ({capture});"),
                "DEFINE ACCESS sdl ON ROOT TYPE JWT URL ",
                " DURATION FOR SESSION NONE;",
            ),
            Slot::AccessKey => (
                format!("DEFINE ACCESS sdl ON ROOT TYPE JWT ALGORITHM ES256 KEY ({capture});"),
                "DEFINE ACCESS sdl ON ROOT TYPE JWT ALGORITHM ES256 KEY ",
                " DURATION FOR SESSION NONE;",
            ),
            Slot::AccessIssuerKey => (
                format!(
                    "DEFINE ACCESS sdl ON ROOT TYPE JWT ALGORITHM ES256 KEY 'sdl' \
                     WITH ISSUER KEY ({capture});"
                ),
                "DEFINE ACCESS sdl ON ROOT TYPE JWT ALGORITHM ES256 KEY 'sdl' WITH ISSUER KEY ",
                // The issuer grants issuing capability, so the
                // materialized DURATION gains its TOKEN side.
                " DURATION FOR TOKEN 1h, FOR SESSION NONE;",
            ),
            Slot::AccessDurationGrant => (
                format!(
                    "DEFINE ACCESS sdl ON ROOT TYPE BEARER FOR USER DURATION FOR GRANT \
                     ({capture});"
                ),
                "DEFINE ACCESS sdl ON ROOT TYPE BEARER FOR USER DURATION FOR GRANT ",
                ", FOR TOKEN 1h, FOR SESSION NONE;",
            ),
            Slot::AccessDurationToken => (
                format!(
                    "DEFINE ACCESS sdl ON ROOT TYPE BEARER FOR USER DURATION FOR TOKEN \
                     ({capture});"
                ),
                // The materialized GRANT default prints `4w2d`, not
                // `30d` — the embedded duration printer normalizes to
                // weeks+days (the `30d ≡ 4w2d` fact, spec/access.md).
                "DEFINE ACCESS sdl ON ROOT TYPE BEARER FOR USER DURATION FOR GRANT 4w2d, \
                 FOR TOKEN ",
                ", FOR SESSION NONE;",
            ),
            Slot::AccessDurationSession => (
                format!(
                    "DEFINE ACCESS sdl ON ROOT TYPE BEARER FOR USER DURATION FOR SESSION \
                     ({capture});"
                ),
                "DEFINE ACCESS sdl ON ROOT TYPE BEARER FOR USER DURATION FOR GRANT 4w2d, \
                 FOR TOKEN 1h, FOR SESSION ",
                ";",
            ),
            Slot::TableViewQuery => (
                format!("DEFINE TABLE sdl TYPE ANY SCHEMALESS AS {capture};"),
                "DEFINE TABLE sdl TYPE ANY SCHEMALESS AS ",
                " PERMISSIONS NONE;",
            ),
            Slot::TablePermissionValue => (
                format!(
                    "DEFINE TABLE sdl TYPE NORMAL SCHEMALESS \
                     PERMISSIONS FOR select WHERE ({capture});"
                ),
                "DEFINE TABLE sdl TYPE NORMAL SCHEMALESS PERMISSIONS FOR select WHERE ",
                ", FOR create, update, delete NONE;",
            ),
            Slot::FieldDefault => (
                format!("DEFINE FIELD sdl ON t TYPE string DEFAULT ({capture});"),
                "DEFINE FIELD sdl ON t TYPE string DEFAULT ",
                " PERMISSIONS FULL;",
            ),
            Slot::FieldDefaultAlways => (
                format!("DEFINE FIELD sdl ON t TYPE string DEFAULT ALWAYS ({capture});"),
                "DEFINE FIELD sdl ON t TYPE string DEFAULT ALWAYS ",
                " PERMISSIONS FULL;",
            ),
            Slot::FieldValue => (
                format!("DEFINE FIELD sdl ON t TYPE string VALUE ({capture});"),
                "DEFINE FIELD sdl ON t TYPE string VALUE ",
                " PERMISSIONS FULL;",
            ),
            Slot::FieldAssert => (
                format!("DEFINE FIELD sdl ON t TYPE string ASSERT ({capture});"),
                "DEFINE FIELD sdl ON t TYPE string ASSERT ",
                " PERMISSIONS FULL;",
            ),
            Slot::FieldComputed => (
                format!("DEFINE FIELD sdl ON t TYPE string COMPUTED ({capture});"),
                "DEFINE FIELD sdl ON t TYPE string COMPUTED ",
                " PERMISSIONS FULL;",
            ),
            Slot::FieldOnDelete => (
                format!(
                    "DEFINE FIELD sdl ON t TYPE string \
                     REFERENCE ON DELETE THEN ({capture});"
                ),
                "DEFINE FIELD sdl ON t TYPE string REFERENCE ON DELETE THEN ",
                " PERMISSIONS FULL;",
            ),
            Slot::FieldPermissionValue => (
                format!(
                    "DEFINE FIELD sdl ON t TYPE string \
                     PERMISSIONS FOR select WHERE ({capture});"
                ),
                "DEFINE FIELD sdl ON t TYPE string PERMISSIONS FOR select WHERE ",
                ", FOR create, update FULL;",
            ),
        }
    }
}

/// Why a capture could not be canonicalized. Both variants become the
/// same diagnostic ([`crate::diag::Diag::EmbeddedParseError`]); the
/// split exists so internal invariant breaks are nameable in the
/// ledger, not for different user guidance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EmbeddedError {
    /// The embedded parser rejected the capture. Where the parser
    /// produced a render, the message carries it whole — cause lines
    /// plus its source snippet, whose `--> [line:column]` marker is
    /// re-anchored to the capture's file position whenever the parse
    /// text was the capture itself (stage 1); the empty-capture,
    /// empty-definition, and multi-statement shapes carry hand-written
    /// sentences instead.
    Rejected { message: String },
    /// A slicing invariant broke — the wrapper's print did not have
    /// the expected shape. Never input-validated; always a bug in the
    /// harness constants (e.g. after an upstream bump).
    Internal { message: String },
}

/// A 1-indexed (line, char-column) position inside a source text —
/// the anchor an embedded render's snippet marker shifts by. Columns
/// count chars: upstream's `Location` does too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Position {
    pub(crate) line: usize,
    pub(crate) column: usize,
}

impl Position {
    /// The head of a text — the anchor every computed position in it
    /// builds from: `line_col_at`'s real file anchoring starts here,
    /// and the read/lint paths' fragment and raw-body texts (which
    /// are their own coordinate spaces) sit here whole.
    pub(crate) const ORIGIN: Position = Position { line: 1, column: 1 };
}

/// Moves `position` past `text` (a prefix of the text it anchors):
/// lines split on `\n`, the column restarted or extended in chars.
fn advance(position: Position, text: &str) -> Position {
    let newlines = text.matches('\n').count();
    let tail = match text.rsplit_once('\n') {
        Some((_, tail)) => tail,
        None => text,
    };
    Position {
        line: position.line + newlines,
        column: if newlines > 0 {
            tail.chars().count() + 1
        } else {
            position.column + tail.chars().count()
        },
    }
}

/// The position of a byte offset within its source, for anchoring a
/// capture's snippet markers to the file. Non-boundary offsets floor
/// forward to the next char edge (scanner offsets are boundaries;
/// the guard keeps the helper total).
pub(crate) fn line_col_at(source: &str, offset: usize) -> Position {
    let mut end = offset.min(source.len());
    while end < source.len() && !source.is_char_boundary(end) {
        end += 1;
    }
    advance(Position::ORIGIN, &source[..end])
}

/// Formats an embedded rejection for [`EmbeddedError::Rejected`]: the
/// upstream render whole — cause lines plus its source snippet — with
/// the snippet's `--> [line:column]` marker shifted by `anchor` when
/// one is given. The shift re-anchors the marker only: upstream keeps
/// the caret's column inside the quoted line separate from the
/// location, so the caret stays aligned with the quoted line while
/// the marker points into the anchored text. `offset_location` adds
/// its column to line-1 snippets only; a multi-line capture's later
/// lines keep capture-relative columns, which are exact regardless —
/// a capture's interior lines are the file's own lines verbatim
/// (contiguous slice). `None` forwards verbatim — the stage-2 harness
/// path (its coordinates name the virtual wrapper text, and the exact
/// wrapper→file column shift can be negative, which `offset_location`'s
/// `usize` params cannot express) and the read/lint paths. The cause
/// lines carry no positions; `ParseError`'s redundant `Parse error: `
/// prefix is simply not reproduced (the inner `RenderedError` prints
/// directly).
///
/// The embed's entry points wrap the concrete [`ParseError`] in an
/// `anyhow` error (`syn::lift` is their sole error source), so the
/// render comes back out through `AsRef<dyn Error>` + downcast; a
/// miss — none today — falls back to the wrapper's own render.
/// Upstream's own multi-cause Display quirk (its `errors[2..]` loop
/// skips the second cause) is forwarded as rendered, never repaired.
fn embedded_rejection<E>(error: &E, anchor: Option<Position>) -> String
where
    E: std::convert::AsRef<dyn std::error::Error + Send + Sync + 'static>,
{
    let error = error.as_ref();
    let Some(ParseError::InvalidQuery(rendered)) = error.downcast_ref::<ParseError>() else {
        return error.to_string();
    };
    match anchor {
        Some(Position { line, column }) => rendered
            .clone()
            .offset_location(line - 1, column - 1)
            .to_string(),
        None => rendered.to_string(),
    }
}

/// Canonicalizes one capture for one clause slot: the DDL text the
/// embedded printer emits for that slot, ready for
/// [`crate::codegen`] to print verbatim. `anchor` is the capture's
/// file position — [`Position::ORIGIN`] where the text is its own
/// coordinate space.
pub(crate) fn canonicalize(
    capture: &str,
    slot: Slot,
    anchor: Position,
) -> Result<String, EmbeddedError> {
    let trimmed = capture.trim();
    if trimmed.is_empty() {
        return Err(EmbeddedError::Rejected {
            message: "the clause value is empty — an expression was expected here".to_string(),
        });
    }
    // The parser sees the trimmed text; advance the anchor over the
    // leading whitespace `trim` removed so the snippet markers land
    // on the file positions of the tokens they quote.
    let anchor = advance(
        anchor,
        &capture[..capture.len() - capture.trim_start().len()],
    );
    // Stage 1: the capture must be a complete query on its own. No
    // depth-0 `;` can occur inside it (the scanner consumed the
    // terminator), so a parse cannot legitimately hold more than one
    // top-level statement; multi-statement shapes only arrive inside
    // braces, which stay one block expression.
    let capabilities = experimental_capabilities();
    let parse_query = |query: &str| -> Result<_, EmbeddedError> {
        syn::parse_with_capabilities(query, &capabilities, &ParserConfig::default()).map_err(
            |error| EmbeddedError::Rejected {
                message: embedded_rejection(&error, Some(anchor)),
            },
        )
    };
    let bare = match parse_query(trimmed) {
        Ok(statement) => Some(statement),
        Err(rejection) => {
            // The deprecated-assignment shape: a bare `$var = …`
            // capture is a valid comparison expression (what every
            // slot takes) even though it dies as a top-level
            // statement. The RETURN retry decides expressionhood;
            // a failed retry reports the original rejection, its
            // marker already re-anchored. `None` means the slot rides
            // stage 2 without a bare statement — only the sequence
            // TIMEOUT sentinel reads `bare`, and no sentinel spelling
            // can take this shape (it would have to contain `=`).
            if parse_query(&format!("RETURN {trimmed};")).is_err() {
                return Err(rejection);
            }
            None
        }
    };
    // Stage 2: the wrapper confirms slot validity and yields the
    // clause's canonical text.
    let (wrapper, prefix, suffix) = slot.harness(trimmed);
    let statement = syn::parse_with_capabilities(&wrapper, &capabilities, &ParserConfig::default())
        .map_err(|error| EmbeddedError::Rejected {
            message: format!(
                "not valid in this clause position: {}",
                embedded_rejection(&error, None)
            ),
        })?;
    debug_assert_eq!(
        statement.num_statements(),
        1,
        "a capture cannot hold multiple top-level statements"
    );
    // The embedded print with the module-call space repaired
    // (`repair_module_call_spaces` — the static slice bounds name no
    // `mod:: ` spelling, so the repair cannot move them).
    let printed = repair_module_call_spaces(&statement.to_sql());
    let Some(clause) = printed
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
    else {
        // The one shape where the wrapper print can lose its clause:
        // v3.2.4's sequence ToSql elides a TIMEOUT holding exactly
        // `Literal::None`, so any capture that parses to the `none`
        // sentinel — `none`, `NONE`, `(none)`, trailing comments —
        // leaves no TIMEOUT clause to slice. Stage 1 proved the
        // capture a complete query and stage 2 accepted it in the
        // slot, so this is the sentinel, not an invariant break: canon
        // is the bare print (`NONE`, probe-verified for every
        // sentinel spelling), which the server accepts and stores as
        // the same absent value.
        if matches!(slot, Slot::SequenceTimeout) {
            let Some(bare) = bare else {
                return Err(EmbeddedError::Internal {
                    message: format!(
                        "TIMEOUT capture `{trimmed}` reached the sentinel without a bare parse"
                    ),
                });
            };
            let bare_printed = repair_module_call_spaces(&bare.to_sql());
            let Some(sentinel) = bare_printed.strip_suffix(';') else {
                return Err(EmbeddedError::Internal {
                    message: format!(
                        "canonical print of `{trimmed}` did not end in `;`: `{bare_printed}`"
                    ),
                });
            };
            return Ok(sentinel.to_string());
        }
        return Err(EmbeddedError::Internal {
            message: format!(
                "canonical print of `{wrapper}` did not have the expected shape: `{printed}`"
            ),
        });
    };
    Ok(clause.to_string())
}

/// Validates verbatim DDL through the embedded parser — two
/// consumers: the lint's raw-body probe (rule 8's advisory) and,
/// through `codegen::validate_ddl`'s public wrapper,
/// the stage-2 oracle the wasm `check` runs over the whole emission.
/// One or more statements, capabilities on (the same experimental
/// set every other embed site uses), and no re-print (the body rides
/// verbatim; only its parseability is in question). `Err` carries the
/// upstream render verbatim (the body text is its own coordinate
/// space), `canonicalize_statement`'s error shape.
pub(crate) fn probe_statements(ddl: &str) -> Result<(), String> {
    let mut query = ddl.trim().to_string();
    if !query.is_empty() && !query.ends_with(';') {
        query.push(';');
    }
    syn::parse_with_capabilities(
        &query,
        &experimental_capabilities(),
        &ParserConfig::default(),
    )
    .map_err(|error| embedded_rejection(&error, None))?;
    Ok(())
}

/// Validates one INFO DDL statement through the embedded parser and
/// re-prints it canonically: the reader's
/// role for the embed is VALIDATE + CANONICALIZE, so the SDL-side
/// splitters face exactly one deterministic dialect — this printer's.
/// A missing trailing `;` is supplied (INFO never prints one). The
/// input must be exactly one statement; the re-print ends with `;`.
pub(crate) fn canonicalize_statement(ddl: &str) -> Result<String, EmbeddedError> {
    let trimmed = ddl.trim();
    if trimmed.is_empty() {
        return Err(EmbeddedError::Rejected {
            message: "the definition is empty — a DDL statement was expected".to_string(),
        });
    }
    let mut query = trimmed.to_string();
    if !query.ends_with(';') {
        query.push(';');
    }
    let ast = syn::parse_with_capabilities(
        &query,
        &experimental_capabilities(),
        &ParserConfig::default(),
    )
    .map_err(|error| EmbeddedError::Rejected {
        message: embedded_rejection(&error, None),
    })?;
    let statements = ast.num_statements();
    if statements != 1 {
        return Err(EmbeddedError::Rejected {
            message: format!("expected one statement, found {statements}"),
        });
    }
    Ok(repair_module_call_spaces(&ast.to_sql()))
}

/// The embedded AST behind a canonical expression text — the
/// pretty-printer's input ([`crate::pretty`]). The tree that
/// canonicalized the capture is not retained (the AST stores the
/// canon text), so the shape is read back through the same wrapper
/// trick the bridge itself uses: `RETURN <canon>;` puts a
/// value-shaped canon into expression position, and braced canon
/// (function bodies, `THEN` blocks) reads through the function
/// wrapper's block position — the grammar position that accepts a
/// block unambiguously. Anything neither wrapper accepts (view
/// queries: statement-shaped canon with no wrapper-free expression
/// reading) is [`CanonTree::Opaque`] and stays a single line.
#[derive(PartialEq)]
pub(crate) enum CanonTree {
    /// A value-shaped expression.
    Expr(surrealdb_sql::Expr),
    /// A braced statement block, its elements in order.
    Block(Vec<surrealdb_sql::Expr>),
    /// No wrapper accepts the shape; the canon text rides verbatim.
    Opaque,
}

/// Reads the canon text back into its embedded tree. Infallible by
/// construction: every path either reads a tree the embedded parser
/// itself built from this exact text or reports [`CanonTree::Opaque`].
pub(crate) fn canon_tree(canon: &str) -> CanonTree {
    let capabilities = experimental_capabilities();
    // Braced canon is ambiguous between a statement block, an object
    // literal, and a set literal — the block wrapper decides (an
    // object or set body dies in statement position), and the RETURN
    // retry below catches whatever the block rejects.
    if canon.starts_with('{') {
        let wrapper = format!("DEFINE FUNCTION fn::sdl() {canon};");
        let Ok(ast) =
            syn::parse_with_capabilities(&wrapper, &capabilities, &ParserConfig::default())
        else {
            return return_wrapped(canon, &capabilities);
        };
        if let [surrealdb_sql::TopLevelExpr::Expr(surrealdb_sql::Expr::Define(define))] =
            ast.expressions.as_slice()
            && let surrealdb_sql::statements::define::DefineStatement::Function(function) =
                define.as_ref()
        {
            return CanonTree::Block(function.block.0.clone());
        }
    }
    return_wrapped(canon, &capabilities)
}

/// The `RETURN` wrapper: a value-shaped canon in expression position.
fn return_wrapped(canon: &str, capabilities: &Capabilities) -> CanonTree {
    let wrapper = format!("RETURN {canon};");
    let Ok(ast) = syn::parse_with_capabilities(&wrapper, capabilities, &ParserConfig::default())
    else {
        return CanonTree::Opaque;
    };
    match ast.expressions.as_slice() {
        [surrealdb_sql::TopLevelExpr::Expr(surrealdb_sql::Expr::Return(output))] => {
            CanonTree::Expr(output.what.clone())
        }
        _ => CanonTree::Opaque,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The origin-anchored form: these pins hold the capture text
    /// alone, with no file around it. Rejection-anchoring pins call
    /// `super::canonicalize` with a real `Position`.
    fn canonicalize(capture: &str, slot: Slot) -> Result<String, EmbeddedError> {
        super::canonicalize(capture, slot, Position::ORIGIN)
    }

    /// The slice bounds, pinned against the embedded parser exactly
    /// as probed (re-pinned at the 3.3.0 floor move). Any of these
    /// failing after an upstream bump
    /// means the wrapper templates or bounds in [`Slot::harness`] are
    /// stale.
    #[test]
    fn slice_bounds_match_the_embedded_printer() {
        assert_eq!(canonicalize("50", Slot::ParamValue), Ok("50".to_string()));
        assert_eq!(
            canonicalize("level > 10", Slot::ParamPermissions),
            Ok("level > 10".to_string())
        );
        assert_eq!(
            canonicalize("$auth.admin = true", Slot::ModulePermissions),
            Ok("$auth.admin = true".to_string())
        );
        // `none` canon prints uppercase (the None literal — the same
        // print the TIMEOUT sentinel rides); the fold to
        // Permission::None is case-insensitive.
        assert_eq!(
            canonicalize("none", Slot::ModulePermissions),
            Ok("NONE".to_string())
        );
        assert_eq!(
            canonicalize("1000", Slot::SequenceStart),
            Ok("1000".to_string())
        );
        assert_eq!(
            canonicalize("100", Slot::SequenceBatch),
            Ok("100".to_string())
        );
        assert_eq!(
            canonicalize("5s", Slot::SequenceTimeout),
            Ok("5s".to_string())
        );
        assert_eq!(
            canonicalize("'memory'", Slot::BucketBackend),
            Ok("'memory'".to_string())
        );
        assert_eq!(
            canonicalize("$auth.admin = true", Slot::BucketPermissions),
            Ok("$auth.admin = true".to_string())
        );
        assert_eq!(
            canonicalize("1h", Slot::UserDurationToken),
            Ok("1h".to_string())
        );
        assert_eq!(
            canonicalize("12h", Slot::UserDurationSession),
            Ok("12h".to_string())
        );
        // The `none` sentinel rides the wrapper for the user slots —
        // unlike sequence's TIMEOUT, the user emitter prints the
        // sentinel's clause unconditionally (`TOKEN NONE` is the
        // probe-pinned default-session print), so there is no
        // elision to fall back around.
        assert_eq!(
            canonicalize("none", Slot::UserDurationToken),
            Ok("NONE".to_string())
        );
        assert_eq!(
            canonicalize("none", Slot::UserDurationSession),
            Ok("NONE".to_string())
        );
        // `event`'s slots, pinned against the embedded v3.2.4 printer:
        // WHEN prints unconditionally (the emitter materializes the
        // statement-default `true`), so the THEN wrapper's prefix
        // carries `WHEN true` and the WHEN wrapper's suffix carries
        // the dummy `THEN 1`. A bare statement-like item comes back
        // CoverStmts-wrapped; a block stays a block.
        assert_eq!(
            canonicalize("$event = 'CREATE'", Slot::EventWhen),
            Ok("$event = 'CREATE'".to_string())
        );
        assert_eq!(
            canonicalize("true", Slot::EventWhen),
            Ok("true".to_string())
        );
        // THEN's CoverStmts block print is element-counted, like
        // every block print upstream: a single-statement block has
        // no trailing `;` (`{ CREATE log … }`), a multi-statement
        // one is `;`-terminated — probe-pinned against the embedded
        // printer.
        assert_eq!(
            canonicalize("{ create log set at = time::now(); }", Slot::EventThen),
            Ok("{ CREATE log SET at = time::now() }".to_string())
        );
        assert_eq!(
            canonicalize(
                "update counter:posts set total = total + 1",
                Slot::EventThen
            ),
            Ok("(UPDATE counter:posts SET total = total + 1)".to_string())
        );
        // `function`'s body slot, pinned against the embedded v3.2.4
        // printer: the canon is the whole block (braces included),
        // PERMISSIONS FULL is unconditional (the parse default — the
        // wrapper carries no permission clause), and the block print
        // is element-counted (`{;}` for an empty or comment-only
        // body, bare single statement, `;` after every statement of a
        // multi-statement body). Inside a body, a bare `$var = …`
        // dies as the deprecated assignment STATEMENT — the block
        // grammar has no RETURN retry to save it; `let $x = …` is the
        // spelling there.
        assert_eq!(
            canonicalize("{ let $x = 1; $x }", Slot::FunctionBody),
            Ok("{ LET $x = 1; $x; }".to_string())
        );
        assert_eq!(
            canonicalize("{ 'pong' }", Slot::FunctionBody),
            Ok("{ 'pong' }".to_string())
        );
        assert_eq!(
            canonicalize("{ }", Slot::FunctionBody),
            Ok("{;}".to_string())
        );
        assert_eq!(
            canonicalize("{ /* only a comment */ }", Slot::FunctionBody),
            Ok("{;}".to_string())
        );
        assert_eq!(
            canonicalize("{ if $x { throw 'no' } else { 1 } }", Slot::FunctionBody),
            Ok("{ IF $x { THROW 'no' } ELSE { 1 } }".to_string())
        );
        // The permissions arm rides function's own wrapper: the
        // PERMISSIONS WHERE clause prints with the dummy body block
        // ahead of it, no default materialization after it.
        assert_eq!(
            canonicalize("$auth.admin = true", Slot::FunctionPermissions),
            Ok("$auth.admin = true".to_string())
        );
        assert_eq!(
            canonicalize("level > 10", Slot::FunctionPermissions),
            Ok("level > 10".to_string())
        );
        // `config`'s slots, pinned against the embedded printer: the
        // api permissions print behind the statement's own grammar
        // position; the middleware argument is a single-argument call
        // so the printed interior is exactly the argument's canon
        // (CoverStmts wrapping included); the default-config sides
        // carry the upstream emitter's double space after OVERWRITE
        // (the kind writes nothing, the inner emitter starts with a
        // space of its own — the print quirk the prefixes pin).
        assert_eq!(
            canonicalize("$auth.admin = true", Slot::ConfigApiPermissions),
            Ok("$auth.admin = true".to_string())
        );
        assert_eq!(
            canonicalize("math::round(1, 2)", Slot::ConfigMiddlewareArg),
            Ok("math::round(1, 2)".to_string())
        );
        assert_eq!(
            canonicalize("CREATE audit", Slot::ConfigMiddlewareArg),
            Ok("(CREATE audit)".to_string())
        );
        assert_eq!(
            canonicalize("'acme'", Slot::ConfigDefaultNamespace),
            Ok("'acme'".to_string())
        );
        assert_eq!(
            canonicalize("'blog'", Slot::ConfigDefaultDatabase),
            Ok("'blog'".to_string())
        );
        // `index`'s slots, pinned against the embedded v3.2.4 printer:
        // the FIELDS print carries no index kind (plain `Idx` prints
        // nothing) and no trailing clauses at their defaults; the
        // COUNT WHERE print rides the kind clause's own condition
        // position, with COUNT printed unconditionally. Multi-item
        // lists are codegen's comma join over per-item slices — one
        // item here pins the single-item shape; statement-like items
        // CoverStmts-wrap like every embedded print.
        assert_eq!(
            canonicalize("account", Slot::IndexFields),
            Ok("account".to_string())
        );
        assert_eq!(
            canonicalize("create audit_log", Slot::IndexFields),
            Ok("(CREATE audit_log)".to_string())
        );
        assert_eq!(
            canonicalize("status = 'active'", Slot::IndexWhere),
            Ok("status = 'active'".to_string())
        );
        assert_eq!(
            canonicalize("$event = 'CREATE'", Slot::IndexWhere),
            Ok("$event = 'CREATE'".to_string())
        );
        // `api`'s slots, pinned against the embedded v3.2.4 printer:
        // the action slot's print carries the emitter's
        // always-materialized `FOR any PERMISSIONS FULL` (the
        // statement-default config) ahead of the group, and the
        // group's own default PERMISSIONS behind it. The action
        // prints UNWRAPPED — a statement-like action stays bare —
        // while the fallback (probed through the fallback-only
        // wrapper, which parses with zero actions) CoverStmts-wraps
        // like the other statement-like slots.
        assert_eq!(
            canonicalize("{ { status: 200, body: { ok: true } } }", Slot::ApiAction),
            Ok("{ { status: 200, body: { ok: true } } }".to_string())
        );
        assert_eq!(
            canonicalize("create audit", Slot::ApiAction),
            Ok("CREATE audit".to_string())
        );
        assert_eq!(
            canonicalize("$auth.admin = true", Slot::ApiAction),
            Ok("$auth.admin = true".to_string())
        );
        assert_eq!(
            canonicalize("{ { status: 404 } }", Slot::ApiFallback),
            Ok("{ { status: 404 } }".to_string())
        );
        assert_eq!(
            canonicalize("create fallback", Slot::ApiFallback),
            Ok("(CREATE fallback)".to_string())
        );
        // `access`'s slots, pinned against the embedded v3.2.4 printer.
        // The record slots ride an explicit symmetric jwt in the
        // wrapper: the jwt-less record's default materializes a RANDOM
        // HS512 key (no static suffix could slice that print), while
        // HS256 mirrors a known issuer. The verify-key slot rides an
        // ASYMMETRIC algorithm so the print stays issuer-less (HS*
        // would mirror the capture into a capture-dependent tail), and
        // the issuer-key slot the same trick behind `WITH ISSUER KEY`.
        // The duration sides ride the always-grant-capable bearer; the
        // emitter materializes the other sides' defaults around the
        // sliced one. Statement-like values CoverStmts-wrap like
        // everywhere else.
        assert_eq!(
            canonicalize(
                "create user set password = crypto::argon2::generate($password)",
                Slot::AccessSignup
            ),
            Ok("(CREATE user SET password = crypto::argon2::generate($password))".to_string())
        );
        assert_eq!(
            canonicalize(
                "select * from user where email = $email and crypto::argon2::compare(password, $password)",
                Slot::AccessSignin
            ),
            Ok(
                "(SELECT * FROM user WHERE email = $email AND crypto::argon2::compare(password, $password))"
                    .to_string()
            )
        );
        // The paren buffer's reason to exist: an unbuffered SELECT
        // capture glues with the wrapper's trailing `WITH JWT` (it
        // reads as a SELECT index hint — stage 2 rejects). Buffered,
        // a bare select canonizes WITH its CoverStmts parens, so the
        // emitted DDL re-parses.
        assert_eq!(
            canonicalize("select * from user", Slot::AccessSignup),
            Ok("(SELECT * FROM user)".to_string())
        );
        assert_eq!(
            canonicalize(
                "'https://sso.acme.com/.well-known/jwks.json'",
                Slot::AccessUrl
            ),
            Ok("'https://sso.acme.com/.well-known/jwks.json'".to_string())
        );
        assert_eq!(
            canonicalize(
                "'https://sso.acme.com/.well-known/jwks.json'",
                Slot::AccessUrl
            ),
            Ok("'https://sso.acme.com/.well-known/jwks.json'".to_string())
        );
        assert_eq!(
            canonicalize("$secret", Slot::AccessKey),
            Ok("$secret".to_string())
        );
        assert_eq!(
            canonicalize("$signing_key", Slot::AccessIssuerKey),
            Ok("$signing_key".to_string())
        );
        // Duration canon rides the embedded printer's own duration
        // normalization: `30d` re-prints as `4w2d` (weeks+days — the
        // spec's `30d ≡ 4w2d` fact, and the reason durations compare
        // by parsed value, never spelling).
        assert_eq!(
            canonicalize("30d", Slot::AccessDurationGrant),
            Ok("4w2d".to_string())
        );
        assert_eq!(
            canonicalize("1h", Slot::AccessDurationToken),
            Ok("1h".to_string())
        );
        assert_eq!(
            canonicalize("8h", Slot::AccessDurationSession),
            Ok("8h".to_string())
        );
        assert_eq!(
            canonicalize("none", Slot::AccessDurationSession),
            Ok("NONE".to_string())
        );
        // `table`'s slots, pinned against the embedded v3.2.4 printer.
        // The view's query rides the fixed `TYPE ANY SCHEMALESS AS`
        // emission; the print's tail is the emitter's
        // ALWAYS-materialized `PERMISSIONS NONE` (a table permission
        // prints unconditionally — with none written it is the
        // all-NONE single value). The restricted-select grammar
        // enforces itself in the AS position (upstream's parse_view).
        assert_eq!(
            canonicalize(
                "select author, count() as posts from post group by author",
                Slot::TableViewQuery
            ),
            Ok("SELECT author, count() AS posts FROM post GROUP BY author".to_string())
        );
        assert_eq!(
            canonicalize("select * from post", Slot::TableViewQuery),
            Ok("SELECT * FROM post".to_string())
        );
        // The per-op value's print tail is the emitter's own
        // materialization of the three unlisted operations (grouped
        // `FOR create, update, delete NONE` — the emitter prints the
        // full op set once any per-op value is written). The paren
        // buffer is canon-transparent for simple values and gives
        // statement-like ones their CoverStmts parens.
        assert_eq!(
            canonicalize("$auth.id = id", Slot::TablePermissionValue),
            Ok("$auth.id = id".to_string())
        );
        assert_eq!(
            canonicalize("select * from user", Slot::TablePermissionValue),
            Ok("(SELECT * FROM user)".to_string())
        );
        // `field`'s slots, pinned against the embedded v3.2.4 printer.
        // Every expression slot rides the statement's own grammar
        // position (paren-buffered like every clause position whose
        // statement-like values could glue with a trailing clause),
        // and the print's tail is the ALWAYS-materialized
        // `PERMISSIONS FULL` (fields print PERMISSIONS
        // unconditionally; the parse default is all-FULL — the
        // opposite of the table's grouped NONE tail). The ON DELETE
        // THEN slot keeps statement-like braced values as their block
        // print (no CoverStmts parens survive around braces).
        assert_eq!(canonicalize("50", Slot::FieldDefault), Ok("50".to_string()));
        assert_eq!(
            canonicalize("'x'", Slot::FieldDefaultAlways),
            Ok("'x'".to_string())
        );
        assert_eq!(
            canonicalize("true", Slot::FieldValue),
            Ok("true".to_string())
        );
        assert_eq!(
            canonicalize("$value > 0", Slot::FieldAssert),
            Ok("$value > 0".to_string())
        );
        assert_eq!(
            canonicalize("string::uppercase(name)", Slot::FieldComputed),
            Ok("string::uppercase(name)".to_string())
        );
        assert_eq!(
            canonicalize("$event = 'CREATE'", Slot::FieldComputed),
            Ok("$event = 'CREATE'".to_string())
        );
        assert_eq!(
            canonicalize(
                "{ create cleanup_log set at = time::now() }",
                Slot::FieldOnDelete
            ),
            Ok("{ CREATE cleanup_log SET at = time::now() }".to_string())
        );
        assert_eq!(
            canonicalize("create audit", Slot::FieldOnDelete),
            Ok("(CREATE audit)".to_string())
        );
        // The per-op value's tail groups the unlisted operations as
        // FULL — fields seed allow, tables seed deny.
        assert_eq!(
            canonicalize("$auth.admin = true", Slot::FieldPermissionValue),
            Ok("$auth.admin = true".to_string())
        );
        assert_eq!(
            canonicalize("select * from user", Slot::FieldPermissionValue),
            Ok("(SELECT * FROM user)".to_string())
        );
    }

    /// `DEFINE BUCKET` is gated *inside the v3.2.4 parser*
    /// (`files_enabled`), so plain [`syn::parse`] rejects the wrapper
    /// — the reason canonicalize runs with the experimental
    /// capabilities enabled for every slot, not just bucket's.
    #[test]
    fn bucket_slots_need_the_experimental_files_capability() {
        assert!(syn::parse("DEFINE BUCKET sdl BACKEND 'memory';").is_err());
        assert_eq!(
            canonicalize("'file:/srv/uploads'", Slot::BucketBackend),
            Ok("'file:/srv/uploads'".to_string())
        );
    }

    /// Codegen prints `canon` verbatim, so the embedded printer's
    /// normalization is SDL's: comments dropped, whitespace and
    /// keyword casing canonicalized, statement-like expressions
    /// wrapped in `CoverStmts` parens, blocks and objects braced.
    #[test]
    fn canon_is_the_embedded_print_not_the_source() {
        let cases = [
            ("1 /* note */  +\n1", "1 + 1"),
            // Statement-like expressions print wrapped.
            ("create foo", "(CREATE foo)"),
            ("select name from user", "(SELECT name FROM user)"),
            ("if $x { 1 } else { 2 }", "(IF $x { 1 } ELSE { 2 })"),
            // Blocks and objects keep their braces; block statements
            // gain their `;` and uppercased keywords.
            ("{ a: 1 }", "{ a: 1 }"),
            ("{ let $y = 1; $y }", "{ LET $y = 1; $y; }"),
            ("{ 1; 2 }", "{ 1; 2; }"),
            (
                "{ string::split('a.b', '.')[0] }",
                "{ string::split('a.b', '.')[0] }",
            ),
        ];
        for (capture, expected) in cases {
            assert_eq!(
                canonicalize(capture, Slot::ParamValue),
                Ok(expected.to_string()),
                "capture: {capture:?}"
            );
        }
    }

    /// Trivia around the capture is scanner business, not canon.
    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            canonicalize("\n  50  \n", Slot::ParamValue),
            Ok("50".to_string())
        );
    }

    /// The deprecated-assignment retry (stage 1): a bare `$var = …`
    /// capture is rejected as a top-level statement but is a valid
    /// comparison expression — event `when:`'s idiom, and param
    /// `value:`'s too. The RETURN retry decides expressionhood; the
    /// leak guard rides it (a leaked clause keyword is not an
    /// expression in RETURN position either).
    #[test]
    fn bare_variable_comparisons_canonize_through_the_return_retry() {
        assert_eq!(
            canonicalize("$flag = true", Slot::ParamValue),
            Ok("$flag = true".to_string())
        );
        assert_eq!(
            canonicalize("$event = 'CREATE'", Slot::EventWhen),
            Ok("$event = 'CREATE'".to_string())
        );
        assert!(matches!(
            canonicalize("1 COMMENT 'x'", Slot::EventThen),
            Err(EmbeddedError::Rejected { .. })
        ));
    }

    /// The stage-1 guard: clause-keyword leaks die at the bare parse,
    /// with the embedded render forwarded and its snippet marker
    /// re-anchored to the capture's position (ORIGIN here, so the
    /// capture-relative column).
    #[test]
    fn clause_keyword_leaks_are_rejected() {
        let error = canonicalize("1 COMMENT 'x'", Slot::ParamValue).expect_err("must reject");
        let EmbeddedError::Rejected { message } = error else {
            panic!("expected Rejected: {error:?}");
        };
        assert!(
            message.contains("Unexpected token `COMMENT`"),
            "message: {message}"
        );
        assert!(message.contains("--> [1:3]"), "message: {message}");
        // The SurrealQL `WHERE` spelling is not an SDL expression.
        assert!(matches!(
            canonicalize("where x > 1", Slot::ParamPermissions),
            Err(EmbeddedError::Rejected { .. })
        ));
        // Trailing junk likewise.
        assert!(matches!(
            canonicalize("1 2", Slot::SequenceStart),
            Err(EmbeddedError::Rejected { .. })
        ));
    }

    #[test]
    fn empty_capture_is_rejected_with_our_message() {
        let error = canonicalize("   ", Slot::ParamValue).expect_err("must reject");
        assert_eq!(
            error,
            EmbeddedError::Rejected {
                message: "the clause value is empty — an expression was expected here".to_string()
            }
        );
    }

    /// The module-call space repair at both print doors (#41):
    /// upstream's `Function::Module` arm prints `mod:: name::fn(…)`
    /// — the quirk the engine's own INFO print carries — and the
    /// clause slice and the read path's whole-statement re-print hand
    /// SDL the glued spelling.
    #[test]
    fn the_module_call_space_repairs_at_both_print_doors() {
        assert_eq!(
            canonicalize("mod::thumbnail::render(b\"00\", 512)", Slot::EventThen),
            Ok("mod::thumbnail::render(b\"00\", 512)".to_string())
        );
        assert_eq!(
            canonicalize("{ let $x = mod:: a::b(1); $x }", Slot::FunctionBody),
            Ok("{ LET $x = mod::a::b(1); $x; }".to_string())
        );
        assert_eq!(
            canonicalize_statement(
                "DEFINE EVENT sdl ON t WHEN true \
                 THEN mod:: thumbnail::render(b\"00\", 512)"
            ),
            Ok(
                "DEFINE EVENT sdl ON t WHEN true THEN mod::thumbnail::render(b\"00\", 512);"
                    .to_string()
            )
        );
    }

    /// The `none` sentinel takes the bare-print fallback for the
    /// TIMEOUT slot (the wrapper's ToSql elides `Literal::None` —
    /// probe-pinned) but rides the wrapper everywhere else (`VALUE`
    /// prints unconditionally). The elision is AST-based, so every
    /// spelling that *parses* to the sentinel must canonize — not
    /// just the bare word.
    #[test]
    fn timeout_none_sentinel_canonizes_from_the_bare_parse() {
        assert_eq!(
            canonicalize("none", Slot::SequenceTimeout),
            Ok("NONE".to_string())
        );
        assert_eq!(
            canonicalize("NONE", Slot::SequenceTimeout),
            Ok("NONE".to_string())
        );
        // Parenthesized and comment-carrying spellings parse to the
        // same sentinel.
        assert_eq!(
            canonicalize("(none)", Slot::SequenceTimeout),
            Ok("NONE".to_string())
        );
        assert_eq!(
            canonicalize("NONE /* sentinel */", Slot::SequenceTimeout),
            Ok("NONE".to_string())
        );
        assert_eq!(
            canonicalize("none // disabled\n", Slot::SequenceTimeout),
            Ok("NONE".to_string())
        );
        // Other slots keep the wrapper path (`VALUE` prints
        // unconditionally, so there is nothing to fall back for).
        assert_eq!(
            canonicalize("none", Slot::ParamValue),
            Ok("NONE".to_string())
        );
        assert_eq!(
            canonicalize("(none)", Slot::ParamValue),
            Ok("NONE".to_string())
        );
    }

    /// The anchor arithmetic: byte offsets become 1-indexed lines and
    /// char columns — multi-byte chars are one column (upstream's
    /// `Location` counts chars), and past-the-end floors to the text
    /// end.
    #[test]
    fn line_col_at_counts_lines_and_chars() {
        assert_eq!(line_col_at("", 0), Position::ORIGIN);
        assert_eq!(line_col_at("ab\ncd", 0), Position { line: 1, column: 1 });
        assert_eq!(line_col_at("ab\ncd", 2), Position { line: 1, column: 3 });
        assert_eq!(line_col_at("ab\ncd", 3), Position { line: 2, column: 1 });
        assert_eq!(line_col_at("ab\ncd", 5), Position { line: 2, column: 3 });
        // `é` is two bytes, one char/one column.
        assert_eq!(line_col_at("éx", 2), Position { line: 1, column: 2 });
        assert_eq!(line_col_at("ab", 99), Position { line: 1, column: 3 });
    }

    /// The span-precision case: a long capture whose rejection
    /// lands late. The forwarded snippet's marker names the file
    /// position of the token it quotes, not the capture's start.
    #[test]
    fn rejection_markers_are_re_anchored_to_the_file() {
        // `COMMENT` is capture char 7; the capture sits at file
        // line 7, column 25 → the marker lands at [7:31].
        let Err(EmbeddedError::Rejected { message }) = super::canonicalize(
            "1 + 1 COMMENT 'x'",
            Slot::ParamValue,
            Position {
                line: 7,
                column: 25,
            },
        ) else {
            panic!("expected Rejected");
        };
        assert!(message.contains("--> [7:31]"), "message: {message}");
    }

    /// A multi-line capture: the line shift covers every snippet, the
    /// column shift only line 1 (upstream's rule), so line 2's marker
    /// keeps the capture-relative column while naming the right file
    /// line.
    #[test]
    fn multi_line_capture_markers_shift_lines() {
        let Err(EmbeddedError::Rejected { message }) = super::canonicalize(
            "1 +\n1 COMMENT 'x'",
            Slot::ParamValue,
            Position { line: 3, column: 9 },
        ) else {
            panic!("expected Rejected");
        };
        // The rejection rides on capture line 2's `COMMENT`.
        assert!(message.contains("--> [4:3]"), "message: {message}");
    }

    /// Stage-2 rejections forward the harness render verbatim: the
    /// wrapper is virtual text absent from the file, so no
    /// re-anchoring is applied — marker, caret and quoted line stay
    /// internally consistent (harness coordinates).
    #[test]
    fn stage2_rejections_forward_the_harness_render_verbatim() {
        // A full DEFINE statement parses as a top-level query (stage
        // 1) but no field-default clause takes one (stage 2).
        let Err(EmbeddedError::Rejected { message }) =
            super::canonicalize("DEFINE TABLE x;", Slot::FieldDefault, Position::ORIGIN)
        else {
            panic!("expected Rejected");
        };
        assert!(
            message.starts_with("not valid in this clause position: "),
            "message: {message}"
        );
        // The harness's own line, quoted with its own coordinates —
        // both snippets: the offending `;` (char 58) and the `(` the
        // wrapper wants closed (char 43).
        assert!(
            message.contains("DEFINE FIELD sdl ON t TYPE string DEFAULT"),
            "message: {message}"
        );
        assert!(message.contains("--> [1:58]"), "message: {message}");
        assert!(message.contains("--> [1:43]"), "message: {message}");
    }
}
