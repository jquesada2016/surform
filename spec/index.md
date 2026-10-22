# DEFINE INDEX

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_index`),
AST/enums (`sql/index.rs`, `sql/scoring.rs`), and emitters
(`sql/statements/define/index.rs`, `catalog/schema/index.rs`).
Cross-checked against a local in-memory 3.2.3 server (2026-09-18); the floor-move corpus re-runs on the 3.3.0 embed (2026-09-25).

The kind is the discriminant and rides in the **header variant slot**:
`index [unique | count | fulltext | hnsw | diskann] <name>`. A plain
index omits the slot. Kind parameters are direct clauses — no extra
nesting.

```rust
enum IndexKind {
  Plain,
  Unique,
  Count { where? },
  FullText { analyzer = 'like',          // defaults materialize in INFO
             bm25 = (1.2, 0.75),         // omit for defaults
             highlights? },
  Hnsw { dimension,                       // required; u16
         distance? = euclidean, type? = f32,
         m? = 12 /* u8, ≤127 */, m0? = 2×m, lm? = 1/ln(m),
         efc? = 150 /* u16 */,
         extend_candidates?, keep_pruned_connections?, hashed_vector? },
  DiskAnn { dimension,                    // u16
            distance? = euclidean, type? = f32,
            degree? = 64, l_build? = 100,   // u16, > 0
            alpha? = 1.2, hashed_vector? },
}
struct Index {
  kind: IndexKind,                        // from the header slot
  name: Ident,
  on: TableRef,                           // standalone only
  fields: Vec<Expr>,                      // `columns:` alias accepted
  concurrently: bool,                     // build directive; not persisted
  comment: Option<DocComment>,            // from ///
}
```

```
/// <doc comment>                        // optional — emitted as COMMENT
index [unique | count | fulltext | hnsw | diskann] <name> {
  // required when standalone (omit inside a table block):
  on: <table>;

  // shared:
  fields: <expr>, …;                   // required; exactly ONE field for
                                       //   fulltext/hnsw/diskann;
                                       //   FORBIDDEN for count;
                                       //   `columns:` alias accepted
  concurrently;                        // flag — build in background

  // variant `count`:
  where: <expr>;                       // filtered count

  // variant `fulltext`:
  analyzer: <ident>;                   // default 'like' (materializes)
  bm25: (<number>, <number>);          // SDL-lexed pair — two numbers,
                                       //   parens mandatory; omit for the
                                       //   (1.2, 0.75) default
  highlights;                          // flag

  // variant `hnsw`:
  dimension: <number>;                 // required; u16
  distance: <distance>;                // default euclidean; minkowski
                                       //   takes (order) in SDL
  type: <vector-type>;                 // default f32
  m: <number>;                         // u8, ≤ 127 (parser-enforced);
                                       //   default 12
  m0: <number>;                        // u8; default 2×m
  lm: <number>;                        //   lm default 1/ln(m) — omitted
                                       //   params derive from m at parse;
  efc: <number>;                       // u16; default 150
  extend_candidates;                   // flags (underscore names kept
  keep_pruned_connections;             //   from SurrealQL, lowercased)
  hashed_vector;

  // variant `diskann` (3.1+, not on WASM):
  dimension: <number>;                 // required; u16
  distance: <distance>;                // restricted domain below
  type: <vector-type>;                 // restricted domain below;
                                       //   cosine_normalized ⇒ f32|f16
  degree: <number>;                    // u16, > 0; default 64
  l_build: <number>;                   // u16, > 0; default 100
  alpha: <number>;                     // default 1.2
  hashed_vector;
};
```

**Domains (ground-truthed from `parse_distance`/`parse_vector_type`):**

- `distance` — the SurrealQL distance set is 10 kinds: `euclidean`,
  `cosine`, `cosine_normalized`, `chebyshev`, `hamming`, `jaccard`,
  `pearson`, `inner_product`, `manhattan`, and `minkowski` — the last
  takes an order argument, spelled **without parentheses** in
  SurrealQL (`DISTANCE MINKOWSKI 3`); SDL spells it
  `distance: minkowski(3);` (paren form, consistent with `bm25:`) and
  codegen emits the bare form. **Only DISKANN restricts the set**
  (parser-enforced: `euclidean | cosine | inner_product |
  cosine_normalized`); HNSW accepts all ten.
- `type` — the SurrealQL vector-type set is 8 kinds: `f64`, `f32`,
  `f16`, `i64`, `i32`, `i16`, `i8`, `u8`. **Only DISKANN restricts**
  (`f32 | f16 | i8 | u8`, and `cosine_normalized` ⇒ `f32 | f16`);
  HNSW accepts all eight.

Validation (parser-enforced, verified): count forbids `fields:`;
fulltext/hnsw/diskann require exactly one; unique/plain need ≥ 1;
`m ≤ 127`; diskann degree/l_build > 0 and combo checks. Float
scalars are unsigned bounded decimals (verified): no sign, no
`NaN`/`Infinity` spellings, magnitudes under 2⁶³ — SDL prints the
value bare, so anything larger would emit an integer literal
upstream's 64-bit lexer rejects (the bm25 sides additionally store
f32, which the bound keeps finite).

Emit: `DEFINE INDEX <name> ON <table> [FIELDS <f>, …] <kind clause>
[COMMENT …] [CONCURRENTLY];` — kind params emit in SurrealQL keyword
form; codegen writes `DISTANCE` (both it and the server's printed
`DIST` alias parse — the reverse parser accepts both, canonical SDL
says `distance:`). Note the trailing-clause order: the server's own
canonical order is `… <kind> COMMENT … CONCURRENTLY`; both orders
parse; the differ compares resolved state (kinds under the algebra, expressions over canonical text); the transient `concurrently;` directive never
drifts — INFO cannot report it.

## Reverse parser (INFO → SDL)

Two always-materialized shapes (verified): bare `FULLTEXT` returns as
`FULLTEXT ANALYZER like BM25(1.2,0.75)`, and a full HNSW param set is
printed explicitly, **including derived `M0`/`LM`** (computed from m
at define). Canonical minimal form elides: analyzer/bm25 at defaults;
`m0`/`lm` when they equal the values derived from the (possibly
defaulted) `m` — compare by recomputing from `m` (relative epsilon
1e-9 for `lm`'s float; never string-compare); `m`/`efc`/`degree`/
`l_build`/`alpha`/`type`/`distance` at defaults. `CONCURRENTLY` never
appears in INFO (transient — preserve from SDL). Unrepresentable-but-
legal DDL (e.g. legacy `VS` scoring, or a DISKANN-restricted kind
appearing where SDL's grammar can't express it) routes the statement
to `raw`. `bm25:` values are stored as **f32** — INFO reprints the
rounded pair (adopt per protocol). Indexes are listed by `INFO FOR
TABLE <t>`; canonical placement nests them into their table block.

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): multi-field + `COLUMNS` alias
✅; `UNIQUE CONCURRENTLY` ✅; `COUNT` bare and `COUNT WHERE` ✅;
count+fields → `Cannot create a count index with fields` ✅; bare
`FULLTEXT` materializes `ANALYZER like BM25(1.2,0.75)` ✅; full HNSW
param set incl. `EXTEND_CANDIDATES`/`KEEP_PRUNED_CONNECTIONS`/
`HASHED_VECTOR` ✅; `M 200` → `Invalid value for HNSW parameter M` ✅;
DISKANN `COSINE_NORMALIZED`+`I8` → combo error ✅; full DISKANN
params ✅; two-field HNSW → `Expected one column, found 2` ✅;
`CONCURRENTLY` absent from INFO ✅.

## Examples (one per variant)

plain multi-field + doc comment:

```sdl
/// Composite lookup for inbox queries
index by_account_thread {
  on: message;
  fields: account, thread_id;
};
```

```surql
DEFINE INDEX by_account_thread ON message FIELDS account, thread_id
  COMMENT 'Composite lookup for inbox queries';
