# DEFINE TABLE

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_table`,
`parse_relation_schema`, `parse_tables`, `parse_view` in
`stmt/parts.rs`), executor (`expr/statements/define/table.rs`), and
emitters (`sql/statements/define/table.rs`, `sql/permission.rs`).
Cross-checked against a local in-memory 3.2.3 server (2026-09-18); the floor-move corpus re-runs on the 3.3.0 embed (2026-09-25).

**Semantics you must know (all verified):**

- **Default table permissions are `NONE`** and materialize in INFO
  (`DEFINE TABLE t SCHEMALESS;` round-trips with `PERMISSIONS NONE`).
  Record users can do nothing on a table unless permissions are
  granted — omission is the secure default and never warns; SDL's
  linter warns on `permissions: full` (SDL-L0001 — anyone, even
  unauthenticated users, can access every record; advisory: it
  evaluates the resolved state, so a table the database itself holds
  wide open warns through INFO).
- **Variant resolution is total and explicit at emit.** A bare table
  (no qualifier, no arrow) is `normal` and **emits `TYPE NORMAL`** — a
  deliberate fail-closed default that rejects `RELATE` (verified),
  unlike SurrealQL's bare-schemaless→`TYPE ANY` materialization quirk
  (bare `SCHEMAFULL` sets NORMAL in the parser — DDL-level trivia SDL
  no longer depends on). `any` is the explicit escape hatch for tables
  that must accept `RELATE` without being declared relations (verified:
  RELATE into ANY succeeds, into NORMAL fails). The normalizer's
  round-trip is bijective: `TYPE ANY ↔ any`, `TYPE NORMAL ↔ bare`,
  `TYPE RELATION ↔ arrow (full, IN-only, or OUT-only) | relation`,
  view ↔ `view` — every `TYPE RELATION` shape INFO can report is
  expressible (the unconstrained one in two spellings:
  `table relation <name>;` canonical, `table -> <name> ->;`
  equivalent).
- **`SCHEMALESS` materializes in INFO** (the emitter prints
  SCHEMAFULL/SCHEMALESS unconditionally). SDL adopts this: codegen
  always emits the schema flag explicitly — `schemaless;` is not
  elided from canonical SDL (a licensed deviation from omission-parity,
  keeping emitted DDL byte-stable against INFO).
- **`FOR delete FULL` is elided by the emitter while re-parsing yields
  NONE** — a definition with `delete: full;` must never be downgraded
  by retro-apply (preserve rule). The differ's posture is
  converged-silently in every reachable shape: the projection both
  sides compare through drops the delete slot unless it carries a real
  expression (the only delete value INFO can report), so SDL-said-full
  converges against whatever the database holds (one-directional by
  construction; the linter owns the ambiguity — SDL-L0014).
- Per-op **unlisted operations are NONE** for tables (the parser seeds
  deny — the opposite of fields); SurrealQL also accepts the
  single-value `PERMISSIONS NONE | FULL` on tables (verified) — SDL
  allows it as the all-operations shorthand.
- **`LIGHTWEIGHT` relations (3.3)** are the record-less edge form:
  `lightweight` trails the relation header (`table person -> likes ->
  post lightweight { … };`), requires both sides named (the engine:
  "a LIGHTWEIGHT relation requires explicit IN and OUT tables"), and
  **implies `enforced`** — the engine normalizes the flag at define
  (vertex deletion is a record-less edge's only cleanup, so dangling
  endpoints would be permanent garbage) and INFO always prints
  `ENFORCED LIGHTWEIGHT`. SDL folds the implication at parse: the AST
  never holds `lightweight` without `enforced`, the emission always
  carries both words, and the differ needs no default-equivalence arm.
  The record-consuming clauses are rejected (the engine's define-time
  rejections mirrored at the fold, rule 10's ADT pattern): `drop;`,
  `changefeed { … };`, `schemafull;` (either spelling), and nested
  field/index/event definitions — the engine auto-installs `in`/`out`
  from the header and rejects every `DEFINE FIELD/INDEX/EVENT` on the
  table ("a LIGHTWEIGHT relation cannot carry …: its edges store no
  records"). `PERMISSIONS`, `graphql`, the schema-less default, and
  the INLINE caps all remain legal on it (probe-pinned). Write-path
  facts (engine-pinned, outside SDL's grammar): edge ids are canonical
  `edge:[in, out]` two-record arrays, `RELATE` is idempotent per pair,
  and no data mutations are possible beyond the endpoints. A table can
  leave the lightweight form while it holds no edges
  (`table <from> -> <name> -> <to>;` without the word).
- **The INLINE cache caps (3.3)** — `inline edges: <n>;` and
  `inline references: <n>;` — cap the per-table inline caches; the
  number is mandatory and the domain is 0–256 (the engine's
  `MAX_INLINE_CACHE_CAP`, enforced at define/alter; SDL mirrors the
  bound at the fold — SDL-E1057). The clauses are **kind-independent**:
  the engine accepts them on NORMAL/ANY/relation/lightweight tables
  and even views (where SDL does not author them — semantically inert
  there, and SDL's view grammar stays the two-clause table; the
  reader routes a cap-bearing view to `raw` as deviant). They print
  only when written, EDGES before REFERENCES, as bare integers.
- The schema flags fold **last-one-wins, never error**: upstream's
  clause loop silently takes the last spelling (no conflict
  tracking), and the header and block spellings are one fold —
  `table t schemafull { schemaless; };` is `SCHEMALESS` (rule 7,
  SurrealQL parity; probe-verified against v3.2.4
  `parse_define_table`).

The kind is the discriminant and rides in the **header**:

```rust
enum TableKind {
  Any,                                  // explicit escape hatch
  Normal,                               // bare tables — emits TYPE NORMAL
  Relation { from?: Vec<Table>,         // header arrows in RELATE's own
             to?: Vec<Table>,           //   order: from -> name -> to
             enforced?,                 //   (SurrealQL: IN a | b / OUT c —
             lightweight? },            //   independent, each optional,
                                        //   both seeded empty). The arrow
                                        //   spelling always flanks the
                                        //   name with `->`; either side
                                        //   list may be omitted, and both
                                        //   omitted (`table -> name ->;`)
                                        //   is equivalent to `table
                                        //   relation <name>;`. A side is
                                        //   None exactly when unnamed —
                                        //   Some never wraps an empty vec
                                        //   (the forward and reverse
                                        //   parsers must agree; the
                                        //   differ compares resolved
                                        //   state). `lightweight` is
                                        //   3.3-only, header-only, and
                                        //   implies `enforced` (the fold).
  View { query: Expr },                 // AS SELECT … — read-only
}
struct Table {
  name, kind: TableKind,
  full: bool,                           // schemafull; / schemaless; flags
  drop: bool,                           // skip storing/indexing/edge-linking
  changefeed: Option<ChangeFeed>,       // { expiry, include_original }
  permissions: Permission,              // per-op (all four) + none|full
  graphql: Option<Graphql>,             // alias?, deprecated?
  inline_edges: Option<u16>,            // 3.3: `inline edges: <n>;` (0–256)
  inline_references: Option<u16>,       // 3.3: `inline references: <n>;`
  nested: Vec<Definition>,              // fields, indexes, events
  comment: Option<DocComment>,          // from ///
}
```

```
/// <doc comment>                        // optional — emitted as COMMENT
table <name> { … };                      // NORMAL — bare means normal and
                                         //   emits TYPE NORMAL (fail-closed)
