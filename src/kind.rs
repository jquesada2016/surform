//! The SDL kind grammar (README rule 4):
//! the family is text-in/text-out and serves
//! two statement headers (`function`'s parameter/return kinds, the
//! `field` header type) plus field's flexible check.

use crate::keywords::escape_with;
use crate::lexer::{Scanner, lex_string_literal, strip_capture_trivia, word_prefix_len};

// ---- The SDL kind parser (README rule 4) ----
//
// Types are SDL-lexed, never expression-captured: these functions
// mirror v3.2.4's kind grammar (`syn/parser/kind.rs`, the authority
// probed 2026-09-20) structurally and carry the source spelling
// verbatim — the embedded printer's own kind normalization
// (`option<T>` → `none | T`) is deliberately not adopted, since the
// source spelling re-parses identically. All the `read_kind*`
// helpers take trivia-stripped text and return the tail after the
// kind they consumed; callers strip between tokens. Trivia is legal
// anywhere SurrealQL puts a token boundary (upstream accepts
// `record < person >` and `/* t */ record<post>` alike).

/// The bare kind heads: ASCII-case-insensitive SurrealQL keywords
/// (`t!("BOOL")` et al). `point` is upstream's own bare spelling of
/// `geometry<point>`; `true`/`false` are literal kinds in keyword
/// position. The generic heads (`option record table geometry file
/// array set`) are handled structurally.
const KIND_BARE_HEADS: &[&str] = &[
    "bool", "bytes", "datetime", "decimal", "duration", "float", "function", "int", "none", "null",
    "number", "object", "point", "range", "regex", "string", "true", "false", "uuid",
];

/// Whether a kind grammar accepts Capitalized alias references at
/// kind positions (spec/type-alias.md). [`KindMode::Strict`] is the
/// reverse reader's grammar — the database stores only concrete
/// kinds, so INFO never carries an alias name and the reader stays
/// fail-closed. [`KindMode::WithAliases`] is every forward capture's
/// grammar: an identifier whose first char is uppercase and whose
/// ASCII-lowercase form is NOT a reserved head parses as
/// [`KindAst::Alias`]. Lowercase unknown words are rejected in BOTH
/// modes (the SDL-E1026 class is untouched).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KindMode {
    Strict,
    WithAliases,
}

