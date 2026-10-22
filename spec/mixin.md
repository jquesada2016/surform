# MIXIN

Global conventions and notation: [spec/README.md](README.md).

**SDL-only statement** — `mixin` has no SurrealQL counterpart: it names
a group of member definitions that expands, at compile time, into every
table that spreads it. No parser ground truth applies to the statement
itself (the surface is SDL's own, 2026-09-29); its member
grammar is a table block's body minus `on:` clauses — fields
([field.md](field.md)), indexes ([index.md](index.md)), and events
([event.md](event.md)) — and the expansion emits the plain `DEFINE`
statements each member spells: every engine-level guarantee is the
member's own spec's.

**Surface decisions (settled):**

- **Licensed sugar (rule 6's one licensed deviation):** mixins are
  compile-time expansion and nothing else. A `mixin` statement emits no
  DDL of its own; a spread expands to the member statements, in place,
  at the spread's position in the table's nested order. The database
  never sees a mixin — only the expanded members.
- **Members are the table body minus `on:` clauses** — fields (the
  [field.md](field.md) grammar: bare `name: type <clauses>;` with the
  optional `field` prefix, dotted paths and wildcards, computed forms,
  permissions both forms), indexes, and events (their own specs'
  clause grammars, `on:` never appearing — the host is whichever table
  spreads the mixin). A bare field head named `index` or `event`
  needs the `field` prefix or quoting, exactly as in a table block
  (the keys are active in this body). `///` is rejected on the
  statement itself — it emits nothing, so there is no `COMMENT`
  target (the `sequence`/`config` precedent, rule 5) — and on
  members, the nested-statement rule (spec/table.md: doc comments are
  top-level-only; a member that must carry a `COMMENT` spells the
  statement explicitly on the spreading table, overshadowing). The
  kind-local validations (`fields:` arity per index kind, an event's
  required `then:`) fire at the member's own parse, host-independent;
  the host-dependent rules re-run at the table fold (below).
- **The spread** — `...<name>;` — is a **body entry** of a non-view
  `table` block, not a clause: sigil-led, no key, no colon (the
  taxonomy's clause forms do not apply). One name per entry; the name
  follows identifier rules (quoted forms legal). It is legal nowhere
  else — a view block (views take no nested definitions), a mixin body
  (v1: no mixin-into-mixin), and every other block reject it.
- **Members collide per KIND**: the collision key is the family plus
  its name — a field's dotted path, an index's or event's name. A
  field `x`, an index `x`, and an event `x` never collide with one
  another (distinct namespaces, the merge's family+key model). The
  same key TWICE within ONE body — a mixin's or a table's — is an
  error (SDL-E1067, the E1064 copy-paste rationale: the engine's
  last-one-wins would silently hide the duplicate); the same key in
  two different mixins, or a mixin member against a spread-carrying
  table's explicit member, is the collision machinery above instead
  (last-wins between spreads, explicit overshadow, never this
  error).
- **Explicit members wholly overshadow** — an explicit field, index,
  or event anywhere in the table with the same family key as a member
  replaces that member in that table entirely:
  position-independent (order of the spread and the explicit
  statement is irrelevant), whole-statement (no clause merging — the
  explicit statement *is* the definition). Overriding is per-table;
  other tables spreading the same mixin are unaffected.
- **Spread collisions resolve last-occurrence-wins** (the 2026-09-30
  amendment): when two spreads carry the same family key and no
  explicit member decides it, the LATER spread's member wholly wins —
  the loser is skipped at expansion, never emitted-then-overwritten.
  Engine parity is the ground truth (`DEFINE FIELD OVERWRITE x` twice
  applies the second; the expanded DDL of a collision was always
  legal) and rule 7's own clause fold already plays last-one-wins.
  Spread order is therefore semantic — which is why the formatter
  preserves it and the linter advises on the possibly-unintended
  class (SDL-L0022: a later spread's member shadows an earlier
  spread's; an explicit member's overshadow stays unlinted — the
  author decided it). The same mixin spread twice into one table
  stays an error (SDL-E1064): under last-wins it would be a harmless
  no-op, which is exactly the copy-paste smell to reject.
- **Resolution is global** (the declarative namespace): a spread
  resolves against the document's WHOLE mixin set — defined anywhere,
  any file of the composition, any position. Parse carries the entry
  as an opaque name; a post-parse resolution pass checks every entry
  against the set, so a name resolving to nothing is SDL-E1061 — an
  error, never a silent empty expansion — in every check-class
  context (the CLI's loader runs it; fmt never does, which is why
  any single file formats standalone). Within a mixin body, the
  table-block discipline carries over as PRESENTATION: fields before
  the indexes that use them reads best, but the compiler's emission
  order (README rule 7) places an index after the fields it names
  regardless of authored position.
- **Names**: mixin names are their own namespace (they collide with
  nothing); a duplicate mixin name — within one file (the parse), or
  across a composition's files (the order pass) — is an error, not a
  last-one-wins fold: the reference would be ambiguous in intent.
  Every other definition family shares the class since the
  declarative namespace (README rule 7's duplicates rule: one error,
  any file, any position). `mixin` is not a SurrealQL
  keyword and is never reserved at field
  positions (`mixin: string;` is an ordinary field; quoting and the
  `field` prefix escape it at the ambiguous positions like any word).
- **Validation at expansion**: the rules a nested member meets at the
  table fold are re-run over each LANDED member (an explicitly
  overshadowed member and a later-spread-beaten loser never land):
  the LIGHTWEIGHT rejection rides the spread entry itself; `inline;`'s
  host-relation rule re-runs at the fold, anchored at the member's
  authored site with the host named in its message;
  subfield-under-computed re-runs the same way (the rule's own
  message, no host); the host-independent member grammar (`id`,
  computed shape, flexible's kind, the index/event kind-local rules)
  fires at the member's own parse like any nested statement's.
  Engine-level validations that fire at define time (an index needing
  an existing field, a computed referencing a missing sibling) apply
  to the expanded DDL unchanged.
- **fmt round-trips spreads**: the canonical writer prints the `mixin`
  statement and each `...<name>;` entry (rule 5 anchoring applies — a
  member's or a spread entry's `//` lines ride it). Expansion is never
  visible in canonical SDL written from an authored file. The table
  body prints in the canonical member layout (spec/table.md):
  clauses, then fields, then spreads (relative order preserved — the
  semantics lives there), then indexes, then events, each group a
  stable sort — fields before the spreads so a spread-carried index
  can never define above an explicit field it names (the engine
  validates field existence at define); a mixin body prints its
  members in the same layout minus spreads.
- **Reverse path** (the spread-preserving merge): INFO
  cannot carry mixins — the reader never adopts or synthesizes one
  (the class `raw` occupies: apply-pending, never a define the tool
  emits). The merge keeps the `mixin` statement and every
  `...<name>;` entry authored; per member, the spread's LANDED
  definition pairs against the database child of its key —
  **state-equal** (the differ's own resolution: elisions, the kind
  algebra, comments stripped) and the child is covered by the spread
  (the merged file declares it through the entry); **diverged** and
  the child materializes as an explicit statement on the table — the
  overshadow rule performing the adoption, the spread surviving with
  its member overshadowed position-independently (the canonical
  layout prints the override before the spread — order is
  semantic-free there; idempotent: round two sees the override as an
  explicit member and converges); **absent from the database** and
  the member stays declared through the spread (apply-pending). A
  spread never dies of MEMBER drift — a single-member divergence
  materializes only that member, because no later merge could
  re-derive a removed spread (the reader never synthesizes one) and
  the suddenly-unspread mixin would lint L0021 on the merge's own
  output. The one representation-level exception: a database that
  redefines the whole table as a VIEW merges to the view statement —
  a view body takes no nested definitions (SDL-E1065's class), so no
  spread can ride it; the mixin lints L0021 and the differ reports
  the kind drift. The differ expands the SDL side itself: a diverged
  member is reported as drift on that member's statement, never as a
  mixin-level finding.

```
// no /// on the statement — rejected, no COMMENT target; members
// take none either (the nested-statement rule)
mixin <name> {
  <member entries — the field/index/event grammars, on: never present>;
};                                      // block optional when empty
                                        //   (`mixin base;`)

// inside a non-view table block only:
...<name>;                              // expands to the mixin's
                                        //   members, in place
```

**Errors** (SDL-E1061 block; the numbering keeps its gap — codes are
hand-assigned, the E2xxx precedent):

| code | error |
| --- | --- |
| SDL-E1061 | `...<name>;` naming no mixin of the document — spreads resolve against the composition's whole mixin set (the resolution pass's finding: any file, any position; a typo'd spread is an error, never a silent empty expansion) |
| SDL-E1062 | duplicate mixin name — within one file (parse) or across a composition's files (the order pass); the same code generalizes to every definition family (README rule 7's duplicates rule — one error, any file, any position) |
| SDL-E1063 | *(retired with the last-occurrence-wins amendment — a collision between two spreads no longer errors; SDL-L0022 advises)* |
| SDL-E1064 | the same mixin spread twice into one table |
| SDL-E1065 | a spread entry in any block that is not a non-view table body |
| SDL-E1066 | *(retired with the spread-preserving merge — `normalize` merges mixin-carrying files instead of refusing)* |
| SDL-E1067 | the same member key (family + name) defined twice within one body — a mixin's or a table's |

**Linter:** SDL-L0021 — a mixin defined but never spread (advisory;
the definition is dead weight — it emits nothing anywhere);
SDL-L0022 — a later spread's member shadows an earlier spread's
(advisory; the file compiles to the later definition — the
possibly-unintended class; an explicit member's overshadow stays
unlinted, the author decided it).

## Examples

a mixin, spread, and an overshadowing explicit field:

```sdl
mixin timestamped {
  created_at: datetime;
  updated_at: datetime;
};

table post schemafull {
  ...timestamped;
  title: string;
  // explicit field wholly replaces the mixin's created_at here:
  created_at: datetime {
    default: time::now();
  };
};
```

```surql
DEFINE TABLE post TYPE NORMAL SCHEMAFULL;
DEFINE FIELD updated_at ON post TYPE datetime;
DEFINE FIELD title ON post TYPE string;
DEFINE FIELD created_at ON post TYPE datetime DEFAULT time::now();
```

the same mixin spread into a second table (per-table override,
no cross-table effect):

```sdl
mixin stamp {
  created_at: datetime;
  updated_at: datetime;
};

table account schemafull {
  ...stamp;
  email: string {
    assert: string::is_email($value);
  };
};
```

members carry the full field grammar — dotted paths, computed forms,
permissions:

```sdl
mixin metadata {
  meta.created_by: record<user>;
  meta.created_at: datetime {
    default: time::now();
  };
  label = string::uppercase(title);
  secret_key: string {
    permissions {
      select: none;
    };
  };
};

table invoice schemafull {
  ...metadata;
  amount: decimal;
};
```

members of every kind — indexes and events spread with the fields
(`on:` never appears on a member; the spreading table becomes the
`on` at expansion):

```sdl
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

```surql
DEFINE TABLE article TYPE NORMAL SCHEMAFULL;
DEFINE FIELD slug ON article TYPE string;
DEFINE INDEX slug_u ON article FIELDS slug CONCURRENTLY;
DEFINE EVENT slug_touched ON article WHEN slug != '' THEN 1;
DEFINE FIELD body ON article TYPE string;
```

two spreads colliding — last-occurrence-wins (the later spread's
member wholly wins, the loser skipped at expansion; the explicit
field on the second table decides instead, position-independently):

```sdl
mixin short_id {
  id_hint: int;
};

mixin long_id {
  id_hint: string;
};

table post schemafull {
  ...short_id;
  ...long_id;
};

table comment schemafull {
  ...short_id;
  ...long_id;
  id_hint: bool;
};
```