table any <name> { … };                  // ANY — explicit escape hatch

table [<from> [| <from> …]] -> <name> -> [<to> [| <to> …]] [lightweight] { … };
                                         // the relation variant, spelled in
                                         //   RELATE's own from -> edge -> to
                                         //   order; the name is always
                                         //   flanked by `->` on both sides
                                         //   (arrow detection skips
                                         //   whitespace, so `a->b->c` and
                                         //   `a -> b -> c` are the same
                                         //   header — the canonical writer
                                         //   prints it compact). An omitted
                                         //   side list is
                                         //   unconstrained (IN-only /
                                         //   OUT-only are legal SurrealQL —
                                         //   the emitter prints only the
                                         //   non-empty side), and both
                                         //   omitted (`table -> <name> ->;`)
                                         //   is equivalent to the
                                         //   `table relation <name>;` form
                                         //   below. Excludes the any
                                         //   qualifier. The `lightweight`
                                         //   word trails the header (3.3 —
                                         //   below); it needs both sides
                                         //   named.

// Header reading: left to right, no backtracking — a from-list is
// the `|`-separated identifiers before the first `->` (absent when
// `->` comes straight after `table`), the name is the identifier
// between the two arrows, and the to-list is the `|`-separated
// identifiers following the second arrow, up to the block or `;`.
// A header with only one arrow is a parse
// error — the second arrow is mandatory. The variant words (`any`,
// `relation`, `view`) are variant words only when followed by a
// name — a table literally named `any` in a from-list
// (`table any -> wrote ->;`) is an identifier like any other. The
// to-list read is GREEDY: an endpoint named `lightweight` reads as
// an endpoint (`table a -> b -> lightweight;` — OUT [lightweight],
// no flag), and the flag needs its list closed before it
// (`-> c lightweight { … }`); the quoted spelling is always an
// endpoint (rule 9). Missing-FROM is therefore the only sides
// violation the arrow form can spell — the validation still guards
// the hand-built AST. The `relation` spelling cannot carry the word
// (`table relation <name> lightweight` diagnoses loud — the word is
// junk after the name, and the engine's own define would reject the
// sides-less shape).

