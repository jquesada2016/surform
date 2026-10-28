Six commands, one job each — `migrate` carries six subcommands.
Output conventions are uniform across all of them: **DDL and schema
text go to stdout; diagnostics and lint findings go to stderr; a clean
run prints nothing on stderr and exits 0.**

| command | job | exits 1 when |
|---|---|---|
| `surform check` | parse and report | the file has diagnostics |
| `surform build` | emit DDL | the file has diagnostics |
| `surform fmt` | print canonical SDL (`--check`: verify without writing) | the file has diagnostics; `--check`: the input is not canonical |
| `surform normalize` | merge database state into the file | diagnostics, bad `--info` tags (a missing tag is a usage error — exit 2) |
| `surform migrate generate` | report drift + convergence script | drift exists in either direction and nothing was saved — a successful `--save` exits 0 |
| `surform migrate commit` | hash + ledger-record a filled draft | unresolved slots, re-commit, source unreadable, the file name's number differing from the header's |
| `surform migrate renumber` | move a draft to another number — header and filename together | the draft is committed, unreadable, or malformed; the number is taken (or outside 1–9999) |
| `surform migrate push` | apply committed migrations over HTTP `/sql` | an apply failed, drift, a pre-flight refusal — a `--dry-run` exits 1 on its would-refuse row |
| `surform migrate status` | the directory against the live ledger | anything DRIFTED or missing from the ledger |
| `surform migrate revert` | draft the migration toward a recorded state | an unreadable target, an ambiguous row, a schema that never declares `--ns`/`--db` |
| `surform migrate pull` | fetch the database's rendered INFO as the interchange files `--info` consumes | a wire refusal (a bad endpoint, credentials, a failed INFO), a name that cannot ride a file, an unwritable file |
| `surform lint` | advisory warnings | any finding exists |

Global flag: `--json` renders every diagnostic and finding as one
JSON object per line — machine-friendly for editors and CI. The
migrate pipeline's refusals render as one JSON object too —
`{"error": …, "hint": …}`: the refusal's sentence as the error, its
fix (the text after the ` — `) as the hint. The pipeline's SUCCESS
reports (the drift report and script, the draft banner, push/status/
revert output) stay plain stdout even under `--json`.

On a composition root (a file with `include` statements — see
[Multi-file schemas](includes.html)), `check`, `build`, `migrate`,
and `lint` follow the includes and work on the whole tree; every
diagnostic and finding renders against the file it came from. `fmt`
formats the root as authored without resolving anything, and
`normalize` writes the whole tree back — `--in-place` returns every
merged statement to its home file, without it the flattened form
prints.

## check — validate

```cli-help cmd=check
```

A clean file prints nothing and exits 0. A broken
file gets line-anchored diagnostics:

```session cmd=check src=broken exit=1
```

Read the anatomy: the **code** links to the spec section that defines
the rule; the snippet points at the exact line and column; the `help:`
line suggests the next step. With `--json` the same information is one
JSON object per line on stderr.

Stdin is `fmt`'s alone — `check` and `build` read a file only, and
that asymmetry is deliberate: formatting a pathless buffer is the
pipe's job (`surform fmt` with no file, or `-`), while validation runs
the resolution pass, which needs the file's root to follow includes
and spreads — a stdin `check` would silently run the weaker
pathless parse and look like a guarantee it isn't.

## build — compile to DDL

```cli-help cmd=build
```

On success the SurrealQL DDL prints to stdout — pipe it wherever your
SurrealQL goes:

```sh
surform build schema.surdl > schema.surql
surform build schema.surdl | surreal sql --conn ws://localhost:8000 --user root --pass … --ns acme --db shop
```

A dirty file emits no DDL at all — you never apply half a schema.

## fmt — canonical style

```cli-help cmd=fmt
```

`fmt` prints the file's canonical form; `--in-place` rewrites in
place instead — and on a [composition root](includes.html) it
rewrites the whole tree, every participating file formatting from its
own loader parse. Formatting has a fixed point — running
it again changes nothing — which makes it safe to run in CI as a
check:

```sh
surform fmt --check schema.surdl   # exit 1 + the diff it would apply
surform fmt --in-place schema.surdl
git diff --exit-code   # the older dance --check replaces
```

`--check` is that gate in one command: exit 0, silent, when the
input is already canonical; exit 1 with a diff of what would change
on stderr (finding-class output — stdout stays empty; the diff stays
plain text under `--json` — it is not a diagnostic). The
`--max-width` budget is part of the verdict, and the pipe is an input
here too (`surform fmt --check < schema.surdl`); `--in-place` cannot
ride along — one refuses to write, the other's whole job is writing.

