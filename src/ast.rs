//! SDL's own statement AST — one type per DEFINE statement.
//!
//! Parsed by [`crate::parser`], consumed by [`crate::codegen`].
//! Expression clauses hold the embedded printer's canonical text
//! (computed at parse time by [`crate::expr`]: a wrapper-statement
//! parse through the official parser, then a `ToSql` slice — the
//! embedded AST itself is unextractable
//! through the 3.2.4 public API). The AST grows statement by
//! statement: it is never a big-bang 16-statement
//! structure, and nothing lands here without a parser and codegen
//! that exercise it.

use crate::lexer::Span;

/// A parsed SDL file: its definitions in source order (rule 7 — SDL
/// is order-dependent and codegen preserves that order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdlFile {
    pub items: Vec<Stmt>,
    /// Plain `//` lines after the last definition (emitted at end of
    /// file by the writer).
    pub trailing_comments: Vec<Comment>,
}

/// One top-level definition. Variants accumulate per unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Namespace(Namespace),
    Sequence(Sequence),
    Param(Param),
    Database(Database),
    Bucket(Bucket),
    User(User),
    Analyzer(Analyzer),
    Event(Event),
    Module(Module),
    Function(Function),
    Config(Config),
    Index(Index),
    Api(Api),
    Access(Access),
    Table(Table),
    Field(Field),
    Mixin(Mixin),
    /// `...<name>;` — a spread entry, produced only inside a non-view
    /// [`Table::nested`] (spec/mixin.md).
    Spread(Spread),
    Raw(Raw),
    /// `include "<path>";` (spec/include.md) — file composition: the
    /// loader splices the included file's statements in at this
    /// statement's position, so codegen never sees one. Only the
    /// composable parse entry produces it; the strict entry rejects
    /// the statement (SDL-E1068).
    Include(Include),
}

/// `raw { … };` (README rule 8) — the escape hatch: SurrealQL DDL
/// captured verbatim and emitted back verbatim, for legal shapes
/// SDL's surface cannot express (legacy `VS` scoring data, future
/// statements). The body is the trimmed capture interior: leading and
/// trailing whitespace stripped, every interior byte — comments,
/// spellings, strings, inner indentation — verbatim. Validation is
/// advisory only: a body the embedded parser rejects is still carried
/// and emitted (rule 8 exists for exactly that class); the linter
/// flags it. Top-level placement only; `raw`
/// is a reserved word in every block context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raw {
    pub body: String,
    /// Plain `//` lines captured directly above the statement (the
    /// body's own interior rides verbatim inside [`Self::body`]).
    pub comments: Vec<Comment>,
    /// Same-line note after the statement's terminator (rule 5) —
    /// [`ClauseComment`]; `raw` has no clauses, so the empty path is
    /// this field's only resident.
    pub clause_comments: Vec<ClauseComment>,
}

/// `namespace <name>;` (spec/namespace.md) — the simplest statement: a
/// name plus an optional doc comment. The block form
/// `namespace <name> { };` is equivalent (a non-empty block is a
/// parse error — `namespace` defines no clauses). The `ns` keyword
/// alias (rule 9) rides [`Carried`]: fmt keeps the authored word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace {
    pub name: Ident,
    pub keyword: Carried,
    pub comment: Option<DocComment>,
    /// Plain `//` comment lines captured directly above the statement
    /// (rule 5's layout preservation — re-emitted by the file writer,
    /// never part of the definition).
    pub comments: Vec<Comment>,
    /// Same-line note after the statement's terminator (rule 5) —
    /// [`ClauseComment`]; `namespace` defines no clauses, so the
    /// empty path is this field's only resident.
    pub clause_comments: Vec<ClauseComment>,
}

/// An identifier: the unescaped interior spelling (bare word, or the
/// inside of `` `…` `` / `⟨…⟩`). Codegen emits it bare when it is a
/// plain non-keyword word, backtick-escaped otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub raw: String,
}

impl Ident {
    pub fn new(raw: impl Into<String>) -> Self {
        Ident { raw: raw.into() }
    }
}

/// A `///` doc-comment block (rule 5): one string per line, with the
/// leading space and trailing whitespace already stripped. Codegen
/// joins the lines with `\n` into the definition's `COMMENT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocComment {
    pub lines: Vec<String>,
}

impl DocComment {
    /// The `COMMENT` value: lines joined with `\n`.
    pub fn joined(&self) -> String {
        self.lines.join("\n")
    }
}

/// A preserved plain `//` comment line (rule 5's layout-preservation
/// half): the raw text after the `//` marker, trailing whitespace
/// trimmed, everything else verbatim — the writer re-emits `//` plus
/// this text above the anchor it was captured above. Equality is
/// definition equality: the compare normalizer strips comment fields,
/// so a comment never changes what a definition IS (the differ and
/// the fmt fixed point compare anchor-free trees; only the writer's
/// output shows the comments).
pub type Comment = String;

/// One clause's comment anchors (rule 5's clause-level half): the
/// `//` lines captured directly above the clause (`before`) and the
/// same-line note captured after the clause's terminator (`after` —
/// the writer re-emits it on the clause's own line, so re-capture
/// lands back on the same anchor and fmt stays a fixed point).
/// `path` locates the clause inside the statement: a clause name
/// (`"value"`), a dot-joined sub-block path (`"duration.token"`), or
/// `permissions.#<n>` for the per-operation permissions group at
/// emitted index `<n>`. The empty path `""` is the statement's own
/// final line and carries only `after` — a same-line note after the
/// statement's terminator; the statement's leading lines live in its
/// `comments`, and anchorless interior lines in its
/// `trailing_comments`. Like every comment field this is layout,
/// never definition: the compare normalizer strips it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClauseComment {
    pub path: String,
    pub before: Vec<Comment>,
    pub after: Vec<Comment>,
}

/// A value together with the authored SDL spelling that produced it
/// when that spelling was an input-only alias (rule 9 / codegen-model
/// item 4): `on: kv;` stays `kv` through fmt instead of churning to
/// `root`, while emitted DDL prints the canonical full word either
/// way. `spelling` is `None` when the authored word was already
/// canonical. Equality is VALUE equality — the carried spelling never
/// participates (`on: kv;` and `on: root;` hold the
/// same definition; the differ compares canonical values only) —
/// the writer's output is the one place the spelling shows.
#[derive(Debug, Clone, Copy)]
pub struct Authored<T> {
    pub value: T,
    pub spelling: Option<&'static str>,
}

impl<T> Authored<T> {
    /// The canonical spelling — the parser's shape for a word that
    /// was not an alias.
    pub fn canonical(value: T) -> Self {
        Authored {
            value,
            spelling: None,
        }
    }

    /// An aliased authored spelling.
    pub fn aliased(value: T, spelling: &'static str) -> Self {
        Authored {
            value,
            spelling: Some(spelling),
        }
    }
}

impl<T: PartialEq> PartialEq for Authored<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq> Eq for Authored<T> {}

/// Authored-spelling provenance for a class with no separate value to
/// carry: the `ns`/`db` statement-keyword aliases (`ns acme;`) and the
/// index clause-key alias `columns:` ≡ `fields:`. `Some` holds the
/// authored word; `None` means the canonical spelling. ALWAYS equal —
/// provenance, never semantics (only the writer's output shows the
/// difference; equality and the differ see identical definitions).
#[derive(Debug, Clone, Default)]
pub struct Carried(pub Option<&'static str>);

impl PartialEq for Carried {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Carried {}

/// An expression clause: the embedded official printer's canonical
/// text for the capture (see [`crate::expr`]). Equality is canonical
/// equality — two spellings of one expression (`1 + 1` and `1  +/*a*/
/// 1`) hold the same text, so `PartialEq` never distinguishes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr {
    pub canon: String,
}

/// A single-value permission clause (spec/README.md, permission form
/// 1): `none`, `full`, or an expression — the shape `param` (and, in
/// the next unit, `bucket`) uses. `Option` at the statement site
/// distinguishes an absent clause (rule 9: SurrealQL's default, no
/// emission) from an explicit `full`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permission {
    /// `permissions: none;`
    None,
    /// `permissions: full;` (SurrealQL's default when the clause is
    /// absent).
    Full,
    /// `permissions: <expr>;` — codegen emits `PERMISSIONS WHERE`.
    Expr(Expr),
}

/// `sequence <name> = <start>;` / `sequence <name> { … };`
/// (spec/sequence.md). Absent clauses are SurrealQL's defaults
/// (`start` 0, `batch` 1000, no `timeout`) and elide at emission;
/// there is no `COMMENT` clause, so `///` is rejected on this
/// statement (rule 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sequence {
    pub name: Ident,
    pub start: Option<Expr>,
    pub batch: Option<Expr>,
    pub timeout: Option<Expr>,
    /// Plain `//` lines above the statement, and lines inside the
    /// block form that anchored to no clause (emitted before the
    /// block's close).
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// A `$`-prefixed parameter name: the unescaped interior without the
/// `$` (mirrors [`Ident`]; the `$` is added at emission, and unlike
/// [`Ident`] a keyword-colliding name emits bare — upstream prints
/// `$table` unquoted, probe-verified).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamName {
    pub raw: String,
}

