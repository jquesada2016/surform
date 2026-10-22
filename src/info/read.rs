//! The INFO round-trip's read side: ingest rendered
//! INFO query results and read the five simple statements' DDL back
//! into the SDL AST.
//!
//! Two layers:
//!
//! **The interchange reader.** The tool consumes rendered INFO query
//! results — SurrealQL object-literal text (`{ namespaces: { acme:
//! 'DEFINE NAMESPACE …' }, … }`). [`read_info_object`] walks that
//! text with the SDL lexer's rule-3 skip set (an SDL-lexed object
//! walk — never a SurrealQL parse) into [`InfoObject`]: per-section
//! maps whose leaves are string literals (definitions' DDL text),
//! nested objects (structured values such as ROOT's default config),
//! or raw scalar tokens (`INFO FOR ROOT`'s `system`/`nodes`/`config`
//! sections carry numbers, arrays, and `NONE` — carried, never read).
//!
//! **The DDL readers.** The embedded parser's role on
//! the reverse path is VALIDATE + CANONICALIZE: every DDL string is
//! parsed with all capabilities and re-printed through its public
//! `to_sql()` ([`crate::expr::canonicalize_statement`]), so the
//! SDL-side splitters face exactly one deterministic dialect — the
//! pinned printer's. Splitting is keyword matching over that text
//! (the `expr.rs` harness-constants discipline scaled up); every
//! shape the splitters accept is pinned by tripwire tests against
//! live engine output. A statement that re-prints outside the pinned
//! dialect diagnoses ([`Diag::DdlTripwire`]) — in a round trip that
//! is printer drift, a deliberate-spec-event signal, never silently
//! swallowed. What the AST cannot carry rides the read-model side
//! channel ([`ReadEntry`]/[`Markers`]); the adopt/elide/preserve/fold
//! decisions are the normalizer's — the readers keep
//! everything INFO reports, faithfully.
//!
//! Span contract: [`read_info_object`] diagnostics anchor the
//! interchange text it was given; the per-entry readers' diagnostics
//! anchor `Span::point(0)` — they render against that entry's DDL
//! string, with the offending token quoted in the message (the
//! `SDL-E1009` whole-capture precedent; precise re-print spans land
//! with the CLI that renders them).

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::ast::{
    Access, AccessDuration, AccessKind, Algorithm, Analyzer, Api, ApiAction, ApiAnyBlock,
    ApiConfig, ApiGroupConfig, ApiMethod, Async, Authored, BearerSubject, Bm25, Bucket, Carried,
    ChangeFeed, Config, Database, DefaultConfig, Distance, DocComment, Event, Executable, Expr,
    Family, Field, FieldDefault, FieldPath, FieldPathSeg, Filter, Float, FnKind, FnPath, Function,
    Graphql, GraphqlConfig, Ident, Index, IndexKind, Introspection, JwtIssuer, JwtSource,
    JwtVerify, Kind, Level, Middleware, Module, Namespace, OnDelete, OpPermissions, Param,
    ParamName, Pass, Permission, PermissionGroup, PermissionOp, Raw, Role, Sequence, Stmt, Table,
    TableKind, Tokenizer, User, VectorType,
};
use crate::diag::Diag;
use crate::expr::{EmbeddedError, canonicalize_statement};
use crate::lexer::Span;
use crate::lexer::{CaptureFollow, CaptureStopWord, Scanner};
use crate::parser::{SECRET_SENTINEL_CANON, SECRET_SENTINEL_CANON_DECODED, parse_duration_text};

// ---------------------------------------------------------------------
// The interchange reader
// ---------------------------------------------------------------------

/// One ingested INFO result: the sections of a rendered INFO query
/// result, each a name→value map. Key order is the map's (sorted) —
/// INFO order is irrelevant; canonical placement is the normalizer's.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InfoObject(pub BTreeMap<String, InfoValue>);

/// One entry of an [`InfoObject`] section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfoValue {
    /// A string leaf — a definition's DDL text (or any string value).
    Ddl(String),
    /// A nested object — an empty section, or a structured value (the
    /// ROOT default config's shape; the medium readers consume it).
    Object(InfoObject),
    /// Any other leaf, carried as its raw token text — numbers,
    /// floats, arrays, `NONE`, booleans. Real INFO output carries
    /// these in sections the readers never request (`system`,
    /// `nodes`, `config`); they are preserved so real input
    /// round-trips, and diagnosed only where a reader asks for them.
    Other(String),
}

/// Reads rendered INFO query results (the interchange format)
/// into an [`InfoObject`]. Diagnostics
/// ([`Diag::InfoUnparsable`] for a malformed object walk,
/// [`Diag::InfoUnknownShape`] for a leaf that is neither a string, an
/// object, nor a scalar token) anchor the input text.
pub fn read_info_object(text: &str) -> Result<InfoObject, Diag> {
    let mut scan = Scanner::new(text);
    scan.skip_surrealql_trivia();
    if scan.peek_byte() != Some(b'{') {
        return Err(walk_error(
            scan.offset(),
            "expected `{` opening the INFO object".to_string(),
        ));
    }
    let object = walk_object(text, &mut scan, 0)?;
    scan.skip_surrealql_trivia();
    if !scan.is_at_end() {
        return Err(walk_error(
            scan.offset(),
            "trailing text after the INFO object".to_string(),
        ));
    }
    Ok(object)
}

/// A walk failure: always [`Diag::InfoUnparsable`], anchored at
/// `offset` of the interchange text.
fn walk_error(offset: usize, message: String) -> Diag {
    Diag::InfoUnparsable {
        span: Span::point(offset),
        message,
    }
}

/// Whether a leaf's scan already consumed the entry separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeafEnd {
    /// The leaf ended before any separator — the object loop consumes
    /// `,` or `}` itself.
    Unconsumed,
    /// The raw-scalar capture ate the entry's `,`.
    Comma,
    /// The raw-scalar capture ate the object's closing `}`.
    CloseBrace,
}

/// The object walk's maximum nesting depth (the CLIs accept
/// interchange text from files, where an
/// adversarial input is realistic — the budget turns the old
/// stack-overflow abort into [`Diag::InfoDepthExceeded`]). Real INFO
/// output nests ~3 levels; the SDL parser's recursive descent shares
/// the old unbounded posture and is pre-existing.
pub(crate) const MAX_INFO_DEPTH: usize = 128;

/// Walks an object literal; the cursor sits on its opening `{`.
/// `depth` is the nesting level of this object (the interchange's
/// outermost object is 0).
fn walk_object(src: &str, scan: &mut Scanner, depth: usize) -> Result<InfoObject, Diag> {
    debug_assert_eq!(scan.peek_byte(), Some(b'{'));
    scan.eat_byte(b'{');
    let mut map = BTreeMap::new();
    loop {
        scan.skip_surrealql_trivia();
        if scan.eat_byte(b'}') {
            return Ok(InfoObject(map));
        }
        let key = walk_key(src, scan)?;
        scan.skip_surrealql_trivia();
        if !scan.eat_byte(b':') {
            return Err(walk_error(
                scan.offset(),
                format!("expected `:` after the key `{key}`"),
            ));
        }
        scan.skip_surrealql_trivia();
        let (value, end) = walk_value(src, scan, depth)?;
        match end {
            LeafEnd::CloseBrace => {
                map.insert(key, value);
                return Ok(InfoObject(map));
            }
            LeafEnd::Comma => {
                map.insert(key, value);
                continue;
            }
            LeafEnd::Unconsumed => {
                scan.skip_surrealql_trivia();
                if scan.eat_byte(b',') {
                    map.insert(key, value);
                    continue;
                }
                if scan.eat_byte(b'}') {
                    map.insert(key, value);
                    return Ok(InfoObject(map));
                }
                return Err(walk_error(
                    scan.offset(),
                    format!("expected `,` or `}}` after the entry `{key}`"),
                ));
            }
        }
    }
}

/// Reads one object key: a string or an identifier (INFO prints keys
/// bare when plain, and doubly wrapped when the name itself needed
/// SurrealQL quoting — the object renderer's JSON string around the
/// backticked ident, `` "`a.b`" `` — decoded here to the name the
/// header carries).
fn walk_key(src: &str, scan: &mut Scanner) -> Result<String, Diag> {
    if matches!(scan.peek_byte(), Some(b'\'' | b'"')) {
        let span = scan
            .capture_string_literal()
            .map_err(|diag| walk_error(span_offset(&diag), describe_capture(&diag)))?;
        let decoded = string_value(src, span)?;
        if (decoded.starts_with('`') && decoded.ends_with('`') && decoded.len() >= 2)
            || (decoded.starts_with('⟨') && decoded.ends_with('⟩'))
        {
            // The interior is the printer's own quoted ident — read
            // it as one and carry the decoded name.
            let mut ident = Scanner::new(&decoded);
            if let Ok(Some((ident_span, text, _))) = ident.read_ident()
                && ident_span.len == decoded.len()
            {
                return Ok(text);
            }
        }
        return Ok(decoded);
    }
    match scan.read_ident() {
        Ok(Some((_, text, _))) => Ok(text),
        Ok(None) => Err(walk_error(
            scan.offset(),
            "expected a section key".to_string(),
        )),
        Err(diag) => Err(walk_error(
            diag.primary_label()
                .map_or(scan.offset(), |(_, s)| s.offset),
            "malformed escape in a quoted key".to_string(),
        )),
    }
}

/// Reads one leaf value plus whether its scan consumed the entry
/// separator (only the raw-scalar capture does). `depth` is the
/// nesting level of the object holding the value.
fn walk_value(src: &str, scan: &mut Scanner, depth: usize) -> Result<(InfoValue, LeafEnd), Diag> {
    match scan.peek_byte() {
        Some(b'{') => {
            // This object's child would sit at depth+1; the budget is
            // the maximum nesting level allowed (depth 0 counts).
            if depth + 1 >= MAX_INFO_DEPTH {
                return Err(Diag::InfoDepthExceeded {
                    span: Span::point(scan.offset()),
                    message: format!("the object nests deeper than {MAX_INFO_DEPTH} levels"),
                });
            }
            Ok((
                InfoValue::Object(walk_object(src, scan, depth + 1)?),
                LeafEnd::Unconsumed,
            ))
        }
        Some(b'\'' | b'"') => {
            let span = scan
                .capture_string_literal()
                .map_err(|diag| walk_error(span_offset(&diag), describe_capture(&diag)))?;
            Ok((
                InfoValue::Ddl(string_value(src, span)?),
                LeafEnd::Unconsumed,
            ))
        }
        _ => {
            // A raw scalar token (number, float, array, `NONE`, …):
            // captured to whichever of `,`/`}` ends the entry — both
            // at depth 0; array elements' commas ride inside the
            // brackets the skip set tracks.
            let (span, stop) = scan
                .capture_until(b",}")
                .map_err(|diag| walk_error(span_offset(&diag), describe_capture(&diag)))?;
            let text = span.slice(src).trim();
            if text.is_empty() {
                return Err(Diag::InfoUnknownShape {
                    span: Span::point(span.offset),
                    message: "expected a value".to_string(),
                });
            }
            let end = if stop == b'}' {
                LeafEnd::CloseBrace
            } else {
                LeafEnd::Comma
            };
            Ok((InfoValue::Other(text.to_string()), end))
        }
    }
}

/// Decodes a captured string literal's interior through the SurrealQL
/// escape table (the printer's own `EscapeWriter` table plus the
/// lexer's full input set).
fn string_value(src: &str, span: Span) -> Result<String, Diag> {
    let interior = &src[span.offset + 1..span.end() - 1];
    crate::lexer::unescape_escapes(interior).map_err(|at| {
        walk_error(
            span.offset + 1 + at,
            "malformed escape in a string value".to_string(),
        )
    })
}

fn span_offset(diag: &Diag) -> usize {
    diag.primary_label().map_or(0, |(_, span)| span.offset)
}

fn describe_capture(diag: &Diag) -> String {
    match diag {
        Diag::UnterminatedCapture { terminator, .. } => {
            format!("expected `{terminator}` before end of input")
        }
        Diag::UnterminatedString { .. } => "unterminated string value".to_string(),
        Diag::UnterminatedQuotedIdent { .. } => "unterminated quoted key".to_string(),
        Diag::UnbalancedCapture { bracket, .. } => format!("unbalanced `{bracket}` in the value"),
        _ => "malformed value".to_string(),
    }
}

// ---------------------------------------------------------------------
// The read model — the side channel the AST cannot carry
// ---------------------------------------------------------------------

/// One definition read out of an INFO section: the statement, the
/// name it is filed under, and the read-model markers its text
/// produced (facts the DDL carried that the AST
/// cannot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadEntry {
    pub name: String,
    pub stmt: Stmt,
    pub markers: Markers,
}

/// The read-model side channel (the type every later
/// reader feeds): provenance the INFO text carried that the AST
/// cannot hold. The normalizer and the differ
/// consume these; the readers only produce them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Markers {
    /// The statement routed to verbatim `raw` — the variant names the
    /// shape that forced it.
    pub raw_routed: Option<RawRoute>,
    /// The auto-seeded default namespace, identified by its comment
    /// fingerprint (spec/namespace.md's reverse section) — the differ
    /// reports it expected-unmanaged unless the SDL file declares it.
    pub auto_seeded_default_namespace: bool,
    /// A secret the server redacted before reporting (`[REDACTED]`) —
    /// fed by the medium readers (ACCESS key secrets, user
    /// hashes); never adopted as a literal.
    pub redacted: bool,
    /// `PASSHASH ''` — the stored marker of a secret-less user (fed
    /// by the medium readers); likewise never adopted as a literal.
    pub secretless_passhash: bool,
}

/// Why a statement routed to verbatim `raw` (README rule 8 — the
/// shapes the reverse sections license).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawRoute {
    /// A `COMMENT` value that is not a plain string literal (the
    /// normalization protocol's form-selection rule: such a comment
    /// routes the statement to `raw`).
    CommentNotPlainString,
    /// A module carrying both an explicit name and a silo
    /// executable (`mod::x AS silo::…` — spec/module.md's reverse
    /// section: the round-trip asymmetry SDL's grammar rejects).
    ModuleNamedSilo,
    /// An index whose stored scoring is the legacy `VS` (vector
    /// search) token — print-only in 3.2.4: the embedded parser
    /// rejects the DDL its own printer emits, and
    /// spec/index.md's reverse section routes the shape to verbatim
    /// `raw`. Reachable only from a database upgraded across
    /// SurrealDB versions.
    IndexLegacyVs,
    /// A view whose INFO deviates from SDL's fixed emission
    /// (`TYPE ANY SCHEMALESS AS SELECT …`): `TYPE NORMAL`/`RELATION`,
    /// `SCHEMAFULL`, `DROP`, or a `CHANGEFEED` on a view statement.
    /// Re-emitting SDL's fixed shape would silently mutate the table
    /// on re-apply (spec/table.md's reverse section), so the
    /// statement routes to verbatim `raw`.
    TableViewDeviant,
    /// An index kind parameter outside SDL's unsigned float scalar
    /// grammar — a negative `BM25` side, `ALPHA`, `LM`, or MINKOWSKI
    /// order (engine-legal and re-printed as such, live-probed;
    /// spec/index.md's reverse section routes unrepresentable-but-
    /// legal DDL to verbatim `raw`).
    IndexSignedFloat,
    /// A field with no TYPE and no COMPUTED (`DEFINE FIELD x ON t;` —
    /// engine-legal hand-run DDL, INFO prints it typeless; SDL's
    /// grammar demands a type on every non-computed field, so the
    /// statement routes to verbatim `raw` per spec/field.md's reverse
    /// section).
    FieldUntyped,
    /// A relation whose FIRST `IN` endpoint needs ident escaping
    /// (`` `a.b` `` — engine-legal, live-probed; the arrow form's
    /// first from-side element prints bare, so re-emitting SDL's
    /// header would not re-parse — spec/table.md's reverse section
    /// routes the statement to verbatim `raw`).
    TableEndpointUnrepresentable,
    /// A second `FOR any` clause in one api statement — SDL's single
    /// `any` rule is a parse error, so the statement is inexpressible
    /// and routes to verbatim `raw` (spec/api.md's reverse section).
    /// Unreachable from live INFO: upstream folds repeated `FOR ANY`
    /// config-last-wins at define time, so the catalog never stores
    /// two (live-probed) — reachable only from hand-fed interchange
    /// text.
    ApiDuplicateAny,
}

// ---------------------------------------------------------------------
// The DDL splitters (over the pinned printer dialect)
// ---------------------------------------------------------------------

/// One statement's split outcome.
#[derive(Debug)]
enum SplitOutcome {
    /// A definition the AST carries. The statement boxes: the raw
    /// route is one word wide, and the lint holds the two arms
    /// near-equal.
    Stmt {
        name: String,
        stmt: Box<Stmt>,
        markers: Markers,
    },
    /// The statement routes to verbatim `raw`; the reader wraps the
    /// original DDL string (edges trimmed).
    Raw(RawRoute),
}

/// The `COMMENT` clause's outcome: a decoded doc comment, or the not-
/// a-plain-string shape that routes the statement to `raw`.
enum CommentValue {
    Doc(DocComment),
    NotPlainString,
}

/// Keyword-level splitter over one canonical DDL statement — the
/// SDL-side structure extraction. Every shape constant
/// matched here (the clause sets, the always-materialized clauses,
/// the stop lists of the value captures) is pinned by tripwire tests
/// against live engine output (README codegen model: the emitter is
/// the authority on INFO output shape).
struct Splitter<'src> {
    src: &'src str,
    lexer: Scanner<'src>,
    /// Set when a shape routes the statement to verbatim `raw`: the
    /// clause loop returns immediately (the routed statement's
    /// remaining text is discarded, never validated), and the
    /// splitter's caller checks this before anything else.
    raw: Option<RawRoute>,
}

impl<'src> Splitter<'src> {
    fn new(src: &'src str) -> Self {
        Splitter {
            src,
            lexer: Scanner::new(src),
            raw: None,
        }
    }

    /// Flags the statement for the verbatim-`raw` route.
    fn route_raw(&mut self, route: RawRoute) {
        self.raw = Some(route);
    }

    /// The next bare word, trivia skipped.
    fn word(&mut self) -> Option<&'src str> {
        self.lexer.skip_surrealql_trivia();
        self.lexer.read_word().map(|span| span.slice(self.src))
    }

    /// Whether the next bare word (trivia skipped, the word itself
    /// left unconsumed) spells `expected` ASCII case-insensitively.
    fn peek_word_is(&mut self, expected: &str) -> bool {
        self.lexer.skip_surrealql_trivia();
        self.lexer
            .peek_word()
            .is_some_and(|span| span.slice(self.src).eq_ignore_ascii_case(expected))
    }

    /// The next bare word, which must spell `expected`
    /// (ASCII case-insensitively — the pinned dialect spells clauses
    /// uppercase, but INFO keywords are case-insensitive input).
    fn expect_word(&mut self, expected: &str, what: &str) -> Result<(), Diag> {
        match self.word() {
            Some(word) if word.eq_ignore_ascii_case(expected) => Ok(()),
            other => Err(tripwire(format!(
                "expected `{expected}` ({what}), found `{}`",
                other.unwrap_or("<end>")
            ))),
        }
    }

    /// One bare atom token (a number, a float with the printer's `f`
    /// suffix, a keyword word like `F32`): trivia skipped, punctuation
    /// ends it. An optional leading `-` is consumed — the engine
    /// accepts and re-prints signed kind floats (`BM25(-1,-2)`,
    /// `ALPHA -1.2f`); the readers route those to verbatim `raw`, and
    /// the sign must ride the atom for the peel to see the real value.
    /// The kind tail's parameter values are single tokens in the
    /// pinned dialect, so a capture-to-next-stop is never needed.
    fn atom(&mut self) -> Option<&'src str> {
        self.lexer.skip_surrealql_trivia();
        let start = self.lexer.offset();
        if self.lexer.peek_byte() == Some(b'-') {
            self.lexer.advance_by(1);
        }
        while let Some(b) = self.lexer.peek_byte() {
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'.' {
                self.lexer.advance_by(1);
            } else {
                break;
            }
        }
        if self.lexer.offset() == start {
            None
        } else {
            Some(&self.src[start..self.lexer.offset()])
        }
    }

    /// A clause value captured to one of the statement's other clause
    /// keywords (or its `;`): the canonical text for the AST's
    /// expression clause — already this printer's own spelling. The
    /// stops carry follow shapes (the capture-grammar split): an
    /// expression-valued clause may legitimately contain a stop word,
    /// which only ends the capture when its own value's shape
    /// confirms the clause.
    fn capture_expr(&mut self, stops: &[CaptureStopWord], clause: &str) -> Result<String, Diag> {
        let (span, _) = self
            .lexer
            .capture_until_word(stops)
            .map_err(|diag| tripwire(describe_clause_capture(&diag)))?;
        let text = span.slice(self.src).trim();
        if text.is_empty() {
            return Err(tripwire(format!("the `{clause}` clause value is empty")));
        }
        Ok(text.to_string())
    }

    /// A plain string literal clause value (`PASSHASH`, the graphql
    /// function options): the decoded interior. A missing literal is
    /// dialect drift.
    fn capture_string(&mut self, clause: &str) -> Result<String, Diag> {
        self.lexer.skip_surrealql_trivia();
        if !matches!(self.lexer.peek_byte(), Some(b'\'' | b'"')) {
            return Err(tripwire(format!(
                "expected a string literal as the `{clause}` value"
            )));
        }
        let span = self
            .lexer
            .capture_string_literal()
            .map_err(|diag| tripwire(describe_clause_capture(&diag)))?;
        string_value(self.src, span).map_err(|diag| tripwire(diag.to_string()))
    }

    /// An identifier clause value (event `ON`'s table): a bare word
    /// or quoted identifier, unescaped.
    fn capture_ident(&mut self, clause: &str) -> Result<String, Diag> {
        self.lexer.skip_surrealql_trivia();
        match self.lexer.read_ident() {
            Ok(Some((_, text, _))) => Ok(text),
            Ok(None) => Err(tripwire(format!("expected an identifier after `{clause}`"))),
            Err(_) => Err(tripwire(format!(
                "malformed escape in the `{clause}` identifier"
            ))),
        }
    }

    /// A `COMMENT` clause value: a plain string literal decoded to
    /// doc-comment lines (embedded `\n` splits). Anything else — a
    /// prefixed string, or any other SurrealQL literal/expression the
    /// embedded parser accepted (upstream parses the clause as an
    /// expression field) — is "not a plain string literal" per the
    /// normalization protocol's form-selection rule and routes the
    /// statement to `raw`.
    fn capture_comment(&mut self) -> Result<CommentValue, Diag> {
        self.lexer.skip_surrealql_trivia();
        match self.lexer.peek_byte() {
            Some(b'\'' | b'"') => {
                let span = self
                    .lexer
                    .capture_string_literal()
                    .map_err(|diag| tripwire(describe_clause_capture(&diag)))?;
                let value =
                    string_value(self.src, span).map_err(|diag| tripwire(diag.to_string()))?;
                Ok(CommentValue::Doc(DocComment {
                    lines: value.split('\n').map(str::to_string).collect(),
                }))
            }
            _ => Ok(CommentValue::NotPlainString),
        }
    }

    /// A `PERMISSIONS` value after its keyword: `NONE`, `FULL`, or
    /// `WHERE <expr>` (the single-value form).
    fn capture_permission(&mut self, stops: &[CaptureStopWord]) -> Result<Permission, Diag> {
        match self.word() {
            Some(word) if word.eq_ignore_ascii_case("NONE") => Ok(Permission::None),
            Some(word) if word.eq_ignore_ascii_case("FULL") => Ok(Permission::Full),
            Some(word) if word.eq_ignore_ascii_case("WHERE") => Ok(Permission::Expr(Expr {
                canon: self.capture_expr(stops, "PERMISSIONS")?,
            })),
            other => Err(tripwire(format!(
                "expected `NONE`, `FULL`, or `WHERE` after `PERMISSIONS`, found `{}`",
                other.unwrap_or("<end>")
            ))),
        }
    }

    /// After the clauses: only trivia may remain (the clause loop or
    /// a value capture consumed the statement's `;`).
    fn require_end(&mut self) -> Result<(), Diag> {
        self.lexer.skip_surrealql_trivia();
        if self.lexer.is_at_end() {
            Ok(())
        } else {
            Err(tripwire(
                "text remains after the definition's `;`".to_string(),
            ))
        }
    }

    /// The positional readers' statement end ([`Self::finish`] of the
    /// splitter's two shapes): unlike the clause loop — which eats the
    /// trailing `;` mid-loop — a positional read leaves it standing.
    fn finish(&mut self) -> Result<(), Diag> {
        self.lexer.skip_surrealql_trivia();
        self.lexer.eat_byte(b';');
        self.require_end()
    }

    /// The clause loop: read clause keywords to the statement's `;`
    /// (or end — INFO DDL carries none in its strings; the
    /// canonicalized re-print adds one), handing each to `on_clause`
    /// with the lowercased key. The shared scaffold of every
    /// statement splitter; the closure
    /// folds the values and rejects unknown or duplicated clauses.
    /// Once [`Self::route_raw`] has fired the loop returns
    /// immediately — the routed statement's remaining text is
    /// discarded, never validated.
    fn clause_loop(
        &mut self,
        statement: &str,
        on_clause: &mut dyn FnMut(&mut Self, &str) -> Result<(), Diag>,
    ) -> Result<(), Diag> {
        loop {
            if self.raw.is_some() {
                return Ok(());
            }
            self.lexer.skip_surrealql_trivia();
            if self.lexer.is_at_end() || self.lexer.eat_byte(b';') {
                return Ok(());
            }
            let Some(word) = self.word() else {
                return Err(tripwire(format!("expected a `{statement}` clause keyword")));
            };
            on_clause(self, word.to_ascii_lowercase().as_str())?;
            self.lexer.skip_surrealql_trivia();
        }
    }
}

/// An entry-level diagnostic: always anchored at the DDL string's
/// start (the span contract above).
fn tripwire(message: String) -> Diag {
    Diag::DdlTripwire {
        span: Span::point(0),
        message,
    }
}

fn describe_clause_capture(diag: &Diag) -> String {
    match diag {
        Diag::UnterminatedCapture { terminator, .. } => {
            format!("expected `{terminator}` before end of statement")
        }
        Diag::UnterminatedString { .. } => "unterminated string in the clause value".to_string(),
        Diag::UnterminatedRegex { .. } => "unterminated regex in the clause value".to_string(),
        Diag::UnterminatedQuotedIdent { .. } => "unterminated quoted identifier".to_string(),
        Diag::UnbalancedCapture { bracket, .. } => {
            format!("unbalanced `{bracket}` in the clause value")
        }
        _ => "malformed clause value".to_string(),
    }
}

/// A clause key seen twice — the canonical print states each clause
/// once; a repeat is dialect drift.
fn duplicated(clause: &str) -> Diag {
    tripwire(format!("the `{clause}` clause appears more than once"))
}

/// A canonical INFO integer (BATCH/START materialize as plain
/// literals — define-time evaluation leaves no expression behind).
fn uint_atom(text: &str, clause: &str) -> Result<(), Diag> {
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(());
    }
    Err(tripwire(format!(
        "`{text}` is not the integer literal the `{clause}` clause materializes"
    )))
}

/// The header every simple DDL statement shares: `DEFINE`, the kind
/// word, and an optional `OVERWRITE` / `IF NOT EXISTS` kind prefix —
/// stripped either way (README normalization item 4). Both prefix
/// orders parse: the engine's own `DEFINE <kind> OVERWRITE …` (our
/// codegen's emission shape) and the prefix-first spelling. The kind
/// must match the section the string arrived in, or the shape
/// diagnoses.
fn expect_header(split: &mut Splitter, kind: &str) -> Result<(), Diag> {
    split.expect_word("DEFINE", "every definition starts with it")?;
    if split.peek_word_is(kind) {
        split.expect_word(kind, "the definition kind must match its INFO section")?;
        if split.peek_word_is("OVERWRITE") {
            split.expect_word("OVERWRITE", "the kind prefix")?;
        } else if split.peek_word_is("IF") {
            split.expect_word("IF", "the `IF NOT EXISTS` prefix")?;
            split.expect_word("NOT", "`IF NOT EXISTS`")?;
            split.expect_word("EXISTS", "`IF NOT EXISTS`")?;
        }
        return Ok(());
    }
    if split.peek_word_is("OVERWRITE") {
        split.expect_word("OVERWRITE", "the kind prefix")?;
    } else if split.peek_word_is("IF") {
        split.expect_word("IF", "the `IF NOT EXISTS` prefix")?;
        split.expect_word("NOT", "`IF NOT EXISTS`")?;
        split.expect_word("EXISTS", "`IF NOT EXISTS`")?;
    } else {
        let found = split
            .word()
            .map(|word| format!("`DEFINE {word}`"))
            .unwrap_or_else(|| "`DEFINE <end>`".to_string());
        return Err(Diag::InfoUnknownShape {
            span: Span::point(0),
            message: format!("expected a {kind} definition, found {found}"),
        });
    }
    split
        .expect_word(kind, "the definition kind must match its INFO section")
        .map_err(|diag| match diag {
            Diag::DdlTripwire { message, .. } => Diag::InfoUnknownShape {
                span: Span::point(0),
                message,
            },
            diag => diag,
        })
}

/// A header name: a bare word or a quoted identifier, unescaped (the
/// inverse of the printer's ident escaping).
fn header_name(split: &mut Splitter) -> Result<String, Diag> {
    split.lexer.skip_surrealql_trivia();
    match split.lexer.read_ident() {
        Ok(Some((_, text, _))) => Ok(text),
        Ok(None) => Err(tripwire("expected the definition's name".to_string())),
        Err(_) => Err(tripwire(
            "malformed escape in the definition's name".to_string(),
        )),
    }
}

/// Reads one DDL string of `kind` filed under `name` in its section:
/// canonicalize, split, then wrap. The `kind` word /
/// name cross-checks diagnose [`Diag::InfoUnknownShape`] — INFO keys
/// and headers always agree.
fn read_definition(
    name: &str,
    ddl: &str,
    split: fn(&str) -> Result<SplitOutcome, Diag>,
) -> Result<ReadEntry, Diag> {
    let canonical = canonicalize_statement(ddl).map_err(|error| Diag::InfoUnparsable {
        span: Span::point(0),
        message: match error {
            EmbeddedError::Rejected { message } => {
                format!("the embedded parser rejected `{ddl}`: {message}")
            }
            EmbeddedError::Internal { message } => message,
        },
    })?;
    split_outcome(name, ddl, split(&canonical)?)
}

/// Wraps a splitter's outcome into the read entry: the header name /
/// INFO key cross-check, and the raw route's verbatim body (the
/// ORIGINAL DDL text — edges trimmed, interior verbatim).
fn split_outcome(name: &str, ddl: &str, outcome: SplitOutcome) -> Result<ReadEntry, Diag> {
    match outcome {
        SplitOutcome::Stmt {
            name: header_name,
            stmt,
            markers,
        } => {
            // The engine stores a named module under `mod::<name>` —
            // the catalog's own get_storage_name derivation
            // (surrealdb-catalog-3.3.0/src/schema/module.rs,
            // source-verified) — while the definition's header, and
            // SDL, spell the bare name (upstream's parser eats `mod::`
            // as grammar). Pair either spelling; the entry records the
            // SDL-side name so the differ pairs SDL keys against it
            // (issue #61: a post-registration capture refused here as
            // E3002).
            let storage_stripped = name.strip_prefix("mod::").unwrap_or(name);
            if header_name != name && header_name != storage_stripped {
                return Err(Diag::InfoUnknownShape {
                    span: Span::point(0),
                    message: format!(
                        "the definition's header name `{header_name}` disagrees with its \
                         INFO key `{name}`"
                    ),
                });
            }
            let paired = if header_name == name {
                name.to_string()
            } else {
                header_name
            };
            Ok(ReadEntry {
                name: paired,
                stmt: *stmt,
                markers,
            })
        }
        SplitOutcome::Raw(route) => Ok(raw_read_entry(name, ddl, route)),
    }
}

/// Reads one INFO section of DDL strings through its per-entry
/// reader (simple tiers: [`read_definition`] over their splitter;
/// `config` prepends the definition header to its fragments;
/// `module` reads SDL-side without the canonicalizing parse).
/// Absent sections read as none of the kind (real INFO omits
/// what the scope holds nothing of); a present section of the wrong
/// shape, or an entry that is not a DDL string, diagnoses.
fn read_section(
    info: &InfoObject,
    section: &str,
    read: impl Fn(&str, &str) -> Result<ReadEntry, Diag>,
) -> Vec<Result<ReadEntry, Diag>> {
    let Some(InfoValue::Object(entries)) = info.0.get(section) else {
        return if info.0.contains_key(section) {
            vec![Err(Diag::InfoUnknownShape {
                span: Span::point(0),
                message: format!("the `{section}` section is not an object"),
            })]
        } else {
            Vec::new()
        };
    };
    entries
        .0
        .iter()
        .map(|(name, value)| {
            let InfoValue::Ddl(ddl) = value else {
                return Err(Diag::InfoUnknownShape {
                    span: Span::point(0),
                    message: format!("the `{section}` entry `{name}` is not a DDL string"),
                });
            };
            read(name, ddl)
        })
        .collect()
}

// ---------------------------------------------------------------------
// The five simple statement readers
// ---------------------------------------------------------------------

/// The auto-seeded default namespace's comment fingerprint
/// (spec/namespace.md, verified against the server): a namespace
/// carrying exactly this comment is the server's own seeding, not a
/// definition the SDL file manages.
pub(crate) const AUTO_SEEDED_COMMENT: &str = "Default namespace generated by SurrealDB";

/// The `namespaces` section of `INFO FOR ROOT` (spec/namespace.md):
/// name → DDL, `COMMENT` mapping back to `///` lines. The auto-seeded
/// default namespace rides the fingerprint marker.
pub fn read_namespaces(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "namespaces", |name, ddl| {
        read_definition(name, ddl, split_namespace)
    })
}

fn split_namespace(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "NAMESPACE")?;
    let name = header_name(&mut split)?;
    let mut comment = None;
    split.clause_loop("namespace", &mut |split, key| {
        match key {
            "comment" => {
                if comment.is_some() {
                    return Err(duplicated("COMMENT"));
                }
                match split.capture_comment()? {
                    CommentValue::Doc(doc) => comment = Some(doc),
                    CommentValue::NotPlainString => {
                        split.route_raw(RawRoute::CommentNotPlainString);
                    }
                }
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the namespace definition"
                )));
            }
        }
        Ok(())
    })?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.require_end()?;
    let mut markers = Markers::default();
    if comment
        .as_ref()
        .is_some_and(|doc| doc.joined() == AUTO_SEEDED_COMMENT)
    {
        markers.auto_seeded_default_namespace = true;
    }
    Ok(SplitOutcome::Stmt {
        markers,
        stmt: Box::new(Stmt::Namespace(Namespace {
            name: Ident::new(name.clone()),
            keyword: Carried::default(),
            comment,
            comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

/// The `databases` section of `INFO FOR NS` (spec/database.md).
/// STRICT is never reported (active yet absent — preserve is the
/// normalizer's verb, so the read result simply carries none);
/// changefeed durations compare by value, never spelling.
pub fn read_databases(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "databases", |name, ddl| {
        read_definition(name, ddl, split_database)
    })
}

fn split_database(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "DATABASE")?;
    let name = header_name(&mut split)?;
    let mut changefeed = None;
    let mut comment = None;
    split.clause_loop("database", &mut |split, key| {
        match key {
            "comment" => {
                if comment.is_some() {
                    return Err(duplicated("COMMENT"));
                }
                match split.capture_comment()? {
                    CommentValue::Doc(doc) => comment = Some(doc),
                    CommentValue::NotPlainString => {
                        split.route_raw(RawRoute::CommentNotPlainString);
                    }
                }
            }
            "changefeed" => {
                if changefeed.is_some() {
                    return Err(duplicated("CHANGEFEED"));
                }
                let text = split.capture_expr(
                    &[CaptureStopWord("COMMENT", CaptureFollow::Value)],
                    "CHANGEFEED",
                )?;
                changefeed = Some(parse_changefeed(&text)?);
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the database definition"
                )));
            }
        }
        Ok(())
    })?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.require_end()?;
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Database(Database {
            name: Ident::new(name.clone()),
            keyword: Carried::default(),
            strict: false,
            changefeed,
            comment,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

/// `CHANGEFEED <duration> [INCLUDE ORIGINAL]` — the capture carries
/// both; the duration parses by value (the printer re-decomposes:
/// `7d` arrives as `1w`), never by spelling.
fn parse_changefeed(text: &str) -> Result<ChangeFeed, Diag> {
    const INCLUDE_ORIGINAL: &str = "INCLUDE ORIGINAL";
    let mut expiry = text.trim();
    let include_original = expiry.len() >= INCLUDE_ORIGINAL.len()
        && expiry[expiry.len() - INCLUDE_ORIGINAL.len()..].eq_ignore_ascii_case(INCLUDE_ORIGINAL);
    if include_original {
        expiry = expiry[..expiry.len() - INCLUDE_ORIGINAL.len()].trim_end();
    }
    match crate::parser::parse_duration_text(expiry) {
        Some(duration) => Ok(ChangeFeed {
            expiry: duration,
            include_original,
        }),
        None => Err(tripwire(format!(
            "`{text}` is not a changefeed duration (with optional INCLUDE ORIGINAL)"
        ))),
    }
}

/// The `sequences` section of `INFO FOR DB` (spec/sequence.md):
/// `BATCH`/`START` always materialize (the defaults ride the read
/// result — elision is the normalizer's); `TIMEOUT` is omitted when
/// absent, and the `TIMEOUT NONE` sentinel stores as absent too.
pub fn read_sequences(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "sequences", |name, ddl| {
        read_definition(name, ddl, split_sequence)
    })
}

fn split_sequence(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "SEQUENCE")?;
    let name = header_name(&mut split)?;
    let mut start = None;
    let mut batch = None;
    let mut timeout = None;
    let mut saw_batch = false;
    let mut saw_start = false;
    split.clause_loop("sequence", &mut |split, key| {
        match key {
            "batch" => {
                if saw_batch {
                    return Err(duplicated("BATCH"));
                }
                let text = split.capture_expr(
                    &[
                        CaptureStopWord("START", CaptureFollow::Any),
                        CaptureStopWord("TIMEOUT", CaptureFollow::Any),
                    ],
                    "BATCH",
                )?;
                uint_atom(&text, "BATCH")?;
                batch = Some(Expr { canon: text });
                saw_batch = true;
            }
            "start" => {
                if saw_start {
                    return Err(duplicated("START"));
                }
                let text = split
                    .capture_expr(&[CaptureStopWord("TIMEOUT", CaptureFollow::Any)], "START")?;
                uint_atom(&text, "START")?;
                start = Some(Expr { canon: text });
                saw_start = true;
            }
            "timeout" => {
                if timeout.is_some() {
                    return Err(duplicated("TIMEOUT"));
                }
                let text = split.capture_expr(&[], "TIMEOUT")?;
                // The sentinel stores as the absent clause (the
                // emitter never prints TIMEOUT for it — probe-pinned);
                // a present timeout is a duration literal.
                if !text.eq_ignore_ascii_case("NONE") {
                    if crate::parser::parse_duration_text(&text).is_none() {
                        return Err(tripwire(format!("`{text}` is not a TIMEOUT duration")));
                    }
                    timeout = Some(Expr { canon: text });
                }
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the sequence definition"
                )));
            }
        }
        Ok(())
    })?;
    split.require_end()?;
    if !saw_batch || !saw_start {
        return Err(tripwire(
            "BATCH and START always materialize in INFO — one is missing".to_string(),
        ));
    }
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Sequence(Sequence {
            name: Ident::new(name.clone()),
            start,
            batch,
            timeout,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

/// The `params` section of `INFO FOR DB` (spec/param.md): `VALUE`
/// always renders (`VALUE NONE` is the valueless param — read
/// faithfully as no value, not elided), `PERMISSIONS` always renders
/// (`FULL` is the default), and the value is the define-time
/// evaluated result literal — not the SDL source expression.
pub fn read_params(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "params", |name, ddl| {
        read_definition(name, ddl, split_param)
    })
}

fn split_param(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "PARAM")?;
    split.lexer.skip_surrealql_trivia();
    if !split.lexer.eat_byte(b'$') {
        return Err(tripwire("expected `$` opening the param name".to_string()));
    }
    let name = match split.lexer.read_ident() {
        Ok(Some((_, text, _))) => text,
        Ok(None) | Err(_) => {
            return Err(tripwire("expected the param name after `$`".to_string()));
        }
    };
    let mut value = None;
    let mut permissions = None;
    let mut comment = None;
    let mut saw_value = false;
    let mut saw_permissions = false;
    split.clause_loop("param", &mut |split, key| {
        match key {
            "value" => {
                if saw_value {
                    return Err(duplicated("VALUE"));
                }
                // The capture-grammar split (the
                // stop-word corner): a value like `comment:1` (a
                // record id over a table named `comment`) carries the
                // stop word as expression content — only the string
                // / permission value shapes confirm the clause.
                let text = split.capture_expr(
                    &[
                        CaptureStopWord("COMMENT", CaptureFollow::Value),
                        CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
                    ],
                    "VALUE",
                )?;
                // VALUE NONE is the valueless param (faithful
                // mapping — the clause-less SDL form, linted).
                if !text.eq_ignore_ascii_case("NONE") {
                    value = Some(Expr { canon: text });
                }
                saw_value = true;
            }
            "comment" => {
                if comment.is_some() {
                    return Err(duplicated("COMMENT"));
                }
                match split.capture_comment()? {
                    CommentValue::Doc(doc) => comment = Some(doc),
                    CommentValue::NotPlainString => {
                        split.route_raw(RawRoute::CommentNotPlainString);
                    }
                }
            }
            "permissions" => {
                if saw_permissions {
                    return Err(duplicated("PERMISSIONS"));
                }
                permissions = Some(split.capture_permission(&[])?);
                saw_permissions = true;
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the param definition"
                )));
            }
        }
        Ok(())
    })?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.require_end()?;
    if !saw_value || !saw_permissions {
        return Err(tripwire(
            "VALUE and PERMISSIONS always render in INFO — one is missing".to_string(),
        ));
    }
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Param(Param {
            name: ParamName::new(name.clone()),
            value,
            permissions,
            comment,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

/// The `buckets` section of `INFO FOR DB` (spec/bucket.md):
/// `PERMISSIONS` always renders; the DDL parses only under the
/// files capability, which the canonicalizing parse enables.
pub fn read_buckets(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "buckets", |name, ddl| {
        read_definition(name, ddl, split_bucket)
    })
}

fn split_bucket(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "BUCKET")?;
    let name = header_name(&mut split)?;
    let mut readonly = false;
    let mut backend = None;
    let mut permissions = None;
    let mut comment = None;
    let mut saw_permissions = false;
    split.clause_loop("bucket", &mut |split, key| {
        match key {
            "readonly" => {
                if readonly {
                    return Err(duplicated("READONLY"));
                }
                readonly = true;
            }
            "backend" => {
                if backend.is_some() {
                    return Err(duplicated("BACKEND"));
                }
                backend = Some(Expr {
                    canon: split.capture_expr(
                        &[
                            CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
                            CaptureStopWord("COMMENT", CaptureFollow::Value),
                        ],
                        "BACKEND",
                    )?,
                });
            }
            "permissions" => {
                if saw_permissions {
                    return Err(duplicated("PERMISSIONS"));
                }
                permissions = Some(
                    split
                        .capture_permission(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])?,
                );
                saw_permissions = true;
            }
            "comment" => {
                if comment.is_some() {
                    return Err(duplicated("COMMENT"));
                }
                match split.capture_comment()? {
                    CommentValue::Doc(doc) => comment = Some(doc),
                    CommentValue::NotPlainString => {
                        split.route_raw(RawRoute::CommentNotPlainString);
                    }
                }
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the bucket definition"
                )));
            }
        }
        Ok(())
    })?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.require_end()?;
    if !saw_permissions {
        return Err(tripwire(
            "PERMISSIONS always renders in INFO — it is missing".to_string(),
        ));
    }
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Bucket(Bucket {
            name: Ident::new(name.clone()),
            backend,
            readonly,
            permissions,
            comment,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

// ---------------------------------------------------------------------
// The six medium statement readers
// ---------------------------------------------------------------------

/// The value atoms of a list clause split at depth-0 commas
/// (`EDGENGRAM(2,10)`'s inner comma never splits; the final item
/// rides to the end of its capture without one); each item is
/// trimmed and handed to `parse`, and the first malformed one
/// diagnoses.
fn comma_items<T>(
    text: &str,
    clause: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<Vec<T>, Diag> {
    let mut scan = Scanner::new(text);
    let mut items = Vec::new();
    loop {
        let start = scan.offset();
        let item = match scan.capture_until(b",") {
            Ok((span, _)) => span.slice(text).trim(),
            // The list's final item rode to the end of its capture.
            Err(Diag::UnterminatedCapture { .. }) => text[start..].trim(),
            Err(_) => {
                return Err(tripwire(format!(
                    "the `{clause}` clause value is malformed"
                )));
            }
        };
        let Some(parsed) = parse(item) else {
            return Err(tripwire(format!(
                "`{item}` is not an atom the `{clause}` clause materializes"
            )));
        };
        items.push(parsed);
        if scan.is_at_end() {
            return Ok(items);
        }
    }
}

/// `::<seg>` — one path segment after its `::` join (the silo
/// executable's `::<org>::<pkg>`).
fn path_segment_after_colons(split: &mut Splitter, clause: &str) -> Result<String, Diag> {
    split.lexer.skip_surrealql_trivia();
    if !split.lexer.rest().starts_with("::") {
        return Err(tripwire(format!("expected `::` in the `{clause}` path")));
    }
    split.lexer.advance_by(2);
    split.capture_ident(clause)
}

/// `fn::`/`mod::<seg>[::<seg>…]` over one position — the bare keyword
/// head (a quoted spelling is an identifier, never the keyword), then
/// at least one segment. `what` names the clause for the tripwires.
fn prefixed_path(split: &mut Splitter, prefix: &str, what: &str) -> Result<Vec<String>, Diag> {
    split.lexer.skip_surrealql_trivia();
    match split.lexer.read_ident() {
        Ok(Some((_, word, false))) if word.eq_ignore_ascii_case(prefix) => {}
        Ok(Some((_, word, _))) => {
            return Err(tripwire(format!(
                "expected the `{prefix}::` prefix in the `{what}` value, found `{word}`"
            )));
        }
        _ => {
            return Err(tripwire(format!(
                "expected the `{prefix}::` prefix in the `{what}` value"
            )));
        }
    }
    split.lexer.skip_surrealql_trivia();
    if !split.lexer.rest().starts_with("::") {
        return Err(tripwire(format!(
            "expected `::` after the `{prefix}` prefix in the `{what}` value"
        )));
    }
    split.lexer.advance_by(2);
    let mut segments = Vec::new();
    loop {
        let segment = split.capture_ident(what)?;
        segments.push(segment);
        split.lexer.skip_surrealql_trivia();
        if split.lexer.rest().starts_with("::") {
            split.lexer.advance_by(2);
            continue;
        }
        return Ok(segments);
    }
}

/// One kind production at the cursor (function parameters, the `->`
/// return): the shared kind reader consumes the production and the
/// scanner advances over it; the trimmed spelling rides `Kind`
/// verbatim (the printer normalized it — `option<int>` arrives as
/// `none | int` — and same_definition compares parsed kind ASTs).
fn capture_kind(split: &mut Splitter) -> Result<String, Diag> {
    split.lexer.skip_surrealql_trivia();
    let rest = split.lexer.rest();
    let Some(tail) = crate::kind::read_kind(rest) else {
        return Err(tripwire("expected a kind".to_string()));
    };
    let len = rest.len() - tail.len();
    let text = rest[..len].trim().to_string();
    split.lexer.advance_by(len);
    Ok(text)
}

/// The `analyzers` section of `INFO FOR DB` (spec/analyzer.md):
/// nothing materializes — the reverse mapping is 1:1, list order is
/// preserved and significant, and the printer's UPPERCASE filter
/// atoms / comma-without-space tokenizer list read back to values.
pub fn read_analyzers(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "analyzers", |name, ddl| {
        read_definition(name, ddl, split_analyzer)
    })
}

fn split_analyzer(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "ANALYZER")?;
    let name = header_name(&mut split)?;
    let mut function: Option<FnPath> = None;
    let mut tokenizers: Option<Vec<Tokenizer>> = None;
    let mut filters: Option<Vec<Filter>> = None;
    let mut comment = None;
    split.clause_loop("analyzer", &mut |split, key| {
        match key {
            "function" => {
                if function.is_some() {
                    return Err(duplicated("FUNCTION"));
                }
                let segments = prefixed_path(split, "fn", "FUNCTION")?;
                function = Some(FnPath {
                    kind: FnKind::Fn,
                    segments,
                });
            }
            "tokenizers" => {
                if tokenizers.is_some() {
                    return Err(duplicated("TOKENIZERS"));
                }
                let text = split.capture_expr(
                    &[
                        CaptureStopWord("FILTERS", CaptureFollow::Any),
                        CaptureStopWord("COMMENT", CaptureFollow::Value),
                    ],
                    "TOKENIZERS",
                )?;
                tokenizers = Some(comma_items(&text, "TOKENIZERS", tokenizer_atom)?);
            }
            "filters" => {
                if filters.is_some() {
                    return Err(duplicated("FILTERS"));
                }
                let text = split.capture_expr(
                    &[CaptureStopWord("COMMENT", CaptureFollow::Value)],
                    "FILTERS",
                )?;
                filters = Some(comma_items(&text, "FILTERS", filter_atom)?);
            }
            "comment" => {
                if comment.is_some() {
                    return Err(duplicated("COMMENT"));
                }
                match split.capture_comment()? {
                    CommentValue::Doc(doc) => comment = Some(doc),
                    CommentValue::NotPlainString => {
                        split.route_raw(RawRoute::CommentNotPlainString);
                    }
                }
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the analyzer definition"
                )));
            }
        }
        Ok(())
    })?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.require_end()?;
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Analyzer(Analyzer {
            name: Ident::new(name.clone()),
            function,
            tokenizers,
            filters,
            comment,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

/// One tokenizer atom (canonical INFO spells it UPPERCASE).
fn tokenizer_atom(text: &str) -> Option<Tokenizer> {
    match text.to_ascii_lowercase().as_str() {
        "blank" => Some(Tokenizer::Blank),
        "camel" => Some(Tokenizer::Camel),
        "class" => Some(Tokenizer::Class),
        "punct" => Some(Tokenizer::Punct),
        _ => None,
    }
}

/// One filter atom over the canonical text: the printer spells the
/// atoms UPPERCASE (`EDGENGRAM(2,10)`, `SNOWBALL(ENGLISH)`) while the
/// shared forward grammar's words are lowercase — fold the name word
/// (and a snowball's whole paren interior, always one bare language
/// word; never the mapper path's quoted content) and ride
/// [`crate::parser::parse_filter_atom`].
fn filter_atom(text: &str) -> Option<Filter> {
    let text = text.trim();
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    let (name, rest) = text.split_at(end);
    if name.is_empty() {
        return None;
    }
    let lowered = name.to_ascii_lowercase();
    let folded = if lowered == "snowball" {
        let (open, tail) = rest.split_once('(')?;
        let (language, close) = tail.split_once(')')?;
        format!("{open}({}){close}", language.to_ascii_lowercase())
    } else {
        rest.to_string()
    };
    crate::parser::parse_filter_atom(&format!("{lowered}{folded}"))
}

/// The `users` section of `INFO FOR ROOT` / `INFO FOR NS` /
/// `INFO FOR DB` (spec/user.md — the same name may exist at several
/// levels; `ON <LEVEL>` rides the text). `ROLES` and both durations
/// always materialize (`ROLES VIEWER`, `DURATION FOR TOKEN 1h, FOR
/// SESSION NONE`) — the defaults ride the read result, never elided
/// here. The secret sentinels never adopt as literals:
/// `PASSHASH '[REDACTED]'` feeds [`Markers::redacted`], `PASSHASH ''`
/// feeds [`Markers::secretless_passhash`], and `pass` stays `None`.
pub fn read_users(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "users", |name, ddl| {
        read_definition(name, &strip_passscram(ddl), split_user)
    })
}

/// Strips the 3.3.0 `PASSSCRAM '[REDACTED]'` clause (probe-pinned
/// against the live engine, 2026-09-25): it renders only for users
/// with a stored SCRAM verifier — SDL-defined `password:` users; the
/// hash-defined and secretless shapes never carry it — and its value
/// is ALWAYS the redaction sentinel, because a SCRAM verifier is a
/// secret and INFO redacts secrets. The sentinel spelling is
/// define-rejected upstream (`Invalid SCRAM verifier: missing
/// mechanism separator` — parse-time validation), so the clause can
/// never survive the canonicalizing parse, let alone a re-apply: it
/// is removed before validation, feeding no marker (the PASSHASH
/// sentinel already carries the redaction fact, and a raw-routed user
/// body must stay executable). Exact-splice is fail-closed for the
/// printer's dialect: `'` never appears unescaped inside a
/// single-quoted literal, so the byte sequence can only be the
/// depth-0 clause (the printer never double-quotes — a hand-fed
/// double-quoted `COMMENT "… PASSSCRAM '[REDACTED]' …"` would splice
/// inside the literal); any other PASSSCRAM spelling
/// stays in the text and fails validation loud.
fn strip_passscram(ddl: &str) -> Cow<'_, str> {
    let clause = format!(" PASSSCRAM {}", SECRET_SENTINEL_CANON);
    match ddl.find(&clause) {
        Some(at) => Cow::Owned(format!("{}{}", &ddl[..at], &ddl[at + clause.len()..])),
        None => Cow::Borrowed(ddl),
    }
}

fn split_user(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "USER")?;
    let name = header_name(&mut split)?;
    let mut on: Option<Authored<Level>> = None;
    let mut pass: Option<Pass> = None;
    let mut roles: Option<Vec<Role>> = None;
    let mut token_duration: Option<Expr> = None;
    let mut session_duration: Option<Expr> = None;
    let mut comment = None;
    let mut markers = Markers::default();
    let mut saw_pass = false;
    split.clause_loop("user", &mut |split, key| {
        match key {
            "on" => {
                if on.is_some() {
                    return Err(duplicated("ON"));
                }
                let word = split
                    .word()
                    .ok_or_else(|| tripwire("expected the level after `ON`".to_string()))?;
                let level = match word.to_ascii_lowercase().as_str() {
                    "root" => Level::Root,
                    "namespace" => Level::Namespace,
                    "database" => Level::Database,
                    other => {
                        return Err(tripwire(format!(
                            "`{other}` is not a user level (the canonical words are ROOT, \
                             NAMESPACE, DATABASE)"
                        )));
                    }
                };
                on = Some(Authored::canonical(level));
            }
            "password" | "passhash" => {
                if saw_pass {
                    return Err(duplicated("PASSWORD|PASSHASH"));
                }
                saw_pass = true;
                let value = split.capture_string("PASSWORD|PASSHASH")?;
                // The sentinels are the server's redaction markers,
                // never literals: the SDL value / absence is
                // preserve-the-normalizer's territory, and the differ
                // treats secrets as opaque.
                if value == SECRET_SENTINEL_CANON_DECODED {
                    markers.redacted = true;
                } else if value.is_empty() {
                    markers.secretless_passhash = true;
                } else if key == "password" {
                    pass = Some(Pass::Password(value));
                } else {
                    pass = Some(Pass::Hash(value));
                }
            }
            "roles" => {
                if roles.is_some() {
                    return Err(duplicated("ROLES"));
                }
                let text = split.capture_expr(
                    &[
                        CaptureStopWord("DURATION", CaptureFollow::Any),
                        CaptureStopWord("COMMENT", CaptureFollow::Value),
                    ],
                    "ROLES",
                )?;
                roles = Some(comma_items(&text, "ROLES", role_atom)?);
            }
            "duration" => {
                if token_duration.is_some() || session_duration.is_some() {
                    return Err(duplicated("DURATION"));
                }
                let text = split.capture_expr(
                    &[CaptureStopWord("COMMENT", CaptureFollow::Value)],
                    "DURATION",
                )?;
                let (token, session) = duration_group(&text)?;
                token_duration = token;
                session_duration = session;
            }
            "comment" => {
                if comment.is_some() {
                    return Err(duplicated("COMMENT"));
                }
                match split.capture_comment()? {
                    CommentValue::Doc(doc) => comment = Some(doc),
                    CommentValue::NotPlainString => {
                        split.route_raw(RawRoute::CommentNotPlainString);
                    }
                }
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the user definition"
                )));
            }
        }
        Ok(())
    })?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.require_end()?;
    if on.is_none() {
        return Err(tripwire(
            "ON always renders in INFO — it is missing".to_string(),
        ));
    }
    let Some(roles) = roles else {
        return Err(tripwire(
            "ROLES always materializes in INFO (VIEWER is the default) — it is missing".to_string(),
        ));
    };
    let (token_duration, session_duration) = match (token_duration, session_duration) {
        (None, None) => {
            return Err(tripwire(
                "DURATION always materializes in INFO with both sides — it is missing".to_string(),
            ));
        }
        present => present,
    };
    Ok(SplitOutcome::Stmt {
        markers,
        stmt: Box::new(Stmt::User(User {
            name: Ident::new(name.clone()),
            on: on.expect("checked above"),
            pass,
            // PASSSCRAM never reaches here: the sentinel spelling is
            // stripped before validation, and any other spelling
            // fails loud downstream — an invalid verifier at the
            // canonicalizing parse (the engine's own check), a valid
            // one at this splitter, which knows no PASSSCRAM clause
            // (the catalog never prints a real verifier either way).
            scram: None,
            roles,
            token_duration,
            session_duration,
            comment,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

/// One role atom (canonical INFO spells it UPPERCASE).
fn role_atom(text: &str) -> Option<Role> {
    match text.trim().to_ascii_lowercase().as_str() {
        "owner" => Some(Role::Owner),
        "editor" => Some(Role::Editor),
        "viewer" => Some(Role::Viewer),
        _ => None,
    }
}

/// `FOR TOKEN <duration>, FOR SESSION <duration|NONE>` — both sides
/// always materialize, TOKEN first. The session sentinel reads as the
/// faithful `NONE` expression (SDL spells `session: none;` and the
/// emitter prints it verbatim); a present duration parses by value.
fn duration_group(text: &str) -> Result<(Option<Expr>, Option<Expr>), Diag> {
    let rest = strip_word(text, "FOR")
        .and_then(|rest| strip_word(rest, "TOKEN"))
        .ok_or_else(|| tripwire("expected `FOR TOKEN` opening the DURATION value".to_string()))?;
    let (token_text, rest) = match rest.split_once(',') {
        Some((token, rest)) => (token, rest),
        None => {
            return Err(tripwire(
                "the DURATION value always carries both sides — `, FOR SESSION` is missing"
                    .to_string(),
            ));
        }
    };
    let token_text = token_text.trim();
    if crate::parser::parse_duration_text(token_text).is_none() {
        return Err(tripwire(format!(
            "`{token_text}` is not the TOKEN duration the DURATION clause materializes"
        )));
    }
    let rest = strip_word(rest, "FOR")
        .and_then(|rest| strip_word(rest, "SESSION"))
        .ok_or_else(|| tripwire("expected `FOR SESSION` in the DURATION value".to_string()))?;
    let session_text = rest.trim();
    let session_is_none = session_text.eq_ignore_ascii_case("NONE");
    if !session_is_none && crate::parser::parse_duration_text(session_text).is_none() {
        return Err(tripwire(format!(
            "`{session_text}` is not the SESSION duration the DURATION clause materializes"
        )));
    }
    Ok((
        Some(Expr {
            canon: token_text.to_string(),
        }),
        Some(Expr {
            canon: session_text.to_string(),
        }),
    ))
}

/// A word prefix, ASCII case-insensitively (the DURATION group's
/// `FOR`/`TOKEN`/`SESSION` shape).
fn strip_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let trimmed = text.trim_start();
    trimmed
        .len()
        .checked_sub(word.len())
        .filter(|_| trimmed[..word.len()].eq_ignore_ascii_case(word))
        .map(|_| &trimmed[word.len()..])
}

/// The `events` section of `INFO FOR TABLE` (spec/event.md):
/// `WHEN true` and `ASYNC RETRY n MAXDEPTH n` always materialize (the
/// defaults ride the read result); the canonical order is ASYNC →
/// WHEN → THEN → COMMENT, and the THEN items are the printer's own
/// statement texts (parens or braces included) split at depth-0
/// commas.
pub fn read_events(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "events", |name, ddl| {
        read_definition(name, ddl, split_event)
    })
}

fn split_event(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "EVENT")?;
    let name = header_name(&mut split)?;
    let mut on: Option<String> = None;
    let mut when: Option<Expr> = None;
    let mut then: Option<Vec<Expr>> = None;
    let mut r#async: Option<Async> = None;
    let mut comment = None;
    split.clause_loop("event", &mut |split, key| {
        match key {
            "on" => {
                if on.is_some() {
                    return Err(duplicated("ON"));
                }
                on = Some(split.capture_ident("ON")?);
            }
            "async" => {
                if r#async.is_some() {
                    return Err(duplicated("ASYNC"));
                }
                r#async = Some(Async {
                    retry: None,
                    maxdepth: None,
                });
            }
            "retry" => {
                let Some(event_async) = &mut r#async else {
                    return Err(tripwire(
                        "`RETRY` appears without `ASYNC` (SurrealQL prints them together)"
                            .to_string(),
                    ));
                };
                if event_async.retry.is_some() {
                    return Err(duplicated("RETRY"));
                }
                let text = split.capture_expr(
                    &[
                        CaptureStopWord("MAXDEPTH", CaptureFollow::Any),
                        CaptureStopWord("WHEN", CaptureFollow::Any),
                        CaptureStopWord("THEN", CaptureFollow::ThenItem),
                        CaptureStopWord("COMMENT", CaptureFollow::Value),
                    ],
                    "RETRY",
                )?;
                event_async.retry = Some(u16_atom(&text, "RETRY")?);
            }
            "maxdepth" => {
                let Some(event_async) = &mut r#async else {
                    return Err(tripwire(
                        "`MAXDEPTH` appears without `ASYNC` (SurrealQL prints them together)"
                            .to_string(),
                    ));
                };
                if event_async.maxdepth.is_some() {
                    return Err(duplicated("MAXDEPTH"));
                }
                let text = split.capture_expr(
                    &[
                        CaptureStopWord("WHEN", CaptureFollow::Any),
                        CaptureStopWord("THEN", CaptureFollow::ThenItem),
                        CaptureStopWord("COMMENT", CaptureFollow::Value),
                    ],
                    "MAXDEPTH",
                )?;
                event_async.maxdepth = Some(u16_atom(&text, "MAXDEPTH")?);
            }
            "when" => {
                if when.is_some() {
                    return Err(duplicated("WHEN"));
                }
                // The capture-grammar split: a WHEN expression may
                // carry the word `then` (or a `comment` record id) as
                // content — only the then-item / string shapes confirm
                // the clause.
                let text = split.capture_expr(
                    &[
                        CaptureStopWord("THEN", CaptureFollow::ThenItem),
                        CaptureStopWord("COMMENT", CaptureFollow::Value),
                    ],
                    "WHEN",
                )?;
                when = Some(Expr { canon: text });
            }
            "then" => {
                if then.is_some() {
                    return Err(duplicated("THEN"));
                }
                // A then-item statement can carry the word `comment`
                // as content (`SET x = comment`) — only the string
                // shape confirms COMMENT; ASYNC (canonical text never
                // places it after THEN) still ends the list.
                let text = split.capture_expr(
                    &[
                        CaptureStopWord("ASYNC", CaptureFollow::Any),
                        CaptureStopWord("COMMENT", CaptureFollow::Value),
                    ],
                    "THEN",
                )?;
                then = Some(comma_items(&text, "THEN", |item| {
                    (!item.is_empty()).then(|| Expr {
                        canon: item.to_string(),
                    })
                })?);
            }
            "comment" => {
                if comment.is_some() {
                    return Err(duplicated("COMMENT"));
                }
                match split.capture_comment()? {
                    CommentValue::Doc(doc) => comment = Some(doc),
                    CommentValue::NotPlainString => {
                        split.route_raw(RawRoute::CommentNotPlainString);
                    }
                }
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the event definition"
                )));
            }
        }
        Ok(())
    })?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.require_end()?;
    let Some(on) = on else {
        return Err(tripwire(
            "ON always renders in INFO — it is missing".to_string(),
        ));
    };
    let Some(then) = then else {
        return Err(tripwire(
            "THEN always renders in INFO — it is missing".to_string(),
        ));
    };
    if let Some(event_async) = &r#async
        && (event_async.retry.is_none() || event_async.maxdepth.is_none())
    {
        return Err(tripwire(
            "ASYNC RETRY and MAXDEPTH always materialize together — one is missing".to_string(),
        ));
    }
    if when.is_none() {
        return Err(tripwire(
            "WHEN always materializes in INFO — it is missing".to_string(),
        ));
    }
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Event(Event {
            name: Ident::new(name.clone()),
            on: Ident::new(on),
            when,
            then,
            r#async,
            comment,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

/// A canonical INFO u16 (`RETRY`/`MAXDEPTH` materialize as plain
/// integer literals — define-time evaluation leaves no expression
/// behind).
fn u16_atom(text: &str, clause: &str) -> Result<u16, Diag> {
    uint_atom(text, clause)?;
    text.parse::<u16>().map_err(|_| {
        tripwire(format!(
            "`{text}` exceeds the u16 the `{clause}` clause carries"
        ))
    })
}

/// The `functions` section of `INFO FOR DB` (spec/function.md): the
/// one two-block statement's fixed-order SurrealQL head (path →
/// parameters → optional `-> kind` → body) reads positionally, the
/// trailing clauses through the shared loop — server order is body →
/// COMMENT → GRAPHQL_* → PERMISSIONS, and PERMISSIONS always renders
/// (FULL is the default). Param names arrive without `$` in upstream's
/// AST — the printer re-adds it (the display shape the reader sees).
pub fn read_functions(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "functions", |name, ddl| {
        read_definition(name, ddl, split_function)
    })
}

fn split_function(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "FUNCTION")?;
    let segments = prefixed_path(&mut split, "fn", "FUNCTION")?;
    // The parameter list: `$name: kind` items — one kind production
    // per parameter (a raw depth-0 comma split would cut
    // `array<float, 10>` apart).
    split.lexer.skip_surrealql_trivia();
    if split.lexer.peek_byte() != Some(b'(') {
        return Err(tripwire(
            "expected `(` opening the parameter list".to_string(),
        ));
    }
    let interior = split
        .lexer
        .capture_balanced_parens()
        .map_err(|diag| tripwire(describe_clause_capture(&diag)))?;
    let args = parse_param_list(interior.slice(canonical))?;
    // The optional `-> kind` return.
    split.lexer.skip_surrealql_trivia();
    let mut returns = None;
    if split.lexer.rest().starts_with("->") {
        split.lexer.advance_by(2);
        returns = Some(Kind {
            text: capture_kind(&mut split)?,
        });
    }
    // The body block: braces included in the canon (the whole `{ … }`
    // print, empty canonizing to `{;}`).
    split.lexer.skip_surrealql_trivia();
    if !split.lexer.eat_byte(b'{') {
        return Err(tripwire(
            "expected `{` opening the function body".to_string(),
        ));
    }
    let interior = split
        .lexer
        .capture_balanced_braces_open_eaten()
        .map_err(|diag| tripwire(describe_clause_capture(&diag)))?;
    let interior_text = interior.slice(canonical);
    let mut body = String::with_capacity(interior_text.len() + 2);
    body.push('{');
    body.push_str(interior_text);
    body.push('}');
    // The trailing clauses — the server's ToSql prints COMMENT →
    // GRAPHQL_* → PERMISSIONS; SDL's own trailing order is its
    // canonical choice and both parse (the differ compares ASTs).
    let mut alias = None;
    let mut deprecated = None;
    let mut permissions = None;
    let mut comment = None;
    let mut saw_permissions = false;
    split.clause_loop("function", &mut |split, key| {
        match key {
            "comment" => {
                if comment.is_some() {
                    return Err(duplicated("COMMENT"));
                }
                match split.capture_comment()? {
                    CommentValue::Doc(doc) => comment = Some(doc),
                    CommentValue::NotPlainString => {
                        split.route_raw(RawRoute::CommentNotPlainString);
                    }
                }
            }
            "graphql_alias" => {
                if alias.is_some() {
                    return Err(duplicated("GRAPHQL_ALIAS"));
                }
                alias = Some(split.capture_string("GRAPHQL_ALIAS")?);
            }
            "graphql_deprecated" => {
                if deprecated.is_some() {
                    return Err(duplicated("GRAPHQL_DEPRECATED"));
                }
                deprecated = Some(split.capture_string("GRAPHQL_DEPRECATED")?);
            }
            "permissions" => {
                if saw_permissions {
                    return Err(duplicated("PERMISSIONS"));
                }
                permissions = Some(split.capture_permission(&[])?);
                saw_permissions = true;
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the function definition"
                )));
            }
        }
        Ok(())
    })?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.require_end()?;
    if !saw_permissions {
        return Err(tripwire(
            "PERMISSIONS always renders in INFO — it is missing".to_string(),
        ));
    }
    let graphql =
        (alias.is_some() || deprecated.is_some()).then_some(Graphql { alias, deprecated });
    // The INFO key is the path without its `fn::` prefix
    // (`fn::a::b` files under `a::b` — probe-pinned).
    let name = segments.join("::");
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Function(Function {
            name: FnPath {
                kind: FnKind::Fn,
                segments,
            },
            args,
            returns,
            body: Expr { canon: body },
            permissions,
            graphql,
            comment,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

/// One `$name: kind` parameter (over the parameter list's interior
/// text — the balanced capture keeps `array<float, 10>` whole).
fn parse_param_list(text: &str) -> Result<Vec<(ParamName, Kind)>, Diag> {
    let mut scan = Scanner::new(text);
    let mut args = Vec::new();
    loop {
        scan.skip_surrealql_trivia();
        if scan.is_at_end() {
            break;
        }
        if !scan.eat_byte(b'$') {
            return Err(tripwire(
                "expected `$` opening a parameter name".to_string(),
            ));
        }
        let param = match scan.read_ident() {
            Ok(Some((_, text, _))) => text,
            _ => {
                return Err(tripwire(
                    "expected the parameter name after `$`".to_string(),
                ));
            }
        };
        scan.skip_surrealql_trivia();
        if !scan.eat_byte(b':') {
            return Err(tripwire(format!(
                "expected `:` after the parameter `${param}`"
            )));
        }
        scan.skip_surrealql_trivia();
        let rest = scan.rest();
        let Some(tail) = crate::kind::read_kind(rest) else {
            return Err(tripwire(format!(
                "expected a kind after the parameter `${param}`"
            )));
        };
        let kind_text = rest[..rest.len() - tail.len()].trim().to_string();
        scan.advance_by(rest.len() - tail.len());
        args.push((ParamName::new(param), Kind { text: kind_text }));
        scan.skip_surrealql_trivia();
        if !scan.eat_byte(b',') {
            break;
        }
    }
    scan.skip_surrealql_trivia();
    if !scan.is_at_end() {
        return Err(tripwire(
            "text remains after the parameter list".to_string(),
        ));
    }
    Ok(args)
}

/// The `configs` section of `INFO FOR DB` (spec/config.md): INFO
/// renders configs as HEADER-LESS fragments (`API MIDDLEWARE …`,
/// `GRAPHQL TABLES …`) — `DEFINE CONFIG OVERWRITE` is prepended
/// before the canonicalizing parse (README item 4). The map keys are
/// the variant spellings (`API`, `GraphQL`) — normalized to the
/// variant word for the header cross-check.
pub fn read_configs(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "configs", |name, fragment| {
        let full = format!("DEFINE CONFIG OVERWRITE {fragment}");
        read_definition(&name.to_ascii_lowercase(), &full, split_config)
    })
}

fn split_config(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "CONFIG")?;
    let variant = split
        .word()
        .ok_or_else(|| tripwire("expected the config variant".to_string()))?;
    match variant.to_ascii_lowercase().as_str() {
        "api" => finish_config_api(&mut split),
        "graphql" => finish_config_graphql(&mut split),
        "default" => finish_config_default(&mut split),
        other => Err(tripwire(format!(
            "`{other}` is not a config variant (the variants are API, GRAPHQL, DEFAULT)"
        ))),
    }
}

/// The API variant's fragments always carry PERMISSIONS (FULL is the
/// default); the middleware list reads back with its computed
/// argument values (define-time evaluation leaves no expression).
/// One middleware entry over the canonical text: `<head>(<args>)` —
/// the head is a name path (bare, `fn::`, or `mod::`; a quoted head
/// is an ordinary bare name, the forward grammar's rule), the parens
/// are required, and the interior splits at depth-0 `,` into the
/// computed argument values (already the printer's own spellings).
fn middleware_entry(text: &str) -> Option<Middleware> {
    let mut split = Splitter::new(text);
    split.lexer.skip_surrealql_trivia();
    let (_, head, quoted) = split.lexer.read_ident().ok()??;
    let kind = if quoted {
        FnKind::Bare
    } else {
        match head.to_ascii_lowercase().as_str() {
            "fn" => FnKind::Fn,
            "mod" => FnKind::Mod,
            _ => FnKind::Bare,
        }
    };
    let rest = crate::parser::read_path_segments(&mut split.lexer)?;
    if matches!(kind, FnKind::Fn | FnKind::Mod) && rest.is_empty() {
        return None;
    }
    let mut segments = Vec::new();
    if kind == FnKind::Bare {
        segments.push(head);
    }
    segments.extend(rest);
    split.lexer.skip_surrealql_trivia();
    if split.lexer.peek_byte() != Some(b'(') {
        return None;
    }
    let interior = split.lexer.capture_balanced_parens().ok()?;
    let interior_text = interior.slice(text);
    let args = if interior_text.trim().is_empty() {
        Vec::new()
    } else {
        comma_items(interior_text, "MIDDLEWARE", |item| {
            (!item.is_empty()).then(|| Expr {
                canon: item.to_string(),
            })
        })
        .ok()?
    };
    split.lexer.skip_surrealql_trivia();
    split.lexer.is_at_end().then_some(Middleware {
        path: FnPath { kind, segments },
        args,
    })
}

fn finish_config_api(split: &mut Splitter) -> Result<SplitOutcome, Diag> {
    let mut middleware: Option<Vec<Middleware>> = None;
    let mut permissions = None;
    let mut saw_permissions = false;
    split.clause_loop("config api", &mut |split, key| {
        match key {
            "middleware" => {
                if middleware.is_some() {
                    return Err(duplicated("MIDDLEWARE"));
                }
                let text = split.capture_expr(
                    &[CaptureStopWord("PERMISSIONS", CaptureFollow::Perm)],
                    "MIDDLEWARE",
                )?;
                middleware = Some(comma_items(&text, "MIDDLEWARE", middleware_entry)?);
            }
            "permissions" => {
                if saw_permissions {
                    return Err(duplicated("PERMISSIONS"));
                }
                permissions = Some(split.capture_permission(&[])?);
                saw_permissions = true;
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the config api definition"
                )));
            }
        }
        Ok(())
    })?;
    split.require_end()?;
    if !saw_permissions {
        return Err(tripwire(
            "PERMISSIONS always renders in the api fragment — it is missing".to_string(),
        ));
    }
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Config(Config::Api(ApiConfig {
            middleware: middleware.unwrap_or_default(),
            permissions,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }))),
        name: "api".to_string(),
    })
}

/// Both families always materialize (NONE when unset — the SDL
/// `tables: none;` spelling is representation-equal to absence, the
/// AST cannot tell them apart); DEPTH/COMPLEXITY only when set;
/// INTROSPECTION only as the NONE sentinel (absent = auto).
fn finish_config_graphql(split: &mut Splitter) -> Result<SplitOutcome, Diag> {
    const FAMILY_STOPS: &[CaptureStopWord] = &[
        CaptureStopWord(
            "FUNCTIONS",
            CaptureFollow::Word(&["none", "auto", "include", "exclude"]),
        ),
        CaptureStopWord("DEPTH", CaptureFollow::Digit),
        CaptureStopWord("COMPLEXITY", CaptureFollow::Digit),
        CaptureStopWord("INTROSPECTION", CaptureFollow::Word(&["none", "auto"])),
    ];
    let mut tables = Family::None;
    let mut functions = Family::None;
    let mut depth = None;
    let mut complexity = None;
    let mut introspection = Introspection::Auto;
    let mut saw_tables = false;
    let mut saw_functions = false;
    split.clause_loop("config graphql", &mut |split, key| {
        match key {
            "tables" => {
                if saw_tables {
                    return Err(duplicated("TABLES"));
                }
                tables = capture_family(split, FAMILY_STOPS, ident_item)?;
                saw_tables = true;
            }
            "functions" => {
                if saw_functions {
                    return Err(duplicated("FUNCTIONS"));
                }
                functions = capture_family(split, FAMILY_STOPS, fn_path_item)?;
                saw_functions = true;
            }
            "depth" => {
                if depth.is_some() {
                    return Err(duplicated("DEPTH"));
                }
                let text = split.capture_expr(
                    &[
                        CaptureStopWord("COMPLEXITY", CaptureFollow::Digit),
                        CaptureStopWord("INTROSPECTION", CaptureFollow::Word(&["none", "auto"])),
                    ],
                    "DEPTH",
                )?;
                depth = Some(u32_atom(&text, "DEPTH")?);
            }
            "complexity" => {
                if complexity.is_some() {
                    return Err(duplicated("COMPLEXITY"));
                }
                let text = split.capture_expr(
                    &[CaptureStopWord(
                        "INTROSPECTION",
                        CaptureFollow::Word(&["none", "auto"]),
                    )],
                    "COMPLEXITY",
                )?;
                complexity = Some(u32_atom(&text, "COMPLEXITY")?);
            }
            "introspection" => {
                let word = split
                    .word()
                    .ok_or_else(|| tripwire("expected the introspection word".to_string()))?;
                introspection = match word.to_ascii_lowercase().as_str() {
                    "none" => Introspection::None,
                    // The absent clause's value — spelled only by
                    // hand-fed input (INFO elides it).
                    "auto" => Introspection::Auto,
                    other => {
                        return Err(tripwire(format!(
                            "`{other}` is not an INTROSPECTION word (NONE or AUTO)"
                        )));
                    }
                };
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the config graphql definition"
                )));
            }
        }
        Ok(())
    })?;
    split.require_end()?;
    if !saw_tables || !saw_functions {
        return Err(tripwire(
            "both families always materialize in the graphql fragment — one is missing".to_string(),
        ));
    }
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Config(Config::Graphql(GraphqlConfig {
            tables,
            functions,
            depth,
            complexity,
            introspection,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }))),
        name: "graphql".to_string(),
    })
}

/// One exposure family after its clause keyword: `NONE`, `AUTO`, or
/// INCLUDE/EXCLUDE with at least one entry (the list items are the
/// printer's own texts — backtick-escaped table idents, fn-paths).
fn capture_family<T>(
    split: &mut Splitter,
    stops: &[CaptureStopWord],
    item: impl Fn(&str) -> Result<T, Diag>,
) -> Result<Family<T>, Diag> {
    let word = split
        .word()
        .ok_or_else(|| tripwire("expected the family word".to_string()))?;
    match word.to_ascii_lowercase().as_str() {
        "none" => Ok(Family::None),
        "auto" => Ok(Family::Auto),
        "include" => {
            let text = split.capture_expr(stops, "INCLUDE")?;
            Ok(Family::Include(comma_items_result(&text, "INCLUDE", item)?))
        }
        "exclude" => {
            let text = split.capture_expr(stops, "EXCLUDE")?;
            Ok(Family::Exclude(comma_items_result(&text, "EXCLUDE", item)?))
        }
        other => Err(tripwire(format!(
            "`{other}` is not a family word (NONE, AUTO, INCLUDE, EXCLUDE)"
        ))),
    }
}

/// The list form of [`comma_items`] over a `Result`-producing parser
/// (the family items carry their own tripwires); the final item rides
/// to the end of its capture without a trailing comma.
fn comma_items_result<T>(
    text: &str,
    clause: &str,
    parse: impl Fn(&str) -> Result<T, Diag>,
) -> Result<Vec<T>, Diag> {
    let mut scan = Scanner::new(text);
    let mut items = Vec::new();
    loop {
        let start = scan.offset();
        let item = match scan.capture_until(b",") {
            Ok((span, _)) => span.slice(text).trim().to_string(),
            Err(Diag::UnterminatedCapture { .. }) => text[start..].trim().to_string(),
            Err(_) => {
                return Err(tripwire(format!(
                    "the `{clause}` clause value is malformed"
                )));
            }
        };
        items.push(parse(&item)?);
        if scan.is_at_end() {
            return Ok(items);
        }
    }
}

/// One table ident (a bare word or quoted identifier, unescaped).
fn ident_item(text: &str) -> Result<Ident, Diag> {
    let mut scan = Scanner::new(text);
    scan.skip_surrealql_trivia();
    match scan.read_ident() {
        Ok(Some((_, name, _))) if scan.rest().trim().is_empty() => Ok(Ident::new(name)),
        _ => Err(tripwire(format!(
            "`{}` is not a table identifier",
            text.trim()
        ))),
    }
}

/// One `fn::`-prefixed path (the graphql function families demand the
/// prefix, like the forward grammar).
fn fn_path_item(text: &str) -> Result<FnPath, Diag> {
    let mut split = Splitter::new(text);
    let segments = prefixed_path(&mut split, "fn", "FUNCTIONS")?;
    split.lexer.skip_surrealql_trivia();
    if !split.lexer.is_at_end() {
        return Err(tripwire(format!("text remains after the `{text}` path")));
    }
    Ok(FnPath {
        kind: FnKind::Fn,
        segments,
    })
}

/// A canonical INFO u32 (`DEPTH`/`COMPLEXITY`).
fn u32_atom(text: &str, clause: &str) -> Result<u32, Diag> {
    uint_atom(text, clause)?;
    text.parse::<u32>().map_err(|_| {
        tripwire(format!(
            "`{text}` exceeds the u32 the `{clause}` clause carries"
        ))
    })
}

/// The DEFAULT variant's fragment grammar: `DEFAULT NAMESPACE <expr>
/// [DATABASE <expr>]` — the set sides only. Unreachable through INFO
/// in 3.2.4 (the `default` config surfaces only as INFO FOR ROOT's
/// structured `defaults` value — see [`read_default_config`]);
/// accepted here per the spec's reverse grammar, unit-tested only. A
/// bare-identifier side (the server's fragment spelling) quotes to
/// the same canon the structured value and SDL's authored string
/// carry — one semantic value, one `same_definition` outcome.
fn finish_config_default(split: &mut Splitter) -> Result<SplitOutcome, Diag> {
    let mut namespace = None;
    let mut database = None;
    split.clause_loop("config default", &mut |split, key| {
        match key {
            "namespace" => {
                if namespace.is_some() {
                    return Err(duplicated("NAMESPACE"));
                }
                // The fragment's values are the bare identifier (the
                // server's catalog shape) or the quoted string (SDL's
                // authored spelling) — never an expression that could
                // legitimately carry the word `database`.
                namespace = default_config_side(split.capture_expr(
                    &[CaptureStopWord("DATABASE", CaptureFollow::Any)],
                    "NAMESPACE",
                )?)?
                .map(|canon| Expr { canon });
            }
            "database" => {
                if database.is_some() {
                    return Err(duplicated("DATABASE"));
                }
                database = default_config_side(split.capture_expr(&[], "DATABASE")?)?
                    .map(|canon| Expr { canon });
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the config default definition"
                )));
            }
        }
        Ok(())
    })?;
    split.require_end()?;
    if namespace.is_none() && database.is_none() {
        return Err(tripwire(
            "at least one of NAMESPACE, DATABASE is required in the default config".to_string(),
        ));
    }
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Config(Config::Default(DefaultConfig {
            namespace,
            database,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        }))),
        name: "default".to_string(),
    })
}

/// One `config default` side's canon: a bare identifier (the
/// fragment spelling — the server stores the NAME as the string)
/// quotes so it holds the same canon as SDL's authored string and
/// [`read_default_config`]'s structured value — through
/// [`crate::keywords::quoted_string`], the one quoting home (a
/// quote-needing name carries the switched-quote canon, escape
/// chars included); a backticked ident is source escaping — its
/// interior decodes first; an already-quoted string rides verbatim.
/// The `NONE` sentinel is the emitter's spelling of the UNSET side —
/// read as absence, not as a string.
fn default_config_side(text: String) -> Result<Option<String>, Diag> {
    if text.eq_ignore_ascii_case("NONE") {
        return Ok(None);
    }
    // An already-quoted string rides verbatim — checked on the RAW
    // fragment text (a backticked ident whose NAME begins with a
    // quote character is still the backtick form).
    if text.starts_with('\'') || text.starts_with('"') {
        return Ok(Some(text));
    }
    let name = if text.starts_with('`') && text.ends_with('`') {
        crate::lexer::unescape_escapes(&text[1..text.len() - 1])
            .map_err(|_| tripwire("malformed escape in the default config's name".to_string()))?
    } else {
        text
    };
    let mut canon = String::new();
    crate::keywords::quoted_string(&mut canon, &name);
    Ok(Some(canon))
}

/// The `defaults` section of `INFO FOR ROOT` (spec/config.md): the
/// `default` config's STRUCTURED value — `{}` when unset, else
/// `{ namespace?: string, database?: string }` with the set sides
/// only. The fragment DDL form is unreachable through INFO in 3.2.4.
/// `None` when the section is absent or empty (no default config is
/// defined).
pub fn read_default_config(info: &InfoObject) -> Result<Option<ReadEntry>, Diag> {
    let Some(InfoValue::Object(entries)) = info.0.get("defaults") else {
        return if info.0.contains_key("defaults") {
            Err(Diag::InfoUnknownShape {
                span: Span::point(0),
                message: "the `defaults` section is not an object".to_string(),
            })
        } else {
            Ok(None)
        };
    };
    if entries.0.is_empty() {
        return Ok(None);
    }
    let mut namespace = None;
    let mut database = None;
    for (key, value) in &entries.0 {
        let InfoValue::Ddl(text) = value else {
            return Err(Diag::InfoUnknownShape {
                span: Span::point(0),
                message: format!("the `defaults` entry `{key}` is not a string"),
            });
        };
        let mut canon = String::new();
        crate::keywords::quoted_string(&mut canon, text);
        match key.as_str() {
            "namespace" => namespace = Some(Expr { canon }),
            "database" => database = Some(Expr { canon }),
            other => {
                return Err(Diag::InfoUnknownShape {
                    span: Span::point(0),
                    message: format!("unknown `defaults` key `{other}`"),
                });
            }
        }
    }
    Ok(Some(ReadEntry {
        name: "default".to_string(),
        stmt: Stmt::Config(Config::Default(DefaultConfig {
            namespace,
            database,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        markers: Markers::default(),
    }))
}

/// The `modules` section of `INFO FOR DB` (spec/module.md) — SDL-side
/// ONLY: the embedded 3.2.4 parser cannot read module forms (the
/// vendored build compiles without the surrealism feature; even the
/// 3.2 spelling is rejected at parse), so no canonicalizing parse
/// fronts this reader. Both dialects read over the raw text per the
/// spec's reverse section — the 3.3 emission grammar (`mod::name FROM
/// … UNSIGNED`, `::<1.2.0>`) and the 3.2 AS form INFO carries
/// (`mod::name AS …`, `<1.2.0>`, absent UNSIGNED = implicitly
/// unsigned, PERMISSIONS always appended) — golden-only against the
/// spec grammar.
/// `mod::x AS silo::…` — the name+silo round-trip asymmetry — routes
/// to verbatim `raw`.
pub fn read_modules(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "modules", read_module_entry)
}

fn read_module_entry(name: &str, ddl: &str) -> Result<ReadEntry, Diag> {
    // The module reader is un-gated (no canonicalizing parse fronts
    // it), so it checks the one invariant both pinned dialects
    // guarantee themselves: the statement's brackets balance. An
    // unclosed bracket would otherwise swallow the statement's `;`
    // inside a value capture and read as a definition.
    if !brackets_balance(ddl) {
        return Err(tripwire(
            "unbalanced brackets in the module definition".to_string(),
        ));
    }
    let outcome = split_module(ddl)?;
    split_outcome(name, ddl, outcome)
}

/// Whether the brackets of `text` balance under the rule-3 skip set
/// (strings, comments, and quoted idents are skipped wholesale). The
/// walk runs to end of input — module DDL carries no trailing `;`
/// (INFO strings never do), so EOF is the normal end: balanced
/// exactly when nothing stays open.
fn brackets_balance(text: &str) -> bool {
    let mut scan = Scanner::new(text);
    let mut stack: Vec<u8> = Vec::new();
    loop {
        match scan.capture_until(b"()[]{}") {
            Ok((_, term)) => match term {
                b'(' | b'[' | b'{' => stack.push(term),
                _ => {
                    let Some(open) = stack.pop() else {
                        return false;
                    };
                    let matches_open =
                        matches!((open, term), (b'(', b')') | (b'[', b']') | (b'{', b'}'));
                    if !matches_open {
                        return false;
                    }
                }
            },
            // End of input: balanced exactly when nothing stays open.
            Err(Diag::UnterminatedCapture { .. }) => return stack.is_empty(),
            // A string/comment/quoted-ident capture error inside an
            // already string-skipping walk — malformed beyond the
            // balance question.
            Err(_) => return false,
        }
    }
}

fn split_module(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "MODULE")?;
    // The name–executable head: `[mod::<name>] (FROM | AS)`. The
    // head word `mod` is the path head (executables start with an
    // f-string or `silo::` — no other shape opens with a bare word).
    split.lexer.skip_surrealql_trivia();
    let mut mod_name: Option<String> = None;
    // The dialect word: `Some(true)` = FROM (3.3), `Some(false)` = AS
    // (3.2), `None` = the anonymous silo shape (no name head, no
    // dialect word).
    let mut from_form: Option<bool> = None;
    if let Some(span) = split.lexer.peek_word()
        && span.slice(split.src).eq_ignore_ascii_case("mod")
    {
        if !matches!(split.src.as_bytes().get(span.offset + span.len), Some(b':')) {
            return Err(tripwire(
                "expected `::` after the `mod` path head".to_string(),
            ));
        }
        split.lexer.advance_by(span.len + 2);
        mod_name = Some(split.capture_ident("MODULE")?);
        let dialect = split
            .word()
            .ok_or_else(|| tripwire("expected `FROM` or `AS` after the module name".to_string()))?;
        if !dialect.eq_ignore_ascii_case("FROM") && !dialect.eq_ignore_ascii_case("AS") {
            return Err(tripwire(format!(
                "expected `FROM` or `AS` after the module name, found `{dialect}`"
            )));
        }
        from_form = Some(dialect.eq_ignore_ascii_case("FROM"));
    }
    // The executable — exactly one.
    split.lexer.skip_surrealql_trivia();
    let executable = match split.lexer.peek_byte() {
        Some(b'f') => {
            if !matches!(split.lexer.rest().as_bytes().get(1), Some(b'\'' | b'"')) {
                return Err(tripwire(
                    "expected an f-string file literal (f'…' / f\"…\")".to_string(),
                ));
            }
            split.lexer.advance_by(1);
            let span = split
                .lexer
                .capture_string_literal()
                .map_err(|diag| tripwire(describe_clause_capture(&diag)))?;
            let value = string_value(split.src, span).map_err(|diag| tripwire(diag.to_string()))?;
            let Some((bucket, key)) = value.split_once(':') else {
                return Err(tripwire(format!(
                    "the file string `{value}` carries no `:` between its bucket and key"
                )));
            };
            // The forward file-string grammar (spec/module.md): the
            // key always begins `/` (upstream's lexer guarantees it),
            // the split is once, and both sides carry the forward
            // lexer's charsets (`[A-Za-z0-9_.-]` bucket,
            // `[A-Za-z0-9_./-]` key) — the written SDL must re-parse.
            let charset_ok = |text: &str, slash: bool| {
                text.bytes().all(|b| {
                    b.is_ascii_alphanumeric()
                        || matches!(b, b'_' | b'.' | b'-')
                        || (slash && b == b'/')
                })
            };
            if bucket.is_empty()
                || !key.starts_with('/')
                || key.contains(':')
                || !charset_ok(bucket, false)
                || !charset_ok(key, true)
            {
                return Err(tripwire(format!(
                    "the file string `{value}` is not a `bucket:/key` path (the key begins \
                     `/`, one `:`, the forward lexer's charsets)"
                )));
            }
            Executable::File {
                bucket: bucket.to_string(),
                key: key.to_string(),
            }
        }
        _ if split.peek_word_is("silo") => {
            split.expect_word("silo", "the silo executable")?;
            let org = path_segment_after_colons(&mut split, "SILO")?;
            let pkg = path_segment_after_colons(&mut split, "SILO")?;
            // The version triple: 3.3 `::<maj.min.patch>`, 3.2
            // `<maj.min.patch>` (the catalog's lexer-broken shape —
            // the digits parse tolerantly, spec/module.md).
            split.lexer.skip_surrealql_trivia();
            if split.lexer.rest().starts_with("::") {
                split.lexer.advance_by(2);
            }
            if !split.lexer.eat_byte(b'<') {
                return Err(tripwire(
                    "expected `<` opening the version triple".to_string(),
                ));
            }
            let (span, _) = split
                .lexer
                .capture_until(b">")
                .map_err(|diag| tripwire(describe_clause_capture(&diag)))?;
            let mut parts = span.slice(split.src).trim().split('.');
            let bad = || {
                tripwire(
                    "expected `<maj.min.patch>` — three `.`-joined integers — in the \
                     silo version"
                        .to_string(),
                )
            };
            let major = parts
                .next()
                .and_then(|part| part.trim().parse::<u32>().ok())
                .ok_or_else(bad)?;
            let minor = parts
                .next()
                .and_then(|part| part.trim().parse::<u32>().ok())
                .ok_or_else(bad)?;
            let patch = parts
                .next()
                .and_then(|part| part.trim().parse::<u32>().ok())
                .ok_or_else(bad)?;
            if parts.next().is_some() {
                return Err(bad());
            }
            Executable::Silo {
                org,
                pkg,
                major,
                minor,
                patch,
            }
        }
        _ => {
            return Err(tripwire(
                "expected the module executable (`f\"…\"` or `silo::…`)".to_string(),
            ));
        }
    };
    // The name–executable coupling (spec/module.md): a silo
    // executable is self-named — the explicit-name+silo shape SDL's
    // grammar rejects routes to verbatim `raw`; an anonymous FILE
    // module parses upstream but is rejected at execution ("A module
    // without a name cannot be stored") — the catalog cannot carry
    // one, so it diagnoses.
    match (&mod_name, &executable) {
        (Some(_), Executable::Silo { .. }) => {
            return Ok(SplitOutcome::Raw(RawRoute::ModuleNamedSilo));
        }
        (None, Executable::File { .. }) => {
            return Err(tripwire(
                "an anonymous file module cannot be stored — the module name is required"
                    .to_string(),
            ));
        }
        _ => {}
    }
    // The trailing clauses: UNSIGNED (3.3 only), PERMISSIONS (always
    // appended in the 3.2 INFO form), COMMENT.
    let mut saw_unsigned = false;
    let mut permissions = None;
    let mut comment = None;
    split.clause_loop("module", &mut |split, key| {
        match key {
            "unsigned" => {
                if saw_unsigned {
                    return Err(duplicated("UNSIGNED"));
                }
                saw_unsigned = true;
            }
            "permissions" => {
                if permissions.is_some() {
                    return Err(duplicated("PERMISSIONS"));
                }
                permissions = Some(
                    split
                        .capture_permission(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])?,
                );
            }
            "comment" => {
                if comment.is_some() {
                    return Err(duplicated("COMMENT"));
                }
                match split.capture_comment()? {
                    CommentValue::Doc(doc) => comment = Some(doc),
                    CommentValue::NotPlainString => {
                        split.route_raw(RawRoute::CommentNotPlainString);
                    }
                }
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the module definition"
                )));
            }
        }
        Ok(())
    })?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.require_end()?;
    // A FROM-form module always carries UNSIGNED (SDL's own emission
    // does; the docs' grammar has it); the 3.2 AS form never does —
    // implicitly unsigned (spec/module.md's reverse section), so an
    // UNSIGNED there is outside both dialects. The anonymous silo
    // shape carries no dialect word (tolerant either way — it can
    // never be in a catalog). Both store `unsigned: true`.
    match (from_form, saw_unsigned) {
        (Some(true), false) => {
            return Err(tripwire(
                "a FROM-form module always carries UNSIGNED".to_string(),
            ));
        }
        (Some(false), true) => {
            return Err(tripwire(
                "the AS form is implicitly unsigned — UNSIGNED belongs to the FROM form"
                    .to_string(),
            ));
        }
        _ => {}
    }
    // The cross-check name: the explicit `mod::<name>` head, or the
    // self-named silo's `org::pkg` (the anonymous-silo identity). A
    // post-registration capture's INFO carries `mod::<name>` storage
    // keys; split_outcome pairs the prefix and records the bare name.
    let name = match (&mod_name, &executable) {
        (Some(name), _) => name.clone(),
        (None, Executable::Silo { org, pkg, .. }) => format!("{org}::{pkg}"),
        (None, Executable::File { .. }) => {
            return Err(tripwire(
                "an anonymous file module cannot be stored — the module name is required"
                    .to_string(),
            ));
        }
    };
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Module(Module {
            name: mod_name.map(Ident::new),
            executable,
            unsigned: true,
            permissions,
            comment,
            // Span-blind in comparisons (same_definition zeroes it);
            // the target-version diagnostics are emit-side only.
            from_span: Span::point(0),
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

// ---------------------------------------------------------------------
// The high-tier statement readers (index, field, table)
// ---------------------------------------------------------------------

/// Every bare word in `text` with its byte offset and bracket depth
/// (the peel scans of the FIELDS tail, the relation `IN` payload, and
/// the legacy-`VS` recognition): strings (plain and prefixed),
/// comments, and quoted identifiers skip wholesale; `(`/`[`/`{` open
/// a depth. Unlike [`Scanner::capture_until_word`] there is no stop
/// machinery — the callers match words themselves.
fn scanned_words(text: &str) -> Vec<(usize, String, usize)> {
    let mut scan = Scanner::new(text);
    let mut words = Vec::new();
    let mut depth = 0usize;
    loop {
        scan.skip_surrealql_trivia();
        let Some(b) = scan.peek_byte() else {
            break;
        };
        match b {
            b'(' | b'[' | b'{' => {
                depth += 1;
                scan.advance_by(1);
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                scan.advance_by(1);
            }
            b'\'' | b'"' => {
                // A string body: through the closing quote, `\`
                // escapes skipped.
                let quote = b;
                scan.advance_by(1);
                while let Some(next) = scan.peek_byte() {
                    scan.advance_by(1);
                    match next {
                        b'\\' => {
                            scan.advance_by(1);
                        }
                        closed if closed == quote => break,
                        _ => {}
                    }
                }
            }
            _ if scan.rest().starts_with('`') => {
                scan.advance_by(1);
                while let Some(inner) = scan.peek_byte() {
                    scan.advance_by(1);
                    if inner == b'`' {
                        break;
                    }
                }
            }
            _ if scan.rest().starts_with('⟨') => {
                scan.advance_by('⟨'.len_utf8());
                while !scan.is_at_end() {
                    if scan.rest().starts_with('⟩') {
                        scan.advance_by('⟩'.len_utf8());
                        break;
                    }
                    scan.advance_by(1);
                }
            }
            _ if b.is_ascii_alphabetic() || b == b'_' => {
                if let Some(span) = scan.read_word() {
                    words.push((span.offset, span.slice(text).to_string(), depth));
                }
            }
            _ => {
                scan.advance_by(1);
            }
        }
    }
    words
}

/// Whether `word` appears as a bare word anywhere outside strings and
/// comments (the legacy-`VS` recognition — consulted only when the
/// embedded parser already rejected the DDL). Exact case: the printer
/// spells the token `VS` uppercase, so an authored lowercase `vs`
/// ident never resembles it.
fn has_bare_word(text: &str, word: &str) -> bool {
    scanned_words(text)
        .iter()
        .any(|(_, found, _)| found == word)
}

/// A canonical INFO float: fractional values carry the printer's `f`
/// suffix (`LM 0.40242960438184466f`, `ALPHA 1.2f`), integral ones
/// print bare (`MINKOWSKI 3`).
fn float_atom(text: &str, clause: &str) -> Result<f64, Diag> {
    let text = text.trim();
    let digits = text.strip_suffix('f').unwrap_or(text);
    let value = digits.parse::<f64>().map_err(|_| {
        tripwire(format!(
            "`{text}` is not the float the `{clause}` clause materializes"
        ))
    })?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(tripwire(format!(
            "`{text}` is out of the float domain the `{clause}` clause materializes"
        )))
    }
}

/// The `Infinity` spellings the printer writes for a diverged float
/// parameter (`LM Infinity` — the m=1 derived default).
fn is_infinity_atom(text: &str) -> bool {
    let text = text.trim();
    let text = text.strip_prefix(['+', '-']).unwrap_or(text);
    text == "Infinity" || text == "Infinityf"
}

/// The same for an f32 atom (the `BM25` pair stores f32 — upstream's
/// `Scoring::Bm`).
fn f32_atom(text: &str, clause: &str) -> Result<f32, Diag> {
    let text = text.trim();
    let digits = text.strip_suffix('f').unwrap_or(text);
    let value = digits.parse::<f32>().map_err(|_| {
        tripwire(format!(
            "`{text}` is not the float the `{clause}` clause materializes"
        ))
    })?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(tripwire(format!(
            "`{text}` is out of the float domain the `{clause}` clause materializes"
        )))
    }
}

/// One `DIST`/`DISTANCE` value word (the printer's uppercase
/// spellings; `MINKOWSKI` carries a following order float and is read
/// by the caller).
fn distance_atom(word: &str) -> Option<Distance> {
    Some(match word {
        "EUCLIDEAN" => Distance::Euclidean,
        "COSINE" => Distance::Cosine,
        "COSINE_NORMALIZED" => Distance::CosineNormalized,
        "CHEBYSHEV" => Distance::Chebyshev,
        "HAMMING" => Distance::Hamming,
        "JACCARD" => Distance::Jaccard,
        "PEARSON" => Distance::Pearson,
        "INNER_PRODUCT" => Distance::InnerProduct,
        "MANHATTAN" => Distance::Manhattan,
        _ => return None,
    })
}

/// One vector element type word (the printer's uppercase spellings).
fn vector_type_atom(word: &str) -> Option<VectorType> {
    Some(match word {
        "F64" => VectorType::F64,
        "F32" => VectorType::F32,
        "F16" => VectorType::F16,
        "I64" => VectorType::I64,
        "I32" => VectorType::I32,
        "I16" => VectorType::I16,
        "I8" => VectorType::I8,
        "U8" => VectorType::U8,
        _ => return None,
    })
}

/// The exact-uppercase kind spellings (the printer's own): authored
/// field idents keep their case, so a field named `unique` never
/// matches — the FIELDS peel's discriminator.
fn exact_kind_word(word: &str) -> bool {
    matches!(word, "UNIQUE" | "COUNT" | "FULLTEXT" | "HNSW" | "DISKANN")
}

/// `BM25(k1, b)` — the printer glues the parens and the pair comma
/// (`BM25(1.2,0.75)`).
fn capture_bm25(split: &mut Splitter) -> Result<Bm25, Diag> {
    split.lexer.skip_surrealql_trivia();
    if split.lexer.peek_byte() != Some(b'(') {
        return Err(tripwire("expected the `BM25(k1, b)` pair".to_string()));
    }
    let interior = split
        .lexer
        .capture_balanced_parens()
        .map_err(|diag| tripwire(describe_clause_capture(&diag)))?;
    let text = interior.slice(split.src);
    let mut sides = text.splitn(2, ',');
    let (k1, b) = match (sides.next(), sides.next()) {
        (Some(k1), Some(b)) => (k1, b),
        _ => {
            return Err(tripwire(format!(
                "`{text}` is not the `BM25(k1, b)` pair the FULLTEXT kind materializes"
            )));
        }
    };
    Ok(Bm25(f32_atom(k1, "BM25")?, f32_atom(b, "BM25")?))
}

/// One index kind's parameter tail — the FIELDS peel's two-sided
/// validation: `Some` only when the text opens with a kind word and
/// every word after it is a parameter that kind takes, each at most
/// once, consuming the text (anything else is not the kind's tail,
/// and the peel tries the next candidate). Domain validation (the
/// DISKANN restriction set, `m ≤ 127`, positive `degree`/`l_build`)
/// is [`validate_index_kind`]'s — a structurally-valid tail with an
/// out-of-domain value is the statement's real kind, hand-fed,
/// fail-closed.
fn index_kind_tail(tail: &str, unrepresentable: &mut bool) -> Option<IndexKind> {
    let mut split = Splitter::new(tail);
    let word = split.word()?;
    let (is_count, is_ft, is_vector, is_diskann) = (
        word == "COUNT",
        word == "FULLTEXT",
        word == "HNSW" || word == "DISKANN",
        word == "DISKANN",
    );
    if !matches!(word, "UNIQUE" | "COUNT" | "FULLTEXT" | "HNSW" | "DISKANN") {
        return None;
    }
    let mut count_where = None;
    let mut analyzer = None;
    let mut bm25 = None;
    let mut highlights = false;
    let mut dimension = None;
    let mut distance = None;
    let mut vector_type = None;
    let mut m = None;
    let mut m0 = None;
    let mut lm = None;
    let mut efc = None;
    let mut extend_candidates = false;
    let mut keep_pruned_connections = false;
    let mut hashed_vector = false;
    let mut degree = None;
    let mut l_build = None;
    let mut alpha = None;
    loop {
        split.lexer.skip_surrealql_trivia();
        if split.lexer.is_at_end() {
            break;
        }
        let word = split.word()?;
        let lower = word.to_ascii_lowercase();
        match lower.as_str() {
            "where" if is_count && count_where.is_none() => {
                let text = split.capture_expr(&[], "WHERE").ok()?;
                count_where = Some(Expr { canon: text });
            }
            "analyzer" if is_ft && analyzer.is_none() => {
                analyzer = Some(Ident::new(split.capture_ident("ANALYZER").ok()?));
            }
            "bm25" if is_ft && bm25.is_none() => {
                bm25 = Some(capture_bm25(&mut split).ok()?);
            }
            "highlights" if is_ft && !highlights => {
                highlights = true;
            }
            "dimension" if is_vector && dimension.is_none() => {
                let value = split.atom()?;
                dimension = Some(u16_atom(value, "DIMENSION").ok()?);
            }
            "distance" | "dist" if is_vector && distance.is_none() => {
                let value = split.atom()?;
                if value.eq_ignore_ascii_case("MINKOWSKI") {
                    let order = split.atom()?;
                    distance = Some(Distance::Minkowski(Float(
                        float_atom(order, "MINKOWSKI").ok()?,
                    )));
                } else {
                    distance = Some(distance_atom(value)?);
                }
            }
            "type" if is_vector && vector_type.is_none() => {
                let value = split.atom()?;
                vector_type = Some(vector_type_atom(value)?);
            }
            "m" if is_vector && m.is_none() => {
                let value = split.atom()?;
                m = Some(u16_atom(value, "M").ok()?);
            }
            "m0" if is_vector && m0.is_none() => {
                let value = split.atom()?;
                m0 = Some(u16_atom(value, "M0").ok()?);
            }
            "lm" if is_vector && lm.is_none() => {
                let value = split.atom()?;
                match float_atom(value, "LM") {
                    Ok(v) => lm = Some(Float(v)),
                    Err(_) => {
                        // `LM Infinity` — the engine's own derived
                        // default when m = 1 (1/ln(1) diverges;
                        // live-probed). Engine-legal, outside SDL's
                        // unsigned float scalar grammar — the
                        // IndexSignedFloat route's class.
                        if is_infinity_atom(value) {
                            *unrepresentable = true;
                        }
                        return None;
                    }
                }
            }
            "efc" if is_vector && efc.is_none() => {
                let value = split.atom()?;
                efc = Some(u16_atom(value, "EFC").ok()?);
            }
            "extend_candidates" if is_vector && !extend_candidates => {
                extend_candidates = true;
            }
            "keep_pruned_connections" if is_vector && !keep_pruned_connections => {
                keep_pruned_connections = true;
            }
            "hashed_vector" if is_vector && !hashed_vector => {
                hashed_vector = true;
            }
            "degree" if is_diskann && degree.is_none() => {
                let value = split.atom()?;
                degree = Some(u16_atom(value, "DEGREE").ok()?);
            }
            "l_build" if is_diskann && l_build.is_none() => {
                let value = split.atom()?;
                l_build = Some(u16_atom(value, "L_BUILD").ok()?);
            }
            "alpha" if is_diskann && alpha.is_none() => {
                let value = split.atom()?;
                alpha = Some(Float(float_atom(value, "ALPHA").ok()?));
            }
            _ => {
                return None;
            }
        }
    }
    Some(match word {
        "UNIQUE" => IndexKind::Unique,
        "COUNT" => IndexKind::Count {
            where_: count_where,
        },
        "FULLTEXT" => IndexKind::FullText {
            analyzer,
            bm25,
            highlights,
        },
        "HNSW" => IndexKind::Hnsw {
            dimension: dimension?,
            distance,
            vector_type,
            m,
            m0,
            lm,
            efc,
            extend_candidates,
            keep_pruned_connections,
            hashed_vector,
        },
        "DISKANN" => IndexKind::DiskAnn {
            dimension: dimension?,
            distance,
            vector_type,
            degree,
            l_build,
            alpha,
            hashed_vector,
        },
        _ => unreachable!("the header word matched the kind set"),
    })
}

/// The fold-site domain rules mirrored fail-closed (spec/index.md;
/// the engine's define-time checks — live INFO cannot carry a
/// violation, hand-fed canonical text fail-closes here).
fn validate_index_kind(kind: &IndexKind) -> Result<(), Diag> {
    match kind {
        IndexKind::Hnsw { m, m0, .. } => {
            if let Some(m) = m
                && *m > 127
            {
                return Err(tripwire(format!(
                    "`M {m}` exceeds the bound the HNSW kind validates (127)"
                )));
            }
            if let Some(m0) = m0
                && *m0 > u8::MAX as u16
            {
                return Err(tripwire(format!(
                    "`M0 {m0}` exceeds the bound the HNSW kind validates ({})",
                    u8::MAX
                )));
            }
        }
        IndexKind::DiskAnn {
            distance,
            vector_type,
            degree,
            l_build,
            ..
        } => {
            if let Some(distance) = distance
                && !matches!(
                    distance,
                    Distance::Euclidean
                        | Distance::Cosine
                        | Distance::InnerProduct
                        | Distance::CosineNormalized
                )
            {
                return Err(tripwire(format!(
                    "`{distance:?}` is outside the DISKANN distance domain"
                )));
            }
            if let Some(vector_type) = vector_type
                && !matches!(
                    vector_type,
                    VectorType::F32 | VectorType::F16 | VectorType::I8 | VectorType::U8
                )
            {
                return Err(tripwire(format!(
                    "`{vector_type:?}` is outside the DISKANN vector-type domain"
                )));
            }
            if matches!(distance, Some(Distance::CosineNormalized))
                && matches!(vector_type, Some(VectorType::I8) | Some(VectorType::U8))
            {
                return Err(tripwire(
                    "DISKANN `COSINE_NORMALIZED` takes only the F32 and F16 vector types"
                        .to_string(),
                ));
            }
            if let Some(degree) = degree
                && *degree == 0
            {
                return Err(tripwire("`DEGREE 0` is not positive".to_string()));
            }
            if let Some(l_build) = l_build
                && *l_build == 0
            {
                return Err(tripwire("`L_BUILD 0` is not positive".to_string()));
            }
        }
        _ => {}
    }
    Ok(())
}

/// The fields-arity fold rules mirrored fail-closed: count takes no
/// fields, the fulltext and vector kinds exactly one, plain/unique at
/// least one.
fn validate_index_fields(kind: &IndexKind, fields: &[Expr]) -> Result<(), Diag> {
    let (required, at_most) = match kind {
        IndexKind::Count { .. } => (0usize, 0usize),
        IndexKind::FullText { .. } | IndexKind::Hnsw { .. } | IndexKind::DiskAnn { .. } => (1, 1),
        IndexKind::Plain | IndexKind::Unique => (1, usize::MAX),
    };
    if fields.len() < required || fields.len() > at_most {
        let (expected, kind_word) = match kind {
            IndexKind::Count { .. } => ("no fields", "COUNT"),
            IndexKind::FullText { .. } => ("exactly one field", "FULLTEXT"),
            IndexKind::Hnsw { .. } => ("exactly one field", "HNSW"),
            IndexKind::DiskAnn { .. } => ("exactly one field", "DISKANN"),
            IndexKind::Plain => ("at least one field", "plain"),
            IndexKind::Unique => ("at least one field", "UNIQUE"),
        };
        return Err(tripwire(format!(
            "{} field(s) — the {} kind takes {expected}",
            fields.len(),
            kind_word
        )));
    }
    Ok(())
}

/// The `indexes` section of `INFO FOR TABLE` (spec/index.md): INFO
/// input is maximal — a bare SDL `FULLTEXT` materializes
/// `FULLTEXT ANALYZER like BM25(1.2,0.75)` and HNSW/DISKANN print
/// every parameter including the derived `M0`/`LM` — and the read
/// result carries all of it (the elision at defaults is the
/// normalizer's). `CONCURRENTLY` never appears (transient —
/// preserve from SDL); the printer always spells `FIELDS` (the
/// `columns:` alias is input-only) and `DIST` (codegen's
/// `DISTANCE` alias — both accepted). The kind word is PEELed off the
/// FIELDS payload, not captured: a field expression may legitimately
/// be named `unique` or `count` (`FIELDS unique UNIQUE` is
/// live-printed), so the kind words cannot be capture stops.
pub fn read_indexes(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "indexes", |name, ddl| {
        match read_definition(name, ddl, split_index) {
            // Legacy `VS` scoring (spec/index.md's reverse section):
            // the token is print-only in 3.2.4 — the embedded parser
            // rejects the DDL the printer emits — and
            // the shape routes to verbatim `raw`.
            Err(Diag::InfoUnparsable { .. }) if has_bare_word(ddl, "VS") => {
                Ok(raw_read_entry(name, ddl, RawRoute::IndexLegacyVs))
            }
            other => other,
        }
    })
}

/// The verbatim-`raw` entry: the one construction the Raw arms share
/// — [`split_outcome`]'s, and the readers that catch the route before
/// the canonicalizing parse. The body is the trimmed DDL with a
/// terminator normalized on: INFO never prints the `;`, and a
/// re-emitted multi-statement batch rejects a non-final statement
/// without one (SDL-authored raw blocks carry their own).
fn raw_read_entry(name: &str, ddl: &str, route: RawRoute) -> ReadEntry {
    let mut body = ddl.trim().to_string();
    if !body.ends_with(';') {
        body.push(';');
    }
    ReadEntry {
        name: name.to_string(),
        stmt: Stmt::Raw(Raw {
            body,
            comments: Vec::new(),
            clause_comments: Vec::new(),
        }),
        markers: Markers {
            raw_routed: Some(route),
            ..Markers::default()
        },
    }
}

/// Whether the kind carries a signed float — a negative `BM25` side,
/// `ALPHA`, `LM`, or MINKOWSKI order (engine-legal, outside SDL's
/// unsigned scalar grammar — the statement routes to verbatim `raw`).
/// Sign, not ordering: `-0` prints and must route like `-1`
/// (`-0.0 < 0.0` is false).
fn index_kind_has_signed_float(kind: &IndexKind) -> bool {
    match kind {
        IndexKind::FullText {
            bm25: Some(Bm25(k, b)),
            ..
        } => k.is_sign_negative() || b.is_sign_negative(),
        IndexKind::Hnsw { lm, distance, .. } => {
            matches!(lm, Some(Float(v)) if v.is_sign_negative())
                || matches!(distance, Some(Distance::Minkowski(Float(v))) if v.is_sign_negative())
        }
        IndexKind::DiskAnn {
            alpha, distance, ..
        } => {
            matches!(alpha, Some(Float(v)) if v.is_sign_negative())
                || matches!(distance, Some(Distance::Minkowski(Float(v))) if v.is_sign_negative())
        }
        _ => false,
    }
}

fn split_index(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "INDEX")?;
    let name = header_name(&mut split)?;
    // The positional head (canonicalize pins the printer's order:
    // name → ON → FIELDS → kind → COMMENT).
    split.expect_word("ON", "the index's table")?;
    let on = split.capture_ident("ON")?;
    let mut unrepresentable = false;
    let (fields, kind) = if split.peek_word_is("FIELDS") {
        split.expect_word("FIELDS", "the index's field list")?;
        match capture_fields_and_kind(&mut split, &mut unrepresentable)? {
            Ok(pair) => pair,
            Err(route) => return Ok(SplitOutcome::Raw(route)),
        }
    } else {
        // The COUNT kind's bare form: the kind tail directly (a count
        // index takes no fields — its INFO entry prints none).
        let blob = split.capture_expr(
            &[CaptureStopWord("COMMENT", CaptureFollow::String)],
            "COUNT",
        )?;
        let kind = index_kind_tail(blob.trim(), &mut unrepresentable).ok_or_else(|| {
            tripwire("`COUNT` is the only kind that prints without FIELDS".to_string())
        })?;
        (Vec::new(), kind)
    };
    if unrepresentable || index_kind_has_signed_float(&kind) {
        return Ok(SplitOutcome::Raw(RawRoute::IndexSignedFloat));
    }
    validate_index_fields(&kind, &fields)?;
    validate_index_kind(&kind)?;
    let mut comment = None;
    if split.peek_word_is("COMMENT") {
        split.expect_word("COMMENT", "the COMMENT clause")?;
        match split.capture_comment()? {
            CommentValue::Doc(doc) => comment = Some(doc),
            CommentValue::NotPlainString => split.route_raw(RawRoute::CommentNotPlainString),
        }
    }
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.finish()?;
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Index(Index {
            name: Ident::new(name.clone()),
            kind,
            on: Ident::new(on),
            fields,
            fields_key: Carried::default(),
            concurrently: false,
            comment,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

/// The FIELDS payload plus the kind riding its tail. The capture ends
/// at the COMMENT clause (whose value must open a string —
/// [`CaptureFollow::String`] — so a field named `comment` never stops
/// it) or the statement's `;`. The peel: the leftmost depth-0
/// EXACT-uppercase kind word that sits in KIND position — not in the
/// comma list (the printer joins fields with `", "` and appends the
/// kind after a single space), not at the payload's head (every
/// FIELDS-bearing kind takes at least one field; the COUNT kind
/// prints without FIELDS at all) — and whose tail fully parses as
/// that kind's parameters.
fn capture_fields_and_kind(
    split: &mut Splitter,
    unrepresentable: &mut bool,
) -> Result<Result<(Vec<Expr>, IndexKind), RawRoute>, Diag> {
    let blob = split.capture_expr(
        &[CaptureStopWord("COMMENT", CaptureFollow::String)],
        "FIELDS",
    )?;
    let mut saw_candidate = false;
    for (offset, word, depth) in scanned_words(&blob) {
        if depth != 0 || !exact_kind_word(&word) {
            continue;
        }
        let head = blob[..offset].trim();
        if head.is_empty() {
            // A kind word with no fields before it cannot be the
            // kind — it is a field ident spelled in uppercase, and
            // the plain fallback reads it.
            continue;
        }
        if head.ends_with('.') || head.ends_with(':') {
            // Name-segment position (`meta.UNIQUE` — a field path's
            // last segment prints bare): an operand segment, never
            // the kind.
            continue;
        }
        if head.ends_with(',') {
            // Comma-list position: the printer joins fields with
            // `", "`, so this word is a field item (`FIELDS a,
            // UNIQUE` — two plain fields), never the kind.
            continue;
        }
        saw_candidate = true;
        let Some(kind) = index_kind_tail(&blob[offset..], unrepresentable) else {
            // The kind's parameters hold an engine-legal value outside
            // SDL's scalar grammar (`LM Infinity`) — the raw route, not
            // the drift tripwire.
            if *unrepresentable {
                return Ok(Err(RawRoute::IndexSignedFloat));
            }
            continue;
        };
        let fields = comma_items(head, "FIELDS", |item| {
            (!item.trim().is_empty()).then(|| Expr {
                canon: item.trim().to_string(),
            })
        })?;
        return Ok(Ok((fields, kind)));
    }
    if saw_candidate {
        // A kind word rode the payload but its parameters do not
        // parse — printer drift or hand-fed drift, never a silent
        // fold-back to a plain field list.
        return Err(tripwire(
            "the FIELDS payload carries a kind word whose parameters do not parse".to_string(),
        ));
    }
    // No kind word peels: the plain index's field list.
    let fields = comma_items(blob.trim(), "FIELDS", |item| {
        (!item.trim().is_empty()).then(|| Expr {
            canon: item.trim().to_string(),
        })
    })?;
    Ok(Ok((fields, IndexKind::Plain)))
}

/// The permission operation words — the `FOR` stop's follow set (a
/// WHERE expression cannot contain a bare `for` followed by an
/// operation word, so the confirmation is unambiguous).
const PERMISSION_OP_WORDS: &[&str] = &["select", "create", "update", "delete"];

/// The stops after a group's `WHERE` expression inside PERMISSIONS
/// (the next group's `FOR`, confirmed by its operation word).
const PERMISSION_GROUP_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("FOR", CaptureFollow::Word(PERMISSION_OP_WORDS)),
    CaptureStopWord("COMMENT", CaptureFollow::String),
];

/// The `PERMISSIONS` payload shared by `table` and `field`
/// (spec/README permission form 2): `NONE`/`FULL` single values, or
/// `FOR <ops> <value>` groups merged by equal value — the printer's
/// own grouping (op words lowercase, groups `,`-joined; an all-`NONE`
/// field payload prints as one merged group, delete-Full elided on
/// tables). `allow_delete` is `false` for fields (the engine rejects
/// `FOR delete` on fields at define — mirrored fail-closed).
fn capture_op_permissions(split: &mut Splitter, allow_delete: bool) -> Result<OpPermissions, Diag> {
    let word = split
        .word()
        .ok_or_else(|| tripwire("expected the PERMISSIONS value".to_string()))?;
    match word.to_ascii_lowercase().as_str() {
        "none" => return Ok(OpPermissions::None),
        "full" => return Ok(OpPermissions::Full),
        "for" => {}
        other => {
            return Err(tripwire(format!(
                "`{other}` is not a PERMISSIONS value the printer materializes"
            )));
        }
    }
    let mut groups = Vec::new();
    loop {
        let mut ops = Vec::new();
        let value;
        loop {
            let Some(word) = split.word() else {
                return Err(tripwire("expected a permission operation".to_string()));
            };
            if !allow_delete && word.eq_ignore_ascii_case("delete") {
                return Err(tripwire(
                    "`FOR delete` never materializes on a field (the engine rejects it)"
                        .to_string(),
                ));
            }
            let op = op_atom(word);
            match op {
                Some(op) => {
                    ops.push(op);
                    split.lexer.skip_surrealql_trivia();
                    if split.lexer.eat_byte(b',') {
                        continue;
                    }
                    let Some(value_word) = split.word() else {
                        return Err(tripwire(
                            "expected the group's permission value".to_string(),
                        ));
                    };
                    value = permission_value(split, value_word, PERMISSION_GROUP_STOPS)?;
                    break;
                }
                None if ops.is_empty() => {
                    // `FOR` with no operation word — the group shape
                    // the printer materializes always carries at
                    // least one operation.
                    return Err(tripwire(
                        "`FOR` demands at least one permission operation".to_string(),
                    ));
                }
                None => {
                    return Err(tripwire(format!(
                        "`{word}` is not a permission value the printer materializes"
                    )));
                }
            }
        }
        groups.push(PermissionGroup { ops, value });
        split.lexer.skip_surrealql_trivia();
        if split.peek_word_is("FOR") {
            // A WHERE capture ate the group separator `,` (it stops
            // at the next group's FOR word).
            split.expect_word("FOR", "the next permission group")?;
            continue;
        }
        if split.lexer.eat_byte(b',') {
            split.expect_word("FOR", "the next permission group")?;
            continue;
        }
        break;
    }
    Ok(OpPermissions::Groups(groups))
}

/// One operation word (the lowercase printer spellings).
fn op_atom(word: &str) -> Option<PermissionOp> {
    Some(match word.to_ascii_lowercase().as_str() {
        "select" => PermissionOp::Select,
        "create" => PermissionOp::Create,
        "update" => PermissionOp::Update,
        "delete" => PermissionOp::Delete,
        _ => return None,
    })
}

/// One permission group's value word; `WHERE`'s expression capture
/// rides `stops` (the next group's `FOR`), and a trailing group
/// separator comma is trimmed off the captured text.
fn permission_value(
    split: &mut Splitter,
    word: &str,
    stops: &[CaptureStopWord],
) -> Result<Permission, Diag> {
    match word.to_ascii_lowercase().as_str() {
        "none" => Ok(Permission::None),
        "full" => Ok(Permission::Full),
        "where" => {
            let text = split.capture_expr(stops, "PERMISSIONS")?;
            let text = text.trim().trim_end_matches(',').trim_end();
            Ok(Permission::Expr(Expr {
                canon: text.to_string(),
            }))
        }
        other => Err(tripwire(format!(
            "`{other}` is not a permission value the printer materializes"
        ))),
    }
}

/// The stops after a field's `DEFAULT [ALWAYS]` capture — the clauses
/// that can follow in the printer's pinned order (`define/field.rs`'s
/// fmt arm order: TYPE [FLEXIBLE] → DEFAULT → READONLY → INLINE →
/// VALUE → ASSERT → COMPUTED → REFERENCE → COMMENT → GRAPHQL_* →
/// PERMISSIONS), each with its value's confirming shape.
const FIELD_DEFAULT_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("READONLY", CaptureFollow::Any),
    CaptureStopWord("INLINE", CaptureFollow::Any),
    CaptureStopWord("VALUE", CaptureFollow::Value),
    CaptureStopWord("ASSERT", CaptureFollow::Value),
    CaptureStopWord("COMPUTED", CaptureFollow::Value),
    CaptureStopWord("REFERENCE", CaptureFollow::Value),
    CaptureStopWord("COMMENT", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_ALIAS", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_DEPRECATED", CaptureFollow::String),
    CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
];

/// After `VALUE` (DEFAULT precedes it in the print order).
const FIELD_VALUE_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("ASSERT", CaptureFollow::Value),
    CaptureStopWord("COMPUTED", CaptureFollow::Value),
    CaptureStopWord("REFERENCE", CaptureFollow::Value),
    CaptureStopWord("COMMENT", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_ALIAS", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_DEPRECATED", CaptureFollow::String),
    CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
];

/// After `ASSERT`.
const FIELD_ASSERT_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("COMPUTED", CaptureFollow::Value),
    CaptureStopWord("REFERENCE", CaptureFollow::Value),
    CaptureStopWord("COMMENT", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_ALIAS", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_DEPRECATED", CaptureFollow::String),
    CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
];

/// After `COMPUTED`.
const FIELD_COMPUTED_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("REFERENCE", CaptureFollow::Value),
    CaptureStopWord("COMMENT", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_ALIAS", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_DEPRECATED", CaptureFollow::String),
    CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
];

/// After `REFERENCE ON DELETE THEN`'s action expression.
const FIELD_REFERENCE_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("COMMENT", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_ALIAS", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_DEPRECATED", CaptureFollow::String),
    CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
];

/// The `fields` section of `INFO FOR TABLE` (spec/field.md):
/// `PERMISSIONS` renders unconditionally (FULL is the default — the
/// read carries it; elision is the normalizer's), `REFERENCE` always
/// prints `ON DELETE …` (IGNORE maps to the bare-flag
/// [`OnDelete::Ignore`]), types arrive desugared (`option<T>` as
/// `none | T` — same_definition compares canonical kind texts), and
/// the server-synthesized `.*` subfields read as the ordinary fields
/// they print as (the deepest-first fold is the normalizer's).
pub fn read_fields(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "fields", |name, ddl| {
        read_definition(name, ddl, split_field)
    })
}

fn split_field(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "FIELD")?;
    let (name, path) = capture_field_path(&mut split)?;
    split.expect_word("ON", "the field's table")?;
    let on = split.capture_ident("ON")?;
    // The pinned clause order (canonicalize re-prints the printer's
    // `define/field.rs` fmt arm order): TYPE [FLEXIBLE] → DEFAULT
    // [ALWAYS] → READONLY → INLINE → VALUE → ASSERT → COMPUTED →
    // REFERENCE → COMMENT → GRAPHQL_* → PERMISSIONS.
    let mut kind = None;
    let mut flexible = false;
    if split.peek_word_is("TYPE") {
        split.expect_word("TYPE", "the TYPE clause")?;
        kind = Some(Kind::new(capture_kind(&mut split)?));
        if split.peek_word_is("FLEXIBLE") {
            split.expect_word("FLEXIBLE", "the FLEXIBLE flag")?;
            flexible = true;
        }
    }
    let mut default = None;
    if split.peek_word_is("DEFAULT") {
        split.expect_word("DEFAULT", "the DEFAULT clause")?;
        let always = split.peek_word_is("ALWAYS");
        if always {
            split.expect_word("ALWAYS", "the DEFAULT ALWAYS form")?;
        }
        let text = split.capture_expr(FIELD_DEFAULT_STOPS, "DEFAULT")?;
        let expr = Expr { canon: text };
        default = Some(if always {
            FieldDefault::Always(expr)
        } else {
            FieldDefault::Set(expr)
        });
    }
    let mut readonly = false;
    if split.peek_word_is("READONLY") {
        split.expect_word("READONLY", "the READONLY flag")?;
        readonly = true;
    }
    let mut inline = false;
    if split.peek_word_is("INLINE") {
        split.expect_word("INLINE", "the INLINE flag")?;
        inline = true;
    }
    let mut value = None;
    if split.peek_word_is("VALUE") {
        split.expect_word("VALUE", "the VALUE clause")?;
        let text = split.capture_expr(FIELD_VALUE_STOPS, "VALUE")?;
        value = Some(Expr { canon: text });
    }
    let mut assert_ = None;
    if split.peek_word_is("ASSERT") {
        split.expect_word("ASSERT", "the ASSERT clause")?;
        let text = split.capture_expr(FIELD_ASSERT_STOPS, "ASSERT")?;
        assert_ = Some(Expr { canon: text });
    }
    let mut computed = None;
    if split.peek_word_is("COMPUTED") {
        split.expect_word("COMPUTED", "the COMPUTED clause")?;
        let text = split.capture_expr(FIELD_COMPUTED_STOPS, "COMPUTED")?;
        computed = Some(Expr { canon: text });
    }
    let mut reference = None;
    if split.peek_word_is("REFERENCE") {
        split.expect_word("REFERENCE", "the REFERENCE clause")?;
        // The printer always spells the action (`ON DELETE IGNORE`
        // minimum — never bare).
        split.expect_word("ON", "`REFERENCE ON DELETE`")?;
        split.expect_word("DELETE", "`REFERENCE ON DELETE`")?;
        let word = split
            .word()
            .ok_or_else(|| tripwire("expected the ON DELETE action".to_string()))?;
        reference = Some(match word.to_ascii_lowercase().as_str() {
            "reject" => OnDelete::Reject,
            "cascade" => OnDelete::Cascade,
            "ignore" => OnDelete::Ignore,
            "unset" => OnDelete::Unset,
            "then" => {
                let text = split.capture_expr(FIELD_REFERENCE_STOPS, "ON DELETE THEN")?;
                OnDelete::Then(Expr { canon: text })
            }
            other => {
                return Err(tripwire(format!(
                    "`{other}` is not an ON DELETE action the printer materializes"
                )));
            }
        });
    }
    let mut comment = None;
    if split.peek_word_is("COMMENT") {
        split.expect_word("COMMENT", "the COMMENT clause")?;
        match split.capture_comment()? {
            CommentValue::Doc(doc) => comment = Some(doc),
            CommentValue::NotPlainString => split.route_raw(RawRoute::CommentNotPlainString),
        }
    }
    let mut alias = None;
    let mut deprecated = None;
    if split.peek_word_is("GRAPHQL_ALIAS") {
        split.expect_word("GRAPHQL_ALIAS", "the GRAPHQL_ALIAS clause")?;
        alias = Some(split.capture_string("GRAPHQL_ALIAS")?);
    }
    if split.peek_word_is("GRAPHQL_DEPRECATED") {
        split.expect_word("GRAPHQL_DEPRECATED", "the GRAPHQL_DEPRECATED clause")?;
        deprecated = Some(split.capture_string("GRAPHQL_DEPRECATED")?);
    }
    let graphql = (alias.is_some() || deprecated.is_some()).then(|| Graphql {
        alias: alias.clone(),
        deprecated: deprecated.clone(),
    });
    if !split.peek_word_is("PERMISSIONS") {
        return Err(tripwire(
            "PERMISSIONS always renders in INFO (FULL is the default) — it is missing".to_string(),
        ));
    }
    split.expect_word("PERMISSIONS", "the PERMISSIONS clause")?;
    let permissions = capture_op_permissions(&mut split, false)?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.finish()?;
    // A TYPE-less non-computed field (`DEFINE FIELD x ON t;` — legal
    // hand-run DDL the printer reprints typeless) is outside SDL's
    // grammar: verbatim `raw` per spec/field.md's reverse section.
    if kind.is_none() && computed.is_none() {
        return Ok(SplitOutcome::Raw(RawRoute::FieldUntyped));
    }
    let field = Field {
        name: path,
        kind,
        on: Ident::new(on),
        default,
        value,
        assert: assert_,
        computed,
        readonly,
        flexible,
        inline,
        reference,
        permissions: Some(permissions),
        graphql,
        comment,
        comments: Vec::new(),
        trailing_comments: Vec::new(),
        clause_comments: Vec::new(),
    };
    validate_field_shape(&field)?;
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Field(field)),
        name,
    })
}

/// A field's `.`-path: name segments (bare or quoted, decoded) and
/// `*` wildcards; the first segment is always a name (the AST's
/// invariant). The cross-check name is the INFO key's own rendering:
/// per-segment printer escaping (`ident_needs_escape` — a segment
/// holding a dot prints `` `a.b` ``), `.`-joined.
fn capture_field_path(split: &mut Splitter) -> Result<(String, FieldPath), Diag> {
    let mut segments = Vec::new();
    loop {
        split.lexer.skip_surrealql_trivia();
        if split.lexer.eat_byte(b'*') {
            segments.push(FieldPathSeg::Wildcard);
        } else {
            match split.lexer.read_ident() {
                Ok(Some((_, text, _))) => {
                    segments.push(FieldPathSeg::Name(Ident::new(text)));
                }
                Ok(None) => {
                    return Err(tripwire("expected the field's name path".to_string()));
                }
                Err(_) => {
                    return Err(tripwire(
                        "malformed escape in the field's name path".to_string(),
                    ));
                }
            }
        }
        split.lexer.skip_surrealql_trivia();
        if split.lexer.eat_byte(b'.') {
            continue;
        }
        break;
    }
    if matches!(segments.first(), Some(FieldPathSeg::Wildcard)) {
        return Err(tripwire(
            "the field's path cannot start with the `*` wildcard".to_string(),
        ));
    }
    // The cross-check name is the INFO key's own rendering: a
    // single-name path is the decoded name (the object renderer's
    // JSON wrapper decodes away — keyword names print bare, live
    // `in:`), while a multi-segment path keeps per-segment shape
    // escaping (`` `a.b`.c `` — `word_shaped`'s rule, keywords bare).
    let key = match segments.as_slice() {
        [FieldPathSeg::Name(ident)] => ident.raw.clone(),
        segments => segments
            .iter()
            .map(|segment| match segment {
                FieldPathSeg::Wildcard => "*".to_string(),
                FieldPathSeg::Name(ident) => {
                    if crate::keywords::word_shaped(&ident.raw) {
                        ident.raw.clone()
                    } else {
                        let mut out = String::new();
                        out.push('`');
                        crate::keywords::escape_with(&mut out, &ident.raw, '`');
                        out.push('`');
                        out
                    }
                }
            })
            .collect::<Vec<_>>()
            .join("."),
    };
    Ok((key, FieldPath { segments }))
}

/// The fold-site field rules mirrored fail-closed (spec/field.md; the
/// engine's define-time checks): computed is exclusive with the value
/// clauses and demands a top-level path, `flexible` needs an
/// object-bearing kind, `reference` is top-level only, `inline` needs
/// a top-level non-`id`/`in`/`out` non-computed field (the host-relation
/// half is define-order, the engine's to say), and the `id`
/// field takes no VALUE, DEFAULT ALWAYS, COMPUTED, READONLY,
/// FLEXIBLE, INLINE, or REFERENCE. (A TYPE-less non-computed field
/// routes to verbatim `raw` before this runs —
/// [`RawRoute::FieldUntyped`].)
fn validate_field_shape(field: &Field) -> Result<(), Diag> {
    let path = &field.name;
    if field.computed.is_some() {
        if field.default.is_some() {
            return Err(tripwire("COMPUTED excludes DEFAULT".to_string()));
        }
        if field.value.is_some() {
            return Err(tripwire("COMPUTED excludes VALUE".to_string()));
        }
        if field.assert.is_some() {
            return Err(tripwire("COMPUTED excludes ASSERT".to_string()));
        }
        if field.reference.is_some() {
            return Err(tripwire("COMPUTED excludes REFERENCE".to_string()));
        }
        if field.readonly {
            return Err(tripwire("COMPUTED excludes READONLY".to_string()));
        }
        if field.inline {
            return Err(tripwire("COMPUTED excludes INLINE".to_string()));
        }
        if !path.is_top_level() {
            return Err(tripwire("COMPUTED demands a top-level path".to_string()));
        }
    }
    if field.reference.is_some() && !path.is_top_level() {
        return Err(tripwire("REFERENCE demands a top-level path".to_string()));
    }
    if field.inline && !path.is_top_level() {
        return Err(tripwire("INLINE demands a top-level path".to_string()));
    }
    // The `id` spelling rides the id block below (the parse side's
    // InvalidFieldId parity); `in`/`out` ride this check.
    if field.inline
        && path
            .sole_name()
            .is_some_and(|name| matches!(name.raw.as_str(), "in" | "out"))
    {
        return Err(tripwire(
            "the `in` and `out` fields cannot be INLINE".to_string(),
        ));
    }
    if field.flexible
        && !field
            .kind
            .as_ref()
            .is_some_and(|kind| crate::kind::kind_contains_object(&kind.text))
    {
        return Err(tripwire(
            "FLEXIBLE takes only a kind containing an object".to_string(),
        ));
    }
    if path.sole_name().is_some_and(|name| name.raw == "id") {
        if field.value.is_some() {
            return Err(tripwire("the `id` field takes no VALUE clause".to_string()));
        }
        if matches!(field.default, Some(FieldDefault::Always(_))) {
            return Err(tripwire(
                "the `id` field takes no DEFAULT ALWAYS".to_string(),
            ));
        }
        if field.computed.is_some() {
            return Err(tripwire(
                "the `id` field takes no COMPUTED clause".to_string(),
            ));
        }
        if field.readonly {
            return Err(tripwire(
                "the `id` field takes no READONLY flag".to_string(),
            ));
        }
        if field.flexible {
            return Err(tripwire(
                "the `id` field takes no FLEXIBLE flag".to_string(),
            ));
        }
        if field.inline {
            return Err(tripwire("the `id` field takes no INLINE flag".to_string()));
        }
        if field.reference.is_some() {
            return Err(tripwire(
                "the `id` field takes no REFERENCE clause".to_string(),
            ));
        }
    }
    Ok(())
}

/// The stops after a table relation's `IN`/`OUT` payload (the
/// relation's remaining clauses — `DROP` prints between the sides and
/// the schema flag; a view's `AS` included — a relation with a query
/// is a deviant view, and the capture leaves the `AS` word for the
/// clause loop; `ENFORCED` prints after the sides). `LIGHTWEIGHT` is
/// deliberately NOT a stop: it is also a legal endpoint ident (the
/// greedy list read — spec/table.md), so it rides the payload and
/// [`peel_lightweight_flag`] splits the flag spelling off afterwards.
const RELATION_TAIL_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("DROP", CaptureFollow::Any),
    CaptureStopWord("ENFORCED", CaptureFollow::Any),
    CaptureStopWord("SCHEMAFULL", CaptureFollow::Any),
    CaptureStopWord("SCHEMALESS", CaptureFollow::Any),
    CaptureStopWord("CHANGEFEED", CaptureFollow::Digit),
    CaptureStopWord("AS", CaptureFollow::Value),
    CaptureStopWord("COMMENT", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_ALIAS", CaptureFollow::String),
    CaptureStopWord("GRAPHQL_DEPRECATED", CaptureFollow::String),
    CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
];

/// The `tables` section of `INFO FOR DB` together with its `INFO FOR
/// TABLE` child maps (the child maps ride their DB input
/// — INFO output never names its own scope). Each table's
/// `fields`/`indexes`/`events` sections nest into it in that fixed
/// order; a child the AST cannot express (a legacy-`VS` index), a
/// raw-routed table, and a view (whose grammar has no nested-define
/// spelling) surface their statements top-level, each its own entry —
/// SDL reserves `raw` for top-level placement (README rule 8), so
/// nesting them would be unprintable. The `lives` and `tables` child
/// keys (live queries and the foreign-view registry) are ignored per
/// spec/table.md. A child naming no table, or a child whose `ON`
/// disagrees with its host, diagnoses (one child diagnostic
/// fails the whole table entry).
pub fn read_tables(
    db: &InfoObject,
    children: &BTreeMap<String, InfoObject>,
) -> Vec<Result<ReadEntry, Diag>> {
    let mut out = Vec::new();
    let Some(InfoValue::Object(tables)) = db.0.get("tables") else {
        if db.0.contains_key("tables") {
            out.push(Err(Diag::InfoUnknownShape {
                span: Span::point(0),
                message: "the `tables` section is not an object".to_string(),
            }));
        } else if !children.is_empty() {
            for name in children.keys() {
                out.push(Err(Diag::InfoUnknownShape {
                    span: Span::point(0),
                    message: format!(
                        "the child map names `{name}`, but there is no `tables` section"
                    ),
                }));
            }
        }
        return out;
    };
    for name in children.keys() {
        if !tables.0.contains_key(name) {
            out.push(Err(Diag::InfoUnknownShape {
                span: Span::point(0),
                message: format!(
                    "the child map names `{name}`, which the `tables` section does not hold"
                ),
            }));
        }
    }
    for (name, value) in &tables.0 {
        let InfoValue::Ddl(ddl) = value else {
            out.push(Err(Diag::InfoUnknownShape {
                span: Span::point(0),
                message: format!("the `tables` entry `{name}` is not a DDL string"),
            }));
            continue;
        };
        let entry = match read_definition(name, ddl, split_table) {
            Err(diag) => {
                // The table's own diagnostic, and its child map's
                // entries surfaced top-level — the same posture as a
                // raw-routed table (nothing a child map held is
                // silently dropped).
                out.push(Err(diag));
                if let Some(child) = children.get(name) {
                    out.extend(child_entries(name, child));
                }
                continue;
            }
            Ok(entry) => entry,
        };
        let Some(child) = children.get(name) else {
            out.push(Ok(entry));
            continue;
        };
        if entry.markers.raw_routed.is_some() {
            // A raw-routed table (a deviant view, say) carries no
            // structured body to nest into — its children surface
            // top-level, each its own entry, exactly like a raw child
            // does; nothing is silently dropped.
            out.push(Ok(entry));
            out.extend(child_entries(name, child));
            continue;
        }
        // A view's grammar has no nested-define spelling (SDL's view
        // block holds only the query and trailing clauses) — its
        // children surface top-level too, as the standalone `on:`-
        // carrying statements they read as.
        if matches!(&entry.stmt, Stmt::Table(table) if table.kind.is_view()) {
            out.push(Ok(entry));
            out.extend(child_entries(name, child));
            continue;
        }
        match nest_table_children(name, entry, child) {
            Ok((entry, raw_children)) => {
                out.push(Ok(entry));
                out.extend(raw_children.into_iter().map(Ok));
            }
            Err(diag) => out.push(Err(diag)),
        }
    }
    out
}

/// The three child sections of one table's child map, in the fixed
/// order: expressible children host-checked; a raw-routed child and
/// any diagnostic ride as their own entries.
fn child_entries(host: &str, child: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    let mut out = Vec::new();
    for section in ["fields", "indexes", "events"] {
        let read: fn(&InfoObject) -> Vec<Result<ReadEntry, Diag>> = match section {
            "fields" => read_fields,
            "indexes" => read_indexes,
            _ => read_events,
        };
        for entry in read(child) {
            match entry {
                Err(diag) => out.push(Err(diag)),
                Ok(entry) => {
                    if entry.markers.raw_routed.is_some() {
                        out.push(Ok(entry));
                        continue;
                    }
                    match check_child_on(host, &entry.stmt) {
                        Ok(()) => out.push(Ok(entry)),
                        Err(diag) => out.push(Err(diag)),
                    }
                }
            }
        }
    }
    out
}

/// Nests one table's child sections (fixed order fields → indexes →
/// events): expressible children nest (their `ON` must name the
/// host), a raw-routed child surfaces top-level (see
/// [`read_tables`]), and any child diagnostic fails the TABLE entry —
/// a silently dropped child would mis-represent the table. Raw-routed
/// tables and views never reach here (their children surface
/// top-level at the call site).
fn nest_table_children(
    host: &str,
    mut table: ReadEntry,
    child: &InfoObject,
) -> Result<(ReadEntry, Vec<ReadEntry>), Diag> {
    let Stmt::Table(inner) = &mut table.stmt else {
        unreachable!("the caller routed raw-routed tables to child_entries");
    };
    // A LIGHTWEIGHT relation's `in`/`out` fields are the header's own
    // implication (the engine auto-installs them from the endpoints
    // and rejects any DEFINE FIELD on the table — probe-pinned), so
    // nesting them would print SDL the engine itself rejects at
    // define. They drop at read — the endpoints in the kind are the
    // source of truth, exactly as `enforced` is — but ONLY in their
    // implied shape (the endpoint kind, the FULL default, no other
    // clause): a hand-carried divergent `in`/`out` is impossible-live,
    // and dropping it would mis-represent state, so it stays and the
    // written SDL trips the parser's lightweight validation loud
    // (fail-closed).
    let lightweight = matches!(
        inner.kind,
        TableKind::Relation {
            lightweight: true,
            ..
        }
    );
    let implied = if lightweight {
        let TableKind::Relation { from, to, .. } = &inner.kind else {
            unreachable!("lightweight is a Relation payload");
        };
        Some([
            ("in", super::normalize::endpoint_kind(from)),
            ("out", super::normalize::endpoint_kind(to)),
        ])
    } else {
        None
    };
    let mut raw_children = Vec::new();
    for entry in child_entries(host, child) {
        let entry = entry?;
        if entry.markers.raw_routed.is_some() {
            raw_children.push(entry);
            continue;
        }
        match entry.stmt {
            stmt @ (Stmt::Field(_) | Stmt::Index(_) | Stmt::Event(_)) => {
                if let (Some(implied), Stmt::Field(field)) = (&implied, &stmt) {
                    let dropped = implied.iter().any(|(name, expected)| {
                        field.name.sole_name().is_some_and(|n| n.raw == *name)
                            && field.permissions == Some(OpPermissions::Full)
                            && !field.flexible
                            && super::normalize::field_is_default_shaped_except_permissions(field)
                            && field.kind.as_ref().is_some_and(|k| {
                                super::normalize::kind_text_equal(&k.text, expected)
                            })
                    });
                    if dropped {
                        continue;
                    }
                }
                inner.nested.push(stmt);
            }
            _ => unreachable!("a host-checked non-raw child is a field, index, or event"),
        }
    }
    Ok((table, raw_children))
}

/// The child's `ON` must name its host table — the parser's
/// `table_host` injection, mirrored.
fn check_child_on(host: &str, stmt: &Stmt) -> Result<(), Diag> {
    let on = match stmt {
        Stmt::Field(field) => &field.on,
        Stmt::Index(index) => &index.on,
        Stmt::Event(event) => &event.on,
        _ => unreachable!("a non-raw child is a field, index, or event"),
    };
    if on.raw != host {
        return Err(Diag::InfoUnknownShape {
            span: Span::point(0),
            message: format!(
                "the child definition's `ON {}` disagrees with its host table `{host}`",
                on.raw
            ),
        });
    }
    Ok(())
}

fn split_table(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "TABLE")?;
    let name = header_name(&mut split)?;
    let mut type_word: Option<String> = None;
    let mut drop = false;
    let mut schema_full: Option<bool> = None;
    let mut from: Option<Vec<Ident>> = None;
    let mut to: Option<Vec<Ident>> = None;
    let mut enforced = false;
    let mut lightweight = false;
    let mut inline_edges: Option<u16> = None;
    let mut inline_references: Option<u16> = None;
    let mut changefeed: Option<ChangeFeed> = None;
    let mut as_query: Option<Expr> = None;
    let mut permissions: Option<OpPermissions> = None;
    let mut alias: Option<String> = None;
    let mut deprecated: Option<String> = None;
    let mut comment = None;
    split.clause_loop("table", &mut |split, key| {
        match key {
            "type" => {
                if type_word.is_some() {
                    return Err(duplicated("TYPE"));
                }
                let word = split
                    .word()
                    .ok_or_else(|| tripwire("expected the TYPE value".to_string()))?;
                type_word = Some(word.to_ascii_uppercase());
            }
            "drop" => {
                if drop {
                    return Err(duplicated("DROP"));
                }
                drop = true;
            }
            "schemafull" => {
                if schema_full.is_some() {
                    return Err(duplicated("SCHEMAFULL"));
                }
                schema_full = Some(true);
            }
            "schemaless" => {
                if schema_full.is_some() {
                    return Err(duplicated("SCHEMALESS"));
                }
                schema_full = Some(false);
            }
            "in" => {
                if from.is_some() {
                    return Err(duplicated("IN"));
                }
                let blob = split.capture_expr(RELATION_TAIL_STOPS, "IN")?;
                let (left, right) = peel_out_word(&blob);
                let (left, flag) = peel_lightweight_flag(left);
                if flag {
                    read_lightweight_flag(&mut lightweight, &mut enforced)?;
                }
                from = Some(pipe_idents(left, "IN")?);
                if let Some(right) = right {
                    if to.is_some() {
                        return Err(duplicated("OUT"));
                    }
                    let (right, flag) = peel_lightweight_flag(right);
                    if flag {
                        read_lightweight_flag(&mut lightweight, &mut enforced)?;
                    }
                    to = Some(pipe_idents(right, "OUT")?);
                }
            }
            "out" => {
                if to.is_some() {
                    return Err(duplicated("OUT"));
                }
                let blob = split.capture_expr(RELATION_TAIL_STOPS, "OUT")?;
                let (blob, flag) = peel_lightweight_flag(&blob);
                if flag {
                    read_lightweight_flag(&mut lightweight, &mut enforced)?;
                }
                to = Some(pipe_idents(blob, "OUT")?);
            }
            "enforced" => {
                if enforced {
                    return Err(duplicated("ENFORCED"));
                }
                enforced = true;
            }
            // The engine's define normalization means INFO always
            // prints `ENFORCED LIGHTWEIGHT` together — the SDL-side
            // parse-fold (lightweight implies enforced) rides the
            // same rule, so a hand-fed `LIGHTWEIGHT` without ENFORCED
            // reads the same AST. (The reverse order would trip the
            // ENFORCED duplicate check — a non-live shape, fail-closed.)
            "lightweight" => read_lightweight_flag(&mut lightweight, &mut enforced)?,
            // The INLINE cache caps: `INLINE EDGES <n>` /
            // `INLINE REFERENCES <n>` (the number mandatory — the
            // printer only materializes `Some` sides, probe-pinned
            // live 2026-09-25). Kind-independent on the engine; the
            // >256 bound is the engine's define check mirrored — a cap
            // it would reject never rides the AST.
            "inline" => {
                let which = if split.peek_word_is("EDGES") {
                    "EDGES"
                } else if split.peek_word_is("REFERENCES") {
                    "REFERENCES"
                } else {
                    return Err(tripwire(format!(
                        "expected `EDGES` or `REFERENCES` after `INLINE`, found `{}`",
                        split.word().unwrap_or("<end>")
                    )));
                };
                split.expect_word(which, "the `INLINE` clause")?;
                let text = split
                    .atom()
                    .ok_or_else(|| tripwire("expected the `INLINE` cap value".to_string()))?;
                uint_atom(text, "INLINE")?;
                // u64 so an out-of-domain value gets the domain message
                // (the printer's caps are small; anything past u64 is
                // not an integer literal the dialect materializes).
                let cap: u64 = text.parse().map_err(|_| {
                    tripwire(format!(
                        "`{text}` is not the integer literal the `INLINE` clause materializes"
                    ))
                })?;
                if cap > 256 {
                    return Err(tripwire(
                        "an INLINE cache cap cannot exceed 256".to_string(),
                    ));
                }
                let cap = cap as u16;
                if which == "EDGES" {
                    if inline_edges.is_some() {
                        return Err(duplicated("INLINE EDGES"));
                    }
                    inline_edges = Some(cap);
                } else {
                    if inline_references.is_some() {
                        return Err(duplicated("INLINE REFERENCES"));
                    }
                    inline_references = Some(cap);
                }
            }
            "changefeed" => {
                if changefeed.is_some() {
                    return Err(duplicated("CHANGEFEED"));
                }
                let text = split.capture_expr(
                    &[
                        CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
                        CaptureStopWord("COMMENT", CaptureFollow::String),
                        CaptureStopWord("GRAPHQL_ALIAS", CaptureFollow::String),
                        CaptureStopWord("GRAPHQL_DEPRECATED", CaptureFollow::String),
                    ],
                    "CHANGEFEED",
                )?;
                changefeed = Some(parse_changefeed(&text)?);
            }
            "as" => {
                if as_query.is_some() {
                    return Err(duplicated("AS"));
                }
                let text = split.capture_expr(
                    &[
                        // A view's CHANGEFEED prints after the query
                        // (the deviant shape the raw route covers);
                        // the duration opens with a digit.
                        CaptureStopWord("CHANGEFEED", CaptureFollow::Digit),
                        CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
                    ],
                    "AS",
                )?;
                as_query = Some(Expr { canon: text });
            }
            "comment" => {
                if comment.is_some() {
                    return Err(duplicated("COMMENT"));
                }
                match split.capture_comment()? {
                    CommentValue::Doc(doc) => comment = Some(doc),
                    CommentValue::NotPlainString => {
                        split.route_raw(RawRoute::CommentNotPlainString);
                    }
                }
            }
            "graphql_alias" => {
                if alias.is_some() {
                    return Err(duplicated("GRAPHQL_ALIAS"));
                }
                alias = Some(split.capture_string("GRAPHQL_ALIAS")?);
            }
            "graphql_deprecated" => {
                if deprecated.is_some() {
                    return Err(duplicated("GRAPHQL_DEPRECATED"));
                }
                deprecated = Some(split.capture_string("GRAPHQL_DEPRECATED")?);
            }
            "permissions" => {
                if permissions.is_some() {
                    return Err(duplicated("PERMISSIONS"));
                }
                permissions = Some(capture_op_permissions(split, true)?);
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the table definition"
                )));
            }
        }
        Ok(())
    })?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.require_end()?;
    let Some(permissions) = permissions else {
        return Err(tripwire(
            "PERMISSIONS always renders in INFO (NONE is the default) — it is missing".to_string(),
        ));
    };
    let graphql =
        (alias.is_some() || deprecated.is_some()).then_some(Graphql { alias, deprecated });
    // The view branch: SDL's fixed shape (`TYPE ANY SCHEMALESS`, no
    // DROP, no CHANGEFEED) or verbatim raw — re-emitting the fixed
    // shape over a deviation would silently mutate the table on
    // re-apply (spec/table.md's reverse section).
    if let Some(query) = as_query {
        // The printer always spells the view's TYPE (`TYPE ANY`).
        let view_type = type_word.as_deref() == Some("ANY");
        let relation_clauses = enforced || from.is_some() || to.is_some();
        // A cap-bearing view deviates too: SDL does not author the
        // INLINE caps on views (semantically inert there), so the
        // fixed emission could not carry them.
        let inline_caps = inline_edges.is_some() || inline_references.is_some();
        if !view_type
            || relation_clauses
            || schema_full != Some(false)
            || drop
            || changefeed.is_some()
            || inline_caps
        {
            return Ok(SplitOutcome::Raw(RawRoute::TableViewDeviant));
        }
        let kind = TableKind::View { query };
        return Ok(SplitOutcome::Stmt {
            markers: Markers::default(),
            stmt: Box::new(Stmt::Table(Table {
                name: Ident::new(name.clone()),
                kind,
                // The view's fixed emission carries the schema flag.
                schema_full: None,
                drop: false,
                changefeed: None,
                inline_edges: None,
                inline_references: None,
                permissions: Some(permissions),
                graphql,
                nested: Vec::new(),
                comment,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })),
            name,
        });
    }
    let type_word = type_word
        .as_deref()
        .ok_or_else(|| tripwire("TYPE always renders in INFO — it is missing".to_string()))?;
    let schema = schema_full.ok_or_else(|| {
        tripwire("the schema flag always renders in INFO — it is missing".to_string())
    })?;
    let kind = match type_word {
        "NORMAL" | "ANY" => {
            if enforced || from.is_some() || to.is_some() {
                return Err(tripwire(
                    "the relation clauses (`IN`/`OUT`/`ENFORCED`/`LIGHTWEIGHT`) ride the \
                     RELATION kind only"
                        .to_string(),
                ));
            }
            if type_word == "NORMAL" {
                TableKind::Normal
            } else {
                TableKind::Any
            }
        }
        "RELATION" => {
            // The arrow form's first from-side element prints bare
            // (a quoted head would parse as the table's NAME), so an
            // escape-requiring first endpoint cannot ride the AST —
            // the writer's invariant; the statement routes to
            // verbatim `raw` (spec/table.md's reverse section). Later
            // elements and the OUT side print via `push_ident` and
            // re-parse quoted.
            if from
                .as_ref()
                .and_then(|endpoints| endpoints.first())
                .is_some_and(|first| !crate::keywords::word_shaped(&first.raw))
            {
                return Ok(SplitOutcome::Raw(RawRoute::TableEndpointUnrepresentable));
            }
            // The parse-fold: `lightweight` implies `enforced` (the
            // engine's define normalization, probe-pinned — the SDL
            // parser's rule, mirrored here so both sides build the
            // same AST from either spelling). A sides-less
            // LIGHTWEIGHT is the engine's own define rejection
            // mirrored: the writer's `(None, None)` spelling could
            // not carry the word, so reading one would silently drop
            // the flag on fmt — it diagnoses instead (hand-fed only;
            // live INFO cannot carry the shape).
            if lightweight && (from.is_none() || to.is_none()) {
                return Err(tripwire(
                    "a LIGHTWEIGHT relation requires explicit IN and OUT tables".to_string(),
                ));
            }
            TableKind::Relation {
                from,
                to,
                enforced: enforced || lightweight,
                lightweight,
            }
        }
        other => {
            return Err(tripwire(format!(
                "`{other}` is not a TYPE the printer materializes"
            )));
        }
    };
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Table(Table {
            name: Ident::new(name.clone()),
            kind,
            schema_full: Some(schema),
            drop,
            changefeed,
            inline_edges,
            inline_references,
            permissions: Some(permissions),
            graphql,
            nested: Vec::new(),
            comment,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

/// Splits a relation's `IN` payload at the printer's own uppercase
/// `OUT` word (the sides print back-to-back: `IN a | b OUT c`). The
/// printer joins endpoints with `" | "` and appends the clause `OUT`
/// after a space, so a candidate whose head is EMPTY is an endpoint
/// ident named `OUT` in list position (`IN OUT OUT host` — the first
/// `OUT` is the endpoint, the second the clause); only when every
/// candidate sits at the head does the ident reading win and no OUT
/// side follow.
fn peel_out_word(blob: &str) -> (&str, Option<&str>) {
    for (offset, word, depth) in scanned_words(blob) {
        if depth != 0 || word != "OUT" {
            continue;
        }
        if !blob[..offset].trim().is_empty() {
            return (&blob[..offset], Some(blob[offset + word.len()..].trim()));
        }
        // A head-empty candidate is an endpoint ident named `OUT` in
        // list position, never the clause — keep scanning.
    }
    // Every `OUT` sat at the payload's head (or none did): the ident
    // reading wins — the endpoints are named `OUT` and no OUT side
    // follows.
    (blob, None)
}

/// Splits a trailing depth-0 `LIGHTWEIGHT` flag off an IN/OUT payload
/// (`(blob, was_flag)`). `lightweight` is also a legal endpoint ident,
/// so position decides, mirroring the SDL parser's greedy list read:
/// a candidate at the payload's HEAD or preceded by `|` (a list
/// continuation — `OUT a | LIGHTWEIGHT`, the endpoint spelled in the
/// flag's own uppercase) is an endpoint; one preceded by an endpoint
/// word is the flag (`OUT a LIGHTWEIGHT SCHEMALESS` — a hand-fed
/// order; the live print carries the flag after `ENFORCED`, outside
/// the payload). The pipe check reads the RAW prefix (scanned_words
/// records bare words only), trimmed of trivia; a quoted spelling
/// never yields as a bare word, so it never peels. The flag spelling
/// is exact-uppercase — the pinned dialect spells clause words
/// uppercase, so a lowercase endpoint ident never collides.
fn peel_lightweight_flag(blob: &str) -> (&str, bool) {
    let last = scanned_words(blob)
        .into_iter()
        .rev()
        .find(|(_, _, depth)| *depth == 0);
    let Some((offset, word, _)) = last else {
        return (blob, false);
    };
    if word != "LIGHTWEIGHT" {
        return (blob, false);
    }
    let prefix = blob[..offset].trim_end();
    if prefix.is_empty() || prefix.ends_with('|') {
        return (blob, false);
    }
    (&blob[..offset], true)
}

/// The clause loop's LIGHTWEIGHT arm: the parse-fold sets both flags
/// (the engine's define normalization — INFO always prints `ENFORCED
/// LIGHTWEIGHT` together, so a hand-fed `LIGHTWEIGHT` without
/// ENFORCED reads the same AST; the reverse order trips the ENFORCED
/// duplicate check — a non-live shape, fail-closed).
fn read_lightweight_flag(lightweight: &mut bool, enforced: &mut bool) -> Result<(), Diag> {
    if *lightweight {
        return Err(duplicated("LIGHTWEIGHT"));
    }
    *lightweight = true;
    *enforced = true;
    Ok(())
}

/// A `|`-separated identifier list (a relation's `IN`/`OUT` endpoints)
/// — the printer's own join.
fn pipe_idents(text: &str, clause: &str) -> Result<Vec<Ident>, Diag> {
    let mut scan = Scanner::new(text);
    let mut idents = Vec::new();
    loop {
        scan.skip_surrealql_trivia();
        match scan.read_ident() {
            Ok(Some((_, raw, _))) => idents.push(Ident::new(raw)),
            _ => {
                return Err(tripwire(format!(
                    "`{text}` is not the endpoint list the `{clause}` clause materializes"
                )));
            }
        }
        scan.skip_surrealql_trivia();
        if scan.eat_byte(b'|') {
            continue;
        }
        if scan.is_at_end() {
            return Ok(idents);
        }
        return Err(tripwire(format!(
            "`{text}` is not the endpoint list the `{clause}` clause materializes"
        )));
    }
}

// ---------------------------------------------------------------------
// The `accesses` reader
// ---------------------------------------------------------------------

/// The `accesses` section — ROOT, NS, and DB all file `accesses` and
/// the `ON <LEVEL>` word rides the DDL text at every tier
/// (live-probed full words), so one reader serves all three
/// (`read_users`'s shape). INFO input is maximal: DURATION always
/// materializes, a symmetric verify key and every issuer key arrive
/// as the `'[REDACTED]'` sentinel (the engine's own redaction, which
/// SDL's shape-only rule made the only authorable spelling), a
/// record's auto-issued HS512 block prints like an authored one, and
/// a bearer's `jwt` never prints at all — the read carries all of it
/// faithfully (the elision/preserve decisions are the normalizer's).
pub fn read_accesses(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "accesses", |name, ddl| {
        read_definition(name, ddl, split_access)
    })
}

/// The `WITH` stop's follow — the second word confirms the clause
/// (`WITH JWT`, `WITH REFRESH`): a SurrealQL `WITH INDEX` inside a
/// captured expression is content, not a clause.
const ACCESS_WITH_WORDS: &[&str] = &["jwt", "refresh"];

/// The JWT source captures' `WITH` follow — widened with `issuer`:
/// inside `WITH JWT` the next `WITH` opens `WITH ISSUER KEY …`.
const ACCESS_SOURCE_WITH_WORDS: &[&str] = &["jwt", "refresh", "issuer"];

/// The `DURATION` stop's follow — the value's sides open with `FOR`.
const ACCESS_DURATION_FOLLOW: &[&str] = &["for"];

/// Stops after SIGNIN: the printer-order clause keywords that can
/// follow it. `CONTEXT` (3.3.0's record payload) stops the capture so
/// the clause loop diagnoses it loud — SDL cannot author it yet, and
/// swallowing it into the value would silently mis-read.
const ACCESS_SIGNIN_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("WITH", CaptureFollow::Word(ACCESS_WITH_WORDS)),
    CaptureStopWord("AUTHENTICATE", CaptureFollow::Value),
    CaptureStopWord("CONTEXT", CaptureFollow::Any),
    CaptureStopWord("DURATION", CaptureFollow::Word(ACCESS_DURATION_FOLLOW)),
    CaptureStopWord("COMMENT", CaptureFollow::String),
];

/// Stops after SIGNUP — SIGNIN itself prepended (the printer emits
/// the pair in that order).
const ACCESS_SIGNUP_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("SIGNIN", CaptureFollow::Value),
    CaptureStopWord("WITH", CaptureFollow::Word(ACCESS_WITH_WORDS)),
    CaptureStopWord("AUTHENTICATE", CaptureFollow::Value),
    CaptureStopWord("CONTEXT", CaptureFollow::Any),
    CaptureStopWord("DURATION", CaptureFollow::Word(ACCESS_DURATION_FOLLOW)),
    CaptureStopWord("COMMENT", CaptureFollow::String),
];

/// Stops after AUTHENTICATE (printer order: DURATION, then COMMENT).
const ACCESS_AUTHENTICATE_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("CONTEXT", CaptureFollow::Any),
    CaptureStopWord("DURATION", CaptureFollow::Word(ACCESS_DURATION_FOLLOW)),
    CaptureStopWord("COMMENT", CaptureFollow::String),
];

/// Stops after CONTEXT (printer order: DURATION, then COMMENT).
const ACCESS_CONTEXT_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("DURATION", CaptureFollow::Word(ACCESS_DURATION_FOLLOW)),
    CaptureStopWord("COMMENT", CaptureFollow::String),
];

/// Stops after a verification-source value (URL / local KEY): the
/// source's own `WITH ISSUER` tail and the statement clauses that can
/// follow — including the record-only clauses, so a hand-fed
/// order-deviant text reaches the membership tripwire instead of
/// swallowing them. The 3.3.0 additions (`CONTEXT`, `AUDIENCE`) stop
/// the capture so the clause loop diagnoses them loud (probe-pinned
/// live, 2026-09-25: both parse on the embed; SDL cannot author them
/// yet — swallowing `AUDIENCE 'aud1'` into the URL canon was a real
/// mis-read the fail-closed pin caught).
const ACCESS_SOURCE_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("SIGNUP", CaptureFollow::Value),
    CaptureStopWord("SIGNIN", CaptureFollow::Value),
    CaptureStopWord("WITH", CaptureFollow::Word(ACCESS_SOURCE_WITH_WORDS)),
    CaptureStopWord("AUTHENTICATE", CaptureFollow::Value),
    CaptureStopWord("CONTEXT", CaptureFollow::Any),
    CaptureStopWord("AUDIENCE", CaptureFollow::String),
    CaptureStopWord("DURATION", CaptureFollow::Word(ACCESS_DURATION_FOLLOW)),
    CaptureStopWord("COMMENT", CaptureFollow::String),
];

/// Stops after the issuer's KEY value (nothing re-opens the source).
const ACCESS_ISSUER_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("AUTHENTICATE", CaptureFollow::Value),
    CaptureStopWord("CONTEXT", CaptureFollow::Any),
    CaptureStopWord("AUDIENCE", CaptureFollow::String),
    CaptureStopWord("DURATION", CaptureFollow::Word(ACCESS_DURATION_FOLLOW)),
    CaptureStopWord("COMMENT", CaptureFollow::String),
];

fn split_access(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "ACCESS")?;
    let name = header_name(&mut split)?;
    let mut on: Option<Authored<Level>> = None;
    let mut type_word: Option<String> = None;
    let mut subject: Option<BearerSubject> = None;
    let mut signup: Option<Expr> = None;
    let mut signin: Option<Expr> = None;
    let mut refresh = false;
    let mut jwt: Option<JwtSource> = None;
    let mut duration = AccessDuration::default();
    let mut saw_duration = false;
    let mut authenticate: Option<Expr> = None;
    let mut context: Option<Expr> = None;
    let mut comment = None;
    let mut markers = Markers::default();
    split.clause_loop("access", &mut |split, key| {
        match key {
            "on" => {
                if on.is_some() {
                    return Err(duplicated("ON"));
                }
                let word = split
                    .word()
                    .ok_or_else(|| tripwire("expected the level after `ON`".to_string()))?;
                let level = match word.to_ascii_lowercase().as_str() {
                    "root" => Level::Root,
                    "namespace" => Level::Namespace,
                    "database" => Level::Database,
                    other => {
                        return Err(tripwire(format!(
                            "`{other}` is not an access level (the canonical words are ROOT, \
                             NAMESPACE, DATABASE)"
                        )));
                    }
                };
                on = Some(Authored::canonical(level));
            }
            "type" => {
                if type_word.is_some() {
                    return Err(duplicated("TYPE"));
                }
                let word = split
                    .word()
                    .ok_or_else(|| tripwire("expected the variant after `TYPE`".to_string()))?;
                // The jwt variant's verification source is POSITIONAL
                // — `TYPE JWT <source>` with no `WITH` (upstream's
                // grammar; only the issuer tail carries one). Record
                // and bearer reach the same source shape through
                // `WITH JWT` in the WITH loop.
                match word.to_ascii_lowercase().as_str() {
                    "jwt" => {
                        if jwt.is_some() {
                            return Err(duplicated("WITH JWT"));
                        }
                        let mut source = capture_jwt_source(split, &mut markers)?;
                        fold_issuer_mirror(&mut source);
                        jwt = Some(source);
                        type_word = Some("jwt".to_string());
                    }
                    "record" => type_word = Some("record".to_string()),
                    "bearer" => type_word = Some("bearer".to_string()),
                    other => {
                        return Err(tripwire(format!(
                            "`{other}` is not an access variant (the canonical words are JWT, \
                             RECORD, BEARER)"
                        )));
                    }
                }
            }
            "signup" | "signin" => {
                if type_word.as_deref() != Some("record") {
                    return Err(tripwire(format!(
                        "`{}` never renders on a non-record access (the printer emits it \
                         only after `TYPE RECORD`)",
                        key.to_ascii_uppercase()
                    )));
                }
                if key == "signup" {
                    if signup.is_some() {
                        return Err(duplicated("SIGNUP"));
                    }
                    signup = Some(Expr {
                        canon: split.capture_expr(ACCESS_SIGNUP_STOPS, "SIGNUP")?,
                    });
                } else {
                    if signin.is_some() {
                        return Err(duplicated("SIGNIN"));
                    }
                    signin = Some(Expr {
                        canon: split.capture_expr(ACCESS_SIGNIN_STOPS, "SIGNIN")?,
                    });
                }
            }
            "with" => {
                let word = split.word().ok_or_else(|| {
                    tripwire("expected `JWT` or `REFRESH` after `WITH`".to_string())
                })?;
                match word.to_ascii_lowercase().as_str() {
                    "refresh" => {
                        if type_word.as_deref() != Some("record") {
                            return Err(tripwire(
                                "`WITH REFRESH` never renders on a non-record access".to_string(),
                            ));
                        }
                        if refresh {
                            return Err(duplicated("WITH REFRESH"));
                        }
                        refresh = true;
                    }
                    "jwt" => {
                        if jwt.is_some() {
                            return Err(duplicated("WITH JWT"));
                        }
                        let mut source = capture_jwt_source(split, &mut markers)?;
                        fold_issuer_mirror(&mut source);
                        jwt = Some(source);
                    }
                    other => {
                        return Err(tripwire(format!(
                            "`{other}` is not a `WITH` clause (the printer emits JWT or \
                             REFRESH)"
                        )));
                    }
                }
            }
            "for" => {
                if type_word.as_deref() != Some("bearer") {
                    return Err(tripwire(
                        "`FOR <subject>` never renders on a non-bearer access".to_string(),
                    ));
                }
                if subject.is_some() {
                    return Err(duplicated("FOR"));
                }
                let word = split
                    .word()
                    .ok_or_else(|| tripwire("expected the subject after `FOR`".to_string()))?;
                subject = Some(match word.to_ascii_lowercase().as_str() {
                    "user" => BearerSubject::User,
                    "record" => BearerSubject::Record,
                    other => {
                        return Err(tripwire(format!(
                            "`{other}` is not a bearer subject (the canonical words are \
                             USER, RECORD)"
                        )));
                    }
                });
            }
            "duration" => {
                if saw_duration {
                    return Err(duplicated("DURATION"));
                }
                saw_duration = true;
                let text = split.capture_expr(
                    &[CaptureStopWord("COMMENT", CaptureFollow::String)],
                    "DURATION",
                )?;
                duration = access_duration_group(&text)?;
            }
            "authenticate" => {
                if authenticate.is_some() {
                    return Err(duplicated("AUTHENTICATE"));
                }
                authenticate = Some(Expr {
                    canon: split.capture_expr(ACCESS_AUTHENTICATE_STOPS, "AUTHENTICATE")?,
                });
            }
            "context" => {
                if context.is_some() {
                    return Err(duplicated("CONTEXT"));
                }
                // The pinned print order: CONTEXT sits after
                // AUTHENTICATE, before DURATION (the printer's own
                // note — a rendered definition must re-parse).
                context = Some(Expr {
                    canon: split.capture_expr(ACCESS_CONTEXT_STOPS, "CONTEXT")?,
                });
            }
            "comment" => {
                if comment.is_some() {
                    return Err(duplicated("COMMENT"));
                }
                match split.capture_comment()? {
                    CommentValue::Doc(doc) => comment = Some(doc),
                    CommentValue::NotPlainString => {
                        split.route_raw(RawRoute::CommentNotPlainString);
                    }
                }
            }
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the access definition"
                )));
            }
        }
        Ok(())
    })?;
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    split.require_end()?;
    let on = on.ok_or_else(|| tripwire("ON always renders in INFO — it is missing".to_string()))?;
    let type_word = type_word
        .ok_or_else(|| tripwire("TYPE always renders in INFO — it is missing".to_string()))?;
    if !saw_duration {
        return Err(tripwire(
            "DURATION always materializes in INFO — it is missing".to_string(),
        ));
    }
    let kind = match type_word.as_str() {
        "jwt" => AccessKind::Jwt {
            source: jwt.ok_or_else(|| {
                tripwire(
                    "a jwt access always renders a verification source — it is missing".to_string(),
                )
            })?,
        },
        "record" => AccessKind::Record {
            signup,
            signin,
            // A record with no `jwt { … }` of its own prints the
            // auto-issued HS512 block (`KEY '[REDACTED]' WITH ISSUER
            // KEY '[REDACTED]'`) — which reads through the ordinary
            // WITH JWT arm and the mirror fold below into the
            // authored-mirror shape (spec/access.md's reverse
            // section; the sentinel adoption rule).
            jwt,
            refresh,
        },
        _ => AccessKind::Bearer {
            subject: subject.ok_or_else(|| {
                tripwire(
                    "a bearer access always renders `FOR <subject>` — it is missing".to_string(),
                )
            })?,
            // A bearer's `jwt { … }` is stored but never rendered
            // (live-probed): INFO's absence reads as absence, and
            // re-emitting without it drops the stored clause — the
            // preserve list's territory.
            jwt,
        },
    };
    // Hand-fed shapes the engine's define-time check never stores:
    // CONTEXT rides only a record-authenticating access at the
    // database level (validate_access_context's two conditions — the
    // variant half and the level half — mirrored exactly; the parser
    // holds the same mirror at its fold).
    if context.is_some() {
        let authenticates_record = matches!(
            kind,
            AccessKind::Record { .. }
                | AccessKind::Bearer {
                    subject: BearerSubject::Record,
                    ..
                }
        ) && on.value == Level::Database;
        if !authenticates_record {
            return Err(tripwire(
                "a CONTEXT clause can only ride an access method that authenticates a record"
                    .to_string(),
            ));
        }
    }
    Ok(SplitOutcome::Stmt {
        markers,
        stmt: Box::new(Stmt::Access(Access {
            name: Ident::new(name.clone()),
            on,
            kind,
            duration: Some(duration),
            authenticate,
            context,
            comment,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name,
    })
}

/// One `WITH JWT <source>` payload (spec/access.md): the verification
/// shape (`URL <expr>` or `ALGORITHM <word> KEY <canon>`) plus the
/// optional `WITH ISSUER [ALGORITHM <word>] KEY <canon>`. Key values
/// adopt as canon text; the `'[REDACTED]'` sentinel sets
/// [`Markers::redacted`] and never rides as a real secret (the
/// `PASSHASH` precedent).
fn capture_jwt_source(split: &mut Splitter, markers: &mut Markers) -> Result<JwtSource, Diag> {
    let word = split
        .word()
        .ok_or_else(|| tripwire("expected the verification source after `WITH JWT`".to_string()))?;
    let verify = match word.to_ascii_lowercase().as_str() {
        "url" => JwtVerify::Url(Expr {
            canon: split.capture_expr(ACCESS_SOURCE_STOPS, "URL")?,
        }),
        "algorithm" => {
            let algorithm = access_algorithm_word(split)?;
            split.expect_word("KEY", "the local key's clause")?;
            let canon = split.capture_expr(ACCESS_SOURCE_STOPS, "KEY")?;
            if canon == SECRET_SENTINEL_CANON {
                markers.redacted = true;
            }
            JwtVerify::Key {
                algorithm,
                key: Expr { canon },
            }
        }
        other => {
            return Err(tripwire(format!(
                "`{other}` is not a verification source (the printer emits URL or ALGORITHM \
                 KEY)"
            )));
        }
    };
    // `AUDIENCE '<value>', …` rides positionally between the source
    // and the issuer (3.3 — the printer's fixed order). The values
    // are the stored strings (the engine string-checks at define),
    // printed as plain literals.
    let audience = if split.peek_word_is("AUDIENCE") {
        split.expect_word("AUDIENCE", "the audience clause")?;
        let mut values = vec![split.capture_string("AUDIENCE")?];
        loop {
            split.lexer.skip_surrealql_trivia();
            if !split.lexer.eat_byte(b',') {
                break;
            }
            values.push(split.capture_string("AUDIENCE")?);
        }
        Some(values)
    } else {
        None
    };
    let issuer = if split.peek_word_is("WITH") {
        split.expect_word("WITH", "the issuer clause")?;
        split.expect_word("ISSUER", "the issuer clause")?;
        let algorithm = if split.peek_word_is("ALGORITHM") {
            split.expect_word("ALGORITHM", "the issuer algorithm")?;
            Some(access_algorithm_word(split)?)
        } else {
            None
        };
        split.expect_word("KEY", "the issuer key clause")?;
        let canon = split.capture_expr(ACCESS_ISSUER_STOPS, "KEY")?;
        if canon == SECRET_SENTINEL_CANON {
            markers.redacted = true;
        }
        Some(JwtIssuer {
            algorithm,
            key: Some(Expr { canon }),
        })
    } else {
        None
    };
    Ok(JwtSource {
        verify,
        audience,
        issuer,
    })
}

/// One algorithm word (the lowercase printer spellings, case
/// read-insensitively) over the shared 13-kind mapping; a word
/// outside the set is dialect drift.
fn access_algorithm_word(split: &mut Splitter) -> Result<Algorithm, Diag> {
    let word = split
        .word()
        .ok_or_else(|| tripwire("expected the algorithm word".to_string()))?;
    crate::parser::algorithm_of_word(&word.to_ascii_lowercase()).ok_or_else(|| {
        tripwire(format!(
            "`{word}` is not a JWT algorithm word (the printer spells one of the 13 kinds)"
        ))
    })
}

/// The symmetric mirror's fold: an issuer whose key equals a symmetric
/// verify key re-emits from the verify key alone (`issuer.key: None` —
/// the authored shape, which codegen re-derives). The printer emits
/// the mirror exactly when SurrealQL mirrored the key, so the printed
/// pair reads back to the authored absence. Every symmetric key is
/// the sentinel, so an explicit `issuer { key:
/// '[REDACTED]'; }` is textually the mirror and reads as it too —
/// indistinguishable by design.
fn fold_issuer_mirror(source: &mut JwtSource) {
    if let (JwtVerify::Key { algorithm, key }, Some(issuer)) = (&source.verify, &mut source.issuer)
        && algorithm.is_symmetric()
        && issuer
            .key
            .as_ref()
            .is_some_and(|issuer_key| issuer_key.canon == key.canon)
    {
        issuer.key = None;
    }
}

/// The DURATION value's sides — `FOR GRANT <d>, FOR TOKEN <d>, FOR
/// SESSION <d|NONE>`, capability-dependent in the printer's order,
/// each `,`-joined. SESSION always prints (`NONE` when unset — the
/// faithful `NONE` expression, `duration_group`'s precedent); TOKEN
/// only when the access can issue; GRANT only when grant-capable.
/// Values parse by value, never spelling (`parse_duration_text`).
fn access_duration_group(text: &str) -> Result<AccessDuration, Diag> {
    let mut duration = AccessDuration::default();
    for side in text.split(',') {
        let rest = strip_word(side, "FOR")
            .map(str::trim_start)
            .ok_or_else(|| {
                tripwire(format!("expected `FOR` opening the DURATION side `{side}`"))
            })?;
        let (label, value) = rest
            .split_once(char::is_whitespace)
            .ok_or_else(|| tripwire("a DURATION side always carries a value".to_string()))?;
        let label = label.to_ascii_lowercase();
        let value = value.trim();
        // SESSION's unset side prints as the NONE sentinel (the
        // faithful expression, `duration_group`'s precedent); GRANT
        // and TOKEN only ever carry real durations.
        let value_ok = match label.as_str() {
            "session" => value.eq_ignore_ascii_case("NONE") || parse_duration_text(value).is_some(),
            "grant" | "token" => parse_duration_text(value).is_some(),
            other => {
                return Err(tripwire(format!(
                    "`{other}` is not a DURATION side (the printer emits GRANT, TOKEN, \
                     SESSION)"
                )));
            }
        };
        if !value_ok {
            return Err(tripwire(format!(
                "`{value}` is not a duration the DURATION clause materializes"
            )));
        }
        let expr = Some(Expr {
            canon: value.to_string(),
        });
        match label.as_str() {
            "grant" => duration.grant = expr,
            "token" => duration.token = expr,
            _ => duration.session = expr,
        }
    }
    Ok(duration)
}

// ---------------------------------------------------------------------
// The `apis` reader
// ---------------------------------------------------------------------

/// The `apis` section (DB tier — spec/README's INFO scope table). The
/// printer always emits `FOR any` first with an unconditional
/// `PERMISSIONS` on every FOR clause (live-probed): the absent-`any`
/// rule folds a `FOR any` carrying no THEN, no middleware, and
/// `PERMISSIONS FULL` (the group-level FULL elision) back to an
/// absent `any`, and every other clause reads faithfully. The path is
/// carried verbatim as printed — the catalog re-prints its PARSED
/// path, so an escape-authored path arrives post-flip and
/// textually indistinguishable from a normally-authored dynamic
/// segment; flip detection is the differ's (re-parse comparison),
/// never the reader's.
pub fn read_apis(info: &InfoObject) -> Vec<Result<ReadEntry, Diag>> {
    read_section(info, "apis", |name, ddl| {
        // A duplicate `FOR any` folds config-last-wins inside the
        // embedded parser itself, so canonicalization would silently
        // deliver the folded shape as if it were authored — detect
        // the duplicate on the RAW text and route the statement to
        // verbatim `raw` (SDL's single-`any` rule is a parse error).
        // Unreachable from live INFO: upstream folds at define time,
        // so the catalog never stores two (live-probed).
        if has_duplicate_for_any(ddl) {
            return Ok(raw_read_entry(name, ddl, RawRoute::ApiDuplicateAny));
        }
        read_definition(name, ddl, split_api)
    })
}

/// Whether two depth-0 `FOR any` clauses ride the raw DDL (the
/// duplicate-`any` detector [`read_apis`] runs before
/// canonicalization). A pair whose words are GLUED to a preceding `.`
/// or `:` is a path segment (`meta.for.any`, `fn::for::any(1)` — both
/// live-printed inside expressions and middleware), never the clause;
/// the printed clause's words always follow trivia.
fn has_duplicate_for_any(ddl: &str) -> bool {
    let mut pairs = 0;
    for pair in scanned_words(ddl).windows(2) {
        let (first_offset, first, first_depth) = &pair[0];
        let (second_offset, second, second_depth) = &pair[1];
        if *first_depth == 0
            && *second_depth == 0
            && first.eq_ignore_ascii_case("FOR")
            && second.eq_ignore_ascii_case("ANY")
            && !word_is_path_joined(ddl, *first_offset)
            && !word_is_path_joined(ddl, *second_offset)
        {
            pairs += 1;
        }
    }
    pairs >= 2
}

/// Whether the word at `offset` is glued to a preceding `.` or `:` —
/// a path segment or idiom part, not a clause keyword.
fn word_is_path_joined(text: &str, offset: usize) -> bool {
    offset > 0 && matches!(text.as_bytes()[offset - 1], b'.' | b':')
}

/// The api `FOR` stop's follow set — the head words that confirm a
/// FOR clause (a THEN expression cannot contain a bare `for` followed
/// by one of these).
const API_FOR_WORDS: &[&str] = &["any", "get", "post", "put", "patch", "delete", "trace"];

/// Stops inside a group's MIDDLEWARE list: the group's other clause
/// keywords, the next group's `FOR`, the statement's `COMMENT`.
const API_MIDDLEWARE_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
    CaptureStopWord("THEN", CaptureFollow::Value),
    CaptureStopWord("FOR", CaptureFollow::Word(API_FOR_WORDS)),
    CaptureStopWord("COMMENT", CaptureFollow::String),
];

/// Stops inside a group's PERMISSIONS value.
const API_PERMISSIONS_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("THEN", CaptureFollow::Value),
    CaptureStopWord("FOR", CaptureFollow::Word(API_FOR_WORDS)),
    CaptureStopWord("COMMENT", CaptureFollow::String),
];

/// Stops after THEN (the group's last clause).
const API_THEN_STOPS: &[CaptureStopWord] = &[
    CaptureStopWord("FOR", CaptureFollow::Word(API_FOR_WORDS)),
    CaptureStopWord("COMMENT", CaptureFollow::String),
];

fn split_api(canonical: &str) -> Result<SplitOutcome, Diag> {
    let mut split = Splitter::new(canonical);
    expect_header(&mut split, "API")?;
    // The route path: one plain string literal, decoded (the inverse
    // of the printer's string rendering). The INFO key is the path
    // string; `split_outcome` cross-checks the two.
    split.lexer.skip_surrealql_trivia();
    let path_span = split
        .lexer
        .capture_string_literal()
        .map_err(|diag| tripwire(describe_clause_capture(&diag)))?;
    let path = string_value(split.src, path_span).map_err(|diag| tripwire(diag.to_string()))?;
    let mut any: Option<ApiAnyBlock> = None;
    let mut actions: Vec<ApiAction> = Vec::new();
    let mut comment = None;
    let mut saw_for = false;
    loop {
        split.lexer.skip_surrealql_trivia();
        if split.lexer.is_at_end() || split.lexer.eat_byte(b';') {
            break;
        }
        let word = split
            .word()
            .ok_or_else(|| tripwire("expected a `FOR` clause or `COMMENT`".to_string()))?;
        match word.to_ascii_lowercase().as_str() {
            "for" => {
                saw_for = true;
                let head = split
                    .word()
                    .ok_or_else(|| tripwire("expected the FOR clause's head".to_string()))?;
                if head.eq_ignore_ascii_case("any") {
                    if any.is_some() {
                        // A second `FOR any` is a parse error in SDL
                        // and never survives to the splitter: both
                        // canonicalization and define time fold it
                        // away (the raw route catches the hand-fed
                        // shape pre-parse in [`read_apis`]). Diagnose
                        // rather than fold silently.
                        return Err(tripwire(
                            "at most one `FOR any` clause renders in an api definition".to_string(),
                        ));
                    }
                    let (config, fallback) = read_api_group(&mut split)?;
                    any = Some(ApiAnyBlock {
                        config,
                        fallback,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    });
                } else if let Some(method) = api_method_atom(head) {
                    let mut methods = vec![method];
                    while split.lexer.eat_byte(b',') {
                        let word = split
                            .word()
                            .ok_or_else(|| tripwire("expected a method after `,`".to_string()))?;
                        match api_method_atom(word) {
                            Some(method) => methods.push(method),
                            None => {
                                return Err(tripwire(format!(
                                    "`{word}` is not an api method (the printer spells the \
                                     six lowercase)"
                                )));
                            }
                        }
                    }
                    let (config, action) = read_api_group(&mut split)?;
                    let Some(action) = action else {
                        return Err(tripwire(
                            "a method group always renders THEN — it is missing".to_string(),
                        ));
                    };
                    actions.push(ApiAction {
                        methods,
                        config,
                        action,
                        comments: Vec::new(),
                        trailing_comments: Vec::new(),
                        clause_comments: Vec::new(),
                    });
                } else {
                    return Err(tripwire(format!(
                        "`{head}` is not a FOR head (the printer emits `any` or the six \
                         methods)"
                    )));
                }
            }
            "comment" => match split.capture_comment()? {
                CommentValue::Doc(doc) => comment = Some(doc),
                CommentValue::NotPlainString => {
                    split.route_raw(RawRoute::CommentNotPlainString);
                    break;
                }
            },
            other => {
                return Err(tripwire(format!(
                    "unknown or misplaced clause `{other}` in the api definition"
                )));
            }
        }
    }
    if let Some(route) = split.raw {
        return Ok(SplitOutcome::Raw(route));
    }
    if !saw_for {
        return Err(tripwire(
            "`FOR any` always renders in INFO — it is missing".to_string(),
        ));
    }
    split.require_end()?;
    // The absent-`any` rule: a `FOR any` with no THEN, no middleware,
    // and the elided default `PERMISSIONS FULL` is the printer's
    // always-materialized clause, not an authored block (spec/api.md's
    // reverse section) — it folds to absence.
    let any = any.filter(|block| {
        !block.config.middleware.is_empty()
            || block.config.permissions.is_some()
            || block.fallback.is_some()
    });
    Ok(SplitOutcome::Stmt {
        markers: Markers::default(),
        stmt: Box::new(Stmt::Api(Api {
            path: path.clone(),
            actions,
            any,
            comment,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        })),
        name: path,
    })
}

/// One method word (the lowercase printer spellings).
fn api_method_atom(word: &str) -> Option<ApiMethod> {
    match word.to_ascii_lowercase().as_str() {
        "get" => Some(ApiMethod::Get),
        "post" => Some(ApiMethod::Post),
        "put" => Some(ApiMethod::Put),
        "patch" => Some(ApiMethod::Patch),
        "delete" => Some(ApiMethod::Delete),
        "trace" => Some(ApiMethod::Trace),
        _ => None,
    }
}

/// One FOR group's config + THEN slot: the printer emits MIDDLEWARE →
/// PERMISSIONS → THEN (read leniently — the three clause keywords are
/// consumed only when they head the cursor). A group's
/// `PERMISSIONS FULL` elides (the default value rides absence); the
/// `any` group's THEN is optional, a method group's required.
fn read_api_group(split: &mut Splitter) -> Result<(ApiGroupConfig, Option<Expr>), Diag> {
    let mut middleware: Option<Vec<Middleware>> = None;
    let mut permissions: Option<Permission> = None;
    let mut then: Option<Expr> = None;
    loop {
        split.lexer.skip_surrealql_trivia();
        let Some(span) = split.lexer.peek_word() else {
            break;
        };
        match span.slice(split.src).to_ascii_lowercase().as_str() {
            "middleware" => {
                if middleware.is_some() {
                    return Err(duplicated("MIDDLEWARE"));
                }
                split.expect_word("MIDDLEWARE", "the group's middleware clause")?;
                let text = split.capture_expr(API_MIDDLEWARE_STOPS, "MIDDLEWARE")?;
                middleware = Some(comma_items(&text, "MIDDLEWARE", middleware_entry)?);
            }
            "permissions" => {
                if permissions.is_some() {
                    return Err(duplicated("PERMISSIONS"));
                }
                split.expect_word("PERMISSIONS", "the group's permissions clause")?;
                permissions =
                    elide_default_permission(split.capture_permission(API_PERMISSIONS_STOPS)?);
            }
            "then" => {
                if then.is_some() {
                    return Err(duplicated("THEN"));
                }
                split.expect_word("THEN", "the group's THEN clause")?;
                then = Some(Expr {
                    canon: split.capture_expr(API_THEN_STOPS, "THEN")?,
                });
            }
            _ => break,
        }
    }
    Ok((
        ApiGroupConfig {
            middleware: middleware.unwrap_or_default(),
            permissions,
        },
        then,
    ))
}

/// The group-level `PERMISSIONS FULL` elision — FULL is SurrealQL's
/// parse default, which the printer always materializes; canonical SDL
/// spells only the deviations (spec/api.md's reverse section).
fn elide_default_permission(permission: Permission) -> Option<Permission> {
    match permission {
        Permission::Full => None,
        other => Some(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Language;

    /// A walk failure is always `SDL-E3001`/`SDL-E3002` — the walker
    /// produces no other diagnostic.
    fn walk_err(text: &str) -> Diag {
        read_info_object(text).expect_err("expected a walk failure")
    }

    /// The `THEN` stop's literal-led follow (the full-product oracle's
    /// catch, pinned per-commit): the printer emits bare literal
    /// then-items — `THEN 1`, `THEN 'done'`, `THEN 5s` — and a WHEN
    /// expression carrying `then` as a path segment never stops
    /// there (the word-led shape stays fail-closed for exactly that
    /// ambiguity).
    #[test]
    fn event_then_literal_items_read() {
        for ddl in [
            "DEFINE EVENT e ON t WHEN true THEN 1",
            "DEFINE EVENT e ON t WHEN true THEN 'done'",
            "DEFINE EVENT e ON t WHEN true THEN 5s",
            "DEFINE EVENT e ON t WHEN true THEN (CREATE log), 2",
        ] {
            let outcome = split_event(ddl).expect("the literal THEN item reads");
            let SplitOutcome::Stmt { stmt, .. } = outcome else {
                panic!("no raw route: {ddl}");
            };
            let Stmt::Event(event) = *stmt else {
                panic!("not an event: {ddl}");
            };
            assert!(!event.then.is_empty(), "THEN dropped: {ddl}");
        }
        // The path shape: `x.then` is content, not the clause.
        let outcome = split_event("DEFINE EVENT e ON t WHEN x.then > 1 THEN (CREATE log)")
            .expect("the path-then WHEN reads");
        let SplitOutcome::Stmt { stmt, .. } = outcome else {
            panic!("no raw route");
        };
        let Stmt::Event(event) = *stmt else {
            panic!("not an event")
        };
        assert_eq!(event.when.expect("WHEN present").canon, "x.then > 1");
    }

    #[test]
    fn walk_error_codes() {
        assert!(matches!(walk_err(""), Diag::InfoUnparsable { .. }));
        assert!(matches!(walk_err("5"), Diag::InfoUnparsable { .. }));
        assert!(matches!(
            walk_err("{ namespaces { }"),
            Diag::InfoUnparsable { .. }
        ));
        assert!(matches!(
            walk_err("{ namespaces: }"),
            Diag::InfoUnknownShape { .. }
        ));
        // A missing entry separator (`a: 'x' 'y'` — no `,`).
        assert!(matches!(
            walk_err("{ a: 'x' 'y' }"),
            Diag::InfoUnparsable { .. }
        ));
        assert!(matches!(
            walk_err("{ a: 1 } tail"),
            Diag::InfoUnparsable { .. }
        ));
        assert!(matches!(
            walk_err("{ a: 'unterminated }"),
            Diag::InfoUnparsable { .. }
        ));
        assert!(matches!(walk_err("{ a }"), Diag::InfoUnparsable { .. }));
    }

    /// The walker over a real ROOT-shaped interchange: string leaves,
    /// a nested object, quoted string keys, scalar/array leaves, and
    /// empty sections — exactly what `INFO FOR ROOT` renders.
    #[test]
    fn walks_a_real_root_shape() {
        let text = "{ accesses: {  }, config: { QUERY_TIMEOUT: NONE }, defaults: {  }, \
             namespaces: { acme: \"DEFINE NAMESPACE acme COMMENT 'Acme Corporation'\", \
             table: 'DEFINE NAMESPACE `table`' }, \
             nodes: { \"68870a11-e6a2-4b7c-b4af-e86b63d714f4\": \"NODE u'68870a11' SEEN 1 ACTIVE\" }, \
             system: { available_parallelism: 16, cpu_usage: 0.0027f, \
             load_average: [2.7f, 3.5f], memory_usage: 35880960 }, users: {  } }";
        let info = read_info_object(text).expect("real ROOT shape must walk");
        assert_eq!(info.0.len(), 7);
        let Some(InfoValue::Object(namespaces)) = info.0.get("namespaces") else {
            panic!("namespaces must be an object");
        };
        assert_eq!(
            namespaces.0.get("acme"),
            Some(&InfoValue::Ddl(
                "DEFINE NAMESPACE acme COMMENT 'Acme Corporation'".to_string()
            ))
        );
        assert_eq!(
            namespaces.0.get("table"),
            Some(&InfoValue::Ddl("DEFINE NAMESPACE `table`".to_string()))
        );
        let Some(InfoValue::Object(system)) = info.0.get("system") else {
            panic!("system must be an object");
        };
        assert_eq!(
            system.0.get("available_parallelism"),
            Some(&InfoValue::Other("16".to_string()))
        );
        assert_eq!(
            system.0.get("cpu_usage"),
            Some(&InfoValue::Other("0.0027f".to_string()))
        );
        assert_eq!(
            system.0.get("load_average"),
            Some(&InfoValue::Other("[2.7f, 3.5f]".to_string()))
        );
        assert_eq!(
            info.0.get("config"),
            Some(&InfoValue::Object(InfoObject(BTreeMap::from([(
                "QUERY_TIMEOUT".to_string(),
                InfoValue::Other("NONE".into())
            )]))))
        );
        assert_eq!(
            info.0.get("users"),
            Some(&InfoValue::Object(InfoObject::default()))
        );
    }

    /// String leaves decode through the SurrealQL escape table: the
    /// printer's `\n` and quote-switching (`"…it's…"`). The walk
    /// unescapes one level (the interchange's), so the DDL text
    /// arrives with the comment's own escape already decoded — the
    /// splitter's string decode then sees the plain literal.
    #[test]
    fn string_leaves_unescape() {
        let text = concat!(
            "{ params: { nl: \"DEFINE PARAM $nl VALUE NONE COMMENT 'line1\\nline2' ",
            "PERMISSIONS FULL\", q: \"DEFINE PARAM $q VALUE NONE COMMENT \\\"it's\\\" ",
            "PERMISSIONS FULL\" } }"
        );
        let info = read_info_object(text).expect("must walk");
        let Some(InfoValue::Object(params)) = info.0.get("params") else {
            panic!("params must be an object");
        };
        let Some(InfoValue::Ddl(nl)) = params.0.get("nl") else {
            panic!("nl must be a string leaf");
        };
        assert!(
            nl.contains("COMMENT 'line1\nline2'"),
            "the leaf decodes the comment's escape: {nl}"
        );
        let Some(InfoValue::Ddl(q)) = params.0.get("q") else {
            panic!("q must be a string leaf");
        };
        assert!(q.contains("it's"), "the quote survives: {q}");
        // And the param split reads both comments back whole.
        let entries = read_params(&info);
        let nl_entry = entries
            .into_iter()
            .find(|entry| entry.as_ref().map(|e| e.name == "nl").unwrap_or(false))
            .expect("nl must read")
            .expect("nl must split");
        let Stmt::Param(param) = nl_entry.stmt else {
            panic!();
        };
        assert_eq!(
            param.comment,
            Some(DocComment {
                lines: vec!["line1".to_string(), "line2".to_string()]
            })
        );
    }

    // -- the statement splitters -------------------------------------

    fn split_ok(kind: fn(&str) -> Result<SplitOutcome, Diag>, canonical: &str) -> ReadEntry {
        match kind(canonical) {
            Err(diag) => panic!("split must succeed for `{canonical}`: {diag:?}"),
            Ok(SplitOutcome::Stmt {
                name,
                stmt,
                markers,
            }) => ReadEntry {
                name,
                stmt: *stmt,
                markers,
            },
            Ok(SplitOutcome::Raw(route)) => {
                panic!("unexpected raw route for `{canonical}`: {route:?}")
            }
        }
    }

    fn split_raw(kind: fn(&str) -> Result<SplitOutcome, Diag>, canonical: &str) -> RawRoute {
        match kind(canonical).expect("raw route must not error") {
            SplitOutcome::Raw(route) => route,
            SplitOutcome::Stmt { .. } => panic!("expected a raw route"),
        }
    }

    fn tripwire_err(kind: fn(&str) -> Result<SplitOutcome, Diag>, canonical: &str) -> Diag {
        kind(canonical).expect_err("expected a tripwire diagnostic")
    }

    #[test]
    fn splits_namespace() {
        let entry = split_ok(
            split_namespace,
            "DEFINE NAMESPACE acme COMMENT 'Acme Corporation';",
        );
        assert_eq!(entry.name, "acme");
        assert_eq!(
            entry.stmt,
            Stmt::Namespace(Namespace {
                name: Ident::new("acme"),
                keyword: Carried::default(),
                comment: Some(DocComment {
                    lines: vec!["Acme Corporation".to_string()]
                }),
                comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
        // Bare (INFO's own shape — no prefix), a quoted keyword name,
        // and both kind prefixes strip.
        for text in [
            "DEFINE NAMESPACE `table`;",
            "DEFINE NAMESPACE OVERWRITE `table`;",
            "DEFINE NAMESPACE IF NOT EXISTS `table`;",
        ] {
            let entry = split_ok(split_namespace, text);
            assert_eq!(entry.name, "table");
            assert_eq!(
                entry.stmt,
                Stmt::Namespace(Namespace {
                    name: Ident::new("table"),
                    keyword: Carried::default(),
                    comment: None,
                    comments: Vec::new(),
                    clause_comments: Vec::new(),
                })
            );
        }
        // The multi-line comment splits into lines.
        let entry = split_ok(
            split_namespace,
            "DEFINE NAMESPACE ns COMMENT 'line1\\nline2';",
        );
        assert_eq!(
            entry.stmt,
            Stmt::Namespace(Namespace {
                name: Ident::new("ns"),
                keyword: Carried::default(),
                comment: Some(DocComment {
                    lines: vec!["line1".to_string(), "line2".to_string()]
                }),
                comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
    }

    /// spec/namespace.md's fingerprint: the auto-seeded default
    /// namespace carries exactly this comment.
    #[test]
    fn namespace_fingerprint_marks_the_auto_seed() {
        let entry = split_ok(
            split_namespace,
            "DEFINE NAMESPACE main COMMENT 'Default namespace generated by SurrealDB';",
        );
        assert!(entry.markers.auto_seeded_default_namespace);
        let entry = split_ok(split_namespace, "DEFINE NAMESPACE main COMMENT 'other';");
        assert!(!entry.markers.auto_seeded_default_namespace);
    }

    #[test]
    fn splits_database() {
        // The server's own clause order (COMMENT before CHANGEFEED),
        // with the printer's re-decomposed duration (`7d` arrives as
        // `1w` — compared by value, never spelling).
        let entry = split_ok(
            split_database,
            "DEFINE DATABASE blog COMMENT 'Primary application database' CHANGEFEED 1w;",
        );
        let Stmt::Database(db) = entry.stmt else {
            panic!();
        };
        assert_eq!(db.name, Ident::new("blog"));
        assert!(!db.strict);
        let changefeed = db.changefeed.expect("changefeed must parse");
        use surrealdb_types::ToSql as _;
        assert_eq!(changefeed.expiry.to_sql(), "1w");
        assert!(!changefeed.include_original);
        assert_eq!(
            db.comment,
            Some(DocComment {
                lines: vec!["Primary application database".to_string()]
            })
        );
        // INCLUDE ORIGINAL rides the capture.
        let entry = split_ok(
            split_database,
            "DEFINE DATABASE db CHANGEFEED 1h30m INCLUDE ORIGINAL;",
        );
        let Stmt::Database(db) = entry.stmt else {
            panic!();
        };
        let changefeed = db.changefeed.expect("changefeed must parse");
        assert!(changefeed.include_original);
        assert_eq!(changefeed.expiry.to_sql(), "1h30m");
        // STRICT never appears in INFO — spelled input is drift.
        assert!(matches!(
            tripwire_err(split_database, "DEFINE DATABASE db STRICT;"),
            Diag::DdlTripwire { .. }
        ));
    }

    #[test]
    fn splits_sequence() {
        // Defaults materialize — the read result carries them.
        let entry = split_ok(split_sequence, "DEFINE SEQUENCE bare BATCH 1000 START 0;");
        assert_eq!(
            entry.stmt,
            Stmt::Sequence(Sequence {
                name: Ident::new("bare"),
                start: Some(Expr {
                    canon: "0".to_string()
                }),
                batch: Some(Expr {
                    canon: "1000".to_string()
                }),
                timeout: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
        // TIMEOUT NONE stores as the absent clause.
        let entry = split_ok(
            split_sequence,
            "DEFINE SEQUENCE s BATCH 5 START 1 TIMEOUT NONE;",
        );
        let Stmt::Sequence(seq) = entry.stmt else {
            panic!();
        };
        assert!(seq.timeout.is_none());
        // A present timeout rides its text.
        let entry = split_ok(
            split_sequence,
            "DEFINE SEQUENCE s BATCH 5 START 1 TIMEOUT 5s;",
        );
        let Stmt::Sequence(seq) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            seq.timeout,
            Some(Expr {
                canon: "5s".to_string()
            })
        );
        // The always-materialized clauses diagnose when missing.
        assert!(matches!(
            tripwire_err(split_sequence, "DEFINE SEQUENCE s START 0;"),
            Diag::DdlTripwire { .. }
        ));
    }

    #[test]
    fn splits_param() {
        // VALUE NONE is the valueless param; PERMISSIONS FULL the
        // default — both carried as their AST shapes, not elided.
        let entry = split_ok(
            split_param,
            "DEFINE PARAM $bare VALUE NONE PERMISSIONS FULL;",
        );
        assert_eq!(
            entry.stmt,
            Stmt::Param(Param {
                name: ParamName::new("bare"),
                value: None,
                permissions: Some(Permission::Full),
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
        // The full shape: value, comment, WHERE permissions.
        let entry = split_ok(
            split_param,
            "DEFINE PARAM $limit VALUE 50 COMMENT 'Page size' PERMISSIONS WHERE \
             $user.role == 'admin';",
        );
        let Stmt::Param(param) = entry.stmt else {
            panic!();
        };
        assert_eq!(param.name, ParamName::new("limit"));
        assert_eq!(
            param.value,
            Some(Expr {
                canon: "50".to_string()
            })
        );
        assert_eq!(
            param.permissions,
            Some(Permission::Expr(Expr {
                canon: "$user.role == 'admin'".to_string()
            }))
        );
        assert_eq!(
            param.comment,
            Some(DocComment {
                lines: vec!["Page size".to_string()]
            })
        );
        // An object-literal value keeps its braces and spaces.
        let entry = split_ok(
            split_param,
            "DEFINE PARAM $obj VALUE { read: '/api/read' } PERMISSIONS NONE;",
        );
        let Stmt::Param(param) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            param.value,
            Some(Expr {
                canon: "{ read: '/api/read' }".to_string()
            })
        );
        // Missing always-materialized clauses diagnose.
        assert!(matches!(
            tripwire_err(split_param, "DEFINE PARAM $x VALUE NONE;"),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(split_param, "DEFINE PARAM $x PERMISSIONS FULL;"),
            Diag::DdlTripwire { .. }
        ));
    }

    #[test]
    fn splits_bucket() {
        let entry = split_ok(
            split_bucket,
            "DEFINE BUCKET ro READONLY BACKEND 'memory' PERMISSIONS NONE COMMENT 'uploads';",
        );
        assert_eq!(
            entry.stmt,
            Stmt::Bucket(Bucket {
                name: Ident::new("ro"),
                backend: Some(Expr {
                    canon: "'memory'".to_string()
                }),
                readonly: true,
                permissions: Some(Permission::None),
                comment: Some(DocComment {
                    lines: vec!["uploads".to_string()]
                }),
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
        // PERMISSIONS FULL materializes from the absent clause.
        let entry = split_ok(
            split_bucket,
            "DEFINE BUCKET b BACKEND 'memory' PERMISSIONS FULL;",
        );
        let Stmt::Bucket(bucket) = entry.stmt else {
            panic!();
        };
        assert_eq!(bucket.permissions, Some(Permission::Full));
        assert!(!bucket.readonly);
        // A WHERE-permissions expression carrying the word `comment`
        // (idiom access, a record id) is content — only the string
        // shape confirms the COMMENT clause (the pinned
        // reading).
        let entry = split_ok(
            split_bucket,
            "DEFINE BUCKET b BACKEND 'memory' PERMISSIONS WHERE comment == 1 COMMENT 'c';",
        );
        let Stmt::Bucket(bucket) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            bucket.permissions,
            Some(Permission::Expr(Expr {
                canon: "comment == 1".to_string()
            }))
        );
        assert_eq!(
            bucket.comment,
            Some(DocComment {
                lines: vec!["c".to_string()]
            })
        );
        let entry = split_ok(
            split_bucket,
            "DEFINE BUCKET b BACKEND 'memory' PERMISSIONS WHERE x.comment == 1 COMMENT 'c';",
        );
        let Stmt::Bucket(bucket) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            bucket.permissions,
            Some(Permission::Expr(Expr {
                canon: "x.comment == 1".to_string()
            }))
        );
        assert_eq!(
            bucket.comment,
            Some(DocComment {
                lines: vec!["c".to_string()]
            })
        );
        // Glued idiom parts on a stop-word-named operand — an index
        // or a method call — are content, never the clause (the
        // live-printed shapes).
        let entry = split_ok(
            split_bucket,
            "DEFINE BUCKET b BACKEND 'memory' PERMISSIONS WHERE comment[0] = 1 COMMENT 'c';",
        );
        let Stmt::Bucket(bucket) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            bucket.permissions,
            Some(Permission::Expr(Expr {
                canon: "comment[0] = 1".to_string()
            }))
        );
        let entry = split_ok(
            split_bucket,
            "DEFINE BUCKET b BACKEND 'memory' PERMISSIONS WHERE meta.comment(1) = 1 COMMENT \
             'c';",
        );
        let Stmt::Bucket(bucket) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            bucket.permissions,
            Some(Permission::Expr(Expr {
                canon: "meta.comment(1) = 1".to_string()
            }))
        );
        // Missing always-materialized PERMISSIONS diagnoses.
        assert!(matches!(
            tripwire_err(split_bucket, "DEFINE BUCKET b BACKEND 'memory';"),
            Diag::DdlTripwire { .. }
        ));
    }

    /// A COMMENT that is not a plain string literal routes the
    /// statement to verbatim `raw` (the normalization protocol's
    /// form-selection rule).
    #[test]
    fn prefixed_comment_routes_to_raw() {
        let route = split_raw(
            split_namespace,
            "DEFINE NAMESPACE ns COMMENT d'2024-01-01';",
        );
        assert_eq!(route, RawRoute::CommentNotPlainString);
        // Any parseable value that is not a plain string literal rides
        // the same route (README form-selection: the statement goes to
        // verbatim `raw`) — a duration literal here, for one.
        let route = split_raw(split_namespace, "DEFINE NAMESPACE ns COMMENT 5s;");
        assert_eq!(route, RawRoute::CommentNotPlainString);
        // A non-string COMMENT AFTER an expression-valued clause: the
        // Value follow still stops at the digit-led comment value, so
        // the statement routes to `raw` rather than swallowing the
        // comment into the value.
        let route = split_raw(
            split_param,
            "DEFINE PARAM $x VALUE 1 COMMENT 5s PERMISSIONS FULL;",
        );
        assert_eq!(route, RawRoute::CommentNotPlainString);
        let route = split_raw(
            split_bucket,
            "DEFINE BUCKET b BACKEND 'memory' PERMISSIONS WHERE x == 1 COMMENT 5s;",
        );
        assert_eq!(route, RawRoute::CommentNotPlainString);
        // The sign-led and closure value forms (upstream's glued
        // unary minus; the closure's parameter bar) are value
        // openers — non-plain-string comments route to `raw` rather
        // than swallowing.
        let route = split_raw(
            split_param,
            "DEFINE PARAM $x VALUE 1 COMMENT -1 PERMISSIONS FULL;",
        );
        assert_eq!(route, RawRoute::CommentNotPlainString);
        let route = split_raw(
            split_param,
            "DEFINE PARAM $x VALUE 1 COMMENT |$a: any| $a PERMISSIONS FULL;",
        );
        assert_eq!(route, RawRoute::CommentNotPlainString);
    }

    /// The tripwire class: every shape outside the pinned dialect
    /// diagnoses — unknown clauses, duplicates, leftovers, kind and
    /// name mismatches.
    #[test]
    fn dialect_drift_diagnoses() {
        assert!(matches!(
            tripwire_err(split_namespace, "DEFINE NAMESPACE ns BOGUS 1;"),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(
                split_namespace,
                "DEFINE NAMESPACE ns COMMENT 'a' COMMENT 'b';"
            ),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(split_sequence, "DEFINE SEQUENCE s BATCH 1 START 0 junk;"),
            Diag::DdlTripwire { .. }
        ));
        // Kind word vs section.
        assert!(matches!(
            tripwire_err(split_namespace, "DEFINE TABLE x;"),
            Diag::InfoUnknownShape { .. }
        ));
        // The statement's case-insensitive keywords accept hand-fed
        // lowercase — the dialect's words, not its casing, are pinned.
        let entry = split_ok(split_namespace, "define namespace lower comment 'x';");
        assert_eq!(entry.name, "lower");
    }

    // -- the medium splitters -------------------------------

    /// The capture-grammar split's regression (the
    /// stop-word corner): a param VALUE of `comment:1` — a record id
    /// over a table named `comment` — carries the stop word as
    /// expression content, because no string literal confirms the
    /// COMMENT clause there.
    #[test]
    fn param_value_carries_stop_word_content() {
        let entry = split_ok(
            split_param,
            "DEFINE PARAM $x VALUE comment:1 PERMISSIONS FULL;",
        );
        let Stmt::Param(param) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            param.value,
            Some(Expr {
                canon: "comment:1".to_string()
            })
        );
    }

    #[test]
    fn splits_analyzer() {
        // The server's canonical order (FUNCTION → TOKENIZERS →
        // FILTERS → COMMENT), the printer's comma-without-space
        // tokenizer list, and the UPPERCASE filter atoms.
        let entry = split_ok(
            split_analyzer,
            "DEFINE ANALYZER full FUNCTION fn::strip TOKENIZERS CLASS,PUNCT FILTERS LOWERCASE, \
             EDGENGRAM(2,10), SNOWBALL(ENGLISH) COMMENT 'full';",
        );
        let Stmt::Analyzer(analyzer) = entry.stmt else {
            panic!();
        };
        assert_eq!(analyzer.name, Ident::new("full"));
        assert_eq!(
            analyzer.function,
            Some(FnPath {
                kind: FnKind::Fn,
                segments: vec!["strip".to_string()],
            })
        );
        assert_eq!(
            analyzer.tokenizers,
            Some(vec![Tokenizer::Class, Tokenizer::Punct])
        );
        assert_eq!(
            analyzer.filters,
            Some(vec![
                Filter::Lowercase,
                Filter::EdgeNgram(2, 10),
                Filter::Snowball(Authored::canonical(Language::English)),
            ])
        );
        assert_eq!(
            analyzer.comment,
            Some(DocComment {
                lines: vec!["full".to_string()]
            })
        );
        // The bare shape (probe-pinned: nothing materializes).
        let entry = split_ok(split_analyzer, "DEFINE ANALYZER empty;");
        assert_eq!(
            entry.stmt,
            Stmt::Analyzer(Analyzer {
                name: Ident::new("empty"),
                function: None,
                tokenizers: None,
                filters: None,
                comment: None,
                comments: Vec::new(),
                trailing_comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
        // The remaining filter atoms — mapper's quoted path survives
        // the name folding; ngram carries its pair.
        let entry = split_ok(
            split_analyzer,
            "DEFINE ANALYZER kinds TOKENIZERS BLANK,CAMEL,CLASS,PUNCT FILTERS ASCII, UPPERCASE, \
             NGRAM(1,4), MAPPER('m.rs');",
        );
        let Stmt::Analyzer(analyzer) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            analyzer.tokenizers,
            Some(vec![
                Tokenizer::Blank,
                Tokenizer::Camel,
                Tokenizer::Class,
                Tokenizer::Punct,
            ])
        );
        assert_eq!(
            analyzer.filters,
            Some(vec![
                Filter::Ascii,
                Filter::Uppercase,
                Filter::Ngram(1, 4),
                Filter::Mapper("m.rs".to_string()),
            ])
        );
        // Multi-segment fn paths and quoted segments ride the shared
        // segment reader.
        let entry = split_ok(
            split_analyzer,
            "DEFINE ANALYZER quoted FUNCTION fn::a::`table`;",
        );
        let Stmt::Analyzer(analyzer) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            analyzer.function,
            Some(FnPath {
                kind: FnKind::Fn,
                segments: vec!["a".to_string(), "table".to_string()],
            })
        );
        // A word inside a filter's string argument is content, never
        // the COMMENT clause.
        let entry = split_ok(
            split_analyzer,
            "DEFINE ANALYZER mapped FILTERS MAPPER('with comment inside');",
        );
        let Stmt::Analyzer(analyzer) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            analyzer.filters,
            Some(vec![Filter::Mapper("with comment inside".to_string())])
        );
    }

    #[test]
    fn splits_user() {
        // The full shape: hash + roles + both durations + comment
        // (probe-pinned order, both durations materialize).
        let entry = split_ok(
            split_user,
            "DEFINE USER root_u ON ROOT PASSHASH 'h' ROLES OWNER, EDITOR DURATION FOR TOKEN 2h, \
             FOR SESSION 30m COMMENT 'root one';",
        );
        let Stmt::User(user) = entry.stmt else {
            panic!();
        };
        assert_eq!(user.name, Ident::new("root_u"));
        assert_eq!(user.on, Authored::canonical(Level::Root));
        assert_eq!(user.pass, Some(Pass::Hash("h".to_string())));
        assert_eq!(user.roles, vec![Role::Owner, Role::Editor]);
        assert_eq!(
            user.token_duration,
            Some(Expr {
                canon: "2h".to_string()
            })
        );
        assert_eq!(
            user.session_duration,
            Some(Expr {
                canon: "30m".to_string()
            })
        );
        assert_eq!(
            user.comment,
            Some(DocComment {
                lines: vec!["root one".to_string()]
            })
        );
        assert_eq!(entry.markers, Markers::default());
        // The materialized defaults ride the read result (ROLES
        // VIEWER, token 1h / session NONE).
        let entry = split_ok(
            split_user,
            "DEFINE USER plain_u ON DATABASE PASSHASH 'x' ROLES VIEWER DURATION FOR TOKEN 1h, \
             FOR SESSION NONE;",
        );
        let Stmt::User(user) = entry.stmt else {
            panic!();
        };
        assert_eq!(user.roles, vec![Role::Viewer]);
        assert_eq!(
            user.token_duration,
            Some(Expr {
                canon: "1h".to_string()
            })
        );
        assert_eq!(
            user.session_duration,
            Some(Expr {
                canon: "NONE".to_string()
            })
        );
    }

    /// The 3.3.0 `PASSSCRAM '[REDACTED]'` clause (probe-pinned live,
    /// 2026-09-25) strips before validation: the sentinel spelling is
    /// define-rejected upstream, so leaving it in the text would fail
    /// the canonicalizing parse. The exact-splice contract: neighbors
    /// close up, absence borrows, and any other PASSSCRAM spelling
    /// survives to fail validation loud.
    #[test]
    fn passscram_clause_strips_before_validation() {
        let ddl = "DEFINE USER pw ON DATABASE PASSHASH '[REDACTED]' PASSSCRAM '[REDACTED]' \
                   ROLES VIEWER;";
        assert_eq!(
            strip_passscram(ddl),
            "DEFINE USER pw ON DATABASE PASSHASH '[REDACTED]' ROLES VIEWER;"
        );
        // Absence borrows.
        let clean = "DEFINE USER h ON DATABASE PASSHASH 'h' ROLES VIEWER;";
        assert!(matches!(strip_passscram(clean), Cow::Borrowed(_)));
        // A non-sentinel PASSSCRAM is printer drift: untouched, loud
        // downstream.
        let drifted = "DEFINE USER pw ON DATABASE PASSHASH '[REDACTED]' PASSSCRAM 'scram$256:xyz' \
                       ROLES VIEWER;";
        assert_eq!(strip_passscram(drifted), drifted);
    }

    /// The 3.3 shapes the access reader adopts: the live
    /// INFO prints — CONTEXT after AUTHENTICATE, before DURATION
    /// (the printer's own re-parse note); AUDIENCE positionally
    /// between the verification source and the issuer, comma-space
    /// separated — plus the hand-fed shapes the engine's
    /// define-time checks never store (CONTEXT on a jwt access, on a
    /// bearer for users), which trip loud. (The fail-closed pin's
    /// `LIGHTWEIGHT` and `INLINE` arms are retired.)
    #[test]
    fn splits_access_context_and_audience() {
        // The record + CONTEXT live print (the auto-issue jwt
        // signature rides before AUTHENTICATE — the mirror fold reads
        // through it).
        let entry = split_ok(
            split_access,
            "DEFINE ACCESS rec ON DATABASE TYPE RECORD SIGNUP (CREATE user SET email = \
             $email) WITH JWT ALGORITHM HS512 KEY '[REDACTED]' WITH ISSUER KEY '[REDACTED]' \
             AUTHENTICATE (SELECT * FROM user) CONTEXT { extra: 1, tier: 'gold' } \
             DURATION FOR TOKEN 1h, FOR SESSION NONE;",
        );
        let Stmt::Access(access) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            access.context.expect("the context adopts").canon,
            "{ extra: 1, tier: 'gold' }"
        );
        // AUDIENCE between the source and the issuer, comma-space
        // separated; the values adopt decoded.
        let entry = split_ok(
            split_access,
            "DEFINE ACCESS a ON DATABASE TYPE JWT URL 'https://x/jwks' AUDIENCE 'aud1', \
             'aud2' DURATION FOR SESSION NONE;",
        );
        let Stmt::Access(access) = entry.stmt else {
            panic!();
        };
        let AccessKind::Jwt { source } = access.kind else {
            panic!();
        };
        assert_eq!(
            source.audience.expect("the audience adopts"),
            vec!["aud1".to_string(), "aud2".to_string()]
        );
        // The peel rides the KEY source too (the mirror fold reads
        // through the hs* print).
        let entry = split_ok(
            split_access,
            "DEFINE ACCESS a ON DATABASE TYPE JWT ALGORITHM HS256 KEY '[REDACTED]' \
             AUDIENCE 'a' WITH ISSUER KEY '[REDACTED]' DURATION FOR SESSION NONE;",
        );
        let Stmt::Access(access) = entry.stmt else {
            panic!();
        };
        let AccessKind::Jwt { source } = access.kind else {
            panic!();
        };
        assert_eq!(
            source.audience.as_deref(),
            Some(["a".to_string()].as_slice())
        );
        // Hand-fed shapes the engine's define-time check never
        // stores: CONTEXT on a jwt access, on a bearer for users.
        for ddl in [
            "DEFINE ACCESS a ON DATABASE TYPE JWT URL 'https://x/jwks' CONTEXT { a: 1 } \
             DURATION FOR SESSION NONE;",
            "DEFINE ACCESS a ON DATABASE TYPE BEARER FOR USER WITH JWT \
             URL 'https://x/jwks' CONTEXT { a: 1 } DURATION FOR SESSION NONE;",
        ] {
            assert!(split_access(ddl).is_err(), "should trip: {ddl}");
        }
    }

    /// The 3.3 INLINE field the reader adopts: the live INFO
    /// prints — the flag between READONLY and VALUE (the printer's
    /// fixed order, probe-pinned with a DEFAULT-riding field) — plus
    /// the hand-fed shapes the engine's define-time checks never
    /// store, which trip the shape validation loud (canonicalize
    /// first, the read path's own order — the upstream clause loop
    /// accepts INLINE anywhere, so the malformed spellings re-print
    /// into the printer order and the fold-site mirrors catch them).
    #[test]
    fn splits_field_inline() {
        // The live print for the SDL-authored spelling.
        let entry = split_ok(
            split_field,
            "DEFINE FIELD score ON likes TYPE number INLINE PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert!(field.inline);
        // The printer's fixed relative order with every rider.
        let entry = split_ok(
            split_field,
            "DEFINE FIELD score ON likes TYPE number DEFAULT 1 READONLY INLINE \
             PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert!(field.inline && field.readonly);
        assert!(matches!(field.default, Some(FieldDefault::Set(_))));
        // Hand-fed shapes the engine's define-time checks never store:
        // computed, the adjacency-key names, dotted paths — loud at
        // the shape validation (after canonicalize, the read path's
        // own order).
        for ddl in [
            "DEFINE FIELD c ON likes TYPE number COMPUTED 1 + 1 INLINE PERMISSIONS FULL;",
            "DEFINE FIELD in ON likes TYPE number INLINE PERMISSIONS FULL;",
            "DEFINE FIELD a.b ON likes TYPE number INLINE PERMISSIONS FULL;",
            "DEFINE FIELD id ON likes TYPE string INLINE PERMISSIONS FULL;",
        ] {
            let canonical = canonicalize_statement(ddl).expect("parse-legal per parity");
            assert!(split_field(&canonical).is_err(), "should trip: {canonical}");
        }
    }

    /// The 3.3 shapes the table reader adopts: the live
    /// INFO prints — `ENFORCED LIGHTWEIGHT` after the sides (the
    /// engine's define normalization, probe-pinned), the caps after
    /// COMMENT — plus the parse-fold (`LIGHTWEIGHT` without ENFORCED
    /// reads enforced), the kind guard, and the view's deviant route.
    #[test]
    fn splits_lightweight_and_caps() {
        // The live print: authored without ENFORCED, INFO shows both
        // words; the AST folds the implication.
        let entry = split_ok(
            split_table,
            "DEFINE TABLE likes TYPE RELATION IN person OUT post ENFORCED LIGHTWEIGHT \
             SCHEMALESS PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            table.kind,
            TableKind::Relation {
                from: Some(vec![Ident::new("person")]),
                to: Some(vec![Ident::new("post")]),
                enforced: true,
                lightweight: true,
            }
        );
        // LIGHTWEIGHT alone (hand-fed shape): the same fold.
        let entry = split_ok(
            split_table,
            "DEFINE TABLE likes TYPE RELATION IN person OUT post LIGHTWEIGHT SCHEMALESS \
             PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert!(matches!(
            table.kind,
            TableKind::Relation {
                enforced: true,
                lightweight: true,
                ..
            }
        ));
        // An ENDPOINT named `lightweight` reads as an endpoint — the
        // word rides the payload (it is NOT a capture stop) and the
        // flag peel leaves it in list position (the live print for the
        // SDL-authored spelling; the printer emits keyword idents
        // bare).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE likes TYPE RELATION IN person OUT lightweight SCHEMALESS \
             PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            table.kind,
            TableKind::Relation {
                from: Some(vec![Ident::new("person")]),
                to: Some(vec![Ident::new("lightweight")]),
                enforced: false,
                lightweight: false,
            }
        );
        // List continuation: `a | lightweight` is two endpoints — and
        // the UPPERCASE spelling too (the flag's own spelling; the
        // pipe check reads the raw prefix because scanned_words
        // records bare words only).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE t TYPE RELATION IN a | LIGHTWEIGHT OUT post | LIGHTWEIGHT \
             SCHEMALESS PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            table.kind,
            TableKind::Relation {
                from: Some(vec![Ident::new("a"), Ident::new("LIGHTWEIGHT")]),
                to: Some(vec![Ident::new("post"), Ident::new("LIGHTWEIGHT")]),
                enforced: false,
                lightweight: false,
            }
        );
        // A flag AFTER a complete list inside the payload (a hand-fed
        // order the engine's grammar accepts) peels: endpoint `a` plus
        // the flag (which implies `enforced`).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE t TYPE RELATION IN a LIGHTWEIGHT OUT b SCHEMALESS PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert!(matches!(
            table.kind,
            TableKind::Relation {
                from: Some(..),
                to: Some(..),
                enforced: true,
                lightweight: true,
            }
        ));
        // Head + trailing: an endpoint named `lightweight` AND the
        // flag on the same side.
        let entry = split_ok(
            split_table,
            "DEFINE TABLE t TYPE RELATION IN a OUT lightweight LIGHTWEIGHT SCHEMALESS \
             PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            table.kind,
            TableKind::Relation {
                from: Some(vec![Ident::new("a")]),
                to: Some(vec![Ident::new("lightweight")]),
                enforced: true,
                lightweight: true,
            }
        );
        // The peel takes the LAST depth-0 word (`rev().find`) — a
        // forward first-find would peel mid-list and silently drop
        // the `| b` tail. The tail must die loud instead (the
        // invariant's pin).
        assert!(
            split_table(
                "DEFINE TABLE t TYPE RELATION IN a LIGHTWEIGHT | b OUT c SCHEMALESS \
                 PERMISSIONS NONE;"
            )
            .is_err()
        );
        // The caps ride through, in either position the printer
        // spells them, with the kind-independent shape (NORMAL host).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE t TYPE NORMAL SCHEMAFULL COMMENT 'c' INLINE EDGES 64 \
             INLINE REFERENCES 16 CHANGEFEED 4w2d PERMISSIONS FULL;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(table.inline_edges, Some(64));
        assert_eq!(table.inline_references, Some(16));
        assert_eq!(table.kind, TableKind::Normal);
        // Cap 0 is a real value (the printer materializes it).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE t0 TYPE ANY SCHEMALESS INLINE EDGES 0 PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(table.inline_edges, Some(0));
        assert_eq!(table.inline_references, None);
        // LIGHTWEIGHT on a NORMAL table is not the printer's shape.
        assert!(
            split_table("DEFINE TABLE t TYPE NORMAL LIGHTWEIGHT SCHEMALESS PERMISSIONS NONE;")
                .is_err()
        );
        // A sides-less LIGHTWEIGHT is the engine's own define
        // rejection mirrored — the `(None, None)` writer spelling
        // could not carry the word, so reading one would silently
        // drop the flag on fmt (hand-fed only; live INFO cannot
        // carry the shape).
        assert!(
            split_table("DEFINE TABLE t TYPE RELATION LIGHTWEIGHT SCHEMALESS PERMISSIONS NONE;")
                .is_err()
        );
        assert!(
            split_table(
                "DEFINE TABLE t TYPE RELATION IN person LIGHTWEIGHT SCHEMALESS PERMISSIONS NONE;"
            )
            .is_err()
        );
        // A cap-bearing view deviates (SDL does not author the caps
        // on views — the fixed emission could not carry them).
        assert_eq!(
            split_raw(
                split_table,
                "DEFINE TABLE v TYPE ANY SCHEMALESS INLINE EDGES 4 AS SELECT * FROM t \
                 PERMISSIONS NONE;",
            ),
            RawRoute::TableViewDeviant
        );
        // A cap the engine would reject at define never reads.
        assert!(
            split_table("DEFINE TABLE t TYPE NORMAL SCHEMALESS INLINE EDGES 257 PERMISSIONS NONE;")
                .is_err()
        );
        // A non-integer cap is not the printer's shape.
        assert!(
            split_table("DEFINE TABLE t TYPE NORMAL SCHEMALESS INLINE EDGES big PERMISSIONS NONE;")
                .is_err()
        );
        // INLINE without EDGES/REFERENCES is not either.
        assert!(
            split_table("DEFINE TABLE t TYPE NORMAL SCHEMALESS INLINE 4 PERMISSIONS NONE;")
                .is_err()
        );
    }

    /// The secret sentinels never adopt as literals: `[REDACTED]`
    /// feeds `markers.redacted`, `PASSHASH ''` feeds
    /// `markers.secretless_passhash` — `pass` stays `None` (the
    /// preserve decision is the normalizer's; the differ treats
    /// secrets as opaque).
    #[test]
    fn user_secret_sentinels_feed_markers() {
        let entry = split_ok(
            split_user,
            "DEFINE USER pw_u ON NAMESPACE PASSHASH '[REDACTED]' ROLES VIEWER DURATION FOR \
             TOKEN 1h, FOR SESSION NONE;",
        );
        let Stmt::User(user) = entry.stmt else {
            panic!();
        };
        assert_eq!(user.pass, None);
        assert!(entry.markers.redacted);
        assert!(!entry.markers.secretless_passhash);
        let entry = split_ok(
            split_user,
            "DEFINE USER bare_u ON ROOT PASSHASH '' ROLES VIEWER DURATION FOR TOKEN 1h, FOR \
             SESSION NONE;",
        );
        let Stmt::User(user) = entry.stmt else {
            panic!();
        };
        assert_eq!(user.pass, None);
        assert!(entry.markers.secretless_passhash);
        assert!(!entry.markers.redacted);
    }

    #[test]
    fn splits_event() {
        // The full shape (probe-pinned order ASYNC → WHEN → THEN →
        // COMMENT); the THEN items keep the printer's statement texts.
        let entry = split_ok(
            split_event,
            "DEFINE EVENT e3 ON ev_src ASYNC RETRY 5 MAXDEPTH 8 WHEN true THEN (CREATE l3 SET \
             a = 1) COMMENT 'note';",
        );
        let Stmt::Event(event) = entry.stmt else {
            panic!();
        };
        assert_eq!(event.name, Ident::new("e3"));
        assert_eq!(event.on, Ident::new("ev_src"));
        assert_eq!(
            event.r#async,
            Some(Async {
                retry: Some(5),
                maxdepth: Some(8),
            })
        );
        assert_eq!(
            event.when,
            Some(Expr {
                canon: "true".to_string()
            })
        );
        assert_eq!(
            event.then,
            vec![Expr {
                canon: "(CREATE l3 SET a = 1)".to_string()
            }]
        );
        assert_eq!(
            event.comment,
            Some(DocComment {
                lines: vec!["note".to_string()]
            })
        );
        // The materialized defaults (WHEN true; RETRY 1 MAXDEPTH 3
        // for bare async) and the multi-THEN comma split at depth 0.
        let entry = split_ok(
            split_event,
            "DEFINE EVENT e2 ON ev_src WHEN true THEN (CREATE l2 SET x = 1), (UPDATE stats SET \
             n += 1);",
        );
        let Stmt::Event(event) = entry.stmt else {
            panic!();
        };
        assert_eq!(event.r#async, None);
        assert_eq!(event.then.len(), 2);
        // The braced THEN form rides verbatim.
        let entry = split_ok(
            split_event,
            "DEFINE EVENT braced ON t2 WHEN $event = 'UPDATE' THEN { CREATE b SET x = 1 };",
        );
        let Stmt::Event(event) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            event.then,
            vec![Expr {
                canon: "{ CREATE b SET x = 1 }".to_string()
            }]
        );
        // The always-materialized clauses diagnose when missing.
        assert!(matches!(
            tripwire_err(split_event, "DEFINE EVENT e ON t THEN (CREATE x);"),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(
                split_event,
                "DEFINE EVENT e ON t WHEN true THEN (CREATE x) ASYNC;"
            ),
            Diag::DdlTripwire { .. }
        ));
        // RETRY without ASYNC is drift (SurrealQL prints them
        // together).
        assert!(matches!(
            tripwire_err(
                split_event,
                "DEFINE EVENT e ON t RETRY 1 WHEN true THEN (CREATE x);"
            ),
            Diag::DdlTripwire { .. }
        ));
        // A function CALL spelled `then(` inside the WHEN expression
        // is content, never the THEN clause (the path-segment
        // predecessor guard, the pinned reading).
        let entry = split_ok(
            split_event,
            "DEFINE EVENT e ON t WHEN fn::then(1) = 1 THEN (CREATE x);",
        );
        let Stmt::Event(event) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            event.when,
            Some(Expr {
                canon: "fn::then(1) = 1".to_string()
            })
        );
        assert_eq!(event.then.len(), 1);
        // A glued method call on `then` is likewise content (`(`
        // glued to the word is a call, never the
        // spaced then-item).
        let entry = split_ok(
            split_event,
            "DEFINE EVENT e ON t WHEN meta.then(1) = 1 THEN (CREATE x);",
        );
        let Stmt::Event(event) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            event.when,
            Some(Expr {
                canon: "meta.then(1) = 1".to_string()
            })
        );
        assert_eq!(event.then.len(), 1);
    }

    #[test]
    fn splits_function() {
        // The full head: params (kinds with inner commas stay whole),
        // the `->` return, the body with braces included, then the
        // server's trailing order COMMENT → GRAPHQL_* → PERMISSIONS.
        let entry = split_ok(
            split_function,
            "DEFINE FUNCTION fn::one($a: int, $b: array<float, 10>) -> bool { RETURN true } \
             COMMENT 'c' PERMISSIONS WHERE $auth.admin = true;",
        );
        let Stmt::Function(function) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            function.name,
            FnPath {
                kind: FnKind::Fn,
                segments: vec!["one".to_string()],
            }
        );
        assert_eq!(
            function.args,
            vec![
                (ParamName::new("a"), Kind::new("int")),
                (ParamName::new("b"), Kind::new("array<float, 10>")),
            ]
        );
        assert_eq!(function.returns, Some(Kind::new("bool")));
        assert_eq!(
            function.body,
            Expr {
                canon: "{ RETURN true }".to_string()
            }
        );
        assert_eq!(
            function.permissions,
            Some(Permission::Expr(Expr {
                canon: "$auth.admin = true".to_string()
            }))
        );
        assert_eq!(
            function.comment,
            Some(DocComment {
                lines: vec!["c".to_string()]
            })
        );
        assert_eq!(function.graphql, None);
        // The server's option normalization rides verbatim (`none |
        // int` — same_definition compares parsed kind ASTs); the
        // GRAPHQL_* clauses fold into the sub-block; PERMISSIONS
        // FULL materializes; the multi-statement body keeps its `;`s;
        // the empty body canonizes to `{;}`.
        let entry = split_ok(
            split_function,
            "DEFINE FUNCTION fn::dep($x: none | int) { RETURN $x } GRAPHQL_DEPRECATED 'old' \
             PERMISSIONS FULL;",
        );
        let Stmt::Function(function) = entry.stmt else {
            panic!();
        };
        assert_eq!(function.args[0].1, Kind::new("none | int"));
        assert_eq!(
            function.graphql,
            Some(Graphql {
                alias: None,
                deprecated: Some("old".to_string()),
            })
        );
        assert_eq!(function.permissions, Some(Permission::Full));
        let entry = split_ok(
            split_function,
            "DEFINE FUNCTION fn::multi($x: float) { LET $y = 1; RETURN $y; } GRAPHQL_ALIAS \
             'nick' PERMISSIONS NONE;",
        );
        let Stmt::Function(function) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            function.body,
            Expr {
                canon: "{ LET $y = 1; RETURN $y; }".to_string()
            }
        );
        assert_eq!(
            function.graphql,
            Some(Graphql {
                alias: Some("nick".to_string()),
                deprecated: None,
            })
        );
        let entry = split_ok(
            split_function,
            "DEFINE FUNCTION fn::empty() {;} PERMISSIONS FULL;",
        );
        let Stmt::Function(function) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            function.body,
            Expr {
                canon: "{;}".to_string()
            }
        );
        // The INFO key is the path without `fn::` — multi-segment
        // paths join with `::` (probe-pinned).
        let entry = split_ok(
            split_function,
            "DEFINE FUNCTION fn::a::b() { RETURN 1 } PERMISSIONS FULL;",
        );
        assert_eq!(entry.name, "a::b");
        // Missing always-materialized PERMISSIONS diagnoses.
        assert!(matches!(
            tripwire_err(
                split_function,
                "DEFINE FUNCTION fn::x() { RETURN 1 } COMMENT 'c';"
            ),
            Diag::DdlTripwire { .. }
        ));
    }

    #[test]
    fn splits_config() {
        // The api fragment: middleware entries with computed values,
        // PERMISSIONS always.
        let entry = split_ok(
            split_config,
            "DEFINE CONFIG OVERWRITE API MIDDLEWARE api::timeout(5s), api::log() PERMISSIONS \
             NONE;",
        );
        assert_eq!(entry.name, "api");
        let Stmt::Config(Config::Api(api)) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            api.middleware,
            vec![
                Middleware {
                    path: FnPath {
                        kind: FnKind::Bare,
                        segments: vec!["api".to_string(), "timeout".to_string()],
                    },
                    args: vec![Expr {
                        canon: "5s".to_string()
                    }],
                },
                Middleware {
                    path: FnPath {
                        kind: FnKind::Bare,
                        segments: vec!["api".to_string(), "log".to_string()],
                    },
                    args: vec![],
                },
            ]
        );
        assert_eq!(api.permissions, Some(Permission::None));
        // The minimal api fragment (middleware omitted).
        let entry = split_ok(
            split_config,
            "DEFINE CONFIG OVERWRITE API PERMISSIONS FULL;",
        );
        let Stmt::Config(Config::Api(api)) = entry.stmt else {
            panic!();
        };
        assert!(api.middleware.is_empty());
        assert_eq!(api.permissions, Some(Permission::Full));
        // The graphql fragment: families (the include list keeps the
        // fn-path's structure), DEPTH/COMPLEXITY, INTROSPECTION NONE.
        let entry = split_ok(
            split_config,
            "DEFINE CONFIG OVERWRITE GRAPHQL TABLES INCLUDE t1, t2 FUNCTIONS EXCLUDE \
             fn::legacy DEPTH 4 COMPLEXITY 100 INTROSPECTION NONE;",
        );
        assert_eq!(entry.name, "graphql");
        let Stmt::Config(Config::Graphql(graphql)) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            graphql.tables,
            Family::Include(vec![Ident::new("t1"), Ident::new("t2")])
        );
        assert_eq!(
            graphql.functions,
            Family::Exclude(vec![FnPath {
                kind: FnKind::Fn,
                segments: vec!["legacy".to_string()],
            }])
        );
        assert_eq!(graphql.depth, Some(4));
        assert_eq!(graphql.complexity, Some(100));
        assert_eq!(graphql.introspection, Introspection::None);
        // `AUTO` materializes as the resolved per-family spellings;
        // unset families ride NONE.
        let entry = split_ok(
            split_config,
            "DEFINE CONFIG OVERWRITE GRAPHQL TABLES AUTO FUNCTIONS AUTO;",
        );
        let Stmt::Config(Config::Graphql(graphql)) = entry.stmt else {
            panic!();
        };
        assert_eq!(graphql.tables, Family::Auto);
        assert_eq!(graphql.functions, Family::Auto);
        assert_eq!(graphql.introspection, Introspection::Auto);
        let entry = split_ok(
            split_config,
            "DEFINE CONFIG OVERWRITE GRAPHQL TABLES NONE FUNCTIONS NONE INTROSPECTION NONE;",
        );
        let Stmt::Config(Config::Graphql(graphql)) = entry.stmt else {
            panic!();
        };
        assert_eq!(graphql.tables, Family::None);
        assert_eq!(graphql.functions, Family::None);
        // The default fragment grammar: the set sides only (bare
        // idents per the spec).
        let entry = split_ok(
            split_config,
            "DEFINE CONFIG OVERWRITE DEFAULT NAMESPACE acme;",
        );
        assert_eq!(entry.name, "default");
        let Stmt::Config(Config::Default(config)) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            config.namespace,
            Some(Expr {
                // The bare fragment ident quotes to the canon the
                // structured value and SDL's authored string carry.
                canon: "'acme'".to_string()
            })
        );
        assert_eq!(config.database, None);
        // The NONE sentinel is the emitter's unset-side spelling —
        // read as absence; a backticked ident unescapes before the
        // quoting, which route through the one quoting home
        // (quote-switching included — an escape-needing name carries
        // the same canon the structured path builds).
        let entry = split_ok(
            split_config,
            "DEFINE CONFIG OVERWRITE DEFAULT NAMESPACE NONE DATABASE `we ird`;",
        );
        let Stmt::Config(Config::Default(config)) = entry.stmt else {
            panic!();
        };
        assert_eq!(config.namespace, None);
        assert_eq!(
            config.database,
            Some(Expr {
                canon: "'we ird'".to_string()
            })
        );
        let entry = split_ok(
            split_config,
            "DEFINE CONFIG OVERWRITE DEFAULT NAMESPACE `it's` DATABASE NONE;",
        );
        let Stmt::Config(Config::Default(config)) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            config.namespace,
            Some(Expr {
                canon: "\"it's\"".to_string()
            })
        );
        assert_eq!(config.database, None);
        // The always-materialized shapes diagnose when missing.
        assert!(matches!(
            tripwire_err(
                split_config,
                "DEFINE CONFIG OVERWRITE API MIDDLEWARE api::log();"
            ),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(split_config, "DEFINE CONFIG OVERWRITE GRAPHQL TABLES AUTO;"),
            Diag::DdlTripwire { .. }
        ));
    }

    #[test]
    fn splits_module() {
        // The 3.3 emission grammar: named file module, UNSIGNED,
        // PERMISSIONS, COMMENT.
        let entry = split_ok(
            split_module,
            "DEFINE MODULE mod::color FROM f\"modules:/color.surli\" UNSIGNED PERMISSIONS NONE \
             COMMENT 'Colour helpers';",
        );
        assert_eq!(entry.name, "color");
        let Stmt::Module(module) = entry.stmt else {
            panic!();
        };
        assert_eq!(module.name, Some(Ident::new("color")));
        assert_eq!(
            module.executable,
            Executable::File {
                bucket: "modules".to_string(),
                key: "/color.surli".to_string(),
            }
        );
        assert!(module.unsigned);
        assert_eq!(module.permissions, Some(Permission::None));
        assert_eq!(
            module.comment,
            Some(DocComment {
                lines: vec!["Colour helpers".to_string()]
            })
        );
        // The 3.2 AS form: no UNSIGNED (implicitly unsigned —
        // spec/module.md's reverse section), PERMISSIONS always
        // appended.
        let entry = split_ok(
            split_module,
            "DEFINE MODULE mod::color AS f\"modules:/color.surli\" PERMISSIONS FULL;",
        );
        let Stmt::Module(module) = entry.stmt else {
            panic!();
        };
        assert!(module.unsigned);
        assert_eq!(module.permissions, Some(Permission::Full));
        // The silo executables: 3.3's `::<1.2.0>` and 3.2's
        // lexer-broken `<1.2.0>` (digits parse tolerantly); a quoted
        // org segment unescapes. The anonymous-silo cross-check name
        // is the self-name `org::pkg` (a post-registration capture's
        // INFO carries `mod::<name>` storage keys; split_outcome
        // pairs the prefix and records the bare name).
        let entry = split_ok(
            split_module,
            "DEFINE MODULE silo::acme::utils::<1.2.0> UNSIGNED;",
        );
        assert_eq!(entry.name, "acme::utils");
        let Stmt::Module(module) = entry.stmt else {
            panic!();
        };
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
        let entry = split_ok(
            split_module,
            "DEFINE MODULE silo::`9lives`::pkg<0.1.2> PERMISSIONS FULL;",
        );
        assert_eq!(entry.name, "9lives::pkg");
        let Stmt::Module(module) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            module.executable,
            Executable::Silo {
                org: "9lives".to_string(),
                pkg: "pkg".to_string(),
                major: 0,
                minor: 1,
                patch: 2,
            }
        );
        // The name+silo round-trip asymmetry routes to verbatim raw
        // (the one licensed module shape).
        let route = split_raw(
            split_module,
            "DEFINE MODULE mod::x AS silo::acme::pkg<1.0.0> PERMISSIONS FULL;",
        );
        assert_eq!(route, RawRoute::ModuleNamedSilo);
        let route = split_raw(
            split_module,
            "DEFINE MODULE mod::x FROM silo::acme::pkg::<1.0.0> UNSIGNED;",
        );
        assert_eq!(route, RawRoute::ModuleNamedSilo);
        // An anonymous file module cannot be stored — the catalog
        // cannot carry one, so it diagnoses.
        assert!(matches!(
            tripwire_err(split_module, "DEFINE MODULE f\"b:/k\";"),
            Diag::DdlTripwire { .. }
        ));
        // A FROM-form module without UNSIGNED is drift; so is its
        // inverse — an AS form carrying UNSIGNED (outside both pinned
        // dialects).
        assert!(matches!(
            tripwire_err(split_module, "DEFINE MODULE mod::x FROM f\"b:/k\";"),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(
                split_module,
                "DEFINE MODULE mod::x AS f\"b:/k\" UNSIGNED PERMISSIONS FULL;"
            ),
            Diag::DdlTripwire { .. }
        ));
        // The file string must be a `bucket:/key` path — no second
        // colon, a `/`-led key (the forward grammar's guarantees).
        assert!(matches!(
            tripwire_err(
                split_module,
                "DEFINE MODULE mod::x AS f\"b:k\" PERMISSIONS FULL;"
            ),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(
                split_module,
                "DEFINE MODULE mod::x AS f\"b:/k:extra\" PERMISSIONS FULL;"
            ),
            Diag::DdlTripwire { .. }
        ));
        // …and the forward lexer's charsets (spaces are not bucket/key
        // characters — the written SDL must re-parse).
        assert!(matches!(
            tripwire_err(
                split_module,
                "DEFINE MODULE mod::x AS f\"we ird:/k k\" PERMISSIONS FULL;"
            ),
            Diag::DdlTripwire { .. }
        ));
    }

    /// The ROOT default config's structured value (spec/config.md):
    /// the set sides only, `{}` reads as none, unknown keys diagnose.
    #[test]
    fn reads_the_default_config_structured_value() {
        let info = read_info_object("{ defaults: { database: 'blog', namespace: 'acme' } }")
            .expect("walks");
        let entry = read_default_config(&info)
            .expect("reads")
            .expect("the default config is set");
        assert_eq!(entry.name, "default");
        assert_eq!(entry.markers, Markers::default());
        let Stmt::Config(Config::Default(config)) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            config.namespace,
            Some(Expr {
                canon: "'acme'".to_string()
            })
        );
        assert_eq!(
            config.database,
            Some(Expr {
                canon: "'blog'".to_string()
            })
        );
        // Empty or absent → none defined.
        let info = read_info_object("{ defaults: {  } }").expect("walks");
        assert!(read_default_config(&info).expect("reads").is_none());
        let info = read_info_object("{ namespaces: {  } }").expect("walks");
        assert!(read_default_config(&info).expect("reads").is_none());
        // Unknown keys diagnose.
        let info = read_info_object("{ defaults: { bogus: 'x' } }").expect("walks");
        assert!(matches!(
            read_default_config(&info),
            Err(Diag::InfoUnknownShape { .. })
        ));
    }

    /// The module reader's public path over hand-built interchange
    /// text — necessarily hand-fed (the embedded build cannot define
    /// modules, so ITS captures carry no entries; a live server's
    /// post-registration capture does — its INFO carries
    /// `mod::<name>` storage keys, which split_outcome pairs to the
    /// bare name): both dialects read to the AST, the name+silo
    /// asymmetry routes to verbatim `raw`, and a header/key
    /// disagreement diagnoses.
    #[test]
    fn modules_read_through_the_public_path() {
        let info = read_info_object(
            "{ modules: { color: \"DEFINE MODULE mod::color AS \
             f\\\"modules:/color.surli\\\" PERMISSIONS FULL\" } }",
        )
        .expect("walks");
        let entries = read_modules(&info);
        let entry = entries.into_iter().next().expect("one entry");
        let entry = entry.expect("it must read cleanly");
        assert_eq!(entry.name, "color");
        assert_eq!(entry.markers, Markers::default());
        let Stmt::Module(module) = entry.stmt else {
            panic!("must be a module");
        };
        assert_eq!(module.name, Some(Ident::new("color")));
        assert_eq!(
            module.executable,
            Executable::File {
                bucket: "modules".to_string(),
                key: "/color.surli".to_string(),
            }
        );
        assert!(module.unsigned);
        assert_eq!(module.permissions, Some(Permission::Full));
        // The name+silo asymmetry routes to verbatim `raw` — the raw
        // body is the ORIGINAL DDL text (edges trimmed).
        let info = read_info_object(
            "{ modules: { x: 'DEFINE MODULE mod::x AS silo::acme::pkg<1.0.0> PERMISSIONS \
             FULL' } }",
        )
        .expect("walks");
        let entries = read_modules(&info);
        let entry = entries.into_iter().next().expect("one entry");
        let entry = entry.expect("a raw route is not an error");
        assert_eq!(
            entry.markers.raw_routed,
            Some(RawRoute::ModuleNamedSilo),
            "the route marker names the shape"
        );
        assert_eq!(
            entry.stmt,
            Stmt::Raw(Raw {
                // The read-side terminator normalizes on (INFO never
                // prints the `;`).
                body: "DEFINE MODULE mod::x AS silo::acme::pkg<1.0.0> PERMISSIONS FULL;"
                    .to_string(),
                comments: Vec::new(),
                clause_comments: Vec::new(),
            })
        );
        // A header name that disagrees with its INFO key diagnoses.
        let info = read_info_object(
            "{ modules: { other: 'DEFINE MODULE mod::x FROM f\"b:/k\" UNSIGNED' } }",
        )
        .expect("walks");
        let entries = read_modules(&info);
        assert!(matches!(
            entries.into_iter().next().expect("one entry"),
            Err(Diag::InfoUnknownShape { .. })
        ));
        // The un-gated reader checks bracket balance itself: an
        // unclosed bracket would otherwise swallow the statement's
        // `;` inside a value capture.
        let info = read_info_object(
            "{ modules: { x: \"DEFINE MODULE mod::x AS f'bucket:/key' PERMISSIONS WHERE (a;\" \
             } }",
        )
        .expect("walks");
        let entries = read_modules(&info);
        assert!(matches!(
            entries.into_iter().next().expect("one entry"),
            Err(Diag::DdlTripwire { .. })
        ));
        // …and an empty bucket is not a forward-grammar file string.
        let info = read_info_object(
            "{ modules: { x: \"DEFINE MODULE mod::x AS f':/key' PERMISSIONS FULL\" } }",
        )
        .expect("walks");
        let entries = read_modules(&info);
        assert!(matches!(
            entries.into_iter().next().expect("one entry"),
            Err(Diag::DdlTripwire { .. })
        ));
    }

    /// The storage-key pairing (issue #61): the engine stores a named
    /// module under `mod::<name>` (the catalog's get_storage_name
    /// derivation, source-verified), so a post-registration capture's
    /// INFO key carries the prefix while the definition's header —
    /// and SDL — spell the bare name. The pairing accepts either
    /// spelling and the entry records the SDL-side (bare) name; a
    /// genuine disagreement still diagnoses.
    #[test]
    fn modules_pair_the_storage_key_prefix() {
        // The issue's exact shape: key `mod::surform`, header `surform`.
        let info = read_info_object(
            "{ modules: { \"mod::surform\": \"DEFINE MODULE mod::surform FROM \
             f\\\"modules:/surform.surli\\\" UNSIGNED PERMISSIONS FULL\" } }",
        )
        .expect("walks");
        let entries = read_modules(&info);
        let entry = entries.into_iter().next().expect("one entry");
        let entry = entry.expect("the storage-key prefix pairs");
        assert_eq!(entry.name, "surform", "the SDL-side bare name");
        let Stmt::Module(module) = entry.stmt else {
            panic!("must be a module");
        };
        assert_eq!(module.name, Some(Ident::new("surform")));
        // The legacy bare key pairs verbatim (today's posture).
        let info = read_info_object(
            "{ modules: { surform: \"DEFINE MODULE mod::surform FROM \
             f\\\"modules:/surform.surli\\\" UNSIGNED PERMISSIONS FULL\" } }",
        )
        .expect("walks");
        let entries = read_modules(&info);
        let entry = entries.into_iter().next().expect("one entry");
        let entry = entry.expect("the bare key pairs");
        assert_eq!(entry.name, "surform");
        // A genuine disagreement still diagnoses.
        let info = read_info_object(
            "{ modules: { \"mod::other\": \"DEFINE MODULE mod::surform FROM \
             f\\\"modules:/surform.surli\\\" UNSIGNED PERMISSIONS FULL\" } }",
        )
        .expect("walks");
        let entries = read_modules(&info);
        assert!(matches!(
            entries.into_iter().next().expect("one entry"),
            Err(Diag::InfoUnknownShape { .. })
        ));
    }

    // -- the high-tier splitters --------------------------------------
    // Canonical texts are the live engine's own prints (probe-pinned
    // 2026-09-23); the LIVE-captured goldens ride tests/info_read.rs.

    #[test]
    fn splits_index_variants() {
        // Plain, multi-field, COMMENT last (live: ix_plain).
        let entry = split_ok(
            split_index,
            "DEFINE INDEX ix_plain ON t FIELDS email, title COMMENT 'lookup';",
        );
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        assert_eq!(index.name, Ident::new("ix_plain"));
        assert_eq!(index.on, Ident::new("t"));
        assert_eq!(index.kind, IndexKind::Plain);
        assert_eq!(
            index.fields,
            vec![
                Expr {
                    canon: "email".to_string()
                },
                Expr {
                    canon: "title".to_string()
                },
            ]
        );
        assert!(!index.concurrently, "CONCURRENTLY never appears in INFO");
        assert_eq!(
            index.comment,
            Some(DocComment {
                lines: vec!["lookup".to_string()]
            })
        );
        // UNIQUE (live: ix_unique — a CONCURRENTLY-authored index
        // prints without it, so the read cannot carry it).
        let entry = split_ok(split_index, "DEFINE INDEX ix_u ON t FIELDS title UNIQUE;");
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        assert_eq!(index.kind, IndexKind::Unique);
        // COUNT bare and filtered (live: ix_count / ix_count_w — no
        // FIELDS clause prints for a count index).
        let entry = split_ok(split_index, "DEFINE INDEX ix_count ON t COUNT;");
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        assert!(matches!(index.kind, IndexKind::Count { where_: None }));
        assert!(index.fields.is_empty());
        let entry = split_ok(
            split_index,
            "DEFINE INDEX ix_count_w ON t COUNT WHERE status = 'active';",
        );
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        let IndexKind::Count { where_ } = &index.kind else {
            panic!();
        };
        assert_eq!(
            where_.as_ref().expect("the WHERE rides").canon,
            "status = 'active'"
        );
        // FULLTEXT minimal — the server's defaults materialize (live:
        // ix_ft) — and full (live: ix_ft_full, the glued pair).
        let entry = split_ok(
            split_index,
            "DEFINE INDEX ix_ft ON t FIELDS body FULLTEXT ANALYZER like BM25(1.2,0.75);",
        );
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        let IndexKind::FullText {
            analyzer,
            bm25,
            highlights,
        } = index.kind
        else {
            panic!();
        };
        assert_eq!(analyzer.expect("materializes").raw, "like");
        assert_eq!(bm25.expect("materializes"), Bm25(1.2, 0.75));
        assert!(!highlights);
        let entry = split_ok(
            split_index,
            "DEFINE INDEX ix_ft_full ON t FIELDS title FULLTEXT ANALYZER english \
             BM25(1.2,0.9) HIGHLIGHTS;",
        );
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        let IndexKind::FullText {
            analyzer,
            bm25,
            highlights,
        } = index.kind
        else {
            panic!();
        };
        assert_eq!(analyzer.expect("authored").raw, "english");
        assert_eq!(bm25.expect("authored"), Bm25(1.2, 0.9));
        assert!(highlights);
        // HNSW minimal — every parameter materializes, DIST spelled,
        // LM in the printer's `f` form (live: ix_hnsw_min).
        let entry = split_ok(
            split_index,
            "DEFINE INDEX ix_hnsw_min ON t FIELDS vector HNSW DIMENSION 4 DIST EUCLIDEAN \
             TYPE F32 EFC 150 M 12 M0 24 LM 0.40242960438184466f;",
        );
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        let IndexKind::Hnsw {
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
        } = index.kind
        else {
            panic!();
        };
        assert_eq!(dimension, 4);
        assert_eq!(distance, Some(Distance::Euclidean));
        assert_eq!(vector_type, Some(VectorType::F32));
        assert_eq!((m, m0, efc), (Some(12), Some(24), Some(150)));
        assert_eq!(lm, Some(Float(1.0 / (12.0f64).ln())));
        assert!(!extend_candidates && !keep_pruned_connections && !hashed_vector);
        // HNSW tuned: MINKOWSKI's order prints bare, the flags ride
        // (live: ix_hnsw_full).
        let entry = split_ok(
            split_index,
            "DEFINE INDEX ix_hnsw_full ON t FIELDS vector HNSW DIMENSION 4 DIST MINKOWSKI 3 \
             TYPE F16 EFC 200 M 16 M0 32 LM 0.36067376022224085f EXTEND_CANDIDATES \
             KEEP_PRUNED_CONNECTIONS;",
        );
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        let IndexKind::Hnsw {
            distance,
            vector_type,
            m,
            m0,
            lm,
            efc,
            extend_candidates,
            keep_pruned_connections,
            ..
        } = index.kind
        else {
            panic!();
        };
        assert_eq!(distance, Some(Distance::Minkowski(Float(3.0))));
        assert_eq!(vector_type, Some(VectorType::F16));
        assert_eq!((m, m0, efc), (Some(16), Some(32), Some(200)));
        assert_eq!(lm, Some(Float(1.0 / (16.0f64).ln())));
        assert!(extend_candidates && keep_pruned_connections);
        // DISKANN full (live: ix_disk_full) — `f`-suffixed ALPHA.
        let entry = split_ok(
            split_index,
            "DEFINE INDEX ix_disk_full ON t FIELDS embedding DISKANN DIMENSION 8 \
             DIST COSINE_NORMALIZED TYPE F16 DEGREE 32 L_BUILD 50 ALPHA 1.3f HASHED_VECTOR;",
        );
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        let IndexKind::DiskAnn {
            dimension,
            distance,
            vector_type,
            degree,
            l_build,
            alpha,
            hashed_vector,
        } = index.kind
        else {
            panic!();
        };
        assert_eq!(dimension, 8);
        assert_eq!(distance, Some(Distance::CosineNormalized));
        assert_eq!(vector_type, Some(VectorType::F16));
        assert_eq!(
            (degree, l_build, alpha),
            (Some(32), Some(50), Some(Float(1.3)))
        );
        assert!(hashed_vector);
    }

    #[test]
    fn splits_index_keyword_named_fields() {
        // A field literally named `unique` with a unique index
        // (live-printed): the peel takes the UPPERCASE kind word, the
        // lowercase ident rides the field list.
        let entry = split_ok(split_index, "DEFINE INDEX ix_u2 ON t FIELDS unique UNIQUE;");
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        assert_eq!(index.kind, IndexKind::Unique);
        assert_eq!(
            index.fields,
            vec![Expr {
                canon: "unique".to_string()
            }]
        );
        // The same for `count` under HNSW (live: ix_hnsw_kw).
        let entry = split_ok(
            split_index,
            "DEFINE INDEX ix_hnsw_kw ON t FIELDS count HNSW DIMENSION 2 DIST EUCLIDEAN \
             TYPE F32 EFC 150 M 12 M0 24 LM 0.4804530139182014f;",
        );
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        assert!(matches!(index.kind, IndexKind::Hnsw { dimension: 2, .. }));
        assert_eq!(
            index.fields,
            vec![Expr {
                canon: "count".to_string()
            }]
        );
        // …and a field named `comment` under the COMMENT clause (the
        // String follow keeps it content; live: ix_cm).
        let entry = split_ok(
            split_index,
            "DEFINE INDEX ix_cm ON t FIELDS comment COMMENT 'c';",
        );
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        assert_eq!(index.kind, IndexKind::Plain);
        assert_eq!(
            index.fields,
            vec![Expr {
                canon: "comment".to_string()
            }]
        );
        // The String follow's distinguishing case on the index path:
        // `comment UNIQUE` — a `Value` follow would confirm `UNIQUE`
        // as a value-opening word and empty the capture.
        let entry = split_ok(
            split_index,
            "DEFINE INDEX ix_cu ON t FIELDS comment UNIQUE;",
        );
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        assert_eq!(index.kind, IndexKind::Unique);
        assert_eq!(
            index.fields,
            vec![Expr {
                canon: "comment".to_string()
            }]
        );
    }

    #[test]
    fn index_split_tripwires() {
        // Fold-site domain mirrors (the engine's define-time checks):
        // `M 200`, the DISKANN restriction set, `DEGREE 0`.
        assert!(matches!(
            tripwire_err(
                split_index,
                "DEFINE INDEX bad ON t FIELDS vector HNSW DIMENSION 4 DIST EUCLIDEAN \
                 TYPE F32 EFC 150 M 200 M0 400 LM 0.1f;",
            ),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(
                split_index,
                "DEFINE INDEX bad ON t FIELDS embedding DISKANN DIMENSION 8 \
                 DIST CHEBYSHEV TYPE F32 DEGREE 64 L_BUILD 100 ALPHA 1.2f;",
            ),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(
                split_index,
                "DEFINE INDEX bad ON t FIELDS embedding DISKANN DIMENSION 8 \
                 DIST COSINE_NORMALIZED TYPE I8 DEGREE 64 L_BUILD 100 ALPHA 1.2f;",
            ),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(
                split_index,
                "DEFINE INDEX bad ON t FIELDS embedding DISKANN DIMENSION 8 \
                 DIST EUCLIDEAN TYPE F32 DEGREE 0 L_BUILD 100 ALPHA 1.2f;",
            ),
            Diag::DdlTripwire { .. }
        ));
        // Fields arity per kind: count takes none, the vector and
        // fulltext kinds exactly one.
        assert!(matches!(
            tripwire_err(split_index, "DEFINE INDEX bad ON t FIELDS a COUNT;"),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(
                split_index,
                "DEFINE INDEX bad ON t FIELDS a, b FULLTEXT ANALYZER like BM25(1.2,0.75);"
            ),
            Diag::DdlTripwire { .. }
        ));
        // An HNSW tail without DIMENSION is not a valid kind tail.
        assert!(matches!(
            tripwire_err(
                split_index,
                "DEFINE INDEX bad ON t FIELDS vector HNSW DIST EUCLIDEAN;",
            ),
            Diag::DdlTripwire { .. }
        ));
        // The keyword-ident ambiguity resolves the WRONG way only when
        // the peel is bypassed — a bare lowercase kind spelling in the
        // tail position is field content, and a fields-empty peel is a
        // tripwire, never a silent mis-read.
        assert!(matches!(
            tripwire_err(
                split_index,
                "DEFINE INDEX bad ON t FIELDS unique UNIQUE (x);",
            ),
            Diag::DdlTripwire { .. }
        ));
    }

    #[test]
    fn splits_field_shapes() {
        // Typed, DEFAULT before ASSERT in the print order, COMMENT
        // before GRAPHQL_*, PERMISSIONS last (live: contact / both).
        let entry = split_ok(
            split_field,
            "DEFINE FIELD contact ON t TYPE string DEFAULT 'unknown@example.com' \
             ASSERT string::is_email($value) COMMENT 'the address' PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert_eq!(entry.name, "contact");
        assert_eq!(field.kind, Some(Kind::new("string")));
        assert_eq!(field.on, Ident::new("t"));
        assert_eq!(
            field.default,
            Some(FieldDefault::Set(Expr {
                canon: "'unknown@example.com'".to_string()
            }))
        );
        assert_eq!(
            field.assert,
            Some(Expr {
                canon: "string::is_email($value)".to_string()
            })
        );
        assert_eq!(
            field.comment,
            Some(DocComment {
                lines: vec!["the address".to_string()]
            })
        );
        assert_eq!(field.permissions, Some(OpPermissions::Full));
        // READONLY + VALUE (live: locked).
        let entry = split_ok(
            split_field,
            "DEFINE FIELD locked ON t TYPE bool READONLY VALUE true PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert!(field.readonly);
        assert_eq!(
            field.value,
            Some(Expr {
                canon: "true".to_string()
            })
        );
        // Typed and typeless computed (live: shout / derived); the
        // option desugar rides as printed (live: optf).
        let entry = split_ok(
            split_field,
            "DEFINE FIELD shout ON t TYPE string COMPUTED string::uppercase(name) \
             PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            field.computed,
            Some(Expr {
                canon: "string::uppercase(name)".to_string()
            })
        );
        assert!(field.kind.is_some());
        let entry = split_ok(
            split_field,
            "DEFINE FIELD derived ON t COMPUTED string::uppercase(name) PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert!(field.kind.is_none(), "typeless computed");
        assert_eq!(entry.name, "derived");
        let entry = split_ok(
            split_field,
            "DEFINE FIELD optf ON t TYPE none | string PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert_eq!(field.kind, Some(Kind::new("none | string")));
        // DEFAULT ALWAYS (live: da) and FLEXIBLE after the kind
        // (live: meta2).
        let entry = split_ok(
            split_field,
            "DEFINE FIELD da ON t TYPE array<string> DEFAULT ALWAYS [] PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert!(matches!(
            field.default,
            Some(FieldDefault::Always(Expr { .. }))
        ));
        let entry = split_ok(
            split_field,
            "DEFINE FIELD meta2 ON t TYPE object FLEXIBLE PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert!(field.flexible);
        // REFERENCE: CASCADE, the always-printed IGNORE (the bare
        // flag's spelling), and a THEN expression — paren-wrapped by
        // the printer (live: author / owner / parent).
        for (text, expected) in [
            (
                "DEFINE FIELD author ON t TYPE record<user> REFERENCE ON DELETE CASCADE \
                 PERMISSIONS FULL;",
                OnDelete::Cascade,
            ),
            (
                "DEFINE FIELD owner ON t TYPE record<user> REFERENCE ON DELETE IGNORE \
                 PERMISSIONS FULL;",
                OnDelete::Ignore,
            ),
            (
                "DEFINE FIELD parent ON t TYPE record<page> REFERENCE ON DELETE THEN \
                 (CREATE cleanup_log SET at = time::now()) PERMISSIONS FULL;",
                OnDelete::Then(Expr {
                    canon: "(CREATE cleanup_log SET at = time::now())".to_string(),
                }),
            ),
        ] {
            let entry = split_ok(split_field, text);
            let Stmt::Field(field) = entry.stmt else {
                panic!();
            };
            assert_eq!(field.reference, Some(expected), "{text}");
        }
        // PERMISSIONS: the all-NONE shorthand prints as the merged
        // group (live: secret), and merged mixed groups split by the
        // printer's own grouping (live: audited).
        let entry = split_ok(
            split_field,
            "DEFINE FIELD secret ON t TYPE string PERMISSIONS FOR select, create, update NONE;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            field.permissions,
            Some(OpPermissions::Groups(vec![PermissionGroup {
                ops: vec![
                    PermissionOp::Select,
                    PermissionOp::Create,
                    PermissionOp::Update,
                ],
                value: Permission::None,
            }]))
        );
        let entry = split_ok(
            split_field,
            "DEFINE FIELD audited ON t TYPE string PERMISSIONS FOR select NONE, \
             FOR create, update WHERE $auth.admin = true;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            field.permissions,
            Some(OpPermissions::Groups(vec![
                PermissionGroup {
                    ops: vec![PermissionOp::Select],
                    value: Permission::None,
                },
                PermissionGroup {
                    ops: vec![PermissionOp::Create, PermissionOp::Update],
                    value: Permission::Expr(Expr {
                        canon: "$auth.admin = true".to_string(),
                    }),
                },
            ]))
        );
        // GRAPHQL_ALIAS + GRAPHQL_DEPRECATED (live: both).
        let entry = split_ok(
            split_field,
            "DEFINE FIELD both ON t TYPE string COMMENT 'both' GRAPHQL_ALIAS 'al' \
             GRAPHQL_DEPRECATED 'de' PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            field.graphql,
            Some(Graphql {
                alias: Some("al".to_string()),
                deprecated: Some("de".to_string()),
            })
        );
        // Dotted and wildcard paths — the cross-check name is the
        // decoded rejoined path.
        let entry = split_ok(
            split_field,
            "DEFINE FIELD matrix.*.* ON t TYPE int PERMISSIONS FULL;",
        );
        assert_eq!(entry.name, "matrix.*.*");
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert_eq!(field.name.segments.len(), 3);
        // The id field reads like any field (live: t_any's id).
        let entry = split_ok(
            split_field,
            "DEFINE FIELD id ON t_any TYPE string PERMISSIONS FULL;",
        );
        assert_eq!(entry.name, "id");
    }

    #[test]
    fn field_split_tripwires() {
        // PERMISSIONS always materializes.
        assert!(matches!(
            tripwire_err(split_field, "DEFINE FIELD x ON t TYPE string;"),
            Diag::DdlTripwire { .. }
        ));
        // A TYPE-less non-computed field (`DEFINE FIELD x ON t;` —
        // engine-legal hand-run DDL, printed typeless) routes to
        // verbatim raw: SDL's grammar demands a type on every
        // non-computed field.
        assert_eq!(
            split_raw(split_field, "DEFINE FIELD x ON t PERMISSIONS FULL;"),
            RawRoute::FieldUntyped
        );
        // COMPUTED's exclusions (the conflict text in the printer's
        // own clause order) and top-level demand.
        for text in [
            "DEFINE FIELD x ON t TYPE string DEFAULT 0 COMPUTED a + 1 PERMISSIONS FULL;",
            "DEFINE FIELD x.y ON t COMPUTED a + 1 PERMISSIONS FULL;",
        ] {
            assert!(matches!(
                tripwire_err(split_field, text),
                Diag::DdlTripwire { .. }
            ));
        }
        // FLEXIBLE needs an object-bearing kind.
        assert!(matches!(
            tripwire_err(
                split_field,
                "DEFINE FIELD x ON t TYPE string FLEXIBLE PERMISSIONS FULL;"
            ),
            Diag::DdlTripwire { .. }
        ));
        // REFERENCE is top-level only.
        assert!(matches!(
            tripwire_err(
                split_field,
                "DEFINE FIELD a.b ON t TYPE record<user> REFERENCE ON DELETE IGNORE \
                 PERMISSIONS FULL;"
            ),
            Diag::DdlTripwire { .. }
        ));
        // The id field's grammar.
        for text in [
            "DEFINE FIELD id ON t TYPE string VALUE 1 PERMISSIONS FULL;",
            "DEFINE FIELD id ON t TYPE string DEFAULT ALWAYS 1 PERMISSIONS FULL;",
            "DEFINE FIELD id ON t TYPE string READONLY PERMISSIONS FULL;",
        ] {
            assert!(matches!(
                tripwire_err(split_field, text),
                Diag::DdlTripwire { .. }
            ));
        }
    }

    #[test]
    fn splits_table_shapes() {
        // Normal + SCHEMAFULL + merged permission groups, delete-Full
        // elided by the emitter (live: t_perm).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE t_perm TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select, create FULL, \
             FOR update WHERE $auth.id = id, FOR delete NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(table.kind, TableKind::Normal);
        assert_eq!(table.schema_full, Some(true));
        assert!(!table.drop);
        assert_eq!(
            table.permissions,
            Some(OpPermissions::Groups(vec![
                PermissionGroup {
                    ops: vec![PermissionOp::Select, PermissionOp::Create],
                    value: Permission::Full,
                },
                PermissionGroup {
                    ops: vec![PermissionOp::Update],
                    value: Permission::Expr(Expr {
                        canon: "$auth.id = id".to_string(),
                    }),
                },
                PermissionGroup {
                    ops: vec![PermissionOp::Delete],
                    value: Permission::None,
                },
            ]))
        );
        // The all-NONE merge prints as the single value (live:
        // t_perm_same) and FULL stays the single value (live:
        // t_perm_full); PERMISSIONS NONE always materializes even
        // when SDL wrote nothing.
        let entry = split_ok(
            split_table,
            "DEFINE TABLE t_perm_same TYPE NORMAL SCHEMALESS PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(table.permissions, Some(OpPermissions::None));
        // DROP + CHANGEFEED, the value-normalized canon (live:
        // t_drop).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE t_drop TYPE NORMAL DROP SCHEMALESS CHANGEFEED 4w2d INCLUDE \
             ORIGINAL PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert!(table.drop);
        let changefeed = table.changefeed.expect("changefeed rides");
        use surrealdb_types::ToSql as _;
        assert_eq!(changefeed.expiry.to_sql(), "4w2d");
        assert!(changefeed.include_original);
        // COMMENT before GRAPHQL_* (live: t_cmt2).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE t_cmt2 TYPE NORMAL SCHEMALESS COMMENT 'the comment' \
             GRAPHQL_ALIAS 'a' GRAPHQL_DEPRECATED 'd' PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            table.comment,
            Some(DocComment {
                lines: vec!["the comment".to_string()]
            })
        );
        assert_eq!(
            table.graphql,
            Some(Graphql {
                alias: Some("a".to_string()),
                deprecated: Some("d".to_string()),
            })
        );
        // Relations: the full arrow shape (live: wrote), the
        // one-sided and unconstrained forms (live: one_in / uncon),
        // and an endpoint literally named `out` (live: rel1).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE wrote TYPE RELATION IN user | org OUT post | comment ENFORCED \
             SCHEMALESS PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            table.kind,
            TableKind::Relation {
                from: Some(vec![Ident::new("user"), Ident::new("org")]),
                to: Some(vec![Ident::new("post"), Ident::new("comment")]),
                enforced: true,
                lightweight: false,
            }
        );
        let entry = split_ok(
            split_table,
            "DEFINE TABLE one_in TYPE RELATION IN follower SCHEMALESS PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            table.kind,
            TableKind::Relation {
                from: Some(vec![Ident::new("follower")]),
                to: None,
                enforced: false,
                lightweight: false,
            }
        );
        let entry = split_ok(
            split_table,
            "DEFINE TABLE uncon TYPE RELATION SCHEMALESS PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert!(matches!(
            table.kind,
            TableKind::Relation {
                from: None,
                to: None,
                enforced: false,
                lightweight: false,
            }
        ));
        let entry = split_ok(
            split_table,
            "DEFINE TABLE rel1 TYPE RELATION IN out OUT post SCHEMALESS PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            table.kind,
            TableKind::Relation {
                from: Some(vec![Ident::new("out")]),
                to: Some(vec![Ident::new("post")]),
                enforced: false,
                lightweight: false,
            }
        );
        // Views: the fixed shape with the COMMENT before the query
        // (live: v_fixed) and bare (live: v_bare, TYPE-less).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE v_fixed TYPE ANY SCHEMALESS COMMENT 'Posts per author' \
             AS SELECT author, count() AS posts FROM post GROUP BY author \
             PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert!(matches!(table.kind, TableKind::View { .. }));
        assert_eq!(table.schema_full, None, "the view's fixed shape");
        assert_eq!(
            table.comment,
            Some(DocComment {
                lines: vec!["Posts per author".to_string()]
            })
        );
        let entry = split_ok(
            split_table,
            "DEFINE TABLE v_bare TYPE ANY SCHEMALESS AS SELECT 1 FROM post PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        let TableKind::View { query } = table.kind else {
            panic!();
        };
        assert_eq!(query.canon, "SELECT 1 FROM post");
        assert!(matches!(table.permissions, Some(OpPermissions::None)));
    }

    #[test]
    fn deviant_views_route_raw() {
        // Every shape the engine prints and SDL's fixed view form
        // cannot express (probe-pinned live: v_normal / v_schemafull /
        // v_drop / v_cf) routes to verbatim raw.
        for text in [
            "DEFINE TABLE v_normal TYPE NORMAL SCHEMALESS AS SELECT 1 FROM t \
             PERMISSIONS NONE;",
            "DEFINE TABLE v_schemafull TYPE ANY SCHEMAFULL AS SELECT 1 FROM t \
             PERMISSIONS NONE;",
            "DEFINE TABLE v_drop TYPE ANY DROP SCHEMALESS AS SELECT 1 FROM t \
             PERMISSIONS NONE;",
            "DEFINE TABLE v_cf TYPE ANY SCHEMALESS AS SELECT 1 FROM t CHANGEFEED 1d \
             PERMISSIONS NONE;",
            "DEFINE TABLE v_rel TYPE RELATION IN a AS SELECT 1 FROM t PERMISSIONS NONE;",
        ] {
            assert_eq!(
                split_raw(split_table, text),
                RawRoute::TableViewDeviant,
                "{text}"
            );
        }
    }

    #[test]
    fn relation_clauses_demand_the_relation_kind() {
        // Hand-fed defense: the printer only prints `IN`/`OUT`/
        // `ENFORCED` on RELATION tables (the engine folds them into
        // the relation shape at define); a non-relation statement
        // carrying them is drift, fail-closed.
        for text in [
            "DEFINE TABLE t TYPE NORMAL SCHEMALESS ENFORCED PERMISSIONS NONE;",
            "DEFINE TABLE t TYPE ANY SCHEMALESS IN a PERMISSIONS NONE;",
            "DEFINE TABLE t TYPE ANY SCHEMALESS OUT a PERMISSIONS NONE;",
        ] {
            assert!(
                matches!(tripwire_err(split_table, text), Diag::DdlTripwire { .. }),
                "{text}"
            );
        }
        // …and a permission group with no operation word never
        // materializes.
        assert!(matches!(
            tripwire_err(
                split_table,
                "DEFINE TABLE t TYPE NORMAL SCHEMALESS PERMISSIONS FOR FULL;"
            ),
            Diag::DdlTripwire { .. }
        ));
        // A relation with DROP reads (DROP prints
        // between the sides and the schema flag).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE rd TYPE RELATION IN a DROP SCHEMALESS PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert!(table.drop);
        assert!(matches!(
            table.kind,
            TableKind::Relation {
                from: Some(..),
                to: None,
                enforced: false,
                lightweight: false,
            }
        ));
        // Group PERMISSIONS after a captured expression clause (the
        // Perm follow confirms the FOR form).
        let entry = split_ok(
            split_table,
            "DEFINE TABLE tp TYPE NORMAL SCHEMALESS CHANGEFEED 1d PERMISSIONS FOR select \
             WHERE meta = 1, FOR create, update FULL;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            table.permissions,
            Some(OpPermissions::Groups(vec![
                PermissionGroup {
                    ops: vec![PermissionOp::Select],
                    value: Permission::Expr(Expr {
                        canon: "meta = 1".into()
                    }),
                },
                PermissionGroup {
                    ops: vec![PermissionOp::Create, PermissionOp::Update],
                    value: Permission::Full,
                },
            ]))
        );
    }

    #[test]
    fn signed_kind_floats_route_raw() {
        // Engine-legal, live-printed, outside SDL's unsigned float
        // grammar — verbatim raw.
        for text in [
            "DEFINE INDEX neg_bm ON t FIELDS b FULLTEXT ANALYZER like BM25(-1,-2);",
            "DEFINE INDEX neg_al ON t FIELDS e DISKANN DIMENSION 8 DIST EUCLIDEAN TYPE F32 \
             DEGREE 64 L_BUILD 100 ALPHA -1.2f;",
            "DEFINE INDEX neg_lm ON t FIELDS v HNSW DIMENSION 4 DIST EUCLIDEAN TYPE F32 \
             EFC 150 M 12 M0 24 LM -0.4f;",
        ] {
            assert_eq!(
                split_raw(split_index, text),
                RawRoute::IndexSignedFloat,
                "{text}"
            );
        }
    }

    #[test]
    fn uppercase_kind_field_reads_as_plain() {
        // A plain index over a field literally spelled `UNIQUE`
        // (live-printed bare): the empty-headed candidate cannot be
        // the kind, so the fallback reads it.
        let entry = split_ok(split_index, "DEFINE INDEX ix_up ON t FIELDS UNIQUE;");
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        assert_eq!(index.kind, IndexKind::Plain);
        assert_eq!(
            index.fields,
            vec![Expr {
                canon: "UNIQUE".into()
            }]
        );
        // …and comma-list position is likewise a field item, never
        // the kind (the printer joins fields with `", "`):
        // two plain fields.
        let entry = split_ok(split_index, "DEFINE INDEX ix ON t FIELDS a, UNIQUE;");
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        assert_eq!(index.kind, IndexKind::Plain);
        assert_eq!(
            index.fields,
            vec![
                Expr { canon: "a".into() },
                Expr {
                    canon: "UNIQUE".into()
                }
            ]
        );
        // The kind reading still wins in kind position (the kind
        // follows the last field after a single space).
        let entry = split_ok(split_index, "DEFINE INDEX ix ON t FIELDS a, b UNIQUE;");
        let Stmt::Index(index) = entry.stmt else {
            panic!();
        };
        assert_eq!(index.kind, IndexKind::Unique);
        assert_eq!(
            index.fields,
            vec![Expr { canon: "a".into() }, Expr { canon: "b".into() }]
        );
    }

    #[test]
    fn signed_zero_kind_float_routes_raw() {
        // `-0` prints and is outside SDL's unsigned grammar like any
        // negative (`< 0.0` misses it).
        assert_eq!(
            split_raw(
                split_index,
                "DEFINE INDEX i0 ON t FIELDS b FULLTEXT ANALYZER english BM25(-0, 0.75);"
            ),
            RawRoute::IndexSignedFloat
        );
    }

    #[test]
    fn idiom_segments_never_stop_captures() {
        // `DEFAULT src.readonly` — a word directly preceded by `.` is
        // an idiom segment, never the READONLY flag (the Any follow
        // misfired on it).
        let entry = split_ok(
            split_field,
            "DEFINE FIELD dr ON t TYPE any DEFAULT src.readonly PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert!(!field.readonly);
        assert_eq!(
            field.default,
            Some(FieldDefault::Set(Expr {
                canon: "src.readonly".into()
            }))
        );
        // A value ENDING in a regex literal: the closer `/` is glued
        // to its body (division prints spaced both sides), so the
        // READONLY flag after it is the clause,
        // never swallowed as an operand.
        let entry = split_ok(
            split_field,
            "DEFINE FIELD re ON t TYPE string DEFAULT /^[a-z]+$/ READONLY \
             PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert!(field.readonly);
        assert_eq!(
            field.default,
            Some(FieldDefault::Set(Expr {
                canon: "/^[a-z]+$/".into()
            }))
        );
        // …and a BARE `readonly` operand keeps its capture whole: one
        // preceded by an operator (the silent
        // truncation face) and one standing alone as the whole value.
        for value in ["x1 + readonly", "readonly"] {
            let entry = split_ok(
                split_field,
                &format!("DEFINE FIELD x ON t TYPE any DEFAULT {value} PERMISSIONS FULL;"),
            );
            let Stmt::Field(field) = entry.stmt else {
                panic!();
            };
            assert!(!field.readonly, "{value}");
            assert_eq!(
                field.default,
                Some(FieldDefault::Set(Expr {
                    canon: value.to_string()
                })),
                "{value}"
            );
        }
    }

    #[test]
    fn index_field_path_kind_segment_reads_plain() {
        // A field path whose last segment spells an uppercase kind
        // word is a field, never the kind (the
        // peel once turned `meta.UNIQUE` into a Unique kind over a
        // truncated `meta.` path).
        for text in ["meta.UNIQUE", "meta.FULLTEXT", "meta.COUNT"] {
            let entry = split_ok(
                split_index,
                &format!("DEFINE INDEX ix ON pt FIELDS {text};"),
            );
            let Stmt::Index(index) = entry.stmt else {
                panic!();
            };
            assert_eq!(index.kind, IndexKind::Plain, "{text}");
            assert_eq!(
                index.fields,
                vec![Expr {
                    canon: text.to_string()
                }]
            );
        }
    }

    #[test]
    fn endpoint_named_out_reads() {
        // `IN OUT OUT host` — the first `OUT` is an endpoint ident in
        // list position, the second the clause);
        // and a lone `IN OUT` is a one-sided relation whose endpoint
        // is named OUT.
        for (text, from, to) in [
            (
                "DEFINE TABLE rel_out TYPE RELATION IN OUT OUT host SCHEMALESS \
                 PERMISSIONS NONE;",
                Some(vec![Ident::new("OUT")]),
                Some(vec![Ident::new("host")]),
            ),
            (
                "DEFINE TABLE rel_one TYPE RELATION IN OUT SCHEMALESS PERMISSIONS NONE;",
                Some(vec![Ident::new("OUT")]),
                None,
            ),
        ] {
            let entry = split_ok(split_table, text);
            let Stmt::Table(table) = entry.stmt else {
                panic!();
            };
            assert_eq!(
                table.kind,
                TableKind::Relation {
                    from,
                    to,
                    enforced: false,
                    lightweight: false,
                },
                "{text}"
            );
        }
        // Endpoints named the other table-clause keywords read the
        // same way — capture-first and operator-preceded positions
        // are operand positions (the first-token
        // check is trivia-aware, and the printer's own list position
        // `a | drop` is operator-preceded), so no table-clause
        // keyword mis-fires as a clause here.
        for (text, from, to) in [
            (
                "DEFINE TABLE rel_enf TYPE RELATION IN enforced OUT hostt SCHEMALESS \
                 PERMISSIONS NONE;",
                Some(vec![Ident::new("enforced")]),
                Some(vec![Ident::new("hostt")]),
            ),
            (
                "DEFINE TABLE rel_drop TYPE RELATION IN a | drop SCHEMALESS \
                 PERMISSIONS NONE;",
                Some(vec![Ident::new("a"), Ident::new("drop")]),
                None,
            ),
        ] {
            let entry = split_ok(split_table, text);
            let Stmt::Table(table) = entry.stmt else {
                panic!();
            };
            assert_eq!(
                table.kind,
                TableKind::Relation {
                    from,
                    to,
                    enforced: false,
                    lightweight: false,
                },
                "{text}"
            );
        }
    }

    #[test]
    fn unrepresentable_first_endpoint_routes_raw() {
        // The arrow form's first from-side element prints bare — an
        // escape-requiring endpoint cannot ride the AST (the writer's
        // invariant, live-probed) — verbatim raw.
        assert_eq!(
            split_raw(
                split_table,
                "DEFINE TABLE rel2 TYPE RELATION IN `a.b` OUT other SCHEMALESS \
                 PERMISSIONS NONE;",
            ),
            RawRoute::TableEndpointUnrepresentable
        );
        // Later elements and the OUT side print quoted and ride.
        let entry = split_ok(
            split_table,
            "DEFINE TABLE rel3 TYPE RELATION IN a | `b.c` OUT `d.e` SCHEMALESS \
             PERMISSIONS NONE;",
        );
        let Stmt::Table(table) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            table.kind,
            TableKind::Relation {
                from: Some(vec![Ident::new("a"), Ident::new("b.c")]),
                to: Some(vec![Ident::new("d.e")]),
                enforced: false,
                lightweight: false,
            }
        );
    }

    #[test]
    fn quoted_path_segments_cross_check() {
        // A field path with a quote-requiring segment: the INFO key
        // renders per-segment escaping (`` "`a.b`.c" ``), and the
        // cross-check name is the escaped rejoin (the
        // decoded rejoin once disagreed with the key and failed
        // the entry).
        let info = read_info_object(
            r#"{ fields: { "`a.b`.c": 'DEFINE FIELD `a.b`.c ON t TYPE int PERMISSIONS FULL' } }"#,
        )
        .expect("walks");
        let entries = read_fields(&info);
        let entry = entries.into_iter().next().expect("one entry");
        let entry = entry.expect("the quoted-segment field reads");
        assert_eq!(entry.name, "`a.b`.c");
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            field.name.segments,
            vec![
                FieldPathSeg::Name(Ident::new("a.b")),
                FieldPathSeg::Name(Ident::new("c")),
            ]
        );
    }

    #[test]
    fn field_clause_order_matches_the_printer() {
        // DEFAULT precedes VALUE and READONLY in the printer's fmt
        // arm order (`define/field.rs` — the
        // DEFAULT capture once swallowed the VALUE clause).
        let entry = split_ok(
            split_field,
            "DEFINE FIELD fvd ON a TYPE string DEFAULT 2 VALUE 1 ASSERT 3 > 2 \
             PERMISSIONS FOR select WHERE meta = 1, FOR create, update FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            field.default,
            Some(FieldDefault::Set(Expr { canon: "2".into() }))
        );
        assert_eq!(field.value, Some(Expr { canon: "1".into() }));
        assert_eq!(
            field.assert,
            Some(Expr {
                canon: "3 > 2".into()
            })
        );
        assert_eq!(
            field.permissions,
            Some(OpPermissions::Groups(vec![
                PermissionGroup {
                    ops: vec![PermissionOp::Select],
                    value: Permission::Expr(Expr {
                        canon: "meta = 1".into()
                    }),
                },
                PermissionGroup {
                    ops: vec![PermissionOp::Create, PermissionOp::Update],
                    value: Permission::Full,
                },
            ]))
        );
        // READONLY rides between DEFAULT and VALUE.
        let entry = split_ok(
            split_field,
            "DEFINE FIELD ord ON a TYPE string DEFAULT 2 READONLY VALUE 1 \
             PERMISSIONS FULL;",
        );
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert!(field.readonly);
        assert_eq!(
            field.default,
            Some(FieldDefault::Set(Expr { canon: "2".into() }))
        );
        assert_eq!(field.value, Some(Expr { canon: "1".into() }));
    }

    #[test]
    fn raw_routed_table_children_surface_top_level() {
        // A deviant view WITH a child map must not
        // panic — the raw table surfaces, its children ride top-level
        // (nothing a child map held is dropped), and a child
        // diagnostic rides its own entry.
        let info = read_info_object(
            "{ tables: { v: 'DEFINE TABLE v TYPE ANY SCHEMALESS AS SELECT 1 FROM t \
             CHANGEFEED 1d PERMISSIONS NONE' } }",
        )
        .expect("walks");
        let children = BTreeMap::from([(
            "v".to_string(),
            read_info_object(
                "{ fields: { f: 'DEFINE FIELD f ON v TYPE string PERMISSIONS FULL' }, \
                 indexes: { ix: 'DEFINE INDEX ix ON v FIELDS body FULLTEXT ANALYZER like \
                 VS' } }",
            )
            .expect("walks"),
        )]);
        let entries = read_tables(&info, &children);
        assert_eq!(entries.len(), 3, "the raw table, the field, the raw index");
        assert!(
            matches!(entries[0], Ok(ref e) if e.markers.raw_routed == Some(RawRoute::TableViewDeviant))
        );
        assert!(matches!(entries[1], Ok(ref e) if matches!(e.stmt, Stmt::Field(_))));
        assert!(matches!(
            entries[2],
            Ok(ref e) if e.markers.raw_routed == Some(RawRoute::IndexLegacyVs)
        ));
        // No child map for a deviant view reads as before.
        let entries = read_tables(&info, &BTreeMap::new());
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(entries[0], Ok(ref e) if e.markers.raw_routed == Some(RawRoute::TableViewDeviant))
        );
        // Children with no `tables` section diagnose (they can name
        // nothing — never silently dropped).
        let info = read_info_object("{ users: {  } }").expect("walks");
        let children = BTreeMap::from([(
            "ghost".to_string(),
            read_info_object("{ fields: {  } }").expect("walks"),
        )]);
        let entries = read_tables(&info, &children);
        assert!(matches!(
            entries.into_iter().next().expect("one entry"),
            Err(Diag::InfoUnknownShape { .. })
        ));
        // A view's grammar has no nested-define spelling: a view with
        // children keeps the children top-level (the engine accepts
        // children on views — they once nested and
        // vanished at write time).
        let info = read_info_object(
            "{ tables: { v: 'DEFINE TABLE v TYPE ANY SCHEMALESS AS SELECT 1 FROM t \
             PERMISSIONS NONE' } }",
        )
        .expect("walks");
        let children = BTreeMap::from([(
            "v".to_string(),
            read_info_object(
                "{ fields: { f: 'DEFINE FIELD f ON v TYPE string PERMISSIONS FULL' } }",
            )
            .expect("walks"),
        )]);
        let entries = read_tables(&info, &children);
        assert_eq!(entries.len(), 2, "the view, then its child top-level");
        assert!(
            matches!(entries[0], Ok(ref e) if matches!(e.stmt, Stmt::Table(ref t) if t.kind.is_view()))
        );
        assert!(matches!(entries[1], Ok(ref e) if matches!(e.stmt, Stmt::Field(_))));
    }

    #[test]
    fn doubly_quoted_keys_decode_to_the_header_name() {
        // The object renderer wraps keys that need JSON quoting in a
        // string; a name that itself needed SurrealQL quoting arrives
        // doubly wrapped (`` "`a.b`" ``) — decoded to the name the
        // header carries, so the cross-check holds (the
        // E3002 disagreement once poisoned the host table).
        let info = read_info_object(
            r#"{ fields: { "`a.b`": 'DEFINE FIELD ⟨a.b⟩ ON t TYPE int PERMISSIONS FULL' } }"#,
        )
        .expect("walks");
        let entries = read_fields(&info);
        let entry = entries.into_iter().next().expect("one entry");
        let entry = entry.expect("the quoted-key field reads");
        assert_eq!(entry.name, "a.b");
        let Stmt::Field(field) = entry.stmt else {
            panic!();
        };
        assert_eq!(
            field.name.segments,
            vec![FieldPathSeg::Name(Ident::new("a.b"))]
        );
    }

    #[test]
    fn typeless_views_route_deviant() {
        // The printer always spells the view's TYPE (`TYPE ANY`) — a
        // TYPE-less view is hand-fed drift, routed (the reader once
        // accepted the missing word).
        assert_eq!(
            split_raw(
                split_table,
                "DEFINE TABLE v SCHEMALESS AS SELECT 1 FROM t PERMISSIONS NONE;"
            ),
            RawRoute::TableViewDeviant
        );
    }

    #[test]
    fn legacy_vs_index_routes_raw() {
        // The VS token is print-only in 3.2.4: the embedded parser
        // rejects the DDL (E3001), and the reverse section routes the
        // shape to verbatim raw. Unproducible live on 3.2.4 (the
        // define-time parser rejects VS) — reachable from a database
        // upgraded across versions; hand-fed here by necessity.
        let info = read_info_object(
            "{ indexes: { ix: 'DEFINE INDEX ix ON t FIELDS body FULLTEXT ANALYZER like VS' \
             } }",
        )
        .expect("walks");
        let entries = read_indexes(&info);
        let entry = entries.into_iter().next().expect("one entry");
        let entry = entry.expect("the VS shape reads as raw");
        assert_eq!(
            entry.markers,
            Markers {
                raw_routed: Some(RawRoute::IndexLegacyVs),
                ..Markers::default()
            }
        );
        let Stmt::Raw(raw) = &entry.stmt else {
            panic!("expected the verbatim body");
        };
        assert!(raw.body.contains("VS"));
        // The terminator normalizes on (INFO never prints it; a
        // re-emitted multi-statement batch needs it).
        assert!(raw.body.ends_with(';'));
        // The terminated body rides the writer/parse fixed point
        // (the read-side raw bytes changed — pin
        // the round trip).
        let sdl = crate::info::write::write_canonical(&crate::ast::SdlFile {
            items: vec![entry.stmt.clone()],
            trailing_comments: Vec::new(),
        });
        let (reparsed, diags) = crate::parser::parse_file(&sdl);
        assert!(diags.is_empty(), "{sdl}");
        assert_eq!(reparsed.items, vec![entry.stmt]);
        // An unparsable index WITHOUT the token stays E3001 — the
        // route never fires on other rejections.
        let info =
            read_info_object("{ indexes: { ix: 'DEFINE INDEX ix ON t FIELDS ' } }").expect("walks");
        let entries = read_indexes(&info);
        assert!(matches!(
            entries.into_iter().next().expect("one entry"),
            Err(Diag::InfoUnparsable { .. })
        ));
    }

    /// The object walk's depth budget: a pathological nesting depth
    /// (a stack-overflow abort, ~10⁴ levels of hand-crafted text)
    /// produces the well-formed E3004 instead; real INFO nesting
    /// (and the budget's own edge) walks fine.
    #[test]
    fn object_walk_depth_budgets_at_max_info_depth() {
        let deep = |levels: usize| {
            let mut text = "{a:".repeat(levels);
            text.push('1');
            text.push_str(&"}".repeat(levels));
            read_info_object(&text)
        };
        // At the budget: the outermost object is depth 0, so
        // MAX_INFO_DEPTH nested objects walk.
        deep(MAX_INFO_DEPTH).expect("the budget's edge walks");
        // One deeper: the well-formed diagnostic, not an abort.
        let err = deep(MAX_INFO_DEPTH + 1).expect_err("over the budget");
        assert!(matches!(err, Diag::InfoDepthExceeded { .. }), "{err:?}");
        assert_eq!(
            err.to_string(),
            "cannot read INFO input: the object nests deeper than 128 levels"
        );
    }

    #[test]
    fn read_tables_nests_children() {
        let info = read_info_object(
            "{ tables: { wrote: 'DEFINE TABLE wrote TYPE RELATION IN user | org OUT post \
             SCHEMALESS PERMISSIONS NONE', plain: 'DEFINE TABLE plain TYPE ANY SCHEMALESS \
             PERMISSIONS NONE' } }",
        )
        .expect("walks");
        let children = BTreeMap::from([
            (
                "wrote".to_string(),
                read_info_object(
                    "{ fields: { in: 'DEFINE FIELD in ON wrote TYPE record<user | org> \
                     PERMISSIONS FULL', out: 'DEFINE FIELD out ON wrote TYPE record<post> \
                     PERMISSIONS FULL' }, indexes: {}, events: {} }",
                )
                .expect("walks"),
            ),
            (
                "stray".to_string(),
                read_info_object("{ fields: {}, indexes: {}, events: {} }").expect("walks"),
            ),
        ]);
        let entries = read_tables(&info, &children);
        assert_eq!(entries.len(), 3, "the stray child diagnostic first");
        assert!(
            matches!(entries[0], Err(Diag::InfoUnknownShape { .. })),
            "a child naming no table diagnoses"
        );
        // Map order: plain < wrote (BTreeMap keys sort).
        let plain = entries[1].as_ref().expect("plain reads");
        let Stmt::Table(table) = &plain.stmt else {
            panic!();
        };
        assert!(table.nested.is_empty(), "no child map, no nesting");
        let wrote = &entries[2];
        let wrote = wrote.as_ref().expect("wrote reads");
        assert_eq!(wrote.name, "wrote");
        let Stmt::Table(table) = &wrote.stmt else {
            panic!();
        };
        assert_eq!(table.nested.len(), 2, "the endpoint fields nest");
        assert!(
            matches!(&table.nested[0], Stmt::Field(f) if f.name.sole_name().is_some_and(|n| n.raw == "in"))
        );
        assert!(
            matches!(&table.nested[1], Stmt::Field(f) if f.name.sole_name().is_some_and(|n| n.raw == "out"))
        );
        // A child whose ON disagrees with its host fails the table
        // (map order: plain carries no child map and reads first).
        let children = BTreeMap::from([(
            "wrote".to_string(),
            read_info_object(
                "{ fields: { f: 'DEFINE FIELD f ON elsewhere TYPE string PERMISSIONS FULL' } }",
            )
            .expect("walks"),
        )]);
        let entries = read_tables(&info, &children);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_ok(), "plain has no child map");
        assert!(matches!(entries[1], Err(Diag::InfoUnknownShape { .. })));
        // A VS-routed child surfaces as a top-level raw entry, never
        // nested (SDL reserves `raw` for top-level placement).
        let info = read_info_object(
            "{ tables: { t: 'DEFINE TABLE t TYPE ANY SCHEMALESS PERMISSIONS NONE' } }",
        )
        .expect("walks");
        let children = BTreeMap::from([(
            "t".to_string(),
            read_info_object(
                "{ indexes: { ix: 'DEFINE INDEX ix ON t FIELDS body FULLTEXT ANALYZER like \
                 VS' } }",
            )
            .expect("walks"),
        )]);
        let entries = read_tables(&info, &children);
        assert_eq!(entries.len(), 2, "the table, then the raw child");
        let table = entries[0].as_ref().expect("the table reads");
        let Stmt::Table(table) = &table.stmt else {
            panic!();
        };
        assert!(table.nested.is_empty());
        let raw_child = entries[1].as_ref().expect("the raw child rides");
        assert_eq!(raw_child.markers.raw_routed, Some(RawRoute::IndexLegacyVs));
        assert!(matches!(raw_child.stmt, Stmt::Raw(_)));
    }

    #[test]
    fn read_tables_drops_implied_lightweight_fields() {
        // A LIGHTWEIGHT relation's auto-installed `in`/`out` fields
        // drop at read (nesting them would print SDL the engine
        // itself rejects) — but ONLY in their implied shape: a
        // hand-carried divergent `in` stays (fail-closed loud on
        // re-parse; live INFO cannot carry the shape).
        let info = read_info_object(
            "{ tables: { lw: 'DEFINE TABLE lw TYPE RELATION IN a OUT b ENFORCED LIGHTWEIGHT \
             SCHEMALESS PERMISSIONS NONE' } }",
        )
        .expect("walks");
        let children = BTreeMap::from([(
            "lw".to_string(),
            read_info_object(
                "{ fields: { in: 'DEFINE FIELD in ON lw TYPE record<a> PERMISSIONS FULL', \
                 out: 'DEFINE FIELD out ON lw TYPE record<b> PERMISSIONS FULL', \
                 divergent: 'DEFINE FIELD divergent ON lw TYPE record<a> READONLY \
                 PERMISSIONS FULL' } }",
            )
            .expect("walks"),
        )]);
        let entries = read_tables(&info, &children);
        let entry = entries.into_iter().next().expect("one entry");
        let entry = entry.expect("reads");
        let Stmt::Table(table) = &entry.stmt else {
            panic!();
        };
        assert_eq!(table.nested.len(), 1, "the implied pair drops");
        assert!(
            matches!(
                &table.nested[0],
                Stmt::Field(f) if f.name.sole_name().is_some_and(|n| n.raw == "divergent")
            ),
            "the divergent shape stays loud: {:?}",
            table.nested
        );
    }

    #[test]
    fn splits_access() {
        // The jwt variant's positional source, the redacted sentinel
        // feeding `Markers::redacted`, and the mirrored issuer folding
        // to the key-less shape.
        let entry = split_ok(
            split_access,
            "DEFINE ACCESS a ON DATABASE TYPE JWT ALGORITHM HS256 KEY '[REDACTED]' WITH ISSUER \
             KEY '[REDACTED]' DURATION FOR TOKEN 2h, FOR SESSION NONE;",
        );
        assert_eq!(
            entry.markers,
            Markers {
                redacted: true,
                ..Markers::default()
            }
        );
        let Stmt::Access(access) = entry.stmt else {
            panic!();
        };
        let AccessKind::Jwt { source } = access.kind else {
            panic!();
        };
        assert_eq!(
            source.verify,
            JwtVerify::Key {
                algorithm: Algorithm::Hs256,
                key: Expr {
                    canon: "'[REDACTED]'".to_string(),
                },
            }
        );
        assert_eq!(
            source.issuer,
            Some(JwtIssuer {
                algorithm: None,
                key: None,
            }),
            "the printed mirror reads as the key-less issuer"
        );
        assert_eq!(
            access.duration,
            Some(AccessDuration {
                grant: None,
                token: Some(Expr {
                    canon: "2h".to_string(),
                }),
                session: Some(Expr {
                    canon: "NONE".to_string(),
                }),
            })
        );
        // The asymmetric verify key renders verbatim and carries no
        // marker; only the issuer key's sentinel does.
        let entry = split_ok(
            split_access,
            "DEFINE ACCESS b ON ROOT TYPE JWT ALGORITHM ES256 KEY 'public' WITH ISSUER \
             ALGORITHM ES256 KEY '[REDACTED]' DURATION FOR TOKEN 1h, FOR SESSION NONE \
             COMMENT 'note';",
        );
        assert!(entry.markers.redacted);
        let Stmt::Access(access) = entry.stmt else {
            panic!();
        };
        let AccessKind::Jwt { source } = access.kind else {
            panic!();
        };
        assert_eq!(
            source.issuer.and_then(|issuer| issuer.key),
            Some(Expr {
                canon: "'[REDACTED]'".to_string(),
            }),
            "an asymmetric pair keeps its explicit issuer key"
        );
        assert_eq!(
            access.comment.map(|comment| comment.joined()),
            Some("note".to_string())
        );
        // The record variant's auto-issued block reads through the
        // ordinary WITH JWT arm; signup/signin capture braced bodies.
        let entry = split_ok(
            split_access,
            "DEFINE ACCESS c ON DATABASE TYPE RECORD SIGNIN { SELECT * FROM user WHERE email \
             = $email } WITH JWT ALGORITHM HS512 KEY '[REDACTED]' WITH ISSUER KEY \
             '[REDACTED]' DURATION FOR TOKEN 1h, FOR SESSION NONE;",
        );
        assert!(entry.markers.redacted);
        let Stmt::Access(access) = entry.stmt else {
            panic!();
        };
        let AccessKind::Record { jwt, .. } = access.kind else {
            panic!();
        };
        assert_eq!(jwt, auto_shape_record_jwt());
        // Bearer: FOR USER with no jwt (never rendered) and the
        // grant-capable DURATION trio.
        let entry = split_ok(
            split_access,
            "DEFINE ACCESS d ON DATABASE TYPE BEARER FOR USER DURATION FOR GRANT 4w2d, FOR \
             TOKEN 1h, FOR SESSION NONE;",
        );
        let Stmt::Access(access) = entry.stmt else {
            panic!();
        };
        assert!(matches!(
            access.kind,
            AccessKind::Bearer {
                subject: BearerSubject::User,
                jwt: None,
            }
        ));
        assert_eq!(
            access.duration.and_then(|d| d.grant).map(|g| g.canon),
            Some("4w2d".to_string())
        );
    }

    /// The read shape of the auto-issued record jwt block: the
    /// sentinel verify key with the mirrored key-less issuer.
    fn auto_shape_record_jwt() -> Option<JwtSource> {
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

    #[test]
    fn access_splitter_diagnoses_drift() {
        // The always-materialized clauses.
        assert!(matches!(
            tripwire_err(split_access, "DEFINE ACCESS a TYPE JWT URL 'u';"),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(
                split_access,
                "DEFINE ACCESS a ON DATABASE TYPE JWT ALGORITHM HS256 KEY '[REDACTED]';"
            ),
            Diag::DdlTripwire { .. }
        ));
        // A record clause on a jwt access never renders.
        assert!(matches!(
            tripwire_err(
                split_access,
                "DEFINE ACCESS a ON DATABASE TYPE JWT URL 'u' SIGNIN 1 DURATION FOR SESSION \
                 NONE;"
            ),
            Diag::DdlTripwire { .. }
        ));
        // A non-NONE value in a GRANT/TOKEN side, and an unknown side.
        assert!(matches!(
            tripwire_err(
                split_access,
                "DEFINE ACCESS a ON DATABASE TYPE BEARER FOR USER DURATION FOR GRANT NONE, \
                 FOR SESSION NONE;"
            ),
            Diag::DdlTripwire { .. }
        ));
        assert!(matches!(
            tripwire_err(
                split_access,
                "DEFINE ACCESS a ON DATABASE TYPE BEARER FOR USER DURATION FOR BOGUS 1h, FOR \
                 SESSION NONE;"
            ),
            Diag::DdlTripwire { .. }
        ));
    }

    #[test]
    fn splits_api() {
        // The always-materialized FOR any + PERMISSIONS FULL folds to
        // an absent any; the group's FULL elides.
        let entry = split_ok(
            split_api,
            "DEFINE API '/health' FOR any PERMISSIONS FULL FOR get PERMISSIONS FULL THEN { { \
             status: 200 } } COMMENT 'note';",
        );
        assert_eq!(entry.name, "/health");
        let Stmt::Api(api) = entry.stmt else {
            panic!();
        };
        assert!(api.any.is_none(), "the absent-any rule folds");
        assert!(api.actions[0].config.permissions.is_none(), "FULL elides");
        assert_eq!(
            api.comment.map(|comment| comment.joined()),
            Some("note".to_string())
        );
        // Middleware + a fallback keep the any block present; a NONE
        // permission and a WHERE expression ride.
        let entry = split_ok(
            split_api,
            "DEFINE API '/x' FOR any MIDDLEWARE api::timeout(5s) PERMISSIONS NONE THEN { { \
             status: 404 } } FOR get, delete PERMISSIONS WHERE $auth.admin = true THEN { { \
             status: 200 } };",
        );
        let Stmt::Api(api) = entry.stmt else {
            panic!();
        };
        let any = api.any.expect("middleware keeps the block");
        assert_eq!(
            any.config.permissions,
            Some(Permission::None),
            "NONE is a deviation and rides"
        );
        assert!(any.fallback.is_some());
        assert_eq!(
            api.actions[0].methods,
            vec![ApiMethod::Get, ApiMethod::Delete]
        );
        assert!(matches!(
            api.actions[0].config.permissions,
            Some(Permission::Expr(_))
        ));
    }

    #[test]
    fn api_splitter_diagnoses_drift() {
        // FOR any always renders (even the bare form prints it).
        assert!(matches!(
            tripwire_err(split_api, "DEFINE API '/x';"),
            Diag::DdlTripwire { .. }
        ));
        // A method group's THEN is required (the printer always
        // emits it).
        assert!(matches!(
            tripwire_err(
                split_api,
                "DEFINE API '/x' FOR any PERMISSIONS FULL FOR get;"
            ),
            Diag::DdlTripwire { .. }
        ));
        // A second FOR any (impossible post-canonicalization — the
        // parser folds it) diagnoses rather than folds silently.
        assert!(matches!(
            tripwire_err(
                split_api,
                "DEFINE API '/x' FOR any PERMISSIONS FULL FOR any PERMISSIONS NONE;"
            ),
            Diag::DdlTripwire { .. }
        ));
        // The duplicate detector fires on the raw text, whichever
        // spelling the clauses use — but a path-joined `for`/`any`
        // pair (a live-printed idiom or middleware segment) is never
        // the clause. The WHERE and MIDDLEWARE assertions each carry
        // a genuine `FOR any` clause plus one path-joined pair: with
        // the guard reverted the two pairs count as a duplicate and
        // the assertion fails, so the guard is what holds them.
        assert!(has_duplicate_for_any(
            "DEFINE API '/x' FOR any PERMISSIONS FULL FOR any PERMISSIONS NONE"
        ));
        assert!(!has_duplicate_for_any(
            "DEFINE API '/x' FOR any PERMISSIONS FULL FOR get THEN { { status: 200 } }"
        ));
        assert!(!has_duplicate_for_any(
            "DEFINE API '/x' FOR any PERMISSIONS WHERE meta.for.any = true THEN { { status: \
             200 } }"
        ));
        assert!(!has_duplicate_for_any(
            "DEFINE API '/x' FOR any MIDDLEWARE fn::for::any(1) THEN { { status: 200 } }"
        ));
    }

    // -- the proptest layer (testing policy 3): mutated interchange
    // text never panics, and every failure is a well-formed SDL-E3xxx
    // diagnostic. Corpus = live-captured INFO shapes.

    const CORPUS: [&str; 22] = [
        "{ namespaces: { acme: \"DEFINE NAMESPACE acme COMMENT 'Acme'\" } }",
        "{ databases: { blog: \"DEFINE DATABASE blog COMMENT 'x' CHANGEFEED 1w\" } }",
        "{ sequences: { s: 'DEFINE SEQUENCE s BATCH 1000 START 0' } }",
        "{ params: { p: 'DEFINE PARAM $p VALUE NONE PERMISSIONS FULL' } }",
        "{ buckets: { b: \"DEFINE BUCKET b BACKEND 'memory' PERMISSIONS FULL\" } }",
        "{ config: { QUERY_TIMEOUT: NONE }, system: { cpu_usage: 0.0027f, \
         load_average: [2.7f] } }",
        "{ users: { u: \"DEFINE USER u ON ROOT PASSHASH '[REDACTED]' ROLES VIEWER DURATION \
         FOR TOKEN 1h, FOR SESSION NONE\" } }",
        "{ analyzers: { a: 'DEFINE ANALYZER a FUNCTION fn::x TOKENIZERS CLASS,PUNCT FILTERS \
         LOWERCASE, EDGENGRAM(2,10), SNOWBALL(ENGLISH)' } }",
        "{ events: { e: \"DEFINE EVENT e ON t WHEN true THEN (CREATE l SET x = 1), (UPDATE s \
         SET n += 1)\" } }",
        "{ functions: { f: 'DEFINE FUNCTION fn::f($a: int, $b: array<float, 10>) -> bool { \
         LET $y = 1; RETURN $y; } COMMENT \\\"c\\\" PERMISSIONS FULL' } }",
        "{ configs: { API: 'API MIDDLEWARE api::timeout(5s) PERMISSIONS FULL', GraphQL: \
         'GRAPHQL TABLES AUTO FUNCTIONS AUTO' } }",
        "{ defaults: { namespace: 'acme' }, modules: { m: 'DEFINE MODULE mod::m AS \
         f\\\"b:/k\\\" PERMISSIONS FULL' } }",
        "{ indexes: { i: 'DEFINE INDEX i ON t FIELDS vector HNSW DIMENSION 4 DIST EUCLIDEAN \
         TYPE F32 EFC 150 M 12 M0 24 LM 0.40242960438184466f' } }",
        "{ indexes: { i: 'DEFINE INDEX i ON t FIELDS body FULLTEXT ANALYZER like \
         BM25(1.2,0.75) HIGHLIGHTS' } }",
        "{ fields: { f: 'DEFINE FIELD matrix.* ON t TYPE int PERMISSIONS FOR select, \
         create, update NONE' } }",
        "{ tables: { t: 'DEFINE TABLE t TYPE RELATION IN a | b OUT c ENFORCED SCHEMALESS \
         PERMISSIONS NONE' } }",
        "{ tables: { lw: 'DEFINE TABLE lw TYPE RELATION IN a OUT b ENFORCED LIGHTWEIGHT \
         SCHEMALESS INLINE EDGES 8 PERMISSIONS NONE' } }",
        "{ tables: { c: \"DEFINE TABLE c TYPE NORMAL SCHEMALESS COMMENT 'x' INLINE EDGES 64 \
         INLINE REFERENCES 16 CHANGEFEED 4w2d PERMISSIONS FULL\" } }",
        "{ tables: { v: \"DEFINE TABLE v TYPE ANY SCHEMALESS COMMENT 'x' AS SELECT 1 FROM t \
         PERMISSIONS NONE\" } }",
        "{ tables: { v: 'DEFINE TABLE v TYPE NORMAL SCHEMALESS AS SELECT 1 FROM t \
         PERMISSIONS NONE' } }",
        "{ accesses: { a: \"DEFINE ACCESS a ON DATABASE TYPE RECORD SIGNIN { SELECT * FROM \
         user WHERE email = $email } WITH JWT ALGORITHM HS512 KEY '[REDACTED]' WITH ISSUER \
         KEY '[REDACTED]' DURATION FOR TOKEN 1h, FOR SESSION NONE\" } }",
        "{ apis: { '/health': \"DEFINE API '/health' FOR any PERMISSIONS FULL FOR get \
         PERMISSIONS FULL THEN { { status: 200 } } COMMENT 'health'\" } }",
    ];

    fn corrupt(text: &str, cut: usize, edits: &[(usize, u8)]) -> String {
        let mut bytes = text.as_bytes().to_vec();
        bytes.truncate(cut % (bytes.len() + 1));
        for (at, byte) in edits {
            let at = *at % (bytes.len() + 1);
            bytes.insert(at, *byte);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(512))]

        #[test]
        fn mutated_interchange_never_panics(
            which in 0usize..CORPUS.len(),
            cut in 0usize..200,
            edits in proptest::collection::vec(
                (0usize..300, proptest::sample::select(vec![
                    b'{', b'}', b':', b',', b'\'', b'"', b';', b'\\', b'a', b' ',
                ])),
                0..12,
            ),
        ) {
            let text = corrupt(CORPUS[which], cut, &edits);
            // No panics; every diagnostic well-formed interchange. Every
            // public reader rides the fan-out, high tier included.
            match read_info_object(&text) {
                Ok(info) => {
                    let _ = read_namespaces(&info);
                    let _ = read_databases(&info);
                    let _ = read_sequences(&info);
                    let _ = read_params(&info);
                    let _ = read_buckets(&info);
                    let _ = read_users(&info);
                    let _ = read_analyzers(&info);
                    let _ = read_events(&info);
                    let _ = read_functions(&info);
                    let _ = read_configs(&info);
                    let _ = read_modules(&info);
                    let _ = read_default_config(&info);
                    let _ = read_indexes(&info);
                    let _ = read_fields(&info);
                    let _ = read_tables(&info, &BTreeMap::new());
                    let _ = read_accesses(&info);
                    let _ = read_apis(&info);
                }
                Err(diag) => proptest::prop_assert!(
                    matches!(
                        diag,
                        Diag::InfoUnparsable { .. } | Diag::InfoUnknownShape { .. }
                    ),
                    "diagnostic outside the interchange-rejection family: {diag:?}"
                ),
            }
        }

        #[test]
        fn arbitrary_text_never_panics(text in ".*") {
            let _ = read_info_object(&text);
        }
    }
}
