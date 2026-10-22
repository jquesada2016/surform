Tables are the largest unit of a schema file: the kind of the table lives
in the header, and everything else — permissions, changefeeds, nested
fields and indexes — lives in the block.

## The everyday table

A bare table is a **normal** table, and the DDL says so even when you
don't — `TYPE NORMAL` is always emitted, because a normal table
rejects SurrealQL's `RELATE` statements:

```sdl info
namespace acme;
database shop;

/// Application users.
table user schemafull {
  field email: string;
};
```

The third panel is the round trip: the DDL was applied to an
in-process SurrealDB 3.3.0 engine, its `INFO` report read back, and
the canonical form
is what Surform writes from that — engine-side defaults
(`PERMISSIONS NONE`, the explicit `TYPE NORMAL`) elide again.

`schemafull` (records must match their declared fields) or
`schemaless` (records may carry anything) — the schema flag is always
emitted, either way. You can also spell it in the header:
`table user schemafull { … };` is the same table.

## The kind rides the header

Four kinds, four header shapes:

```sdl
namespace acme;
database shop;

table person { };                          // normal (the default, emitted as TYPE NORMAL)
table any thing { };                       // any — accepts RELATE without being a relation
table relation generic_edge;               // relation, unconstrained
table post { schemaless; field author: string; };

/// Posts per author — a view. The block IS the query.
table view post_stats {
  select author, count() as posts from post group by author;
};
```

Only relation tables accept
`RELATE`, plus tables explicitly declared `any`. If your table must
take graph edges some day but is schemaless for now, say `any`.

## Relations and graph edges

Relations get their own page — [Relations](relations.html) — because
the arrow forms, `enforced`, and the 3.3 `lightweight` edges have a
lot to say. The one-line version:

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

`from -> edge -> to`, spelled in `RELATE`'s own order, with `enforced`
checking that both endpoints exist on every edge.

## Permissions: the default is NO

**A table with no permissions clause denies record users
everything.** That default is stored
(`PERMISSIONS NONE` in the database's record) — omitting the clause
is the secure default, and the linter never warns on it. What the
linter does warn on is the opposite extreme, `permissions: full;`,
which hands the table to everyone. The per-operation form scopes
each operation independently, and unlisted field operations default
the other way — to FULL:

```sdl
namespace acme;
database shop;

table user schemafull {
  permissions {
    select: full;
    create: full;
    update: $auth.id = id;
    delete: none;
  };
  field email: string;
  field password_hash: string {
    permissions {
      select: none;
      update: none;
      // create stays allowed — fields default unlisted
      // operations to FULL (the opposite of tables).
    };
  };
};
```

Unlisted operations are **none** for tables. Operations can be
comma-grouped when they share a value, and the whole clause has a
one-line shorthand (`permissions: full;`) when every operation gets
the same answer. Expressions are real SurrealQL — `$auth` is the
signed-in record.

## Lifecycle: drop and changefeed

```sdl
namespace acme;
database shop;

table audit {
  drop;
  schemaless;
  changefeed {
    expiry: 30d;
    include original;
  };
};
```

`drop;` tells the server to silently skip storing, indexing, and
edge-linking anything written to the table — useful to keep history
flowing without keeping data. `changefeed` keeps a change log for the
given duration; `include original;` stores each change's before-image.
(Durations print value-normalized — `30d` appears as `4w2d` in the
DDL, SurrealDB's canonical decomposition. The value is identical.)

## Inline cache caps (SurrealDB 3.3)

Two numeric clauses tune the per-table inline caches. The number is
mandatory, 0–256, and the caps are legal on every table SDL can
author (views don't carry them — SDL has no spelling there):

```sdl
namespace acme;
database shop;

table person->likes->post lightweight {
  permissions: full;
  inline edges: 8;
};

table hot {
  schemaless;
  inline references: 16;
};
```

Above the bound Surform rejects it at check time:

```sdl-error expect=SDL-E1057
namespace acme;
database shop;

table hot {
  schemaless;
  inline references: 257;
};
```

## Views: the block is the query

A view is a stored, read-only query pretending to be a table. There is
no clause key — the block's first statement is the `SELECT` itself,
validated by SurrealQL's own parser:

```sdl
namespace acme;
database shop;

table post { schemaless; field author: string; };

/// Posts per author.
table view post_stats {
  select author, count() as posts from post group by author;
  permissions: full;
};
```

Views always emit as `TYPE ANY SCHEMALESS AS SELECT …`. A view reads
other tables — `build` orders the view after them in the emitted DDL,
so the view may stand anywhere in the file (a view chain that reads
itself is the SDL-E0015 error — [who owns the
order](basics.html#who-owns-the-order)).

## GraphQL hints

If you serve the table through SurrealDB's GraphQL interface, the
`graphql` block carries its per-table hints:

```sdl
namespace acme;
database shop;

table legacy_reports {
  schemaless;
  graphql {
    alias: 'old_reports';
    deprecated: 'use reports instead';
  };
};
```

## Nesting: fields, indexes, events live in the table

The common pattern — children nested in their table, no `on:` line
needed. They emit as standalone `DEFINE FIELD/INDEX/EVENT` statements,
right after their table, in authored order (the emission sort's
tie-break — an index that names a field always follows that field,
however the file orders them):

```sdl
namespace acme;
database shop;

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

  index unique slug_unique {
    fields: slug;
  };

  event audit_title {
    when: $event = 'UPDATE' and $before.title != $after.title;
    then: { create audit set at = time::now(), id = $after.id; };
  };
};
```

Standalone forms (`index … on post { … };` at top level) exist and
read back from the database too — see
[Fields](fields.html), [Indexes](indexes.html), and
[Events](events.html).

## Where next

- [Fields](fields.html) — the type system, defaults, assertions,
  computed fields.
- [Relations](relations.html) — arrows, `enforced`, `lightweight`.
- [The linter](lint.html) — the world-open-table warning, among
  others.
