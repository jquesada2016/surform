Analyzers define how fulltext search reads text: how to split
it into terms, and how to normalize each term. They are named
definitions a [fulltext index](indexes.html#fulltext-search-over-text)
points at.

## The shape

```sdl
namespace acme;
database shop;

/// English text search.
analyzer english {
  tokenizers: class;
  filters: snowball(english), lowercase;
};
```

Three optional clauses — an analyzer with none is legal but inert:

- `tokenizers:` — how text splits. `blank` (whitespace), `camel`
  (camelCase boundaries), `class` (alphanumeric runs), `punct`
  (punctuation).
- `filters:` — term transformations, **order matters**: `ascii`,
  `lowercase`, `uppercase`, `snowball(<language>)` (stemming — 18
  languages, bare keywords, short aliases like `en` accepted),
  `edgengram(min, max)` (prefix matching for autocomplete),
  `ngram(min, max)` (fuzzy matching).
- `function:` — a preprocessing function of your own (it must accept
  and return a string), running before tokenization.

## Building blocks for search features

Autocomplete and fuzzy search are analyzer combinations:

```sdl
namespace acme;
database shop;

analyzer autocomplete {
  filters: lowercase, edgengram(2, 10);
};
analyzer fuzzy {
  filters: ascii, ngram(3, 5);
};
```

Point a fulltext index at one and queries like
`… WHERE title @@ 'sur'` start matching prefixes. The linter checks
your ngram windows (`SDL-L0004` — a min larger than the max can only
degrade silently).

## Preprocessing with your own function

```sdl
namespace acme;
database shop;

fn strip_markup($s: string) -> string {
  string::replace($s, '<em>', '')
};

analyzer preprocessed {
  function: fn::strip_markup;
  tokenizers: blank;
  filters: lowercase;
};
```

The `function:` clause is a **name**, not a call — no trailing
parentheses.

## Lists, repeats, and wrapping

Lists are comma-separated and may wrap across lines; repeating a
clause replaces the whole list (last one wins); a duplicate *entry*
inside one list is accepted by the database but flagged by the
linter:

```sdl
namespace acme;
database shop;

analyzer everything {
  tokenizers: blank,
              camel,
              punct;
  filters: ascii, lowercase, snowball(english);
};
```

## Where next

- [Indexes](indexes.html#fulltext-search-over-text) — attaching an
  analyzer to a field.
- [Functions](functions.html) — writing the preprocessing function.