table relation <name> { … };             // canonical spelling — the bare
                                         //   skeleton `table -> <name> ->;`
                                         //   is equivalent

table view <name> {
  <select-query>;                        // the block IS the query — a single
                                       //   `;`-terminated expression, no
  // optional trailing clauses:          //   clause key; SurrealQL views are
  permissions: none | full;              //   a RESTRICTED select (field list,
  permissions { … };                     //   FROM table list, WHERE, GROUP)
  graphql { … };                         //   — validated by the embedded
};                                     //   parser; anything beyond routes
                                       //   to raw. changefeed;/drop; are
                                       //   meaningless on views — SDL
                                       //   rejects them.

// clauses valid in any non-view table block:
{
  // optional:
  schemafull;                         // flag — see the fold note above;
                                       //   also legal in the HEADER
                                       //   (before the block): the
                                       //   examples' `table user
                                       //   schemafull { … }` spelling
  schemaless;                         // flag — always emitted
  drop;                               // flag — the server silently skips
                                       //   storing/indexing/edge-linking
                                       //   records written to this table
  changefeed {
    expiry: <duration>;               // required inside the sub-block
    include original;                 // flag
  };
  permissions: none | full;            // all-ops shorthand, or:
  permissions {                        // per-op; unlisted ops are NONE
    select: none | full | <expr>;
    create, update: none | full | <expr>;
    delete: none | full | <expr>;
  };
  graphql {
    alias: <string>;
    deprecated: <string>;
  };
  inline edges: <n>;                   // 3.3 — the inline cache cap, 0–256
                                       //   (number mandatory; SDL-E1057
                                       //   above the bound). Kind-
                                       //   independent; not authorable
                                       //   on views
  inline references: <n>;              // 3.3 — same shape

  // relation only:
  enforced;                           // flag — see the semantics matrix;
                                       //   legal on the unconstrained form
                                       //   too (checks existence only).
                                       //   Implied by `lightweight` —
                                       //   canonical SDL prints the word
                                       //   only when NOT lightweight

  // nested definitions (fields, indexes, events):
  <field | index | event definitions>  // forbidden under `lightweight`
                                       //   (the engine auto-installs
                                       //   `in`/`out` and rejects every
                                       //   DEFINE FIELD/INDEX/EVENT on
                                       //   the table)
};
```

## Relation semantics (verified, v3.2.3)

`RELATE` **requires** `TYPE RELATION` — a normal table with typed `in`/
`out` fields is rejected as an edge target (`… which is a relation, but
expected a NORMAL`). Within a relation table:

| declaration | endpoint table types | dangling endpoints |
| --- | --- | --- |
| both endpoint lists empty, no `enforced;` (`table relation generic_edge;` or the equivalent bare skeleton `table -> generic_edge ->;`) | unconstrained | allowed |
| arrow with a present side, no `enforced;` | the present side **typed** (`record<…>` coercion); an omitted side unconstrained (bare `record`) | **allowed** (`user:99` need not exist) |
| any relation shape (arrow or relation-word) + `enforced;` | enforced | **rejected** (`The record 'user:99' does not exist`) — both endpoints' existence is checked, including on an omitted (unconstrained) side |
| `lightweight` (both sides **mandatory**) | both sides typed, `enforced` **implied** (the engine normalizes the flag at define — INFO always prints `ENFORCED LIGHTWEIGHT`) | rejected (as `enforced;`) — and the table is **record-less**: edges are canonical `[in, out]` id pairs, `RELATE` is idempotent per pair, no data mutations, no fields/indexes/events/changefeed/drop/schemafull (see the semantics list) |

The arrow implies typed endpoints on the sides it names; `enforced;`
adds exactly the endpoint-existence check — unconditionally on both
endpoints, regardless of the lists (source: `doc/edges.rs`
`store_edges_data`, `record_exists` on `in` and `out`; the type
constraint itself rides the materialized `in`/`out` fields).

## Nesting

Fields, indexes, and events are table-scoped and nest inside the table
block, omitting their `on:` clause (standalone forms keep it — exactly
one of the two applies). A non-view table block also carries **spread
entries** — `...<name>;` — which expand to a [mixin](mixin.md)'s
members at compile time, in place; an explicit member of the same
family key wholly overshadows the member ([mixin.md](mixin.md)).
Collision
rule (from [field.md](field.md)): a
field named like an active clause key takes the `field` prefix.
Emission order: the `DEFINE TABLE …` header+clauses first, then nested
definitions in the compiler's order (rule 7 — an index lands after the
fields it names regardless of authored position, a fulltext index's
analyzer rides the table's own top-level edge, and authored order
breaks the ties; on schemafull tables SurrealDB validates
field existence at define time). The canonical SDL body prints in the
member layout: the
table's clauses, then fields, then the spread entries (relative order
preserved — the last-occurrence-wins resolution's semantics lives
there), then indexes, then events, each group a stable sort — fields
before the spreads so a spread-carried index can never define above
an explicit field it names; DDL emission reorders only by the rule-7
sort.

```sdl
table post schemafull {
  permissions {
    select: published = true or author = $auth.id;
    create, update: author = $auth.id;
    delete: $auth.admin = true;
  };

  author: record<user> {
    reference {
      on delete: cascade;
    };
  };
  title: string {
    assert: string::len($value) > 0;
  };
  slug: string;
  body: string;

  index unique slug_unique {
    fields: slug;
  };
  index fulltext body_fts {
    fields: body;
    analyzer: english;
    highlights;
  };

  event audit_title {
    when: $event = 'UPDATE' and $before.title != $after.title;
    then: { create audit set at = time::now(), id = $after.id; };
  };
};
```

Emit (SDL canonical order; `OVERWRITE` implied): `DEFINE TABLE <name>
[TYPE …] [DROP] [SCHEMAFULL|SCHEMALESS] [CHANGEFEED …] [AS SELECT …]
[PERMISSIONS …] [GRAPHQL_* …] [INLINE EDGES <n>] [INLINE REFERENCES
<n>] [COMMENT …];` then nested `DEFINE FIELD/INDEX/EVENT …` in order.
The server's own ToSql order differs slightly (COMMENT/GRAPHQL before
AS/CHANGEFEED, the INLINE caps after COMMENT/GRAPHQL but before
AS/CHANGEFEED, permission groups merged by equal value with
delete-Full elided) — both parse; the differ compares resolved state:
kinds under the algebra, expressions over canonical text, raw bodies
as trimmed text — never authored spellings.

## Reverse parser (INFO → SDL)

`PERMISSIONS` always renders: all-none as the single value `NONE`,
per-op lines grouped by equal value (`FOR select, create FULL` — op
words lowercase, the emitter's own spelling), with `FOR delete FULL`
**elided** (see the semantics list; engine-probed 2026-09-21). Canonical
minimal form: `PERMISSIONS NONE` (the default) elides entirely;
`TYPE`/`SCHEMALESS` materialize per the bijective mapping above.
- **View emission is fixed:** SDL views always emit
  `TYPE ANY SCHEMALESS AS SELECT …`. A parsed view whose INFO deviates
  — `TYPE NORMAL`/`SCHEMAFULL`, or carrying `DROP`/`CHANGEFEED` (legal
  SurrealQL; SDL rejects those clauses on views) — cannot round-trip
  through the `table view` form and **routes to `raw`** (re-emitting
  the SDL form would silently mutate the table on re-apply).
- **`in`/`out` field fold (relation tables):** every relation table's
  executor materializes `in`/`out` field definitions
  (`record<from-list>` / `record<to-list>` — an **empty list renders
  as bare `record`**: the unconstrained form's sides, or the omitted
  side of a one-sided arrow). The reverse parser reads them as the
  ordinary nested fields they print as — faithful reads carry the
  materialization (elision is the normalizer's verb); the
  **normalizer** folds each into the table's relation shape exactly
  when its kind equals the endpoint list's kind and all other
  clauses are defaults (`PERMISSIONS FULL`, nothing else); folds are
  independent per side (in may fold while a customized out is kept).
  A divergent or user-customized `in`/`out` (own clauses, different
  kind) is kept as a nested field and never a REMOVE candidate.
  **Exception — lightweight hosts drop them:** a LIGHTWEIGHT
  relation's `in`/`out` fields are the header's own implication (the
  engine auto-installs them and rejects every DEFINE FIELD on the
  table), so nesting them would print SDL the engine itself rejects;
  the reader drops them **in their implied shape** — the endpoint
  kind under the algebra, the FULL default, no other clause — the
  endpoints in the kind being the source of truth, exactly as
  `enforced` is. Any other child (a divergent hand-carried
  `in`/`out` included — impossible live, and dropping it would
  mis-represent state) keeps the faithful-read behavior and the
  written SDL trips the parser's lightweight validation loud
  (fail-closed; issue #21). A sides-less LIGHTWEIGHT (`TYPE RELATION
  LIGHTWEIGHT …` with no IN/OUT) diagnoses — the writer's
  `(None, None)` spelling could not carry the word, so reading one
  would silently drop the flag on fmt.
- **The 3.3 shapes adopt:** `ENFORCED LIGHTWEIGHT` prints
  after the sides; `ENFORCED` stops the side captures and
  `LIGHTWEIGHT` rides the payload, where the trailing-flag peel
  splits it (`lightweight` is also a legal endpoint ident — a
  candidate at the payload's head or preceded by `|` is an endpoint,
  mirroring the SDL-side greedy read); a hand-fed `LIGHTWEIGHT`
  without `ENFORCED` reads the same AST — the parse-fold. `INLINE
  EDGES|REFERENCES <n>` read after COMMENT (the printer's position).
  A cap the engine would reject (>256, non-integer) diagnoses. A
  cap-bearing **view** deviates to `raw` (SDL does not author the
  caps on views). `ENFORCED`/`LIGHTWEIGHT` on a NORMAL/ANY table
  trips the kind guard.
- Fields/indexes/events arrive via `INFO FOR TABLE` as separate
  statements and nest into their table (the map's `lives` and
  `tables` keys — live queries and the foreign-view registry — are
  ignored). Every `TYPE RELATION` shape is expressible: full, IN-only
  (`table user -> wrote ->;`), OUT-only (`table -> wrote -> post;`),
  and the unconstrained `table relation <name>;` (or the equivalent
  bare skeleton `table -> name ->;`, emitted canonically as the
  relation-word form) — the header arrows re-spell
  exactly the non-empty `IN`/`OUT` sides. One exception: a FIRST
  `IN` endpoint whose name is not word-shaped (`` `a.b` `` —
  engine-legal; the header position prints that first element bare,
  so only shape-conforming names re-parse) cannot ride the arrow
  form — the statement routes to verbatim `raw`; later
  elements and the `OUT` side re-parse quoted.

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): default `PERMISSIONS NONE`
materialized ✅; `TYPE NORMAL SCHEMALESS` combo ✅; RELATE into NORMAL
rejected / into ANY accepted ✅; relation pipe lists `IN user | org
OUT post | comment ENFORCED` ✅; bare `TYPE RELATION` (unconstrained)
✅; view `AS SELECT count() AS total FROM post GROUP ALL` ✅; `DROP` +
`CHANGEFEED 1d INCLUDE ORIGINAL` ✅; `GRAPHQL_ALIAS` ✅; single-value
`PERMISSIONS NONE|FULL` on tables ✅; comma endpoint lists rejected ✅
(`|` is the separator); unlisted-op=NONE seeding ✅. Live probes
(2026-09-25): a lightweight relation authored without ENFORCED
re-prints `ENFORCED LIGHTWEIGHT` ✅; `DROP`/`CHANGEFEED`/`SCHEMAFULL`
+ lightweight rejected ("cannot be declared …: its edges store no
records") ✅; `DEFINE FIELD/INDEX/EVENT` on lightweight rejected
(carry-variant messages) ✅; `PERMISSIONS FULL`/`SCHEMALESS`/caps/
explicit `ENFORCED` + lightweight accepted ✅; caps print only when
written, EDGES before REFERENCES, after COMMENT ✅; cap 256 accepted,
257 rejected ("an INLINE cache cap cannot exceed 256") ✅; cap `0`
materializes and re-prints ✅; caps accepted on NORMAL/ANY/view/
lightweight tables ✅; turning lightweight off on an empty table
accepted ✅; lightweight's `INFO FOR TABLE` fields are exactly the
auto-installed `in`/`out` ✅.

## Examples (one per clause)

schemafull + nested field + doc comment (the workhorse form — the
Nesting section has the full example):

```sdl
/// Application users
table user schemafull {
  permissions {
    select: full;
    create: full;
    update: $auth.id = id;
    delete: none;
  };

  email: string {
    assert: string::is_email($value);
  };
};
```

```surql
DEFINE TABLE user TYPE NORMAL SCHEMAFULL
  PERMISSIONS FOR select FULL, FOR create FULL,
  FOR update WHERE $auth.id = id, FOR delete NONE
  COMMENT 'Application users';
