# TYPE (type aliases)

Global conventions and notation: [spec/README.md](README.md).

**SDL-only statement** — `type <Name> = <kind>;` names a type alias:
a compile-time textual expansion rewrites every kind that references
the name into the alias's kind. SurrealDB has no type-alias concept
(no `DEFINE TYPE` exists; the 3.3 `Kind` grammar is ground truth —
`sql/kind.rs`), so the statement emits no DDL of its own, the
database only ever sees the expanded kinds, and no parser ground
truth applies to the statement itself (the surface is SDL's own, the
[mixin](mixin.md) precedent).

**Surface decisions (settled):**

- **Names are Capitalized, and that is the disambiguation** (2026-10-25
  design dialogue): an alias's name is an identifier whose decoded
  form matches `[A-Z][A-Za-z0-9_]*`. At every kind position, a bare
  Capitalized word is an alias REFERENCE and a lowercase word is a
  SurrealQL kind — the case of the first letter is the whole
  distinction, no sigil, no qualification. SurrealQL's kind heads are
  all lowercase (`string`, `record`, `array`, …) except the exact-case
  math literals `NaN`/`Infinity`, so the grammar territories do not
  overlap: no SurrealDB kind can ever be spelled `Email`. This is the
  collision-immunity guarantee, and it is structural, not procedural:
  a SurrealDB minor bump that adds kind heads cannot reinterpret an
  existing alias use (expansion is total before emission anyway — the
  server never sees an alias name), and the unthinkable case of a
  capitalized upstream head is a floor-bump spec event whose
  reservation check re-extracts from the new grammar and fails the
  DECLARATION loudly — never a silent change of meaning at use sites.
- **Reserved names**: a declaration whose name ASCII-lowercases to
  any SurrealQL kind head — the bare heads plus the generic heads
  (`record table geometry file array set option`) plus `any` — is
  SDL-E1072: head matching is ASCII-case-insensitive, so such an
  alias would lose EVERY use site to the head and be unreachable
  (`type String = int;` parses uses of `String` as the `string`
  head). The exact-case `NaN`/`Infinity` spellings are reserved by
  the same code. A quoted declaration spelling decodes first: the
  DECODED name must satisfy the Capitalized rule; a name that could
  not be spelled bare (`2Fast`, non-word characters) is E1072 or the
  name position's own rejection.
- **Legal reference positions** — exactly the kind positions the
  [field](field.md) grammar's type clause and the [function](function.md)
  argument/return kinds share: the kind's top level, a union arm, an
  `option<…>` interior, an `array</set<` element kind, an object
  literal's VALUE, an array literal's element. **The name-list
  positions are NEVER references**: `record<Email>`, `table<Email>`,
  and `file<Email>` interiors name tables and buckets at every case
  (the name grammar is unchanged), and `geometry<…>` interiors are
  the fixed keyword set. Quoted spellings are not references either —
  a backtick word at a kind position stays invalid.
- **Expansion is total before emission** (the codegen model's
  "alias expansion precedes emission"): the lowering pass rewrites
  every `Kind` referencing an alias to its expansion's text (chained
  aliases resolve recursively, memoized) and drops the `type`
  statements. Upstream's kind parser rejects unknown bare words, so
  a leaked name would be a hard error there — the stage-2 DDL oracle
  would catch it; the totality is what makes the feature
  server-version-immune. The `table` TYPE clause (`NORMAL/ANY/
  RELATION/VIEW`) and the vector-index `TYPE` (F32/BF16/F64) are not
  data kinds and take no aliases.
- **The kind grammar inside a declaration is the permissive one**: a
  declaration's right-hand side may reference other aliases
  (`type Emails = array<Email>;`), and everything else the kind
  grammar accepts — unions, generics, literal kinds, object literals
  — parses unchanged. `none | T` folds to `option<T>` at use and
  emission exactly as authored kinds do.
- **Resolution is global** (the declarative namespace): a reference
  resolves against the composition's WHOLE alias set — defined
  anywhere, any file, any position; use before define is legal. The
  parse carries references as opaque names; the check-class pass
  ([`order::check_document`] — the CLI's loader runs it; fmt never
  does) reports a name resolving to nothing as SDL-E1071 with a
  did-you-mean: a typo'd alias is an error, never a silent kind. A
  lowercase unknown word keeps the kind grammar's own rejection
  (SDL-E1026).
