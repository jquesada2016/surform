`surform normalize` builds a schema file from a database's state: it
merges the database's `INFO` output — handed over as
interchange files — into an SDL file, and writes the result in
canonical form: the same style [fmt](fmt.html) prints, under the same
width budget (`--max-width`, default 100).

## The happy path: adopt a whole database

Start from a stub that names the scope, hand normalize the
database's `INFO FOR DB` output, and you get a schema file:

```adopt ns=acme db=shop
{
  tables: {
    user: 'DEFINE TABLE user TYPE NORMAL SCHEMAFULL PERMISSIONS FULL'
  },
  params: {
    limit: 'DEFINE PARAM $limit VALUE 10 PERMISSIONS FULL'
  },
  "@tables": {
    user: {
      fields: {
        email: 'DEFINE FIELD email ON user TYPE string PERMISSIONS FULL'
      },
      indexes: {  },
      events: {  }
    }
  }
}
```

Noise the database adds disappears on the way — `PERMISSIONS FULL` on
a field is the engine's default, so canonical SDL elides it.

## The command

```sh
surform normalize schema.surdl --in-place \
  --info root=info-root.txt \
  --info ns=acme=info-ns.txt \
  --info db=acme/shop=info-db.txt
```

At least one `--info` tag is required — with none there is nothing to
adopt, and the run would silently be a disguised `fmt`. One tag per
scope you have; missing scopes simply don't merge:

- `root=<path>` — `INFO FOR ROOT` output (namespaces, root users,
  the default config).
- `ns=<name>=<path>` — `INFO FOR NS` output.
- `db=<ns>/<name>=<path>` — `INFO FOR DB` output **plus** each
  table's `INFO FOR TABLE` child map under a reserved `"@tables"`
  key.

The interchange is `surform migrate pull`'s output — all three
scopes, the per-table child maps included, fetched straight from the
live database (a restricted credential pulls only the scopes it can
read; see the [migration page](migrate.html)):

```sh
surform migrate pull --endpoint http://127.0.0.1:8000 --ns acme --db shop
```

The `surform-info-*.txt` files land in the working directory
(`--dir` moves them; a pull under a restricted credential writes
only the scopes it can read) and the ready-made command prints — the
same tags the [migration page](migrate.html) shows from the compare
side.

A [composition root](includes.html) normalizes as a project: with
`--in-place` every merged statement returns to the file that authored
it (the includes stay put; unchanged files are not rewritten), and
without it the flattened canonical form prints. One refusal
(SDL-E0013), two shapes: an adopted `namespace`/`database` — a scope
statement belongs in the root's preamble, where its position re-binds
every scope below it — and a statement the merge would have to move
past an include subtree that another file's definitions still depend
on (an adopted analyzer whose index lives in a composed file, say).
Place those by hand and re-run.

## The four verbs

What normalize does with each difference between your file and the
database:

- **adopt** — statements the database has and your file lacks come
  in (that is the adoption above).
- **elide** — values equal to the engine's own defaults disappear.
  Canonical SDL is minimal: `BATCH 1000 START 0` collapses to a bare
  sequence, an event's `WHEN true` vanishes.
- **preserve** — what `INFO` cannot express stays yours: database
  `strict;`, index `concurrently;`, user secrets, a bearer access's
  stored-but-unprinted JWT clauses.
- **fold** — server-synthesized bookkeeping folds back into the
  shape that implies it: an array field's `tags.*` subfield folds
  into `tags: array<int>`, a relation's materialized `in`/`out`
  fields fold into the header arrows.

Placement is topological: adoptees land after the last definition
they depend on — an index nests after its table, a fulltext index
after its analyzer — with deterministic tie-breaks, so repeated runs
are stable.

## Running it twice

Normalize the same database twice and the second run changes
nothing — the merge of the merged file is the merged file. Combined
with the fixed point on [fmt](fmt.html), your schema file settles:
it stops churning once it matches the database.

## Where next

- [Schema migration](migrate.html) — compare without merging.
- [Formatting](fmt.html) — the canonical style normalize writes.
