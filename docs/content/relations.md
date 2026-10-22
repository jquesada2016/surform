SurrealDB's graph model lives in *relation tables*: tables whose
records are edges, each connecting an `in` record to an `out` record.
Surform spells relations in the header, in `RELATE`'s own order.

## The arrow form

```sdl
namespace acme;
database shop;

table user->wrote->post {
  enforced;
  at: datetime {
    default: time::now();
  };
};
```

`from -> edge -> to`. The edge table's own fields (`at` above) nest
inside, exactly like a normal table's. What the header adds:

- the sides become **typed** — `in` must be a `user` record, `out`
  a `post` record;
- `enforced;` adds the existence check: writing an edge whose
  endpoints don't exist is rejected. Without it, dangling endpoints
  are allowed.

Sides can be **lists** (with `|`, SurrealQL's separator) and either
side can be omitted to leave it unconstrained:

```sdl
namespace acme;
database shop;

table user | org->wrote->post | comment {
  enforced;
};

table follower->watched->;  // IN-only: out is unconstrained
table->tagged->post;        // OUT-only: in is unconstrained
```

## The unconstrained relation

An edge with no named sides at all — the generic join:

```sdl
namespace acme;
database shop;

table relation generic_edge;
```

`table -> generic_edge ->;` means the same thing; the
`relation` word is the canonical spelling.

## Lightweight edges (SurrealDB 3.3)

A **lightweight** relation stores no edge records at all — an edge is
just the canonical pair `[in, out]`. `RELATE` becomes idempotent per
pair, nothing beyond the endpoints can be mutated, and the endpoints
must be named:

```sdl
namespace acme;
database shop;

table person->likes->post lightweight;

table person->watched->post lightweight {
  permissions: full;
  inline edges: 8;
};
```

`lightweight` **implies `enforced`** — the engine always prints the
pair together, and so does Surform. Because a lightweight edge has no
records, the record-oriented clauses are rejected: `drop;`,
`changefeed`, `schemafull`, and any nested field, index, or event:

```sdl-error expect=SDL-E1044
namespace acme;
database shop;

table person->likes->post lightweight {
  schemafull;
};
```

A lightweight table can grow into a full relation later (while it
holds no edges) by dropping the word.

## The in/out fields you never write

Every relation table carries hidden `in` and `out` fields — the
endpoints. You never declare them; the engine installs them from the
header, the reader folds them back, and rewriting them by hand is a
lint (`SDL-L0010` — a record-typed `in`/`out` field would redefine the
endpoints behind the table's back).

## Relate-ability for normal tables

`RELATE` requires a relation table — with one exception: a table
declared `any` accepts edges too. If you have a schemaless table that
must take `RELATE` today and become a real relation someday, say
[`table any thing`](tables.html#the-kind-rides-the-header).

## Where next

- [Fields](fields.html) — `inline;` payloads on edge fields, and
  `reference` on plain record fields.
- [The linter](lint.html) — the un-enforced relation warning
  (`SDL-L0006`).
- [Indexes](indexes.html) — indexing edge fields for fast traversals.
