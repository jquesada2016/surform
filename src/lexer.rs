//! Hand-written SDL lexer (spec rule 3): depth-0 `;` statement
//! splitting, capture-mode spans for expression clauses, skip set =
//! strings (six prefixes × two quote styles), four SurrealQL comment
//! syntaxes, quoted identifiers.
//!
//! This is not a tokenizer. The parser drives the [`Scanner`] and
//! decides when a capture begins — brace classification is
//! context-dependent (`key: {` starts a SurrealQL capture; `key {`
//! stays in SDL parsing), so no context-free token stream can express
//! it. The scanner only offers skip-set-aware primitives over the
//! byte stream.
//!
//! Where the scanner reads SurrealQL text (inside captures) it mirrors
//! v3.2.4's `syn/lexer` byte for byte: string prefixes are lowercase
//! `d s u b f r` with either quote style, and `\` skips the next
//! character in *every* string — raw `r'…'` included — because
//! `eat_string_like` is prefix-agnostic (escape *contents* are the
//! embedded parser's business, scanning is uniform). Line comments
//! (`--`, `//`, `#`) end at LF, CRLF, lone CR, U+2028/U+2029/U+0085;
//! `/* … */` ends at the first `*/` and unterminated is an error;
//! quoted identifiers close at the first unescaped `` ` `` or `⟩`;
//! bare words are `[A-Za-z_][A-Za-z0-9_]*`; whitespace is
//! `eat_whitespace`'s set (ASCII SP/TAB/CR/FF/LF/VT plus the Unicode
//! space list). Outside captures SDL trivia is that whitespace plus
//! `//` comments; `///` doc blocks belong to the parser (rule 5), so
//! [`Scanner::skip_trivia`] stops at one.

use crate::diag::Diag;

mod keywords;

use keywords::{is_clause_expr_head, is_reserved_word};

/// A half-open byte range in the source file. Offsets are byte
/// positions (miette derives line/column from the source when
/// rendering); `len` is in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub offset: usize,
    pub len: usize,
}

impl Span {
    pub fn new(offset: usize, len: usize) -> Self {
        Span { offset, len }
    }

    /// A zero-length mark (renders as a caret under a label).
    pub fn point(offset: usize) -> Self {
        Span { offset, len: 0 }
    }

    pub fn end(&self) -> usize {
        self.offset + self.len
    }

    pub fn slice<'a>(&self, src: &'a str) -> &'a str {
        &src[self.offset..self.end()]
    }
}

/// The 1-indexed (line, char-column) position of a byte offset in
/// `source` — the public face of the diagnostic anchor semantics
/// (columns count chars, not bytes; non-boundary offsets floor
/// forward to the next char edge; the arithmetic is `expr.rs`'s
/// `Position`). The wasm bindings' JSON report converts spans through
/// this so the browser's squiggles sit where miette's carets do.
pub fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let position = crate::expr::line_col_at(source, offset);
    (position.line, position.column)
}

/// Repairs the embedded printer's module-call spelling in a printed
/// dialect text: upstream's `FunctionCall` printer emits a stray
/// space after the `mod::` prefix (`mod:: name::fn(…)` — the
/// `Function::Module` arm's `" {}"` in surrealdb-sql 3.3.0's
/// `function.rs`, the one spelling its sibling arms — and `Function`'s
/// own `to_string` — do not carry). The engine tokenizes the space
/// away, so the DDL applies either way, but fmt-canonical source
/// reads as a typo and the engine's own INFO print carries the quirk
/// onto the reverse path. The repair removes exactly the quirk's byte
/// pattern (`mod::` + one space) outside the printer dialect's opaque
/// spans: the capture skip set ([`Scanner::skip_skip_set`] — strings
/// with their six prefixes, comments, quoted identifiers) plus regex
/// literals, which printer output delimits unambiguously
/// ([`Scanner::skip_regex_literal`] — interior slashes are escaped;
/// the capture engines cannot skip regex in hand-written text, but
/// this function's inputs are printer outputs). Inputs are printer
/// outputs; any surprise — a span that never closes — abandons the
/// repair and returns the text whole: the quirk is cosmetic, and a
/// repair must never break a working print. Script (`function(…) { …
/// }`) bodies print verbatim with their JS interior, which this scan
/// reads as SDL: JS comments open phantom spans and a JS `--` rides
/// the comment arm — the worst case is a missed repair or the whole
/// abandonment, never a corrupted span (skip arms copy verbatim).
pub(crate) fn repair_module_call_spaces(text: &str) -> String {
    if !text.contains("mod:: ") {
        return text.to_string();
    }
    let mut scanner = Scanner::new(text);
    let mut out = Vec::with_capacity(text.len());
    while let Some(b) = scanner.peek_byte() {
        let start = scanner.offset();
        match scanner.skip_regex_literal(0) {
            SkipOutcome::Skipped => {
                out.extend_from_slice(&text.as_bytes()[start..scanner.offset()]);
                continue;
            }
            // An opened span that never closes is not printer
            // output — abandon the repair whole.
            SkipOutcome::Unterminated => return text.to_string(),
            SkipOutcome::None => {}
        }
        match scanner.skip_skip_set() {
            Ok(true) => {
                out.extend_from_slice(&text.as_bytes()[start..scanner.offset()]);
                continue;
            }
            // An unterminated string or comment: not printer output —
            // abandon whole.
            Err(_) => return text.to_string(),
            Ok(false) => {}
        }
        if b == b'm' && scanner.rest().starts_with("mod:: ") {
            out.extend_from_slice(b"mod::");
            scanner.advance_by(6);
            continue;
        }
        // One char of content, advanced whole — a mid-char cursor
        // would trip the span scanners' `rest()` slices on the next
        // iteration. A continuation byte can never open the pattern
        // or a span, so the char-level copy stays byte-exact.
        let ch = text[start..]
            .chars()
            .next()
            .expect("peek_byte returned a byte");
        out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
        scanner.advance_by(ch.len_utf8());
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

/// The bracket pair a balanced capture opens and closes with —
/// braces for statement bodies (`fn`, `raw`), parens for lists
/// (`fn`'s parameter list, whose kinds carry `,`s inside `<>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Brackets {
    Braces,
    Parens,
}

/// What ended a [`Scanner::capture_until_word`] capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureStop {
    /// A depth-0 bare word matched one of the stop words — the cursor
    /// rests on it, unconsumed; the payload is the matched text as it
    /// appears in the source.
    Word(String),
    /// A depth-0 `;` — consumed.
    Semicolon,
    /// End of source.
    End,
}

/// What an opaque-span skip attempt did — [`Scanner::skip_regex_literal`]'s
/// tri-state: the byte opened no span ([`SkipOutcome::None`]), a span
/// rode past whole ([`SkipOutcome::Skipped`]), or a span opened and
/// never closed ([`SkipOutcome::Unterminated`] — printer output
/// always closes its spans, so the text is not a print).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipOutcome {
    None,
    Skipped,
    Unterminated,
}

/// What must follow a stop word for it to end a
/// [`Scanner::capture_until_word`] capture: a clause keyword inside
/// an EXPRESSION-valued capture only ends the value when the shape of
/// its own value confirms it (`COMMENT` before a `'`-string, say) —
/// otherwise the word is expression content and the capture continues
/// past it. Atom-valued captures (whose closed vocabularies can never
/// contain a clause keyword) use [`CaptureFollow::Any`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFollow {
    /// The word ends the capture unconditionally.
    Any,
    /// A VALUE token must open (the `COMMENT` clauses' follow): a
    /// quote, `$`, a bracket, a digit, a sign, or a word that is
    /// neither a SurrealQL operator (`IN`, `AND`, … — expression
    /// content continues through those) nor one of the capture's own
    /// stop words (that would be the NEXT clause — this candidate is
    /// content, and the capture stops there instead). A prefixed
    /// string, `NONE`, `true`, or a function path all qualify — the
    /// capture's clause reader decides plain-string vs the raw route.
    Value,
    /// A permission value must open — the word `NONE`, `FULL`, or
    /// `WHERE` (the `PERMISSIONS` clause's single-value form), or
    /// `FOR` (the printer merges equal-valued operations into
    /// `FOR <ops> <value>` groups).
    Perm,
    /// An ASCII digit (the DEPTH/COMPLEXITY atoms).
    Digit,
    /// One of these words (the graphql family words after
    /// `FUNCTIONS`; the introspection words).
    Word(&'static [&'static str]),
    /// A string literal must open — the always-a-string clause values
    /// (`COMMENT`). Unlike [`CaptureFollow::Value`]'s word branch, a
    /// bare word never confirms (`SELECT comment FROM post`'s field
    /// ident is content, not the COMMENT clause); a prefixed string's
    /// single-letter opener (`d"`, `s'`) confirms too, so the clause
    /// reader can route it.
    String,
    /// A then-item opens: `(` or `{` SPACED (event `THEN`'s statement
    /// list), or a literal item — digit- or quote-led (`THEN 1`,
    /// `THEN 'x'`; the printer emits bare literals).
    ThenItem,
}

/// One [`Scanner::capture_until_word`] stop: the clause keyword and
/// the follow shape that confirms it.
#[derive(Debug, Clone, Copy)]
pub struct CaptureStopWord(pub &'static str, pub CaptureFollow);

/// The SurrealQL operator words that can follow an operand inside an
/// expression — a candidate stop word followed by one of these is
/// expression content, never a clause ([`CaptureFollow::Value`]'s
/// word branch). The complete 3.2.4 infix word set; the `matches*`
/// entries are tolerance for the `@@` operator's word spellings,
/// which the pinned printer never emits.
const OPERATOR_WORDS: &[&str] = &[
    "in",
    "and",
    "or",
    "not",
    "is",
    "contains",
    "containsnot",
    "containsall",
    "containsany",
    "containsnone",
    "inside",
    "notinside",
    "allinside",
    "anyinside",
    "noneinside",
    "outside",
    "intersects",
    "matches",
    "matchesall",
    "matchesany",
];

/// Whether `word` (lowercased) is one of [`OPERATOR_WORDS`].
fn is_operator_word(word: &str) -> bool {
    OPERATOR_WORDS.contains(&word.to_ascii_lowercase().as_str())
}

/// The stop-word lookahead's recursion cap ([`Scanner::word_follows`]):
/// real clause chains are one stop word deep; anything past this is
/// garbage input, and unconfirmable means content — the capture
/// continues and fails closed downstream.
const FOLLOW_DEPTH_CAP: usize = 8;

/// Whether `b` is a whitespace byte (the sign-vs-binary-minus glue
/// probe's trivia test).
fn is_trivia_byte(b: Option<u8>) -> bool {
    matches!(b, Some(b' ' | b'\t' | b'\n' | b'\r'))
}

/// Whether the `/` at byte `at` OPENS a regex literal in printer
/// output — the engine's prime-vs-infix discrimination, closed into a
/// predecessor test for the capture engine (the full contract, and
/// the vendored citations, live on [`Scanner::skip_regex_literal`]).
/// `scan_start` bounds the walk-back at the scan's own beginning (a
/// statement capture's first byte; 0 for whole-text scans), so a
/// statement's verdict never reads the previous statement's tokens.
fn printer_regex_opens(src: &str, at: usize, scan_start: usize) -> bool {
    let bytes = src.as_bytes();
    // Walk back over whitespace to the previous significant byte.
    let mut back = at;
    while back > scan_start && matches!(bytes[back - 1], b' ' | b'\t' | b'\n' | b'\r') {
        back -= 1;
    }
    // Nothing significant between the scan's start and the candidate:
    // prime position (the expression's first token).
    if back == scan_start {
        return true;
    }
    let prev = bytes[back - 1];
    match prev {
        // Operand ends — a complete operand precedes, so the parser is
        // in infix position and the slash is division.
        b')' | b']' | b'}' | b'\'' | b'"' | b'`' => false,
        // A regex literal's end anchor — `$` immediately before the
        // candidate means the candidate CLOSES a regex (`/^[a-z]+$/`),
        // a complete operand, infix follows, division. (A bare `$`
        // otherwise appears only as the param sigil — and `$param`
        // reaches the word arm, not this byte.) The hand-written
        // capture's regression guard: without it, a
        // division-classified regex's closer re-read as an OPENER and
        // consumed the statement tail (issue #60's follow-up).
        b'$' => false,
        _ if src[..back].ends_with('⟩') => false,
        // A number's last digit (digit-led durations included — `2s`
        // walks back to a word that spells no keyword).
        b'0'..=b'9' => false,
        // Another slash: LEFT-SPACED (or at the scan's bound) is the
        // division OPERATOR itself — the candidate is its fresh right
        // operand, prime position, regex open. LEFT-GLUED is a regex
        // literal's closer — the regex is a complete operand, infix
        // follows, division.
        b'/' => {
            !(back >= scan_start + 2 && !matches!(bytes[back - 2], b' ' | b'\t' | b'\n' | b'\r'))
        }
        // A bare word: identifier-shaped. Sigil-introduced segments
        // (`$param`, `mod::seg`, `math::seg`, idiom `.part`) print
        // bare even when keyword-spelled (the printer's
        // `EscapeKwFreeIdent`) and complete their operand — division.
        // The operand-valued keyword literals (`NONE`, `NULL`,
        // `true`, `false` — upstream's `Literal` print) are complete
        // operands too — tested FIRST, before the reserved set they
        // belong to: the printer emits them as literal VALUES, not as
        // escaped identifiers. Every other bare word mirrors the
        // printer's OWN escape decision (`EscapeIdent` →
        // `could_be_reserved`, mirrored in [`crate::lexer::keywords`]):
        // the printer backticks identifier collisions against the
        // 44-word RESERVED set only, so a bare reserved spelling IS
        // the keyword, and the expression-leading clause heads and
        // infix operator words are syntax positions whose
        // continuation expects an operand — a regex may open. Every
        // other spelling — `count`, `comment`, `set` — is an
        // IDENTIFIER operand (the engine accepts every keyword token
        // as an ident, `parser/token.rs` `kind_is_identifier`); the
        // operand just ended, division. Position is recursive: a
        // syntax word whose OWN predecessor expects an operand is
        // itself the ident (`VALUE and / 2`, `VALUE default / 2` —
        // the word follows the VALUE clause), resolved by
        // [`word_opens_after`].
        b if b.is_ascii_alphanumeric() || b == b'_' => {
            let mut word_at = back - 1;
            while word_at > scan_start
                && (bytes[word_at - 1].is_ascii_alphanumeric() || bytes[word_at - 1] == b'_')
            {
                word_at -= 1;
            }
            let sigil = word_at > scan_start && matches!(bytes[word_at - 1], b'$' | b':' | b'.');
            if sigil {
                return false;
            }
            let word = &src[word_at..back];
            // The operand-valued keyword literals — the printer's
            // spellings (`NONE`/`NULL` uppercase, `true`/`false`
            // lowercase), matched as the engine matches keywords,
            // ASCII case-insensitively.
            if matches!(
                word.to_ascii_lowercase().as_str(),
                "none" | "null" | "true" | "false"
            ) {
                return false;
            }
            word_opens_after(src, word, word_at, scan_start)
        }
        // Everything else — the operator bytes, `,`/`:`/`(`/`[`/`{`,
        // a statement's `;`, any multibyte content — leaves the parser
        // expecting an operand: prime position, regex open.
        _ => true,
    }
}

/// The word arm's recursion cap ([`word_opens_after`]): each level
/// strictly walks back, and a canonical statement's plain-word run is
/// a handful of words deep (`DEFINE FIELD OVERWRITE pad ON t TYPE
/// regex DEFAULT` bottoms out at seven). Past the cap the chain is
/// UNCONFIRMABLE, and the candidate reads as infix-position
/// ([`WordPosition::AfterOperand`] — the cap arm's return): a plain
/// word there is the operand and the slash divides, but a
/// clause-head or operator-word spelling there OPENS the regex (the
/// merge-capable direction — [`word_opens_after`]'s AfterOperand
/// arm), the [`FOLLOW_DEPTH_CAP`] convention. Unreachable from
/// canonical output: the deepest chains bottom out well under the
/// cap.
const WORD_DEPTH_CAP: u8 = 16;

/// Where the parser sits AT a word: expecting an operand (the word is
/// being read as one — the engine accepts every keyword token as an
/// ident), or just past a complete operand (infix/clause position,
/// where the word's own class decides what it is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordPosition {
    ExpectsOperand,
    AfterOperand,
}

