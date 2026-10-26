//! Diagnostics: the [`Diag`] enum, `SDL-Exxxx` codes, and miette
//! wiring — a graphical renderer for humans (the JSON renderer for
//! tooling attaches at the CLI boundary).
//!
//! Statement-level recovery rides the lexer's depth-0 `;` scanner
//! (every statement is diagnosed); clause-level recovery skips to the
//! next known clause keyword (one typo never blanks the rest of a
//! statement's diagnostics).
//!
//! Code registry: every diagnostic carries the literal `SDL-Exxxx`
//! spelling — the same string in graphical, JSON, and doc rendering.
//! Blocks: `SDL-E0xxx` the scanner and the front of the pipeline
//! (lexing, doc-comment capture, file entry), `SDL-E1xxx` parser
//! (plus E1060, the pipeline's stage-2 DDL oracle), `SDL-E3xxx` the
//! INFO round-trip readers. (`SDL-E2xxx` was the
//! codegen block; it retired with the 3.2 downgrade target on
//! 2026-09-25 — codes are hand-assigned, so the gap stays.) Each
//! diagnostic's `url()` deep-links its code into the
//! governing spec file (repo-relative until the crate has a
//! canonical home to link to).

use crate::lexer::Span;
use miette::LabeledSpan;
use std::fmt;

