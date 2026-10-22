Fields decide what a record may hold — and what the database does with
values on the way in. The type sits right in the header
(`name: type`); everything else is a clause in the block.

## A field, standalone and nested

Inside a table block you write `name: type`. At top level the
same field takes an `on:` line naming its table — both spellings emit
the same `DEFINE FIELD`:

```sdl
namespace acme;
database shop;

table account {
  schemafull;
  field email: string;
};

/// Contact address for the account owner.
field email: string {
  on: account;
  assert: string::is_email($value);
  default: 'unknown@example.com';
};
```

The nested spelling is the everyday one (the [table
page](tables.html#nesting-fields-indexes-events-live-in-the-table)
shows it in context); the standalone form is how the database itself
reports fields, and it reads back into nested form on
[normalize](normalize.html).

## Controlling values: default, value, assert

- `default:` fills the value in when the create leaves it out.
- `default always:` fills it in even when a value was supplied.
- `value:` overrides whatever was supplied, every time.
- `assert:` is a predicate every write must satisfy — `$value` is the
  incoming value, and `throw` inside lets you customize the error.

```sdl
namespace acme;
database shop;

table ticket {
  schemafull;
  status: string {
    assert: $value inside ['open', 'closed'];
    default: 'open';
  };
  locked: bool {
    readonly;
    value: false;
  };
  tags: array<string> {
    default always: [];
  };
};
```

`$value inside ['open', 'closed']` is ordinary SurrealQL — the full
function library is available in every expression clause.

## Computed fields

A computed field has no storage: its value is derived at read time.
The short form is a header assignment — typed or typeless:

```sdl
namespace acme;
database shop;

table account {
  schemafull;
  field name: string;

  shout: string = string::uppercase(name);
  derived = string::uppercase(name);
};
```

When a computed field needs other clauses, use the block form with
`computed:` — a block can't follow the `=` form:

```sdl
namespace acme;
database shop;

table account {
  schemafull;
  field name: string;

  secret_shout: string {
    computed: string::uppercase(name);
    permissions {
      select: none;
    };
  };
};
```

Computed is mutually exclusive with `default`, `value`, `assert`,
`reference`, and `readonly` — Surform rejects the combination:

```sdl-error expect=SDL-E1049
namespace acme;
database shop;

table account {
  schemafull;
  field name: string;
  shout: string {
    computed: string::uppercase(name);
    readonly;
  };
};
```

## References: what happens when the target dies

Any field whose type is a record can declare how SurrealDB reacts when
the referenced record is deleted:

```sdl
namespace acme;
database shop;

table post {
  schemafull;
  field author: record<user> {
    reference {
      on delete: cascade;
    };
  };
  field editor: record<user> {
    reference;
  };
  field parent: record<post> {
    reference {
      on delete: { create cleanup_log set at = time::now() };
    };
  };
};
```

`reference;` alone means *ignore* (the engine's default, and what it
always prints back). The strategies are `reject`, `cascade`, `ignore`,
`unset` — or a custom SurrealQL block that runs instead. References
must sit on top-level record fields; the back-traversal `<~` idiom
lets the *referenced* side query its referrers, even as a computed
field:

```sdl
namespace acme;
database shop;

table person {
  schemafull;
  field comics: option<array<record<comic_book>>> {
    reference;
  };
};

table comic_book {
  schemafull;
  owners = <~person;
};
```

## Permissions: fields start open

The mirror image of tables: **unlisted operations are FULL for
fields**, and there is no `delete` operation (you can't delete one
field of a record). Use the shorthand or per-operation groups:

```sdl
namespace acme;
database shop;

table account {
  schemafull;
  secret: string {
    permissions: none;
  };
  audited: string {
    permissions {
      select: none;
      create, update: $auth.admin = true;
    };
  };
};
```

## Inline payloads (SurrealDB 3.3)

`inline;` on a field of a **relation table** embeds that value into
the edge-adjacency payloads, so filtered traversals like
`->likes WHERE score > 5` run without fetching edge records:

```sdl
namespace acme;
database shop;

table user->likes->post {
  schemaless;
  score: number {
    inline;
  };
};
```

The host must be a relation table, the name must be top-level (not
`id`/`in`/`out`, not dotted), and computed fields can't be inline —
there is nothing stored to embed:

```sdl-error expect=SDL-E1058
namespace acme;
database shop;

table page {
  schemafull;
  meta: object;
  meta.rating: number {
    inline;
  };
};
```

A table carries at most 255 inline fields; [the linter](lint.html)
counts them for you (`SDL-L0020`).

## GraphQL hints and paths

Fields take the same two GraphQL hints tables do, and field names may
be paths — dotted subfields and `.*` wildcards are ordinary fields to
SurrealDB:

```sdl
namespace acme;
database shop;

table profile {
  schemafull;
  display: string {
    graphql {
      alias: 'displayName';
    };
  };
  legacy: string {
    graphql {
      deprecated: 'use display';
    };
  };
  stats: object;
  stats.views: int {
    default: 0;
  };
  tags: array;
  tags.*: int;
};
```

## Where next

- [Types (kinds)](types.html) — every type the header accepts.
- [Relations](relations.html) — the tables `inline;` fields attach to.
- [Indexes](indexes.html) — indexes over these fields.