/// The word arm's verdict: whether a REGEX may open after the bare
/// `word` spelled at `word_at` — i.e. whether the word sits in SYNTAX
/// position (a reserved word, expression-leading clause head, or
/// infix operator word, whose continuation expects an operand) or
/// completed an OPERAND (the `/` is infix — division). Position is
/// recursive ([`word_position`]): a syntax word whose own predecessor
/// expects an operand is itself the ident, which is how
/// `VALUE and / 2` and `VALUE default / 2` stay divisions while
/// `x and /a;b/` and `DEFAULT / */` open.
fn word_opens_after(src: &str, word: &str, word_at: usize, scan_start: usize) -> bool {
    // A bare RESERVED spelling can never take the operand reading:
    // the printer backticks identifier collisions against exactly
    // this set (`EscapeIdent` → `could_be_reserved`), so a bare
    // occurrence IS the keyword, whatever position it sits in. (The
    // operand-valued literals among them — `NONE` `NULL` `true`
    // `false` — never reach here: the word arm classified them
    // first.)
    if is_reserved_word(word) {
        return true;
    }
    match word_position(src, word_at, scan_start, 0) {
        WordPosition::ExpectsOperand => false,
        WordPosition::AfterOperand => is_operator_word(word) || is_clause_expr_head(word),
    }
}

/// Where the parser sits at the word ENDING at `at` — decided PURELY
/// by what precedes it (the same predecessor classes as
/// [`printer_regex_opens`], one level up; the whitespace walk-back is
/// bounded at the scan's own start, so a statement's verdict never
/// reads the previous statement's tokens). What the word itself
/// spells is its CALLER's question — a syntax word in operand
/// position is the ident ([`word_opens_after`]'s division case), and
/// the recursion consumes exactly this: an operand before a word
/// puts the word in infix/clause position, and a syntax word before
/// it puts the word in operand position.
fn word_position(src: &str, at: usize, scan_start: usize, depth: u8) -> WordPosition {
    let bytes = src.as_bytes();
    let mut back = at;
    while back > scan_start && matches!(bytes[back - 1], b' ' | b'\t' | b'\n' | b'\r') {
        back -= 1;
    }
    // Nothing significant before the word: prime position — the word
    // is the expression's first token, an operand.
    if back == scan_start {
        return WordPosition::ExpectsOperand;
    }
    match bytes[back - 1] {
        // A complete operand precedes the word: infix/clause position.
        b')' | b']' | b'}' | b'\'' | b'"' | b'`' => WordPosition::AfterOperand,
        // A regex literal's end anchor (the word arm's `$`-guard's
        // mirror — see [`printer_regex_opens`]).
        b'$' => WordPosition::AfterOperand,
        _ if src[..back].ends_with('⟩') => WordPosition::AfterOperand,
        b'0'..=b'9' => WordPosition::AfterOperand,
        // A slash: LEFT-SPACED (or at the scan's bound) is the
        // division operator — the word is its fresh right operand.
        // LEFT-GLUED is a regex literal's closer — a complete operand.
        b'/' if back < scan_start + 2
            || matches!(bytes[back - 2], b' ' | b'\t' | b'\n' | b'\r') =>
        {
            WordPosition::ExpectsOperand
        }
        b'/' => WordPosition::AfterOperand,
        // Another bare word: a sigil-introduced segment or one of the
        // operand-valued keyword literals is a complete operand; a
        // syntax word (operator, clause head, reserved) continues
        // into an operand; a plain word takes its OWN position — an
        // operand there makes THIS word infix-position content,
        // otherwise the operand-expected position carries past it.
        _ if bytes[back - 1].is_ascii_alphanumeric() || bytes[back - 1] == b'_' => {
            let mut word_at = back - 1;
            while word_at > scan_start
                && (bytes[word_at - 1].is_ascii_alphanumeric() || bytes[word_at - 1] == b'_')
            {
                word_at -= 1;
            }
            let sigil = word_at > scan_start && matches!(bytes[word_at - 1], b'$' | b':' | b'.');
            if sigil {
                return WordPosition::AfterOperand;
            }
            let predecessor = &src[word_at..back];
            if matches!(
                predecessor.to_ascii_lowercase().as_str(),
                "none" | "null" | "true" | "false"
            ) {
                return WordPosition::AfterOperand;
            }
            if is_operator_word(predecessor)
                || is_clause_expr_head(predecessor)
                || is_reserved_word(predecessor)
            {
                return WordPosition::ExpectsOperand;
            }
            // Past the cap the chain is unconfirmable: the candidate
            // reads as infix-position (after an operand) — a plain
            // word there is the operand and the slash divides, but a
            // clause-head/operator-word spelling there OPENS the
            // regex (see [`WORD_DEPTH_CAP`]), the
            // [`FOLLOW_DEPTH_CAP`] convention. Unreachable from
            // canonical output.
            if depth >= WORD_DEPTH_CAP {
                return WordPosition::AfterOperand;
            }
            match word_position(src, word_at, scan_start, depth + 1) {
                WordPosition::ExpectsOperand => WordPosition::AfterOperand,
                WordPosition::AfterOperand => WordPosition::ExpectsOperand,
            }
        }
        // Every other byte — the operator bytes, `,`/`:`/`(`/`[`/`{`,
        // a statement's `;`, any multibyte content — leaves the parser
        // expecting an operand.
        _ => WordPosition::ExpectsOperand,
    }
}

/// Skip-set-aware scanner over SDL source; see the module docs.
pub struct Scanner<'src> {
    src: &'src str,
    pos: usize,
    /// When set, the capture engine records every plain `//` comment
    /// it skips (rule 5's preservation model — a caller scanning a
    /// span whose comments must survive flips this on and drains the
    /// log; see [`Self::set_record_comments`] and
    /// [`Self::take_recorded_comments`]).
    record_comments: bool,
    comment_log: Vec<Span>,
}

impl<'src> Scanner<'src> {
    pub fn new(src: &'src str) -> Self {
        Scanner {
            src,
            pos: 0,
            record_comments: false,
            comment_log: Vec::new(),
        }
    }

    /// Comment recording during captures: off by default (a capture's
    /// comments drop with its canon), on for spans whose comments the
    /// parser re-anchors (field-kind captures — the kind is re-printed
    /// from the SDL kind AST, so there is no canonicalization story
    /// that could eat them silently).
    pub fn set_record_comments(&mut self, on: bool) {
        self.record_comments = on;
    }

    /// Drains the recorded `//` comments (whole-comment spans) and
    /// clears the log.
    pub fn take_recorded_comments(&mut self) -> Vec<Span> {
        std::mem::take(&mut self.comment_log)
    }

    /// Current byte offset (always a character boundary).
    pub fn offset(&self) -> usize {
        self.pos
    }

    /// Repositions the cursor to a former [`Self::offset`] of this
    /// scan (the parser's close-note peek restores its position when
    /// no same-line comment follows a terminator — the one caller;
    /// offsets always come from this same scanner, so the cursor can
    /// never land mid-character).
    pub fn seek(&mut self, offset: usize) {
        self.pos = offset;
    }

    pub fn is_at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    /// Advances `n` bytes — a character boundary the caller's
    /// consumed-prefix arithmetic provides (the kind reader of the
    /// function splitter).
    pub(crate) fn advance_by(&mut self, n: usize) {
        self.pos += n;
    }

