A schema can live in more than one file. An `include` statement
composes another schema file into this one: the included file's
statements take the include's place in the stream, in order
(depth-first — an included file's own includes splice at their
positions).

Composition is defined to behave exactly like its flattening: a
project of several files compiles to the same DDL, produces the same
lint findings, and migrates the same as the one file you would get by
pasting them together. Nothing is renamed or namespaced — SurrealDB's
catalog is flat, and the statement composes source, not names.

## The layout

Two order rules keep the combined stream well-scoped:

- `namespace`/`database` precede the file's FIRST `include` — the
  composition preamble (SDL-E1069). They scope everything below them,
  the composed content included.
- An included file carries no `namespace`/`database` at all — those
  would re-scope the importing file below the splice (SDL-E1070).

Beyond that, an `include` is legal anywhere in the file — mid-body
included — so mixins, tables, and other definitions may sit above,
between, or below the includes.

A typical tree:

```text
schema/
  root.surdl          namespace acme;  database shop;
  mixins.surdl          mixin timestamps { ... };
  tables/users.surdl    include '../shared/ids.surdl';  table users { ...timestamps; };
  tables/sessions.surdl include '../shared/ids.surdl';  table sessions { ...timestamps; };
  shared/ids.surdl      sequence user_id { start: 1; };
```

with `root.surdl` naming its parts:

```text
namespace acme;
database shop;

include 'mixins.surdl';
include 'tables/users.surdl';
include 'tables/sessions.surdl';

table account {
    ...timestamps;
    field owner: record<users>;
};
```

The flattened equivalent — what the compiler behaves as if you had
written — verifies as an ordinary schema:

```sdl
namespace acme;
database shop;

mixin timestamps {
    field created_at: datetime { default: time::now(); };
    field updated_at: datetime { default: time::now(); };
};

sequence user_id { start: 1; };

table users schemafull {
    ...timestamps;
    field name: string;
};

table sessions schemafull {
    ...timestamps;
    field token: string;
};

table account {
    ...timestamps;
    field owner: record<users>;
};
```

## Resolution

Paths resolve relative to the including file's directory (`..` may
climb; absolute paths are rejected — a schema tree stays portable).
The include graph declares participation, not an elaboration order:
cycles are legal (A includes B includes A composes fine), each file
splices exactly once, and discovery order (depth-first, first visit)
breaks ties. The graph still polices itself: the same file twice in
ONE file's include list is an error (SDL-E0014), and a diamond (a
file already spliced, declared again by another file) is a no-op the
linter advises on (SDL-L0026, anchored on the no-op declaration;
`// surform-lint-allow: SDL-L0026` above the include silences it).
A missing file is SDL-E0008, anchored on its include.

## Mixins across files

A spread resolves against the whole composed document's mixin set —
defined in any file, at any position. A forward spread is legal: a
table may spread a mixin its file composes later. Two consequences:

- Shared mixins live anywhere in the tree, root body included — there
  is no composed-first preamble discipline.
- Duplicate names collide (SDL-E1062) — mixins and every other
  definition family alike, across files exactly as within one. Stream
  position decides nothing: composition only extends the stream, and
  the flattening's authored order is just the tie-break in the
  compiler's emission sort.

## Tools on a composition root

`check`, `build`, `lint`, and `migrate generate` follow includes and
work on the whole tree. Diagnostics and lint findings render against
the file they came from, and an allow comment (`surform-lint-allow:`,
see [the linter](lint.html)) only excuses findings in its own file.
The reference warnings are composition-aware: an undefined table
(`SDL-L0024`) checks the whole tree before it fires — a target
another participant defines resolves, wherever that file sits in the
composition.
`normalize` writes the whole tree back: with `--in-place` every merged
statement returns to the file that authored it, and without it the
flattened canonical form prints (see [normalize](normalize.html)).
`fmt` is the authored counterpart: print mode formats the file's own
text as authored, and `fmt --in-place` writes the whole tree — every
participating file formats from its own loader parse and
writes back, so a hand-edit in any included file is tidied by
formatting the root (a dirty tree is never rewritten). `fmt` itself
never resolves spreads, so any single file, even a leaf whose spreads
resolve only through its composition, formats cleanly by its own
path in print mode; only `--in-place` is load-gated.

Contexts that parse inline text — the [playground](playground.html),
runnable examples on these pages — have no file system, so they
reject the statement outright:

```sdl-error expect=SDL-E1068
include 'tables/users.surdl';
```