impl ParamName {
    pub fn new(raw: impl Into<String>) -> Self {
        ParamName { raw: raw.into() }
    }
}

/// `param $<name> = <expr>;` / `param $<name> { … };`
/// (spec/param.md). A param without `value` stores `VALUE NONE`
/// (SurrealQL parity — accepted here, flagged by the spec's linter
/// note); `permissions` is `None` when the clause is absent
/// (SurrealQL then defaults to `FULL`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: ParamName,
    pub value: Option<Expr>,
    pub permissions: Option<Permission>,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; lines inside the block
    /// that anchored to no clause ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// `database <name> { … };` (spec/database.md). `strict` stays in the
/// AST even though the server's INFO round-trip drops it while it is
/// active — the normalizer preserves SDL-meaningful clauses
/// INFO cannot report (spec/database.md, "STRICT decoded").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Database {
    pub name: Ident,
    /// The `db` keyword alias (rule 9) — see [`Namespace.keyword`].
    pub keyword: Carried,
    pub strict: bool,
    pub changefeed: Option<ChangeFeed>,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// `changefeed { expiry: <duration>; include original; };` — the
/// colon-less sub-block (rule 2). `expiry` is required inside the
/// block; the duration compares by value (`90m` ≡ `1h30m`), never by
/// spelling, so it stores the parsed duration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeFeed {
    pub expiry: surrealdb_types::Duration,
    pub include_original: bool,
}

/// `bucket <name> { … };` (spec/bucket.md). Absent `permissions` is
/// SurrealQL's FULL default and elides at emission (rule 9); an
/// explicit `permissions: full;` emits `PERMISSIONS FULL`.
/// Capability-gated at define time by the server's `files` feature —
/// SDL accepts the statement regardless (spec/README.md rule 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bucket {
    pub name: Ident,
    pub backend: Option<Expr>,
    pub readonly: bool,
    pub permissions: Option<Permission>,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// The `ON <level>` of a `user` definition (spec/user.md). Aliases are
/// input-only (rule 9): `kv`/`ns`/`db` parse to the canonical variants,
/// and codegen emits the full word (`on: kv;` → `ON ROOT`). The
/// authored alias spelling rides [`Authored`] for the writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Root,
    Namespace,
    Database,
}

/// A user credential (spec/user.md): at most one, parser-enforced.
/// The string is the DECODED literal value — comparisons and emission
/// go through the value, never the source spelling (like durations).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pass {
    /// `password: '…';` — plaintext (the server hashes at define time).
    Password(String),
    /// `passhash: '…';` — a pre-computed argon2 hash.
    Hash(String),
}

/// A user role (spec/user.md): SurrealQL's hardcoded enum. Codegen
/// emits the uppercased spelling (`ROLES OWNER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Owner,
    Editor,
    Viewer,
}

/// `user <name> { … };` (spec/user.md). `on` is required (upstream
/// rejects a user without `ON`); a file whose `user` lacks it is
/// diagnosed and the statement dropped. Absent `roles` is SurrealQL's
/// `VIEWER` default and elides at emission; absent durations elide
/// (the server's `1h`/`none` defaults apply) — a `duration` block
/// with neither side present folds to `None`. The codegen model's
/// sole `OVERWRITE` exception rides the secret clauses: a secret-less
/// user is define-once (`IF NOT EXISTS`), a secret-carrying one
/// emits `OVERWRITE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub name: Ident,
    pub on: Authored<Level>,
    pub pass: Option<Pass>,
    /// `passscram: '…';` — a pre-computed SCRAM-SHA-256 verifier (the
    /// 3.3 grammar). ADDITIVE with `pass`: upstream stores it in its
    /// own field and an explicit verifier wins over the
    /// password-derived one. Shape-validated at parse (the engine
    /// validates at DDL parse — `SDL-E1059`). Secret protocol: like
    /// `pass`, never round-trips (INFO prints `PASSSCRAM '[REDACTED]'`;
    /// the sentinel is define-rejected, so the reader strips it); a
    /// scram-carrying user is secret-carrying for codegen (`OVERWRITE`).
    pub scram: Option<String>,
    pub roles: Vec<Role>,
    pub token_duration: Option<Expr>,
    pub session_duration: Option<Expr>,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// An SDL-lexed name path — `fn::<seg>[::<seg>…]` or
/// `mod::<seg>[::<seg>…]` (spec/README.md, `<fn-path>` notation):
/// never a call (trailing parens rejected). Segments may be
/// keyword-like words bare, or quoted identifiers (unescaped like any
/// quoted ident). `mod::` is capability-gated by the server's
/// surrealism feature at define time; SDL accepts it regardless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnPath {
    pub kind: FnKind,
    pub segments: Vec<String>,
}

/// The `<fn-path>` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FnKind {
    /// `fn::` — a user-defined function.
    Fn,
    /// `mod::` — a Surrealism module function.
    Mod,
    /// No prefix — a bare name path. Only api `middleware:` heads take
    /// one (README `<fn-path>`: middleware names take the prefix bare
    /// or omitted); analyzer `function:` and graphql function lists
    /// reject it at their parse sites.
    Bare,
}

/// An analyzer tokenizer (spec/analyzer.md). Codegen emits the
/// uppercased spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tokenizer {
    Blank,
    Camel,
    Class,
    Punct,
}

/// A snowball stemming language (spec/analyzer.md): SurrealQL's
/// hardcoded enum of 18. Input spellings (full names and the lexer's
/// 2/3-letter aliases) canonicalize to a variant at parse time;
/// codegen emits the full name uppercased.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Arabic,
    Danish,
    Dutch,
    English,
    Finnish,
    French,
    German,
    Greek,
    Hungarian,
    Italian,
    Norwegian,
    Portuguese,
    Romanian,
    Russian,
    Spanish,
    Swedish,
    Tamil,
    Turkish,
}

impl Language {
    /// The canonical full name, uppercased (`ENGLISH`) — the server's
    /// own `Language::as_str` spelling, which is what its emitter
    /// prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Arabic => "ARABIC",
            Language::Danish => "DANISH",
            Language::Dutch => "DUTCH",
            Language::English => "ENGLISH",
            Language::Finnish => "FINNISH",
            Language::French => "FRENCH",
            Language::German => "GERMAN",
            Language::Greek => "GREEK",
            Language::Hungarian => "HUNGARIAN",
            Language::Italian => "ITALIAN",
            Language::Norwegian => "NORWEGIAN",
            Language::Portuguese => "PORTUGUESE",
            Language::Romanian => "ROMANIAN",
            Language::Russian => "RUSSIAN",
            Language::Spanish => "SPANISH",
            Language::Swedish => "SWEDISH",
            Language::Tamil => "TAMIL",
            Language::Turkish => "TURKISH",
        }
    }
}

/// An analyzer filter (spec/analyzer.md). The parameterized atoms
/// carry parsed values (u16 pairs, the language variant, the decoded
/// mapper path) — value, never spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    Ascii,
    Lowercase,
    Uppercase,
    /// `edgengram(<min>, <max>)` — SurrealQL does not validate
    /// `min ≤ max` (it silently degrades); SDL accepts it per parity.
    EdgeNgram(u16, u16),
    /// `ngram(<min>, <max>)`.
    Ngram(u16, u16),
    /// `snowball(<language>)` — the language is a bare keyword; the
    /// authored alias spelling (`snowball(eng)`) rides [`Authored`]
    /// for the writer.
    Snowball(Authored<Language>),
    /// `mapper('<path>')` — a string literal whose path must appear in
    /// the runtime file allowlist for actual use.
    Mapper(String),
}

/// `analyzer <name> { … };` (spec/analyzer.md) — all clauses optional
/// (an analyzer with none is accepted, though inert). Clause order is
/// free (rule 7); a repeated list clause replaces the whole list
/// (last-one-wins, SurrealQL parity). Codegen emits FUNCTION →
/// TOKENIZERS → FILTERS → COMMENT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analyzer {
    pub name: Ident,
    pub function: Option<FnPath>,
    pub tokenizers: Option<Vec<Tokenizer>>,
    pub filters: Option<Vec<Filter>>,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// The parameters of `event`'s `async` dependent group (spec/README,
/// "Dependent clause groups"): the bare `async;` flag and the
/// `async { retry: …; maxdepth: …; };` block both land here — the
/// flag as both sides absent (SurrealQL's 1/3 defaults apply), the
/// block carrying only the sides written (rule 9: absent sides elide
/// at emission). Both are u16, upstream's exact type — unclamped
/// beyond it (the docs' 0–16 `maxdepth` range is folklore).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Async {
    pub retry: Option<u16>,
    pub maxdepth: Option<u16>,
}

