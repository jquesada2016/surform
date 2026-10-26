When the same kind repeats across a schema — an email, a slug, a
score shape — a `type` statement names it once and every kind
position can reference the name. The alias is compile-time only: the
emitted DDL carries the expanded kind, exactly as if you had written
it out.

## The mechanism

```sdl
namespace acme;
database shop;

type Email = string;
type Emails = array<Email>;

table user schemafull {
  name: string;
  contact: Emails;
  recovery: option<Email>;
  both: Email | int;
  profile: { email: Email };
};

fn contacts($e: Email) -> Emails { return [$e]; };
```

The database never sees an alias. `contact` emits as
`TYPE array<string>`, `recovery` as `TYPE option<string>` — the
expansion happens at compile time, before emission, everywhere a kind
is legal: field types, function arguments and returns, union arms,
generic interiors, object-literal and array-literal values. Chained
aliases resolve recursively (`Emails` is `array<Email>`, which is
`array<string>`).

## Capitalized is the whole rule

A bare **Capitalized** word at a kind position is an alias reference;
a lowercase word is a SurrealQL kind. That is the entire
disambiguation — no sigil, no qualification:

- SurrealQL's kind heads are all lowercase (`string`, `record`,
  `array`, …), so the two territories never overlap.
- A declaration whose name would lose to a kind head is rejected:
  `type String = int;` is an error (`String` would match the
  `string` head at every use site — head matching ignores case).
- `record<Email>` is NOT a reference: the interiors of `record<`,
  `table<`, and `file<` name tables and buckets at every case. The
  field below keeps the table `email` as its target:

```sdl
namespace acme;
database shop;

type Email = string;

table account schemafull {
  owner: record<email>;
};
```

## Names resolve globally

The declarative namespace applies: an alias can be declared after
the field that uses it, or in another file of a
[multi-file schema](includes.html) — position is not resolution's
business. A reference that names nothing is an error (SDL-E1071 with
a did-you-mean), never a silent kind, so a typo'd alias cannot slip
through. Duplicate names are errors (SDL-E1062), and an alias cycle
(`type A = B; type B = A;`) is an error with the chain named
(SDL-E0015).

## fmt round-trips them

The canonical writer prints the statement verbatim — the kind in its
authored spelling — and expansion is never visible in canonical SDL.
The formatter, the linter, and the differ all treat a `type`
statement as a definition: comments ride it, `--in-place` writes it,
and it participates in the [declarative namespace](basics.html)'s
duplicates rule. Aliases work in [mixin](mixins.html) members and are
never expanded inside [raw](raw.html) bodies.

## Errors

| code | meaning |
| --- | --- |
| SDL-E1071 | a Capitalized reference names no alias of the document |
| SDL-E1072 | a declaration's name is not Capitalized, or is reserved (a kind head, or `NaN`/`Infinity`) |
| SDL-E1062 | the same alias name twice |
| SDL-E0015 | an alias-definition cycle |
