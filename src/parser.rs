//! Hand-written recursive-descent parser: one clause-loop per
//! statement over the [`crate::lexer`] scanner; expected-sets and
//! did-you-mean on every unknown-keyword path.
//!
//! Shape (spec rules 2–3): the parser drives the
//! [`Scanner`] and decides when a capture begins. The file driver
//! collects a doc-comment block, reads the statement keyword, and
//! dispatches; statement-level recovery rides the depth-0 `;` scanner
//! so every statement in the file is diagnosed (file-level
//! `Vec<Diag>`), and clause-level recovery skips to the next known
//! clause boundary so one typo never blanks the rest of a statement.
//! Statements fold repeated clauses last-one-wins (rule 7) as they
//! gain clauses; a clause block is optional when empty.
//!
//! Expression clauses (rule 3) capture to the depth-0 `;` and hand
//! the span to [`crate::expr`] for canonicalization at parse time;
//! the assignment short form (`key = <expr>;`) rides the same
//! capture, with EOF standing in for the `;` on the file's final
//! statement.

use crate::ast::{
    Access, AccessDuration, AccessKind, Algorithm, Analyzer, Api, ApiAction, ApiAnyBlock,
    ApiConfig, ApiGroupConfig, ApiMethod, Async, Authored, BearerSubject, Bm25, Bucket, Carried,
    ChangeFeed, ClauseComment, Config, Database, DefaultConfig, Distance, DocComment, Event,
    Executable, Expr, Family, Field, FieldDefault, FieldPath, FieldPathSeg, Filter, Float, FnKind,
    FnPath, Function, Graphql, GraphqlConfig, Ident, Include, Index, IndexKind, Introspection,
    JwtIssuer, JwtSource, JwtVerify, Kind, Language, Level, MemberKey, Middleware, Mixin, Module,
    Namespace, OnDelete, OpPermissions, Param, ParamName, Pass, Permission, PermissionGroup,
    PermissionOp, Raw, Role, SdlFile, Sequence, Spread, Stmt, Table, TableKind, Tokenizer,
    TypeAlias, User, VectorType,
};
use crate::diag::Diag;
use crate::expr::{self, EmbeddedError, Slot};
use crate::kind::{kind_contains_object, parse_kind_text, read_kind_with_aliases};
use crate::lexer::{Scanner, Span, lex_string_literal, strip_capture_trivia, word_prefix_len};
use std::str::FromStr;
use surrealdb_types::Duration;

/// The statement keywords, with SDL-level aliases (rule 9): `ns` and
/// `db` are accepted because v3.2.4 maps them onto the same keyword
/// tokens as `namespace` and `database`. Codegen always emits the
/// canonical spelling.
const STATEMENT_KEYWORDS: &[&str] = &[
    "namespace",
    "ns",
    "param",
    "sequence",
    "database",
    "db",
    "bucket",
    "user",
    "analyzer",
    "event",
    "module",
    "fn",
    "config",
    "index",
    "api",
    "access",
    "table",
    "field",
    "mixin",
    "type",
    "raw",
    "include",
];

/// How a statement's known clause parses (spec rule 2's clause
/// forms). The separator after the key selects among the forms: `:`
/// starts the value forms (expression capture, keyword set, duration
/// literal), `{` opens a sub-block, a bare `;` (or the position where
/// one belongs) confirms a flag.
enum ClauseKind {
    /// `key: <expr>;` — captured to the depth-0 `;` and canonicalized
    /// for one embedded-parser slot.
    Expr(Slot),
    /// `permissions: none | full | <expr>;` — the single-value form;
    /// the slot is the wrapper the expression arm canonicalizes
    /// through (param's or bucket's).
    Permissions(Slot),
    /// `key: <duration>;` — a SurrealQL duration literal, SDL-lexed
    /// (rule 3's keyword-clause kind: never an expression capture).
    Duration,
    /// `key;` — a bare flag.
    Flag,
    /// `key { clauses };` — a colon-less sub-block (rule 2) whose
    /// body parses through the nested clause table.
    SubBlock(&'static [(&'static str, ClauseKind)]),
    /// `key: <level>;` — a keyword set with input-only aliases
    /// (`root | kv | namespace | ns | database | db`), canonicalized
    /// at parse time; codegen emits the full word (rule 9).
    Level,
    /// `key: '<string>';` — a SurrealQL string literal, SDL-lexed and
    /// decoded to its value (rule 3's keyword clause).
    Str,
    /// `key: <word>, <word>, …;` — a non-empty comma list of bare
    /// words from a fixed set (rule 3's keyword clause; wrapped lists
    /// split at depth-0 `,` under the capture scanner's skip set).
    /// Repeats of the clause replace the whole list (last-one-wins).
    WordList(&'static [&'static str]),
    /// `key: '<string>', '<string>', …;` — a non-empty comma list of
    /// SurrealQL string literals, SDL-lexed and decoded (rule 3's
    /// keyword clause in its string-list form; access `audience:` is
    /// the consumer). Items split at depth-0 `,` under the capture
    /// scanner's skip set; every bad item is diagnosed and one bad
    /// item fails the clause. Repeats of the clause replace the whole
    /// list (last-one-wins).
    StrList,
    /// `key: <filter>, …;` — like [`ClauseKind::WordList`], but each
    /// entry is a filter atom that may carry `(...)` arguments.
    FilterList,
    /// `key: fn::<seg>[::<seg>…];` — an SDL-lexed name path (rule
    /// 3's `<fn-path>` clause), never a call.
    FnPath,
    /// `key: <expr>, <expr>, …;` — the expression comma list (rule
    /// 3's comma-list clause in its expression form): items split at
    /// depth-0 `,` under the capture scanner's skip set, each
    /// canonicalized separately for its slot (event `then:` is the
    /// first; index `fields:` follows later). Every bad item is
    /// diagnosed; one bad item fails the clause. Repeats of the
    /// clause replace the whole list (last-one-wins).
    ExprList(Slot),
    /// `key;` or `key { clauses };` — the dependent group (spec
    /// README's flag-plus-sub-block pattern): a bare flag selects
    /// the parent's defaults, the same-key block configures the
    /// parameters (`async;` / `async { retry: …; maxdepth: …; };`).
    /// The parameters never appear as standalone clauses outside
    /// the block.
    DependentFlag(&'static [(&'static str, ClauseKind)]),
    /// `key: <ident>;` — a single (possibly quoted) identifier,
    /// SDL-lexed over the capture (event's `on:`; index's `on:`
    /// follows later).
    Ident,
    /// `key: <number>;` — a u16 whole number, SDL-lexed (the `async`
    /// group's `retry:`/`maxdepth:` — upstream's exact type).
    Number,
    /// `key: f'<…>' | silo::<org>::<pkg>::<maj.min.patch>;` — module's
    /// executable (rule 3's keyword-clause kind, SDL-lexed: an
    /// f-string literal or a silo name path with a version triple,
    /// never an expression).
    Executable,
    /// `key: <word>;` — one word from a fixed set (the singleton
    /// sibling of [`ClauseKind::WordList`]: graphql's
    /// `introspection: auto | none;`).
    WordSet(&'static [&'static str]),
    /// `key: <word>;` or `key { <clause>|<clause>; };` — the
    /// keyword/sub-block duality (spec/config.md: `tables: auto;` vs
    /// `tables { include: …; };`). The separator picks the form: `:`
    /// reads one word of the set, `{` opens the sub-block (which must
    /// hold exactly one of its two keys), anything else diagnoses the
    /// two spellings.
    WordOrSubBlock(
        &'static [&'static str],
        &'static [(&'static str, ClauseKind)],
    ),
    /// `key: <ident>, <ident>, …;` — a comma list of identifiers
    /// (graphql `tables { include: …; }`; non-empty like every list).
    IdentList,
    /// `key: fn::<seg>, …;` — a comma list of `<fn-path>`s with the
    /// `fn::` prefix REQUIRED (graphql `functions { … }`; upstream
    /// stores the stripped bare name and re-prefixed print).
    FnPathList,
    /// `key: <number>;` — a u32 whole number (graphql's
    /// `depth:`/`complexity:`, upstream's exact types).
    Number32,
    /// `key: <head>(<args>), …;` — api's hybrid middleware list
    /// (spec/config.md): SDL-lexed name-path heads (bare, `fn::`, or
    /// `mod::`), each with balanced-paren arguments that split at
    /// depth-0 `,` into embedded-expression items. Non-empty
    /// (upstream demands a name right after the keyword).
    MiddlewareList,
    /// `key: (<number>, <number>);` — a parenthesized pair of
    /// decimals (index `bm25:`; rule 3's keyword-clause kind, SDL-
    /// lexed: the parens are mandatory, never an expression capture).
    Bm25,
    /// `key: <float>;` — one decimal scalar (index `lm:`/`alpha:`,
    /// minkowski's order; rule 3's keyword-clause kind, SDL-lexed:
    /// the SurrealQL float token shape minus the `NaN`/`Infinity`
    /// constants — see [`float_arg`]).
    Float,
    /// `key: <distance>[(<order>)];` — one keyword of the distance
    /// set, `minkowski` taking a parenthesized order argument
    /// (index `distance:`; rule 3's keyword-clause kind — SurrealQL
    /// spells the order without parens, SDL's paren form matches
    /// `bm25:` and codegen emits the bare form).
    Distance,
    /// `key: none | full;` or `key { select: …; create, update: …; };`
    /// — the per-operation `permissions` shared by `table` and `field`
    /// (spec/table.md, spec/field.md): the single-value form is ONLY
    /// the shorthand (both parsers accept `NONE | FULL | FOR` — a bare
    /// expression is invalid there, unlike param's), or the
    /// per-operation block whose comma-grouped op lines do not fit the
    /// clause-key model. The flag selects the op domain: `false`
    /// (fields) rejects the `delete` operation outright.
    OpPermissions(bool),
    /// `key: <word> | <expr>;` — one bare word of a fixed set, or an
    /// embedded expression (field reference's `on delete:`: the four
    /// enum actions, anything else SurrealQL runs as
    /// `ON DELETE THEN <expr>` — spec/field.md). The two readings ride
    /// the two clause values the statement's fold already knows.
    WordOrExpr(&'static [&'static str], Slot),
    /// A nested statement head (`index`/`event`/`field` inside a
    /// `table` block, spec/table.md): a non-clause-key head that opens
    /// a full nested definition — README rule 2's structural statement
    /// bodies (api's precedent), riding the clause loop so the
    /// table's did-you-mean covers the nested heads too. The nested
    /// definition omits its standalone `on:` (its host table is the
    /// header name).
    Nested,
}

/// One parsed clause, yielded by [`FileParser::clause_loop`] in
/// source order, with the span of its key (for diagnostics about the
/// clause as a whole — a sub-block missing its required clause, say)
/// and its rule-5 comment anchors: the `//` lines directly above the
/// key (`before`) and the same-line note after its terminator
/// (`after` — filled by the NEXT loop iteration's same-line split).
struct ParsedClause {
    span: Span,
    name: &'static str,
    value: ClauseValue,
    before: Vec<String>,
    after: Vec<String>,
    /// Anchors from a bespoke interior the generic loop cannot see —
    /// the per-operation permissions groups (already path-qualified).
    interior: Vec<ClauseComment>,
}

/// Rule 5's clause-anchor fold: one [`ClauseComment`] per clause
/// carrying before/after notes (in source order), sub-block
/// interiors path-prefixed recursively (`duration.token`), plus the
/// statement's same-line close note under the empty path. Repeated
/// clauses (rule 7's last-one-wins) merge into one entry per path,
/// notes concatenated in source order.
fn collect_clause_comments(
    clauses: &[ParsedClause],
    close_note: Option<String>,
) -> Vec<ClauseComment> {
    let mut out: Vec<ClauseComment> = Vec::new();
    for clause in clauses {
        if !clause.before.is_empty() || !clause.after.is_empty() {
            push_clause_anchor(&mut out, clause.name, &clause.before, &clause.after);
        }
        for entry in &clause.interior {
            push_clause_anchor(&mut out, &entry.path, &entry.before, &entry.after);
        }
        if let ClauseValue::Sub(sub) = &clause.value {
            for entry in collect_clause_comments(sub, None) {
                push_clause_anchor(
                    &mut out,
                    &format!("{}.{}", clause.name, entry.path),
                    &entry.before,
                    &entry.after,
                );
            }
        }
    }
    if let Some(note) = close_note {
        push_clause_anchor(&mut out, "", &[], &[note]);
    }
    out
}

/// Rule 5's nested close-note transfer: the clause loop's same-line
/// split landed a note after a nested statement on that clause's
/// `after`; the nested statement itself carries it as its own
/// empty-path anchor.
fn attach_nested_close_note(stmt: &mut Stmt, after: Vec<String>) {
    if after.is_empty() {
        return;
    }
    stmt.comment_slots().clause.push(ClauseComment {
        path: String::new(),
        before: Vec::new(),
        after,
    });
}

/// [`collect_clause_comments`]'s merge step: same-path entries
/// extend (source order), a new path appends.
fn push_clause_anchor(
    out: &mut Vec<ClauseComment>,
    path: &str,
    before: &[String],
    after: &[String],
) {
    if let Some(entry) = out.iter_mut().find(|entry| entry.path == path) {
        entry.before.extend(before.iter().cloned());
        entry.after.extend(after.iter().cloned());
    } else {
        out.push(ClauseComment {
            path: path.to_string(),
            before: before.to_vec(),
            after: after.to_vec(),
        });
    }
}

enum ClauseValue {
    Expr(Expr),
    Permission(Permission),
    Duration(Duration),
    Flag,
    /// A parsed sub-block: its clauses in source order.
    Sub(Vec<ParsedClause>),
    /// A known clause whose value failed to parse — its own
    /// diagnostic was already pushed at the failure site, and the
    /// marker is what lets required-clause validation distinguish an
    /// absent clause (report it) from an invalid one (already
    /// reported). Folds skip it.
    Failed,
    /// `on: <level>;` — the canonicalized level with the authored
    /// alias spelling carried for the writer.
    Level(Authored<Level>),
    /// The decoded string-literal value.
    Str(String),
    /// The list's words as parsed (each a member of the clause's set).
    WordList(Vec<String>),
    /// The list's decoded string-literal values.
    StrList(Vec<String>),
    FilterList(Vec<Filter>),
    FnPath(FnPath),
    /// The list's expressions, each an embedded canonical print.
    ExprList(Vec<Expr>),
    /// The parsed identifier.
    Ident(Ident),
    /// The parsed whole number.
    Number(u16),
    /// The parsed module executable.
    Executable(Executable),
    /// The one word of a [`ClauseKind::WordSet`] value.
    Word(String),
    /// The list's identifiers as parsed.
    IdentList(Vec<Ident>),
    /// The list's fn paths as parsed.
    FnPathList(Vec<FnPath>),
    /// The parsed u32 whole number.
    Number32(u32),
    /// The parsed middleware entries.
    Middleware(Vec<Middleware>),
    /// The parsed bm25 pair.
    Bm25(Bm25),
    /// The parsed decimal scalar.
    Float(Float),
    /// The parsed distance (minkowski carrying its order).
    Distance(Distance),
    /// The parsed per-operation `permissions` clause (`table`/`field`).
    OpPermissions(OpPermissions),
    /// A nested definition (`index`/`event`) parsed in full. Boxed:
    /// a whole statement dwarfs every other clause value, and
    /// `ParsedClause` rides `clause_loop`'s `Result` (the large-Err
    /// lint's concern).
    Nested(Box<Stmt>),
}

/// The tail a statement parser reads through
/// [`FileParser::parse_stmt_block`]: the bare `;`-or-EOF form, or the
/// clause block with its parsed clauses and the block's `{` span (the
/// anchor some required-clause diagnostics want). The `close_note`
/// is the same-line `//` note captured after the tail's terminator —
/// rule 5's empty-path clause-comment anchor, which the statement
/// fold appends to its `clause_comments`.
enum StmtTail {
    Bare {
        close_note: Option<String>,
    },
    Block {
        clauses: Vec<ParsedClause>,
        brace: Span,
        close_note: Option<String>,
    },
}

/// What follows a completed field-header kind — the terminator byte
/// the kind fold read (`;` bare form, `=` computed short form, `{`
/// the clause block with its consumed brace handed back).
enum FieldHead {
    Kind(Kind),
    KindThenComputed(Kind),
    KindThenBlock(Kind, Span),
}

/// A field's block disposition: no clause block (the bare form or a
/// `= <expr>;` short form — all their terminators consumed), or the
/// block whose `{` is already consumed.
enum FieldBlock {
    None,
    Open(Span),
}

/// The field header facts [`FileParser::finish_field`] folds against:
/// the path plus its span (the anchor for the type-required report),
/// the header type and computed assignment (the block's `computed:`
/// joins them last-one-wins), and the doc comment.
struct FieldHeader {
    name: FieldPath,
    name_span: Span,
    kind: Option<Kind>,
    computed: Option<Expr>,
    comment: Option<DocComment>,
    /// The leading `//` lines taken at the parse entry points, joined
    /// with any captured between the path and the header's tail.
    comments: Vec<String>,
}

/// Which block a hostable statement (field, index, event) is being
/// parsed for — the `on:` machinery's three modes
/// ([`FileParser::fold_on`]): standalone demands the clause, a table
/// block supplies the host and forbids the clause, and a mixin body
/// (spec/mixin.md) has no host at parse time — the explicit clause is
/// forbidden and the stored `on` is the empty placeholder expansion
/// overwrites with the spreading table (the writer's nested form
/// never prints it; codegen runs post-expansion).
#[derive(Clone, Copy)]
enum FieldHost<'a> {
    Standalone,
    Table(&'a Ident),
    Mixin,
}

/// What a hostable statement's clause loop collected for `on:` —
/// [`FileParser::fold_on`]'s input: the explicit spelling's value and
/// span when the loop saw one (whether that spelling is legal is the
/// host mode's call, not the loop's), whether a failed attempt
/// already carried its own diagnostic, and the block anchor for the
/// standalone missing-clause report.
struct OnClause {
    captured: Option<Ident>,
    explicit_span: Option<Span>,
    attempted: bool,
    missing_span: Span,
}

/// `sequence`'s clause table (spec/sequence.md) — also the
/// did-you-mean candidates for its block form.
const SEQUENCE_CLAUSES: &[(&str, ClauseKind)] = &[
    ("batch", ClauseKind::Expr(Slot::SequenceBatch)),
    ("start", ClauseKind::Expr(Slot::SequenceStart)),
    ("timeout", ClauseKind::Expr(Slot::SequenceTimeout)),
];

/// `param`'s clause table (spec/param.md).
const PARAM_CLAUSES: &[(&str, ClauseKind)] = &[
    (
        "permissions",
        ClauseKind::Permissions(Slot::ParamPermissions),
    ),
    ("value", ClauseKind::Expr(Slot::ParamValue)),
];

/// `database`'s clause table (spec/database.md).
const DATABASE_CLAUSES: &[(&str, ClauseKind)] = &[
    ("changefeed", ClauseKind::SubBlock(CHANGEEFEED_CLAUSES)),
    ("strict", ClauseKind::Flag),
];

/// The `changefeed` sub-block's clauses (spec/database.md); `expiry`
/// is required inside the block, enforced by the folding caller.
const CHANGEEFEED_CLAUSES: &[(&str, ClauseKind)] = &[
    ("expiry", ClauseKind::Duration),
    ("include original", ClauseKind::Flag),
];

/// `bucket`'s clause table (spec/bucket.md).
const BUCKET_CLAUSES: &[(&str, ClauseKind)] = &[
    ("backend", ClauseKind::Expr(Slot::BucketBackend)),
    ("readonly", ClauseKind::Flag),
    (
        "permissions",
        ClauseKind::Permissions(Slot::BucketPermissions),
    ),
];

/// `user`'s clause table (spec/user.md); `on` is required (upstream
/// `expected!(ON)`), enforced by the folding caller.
const USER_CLAUSES: &[(&str, ClauseKind)] = &[
    ("on", ClauseKind::Level),
    ("password", ClauseKind::Str),
    ("passhash", ClauseKind::Str),
    ("passscram", ClauseKind::Str),
    ("roles", ClauseKind::WordList(ROLE_WORDS)),
    ("duration", ClauseKind::SubBlock(DURATION_CLAUSES)),
];

/// `user`'s `on:` keyword set (spec/user.md) — canonical variants plus
/// the input-only aliases (rule 9): `kv` ≡ `root`, `ns` ≡ `namespace`,
/// `db` ≡ `database`. Also the did-you-mean candidates for the clause.
const LEVEL_WORDS: &[&str] = &["root", "kv", "namespace", "ns", "database", "db"];

/// The alias members of [`LEVEL_WORDS`]: everything else
/// canonicalizes with no spelling to carry.
const LEVEL_ALIAS_WORDS: &[&str] = &["kv", "ns", "db"];

/// SurrealQL's hardcoded role set (spec/user.md) — the parse-side
/// mirror of upstream's `matches!(role.to_lowercase(), …)` check.
const ROLE_WORDS: &[&str] = &["owner", "editor", "viewer"];

/// The `duration` sub-block's clauses (spec/user.md): both sides are
/// optional ("partial blocks emit only the present sides"); a block
/// with neither side present folds to an absent `duration`.
const DURATION_CLAUSES: &[(&str, ClauseKind)] = &[
    ("token", ClauseKind::Expr(Slot::UserDurationToken)),
    ("session", ClauseKind::Expr(Slot::UserDurationSession)),
];

/// `analyzer`'s clause table (spec/analyzer.md) — every clause
/// optional.
const ANALYZER_CLAUSES: &[(&str, ClauseKind)] = &[
    ("function", ClauseKind::FnPath),
    ("tokenizers", ClauseKind::WordList(TOKENIZER_WORDS)),
    ("filters", ClauseKind::FilterList),
];

/// SurrealQL's tokenizer set (spec/analyzer.md).
const TOKENIZER_WORDS: &[&str] = &["blank", "camel", "class", "punct"];

/// `event`'s clause table (spec/event.md); `on` and `then` are
/// required (upstream rejects an event without `ON` or with an empty
/// `THEN`), enforced by the folding caller.
const EVENT_CLAUSES: &[(&str, ClauseKind)] = &[
    ("on", ClauseKind::Ident),
    ("when", ClauseKind::Expr(Slot::EventWhen)),
    ("then", ClauseKind::ExprList(Slot::EventThen)),
    ("async", ClauseKind::DependentFlag(ASYNC_CLAUSES)),
];

/// The `async` dependent group's parameter block (spec/event.md):
/// both sides optional — the bare `async;` flag is SurrealQL's
/// 1/3 defaults. u16 is upstream's exact type
/// (`catalog::schema::EventKind::Async`), unclamped beyond it (the
/// docs' 0–16 `maxdepth` range is folklore; upstream's own tests
/// parse 64).
const ASYNC_CLAUSES: &[(&str, ClauseKind)] = &[
    ("retry", ClauseKind::Number),
    ("maxdepth", ClauseKind::Number),
];

/// `module`'s clause table (spec/module.md); `from` and `unsigned`
/// are required (SDL's 3.3 grammar), enforced by the folding caller.
/// The permissions slot rides `module`'s own wrapper: since the 3.3.0
/// embed its grammar parses under the experimental capabilities (the
/// 3.2.4 cargo-feature bail arm that forced param's stand-in is gone
/// as of 2026-09-25).
const MODULE_CLAUSES: &[(&str, ClauseKind)] = &[
    ("from", ClauseKind::Executable),
    ("unsigned", ClauseKind::Flag),
    (
        "permissions",
        ClauseKind::Permissions(Slot::ModulePermissions),
    ),
];

/// `function`'s clause table (spec/function.md) — the optional SDL
/// block after the body; every clause optional. The permissions slot
/// rides `function`'s own wrapper (its grammar position parses on the
/// embed).
const FUNCTION_CLAUSES: &[(&str, ClauseKind)] = &[
    ("graphql", ClauseKind::SubBlock(GRAPHQL_CLAUSES)),
    (
        "permissions",
        ClauseKind::Permissions(Slot::FunctionPermissions),
    ),
];

/// The `graphql` sub-block's clauses (spec/function.md): both
/// independently optional strings; a block with neither present folds
/// to an absent `graphql`.
const GRAPHQL_CLAUSES: &[(&str, ClauseKind)] =
    &[("alias", ClauseKind::Str), ("deprecated", ClauseKind::Str)];

/// `config api`'s clause table (spec/config.md). Both clauses
/// optional; the `permissions` slot is the statement's own grammar
/// position (config has no capability gate — the wrapper parses on
/// the embed, unlike `module`'s).
const CONFIG_API_CLAUSES: &[(&str, ClauseKind)] = &[
    ("middleware", ClauseKind::MiddlewareList),
    (
        "permissions",
        ClauseKind::Permissions(Slot::ConfigApiPermissions),
    ),
];

/// The graphql family word set (spec/config.md) — the `:` form of a
/// [`ClauseKind::WordOrSubBlock`] value.
const FAMILY_WORDS: &[&str] = &["auto", "none"];

/// `config graphql`'s clause table (spec/config.md). Clause order is
/// free (rule 7); the bare `auto;`/`none;` flags write BOTH families
/// and fold last-one-wins with `tables:`/`functions:` per family.
const CONFIG_GRAPHQL_CLAUSES: &[(&str, ClauseKind)] = &[
    ("auto", ClauseKind::Flag),
    ("complexity", ClauseKind::Number32),
    ("depth", ClauseKind::Number32),
    (
        "functions",
        ClauseKind::WordOrSubBlock(FAMILY_WORDS, GRAPHQL_FUNCTIONS_CLAUSES),
    ),
    ("introspection", ClauseKind::WordSet(INTROSPECTION_WORDS)),
    ("none", ClauseKind::Flag),
    (
        "tables",
        ClauseKind::WordOrSubBlock(FAMILY_WORDS, GRAPHQL_TABLES_CLAUSES),
    ),
];

/// The `introspection:` word set (spec/config.md).
const INTROSPECTION_WORDS: &[&str] = &["auto", "none"];

/// The `tables` sub-block's clauses (spec/config.md): the block holds
/// exactly ONE of the two — enforced by the folding caller.
const GRAPHQL_TABLES_CLAUSES: &[(&str, ClauseKind)] = &[
    ("exclude", ClauseKind::IdentList),
    ("include", ClauseKind::IdentList),
];

/// The `functions` sub-block's clauses (spec/config.md): same
/// exactly-one rule; entries require the `fn::` prefix.
const GRAPHQL_FUNCTIONS_CLAUSES: &[(&str, ClauseKind)] = &[
    ("exclude", ClauseKind::FnPathList),
    ("include", ClauseKind::FnPathList),
];

/// `config default`'s clause table (spec/config.md): at least one of
/// the two sides is required (upstream rejects a bare `DEFAULT`),
/// enforced by the folding caller.
const CONFIG_DEFAULT_CLAUSES: &[(&str, ClauseKind)] = &[
    ("database", ClauseKind::Expr(Slot::ConfigDefaultDatabase)),
    ("namespace", ClauseKind::Expr(Slot::ConfigDefaultNamespace)),
];

/// `config`'s header variants (spec/config.md) — the did-you-mean
/// candidates for an unknown variant word.
const CONFIG_VARIANTS: &[&str] = &["api", "graphql", "default"];

/// An api sub-block's clause table (spec/api.md) — the shared shape,
/// the two block kinds differing only in the `then:` slot's wrapper
/// (the group action prints unwrapped, the fallback CoverStmts-wrapped
/// — separate `expr.rs` slots). Clause order inside a block is free
/// (SurrealQL parity); a repeat folds last-one-wins (rule 7).
const API_GROUP_CLAUSES: &[(&str, ClauseKind)] = &[
    ("middleware", ClauseKind::MiddlewareList),
    (
        "permissions",
        ClauseKind::Permissions(Slot::ConfigApiPermissions),
    ),
    ("then", ClauseKind::Expr(Slot::ApiAction)),
];

const API_ANY_CLAUSES: &[(&str, ClauseKind)] = &[
    ("middleware", ClauseKind::MiddlewareList),
    (
        "permissions",
        ClauseKind::Permissions(Slot::ConfigApiPermissions),
    ),
    ("then", ClauseKind::Expr(Slot::ApiFallback)),
];

/// The six HTTP method words (spec/api.md) — the members a head comma
/// list may hold.
const API_METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "trace"];

/// The api body's head words — the methods plus the one `any` block.
/// The did-you-mean candidates for an unknown head.
const API_HEADS: &[&str] = &["get", "post", "put", "patch", "delete", "trace", "any"];

/// `index`'s header variant words (spec/index.md) — the closed set
/// the optional variant slot takes; a bare word outside it opens the
/// name position of a plain index (rule 6: the default kind is
/// expressed by omitting the slot).
const INDEX_VARIANTS: &[&str] = &["unique", "count", "fulltext", "hnsw", "diskann"];

/// The clause vocabulary of the plain and `unique` index kinds
/// (spec/index.md): `fields` required (≥ 1, enforced by the folding
/// caller), everything else optional. `count` omits both field
/// spellings from its table — the clause simply does not exist for
/// that kind (rule 10), so `fields:` there diagnoses as unknown.
const INDEX_PLAIN_CLAUSES: &[(&str, ClauseKind)] = &[
    ("on", ClauseKind::Ident),
    ("columns", ClauseKind::ExprList(Slot::IndexFields)),
    ("fields", ClauseKind::ExprList(Slot::IndexFields)),
    ("concurrently", ClauseKind::Flag),
];

/// `index count`'s clause table (spec/index.md): `where` optional,
/// no field clauses of either spelling.
const INDEX_COUNT_CLAUSES: &[(&str, ClauseKind)] = &[
    ("on", ClauseKind::Ident),
    ("where", ClauseKind::Expr(Slot::IndexWhere)),
    ("concurrently", ClauseKind::Flag),
];

/// `index fulltext`'s clause table (spec/index.md): exactly one field
/// required; the tuning clauses optional (defaults materialize
/// server-side and elide at emission, rule 9).
const INDEX_FULLTEXT_CLAUSES: &[(&str, ClauseKind)] = &[
    ("on", ClauseKind::Ident),
    ("columns", ClauseKind::ExprList(Slot::IndexFields)),
    ("fields", ClauseKind::ExprList(Slot::IndexFields)),
    ("analyzer", ClauseKind::Ident),
    ("bm25", ClauseKind::Bm25),
    ("highlights", ClauseKind::Flag),
    ("concurrently", ClauseKind::Flag),
];

/// `index hnsw`'s clause table (spec/index.md): `dimension` required
/// and exactly one field; the range/domain checks (`m` ≤ 127, `m0`
/// within u8) are the folding caller's (upstream's own post-parse
/// checks). `type` is the plain vector-type word set — DISKANN's
/// restriction is its own kind's business.
const INDEX_HNSW_CLAUSES: &[(&str, ClauseKind)] = &[
    ("on", ClauseKind::Ident),
    ("columns", ClauseKind::ExprList(Slot::IndexFields)),
    ("fields", ClauseKind::ExprList(Slot::IndexFields)),
    ("dimension", ClauseKind::Number),
    ("distance", ClauseKind::Distance),
    ("type", ClauseKind::WordSet(VECTOR_TYPE_WORDS)),
    ("m", ClauseKind::Number),
    ("m0", ClauseKind::Number),
    ("lm", ClauseKind::Float),
    ("efc", ClauseKind::Number),
    ("extend_candidates", ClauseKind::Flag),
    ("keep_pruned_connections", ClauseKind::Flag),
    ("hashed_vector", ClauseKind::Flag),
    ("concurrently", ClauseKind::Flag),
];

/// `index diskann`'s clause table (spec/index.md): HNSW's vector
/// parameters with DISKANN's own — the restricted distance/type
/// domains and `degree`/`l_build` > 0 are the folding caller's
/// (upstream rejects post-parse).
const INDEX_DISKANN_CLAUSES: &[(&str, ClauseKind)] = &[
    ("on", ClauseKind::Ident),
    ("columns", ClauseKind::ExprList(Slot::IndexFields)),
    ("fields", ClauseKind::ExprList(Slot::IndexFields)),
    ("dimension", ClauseKind::Number),
    ("distance", ClauseKind::Distance),
    ("type", ClauseKind::WordSet(VECTOR_TYPE_WORDS)),
    ("degree", ClauseKind::Number),
    ("l_build", ClauseKind::Number),
    ("alpha", ClauseKind::Float),
    ("hashed_vector", ClauseKind::Flag),
    ("concurrently", ClauseKind::Flag),
];

/// SurrealQL's vector-type set (spec/index.md) — the `type:` word
/// set, also the did-you-mean candidates.
const VECTOR_TYPE_WORDS: &[&str] = &["f64", "f32", "f16", "i64", "i32", "i16", "i8", "u8"];

/// SurrealQL's distance set (spec/index.md) — the `distance:`
/// keywords (minkowski additionally takes its order argument); also
/// the did-you-mean candidates.
const DISTANCE_WORDS: &[&str] = &[
    "euclidean",
    "cosine",
    "cosine_normalized",
    "chebyshev",
    "hamming",
    "jaccard",
    "pearson",
    "inner_product",
    "manhattan",
    "minkowski",
];

/// The whole-number ranges the two number clauses carry — upstream's
/// exact types: u16 for `async`'s parameters, u32 for graphql's
/// `depth`/`complexity` limits. They are the diagnostic's range
/// spelling, nothing more.
const U16_RANGE: &str = "0–65535";
const U32_RANGE: &str = "0–4294967295";

/// `access`'s clause table (spec/access.md) — the UNION vocabulary of
/// all three variants: the `type:` discriminant lives inside the block
/// (README rule 10's ACCESS enum), so no per-variant table exists to
/// omit forbidden keys from (index's mechanism). The folding caller
/// owns variant membership instead: every clause carries the variants
/// that define it, and a clause outside the selected variant's set
/// diagnoses there. `on` and `type` are required (upstream `expected
/// ON`; the discriminant is implicitly required — README grammar
/// notation).
const ACCESS_CLAUSES: &[(&str, ClauseKind)] = &[
    ("on", ClauseKind::Level),
    ("type", ClauseKind::WordSet(ACCESS_TYPE_WORDS)),
    ("url", ClauseKind::Expr(Slot::AccessUrl)),
    ("algorithm", ClauseKind::WordSet(ALGORITHM_WORDS)),
    ("key", ClauseKind::Expr(Slot::AccessKey)),
    ("issuer", ClauseKind::SubBlock(ACCESS_ISSUER_CLAUSES)),
    ("signup", ClauseKind::Expr(Slot::AccessSignup)),
    ("signin", ClauseKind::Expr(Slot::AccessSignin)),
    ("jwt", ClauseKind::SubBlock(ACCESS_JWT_CLAUSES)),
    ("refresh", ClauseKind::Flag),
    ("for", ClauseKind::WordSet(ACCESS_SUBJECT_WORDS)),
    ("duration", ClauseKind::SubBlock(ACCESS_DURATION_CLAUSES)),
    ("authenticate", ClauseKind::Expr(Slot::AccessAuthenticate)),
    ("context", ClauseKind::Expr(Slot::AccessContext)),
    ("audience", ClauseKind::StrList),
];

/// The `type:` discriminant set (spec/access.md).
const ACCESS_TYPE_WORDS: &[&str] = &["jwt", "record", "bearer"];

/// The `for:` subject set (spec/access.md) — the bearer variant's
/// required clause.
const ACCESS_SUBJECT_WORDS: &[&str] = &["user", "record"];

/// The one spelling secret-bearing access key slots accept
/// (spec/access.md's shape-only rule, SDL-E1056): the single-quoted
/// canon the embedded printer gives the `'[REDACTED]'` literal — the
/// value the engine itself redacts to in INFO, so the sentinel
/// round-trips by construction. `pub(crate)`: the DDL readers
/// ([`crate::info::read`]) detect the sentinel with the same canon.
pub(crate) const SECRET_SENTINEL_CANON: &str = "'[REDACTED]'";

/// The sentinel's DECODED value — what a string-literal clause stores
/// (`ClauseKind::Str` decodes the literal; `Pass::Hash` likewise).
/// The lint's sentinel checks compare the same decoded spelling.
pub(crate) const SECRET_SENTINEL_CANON_DECODED: &str = "[REDACTED]";

/// The JWT algorithm set (v3.2.4 `Algorithm`'s 13) — `algorithm:`'s
/// word set, also the did-you-mean candidates.
const ALGORITHM_WORDS: &[&str] = &[
    "eddsa", "es256", "es384", "es512", "hs256", "hs384", "hs512", "ps256", "ps384", "ps512",
    "rs256", "rs384", "rs512",
];

/// The `jwt { … }` sub-block's clauses (spec/access.md) — the same
/// source vocabulary the jwt variant spells directly: one verification
/// source (`url:` XOR `algorithm:`+`key:`) plus the optional
/// `issuer { … }`.
const ACCESS_JWT_CLAUSES: &[(&str, ClauseKind)] = &[
    ("algorithm", ClauseKind::WordSet(ALGORITHM_WORDS)),
    ("audience", ClauseKind::StrList),
    ("issuer", ClauseKind::SubBlock(ACCESS_ISSUER_CLAUSES)),
    ("key", ClauseKind::Expr(Slot::AccessKey)),
    ("url", ClauseKind::Expr(Slot::AccessUrl)),
];

/// The `issuer { … }` sub-block's clauses (spec/access.md): both
/// optional at parse; the folding caller enforces `key`'s
/// required-unless-mirrored rule and the algorithm compatibility rule.
const ACCESS_ISSUER_CLAUSES: &[(&str, ClauseKind)] = &[
    ("algorithm", ClauseKind::WordSet(ALGORITHM_WORDS)),
    ("key", ClauseKind::Expr(Slot::AccessIssuerKey)),
];

/// The `duration` sub-block's clauses (spec/access.md): three
/// expression sides, each independently optional — a block with none
/// present folds to an absent `duration` (rule 9, like `user`'s).
const ACCESS_DURATION_CLAUSES: &[(&str, ClauseKind)] = &[
    ("grant", ClauseKind::Expr(Slot::AccessDurationGrant)),
    ("token", ClauseKind::Expr(Slot::AccessDurationToken)),
    ("session", ClauseKind::Expr(Slot::AccessDurationSession)),
];

/// `table`'s clause table (spec/table.md) — the UNION vocabulary of
/// the non-view kinds: bare/normal, `any`, and the two relation
/// spellings share it, with `enforced` relation-only and membership
/// validated at the fold (access's precedent — the kind rides the
/// header, but one table serves every kind since the forbidden-key
/// mechanism (index's per-variant tables) would triple the
/// vocabulary for one shared flag). The nested `index`/`event` heads
/// ride the same table, so an unknown-key did-you-mean covers them
/// (`indx` → `index`); view takes its own two-clause table.
/// `schemafull`/`schemaless` mutual exclusion is the fold's, too.
/// The INLINE cache caps are kind-independent (the engine accepts
/// them on every table kind, probe-pinned) — only the ≤256 bound is
/// validated, at the fold ([`Diag::InvalidTableParam`]); a view does
/// not author them (SDL's view grammar stays the two-clause table,
/// and the reader routes a cap-bearing view to `raw` as deviant).
const TABLE_CLAUSES: &[(&str, ClauseKind)] = &[
    ("changefeed", ClauseKind::SubBlock(CHANGEEFEED_CLAUSES)),
    ("drop", ClauseKind::Flag),
    ("enforced", ClauseKind::Flag),
    ("event", ClauseKind::Nested),
    ("field", ClauseKind::Nested),
    ("graphql", ClauseKind::SubBlock(GRAPHQL_CLAUSES)),
    ("index", ClauseKind::Nested),
    ("inline edges", ClauseKind::Number),
    ("inline references", ClauseKind::Number),
    ("permissions", ClauseKind::OpPermissions(true)),
    ("schemaless", ClauseKind::Flag),
    ("schemafull", ClauseKind::Flag),
];

/// `table view`'s clause table (spec/table.md): the only clauses a
/// view takes — `changefeed;`/`drop;`/the schema flags/`enforced;`/
/// nested heads are meaningless on a view and diagnose here as the
/// unknown clauses they are for this kind (rule 10).
const TABLE_VIEW_CLAUSES: &[(&str, ClauseKind)] = &[
    ("graphql", ClauseKind::SubBlock(GRAPHQL_CLAUSES)),
    ("permissions", ClauseKind::OpPermissions(true)),
];

/// `mixin`'s clause table (spec/mixin.md): the body is a table
/// block's body minus `on:` clauses — fields (the `field`-prefixed
/// head; a bare field head `name: type …;` lands through the clause
/// loop's field-head detection, gated to this statement too),
/// indexes, and events. A bare field head named `index`/`event`
/// needs the `field` prefix or quoting, exactly as in a table block
/// (the keys are active here); `...spread;` and every other word
/// diagnoses as the unknown clause it is for this body.
const MIXIN_CLAUSES: &[(&str, ClauseKind)] = &[
    ("event", ClauseKind::Nested),
    ("field", ClauseKind::Nested),
    ("index", ClauseKind::Nested),
];

/// `field`'s clause table (spec/field.md) — SurrealQL's own clause
/// vocabulary, free-ordered (rule 7), folded last-one-wins by
/// [`FileParser::finish_field`]. The header carries the type and the
/// optional computed assignment; `default`/`default always` share one
/// slot through their two keys. `reference` is the dependent
/// flag-plus-block (the bare flag selects the engine's default
/// action); its block's `on delete:` takes an enum word or an
/// expression. `permissions` is the per-operation clause in its
/// three-op domain (`delete` rejected).
const FIELD_CLAUSES: &[(&str, ClauseKind)] = &[
    ("assert", ClauseKind::Expr(Slot::FieldAssert)),
    ("computed", ClauseKind::Expr(Slot::FieldComputed)),
    ("default", ClauseKind::Expr(Slot::FieldDefault)),
    ("default always", ClauseKind::Expr(Slot::FieldDefaultAlways)),
    ("flexible", ClauseKind::Flag),
    ("graphql", ClauseKind::SubBlock(GRAPHQL_CLAUSES)),
    ("inline", ClauseKind::Flag),
    ("on", ClauseKind::Ident),
    ("permissions", ClauseKind::OpPermissions(false)),
    ("readonly", ClauseKind::Flag),
    (
        "reference",
        ClauseKind::DependentFlag(FIELD_REFERENCE_CLAUSES),
    ),
    ("value", ClauseKind::Expr(Slot::FieldValue)),
];

/// The `reference { … }` sub-block's clauses (spec/field.md): the one
/// referential action. A block with no `on delete:` folds to the
/// engine default (`IGNORE` — the bare flag's meaning).
const FIELD_REFERENCE_CLAUSES: &[(&str, ClauseKind)] = &[(
    "on delete",
    ClauseKind::WordOrExpr(ON_DELETE_WORDS, Slot::FieldOnDelete),
)];

/// The `on delete:` action words (spec/field.md) — the enum set the
/// engine itself defines; any other value runs as `ON DELETE THEN`.
const ON_DELETE_WORDS: &[&str] = &["reject", "cascade", "ignore", "unset"];

/// `table`'s header variant words (spec/table.md): variant words ONLY
/// when followed by a name — `table any { … };` is a bare table named
/// `any`, and `table any -> wrote ->;` reads `any` as a from-list
/// identifier.
const TABLE_VARIANT_WORDS: &[&str] = &["any", "relation", "view"];

/// The INLINE cache caps' inclusive upper bound (3.3) — the engine's
/// own `MAX_INLINE_CACHE_CAP` (`surrealdb-idx-3.3.0/src/inline_cache/
/// mod.rs`), enforced at define/alter, not parse; SDL mirrors it at
/// the fold so a cap the engine would reject never emits.
const MAX_INLINE_CACHE_CAP: u16 = 256;

/// SurrealQL's reserved words (v3.2.4 `keywords::RESERVED_KEYWORD`):
/// a bare word from this set cannot serve as an identifier, in SDL
/// exactly as in SurrealQL (rule 9). Quoted forms are always legal.
/// Sorted; match ASCII-case-insensitively — keyword-ness follows the
/// SurrealQL lexer, not SDL's lowercase-only keyword rule.
const RESERVED_WORDS: &[&str] = &[
    "after", "all", "alter", "before", "begin", "break", "by", "cancel", "commit", "continue",
    "create", "define", "delete", "diff", "false", "for", "function", "if", "info", "insert",
    "kill", "let", "live", "none", "null", "option", "rand", "rebuild", "relate", "remove",
    "return", "select", "sequence", "show", "sleep", "table", "tb", "throw", "true", "update",
    "upsert", "use", "value", "where",
];

/// Parses a whole SDL file: the statements that parsed, plus every
/// diagnostic — one statement's failure never hides its siblings.
///
/// The strict entry (spec/include.md): `include` statements are
/// rejected (SDL-E1068) and dropped, so a context without a file
/// system never compiles a composition it cannot resolve. The
/// loader and the formatter parse through
/// [`parse_file_composable`].
pub fn parse_file(src: &str) -> (SdlFile, Vec<Diag>) {
    parse_with_mode(src, false, false)
}

/// The composable entry (spec/include.md): `include` statements are
/// kept, for the loader to splice and the canonical writer to
/// round-trip. Everything else parses identically to
/// [`parse_file`].
pub fn parse_file_composable(src: &str) -> (SdlFile, Vec<Diag>) {
    parse_with_mode(src, true, false)
}

/// The loader's entry for an INCLUDED file (spec/include.md): the
/// composable grammar plus the composition rule the root never
/// sees — `namespace`/`database` are banned outright (SDL-E1070):
/// diagnosed at dispatch, against this file's own text, and the
/// statement still parses so its siblings keep reporting. Spreads
/// parse as opaque names in every entry; the composition's global
/// resolution pass ([`resolve_spreads_homed`]) checks them against
/// the whole composed document after the splice.
pub(crate) fn parse_file_included(src: &str) -> (SdlFile, Vec<Diag>) {
    parse_with_mode(src, true, true)
}

fn parse_with_mode(src: &str, composable: bool, included: bool) -> (SdlFile, Vec<Diag>) {
    let mut parser = FileParser {
        src,
        scan: Scanner::new(src),
        diags: Vec::new(),
        table_host: None,
        mixin_names: Vec::new(),
        alias_names: Vec::new(),
        comments: Vec::new(),
        field_close_note: None,
        op_permissions_interior: Vec::new(),
        composable,
        included,
        saw_include: false,
    };
    let file = parser.file();
    (file, parser.diags)
}

/// The global spread-resolution pass (the declarative namespace —
/// spec/mixin.md, spec/include.md): every `...<name>;` entry resolves
/// against the WHOLE document's mixin set, defined anywhere, any
/// position. The parse carries the entries as opaque names; this pass
/// is where a name resolving to nothing is SDL-E1061 (the message
/// names the document, never "above it"), and where the two
/// landed-member validations re-run — `inline;`'s host-relation rule
/// (its message names the mixin) and no-subfield-under-a-computed-
/// parent (its message names the root and path) — the legs the old
/// positional fold owned. Both anchor at the spread entry (member
/// statements carry no spans of their own), and both skip a LOSER —
/// an explicitly overshadowed or later-spread-beaten member never
/// lands ([`crate::expand::winning_members`] is the one resolution
/// home).
///
/// The consumer entry is [`crate::order::check_document`] — the whole
/// check-class contract in one call (this half plus the duplicates and
/// the cycle checks) — a typo'd spread must be an error, never a
/// silent empty emission. fmt's print path never runs it (that is the
/// standalone-leaf win); the committal `fmt --in-place` mode is
/// load-gated, and the load runs the contract.
pub fn resolve_spreads(file: &SdlFile) -> Vec<Diag> {
    resolve_spreads_homed(&file.items, &[])
        .into_iter()
        .map(|(diag, _)| diag)
        .collect()
}

/// The loader's shape of the same pass: `item_homes` runs parallel to
/// `items`, so a composed document's diagnostics tag with the home
/// whose text their span renders against.
pub(crate) fn resolve_spreads_homed(items: &[Stmt], item_homes: &[usize]) -> Vec<(Diag, usize)> {
    let home_of = |at: usize| item_homes.get(at).copied().unwrap_or(0);
    let mut out: Vec<(Diag, usize)> = Vec::new();
    // The document's mixin set, in first-definition order (the
    // did-you-mean candidates). Duplicate names are the order pass's
    // SDL-E1062 — this pass only resolves references.
    let mut mixins: Vec<(&str, &Mixin)> = Vec::new();
    for stmt in items {
        if let Stmt::Mixin(mixin) = stmt
            && !mixins
                .iter()
                .any(|(name, _)| *name == mixin.name.raw.as_str())
        {
            mixins.push((mixin.name.raw.as_str(), mixin));
        }
    }
    let candidates: Vec<&str> = mixins.iter().map(|(name, _)| *name).collect();
    for (at, stmt) in items.iter().enumerate() {
        let Stmt::Table(table) = stmt else {
            continue;
        };
        let home = home_of(at);
        // The table's spread entries in body order, each resolving
        // against the document set. An unresolved entry lands nothing
        // (its E1061 is already reported).
        let mut resolved: Vec<(&Mixin, Span)> = Vec::new();
        for entry in &table.nested {
            if let Stmt::Spread(spread) = entry {
                match mixins
                    .iter()
                    .find(|(name, _)| *name == spread.name.raw.as_str())
                {
                    Some((_, mixin)) => resolved.push((mixin, spread.span)),
                    None => out.push((
                        Diag::UnknownMixin {
                            span: spread.span,
                            name: spread.name.raw.clone(),
                            suggestion: did_you_mean(&spread.name.raw, &candidates),
                        },
                        home,
                    )),
                }
            }
        }
        if resolved.is_empty() {
            continue;
        }
        // What lands (spec/mixin.md's resolution): an explicit member
        // overshadows its same-keyed member position-independently;
        // among spreads the later occurrence wholly wins.
        let explicit: Vec<MemberKey> = table
            .nested
            .iter()
            .filter_map(|stmt| stmt.member_key())
            .collect();
        let member_keys: Vec<Vec<Option<MemberKey>>> = resolved
            .iter()
            .map(|(mixin, _)| mixin.members.iter().map(|stmt| stmt.member_key()).collect())
            .collect();
        let lands = crate::expand::winning_members(&member_keys, &explicit);
        let relation = matches!(table.kind, TableKind::Relation { .. });
        // The two landed-member validations — `inline;`'s host rule
        // and no-subfield-under-a-computed-parent — anchored at the
        // spread entry. The computed pool holds the explicit fields'
        // roots with every landed spread member's (one body, either
        // source order); a loser never joins it (the filter). The
        // SPREAD roots are kept apart: the parse checked every
        // explicit field against the explicit roots only (its
        // block-local pool), so this pass owes each explicit field
        // the spread-contributed roots — the pair neither pass may
        // miss.
        let explicit_roots: Vec<String> = table
            .nested
            .iter()
            .filter_map(|stmt| match stmt {
                Stmt::Field(field) if field.computed.is_some() => {
                    Some(field_path_text(&field.name))
                }
                _ => None,
            })
            .collect();
        let mut spread_roots: Vec<(String, Span)> = Vec::new();
        for (land, (mixin, span)) in lands.iter().zip(&resolved) {
            for (pos, stmt) in mixin.members.iter().enumerate() {
                if land[pos]
                    && let Stmt::Field(field) = stmt
                    && field.computed.is_some()
                {
                    spread_roots.push((field_path_text(&field.name), *span));
                }
            }
        }
        let mut computed_roots = explicit_roots.clone();
        computed_roots.extend(spread_roots.iter().map(|(root, _)| root.clone()));
        // The explicit fields against the spread-contributed roots
        // (the parse's pool was explicit-only). The diagnostic
        // anchors at the spread that computed the parent — the field
        // statements carry no spans of their own — with the rule's
        // own message.
        for stmt in &table.nested {
            let Stmt::Field(field) = stmt else {
                continue;
            };
            if field.computed.is_some() {
                continue;
            }
            let path = field_path_text(&field.name);
            let Some(dot) = path.find('.') else {
                continue;
            };
            let root = &path[..dot];
            if let Some((_, spread_span)) =
                spread_roots.iter().find(|(candidate, _)| candidate == root)
            {
                out.push((
                    Diag::InvalidFieldComputed {
                        span: *spread_span,
                        message: format!(
                            "no subfield may nest under a computed parent — `{root}` is \
                             computed, so `{path}` cannot be defined"
                        ),
                    },
                    home,
                ));
            }
        }
        for (land, (mixin, span)) in lands.iter().zip(&resolved) {
            for (pos, stmt) in mixin.members.iter().enumerate() {
                if !land[pos] {
                    continue;
                }
                let Stmt::Field(field) = stmt else {
                    continue;
                };
                if !relation && field.inline {
                    out.push((
                        Diag::InvalidTableClause {
                            span: *span,
                            message: format!(
                                "an INLINE field requires a TYPE RELATION table — mixin \
                                 `{}`'s member `{}` is INLINE and `{}` is not one",
                                mixin.name.raw,
                                field_path_text(&field.name),
                                kind_word(&table.kind)
                            ),
                        },
                        home,
                    ));
                }
                let mut local: Vec<Diag> = Vec::new();
                check_subfield_under_computed(&mut local, *span, field, &computed_roots);
                out.extend(local.into_iter().map(|diag| (diag, home)));
            }
        }
    }
    out
}

/// A doc-comment block plus the span of its `///` opener (for
/// diagnostics about the block as a whole).
struct DocBlock {
    comment: DocComment,
    span: Span,
}

/// The clause state every `index` kind shares (spec/index.md's
/// "shared" block): the required `on`, the field list under either
/// spelling, and the `concurrently` flag. Folded by
/// [`fold_index_shared`], validated by
/// [`FileParser::finish_index_shared`] — one home for the rules all
/// five kinds repeat.
struct IndexShared {
    on: Option<Ident>,
    /// The `on:` clause key's span, for the nested form's forbidden-
    /// clause report.
    on_span: Option<Span>,
    on_attempted: bool,
    fields: Option<(Span, Vec<Expr>)>,
    /// The clause key as authored: `columns` when the file spelled the
    /// alias, `None` for `fields` (the writer keeps it).
    fields_key: Carried,
    fields_attempted: bool,
    concurrently: bool,
    /// Rule 5's clause anchors, collected by whichever variant fold
    /// ran (all five share this home so the Index constructor reads
    /// one field).
    clause_comments: Vec<ClauseComment>,
}

impl IndexShared {
    fn new() -> Self {
        IndexShared {
            on: None,
            on_span: None,
            on_attempted: false,
            fields: None,
            fields_key: Carried::default(),
            fields_attempted: false,
            concurrently: false,
            clause_comments: Vec::new(),
        }
    }
}

struct FileParser<'src> {
    src: &'src str,
    scan: Scanner<'src>,
    diags: Vec<Diag>,
    /// The `table` block currently being parsed (spec/table.md's
    /// nesting): [`ClauseKind::Nested`] heads read it to inject the
    /// host table in place of their `on:` clause. Set only while a
    /// table body's clause loop runs and cleared on exit — a nested
    /// head is never itself a table, so the field never nests.
    table_host: Option<Ident>,
    /// Plain `//` comments seen since the last anchor took them (rule
    /// 5's layout preservation), each carrying its source line — the
    /// line is what the same-line split compares against the previous
    /// anchor's terminator line (a comment ON that terminator's line
    /// is its after-note, never the next anchor's before-note). One
    /// shared buffer: every trivia stop collects into it
    /// ([`FileParser::take_comments`]), a statement entry takes
    /// everything as its leading comments, a clause read takes what
    /// precedes its key as the clause's before-notes, and a block
    /// close takes whatever remains as the statement's trailing
    /// comments — so the buffer always empties at the same boundaries
    /// the parser crosses, and a comment can never skip a region.
    comments: Vec<PendingComment>,
    /// The field block's same-line close note, set by
    /// [`FileParser::run_field_block`] and drained by the field fold
    /// (the field's bespoke block path cannot ride [`StmtTail`]).
    field_close_note: Option<String>,
    /// The per-group permissions anchors, set by the op-permissions
    /// clause arm and drained into the pushed [`ParsedClause`]'s
    /// `interior` — set immediately before the arm returns and
    /// drained immediately after the match, with no recursion between
    /// (the arm's parsers never enter a clause loop), so the handoff
    /// never crosses a nested loop's own push.
    op_permissions_interior: Vec<ClauseComment>,
    /// The mixin names parsed so far (spec/mixin.md): a duplicate
    /// name in one file is SDL-E1062 at parse — no last-one-wins
    /// fold, the reference would be ambiguous in intent. Spread
    /// resolution is NOT the parser's: entries parse as opaque names
    /// and [`resolve_spreads_homed`] checks them against the whole
    /// document's mixin set after the parse.
    mixin_names: Vec<String>,
    /// The type-alias names parsed so far (spec/type-alias.md): a
    /// duplicate name in one file is SDL-E1062 at parse — no
    /// last-one-wins fold, the reference would be ambiguous in
    /// intent. Use-site resolution is NOT the parser's: references
    /// parse as opaque [`KindAst::Alias`] nodes and the order pass
    /// checks them against the whole document's alias set after the
    /// parse.
    alias_names: Vec<String>,
    /// Whether `include` statements are kept (spec/include.md): the
    /// composable entry (loader, formatter) versus the strict entry,
    /// which rejects and drops them.
    composable: bool,
    /// Whether this file is an INCLUDED file (spec/include.md's
    /// position rule 3, via [`parse_file_included`]): `namespace`/
    /// `database` are banned outright (SDL-E1070) — they would
    /// re-scope the importing file below the splice. Implies
    /// `composable`.
    included: bool,
    /// The composition-preamble gate (spec/include.md): an include
    /// has parsed above, forbidding any later `namespace`/`database`
    /// (the scope law — E1069's remaining half; includes themselves
    /// are legal anywhere in the file, splicing in place).
    /// Dispatch-time flag — a statement that fails to parse still
    /// ordered the stream.
    saw_include: bool,
}

/// One registered mixin (see [`FileParser::mixins`]): the name and
/// the member statements with their head spans (the fold
/// diagnostics' anchors — the AST carries no per-member span).
/// One collected `//` comment: its text (after `//`, trailing
/// whitespace already trimmed) and its 1-based source line — rule
/// 5's anchor-classification input.
struct PendingComment {
    line: usize,
    text: String,
}

impl<'src> FileParser<'src> {
    fn file(&mut self) -> SdlFile {
        let mut items = Vec::new();
        let mut pending_doc: Option<DocBlock> = None;
        loop {
            self.take_comments();
            if self.scan.at_doc_comment() {
                // Rule 5: a contiguous block attaches to the definition
                // below it. A second block (necessarily separated by a
                // blank line, which ends the first) simply replaces the
                // pending one; only a block with nothing below it at
                // all is diagnosed.
                pending_doc = Some(self.take_doc_block());
                continue;
            }
            if self.scan.is_at_end() {
                if let Some(doc) = pending_doc.take() {
                    self.diags.push(Diag::DanglingDocComment { span: doc.span });
                }
                break;
            }
            if self.scan.eat_byte(b';') {
                // An empty statement between definitions is noise —
                // but it orphans a pending doc comment: rule 5
                // attaches a block only to the definition directly
                // below it, and the `;` is in between now.
                if let Some(doc) = pending_doc.take() {
                    self.diags.push(Diag::DanglingDocComment { span: doc.span });
                }
                continue;
            }
            let word_start = self.scan.offset();
            match self.scan.read_word() {
                Some(word_span) => {
                    let word = word_span.slice(self.src);
                    // The composition-preamble gate (spec/include.md),
                    // driven at dispatch — a statement that fails to parse
                    // still ordered the stream. Scope statements (with
                    // their aliases) may only precede the file's first
                    // include; every other definition kind forbids any
                    // later include. Composable mode only: the strict
                    // entry drops the include, so ordering relative to a
                    // statement that does not exist is diagnostic noise on
                    // an already-rejected file.
                    let scope = matches!(word, "namespace" | "ns" | "database" | "db");
                    if scope {
                        // Position rule 3 first (spec/include.md): an
                        // INCLUDED file carries no scopes at all —
                        // they would re-scope the importing file
                        // below the splice. The root's own rule (no
                        // scope after an include) rides only when
                        // this file is the root.
                        let statement = if word == "database" || word == "db" {
                            "database"
                        } else {
                            "namespace"
                        };
                        if self.included {
                            self.diags.push(Diag::IncludeScopeBanned {
                                span: word_span,
                                statement,
                            });
                        } else if self.composable && self.saw_include {
                            self.diags.push(Diag::IncludeMisplaced {
                                span: word_span,
                                statement,
                            });
                        }
                    }
                    match word {
                        "namespace" | "ns" => {
                            if let Some(stmt) =
                                self.parse_namespace(pending_doc.take(), carried_keyword(word))
                            {
                                items.push(stmt);
                            }
                        }
                        "sequence" => {
                            if let Some(stmt) = self.parse_sequence(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "param" => {
                            if let Some(stmt) = self.parse_param(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "database" | "db" => {
                            if let Some(stmt) =
                                self.parse_database(pending_doc.take(), carried_keyword(word))
                            {
                                items.push(stmt);
                            }
                        }
                        "bucket" => {
                            if let Some(stmt) = self.parse_bucket(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "user" => {
                            if let Some(stmt) = self.parse_user(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "analyzer" => {
                            if let Some(stmt) = self.parse_analyzer(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "event" => {
                            if let Some(stmt) = self.parse_event(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "module" => {
                            if let Some(stmt) = self.parse_module(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "fn" => {
                            if let Some(stmt) = self.parse_function(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "config" => {
                            if let Some(stmt) = self.parse_config(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "index" => {
                            if let Some(stmt) = self.parse_index(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "api" => {
                            if let Some(stmt) = self.parse_api(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "access" => {
                            if let Some(stmt) = self.parse_access(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "table" => {
                            if let Some(stmt) = self.parse_table(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "field" => {
                            if let Some(stmt) = self.parse_field(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "mixin" => {
                            if let Some(stmt) = self.parse_mixin(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "type" => {
                            if let Some(stmt) = self.parse_type_alias(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "raw" => {
                            if let Some(stmt) = self.parse_raw(pending_doc.take()) {
                                items.push(stmt);
                            }
                        }
                        "include" => {
                            if let Some(stmt) = self.parse_include(word_span, pending_doc.take()) {
                                // The strict entry rejects and drops the
                                // statement (SDL-E1068): emit never sees
                                // an include without a loader behind it.
                                if self.composable {
                                    items.push(stmt);
                                }
                            }
                        }
                        _ => {
                            // A doc comment above a broken statement
                            // stays pending deliberately: it attaches
                            // to the next definition that parses — the
                            // most useful recovery — and the file is
                            // rejected either way (a dangling-doc
                            // diagnostic here would buy nothing).
                            let suggestion = did_you_mean(word, STATEMENT_KEYWORDS);
                            self.diags.push(Diag::UnknownStatement {
                                span: word_span,
                                word: word.to_string(),
                                suggestion,
                            });
                            self.recover_to_statement_end();
                        }
                    }
                }
                None => {
                    let ch = self
                        .cursor_char()
                        .expect("not at end and a word cannot start here");
                    self.diags.push(Diag::IllegalCharacter {
                        span: Span::new(word_start, ch.len_utf8()),
                        ch,
                    });
                    pending_doc = None;
                    self.recover_to_statement_end();
                }
            }
        }
        SdlFile {
            items,
            trailing_comments: self.take_pending_comments(),
        }
    }

    /// `namespace <name>;` or `namespace <name> { };` — a name plus an
    /// optional doc comment, and nothing else (spec/namespace.md: a
    /// non-empty block is an error, because `namespace` defines no
    /// clauses). `keyword` carries the authored `ns` alias spelling
    /// for the writer.
    fn parse_namespace(&mut self, comment: Option<DocBlock>, keyword: Carried) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let name = self.read_statement_name()?;
        // `namespace` defines no clauses, so any block content is the
        // spec's non-empty-block error, diagnosed by the clause loop
        // with an empty did-you-mean table. A statement that parsed is
        // kept — the driver is already positioned at the next one. No
        // recovery on the junk arm: without a `}` there is no reliable
        // end marker to skip to, but the driver is already positioned
        // at the next thing — often exactly the statement the user
        // meant to write, which therefore still parses (`namespace a
        // namespace b;` keeps b). Junk after the name gets its own
        // driver-level diagnostic.
        // The bare tail's close note (a `// note` on the statement's
        // own line) rides the empty-path anchor like every statement.
        let close_note = match self.parse_stmt_block("namespace", &[])? {
            StmtTail::Bare { close_note } | StmtTail::Block { close_note, .. } => close_note,
        };
        Some(Stmt::Namespace(Namespace {
            name,
            keyword,
            comment: comment.map(|doc| doc.comment),
            comments,
            clause_comments: collect_clause_comments(&[], close_note),
        }))
    }

    /// The one clause-block loop every statement shares (rule 2's
    /// block form): trivia and bare `;`s are noise, `}` closes, EOF
    /// with the block open is `Err(unterminated-block span)`, and each
    /// clause dispatches through `clauses` (key → behavior): the key
    /// is read with multi-word extension (`include original`), then
    /// the kind decides which separator it takes — `:` the value
    /// forms, `{` a sub-block, `;` a flag. Unknown keys diagnose with
    /// did-you-mean over the statement's table (an empty table —
    /// `namespace` — makes every key unknown); a `///` line inside a
    /// block is framed as the misplaced clause it is (rule 5 attaches
    /// docs above definitions); junk and scanner errors recover to
    /// the next clause boundary. That recovery is load-bearing —
    /// without it the loop would re-read the same junk forever —
    /// which is exactly why the loop lives in ONE place rather than
    /// per statement. Returns the parsed clauses in source order; the
    /// caller folds last-one-wins (rule 7).
    fn clause_loop(
        &mut self,
        brace: Span,
        statement: &'static str,
        clauses: &'static [(&'static str, ClauseKind)],
    ) -> Result<Vec<ParsedClause>, Span> {
        let mut parsed: Vec<ParsedClause> = Vec::new();
        // The line the previous clause's terminator sat on: a pending
        // comment on that line is that clause's after-note; every
        // other comment anchors forward (rule 5's clause anchoring).
        let mut prev_end_line: Option<usize> = None;
        loop {
            self.take_comments();
            if let Some(line) = prev_end_line
                && let Some(note) = self.take_pending_same_line(line)
                && let Some(last) = parsed.last_mut()
            {
                last.after.push(note);
            }
            if self.scan.eat_byte(b'}') {
                return Ok(parsed);
            }
            if self.scan.is_at_end() {
                return Err(brace);
            }
            if self.scan.eat_byte(b';') {
                continue;
            }
            // A spread entry (`...<name>;`, spec/mixin.md) — a table
            // body entry, not a clause: no key, no colon, the leading
            // `...` is the whole grammar. A `.` starts no clause key
            // and no field head (dotted heads read their first
            // segment bare), so the probe is unambiguous. Table
            // bodies only: anywhere else the entry diagnoses as
            // misplaced (SDL-E1065) rather than falling to the
            // illegal-character arm's cryptic `.`.
            if self.src[self.scan.offset()..].starts_with("...") {
                if statement == "table" {
                    parsed.push(self.spread_entry());
                    prev_end_line = Some(self.cursor_line());
                    continue;
                }
                self.diags.push(Diag::SpreadMisplaced {
                    span: Span::new(self.scan.offset(), 3),
                    statement,
                });
                self.recover_to_clause_end();
                continue;
            }
            let key_start = self.scan.offset();
            match self.scan.read_ident() {
                Ok(Some((first_span, first_word, quoted))) => {
                    // Clause keys are SDL keywords: lowercase only
                    // (rule 9), spelled bare. A quoted spelling is an
                    // identifier, not a key — inside a table body a
                    // head-shaped one opens a nested field (its quoted
                    // first segment; spec/field.md), elsewhere it is
                    // diagnosed without a did-you-mean (the interior
                    // is not a keyword spelling, so suggesting one
                    // would be noise).
                    if quoted {
                        if matches!(statement, "table" | "mixin") && self.at_field_head_tail() {
                            parsed.push(self.nested_field_clause((first_span, first_word, true)));
                            // The nested field consumed its terminator
                            // — its line is the same-line split's next
                            // comparison, like every landed clause.
                            prev_end_line = Some(self.cursor_line());
                            continue;
                        }
                        self.diags.push(Diag::UnknownClause {
                            span: first_span,
                            key: first_word,
                            statement,
                            suggestion: None,
                        });
                        self.recover_to_clause_end();
                        continue;
                    }
                    let (key_span, key) = self.read_clause_key(clauses, first_span, first_word);
                    // Clause keys are SDL keywords: lowercase only
                    // (rule 9) — an uppercase spelling falls into the
                    // unknown-clause arm and gets a did-you-mean.
                    let Some((name, kind)) = clauses
                        .iter()
                        .find(|(known, _)| key == *known)
                        .map(|(name, kind)| (*name, kind))
                    else {
                        let candidates: Vec<&str> =
                            clauses.iter().map(|(known, _)| *known).collect();
                        let suggestion = did_you_mean(&key, &candidates);
                        // Inside a table body, an unknown bare head
                        // shaped like a field header opens a nested
                        // field (spec/field.md — the `field` prefix is
                        // optional; REQUIRED only to escape a name
                        // colliding with an active clause key). The
                        // typo guard is a NEAR-IDENTICAL spelling of a
                        // real key (one Damerau-Levenshtein edit —
                        // `permisions:`, `drop2;`), not mere
                        // similarity: `id:` and `in:` are
                        // spec-promised field names, not misspelled
                        // `index`. Uppercase spellings ride the
                        // unknown-clause arm — keys are lowercase, so
                        // `INDEX` here is a misspelling, not a field
                        // named INDEX. Quoted heads never reach this
                        // arm. `raw` is excluded: rule 8 reserves it
                        // in every block context (the README's
                        // reserved-words list), so a bare `raw:` head
                        // cannot silently become a field named raw —
                        // the `field` prefix or quoting is the escape.
                        if matches!(statement, "table" | "mixin")
                            && key != "raw"
                            && key
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                            && !candidates
                                .iter()
                                .any(|candidate| is_single_edit(&key, candidate))
                            && self.at_field_head_tail()
                        {
                            parsed.push(self.nested_field_clause((key_span, key, false)));
                            prev_end_line = Some(self.cursor_line());
                            continue;
                        }
                        self.diags.push(Diag::UnknownClause {
                            span: key_span,
                            key,
                            statement,
                            suggestion,
                        });
                        self.recover_to_clause_end();
                        continue;
                    };
                    // Rule 5's clause anchoring: what sits pending
                    // above the key is this clause's before-notes. A
                    // nested head is the one exception — it drains
                    // the buffer through its own statement entry, as
                    // its leading comments.
                    let before = if matches!(kind, ClauseKind::Nested) {
                        Vec::new()
                    } else {
                        self.take_pending_comments()
                    };
                    let value = match kind {
                        ClauseKind::Expr(slot) => self
                            .parse_expr_clause(key_span, name, *slot)
                            .map(ClauseValue::Expr),
                        ClauseKind::Permissions(slot) => self
                            .parse_permissions_clause(key_span, *slot)
                            .map(ClauseValue::Permission),
                        ClauseKind::Duration => self
                            .parse_duration_clause(key_span, name)
                            .map(ClauseValue::Duration),
                        ClauseKind::Flag => self
                            .parse_flag_clause(key_span, name)
                            .map(|_| ClauseValue::Flag),
                        ClauseKind::SubBlock(table) => {
                            self.parse_sub_block_clause(key_span, name, table)
                        }
                        ClauseKind::Level => self
                            .parse_level_clause(key_span, name)
                            .map(ClauseValue::Level),
                        ClauseKind::Str => {
                            self.parse_str_clause(key_span, name).map(ClauseValue::Str)
                        }
                        ClauseKind::WordList(allowed) => self
                            .parse_word_list_clause(key_span, name, allowed)
                            .map(ClauseValue::WordList),
                        ClauseKind::StrList => self
                            .parse_str_list_clause(key_span, name)
                            .map(ClauseValue::StrList),
                        ClauseKind::FilterList => self
                            .parse_filter_list_clause(key_span, name)
                            .map(ClauseValue::FilterList),
                        ClauseKind::FnPath => self
                            .parse_fn_path_clause(key_span, name)
                            .map(ClauseValue::FnPath),
                        ClauseKind::ExprList(slot) => self
                            .parse_expr_list_clause(key_span, name, *slot)
                            .map(ClauseValue::ExprList),
                        ClauseKind::DependentFlag(table) => {
                            self.parse_dependent_flag_clause(key_span, name, table)
                        }
                        ClauseKind::Ident => self
                            .parse_ident_clause(key_span, name)
                            .map(ClauseValue::Ident),
                        ClauseKind::Number => self
                            .parse_number_clause(key_span, name)
                            .map(ClauseValue::Number),
                        ClauseKind::Executable => self
                            .parse_executable_clause(key_span, name)
                            .map(ClauseValue::Executable),
                        ClauseKind::WordSet(words) => self
                            .parse_word_set_clause(key_span, name, words)
                            .map(ClauseValue::Word),
                        ClauseKind::WordOrSubBlock(words, table) => {
                            self.parse_word_or_sub_block_clause(key_span, name, words, table)
                        }
                        ClauseKind::IdentList => self
                            .parse_ident_list_clause(key_span, name)
                            .map(ClauseValue::IdentList),
                        ClauseKind::FnPathList => self
                            .parse_fn_path_list_clause(key_span, name)
                            .map(ClauseValue::FnPathList),
                        ClauseKind::Number32 => self
                            .parse_number32_clause(key_span, name)
                            .map(ClauseValue::Number32),
                        ClauseKind::MiddlewareList => self
                            .parse_middleware_list_clause(key_span, name)
                            .map(ClauseValue::Middleware),
                        ClauseKind::Bm25 => self
                            .parse_bm25_clause(key_span, name)
                            .map(ClauseValue::Bm25),
                        ClauseKind::Float => self
                            .parse_float_clause(key_span, name)
                            .map(ClauseValue::Float),
                        ClauseKind::Distance => self
                            .parse_distance_clause(key_span, name)
                            .map(ClauseValue::Distance),
                        ClauseKind::WordOrExpr(words, slot) => {
                            self.parse_word_or_expr_clause(key_span, name, words, *slot)
                        }
                        ClauseKind::OpPermissions(allow_delete) => self
                            .parse_op_permissions_clause(key_span, statement, *allow_delete)
                            .map(|(permissions, interior)| {
                                self.op_permissions_interior = interior;
                                ClauseValue::OpPermissions(permissions)
                            }),
                        ClauseKind::Nested => self
                            .parse_nested_head(name)
                            .map(|stmt| ClauseValue::Nested(Box::new(stmt))),
                    };
                    // A failed parse always pushed its own diagnostic;
                    // the Failed marker keeps the clause present so
                    // required-clause validation sees the attempt.
                    parsed.push(ParsedClause {
                        span: key_span,
                        name,
                        value: value.unwrap_or(ClauseValue::Failed),
                        before,
                        after: Vec::new(),
                        interior: std::mem::take(&mut self.op_permissions_interior),
                    });
                    // A value capture or flag consumed its `;`; a
                    // sub-block consumed its terminator; a failed
                    // parse recovered to the boundary. Either way the
                    // next clause starts right after it — and the
                    // cursor's line is this clause's terminator line,
                    // what the next iteration's same-line split
                    // compares against.
                    prev_end_line = Some(self.cursor_line());
                    continue;
                }
                Ok(None) => {
                    if self.scan.at_doc_comment() {
                        self.push_misplaced_doc_clause(statement);
                        self.recover_to_clause_end();
                        continue;
                    }
                    let ch = self
                        .cursor_char()
                        .expect("not at end and a word cannot start here");
                    self.diags.push(Diag::IllegalCharacter {
                        span: Span::new(key_start, ch.len_utf8()),
                        ch,
                    });
                    self.recover_to_clause_end();
                    continue;
                }
                Err(diag) => {
                    self.diags.push(diag);
                    self.recover_to_clause_end();
                    continue;
                }
            }
        }
    }

    /// A statement's optional clause block: the bare `;` form, the
    /// `{ … }` clause loop, EOF standing in for the `;` (the file's
    /// final statement), or the [`Diag::ExpectedStatementEnd`]
    /// position. `None` = diagnosed, the statement is dead (the
    /// cursor already rests at a boundary or EOF — recovery must not
    /// re-skip); [`StmtTail::Bare`] = the bare form;
    /// [`StmtTail::Block`] = the block's parsed clauses plus the
    /// block's `{` span (required-clause diagnostics anchor there).
    /// The one home of the block-tail shape every statement shares.
    fn parse_stmt_block(
        &mut self,
        statement: &'static str,
        clauses: &'static [(&'static str, ClauseKind)],
    ) -> Option<StmtTail> {
        self.take_comments();
        match self.scan.peek_byte() {
            Some(b';') => {
                self.scan.eat_byte(b';');
                let close_note = self.peek_same_line_note(self.cursor_line());
                Some(StmtTail::Bare { close_note })
            }
            Some(b'{') => {
                let brace = Span::new(self.scan.offset(), 1);
                self.scan.eat_byte(b'{');
                match self.clause_loop(brace, statement, clauses) {
                    Ok(parsed) => {
                        self.check_block_terminator();
                        let close_note = self.peek_same_line_note(self.cursor_line());
                        Some(StmtTail::Block {
                            clauses: parsed,
                            brace,
                            close_note,
                        })
                    }
                    Err(span) => {
                        self.diags.push(Diag::UnterminatedBlock { span });
                        None
                    }
                }
            }
            None => Some(StmtTail::Bare { close_note: None }),
            _ => {
                self.diags.push(Diag::ExpectedStatementEnd {
                    span: Span::point(self.scan.offset()),
                    found: self.cursor_char(),
                });
                None
            }
        }
    }

    /// Captures the pending `///` block. Only called when
    /// [`Scanner::at_doc_comment`] held, so the block is non-empty.
    fn take_doc_block(&mut self) -> DocBlock {
        // The block's `///` opener is exactly at the cursor (the
        // caller checked `at_doc_comment`); the line-text spans inside
        // start after the eaten `/// ` prefix, so deriving the opener
        // from them would land one byte right whenever the
        // conventional space follows.
        let block_start = self.scan.offset();
        let line_spans = self.scan.take_doc_block();
        let lines = line_spans
            .iter()
            .map(|line| line.slice(self.src).to_string())
            .collect();
        DocBlock {
            comment: DocComment { lines },
            span: Span::new(block_start, 3),
        }
    }

    /// A clause block's `key: <expr>;` value. The colon is what
    /// switches the lexer into capture mode (rule 2) — `key {` would
    /// start an SDL sub-block, so its absence here is
    /// [`Diag::ExpectedClauseValue`], not a capture. Whitespace before
    /// the colon is trivia like everywhere else in the grammar.
    fn parse_expr_clause(&mut self, key_span: Span, key: &'static str, slot: Slot) -> Option<Expr> {
        let span = self.capture_value(key_span, key)?;
        self.canonicalize_capture(span, slot)
    }

    /// Captures a value clause's text after its `:` — everything to
    /// the depth-0 `;` (consumed by the scanner). The colon is the
    /// form selector (rule 2); the capture itself and its EOF contract
    /// are [`Self::capture_to_terminator`]'s. On a missing `:` or a
    /// failed capture the diagnostic is pushed and `None` comes back.
    fn capture_value(&mut self, key_span: Span, key: &'static str) -> Option<Span> {
        self.take_comments();
        if !self.scan.eat_byte(b':') {
            self.diags.push(Diag::ExpectedClauseValue {
                span: key_span,
                key,
            });
            self.recover_to_clause_end();
            return None;
        }
        self.capture_to_terminator()
    }

    /// Captures from the cursor to the depth-0 `;` (consumed by the
    /// scanner), with EOF standing in on the file's final clause like
    /// every capture (upstream requires `;` *between* statements
    /// only). The one home of the capture cursor-contract: the
    /// scanner rests at EOF on the unterminated error, so
    /// the capture's start is recorded here, before the call — value
    /// clauses ([`Self::capture_value`]) and short-form expressions
    /// ([`Self::capture_expr`]) share it.
    fn capture_to_terminator(&mut self) -> Option<Span> {
        let start = self.scan.offset();
        match self.scan.capture_until(b";") {
            Ok((span, _)) => Some(span),
            Err(Diag::UnterminatedCapture { .. }) => Some(Span::new(start, self.src.len() - start)),
            Err(diag) => {
                self.diags.push(diag);
                None
            }
        }
    }

    /// Extends an already-read first word into a full clause key:
    /// while any table entry continues past the accumulated words
    /// (`include` → `include original`), a following word joins the
    /// key across SDL trivia; longest full match wins (rule 2's
    /// multi-word keys). A non-word token (`:`, `;`, `{`) or a word
    /// that completes no candidate leaves the key as read — the
    /// extension is greedy, so a broken multi-word key is diagnosed
    /// as the whole misspelling rather than two fragments.
    fn read_clause_key(
        &mut self,
        clauses: &'static [(&'static str, ClauseKind)],
        first_span: Span,
        first_word: String,
    ) -> (Span, String) {
        let mut key = first_word;
        let mut end = first_span.end();
        loop {
            let continues = clauses.iter().any(|(known, _)| {
                known.len() > key.len()
                    && known.starts_with(&key)
                    && known.as_bytes()[key.len()] == b' '
            });
            // Only a word continues a multi-word key; anything else
            // (`:`, `;`, `{`, `}`) leaves it as read.
            let at_word = matches!(
                {
                    self.take_comments();
                    self.scan.peek_byte()
                },
                Some(b) if b.is_ascii_alphabetic() || b == b'_'
            );
            if !continues || !at_word {
                return (Span::new(first_span.offset, end - first_span.offset), key);
            }
            match self.scan.read_ident() {
                Ok(Some((span, word, _quoted))) => {
                    key.push(' ');
                    key.push_str(&word);
                    end = span.end();
                }
                // Not identifier-shaped: the key stands as read and
                // the junk is left for the caller's lookup/recovery.
                _ => return (Span::new(first_span.offset, end - first_span.offset), key),
            }
        }
    }

    /// A `<duration>` clause `key: <duration>;` — rule 3's keyword
    /// clause: the capture rides the depth-0 `;` scanner like an
    /// expression's, but the text is validated by SDL as a SurrealQL
    /// duration literal and never reaches the embedded parser.
    fn parse_duration_clause(&mut self, key_span: Span, key: &'static str) -> Option<Duration> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        match parse_duration_text(text) {
            Some(expiry) => Some(expiry),
            None => {
                self.diags.push(Diag::InvalidDuration {
                    span,
                    text: text.trim().to_string(),
                });
                None
            }
        }
    }

    /// A flag clause `key;` (rule 2): the bare `;` confirms it. A `:`
    /// or `{` here selects another clause form — that mismatch is
    /// [`Diag::UnexpectedClauseForm`], with the flag's own spelling as
    /// the remedy. EOF (final clause, `;` omitted) is tolerated: the
    /// enclosing loop reports the unterminated block, like a capture
    /// ending at EOF.
    fn parse_flag_clause(&mut self, key_span: Span, key: &'static str) -> Option<()> {
        self.take_comments();
        match self.scan.peek_byte() {
            Some(b';') => {
                self.scan.eat_byte(b';');
                Some(())
            }
            None => Some(()),
            _ => {
                self.diags.push(Diag::UnexpectedClauseForm {
                    span: key_span,
                    message: format!(
                        "clause `{key}` is a flag — write `{key};` (no value, no block)"
                    ),
                });
                self.recover_to_clause_end();
                None
            }
        }
    }

    /// The `{ … };` interior shape every nested block shares — clause
    /// sub-blocks (rule 2) and api's structural group bodies alike:
    /// eat the `{`, run the clause loop, and demand the `;`
    /// terminator only after a clean body (one root cause, one
    /// diagnostic — when the body already failed, recovery may have
    /// parked the cursor on the enclosing `}`, and the terminator
    /// report would aim at an innocent character). `None` = the block
    /// never closed ([`Diag::UnterminatedBlock`] pushed; the cursor
    /// rests at EOF, so the statement is dead).
    fn parse_block_interior(
        &mut self,
        statement: &'static str,
        clauses: &'static [(&'static str, ClauseKind)],
    ) -> Option<Vec<ParsedClause>> {
        self.take_comments();
        let brace = Span::new(self.scan.offset(), 1);
        self.scan.eat_byte(b'{');
        let body_diags = self.diags.len();
        match self.clause_loop(brace, statement, clauses) {
            Ok(parsed) => {
                if self.diags.len() == body_diags {
                    self.check_block_terminator();
                } else {
                    self.take_comments();
                    self.scan.eat_byte(b';');
                }
                Some(parsed)
            }
            Err(span) => {
                self.diags.push(Diag::UnterminatedBlock { span });
                None
            }
        }
    }

    /// A colon-less sub-block `key { clauses };` (rule 2) —
    /// `changefeed` is the first. The body parses through the nested
    /// table via the shared [`FileParser::clause_loop`], so recovery,
    /// did-you-mean, and misplaced-doc handling all apply inside it;
    /// the `;` after the closing `}` is required like any clause
    /// terminator. The returned [`ClauseValue::Sub`] leaves required-
    /// clause enforcement to the folding caller, which owns the
    /// statement's shape.
    fn parse_sub_block_clause(
        &mut self,
        key_span: Span,
        key: &'static str,
        table: &'static [(&'static str, ClauseKind)],
    ) -> Option<ClauseValue> {
        self.take_comments();
        if self.scan.peek_byte() == Some(b'{') {
            return self.parse_block_interior(key, table).map(ClauseValue::Sub);
        }
        self.diags.push(Diag::UnexpectedClauseForm {
            span: key_span,
            message: format!(
                "clause `{key}` takes a sub-block — write `{key} {{ … }};` \
                 (a `:` would start a SurrealQL expression capture)"
            ),
        });
        self.recover_to_clause_end();
        None
    }

    /// A dependent clause group `key;` / `key { clauses };` (spec
    /// README's flag-plus-sub-block pattern) — the first
    /// [`ClauseKind`] where flag and sub-block share a key: the bare
    /// flag selects the parent's defaults, the same-key block
    /// configures the parameters, and the parameters never appear as
    /// standalone clauses outside the block. `async; async { retry:
    /// 2; };` folds last-one-wins like any clause (rule 7), so the
    /// README's ≡ `async { retry: 2; };` holds.
    fn parse_dependent_flag_clause(
        &mut self,
        key_span: Span,
        key: &'static str,
        table: &'static [(&'static str, ClauseKind)],
    ) -> Option<ClauseValue> {
        self.take_comments();
        match self.scan.peek_byte() {
            Some(b';') => {
                self.scan.eat_byte(b';');
                Some(ClauseValue::Flag)
            }
            // EOF on the file's final clause: the enclosing loop
            // reports the unterminated block, like a bare flag's.
            None => Some(ClauseValue::Flag),
            Some(b'{') => self.parse_sub_block_clause(key_span, key, table),
            _ => {
                self.diags.push(Diag::UnexpectedClauseForm {
                    span: key_span,
                    message: format!(
                        "clause `{key}` is a flag or a sub-block — write `{key};` or \
                         `{key} {{ … }};` (a `:` would start a SurrealQL expression capture)"
                    ),
                });
                self.recover_to_clause_end();
                None
            }
        }
    }

    /// Captures the expression right after a `:` or `=`: everything
    /// to the depth-0 `;` (consumed by the scanner), canonicalized
    /// through the embedded parser ([`expr::canonicalize`]). EOF
    /// without `;` is accepted as the capture end — upstream requires
    /// `;` *between* statements only, so the file's final statement
    /// may omit it, exactly like the bare statement forms.
    fn capture_expr(&mut self, slot: Slot) -> Option<Expr> {
        let span = self.capture_to_terminator()?;
        self.canonicalize_capture(span, slot)
    }

    /// Hands one capture span to the embedded-expression bridge; a
    /// rejection (or an internal invariant break — never a panic)
    /// becomes [`Diag::EmbeddedParseError`] over the capture, its
    /// message carrying the upstream render — the snippet marker
    /// re-anchored to the capture's file position for stage-1
    /// rejections, harness-relative for stage 2.
    fn canonicalize_capture(&mut self, span: Span, slot: Slot) -> Option<Expr> {
        let capture = span.slice(self.src);
        match expr::canonicalize(capture, slot, expr::line_col_at(self.src, span.offset)) {
            Ok(canon) => Some(Expr { canon }),
            Err(EmbeddedError::Rejected { message }) | Err(EmbeddedError::Internal { message }) => {
                self.diags.push(Diag::EmbeddedParseError { span, message });
                None
            }
        }
    }

    /// `sequence <name> (= <expr> | { … } | );` — spec/sequence.md.
    /// `sequence` has no COMMENT clause, so a `///` block attached to
    /// it is rejected (rule 5) while the statement itself still
    /// parses; its block form is free-ordered (rule 7) and codegen
    /// emits the fixed SurrealQL order.
    fn parse_sequence(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        if let Some(doc) = pending_doc {
            self.diags.push(Diag::DocCommentUnsupported {
                span: doc.span,
                statement: "sequence",
            });
        }
        let name = self.read_statement_name()?;
        self.take_comments();
        let comments = self.take_pending_comments();
        let mut stmt = Sequence {
            name,
            start: None,
            batch: None,
            timeout: None,
            comments,
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        };
        // Assignment short form: `sequence n = 1000;` — the start
        // value, everything else default. Checked ahead of the block
        // tail (the only extra separator this statement's tail takes).
        // A same-line note after the `;` joins the short form's own
        // line (rule 5's empty-path anchor — the capture consumed the
        // terminator, so the peek is here).
        if self.scan.peek_byte() == Some(b'=') {
            self.scan.eat_byte(b'=');
            stmt.start = Some(self.capture_expr(Slot::SequenceStart)?);
            let mut wrapped = Stmt::Sequence(stmt);
            if let Some(note) = self.peek_same_line_note(self.cursor_line()) {
                attach_nested_close_note(&mut wrapped, vec![note]);
            }
            return Some(wrapped);
        }
        let clause_comments;
        match self.parse_stmt_block("sequence", SEQUENCE_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                close_note,
                ..
            }) => {
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.name, clause.value) {
                        ("start", ClauseValue::Expr(e)) => stmt.start = Some(e),
                        ("batch", ClauseValue::Expr(e)) => stmt.batch = Some(e),
                        ("timeout", ClauseValue::Expr(e)) => stmt.timeout = Some(e),
                        (_, ClauseValue::Failed) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        stmt.clause_comments = clause_comments;
        stmt.trailing_comments = self.take_pending_comments();
        Some(Stmt::Sequence(stmt))
    }

    /// `param $<name> (= <expr> | { … } | );` — spec/param.md. The
    /// name keeps its `$` (SurrealQL-faithful). After it, upstream's
    /// parameter lexer accepts keywords, quoted spellings, and even
    /// digit-led *quoted* names (`` $`9lives` ``) — SDL accepts all of
    /// those too — while a *bare* digit-led name does not lex as a
    /// parameter upstream and is rejected here (probe-verified).
    fn parse_param(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        let comment = pending_doc.map(|doc| doc.comment);
        self.take_comments();
        if !self.scan.eat_byte(b'$') {
            self.diags.push(Diag::ExpectedName {
                span: Span::point(self.scan.offset()),
            });
            self.recover_to_statement_end();
            return None;
        }
        let name_start = self.scan.offset();
        let raw = match self.scan.read_ident() {
            Ok(Some((name_span, raw, quoted))) => {
                if !quoted && raw.starts_with(|c: char| c.is_ascii_digit()) {
                    self.diags.push(Diag::ExpectedName { span: name_span });
                    self.recover_to_statement_end();
                    return None;
                }
                raw
            }
            Ok(None) => {
                self.diags.push(Diag::ExpectedName {
                    span: Span::point(name_start),
                });
                self.recover_to_statement_end();
                return None;
            }
            Err(diag) => {
                self.diags.push(diag);
                self.recover_to_statement_end();
                return None;
            }
        };
        let mut stmt = Param {
            name: ParamName::new(raw),
            value: None,
            permissions: None,
            comment,
            comments: self.take_pending_comments(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        };
        self.take_comments();
        // Assignment short form: `param $x = 50;` — value only. A `{`
        // after `=` stays inside the capture (it is an object/block
        // literal), so the trailing-block rejection rule needs no
        // extra machinery: `param $x = 50 { … };` is rejected by the
        // embedded parser, while `param $x = { a: 1 };` is the
        // object-valued short form. Checked ahead of the block tail
        // (the only extra separator this statement's tail takes).
        if self.scan.peek_byte() == Some(b'=') {
            self.scan.eat_byte(b'=');
            stmt.value = Some(self.capture_expr(Slot::ParamValue)?);
            let mut wrapped = Stmt::Param(stmt);
            if let Some(note) = self.peek_same_line_note(self.cursor_line()) {
                attach_nested_close_note(&mut wrapped, vec![note]);
            }
            return Some(wrapped);
        }
        let clause_comments;
        match self.parse_stmt_block("param", PARAM_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                close_note,
                ..
            }) => {
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.name, clause.value) {
                        ("value", ClauseValue::Expr(e)) => stmt.value = Some(e),
                        ("permissions", ClauseValue::Permission(p)) => stmt.permissions = Some(p),
                        (_, ClauseValue::Failed) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            // Bare form: stores VALUE NONE (SurrealQL parity).
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        stmt.clause_comments = clause_comments;
        stmt.trailing_comments = self.take_pending_comments();
        Some(Stmt::Param(stmt))
    }

    /// `database <name> { … };` (spec/database.md) — also spelled
    /// `db` (rule 9's lexer-level alias; `keyword` carries the authored
    /// spelling for the writer). The block is free-ordered
    /// (rule 7) and optional when empty; a `///` block attaches (the
    /// statement has a COMMENT clause). The changefeed sub-block
    /// folds last-one-wins, and `expiry` is required inside it — a
    /// changefeed without one is diagnosed and dropped while the
    /// statement survives.
    fn parse_database(&mut self, pending_doc: Option<DocBlock>, keyword: Carried) -> Option<Stmt> {
        let comment = pending_doc.map(|doc| doc.comment);
        let name = self.read_statement_name()?;
        self.take_comments();
        let mut stmt = Database {
            name,
            keyword,
            strict: false,
            changefeed: None,
            comment,
            comments: self.take_pending_comments(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        };
        let clause_comments;
        match self.parse_stmt_block("database", DATABASE_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                close_note,
                ..
            }) => {
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.name, clause.value) {
                        ("strict", ClauseValue::Flag) => stmt.strict = true,
                        ("changefeed", ClauseValue::Sub(sub)) => {
                            let (changefeed, expiry_attempted) = changefeed_of(&sub);
                            if let Some(changefeed) = changefeed {
                                stmt.changefeed = Some(changefeed);
                            } else if !expiry_attempted {
                                // No expiry clause at all; a failed
                                // attempt already carried its own
                                // diagnostic.
                                self.diags.push(Diag::RequiredClauseMissing {
                                    span: clause.span,
                                    clause: "expiry",
                                    block: "changefeed",
                                });
                            }
                        }
                        (_, ClauseValue::Failed) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        stmt.clause_comments = clause_comments;
        stmt.trailing_comments = self.take_pending_comments();
        Some(Stmt::Database(stmt))
    }

    /// `bucket <name> { … };` (spec/bucket.md). The server gates the
    /// statement behind the experimental `files` capability at define
    /// time; SDL accepts it unconditionally (spec/README.md rule 4)
    /// — only the expression clauses' canonicalization needs the
    /// capability-enabled embed (see [`crate::expr`]).
    fn parse_bucket(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        let comment = pending_doc.map(|doc| doc.comment);
        let name = self.read_statement_name()?;
        self.take_comments();
        let mut stmt = Bucket {
            name,
            backend: None,
            readonly: false,
            permissions: None,
            comment,
            comments: self.take_pending_comments(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        };
        let clause_comments;
        match self.parse_stmt_block("bucket", BUCKET_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                close_note,
                ..
            }) => {
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.name, clause.value) {
                        ("backend", ClauseValue::Expr(expr)) => stmt.backend = Some(expr),
                        ("readonly", ClauseValue::Flag) => stmt.readonly = true,
                        ("permissions", ClauseValue::Permission(p)) => stmt.permissions = Some(p),
                        (_, ClauseValue::Failed) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        stmt.clause_comments = clause_comments;
        stmt.trailing_comments = self.take_pending_comments();
        Some(Stmt::Bucket(stmt))
    }

    /// `user <name> { … };` (spec/user.md) — `on` is required
    /// (upstream `expected!(ON)`): a user without it is diagnosed
    /// against the block (or the would-be block position on the bare
    /// form) and the statement dropped. The `password`/`passhash`
    /// exclusivity is parser-enforced upstream ("Can't set both") and
    /// diagnosed here at the second clause (cascade rule: a failed
    /// attempt already carries its own diagnostic, so only successful
    /// values collide).
    fn parse_user(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let comment = pending_doc.map(|doc| doc.comment);
        let name = self.read_statement_name()?;
        self.take_comments();
        let mut on: Option<Authored<Level>> = None;
        let mut on_attempted = false;
        let mut pass: Option<Pass> = None;
        let mut pass_seen: Option<&'static str> = None;
        let mut scram: Option<String> = None;
        let mut scram_invalid = false;
        let mut roles: Vec<Role> = Vec::new();
        let mut token_duration: Option<Expr> = None;
        let mut session_duration: Option<Expr> = None;
        let mut missing_on_span = Span::point(self.scan.offset());
        let clause_comments;
        match self.parse_stmt_block("user", USER_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                brace: block_brace,
                close_note,
            }) => {
                // A block user missing `on` anchors on the block's `{`.
                missing_on_span = block_brace;
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.value, clause.name) {
                        (ClauseValue::Level(level), "on") => on = Some(level),
                        (ClauseValue::Failed, "on") => on_attempted = true,
                        (ClauseValue::Str(value), name @ ("password" | "passhash")) => {
                            // Upstream's parser-enforced
                            // exclusivity is between the two
                            // KINDS: a repeat of the same
                            // clause folds last-one-wins
                            // (rule 7), only the opposite
                            // kind collides.
                            let other = if name == "password" {
                                "passhash"
                            } else {
                                "password"
                            };
                            match pass_seen {
                                Some(seen) if seen != name => {
                                    self.diags.push(Diag::MutuallyExclusive {
                                        span: clause.span,
                                        clause: name,
                                        other,
                                    });
                                    pass = None;
                                    pass_seen = Some(name);
                                }
                                _ => {
                                    pass_seen = Some(name);
                                    pass = Some(if name == "password" {
                                        Pass::Password(value)
                                    } else {
                                        Pass::Hash(value)
                                    });
                                }
                            }
                        }
                        (ClauseValue::Str(value), "passscram") => {
                            // Upstream validates the verifier at DDL
                            // parse — the mirror keeps accepted SDL
                            // emitting engine-parseable DDL (the
                            // kind-grammar precedent). This rejects
                            // INFO's `'[REDACTED]'` sentinel outright:
                            // the clause can never round-trip, so the
                            // parse error names the trap instead of a
                            // lint warning (the passhash sentinel's
                            // class — that one parses as DDL, this
                            // never can). A repeat folds
                            // last-one-wins (rule 7) between valid
                            // values; an invalid one drops the
                            // statement (fail-closed).
                            let reason = if value == SECRET_SENTINEL_CANON_DECODED {
                                Some(
                                    "the `'[REDACTED]'` sentinel is INFO's redaction — \
                                     a PASSSCRAM verifier never round-trips; author the \
                                     real value"
                                        .to_string(),
                                )
                            } else {
                                validate_scram_verifier(&value)
                                    .err()
                                    .map(|reason| reason.to_string())
                            };
                            if let Some(message) = reason {
                                self.diags.push(Diag::InvalidPassscram {
                                    span: clause.span,
                                    message,
                                });
                                scram_invalid = true;
                            } else {
                                scram = Some(value);
                            }
                        }
                        (ClauseValue::WordList(words), "roles") => {
                            roles = words
                                .iter()
                                .map(|word| match word.as_str() {
                                    "owner" => Role::Owner,
                                    "editor" => Role::Editor,
                                    _ => Role::Viewer,
                                })
                                .collect();
                        }
                        (ClauseValue::Sub(sub), "duration") => {
                            let (token, session) = duration_of(&sub);
                            token_duration = token;
                            session_duration = session;
                        }
                        (ClauseValue::Failed, _) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            // Bare form at EOF: a user with no clauses is missing its
            // required `on`.
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        let on = match on {
            Some(on) => on,
            None => {
                if !on_attempted {
                    self.diags.push(Diag::RequiredClauseMissing {
                        span: missing_on_span,
                        clause: "on",
                        block: "user",
                    });
                }
                return None;
            }
        };
        if scram_invalid {
            return None;
        }
        Some(Stmt::User(User {
            name,
            on,
            pass,
            scram,
            roles,
            token_duration,
            session_duration,
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// `analyzer <name> { … };` (spec/analyzer.md) — every clause
    /// optional (an analyzer with none is accepted, though inert);
    /// list clauses fold last-one-wins, a repeat replacing the whole
    /// list (SurrealQL parity).
    fn parse_analyzer(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let comment = pending_doc.map(|doc| doc.comment);
        let name = self.read_statement_name()?;
        self.take_comments();
        let mut function: Option<FnPath> = None;
        let mut tokenizers: Option<Vec<Tokenizer>> = None;
        let mut filters: Option<Vec<Filter>> = None;
        let clause_comments;
        match self.parse_stmt_block("analyzer", ANALYZER_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                close_note,
                ..
            }) => {
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.value, clause.name) {
                        (ClauseValue::FnPath(path), "function") => function = Some(path),
                        (ClauseValue::WordList(words), "tokenizers") => {
                            tokenizers = Some(
                                words
                                    .iter()
                                    .map(|word| match word.as_str() {
                                        "blank" => Tokenizer::Blank,
                                        "camel" => Tokenizer::Camel,
                                        "class" => Tokenizer::Class,
                                        _ => Tokenizer::Punct,
                                    })
                                    .collect(),
                            );
                        }
                        (ClauseValue::FilterList(list), "filters") => filters = Some(list),
                        (ClauseValue::Failed, _) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        Some(Stmt::Analyzer(Analyzer {
            name,
            function,
            tokenizers,
            filters,
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// The `on:` fold the three hostable statements share (field,
    /// index, event — the [`FieldHost`] three-mode model): standalone
    /// demands the clause, a table block supplies the host and forbids
    /// the explicit spelling, and a mixin body holds the empty
    /// placeholder expansion overwrites with the spreading table. One
    /// home, so the three cannot drift. `None` = the statement is
    /// dead (its diagnostic already fired — a failed `on:` attempt
    /// carries its own, and the missing-clause report is suppressed
    /// for it: one root cause, one diagnostic).
    fn fold_on(
        &mut self,
        statement: &'static str,
        host: FieldHost<'_>,
        on: OnClause,
        missing_block: &'static str,
    ) -> Option<Ident> {
        // An explicit `on:` where the host is already decided (the
        // table block's host, or the mixin body's deferred one) is the
        // forbidden spelling there — exactly one of the two applies.
        if !matches!(host, FieldHost::Standalone)
            && let Some(span) = on.explicit_span
        {
            self.diags.push(Diag::InvalidNestedOn { span, statement });
            return None;
        }
        match host {
            FieldHost::Table(host_ident) if !on.attempted => Some(host_ident.clone()),
            // The empty placeholder: `on` is never printed in the
            // nested form and codegen runs post-expansion, which
            // overwrites it with the spreading table.
            FieldHost::Mixin if !on.attempted => Some(Ident::new("")),
            // A failed `on:` attempt in a host-supplied mode already
            // carried its own diagnostic. (The old field fold kept
            // such a member alive with the placeholder; this fold
            // drops it — unobservable either way, a dirty parse never
            // reaches a consumer: check/build/fmt/lint all exit on
            // the diagnostics first.)
            FieldHost::Table(_) | FieldHost::Mixin => None,
            FieldHost::Standalone => match on.captured {
                Some(on) => Some(on),
                None => {
                    if !on.attempted {
                        self.diags.push(Diag::RequiredClauseMissing {
                            span: on.missing_span,
                            clause: "on",
                            block: missing_block,
                        });
                    }
                    None
                }
            },
        }
    }

    /// `event <name> { … };` (spec/event.md) — `on` and `then` are
    /// required (upstream rejects an event without `ON` or with an
    /// empty `THEN`); `when` and `async` are optional. Folds are
    /// last-one-wins (rule 7): a repeated `then:` replaces the whole
    /// list, a repeated `async` — flag or block — replaces the whole
    /// group, so `async; async { retry: 2; };` ≡
    /// `async { retry: 2; };` by the same fold that governs every
    /// clause. In the nested form (a `table` block head) the host
    /// table IS the `on`; a mixin member holds the placeholder
    /// expansion overwrites — an explicit `on:` diagnoses in either
    /// host-supplied mode ([`Self::parse_event_inner`]).
    fn parse_event(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        self.parse_event_inner(pending_doc, FieldHost::Standalone, false)
    }

    /// `nested` marks the table-block head form (clause-level
    /// failure recovery — see [`Self::parse_index_inner`]).
    fn parse_event_inner(
        &mut self,
        pending_doc: Option<DocBlock>,
        host: FieldHost<'_>,
        nested: bool,
    ) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let comment = pending_doc.map(|doc| doc.comment);
        let name = self.read_statement_name_in_block(nested)?;
        self.take_comments();
        let mut on: Option<Ident> = None;
        let mut on_attempted = false;
        let mut on_forbidden: Option<Span> = None;
        let mut when: Option<Expr> = None;
        let mut then: Vec<Expr> = Vec::new();
        let mut then_attempted = false;
        let mut r#async: Option<Async> = None;
        let mut missing_span = Span::point(self.scan.offset());
        let clause_comments;
        match self.parse_stmt_block("event", EVENT_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                brace,
                close_note,
            }) => {
                missing_span = brace;
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.value, clause.name) {
                        (ClauseValue::Ident(table), "on") => {
                            // Host-supplied modes (the table block's
                            // host, the mixin body's deferred one): an
                            // explicit `on:` is the forbidden spelling
                            // there (spec/table.md — exactly one of
                            // the two applies).
                            if matches!(host, FieldHost::Standalone) {
                                on = Some(table);
                            } else {
                                on_forbidden = Some(clause.span);
                            }
                        }
                        (ClauseValue::Failed, "on") => on_attempted = true,
                        (ClauseValue::Expr(expr), "when") => when = Some(expr),
                        (ClauseValue::ExprList(items), "then") => then = items,
                        (ClauseValue::Failed, "then") => then_attempted = true,
                        (ClauseValue::Flag, "async") => {
                            // The bare flag: SurrealQL's
                            // 1/3 defaults, no sides to
                            // emit.
                            r#async = Some(Async {
                                retry: None,
                                maxdepth: None,
                            });
                        }
                        (ClauseValue::Sub(sub), "async") => {
                            let mut params = Async {
                                retry: None,
                                maxdepth: None,
                            };
                            for sub_clause in sub {
                                match (sub_clause.value, sub_clause.name) {
                                    (ClauseValue::Number(value), "retry") => {
                                        params.retry = Some(value);
                                    }
                                    (ClauseValue::Number(value), "maxdepth") => {
                                        params.maxdepth = Some(value);
                                    }
                                    (ClauseValue::Failed, _) => {}
                                    _ => unreachable!("clause_loop yields table keys only"),
                                }
                            }
                            r#async = Some(params);
                        }
                        (ClauseValue::Failed, _) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            // Bare form at EOF: both required clauses are missing.
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        let on = self.fold_on(
            "event",
            host,
            OnClause {
                captured: on,
                explicit_span: on_forbidden,
                attempted: on_attempted,
                missing_span,
            },
            "event",
        );
        if then.is_empty() && !then_attempted {
            self.diags.push(Diag::RequiredClauseMissing {
                span: missing_span,
                clause: "then",
                block: "event",
            });
        }
        match (on, then) {
            (Some(on), then) if !then.is_empty() => Some(Stmt::Event(Event {
                name,
                on,
                when,
                then,
                r#async,
                comment,
                comments,
                trailing_comments: self.take_pending_comments(),
                clause_comments,
            })),
            _ => None,
        }
    }

    /// `module [<name>] { … };` (spec/module.md) — the grammar is the
    /// 3.3 module form, the one spelling SDL emits. `from`
    /// and `unsigned` are required (SDL's grammar; the 3.2 parser
    /// additionally rejects storing anonymous file modules at
    /// execution — verified), each diagnosed separately when missing,
    /// per [`Self::parse_event`]'s precedent. The name–executable
    /// coupling is validated after the fold: a file executable
    /// requires the header name, a silo executable is self-named and
    /// rejects one.
    fn parse_module(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let comment = pending_doc.map(|doc| doc.comment);
        self.take_comments();
        // The name is optional only when the block starts right here —
        // `module { … };`. Anything else is the name position (a bare
        // `module;` therefore reads as a missing name, which is what
        // the grammar says).
        let name_start = self.scan.offset();
        let (name, name_span) = if self.scan.peek_byte() == Some(b'{') {
            (None, Span::point(self.scan.offset()))
        } else {
            let name = self.read_statement_name()?;
            (
                Some(name),
                Span::new(name_start, self.scan.offset() - name_start),
            )
        };
        self.take_comments();
        let mut executable: Option<Executable> = None;
        let mut from_attempted = false;
        let mut from_span = Span::point(self.scan.offset());
        let mut unsigned = false;
        let mut unsigned_attempted = false;
        let mut permissions: Option<Permission> = None;
        let mut missing_span = Span::point(self.scan.offset());
        let clause_comments;
        match self.parse_stmt_block("module", MODULE_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                brace,
                close_note,
            }) => {
                missing_span = brace;
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.value, clause.name) {
                        (ClauseValue::Executable(exec), "from") => {
                            executable = Some(exec);
                            from_span = clause.span;
                        }
                        (ClauseValue::Failed, "from") => from_attempted = true,
                        (ClauseValue::Flag, "unsigned") => unsigned = true,
                        (ClauseValue::Failed, "unsigned") => unsigned_attempted = true,
                        (ClauseValue::Permission(p), "permissions") => permissions = Some(p),
                        (ClauseValue::Failed, _) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            // Bare form at EOF: both required clauses are missing.
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        // Required-clause validation (a failed attempt already carried
        // its own diagnostic — one root cause, one diagnostic).
        if executable.is_none() && !from_attempted {
            self.diags.push(Diag::RequiredClauseMissing {
                span: missing_span,
                clause: "from",
                block: "module",
            });
        }
        if !unsigned && !unsigned_attempted {
            self.diags.push(Diag::RequiredClauseMissing {
                span: missing_span,
                clause: "unsigned;",
                block: "module",
            });
        }
        let executable = executable?;
        // Name–executable coupling (spec/module.md). The silo arm's
        // self-naming is the documented 3.3 form taking no alias; the
        // file arm's requirement is upstream's own rejection.
        match (&name, &executable) {
            (None, Executable::File { .. }) => {
                self.diags.push(Diag::ModuleExecutableName {
                    span: from_span,
                    message:
                        "a file module requires a name — write `module <name> { from: f'…'; … };`"
                            .to_string(),
                });
                return None;
            }
            (Some(_), Executable::Silo { .. }) => {
                self.diags.push(Diag::ModuleExecutableName {
                    span: name_span,
                    message: "a silo module is self-named — remove the header name \
                              (the 3.3 silo form takes no alias)"
                        .to_string(),
                });
                return None;
            }
            _ => {}
        }
        // The required flag parsed above; a failed `unsigned:` attempt
        // already carried its own diagnostic.
        if !unsigned {
            return None;
        }
        Some(Stmt::Module(Module {
            name,
            executable,
            unsigned,
            permissions,
            comment,
            from_span,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// `fn <path>([$<param>: <kind>, …]) [-> <kind>] { <body> }
    /// [{ … }];` (spec/function.md) — the one two-block statement.
    /// The signature header is SDL-lexed positionally, with the kind
    /// parser mirroring SurrealQL's kind grammar (README rule 4); the
    /// body is captured brace-balanced and canonicalized through the
    /// embedded parser's own block print ([`Slot::FunctionBody`]);
    /// the optional second block is the ordinary clause loop. No
    /// required-clause tracking: the name, parameter list, and body
    /// are syntactic positions, enforced here.
    fn parse_function(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let comment = pending_doc.map(|doc| doc.comment);
        self.scan.skip_surrealql_trivia();
        // The name is the path AFTER `fn::` (spec/function.md): one
        // segment, then `::`-joined more. Segments follow the fn-path
        // rules — keyword-like words bare, quoted spellings decoded —
        // so they read through `read_ident`, not the statement-name
        // position (which rejects bare reserved words upstream
        // accepts in fn paths).
        let mut segments = Vec::new();
        match self.scan.read_ident() {
            Ok(Some((segment_span, segment, _))) => {
                // The keyword already carries the `fn::` prefix, so a
                // first segment spelling `fn` would emit
                // `fn::fn::…` — a different function than the author
                // spelled (issue #110). Fail at the declaration; SDL
                // spells the header path bare (`raw` carries the
                // SurrealQL spelling if the doubled name is really
                // wanted).
                if segment == "fn" {
                    self.diags
                        .push(Diag::FnNameDoubledPrefix { span: segment_span });
                    self.recover_to_statement_end();
                    return None;
                }
                segments.push(segment);
            }
            Ok(None) => {
                self.diags.push(Diag::InvalidFunctionSignature {
                    span: Span::point(self.scan.offset()),
                    message: "expected the function name after `fn` — the path \
                              after `fn::`, `::`-joined"
                        .to_string(),
                });
                self.recover_to_statement_end();
                return None;
            }
            Err(diag) => {
                self.diags.push(diag);
                self.recover_to_statement_end();
                return None;
            }
        }
        loop {
            self.scan.skip_surrealql_trivia();
            if !(self.scan.eat_byte(b':') && self.scan.eat_byte(b':')) {
                break;
            }
            self.scan.skip_surrealql_trivia();
            match self.scan.read_ident() {
                Ok(Some((_, segment, _))) => segments.push(segment),
                Ok(None) => {
                    self.diags.push(Diag::InvalidFunctionSignature {
                        span: Span::point(self.scan.offset()),
                        message: "expected a path segment after `::`".to_string(),
                    });
                    self.recover_to_statement_end();
                    return None;
                }
                Err(diag) => {
                    self.diags.push(diag);
                    self.recover_to_statement_end();
                    return None;
                }
            }
        }
        let name = FnPath {
            kind: FnKind::Fn,
            segments,
        };
        // The parameter list rides one depth-0 `)` capture: kinds
        // hold commas INSIDE `<>` (`array<float, 10>`) that no brace
        // depth tracks, so a textual item split would cut them — the
        // list parses from the capture text where the kind reader
        // knows where each kind ends.
        self.scan.skip_surrealql_trivia();
        if self.scan.peek_byte() != Some(b'(') {
            self.diags.push(Diag::InvalidFunctionSignature {
                span: Span::point(self.scan.offset()),
                message: "expected `(` to open the parameter list".to_string(),
            });
            self.recover_to_statement_end();
            return None;
        }
        let params_span = match self.scan.capture_balanced_parens() {
            Ok(span) => span,
            Err(diag) => {
                self.diags.push(diag);
                return None;
            }
        };
        let Some(args) = self.parse_function_param_list(params_span) else {
            // The list's failure sites scan capture text, not the
            // live cursor; recovery rides the statement scanner.
            self.recover_to_statement_end();
            return None;
        };
        // Optional `-> <kind>`: the kind reads through
        // [`Self::capture_incremental_kind`] — the shared
        // capture-and-fold loop (brace-bearing sub-kinds like
        // `-> array<{ x: int }>` fold into the candidate). A kind
        // complete at `{` means that brace was the body (consumed);
        // a kind complete anywhere else leaves the body missing.
        let mut returns: Option<Kind> = None;
        // The body capture, when the `->` loop finds and takes it; the
        // no-return-type path captures below.
        let mut body_from_arrow: Option<Span> = None;
        self.scan.skip_surrealql_trivia();
        if self.scan.peek_byte() == Some(b'-') {
            self.scan.eat_byte(b'-');
            if !self.scan.eat_byte(b'>') {
                self.diags.push(Diag::InvalidFunctionSignature {
                    span: Span::point(self.scan.offset()),
                    message: "expected `>` to finish the `->` arrow".to_string(),
                });
                self.recover_to_statement_end();
                return None;
            }
            let (kind, terminator) = match self.capture_incremental_kind(b"{;") {
                Ok(found) => found,
                Err(_) => return None,
            };
            returns = Some(Kind::new(kind));
            match terminator {
                Some(b'{') => match self.scan.capture_balanced_braces_open_eaten() {
                    Ok(span) => body_from_arrow = Some(span),
                    Err(diag) => {
                        self.diags.push(diag);
                        return None;
                    }
                },
                _ => {
                    self.diags.push(Diag::InvalidFunctionSignature {
                        span: Span::point(self.scan.offset()),
                        message: "expected `{` to open the body block after the \
                                  return kind"
                            .to_string(),
                    });
                    return None;
                }
            }
        }
        // The body block — required (SurrealQL functions always carry
        // one); the `->` loop captured it when a return type was read.
        let body = if let Some(span) = body_from_arrow {
            span
        } else if self.scan.peek_byte() == Some(b'{') {
            match self.scan.capture_balanced_braces() {
                Ok(span) => span,
                Err(diag) => {
                    self.diags.push(diag);
                    return None;
                }
            }
        } else {
            self.diags.push(Diag::InvalidFunctionSignature {
                span: Span::point(self.scan.offset()),
                message: "expected `{` to open the body block".to_string(),
            });
            self.recover_to_statement_end();
            return None;
        };
        // Canon holds the whole block (braces included) — the slot's
        // wrapper feeds the braced text to the embedded parser and
        // slices its block print back out.
        let braced = Span::new(body.offset - 1, body.len + 2);
        let Some(body) = self.canonicalize_capture(braced, Slot::FunctionBody) else {
            self.recover_to_statement_end();
            return None;
        };
        // The standard statement tail; the optional second block is
        // the ordinary SDL clause loop.
        let mut permissions: Option<Permission> = None;
        let mut graphql: Option<Graphql> = None;
        let clause_comments;
        match self.parse_stmt_block("function", FUNCTION_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                close_note,
                ..
            }) => {
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.value, clause.name) {
                        (ClauseValue::Permission(p), "permissions") => permissions = Some(p),
                        (ClauseValue::Sub(sub), "graphql") => {
                            graphql = Some(graphql_of(&sub));
                        }
                        (ClauseValue::Failed, _) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
                // A block with neither side present emits nothing —
                // fold it to absent.
                if matches!(&graphql, Some(g) if g.alias.is_none() && g.deprecated.is_none()) {
                    graphql = None;
                }
            }
            // Bare form at EOF: the file's final statement.
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        Some(Stmt::Function(Function {
            name,
            args,
            returns,
            body,
            permissions,
            graphql,
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// `raw { … };` (README rule 8) — the escape hatch: SurrealQL DDL
    /// captured brace-balanced under the full rule-3 skip set and
    /// carried verbatim (comments and spelling included; only the
    /// edges trim). No header, no clauses, no bare form: the braces
    /// ARE the grammar. Top-level only — in block context `raw` is a
    /// reserved word (the clause loop's field-head detection excludes
    /// it; [`Diag::UnknownClause`]'s `raw` help says so). A `///`
    /// block above is rejected while the statement still parses (rule
    /// 5, like `config`): raw has no COMMENT clause to hang it on,
    /// and silently dropping it would lose the user's text.
    fn parse_raw(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        if let Some(doc) = pending_doc {
            self.diags.push(Diag::DocCommentUnsupported {
                span: doc.span,
                statement: "raw",
            });
        }
        self.take_comments();
        let comments = self.take_pending_comments();
        if self.scan.peek_byte() != Some(b'{') {
            self.diags.push(Diag::RawBodyExpected {
                span: Span::point(self.scan.offset()),
                found: self.cursor_char(),
            });
            self.recover_to_statement_end();
            return None;
        }
        let body_span = match self.scan.capture_balanced_braces() {
            Ok(span) => span,
            // Unterminated/unbalanced captures already rest the cursor
            // at EOF (or on the unconsumed closer, which the driver's
            // illegal-character path handles) — no recovery to add.
            Err(diag) => {
                self.diags.push(diag);
                return None;
            }
        };
        let body = body_span.slice(self.src).trim().to_string();
        self.check_block_terminator();
        // The close note joins the body's closing `};` line (the
        // empty-path anchor — `raw` has no clauses to anchor).
        let close_note = self.peek_same_line_note(self.cursor_line());
        Some(Stmt::Raw(Raw {
            body,
            comments,
            clause_comments: collect_clause_comments(&[], close_note),
        }))
    }

    /// `include "<path>";` (spec/include.md) — file composition. Only
    /// the composable entry keeps the statement: the strict entry
    /// parses it (so the literal's syntax is validated and recovery
    /// positions the driver) but rejects and drops it — a context
    /// that cannot resolve compositions must not compile as if the
    /// include were absent. No doc comment (no DDL COMMENT target,
    /// rule 5, like `mixin`) and no block: one string literal, both
    /// quote styles, then the terminator. The statement is legal
    /// anywhere in the file (the declarative namespace: it declares
    /// participation, not an elaboration point — the splice lands at
    /// its position); only the scope direction stays order-bound (a
    /// `namespace`/`database` after an include is E1069, the
    /// dispatch's gate).
    fn parse_include(&mut self, head_span: Span, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        self.saw_include = true;
        if let Some(doc) = pending_doc {
            self.diags.push(Diag::DocCommentUnsupported {
                span: doc.span,
                statement: "include",
            });
        }
        if !self.composable {
            self.diags
                .push(Diag::IncludeUnsupported { span: head_span });
        }
        // The collecting trivia skip: a `//` line between the head and
        // the literal is legal input and anchors with the statement's
        // leading comments (rule 5) — a bare skip would drop it.
        self.take_comments();
        let comments = self.take_pending_comments();
        let text = &self.src[self.scan.offset()..];
        let Some((path, rest)) = lex_string_literal(text) else {
            self.diags.push(Diag::InvalidString {
                span: Span::point(self.scan.offset()),
                text: text.chars().take(16).collect(),
            });
            self.recover_to_statement_end();
            return None;
        };
        self.scan
            .seek(self.scan.offset() + (text.len() - rest.len()));
        self.check_block_terminator();
        // The close note joins the statement's own line (the
        // empty-path anchor — `include` has no clauses to anchor).
        let close_note = self.peek_same_line_note(self.cursor_line());
        Some(Stmt::Include(Include {
            path,
            span: head_span,
            comments,
            clause_comments: collect_clause_comments(&[], close_note),
        }))
    }

    /// `config <variant> { … };` (spec/config.md) — the variant IS
    /// the header (README rule 1's variant slot with no name to fall
    /// back to), exactly one of `api | graphql | default`. `config`
    /// has no `COMMENT` clause on any variant, so a `///` block is
    /// rejected while the statement still parses (rule 5, like
    /// `sequence`).
    fn parse_config(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        if let Some(doc) = pending_doc {
            self.diags.push(Diag::DocCommentUnsupported {
                span: doc.span,
                statement: "config",
            });
        }
        self.take_comments();
        let (variant_span, variant, quoted) = match self.scan.read_ident() {
            Ok(Some(ident)) => ident,
            Ok(None) => {
                self.diags.push(Diag::UnknownVariant {
                    span: Span::point(self.scan.offset()),
                    word: String::new(),
                    suggestion: None,
                });
                self.recover_to_statement_end();
                return None;
            }
            Err(diag) => {
                self.diags.push(diag);
                self.recover_to_statement_end();
                return None;
            }
        };
        // The variant is a BARE keyword — a quoted spelling is an
        // identifier (rule 9), and the header takes no name besides.
        if quoted {
            self.diags.push(Diag::UnknownVariant {
                span: variant_span,
                word: variant,
                suggestion: None,
            });
            self.recover_to_statement_end();
            return None;
        }
        match variant.as_str() {
            "api" => self.parse_config_api(),
            "graphql" => self.parse_config_graphql(),
            "default" => self.parse_config_default(),
            _ => {
                let suggestion = did_you_mean(&variant, CONFIG_VARIANTS);
                self.diags.push(Diag::UnknownVariant {
                    span: variant_span,
                    word: variant,
                    suggestion,
                });
                self.recover_to_statement_end();
                None
            }
        }
    }

    /// `config api { middleware: …; permissions: …; };` — both
    /// clauses optional, free-ordered (rule 7); the bare form
    /// `config api;` is the empty block (SurrealQL accepts a bare
    /// `DEFINE CONFIG … API;` — the parse default FULL applies).
    fn parse_config_api(&mut self) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let mut middleware = None;
        let mut permissions = None;
        let clause_comments;
        match self.parse_stmt_block("config api", CONFIG_API_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                close_note,
                ..
            }) => {
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.name, clause.value) {
                        ("middleware", ClauseValue::Middleware(list)) => middleware = Some(list),
                        ("permissions", ClauseValue::Permission(p)) => permissions = Some(p),
                        (_, ClauseValue::Failed) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        let trailing_comments = self.take_pending_comments();
        Some(Stmt::Config(Config::Api(ApiConfig {
            middleware: middleware.unwrap_or_default(),
            permissions,
            comments,
            trailing_comments,
            clause_comments,
        })))
    }

    /// `config graphql { … };` — the families resolve at parse: the
    /// bare `auto;`/`none;` flags write BOTH families, and every
    /// write (flag, `:` form, sub-block form) folds last-one-wins per
    /// family (rule 7), exactly upstream's positional overwrite.
    fn parse_config_graphql(&mut self) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let mut config = GraphqlConfig::default();
        let clause_comments;
        match self.parse_stmt_block("config graphql", CONFIG_GRAPHQL_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                close_note,
                ..
            }) => {
                // The bare `auto;`/`none;` flags fold BOTH families;
                // their notes re-key to the first-printed family
                // (`tables:`) — everything routes through the merge
                // helper so no path ever lands twice.
                let mut anchors = Vec::new();
                for entry in collect_clause_comments(&clauses, close_note) {
                    let path = if entry.path == "auto" || entry.path == "none" {
                        "tables"
                    } else {
                        entry.path.as_str()
                    };
                    push_clause_anchor(&mut anchors, path, &entry.before, &entry.after);
                }
                clause_comments = anchors;
                for clause in clauses {
                    match (clause.name, clause.value) {
                        ("auto", ClauseValue::Flag) => {
                            config.tables = Family::Auto;
                            config.functions = Family::Auto;
                        }
                        ("none", ClauseValue::Flag) => {
                            config.tables = Family::None;
                            config.functions = Family::None;
                        }
                        ("tables", ClauseValue::Word(word)) => {
                            config.tables = graphql_family_word(&word);
                        }
                        ("functions", ClauseValue::Word(word)) => {
                            config.functions = graphql_family_word(&word);
                        }
                        ("tables", ClauseValue::Sub(sub)) => {
                            if let Some(family) =
                                self.parse_graphql_family(clause.span, "tables", &sub, |value| {
                                    match value {
                                        ClauseValue::IdentList(list) => Some(list.clone()),
                                        _ => None,
                                    }
                                })
                            {
                                config.tables = family;
                            }
                        }
                        ("functions", ClauseValue::Sub(sub)) => {
                            if let Some(family) =
                                self.parse_graphql_family(clause.span, "functions", &sub, |value| {
                                    match value {
                                        ClauseValue::FnPathList(list) => Some(list.clone()),
                                        _ => None,
                                    }
                                })
                            {
                                config.functions = family;
                            }
                        }
                        ("depth", ClauseValue::Number32(depth)) => config.depth = Some(depth),
                        ("complexity", ClauseValue::Number32(limit)) => {
                            config.complexity = Some(limit)
                        }
                        ("introspection", ClauseValue::Word(word)) => {
                            config.introspection = if word == "none" {
                                Introspection::None
                            } else {
                                Introspection::Auto
                            };
                        }
                        (_, ClauseValue::Failed) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        config.comments = comments;
        config.trailing_comments = self.take_pending_comments();
        config.clause_comments = clause_comments;
        Some(Stmt::Config(Config::Graphql(config)))
    }

    /// One graphql family sub-block (`tables { … };` /
    /// `functions { … };`): exactly ONE of its two clauses must be
    /// present — both is [`Diag::MutuallyExclusive`] on the later
    /// one, an empty block is the form error naming both spellings;
    /// a failed list attempt was already diagnosed and leaves the
    /// clause folded-to-default. `list_of` pulls the entries out of
    /// the sub-clause value (idents for tables, fn paths for
    /// functions).
    fn parse_graphql_family<T>(
        &mut self,
        key_span: Span,
        key: &'static str,
        sub: &[ParsedClause],
        list_of: impl Fn(&ClauseValue) -> Option<Vec<T>>,
    ) -> Option<Family<T>> {
        let mut items: Option<(bool, Vec<T>)> = None;
        let mut attempted = false;
        for clause in sub {
            match list_of(&clause.value) {
                Some(list) => {
                    let is_include = clause.name == "include";
                    if let Some((seen_include, _)) = &items
                        && *seen_include != is_include
                    {
                        self.diags.push(Diag::MutuallyExclusive {
                            span: clause.span,
                            clause: if is_include { "include" } else { "exclude" },
                            other: if is_include { "exclude" } else { "include" },
                        });
                        return None;
                    }
                    items = Some((is_include, list));
                }
                None => attempted = true,
            }
        }
        match items {
            Some((is_include, list)) => Some(if is_include {
                Family::Include(list)
            } else {
                Family::Exclude(list)
            }),
            None if attempted => None,
            None => {
                self.diags.push(Diag::UnexpectedClauseForm {
                    span: key_span,
                    message: format!(
                        "the `{key}` block holds exactly one of \
                         `{key} {{ include: …; }};` or `{key} {{ exclude: …; }};`"
                    ),
                });
                None
            }
        }
    }

    /// `config default { namespace: …; database: …; };` — at least
    /// one side required (upstream rejects a bare `DEFAULT`); the
    /// unset side emits `NONE` at codegen. Repeats fold
    /// last-one-wins like any clause.
    fn parse_config_default(&mut self) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let mut namespace = None;
        let mut database = None;
        let mut side_attempted = false;
        let missing_span = Span::point(self.scan.offset());
        let clause_comments;
        match self.parse_stmt_block("config default", CONFIG_DEFAULT_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                close_note,
                ..
            }) => {
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.name, clause.value) {
                        ("namespace", ClauseValue::Expr(expr)) => namespace = Some(expr),
                        ("database", ClauseValue::Expr(expr)) => database = Some(expr),
                        ("namespace", ClauseValue::Failed) | ("database", ClauseValue::Failed) => {
                            side_attempted = true
                        }
                        (_, ClauseValue::Failed) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        // Both sides absent = nothing to keep (the requirement, and
        // `NONE NONE` is not a definition). Nothing attempted gets
        // the missing-clause diagnostic; a failed attempt on either
        // side already carried its own, so the statement just drops —
        // module's required-clause discipline.
        if namespace.is_none() && database.is_none() {
            if !side_attempted {
                self.diags.push(Diag::RequiredClauseMissing {
                    span: missing_span,
                    clause: "namespace",
                    block: "config default",
                });
            }
            return None;
        }
        let trailing_comments = self.take_pending_comments();
        Some(Stmt::Config(Config::Default(DefaultConfig {
            namespace,
            database,
            comments,
            trailing_comments,
            clause_comments,
        })))
    }

    /// `index [unique | count | fulltext | hnsw | diskann] <name> { …
    /// };` (spec/index.md) — the header's OPTIONAL variant slot: a
    /// bare word from the fixed set opens it and the name follows; a
    /// bare word outside the set is the name of a plain index (rule
    /// 6: the default kind is expressed by omitting the slot); a
    /// quoted spelling is always a name (rule 9 — a misspelled
    /// variant like `index unikue` therefore reads as a legal plain
    /// index, the grammar's own unambiguous reading). Standalone form
    /// only: `on:` is required for every kind; the nested form (a
    /// `table` block head) takes the host table instead, a mixin
    /// member the placeholder expansion overwrites. Rule 10 puts
    /// each kind's clauses in its own table and the fold-site
    /// validation carries the arity/domain rules.
    fn parse_index(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        self.parse_index_inner(pending_doc, FieldHost::Standalone, false)
    }

    fn parse_index_inner(
        &mut self,
        pending_doc: Option<DocBlock>,
        host: FieldHost<'_>,
        nested: bool,
    ) -> Option<Stmt> {
        let comment = pending_doc.map(|doc| doc.comment);
        self.take_comments();
        let variant = match self.scan.read_ident() {
            Ok(Some((_, word, false))) if INDEX_VARIANTS.contains(&word.as_str()) => {
                // The slot opens; the name is required after it
                // (`index hnsw { … };` reads as a missing name).
                let name = self.read_statement_name_in_block(nested)?;
                return match word.as_str() {
                    "unique" => self.parse_index_scalar(
                        IndexKind::Unique,
                        "index unique",
                        name,
                        comment,
                        host,
                    ),
                    "count" => self.parse_index_count(name, comment, host),
                    "fulltext" => self.parse_index_fulltext(name, comment, host),
                    "hnsw" => self.parse_index_hnsw(name, comment, host),
                    _ => self.parse_index_diskann(name, comment, host),
                };
            }
            Ok(Some((span, word, false))) => {
                // A bare non-variant word: the name of a plain index —
                // the name position's rules apply (rule 9).
                if is_reserved(&word) {
                    self.diags.push(Diag::ReservedIdent { span, word });
                    self.recover_name_failure(nested);
                    return None;
                }
                word
            }
            // A quoted spelling is an identifier, never a keyword
            // (rule 9): the name of a plain index.
            Ok(Some((_, word, true))) => word,
            Ok(None) => {
                self.diags.push(Diag::ExpectedName {
                    span: Span::point(self.scan.offset()),
                });
                self.recover_name_failure(nested);
                return None;
            }
            Err(diag) => {
                self.diags.push(diag);
                self.recover_name_failure(nested);
                return None;
            }
        };
        self.parse_index_scalar(
            IndexKind::Plain,
            "index",
            Ident::new(variant),
            comment,
            host,
        )
    }

    /// The shared `index` kind validation, one home for the rules
    /// every kind repeats: the shared `on:` fold ([`Self::fold_on`] —
    /// standalone requires it, a table block's host supplies it, a
    /// mixin member holds the placeholder) and `fields` per the
    /// kind's [`FieldsPolicy`] (exactly-one is upstream's
    /// `Expected one column` check). A failed attempt already carried
    /// its own diagnostic, so the missing-clause reports are
    /// suppressed for it (one root cause, one diagnostic). Returns
    /// the shared parts of the built [`Index`], `None` = the
    /// statement is dead.
    fn finish_index_shared(
        &mut self,
        shared: IndexShared,
        label: &'static str,
        missing_span: Span,
        fields_policy: FieldsPolicy,
        host: FieldHost<'_>,
    ) -> Option<(Ident, Carried, Vec<Expr>, bool)> {
        let IndexShared {
            on,
            on_span,
            on_attempted,
            fields,
            fields_key,
            fields_attempted,
            concurrently,
            clause_comments: _,
        } = shared;
        let on = self.fold_on(
            "index",
            host,
            OnClause {
                captured: on,
                explicit_span: on_span,
                attempted: on_attempted,
                missing_span,
            },
            label,
        );
        if matches!(fields_policy, FieldsPolicy::Forbidden) {
            // The kind takes no fields (its table omits the clause):
            // nothing to require or count.
            return on.map(|on| (on, fields_key, Vec::new(), concurrently));
        }
        let mut arity_ok = true;
        match (&fields, fields_attempted) {
            (None, false) => {
                self.diags.push(Diag::RequiredClauseMissing {
                    span: missing_span,
                    clause: "fields",
                    block: label,
                });
            }
            (Some((span, items)), _) if fields_policy.is_exactly_one() && items.len() != 1 => {
                arity_ok = false;
                self.diags.push(Diag::InvalidIndexFields {
                    span: *span,
                    message: format!(
                        "a {} index takes exactly one field — found {}",
                        label.strip_prefix("index ").unwrap_or(label),
                        items.len()
                    ),
                });
            }
            _ => {}
        }
        if !arity_ok {
            return None;
        }
        match (on, fields) {
            (Some(on), Some((_, fields))) => Some((on, fields_key, fields, concurrently)),
            _ => None,
        }
    }

    /// The plain and `unique` kinds — one shared vocabulary (spec/
    /// index.md): `fields` required with at least one item, `on`
    /// required, `concurrently` optional. The kind and the diagnostic
    /// label ride in from [`Self::parse_index`].
    fn parse_index_scalar(
        &mut self,
        kind: IndexKind,
        label: &'static str,
        name: Ident,
        comment: Option<DocComment>,
        host: FieldHost<'_>,
    ) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let mut shared = IndexShared::new();
        let mut missing_span = Span::point(self.scan.offset());
        match self.parse_stmt_block(label, INDEX_PLAIN_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                brace,
                close_note,
            }) => {
                missing_span = brace;
                shared.clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    if let Err(clause) = fold_index_shared(&mut shared, clause) {
                        unreachable!(
                            "the plain/unique table holds shared clauses only: {}",
                            clause.name
                        );
                    }
                }
            }
            // Bare form at EOF: both required clauses are missing.
            Some(StmtTail::Bare { close_note }) => {
                shared.clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        let clause_comments = std::mem::take(&mut shared.clause_comments);
        let (on, fields_key, fields, concurrently) =
            self.finish_index_shared(shared, label, missing_span, FieldsPolicy::Required, host)?;
        Some(Stmt::Index(Index {
            name,
            kind,
            on,
            fields,
            fields_key,
            concurrently,
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// `index count <name> { … };` — `where` optional, `on` required;
    /// both field spellings are absent from the table, so a `fields:`
    /// attempt diagnoses as the unknown clause it is for this kind.
    fn parse_index_count(
        &mut self,
        name: Ident,
        comment: Option<DocComment>,
        host: FieldHost<'_>,
    ) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let mut shared = IndexShared::new();
        let mut where_: Option<Expr> = None;
        let mut missing_span = Span::point(self.scan.offset());
        match self.parse_stmt_block("index count", INDEX_COUNT_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                brace,
                close_note,
            }) => {
                missing_span = brace;
                shared.clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    if let Err(clause) = fold_index_shared(&mut shared, clause) {
                        match (clause.name, clause.value) {
                            ("where", ClauseValue::Expr(expr)) => where_ = Some(expr),
                            // A failed capture was already diagnosed.
                            (_, ClauseValue::Failed) => {}
                            _ => unreachable!("clause_loop yields table keys only"),
                        }
                    }
                }
            }
            Some(StmtTail::Bare { close_note }) => {
                shared.clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        // No field clauses exist for this kind: the shared fields
        // check runs with the requirement OFF (`count` forbids them,
        // and its table already rejects the spelling).
        let clause_comments = std::mem::take(&mut shared.clause_comments);
        let (on, _, _, concurrently) = self.finish_index_shared(
            shared,
            "index count",
            missing_span,
            FieldsPolicy::Forbidden,
            host,
        )?;
        Some(Stmt::Index(Index {
            name,
            kind: IndexKind::Count { where_ },
            on,
            fields: Vec::new(),
            fields_key: Carried::default(),
            concurrently,
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// `index fulltext <name> { … };` — exactly one field required
    /// (upstream's `Expected one column` check); the tuning clauses
    /// optional (defaults materialize server-side and elide at
    /// emission, rule 9).
    fn parse_index_fulltext(
        &mut self,
        name: Ident,
        comment: Option<DocComment>,
        host: FieldHost<'_>,
    ) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let mut shared = IndexShared::new();
        let mut analyzer: Option<Ident> = None;
        let mut bm25: Option<Bm25> = None;
        let mut highlights = false;
        let mut missing_span = Span::point(self.scan.offset());
        match self.parse_stmt_block("index fulltext", INDEX_FULLTEXT_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                brace,
                close_note,
            }) => {
                missing_span = brace;
                shared.clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    if let Err(clause) = fold_index_shared(&mut shared, clause) {
                        match (clause.name, clause.value) {
                            ("analyzer", ClauseValue::Ident(az)) => analyzer = Some(az),
                            ("bm25", ClauseValue::Bm25(pair)) => bm25 = Some(pair),
                            ("highlights", ClauseValue::Flag) => highlights = true,
                            (_, ClauseValue::Failed) => {}
                            _ => unreachable!("clause_loop yields table keys only"),
                        }
                    }
                }
            }
            Some(StmtTail::Bare { close_note }) => {
                shared.clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        let clause_comments = std::mem::take(&mut shared.clause_comments);
        let (on, fields_key, fields, concurrently) = self.finish_index_shared(
            shared,
            "index fulltext",
            missing_span,
            FieldsPolicy::ExactlyOne,
            host,
        )?;
        Some(Stmt::Index(Index {
            name,
            kind: IndexKind::FullText {
                analyzer,
                bm25,
                highlights,
            },
            on,
            fields,
            fields_key,
            concurrently,
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// `index hnsw <name> { … };` — `dimension` required, exactly one
    /// field; the numeric domains are upstream's own post-parse
    /// checks (`m` ≤ 127 with its message, `m0` within u8), applied
    /// at the fold. The unrestricted distance/type sets are the kind's
    /// whole vocabulary here — DISKANN's restrictions are its own.
    fn parse_index_hnsw(
        &mut self,
        name: Ident,
        comment: Option<DocComment>,
        host: FieldHost<'_>,
    ) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let mut shared = IndexShared::new();
        let mut dimension: Option<u16> = None;
        let mut dimension_attempted = false;
        let mut distance: Option<Distance> = None;
        let mut vector_type: Option<VectorType> = None;
        let mut m: Option<(Span, u16)> = None;
        let mut m0: Option<(Span, u16)> = None;
        let mut lm: Option<Float> = None;
        let mut efc: Option<u16> = None;
        let mut extend_candidates = false;
        let mut keep_pruned_connections = false;
        let mut hashed_vector = false;
        let mut missing_span = Span::point(self.scan.offset());
        match self.parse_stmt_block("index hnsw", INDEX_HNSW_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                brace,
                close_note,
            }) => {
                missing_span = brace;
                shared.clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    if let Err(clause) = fold_index_shared(&mut shared, clause) {
                        match (clause.name, clause.value) {
                            ("dimension", ClauseValue::Number(value)) => dimension = Some(value),
                            ("dimension", ClauseValue::Failed) => dimension_attempted = true,
                            ("distance", ClauseValue::Distance(value)) => distance = Some(value),
                            ("type", ClauseValue::Word(word)) => {
                                vector_type = Some(vector_type_of(&word))
                            }
                            ("m", ClauseValue::Number(value)) => m = Some((clause.span, value)),
                            ("m0", ClauseValue::Number(value)) => m0 = Some((clause.span, value)),
                            ("lm", ClauseValue::Float(value)) => lm = Some(value),
                            ("efc", ClauseValue::Number(value)) => efc = Some(value),
                            ("extend_candidates", ClauseValue::Flag) => extend_candidates = true,
                            ("keep_pruned_connections", ClauseValue::Flag) => {
                                keep_pruned_connections = true
                            }
                            ("hashed_vector", ClauseValue::Flag) => hashed_vector = true,
                            (_, ClauseValue::Failed) => {}
                            _ => unreachable!("clause_loop yields table keys only"),
                        }
                    }
                }
            }
            Some(StmtTail::Bare { close_note }) => {
                shared.clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        let clause_comments = std::mem::take(&mut shared.clause_comments);
        let (on, fields_key, fields, concurrently) = self.finish_index_shared(
            shared,
            "index hnsw",
            missing_span,
            FieldsPolicy::ExactlyOne,
            host,
        )?;
        let dimension =
            self.require_dimension(dimension, dimension_attempted, missing_span, "index hnsw")?;
        if let Some((span, value)) = m
            && value > 127
        {
            self.diags.push(Diag::InvalidIndexParam {
                span,
                message: "HNSW parameter `m` cannot be larger than 127".to_string(),
            });
            return None;
        }
        if let Some((span, value)) = m0
            && value > u8::MAX as u16
        {
            self.diags.push(Diag::InvalidIndexParam {
                span,
                message: format!("HNSW parameter `m0` must fit u8 — 0–{}", u8::MAX),
            });
            return None;
        }
        Some(Stmt::Index(Index {
            name,
            kind: IndexKind::Hnsw {
                dimension,
                distance,
                vector_type,
                m: m.map(|(_, value)| value),
                m0: m0.map(|(_, value)| value),
                lm,
                efc,
                extend_candidates,
                keep_pruned_connections,
                hashed_vector,
            },
            on,
            fields,
            fields_key,
            concurrently,
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// `index diskann <name> { … };` — `dimension` required, exactly
    /// one field; DISKANN's restricted distance/type domains, the
    /// `cosine_normalized` type combo, and `degree`/`l_build` > 0 are
    /// upstream's post-parse checks, applied at the fold in upstream's
    /// own order (distance → type → combo → degree → l_build).
    fn parse_index_diskann(
        &mut self,
        name: Ident,
        comment: Option<DocComment>,
        host: FieldHost<'_>,
    ) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let mut shared = IndexShared::new();
        let mut dimension: Option<u16> = None;
        let mut dimension_attempted = false;
        let mut distance: Option<(Span, Distance)> = None;
        let mut vector_type: Option<(Span, VectorType)> = None;
        let mut degree: Option<(Span, u16)> = None;
        let mut l_build: Option<(Span, u16)> = None;
        let mut alpha: Option<Float> = None;
        let mut hashed_vector = false;
        let mut missing_span = Span::point(self.scan.offset());
        match self.parse_stmt_block("index diskann", INDEX_DISKANN_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                brace,
                close_note,
            }) => {
                missing_span = brace;
                shared.clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    if let Err(clause) = fold_index_shared(&mut shared, clause) {
                        match (clause.name, clause.value) {
                            ("dimension", ClauseValue::Number(value)) => dimension = Some(value),
                            ("dimension", ClauseValue::Failed) => dimension_attempted = true,
                            ("distance", ClauseValue::Distance(value)) => {
                                distance = Some((clause.span, value))
                            }
                            ("type", ClauseValue::Word(word)) => {
                                vector_type = Some((clause.span, vector_type_of(&word)))
                            }
                            ("degree", ClauseValue::Number(value)) => {
                                degree = Some((clause.span, value))
                            }
                            ("l_build", ClauseValue::Number(value)) => {
                                l_build = Some((clause.span, value))
                            }
                            ("alpha", ClauseValue::Float(value)) => alpha = Some(value),
                            ("hashed_vector", ClauseValue::Flag) => hashed_vector = true,
                            (_, ClauseValue::Failed) => {}
                            _ => unreachable!("clause_loop yields table keys only"),
                        }
                    }
                }
            }
            Some(StmtTail::Bare { close_note }) => {
                shared.clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        let clause_comments = std::mem::take(&mut shared.clause_comments);
        let (on, fields_key, fields, concurrently) = self.finish_index_shared(
            shared,
            "index diskann",
            missing_span,
            FieldsPolicy::ExactlyOne,
            host,
        )?;
        let dimension = self.require_dimension(
            dimension,
            dimension_attempted,
            missing_span,
            "index diskann",
        )?;
        // DISKANN's domain checks, upstream's own order and messages.
        if let Some((span, value)) = distance
            && !matches!(
                value,
                Distance::Euclidean
                    | Distance::Cosine
                    | Distance::InnerProduct
                    | Distance::CosineNormalized
            )
        {
            self.diags.push(Diag::InvalidIndexParam {
                span,
                message: "DISKANN supports EUCLIDEAN, COSINE, INNER_PRODUCT, and \
                          COSINE_NORMALIZED distances"
                    .to_string(),
            });
            return None;
        }
        if let Some((span, value)) = vector_type
            && !matches!(
                value,
                VectorType::F32 | VectorType::F16 | VectorType::I8 | VectorType::U8
            )
        {
            self.diags.push(Diag::InvalidIndexParam {
                span,
                message: "DISKANN supports F32, F16, I8, and U8 vector types".to_string(),
            });
            return None;
        }
        if let (Some((_, Distance::CosineNormalized)), Some((span, kind))) = (distance, vector_type)
            && matches!(kind, VectorType::I8 | VectorType::U8)
        {
            self.diags.push(Diag::InvalidIndexParam {
                span,
                message: "the COSINE_NORMALIZED distance supports F32 and F16 types".to_string(),
            });
            return None;
        }
        if let Some((span, value)) = degree
            && value == 0
        {
            self.diags.push(Diag::InvalidIndexParam {
                span,
                message: "`degree` must be greater than 0".to_string(),
            });
            return None;
        }
        if let Some((span, value)) = l_build
            && value == 0
        {
            self.diags.push(Diag::InvalidIndexParam {
                span,
                message: "`l_build` must be greater than 0".to_string(),
            });
            return None;
        }
        Some(Stmt::Index(Index {
            name,
            kind: IndexKind::DiskAnn {
                dimension,
                distance: distance.map(|(_, value)| value),
                vector_type: vector_type.map(|(_, value)| value),
                degree: degree.map(|(_, value)| value),
                l_build: l_build.map(|(_, value)| value),
                alpha,
                hashed_vector,
            },
            on,
            fields,
            fields_key,
            concurrently,
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// `dimension` is required for the vector kinds; a failed attempt
    /// already carried its own diagnostic, so the missing-clause
    /// report is suppressed for it (one root cause, one diagnostic).
    fn require_dimension(
        &mut self,
        dimension: Option<u16>,
        dimension_attempted: bool,
        missing_span: Span,
        label: &'static str,
    ) -> Option<u16> {
        if dimension.is_none() && !dimension_attempted {
            self.diags.push(Diag::RequiredClauseMissing {
                span: missing_span,
                clause: "dimension",
                block: label,
            });
        }
        dimension
    }

    /// The captured parameter list of a `fn` signature:
    /// `$<name>: <kind>` items joined by `,` — empty legal, trailing
    /// comma rejected (spec/function.md: SurrealQL accepts one; SDL
    /// is deliberately stricter since INFO never emits one). The kind
    /// reader ends each item, so the split happens after it — never
    /// at a textual `,`, which kinds may contain inside `<>`.
    /// Sub-diagnostics anchor at their true file positions: `span` is
    /// the capture, `base` its file offset.
    fn parse_function_param_list(&mut self, span: Span) -> Option<Vec<(ParamName, Kind)>> {
        let text = span.slice(self.src);
        let base = span.offset;
        let mut args = Vec::new();
        let mut rest = strip_capture_trivia(text)?;
        if rest.is_empty() {
            return Some(args);
        }
        loop {
            // `$<name>` — the `$` is SurrealQL-required (verified:
            // `expected a parameter`; spec/function.md).
            let at = text.len() - rest.len();
            let Some(after_dollar) = rest.strip_prefix('$') else {
                // A comma in the name position is a stray separator,
                // not a missing `$` (`,$b` after an earlier list) —
                // steer at the comma, not the prefix.
                if rest.starts_with(',') {
                    self.diags.push(Diag::InvalidFunctionSignature {
                        span: Span::new(base + at, 1),
                        message: "expected a parameter — remove the extra `,`".to_string(),
                    });
                } else {
                    self.diags.push(Diag::FunctionParamNeedsDollar {
                        span: Span::new(base + at, word_prefix_len(rest).max(1)),
                    });
                }
                return None;
            };
            let name = match Scanner::new(after_dollar).read_ident() {
                Ok(Some((name_span, raw, _))) => {
                    rest = &after_dollar[name_span.end()..];
                    ParamName::new(raw)
                }
                // A malformed quoted spelling folds into the same
                // positional message (its own error's span is
                // capture-relative — sub-scanner coordinates).
                _ => {
                    self.diags.push(Diag::InvalidFunctionSignature {
                        span: Span::new(base + at, 1),
                        message: "expected a parameter name after `$`".to_string(),
                    });
                    return None;
                }
            };
            let after_ws = strip_capture_trivia(rest)?;
            let Some(after_colon) = after_ws.strip_prefix(':') else {
                self.diags.push(Diag::InvalidFunctionSignature {
                    span: Span::new(
                        base + (text.len() - after_ws.len()),
                        word_prefix_len(after_ws).max(1),
                    ),
                    message: "expected `:` between the parameter name and its kind".to_string(),
                });
                return None;
            };
            rest = strip_capture_trivia(after_colon)?;
            let kind_at = text.len() - rest.len();
            let Some(kind_tail) = read_kind_with_aliases(rest) else {
                self.diags.push(Diag::InvalidKind {
                    span: Span::new(base + kind_at, text.len() - kind_at),
                    text: rest.trim().to_string(),
                });
                return None;
            };
            let kind_text = &rest[..rest.len() - kind_tail.len()];
            args.push((name, Kind::new(kind_text.trim_end())));
            rest = strip_capture_trivia(kind_tail)?;
            let Some(after_comma) = rest.strip_prefix(',') else {
                if rest.is_empty() {
                    return Some(args);
                }
                self.diags.push(Diag::InvalidFunctionSignature {
                    span: Span::new(
                        base + (text.len() - rest.len()),
                        word_prefix_len(rest).max(1),
                    ),
                    message: "expected `,` between parameters or `)` to close the list".to_string(),
                });
                return None;
            };
            let comma_at = base + (text.len() - after_comma.len()) - 1;
            rest = strip_capture_trivia(after_comma)?;
            if rest.is_empty() {
                self.diags.push(Diag::FunctionParamTrailingComma {
                    span: Span::new(comma_at, 1),
                });
                return None;
            }
        }
    }

    /// `api '<path>' { … };` (spec/api.md) — the one structural-
    /// sub-block statement: the body holds method groups
    /// (`get, post { … };`) and at most one `any { … };` block, in
    /// any order, whose heads are NOT clause keys (README rule 2) —
    /// a hand-rolled body loop, each block's interior riding the
    /// shared [`Self::parse_block_interior`]. The path is one plain
    /// string literal; the decoded value is carried and the route
    /// grammar (static, `:param`, `*wildcard` segments) is the
    /// server's to validate at define time. A body with neither
    /// groups nor `any` is legal, as is the bare `api '<path>';`
    /// form (upstream parses the zero-`FOR` statement —
    /// probe-pinned). One broken group never hides its siblings: a
    /// head or list failure recovers to the group's `;` and the loop
    /// continues; only an unterminated block (EOF) kills the
    /// statement.
    fn parse_api(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        self.take_comments();
        let comments = self.take_pending_comments();
        let path = match self.scan.peek_byte() {
            Some(b'\'') | Some(b'"') => {
                let span = match self.scan.capture_string_literal() {
                    Ok(span) => span,
                    Err(diag) => {
                        self.diags.push(diag);
                        self.recover_to_statement_end();
                        return None;
                    }
                };
                match parse_string_literal_text(span.slice(self.src)) {
                    Some(value) => value,
                    None => {
                        self.diags.push(Diag::InvalidString {
                            span,
                            text: span.slice(self.src).trim().to_string(),
                        });
                        self.recover_to_statement_end();
                        return None;
                    }
                }
            }
            _ => {
                // A word (if one follows) rides the message; anything
                // else is a point diagnostic at the expected position.
                let span = self
                    .scan
                    .read_word()
                    .unwrap_or_else(|| Span::point(self.scan.offset()));
                self.diags.push(Diag::InvalidApiPath {
                    span,
                    text: span.slice(self.src).to_string(),
                });
                self.recover_to_statement_end();
                return None;
            }
        };
        self.take_comments();
        // The bare form `api '<path>';` is the empty body (the file's
        // final statement may omit the `;`, like every statement).
        if matches!(self.scan.peek_byte(), Some(b';') | None) {
            self.scan.eat_byte(b';');
            return Some(Stmt::Api(Api {
                path,
                actions: Vec::new(),
                any: None,
                comment: pending_doc.map(|doc| doc.comment),
                comments,
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }));
        }
        if self.scan.peek_byte() != Some(b'{') {
            self.diags.push(Diag::ExpectedStatementEnd {
                span: Span::point(self.scan.offset()),
                found: self.cursor_char(),
            });
            self.recover_to_statement_end();
            return None;
        }
        let body_brace = Span::new(self.scan.offset(), 1);
        self.scan.eat_byte(b'{');
        let mut actions: Vec<ApiAction> = Vec::new();
        let mut any: Option<ApiAnyBlock> = None;
        // Methods claimed by landed groups — the eager one-group rule
        // (spec/api.md; upstream errors only at define time).
        let mut seen: Vec<ApiMethod> = Vec::new();
        loop {
            self.take_comments();
            match self.scan.peek_byte() {
                Some(b'}') => {
                    self.scan.eat_byte(b'}');
                    break;
                }
                None => {
                    self.diags
                        .push(Diag::UnterminatedBlock { span: body_brace });
                    return None;
                }
                // Stray `;`s between groups are noise, like inside a
                // clause block.
                Some(b';') => {
                    self.scan.eat_byte(b';');
                    continue;
                }
                _ => {}
            }
            if self.scan.at_doc_comment() {
                // A `///` line inside the body is the misplaced clause
                // it is (rule 5 attaches docs above definitions).
                self.push_misplaced_doc_clause("api");
                self.recover_to_clause_end();
                continue;
            }
            // The head is a BARE keyword (rule 9) — a quoted spelling
            // is an identifier, not a head.
            let (head_span, head, quoted) = match self.scan.read_ident() {
                Ok(Some(ident)) => ident,
                Ok(None) => {
                    let ch = self
                        .cursor_char()
                        .expect("not at end and a head cannot start here");
                    self.diags.push(Diag::IllegalCharacter {
                        span: Span::new(self.scan.offset(), ch.len_utf8()),
                        ch,
                    });
                    self.recover_to_clause_end();
                    continue;
                }
                Err(diag) => {
                    self.diags.push(diag);
                    self.recover_to_clause_end();
                    continue;
                }
            };
            if quoted || !API_HEADS.contains(&head.as_str()) {
                let suggestion = (!quoted).then(|| did_you_mean(&head, API_HEADS)).flatten();
                self.diags.push(Diag::UnknownApiMethod {
                    span: head_span,
                    word: head,
                    suggestion,
                });
                self.recover_to_clause_end();
                continue;
            }
            if head == "any" {
                if any.is_some() {
                    self.diags.push(Diag::DuplicateAnyBlock { span: head_span });
                }
                self.take_comments();
                let any_comments = self.take_pending_comments();
                if self.scan.peek_byte() != Some(b'{') {
                    self.diags.push(Diag::UnexpectedClauseForm {
                        span: head_span,
                        message: "the `any` block takes a body — write `any { … };`".to_string(),
                    });
                    self.recover_to_clause_end();
                    continue;
                }
                let clauses = self.parse_block_interior("api any", API_ANY_CLAUSES)?;
                let close_note = self.peek_same_line_note(self.cursor_line());
                let parts = fold_api_group(&clauses);
                if any.is_none() {
                    any = Some(ApiAnyBlock {
                        config: parts.config,
                        fallback: parts.then.and_then(Result::ok),
                        comments: any_comments,
                        trailing_comments: self.take_pending_comments(),
                        clause_comments: collect_clause_comments(&clauses, close_note),
                    });
                }
                continue;
            }
            let list = match self.read_api_method_list(head_span, head, &seen) {
                Ok(list) => list,
                Err(()) => {
                    self.recover_to_clause_end();
                    continue;
                }
            };
            self.take_comments();
            let group_comments = self.take_pending_comments();
            let group_brace = Span::new(self.scan.offset(), 1);
            if self.scan.peek_byte() != Some(b'{') {
                self.diags.push(Diag::UnexpectedClauseForm {
                    span: head_span,
                    message: "a method group takes a body — write `get { … };`".to_string(),
                });
                self.recover_to_clause_end();
                continue;
            }
            let clauses = self.parse_block_interior("api group", API_GROUP_CLAUSES)?;
            let close_note = self.peek_same_line_note(self.cursor_line());
            let parts = fold_api_group(&clauses);
            let action = match parts.then {
                Some(Ok(action)) => action,
                // A failed capture carried its own diagnostic; an
                // absent one is the required-clause error, anchored
                // on the group's `{` like every block-shape anchor.
                Some(Err(())) => continue,
                None => {
                    self.diags.push(Diag::MissingApiThen { span: group_brace });
                    continue;
                }
            };
            seen.extend(list.iter().map(|(_, method)| *method));
            actions.push(ApiAction {
                methods: list.into_iter().map(|(_, method)| method).collect(),
                config: parts.config,
                action,
                comments: group_comments,
                trailing_comments: self.take_pending_comments(),
                clause_comments: collect_clause_comments(&clauses, close_note),
            });
        }
        self.check_block_terminator();
        let close_note = self.peek_same_line_note(self.cursor_line());
        let trailing_comments = self.take_pending_comments();
        Some(Stmt::Api(Api {
            path,
            actions,
            any,
            comment: pending_doc.map(|doc| doc.comment),
            comments,
            trailing_comments,
            clause_comments: collect_clause_comments(&[], close_note),
        }))
    }

    /// `access <name> { … };` (spec/access.md) — the discriminant-in-
    /// block statement: one UNION clause table feeds all three
    /// variants, the fold collects every clause into a presence bag,
    /// and the validation pass below enforces, in order — required
    /// `type:` (the discriminant is implicitly required, README
    /// grammar notation), variant membership (a clause another
    /// variant defines diagnoses; there is no per-variant table to
    /// omit it from), required `on:`, the level domains (record and
    /// bearer-for-record are database-only, upstream parser-enforced),
    /// bearer's `for:`, and the JWT source shape ([`JwtParts`]'s
    /// XOR/pair/issuer rules, shared with the `jwt { … }` sub-block).
    /// Any validation failure drops the statement (the house
    /// required-clause policy).
    fn parse_access(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let comment = pending_doc.map(|doc| doc.comment);
        let name = self.read_statement_name()?;
        self.take_comments();
        let mut on: Option<Authored<Level>> = None;
        let mut on_attempted = false;
        let mut type_word: Option<String> = None;
        let mut type_attempted = false;
        let mut type_span = Span::point(self.scan.offset());
        let mut subject: Option<BearerSubject> = None;
        let mut subject_attempted = false;
        let mut subject_span = Span::point(self.scan.offset());
        let mut signup: Option<Expr> = None;
        let mut signin: Option<Expr> = None;
        let mut refresh = false;
        let mut jwt_block: Option<(Span, JwtParts)> = None;
        let mut duration: Option<AccessDuration> = None;
        let mut authenticate: Option<Expr> = None;
        let mut context: Option<(Span, Expr)> = None;
        let mut missing_span = Span::point(self.scan.offset());
        // The jwt variant's direct source clauses and the `jwt { … }`
        // sub-block ride the same shape-fold; `seen` carries every
        // parsed clause (name, span, failed?) for the membership pass.
        // Both arms assign; the bare arm never returns.
        let direct: JwtParts;
        let seen: Vec<(&'static str, Span, bool)>;
        let clause_comments;
        match self.parse_stmt_block("access", ACCESS_CLAUSES) {
            None => return None,
            Some(StmtTail::Block {
                clauses,
                brace,
                close_note,
            }) => {
                missing_span = brace;
                clause_comments = collect_clause_comments(&clauses, close_note);
                direct = fold_jwt_parts(&clauses);
                seen = clauses
                    .iter()
                    .map(|clause| {
                        (
                            clause.name,
                            clause.span,
                            matches!(clause.value, ClauseValue::Failed),
                        )
                    })
                    .collect();
                for clause in &clauses {
                    match (clause.name, &clause.value) {
                        ("on", ClauseValue::Level(level)) => on = Some(*level),
                        ("on", ClauseValue::Failed) => on_attempted = true,
                        ("type", ClauseValue::Word(word)) => {
                            type_word = Some(word.clone());
                            type_span = clause.span;
                        }
                        ("type", ClauseValue::Failed) => type_attempted = true,
                        ("for", ClauseValue::Word(word)) => {
                            subject = Some(match word.as_str() {
                                "user" => BearerSubject::User,
                                _ => BearerSubject::Record,
                            });
                            subject_span = clause.span;
                        }
                        ("for", ClauseValue::Failed) => subject_attempted = true,
                        ("signup", ClauseValue::Expr(expr)) => signup = Some(expr.clone()),
                        ("signin", ClauseValue::Expr(expr)) => signin = Some(expr.clone()),
                        ("refresh", ClauseValue::Flag) => refresh = true,
                        ("jwt", ClauseValue::Sub(sub)) => {
                            jwt_block = Some((clause.span, fold_jwt_parts(sub)))
                        }
                        ("duration", ClauseValue::Sub(sub)) => duration = access_duration_of(sub),
                        ("authenticate", ClauseValue::Expr(expr)) => {
                            authenticate = Some(expr.clone())
                        }
                        ("context", ClauseValue::Expr(expr)) => {
                            context = Some((clause.span, expr.clone()))
                        }
                        // The jwt variant's source keys — folded by
                        // `fold_jwt_parts` above.
                        ("url" | "algorithm" | "key" | "issuer" | "audience", _) => {}
                        (_, ClauseValue::Failed) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            // Bare form at EOF: nothing required is present.
            Some(StmtTail::Bare { .. }) => {
                self.diags.push(Diag::RequiredClauseMissing {
                    span: missing_span,
                    clause: "type",
                    block: "access",
                });
                return None;
            }
        }
        // ---- validation, one pass; each failure drops the statement.
        let type_word = match type_word {
            Some(word) => word,
            None => {
                if !type_attempted {
                    self.diags.push(Diag::RequiredClauseMissing {
                        span: missing_span,
                        clause: "type",
                        block: "access",
                    });
                }
                return None;
            }
        };
        // Variant membership: the union table parses every variant's
        // clauses, so a clause the selected variant does not define
        // diagnoses here instead of folding silently (spec/access.md
        // — each variant declares its own clauses, rule 10). Failed
        // values skip — their own diagnostic already fired.
        let legal: &[&str] = match type_word.as_str() {
            "jwt" => &[
                "on",
                "type",
                "url",
                "algorithm",
                "key",
                "audience",
                "issuer",
                "duration",
                "authenticate",
            ],
            "record" => &[
                "on",
                "type",
                "signup",
                "signin",
                "jwt",
                "refresh",
                "duration",
                "authenticate",
                "context",
            ],
            _ => &[
                "on",
                "type",
                "for",
                "jwt",
                "duration",
                "authenticate",
                "context",
            ],
        };
        for (clause, span, failed) in &seen {
            if *failed || legal.contains(clause) {
                continue;
            }
            let owners: &[&str] = match *clause {
                "url" | "algorithm" | "key" | "issuer" | "audience" => &["jwt"],
                "signup" | "signin" | "refresh" => &["record"],
                "jwt" => &["record", "bearer"],
                "for" => &["bearer"],
                "context" => &["record", "bearer"],
                _ => &[],
            };
            self.diags.push(Diag::InvalidAccessClause {
                span: *span,
                message: format!(
                    "clause `{clause}` is not part of the `{type_word}` access vocabulary — \
                     it belongs to a {} access",
                    owners.join("/")
                ),
            });
        }
        let on = match on {
            Some(on) => on,
            None => {
                if !on_attempted {
                    self.diags.push(Diag::RequiredClauseMissing {
                        span: missing_span,
                        clause: "on",
                        block: "access",
                    });
                }
                return None;
            }
        };
        let kind = match type_word.as_str() {
            "jwt" => AccessKind::Jwt {
                source: self.finish_jwt_source(direct, missing_span)?,
            },
            "record" => {
                if on.value != Level::Database {
                    self.diags.push(Diag::InvalidAccessLevel {
                        span: type_span,
                        message: "`type: record` requires `on: database` (parser-enforced \
                         upstream)"
                            .to_string(),
                    });
                    return None;
                }
                let jwt = match jwt_block {
                    None => None,
                    // A `jwt { … }` block that cannot supply a source
                    // anchors its report on its own clause key.
                    Some((jwt_span, parts)) => Some(self.finish_jwt_source(parts, jwt_span)?),
                };
                AccessKind::Record {
                    signup,
                    signin,
                    jwt,
                    refresh,
                }
            }
            _ => {
                let subject = match subject {
                    Some(subject) => subject,
                    None => {
                        if !subject_attempted {
                            self.diags.push(Diag::RequiredClauseMissing {
                                span: missing_span,
                                clause: "for",
                                block: "access",
                            });
                        }
                        return None;
                    }
                };
                if subject == BearerSubject::Record && on.value != Level::Database {
                    self.diags.push(Diag::InvalidAccessLevel {
                        span: subject_span,
                        message: "`for: record` requires `on: database` (parser-enforced \
                         upstream)"
                            .to_string(),
                    });
                    return None;
                }
                let jwt = match jwt_block {
                    None => None,
                    Some((jwt_span, parts)) => Some(self.finish_jwt_source(parts, jwt_span)?),
                };
                AccessKind::Bearer { subject, jwt }
            }
        };
        // The CONTEXT contract's shape half (3.3, spec/access.md; the
        // engine's validate_access_context — each message tracks the
        // engine's): the clause requires an access method that
        // authenticates a record. The jwt spelling never does (the
        // membership pass already diagnosed it there); a bearer for
        // users doesn't either; and the database-level half rides the
        // record/bearer-for-record checks above (at root/namespace
        // nothing authenticates a record). The read-only half is
        // semantic — the engine's to say (the standing division of
        // labor).
        // The jwt spelling is the membership pass's ("belongs to a
        // record/bearer access" — one root cause, one diagnostic);
        // this is the subject/level half: a bearer for users, or a
        // root/namespace level, never authenticates a record (the
        // record and bearer-for-record level checks already fired
        // above).
        if let Some((span, _)) = &context
            && !matches!(kind, AccessKind::Jwt { .. })
        {
            let authenticates_record = matches!(
                kind,
                AccessKind::Record { .. }
                    | AccessKind::Bearer {
                        subject: BearerSubject::Record,
                        ..
                    }
            ) && on.value == Level::Database;
            if !authenticates_record {
                self.diags.push(Diag::InvalidAccessClause {
                    span: *span,
                    message: "a CONTEXT clause is evaluated with the authenticated \
                              record's own permissions, so it can only be defined on an \
                              access method that authenticates a record"
                        .to_string(),
                });
                return None;
            }
        }
        Some(Stmt::Access(Access {
            name,
            on,
            kind,
            duration,
            authenticate,
            context: context.map(|(_, expr)| expr),
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// One JWT source's shape (spec/access.md): the verification
    /// source XOR (`url:` vs `algorithm:`+`key:` — upstream's grammar
    /// is positional, so half pairs and mixes are rejected where
    /// SurrealQL itself errors) and the `issuer { … }` rules (the
    /// declared algorithm must equal a local-key verification
    /// algorithm; `key:` is required unless a symmetric local verify
    /// key auto-derives it — a key-less issuer would materialize a
    /// RANDOM key, which SDL rejects per the spec). `missing_span` is
    /// the no-source report's anchor: the statement block's `{` for
    /// the jwt variant's direct clauses, the `jwt` clause key for a
    /// record/bearer sub-block.
    fn finish_jwt_source(&mut self, parts: JwtParts, missing_span: Span) -> Option<JwtSource> {
        let JwtParts {
            url,
            url_attempted,
            algorithm,
            algorithm_attempted,
            key,
            key_attempted,
            audience,
            audience_attempted: _,
            issuer,
            issuer_attempted,
            issuer_key_attempted,
        } = parts;
        // The exposure guard's anchors: the key clause spans, captured
        // before the tuples move into the fold below.
        let key_span = key.as_ref().map(|(span, _)| *span);
        let issuer_span = issuer.as_ref().map(|(span, _)| *span);
        let url_side = url.is_some() || url_attempted;
        let key_side = algorithm.is_some() || algorithm_attempted || key.is_some() || key_attempted;
        let verify = match (url_side, key_side) {
            (false, false) => {
                self.diags.push(Diag::InvalidAccessSource {
                    span: missing_span,
                    message: "a verification source is required — write `url: <expr>;` or \
                     `algorithm: <word>;` with `key: <expr>;`"
                        .to_string(),
                });
                return None;
            }
            // Both sides present: a clean url alongside a clean
            // key-side clause is the mixed-source rejection (anchored
            // on the key-side clause); a failed side already carried
            // its own diagnostic, so nothing re-reports it. Either
            // way the statement is dead.
            (true, true) => {
                if url.is_some() {
                    match (&algorithm, &key) {
                        (Some((span, _)), _) => {
                            self.diags.push(Diag::InvalidAccessSource {
                                span: *span,
                                message: "a `url:` source takes no local key — remove \
                                 `algorithm:` (and `key:`)"
                                    .to_string(),
                            });
                        }
                        (None, Some((span, _))) => {
                            self.diags.push(Diag::InvalidAccessSource {
                                span: *span,
                                message: "a `url:` source takes no local key — remove \
                                 `key:`"
                                    .to_string(),
                            });
                        }
                        (None, None) => {}
                    }
                }
                return None;
            }
            (true, false) => match url {
                Some((_, expr)) => JwtVerify::Url(expr),
                None => return None,
            },
            (false, true) => match (algorithm, key) {
                (Some((_, algorithm)), Some((_, key))) => JwtVerify::Key { algorithm, key },
                (Some((span, _)), None) if !key_attempted => {
                    self.diags.push(Diag::InvalidAccessSource {
                        span,
                        message: "`algorithm:` needs its `key:` — verification by local key \
                         takes both"
                            .to_string(),
                    });
                    return None;
                }
                (None, Some((span, _))) if !algorithm_attempted => {
                    self.diags.push(Diag::InvalidAccessSource {
                        span,
                        message: "`key:` needs its `algorithm:` — verification by local key \
                         takes both"
                            .to_string(),
                    });
                    return None;
                }
                _ => return None,
            },
        };
        let issuer = match issuer {
            // A failed sub-block (its own diagnostic fired at the
            // parse) cannot validate — the statement is dead.
            None if issuer_attempted => return None,
            None => None,
            Some((span, issuer)) => {
                let mut valid = true;
                if let (Some(issuer_algorithm), JwtVerify::Key { algorithm, .. }) =
                    (issuer.algorithm, &verify)
                    && issuer_algorithm != *algorithm
                {
                    valid = false;
                    self.diags.push(Diag::InvalidAccessIssuer {
                        span,
                        message: format!(
                            "issuer algorithm `{}` must equal the verification algorithm \
                             `{}` when verifying by local key",
                            issuer_algorithm.as_str(),
                            algorithm.as_str()
                        ),
                    });
                }
                let mirrored =
                    matches!(&verify, JwtVerify::Key { algorithm, .. } if algorithm.is_symmetric());
                if issuer.key.is_none() && !issuer_key_attempted && !mirrored {
                    valid = false;
                    self.diags.push(Diag::InvalidAccessIssuer {
                        span,
                        message: "an `issuer` block needs `key:` — without one SurrealDB \
                         materializes a random key"
                            .to_string(),
                    });
                }
                if !valid {
                    return None;
                }
                Some(issuer)
            }
        };
        // ---- the exposure guard (spec/access.md's shape-only rule):
        // a symmetric verify key and every issuer key are secret
        // material the engine redacts in INFO — SDL only accepts the
        // `'[REDACTED]'` sentinel there (the key is provisioned
        // outside SDL; the differ never auto-emits these defines).
        // Fires only on statements that passed the
        // shape checks above: one root cause, one diagnostic.
        if let JwtVerify::Key { algorithm, key } = &verify
            && algorithm.is_symmetric()
            && key.canon != SECRET_SENTINEL_CANON
        {
            self.diags.push(Diag::AccessSecretExposure {
                span: key_span.unwrap_or(missing_span),
                message: format!(
                    "a symmetric (`{}`) verify key is secret material — write \
                     `key: '[REDACTED]';`",
                    algorithm.as_str()
                ),
            });
            return None;
        }
        if let Some(issuer) = &issuer
            && let Some(key) = &issuer.key
            && key.canon != SECRET_SENTINEL_CANON
        {
            self.diags.push(Diag::AccessSecretExposure {
                span: issuer_span.unwrap_or(missing_span),
                message: "an issuer key is secret material — write `key: '[REDACTED]';`"
                    .to_string(),
            });
            return None;
        }
        Some(JwtSource {
            verify,
            audience,
            issuer,
        })
    }

    /// `table <name> { … };` (spec/table.md) — the nesting statement.
    /// The kind is the header discriminant, read left to right with no
    /// backtracking: an arrow straight after `table` opens the
    /// relation form with no from-list; a variant word (`any`,
    /// `relation`, `view`) is a variant ONLY when followed by a name;
    /// a first word followed by `->` or `|` opens a relation
    /// from-list; anything else is the name of a bare table (which
    /// emits `TYPE NORMAL` — the fail-closed default). `view` takes
    /// the keyless query body ([`Self::parse_table_view_body`]); the
    /// other kinds share the union clause table over
    /// [`TABLE_CLAUSES`], whose nested `index`/`event` heads ride it
    /// too, with kind membership validated at the fold (access's
    /// precedent).
    fn parse_table(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        let comment = pending_doc.map(|doc| doc.comment);
        self.take_comments();
        if self.at_arrow() {
            return self.parse_table_relation(None, comment);
        }
        let (first_span, first_word, quoted) = match self.scan.read_ident() {
            Ok(Some(ident)) => ident,
            Ok(None) => {
                self.diags.push(Diag::ExpectedName {
                    span: Span::point(self.scan.offset()),
                });
                self.recover_to_statement_end();
                return None;
            }
            Err(diag) => {
                self.diags.push(diag);
                self.recover_to_statement_end();
                return None;
            }
        };
        // A quoted spelling is a name, never a keyword (rule 9): the
        // bare table form.
        if quoted {
            return self.parse_table_body(
                Ident::new(first_word),
                TableKind::Normal,
                comment,
                Span::point(self.scan.offset()),
            );
        }
        self.take_comments();
        // A comma straight after the first word is the separator
        // rejection — the endpoint grammar takes `|` only (upstream
        // rejects `IN a, b` too), and this way the header never
        // misreads a broken from-list as a name-plus-junk.
        if self.scan.peek_byte() == Some(b',') {
            let span = Span::point(self.scan.offset());
            self.diags.push(Diag::InvalidTableHeader {
                span,
                message: "endpoint lists take `|` separators, no commas — \
                          write `table user | org -> …`"
                    .to_string(),
            });
            self.recover_to_statement_end();
            return None;
        }
        if self.at_arrow() || self.scan.peek_byte() == Some(b'|') {
            // The first word opens the from-list: the `|`-separated
            // identifiers before the first `->`. A comma here is the
            // spec's documented rejection (`|` is the separator —
            // upstream errors on `IN a, b` too), diagnosed as such
            // rather than as a missing arrow.
            let mut from = vec![Ident::new(first_word)];
            loop {
                self.take_comments();
                match self.scan.peek_byte() {
                    Some(b'|') => {
                        self.scan.eat_byte(b'|');
                        from.push(self.read_table_endpoint()?);
                    }
                    Some(b',') => {
                        let span = Span::point(self.scan.offset());
                        self.diags.push(Diag::InvalidTableHeader {
                            span,
                            message: "endpoint lists take `|` separators, no commas — \
                                      write `table user | org -> …`"
                                .to_string(),
                        });
                        self.recover_to_statement_end();
                        return None;
                    }
                    _ => break,
                }
            }
            return self.parse_table_relation(Some(from), comment);
        }
        // The variant words are variant words only when followed by a
        // name — a word, backtick-quoted, or angle-quoted spelling.
        let at_name = matches!(
            self.cursor_char(),
            Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '`' || c == '⟨'
        );
        if at_name && first_word == "view" {
            let name = self.read_statement_name()?;
            return self.parse_table_view_body(name, comment);
        }
        if at_name && TABLE_VARIANT_WORDS.contains(&first_word.as_str()) {
            let name = self.read_statement_name()?;
            let kind = match first_word.as_str() {
                "any" => TableKind::Any,
                // `relation` — the unconstrained spelling; the arrow
                // forms carry their sides in the header.
                _ => TableKind::Relation {
                    from: None,
                    to: None,
                    enforced: false,
                    lightweight: false,
                },
            };
            return self.parse_table_body(name, kind, comment, Span::point(self.scan.offset()));
        }
        // The name of a bare table — the name position's rules apply
        // (rule 9; a bare reserved word cannot be a name).
        if is_reserved(&first_word) {
            self.diags.push(Diag::ReservedIdent {
                span: first_span,
                word: first_word,
            });
            self.recover_to_statement_end();
            return None;
        }
        self.parse_table_body(
            Ident::new(first_word),
            TableKind::Normal,
            comment,
            Span::point(self.scan.offset()),
        )
    }

    /// The relation header's tail: `-> <name> -> [<to-list>]`. Both
    /// arrows are mandatory (spec/table.md — the name is always
    /// flanked by `->` on both sides; a one-arrow header is the spec's
    /// parse error), and the to-list runs to the block or `;`. An
    /// omitted side is the unconstrained one; both omitted is the
    /// `table relation <name>;` form's equivalent.
    fn parse_table_relation(
        &mut self,
        from: Option<Vec<Ident>>,
        comment: Option<DocComment>,
    ) -> Option<Stmt> {
        // The first `->` is at the cursor (the caller peeked it).
        self.scan.eat_byte(b'-');
        self.scan.eat_byte(b'>');
        let name = self.read_statement_name()?;
        self.take_comments();
        if !self.at_arrow() {
            self.diags.push(Diag::InvalidTableHeader {
                span: Span::point(self.scan.offset()),
                message: "the relation header needs its second arrow — write \
                          `table [<from>] -> <name> -> [<to>]`"
                    .to_string(),
            });
            self.recover_to_statement_end();
            return None;
        }
        self.scan.eat_byte(b'-');
        self.scan.eat_byte(b'>');
        self.take_comments();
        let to = match self.scan.peek_byte() {
            Some(b) if b != b'{' && b != b';' => {
                let mut to = vec![self.read_table_endpoint()?];
                loop {
                    self.take_comments();
                    match self.scan.peek_byte() {
                        Some(b'|') => {
                            self.scan.eat_byte(b'|');
                            to.push(self.read_table_endpoint()?);
                        }
                        Some(b',') => {
                            let span = Span::point(self.scan.offset());
                            self.diags.push(Diag::InvalidTableHeader {
                                span,
                                message: "endpoint lists take `|` separators, no commas — \
                                          write `… -> post | comment`"
                                    .to_string(),
                            });
                            self.recover_to_statement_end();
                            return None;
                        }
                        _ => break,
                    }
                }
                Some(to)
            }
            _ => None,
        };
        // The LIGHTWEIGHT flag trails the header (spec/table.md): the
        // to-list read is greedy, so an endpoint named `lightweight`
        // reads as an endpoint and the flag needs the list closed
        // before it (`-> c lightweight {`); the quoted spelling is a
        // name, never the flag (rule 9). The word's span anchors the
        // fold-site shape validations.
        let mut lightweight = false;
        let mut lightweight_span = Span::point(self.scan.offset());
        self.take_comments();
        if let Some(word) = self
            .scan
            .peek_word()
            .filter(|word| word.slice(self.src) == "lightweight")
        {
            self.scan.advance_by(word.end() - self.scan.offset());
            lightweight = true;
            lightweight_span = word;
        }
        self.take_comments();
        self.parse_table_body(
            name,
            TableKind::Relation {
                from,
                to,
                enforced: false,
                lightweight,
            },
            comment,
            lightweight_span,
        )
    }

    /// One relation endpoint: an identifier in any of SurrealQL's
    /// three forms, rejecting bare reserved words (rule 9 — endpoints
    /// are names, `any` included; SDL is conservatively stricter than
    /// upstream's ident reader). On failure the diagnostic is pushed,
    /// recovery skips to statement end, and `None` comes back.
    fn read_table_endpoint(&mut self) -> Option<Ident> {
        self.take_comments();
        let (span, raw, quoted) = match self.scan.read_ident() {
            Ok(Some(ident)) => ident,
            Ok(None) => {
                self.diags.push(Diag::InvalidTableHeader {
                    span: Span::point(self.scan.offset()),
                    message: "expected an endpoint table here — `|`-separated \
                              identifiers, no commas"
                        .to_string(),
                });
                self.recover_to_statement_end();
                return None;
            }
            Err(diag) => {
                self.diags.push(diag);
                self.recover_to_statement_end();
                return None;
            }
        };
        if !quoted && is_reserved(&raw) {
            self.diags.push(Diag::ReservedIdent { span, word: raw });
            self.recover_to_statement_end();
            return None;
        }
        Some(Ident::new(raw))
    }

    /// The non-view table body: the header-position schema flags
    /// (`table user schemafull {` — the spec's example spelling),
    /// then the clause block over [`TABLE_CLAUSES`] (nested heads
    /// included), then the fold and the validation pass — `enforced`
    /// is relation-only (a clause the other kinds do not define; the
    /// union table parses it for the relation's sake), and the
    /// changefeed sub-block demands its `expiry`. The schema flags
    /// are NOT exclusive: upstream's clause loop silently takes the
    /// last spelling (no conflict tracking), so header and block
    /// spellings fold plain last-one-wins (rule 7, SurrealQL parity —
    /// probe-verified against v3.2.4 `parse_define_table`).
    fn parse_table_body(
        &mut self,
        name: Ident,
        kind: TableKind,
        comment: Option<DocComment>,
        lightweight_span: Span,
    ) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let mut schema_full = self.read_header_schema_flags();
        let mut drop = false;
        let mut changefeed: Option<ChangeFeed> = None;
        let mut permissions: Option<OpPermissions> = None;
        let mut graphql: Option<Graphql> = None;
        let mut enforced = false;
        let mut enforced_span = Span::point(self.scan.offset());
        let mut inline_edges: Option<u16> = None;
        let mut inline_references: Option<u16> = None;
        // The nested children with their head spans, per statement
        // word — the LIGHTWEIGHT rejection anchors on them, and
        // fields additionally feed the subfield-under-computed pass.
        let mut children: Vec<(&'static str, Span)> = Vec::new();
        let mut nested: Vec<Stmt> = Vec::new();
        // The spread entries (name + site span) for the cross-entry
        // rules below (spec/mixin.md).
        let mut spread_refs: Vec<(String, Span)> = Vec::new();
        // The nested fields with their head spans (the subfield-under-
        // computed check's anchors; spec/field.md).
        let mut nested_fields: Vec<(Span, Stmt)> = Vec::new();
        // The host table rides the parser for the body's clause loop
        // (nested heads read it). Cleared the moment the block parse
        // returns — the fold below parses nothing, and EVERY exit
        // (the caps' early returns included) passes this clear: a
        // stale host would leak a `FieldHost::Table` into a later
        // mixin body's bare-head members (the shapes converge —
        // `on:` is forbidden either way and expansion overwrites it —
        // but the invariant stays total).
        self.table_host = Some(name.clone());
        let clause_comments;
        let tail = self.parse_stmt_block("table", TABLE_CLAUSES);
        self.table_host = None;
        match tail {
            None => {
                return None;
            }
            Some(StmtTail::Block {
                clauses,
                close_note,
                ..
            }) => {
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.name, clause.value) {
                        ("schemafull", ClauseValue::Flag) => schema_full = Some(true),
                        ("schemaless", ClauseValue::Flag) => schema_full = Some(false),
                        ("drop", ClauseValue::Flag) => drop = true,
                        ("enforced", ClauseValue::Flag) => {
                            enforced = true;
                            enforced_span = clause.span;
                        }
                        ("inline edges", ClauseValue::Number(value)) => {
                            if value > MAX_INLINE_CACHE_CAP {
                                self.diags.push(Diag::InvalidTableParam {
                                    span: clause.span,
                                    message: format!(
                                        "an INLINE cache cap cannot exceed \
                                         {MAX_INLINE_CACHE_CAP} — `inline edges: {value};` is \
                                         out of domain"
                                    ),
                                });
                                return None;
                            }
                            inline_edges = Some(value);
                        }
                        ("inline references", ClauseValue::Number(value)) => {
                            if value > MAX_INLINE_CACHE_CAP {
                                self.diags.push(Diag::InvalidTableParam {
                                    span: clause.span,
                                    message: format!(
                                        "an INLINE cache cap cannot exceed \
                                         {MAX_INLINE_CACHE_CAP} — `inline references: \
                                         {value};` is out of domain"
                                    ),
                                });
                                return None;
                            }
                            inline_references = Some(value);
                        }
                        ("changefeed", ClauseValue::Sub(sub)) => {
                            let (feed, expiry_attempted) = changefeed_of(&sub);
                            if let Some(feed) = feed {
                                changefeed = Some(feed);
                            } else if !expiry_attempted {
                                self.diags.push(Diag::RequiredClauseMissing {
                                    span: clause.span,
                                    clause: "expiry",
                                    block: "changefeed",
                                });
                            }
                        }
                        ("permissions", ClauseValue::OpPermissions(p)) => permissions = Some(p),
                        ("graphql", ClauseValue::Sub(sub)) => {
                            let params = graphql_of(&sub);
                            if params.alias.is_some() || params.deprecated.is_some() {
                                graphql = Some(params);
                            }
                        }
                        ("index", ClauseValue::Nested(mut stmt))
                        | ("event", ClauseValue::Nested(mut stmt)) => {
                            children.push((clause.name, clause.span));
                            attach_nested_close_note(&mut stmt, clause.after);
                            nested.push(*stmt);
                        }
                        ("field", ClauseValue::Nested(mut stmt)) => {
                            children.push(("field", clause.span));
                            attach_nested_close_note(&mut stmt, clause.after);
                            nested_fields.push((clause.span, (*stmt).clone()));
                            nested.push(*stmt);
                        }
                        ("spread", ClauseValue::Nested(mut stmt)) => {
                            // The entry's same-line after-note rides its
                            // own empty-path anchor, the nested fields'
                            // carry (rule 5 — nothing is lost).
                            attach_nested_close_note(&mut stmt, clause.after);
                            let Stmt::Spread(spread) = *stmt else {
                                unreachable!("the spread entry parse yields Stmt::Spread only");
                            };
                            children.push(("spread", clause.span));
                            spread_refs.push((spread.name.raw.clone(), spread.span));
                            nested.push(Stmt::Spread(spread));
                        }
                        (_, ClauseValue::Failed) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
            }
            // The bare form — `table relation generic_edge;` — is the
            // clause-less statement; a view never reaches here.
            Some(StmtTail::Bare { close_note }) => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
        }
        // One key per body (spec/mixin.md; SDL-E1067): the same
        // member key twice here is the copy-paste smell — diagnosed
        // before the spread rules so a duplicate never reaches them.
        check_duplicate_member_keys(
            &mut self.diags,
            nested.iter().zip(children.iter().map(|(_, span)| *span)),
        );
        // The spread entries' cross-entry rule that stays at parse
        // (spec/mixin.md): the same mixin twice in one body is the
        // copy-paste smell (SDL-E1064), decided on raw names — the
        // table drops (the fold's validation discipline). Resolution
        // itself is NOT parse's: the entries ride the AST as opaque
        // names, and [`resolve_spreads_homed`] checks them against
        // the whole document after the parse.
        let mut spread_names: Vec<&str> = Vec::new();
        for (name, span) in &spread_refs {
            if spread_names.contains(&name.as_str()) {
                self.diags.push(Diag::DuplicateSpread {
                    span: *span,
                    name: name.clone(),
                });
                return None;
            }
            spread_names.push(name);
        }
        // No subfield under a computed parent (spec/field.md): the
        // block-local pass — one table body is the one place every
        // EXPLICIT candidate is in hand at parse time, in EITHER
        // source order (the engine checks both directions at define).
        // The spread entries' landed members ride the resolution pass
        // instead — they are not parse's to name (their mixins may be
        // defined anywhere in the document).
        let computed_roots: Vec<String> = nested_fields
            .iter()
            .filter_map(|(_, stmt)| match stmt {
                Stmt::Field(field) if field.computed.is_some() => {
                    Some(field_path_text(&field.name))
                }
                _ => None,
            })
            .collect();
        for (head_span, stmt) in &nested_fields {
            let Stmt::Field(field) = stmt else {
                unreachable!("only fields land in nested_fields");
            };
            check_subfield_under_computed(&mut self.diags, *head_span, field, &computed_roots);
        }
        if enforced && !matches!(kind, TableKind::Relation { .. }) {
            self.diags.push(Diag::InvalidTableClause {
                span: enforced_span,
                message: format!(
                    "`enforced;` is not part of the {} table vocabulary — it belongs to a \
                     relation table",
                    kind_word(&kind)
                ),
            });
            return None;
        }
        // The LIGHTWEIGHT shape (3.3, spec/table.md): the engine's
        // define-time validations mirrored at parse (rule 10's ADT
        // pattern — each message tracks the engine's). Both sides
        // named, and the record-consuming clauses rejected — a shape
        // the engine would reject at define diagnoses here instead of
        // emitting DDL that fails at exec.
        if matches!(
            kind,
            TableKind::Relation {
                lightweight: true,
                ..
            }
        ) {
            if !matches!(
                kind,
                TableKind::Relation {
                    from: Some(_),
                    to: Some(_),
                    ..
                }
            ) {
                self.diags.push(Diag::InvalidTableClause {
                    span: lightweight_span,
                    message: "a LIGHTWEIGHT relation requires explicit IN and OUT tables — \
                              write `table <from> -> <name> -> <to> lightweight`"
                        .to_string(),
                });
                return None;
            }
            if drop {
                self.diags.push(Diag::InvalidTableClause {
                    span: lightweight_span,
                    message: "a LIGHTWEIGHT relation cannot be declared DROP: its edges \
                              store no records"
                        .to_string(),
                });
                return None;
            }
            if changefeed.is_some() {
                self.diags.push(Diag::InvalidTableClause {
                    span: lightweight_span,
                    message: "a LIGHTWEIGHT relation cannot be declared CHANGEFEED: its \
                              edges store no records"
                        .to_string(),
                });
                return None;
            }
            if schema_full == Some(true) {
                self.diags.push(Diag::InvalidTableClause {
                    span: lightweight_span,
                    message: "a LIGHTWEIGHT relation cannot be declared SCHEMAFULL: its \
                              edges store no records"
                        .to_string(),
                });
                return None;
            }
            if let Some((word, span)) = children.first() {
                let object = match *word {
                    "field" => "a field definition",
                    "index" => "an index",
                    "spread" => "a spread (a mixin's members)",
                    _ => "an event",
                };
                self.diags.push(Diag::InvalidTableClause {
                    span: *span,
                    message: format!(
                        "a LIGHTWEIGHT relation cannot carry {object}: its edges store no \
                         records"
                    ),
                });
                return None;
            }
        }
        // The INLINE field contract's host half (3.3, spec/field.md):
        // the engine demands an existing TYPE RELATION table at
        // define — the nested form's host kind is known here, so the
        // mirror is exact (the standalone form is the linter's L0019).
        if !matches!(kind, TableKind::Relation { .. })
            && let Some((span, _)) = nested_fields
                .iter()
                .find(|(_, stmt)| matches!(stmt, Stmt::Field(field) if field.inline))
        {
            self.diags.push(Diag::InvalidTableClause {
                span: *span,
                message: format!(
                    "an INLINE field requires a TYPE RELATION table — `{}` is not one",
                    kind_word(&kind)
                ),
            });
            return None;
        }
        // The same mirror for a spread member's `inline;` rides the
        // resolution pass — the member's mixin may live anywhere in
        // the document, so the fold cannot name it (the pass anchors
        // at the spread entry, with the mixin named in its message).
        // The folded flags ride their kind: `enforced` is part of
        // TableKind::Relation's payload, but it arrives via the
        // clause block like every other flag — and `lightweight`
        // implies it (the engine's define-time normalization,
        // probe-pinned: INFO always prints `ENFORCED LIGHTWEIGHT`).
        let kind = match kind {
            TableKind::Relation {
                from,
                to,
                lightweight,
                ..
            } => TableKind::Relation {
                from,
                to,
                enforced: enforced || lightweight,
                lightweight,
            },
            other => other,
        };
        Some(Stmt::Table(Table {
            name,
            kind,
            schema_full,
            drop,
            changefeed,
            inline_edges,
            inline_references,
            permissions,
            graphql,
            nested,
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// One `...<name>;` spread entry (spec/mixin.md), the `...`
    /// already peeked by the clause loop: the name reads as an
    /// identifier, `;` terminates (EOF stands in on the file's last
    /// line, everywhere's rule). The entry is a body sibling of the
    /// nested field heads — it drains the comment buffer itself and
    /// lands as a [`ParsedClause`] so the loop's anchoring machinery
    /// treats it like any landed entry. The name rides the AST
    /// opaque — resolution (SDL-E1061) is [`resolve_spreads_homed`]'s;
    /// the same-mixin-twice rejection is the table fold's.
    fn spread_entry(&mut self) -> ParsedClause {
        let span_start = self.scan.offset();
        self.scan.eat_byte(b'.');
        self.scan.eat_byte(b'.');
        self.scan.eat_byte(b'.');
        let comments = self.take_pending_comments();
        // The name read skips trivia like every name position — a
        // spread split across lines is the same entry.
        self.take_comments();
        let name_start = self.scan.offset();
        let value = match self.scan.read_ident() {
            Ok(Some((_, raw, _))) => {
                let span = Span::new(span_start, self.scan.offset() - span_start);
                let stmt = Stmt::Spread(Spread {
                    name: Ident::new(raw),
                    span,
                    comments,
                    clause_comments: Vec::new(),
                });
                self.take_comments();
                if self.scan.eat_byte(b';') || self.scan.is_at_end() {
                    Some(stmt)
                } else {
                    self.diags.push(Diag::ExpectedStatementEnd {
                        span: Span::point(self.scan.offset()),
                        found: self.cursor_char(),
                    });
                    self.recover_to_clause_end();
                    None
                }
            }
            Ok(None) => {
                self.diags.push(Diag::ExpectedName {
                    span: Span::point(name_start),
                });
                self.recover_to_clause_end();
                None
            }
            Err(diag) => {
                self.diags.push(diag);
                self.recover_to_clause_end();
                None
            }
        };
        ParsedClause {
            span: Span::new(span_start, self.scan.offset() - span_start),
            name: "spread",
            value: value
                .map(|stmt| ClauseValue::Nested(Box::new(stmt)))
                .unwrap_or(ClauseValue::Failed),
            before: Vec::new(),
            after: Vec::new(),
            interior: Vec::new(),
        }
    }

    /// `mixin <name> { members };` (spec/mixin.md) — the SDL-only
    /// member group. Members are a table block's body minus `on:`
    /// clauses: fields (the `field`-prefixed head and bare heads
    /// alike — the body loop runs over [`MIXIN_CLAUSES`] with the
    /// field-head detection gated open), indexes, and events, each
    /// parsed as [`FieldHost::Mixin`]: no host at parse, `on:`
    /// forbidden, the empty `on` placeholder expansion overwrites.
    /// `///` on the statement is rejected (it emits nothing, so there
    /// is no COMMENT target — rule 5; members keep their own). The
    /// statement registers its NAME for the duplicate rejection, and
    /// a duplicate name is rejected (SDL-E1062 — no last-one-wins
    /// fold: the reference would be ambiguous in intent). Emits no
    /// DDL; expansion ([`crate::expand`]) drops it from the lowered
    /// file.
    fn parse_mixin(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        if let Some(doc) = pending_doc {
            self.diags.push(Diag::DocCommentUnsupported {
                span: doc.span,
                statement: "mixin",
            });
        }
        let comments = self.take_pending_comments();
        let name_start = self.scan.offset();
        let name = self.read_statement_name()?;
        let name_span = Span::new(name_start, self.scan.offset() - name_start);
        // Members with their head spans — the registry's diagnostic
        // anchors (the AST carries no per-member span).
        let mut members: Vec<(Stmt, Span)> = Vec::new();
        // The statement's own anchors: the block's close note (the
        // empty path) — member entries' anchors move onto the member
        // statements in the fold below, and their leftover clause
        // paths are never read (the writer prints no member clauses
        // at the statement level; the table fold's established
        // shape).
        let clause_comments;
        match self.parse_stmt_block("mixin", MIXIN_CLAUSES)? {
            StmtTail::Bare { close_note } => {
                clause_comments = collect_clause_comments(&[], close_note);
            }
            StmtTail::Block {
                clauses,
                close_note,
                ..
            } => {
                clause_comments = collect_clause_comments(&clauses, close_note);
                for clause in clauses {
                    match (clause.name, clause.value) {
                        ("field" | "index" | "event", ClauseValue::Nested(mut stmt)) => {
                            attach_nested_close_note(&mut stmt, clause.after);
                            members.push((*stmt, clause.span));
                        }
                        (_, ClauseValue::Failed) => {}
                        _ => unreachable!("clause_loop yields mixin keys only"),
                    }
                }
            }
        }
        // No subfield under a computed parent — the same block-local
        // pass the table body runs (spec/field.md), over the mixin's
        // own members: one body holds every candidate.
        let computed_roots: Vec<String> = members
            .iter()
            .filter_map(|(stmt, _)| match stmt {
                Stmt::Field(field) if field.computed.is_some() => {
                    Some(field_path_text(&field.name))
                }
                _ => None,
            })
            .collect();
        for (stmt, head_span) in &members {
            if let Stmt::Field(field) = stmt {
                check_subfield_under_computed(&mut self.diags, *head_span, field, &computed_roots);
            }
        }
        check_duplicate_member_keys(
            &mut self.diags,
            members.iter().map(|(stmt, span)| (stmt, *span)),
        );
        // The duplicate-name rejection (spec/README.md rule 7's
        // generalized rule, mixin family) — parse keeps only the
        // first spelling; the order pass catches every other family
        // and the cross-file class.
        if self.mixin_names.iter().any(|entry| entry == &name.raw) {
            self.diags.push(Diag::DuplicateDefinition {
                span: name_span,
                family: "mixin",
                name: name.raw.clone(),
            });
            return None;
        }
        self.mixin_names.push(name.raw.clone());
        Some(Stmt::Mixin(Mixin {
            name,
            members: members.into_iter().map(|(stmt, _)| stmt).collect(),
            name_span,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// `type <Name> = <kind>;` (spec/type-alias.md) — the SDL-only
    /// type alias, top-level only. `///` is rejected (no COMMENT
    /// target — the statement emits nothing). The name MUST be
    /// alias-shaped — an uppercase ASCII letter, then letters, digits,
    /// or `_`, decoded (a quoted declaration spelling decodes first) —
    /// and not reserved: a SurrealQL kind head would win at every use
    /// site (head matching is ASCII-case-insensitive) and leave the
    /// alias unreachable; the Capitalized rule is what keeps alias
    /// uses grammatically distinct from the lowercase kind heads
    /// (SDL-E1072 either way). The right-hand kind captures through
    /// the permissive grammar to the `;` — the same
    /// [`Self::capture_incremental_kind`] a field's header rides, so
    /// brace-bearing object kinds fold. Uses parse as opaque
    /// [`crate::kind::KindAst::Alias`] nodes; the order pass owns the
    /// unknown-reference (SDL-E1071), cycle (SDL-E0015), and deferred
    /// flexible checks against the whole document's alias set. Emits
    /// no DDL; expansion ([`crate::expand`]) rewrites every
    /// referencing `Kind` and drops the statement from the lowered
    /// file.
    fn parse_type_alias(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        if let Some(doc) = pending_doc {
            self.diags.push(Diag::DocCommentUnsupported {
                span: doc.span,
                statement: "type",
            });
        }
        let comments = self.take_pending_comments();
        let name_start = self.scan.offset();
        let name = self.read_statement_name()?;
        let name_span = Span::new(name_start, self.scan.offset() - name_start);
        if !crate::kind::is_valid_alias_name(&name.raw) {
            let reason = if crate::kind::is_reserved_alias_name(&name.raw) {
                format!(
                    "`{}` is a SurrealQL kind head — it would win at every use site",
                    name.raw
                )
            } else {
                "alias names are Capitalized: an uppercase letter, then letters, digits, or `_`"
                    .to_string()
            };
            self.diags.push(Diag::InvalidAliasName {
                span: name_span,
                name: name.raw.clone(),
                reason,
            });
            self.recover_to_statement_end();
            return None;
        }
        self.take_comments();
        if self.scan.peek_byte() != Some(b'=') {
            self.diags.push(Diag::UnexpectedClauseForm {
                span: Span::point(self.scan.offset()),
                message: "expected ` = <kind>` — a type alias declares its kind in the header"
                    .to_string(),
            });
            self.recover_to_statement_end();
            return None;
        }
        self.scan.eat_byte(b'=');
        // The `;`-terminated capture: EOF is the file's final
        // statement missing its terminator — the kind still landed.
        let (kind_text, _) = match self.capture_incremental_kind(b";") {
            Ok(found) => found,
            Err(_) => return None,
        };
        // The duplicate-name rejection (README rule 7's generalized
        // rule, type-alias family) — parse keeps only the first
        // spelling; the order pass catches the cross-file class.
        if self.alias_names.iter().any(|entry| entry == &name.raw) {
            self.diags.push(Diag::DuplicateDefinition {
                span: name_span,
                family: "type alias",
                name: name.raw.clone(),
            });
            return None;
        }
        self.alias_names.push(name.raw.clone());
        // The close note joins the statement's own line (the
        // empty-path anchor — the statement has no clauses).
        let close_note = self.peek_same_line_note(self.cursor_line());
        Some(Stmt::TypeAlias(TypeAlias {
            name,
            kind: Kind::new(kind_text),
            name_span,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments: collect_clause_comments(&[], close_note),
        }))
    }

    /// The view body (spec/table.md): the block IS the query — a
    /// single `;`-terminated select, no clause key — optionally
    /// followed by ordinary clauses over [`TABLE_VIEW_CLAUSES`]
    /// (`permissions`, `graphql`). The query canonicalizes through the
    /// embedded parser's own AS-SELECT position ([`Slot::
    /// TableViewQuery`]), which is where SurrealQL's restricted-select
    /// grammar enforces itself; anything beyond it dies there.
    fn parse_table_view_body(&mut self, name: Ident, comment: Option<DocComment>) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        self.take_comments();
        if self.scan.peek_byte() != Some(b'{') {
            self.diags.push(Diag::InvalidViewBody {
                span: Span::point(self.scan.offset()),
                message: "a view's block is its query — write \
                          `table view <name> { select …; };`"
                    .to_string(),
            });
            self.recover_to_statement_end();
            return None;
        }
        let brace = Span::new(self.scan.offset(), 1);
        self.scan.eat_byte(b'{');
        self.take_comments();
        if self.scan.peek_byte() == Some(b'}') || self.scan.is_at_end() {
            self.diags.push(Diag::InvalidViewBody {
                span: brace,
                message: "the query is required — a view without one is not a view".to_string(),
            });
            self.recover_to_statement_end();
            return None;
        }
        // A clause-key-shaped head (`permissions {`, `graphql:`) is
        // the misplaced query — diagnosed as such rather than as a
        // confusing embedded rejection. The head is peeked on the raw
        // source BEFORE the capture: a block-form head would
        // otherwise kill the capture on the view's own closer and
        // diagnose the wrong byte. A select never opens with these
        // words (upstream's parse_view demands SELECT first), so the
        // shape is unambiguous.
        if self.view_head_shaped() {
            self.diags.push(Diag::InvalidViewBody {
                span: brace,
                message: "the view's query comes first — write `select …;` \
                          before any clauses"
                    .to_string(),
            });
            self.recover_view_body_end();
            return None;
        }
        let Some(span) = self.capture_to_terminator() else {
            // The capture died mid-body (a stray closer — its own
            // diagnostic already fired); consume the rest of the body
            // so no spurious junk diagnostics follow.
            self.recover_view_body_end();
            return None;
        };
        // The comment-interrupted spellings (`permissions /* c */: …`)
        // evade the raw-source peek — the same check on the captured
        // text catches them.
        if opens_with_clause_head(span.slice(self.src), VIEW_HEAD_WORDS) {
            self.diags.push(Diag::InvalidViewBody {
                span: brace,
                message: "the view's query comes first — write `select …;` \
                          before any clauses"
                    .to_string(),
            });
            self.recover_view_body_end();
            return None;
        }
        let Some(query) = self.canonicalize_capture(span, Slot::TableViewQuery) else {
            // The rejected query already reported; consume the rest of
            // the view body so no spurious junk diagnostics follow.
            self.recover_view_body_end();
            return None;
        };
        // The trailing clauses over the two-key table — the shared
        // interior/terminator discipline (a failed body parks the
        // cursor past the `}`; only a clean body demands its `;`).
        let mut permissions: Option<OpPermissions> = None;
        let mut graphql: Option<Graphql> = None;
        let body_diags = self.diags.len();
        let clause_comments = match self.clause_loop(brace, "table view", TABLE_VIEW_CLAUSES) {
            Ok(clauses) => {
                let mut anchors = collect_clause_comments(&clauses, None);
                for clause in clauses {
                    match (clause.name, clause.value) {
                        ("permissions", ClauseValue::OpPermissions(p)) => permissions = Some(p),
                        ("graphql", ClauseValue::Sub(sub)) => {
                            let params = graphql_of(&sub);
                            if params.alias.is_some() || params.deprecated.is_some() {
                                graphql = Some(params);
                            }
                        }
                        (_, ClauseValue::Failed) => {}
                        _ => unreachable!("clause_loop yields table keys only"),
                    }
                }
                if self.diags.len() == body_diags {
                    self.check_block_terminator();
                } else {
                    self.take_comments();
                    self.scan.eat_byte(b';');
                }
                // The view's own close note rides the empty path.
                if let Some(note) = self.peek_same_line_note(self.cursor_line()) {
                    push_clause_anchor(&mut anchors, "", &[], &[note]);
                }
                anchors
            }
            Err(span) => {
                self.diags.push(Diag::UnterminatedBlock { span });
                return None;
            }
        };
        // The view's fixed emission — `TYPE ANY SCHEMALESS AS SELECT …`
        // — rides the codegen arm; `drop`/`changefeed`/the schema
        // flags/`enforced`/nested heads never reach here (the view's
        // clause table omits them all).
        Some(Stmt::Table(Table {
            name,
            kind: TableKind::View { query },
            schema_full: None,
            drop: false,
            changefeed: None,
            inline_edges: None,
            inline_references: None,
            permissions,
            graphql,
            nested: Vec::new(),
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// A mid-body view failure's cleanup: the query's capture already
    /// consumed its own `;`, so the cursor rests on the body's tail —
    /// which may hold SEVERAL leftover clause segments before the
    /// view's closing `}`. Each `;`-segment is consumed in turn
    /// (braces inside a segment — `graphql { … };` — ride the capture
    /// scanner's depth), then the body's `}` and the statement's `;`
    /// are eaten, so no spurious junk diagnostics follow and the
    /// driver rests at the next statement. EOF anywhere along the way
    /// is fine — every eat is guarded.
    fn recover_view_body_end(&mut self) {
        loop {
            self.recover_to_clause_end();
            self.take_comments();
            match self.scan.peek_byte() {
                // The view body's closing `}` — eat it and the
                // statement's `;`.
                Some(b'}') => {
                    self.scan.eat_byte(b'}');
                    self.take_comments();
                    self.scan.eat_byte(b';');
                    return;
                }
                // EOF: nothing left to close.
                None => return,
                // Another leftover segment: consume it too.
                _ => continue,
            }
        }
    }

    /// Whether the pending view-body head is clause-shaped — a view
    /// clause key followed (across spaces) by `:` or `{` — peeked on
    /// the raw source before the query capture runs. A select never
    /// opens with these words, so the shape is unambiguous; the
    /// comment-interrupted spellings ride the capture path's text
    /// check instead ([`opens_with_clause_head`]).
    fn view_head_shaped(&self) -> bool {
        let rest = &self.src[self.scan.offset()..];
        let word_len = word_prefix_len(rest);
        let word = &rest[..word_len];
        if !VIEW_HEAD_WORDS.contains(&word) {
            return false;
        }
        let tail = rest[word_len..].trim_start();
        tail.starts_with(':') || tail.starts_with('{')
    }

    /// The header-position schema flags: `schemafull` / `schemaless`,
    /// repeatedly, before the block or `;` (the spec's example
    /// spelling — `table user schemafull { … };` — alongside the
    /// block form; last-one-wins across both positions, rule 7). The
    /// word is peeked textually so a non-flag spelling stays put for
    /// the block tail to diagnose; bare lowercase only (rule 9) —
    /// SDL keywords here are words, so a quoted spelling is not a
    /// flag.
    fn read_header_schema_flags(&mut self) -> Option<bool> {
        let mut schema_full: Option<bool> = None;
        loop {
            self.take_comments();
            let rest = &self.src[self.scan.offset()..];
            let word_len = rest
                .char_indices()
                .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
            match &rest[..word_len] {
                "schemafull" => schema_full = Some(true),
                "schemaless" => schema_full = Some(false),
                _ => break,
            }
            for byte in &rest.as_bytes()[..word_len] {
                self.scan.eat_byte(*byte);
            }
        }
        schema_full
    }

    /// `permissions: none | full;` or `permissions { … };` — table
    /// permissions (spec/table.md). The single-value form is ONLY the
    /// `permissions: none | full;` or `permissions { … };` — the
    /// per-operation permissions shared by `table` and `field`
    /// (spec/table.md, spec/field.md). The single-value form is ONLY
    /// the all-operations shorthand: both statements use the
    /// per-operation parser, which accepts `NONE | FULL | FOR` and no
    /// expression (README permission forms), so a bare value diagnoses
    /// as [`Diag::InvalidPermissionsShorthand`]. `allow_delete` selects the
    /// op domain — fields reject `delete` outright (the engine's own
    /// rejection, adopted eagerly). The block's comma-grouped op lines
    /// do not fit the clause-key model, so its interior is hand-rolled
    /// over [`PERMISSION_OP_WORDS`], mirroring
    /// [`Self::parse_block_interior`]'s terminator discipline.
    fn parse_op_permissions_clause(
        &mut self,
        key_span: Span,
        statement: &'static str,
        allow_delete: bool,
    ) -> Option<(OpPermissions, Vec<ClauseComment>)> {
        self.take_comments();
        if self.scan.peek_byte() == Some(b'{') {
            return self.parse_op_permissions_block(allow_delete);
        }
        let span = self.capture_value(key_span, "permissions")?;
        let text = span.slice(self.src);
        match sole_bare_word(text) {
            Some("none") => Some((OpPermissions::None, Vec::new())),
            Some("full") => Some((OpPermissions::Full, Vec::new())),
            _ => {
                self.diags.push(Diag::InvalidPermissionsShorthand {
                    span,
                    text: text.trim().to_string(),
                    statement,
                });
                None
            }
        }
    }

    /// The per-operation `permissions { … };` interior (spec/table.md,
    /// spec/field.md): each line is `<op>[, <op>…]: none | full |
    /// <expr>;`, in any order, repeating last-one-wins per operation
    /// server-side — the groups are preserved as written (the differ
    /// compares resolved ASTs). Unlisted operations are NONE upstream
    /// for tables (the parser seeds deny) and FULL for fields (the
    /// parser seeds allow) — the engine's fold either way, never SDL
    /// emission, so nothing is required here.
    fn parse_op_permissions_block(
        &mut self,
        allow_delete: bool,
    ) -> Option<(OpPermissions, Vec<ClauseComment>)> {
        self.take_comments();
        let brace = Span::new(self.scan.offset(), 1);
        self.scan.eat_byte(b'{');
        let mut groups: Vec<PermissionGroup> = Vec::new();
        // Rule 5's per-group anchors, parallel to `groups`: path
        // `permissions.#<emitted index>` — the groups print as
        // written, so the parse index IS the emitted index.
        let mut anchors: Vec<ClauseComment> = Vec::new();
        // The line the previous group's terminator sat on (the
        // same-line split's comparison).
        let mut prev_end_line: Option<usize> = None;
        let body_diags = self.diags.len();
        'group: loop {
            self.take_comments();
            if let Some(line) = prev_end_line
                && let Some(note) = self.take_pending_same_line(line)
            {
                // The note's group is the one that just landed; its
                // entry exists only when a before-note created it.
                let path = format!("permissions.#{}", groups.len().saturating_sub(1));
                match anchors.last_mut() {
                    Some(entry) if entry.path == path => entry.after.push(note),
                    _ => anchors.push(ClauseComment {
                        path,
                        before: Vec::new(),
                        after: vec![note],
                    }),
                }
            }
            match self.scan.peek_byte() {
                Some(b'}') => {
                    self.scan.eat_byte(b'}');
                    break;
                }
                None => {
                    self.diags.push(Diag::UnterminatedBlock { span: brace });
                    return None;
                }
                // Stray `;`s between groups are noise, like inside a
                // clause block.
                Some(b';') => {
                    self.scan.eat_byte(b';');
                    continue;
                }
                _ => {}
            }
            if self.scan.at_doc_comment() {
                self.push_misplaced_doc_clause(if allow_delete {
                    "table permissions"
                } else {
                    "field permissions"
                });
                self.recover_to_clause_end();
                continue;
            }
            // Rule 5: what sits pending above the group's head word
            // anchors to the group.
            let before = self.take_pending_comments();
            let first_op = match self.read_permission_op(allow_delete) {
                Some((_, op)) => op,
                None => {
                    self.recover_to_clause_end();
                    continue;
                }
            };
            // The comma-grouped ops: a failed word abandons the whole
            // group (recovery parked the cursor past its `;`) — the
            // label re-enters the group loop, not the comma list.
            let mut ops = vec![first_op];
            'ops: loop {
                self.take_comments();
                match self.scan.peek_byte() {
                    Some(b',') => {
                        self.scan.eat_byte(b',');
                        match self.read_permission_op(allow_delete) {
                            Some((_, op)) => ops.push(op),
                            None => {
                                self.recover_to_clause_end();
                                continue 'group;
                            }
                        }
                    }
                    _ => break 'ops,
                }
            }
            self.take_comments();
            if !self.scan.eat_byte(b':') {
                self.diags.push(Diag::ExpectedClauseValue {
                    span: brace,
                    key: "permissions",
                });
                self.recover_to_clause_end();
                continue;
            }
            let Some(span) = self.capture_to_terminator() else {
                continue;
            };
            let text = span.slice(self.src);
            let value = match sole_bare_word(text) {
                Some("none") => Permission::None,
                Some("full") => Permission::Full,
                _ => {
                    // The expression slot rides the statement's own
                    // grammar position: the print's materialized tail
                    // differs by domain (tables group their unlisted
                    // ops as NONE, fields as FULL), so the slice
                    // bounds do too.
                    let slot = if allow_delete {
                        Slot::TablePermissionValue
                    } else {
                        Slot::FieldPermissionValue
                    };
                    match self.canonicalize_capture(span, slot) {
                        Some(expr) => Permission::Expr(expr),
                        None => continue,
                    }
                }
            };
            groups.push(PermissionGroup { ops, value });
            // An anchor lands only when it carries notes — an empty
            // entry would ride the differ's structural comparison.
            if !before.is_empty() {
                anchors.push(ClauseComment {
                    path: format!("permissions.#{}", groups.len() - 1),
                    before,
                    after: Vec::new(),
                });
            }
            prev_end_line = Some(self.cursor_line());
        }
        if self.diags.len() == body_diags {
            self.check_block_terminator();
        } else {
            self.take_comments();
            self.scan.eat_byte(b';');
        }
        // An empty block is the all-NONE default (upstream's zero-FOR
        // parse) — fold to an absent clause, like `access`'s empty
        // duration (rule 9: the default elides at emission).
        (!groups.is_empty()).then_some((OpPermissions::Groups(groups), anchors))
    }

    /// One operation word of the per-op form: a bare word from the
    /// statement's op domain (rule 9 — bare, lowercase). `allow_delete`
    /// selects the domain: fields reject `delete` with its own
    /// diagnostic (the engine's rejection, adopted eagerly) and
    /// did-you-mean over the three-word set. The span rides along for
    /// the diagnostics.
    fn read_permission_op(&mut self, allow_delete: bool) -> Option<(Span, PermissionOp)> {
        self.take_comments();
        let (span, word, quoted) = match self.scan.read_ident() {
            Ok(Some(ident)) => ident,
            Ok(None) => {
                let ch = self
                    .cursor_char()
                    .expect("not at end and an op word cannot start here");
                self.diags.push(Diag::IllegalCharacter {
                    span: Span::new(self.scan.offset(), ch.len_utf8()),
                    ch,
                });
                return None;
            }
            Err(diag) => {
                self.diags.push(diag);
                return None;
            }
        };
        if quoted {
            self.diags.push(Diag::UnknownClauseWord {
                span,
                word,
                key: "permissions",
                suggestion: None,
            });
            return None;
        }
        match permission_op_of(&word) {
            Some(op) => {
                if op == PermissionOp::Delete && !allow_delete {
                    self.diags.push(Diag::FieldDeletePermission { span });
                    return None;
                }
                Some((span, op))
            }
            None => {
                let candidates = if allow_delete {
                    PERMISSION_OP_WORDS
                } else {
                    FIELD_PERMISSION_OP_WORDS
                };
                let suggestion = did_you_mean(&word, candidates);
                self.diags.push(Diag::UnknownClauseWord {
                    span,
                    word,
                    key: "permissions",
                    suggestion,
                });
                None
            }
        }
    }

    /// A nested definition head inside the table body
    /// ([`ClauseKind::Nested`]): `index`/`event`/`field` parse their
    /// full grammar — header included — with the host table injected
    /// in place of the `on:` clause (spec/table.md: exactly one of the
    /// two spellings applies). The host rides the parser's
    /// `table_host`, set for the duration of the body's clause loop.
    /// The pending doc comment is always `None`: a `///` inside the
    /// block is the misplaced clause the body loop already diagnoses.
    fn parse_nested_head(&mut self, head: &'static str) -> Option<Stmt> {
        // Cloned to release the parser borrow — the host is a word.
        // A set table_host is the table body (the host table IS the
        // `on`); a clear one is the mixin body — the member has no
        // host until expansion injects the spreading table.
        let host_ident = self.table_host.clone();
        let host = match host_ident.as_ref() {
            Some(host) => FieldHost::Table(host),
            None => FieldHost::Mixin,
        };
        match head {
            "index" => self.parse_index_inner(None, host, true),
            "event" => self.parse_event_inner(None, host, true),
            "field" => self.parse_field_inner(None, host, true),
            _ => unreachable!("the table table holds nested heads only"),
        }
    }

    /// A bare field head the clause loop detected (see the
    /// unknown-head arm): the head word is already consumed as the
    /// path's first segment, and the field parses to the clause value
    /// the loop records (a failure — already diagnosed — keeps the
    /// clause present, the established [`ClauseValue::Failed`] mark).
    fn nested_field_clause(&mut self, first: (Span, String, bool)) -> ParsedClause {
        let span = first.0;
        let value = self
            .parse_bare_field_head(first)
            .map(|stmt| ClauseValue::Nested(Box::new(stmt)));
        // The nested field drains the shared buffer itself (its own
        // statement entry) — no before-notes here; the same-line
        // after-note (if the field's terminator shares a line with a
        // comment) attaches to this clause at the loop's next split,
        // and the table fold transfers it onto the field statement.
        ParsedClause {
            span,
            name: "field",
            value: value.unwrap_or(ClauseValue::Failed),
            before: Vec::new(),
            after: Vec::new(),
            interior: Vec::new(),
        }
    }

    /// Whether the cursor rests on `->` (the caller skips trivia
    /// first). The two bytes are tested before either is consumed —
    /// the two-byte-operator lesson.
    fn at_arrow(&self) -> bool {
        self.src[self.scan.offset()..].starts_with("->")
    }

    /// Whether the cursor (after trivia) rests where a field header
    /// continues: `:`, `=`, `;`, `{` — the four tails a field path
    /// takes (spec/field.md) — or `.` a dotted continuation (`meta.
    /// user_id: int;` reads head word `meta`, the loop's only other
    /// depth-0 use of a dot). Peeked, never consumed.
    fn at_field_head_tail(&mut self) -> bool {
        self.take_comments();
        matches!(
            self.scan.peek_byte(),
            Some(b':' | b'=' | b';' | b'{' | b'.')
        )
    }

    /// The incremental kind capture shared by function's `-> <kind>`
    /// return position and field's `: <kind>` header position — the
    /// two statement headers that carry a kind followed by more
    /// statement (the one home of the loop's reflection step
    /// extracted from their duplicated copies): capture to the next
    /// depth-0 byte of `terminators`, folding each braced group into
    /// the candidate text, because brace-bearing sub-kinds
    /// (`array<{ x: int }>`) end any single capture early. A complete
    /// candidate returns with the terminator byte that ended it
    /// (`Some`) or `None` at end of file (EOF stands in for the `;`
    /// everywhere). An incomplete candidate at a non-`{` terminator
    /// (or at EOF) diagnoses [`Diag::InvalidKind`] over the whole
    /// candidate (whole-capture anchoring); a capture error pushes its own diagnostic. All
    /// diagnosed paths return `Err` carrying the terminator byte that
    /// ended the capture (`None` for EOF or a capture error), so the
    /// caller can tell the mid-statement shape (`=` — its remainder
    /// still ahead of the cursor) from the boundary shapes (`;`, EOF)
    /// and recover only where recovery does not overshoot.
    ///
    /// Kind captures record plain `//` comments (rule 5): the kind is
    /// re-printed from the SDL kind AST, so unlike embedded-printed
    /// captures there is no canonicalization story that could eat an
    /// annotation silently. The recorded lines re-anchor into the
    /// shared pending buffer — they attach to whatever follows the
    /// kind header (the block's first child, the next clause, or the
    /// statement's trailing lines).
    fn capture_incremental_kind(
        &mut self,
        terminators: &'static [u8],
    ) -> Result<(String, Option<u8>), Option<u8>> {
        self.scan.set_record_comments(true);
        let result = self.capture_incremental_kind_inner(terminators);
        self.scan.set_record_comments(false);
        for span in self.scan.take_recorded_comments() {
            let line = crate::lexer::line_col(self.src, span.offset).0;
            let text = span.slice(self.src);
            let text = text.strip_prefix("//").unwrap_or(text).trim_end();
            self.comments.push(PendingComment {
                line,
                text: text.to_string(),
            });
        }
        result
    }

    fn capture_incremental_kind_inner(
        &mut self,
        terminators: &'static [u8],
    ) -> Result<(String, Option<u8>), Option<u8>> {
        let mut pre = String::new();
        // The candidate's full span: the first capture's start through
        // wherever the diagnosis happens (folds make the last capture
        // alone a misleading anchor).
        let mut pre_start: Option<usize> = None;
        loop {
            let capture_start = self.scan.offset();
            let (seg_span, end) = match self.scan.capture_until(terminators) {
                Ok(capture) => capture,
                Err(Diag::UnterminatedCapture { .. }) => {
                    pre.push_str(&self.src[capture_start..self.scan.offset()]);
                    let start = pre_start.unwrap_or(capture_start);
                    match parse_kind_text(&pre) {
                        Some(kind) => return Ok((kind, None)),
                        None => {
                            self.diags.push(Diag::InvalidKind {
                                span: Span::new(start, self.scan.offset() - start),
                                text: pre.trim().to_string(),
                            });
                            return Err(None);
                        }
                    }
                }
                Err(diag) => {
                    self.diags.push(diag);
                    return Err(None);
                }
            };
            if pre_start.is_none() {
                pre_start = Some(seg_span.offset);
            }
            pre.push_str(seg_span.slice(self.src));
            if let Some(kind) = parse_kind_text(&pre) {
                return Ok((kind, Some(end)));
            }
            if end != b'{' {
                self.diags.push(Diag::InvalidKind {
                    span: Span::new(
                        pre_start.expect("set above"),
                        self.scan.offset() - pre_start.expect("set above"),
                    ),
                    text: pre.trim().to_string(),
                });
                // The terminator was consumed: `;` parks at a clause
                // boundary, but `=` leaves the expression text ahead —
                // the caller recovers by its form depth.
                return Err(Some(end));
            }
            // A brace-bearing sub-kind, folded in; the capture
            // continues after the group (its leading trivia rides the
            // span, keeping the kind text verbatim).
            let interior = match self.scan.capture_balanced_braces_open_eaten() {
                Ok(span) => span,
                Err(diag) => {
                    self.diags.push(diag);
                    return Err(None);
                }
            };
            pre.push('{');
            pre.push_str(interior.slice(self.src));
            pre.push('}');
        }
    }

    /// `field [<path>][: <type>] [= <expr>] { … };` (spec/field.md) —
    /// the type statement, standalone form (`on:` required; nesting
    /// into `table` blocks, where the host table IS the `on`, rides
    /// [`Self::parse_nested_head`] and the table body's bare-head
    /// detection). Top level, the `field` prefix is REQUIRED (it is
    /// the statement keyword); inside a table block it is optional.
    fn parse_field(&mut self, pending_doc: Option<DocBlock>) -> Option<Stmt> {
        self.parse_field_inner(pending_doc, FieldHost::Standalone, false)
    }

    /// The field grammar proper, shared by the standalone and nested
    /// forms. The header reads left to right with no backtracking:
    /// the path, then `:` a type (SDL-lexed incrementally —
    /// brace-bearing object kinds fold like function's return kinds),
    /// `=` the typeless computed capture, `;` the (dead) typeless
    /// regular field, or `{` the clause block. `name: <type> = <expr>`
    /// is the typed computed short form; a trailing block after `=`
    /// stays inside the capture and dies in the embedded parser (the
    /// param precedent — expression capture ambiguity).
    fn parse_field_inner(
        &mut self,
        pending_doc: Option<DocBlock>,
        host: FieldHost<'_>,
        nested: bool,
    ) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let comment = pending_doc.map(|doc| doc.comment);
        let (name, name_span) = self.read_field_path(nested, None)?;
        self.field_header_tail(name, name_span, comments, comment, host, nested)
    }

    /// The clause loop's bare-head entry into the same grammar: the
    /// head word is already consumed (it is the path's first segment —
    /// quoted spellings included), so the path read resumes from it.
    /// Always the nested form; the host rides the parser — a set
    /// `table_host` means the table body, a clear one the mixin body
    /// (the only two statements whose clause loops accept bare field
    /// heads).
    fn parse_bare_field_head(&mut self, first: (Span, String, bool)) -> Option<Stmt> {
        let comments = self.take_pending_comments();
        let (name, name_span) = self.read_field_path(true, Some(first))?;
        let host = self.table_host.clone();
        let host = match host.as_ref() {
            Some(host) => FieldHost::Table(host),
            None => FieldHost::Mixin,
        };
        self.field_header_tail(name, name_span, comments, None, host, true)
    }

    /// The field header's tail after the path: the type / computed /
    /// block dispatch, then the fold and validation.
    fn field_header_tail(
        &mut self,
        name: FieldPath,
        name_span: Span,
        mut comments: Vec<String>,
        comment: Option<DocComment>,
        host: FieldHost<'_>,
        nested: bool,
    ) -> Option<Stmt> {
        // Comments between the path and the header's tail are still
        // leading — join them to the ones taken at the entry points.
        self.take_comments();
        comments.extend(self.take_pending_comments());
        let block = match self.scan.peek_byte() {
            Some(b':') => {
                self.scan.eat_byte(b':');
                match self.read_field_kind(nested)? {
                    FieldHead::Kind(kind) => {
                        return self.finish_field(
                            FieldHeader {
                                name,
                                name_span,
                                kind: Some(kind),
                                computed: None,
                                comment,
                                comments: comments.clone(),
                            },
                            host,
                            FieldBlock::None,
                        );
                    }
                    FieldHead::KindThenComputed(kind) => {
                        // The typed computed short form: the capture
                        // consumes its own `;`, so a failure needs no
                        // recovery — the cursor already rests at the
                        // next boundary (param's discipline).
                        let computed = self.capture_expr(Slot::FieldComputed)?;
                        return self.finish_field(
                            FieldHeader {
                                name,
                                name_span,
                                kind: Some(kind),
                                computed: Some(computed),
                                comment,
                                comments: comments.clone(),
                            },
                            host,
                            FieldBlock::None,
                        );
                    }
                    FieldHead::KindThenBlock(kind, brace) => {
                        return self.finish_field(
                            FieldHeader {
                                name,
                                name_span,
                                kind: Some(kind),
                                computed: None,
                                comment,
                                comments: comments.clone(),
                            },
                            host,
                            FieldBlock::Open(brace),
                        );
                    }
                }
            }
            Some(b'=') => {
                self.scan.eat_byte(b'=');
                // The typeless computed short form (capture consumes
                // its own `;`, same discipline).
                let computed = self.capture_expr(Slot::FieldComputed)?;
                return self.finish_field(
                    FieldHeader {
                        name,
                        name_span,
                        kind: None,
                        computed: Some(computed),
                        comment,
                        comments: comments.clone(),
                    },
                    host,
                    FieldBlock::None,
                );
            }
            // A typeless regular field: SDL's grammar cannot express
            // it (the `raw` escape hatch is rule 8's business) — the
            // type requirement is the diagnosis. The typeless block
            // form below may still hold `computed:`.
            Some(b';') | None => {
                self.diags.push(Diag::FieldTypeRequired { span: name_span });
                self.recover_name_failure(nested);
                return None;
            }
            // The typeless block form: the brace is consumed here so
            // every block rides the same disposition.
            Some(b'{') => {
                let brace = Span::new(self.scan.offset(), 1);
                self.scan.eat_byte(b'{');
                FieldBlock::Open(brace)
            }
            _ => {
                self.diags.push(Diag::ExpectedStatementEnd {
                    span: Span::point(self.scan.offset()),
                    found: self.cursor_char(),
                });
                self.recover_name_failure(nested);
                return None;
            }
        };
        self.finish_field(
            FieldHeader {
                name,
                name_span,
                kind: None,
                computed: None,
                comment,
                comments,
            },
            host,
            block,
        )
    }

    /// The header type after `:`, through
    /// [`Self::capture_incremental_kind`] (the loop shared with
    /// function's return kind). The terminator byte says what follows:
    /// `;` (or EOF — the file's final statement) the bare form, `=`
    /// the computed short form, `{` the clause block with its consumed
    /// brace handed back.
    fn read_field_kind(&mut self, nested: bool) -> Option<FieldHead> {
        let (kind_text, terminator) = match self.capture_incremental_kind(b"{;=") {
            Ok(found) => found,
            // `=` parks the cursor before the expression the capture
            // declined to take — recover by form depth; `;` and EOF
            // already rest at boundaries where recovery would
            // overshoot.
            Err(Some(b'=')) => {
                self.recover_name_failure(nested);
                return None;
            }
            Err(_) => return None,
        };
        let kind = Kind::new(kind_text);
        Some(match terminator {
            Some(b'=') => FieldHead::KindThenComputed(kind),
            Some(b'{') => FieldHead::KindThenBlock(kind, Span::new(self.scan.offset() - 1, 1)),
            _ => FieldHead::Kind(kind),
        })
    }

    /// A field name path (spec/field.md): `.`-joined segments, `.*`
    /// the array wildcard anywhere after the first (named) segment.
    /// Segments are identifiers in any of SurrealQL's three forms —
    /// bare reserved words rejected (rule 9); the `field` prefix is
    /// the escape for names colliding with clause keys. `pre` hands in
    /// an already-read first segment (the clause loop's bare-head
    /// path). On failure the diagnostic is pushed, recovery matches
    /// the form's depth, and `None` comes back.
    fn read_field_path(
        &mut self,
        nested: bool,
        pre: Option<(Span, String, bool)>,
    ) -> Option<(FieldPath, Span)> {
        self.take_comments();
        let (name_span, raw, quoted) = match pre {
            Some(pre) => pre,
            None => match self.scan.read_ident() {
                Ok(Some(ident)) => ident,
                Ok(None) => {
                    self.diags.push(Diag::ExpectedName {
                        span: Span::point(self.scan.offset()),
                    });
                    self.recover_name_failure(nested);
                    return None;
                }
                Err(diag) => {
                    self.diags.push(diag);
                    self.recover_name_failure(nested);
                    return None;
                }
            },
        };
        if !quoted && is_reserved(&raw) {
            self.diags.push(Diag::ReservedIdent {
                span: name_span,
                word: raw,
            });
            self.recover_name_failure(nested);
            return None;
        }
        let mut segments = vec![FieldPathSeg::Name(Ident::new(raw))];
        loop {
            self.take_comments();
            if self.scan.peek_byte() != Some(b'.') {
                return Some((FieldPath { segments }, name_span));
            }
            self.scan.eat_byte(b'.');
            self.take_comments();
            if self.scan.eat_byte(b'*') {
                segments.push(FieldPathSeg::Wildcard);
                continue;
            }
            let (seg_span, seg, quoted) = match self.scan.read_ident() {
                Ok(Some(ident)) => ident,
                _ => {
                    self.diags.push(Diag::InvalidFieldPath {
                        span: Span::point(self.scan.offset()),
                        message: "expected a name segment or `*` after `.`".to_string(),
                    });
                    self.recover_name_failure(nested);
                    return None;
                }
            };
            if !quoted && is_reserved(&seg) {
                self.diags.push(Diag::ReservedIdent {
                    span: seg_span,
                    word: seg,
                });
                self.recover_name_failure(nested);
                return None;
            }
            segments.push(FieldPathSeg::Name(Ident::new(seg)));
        }
    }

    /// The field's fold and validation pass — every clause last-one-
    /// wins (rule 7), then the eager rules the engine enforces at
    /// define/parse time (spec/field.md), adopted here so SDL never
    /// emits what the engine rejects.
    fn finish_field(
        &mut self,
        header: FieldHeader,
        host: FieldHost<'_>,
        block: FieldBlock,
    ) -> Option<Stmt> {
        let FieldHeader {
            name,
            name_span,
            kind,
            mut computed,
            comment,
            comments,
        } = header;
        let mut on: Option<Ident> = None;
        let mut on_forbidden: Option<Span> = None;
        let mut on_attempted = false;
        let mut default: Option<FieldDefault> = None;
        let mut default_span = Span::point(0);
        let mut value: Option<(Span, Expr)> = None;
        let mut assert: Option<(Span, Expr)> = None;
        let mut readonly = false;
        let mut readonly_span = Span::point(0);
        let mut flexible = false;
        let mut flexible_span = Span::point(0);
        let mut inline = false;
        let mut inline_span = Span::point(0);
        let mut reference: Option<OnDelete> = None;
        let mut reference_span = Span::point(0);
        let mut permissions: Option<OpPermissions> = None;
        let mut graphql: Option<Graphql> = None;
        let mut missing_span = name_span;
        let mut clause_comments = Vec::new();
        let clauses = match block {
            FieldBlock::None => None,
            // A block that never closed already reported; the
            // statement is dead — the fold and its validation pass
            // must not pile a required-`on` report onto it (one root
            // cause, one diagnostic).
            FieldBlock::Open(brace) => {
                missing_span = brace;
                Some(self.run_field_block(brace)?)
            }
        };
        if let Some(clauses) = clauses {
            clause_comments = collect_clause_comments(&clauses, self.field_close_note.take());
            for clause in clauses {
                match (&clause.value, clause.name) {
                    (ClauseValue::Ident(table), "on") => {
                        // Nested or a mixin member, the host supplies
                        // the `on` (the table block directly, the
                        // spreading table at expansion): an explicit
                        // `on:` is the forbidden spelling there
                        // (spec/table.md, spec/mixin.md — exactly one
                        // of the two forms applies).
                        if matches!(host, FieldHost::Standalone) {
                            on = Some(table.clone());
                        } else {
                            on_forbidden = Some(clause.span);
                        }
                    }
                    (ClauseValue::Failed, "on") => on_attempted = true,
                    (ClauseValue::Expr(expr), "default") => {
                        default_span = clause.span;
                        default = Some(FieldDefault::Set(expr.clone()));
                    }
                    (ClauseValue::Expr(expr), "default always") => {
                        default_span = clause.span;
                        default = Some(FieldDefault::Always(expr.clone()));
                    }
                    (ClauseValue::Expr(expr), "value") => value = Some((clause.span, expr.clone())),
                    (ClauseValue::Expr(expr), "assert") => {
                        assert = Some((clause.span, expr.clone()))
                    }
                    (ClauseValue::Expr(expr), "computed") => {
                        // The header assignment and the block clause
                        // share one slot, last-one-wins (rule 7).
                        computed = Some(expr.clone());
                    }
                    (ClauseValue::Flag, "readonly") => {
                        readonly = true;
                        readonly_span = clause.span;
                    }
                    (ClauseValue::Flag, "flexible") => {
                        flexible = true;
                        flexible_span = clause.span;
                    }
                    (ClauseValue::Flag, "inline") => {
                        inline = true;
                        inline_span = clause.span;
                    }
                    (ClauseValue::Flag, "reference") => {
                        reference = Some(OnDelete::Ignore);
                        reference_span = clause.span;
                    }
                    (ClauseValue::Sub(sub), "reference") => {
                        reference = Some(on_delete_of(sub));
                        reference_span = clause.span;
                    }
                    (ClauseValue::OpPermissions(p), "permissions") => permissions = Some(p.clone()),
                    (ClauseValue::Sub(sub), "graphql") => {
                        let params = graphql_of(sub);
                        if params.alias.is_some() || params.deprecated.is_some() {
                            graphql = Some(params);
                        }
                    }
                    (ClauseValue::Failed, _) => {}
                    _ => unreachable!("clause_loop yields table keys only"),
                }
            }
        }
        // ---- eager validation (spec/field.md's engine-enforced rules)
        let mut valid = true;
        // The clause spans both validation passes need (borrowed out —
        // the Exprs themselves fold into the AST below).
        let value_span = value.as_ref().map(|(span, _)| *span);
        let assert_span = assert.as_ref().map(|(span, _)| *span);
        // The type requirement: `None` only for a computed field.
        if kind.is_none() && computed.is_none() {
            valid = false;
            self.diags.push(Diag::FieldTypeRequired { span: name_span });
        }
        // Computed exclusivity and shape (executor-enforced upstream):
        // the first conflicting clause is the diagnosis (the help
        // covers the rest).
        if computed.is_some() {
            let conflicts = [
                ("default", default.is_some().then_some(default_span)),
                ("value", value_span),
                ("assert", assert_span),
                ("reference", reference.is_some().then_some(reference_span)),
                ("readonly", readonly.then_some(readonly_span)),
                ("inline", inline.then_some(inline_span)),
            ];
            if let Some((spelled, span)) = conflicts
                .into_iter()
                .find_map(|(spelled, hit)| hit.map(|span| (spelled, span)))
            {
                valid = false;
                self.diags.push(Diag::InvalidFieldComputed {
                    span,
                    message: format!(
                        "`computed` is mutually exclusive with `{spelled}` — a computed \
                         field is derived and takes no other clauses"
                    ),
                });
            }
            if !name.is_top_level() {
                valid = false;
                self.diags.push(Diag::InvalidFieldComputed {
                    span: name_span,
                    message: "computed fields must be top-level — a dotted or wildcard \
                              path cannot hold a computed expression"
                        .to_string(),
                });
            }
        }
        // Flexible needs a type whose kind contains an object (the
        // runtime parser's own parse-time check, mirrored).
        if flexible {
            match &kind {
                None => {
                    valid = false;
                    self.diags.push(Diag::InvalidFieldFlexible {
                        span: flexible_span,
                        message: "`flexible;` needs a header type — none was written".to_string(),
                    });
                }
                // An alias-referencing kind decides at resolution: the
                // answer is the expansion's, and the order pass
                // re-asks on the expanded text.
                Some(kind)
                    if kind_contains_object(&kind.text) == crate::kind::ObjectPresence::No =>
                {
                    valid = false;
                    self.diags.push(Diag::InvalidFieldFlexible {
                        span: flexible_span,
                        message: format!(
                            "FLEXIBLE can only be used with types containing object — \
                             `{}` does not",
                            kind.text
                        ),
                    });
                }
                _ => {}
            }
        }
        // Reference is a top-level field's clause (the engine's
        // define-time check, adopted eagerly).
        if reference.is_some() && !name.is_top_level() {
            valid = false;
            self.diags.push(Diag::InvalidFieldReference {
                span: reference_span,
                message: "`reference` belongs on a top-level field — specify a referencing \
                          field at the root level instead"
                    .to_string(),
            });
        }
        // The INLINE contract (3.3, spec/field.md): the engine's
        // define-time validations mirrored at parse (rule 10's ADT
        // pattern — each message tracks the engine's). The `id`
        // spelling rides the `id` grammar's forbidden list below, the
        // computed arm rides the conflicts above, and the host-relation
        // half is the table fold's (nested) and L0019's (standalone) —
        // the field statement cannot see its host's kind.
        if inline && !name.is_top_level() {
            valid = false;
            self.diags.push(Diag::InvalidFieldInline {
                span: inline_span,
                message: "only a top-level field can be INLINE — a dotted or wildcard \
                          path cannot embed into the adjacency payloads"
                    .to_string(),
            });
        }
        if inline
            && name
                .sole_name()
                .is_some_and(|name| matches!(name.raw.as_str(), "in" | "out"))
        {
            valid = false;
            self.diags.push(Diag::InvalidFieldInline {
                span: inline_span,
                message: "the `in` and `out` fields already live in the adjacency keys \
                          and cannot be INLINE"
                    .to_string(),
            });
        }
        // The `id` field's grammar (executor-enforced upstream; the
        // kind-compatibility half is the engine's to reject).
        if name.sole_name().map(|id| id.raw == "id").unwrap_or(false) {
            let forbidden = [
                ("value", value_span),
                (
                    "default always",
                    matches!(default, Some(FieldDefault::Always(_))).then_some(default_span),
                ),
                ("computed", computed.is_some().then_some(name_span)),
                ("readonly", readonly.then_some(readonly_span)),
                ("flexible", flexible.then_some(flexible_span)),
                ("inline", inline.then_some(inline_span)),
                ("reference", reference.is_some().then_some(reference_span)),
            ];
            for (spelled, hit) in forbidden {
                let Some(span) = hit else { continue };
                valid = false;
                self.diags.push(Diag::InvalidFieldId {
                    span,
                    message: format!(
                        "the `id` field takes only `type:`, `assert:`, and plain \
                         `default:` — `{spelled}` contradicts the record's identity"
                    ),
                });
            }
        }
        // The `on` machinery — index/event's shared fold: standalone
        // requires it, the nested form injects the host and forbids
        // the explicit spelling, and a mixin member holds the empty
        // placeholder expansion overwrites (the host is unknown until
        // a table spreads the mixin).
        let on = match self.fold_on(
            "field",
            host,
            OnClause {
                captured: on,
                explicit_span: on_forbidden,
                attempted: on_attempted,
                missing_span,
            },
            "field",
        ) {
            Some(on) if valid => on,
            _ => return None,
        };
        Some(Stmt::Field(Field {
            name,
            kind,
            on,
            default,
            value: value.map(|(_, expr)| expr),
            assert: assert.map(|(_, expr)| expr),
            computed,
            readonly,
            flexible,
            inline,
            reference,
            permissions,
            graphql,
            comment,
            comments,
            trailing_comments: self.take_pending_comments(),
            clause_comments,
        }))
    }

    /// The field's clause block over [`FIELD_CLAUSES`]: the `{` is
    /// already consumed (`brace` carried in), the shared interior and
    /// terminator discipline apply, and `None` means the block never
    /// closed. The folded clauses come back in source order.
    fn run_field_block(&mut self, brace: Span) -> Option<Vec<ParsedClause>> {
        let body_diags = self.diags.len();
        match self.clause_loop(brace, "field", FIELD_CLAUSES) {
            Ok(clauses) => {
                if self.diags.len() == body_diags {
                    self.check_block_terminator();
                } else {
                    self.take_comments();
                    self.scan.eat_byte(b';');
                }
                let end_line = self.cursor_line();
                self.field_close_note = self.peek_same_line_note(end_line);
                Some(clauses)
            }
            Err(span) => {
                self.diags.push(Diag::UnterminatedBlock { span });
                None
            }
        }
    }

    /// A group head's comma list — `<method>, <method>` — with the
    /// first head word already consumed. Bare method words only
    /// (rule 9); no trailing comma (spec/api.md, like every SDL
    /// list); a duplicate — within the list or against a method a
    /// landed group already claims — fails the list at the first
    /// offender. `Err(())` = diagnosed; the caller recovers to the
    /// group's `;`.
    fn read_api_method_list(
        &mut self,
        first_span: Span,
        first_word: String,
        seen: &[ApiMethod],
    ) -> Result<Vec<(Span, ApiMethod)>, ()> {
        let mut list: Vec<(Span, ApiMethod)> = Vec::new();
        let mut span = first_span;
        let mut word = first_word;
        loop {
            let method = api_method_of(&word).expect("the caller matched a method word");
            if list.iter().any(|(_, other)| *other == method) || seen.contains(&method) {
                self.diags
                    .push(Diag::DuplicateApiMethod { span, method: word });
                return Err(());
            }
            list.push((span, method));
            self.take_comments();
            if self.scan.peek_byte() != Some(b',') {
                return Ok(list);
            }
            self.scan.eat_byte(b',');
            self.take_comments();
            match self.scan.read_ident() {
                Ok(Some((next_span, next_word, false))) if api_method_of(&next_word).is_some() => {
                    span = next_span;
                    word = next_word;
                }
                Ok(Some((next_span, next_word, quoted))) => {
                    let suggestion = (!quoted)
                        .then(|| did_you_mean(&next_word, API_METHODS))
                        .flatten();
                    self.diags.push(Diag::UnknownApiMethod {
                        span: next_span,
                        word: next_word,
                        suggestion,
                    });
                    return Err(());
                }
                Ok(None) => {
                    // The list's trailing comma (or a `{` where the
                    // next method belongs): the empty item is the
                    // diagnosis.
                    self.diags.push(Diag::UnknownApiMethod {
                        span: Span::point(self.scan.offset()),
                        word: String::new(),
                        suggestion: None,
                    });
                    return Err(());
                }
                Err(diag) => {
                    self.diags.push(diag);
                    return Err(());
                }
            }
        }
    }

    /// `permissions: none | full | <expr>;` — the single-value form
    /// (spec/README.md, permission form 1). SDL writes no `where`
    /// keyword; the expression arm canonicalizes through the
    /// embedded parser's own `PERMISSIONS WHERE` slot (`param`'s or
    /// `bucket`'s). The colon-less `permissions {` sub-block form
    /// arrives with the per-operation statements and reads
    /// here as a missing value.
    ///
    /// Keyword detection compares the CANONICAL print, never the raw
    /// capture: trivia may legally interrupt the keyword (`full /*
    /// allow */` — rule 3's comment model), and the bare `full`
    /// idiom or `none` literal is semantically its keyword — a
    /// trivia-interrupted `full` read as the expression would emit
    /// `PERMISSIONS WHERE full`, which evaluates falsy at runtime
    /// and silently flips an allow into a deny. A string `'none'`
    /// canonizes with its quotes and stays an expression.
    fn parse_permissions_clause(&mut self, key_span: Span, slot: Slot) -> Option<Permission> {
        let span = self.capture_value(key_span, "permissions")?;
        self.canonicalize_capture(span, slot).map(|expr| {
            if expr.canon.eq_ignore_ascii_case("none") {
                Permission::None
            } else if expr.canon.eq_ignore_ascii_case("full") {
                Permission::Full
            } else {
                Permission::Expr(expr)
            }
        })
    }

    /// An `on: <level>;` clause — rule 3's keyword-set kind with
    /// input-only aliases: the capture must hold one bare word from
    /// [`LEVEL_WORDS`], canonicalized at parse time (`kv` →
    /// [`Level::Root`]) with the authored spelling carried for the
    /// writer); codegen emits the full word (rule 9).
    fn parse_level_clause(&mut self, key_span: Span, key: &'static str) -> Option<Authored<Level>> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        match sole_bare_word(text) {
            Some(word) if LEVEL_WORDS.contains(&word) => Some(Authored {
                value: level_of(word),
                spelling: LEVEL_ALIAS_WORDS.iter().find(|w| **w == word).copied(),
            }),
            Some(word) => {
                let suggestion = did_you_mean(word, LEVEL_WORDS);
                self.diags.push(Diag::UnknownClauseWord {
                    span,
                    word: word.to_string(),
                    key,
                    suggestion,
                });
                None
            }
            None => {
                self.diags.push(Diag::UnknownClauseWord {
                    span,
                    word: text.trim().to_string(),
                    key,
                    suggestion: None,
                });
                None
            }
        }
    }

    /// A `<string>` clause `key: '<string>';` — rule 3's keyword
    /// clause: SDL-lexed and decoded to its value, never passed to
    /// the embedded parser. Quoting styles and `\` escapes follow
    /// SurrealQL (`parse_string_lit`); the value, never the spelling,
    /// is what the AST holds.
    fn parse_str_clause(&mut self, key_span: Span, key: &'static str) -> Option<String> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        match parse_string_literal_text(text) {
            Some(value) => Some(value),
            None => {
                self.diags.push(Diag::InvalidString {
                    span,
                    text: text.trim().to_string(),
                });
                None
            }
        }
    }

    /// A keyword comma list `key: <word>, <word>, …;` — rule 3's
    /// keyword clause: non-empty (an empty entry is diagnosed), bare
    /// words only, each a member of `allowed`. Items split at depth-0
    /// `,` under the capture scanner's skip set, so wrapped lists and
    /// trivia between items read fine (rule 3's comment model);
    /// duplicates ride through (SurrealQL parity — the linter flags
    /// them). Every bad entry is diagnosed; one bad entry fails
    /// the whole clause.
    fn parse_word_list_clause(
        &mut self,
        key_span: Span,
        key: &'static str,
        allowed: &'static [&'static str],
    ) -> Option<Vec<String>> {
        self.parse_item_list(key_span, key, |this, item| {
            let text = item.slice(this.src);
            match sole_bare_word(text) {
                Some(word) if allowed.contains(&word) => Some(word.to_string()),
                Some(word) => {
                    let suggestion = did_you_mean(word, allowed);
                    this.diags.push(Diag::UnknownClauseWord {
                        span: item,
                        word: word.to_string(),
                        key,
                        suggestion,
                    });
                    None
                }
                None => {
                    this.diags.push(Diag::UnknownClauseWord {
                        span: item,
                        word: text.trim().to_string(),
                        key,
                        suggestion: None,
                    });
                    None
                }
            }
        })
    }

    /// A string comma list `key: '<s>', '<s>', …;` — the string
    /// literal form of [`ClauseKind::WordList`]'s shape: items split
    /// at depth-0 `,` under the capture scanner's skip set, each an
    /// SDL-lexed string literal decoded to its value (rule 3). Every
    /// bad item is diagnosed; one bad item fails the whole clause.
    fn parse_str_list_clause(&mut self, key_span: Span, key: &'static str) -> Option<Vec<String>> {
        self.parse_item_list(key_span, key, |this, item| {
            let text = item.slice(this.src);
            match parse_string_literal_text(text) {
                Some(value) => Some(value),
                None => {
                    this.diags.push(Diag::InvalidString {
                        span: item,
                        text: text.trim().to_string(),
                    });
                    None
                }
            }
        })
    }

    /// A filter list `key: <filter>, …;` — the word-list shape with
    /// per-entry argument grammar ([`parse_filter_atom`]). Parens are
    /// safe inside items: the depth-0 split lowers them (a comma
    /// inside `edgengram(2, 10)` never splits).
    fn parse_filter_list_clause(
        &mut self,
        key_span: Span,
        key: &'static str,
    ) -> Option<Vec<Filter>> {
        self.parse_item_list(key_span, key, |this, item| {
            let text = item.slice(this.src);
            match parse_filter_atom(text) {
                Some(filter) => Some(filter),
                None => {
                    this.diags.push(Diag::InvalidFilter {
                        span: item,
                        text: text.trim().to_string(),
                    });
                    None
                }
            }
        })
    }

    /// A `<fn-path>` clause `key: fn::<seg>[::<seg>…];` — rule 3's
    /// name-path kind, never a call. Validated over a sub-scanner on
    /// the capture text (segments are ordinary identifiers, quoted
    /// spellings included) under the capture-trivia model — all four
    /// SurrealQL comment syntaxes, like the other keyword clauses;
    /// diagnostics anchor the whole capture, like a duration's.
    fn parse_fn_path_clause(&mut self, key_span: Span, key: &'static str) -> Option<FnPath> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        let mut scan = Scanner::new(text);
        scan.skip_surrealql_trivia();
        let bad = |message: String| Diag::InvalidFnPath { span, message };
        // The prefix is a BARE keyword (`fn` or `mod`) — a quoted
        // spelling is an identifier, not the keyword (rule 9).
        let kind = match scan.read_ident() {
            Ok(Some((_, word, false))) => match word.as_str() {
                "fn" => FnKind::Fn,
                "mod" => FnKind::Mod,
                "ml" => {
                    self.diags.push(bad(
                        "`ml::` paths cannot be used in a `function` clause (upstream: \
                         model functions are not analyzer functions)"
                            .to_string(),
                    ));
                    return None;
                }
                "silo" => {
                    self.diags.push(bad(
                        "`silo::` paths cannot be used in a `function` clause (upstream: \
                         silo functions are not analyzer functions)"
                            .to_string(),
                    ));
                    return None;
                }
                _ => {
                    self.diags
                        .push(bad("expected the `fn::` or `mod::` prefix".to_string()));
                    return None;
                }
            },
            _ => {
                self.diags
                    .push(bad("expected the `fn::` or `mod::` prefix".to_string()));
                return None;
            }
        };
        // `read_path_segments` shares the segment rule with the other
        // name paths (and leaves a lone `:` for the trailing-junk
        // check below); an empty list means no `::` followed the
        // prefix at all.
        let segments = match read_path_segments(&mut scan) {
            Some(segments) if !segments.is_empty() => segments,
            Some(_) => {
                self.diags
                    .push(bad("expected `::<segment>` after the prefix".to_string()));
                return None;
            }
            None => {
                self.diags
                    .push(bad("expected a path segment after `::`".to_string()));
                return None;
            }
        };
        // A trailing `(` is the call shape upstream rejects with its
        // own message; anything else non-trivia is malformed.
        scan.skip_surrealql_trivia();
        if scan.peek_byte() == Some(b'(') {
            self.diags.push(bad(
                "`(…)` is a call — the clause takes a name path (remove the parens)".to_string(),
            ));
            return None;
        }
        if !scan.is_at_end() {
            self.diags.push(bad(
                "expected `::`, the call-rejecting `(`, or the clause's `;`".to_string(),
            ));
            return None;
        }
        Some(FnPath { kind, segments })
    }

    /// The shared shape of the rule-3 comma-list clauses (word
    /// lists, filter lists, expression lists): [`Self::capture_list_items`]
    /// makes the split, then `item` parses each entry — pushing its
    /// own diagnostic on failure. Every bad entry is attempted (a
    /// multi-entry list reports each of them); one bad entry fails
    /// the whole clause.
    fn parse_item_list<T>(
        &mut self,
        key_span: Span,
        key: &'static str,
        mut item: impl FnMut(&mut Self, Span) -> Option<T>,
    ) -> Option<Vec<T>> {
        let items = self.capture_list_items(key_span, key)?;
        let mut parsed = Vec::with_capacity(items.len());
        let mut ok = true;
        for span in items {
            match item(self, span) {
                Some(value) => parsed.push(value),
                None => ok = false,
            }
        }
        ok.then_some(parsed)
    }

    /// An expression comma list `key: <expr>, <expr>, …;` — rule 3's
    /// comma-list clause in its expression form (event `then:` is
    /// the first). Items split at depth-0 `,` by
    /// [`Self::capture_list_items`] — a braced block with internal
    /// commas stays one item — and each canonicalizes separately
    /// through the embedded parser. Every bad item is diagnosed; one
    /// bad item fails the whole clause, like a word list's.
    fn parse_expr_list_clause(
        &mut self,
        key_span: Span,
        key: &'static str,
        slot: Slot,
    ) -> Option<Vec<Expr>> {
        self.parse_item_list(key_span, key, |this, span| {
            this.canonicalize_capture(span, slot)
        })
    }

    /// An identifier clause `key: <ident>;` — rule 3's keyword
    /// clause: one bare word or quoted identifier, validated over a
    /// sub-scanner on the capture text (the [`parse_fn_path_clause`]
    /// shape, whose trivia and quoting rules it shares — the
    /// scanner's capture-trivia model, all four SurrealQL comment
    /// syntaxes). A bare reserved word is rejected exactly as in a
    /// name position (rule 9) — upstream's table position cannot
    /// take one either. `event`'s `on:` is the first; `index`'s
    /// `on:` follows later.
    fn parse_ident_clause(&mut self, key_span: Span, key: &'static str) -> Option<Ident> {
        let span = self.capture_value(key_span, key)?;
        self.parse_ident_span(span)
    }

    /// Validates one identifier-shaped span (a bare word or quoted
    /// identifier) over a sub-scanner on its text — the
    /// [`parse_ident_clause`] body shared with the ident list
    /// (graphql `tables { include: …; }`). A bare reserved word is
    /// rejected exactly as in a name position (rule 9); the label is
    /// re-anchored into file coordinates.
    fn parse_ident_span(&mut self, span: Span) -> Option<Ident> {
        let text = span.slice(self.src);
        let mut scan = Scanner::new(text);
        scan.skip_surrealql_trivia();
        let bad = |text: &str| Diag::InvalidIdent {
            span,
            text: text.trim().to_string(),
        };
        match scan.read_ident() {
            Ok(Some((ident_span, word, quoted))) => {
                // Rule 9, as in a name position: a *bare* reserved
                // word cannot serve; quoted spellings are always
                // legal.
                if !quoted && is_reserved(&word) {
                    self.diags.push(Diag::ReservedIdent {
                        span: Span::new(span.offset + ident_span.offset, ident_span.len),
                        word,
                    });
                    return None;
                }
                scan.skip_surrealql_trivia();
                if scan.is_at_end() {
                    return Some(Ident::new(word));
                }
                self.diags.push(bad(text));
                None
            }
            _ => {
                self.diags.push(bad(text));
                None
            }
        }
    }

    /// An identifier comma list `key: <ident>, <ident>, …;` — the
    /// ident clause per entry over the shared item split (graphql
    /// `tables { include: …; }`; non-empty like every list).
    fn parse_ident_list_clause(&mut self, key_span: Span, key: &'static str) -> Option<Vec<Ident>> {
        self.parse_item_list(key_span, key, |this, item| this.parse_ident_span(item))
    }

    /// A `fn::<seg>` comma list `key: fn::<seg>, …;` — the fn-path
    /// clause per entry over the shared item split, with the `fn::`
    /// prefix REQUIRED: upstream's graphql function position
    /// (`parse_custom_function_name`) takes no bare or `mod::`
    /// spelling (it stores the stripped bare name and re-prefixed
    /// print).
    fn parse_fn_path_list_clause(
        &mut self,
        key_span: Span,
        key: &'static str,
    ) -> Option<Vec<FnPath>> {
        self.parse_item_list(key_span, key, |this, item| {
            let text = item.slice(this.src);
            let mut scan = Scanner::new(text);
            scan.skip_surrealql_trivia();
            let bad = |message: String| Diag::InvalidFnPath {
                span: item,
                message,
            };
            match scan.read_ident() {
                // The prefix is a BARE keyword — a quoted spelling is
                // an identifier, not the keyword (rule 9).
                Ok(Some((_, word, false))) if word == "fn" => {}
                _ => {
                    this.diags.push(bad(
                        "expected the `fn::` prefix (graphql function lists take \
                         user-defined functions only)"
                            .to_string(),
                    ));
                    return None;
                }
            }
            scan.skip_surrealql_trivia();
            // `read_path_segments` consumes the `::` itself: an empty
            // list means none followed the prefix.
            let segments = match read_path_segments(&mut scan) {
                Some(segments) if !segments.is_empty() => segments,
                Some(_) => {
                    this.diags.push(bad("expected `::` after `fn`".to_string()));
                    return None;
                }
                None => {
                    this.diags
                        .push(bad("expected a path segment after `::`".to_string()));
                    return None;
                }
            };
            scan.skip_surrealql_trivia();
            if scan.peek_byte() == Some(b'(') {
                this.diags.push(bad(
                    "`(…)` is a call — the list takes a name path (remove the parens)".to_string(),
                ));
                return None;
            }
            if !scan.is_at_end() {
                this.diags
                    .push(bad("expected `,` or the clause's `;`".to_string()));
                return None;
            }
            Some(FnPath {
                kind: FnKind::Fn,
                segments,
            })
        })
    }

    /// A u32 whole-number clause `key: <number>;` — graphql's
    /// `depth:`/`complexity:`, upstream's exact types
    /// (`next_token_value::<u32>`); the parse is the u16 clause's
    /// over the wider range.
    fn parse_number32_clause(&mut self, key_span: Span, key: &'static str) -> Option<u32> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        match numeric_arg(text) {
            Some((value, rest)) if strip_capture_trivia(rest).is_some_and(str::is_empty) => {
                Some(value)
            }
            _ => {
                self.diags.push(Diag::InvalidNumber {
                    span,
                    text: text.trim().to_string(),
                    range: U32_RANGE,
                });
                None
            }
        }
    }

    /// A single-word clause `key: <word>;` from a fixed set — the
    /// singleton sibling of the word list (graphql's
    /// `introspection:`).
    fn parse_word_set_clause(
        &mut self,
        key_span: Span,
        key: &'static str,
        allowed: &'static [&'static str],
    ) -> Option<String> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        match sole_bare_word(text) {
            Some(word) if allowed.contains(&word) => Some(word.to_string()),
            Some(word) => {
                let suggestion = did_you_mean(word, allowed);
                self.diags.push(Diag::UnknownClauseWord {
                    span,
                    word: word.to_string(),
                    key,
                    suggestion,
                });
                None
            }
            None => {
                self.diags.push(Diag::UnknownClauseWord {
                    span,
                    word: text.trim().to_string(),
                    key,
                    suggestion: None,
                });
                None
            }
        }
    }

    /// A word-or-expression clause `key: <word> | <expr>;` — one bare
    /// word of the fixed set, or anything else as an embedded
    /// expression (field reference's `on delete:`: the engine itself
    /// runs non-enum values as `ON DELETE THEN <expr>`, so there is no
    /// did-you-mean on the word set — a misspelled action word is a
    /// legal expression by the grammar's own reading, spec/field.md).
    fn parse_word_or_expr_clause(
        &mut self,
        key_span: Span,
        key: &'static str,
        allowed: &'static [&'static str],
        slot: Slot,
    ) -> Option<ClauseValue> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        match sole_bare_word(text) {
            Some(word) if allowed.contains(&word) => Some(ClauseValue::Word(word.to_string())),
            // An UPPERCASE spelling of a known action word is a
            // misspelled keyword (rule 9), not a THEN expression —
            // reading `REJECT` as the THEN idiom would silently
            // change the meaning.
            Some(word) if allowed.contains(&word.to_ascii_lowercase().as_str()) => {
                let suggestion = did_you_mean(word, allowed);
                self.diags.push(Diag::UnknownClauseWord {
                    span,
                    word: word.to_string(),
                    key,
                    suggestion,
                });
                None
            }
            _ => self.canonicalize_capture(span, slot).map(ClauseValue::Expr),
        }
    }

    /// The keyword/sub-block duality (spec/config.md): `tables: auto;`
    /// selects the bare family word, `tables { include: …; };` the
    /// structured one. The separator picks the form — `:` reads a
    /// word of the set, `{` opens the sub-block; anything else
    /// diagnoses both spellings.
    fn parse_word_or_sub_block_clause(
        &mut self,
        key_span: Span,
        key: &'static str,
        words: &'static [&'static str],
        table: &'static [(&'static str, ClauseKind)],
    ) -> Option<ClauseValue> {
        self.take_comments();
        match self.scan.peek_byte() {
            Some(b':') => self
                .parse_word_set_clause(key_span, key, words)
                .map(ClauseValue::Word),
            Some(b'{') => self.parse_sub_block_clause(key_span, key, table),
            _ => {
                self.diags.push(Diag::UnexpectedClauseForm {
                    span: key_span,
                    message: format!(
                        "clause `{key}` takes a word or a sub-block — write \
                         `{key}: auto;`, `{key}: none;`, or `{key} {{ include: …; }};`"
                    ),
                });
                self.recover_to_clause_end();
                None
            }
        }
    }

    /// A bm25 clause `key: (<k1>, <b>);` — rule 3's keyword-clause
    /// kind, SDL-lexed over the capture (never an expression capture;
    /// spec/index.md): the parens are mandatory — upstream's bare
    /// `BM25` spelling IS the default pair, which SDL expresses by
    /// omitting the clause (rule 6) — and exactly two decimal sides
    /// are taken, split at depth-0 `,` under the capture scanner's
    /// skip set. Values store as f32, upstream's `Scoring` storage.
    /// Diagnostics anchor the whole capture, like a duration's.
    fn parse_bm25_clause(&mut self, key_span: Span, key: &'static str) -> Option<Bm25> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        let bad = |message: String| Diag::InvalidBm25 { span, message };
        let Some(stripped) = strip_capture_trivia(text) else {
            // Unreachable through the capture scanner (an unterminated
            // block comment dies at capture time); defensive.
            self.diags
                .push(bad("unterminated block comment".to_string()));
            return None;
        };
        if !stripped.starts_with('(') {
            self.diags.push(bad(
                "the parens are mandatory — write `bm25: (1.2, 0.75);`".to_string()
            ));
            return None;
        }
        let mut scan = Scanner::new(stripped);
        // Entries arrive bracket-balanced (the capture scanner ran the
        // parens), so a capture error here is defensive.
        let interior = match scan.capture_balanced_parens() {
            Ok(interior) => interior,
            Err(_) => {
                self.diags
                    .push(bad("unbalanced `(` — the pair never closes".to_string()));
                return None;
            }
        };
        let interior_text = interior.slice(stripped);
        let mut sides: Vec<Span> = Vec::new();
        let mut side_scan = Scanner::new(interior_text);
        loop {
            let start = side_scan.offset();
            match side_scan.capture_until(b",") {
                Ok((side, _)) => sides.push(side),
                Err(Diag::UnterminatedCapture { .. }) => {
                    // The final side rode to its end.
                    sides.push(Span::new(start, interior_text.len() - start));
                    break;
                }
                Err(_) => unreachable!("pair interiors are bracket-balanced"),
            }
            if side_scan.is_at_end() {
                break;
            }
        }
        if sides.len() != 2 {
            self.diags.push(bad(
                "expected exactly two numbers in the pair — `bm25: (<k1>, <b>);`".to_string(),
            ));
            return None;
        }
        let mut values = [0.0_f64; 2];
        for (side, value) in sides.iter().zip(values.iter_mut()) {
            match float_arg(side.slice(interior_text)) {
                Some(parsed) => *value = parsed,
                None => {
                    let text = side.slice(interior_text).trim();
                    self.diags.push(bad(format!("`{text}` is not a number")));
                    return None;
                }
            }
        }
        scan.skip_surrealql_trivia();
        if !scan.is_at_end() {
            self.diags.push(bad(
                "expected only trivia between the pair and the clause's `;`".to_string(),
            ));
            return None;
        }
        Some(Bm25(values[0] as f32, values[1] as f32))
    }

    /// A float clause `key: <float>;` — rule 3's keyword-clause kind,
    /// SDL-lexed (index's `lm:`/`alpha:`; minkowski's order rides the
    /// distance clause): the SurrealQL float token shape minus the
    /// constants, per [`float_arg`]. The AST carries the value, never
    /// the spelling.
    fn parse_float_clause(&mut self, key_span: Span, key: &'static str) -> Option<Float> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        match float_arg(text) {
            Some(value) => Some(Float(value)),
            None => {
                self.diags.push(Diag::InvalidFloat {
                    span,
                    text: text.trim().to_string(),
                });
                None
            }
        }
    }

    /// A distance clause `key: <distance>[(<order>)];` — rule 3's
    /// keyword-clause kind (spec/index.md): one bare lowercase keyword
    /// of the ten-word set (rule 9), `minkowski` taking a
    /// parenthesized order — SurrealQL spells the order without parens
    /// (`DISTANCE MINKOWSKI 3`); SDL's paren form is consistent with
    /// `bm25:` and codegen emits the bare form. Diagnostics anchor the
    /// whole capture, like a duration's.
    fn parse_distance_clause(&mut self, key_span: Span, key: &'static str) -> Option<Distance> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        let bad = |message: String| Diag::InvalidDistance { span, message };
        let Some(stripped) = strip_capture_trivia(text) else {
            self.diags
                .push(bad("unterminated block comment".to_string()));
            return None;
        };
        let len = word_prefix_len(stripped);
        let word = &stripped[..len];
        if word.is_empty() || !word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            self.diags
                .push(bad("expected a distance keyword".to_string()));
            return None;
        }
        if !DISTANCE_WORDS.contains(&word) {
            let message = match did_you_mean(word, DISTANCE_WORDS) {
                Some(s) => format!("unknown distance `{word}` — did you mean `{s}`?"),
                None => format!("unknown distance `{word}`"),
            };
            self.diags.push(bad(message));
            return None;
        }
        if word == "minkowski" {
            let after = &stripped[len..];
            let mut scan = Scanner::new(after);
            scan.skip_surrealql_trivia();
            if scan.peek_byte() != Some(b'(') {
                self.diags.push(bad(
                    "minkowski takes an order — write `distance: minkowski(3);`".to_string(),
                ));
                return None;
            }
            let interior = match scan.capture_balanced_parens() {
                Ok(interior) => interior,
                Err(_) => {
                    self.diags
                        .push(bad("unbalanced `(` — the order never closes".to_string()));
                    return None;
                }
            };
            let order = match float_arg(interior.slice(after)) {
                Some(order) => order,
                None => {
                    self.diags
                        .push(bad("the order must be one number".to_string()));
                    return None;
                }
            };
            scan.skip_surrealql_trivia();
            if !scan.is_at_end() {
                self.diags.push(bad(
                    "expected only trivia between the order and the clause's `;`".to_string(),
                ));
                return None;
            }
            return Some(Distance::Minkowski(Float(order)));
        }
        // A no-argument distance: only trivia may follow the keyword.
        let after = &stripped[len..];
        let Some(after) = strip_capture_trivia(after) else {
            self.diags
                .push(bad("unterminated block comment".to_string()));
            return None;
        };
        if !after.is_empty() {
            if after.starts_with('(') {
                self.diags.push(bad(format!("`{word}` takes no argument")));
            } else {
                self.diags.push(bad(
                    "expected only trivia between the distance and the clause's `;`".to_string(),
                ));
            }
            return None;
        }
        distance_of(word)
    }

    /// `middleware: <call>, <call>, …;` — api's hybrid list
    /// (spec/config.md): entries split at depth-0 `,` (argument parens
    /// raise depth, like every comma list), each SDL-lexed as a name
    /// path plus its parenthesized arguments.
    fn parse_middleware_list_clause(
        &mut self,
        key_span: Span,
        key: &'static str,
    ) -> Option<Vec<Middleware>> {
        self.parse_item_list(key_span, key, |this, item| {
            this.parse_middleware_entry(item)
        })
    }

    /// One middleware entry: `<head>(<args>)` — the head is a name
    /// path (bare, `fn::`, or `mod::`; `silo::`/`ml::` are outside
    /// SDL's surface, `raw { … }` is the escape hatch), the parens
    /// are required (upstream `expected!(t!("("))`), and the interior
    /// splits at depth-0 `,` into embedded-expression arguments
    /// (`()` parses to none — upstream accepts the empty list).
    fn parse_middleware_entry(&mut self, span: Span) -> Option<Middleware> {
        let text = span.slice(self.src);
        let mut scan = Scanner::new(text);
        scan.skip_surrealql_trivia();
        let bad = |message: String| Diag::InvalidMiddleware { span, message };
        let (head, quoted) = match scan.read_ident() {
            Ok(Some((_, word, quoted))) => (word, quoted),
            Ok(None) | Err(_) => {
                self.diags
                    .push(bad("expected the middleware name".to_string()));
                return None;
            }
        };
        // `fn`/`mod` are BARE keywords only (rule 9); a quoted `fn`
        // is an ordinary head name, exactly upstream's
        // identifier-shaped `Function::Normal` path.
        let kind = if !quoted {
            match head.as_str() {
                "fn" => FnKind::Fn,
                "mod" => FnKind::Mod,
                "silo" | "ml" => {
                    self.diags.push(bad(format!(
                        "`{head}::` middleware is outside SDL's surface — SurrealQL \
                         accepts it; route the definition through `raw {{ … }}`"
                    )));
                    return None;
                }
                _ => FnKind::Bare,
            }
        } else {
            FnKind::Bare
        };
        // `read_path_segments` consumes each `::` itself: an empty
        // list means none followed the head. The head word joins the
        // segments only for a BARE head — for `fn`/`mod` it IS the
        // prefix, never a segment.
        let rest = match read_path_segments(&mut scan) {
            Some(rest) => rest,
            None => {
                self.diags
                    .push(bad("expected a path segment after `::`".to_string()));
                return None;
            }
        };
        if matches!(kind, FnKind::Fn | FnKind::Mod) && rest.is_empty() {
            self.diags.push(bad(format!(
                "expected `::` after `{head}` (the prefix is not spellable alone)"
            )));
            return None;
        }
        let mut segments = Vec::new();
        if kind == FnKind::Bare {
            segments.push(head);
        }
        segments.extend(rest);
        scan.skip_surrealql_trivia();
        if scan.peek_byte() != Some(b'(') {
            // A stray `:` right after the path is the one shape worth
            // naming: the user almost certainly meant `::`.
            if scan.peek_byte() == Some(b':') {
                self.diags.push(bad(
                    "a lone `:` cannot join path segments — use `::`".to_string()
                ));
            } else {
                self.diags.push(bad(
                    "expected `(` with the middleware's arguments — write `name()` \
                     when there are none"
                        .to_string(),
                ));
            }
            return None;
        }
        // Entries arrive bracket-balanced (the depth-0 split only
        // cuts at depth 0), so a capture error here is defensive —
        // the entry-relative span of a lexer diagnostic is not worth
        // translating for a shape that cannot occur.
        let interior = match scan.capture_balanced_parens() {
            Ok(span) => span,
            Err(_) => {
                self.diags.push(bad(
                    "unbalanced `(` — the argument list never closes".to_string()
                ));
                return None;
            }
        };
        let mut args = Vec::new();
        let mut ok = true;
        let interior_text = interior.slice(text);
        if !strip_capture_trivia(interior_text).is_some_and(str::is_empty) {
            let mut arg_scan = Scanner::new(interior_text);
            loop {
                let start = arg_scan.offset();
                match arg_scan.capture_until(b",") {
                    Ok((item, _)) => {
                        match self.canonicalize_capture(
                            Span::new(span.offset + interior.offset + item.offset, item.len),
                            Slot::ConfigMiddlewareArg,
                        ) {
                            Some(arg) => args.push(arg),
                            None => ok = false,
                        }
                    }
                    Err(Diag::UnterminatedCapture { .. }) => {
                        // The interior's final item rode to its end.
                        let item = Span::new(
                            span.offset + interior.offset + start,
                            interior_text.len() - start,
                        );
                        match self.canonicalize_capture(item, Slot::ConfigMiddlewareArg) {
                            Some(arg) => args.push(arg),
                            None => ok = false,
                        }
                        break;
                    }
                    Err(_) => unreachable!("entry interiors are bracket-balanced"),
                }
                if arg_scan.is_at_end() {
                    break;
                }
            }
        }
        scan.skip_surrealql_trivia();
        if !scan.is_at_end() {
            self.diags
                .push(bad("unexpected text after the middleware call".to_string()));
            return None;
        }
        // A failed argument already carried its own diagnostic. A
        // trailing `,` rides like upstream's argument loop (the final
        // empty item resolves to no argument — `a(1,)` ≡ `a(1)`); an
        // empty item MID-LIST rejects through the bridge's
        // empty-capture check on both sides.
        ok.then_some(Middleware {
            path: FnPath { kind, segments },
            args,
        })
    }

    /// A whole-number clause `key: <number>;` — rule 3's keyword
    /// clause: the SurrealQL integer shape (digits with `_`
    /// separators, no sign or fraction) via [`numeric_arg`], u16 —
    /// upstream's exact type for the `async` parameters. Overflow
    /// and every other spelling diagnose; the AST carries the
    /// value, never the spelling.
    fn parse_number_clause(&mut self, key_span: Span, key: &'static str) -> Option<u16> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        match numeric_arg(text) {
            // numeric_arg stops at the first non-digit run; the
            // clause takes exactly one number, so the trivia-stripped
            // rest must be empty (`5s`, `5,` diagnose; `5 /* n */`
            // reads).
            Some((value, rest)) if strip_capture_trivia(rest).is_some_and(str::is_empty) => {
                Some(value)
            }
            _ => {
                self.diags.push(Diag::InvalidNumber {
                    span,
                    text: text.trim().to_string(),
                    range: U16_RANGE,
                });
                None
            }
        }
    }

    /// A module executable clause `from: (f'…' | silo::…);` — rule 3's
    /// keyword-clause kind, SDL-lexed over the capture (never an
    /// expression capture; spec/module.md): an f-string literal whose
    /// interior must be a file string, or the silo package path in its
    /// SDL (documented 3.3) spelling. Diagnostics anchor the whole
    /// capture, like a duration's.
    fn parse_executable_clause(&mut self, key_span: Span, key: &'static str) -> Option<Executable> {
        let span = self.capture_value(key_span, key)?;
        let text = span.slice(self.src);
        let bad = |message: String| Diag::InvalidExecutable { span, message };
        let Some(stripped) = strip_capture_trivia(text) else {
            // Unreachable through the capture scanner (an unterminated
            // block comment dies at capture time); defensive.
            self.diags
                .push(bad("unterminated block comment".to_string()));
            return None;
        };
        // File arm: `f'…'` / `f"…"` glued, like upstream's lexer (a
        // space after `f` makes it an identifier plus a plain string,
        // which is not an executable). Only lowercase prefixes — SDL
        // keywords are lowercase only (rule 9).
        if stripped.starts_with("f'") || stripped.starts_with("f\"") {
            let Some((value, after)) = lex_string_literal(&stripped[1..]) else {
                self.diags.push(bad(
                    "malformed f-string — unterminated quote or an invalid `\\` escape".to_string(),
                ));
                return None;
            };
            if !strip_capture_trivia(after).is_some_and(str::is_empty) {
                self.diags.push(bad(
                    "expected only trivia between the f-string and the clause's `;`".to_string(),
                ));
                return None;
            }
            return match lex_file_string(&value) {
                Some((bucket, key)) => Some(Executable::File { bucket, key }),
                None => {
                    self.diags.push(bad(format!(
                        "`{value}` is not a file string — expected `bucket:/key` \
                         (buckets allow alphanumerics and `_`, `-`, `.`; keys \
                         additionally `/`, never `:`)"
                    )));
                    None
                }
            };
        }
        // Silo arm: `silo::<org>::<pkg>::<maj.min.patch>` — scanned on
        // a sub-scanner like a fn-path (quoted segments, escapes
        // decoded), the version tail over the remaining text.
        let mut scan = Scanner::new(stripped);
        scan.skip_surrealql_trivia();
        // The prefix is a BARE keyword (`silo`) — a quoted spelling is
        // an identifier, not the keyword (rule 9, fn-path par).
        match scan.read_ident() {
            Ok(Some((_, word, false))) if word == "silo" => {}
            _ => {
                self.diags.push(bad(
                    "expected an f-string (`f'…'`) or the `silo::` prefix".to_string()
                ));
                return None;
            }
        }
        fn eat_punct(scan: &mut Scanner<'_>, bytes: &[u8]) -> bool {
            scan.skip_surrealql_trivia();
            bytes.iter().all(|b| scan.eat_byte(*b))
        }
        fn read_segment(scan: &mut Scanner<'_>) -> Option<String> {
            scan.skip_surrealql_trivia();
            let (_, word, _quoted) = scan.read_ident().ok().flatten()?;
            Some(word)
        }
        if !eat_punct(&mut scan, b"::") {
            self.diags
                .push(bad("expected `::` after `silo`".to_string()));
            return None;
        }
        let Some(org) = read_segment(&mut scan) else {
            self.diags
                .push(bad("expected the organisation name".to_string()));
            return None;
        };
        if !eat_punct(&mut scan, b"::") {
            self.diags
                .push(bad("expected `::` before the package name".to_string()));
            return None;
        }
        let Some(pkg) = read_segment(&mut scan) else {
            self.diags
                .push(bad("expected the package name".to_string()));
            return None;
        };
        if !eat_punct(&mut scan, b"::") {
            self.diags.push(bad(
                "expected `::<version>` after the package name — SDL's silo spelling, \
                 e.g. `silo::org::pkg::<1.2.0>`"
                    .to_string(),
            ));
            return None;
        }
        let (major, minor, patch) = match parse_version_triple(&stripped[scan.offset()..]) {
            Some(version) => version,
            None => {
                self.diags.push(bad(
                    "expected `<major>.<minor>.<patch>` — three whole numbers, \
                     e.g. `::<1.2.0>`"
                        .to_string(),
                ));
                return None;
            }
        };
        Some(Executable::Silo {
            org,
            pkg,
            major,
            minor,
            patch,
        })
    }

    /// Splits a value clause's list into depth-0 `,`-separated item
    /// spans (rule 3's comma-list clauses): the `:` is consumed first
    /// like [`Self::capture_value`], then each item runs to the next
    /// `,` or the `;` at bracket depth 0 — under the capture scanner's
    /// full skip set, so wrapped lists, trivia, and parens in items
    /// (`snowball(english)`) never split mid-entry. The final item
    /// ends at the `;` (consumed), or EOF on the file's final clause.
    /// Always yields at least one item (possibly empty — the caller's
    /// grammar diagnoses emptiness).
    fn capture_list_items(&mut self, key_span: Span, key: &'static str) -> Option<Vec<Span>> {
        self.take_comments();
        if !self.scan.eat_byte(b':') {
            self.diags.push(Diag::ExpectedClauseValue {
                span: key_span,
                key,
            });
            self.recover_to_clause_end();
            return None;
        }
        let mut items = Vec::new();
        loop {
            let start = self.scan.offset();
            match self.scan.capture_until(b",;") {
                Ok((span, terminator)) => {
                    items.push(span);
                    if terminator == b';' {
                        return Some(items);
                    }
                }
                Err(Diag::UnterminatedCapture { .. }) => {
                    items.push(Span::new(start, self.src.len() - start));
                    return Some(items);
                }
                Err(diag) => {
                    self.diags.push(diag);
                    return None;
                }
            }
        }
    }

    /// A `///` line inside a clause block, framed as the unknown
    /// clause it is (rule 5 attaches docs above definitions, never
    /// inside blocks).
    fn push_misplaced_doc_clause(&mut self, statement: &'static str) {
        let span = Span::new(self.scan.offset(), 3);
        self.diags.push(Diag::UnknownClause {
            span,
            key: "///…".to_string(),
            statement,
            suggestion: None,
        });
    }

    /// Reads a statement's name position: an identifier in any of
    /// SurrealQL's three forms, rejecting bare reserved words
    /// (rule 9). On failure the diagnostic is pushed, recovery skips
    /// to statement end, and `None` comes back.
    fn read_statement_name(&mut self) -> Option<Ident> {
        self.read_statement_name_in_block(false)
    }

    /// The name position's read, with the recovery depth selected by
    /// `in_block`: a TOP-LEVEL statement's failure recovers to
    /// statement end, but a statement running as a NESTED head (a
    /// `table` block's `index`/`event`) must not overrun its
    /// enclosing block — its recovery stops at the clause boundary,
    /// leaving a stray `}` unconsumed so the host's clause loop can
    /// close the block and the file's remaining statements survive
    /// (the "one typo never blanks the rest" contract).
    fn read_statement_name_in_block(&mut self, in_block: bool) -> Option<Ident> {
        self.take_comments();
        let name_start = self.scan.offset();
        let (name_span, raw, quoted) = match self.scan.read_ident() {
            Ok(Some(ident)) => ident,
            Ok(None) => {
                // Nothing identifier-shaped here — `;`, junk, or EOF;
                // the position demands a name either way.
                self.diags.push(Diag::ExpectedName {
                    span: Span::point(name_start),
                });
                self.recover_name_failure(in_block);
                return None;
            }
            Err(diag) => {
                self.diags.push(diag);
                self.recover_name_failure(in_block);
                return None;
            }
        };
        // Rule 9: identifiers follow SurrealQL's rules — a *bare*
        // reserved word cannot be a name (SDL is conservatively
        // stricter than upstream's own name position, which accepts
        // some reserved words bare; SDL requires backticks for the
        // whole set upstream defines as "might be disallowed").
        if !quoted && is_reserved(&raw) {
            self.diags.push(Diag::ReservedIdent {
                span: name_span,
                word: raw,
            });
            self.recover_name_failure(in_block);
            return None;
        }
        Some(Ident::new(raw))
    }

    /// A name-position failure's recovery: statement end, unless the
    /// statement runs nested inside a block (see
    /// [`Self::read_statement_name_in_block`]).
    fn recover_name_failure(&mut self, in_block: bool) {
        if in_block {
            self.recover_to_clause_end();
        } else {
            self.recover_to_statement_end();
        }
    }

    /// After a clause block closes: upstream requires `;` between
    /// statements; only the file's final statement may omit it
    /// (probe-verified against v3.2.4 `syn`). A statement that parsed
    /// is kept either way — the driver is already positioned at the
    /// next one, so recovery must not skip.
    fn check_block_terminator(&mut self) {
        self.take_comments();
        if self.scan.eat_byte(b';') || self.scan.is_at_end() {
            return;
        }
        self.diags.push(Diag::ExpectedStatementEnd {
            span: Span::point(self.scan.offset()),
            found: self.cursor_char(),
        });
    }

    /// Skips trivia, collecting any plain `//` comments at the cursor
    /// into the shared pending buffer (rule 5's layout preservation).
    /// The drop-in replacement for a bare trivia skip at every
    /// between-token position: a `//` there is legal input, and
    /// letting it stop the skip unconsumed would break the parse.
    /// A `///` doc block still stops the skip for the loop's own
    /// doc-block handling.
    fn take_comments(&mut self) {
        loop {
            self.scan.skip_trivia();
            if self.scan.at_plain_comment() {
                let line = crate::lexer::line_col(self.src, self.scan.offset()).0;
                let text = self.scan.take_comment();
                self.comments.push(PendingComment { line, text });
            } else {
                return;
            }
        }
    }

    /// The anchor handoff: everything pending becomes the statement's
    /// leading comments (entry point of every statement parse).
    fn take_pending_comments(&mut self) -> Vec<String> {
        self.comments
            .drain(..)
            .map(|comment| comment.text)
            .collect()
    }

    /// The same-line after-note handoff: the first pending comment,
    /// when it sits on `line` (a terminator's line), leaves the
    /// buffer as that terminator's after-note. At most one comment
    /// can share a line outside captures (`//` runs to end of line),
    /// so a head check is exact.
    fn take_pending_same_line(&mut self, line: usize) -> Option<String> {
        if self
            .comments
            .first()
            .is_some_and(|comment| comment.line == line)
        {
            Some(self.comments.remove(0).text)
        } else {
            None
        }
    }

    /// The 1-based source line at the cursor — a terminator's line,
    /// the anchor line the same-line split compares against.
    fn cursor_line(&self) -> usize {
        crate::lexer::line_col(self.src, self.scan.offset()).0
    }

    /// A terminator's same-line close note, WITHOUT disturbing the
    /// pending buffer: trivia is skipped only provisionally, and the
    /// cursor restores unless a `//` comment starts on `line`. A
    /// comment past that line belongs to whatever follows the
    /// terminator (the next statement's leading comments, the next
    /// clause's before-notes) — collecting it here would let the
    /// statement's own trailing drain steal it.
    fn peek_same_line_note(&mut self, line: usize) -> Option<String> {
        let saved = self.scan.offset();
        self.scan.skip_trivia();
        let same_line = self.scan.at_plain_comment()
            && crate::lexer::line_col(self.src, self.scan.offset()).0 == line;
        if same_line {
            Some(self.scan.take_comment())
        } else {
            self.scan.seek(saved);
            None
        }
    }

    /// Statement-level recovery rides the depth-0 `;` scanner: skip to
    /// the next `;` at file depth (consumed) or to EOF. Tolerant by
    /// design — recovery diagnostics are never reported, a stray
    /// closer is consumed and the skip resumes, and every capture
    /// error leaves the cursor at EOF — so it always terminates.
    fn recover_to_statement_end(&mut self) {
        loop {
            match self.scan.capture_until(b";") {
                Ok(_) => return,
                Err(Diag::UnbalancedCapture { .. }) => match self.scan.peek_byte() {
                    Some(closer @ (b')' | b']' | b'}')) => {
                        self.scan.eat_byte(closer);
                    }
                    _ => return,
                },
                Err(_) => return,
            }
        }
    }

    /// Clause-level recovery: as [`Self::recover_to_statement_end`],
    /// except a stray `}` at block depth ends the skip unconsumed so
    /// the clause loop can close the block; a stray `)` or `]` serves
    /// no clause and is consumed.
    fn recover_to_clause_end(&mut self) {
        loop {
            match self.scan.capture_until(b";") {
                Ok(_) => return,
                Err(Diag::UnbalancedCapture { .. }) => match self.scan.peek_byte() {
                    Some(closer @ (b')' | b']')) => {
                        self.scan.eat_byte(closer);
                    }
                    _ => return,
                },
                Err(_) => return,
            }
        }
    }

    fn cursor_char(&self) -> Option<char> {
        self.src[self.scan.offset()..].chars().next()
    }
}

/// Did-you-mean over a keyword table (strsim Jaro-Winkler): the
/// closest candidate scoring at least 0.75, else `None`. The order
/// pass's alias-reference findings share it.
pub(crate) fn did_you_mean(word: &str, candidates: &[&str]) -> Option<String> {
    // Candidates are canonical lowercase spellings; scoring the
    // lowercased input too means an uppercase or mixed-case misspelling
    // (`VALUE:`, `Namespace`) still gets a did-you-mean, while plain
    // lowercase inputs score identically either way.
    let lower = word.to_ascii_lowercase();
    candidates
        .iter()
        .filter_map(|candidate| {
            let score =
                strsim::jaro_winkler(word, candidate).max(strsim::jaro_winkler(&lower, candidate));
            (score >= 0.75).then_some((score, *candidate))
        })
        .max_by(|(a, _), (b, _)| a.partial_cmp(b).expect("scores are not NaN"))
        .map(|(_, candidate)| candidate.to_string())
}

/// Whether `word` is `candidate` spelled with ONE Damerau-Levenshtein
/// edit — an insertion, deletion, substitution, or adjacent
/// transposition — case-sensitively (callers gate case separately).
/// The typo guard behind the table body's bare-head detection: a
/// near-identical spelling of a clause key (`permisions`, `drop2`)
/// reads as the typo it almost certainly is, while merely SIMILAR
/// names (`id` vs `index`, `schema` vs `schemafull`) are legitimate
/// field names the spec promises work bare.
fn is_single_edit(word: &str, candidate: &str) -> bool {
    let a: Vec<char> = word.chars().collect();
    let b: Vec<char> = candidate.chars().collect();
    let (la, lb) = (a.len(), b.len());
    let diff = (la as i32 - lb as i32).abs();
    if diff > 1 {
        return false;
    }
    if la == lb {
        // Same length: one substitution, or one adjacent transposition.
        let edits: Vec<usize> = (0..la).filter(|&i| a[i] != b[i]).collect();
        match edits.as_slice() {
            [_] => true,
            [i, j] => i + 1 == *j && a[*i] == b[*j] && a[*j] == b[*i],
            _ => false,
        }
    } else {
        // One length apart: a single insertion or deletion — walk
        // both, skipping at most one mismatch position.
        let (short, long) = if la < lb { (&a, &b) } else { (&b, &a) };
        let mut skipped = false;
        let mut si = 0;
        for (li, lc) in long.iter().enumerate() {
            if si == short.len() {
                // Trailing insert: fine iff it is the one skip.
                return !skipped && li == long.len() - 1;
            }
            if *lc == short[si] {
                si += 1;
            } else if skipped {
                return false;
            } else {
                skipped = true;
            }
        }
        true
    }
}

/// The api method word's variant (spec/api.md's six-kind set) — the
/// lowercase canonical spellings only (rule 9).
fn api_method_of(word: &str) -> Option<ApiMethod> {
    match word {
        "get" => Some(ApiMethod::Get),
        "post" => Some(ApiMethod::Post),
        "put" => Some(ApiMethod::Put),
        "patch" => Some(ApiMethod::Patch),
        "delete" => Some(ApiMethod::Delete),
        "trace" => Some(ApiMethod::Trace),
        _ => None,
    }
}

/// One api sub-block's folded clauses (rule 7): the config pair plus
/// the `then` slot. `then` is `Some(Err(()))` when a capture failed —
/// its diagnostic was already pushed, and the caller drops the block
/// without a second report; `None` is a genuinely absent clause (the
/// required-`then` check fires on method groups only).
struct ApiGroupParts {
    config: ApiGroupConfig,
    then: Option<Result<Expr, ()>>,
}

fn fold_api_group(clauses: &[ParsedClause]) -> ApiGroupParts {
    let mut parts = ApiGroupParts {
        config: ApiGroupConfig::default(),
        then: None,
    };
    for clause in clauses {
        match (clause.name, &clause.value) {
            ("middleware", ClauseValue::Middleware(list)) => {
                parts.config.middleware = list.clone();
            }
            ("permissions", ClauseValue::Permission(permissions)) => {
                parts.config.permissions = Some(permissions.clone());
            }
            ("then", ClauseValue::Expr(expr)) => parts.then = Some(Ok(expr.clone())),
            ("then", ClauseValue::Failed) => parts.then = Some(Err(())),
            (_, ClauseValue::Failed) => {}
            _ => unreachable!("clause_loop yields table keys only"),
        }
    }
    parts
}

/// The fields policy an `index` kind applies to its field list
/// (spec/index.md's per-kind validation): plain/unique require one or
/// more, fulltext/hnsw/diskann require exactly one (upstream's
/// `Expected one column` check), count forbids the clause outright —
/// its table omits both spellings, so the spelling itself is what
/// diagnoses there.
enum FieldsPolicy {
    Required,
    ExactlyOne,
    Forbidden,
}

impl FieldsPolicy {
    fn is_exactly_one(&self) -> bool {
        matches!(self, FieldsPolicy::ExactlyOne)
    }
}

/// Folds one clause into an [`IndexShared`] when it is one of the
/// shared `index` clauses (`on`, `fields`/`columns`, `concurrently`
/// — including their failed attempts, which keep the required-clause
/// reports suppressed); `Err` hands the clause back for the kind's
/// own vocabulary match.
fn fold_index_shared(
    shared: &mut IndexShared,
    clause: ParsedClause,
) -> Result<(), Box<ParsedClause>> {
    match (clause.name, &clause.value) {
        ("on", ClauseValue::Ident(table)) => {
            shared.on_span = Some(clause.span);
            shared.on = Some(table.clone());
            Ok(())
        }
        ("on", ClauseValue::Failed) => {
            shared.on_attempted = true;
            Ok(())
        }
        ("fields", ClauseValue::ExprList(items)) | ("columns", ClauseValue::ExprList(items)) => {
            shared.fields = Some((clause.span, items.clone()));
            // Last one wins with the clause itself: a later `fields:`
            // after `columns:` resets the carried spelling.
            shared.fields_key = Carried((clause.name == "columns").then_some("columns"));
            Ok(())
        }
        ("fields", ClauseValue::Failed) | ("columns", ClauseValue::Failed) => {
            shared.fields_attempted = true;
            Ok(())
        }
        ("concurrently", ClauseValue::Flag) => {
            shared.concurrently = true;
            Ok(())
        }
        // A failed flag (form error at the clause, already diagnosed)
        // keeps the clause present without setting it.
        ("concurrently", ClauseValue::Failed) => Ok(()),
        _ => Err(Box::new(clause)),
    }
}

/// Folds a changefeed sub-block's clauses: repeated clauses fold
/// last-one-wins (rule 7 — `expiry` included, exactly like
/// upstream's positional clause loop), `include original` is
/// order-independent. Returns the changefeed when its required
/// `expiry` parsed, plus whether an `expiry` clause was attempted at
/// all — a failed attempt already carried its own diagnostic, so the
/// caller suppresses the missing-clause report for it (one root
/// cause, one diagnostic).
fn changefeed_of(sub: &[ParsedClause]) -> (Option<ChangeFeed>, bool) {
    let mut expiry: Option<Duration> = None;
    let mut expiry_attempted = false;
    let mut include_original = false;
    for clause in sub {
        match (&clause.value, clause.name) {
            (ClauseValue::Duration(value), "expiry") => {
                expiry = Some(*value);
                expiry_attempted = true;
            }
            (ClauseValue::Failed, "expiry") => expiry_attempted = true,
            (ClauseValue::Flag, "include original") => include_original = true,
            _ => {}
        }
    }
    (
        expiry.map(|expiry| ChangeFeed {
            expiry,
            include_original,
        }),
        expiry_attempted,
    )
}

/// The SCRAM-SHA-256 verifier shape `passscram:` accepts (spec/
/// user.md) — the structural mirror of the engine's parse-time
/// `ScramCredential::from_verifier_string` (surrealdb-iam):
/// `SCRAM-SHA-256$<iterations>:<salt>$<stored key>:<server key>`,
/// iterations ∈ [4096, 2²⁴], standard-base64 parts, salt ≥ 1
/// byte, keys exactly 32 bytes. The mirror is charset+length
/// arithmetic (no decoder dependency) — the bit-canonicality
/// corners a real decoder also rejects stay apply-time-loud (the
/// engine's parse is the final gate either way).
fn validate_scram_verifier(value: &str) -> Result<(), &'static str> {
    const MECHANISM: &str = "SCRAM-SHA-256";
    const ITERATIONS_RANGE: std::ops::RangeInclusive<u32> = 4096..=(1 << 24);
    const KEY_LEN: usize = 32;
    let (mechanism, rest) = value.split_once('$').ok_or("missing mechanism separator")?;
    if mechanism != MECHANISM {
        return Err("unsupported mechanism (expected `SCRAM-SHA-256`)");
    }
    let (iter_salt, keys) = rest.split_once('$').ok_or("missing key separator")?;
    let (iterations, salt) = iter_salt.split_once(':').ok_or("missing salt separator")?;
    let (stored_key, server_key) = keys.split_once(':').ok_or("missing server-key separator")?;
    let iterations: u32 = iterations
        .parse()
        .map_err(|_| "iteration count is not a valid integer")?;
    if !ITERATIONS_RANGE.contains(&iterations) {
        return Err("iteration count out of range (must be between 4096 and 16777216)");
    }
    let salt = base64_byte_len(salt).ok_or("salt is not valid base64")?;
    let stored_key = base64_byte_len(stored_key).ok_or("stored key is not valid base64")?;
    let server_key = base64_byte_len(server_key).ok_or("server key is not valid base64")?;
    if salt == 0 {
        return Err("the salt must not be empty");
    }
    if stored_key != KEY_LEN || server_key != KEY_LEN {
        return Err("the stored and server keys must be 32 bytes (44 base64 chars)");
    }
    Ok(())
}

/// The decoded byte length of a standard-base64 string — `None` when
/// the charset, length, or padding is malformed. Length arithmetic
/// only (`len/4*3 − padding`): [`validate_scram_verifier`] needs the
/// byte sizes, not the bytes.
fn base64_byte_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let padding = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    if padding > 2 {
        return None;
    }
    let body = &bytes[..bytes.len() - padding];
    if !body
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
    {
        return None;
    }
    Some(bytes.len() / 4 * 3 - padding)
}

/// Folds a user `duration` sub-block (spec/user.md): both sides are
/// optional and repeat last-one-wins (rule 7, like changefeed's
/// `expiry`). `(None, None)` — an empty block — folds to an absent
/// `duration`: SurrealQL's defaults apply, and partial blocks emit
/// only the present sides.
fn duration_of(sub: &[ParsedClause]) -> (Option<Expr>, Option<Expr>) {
    let mut token: Option<Expr> = None;
    let mut session: Option<Expr> = None;
    for clause in sub {
        match (&clause.value, clause.name) {
            (ClauseValue::Expr(expr), "token") => token = Some(expr.clone()),
            (ClauseValue::Expr(expr), "session") => session = Some(expr.clone()),
            _ => {}
        }
    }
    (token, session)
}

/// A JWT verification source's folded clauses (spec/access.md) — the
/// shape both the jwt variant's DIRECT clauses and the `jwt { … }`
/// sub-block share, so one fold serves both ([`FileParser::
/// finish_jwt_source`] validates either). Every optional clause
/// tracks whether it was attempted at all: a failed value's
/// diagnostic already fired, and the shape checks must not re-report
/// it (one root cause, one diagnostic).
struct JwtParts {
    url: Option<(Span, Expr)>,
    url_attempted: bool,
    algorithm: Option<(Span, Algorithm)>,
    algorithm_attempted: bool,
    key: Option<(Span, Expr)>,
    key_attempted: bool,
    audience: Option<Vec<String>>,
    /// Uniform with the struct's attempted-flag model; never
    /// consulted — audience does not participate in the source XOR
    /// (optional, so absent-vs-failed needs no distinguishing).
    audience_attempted: bool,
    issuer: Option<(Span, JwtIssuer)>,
    issuer_attempted: bool,
    issuer_key_attempted: bool,
}

impl JwtParts {
    fn new() -> Self {
        JwtParts {
            url: None,
            url_attempted: false,
            algorithm: None,
            algorithm_attempted: false,
            key: None,
            key_attempted: false,
            audience: None,
            audience_attempted: false,
            issuer: None,
            issuer_attempted: false,
            issuer_key_attempted: false,
        }
    }
}

/// Folds the four source clauses (`url`, `algorithm`, `key`,
/// `issuer`) out of a clause list — the union table's top level for
/// the jwt variant, a `jwt { … }` sub-block's interior for record and
/// bearer. All other clauses fall through untouched, so the same walk
/// works on either list.
fn fold_jwt_parts(clauses: &[ParsedClause]) -> JwtParts {
    let mut parts = JwtParts::new();
    for clause in clauses {
        match (clause.name, &clause.value) {
            ("url", ClauseValue::Expr(expr)) => parts.url = Some((clause.span, expr.clone())),
            ("url", ClauseValue::Failed) => parts.url_attempted = true,
            ("algorithm", ClauseValue::Word(word)) => {
                parts.algorithm = Some((clause.span, algorithm_of(word)))
            }
            ("algorithm", ClauseValue::Failed) => parts.algorithm_attempted = true,
            ("key", ClauseValue::Expr(expr)) => parts.key = Some((clause.span, expr.clone())),
            ("key", ClauseValue::Failed) => parts.key_attempted = true,
            ("audience", ClauseValue::StrList(values)) => parts.audience = Some(values.clone()),
            ("audience", ClauseValue::Failed) => parts.audience_attempted = true,
            ("issuer", ClauseValue::Sub(sub)) => {
                let (issuer, issuer_key_attempted) = fold_jwt_issuer(sub);
                parts.issuer = Some((clause.span, issuer));
                parts.issuer_key_attempted = issuer_key_attempted;
            }
            ("issuer", ClauseValue::Failed) => parts.issuer_attempted = true,
            _ => {}
        }
    }
    parts
}

/// Folds an `issuer { … }` interior (spec/access.md): both clauses
/// optional at parse, repeating last-one-wins; whether the `key:`
/// clause was attempted rides along for the required-unless-mirrored
/// validation.
fn fold_jwt_issuer(sub: &[ParsedClause]) -> (JwtIssuer, bool) {
    let mut issuer = JwtIssuer {
        algorithm: None,
        key: None,
    };
    let mut key_attempted = false;
    for clause in sub {
        match (clause.name, &clause.value) {
            ("algorithm", ClauseValue::Word(word)) => issuer.algorithm = Some(algorithm_of(word)),
            ("key", ClauseValue::Expr(expr)) => issuer.key = Some(expr.clone()),
            ("key", ClauseValue::Failed) => key_attempted = true,
            _ => {}
        }
    }
    (issuer, key_attempted)
}

/// Folds an `access` `duration` sub-block (spec/access.md): the three
/// expression sides are independently optional and repeat
/// last-one-wins; a block with no side present folds to an absent
/// `duration` (rule 9, like [`duration_of`]'s empty block).
fn access_duration_of(sub: &[ParsedClause]) -> Option<AccessDuration> {
    let mut duration = AccessDuration::default();
    for clause in sub {
        match (clause.name, &clause.value) {
            ("grant", ClauseValue::Expr(expr)) => duration.grant = Some(expr.clone()),
            ("token", ClauseValue::Expr(expr)) => duration.token = Some(expr.clone()),
            ("session", ClauseValue::Expr(expr)) => duration.session = Some(expr.clone()),
            _ => {}
        }
    }
    (duration.grant.is_some() || duration.token.is_some() || duration.session.is_some())
        .then_some(duration)
}

/// The algorithm word's variant (v3.2.4 `Algorithm`'s 13-kind set) —
/// the lowercase canonical spellings only (rule 9). `pub(crate)`: the
/// access reader shares the mapping, diagnosing on `None` what
/// the clause loop guarantees here.
pub(crate) fn algorithm_of_word(word: &str) -> Option<Algorithm> {
    match word {
        "eddsa" => Some(Algorithm::EdDsa),
        "es256" => Some(Algorithm::Es256),
        "es384" => Some(Algorithm::Es384),
        "es512" => Some(Algorithm::Es512),
        "hs256" => Some(Algorithm::Hs256),
        "hs384" => Some(Algorithm::Hs384),
        "hs512" => Some(Algorithm::Hs512),
        "ps256" => Some(Algorithm::Ps256),
        "ps384" => Some(Algorithm::Ps384),
        "ps512" => Some(Algorithm::Ps512),
        "rs256" => Some(Algorithm::Rs256),
        "rs384" => Some(Algorithm::Rs384),
        "rs512" => Some(Algorithm::Rs512),
        _ => None,
    }
}

/// The clause loop's algorithm fold: the word set parse already
/// guaranteed membership, so the `None` fallthrough is unreachable
/// through the clause loop.
fn algorithm_of(word: &str) -> Algorithm {
    algorithm_of_word(word).unwrap_or(Algorithm::Rs512)
}

/// Canonicalizes a level spelling (rule 9's input-only aliases; the
/// spellings are exactly [`LEVEL_WORDS`]'s members).
fn level_of(word: &str) -> Level {
    match word {
        "root" | "kv" => Level::Root,
        "namespace" | "ns" => Level::Namespace,
        _ => Level::Database,
    }
}

/// The authored spelling of a snowball language word when it was one
/// of the 2/3-letter ISO aliases rather than the canonical full name.
fn language_spelling(word: &str) -> Option<&'static str> {
    LANGUAGE_ALIAS_WORDS.iter().find(|w| **w == word).copied()
}

/// The alias spellings [`language_of`] accepts — everything but the
/// 18 canonical full names.
const LANGUAGE_ALIAS_WORDS: &[&str] = &[
    "ar", "ara", "da", "dan", "nl", "nld", "en", "eng", "fi", "fin", "fr", "fra", "de", "deu",
    "el", "ell", "hu", "hun", "it", "ita", "no", "nor", "pt", "por", "ro", "ron", "ru", "rus",
    "es", "spa", "sv", "swe", "ta", "tam", "tr", "tur",
];

/// The carried statement-keyword spelling: `Some` when the file wrote
/// the `ns`/`db` alias (rule 9), `None` for the canonical word —
/// the writer keeps the authored form.
fn carried_keyword(word: &str) -> Carried {
    Carried(match word {
        "ns" => Some("ns"),
        "db" => Some("db"),
        _ => None,
    })
}

/// The per-op permission words (spec/table.md's four-op set) — the
/// operations a `permissions { … }` line groups; also the
/// did-you-mean candidates.
const PERMISSION_OP_WORDS: &[&str] = &["select", "create", "update", "delete"];

/// The field domain of the same set — `delete` is not an operation on
/// a field (SurrealQL rejects it: "Can't define permission DELETE for
/// fields"), so it is out of the did-you-mean candidates too.
const FIELD_PERMISSION_OP_WORDS: &[&str] = &["create", "select", "update"];

/// The operation word's variant (lowercase canonical spellings only,
/// rule 9).
fn permission_op_of(word: &str) -> Option<PermissionOp> {
    match word {
        "select" => Some(PermissionOp::Select),
        "create" => Some(PermissionOp::Create),
        "update" => Some(PermissionOp::Update),
        "delete" => Some(PermissionOp::Delete),
        _ => None,
    }
}

/// The view body's clause-key heads ([`TABLE_VIEW_CLAUSES`]'s keys) —
/// the words a misplaced-query head check recognizes.
const VIEW_HEAD_WORDS: &[&str] = &["permissions", "graphql"];

/// Whether a captured value opens with one of `keys` spelled as a
/// clause head — the word followed (across SurrealQL trivia) by `:`
/// or `{` — rather than as an expression. Rule 3's comment model:
/// detection compares trivia-stripped text, never raw bytes.
fn opens_with_clause_head(text: &str, keys: &[&str]) -> bool {
    let Some(after_start) = strip_capture_trivia(text) else {
        return false;
    };
    let word_len = word_prefix_len(after_start);
    let word = &after_start[..word_len];
    if !keys.contains(&word) {
        return false;
    }
    let Some(after_word) = strip_capture_trivia(&after_start[word_len..]) else {
        return false;
    };
    matches!(after_word.as_bytes().first(), Some(b':') | Some(b'{'))
}

/// A [`TableKind`]'s name for composed diagnostics.
fn kind_word(kind: &TableKind) -> &'static str {
    match kind {
        TableKind::Any => "any",
        TableKind::Normal => "normal",
        TableKind::Relation { .. } => "relation",
        TableKind::View { .. } => "view",
    }
}

/// Folds a `graphql { … }` interior (spec/function.md, spec/table.md):
/// both string sides optional, repeating last-one-wins; a block with
/// neither side present folds to `None` (nothing to emit).
fn graphql_of(sub: &[ParsedClause]) -> Graphql {
    let mut params = Graphql::default();
    for clause in sub {
        match (clause.name, &clause.value) {
            ("alias", ClauseValue::Str(value)) => params.alias = Some(value.clone()),
            ("deprecated", ClauseValue::Str(value)) => params.deprecated = Some(value.clone()),
            _ => {}
        }
    }
    params
}

/// A field path's dotted source shape (the subfield-under-computed
/// check's key): name segments raw, the wildcard `*`.
fn field_path_text(path: &FieldPath) -> String {
    path.to_text()
}

/// The subfield-under-computed check over one candidate field
/// (spec/field.md): a dotted, non-computed path whose root is a
/// computed field's path diagnoses at `head_span`. Shared by the
/// table body, the mixin body, and the spread-member extension of
/// the table's (the roots and candidates differ per site; the rule
/// is one).
fn check_subfield_under_computed(
    diags: &mut Vec<Diag>,
    head_span: Span,
    field: &Field,
    computed_roots: &[String],
) {
    if field.computed.is_some() {
        return;
    }
    let path = field_path_text(&field.name);
    if let Some(dot) = path.find('.') {
        let root = &path[..dot];
        if computed_roots.iter().any(|other| *other == root) {
            diags.push(Diag::InvalidFieldComputed {
                span: head_span,
                message: format!(
                    "no subfield may nest under a computed parent — `{root}` is \
                     computed, so `{path}` cannot be defined"
                ),
            });
        }
    }
}

/// One body's duplicate member keys (spec/mixin.md; SDL-E1067): the
/// same family+key — a field's dotted path, an index's or event's
/// name — twice within ONE body, a mixin's or a table's. The engine's
/// last-one-wins would silently hide the duplicate (issue #38's
/// promotion: the copy-paste smell E1064 rejects for spread
/// entries), so the parser rejects it; the diagnostic anchors the
/// SECOND occurrence's head. Statement recovery rides the depth-0
/// scanner as usual — the body keeps parsing, the file is dirty.
fn check_duplicate_member_keys<'a>(
    diags: &mut Vec<Diag>,
    members: impl Iterator<Item = (&'a Stmt, Span)>,
) {
    let mut seen: Vec<MemberKey> = Vec::new();
    for (stmt, span) in members {
        let Some(key) = stmt.member_key() else {
            continue;
        };
        if seen.contains(&key) {
            diags.push(Diag::DuplicateMemberKey {
                span,
                kind: key.0,
                key: key.1,
            });
        } else {
            seen.push(key);
        }
    }
}

/// Folds a `reference { … }` interior (spec/field.md): the one `on
/// delete:` action, repeating last-one-wins; a block with none
/// present folds to the engine default (`IGNORE` — the bare flag's
/// meaning, elided at emission).
fn on_delete_of(sub: &[ParsedClause]) -> OnDelete {
    let mut action = OnDelete::Ignore;
    for clause in sub {
        match (&clause.value, clause.name) {
            (ClauseValue::Word(word), "on delete") => {
                action = match word.as_str() {
                    "reject" => OnDelete::Reject,
                    "cascade" => OnDelete::Cascade,
                    "unset" => OnDelete::Unset,
                    _ => OnDelete::Ignore,
                };
            }
            (ClauseValue::Expr(expr), "on delete") => action = OnDelete::Then(expr.clone()),
            _ => {}
        }
    }
    action
}

/// The one bare word a stripped capture may hold: a word-shaped run
/// starting with a letter or `_`, surrounded by SurrealQL trivia
/// (`owner /* pick */` reads as `owner` — rule 3's comment model;
/// keyword detection compares trivia-stripped forms, never raw text).
/// Quoted spellings are not words here — SDL keywords are bare and
/// lowercase-only (rule 9).
fn sole_bare_word(text: &str) -> Option<&str> {
    let text = strip_capture_trivia(text)?;
    let (word, rest) = text.split_at(word_prefix_len(text));
    if word.is_empty() || !word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return None;
    }
    let rest = strip_capture_trivia(rest)?;
    rest.is_empty().then_some(word)
}

/// A snowball language spelling → its variant (spec/analyzer.md): the
/// 18 canonical full names plus the SurrealQL lexer's 2- and 3-letter
/// aliases (ISO 639-2/T spellings — `deu` not `ger`; `no` is the
/// parser's special-case Norwegian alias). Lowercase only (rule 9).
fn language_of(word: &str) -> Option<Language> {
    Some(match word {
        "arabic" | "ar" | "ara" => Language::Arabic,
        "danish" | "da" | "dan" => Language::Danish,
        "dutch" | "nl" | "nld" => Language::Dutch,
        "english" | "en" | "eng" => Language::English,
        "finnish" | "fi" | "fin" => Language::Finnish,
        "french" | "fr" | "fra" => Language::French,
        "german" | "de" | "deu" => Language::German,
        "greek" | "el" | "ell" => Language::Greek,
        "hungarian" | "hu" | "hun" => Language::Hungarian,
        "italian" | "it" | "ita" => Language::Italian,
        "norwegian" | "no" | "nor" => Language::Norwegian,
        "portuguese" | "pt" | "por" => Language::Portuguese,
        "romanian" | "ro" | "ron" => Language::Romanian,
        "russian" | "ru" | "rus" => Language::Russian,
        "spanish" | "es" | "spa" => Language::Spanish,
        "swedish" | "sv" | "swe" => Language::Swedish,
        "tamil" | "ta" | "tam" => Language::Tamil,
        "turkish" | "tr" | "tur" => Language::Turkish,
        _ => return None,
    })
}

/// `::<seg>[::<seg>…]` — the shared segment continuation of every
/// name path (middleware heads, the graphql fn lists, the analyzer
/// `function:` clause; the reverse readers ride it too).
/// Consumes each `::` itself: an empty `Ok`
/// means no `::` followed (the caller decides whether the head alone
/// suffices); `None` = malformed, a `::` with no segment after it.
/// The pair is tested before anything is consumed, so a lone `:`
/// survives for the caller's trailing-junk diagnostic instead of
/// being silently swallowed mid-path.
pub(crate) fn read_path_segments(scan: &mut Scanner<'_>) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    loop {
        scan.skip_surrealql_trivia();
        // `::` joins segments; anything else ends the path.
        if !scan.rest().starts_with("::") {
            break;
        }
        scan.eat_byte(b':');
        scan.eat_byte(b':');
        scan.skip_surrealql_trivia();
        match scan.read_ident() {
            Ok(Some((_, segment, _))) => segments.push(segment),
            _ => return None,
        }
    }
    Some(segments)
}

/// The `:` form of a graphql family clause — `auto`/`none`, the set
/// the clause parser validated; the structured families arrive only
/// through the sub-block form.
fn graphql_family_word<T>(word: &str) -> Family<T> {
    if word == "auto" {
        Family::Auto
    } else {
        Family::None
    }
}

/// One whole-number filter/version argument: digits with `_`
/// separators (the SurrealQL integer shape — no sign, no fraction; a
/// trailing `_` does not lex as digits upstream). Overflow fails.
fn numeric_arg<T: FromStr>(text: &str) -> Option<(T, &str)> {
    let text = strip_capture_trivia(text)?;
    let len = word_prefix_len(text);
    let raw = &text[..len];
    if raw.is_empty() || raw.starts_with('_') || raw.ends_with('_') {
        return None;
    }
    let value = raw.replace('_', "").parse().ok()?;
    Some((value, &text[len..]))
}

/// One decimal scalar (index's `lm:`/`alpha:`, bm25's sides,
/// minkowski's order): the SurrealQL float token shape minus the
/// `NaN`/`Infinity` constants — digits with `_` separators, an
/// optional `.` fraction, an optional `e`/`E` exponent with sign, an
/// optional `f` suffix (`1.2`, `1_000.5e-2`, `3f`). No leading `.`
/// (the token does not start there upstream) and no sign — the
/// integer clauses' unsigned shape carried over. The whole capture
/// must be the one number. Two bound rules keep the emitted DDL
/// re-parsable: overflow to infinity fails, and so does any magnitude
/// ≥ 2⁶³ — the bound is load-bearing for the f64 clauses (`lm`,
/// `alpha`, minkowski's order: Rust's `Display` never uses exponent
/// form, so such a value would print as an integer literal beyond
/// upstream's i64), and it keeps the bm25 `as f32` cast finite
/// (upstream itself has no 64-bit bound on the bm25 sides — probed —
/// but the narrowing window is semantically dead for ranking
/// constants).
fn float_arg(text: &str) -> Option<f64> {
    fn digit_run(bytes: &[u8], start: usize) -> Option<usize> {
        let mut i = start;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
        // At least one digit; `_` only as separator.
        if i == start || bytes[start] == b'_' || bytes[i - 1] == b'_' {
            return None;
        }
        Some(i)
    }
    let rest = strip_capture_trivia(text)?;
    let bytes = rest.as_bytes();
    let mut i = digit_run(bytes, 0)?;
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i = digit_run(bytes, i + 1)?;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        let after_sign = i + 1 + usize::from(matches!(bytes.get(i + 1), Some(b'+' | b'-')));
        i = digit_run(bytes, after_sign)?;
    }
    if bytes.get(i) == Some(&b'f') {
        i += 1;
    }
    if !strip_capture_trivia(&rest[i..]).is_some_and(str::is_empty) {
        return None;
    }
    let value = rest[..i]
        .replace('_', "")
        .trim_end_matches('f')
        .parse::<f64>()
        .ok()?;
    // 2⁶³ — the first magnitude whose bare print overflows upstream's
    // integer lexer (exactly representable as f64, so the comparison
    // is exact).
    (value.is_finite() && value.abs() < 9_223_372_036_854_775_808.0).then_some(value)
}

/// The `distance:` keyword → its variant (minkowski never arrives
/// here — the clause reader builds it with its order; the spellings
/// are exactly [`DISTANCE_WORDS`]'s other members).
fn distance_of(word: &str) -> Option<Distance> {
    Some(match word {
        "euclidean" => Distance::Euclidean,
        "cosine" => Distance::Cosine,
        "cosine_normalized" => Distance::CosineNormalized,
        "chebyshev" => Distance::Chebyshev,
        "hamming" => Distance::Hamming,
        "jaccard" => Distance::Jaccard,
        "pearson" => Distance::Pearson,
        "inner_product" => Distance::InnerProduct,
        "manhattan" => Distance::Manhattan,
        _ => return None,
    })
}

/// The `type:` word → its variant (the clause's word set validated
/// membership; the spellings are exactly [`VECTOR_TYPE_WORDS`]'s
/// members).
fn vector_type_of(word: &str) -> VectorType {
    match word {
        "f64" => VectorType::F64,
        "f32" => VectorType::F32,
        "f16" => VectorType::F16,
        "i64" => VectorType::I64,
        "i32" => VectorType::I32,
        "i16" => VectorType::I16,
        "i8" => VectorType::I8,
        _ => VectorType::U8,
    }
}

/// Splits a decoded file string into its bucket and key (upstream
/// `Lexer::lex_file`): the bucket is `[A-Za-z0-9_.-]*` up to the first
/// `:`, then `/`, then the key `[A-Za-z0-9_./-]*`. An empty bucket
/// rides through (upstream's loop accepts one); the key is always
/// emitted with its leading `/`. One deliberate strictness: upstream's
/// key loop *stops* at a second `:` and silently drops the tail; SDL
/// rejects instead — a compiler must not truncate a path it cannot
/// represent.
fn lex_file_string(text: &str) -> Option<(String, String)> {
    let colon = text.find(':')?;
    let bucket = &text[..colon];
    if !bucket
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return None;
    }
    let key = text[colon + 1..].strip_prefix('/')?;
    if key.contains(':') {
        return None;
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
    {
        return None;
    }
    Some((bucket.to_string(), format!("/{key}")))
}

/// `<maj.min.patch>` — the silo version tail: three u32 components
/// (the SurrealQL integer shape) joined by `.`; SurrealQL trivia may
/// sit between the tokens (upstream's token loop skips it).
fn parse_version_triple(text: &str) -> Option<(u32, u32, u32)> {
    let rest = strip_capture_trivia(text)?.strip_prefix('<')?;
    let (major, rest) = numeric_arg::<u32>(strip_capture_trivia(rest)?)?;
    let rest = strip_capture_trivia(rest)?.strip_prefix('.')?;
    let (minor, rest) = numeric_arg::<u32>(strip_capture_trivia(rest)?)?;
    let rest = strip_capture_trivia(rest)?.strip_prefix('.')?;
    let (patch, rest) = numeric_arg::<u32>(strip_capture_trivia(rest)?)?;
    let rest = strip_capture_trivia(rest)?.strip_prefix('>')?;
    strip_capture_trivia(rest)?
        .is_empty()
        .then_some((major, minor, patch))
}

/// Parses one filter atom (spec/analyzer.md): a bare keyword, or a
/// keyword with `(...)` arguments — two u16s for the ngrams (SurrealQL
/// does not validate `min ≤ max`; the linter flags it), a bare
/// language keyword for `snowball`, a string literal for `mapper`.
/// SurrealQL trivia may surround the name and arguments. (The
/// reverse readers ride this too, over name-folded canonical
/// text — see `read.rs`.)
pub(crate) fn parse_filter_atom(text: &str) -> Option<Filter> {
    let text = strip_capture_trivia(text)?;
    let (name, rest) = text.split_at(word_prefix_len(text));
    let (filter, rest) = match name {
        "ascii" => (Filter::Ascii, rest),
        "lowercase" => (Filter::Lowercase, rest),
        "uppercase" => (Filter::Uppercase, rest),
        "edgengram" | "ngram" => {
            let rest = strip_capture_trivia(rest)?.strip_prefix('(')?;
            let (min, rest) = numeric_arg(rest)?;
            let rest = strip_capture_trivia(rest)?.strip_prefix(',')?;
            let (max, rest) = numeric_arg(rest)?;
            let rest = strip_capture_trivia(rest)?.strip_prefix(')')?;
            let filter = if name == "edgengram" {
                Filter::EdgeNgram(min, max)
            } else {
                Filter::Ngram(min, max)
            };
            (filter, rest)
        }
        "snowball" => {
            let rest = strip_capture_trivia(rest)?.strip_prefix('(')?;
            let text = strip_capture_trivia(rest)?;
            let (word, rest) = text.split_at(word_prefix_len(text));
            let language = language_of(word)?;
            let rest = strip_capture_trivia(rest)?.strip_prefix(')')?;
            (
                Filter::Snowball(Authored {
                    value: language,
                    spelling: language_spelling(word),
                }),
                rest,
            )
        }
        "mapper" => {
            let rest = strip_capture_trivia(rest)?.strip_prefix('(')?;
            let (path, rest) = lex_string_literal(strip_capture_trivia(rest)?)?;
            let rest = strip_capture_trivia(rest)?.strip_prefix(')')?;
            (Filter::Mapper(path), rest)
        }
        _ => return None,
    };
    strip_capture_trivia(rest)?.is_empty().then_some(filter)
}

/// A SurrealQL string literal capture (rule 3's `<string>` keyword
/// clause): `'…'` or `"…"`, escapes decoded through the shared table,
/// nothing but SurrealQL trivia after the closing quote. Returns the
/// decoded value — comparisons and emission go through the value,
/// never the spelling (like durations).
fn parse_string_literal_text(text: &str) -> Option<String> {
    let (value, rest) = lex_string_literal(strip_capture_trivia(text)?)?;
    strip_capture_trivia(rest)?.is_empty().then_some(value)
}

/// A SurrealQL duration literal (rule 9): digit runs joined by unit
/// suffixes — `7d`, `1h30m`, `1_000ms` — over the unit set `ns us µs
/// ms s m h d w y`, integers only. Mirrors v3.2.4's duration lexer
/// (`syn/lexer/compound/number.rs`): suffixes are lowercase and glued
/// to the digits (no trivia inside the literal), `_` separators are
/// stripped before the value parses, and the arithmetic is checked
/// u64 (a year is 365 days, upstream's constant). The four SurrealQL
/// comment syntaxes may surround the literal (rule 3's comment
/// model); nothing else is allowed. Returns the parsed duration —
/// comparisons and emission go through its value, never the source
/// spelling. `pub(crate)`: the DDL readers
/// ([`crate::info::read`]) parse CHANGEFEED durations with the same
/// value-not-spelling rule.
pub(crate) fn parse_duration_text(text: &str) -> Option<Duration> {
    let mut rest = strip_capture_trivia(text)?;
    let mut total = std::time::Duration::ZERO;
    loop {
        // One component: an integer (`_` separators allowed) glued to
        // its unit suffix.
        let digits_len = rest
            .char_indices()
            .find(|(_, c)| !(c.is_ascii_digit() || *c == '_'))
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        if digits_len == 0 || rest.starts_with('_') {
            // A leading `_` does not lex as digits upstream (it starts
            // an identifier); `_` separators sit between digits only.
            return None;
        }
        let value: u64 = rest[..digits_len].replace('_', "").parse().ok()?;
        rest = &rest[digits_len..];
        let (unit, suffix_len) = duration_suffix(rest)?;
        rest = &rest[suffix_len..];
        let addition = match unit {
            DurationUnit::Nano => std::time::Duration::from_nanos(value),
            DurationUnit::Micro => std::time::Duration::from_micros(value),
            DurationUnit::Milli => std::time::Duration::from_millis(value),
            DurationUnit::Second => std::time::Duration::from_secs(value),
            DurationUnit::Minute => std::time::Duration::from_secs(value.checked_mul(60)?),
            DurationUnit::Hour => std::time::Duration::from_secs(value.checked_mul(3_600)?),
            DurationUnit::Day => std::time::Duration::from_secs(value.checked_mul(86_400)?),
            DurationUnit::Week => std::time::Duration::from_secs(value.checked_mul(604_800)?),
            DurationUnit::Year => std::time::Duration::from_secs(value.checked_mul(31_536_000)?),
        };
        total = total.checked_add(addition)?;
        // A digit starts the next component immediately; anything
        // else must be only trivia to the end — `1h 30m` is two
        // tokens upstream, not one compound, and is rejected here.
        match rest.chars().next() {
            Some(c) if c.is_ascii_digit() => continue,
            _ => {
                rest = strip_capture_trivia(rest)?;
                return if rest.is_empty() {
                    Some(Duration::from_std(total))
                } else {
                    None
                };
            }
        }
    }
}

/// A duration component's unit suffix (longest spellings first, so
/// `ms` wins over `m`; `µ` is U+00B5, upstream's three-byte spelling)
/// plus its byte length.
fn duration_suffix(text: &str) -> Option<(DurationUnit, usize)> {
    let bytes = text.as_bytes();
    let (unit, len) = if bytes.starts_with(b"ns") {
        (DurationUnit::Nano, 2)
    } else if bytes.starts_with(b"us") {
        (DurationUnit::Micro, 2)
    } else if bytes.starts_with("\u{b5}".as_bytes()) && bytes.get(2) == Some(&b's') {
        // U+00B5 is two bytes; `µs` is three.
        (DurationUnit::Micro, 3)
    } else if bytes.starts_with(b"ms") {
        (DurationUnit::Milli, 2)
    } else if bytes.starts_with(b"s") {
        (DurationUnit::Second, 1)
    } else if bytes.starts_with(b"m") {
        (DurationUnit::Minute, 1)
    } else if bytes.starts_with(b"h") {
        (DurationUnit::Hour, 1)
    } else if bytes.starts_with(b"d") {
        (DurationUnit::Day, 1)
    } else if bytes.starts_with(b"w") {
        (DurationUnit::Week, 1)
    } else if bytes.starts_with(b"y") {
        (DurationUnit::Year, 1)
    } else {
        return None;
    };
    Some((unit, len))
}

enum DurationUnit {
    Nano,
    Micro,
    Milli,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Year,
}

/// SurrealQL ident rule (rule 9): a bare word from the reserved set
/// cannot be an identifier; keyword-ness is ASCII-case-insensitive.
fn is_reserved(word: &str) -> bool {
    if !word.is_ascii() {
        return false;
    }
    let lower = word.to_ascii_lowercase();
    RESERVED_WORDS.binary_search(&lower.as_str()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::test_support::render;
    use proptest::prelude::*;

    /// A valid SDL file exercising every landed statement shape; the
    /// mutation strategy perturbs its bytes. The capture stressors
    /// (`;` inside a string, braces inside captures, comments inside
    /// captures) double as scanner tripwires; the database/bucket
    /// statements
    /// churn flags, sub-blocks, multi-word keys, and duration
    /// captures.
    const SAMPLE: &str = "\
/// Acme Corporation
namespace acme;

namespace staging { };

sequence invoice_number = 1000;

sequence order_number {
    batch: 100;
    start: 5000;
    timeout: 5s;
};

/// Default page size for list endpoints
param $default_limit = 50;

param $internal_seed {
    value: 42;
    permissions: none;
};

param $endpoints = { read: '/api/read', write: '/api/write' };

param $greeting = 'hi; there';

/// Primary application database
database app {
    strict;
    changefeed {
        expiry: 7d;
        include original;
    };
};

/// Ephemeral uploads
bucket scratch_uploads {
    backend: 'memory';
    readonly;
    permissions: $auth.admin = true;
};

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

user analyst {
    on: db;
};

/// English text search
analyzer english {
    function: fn::strip_markup;
    tokenizers: class;
    filters: snowball(en), lowercase, edgengram(2, 10);
};

analyzer raw_split {
    tokenizers: camel, punct;
};

/// Log every user creation
event user_created {
    on: user;
    when: $event = 'CREATE';
    then: { create log set at = time::now(), user = $after.id; };
};

event webhook_fanout {
    on: invoice;
    async {
        retry: 5;
        maxdepth: 8;
    };
    then: (create delivery set invoice = $after.id), (update stats:queue set n = n + 1);
};

/// Colour helpers
module color {
    from: f'modules:/color.surli';
    unsigned;
};

module {
    from: silo::acme::utils::<1.2.0>;
    unsigned;
    permissions: $auth.admin = true;
};

/// Post permissions
fn can_edit($p: record<post>) -> bool {
    return $auth.id = $p.author or $auth.role = 'admin';
} {
    permissions: $auth.admin = true;
};

fn tax($net: number, $rate: option<number>) {
    let $r = $rate ?? 0.15;
    let $tax = math::round($net * $r, 2);
    $net + $tax
};

fn shape($s: geometry<point>) -> { kind: string, at: datetime } {
    { kind: 'point', at: time::now() }
};

// Global api middleware (plain `//` — `config` has no COMMENT clause,
// so a `///` block here is rejected)
config api {
    middleware: api::timeout(5s), api::log();
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

/// Composite lookup for inbox queries
index by_account_thread {
    on: message;
    fields: account, thread_id;
};

index unique one_email {
    on: user;
    columns: email;
    concurrently;
};

index count active_count {
    on: item;
    where: status = 'active';
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
    lm: 0.4;
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

/// Route health checks
api '/health' {
    get {
        then: { { status: 200, body: { ok: true } } };
    };
};

api '/users/:id' {
    any {
        middleware: api::timeout(5s);
    };
    get, delete {
        permissions: $auth.admin = true;
        then: { { status: 200 } };
    };
};

api '/admin/*rest' {
    any {
        then: { { status: 404, body: { error: 'no such route' } } };
    };
    post, put {
        middleware: api::guard(), fn::audit('write');
        then: { { status: 202 } };
    };
};

api '/bare';

/// Tokens issued by Acme SSO
access corp_sso {
    on: database;
    type: jwt;
    url: 'https://sso.acme.com/.well-known/jwks.json';
};

access member {
    on: database;
    type: record;
    refresh;
    signin: { select id from user where email = $email and crypto::argon2::compare(password, $password) };
    jwt {
        algorithm: eddsa;
        key: 'member_signing_key';
        issuer {
            key: '[REDACTED]';
        };
    };
};

/// CI bearer tokens
access ci_tokens {
    on: root;
    type: bearer;
    for: user;
    duration {
        grant: 30d;
        token: 1d;
    };
    authenticate: $auth.role = 'ci';
};

// Shared audit columns (no `///` — a mixin takes no doc comment)
mixin audited {
  created_at: datetime { default: time::now(); };
  updated_at: datetime;
};

/// Application users
table user schemafull {
  permissions {
    select: full;
    create: full;
    update: $auth.id = id;
    delete: none;
  };
  ...audited;
  email: string {
    assert: string::is_email($value);
    default: 'unknown@example.com';
  };
  metadata: object { flexible; };
  metadata.trusted_device: bool;
  index unique email_unique { fields: email; };
  event audit_email {
    when: $event = 'UPDATE' and $before.email != $after.email;
    then: { create audit set at = time::now(); };
  };
};

table audit {
  drop;
  schemaless;
  changefeed { expiry: 30d; include original; };
};

table user | org -> wrote -> post | comment { enforced; schemaless; };
table relation generic_edge;

/// Author display name, derived
field display_name {
  on: user;
  computed: string::uppercase(name);
};

/// Posts per author
table view post_stats {
  select author, count() as posts from post group by author;
};

// The rule-8 escape hatch: verbatim DDL — interior comments (all
// four syntaxes) and brace-bearing strings ride along unharmed.
raw {
  // shapes SDL cannot express yet
  DEFINE TABLE legacy_data SCHEMALESS; /* trailing comment */
  DEFINE PARAM OVERWRITE $weird VALUE '} misuse { of braces';
};
";

    fn parse_ok(src: &str) -> SdlFile {
        let (file, diags) = parse_file(src);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        file
    }

    /// `is_reserved` binary-searches the table: sortedness (and, with
    /// strict `<`, uniqueness) is load-bearing — a mis-sorted entry
    /// would silently stop being rejected as a bare name.
    #[test]
    fn reserved_words_are_sorted_unique_and_populated() {
        assert!(RESERVED_WORDS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(RESERVED_WORDS.len(), 44, "the upstream set changed");
        for word in ["select", "create", "define", "table", "where"] {
            assert!(is_reserved(word), "`{word}` must be in the set");
        }
        for word in ["namespace", "ns", "acme", "table_x"] {
            assert!(!is_reserved(word), "`{word}` must not be in the set");
        }
    }

    fn single_namespace(src: &str) -> Namespace {
        let file = parse_ok(src);
        assert_eq!(file.items.len(), 1, "src: {src}");
        match file.items.into_iter().next().expect("len checked") {
            Stmt::Namespace(ns) => ns,
            other => panic!("expected a namespace: {other:?}"),
        }
    }

    fn first_error(src: &str) -> Diag {
        let (_, diags) = parse_file(src);
        assert!(!diags.is_empty(), "expected a diagnostic: {src}");
        diags.into_iter().next().expect("checked above")
    }

    #[test]
    fn bare_and_empty_block_forms_are_equivalent() {
        let expected = Namespace {
            name: Ident::new("staging"),
            keyword: Carried::default(),
            comment: None,
            comments: Vec::new(),
            clause_comments: Vec::new(),
        };
        assert_eq!(single_namespace("namespace staging;"), expected);
        assert_eq!(single_namespace("namespace staging { };"), expected);
        assert_eq!(single_namespace("namespace staging {}"), expected);
        // Comments and bare `;`s inside the block are not clauses.
        assert_eq!(
            single_namespace("namespace staging { // note\n;; };"),
            expected
        );
    }

    /// Upstream requires `;` *between* statements and accepts its
    /// absence only on the file's final statement (probe-verified
    /// against v3.2.4: `DEFINE NS a DEFINE NS b;` is rejected).
    #[test]
    fn statement_terminators_follow_surrealql() {
        // Final statement without `;`: accepted, bare and block form.
        assert!(parse_ok("namespace acme").items.len() == 1);
        assert!(parse_ok("namespace acme { }").items.len() == 1);
        // Missing `;` between statements: one diagnostic, both
        // statements still parsed (the driver is already positioned
        // at the next statement, so recovery must not skip it).
        let (file, diags) = parse_file("namespace a {} namespace b;\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::ExpectedStatementEnd {
                found: Some('n'),
                ..
            }]
        ));
        assert_eq!(file.items.len(), 2);
        // Bare form: `a` itself is incomplete, so only the diagnostic
        // marks it — but no recovery skip: `b` still parses in place
        // (before the fix, recovery swallowed it without a trace).
        let (file, diags) = parse_file("namespace a namespace b;\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::ExpectedStatementEnd {
                found: Some('n'),
                ..
            }]
        ));
        assert_eq!(file.items.len(), 1);
        // Junk after the name gets its own diagnostic as well: the
        // terminator error does not consume the junk.
        let (file, diags) = parse_file("namespace acme = 5;\n");
        assert_eq!(diags.len(), 2, "terminator + junk char: {diags:?}");
        assert!(file.items.is_empty());
    }

    #[test]
    fn docs_survive_only_where_they_can_attach() {
        // A stray `;` between a doc block and a definition orphans the
        // doc — reported, never silently dropped from a clean file.
        let (_, diags) = parse_file("/// doc\n;\nnamespace a;\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::DanglingDocComment { .. }]
        ));
        // A doc line inside a block reads as the unknown clause it is.
        let diag = first_error("namespace a { /// doc\n };\n");
        assert!(matches!(
            diag,
            Diag::UnknownClause { ref key, statement: "namespace", .. } if key == "///…"
        ));
    }

    #[test]
    fn doc_comment_lines_attach_in_order() {
        let ns = single_namespace("/// Acme Corporation\n/// Est. 2026.\nnamespace acme;\n");
        assert_eq!(ns.name, Ident::new("acme"));
        let comment = ns.comment.as_ref().expect("doc attached");
        assert_eq!(comment.lines, ["Acme Corporation", "Est. 2026."]);
        // The joined form is what codegen will emit.
        assert_eq!(comment.joined(), "Acme Corporation\nEst. 2026.");
    }

    #[test]
    fn alias_and_quoted_names_read_to_the_same_ast() {
        let expected = Namespace {
            name: Ident::new("acme"),
            keyword: Carried::default(),
            comment: None,
            comments: Vec::new(),
            clause_comments: Vec::new(),
        };
        assert_eq!(single_namespace("ns acme;"), expected);
        assert_eq!(single_namespace("namespace acme;"), expected);
        // Quoted spellings keep their unescaped interior.
        assert_eq!(
            single_namespace("namespace `table`;").name,
            Ident::new("table")
        );
        assert_eq!(
            single_namespace("namespace ⟨acme⟩;").name,
            Ident::new("acme")
        );
        assert_eq!(
            single_namespace("namespace `a\\nb`;").name,
            Ident::new("a\nb")
        );
    }

    #[test]
    fn statements_parse_in_source_order() {
        let file = parse_ok("namespace one;\nnamespace two;\n");
        let names: Vec<&str> = file
            .items
            .iter()
            .filter_map(|stmt| match stmt {
                Stmt::Namespace(ns) => Some(ns.name.raw.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["one", "two"]);
    }

    #[test]
    fn unknown_statement_carries_did_you_mean() {
        let diag = first_error("namespac acme;\n");
        assert!(matches!(
            diag,
            Diag::UnknownStatement { ref word, ref suggestion, .. }
                if word == "namespac" && suggestion.as_deref() == Some("namespace")
        ));
        // Nothing resembling a statement keyword: no suggestion.
        let diag = first_error("flurb x;\n");
        assert!(matches!(
            diag,
            Diag::UnknownStatement { ref suggestion, .. } if suggestion.is_none()
        ));
    }

    #[test]
    fn non_empty_block_is_the_unknown_clause_error() {
        let diag = first_error("namespace acme { oops; };\n");
        assert!(matches!(
            diag,
            Diag::UnknownClause { ref key, statement: "namespace", .. } if key == "oops"
        ));
        // Every junk clause is reported; recovery skips whole clauses.
        let (_, diags) = parse_file("namespace acme { a: 1; b: 2; c };\n");
        assert_eq!(diags.len(), 3, "three unknown clauses: {diags:?}");
    }

    #[test]
    fn bare_reserved_names_follow_surrealql_ident_rules() {
        let diag = first_error("namespace select;\n");
        assert!(matches!(
            diag,
            Diag::ReservedIdent { ref word, .. } if word == "select"
        ));
        // Keyword-ness is ASCII-case-insensitive…
        assert!(matches!(
            first_error("namespace SELECT;"),
            Diag::ReservedIdent { .. }
        ));
        // …and SDL conservatively requires backticks for the whole
        // upstream reserved set: `table` is in it, even though the
        // probe showed upstream's own name position would accept bare
        // `table` (SDL is stricter, never looser).
        assert!(matches!(
            first_error("namespace table;"),
            Diag::ReservedIdent { ref word, .. } if word == "table"
        ));
        // Quoting clears any collision.
        assert_eq!(
            single_namespace("namespace `table`;").name,
            Ident::new("table")
        );
        assert!(parse_ok("namespace `select`;").items.len() == 1);
    }

    #[test]
    fn name_position_errors() {
        // `;` and EOF say "expected a name"…
        assert!(matches!(
            first_error("namespace ;"),
            Diag::ExpectedName { .. }
        ));
        assert!(matches!(
            first_error("namespace"),
            Diag::ExpectedName { .. }
        ));
        // …so does an identifier that fails to scan.
        assert!(matches!(
            first_error("namespace ⟨oops"),
            Diag::UnterminatedQuotedIdent { .. }
        ));
        // Only `;` or `{` may follow the name.
        assert!(matches!(
            first_error("namespace acme = 5;"),
            Diag::ExpectedStatementEnd {
                found: Some('='),
                ..
            }
        ));
        assert!(matches!(
            first_error("namespace acme ) ;"),
            Diag::ExpectedStatementEnd {
                found: Some(')'),
                ..
            }
        ));
    }

    #[test]
    fn block_and_doc_errors() {
        assert!(matches!(
            first_error("namespace acme {"),
            Diag::UnterminatedBlock { .. }
        ));
        assert!(matches!(
            first_error("/// Acme Corporation\n"),
            Diag::DanglingDocComment { .. }
        ));
        // A doc block above the second of two statements attaches to
        // it; only one with nothing below it dangles.
        let file = parse_ok("namespace a;\n/// doc\nnamespace b;\n");
        let [_, Stmt::Namespace(second)] = &file.items[..] else {
            panic!("expected two namespaces: {file:?}");
        };
        assert!(second.comment.is_some());
    }

    #[test]
    fn illegal_characters_report_and_recover() {
        let diag = first_error("# hash\nnamespace acme;\n");
        assert!(matches!(diag, Diag::IllegalCharacter { ch: '#', .. }));
        // Junk inside a block reports as an illegal character.
        assert!(matches!(
            first_error("namespace acme { 9 };"),
            Diag::IllegalCharacter { ch: '9', .. }
        ));
    }

    #[test]
    fn broken_statements_do_not_hide_their_siblings() {
        // Statement-level recovery blanks one statement only.
        let (file, diags) = parse_file("bogus here; namespace acme;\n");
        assert!(matches!(diags.as_slice(), [Diag::UnknownStatement { .. }]));
        assert_eq!(file.items.len(), 1);
        // Clause-level recovery keeps the block's own statement alive
        // and its siblings too.
        let (file, diags) = parse_file("namespace a { oops: 1; };\nnamespace b;\n");
        assert!(matches!(diags.as_slice(), [Diag::UnknownClause { .. }]));
        assert_eq!(file.items.len(), 2);
        // Statement-level recovery rides `;`: a stray top-level `}`
        // does not end the statement, the next `;` does.
        let (file, diags) = parse_file("bogus } namespace acme;\n");
        assert_eq!(diags.len(), 1);
        assert!(file.items.is_empty());
        // Clause-level recovery consumes a stray `)`/`]` silently and
        // stops at `}`.
        let (file, diags) = parse_file("namespace a { oops ) ; };\nnamespace b;\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(file.items.len(), 2);
    }

    #[test]
    fn empty_file_is_empty() {
        assert!(parse_ok("").items.is_empty());
        assert!(parse_ok("// just a comment\n").items.is_empty());
        // A stray `;` between statements is an empty statement.
        assert!(parse_ok(";;").items.is_empty());
    }

    // ---- sequence and param ----

    fn single_statement(src: &str) -> Stmt {
        let file = parse_ok(src);
        assert_eq!(file.items.len(), 1, "src: {src}");
        file.items.into_iter().next().expect("len checked")
    }

    fn single_sequence(src: &str) -> Sequence {
        match single_statement(src) {
            Stmt::Sequence(seq) => seq,
            other => panic!("expected a sequence: {other:?}"),
        }
    }

    fn single_param(src: &str) -> Param {
        match single_statement(src) {
            Stmt::Param(param) => param,
            other => panic!("expected a param: {other:?}"),
        }
    }

    /// spec/sequence.md: all three surface forms, the fixed SurrealQL
    /// clause order applied at emission only, and free SDL order.
    #[test]
    fn sequence_forms_parse_to_presence() {
        // Short form: the start value, everything else default.
        assert_eq!(
            single_sequence("sequence invoice_number = 1000;"),
            Sequence {
                name: Ident::new("invoice_number"),
                start: Some(Expr {
                    canon: "1000".to_string()
                }),
                batch: None,
                timeout: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }
        );
        // Bare form: all defaults.
        assert_eq!(
            single_sequence("sequence row_ids;"),
            Sequence {
                name: Ident::new("row_ids"),
                start: None,
                batch: None,
                timeout: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }
        );
        // The empty block form is the bare form.
        assert_eq!(
            single_sequence("sequence row_ids { };"),
            single_sequence("sequence row_ids;")
        );
        // Block form with every clause, in the spec's example order.
        assert_eq!(
            single_sequence("sequence n { start: 5000; batch: 100; timeout: 5s; };"),
            Sequence {
                name: Ident::new("n"),
                start: Some(Expr {
                    canon: "5000".to_string()
                }),
                batch: Some(Expr {
                    canon: "100".to_string()
                }),
                timeout: Some(Expr {
                    canon: "5s".to_string()
                }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }
        );
        // SDL's block order is free (rule 7); presence is what matters.
        assert_eq!(
            single_sequence("sequence n { timeout: 5s; batch: 100; };"),
            Sequence {
                name: Ident::new("n"),
                start: None,
                batch: Some(Expr {
                    canon: "100".to_string()
                }),
                timeout: Some(Expr {
                    canon: "5s".to_string()
                }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }
        );
        // The file's final statement may omit `;` in every form —
        // though a clause *inside* a block still needs its `;`: the
        // depth-0 `;` is what ends a capture (rule 3), so `start: 1 }`
        // is an unbalanced-capture error.
        assert!(parse_ok("sequence n = 1").items.len() == 1);
        assert!(parse_ok("sequence n").items.len() == 1);
        assert!(parse_ok("sequence n { start: 1; }").items.len() == 1);
        assert!(matches!(
            first_error("sequence n { start: 1 }"),
            Diag::UnbalancedCapture { bracket: '}', .. }
        ));
    }

    #[test]
    fn sequence_clauses_fold_last_one_wins() {
        assert_eq!(
            single_sequence("sequence n { start: 1; start: 1 + 1; };").start,
            Some(Expr {
                canon: "1 + 1".to_string()
            })
        );
    }

    /// Captures go through the embedded parser at parse time: comments
    /// dropped, whitespace canonicalized — source text is never kept.
    #[test]
    fn sequence_values_canonize_through_the_embedded_parser() {
        let seq = single_sequence("sequence n { start: 1  /* one */  +\n1; };");
        assert_eq!(
            seq.start,
            Some(Expr {
                canon: "1 + 1".to_string()
            })
        );
        // A trailing comment inside the capture is dropped too.
        let seq = single_sequence("sequence n { timeout: 5s // seconds\n; };");
        assert_eq!(
            seq.timeout,
            Some(Expr {
                canon: "5s".to_string()
            })
        );
    }

    /// Rule 5 + spec/sequence.md: no COMMENT clause exists, so `///`
    /// is rejected — but the statement itself still parses.
    #[test]
    fn doc_comment_on_sequence_is_rejected_but_statement_survives() {
        let (file, diags) = parse_file("/// invoice numbering\nsequence invoice_number;\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::DocCommentUnsupported {
                statement: "sequence",
                ..
            }]
        ));
        assert_eq!(file.items.len(), 1);
    }

    /// The doc-block diagnostic span anchors on the `///` opener
    /// itself — not one byte right of it, which is where the first
    /// line's *text* starts when the conventional space follows.
    /// Parsed-file spans, not the hand-built diag snapshots, are what
    /// pin this (the original bug lived exactly in that seam).
    #[test]
    fn doc_block_spans_cover_the_opener() {
        let (_, diags) = parse_file("/// invoice numbering\nsequence n;\n");
        let Some(Diag::DocCommentUnsupported { span, .. }) = diags.first() else {
            panic!("expected DocCommentUnsupported: {diags:?}");
        };
        assert_eq!(*span, Span::new(0, 3));

        let (_, diags) = parse_file("/// doc\n;\nnamespace a;\n");
        let Some(Diag::DanglingDocComment { span }) = diags.first() else {
            panic!("expected DanglingDocComment: {diags:?}");
        };
        assert_eq!(*span, Span::new(0, 3));
    }

    /// spec/param.md: the surface forms and their ASTs.
    #[test]
    fn param_forms_parse_to_presence() {
        // Short form.
        assert_eq!(
            single_param("param $default_limit = 50;"),
            Param {
                name: ParamName::new("default_limit"),
                value: Some(Expr {
                    canon: "50".to_string()
                }),
                permissions: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }
        );
        // Bare form: stores VALUE NONE (SurrealQL parity).
        assert_eq!(
            single_param("param $x;"),
            Param {
                name: ParamName::new("x"),
                value: None,
                permissions: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }
        );
        // Block form, permissions keyword arm (ASCII-case-insensitive —
        // probe-verified: upstream accepts `PERMISSIONS none`).
        assert_eq!(
            single_param("param $x { value: 42; permissions: none; };"),
            Param {
                name: ParamName::new("x"),
                value: Some(Expr {
                    canon: "42".to_string()
                }),
                permissions: Some(Permission::None),
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }
        );
        assert_eq!(
            single_param("param $x { permissions: FULL; };").permissions,
            Some(Permission::Full)
        );
        // Permissions expression arm: canon holds the WHERE-less text.
        assert_eq!(
            single_param("param $x { permissions: level > 10; };").permissions,
            Some(Permission::Expr(Expr {
                canon: "level > 10".to_string()
            }))
        );
        // `param` takes a doc comment (COMMENT clause exists).
        let param = single_param("/// Default page size\nparam $x = 50;");
        assert_eq!(
            param.comment.expect("doc attached").lines,
            ["Default page size"]
        );
        // The file's final statement may omit `;` in every form —
        // same capture rule as sequence's block clauses.
        assert!(parse_ok("param $x = 1").items.len() == 1);
        assert!(parse_ok("param $x").items.len() == 1);
        assert!(parse_ok("param $x { value: 1; }").items.len() == 1);
    }

    /// README assignment-short-form rule: `{` after `=` stays inside
    /// the capture. An object literal is the object-valued short form;
    /// anything the embedded parser rejects (including a
    /// trailing SDL-looking block) is rejected with it.
    #[test]
    fn object_valued_short_form_vs_trailing_block() {
        // Object-valued short form — braces survive canonicalization.
        assert_eq!(
            single_param("param $endpoints = { read: '/api/read', write: '/api/write' };").value,
            Some(Expr {
                canon: "{ read: '/api/read', write: '/api/write' }".to_string()
            })
        );
        // A block after a plain value is no block opener — the capture
        // `50 { value: 1; }` cannot parse as an expression.
        let (_, diags) = parse_file("param $x = 50 { value: 1; };\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::EmbeddedParseError { .. }]
        ));
        // The clause-leak guard: a capture that would ride the
        // wrapper's clause loop is rejected at the bare parse.
        let (_, diags) = parse_file("param $x = 1 COMMENT 'sneaky';\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::EmbeddedParseError { .. }]
        ));
    }

    /// Param names keep the `$`; upstream's parameter lexer accepts
    /// keywords, quoted spellings, and digit-led *quoted* names after
    /// it, while a bare digit-led name does not lex as a parameter
    /// (all probe-verified against v3.2.4).
    #[test]
    fn param_names_follow_upstream_lexing() {
        // A reserved word is fine after `$`.
        assert_eq!(
            single_param("param $table = 1;").name,
            ParamName::new("table")
        );
        // Quoted forms keep their unescaped interior.
        assert_eq!(
            single_param("param $`weird name` = 1;").name,
            ParamName::new("weird name")
        );
        assert_eq!(single_param("param $⟨x⟩ = 1;").name, ParamName::new("x"));
        // A digit-led name is accepted quoted — upstream's
        // `$` + surrounded-ident path accepts it — and rejected bare.
        assert_eq!(
            single_param("param $`9lives` = 1;").name,
            ParamName::new("9lives")
        );
        assert!(matches!(
            first_error("param $9lives = 1;"),
            Diag::ExpectedName { .. }
        ));
        // Missing `$`, or `$` with nothing after it.
        assert!(matches!(
            first_error("param limit = 1;"),
            Diag::ExpectedName { .. }
        ));
        assert!(matches!(
            first_error("param $ = 1;"),
            Diag::ExpectedName { .. }
        ));
    }

    #[test]
    fn unknown_clauses_and_missing_values_on_the_new_statements() {
        let diag = first_error("sequence n { oops: 1; };\n");
        assert!(matches!(
            diag,
            Diag::UnknownClause { ref key, statement: "sequence", .. } if key == "oops"
        ));
        let diag = first_error("param $x { oops: 1; };\n");
        assert!(matches!(
            diag,
            Diag::UnknownClause { ref key, statement: "param", .. } if key == "oops"
        ));
        // A clause key with no `:` (and so no capture) — the
        // colon-less sub-block form is parser machinery.
        let diag = first_error("sequence n { start; };\n");
        assert!(matches!(
            diag,
            Diag::ExpectedClauseValue { key: "start", .. }
        ));
        let diag = first_error("param $x { permissions { } };\n");
        assert!(matches!(
            diag,
            Diag::ExpectedClauseValue {
                key: "permissions",
                ..
            }
        ));
        // Whitespace before the colon is trivia, like every other
        // seam in the grammar.
        assert!(parse_ok("sequence n { start : 5000; };").items.len() == 1);
        assert!(parse_ok("param $x { permissions : none; };").items.len() == 1);
        // A permissions capture ending at EOF (file-final statement,
        // no `;`) canonizes instead of double-diagnosing: the value is
        // real, so only the unterminated block is reported.
        let (file, diags) = parse_file("param $x { permissions: full");
        assert!(matches!(diags.as_slice(), [Diag::UnterminatedBlock { .. }]));
        assert!(file.items.is_empty());
        // An uppercase clause key is rejected (rule 9: SDL keywords
        // are lowercase) — but gets a did-you-mean.
        let diag = first_error("param $x { VALUE: 50; };\n");
        assert!(matches!(
            diag,
            Diag::UnknownClause { ref key, ref suggestion, .. }
                if key == "VALUE" && suggestion.as_deref() == Some("value")
        ));
        // Every junk clause is reported; recovery skips whole clauses.
        let (_, diags) = parse_file("sequence n { a: 1; b: 2; c };\n");
        assert_eq!(diags.len(), 3, "three unknown clauses: {diags:?}");
    }

    #[test]
    fn embedded_rejections_and_capture_errors_are_distinct() {
        // Trailing junk in a capture: the embedded parser's rejection.
        let diag = first_error("sequence n = 1 2;\n");
        assert!(matches!(diag, Diag::EmbeddedParseError { .. }));
        // An empty capture: our own message through the same code.
        let diag = first_error("param $x = ;\n");
        assert!(matches!(diag, Diag::EmbeddedParseError { .. }));
        // A string that never closes is a scanner error, not an
        // embedded rejection — the capture never completed.
        assert!(matches!(
            first_error("param $x = 'oops;\n"),
            Diag::UnterminatedString { .. }
        ));
    }

    #[test]
    fn embedded_rejection_markers_land_on_the_file_position() {
        // `COMMENT` sits at char column 14 in this file; the forwarded
        // snippet's marker names it in file coordinates (the miette
        // label still brackets the whole capture).
        let diag = first_error("param $x = 1 COMMENT 'oops';\n");
        assert!(matches!(
            diag,
            Diag::EmbeddedParseError { ref message, .. }
                if message.contains("--> [1:14]")
        ));
    }

    #[test]
    fn broken_new_statements_do_not_hide_their_siblings() {
        // Junk clauses inside a block keep the statement and its
        // siblings alive.
        let (file, diags) = parse_file("sequence a { oops: 1; };\nparam $x = 1;\n");
        assert!(matches!(diags.as_slice(), [Diag::UnknownClause { .. }]));
        assert_eq!(file.items.len(), 2);
        // A rejected capture drops its own statement only.
        let (file, diags) = parse_file("param $bad = 1 COMMENT 'x';\nsequence n = 1;\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::EmbeddedParseError { .. }]
        ));
        assert_eq!(file.items.len(), 1);
        // Statement-level recovery blanks one statement only.
        let (file, diags) = parse_file("sequence = 1;\nnamespace acme;\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(file.items.len(), 1);
    }

    #[test]
    fn mixed_statements_keep_source_order() {
        let file = parse_ok(
            "namespace acme;\nsequence n = 1;\nparam $x = 2;\nsequence m;\nuser u { on: root; };\nanalyzer a;\nevent e { on: t; then: 1; };\nmodule silent { from: f'm:/silent.surli'; unsigned; };\nfn loud() { 1 };\n",
        );
        let summary: Vec<String> = file
            .items
            .iter()
            .map(|stmt| match stmt {
                Stmt::Namespace(ns) => ns.name.raw.clone(),
                Stmt::Sequence(seq) => seq.name.raw.clone(),
                Stmt::Param(param) => param.name.raw.clone(),
                Stmt::Database(db) => db.name.raw.clone(),
                Stmt::Bucket(bucket) => bucket.name.raw.clone(),
                Stmt::User(user) => user.name.raw.clone(),
                Stmt::Analyzer(analyzer) => analyzer.name.raw.clone(),
                Stmt::Event(event) => event.name.raw.clone(),
                Stmt::Module(module) => module
                    .name
                    .as_ref()
                    .map(|n| n.raw.clone())
                    .unwrap_or_default(),
                Stmt::Function(function) => function.name.segments.join("::"),
                Stmt::Config(Config::Default(_)) => "default".to_string(),
                Stmt::Config(_) => "config".to_string(),
                Stmt::Index(index) => index.name.raw.clone(),
                Stmt::Api(api) => api.path.clone(),
                Stmt::Access(access) => access.name.raw.clone(),
                Stmt::Table(table) => table.name.raw.clone(),
                Stmt::Field(_) => String::new(),
                Stmt::Mixin(mixin) => mixin.name.raw.clone(),
                Stmt::TypeAlias(alias) => alias.name.raw.clone(),
                // A spread never rides the file's top level.
                Stmt::Spread(_) => String::new(),
                Stmt::Raw(_) => String::new(),
                Stmt::Include(include) => include.path.clone(),
            })
            .collect();
        assert_eq!(
            summary,
            ["acme", "n", "x", "m", "u", "a", "e", "silent", "loud"]
        );
    }

    // ---- database and bucket ----

    fn single_database(src: &str) -> Database {
        match single_statement(src) {
            Stmt::Database(db) => db,
            other => panic!("expected a database: {other:?}"),
        }
    }

    fn single_bucket(src: &str) -> Bucket {
        match single_statement(src) {
            Stmt::Bucket(bucket) => bucket,
            other => panic!("expected a bucket: {other:?}"),
        }
    }

    /// spec/database.md: the surface forms, the `db` alias, and value
    /// (not spelling) equality of durations.
    #[test]
    fn database_forms_parse_to_presence() {
        let expected = Database {
            name: Ident::new("blog"),
            keyword: Carried::default(),
            strict: false,
            changefeed: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        };
        assert_eq!(single_database("database blog;"), expected);
        assert_eq!(single_database("database blog { };"), expected);
        // Rule 9's lexer-level alias.
        assert_eq!(single_database("db blog;"), expected);
        // A `///` block attaches (the statement has a COMMENT clause).
        let db = single_database("/// Primary application database\ndatabase blog;");
        assert_eq!(
            db.comment.expect("doc attached").lines,
            ["Primary application database"]
        );
        assert!(single_database("database blog { strict; };").strict);
        // The spec's changefeed example.
        assert_eq!(
            single_database("database blog { changefeed { expiry: 7d; include original; }; };")
                .changefeed,
            Some(ChangeFeed {
                expiry: Duration::from_secs(7 * 86_400),
                include_original: true,
            })
        );
        // Clause and sub-clause order are free (rule 7).
        let db = single_database(
            "database blog { strict; changefeed { include original; expiry: 7d; }; };",
        );
        assert!(db.strict);
        assert!(db.changefeed.as_ref().expect("parsed").include_original);
        // Durations compare by value: `90m` ≡ `1h30m` (the normalizer
        // compares parsed values, never spelling).
        assert_eq!(
            single_database("database a { changefeed { expiry: 90m; }; };"),
            single_database("database a { changefeed { expiry: 1h30m; }; };")
        );
        // Repeated clauses fold last-one-wins (rule 7) — inside the
        // sub-block too: `expiry` repeats overwrite, exactly like
        // upstream's positional clause loop.
        let db = single_database(
            "database a { changefeed { expiry: 1d; expiry: 2d; include original; }; strict; strict; };",
        );
        assert_eq!(
            db.changefeed.expect("parsed").expiry,
            Duration::from_secs(2 * 86_400)
        );
        // The file's final statement may omit `;` in every form —
        // but a clause's own terminator is never optional: the
        // sub-block's `};` is grammar (spec/database.md), so a
        // `}`-closed sub-block mid-block is diagnosed.
        assert!(parse_ok("database blog { strict; }").items.len() == 1);
        assert!(parse_ok("database blog").items.len() == 1);
        assert!(
            parse_ok("database blog { strict; changefeed { expiry: 1d; }; }")
                .items
                .len()
                == 1
        );
        assert!(matches!(
            first_error("database blog { strict; changefeed { expiry: 1d; } }"),
            Diag::ExpectedStatementEnd { .. }
        ));
        // Names follow SurrealQL's rules (rule 9).
        assert_eq!(
            single_database("database `table`;").name,
            Ident::new("table")
        );
        assert!(matches!(
            first_error("database select;"),
            Diag::ReservedIdent { .. }
        ));
    }

    /// The flag/sub-block separator matrix (rule 2): the wrong
    /// separator is [`Diag::UnexpectedClauseForm`], and a changefeed
    /// without its required `expiry` is dropped while the statement
    /// survives.
    #[test]
    fn database_clause_forms_and_required_expiry() {
        assert!(matches!(
            first_error("database blog { strict: true; };"),
            Diag::UnexpectedClauseForm { .. }
        ));
        assert!(matches!(
            first_error("database blog { strict { }; };"),
            Diag::UnexpectedClauseForm { .. }
        ));
        assert!(matches!(
            first_error("database blog { changefeed; };"),
            Diag::UnexpectedClauseForm { .. }
        ));
        assert!(matches!(
            first_error("database blog { changefeed: 7d; };"),
            Diag::UnexpectedClauseForm { .. }
        ));
        // Sub-block recovery: a failing clause inside the block is
        // one root cause, one diagnostic — the trailing `;` demand
        // goes quiet so no report aims at the enclosing `}`.
        let (_, diags) = parse_file("database b { changefeed { expiry } };\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::ExpectedClauseValue { key: "expiry", .. }]
        ));
        let (_, diags) = parse_file("database b { changefeed { expiry: 1d } };\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::UnbalancedCapture { bracket: '}', .. }]
        ));
        // A changefeed block without `expiry` — empty or not — is
        // diagnosed against the changefeed key and dropped.
        let (file, diags) =
            parse_file("/// blog\ndatabase blog { changefeed { include original; }; };\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::RequiredClauseMissing {
                clause: "expiry",
                block: "changefeed",
                ..
            }]
        ));
        let Stmt::Database(db) = file.items.first().expect("kept") else {
            panic!("expected a database: {file:?}");
        };
        assert!(db.changefeed.is_none());
        assert!(db.comment.is_some());
        assert!(matches!(
            first_error("database blog { changefeed { }; };"),
            Diag::RequiredClauseMissing { .. }
        ));
        // A last-one-wins fold where the later block is invalid keeps
        // the earlier valid one (invalid clauses never enter the
        // fold).
        let (file, diags) = parse_file(
            "database a { changefeed { expiry: 1d; }; changefeed { include original; }; };\n",
        );
        assert!(matches!(
            diags.as_slice(),
            [Diag::RequiredClauseMissing { .. }]
        ));
        let Stmt::Database(db) = file.items.first().expect("kept") else {
            panic!("expected a database: {file:?}");
        };
        assert!(db.changefeed.is_some());
        // Sub-block clauses are table-scoped: `include original`
        // outside the changefeed cannot extend (no candidate
        // continues `include` in `database`'s table), so the unknown
        // key is the first word and recovery skips the rest.
        let diag = first_error("database blog { include original; };");
        assert!(matches!(
            diag,
            Diag::UnknownClause { ref key, statement: "database", .. } if key == "include"
        ));
        // Clause keys are SDL keywords spelled bare (rule 9): a
        // quoted spelling is an identifier, not a key — diagnosed
        // without a did-you-mean, quoting clears nothing here.
        let diag = first_error("database blog { `strict`; };");
        assert!(matches!(
            diag,
            Diag::UnknownClause { ref key, ref suggestion, statement: "database", .. }
                if key == "strict" && suggestion.is_none()
        ));
    }

    /// Multi-word clause keys: the extended spelling matches, while
    /// a truncated or broken extension is diagnosed with the full
    /// joined spelling and a did-you-mean.
    #[test]
    fn multi_word_keys_extend_and_diagnose_whole() {
        assert!(matches!(
            first_error("database blog { changefeed { expiry: 1d; include; }; };"),
            Diag::UnknownClause {
                ref key,
                ref suggestion,
                ..
            } if key == "include" && suggestion.as_deref() == Some("include original")
        ));
        // A word that completes no candidate joins the key: one
        // diagnosis over the whole misspelling, recovery past it.
        let diag = first_error("database blog { changefeed { expiry: 1d; include exp; }; };");
        assert!(matches!(
            diag,
            Diag::UnknownClause { ref key, .. } if key == "include exp"
        ));
        // Trivia between the key's words is SDL trivia.
        assert!(
            parse_ok("database blog { changefeed { expiry: 1d; include\n  original; }; };")
                .items
                .len()
                == 1
        );
    }

    /// The SDL duration validator mirrors the v3.2.4 duration lexer:
    /// unit set, compounds, `_` separators, checked arithmetic; and
    /// it agrees with the embedded parser's own `syn::duration` on
    /// every corpus member (value, not spelling).
    #[test]
    fn duration_literals_follow_upstream_lexing() {
        // The full pipeline: a bad duration in the sub-block is
        // SDL-E1012 over the capture — and only that diagnostic (the
        // failed attempt suppresses the required-clause report) — the
        // changefeed is dropped, and the statement survives.
        let (file, diags) = parse_file("database a { changefeed { expiry: 7days; }; };\n");
        let Some(Diag::InvalidDuration { text, .. }) = diags.first() else {
            panic!("expected InvalidDuration: {diags:?}");
        };
        assert_eq!(text, "7days");
        assert_eq!(diags.len(), 1, "one root cause, one diagnostic: {diags:?}");
        let Stmt::Database(db) = file.items.first().expect("kept") else {
            panic!("expected a database: {file:?}");
        };
        assert!(db.changefeed.is_none());
        // A duration key with no colon is the value-clause error,
        // pinned unchanged from `sequence`; an empty capture reads as
        // the plain missing-value message, not the malformed-text one.
        assert!(matches!(
            first_error("database a { changefeed { expiry; }; };"),
            Diag::ExpectedClauseValue { key: "expiry", .. }
        ));
        let diag = first_error("database a { changefeed { expiry: ; }; };");
        let Diag::InvalidDuration { text, .. } = diag else {
            panic!("expected InvalidDuration: {diag:?}");
        };
        assert!(text.is_empty());
        assert_eq!(
            parse_duration_text("7d"),
            Some(Duration::from_secs(7 * 86_400))
        );
        assert_eq!(
            parse_duration_text("1h30m"),
            Some(Duration::from_secs(5_400))
        );
        assert_eq!(
            parse_duration_text("1_000ms"),
            Some(Duration::from_millis(1_000))
        );
        assert_eq!(
            parse_duration_text("500us"),
            Some(Duration::from_micros(500))
        );
        assert_eq!(
            parse_duration_text("500\u{b5}s"),
            Some(Duration::from_micros(500))
        );
        assert_eq!(
            parse_duration_text("100ns"),
            Some(Duration::from_nanos(100))
        );
        assert_eq!(parse_duration_text("0s"), Some(Duration::from_secs(0)));
        assert_eq!(
            parse_duration_text("2w1y"),
            Some(Duration::from_secs(2 * 604_800 + 31_536_000))
        );
        // Comments may surround the literal (rule 3's comment model).
        assert_eq!(
            parse_duration_text("1d /* days */"),
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(
            parse_duration_text("// leading\n1d"),
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(
            parse_duration_text("1d -- trailing"),
            Some(Duration::from_secs(86_400))
        );
        // Malformed shapes.
        for text in [
            "",
            "7",
            "7x",
            "7 d",
            "7days",
            "7D",
            "1.5h",
            "-5s",
            "1h 30m",
            "d",
            "s",
            "_500ms",
            "'1d'",
            "`1d`",
            "1h/*x*/30m",
            "18446744073709551616s",
            "600000000000y",
            "18446744073709551615s1s",
            "1d /* oops",
        ] {
            assert_eq!(parse_duration_text(text), None, "text: {text:?}");
        }
    }

    #[test]
    fn duration_validator_agrees_with_the_embedded_parser() {
        for text in [
            "7d",
            "1h30m",
            "1_000ms",
            "0s",
            "500us",
            "500\u{b5}s",
            "90m",
            "100ns",
            "0ns",
            "12h30m15s",
            "2w",
            "1y",
        ] {
            let ours = parse_duration_text(text).unwrap_or_else(|| panic!("{text} must parse"));
            let theirs = surrealdb_core::syn::duration(text)
                .unwrap_or_else(|error| panic!("{text}: upstream rejected: {error}"));
            assert_eq!(ours.nanos(), theirs.nanos(), "{text}");
        }
        // Both reject the same malformed shapes.
        for text in ["7", "7x", "1h 30m", "1.5h", "-5s", "7days", "7 d", "7D"] {
            assert!(parse_duration_text(text).is_none(), "{text}");
            assert!(surrealdb_core::syn::duration(text).is_err(), "{text}");
        }
    }

    /// spec/bucket.md: the surface forms and clause arms.
    #[test]
    fn bucket_forms_parse_to_presence() {
        let expected = Bucket {
            name: Ident::new("shared"),
            backend: None,
            readonly: false,
            permissions: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        };
        assert_eq!(single_bucket("bucket shared;"), expected);
        assert_eq!(single_bucket("bucket shared { };"), expected);
        // Backend expression canon + doc comment.
        let bucket = single_bucket(
            "/// Ephemeral test uploads\nbucket scratch_uploads { backend: 'memory'; };",
        );
        assert_eq!(
            bucket.backend,
            Some(Expr {
                canon: "'memory'".to_string()
            })
        );
        assert_eq!(
            bucket.comment.expect("doc attached").lines,
            ["Ephemeral test uploads"]
        );
        // Readonly + free clause order (rule 7).
        let bucket = single_bucket("bucket published_reports { readonly; backend: 'memory'; };");
        assert!(bucket.readonly);
        assert!(bucket.backend.is_some());
        // The permissions keyword arms (ASCII-case-insensitive) and
        // the expression arm through bucket's own slot.
        assert_eq!(
            single_bucket("bucket x { permissions: none; };").permissions,
            Some(Permission::None)
        );
        assert_eq!(
            single_bucket("bucket x { permissions: FULL; };").permissions,
            Some(Permission::Full)
        );
        assert_eq!(
            single_bucket("bucket x { permissions: $auth.admin = true; };").permissions,
            Some(Permission::Expr(Expr {
                canon: "$auth.admin = true".to_string()
            }))
        );
        // Keyword detection rides the canonical print: trivia may
        // interrupt the keyword (rule 3's comment model), and a bare
        // `full` idiom must read as the keyword — as the expression
        // it evaluates falsy at runtime, silently flipping an allow
        // into a deny. A quoted `'none'` stays an expression.
        assert_eq!(
            single_bucket("bucket x { permissions: full /* allow */; };").permissions,
            Some(Permission::Full)
        );
        assert_eq!(
            single_param("param $x { permissions: // grant\nfull; };").permissions,
            Some(Permission::Full)
        );
        assert!(matches!(
            single_bucket("bucket x { permissions: 'none'; };").permissions,
            Some(Permission::Expr(_))
        ));
        // Last-one-wins on expressions.
        assert_eq!(
            single_bucket("bucket x { backend: 'a'; backend: 'b'; };").backend,
            Some(Expr {
                canon: "'b'".to_string()
            })
        );
        // The file's final statement may omit `;`.
        assert!(parse_ok("bucket shared").items.len() == 1);
        // The flag separator matrix, and `backend`'s bare-key form is
        // the value-clause error (pinned, unchanged from `sequence`).
        assert!(matches!(
            first_error("bucket x { readonly: true; };"),
            Diag::UnexpectedClauseForm { .. }
        ));
        assert!(matches!(
            first_error("bucket x { readonly { }; };"),
            Diag::UnexpectedClauseForm { .. }
        ));
        assert!(matches!(
            first_error("bucket x { backend; };"),
            Diag::ExpectedClauseValue { key: "backend", .. }
        ));
    }

    /// Broken database/bucket statements do not hide their siblings —
    /// and like every clause-level error, a wrong-form clause keeps
    /// its own statement alive (only a failed capture drops one).
    #[test]
    fn broken_new_scoped_statements_do_not_hide_their_siblings() {
        let (file, diags) =
            parse_file("database a { oops: 1; };\nnamespace b;\nbucket c { readonly: x; };\n");
        assert_eq!(
            diags.len(),
            2,
            "unknown clause + wrong flag form: {diags:?}"
        );
        assert_eq!(
            file.items.len(),
            3,
            "every statement survives clause errors"
        );
        // A rejected backend capture keeps its statement (minus the
        // clause) — only a statement-level short-form capture drops
        // one; the namespace sibling is untouched either way.
        let (file, diags) = parse_file("bucket bad { backend: 1 2; };\nnamespace ok;\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::EmbeddedParseError { .. }]
        ));
        assert_eq!(file.items.len(), 2);
        // An unterminated changefeed sub-block swallows the following
        // statement into its recovery: the misfiled `namespace` word
        // is the unknown clause it reads as, then EOF leaves both the
        // sub-block and the outer block unterminated — each reported
        // against its own `{`, and nothing parses.
        let (file, diags) = parse_file("database a { changefeed { expiry: 1d;\nnamespace b;\n");
        assert!(matches!(
            diags.as_slice(),
            [
                Diag::UnknownClause { .. },
                Diag::UnterminatedBlock { .. },
                Diag::UnterminatedBlock { .. },
            ]
        ));
        assert!(file.items.is_empty());
    }

    // ---- user and analyzer ----

    fn single_user(src: &str) -> User {
        match single_statement(src) {
            Stmt::User(user) => user,
            other => panic!("expected a user: {other:?}"),
        }
    }

    fn single_analyzer(src: &str) -> Analyzer {
        match single_statement(src) {
            Stmt::Analyzer(analyzer) => analyzer,
            other => panic!("expected an analyzer: {other:?}"),
        }
    }

    fn single_event(src: &str) -> Event {
        match single_statement(src) {
            Stmt::Event(event) => event,
            other => panic!("expected an event: {other:?}"),
        }
    }

    /// spec/user.md: the surface forms, the input-only `on:` aliases
    /// (rule 9), the value-not-spelling equality of decoded strings,
    /// and last-one-wins folds.
    #[test]
    fn user_forms_parse_to_presence() {
        let expected = User {
            name: Ident::new("analyst"),
            on: Authored::canonical(Level::Database),
            pass: None,
            scram: None,
            roles: Vec::new(),
            token_duration: None,
            session_duration: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        };
        assert_eq!(single_user("user analyst { on: database; };"), expected);
        // `db` is an input-only alias: same AST (the authored spelling
        // is carried, never compared), canonical emission at codegen.
        assert_eq!(single_user("user analyst { on: db; };"), expected);
        assert_eq!(
            single_user("user analyst { on: kv; };").on.value,
            Level::Root
        );
        assert_eq!(
            single_user("user analyst { on: ns; };").on.value,
            Level::Namespace
        );
        // A `///` block attaches (the statement has a COMMENT clause).
        let user = single_user("/// CI deployment account\nuser deployer { on: namespace; };");
        assert_eq!(
            user.comment.expect("doc attached").lines,
            ["CI deployment account"]
        );
        // The spec's passhash example: multi-role + both duration sides.
        let user = single_user(
            "user deployer { on: namespace; passhash: '$argon2id$'; roles: editor, viewer; \
             duration { token: 1h; session: 12h; }; };",
        );
        assert_eq!(user.pass, Some(Pass::Hash("$argon2id$".to_string())));
        assert_eq!(user.roles, [Role::Editor, Role::Viewer]);
        assert_eq!(
            user.token_duration,
            Some(Expr {
                canon: "1h".to_string()
            })
        );
        assert_eq!(
            user.session_duration,
            Some(Expr {
                canon: "12h".to_string()
            })
        );
        // Partial duration blocks keep only the present side.
        let user = single_user("user u { on: root; duration { token: 30m; }; };");
        assert!(user.token_duration.is_some() && user.session_duration.is_none());
        // Strings decode to their VALUE: escape spellings are equal.
        assert_eq!(
            single_user("user u { on: root; password: 'a\\nb'; };").pass,
            single_user("user u { on: root; password: \"a\\nb\"; };").pass
        );
        assert_eq!(
            single_user("user u { on: root; password: 'a\\nb'; };").pass,
            Some(Pass::Password("a\nb".to_string()))
        );
        // Repeated clauses fold last-one-wins (rule 7): the pass
        // value, the whole roles list, and the whole duration block.
        let user = single_user(
            "user u { on: root; passhash: 'one'; passhash: 'two'; roles: owner; roles: viewer; \
             duration { token: 1h; }; duration { session: 2h; }; };",
        );
        assert_eq!(user.pass, Some(Pass::Hash("two".to_string())));
        assert_eq!(user.roles, [Role::Viewer]);
        assert!(user.token_duration.is_none() && user.session_duration.is_some());
        // Free clause order (rule 7), wrapped role list, and the
        // final statement's optional `;`.
        assert!(
            parse_ok("user u { roles: owner,\n  editor; on: root; duration { token: 1h; }; }")
                .items
                .len()
                == 1
        );
        // Duplicate roles ride through (SurrealQL parity; the linter
        // flags duplicates).
        assert_eq!(
            single_user("user u { on: root; roles: viewer, viewer; };").roles,
            [Role::Viewer, Role::Viewer]
        );
    }

    /// `passscram:` (spec/user.md, 3.3): the verifier folds into its
    /// own slot — ADDITIVE with the pass clauses (unlike their mutual
    /// exclusivity) — repeats fold last-one-wins between valid values,
    /// and a value the engine's parse-time check would reject
    /// diagnoses SDL-E1059 and drops the statement (accepted SDL must
    /// emit engine-parseable DDL). The sentinel literal is one of
    /// those rejections — sharper than the passhash sentinel's lint:
    /// a PASSSCRAM verifier never round-trips (INFO redacts it AND
    /// the sentinel is define-rejected), so the parse error names the
    /// trap instead of deferring to an apply that can never work.
    #[test]
    fn user_passscram_folds_and_validates() {
        const KAT: &str = "SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=";
        // Scram-only and coexisting with a pass clause.
        let user = single_user(&format!("user u {{ on: root; passscram: '{KAT}'; }};"));
        assert_eq!(user.scram.as_deref(), Some(KAT));
        assert!(user.pass.is_none());
        let user = single_user(&format!(
            "user u {{ on: root; passhash: 'h'; passscram: '{KAT}'; }};"
        ));
        assert_eq!(user.pass, Some(Pass::Hash("h".to_string())));
        assert_eq!(user.scram.as_deref(), Some(KAT));
        // Last-one-wins (rule 7) — between VALID values; an invalid
        // one drops the statement regardless of position.
        let user = single_user(&format!(
            "user u {{ on: root; passscram: 'SCRAM-SHA-256$8192:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20='; passscram: '{KAT}'; }};"
        ));
        assert_eq!(user.scram.as_deref(), Some(KAT));
        // Every invalid shape the engine's parse-time check rejects —
        // E1059 + the statement drops (fail-closed). The sentinel is
        // the loop's headliner: its message names the round-trip trap.
        let sentinel = first_error("user u { on: root; passscram: '[REDACTED]'; };");
        assert!(matches!(
            sentinel,
            Diag::InvalidPassscram { ref message, .. }
                if message.contains("never round-trips")
        ));
        for (bad, why) in [
            ("garbage", "missing mechanism"),
            (
                "SCRAM-SHA-1$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=",
                "wrong mechanism",
            ),
            (
                "SCRAM-SHA-256$4095:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=",
                "iterations below the floor",
            ),
            (
                "SCRAM-SHA-256$16777217:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=",
                "iterations above the ceiling",
            ),
            (
                "SCRAM-SHA-256$nope:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=",
                "non-numeric iterations",
            ),
            ("SCRAM-SHA-256$4096:salt-without-$", "missing key separator"),
            (
                "SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$short=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=",
                "a truncated key",
            ),
            (
                "SCRAM-SHA-256$4096:!!!$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=",
                "non-base64 salt",
            ),
        ] {
            let (file, diags) =
                parse_file(&format!("user u {{ on: root; passscram: '{bad}'; }};\n"));
            assert!(
                matches!(diags.as_slice(), [Diag::InvalidPassscram { .. }]),
                "{why}: {diags:?}"
            );
            assert!(file.items.is_empty(), "{why}: the statement drops");
        }
    }

    /// The verifier mirror's shape table — the charset+length base64
    /// arithmetic (no decoder) against surrealdb-iam's
    /// `from_verifier_string` semantics.
    #[test]
    fn scram_verifier_shape_table() {
        // 44 chars, one `=` → 32 bytes (the key shape).
        assert_eq!(
            super::base64_byte_len("nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM="),
            Some(32)
        );
        // 24 chars, two `=` → 16 bytes (the generated salt shape).
        assert_eq!(super::base64_byte_len("MDEyMzQ1Njc4OWFiY2RlZg=="), Some(16));
        assert_eq!(super::base64_byte_len(""), None);
        assert_eq!(super::base64_byte_len("abc"), None, "length % 4");
        assert_eq!(super::base64_byte_len("ab$c"), None, "charset");
        assert_eq!(super::base64_byte_len("A==="), None, "padding > 2");
        // `+` and `/` are standard-alphabet members.
        assert_eq!(
            super::base64_byte_len("jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20="),
            Some(32)
        );
    }

    /// `on:` is required (upstream `expected!(ON)`): the missing
    /// clause is diagnosed against the block — or the would-be block
    /// position on the bare form — and the statement dropped.
    #[test]
    fn user_requires_on() {
        let (file, diags) = parse_file("/// doc\nuser analyst { roles: viewer; };\nnamespace k;\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::RequiredClauseMissing {
                clause: "on",
                block: "user",
                ..
            }]
        ));
        assert!(matches!(file.items.first(), Some(Stmt::Namespace(_))));
        // Bare form: no block, the same required-clause report.
        assert!(matches!(
            first_error("user analyst;"),
            Diag::RequiredClauseMissing { clause: "on", .. }
        ));
        // A failed `on` attempt already carried its own diagnostic;
        // the missing-clause report goes quiet (one root cause, one
        // diagnostic), and the statement still drops.
        let (file, diags) = parse_file("user analyst { on: roto; };\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::UnknownClauseWord { key: "on", .. }]
        ));
        assert!(file.items.is_empty());
    }

    /// The pass-clause surface: string validation, and upstream's
    /// parser-enforced exclusivity ("Can't set both a passhash and a
    /// password").
    #[test]
    fn user_pass_clauses() {
        // A non-string value is SDL-E1016 over the capture.
        assert!(matches!(
            first_error("user u { on: root; password: nope; };"),
            Diag::InvalidString { .. }
        ));
        // Prefixed strings are not string literals in this position.
        assert!(matches!(
            first_error("user u { on: root; password: u'x'; };"),
            Diag::InvalidString { .. }
        ));
        // The two clauses together: the second is diagnosed, no pass
        // survives, the statement does.
        let (file, diags) = parse_file("user u { on: root; passhash: 'h'; password: 'p'; };\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::MutuallyExclusive {
                clause: "password",
                other: "passhash",
                ..
            }]
        ));
        let Stmt::User(user) = file.items.first().expect("kept") else {
            panic!("expected a user: {file:?}");
        };
        assert!(user.pass.is_none());
        // Either order reports the later clause; a second collision
        // reports against the first-seen clause.
        assert!(matches!(
            first_error("user u { on: root; password: 'p'; passhash: 'h'; };"),
            Diag::MutuallyExclusive {
                clause: "passhash",
                other: "password",
                ..
            }
        ));
        // A failed first attempt does not collide with a valid second
        // (one root cause, one diagnostic — the valid value wins).
        let (file, diags) = parse_file("user u { on: root; password: nope; passhash: 'h'; };\n");
        assert!(matches!(diags.as_slice(), [Diag::InvalidString { .. }]));
        let Stmt::User(user) = file.items.first().expect("kept") else {
            panic!("expected a user: {file:?}");
        };
        assert_eq!(user.pass, Some(Pass::Hash("h".to_string())));
    }

    /// Keyword-set and keyword-list clauses: unknown words diagnose
    /// with did-you-mean; entries are bare lowercase words (rule 9).
    #[test]
    fn keyword_lists_validate_entries() {
        let diag = first_error("user u { on: root; roles: ownr; };");
        assert!(matches!(
            diag,
            Diag::UnknownClauseWord {
                ref word,
                ref suggestion,
                key: "roles",
                ..
            } if word == "ownr" && suggestion.as_deref() == Some("owner")
        ));
        // Uppercase spellings are part of the misspelling (rule 9:
        // SDL keywords are lowercase only).
        assert!(matches!(
            first_error("user u { on: root; roles: OWNER; };"),
            Diag::UnknownClauseWord { suggestion: Some(suggestion), .. } if suggestion == "owner"
        ));
        // Quoted spellings are identifiers, not keyword entries.
        assert!(matches!(
            first_error("user u { on: root; roles: `owner`; };"),
            Diag::UnknownClauseWord { .. }
        ));
        // A trailing comma leaves an empty entry.
        assert!(matches!(
            first_error("user u { on: root; roles: viewer,; };"),
            Diag::UnknownClauseWord {
                ref word,
                key: "roles",
                ..
            } if word.is_empty()
        ));
        // Every bad entry is diagnosed; one bad entry fails the whole
        // clause (the fold skips it) while the statement survives.
        let (file, diags) = parse_file("user u { on: root; roles: editr, viewr; };\n");
        assert_eq!(diags.len(), 2, "both bad entries: {diags:?}");
        let Stmt::User(user) = file.items.first().expect("kept") else {
            panic!("expected a user: {file:?}");
        };
        assert!(user.roles.is_empty());
        // Comments and trivia around entries (rule 3's comment model).
        let user = single_user("user u { on: /* lvl */ root; roles: owner /* pick */, viewer; };");
        assert!(
            user.on == Authored::canonical(Level::Root)
                && user.roles == [Role::Owner, Role::Viewer]
        );
        // The `on` set's did-you-mean covers the aliases too.
        assert!(matches!(
            first_error("user u { on: roto; };"),
            Diag::UnknownClauseWord { suggestion: Some(s), key: "on", .. } if s == "root"
        ));
    }

    /// spec/analyzer.md: the surface forms, fn-path shapes, list
    /// folds, and filter atoms.
    #[test]
    fn analyzer_forms_parse_to_presence() {
        let expected = Analyzer {
            name: Ident::new("empty"),
            function: None,
            tokenizers: None,
            filters: None,
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        };
        assert_eq!(single_analyzer("analyzer empty;"), expected);
        assert_eq!(single_analyzer("analyzer empty { };"), expected);
        // A `///` block attaches.
        let analyzer =
            single_analyzer("/// English text search\nanalyzer english { tokenizers: class; };");
        assert_eq!(
            analyzer.comment.expect("doc attached").lines,
            ["English text search"]
        );
        // Every clause, with a snowball alias canonicalizing at parse
        // time (`en` → English) and a parameterized atom.
        let analyzer = single_analyzer(
            "analyzer english { function: fn::strip_markup; tokenizers: class; \
             filters: snowball(en), lowercase, edgengram(2, 10); };",
        );
        assert_eq!(
            analyzer.function,
            Some(FnPath {
                kind: FnKind::Fn,
                segments: vec!["strip_markup".to_string()],
            })
        );
        assert_eq!(analyzer.tokenizers, Some(vec![Tokenizer::Class]));
        assert_eq!(
            analyzer.filters,
            Some(vec![
                Filter::Snowball(Authored::canonical(Language::English)),
                Filter::Lowercase,
                Filter::EdgeNgram(2, 10),
            ])
        );
        // Wrapped lists split at depth-0 commas (the spec's example).
        let analyzer =
            single_analyzer("analyzer everything { tokenizers: blank,\n  camel,\n  punct; };");
        assert_eq!(
            analyzer.tokenizers,
            Some(vec![Tokenizer::Blank, Tokenizer::Camel, Tokenizer::Punct])
        );
        // Repeating a list clause replaces the whole list (rule 7).
        let analyzer = single_analyzer("analyzer a { tokenizers: class; tokenizers: punct; };");
        assert_eq!(analyzer.tokenizers, Some(vec![Tokenizer::Punct]));
        // `no` is the parser's Norwegian alias; `ngram` parses its u16
        // pair; mapper decodes its path.
        let analyzer = single_analyzer(
            "analyzer a { filters: snowball(no), ngram(3, 5), mapper('/etc/x.txt'); };",
        );
        assert_eq!(
            analyzer.filters,
            Some(vec![
                Filter::Snowball(Authored::canonical(Language::Norwegian)),
                Filter::Ngram(3, 5),
                Filter::Mapper("/etc/x.txt".to_string()),
            ])
        );
        // The full-name spellings and the 2/3-letter aliases land on
        // the same variants.
        assert_eq!(
            single_analyzer("analyzer a { filters: snowball(deu); };").filters,
            single_analyzer("analyzer a { filters: snowball(german); };").filters
        );
        assert_eq!(
            single_analyzer("analyzer a { filters: snowball(de); };").filters,
            single_analyzer("analyzer a { filters: snowball(deu); };").filters
        );
        // Comments around atoms (rule 3's comment model).
        assert!(
            parse_ok("analyzer a { filters: /* pre */ ascii, lowercase /* post */; };")
                .items
                .len()
                == 1
        );
        // The final statement may omit `;`.
        assert!(parse_ok("analyzer a { tokenizers: class; }").items.len() == 1);
    }

    /// The `<fn-path>` grammar: prefixes, multi-segment paths, quoted
    /// and keyword-like segments, and the malformed shapes.
    #[test]
    fn fn_paths_validate() {
        // mod:: paths parse SDL-side (the server enforces its
        // surrealism capability at define time).
        let analyzer = single_analyzer("analyzer a { function: mod::demo::some; };");
        assert_eq!(
            analyzer.function,
            Some(FnPath {
                kind: FnKind::Mod,
                segments: vec!["demo".to_string(), "some".to_string()],
            })
        );
        // Keyword-like segments ride bare; quoted segments unescape.
        assert_eq!(
            single_analyzer("analyzer a { function: fn::class; };")
                .function
                .expect("parsed")
                .segments,
            ["class"]
        );
        assert_eq!(
            single_analyzer("analyzer a { function: fn::`weird name`; };")
                .function
                .expect("parsed")
                .segments,
            ["weird name"]
        );
        // A quoted PREFIX is an identifier, not the keyword.
        assert!(matches!(
            first_error("analyzer a { function: `fn`::x; };"),
            Diag::InvalidFnPath { .. }
        ));
        // A missing prefix.
        assert!(matches!(
            first_error("analyzer a { function: strip_markup; };"),
            Diag::InvalidFnPath { .. }
        ));
        // ml::/silo:: mirror upstream's rejections.
        assert!(matches!(
            first_error("analyzer a { function: ml::x; };"),
            Diag::InvalidFnPath { .. }
        ));
        assert!(matches!(
            first_error("analyzer a { function: silo::x; };"),
            Diag::InvalidFnPath { .. }
        ));
        // Trailing call parens.
        assert!(matches!(
            first_error("analyzer a { function: fn::name(); };"),
            Diag::InvalidFnPath { .. }
        ));
        // A missing segment after `::`, and a bare prefix — each with
        // its own message, pinned (they steer different fixes).
        match first_error("analyzer a { function: fn::; };") {
            Diag::InvalidFnPath { message, .. } => assert!(
                message.contains("expected a path segment after `::`"),
                "message: {message}"
            ),
            other => panic!("expected InvalidFnPath: {other:?}"),
        }
        match first_error("analyzer a { function: fn; };") {
            Diag::InvalidFnPath { message, .. } => assert!(
                message.contains("expected `::<segment>` after the prefix"),
                "message: {message}"
            ),
            other => panic!("expected InvalidFnPath: {other:?}"),
        }
        // Trailing junk after the path.
        assert!(matches!(
            first_error("analyzer a { function: fn::x y; };"),
            Diag::InvalidFnPath { .. }
        ));
        // A lone `:` at the path's end is never swallowed — it reads
        // as trailing junk, not a truncated path.
        assert!(matches!(
            first_error("analyzer a { function: fn::foo:; };"),
            Diag::InvalidFnPath { .. }
        ));
        // Comments are trivia inside the capture too (rule 3).
        assert_eq!(
            single_analyzer("analyzer a { function: fn::strip /* x */::markup; };")
                .function
                .expect("parsed")
                .segments,
            ["strip", "markup"]
        );
    }

    /// Filter atoms validate their arguments: wrong names, malformed
    /// or overflowing u16 pairs, unknown languages, non-string mapper
    /// paths.
    #[test]
    fn filter_atoms_validate() {
        let bad = |src: &str| match first_error(src) {
            Diag::InvalidFilter { text, .. } => text,
            other => panic!("expected InvalidFilter: {other:?}"),
        };
        assert_eq!(
            bad("analyzer a { filters: edgengram(2); };"),
            "edgengram(2)"
        );
        assert_eq!(
            bad("analyzer a { filters: edgengram(2, 10, 3); };"),
            "edgengram(2, 10, 3)"
        );
        assert_eq!(bad("analyzer a { filters: ngram(x, 2); };"), "ngram(x, 2)");
        assert_eq!(bad("analyzer a { filters: ngram(2, x); };"), "ngram(2, x)");
        // Overflowing u16 and a trailing `_` (not digits upstream).
        assert_eq!(
            bad("analyzer a { filters: ngram(70000, 1); };"),
            "ngram(70000, 1)"
        );
        assert_eq!(
            bad("analyzer a { filters: ngram(2_, 3); };"),
            "ngram(2_, 3)"
        );
        // A string language, an unknown language, and a bare unknown
        // atom are all SDL-E1017 over the entry.
        assert_eq!(
            bad("analyzer a { filters: snowball('english'); };"),
            "snowball('english')"
        );
        assert_eq!(
            bad("analyzer a { filters: snowball(klingon); };"),
            "snowball(klingon)"
        );
        assert_eq!(bad("analyzer a { filters: stem; };"), "stem");
        // A non-string mapper path.
        assert_eq!(
            bad("analyzer a { filters: mapper(/etc/x); };"),
            "mapper(/etc/x)"
        );
        // min > max rides through (SurrealQL accepts it; the linter
        // flags it).
        let analyzer = single_analyzer("analyzer a { filters: edgengram(10, 2); };");
        assert_eq!(analyzer.filters, Some(vec![Filter::EdgeNgram(10, 2)]));
        // `min ≤ max` acceptance and u16 boundary values.
        let analyzer = single_analyzer("analyzer a { filters: ngram(0, 65535); };");
        assert_eq!(analyzer.filters, Some(vec![Filter::Ngram(0, 65535)]));
    }

    /// Broken user/analyzer statements do not hide their siblings —
    /// and clause-level errors keep their own statement alive.
    #[test]
    fn broken_unit5_statements_do_not_hide_their_siblings() {
        // A junk clause keeps the statement (its `on` is present, so
        // the required-clause check passes) and its siblings too.
        let (file, diags) = parse_file("user u { on: root; oops: 1; };\nnamespace b;\n");
        assert!(matches!(diags.as_slice(), [Diag::UnknownClause { .. }]));
        assert_eq!(file.items.len(), 2);
        // An analyzer whose only defect is a rejected clause value
        // keeps the statement (minus the clause).
        let (file, diags) =
            parse_file("analyzer a { filters: oops(1); };\nuser u { on: root; };\n");
        assert!(matches!(diags.as_slice(), [Diag::InvalidFilter { .. }]));
        assert_eq!(file.items.len(), 2);
        // A bad `on` drops the user only (no required `on` to keep).
        let (file, diags) = parse_file("user bad { on: roto; };\nanalyzer ok;\n");
        assert_eq!(diags.len(), 1);
        assert!(matches!(file.items.first(), Some(Stmt::Analyzer(_))));
    }

    // ---- event ----

    /// spec/event.md: the surface forms — required `on`/`then`, the
    /// optional `when`, the async dependent group in both spellings,
    /// the expression comma list, and the rule-7 folds.
    #[test]
    fn event_forms_parse_to_presence() {
        // The minimal form; canon is the embedded print, not the
        // source spelling.
        assert_eq!(
            single_event(
                "event touch { on: post; then: (update counter:posts set total = total + 1); };"
            ),
            Event {
                name: Ident::new("touch"),
                on: Ident::new("post"),
                when: None,
                then: vec![Expr {
                    canon: "(UPDATE counter:posts SET total = total + 1)".to_string(),
                }],
                r#async: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            }
        );
        // A `///` block attaches; `when` canonicalizes (the
        // deprecated-assignment idiom rides the RETURN retry).
        let event = single_event(
            "/// Log every user creation\n\
             event user_created { on: user; when: $event = 'CREATE'; \
             then: { create log set at = time::now(); }; };",
        );
        assert_eq!(
            event.comment.expect("doc attached").lines,
            ["Log every user creation"]
        );
        assert_eq!(event.when.expect("when").canon, "$event = 'CREATE'");
        assert_eq!(
            event.then,
            vec![Expr {
                canon: "{ CREATE log SET at = time::now() }".to_string(),
            }]
        );
        // The comma list (SurrealQL's THEN a, b form).
        let event = single_event(
            "event two { on: payment; then: (create ledger set p = $after.id), \
             (update stats set n = n + 1); };",
        );
        assert_eq!(event.then.len(), 2);
        // Clause order is free (rule 7); a repeated `then:` replaces
        // the whole list.
        assert_eq!(
            single_event("event e { then: 1, 2; on: t; then: 3; };")
                .then
                .len(),
            1
        );
    }

    /// spec/event.md: `on` and `then` are required; a failed attempt
    /// is diagnosed at the clause, never re-reported as missing.
    #[test]
    fn event_requires_on_and_then() {
        // Both missing, both reported, anchored at the block.
        let (_, diags) = parse_file("event e { };\n");
        let clauses: Vec<&str> = diags
            .iter()
            .map(|diag| match diag {
                Diag::RequiredClauseMissing { clause, .. } => *clause,
                other => panic!("expected RequiredClauseMissing: {other:?}"),
            })
            .collect();
        assert_eq!(clauses, ["on", "then"]);
        // Each missing alone.
        assert!(matches!(
            first_error("event e { on: t; };"),
            Diag::RequiredClauseMissing { clause: "then", .. }
        ));
        assert!(matches!(
            first_error("event e { then: 1; };"),
            Diag::RequiredClauseMissing { clause: "on", .. }
        ));
        // A failed attempt is already diagnosed — no missing report.
        let (_, diags) = parse_file("event e { on: t; then: 1 COMMENT 'x'; };\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::EmbeddedParseError { .. }]
        ));
    }

    /// The `async` dependent group: flag and block share the key,
    /// folds are last-one-wins (rule 7), and a `:` selects neither
    /// form.
    #[test]
    fn event_async_dependent_group() {
        // The spec README's equivalence: `async; async { retry: 2; };`
        // ≡ `async { retry: 2; };` under last-one-wins.
        assert_eq!(
            single_event("event e { on: t; then: 1; async; async { retry: 2; }; };").r#async,
            single_event("event e { on: t; then: 1; async { retry: 2; }; };").r#async
        );
        // The reverse order folds to the bare flag; an empty block
        // lands there too.
        assert_eq!(
            single_event("event e { on: t; then: 1; async { retry: 2; }; async; };").r#async,
            Some(Async {
                retry: None,
                maxdepth: None
            })
        );
        assert_eq!(
            single_event("event e { on: t; then: 1; async { }; };").r#async,
            Some(Async {
                retry: None,
                maxdepth: None
            })
        );
        // Sides fold inside the block; retry 0 is a legal u16.
        assert_eq!(
            single_event(
                "event e { on: t; then: 1; async { maxdepth: 1; retry: 0; retry: 2; }; };"
            )
            .r#async,
            Some(Async {
                retry: Some(2),
                maxdepth: Some(1)
            })
        );
        // `async: 5;` is neither form — the message names both.
        match first_error("event e { on: t; then: 1; async: 5; };") {
            Diag::UnexpectedClauseForm { message, .. } => {
                assert!(
                    message.contains("`async;`") && message.contains("`async { … };`"),
                    "message: {message}"
                );
            }
            other => panic!("expected UnexpectedClauseForm: {other:?}"),
        }
    }

    /// The SDL-lexed clause values: `on:`'s identifier grammar
    /// (quoted spellings, reserved words, junk) and the async block's
    /// u16s.
    #[test]
    fn event_clause_values_validate() {
        // A quoted table name unescapes like any ident.
        assert_eq!(
            single_event("event e { on: `my table`; then: 1; };").on,
            Ident::new("my table")
        );
        // A bare reserved word cannot serve (rule 9, as in a name
        // position); a quoted spelling can.
        assert!(matches!(
            first_error("event e { on: select; then: 1; };"),
            Diag::ReservedIdent { .. }
        ));
        assert!(parse_ok("event e { on: `select`; then: 1; };").items.len() == 1);
        // Trailing junk and an empty value.
        assert!(matches!(
            first_error("event e { on: t extra; then: 1; };"),
            Diag::InvalidIdent { .. }
        ));
        assert!(matches!(
            first_error("event e { on: ; then: 1; };"),
            Diag::InvalidIdent { .. }
        ));
        // u16 values: the boundaries are legal (retry 0 disables
        // retries; upstream is unclamped above).
        assert_eq!(
            single_event("event e { on: t; then: 1; async { retry: 65535; maxdepth: 0; }; };")
                .r#async,
            Some(Async {
                retry: Some(65535),
                maxdepth: Some(0)
            })
        );
        // Overflow, sign, unit, and empty all diagnose.
        assert!(matches!(
            first_error("event e { on: t; then: 1; async { retry: 65536; }; };"),
            Diag::InvalidNumber { .. }
        ));
        assert!(matches!(
            first_error("event e { on: t; then: 1; async { retry: +5; }; };"),
            Diag::InvalidNumber { .. }
        ));
        assert!(matches!(
            first_error("event e { on: t; then: 1; async { maxdepth: 5s; }; };"),
            Diag::InvalidNumber { .. }
        ));
        assert!(matches!(
            first_error("event e { on: t; then: 1; async { retry: ; }; };"),
            Diag::InvalidNumber { .. }
        ));
        // A failed attempt suppresses the missing-clause report.
        let (_, diags) = parse_file("event e { on: t; then: 1; async { retry: x; }; };\n");
        assert!(matches!(diags.as_slice(), [Diag::InvalidNumber { .. }]));
        // Rule 3's comment model over captures: all four SurrealQL
        // comment syntaxes are trivia around the value.
        assert!(
            parse_ok("event e { on: /* pick */ t -- note\n; then: 1; };")
                .items
                .len()
                == 1
        );
        // `_` is the SurrealQL digit separator (value, not spelling);
        // a trailing `_` does not lex as digits upstream.
        assert_eq!(
            single_event("event e { on: t; then: 1; async { retry: 1_0 /* n */; }; };").r#async,
            Some(Async {
                retry: Some(10),
                maxdepth: None
            })
        );
        assert!(matches!(
            first_error("event e { on: t; then: 1; async { retry: 5_; }; };"),
            Diag::InvalidNumber { .. }
        ));
    }

    /// Broken event statements do not hide their siblings.
    #[test]
    fn broken_event_does_not_hide_its_siblings() {
        // A bad `on` drops the event (required clause unmet) but not
        // its siblings.
        let (file, diags) = parse_file("event bad { on: 1; then: 1; };\nnamespace ok;\n");
        assert!(matches!(diags.as_slice(), [Diag::InvalidIdent { .. }]));
        assert_eq!(file.items.len(), 1);
        // `retry` outside the block is an unknown clause over the
        // event table.
        let (_, diags) = parse_file("event e { on: t; then: 1; retry: 2; };");
        assert!(matches!(
            diags.first(),
            Some(Diag::UnknownClause { key, .. }) if key == "retry"
        ));
    }

    // ---- module ----

    fn single_module(src: &str) -> Module {
        match single_statement(src) {
            Stmt::Module(module) => module,
            other => panic!("expected a module: {other:?}"),
        }
    }

    /// spec/module.md: the surface forms — the named file module with
    /// its doc comment, the self-named silo package, the permissions
    /// arm — plus the rule-7 folds. Field-wise (not whole-struct):
    /// `from_span` anchors on source offsets, pinned separately in
    /// `module_from_span_anchors_the_from_key`.
    #[test]
    fn module_forms_parse() {
        // The doc-comment example. Values, never spellings: the
        // f-string decodes into bucket + key.
        let module = single_module(
            "/// Colour helpers\n\
             module color { from: f'modules:/color.surli'; unsigned; };",
        );
        assert_eq!(module.name.as_ref().map(|n| n.raw.as_str()), Some("color"));
        assert_eq!(
            module.executable,
            Executable::File {
                bucket: "modules".to_string(),
                key: "/color.surli".to_string(),
            }
        );
        assert!(module.unsigned);
        assert_eq!(module.permissions, None);
        assert_eq!(
            module.comment.map(|c| c.joined()),
            Some("Colour helpers".to_string())
        );
        // The silo example: self-named (no header name), the u32
        // triple as parsed values.
        let module = single_module("module { from: silo::acme::utils::<1.2.0>; unsigned; };");
        assert_eq!(module.name, None);
        assert_eq!(
            module.executable,
            Executable::Silo {
                org: "acme".to_string(),
                pkg: "utils".to_string(),
                major: 1,
                minor: 2,
                patch: 0,
            }
        );
        assert!(module.unsigned);
        assert_eq!(module.permissions, None);
        assert_eq!(module.comment, None);
        // The permissions example (single-value expression arm).
        let module = single_module(
            "module admin_tools { from: f'modules:/admin.surli'; unsigned; \
             permissions: $auth.admin = true; };",
        );
        assert_eq!(
            module.permissions,
            Some(Permission::Expr(Expr {
                canon: "$auth.admin = true".to_string(),
            }))
        );
        // Both f-string quote styles parse to the same value; clause
        // order is free (rule 7); `permissions: none` is the keyword.
        assert_eq!(
            single_module("module color { unsigned; from: f\"modules:/color.surli\"; };")
                .executable,
            single_module("module color { from: f'modules:/color.surli'; unsigned; };").executable
        );
        assert_eq!(
            single_module("module x { from: f'b:/k'; unsigned; permissions: NONE; };").permissions,
            Some(Permission::None)
        );
        // Repeats fold last-one-wins (rule 7) — including `from`, so
        // the executable itself is replaced. (The module stays
        // anonymous: a name would collide with the folded silo — see
        // the coupling test below.)
        let module =
            single_module("module { from: f'a:/one'; from: silo::a::b::<2.0.0>; unsigned; };");
        assert_eq!(
            module.executable,
            Executable::Silo {
                org: "a".to_string(),
                pkg: "b".to_string(),
                major: 2,
                minor: 0,
                patch: 0,
            }
        );
    }

    /// The name–executable coupling (spec/module.md): a file
    /// executable requires the header name; a silo executable is
    /// self-named and rejects one.
    #[test]
    fn module_name_couples_to_the_executable() {
        // Anonymous file module: diagnosed at the `from` clause.
        match first_error("module { from: f'b:/k'; unsigned; };") {
            Diag::ModuleExecutableName { span, message, .. } => {
                assert_eq!(span.slice("module { from: f'b:/k'; unsigned; };"), "from");
                assert!(message.contains("requires a name"), "message: {message}");
            }
            other => panic!("expected ModuleExecutableName: {other:?}"),
        }
        // Named silo module: diagnosed at the header name.
        let src = "module color { from: silo::a::b::<1.2.0>; unsigned; };";
        match first_error(src) {
            Diag::ModuleExecutableName { span, message, .. } => {
                assert_eq!(span.slice(src), "color");
                assert!(message.contains("self-named"), "message: {message}");
            }
            other => panic!("expected ModuleExecutableName: {other:?}"),
        }
        // The coupling follows the fold: a named file module whose
        // `from` is *replaced* by a silo package is a named silo.
        let (_, diags) =
            parse_file("module x { from: f'a:/one'; from: silo::a::b::<2.0.0>; unsigned; };\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::ModuleExecutableName { .. }]
        ));
    }

    /// `from` and `unsigned` are required (SDL's 3.3 grammar); a
    /// failed attempt is diagnosed at the clause, never re-reported
    /// as missing.
    #[test]
    fn module_requires_from_and_unsigned() {
        // Both missing — the bare form, the empty block, and `module;`
        // (whose name position fails first).
        for src in ["module color;", "module color { };", "module { };"] {
            let clauses: Vec<&str> = parse_file(src)
                .1
                .iter()
                .filter_map(|diag| match diag {
                    Diag::RequiredClauseMissing { clause, .. } => Some(*clause),
                    _ => None,
                })
                .collect();
            assert_eq!(clauses, ["from", "unsigned;"], "src: {src}");
        }
        // `module;` also reports the missing name.
        assert!(matches!(first_error("module;"), Diag::ExpectedName { .. }));
        // Each missing alone.
        assert!(matches!(
            first_error("module color { from: f'b:/k'; };"),
            Diag::RequiredClauseMissing {
                clause: "unsigned;",
                ..
            }
        ));
        assert!(matches!(
            first_error("module color { unsigned; };"),
            Diag::RequiredClauseMissing { clause: "from", .. }
        ));
        // A failed `unsigned:` attempt (a flag takes no value) is
        // already diagnosed — no missing report, statement dropped.
        let (_, diags) = parse_file("module color { from: f'b:/k'; unsigned: true; };\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::UnexpectedClauseForm { .. }]
        ));
        // A failed `from` attempt likewise.
        let (_, diags) = parse_file("module color { from: 'memory'; unsigned; };\n");
        assert!(matches!(diags.as_slice(), [Diag::InvalidExecutable { .. }]));
    }

    /// The executable arms' grammars: f-string decoding, the file-string
    /// charset rules, and the silo path spellings.
    #[test]
    fn module_executables_validate() {
        // Escapes decode before the file-string grammar runs.
        assert_eq!(
            single_module("module x { from: f'b\\u{75}cket:/k'; unsigned; };").executable,
            Executable::File {
                bucket: "bucket".to_string(),
                key: "/k".to_string(),
            }
        );
        // The bucket/key charsets (`_`, `-`, `.`; the key also `/`).
        assert_eq!(
            single_module("module x { from: f'B-k.1:/a_b-c.d/e'; unsigned; };").executable,
            Executable::File {
                bucket: "B-k.1".to_string(),
                key: "/a_b-c.d/e".to_string(),
            }
        );
        // An empty bucket rides through (upstream's lexer loop accepts
        // one) — pinned for the 3.3-bump re-verification.
        assert_eq!(
            single_module("module x { from: f':/k'; unsigned; };").executable,
            Executable::File {
                bucket: String::new(),
                key: "/k".to_string(),
            }
        );
        // SurrealQL comments are trivia around the value (rule 3).
        assert!(
            parse_ok("module x { from: /* pick */ f'b:/k' -- why\n; unsigned; };")
                .items
                .len()
                == 1
        );
        // A `;` inside the f-string stays inside the capture — and the
        // full value then fails the key charset (the diag quoting
        // `b:/a;b` proves the capture ran past the in-string `;`).
        match first_error("module x { from: f'b:/a;b'; unsigned; };") {
            Diag::InvalidExecutable { message, .. } => {
                assert!(message.contains("b:/a;b"), "message: {message}");
            }
            other => panic!("expected InvalidExecutable: {other:?}"),
        };
        // Silo spellings: keyword-like segments bare (upstream
        // parse_ident parity), quoted segments unescape, `_` digit
        // separators, trivia between tokens, the u32 ceiling.
        assert_eq!(
            single_module("module { from: silo::select::table::<1.2.0>; unsigned; };").executable,
            Executable::Silo {
                org: "select".to_string(),
                pkg: "table".to_string(),
                major: 1,
                minor: 2,
                patch: 0,
            }
        );
        assert_eq!(
            single_module(
                "module { from: silo::`weird org`::pkg::<1_0.2.4294967295>; unsigned; };"
            )
            .executable,
            Executable::Silo {
                org: "weird org".to_string(),
                pkg: "pkg".to_string(),
                major: 10,
                minor: 2,
                patch: 4294967295,
            }
        );
        assert_eq!(
            single_module("module { from: silo :: a :: b :: < 1 . 2 . 0 > /* v */; unsigned; };")
                .executable,
            Executable::Silo {
                org: "a".to_string(),
                pkg: "b".to_string(),
                major: 1,
                minor: 2,
                patch: 0,
            }
        );
    }

    /// Every malformed executable spelling: wrong kinds, broken
    /// f-strings, file-string charset violations (including the
    /// upstream key-`:` quirk SDL rejects), and broken silo paths.
    #[test]
    fn module_executable_spelling_errors() {
        // Not an executable at all.
        for src in [
            "module x { from: 'memory'; unsigned; };",
            "module x { from: backend; unsigned; };",
            "module x { from: fn::a; unsigned; };",
            "module x { from: ; unsigned; };",
            // `F'…'` — uppercase prefixes are not f-strings (rule 9).
            "module x { from: F'b:/k'; unsigned; };",
            // The space makes `f` an identifier before a plain string.
            "module x { from: f 'b:/k'; unsigned; };",
        ] {
            assert!(
                matches!(first_error(src), Diag::InvalidExecutable { .. }),
                "src: {src}"
            );
        }
        // A quoted prefix is an identifier, not the keyword (rule 9).
        assert!(matches!(
            first_error("module { from: `silo`::a::b::<1.2.0>; unsigned; };"),
            Diag::InvalidExecutable { .. }
        ));
        // Malformed f-string: unterminated (the capture itself dies).
        assert!(matches!(
            first_error("module x { from: f'b:/k; unsigned; };"),
            Diag::UnterminatedString { .. }
        ));
        // Trailing junk after the closing quote.
        assert!(matches!(
            first_error("module x { from: f'b:/k' extra; unsigned; };"),
            Diag::InvalidExecutable { .. }
        ));
        // File-string charset violations.
        for from in [
            "f'b'",         // no bucket separator
            "f'b:noslash'", // missing `/` after the `:`
            "f'b!d:/k'",    // a bucket character outside the set
            "f'b:/k:tail'", // a `:` in the key (upstream drops the tail; SDL rejects)
            "f'b:/k;bad'",  // a key character outside the set
        ] {
            let src = format!("module x {{ from: {from}; unsigned; }};");
            assert!(
                matches!(first_error(&src), Diag::InvalidExecutable { .. }),
                "from: {from}"
            );
        }
        // Broken silo paths.
        for from in [
            "silo::a",                      // missing everything after the org
            "silo::a::b",                   // missing the version
            "silo::a::b<1.2.0>",            // 3.2 spelling — SDL input is the :: form
            "silo::a::b::<1.2>",            // two components
            "silo::a::b::<1.2.0.1>",        // four components
            "silo::a::b::<4294967296.0.0>", // u32 overflow
            "silo::a::b::<1.2.0> extra",    // trailing junk
            "silo a b",                     // not `::`-joined
        ] {
            let src = format!("module {{ from: {from}; unsigned; }};");
            assert!(
                matches!(first_error(&src), Diag::InvalidExecutable { .. }),
                "from: {from}"
            );
        }
    }

    /// Broken module statements do not hide their siblings.
    #[test]
    fn broken_module_does_not_hide_its_siblings() {
        let (file, diags) = parse_file("module color { from: f'b:/k'; };\nnamespace ok;\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::RequiredClauseMissing {
                clause: "unsigned;",
                ..
            }]
        ));
        assert_eq!(file.items.len(), 1);
        // An unknown clause diagnoses over the module table.
        let (_, diags) = parse_file("module color { from: f'b:/k'; unsigned; backend: 'm'; };");
        assert!(matches!(
            diags.first(),
            Some(Diag::UnknownClause { key, .. }) if key == "backend"
        ));
    }

    fn single_function(src: &str) -> Function {
        match single_statement(src) {
            Stmt::Function(function) => function,
            other => panic!("expected a function: {other:?}"),
        }
    }

    /// spec/function.md: the surface forms — the doc-comment example,
    /// the body-only multi-statement form, zero params, the
    /// multi-segment path with graphql, the deprecated graphql
    /// option, and the rule-7 folds.
    #[test]
    fn function_forms_parse() {
        // The doc-comment example, two-block form: permissions
        // expression + COMMENT.
        let function = single_function(
            "/// Whether the current user may edit this post\n\
             fn can_edit($p: record<post>) -> bool {\n\
             \x20 return $auth.id = $p.author or $auth.role = 'admin';\n\
             } {\n\
             \x20 permissions: $auth.admin = true;\n\
             };",
        );
        assert_eq!(
            function.name,
            FnPath {
                kind: FnKind::Fn,
                segments: vec!["can_edit".to_string()],
            }
        );
        assert_eq!(
            function.args,
            vec![(ParamName::new("p"), Kind::new("record<post>"))]
        );
        assert_eq!(function.returns, Some(Kind::new("bool")));
        assert_eq!(
            function.body,
            Expr {
                canon: "{ RETURN $auth.id = $p.author OR $auth.role = 'admin' }".to_string()
            }
        );
        assert_eq!(
            function.permissions,
            Some(Permission::Expr(Expr {
                canon: "$auth.admin = true".to_string(),
            }))
        );
        assert_eq!(
            function.comment,
            Some(DocComment {
                lines: vec!["Whether the current user may edit this post".to_string()],
            })
        );
        // The multi-statement body: canon carries one `;` after every
        // statement (the embedded block print), decimals re-spelled
        // upstream's way (`0.15f`).
        let function = single_function(
            "fn tax($net: number, $rate: option<number>) {\n\
             \x20 let $r = $rate ?? 0.15;\n\
             \x20 let $tax = math::round($net * $r, 2);\n\
             \x20 $net + $tax\n\
             };",
        );
        assert_eq!(function.returns, None);
        assert_eq!(function.permissions, None);
        assert_eq!(
            function.body.canon,
            "{ LET $r = $rate ?? 0.15f; LET $tax = math::round($net * $r, 2); $net + $tax; }"
        );
        // Zero params, body only; the block-less tail rides EOF.
        let function = single_function("fn ping() -> string { 'pong' }");
        assert!(function.args.is_empty());
        assert_eq!(function.body.canon, "{ 'pong' }");
        // Multi-segment name + the graphql alias option.
        let function = single_function(
            "fn util::slugify($s: string) -> string {\
             string::lowercase(string::replace($s, ' ', '-'))\
             } { graphql { alias: 'slugify'; }; };",
        );
        assert_eq!(
            function.name,
            FnPath {
                kind: FnKind::Fn,
                segments: vec!["util".to_string(), "slugify".to_string()],
            }
        );
        assert_eq!(
            function.graphql,
            Some(Graphql {
                alias: Some("slugify".to_string()),
                deprecated: None,
            })
        );
        // The deprecated option rides the same sub-block; an explicit
        // `permissions: full` stays in the AST (rule 9: only the
        // ABSENT clause elides at emission).
        let function = single_function(
            "fn old($a: int) { 1 } { graphql { deprecated: 'use new'; }; \
             permissions: full; };",
        );
        assert_eq!(
            function.graphql,
            Some(Graphql {
                alias: None,
                deprecated: Some("use new".to_string()),
            })
        );
        assert_eq!(function.permissions, Some(Permission::Full));
        // A graphql block with neither side present folds to absent.
        assert_eq!(
            single_function("fn f($a: int) { 1 } { graphql { }; };").graphql,
            None
        );
        // Repeats fold last-one-wins (rule 7) — the whole graphql
        // block included.
        let function = single_function(
            "fn f($a: int) { 1 } { graphql { alias: 'a'; }; \
             graphql { alias: 'b'; deprecated: 'd'; }; };",
        );
        assert_eq!(
            function.graphql,
            Some(Graphql {
                alias: Some("b".to_string()),
                deprecated: Some("d".to_string()),
            })
        );
        // Comments are legal anywhere SurrealQL puts a token
        // boundary — inside the signature, the kinds, and the body.
        let function = single_function(
            "fn f /* sig */ ($a: /* k */ record<post>, $b: int /* t */) { /* b */ 1 };",
        );
        assert_eq!(
            function.args,
            vec![
                (ParamName::new("a"), Kind::new("record<post>")),
                (ParamName::new("b"), Kind::new("int")),
            ]
        );
    }

    /// The kind grammar (README rule 4) mirrors v3.2.4's
    /// `syn/parser/kind.rs` — probe-pinned shapes and rejections.
    /// Types carry the source spelling verbatim; only the trimmed
    /// text is normalized.
    #[test]
    fn function_kind_grammar_mirrors_upstream() {
        let kinds = [
            "option<number>",
            "int | float",
            "none | int | float",
            "option<int | float>",
            // option may OPEN a union (folds to the same Either
            // upstream); `any` stands alone; nested option dies.
            "option<int> | string",
            "any",
            "record<x | y>",
            "record < person | animal >",
            "record",
            "table<person>",
            "geometry<point | line>",
            "geometry<POINT | Line>",
            "geometry",
            "point",
            "file<b | c>",
            "array<float, 10>",
            "array<float, 1_0>",
            "array<option<int> | string, 3>",
            "set<int, 2>",
            "array",
            "set",
            "function",
            "range",
            "regex",
            "bytes",
            "null",
            "uuid",
            "duration",
            "datetime",
            "decimal",
            "number",
            "object",
            "string",
            "bool",
            "int",
            "float",
            // Literal kinds: strings, signed numbers, durations, and
            // the exact-case NaN/Infinity spellings. Upstream's
            // compound number accepts the `f`/`dec` suffixes, signed
            // `Infinity`, and trailing-`_` digit runs.
            "'read' | 'write'",
            "\"a\" | 'b'",
            "-3 | 4",
            "1h",
            "NaN",
            "Infinity",
            "-Infinity",
            "+Infinity | -Infinity",
            "1f",
            "1.5f",
            "1dec",
            "1.5dec | int",
            "1_",
            "array<float, 1_>",
            // Brace-bearing kinds: the param list splits AFTER the
            // kind ends, never at the `,` inside `<>` or `{…}`.
            "{ a: string, b: int }",
            "{ a: { b: option<int> } }",
            "[1, 'two']",
            // Nested inside generics and unions — the return-kind
            // reader folds braced groups until the kind completes.
            "array<{ x: int }>",
            "option<{ a: int } | string>",
            "int | { a: string }",
            "array<float, 3> | [int]",
        ];
        for kind in kinds {
            let src = format!("fn f($a: {kind}) {{ 1 }};");
            let function = single_function(&src);
            assert_eq!(
                function.args,
                vec![(ParamName::new("a"), Kind::new(kind))],
                "src: {src}"
            );
            let ret_src = format!("fn f() -> {kind} {{ 1 }};");
            assert_eq!(
                single_function(&ret_src).returns,
                Some(Kind::new(kind)),
                "src: {ret_src}"
            );
        }
        // Verbatim with interior whitespace; only the ends trim.
        let function = single_function("fn f($a: record < person | animal >) { 1 };");
        assert_eq!(function.args[0].1.text, "record < person | animal >");
        // The rejections — probe-pinned upstream deaths. A union
        // whose head cannot join one (`any | int`) dies as the
        // signature error at the leftover `|`, upstream's "expected
        // delimiter `)`" shape; the rest are kind rejections.
        for kind in [
            "option<option<int>>",
            "any | int",
            "frobnicate",
            "option",
            "array<float, -1>",
            "array<float, 1h>",
            "array<float, 1.5>",
            "array<float, 1e2>",
            "int | | float",
            "nan",
            "infinity",
            "record<>",
            "geometry<circle>",
            "'read' 'write'",
            // Keys are bare words, quoted idents, strings, or unsigned
            // numbers — signed numbers and durations are upstream
            // rejections (probe-pinned `parse_object_key`).
            "{ -3: int }",
            "{ 1h: int }",
        ] {
            let src = format!("fn f($a: {kind}) {{ 1 }};");
            let (_, diags) = parse_file(&src);
            assert!(
                matches!(
                    diags.first(),
                    Some(Diag::InvalidKind { .. }) | Some(Diag::InvalidFunctionSignature { .. })
                ),
                "`{kind}` must be rejected: {diags:?}"
            );
        }
    }

    /// Signature diagnostics: one per distinct mistake (spec's
    /// verified SurrealQL rejection included), anchored where the fix
    /// goes.
    #[test]
    fn function_signature_errors() {
        let expects = [
            (
                "fn ($a: int) { 1 };",
                "expected the function name after `fn`",
            ),
            ("fn a::() { 1 };", "expected a path segment after `::`"),
            (
                "fn a $p: int) { 1 };",
                "expected `(` to open the parameter list",
            ),
            (
                "fn a($a int) { 1 };",
                "expected `:` between the parameter name and its kind",
            ),
            ("fn a($ int) { 1 };", "expected a parameter name after `$`"),
            (
                "fn a($a: int $b: int) { 1 };",
                "expected `,` between parameters or `)` to close the list",
            ),
            (
                "fn a($a: int) -> int;",
                "expected `{` to open the body block after the return kind",
            ),
            ("fn a($a: int);", "expected `{` to open the body block"),
            // `-> { 1 }`: the braced group is the object-kind attempt
            // (nothing else may follow `->`), and `{ 1 }` is not one.
            ("fn a($a: int) -> { 1 };", "is not a SurrealQL kind"),
        ];
        for (src, message) in expects {
            match first_error(src) {
                Diag::InvalidFunctionSignature { message: got, .. } if got.contains(message) => {
                    continue;
                }
                Diag::InvalidKind { .. } if message.contains("kind") => {}
                other => panic!("`{src}`: expected `{message}`, got {other:?}"),
            }
        }
        // The spec's verified SurrealQL rejection gets its own code.
        match first_error("fn a(p: int) { 1 };") {
            Diag::FunctionParamNeedsDollar { span } => {
                assert_eq!(span.slice("fn a(p: int) { 1 };"), "p");
            }
            other => panic!("expected FunctionParamNeedsDollar: {other:?}"),
        }
        // The deliberate strictness: SurrealQL accepts a trailing
        // comma, SDL rejects it (spec/function.md).
        match first_error("fn a($a: int,) { 1 };") {
            Diag::FunctionParamTrailingComma { span } => {
                assert_eq!(span.slice("fn a($a: int,) { 1 };"), ",");
            }
            other => panic!("expected FunctionParamTrailingComma: {other:?}"),
        }
        // The doubled prefix (issue #110): the keyword already carries
        // the `fn::`, so a first segment spelling `fn` would emit
        // `fn::fn::…` — a different function than the author spelled.
        // Quoted spellings decode first, so the quoted shape rejects
        // too; every other first segment parses (multi-segment paths
        // included).
        match first_error("fn fn::weird($a: int) { 1 };") {
            Diag::FnNameDoubledPrefix { span } => {
                assert_eq!(span.slice("fn fn::weird($a: int) { 1 };"), "fn");
            }
            other => panic!("expected FnNameDoubledPrefix: {other:?}"),
        }
        match first_error("fn `fn`::weird($a: int) { 1 };") {
            Diag::FnNameDoubledPrefix { span } => {
                assert_eq!(span.slice("fn `fn`::weird($a: int) { 1 };"), "`fn`");
            }
            other => panic!("expected FnNameDoubledPrefix (quoted): {other:?}"),
        }
        let (_, diags) = parse_file("fn util::deep($a: int) { 1 };");
        assert!(diags.is_empty(), "{diags:?}");
        // A bad kind anchors at its own text.
        let src = "fn a($a: frobnicate<int>) { 1 };";
        match first_error(src) {
            Diag::InvalidKind { span, text } => {
                assert_eq!(span.slice(src), "frobnicate<int>");
                assert_eq!(text, "frobnicate<int>");
            }
            other => panic!("expected InvalidKind: {other:?}"),
        }
        // The missing-`:` caret anchors on the junk, not the trivia
        // ahead of it.
        let src = "fn a($a int) { 1 };";
        match first_error(src) {
            Diag::InvalidFunctionSignature { span, .. } => {
                assert_eq!(span.slice(src), "int");
            }
            other => panic!("expected InvalidFunctionSignature: {other:?}"),
        }
        // EOF after `->` demands a kind, not a body brace.
        match first_error("fn a($a: int) -> ") {
            Diag::InvalidKind { text, .. } => assert!(text.is_empty()),
            other => panic!("expected InvalidKind: {other:?}"),
        }
        // A COMPLETE kind at EOF means only the body is missing.
        match first_error("fn a($a: int) -> int") {
            Diag::InvalidFunctionSignature { message, .. } => {
                assert!(
                    message.contains("expected `{` to open the body block"),
                    "message: {message}"
                );
            }
            other => panic!("expected InvalidFunctionSignature: {other:?}"),
        }
    }

    /// One broken `fn` never hides its siblings: the diagnostic
    /// fires, the file keeps the statements around it, and recovery
    /// rides the depth-0 `;` scanner.
    #[test]
    fn broken_function_does_not_hide_its_siblings() {
        let (file, diags) = parse_file(
            "namespace before_us;\nfn broken($a: frobnicate) { 1 };\nnamespace after_us;\n",
        );
        assert!(matches!(diags.as_slice(), [Diag::InvalidKind { .. }]));
        assert_eq!(file.items.len(), 2);
        // An unknown clause diagnoses over the function table.
        let (_, diags) = parse_file("fn f($a: int) { 1 } { backend: 'm'; };");
        assert!(matches!(
            diags.first(),
            Some(Diag::UnknownClause { key, .. }) if key == "backend"
        ));
    }

    // ---- config ----

    fn single_config(src: &str) -> Config {
        match single_statement(src) {
            Stmt::Config(config) => config,
            other => panic!("expected a config: {other:?}"),
        }
    }

    /// spec/config.md: the api variant — the middleware hybrid list
    /// (head grammars, argument splits) and the single-value
    /// permissions; the bare form is the empty block; rule-7 folds
    /// replace whole lists.
    #[test]
    fn config_api_forms_parse() {
        // The spec example: bare heads, a duration argument, an
        // empty-argument entry, and the permissions expression. The
        // comma inside `math::round(1, 2)` stays one argument.
        let config = single_config(
            "config api { middleware: timeout(5s), fn::guard::a(math::round(1.5, 2)); \
             permissions: $auth.admin = true; };",
        );
        let Config::Api(api) = config else {
            panic!("expected an api config: {config:?}")
        };
        assert_eq!(api.middleware.len(), 2);
        assert_eq!(
            api.middleware[0].path,
            FnPath {
                kind: FnKind::Bare,
                segments: vec!["timeout".to_string()]
            }
        );
        assert_eq!(api.middleware[0].args.len(), 1);
        assert_eq!(
            api.middleware[1].path,
            FnPath {
                kind: FnKind::Fn,
                segments: vec!["guard".to_string(), "a".to_string()]
            }
        );
        assert_eq!(api.middleware[1].args.len(), 1);
        assert_eq!(
            api.permissions,
            Some(Permission::Expr(Expr {
                canon: "$auth.admin = true".to_string()
            }))
        );
        // `mod::` heads parse SDL-side (capability-gated at define
        // time only); empty parens carry no arguments.
        let config = single_config("config api { middleware: mod::pkg::check(); };");
        let Config::Api(api) = config else {
            panic!("expected an api config: {config:?}")
        };
        assert_eq!(api.middleware[0].path.kind, FnKind::Mod);
        assert!(api.middleware[0].args.is_empty());
        assert_eq!(api.permissions, None);
        // The bare form is legal (upstream accepts a bare
        // `DEFINE CONFIG … API;`); clause order is free (rule 7).
        assert_eq!(
            single_config("config api;"),
            Config::Api(ApiConfig {
                middleware: vec![],
                permissions: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
        assert_eq!(
            single_config("config api { permissions: none; };"),
            Config::Api(ApiConfig {
                middleware: vec![],
                permissions: Some(Permission::None),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
        // A repeated clause replaces the whole value (rule 7).
        let config = single_config(
            "config api { middleware: a(); middleware: fn::b(1); permissions: full; \
             permissions: none; };",
        );
        let Config::Api(api) = config else {
            panic!("expected an api config: {config:?}")
        };
        assert_eq!(api.middleware.len(), 1);
        assert_eq!(api.middleware[0].path.segments, vec!["b".to_string()]);
        assert_eq!(api.permissions, Some(Permission::None));
    }

    /// The middleware entry grammar's rejection paths — parens
    /// required, `silo::`/`ml::` outside SDL, no junk after the call,
    /// a bare `fn`/`mod` without its `::`, a mid-list empty argument,
    /// and a quoted `fn` head as an ordinary name (spec/config.md +
    /// the `<fn-path>` note).
    #[test]
    fn config_middleware_grammar_errors() {
        // Parens are required (upstream `expected!(t!("(")))`.
        match first_error("config api { middleware: api::timeout; };") {
            Diag::InvalidMiddleware { message, .. } => {
                assert!(message.contains("expected `(`"), "message: {message}");
            }
            other => panic!("expected InvalidMiddleware: {other:?}"),
        }
        // `silo::`/`ml::` heads are outside SDL's surface.
        for head in ["silo", "ml"] {
            match first_error(&format!("config api {{ middleware: {head}::a(); }};")) {
                Diag::InvalidMiddleware { message, .. } => {
                    assert!(message.contains("outside SDL's surface"));
                }
                other => panic!("expected InvalidMiddleware for {head}: {other:?}"),
            }
        }
        // A bare `fn`/`mod` without `::` is the missing prefix.
        match first_error("config api { middleware: fn(); };") {
            Diag::InvalidMiddleware { message, .. } => {
                assert!(message.contains("expected `::` after `fn`"));
            }
            other => panic!("expected InvalidMiddleware: {other:?}"),
        }
        // Junk after the call.
        match first_error("config api { middleware: a() junk; };") {
            Diag::InvalidMiddleware { message, .. } => {
                assert!(message.contains("unexpected text after"));
            }
            other => panic!("expected InvalidMiddleware: {other:?}"),
        }
        // A lone `:` mid-path is never swallowed — the entry
        // diagnoses instead of silently truncating the path.
        match first_error("config api { middleware: api::timeout:(5s); };") {
            Diag::InvalidMiddleware { message, .. } => {
                assert!(message.contains("a lone `:`"), "message: {message}");
            }
            other => panic!("expected InvalidMiddleware for the lone `:`: {other:?}"),
        }
        // A trailing `,` resolves like upstream's argument loop: the
        // final empty item is no argument (`a(1,)` ≡ `a(1)`), so the
        // entry parses clean with one argument.
        let config = single_config("config api { middleware: a(1,); };");
        let Config::Api(api) = config else {
            panic!("expected an api config: {config:?}")
        };
        assert_eq!(api.middleware[0].args.len(), 1);
        // An empty item MID-LIST rejects on both sides.
        let (_, diags) = parse_file("config api { middleware: a(1,, 2); };");
        assert!(matches!(
            diags.first(),
            Some(Diag::EmbeddedParseError { message, .. }) if message.contains("empty")
        ));
        // A quoted `fn` is an ordinary bare head.
        let config = single_config("config api { middleware: `fn`(); };");
        let Config::Api(api) = config else {
            panic!("expected an api config: {config:?}")
        };
        assert_eq!(api.middleware[0].path.kind, FnKind::Bare);
        assert_eq!(api.middleware[0].path.segments, vec!["fn".to_string()]);
    }

    /// spec/config.md: the graphql variant — the bare flags write
    /// both families, the `:` and sub-block forms write one, and
    /// every write folds last-one-wins per family (rule 7).
    #[test]
    fn config_graphql_forms_parse() {
        // The flag forms.
        assert_eq!(
            single_config("config graphql { auto; };"),
            Config::Graphql(GraphqlConfig {
                tables: Family::Auto,
                functions: Family::Auto,
                ..GraphqlConfig::default()
            })
        );
        assert_eq!(
            single_config("config graphql;"),
            Config::Graphql(GraphqlConfig::default())
        );
        // The `:` forms and the sub-block forms.
        assert_eq!(
            single_config("config graphql { tables: none; functions: auto; };"),
            Config::Graphql(GraphqlConfig {
                tables: Family::None,
                functions: Family::Auto,
                ..GraphqlConfig::default()
            })
        );
        assert_eq!(
            single_config(
                "config graphql { tables { include: user, `order`; }; functions { exclude: \
                 fn::a::b; }; };"
            ),
            Config::Graphql(GraphqlConfig {
                tables: Family::Include(vec![Ident::new("user"), Ident::new("order")]),
                functions: Family::Exclude(vec![FnPath {
                    kind: FnKind::Fn,
                    segments: vec!["a".to_string(), "b".to_string()],
                }]),
                ..GraphqlConfig::default()
            })
        );
        // Folds: the flag writes both, then a family clause overwrites
        // one side (probe-pinned upstream shape: AUTO TABLES NONE →
        // TABLES NONE FUNCTIONS AUTO).
        assert_eq!(
            single_config("config graphql { auto; tables: none; };"),
            Config::Graphql(GraphqlConfig {
                tables: Family::None,
                functions: Family::Auto,
                ..GraphqlConfig::default()
            })
        );
        // Limits and introspection: u32 values with `_` separators
        // (upstream accepts `DEPTH 1_000`, probe-pinned); an explicit
        // `introspection: auto` folds to the default.
        let config = single_config(
            "config graphql { depth: 1_000; complexity: 100; introspection: none; };",
        );
        assert_eq!(
            config,
            Config::Graphql(GraphqlConfig {
                depth: Some(1_000),
                complexity: Some(100),
                introspection: Introspection::None,
                ..GraphqlConfig::default()
            })
        );
        assert_eq!(
            single_config("config graphql { introspection: auto; };"),
            Config::Graphql(GraphqlConfig::default())
        );
    }

    /// The graphql clause grammar's rejection paths: the sub-block
    /// holds exactly ONE of include/exclude; fn lists require the
    /// `fn::` prefix; the u32 range is enforced; unknown words get
    /// did-you-mean.
    #[test]
    fn config_graphql_grammar_errors() {
        // Both sides in one sub-block (the span is the clause key,
        // like the password/passhash pairing).
        let src = "config graphql { tables { include: user; exclude: log; }; };";
        match first_error(src) {
            Diag::MutuallyExclusive {
                span,
                clause,
                other,
            } => {
                assert_eq!(span.slice(src), "exclude");
                assert_eq!((clause, other), ("exclude", "include"));
            }
            other => panic!("expected MutuallyExclusive: {other:?}"),
        }
        // An empty sub-block: neither side.
        match first_error("config graphql { tables { }; };") {
            Diag::UnexpectedClauseForm { message, .. } => {
                assert!(message.contains("exactly one of"), "message: {message}");
            }
            other => panic!("expected UnexpectedClauseForm: {other:?}"),
        }
        // fn lists demand the `fn::` prefix (mod:: included —
        // upstream `parse_custom_function_name`).
        for spelling in ["a", "mod::a"] {
            let (_, diags) = parse_file(&format!(
                "config graphql {{ functions {{ include: {spelling}; }}; }};"
            ));
            assert!(
                matches!(
                    diags.first(),
                    Some(Diag::InvalidFnPath { message, .. }) if message.contains("`fn::` prefix")
                ),
                "spelling {spelling}: {diags:?}"
            );
        }
        // A lone `:` at the path's end diagnoses — never silently
        // truncated (`fn::foo:` must not read as `fn::foo`).
        let (_, diags) = parse_file("config graphql { functions { include: fn::foo:; }; };");
        assert!(matches!(
            diags.first(),
            Some(Diag::InvalidFnPath { message, .. }) if message.contains("expected `,`")
        ));
        // The u32 range: 2^32 overflows.
        let (_, diags) = parse_file("config graphql { depth: 4294967296; };");
        assert!(matches!(
            diags.first(),
            Some(Diag::InvalidNumber { range, .. }) if *range == "0–4294967295"
        ));
        // Unknown introspection word with did-you-mean.
        match first_error("config graphql { introspection: nane; };") {
            Diag::UnknownClauseWord { suggestion, .. } => {
                assert_eq!(suggestion.as_deref(), Some("none"));
            }
            other => panic!("expected UnknownClauseWord: {other:?}"),
        }
        // The `:` form takes a word, not a block.
        match first_error("config graphql { tables: { }; };") {
            Diag::UnknownClauseWord { .. } => {}
            other => panic!("expected UnknownClauseWord: {other:?}"),
        }
    }

    /// spec/config.md: the default variant — both sides, either side
    /// alone, rule-7 folds, and the ≥ 1-side requirement (a failed
    /// attempt already carried its own diagnostic, so it silences the
    /// missing-clause one).
    #[test]
    fn config_default_forms_parse() {
        assert_eq!(
            single_config("config default { namespace: 'acme'; database: 'blog'; };"),
            Config::Default(DefaultConfig {
                namespace: Some(Expr {
                    canon: "'acme'".to_string()
                }),
                database: Some(Expr {
                    canon: "'blog'".to_string()
                }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
        assert_eq!(
            single_config("config default { database: 'blog'; };"),
            Config::Default(DefaultConfig {
                namespace: None,
                database: Some(Expr {
                    canon: "'blog'".to_string()
                }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
        // Repeats fold last-one-wins (rule 7), upstream-probed.
        assert_eq!(
            single_config("config default { namespace: 'a'; database: 'b'; namespace: 'c'; };"),
            Config::Default(DefaultConfig {
                namespace: Some(Expr {
                    canon: "'c'".to_string()
                }),
                database: Some(Expr {
                    canon: "'b'".to_string()
                }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
        // The requirement: bare form and empty block both demand a
        // side.
        for src in ["config default;", "config default { };"] {
            match first_error(src) {
                Diag::RequiredClauseMissing { clause, block, .. } => {
                    assert_eq!((clause, block), ("namespace", "config default"));
                }
                other => panic!("expected RequiredClauseMissing for {src}: {other:?}"),
            }
        }
        // A failed attempt on either side is already diagnosed — no
        // second, redundant missing-clause diagnostic.
        for src in [
            "config default { namespace: ; };",
            "config default { database: ; };",
        ] {
            let (_, diags) = parse_file(src);
            assert_eq!(diags.len(), 1, "src: {src} — {diags:?}");
        }
    }

    /// The header: the variant word is required and bare (rule 9),
    /// with did-you-mean over the three spellings; a `///` block is
    /// rejected on every variant (no COMMENT clause, rule 5) while
    /// the statement still parses.
    #[test]
    fn config_header_and_doc_rules() {
        // Unknown variant with did-you-mean.
        match first_error("config grapql { auto; };") {
            Diag::UnknownVariant {
                word, suggestion, ..
            } => {
                assert_eq!(word, "grapql");
                assert_eq!(suggestion.as_deref(), Some("graphql"));
            }
            other => panic!("expected UnknownVariant: {other:?}"),
        }
        // No variant at all.
        match first_error("config;") {
            Diag::UnknownVariant { word, .. } => assert!(word.is_empty()),
            other => panic!("expected UnknownVariant: {other:?}"),
        }
        // A quoted spelling is an identifier, not the keyword.
        match first_error("config `api`;") {
            Diag::UnknownVariant {
                word, suggestion, ..
            } => {
                assert_eq!(word, "api");
                assert_eq!(suggestion, None);
            }
            other => panic!("expected UnknownVariant: {other:?}"),
        }
        // `///` rejected on all three variants; the statement below
        // still parses.
        for (src, variant) in [
            ("/// doc\nconfig api;", "api"),
            ("/// doc\nconfig graphql { auto; };", "graphql"),
            ("/// doc\nconfig default { namespace: 'a'; };", "default"),
        ] {
            let (file, diags) = parse_file(src);
            assert!(
                matches!(
                    diags.first(),
                    Some(Diag::DocCommentUnsupported { statement, .. }) if *statement == "config"
                ),
                "variant {variant}: {diags:?}"
            );
            assert_eq!(file.items.len(), 1, "variant {variant}");
        }
        // An unknown clause diagnoses over the config tables with a
        // did-you-mean (clause-loop recovery intact).
        match first_error("config api { middlewhere: a(); };") {
            Diag::UnknownClause {
                key,
                statement,
                suggestion,
                ..
            } => {
                assert_eq!(key, "middlewhere");
                assert_eq!(statement, "config api");
                assert_eq!(suggestion.as_deref(), Some("middleware"));
            }
            other => panic!("expected UnknownClause: {other:?}"),
        }
    }

    /// One broken `config` never hides its siblings (statement-level
    /// recovery rides the depth-0 `;` scanner), and a `config default`
    /// after a namespace still parses — placement is codegen's job.
    #[test]
    fn broken_config_does_not_hide_its_siblings() {
        let (file, diags) =
            parse_file("namespace before_us;\nconfig broken { auto; };\nnamespace after_us;\n");
        assert!(matches!(diags.as_slice(), [Diag::UnknownVariant { .. }]));
        assert_eq!(file.items.len(), 2);
    }

    // ---- index ----

    fn single_index(src: &str) -> Index {
        let file = parse_ok(src);
        assert_eq!(file.items.len(), 1, "src: {src}");
        match file.items.into_iter().next().expect("len checked") {
            Stmt::Index(index) => index,
            other => panic!("expected an index: {other:?}"),
        }
    }

    /// spec/index.md: the header's optional variant slot — a bare
    /// variant word selects the kind (name required after it), a bare
    /// non-variant word is the name of a plain index, a quoted
    /// spelling is always a name (rule 9), and the plain kinds take
    /// their per-variant clauses.
    #[test]
    fn index_header_reads_variant_and_name() {
        // Bare variant + name.
        let index = single_index("index unique one_email { on: user; fields: email; };");
        assert_eq!(index.name, Ident::new("one_email"));
        assert_eq!(index.kind, IndexKind::Unique);
        // Quoted variant spelling is a NAME (rule 9): a plain index
        // named `unique`.
        let index = single_index("index `unique` { on: user; fields: email; };");
        assert_eq!(index.name, Ident::new("unique"));
        assert_eq!(index.kind, IndexKind::Plain);
        // A bare non-variant word is a plain index's name (rule 6:
        // the default kind expressed by omission).
        let index = single_index("index by_account { on: t; fields: a; };");
        assert_eq!(index.kind, IndexKind::Plain);
        // The slot demands a name after it.
        match first_error("index hnsw { dimension: 8; };") {
            Diag::ExpectedName { .. } => {}
            other => panic!("expected ExpectedName: {other:?}"),
        }
        // A bare reserved word cannot be the plain name (rule 9) —
        // nor a variant (it is not one).
        match first_error("index table { on: t; fields: a; };") {
            Diag::ReservedIdent { word, .. } => assert_eq!(word, "table"),
            other => panic!("expected ReservedIdent: {other:?}"),
        }
        // Nothing identifier-shaped at the name position either.
        match first_error("index ;") {
            Diag::ExpectedName { .. } => {}
            other => panic!("expected ExpectedName: {other:?}"),
        }
    }

    /// spec/index.md: the surface forms of all five variants —
    /// `columns:` as the alias, free clause order, last-one-wins
    /// folds, doc-comment attach, and the value forms of the SDL-lexed
    /// literal clauses.
    #[test]
    fn index_forms_parse_to_presence() {
        // Plain multi-field with a doc comment.
        let index = single_index(
            "/// Composite lookup\nindex by_account_thread { on: message; \
             fields: account, thread_id; };",
        );
        assert_eq!(
            index.comment.expect("doc attached").lines,
            ["Composite lookup"]
        );
        assert_eq!(index.fields.len(), 2);
        assert_eq!(index.kind, IndexKind::Plain);
        assert!(!index.concurrently);
        // `columns:` is the alias (rule 9); concurrently reads.
        let index =
            single_index("index unique one_email { columns: email; concurrently; on: user; };");
        assert_eq!(index.fields.len(), 1);
        assert!(index.concurrently);
        // count: bare and filtered.
        assert_eq!(
            single_index("index count row_count { on: reading; };").kind,
            IndexKind::Count { where_: None }
        );
        let index =
            single_index("index count active_count { on: item; where: status = 'active'; };");
        assert_eq!(
            index.kind,
            IndexKind::Count {
                where_: Some(Expr {
                    canon: "status = 'active'".to_string()
                })
            }
        );
        // fulltext: the tuning clauses, in free order, with the pair
        // and the flags.
        let index = single_index(
            "index fulltext title_fts { highlights; bm25: (1.4, 0.8); \
             analyzer: english; fields: title; on: post; };",
        );
        assert_eq!(
            index.kind,
            IndexKind::FullText {
                analyzer: Some(Ident::new("english")),
                bm25: Some(Bm25(1.4, 0.8)),
                highlights: true,
            }
        );
        // hnsw: minkowski's order, the float `lm`, the underscore
        // flags; values are values (`1.30` ≡ `1.3`).
        let index = single_index(
            "index hnsw vec_tuned { on: embedding; fields: vector; dimension: 768; \
             distance: minkowski(3); type: f16; m: 16; m0: 32; lm: 1.30; efc: 200; \
             extend_candidates; keep_pruned_connections; };",
        );
        assert_eq!(
            index.kind,
            IndexKind::Hnsw {
                dimension: 768,
                distance: Some(Distance::Minkowski(Float(3.0))),
                vector_type: Some(VectorType::F16),
                m: Some(16),
                m0: Some(32),
                lm: Some(Float(1.3)),
                efc: Some(200),
                extend_candidates: true,
                keep_pruned_connections: true,
                hashed_vector: false,
            }
        );
        // diskann: DISKANN's own parameters and the restricted-but-
        // legal domain corner (`inner_product` + `i8`).
        let index = single_index(
            "index diskann big_vec { on: document; fields: embedding; dimension: 1536; \
             distance: inner_product; type: i8; degree: 32; l_build: 50; alpha: 1.2e0; \
             hashed_vector; };",
        );
        assert_eq!(
            index.kind,
            IndexKind::DiskAnn {
                dimension: 1536,
                distance: Some(Distance::InnerProduct),
                vector_type: Some(VectorType::I8),
                degree: Some(32),
                l_build: Some(50),
                alpha: Some(Float(1.2)),
                hashed_vector: true,
            }
        );
        // A repeated clause folds last-one-wins (rule 7) — across the
        // alias spellings too.
        let index = single_index("index u { on: t; fields: a; columns: b; };");
        assert_eq!(index.fields.len(), 1);
        // The integer spellings SurrealQL's number lexer takes.
        let index = single_index("index hnsw v { on: t; fields: a; dimension: 1_0; m0: 0_04; };");
        assert!(matches!(
            index.kind,
            IndexKind::Hnsw {
                dimension: 10,
                m0: Some(4),
                ..
            }
        ));
    }

    /// spec/index.md: `on` and `fields` are required per kind, count
    /// forbids the field clauses outright (rule 10: its table omits
    /// them), and the exactly-one kinds reject multi-field lists. A
    /// failed attempt carries its own diagnostic, never a second
    /// missing-clause report.
    #[test]
    fn index_requires_on_and_validates_fields_arity() {
        // Both missing: both reported, anchored at the block.
        let (_, diags) = parse_file("index u { };\n");
        let clauses: Vec<&str> = diags
            .iter()
            .map(|diag| match diag {
                Diag::RequiredClauseMissing { clause, .. } => *clause,
                other => panic!("expected RequiredClauseMissing: {other:?}"),
            })
            .collect();
        assert_eq!(clauses, ["on", "fields"]);
        // count takes no fields of either spelling: unknown clause.
        match first_error("index count c { on: t; fields: a; };") {
            Diag::UnknownClause { key, statement, .. } => {
                assert_eq!(key, "fields");
                assert_eq!(statement, "index count");
            }
            other => panic!("expected UnknownClause: {other:?}"),
        }
        // Exactly-one kinds: two fields diagnosed at the clause.
        match first_error("index hnsw v { on: t; fields: a, b; dimension: 8; };") {
            Diag::InvalidIndexFields { message, .. } => {
                assert!(message.contains("exactly one"), "message: {message}");
            }
            other => panic!("expected InvalidIndexFields: {other:?}"),
        }
        // …and `columns:` too.
        match first_error("index fulltext f { on: t; columns: a, b; };") {
            Diag::InvalidIndexFields { .. } => {}
            other => panic!("expected InvalidIndexFields: {other:?}"),
        }
        // A failed `on:` attempt (reserved bare word) is diagnosed at
        // the clause, not re-reported as missing.
        let (_, diags) = parse_file("index u { on: select; fields: a; };");
        assert!(
            diags
                .iter()
                .all(|diag| !matches!(diag, Diag::RequiredClauseMissing { .. })),
            "diags: {diags:?}"
        );
        // A missing `on:` on each kind reports once — never a duplicate
        // from a second validation pass.
        for src in [
            "index u { fields: a; };",
            "index unique u { fields: a; };",
            "index count c { };",
            "index fulltext f { fields: a; };",
            "index hnsw v { fields: a; dimension: 8; };",
            "index diskann v { fields: a; dimension: 8; };",
        ] {
            let (_, diags) = parse_file(src);
            let on_diags = diags
                .iter()
                .filter(|diag| matches!(diag, Diag::RequiredClauseMissing { clause: "on", .. }))
                .count();
            assert_eq!(on_diags, 1, "src: {src} — diags: {diags:?}");
        }
    }

    /// spec/index.md's kind domains: `m` ≤ 127 and `m0` within u8
    /// (upstream's post-parse checks), DISKANN's restricted
    /// distance/type sets, the `cosine_normalized` combo, and
    /// `degree`/`l_build` > 0 — all on, `dimension` required for both
    /// vector kinds.
    #[test]
    fn index_vector_kinds_validate_their_domains() {
        // `m` past 127 gets upstream's own message.
        match first_error("index hnsw v { on: t; fields: a; dimension: 8; m: 200; };") {
            Diag::InvalidIndexParam { message, .. } => {
                assert!(message.contains("127"), "message: {message}");
            }
            other => panic!("expected InvalidIndexParam: {other:?}"),
        }
        // `m0` beyond u8.
        match first_error("index hnsw v { on: t; fields: a; dimension: 8; m0: 300; };") {
            Diag::InvalidIndexParam { message, .. } => {
                assert!(message.contains("m0"), "message: {message}");
            }
            other => panic!("expected InvalidIndexParam: {other:?}"),
        }
        // `dimension` required for hnsw and diskann.
        for src in [
            "index hnsw v { on: t; fields: a; };",
            "index diskann v { on: t; fields: a; };",
        ] {
            match first_error(src) {
                Diag::RequiredClauseMissing { clause, .. } => assert_eq!(clause, "dimension"),
                other => panic!("expected RequiredClauseMissing: {other:?}"),
            }
        }
        // DISKANN's distance and type domains.
        match first_error(
            "index diskann v { on: t; fields: a; dimension: 8; distance: chebyshev; };",
        ) {
            Diag::InvalidIndexParam { message, .. } => {
                assert!(message.contains("DISKANN"), "message: {message}");
            }
            other => panic!("expected InvalidIndexParam: {other:?}"),
        }
        match first_error("index diskann v { on: t; fields: a; dimension: 8; type: i64; };") {
            Diag::InvalidIndexParam { .. } => {}
            other => panic!("expected InvalidIndexParam: {other:?}"),
        }
        // …while HNSW takes both unrestricted.
        parse_ok(
            "index hnsw v { on: t; fields: a; dimension: 8; distance: chebyshev; type: i64; };",
        );
        // The cosine_normalized combo.
        match first_error(
            "index diskann v { on: t; fields: a; dimension: 8; distance: cosine_normalized; type: u8; };",
        ) {
            Diag::InvalidIndexParam { message, .. } => {
                assert!(message.contains("COSINE_NORMALIZED"), "message: {message}");
            }
            other => panic!("expected InvalidIndexParam: {other:?}"),
        }
        // `degree`/`l_build` must be positive.
        for clause in ["degree", "l_build"] {
            let src =
                format!("index diskann v {{ on: t; fields: a; dimension: 8; {clause}: 0; }};");
            match first_error(&src) {
                Diag::InvalidIndexParam { message, .. } => {
                    assert!(message.contains(clause), "message: {message}");
                }
                other => panic!("expected InvalidIndexParam: {other:?}"),
            }
        }
    }

    /// The SDL-lexed literal clauses (`bm25:`, `distance:`, the float
    /// scalars) diagnose their shapes with the whole-capture anchor.
    #[test]
    fn index_literal_clauses_validate_their_shapes() {
        // bm25: parens mandatory, exactly two sides.
        match first_error("index fulltext f { on: t; fields: a; bm25: 1.2, 0.75; };") {
            Diag::InvalidBm25 { message, .. } => {
                assert!(message.contains("mandatory"), "message: {message}");
            }
            other => panic!("expected InvalidBm25: {other:?}"),
        }
        for src in [
            "index fulltext f { on: t; fields: a; bm25: (1.2); };",
            "index fulltext f { on: t; fields: a; bm25: (1.2, 0.75, 1); };",
            "index fulltext f { on: t; fields: a; bm25: (1.2, x); };",
        ] {
            match first_error(src) {
                Diag::InvalidBm25 { .. } => {}
                other => panic!("expected InvalidBm25: {other:?} — src: {src}"),
            }
        }
        // distance: unknown word with did-you-mean; minkowski's order
        // mandatory and single; a no-arg distance takes no argument.
        match first_error("index hnsw v { on: t; fields: a; dimension: 8; distance: cosin; };") {
            Diag::InvalidDistance { message, .. } => {
                assert!(message.contains("cosine"), "message: {message}");
            }
            other => panic!("expected InvalidDistance: {other:?}"),
        }
        match first_error("index hnsw v { on: t; fields: a; dimension: 8; distance: minkowski; };")
        {
            Diag::InvalidDistance { message, .. } => {
                assert!(message.contains("order"), "message: {message}");
            }
            other => panic!("expected InvalidDistance: {other:?}"),
        }
        match first_error("index hnsw v { on: t; fields: a; dimension: 8; distance: cosine(1); };")
        {
            Diag::InvalidDistance { .. } => {}
            other => panic!("expected InvalidDistance: {other:?}"),
        }
        // Float scalars: decimal shape only.
        match first_error("index diskann v { on: t; fields: a; dimension: 8; alpha: 1.2.3; };") {
            Diag::InvalidFloat { text, .. } => assert_eq!(text, "1.2.3"),
            other => panic!("expected InvalidFloat: {other:?}"),
        }
        // Magnitudes ≥ 2⁶³ would emit bare prints upstream's integer
        // lexer rejects (`LM 10000000000000000000` — over i64), and a
        // bm25 side that large would cast to an f32 infinity: the
        // bound rejects at parse, where the fix belongs.
        for src in [
            "index hnsw v { on: t; fields: a; dimension: 8; lm: 1e19; };",
            "index fulltext f { on: t; fields: a; bm25: (1e40, 0.75); };",
        ] {
            match first_error(src) {
                Diag::InvalidFloat { .. } | Diag::InvalidBm25 { .. } => {}
                other => panic!("expected a float rejection: {other:?} — src: {src}"),
            }
        }
        // Comments inside the literals are trivia (rule 3's comment
        // model).
        let index =
            single_index("index fulltext f { on: t; fields: a; bm25: (1.2 /* k1 */, 0.75); };");
        assert_eq!(
            index.kind,
            IndexKind::FullText {
                analyzer: None,
                bm25: Some(Bm25(1.2, 0.75)),
                highlights: false
            }
        );
    }

    /// One broken `index` never hides its siblings.
    #[test]
    fn broken_index_does_not_hide_its_siblings() {
        let (file, diags) = parse_file(
            "namespace before_us;\nindex unique broken { fields: a; };\nnamespace after_us;\n",
        );
        assert!(matches!(
            diags.as_slice(),
            [Diag::RequiredClauseMissing { clause: "on", .. }]
        ));
        assert_eq!(file.items.len(), 2);
    }

    // ---- api ----

    fn single_api(src: &str) -> Api {
        let file = parse_ok(src);
        assert_eq!(file.items.len(), 1, "src: {src}");
        match file.items.into_iter().next().expect("len checked") {
            Stmt::Api(api) => api,
            other => panic!("expected an api: {other:?}"),
        }
    }

    /// spec/api.md's examples: the minimal doc-commented group, the
    /// wildcard + any + middleware + permissions shape, and the
    /// `$request` echo. Clause order inside a group is free and the
    /// `any` block may sit anywhere among the groups (SurrealQL
    /// parity); paths decode to their values.
    #[test]
    fn api_grammar() {
        let api = single_api(
            "/// Health check\napi '/health' {\n  get {\n    then: \
             { { status: 200, body: { ok: true } } };\n  };\n};\n",
        );
        assert_eq!(api.path, "/health");
        assert_eq!(api.comment.as_ref().expect("doc").joined(), "Health check");
        assert!(api.any.is_none());
        let [action] = api.actions.as_slice() else {
            panic!("one action: {api:?}")
        };
        assert_eq!(action.methods, vec![ApiMethod::Get]);
        assert!(action.config.permissions.is_none());
        assert!(action.config.middleware.is_empty());
        assert_eq!(
            action.action.canon,
            "{ { status: 200, body: { ok: true } } }"
        );

        // The `any` block first, then its group; permissions take all
        // three forms across the groups.
        let api = single_api(
            "api '/admin/*rest' {\n  any {\n    middleware: api::timeout(5s);\n    then: \
             { { status: 404, body: { error: 'no such route' } } };\n  };\n  get {\n    \
             permissions: full;\n    then: { { status: 200 } };\n  };\n  delete {\n    \
             permissions: none;\n    then: { { status: 204 } };\n  };\n  trace {\n    \
             permissions: $auth.admin = true;\n    then: { { status: 200 } };\n  };\n};\n",
        );
        assert_eq!(api.path, "/admin/*rest");
        assert_eq!(api.actions.len(), 3);
        let any = api.any.expect("any");
        assert_eq!(any.config.middleware.len(), 1);
        assert_eq!(
            any.config.middleware[0].path.segments,
            vec!["api".to_string(), "timeout".to_string()]
        );
        assert_eq!(any.config.middleware[0].args[0].canon, "5s");
        assert_eq!(
            any.fallback.expect("fallback").canon,
            "{ { status: 404, body: { error: 'no such route' } } }"
        );
        assert_eq!(api.actions[0].config.permissions, Some(Permission::Full));
        assert_eq!(api.actions[1].config.permissions, Some(Permission::None));
        assert_eq!(
            api.actions[2].config.permissions,
            Some(Permission::Expr(Expr {
                canon: "$auth.admin = true".to_string()
            }))
        );

        // Clause order inside a group is free; the `any` block may
        // come last; methods comma-group; a `fn::` middleware head
        // carries the prefix.
        let api = single_api(
            "api '/users/:id' {\n  delete, get {\n    then: { { status: 200 } };\n    \
             permissions: none;\n  };\n  any {\n    then: { { status: 404 } };\n  };\n};\n",
        );
        let [action] = api.actions.as_slice() else {
            panic!("one action: {api:?}")
        };
        assert_eq!(action.methods, vec![ApiMethod::Delete, ApiMethod::Get]);
        assert_eq!(action.config.permissions, Some(Permission::None));
        let any = api.any.expect("any");
        assert_eq!(any.fallback.expect("fallback").canon, "{ { status: 404 } }");

        // The path decodes: an escaped backslash survives into the
        // value (the route grammar is the server's business).
        let api = single_api("api '/e\\\\:x' { get { then: 1; }; };");
        assert_eq!(api.path, "/e\\:x");
    }

    /// The bare form, the empty body, and the empty `any` block: all
    /// legal (upstream parses the zero-`FOR` statement), distinct
    /// only in what they carry.
    #[test]
    fn api_bare_and_empty_forms() {
        assert_eq!(single_api("api '/x';"), single_api("api '/x' { };"));
        let api = single_api("api '/x' { any { }; };");
        let any = api.any.expect("any");
        assert!(any.fallback.is_none());
        assert!(any.config.middleware.is_empty());
        assert!(any.config.permissions.is_none());
        // EOF stands in for the final statement's `;`.
        assert_eq!(parse_ok("api '/x'").items.len(), 1);
    }

    /// Repeats of every group clause fold last-one-wins (rule 7),
    /// like any clause.
    #[test]
    fn api_group_clauses_fold_last_one_wins() {
        let api = single_api(
            "api '/x' { get { permissions: none; permissions: full; \
             middleware: api::a(); middleware: api::b(); then: 1; then: 2; }; };",
        );
        let [action] = api.actions.as_slice() else {
            panic!("one action: {api:?}")
        };
        assert_eq!(action.config.permissions, Some(Permission::Full));
        assert_eq!(action.config.middleware.len(), 1);
        assert_eq!(
            action.config.middleware[0].path.segments,
            vec!["api".to_string(), "b".to_string()]
        );
        assert_eq!(action.action.canon, "2");
    }

    /// Every api-specific diagnostic, including the eager validations
    /// upstream only performs at define time.
    #[test]
    fn api_errors() {
        // The path is a string literal only — a word or nothing.
        assert!(matches!(
            first_error("api health { get { then: 1; }; };"),
            Diag::InvalidApiPath { text, .. } if text == "health"
        ));
        assert!(matches!(
            first_error("api { get { then: 1; }; };"),
            Diag::InvalidApiPath { text, .. } if text.is_empty()
        ));
        // Unknown head, with did-you-mean; a quoted head is not a
        // keyword (rule 9) and gets no suggestion.
        let Diag::UnknownApiMethod {
            word, suggestion, ..
        } = first_error("api '/x' { gets { then: 1; }; };")
        else {
            panic!("expected UnknownApiMethod")
        };
        assert_eq!(
            (word.as_str(), suggestion.as_deref()),
            ("gets", Some("get"))
        );
        assert!(matches!(
            first_error("api '/x' { `get` { then: 1; }; };"),
            Diag::UnknownApiMethod {
                suggestion: None,
                ..
            }
        ));
        // `any` is not a list member; a trailing comma is the empty
        // item; an uppercase spelling is not an SDL keyword.
        assert!(matches!(
            first_error("api '/x' { get, any { then: 1; }; };"),
            Diag::UnknownApiMethod { .. }
        ));
        assert!(matches!(
            first_error("api '/x' { get, { then: 1; }; };"),
            Diag::UnknownApiMethod { word, .. } if word.is_empty()
        ));
        assert!(matches!(
            first_error("api '/x' { GET { then: 1; }; };"),
            Diag::UnknownApiMethod { .. }
        ));
        // Duplicates: within one list and across groups.
        assert!(matches!(
            first_error("api '/x' { get, get { then: 1; }; };"),
            Diag::DuplicateApiMethod { .. }
        ));
        assert!(matches!(
            first_error("api '/x' { get { then: 1; }; get, put { then: 2; }; };"),
            Diag::DuplicateApiMethod { .. }
        ));
        // A second `any` is a parse error, not a fold.
        assert!(matches!(
            first_error("api '/x' { any { then: 1; }; any { then: 2; }; };"),
            Diag::DuplicateAnyBlock { .. }
        ));
        // A method group requires `then:` — the `any` block may omit
        // it (that is the no-fallback form).
        assert!(matches!(
            first_error("api '/x' { get { permissions: none; }; };"),
            Diag::MissingApiThen { .. }
        ));
        assert!(
            parse_ok("api '/x' { any { permissions: none; }; };")
                .items
                .len()
                == 1
        );
        // A group head with no body; junk where the body's `{}` opens.
        assert!(matches!(
            first_error("api '/x' { get; };"),
            Diag::UnexpectedClauseForm { .. }
        ));
        // The `any` twin: the one block with no body.
        assert!(matches!(
            first_error("api '/x' { any; };"),
            Diag::UnexpectedClauseForm { .. }
        ));
        // An unknown clause inside a group body is the shared
        // clause-loop error.
        assert!(matches!(
            first_error("api '/x' { get { then: 1; bogus: 2; }; };"),
            Diag::UnknownClause {
                statement: "api group",
                ..
            }
        ));
        // A misplaced doc comment inside the body is framed as the
        // clause it is not.
        assert!(matches!(
            first_error("api '/x' { /// docs\n get { then: 1; }; };"),
            Diag::UnknownClause {
                statement: "api",
                ..
            }
        ));
    }

    /// Stray `;`s between groups are noise, like inside a clause
    /// block (the `namespace staging { ;; };` convention).
    #[test]
    fn api_body_stray_semicolons_are_noise() {
        let api = single_api("api '/x' { ; get { then: 1; }; ;; };");
        assert_eq!(api.actions.len(), 1);
    }

    /// A failed `then` capture diagnoses once and drops only its own
    /// group; a broken head never hides its siblings.
    #[test]
    fn broken_api_does_not_hide_its_siblings() {
        // The broken group's recovery stops at its own `;` (the
        // depth-0 scanner rides over the body's braces).
        let (file, diags) = parse_file(
            "namespace before_us;\napi '/x' { gets { then: 1; }; get { then: 2; }; };\n\
             namespace after_us;\n",
        );
        assert!(matches!(diags.as_slice(), [Diag::UnknownApiMethod { .. }]));
        assert_eq!(file.items.len(), 3);
        let Stmt::Api(api) = &file.items[1] else {
            panic!("expected the api: {:?}", file.items[1])
        };
        assert_eq!(api.actions.len(), 1);
        assert_eq!(api.actions[0].methods, vec![ApiMethod::Get]);

        // A rejected capture drops the group, keeps the statement.
        let (file, diags) =
            parse_file("api '/x' { get { then: 1 COMMENT 'x'; }; put { then: 2; }; };\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::EmbeddedParseError { .. }]
        ));
        let Stmt::Api(api) = &file.items[0] else {
            panic!("expected the api")
        };
        assert_eq!(api.actions.len(), 1);
        assert_eq!(api.actions[0].methods, vec![ApiMethod::Put]);
    }

    // ---- access ----

    fn single_access(src: &str) -> Access {
        let file = parse_ok(src);
        assert_eq!(file.items.len(), 1, "src: {src}");
        match file.items.into_iter().next().expect("len checked") {
            Stmt::Access(access) => access,
            other => panic!("expected an access: {other:?}"),
        }
    }

    /// The jwt variant in both source shapes, with the issuer block,
    /// duration, authenticate, the doc comment, and the level aliases
    /// (rule 9: `kv`/`ns`/`db` canonicalize at parse).
    #[test]
    fn access_jwt_grammar() {
        let access = single_access(
            "/// Tokens issued by Acme SSO\naccess corp_sso {\n  on: database;\n  type: \
             jwt;\n  url: 'https://sso.acme.com/.well-known/jwks.json';\n};\n",
        );
        assert_eq!(access.name.raw, "corp_sso");
        assert_eq!(access.on.value, Level::Database);
        assert_eq!(
            access.comment.as_ref().expect("doc").joined(),
            "Tokens issued by Acme SSO"
        );
        let AccessKind::Jwt { source } = access.kind else {
            panic!("expected jwt: {:?}", access.kind)
        };
        let JwtVerify::Url(url) = source.verify else {
            panic!("expected url verify: {:?}", source.verify)
        };
        assert_eq!(url.canon, "'https://sso.acme.com/.well-known/jwks.json'");
        assert!(source.issuer.is_none());

        // The local-key source with issuer, duration, authenticate.
        let access = single_access(
            "access ops {\n  on: kv;\n  type: jwt;\n  algorithm: es256;\n  key: 'pub';\n  \
             issuer { key: '[REDACTED]'; };\n  duration { session: 8h; token: 1h; };\n  \
             authenticate: $auth.active;\n};\n",
        );
        assert_eq!(
            access.on.value,
            Level::Root,
            "`kv` is the input alias for root"
        );
        let AccessKind::Jwt { source } = access.kind else {
            panic!("expected jwt: {:?}", access.kind)
        };
        let JwtVerify::Key { algorithm, key } = source.verify else {
            panic!("expected key verify: {:?}", source.verify)
        };
        assert_eq!(algorithm, Algorithm::Es256);
        assert_eq!(key.canon, "'pub'");
        let issuer = source.issuer.expect("issuer");
        assert_eq!(issuer.algorithm, None);
        assert_eq!(issuer.key.expect("key").canon, "'[REDACTED]'");
        let duration = access.duration.expect("duration");
        assert_eq!(duration.session.expect("session").canon, "8h");
        assert_eq!(duration.token.expect("token").canon, "1h");
        assert!(duration.grant.is_none());
        assert_eq!(
            access.authenticate.expect("authenticate").canon,
            "$auth.active"
        );

        // A symmetric verify with an explicit equal issuer algorithm
        // (forced-equal — parses, elides at emission). Both keys are
        // secret-bearing slots, spelled with the only legal literal.
        let access = single_access(
            "access mirrored { on: db; type: jwt; algorithm: hs256; key: '[REDACTED]'; issuer { \
             algorithm: hs256; key: '[REDACTED]'; }; };",
        );
        let AccessKind::Jwt { source } = access.kind else {
            panic!("expected jwt")
        };
        let issuer = source.issuer.expect("issuer");
        assert_eq!(issuer.algorithm, Some(Algorithm::Hs256));

        // Every algorithm word of the 13-kind set decodes.
        for (word, expected) in [
            ("eddsa", Algorithm::EdDsa),
            ("es256", Algorithm::Es256),
            ("es384", Algorithm::Es384),
            ("es512", Algorithm::Es512),
            ("hs256", Algorithm::Hs256),
            ("hs384", Algorithm::Hs384),
            ("hs512", Algorithm::Hs512),
            ("ps256", Algorithm::Ps256),
            ("ps384", Algorithm::Ps384),
            ("ps512", Algorithm::Ps512),
            ("rs256", Algorithm::Rs256),
            ("rs384", Algorithm::Rs384),
            ("rs512", Algorithm::Rs512),
        ] {
            // Symmetric verify keys are secret-bearing slots (SDL-E1056):
            // only the sentinel literal authors there.
            let key = if expected.is_symmetric() {
                "'[REDACTED]'"
            } else {
                "'k'"
            };
            let access = single_access(&format!(
                "access a {{ on: root; type: jwt; algorithm: {word}; key: {key}; }}"
            ));
            let AccessKind::Jwt { source } = access.kind else {
                panic!("expected jwt")
            };
            let JwtVerify::Key { algorithm, .. } = source.verify else {
                panic!("expected key verify")
            };
            assert_eq!(algorithm, expected, "word: {word}");
        }
    }

    /// The record and bearer variants: optional signup/signin in any
    /// order, the `jwt { … }` sub-block with its nested issuer
    /// (two-level sub-blocks through the shared clause loop), the
    /// refresh flag, the `for:` subjects, and the database-only
    /// domains.
    #[test]
    fn access_record_and_bearer_grammar() {
        // Neither signup nor signin is required (upstream accepts
        // neither); the jwt-less record stays jwt-less.
        let access = single_access("access plain { on: db; type: record; };");
        let AccessKind::Record {
            signup,
            signin,
            jwt,
            refresh,
        } = access.kind
        else {
            panic!("expected record: {:?}", access.kind)
        };
        assert!(signup.is_none() && signin.is_none() && jwt.is_none() && !refresh);

        // signup/signin in either order, braced bodies.
        let access = single_access(
            "access account { on: database; type: record; signin: { select * from user }; \
             signup: create user set email = $email; };",
        );
        let AccessKind::Record { signup, signin, .. } = access.kind else {
            panic!("expected record")
        };
        assert_eq!(
            signup.expect("signup").canon,
            "(CREATE user SET email = $email)"
        );
        assert_eq!(signin.expect("signin").canon, "{ SELECT * FROM user }");

        // The jwt sub-block with a nested issuer, plus refresh.
        let access = single_access(
            "access member { on: database; type: record; refresh; jwt { algorithm: eddsa; \
             key: 'sign'; issuer { algorithm: eddsa; key: '[REDACTED]'; }; }; };",
        );
        let AccessKind::Record { jwt, refresh, .. } = access.kind else {
            panic!("expected record")
        };
        assert!(refresh);
        let source = jwt.expect("jwt");
        let JwtVerify::Key { algorithm, key } = source.verify else {
            panic!("expected key verify")
        };
        assert_eq!(algorithm, Algorithm::EdDsa);
        assert_eq!(key.canon, "'sign'");
        let issuer = source.issuer.expect("issuer");
        assert_eq!(issuer.algorithm, Some(Algorithm::EdDsa));
        assert_eq!(issuer.key.expect("key").canon, "'[REDACTED]'");

        // Bearer for user (any level) and for record (database-only,
        // enforced below in the errors test); the subject decodes.
        let access = single_access("access ci { on: root; type: bearer; for: user; };");
        let AccessKind::Bearer { subject, jwt } = access.kind else {
            panic!("expected bearer: {:?}", access.kind)
        };
        assert_eq!(subject, BearerSubject::User);
        assert!(jwt.is_none());

        let access = single_access(
            "access devices { on: db; type: bearer; for: record; jwt { url: 'https://x/jwks'; \
             }; };",
        );
        let AccessKind::Bearer { subject, jwt } = access.kind else {
            panic!("expected bearer")
        };
        assert_eq!(subject, BearerSubject::Record);
        let JwtVerify::Url(url) = jwt.expect("jwt").verify else {
            panic!("expected url verify")
        };
        assert_eq!(url.canon, "'https://x/jwks'");

        // The empty duration block folds to an absent duration (rule
        // 9, like user's).
        let access = single_access("access d { on: root; type: jwt; url: 'u'; duration { }; };");
        assert!(access.duration.is_none());
    }

    /// Repeats of every access clause fold last-one-wins (rule 7) —
    /// including the whole `jwt` and `duration` sub-blocks and the
    /// source clauses inside them.
    #[test]
    fn access_clauses_fold_last_one_wins() {
        let access = single_access(
            "access fold { on: root; on: db; type: jwt; url: 'a'; url: 'b'; duration { \
             session: 1h; session: 2h; }; };",
        );
        assert_eq!(access.on.value, Level::Database);
        let AccessKind::Jwt { source } = access.kind else {
            panic!("expected jwt")
        };
        let JwtVerify::Url(url) = source.verify else {
            panic!("expected url verify")
        };
        assert_eq!(url.canon, "'b'");
        assert_eq!(
            access
                .duration
                .expect("duration")
                .session
                .expect("session")
                .canon,
            "2h"
        );

        // Repeated jwt blocks replace wholesale; a source clause
        // inside repeats likewise.
        let access = single_access(
            "access fold2 { on: db; type: record; jwt { url: 'a'; issuer { key: \
             '[REDACTED]'; }; }; jwt { algorithm: hs256; key: '[REDACTED]'; }; };",
        );
        let AccessKind::Record { jwt, .. } = access.kind else {
            panic!("expected record")
        };
        let source = jwt.expect("jwt");
        assert!(matches!(source.verify, JwtVerify::Key { .. }));
        assert!(
            source.issuer.is_none(),
            "the second block replaces the first"
        );
    }

    /// Every access-specific diagnostic: the required discriminant
    /// and level, variant membership, the source shape (missing,
    /// mixed, half pairs), the level domains, bearer's `for:`, and
    /// the issuer rules. Unknown algorithm words ride the word-set
    /// diagnostic with did-you-mean.
    #[test]
    fn access_errors() {
        // Missing type: / failed type:.
        assert!(matches!(
            first_error("access a { on: root; };"),
            Diag::RequiredClauseMissing {
                clause: "type",
                block: "access",
                ..
            }
        ));
        assert!(matches!(
            first_error("access a { on: root; type: banana; };"),
            Diag::UnknownClauseWord { key: "type", .. }
        ));
        // Missing on:.
        assert!(matches!(
            first_error("access a { type: jwt; url: 'u'; };"),
            Diag::RequiredClauseMissing {
                clause: "on",
                block: "access",
                ..
            }
        ));
        // No source at all.
        assert!(matches!(
            first_error("access a { on: root; type: jwt; };"),
            Diag::InvalidAccessSource { .. }
        ));
        // The shapes mixed.
        let error =
            first_error("access a { on: root; type: jwt; url: 'u'; algorithm: hs256; key: 'k'; };");
        assert!(matches!(error, Diag::InvalidAccessSource { .. }));
        assert!(
            format!("{error}").contains("takes no local key"),
            "the mix names the conflict: {error}"
        );
        // Half pairs, each direction.
        assert!(matches!(
            first_error("access a { on: root; type: jwt; algorithm: hs256; };"),
            Diag::InvalidAccessSource { .. }
        ));
        assert!(matches!(
            first_error("access a { on: root; type: jwt; key: 'k'; };"),
            Diag::InvalidAccessSource { .. }
        ));
        // Variant membership: a record clause on a jwt access, a
        // bearer clause on a record one.
        assert!(matches!(
            first_error("access a { on: root; type: jwt; signup: 1; url: 'u'; };"),
            Diag::InvalidAccessClause { .. }
        ));
        assert!(matches!(
            first_error("access a { on: db; type: record; for: user; };"),
            Diag::InvalidAccessClause { .. }
        ));
        // The level domains.
        assert!(matches!(
            first_error("access a { on: root; type: record; };"),
            Diag::InvalidAccessLevel { .. }
        ));
        assert!(matches!(
            first_error("access a { on: ns; type: bearer; for: record; };"),
            Diag::InvalidAccessLevel { .. }
        ));
        // Bearer's required for: (a failed attempt stays silent).
        assert!(matches!(
            first_error("access a { on: root; type: bearer; };"),
            Diag::RequiredClauseMissing {
                clause: "for",
                block: "access",
                ..
            }
        ));
        // Issuer rules: mismatch and missing key.
        assert!(matches!(
            first_error(
                "access a { on: root; type: jwt; algorithm: es256; key: 'k'; issuer { \
                 algorithm: hs256; key: 's'; }; };"
            ),
            Diag::InvalidAccessIssuer { .. }
        ));
        assert!(matches!(
            first_error(
                "access a { on: root; type: jwt; algorithm: es256; key: 'k'; issuer { }; };"
            ),
            Diag::InvalidAccessIssuer { .. }
        ));
        // A symmetric verify mirrors the key, so a key-less issuer is
        // the one legal key-less shape.
        assert!(
            parse_ok(
                "access a { on: root; type: jwt; algorithm: hs256; key: '[REDACTED]'; issuer { \
             algorithm: hs256; }; };"
            )
            .items
            .len()
                == 1
        );
        // Unknown algorithm word, with did-you-mean.
        assert!(matches!(
            first_error("access a { on: root; type: jwt; algorithm: hs255; key: 'k'; };"),
            Diag::UnknownClauseWord {
                key: "algorithm",
                ..
            }
        ));
        // The no-source report for a `jwt { … }` block anchors on the
        // block's own clause key; the jwt variant's anchors the `{`.
        let src = "access x { on: db; type: record; jwt { }; };";
        let (_, diags) = parse_file(src);
        match diags.as_slice() {
            [Diag::InvalidAccessSource { span, .. }] => assert_eq!(span.slice(src), "jwt"),
            other => panic!("expected one source diagnostic: {other:?}"),
        }
    }

    /// The exposure guard (spec/access.md's shape-only rule, SDL-E1056):
    /// symmetric verify keys and every issuer key accept only the
    /// `'[REDACTED]'` sentinel; asymmetric verify keys stay freely
    /// authorable. Anchors ride the offending clause's own key word.
    /// The 3.3 access clauses (spec/access.md): `audience:`
    /// rides the jwt source (the variant's direct clauses and the
    /// `jwt { … }` sub-block), `context:` rides a record-
    /// authenticating access at the database level — the engine's
    /// define-time shape demand mirrored at the fold (the read-only
    /// half is semantic, the engine's to say). Codegen prints CONTEXT
    /// after AUTHENTICATE (the printer's re-parse note) and AUDIENCE
    /// between the source and the issuer.
    #[test]
    fn access_context_and_audience() {
        // Positive: audience direct (one and two values, decoded) and
        // in the record's jwt sub-block; codegen in the printer's
        // fixed positions.
        let file =
            parse_ok("access a { on: db; type: jwt; url: 'u'; audience: 'aud1', 'aud2'; };\n");
        let Stmt::Access(access) = &file.items[0] else {
            panic!()
        };
        let AccessKind::Jwt { source } = &access.kind else {
            panic!()
        };
        assert_eq!(
            source.audience.as_deref(),
            Some(["aud1".to_string(), "aud2".to_string()].as_slice())
        );
        let ddl = crate::codegen::emit(&file);
        assert_eq!(
            ddl.trim_end(),
            "DEFINE ACCESS OVERWRITE a ON DATABASE TYPE JWT URL 'u' AUDIENCE 'aud1', \
             'aud2';"
        );
        let file =
            parse_ok("access r { on: db; type: record; jwt { url: 'u'; audience: 'a'; }; };\n");
        let Stmt::Access(access) = &file.items[0] else {
            panic!()
        };
        let AccessKind::Record { jwt, .. } = &access.kind else {
            panic!()
        };
        assert_eq!(
            jwt.as_ref().and_then(|s| s.audience.as_deref()),
            Some(["a".to_string()].as_slice())
        );
        // Positive: context on a record access, and on a
        // bearer-for-record one; codegen prints it after
        // AUTHENTICATE, before DURATION.
        let file = parse_ok(
            "access c { on: db; type: record; signup: create user set email = $email; \
             authenticate: $auth.active; context: { extra: 1 }; };\n",
        );
        let ddl = crate::codegen::emit(&file);
        // The emission may lay the skeleton clause-per-line (issue #1's
        // model), so the ordering claim compares whitespace-collapsed.
        let flat = ddl.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains("AUTHENTICATE $auth.active CONTEXT { extra: 1 }"),
            "{ddl}"
        );
        let file =
            parse_ok("access b { on: db; type: bearer; for: record; context: { a: 1 }; };\n");
        assert!(matches!(&file.items[0], Stmt::Access(_)));
        // CONTEXT on a jwt access — the membership pass names the
        // owners, and it is the ONLY diagnostic (one root cause, one
        // diagnostic: the shape check below skips the jwt spelling).
        {
            let (_, diags) =
                parse_file("access a { on: db; type: jwt; url: 'u'; context: { a: 1 }; };\n");
            assert_eq!(diags.len(), 1, "one root cause, one diagnostic: {diags:?}");
            match &diags[0] {
                Diag::InvalidAccessClause { message, .. } => {
                    assert!(message.contains("record/bearer"), "{message}");
                }
                other => panic!("expected InvalidAccessClause: {other:?}"),
            }
        }
        // CONTEXT on a bearer for users — the shape half (the engine's
        // "authenticates a record" message).
        match first_error("access a { on: db; type: bearer; for: user; context: { a: 1 }; };") {
            Diag::InvalidAccessClause { message, .. } => {
                assert!(message.contains("authenticates a record"), "{message}");
            }
            other => panic!("expected InvalidAccessClause: {other:?}"),
        }
        // CONTEXT at the root level — nothing there authenticates a
        // record.
        assert!(matches!(
            first_error("access a { on: root; type: bearer; for: user; context: { a: 1 }; };"),
            Diag::InvalidAccessClause { .. }
        ));
        // AUDIENCE outside the jwt source (a direct record clause) —
        // membership names jwt.
        match first_error("access a { on: db; type: record; audience: 'a'; };") {
            Diag::InvalidAccessClause { message, .. } => {
                assert!(message.contains("belongs to a jwt"), "{message}");
            }
            other => panic!("expected InvalidAccessClause: {other:?}"),
        }
        // A non-string audience item is a parse-level rejection (the
        // engine string-checks at define; SDL's list is string
        // literals).
        assert!(matches!(
            first_error("access a { on: db; type: jwt; url: 'u'; audience: 1; };"),
            Diag::InvalidString { .. }
        ));
    }

    #[test]
    fn access_secret_exposure_guard() {
        // A real symmetric verify key.
        let src = "access a { on: db; type: jwt; algorithm: hs256; key: 'real'; };";
        let (_, diags) = parse_file(src);
        match diags.as_slice() {
            [Diag::AccessSecretExposure { span, .. }] => assert_eq!(span.slice(src), "key"),
            other => panic!("expected one exposure diagnostic: {other:?}"),
        }
        // A real issuer key — even beside a public (asymmetric) verify
        // key, and inside the `jwt { … }` sub-block.
        let src = "access a { on: db; type: jwt; algorithm: es256; key: 'pub'; issuer { key: \
                   'real'; }; };";
        let (_, diags) = parse_file(src);
        match diags.as_slice() {
            [Diag::AccessSecretExposure { span, .. }] => {
                assert_eq!(span.slice(src), "issuer", "the issuer block's clause key")
            }
            other => panic!("expected one exposure diagnostic: {other:?}"),
        }
        let (_, diags) = parse_file(
            "access a { on: db; type: record; jwt { algorithm: hs384; key: 'real'; }; };",
        );
        assert!(matches!(
            diags.as_slice(),
            [Diag::AccessSecretExposure { .. }]
        ));
        // The sentinel authors everywhere the guard applies — direct,
        // mirrored, and sub-block.
        for src in [
            "access a { on: db; type: jwt; algorithm: hs256; key: '[REDACTED]'; };",
            "access a { on: db; type: jwt; algorithm: hs512; key: '[REDACTED]'; issuer { \
             key: '[REDACTED]'; }; };",
            "access a { on: db; type: bearer; for: user; jwt { algorithm: hs256; key: \
             '[REDACTED]'; }; };",
        ] {
            let (_, diags) = parse_file(src);
            assert!(diags.is_empty(), "the sentinel authors: {src}: {diags:?}");
        }
        // Structural errors keep firing before the guard (one root
        // cause, one diagnostic): a mixed source never reports the key.
        assert!(matches!(
            first_error(
                "access a { on: root; type: jwt; url: 'u'; algorithm: hs256; key: 'real'; };"
            ),
            Diag::InvalidAccessSource { .. }
        ));
    }

    /// A broken access neither hides its siblings nor its own
    /// remainder: statement-level recovery skips to the depth-0 `;`.
    #[test]
    fn broken_access_does_not_hide_its_siblings() {
        let (file, diags) = parse_file(
            "namespace before_us;\naccess broken { on: root; type: jwt; };\nnamespace \
             after_us;\n",
        );
        assert!(matches!(
            diags.as_slice(),
            [Diag::InvalidAccessSource { .. }]
        ));
        assert_eq!(file.items.len(), 2);
        assert!(matches!(&file.items[0], Stmt::Namespace(_)));
        assert!(matches!(&file.items[1], Stmt::Namespace(_)));

        // A garbled issuer block keeps its statement dead but the
        // file's other statements alive.
        let (file, diags) = parse_file(
            "access bad { on: root; type: jwt; algorithm: es256; key: 'k'; issuer { algorithm: \
             banana; }; };\nnamespace next;\n",
        );
        assert!(
            diags
                .iter()
                .any(|d| matches!(d, Diag::UnknownClauseWord { .. })),
            "the bad algorithm word: {diags:?}"
        );
        assert_eq!(file.items.len(), 1);
        assert!(matches!(&file.items[0], Stmt::Namespace(_)));
    }

    fn single_table(src: &str) -> Table {
        let file = parse_ok(src);
        assert_eq!(file.items.len(), 1, "src: {src}");
        match file.items.into_iter().next().expect("len checked") {
            Stmt::Table(table) => table,
            other => panic!("expected a table: {other:?}"),
        }
    }

    fn relation(from: Option<Vec<&str>>, to: Option<Vec<&str>>) -> TableKind {
        TableKind::Relation {
            from: from.map(|list| list.into_iter().map(Ident::new).collect()),
            to: to.map(|list| list.into_iter().map(Ident::new).collect()),
            enforced: false,
            lightweight: false,
        }
    }

    /// spec/table.md: the kind rides the header — left to right, no
    /// backtracking; the variant words are variant words ONLY when
    /// followed by a name; the arrows are mandatory on both sides of
    /// the name.
    #[test]
    fn table_header_forms() {
        // Bare table → Normal; the schema flag rides the header (the
        // spec's example spelling) or the block (the grammar's).
        let table = single_table("table user;");
        assert_eq!(table.name, Ident::new("user"));
        assert_eq!(table.kind, TableKind::Normal);
        assert_eq!(table.schema_full, None);
        let table = single_table("table user schemafull { };");
        assert_eq!(table.kind, TableKind::Normal);
        assert_eq!(table.schema_full, Some(true));
        let table = single_table("table user { schemaless; };");
        assert_eq!(table.schema_full, Some(false));
        // Quoted spellings are names, never keywords (rule 9).
        let table = single_table("table `any` { };");
        assert_eq!(table.name, Ident::new("any"));
        assert_eq!(table.kind, TableKind::Normal);
        // The variant words.
        assert_eq!(single_table("table any t { };").kind, TableKind::Any);
        assert_eq!(
            single_table("table relation generic_edge;").kind,
            relation(None, None)
        );
        match single_table("table view stats { select author from post; };").kind {
            TableKind::View { query } => assert_eq!(query.canon, "SELECT author FROM post"),
            other => panic!("expected a view: {other:?}"),
        }
        // …are variant words only when followed by a name: a table
        // named `any` (block follows), a from-list identifier `any`
        // (arrow follows).
        let table = single_table("table any { };");
        assert_eq!(table.name, Ident::new("any"));
        assert_eq!(table.kind, TableKind::Normal);
        assert_eq!(
            single_table("table any -> wrote ->;").kind,
            relation(Some(vec!["any"]), None)
        );
        // The arrow forms: full, one-sided, unconstrained; pipe
        // lists; the to-list runs to the block or `;`.
        assert_eq!(
            single_table("table user | org -> wrote -> post | comment { };").kind,
            relation(Some(vec!["user", "org"]), Some(vec!["post", "comment"]))
        );
        assert_eq!(
            single_table("table follower -> watched ->;").kind,
            relation(Some(vec!["follower"]), None)
        );
        assert_eq!(
            single_table("table -> tagged -> post;").kind,
            relation(None, Some(vec!["post"]))
        );
        assert_eq!(
            single_table("table -> follows ->;").kind,
            relation(None, None)
        );
        // Doc comments attach (the statement has a COMMENT clause).
        let table = single_table("/// docs\ntable t { };");
        assert_eq!(
            table.comment.map(|comment| comment.lines),
            Some(vec!["docs".to_string()])
        );
    }

    /// spec/table.md: the clause vocabulary folds last-one-wins
    /// (rule 7); the per-op permission groups land as written; the
    /// nested index/event inherit the host table (their standalone
    /// `on:` forbidden).
    #[test]
    fn table_clauses_fold() {
        // Flags and the changefeed sub-block.
        let table =
            single_table("table audit { drop; changefeed { expiry: 1d; include original; }; };");
        assert!(table.drop);
        let changefeed = table.changefeed.expect("changefeed written");
        assert_eq!(changefeed.expiry.to_string(), "1d");
        assert!(changefeed.include_original);
        // The schema flags fold last-one-wins across header AND block.
        assert_eq!(
            single_table("table t schemaless { schemafull; };").schema_full,
            Some(true)
        );
        // Single-value permissions: the shorthand only.
        let table = single_table("table t { permissions: none; };");
        assert_eq!(table.permissions, Some(OpPermissions::None));
        let table = single_table("table t { permissions: full; };");
        assert_eq!(table.permissions, Some(OpPermissions::Full));
        // Per-op groups: comma-grouped ops, expression values,
        // preserved as written.
        let table = single_table(
            "table t { permissions { select, create: full; update: $auth.id = id; delete: \
             none; }; };",
        );
        match table.permissions {
            Some(OpPermissions::Groups(groups)) => {
                assert_eq!(groups.len(), 3);
                assert_eq!(
                    groups[0].ops,
                    vec![PermissionOp::Select, PermissionOp::Create]
                );
                assert_eq!(groups[0].value, Permission::Full);
                assert_eq!(groups[1].ops, vec![PermissionOp::Update]);
                assert_eq!(
                    groups[1].value,
                    Permission::Expr(Expr {
                        canon: "$auth.id = id".to_string(),
                    })
                );
                assert_eq!(groups[2].ops, vec![PermissionOp::Delete]);
                assert_eq!(groups[2].value, Permission::None);
            }
            other => panic!("expected permission groups: {other:?}"),
        }
        // graphql sub-block.
        let table = single_table("table t { graphql { alias: 'a'; deprecated: 'd'; }; };");
        assert_eq!(
            table
                .graphql
                .map(|graphql| (graphql.alias, graphql.deprecated)),
            Some((Some("a".to_string()), Some("d".to_string())))
        );
        // Nested definitions inherit the host; source order is kept.
        let table = single_table(
            "table post schemafull { index unique slug_unique { fields: slug; }; event \
             audit { when: true; then: 1; }; };",
        );
        assert_eq!(table.nested.len(), 2);
        match &table.nested[0] {
            Stmt::Index(index) => {
                assert_eq!(index.name, Ident::new("slug_unique"));
                assert_eq!(index.on, Ident::new("post"));
            }
            other => panic!("expected a nested index: {other:?}"),
        }
        match &table.nested[1] {
            Stmt::Event(event) => {
                assert_eq!(event.name, Ident::new("audit"));
                assert_eq!(event.on, Ident::new("post"));
            }
            other => panic!("expected a nested event: {other:?}"),
        }
        // The relation-only `enforced;` folds on the relation kinds —
        // arrow and relation-word alike.
        assert!(matches!(
            &single_table("table -> wrote -> post { enforced; };").kind,
            TableKind::Relation { enforced: true, .. }
        ));
        assert!(matches!(
            &single_table("table relation g { enforced; };").kind,
            TableKind::Relation { enforced: true, .. }
        ));
    }

    /// The view body: the query comes first (`;`-terminated, no
    /// clause key), then the ordinary two-key clause loop. The
    /// restricted select rides the embedded parser's own view
    /// position.
    #[test]
    fn table_view_body() {
        // Query + trailing clauses.
        let table = single_table(
            "table view stats { select author, count() as posts from post group by author; \
             permissions: full; };",
        );
        match &table.kind {
            TableKind::View { query } => assert_eq!(
                query.canon,
                "SELECT author, count() AS posts FROM post GROUP BY author"
            ),
            other => panic!("expected a view: {other:?}"),
        }
        assert_eq!(table.permissions, Some(OpPermissions::Full));
        // A clause-key-shaped head is the misplaced query — the
        // dedicated view-body diagnostic, not an embedded rejection —
        // and the ONLY diagnostic: the body recovery consumes the
        // view's tail so no spurious junk follows. The view is dead
        // but the file's sibling survives.
        let (file, diags) = parse_file(
            "table view stats { permissions: full; select author from post; };\nnamespace \
             later;\n",
        );
        assert!(matches!(diags.as_slice(), [Diag::InvalidViewBody { .. }]));
        assert_eq!(file.items.len(), 1, "the sibling survives: {diags:?}");
        assert!(matches!(&file.items[0], Stmt::Namespace(_)));
        assert!(matches!(
            first_error("table view stats { };"),
            Diag::InvalidViewBody { .. }
        ));
        // The block-form clause head as the body's ONLY content: the
        // raw-source peek diagnoses the misplaced query before the
        // capture can die on the view's own closer.
        let (file, diags) =
            parse_file("table view stats { permissions { select: full; } };\nnamespace later;\n");
        assert!(matches!(diags.as_slice(), [Diag::InvalidViewBody { .. }]));
        assert_eq!(file.items.len(), 1, "the sibling survives: {diags:?}");
        assert!(matches!(&file.items[0], Stmt::Namespace(_)));
        // The comment-interrupted head evades the raw-source peek —
        // the capture-path text check catches it with the same
        // dedicated diagnostic.
        let (file, diags) = parse_file(
            "table view stats { permissions /* why */ : full; select author from post; };\n\
             namespace later;\n",
        );
        assert!(matches!(diags.as_slice(), [Diag::InvalidViewBody { .. }]));
        assert_eq!(file.items.len(), 1, "the sibling survives: {diags:?}");
        assert!(matches!(&file.items[0], Stmt::Namespace(_)));
        // A query missing its terminator dies on the view's own
        // closer (the unbalanced-capture report) — still the sole
        // diagnostic, tail consumed, sibling alive.
        let (file, diags) =
            parse_file("table view stats { selet * from post };\nnamespace later;\n");
        assert!(matches!(diags.as_slice(), [Diag::UnbalancedCapture { .. }]));
        assert_eq!(file.items.len(), 1, "the sibling survives: {diags:?}");
        assert!(matches!(&file.items[0], Stmt::Namespace(_)));
        // A rejected query with SEVERAL leftover segments: every
        // segment is consumed; exactly one diagnostic, sibling alive.
        let (file, diags) = parse_file(
            "table view stats { selet * from post; permissions: full; graphql { alias: 'a'; \
             } };\nnamespace later;\n",
        );
        assert!(matches!(
            diags.as_slice(),
            [Diag::EmbeddedParseError { .. }]
        ));
        assert_eq!(file.items.len(), 1, "the sibling survives: {diags:?}");
        assert!(matches!(&file.items[0], Stmt::Namespace(_)));
        // Beyond the restricted select: rejected by the embedded
        // parser (the ORDER BY the view grammar does not take) —
        // again as the sole diagnostic, sibling intact.
        let (file, diags) =
            parse_file("table view stats { select * from post order by id; };\nnamespace later;\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::EmbeddedParseError { .. }]
        ));
        assert_eq!(file.items.len(), 1, "the sibling survives: {diags:?}");
        assert!(matches!(&file.items[0], Stmt::Namespace(_)));
    }

    /// spec/table.md's rejections: the one-arrow header, empty
    /// positions, `enforced;` off a relation, bare-expression
    /// permissions, a nested definition's explicit `on:`, and comma
    /// endpoint lists.
    #[test]
    fn table_errors() {
        // One arrow is the parse error — the second is mandatory.
        assert!(matches!(
            first_error("table user -> wrote { };"),
            Diag::InvalidTableHeader { .. }
        ));
        assert!(matches!(
            first_error("table user -> wrote -> post, comment;"),
            Diag::InvalidTableHeader { .. }
        ));
        assert!(matches!(
            first_error("table user, org -> wrote -> post;"),
            Diag::InvalidTableHeader { .. }
        ));
        // `enforced;` is relation-only (access's membership pass).
        assert!(matches!(
            first_error("table t { enforced; };"),
            Diag::InvalidTableClause { .. }
        ));
        assert!(matches!(
            first_error("table any t { enforced; };"),
            Diag::InvalidTableClause { .. }
        ));
        // Tables take the per-op parser: no bare expression.
        assert!(matches!(
            first_error("table t { permissions: $auth.id = id; };"),
            Diag::InvalidPermissionsShorthand { .. }
        ));
        // Unknown ops did-you-mean over the four.
        match first_error("table t { permissions { select: full; updat: none; }; };") {
            Diag::UnknownClauseWord { suggestion, .. } => {
                assert_eq!(suggestion.as_deref(), Some("update"))
            }
            other => panic!("expected UnknownClauseWord: {other:?}"),
        }
        // A nested definition cannot carry `on:` — the host table IS
        // the `on` (exactly one of the two applies).
        assert!(matches!(
            first_error("table t { index u { on: t; fields: a; }; };"),
            Diag::InvalidNestedOn { .. }
        ));
        assert!(matches!(
            first_error("table t { event e { on: t; then: 1; }; };"),
            Diag::InvalidNestedOn { .. }
        ));
        // Unknown keys did-you-mean over the table's clauses AND the
        // nested heads.
        match first_error("table t { indx u { fields: a; }; };") {
            Diag::UnknownClause { suggestion, .. } => {
                assert_eq!(suggestion.as_deref(), Some("index"))
            }
            other => panic!("expected UnknownClause: {other:?}"),
        }
        // The reserved word cannot be the bare name (rule 9).
        assert!(matches!(
            first_error("table select { };"),
            Diag::ReservedIdent { .. }
        ));
    }

    // ---- LIGHTWEIGHT relations + the INLINE cache caps ----

    /// spec/table.md's 3.3 grammar: the flag trails the header, the
    /// implication folds (`lightweight` ⇒ `enforced` — the engine's
    /// define normalization), the to-list read is greedy (an endpoint
    /// named `lightweight` reads as an endpoint), and the INLINE caps
    /// fold with their ≤256 domain.
    #[test]
    fn lightweight_and_inline_caps() {
        // The header word; `enforced` arrives folded.
        let table = single_table("table person -> likes -> post lightweight;");
        assert_eq!(
            table.kind,
            TableKind::Relation {
                from: Some(vec![Ident::new("person")]),
                to: Some(vec![Ident::new("post")]),
                enforced: true,
                lightweight: true,
            }
        );
        // Explicit `enforced;` alongside the flag folds to the same AST.
        let table = single_table("table person -> likes -> post lightweight { enforced; };");
        assert!(matches!(
            table.kind,
            TableKind::Relation {
                enforced: true,
                lightweight: true,
                ..
            }
        ));
        // The greedy read: an endpoint named `lightweight` is an
        // endpoint — the flag needs the list closed before it.
        let table = single_table("table person -> likes -> lightweight;");
        assert_eq!(
            table.kind,
            TableKind::Relation {
                from: Some(vec![Ident::new("person")]),
                to: Some(vec![Ident::new("lightweight")]),
                enforced: false,
                lightweight: false,
            }
        );
        // The caps fold, in either clause position (rule 7), on any
        // kind.
        let table = single_table("table t { inline references: 4; inline edges: 8; };");
        assert_eq!(table.inline_edges, Some(8));
        assert_eq!(table.inline_references, Some(4));
        // The boundary values ride (0 and 256 — the engine's domain).
        let table = single_table("table t { inline edges: 256; };");
        assert_eq!(table.inline_edges, Some(256));
        // Caps + lightweight combine (both probe-accepted on the engine).
        let table = single_table("table person -> likes -> post lightweight { inline edges: 8; };");
        assert_eq!(table.inline_edges, Some(8));
        assert!(matches!(
            table.kind,
            TableKind::Relation {
                enforced: true,
                lightweight: true,
                ..
            }
        ));
        // A field named `inline` stays authorable — the key extension
        // needs the second word (`inline edges`) to fire, and `edges`
        // alone is not a key.
        let table = single_table("table t { inline: string; edges: int; };");
        assert_eq!(table.nested.len(), 2);
    }

    /// The LIGHTWEIGHT shape's eager validations (the engine's
    /// define-time rejections mirrored at the fold, rule 10's ADT
    /// pattern) and the caps' domain.
    #[test]
    fn lightweight_and_caps_rejections() {
        // Both sides required (the engine: "requires explicit IN and
        // OUT tables"). The arrow form's greedy to-list read absorbs a
        // to-side word (`-> lightweight;` is an endpoint), so
        // missing-FROM is the only spellable sides violation — the
        // missing-TO shape is unreachable at parse (the validation
        // still guards the hand-built AST).
        assert!(matches!(
            first_error("table -> likes -> post lightweight;"),
            Diag::InvalidTableClause { .. }
        ));
        // The record-consuming clauses (the engine: "cannot be
        // declared …: its edges store no records").
        assert!(matches!(
            first_error("table person -> likes -> post lightweight { drop; };"),
            Diag::InvalidTableClause { .. }
        ));
        assert!(matches!(
            first_error(
                "table person -> likes -> post lightweight { changefeed { expiry: 30d; }; };"
            ),
            Diag::InvalidTableClause { .. }
        ));
        // The schema flag rides the header too (the spec's example
        // spelling) — both spellings reject under lightweight.
        assert!(matches!(
            first_error("table person -> likes -> post lightweight schemafull;"),
            Diag::InvalidTableClause { .. }
        ));
        assert!(matches!(
            first_error("table person -> likes -> post lightweight { schemafull; };"),
            Diag::InvalidTableClause { .. }
        ));
        // The children (the engine: "cannot carry a field definition/
        // an index/an event").
        assert!(matches!(
            first_error("table person -> likes -> post lightweight { field extra: string; };"),
            Diag::InvalidTableClause { .. }
        ));
        assert!(matches!(
            first_error("table person -> likes -> post lightweight { index i { fields: id; }; };"),
            Diag::InvalidTableClause { .. }
        ));
        assert!(matches!(
            first_error("table person -> likes -> post lightweight { event e { then: 1; }; };"),
            Diag::InvalidTableClause { .. }
        ));
        // `lightweight;` in the block is not the flag's spelling —
        // the flag rides the header, and the block-local head opens a
        // nested FIELD named `lightweight` (a legal field name — the
        // keyword set is not the reserved set), which then fails the
        // field's own type requirement.
        assert!(matches!(
            first_error("table person -> likes -> post { lightweight; };"),
            Diag::FieldTypeRequired { .. }
        ));
        // The caps' domain: >256 is the engine's own define rejection.
        assert!(matches!(
            first_error("table t { inline edges: 257; };"),
            Diag::InvalidTableParam { .. }
        ));
        assert!(matches!(
            first_error("table t { inline references: 300; };"),
            Diag::InvalidTableParam { .. }
        ));
    }

    #[test]
    fn broken_table_does_not_hide_its_siblings() {
        let (file, diags) =
            parse_file("namespace before_us;\ntable broken { enforced; };\nnamespace after_us;\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::InvalidTableClause { .. }]
        ));
        assert_eq!(file.items.len(), 2);
        assert!(matches!(&file.items[0], Stmt::Namespace(_)));
        assert!(matches!(&file.items[1], Stmt::Namespace(_)));

        // A garbled nested index keeps its own diagnostic and the
        // file's other statements alive; the table itself survives —
        // its own clauses are sound, and the broken nested definition
        // already reported (a broken sub-part is dropped, like a
        // changefeed without its expiry).
        let (file, diags) = parse_file("table bad { index u { fields: ; }; };\nnamespace next;\n");
        assert!(
            diags
                .iter()
                .any(|d| matches!(d, Diag::EmbeddedParseError { .. })),
            "the bad field capture: {diags:?}"
        );
        assert_eq!(file.items.len(), 2);
        assert!(matches!(&file.items[0], Stmt::Table(_)));
        assert!(matches!(&file.items[1], Stmt::Namespace(_)));

        // A nested head with a MALFORMED HEADER (no name) must not
        // overrun the enclosing block: its recovery stops at the
        // clause boundary, the table closes, and the file's siblings
        // survive (the nested-recovery contract).
        let (file, diags) = parse_file("table t { index { fields: a; }; };\nnamespace next;\n");
        assert!(
            diags.iter().any(|d| matches!(d, Diag::ExpectedName { .. })),
            "the missing name: {diags:?}"
        );
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d, Diag::UnknownClause { .. }))
        );
        assert_eq!(file.items.len(), 2, "siblings survive: {diags:?}");
        assert!(matches!(&file.items[0], Stmt::Table(_)));
        assert!(matches!(&file.items[1], Stmt::Namespace(_)));

        let (file, diags) = parse_file("table t { event { then: 1; }; };\nnamespace next;\n");
        assert!(
            diags.iter().any(|d| matches!(d, Diag::ExpectedName { .. })),
            "the missing name: {diags:?}"
        );
        assert_eq!(file.items.len(), 2, "siblings survive: {diags:?}");
        assert!(matches!(&file.items[0], Stmt::Table(_)));
        assert!(matches!(&file.items[1], Stmt::Namespace(_)));
    }

    // ---- field ----

    fn single_field(src: &str) -> Field {
        match single_statement(src) {
            Stmt::Field(field) => field,
            other => panic!("expected a field: {other:?}"),
        }
    }

    #[test]
    fn field_forms_parse_to_presence() {
        // The standalone form: type + on + assert + default + doc
        // comment (rule 5 — field has a COMMENT clause).
        let field = single_field(
            "/// Contact address\nfield email: string {\n  on: account;\n  assert: \
             string::is_email($value);\n  default: 'unknown@example.com';\n};",
        );
        assert_eq!(
            field.name.sole_name().map(|n| n.raw.clone()),
            Some("email".into())
        );
        assert_eq!(field.kind, Some(Kind::new("string")));
        assert_eq!(field.on.raw, "account");
        assert!(
            matches!(&field.assert, Some(Expr { canon }) if canon == "string::is_email($value)"),
            "the assert canon: {:?}",
            field.assert
        );
        assert!(matches!(&field.default, Some(FieldDefault::Set(_))));
        assert_eq!(
            field.comment.map(|c| c.joined()),
            Some("Contact address".to_string())
        );
        // The bare one-liner.
        let field = single_field("field stats: object { on: t; };");
        assert_eq!(field.kind, Some(Kind::new("object")));
        // The computed short forms are nested-only sugar: standalone a
        // field always needs its `on:` clause, and the short form has
        // no block to carry one (the spec's block-form note). Inside
        // a table block they read as bare heads.
        let table = single_table(
            "table t {\n  shout: string = string::uppercase(name);\n  derived = \
             string::uppercase(name);\n};",
        );
        let computed_fields: Vec<(Option<&str>, bool)> = table
            .nested
            .iter()
            .filter_map(|stmt| match stmt {
                Stmt::Field(field) => Some((
                    field.kind.as_ref().map(|k| k.text.as_str()),
                    field.computed.is_some(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            computed_fields,
            vec![(Some("string"), true), (None, true)],
            "the typed and typeless computed short forms"
        );
        // Digit-bearing names are fields, not typo'd keys (the
        // detection's lowercase-only reading would miss them); an
        // UPPERCASE spelling stays a typo'd key with its
        // did-you-mean.
        let table = single_table("table t { user2: string; };");
        assert!(matches!(table.nested.as_slice(), [Stmt::Field(_)]));
        let (_, diags) = parse_file("table t { INDEX2: string; };");
        match &diags[0] {
            Diag::UnknownClause { suggestion, .. } => {
                assert_eq!(suggestion.as_deref(), Some("index"))
            }
            other => panic!("expected UnknownClause: {other:?}"),
        }
        // The typo guard is a NEAR-IDENTICAL spelling of a real key
        // (one edit): `id` and `in` are spec-promised field names,
        // not misspelled `index` — and `in`/`out` are the relation
        // endpoints' own shape (spec/field.md).
        for head in [
            "id: string;",
            "in: record<user>;",
            "out: record<post>;",
            // Merely similar names are fields, not misspelled keys
            // (`schema` is four edits from `schemafull`).
            "schema: string;",
        ] {
            let table = single_table(&format!("table t {{ {head} }};"));
            assert!(
                matches!(table.nested.as_slice(), [Stmt::Field(_)]),
                "a field named by `{head}`"
            );
        }
        for (head, suggestion) in [
            ("permisions: none;", Some("permissions")),
            ("drop2;", Some("drop")),
            // The substitution and transposition arms of the one-edit
            // metric, and `field2` (one append from `field`).
            ("indax: string;", Some("index")),
            ("permissiosn: none;", Some("permissions")),
            ("field2: string;", Some("field")),
        ] {
            match first_error(&format!("table t {{ {head} }};")) {
                Diag::UnknownClause {
                    suggestion: got, ..
                } => {
                    assert_eq!(got.as_deref(), suggestion, "typo pin: {head}")
                }
                other => panic!("expected UnknownClause for `{head}`: {other:?}"),
            }
        }
        // The computed block form (other clauses alongside).
        let field = single_field(
            "field secret_shout: string {\n  on: t;\n  computed: string::uppercase(name);\n  \
             permissions {\n    select: none;\n  };\n};",
        );
        assert!(field.computed.is_some());
        assert!(field.permissions.is_some());
        // Path shapes: dotted, wildcard, quoted segments.
        for (src, expected) in [
            ("field meta.user_id: int { on: t; };", "meta.user_id"),
            ("field tags.*: string { on: t; };", "tags.*"),
            ("field matrix.*.*: int { on: t; };", "matrix.*.*"),
            (
                "field `weird name`.views: int { on: t; };",
                "weird name.views",
            ),
        ] {
            let field = single_field(src);
            assert_eq!(field_path_text(&field.name), expected, "path of: {src}");
        }
        // The kind grammar rides the header verbatim: brace-bearing
        // object kinds fold across captures, unions, literals, sizes.
        for (src, expected) in [
            (
                "field m: array<array<int>> { on: t; };",
                "array<array<int>>",
            ),
            (
                "field s: { a: string, b: number } { on: t; };",
                "{ a: string, b: number }",
            ),
            (
                "field s: 'small' | 'large' | int { on: t; };",
                "'small' | 'large' | int",
            ),
            ("field a: array<int, 5> { on: t; };", "array<int, 5>"),
            (
                "field g: geometry<point | line> { on: t; };",
                "geometry<point | line>",
            ),
            (
                "field o: option<record<user>> { on: t; };",
                "option<record<user>>",
            ),
        ] {
            let field = single_field(src);
            assert_eq!(field.kind, Some(Kind::new(expected)), "kind of: {src}");
        }
        // The nested forms: bare heads (dotted included) and the
        // `field`-prefixed escape.
        let table = single_table(
            "table t {\n  email: string {\n    default: 'x';\n  };\n  meta: object;\n  \
             meta.trusted: bool;\n  tags: array<string>;\n  tags.*: string;\n  field drop: \
             string;\n  index u { fields: email; };\n};",
        );
        let mut nested_fields = table.nested.iter().filter_map(|stmt| match stmt {
            Stmt::Field(field) => Some(field_path_text(&field.name)),
            _ => None,
        });
        assert_eq!(nested_fields.next(), Some("email".to_string()));
        assert_eq!(nested_fields.next(), Some("meta".to_string()));
        assert_eq!(nested_fields.next(), Some("meta.trusted".to_string()));
        assert_eq!(nested_fields.next(), Some("tags".to_string()));
        assert_eq!(nested_fields.next(), Some("tags.*".to_string()));
        assert_eq!(nested_fields.next(), Some("drop".to_string()));
        assert_eq!(
            nested_fields.next(),
            None,
            "the nested index is not a field"
        );
        // Folds are last-one-wins (rule 7): default/default always
        // share one slot.
        let field = single_field(
            "field n: int {\n  on: t;\n  default: 1;\n  default always: 2;\n  default: 3;\n};",
        );
        assert!(matches!(&field.default, Some(FieldDefault::Set(_))));
        let field =
            single_field("field n: int {\n  on: t;\n  default: 1;\n  default always: 2;\n};");
        assert!(matches!(&field.default, Some(FieldDefault::Always(_))));
        // Reference: the flag folds to the engine default; the block
        // carries the action; a non-enum value is the THEN expression.
        let field = single_field("field p: record<user> {\n  on: t;\n  reference;\n};");
        assert!(matches!(field.reference, Some(OnDelete::Ignore)));
        let field = single_field(
            "field p: record<user> {\n  on: t;\n  reference { on delete: cascade; };\n};",
        );
        assert!(matches!(field.reference, Some(OnDelete::Cascade)));
        let field = single_field(
            "field p: record<user> {\n  on: t;\n  reference { on delete: { create log }; };\n};",
        );
        assert!(matches!(&field.reference, Some(OnDelete::Then(_))));
        // Permissions: the shorthand and the per-op block (no delete).
        let field = single_field("field s: string {\n  on: t;\n  permissions: none;\n};");
        assert_eq!(field.permissions, Some(OpPermissions::None));
        let field = single_field(
            "field s: string {\n  on: t;\n  permissions {\n    select: none;\n    create, \
             update: $auth.admin = true;\n  };\n};",
        );
        match field.permissions {
            Some(OpPermissions::Groups(groups)) => {
                assert_eq!(groups.len(), 2);
                assert_eq!(groups[0].ops, vec![PermissionOp::Select]);
                assert_eq!(
                    groups[1].ops,
                    vec![PermissionOp::Create, PermissionOp::Update]
                );
                assert!(matches!(&groups[1].value, Permission::Expr(_)));
            }
            other => panic!("expected groups: {other:?}"),
        }
    }

    #[test]
    fn field_errors() {
        // The type requirement: only the computed field goes without.
        assert!(matches!(
            first_error("field email;"),
            Diag::FieldTypeRequired { .. }
        ));
        assert!(matches!(
            first_error("field email { on: t; };"),
            Diag::FieldTypeRequired { .. }
        ));
        // Computed exclusivity (executor-enforced, adopted eagerly).
        assert!(matches!(
            first_error("field x: int { on: t; computed: 1; value: 2; };"),
            Diag::InvalidFieldComputed { .. }
        ));
        assert!(matches!(
            first_error("field x: int { on: t; computed: 1; readonly; };"),
            Diag::InvalidFieldComputed { .. }
        ));
        // Computed must be top-level.
        assert!(matches!(
            first_error("field a.b { on: t; computed: 1; };"),
            Diag::InvalidFieldComputed { .. }
        ));
        // No subfield under a computed parent, in EITHER source order
        // (the engine checks both directions at define).
        for table_src in [
            "table t { c: int = 1; c.x: int; };",
            "table t { c.x: int; c: int = 1; };",
        ] {
            assert!(
                matches!(first_error(table_src), Diag::InvalidFieldComputed { .. }),
                "order-insensitive: {table_src}"
            );
        }
        // The InvalidKind diagnosis on an `=`-terminated candidate
        // recovers to the statement end — no spurious second
        // diagnostic from the expression that follows.
        let (_, diags) = parse_file("field x: strng = 5;\nfield ok: string { on: t; };\n");
        assert_eq!(diags.len(), 1, "one root cause: {diags:?}");
        assert!(matches!(diags[0], Diag::InvalidKind { .. }));
        // An UPPERCASE spelling of an action word is a misspelled
        // keyword (rule 9), not a THEN expression.
        match first_error(
            "field p: record<user> {\n  on: t;\n  reference { on delete: REJECT; };\n};",
        ) {
            Diag::UnknownClauseWord {
                suggestion, key, ..
            } => {
                assert_eq!(key, "on delete");
                assert_eq!(suggestion.as_deref(), Some("reject"));
            }
            other => panic!("expected UnknownClauseWord: {other:?}"),
        }
        // The permissions-shorthand rejection names its statement.
        match first_error("field s: string { on: t; permissions: $auth.admin; };") {
            Diag::InvalidPermissionsShorthand { statement, .. } => {
                assert_eq!(statement, "field");
            }
            other => panic!("expected InvalidPermissionsShorthand: {other:?}"),
        }
        // Flexible needs an object-bearing type (the runtime parser's
        // own check).
        assert!(matches!(
            first_error("field s: string { on: t; flexible; };"),
            Diag::InvalidFieldFlexible { .. }
        ));
        assert!(matches!(
            first_error("field s: record<user> { on: t; flexible; };"),
            Diag::InvalidFieldFlexible { .. }
        ));
        // ... and object-bearing unions of arrays and bare object pass.
        let field =
            single_field("field m: array<object> | { a: int } {\n  on: t;\n  flexible;\n};");
        assert!(field.flexible);
        // Reference on a nested path (the engine's define-time check).
        assert!(matches!(
            first_error("field a.b: record<user> { on: t; reference; };"),
            Diag::InvalidFieldReference { .. }
        ));
        // The `id` field's grammar.
        assert!(matches!(
            first_error("field id: string { on: t; value: 1; };"),
            Diag::InvalidFieldId { .. }
        ));
        assert!(matches!(
            first_error("field id: string { on: t; default always: 1; };"),
            Diag::InvalidFieldId { .. }
        ));
        // Plain `default` is allowed on id (`rand::guid` does not
        // exist on 3.2.4 — `rand::id` is the spelling there).
        let field = single_field("field id: record<user> {\n  on: t;\n  default: rand::id();\n};");
        assert!(matches!(&field.default, Some(FieldDefault::Set(_))));
        // Fields take no `delete` permission.
        assert!(matches!(
            first_error("field s: string { on: t; permissions { delete: none; }; };"),
            Diag::FieldDeletePermission { .. }
        ));
        // Unknown ops did-you-mean over the THREE field ops.
        match first_error("field s: string { on: t; permissions { updat: none; }; };") {
            Diag::UnknownClauseWord { suggestion, .. } => {
                assert_eq!(suggestion.as_deref(), Some("update"))
            }
            other => panic!("expected UnknownClauseWord: {other:?}"),
        }
        // The `on` machinery: required standalone, forbidden nested.
        assert!(matches!(
            first_error("field s: string;"),
            Diag::RequiredClauseMissing { .. }
        ));
        assert!(matches!(
            first_error("table t { s: string { on: t; }; };"),
            Diag::InvalidNestedOn { .. }
        ));
        // Bad kinds diagnose over the whole header candidate.
        assert!(matches!(
            first_error("field s: strng { on: t; };"),
            Diag::InvalidKind { .. }
        ));
        // Malformed paths.
        assert!(matches!(
            first_error("field a.: int { on: t; };"),
            Diag::InvalidFieldPath { .. }
        ));
        // Bare reserved words cannot be field names (rule 9).
        assert!(matches!(
            first_error("field select: string { on: t; };"),
            Diag::ReservedIdent { .. }
        ));
        // A typo'd clause key inside a table block keeps its
        // did-you-mean — it does not become a field named `permisions`.
        match first_error("table t { permisions: none; };") {
            Diag::UnknownClause { suggestion, .. } => {
                assert_eq!(suggestion.as_deref(), Some("permissions"))
            }
            other => panic!("expected UnknownClause: {other:?}"),
        }
    }

    /// The INLINE field contract (3.3; spec/field.md): the
    /// flag parses on any field, and the engine's define-time
    /// validations mirror at the fold — top-level only, the
    /// adjacency-key names, computed exclusion, and the host-relation
    /// half (nested at the table fold, standalone via L0019).
    #[test]
    fn field_inline_contract() {
        // The flag parses and codegens in the printer's fixed spot
        // (after READONLY, before VALUE).
        let file = parse_ok(
            "table person -> likes -> post {\n\
             \x20 score: number { readonly; inline; value: 1; };\n};\n",
        );
        let Stmt::Table(table) = &file.items[0] else {
            panic!()
        };
        let Stmt::Field(field) = &table.nested[0] else {
            panic!()
        };
        assert!(field.inline && field.readonly);
        let ddl = crate::codegen::emit(&file);
        assert_eq!(
            ddl.trim_end(),
            "DEFINE TABLE OVERWRITE likes TYPE RELATION IN person OUT `post` SCHEMALESS;\n\
             DEFINE FIELD OVERWRITE score ON likes TYPE number READONLY INLINE VALUE 1;"
        );
        // A dotted path cannot embed (the engine's "only a top-level
        // field can be INLINE").
        assert!(matches!(
            first_error("field a.b: number { on: t; inline; };"),
            Diag::InvalidFieldInline { .. }
        ));
        // The adjacency-key names cannot embed (the `id` spelling
        // rides the id grammar's forbidden list).
        assert!(matches!(
            first_error("field in: number { on: t; inline; };"),
            Diag::InvalidFieldInline { .. }
        ));
        assert!(matches!(
            first_error("field out: number { on: t; inline; };"),
            Diag::InvalidFieldInline { .. }
        ));
        assert!(matches!(
            first_error("field id: string { on: t; inline; };"),
            Diag::InvalidFieldId { .. }
        ));
        // Computed exclusion (the conflicts array).
        assert!(matches!(
            first_error("field c: number { on: t; inline; computed: 1 + 1; };"),
            Diag::InvalidFieldComputed { .. }
        ));
        // The host-relation half, nested form: the table fold sees the
        // host kind (the standalone form is L0019's).
        assert!(matches!(
            first_error("table t { s: number { inline; }; };"),
            Diag::InvalidTableClause { .. }
        ));
        // A relation host passes.
        let file = parse_ok(
            "table person -> likes -> post {\n\
             \x20 score: number { inline; };\n};\n",
        );
        assert_eq!(file.items.len(), 1);
        // And a field merely NAMED `inline` still parses (the block
        // clause key and the bare-head field name never collide —
        // different blocks).
        let field = single_field("field inline: string { on: t; };");
        assert!(!field.inline);
    }

    #[test]
    fn broken_field_does_not_hide_its_siblings() {
        // A broken nested field's recovery stops at the clause
        // boundary (the recovery-depth contract), the table closes,
        // and the file's siblings survive.
        let (file, diags) = parse_file("table bad { x.: int; };\nnamespace next;\n");
        assert!(
            diags
                .iter()
                .any(|d| matches!(d, Diag::InvalidFieldPath { .. })),
            "the bad path: {diags:?}"
        );
        assert_eq!(file.items.len(), 2, "siblings survive: {diags:?}");
        assert!(matches!(&file.items[0], Stmt::Table(_)));
        assert!(matches!(&file.items[1], Stmt::Namespace(_)));

        // A nested header failure (no name after the prefix) likewise.
        let (file, diags) = parse_file("table t { field { computed: 1; }; };\nnamespace next;\n");
        assert!(
            diags.iter().any(|d| matches!(d, Diag::ExpectedName { .. })),
            "the missing name: {diags:?}"
        );
        assert_eq!(file.items.len(), 2, "siblings survive: {diags:?}");
        assert!(matches!(&file.items[0], Stmt::Table(_)));
        assert!(matches!(&file.items[1], Stmt::Namespace(_)));

        // The sibling AFTER the broken field inside the same block
        // still parses.
        let (file, _) = parse_file("table t { x.: int; ok: string; };\n");
        match &file.items[0] {
            Stmt::Table(table) => {
                let fields: Vec<()> = table
                    .nested
                    .iter()
                    .filter_map(|stmt| matches!(stmt, Stmt::Field(_)).then_some(()))
                    .collect();
                assert_eq!(fields.len(), 1, "the healthy sibling survives");
            }
            other => panic!("expected the table: {other:?}"),
        }
    }

    // ---- raw + target ----

    fn single_raw(src: &str) -> Raw {
        match single_statement(src) {
            Stmt::Raw(raw) => raw,
            other => panic!("expected a raw statement: {other:?}"),
        }
    }

    /// spec rule 8: the body rides the full rule-3 skip set (the
    /// `fn`-body discipline), and only the edges trim — interior
    /// bytes, comments and indentation included, are verbatim.
    #[test]
    fn raw_captures_verbatim_and_trims_edges_only() {
        let raw = single_raw("raw {\n  // interior comment\n  DEFINE x VALUE 1;\n};");
        assert_eq!(
            raw.body, "// interior comment\n  DEFINE x VALUE 1;",
            "interior indentation survives; only the edges trim"
        );
        // A whitespace-only body trims to empty (and emits nothing).
        assert_eq!(single_raw("raw { } ;").body, "");
        // All four comment syntaxes and a quoted identifier, braces
        // and all — none of them unbalance the capture.
        let raw = single_raw("raw { /* } { */ DEFINE x VALUE 1; // }\nSELECT `}weird}`; };");
        assert_eq!(
            raw.body,
            "/* } { */ DEFINE x VALUE 1; // }\nSELECT `}weird}`;"
        );
    }

    /// The advisory contract: a body the embedded
    /// parser accepts and a body it rejects BOTH parse with zero
    /// diagnostics — validation is the linter's business.
    #[test]
    fn raw_is_advisory_never_an_error() {
        let (_, diags) = parse_file("raw { DEFINE PARAM OVERWRITE $ok VALUE 1; };");
        assert!(diags.is_empty(), "{diags:?}");
        let (_, diags) = parse_file("raw { DEFINE TABLE future TYPE LIGHTWEIGHT; };");
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// No bare form, no name: the braces ARE the grammar (rule 8).
    #[test]
    fn raw_requires_its_braces() {
        for (src, found) in [("raw;", ';'), ("raw x;", 'x'), ("raw", '\0')] {
            let expected = (found != '\0').then_some(found);
            match first_error(src) {
                Diag::RawBodyExpected { found, .. } => assert_eq!(found, expected),
                other => panic!("expected RawBodyExpected for `{src}`: {other:?}"),
            }
        }
        // Unterminated body: the scanner's capture diagnostic.
        assert!(matches!(
            first_error("raw { DEFINE x VALUE 1;"),
            Diag::UnterminatedCapture { .. }
        ));
    }

    /// A `///` block above `raw` is rejected (rule 5, like `config`):
    /// there is no COMMENT clause to hang it on, and silently
    /// dropping the user's text would be a trap. The statement still
    /// parses.
    #[test]
    fn raw_takes_no_doc_comment() {
        let (file, diags) = parse_file("/// docs\nraw { DEFINE x VALUE 1; };");
        assert!(matches!(
            diags.as_slice(),
            [Diag::DocCommentUnsupported {
                statement: "raw",
                ..
            }]
        ));
        assert!(matches!(file.items.as_slice(), [Stmt::Raw(_)]));
    }

    /// Reservation: `raw` joins the block-context reserved words
    /// (README's reserved-words list) — a bare `raw:` head in a table
    /// body cannot silently become a field. Quoting and the `field`
    /// prefix are the escapes; outside block context the word stays a
    /// legal name.
    #[test]
    fn raw_is_reserved_in_block_context() {
        let (_, diags) = parse_file("table t { raw: string; };");
        match diags.first() {
            Some(Diag::UnknownClause {
                key, suggestion, ..
            }) => {
                assert_eq!(key, "raw");
                assert_eq!(suggestion, &None);
            }
            other => panic!("expected UnknownClause for `raw`: {other:?}"),
        }
        // The escapes: a quoted head or the `field` prefix.
        let table = single_table("table t { `raw`: string; };");
        match table.nested.as_slice() {
            [Stmt::Field(field)] => {
                assert_eq!(field.name.sole_name().map(|n| n.raw.as_str()), Some("raw"))
            }
            other => panic!("expected the quoted field: {other:?}"),
        }
        let table = single_table("table t { field raw: string; };");
        match table.nested.as_slice() {
            [Stmt::Field(field)] => {
                assert_eq!(field.name.sole_name().map(|n| n.raw.as_str()), Some("raw"))
            }
            other => panic!("expected the prefixed field: {other:?}"),
        }
        // Reserved only in block context: a legal name otherwise.
        parse_ok("namespace raw;\nparam $raw = 1;");
    }

    /// The `from:` clause span rides the AST for the target diags.
    #[test]
    fn module_from_span_anchors_the_from_key() {
        let src = "module color { from: f'm:/x.surli'; unsigned; };";
        let module = single_module(src);
        assert_eq!(module.from_span.slice(src), "from");
    }

    /// The proptest asserts every failure is renderable, i.e. a
    /// well-formed diagnostic (rendering itself is the shared
    /// `test_support` helper).
    fn check_well_formed(src: &str) {
        let (_, diags) = parse_file(src);
        for diag in &diags {
            let _ = render(src, diag);
        }
    }

    /// The mutation base is a valid file exercising every landed
    /// statement shape — its validity is the mutation strategy's
    /// contract, pinned here so it cannot rot silently.
    #[test]
    fn sample_parses_clean() {
        parse_ok(SAMPLE);
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(512))]

        #[test]
        fn garbage_never_panics(input in proptest::collection::vec(any::<u8>(), 0..512)) {
            check_well_formed(&String::from_utf8_lossy(&input));
        }

        #[test]
        fn mutated_sdl_never_panics(
            mutations in proptest::collection::vec((0usize..SAMPLE.len(), any::<u8>()), 0..8),
        ) {
            let mut bytes = SAMPLE.as_bytes().to_vec();
            for (idx, byte) in mutations {
                bytes[idx] = byte;
            }
            check_well_formed(&String::from_utf8_lossy(&bytes));
        }
    }

    /// The mixin statement and its spread entries (spec/mixin.md):
    /// the clean shape (statement, members, spread position), the
    /// SDL-E1061–E1065 block, the `///` rejection on the statement,
    /// and the identifier coexistence (`mixin` names nothing else —
    /// a field named `mixin` parses bare).
    #[test]
    fn mixin_statement_and_spreads() {
        // The clean shape: members are nested-form fields (empty `on`
        // placeholder), the spread rides the table's nested list at
        // its position.
        let (file, diags) = parse_file(
            "mixin stamp {\n  a: int;\n  b: string { default: 'x'; };\n};\n\ntable t \
                        schemafull {\n  ...stamp;\n  x: int;\n};\n",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            matches!(&file.items[0], Stmt::Mixin(m) if m.name.raw == "stamp"
            && m.members.len() == 2)
        );
        let Stmt::Table(table) = &file.items[1] else {
            panic!("the table follows the mixin");
        };
        assert_eq!(table.nested.len(), 2, "{:?}", table.nested);
        assert!(matches!(&table.nested[0], Stmt::Spread(s) if s.name.raw == "stamp"));
        // An empty mixin collapses to the bare form.
        let (file, diags) = parse_file("mixin base;");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(matches!(&file.items[0], Stmt::Mixin(m) if m.members.is_empty()));

        // SDL-E1061: an unknown reference — the resolution pass's
        // finding (parse carries the entry opaque), with a did-you-mean
        // over the document's mixins, and without one when the
        // document has no mixins to name.
        let (file, diags) = parse_file("mixin stamp { a: int; };\ntable t { ...stmp; };");
        assert!(diags.is_empty(), "the spread parses opaque: {diags:?}");
        match resolve_spreads(&file).first().expect("E1061") {
            Diag::UnknownMixin {
                name, suggestion, ..
            } => {
                assert_eq!(name, "stmp");
                assert_eq!(suggestion.as_deref(), Some("stamp"));
            }
            other => panic!("expected UnknownMixin: {other:?}"),
        }
        let (file, diags) = parse_file("table t { ...nope; };");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(matches!(
            resolve_spreads(&file).first(),
            Some(Diag::UnknownMixin { .. })
        ));
        // The declarative namespace: a mixin defined BELOW the table
        // resolves like any other — position is not resolution's
        // business.
        let (file, diags) = parse_file("table t { ...late; };\nmixin late { a: int; };");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(resolve_spreads(&file).is_empty(), "forward is legal");

        // SDL-E1062: a duplicate mixin name (parse-time, one file).
        assert!(matches!(
            first_error("mixin a { x: int; };\nmixin a { y: int; };"),
            Diag::DuplicateDefinition {
                family: "mixin",
                ..
            }
        ));

        // Collisions resolve last-occurrence-wins (spec/mixin.md's
        // amendment — SDL-E1063 retired): two spreads defining one
        // member PARSE CLEAN, the later spread's member wholly wins
        // at expansion (SDL-L0022 advises at lint), and an explicit
        // member decides position-independently.
        let collide = "mixin a { x: int; y: int; };\nmixin b { x: string; z: int; };\n\
                       table t { ...a; ...b; };";
        let (file, diags) = parse_file(collide);
        assert!(diags.is_empty(), "last-wins, not an error: {diags:?}");
        assert!(matches!(&file.items[2], Stmt::Table(t) if t.nested.len() == 2));
        let (file, diags) = parse_file(
            "mixin a { x: int; y: int; };\nmixin b { x: string; z: int; };\n\
             table t { x: bool; ...a; ...b; };",
        );
        assert!(diags.is_empty(), "the explicit field decides: {diags:?}");
        // The explicit `x` overshadows BOTH mixins' `x` members
        // (position-independent): the table keeps x + a's y + b's z.
        assert!(matches!(&file.items[2], Stmt::Table(t) if t.nested.len() == 3));

        // SDL-E1064: the same mixin twice.
        assert!(matches!(
            first_error("mixin a { x: int; };\ntable t { ...a; ...a; };"),
            Diag::DuplicateSpread { .. }
        ));

        // SDL-E1065: a spread in any other block.
        for src in [
            "table view v { select * from x; ...a; };",
            "mixin a { ...b; };",
            "field f: int { on: t; ...m; };",
        ] {
            assert!(
                matches!(first_error(src), Diag::SpreadMisplaced { .. }),
                "{src}"
            );
        }

        // `///` is rejected on the statement (no COMMENT target — the
        // sequence/config precedent) and on members (the nested-field
        // rule), but a mixin still parses under an authored `//` note.
        assert!(matches!(
            first_error("/// doc\nmixin a { x: int; };"),
            Diag::DocCommentUnsupported { statement, .. } if statement == "mixin"
        ));
        assert!(matches!(
            first_error("mixin a { /// doc\n x: int; };"),
            Diag::UnknownClause { .. }
        ));
        let (file, diags) = parse_file("// note\nmixin a { x: int; };");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            matches!(&file.items[0], Stmt::Mixin(m) if m.comments == vec![" note".to_string()])
        );

        // `on:` is the forbidden spelling inside a mixin body (the
        // host is whichever table spreads it).
        assert!(matches!(
            first_error("mixin a { x: int { on: t; }; };"),
            Diag::InvalidNestedOn { .. }
        ));

        // `mixin` is not a SurrealQL keyword and reserves nothing: a
        // field named `mixin` parses bare, and a mixin named `mixin`
        // is legal.
        let (file, diags) = parse_file("mixin mixin { mixin: int; };\ntable t { ...mixin; };");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(matches!(&file.items[0], Stmt::Mixin(m) if m.name.raw == "mixin"));

        // The INLINE host mirror at the resolution pass (spec/field.md
        //'s rule, anchored at the spread entry with the mixin named):
        // a non-relation host rejects an INLINE member, an explicit
        // override of that member sidesteps it (the loser never
        // lands).
        let (file, diags) = parse_file("mixin m { f: int { inline; }; };\ntable t { ...m; };");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(matches!(
            resolve_spreads(&file).first(),
            Some(Diag::InvalidTableClause { .. })
        ));
        let (file, diags) =
            parse_file("mixin m { f: int { inline; }; };\ntable t { f: int; ...m; };");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            resolve_spreads(&file).is_empty(),
            "the override shadows the INLINE member"
        );
        assert!(matches!(&file.items[1], Stmt::Table(t) if t.nested.len() == 2));

        // A LIGHTWEIGHT relation carries no fields — a spread counts
        // (its members are fields).
        assert!(matches!(
            first_error("mixin m { f: int; };\ntable a -> e -> b lightweight { ...m; };"),
            Diag::InvalidTableClause { .. }
        ));

        // The cross-entry passes see only EXPANDED members — an
        // overshadowed member never lands (the computed pass's filter,
        // the INLINE mirror's rule): a computed member replaced by an
        // explicit field is not a computed parent, and its dotted
        // sibling's diagnostic fires once, anchored on the table's own
        // field.
        let (file, diags) =
            parse_file("mixin m { c = 1; };\ntable t schemafull { c: string; c.x: int; ...m; };");
        assert!(
            diags.is_empty(),
            "the explicit `c` replaces the member: {diags:?}"
        );
        assert!(matches!(&file.items[1], Stmt::Table(t) if t.nested.len() == 3));
        let (_, diags) =
            parse_file("mixin m { c.x: int; };\ntable t schemafull { c = 1; c.x: int; ...m; };");
        assert_eq!(
            diags.len(),
            1,
            "one diagnostic, the table's own c.x: {diags:?}"
        );
    }

    /// A mixin body is a table block's body minus `on:` clauses
    /// (spec/mixin.md): index and event members parse with their
    /// full clause grammar, hold the empty `on` placeholder, and
    /// reject the explicit `on:` spelling; their kind-local
    /// validations (an index's `fields:`, an event's `then:`) fire at
    /// the member's own parse, host-independent; a bare field head
    /// named `index`/`event` needs the `field` prefix or quoting
    /// (the keys are active in this body, the table block's rule).
    #[test]
    fn mixin_index_and_event_members() {
        let (file, diags) = parse_file(
            "mixin indexed {\n  slug: string;\n  index slug_u { fields: slug; };\n  \
             event touched { when: slug != ''; then: 1; };\n};",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let Stmt::Mixin(mixin) = &file.items[0] else {
            panic!()
        };
        let [Stmt::Field(field), Stmt::Index(index), Stmt::Event(event)] = mixin.members.as_slice()
        else {
            panic!("three members: {:?}", mixin.members)
        };
        // The empty `on` placeholder every member family holds —
        // expansion overwrites it with the spreading table.
        assert_eq!(field.on.raw, "");
        assert_eq!(index.on.raw, "");
        assert_eq!(event.on.raw, "");

        // `on:` is the forbidden spelling on every member family (the
        // host is whichever table spreads the mixin).
        for src in [
            "mixin m { index i { on: t; fields: a; }; };",
            "mixin m { event e { on: t; then: 1; }; };",
        ] {
            assert!(
                matches!(first_error(src), Diag::InvalidNestedOn { statement, .. } if statement == "index" || statement == "event"),
                "{src}"
            );
        }

        // Kind-local validations fire at the member's own parse.
        assert!(matches!(
            first_error("mixin m { index i { }; };"),
            Diag::RequiredClauseMissing {
                clause: "fields",
                ..
            }
        ));
        assert!(matches!(
            first_error("mixin m { event e { }; };"),
            Diag::RequiredClauseMissing { clause: "then", .. }
        ));

        // A bare head named `index`/`event` routes to the member head
        // grammar (the keys are active) — the same ExpectedName a
        // table block's bare `index: int;` yields (the parity the
        // spec promises); the `field` prefix and quoting are the
        // escapes, the table block's rule.
        assert!(matches!(
            first_error("mixin m { index: int; };"),
            Diag::ExpectedName { .. }
        ));
        assert!(matches!(
            first_error("table t { index: int; };"),
            Diag::ExpectedName { .. }
        ));
        assert!(matches!(
            first_error("mixin m { event: int; };"),
            Diag::ExpectedName { .. }
        ));
        let (file, diags) =
            parse_file("mixin m { field index: int; `event`: string; index n { fields: a; }; };");
        assert!(diags.is_empty(), "{diags:?}");
        let Stmt::Mixin(mixin) = &file.items[0] else {
            panic!()
        };
        assert!(
            matches!(&mixin.members[0], Stmt::Field(f) if f.name.sole_name().map(|n| &n.raw) == Some(&"index".to_string()))
        );
        assert!(
            matches!(&mixin.members[1], Stmt::Field(f) if f.name.sole_name().map(|n| &n.raw) == Some(&"event".to_string()))
        );
        // The typo guard: a near-miss of an active key is the unknown
        // clause, not a field head.
        match first_error("mixin m { indx: int; };") {
            Diag::UnknownClause {
                key, suggestion, ..
            } => {
                assert_eq!(key, "indx");
                assert_eq!(suggestion.as_deref(), Some("index"));
            }
            other => panic!("expected UnknownClause: {other:?}"),
        }

        // The LIGHTWEIGHT rejection rides the spread entry itself (a
        // member of any kind never lands on a lightweight host).
        match first_error(
            "mixin m { index i { fields: a; }; };\n\
             table a -> likes -> b lightweight { ...m; };",
        ) {
            Diag::InvalidTableClause { message, .. } => {
                assert!(
                    message.contains("a spread (a mixin's members)"),
                    "{message}"
                )
            }
            other => panic!("expected InvalidTableClause: {other:?}"),
        }

        // The resolution pass sees only LANDED members: a member
        // beaten by a LATER spread's same-keyed member never reaches
        // the INLINE mirror (last-wins, spec/mixin.md's amendment).
        let (file, diags) = parse_file(
            "mixin a { f: int { inline; }; };\n\
             mixin b { f: int; };\n\
             table t { ...a; ...b; };",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            resolve_spreads(&file).is_empty(),
            "b's f wins, never INLINE here"
        );
        // ...and the reverse order still rejects: a's INLINE f wins.
        let (file, diags) = parse_file(
            "mixin a { f: int { inline; }; };\n\
             mixin b { f: int; };\n\
             table t { ...b; ...a; };",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(matches!(
            resolve_spreads(&file).first(),
            Some(Diag::InvalidTableClause { .. })
        ));

        // The computed rule holds ACROSS the two passes: an explicit
        // subfield under a SPREAD-contributed computed parent is the
        // same hard error as the hand-written pair (the parse's pool
        // is explicit-only; the resolution pass owes the explicit
        // fields the spread roots).
        let (file, diags) = parse_file("mixin m { x = 1; };\ntable t { ...m; x.y: int; };");
        assert!(diags.is_empty(), "{diags:?}");
        match resolve_spreads(&file).first().expect("E1049") {
            Diag::InvalidFieldComputed { message, .. } => {
                assert!(
                    message.contains("`x` is computed") && message.contains("`x.y`"),
                    "{message}"
                )
            }
            other => panic!("expected InvalidFieldComputed: {other:?}"),
        }
        // The explicit twin keeps its parse-time diagnostic, and an
        // explicit override of the computed member silences the
        // spread leg (the loser never joins the pool).
        assert!(matches!(
            first_error("table t { x = 1; x.y: int; };"),
            Diag::InvalidFieldComputed { .. }
        ));
        let (file, diags) =
            parse_file("mixin m { x = 1; };\ntable t { ...m; x: string; x.y: int; };");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            resolve_spreads(&file).is_empty(),
            "the explicit x overshadows the computed member"
        );
        // The pool's other two directions: a spread member under an
        // EXPLICIT computed parent (the member leg reads the full
        // pool), and a cross-mixin pair (a computed member in one
        // mixin, its subfield member in another, one table spreads
        // both).
        let (file, diags) = parse_file("table t { x = 1; ...m; };\nmixin m { x.y: int; };");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(matches!(
            resolve_spreads(&file).first(),
            Some(Diag::InvalidFieldComputed { .. })
        ));
        let (file, diags) =
            parse_file("mixin a { x = 1; };\nmixin b { x.y: int; };\ntable t { ...a; ...b; };");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(matches!(
            resolve_spreads(&file).first(),
            Some(Diag::InvalidFieldComputed { .. })
        ));
    }

    /// SDL-E1067 (issue #38's promotion): the same member key twice
    /// within ONE body — a mixin's or a table's — rejects at parse;
    /// the diagnostic anchors the SECOND occurrence and names the
    /// kind. Per family (a field `x`, an index `x`, an event `x`
    /// never collide with one another), and per body (the same key
    /// in two different mixins, or a mixin member colliding with a
    /// spread-carried key, is the COLLISION machinery's business —
    /// last-wins there, not this error).
    #[test]
    fn duplicate_member_keys_within_one_body_reject() {
        for (src, kind, key) in [
            ("mixin m { a: int; b: string; a: bool; };", "field", "a"),
            ("table t { x: int; x: string; };", "field", "x"),
            (
                "table t { f: int; index i { fields: f; }; index i { fields: f; }; };",
                "index",
                "i",
            ),
            (
                "table t { event e { then: 1; }; event e { then: 2; }; };",
                "event",
                "e",
            ),
        ] {
            match first_error(src) {
                Diag::DuplicateMemberKey {
                    kind: found_kind,
                    key: found_key,
                    ..
                } => {
                    assert_eq!(found_kind, kind, "{src}");
                    assert_eq!(found_key, key, "{src}");
                }
                other => panic!("expected DuplicateMemberKey for {src}: {other:?}"),
            }
        }
        // The prefixed and bare field spellings are one key; a dotted
        // path keys by its full text.
        assert!(matches!(
            first_error("mixin m { m.x: int; field m.x: string; };"),
            Diag::DuplicateMemberKey { .. }
        ));
        // Per body and per family: none of these reject.
        for src in [
            "mixin a { x: int; };\nmixin b { x: string; };\ntable t { ...a; ...b; };",
            "mixin a { x: int; };\ntable t { x: bool; ...a; };",
            "mixin m { x: int; index x { fields: x; }; event x { then: 1; }; };\n\
             table t { ...m; };",
        ] {
            let (_, diags) = parse_file(src);
            assert!(diags.is_empty(), "{src}: {diags:?}");
        }
    }

    // ---- include (spec/include.md) ----

    fn parse_composable_ok(src: &str) -> SdlFile {
        let (file, diags) = parse_file_composable(src);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        file
    }

    fn first_error_composable(src: &str) -> Diag {
        let (_, diags) = parse_file_composable(src);
        assert!(!diags.is_empty(), "expected a diagnostic: {src}");
        diags.into_iter().next().expect("checked above")
    }

    fn include_of(src: &str) -> Include {
        let file = parse_composable_ok(src);
        assert_eq!(file.items.len(), 1, "src: {src}");
        match file.items.into_iter().next().expect("len checked") {
            Stmt::Include(include) => include,
            other => panic!("expected an include: {other:?}"),
        }
    }

    #[test]
    fn include_parses_in_the_composable_entry_only() {
        // The composable entry keeps the statement, either quote style,
        // with escapes decoded and the authored path carried.
        assert_eq!(include_of("include \"a/b.surdl\";").path, "a/b.surdl");
        assert_eq!(include_of("include 'a b.surdl';").path, "a b.surdl");
        assert_eq!(include_of(r#"include "a\tb.surdl";"#).path, "a\tb.surdl");
        // The strict entry rejects with exactly one diagnostic and
        // drops the statement — emit never sees it.
        let (file, diags) = parse_file("include \"a.surdl\";\n");
        assert!(matches!(
            diags.as_slice(),
            [Diag::IncludeUnsupported { .. }]
        ));
        assert!(file.items.is_empty());
        // A malformed literal is a parse error in the composable entry
        // (the strict entry's rejection comes first there).
        assert!(matches!(
            first_error_composable("include a.surdl;"),
            Diag::InvalidString { .. }
        ));
        assert!(matches!(
            first_error_composable("include \"unterminated;"),
            Diag::InvalidString { .. }
        ));
        // No doc comment (no DDL COMMENT target — rule 5, like mixin).
        assert!(matches!(
            first_error_composable("/// docs\ninclude \"a.surdl\";"),
            Diag::DocCommentUnsupported { .. }
        ));
    }

    #[test]
    fn include_position_rules() {
        // An include is legal ANYWHERE in the file (the declarative
        // namespace: it declares participation, not an elaboration
        // point) — after definitions, mid-body, anywhere.
        for src in [
            "table t { };\ninclude \"a.surdl\";",
            "namespace acme;\ndatabase db;\ntable t { };\ninclude \"a.surdl\";",
            "table t { };\ninclude \"a.surdl\";\ntable u { };",
        ] {
            parse_composable_ok(src);
        }
        // Scope statements after an include are the one order rule
        // left (E1069's remaining half — the scope law); the alias
        // spellings name their canonical statement.
        for (src, statement) in [
            ("include \"a.surdl\";\nnamespace acme;", "namespace"),
            ("include \"a.surdl\";\nns acme;", "namespace"),
            ("include \"a.surdl\";\ndatabase shop;", "database"),
            ("include \"a.surdl\";\ndb shop;", "database"),
        ] {
            match first_error_composable(src) {
                Diag::IncludeMisplaced {
                    statement: found, ..
                } => {
                    assert_eq!(found, statement, "{src}");
                }
                other => panic!("expected IncludeMisplaced for {src}: {other:?}"),
            }
        }
        // The legal layout: preamble, includes, body — and the body
        // may carry includes of its own.
        parse_composable_ok(
            "namespace acme;\ndatabase shop;\ninclude \"a.surdl\";\n\
             include \"b.surdl\";\ntable t { };\n",
        );
    }

    #[test]
    fn include_anchors_ride_the_statement() {
        // Leading comments bind into the include's group; a same-line
        // note after the terminator is the close note (rule 5). The
        // pending buffer carries the text after `//` (trailing
        // whitespace trimmed) — the writer does the indenting.
        let include = include_of("// head note\ninclude \"a.surdl\"; // tail note\n");
        assert_eq!(include.comments, vec![" head note".to_string()]);
        assert_eq!(
            include.clause_comments,
            vec![ClauseComment {
                path: String::new(),
                before: Vec::new(),
                after: vec![" tail note".to_string()],
            }]
        );
        // A comment between the head and the literal is legal input —
        // the collecting trivia skip anchors it with the leading
        // comments instead of dropping it.
        let include = include_of("include // between\n\"a.surdl\";");
        assert_eq!(include.comments, vec![" between".to_string()]);
    }

    #[test]
    fn include_compares_by_path_only() {
        // The compare contract strips comments/spans; the path IS the
        // definition.
        let a = parse_composable_ok("// note\ninclude \"a.surdl\"; // note\n");
        let b = parse_composable_ok("include \"a.surdl\";");
        assert!(a.same_definition(&b));
        let c = parse_composable_ok("include \"b.surdl\";");
        assert!(!a.same_definition(&c));
    }

    #[test]
    fn include_survives_fmt_byte_stably() {
        // The canonical spelling re-quotes the carried path through
        // the shared string quoter; the statement is a fixed point
        // like any other.
        let src = "namespace acme;\n\ninclude 'tables/users.surdl';\n\ntable t { };\n";
        let file = parse_composable_ok(src);
        let canonical = crate::info::write::write_canonical(&file);
        assert_eq!(
            canonical,
            "namespace acme;\n\ninclude 'tables/users.surdl';\n\ntable t;\n"
        );
        let (file2, diags2) = parse_file_composable(&canonical);
        assert!(diags2.is_empty(), "{diags2:?}");
        assert_eq!(crate::info::write::write_canonical(&file2), canonical);
        // same_definition holds across the quote-style spelling.
        assert!(file.same_definition(&file2));
    }
}