- **Duplicates and cycles**: a duplicate alias name — within one file
  (the parse's registry) or across a composition's files (the order
  pass) — is SDL-E1062, no last-one-wins fold (the reference would be
  ambiguous in intent). Alias definitions form a cycle class of their
  own: `type A = B; type B = A;` has no expansion and is SDL-E0015
  with the chain named.
- **The expansion must be an expressible kind within the ceiling**
  (2026-10-25, the deep-chain review): each alias's expansion is
  resolved bottom-up (iteratively — a chain never recurses per link)
  and checked whole. A composition whose expansion crosses the kind
  grammar is refused — each link legal on its own, the whole
  unexpressible: a chain of option wrappers composes
  `option<option<…>>`, which no parser accepts (SDL-E1074, the
  expansion named). An expansion nesting beyond the **kind nesting
  ceiling (256 levels)** is refused — every walk the expanded kind
  feeds (the writer's, the algebra's, the engine's own parser)
  recurses over it, and the ceiling keeps that bounded at a height no
  real schema approaches (SDL-E1073, the measured depth named). Both
  gates hold at every kind position: a declaration's expansion AND a
  use site's composition (`option<A>` over an option-alias is the
  same unexpressible whole, the field the offender), the site's own
  wrapper counted against the ceiling. Aliases riding an unresolved
  name carry no finding of their own — the root cause is reported
  once.
- **Alias names are their own namespace** (the mixin rule): they
  collide with nothing — a table, a mixin, or a param may share an
  alias's name; only another TYPE declaration collides.
- **The `flexible` interaction defers**: `flexible;` demands a kind
  containing an object — a parse-time check ([field.md](field.md)).
  A kind that references an alias decides at resolution: the order
  pass re-asks the question on the EXPANSION and reports SDL-E1050
  naming it when the expansion holds no object. An unresolved
  reference stays the E1071's business (no double-report).
- **`raw` is verbatim** (rule 8): alias references inside a raw body
  are SurrealQL text and are never expanded.
- **`///` is rejected on the statement** — it emits nothing, so
  there is no `COMMENT` target (the `sequence`/`config`/`mixin`
  precedent, rule 5); `//` comments ride at both ends (the
  statement's leading block and its same-line after-note).
- **fmt round-trips aliases**: the canonical writer prints the
  statement verbatim — the kind in its authored spelling (rule 4's
  invariant: the writer never re-spells a kind) — and expansion is
  never visible in canonical SDL written from an authored file.
  Statements break with one blank line like any definition (no
  include-style packing). Top level only: the statement is illegal
  in every block (a table body's `type` remains the clause key; the
  extension grammar gives the head its own rule so an indented
  `type:` clause line never inks as a statement).
- **Reverse path**: INFO stores only concrete kinds, so the reader
  never sees an alias name and the strict kind grammar the readers
  use is unchanged. The merge's alias handling (authored spellings
  surviving when the read state algebra-equals the expansion) lands
  with the reverse-path unit (2026-10-25, follow-up to this one).

```
// top level only; no /// on the statement; the name is [A-Z][A-Za-z0-9_]*
type <Name> = <kind>;        // the kind: the permissive grammar —
                             //   other alias references legal (chaining)

// inside a kind (field type, fn args/returns) — a bare Capitalized
// word at a kind position is the reference:
//   <Name>                    top level
//   <Name> | <concrete>       a union arm
//   option<<Name>>            an option interior
//   array<<Name>(, N)?>       the element kind
//   { key: <Name> }           an object-literal value
//   [<Name>, …]               an array-literal element
//   record<lowercase-name>    NOT a reference — a table name
```

**Errors** (the SDL-only-construct block continues; codes are
hand-assigned, gaps stay):

| code | error |
| --- | --- |
| SDL-E1071 | a Capitalized word at a kind position resolving to no alias of the composition — the order pass's finding: any file, any position; a typo'd alias is an error, never a silent kind |
| SDL-E1072 | a declaration's name is not usable: not Capitalized (`[A-Z][A-Za-z0-9_]*`), or reserved — a SurrealQL kind head ASCII-case-insensitively, or the exact-case `NaN`/`Infinity` |
| SDL-E1073 | a kind's expansion nests beyond the kind nesting ceiling (256 levels) — a declaration's or a use site's composition alike, the site's own wrapper counted; the owner and measured depth are named |
| SDL-E1074 | a kind's expansion is not a kind the grammar accepts — each part legal, the composition crossed (an option chain's or an `option<A>` use site's `option<option<…>>`); the owner and expansion are named |

Ridden rules: SDL-E1062 (duplicate alias name — parse within a file,
the order pass across files), SDL-E0015 (an alias-definition cycle,
the chain named), SDL-E1050 (the deferred flexible check on the
expansion), SDL-E1026 (a lowercase unknown word at a kind position —
unchanged).

## Examples

a declaration, chained aliases, and the use positions:

```sdl
type Email = string;
type Emails = array<Email>;

table user schemafull {
  name: string;
  contact: Emails;
  recovery: option<Email>;
  both: Email | int;
  profile: { email: Email };
};
```

a function's argument and return kinds take aliases:

```sdl
type Email = string;
type Emails = array<Email>;

fn contacts($e: Email) -> Emails { return [$e]; };
```

use before define — the declarative namespace (position is not
resolution's business):

```sdl
table post schemafull {
  author_email: Email;
};

type Email = string;
```

the name-list position is a table NAME, never a reference (the
`record<email>` kind survives expansion untouched):

```sdl
type Email = string;

table account schemafull {
  owner: record<email>;
};
```

an unknown reference is an error — a typo'd alias can never slip
through as a silent kind:

```sdl-error expect=SDL-E1071
type Email = string;

table user schemafull {
  contact: Emial;
};
```

a reserved name is rejected — the kind head would win at every use
site:

```sdl-error expect=SDL-E1072
type String = int;
```

a duplicate name is SDL-E1062, and an alias cycle is SDL-E0015 with
the chain named:

```sdl-error expect=SDL-E1062
type Foo = int;
type Foo = string;
```

```sdl-error expect=SDL-E0015
type A = B;
type B = A;
```

an expansion that crosses the grammar is refused — each link legal,
the whole unexpressible:

```sdl-error expect=SDL-E1074
type A = option<int>;
type B = option<A>;
```

a chain nesting past the ceiling (an `array` chain is the
shape-legal way to reach it — 257 aliased links) is SDL-E1073 with
the measured depth named; at 256 links per level the example does not
fit a fence, and the depth pass reports the FIRST link that crosses
the line.
