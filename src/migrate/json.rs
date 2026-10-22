//! The `/sql` response's read-only JSON walk. The HTTP
//! response of `POST /sql` is a JSON array of per-statement results
//! (docs: surrealdb.com/docs/reference/rest-api/http-protocol.md
//! `POST /sql`, and rest-api/errors.md for the ERR element shape) and
//! the push pipeline reads exactly three things out of it: each
//! element's `status`, an ERR element's `kind`/`details` chain (the
//! errors page's sanctioned branch keys) and its `result` message
//! string, plus the ledger SELECT's projected rows (`number` as a
//! JSON number, `name`/`hash` as strings — engine-probed 3.3.0). A
//! full RFC 8259 reader, but a READER only: the whole document
//! materializes into [`Json`], and nothing on the send side produces
//! JSON — request bodies are raw SurrealQL text.
//!
//! In-repo like the SHA-256 module ([`super::sha256`]): the decided
//! scope is the HTTP client alone, so the JSON walk is hand-rolled
//! and vector-tested rather than adding a JSON crate. It is a total
//! function from input text to `Result` — malformed input is
//! [`JsonError`] (byte offset included), never a panic, and a nesting
//! cap turns pathologically deep documents into an error (the proptest
//! layer-3 contract: no panics).

/// The byte cap on nesting depth — far above anything the `/sql`
/// contract produces (depth 3: array → element object → result
/// array → row), far below stack-exhaustion depth.
const MAX_DEPTH: usize = 128;

/// A parsed JSON value. Numbers keep their raw token — [`Json::as_integer`]
/// interprets the ones callers care about.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// The number's raw spelling (the caller interprets: integers via
    /// [`Json::as_integer`]).
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    /// Insertion-ordered members; [`Json::get`] scans first match.
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// The first member named `key` (objects only).
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The value as an integer — the raw number token parsed as `i64`
    /// (fraction/exponent spellings parse only when they name an
    /// integral value, `1e3` included).
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Json::Num(raw) => raw.parse::<i64>().ok().or_else(|| {
                raw.parse::<f64>().ok().and_then(|f| {
                    if f.fract() == 0.0 && f.abs() < 9.223_372_036_854_776E18 {
                        Some(f as i64)
                    } else {
                        None
                    }
                })
            }),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }
}

/// Why the text is not JSON — the byte offset anchors it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    pub offset: usize,
    pub message: String,
}

impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "malformed JSON at byte {}: {}",
            self.offset, self.message
        )
    }
}

