//! The printer-syntax word tables for the regex-opener test — the
//! closed vocabulary [`super::Scanner::skip_regex_literal`]'s word
//! predecessor arm classifies bare words against. THE TRUE RULE
//! mirrors the printer's own escape decision (`EscapeIdent` →
//! `could_be_reserved`, surrealdb-common-3.3.0 `fmt/escape.rs`): the
//! printer backticks an identifier collision against the 44-word
//! RESERVED set and NOTHING else, so a bare word in printer output is
//! an identifier OPERAND unless it spells a reserved word — where it
//! IS the keyword and the parser expects an operand after it. Two
//! derived classes share that "expects an operand after it" property
//! and join the reserved set: the printer's expression-leading clause
//! heads ([`CLAUSE_EXPR_HEADS`], derived from the codegen's own
//! emissions) and the infix word operators (the lexer's
//! `OPERATOR_WORDS`). Every other spelling — `count`, `comment`,
//! `set` — is an identifier: the engine accepts EVERY keyword token
//! as an ident (`surrealdb-syn-3.3.0` `parser/token.rs`
//! `kind_is_identifier`), so a non-reserved keyword-spelled operand
//! prints bare and simply ends its operand (`count / 2` is division).
//! This module FIRST mirrored the engine's full 363-entry `KEYWORDS`
//! map and called every keyword spelling the keyword — a false
//! premise (the full map is the LEXER's vocabulary, not the
//! printer's escape set) that misread exactly those divisions as
//! regex opens and merged apply scripts; the mirror moved
//! out and what remains is the printer's own decision.
//!
//! Source of truth: `surrealdb-common-3.3.0/src/keywords.rs`'s
//! `RESERVED_KEYWORD` phf_set (44 entries; the engine matches them
//! UniCase — ASCII case-insensitively — and so do these tables).
//! Deliberately NOT re-exported from the crate anywhere else: these
//! are the splitter's closed tables, re-derived by hand at the next
//! floor move (a `=3.3.0` bump is a deliberate spec decision). The
//! engine's full keyword
//! vocabulary lives on separately at [`crate::keywords`]
//! (`KEYWORDS_3_3_0` — the SDL-side tooling's table).

/// The vendored RESERVED spellings — `could_be_reserved`'s exact set,
/// lowercased and sorted: the words the printer backticks an
/// identifier collision against, so a bare occurrence in printer
/// output is always the keyword itself. `NONE`/`NULL`/`true`/`false`
/// are IN this set but classify EARLIER in the opener (operand-valued
/// keyword literals — upstream's `Literal` print emits them bare as
/// complete operands, not as escaped identifiers).
pub(crate) static RESERVED_KEYWORDS: &[&str] = &[
    "after", "all", "alter", "before", "begin", "break", "by", "cancel", "commit", "continue",
    "create", "define", "delete", "diff", "false", "for", "function", "if", "info", "insert",
    "kill", "let", "live", "none", "null", "option", "rand", "rebuild", "relate", "remove",
    "return", "select", "sequence", "show", "sleep", "table", "tb", "throw", "true", "update",
    "upsert", "use", "value", "where",
];

/// The printer's expression-leading clause heads spelled OUTSIDE the
/// reserved set — every expression clause emission of the codegen's
/// DEFINE printer (`src/codegen.rs`'s `Seg::value` clauses), each of
/// whose positions the SDL parses as a free expression
/// (`ClauseKind::Expr`), so a slash-led or space-led REGEX can
/// legally follow: `DEFAULT`/`DEFAULT ALWAYS` (field default),
/// `ASSERT`/`COMPUTED` (field), `WHEN`/`THEN` (event, and
/// `REFERENCE ON DELETE THEN`), `SIGNUP`/`SIGNIN`/`AUTHENTICATE`/
/// `CONTEXT` (access), `BACKEND` (bucket), `BATCH`/`START`/`TIMEOUT`
/// (sequence), `NAMESPACE`/`DATABASE` (config default), and `ELSE`
/// (an IF/ELSE expression's branch). The reserved spellings that
/// share the class — `VALUE` (field/param value), `WHERE`
/// (permissions) — ride [`RESERVED_KEYWORDS`] already.
///
/// An OPERAND ident spelled exactly like one of these is the word
/// rule's ambiguity residue: the predecessor recursion (a clause head
/// whose own predecessor expects an operand is the ident) resolves
/// every single-event shape (`VALUE default / 2` divides); the
/// COMPOUND — such an ident dividing BY a space-led regex — is the
/// ledgered corner (tracker #62).
pub(crate) static CLAUSE_EXPR_HEADS: &[&str] = &[
    "always",
    "assert",
    "authenticate",
    "backend",
    "batch",
    "computed",
    "context",
    "database",
    "default",
    "else",
    "namespace",
    "signin",
    "signup",
    "start",
    "then",
    "timeout",
    "when",
];

/// Whether `word` (any case — the engine folds keyword case) spells a
/// RESERVED word: the printer's own escape predicate, mirrored.
pub(crate) fn is_reserved_word(word: &str) -> bool {
    RESERVED_KEYWORDS
        .binary_search(&word.to_ascii_lowercase().as_str())
        .is_ok()
}

/// Whether `word` (any case) spells one of the printer's
/// expression-leading clause heads ([`CLAUSE_EXPR_HEADS`]).
pub(crate) fn is_clause_expr_head(word: &str) -> bool {
    CLAUSE_EXPR_HEADS
        .binary_search(&word.to_ascii_lowercase().as_str())
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reserved mirror is exactly the vendored 44 — byte-exact
    /// against `surrealdb-common-3.3.0`'s `RESERVED_KEYWORD` phf_set
    /// (this pins the count, the sort
    /// the binary search needs, and the load-bearing members).
    #[test]
    fn reserved_mirror_is_the_vendored_44() {
        assert_eq!(RESERVED_KEYWORDS.len(), 44);
        assert!(RESERVED_KEYWORDS.windows(2).all(|w| w[0] < w[1]));
        // The membership pins: WHERE is in (a
        // WHERE-position regex still opens), the four operand-valued
        // literals are in (and classify earlier), and the
        // trigger spellings are NOT (bare operands, division).
        assert!(is_reserved_word("where"));
        assert!(is_reserved_word("value"));
        for literal in ["none", "null", "true", "false"] {
            assert!(is_reserved_word(literal));
        }
        for operand in ["count", "comment", "set", "and", "default", "type"] {
            assert!(!is_reserved_word(operand), "{operand}");
        }
    }

    /// The clause-head table is sorted (binary search) and carries
    /// exactly the derivation's members; the reserved spellings that
    /// share the class (`VALUE`, `WHERE`) must NOT be duplicated in
    /// it — they ride the reserved mirror.
    #[test]
    fn clause_heads_are_sorted_and_disjoint_from_reserved() {
        assert_eq!(CLAUSE_EXPR_HEADS.len(), 17);
        assert!(CLAUSE_EXPR_HEADS.windows(2).all(|w| w[0] < w[1]));
        for head in [
            "default", "when", "then", "else", "always", "signin", "backend",
        ] {
            assert!(is_clause_expr_head(head));
        }
        for word in RESERVED_KEYWORDS {
            assert!(!is_clause_expr_head(word), "{word}");
        }
    }
}