/// An alias reference's word shape: `[A-Z][A-Za-z0-9_]*` — the whole
/// name, decoded (quoted declaration spellings decode first). The
/// Capitalized rule is what keeps alias uses grammatically distinct
/// from SurrealQL's lowercase kind heads (only the exact
/// `NaN`/`Infinity` literal spellings share the territory, and
/// [`is_reserved_alias_name`] rejects those).
fn is_alias_shaped(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The heads an alias name may not take, ASCII-case-insensitively:
/// every kind head would win at the use site (head matching is
/// ASCII-case-insensitive) and leave the alias unreachable. Only
/// `NaN`/`Infinity` are exact-case (the constant-literal spellings).
pub(crate) fn is_reserved_alias_name(name: &str) -> bool {
    // The constant-literal spellings are exact-case (lowercase `nan`
    // is an identifier upstream); every other head matches
    // ASCII-case-insensitively.
    if matches!(name, "NaN" | "Infinity") {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    KIND_BARE_HEADS.contains(&lower.as_str())
        || matches!(
            lower.as_str(),
            "record" | "table" | "geometry" | "file" | "array" | "set" | "option" | "any"
        )
}

/// A declaration's name is valid when it is alias-shaped and not
/// reserved — every declared alias is then usable at every alias
/// position (a bare word decision can never miss it).
pub(crate) fn is_valid_alias_name(name: &str) -> bool {
    is_alias_shaped(name) && !is_reserved_alias_name(name)
}

/// `geometry<…>`'s interiors.
const GEOMETRY_WORDS: &[&str] = &[
    "collection",
    "line",
    "multipoint",
    "multiline",
    "multipolygon",
    "point",
    "polygon",
];

/// Duration unit suffixes, longest first (`ms` before `m`).
const KIND_DURATION_UNITS: &[&str] = &["µs", "ns", "us", "ms", "s", "m", "h", "d", "w", "y"];

/// Parses one kind production (README rule 4) and returns the
/// trimmed source spelling — codegen prints it verbatim. The FORWARD
/// captures' entry (field kinds, function returns): alias references
/// parse (spec/type-alias.md); the reverse reader's strict grammar is
/// [`read_kind`].
pub(crate) fn parse_kind_text(text: &str) -> Option<String> {
    let start = strip_capture_trivia(text)?;
    let tail = read_kind_mode(start, KindMode::WithAliases)?;
    strip_capture_trivia(tail)?
        .is_empty()
        .then(|| start[..start.len() - tail.len()].to_string())
}

/// A kind union: one single kind, then `|`-joined concrete kinds.
/// `any` stands alone — upstream's `parse_inner_kind` returns before
/// the union loop, leaving the `|` for the caller to choke on, so
/// `any | int` is a parse error there and here alike. STRICT: the
/// reverse reader's entry (INFO kinds are always concrete).
pub(crate) fn read_kind(text: &str) -> Option<&str> {
    read_kind_mode(text, KindMode::Strict)
}

/// The forward entry for function-argument kinds (the fn header's
/// capture carries the text, so only validation is needed):
/// [`read_kind`]'s shape with alias references legal.
pub(crate) fn read_kind_with_aliases(text: &str) -> Option<&str> {
    read_kind_mode(text, KindMode::WithAliases)
}

fn read_kind_mode(text: &str, mode: KindMode) -> Option<&str> {
    let (head, tail) = read_kind_single(text, mode)?;
    if head.eq_ignore_ascii_case("any") {
        return Some(tail);
    }
    kind_union_tail(tail, mode)
}

/// The `|`-joined continuation shared by the top-level union and the
/// `option<…>` interior's (members are concrete kinds in both).
fn kind_union_tail(mut tail: &str, mode: KindMode) -> Option<&str> {
    loop {
        let rest = strip_capture_trivia(tail)?;
        let Some(rest) = rest.strip_prefix('|') else {
            return Some(tail);
        };
        tail = read_kind_concrete(strip_capture_trivia(rest)?, mode)?;
    }
}

/// One single kind: `any`, `option<…>`, or a concrete kind.
fn read_kind_single(text: &str, mode: KindMode) -> Option<(&str, &str)> {
    // Digit- and sign-led text is a literal kind, never a word head
    // (`word_prefix_len` counts digits — `1h` is not a word).
    match text.chars().next()? {
        c if c.is_ascii_alphabetic() || c == '_' => {
            let len = word_prefix_len(text);
            match text[..len].to_ascii_lowercase().as_str() {
                "any" => Some((&text[..len], &text[len..])),
                "option" => read_kind_angle(text, len, mode),
                // The literal spellings are case-SENSITIVE (upstream's
                // lexer compares them exactly — lowercase `nan` is an
                // identifier).
                "nan" if &text[..len] == "NaN" => Some((&text[..len], &text[len..])),
                "infinity" if &text[..len] == "Infinity" => Some((&text[..len], &text[len..])),
                _ => read_kind_concrete_word(text, len, mode),
            }
        }
        _ => read_kind_literal(text, mode),
    }
}

/// A concrete kind — everything but `any`/`option` (upstream's
/// `parse_concrete_kind`), the only members a union may join.
fn read_kind_concrete(text: &str, mode: KindMode) -> Option<&str> {
    match text.chars().next()? {
        c if c.is_ascii_alphabetic() || c == '_' => {
            read_kind_concrete_word(text, word_prefix_len(text), mode).map(|(_, tail)| tail)
        }
        _ => read_kind_literal(text, mode).map(|(_, tail)| tail),
    }
}

fn read_kind_concrete_word(text: &str, len: usize, mode: KindMode) -> Option<(&str, &str)> {
    match text[..len].to_ascii_lowercase().as_str() {
        // Excluded from concrete position: `option` has no arm in
        // upstream's concrete parser and `any` returns early above.
        "any" | "option" => None,
        "nan" if &text[..len] == "NaN" => Some((&text[..len], &text[len..])),
        "infinity" if &text[..len] == "Infinity" => Some((&text[..len], &text[len..])),
        _ if KIND_BARE_HEADS.contains(&text[..len].to_ascii_lowercase().as_str()) => {
            Some((&text[..len], &text[len..]))
        }
        "record" | "table" | "geometry" | "file" | "array" | "set" => {
            read_kind_angle(text, len, mode)
        }
        // The alias fallthrough (spec/type-alias.md): a Capitalized
        // word at a kind position references the document's alias of
        // that name. Bare words only — a quoted spelling at kind
        // position stays invalid, and lowercase words stay the
        // SDL-E1026 rejection in every mode.
        _ if mode == KindMode::WithAliases && is_alias_shaped(&text[..len]) => {
            Some((&text[..len], &text[len..]))
        }
        _ => None,
    }
}

/// A generic head, its `<…>` interior, and the closing `>` (trivia
/// allowed around both, like any token boundary). The `<` is
/// optional for every head but `option` (upstream `expected!(<)`
/// there); interiors:
/// `option` — a concrete union (no nested `option`, no `any`:
/// upstream parses `parse_concrete_kind` inside, so
/// `option<option<int>>` is a parse error);
/// `record`/`table`/`file` — `|`-joined identifiers;
/// `geometry` — `|`-joined geometry keywords;
/// `array`/`set` — a full union plus an optional `, <size>`.
/// `head_len` is the head word's length in `text`; the returned head
/// spelling runs from the head through the `>`.
fn read_kind_angle(text: &str, head_len: usize, mode: KindMode) -> Option<(&str, &str)> {
    let head = &text[..head_len];
    let tail = &text[head_len..];
    let rest = strip_capture_trivia(tail)?;
    let Some(rest) = rest.strip_prefix('<') else {
        return (!head.eq_ignore_ascii_case("option")).then_some((head, tail));
    };
    let interior_tail = match head.to_ascii_lowercase().as_str() {
        "option" => {
            let first = read_kind_concrete(strip_capture_trivia(rest)?, mode)?;
            kind_union_tail(first, mode)?
        }
        // Name/geometry lists take no mode: a `record<Email>` interior
        // is a table NAME at every case, never an alias reference.
        "record" | "table" | "file" => read_kind_name_list(rest)?,
        "geometry" => read_kind_geometry_list(rest)?,
        _ => read_kind_sized(rest, mode)?,
    };
    let close = strip_capture_trivia(interior_tail)?.strip_prefix('>')?;
    Some((&text[..text.len() - close.len()], close))
}

/// `record<T | T…>` / `table<T | T…>` / `file<B | B…>`: at least one
/// name, `|`-separated (never commas).
fn read_kind_name_list(mut rest: &str) -> Option<&str> {
    loop {
        rest = read_kind_name(strip_capture_trivia(rest)?)?;
        match strip_capture_trivia(rest)?.strip_prefix('|') {
            Some(tail) => rest = tail,
            None => return Some(rest),
        }
    }
}

/// One interior name: any identifier form (upstream `parse_ident` —
/// keyword-like words bare, quoted spellings decoded) EXCEPT the bare
/// `NaN`/`Infinity` constant-literal spellings: upstream's lexer
/// classifies them as dedicated token kinds that ident positions
/// refuse, and codegen prints `Kind.text` verbatim — accepting them
/// bare would emit DDL that no longer parses (the "the source
/// spelling re-parses identically" invariant, ast.rs `Kind`). Quoted
/// forms decode to the same names and stay accepted.
fn read_kind_name(text: &str) -> Option<&str> {
    match Scanner::new(text).read_ident() {
        Ok(Some((span, decoded, quoted)))
            if quoted || !matches!(decoded.as_str(), "NaN" | "Infinity") =>
        {
            Some(&text[span.end()..])
        }
        _ => None,
    }
}

/// `geometry<G | G…>`: the same list shape over the fixed
/// geometry-keyword set.
fn read_kind_geometry_list(mut rest: &str) -> Option<&str> {
    loop {
        rest = read_kind_geometry_name(strip_capture_trivia(rest)?)?;
        match strip_capture_trivia(rest)?.strip_prefix('|') {
            Some(tail) => rest = tail,
            None => return Some(rest),
        }
    }
}

fn read_kind_geometry_name(text: &str) -> Option<&str> {
    let len = word_prefix_len(text);
    let word = &text[..len];
    (len > 0 && GEOMETRY_WORDS.contains(&word.to_ascii_lowercase().as_str()))
        .then_some(&text[len..])
}

/// `array<K(, N)?>` / `set<K(, N)?>`: the element kind is a full
/// union (`array<option<int> | string>` parses upstream), the size
/// an optional number or duration literal.
fn read_kind_sized(mut rest: &str, mode: KindMode) -> Option<&str> {
    rest = read_kind_mode(strip_capture_trivia(rest)?, mode)?;
    let Some(after_comma) = strip_capture_trivia(rest)?.strip_prefix(',') else {
        return Some(rest);
    };
    read_kind_size(strip_capture_trivia(after_comma)?)
}

/// The `array<T, N>` size — upstream `next_token_value` in
/// `Option<i64>` position accepts INTEGERS ONLY here (probe-pinned:
/// `-1`, `1.5`, `1e2`, and `1h` all die as size spellings; `1_0` is
/// the one alternate shape): an unsigned digit run with `_`
/// separators. Whatever follows must be the closing `>` — the angle
/// reader's check.
fn read_kind_size(text: &str) -> Option<&str> {
    let digits = kind_digit_run(text)?;
    (digits > 0).then_some(&text[digits..])
}

/// An unsigned duration compound consumed from the front (`1h30m`):
/// digit runs glued to unit suffixes, `_` separators between digits.
fn kind_duration_tail(mut rest: &str) -> Option<&str> {
    loop {
        let digits = kind_digit_run(rest)?;
        if digits == 0 {
            return None;
        }
        rest = &rest[digits..];
        let unit = KIND_DURATION_UNITS.iter().find(|u| rest.starts_with(**u))?;
        rest = &rest[unit.len()..];
        if !rest.starts_with(|c: char| c.is_ascii_digit()) {
            return Some(rest);
        }
    }
}

/// A digit run with `_` separators. Upstream `lex_digits` admits a
/// trailing `_` in compound positions (`1_` is the integer 1); a
/// LEADING `_` starts an identifier, not digits. The run length,
/// `Some(0)` when there are no digits.
fn kind_digit_run(text: &str) -> Option<usize> {
    if text.starts_with('_') {
        return None;
    }
    Some(
        text.char_indices()
            .find(|(_, c)| !(c.is_ascii_digit() || *c == '_'))
            .map(|(i, _)| i)
            .unwrap_or(text.len()),
    )
}

/// The literal kinds: strings, signed numbers, durations, and the
/// brace-bearing object/array literals.
fn read_kind_literal(text: &str, mode: KindMode) -> Option<(&str, &str)> {
    match text.chars().next()? {
        '\'' | '"' => {
            let (_, rest) = lex_string_literal(text)?;
            Some((&text[..text.len() - rest.len()], rest))
        }
        '{' => {
            read_kind_object(&text[1..], mode).map(|tail| (&text[..text.len() - tail.len()], tail))
        }
        '[' => read_kind_array_lit(&text[1..], mode)
            .map(|tail| (&text[..text.len() - tail.len()], tail)),
        '+' | '-' | '0'..='9' => {
            let tail = kind_number_or_duration_tail(text)?;
            Some((&text[..text.len() - tail.len()], tail))
        }
        _ => None,
    }
}

/// The digit/sign-led literal kinds (upstream: the sign arm lexes
/// `compound::number`, the digits arm `compound::numeric`): unsigned
/// duration compounds, numbers with the `f` (float) / `dec` (decimal)
/// suffixes, and `Infinity` after a sign.
fn kind_number_or_duration_tail(text: &str) -> Option<&str> {
    match text.chars().next()? {
        '+' | '-' => kind_number_compound_tail(text),
        // `dec` reads before the `d` duration unit does: `1dec` is one
        // decimal compound upstream, not `1d` plus junk.
        _ => kind_dec_tail(text)
            .or_else(|| kind_duration_tail(text).or_else(|| kind_number_compound_tail(text))),
    }
}

/// A `dec`-suffixed decimal consumed from the front.
fn kind_dec_tail(mut rest: &str) -> Option<&str> {
    let digits = kind_digit_run(rest)?;
    if digits == 0 {
        return None;
    }
    rest = &rest[digits..];
    if let Some(after_dot) = rest.strip_prefix('.') {
        let frac = kind_digit_run(after_dot)?;
        if frac > 0 {
            rest = &after_dot[frac..];
        }
    }
    rest.strip_prefix("dec")
}

/// A number compound consumed from the front (upstream
/// `compound::number`): optional sign — `Infinity` (exact case) may
/// follow it directly — digits with `_` separators, optional `.`
/// fraction and `e`/`E` exponent, and the `f`/`dec` suffixes.
fn kind_number_compound_tail(mut rest: &str) -> Option<&str> {
    if matches!(rest.chars().next(), Some('+') | Some('-')) {
        rest = &rest[1..];
        if let Some(after) = rest.strip_prefix("Infinity") {
            return Some(after);
        }
    }
    let digits = kind_digit_run(rest)?;
    if digits == 0 {
        return None;
    }
    rest = &rest[digits..];
    if let Some(after_dot) = rest.strip_prefix('.') {
        let frac = kind_digit_run(after_dot)?;
        if frac > 0 {
            rest = &after_dot[frac..];
        }
    }
    if let Some(after_e) = rest.strip_prefix(['e', 'E']) {
        let after_sign = after_e.strip_prefix(['+', '-']).unwrap_or(after_e);
        let exp = kind_digit_run(after_sign)?;
        if exp > 0 {
            rest = &after_sign[exp..];
        }
    }
    if let Some(after) = rest.strip_prefix("dec") {
        rest = after;
    } else if let Some(after) = rest.strip_prefix('f') {
        rest = after;
    }
    Some(rest)
}

/// An object literal kind `{ key: kind, … }` — commas optional
/// between pairs and a trailing one legal (upstream's loop `eat`s
/// the comma), the empty `{}` legal. Values are full unions.
fn read_kind_object(mut rest: &str, mode: KindMode) -> Option<&str> {
    loop {
        rest = strip_capture_trivia(rest)?;
        if let Some(tail) = rest.strip_prefix('}') {
            return Some(tail);
        }
        rest = read_kind_object_key(rest)?;
        rest = strip_capture_trivia(rest)?.strip_prefix(':')?;
        rest = read_kind_mode(strip_capture_trivia(rest)?, mode)?;
        rest = strip_capture_trivia(rest)?;
        rest = rest.strip_prefix(',').unwrap_or(rest);
    }
}

/// An object literal kind's key: a bare word (keyword-like words
/// included; the bare `NaN`/`Infinity` constant kinds excluded — same
/// upstream rule as [`read_kind_name`]), a quoted identifier, a
/// string, or an unsigned number — signed numbers and durations are
/// NOT keys (probe-pinned: upstream `parse_object_key` takes a bare
/// `Digits` compound; `{ -3: int }` and `{ 1h: int }` are rejections
/// there, and SDL must not emit what the embedded parser refuses).
fn read_kind_object_key(text: &str) -> Option<&str> {
    match text.chars().next()? {
        c if c.is_ascii_alphabetic() || c == '_' => {
            let len = word_prefix_len(text);
            let word = &text[..len];
            (!(word == "NaN" || word == "Infinity")).then(|| &text[len..])
        }
        '`' | '⟨' => read_kind_name(text),
        '\'' | '"' => lex_string_literal(text).map(|(_, rest)| rest),
        '0'..='9' => kind_number_compound_tail(text),
        _ => None,
    }
}

/// An array literal kind `[kind, …]` — the same optional-comma shape.
fn read_kind_array_lit(mut rest: &str, mode: KindMode) -> Option<&str> {
    loop {
        rest = strip_capture_trivia(rest)?;
        if let Some(tail) = rest.strip_prefix(']') {
            return Some(tail);
        }
        rest = read_kind_mode(strip_capture_trivia(rest)?, mode)?;
        rest = strip_capture_trivia(rest)?;
        rest = rest.strip_prefix(',').unwrap_or(rest);
    }
}

/// Whether a kind holds an object — with the alias-aware third
/// answer (spec/type-alias.md): `AliasReferenced` when the kind
/// references an alias, so the answer is the EXPANSION's, unknown
/// until resolution — the order pass re-asks on the expanded text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectPresence {
    Yes,
    No,
    AliasReferenced,
}

impl ObjectPresence {
    /// Yes dominates; an unresolved alias beats a definite No.
    fn or(self, other: ObjectPresence) -> ObjectPresence {
        match (self, other) {
            (ObjectPresence::Yes, _) | (_, ObjectPresence::Yes) => ObjectPresence::Yes,
            (ObjectPresence::AliasReferenced, _) | (_, ObjectPresence::AliasReferenced) => {
                ObjectPresence::AliasReferenced
            }
            (ObjectPresence::No, ObjectPresence::No) => ObjectPresence::No,
        }
    }
}

/// Whether a kind's AST contains an object kind — SDL's mirror of the
/// engine's `kind_contains_object` recursion (v3.2.4
/// `core/src/expr/statements/define/field.rs`, the `flexible` check):
/// unions recurse into their arms, `array`/`set` into their element
/// kind, `option<…>` into its concrete union, literal arrays into
/// their elements, and the object literal plus the bare `object`
/// keyword ARE objects. `record`/`table`/`file`/`geometry` interiors
/// are name lists, never kinds — they contain nothing. `text` must
/// have parsed through [`parse_kind_text`] first (the flexible check
/// is the only caller).
pub(crate) fn kind_contains_object(text: &str) -> ObjectPresence {
    let Some(start) = strip_capture_trivia(text) else {
        return ObjectPresence::No;
    };
    match kind_has_object(start, KindMode::WithAliases) {
        Some((_, presence)) => presence,
        None => ObjectPresence::No,
    }
}

/// One kind (a union head plus its `|`-joined concrete arms): the
/// tail after the production and whether any arm holds an object.
fn kind_has_object(text: &str, mode: KindMode) -> Option<(&str, ObjectPresence)> {
    let (head, tail) = read_kind_single(text, mode)?;
    let mut has = kind_head_has_object(head, mode)?;
    if head.eq_ignore_ascii_case("any") {
        // `any` stands alone — upstream's parser leaves any `|` for
        // the caller to choke on.
        return Some((tail, has));
    }
    let mut tail = tail;
    loop {
        let rest = strip_capture_trivia(tail)?;
        let Some(rest) = rest.strip_prefix('|') else {
            return Some((tail, has));
        };
        let (arm, arm_tail) = read_kind_concrete_pair(strip_capture_trivia(rest)?, mode)?;
        has = has.or(kind_head_has_object(arm, mode)?);
        tail = arm_tail;
    }
}

/// A union arm's `(head, tail)` — concrete kinds only (no `any`, no
/// `option<…>`), word or literal.
fn read_kind_concrete_pair(text: &str, mode: KindMode) -> Option<(&str, &str)> {
    match text.chars().next()? {
        c if c.is_ascii_alphabetic() || c == '_' => {
            read_kind_concrete_word(text, word_prefix_len(text), mode)
        }
        _ => read_kind_literal(text, mode),
    }
}

/// Whether one kind production (no union tail) holds an object. The
/// generic-head interiors are walked through the real readers — no
/// interior slicing, so strings and trivia inside them cannot mislead.
fn kind_head_has_object(head: &str, mode: KindMode) -> Option<ObjectPresence> {
    let len = word_prefix_len(head);
    if len == 0 || !head.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        // A literal kind: the object literal IS an object; array
        // literals recurse into their elements; strings, numbers,
        // and durations hold nothing.
        return match head.chars().next()? {
            '{' => Some(ObjectPresence::Yes),
            '[' => kind_array_lit_has_object(head, mode),
            _ => Some(ObjectPresence::No),
        };
    }
    match head[..len].to_ascii_lowercase().as_str() {
        "object" => Some(ObjectPresence::Yes),
        "option" | "array" | "set" => {
            // The `<…>` interiors hold kinds; the bare generic forms
            // (`array` = inner `any`) hold none.
            let after_word = strip_capture_trivia(&head[len..])?;
            let Some(after_lt) = after_word.strip_prefix('<') else {
                return Some(ObjectPresence::No);
            };
            let (_, has) = kind_has_object(strip_capture_trivia(after_lt)?, mode)?;
            Some(has)
        }
        // An alias may expand to an object — the expansion decides.
        _ if mode == KindMode::WithAliases && is_alias_shaped(&head[..len]) => {
            Some(ObjectPresence::AliasReferenced)
        }
        // Bare heads hold nothing; `record`/`table`/`file`/`geometry`
        // interiors are name lists, never kinds.
        _ => Some(ObjectPresence::No),
    }
}

