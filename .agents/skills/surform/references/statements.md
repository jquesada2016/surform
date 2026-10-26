# The 16 DEFINE statements (+ raw + mixin + type aliases)

Grammar sketches and verified examples, one section per statement, in
a sensible file order. Fences tagged `sdl` are runnable: every one
parses clean under `surform check` exactly as written. Fences tagged
`sdl-grammar` are sketches (placeholders, alternatives) — read them,
don't run them. Clause menus mark `// required:` / `// optional:`;
`<expr>` marks a SurrealQL expression clause (verbatim capture after
`:`), bare keywords mark SDL-lexed menu clauses.

- [namespace](#namespace) · [database](#database) · [table](#table) ·
  [field](#field) · [index](#index) · [event](#event) ·
  [analyzer](#analyzer) · [access](#access) · [user](#user) ·
  [fn](#fn) · [param](#param) · [sequence](#sequence) ·
  [api](#api) · [bucket](#bucket) · [config](#config) ·
  [module](#module) · [raw](#raw) · [mixin](#mixin) ·
  [type aliases](#type-aliases)

## namespace

The file's scope. No clauses. The compiler emits the matching
`USE NS <name>;` after it.

```sdl
/// Acme Corporation
namespace acme;
```

## database

Binds to the nearest preceding `namespace` (emits `USE NS <ns> DB <db>;`).

```sdl
/// Primary application database
database blog {
  strict;                 // undefined tables error instead of implicit-create
  changefeed {
    expiry: 7d;           // required; emits CHANGEFEED 1w — durations
                          //   value-normalize
    include original;     // also store pre-change records
  };
};
```

Gotchas: `strict;` is active on the server but INVISIBLE to `INFO`
round-trips — normalize preserves it, the differ cannot see it drift.
A non-strict database implicitly creates tables on first write.

## table

The kind is the discriminant and rides in the header:

```sdl-grammar
/// <doc comment>
table <name> { … };                          // NORMAL — bare means normal
                                             //   (emits TYPE NORMAL: RELATE fails)
table any <name> { … };                      // ANY — the RELATE escape hatch

table [<from>|…] -> <name> -> [<to>|…] [lightweight] { … };
                                             // relation — RELATE's own order;
                                             //   either side list omittable
table relation <name> { … };                 // unconstrained relation (canonical
                                             //   spelling of `table -> n ->;`)

table view <name> {
  <select-query>;                            // the block IS the query (no clause key)
  // optional trailing clauses only: permissions:|permissions { … };, graphql { … };
};
```

Clauses (non-view tables):

```sdl-grammar
table <name> {
  // optional:
  schemafull;                          // or schemaless; — always emitted in DDL
                                       //   (also legal in the header:
                                       //   `table user schemafull { … };`)
  drop;                                // records written here are not stored
  changefeed { expiry: <duration>; include original; };
                                       //   expiry: is REQUIRED inside the block
  permissions: none | full;            // all-ops shorthand, or:
  permissions {                        // per-op — UNLISTED OPS ARE NONE (deny)
    select: none | full | <expr>;
    create, update: none | full | <expr>;
    delete: none | full | <expr>;      // table-only; fields reject delete
  };
  graphql { alias: <string>; deprecated: <string>; };
  inline edges: <n>;                   // 3.3 inline cache cap, 0–256
  inline references: <n>;              //   (not authorable on views)

  // relation only:
  enforced;                            // endpoints must exist at write time;
                                       //   implied by lightweight

  // nested definitions — fields, indexes, events (omit their `on:`):
  field …;  index …;  event …;
};
```

Relation semantics: the arrows type the endpoints (`record<…>`);
`enforced;` additionally rejects dangling endpoint records. A
LIGHTWEIGHT relation (`lightweight` trailing the header, both sides
named) is record-less — edges are canonical `[in, out]` id pairs,
`RELATE` is idempotent per pair, and `drop;`/`changefeed`/`schemafull;`
/nested definitions are all rejected.

Examples — the workhorse form:

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

Append-only audit trail (drop + changefeed):

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

Relations — pipe endpoint lists, one-sided, unconstrained. The first
form carries `enforced;`; the three minimal forms deliberately omit
it, so `surform lint` flags each SDL-L0006 (dangling endpoints are
allowed without it):

```sdl
table user | org->wrote->post | comment {
  enforced;
  at: datetime {
    default: { time::now() };
  };
};
table follower->watched->;                  // IN-only — OUT unconstrained
table->tagged->post;                        // OUT-only — IN unconstrained
table relation generic_edge;                // no sides at all
```

Lightweight edges (3.3) and inline cache caps:

```sdl
table person->likes->post lightweight;      // enforced is implied
table hot { inline references: 16; };
```

A view (read-only aggregation — the block is the query):

```sdl
/// Posts per author
table view post_stats {
  select author, count() as posts from post group by author;
};
```

Gotchas:
- Bare table → `TYPE NORMAL` → `RELATE` into it errors. This is
  deliberate (fail-closed); write `any` or arrows when you mean it.
- Views always emit `TYPE ANY SCHEMALESS AS SELECT …`; `drop;` /
  `changefeed` on a view are rejected.
- The schema flags fold last-one-wins — `table t schemafull { schemaless; };`
  is SCHEMALESS.
- `delete: full;` works but cannot round-trip through INFO (the
  emitter elides it) — linted SDL-L0014.

## field

Fields nest inside their table (no `on:`) or stand alone with `on:`.
The type is REQUIRED (unless computed):

```sdl-grammar
/// <doc comment>
[field] <name>: <type> [= <expr>] {
  // required when standalone (omit inside a table block):
  on: <table>;

  // optional:
  default: <expr>;                    // applied when no value supplied
  default always: <expr>;             // applied even when one is
  value: <expr>;                      // overrides any supplied value
  assert: <expr>;                     // predicate; $value is the candidate
  computed: <expr>;                   // derived, read-only (block form)
  readonly;                           // flag
  inline;                             // 3.3 — embed into a relation host's payloads
  flexible;                           // type must contain an object
  reference;                          // ON DELETE IGNORE (the default)
  reference {
    on delete: reject | cascade | ignore | unset | <expr>;   // <expr> → ON DELETE THEN
  };
  permissions: none | full;           // shorthand, or:
  permissions {                       // per-op — NO delete; UNLISTED OPS ARE FULL
    select: none | full | <expr>;
    create, update: none | full | <expr>;
  };
  graphql { alias: <string>; deprecated: <string>; };
};
```

Standalone form (with `on:`, doc comment):

```sdl
/// Contact address for the account owner
field email: string {
  on: account;
  assert: string::is_email($value);
  default: 'unknown@example.com';
};
```

Nested forms — subfields, wildcards, unions, inline object types
(inside a table block, the recommended authoring):

```sdl
table product schemafull {
  locked: bool {
    readonly;
    value: true;
  };
  tags: array<string> {
    default always: [];
  };
  meta: object {
    flexible;
  };
  meta.user_id: int;                       // subfields: dotted entries
  tags.*: string;                          //   and wildcard entries
  mixed: int | string;                     // unions, literals, inline objects:
  size: 'small' | 'large' | int;
  inline_obj: { a: string, b: number };
};
```

Computed fields — header assignment (preferred), typeless, block form
with clauses, and `<~` back-reference traversal (lives on the
REFERENCED table's side of the relation):

```sdl
table article schemafull {
  name: string;
  shout: string = string::uppercase(name);   // header assignment
  derived = string::uppercase(name);         // typeless computed
  secret_shout: string {                     // needs clauses → block form
    computed: string::uppercase(name);
    permissions {
      select: none;
    };
  };
  owners = <~person;
};
```

References:

```sdl
table post schemafull {
  author: record<user> {
    reference {
      on delete: cascade;
    };
  };
  parent: record<post> {
    reference {
      on delete: { create cleanup_log set at = time::now() };
    };
  };
};
```

Gotchas:
- `computed:` is mutually exclusive with `default:`/`default always:`/
  `value:`/`assert:`/`reference`/`readonly;`, must be top-level (no
  dotted names), and cannot be `inline;`. A computed field needing
  clauses uses the block form — a trailing block after `=` is rejected.
- The `id` field forbids `value:`, `default always:`, `computed:`,
  `readonly;`, `flexible;`, `reference`; its type must be a
  record-id-key kind.
- Defining a record-typed field literally named `in`/`out` on a
  relation REWRITES that relation's endpoints (linted SDL-L0010).
- `reference;` needs a record-shaped kind (record, array/set of
  record, unions of record|none).

## mixin

SDL-only (no SurrealQL counterpart): a named group of member
definitions, spread into tables at compile time. `mixin` statements
emit no DDL of their own — a spread expands to the members, in place,
at its position in the table block. A mixin body is a table block's
body minus `on:` clauses: fields, indexes, and events (`on:` never
appears; the spreading table becomes the host at expansion). An
explicit member with the same family key — a field's dotted path, an
index/event's name; a field `x` and an index `x` never collide —
wholly replaces that member in that table: position-independent, no
clause merging, per-table. Between two spreads the LATER spread's
member wholly wins (the loser is skipped at expansion); the shadow
lints SDL-L0022.

```sdl
mixin timestamped {
  created_at: datetime { default: time::now(); };
  updated_at: datetime;
  index created_u { fields: created_at; };
  event touched { then: 1; };
};

table post schemafull {
  ...timestamped;
  title: string;
};
```

Rules: top-level only, resolved document-wide (a spread resolves
against every mixin the composition defines — any file, any position;
a name nothing defines is SDL-E1061); spreads
live in non-view table blocks only (`...name;` is a body entry, not a
clause); no `///` on the statement or members; a bare member head
named `index`/`event` needs the `field` prefix or quoting (the keys
are active in this body, the table-block rule); spreading the same
mixin twice is SDL-E1064; the same member key twice within ONE body
(mixin or table) is SDL-E1067; an unspread mixin lints SDL-L0021.

Gotchas: the merge/compare REVERSE path is spread-preserving —
`migrate generate`
expands the SDL side itself and reports a diverged member as drift on
that member (never a mixin-level finding); `normalize` keeps the
spread, covering state-equal members and materializing a diverged one
as the explicit override (the overshadow rule performing the
adoption). `mixin` is not a SurrealQL keyword: fields named `mixin`
parse bare.

## type-aliases

SDL-only: `type <Name> = <kind>;` names a kind once; a bare
**Capitalized** word at any kind position is the reference, expanded
to the alias's kind at compile time (before emission — the database
never sees an alias name, so the feature's meaning does not depend on
the server version). Lowercase words at kind positions are SurrealQL
kinds — the case of the first letter is the whole disambiguation.

```sdl
type Email = string;
type Emails = array<Email>;

table user schemafull {
  name: string;
  contact: Emails;
  recovery: option<Email>;
  both: Email | int;
  profile: { email: Email };
  owner: record<email>;
};

fn contacts($e: Email) -> Emails { return [$e]; };
```

Rules: the name is `[A-Z][A-Za-z0-9_]*` and not a SurrealQL kind head
(ASCII-case-insensitively — `type String = int;` is rejected, `String`
would lose every use site to the `string` head; `NaN`/`Infinity` are
reserved exact-case) — SDL-E1072; references resolve document-wide
(any file, any position, use before define legal), a reference naming
nothing is SDL-E1071 with a did-you-mean; duplicates are SDL-E1062;
an alias cycle is SDL-E0015 with the chain named; `flexible` on an
alias-referencing field decides on the EXPANSION (SDL-E1050 when it
holds no object); no `///` on the statement (it emits nothing).

Gotchas: `record<Email>` / `table<Email>` / `file<Email>` interiors
are table NAMES at every case, never references (the example above
names the table `email`); `raw` bodies are verbatim — never expanded;
fmt round-trips the statement verbatim (the kind in its authored
spelling); the reverse path lands with the merge unit — INFO stores
only concrete kinds, so reads emit expanded text and the reverse
reader's strict grammar accepts no references.

## include

SDL-only (no SurrealQL counterpart): compose another schema file into
this one. The included file's statements take the include's place in
the stream (depth-first, pre-order), and the whole project is defined
to behave exactly like its flattening — same DDL, same lint findings.

```sdl-grammar
include '<path>';     // one string literal, relative to THIS file's
                      //   directory (`..` fine; absolute → SDL-E0012)
```

Layout per file: `namespace`/`database` come before the file's FIRST
`include` (SDL-E1069 — the one order rule left); includes themselves
are legal anywhere in the file; an INCLUDED file
carries no scopes at all (SDL-E1070 — they would re-scope the
importing file below the splice). A file composes once across
diamonds (first include wins; a later declaration by another file is
the SDL-L0026 advisory, excused by `// surform-lint-allow:
SDL-L0026`; the SAME file declaring one path twice is the SDL-E0014
error); a cycle is legal (the graph declares participation, not an
elaboration order — splice-once makes it terminate); a
missing/unreadable file is SDL-E0008, anchored on its include.

```sdl
// The runnable flattening of a composed tree — what the compiler
// behaves as if you had written in one file:
namespace acme;
database shop;

mixin timestamps {
  field created_at: datetime { default: time::now(); };
  field updated_at: datetime { default: time::now(); };
};

table users schemafull {
  ...timestamps;
  field name: string;
};
```

Mixin resolution is document-wide: shared mixins live anywhere in the
tree, the root's body included. Name collisions fire SDL-E1062 for
every definition family — tables and mixins alike, across files as
within one (last-wins overrides are gone; the compiler owns the
emission order, authored order only breaks its ties).

Tools: `check`/`build`/`lint`/`migrate generate` follow includes
(per-file
diagnostic rendering; `surform-lint-allow:` is file-scoped); `fmt`
formats any file as authored — includes survive, print mode never
resolves (so a leaf formats by its own path), while the committal
`fmt --in-place` is load-gated (a spread nothing defines refuses the
write); `normalize
--in-place` writes the whole tree back (each merged statement to its
home file; a scope-statement or dependent preamble-bound adoptee
refuses, SDL-E0013; without `--in-place` the flattened form prints).
Contexts without a file
system — the playground, inline buffers, runnable doc fences — reject
the statement (SDL-E1068); `include` is inert in text no loader reads.

## index

The kind is the header variant slot:

```sdl-grammar
/// <doc comment>
index [unique | count | fulltext | hnsw | diskann] <name> {
  // required when standalone (omit inside a table block):
  on: <table>;

  fields: <expr>, …;              // required (exactly ONE field for
                                  //   fulltext/hnsw/diskann; FORBIDDEN for count)
  concurrently;                   // build in background; never persisted

  // count:
  where: <expr>;                  // filtered count

  // fulltext:
  analyzer: <ident>;              // default 'like' — define the analyzer FIRST
  bm25: (<k1>, <b>);              // omit for the (1.2, 0.75) default
  highlights;

  // hnsw / diskann:
  dimension: <number>;            // required (u16)
  distance: <distance>;           // euclidean|cosine|cosine_normalized|chebyshev|
                                  //   hamming|jaccard|pearson|inner_product|
                                  //   manhattan|minkowski(<order>)
  type: <vector-type>;            // f64|f32|f16|i64|i32|i16|i8|u8
  m: <number>; m0: <number>; lm: <number>; efc: <number>;       // hnsw
  extend_candidates; keep_pruned_connections;                   // hnsw flags
  hashed_vector;          // hnsw AND diskann
  degree: <number>; l_build: <number>; alpha: <number>;         // diskann
};
```

Examples:

```sdl
/// Composite lookup for inbox queries
index by_account_thread {
  on: message;
  fields: account, thread_id;
};
index unique one_email {
  on: user;
  fields: email;
  concurrently;
};
index count active_count {
  on: item;
  where: status = 'active';
};
```

Fulltext and vectors:

```sdl
index fulltext title_fts {
  on: post;
  fields: title;
  analyzer: english;
  bm25: (1.2, 0.75);
  highlights;
};
index hnsw vec {
  on: embedding;
  fields: vector;
  dimension: 768;
  distance: cosine;
};
```

Gotchas:
- `fields:` is exactly one field for fulltext/hnsw/diskann and
  forbidden for count (the parser enforces all three).
- DISKANN restricts distance to `euclidean | cosine | inner_product |
  cosine_normalized` and type to `f32 | f16 | i8 | u8` (with
  `cosine_normalized` ⇒ `f32 | f16`); HNSW accepts all ten distances
  and all eight types.
- `distance: minkowski(3);` — SDL spells the order in parens; the DDL
  emits bare `MINKOWSKI 3`.

## event

```sdl-grammar
/// <doc comment>
event <name> {
  // required when standalone (omit inside a table block):
  on: <table>;

  then: <expr>;                   // ≥ 1 required — a block or a comma list
  when: <expr>;                   // omitted → WHEN true (materializes, elided again)
  async;                          // run outside the transaction (retry 1, maxdepth 3)
  async {
    retry: <number>;              // 0 disables retries
    maxdepth: <number>;
  };
};
```

Event bodies see `$event` (`'CREATE'|'UPDATE'|'DELETE'`), `$before`,
`$after`, `$value`, `$input`.

```sdl
/// Log every user creation
event user_created {
  on: user;
  when: $event = 'CREATE';
  then: { create log set at = time::now(), user = $after.id; };
};
event double_bookkeeping {
  on: payment;
  when: $event = 'UPDATE' and $before.status != $after.status;
  then: (create ledger_entry set payment = $after.id),
        (update stats:payments set changes = changes + 1);
};
```

Gotchas: `RETRY`/`MAXDEPTH` only ever appear under `async` — SDL
groups them in the flag+block pattern; the canonical writer collapses
a block equal to the 1/3 defaults back to bare `async;`.

## analyzer

Full-text analysis for `index fulltext`. All clauses optional (an
empty analyzer is inert), lists comma-separated and order-significant:

```sdl-grammar
/// <doc comment>
analyzer <name> {
  function: fn::<seg>[::<seg>…];   // preprocessing fn — a NAME PATH, no call parens
  tokenizers: <tokenizer>, …;      // blank | camel | class | punct
  filters: <filter>, …;            // ascii | lowercase | uppercase
                                   // | edgengram(<min>, <max>) | ngram(<min>, <max>)
                                   // | snowball(<language>) | mapper('<path>')
};
```

```sdl
/// English text search
analyzer english {
  tokenizers: class;
  filters: snowball(english), lowercase;
};
analyzer autocomplete {
  filters: lowercase, edgengram(2, 10);
};
```

Gotchas: `snowball('english')` (quoted) is rejected — the language is
a bare keyword; 18 canonical languages with 2/3-letter aliases accepted,
canonical output normalizes to full names (`snowball(en)` →
`snowball(english)`). Filter order is preserved and matters. List
order is intra-analyzer only: analyzer-vs-index placement is free —
`build` orders the analyzer before the fulltext index that names it,
whatever the file's order.

## access

Three variants — `type:` is the discriminant. Shared clauses:
`duration { grant:; token:; session:; };`, `authenticate: <expr>;`
(post-auth check; THROW rejects), `context: <expr>;` (3.3 — the
`$session.data` payload; record-authenticating accesses only).

```sdl-grammar
/// <doc comment>
access <name> {
  // required:
  on: root | namespace | database;        // aliases kv/ns/db accepted
  type: jwt | record | bearer;

  // jwt — verification source (url: XOR algorithm:+key:):
  url: <expr>;                            // JWKS endpoint
  algorithm: eddsa | es256 | es384 | es512 | hs256 | hs384 | hs512
           | ps256 | ps384 | ps512 | rs256 | rs384 | rs512;
  key: <expr>;                            // symmetric (hs*) + all issuer keys:
                                          //   ONLY '[REDACTED]' is authorable
  issuer {
    algorithm?: <algorithm>;
    key: <expr>;
  };

  // record — database level only:
  signup: <expr>;                         // omit the trailing `;` — return a value
  signin: <expr>;
  jwt { url: … | algorithm:+key: …; issuer { … }; };   // issues its own tokens
  refresh;                                // WITH REFRESH

  // bearer:
  for: user | record;                     // record subject: database level only
  jwt { … };                              // bearer-issued JWTs

  audience: '<value>', …;                 // 3.3 — string literals; on the jwt
                                          //   source or inside jwt { … }
};
```

Examples — the record workhorse:

```sdl
access account {
  on: database;
  type: record;
  signup: {
    create user set
      email = $email,
      password = crypto::argon2::generate($password)
  };
  signin: {
    select * from user
      where email = $email
      and crypto::argon2::compare(password, $password)
  };
};
```

JWT verification and issuance (the `'[REDACTED]'` blocks below are
the canonical spelling, but `surform lint` flags them SDL-L0013 —
applying the file would define the literal as key material; provision
real keys outside SDL):

```sdl
/// Tokens issued by Acme SSO
access corp_sso {
  on: database;
  type: jwt;
  url: 'https://sso.acme.com/.well-known/jwks.json';
};

access partner_api {
  on: database;
  type: jwt;
  algorithm: es256;
  key: 'partners_public_key';             // asymmetric verify key: public, fine
  issuer {
    key: '[REDACTED]';                    // issuer key: sentinel only
  };
  duration { token: 1h; };
};

access device_keys {
  on: database;
  type: bearer;
  for: record;
  jwt {
    algorithm: hs256;
    key: '[REDACTED]';
  };
  duration { token: 365d; };              // emits DURATION FOR TOKEN 1y
};
```

Gotchas:
- Signup/signin captures are SurrealQL blocks — the last statement is
  the result; a trailing `;` inside returns nothing.
- INFO redacts symmetric verify keys and all issuer keys; the sentinel
  `'[REDACTED]'` is the canonical spelling there (SDL-E1056 enforces
  it; SDL-L0013 flags applying it as real material).
- A verify-only access (no issuer, no grant path) cannot issue tokens
  — a `duration { token: …; }` there silently disappears on
  round-trips (linted SDL-L0011).

## user

```sdl-grammar
/// <doc comment>
user <name> {
  // required:
  on: root | namespace | database;

  // optional — at most ONE of:
  password: <string>;               // plaintext — linted SDL-L0002 (this is a
                                    //   user credential clause; a table FIELD
                                    //   named password is not flagged)
  passhash: <string>;               // pre-computed argon2 hash — preferred

  passscram: <verifier>;            // 3.3 SCRAM-SHA-256 verifier — ADDITIVE
                                    //   with the pass clauses; parse-validated
  roles: owner | editor | viewer, …;   // default viewer
  duration {
    token: <expr>;                  // default 1h
    session: <expr>;                // default none
  };
};
```

```sdl
/// CI deployment account
user deployer {
  on: namespace;
  passhash: '$argon2id$v=19$…';
  roles: editor, viewer;
  duration {
    token: 1h;
    session: 12h;
  };
};
```

Gotchas:
- A user with NO credential is define-once: codegen emits
  `IF NOT EXISTS`, and `OVERWRITE` re-apply of a credential-less user
  would brick the account (linted SDL-L0003).
- `PASSWORD`+`PASSHASH` together is a parse error. INFO shows
  `PASSHASH '[REDACTED]'` — never echo it back; keep the SDL value.

## fn

The one two-block statement: first block is the SurrealQL body
(captured brace-balanced), optional second block holds SDL clauses.
The name emits with the `fn::` prefix; params keep their `$`.

```sdl-grammar
/// <doc comment>
fn <name>($<param>: <type>, …) [-> <type>] {
  <surrealql statements>          // last statement sans `;` is the return value
} {
  // optional:
  permissions: none | full | <expr>;    // default full
  graphql { alias: <string>; deprecated: <string>; };
};
```

```sdl
/// Whether the current user may edit this post
fn can_edit($p: record<post>) -> bool {
  return $auth.id = $p.author or $auth.role = 'admin';
} {
  permissions: $auth.admin = true;
};

fn tax_total($net: number, $rate: option<number>) {
  let $r = $rate ?? 0.15;
  let $tax = math::round($net * $r, 2);
  $net + $tax                                   // no trailing `;` — the result
};

fn util::slugify($s: string) -> string {
  string::lowercase(string::replace($s, ' ', '-'))
};
```

Gotchas: zero-param functions are legal (`fn ping() -> string { 'pong' };`).
SDL rejects trailing commas in the param list (SurrealQL accepts them).

## param

Define-time constants — the value expression is evaluated ONCE at
define and the RESULT is stored (normalize replaces the expression
with the frozen literal).

```sdl
/// Default page size for list endpoints
param $default_limit = 50;              // short form

param $internal_seed {                  // block form — any other clause forces it
  value: 42;
  permissions: none;
};

param $endpoints = { read: '/api/read', write: '/api/write' };
                                        // `=` then `{` is an OBJECT literal,
                                        //   not a block opener
```

Gotchas: a param without `value:` (stores `VALUE NONE`) is almost
certainly unintended — linted SDL-L0008. No type clause exists.

## sequence

Distributed monotonic numbers, consumed via
`sequence::nextval('<name>')`. No COMMENT clause — `///` is rejected.

```sdl
sequence invoice_number = 1000;         // short form — start value only

sequence order_number {
  start: 5000;                          // default 0
  batch: 100;                           // values reserved per node; default 1000
  timeout: 5s;                          // per-query acquisition limit
};
```

Gotchas: SurrealQL's own clause order is fixed (`BATCH` before
`START` before `TIMEOUT`) — SDL's block is free-ordered and codegen
emits the canonical order, so author freely.

## api

The name is a route path (single string literal): static segments,
`:name` / `:name<type>` dynamic segments, `*name` wildcard tail.
Method groups + at most one `any` fallback block:

```sdl-grammar
/// <doc comment>
api <path-string> {
  any {                                  // endpoint-wide config + fallback (≤ 1)
    permissions: none | full | <expr>;
    middleware: <fn-path>(<args>), …;    // parens mandatory; args are SurrealQL,
    then: <expr>;                        //   evaluated once at define
  };
  get, post, put, patch, delete, trace { // each method in ≤ one group
    permissions: none | full | <expr>;
    middleware: <call>, …;
    then: <expr>;                        // required on method groups
  };
};
```

```sdl
/// Health check
api '/health' {
  get {
    then: { { status: 200, body: { ok: true } } };
  };
};

api '/admin/*rest' {
  any {
    middleware: api::timeout(5s);
    then: { { status: 404, body: { error: 'no such route' } } };
  };
  get, delete {
    permissions: $auth.admin = true;
    then: { { status: 200 } };
  };
};
```

(The `/health` endpoint trips SDL-L0009 by design: without an
effective `any` block, the engine's IMPLICIT catch-all answers every
unmatched method under FULL permissions and runs nothing. An empty
`any { };` does not help — the linter calls that a no-op too. Give
the `any` block a `then:` fallback, middleware, or a real
`permissions:` guard.)

Gotchas: `then:` bodies conventionally wrap a response object in
SurrealQL's block braces — `then: { { status: 200, … } };` (outer
braces = block, inner = object). `$request` is available inside with
`body`/`headers`/`params`/`method`/`query`/`context`/`request_id`.
An empty `any { };` is a lint (SDL-L0009).

## bucket

File storage (experimental `files` capability at define time — the
server rejects it otherwise). Permissions expression sees `$action`,
`$file`, `$target`.

```sdl
/// Ephemeral test uploads
bucket scratch_uploads {
  backend: 'memory';              // or 'file:/srv/uploads' (runtime allowlist)
  readonly;
  permissions: $auth.admin = true;
};
```

## config

Per-variant singletons (always emit `OVERWRITE`). No COMMENT clause —
`///` is rejected. `config default` executes at ROOT level; `api` /
`graphql` at database level.

```sdl
config api {
  middleware: api::timeout(5s), api::log();
  permissions: $auth.admin = true;
};

config graphql {
  tables { include: user, post; };
  functions { exclude: fn::legacy_report; };
  depth: 4;
  complexity: 100;
  introspection: none;            // default auto
};

config default {
  namespace: 'acme';              // ≥ one of the two required
  database: 'blog';
};
```

(A bare `auto;` — or `none;` — sets both families at once; later
`tables:`/`functions:` clauses fold last-one-wins per family.)

Gotcha: unspecified GraphQL families default to NONE — expose nothing
until you say otherwise. `include:` and `exclude:` together in ONE
sub-block is a parse error (exactly one of the two).

## module

Surrealism modules (experimental capability at define). Exactly one
executable; `unsigned;` is required.

```sdl
/// Colour helpers
module color {
  from: f'modules:/color.surli';        // file module — name REQUIRED
  unsigned;
};

module {                                // silo package — self-named
  from: silo::acme::utils::<1.2.0>;
  unsigned;
};
```

## raw

The escape hatch — verbatim SurrealQL DDL for anything SDL cannot
express. Top-level ONLY (`raw` is reserved inside blocks; a field
named `raw` takes the `field` prefix). The interior is carried
byte-for-byte (comments and spelling included, edges trimmed);
validation is advisory — a body the parser rejects is still emitted
and linted (SDL-L0012). Takes no doc comment.

```sdl
raw {
  DEFINE FIELD legacy_untyped ON events;
};
```

(A typeless field is legal SurrealQL but outside SDL's grammar, which
requires a type — `raw` is exactly for this class.)

Gotcha: everything `raw` says is a promise the compiler cannot check —
prefer a real statement whenever SDL can express the shape.
