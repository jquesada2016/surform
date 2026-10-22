Indexes make lookups fast and rules enforceable. The index **kind**
sits in the header — `unique`, `count`, `fulltext`, `hnsw`, `diskann`
— and each kind brings its own clauses.

## The everyday two

A plain index is a lookup; a `unique` index is a rule:

```sdl
namespace acme;
database shop;

table message {
  schemafull;
  field account: string;
  field thread_id: string;
  field sent_at: datetime;

  // Composite lookup for inbox queries.
  index by_account_thread {
    fields: account, thread_id;
  };
  index by_recency {
    fields: sent_at;
  };
};
```

`fields:` takes an expression list — usually plain field names, comma
separated. (SurrealQL's `COLUMNS` spelling is accepted as an alias.)

```sdl
namespace acme;
database shop;

table user {
  schemafull;
  field email: string;

  index unique one_email {
    fields: email;
    concurrently;
  };
};
```

`concurrently;` builds the index in the background — a build-time
directive only; the database doesn't remember it.

## Standalone or nested

Every index above nests inside its table. The standalone form carries
an `on:` line instead — handy when you generate fields
programmatically, and what the database's report reads back as:

```sdl
namespace acme;
database shop;

table user {
  schemafull;
  field email: string;
};

/// No two accounts share an email.
index unique one_email {
  on: user;
  fields: email;
};
```

## Count: fast totals

A count index maintains a running count — no `fields:`, optionally
filtered:

```sdl
namespace acme;
database shop;

table reading {
  schemafull;
  field device: string;
  field reading: number;
};

table item {
  schemafull;
  field status: string;

  index count active_count {
    where: status = 'active';
  };
};
```

## Fulltext: search over text

A fulltext index covers exactly **one** field, through an analyzer
(the default analyzer applies when you omit the rest):

```sdl
namespace acme;
database shop;

table post {
  schemafull;
  field title: string;
  field body: string;

  index fulltext body_fts {
    fields: body;
  };
  index fulltext title_fts {
    fields: title;
    analyzer: english;
    bm25: (1.2, 0.75);
    highlights;
  };
};
```

The pieces: `analyzer:` names an [analyzer](analyzers.html);
`bm25:` is the scoring pair; `highlights;` enables highlighting in
query results. At the defaults the clause disappears —
`index fulltext body_fts { fields: body; };` is the complete minimal
form, and the database's verbose default printout folds back to it on
[normalize](normalize.html). Authoring order between an analyzer and
the index that names it does not matter: `surform build` orders the
emitted DDL itself — the analyzer lands before the fulltext index
that names it — so the define-time hazard never arises (a true
dependency cycle is an error, not an ordering warning).

## Vector search: hnsw and diskann

Both vector kinds index exactly one field — an array of numbers —
and require `dimension:`:

```sdl
namespace acme;
database shop;

table embedding {
  schemafull;
  field vector: array<float>;

  index hnsw vec {
    fields: vector;
    dimension: 768;
    distance: cosine;
  };
};
```

Distance — `minkowski` takes its order in SDL's paren form, and the two
pruning flags ride as plain flags:

```sdl
namespace acme;
database shop;

table embedding {
  schemafull;
  field vector: array<float>;

  index hnsw vec_tuned {
    fields: vector;
    dimension: 768;
    distance: minkowski(3);
    type: f16;
    m: 16;
    efc: 200;
    extend_candidates;
    keep_pruned_connections;
  };
};
```

Omitted HNSW parameters derive from `m` — and when the database
reports the derived values back (`m0`, `lm`), normalize folds them
away again as long as they match.

`diskann` is the on-disk cousin — different parameter names, a
restricted distance/type menu:

```sdl
namespace acme;
database shop;

table document {
  schemafull;
  field embedding: array<float>;

  index diskann big_vec {
    fields: embedding;
    dimension: 1536;
    distance: cosine_normalized;
    type: f16;
    degree: 32;
    l_build: 50;
    alpha: 1.3;
    hashed_vector;
  };
};
```

## Where next

- [Analyzers](analyzers.html) — tokenizers and filters for fulltext.
- [Fields](fields.html) — the fields these indexes cover.
- [Schema migration](migrate.html) — comparing declared indexes with
  what `INFO FOR TABLE` reports.