/// An array literal's elements: each a full union, comma-optional
/// between them (the same loop [`read_kind_array_lit`] walks).
fn kind_array_lit_has_object(text: &str, mode: KindMode) -> Option<ObjectPresence> {
    let mut rest = strip_capture_trivia(&text[1..])?;
    let mut has = ObjectPresence::No;
    loop {
        if rest.starts_with(']') {
            return Some(has);
        }
        let (tail, element_has) = kind_has_object(rest, mode)?;
        has = has.or(element_has);
        rest = strip_capture_trivia(tail)?;
        rest = rest.strip_prefix(',').unwrap_or(rest);
        rest = strip_capture_trivia(rest)?;
    }
}

// ---- The canonical kind printer ----
//
// [`parse_kind_ast`] builds the structural form [`read_kind`] walks;
// [`write_kind`] prints it in the canonical SDL spelling. Literal
// leaves other than strings keep their trimmed source text (number
// and duration VALUE normalization is the kind-algebra unit's
// business — spelling text re-parses identically), while every
// structural spelling canonicalizes: heads lowercase, trivia
// collapsed, `none | T…` unions folded to `option<T…>` (either arm
// order, rule 4), and decoded names/strings re-quoted only when their
// shape demands it.

/// A parsed kind (README rule 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KindAst {
    /// `any` — stands alone, never a union arm (upstream's parser
    /// returns before the union loop).
    Any,
    /// A bare kind head, canonical lowercase (`bool`, `datetime`,
    /// `null`, `point`, `none`, …).
    Word(&'static str),
    /// The `true` / `false` literal kinds.
    Bool(bool),
    /// `NaN` — exact-case (lowercase `nan` is an identifier).
    Nan,
    /// `Infinity` — exact-case.
    Infinity,
    /// A string literal kind: the decoded value.
    Str(String),
    /// A number or duration literal kind: the trimmed source text.
    Num(String),
    /// `record<…>` / `table<…>` / `file<…>`: decoded names, empty when
    /// the head was bare.
    Record(Vec<String>),
    Table(Vec<String>),
    File(Vec<String>),
    /// `geometry<…>`: canonical lowercase geometry words, empty when
    /// the head was bare.
    Geometry(Vec<&'static str>),
    /// `array<…>` / `set<…>`: `inner: None` is the bare head (`array`
    /// with no `<`, inner kind `any`); `Some` carries the element kind
    /// (`any` included — upstream's sized interior is a FULL union)
    /// and the optional size text.
    Sized {
        array: bool,
        inner: Option<Box<KindAst>>,
        size: Option<String>,
    },
    /// `option<T>` — also what every `none | T…` union parses to
    /// (rule 4's fold, either arm order): the concrete union as one
    /// kind, `option<none>` degenerate included.
    Option(Box<KindAst>),
    /// A `|`-joined union of concrete kinds. `none` never appears (it
    /// folds into [`KindAst::Option`]), nor does `any`.
    Union(Vec<KindAst>),
    /// An object literal kind: ordered pairs.
    Object(Vec<(ObjKey, KindAst)>),
    /// An array literal kind.
    ArrayLit(Vec<KindAst>),
    /// A type-alias reference (spec/type-alias.md): the decoded name —
    /// Capitalized by the grammar, so it can never be a SurrealQL
    /// kind head. Exists only under [`KindMode::WithAliases`]; the
    /// strict entries never produce one, and expansion rewrites every
    /// reference away — the kind algebra below never compares an
    /// unexpanded one.
    Alias(String),
}

/// An object literal kind's key, kept in its authored class: a bare
/// word and a quoted identifier decode to the same key and the writer
/// spells both bare when word-shaped; a string key stays a string;
/// a numeric key keeps its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObjKey {
    Word(String),
    Str(String),
    Num(String),
}