/// `event <name> { … };` (spec/event.md), standalone form — nesting
/// into `table` blocks (where `on` is implied) is the parser's
/// business. `on` and
/// `then` are required (upstream rejects an event without either);
/// both are validated at parse time with their own diagnostics. An
/// absent `when` stores `None` and emits nothing (rule 9 — SurrealQL
/// materializes `WHEN true` internally; the adopt/elide decision is
/// the normalizer's, not codegen). `then` is SurrealQL's
/// `THEN a, b` comma list (always ≥ 1: parse-enforced). Clause order
/// is free (rule 7); codegen emits SurrealQL's canonical ASYNC →
/// WHEN → THEN → COMMENT. A repeat of `async` (flag or block) folds
/// last-one-wins like any clause (rule 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub name: Ident,
    pub on: Ident,
    pub when: Option<Expr>,
    pub then: Vec<Expr>,
    pub r#async: Option<Async>,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`]. Comments inside the
    /// `then` capture (SurrealQL-verbatim) are not SDL structure and
    /// do not survive (rule 5's anchor model).
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// `module [<name>] { … };` (spec/module.md) — the grammar is the 3.3
/// module form (`unsigned;` required, `FROM … UNSIGNED` emission), the
/// one spelling SDL emits. `from` and `unsigned` are
/// required (parse-enforced); the name–executable coupling is too — a
/// file executable requires the header name (3.2.4 rejects storing
/// anonymous file modules — verified), a silo executable is
/// self-named and rejects one. Absent `permissions` is SurrealQL's
/// FULL default and elides at emission (rule 9). Statement-level
/// capability-gated by the server's surrealism feature at define
/// time; SDL accepts it regardless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub name: Option<Ident>,
    pub executable: Executable,
    pub unsigned: bool,
    pub permissions: Option<Permission>,
    pub comment: Option<DocComment>,
    /// The `from:` clause key's span — the statement's source-fidelity
    /// anchor. Comparisons zero it (`same_definition`, the differ's
    /// span-blind convention).
    pub from_span: Span,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// `module`'s executable — exactly one, carried by the required
/// `from:` clause (spec/module.md). Both arms are rule-3 keyword
/// clauses: SDL-lexed literals, never expression captures. Values,
/// never spellings: escapes are decoded, the file string is split per
/// upstream's file-string grammar, the version is a parsed u32
/// triple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Executable {
    /// `from: f'bucket:/key';` — a surrealism file string (f' or f").
    /// The key always begins `/` (upstream's lexer guarantees it).
    File { bucket: String, key: String },
    /// `from: silo::<org>::<pkg>::<maj.min.patch>;` — a
    /// Silo-published package, self-named (the header name is
    /// rejected with it). SDL's spelling is the documented 3.3 form;
    /// the 3.2 `<maj.min.patch>` shape belongs to the reverse
    /// parser.
    Silo {
        org: String,
        pkg: String,
        major: u32,
        minor: u32,
        patch: u32,
    },
}

/// A SurrealQL kind (README rule 4): the trimmed source spelling,
/// SDL-lexed and structure-validated by the kind parser, then carried
/// verbatim — codegen never re-spells it (the embedded printer's own
/// kind normalization, `option<T>` → `none | T`, is deliberately NOT
/// adopted: the source spelling re-parses identically, and the
/// differ compares parsed kind ASTs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kind {
    pub text: String,
}

impl Kind {
    pub fn new(text: impl Into<String>) -> Self {
        Kind { text: text.into() }
    }
}

/// `function`'s `graphql { … }` sub-options (spec/function.md): the
/// two GRAPHQL_* clauses, each independently optional. Both absent —
/// an empty block — stores `None` (nothing to emit).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Graphql {
    pub alias: Option<String>,
    pub deprecated: Option<String>,
}

/// `fn <path>([$<param>: <kind>, …]) [-> <kind>] { <body> } [{ … }];`
/// (spec/function.md) — the one two-block statement: the SurrealQL
/// body block, then an optional ordinary SDL clause block. The body
/// stores the embedded printer's canonical block text (braces
/// included — [`crate::expr`] slices the whole `{ … }` print; an
/// empty or comment-only body canonizes to upstream's `{;}`). Params
/// keep the `$` in the source sense — [`ParamName`] strips it like
/// everywhere else, and codegen re-adds it (upstream stores names
/// without the `$` and re-adds at display). `permissions` is `None`
/// when the clause is absent (SurrealQL's FULL default elides, rule
/// 9). SDL's trailing-clause order (PERMISSIONS → GRAPHQL_* →
/// COMMENT) diverges from the server's ToSql order (body → COMMENT →
/// GRAPHQL_* → PERMISSIONS) — both parse; the differ compares
/// ASTs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub name: FnPath,
    pub args: Vec<(ParamName, Kind)>,
    pub returns: Option<Kind>,
    pub body: Expr,
    pub permissions: Option<Permission>,
    pub graphql: Option<Graphql>,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the trailing clause block ride [`Self::trailing_comments`].
    /// Comments inside the body capture (SurrealQL-verbatim) are not
    /// SDL structure and do not survive (rule 5's anchor model).
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// `config <variant> { … };` (spec/config.md) — the one statement
/// whose header carries a variant instead of a name (README rule 1:
/// the variant is the header; exactly one of `api | graphql |
/// default`). Configs are per-variant singletons, so codegen always
/// emits `DEFINE CONFIG OVERWRITE …`, and `config` has NO `COMMENT`
/// clause in SurrealQL — `///` is rejected on all three variants
/// (rule 5, like `sequence`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Config {
    /// `config api { … };` — global api-endpoint defaults. Absent
    /// `permissions` elides at emission (rule 9: SurrealQL's parse
    /// default FULL applies).
    Api(ApiConfig),
    /// `config graphql { … };` — exposure families and limits, fully
    /// resolved at parse (defaults: both families [`Family::None`],
    /// [`Introspection::Auto`]); clause folds are last-one-wins per
    /// slot, the bare `auto;`/`none;` flags writing both families.
    Graphql(GraphqlConfig),
    /// `config default { … };` — the ROOT-level default ns/db. At
    /// least one side is required (parse-enforced); the unset side
    /// emits `NONE` (SurrealQL prints both sides unconditionally).
    Default(DefaultConfig),
}

/// `config api`'s clauses (spec/config.md). The middleware list is
/// never empty when present (upstream demands a name immediately).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiConfig {
    pub middleware: Vec<Middleware>,
    pub permissions: Option<Permission>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// One `middleware:` entry: a name path — bare, `fn::`, or `mod::`
/// (`silo::`/`ml::` heads are outside SDL's surface; `raw` is the
/// escape hatch) — plus its argument expressions, evaluated once at
/// define time. `()` parses to zero arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Middleware {
    pub path: FnPath,
    pub args: Vec<Expr>,
}

/// `config graphql`'s clauses (spec/config.md): each slot resolved —
/// repeated clauses and the bare flags fold last-one-wins at parse
/// (rule 7), so the AST never holds a fold history. `depth` and
/// `complexity` are u32, upstream's exact types.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphqlConfig {
    pub tables: Family<Ident>,
    pub functions: Family<FnPath>,
    pub depth: Option<u32>,
    pub complexity: Option<u32>,
    pub introspection: Introspection,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// A graphql exposure family (spec/config.md): `auto | none`, or the
/// structured form — an include/exclude list with at least one entry
/// (parse-enforced, like upstream's list loops).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Family<T> {
    #[default]
    None,
    Auto,
    Include(Vec<T>),
    Exclude(Vec<T>),
}

/// Whether graphql schema introspection is enabled
/// (spec/config.md): `auto` is SurrealQL's default and elides at
/// emission; `none` prints `INTROSPECTION NONE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Introspection {
    #[default]
    Auto,
    None,
}

/// `config default`'s clauses (spec/config.md): each side
/// independently optional (≥ 1 required, parse-enforced); absent
/// sides emit `NONE`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DefaultConfig {
    pub namespace: Option<Expr>,
    pub database: Option<Expr>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// A float scalar (rule 3's keyword-clause kind): the value, never
/// the spelling — `1.30` and `1.3` hold the same [`f64`]. Equality is
/// float equality; it is a safe [`Eq`] on this type's domain because
/// the grammar produces finite values only (no sign, no `NaN`/`inf`
/// spellings, magnitudes under 2⁶³ — see the parser's `float_arg`),
/// so reflexivity holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Float(pub f64);

impl Eq for Float {}

/// `bm25: (k1, b)` — f32, upstream's `Scoring::Bm` storage (the INFO
/// reprint rounds through it). Same [`Eq`] reasoning as [`Float`]:
/// the grammar's unsigned bounded decimals cast to f32 without
/// reaching infinity, so reflexivity holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bm25(pub f32, pub f32);

impl Eq for Bm25 {}

/// A vector distance function (spec/index.md): SurrealQL's 10-kind
/// set. Codegen emits the uppercased spelling; `minkowski`'s order
/// rides the variant — SDL spells it `minkowski(3)`, codegen the bare
/// `MINKOWSKI 3` form (spec/index.md, "Emit"). The order stores as a
/// float value; an integer spelling therefore emits integer-shaped
/// (`minkowski(3)` → `MINKOWSKI 3` — spec's example, and the same
/// value server-side either way).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distance {
    Euclidean,
    Cosine,
    CosineNormalized,
    Chebyshev,
    Hamming,
    Jaccard,
    Pearson,
    InnerProduct,
    Manhattan,
    Minkowski(Float),
}

