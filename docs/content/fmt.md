`surform fmt` prints a schema file in the **canonical style** — one
clause order, one spelling per construct, one layout. Pass
`--in-place` to rewrite instead of printing — and on a
[composition root](includes.html), `--in-place` rewrites the whole
tree: every participating file formats and writes back, so a
hand-edit in any included file is tidied by formatting the root.

Canonical output is a *fixed point*: format a formatted file and
nothing changes. That turns style debates into a one-line CI check —
`surform fmt --check schema.surdl` exits 0, silent, when the file is
already canonical and exits 1 with a diff of what would change when
it is not (or the older dance: `--in-place`, then `git diff
--exit-code`).

One input is refused in every mode, `--check` included: a generated
migration script — a file whose first line is the
`-- surform migration` draft header [migrate](migrate.html) writes.
`--` is comment syntax in the database's own grammar, not SDL's, so
the script is not SDL to reflow; the schema to format is the draft
header's `-- source:` file.

## What canonical means

Two files can say the same thing many ways — aliases (`ns` vs
`namespace`), header vs block spellings, clause order, indentation.
`fmt` picks one and only one. For example: backticks that aren't
needed disappear, keywords settle at their canonical word, clauses
print in the spec's order, and empty blocks collapse:

```sdl-fmt
namespace acme; database shop;
table `user` { schemafull; field name:string; field email: string { readonly; }; }
```

Notice: `name:string` grew its space, and the one-line squeeze
unfolded into the standard layout. And `` `user` `` **keeps its
backticks** — `user` is a SurrealQL keyword, so canonical form quotes
it even though SDL also accepts it bare.

Assignment short forms survive when they're the whole story —
`param $limit = 50;` stays short; add any other clause and the block
form takes over:

```sdl-fmt
namespace acme;
database shop;

sequence invoice_number = 1000;
param $default_limit = 50;
```

## What's kept and what's dropped

- `///` **doc comments are kept** — they are part of the schema
  (they become `COMMENT`s).
- `//` **plain comments are kept**, and the anchor is the clause,
  not just the statement. A comment prints above whatever it
  preceded: the statement, a nested child, or any clause at any depth
  (sub-block clauses, per-operation permission groups, api method
  groups included). It rides that clause through the canonical clause
  reordering instead of drifting to the block's end. A comment on the
  same line as a terminator, as in `then: select …; // why`, stays on
  that clause's line — same-line notes are how a complex SurrealQL
  expression clause carries its explanation. Lines inside a block
  that anchored to no child print just before that block's close;
  lines after the last definition print at end of file.
  Comments are layout, not meaning: they never change what a file
  compiles to, and [migrate](migrate.html) treats a commented and an
  uncommented file as the same schema. One boundary: comments inside
  captured values — fn bodies, `then:` blocks, view queries, expression
  values, fn parameter lists — are not SDL structure and drop. (A
  comment in a field's kind capture re-anchors to the field.)
- **One blank line between top-level definitions** — `table`, `fn`,
  `access`, `sequence`, and every other statement separates from its
  neighbors so a file reads in paragraphs. The one exception: a run
  of `include` statements packs tight as one import group (the group
  still separates from the definitions around it). The separation is
  part of the canonical form: comments stay glued to the statement
  they annotate (a `///` doc block never floats off its definition),
  and the block interiors — fn bodies, a table's fields — stay
  packed, where structure comes from indentation instead.
- A table body prints in the **member layout**: the table's clauses,
  then fields, then the spread entries (in their authored order —
  which spread wins a collision is order), then indexes, then events
  — fields first keeps a spread-carried index from ever defining
  above the explicit field it names. A `mixin` body prints its
  members in the same layout minus spreads.
- Long lines wrap: a statement that would run past the width budget
  (100 columns by default, `--max-width` to change it) breaks at its
  natural seams — one IF/ELSE branch per line, boolean chains
  at their operators (an `OR` group per line, its `AND` comparisons
  breaking within the group only when the group itself is too long),
  string concatenations at `+`, one call argument per line, a long
  field type one union arm per line, and a statement whose clauses
  overflow the line one clause per line (a view's SELECT lays out the
  same way: projections indented under `SELECT`, then `FROM`,
  `WHERE`, `GROUP BY` on their own lines) — and a definition's
  closing `};` lands at the opener's column. One exception: an
  expression value under 40 characters never breaks — a short call
  broken across three lines reads worse than the long line it sits
  on, so a narrow `--max-width` cannot shatter small expressions (a
  field type's union still breaks by line budget alone). Two touches
  are fmt's own and both
  reparse-verified against the original: a wrapped mixed `AND`/`OR`
  chain prints explicit parentheses around each `AND` group, so the
  wrapped form shows the precedence instead of asking you to remember
  it; and a relation header's arrows print compact
  (`table user->member_of->organization` — the spaced spelling means
  the same thing). [normalize](normalize.html) takes the same
  `--max-width`: it writes canonical files under the same budget.
- Values normalize to the engine's spelling: `expiry: 30d;` prints as
  the equivalent `4w2d` — same value, canonical spelling.

## fmt in the loop

The usual rhythm:

```sh
surform fmt --in-place schema.surdl   # tidy
surform check schema.surdl            # validate
surform build schema.surdl            # emit
```

Since [normalize](normalize.html) writes canonical form too, running
`fmt --in-place` on its output produces no diff.

On a [composition root](includes.html), the two modes split:

- **Print mode** formats the file as authored — the `include`
  statements survive untouched, and nothing is resolved, flattened,
  or read from the tree. Spreads parse as opaque names, so ANY single
  file formats by its own path, its spreads' mixins wherever they
  live.
- **`--in-place`** formats the whole tree and writes every
  participant back, each file from its own loader parse — the same
  validation the loader applies (the load includes the composition's
  spread resolution), while each file's text stays authored. The load
  runs first: a dirty tree (a broken file, a spread nothing defines)
  is never rewritten, in any file. A file without includes is the
  one-file tree — same behavior as ever.

Print mode is per-file and tolerant: a spread that names nothing
still round-trips (resolution is check-class — `check` is where the
typo reports). `--in-place` is load-gated, so the same typo fails
the write.

## In your editor

The [VS Code extension](vscode.html) formats `.surdl` files with this
same writer: **Format Document** pipes the buffer through `surform
fmt` (stdin in, canonical SDL out), so what lands in your file is
exactly what the CLI would write — modulo the buffer's own line-ending
convention. The binary is looked up on `PATH` — or pointed at with the
`surform.path` setting. A file with diagnostics is left untouched; the
compiler's errors surface instead. The extension page also covers the
grammar's syntax highlighting and the diagnostics channel.

## Where next

- [Adopting a database](normalize.html) — merge database state into
  your file.
- [The CLI](cli.html) — flags and exits.