/// The canonical SDL spelling of a kind text: [`parse_kind_ast`] then
/// [`write_kind`]. The fallback returns the input verbatim — callers
/// feed parser-validated text (`Kind.text` never holds an unparsed
/// spelling), but the writer stays total.
pub(crate) fn canonical_kind_text(text: &str) -> String {
    parse_kind_ast(text)
        .map(|ast| write_kind(&ast))
        .unwrap_or_else(|| text.to_string())
}

/// Parses one kind production into the AST — [`parse_kind_text`]'s
/// shape (strip, read, require an empty tail) building structure.
/// Alias-permissive: the compiled walks (expansion, the order pass's
/// resolution, the differ's canonicalization) run on authored text
/// where references are legal, and on expanded text where none
/// remain — the variant is inert either way.
pub(crate) fn parse_kind_ast(text: &str) -> Option<KindAst> {
    let start = strip_capture_trivia(text)?;
    let (ast, tail) = kind_ast(start, KindMode::WithAliases)?;
    strip_capture_trivia(tail)?.is_empty().then_some(ast)
}

/// Prints the canonical SDL spelling.
pub(crate) fn write_kind(ast: &KindAst) -> String {
    let mut out = String::new();
    push_kind(&mut out, ast);
    out
}

/// One kind (a union head plus its `|`-joined concrete arms):
/// [`read_kind`]'s exact shape — `any` returns before the union loop,
/// everything else collects arms and folds.
fn kind_ast(text: &str, mode: KindMode) -> Option<(KindAst, &str)> {
    let (first, mut tail) = kind_ast_single(text, mode)?;
    if matches!(first, KindAst::Any) {
        return Some((first, tail));
    }
    let mut arms = vec![first];
    loop {
        let rest = strip_capture_trivia(tail)?;
        let Some(rest) = rest.strip_prefix('|') else {
            break;
        };
        let (arm, arm_tail) = kind_ast_concrete(strip_capture_trivia(rest)?, mode)?;
        arms.push(arm);
        tail = arm_tail;
    }
    Some((fold_union(arms), tail))
}

/// `none | T…` folds to `option<T…>` — either arm order (rule 4); a
/// degenerate all-`none` union collapses to bare `none` (the same
/// kind; idempotent). A lone arm is itself.
fn fold_union(arms: Vec<KindAst>) -> KindAst {
    let has_none = arms.contains(&KindAst::Word("none"));
    if !has_none {
        return match arms.len() {
            1 => arms.into_iter().next().unwrap(),
            _ => KindAst::Union(arms),
        };
    }
    let mut concretes: Vec<_> = arms
        .into_iter()
        .filter(|a| *a != KindAst::Word("none"))
        .collect();
    match concretes.len() {
        0 => KindAst::Word("none"),
        1 => KindAst::Option(Box::new(concretes.pop().unwrap())),
        _ => KindAst::Option(Box::new(KindAst::Union(concretes))),
    }
}

/// One single kind: `any`, `option<…>`, or a concrete kind —
/// [`read_kind_single`]'s shape.
fn kind_ast_single(text: &str, mode: KindMode) -> Option<(KindAst, &str)> {
    match text.chars().next()? {
        c if c.is_ascii_alphabetic() || c == '_' => {
            let len = word_prefix_len(text);
            match text[..len].to_ascii_lowercase().as_str() {
                "any" => Some((KindAst::Any, &text[len..])),
                "option" => kind_ast_angle(text, len, mode),
                // The literal spellings are case-SENSITIVE (lowercase
                // `nan` is an identifier).
                "nan" if &text[..len] == "NaN" => Some((KindAst::Nan, &text[len..])),
                "infinity" if &text[..len] == "Infinity" => Some((KindAst::Infinity, &text[len..])),
                "true" => Some((KindAst::Bool(true), &text[len..])),
                "false" => Some((KindAst::Bool(false), &text[len..])),
                _ if KIND_BARE_HEADS.contains(&text[..len].to_ascii_lowercase().as_str()) => {
                    let word = KIND_BARE_HEADS
                        .iter()
                        .find(|head| **head == text[..len].to_ascii_lowercase())?;
                    Some((KindAst::Word(word), &text[len..]))
                }
                _ => kind_ast_concrete_word(text, len, mode),
            }
        }
        _ => kind_ast_literal(text, mode),
    }
}

/// A concrete kind — everything but `any`/`option` —
/// [`read_kind_concrete`]'s shape.
fn kind_ast_concrete(text: &str, mode: KindMode) -> Option<(KindAst, &str)> {
    match text.chars().next()? {
        c if c.is_ascii_alphabetic() || c == '_' => {
            kind_ast_concrete_word(text, word_prefix_len(text), mode)
        }
        _ => kind_ast_literal(text, mode),
    }
}

fn kind_ast_concrete_word(text: &str, len: usize, mode: KindMode) -> Option<(KindAst, &str)> {
    match text[..len].to_ascii_lowercase().as_str() {
        "any" | "option" => None,
        "nan" if &text[..len] == "NaN" => Some((KindAst::Nan, &text[len..])),
        "infinity" if &text[..len] == "Infinity" => Some((KindAst::Infinity, &text[len..])),
        _ if KIND_BARE_HEADS.contains(&text[..len].to_ascii_lowercase().as_str()) => {
            let word = KIND_BARE_HEADS
                .iter()
                .find(|head| **head == text[..len].to_ascii_lowercase())?;
            Some((KindAst::Word(word), &text[len..]))
        }
        "record" | "table" | "file" | "geometry" | "array" | "set" => {
            kind_ast_angle(text, len, mode)
        }
        // The alias fallthrough — the authored spelling rides (the
        // head word as written; `is_alias_shaped` already bounded it
        // to bare-word characters, so no decoding applies).
        _ if mode == KindMode::WithAliases && is_alias_shaped(&text[..len]) => {
            Some((KindAst::Alias(text[..len].to_string()), &text[len..]))
        }
        _ => None,
    }
}