One input is refused in every mode: a generated migration script (a
file whose first line is the `-- surform migration` draft header
`migrate generate --save` writes). `--` is SurrealQL comment syntax,
not SDL's, and fmt does not reflow migration scripts — the schema to
format is the draft header's `-- source:` file.

The pipe is an input too: with no file (or `-`), `fmt` reads stdin
and prints the canonical form — the interface editors drive when they
format an unsaved buffer:

```sh
surform fmt < schema.surdl
```

`--max-width N` moves the wrap budget long lines break under (default
100). Details and before/after examples: [Formatting](fmt.html).

## normalize — adopt the database

```cli-help cmd=normalize
```

`normalize` merges the database's rendered `INFO`
output — handed over as interchange files — into your schema file, so
an existing database becomes a Surform file:

```sh
surform normalize schema.surdl --in-place --info db=acme/shop=info-db.txt
```

At least one `--info` tag is required — with none there is nothing to
adopt, and the run would silently be a disguised `fmt` (with
`--in-place`, a rewrite); the command refuses at the CLI instead.
`fmt` is the no-merge path. Scope tags, the interchange format, and
the full walkthrough: [Adopting a database](normalize.html).

Mixin-carrying files merge [spread-preserving](mixins.html): a
member whose database state matches its definition stays covered by
the spread, a diverged one materializes as the explicit override,
and `migrate generate` reports drift on the member itself.

## migrate generate — what drifted, and the script

```cli-help cmd=migrate
```

`migrate generate` compares the file with the database's rendered
`INFO` and prints two sections: the drift report and the convergence
script — defines first (`DEFINE … OVERWRITE` from your file's side,
`IF NOT EXISTS` for the define-once families),
removals last (`IF EXISTS`, the data-destroying lines under
`SURFORM WARNING` blocks), one transaction per scope section with the
`USE` switches outside the frames. It exits
1 when drift exists in either direction — the CI-friendly signal,
report-only with `--check`. A converged run prints one
`(converged)` line and exits 0. The `--from <file> --to <file>` input
mode swaps the database side for a second schema version — the same
report and the same script shape, schema version against schema
version:

```sh
surform migrate generate schema.surdl --info db=acme/shop=info-db.txt --check || echo "schema drifted!"
surform migrate generate --from schema-v1.surdl --to schema-v2.surdl
```

The bare file is the third input, the baseline: the file is the whole
desired state and the FROM side is empty, so every definition rides
apply-pending and the script defines from nothing - the first
migration of a fresh pipeline in one command, with no database
reading and no scaffolding files:

```sh
surform migrate generate schema.surdl --save
```

`--save` writes the script as a numbered draft under `migrations/`
instead of printing it — a script spanning several scopes saves as a
draft GROUP instead (one file per scope section, `NNNNa_…`/`NNNNb_…`,
applied in name order — the full story in
[Schema migration](migrate.html)). The stdout
names the file and the next move (`fill the SURFORM SLOT regions,
then: surform migrate commit <file>`), and a successful save exits 0
— the draft is the artifact; the drift exit 1 belongs to `--check`
and to the plain run when nothing was saved. Regenerating a script
the directory already drafts prints
`note: an existing draft carries this exact script: <file>` and
saves anyway (informational — the stale draft may hold your
fill-in-progress edits); `--save --replace <n>` is the take-over:
the draft `<n>` on disk must carry this exact script (its hash must
match), then it is removed and the new draft saves under its number —
a missing, committed, or differently-scripted target refuses (a
group's members are not replaceable — delete a stale group by hand
and regenerate).
`migrate commit` hashes the filled draft
and appends the migration ledger's statements inside its transaction —
a draft carrying statements after its `COMMIT;` frame refuses (commit
wraps exactly one transaction) — the full draft → fill → commit story
in [Schema migration](migrate.html).

## migrate push — apply the committed migrations

`migrate push` walks the migrations directory in number order and
applies each committed file to a live database through SurrealDB's
HTTP `POST /sql` endpoint — the ledger-recorded ones skip, the first
failure stops the run with the engine's error and the count applied
before it:

```sh
surform migrate push --endpoint http://127.0.0.1:8000 \
  --ns acme --db shop --user acme-admin
```