impl Distance {
    /// The `DISTANCE` value spelling, uppercased per the server
    /// emitter (`Distance`'s own `ToSql`).
    pub fn as_str(self) -> &'static str {
        match self {
            Distance::Euclidean => "EUCLIDEAN",
            Distance::Cosine => "COSINE",
            Distance::CosineNormalized => "COSINE_NORMALIZED",
            Distance::Chebyshev => "CHEBYSHEV",
            Distance::Hamming => "HAMMING",
            Distance::Jaccard => "JACCARD",
            Distance::Pearson => "PEARSON",
            Distance::InnerProduct => "INNER_PRODUCT",
            Distance::Manhattan => "MANHATTAN",
            Distance::Minkowski(_) => "MINKOWSKI",
        }
    }
}

/// A vector element type (spec/index.md): SurrealQL's 8-kind set,
/// uppercased spellings (`VectorType`'s own `ToSql`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorType {
    F64,
    F32,
    F16,
    I64,
    I32,
    I16,
    I8,
    U8,
}

impl VectorType {
    pub fn as_str(self) -> &'static str {
        match self {
            VectorType::F64 => "F64",
            VectorType::F32 => "F32",
            VectorType::F16 => "F16",
            VectorType::I64 => "I64",
            VectorType::I32 => "I32",
            VectorType::I16 => "I16",
            VectorType::I8 => "I8",
            VectorType::U8 => "U8",
        }
    }
}

/// `index`'s kind — the header variant slot's payload (README rule
/// 10's enum for INDEX). One arm per SDL variant plus the omitted-slot
/// default (rule 6: defaults are expressed by omission, so plain is
/// the empty arm); each variant carries only its own clauses, with
/// the absent-clause default (`Option`/`false`) eliding at emission
/// (rule 9 — server-side defaults like `ANALYZER like` materialize in
/// INFO, which is the normalizer's business).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexKind {
    /// No variant word: a plain (non-unique) index.
    Plain,
    /// `index unique <name> { … };`
    Unique,
    /// `index count <name> { … };` — a filtered count takes `where:`.
    Count {
        /// `where: <expr>;` — absent emits bare `COUNT`.
        where_: Option<Expr>,
    },
    /// `index fulltext <name> { … };`
    FullText {
        /// `analyzer: <ident>;` — absent emits bare `FULLTEXT` (the
        /// server materializes `ANALYZER like`).
        analyzer: Option<Ident>,
        /// `bm25: (k1, b);` — absent emits bare `FULLTEXT` (the
        /// server defaults and reprints `BM25(1.2,0.75)`).
        bm25: Option<Bm25>,
        /// `highlights;`
        highlights: bool,
    },
    /// `index hnsw <name> { … };` — `dimension` required (parse-
    /// enforced); the numeric params carry upstream's exact types
    /// (`m`/`m0` u8 — `m ≤ 127` is fold-site validation, upstream's
    /// own post-parse bail; `efc` u16; `lm`/order floats).
    Hnsw {
        dimension: u16,
        /// `distance: <distance>;` — absent emits the server default
        /// `EUCLIDEAN`.
        distance: Option<Distance>,
        /// `type: <vector-type>;` — absent emits the server default
        /// `F32`.
        vector_type: Option<VectorType>,
        /// u8 ≤ 127 (validated).
        m: Option<u16>,
        /// u8 (validated).
        m0: Option<u16>,
        lm: Option<Float>,
        efc: Option<u16>,
        extend_candidates: bool,
        keep_pruned_connections: bool,
        hashed_vector: bool,
    },
    /// `index diskann <name> { … };` — the same shapes as HNSW with
    /// DISKANN's own parameters; the restricted distance/type domains
    /// and `degree`/`l_build` > 0 are fold-site validation (upstream
    /// rejects post-parse).
    DiskAnn {
        dimension: u16,
        distance: Option<Distance>,
        vector_type: Option<VectorType>,
        degree: Option<u16>,
        l_build: Option<u16>,
        alpha: Option<Float>,
        hashed_vector: bool,
    },
}

/// `index [unique | count | fulltext | hnsw | diskann] <name> { … };`
/// (spec/index.md) — standalone form only here: `on` is always
/// required (nesting into `table` blocks, where it is implied, lands
/// in the table block — event's precedent). `fields` is required per
/// kind and arity-validated at parse (rule 10); count forbids it (its
/// clause table omits both spellings). Clause order is free (rule 7);
/// codegen emits the server's canonical order NAME → ON → FIELDS →
/// kind → COMMENT → CONCURRENTLY. `concurrently` is a transient build
/// directive — the server never reports it in INFO (spec/index.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub name: Ident,
    pub kind: IndexKind,
    pub on: Ident,
    pub fields: Vec<Expr>,
    /// The clause key as authored: `columns:` when the file spelled
    /// the alias (`fields:` is canonical) — see [`Carried`].
    pub fields_key: Carried,
    pub concurrently: bool,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// `api '<path>' { … };` (spec/api.md) — the one structural-sub-block
/// statement (README rule 2's designated case): the body holds zero
/// or more method groups and at most one `any` block, in any order,
/// whose heads are not clause keys. The path is the decoded string
/// literal's value — SurrealDB's route-path grammar
/// (static/`:param`/`*wildcard` segments) is the server's to validate
/// at define time; SDL passes the literal through. A body with
/// neither groups nor an `any` is legal (upstream parses the
/// zero-`FOR` statement — probe-pinned); codegen emits `any` first
/// (canonical, matching the emitter), then groups in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Api {
    pub path: String,
    pub actions: Vec<ApiAction>,
    pub any: Option<ApiAnyBlock>,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// One method-group sub-block: a non-empty comma list of methods
/// (each method may appear in only ONE group — parse-enforced; upstream
/// errors only at define time) sharing one config and one required
/// `then` action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiAction {
    pub methods: Vec<ApiMethod>,
    pub config: ApiGroupConfig,
    pub action: Expr,
    /// Plain `//` lines above the sub-block; anchorless lines inside
    /// it ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// The endpoint-wide `any { … }` block: the same config as a group
/// plus the optional fallback `then` (its absence — a bare `any { };`
/// — emits bare `FOR ANY`, which upstream re-materializes with the
/// default `PERMISSIONS FULL` on the INFO side; the normalizer's
/// concern).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiAnyBlock {
    pub config: ApiGroupConfig,
    /// Plain `//` lines above the sub-block; anchorless lines inside
    /// it ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
    pub fallback: Option<Expr>,
}

/// The clause pair every api sub-block shares (spec/api.md's
/// `ApiConfig`, named for the groups that carry it — the config
/// statement's payload owns the [`ApiConfig`] name). Both sides
/// optional: absent `permissions` elides at emission (rule 9 —
/// SurrealQL's parse default FULL applies), and an absent middleware
/// list emits nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApiGroupConfig {
    pub middleware: Vec<Middleware>,
    pub permissions: Option<Permission>,
}

/// An HTTP method word (spec/api.md's six-kind set). Codegen emits the
/// lowercase spelling (`FOR get` — the server emitter's own shape;
/// case-insensitive input, canonical lowercase per rule 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Trace,
}

impl ApiMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            ApiMethod::Get => "get",
            ApiMethod::Post => "post",
            ApiMethod::Put => "put",
            ApiMethod::Patch => "patch",
            ApiMethod::Delete => "delete",
            ApiMethod::Trace => "trace",
        }
    }
}

/// `access <name> { … };` (spec/access.md) — the discriminant-in-block
/// statement (README rule 10's enum for ACCESS): `type:` selects the
/// variant from inside the block, so the clause vocabulary is a union
/// table at parse and the variant owns its membership (a clause
/// another variant defines diagnoses, not silently folds). `on` is
/// required (upstream `expected ON`); the record variant and the
/// bearer-for-record subject are database-level only (upstream
/// parser-enforced — validated eagerly here, index's precedent).
/// Clause order is free (rule 7); codegen emits the server's
/// canonical order TYPE → SIGNUP/SIGNIN → WITH REFRESH → WITH JWT →
/// AUTHENTICATE → DURATION → COMMENT. Access has a COMMENT clause, so
/// `///` is accepted (rule 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    pub name: Ident,
    pub on: Authored<Level>,
    pub kind: AccessKind,
    /// `duration { … };` — absent block, or one with no sides written,
    /// folds to `None` (rule 9: SurrealQL's grant-30d/token-1h/
    /// session-none defaults apply and elide at emission; the server
    /// re-materializes the clause in INFO — the normalizer's business).
    pub duration: Option<AccessDuration>,
    /// `authenticate: <expr>;` — the post-auth check.
    pub authenticate: Option<Expr>,
    /// `context: <expr>;` — the authorization payload that rides
    /// `$session.data` (3.3). The engine demands a read-only
    /// expression (semantic — the engine's to say, the standing
    /// division of labor) and an access method that authenticates a
    /// record (mirrored at the fold: record or bearer-for-record at
    /// the database level; the variant half rides the membership
    /// pass). Unevaluated at define — INFO re-prints the expression.
    pub context: Option<Expr>,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// ACCESS's kind — `type:`'s payload (spec/access.md's enum). Each