/// A generic head, its `<…>` interior, and the closing `>` —
/// [`read_kind_angle`]'s shape. The `<` is optional for every head
/// but `option`; the interiors mirror the reader's exactly.
fn kind_ast_angle(text: &str, head_len: usize, mode: KindMode) -> Option<(KindAst, &str)> {
    let head = &text[..head_len];
    let tail = &text[head_len..];
    let rest = strip_capture_trivia(tail)?;
    let Some(rest) = rest.strip_prefix('<') else {
        let ast = match head.to_ascii_lowercase().as_str() {
            "record" => KindAst::Record(Vec::new()),
            "table" => KindAst::Table(Vec::new()),
            "file" => KindAst::File(Vec::new()),
            "geometry" => KindAst::Geometry(Vec::new()),
            _ => KindAst::Sized {
                array: head.eq_ignore_ascii_case("array"),
                inner: None,
                size: None,
            },
        };
        return (!head.eq_ignore_ascii_case("option")).then_some((ast, tail));
    };
    let (ast, interior_tail) = match head.to_ascii_lowercase().as_str() {
        "option" => {
            let (inner, interior_tail) =
                kind_ast_concrete_union(strip_capture_trivia(rest)?, mode)?;
            (KindAst::Option(Box::new(inner)), interior_tail)
        }
        // Name/geometry lists take no mode: interiors stay names.
        "record" | "table" | "file" => {
            let (names, interior_tail) = kind_ast_name_list(rest)?;
            let ast = match head.to_ascii_lowercase().as_str() {
                "record" => KindAst::Record(names),
                "table" => KindAst::Table(names),
                _ => KindAst::File(names),
            };
            (ast, interior_tail)
        }
        "geometry" => {
            let (words, interior_tail) = kind_ast_geometry_list(rest)?;
            (KindAst::Geometry(words), interior_tail)
        }
        _ => {
            let (inner, interior_tail) = kind_ast_sized(rest, mode)?;
            (
                KindAst::Sized {
                    array: head.eq_ignore_ascii_case("array"),
                    inner: Some(Box::new(inner.0)),
                    size: inner.1,
                },
                interior_tail,
            )
        }
    };
    let close = strip_capture_trivia(interior_tail)?.strip_prefix('>')?;
    Some((ast, close))
}

/// `option`'s interior: the concrete union as one kind — a lone arm
/// is itself, `|`-joined arms a [`KindAst::Union`] (no fold — the
/// wrapper IS the fold).
fn kind_ast_concrete_union(text: &str, mode: KindMode) -> Option<(KindAst, &str)> {
    let (first, mut tail) = kind_ast_concrete(text, mode)?;
    let mut arms = vec![first];
    loop {
        let rest = strip_capture_trivia(tail)?;
        let Some(rest) = rest.strip_prefix('|') else {
            break;
        };
        let (arm, arm_tail) = kind_ast_concrete(strip_capture_trivia(rest)?, mode)?;
        arms.push(arm);
        tail = arm_tail;
    }
    let ast = match arms.len() {
        1 => arms.into_iter().next().unwrap(),
        _ => KindAst::Union(arms),
    };
    Some((ast, tail))
}

/// `record<T | T…>` / `table<…>` / `file<…>`: at least one decoded
/// name, `|`-separated.
fn kind_ast_name_list(mut rest: &str) -> Option<(Vec<String>, &str)> {
    let mut names = Vec::new();
    loop {
        let (name, name_tail) = kind_ast_name(strip_capture_trivia(rest)?)?;
        names.push(name);
        rest = match strip_capture_trivia(name_tail)?.strip_prefix('|') {
            Some(tail) => tail,
            None => return Some((names, name_tail)),
        };
    }
}

fn kind_ast_name(text: &str) -> Option<(String, &str)> {
    let (span, decoded, quoted) = Scanner::new(text).read_ident().ok().flatten()?;
    // Mirrors [`read_kind_name`]'s bare NaN/Infinity exclusion.
    (quoted || !matches!(decoded.as_str(), "NaN" | "Infinity"))
        .then(|| (decoded, &text[span.end()..]))
}

/// `geometry<G | G…>`: canonical lowercase geometry words.
fn kind_ast_geometry_list(mut rest: &str) -> Option<(Vec<&'static str>, &str)> {
    let mut words = Vec::new();
    loop {
        let (word, word_tail) = kind_ast_geometry_name(strip_capture_trivia(rest)?)?;
        words.push(word);
        rest = match strip_capture_trivia(word_tail)?.strip_prefix('|') {
            Some(tail) => tail,
            None => return Some((words, word_tail)),
        };
    }
}

fn kind_ast_geometry_name(text: &str) -> Option<(&'static str, &str)> {
    let len = word_prefix_len(text);
    let word = text.get(..len)?;
    let canonical = GEOMETRY_WORDS
        .iter()
        .find(|g| **g == word.to_ascii_lowercase())?;
    Some((canonical, &text[len..]))
}

/// `array<K(, N)?>` / `set<K(, N)?>`: a full union (folded like any
/// interior — `array<none | int>` ≡ `array<option<int>>`), an
/// optional size kept as its trimmed text.
fn kind_ast_sized(text: &str, mode: KindMode) -> Option<((KindAst, Option<String>), &str)> {
    let (inner, rest) = kind_ast(strip_capture_trivia(text)?, mode)?;
    let Some(after_comma) = strip_capture_trivia(rest)?.strip_prefix(',') else {
        return Some(((inner, None), rest));
    };
    let (size, rest) = kind_ast_size(strip_capture_trivia(after_comma)?)?;
    Some(((inner, Some(size)), rest))
}

fn kind_ast_size(text: &str) -> Option<(String, &str)> {
    let digits = kind_digit_run(text)?;
    (digits > 0).then(|| (text[..digits].to_string(), &text[digits..]))
}

/// The literal kinds — [`read_kind_literal`]'s shape; strings decode,
/// numbers/durations keep their trimmed source text.
fn kind_ast_literal(text: &str, mode: KindMode) -> Option<(KindAst, &str)> {
    match text.chars().next()? {
        '\'' | '"' => {
            let (value, rest) = lex_string_literal(text)?;
            Some((KindAst::Str(value), rest))
        }
        '{' => kind_ast_object(&text[1..], mode),
        '[' => kind_ast_array_lit(&text[1..], mode),
        '+' | '-' | '0'..='9' => {
            let tail = kind_number_or_duration_tail(text)?;
            Some((
                KindAst::Num(text[..text.len() - tail.len()].to_string()),
                tail,
            ))
        }
        _ => None,
    }
}

/// An object literal kind — [`read_kind_object`]'s shape: commas
/// optional between pairs, a trailing one legal, the empty `{}` legal.
fn kind_ast_object(mut rest: &str, mode: KindMode) -> Option<(KindAst, &str)> {
    let mut pairs = Vec::new();
    loop {
        rest = strip_capture_trivia(rest)?;
        if let Some(tail) = rest.strip_prefix('}') {
            return Some((KindAst::Object(pairs), tail));
        }
        let (key, key_tail) = kind_ast_object_key(rest)?;
        rest = strip_capture_trivia(key_tail)?.strip_prefix(':')?;
        let (value, value_tail) = kind_ast(strip_capture_trivia(rest)?, mode)?;
        pairs.push((key, value));
        rest = strip_capture_trivia(value_tail)?;
        rest = rest.strip_prefix(',').unwrap_or(rest);
    }
}

/// An object literal kind's key — [`read_kind_object_key`]'s shape:
/// bare word, quoted identifier (decoded), string (decoded), or an
/// unsigned number.
fn kind_ast_object_key(text: &str) -> Option<(ObjKey, &str)> {
    match text.chars().next()? {
        c if c.is_ascii_alphabetic() || c == '_' => {
            let len = word_prefix_len(text);
            let word = &text[..len];
            // Mirrors [`read_kind_object_key`]'s bare exclusion.
            (!(word == "NaN" || word == "Infinity"))
                .then(|| (ObjKey::Word(word.to_string()), &text[len..]))
        }
        '`' | '⟨' => {
            let (span, decoded, _) = Scanner::new(text).read_ident().ok().flatten()?;
            Some((ObjKey::Word(decoded), &text[span.end()..]))
        }
        '\'' | '"' => {
            let (value, rest) = lex_string_literal(text)?;
            Some((ObjKey::Str(value), rest))
        }
        '0'..='9' => {
            let rest = kind_number_compound_tail(text)?;
            Some((
                ObjKey::Num(text[..text.len() - rest.len()].to_string()),
                rest,
            ))
        }
        _ => None,
    }
}

