Surform is also a [Surrealism](https://surrealdb.com/docs) module: the
same compiler, packaged for in-database dispatch. Instead of piping
DDL through a CLI, a database that has the module registered runs the
pipeline on itself — `RETURN fn::surform::migrate($schema);` migrates
the database from inside SurrealQL.

This page is the working-today flow: get the module, register it,
install the migration snippet, and migrate from any SurrealQL session.
Surrealism is experimental in SurrealDB 3.3 — the server needs
`--allow-experimental=surrealism` (and `files` for the bucket below)
[per the platform docs](https://surrealdb.com/docs/surrealql/statements/define/module).

## The two artifacts

One source, two `.surli` archives — the [crate
README](https://github.com/jquesada2016/surform/blob/main/surform-surrealism/README.md)
carries the full contract:

| artifact | capabilities | exports |
|---|---|---|
| `surform.surli` | zero | `compile`, `converge`, `status` |
| `surform-apply.surli` | the grant surface (below) | all of those, plus `apply` |

The default artifact is pure: it makes no host calls and every output
is reproducible from its inputs alone. The apply variant is what lets
a database migrate itself — read
[the security section](#the-security-posture) before registering it.

## Getting the artifacts

Both `.surli` files are release assets — attached to each
[GitHub release](https://github.com/jquesada2016/surform/releases)
with their SHA-256 checksums in the notes. They are byte-deterministic:
rebuilding from the same source produces the same bytes, so the
checksum doubles as the verification — build and compare before
registering:

```sh
# from a checkout of the release tag; ~1.4 MB each
surreal module build --out target/surform.surli surform-surrealism
surreal module build --out target/surform-apply.surli surform-surrealism-apply
```

(The builds need `rustup target add wasm32-wasip2` and the `surreal`
CLI 3.3.0; the repo carries no CI — releases are cut locally with
exactly these commands.)

## Registering the module

The `.surli` rides a file bucket: create the bucket, write the archive
directly into the backend directory (the `modules:` in the load path
below is the bucket's name, not a folder — a `modules/` subdirectory
inside the backend is not found; megabyte bodies are refused on the
`/sql` route), then `DEFINE MODULE`:

```sql
DEFINE BUCKET modules BACKEND 'file:<dir-under-your-files-allowlist>' PERMISSIONS FULL;
DEFINE MODULE mod::surform FROM f"modules:/surform.surli" UNSIGNED PERMISSIONS FULL;
-- the apply variant, in the database that opts into it:
DEFINE MODULE mod::surform_apply FROM f"modules:/surform-apply.surli" UNSIGNED PERMISSIONS FULL;
```

`UNSIGNED` is mandatory for bucket loads; `PERMISSIONS FULL` on the
`DEFINE MODULE` gates who may CALL the module. The names
(`mod::surform`, `mod::surform_apply`) are the definer's choice — the
two packages' internal identities (`surform/surform` vs
`surform/surform-apply`) are distinct on purpose, so the artifacts can
never alias. The snippet below spells the names as registered here.

What each artifact may DO is its own manifest, enforced by the host —
the apply artifact's grant surface is the narrowest the flow runs
under: `allow_arbitrary_queries` (the host `sql` import), plus
`allow_functions = ["session::ns", "session::db", "time::now"]` for
the scope pre-flight and the ledger's `applied_at`. Nothing wildcarded,
no scripting, no network. Verify an archive before registering:

```sh
tar --zstd -xOf target/surform-apply.surli surrealism/surrealism.toml | sed -n '/\[capabilities\]/,/\[attach\]/p'
```

## The migration snippet

The snippet is a first-class artifact of this repo — a small SDL file
you append to your schema. Its functions are ordinary schema (they
migrate like everything else) and they wrap the module's exports into
the functions you call. It comes in the two artifact twins — pick by
the module you registered:

```sdl no-exec src=surform-migrate
```

```sdl no-exec src=surform-migrate-apply
```

Append the variant's text to your `schema.surdl` (after your
`namespace`/`database` lines) — the file itself is header-less, so it
cannot be migrated as a document of its own. What the wrappers do:

- `fn::surform::compile($sdl)` — SDL text → DDL, byte-identical to
  `surform build`.
- `fn::surform::capture()` — the rendered INFO of the session's
  database assembled into the module's live-info carrier: the
  registration and bucket sections dropped, the pipeline's ledger
  table dropped, each table's `INFO FOR TABLE` nested under the
  reserved `@tables` key (full fidelity — field-level drift visible).
- `fn::surform::converge($sdl)` — the convergence script for this
  database (the CLI's `migrate generate --info`, in-database); empty
  string means converged.
- `fn::surform::ledger_rows()` — the ledger SELECT's response body,
  bridged in-database for `status` (a database with no ledger yet
  bridges as the empty ledger).
- `fn::surform::status($files)` — the `migrate status` report; the
  `$files` carrier is the migrations directory serialized as a JSON
  array of `{"number", "name", "committed", "hash"}` entries (hash
  present iff committed) — the caller's duty, since neither the
  module nor the database can see your filesystem.
- `fn::surform::migrate($sdl)` — the two-artifact contract in one
  name. Against the zero-capability artifact it returns the
  convergence script for you to run (empty string: converged).
  Against the apply artifact it converges AND executes in one call.

## Migrating from within SurrealQL

Hand the wrapper your schema's text — inline, or bound however your
SQL client binds strings. The module reads no files: the text must be
composed, self-contained SDL. An `include`-bearing document refuses
(SDL-E0008 — the Surrealism runtime grants the guest no filesystem, so
an include read never resolves); a multi-file root travels as the one
file you would get by pasting the tree together
([multi-file schemas](includes.html)), and the
[CLI pipeline](migrate.html), which follows includes, stays the
multi-file road.

```sql
RETURN fn::surform::migrate("namespace acme; database shop; table user { schemafull; field email: string; };");
```

Against the apply artifact the outcome string comes back:
`applied 2 statement(s); recorded 0001_apply in the ledger` (the
ledger row — `NNNN_apply`, the previous highest plus one, the SDL it
converged to in its `sdl` column — records itself, so
[migrate revert](migrate.html#reverting-a-migration) stays meaningful
for apply rows). A database already at the SDL's shape answers
`(converged) — nothing to apply, the ledger untouched`. Re-runs are
safe: convergence is idempotent, and the whole call is
one transaction — a failed apply rolls back everything it did,
ledger row included.

Against the zero-capability artifact the script comes back instead —
run it yourself (any SurrealQL tool), or let
[the CLI's pipeline](migrate.html) carry it as a committed migration.

Dry runs and audits ride the same wrappers on either artifact:
`fn::surform::converge($sdl)` is the script `migrate` would apply,
`fn::surform::status($files)` the drift report between your migrations
directory and the ledger.

## The security posture

> **Treat every module as privileged code that can affect data your
> database can access.** Sandboxing limits host access, but a module
> can still implement logic that leaks or corrupts data if you expose
> it to the wrong scopes.
> — [SurrealDB's module-architecture
> documentation](https://surrealdb.com/docs/learn/extensions/guides/module-architecture),
> verbatim.

The zero-capability artifact cannot act on that warning — it makes no
host calls. The apply artifact is exactly that class of module, and
three properties bound it (none optional; the [crate
README](https://github.com/jquesada2016/surform/blob/main/surform-surrealism/README.md#security)
documents them with the probes that pinned each):

- **No escalation, ever.** Every statement executes at the invoking
  session's auth — a caller who may not `DEFINE TABLE` gets an apply
  that cannot either. Whoever can call the module can already do
  everything it does.
- **The caller's session IS the scope.** The SDL must declare the one
  database the session is scoped to, or apply refuses before anything
  is captured or run.
- **Apply is additive-only, literally.** It defines and never
  removes: any `REMOVE` statement in a convergence refuses (by name),
  and so does any script carrying the `SURFORM WARNING`/`REQUIRED`
  markers — the fill-in flow needs a human the module cannot pause
  for. The destructive class stays
  [the CLI's flow](migrate.html#the-data-migration-slots); the escape
  for a refused removal is your own session: remove it yourself, then
  re-apply.

## Working with the CLI

The module and the CLI share one ledger and one capture format, with
two seams:

**Migration numbers are unique per database.** An apply row named
`0001_apply` means the next CLI migration in a fresh migrations
directory — which cannot see the ledger when it drafts — must be
numbered past it, or the database refuses it at `push`. The refusal,
executed against this exact setup, reads: `the ledger already records
number 0001 with hash sha256:a6f3…, but
migrations/0001_live-v3.surql hashes sha256:6ecc… — another migration
owns this number on this database (one migrations directory per
database); status can compare them`. Nothing has happened to the
database at that point — push refuses before it sends anything — and
`migrate status` shows the collision as a DRIFTED pairing (the file
and the ledger row, both hashes, matched by number).

The refused file cannot be repaired in place: the file name is only
push's identity, while the ledger INSERT baked at commit keeps the
number from the draft's header — and commit refuses a re-commit — so
renaming the file just makes it run while `INSERT IGNORE` silently
skips its ledger row (the migration stays unrecorded and re-runs on
every future push). The recovery is a fresh draft at a number past
the ledger, executed against the setup above: archive the refused
file outside the migrations directory — it never ran, so there is
nothing to undo on the database — regenerate the draft (the emptied
directory draws 0001 again), and `surform migrate renumber <file> <n>`
it before committing (a draft carries its number in two places, the
file name and the opening `-- surform migration 0001` line, and
renumber moves both together — commit bakes the header's into the
ledger INSERT). The renumbered draft (`0002_add-name`) pushed,
applied, and the ledger recorded it exactly once; a re-push skipped
it as already applied. The reverse order — CLI-pushed migrations
first, applies after — needs no coordination: apply rows number one
past whatever the ledger holds.

**The capture excludes the flow's own machinery.** `fn::surform::capture()`
drops three things from the rendered INFO before assembling the
interchange: the module registration (the flow's own machinery is
never migration material — a capture that carried it would draft the
registration's removal, and removing the module the flow runs from
breaks the flow; the CLI's reader pairs the registration's storage
key — `mod::<name>`, issue #61 — so declaring the module in your SDL
makes the pair converge if you ever capture without the wrapper), the
database's buckets (the compare would draft their removal — the
module's own bytes live there), and the default ledger table
`_surform_migrations` (the pipeline's machinery is never migration
material). The module's converge and apply drop the same sections
themselves; the wrapper's pruning is what makes the SAME capture file
safe to feed the CLI (`migrate generate schema.surdl --info
db=ns/db=capture.txt`) — the CLI's own `--info` flow performs no
exclusions. One residual: a ledger kept under `commit
--ledger-table <name>` rides a name neither the wrapper nor the
module knows — it is drafted for removal (marked, never silent), so
exclude it from the capture yourself.

## Where next

- [Schema migration](migrate.html) — the CLI pipeline the module's
  machinery mirrors.
- [Modules (Surrealism)](modules.html) — authoring `DEFINE MODULE`
  registrations in SDL.
- The [crate README](https://github.com/jquesada2016/surform/blob/main/surform-surrealism/README.md)
  — the module's full contract: every export, every refusal surface
  (verbatim), the zero-grant re-proofs, the build recipe.