/// variant carries only its own clauses; the JWT verification/issuing
/// source is shared shape ([`JwtSource`]) reached three ways: the jwt
/// variant's direct clauses, and the `jwt { … }` sub-block of record
/// and bearer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessKind {
    /// `type: jwt;` — verify (and optionally issue) arbitrary JWTs.
    Jwt {
        /// The verification source — required (`url:` XOR
        /// `algorithm:`+`key:`, parse-fold validated).
        source: JwtSource,
    },
    /// `type: record;` — database-level only (upstream
    /// parser-enforced). signup/signin are grammar-optional
    /// (SurrealQL accepts neither).
    Record {
        signup: Option<Expr>,
        signin: Option<Expr>,
        /// The `jwt { … }` sub-block — absent means SurrealQL
        /// auto-issues with a random HS512 key at define time; SDL
        /// emits nothing for it (spec/access.md: issuance is
        /// SDL-managed-only).
        jwt: Option<JwtSource>,
        /// `refresh;` — refresh-token grants (WITH REFRESH).
        refresh: bool,
    },
    /// `type: bearer;` — `for:` is required (upstream `expected FOR`).
    Bearer {
        subject: BearerSubject,
        /// The `jwt { … }` sub-block — emitted even though INFO never
        /// renders it (spec/access.md, verified).
        jwt: Option<JwtSource>,
    },
}

/// A JWT verification + issuing configuration (spec/access.md): the
/// required verification source plus the optional `issuer { … }`
/// block. The same shape is the jwt variant's direct clauses and
/// record/bearer's `jwt { … }` payload — "same shape and source
/// requirement" per the spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwtSource {
    pub verify: JwtVerify,
    /// `audience: '<value>', …;` — accepted `aud` claim values
    /// (3.3): when present, verification requires the claim and an
    /// intersection with this list. SDL authors string literals —
    /// the only shape that round-trips (the engine string-checks
    /// each value at define and INFO prints the stored strings).
    pub audience: Option<Vec<String>>,
    /// `issuer { … };` — the issuing configuration. Absent means
    /// SurrealQL's own defaults: a symmetric (hs*) local verify key
    /// mirrors into an implicit issuer (and the SDL emitted nothing —
    /// minimal-form, rule 6); anything else cannot issue.
    pub issuer: Option<JwtIssuer>,
}

/// The verification source — exactly one of the two shapes
/// (XOR-validated at the fold site: upstream's grammar is positional
/// `ALGORITHM <alg> KEY <expr>` or `URL <expr>`, nothing else).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtVerify {
    /// `url: <expr>;` — a JWKS endpoint.
    Url(Expr),
    /// `algorithm:` + `key:` — a local verification key.
    Key { algorithm: Algorithm, key: Expr },
}

/// `issuer { … }` — the issuing configuration. `algorithm` stores the
/// declared word when one was written; at emission it prints only on
/// the URL-verified path (by local key the parser forces it equal to
/// the verification algorithm — elided as forced-equal, and the
/// server's own ToSql never prints it). `key` is `None` only in the
/// symmetric-mirrored case — validation requires it otherwise (a
/// key-less issuer materializes a RANDOM key; SDL rejects, per
/// spec/access.md), and codegen re-derives the mirror from the verify
/// key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwtIssuer {
    pub algorithm: Option<Algorithm>,
    pub key: Option<Expr>,
}

/// A bearer access's subject (spec/access.md's `for:` set) — `record`
/// is database-level only (upstream parser-enforced).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearerSubject {
    User,
    Record,
}

/// A JWT algorithm (v3.2.4 `Algorithm`'s 13-kind set). Codegen emits
/// the uppercased spelling; `is_symmetric` drives the issuer rules
/// (mirrored key, forced-equal algorithm) — upstream's own
/// `Algorithm::is_symmetric` predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    EdDsa,
    Es256,
    Es384,
    Es512,
    Hs256,
    Hs384,
    Hs512,
    Ps256,
    Ps384,
    Ps512,
    Rs256,
    Rs384,
    Rs512,
}

impl Algorithm {
    /// The server emitter's spelling (`Algorithm`'s own `ToSql`).
    pub fn as_str(self) -> &'static str {
        match self {
            Algorithm::EdDsa => "EDDSA",
            Algorithm::Es256 => "ES256",
            Algorithm::Es384 => "ES384",
            Algorithm::Es512 => "ES512",
            Algorithm::Hs256 => "HS256",
            Algorithm::Hs384 => "HS384",
            Algorithm::Hs512 => "HS512",
            Algorithm::Ps256 => "PS256",
            Algorithm::Ps384 => "PS384",
            Algorithm::Ps512 => "PS512",
            Algorithm::Rs256 => "RS256",
            Algorithm::Rs384 => "RS384",
            Algorithm::Rs512 => "RS512",
        }
    }

    /// The HS* family — the algorithms whose verify key mirrors into
    /// an implicit issuer (upstream `is_symmetric`).
    pub fn is_symmetric(self) -> bool {
        matches!(self, Algorithm::Hs256 | Algorithm::Hs384 | Algorithm::Hs512)
    }
}

/// `access`'s `duration { … };` payload (spec/access.md): each side
/// independently optional; a block with no sides present folds to an
/// absent `duration` (like `user`'s). The sides are expressions
/// upstream (`AccessDuration`'s fields are `Expr`), so each carries
/// the embedded printer's canonical text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccessDuration {
    pub grant: Option<Expr>,
    pub token: Option<Expr>,
    pub session: Option<Expr>,
}

/// `table`'s kind — the header discriminant (README rule 10's enum for
/// TABLE; spec/table.md). Unlike `access`, the variant rides the
/// **header**: the variant words (`any`, `relation`, `view`) are
/// variant words only when followed by a name, and the arrow spelling
/// IS the relation variant. Every kind is explicit at emit (rule 9's
/// licensed deviations): a bare table emits `TYPE NORMAL`
/// (fail-closed — it rejects `RELATE`, unlike SurrealQL's
/// bare-schemaless→`TYPE ANY` materialization) and a view always
/// emits `TYPE ANY SCHEMALESS AS SELECT …`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableKind {
    /// `table any <name> { … };` — the explicit escape hatch for
    /// tables that must accept `RELATE` without being relations.
    Any,
    /// `table <name> { … };` — the bare form; emits `TYPE NORMAL`.
    Normal,
    /// `table [<from> | …] -> <name> -> [<to> | …] { … };` or the
    /// canonical `table relation <name>;` spelling (both sides
    /// unnamed). A side is `None` exactly when unnamed — `Some` never
    /// wraps an empty vec (the forward and reverse parsers must
    /// agree; the differ compares ASTs). `enforced;` adds the
    /// endpoint-existence check on both endpoints, unconditionally.
    /// `lightweight` is the 3.3 record-less edge form (spec/table.md):
    /// it requires both sides named and implies `enforced` — the AST
    /// folds the implication at parse (the engine's define-time
    /// normalization, probe-pinned), so `lightweight: true` never
    /// coexists with `enforced: false` and downstream consumers
    /// (differ, normalizer) need no default-equivalence arm.
    Relation {
        from: Option<Vec<Ident>>,
        to: Option<Vec<Ident>>,
        enforced: bool,
        lightweight: bool,
    },
    /// `table view <name> { <select-query>; … };` — the block IS the
    /// query (a restricted select, validated by the embedded parser);
    /// read-only. Emission is fixed: `TYPE ANY SCHEMALESS AS SELECT …`.
    View { query: Expr },
}

impl TableKind {
    /// Whether the kind is the view — the codegen arm uses it to
    /// keep the view's fixed emission single-shaped (`TYPE ANY
    /// SCHEMALESS`, no schema flag or changefeed).
    pub fn is_view(&self) -> bool {
        matches!(self, TableKind::View { .. })
    }
}

/// The per-operation `permissions` clause shared by `table` and
/// `field` (spec/table.md, spec/field.md): the single-value `none |
/// full` shorthand, or the per-operation block. The per-operation
/// form's unlisted operations are NONE for tables (the parser seeds
/// deny) and FULL for fields (the parser seeds allow — the opposite;
/// both are the engine's fold, never SDL emission, since groups emit
/// as written). The single-value `<expr>` form is NOT valid on either
/// (their parser accepts only `NONE | FULL | FOR`) — rejected at
/// parse. A field's groups never hold the `delete` operation
/// (parse-rejected: SurrealQL rejects it for fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpPermissions {
    /// `permissions: none;` — all operations.
    None,
    /// `permissions: full;` — all operations.
    Full,
    /// `permissions { select: …; create, update: …; };` — source-
    /// ordered comma groups, preserved as written (a repeated
    /// operation's later group wins server-side; the differ compares
    /// resolved ASTs).
    Groups(Vec<PermissionGroup>),
}