DEFINE FIELD email ON user TYPE string ASSERT string::is::email($value);
```

drop + changefeed (append-only audit trail):

```sdl
table audit {
  drop;
  schemaless;
  changefeed {
    expiry: 30d;
    include original;
  };
};
```

```surql
DEFINE TABLE audit TYPE NORMAL DROP SCHEMALESS CHANGEFEED 4w2d INCLUDE ORIGINAL;
```

(The changefeed emits value-normalized — the embedded duration canon
prints `30d` as `4w2d`, the same weeks+days fact verified for
`access` durations; durations compare by parsed value, never
spelling.)

relation with pipe endpoints + enforced + its own field, the
one-sided forms, and the unconstrained form:

```sdl
table user | org->wrote->post | comment {
  enforced;
  at: datetime {
    default: { time::now() };
  };
};
table follower->watched->;                 // IN-only — OUT unconstrained
table->tagged->post;                       // OUT-only — IN unconstrained
table->follows->;                          // both omitted — the relation-word
                                           //   form's equivalent (below)
table relation generic_edge;
```

```surql
DEFINE TABLE wrote TYPE RELATION IN user | org OUT post | comment ENFORCED SCHEMALESS;
DEFINE FIELD at ON wrote TYPE datetime DEFAULT time::now();
DEFINE TABLE watched TYPE RELATION IN follower SCHEMALESS;
DEFINE TABLE tagged TYPE RELATION OUT post SCHEMALESS;
DEFINE TABLE follows TYPE RELATION SCHEMALESS;
DEFINE TABLE generic_edge TYPE RELATION SCHEMALESS;
```

view (read-only aggregation — the block is the query):

```sdl
// The view's source must exist first — SurrealDB validates it at
// define time.
table post { schemaless; };