/// Parses a complete JSON document — trailing non-whitespace is an
/// error, and so is an empty input.
pub fn parse(text: &str) -> Result<Json, JsonError> {
    let mut p = Parser {
        bytes: text.as_bytes(),
        at: 0,
        depth: 0,
    };
    p.skip_ws();
    let value = p.value()?;
    p.skip_ws();
    if p.at < p.bytes.len() {
        return Err(p.error("trailing characters after the JSON document"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, message: &str) -> JsonError {
        JsonError {
            offset: self.at,
            message: message.to_string(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn expect(&mut self, word: &str) -> Result<(), JsonError> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(())
        } else {
            Err(self.error(&format!("expected `{word}`")))
        }
    }

    fn value(&mut self) -> Result<Json, JsonError> {
        match self.peek() {
            None => Err(self.error("unexpected end of input")),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.expect("true").map(|_| Json::Bool(true)),
            Some(b'f') => self.expect("false").map(|_| Json::Bool(false)),
            Some(b'n') => self.expect("null").map(|_| Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
        }
    }

    fn nested<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, JsonError>,
    ) -> Result<T, JsonError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(self.error("nesting too deep"));
        }
        let out = f(self);
        self.depth -= 1;
        out
    }

    fn object(&mut self) -> Result<Json, JsonError> {
        self.nested(|p| {
            p.at += 1; // `{`
            let mut members = Vec::new();
            p.skip_ws();
            if p.peek() == Some(b'}') {
                p.at += 1;
                return Ok(Json::Obj(members));
            }
            loop {
                p.skip_ws();
                if p.peek() != Some(b'"') {
                    return Err(p.error("expected a string key"));
                }
                let key = p.string()?;
                p.skip_ws();
                if p.peek() != Some(b':') {
                    return Err(p.error("expected `:` after the key"));
                }
                p.at += 1;
                p.skip_ws();
                let value = p.value()?;
                members.push((key, value));
                p.skip_ws();
                match p.peek() {
                    Some(b',') => p.at += 1,
                    Some(b'}') => {
                        p.at += 1;
                        return Ok(Json::Obj(members));
                    }
                    _ => return Err(p.error("expected `,` or `}`")),
                }
            }
        })
    }

    fn array(&mut self) -> Result<Json, JsonError> {
        self.nested(|p| {
            p.at += 1; // `[`
            let mut items = Vec::new();
            p.skip_ws();
            if p.peek() == Some(b']') {
                p.at += 1;
                return Ok(Json::Arr(items));
            }
            loop {
                p.skip_ws();
                items.push(p.value()?);
                p.skip_ws();
                match p.peek() {
                    Some(b',') => p.at += 1,
                    Some(b']') => {
                        p.at += 1;
                        return Ok(Json::Arr(items));
                    }
                    _ => return Err(p.error("expected `,` or `]`")),
                }
            }
        })
    }

    /// A string literal with its escapes decoded (`\" \\ \/ \b \f \n
    /// \r \t \uXXXX`, lone surrogates an error, surrogate pairs
    /// combined). Raw control characters are rejected — RFC 8259.
    fn string(&mut self) -> Result<String, JsonError> {
        self.at += 1; // opening `"`
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let escape = self
                        .peek()
                        .ok_or_else(|| self.error("unterminated escape"))?;
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let first = self.hex4()?;
                            let ch = match (0xD800..0xDC00).contains(&first) {
                                // A high surrogate must pair: `\uDC00\uD800`
                                // … with a low surrogate next.
                                true => {
                                    if self.peek() != Some(b'\\') {
                                        return Err(self.error("lone high surrogate"));
                                    }
                                    self.at += 1;
                                    if self.peek() != Some(b'u') {
                                        return Err(self.error("lone high surrogate"));
                                    }
                                    self.at += 1;
                                    let second = self.hex4()?;
                                    if !(0xDC00..0xE000).contains(&second) {
                                        return Err(self.error("invalid low surrogate"));
                                    }
                                    let combined =
                                        0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                                    char::from_u32(combined)
                                        .ok_or_else(|| self.error("invalid surrogate pair"))?
                                }
                                false if (0xDC00..0xE000).contains(&first) => {
                                    return Err(self.error("lone low surrogate"));
                                }
                                false => char::from_u32(first)
                                    .ok_or_else(|| self.error("invalid `\\u` escape"))?,
                            };
                            out.push(ch);
                        }
                        _ => return Err(self.error("invalid escape character")),
                    }
                }
                Some(byte) if byte < 0x20 => {
                    return Err(self.error("raw control character in string"));
                }
                // Multi-byte UTF-8 rides its original bytes into the
                // String — the input is `&str`, so the sequence is
                // valid by construction.
                Some(_) => {
                    let rest = &self.bytes[self.at..];
                    let len = utf8_len(rest[0]);
                    let piece = std::str::from_utf8(&rest[..len.min(rest.len())])
                        .map_err(|_| self.error("invalid UTF-8 in string"))?;
                    out.push_str(piece);
                    self.at += piece.len();
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let slice = self
            .bytes
            .get(self.at..self.at + 4)
            .ok_or_else(|| self.error("truncated `\\u` escape"))?;
        let text = std::str::from_utf8(slice).map_err(|_| self.error("invalid `\\u` escape"))?;
        // RFC 8259: exactly four HEXDIGITS — `u32::from_str_radix`
        // tolerates a sign, so the digits are checked first (`\u+041`
        // and `\u-041` are malformed, not U+0041).
        if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.error("invalid `\\u` escape"));
        }
        let value =
            u32::from_str_radix(text, 16).map_err(|_| self.error("invalid `\\u` escape"))?;
        self.at += 4;
        Ok(value)
    }

    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        // int part: `0` or [1-9][0-9]*
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.at += 1;
                }
            }
            _ => return Err(self.error("invalid number")),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("invalid number fraction"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("invalid number exponent"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        Ok(Json::Num(
            String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned(),
        ))
    }
}