/// One `FOR` group of the per-operation form: the comma-grouped
/// operations (source order, duplicates ride through — SurrealQL
/// parity) and the one value all of them take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionGroup {
    pub ops: Vec<PermissionOp>,
    pub value: Permission,
}

/// A per-operation permission's operation (the four-op set; fields
/// reject `delete` at parse). Codegen emits the lowercase spelling
/// (`FOR select FULL` — the server emitter's own shape, probe-pinned
/// through the embedded engine; keywords re-parse case-insensitively).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionOp {
    Select,
    Create,
    Update,
    Delete,
}

impl PermissionOp {
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionOp::Select => "select",
            PermissionOp::Create => "create",
            PermissionOp::Update => "update",
            PermissionOp::Delete => "delete",
        }
    }
}

/// `table <name> { … };` (spec/table.md) — the nesting statement:
/// fields, indexes, and events are table-scoped and nest inside the
/// block omitting their `on:` clause (standalone forms keep it —
/// exactly one of the two applies). `schema_full` is `None` when
/// neither flag was written and still emits `SCHEMALESS` — the schema
/// flag is ALWAYS emitted (README rule 9's licensed deviation,
/// keeping emitted DDL byte-stable against INFO, which materializes
/// it). Absent `permissions` elides (SurrealQL's NONE default; the
/// security-adjacent warning is the linter's). Codegen emits the
/// SDL canonical order `TYPE → DROP → SCHEMAFULL|SCHEMALESS →
/// CHANGEFEED → AS SELECT → PERMISSIONS → GRAPHQL_* → COMMENT`, then
/// nested definitions in source order (rule 7 — referenced fields
/// before the indexes that use them; SurrealDB validates at define
/// time). The server's own ToSql order differs (COMMENT/GRAPHQL
/// before AS/CHANGEFEED, INLINE EDGES|REFERENCES after COMMENT/GRAPHQL
/// but before AS/CHANGEFEED, permission groups merged by equal value
/// with delete-Full elided) — both parse; the differ compares ASTs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub name: Ident,
    pub kind: TableKind,
    /// `schemafull;` / `schemaless;` — fold last-one-wins (never an
    /// error: upstream's clause loop silently takes the last
    /// spelling, header and block spellings alike); `None` = neither
    /// written (emits `SCHEMALESS`).
    pub schema_full: Option<bool>,
    /// `drop;` — the server silently skips storing/indexing/
    /// edge-linking records written to this table.
    pub drop: bool,
    pub changefeed: Option<ChangeFeed>,
    /// `inline edges: <n>;` — the 3.3 inline cache cap (0–256; the
    /// engine's u32 domain narrowed at parse to the u16 the bound
    /// fits in). Prints only when written (`INLINE EDGES <n>`); the
    /// clause is kind-independent (accepted on every table kind, the
    /// engine probes) — only its >256 bound is a parse error.
    pub inline_edges: Option<u16>,
    /// `inline references: <n>;` — the references cap, same shape.
    pub inline_references: Option<u16>,
    pub permissions: Option<OpPermissions>,
    pub graphql: Option<Graphql>,
    /// The nested `index`/`event`/`field` definitions in source order
    /// (`field` nesting spec/table.md's flat dotted-path model — no
    /// sub-block sugar for subfields).
    pub nested: Vec<Stmt>,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// A field name path (spec/field.md): SurrealQL's flat path model —
/// `.`-joined segments with `.*` the array wildcard (`stats.views`,
/// `tags.*`, `matrix.*.*`). The first segment is always a name;
/// wildcard segments ride any later position. Each name segment is an
/// identifier in any of SurrealQL's three forms (bare reserved words
/// rejected, rule 9); the `field` prefix is the escape for names that
/// collide with the enclosing statement's clause keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldPath {
    pub segments: Vec<FieldPathSeg>,
}

impl FieldPath {
    /// Whether the path is exactly one name segment — the computed
    /// and `reference` clauses demand this shape.
    pub fn is_top_level(&self) -> bool {
        matches!(self.segments.as_slice(), [FieldPathSeg::Name(_)])
    }

    /// The path's flat text — `.`-joined segments, `*` the wildcard
    /// (`stats.views`, `tags.*`): one home for the collision/override
    /// comparisons (the parser's folds, the merge's classify key, the
    /// mixin expansion's overshadow rule).
    pub(crate) fn to_text(&self) -> String {
        self.segments
            .iter()
            .map(|segment| match segment {
                FieldPathSeg::Name(name) => name.raw.clone(),
                FieldPathSeg::Wildcard => "*".to_string(),
            })
            .collect::<Vec<_>>()
            .join(".")
    }

    /// The single name segment, when the path is exactly one name —
    /// the shape of the `id` field's name.
    pub fn sole_name(&self) -> Option<&Ident> {
        match self.segments.as_slice() {
            [FieldPathSeg::Name(name)] => Some(name),
            _ => None,
        }
    }
}

/// One `.`-separated segment of a [`FieldPath`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldPathSeg {
    /// A named segment (`stats`, `views`).
    Name(Ident),
    /// The array wildcard (`*` — `tags.*`).
    Wildcard,
}

/// A field's `default` (spec/field.md): `default:` and
/// `default always:` share one slot, last-one-wins — the spelled form
/// is what emits (`DEFAULT` when a value is applied only in the
/// record's absence, `DEFAULT ALWAYS` when it always is).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldDefault {
    /// `default: <expr>;`
    Set(Expr),
    /// `default always: <expr>;`
    Always(Expr),
}

/// A field's `reference` clause (spec/field.md): the referential
/// action on the referenced record's delete. The bare `reference;`
/// flag and an explicit `on delete: ignore;` both store
/// [`OnDelete::Ignore`] — the default, elided at emission (bare
/// `REFERENCE` prints for every ignore spelling, rule 6). `reference`
/// is legal on top-level record/array-of-record-shaped fields only
/// (nested paths are rejected at parse — the engine's define-time
/// check, adopted eagerly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnDelete {
    /// `on delete: reject;`
    Reject,
    /// `on delete: cascade;`
    Cascade,
    /// `reference;` or `on delete: ignore;` — the engine default.
    Ignore,
    /// `on delete: unset;`
    Unset,
    /// `on delete: <expr>;` — a non-enum value runs as
    /// `ON DELETE THEN <expr>`.
    Then(Expr),
}

/// `field` definitions (spec/field.md) — standalone (`on:` required)
/// and nested inside a `table` block (the host table IS the `on`; an
/// explicit `on:` there is the forbidden spelling). The type sits in
/// the header (`name: type`), SDL-lexed per README rule 4, and is
/// required for regular fields — a licensed deviation (rule 9; the
/// typeless form is reachable via `raw { … };`) — optional exactly
/// when a computed expression is present (`name = <expr>;`, the
/// typeless computed SurrealQL accepts). The clause vocabulary is
/// SurrealQL's own, free-ordered (rule 7), folds last-one-wins;
/// `computed` is mutually exclusive with `default`, `value`, `assert`,
/// `reference`, and `readonly` (executor-enforced upstream, validated
/// eagerly here) and demands a top-level path. Codegen emits the SDL
/// canonical order `TYPE [FLEXIBLE] → READONLY → INLINE → VALUE →
/// ASSERT → DEFAULT [ALWAYS] → COMPUTED → REFERENCE [ON DELETE …] →
/// PERMISSIONS → GRAPHQL_* → COMMENT`; the server's ToSql order
/// differs (DEFAULT before ASSERT, COMMENT/GRAPHQL near the end,
/// PERMISSIONS printed unconditionally) — both parse; the differ
/// compares ASTs. Absent clauses elide (rule 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: FieldPath,
    /// The header type — `None` only for computed fields.
    pub kind: Option<Kind>,
    /// `on: <table>;` — standalone only (nested fields inherit their
    /// host table).
    pub on: Ident,
    pub default: Option<FieldDefault>,
    pub value: Option<Expr>,
    pub assert: Option<Expr>,
    /// `computed: <expr>;` — the block form of the header assignment.
    pub computed: Option<Expr>,
    pub readonly: bool,
    /// `flexible;` — the type must contain an object (validated at
    /// parse; the engine additionally demands a SCHEMAFULL table at
    /// define time).
    pub flexible: bool,
    /// `inline;` — the field's value embeds into the host relation's
    /// edge-adjacency payloads (3.3). The engine demands an existing
    /// TYPE RELATION host (mirrored at the table fold for the nested
    /// form; the standalone form is the linter's L0019), forbids the
    /// `id`/`in`/`out` names and computed fields, and caps the count
    /// at 255 per table (L0020). No kind constraint — every kind
    /// embeds (probe-pinned 2026-09-25).
    pub inline: bool,
    /// `reference;` / `reference { on delete: …; };` — `None` = no
    /// reference clause; `Some(OnDelete::Ignore)` covers both the
    /// flag and the explicit ignore (emitted as bare `REFERENCE`).
    pub reference: Option<OnDelete>,
    /// Absent elides (rule 9: SurrealQL's default FULL applies —
    /// INFO materializes `PERMISSIONS FULL`, the normalizer's
    /// adopt/elide business).
    pub permissions: Option<OpPermissions>,
    pub graphql: Option<Graphql>,
    pub comment: Option<DocComment>,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`].
    pub clause_comments: Vec<ClauseComment>,
}

/// `mixin <name> { … };` (spec/mixin.md) — SDL-only: a named group of
/// field definitions that expands, at compile time, into every table
/// that spreads it ([`Stmt::Spread`]). Emits no DDL of its own — no
/// COMMENT target, so `///` is rejected on the statement (rule 5)
/// while members carry their own (every expanded field gets the
/// member's COMMENT). Members are the table block's field-entry
/// grammar ([`Field`], nested form — `on:` never present),
/// parser-enforced. Mixin names are their own namespace: the parser
/// registers each as parsed (rule 7 — define before reference),
/// rejecting duplicates (SDL-E1062) and unknown spread references
/// (SDL-E1061). Expansion ([`crate::expand`]) drops the statement
/// from the lowered file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mixin {
    pub name: Ident,
    /// The member statements in source order (`Stmt::Field`,
    /// `Stmt::Index`, and `Stmt::Event` variants only,
    /// parser-enforced — a table block's body minus `on:` clauses,
    /// spec/mixin.md).
    pub members: Vec<Stmt>,
    /// The name's span — the duplicate-name diagnostic's anchor
    /// (`Module::from_span`'s precedent: zeroed by the compare
    /// normalizer).
    pub name_span: Span,
    /// Plain `//` lines above the statement; anchorless lines inside
    /// the block ride [`Self::trailing_comments`].
    pub comments: Vec<Comment>,
    pub trailing_comments: Vec<Comment>,
    /// Per-clause comment anchors (rule 5) — [`ClauseComment`]; the
    /// statement defines no clauses, so the empty path is this field's
    /// only resident.
    pub clause_comments: Vec<ClauseComment>,
}