    pub fn peek_byte(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    /// Consumes `b` if it is next, reporting whether it was.
    pub fn eat_byte(&mut self, b: u8) -> bool {
        if self.peek_byte() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Skips whitespace and Unicode spaces, stopping at any `//` line
    /// comment — plain or doc — and at any other byte or end of
    /// source. The parser consumes both comment kinds through their
    /// own entry points ([`Self::at_doc_comment`] plus
    /// [`Self::take_doc_block`], [`Self::at_plain_comment`] plus
    /// [`Self::take_comment`]). Infallible: `--`, `#`, and `/*` are
    /// not SDL trivia, so they simply stop the skip and the parser
    /// reports them.
    pub fn skip_trivia(&mut self) {
        loop {
            match self.peek_byte() {
                Some(b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c') => self.pos += 1,
                Some(_) => {
                    let width = self.unicode_space_len();
                    if width == 0 {
                        return;
                    }
                    self.pos += width;
                }
                None => return,
            }
        }
    }

    /// Skips SurrealQL trivia — the trivia model *inside* rule 3's
    /// captures, where all four SurrealQL comment syntaxes are legal
    /// input (`--`, `//`, `#` line comments and `/* … */` blocks),
    /// unlike the SDL file body's `//`-only model
    /// ([`Self::skip_trivia`]; outside captures `--`, `#`, and `/*`
    /// are not comments). Sub-scanners over capture text (fn-paths,
    /// identifier clauses) use this, never [`Self::skip_trivia`].
    pub fn skip_surrealql_trivia(&mut self) {
        loop {
            match self.peek_byte() {
                Some(b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c') => self.pos += 1,
                Some(b'/') if self.rest().starts_with("//") => {
                    self.pos += 2;
                    self.skip_line_comment();
                }
                Some(b'/') if self.rest().starts_with("/*") => {
                    self.pos += 2;
                    // An unterminated block comment cannot reach a
                    // sub-scanner: the capture scanner's skip set
                    // already rejected it. Stopping on one leaves the
                    // cursor for the caller's "expected …" diagnostic.
                    match self.rest().find("*/") {
                        Some(at) => self.pos += at + 2,
                        None => return,
                    }
                }
                Some(b'-') if self.rest().starts_with("--") => {
                    self.pos += 2;
                    self.skip_line_comment();
                }
                Some(b'#') => {
                    self.pos += 1;
                    self.skip_line_comment();
                }
                Some(_) => {
                    let width = self.unicode_space_len();
                    if width == 0 {
                        return;
                    }
                    self.pos += width;
                }
                None => return,
            }
        }
    }

    /// Skips one member of the capture skip set — strings (six
    /// prefixes × two quote styles), the four SurrealQL comment
    /// syntaxes, quoted identifiers (backticks, `⟨…⟩`). `Ok(true)`
    /// leaves the cursor past the member; `Ok(false)` leaves it
    /// unmoved when the byte opens none. This is the skip set's one
    /// home: both capture engines ([`Self::scan_capture`] and
    /// [`Self::capture_until_word_inner`]) and the embedded-print
    /// repair ([`repair_module_call_spaces`]) skip identical shapes
    /// through it.
    fn skip_skip_set(&mut self) -> Result<bool, Diag> {
        let Some(b) = self.peek_byte() else {
            return Ok(false);
        };
        match b {
            b'd' | b's' | b'u' | b'b' | b'f' | b'r'
                if matches!(self.peek_ahead(1), Some(b'\'' | b'"')) =>
            {
                let opener = Span::new(self.pos, 2);
                let quote = self.peek_ahead(1).expect("guard checked");
                self.pos += 2;
                self.skip_string_body(quote, opener)?;
                Ok(true)
            }
            b'\'' | b'"' => {
                let opener = Span::new(self.pos, 1);
                self.pos += 1;
                self.skip_string_body(b, opener)?;
                Ok(true)
            }
            b'`' => {
                let opener = Span::new(self.pos, 1);
                self.pos += 1;
                self.skip_quoted_ident('`', opener)?;
                Ok(true)
            }
            _ if self.rest().starts_with('⟨') => {
                let opener = Span::new(self.pos, '⟨'.len_utf8());
                self.pos += opener.len;
                self.skip_quoted_ident('⟩', opener)?;
                Ok(true)
            }
            b'-' if self.peek_ahead(1) == Some(b'-') => {
                self.pos += 2;
                self.skip_line_comment();
                Ok(true)
            }
            b'/' if self.peek_ahead(1) == Some(b'/') => {
                let start = self.pos;
                self.pos += 2;
                self.skip_line_comment();
                // Recording captures re-anchor plain `//` comments
                // (rule 5); `///` stays trivia (doc blocks are
                // top-level only and never legal in a capture).
                if self.record_comments && !self.src[start..].starts_with("///") {
                    self.comment_log.push(Span::new(start, self.pos - start));
                }
                Ok(true)
            }
            b'#' => {
                self.pos += 1;
                self.skip_line_comment();
                Ok(true)
            }
            b'/' if self.peek_ahead(1) == Some(b'*') => {
                let opener = Span::new(self.pos, 2);
                self.pos += 2;
                self.skip_block_comment(opener)?;
                Ok(true)
            }
            // A regex literal's interior (issue #60): hand-written SDL
            // carries them (an apostrophe inside a regex opened a
            // phantom string and cascaded), and the prime-vs-infix
            // discrimination is the printer repair's own predecessor
            // test — SHARED, so hand-written and printer text classify
            // identically. `None` is the DIVISION verdict (the cursor
            // never moved — the byte is plain content); an unterminated
            // span fails the capture closed, anchored at the opening
            // `/` (the parse refuses the statement anyway; the
            // scanner's job is to not cascade).
            b'/' => {
                let opener = Span::new(self.pos, 1);
                match self.skip_regex_literal(0) {
                    SkipOutcome::Skipped => Ok(true),
                    SkipOutcome::Unterminated => Err(Diag::UnterminatedRegex { span: opener }),
                    SkipOutcome::None => Ok(false),
                }
            }
            _ => Ok(false),
        }
    }

    /// Skips one regex literal in PRINTER output: `/pattern/`, the
    /// interior's slashes escaped `\/` (upstream's `Literal::Regex`
    /// print, surrealdb-sql-3.3.0 `literal.rs` — only slashes are
    /// escaped, so an interior may lead with whitespace), and closes
    /// at the first unescaped `/`.
    ///
    /// Whether the candidate `/` OPENS one is the engine's own
    /// prime-vs-infix discrimination, closed into a predecessor test.
    /// Ground truth: the vendored parser opens a regex at `t!("/")` in
    /// PRIME (operand-expected) position regardless of the following
    /// byte (`surrealdb-syn-3.3.0` `parser/prime.rs` — the lexer emits
    /// plain `/`, `lexer/byte.rs`), and parses it as
    /// `BinaryOperator::Divide` only in the infix loop — after a
    /// complete operand (`parser/expression.rs` `parse_infix_op`).
    /// Spacing is irrelevant to the engine either way, so the old
    /// right-glue heuristic (a space-led interior read as division)
    /// misclassified exactly the space-led regex a regex-typed field
    /// prints (`DEFAULT / */` — the trailing-space pattern), and its
    /// closing `/` then opened a bogus literal that swallowed the
    /// statement tail. The closed contract for printer
    /// output — the DIVISION predecessor classes enumerated from the
    /// vendored source, everything else regex-opens ([`printer_regex_opens`]
    /// implements it; `scan_start` is this scan's own start):
    ///
    /// - DIVISION — the previous significant byte (whitespace walked
    ///   back, bounded at the scan start) is an operand END: a closer
    ///   `)` `]` `}`; a closed string `'` `"` or quoted ident `` ` ``
    ///   `⟩`; a digit (numbers, digit-led durations); a left-GLUED
    ///   `/` (a regex's closer — the regex is a complete operand); a
    ///   sigil-introduced bare word (`$param`, `mod::seg`,
    ///   `math::seg`, idiom `.part` — the printer's `EscapeKwFreeIdent`
    ///   emits these bare even when keyword-spelled); the
    ///   operand-valued keyword literals `NONE` `NULL` `true`
    ///   `false` (reserved spellings, but the printer emits them as
    ///   literal VALUES, not as escaped identifiers); or any bare
    ///   word sitting where the parser expects an operand — which,
    ///   per the printer's own escape decision, is EVERY word
    ///   outside the syntax classes AND every syntax-class spelling
    ///   whose own predecessor expects an operand ([`word_opens_after`]'s
    ///   recursion): `count`/`comment`/`set` are identifiers (the
    ///   printer escapes identifier collisions ONLY against
    ///   `could_be_reserved`, the 44-word RESERVED set,
    ///   [`crate::lexer::keywords`]; the engine accepts every
    ///   keyword token as an ident, `parser/token.rs`
    ///   `kind_is_identifier`), and so are the clause-head and
    ///   operator spellings that follow an operand (`VALUE and / 2`,
    ///   `VALUE default / 2`).
    /// - REGEX OPEN — everything else: the scan's start; the operator
    ///   bytes; `,`/`:`/`(`/`[`/`{`; a left-SPACED `/` (the division
    ///   operator — the candidate is its fresh right operand); and a
    ///   bare word in SYNTAX position — a RESERVED spelling (bare =
    ///   the keyword: the printer escapes identifier collisions
    ///   against exactly this set; `WHERE` is in it, so a
    ///   WHERE-position regex opens), or — after a complete operand —
    ///   an expression-leading clause head or infix operator word
    ///   ([`crate::lexer::keywords::CLAUSE_EXPR_HEADS`], the lexer's
    ///   `OPERATOR_WORDS`), whose continuation expects an operand
    ///   (`DEFAULT / */`, `x and /a;b/`).
    ///
    /// The one unresolvable compound — an operand ident spelled like
    /// a clause head or operator word dividing BY a space-led regex
    /// (`DEFAULT default / */`, a field named `default`) — classifies
    /// the FIRST slash as DIVISION (the ident reading wins, pinned at
    /// tracker #62): the merge then comes second-hand, when the
    /// pattern's CLOSING slash — unconsumed, since no literal opened —
    /// sits in operand-expected position and opens a bogus literal
    /// that swallows statement boundaries to the next unescaped `/`.
    /// The recursion spends the ambiguity on the single-event shapes,
    /// and the compound (two rare events overlapping) is ledgered
    /// (tracker #62).
    ///
    /// Hand-written captures route through this too since #60: the
    /// capture skip set's `/` arm shares this exact discrimination,
    /// so hand-written and printer text classify identically.
    /// [`SkipOutcome::None`] leaves the cursor unmoved when the byte
    /// opens no regex (division); a skipped literal closes at its
    /// first unescaped `/` (`\` escapes one byte, clamped at EOF);
    /// [`SkipOutcome::Unterminated`] — no closing `/` before EOF —
    /// fails the CAPTURE closed ([`Diag::UnterminatedRegex`],
    /// SDL-E0001, anchored at the opening `/`; the engine's own regex
    /// lexer errors at EOF — `lexer/compound/regex.rs` — the
    /// conservative direction). The consumers are the compile-side
    /// repair engine ([`repair_module_call_spaces`]) and, since #60,
    /// the capture skip set's `/` arm; the apply flow no longer
    /// re-derives statement boundaries at all — the convergence
    /// emitter marks them at emission.
    fn skip_regex_literal(&mut self, scan_start: usize) -> SkipOutcome {
        if self.peek_byte() != Some(b'/') {
            return SkipOutcome::None;
        }
        if !printer_regex_opens(self.src, self.pos, scan_start) {
            return SkipOutcome::None;
        }
        self.pos += 1;
        while let Some(b) = self.peek_byte() {
            if b == b'\\' {
                // A trailing `\` at EOF clamps at the text's end (the
                // escape has nothing to skip; the Unterminated verdict
                // fires with the cursor in bounds — a pos past `len`
                // would slice out of every later `rest()`).
                self.pos = (self.pos + 2).min(self.src.len());
            } else if b == b'/' {
                self.pos += 1;
                return SkipOutcome::Skipped;
            } else {
                self.pos += 1;
            }
        }
        SkipOutcome::Unterminated
    }

    /// True if a `///` doc comment starts here.
    pub fn at_doc_comment(&self) -> bool {
        self.rest().starts_with("///")
    }

    /// True if a plain `//` comment starts here (rule 5's layout
    /// preservation — the parser captures these for the writer).
    pub fn at_plain_comment(&self) -> bool {
        self.rest().starts_with("//") && !self.rest().starts_with("///")
    }

    /// Consumes one plain `//` comment to end of line and returns its
    /// preserved text: everything after `//` with trailing whitespace
    /// trimmed, leading spacing verbatim (`// note` keeps its space,
    /// `//note` keeps none — the writer re-emits `//` plus this text
    /// byte-identically). The caller checked [`Self::at_plain_comment`].
    pub fn take_comment(&mut self) -> String {
        debug_assert!(self.at_plain_comment());
        self.pos += 2;
        let text_start = self.pos;
        while self.peek_byte().is_some() && self.line_end_len() == 0 {
            self.advance();
        }
        self.src[text_start..self.pos].trim_end().to_string()
    }

    /// Consumes a contiguous `///` block (rule 5) and returns one span
    /// per line's doc text: everything after `///` and one leading
    /// space, with trailing whitespace trimmed. Lines are adjacent —
    /// a blank or non-doc line ends the block — but may be indented
    /// with spaces or tabs (rule 7: indentation is cosmetic; wider
    /// trivia such as VT or Unicode spaces do not continue a block).
    /// Codegen joins the lines with `\n` into the definition's
    /// `COMMENT`.
    pub fn take_doc_block(&mut self) -> Vec<Span> {
        let mut lines = Vec::new();
        while self.rest().starts_with("///") {
            self.pos += 3;
            self.eat_byte(b' ');
            let text_start = self.pos;
            while self.peek_byte().is_some() && self.line_end_len() == 0 {
                self.advance();
            }
            let text = self.src[text_start..self.pos].trim_end();
            lines.push(Span::new(text_start, text.len()));
            match self.line_end_len() {
                0 => break,
                width => {
                    self.pos += width;
                    let line_start = self.pos;
                    while self.peek_byte().is_some_and(|b| b == b' ' || b == b'\t') {
                        self.pos += 1;
                    }
                    if !self.rest().starts_with("///") {
                        self.pos = line_start;
                    }
                }
            }
        }
        lines
    }

    /// Reads a bare word: `[A-Za-z_][A-Za-z0-9_]*` (SurrealQL's bare
    /// identifier charset). Returns `None` without consuming if the
    /// next byte cannot start one.
    pub fn read_word(&mut self) -> Option<Span> {
        let start = self.pos;
        let b = self.peek_byte()?;
        if !(b.is_ascii_alphabetic() || b == b'_') {
            return None;
        }
        self.pos += 1;
        while self
            .peek_byte()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            self.pos += 1;
        }
        Some(Span::new(start, self.pos - start))
    }

    /// The bare word at the cursor, scanned WITHOUT consuming it —
    /// the one-word lookahead the splitters need at
    /// prefix-or-name decision points. Trivia is not skipped: the
    /// cursor must sit on the word's first byte.
    pub fn peek_word(&self) -> Option<Span> {
        let start = self.pos;
        let b = self.src.as_bytes().get(start).copied()?;
        if !(b.is_ascii_alphabetic() || b == b'_') {
            return None;
        }
        let mut end = start + 1;
        while self
            .src
            .as_bytes()
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            end += 1;
        }
        Some(Span::new(start, end - start))
    }

    /// Reads an identifier in any of SurrealQL's three forms: a bare
    /// word (as [`Scanner::read_word`]), a `` `…` `` quoted identifier,
    /// or a `⟨…⟩` bracketed one. Returns the identifier's full span
    /// (quotes included), its unescaped interior spelling, and whether
    /// it was quoted — `\` escapes decode through the SurrealQL escape
    /// table, mirroring upstream's `unescape_ident_span`, and an
    /// escape outside the table is [`Diag::InvalidIdentEscape`]. The
    /// quoted flag exists because keyword rules differ per form (a
    /// bare reserved word cannot be an identifier; a quoted one can).
    /// Keyword words are otherwise *not* filtered here — that is the
    /// parser's call. Returns `Ok(None)` when no identifier starts at
    /// the cursor, consuming nothing.
    pub fn read_ident(&mut self) -> Result<Option<(Span, String, bool)>, Diag> {
        let quoted = match self.peek_byte() {
            Some(b'`') => Some(('`', 1)),
            _ if self.rest().starts_with('⟨') => Some(('⟩', '⟨'.len_utf8())),
            _ => None,
        };
        if let Some((closer, opener_len)) = quoted {
            let opener = Span::new(self.pos, opener_len);
            let full_start = self.pos;
            self.pos += opener_len;
            let interior = self.skip_quoted_ident(closer, opener)?;
            let full = Span::new(full_start, self.pos - full_start);
            return Ok(Some((full, self.unescape_ident(interior)?, true)));
        }
        Ok(self
            .read_word()
            .map(|span| (span, span.slice(self.src).to_string(), false)))
    }

    /// Decodes a quoted identifier's interior: `\` escapes through the
    /// SurrealQL table (upstream `lex_common_escape_sequence` with
    /// `json_escapes = false`), other characters verbatim.
    fn unescape_ident(&self, span: Span) -> Result<String, Diag> {
        let text = span.slice(self.src);
        unescape_escapes(text).map_err(|at| Diag::InvalidIdentEscape {
            span: Span::point(span.offset + at),
        })
    }

    /// Captures verbatim source until one of `terminators` (ASCII
    /// bytes) appears at bracket depth 0, under the full rule-3 skip
    /// set: `(`/`{`/`[` raise depth, their closers lower it, and
    /// strings, comments, quoted identifiers, and regex literals are
    /// skipped wholesale.
    /// The terminator is consumed and reported; the span covers the
    /// captured text without it (possibly empty — `key: ;`).
    ///
    /// Errors: EOF first ([`Diag::UnterminatedCapture`], anchored at
    /// the capture start; the cursor then rests at EOF) or a closer at
    /// depth 0 ([`Diag::UnbalancedCapture`], anchored at the closer,
    /// which the cursor then rests on, unconsumed). Closer bytes are
    /// never usable as terminators — the closer arms win — and with
    /// several terminators the EOF error names `terminators[0]`.
    pub fn capture_until(&mut self, terminators: &[u8]) -> Result<(Span, u8), Diag> {
        debug_assert!(!terminators.is_empty());
        let anchor = self.pos;
        match self.scan_capture(terminators, None, false, anchor)? {
            (span, Some(term)) => Ok((span, term)),
            (_, None) => unreachable!("terminated captures always report a terminator"),
        }
    }

    /// Captures verbatim source until a depth-0 bare word matches one
    /// of `stops` (ASCII case-insensitively, its follow shape
    /// confirmed — see [`CaptureStopWord`]), until a depth-0 `;`, or
    /// until end of source — [`Self::capture_until`]'s model with word
    /// terminators, for the clause-value captures of the DDL
    /// readers (the next clause's keyword ends the value). The same
    /// skip set applies; a stop word inside a string, comment, or
    /// quoted identifier, or at bracket depth above 0, never ends the
    /// capture, and a stop word whose follow shape does not match is
    /// expression content — the capture continues past it. The span
    /// covers the captured text (possibly empty); on
    /// [`CaptureStop::Word`] the cursor rests on the word,
    /// unconsumed, so the caller reads it as the next clause's key.
    /// A closer at depth 0 is [`Diag::UnbalancedCapture`].
    pub fn capture_until_word(
        &mut self,
        stops: &[CaptureStopWord],
    ) -> Result<(Span, CaptureStop), Diag> {
        self.capture_until_word_inner(stops, true, false)
    }

    /// [`Self::capture_until_word`] with the PRINTER-regex extension:
    /// over printer output a `/…/`
    /// regex literal is lexically exact — the interior's slashes are
    /// escaped (upstream's `Literal::Regex` print) — so a `;` or
    /// quote INSIDE one must not end the statement. Whether a `/`
    /// opens one is the engine's own prime-vs-infix discrimination,
    /// closed into a predecessor test (the full contract on
    /// [`Self::skip_regex_literal`]): after a complete operand it is
    /// division; at operand-expected position — expression start,
    /// after an operator, after a SYNTAX word (a keyword-spelled
    /// OPERAND ends its operand instead — the printer prints
    /// non-reserved keyword-spelled idents bare)
    /// — it opens, regardless of spacing. No production
    /// caller — this module's tests exercise the fn, pinning the
    /// predecessor discrimination for the capture engines; the
    /// compile-side repair engine ([`repair_module_call_spaces`])
    /// consumes the shared skip set ([`Self::skip_regex_literal`])
    /// directly, and the apply flow does not split statements
    /// at all (the convergence emitter marks the boundaries; there is
    /// no splitter). Regex
    /// literals ride the SHARED capture skip set since #60 —
    /// hand-written and printer captures skip interiors identically
    /// (the same prime-vs-infix discrimination), and an unterminated
    /// literal there fails the capture closed
    /// ([`Diag::UnterminatedRegex`], SDL-E0001's regex variant,
    /// anchored at the opening `/`). This extension's own arm —
    /// anchored at the capture start, where the shared arm's
    /// whole-text bound could disagree (no reachable capture start
    /// does today) — keeps a consume-to-EOF posture: the whole tail
    /// rides as ONE span.
    pub fn capture_until_word_printer(
        &mut self,
        stops: &[CaptureStopWord],
    ) -> Result<(Span, CaptureStop), Diag> {
        self.capture_until_word_inner(stops, true, true)
    }

    /// Scans for the first depth-0 bare word matching `stops` (ASCII
    /// case-insensitively) and rests on it, returning the matched
    /// text — [`Self::capture_until_word`]'s model WITHOUT the
    /// operand-position guards and follow confirmations. For the
    /// placement extractor's dialect only (the view `FROM`
    /// walk): the printer prints clause keywords uppercase, and an
    /// operand spelled exactly like a stop word prints bare whenever
    /// the word is outside the printer's own escape set
    /// (`EscapeIdent` backticks only `could_be_reserved` — `from` is
    /// NOT one), so a depth-0 bare stop word is the clause unless the
    /// scanned text carries such a spelling in operand position — the
    /// same clause-head-spelled-operand family as the splitter's word
    /// rule (the view walk's corners ride the read side, tracker
    /// #63; see `info::place::view_tables`'s doc). `None` at end of
    /// source (or at a depth-0 `;` — consumed).
    pub fn scan_to_word(&mut self, stops: &[&'static str]) -> Result<Option<String>, Diag> {
        let stops: Vec<CaptureStopWord> = stops
            .iter()
            .map(|stop| CaptureStopWord(stop, CaptureFollow::Any))
            .collect();
        match self.capture_until_word_inner(&stops, false, false)? {
            (_, CaptureStop::Word(word)) => Ok(Some(word)),
            (_, _) => Ok(None),
        }
    }

    fn capture_until_word_inner(
        &mut self,
        stops: &[CaptureStopWord],
        operand_guard: bool,
        printer_regex: bool,
    ) -> Result<(Span, CaptureStop), Diag> {
        let start = self.pos;
        let mut depth = 0usize;
        loop {
            let Some(b) = self.peek_byte() else {
                return Ok((Span::new(start, self.pos - start), CaptureStop::End));
            };
            // The skip set first (its one home is [`Self::skip_skip_set`]).
            if self.skip_skip_set()? {
                continue;
            }
            // The printer-regex extension (the splitter's entry only):
            // a right-glued `/` over printer output opens a regex
            // literal whose interior a `;` must not split. An
            // unterminated literal consumed to EOF — `continue`
            // re-peeks (the loop returns `End`); the stale `b` must
            // not drive the match.
            if printer_regex && b == b'/' {
                match self.skip_regex_literal(start) {
                    SkipOutcome::Skipped | SkipOutcome::Unterminated => continue,
                    SkipOutcome::None => {}
                }
            }
            match b {
                b';' if depth == 0 => {
                    let span = Span::new(start, self.pos - start);
                    self.pos += 1;
                    return Ok((span, CaptureStop::Semicolon));
                }
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    self.pos += 1;
                }
                b')' | b']' | b'}' if depth == 0 => {
                    return Err(Diag::UnbalancedCapture {
                        span: Span::new(self.pos, 1),
                        bracket: char::from(b),
                    });
                }
                b')' | b']' | b'}' => {
                    depth -= 1;
                    self.pos += 1;
                }
                _ if b.is_ascii_alphabetic() || b == b'_' => {
                    let word_at = self.pos;
                    // `read_word` always consumes here: the lead byte
                    // is a word start by this match arm's guard.
                    if let Some(word) = self.read_word() {
                        if depth == 0 {
                            let text = word.slice(self.src);
                            // The operand-position guards (guarded mode
                            // only): a word in OPERAND position is
                            // content, never a clause keyword — the
                            // capture's first token (a clause value
                            // never begins with a clause keyword),
                            // preceded by a name joiner (`fn::then(1)`
                            // in a WHEN expression, `src.readonly` in a
                            // DEFAULT — idiom segments print bare), or
                            // preceded by an operator (`x1 + readonly`
                            // — the word is the expression's last
                            // operand; a binary operator is always
                            // followed by an operand in printer-valid
                            // text).
                            let matched = if !operand_guard {
                                stops.iter().find(|CaptureStopWord(stop, _)| {
                                    stop.eq_ignore_ascii_case(text)
                                })
                            } else {
                                let mut first = start;
                                while first < word_at
                                    && matches!(
                                        self.src.as_bytes()[first],
                                        b' ' | b'\t' | b'\n' | b'\r'
                                    )
                                {
                                    first += 1;
                                }
                                let operand_position = word_at == first || {
                                    let bytes = self.src.as_bytes();
                                    let mut back = word_at;
                                    while back > start
                                        && matches!(bytes[back - 1], b' ' | b'\t' | b'\n' | b'\r')
                                    {
                                        back -= 1;
                                    }
                                    let prev =
                                        back.checked_sub(1).and_then(|at| bytes.get(at)).copied();
                                    match prev {
                                        Some(b'/') => {
                                            // A left-SPACED `/` is binary
                                            // division (the printer spaces
                                            // both sides, live-probed); a
                                            // left-GLUED `/` is a regex
                                            // literal's closer
                                            // (`[a-z]+$/`) — what follows
                                            // is a fresh token, not an
                                            // operand.
                                            back < start + 2
                                                || matches!(
                                                    bytes[back - 2],
                                                    b' ' | b'\t' | b'\n' | b'\r'
                                                )
                                        }
                                        Some(
                                            b'+' | b'-' | b'*' | b'%' | b'=' | b'<' | b'>' | b'!'
                                            | b'&' | b'|' | b'~' | b'@' | b'?',
                                        ) => true,
                                        Some(b) if b.is_ascii_alphabetic() => {
                                            let mut w = back - 1;
                                            while w > start
                                                && (bytes[w - 1].is_ascii_alphanumeric()
                                                    || bytes[w - 1] == b'_')
                                            {
                                                w -= 1;
                                            }
                                            is_operator_word(&self.src[w..back])
                                        }
                                        _ => false,
                                    }
                                };
                                let path_segment = matches!(
                                    word_at
                                        .checked_sub(1)
                                        .and_then(|at| self.src.as_bytes().get(at)),
                                    Some(b':' | b'.')
                                );
                                if path_segment || operand_position {
                                    None
                                } else {
                                    stops.iter().find(|CaptureStopWord(stop, _)| {
                                        stop.eq_ignore_ascii_case(text)
                                    })
                                }
                            };
                            if let Some(CaptureStopWord(_, follow)) = matched {
                                // The follow probe may skip trivia
                                // ahead; either outcome resumes the
                                // scan where `read_word` left it —
                                // on a pass the span close below
                                // rewinds to the word, on a fail the
                                // word is content and the capture
                                // continues past it. Unguarded mode
                                // stops unconditionally.
                                if !operand_guard || self.word_follows(word.end(), *follow, stops) {
                                    self.pos = word_at;
                                    let span = Span::new(start, word_at - start);
                                    return Ok((span, CaptureStop::Word(text.to_string())));
                                }
                            }
                        }
                    } else {
                        self.pos = word_at;
                        self.advance();
                    }
                }
                _ => self.advance(),
            }
        }
    }

    /// Whether `follow`'s shape sits after byte `at` (the probe skips
    /// trivia non-destructively): the word-end follow check of
    /// [`Self::capture_until_word`]. `stops` rides along for
    /// [`CaptureFollow::Value`]'s stop-word lookahead — a candidate
    /// followed by its own capture's next stop word is content, and
    /// the capture stops at that stop word instead (bounded — see the
    /// depth cap).
    fn word_follows(
        &mut self,
        at: usize,
        follow: CaptureFollow,
        stops: &[CaptureStopWord],
    ) -> bool {
        self.word_follows_depth(at, follow, stops, 0)
    }

    /// The bounded body of [`Self::word_follows`]: the stop-word
    /// lookahead's recursion depth caps at [`FOLLOW_DEPTH_CAP`] —
    /// deeper stop-word stacks are garbage input; unconfirmable there
    /// means the candidate confirms as the clause, the deep stop word
    /// rides into its capture as content, and the shape fails closed
    /// (loudly) there.
    fn word_follows_depth(
        &mut self,
        at: usize,
        follow: CaptureFollow,
        stops: &[CaptureStopWord],
        depth: usize,
    ) -> bool {
        let saved = self.pos;
        self.pos = at;
        self.skip_surrealql_trivia();
        let ok = match follow {
            CaptureFollow::Any => true,
            CaptureFollow::Value => {
                match self.peek_byte() {
                    // A glued `(` or `[` is an idiom continuation on
                    // the candidate word — an index (`comment[0]`) or
                    // a method call (`meta.comment(1)`) — never the
                    // clause's value: the pinned printer always emits
                    // a space before a clause value, while idiom parts
                    // print glued.
                    Some(b'(') | Some(b'[') => self.pos > at,
                    // A sign-led value's minus is GLUED to its
                    // operand (`-1`, `-$x` — upstream's unary minus);
                    // a SPACED minus is the binary operator
                    // (`comment - 1` — live-printed arithmetic,
                    // always ` - ` both sides).
                    Some(b'-') => !is_trivia_byte(self.peek_ahead(1)),
                    // A cast opens a value (`COMMENT <int> $x` — the
                    // printer emits the kind glued inside the angle
                    // brackets); a SPACED `<` is the comparison
                    // operator, always followed by a non-word operand
                    // start.
                    Some(b'<') => {
                        matches!(self.peek_ahead(1), Some(b) if b.is_ascii_alphabetic())
                    }
                    // A quote, `$`, a brace, a backtick, a digit — a
                    // value token opens.
                    Some(b)
                        if matches!(b, b'\'' | b'"' | b'$' | b'{' | b'`') || b.is_ascii_digit() =>
                    {
                        true
                    }
                    // Negation opens a value; `!=` is the inequality
                    // operator — content.
                    Some(b'!') => self.peek_ahead(1) != Some(b'='),
                    // A closure's parameter bar opens a value
                    // (`COMMENT |$a: any| $a` — upstream prints the
                    // closure's resolved kind); `||` is the OR
                    // operator — content.
                    Some(b'|') => self.peek_ahead(1) != Some(b'|'),
                    // A `⟨…⟩`-quoted ident opens a value like a
                    // backticked one (multi-byte opener — checked on
                    // the text, not the byte).
                    _ if self.rest().starts_with('⟨') => true,
                    // A word: an operator continues the expression
                    // (content); one of the capture's own stop words
                    // is the next clause — this candidate is content;
                    // anything else begins a value (a prefixed
                    // string, `NONE`, `true`, a function path, …).
                    Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
                        self.word_follows_item(stops, depth)
                    }
                    _ => false,
                }
            }
            CaptureFollow::Perm => self.peek_word().is_some_and(|span| {
                matches!(
                    span.slice(self.src).to_ascii_lowercase().as_str(),
                    "none" | "full" | "where" | "for"
                )
            }),
            // A string literal opens (the `COMMENT` clauses' follow): a
            // plain quote, or a prefixed string's single-letter opener
            // directly before a quote (`d"`, `s'`, …).
            CaptureFollow::String => {
                matches!(self.peek_byte(), Some(b'\'' | b'"'))
                    || matches!(
                        self.peek_word()
                            .map(|span| (span.len, self.peek_ahead(span.len))),
                        Some((1, Some(b'\'' | b'"')))
                    )
            }
            CaptureFollow::Digit => matches!(self.peek_byte(), Some(b) if b.is_ascii_digit()),
            CaptureFollow::Word(words) => self.peek_word().is_some_and(|span| {
                let word = span.slice(self.src);
                words
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(word))
            }),
            // A then-item opens — `(` or `{`, SPACED from the word
            // (a glued `(` is a method call on a stop-word-named
            // function: `meta.then(1)`). A literal item confirms
            // too (digit- or quote-led: the printer emits bare
            // `THEN 1` / `THEN 'x'` items — the full-product
            // oracle's catch). A word-led item confirms under the
            // shared word branch: the printer emits bare word-led
            // THEN bodies (`THEN fn::a::b()`, `THEN THROW 'x'` —
            // live INFO shapes, the promoted half of accepted-corner
            // #22), and the misread that once kept the shape
            // fail-closed (`x.then THEN …`) never reaches this probe
            // — a path segment is never a stop-word candidate, and a
            // bare `then` operand sits in operand position.
            CaptureFollow::ThenItem => {
                (matches!(self.peek_byte(), Some(b'(' | b'{')) && self.pos > at)
                    || matches!(self.peek_byte(), Some(b'\'' | b'"'))
                    || matches!(self.peek_byte(), Some(b) if b.is_ascii_digit())
                    || self.word_follows_item(stops, depth)
            }
        };
        self.pos = saved;
        ok
    }