/// The UTF-8 sequence length for a leading byte (1 for ASCII; the
/// 2–4 continuation counts for the multi-byte heads). The parser only
/// calls it on bytes inside a valid `&str`.
fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_arrays_and_objects_parse() {
        assert_eq!(parse("null").unwrap(), Json::Null);
        assert_eq!(parse(" true ").unwrap(), Json::Bool(true));
        assert_eq!(parse("-12").unwrap(), Json::Num("-12".into()));
        assert_eq!(parse("\"hi\"").unwrap(), Json::Str("hi".into()));
        assert_eq!(parse("[]").unwrap(), Json::Arr(vec![]));
        assert_eq!(parse("{}").unwrap(), Json::Obj(vec![]));
        let doc = parse(r#"{"a": [1, "x", null, {"b": false}]}"#).unwrap();
        let inner = doc.get("a").and_then(Json::as_array).unwrap();
        assert_eq!(inner.len(), 4);
        assert_eq!(inner[3].get("b"), Some(&Json::Bool(false)));
    }

    #[test]
    fn escapes_decode_including_surrogate_pairs() {
        assert_eq!(
            parse(r#""a\"b\\c\/d\n\t\r\f\b""#).unwrap(),
            Json::Str("a\"b\\c/d\n\t\r\u{000C}\u{0008}".into())
        );
        // U+1F600 as a surrogate pair.
        assert_eq!(parse(r#""😀""#).unwrap(), Json::Str("😀".into()));
        assert_eq!(parse("\"\\ud83d\\ude00\"").unwrap(), Json::Str("😀".into()));
        // A multi-byte character rides raw bytes.
        assert_eq!(parse("\"«ok»\"").unwrap(), Json::Str("«ok»".into()));
        for broken in [
            r#""\ud83d""#,
            r#""\ude00""#,
            r#""\ud83dA""#,
            r#""\uZZZZ""#,
            // Exactly four HEXDIGITS — a sign is not one (RFC 8259;
            // from_str_radix would otherwise fold `\u+041` to U+0041).
            r#""\u+041""#,
            r#""\u-041""#,
            "\"a\nb\"",
            "\"ab",
        ] {
            assert!(parse(broken).is_err(), "{broken} should not parse");
        }
    }

    #[test]
    fn integers_read_through_fraction_and_exponent_forms() {
        assert_eq!(parse("1").unwrap().as_integer(), Some(1));
        assert_eq!(parse("-4").unwrap().as_integer(), Some(-4));
        assert_eq!(parse("1e3").unwrap().as_integer(), Some(1000));
        assert_eq!(parse("2.0").unwrap().as_integer(), Some(2));
        assert_eq!(parse("2.5").unwrap().as_integer(), None);
        assert_eq!(parse("\"1\"").unwrap().as_integer(), None);
    }

    #[test]
    fn malformed_input_is_an_error_with_an_offset() {
        for broken in [
            "",
            "   ",
            "nul",
            "tru, ",
            "[1,]",
            "{",
            "{\"a\"}",
            "{\"a\":}",
            "[1] x",
            "{\"a\":1,}",
            "-",
            "01",
            "1.",
            "[\"\\q\"]",
        ] {
            let error = parse(broken)
                .err()
                .unwrap_or_else(|| panic!("{broken} should not parse"));
            assert!(
                error.offset <= broken.len(),
                "{broken}: offset out of range"
            );
        }
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_panic() {
        let deep = format!("{}{}", "[".repeat(500), "]".repeat(500));
        assert!(parse(&deep).is_err());
        // Just under the cap parses.
        let ok = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(parse(&ok).is_ok());
    }
}
