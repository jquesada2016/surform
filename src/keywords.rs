//! SurrealQL identifier/keyword escaping — the shared home for the
//! keyword table and the string/ident escape rules both writers ride.
//! Extracted from `codegen.rs`: the canonical SDL writer
//! ([`crate::info::write`]) needs the same backtick set and the same
//! `EscapeWriter` table as DDL emission, and one home keeps the two
//! from drifting.
//!
//! Emission mirrors the engine's own writers byte for byte where the
//! two meet: `QuoteStr` + `EscapeWriter` (`fmt/escape.rs`) for
//! string literals, and the ident-escaping rules of `EscapeIdent` /
//! `EscapeKwFreeIdent` — with the spec's codegen-model widening that
//! the backtick set is the supported target's *full keyword set*
//! (every keyword spelling the engine's lexer classifies, not merely
//! the reserved subset), so the same identifier emits the same way
//! wherever it can appear. Byte-identity verified against the v3.2.4
//! writers; re-verified against 3.3.0 at the floor move
//! (the oracle round-trips and fmt goldens are the net).

/// Every keyword spelling the 3.3.0 SurrealQL lexer classifies as a
/// keyword token (`surrealdb-syn/src/lexer/keywords.rs`, `KEYWORDS`;
/// extracted verbatim, case-folded — alias and duplicate-case
/// spellings collapse; extraction must match BOTH the upper- and
/// lowercase `UniCase::ascii` spellings in that file — then
/// byte-order sorted: 363 spellings, including alias mappings like
/// `ns`/`db`/`kv`, snake_case entries like `inner_product`, and
/// SDL's own literal-grammar words `true`/`false`/`fn`/`mod`/`silo`).
/// A word from
/// this set cannot emit bare: in most name positions the parser has
/// no bare-keyword expression to fall back on, and escaping is
/// always accepted. Per the codegen model this is the supported
/// target's full keyword set; a floor bump re-extracts it from that
/// lexer.
pub(crate) static KEYWORDS_3_3_0: &[&str] = &[
    "access",
    "after",
    "algorithm",
    "all",
    "allinside",
    "alter",
    "always",
    "analyzer",
    "and",
    "andkw",
    "any",
    "anyinside",
    "api",
    "ar",
    "ara",
    "arabic",
    "array",
    "as",
    "asc",
    "ascending",
    "ascii",
    "assert",
    "async",
    "at",
    "audience",
    "authenticate",
    "auto",
    "backend",
    "batch",
    "bearer",
    "before",
    "begin",
    "blank",
    "bm25",
    "bool",
    "break",
    "bucket",
    "by",
    "bytes",
    "camel",
    "cancel",
    "capacity",
    "cascade",
    "changefeed",
    "changes",
    "chebyshev",
    "chinese",
    "class",
    "collate",
    "collection",
    "columns",
    "comment",
    "commit",
    "compact",
    "computed",
    "concurrently",
    "config",
    "contains",
    "containsall",
    "containsany",
    "containsnone",
    "containsnot",
    "content",
    "context",
    "continue",
    "cosine",
    "cosine_normalized",
    "count",
    "create",
    "da",
    "dan",
    "danish",
    "database",
    "datetime",
    "db",
    "de",
    "decimal",
    "default",
    "define",
    "delete",
    "desc",
    "descending",
    "deu",
    "diff",
    "dimension",
    "dist",
    "distance",
    "drop",
    "duplicate",
    "duration",
    "dutch",
    "eddsa",
    "edgengram",
    "edges",
    "efc",
    "el",
    "ell",
    "else",
    "en",
    "end",
    "enforced",
    "eng",
    "english",
    "es",
    "es256",
    "es384",
    "es512",
    "euclidean",
    "event",
    "exclude",
    "exists",
    "expired",
    "explain",
    "expunge",
    "extend_candidates",
    "f16",
    "f32",
    "f64",
    "false",
    "feature",
    "fetch",
    "fi",
    "field",
    "fields",
    "file",
    "filters",
    "fin",
    "finnish",
    "flex",
    "flexi",
    "flexible",
    "float",
    "fn",
    "for",
    "fr",
    "fra",
    "french",
    "from",
    "full",
    "fulltext",
    "function",
    "functions",
    "geometry",
    "german",
    "get",
    "grant",
    "graphql",
    "graphql_alias",
    "graphql_deprecated",
    "greek",
    "group",
    "hamming",
    "hashed_vector",
    "headers",
    "highlights",
    "hnsw",
    "hs256",
    "hs384",
    "hs512",
    "hu",
    "hun",
    "hungarian",
    "i16",
    "i32",
    "i64",
    "i8",
    "if",
    "ignore",
    "in",
    "include",
    "index",
    "info",
    "inline",
    "inner_product",
    "insert",
    "inside",
    "int",
    "intersects",
    "into",
    "is",
    "issuer",
    "it",
    "ita",
    "italian",
    "jaccard",
    "japanese",
    "jwks",
    "jwt",
    "keep_pruned_connections",
    "key",
    "kill",
    "korean",
    "kv",
    "let",
    "lightweight",
    "limit",
    "line",
    "live",
    "lm",
    "lowercase",
    "m",
    "m0",
    "manhattan",
    "mapper",
    "maxdepth",
    "merge",
    "middleware",
    "minkowski",
    "ml",
    "mod",
    "model",
    "module",
    "multiline",
    "multipoint",
    "multipolygon",
    "namespace",
    "ngram",
    "nl",
    "nld",
    "no",
    "noindex",
    "none",
    "noneinside",
    "nor",
    "normal",
    "norwegian",
    "not",
    "notinside",
    "ns",
    "null",
    "number",
    "numeric",
    "object",
    "omit",
    "on",
    "only",
    "option",
    "or",
    "order",
    "original",
    "out",
    "outside",
    "overwrite",
    "parallel",
    "param",
    "passhash",
    "passscram",
    "password",
    "patch",
    "pearson",
    "permissions",
    "point",
    "polygon",
    "por",
    "portuguese",
    "post",
    "postings_cache",
    "postings_order",
    "prepare",
    "ps256",
    "ps384",
    "ps512",
    "pt",
    "punct",
    "purge",
    "put",
    "rand",
    "range",
    "readonly",
    "rebuild",
    "record",
    "reference",
    "references",
    "refresh",
    "regex",
    "reject",
    "relate",
    "relation",
    "remove",
    "replace",
    "retry",
    "return",
    "revoke",
    "revoked",
    "ro",
    "roles",
    "romanian",
    "ron",
    "root",
    "rs256",
    "rs384",
    "rs512",
    "ru",
    "rus",
    "russian",
    "sc",
    "schemaful",
    "schemafull",
    "schemaless",
    "segment",
    "select",
    "sequence",
    "session",
    "set",
    "show",
    "signin",
    "signup",
    "silo",
    "since",
    "sleep",
    "snowball",
    "spa",
    "spanish",
    "split",
    "start",
    "strict",
    "string",
    "structure",
    "sv",
    "swe",
    "swedish",
    "system",
    "ta",
    "table",
    "tables",
    "tam",
    "tamil",
    "tb",
    "tempfiles",
    "terms_cache",
    "terms_order",
    "then",
    "throw",
    "timeout",
    "to",
    "token",
    "tokenizers",
    "tr",
    "trace",
    "transaction",
    "true",
    "tur",
    "turkish",
    "type",
    "u8",
    "ulid",
    "unique",
    "unset",
    "unsigned",
    "update",
    "uppercase",
    "upsert",
    "url",
    "use",
    "user",
    "uuid",
    "value",
    "values",
    "version",
    "vs",
    "when",
    "where",
    "with",
];

