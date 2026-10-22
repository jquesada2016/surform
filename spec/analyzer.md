# DEFINE ANALYZER

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_analyzer`,
`parse_analyzer_function_name`) and emitter
(`sql/statements/define/analyzer.rs`). Cross-checked against a local
in-memory 3.2.3 server (2026-09-18); the floor move re-runs the corpus on the 3.3.0 embed (2026-09-25). Clause order free; no discriminant;
the SDL AST is a plain struct. Emit order (canonical):
`FUNCTION → TOKENIZERS → FILTERS → COMMENT`.

```rust
struct Analyzer {
  name: Ident,
  function: Option<FnPath>,            // SDL-lexed name path — see the
                                        //   global <fn-path> notation rule
  tokenizers: Option<Vec<Tokenizer>>,
  filters: Option<Vec<Filter>>,
  comment: Option<DocComment>,         // from ///
}
```

```
/// <doc comment>                        // optional — emitted as COMMENT
analyzer <name> {
  // optional (all clauses — an analyzer with none is accepted, though inert):
  function: fn::<seg>[::<seg>…];        // preprocessing function — an SDL-
                                         //   lexed NAME PATH (global <fn-path>
                                         //   rule; NOT a call — trailing
                                         //   parens rejected; ml::/silo::
                                         //   rejected by SurrealQL;
                                         //   mod::<module>[::<seg>…] is
                                         //   accepted by SDL and enforced
                                         //   by the server's surrealism
                                         //   capability at define time)
  tokenizers: <tokenizer>, …;           // blank | camel | class | punct
  filters: <filter>, …;                 // ascii | lowercase | uppercase
                                         // | edgengram(<min>, <max>)
                                         // | ngram(<min>, <max>)
                                         // | snowball(<language>)
                                         // | mapper(<string>)
};
```

Clause details (all SDL-lexed keyword clauses — none are SurrealQL
expressions):

- Lists are comma-separated, non-empty, no trailing comma, and may wrap
  across lines (the capture-to-`;` scan is bracket-depth-aware, so the
  parameterized forms' parens are handled). Repeating a list clause
  replaces the whole list (last-one-wins, SurrealQL parity). Duplicate
  entries within one list are accepted (parity) and flagged by the
  linter (SDL-L0005).
- `edgengram`/`ngram` take two **integers** (`u16`) inside mandatory
  parentheses: `edgengram(2, 10)`. SurrealQL does not validate
  `min ≤ max` (it silently degrades); SDL validates it as a lint
  (SDL-L0004).
- `snowball(<language>)` — the language is a **bare keyword**, not a
  string (`snowball('english')` is rejected by SurrealQL). Canonical
  full names (18): arabic, danish, dutch, english, finnish, french,
  german, greek, hungarian, italian, norwegian, portuguese, romanian,
  russian, spanish, swedish, tamil, turkish. SDL also accepts the
  lexer's short aliases — 2-letter: ar, da, nl, de, el, en, es, fi, fr,
  hu, it, no, pt, ro, ru, sv, ta, tr; 3-letter: ara, dan, deu, ell,
  eng, fin, fra, hun, ita, nld, nor, por, ron, rus, spa, swe, tam, tur
  (note the ISO 639-2/T spellings: deu not ger, ell not gre, nld not
  dut, ron not rum; `no` is a parser special-case, not a lexer alias).
  Canonical output normalizes to the full name.
- `mapper('<path>')` — a string literal (single or double quotes,
  SurrealQL string escape rules); the path must appear in the runtime
  file allowlist for actual use.

Emit: `DEFINE ANALYZER <name> [FUNCTION …] [TOKENIZERS …] [FILTERS …]
[COMMENT …];` (`OVERWRITE` implied per the codegen model; keywords
uppercased, language emitted as its canonical full name).

## Reverse parser (INFO → SDL)

INFO emits the plain `DEFINE` (no `OVERWRITE`) in the canonical order
above. Nothing materializes for analyzers (comment defaults to absent;
no clause defaults) — the reverse mapping is 1:1, and **list order is
preserved and significant** (filter order affects runtime behavior).
Languages always arrive as canonical full names; alias-authored SDL
normalizes to full names (`snowball(en)` → `snowball(english)`).
fn-path segments arrive escaped only when they are empty, start with a
digit, contain non-`[A-Za-z0-9_]` characters, or equal `NaN`/
`Infinity` — keywords arrive bare (identifier-like token classes) —
strip backticks when present. The analyzer **name** arrives escaped
against SurrealQL's reserved-keyword set (plus digit-start/non-word
conditions) — same rule, strip backticks when present. Tokenizers are printed comma-without-space
and filters with `, ` — irrelevant to parsing, noted for text-diff
avoidal (the differ compares resolved state). External DDL with `min > max` (legal
SurrealQL) is accepted and linted, not rejected.

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): empty analyzer ✅; bare snowball
language ✅; `FUNCTION fn::strip` with a defined function ✅;
multi-value tokenizer/filter lists with `edgengram(2, 10)` ✅.

## Examples (one per clause)

tokenizers + filters + doc comment:

```sdl
/// English text search
analyzer english {
  tokenizers: class;
  filters: snowball(english), lowercase;
};
```

```surql
DEFINE ANALYZER english TOKENIZERS CLASS FILTERS SNOWBALL(ENGLISH), LOWERCASE
  COMMENT 'English text search';
```

tokenizers only:

```sdl
analyzer raw_split {
  tokenizers: camel, punct;
};
```

filters only — autocomplete (edgengram) and fuzzy (ngram):

```sdl
analyzer autocomplete {
  filters: lowercase, edgengram(2, 10);
};
analyzer fuzzy {
  filters: ascii, ngram(3, 5);
};
```

mapper (custom lemmatization dictionary — allowlisted at runtime):

```sdl no-exec
analyzer domain {
  tokenizers: class;
  filters: mapper('/etc/surreal/domain-map.txt');
};
```

(The mapper path must appear in the runtime file allowlist, so define
time rejects it until the server knows the file — this fence is
verified through the parse oracle, not the engine.)

function (v3 preprocessing — runs before tokenization; the function must
accept and return a string):

```sdl
analyzer preprocessed {
  function: fn::strip_markup;
  tokenizers: blank;
  filters: lowercase;
};
```

wrapped multi-line list:

```sdl
analyzer everything {
  tokenizers: blank,
              camel,
              punct;
  filters: ascii, lowercase, snowball(english);
};
```
