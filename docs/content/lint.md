`surform lint` is the advisory pass: warnings about files that are
*legal but suspicious*. Findings never change what `check` and
`build` emit — but like a clippy step, `surform lint` exits 1 when
anything fires, so it drops straight into CI:

```sh
surform lint schema.surdl && surform build schema.surdl > schema.surql
```

Findings render exactly like diagnostics — code, snippet, help line,
or `--json` — at warning severity. Codes are `SDL-Lxxxx`; this page
is the catalog.

## The warnings in action

A table wide open (`permissions: full;` — anyone, even unauthenticated
users, can read and write every record; omitting the clause is the
secure default and never warns):

```sdl-lint expect=SDL-L0001
namespace acme;
database shop;

table public_profile {
  schemafull;
  field name: string;
  permissions: full;
};
```

A plaintext password (the file is going into git — so is your
password):

```sdl-lint expect=SDL-L0002
namespace acme;
database shop;

user admin {
  on: root;
  password: 'change-me-immediately';
  roles: owner;
};
```

An ngram window that can only degrade (min larger than max — the
database accepts it silently):

```sdl-lint expect=SDL-L0004
namespace acme;
database shop;

analyzer fuzzy {
  filters: ngram(5, 2);
};
```

A relation that accepts dangling edges (`enforced;` missing):

```sdl-lint expect=SDL-L0006
namespace acme;
database shop;

table user;
table post;

table user->wrote->post;
```

And `delete: full;` on a table — legal, but the database never
reports it back, so the definition cannot round-trip or survive the
migrate compare:

```sdl-lint expect=SDL-L0014
namespace acme;
database shop;

table audit {
  permissions {
    delete: full;
  };
};
```

A reference to something no file defines — here a typo'd table in a
permission filter (`orgs` for `org`; SurrealDB's catalog is flat, so
the schema stays legal — the warning is the catch):

```sdl-lint expect=SDL-L0024
namespace acme;
database shop;

table org {
  field name: string;
};

table page {
  permissions {
    select: (SELECT * FROM orgs WHERE org = $auth.org);
  };
};
```

The reference warnings stay conservative on purpose: a param read in
a permission or default can be bound by the client at query time, so
only `fn::` bodies (whose context the engine isolates from
client-bound values) flag unknown params — a `param` definition
resolves inside a body, and so do the session params and the record
trio on a document-bound call. `mod::` calls are covered by their
module definitions, `silo::`/`ml::` names by their version tails, and
builtin paths (like an api middleware head named `string::uppercase`)
resolve without any definition.