/// A SurrealQL string literal, after v3.2.4 `QuoteStr`: quoted with
/// `"` only when the value contains `'`, and escaped through
/// [`escape_with`] — `\0 \r \t \n` (spellings), `\u{8}`, `\f`, `\\`,
/// and the quote character itself; every other character is verbatim.
pub(crate) fn quoted_string(out: &mut String, value: &str) {
    let quote = if value.contains('\'') { '"' } else { '\'' };
    out.push(quote);
    escape_with(out, value, quote);
    out.push(quote);
}

/// The `EscapeWriter` table from v3.2.4 `core/src/fmt/escape.rs`:
/// control characters get their SurrealQL spellings, `\` doubles, and
/// `escape` (the active quote or backtick) is backslashed.
pub(crate) fn escape_with(out: &mut String, value: &str, escape: char) {
    for c in value.chars() {
        match c {
            '\0' => out.push_str("\\0"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{8}' => out.push_str("\\u{8}"),
            '\u{c}' => out.push_str("\\f"),
            '\\' => out.push_str("\\\\"),
            c if c == escape => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
}

/// An identifier must be backtick-escaped when it is not a plain
/// word, after v3.2.4 `EscapeKwFreeIdent`: empty, digit-led, holding
/// any character outside `[A-Za-z0-9_]`, or spelled `NaN`/`Infinity`
/// (constant literals) — or when it collides with the keyword table
/// above (ASCII-case-insensitively, like the SurrealQL lexer).
pub(crate) fn ident_needs_escape(raw: &str) -> bool {
    raw.is_empty()
        || raw.starts_with(|c: char| c.is_ascii_digit())
        || raw == "NaN"
        || raw == "Infinity"
        || raw
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_'))
        || (raw.is_ascii() && {
            let lower = raw.to_ascii_lowercase();
            KEYWORDS_3_3_0.binary_search(&lower.as_str()).is_ok()
        })
}

/// The SDL-side additions to the keyword table: words SDL's own
/// grammar treats specially in a name position that the SurrealQL
/// keyword set does not already cover. `raw` is the statement
/// keyword (reserved in every block context); `view` and `diskann`
/// are table/index variant words a header name position could
/// collide with — the remaining variant words (`any`, `relation`,
/// `unique`, `count`, `fulltext`, `hnsw`, `normal`) and every clause
/// key ARE SurrealQL keywords, so [`ident_needs_escape`] covers them.
/// The canonical SDL writer escapes these alongside the keyword
/// table — quoted identifiers are always names.
pub(crate) fn sdl_ident_needs_escape(raw: &str) -> bool {
    matches!(raw, "raw" | "view" | "diskann") || ident_needs_escape(raw)
}

/// The `EscapeKwFreeIdent` SHAPE rule (upstream's ident-adjacent
/// positions — param names after `$`, `<fn-path>` segments, kind
/// interior names and object keys): a plain
/// `[A-Za-z_][A-Za-z0-9_]*` word can emit bare — keyword collisions
/// stay bare (`$table`, `fn::class`, probe-verified) — except the
/// `NaN`/`Infinity` constant-literal spellings, which the ident
/// positions of upstream's DDL grammar refuse (its lexer classifies
/// them as dedicated token kinds) and which therefore
/// backtick-escape. One home: three call sites (codegen's word-shaped
/// emission, the SDL writer's param/segment/shape positions, the kind
/// printer's names and object keys) shared it as hand-copies and the
/// kind printer's copy is exactly where the exclusion went missing
/// once. (The `$` param position itself re-lexes bare `$NaN`
/// identically upstream — the exclusion is conservative and safe
/// there.)
pub(crate) fn word_shaped(raw: &str) -> bool {
    !raw.is_empty()
        && raw != "NaN"
        && raw != "Infinity"
        && !raw.starts_with(|c: char| c.is_ascii_digit())
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ident_needs_escape` binary-searches the table: sortedness
    /// (and, with strict `<`, uniqueness) is load-bearing. Also pins
    /// the exact spelling count and the membership of both the words
    /// the unit's tests rely on and the lowercase/snake_case spellings
    /// an uppercase-only extraction once missed.
    #[test]
    fn keyword_table_is_sorted_unique_and_populated() {
        assert!(KEYWORDS_3_3_0.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(
            KEYWORDS_3_3_0.len(),
            363,
            "the 3.3.0 keyword set changed — re-extract"
        );
        for word in ["namespace", "ns", "select", "table", "use"] {
            assert!(ident_needs_escape(word), "`{word}` must be in the table");
        }
        // The 3.3.0 acquisitions (re-extracted at the floor move).
        // (`true`/`false`/`fn`/`mod`/`silo` are plain members — the
        // 3.3.0 map keeps them classified as keywords.)
        for word in [
            "audience",
            "context",
            "edges",
            "inline",
            "lightweight",
            "passscram",
            "unsigned",
            "false",
            "fn",
            "mod",
            "silo",
            "true",
            "inner_product",
            "terms_order",
        ] {
            assert!(ident_needs_escape(word), "`{word}` must be in the table");
        }
        for word in ["acme", "staging", "order_number"] {
            assert!(!ident_needs_escape(word), "`{word}` must stay bare");
        }
    }

    /// The SDL reservations ride on top of the keyword table: the
    /// SDL-only words (`raw`, and the variant words the SurrealQL
    /// lexer does not classify as keywords — `view`, `diskann`)
    /// escape, ordinary words stay bare, and the variant/clause words
    /// covered by the keyword table stay keyword-driven (pinned here
    /// so a table re-extraction that drops one resurfaces as a writer
    /// decision).
    #[test]
    fn sdl_reservations_escape_beyond_the_table() {
        assert!(sdl_ident_needs_escape("raw"));
        assert!(sdl_ident_needs_escape("view"));
        assert!(sdl_ident_needs_escape("diskann"));
        assert!(sdl_ident_needs_escape("any"));
        assert!(sdl_ident_needs_escape("relation"));
        assert!(sdl_ident_needs_escape("unique"));
        assert!(!ident_needs_escape("view"), "`view` is SDL-only");
        assert!(!ident_needs_escape("diskann"), "`diskann` is SDL-only");
        assert!(!sdl_ident_needs_escape("acme"));
        assert!(!sdl_ident_needs_escape("id"));
    }
}