    /// The shared word branch of the [`CaptureFollow::Value`] and
    /// [`CaptureFollow::ThenItem`] follows: an operator word
    /// continues the candidate's expression — content; one of the
    /// capture's own stop words is looked ahead (depth-capped) so a
    /// candidate whose next clause really follows stays content; any
    /// other word opens the item — the printer's word-led shapes
    /// (`fn::a::b()`, `THROW 'x'`, a bare table) never carry an
    /// operator or a clause keyword in that slot. The cursor sits
    /// past trivia; a word-start byte is the Value arm's match-guard
    /// contract, while the ThenItem arm reaches here after its own
    /// openers all failed — a non-word byte just reports no-confirm.
    fn word_follows_item(&mut self, stops: &[CaptureStopWord], depth: usize) -> bool {
        let Some(span) = self.peek_word() else {
            return false;
        };
        let word = span.slice(self.src);
        if is_operator_word(word) {
            return false;
        }
        if let Some(CaptureStopWord(_, inner)) = stops
            .iter()
            .find(|CaptureStopWord(stop, _)| stop.eq_ignore_ascii_case(word))
        {
            let confirmed = if depth >= FOLLOW_DEPTH_CAP {
                false
            } else {
                self.word_follows_depth(span.offset + span.len, *inner, stops, depth + 1)
            };
            // Confirmed stop → that word is the next clause and our
            // candidate is content (the capture continues to it);
            // unconfirmed → that word is content too, so our
            // candidate is the clause.
            !confirmed
        } else {
            true
        }
    }