/// Posts per author
table view post_stats {
  select author, count() as posts from post group by author;
};
```

```surql
DEFINE TABLE post_stats TYPE ANY SCHEMALESS
  AS SELECT author, count() AS posts FROM post GROUP BY author
  COMMENT 'Posts per author';
```

LIGHTWEIGHT relation (3.3 — the record-less edge; the implied
`enforced` prints) and the inline cache caps:

```sdl
table person->likes->post lightweight;
table person->watched->post lightweight {
  permissions: full;
  inline edges: 8;
};
table hot { inline references: 16; };
```

```surql
DEFINE TABLE likes TYPE RELATION IN person OUT post ENFORCED LIGHTWEIGHT SCHEMALESS;
DEFINE TABLE watched TYPE RELATION IN person OUT post ENFORCED LIGHTWEIGHT
  SCHEMALESS INLINE EDGES 8 PERMISSIONS FULL;
DEFINE TABLE hot TYPE NORMAL SCHEMALESS INLINE REFERENCES 16;
```

graphql deprecation:

```sdl
table legacy_reports {
  graphql {
    deprecated: 'use reports';
  };
};
```

```surql
DEFINE TABLE legacy_reports TYPE NORMAL SCHEMALESS GRAPHQL_DEPRECATED 'use reports';
```
