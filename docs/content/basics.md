Everything Surform knows is in a plain text file. This page explains
the file model — what goes where, what order means, and what applying
does.

## The shape of a file

A schema file is a list of definitions, top to bottom, each ending
with `;`. Nearly every file starts by naming its namespace and
database, then defines everything that lives there:

```sdl
namespace acme;
database shop;

sequence invoice_number = 1000;
param $default_limit = 50;
```

After each `namespace`/`database` the compiler emits a `USE` line (a
database with no preceding namespace omits the `USE` — no
error) — so one file can safely cross boundaries if it needs to. The
recommended layout is simple, though: **one namespace and one database
per file**.

## Names in the header, options in the block

Definitions read like sentences: the statement keyword, a name, then
— if the statement has options — a block in braces. What is *true* of
the thing goes in the header when it is part of its identity, and in
the block when it is a setting:

```sdl
namespace acme;
database shop;

table pages {
  schemafull;
  field slug: string;
  index unique slug_unique {
    fields: slug;
  };
};
```

Some statements put a *variant* in the header too — `index unique …`,
`index fulltext …`, `table owner->post->viewer` — because the
variant changes what the block is allowed to contain. The per-statement
pages cover their variants; the pattern is always the same.

## Who owns the order

Definitions may stand anywhere: above, below, or (through
[include](includes.html)) in another file of the composition. `surform
build` does not follow your file's order. It emits the DDL in its own
stable dependency order — a view after the tables it reads, a fulltext
index after its analyzer and fields, a param after the params its
value reads — and breaks ties by the order you wrote things in. A
definition standing "too early" reorders silently; the emitted script
always applies. Within a scope section that guarantee is
unconditional; across a `namespace`/`database` boundary the compiler
refuses the document (SDL-E0016) rather than emit DDL whose apply can
fail. References the engine only evaluates at runtime — `fn` bodies,
permissions, events, field defaults — carry no ordering constraint at
all.

A view may therefore stand before the table it reads:

```sdl
namespace acme;
database shop;

/// Posts per author.
table view post_stats {
  select author, count() as posts from post group by author;
  permissions: full;
};

table post {
  schemafull;
  field author: string;
};
```

The one thing no order can satisfy is a **cycle** — a view chain that
reads itself, a param chain that feeds itself. That is a hard error
(SDL-E0015), and the message names the chain. Within a file, the
top-to-bottom reading order remains how a file is *written* — children
nested in their table blocks (as every page here does), helpers before
their users — and [normalize](normalize.html) nests adopted children
automatically; but it is presentation, never semantics. What is not
presentation is uniqueness: the same name twice — any family, any
file, any position — is a duplicate error (SDL-E1062), not a
replacement.

## Comments: `//` vs `///`

Two comment styles with different jobs:

```sdl
namespace acme;
database shop;

// A plain comment. For humans reading the file.
// The compiler drops it — the database never sees it.

/// A doc comment. It becomes the definition's COMMENT in
/// SurrealDB.
table customer {
  schemafull;
  field vip: bool;
};
```

Watch the DDL panel: the `//` line vanished, the `///` block turned
into `COMMENT '…'`. A `///` block binds to the next definition below
it, however many blank lines intervene; only a second `///` block
steals the binding (the last one before the definition wins). Two
statements have no COMMENT clause in SurrealQL — `sequence` and
`config` — and Surform rejects `///` there rather than silently
dropping your words. A plain `//`
comment survives [fmt](fmt.html) wherever you put it — above a
statement, above any clause (at any nesting depth), or on the same
line as a clause: `then: select …; // why this query`.

## Names and keywords

- Keywords are **lowercase**: `table`, `field`, `index`, `schemafull`.
  (The compiler uppercases keywords in the emitted DDL — that is
  SurrealQL's dress code, not yours.)
- Identifiers are ordinary words: `user`, `invoice_number`, `$limit`.
- If a name collides with a keyword, wrap it in backticks:
  `` table `index` { … }; `` — SurrealQL's own escaping rule.
- SurrealQL keyword aliases are accepted and remembered the way you
  wrote them: `ns` for `namespace`, `db` for `database`, `kv` for
  `root`. The emitted DDL always uses the canonical word.

## Clauses: value, flag, block

Inside a block there are three clause shapes:

```sdl
namespace acme;
database shop;

table reviews {
  schemafull;                          // a flag: on or off
  changefeed {
    expiry: 1d;                        // a value clause: key: value;
  };
  permissions {
    select: full;                      // a sub-block with per-key values
    create: none;
  };
};
```

That is the whole grammar, with two wrinkles: multi-word keys
(`default always:`, `include original;`), and a few statements that
take an expression straight after a name (`param $limit = 50;`). The
per-statement pages show each one.

## The apply model: convergence, not migration

Applying a built file is **safe to repeat**. Every emitted `DEFINE`
carries `OVERWRITE`, with two define-once exceptions: a user declared
without a secret (redefining would wipe the stored credential) and a
sequence (the redefining form restarts the counter). So the database
converges to the file instead of erroring on the second run — except a
changed define-once clause, which the emission leaves alone: a
sequence's start only seeds a fresh database (repositioning a live one
is a manual step — see [Params and sequences](params.html)), and a
secret-less user's roles, durations, and comment need an in-place
`ALTER USER`. What convergence does *not* do is
delete: definitions that exist in the database but not in your file are
left alone.

That gap is a feature with a tool attached:

- `surform migrate generate` compares the file with the database's
  `INFO` output and prints every difference — including definitions
  that exist only in the database, each one a candidate for removal.
- It also prints the ready-to-run **convergence script**: the
  SDL-only and drifted definitions as `DEFINE … OVERWRITE` (the
  define-once families as `IF NOT EXISTS`), then the removals — one
  transaction per scope section, converging the database to the file.

The full walkthrough — including a live drift demo — is on
[Schema migration](migrate.html).

## Where next

The language pages now pick up statement by statement, starting with
[Tables](tables.html). If you prefer to see the whole toolchain first,
[The surform CLI](cli.html) shows every command.