    /// Captures a brace-balanced body: the cursor must sit on the
    /// opening `{` (`fn` bodies, `raw { … }`). Consumes through the
    /// matching `}` and returns the interior span (braces excluded).
    /// The same skip set applies; a `)` or `]` at the opening brace's
    /// depth is [`Diag::UnbalancedCapture`], EOF is
    /// [`Diag::UnterminatedCapture`] expecting `}`.
    pub fn capture_balanced_braces(&mut self) -> Result<Span, Diag> {
        let anchor = self.pos;
        debug_assert_eq!(self.peek_byte(), Some(b'{'));
        let (span, term) = self.scan_capture(&[], Some(Brackets::Braces), false, anchor)?;
        debug_assert!(term.is_none());
        Ok(span)
    }

    /// The same balanced capture when the opening `{` is already
    /// consumed — `fn`'s `->` return-kind capture
    /// ([`Scanner::capture_until`] with `{` as the terminator) eats
    /// it on the way to the body. The interior span starts directly
    /// after the `{`, so the caller can recover the whole braced
    /// block as `Span::new(span.offset - 1, span.len + 2)`.
    pub fn capture_balanced_braces_open_eaten(&mut self) -> Result<Span, Diag> {
        let anchor = self.pos - 1;
        let (span, term) = self.scan_capture(&[], Some(Brackets::Braces), true, anchor)?;
        debug_assert!(term.is_none());
        Ok(span)
    }

    /// Captures a paren-balanced list: the cursor must sit on the
    /// opening `(` (`fn`'s parameter list — kinds hold `,`s inside
    /// `<>` that no brace depth tracks, so the list rides one
    /// balanced capture). Consumes through the matching `)` and
    /// returns the interior span (parens excluded); the same skip set
    /// applies, and braces inside nest freely.
    pub fn capture_balanced_parens(&mut self) -> Result<Span, Diag> {
        let anchor = self.pos;
        debug_assert_eq!(self.peek_byte(), Some(b'('));
        let (span, term) = self.scan_capture(&[], Some(Brackets::Parens), false, anchor)?;
        debug_assert!(term.is_none());
        Ok(span)
    }

    /// Captures one plain string-literal token: the cursor must sit
    /// on `'` or `"` (no prefix — the prefixed forms are not plain
    /// string literals). Consumes through the closing quote and
    /// returns the whole token's span, quotes included. The api route
    /// path is the one header-position consumer; clause values
    /// capture to `;` and decode the whole capture instead.
    pub fn capture_string_literal(&mut self) -> Result<Span, Diag> {
        let anchor = self.pos;
        let quote = self
            .peek_byte()
            .filter(|b| *b == b'\'' || *b == b'"')
            .expect("caller checked for a quote");
        let opener = Span::new(self.pos, 1);
        self.pos += 1;
        self.skip_string_body(quote, opener)?;
        Ok(Span::new(anchor, self.pos - anchor))
    }

    /// The shared capture engine. Terminated mode ends at a terminator
    /// byte at depth 0 (reported, consumed); balanced mode enters at
    /// the opening bracket — consuming it unless `open_consumed` —
    /// and ends at the closer that returns depth to the opener's
    /// level (consumed, not reported). Closers at or below the floor
    /// are errors. Depth is a count, not per-kind pairing — `(a]`
    /// decrements once, and the embedded parser rejects the shape.
    fn scan_capture(
        &mut self,
        terminators: &[u8],
        balanced: Option<Brackets>,
        open_consumed: bool,
        anchor: usize,
    ) -> Result<(Span, Option<u8>), Diag> {
        let floor = usize::from(balanced.is_some());
        if balanced.is_some() && !open_consumed {
            self.pos += 1; // the opening bracket
        }
        let start = self.pos;
        let mut depth = floor;
        loop {
            let Some(b) = self.peek_byte() else {
                let terminator = match balanced {
                    Some(Brackets::Braces) => '}',
                    Some(Brackets::Parens) => ')',
                    None => char::from(terminators[0]),
                };
                return Err(Diag::UnterminatedCapture {
                    span: Span::point(anchor),
                    terminator,
                });
            };
            // The skip set first (its one home is [`Self::skip_skip_set`]).
            if self.skip_skip_set()? {
                continue;
            }
            // Terminators check before the bracket arms: `fn`'s
            // return-kind capture ends at the body's `{`, an
            // opener byte. No other call site passes an opener
            // (or closer) as a terminator.
            match b {
                _ if terminators.contains(&b) && depth == floor => {
                    let span = Span::new(start, self.pos - start);
                    self.pos += 1; // consume the terminator
                    return Ok((span, Some(b)));
                }
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    self.pos += 1;
                }
                b')' => {
                    if balanced == Some(Brackets::Parens) && depth == 1 {
                        let end = self.pos; // the interior excludes the closer
                        self.pos += 1;
                        return Ok((Span::new(start, end - start), None));
                    }
                    if depth <= floor {
                        return Err(Diag::UnbalancedCapture {
                            span: Span::new(self.pos, 1),
                            bracket: char::from(b),
                        });
                    }
                    depth -= 1;
                    self.pos += 1;
                }
                b']' => {
                    if depth <= floor {
                        return Err(Diag::UnbalancedCapture {
                            span: Span::new(self.pos, 1),
                            bracket: char::from(b),
                        });
                    }
                    depth -= 1;
                    self.pos += 1;
                }
                b'}' => {
                    if balanced == Some(Brackets::Braces) && depth == 1 {
                        let end = self.pos; // the interior excludes the closer
                        self.pos += 1;
                        return Ok((Span::new(start, end - start), None));
                    }
                    if depth <= floor {
                        return Err(Diag::UnbalancedCapture {
                            span: Span::new(self.pos, 1),
                            bracket: char::from(b),
                        });
                    }
                    depth -= 1;
                    self.pos += 1;
                }
                _ => self.advance(),
            }
        }
    }

    /// Scans a string body; the cursor is just past the opening quote
    /// and `quote` closes it. Mirrors `eat_string_like`: `\` skips the
    /// next character (whatever it is — contents are validated later),
    /// and literal newlines are legal inside strings.
    fn skip_string_body(&mut self, quote: u8, opener: Span) -> Result<(), Diag> {
        loop {
            let Some(b) = self.peek_byte() else {
                return Err(Diag::UnterminatedString { span: opener });
            };
            match b {
                b'\\' => {
                    self.pos += 1;
                    if self.peek_byte().is_none() {
                        return Err(Diag::UnterminatedString { span: opener });
                    }
                    self.advance();
                }
                _ if b == quote => {
                    self.pos += 1;
                    return Ok(());
                }
                _ => self.advance(),
            }
        }
    }

    /// Scans a quoted identifier; the cursor is just past the opener.
    /// The first unescaped `closer` ends it; `\` skips the next
    /// character (mirrors `lex_surrounded_ident_err`). Returns the
    /// interior span (quotes excluded) for [`Scanner::read_ident`]'s
    /// unescaping; capture-mode callers ignore it.
    fn skip_quoted_ident(&mut self, closer: char, opener: Span) -> Result<Span, Diag> {
        let interior_start = self.pos;
        loop {
            let Some(b) = self.peek_byte() else {
                return Err(Diag::UnterminatedQuotedIdent {
                    span: opener,
                    closer,
                });
            };
            match b {
                b'\\' => {
                    self.pos += 1;
                    if self.peek_byte().is_none() {
                        return Err(Diag::UnterminatedQuotedIdent {
                            span: opener,
                            closer,
                        });
                    }
                    self.advance();
                }
                _ if self.char_at() == Some(closer) => {
                    let end = self.pos;
                    self.pos += closer.len_utf8();
                    return Ok(Span::new(interior_start, end - interior_start));
                }
                _ => self.advance(),
            }
        }
    }

    /// Scans a block comment; the cursor is just past `/*`. Ends at
    /// the first `*/` — no nesting (mirrors `eat_multi_line_comment`).
    fn skip_block_comment(&mut self, opener: Span) -> Result<(), Diag> {
        loop {
            let Some(b) = self.peek_byte() else {
                return Err(Diag::UnterminatedBlockComment { span: opener });
            };
            if b == b'*' && self.peek_ahead(1) == Some(b'/') {
                self.pos += 2;
                return Ok(());
            }
            self.advance();
        }
    }

    /// Scans a line comment; the cursor is just past the opener
    /// (`--`, `//`, or `#`). Consumes through the line end (mirrors
    /// `eat_single_line_comment`).
    fn skip_line_comment(&mut self) {
        loop {
            let Some(_) = self.peek_byte() else {
                return;
            };
            match self.line_end_len() {
                0 => self.advance(),
                width => {
                    self.pos += width;
                    return;
                }
            }
        }
    }

    /// Byte width of a line end at the cursor: LF, CRLF, lone CR,
    /// U+2028, U+2029, or U+0085; 0 if none.
    fn line_end_len(&self) -> usize {
        match self.peek_byte() {
            Some(b'\n') => 1,
            Some(b'\r') if self.peek_ahead(1) == Some(b'\n') => 2,
            Some(b'\r') => 1,
            Some(b'\xe2')
                if self.rest().starts_with('\u{2028}') || self.rest().starts_with('\u{2029}') =>
            {
                3
            }
            Some(b'\xc2') if self.rest().starts_with('\u{0085}') => 2,
            _ => 0,
        }
    }

    /// Byte width of the Unicode whitespace at the cursor
    /// (`eat_whitespace`'s non-ASCII set), or 0.
    fn unicode_space_len(&self) -> usize {
        match self.char_at() {
            Some(
                c @ ('\u{00a0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200a}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'),
            ) => c.len_utf8(),
            _ => 0,
        }
    }

    /// Consumes one full character; the cursor only ever stops on
    /// character boundaries, so this cannot split one.
    fn advance(&mut self) {
        let len = self
            .char_at()
            .expect("caller checked a byte exists")
            .len_utf8();
        self.pos += len;
    }

    fn char_at(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_ahead(&self, n: usize) -> Option<u8> {
        self.src.as_bytes().get(self.pos + n).copied()
    }

    /// The source from the cursor on — the two-byte lookahead the
    /// segment reader's `::` test needs without consuming.
    pub(crate) fn rest(&self) -> &'src str {
        &self.src[self.pos..]
    }
}

/// Decodes `\` escapes through the SurrealQL table (upstream
/// `lex_common_escape_sequence` with `json_escapes = false`): shared
/// by quoted identifiers ([`Scanner::read_ident`]) and the parser's
/// string-literal clauses (`password:`, `passhash:` — rule 3's
/// keyword clause). `Err` carries the byte offset of the offending
/// position: the unknown escape character, the byte after a trailing
/// backslash, or the backslash of a malformed `\u…`.
pub(crate) fn unescape_escapes(text: &str) -> Result<String, usize> {
    if !text.contains('\\') {
        return Ok(text.to_string());
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let (offset, c) = chars[i];
        i += 1;
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(&(esc_offset, esc)) = chars.get(i) else {
            // A closed ident or string never ends on a lone `\` in the
            // scanner's skip model; defensive.
            return Err(offset + 1);
        };
        i += 1;
        match esc {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            '\'' | '"' | '`' | '\\' => out.push(esc),
            'u' => {
                let hex_at = |j: usize| chars.get(j).and_then(|&(_, c)| c.to_digit(16));
                if chars.get(i).map(|&(_, c)| c) == Some('{') {
                    let start = i + 1;
                    let mut accum: u32 = 0;
                    let mut digits = 0;
                    while digits < 6 {
                        match hex_at(start + digits) {
                            Some(d) => {
                                accum = accum * 16 + d;
                                digits += 1;
                            }
                            None => break,
                        }
                    }
                    match chars.get(start + digits) {
                        Some(&(_, '}')) => {
                            let ch = char::from_u32(accum).ok_or(offset)?;
                            out.push(ch);
                            i = start + digits + 1;
                        }
                        _ => return Err(offset),
                    }
                } else {
                    let mut accum: u32 = 0;
                    for j in 0..4 {
                        match hex_at(i + j) {
                            Some(d) => accum = accum * 16 + d,
                            None => return Err(offset),
                        }
                    }
                    let ch = char::from_u32(accum).ok_or(offset)?;
                    out.push(ch);
                    i += 4;
                }
            }
            _ => return Err(esc_offset),
        }
    }
    Ok(out)
}