#[derive(Debug, Clone)]
pub enum Diag {
    // ---- SDL-E0xxx: the scanner and the front of the pipeline ----
    /// A string literal ran to end of file without its closing quote.
    UnterminatedString { span: Span },
    /// A regex literal ran to end of file without its closing `/` (an
    /// unescaped one — `\` escapes the next byte, source-verified
    /// against upstream's regex eater).
    UnterminatedRegex { span: Span },
    /// A `/* … */` comment ran to end of file without `*/`.
    UnterminatedBlockComment { span: Span },
    /// A quoted identifier ran to end of file without its closer.
    UnterminatedQuotedIdent { span: Span, closer: char },
    /// A capture ran to end of file without its depth-0 terminator.
    UnterminatedCapture { span: Span, terminator: char },
    /// A closing bracket at capture depth 0 closes nothing.
    UnbalancedCapture { span: Span, bracket: char },
    /// A `\` escape inside a quoted identifier is not in the SurrealQL
    /// escape table (upstream rejects it when unescaping the ident).
    InvalidIdentEscape { span: Span },
    /// A `///` block ended the file with no definition below it, so it
    /// can never become a `COMMENT` (rule 5).
    DanglingDocComment { span: Span },
    /// The CLI's file argument could not be read as UTF-8 text. The
    /// loader's missing/unreadable included files share the code: the
    /// same class, anchored this time on the include statement that
    /// named the path (`span`, rendered against the including file's
    /// own text).
    FileUnreadable {
        path: String,
        reason: String,
        span: Option<Span>,
    },
    /// The CLI's file argument named a DIRECTORY, not a file — the
    /// same SDL-E0008 class (the path cannot be read as a schema),
    /// branched on the io error kind at the read: the generic
    /// unreadable help says "the file must exist", which is wrong
    /// twice over for a path that plainly exists as a directory
    /// (issue #90).
    FileIsADirectory { path: String },
    /// The CLI could not write a file it was asked to rewrite
    /// (`fmt --in-place`).
    FileUnwritable { path: String, reason: String },
    /// The input opens with a generated migration script's draft
    /// header (`-- surform migration NNNN — <name>` — the format
    /// `migrate generate --save` writes): `--` is SurrealQL comment
    /// syntax, not SDL's, so the file is not SDL and `fmt` never
    /// reflows migration scripts — the migration pipeline
    /// (generate/commit/push) owns these files, and the schema they
    /// converge to lives in the header's `-- source:` file. The span
    /// anchors the header's first line.
    MigrationScriptRefused { span: Span },
    // ---- SDL-E1xxx: the parser ----
    /// The word opening a definition is not a statement keyword.
    UnknownStatement {
        span: Span,
        word: String,
        suggestion: Option<String>,
    },
    /// A byte that can start neither a statement nor a clause key.
    IllegalCharacter { span: Span, ch: char },
    /// A clause key the statement does not define (for `namespace`,
    /// whose clause table is empty, this is the spec's non-empty-block
    /// error).
    UnknownClause {
        span: Span,
        key: String,
        statement: &'static str,
        suggestion: Option<String>,
    },
    /// A definition's name position holds no identifier.
    ExpectedName { span: Span },
    /// After the name there was neither `;` nor `{` (`None` = end of
    /// file).
    ExpectedStatementEnd { span: Span, found: Option<char> },
    /// A `{` block never closed.
    UnterminatedBlock { span: Span },
    /// A bare reserved word cannot serve as an identifier — SurrealQL
    /// rejects it in name positions (spec rule 9: SDL identifiers
    /// follow SurrealQL's rules).
    ReservedIdent { span: Span, word: String },
    /// A `///` block sits on a statement whose SurrealQL grammar has
    /// no `COMMENT` clause (`sequence`; rule 5) — there is nowhere
    /// for it to attach.
    DocCommentUnsupported { span: Span, statement: &'static str },
    /// The embedded official parser rejected an expression capture.
    /// The span is the whole capture; the message carries the embedded
    /// error's render — cause lines plus its source snippet, whose
    /// `--> [line:column]` marker is re-anchored to the capture's
    /// file position while the quoted line stays capture-relative
    /// (stage 2's marker names the validation harness text instead).
    EmbeddedParseError { span: Span, message: String },
    /// The embedded official parser rejected the compiler's own
    /// emitted DDL — the stage-2 oracle (`codegen::validate_ddl`;
    /// the wasm `check` runs it over the whole emission). This is a
    /// Surform bug, never an authored-file problem: the SDL parsed
    /// clean, so the compiler mis-emitted. The span anchors the file
    /// head; the message carries the upstream render verbatim (the
    /// DDL text is its own coordinate space).
    EmbeddedDdlRejected { span: Span, message: String },
    /// A clause key stands where a value must follow: the `:` (and
    /// the expression after it) is missing.
    ExpectedClauseValue { span: Span, key: &'static str },
    /// A flag or sub-block clause met a separator that selects
    /// another clause form (`strict: true;`, `changefeed: 1d;`) —
    /// the separator is what picks the form (rule 2). The message is
    /// composed at the push site, where the key and its form are
    /// known.
    UnexpectedClauseForm { span: Span, message: String },
    /// A `<duration>` clause value is not a SurrealQL duration
    /// literal (integer + unit, compounds like `1h30m`).
    InvalidDuration { span: Span, text: String },
    /// A sub-block that always carries a required clause arrived
    /// without it (`changefeed { … }` without `expiry`), or a
    /// statement block missing its own required clause (`user { … }`
    /// without `on`). The clause is spelled as written — a value
    /// clause carries its bare name (`expiry`), a flag its full
    /// spelling (`unsigned;`); the `;` is what tells the help which
    /// corrective shape to show.
    RequiredClauseMissing {
        span: Span,
        clause: &'static str,
        block: &'static str,
    },
    /// A keyword-set or keyword-list clause value holds a word the
    /// clause does not define (`roles: ownr;`, `on: roto;`), or is
    /// empty (a trailing comma). SDL keywords are lowercase only
    /// (rule 9), so case is part of the misspelling.
    UnknownClauseWord {
        span: Span,
        word: String,
        key: &'static str,
        suggestion: Option<String>,
    },
    /// Two clauses met that SurrealQL allows at most one of
    /// (`password` + `passhash`, parser-enforced upstream).
    MutuallyExclusive {
        span: Span,
        clause: &'static str,
        other: &'static str,
    },
    /// A `<string>` clause value is not a SurrealQL string literal
    /// (integer+unit's sibling: the capture is validated by SDL, rule
    /// 3's keyword clause).
    InvalidString { span: Span, text: String },
    /// A `filters:` list entry is not a SurrealQL filter atom — wrong
    /// name, malformed or non-u16 arguments, a quoted/unknown
    /// language, a non-string mapper path.
    InvalidFilter { span: Span, text: String },
    /// A `function:` value is not a well-formed `<fn-path>` — a
    /// missing or quoted `fn::`/`mod::` prefix, an empty or absent
    /// segment, trailing call parens, an `ml::`/`silo::` path.
    InvalidFnPath { span: Span, message: String },
    /// An identifier clause value (`event`'s `on:`) is not a single
    /// bare word or quoted identifier.
    InvalidIdent { span: Span, text: String },
    /// A numeric clause value (`async { retry: …; maxdepth: …; }`) is
    /// not a whole number in the clause's range — the range spelling
    /// differs per clause (u16 for `async`, u32 for graphql
    /// `depth:`/`complexity:`).
    InvalidNumber {
        span: Span,
        text: String,
        range: &'static str,
    },
    /// A `from:` clause value is not a module executable — an f-string
    /// (`f'bucket:/key'`) or a Silo package path
    /// (`silo::org::pkg::<maj.min.patch>`). The message is composed at
    /// the push site, where the arm's grammar is known.
    InvalidExecutable { span: Span, message: String },
    /// `module`'s header name and executable contradict each other
    /// (spec/module.md's name–variant coupling): a file executable
    /// requires the name, a silo executable is self-named and rejects
    /// it. The message is composed at the push site, where the
    /// variant is known.
    ModuleExecutableName { span: Span, message: String },
    /// `fn`'s signature header is malformed: no name after the
    /// keyword, a missing `::` path segment, a missing `(` or `:`, a
    /// `$` followed by no parameter name, a missing `->` half, or a
    /// missing body `{`. The message is composed at the push site,
    /// where the position is known.
    InvalidFunctionSignature { span: Span, message: String },
    /// A `fn` parameter name without its `$` — SurrealQL requires it
    /// (`expected a parameter`, spec/function.md), so SDL rejects the
    /// spelling at parse with the fix in the help.
    FunctionParamNeedsDollar { span: Span },
    /// A `fn` header whose first path segment spells `fn` — the
    /// keyword already carries the prefix, so `fn fn::weird(…)` would
    /// emit `DEFINE FUNCTION fn::fn::weird(…)`, a different function
    /// than the author spelled (the doubled-prefix class, issue #110).
    /// SDL's header path is bare; `fn::` belongs to reference
    /// positions.
    FnNameDoubledPrefix { span: Span },
    /// A trailing `,` in `fn`'s parameter list. SurrealQL accepts the
    /// shape; SDL is deliberately stricter since INFO never emits one
    /// (spec/function.md).
    FunctionParamTrailingComma { span: Span },
    /// A `fn` parameter or `->` return kind is not a SurrealQL kind
    /// (README rule 4's SDL type parser mirrors v3.2.4's kind
    /// grammar). The text is the trimmed capture.
    InvalidKind { span: Span, text: String },
    /// `config`'s header variant word is not one of `api | graphql |
    /// default` (spec/config.md: the variant is the header — there is
    /// no name position to fall back to).
    UnknownVariant {
        span: Span,
        word: String,
        suggestion: Option<String>,
    },
    /// A `middleware:` list entry is not a well-formed call — a name
    /// path (bare, `fn::`, or `mod::`; `silo::`/`ml::` are outside
    /// SDL's surface) followed by `(` and its arguments. The message
    /// is composed at the push site, where the position is known.
    InvalidMiddleware { span: Span, message: String },
    /// An `index`'s `fields:` list violates its kind's arity — the
    /// fulltext/hnsw/diskann kinds take exactly one field. (Count
    /// forbids the clause outright: its table omits it, so the shape
    /// diagnoses as [`Diag::UnknownClause`]; plain/unique take ≥ 1,
    /// enforced by the required-clause check.) The message is
    /// composed at the push site, where the kind is known.
    InvalidIndexFields { span: Span, message: String },
    /// An `index` kind parameter violates its kind's domain — `m` >
    /// 127, `m0` beyond u8, a DISKANN-restricted distance/type, the
    /// `cosine_normalized` type combo, `degree`/`l_build` = 0. The
    /// message is composed at the push site, where the parameter and
    /// kind are known.
    InvalidIndexParam { span: Span, message: String },
    /// A float clause value (`index`'s `lm:`/`alpha:`/minkowski
    /// order/`bm25:` sides) is not an unsigned decimal number —
    /// digits with `_` separators, optional `.` fraction, optional
    /// `e` exponent, optional `f` suffix.
    InvalidFloat { span: Span, text: String },
    /// A `bm25:` value is not a parenthesized pair of numbers — the
    /// parens are mandatory and exactly two sides are taken
    /// (spec/index.md).
    InvalidBm25 { span: Span, message: String },
    /// A `distance:` value is not one of the ten distance keywords —
    /// or `minkowski` without its parenthesized order, or a
    /// no-argument distance spelled with one. The message is composed
    /// at the push site, where the failure shape is known.
    InvalidDistance { span: Span, message: String },
    /// An `api` header path is not a single string literal (the one
    /// spelling SDL's route-path rule allows; SurrealDB validates the
    /// path grammar itself at define time).
    InvalidApiPath { span: Span, text: String },
    /// An `api` sub-block head word is neither one of the six methods
    /// nor `any` (rule 9: SDL's keywords are lowercase-only). The
    /// suggestion is did-you-mean over the head set.
    UnknownApiMethod {
        span: Span,
        word: String,
        suggestion: Option<String>,
    },
    /// An HTTP method appears in more than one group — or twice in
    /// one head list (spec/api.md: each method may appear in only ONE
    /// group; SDL validates eagerly where upstream errors at define
    /// time).
    DuplicateApiMethod { span: Span, method: String },
    /// A second `any { … };` block in one `api` body — a parse error,
    /// not a fold (spec/api.md: upstream folds repeated `FOR ANY`
    /// clauses config-last-wins with an earlier THEN surviving, which
    /// SDL rejects as ambiguous).
    DuplicateAnyBlock { span: Span },
    /// A method group carries no `then:` — required on every group
    /// (upstream: `expected THEN`); only the `any` block may omit it.
    MissingApiThen { span: Span },
    /// An `access` JWT verification source violates its shape
    /// (spec/access.md): neither `url:` nor `algorithm:`+`key:` at
    /// all, the two shapes mixed, or a half pair (`algorithm:`
    /// without `key:`, or `key:` without `algorithm:` — upstream's
    /// grammar is positional `ALGORITHM <alg> KEY <expr>` or
    /// `URL <expr>`). The message is composed at the push site, where
    /// the failure shape is known.
    InvalidAccessSource { span: Span, message: String },
    /// An `access` clause the selected `type:` variant does not
    /// define (spec/access.md: `signup:` on a `jwt` access, `for:` on
    /// a `record` one). The union clause table parses every variant's
    /// vocabulary, so membership is the validation pass's job — there
    /// is no per-variant table to diagnose the spelling as unknown.
    /// The message is composed at the push site, where the variant
    /// and the defining variants are known.
    InvalidAccessClause { span: Span, message: String },
    /// An `access` definition sits at a level its variant forbids:
    /// `type: record` and `for: record` are database-level only
    /// (parser-enforced upstream). The message is composed at the
    /// push site, where the offending clause is known.
    InvalidAccessLevel { span: Span, message: String },
    /// An `access` `issuer { … }` block violates its rules
    /// (spec/access.md): a declared `algorithm:` unequal to the
    /// verification algorithm when verifying by local key (upstream
    /// rejects: "a compatible algorithm or no algorithm"), or a
    /// missing `key:` where nothing auto-derives it (a key-less
    /// issuer would materialize a RANDOM key). The message is
    /// composed at the push site, where the rule is known.
    InvalidAccessIssuer { span: Span, message: String },
    /// A `table` relation header is malformed (spec/table.md): a
    /// missing second arrow (both are mandatory — the name is always
    /// flanked by `->` on both sides), an empty name or endpoint
    /// position, or a bad endpoint identifier. The message is
    /// composed at the push site, where the position is known.
    InvalidTableHeader { span: Span, message: String },
    /// A `table` clause the selected header kind does not define
    /// (spec/table.md): `enforced;` on a non-relation table. The
    /// union clause table parses every kind's vocabulary, so
    /// membership is the validation pass's job — there is no
    /// per-kind table to diagnose the spelling as unknown. The
    /// message is composed at the push site, where the kind is known.
    InvalidTableClause { span: Span, message: String },
    /// A `table` clause value outside its kind's domain (spec/
    /// table.md): an INLINE cache cap above 256 — the engine's own
    /// `MAX_INLINE_CACHE_CAP`, enforced at define/alter (SDL mirrors
    /// it at the fold; the index unit's `m` ≤ 127 precedent). The
    /// message is composed at the push site, where the clause is
    /// known.
    InvalidTableParam { span: Span, message: String },
    /// `passscram:` carries a value the engine's DDL parser would
    /// reject (spec/user.md, the 3.3 grammar): upstream validates the
    /// verifier AT PARSE (`ScramCredential::from_verifier_string` —
    /// the `SCRAM-SHA-256$<iter>:<salt>$<stored>:<server>` shape,
    /// iterations ∈ [4096, 2²⁴], base64 parts, 32-byte keys), so SDL
    /// mirrors the shape check at the fold — accepted SDL must emit
    /// engine-parseable DDL. The message is composed at the push
    /// site, where the failed check is known.
    InvalidPassscram { span: Span, message: String },
    /// A `permissions:` value is not the `none | full` shorthand
    /// (spec/table.md, spec/field.md; README permission forms): the
    /// per-operation statements use the per-op parser, which accepts
    /// only `NONE | FULL | FOR` — a bare `permissions: <expr>;` is
    /// invalid there (unlike param/bucket/api, whose single-value form
    /// takes an expression). The statement names itself in the
    /// message and the spec link.
    InvalidPermissionsShorthand {
        span: Span,
        text: String,
        statement: &'static str,
    },
    /// A nested definition inside a `table` block carries an explicit
    /// `on:` clause (spec/table.md): nested statements inherit their
    /// host table — exactly one of the two spellings applies, the
    /// standalone `on:` or the nesting.
    InvalidNestedOn { span: Span, statement: &'static str },
    /// A view's block does not start with its query (spec/table.md):
    /// the block IS the query — a single `;`-terminated select, no
    /// clause key — so an empty block or a clause-key-shaped head is
    /// the misplaced query. The message is composed at the push site.
    InvalidViewBody { span: Span, message: String },
    /// A regular field has neither a header type nor a computed
    /// expression (spec/field.md): the type is required — SDL's
    /// licensed deviation from SurrealQL's typeless form (rule 9;
    /// the typeless shape is reachable via `raw { … };`), because
    /// `name: {` would collide with expression capture. Only the
    /// typeless computed (`name = <expr>;`) goes without.
    FieldTypeRequired { span: Span },
    /// A computed field violates its exclusivity or shape rules
    /// (spec/field.md, executor-enforced upstream, adopted eagerly):
    /// `computed` set alongside `default`/`default always`/`value`/
    /// `assert`/`reference`/`readonly`, a computed field on a dotted
    /// or wildcard path, or a subfield defined under a computed
    /// parent. The message is composed at the push site, where the
    /// violated rule is known.
    InvalidFieldComputed { span: Span, message: String },
    /// `flexible;` sits on a field whose type contains no object kind
    /// (spec/field.md — the engine's parse-time check, mirrored: the
    /// runtime parser rejects "FLEXIBLE can only be used with types
    /// containing object").
    InvalidFieldFlexible { span: Span, message: String },
    /// `reference` sits on a nested (dotted or wildcard) field path
    /// (spec/field.md — the engine's define-time check, adopted
    /// eagerly: "Specify a referencing field at the root level
    /// instead").
    InvalidFieldReference { span: Span, message: String },
    /// The `id` field carries a clause its grammar forbids
    /// (spec/field.md, executor-enforced upstream, adopted eagerly):
    /// `value:`, `default always:`, `computed:`, `readonly`,
    /// `flexible`, or `reference`. The message is composed at the
    /// push site, where the clause is known.
    InvalidFieldId { span: Span, message: String },
    /// `inline;` sits on a field the engine's define-time INLINE
    /// contract forbids (spec/field.md, 3.3 — adopted eagerly): a
    /// dotted or wildcard path (only top-level fields embed), or the
    /// `in`/`out` names (the adjacency keys themselves; the `id`
    /// spelling rides [`Diag::InvalidFieldId`]), or a computed field
    /// (nothing stored to embed — that arm rides
    /// [`Diag::InvalidFieldComputed`]'s conflicts). The message is
    /// composed at the push site, where the violated rule is known.
    InvalidFieldInline { span: Span, message: String },
    /// A field `permissions { … }` block spells the `delete`
    /// operation — SurrealQL rejects it for fields ("Can't define
    /// permission DELETE for fields": fields cannot be deleted).
    FieldDeletePermission { span: Span },
    /// A field name path is malformed (spec/field.md): a missing
    /// segment after a `.`, or a leading wildcard (the first segment
    /// is always a name). The message is composed at the push site.
    InvalidFieldPath { span: Span, message: String },
    /// `raw` was not followed by its verbatim body block — the
    /// statement has no bare form (`raw;`) and takes no name
    /// (README rule 8: the braces ARE the grammar).
    RawBodyExpected { span: Span, found: Option<char> },
    /// An access definition writes real secret key material where SDL
    /// only accepts the `'[REDACTED]'` sentinel (spec/access.md's
    /// shape-only rule): a symmetric (hs*) verify `key:`, or any
    /// issuer `key:` — the slots the engine redacts in INFO and whose
    /// values must be provisioned outside SDL. Asymmetric verify keys
    /// (public keys) stay freely authorable.
    AccessSecretExposure { span: Span, message: String },
    /// A `...<name>;` spread entry resolves to no mixin of the
    /// document (spec/mixin.md — the declarative namespace: the
    /// global resolution pass checks every entry against the whole
    /// composed document's mixin set, any file, any position).
    UnknownMixin {
        span: Span,
        name: String,
        suggestion: Option<String>,
    },
    /// The same definition — one family, one name — twice in the
    /// document, any file, any position (spec/README.md rule 7, the
    /// declarative namespace): the engine's OVERWRITE would silently
    /// fold the second onto the first and the reference would be
    /// ambiguous in intent. `family` names the definition family ("a
    /// mixin", "a table", …); the span anchors the SECOND occurrence.
    /// Mixin duplicates within one file still die at parse (the same
    /// code, raised by the parser's name registry); every other
    /// family — and every cross-file duplicate — dies in the order
    /// pass.
    DuplicateDefinition {
        span: Span,
        family: &'static str,
        name: String,
    },
    /// A define-time dependency cycle (spec/README.md rule 7): the
    /// statements in `chain` read one another through the slots the
    /// engine evaluates at define time — a view's top `FROM` chain
    /// (`TbNotFound` in every application order) or a param's
    /// `VALUE` chain (no order stores the right value: the
    /// first-defined read misses to `none`, or fails its arithmetic
    /// outright). No
    /// topological order exists, so the compiler refuses the
    /// document; every other reference class is runtime-evaluated
    /// and edge-free. `chain` spells the loop ("view `a` → view
    /// `b` → view `a`"); the span anchors the earliest participant.
    DefineCycle { span: Span, chain: String },
    /// A define-time dependency the compiler cannot place
    /// (spec/README.md rule 7): the target stands in a LATER scope
    /// section — after the next `namespace`/`database` statement —
    /// and emission reorders within a section, never across a
    /// leader. The emitted DDL would apply against a catalog missing
    /// the target (a view's `TbNotFound`; a param's silently-`none`
    /// VALUE), so the document is refused instead. `target` names the
    /// definition ("view `t`"); the span anchors the referencing
    /// statement. Out-of-document targets are NOT this class — a
    /// reference nothing in the document defines stays the advisory
    /// lints' territory.
    CrossSectionDependency { span: Span, target: String },
    /// The same mixin is spread twice into one table (spec/mixin.md):
    /// every member would define twice.
    DuplicateSpread { span: Span, name: String },
    /// A `...<name>;` spread entry sits in a block that is not a
    /// non-view table body (spec/mixin.md): a view defines no fields
    /// to spread into, a mixin body holds its own field entries (no
    /// mixin-into-mixin), and no other statement's block takes one.
    SpreadMisplaced { span: Span, statement: &'static str },
    /// The same member key (family + name — a field's dotted path, an
    /// index's or event's name) is defined twice within ONE body, a
    /// mixin's or a table's (spec/mixin.md; issue #38's promotion):
    /// the engine's last-wins would silently hide the duplicate —
    /// the copy-paste smell E1064 rejects for spread entries. The
    /// span anchors the SECOND occurrence.
    DuplicateMemberKey {
        span: Span,
        kind: &'static str,
        key: String,
    },
    /// An `include` statement in a context with no file system
    /// (spec/include.md): the strict parse entry — the playground,
    /// inline buffers, runnable doc fences — cannot resolve a
    /// composition, and one it cannot resolve must not silently
    /// compile as if the include were absent. The composable entry
    /// (the loader, the formatter) accepts the statement.
    IncludeUnsupported { span: Span },
    /// A composition-preamble scope violation (spec/include.md): a
    /// `namespace`/`database` after an include. The scope law is the
    /// one order rule left — includes themselves are legal anywhere
    /// in the file (the declarative namespace: an include declares
    /// participation, not an elaboration point), but the preamble
    /// must precede the first include so the spliced content sits
    /// under the root's scope. `statement` names the misplaced head.
    IncludeMisplaced { span: Span, statement: &'static str },
    /// The same file included twice by ONE file (spec/include.md):
    /// a file splices once, and silently absorbing the second
    /// declaration would hide the duplicate — the copy-paste smell
    /// E1067 rejects for member keys (E0010's retirement does not
    /// recycle the number). The span anchors the SECOND declaration;
    /// `first` names the file that included it first.
    DuplicateInclude {
        span: Span,
        path: String,
        first: String,
    },
    /// The merge adopted a statement whose write-back has no legal
    /// position (spec/include.md): scope statements are only legal in
    /// a composed root's preamble, and inserting one there would
    /// re-bind every scope below it; a preamble-bound adoptee that
    /// some authored statement still depends on cannot relocate to the
    /// body without breaking define order. The write-back refuses
    /// instead of guessing. The span anchors the root's first
    /// `include` statement.
    WriteBackUnplaceable { span: Span, statement: String },
    /// An `include` path is absolute (spec/include.md): a schema tree
    /// must be portable, so paths resolve relative to the including
    /// file's directory only. The span anchors the include statement.
    IncludePathAbsolute { span: Span, path: String },
    /// A `namespace`/`database` statement inside an INCLUDED file
    /// (spec/include.md's position rule 3): those statements re-scope
    /// everything below them, so one inside a splice would silently
    /// re-scope the rest of the importing file. Diagnosed by the
    /// included parse entry — the loader's entry for non-root files —
    /// against the included file's own text.
    IncludeScopeBanned { span: Span, statement: &'static str },
    /// A Capitalized word at a kind position resolves to no type
    /// alias of the document (spec/type-alias.md — the declarative
    /// namespace: the order pass checks every reference against the
    /// whole composed document's alias set, any file, any position).
    /// A typo'd alias is an error, never a silent kind.
    UnknownAlias {
        span: Span,
        name: String,
        suggestion: Option<String>,
    },
    /// A type-alias declaration's name is not usable (spec/
    /// type-alias.md): not Capitalized (the rule that keeps alias
    /// uses grammatically distinct from the lowercase kind heads), or
    /// reserved — a SurrealQL kind head would win at every use site
    /// (head matching is ASCII-case-insensitive) and leave the alias
    /// unreachable. `reason` spells which.
    InvalidAliasName {
        span: Span,
        name: String,
        reason: String,
    },
    /// A kind's expansion nests beyond the kind nesting ceiling
    /// (spec/type-alias.md): a chain composes nesting one link at a
    /// time, and every walk the expanded kind feeds recurses over it
    /// — the ceiling keeps that recursion bounded where real schemas
    /// never approach it. `owner` names where the kind arises ("type
    /// alias `A`", "field `a`", …); `depth` is the
    /// expansion's measured height against the 256 ceiling.
    AliasExpansionDepth {
        span: Span,
        owner: String,
        depth: usize,
    },
    /// A kind's expansion is not a kind the strict grammar accepts
    /// (spec/type-alias.md): each part is legal on its own, but the
    /// composition crosses the grammar — a chain of option wrappers
    /// composes `option<option<…>>`, which no parser (SDL's or the
    /// engine's) accepts. `owner` names where the kind arises;
    /// `text` is the expansion.
    AliasExpansionInvalid {
        span: Span,
        owner: String,
        text: String,
    },
    // ---- SDL-E3xxx: the INFO round-trip readers ----
    /// INFO input could not be read. Two classes share the code: the
    /// interchange object walk failed (malformed SurrealQL object
    /// literal — an unclosed brace, a missing `:` or `,`, an
    /// unterminated string), or an individual DDL string inside it
    /// the embedded parser rejected (the statement's text rides the
    /// message). The span anchors per the reading function's
    /// contract: into the interchange text for walk failures, into
    /// the offending DDL string for embedded rejections.
    InfoUnparsable { span: Span, message: String },
    /// A piece of the INFO input does not have the shape the reader
    /// defines for it — a section or entry is not the expected
    /// object/string form, a DDL statement's kind word does not
    /// match the section it arrived in, or a definition's header
    /// name disagrees with its map key. INFO is the server's own
    /// output; any shape mismatch means the printer dialect moved
    /// (or the input is not real INFO).
    InfoUnknownShape { span: Span, message: String },
    /// A DDL statement re-printed outside the pinned printer dialect
    /// the statement reader splits: an unrecognized clause keyword,
    /// a clause out of the canonical order, or trailing text the
    /// statement's grammar does not consume. Every shape constant
    /// the splitters match is pinned by tripwire tests against the
    /// live engine — this diagnostic firing in a round trip means
    /// the embedded printer's output changed (an upstream bump is a
    /// deliberate spec event that must re-verify the read path).
    DdlTripwire { span: Span, message: String },
    /// The interchange object nests deeper than the object walk's
    /// fixed budget (`MAX_INFO_DEPTH` levels, read.rs). Real INFO
    /// output
    /// nests ~3 deep; the budget exists so an adversarial hand-fed
    /// file produces this well-formed diagnostic instead of a stack
    /// overflow.
    InfoDepthExceeded { span: Span, message: String },
    /// A `--info` interchange spec is malformed or does not mirror
    /// the SDL file's declared scopes: an unknown tag prefix, a
    /// duplicate scope, a namespace or database the file never
    /// declares, or a `db=` tag with an empty namespace segment while
    /// the file declares namespaces. Such a pair of inputs cannot
    /// have come from one applied file (codegen emits no `USE` for
    /// the missing level — the scope-mismatch case), so
    /// normalizing would place content the output could never have
    /// held. The span anchors the offending spec string, which the
    /// CLI feeds the renderer as its source. The help is the
    /// mistake's own (issue #90: one static help covered four
    /// mistake classes; the construction site knows which): `None`
    /// falls back to the generic tags-mirror-the-declarations text.
    InterchangeTagInvalid {
        span: Span,
        message: String,
        help: Option<&'static str>,
    },
}

impl Diag {
    /// The literal `SDL-Exxxx` code, uniform across renderers.
    fn code_str(&self) -> &'static str {
        match self {
            Diag::UnterminatedString { .. } | Diag::UnterminatedRegex { .. } => "SDL-E0001",
            Diag::UnterminatedBlockComment { .. } => "SDL-E0002",
            Diag::UnterminatedQuotedIdent { .. } => "SDL-E0003",
            Diag::UnterminatedCapture { .. } => "SDL-E0004",
            Diag::UnbalancedCapture { .. } => "SDL-E0005",
            Diag::InvalidIdentEscape { .. } => "SDL-E0006",
            Diag::DanglingDocComment { .. } => "SDL-E0007",
            Diag::FileUnreadable { .. } | Diag::FileIsADirectory { .. } => "SDL-E0008",
            Diag::FileUnwritable { .. } => "SDL-E0009",
            Diag::MigrationScriptRefused { .. } => "SDL-E0017",
            Diag::WriteBackUnplaceable { .. } => "SDL-E0013",
            Diag::DuplicateInclude { .. } => "SDL-E0014",
            Diag::DefineCycle { .. } => "SDL-E0015",
            Diag::CrossSectionDependency { .. } => "SDL-E0016",
            Diag::IncludePathAbsolute { .. } => "SDL-E0012",
            Diag::UnknownStatement { .. } => "SDL-E1001",
            Diag::IllegalCharacter { .. } => "SDL-E1002",
            Diag::UnknownClause { .. } => "SDL-E1003",
            Diag::ExpectedName { .. } => "SDL-E1004",
            Diag::ExpectedStatementEnd { .. } => "SDL-E1005",
            Diag::UnterminatedBlock { .. } => "SDL-E1006",
            Diag::ReservedIdent { .. } => "SDL-E1007",
            Diag::DocCommentUnsupported { .. } => "SDL-E1008",
            Diag::EmbeddedParseError { .. } => "SDL-E1009",
            Diag::EmbeddedDdlRejected { .. } => "SDL-E1060",
            Diag::UnknownMixin { .. } => "SDL-E1061",
            Diag::DuplicateDefinition { .. } => "SDL-E1062",
            // SDL-E1063 retired with the last-occurrence-wins
            // resolution (spec/mixin.md's amendment): the block keeps
            // its numbering — codes are hand-assigned, gaps stay.
            Diag::DuplicateSpread { .. } => "SDL-E1064",
            Diag::SpreadMisplaced { .. } => "SDL-E1065",
            Diag::DuplicateMemberKey { .. } => "SDL-E1067",
            Diag::IncludeUnsupported { .. } => "SDL-E1068",
            Diag::IncludeMisplaced { .. } => "SDL-E1069",
            Diag::IncludeScopeBanned { .. } => "SDL-E1070",
            Diag::UnknownAlias { .. } => "SDL-E1071",
            Diag::InvalidAliasName { .. } => "SDL-E1072",
            Diag::AliasExpansionDepth { .. } => "SDL-E1073",
            Diag::AliasExpansionInvalid { .. } => "SDL-E1074",
            Diag::ExpectedClauseValue { .. } => "SDL-E1010",
            Diag::UnexpectedClauseForm { .. } => "SDL-E1011",
            Diag::InvalidDuration { .. } => "SDL-E1012",
            Diag::RequiredClauseMissing { .. } => "SDL-E1013",
            Diag::UnknownClauseWord { .. } => "SDL-E1014",
            Diag::MutuallyExclusive { .. } => "SDL-E1015",
            Diag::InvalidString { .. } => "SDL-E1016",
            Diag::InvalidFilter { .. } => "SDL-E1017",
            Diag::InvalidFnPath { .. } => "SDL-E1018",
            Diag::InvalidIdent { .. } => "SDL-E1019",
            Diag::InvalidNumber { .. } => "SDL-E1020",
            Diag::InvalidExecutable { .. } => "SDL-E1021",
            Diag::ModuleExecutableName { .. } => "SDL-E1022",
            Diag::InvalidFunctionSignature { .. } => "SDL-E1023",
            Diag::FunctionParamNeedsDollar { .. } => "SDL-E1024",
            Diag::FnNameDoubledPrefix { .. } => "SDL-E1075",
            Diag::FunctionParamTrailingComma { .. } => "SDL-E1025",
            Diag::InvalidKind { .. } => "SDL-E1026",
            Diag::UnknownVariant { .. } => "SDL-E1027",
            Diag::InvalidMiddleware { .. } => "SDL-E1028",
            Diag::InvalidIndexFields { .. } => "SDL-E1029",
            Diag::InvalidIndexParam { .. } => "SDL-E1030",
            Diag::InvalidFloat { .. } => "SDL-E1031",
            Diag::InvalidBm25 { .. } => "SDL-E1032",
            Diag::InvalidDistance { .. } => "SDL-E1033",
            Diag::InvalidApiPath { .. } => "SDL-E1034",
            Diag::UnknownApiMethod { .. } => "SDL-E1035",
            Diag::DuplicateApiMethod { .. } => "SDL-E1036",
            Diag::DuplicateAnyBlock { .. } => "SDL-E1037",
            Diag::MissingApiThen { .. } => "SDL-E1038",
            Diag::InvalidAccessSource { .. } => "SDL-E1039",
            Diag::InvalidAccessClause { .. } => "SDL-E1040",
            Diag::InvalidAccessLevel { .. } => "SDL-E1041",
            Diag::InvalidAccessIssuer { .. } => "SDL-E1042",
            Diag::InvalidTableHeader { .. } => "SDL-E1043",
            Diag::InvalidTableClause { .. } => "SDL-E1044",
            Diag::InvalidTableParam { .. } => "SDL-E1057",
            Diag::InvalidPassscram { .. } => "SDL-E1059",
            Diag::InvalidFieldInline { .. } => "SDL-E1058",
            Diag::InvalidPermissionsShorthand { .. } => "SDL-E1045",
            Diag::InvalidNestedOn { .. } => "SDL-E1046",
            Diag::InvalidViewBody { .. } => "SDL-E1047",
            Diag::FieldTypeRequired { .. } => "SDL-E1048",
            Diag::InvalidFieldComputed { .. } => "SDL-E1049",
            Diag::InvalidFieldFlexible { .. } => "SDL-E1050",
            Diag::InvalidFieldReference { .. } => "SDL-E1051",
            Diag::InvalidFieldId { .. } => "SDL-E1052",
            Diag::FieldDeletePermission { .. } => "SDL-E1053",
            Diag::InvalidFieldPath { .. } => "SDL-E1054",
            Diag::RawBodyExpected { .. } => "SDL-E1055",
            Diag::AccessSecretExposure { .. } => "SDL-E1056",
            Diag::InfoUnparsable { .. } => "SDL-E3001",
            Diag::InfoUnknownShape { .. } => "SDL-E3002",
            Diag::DdlTripwire { .. } => "SDL-E3003",
            Diag::InfoDepthExceeded { .. } => "SDL-E3004",
            Diag::InterchangeTagInvalid { .. } => "SDL-E3005",
        }
    }

    /// The spec file this code is defined by.
    fn url_str(&self) -> &'static str {
        match self {
            Diag::FileUnreadable { .. } | Diag::FileIsADirectory { .. } => "spec/README.md",
            Diag::FileUnwritable { .. } => "spec/README.md",
            // The refusal is fmt's file-entry contract (the same
            // front-of-pipeline class as the file diagnostics): the
            // migration format itself is documented with the migrate
            // command, and the design rules' file-entry posture is
            // README's.
            Diag::MigrationScriptRefused { .. } => "spec/README.md",
            Diag::WriteBackUnplaceable { .. } => "spec/include.md",
            Diag::DuplicateInclude { .. } => "spec/include.md",
            Diag::IncludePathAbsolute { .. } => "spec/include.md",
            // The kind rule is README's (rule 4) — `function` is only
            // its first consumer.
            Diag::InvalidKind { .. } => "spec/README.md#design-rules-settled",
            Diag::InvalidFunctionSignature { .. }
            | Diag::FunctionParamNeedsDollar { .. }
            | Diag::FnNameDoubledPrefix { .. }
            | Diag::FunctionParamTrailingComma { .. } => "spec/function.md",
            Diag::UnknownVariant { .. } | Diag::InvalidMiddleware { .. } => "spec/config.md",
            Diag::InvalidIndexFields { .. }
            | Diag::InvalidIndexParam { .. }
            | Diag::InvalidFloat { .. }
            | Diag::InvalidBm25 { .. }
            | Diag::InvalidDistance { .. } => "spec/index.md",
            Diag::InvalidApiPath { .. }
            | Diag::UnknownApiMethod { .. }
            | Diag::DuplicateApiMethod { .. }
            | Diag::DuplicateAnyBlock { .. }
            | Diag::MissingApiThen { .. } => "spec/api.md",
            Diag::InvalidAccessSource { .. }
            | Diag::InvalidAccessClause { .. }
            | Diag::InvalidAccessLevel { .. }
            | Diag::InvalidAccessIssuer { .. }
            | Diag::AccessSecretExposure { .. } => "spec/access.md",
            Diag::InvalidTableHeader { .. }
            | Diag::InvalidTableClause { .. }
            | Diag::InvalidTableParam { .. }
            | Diag::InvalidNestedOn { .. }
            | Diag::InvalidViewBody { .. } => "spec/table.md",
            Diag::UnknownMixin { .. }
            | Diag::DuplicateSpread { .. }
            | Diag::SpreadMisplaced { .. }
            | Diag::DuplicateMemberKey { .. } => "spec/mixin.md",
            // The generalized duplicate rule, the cycle rule, and the
            // cross-section rule live in the design rules (rule 7,
            // the declarative namespace).
            Diag::DuplicateDefinition { .. }
            | Diag::DefineCycle { .. }
            | Diag::CrossSectionDependency { .. } => "spec/README.md#design-rules-settled",
            Diag::IncludeUnsupported { .. } | Diag::IncludeMisplaced { .. } => "spec/include.md",
            Diag::IncludeScopeBanned { .. } => "spec/include.md",
            Diag::UnknownAlias { .. } | Diag::InvalidAliasName { .. } => "spec/type-alias.md",
            Diag::AliasExpansionDepth { .. } | Diag::AliasExpansionInvalid { .. } => {
                "spec/type-alias.md"
            }
            Diag::InvalidPassscram { .. } => "spec/user.md",
            Diag::InvalidPermissionsShorthand {
                statement: "field", ..
            } => "spec/field.md",
            Diag::InvalidPermissionsShorthand { .. } => "spec/table.md",
            Diag::FieldTypeRequired { .. }
            | Diag::InvalidFieldComputed { .. }
            | Diag::InvalidFieldFlexible { .. }
            | Diag::InvalidFieldReference { .. }
            | Diag::InvalidFieldId { .. }
            | Diag::InvalidFieldInline { .. }
            | Diag::FieldDeletePermission { .. }
            | Diag::InvalidFieldPath { .. } => "spec/field.md",
            Diag::InfoUnparsable { .. }
            | Diag::InfoUnknownShape { .. }
            | Diag::DdlTripwire { .. }
            | Diag::InfoDepthExceeded { .. }
            | Diag::InterchangeTagInvalid { .. }
            // The violated contract is the codegen output model: the
            // emission parses.
            | Diag::EmbeddedDdlRejected { .. } => "spec/README.md#codegen-output-model",
            _ => "spec/README.md#design-rules-settled",
        }
    }

    /// One-line fix suggestion.
    fn help_message(&self) -> String {
        match self {
            Diag::UnterminatedString { .. } => {
                "add the closing quote; SurrealQL `\\` escapes (including \
                 `\\'`) are honored while scanning"
                    .to_string()
            }
            Diag::UnterminatedRegex { .. } => {
                "add the closing `/` — an unescaped `/` closes the literal, \
                 and `\\` escapes the next byte"
                    .to_string()
            }
            Diag::UnterminatedBlockComment { .. } => {
                "add `*/`; block comments do not nest and end at the first \
                 `*/`"
                    .to_string()
            }
            Diag::UnterminatedQuotedIdent { closer, .. } => {
                format!(
                    "add the closing `{closer}`; `\\` escapes are honored inside quoted identifiers"
                )
            }
            Diag::UnterminatedCapture { terminator, .. } => {
                format!("add `{terminator}`")
            }
            Diag::UnbalancedCapture { .. } => {
                "remove it, or move it inside a balanced bracket group".to_string()
            }
            Diag::InvalidIdentEscape { .. } => {
                "valid escapes are `\\n`, `\\r`, `\\t`, `\\0`, `\\\\`, \
                 `\\b`, `\\f`, `\\'`, `\\\"`, `\\`` and `\\u{…}`"
                    .to_string()
            }
            Diag::DanglingDocComment { .. } => {
                "doc comments attach to the definition directly below \
                 them (no blank line); move this block or delete it"
                    .to_string()
            }
            Diag::FileUnreadable { span: None, .. } => {
                "check the path; the file must exist and be valid UTF-8".to_string()
            }
            Diag::FileIsADirectory { .. } => {
                "that path is a directory — pass the schema file's path".to_string()
            }
            Diag::FileUnreadable {
                span: Some(_),
                path,
                ..
            } => {
                format!(
                    "the path resolves relative to the including file's directory — \
                     check that `{path}` exists there (create it, or fix the include's \
                     spelling; `..` segments may climb, absolute paths never resolve)"
                )
            }
            Diag::FileUnwritable { .. } => "check the path's directory and permissions".to_string(),
            Diag::MigrationScriptRefused { .. } => {
                "the migration pipeline owns this file — format the schema it \
                 converges to instead (the header's `-- source:` file), and run \
                 the script itself through any SurrealQL tooling"
                    .to_string()
            }
            Diag::WriteBackUnplaceable { statement, .. } => {
                format!(
                    "place the {statement} by hand, then re-run normalize — the \
                     write-back never guesses a position: a scope statement belongs \
                     in the root's preamble (its position re-binds every scope below \
                     it), and an adoptee a composed file still depends on cannot move \
                     past that file's include"
                )
            }
            Diag::IncludePathAbsolute { .. } => {
                "write the path relative to this file's directory — a schema tree \
                 must stay portable (`..` segments may climb)"
                    .to_string()
            }
            Diag::IncludeScopeBanned { statement, .. } => {
                format!(
                    "move this {statement} to the composition root's preamble — an \
                     included file carries no scopes, because one inside a splice \
                     would re-scope the importing file below the splice"
                )
            }
            Diag::UnknownStatement { suggestion, .. } => match suggestion {
                Some(s) => format!("did you mean `{s}`?"),
                None => "the statement keywords SDL defines are listed in \
                 the linked spec"
                    .to_string(),
            },
            Diag::IllegalCharacter { .. } => {
                "SDL statements begin with a definition keyword; outside \
                 captures the only comments are `//` and `///`"
                    .to_string()
            }
            Diag::UnknownClause {
                key,
                statement,
                suggestion,
                ..
            } if key == "raw" => {
                if *statement == "table" {
                    "`raw { … };` is a top-level statement — in block context `raw` \
                     is a reserved word; for a field named `raw` write the `field` \
                     prefix or quote it (`raw`: …)"
                        .to_string()
                } else {
                    "`raw { … };` is a top-level statement — `raw` is reserved in \
                     block context and is not a clause of this block"
                        .to_string()
                }
            }
            Diag::UnknownClause { suggestion, .. } => match suggestion {
                Some(s) => format!("did you mean `{s}`?"),
                None => "remove it; each statement's clause list is in its spec file".to_string(),
            },
            Diag::ExpectedName { .. } => {
                "names are bare words or quoted identifiers (`` `backtick` `` \
                 or `⟨angle⟩`)"
                    .to_string()
            }
            Diag::ExpectedStatementEnd { .. } => {
                "add the `;` this position requires (after a name, `{` opens its block instead)"
                    .to_string()
            }
            Diag::UnterminatedBlock { .. } => "add the closing `}`".to_string(),
            Diag::ReservedIdent { word, .. } => {
                format!(
                    "backtick it (`` `{word}` ``) or choose another name; \
                     SurrealQL reserves these words"
                )
            }
            Diag::DocCommentUnsupported { statement, .. } => {
                format!(
                    "`{statement}` has no COMMENT clause in SurrealQL, so \
                     a doc comment has nowhere to attach — delete it or \
                     move it onto the definition above/below that takes one"
                )
            }
            Diag::EmbeddedParseError { .. } => {
                "this clause takes a SurrealQL expression; the official \
                 parser's rejection is quoted above"
                    .to_string()
            }
            Diag::EmbeddedDdlRejected { .. } => {
                "this is a Surform bug: the SDL parsed clean, but the DDL \
                 it emitted does not parse. Please report it with this \
                 file's contents"
                    .to_string()
            }
            Diag::ExpectedClauseValue { key, .. } => {
                format!("write `{key}: <expression>;` — the `:` starts the value")
            }
            Diag::UnexpectedClauseForm { .. } => {
                "the separator picks the clause form: `key: <value>;`, bare \
                 `key;` (flag), or `key { … };` (sub-block) — spec rule 2"
                    .to_string()
            }
            Diag::InvalidDuration { .. } => {
                "SurrealQL durations are an integer plus a unit (`7d`), or \
                 a compound (`1h30m`); units: ns, us, µs, ms, s, m, h, d, \
                 w, y — integers only, no sign or fraction"
                    .to_string()
            }
            Diag::RequiredClauseMissing { clause, block, .. } => {
                // A flag arrives spelled with its `;` (nothing can
                // follow it); a value clause takes the value hint.
                if clause.ends_with(';') {
                    format!("add `{clause}` inside the `{block}` block")
                } else {
                    format!("add `{clause}: …;` inside the `{block}` block")
                }
            }
            Diag::UnknownClauseWord {
                key, suggestion, ..
            } => match suggestion {
                Some(s) => format!("did you mean `{s}`?"),
                None => format!(
                    "the `{key}` clause takes one of a fixed word set — SDL \
                     keyword values are lowercase only (rule 9)"
                ),
            },
            Diag::MutuallyExclusive { clause, other, .. } => {
                format!("remove one — `{clause}` and `{other}` cannot both be set")
            }
            Diag::InvalidString { .. } => {
                "string literals are single- or double-quoted with SurrealQL `\\` \
                 escapes (`\\n`, `\\t`, `\\'`, `\\\"`, `\\\\`, `\\u{…}` …)"
                    .to_string()
            }
            Diag::InvalidFilter { .. } => {
                "filters are `ascii`, `lowercase`, `uppercase`, `edgengram(<min>, \
                 <max>)`, `ngram(<min>, <max>)`, `snowball(<language>)` with a \
                 bare language keyword, or `mapper('<path>')`"
                    .to_string()
            }
            Diag::InvalidFnPath { .. } => "an analyzer function is a name path — `fn::name`, \
                 `fn::module::name`, or `mod::module[::name…]` — never a call"
                .to_string(),
            Diag::InvalidIdent { .. } => "this clause takes one bare word or a quoted identifier \
                 (`` `backtick` `` or `⟨angle⟩`)"
                .to_string(),
            Diag::InvalidNumber { range, .. } => {
                format!("whole numbers only — {range}, no sign, fraction, or unit")
            }
            Diag::InvalidExecutable { .. } => {
                "the `from` clause takes an f-string (`f'bucket:/key.surli'`) or \
                 a Silo package (`silo::org::pkg::<1.2.0>`)"
                    .to_string()
            }
            Diag::ModuleExecutableName { .. } => {
                "the header name must fit the executable: file modules take \
                 `module <name> { … };`, silo modules take none"
                    .to_string()
            }
            Diag::InvalidFunctionSignature { .. } => {
                "a function is `fn <name>($<param>: <kind>, …) [-> <kind>] \
                 { <body> } [{ <clauses> };]` — spec/function.md"
                    .to_string()
            }
            Diag::FunctionParamNeedsDollar { .. } => {
                "parameters keep their `$` — SurrealQL requires it (spec/function.md)".to_string()
            }
            Diag::FnNameDoubledPrefix { .. } => {
                "the header's path is bare — the `fn` keyword already carries the \
                 `fn::` prefix, so drop it from the name (`fn weird(…)`, never \
                 `fn fn::weird(…)`); a `raw` body carries the SurrealQL spelling \
                 if the doubled name is really wanted"
                    .to_string()
            }
            Diag::FunctionParamTrailingComma { .. } => {
                "remove the trailing `,`; SurrealQL accepts one but INFO never \
                 emits it, so SDL is deliberately stricter"
                    .to_string()
            }
            Diag::InvalidKind { .. } => "kinds mirror SurrealQL's own: `option<T>`, `record<T>`, \
                 `array<T, N>`, unions (`int | float`), literal kinds — \
                 spec/README.md rule 4"
                .to_string(),
            Diag::UnknownVariant { suggestion, .. } => match suggestion {
                Some(s) => format!("did you mean `{s}`?"),
                None => "the header variant is one of `api`, `graphql`, or \
                     `default` — spec/config.md"
                    .to_string(),
            },
            Diag::InvalidMiddleware { .. } => {
                "a middleware entry is a call — `name(args)`, `fn::name(args)`, \
                 or `mod::pkg::name(args)`; use `()` for no arguments"
                    .to_string()
            }
            Diag::InvalidIndexFields { .. } => {
                "fulltext, hnsw, and diskann indexes each index exactly \
                 one field; plain and unique indexes take one or more — \
                 spec/index.md"
                    .to_string()
            }
            Diag::InvalidIndexParam { .. } => {
                "each index kind constrains its own parameters — `m` ≤ 127, \
                 `m0` 0–255, DISKANN's distance/type domains and positive \
                 `degree`/`l_build` — spec/index.md"
                    .to_string()
            }
            Diag::InvalidFloat { .. } => "unsigned decimal numbers only — digits with `_` \
                 separators, an optional `.` fraction, an optional `e` \
                 exponent (`1.2`, `1_000.5e-2`, `3f`); no sign, no \
                 `NaN`/`Infinity` spellings, and magnitudes under 2⁶³ \
                 (the emitted DDL prints the value bare, so it must \
                 re-parse upstream unchanged)"
                .to_string(),
            Diag::InvalidBm25 { .. } => "write `bm25: (<k1>, <b>);` — the parens are mandatory \
                 and exactly two numbers are taken, e.g. `bm25: (1.2, 0.75);`"
                .to_string(),
            Diag::InvalidDistance { .. } => {
                "distances are `euclidean`, `cosine`, `cosine_normalized`, \
                 `chebyshev`, `hamming`, `jaccard`, `pearson`, \
                 `inner_product`, `manhattan`, or `minkowski(<order>)` — \
                 only minkowski takes an argument"
                    .to_string()
            }
            Diag::InvalidApiPath { .. } => {
                "the route path is a single string literal — `api '/users/:id' \
                 { … };`; SurrealDB validates the path grammar (static, `:param`, \
                 `*wildcard` segments) at define time"
                    .to_string()
            }
            Diag::UnknownApiMethod {
                word, suggestion, ..
            } if word == "raw" => {
                "`raw { … };` is a top-level statement — api sub-block heads are \
                 the methods (or one `any` block), never `raw`"
                    .to_string()
            }
            Diag::UnknownApiMethod { suggestion, .. } => match suggestion {
                Some(s) => format!("did you mean `{s}`?"),
                None => "sub-block heads are the methods `get`, `post`, `put`, \
                     `patch`, `delete`, `trace` (comma-grouped, any order) or \
                     one `any` block — spec/api.md"
                    .to_string(),
            },
            Diag::DuplicateApiMethod { .. } => {
                "merge the groups — `get, delete { … };` — or give each \
                 method its own group; a method may appear in only one"
                    .to_string()
            }
            Diag::DuplicateAnyBlock { .. } => {
                "merge the blocks — `any` is endpoint-wide and appears at \
                 most once; SurrealQL folds a repeated `FOR ANY` config-last-wins, \
                 which SDL rejects as ambiguous"
                    .to_string()
            }
            Diag::MissingApiThen { .. } => "every method group takes a `then:` response — e.g. \
                 `then: { { status: 200 } };`; only `any` may omit it \
                 (that is the no-fallback form)"
                .to_string(),
            Diag::InvalidAccessSource { .. } => {
                "the verification source is exactly one of `url: <expr>;` (a JWKS endpoint) \
                 or `algorithm: <word>;` + `key: <expr>;` (a local key)"
                    .to_string()
            }
            Diag::InvalidAccessClause { .. } => {
                "each `access` variant defines its own clauses — check `type:` and the \
                 variant's clause list in the linked spec"
                    .to_string()
            }
            Diag::InvalidAccessLevel { .. } => {
                "record accesses (and bearer accesses for records) live at the database \
                 level — write `on: database;` (or `on: db;`)"
                    .to_string()
            }
            Diag::InvalidAccessIssuer { .. } => {
                "by local key the issuer must restate the verification algorithm and its \
                 own `key:` — without a key SurrealDB materializes a random one"
                    .to_string()
            }
            Diag::AccessSecretExposure { .. } => {
                "write the literal `key: '[REDACTED]';` in SDL and provision the real \
                 secret outside SDL (this access class is run manually, so its secrets \
                 are never committed); asymmetric verify keys are public and stay \
                 authorable"
                    .to_string()
            }
            Diag::UnknownMixin { suggestion, .. } => match suggestion {
                Some(s) => format!(
                    "did you mean `{s}`? define the mixin anywhere in this \
                     document — spreads resolve against the whole composition, \
                     any file, any position"
                ),
                None => "define the mixin anywhere in this document — spreads \
                 resolve against the whole composition, any file, any \
                 position; a spread that names nothing is an error, never a \
                 silent empty expansion"
                    .to_string(),
            },
            Diag::UnknownAlias { suggestion, .. } => match suggestion {
                Some(s) => format!(
                    "did you mean `{s}`? declare the alias anywhere in this \
                     document — `type {s} = <kind>;` — references resolve \
                     against the whole composition, any file, any position"
                ),
                None => "declare the alias anywhere in this document — \
                 `type <Name> = <kind>;` — references resolve against the \
                 whole composition, any file, any position; a reference \
                 that names nothing is an error, never a silent kind"
                    .to_string(),
            },
            Diag::InvalidAliasName { .. } => {
                "rename the alias — a use site is a bare Capitalized word, so \
                 the name must start uppercase and must not be a SurrealQL \
                 kind head"
                    .to_string()
            },
            Diag::AliasExpansionDepth { .. } => {
                "shorten the chain — each link adds its nesting to the whole, \
                 and the expansion the database receives must stay within the \
                 ceiling every kind walk can traverse"
                    .to_string()
            },
            Diag::AliasExpansionInvalid { .. } => {
                "each link is legal on its own, but the composition crosses the \
                 grammar — e.g. a chain of option wrappers composes \
                 option<option<…>>, which no parser accepts; flatten the chain \
                 or drop the wrapping link"
                    .to_string()
            },
            Diag::DuplicateDefinition { family, .. } => {
                format!(
                    "definitions are unique across the document — any file, \
                     any position, this {family} family included: rename one of \
                     the two, or delete the definition you meant to replace; \
                     OVERWRITE remains only the emitted DDL's re-apply default"
                )
            }
            Diag::DefineCycle { .. } => {
                "break the cycle — a view's top `FROM` chain and a param's \
                 `VALUE` chain are the two classes the engine evaluates at \
                 define time and cannot apply in any order; every other \
                 reference (fn bodies, permissions, events, field defaults) \
                 is runtime-evaluated and stays edge-free"
                    .to_string()
            }
            Diag::CrossSectionDependency { target, .. } => {
                format!(
                    "move {target} above this section's last \
                     `namespace`/`database` statement — or this statement \
                     below the target's; the compiler reorders within a \
                     scope section, never across one"
                )
            }
            Diag::DuplicateSpread { .. } => {
                "remove the second spread — every member defines once through one \
                 `...name;` entry"
                    .to_string()
            }
            Diag::SpreadMisplaced { statement, .. } => {
                format!(
                    "spread entries live in non-view `table` blocks only — move this \
                     one into the table that should carry the mixin's fields (this \
                     {statement} block takes none)"
                )
            }
            Diag::DuplicateMemberKey { kind, key, .. } => {
                format!(
                    "this body defines the {kind} `{key}` twice — the engine's \
                     last-one-wins would silently hide one of them; keep one or make \
                     the keys distinct"
                )
            }
            Diag::IncludeUnsupported { .. } => {
                "file composition resolves on a real path through the CLI — \
                 this context parses inline text and has no file system to \
                 resolve the path against"
                    .to_string()
            }
            Diag::IncludeMisplaced { .. } => {
                "move namespace/database above the file's first include — they \
                 scope everything below them, the composed content included \
                 (includes themselves are legal anywhere in the file)"
                    .to_string()
            }
            Diag::DuplicateInclude { first, .. } => {
                format!(
                    "remove the second include — `{first}` already spliced this \
                     file, and a second splice would define its statements \
                     twice (if the two declarations serve different paths, \
                     share the file through one of them)"
                )
            }
            Diag::InvalidTableHeader { .. } => {
                "a relation header is `table [<from> | …] -> <name> -> [<to> | …]` — \
                 both arrows are mandatory, the name always between them"
                    .to_string()
            }
            Diag::InvalidTableClause { .. } => {
                "each `table` kind defines its own clauses — check the header kind and \
                 the clause list in the linked spec"
                    .to_string()
            }
            Diag::InvalidTableParam { .. } => {
                "each `table` clause constrains its own value — the INLINE cache caps take \
                 0\u{2013}256 (the engine rejects anything larger at define time too)"
                    .to_string()
            }
            Diag::InvalidPassscram { .. } => {
                "a passscram verifier is `SCRAM-SHA-256$<iterations>:<salt>$<stored key>:<server key>` \
                 with base64 parts (the engine's parser validates the shape too)"
                    .to_string()
            }
            Diag::InvalidPermissionsShorthand { statement, .. } => format!(
                "{statement} permissions take `permissions: none;`, `permissions: full;`, \
                 or a per-operation `permissions {{ select: …; … }};` block — no expression"
            ),
            Diag::InvalidNestedOn { statement, .. } => {
                format!(
                    "remove the `on:` clause — this {statement} inherits its host table \
                     from the `table` block; the standalone `on:` is for the top-level form"
                )
            }
            Diag::InvalidViewBody { .. } => {
                "a view's block is its query — write `table view <name> {{ select …; }};`, \
                 with any clauses after the query"
                    .to_string()
            }
            Diag::FieldTypeRequired { .. } => {
                "a field's type sits in the header — `name: <type>;` — or give it a \
                 `computed:` expression; SurrealQL's typeless form is reachable \
                 through `raw { … };`"
                    .to_string()
            }
            Diag::InvalidFieldComputed { .. } => {
                "a computed field takes nothing else — drop the other clauses, or move \
                 the expression into a `computed:` field and keep `value`/`assert` on \
                 a regular one"
                    .to_string()
            }
            Diag::InvalidFieldFlexible { .. } => {
                "`flexible;` needs a type containing an object — `object`, an inline \
                 schema (`{ … }`), or an `array`/`set`/union of such"
                    .to_string()
            }
            Diag::InvalidFieldReference { .. } => {
                "`reference` is a top-level field's clause — define it on the root \
                 field (`author: record<user> { reference; };`), not on a nested path"
                    .to_string()
            }
            Diag::InvalidFieldId { .. } => {
                "the `id` field takes only `type:`, `assert:`, and plain `default:` — \
                 the other clauses contradict the record's identity"
                    .to_string()
            }
            Diag::InvalidFieldInline { .. } => {
                "`inline;` embeds a field's value into its table's edge-adjacency \
                 payloads — it needs a TYPE RELATION host, a top-level non-`in`/`out` \
                 name, and a stored (not computed) value"
                    .to_string()
            }
            Diag::FieldDeletePermission { .. } => {
                "fields cannot be deleted, so there is no `delete` permission — SurrealQL \
                 rejects it; unlisted operations are FULL on fields"
                    .to_string()
            }
            Diag::InvalidFieldPath { .. } => {
                "field paths are `.`-joined name segments (`stats.views`) with `.*` the \
                 array wildcard (`tags.*`) — the path starts with a name"
                    .to_string()
            }
            Diag::RawBodyExpected { .. } => {
                "`raw { … };` captures SurrealQL DDL verbatim between the braces — \
                 write `{` right after the keyword"
                    .to_string()
            }
            Diag::InfoUnparsable { .. } => {
                "the input must be rendered INFO output — a SurrealQL object whose \
                 DDL strings each parse as one statement (query `INFO FOR ROOT/NS/DB` \
                 and feed the rendered result)"
                    .to_string()
            }
            Diag::InfoUnknownShape { .. } => {
                "this reader maps the server's own INFO shapes — capture fresh INFO \
                 output from the engine and feed that"
                    .to_string()
            }
            Diag::DdlTripwire { .. } => {
                "the definition re-printed outside the pinned printer dialect — if the \
                 embedded parser version moved, re-verify the read path (a deliberate \
                 spec event); otherwise the input is not real INFO output"
                    .to_string()
            }
            Diag::InfoDepthExceeded { .. } => {
                "real INFO output nests ~3 levels — this input is not engine output; \
                 feed rendered INFO results"
                    .to_string()
            }
            Diag::InterchangeTagInvalid { help, .. } => help
                .unwrap_or(
                    "scope tags must mirror the SDL file's own `namespace`/`database` \
                     declarations — normalize a file against the INFO output of the \
                     database it defines",
                )
                .to_string(),
        }
    }

    /// The primary highlight: label text plus the span it marks.
    /// `None` for span-less diagnostics. `pub(crate)`: the DDL
    /// readers re-anchor scanner diagnostics onto their own texts.
    pub(crate) fn primary_label(&self) -> Option<(String, Span)> {
        match self {
            Diag::UnterminatedString { span } => Some(("string starts here".to_string(), *span)),
            Diag::UnterminatedRegex { span } => Some(("regex starts here".to_string(), *span)),
            Diag::UnterminatedBlockComment { span } => {
                Some(("comment starts here".to_string(), *span))
            }
            Diag::UnterminatedQuotedIdent { span, .. } => {
                Some(("identifier starts here".to_string(), *span))
            }
            Diag::UnterminatedCapture { span, .. } => {
                Some(("capture starts here".to_string(), *span))
            }
            Diag::UnbalancedCapture { span, .. } => Some(("closes nothing".to_string(), *span)),
            Diag::InvalidIdentEscape { span } => {
                Some(("not a valid escape character".to_string(), *span))
            }
            Diag::DanglingDocComment { span } => Some(("doc block starts here".to_string(), *span)),
            Diag::FileUnreadable { span: None, .. } | Diag::FileIsADirectory { .. } => None,
            Diag::FileUnreadable {
                span: Some(span), ..
            } => Some((
                "this include's file is missing or unreadable".to_string(),
                *span,
            )),
            Diag::FileUnwritable { .. } => None,
            Diag::MigrationScriptRefused { span } => {
                Some(("the migration draft header".to_string(), *span))
            }
            Diag::UnknownStatement { span, .. } => {
                Some(("not a statement keyword".to_string(), *span))
            }
            Diag::IllegalCharacter { span, .. } => {
                Some(("cannot start a statement or clause".to_string(), *span))
            }
            Diag::UnknownClause { span, .. } => Some(("unknown clause key".to_string(), *span)),
            Diag::ExpectedName { span } => Some(("expected identifier here".to_string(), *span)),
            Diag::ExpectedStatementEnd { span, .. } => {
                Some(("expected `;` or `{`".to_string(), *span))
            }
            Diag::UnterminatedBlock { span } => Some(("block starts here".to_string(), *span)),
            Diag::ReservedIdent { span, .. } => Some(("reserved word".to_string(), *span)),
            Diag::DocCommentUnsupported { span, .. } => {
                Some(("no COMMENT clause on this statement".to_string(), *span))
            }
            Diag::EmbeddedParseError { span, .. } => Some((
                "the embedded parser rejected this expression".to_string(),
                *span,
            )),
            Diag::EmbeddedDdlRejected { span, .. } => Some(("the compiled DDL".to_string(), *span)),
            Diag::ExpectedClauseValue { span, .. } => {
                Some(("expected `:` and a value".to_string(), *span))
            }
            Diag::UnexpectedClauseForm { span, .. } => {
                Some(("this clause takes a different form".to_string(), *span))
            }
            Diag::InvalidDuration { span, .. } => {
                Some(("not a SurrealQL duration".to_string(), *span))
            }
            Diag::RequiredClauseMissing { span, .. } => {
                Some(("required clause missing here".to_string(), *span))
            }
            Diag::UnknownClauseWord { span, .. } => {
                Some(("not a spelling this clause defines".to_string(), *span))
            }
            Diag::MutuallyExclusive { span, .. } => Some((
                "cannot be set together with the earlier clause".to_string(),
                *span,
            )),
            Diag::InvalidString { span, .. } => {
                Some(("not a SurrealQL string literal".to_string(), *span))
            }
            Diag::InvalidFilter { span, .. } => Some(("not a SurrealQL filter".to_string(), *span)),
            Diag::InvalidFnPath { span, .. } => {
                Some(("not a well-formed function path".to_string(), *span))
            }
            Diag::InvalidIdent { span, .. } => Some(("not a single identifier".to_string(), *span)),
            Diag::InvalidNumber { span, .. } => Some(("not a whole number".to_string(), *span)),
            Diag::InvalidExecutable { span, .. } => {
                Some(("not a module executable".to_string(), *span))
            }
            Diag::ModuleExecutableName { span, .. } => {
                Some(("module name does not fit its executable".to_string(), *span))
            }
            Diag::InvalidFunctionSignature { span, .. } => {
                Some(("malformed function signature".to_string(), *span))
            }
            Diag::FunctionParamNeedsDollar { span } => {
                Some(("parameter name without `$`".to_string(), *span))
            }
            Diag::FnNameDoubledPrefix { span } => {
                Some(("doubled `fn::` prefix".to_string(), *span))
            }
            Diag::FunctionParamTrailingComma { span } => {
                Some(("trailing comma".to_string(), *span))
            }
            Diag::InvalidKind { span, .. } => Some(("not a SurrealQL kind".to_string(), *span)),
            Diag::UnknownVariant { span, .. } => Some(("not a config variant".to_string(), *span)),
            Diag::InvalidMiddleware { span, .. } => {
                Some(("not a well-formed middleware call".to_string(), *span))
            }
            Diag::InvalidIndexFields { span, .. } => {
                Some(("wrong field count for this index kind".to_string(), *span))
            }
            Diag::InvalidIndexParam { span, .. } => {
                Some(("outside this kind's domain".to_string(), *span))
            }
            Diag::InvalidFloat { span, .. } => Some(("not a decimal number".to_string(), *span)),
            Diag::InvalidBm25 { span, .. } => Some(("not a `(<k1>, <b>)` pair".to_string(), *span)),
            Diag::InvalidDistance { span, .. } => {
                Some(("not a distance keyword".to_string(), *span))
            }
            Diag::InvalidApiPath { span, .. } => {
                Some(("the route path is a string literal".to_string(), *span))
            }
            Diag::UnknownApiMethod { span, .. } => {
                Some(("not a sub-block head".to_string(), *span))
            }
            Diag::DuplicateApiMethod { span, .. } => {
                Some(("method already grouped".to_string(), *span))
            }
            Diag::DuplicateAnyBlock { span } => {
                Some(("`any` is already defined".to_string(), *span))
            }
            Diag::MissingApiThen { span } => Some(("this group has no `then:`".to_string(), *span)),
            Diag::InvalidAccessSource { span, .. } => {
                Some(("the verification source is malformed".to_string(), *span))
            }
            Diag::InvalidAccessClause { span, .. } => {
                Some(("not this variant's clause".to_string(), *span))
            }
            Diag::InvalidAccessLevel { span, .. } => {
                Some(("this variant needs `on: database`".to_string(), *span))
            }
            Diag::InvalidAccessIssuer { span, .. } => {
                Some(("this issuer block is invalid".to_string(), *span))
            }
            Diag::AccessSecretExposure { span, .. } => {
                Some(("this key slot takes only `'[REDACTED]'`".to_string(), *span))
            }
            Diag::UnknownMixin { span, .. } => {
                Some(("no mixin of this name in the document".to_string(), *span))
            }
            Diag::UnknownAlias { span, .. } => Some((
                "no type alias of this name in the document".to_string(),
                *span,
            )),
            Diag::InvalidAliasName { span, .. } => {
                Some(("this cannot name a type alias".to_string(), *span))
            }
            Diag::AliasExpansionDepth { span, .. } => {
                Some(("this expansion nests too deep".to_string(), *span))
            }
            Diag::AliasExpansionInvalid { span, .. } => Some((
                "this expansion is not an expressible kind".to_string(),
                *span,
            )),
            Diag::DuplicateDefinition { family, span, .. } => {
                Some((format!("this {family} name is taken"), *span))
            }
            Diag::DefineCycle { span, .. } => {
                Some(("this definition closes a cycle".to_string(), *span))
            }
            Diag::CrossSectionDependency { span, .. } => Some((
                "this reference cannot be reordered into place".to_string(),
                *span,
            )),
            Diag::DuplicateSpread { span, .. } => Some(("already spread above".to_string(), *span)),
            Diag::SpreadMisplaced { span, .. } => {
                Some(("not legal in this block".to_string(), *span))
            }
            Diag::DuplicateMemberKey { span, .. } => {
                Some(("this is the second definition".to_string(), *span))
            }
            Diag::IncludeUnsupported { span, .. } => {
                Some(("no file context here".to_string(), *span))
            }
            Diag::IncludeMisplaced { span, .. } => {
                Some(("the preamble precedes the includes".to_string(), *span))
            }
            Diag::DuplicateInclude { span, .. } => {
                Some(("this file is already included".to_string(), *span))
            }
            Diag::WriteBackUnplaceable { span, .. } => Some((
                "this composition cannot absorb the adopted statement".to_string(),
                *span,
            )),
            Diag::IncludePathAbsolute { span, .. } => {
                Some(("absolute paths never resolve".to_string(), *span))
            }
            Diag::IncludeScopeBanned { span, .. } => {
                Some(("scopes belong in the composition root".to_string(), *span))
            }
            Diag::InvalidTableHeader { span, .. } => {
                Some(("malformed relation header".to_string(), *span))
            }
            Diag::InvalidTableClause { span, .. } => {
                Some(("not this table kind's clause".to_string(), *span))
            }
            Diag::InvalidTableParam { span, .. } => {
                Some(("out of this clause's domain".to_string(), *span))
            }
            Diag::InvalidPassscram { span, .. } => {
                Some(("not a valid SCRAM verifier".to_string(), *span))
            }
            Diag::InvalidPermissionsShorthand { span, .. } => {
                Some(("not the `none | full` shorthand".to_string(), *span))
            }
            Diag::InvalidNestedOn { span, .. } => {
                Some(("a nested definition inherits its table".to_string(), *span))
            }
            Diag::InvalidViewBody { span, .. } => {
                Some(("the view's block starts with its query".to_string(), *span))
            }
            Diag::FieldTypeRequired { span } => Some((
                "the field needs a type or a computed expression".to_string(),
                *span,
            )),
            Diag::InvalidFieldComputed { span, .. } => {
                Some(("computed fields take nothing else".to_string(), *span))
            }
            Diag::InvalidFieldFlexible { span, .. } => {
                Some(("this type contains no object".to_string(), *span))
            }
            Diag::InvalidFieldReference { span, .. } => {
                Some(("reference is a top-level field's clause".to_string(), *span))
            }
            Diag::InvalidFieldId { span, .. } => {
                Some(("the `id` field forbids this clause".to_string(), *span))
            }
            Diag::InvalidFieldInline { span, .. } => {
                Some(("this field cannot be INLINE".to_string(), *span))
            }
            Diag::FieldDeletePermission { span } => {
                Some(("fields take no `delete` permission".to_string(), *span))
            }
            Diag::InvalidFieldPath { span, .. } => {
                Some(("malformed field path".to_string(), *span))
            }
            Diag::RawBodyExpected { span, .. } => {
                Some(("expected `{` to open the raw block".to_string(), *span))
            }
            Diag::InfoUnparsable { span, .. } => {
                Some(("not readable INFO input".to_string(), *span))
            }
            Diag::InfoUnknownShape { span, .. } => {
                Some(("not the shape this reader maps".to_string(), *span))
            }
            Diag::DdlTripwire { span, .. } => {
                Some(("outside the pinned printer dialect".to_string(), *span))
            }
            Diag::InfoDepthExceeded { span, .. } => Some((
                "object nesting exceeds the reader's budget".to_string(),
                *span,
            )),
            Diag::InterchangeTagInvalid { span, .. } => {
                Some(("invalid interchange scope tag".to_string(), *span))
            }
        }
    }
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Diag::UnterminatedString { .. } => write!(f, "unterminated string literal"),
            Diag::UnterminatedRegex { .. } => write!(f, "unterminated regex literal"),
            Diag::UnterminatedBlockComment { .. } => write!(f, "unterminated block comment"),
            Diag::UnterminatedQuotedIdent { closer, .. } => {
                write!(f, "unterminated quoted identifier: expected `{closer}`")
            }
            Diag::UnterminatedCapture { terminator, .. } => {
                write!(
                    f,
                    "unterminated capture: expected `{terminator}` before end of file"
                )
            }
            Diag::UnbalancedCapture { bracket, .. } => {
                write!(f, "unbalanced `{bracket}` in capture: it closes nothing")
            }
            Diag::InvalidIdentEscape { .. } => {
                write!(f, "invalid escape in quoted identifier")
            }
            Diag::DanglingDocComment { .. } => {
                write!(f, "doc comment attaches to no definition")
            }
            Diag::FileUnreadable { path, reason, .. } => {
                write!(f, "cannot read `{path}`: {reason}")
            }
            Diag::FileIsADirectory { path } => {
                write!(f, "cannot read `{path}`: it is a directory, not a file")
            }
            Diag::FileUnwritable { path, reason } => {
                write!(f, "cannot write `{path}`: {reason}")
            }
            Diag::MigrationScriptRefused { .. } => write!(
                f,
                "this file is a generated migration script — fmt does not \
                 reflow migration scripts"
            ),
            Diag::DuplicateInclude { path, first, .. } => {
                write!(
                    f,
                    "`{path}` is included twice by this file — first by `{first}`"
                )
            }
            Diag::WriteBackUnplaceable { statement, .. } => write!(
                f,
                "the merge adopts {statement} — no writable position exists in \
                 this composed project"
            ),
            Diag::IncludePathAbsolute { path, .. } => write!(
                f,
                "include path `{path}` is absolute — paths resolve relative \
                 to the including file's directory"
            ),
            Diag::IncludeScopeBanned { statement, .. } => write!(
                f,
                "an included file carries no {statement} — scopes are the \
                 composition root's preamble"
            ),
            Diag::UnknownStatement { word, .. } => {
                write!(f, "unknown statement keyword `{word}`")
            }
            Diag::IllegalCharacter { ch, .. } => {
                write!(f, "unexpected character `{}`", describe_char(*ch))
            }
            Diag::UnknownClause { key, statement, .. } => {
                write!(f, "unknown clause `{key}` in {statement} definition")
            }
            Diag::ExpectedName { .. } => write!(f, "expected a name"),
            Diag::ExpectedStatementEnd { found, .. } => match found {
                // Fires both after a name (where `{` opens the block)
                // and after a closed block or sub-block (where only
                // `;` terminates) — position-neutral wording.
                Some(ch) => write!(f, "expected `;` or `{{`, found `{}`", describe_char(*ch)),
                None => write!(f, "expected `;` or `{{`, found end of file"),
            },
            Diag::UnterminatedBlock { .. } => {
                write!(f, "unterminated block: expected `}}` before end of file")
            }
            Diag::ReservedIdent { word, .. } => {
                write!(f, "bare `{word}` cannot be an identifier (reserved word)")
            }
            Diag::DocCommentUnsupported { statement, .. } => {
                write!(f, "`{statement}` takes no doc comment (no COMMENT clause)")
            }
            Diag::EmbeddedParseError { message, .. } => {
                write!(
                    f,
                    "the embedded SurrealQL parser rejected this expression: {message}"
                )
            }
            Diag::EmbeddedDdlRejected { message, .. } => {
                write!(
                    f,
                    "the embedded SurrealQL parser rejected Surform's \
                     own emitted DDL: {message}"
                )
            }
            Diag::ExpectedClauseValue { key, .. } => {
                write!(f, "expected `:` and a value after `{key}`")
            }
            Diag::UnexpectedClauseForm { message, .. } => write!(f, "{message}"),
            Diag::InvalidDuration { text, .. } => {
                if text.is_empty() {
                    write!(f, "expected a duration value")
                } else {
                    write!(
                        f,
                        "`{text}` is not a SurrealQL duration — an integer plus a \
                         unit, or a compound like `1h30m`"
                    )
                }
            }
            Diag::RequiredClauseMissing { clause, block, .. } => {
                write!(f, "the `{block}` block requires `{clause}`")
            }
            Diag::UnknownClauseWord { word, key, .. } => {
                if word.is_empty() {
                    write!(f, "expected a word in the `{key}` list")
                } else {
                    write!(f, "unknown word `{word}` in the `{key}` clause")
                }
            }
            Diag::MutuallyExclusive { clause, other, .. } => {
                write!(f, "`{clause}` cannot be set together with `{other}`")
            }
            Diag::InvalidString { text, .. } => {
                if text.is_empty() {
                    write!(f, "expected a string literal value")
                } else {
                    write!(f, "`{text}` is not a SurrealQL string literal")
                }
            }
            Diag::InvalidFilter { text, .. } => {
                write!(f, "`{text}` is not a SurrealQL filter")
            }
            Diag::InvalidFnPath { message, .. } => write!(f, "{message}"),
            Diag::InvalidIdent { text, .. } => {
                if text.is_empty() {
                    write!(f, "expected an identifier value")
                } else {
                    write!(f, "`{text}` is not a single identifier")
                }
            }
            Diag::InvalidNumber { text, range, .. } => {
                if text.is_empty() {
                    write!(f, "expected a whole-number value")
                } else {
                    write!(f, "`{text}` is not a whole number in the {range} range")
                }
            }
            Diag::InvalidExecutable { message, .. } => write!(f, "{message}"),
            Diag::ModuleExecutableName { message, .. } => write!(f, "{message}"),
            Diag::InvalidFunctionSignature { message, .. } => write!(f, "{message}"),
            Diag::FunctionParamNeedsDollar { .. } => {
                write!(f, "fn parameter without `$`")
            }
            Diag::FnNameDoubledPrefix { .. } => {
                write!(
                    f,
                    "the fn header's path starts with `fn` — with the keyword's \
                     own prefix the emitted name would double it"
                )
            }
            Diag::FunctionParamTrailingComma { .. } => {
                write!(f, "trailing `,` in the parameter list")
            }
            Diag::InvalidKind { text, .. } => {
                if text.is_empty() {
                    write!(f, "expected a kind")
                } else {
                    write!(f, "`{text}` is not a SurrealQL kind")
                }
            }
            Diag::UnknownVariant { word, .. } => {
                if word.is_empty() {
                    write!(f, "expected a config variant")
                } else {
                    write!(f, "unknown config variant `{word}`")
                }
            }
            Diag::InvalidMiddleware { message, .. } => write!(f, "{message}"),
            Diag::InvalidIndexFields { message, .. } => write!(f, "{message}"),
            Diag::InvalidIndexParam { message, .. } => write!(f, "{message}"),
            Diag::InvalidFloat { text, .. } => {
                if text.is_empty() {
                    write!(f, "expected a decimal number")
                } else {
                    write!(f, "`{text}` is not a decimal number")
                }
            }
            Diag::InvalidBm25 { message, .. } => write!(f, "{message}"),
            Diag::InvalidDistance { message, .. } => write!(f, "{message}"),
            Diag::InvalidApiPath { text, .. } => {
                if text.is_empty() {
                    write!(f, "expected the route path — a string literal")
                } else {
                    write!(
                        f,
                        "`{text}` is not a string literal — the api route path is one"
                    )
                }
            }
            Diag::UnknownApiMethod { word, .. } => {
                if word.is_empty() {
                    write!(f, "expected a method word")
                } else {
                    write!(f, "unknown api sub-block head `{word}`")
                }
            }
            Diag::DuplicateApiMethod { method, .. } => {
                write!(
                    f,
                    "duplicate method `{method}` — each method may appear in only one group"
                )
            }
            Diag::DuplicateAnyBlock { .. } => {
                write!(f, "at most one `any` block is allowed")
            }
            Diag::MissingApiThen { .. } => {
                write!(f, "this method group requires `then:`")
            }
            Diag::InvalidAccessSource { message, .. } => {
                write!(f, "invalid access verification source: {message}")
            }
            Diag::InvalidAccessClause { message, .. } => {
                write!(f, "clause does not apply to this access variant: {message}")
            }
            Diag::InvalidAccessLevel { message, .. } => {
                write!(f, "access variant not allowed at this level: {message}")
            }
            Diag::InvalidAccessIssuer { message, .. } => {
                write!(f, "invalid access issuer: {message}")
            }
            Diag::AccessSecretExposure { message, .. } => {
                write!(
                    f,
                    "access secret key material may not be committed: {message}"
                )
            }
            Diag::UnknownMixin { name, .. } => {
                write!(f, "spread names no mixin of the document: `{name}`")
            }
            Diag::UnknownAlias { name, .. } => {
                write!(f, "no type alias of the document is named `{name}`")
            }
            Diag::InvalidAliasName { name, reason, .. } => {
                write!(f, "`{name}` cannot name a type alias: {reason}")
            }
            Diag::AliasExpansionDepth { owner, depth, .. } => write!(
                f,
                "the expansion of {owner} nests {depth} levels — \
                 beyond the kind nesting ceiling (256)"
            ),
            Diag::AliasExpansionInvalid { owner, text, .. } => write!(
                f,
                "the expansion of {owner} is not a kind the grammar accepts: `{text}`"
            ),
            Diag::DuplicateDefinition { family, name, .. } => {
                write!(f, "{family} `{name}` is already defined")
            }
            Diag::DefineCycle { chain, .. } => write!(
                f,
                "these definitions form a cycle the engine cannot apply \
                 in any order: {chain}"
            ),
            Diag::CrossSectionDependency { target, .. } => write!(
                f,
                "{target} is defined only after this section's next \
                 scope statement, and emission never reorders across one"
            ),
            Diag::DuplicateSpread { name, .. } => {
                write!(f, "mixin `{name}` is spread twice into this table")
            }
            Diag::SpreadMisplaced { .. } => {
                write!(f, "a spread entry belongs in a non-view table block")
            }
            Diag::DuplicateMemberKey { kind, key, .. } => {
                write!(f, "the {kind} `{key}` is defined twice in this block")
            }
            Diag::IncludeUnsupported { .. } => {
                write!(
                    f,
                    "include needs file composition — this context has no file system"
                )
            }
            Diag::IncludeMisplaced { statement, .. } => write!(
                f,
                "{statement} after an include — the composition preamble \
                 precedes the first include"
            ),
            Diag::InvalidTableHeader { message, .. } => {
                write!(f, "malformed table relation header: {message}")
            }
            Diag::InvalidTableClause { message, .. } => {
                write!(f, "clause does not apply to this table kind: {message}")
            }
            Diag::InvalidTableParam { message, .. } => {
                write!(f, "table clause value out of domain: {message}")
            }
            Diag::InvalidPassscram { message, .. } => {
                write!(f, "invalid SCRAM verifier: {message}")
            }
            Diag::InvalidPermissionsShorthand {
                text, statement, ..
            } => {
                if text.is_empty() {
                    write!(f, "expected `none` or `full`")
                } else {
                    write!(
                        f,
                        "`{text}` is not a {statement} permission — write `none`, \
                         `full`, or a per-operation block"
                    )
                }
            }
            Diag::InvalidNestedOn { statement, .. } => {
                write!(
                    f,
                    "a nested {statement} cannot carry `on:` — it inherits its host table"
                )
            }
            Diag::InvalidViewBody { message, .. } => {
                write!(f, "invalid view body: {message}")
            }
            Diag::FieldTypeRequired { .. } => {
                write!(
                    f,
                    "a field needs a type (`name: <type>;`) or a computed expression"
                )
            }
            Diag::InvalidFieldComputed { message, .. } => {
                write!(f, "invalid computed field: {message}")
            }
            Diag::InvalidFieldFlexible { message, .. } => {
                write!(f, "invalid flexible field: {message}")
            }
            Diag::InvalidFieldReference { message, .. } => {
                write!(f, "invalid reference field: {message}")
            }
            Diag::InvalidFieldId { message, .. } => {
                write!(f, "invalid `id` field: {message}")
            }
            Diag::InvalidFieldInline { message, .. } => {
                write!(f, "invalid INLINE field: {message}")
            }
            Diag::FieldDeletePermission { .. } => {
                write!(f, "fields take no `delete` permission")
            }
            Diag::InvalidFieldPath { message, .. } => {
                write!(f, "malformed field path: {message}")
            }
            Diag::RawBodyExpected { found, .. } => match found {
                Some(ch) => write!(
                    f,
                    "expected `{{` to open the raw block, found `{}`",
                    describe_char(*ch)
                ),
                None => write!(f, "expected `{{` to open the raw block, found end of file"),
            },
            Diag::InfoUnparsable { message, .. } => {
                write!(f, "cannot read INFO input: {message}")
            }
            Diag::InfoUnknownShape { message, .. } => {
                write!(f, "unexpected INFO shape: {message}")
            }
            Diag::DdlTripwire { message, .. } => {
                write!(
                    f,
                    "definition outside the pinned printer dialect (possible printer \
                     drift): {message}"
                )
            }
            Diag::InfoDepthExceeded { message, .. } => {
                write!(f, "cannot read INFO input: {message}")
            }
            Diag::InterchangeTagInvalid { message, .. } => {
                write!(f, "invalid `--info` scope tag: {message}")
            }
        }
    }
}

// The derive cannot express the literal `SDL-Exxxx` spelling (hyphens
// are not path syntax), so `Diagnostic` is implemented by hand.
impl miette::Diagnostic for Diag {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.code_str()))
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.url_str()))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.help_message()))
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let (text, span) = self.primary_label()?;
        Some(Box::new(std::iter::once(LabeledSpan::new(
            Some(text),
            span.offset,
            span.len,
        ))))
    }
}