/// An array literal kind — [`read_kind_array_lit`]'s shape.
fn kind_ast_array_lit(mut rest: &str, mode: KindMode) -> Option<(KindAst, &str)> {
    let mut items = Vec::new();
    loop {
        rest = strip_capture_trivia(rest)?;
        if let Some(tail) = rest.strip_prefix(']') {
            return Some((KindAst::ArrayLit(items), tail));
        }
        let (item, item_tail) = kind_ast(strip_capture_trivia(rest)?, mode)?;
        items.push(item);
        rest = strip_capture_trivia(item_tail)?;
        rest = rest.strip_prefix(',').unwrap_or(rest);
    }
}

fn push_kind(out: &mut String, ast: &KindAst) {
    match ast {
        KindAst::Any => out.push_str("any"),
        KindAst::Word(word) => out.push_str(word),
        KindAst::Bool(true) => out.push_str("true"),
        KindAst::Bool(false) => out.push_str("false"),
        KindAst::Nan => out.push_str("NaN"),
        KindAst::Infinity => out.push_str("Infinity"),
        KindAst::Str(value) => crate::keywords::quoted_string(out, value),
        KindAst::Num(text) => out.push_str(text),
        KindAst::Record(names) => push_named_head(out, "record", names),
        KindAst::Table(names) => push_named_head(out, "table", names),
        KindAst::File(names) => push_named_head(out, "file", names),
        KindAst::Geometry(words) => {
            out.push_str("geometry");
            if !words.is_empty() {
                out.push('<');
                for (i, word) in words.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" | ");
                    }
                    out.push_str(word);
                }
                out.push('>');
            }
        }
        KindAst::Sized { array, inner, size } => {
            out.push_str(if *array { "array" } else { "set" });
            if let Some(inner) = inner {
                out.push('<');
                push_kind(out, inner);
                if let Some(size) = size {
                    out.push_str(", ");
                    out.push_str(size);
                }
                out.push('>');
            }
        }
        KindAst::Option(inner) => {
            out.push_str("option<");
            push_kind(out, inner);
            out.push('>');
        }
        KindAst::Union(arms) => {
            for (i, arm) in arms.iter().enumerate() {
                if i > 0 {
                    out.push_str(" | ");
                }
                push_kind(out, arm);
            }
        }
        KindAst::Object(pairs) => {
            if pairs.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{ ");
            for (i, (key, value)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                push_obj_key(out, key);
                out.push_str(": ");
                push_kind(out, value);
            }
            out.push_str(" }");
        }
        KindAst::ArrayLit(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                push_kind(out, item);
            }
            out.push(']');
        }
        // An unexpanded reference prints its authored name — the
        // canonical writer's fallback path (fmt round-trips aliases);
        // expansion has rewritten every reference the algebra and the
        // emitter ever see.
        KindAst::Alias(name) => out.push_str(name),
    }
}

fn push_named_head(out: &mut String, head: &str, names: &[String]) {
    out.push_str(head);
    if names.is_empty() {
        return;
    }
    out.push('<');
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push_str(" | ");
        }
        push_kind_name(out, name);
    }
    out.push('>');
}

/// A kind-interior name: bare when word-shaped (keyword-like words
/// included — [`read_kind_name`] takes them bare; `NaN`/`Infinity`
/// excluded — upstream's ident positions refuse those spellings, so
/// the DDL they would print is unparseable), backtick-escaped
/// otherwise.
fn push_kind_name(out: &mut String, name: &str) {
    if crate::keywords::word_shaped(name) {
        out.push_str(name);
    } else {
        out.push('`');
        escape_with(out, name, '`');
        out.push('`');
    }
}

fn push_obj_key(out: &mut String, key: &ObjKey) {
    match key {
        ObjKey::Word(word) => push_kind_name(out, word),
        ObjKey::Str(value) => crate::keywords::quoted_string(out, value),
        ObjKey::Num(text) => out.push_str(text),
    }
}

// ---- The kind algebra ----
//
// README rule 4's comparison semantics: SurrealQL desugars
// `option<T>` to the union `none | T` and normalizes unions (flatten,
// dedupe, singleton-collapse — arm order preserved; `int | string` is
// a different kind from `string | int`). Two kinds are EQUAL under
// the algebra when their [`normalize_kind`] forms are `==`; this is
// the fold comparison's licensed exception to "the database is the
// sole authority" — a pure function of the kind AST
// (spec/README.md's normalization protocol).

/// The algebra-normalized form: unions flat, order-preserving dedupe,
/// singleton-collapse, `none` never a union arm (`option<T>` ≡
/// `none | T` both fold to [`KindAst::Option`]; the all-`none` union
/// collapses to bare `none`), number/duration literals and `array`/
/// `set` sizes value-normalized (`_` separators stripped, leading
/// `+` dropped). Idempotent, and the printer prints every normalized
/// form to a spelling that re-parses to it.
pub(crate) fn normalize_kind(ast: &KindAst) -> KindAst {
    match ast {
        KindAst::Any
        | KindAst::Nan
        | KindAst::Infinity
        | KindAst::Word(_)
        | KindAst::Bool(_)
        | KindAst::Str(_)
        | KindAst::Record(_)
        | KindAst::Table(_)
        // An unexpanded reference rides verbatim (expansion has
        // rewritten every reference the algebra compares — this arm
        // is the total function's completeness, not a fold).
        | KindAst::Alias(_)
        | KindAst::File(_) => ast.clone(),
        KindAst::Num(text) => KindAst::Num(normalize_num_text(text)),
        KindAst::Geometry(words) => KindAst::Geometry(words.clone()),
        KindAst::Sized { array, inner, size } => KindAst::Sized {
            array: *array,
            inner: inner.as_ref().map(|inner| Box::new(normalize_kind(inner))),
            size: size.as_ref().map(|size| normalize_num_text(size)),
        },
        KindAst::Option(inner) => normalize_option_arm(normalize_kind(inner)),
        KindAst::Union(arms) => normalize_union_arms(arms.iter().map(normalize_kind).collect()),
        KindAst::Object(pairs) => {
            // Upstream parses object kinds into a `BTreeMap<String,
            // Kind>` (syn/parser/kind.rs — `parse_object_key` returns
            // the plain string: a numeral's RAW span, a string's
            // decoded value, a word's text; `insert` = last wins), so
            // both the key CLASS and the pair ORDER are input-only:
            // the map's key is the decoded text (a bare word, its
            // quoted spelling, and its numeral/string spellings are
            // one key), and the printer iterates map order (sorted,
            // word-shaped keys printed bare, digit-leading keys
            // quoted). The algebra folds to that map: keys collapse
            // to their decoded class, duplicates keep the LAST pair,
            // and pairs sort by decoded key — authored key spelling
            // and ordering never drift, while the writer's own
            // preservation (canonical_kind_text) is untouched.
            let mut folded: Vec<(ObjKey, KindAst)> = pairs
                .iter()
                .map(|(key, value)| (fold_obj_key(key), normalize_kind(value)))
                .collect();
            // Last occurrence wins per folded key (`BTreeMap::insert`).
            let mut deduped: Vec<(ObjKey, KindAst)> = Vec::with_capacity(folded.len());
            for (key, value) in folded.drain(..) {
                match deduped.iter().position(|(seen, _)| *seen == key) {
                    Some(at) => deduped[at] = (key, value),
                    None => deduped.push((key, value)),
                }
            }
            deduped.sort_by_key(|(key, _)| obj_key_order(key));
            KindAst::Object(deduped)
        }
        KindAst::ArrayLit(items) => KindAst::ArrayLit(items.iter().map(normalize_kind).collect()),
    }
}

/// Algebra equality: compare both sides normalized.
pub(crate) fn kinds_equal(a: &KindAst, b: &KindAst) -> bool {
    normalize_kind(a) == normalize_kind(b)
}