/// The byte length of `text`'s leading bare-word run
/// (`[A-Za-z0-9_]*` — SurrealQL's bare identifier charset).
pub(crate) fn word_prefix_len(text: &str) -> usize {
    text.char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

/// Lexes one SurrealQL string literal from the start of `text` — the
/// bare `'`/`"` quote forms upstream `parse_string_lit` accepts (the
/// prefixed forms are not string literals in this position). `\`
/// escapes decode through [`crate::lexer::unescape_escapes`]; the
/// scanner's skip model (`\` takes the next character, whatever it
/// is) finds the closing quote, then the shared table validates the
/// interior. Returns the decoded value and the text after the quote.
pub(crate) fn lex_string_literal(text: &str) -> Option<(String, &str)> {
    let quote = text.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let after_quote = &text[quote.len_utf8()..];
    let mut iter = after_quote.char_indices();
    while let Some((idx, c)) = iter.next() {
        if c == '\\' {
            // The escaped character rides through, quote included.
            iter.next();
            continue;
        }
        if c == quote {
            let value = crate::lexer::unescape_escapes(&after_quote[..idx]).ok()?;
            return Some((value, &after_quote[idx + 1..]));
        }
    }
    None
}

/// Skips SurrealQL comments and whitespace at the start of `text` —
/// a keyword-clause capture's surrounding trivia (rule 3's comment
/// model: `--`, `//`, `#` line comments and `/* … */` blocks).
/// `None` for an unterminated block comment.
pub(crate) fn strip_capture_trivia(mut text: &str) -> Option<&str> {
    loop {
        text = text.trim_start();
        if let Some(rest) = text
            .strip_prefix("//")
            .or_else(|| text.strip_prefix("--"))
            .or_else(|| text.strip_prefix('#'))
        {
            // Keep the line end itself; the next trim_start eats it.
            text = match rest.find(['\n', '\r', '\u{2028}', '\u{2029}', '\u{0085}']) {
                Some(at) => &rest[at..],
                None => "",
            };
            continue;
        }
        if let Some(rest) = text.strip_prefix("/*") {
            let end = rest.find("*/")?;
            text = &rest[end + 2..];
            continue;
        }
        return Some(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The two capture flavors must skip identical shapes: over every
    /// skip-set stressor, the word-terminated capture agrees byte for
    /// byte with the byte-terminated one (`capture_until_word`
    /// mirrors `scan_capture`'s arms — this is
    /// the drift guard that promise rides on).
    #[test]
    fn capture_until_word_matches_capture_until_skip_set() {
        let cases = [
            "value 'str;ing' ;",
            "value \"str;ing\" ;",
            "value d'str;;ing' ;",
            "value r\"raw;stuff\" ;",
            "value `ide;nt` ;",
            "value ⟨ide;nt⟩ ;",
            "value -- line; comment\n ;",
            "value # line; comment\n ;",
            "value /* block ; comment */ ;",
            "value (nested ; brackets) ;",
            "value { brace ; } ;",
            "value [array ;] ;",
            "value m1x3d_W0rds ;",
            "  ;",
        ];
        for case in cases {
            let mut byte_terminated = Scanner::new(case);
            let mut word_terminated = Scanner::new(case);
            let (byte_span, _) = byte_terminated
                .capture_until(b";")
                .expect("the case is terminated");
            let (word_span, stop) = word_terminated
                .capture_until_word(&[])
                .expect("no stop words, terminated");
            assert_eq!(byte_span, word_span, "skip-set disagreement on `{case}`");
            assert_eq!(stop, CaptureStop::Semicolon);
        }
        // The stop word itself ends the capture with the cursor on it
        // — and is invisible to the byte-terminated flavor.
        let mut scanner = Scanner::new("50 START 0;");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("start", CaptureFollow::Any)])
            .unwrap();
        assert_eq!(span.slice("50 START 0;"), "50 ");
        assert_eq!(stop, CaptureStop::Word("START".to_string()));
        assert_eq!(
            scanner
                .peek_word()
                .expect("on the word")
                .slice("50 START 0;"),
            "START"
        );
    }

    /// The follow shapes: a stop
    /// word whose follow does not match is expression content — the
    /// capture continues past it and the value stays whole.
    #[test]
    fn capture_until_word_follow_shapes() {
        // param `VALUE comment:1` — the ledgered corner: `comment`
        // is not followed by a string, so it is record access here.
        let mut scanner = Scanner::new("comment:1 COMMENT 'x';");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(span.slice("comment:1 COMMENT 'x';"), "comment:1 ");
        assert_eq!(stop, CaptureStop::Word("COMMENT".to_string()));
        // `permissions` as expression content vs the clause keyword.
        let mut scanner = Scanner::new("permissions.x PERMISSIONS FULL;");
        let (span, _) = scanner
            .capture_until_word(&[CaptureStopWord("PERMISSIONS", CaptureFollow::Perm)])
            .unwrap();
        assert_eq!(
            span.slice("permissions.x PERMISSIONS FULL;"),
            "permissions.x "
        );
        // The clause keyword WITH its value shape stops.
        let mut scanner = Scanner::new("x PERMISSIONS WHERE y COMMENT 'c';");
        let (span, stop) = scanner
            .capture_until_word(&[
                CaptureStopWord("PERMISSIONS", CaptureFollow::Perm),
                CaptureStopWord("COMMENT", CaptureFollow::Value),
            ])
            .unwrap();
        assert_eq!(span.slice("x PERMISSIONS WHERE y COMMENT 'c';"), "x ");
        assert_eq!(stop, CaptureStop::Word("PERMISSIONS".to_string()));
        // event `WHEN then (CREATE x)` — `then` without a then-item
        // shape is content; with one it stops.
        let mut scanner = Scanner::new("then = 1 THEN (CREATE x);");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("THEN", CaptureFollow::ThenItem)])
            .unwrap();
        assert_eq!(span.slice("then = 1 THEN (CREATE x);"), "then = 1 ");
        assert_eq!(stop, CaptureStop::Word("THEN".to_string()));
        // The follow probe restores the cursor either way.
        assert_eq!(
            scanner
                .peek_word()
                .expect("on the word")
                .slice("then = 1 THEN (CREATE x);"),
            "THEN"
        );
        // Trivia (a comment) may sit between the word and its value.
        let mut scanner = Scanner::new("x COMMENT /* why */ 'c';");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(span.slice("x COMMENT /* why */ 'c';"), "x ");
        assert_eq!(stop, CaptureStop::Word("COMMENT".to_string()));
        // A word directly preceded by `:` is a name-path segment
        // (`fn::then(1)` inside a WHEN expression) — never a clause
        // keyword position, whatever follows it.
        let mut scanner = Scanner::new("fn::then(1) = 1 THEN (CREATE x)");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("THEN", CaptureFollow::ThenItem)])
            .unwrap();
        assert_eq!(
            span.slice("fn::then(1) = 1 THEN (CREATE x)"),
            "fn::then(1) = 1 "
        );
        assert_eq!(stop, CaptureStop::Word("THEN".to_string()));
        // A word-led then-item confirms (the live INFO shapes: the
        // printer emits bare `THEN fn::a::b()` / `THEN THROW 'x'`
        // bodies).
        let mut scanner = Scanner::new("x = 1 THEN fn::audit::write($event)");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("THEN", CaptureFollow::ThenItem)])
            .unwrap();
        assert_eq!(span.slice("x = 1 THEN fn::audit::write($event)"), "x = 1 ");
        assert_eq!(stop, CaptureStop::Word("THEN".to_string()));
        let mut scanner = Scanner::new("x = 1 THEN THROW 'boom'");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("THEN", CaptureFollow::ThenItem)])
            .unwrap();
        assert_eq!(span.slice("x = 1 THEN THROW 'boom'"), "x = 1 ");
        assert_eq!(stop, CaptureStop::Word("THEN".to_string()));
        // A path segment named `then` is never a candidate, so the
        // capture still stops at the real THEN — the misread that
        // once kept word-led items fail-closed.
        let mut scanner = Scanner::new("x.then = 1 THEN (CREATE x)");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("THEN", CaptureFollow::ThenItem)])
            .unwrap();
        assert_eq!(span.slice("x.then = 1 THEN (CREATE x)"), "x.then = 1 ");
        assert_eq!(stop, CaptureStop::Word("THEN".to_string()));
        // A `then`-headed path (`then::x()`) keeps `then` content —
        // the `::` join opens no item.
        let mut scanner = Scanner::new("then::x(1) = 1 THEN (CREATE x)");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("THEN", CaptureFollow::ThenItem)])
            .unwrap();
        assert_eq!(
            span.slice("then::x(1) = 1 THEN (CREATE x)"),
            "then::x(1) = 1 "
        );
        assert_eq!(stop, CaptureStop::Word("THEN".to_string()));
        // A `then` operand at the capture's start stays content with
        // a word-led item after the real THEN.
        let mut scanner = Scanner::new("then = 1 THEN fn::x()");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("THEN", CaptureFollow::ThenItem)])
            .unwrap();
        assert_eq!(span.slice("then = 1 THEN fn::x()"), "then = 1 ");
        assert_eq!(stop, CaptureStop::Word("THEN".to_string()));
        // Binary minus is operator content, not a sign-led value —
        // `comment - 1` is live-printed arithmetic.
        let mut scanner = Scanner::new("comment - 1 = 0 THEN (CREATE x)");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("THEN", CaptureFollow::ThenItem)])
            .unwrap();
        assert_eq!(
            span.slice("comment - 1 = 0 THEN (CREATE x)"),
            "comment - 1 = 0 "
        );
        assert_eq!(stop, CaptureStop::Word("THEN".to_string()));
        // The sign-led value's minus is GLUED to its operand
        // (`-1`, `-$x` — upstream's unary minus); the binary minus
        // is SPACED both sides.
        let mut scanner = Scanner::new("x COMMENT -1 COMMENT 'c';");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(span.slice("x COMMENT -1 COMMENT 'c';"), "x ");
        assert_eq!(stop, CaptureStop::Word("COMMENT".to_string()));
        let mut scanner = Scanner::new("x COMMENT -$x COMMENT 'c';");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(span.slice("x COMMENT -$x COMMENT 'c';"), "x ");
        assert_eq!(stop, CaptureStop::Word("COMMENT".to_string()));
        // A cast opens a value (the kind glued inside the angle
        // brackets); a spaced `<` is the comparison operator.
        let mut scanner = Scanner::new("x COMMENT <int> $x COMMENT 'c';");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(span.slice("x COMMENT <int> $x COMMENT 'c';"), "x ");
        assert_eq!(stop, CaptureStop::Word("COMMENT".to_string()));
        let mut scanner = Scanner::new("comment < 5 COMMENT 'c';");
        let (span, _) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(span.slice("comment < 5 COMMENT 'c';"), "comment < 5 ");
        // A closure's parameter bar opens a value; `||` is the OR
        // operator — content.
        let mut scanner = Scanner::new("x COMMENT |$a: any| $a COMMENT 'c';");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(span.slice("x COMMENT |$a: any| $a COMMENT 'c';"), "x ");
        assert_eq!(stop, CaptureStop::Word("COMMENT".to_string()));
        let mut scanner = Scanner::new("comment || other COMMENT 'c';");
        let (span, _) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(
            span.slice("comment || other COMMENT 'c';"),
            "comment || other "
        );
        // A quoted ident or a negation opens a value — the clause
        // reader decides plain-string vs the raw route. `!=` is the
        // inequality operator, not a negation.
        for tail in ["`we ird`", "⟨we ird⟩", "!x"] {
            let source = format!("x COMMENT {tail};");
            let mut scanner = Scanner::new(&source);
            let (span, stop) = scanner
                .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
                .unwrap();
            assert_eq!(span.slice(&source), "x ");
            assert_eq!(stop, CaptureStop::Word("COMMENT".to_string()));
        }
        let mut scanner = Scanner::new("comment != 1 COMMENT 'c';");
        let (span, _) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(span.slice("comment != 1 COMMENT 'c';"), "comment != 1 ");
        // An operator word continues the expression — `ANYINSIDE`
        // included (3.2.4 spells it as one word).
        let mut scanner = Scanner::new("meta.comment ANYINSIDE ['a'] COMMENT 'c';");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(
            span.slice("meta.comment ANYINSIDE ['a'] COMMENT 'c';"),
            "meta.comment ANYINSIDE ['a'] "
        );
        assert_eq!(stop, CaptureStop::Word("COMMENT".to_string()));
        // A GLUED `(`/`[` is an idiom continuation on the candidate
        // word — an index or a method call (`comment[0]`,
        // `meta.comment(1)`); the clause value is always spaced.
        let mut scanner = Scanner::new("comment[0] = 1 COMMENT 'c';");
        let (span, _) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(span.slice("comment[0] = 1 COMMENT 'c';"), "comment[0] = 1 ");
        let mut scanner = Scanner::new("meta.comment(1) = 1 COMMENT 'c';");
        let (span, _) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::Value)])
            .unwrap();
        assert_eq!(
            span.slice("meta.comment(1) = 1 COMMENT 'c';"),
            "meta.comment(1) = 1 "
        );
        // The ThenItem follow's glued `(` is likewise a method call.
        let mut scanner = Scanner::new("meta.then(1) = 1 THEN (CREATE x)");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("THEN", CaptureFollow::ThenItem)])
            .unwrap();
        assert_eq!(
            span.slice("meta.then(1) = 1 THEN (CREATE x)"),
            "meta.then(1) = 1 "
        );
        assert_eq!(stop, CaptureStop::Word("THEN".to_string()));
        // The String follow: a bare word never opens a
        // COMMENT value — `SELECT comment FROM post`'s field ident is
        // content where `Value`'s word branch would misfire — while a
        // plain or prefixed string confirms the clause.
        let mut scanner = Scanner::new("SELECT comment FROM post COMMENT 'c'");
        let (span, _) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::String)])
            .unwrap();
        assert_eq!(
            span.slice("SELECT comment FROM post COMMENT 'c'"),
            "SELECT comment FROM post "
        );
        let mut scanner = Scanner::new("x COMMENT d\"prefixed\";");
        let (span, stop) = scanner
            .capture_until_word(&[CaptureStopWord("COMMENT", CaptureFollow::String)])
            .unwrap();
        assert_eq!(span.slice("x COMMENT d\"prefixed\";"), "x ");
        assert_eq!(stop, CaptureStop::Word("COMMENT".to_string()));
    }

    /// A capture-stressor corpus: the `;` inside a string, the braced
    /// capture, and the comment inside a capture list exercise the
    /// scanner's skip set.
    /// NOT valid SDL by the current statement grammar — only the
    /// no-panic mutation contract rides it.
    const SAMPLE: &str = "\
/// Acme Corporation
namespace acme;