/// `...<name>;` — a **spread entry**: a table-block body entry (not a
/// clause — no key, no colon; spec/mixin.md) that expands to its
/// mixin's members at the entry's position — explicitly overshadowed
/// members and members a LATER spread's same-keyed member beats are
/// skipped (last-occurrence-wins, spec/mixin.md). Produced by the
/// parser only inside a non-view [`Table::nested`]; expansion
/// ([`crate::expand`]) replaces it with the member clones, so codegen
/// never sees one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spread {
    pub name: Ident,
    /// The `…name` site's span — the reference diagnostics' anchor,
    /// zeroed by the compare normalizer.
    pub span: Span,
    /// Plain `//` lines above the entry (rule 5).
    pub comments: Vec<Comment>,
    /// The empty-path same-line after-note (rule 5) — the entry has no
    /// clauses, the only resident.
    pub clause_comments: Vec<ClauseComment>,
}

/// `include "<path>";` (spec/include.md) — **file composition**: the
/// loader splices the included file's statements into the stream at
/// this statement's position, so the flattening invariant holds
/// (a composed project compiles identically to its flattened single
/// file) and codegen never sees one. The path is the authored literal
/// carried verbatim — the canonical writer re-prints it, resolution
/// (relative to the including file) happens only at load time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    pub path: String,
    /// The head word's span — the loader's diagnostics (missing file,
    /// cycle) and the strict entry's rejection anchor here; zeroed by
    /// the compare normalizer.
    pub span: Span,
    /// Plain `//` lines above the statement (rule 5).
    pub comments: Vec<Comment>,
    /// The empty-path same-line after-note (rule 5) — the statement
    /// has no clauses, the only resident.
    pub clause_comments: Vec<ClauseComment>,
}

impl SdlFile {
    /// Whether two files hold the same definitions under the
    /// comparison contract the fmt round-trip and the differ share:
    /// canonical VALUES only. The carried alias
    /// spellings sit outside `==` already ([`Authored`]/[`Carried`]),
    /// and kinds compare through their canonical re-print, so
    /// authored kind-spelling variance (`record < person >` vs
    /// `record<person>`) never splits a definition. `Module`'s
    /// `from_span` is zeroed first: spans ride the AST for
    /// diagnostics only, and derived equality is otherwise
    /// source-offset-dependent on it. This is the comparison entry
    /// point — plain `==` on [`SdlFile`] stays the structural
    /// identity the parser tests need.
    pub fn same_definition(&self, other: &SdlFile) -> bool {
        normalize_for_compare(self) == normalize_for_compare(other)
    }
}

fn normalize_for_compare(file: &SdlFile) -> SdlFile {
    let mut file = file.clone();
    file.trailing_comments = Vec::new();
    for stmt in &mut file.items {
        normalize_stmt_for_compare(stmt);
    }
    file
}

/// The comment-field strip of [`SdlFile::same_definition`], exposed
/// for the differ's minimal form: its structural comparisons run on
/// resolved statements, and comments are layout, never definition
/// (rule 5) — an SDL-side note must not drift against its read twin.
/// The differ therefore also rides the comparison's member-order
/// blindness (the sort in [`normalize_stmt_for_compare`]): member
/// order is layout, not state, so a body-order difference between
/// the SDL and read sides never classes as drift. Safe by
/// construction — `resolve()` consumes its statements by value, so
/// the sort never mutates a live AST.
pub(crate) fn strip_comment_fields(stmt: &mut Stmt) {
    normalize_stmt_for_compare(stmt)
}

/// Every statement's comment fields as one borrowed surface — the
/// shared mutator behind the merge's carry and the parser's
/// close-note attach (one accessor instead of one 17-arm match per
/// verb). `doc` and `trailing` are `None` for the statements without
/// that field: `raw` and `include` have neither, the configs carry no
/// doc slot, `namespace` no trailing list.
pub(crate) struct CommentSlots<'a> {
    pub doc: Option<&'a mut Option<DocComment>>,
    pub leading: &'a mut Vec<Comment>,
    pub trailing: Option<&'a mut Vec<Comment>>,
    pub clause: &'a mut Vec<ClauseComment>,
}

