When several tables share the same columns, a `mixin` defines the
group once and each table spreads it: `mixin` holds member
definitions — fields, indexes, and events — and `...name;` copies them
into the table at compile time.

## The mechanism

```sdl
namespace acme;
database shop;

mixin timestamped {
  created_at: datetime { default: time::now(); };
  updated_at: datetime { default: time::now(); };
};

table post schemafull {
  ...timestamped;
  title: string;
};

table comment schemafull {
  ...timestamped;
  body: string;
};
```

The database never sees a mixin. At emission each spread expands to
the members as if you had written them in the table — in place, at
the spread's position. A member is an ordinary nested definition, so
everything in the [fields](fields.html) page applies to field
members, and an index or event member follows its own grammar —
`on:` never appears on a member; the spreading table becomes the
`on` at expansion:

```sdl
namespace acme;
database shop;

mixin slug_source {
  slug: string;
  index slug_u {
    fields: slug;
    concurrently;
  };
  event slug_touched {
    when: slug != '';
    then: 1;
  };
};

table article schemafull {
  ...slug_source;
  body: string;
};
```

## Explicit members overshadow

An explicit field, index, or event with the same name as a member
replaces that member in that table — completely, wherever the two sit
in the block, with no clause merging:

```sdl
namespace acme;
database shop;

mixin stamp {
  created_at: datetime { default: time::now(); };
  updated_at: datetime;
};

table invoice schemafull {
  ...stamp;
  // the explicit field wins on this table — the mixin's
  // `created_at` (with its default) never lands here:
  created_at: datetime;
  amount: decimal;
};
```

A field `x` and an index `x` never collide — fields, indexes, and
events override per kind. Other tables spreading the same mixin are
unaffected — overriding is per-table.

## Conflicts between spreads resolve last-wins

Two spreads carrying the same member name compile to the LATER
spread's definition — the earlier one is skipped at expansion, never
defined-then-overwritten. A silent winner between two third-party
mixins is easy to miss, so the linter flags every such shadow:

```sdl-lint expect=SDL-L0022
namespace acme;
database shop;

mixin short_id {
  id_hint: int;
};

mixin long_id {
  id_hint: string;
};

table post schemafull {
  ...short_id;
  // `id_hint` compiles to `string` — long_id is the later spread:
  ...long_id;
};
```

An explicit member decides instead — wherever it sits — and its
overshadow is deliberate, so it stays unlinted. Spreading the same
mixin twice is still an error (`SDL-E1064`).

## The rules mixins play by

- **Top level only, resolved document-wide.** A `mixin` is a
  top-level statement and a spread lives in a non-view `table` block;
  position is free — a spread resolves against every mixin the
  document defines, above or below it, in the file or composed into
  it.
- **No doc comments.** `///` is rejected on the statement (it emits
  nothing, so there is no COMMENT to hang one on) and on members (the
  nested-statement rule).
- **Conflicts resolve last-wins.** Between spreads, the later spread's
  member wholly wins (`SDL-L0022` advises on the shadow); an explicit
  member decides position-independently; spreading the same mixin
  twice is an error.
- **An unspread mixin emits nothing.** The linter flags it
  (`SDL-L0021`) — dead weight in the file.
- **One key per body.** The same member key twice in one body —
  a mixin's or a table's — is an error (`SDL-E1067`): the engine's
  last-wins would silently hide one of them.
- **`normalize` keeps your spreads.** Adopting a database into a
  mixin-carrying file merges per member: a member whose database
  state matches the definition stays covered by its spread, and a
  diverged one materializes as an explicit statement on the table —
  the override you would have written by hand:

```sdl info
mixin stamp {
  created_at: datetime;
  updated_at: datetime;
};

table post schemafull {
  ...stamp;
  title: string;
};
```

  The info panel's canonical readback is the same file — the spread
  survives the round trip. When the database's `created_at` has
  drifted (a default added by hand), `surform normalize` materializes
  the diverged member as the explicit override:

```sdl
mixin stamp {
  created_at: datetime;
  updated_at: datetime;
};

table post schemafull {
  created_at: datetime {
    default: time::now();
  };
  title: string;
  ...stamp;
};
```

  The spread survives — `updated_at` is still covered by it — and
  the override is exactly the overshadow rule performing the
  adoption. `migrate generate` reports the drift on the member
  itself, never on the mixin:

```drift scenario=member-drift
mixin stamp {
  created_at: datetime;
  updated_at: datetime;
};

table post schemafull {
  ...stamp;
  title: string;
};
```

## The reference errors

```sdl-error expect=SDL-E1061
namespace acme;
database shop;

table post schemafull {
  ...timestamped;
  title: string;
};
```

The spread names no mixin of the document (`SDL-E1062` duplicate
name, across composed files too; `SDL-E1064` double spread,
`SDL-E1065` a spread in the wrong block, and `SDL-E1067` a duplicate
member key within one body complete the family — see the
[mixin spec](spec-mixin.html)).

## Where next

- [Fields](fields.html) — the grammar a mixin's members follow.
- [The linter](lint.html) — the unspread-mixin advisory.
