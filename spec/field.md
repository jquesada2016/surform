# DEFINE FIELD

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_field`,
`parse_reference` in `stmt/parts.rs`), executor validation
(`expr/statements/define/field.rs` — computed/id restrictions), and
emitter (`sql/statements/define/field.rs`). Cross-checked against a
local in-memory 3.2.3 server (2026-09-18); the floor move re-runs the corpus on the 3.3.0 embed (2026-09-25). Spec correction
(2026-09-22): the email-assert example spelled the function
`string::is::email` — the 3.2.4 dispatch table registers
`string::is_email` (`core/src/fnc/mod.rs`; the `is::` spelling is a
legacy display form the parser rejects), and the example now uses it.

**Surface decisions (settled):**

- The type sits in the header — `name: type` — and is **SDL-lexed, not
  expression-captured** (README rule 4: the SDL type parser consumes
  the kind grammar, including brace-bearing inline object types that
  end at their balanced `}`; the header then ends at `=`, `;`, or the
  clause block `{`). The type is **required** for regular fields — a
  licensed deviation from SurrealQL's typeless form (reachable via
  `raw`), because `name: {` would collide with expression capture —
  and **optional exactly when a computed expression is present**
  (`name = <expr>;` typeless computed is SurrealQL-legal, verified).
- The `field` prefix is optional; it is REQUIRED to escape a field
  name colliding with an active clause key or block keyword (`index`,
  `event`, `permissions`, `from`, `as`, `enforced`, `drop`, …).
- **No nested-block sugar for subfields** — a subfield entry and a
  clause key would be indistinguishable inside one block. Nested
  fields are separate entries with dotted names (`stats.views: int;`),
  matching SurrealQL's flat path model (`.` nesting, `.*` array
  wildcard).
- Standalone fields carry `on: <table>;`; nested (inside a `table`
  block) they omit it.

```rust
struct Field {
  name: FieldPath,                    // 'stats.views', 'tags.*', …
  type: Option<Kind>,                 // required unless computed is present
  on: TableRef,                       // standalone only
  default: Option<Default>,           // Set(expr) | Always(expr)
  value: Option<Expr>,
  assert: Option<Expr>,
  computed: Option<Expr>,             // derived, read-only
  readonly: bool,
  flexible: bool,                     // type must contain an object
  inline: bool,                       // embeds the value into a relation
                                      //   host's edge-adjacency payloads
                                      //   (3.3); codegen glues INLINE
                                      //   after READONLY
  reference: Option<Reference>,       // { on_delete }, default Ignore
  permissions: Permission,            // select/create/update; single-value
                                      //   none|full shorthand also valid
  graphql: Option<Graphql>,           // alias?, deprecated?
  comment: Option<DocComment>,        // from ///
}
```

**Computed validation (executor-enforced, verified from source):**
`computed` is mutually exclusive with `default:`, `default always:`,
`value:`, `assert:`, `reference`, and `readonly;`; computed fields must
be **top-level** (no dotted names); no subfields may nest under a
computed parent.

**The `id` field (executor-enforced):** `type:`, `assert:`, and plain
`default:` are allowed; `value:`, `default always:`, `computed:`,
`readonly`, `flexible`, and `reference` are forbidden, and the type
must be a record-id-key-compatible kind.

```
/// <doc comment>                        // optional — emitted as COMMENT
[field] <name>: <type> [= <expr>] {
  // required when standalone (omit inside a table block):
  on: <table>;

  // optional:
  default: <expr>;                       // applied when no value is supplied
  default always: <expr>;                // applied even when one is
  value: <expr>;                         // overrides any supplied value
  assert: <expr>;                        // validation predicate; THROW
                                          //   customizes the error
  computed: <expr>;                      // block form of the header
                                          //   assignment — use when other
                                          //   clauses are present
  readonly;                              // flag
  flexible;                              // flag — type must contain an
                                          //   object; codegen glues FLEXIBLE
                                          //   after TYPE
  inline;                                // flag — 3.3; the INLINE
                                          //   contract below
  reference;                             // flag — ON DELETE IGNORE (default)
  reference {
    on delete: reject | cascade | ignore | unset | <expr>;
  };                                     // non-enum value → ON DELETE THEN
  permissions: none | full;              // all-ops shorthand, or:
  permissions {                          // per-op; NO delete — SurrealQL
    select: none | full | <expr>;        //   rejects it; unlisted ops are
    create, update: none | full | <expr>; //  FULL for fields (verified —
  };                                     //   the opposite of tables)
  graphql {
    alias: <string>;                     // GRAPHQL_ALIAS
    deprecated: <string>;                // GRAPHQL_DEPRECATED
  };
};
```

**Type syntax passes through verbatim** (illustrative, not exhaustive —
`sql/kind.rs` is the authority): `string`, `int`, `number`,
`datetime`, `bool`, `uuid`, `float`, `duration`, `option<T>`,
`array<T>` and fixed-size `array<T, N>`, `set<T>`, `record<T, …>`,
`geometry<T>`, unions (`int | string`), literals (`'small' | 'large' |
int`), inline object schemas (`{ a: string, b: number }`), and the
remaining SurrealQL kinds (`any`, `null`, `bytes`, `decimal`, `range`,
`regex`, `function`, `table<T>`, `file<T>`, literal kinds). Kind
equality and round-trip folding follow the README rule-4
normalization (`option<T>` ≡ `none | T`).

**Computed forms:** header assignment (preferred) —
`shout: string = string::uppercase(name);` — or typeless `shout =
string::uppercase(name);`. As with `param`, a trailing block after `=`
is rejected (expression capture ambiguity), so a computed field
needing other clauses uses the block form with `computed:`.

**References and `<~` back-traversal:** reference DDL is a clause
group (not a header operator) — it is behavior config, unlike the
table `->` which encodes shape. The **`<~` idiom** (documented since
v2.2.0, verified live on 3.2.3) traverses *incoming* references: in
queries (`select <~person as owners from comic_book;`) and inside
expressions — SDL supports it verbatim for free, including as a
computed field's header assignment (`owners = <~person;`, example
below). The field-narrowing form `<~(person FIELD comics …)` appears
in current docs but does not parse on 3.2.x (docs-ahead; re-verify on
upgrade). `~>` exists only in the GQL query layer (directed edge
patterns), not SurrealQL. Reference constraints: top-level
record/array-of-record fields only — `reference` on a nested path is
rejected at define time (verified: `Specify a referencing field at
the root level instead`); `on delete:` expressions see `$this` and
`$reference` (documented).

Emit (canonical order; `OVERWRITE` implied): `DEFINE FIELD <name> ON
<table> [TYPE <type> [FLEXIBLE]] [READONLY] [INLINE] [VALUE …] [ASSERT …]
[DEFAULT [ALWAYS] …] [COMPUTED …] [REFERENCE [ON DELETE …]]
[PERMISSIONS …] [GRAPHQL_ALIAS …] [GRAPHQL_DEPRECATED …] [COMMENT …];`

**The INLINE contract (3.3):** `inline;` embeds the field's value into
the host table's edge-adjacency payloads, so filtered traversals
(`->likes WHERE score > 5`) evaluate against the payloads without
fetching edge records. SDL spells it as a block flag next to
`readonly;` (the engine's clause-loop flag with no kind affinity —
the `flexible` precedent: codegen glues INLINE after READONLY,
upstream's fixed relative order). The engine's define-time
validations, mirrored where SDL sees them (rule 10):

- The host must be an **existing `TYPE RELATION` table** — SCHEMALESS
  and SCHEMAFULL both fine, LIGHTWEIGHT unreachable (its tables carry
  no fields at all, spec/table.md). A nested field's host kind is
  known at the table fold → `SDL-E1044`; a standalone field's host is
  cross-statement → the linter's `SDL-L0019` (order-blind — it reads
  the whole file). The *order* half of "existing" is apply-time: the
  engine demands the table already exist as a relation when the field
  defines, so a standalone field authored before its table statement
  fails there — the accepted exec class (the index-before-field
  precedent; `SDL-L0007` nudges nesting, which orders correctly).
  Normalize's twin fold orders the folded twin (nested defines after
  the table); a divergent standalone keeps its authored position and
  is the differ's drift to report.
- The names `id`, `in`, `out` cannot be INLINE (the adjacency keys
  themselves) → `SDL-E1058`; `id` rides the id-field grammar's
  forbidden list (`SDL-E1052`).
- Only **top-level** fields — dotted and `.*` paths cannot embed →
  `SDL-E1058`.
- **Computed fields cannot be INLINE** ("its value is derived at read
  time, so there is nothing stored to embed") — the computed
  exclusivity set → `SDL-E1049`.
- A table carries **at most 255** INLINE fields (the payload's value
  count is a single byte) → the linter's `SDL-L0020` (a table
  aggregate no single statement sees).
- **No kind constraint** — every kind embeds (probe-pinned
  2026-09-25: `number`, `option<string>`, `array<int>`, `record<>`,
  `object`, `geometry<>`, inline-object schemas, unions, even `any`
  and `null`); `flexible` + `inline` coexist on a SCHEMAFULL host.
  Typeless + INLINE is legal upstream but unreachable in SDL (the
  type requirement plus the computed exclusivity).

## Reverse parser (INFO → SDL)

Verified materializations and their canonical handling:

- `PERMISSIONS` renders **unconditionally** (default FULL). Elide when
  all operations are FULL; INFO groups equal-valued ops
  (`FOR select, create FULL`) — split the groups on re-parse.
- `REFERENCE` always renders `ON DELETE IGNORE` (never bare) — map to
  the `reference;` flag.
- `INLINE` prints between `READONLY` and `VALUE` (the printer's fixed
  relative order) — map to the `inline;` flag. Hand-fed shapes the
  engine's define-time checks never store (computed + INLINE, the
  `id`/`in`/`out` names + INLINE, a dotted path + INLINE) trip the
  shape validation loud; the host-relation half stays define-order
  (the engine's to say).
- Computed fields round-trip typeless when SDL was typeless (`type` is
  `Option` in the SDL AST — both forms stable).
- **Types normalize** per README rule 4: INFO returns desugared kinds
  (`none | T` for `option<T>`, flattened/deduped unions); compare by
  parsed kind AST and fold `none | T` back to `option<T>` in canonical
  SDL.
- **Server-synthesized subfields:** for every field whose type has an
  inner kind (`array<T>`, `set<T>`, unions containing them — not bare
  `array`/`set`), the server materializes a stored `name.*` subfield
  with the inner kind (and inherited `flexible;`), recursing until the
  next inner kind would be exactly `any` — `matrix: array<array<int>>`
  synthesizes both `matrix.*: array<int>` and `matrix.*.*: int`; a
  union's inner kind is the union of its arms' inner kinds, dropping
  arms without one. The reverse parser reads them as the ordinary
  fields they print as; the **normalizer** folds them back
  deepest-first per README item 4: union normalization applies to both
  sides of the comparison, and inherited `flexible` never blocks
  folding. Keep user-meaningful `.*` entries — a bare `tags: array;`
  plus an authored `tags.*: int;` is the legitimate explicit pattern,
  since bare `array`'s inner kind is `any` and nothing is synthesized.
- The reference-type predicate is looser than "record or
  array-of-record" — unions whose **every** arm is record-or-`none`
  (incl. what `option<T>` desugars to), `set<T>`, and literal arrays
  also pass (server-enforced at define time; `record<user> | string`
  is rejected).
- A field with no TYPE and no COMPUTED (`DEFINE FIELD x ON t;` —
  legal hand-run SurrealQL the printer reprints typeless) is outside
  SDL's grammar (which demands a type on every non-computed field)
  and routes the statement to verbatim `raw`.
- On relation tables, defining a field literally named `in`/`out` with
  a record type **rewrites the table's relation endpoints** — a later
  table-level diff reflects it; the linter warns on such fields
  (SDL-L0010).
- Doc comments map back to `///`; kind prefixes strip; canonical
  placement nests fields into their table block (`INFO FOR TABLE`).
- The emitted clause order above is SDL's canonical choice; the
  server's ToSql order differs slightly (COMMENT/GRAPHQL near the end)
  — both parse; the differ compares resolved state (kinds under the algebra, expressions over canonical text).

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): dotted and wildcard paths ✅;
`FLEXIBLE` after `TYPE` with `FLEXI` alias ✅; flexible on non-object →
parse error ✅; flexible before `TYPE` → parse error ✅; inline object
/ union / literal types ✅; `COMPUTED` referencing a sibling field ✅;
typeless computed ✅; `DEFAULT ALWAYS` ✅; `REFERENCE` bare / `ON
DELETE CASCADE` / `ON DELETE THEN (expr)` ✅; `GRAPHQL_ALIAS` /
`GRAPHQL_DEPRECATED` ✅; per-op permissions ✅ and `FOR delete` →
`Can't define permission DELETE for fields` ✅; single-value
`PERMISSIONS FULL` on fields ✅; `id` rejects `VALUE` at define time ✅;
`owners = <~person;` computed + query traversal ✅; INLINE (3.3,
2026-09-25): every kind embeds incl. `any`/`null`/`geometry<>` ✅,
`FLEXIBLE`+`INLINE` on a SCHEMAFULL relation ✅, SCHEMALESS relation
host ✅, non-relation host rejected at define ✅, `in`/`out`/`id` +
INLINE rejected ✅, dotted/`.*` paths rejected ("only a top-level
field can be INLINE") ✅, computed + INLINE rejected ✅, the 255-field
cap (256th define rejected) ✅, INFO prints the flag between READONLY
and VALUE ✅.

## Examples (one per clause)

type + default + assert + doc comment (standalone form):

```sdl
/// Contact address for the account owner
field email: string {
  on: account;
  assert: string::is_email($value);
  default: 'unknown@example.com';
};
```

```surql
DEFINE FIELD email ON account TYPE string
  ASSERT string::is_email($value) DEFAULT 'unknown@example.com'
  COMMENT 'Contact address for the account owner';
```

value + readonly:

```sdl-body
locked: bool {
  readonly;
  value: true;
};
```

computed — header assignment (typed and typeless), and block form with
clauses:

```sdl-body
shout: string = string::uppercase(name);
derived = string::uppercase(name);
secret_shout: string {
  computed: string::uppercase(name);
  permissions {
    select: none;
  };
};
```

```surql
DEFINE FIELD shout ON <table> TYPE string COMPUTED string::uppercase(name);
DEFINE FIELD derived ON <table> COMPUTED string::uppercase(name);
DEFINE FIELD secret_shout ON <table> TYPE string
  COMPUTED string::uppercase(name) PERMISSIONS FOR select NONE;
```

back-reference computed field via `<~` (lives on the *referenced*
table):

```sdl-body
// on person:
comics: option<array<record<comic_book>>> {
  reference;
};
// on comic_book:
owners = <~person;
```

```surql
DEFINE FIELD comics ON person TYPE option<array<record<comic_book>>> REFERENCE;
DEFINE FIELD owners ON comic_book COMPUTED <~person;
```

default always:

```sdl-body
tags: array<string> {
  default always: [];
};
```

flexible:

```sdl-body
meta: object {
  flexible;
};
meta.user_id: int;
```

reference — flag, strategy, and custom expression:

```sdl-body
author: record<user> {
  reference {
    on delete: cascade;
  };
};
owner: record<user> {
  reference;                          // ON DELETE IGNORE (the default)
};
parent: record<page> {
  reference {
    on delete: { create cleanup_log set at = time::now() };
  };
};
```

permissions (single-value shorthand and per-op block — no delete):

```sdl-body
secret: string {
  permissions: none;
};
audited: string {
  permissions {
    select: none;
    create, update: $auth.admin = true;
  };
};
```

```surql
DEFINE FIELD secret ON <table> TYPE string PERMISSIONS NONE;
DEFINE FIELD audited ON <table> TYPE string
  PERMISSIONS FOR select NONE, FOR create, update WHERE $auth.admin = true;
```

graphql (alias + deprecation note):

```sdl-body
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
```

nested and wildcard paths:

```sdl-body
stats: object;
stats.views: int {
  default: 0;
};
tags: array<string>;
tags.*: string;
```

union, literal, and inline-object types:

```sdl-body
mixed: int | string;
size: 'small' | 'large' | int;
inline_obj: { a: string, b: number };
```