```

unique + concurrently:

```sdl
index unique one_email {
  on: user;
  fields: email;
  concurrently;
};
```

```surql
DEFINE INDEX one_email ON user FIELDS email UNIQUE CONCURRENTLY;
```

count — bare and filtered:

```sdl
index count row_count {
  on: reading;
};
index count active_count {
  on: item;
  where: status = 'active';
};
```

```surql
DEFINE INDEX row_count ON reading COUNT;
DEFINE INDEX active_count ON item COUNT WHERE status = 'active';
```

fulltext — minimal (defaults materialize) and full:

```sdl
index fulltext body_fts {
  on: post;
  fields: body;
};
index fulltext title_fts {
  on: post;
  fields: title;
  analyzer: english;
  bm25: (1.2, 0.75);
  highlights;
};
```

```surql
DEFINE INDEX body_fts ON post FIELDS body FULLTEXT;
DEFINE INDEX title_fts ON post FIELDS title
  FULLTEXT ANALYZER english BM25(1.2, 0.75) HIGHLIGHTS;
```

hnsw — minimal and tuned (the tuned set exercises minkowski and the
underscore flags; note the emitted `MINKOWSKI 3` — no parentheses):

```sdl
index hnsw vec {
  on: embedding;
  fields: vector;
  dimension: 768;
  distance: cosine;
};
index hnsw vec_tuned {
  on: embedding;
  fields: vector;
  dimension: 768;
  distance: minkowski(3);
  type: f16;
  m: 16;
  efc: 200;
  extend_candidates;
  keep_pruned_connections;
};
```

```surql
DEFINE INDEX vec ON embedding FIELDS vector HNSW DIMENSION 768 DISTANCE COSINE;
DEFINE INDEX vec_tuned ON embedding FIELDS vector
  HNSW DIMENSION 768 DISTANCE MINKOWSKI 3 TYPE F16 M 16 EFC 200
  EXTEND_CANDIDATES KEEP_PRUNED_CONNECTIONS;
```

diskann — on-disk vectors:

```sdl
index diskann big_vec {
  on: document;
  fields: embedding;
  dimension: 1536;
  distance: cosine_normalized;
  type: f16;
  degree: 32;
  l_build: 50;
  alpha: 1.3;
  hashed_vector;
};
```

```surql
DEFINE INDEX big_vec ON document FIELDS embedding
  DISKANN DIMENSION 1536 DISTANCE COSINE_NORMALIZED TYPE F16
  DEGREE 32 L_BUILD 50 ALPHA 1.3 HASHED_VECTOR;
```

## Nesting

Indexes are table-scoped; inside a `table` block they omit `on:` —
same rule as fields and events (see [table.md](table.md)).