/// An object key's decoded class (upstream's map key is the plain
/// string `parse_object_key` returns — a word/ident's text, a string's
/// decoded value, a numeral's RAW span text with no numeric
/// normalization): a word-shaped key and its quoted/string spelling
/// are one key, and a numeric key folds to its string spelling (the
/// engine's own print quotes digit-leading keys, so the read side
/// arrives spelled as a string).
fn fold_obj_key(key: &ObjKey) -> ObjKey {
    match key {
        ObjKey::Word(word) if crate::keywords::word_shaped(word) => ObjKey::Word(word.clone()),
        // A decoded ident whose spelling cannot print bare (a
        // digit-leading backtick ident) is the same map key as its
        // numeral/string spellings — the printer escape-quotes it.
        ObjKey::Word(word) => ObjKey::Str(word.clone()),
        ObjKey::Str(value) if crate::keywords::word_shaped(value) => ObjKey::Word(value.clone()),
        ObjKey::Str(value) => ObjKey::Str(value.clone()),
        ObjKey::Num(text) => ObjKey::Str(text.clone()),
    }
}

/// The sort key of a folded object key — the decoded text, which is
/// exactly what upstream's `BTreeMap<String, Kind>` iterates (Rust's
/// byte-wise string order; the printed form re-parses to the same
/// folded keys).
fn obj_key_order(key: &ObjKey) -> String {
    match key {
        ObjKey::Word(word) => word.clone(),
        ObjKey::Str(value) => value.clone(),
        ObjKey::Num(text) => text.clone(),
    }
}

/// `option<none>` is bare `none` (`none | none` — the same kind);
/// everything else keeps the wrapper (the wrapper IS the none-fold).
fn normalize_option_arm(inner: KindAst) -> KindAst {
    if inner == KindAst::Word("none") {
        KindAst::Word("none")
    } else {
        KindAst::Option(Box::new(inner))
    }
}

/// The union arm-set fold: flatten nested unions (and `option<T>`
/// arms — none plus T's arms), drop `none` into the option flag,
/// dedupe preserving first-occurrence order, then collapse.
fn normalize_union_arms(arms: Vec<KindAst>) -> KindAst {
    let mut flat: Vec<KindAst> = Vec::new();
    let mut has_none = false;
    for arm in arms {
        match arm {
            KindAst::Union(inner) => flat.extend(inner),
            KindAst::Option(inner) => {
                has_none = true;
                match *inner {
                    KindAst::Union(inner_arms) => flat.extend(inner_arms),
                    KindAst::Word("none") => {}
                    other => flat.push(other),
                }
            }
            KindAst::Word("none") => has_none = true,
            other => flat.push(other),
        }
    }
    let mut deduped: Vec<KindAst> = Vec::new();
    for arm in flat {
        if !deduped.contains(&arm) {
            deduped.push(arm);
        }
    }
    match deduped.len() {
        0 => KindAst::Word("none"),
        1 => match has_none {
            true => KindAst::Option(Box::new(deduped.into_iter().next().unwrap())),
            false => deduped.into_iter().next().unwrap(),
        },
        _ => match has_none {
            true => KindAst::Option(Box::new(KindAst::Union(deduped))),
            false => KindAst::Union(deduped),
        },
    }
}

/// A number/duration literal's value-normalized text: `_` separators
/// stripped, a leading `+` dropped (`1_0` ≡ `10`, `+5f` ≡ `5f`).
/// Signs, fractions, exponents, `f`/`dec` suffixes and duration units
/// ride verbatim.
fn normalize_num_text(text: &str) -> String {
    let text = text.strip_prefix('+').unwrap_or(text);
    if text.contains('_') {
        text.chars().filter(|c| *c != '_').collect()
    } else {
        text.to_string()
    }
}