impl std::error::Error for Diag {}

/// A character as it appears in a diagnostic message: control
/// characters, the Unicode line/paragraph separators, and the
/// invisible format characters (soft hyphen, zero-width spaces and
/// directional marks, bidi overrides, the BOM) render as their code
/// point — pasted-in copies of them would otherwise vanish into the
/// message, or worse, flip its rendering.
fn describe_char(ch: char) -> String {
    let invisible = ch.is_control()
        || matches!(
            ch,
            '\u{0085}' | '\u{00ad}' | '\u{200b}'..='\u{200f}' | '\u{2028}' | '\u{2029}'
                | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{feff}'
        );
    if invisible {
        format!("U+{:04X}", ch as u32)
    } else {
        ch.to_string()
    }
}

/// Shared test support: rendering a diagnostic the way every suite
/// that checks rendered output needs it — graphical handler, color,
/// fancy unicode, and hyperlink escapes all disabled: pure text,
/// snapshot-stable byte for byte.
#[cfg(test)]
pub(crate) mod test_support {
    use super::Diag;
    use miette::{GraphicalReportHandler, GraphicalTheme};

    pub(crate) fn render(src: &str, diag: &Diag) -> String {
        let report = miette::Report::new(diag.clone());
        let report = report.with_source_code(src.to_owned());
        let mut out = String::new();
        GraphicalReportHandler::new_themed(GraphicalTheme::none())
            .with_links(false)
            .render_report(&mut out, report.as_ref())
            .expect("rendering into a String cannot fail");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::render;
    use super::*;

    /// The code spelling is the product: the literal `SDL-Exxxx` string,
    /// identical in every renderer, one entry per variant. A slice, not
    /// a fixed-size array — the variant count is not an invariant worth
    /// a hand-maintained length annotation (the E2xxx retirement proved
    /// the trap).
    #[test]
    fn codes_are_literal_sdl_exxxx_strings() {
        let cases: &[(u16, Diag)] = &[
            (
                1,
                Diag::UnterminatedString {
                    span: Span::point(0),
                },
            ),
            (
                2,
                Diag::UnterminatedBlockComment {
                    span: Span::point(0),
                },
            ),
            (
                3,
                Diag::UnterminatedQuotedIdent {
                    span: Span::point(0),
                    closer: '`',
                },
            ),
            (
                4,
                Diag::UnterminatedCapture {
                    span: Span::point(0),
                    terminator: ';',
                },
            ),
            (
                5,
                Diag::UnbalancedCapture {
                    span: Span::point(0),
                    bracket: ')',
                },
            ),
            (
                6,
                Diag::InvalidIdentEscape {
                    span: Span::point(0),
                },
            ),
            (
                7,
                Diag::DanglingDocComment {
                    span: Span::point(0),
                },
            ),
            (
                8,
                Diag::FileUnreadable {
                    path: "x.surdl".into(),
                    reason: "no such file".into(),
                    span: None,
                },
            ),
            (
                9,
                Diag::FileUnwritable {
                    path: "x.surdl".into(),
                    reason: "permission denied".into(),
                },
            ),
            (
                17,
                Diag::MigrationScriptRefused {
                    span: Span::point(0),
                },
            ),
            (
                12,
                Diag::IncludePathAbsolute {
                    span: Span::point(0),
                    path: "/etc/x.surdl".into(),
                },
            ),
            (
                14,
                Diag::DuplicateInclude {
                    span: Span::point(0),
                    path: "x.surdl".into(),
                    first: "root.surdl".into(),
                },
            ),
            (
                13,
                Diag::WriteBackUnplaceable {
                    span: Span::point(0),
                    statement: "namespace `staging`".into(),
                },
            ),
            (
                1001,
                Diag::UnknownStatement {
                    span: Span::point(0),
                    word: "namespac".into(),
                    suggestion: Some("namespace".into()),
                },
            ),
            (
                1002,
                Diag::IllegalCharacter {
                    span: Span::point(0),
                    ch: '#',
                },
            ),
            (
                1003,
                Diag::UnknownClause {
                    span: Span::point(0),
                    key: "start".into(),
                    statement: "namespace",
                    suggestion: None,
                },
            ),
            (
                1004,
                Diag::ExpectedName {
                    span: Span::point(0),
                },
            ),
            (
                1005,
                Diag::ExpectedStatementEnd {
                    span: Span::point(0),
                    found: Some(':'),
                },
            ),
            (
                1006,
                Diag::UnterminatedBlock {
                    span: Span::point(0),
                },
            ),
            (
                1007,
                Diag::ReservedIdent {
                    span: Span::point(0),
                    word: "select".into(),
                },
            ),
            (
                1008,
                Diag::DocCommentUnsupported {
                    span: Span::point(0),
                    statement: "sequence",
                },
            ),
            (
                1009,
                Diag::EmbeddedParseError {
                    span: Span::point(0),
                    message: "Unexpected token `;`, expected a define statement keyword".into(),
                },
            ),
            (
                1010,
                Diag::ExpectedClauseValue {
                    span: Span::point(0),
                    key: "start",
                },
            ),
            (
                1011,
                Diag::UnexpectedClauseForm {
                    span: Span::point(0),
                    message: "clause `strict` is a flag — write `strict;`".into(),
                },
            ),
            (
                1012,
                Diag::InvalidDuration {
                    span: Span::point(0),
                    text: "7days".into(),
                },
            ),
            (
                1013,
                Diag::RequiredClauseMissing {
                    span: Span::point(0),
                    clause: "expiry",
                    block: "changefeed",
                },
            ),
            (
                1014,
                Diag::UnknownClauseWord {
                    span: Span::point(0),
                    word: "ownr".into(),
                    key: "roles",
                    suggestion: Some("owner".into()),
                },
            ),
            (
                1015,
                Diag::MutuallyExclusive {
                    span: Span::point(0),
                    clause: "password",
                    other: "passhash",
                },
            ),
            (
                1016,
                Diag::InvalidString {
                    span: Span::point(0),
                    text: "nope".into(),
                },
            ),
            (
                1017,
                Diag::InvalidFilter {
                    span: Span::point(0),
                    text: "edgengram(x, 2)".into(),
                },
            ),
            (
                1018,
                Diag::InvalidFnPath {
                    span: Span::point(0),
                    message: "`fn::name()` is a call — the `function` clause takes a \
                     name path (remove `()`)"
                        .into(),
                },
            ),
            (
                1019,
                Diag::InvalidIdent {
                    span: Span::point(0),
                    text: "123".into(),
                },
            ),
            (
                1020,
                Diag::InvalidNumber {
                    span: Span::point(0),
                    text: "5s".into(),
                    range: "0–65535",
                },
            ),
            (
                1021,
                Diag::InvalidExecutable {
                    span: Span::point(0),
                    message: "`bucket:noslash` is not a file string".into(),
                },
            ),
            (
                1022,
                Diag::ModuleExecutableName {
                    span: Span::point(0),
                    message: "a silo module is self-named — remove the header name".into(),
                },
            ),
            (
                1023,
                Diag::InvalidFunctionSignature {
                    span: Span::point(0),
                    message: "expected `(` to open the parameter list".into(),
                },
            ),
            (
                1024,
                Diag::FunctionParamNeedsDollar {
                    span: Span::point(0),
                },
            ),
            (
                1025,
                Diag::FunctionParamTrailingComma {
                    span: Span::point(0),
                },
            ),
            (
                1026,
                Diag::InvalidKind {
                    span: Span::point(0),
                    text: "frobnicate<int>".into(),
                },
            ),
            (
                1027,
                Diag::UnknownVariant {
                    span: Span::point(0),
                    word: "grapql".into(),
                    suggestion: Some("graphql".into()),
                },
            ),
            (
                1028,
                Diag::InvalidMiddleware {
                    span: Span::point(0),
                    message: "expected `(` with the middleware's arguments — write \
                     `name()` when there are none"
                        .into(),
                },
            ),
            (
                1029,
                Diag::InvalidIndexFields {
                    span: Span::point(0),
                    message: "a hnsw index takes exactly one field — found 2".into(),
                },
            ),
            (
                1030,
                Diag::InvalidIndexParam {
                    span: Span::point(0),
                    message: "HNSW parameter `m` cannot be larger than 127".into(),
                },
            ),
            (
                1031,
                Diag::InvalidFloat {
                    span: Span::point(0),
                    text: "1.2.3".into(),
                },
            ),
            (
                1032,
                Diag::InvalidBm25 {
                    span: Span::point(0),
                    message: "expected exactly two numbers in the pair".into(),
                },
            ),
            (
                1033,
                Diag::InvalidDistance {
                    span: Span::point(0),
                    message: "`cosine` takes no argument".into(),
                },
            ),
            (
                1034,
                Diag::InvalidApiPath {
                    span: Span::point(0),
                    text: "health".into(),
                },
            ),
            (
                1035,
                Diag::UnknownApiMethod {
                    span: Span::point(0),
                    word: "gets".into(),
                    suggestion: Some("get".into()),
                },
            ),
            (
                1036,
                Diag::DuplicateApiMethod {
                    span: Span::point(0),
                    method: "get".into(),
                },
            ),
            (
                1037,
                Diag::DuplicateAnyBlock {
                    span: Span::point(0),
                },
            ),
            (
                1038,
                Diag::MissingApiThen {
                    span: Span::point(0),
                },
            ),
            (
                1039,
                Diag::InvalidAccessSource {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1040,
                Diag::InvalidAccessClause {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1041,
                Diag::InvalidAccessLevel {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1042,
                Diag::InvalidAccessIssuer {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1043,
                Diag::InvalidTableHeader {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1044,
                Diag::InvalidTableClause {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1045,
                Diag::InvalidPermissionsShorthand {
                    span: Span::point(0),
                    text: "count() > 1".into(),
                    statement: "table",
                },
            ),
            (
                1046,
                Diag::InvalidNestedOn {
                    span: Span::point(0),
                    statement: "index",
                },
            ),
            (
                1047,
                Diag::InvalidViewBody {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1048,
                Diag::FieldTypeRequired {
                    span: Span::point(0),
                },
            ),
            (
                1049,
                Diag::InvalidFieldComputed {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1050,
                Diag::InvalidFieldFlexible {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1051,
                Diag::InvalidFieldReference {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1052,
                Diag::InvalidFieldId {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1053,
                Diag::FieldDeletePermission {
                    span: Span::point(0),
                },
            ),
            (
                1054,
                Diag::InvalidFieldPath {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1055,
                Diag::RawBodyExpected {
                    span: Span::point(0),
                    found: Some(';'),
                },
            ),
            (
                1056,
                Diag::AccessSecretExposure {
                    span: Span::point(0),
                    message: "composed at the push site".into(),
                },
            ),
            (
                1057,
                Diag::InvalidTableParam {
                    span: Span::point(0),
                    message: "the cap cannot exceed 256".into(),
                },
            ),
            (
                1058,
                Diag::InvalidFieldInline {
                    span: Span::point(0),
                    message: "`in` cannot be inline".into(),
                },
            ),
            (
                1059,
                Diag::InvalidPassscram {
                    span: Span::point(0),
                    message: "the verifier's salt is malformed".into(),
                },
            ),
            (
                1060,
                Diag::EmbeddedDdlRejected {
                    span: Span::point(0),
                    message: "the render".into(),
                },
            ),
            (
                1061,
                Diag::UnknownMixin {
                    span: Span::point(0),
                    name: "stamp".into(),
                    suggestion: Some("stamped".into()),
                },
            ),
            (
                1062,
                Diag::DuplicateDefinition {
                    span: Span::point(0),
                    family: "mixin",
                    name: "stamp".into(),
                },
            ),
            (
                15,
                Diag::DefineCycle {
                    span: Span::point(0),
                    chain: "view `a` → view `b` → view `a`".into(),
                },
            ),
            (
                16,
                Diag::CrossSectionDependency {
                    span: Span::point(0),
                    target: "view `t`".into(),
                },
            ),
            (
                1064,
                Diag::DuplicateSpread {
                    span: Span::point(0),
                    name: "stamp".into(),
                },
            ),
            (
                1065,
                Diag::SpreadMisplaced {
                    span: Span::point(0),
                    statement: "view",
                },
            ),
            // 1063/1066 are retired codes — no variant exists to pin;
            // the numbering keeps its gaps.
            (
                1067,
                Diag::DuplicateMemberKey {
                    span: Span::point(0),
                    kind: "field",
                    key: "x".into(),
                },
            ),
            (
                1068,
                Diag::IncludeUnsupported {
                    span: Span::point(0),
                },
            ),
            (
                1069,
                Diag::IncludeMisplaced {
                    span: Span::point(0),
                    statement: "include",
                },
            ),
            (
                1070,
                Diag::IncludeScopeBanned {
                    span: Span::point(0),
                    statement: "namespace",
                },
            ),
            (
                1071,
                Diag::UnknownAlias {
                    span: Span::point(0),
                    name: "Email".into(),
                    suggestion: None,
                },
            ),
            (
                1072,
                Diag::InvalidAliasName {
                    span: Span::point(0),
                    name: "string".into(),
                    reason: "reserved".into(),
                },
            ),
            (
                1073,
                Diag::AliasExpansionDepth {
                    span: Span::point(0),
                    owner: "type alias `Deep`".into(),
                    depth: 300,
                },
            ),
            (
                1074,
                Diag::AliasExpansionInvalid {
                    span: Span::point(0),
                    owner: "type alias `Wrapped`".into(),
                    text: "option<option<int>>".into(),
                },
            ),
            (
                3001,
                Diag::InfoUnparsable {
                    span: Span::point(0),
                    message: "expected `{`".into(),
                },
            ),
            (
                3002,
                Diag::InfoUnknownShape {
                    span: Span::point(0),
                    message: "the `namespaces` entry is not a string".into(),
                },
            ),
            (
                3003,
                Diag::DdlTripwire {
                    span: Span::point(0),
                    message: "unknown clause `bogus`".into(),
                },
            ),
            (
                3004,
                Diag::InfoDepthExceeded {
                    span: Span::point(0),
                    message: "the object nests deeper than 128 levels".into(),
                },
            ),
            (
                3005,
                Diag::InterchangeTagInvalid {
                    span: Span::point(0),
                    message: "`db=` tag names a database the file never declares".into(),
                    help: None,
                },
            ),
        ];
        for (number, diag) in cases {
            assert_eq!(diag.code_str(), format!("SDL-E{number:04}"));
        }
    }

    /// Control and invisible format characters must not smuggle
    /// themselves into messages.
    #[test]
    fn invisible_characters_render_as_code_points() {
        assert_eq!(describe_char('\u{200b}'), "U+200B");
        assert_eq!(describe_char('\u{00ad}'), "U+00AD");
        assert_eq!(describe_char('\u{0085}'), "U+0085");
        assert_eq!(describe_char('\u{2028}'), "U+2028");
        assert_eq!(describe_char('\u{2029}'), "U+2029");
        assert_eq!(describe_char('\u{202e}'), "U+202E");
        assert_eq!(describe_char('\u{feff}'), "U+FEFF");
        assert_eq!(describe_char('\n'), "U+000A");
        // Visible characters pass through, non-ASCII included.
        assert_eq!(describe_char('x'), "x");
        assert_eq!(describe_char('⟨'), "⟨");
    }

    #[test]
    fn snapshot_unterminated_string() {
        let src = "signup: 'oops\n}\n";
        let diag = Diag::UnterminatedString {
            span: Span::new(8, 1),
        };
        insta::assert_snapshot!("unterminated-string", render(src, &diag));
    }

    #[test]
    fn snapshot_unterminated_block_comment() {
        let src = "fn f() { /* oops\n";
        let diag = Diag::UnterminatedBlockComment {
            span: Span::new(9, 2),
        };
        insta::assert_snapshot!("unterminated-block-comment", render(src, &diag));
    }

    #[test]
    fn snapshot_unterminated_quoted_ident() {
        let src = "table user {\n`weird field: int;\n}\n";
        let diag = Diag::UnterminatedQuotedIdent {
            span: Span::new(13, 1),
            closer: '`',
        };
        insta::assert_snapshot!("unterminated-quoted-ident", render(src, &diag));
    }

    #[test]
    fn snapshot_unterminated_capture() {
        let src = "param $x = 1 +\n";
        let diag = Diag::UnterminatedCapture {
            span: Span::point(11),
            terminator: ';',
        };
        insta::assert_snapshot!("unterminated-capture", render(src, &diag));
    }

    #[test]
    fn snapshot_unbalanced_capture() {
        let src = "assert: foo);\n";
        let diag = Diag::UnbalancedCapture {
            span: Span::new(11, 1),
            bracket: ')',
        };
        insta::assert_snapshot!("unbalanced-capture", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_ident_escape() {
        let src = "namespace ⟨a\\⟩b⟩;\n";
        let diag = Diag::InvalidIdentEscape {
            span: Span::new(13, 1),
        };
        insta::assert_snapshot!("invalid-ident-escape", render(src, &diag));
    }

    #[test]
    fn snapshot_dangling_doc_comment() {
        let src = "/// Acme Corporation\n";
        let diag = Diag::DanglingDocComment {
            span: Span::new(0, 3),
        };
        insta::assert_snapshot!("dangling-doc-comment", render(src, &diag));
    }

    /// The migration-script refusal: one diagnostic anchored on the
    /// draft header's first line, in place of the SDL-E1001/E1002
    /// wall the parse would otherwise raise.
    #[test]
    fn snapshot_migration_script_refused() {
        let src = "-- surform migration 0001 — add-users\n\
                   -- generated: 2026-10-07T18:04:05Z\n\
                   BEGIN TRANSACTION;\n";
        let diag = Diag::MigrationScriptRefused {
            // The header line's byte length (the em-dash is 3 bytes).
            span: Span::new(0, 39),
        };
        insta::assert_snapshot!("migration-script-refused", render(src, &diag));
    }

    #[test]
    fn snapshot_unknown_statement() {
        let src = "namespac acme;\n";
        let diag = Diag::UnknownStatement {
            span: Span::new(0, 8),
            word: "namespac".into(),
            suggestion: Some("namespace".into()),
        };
        insta::assert_snapshot!("unknown-statement", render(src, &diag));
    }

    #[test]
    fn snapshot_illegal_character() {
        let src = "# not a comment\n";
        let diag = Diag::IllegalCharacter {
            span: Span::new(0, 1),
            ch: '#',
        };
        insta::assert_snapshot!("illegal-character", render(src, &diag));
    }

    #[test]
    fn snapshot_unknown_clause() {
        let src = "namespace acme { start: 5; };\n";
        let diag = Diag::UnknownClause {
            span: Span::new(17, 5),
            key: "start".into(),
            statement: "namespace",
            suggestion: None,
        };
        insta::assert_snapshot!("unknown-clause", render(src, &diag));
    }

    /// The reserved-word help for a bare `raw` head in block context
    /// (rule 8).
    #[test]
    fn snapshot_unknown_clause_raw() {
        let src = "table t { raw: string; };\n";
        let diag = Diag::UnknownClause {
            span: Span::new(10, 3),
            key: "raw".into(),
            statement: "table",
            suggestion: None,
        };
        insta::assert_snapshot!("unknown-clause-raw", render(src, &diag));
    }

    #[test]
    fn snapshot_raw_body_expected() {
        let src = "raw;\n";
        let diag = Diag::RawBodyExpected {
            span: Span::point(3),
            found: Some(';'),
        };
        insta::assert_snapshot!("raw-body-expected", render(src, &diag));
    }

    /// The clause-neutral raw help (non-table blocks have no field
    /// escape to mention).
    #[test]
    fn snapshot_unknown_clause_raw_non_table() {
        let src = "user u { on: root; raw: 1; };\n";
        let diag = Diag::UnknownClause {
            span: Span::new(19, 3),
            key: "raw".into(),
            statement: "user",
            suggestion: None,
        };
        insta::assert_snapshot!("unknown-clause-raw-non-table", render(src, &diag));
    }

    /// The api body loop's own raw-keyed arm (its head error never
    /// reaches the UnknownClause path).
    #[test]
    fn snapshot_unknown_api_method_raw() {
        let src = "api '/x' { raw { then: { 1 }; }; };\n";
        let diag = Diag::UnknownApiMethod {
            span: Span::new(11, 3),
            word: "raw".into(),
            suggestion: None,
        };
        insta::assert_snapshot!("unknown-api-method-raw", render(src, &diag));
    }

    #[test]
    fn snapshot_expected_name() {
        let src = "namespace ;\n";
        let diag = Diag::ExpectedName {
            span: Span::point(10),
        };
        insta::assert_snapshot!("expected-name", render(src, &diag));
    }

    #[test]
    fn snapshot_expected_statement_end() {
        let src = "namespace acme = 5;\n";
        let diag = Diag::ExpectedStatementEnd {
            span: Span::new(15, 1),
            found: Some('='),
        };
        insta::assert_snapshot!("expected-statement-end", render(src, &diag));
    }

    #[test]
    fn snapshot_unterminated_block() {
        let src = "namespace acme {\n";
        let diag = Diag::UnterminatedBlock {
            span: Span::new(15, 1),
        };
        insta::assert_snapshot!("unterminated-block", render(src, &diag));
    }

    #[test]
    fn snapshot_reserved_ident() {
        let src = "namespace select;\n";
        let diag = Diag::ReservedIdent {
            span: Span::new(10, 6),
            word: "select".into(),
        };
        insta::assert_snapshot!("reserved-ident", render(src, &diag));
    }

    #[test]
    fn snapshot_doc_comment_unsupported() {
        let src = "/// invoice numbering\nsequence invoice_number;\n";
        let diag = Diag::DocCommentUnsupported {
            span: Span::new(0, 3),
            statement: "sequence",
        };
        insta::assert_snapshot!("doc-comment-unsupported", render(src, &diag));
    }

    /// The embedded render rides in the message with its snippet
    /// marker re-anchored to the capture's file position — built
    /// through the real bridge so the pin tracks upstream's renderer.
    #[test]
    fn snapshot_embedded_parse_error() {
        let src = "param $x = 1 COMMENT 'oops';\n";
        let message = match crate::expr::canonicalize(
            &src[11..27],
            crate::expr::Slot::ParamValue,
            crate::expr::Position {
                line: 1,
                column: 12,
            },
        ) {
            Err(crate::expr::EmbeddedError::Rejected { message }) => message,
            _ => panic!("must reject"),
        };
        let diag = Diag::EmbeddedParseError {
            span: Span::new(11, 16),
            message,
        };
        insta::assert_snapshot!("embedded-parse-error", render(src, &diag));
    }

    #[test]
    fn snapshot_expected_clause_value() {
        let src = "sequence n { start; };\n";
        let diag = Diag::ExpectedClauseValue {
            span: Span::new(13, 5),
            key: "start",
        };
        insta::assert_snapshot!("expected-clause-value", render(src, &diag));
    }

    #[test]
    fn snapshot_unexpected_clause_form() {
        let src = "database blog { strict: true; };\n";
        let diag = Diag::UnexpectedClauseForm {
            span: Span::new(17, 6),
            message: "clause `strict` is a flag — write `strict;` (no value, no block)".into(),
        };
        insta::assert_snapshot!("unexpected-clause-form", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_duration() {
        let src = "database blog { changefeed { expiry: 7days; }; };\n";
        let diag = Diag::InvalidDuration {
            span: Span::new(35, 5),
            text: "7days".into(),
        };
        insta::assert_snapshot!("invalid-duration", render(src, &diag));
    }

    #[test]
    fn snapshot_required_clause_missing() {
        let src = "database blog { changefeed { include original; }; };\n";
        let diag = Diag::RequiredClauseMissing {
            span: Span::new(17, 10),
            clause: "expiry",
            block: "changefeed",
        };
        insta::assert_snapshot!("required-clause-missing", render(src, &diag));
    }

    #[test]
    fn snapshot_unknown_clause_word() {
        let src = "user bot { on: root; roles: ownr; };\n";
        let diag = Diag::UnknownClauseWord {
            span: Span::new(28, 4),
            word: "ownr".into(),
            key: "roles",
            suggestion: Some("owner".into()),
        };
        insta::assert_snapshot!("unknown-clause-word", render(src, &diag));
    }

    #[test]
    fn snapshot_mutually_exclusive() {
        let src = "user bot { on: root; passhash: 'h'; password: 'p'; };\n";
        let diag = Diag::MutuallyExclusive {
            span: Span::new(36, 8),
            clause: "password",
            other: "passhash",
        };
        insta::assert_snapshot!("mutually-exclusive", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_string() {
        let src = "user bot { on: root; password: nope; };\n";
        let diag = Diag::InvalidString {
            span: Span::new(31, 4),
            text: "nope".into(),
        };
        insta::assert_snapshot!("invalid-string", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_filter() {
        let src = "analyzer a { filters: edgengram(x, 2); };\n";
        let diag = Diag::InvalidFilter {
            span: Span::new(22, 15),
            text: "edgengram(x, 2)".into(),
        };
        insta::assert_snapshot!("invalid-filter", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_fn_path() {
        let src = "analyzer a { function: fn::name(); };\n";
        let diag = Diag::InvalidFnPath {
            span: Span::new(23, 10),
            message: "`fn::name()` is a call — the `function` clause takes a name path \
                      (remove `()`)"
                .into(),
        };
        insta::assert_snapshot!("invalid-fn-path", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_ident() {
        let src = "event e { on: 123; then: 1; };\n";
        let diag = Diag::InvalidIdent {
            span: Span::new(14, 3),
            text: "123".into(),
        };
        insta::assert_snapshot!("invalid-ident", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_number() {
        let src = "event e { on: t; then: 1; async { retry: 5s; }; };\n";
        let diag = Diag::InvalidNumber {
            span: Span::new(41, 2),
            text: "5s".into(),
            range: "0–65535",
        };
        insta::assert_snapshot!("invalid-number", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_executable() {
        let src = "module color { from: 'memory'; unsigned; };\n";
        let diag = Diag::InvalidExecutable {
            span: Span::new(22, 9),
            message: "expected an f-string (`f'…'`) or the `silo::` prefix".into(),
        };
        insta::assert_snapshot!("invalid-executable", render(src, &diag));
    }

    #[test]
    fn snapshot_module_executable_name() {
        let src = "module color { from: silo::acme::utils::<1.2.0>; unsigned; };\n";
        let diag = Diag::ModuleExecutableName {
            span: Span::new(7, 5),
            message: "a silo module is self-named — remove the header name \
                      (the 3.3 silo form takes no alias)"
                .into(),
        };
        insta::assert_snapshot!("module-executable-name", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_function_signature() {
        let src = "fn color $p: int) { 1 };\n";
        let diag = Diag::InvalidFunctionSignature {
            span: Span::new(9, 0),
            message: "expected `(` to open the parameter list".into(),
        };
        insta::assert_snapshot!("invalid-function-signature", render(src, &diag));
    }

    #[test]
    fn snapshot_function_param_needs_dollar() {
        let src = "fn price(p: int) { 1 };\n";
        let diag = Diag::FunctionParamNeedsDollar {
            span: Span::new(9, 1),
        };
        insta::assert_snapshot!("function-param-needs-dollar", render(src, &diag));
    }

    #[test]
    fn snapshot_function_param_trailing_comma() {
        let src = "fn price($a: int,) { 1 };\n";
        let diag = Diag::FunctionParamTrailingComma {
            span: Span::new(17, 1),
        };
        insta::assert_snapshot!("function-param-trailing-comma", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_kind() {
        let src = "fn price($a: frobnicate<int>) { 1 };\n";
        let diag = Diag::InvalidKind {
            span: Span::new(13, 16),
            text: "frobnicate<int>".into(),
        };
        insta::assert_snapshot!("invalid-kind", render(src, &diag));
    }

    #[test]
    fn snapshot_unknown_variant() {
        let src = "config grapql { auto; };\n";
        let diag = Diag::UnknownVariant {
            span: Span::new(7, 6),
            word: "grapql".into(),
            suggestion: Some("graphql".into()),
        };
        insta::assert_snapshot!("unknown-variant", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_middleware() {
        let src = "config api { middleware: api::timeout; };\n";
        let diag = Diag::InvalidMiddleware {
            span: Span::new(24, 13),
            message: "expected `(` with the middleware's arguments — write \
                      `name()` when there are none"
                .into(),
        };
        insta::assert_snapshot!("invalid-middleware", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_index_fields() {
        let src = "index hnsw v { on: t; fields: a, b; dimension: 8; };\n";
        let diag = Diag::InvalidIndexFields {
            span: Span::new(21, 6),
            message: "a hnsw index takes exactly one field — found 2".into(),
        };
        insta::assert_snapshot!("invalid-index-fields", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_index_param() {
        let src = "index hnsw v { on: t; fields: a; dimension: 8; m: 200; };\n";
        let diag = Diag::InvalidIndexParam {
            span: Span::new(45, 1),
            message: "HNSW parameter `m` cannot be larger than 127".into(),
        };
        insta::assert_snapshot!("invalid-index-param", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_float() {
        let src = "index diskann v { on: t; fields: a; dimension: 8; alpha: 1.2.3; };\n";
        let diag = Diag::InvalidFloat {
            span: Span::new(51, 5),
            text: "1.2.3".into(),
        };
        insta::assert_snapshot!("invalid-float", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_bm25() {
        let src = "index fulltext f { on: t; fields: a; bm25: 1.2, 0.75; };\n";
        let diag = Diag::InvalidBm25 {
            span: Span::new(37, 10),
            message: "the parens are mandatory — write `bm25: (1.2, 0.75);`".into(),
        };
        insta::assert_snapshot!("invalid-bm25", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_distance() {
        let src = "index hnsw v { on: t; fields: a; dimension: 8; distance: cosin; };\n";
        let diag = Diag::InvalidDistance {
            span: Span::new(43, 15),
            message: "unknown distance `cosin` — did you mean `cosine`?".into(),
        };
        insta::assert_snapshot!("invalid-distance", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_api_path() {
        let src = "api health { get { then: { { status: 200 } }; }; };\n";
        let diag = Diag::InvalidApiPath {
            span: Span::new(4, 6),
            text: "health".into(),
        };
        insta::assert_snapshot!("invalid-api-path", render(src, &diag));
    }

    #[test]
    fn snapshot_unknown_api_method() {
        let src = "api '/x' { gets { then: { { status: 200 } }; }; };\n";
        let diag = Diag::UnknownApiMethod {
            span: Span::new(11, 4),
            word: "gets".into(),
            suggestion: Some("get".into()),
        };
        insta::assert_snapshot!("unknown-api-method", render(src, &diag));
    }

    #[test]
    fn snapshot_duplicate_api_method() {
        let src = "api '/x' { get { then: 1; }; get { then: 2; }; };\n";
        let diag = Diag::DuplicateApiMethod {
            // The parser anchors on the offending second head word.
            span: Span::new(29, 3),
            method: "get".into(),
        };
        insta::assert_snapshot!("duplicate-api-method", render(src, &diag));
    }

    #[test]
    fn snapshot_duplicate_any_block() {
        let src = "api '/x' { any { then: 1; }; any { then: 2; }; };\n";
        let diag = Diag::DuplicateAnyBlock {
            // The parser anchors on the second `any` head word.
            span: Span::new(29, 3),
        };
        insta::assert_snapshot!("duplicate-any-block", render(src, &diag));
    }

    #[test]
    fn snapshot_missing_api_then() {
        let src = "api '/x' { get { permissions: none; }; };\n";
        let diag = Diag::MissingApiThen {
            // The parser anchors on the group's `{`, like every
            // block-shape required-clause anchor.
            span: Span::new(15, 1),
        };
        insta::assert_snapshot!("missing-api-then", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_access_source() {
        let src = "access sso { on: root; type: jwt; url: 'https://x/jwks'; algorithm: hs256; };\n";
        let diag = Diag::InvalidAccessSource {
            // The parser anchors on the pair-side clause key.
            span: Span::new(66, 9),
            message: "a `url:` source takes no local key — remove `algorithm:` (and `key:`)".into(),
        };
        insta::assert_snapshot!("invalid-access-source", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_access_clause() {
        let src = "access sso { on: root; type: jwt; for: user; url: 'https://x/jwks'; };\n";
        let diag = Diag::InvalidAccessClause {
            span: Span::new(29, 3),
            message: "clause `for` is not part of the `jwt` access vocabulary — it belongs \
             to a bearer access"
                .into(),
        };
        insta::assert_snapshot!("invalid-access-clause", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_access_level() {
        let src = "access account { on: root; type: record; signin: 1; };\n";
        let diag = Diag::InvalidAccessLevel {
            span: Span::new(29, 12),
            message: "`type: record` requires `on: database` (parser-enforced upstream)".into(),
        };
        insta::assert_snapshot!("invalid-access-level", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_access_issuer() {
        let src = "access api_jwks { on: root; type: jwt; algorithm: es256; key: 'k'; issuer { algorithm: hs256; key: 's'; }; };\n";
        let diag = Diag::InvalidAccessIssuer {
            // The parser anchors on the `issuer {` key.
            span: Span::new(70, 6),
            message: "issuer algorithm `HS256` must equal the verification algorithm `ES256` \
             when verifying by local key"
                .into(),
        };
        insta::assert_snapshot!("invalid-access-issuer", render(src, &diag));
    }

    #[test]
    fn snapshot_access_secret_exposure() {
        let src = "access sso { on: root; type: jwt; algorithm: hs256; key: 'real_secret'; };\n";
        let diag = Diag::AccessSecretExposure {
            // The parser anchors on the `key` clause word.
            span: Span::new(52, 3),
            message: "a symmetric (`HS256`) verify key is secret material — write \
                      `key: '[REDACTED]';`"
                .into(),
        };
        insta::assert_snapshot!("access-secret-exposure", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_table_header() {
        let src = "table user -> wrote { enforced; };\n";
        let diag = Diag::InvalidTableHeader {
            // The parser anchors where the second arrow belongs.
            span: Span::new(19, 1),
            message: "the relation header needs its second arrow — write \
                      `table [<from>] -> <name> -> [<to>]`"
                .into(),
        };
        insta::assert_snapshot!("invalid-table-header", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_table_clause() {
        let src = "table users { enforced; schemafull; };\n";
        let diag = Diag::InvalidTableClause {
            span: Span::new(13, 8),
            message: "`enforced;` is not part of the normal table vocabulary — it belongs \
             to a relation table"
                .into(),
        };
        insta::assert_snapshot!("invalid-table-clause", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_table_param() {
        let src = "table t { inline edges: 257; };\n";
        let diag = Diag::InvalidTableParam {
            span: Span::new(24, 3),
            message: "an INLINE cache cap cannot exceed 256 — `inline edges: 257;` is out \
             of domain"
                .into(),
        };
        insta::assert_snapshot!("invalid-table-param", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_passscram() {
        // The sentinel spelling — the trap the message names (an
        // INFO echo can never round-trip).
        let src = "user u { on: root; passscram: '[REDACTED]'; };\n";
        let diag = Diag::InvalidPassscram {
            span: Span::new(19, 13),
            message: "the `'[REDACTED]'` sentinel is INFO's redaction — a PASSSCRAM \
             verifier never round-trips; author the real value"
                .into(),
        };
        insta::assert_snapshot!("invalid-passscram", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_field_inline() {
        let src = "field stats.views: int { on: t; inline; };\n";
        let diag = Diag::InvalidFieldInline {
            span: Span::new(32, 6),
            message: "only a top-level field can be INLINE — a dotted or wildcard path \
             cannot embed into the adjacency payloads"
                .into(),
        };
        insta::assert_snapshot!("invalid-field-inline", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_permissions_shorthand() {
        let src = "table t { permissions: $auth.id = id; };\n";
        let diag = Diag::InvalidPermissionsShorthand {
            span: Span::new(20, 15),
            text: "$auth.id = id".into(),
            statement: "table",
        };
        insta::assert_snapshot!("invalid-permissions-shorthand", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_nested_on() {
        let src = "table t { index u { on: t; fields: a; }; };\n";
        let diag = Diag::InvalidNestedOn {
            span: Span::new(14, 2),
            statement: "index",
        };
        insta::assert_snapshot!("invalid-nested-on", render(src, &diag));
    }

    #[test]
    fn snapshot_invalid_view_body() {
        let src = "table view stats { permissions: full; select author from post; };\n";
        let diag = Diag::InvalidViewBody {
            // The parser anchors on the block's `{`.
            span: Span::new(17, 1),
            message: "the view's query comes first — write `select …;` before any \
             clauses"
                .into(),
        };
        insta::assert_snapshot!("invalid-view-body", render(src, &diag));
    }
}
