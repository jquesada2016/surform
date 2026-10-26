Every field's type is SurrealDB's own kind grammar, passed through
verbatim — Surform adds no spellings and hides none. (The one
addition is the compile-time [`type` alias](type-aliases.html): a
named kind a Capitalized word references.) This page covers the
shapes you'll use most; SurrealQL's `kind` grammar is the complete
reference.

## Scalars

```sdl
namespace acme;
database shop;

table kinds {
  schemafull;
  a_string: string;
  an_int: int;
  a_float: float;
  a_number: number;
  a_decimal: decimal;
  a_bool: bool;
  a_datetime: datetime;
  a_duration: duration;
  a_uuid: uuid;
  some_bytes: bytes;
};
```

`number` accepts both ints and floats; `decimal` is the exact-base-10
type for money.

## Option: "or nothing"

`option<T>` marks a field the record may leave out. It is equivalent
to the union `none | T` — SurrealQL's parser desugars one
into the other, Surform knows they are the same type, and the
canonical spelling is always `option<T>`:

```sdl
namespace acme;
database shop;

table person {
  schemafull;
  nickname: option<string>;
};
```

## Collections

`array<T>` lists, `set<T>` unique lists, both with optional fixed
length; `object` for free-form nesting:

```sdl
namespace acme;
database shop;

table order {
  schemafull;
  items: array<string>;
  matrix: array<float, 3>;
  lottery: set<int>;
  meta: object;
};
```

## Records: references to other tables

`record<table>` is a pointer to a record in a named table — the type
SurrealDB's graph model runs on. List tables to constrain, or leave the
list empty for any:

```sdl
namespace acme;
database shop;

table post {
  schemafull;
  author: record<user>;
  related: record<post | comment>;
  parent: option<record<post>>;
  anything: record;
};
```

Multiple tables inside `record<…>` join with `|` — it is a union.
Which is the next stop:

## Unions and literals

Types combine with `|`, and literal values are types — hand-rolled enums,
one line:

```sdl
namespace acme;
database shop;

table shirt {
  schemafull;
  size: 'small' | 'medium' | 'large';
  id_or_code: int | string;
  flags: array<'red' | 'green' | 'blue'>;
};
```

## Inline object schemas

An object type can spell out its expected shape right in the header —
the braces belong to the type grammar, not to a clause block:

```sdl
namespace acme;
database shop;

table webhook {
  schemafull;
  payload: { a: string, b: number };
};
```

## Geometry and friends

Geography, tables-as-values, files, ranges, regexes — the remaining
SurrealQL kinds all pass through:

```sdl
namespace acme;
database shop;

table place {
  schemafull;
  location: geometry<point>;
  area: geometry<polygon>;
  attachment: file<photo>;
  window: range;
  pattern: regex;
};
```

(`range` is its own whole kind here — it takes no inner type in the
grammar.)

## How types compare

Two spellings that parse to the same kind are the same type:
`option<string>` ≡ `none | string`, `int | int` ≡ `int`, and unions
compare flattened and deduplicated. This matters when you
[normalize](normalize.html): the database prints its desugared view
(`none | string`), and the compiler folds it back to the `option<T>`
you meant — your file doesn't churn when the database round-trips it.

## Where next

- [Fields](fields.html) — everything a field block can do.
- [Relations](relations.html) — `record<T>` fields with lifelines
  (`reference`).
- [Indexes](indexes.html) — indexes over these types.