/// A kind's inner kind — the kind of the `name.*` subfield the engine
/// synthesizes for it (README item 4's fold): `array<T>`/`set<T>`
/// (sized variants included) yield their element kind; unions yield
/// the union of their arms' inner kinds, dropping arms that have none
/// (`int | array<string>` → `string`); `option<T>` unfolds like its
/// `none | T` union (`none` has no inner kind and drops). `None` when
/// no subfield level is written: a bare `array`/`set` (inner kind
/// exactly `any` — the recursion terminates there), a bare head, a
/// literal, or a union none of whose arms has an inner kind.
pub(crate) fn inner_kind(ast: &KindAst) -> Option<KindAst> {
    match ast {
        KindAst::Sized { inner: Some(k), .. } => match &**k {
            KindAst::Any => None,
            other => Some(normalize_kind(other)),
        },
        // An unexpanded reference writes no subfield level the
        // algebra can name (expansion has replaced every reference
        // the fold reaches).
        KindAst::Alias(_) => None,
        KindAst::Option(inner) => inner_kind(inner),
        KindAst::Union(arms) => {
            let arms: Vec<KindAst> = arms.iter().filter_map(inner_kind).collect();
            match arms.len() {
                0 => None,
                1 => arms.into_iter().next(),
                _ => Some(normalize_kind(&KindAst::Union(arms))),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every text the kind reader accepts must also build an AST, and
    /// the printed spelling must re-parse to the SAME AST (the fixed
    /// point is structural, not merely textual).
    #[test]
    fn kind_ast_fixed_points_over_the_corpus() {
        for text in [
            "any",
            "int",
            "bool",
            "null",
            "none",
            "NaN",
            "Infinity",
            "true",
            "false",
            "option<int>",
            "none | int",
            "int | none",
            "NONE | STRING",
            "none | int | string",
            "option<int | string>",
            "int | string",
            "int|string",
            "int | int",
            "none | none",
            "none | none | int",
            "record",
            "record < person >",
            "record<person | post>",
            "record<`my table`>",
            "table<edge>",
            "file<bucket>",
            "geometry",
            "geometry<point | line>",
            "GEOMETRY<POINT>",
            "array",
            "array<any>",
            "array<int,10>",
            "array<int, 1_0>",
            "set<float>",
            "array<none | int>",
            "array<option<int>>",
            "'str'",
            "'it\\'s'",
            "{}",
            "{ }",
            "{ a: int }",
            "{ a : int, 'b': string, 3: float }",
            "[]",
            "[int, string]",
            "1h30m",
            "3dec",
            "-3",
            "+5f",
            "1_0",
        ] {
            let Some(ast) = parse_kind_ast(text) else {
                panic!("`{text}` parses as a kind but built no AST");
            };
            let printed = write_kind(&ast);
            let reparsed = parse_kind_ast(&printed)
                .unwrap_or_else(|| panic!("printed `{printed}` (from `{text}`) stopped parsing"));
            assert_eq!(
                reparsed, ast,
                "`{text}` printed `{printed}` and did not re-parse to itself"
            );
        }
    }

    /// The reader and the AST builder agree on what a kind is: every
    /// accepted text builds, every rejected text fails to build.
    #[test]
    fn kind_ast_agrees_with_the_reader() {
        for text in [
            "any",
            "option<int>",
            "none | int",
            "int | none",
            "record < person >",
            "array<int, 5>",
            "array<int, 0>",
            "array<int, 1_0>",
            "record<`NaN`>",
            "record<nan>",
            "{ `Infinity`: int }",
            "{ a: int }",
            "[int]",
            "1h30m",
            "-3",
            "+5f",
        ] {
            assert!(
                parse_kind_text(text).is_some() && parse_kind_ast(text).is_some(),
                "`{text}`: reader/AST disagreement"
            );
        }
        for text in [
            "any | int",
            "option<option<int>>",
            "option",
            "int |",
            "| int",
            "int | option<string>",
            "array<int,>",
            "array<int, 1.5>",
            "array<int, -1>",
            // Bare NaN/Infinity refuse ident positions (upstream's
            // lexer classifies them as constant kinds); quoted
            // spellings of the same names stay accepted.
            "record<NaN>",
            "record<Infinity>",
            "{ NaN: int }",
            "{ Infinity: int }",
            "{ -3: int }",
            "{ 1h: int }",
            "point<point>",
            "unknown",
            "1h2",
        ] {
            assert!(
                parse_kind_text(text).is_none() && parse_kind_ast(text).is_none(),
                "`{text}`: reader/AST disagreement"
            );
        }
    }

    /// The canonical spellings — the fold either arm order, lowercase
    /// heads, collapsed trivia, canonical punctuation.
    #[test]
    fn write_kind_canonical_spellings() {
        let case = |text: &str| write_kind(&parse_kind_ast(text).unwrap());
        assert_eq!(case("NONE | STRING"), "option<string>");
        assert_eq!(case("string | none"), "option<string>");
        assert_eq!(case("none | int | string"), "option<int | string>");
        assert_eq!(case("none"), "none");
        assert_eq!(case("none | none"), "none");
        assert_eq!(case("record < person >"), "record<person>");
        assert_eq!(case("int|string"), "int | string");
        assert_eq!(case("array<int,10>"), "array<int, 10>");
        assert_eq!(case("{ a : int }"), "{ a: int }");
        assert_eq!(case("{}"), "{}");
        assert_eq!(case("GEOMETRY<POINT>"), "geometry<point>");
        assert_eq!(case("record<`my table`>"), "record<`my table`>");
        assert_eq!(case("array<none | int>"), "array<option<int>>");
        // `NaN`/`Infinity` refuse the bare shape rule in upstream's
        // ident positions (the lexer classifies them as constant
        // literals): kind-interior names and object keys re-quote
        // them, or the emitted DDL stops parsing.
        assert_eq!(case("record<`NaN`>"), "record<`NaN`>");
        assert_eq!(case("{ `Infinity`: int }"), "{ `Infinity`: int }");
        // Number/duration leaves keep their trimmed source text.
        assert_eq!(case("1_0"), "1_0");
        assert_eq!(case("+5f"), "+5f");
    }

    /// The algebra's pinned equivalences (README rule 4): `option<T>`
    /// ≡ `none | T` both ways, dedupe, flatten through option arms,
    /// singleton-collapse, num/size value normalization — and what it
    /// deliberately does NOT identify (arm order is significant; the
    /// algebra reorders nothing).
    #[test]
    fn kind_algebra_equivalences() {
        let norm = |text: &str| normalize_kind(&parse_kind_ast(text).unwrap());
        assert_eq!(norm("option<int>"), norm("none | int"));
        assert_eq!(norm("int | none"), norm("option<int>"));
        assert_eq!(norm("NONE | INT | STRING"), norm("option<int | string>"));
        assert_eq!(norm("int | int"), norm("int"));
        assert_eq!(norm("none | none"), norm("none"));
        assert_eq!(norm("none | none | int"), norm("option<int>"));
        // The surface grammar rejects `option` in arm position, so
        // option-bearing unions arise only from derived ASTs (an
        // inner-kind union over `array<option<int>>` arms) — the
        // algebra still folds them.
        let hand = KindAst::Union(vec![
            KindAst::Word("int"),
            KindAst::Option(Box::new(KindAst::Word("int"))),
        ]);
        assert_eq!(
            normalize_kind(&hand),
            KindAst::Option(Box::new(KindAst::Word("int")))
        );
        assert_eq!(norm("array<none | int>"), norm("array<option<int>>"));
        assert_eq!(norm("array<int, 1_0>"), norm("array<int, 10>"));
        assert_eq!(norm("+5f"), norm("5f"));
        assert_eq!(norm("1_0"), norm("10"));
        // Not identified: order, near misses.
        assert_ne!(norm("int | string"), norm("string | int"));
        assert_ne!(norm("int | string"), norm("option<int | string>"));
        assert_ne!(norm("record<person>"), norm("record<post>"));
        assert_ne!(norm("int"), norm("option<int>"));
        // `any` stands alone, normalized or not.
        assert_eq!(norm("any"), KindAst::Any);
    }

    /// Object kinds compare class- and order-blind — upstream's
    /// `BTreeMap` is the authority — the probe: the printer
    /// iterates map order, spells word-shaped keys bare, and quotes
    /// digit-leading keys as strings. Authored key spelling and
    /// ordering never drift; duplicates keep the last pair (map
    /// insert); and every folded form still prints to a spelling that
    /// re-parses to it.
    #[test]
    fn kind_algebra_folds_object_keys_and_order() {
        let norm = |text: &str| normalize_kind(&parse_kind_ast(text).unwrap());
        // Key class: quoted word-shaped key ≡ bare; every other key —
        // digit-leading, numeric, punctuation-bearing — folds to the
        // string class the printer escape-quotes (upstream stores the
        // decoded string: `{ 3: int }` round-trips as `{"3": int}`).
        assert_eq!(norm("{ a: int }"), norm("{ 'a': int }"));
        assert_eq!(norm("{ a: int }"), norm("{ `a`: int }"));
        assert_ne!(norm("{ a: int }"), norm("{ '3c': int }"));
        assert_ne!(norm("{ 'a b': int }"), norm("{ a: int }"));
        assert_eq!(norm("{ 3: int }"), norm("{ '3': int }"));
        assert_eq!(norm("{ 3: int }"), norm("{ `3`: int }"));
        // The map key is the numeral's RAW span (upstream performs no
        // numeric normalization on it).
        assert_ne!(norm("{ 1_0: int }"), norm("{ '10': int }"));
        assert_eq!(norm("{ 1_0: int }"), norm("{ '1_0': int }"));
        // Pair order is input-only.
        assert_eq!(norm("{ a: int, b: string }"), norm("{ b: string, a: int }"));
        // Duplicates: the last pair wins (map insert).
        assert_eq!(norm("{ a: int, 'a': string }"), norm("{ a: string }"));
        // The printed folded form re-parses to itself.
        for text in ["{ a: int }", "{ 'a b': int, b: string }", "{ '3c': int }"] {
            let ast = norm(text);
            assert_eq!(
                parse_kind_ast(&write_kind(&ast)).unwrap(),
                ast,
                "folded object kind did not round-trip: {text}"
            );
        }
    }

    /// Normalization is idempotent, and every normalized form prints
    /// to a spelling that re-parses to itself (the algebra never
    /// leaves the printer's domain).
    #[test]
    fn kind_algebra_is_idempotent() {
        for text in [
            "any",
            "int",
            "none",
            "option<int>",
            "none | int",
            "int | string | int",
            "none | int | string",
            "option<int | string>",
            "array<none | int>",
            "array<option<int>, 1_0>",
            "set<float, 2>",
            "record<person | post>",
            "geometry<point>",
            "{ a: int, b: none | string }",
            "[int, none | string]",
            "+5f",
            "1h30m",
        ] {
            let ast = parse_kind_ast(text).unwrap();
            let once = normalize_kind(&ast);
            assert_eq!(normalize_kind(&once), once, "`{text}`: not idempotent");
            let printed = write_kind(&once);
            let reparsed = parse_kind_ast(&printed)
                .unwrap_or_else(|| panic!("`{text}` normalized printed `{printed}`, unparsable"));
            assert_eq!(
                normalize_kind(&reparsed),
                once,
                "`{text}`: normalized print does not re-normalize to itself"
            );
        }
    }

    /// Inner-kind extraction (README item 4): sized variants yield
    /// their element kind, unions join their arms' inner kinds
    /// dropping the empty ones, options unfold — and the recursion
    /// terminates at exactly-`any` (bare `array`/`array<any>` write
    /// no level).
    #[test]
    fn inner_kind_extraction() {
        let inner = |text: &str| {
            inner_kind(&parse_kind_ast(text).unwrap()).map(|k| write_kind(&normalize_kind(&k)))
        };
        assert_eq!(inner("array<int>").as_deref(), Some("int"));
        assert_eq!(inner("array<int, 10>").as_deref(), Some("int"));
        assert_eq!(inner("set<string>").as_deref(), Some("string"));
        // The element kind rides verbatim: `none | int` is the
        // element (the parse folded it to `option<int>`), so the
        // synthesized subfield kind is `option<int>` too.
        assert_eq!(inner("array<none | int>").as_deref(), Some("option<int>"));
        assert_eq!(inner("array<array<int>>").as_deref(), Some("array<int>"));
        assert_eq!(
            inner("array<array<int, 3>, 2>").as_deref(),
            Some("array<int, 3>")
        );
        assert_eq!(inner("array<any>").as_deref(), None);
        assert_eq!(inner("array").as_deref(), None);
        assert_eq!(inner("set").as_deref(), None);
        assert_eq!(inner("int | array<string>").as_deref(), Some("string"));
        assert_eq!(
            inner("int | array<string> | set<float>").as_deref(),
            Some("string | float")
        );
        assert_eq!(
            inner("option<array<int>>").as_deref(),
            Some("int"),
            "option unfolds like its none-union"
        );
        assert_eq!(inner("option<int>").as_deref(), None);
        assert_eq!(inner("array<option<int>>").as_deref(), Some("option<int>"));
        assert_eq!(
            inner("array<int | string>").as_deref(),
            Some("int | string")
        );
        // No inner kind anywhere else.
        for text in [
            "record<person>",
            "table",
            "geometry<point>",
            "{ a: int }",
            "[int]",
            "int",
            "any",
            "1h",
            "'str'",
        ] {
            assert_eq!(inner(text), None, "`{text}` wrote an inner kind");
        }
    }
}