/// A mixin member's collision identity — the family word and key
/// [`Stmt::member_key`] returns (the family word doubles as the lint
/// message's kind word).
pub(crate) type MemberKey = (&'static str, String);

impl Stmt {
    /// A mixin member's collision identity (spec/mixin.md): fields key
    /// by their dotted path, indexes and events by name — a field `x`
    /// and an index `x` never collide (distinct namespaces, the
    /// merge's family+key model). `None` for non-members, the spread
    /// entry included: it resolves against a name, it does not
    /// collide as a member.
    pub(crate) fn member_key(&self) -> Option<MemberKey> {
        match self {
            Stmt::Field(field) => Some(("field", field.name.to_text())),
            Stmt::Index(index) => Some(("index", index.name.raw.clone())),
            Stmt::Event(event) => Some(("event", event.name.raw.clone())),
            _ => None,
        }
    }

    pub(crate) fn comment_slots(&mut self) -> CommentSlots<'_> {
        let (doc, leading, trailing, clause) = match self {
            Stmt::Namespace(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                None,
                &mut s.clause_comments,
            ),
            Stmt::Sequence(s) => (
                None,
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Param(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Database(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Bucket(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::User(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Analyzer(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Event(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Module(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Function(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Index(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Api(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Access(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Table(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Field(s) => (
                Some(&mut s.comment),
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Mixin(s) => (
                None,
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Spread(s) => (None, &mut s.comments, None, &mut s.clause_comments),
            Stmt::Raw(s) => (None, &mut s.comments, None, &mut s.clause_comments),
            Stmt::Include(s) => (None, &mut s.comments, None, &mut s.clause_comments),
            Stmt::Config(Config::Api(s)) => (
                None,
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Config(Config::Graphql(s)) => (
                None,
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
            Stmt::Config(Config::Default(s)) => (
                None,
                &mut s.comments,
                Some(&mut s.trailing_comments),
                &mut s.clause_comments,
            ),
        };
        CommentSlots {
            doc,
            leading,
            trailing,
            clause,
        }
    }

    /// The merge's comment carry (issue #35's fix): the SDL side's
    /// comment fields ride the merged statement — the read model
    /// carries none by construction, and a comment is layout, never
    /// definition, so carrying it is semantics-free. The `///` doc
    /// block matters most: it is the statement's `COMMENT` clause,
    /// and dropping it would change what a re-apply writes. The
    /// discriminant check is variant-blind across the three configs
    /// (their comment slots share one shape, so a cross-pair would be
    /// harmless — and the merge's `classify` pairing never makes
    /// one); the source clone is the read side's price for the one
    /// mutator surface.
    pub(crate) fn carry_comment_fields_from(&mut self, sdl: &Stmt) {
        if std::mem::discriminant(self) != std::mem::discriminant(sdl) {
            return;
        }
        let mut from_stmt = sdl.clone();
        let from = from_stmt.comment_slots();
        let to = self.comment_slots();
        *to.leading = std::mem::take(from.leading);
        *to.clause = std::mem::take(from.clause);
        if let (Some(to_slot), Some(from_slot)) = (to.doc, from.doc) {
            *to_slot = from_slot.take();
        }
        if let (Some(to_slot), Some(from_slot)) = (to.trailing, from.trailing) {
            *to_slot = std::mem::take(from_slot);
        }
    }
}

fn normalize_stmt_for_compare(stmt: &mut Stmt) {
    // Comments never participate in definition equality (the doc on
    // [`Comment`]): every variant strips them, so a parsed file and
    // its fmt output hold the same definition, and the differ sees
    // SDL and read-model statements identically.
    match stmt {
        Stmt::Namespace(stmt) => {
            stmt.comments = Vec::new();
            stmt.clause_comments = Vec::new();
        }
        Stmt::Raw(stmt) => {
            stmt.comments = Vec::new();
            stmt.clause_comments = Vec::new();
        }
        Stmt::Module(module) => {
            module.comments = Vec::new();
            module.trailing_comments = Vec::new();
            module.clause_comments = Vec::new();
            module.from_span = Span::point(0);
        }
        Stmt::Config(config) => {
            let (comments, trailing, clause) = match config {
                Config::Api(c) => (
                    &mut c.comments,
                    &mut c.trailing_comments,
                    &mut c.clause_comments,
                ),
                Config::Graphql(c) => (
                    &mut c.comments,
                    &mut c.trailing_comments,
                    &mut c.clause_comments,
                ),
                Config::Default(c) => (
                    &mut c.comments,
                    &mut c.trailing_comments,
                    &mut c.clause_comments,
                ),
            };
            *comments = Vec::new();
            *trailing = Vec::new();
            *clause = Vec::new();
        }
        Stmt::Function(function) => {
            function.comments = Vec::new();
            function.trailing_comments = Vec::new();
            function.clause_comments = Vec::new();
            for (_, kind) in &mut function.args {
                kind.text = crate::kind::canonical_kind_text(&kind.text);
            }
            if let Some(returns) = &mut function.returns {
                returns.text = crate::kind::canonical_kind_text(&returns.text);
            }
        }
        Stmt::Field(field) => {
            field.comments = Vec::new();
            field.trailing_comments = Vec::new();
            field.clause_comments = Vec::new();
            if let Some(kind) = &mut field.kind {
                kind.text = crate::kind::canonical_kind_text(&kind.text);
            }
        }
        Stmt::Sequence(stmt) => {
            stmt.comments = Vec::new();
            stmt.trailing_comments = Vec::new();
            stmt.clause_comments = Vec::new();
        }
        Stmt::Param(stmt) => {
            stmt.comments = Vec::new();
            stmt.trailing_comments = Vec::new();
            stmt.clause_comments = Vec::new();
        }
        Stmt::Database(stmt) => {
            stmt.comments = Vec::new();
            stmt.trailing_comments = Vec::new();
            stmt.clause_comments = Vec::new();
        }
        Stmt::Bucket(stmt) => {
            stmt.comments = Vec::new();
            stmt.trailing_comments = Vec::new();
            stmt.clause_comments = Vec::new();
        }
        Stmt::User(stmt) => {
            stmt.comments = Vec::new();
            stmt.trailing_comments = Vec::new();
            stmt.clause_comments = Vec::new();
        }
        Stmt::Analyzer(stmt) => {
            stmt.comments = Vec::new();
            stmt.trailing_comments = Vec::new();
            stmt.clause_comments = Vec::new();
        }
        Stmt::Event(stmt) => {
            stmt.comments = Vec::new();
            stmt.trailing_comments = Vec::new();
            stmt.clause_comments = Vec::new();
        }
        Stmt::Index(stmt) => {
            stmt.comments = Vec::new();
            stmt.trailing_comments = Vec::new();
            stmt.clause_comments = Vec::new();
        }
        Stmt::Api(stmt) => {
            stmt.comments = Vec::new();
            stmt.trailing_comments = Vec::new();
            stmt.clause_comments = Vec::new();
            for action in &mut stmt.actions {
                action.comments = Vec::new();
                action.trailing_comments = Vec::new();
                action.clause_comments = Vec::new();
            }
            if let Some(any) = &mut stmt.any {
                any.comments = Vec::new();
                any.trailing_comments = Vec::new();
                any.clause_comments = Vec::new();
            }
        }
        Stmt::Access(stmt) => {
            stmt.comments = Vec::new();
            stmt.trailing_comments = Vec::new();
            stmt.clause_comments = Vec::new();
        }
        // A table's nested definitions compare under the same
        // contract (nested fields carry kinds; comments strip
        // recursively). Body order is layout, never definition — the
        // canonical writer groups the members, so the comparison
        // sorts by [`member_sort_key`] to stay order-blind (stable:
        // duplicate-keyed members keep their authored relative order
        // on both sides).
        Stmt::Table(table) => {
            table.comments = Vec::new();
            table.trailing_comments = Vec::new();
            table.clause_comments = Vec::new();
            for nested in &mut table.nested {
                normalize_stmt_for_compare(nested);
            }
            table.nested.sort_by_key(member_sort_key);
        }
        Stmt::Mixin(mixin) => {
            mixin.comments = Vec::new();
            mixin.trailing_comments = Vec::new();
            mixin.clause_comments = Vec::new();
            mixin.name_span = Span::point(0);
            for member in &mut mixin.members {
                normalize_stmt_for_compare(member);
            }
            mixin.members.sort_by_key(member_sort_key);
        }
        Stmt::Spread(spread) => {
            spread.comments = Vec::new();
            spread.clause_comments = Vec::new();
            spread.span = Span::point(0);
        }
        Stmt::Include(include) => {
            include.comments = Vec::new();
            include.clause_comments = Vec::new();
            include.span = Span::point(0);
        }
    }
}

/// A body member's sort identity for [`SdlFile::same_definition`]:
/// any total order makes the comparison order-blind — this one groups
/// by family and keys exactly as [`Stmt::member_key`] keys collisions
/// (the spread's own key is its name). The trailing rank holds the
/// statement kinds the parser never nests (hand-built ASTs only), so
/// the key stays total.
fn member_sort_key(stmt: &Stmt) -> (u8, String) {
    match stmt {
        Stmt::Field(field) => (0, field.name.to_text()),
        Stmt::Index(index) => (1, index.name.raw.clone()),
        Stmt::Event(event) => (2, event.name.raw.clone()),
        Stmt::Spread(spread) => (3, spread.name.raw.clone()),
        _ => (4, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Carried provenance never participates in equality:
    /// aliased and canonical spellings hold the same definition; only
    /// the writer's output shows the difference. (`Module::from_span`
    /// is the other non-semantic field — covered by
    /// [`SdlFile::same_definition`], since derived `PartialEq` is
    /// span-sensitive by design.)
    #[test]
    fn authored_and_carried_spellings_never_participate_in_eq() {
        assert_eq!(
            Authored::aliased(Level::Root, "kv"),
            Authored::canonical(Level::Root)
        );
        assert_ne!(
            Authored::aliased(Level::Root, "kv"),
            Authored::canonical(Level::Namespace)
        );
        assert_eq!(
            Filter::Snowball(Authored::aliased(Language::English, "eng")),
            Filter::Snowball(Authored::canonical(Language::English))
        );
        assert_eq!(Carried(Some("ns")), Carried(None));
        assert_eq!(Carried(Some("columns")), Carried::default());
    }

    /// `same_definition` is span-blind (`Module::from_span`) and
    /// kind-canonicalizing; everything else is ordinary equality.
    #[test]
    fn same_definition_zeroes_spans_and_canonicalizes_kinds() {
        let parse = |src: &str| crate::parser::parse_file(src).0;
        // from_span lands on different offsets; the definitions match.
        assert!(parse("ns a;").same_definition(&parse("namespace a;")));
        let spaced = parse("module m { from: f'b:/k'; unsigned; };");
        let module = match &spaced.items[0] {
            Stmt::Module(module) => module.clone(),
            _ => unreachable!(),
        };
        let mut shifted = module.clone();
        shifted.from_span = Span::new(module.from_span.offset + 7, module.from_span.len);
        assert_ne!(
            module, shifted,
            "sanity: derived equality is span-sensitive by design"
        );
        assert!(
            SdlFile {
                items: vec![Stmt::Module(module)],
                trailing_comments: Vec::new(),
            }
            .same_definition(&SdlFile {
                items: vec![Stmt::Module(shifted)],
                trailing_comments: Vec::new(),
            })
        );
        // Kind spelling variance is not a definition difference…
        assert!(
            parse("fn f::g($a: record < person >) {};")
                .same_definition(&parse("fn f::g($a: record<person>) {};"))
        );
        // …but value differences still are.
        assert!(!parse("fn f::g($a: int) {};").same_definition(&parse("fn f::g($a: string) {};")));
    }
}
