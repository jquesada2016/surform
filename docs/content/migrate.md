Schemas drift. Someone adds a table by hand, a migration half-ran, a
colleague's branch applied first — and your schema file no longer
describes the database it names. `surform migrate generate` makes the
drift visible and closes it: it compares your file against the
database's rendered `INFO` output (or one schema version against the
next, [below](#from-one-schema-version-to-the-next)), prints the
differences, and generates the one script that converges the database
to your file — defines and removals, per scope section, each in its
own runnable transaction (a changed define-once clause is the one
exception: its note names the manual repair).
The script can stay a pipe (printed to stdout, run today), or become
a migration FILE — numbered, saved, filled, committed, self-recording
— [the pipeline below](#saving-drafts-and-committing-them).

## A live walkthrough

Start from a schema file. This is the whole drift demo — your file
declares `user` and nothing else:

```drift scenario=stale-table
namespace acme;
database shop;

table user {
  schemaless;
};
```

Reading the report: the `−` sigil marks a **database-only**
definition — the `archived_orders` table exists in the database but
not in your file. The row carries the `(required data-migration
slot)` marker — the removal orphans data — and the report's manifest
lists the obligation. The convergence script removes it under a
`SURFORM WARNING` block: `REMOVE TABLE` deletes the table's key
range, so every document in it is destroyed. Between the warning and
the removal sits a `SURFORM REQUIRED` slot — an empty, editable
region where you write the query that moves the data you need
(an `e.g.` example line — delete its `-- e.g. ` prefix and adjust
the target).
`IF EXISTS` makes the removal a no-op on re-runs.

## The other direction: SDL the database lacks

Definitions in your file that the database hasn't got are *apply
pending* in the report (`+`) — and the script closes that direction
too: it defines them from your file's side (`DEFINE … OVERWRITE`, or
`IF NOT EXISTS` for the define-once families), in
the same dependency-aware order `surform build` emits under. There is
no separate "apply by hand" step anymore; the script is the whole
migration.

Kinds drift too. If someone redefines a field's type by hand, the
report shows it as a change (`~`) with the clause-level delta — and
the script does more than re-define the field from your file's side:
it **stages the table**, because a type change over live rows needs a
data step in the middle:

```drift scenario=changed-kind
namespace acme;
database shop;

table user {
  schemafull;
  field name: string;
  field email: string;
};
```

Read the generated script top to bottom and the staged shape falls
out — [why the engine demands this order](#staged-migrations-re-keying-a-field-over-live-rows):
the relaxed shell first, then the kind relax, then the fill-in data
section, then the final definitions. The report's note under the
delta says so, and the `== staged migrations ==` manifest lists the
obligation.

## From one schema version to the next

The database's `INFO` output is one input; a schema file can be the
other. To evolve a deployed schema — v1 lives in the database, you
have written v2 — hand `migrate generate` both versions:

```sh
surform migrate generate --from schema-v1.surdl --to schema-v2.surdl
```

The comparison is the same comparator, the report and script the same
shape: definitions v2 holds that v1 doesn't are apply-pending (`+`,
the script defines them), definitions only v1 holds are database-only
(`−`, the script removes them — data-migration slots and warnings
riding exactly as above), and definitions both hold but differ on
drift (`~`, the script re-defines from v2's side). Running the script
against a database at the v1 state lands it exactly at v2 — that
property is executed against the embedded engine in the test suite,
not assumed.

The scope rules read like the files do:

- A scope (namespace or database) that only **v2** declares is new —
  the script defines its namespace and database, then their content.
- A scope that only **v1** declares is retired — the script says so
  under a `SURFORM WARNING` block with a single `REMOVE DATABASE` /
  `REMOVE NAMESPACE` (the subtree goes with it, and the warning
  names every finding it consumed), not a scatter of
  per-table removals.
- Scopes both declare compare ordinarily.

`--check` rides here too, with the same contract: the report only,
exit 1 on drift — the gate for "every deployed environment carries
the latest schema version" once your deployment tooling records which
version it applied.

## Adopting the pipeline: the baseline migration

Everything above assumes the pipeline already runs. A database your
file already converges has no migration history — the ledger is
empty, `migrate status` starts blank, `revert` has no row to revert
to, and the first real drift becomes `0001` against a database that
has silently been converging for months. The fix is a **baseline**:
migration `0001` records the state the database already stands at,
and every later migration descends from it.

The recipe is the bare-FILE input: hand `migrate generate` one file
and the file is the TO side of a migration whose FROM side is empty -
every definition rides apply-pending (`+`) and lands as
`DEFINE … OVERWRITE` (`IF NOT EXISTS` for the define-once families):

```sh
surform migrate generate schema.surdl --save
# == drift ==           every definition apply-pending (+)
# == draft ==
# migrations/0001_schema.surql
surform migrate commit migrations/0001_schema.surql -m "baseline: the schema as it stands"
surform migrate push --endpoint http://127.0.0.1:8000 --ns acme --db shop
```

The spelling is the version pair with an empty file on the from side,
minus the scaffolding - the two runs produce byte-identical reports
and scripts, and the longhand still works when a from-side file
already exists.

Three properties make the baseline safe to run over a database that
already matches the file (all engine-verified, and the recipe's
artifacts are the same files the tests execute):

- **No slots.** An empty from side removes nothing, so nothing can
  orphan data — the draft carries no `SURFORM REQUIRED` regions and
  commits as-is.
- **The apply is a no-op on the schema.** Every line re-states the
  file's side (`DEFINE … OVERWRITE`; a sequence define is
  `IF NOT EXISTS` and skips an existing counter altogether), so
  applying it over the converged database redefines what is already
  there and changes nothing — and if the database has quietly
  drifted, the baseline converges that drift too, except a
  define-once clause change, which the define-once form cannot apply.
  The re-run
  is a no-op as with any OVERWRITE script.
- **The ledger row is the only new thing.** The push (or applying the
  committed file through any SurrealDB tooling) lands row `0001` —
  `migrate status` then reports directory and ledger in agreement,
  and the first real drift drafts `0002` with history behind it.

One rule holds as on any commit: run `commit` from the directory the
draft's `-- source:` file resolves from, or record an absolute path
([above](#saving-drafts-and-committing-them)).

## The data-migration slots

Removing schema can orphan data, and the script says so where it
happens. Two classes carry a `SURFORM REQUIRED: DATA MIGRATION`
block — a named, greppable, editable region — immediately before the
removal that orphans the data:

- **A removed table** destroys every document in it (the key range
  goes with the table). The slot asks for the export, archive, or
  move query for the data you need; its example line is runnable —
  delete the `-- e.g. ` prefix and adjust the target.
- **A removed field** drops the field from the schema, but the
  stored values are NOT deleted — they remain on the rows, outside
  every field definition, until you clean them up. The slot asks for
  the copy to the values' new home, names the table and the dropped
  field, and hints the later cleanup (`UPDATE t UNSET a;`).

Fill the slots before running the script. The queries you write run
inside their section's transaction, before their removals — and after
the scope's defines, so a table the same section creates is already a
valid target for the move.

Everything else stays quiet: events, indexes, and analyzers are
catalog machinery (nothing is lost removing them), and no other
class orphans data. There is no rename detection — even an obvious
candidate (a table with one field out and one field in) gets no
automatic suggestion: the human names the move, the slot names the
stakes.

The manifest under the report — `== required data-migration slots
==` — lists every slot (scope, family, key, the stakes), so CI and
reviewers see the unfilled obligations without reading the whole
script. `--check` shows it too.

## Staged migrations: re-keying a field over live rows

The hardest migration shape is the re-key: a field's type changes
while the table holds rows — an edge table's `in` moving from
`record<user> | record<fleet_account>` to a new `record<seat>`, say.
Two engine facts (verified against the embedded engine, not assumed)
make the naive script fail:

- **The define itself never validates existing rows.** `DEFINE FIELD
  OVERWRITE in ON t TYPE record<seat>;` applies cleanly over a table
  full of stale values — in or out of a transaction, schemaful or
  not. Rows keep their old values.
- **A data step that rewrites the values fails under the OLD kind.**
  `UPDATE t SET in = seat:r1;` — run before the new kind lands — is
  rejected with `Couldn't coerce value for field 'in' … Expected
  record<user> | record<fleet_account> but found seat:r1`. The error
  names an existing row, so it reads like the define validated rows —
  it is the write the old kind rejected.

So the workable order is the opposite of instinct: the definitions
land first, the data step runs under them. The script stages exactly
that, inside the section's one transaction:

1. **The relaxed shell** — `DEFINE TABLE OVERWRITE t TYPE ANY
   SCHEMALESS;`. Schema mode and permissions drop to the engine's
   defaults: schemaless accepts every write, and a table defined
   without a `PERMISSIONS` clause denies every non-privileged
   session — the relaxed state is writable by the migration and
   closed to everyone else, and the window never commits (the whole
   sequence shares the section's transaction).
2. **The kind relaxes** — `DEFINE FIELD OVERWRITE in ON t TYPE any;`.
   Field definitions survive a table overwrite (that is why the shell
   alone is not enough), so the old kind must be lifted explicitly.
   The relax line is deliberately minimal — no `default`, `value`,
   `assert`, or `flexible` rides, since those would constrain or
   rewrite the data step itself.
3. **A `SURFORM REQUIRED: DATA MIGRATION` section** — the fill-in
   region between the relaxed definitions and the final ones. Its
   prose names the re-keyed fields and their new kinds, and each
   spellable re-keyed field carries a runnable example: the engine's
   own `<kind>` cast for the scalar kinds (`UPDATE t SET age =
   <string> age;` — verified against the engine; the fn-call casts
   like `string::from` are rejected — no such builtin), the record's
   id for `record<...>`. Write
   the backfill (`UPDATE member_of SET in = seat:r1;`); it runs under
   the relaxed kinds, before the final definitions, in the same
   transaction. Delete the region if no data needs moving — the
   marker gate makes that decision explicit at `commit`.
4. **The final definitions** — the state the schema file describes,
   wholesale.
5. **A `SURFORM NOTE: INDEX REBUILD` block** — commented out, after
   the final defines: every UNIQUE index on the table, plus every
   index whose fields name a re-keyed field, each offered as
   `REMOVE INDEX …` + the SDL's own define. Uncomment what applies:
   a data section that rewrote indexed fields very likely needs the
   rebuild, and a UNIQUE index's define is the one row-validating
   statement in the engine — it fails outright if two rewritten rows
   collide.

Views and lightweight relations never stage: a view's `AS SELECT` is
its whole definition (a `SCHEMALESS` shell would destroy it), and a
lightweight relation refuses child defines. A table whose own
definition converged (only its field re-keyed) still stages — and its
own `DEFINE TABLE` rides the final defines so the relaxed shell never
persists. A staged table the SDL never declares (a standalone field's
host) takes no shell at all — it is unmanaged database-side, the kind
relax line alone lifts the old kind, and the script touches nothing
the file never owned.

The report names the class twice: the note under the field's delta
("the script stages this table") and the `== staged migrations ==`
manifest (scope, table, the re-keyed fields - fill the section or
delete it before commit).

## Reading the report

The report leads with a summary line — findings, defines, removals,
data-migration slots, staged tables — the whole drift at a glance
before the rows. A changed kind reads one line (`field x: string` vs
`field x: int`), a nested child never appears twice (its own row is
the only listing), and a block held on one side quotes whole.

| sigil | class | meaning |
|---|---|---|
| `~` | drift | both sides have it, and it differs — clause deltas follow; the script re-defines it from your file |
| `−` | database-only | in the database, not in your file — the script removes it; when the removal orphans data the row is marked and lands in the slot manifest |
| `+` | SDL-only | in your file, not yet applied — the script defines it |
| `·` | expected-unmanaged | informational — e.g. the auto-seeded default namespace; never removed, never trips the exit |

Exit codes: **0** converged, **1** drift in either direction when
nothing was saved (CI signal) — expected-unmanaged never trips it. A
successful `--save` exits 0: the draft is the artifact, so `generate
--save && surform migrate commit …` composes. With `--check` the
report is all you get — no script, and the drift exit stays — which
is exactly the CI gate:

```sh
surform migrate generate schema.surdl --info db=acme/shop=info-db.txt --check \
  || echo "schema drifted!"
```

## The script's shape

- **The staged prelude rides before the defines.** A re-keyed table's
  section opens with the relaxed shell, the kind relaxes, and the
  `SURFORM REQUIRED` data sections ([above](#staged-migrations-re-keying-a-field-over-live-rows))
  — the engine's own order — before the defines half.
- **Defines first.** Every SDL-only and drifted definition renders its
  define statement from your file's side — `DEFINE … OVERWRITE`, the
  desired state, wholesale. The define-once families render
  `IF NOT EXISTS` instead: a secret-less user keeps its stored
  credential, and a sequence keeps its live counter — applying a build
  never rewinds one, so an authored change to its
  `start`/`batch`/`timeout` does not apply in place. A drifted
  define-once definition carries a `-- note:` naming the manual repair
  (for a sequence, `DEFINE SEQUENCE OVERWRITE <name> BATCH <b> START
  <n> TIMEOUT <t>`, spelling every authored clause; the counter
  restarts). A standalone field/event/index define renders inside its
  host table's emission, after the host's `DEFINE TABLE` and before
  the nested members that name it. Both halves of that placement are
  load-bearing: a migration can hold a table in a form the engine
  refuses to hang a member on, and an index refuses to define against
  a `SCHEMAFULL` table that lacks the field. A member your table block
  nests renders once, inside that block's emission; a raw-authored
  define follows its header's `ON <table>` the same way (a raw body
  with no readable `ON` keeps its place). A view defines after every
  table it selects from, and a raw fulltext index after its analyzer —
  either side may be a raw block. Every define-time value slot orders
  after what it reads: a param's `VALUE`, a sequence's
  `BATCH`/`START`/`TIMEOUT`, a user's or access's `DURATION` sides, an
  access jwt's `url:`, a bucket's `backend:`, api middleware args, and
  `config default`'s sides all sort behind the param/fn/module they
  name whenever that target defines in the same scope, whether the
  reader, the target, or both are raw. (A param read before its raw
  define rolls the migration back; an access `url:` read before it
  silently stores `NONE`.)
  `OVERWRITE` redefines and `IF NOT EXISTS` leaves existing state
  alone, so the defines are idempotent too — re-runs are safe for
  typed statements
  and single-statement raw bodies, while a multi-statement raw block
  re-pairs by its first header alone and its unpaired tail
  statements re-drift on re-runs (a known, tracked corner).
- **Removals last.** Children (fields, events, indexes) before their
  table, tables before the analyzer an index points at (the engine
  refuses to remove an in-use analyzer), then `REMOVE DATABASE`, then
  `REMOVE NAMESPACE`.
- **One transaction per scope section.** A scope's defines and
  removals share one `BEGIN TRANSACTION … COMMIT` frame — each
  content frame opens with a `## Scope` header block over a
  `-- section:` comment naming its scope and counts (a retired
  scope's frame labels its `REMOVE` instead) — and the scope's
  `USE` switch sits above the frame, as do the
  namespace and database owner defines (a failed content section
  never un-creates an owner). A failed statement rolls its own
  section back — sections before it stay applied. The file format
  wraps exactly one transaction, and a script spanning several scopes
  saves as a draft GROUP — one file per scope section
  ([below](#draft-groups-multi-scope-schemas)) — so every saved file
  still wraps exactly one. One
  boundary: a fulltext index over a table that already holds records
  builds at define and cannot see an analyzer the same transaction
  defines — let that analyzer converge first (the build defers on an
  empty table, so a fresh pair is fine).
- **`USE` lines** switch session context as scopes change, always
  outside the frames they precede.
- **`SURFORM WARNING` blocks** mark every data-destroying statement —
  `REMOVE TABLE` (the documents go with it), `REMOVE DATABASE`,
  `REMOVE NAMESPACE` (the whole subtree). Read the warnings before
  running.
- **`SURFORM REQUIRED` blocks** are the data-migration slots (above):
  an empty editable region before every removal that orphans data —
  a removed table's documents, a removed field's surviving values —
  and between the staged prelude and the final defines for every
  re-keyed table — with a runnable example line. Everything else
  (events, analyzers, plain clause retypes) carries neither block:
  nothing is lost.
- **`SURFORM NOTE` blocks are advice**, commented out and never
  executed — today, the staged table's index-rebuild offers (above).
  So is the one-line note above any UNIQUE index define whose table
  already exists database-side: the unique build is the one
  row-validating define (it fails if any two rows collide), and the
  note says when the index's fields cover re-keyed fields. A drifted
  define-once definition (a sequence, a secret-less user) carries a
  `-- note:` naming its manual repair for the same reason: the
  emission leaves the live definition alone, and the note is where
  the fix is stated.
- **The defines region says which families define once.** When a
  secret-less user or a sequence rides the script, the `## Defines`
  header explains that they emit `IF NOT EXISTS` and that live state
  (a credential hash, a counter) is never overwritten.
- **The header and the sections read as one page.** The script is
  laid out in ASCII markdown comments: a short header — what
  generated the script, the re-run safety, the marker vocabulary —
  closes with a `-- summary:` line (findings, defines, removals,
  data-migration slots, staged tables, transaction frames — the
  script's own counts — its defines half, staged set, and slot count
  after the SDL-side filters and the retired-scope collapse — can
  differ from the report's same-named numbers); every scope section
  and every region inside it (staged prelude, defines, index
  rebuilds, removals) opens with a rule-and-`##` header block; and
  blank lines separate every block. The header's one conditional
  stanza (the raw re-run corner) rides only the scripts that carry
  raw bodies; the staged contract rides the staged prelude's own
  header.

## The command

```sh
surform migrate generate schema.surdl \
  --info root=info-root.txt \
  --info ns=acme=info-ns.txt \
  --info db=acme/shop=info-db.txt
```

The `--info` tags are the same interchange files
[normalize](normalize.html) uses, and the migration covers exactly
the scopes the tags present — a scope nobody tagged makes no claim
either way. The tags need the file beside them — a lone `--info` is
a usage error.

The three input modes are mutually exclusive — the bare file is the
baseline ([above](#adopting-the-pipeline-the-baseline-migration));
the file with its `--info` tags compares against a live database;
`--from <file> --to <file>` compares two schema versions (above).
All print the same report, generate the same shape of script, honor
`--check` and `--save`, and exit 1 on drift in either direction when
nothing was saved (a successful `--save` exits 0). Every invocation
names exactly one complete mode: the CLI refuses anything less — a
bare run, a lone side, or a mix of the modes' flags — with the
usage error before anything loads.

## Pulling the interchange

The tags' files are a command's output now — the hand-rolled shell
loop's day is done:

```sh
surform migrate pull --endpoint http://127.0.0.1:8000 --ns acme --db shop
```

```
== pull ==
  surform-info-root.txt                written (INFO FOR ROOT)
  surform-info-ns-acme.txt             written (INFO FOR NS acme)
  surform-info-db-acme-shop.txt        written (INFO FOR DB acme/shop, 2 table(s))

next:
  surform migrate generate schema.surdl --info 'db=acme/shop=surform-info-db-acme-shop.txt'
  the wider compares: add --info root='surform-info-root.txt' and --info 'ns=acme=surform-info-ns-acme.txt' to the command above
  a scope the schema file does not manage classifies as removals (the server's root users among them) - read the drift report before running a wider compare's script
```

`pull` fetches the database's rendered INFO over the same endpoint
push applies through — `INFO FOR ROOT`, `INFO FOR NS`, `INFO FOR
DB`, and one `INFO FOR TABLE` per table, each statement its own
query — and writes the interchange files the `--info` mode consumes
(a full pull writes all three), the db file carrying every table's
child map under the reserved `"@tables"` key. The printed command is
the ready-made next step, database scope only: run it verbatim and
the compare reads what pull just fetched. The wider compares are
deliberate acts — the root and namespace files join through their
`--info` tags, named on the advice line — because a schema file does
not manage the server's root users or the default config (the
secrets rule keeps them out of SDL): whenever root-scope interchange
joins a compare, those statements classify as removals. Read the
drift report before running a wider compare's script.

A restricted credential pulls what it can read. The scopes leg still
sends the whole three-statement batch, and a scope the credential
cannot read answers the engine's permission refusal — so a
database-level user (the audience the bearer token serves) pulls its
database alone, a namespace-level user keeps its namespace and
database, and the printed advice names every refused scope and the
fact that a credential which can read them completes the pull. The
database scope is the pull's subject and never degrades: its
permission refusal refuses the run, and any other failure at any
scope refuses the run naming its scope — a mistyped `--ns`/`--db`
still says so.
Nothing but the INFO statements goes over the wire; nothing but
pull's own `surform-info-*.txt` files land on disk (the prefix keeps
hand-written interchange files out of the way — a re-pull refreshes
exactly its own names, nothing else). `--dir` moves where the files
land (created when missing; the printed lines name them there),
and the connection coordinates ride the same flags and environment
variables as push — `--endpoint`/`--ns`/`--db` over
`SURFORM_ENDPOINT`/`SURFORM_NS`/`SURFORM_DB`, the credentials as on
push (a Basic pair or a bearer token — never both). A refusal — a
dead endpoint, credentials that do not
verify, a mistyped `--ns`/`--db` — names itself and writes nothing.

## Saving drafts and committing them

The stdout script runs today; a migration you want to *keep* — fill
at your own pace, review, apply through your SurrealDB tooling —
lives in a file instead. `--save` writes it as a numbered draft and
prints the report, the file's path, and the next command:

```sh
surform migrate generate schema.surdl --info db=acme/shop=info-db.txt --save
# == drift ==
# …the report…
# == draft ==
# migrations/0001_schema.surql
# fill the SURFORM SLOT regions, then: surform migrate commit migrations/0001_schema.surql
```

A successful save exits 0 — the draft is the artifact, so the save
composes into the next command. (The drift exit belongs to
`--check`, and to the plain run when nothing was saved.) A converged
run prints one `(converged)` line and writes nothing.

The draft lands under `migrations/` as `NNNN_<name>.surql` — the
number is the highest existing draft's plus one (four digits, the
order an apply loop walks), and the name defaults to the SDL file's
stem, slug-folded (`schema-v2.surdl` → `schema-v2`); `--name` spells
it yourself and `--dir` moves the directory. A number collision — two
branches converged on one, or a race at the write — is an error,
never a silent bump or overwrite. (A script spanning several scopes
saves as a draft GROUP instead — one file per scope section,
`NNNNa_…`/`NNNNb_…` — [below](#draft-groups-multi-scope-schemas).)

The edit-regenerate loop has a pile-up guard. Regenerating a script
the directory already drafts prints a note and saves anyway —

```
note: an existing draft carries this exact script: 0001_schema.surql
```

— informational, because the stale draft may be exactly what your
fill-in-progress edits want to keep. When it is not,
`--save --replace <n>` takes the named draft's number instead of
stacking: the draft `<n>` on disk must carry THIS exact script (its
hash must match the regeneration), then it is removed and the new
draft saves under its number — the pile-up never grows, and a stale
draft cannot be committed out from under its replacement. A missing
number, a committed draft (history is not replaceable), or a
differently-scripted target refuses, both hashes named.

The draft's header is the migration's identity, and `migrate commit`
re-reads it later (a blank line separates it from the script's own
header below):

```
-- surform migration 0001 — schema
-- generated: 2026-10-07T18:04:05Z
-- source: schema.surdl
-- source-sha256: 3f2a…64 hex…
-- input: info db=acme/shop=info-db.txt
-- status: draft
```

The `-- input:` line records the diff's INPUTS, not just their class:
the `--info` mode spells its tag specs (`-- input: info root=… |
ns=<name>=… | db=<ns>/<db>=…`, as given), the version pair names both
files (`-- input: versions from=<file> to=<file>`), and a revert
drafts `from=(live) to=<sidecar>` (the live schema has no file
spelling). Paths carrying whitespace ride double-quoted; the parse
takes the extended forms back exactly, and the bare `info`/`versions`
spellings of pre-#75 drafts still parse.

(The `-- source-sha256:` line pins the source's canonical-form hash
at generate — [commit refuses a source edited in
between](#saving-drafts-and-committing-them).)

Inside the draft, every `SURFORM REQUIRED` slot region is delimited
by git-style **resolution markers** — bare text lines, not comments:

```
<<<<<<< SURFORM SLOT (db:acme/shop: table stale)
-- - Write the export, archive, or move query between the
--   markers; it runs before the removal, in this same
--   transaction.
-- e.g. INSERT INTO archive (SELECT * FROM stale);
>>>>>>> SURFORM SLOT END
```

An unresolved draft is deliberately un-runnable SurrealQL — the same
semantics as an unresolved git merge. Resolution is the whole point:
replace the delimited region with your query, markers gone. (The
`SURFORM WARNING` anchors are documentation and persist either way;
the stdout script keeps its comment banners and never renders
markers — those scripts run today.)

`migrate commit` turns a filled draft into the committed migration:

```sh
surform migrate commit migrations/0001_schema.surql -m "drop the stale table"
```

The command refuses — naming each unresolved slot — while any marker
remains, and refuses a draft carrying statements AFTER its transaction
frame (the script must be exactly one `BEGIN TRANSACTION; … COMMIT;`
block: the engine runs the frame and anything after it independently,
so a tail failure would leave the frame committed — its ledger row
self-recorded — while a push re-run skipped the file as already
applied, the tail never applying). It then does three things. It
**hashes the filled script**
(SHA-256 over its normal form — the bytes from `BEGIN TRANSACTION;`
through the final `COMMIT;`, newline-normalized — what you certified,
not the header and not the machinery below; the normalization is what
makes an unchanged file's re-commit recognize itself) and marks the
header `-- status: committed sha256:<hash> <message>`. Line endings
normalize at parse — a draft saved or edited with CRLF line endings
commits as clean LF, whatever it carried in. It
**appends the migration ledger's statements inside the script's one
transaction**, before the final `COMMIT;`: a
`DEFINE TABLE IF NOT EXISTS _surform_migrations` plus typed fields
(number, name, hash, message, applied_at, sdl) and one idempotent
record insert for this file — `INSERT IGNORE` keyed on the migration
number, so running the script twice writes one row, and applying it
through *any* SurrealDB tooling records itself. `applied_at` is
`time::now()`: evaluated when the script RUNS, so the ledger records
the true apply time. The ledger's `sdl` column holds the canonical
SDL of the state the migration converges to — `commit` re-loads the
header's `-- source:` file (it must be present and clean) — which is
the revert substrate (the ledger's recorded state can regenerate a
migration later). The `-- source:` path is recorded as given and
re-loaded from the directory the `commit` invocation runs in: commit
from the same root you generated from, or record an absolute path.
`--ledger-table` renames the ledger table everywhere it appears (the
default `_surform_migrations` is a convention, not a law).

Two conventions the machinery assumes. **Migration numbers are
unique per database, not just per directory**: the ledger's record id
is the number, so two different directories' `0001`s applied to one
database collide — the second script's self-record silently skips.
Keep one migrations directory per database. And **do not edit the
appended ledger region** — from the banner line through the final
`COMMIT;` — not just because it is tool-owned text: edits there are
invisible to the hash (the region is excluded by construction), so
they surface at apply time as whatever the edited statements do,
never as a drift report.

Commit is once per file. A second commit refuses: unchanged, "already
committed"; edited, "edited after its commit" — the ledger keeps the
recorded hash, so a post-commit edit to the SCRIPT is a mismatch
[`migrate status` surfaces](#checking-status) (below); `push`, also
below, is the apply half.

The draft's header also pins the SOURCE it was generated against:
`-- source-sha256:` records the source SDL's canonical-form hash at
generate time, and commit re-canonicalizes the re-loaded source and
refuses a mismatch (naming both hashes) — without the pin, a schema
tweaked between generate and commit would silently certify: the
script converges the old shape while the ledger's revert substrate
describes the new one. Drafts predating the line carry no pin and
commit as before. The recorded source must also be RE-READABLE at
commit — it is re-loaded (from this directory; the path is recorded
as given) to store the ledger's `sdl` — and a missing file refuses
naming the mechanism and both ways out (commit from the generation
root, or record an absolute path).

Commit also refuses a draft whose FILE NAME carries a different
number than its header (`0009_…` with a header saying `0001` — the
natural git-merge rename). Push walks file names while the ledger
self-records the header's number, so committing the mismatch would
split the identity into permanent phantom drift: status would show a
pending `0009` and a ledger row `0001` with no local file. The repair
is `surform migrate renumber <file> <n>` — the header's number line
and the file name move together, the one operation that cannot split
the identity. Renumber refuses a COMMITTED migration (the number is
the ledger's record id), a number another draft already carries, and
a number outside the four-digit convention (1–9999).

### Draft groups (multi-scope schemas)

A schema spanning several namespaces and databases saves as a draft
GROUP — the layouts SurrealDB itself encourages: `generate --save`
splits the script at its transaction frames (one per scope section)
and writes ONE DRAFT PER SCOPE SECTION under one number, the letters
ordering the group:

```
migrations/0005a_schema.surql
migrations/0005b_schema.surql
migrations/0005c_schema.surql
```

Each file is exactly one transaction — the file format's invariant
holds per member — and carries its scope's own slot regions (the
script's comment header rides every member; each member's frame
carries its own hoisted `USE` switch and any owner defines its scope
needs). The members share ONE header identity — the same `-- source:`,
`-- source-sha256:`, `-- input:`, `-- generated:` — plus a line of
their own:

```
-- group: 0005 a/3
```

The number is the GROUP's number — every member's, and the ledger
row's: the once-per-number contract holds, number 5 IS the group. The
name order is the apply order (`0005a` < `0005b` < `0005c`), and the
save's stdout names every member with its commit command, in that
order. A group's members are not rewritten one at a time: `--replace`
refuses them, `renumber` refuses them, and the pile-up note stays a
single-draft feature — delete a stale group by hand and regenerate (a
fresh save takes a fresh number).

Committing follows the letters. A member's file name must carry its
header's letter (the name order is push's walk order; the header's
letter decides which member records), and an earlier member still an
uncommitted draft refuses a later member's commit — the group applies
whole, so committing the tail first would let a push record a group
whose head never shipped. The earlier members' commits mark their
headers but write NO ledger region; the LAST member's commit splices
the ledger block — the group records ONCE (the number, the LAST
member's filled hash, the base slug).

Push walks the members in name order and applies them as one run: an
unapplied group applies EVERY member — a mid-group failure leaves the
earlier members applied but unrecorded (the row is the last member's
to write), and the re-run re-applies them (`DEFINE … OVERWRITE` makes
that idempotent — and the failure report says so: a data-migration
query you filled into an earlier member RUNS AGAIN on the re-run, so
keep those queries idempotent); an applied group — the ledger holds the
number with the last member's hash — skips EVERY member. A group only
PARTLY committed blocks the run (a real push refuses; a dry run
classifies it `blocked`): the row could otherwise claim a group whose
head never shipped. A structurally broken group — a missing letter, a
member count disagreeing with the headers' total, a name/header
disagreement (a lettered file name over a headerless draft, either
direction) — refuses before anything is sent.

Status drives a member's verdict from the GROUP's row through the
recording member — applied, pending, or (the last member edited after
the apply) DRIFTED, both hashes named, the earlier members applied —
and the rows render grouped with their positions. The one corner that
verdict cannot see: an EARLIER member edited after the group applied
still renders applied (the row carries only the last member's hash) —
push's pre-flight drift check is the guard there (every committed
member must still hash to its own header's record before anything
applies). Two hand-crafted states sit outside the tool's own flows,
both refused or inert rather than silent where the machinery sees
them: a `USE` line hand-edited onto a committed EARLIER member rides
above its frame — outside the drift hash — and would apply its
statements to the scope it names (nothing checks it; the hash cannot);
and committing past a hand-DELETED middle member succeeds — commit
certifies one file, and the broken letter run refuses at the next
push, before anything is sent.

## Pushing to a live database

`migrate push` applies the directory's committed migrations to a live
database — in number order, each file in one HTTP call, skipping what
the ledger already records, stopping at the first failure:

```sh
surform migrate push --endpoint http://127.0.0.1:8000 \
  --ns acme --db shop --user acme-admin
```

Every connection coordinate falls back to its environment variable
when the flag is absent — `SURFORM_ENDPOINT`, `SURFORM_NS`,
`SURFORM_DB`, `SURFORM_DB_USER`, `SURFORM_DB_PASS`, `SURFORM_DB_TOKEN`
(the recommended form for secrets: a flag lands in shell history; a
flag over a variable wins, and a present-but-empty variable counts as
given). The credentials come in two EXCLUSIVE kinds — a Basic pair
(`--user`/`--pass`, give both or neither; neither speaks to a server
run with `--unauthenticated`) or a bearer token (`--token`, sent as
`Authorization: Bearer <token>`). The token is one YOU minted — a
single `POST /signin` with the database-level credential; the run
carries it verbatim and never mints or refreshes it, so a 401 means
mint a fresh one and re-run. Giving a token alongside a user or
password refuses before anything dials, and on a terminal a Basic run
with a user but no password ASKS (`password:` on stderr; it is read
plainly — std offers no echo suppression — and no credential ever
appears in any output, on any path).

The transport is SurrealDB's raw HTTP `POST /sql` endpoint — the
connection surface the docs pin: the server ROOT goes in `--endpoint`
(`/sql` is appended), `--ns`/`--db` ride the `Surreal-NS`/
`Surreal-DB` headers, and the response is read as the documented
per-statement result array. Before anything is sent, push reads the
ledger — `SELECT number, name, hash FROM <ledger>;` — and walks the
directory:

- a file the ledger records with the SAME hash is **already
  applied** — skipped;
- a file the ledger records with a DIFFERENT hash refuses the run —
  another migration owns that number on this database (one migrations
  directory per database);
- an uncommitted draft is **skipped — draft**: commit declares a
  migration part of the database's history; push ships committed
  history;
- a draft GROUP applies whole ([above](#draft-groups-multi-scope-schemas)):
  every member in name order, the row recorded once by the last; a
  group only partly committed blocks the run, and an applied group
  skips every member;
- a committed file that no longer hashes to its header (edited after
  its commit) refuses the WHOLE run before anything is sent;
- a committed file carrying statements OUTSIDE its one transaction
  frame — content after the `COMMIT;` close, or a second
  `BEGIN … COMMIT` block — refuses the run before anything is sent
  (the rollback guarantee below needs the file to be exactly one
  transaction; commit refuses the same shape, so a tool-committed
  file never carries one);
- everything else applies in number order.

Each apply POSTs the file's full text — the script's own
`BEGIN TRANSACTION; … COMMIT;` wraps it, so a failed statement rolls
the whole file back and the engine saw nothing. The first failure
stops the run with the engine's own error message and the count
applied before it; fixing the file and re-running resumes —
standalone files skip by the ledger, while a draft group's earlier
members re-apply ([above](#draft-groups-multi-scope-schemas); the
failure report says so). That mid-apply stop renders as plain
multi-line stderr even under `--json` — the one failure report the
JSON shape does not cover (every pre-flight refusal JSON-ifies; the
stop is the engine's own report, quoted). A missing ledger table — a
fresh database — reads as an empty ledger, so the first push applies
everything and each file's ledger block creates the table as it goes.

Two things the pipeline checks so you don't discover them at apply
time. A script whose text carries its own `USE NS … DB …` statement
targeting OTHER names refuses — the whole file is scanned, the
`USE` lines above the transaction frames included: a `USE` switches
the session
mid-request (verified against 3.3.0), so the migration — and its
ledger self-record — would land in a different database than the one
push reads the ledger from; point `--ns`/`--db` at the names the
schemas declare. A draft group's members are the deliberate
exception in part: each earlier member carries its OWN scope's `USE`
(that is the split's point — every request is its own HTTP session,
so the switch never leaves its file), while the LAST member — the one
whose script writes the ledger row — keeps the full rule and must
name the push's target. And a ledger table push cannot find is only an empty
ledger when the not-found names the TABLE — a not-found on the
namespace or database (a mistyped `--ns`/`--db`) and a permission
failure are errors, never a silent "nothing applied".

`--dry-run` answers "what would this push do?" without answering it
with the database: the entire pre-flight runs — the directory walk,
the drift checks, the one ledger SELECT — and the plan prints,

```
== push (dry run — nothing was sent) ==
  0001_add-users                       already applied
  0002_add-indexes                     would apply
  0003_work-in-progress                skipped — draft (commit it first)

dry run: 3 migration file(s) — 1 would apply, 1 already applied, 1 draft(s) skipped; zero statements sent
```

— with ZERO migration POSTs (the SELECT is the only request). A
collision the real push would die on — the ledger recording a number
with a different hash — rides the plan as a would-refuse row (both
hashes named), a partly-committed group as a `blocked` row, and either
exits 1, so the dry run works as a CI gate; every
pre-flight refusal (drift, a tail-bearing file, a body-`USE` mismatch)
refuses it exactly as it refuses a push.

## Checking status

`migrate status` compares the migrations directory against the live
ledger — one ledger SELECT through the same endpoint push
applies through, and status never applies anything:

```sh
surform migrate status --endpoint http://127.0.0.1:8000 --ns acme --db shop
```

```
== status ==
  0001_add-users                       applied
  0002_add-indexes                     pending
  0003_fix-thing                       DRIFTED — file sha256:9f2c7a1b3d4e…, ledger sha256:c3d4e5f6a7b8…
  0004_work-in-progress                draft (commit it first)
  0005_lost-migration                  in the ledger, no local file

5 migration file(s): 1 applied, 1 pending, 1 draft, 1 drifted; 1 ledger row(s) without a local file
```

- **applied** — the ledger records the number and the file's current
  script still hashes to what the database recorded.
- **pending** — committed, not yet pushed: the normal state between
  commit and push, not a problem.
- **DRIFTED** — the ledger records the number with a DIFFERENT hash:
  the file was edited after the database applied it (or another
  directory's migration owns the number — both hashes named). Push
  refuses a drifted file outright; status shows it before push gets
  there.
- **draft** — no committed header yet.
- A draft group's members render grouped, each verdict naming its
  position (`applied — group 0005 a/2`); the verdict is the GROUP
  row's, judged through the last member ([above](#draft-groups-multi-scope-schemas)).
- and ledger rows the directory has no file for — history the
  directory lost (a deleted file, a branch without it).

The exit code is the CI gate for history integrity, not progress:
exit 0 when directory and ledger agree, exit 1 when anything is
DRIFTED or missing —

```sh
surform migrate status --endpoint http://127.0.0.1:8000 --ns acme --db shop \
  || echo "migration history is inconsistent!"
```

`--dir` and `--ledger-table` thread exactly as on `push`. Without an
endpoint (flag or `SURFORM_ENDPOINT`) the run is OFFLINE — half of
status is local state, and a dead endpoint cannot take it away:

```
== status ==
  0001_add-users                       committed — ledger state unknown (no endpoint)
  0002_work-in-progress                draft (commit it first)

2 migration file(s): 1 draft, 1 committed (offline — the ledger was not read, no endpoint)
```

Nothing dials, nothing authenticates (no credentials are asked for),
and the exit is 0 always — nothing can be DRIFTED or MISSING without
a ledger to compare against.

## Reverting a migration

`migrate revert` is a convenience over generate, not a down-chain: it
drafts the migration that undoes a recorded one, and the revert then
travels the same road as any migration — fill, commit, push.

Every committed migration's ledger row carries the canonical schema
the migration converged TO. Name a row by its number or its hash's
prefix:

```sh
surform migrate revert --to 1 --endpoint http://127.0.0.1:8000 --ns acme --db shop
```

Revert fetches the live database's schema — `INFO FOR DB` plus each
table's `INFO FOR TABLE`, the same interchange [`--info`
reads](#the-command), over the same endpoint — and drafts the
convergence from there to the recorded state: the same script
`migrate generate --from <live> --to <recorded>` would emit, with
data-migration slots riding wherever the recorded state orphans data.
Two files land in the migrations directory: the next numbered draft
(`0003_revert-to-0001-add-users.surql`) and a `.surdl` sidecar
holding the recorded state verbatim. The sidecar is the draft's
`-- source:`, which is what makes the revert committable: `migrate
commit` re-loads it into the new ledger row's `sdl` column — exactly
the recorded state — so reverting the revert works, wherever the
working tree has drifted since. The sidecar must SURVIVE until that
commit (it is the draft's source — deleting it first fails the
commit at the source load) and is safely deletable once the row is
committed (the ledger then carries the recorded state itself).
Nothing is applied: fill the slots, commit, push.

Two boundaries; both refuse rather than silently no-op. The
LEDGER TABLE itself never joins the compare — it is the pipeline's
own machinery (every migration's self-record creates it), so no
revert can draft its removal and wipe the history it is about to
append to. And the revert compares at DATABASE scope: the recorded
schema must declare `--ns`/`--db`'s database (a namespace-free
schema compares under its database name), and root- or
namespace-level drift stays out of its view — the ledger is a
database-level history.

## Where next

- [Surform inside SurrealDB](in-database.html) — the same pipeline as
  a Surrealism module: migrate from within SurrealQL.
- [Adopting a database](normalize.html) — when the database wins,
  merge it into the file instead.
- [The CLI](cli.html) — the migrate command's flags and exits.
