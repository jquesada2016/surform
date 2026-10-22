# surform-surrealism

Surform as a [Surrealism](https://surrealdb.com/docs) module — the
compiler running INSIDE SurrealDB. The
guest is a thin wrapper over the core, packaged as TWO artifacts from
ONE source file:

- `surform.surli` — the default artifact: **zero capabilities**, pure.
  Its exports make no host calls and every output is reproducible
  from its inputs alone:
  - `mod::surform::compile(sdl)` — SDL text → the same SurrealQL DDL
    `surform build` prints, byte-identical (proven live and pinned by
    the vector suite). Hand the module composed,
    self-contained SDL only: `include`-bearing documents are
    REJECTED — the Surrealism runtime grants the guest no filesystem
    (probe-verified on SurrealDB 3.3), so an include read can never
    resolve and the load fails closed with the loader's own
    diagnostics.
  - `mod::surform::converge(sdl, live_info)` — the DB-pair convergence:
    the same script `surform migrate generate --info`
    prints, byte-identical; empty string means the database and the
    SDL already agree. See [Converge](#converge).
  - `mod::surform::status(files, ledger_rows)` — the migration
    classification: the same report `surform migrate status`
    prints, byte-identical. See [Status](#status).
- `surform-apply.surli` — the apply variant (the
  `surform-surrealism-apply` package, same source, `apply` feature
  on): everything above, PLUS `mod::surform_apply::apply(sdl)` —
  converge the invoking session's database onto the SDL and execute.
  Its manifest declares the grant surface. See
  [Apply](#apply) and [Security](#security) — read the security
  section before enabling this artifact.

Errors surface verbatim as `Function call error: ` + the exported
function's `Err` string — `SDL rejected: <n> diagnostics` (broken
SDL, the compile contract) or `live INFO rejected: <n> diagnostics` (a
malformed capture) followed by the CLI's piped-theme diagnostic
render.

## Build

Prerequisites: `rustup target add wasm32-wasip2` and the `surreal`
CLI (3.3.0) on `PATH`.

```sh
# from the repo root — deterministic (byte-identical across rebuilds)
surreal module build --out target/surform.surli surform-surrealism
surreal module build --out target/surform-apply.surli surform-surrealism-apply
```

Each command runs cargo for `wasm32-wasip2` inside its member (the
workspace target dir is respected), packages the component with the
exports manifest from that member's `surrealism.toml` into the
`.surli` archive, and lands it under `target/` (~1.2 MB each). The
two members share ONE source file (`[lib] path`): the default
artifact's crate keeps the `apply` feature OFF by default; the apply
package turns it ON — the manifest tests in both crates pin the
wiring.

Verify what an artifact actually packs (the archive is a
zstd-compressed tar whose `surrealism/` members ride plain):

```sh
tar --zstd -xOf target/surform.surli surrealism/surrealism.toml | sed -n '/\[capabilities\]/,/\[attach\]/p'
# the default artifact: every grant line false/empty — zero capabilities
tar --zstd -xOf target/surform-apply.surli surrealism/surrealism.toml | sed -n '/\[capabilities\]/,/\[attach\]/p'
# the apply artifact: the documented grant surface (below)
```

Both `tests/manifest.rs` suites run these extractions against built
artifacts when they exist (and skip with a note when they don't).

### Release assets

The repo carries no CI — releases are cut locally with exactly the two
`surreal module build` commands above, from a clean tree at the release
tag. Both `.surli` files attach to the GitHub release with their
SHA-256 checksums in the notes. The builds are byte-deterministic, so
a checksum is the whole verification: anyone rebuilding from the same
source gets the same bytes. Re-derive the hashes per release — any
source change moves them; the release notes are their canonical home
(in-database.md's "Getting the artifacts" points there too).

The working-today story — registration, the migration snippet, and
the in-database migration flow — lives on the docs site ("Surform
inside SurrealDB", `docs/content/in-database.md`).

## Register and call

The `.surli` rides a file bucket (write it into the bucket's backend
directory; megabyte bodies 413 on the `/sql` route):

```sql
DEFINE BUCKET modules BACKEND 'file:<dir>' PERMISSIONS FULL;
DEFINE MODULE mod::surform FROM f"modules:/surform.surli" UNSIGNED PERMISSIONS FULL;
DEFINE MODULE mod::surform_apply FROM f"modules:/surform-apply.surli" UNSIGNED PERMISSIONS FULL;
RETURN mod::surform::compile('namespace acme; database shop; ...');
```

`UNSIGNED` is mandatory for bucket loads on 3.3; the host needs
`--allow-experimental=surrealism` (and `files` for the bucket). The
module names (`mod::surform`, `mod::surform_apply`) are the
DEFINER's choice; the package identities inside the archives
(`surform/surform`, `surform/surform-apply`) are distinct on purpose —
module KV scope follows package identity, so the two artifacts can
never alias.

Direct dispatch like the `RETURN` above is the low-level surface. The
USER-facing calling surface is the migration snippet — a small SDL
file appended to the caller's schema whose `fn::surform::*` wrappers
bridge the capture and the ledger envelope for you — documented with
the whole working-today flow on the docs site
(docs/content/in-database.md; the snippet files are first-class
corpus documents at docs/examples/surform-migrate*.surdl, compiled by
this crate's vector suite).

`compile` accepts composed, self-contained SDL only — an
`include`-bearing document is rejected with the loader's own
diagnostics, never resolved: the runtime grants the module no
filesystem to read includes from.

## Converge

`converge(sdl, live_info)` compares a desired state (SDL text, the
same input `compile` takes) against a live capture, and returns the
migration script that converges the database onto the SDL — the
CLI's `migrate generate --info` machinery in-database, byte-identical.

The `live_info` carrier is the RENDERED INFO — the interchange object
literal the engine produces for one statement:

```sql
RETURN <string>(INFO FOR DB);
```

(the /sql route has no text rendering — `Accept: text/plain` answers
415 — so the `<string>` cast is the capture: probed on 3.3.0, its
output is byte-exact what the interchange reader consumes). Pass the
bare `INFO FOR DB` capture to compare tables at header level; assemble
each table's rendered `INFO FOR TABLE` under the reserved `"@tables"`
key — exactly the interchange file a `--info` caller writes — when
field-level drift matters (removals are the point of a migration).

The scope is the ONE database the SDL declares (the namespace-free
pseudo-scope when the file declares no namespace); zero or several
declared databases refuse. The capture must come from that database —
nothing in the interchange names it, so the pairing is the caller's
(the CLI's `--info db=` tag asserts the pairing the same way). The
default ledger table
(`_surform_migrations`) is dropped from the capture before ingestion —
the pipeline's own machinery is never drafted for removal (the revert
precedent; the CLI's `--info` mode has no such drop — a captured
ledger table WILL be drafted for removal there, so exclude it in the
capture). The drop matches the DEFAULT name only. A ledger kept under
`commit --ledger-table <name>` rides a table the module has no carrier
to learn — nothing in the capture or the SDL names the override — so
an override-named ledger IS drafted for removal by the in-guest
converge. The drafting is never silent: the script carries the
table's SURFORM WARNING and its REQUIRED slot. (The CLI's `--info`
mode is strictly worse: it drafts even the default.) Exclude an
override-named ledger from the capture yourself. The full exclusion
rule — which consumer drops what
— is on the working-today page (docs/content/in-database.md): the
module drops the default ledger from any capture it is handed, and
the docs' migration snippet (`docs/examples/surform-migrate.surdl`)
prunes it wrapper-side too, which is what makes the same capture file
safe to feed the CLI's `--info`, which performs no exclusions of its
own.

## Status

`status(files, ledger_rows)` classifies a migrations directory against
the migration ledger and returns the CLI's `migrate status` report —
applied / pending / draft / DRIFTED per file, missing per ledger row.
The module has no filesystem and no exit code: the carriers arrive as
strings, and the REPORT is the verdict (DRIFTED and missing rows are
the inconsistency classes a CI gate reads off the text).

- `files` — the serialized migrations-dir state, a JSON array of
  `{"number": 1, "name": "init", "committed": true, "hash": "<64 hex>"}`
  entries (any order). The hash is the CLI's committed-file hash:
  sha256 over the filled script (the file with its ledger region
  drained), the exact bytes `migrate status` re-derives. A draft entry
  carries `"committed": false` and no hash.
- `ledger_rows` — the /sql response body of
  `SELECT number, name, hash FROM <ledger>;`, pasted verbatim. The
  module reads the envelope itself: a not-found TABLE is the
  fresh-database empty ledger; any other failure surfaces with the
  engine's message. A SurrealQL caller cannot produce this envelope
  natively (there is no JSON render in-database) — the docs'
  migration snippet (`fn::surform::ledger_rows`, the working-today
  page) bridges it: it renders the narrow envelope the module reads
  (the OK shape and the fresh-database NotFound shape, hand-rolled
  for the bounded string set the ledger rows carry) and bridges a
  ledgerless database as the empty ledger, so `status` works from
  the first call.

Duplicate numbers refuse on either side (the numbering is unique per
database); malformed carriers name the defect.

## Security

Read this section before enabling the apply artifact.

> **Treat every module as privileged code that can affect data your
> database can access.** Sandboxing limits host access, but a module
> can still implement logic that leaks or corrupts data if you expose
> it to the wrong scopes.
> — SurrealDB's own module-architecture documentation, verbatim.

The apply artifact is exactly that class of module: its `apply`
export writes the database. Three properties bound it, and none of
them is optional:

- **No escalation, ever.** Execution runs AT THE INVOKING SESSION'S
  AUTH — the caller's own permissions govern every executed statement
  (the host executes with the invoking session's `Options`; a caller
  who may not `DEFINE TABLE` gets an apply that cannot either, and
  every failure carries the engine's own permission error). Whoever
  can call the module can already do everything apply does
  themselves; the module adds no reach.
- **The caller's session IS the scope.** Apply pre-flights: the SDL's
  one declared database must BE the invoking session's namespace and
  database (read live via the exact-name function grants), or apply
  refuses before anything is captured or run. The script's own `USE`
  line is one of the statements the host import cannot execute — the
  session-switch the CLI's push pre-flight guards against is not
  even expressible here (the probed shape: a `USE`/`BEGIN` text
  through the import runs nothing).
- **The destructive class never auto-applies — apply is
  additive-only, literally.** Two gates enforce it (the Apply flow's
  step 5): a script carrying SURFORM slot markers — the data-migration
  fill-ins and data-loss warnings that ride the MARKED removals
  (tables, fields, scopes) — refuses; and ANY `REMOVE` statement
  among the generator-marked statements — the unmarked catalog
  removals every other family drafts (users, params, functions,
  indexes, events, analyzers, sequences, accesses, configs, apis) —
  refuses too, naming the statement. Apply defines and never removes.
  A third gate keeps the UNMARKABLE class out the same way: a
  raw-authored statement's boundaries are its author's, not the
  generator's, so it refuses outright rather than execute across a
  boundary the flow cannot guarantee.

The grant surface (the apply artifact's `surrealism.toml`; the
default artifact carries every line false/empty — its zero-grant
property is re-proven at source, packed-artifact, and export-surface
level):

| grant | value | why |
|---|---|---|
| `allow_arbitrary_queries` | `true` | the host `sql` import — the captures, the executed statements, the ledger self-record. Runtime-checked on EVERY call; the platform's load-time validation refuses a module asking for more than its server grants; the SQL `PERMISSIONS` clause on the `DEFINE MODULE` gates who may CALL it — what the module may DO is this manifest, enforced by the host |
| `allow_functions` | `["session::ns", "session::db", "time::now"]` | exact names, nothing wildcarded. The module's function gate covers BUILTINS too (probed: `session::ns` refuses under an empty grant) — the first two are the scope pre-flight, the third the ledger's `applied_at` |
| `allow_scripting` | `false` | no scripting inside the module |
| `allow_net` | `[]` | no network |

The refusal surfaces, verbatim:

- the capability refusal (the default artifact can never hit it — it
  makes no host calls — documented from a live probe, `host.rs:153`):
  `Module does not have the 'allow_arbitrary_queries' capability`,
  surfaced to SurrealQL callers as `Function call error: HOST-SQL
  ERROR: host sql: Module does not have the
  'allow_arbitrary_queries' capability`;
- the default artifact's export refusal (probed live):
  `function 'apply' not found in exports manifest`;
- the apply artifact's scope pre-flight and refusal-gates surfaces
  (the marker gate's count + first marker; the additive-only gate's
  count + the named `REMOVE` statements; the raw-body gate's count +
  the named raw bodies): [Apply](#apply).

## Apply

`mod::surform_apply::apply(sdl)` (the apply artifact only) is
converge + execute: the module captures its host database's own
schema through the capability-gated `sql` host import, converges it
against the SDL, and executes the resulting script — **the database
migrates itself, at the invoking session's permission** (the
[Security](#security) properties are the contract).

The flow, step by step:

1. Load the SDL (the same composition loader `compile` runs;
   `include`-bearing documents refuse) and resolve its ONE declared
   database scope (zero or several refuse, as in converge).
2. Pre-flight the session (Security, above).
3. Capture: `RETURN <string>(INFO FOR DB);` plus one
   `RETURN <string>(INFO FOR TABLE <name>);` per table, assembled
   under the reserved `"@tables"` key — the exact full-fidelity
   interchange a caller-assembled capture carries. The flow's own
   scaffolding drops before ingestion: the `modules` section (a
   registration's `org::name` INFO keys refuse the readers — the
   probe-recorded corner — and SDL cannot author a module anyway) and
   the `buckets` section (a database-only bucket drafts `REMOVE
   BUCKET` — the module would delete its own bytes, and the draft is
   a removal the additive-only gate would refuse the flow's own
   scaffolding on; bucket convergence stays the CLI's flow). The
   default ledger table drops like converge's (both this flow's own
   machinery).
4. Converge (the `converge` export's machinery, the caller's database
   as the live side).
   Empty script → `(converged) — nothing to apply, the ledger
   untouched`.
5. **The refusal gates** (both run before any statement executes, no
   host call between converge and them):
   - the **marker gate** first: a script carrying SURFORM slot
     markers (the `REQUIRED` data-migration fill-ins and `WARNING`
     data-loss banners that ride the MARKED removals the generator
     drafts) REFUSES. The fill-ins exist because a human must move
     the data before the removal runs, and the module cannot pause
     for a fill-in its caller never sees — the caller hands the
     module SDL, not the script.
   - the **additive-only gate** second: ANY `REMOVE` statement among
     the convergence's GENERATOR-MARKED statements REFUSES, naming
     it. The emitter hands the flow both
     carriers — the script text and the statements it marked at
     emission (`ConvergenceScript`) — so the head test is syntactic
     over boundaries the generator owns; nothing re-derives them from
     the rendered bytes.
     The markers cover only the marked class; every other family
     drafts its removal bare (a db user the SDL never declares drafts
     `REMOVE USER IF EXISTS … ON DATABASE;` with no banner), so this gate is what makes
     "apply never removes" literally true. The escape is your own
     session: run the removal(s) yourself and re-apply — the next
     convergence drafts only what still differs — or take the
     destructive class through the CLI's generate → fill → commit →
     push flow, where the script is printed and the slots are
     enforced.
   - the **raw-body gate** third: any raw-authored statement (rule 8)
     refuses. A raw body is SurrealQL captured VERBATIM — its
     statement boundaries are the AUTHOR's knowledge, which the
     generator does not mark, so the flow cannot execute it
     one-call-per-marked-statement. `surform migrate push` is the
     faithful path for raw shapes: it sends the whole committed file
     as ONE `POST /sql` body the ENGINE's own statement parser
     splits.
6. Execute: the marked statements, ONE host call per statement, in
   order — exact boundaries by construction (the `USE`/`BEGIN`/
   `COMMIT` context lines are the script text's, never carrier
   elements — see the security note; the host import parses one
   expression per call, and a multi-statement string is silently
   first-expression-only — the probed shapes that fixed this design).
7. Self-record: the pipeline's ledger block (`DEFINE TABLE IF NOT
   EXISTS _surform_migrations` + typed fields + `INSERT IGNORE`)
   executes after every schema statement succeeded — the row
   (`NNNN_apply`, numbered one past the ledger's highest, the CLI's
   1–9999 ceiling) exists only for fully applied scripts, and its
   hash is sha256 over the executed script (the CLI's committed-file
   definition; the `sdl` column carries the SDL that was applied, so
   `migrate revert` stays meaningful for apply rows). The number is
   the ledger's record id, shared with the CLI's migrations — an
   apply row named `0001_apply` means a later CLI migration drawing
   0001 from a directory that cannot see the ledger refuses at
   `push` ("another migration owns this number on this database");
   the working-today page (docs/content/in-database.md) documents
   the seam and the escape.

**The transaction.** The script's own `BEGIN`…`COMMIT` frame cannot
ride (the import cannot parse it), but the atomicity arrives at the
dispatch boundary instead — live-probed on 3.3.0: **the module's host
writes ride the invoking statement's transaction.** A failed apply —
any `Err` the export returns, at any step — rolls back everything the
call did, ledger row included; a successful apply commits whole. The
failure summary names the failing statement and the engine's error
verbatim. One caveat the probe pinned alongside: the rollback stops
at the statement boundary — a caller batching `CREATE …;
RETURN mod::surform_apply::apply(…);` in one request keeps the
`CREATE` when the apply statement fails.

Outcomes, pinned strings:

- `applied 7 statement(s); recorded 0001_apply in the ledger`
- `(converged) — nothing to apply, the ledger untouched`
- `statement 5 of 5 failed: The field 'missing_field' does not exist`
  + the dispatch-rollback note
- the marker-gate refusal (the count + the first marker line)
- the additive-only refusal (the count + every named `REMOVE`
  statement)
- the raw-body refusal (the count + every named raw body)
- the scope pre-flight refusal (the SDL's database vs the session's)

## The artifacts at a glance

| artifact | package / identity | feature | capabilities | surface |
|---|---|---|---|---|
| `surform.surli` | `surform-surrealism` / `surform/surform` | — (apply OFF) | zero | `compile`, `converge`, `status` |
| `surform-apply.surli` | `surform-surrealism-apply` / `surform/surform-apply` | `apply` (default there) | the grant surface above | all four, `apply` marked `writeable` |

The default artifact's zero-grant property is re-proven at THREE
levels: the source manifest's exact-lines test, the packed-artifact
extraction (above; both manifest suites run it), and the export
surface (no `apply` in the default artifact's packed
`exports.toml` — asserted in the same test).

## The feature story (source level)

The `apply` cargo feature on the default crate carries the apply
machinery in the SHARED source: the pure halves (the structured
convergence carrier (`converge_parts` — the emitter's marked
statements), the session pre-flight, the refusal gates (markers + the
additive-only scan + the raw-body gate), the ledger ride)
compile wherever the feature is enabled — the apply package enables
it by default, so the workspace gate (`cargo test --workspace`,
`clippy --workspace`) compiles and tests them; the host-calling flow
and the `apply` export exist only in the wasm32 build (the SDK's
host imports are component bindings). The default artifact never
enables the feature; its manifest tests pin the feature's absence
from `default` and its existence as an empty declaration.

## Vectors (tests)

`tests/vectors.rs` pins byte-identity against the root corpus's
self-contained fixtures (`clean`, `app`, `raw`, `silo-module`) and the
error taxonomy (`broken`, `mixin-broken`). `tests/converge.rs` pins
the convergence script against `tests/goldens/converge.surql` — the
real `surform migrate generate --info` stdout section for the
committed fixture pair (`tests/fixtures/converge.surdl`, the desired
state; `tests/fixtures/converge-live-info.txt`, a REAL rendered
interchange captured from the engine's `<string>` cast and assembled
under `"@tables"` per the docs' shell-loop recipe) — plus the
converged capture (the empty script), the ledger-table drop, the
scope refusals, and the error surfaces. `tests/status.rs` pins the
report render per state, the fresh-database branch, and the carrier
refusals. The expected outputs are COMMITTED goldens: the `.ddl`
files were generated by the real `surform build` binary at unit time,
the `.err` files by the guest's own renderer (the CLI's piped theme,
mirrored exactly), the `.surql` script by the real `migrate generate`:

```sh
# regenerate the DDL goldens after a deliberate codegen change —
# then re-run the root crate's oracle suite, the pipeline's tripwire
cargo run --bin surform -- build tests/fixtures/clean.surdl \
  > surform-surrealism/tests/goldens/clean.ddl
# regenerate the convergence golden the same way (pin the
# `== migration script ==` section's bytes)
cargo run --bin surform -- migrate generate \
  surform-surrealism/tests/fixtures/converge.surdl \
  --info db=s2demo/probe=surform-surrealism/tests/fixtures/converge-live-info.txt
```

Running the CLI binary from the suite was rejected: this package does
not own the `surform` bin, so the test would depend on a
workspace-target path convention. The pinned goldens + the root
oracle's per-commit codegen coverage carry the identity guarantee
instead; the probe additionally verified the in-guest output
byte-identical (SHA-256) to the CLI's for a live dispatch.

`include`-bearing documents are not host-testable vectors: on the host
the loader WOULD resolve files, so the fixture cannot pin the
fail-closed in-guest behavior — that path is pinned live (the
loader's own `SDL-E0008` unreadable-include diagnostic, fired
because the Surrealism runtime grants the guest no filesystem,
probe-verified on 3.3).

The apply package's suites (`surform-surrealism-apply/tests/`) pin
the apply machinery mock-side: the structured carrier over the committed
golden and the probe shapes (SDLs whose marked statements are exact
by construction), the refusal gates
(markers + the additive-only scan + the raw-body gate), the session
pre-flight, the ledger
numbering/ride/summaries, the scaffolding drop, the manifest grant
pins, and the packed-artifact extractions (run against built
artifacts when they exist). The capture ENGINE's own contract (the
printer-regex entries the compile-side repair engine rides) pins in
the core lexer's tests. The apply FLOW's live legs — drift →
apply → converged, the marker refusal, the mid-script failure and
its dispatch rollback, the scope pre-flight, the default artifact's
export refusal — ran against a throwaway database.

## Constraints

- The `surrealism` SDK is exact-pinned `=0.6.0` — the version an early
  spike proved end-to-end (plus its `surrealism-macros` 0.6.2 /
  `surrealism-types` 0.6.0 resolution, the same graph the spike
  locked). The pin rides the surform SurrealDB floor (`=3.3.0`
  family); a bump is a floor-move-shaped event and re-runs the
  server-side probe before release.
- This is a DIFFERENT ABI from the editor crate (WIT component vs
  wasm-bindgen) — no shared code paths; the editor's wasm artifacts
  are untouched by this crate.