Every connection coordinate — `--endpoint`, `--ns`, `--db`, and the
credentials — falls back to its environment variable when the flag is
absent (`SURFORM_ENDPOINT`/`SURFORM_NS`/`SURFORM_DB`/
`SURFORM_DB_USER`/`SURFORM_DB_PASS`/`SURFORM_DB_TOKEN` — the
recommended form; a flag over an environment variable, and a
present-but-empty variable counts as given). The credentials are
either a Basic pair (`--user`/`--pass`) or a bearer token (`--token`,
sent as `Authorization: Bearer <token>`) — never both; the token is
one you minted (a single `POST /signin` with the database-level
credential), carried verbatim and never refreshed. On a terminal, a
Basic run with a user but no password asks for it (`password:` on
stderr); the answer never prints anywhere, and a non-interactive run
without one keeps the give-both-or-neither
refusal. `--dir` moves the migrations directory and
`--ledger-table` threads the ledger read, exactly as on `commit`.
Each file applies in one HTTP call, its own transaction wrapping it —
a failed statement rolls the whole file back. `--dry-run` answers
"what would this push do?" without answering it with the database:
the entire pre-flight runs (directory walk, drift checks, the one
ledger SELECT) and the plan prints — would-apply / already-applied /
skip-draft / would-refuse, that last one exiting 1 — with ZERO
migration POSTs. The apply loop's rules,
the fresh-database case, and the body-`USE` guard:
[Schema migration](migrate.html).

## migrate status — the directory against the ledger

`migrate status` classifies every migration file against the live
ledger — applied (hash match), pending (committed, not yet pushed),
draft, DRIFTED (the ledger records the number with a different hash —
both hashes named) — plus ledger rows the directory has no file for:

```sh
surform migrate status --endpoint http://127.0.0.1:8000 --ns acme --db shop
```

One ledger SELECT through the same endpoint push applies through;
status never applies anything. The connection coordinates fall back
to the environment exactly as on `push` — and with NO endpoint (flag
or environment) the run is OFFLINE: the local classification only
(draft and committed per file, the ledger-derived states rendering as
`unknown (no endpoint)`), exit 0 — nothing can be DRIFTED without a
ledger. Exit 0 when directory and ledger
agree, 1 when anything is DRIFTED or missing — the CI gate for
history integrity. The states and the exit rule:
[Schema migration](migrate.html).

## migrate revert — draft the way back

`migrate revert --to <number|hash-prefix>` drafts the migration from
the LIVE database's schema to a recorded row's schema — the same
script `generate --from <live> --to <recorded>` would emit, slots
riding wherever the recorded state orphans data. The draft (plus a
`.surdl` sidecar carrying the recorded state verbatim — the draft's
`-- source:`, so a later commit re-records exactly that state) lands
in the migrations directory and NOTHING is applied: fill, commit,
push — the revert travels the normal road. The ledger table itself is
immune (no revert can draft its removal); the boundaries and the
round trip: [Schema migration](migrate.html).

Live examples: [Schema migration](migrate.html).

## migrate pull — the interchange, fetched

`migrate pull` is the interchange's producer: the database's rendered
`INFO FOR ROOT`, `INFO FOR NS`, and `INFO FOR DB` — plus one `INFO
FOR TABLE` per table — ride the same HTTP `POST /sql` endpoint push
applies through, and the interchange files `normalize` and
`migrate generate --info` consume land in `--dir` (the db file
carrying every table's child map under the reserved `"@tables"`
key). The ready-made next command prints — the database scope's tag,
with the wider compares (root, ns) named as deliberate acts on the
advice lines under it:

```sh
surform migrate pull --endpoint http://127.0.0.1:8000 --ns acme --db shop
```

Nothing but the INFO statements ever goes over the wire, and nothing
but `surform-info-*.txt` files land on disk (a re-pull refreshes
exactly the names it writes — hand-written interchange files are not
pull's to touch). A pull under a restricted credential writes only
the scopes that credential can read: a scope's permission refusal
drops it (no file; the advice line names the fuller credential that
completes the pull), any other refusal refuses the pull naming its
scope, and the db scope never degrades. The connection coordinates
fall back to the
environment exactly as on `push`
(`SURFORM_ENDPOINT`/`SURFORM_NS`/`SURFORM_DB`, the credentials as on
push — a Basic pair or a bearer token, never both), and
`--dir` moves where the files land; the printed lines name them
there. The whole story: [Adopting a database](normalize.html).

## lint — the advisory pass

```cli-help cmd=lint
```

Findings are warnings, not errors — your file applies fine with them.
Exit 1 when any exist. The catalog: [The linter](lint.html).

## Where next

- [Getting started](getting-started.html) — the commands in a
  walkthrough.
- [How examples are verified](verification.html) — the pipeline
  behind the example panels.