Ordering is not a lint subject: the compiler owns emission order —
definitions may stand anywhere, a true cycle is the SDL-E0015
diagnostic, a dependency that crosses a scope-section boundary is
SDL-E0016, and a duplicate definition is SDL-E1062 (see
[Who owns the order](basics.html#who-owns-the-order)). The linter's
remaining reference concern is the one the compiler cannot decide:
a target that nothing in the document defines.

## The catalog

| code | warning |
|---|---|
| `SDL-L0001` | table `permissions: full;` — anyone, even unauthenticated users, can access every record |
| `SDL-L0002` | plaintext `password:` |
| `SDL-L0003` | secret-less user — define-once, probably a placeholder |
| `SDL-L0004` | `edgengram`/`ngram` min > max |
| `SDL-L0005` | duplicate entry in roles / tokenizers / filters |
| `SDL-L0006` | relation without `enforced;` |
| `SDL-L0007` | top-level field/index/event that could nest inside its table |
| `SDL-L0008` | param without a value |
| `SDL-L0009` | api with no effective `any` catch-all (or no groups) |
| `SDL-L0010` | record-typed `in`/`out` field rewriting a relation's endpoints |
| `SDL-L0011` | access duration side the access's capability silently drops |
| `SDL-L0012` | `raw` body the pinned SurrealQL parser rejects |
| `SDL-L0013` | applying a `'[REDACTED]'` sentinel as real credential material |
| `SDL-L0014` | table `delete: full;` — cannot round-trip |
| `SDL-L0015` | computed field shadowed by a field defined under it |
| `SDL-L0016` | `reference;` on a field whose type isn't record-shaped |
| `SDL-L0017` | `id` field type can't be a record-id key |
| `SDL-L0018` | `flexible;` on a non-SCHEMAFULL host (where it does nothing) |
| `SDL-L0019` | `inline;` field whose host isn't a relation table |
| `SDL-L0020` | table carrying more than 255 `inline;` fields |
| `SDL-L0021` | mixin no table spreads — it emits nothing anywhere |
| `SDL-L0022` | a later spread's member shadows an earlier spread's — the later definition wins at expansion |
| `SDL-L0023` | a `surform-lint-allow:` comment's own problem — unknown code, misspelled marker, or no statement to attach to |
| `SDL-L0024` | a referenced table, analyzer, or `fn::` path (or a `fn::`-body param) that nothing in the linted files defines |
| `SDL-L0025` | RETIRED (2026-10-06) — the compiler owns emission order (see [the basics](basics.html#who-owns-the-order)); the number keeps its gap |
| `SDL-L0026` | a diamond include — a file the composition already spliced, declared again (first inclusion wins) |

## Suppressing a warning at its site

A deliberate exception does not need to live with the noise: a
`// surform-lint-allow:` comment names the codes it excuses, and it
excuses them on ONE statement — the statement it precedes (a
same-line comment after the `;` works too, the one-liner's natural
home):

```sdl-lint expect=none
namespace acme;
database shop;

// A public reference table: world-readable by design.
// surform-lint-allow: SDL-L0001
table countries {
  field code: string;
  field name: string;
  permissions: full;
};
```

The allow is statement-scoped, not file-wide — moved onto another
statement, it excuses nothing:

```sdl-lint expect=SDL-L0001
namespace acme;
database shop;

// surform-lint-allow: SDL-L0001
table settings {
  field key: string;
};

table orders {
  field id: int;
  permissions: full;
};
```

The `orders` table warns — its statement carries no allow. Codes are
comma-listed on one line (`// surform-lint-allow: SDL-L0001,
SDL-L0006`), and the comment rides its anchor through `fmt` and
`normalize`, so the suppression survives reformatting. The same-line
form after a one-liner's `;`:

```sdl-lint expect=none
namespace acme;
database shop;

table person {
  field name: string;
};

field nickname: string { on: person; }; // surform-lint-allow: SDL-L0007
```

A typo'd code suppresses nothing, so it gets its own warning instead
of silence:

```sdl-lint expect=SDL-L0001,SDL-L0023
namespace acme;
database shop;

// surform-lint-allow: SDL-L0099
table countries {
  permissions: full;
};
```

`SDL-L0099` is not a code — the linter reports `SDL-L0023` at the
comment, and the table's `SDL-L0001` stands (only registered codes
suppress). Misspelled markers (`surform-lint-alow:`) and allow
comments with no statement after them report `SDL-L0023` too. Two
limits are by design: a REGISTERED code that fires nothing on its
statement stays silent (it may be guarding a future change), and
findings the file carries no statement for (an adoptee-only table
under `--info`) have no site to excuse — they never suppress.
`SDL-L0023` itself cannot be allowed away.

On a [composition root](includes.html) the allow stays file-scoped:
it excuses findings in its own file only, even though the other
files' spans overlap numerically. Findings render against the file
they came from.

## Resolved state, not source text

Some warnings only make sense once the database's state is available.
With `--info` tags, lint evaluates the **resolved state** — the
merged, placed, elided file — so a table the database itself holds
wide open warns even though the schema file never spells a
permissions clause: the reader adopts the stored `PERMISSIONS FULL`
into the merged file, and `SDL-L0001` evaluates that:

```sh
surform lint schema.surdl --info db=acme/shop=info-db.txt
```

## Where next

- [Users](users.html#credentials-and-the-re-apply-rule) and
  [access](access.html) — the pages behind the security-adjacent
  warnings.
- [Schema migration](migrate.html) — the other database-aware command.