// a plain comment
sequence n = 1 + 2;
table user {
  signup: { if $auth.blocked { throw 'no;'; } };
  fields: `a;b`, ⟨c⟩, -- why; not
    1;
}
";

    /// A printer-output sample for the module-call repair's proptest:
    /// the quirk itself, multibyte content outside spans (a script
    /// body's JS interior), strings, a regex, and
    /// quoted identifiers, all in one line.
    const PRINT_SAMPLE: &str = "\
field note: string { default: mod:: a::b(1) ?? function($v){ return café; } }; \
event e { on: t; then: mod:: c::d('mod:: e::f'), /mod:: \\/ g/, ⟨mod:: h⟩, d\"mod:: i\"; };";

    fn capture(src: &str) -> Result<(String, u8), Diag> {
        let mut scanner = Scanner::new(src);
        let (span, term) = scanner.capture_until(b";")?;
        Ok((span.slice(src).to_string(), term))
    }

    fn capture_err(src: &str) -> Diag {
        Scanner::new(src).capture_until(b";").unwrap_err()
    }

    #[test]
    fn capture_ends_at_depth0_semicolon_and_consumes_it() {
        let mut scanner = Scanner::new("true; rest");
        let (span, term) = scanner.capture_until(b";").unwrap();
        assert_eq!((span.slice("true; rest"), term), ("true", b';'));
        assert_eq!(scanner.peek_byte(), Some(b' '));
    }

    #[test]
    fn capture_is_verbatim_and_may_be_empty() {
        assert_eq!(capture(" 1 + 2 ;").unwrap().0, " 1 + 2 ");
        assert_eq!(capture(";x").unwrap().0, "");
    }

    #[test]
    fn capture_tracks_nested_brackets() {
        assert_eq!(
            capture("{ if $x { throw 'no' } };y").unwrap().0,
            "{ if $x { throw 'no' } }"
        );
        assert_eq!(capture("f(a[1;2]);").unwrap().0, "f(a[1;2])");
    }

    #[test]
    fn any_terminator_byte_ends_the_capture_at_depth0() {
        let mut scanner = Scanner::new("a, b; c");
        let (span, term) = scanner.capture_until(b",").unwrap();
        assert_eq!((span.slice("a, b; c"), term), ("a", b','));
    }

    #[test]
    fn strings_may_contain_literal_newlines() {
        assert_eq!(capture("'a\nb';").unwrap().0, "'a\nb'");
    }

    #[test]
    fn module_call_spaces_repair_to_the_glued_spelling() {
        // The quirk's exact shape (the issue repro): the call repairs,
        // the adjacent fn:: path — which never carries the space —
        // rides untouched.
        assert_eq!(
            repair_module_call_spaces(
                "LET $a = fn::inventory::thumbnail_key(f\"attachments:/a.jpg\");\n\
                 RETURN mod:: thumbnail::render(b\"00\", 512);"
            ),
            "LET $a = fn::inventory::thumbnail_key(f\"attachments:/a.jpg\");\n\
             RETURN mod::thumbnail::render(b\"00\", 512);"
        );
        // The already-glued spelling is a byte-level no-op.
        assert_eq!(
            repair_module_call_spaces("RETURN mod::thumbnail::render(1);"),
            "RETURN mod::thumbnail::render(1);"
        );
        // Opaque spans keep their interiors: strings (plain and
        // prefixed — one per quote-class family), regex literals
        // (printer output escapes interior slashes), quoted
        // identifiers.
        for interior in [
            "'mod:: untouched'",
            "\"mod:: untouched\"",
            "f\"mod:: untouched\"",
            "d\"mod:: untouched\"",
            "/mod:: \\/ untouched/",
            "`mod:: untouched`",
            "⟨mod:: untouched⟩",
        ] {
            let text = format!("RETURN {interior};");
            assert_eq!(
                repair_module_call_spaces(&text),
                text,
                "interior: {interior}"
            );
        }
        // A mixed line: the real call repairs, the string interior
        // rides.
        assert_eq!(
            repair_module_call_spaces("mod:: a::b(1) + 'mod:: c::d'"),
            "mod::a::b(1) + 'mod:: c::d'"
        );
        // Unterminated spans abandon the repair whole (fail-open —
        // never panic, never truncate).
        let unterminated = "RETURN 'mod:: oops";
        assert_eq!(repair_module_call_spaces(unterminated), unterminated);
        let unterminated = "RETURN /mod:: oops";
        assert_eq!(repair_module_call_spaces(unterminated), unterminated);
        let unterminated = "RETURN /*mod:: oops";
        assert_eq!(repair_module_call_spaces(unterminated), unterminated);
        let unterminated = "RETURN `mod:: oops";
        assert_eq!(repair_module_call_spaces(unterminated), unterminated);
        // Multibyte content outside spans (a script body's verbatim
        // JS interior) rides the char-aware copy — the pre-fix byte
        // loop parked mid-char and tripped the span scanners'
        // `rest()` slices.
        let scripted = "default: mod:: a::b(1) ?? function($v){ return café; }";
        assert_eq!(
            repair_module_call_spaces(scripted),
            "default: mod::a::b(1) ?? function($v){ return café; }"
        );
        // ...and a multibyte char directly adjacent to the quirk.
        assert_eq!(repair_module_call_spaces("mod:: a::b(é)"), "mod::a::b(é)");
    }

    #[test]
    fn semicolons_in_strings_do_not_terminate() {
        for (src, expect) in [
            ("'a;b'c; rest", "'a;b'c"),
            ("\"a;b\"; rest", "\"a;b\""),
            ("d'2024-01-01;x'; rest", "d'2024-01-01;x'"),
            ("s'x;y'; rest", "s'x;y'"),
            ("u'x;y'; rest", "u'x;y'"),
            ("b'x;y'; rest", "b'x;y'"),
            ("f'a{x};y'; rest", "f'a{x};y'"),
            ("r'a\\;b'; rest", "r'a\\;b'"),
        ] {
            assert_eq!(
                capture(src).unwrap(),
                (expect.to_string(), b';'),
                "src: {src}"
            );
        }
    }

    #[test]
    fn escaped_quotes_do_not_close_strings() {
        // `\'` skips the quote, so the string continues past it.
        assert_eq!(capture("'a\\''; x").unwrap().0, "'a\\''");
        assert!(matches!(
            capture_err("'a\\';"),
            Diag::UnterminatedString { .. }
        ));
    }

    #[test]
    fn raw_strings_still_honor_backslash_while_scanning() {
        // Mirrors byte.rs `eat_string_like`, which is prefix-agnostic:
        // even `r'…'` skips the character after `\`, so `\'` cannot
        // close a raw string (upstream validates contents later).
        assert!(matches!(
            capture_err("r'a\\';"),
            Diag::UnterminatedString { .. }
        ));
        assert_eq!(capture("r'a\\\\'; x").unwrap().0, "r'a\\\\'");
    }

    #[test]
    fn uppercase_prefix_is_not_a_string_opener() {
        // Only lowercase `d s u b f r` open prefixed strings, so
        // `D'…'` is a bare word plus an ordinary string, byte.rs par.
        assert_eq!(capture("D'x'; rest").unwrap().0, "D'x'");
    }

    #[test]
    fn line_comments_inside_capture_swallow_semicolons() {
        assert_eq!(capture("a -- x; y\nb;").unwrap().0, "a -- x; y\nb");
        assert_eq!(capture("a // x; y\nb;").unwrap().0, "a // x; y\nb");
        assert_eq!(capture("a # x; y\nb;").unwrap().0, "a # x; y\nb");
        // `--` starts a comment even glued to a term (byte.rs par).
        assert_eq!(capture("1 --3\n+ 2;").unwrap().0, "1 --3\n+ 2");
    }

    #[test]
    fn all_surrealql_line_ends_close_a_line_comment() {
        for line_end in ["\n", "\r\n", "\r", "\u{2028}", "\u{2029}", "\u{0085}"] {
            let src = format!("a -- x{line_end}; b");
            // Captures are verbatim: the line end that closed the
            // comment stays inside the captured span.
            assert_eq!(
                capture(&src).unwrap(),
                (format!("a -- x{line_end}"), b';'),
                "line end {line_end:?}"
            );
        }
    }

    #[test]
    fn block_comments_swallow_semicolons_and_do_not_nest() {
        assert_eq!(capture("a /* ; */ b;").unwrap().0, "a /* ; */ b");
        assert_eq!(capture("a /* ; * ; */ b;").unwrap().0, "a /* ; * ; */ b");
        assert_eq!(capture("/* /* ; */ ;").unwrap().0, "/* /* ; */ ");
        assert!(matches!(
            capture_err("a /* oops"),
            Diag::UnterminatedBlockComment { span } if span == Span::new(2, 2)
        ));
    }

    #[test]
    fn quoted_identifiers_swallow_terminators_and_escaped_closers() {
        assert_eq!(capture("`a;b`c;").unwrap().0, "`a;b`c");
        assert_eq!(capture("⟨a;b⟩c;").unwrap().0, "⟨a;b⟩c");
        assert_eq!(capture("`a\\`b`;").unwrap().0, "`a\\`b`");
        assert_eq!(capture("⟨a\\⟩b⟩;").unwrap().0, "⟨a\\⟩b⟩");
    }

    #[test]
    fn unterminated_quoted_idents_report_their_closer() {
        assert!(matches!(
            capture_err("`abc"),
            Diag::UnterminatedQuotedIdent { closer: '`', span } if span == Span::new(0, 1)
        ));
        assert!(matches!(
            capture_err("⟨abc"),
            Diag::UnterminatedQuotedIdent { closer: '⟩', span } if span == Span::new(0, 3)
        ));
    }

    #[test]
    fn unterminated_strings_anchor_at_their_opener() {
        assert!(matches!(
            capture_err("'abc"),
            Diag::UnterminatedString { span } if span == Span::new(0, 1)
        ));
        assert!(matches!(
            capture_err("d'abc"),
            Diag::UnterminatedString { span } if span == Span::new(0, 2)
        ));
        assert!(matches!(
            capture_err("'a\\"),
            Diag::UnterminatedString { span } if span == Span::new(0, 1)
        ));
    }

    #[test]
    fn closer_at_depth0_is_unbalanced() {
        assert!(matches!(
            capture_err("foo);"),
            Diag::UnbalancedCapture { bracket: ')', span } if span == Span::new(3, 1)
        ));
        assert!(matches!(
            capture_err("foo];"),
            Diag::UnbalancedCapture { bracket: ']', .. }
        ));
        assert!(matches!(
            capture_err("foo};"),
            Diag::UnbalancedCapture { bracket: '}', .. }
        ));
    }

    #[test]
    fn missing_terminator_is_an_unterminated_capture() {
        assert!(matches!(
            capture_err("true"),
            Diag::UnterminatedCapture { terminator: ';', span } if span == Span::point(0)
        ));
    }

    #[test]
    fn balanced_brace_capture_returns_the_interior() {
        let src = "{ body { with; nested } }; x";
        let mut scanner = Scanner::new(src);
        let span = scanner.capture_balanced_braces().unwrap();
        assert_eq!(span.slice(src), " body { with; nested } ");
        assert_eq!(scanner.peek_byte(), Some(b';'));
        let mut scanner = Scanner::new("{ a ) }");
        assert!(matches!(
            scanner.capture_balanced_braces(),
            Err(Diag::UnbalancedCapture { bracket: ')', .. })
        ));
        let mut scanner = Scanner::new("{ oops");
        assert!(matches!(
            scanner.capture_balanced_braces(),
            Err(Diag::UnterminatedCapture {
                terminator: '}',
                ..
            })
        ));
    }

    #[test]
    fn doc_blocks_collect_adjacent_lines_then_resume_parsing() {
        let src = "/// Acme Corporation\n/// The first namespace.\nnamespace acme;";
        let mut scanner = Scanner::new(src);
        scanner.skip_trivia();
        assert!(scanner.at_doc_comment());
        let lines = scanner.take_doc_block();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].slice(src), "Acme Corporation");
        assert_eq!(lines[1].slice(src), "The first namespace.");
        scanner.skip_trivia();
        assert_eq!(scanner.read_word().unwrap().slice(src), "namespace");
    }

    #[test]
    fn doc_block_ends_at_blank_line() {
        let mut scanner = Scanner::new("/// a\n\n/// b\nnamespace x;");
        scanner.skip_trivia();
        assert_eq!(scanner.take_doc_block().len(), 1);
    }

    #[test]
    fn doc_block_continuation_lines_may_be_indented() {
        // Rule 7: indentation is cosmetic — nested definitions legally
        // carry indented doc blocks.
        let src = "table user {\n  /// one\n  /// two\n  f: int;\n}";
        let mut scanner = Scanner::new(src);
        scanner.skip_trivia();
        scanner.read_word();
        scanner.skip_trivia();
        scanner.read_word();
        scanner.skip_trivia();
        scanner.eat_byte(b'{');
        scanner.skip_trivia();
        let lines = scanner.take_doc_block();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].slice(src), "one");
        assert_eq!(lines[1].slice(src), "two");
        // The cursor rewinds to the start of the next line's trivia.
        scanner.skip_trivia();
        assert_eq!(scanner.peek_byte(), Some(b'f'));
    }

    #[test]
    fn four_slashes_are_a_doc_line_with_slash_text() {
        // `////` starts with `///`, so it is a doc line whose text
        // begins at the fourth slash (rustdoc-like convention, pinned).
        let src = "//// four slashes\n";
        let mut scanner = Scanner::new(src);
        scanner.skip_trivia();
        let lines = scanner.take_doc_block();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].slice(src), "/ four slashes");
    }

    #[test]
    fn doc_text_strips_one_leading_space_and_trailing_whitespace() {
        let src = "///  two spaces\n///trailing   \n///\n";
        let mut scanner = Scanner::new(src);
        scanner.skip_trivia();
        let lines = scanner.take_doc_block();
        assert_eq!(lines[0].slice(src), " two spaces");
        assert_eq!(lines[1].slice(src), "trailing");
        assert_eq!(lines[2].slice(src), "");
    }

    #[test]
    fn trivia_stops_at_plain_and_doc_comments() {
        let src = "  // note\n \t\n/// doc\nnamespace";
        let mut scanner = Scanner::new(src);
        scanner.skip_trivia();
        // A plain `//` now STOPS the skip — the parser captures it for
        // the writer (rule 5's layout preservation).
        assert!(scanner.at_plain_comment());
        assert_eq!(scanner.take_comment(), " note");
        // After the line ends, trivia skips to the doc block.
        scanner.skip_trivia();
        assert!(scanner.at_doc_comment());
        // `/*`, `#`, and `--` are not SDL trivia: the skip stops at them.
        for head in ["/* block */", "# hash", "-- dash"] {
            let mut scanner = Scanner::new(head);
            scanner.skip_trivia();
            assert_eq!(scanner.peek_byte(), Some(head.as_bytes()[0]));
        }
        let mut scanner = Scanner::new("  /* block */ x");
        scanner.skip_trivia();
        assert_eq!(scanner.peek_byte(), Some(b'/'));
    }

    #[test]
    fn trivia_skips_the_surrealql_whitespace_set() {
        let src = "a\u{00a0}\u{1680}\u{2000}\u{200a}\u{202f}\u{205f}\u{3000}\u{000b}\u{000c}b";
        let mut scanner = Scanner::new(src);
        assert_eq!(scanner.read_word().unwrap().slice(src), "a");
        scanner.skip_trivia();
        assert_eq!(scanner.read_word().unwrap().slice(src), "b");
        assert!(scanner.is_at_end());
    }

    #[test]
    fn words_stop_at_sdl_punctuation_and_reject_digit_starts() {
        let src = "namespace_9 x: 9x";
        let mut scanner = Scanner::new(src);
        assert_eq!(scanner.read_word().unwrap().slice(src), "namespace_9");
        scanner.skip_trivia();
        assert_eq!(scanner.read_word().unwrap().slice(src), "x");
        scanner.skip_trivia();
        scanner.eat_byte(b':');
        scanner.skip_trivia();
        assert_eq!(scanner.read_word(), None);
        assert_eq!(scanner.peek_byte(), Some(b'9'));
    }

    /// Reads one identifier and returns (raw spelling, cursor offset
    /// after it, quoted flag).
    fn ident(src: &str) -> (String, usize, bool) {
        let mut scanner = Scanner::new(src);
        let (span, raw, quoted) = scanner.read_ident().unwrap().expect("ident starts here");
        assert_span_in_bounds(src, span);
        (raw, scanner.offset(), quoted)
    }

    #[test]
    fn idents_read_in_all_three_forms() {
        assert_eq!(ident("acme;"), ("acme".into(), 4, false));
        // Quoted forms swallow `;` and brackets in their interior; the
        // span covers the quotes.
        assert_eq!(ident("`ta;ble`x"), ("ta;ble".into(), 8, true));
        // ⟨ and ⟩ are three bytes each.
        assert_eq!(ident("⟨a b⟩;"), ("a b".into(), 9, true));
        // Keyword words are legal identifier spellings here; filtering
        // bare reserved words is the parser's business.
        assert_eq!(ident("select;"), ("select".into(), 6, false));
        // Nothing consumable: `Ok(None)`, cursor untouched.
        let mut scanner = Scanner::new("9x");
        assert_eq!(scanner.read_ident().unwrap(), None);
        assert_eq!(scanner.peek_byte(), Some(b'9'));
    }

    #[test]
    fn ident_escapes_decode_through_the_surrealql_table() {
        assert_eq!(ident("`a\\nb`").0, "a\nb");
        assert_eq!(ident("`a\\tb`").0, "a\tb");
        assert_eq!(ident("`\\\\`").0, "\\");
        assert_eq!(ident("`a\\`b`").0, "a`b");
        assert_eq!(ident("`a\\u{41}b`").0, "aAb");
        assert_eq!(ident("`a\\u0042b`").0, "aBb");
        // Quoted forms accept anything bare words cannot spell.
        assert_eq!(ident("`9lives`").0, "9lives");
    }

    #[test]
    fn ident_escapes_outside_the_table_are_rejected() {
        for src in ["`a\\qb`", "⟨a\\⟩b⟩", "`a\\u{zz}`", "`a\\u00`"] {
            let mut scanner = Scanner::new(src);
            assert!(
                matches!(scanner.read_ident(), Err(Diag::InvalidIdentEscape { .. })),
                "src: {src}"
            );
        }
    }

    #[test]
    fn unterminated_quoted_ident_through_read_ident() {
        let mut scanner = Scanner::new("`abc");
        assert!(matches!(
            scanner.read_ident(),
            Err(Diag::UnterminatedQuotedIdent { closer: '`', .. })
        ));
    }

    fn assert_span_in_bounds(src: &str, span: Span) {
        assert!(span.offset <= src.len());
        assert!(span.end() <= src.len());
        assert!(src.is_char_boundary(span.offset));
        assert!(src.is_char_boundary(span.end()));
    }

    /// Drives every cursor-advancing public primitive over arbitrary
    /// input: no panics, the cursor stays on an in-bounds character
    /// boundary, and every produced span is a valid slice.
    fn drive_garbage(src: &str) {
        let mut scanner = Scanner::new(src);
        for _ in 0..10_000 {
            assert!(scanner.offset() <= src.len() && src.is_char_boundary(scanner.offset()));
            scanner.skip_trivia();
            if scanner.at_doc_comment() {
                for span in scanner.take_doc_block() {
                    assert_span_in_bounds(src, span);
                }
                continue;
            }
            match scanner.peek_byte() {
                None => break,
                Some(b'{') => {
                    if scanner.capture_balanced_braces().is_err() {
                        break;
                    }
                }
                Some(b';' | b',' | b'}' | b'(' | b')' | b'[' | b']' | b':' | b'$') => {
                    let b = scanner.peek_byte().expect("matched above");
                    assert!(scanner.eat_byte(b));
                }
                Some(b'`') => match scanner.read_ident() {
                    Ok(Some((span, _, _))) => assert_span_in_bounds(src, span),
                    Ok(None) => unreachable!("quote checked"),
                    // A well-formed Diag; this driver just stops.
                    Err(_) => break,
                },
                Some(_) if scanner.rest().starts_with('⟨') => match scanner.read_ident() {
                    Ok(Some((span, _, _))) => assert_span_in_bounds(src, span),
                    Ok(None) => unreachable!("quote checked"),
                    Err(_) => break,
                },
                Some(b'_' | b'a'..=b'z' | b'A'..=b'Z') => {
                    assert_span_in_bounds(src, scanner.read_word().expect("word start checked"));
                }
                Some(_) => match scanner.capture_until(b";") {
                    Ok((span, _)) => assert_span_in_bounds(src, span),
                    // A well-formed Diag; this driver just stops.
                    Err(_) => break,
                },
            }
        }
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(512))]

        #[test]
        fn garbage_never_panics(input in proptest::collection::vec(any::<u8>(), 0..512)) {
            drive_garbage(&String::from_utf8_lossy(&input));
        }

        #[test]
        fn mutated_sdl_never_panics(
            mutations in proptest::collection::vec((0usize..SAMPLE.len(), any::<u8>()), 0..8),
        ) {
            let mut bytes = SAMPLE.as_bytes().to_vec();
            for (idx, byte) in mutations {
                bytes[idx] = byte;
            }
            drive_garbage(&String::from_utf8_lossy(&bytes));
        }

        #[test]
        fn repair_garbage_never_panics(input in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = repair_module_call_spaces(&String::from_utf8_lossy(&input));
        }

        #[test]
        fn repair_mutated_print_never_panics(
            mutations in proptest::collection::vec((0usize..PRINT_SAMPLE.len(), any::<u8>()), 0..8),
        ) {
            let mut bytes = PRINT_SAMPLE.as_bytes().to_vec();
            for (idx, byte) in mutations {
                bytes[idx] = byte;
            }
            let _ = repair_module_call_spaces(&String::from_utf8_lossy(&bytes));
        }
    }

    /// The capture harness the printer-regex pins drive: repeatedly
    /// capture depth-0 segments the way the RETIRED apply splitter
    /// did (`split_statements`, whose merges
    /// motivated the emitter-marks-boundaries redesign). The contract
    /// these pins hold is the capture ENGINE's — its remaining
    /// consumer is [`repair_module_call_spaces`], and the pins are
    /// the regression record for those merges.
    fn printer_segments(script: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut scanner = Scanner::new(script);
        loop {
            scanner.skip_surrealql_trivia();
            if scanner.is_at_end() {
                return out;
            }
            let (span, stop) = scanner
                .capture_until_word_printer(&[])
                .expect("the pin's script is balanced");
            let body = span.slice(script).trim().to_owned();
            if !body.is_empty() {
                out.push(body);
            }
            match stop {
                CaptureStop::Semicolon => {}
                CaptureStop::Word(_) | CaptureStop::End => return out,
            }
        }
    }

    /// The printer-regex extension keeps a regex literal whole — a
    /// `;` and a quote inside one stay inside their segment — while
    /// division never opens; the whitespace-led interior opens at
    /// the DEFAULT keyword and the next statement
    /// still splits. An unterminated literal fails the capture CLOSED
    /// since #60's skip-set arm (SDL-E0001's regex variant, anchored
    /// at the opening `/`) — the retired splitter's old
    /// consume-the-whole-tail-as-one-segment posture predates the
    /// diagnostic and was its own silent-merge hazard.
    #[test]
    fn printer_capture_keeps_regex_literals_whole() {
        assert_eq!(
            printer_segments(
                "DEFINE FIELD OVERWRITE r ON t TYPE regex DEFAULT /a;b c'd/; \
                 DEFINE FIELD OVERWRITE s ON t TYPE int DEFAULT 4 / 2; \
                 DEFINE TABLE OVERWRITE t;",
            ),
            [
                "DEFINE FIELD OVERWRITE r ON t TYPE regex DEFAULT /a;b c'd/",
                "DEFINE FIELD OVERWRITE s ON t TYPE int DEFAULT 4 / 2",
                "DEFINE TABLE OVERWRITE t",
            ],
        );
        assert_eq!(
            printer_segments(
                "DEFINE FIELD OVERWRITE pad ON t TYPE regex DEFAULT / */; \
                 DEFINE TABLE OVERWRITE t2;",
            ),
            [
                "DEFINE FIELD OVERWRITE pad ON t TYPE regex DEFAULT / */",
                "DEFINE TABLE OVERWRITE t2",
            ],
        );
        // The unterminated shape refuses, anchored at the opening `/`.
        let mut scanner = Scanner::new(
            "DEFINE FIELD OVERWRITE r ON t TYPE regex DEFAULT /unterminated; DEFINE TABLE t;",
        );
        let error = scanner
            .capture_until_word_printer(&[])
            .expect_err("the unterminated literal fails closed");
        assert!(matches!(error, Diag::UnterminatedRegex { .. }), "{error}");
    }

    /// The division-predecessor contract, pinned class by class:
    /// every predecessor class the engine parses
    /// in INFIX position leaves the statement boundary alone;
    /// operand-EXPECTED predecessors (after the division operator,
    /// after an operator word, after a clause keyword, after a
    /// RESERVED spelling) open the literal.
    #[test]
    fn printer_capture_opener_follows_the_operand_position_rule() {
        let divisions: &[&str] = &[
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE x / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE 4 / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE 2s / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE (1 + 2) / 3;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE `default` / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE count / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE comment / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE set / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE default / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int DEFAULT default / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE and / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE `where` / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE `set` / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE number VALUE $value / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE x.default / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE NONE / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE true / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE regex DEFAULT /a/ / 2;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE FOO / 2;\nDEFINE TABLE t2;",
        ];
        for case in divisions {
            assert_eq!(
                printer_segments(case).len(),
                2,
                "division must not open: {case}"
            );
        }
        for case in [
            "DEFINE FIELD OVERWRITE v ON t TYPE int VALUE x / /a;b/;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE string VALUE /a/ AND /b;c/;\nDEFINE TABLE t2;",
            "DEFINE FIELD OVERWRITE v ON t TYPE int PERMISSIONS WHERE / */;\nDEFINE TABLE t2;",
        ] {
            assert_eq!(
                printer_segments(case).len(),
                2,
                "must open, not merge: {case}"
            );
        }
    }

    /// The keyword-spelled-operand pin: a NON-reserved keyword-spelled
    /// identifier as a
    /// division operand — the printer prints it bare and the engine
    /// parses it as an ident — splits its segments (pre-fix the opener
    /// read the SPELLING as the keyword and merged the script). Pinned
    /// raw (the silent-Ok probe shape) and compiled (the
    /// probe SDL through the real compiler).
    #[test]
    fn printer_capture_keyword_spelled_operands_divide() {
        let script = "\
             DEFINE FIELD OVERWRITE a ON t TYPE int DEFAULT comment / 2;\n\
             DEFINE TABLE OVERWRITE u TYPE NORMAL SCHEMAFULL;\n\
             DEFINE FIELD OVERWRITE b ON u TYPE int VALUE q / r;\n\
             DEFINE TABLE OVERWRITE w TYPE NORMAL SCHEMAFULL;\n\
             DEFINE FIELD OVERWRITE c ON w TYPE string;\n";
        assert_eq!(printer_segments(script).len(), 5, "{script}");

        let ddl = crate::codegen::emit(
            &crate::load::load(
                std::path::Path::new("schema.surdl"),
                "schema.surdl",
                "database s3r2;\n\
                 \n\
                 table metrics schemafull {\n    \
                 count: int;\n    \
                 avg2: int {\n        \
                 value: count / 2;\n    };\n};\n"
                    .to_owned(),
            )
            .expect("the probe SDL compiles")
            .file,
        );
        assert_eq!(printer_segments(&ddl).len(), 4, "{ddl}");
    }

    /// The end-to-end pin: the exact SDL — a
    /// regex-typed field whose pattern's source leads with whitespace
    /// — compiles to DDL whose space-led literal opens, and every
    /// statement still splits.
    #[test]
    fn printer_capture_splits_the_space_led_regex_field_from_the_compiled_sdl() {
        let ddl = crate::codegen::emit(
            &crate::load::load(
                std::path::Path::new("schema.surdl"),
                "schema.surdl",
                "database s3r1;\n\
                 \n\
                 table t schemafull {\n    \
                 pad: regex {\n        \
                 default: / */;\n    };\n};\n\
                 \n\
                 table t2 schemafull;\n"
                    .to_owned(),
            )
            .expect("the probe SDL compiles")
            .file,
        );
        assert_eq!(
            printer_segments(&ddl),
            [
                "DEFINE DATABASE OVERWRITE s3r1",
                "DEFINE TABLE OVERWRITE t TYPE NORMAL SCHEMAFULL",
                "DEFINE FIELD OVERWRITE pad ON t TYPE regex DEFAULT / */",
                "DEFINE TABLE OVERWRITE t2 TYPE NORMAL SCHEMAFULL",
            ],
        );
    }

    /// The regex interior joins the hand-written capture skip set
    /// (issue #60): an apostrophe inside a regex no longer opens a
    /// phantom string; division still reads as division (the
    /// prime-vs-infix discrimination is the printer repair's own);
    /// an interior `;` stays statement content; an unterminated span
    /// fails the capture closed at its opening `/`.
    #[test]
    fn capture_skips_hand_written_regex_interiors() {
        // The issue's exact repro class: the apostrophe is interior
        // content, the escaped closer is honored, the statement's `;`
        // ends the capture.
        let mut scanner = Scanner::new("assert: $value contains /a'b\\/c/;");
        let (span, stop) = scanner.capture_until(b";").unwrap();
        assert_eq!(
            span.slice("assert: $value contains /a'b\\/c/;"),
            "assert: $value contains /a'b\\/c/"
        );
        assert_eq!(stop, b';');
        // Division: both slashes read infix — the capture ends at the
        // statement's own `;`, not at a phantom regex close.
        let mut scanner = Scanner::new("default: 4 / 2;");
        let (span, stop) = scanner.capture_until(b";").unwrap();
        assert_eq!(span.slice("default: 4 / 2;"), "default: 4 / 2");
        assert_eq!(stop, b';');
        // An interior `;` is regex content — one capture, one stop;
        // the span includes the regex's closing `/` (skipped, not
        // content — the `;` AFTER it stopped the capture).
        let mut scanner = Scanner::new("assert: $v ~ /a;b/;");
        let (span, stop) = scanner.capture_until(b";").unwrap();
        assert_eq!(span.slice("assert: $v ~ /a;b/;"), "assert: $v ~ /a;b/");
        assert_eq!(stop, b';');
        // Unterminated: fail closed at the opening slash.
        let mut scanner = Scanner::new("assert: $v ~ /a'b;");
        let error = scanner.capture_until(b";").unwrap_err();
        assert!(matches!(error, Diag::UnterminatedRegex { .. }), "{error}");
        // The operator-word spelling is case-insensitive (the engine
        // matches keywords that way): lowercase `contains` opens.
        let mut scanner = Scanner::new("assert: $v contains /x'y/;");
        let (span, _) = scanner.capture_until(b";").unwrap();
        assert_eq!(
            span.slice("assert: $v contains /x'y/;"),
            "assert: $v contains /x'y/"
        );
    }
}
