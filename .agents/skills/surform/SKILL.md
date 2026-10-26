---
name: surform
description: Surform SDL schemas (.surdl) for SurrealDB 3.3 and the surform CLI (check, build, fmt, lint, normalize, migrate generate/commit/renumber/push/status/revert/pull): compile declarative schema to SurrealQL DDL, converge schema drift, and run the numbered-draft migration pipeline (fill-in slots, ledger, HTTP push, status, revert). Use for any .surdl file, surform command, schema drift, or SurrealDB schema-as-code — even without the word "Surform" when the content is SDL. For SurrealQL queries and SurrealDB runtime behavior, use the surrealdb skill instead.
---

# Surform SDL (SurrealDB 3.3)

*This is the repo's own copy of the skill (`.agents/skills/surform/`), auto-discovered when working in this repository. To install it in your own agent harness, copy this folder to `~/.agents/skills/surform/`.*

Surform is a declarative schema language for SurrealDB: a plain-text
`.surdl` file is the *desired state*, and the `surform` compiler turns it
into SurrealQL `DEFINE` DDL. The reverse path reads a database's
rendered `INFO` output back into canonical SDL, diffs it against the
file, and merges it — so the file and the database never drift
silently. There is exactly one emission target: **SurrealDB 3.3** (no
`--target` flag; deprecated grammar like `DEFINE SCOPE` or MTREE
indexes is rejected).

The binary never touches a database connection. `surform build` prints
the script; your SurrealQL tooling applies it.

A Surrealism module artifact exists alongside the CLI
(`surform-surrealism/`): the same compiler packaged for
in-database dispatch as `mod::surform::compile(sdl)` plus
`mod::surform::converge(sdl, live_info)` and
`mod::surform::status(files, ledger_rows)` (the migrate pipeline's
generate/status machinery) — byte-identical outputs, zero capabilities
by default, errors verbatim as `Function call error: …`. There is a
SECOND artifact, the apply variant (`surform-surrealism-apply`,
dispatches as `mod::surform_apply::…`): the same exports plus
`apply(sdl)` — the module captures its host database's own INFO,
converges, and executes the script. Its writes run at the INVOKING
SESSION'S permissions (no escalation; a failed apply rolls back
whole). It is additive-only, literally: it refuses any script
carrying SURFORM slot markers AND any `REMOVE` statement among the
generator-marked statements (the marker gate covers only the marked
data-loss class — tables, fields, scopes; every other family's
catalog removal drafts bare, so the second gate refuses it by name —
and a raw-authored statement refuses too: its boundaries are the
author's, which the generator does not mark — the destructive class
stays your own session or the CLI's fill → commit → push flow), and
it never drafts its own scaffolding (its module registration, its
bucket, the ledger table). Execution rides the emitter's marked
statements, one host call per statement — no lexical splitting
anywhere. Read that crate README's Security
section before registering it. The USER-facing calling surface is
the migration snippet (`docs/examples/surform-migrate.surdl` and the
`-apply` twin): a header-less `.surdl` appended to the caller's
schema whose `fn::surform::*` wrappers (compile/converge/status/
ledger-bridge/migrate) bridge the capture and the ledger envelope in
pure SurrealQL — the whole working-today flow (release assets,
bucket/DEFINE MODULE registration, in-SurrealQL migration, the CLI
interop seams) is the docs page `docs/content/in-database.md`.
Build recipes live in the crate README; the
`surreal module build` flow is repo maintenance, not part of the
language or the CLI surface this skill teaches.

For SurrealQL query syntax, expression functions, and SurrealDB
runtime behavior, read the `surrealdb` skill — SDL expression clauses
are SurrealQL verbatim, so real schema work usually needs both.

## Reference files

- `references/statements.md` — all 16 `DEFINE` statements plus `raw`,
  `mixin`, and the `type` aliases:
  grammar sketch, verified examples, per-statement gotchas. **Read a
  statement's entry before writing it non-trivially** — the variant
  rules and clause menus are where the traps live.

## The verification loop

The compiler is local and fast. Never hand-verify SDL — run it:

```sh
surform check schema.surdl      # parse + diagnostics; silence + exit 0 = good
surform lint schema.surdl       # advisory findings (exit 1 when any exist)
surform build schema.surdl      # eyeball the emitted DDL on stdout
surform fmt --in-place schema.surdl   # canonicalize (fixed point — safe to re-run);
                                      #   on a composition root this writes the WHOLE tree
```

After every edit: `check` must be clean, then `lint`. `check` proves
grammar; it cannot see cross-statement define-time rules the database
enforces — but placement is not one of them: the compiler owns the
emission order (Golden rule 1). `surform --json <cmd>` renders every
diagnostic as one JSON object per line on stderr, and the migrate
pipeline's refusals (commit/push/status/save errors) as one
`{"error": …, "hint": …}` object — the sentence and its fix.

Conventions: DDL and schema text go to **stdout**; diagnostics and
lint findings go to **stderr**. A clean `check` prints nothing and
exits 0; `build`/`fmt` print their artifact to stdout on success.
`fmt` with no file (or `-`) reads stdin and prints the canonical form
— the pipe the VS Code extension's Format Document drives; `check`
and `build` read a FILE only (deliberate — validation runs the
resolution pass, which needs the file's root for includes/spreads;
`fmt` is the pipe's tool).
`--max-width N` moves the wrap budget long lines break under
(default 100; `normalize` takes the same flag).

If `surform` is not on PATH: build it from this repository —
`cargo install --path .` puts it on PATH, or a plain `cargo build`
leaves the binary at `target/debug/surform`.

## A complete file

```sdl
namespace acme;
database shop;

/// English text search.
analyzer english {
  tokenizers: class;
  filters: snowball(english), lowercase;
};

/// People who buy things.
table user {
  schemafull;
  permissions {
    select: full;
    create, update: $auth.id = id;
    delete: none;
  };

  field email: string { assert: string::is_email($value); };
  field name: string;
  // Stores the argon2 hash written by access account's signup below.
  field password: string;
  field created_at: datetime { default: time::now(); };

  index unique email_unique {
    fields: email;
  };
};

/// Anything a user publishes.
table post {
  schemafull;

  field title: string { assert: string::len($value) > 0; };
  field body: string;
  field author: record<user> {
    reference { on delete: cascade; };
  };

  index author_idx {
    fields: author;
  };
  index fulltext body_fts {
    fields: body;
    analyzer: english;
  };

  event audit_title {
    when: $event = 'UPDATE' and $before.title != $after.title;
    then: { create audit set at = time::now(), user = $after.id; };
  };
};

/// Record sign-in for shoppers.
access account {
  on: database;
  type: record;
  signin: {
    select * from user
      where email = $email
      and crypto::argon2::compare(password, $password)
  };
};
```

The analyzer sits before the fulltext index that uses it for
readability only — the compiler orders the emitted DDL by dependency,
not by file position.

## The language in one page

**File shape.** A file is a list of `;`-terminated definitions, read
top to bottom, emitted in the compiler's dependency order (authored
order only breaks ties). Nearly every file starts
`namespace acme; database shop;` (the compiler emits the matching
`USE` lines). Recommended layout: one namespace + one database per
file.

**Identity in the header, configuration in the block.** The header
carries the name plus grammatical elements that change what the block
may contain: `index unique <name>`, `table any <name>`,
`table user->wrote->post`, `fn name($p: type) -> type`. Everything
optional lives in the `{ … };` block.

**Three clause shapes** (the whole grammar — sketch, not runnable):

```sdl-grammar
schemafull;            // flag — no colon, no block (flags may be phrases: `include original;`)
expiry: 1d;            // value clause — `:` exactly when a value follows
changefeed { … };      // sub-block — `key {` with NO colon (`key: {` would start a SurrealQL capture)
```

Clause keys may be multi-word (`default always:`, `on delete:`).
Repeated clauses fold last-one-wins. A clause block is optional when
empty (`namespace acme;` ≡ `namespace acme { };`).

**Two value kinds.** *Keyword clauses* (`tokenizers:`, `bm25: (1.2, 0.75);`,
`roles: editor, viewer;`) are SDL-lexed from a fixed menu. *Expression
clauses* (`assert:`, `default:`, `when:`, `then:`, `signin:`, `value:` …)
capture everything after `:` verbatim — to the first `;` at bracket
depth 0, skipping strings, comments, and quoted identifiers — and hand
it to the embedded official SurrealQL parser. **Never invent SurrealQL:
if you would not write it in a `.surql` file, it will not parse** — and
unknown function paths fail at *check* time (SDL-E1009), so typos
surface before apply.

**Types pass through verbatim**: `string`, `int`, `option<string>`,
`array<T, N>`, `set<T>`, `record<user>`, unions (`'small' | 'large' |
int`), inline objects (`{ a: string, b: number }`), `geometry<point>`,
`file<image>`, …

**Comments.** `//` is for humans — never emitted at codegen, and it
never changes what a file means (the migration compare and the fixed
point ignore it),
but `fmt` **keeps it**: every `//` line re-emits at its anchor — above
the statement, nested child, or CLAUSE it preceded, at any depth
(sub-block clauses, permission groups, api groups too), riding that
clause through the canonical reordering; a note on the same line as a
terminator (`then: select …; // why`) stays on that clause's line;
anchorless lines inside a block trail just before the close. Comments
inside fn bodies,
`then:` blocks, and view captures are SurrealQL territory and drop.
`///` is a doc comment: a contiguous block directly above a definition
(no blank line) becomes its `COMMENT`. `///` attaches to **top-level
definitions only** — inside a table block it is rejected (SDL-E1003);
comment nested children with `//`. `///` is also rejected on `sequence`
and `config` (no COMMENT clause exists there).

**Assignment short form.** Statements whose one required clause is a
value may write `keyword <name> = <expr>;`: `param $limit = 50;`,
`sequence invoice_number = 1000;`, computed field `slug: string =
string::slug(title);`. Any additional clause requires the block form.

**Escape hatch.** `raw { <verbatim SurrealQL DDL>; };` forwards
anything SDL cannot express. Top-level only; the interior is carried
byte-for-byte; validation is advisory (the linter flags bodies the
parser rejects, SDL-L0012); no doc comment.

**Convergence, not migration — until you ask for the migration.**
Every emitted `DEFINE` carries
`OVERWRITE` (sole exception: a credential-less user defines once),
so applying a built file is safe to repeat — the database
converges to the file. Applying never *removes*: definitions the file
lacks stay. Removal — and the whole converge-the-database script — is
`surform migrate generate`'s (below).

**Multi-file schemas.** `include 'tables/users.surdl';` splices
another file's statements at the include's position (depth-first);
the whole behaves exactly like its flattening — same DDL, same lint
findings. An include is legal anywhere in the file; the one order
rule is `namespace`/`database` before the file's FIRST include
(SDL-E1069), and an INCLUDED file carries no scopes at all
(SDL-E1070). Paths resolve relative to the including file (`..` fine;
absolute rejected, SDL-E0012). Cycles are legal: the include graph
declares participation, not an elaboration order — each file splices
exactly once, and discovery order (depth-first, first visit) breaks
ties. The same file twice in ONE include list is SDL-E0014; a diamond
— an already-spliced file declared again by another file — is a no-op
the linter advises on (SDL-L0026, allow-suppressible above the
include); a missing file is SDL-E0008 (anchored on the include).
Mixin spreads resolve document-wide: a spread names any mixin the
composed document defines — any file, any position, forward spreads
included — and naming none is SDL-E1061 ("spread names no mixin of
the document"). Name collisions — mixins, tables, every definition
family alike — are SDL-E1062 across files exactly as within one:
last-wins overrides are gone, and the emission order is the
compiler's regardless of how the files splice.
`check`/`build`/`lint`/`migrate generate` follow includes; findings
render per
file and `surform-lint-allow:` stays file-scoped; `fmt` print mode
formats the file's own text as authored (never flattens) and never
resolves spreads, so any single file — a leaf whose spreads resolve
only through its composition included — formats by its own path;
`fmt --in-place` writes the whole tree — every participating file
formats from its own loader parse (a dirty tree is never
rewritten). `normalize --in-place` writes the whole
tree back — every merged statement returns to its home file (a
scope-statement or dependent preamble-bound adoptee refuses,
SDL-E0013); without `--in-place` the flattened canonical form prints.
Contexts without a file system (playground, inline
buffers, doc fences) reject the statement (SDL-E1068).

## Golden rules (the verified traps)

1. **Order is the compiler's job; uniqueness and acyclicity are
   yours.** Definitions may stand anywhere — `build` emits the DDL in
   its own stable topological order over the engine's define-time
   edges (a view over the tables its top `FROM` names; a fulltext
   index over its analyzer and the fields it names; a param's
   `VALUE`, a sequence's sides, user/access durations, a jwt's
   `url:`/`key:`, a bucket's `backend:`, api middleware arguments,
   and `config default`'s sides, over the params and fns those
   expressions read — and over the `mod::`/`silo::` modules they call
   (the engine resolves `mod::<first-segment>` and the silo catalog
   name at define)), with authored order as the tie-break and `fmt`
   never reordering. Runtime-evaluated references (fn bodies,
   permissions, events, field defaults) carry no edge at all. The
   three hard errors that replace positional discipline: the same
   name defined twice — any family, any file, any position — is
   SDL-E1062 (the old last-wins override is gone), a define-time
   cycle (a view chain reading itself, a param chain feeding itself)
   is SDL-E0015, the message naming the chain, and a dependency the
   compiler cannot place — its target stands after the section's
   next `namespace`/`database` statement — is SDL-E0016 (emission
   reorders within a scope section, never across one; the compiler
   refuses rather than emit DDL that fails at apply). (Mixin spreads
   are still document-wide.) Nesting children inside their table
   blocks remains the readable default.
2. **A bare table is `normal`** and emits `TYPE NORMAL` — `RELATE`
   into it fails. Declared relations use arrows:
   `table user->wrote->post { enforced; };`. `table any <name>;`
   is the explicit escape hatch for RELATE targets. `enforced;` adds
   the endpoint-existence check. `lightweight` (trailing the header,
   both sides named) is the record-less edge: it implies `enforced`
   and forbids fields/indexes/events entirely.
3. **Field types are required** (unless the field is computed)
   (`name: string;`, not `name;`). Nest
   subfields as separate dotted entries (`stats.views: int;`,
   `tags.*: string;`) — there is no nested-block sugar. A field named
   like a clause key (`index`, `event`, `permissions`, `in`, `out`…)
   takes the `field` prefix.
4. **Permissions are asymmetric.** In a table `permissions { … }`
   block, unlisted operations are NONE (deny); in a field block they
   are FULL (allow). A table's default is no clause at all = all
   operations denied — the secure default, never warns. `permissions:
   full;` trips the world-open lint (SDL-L0001).
5. **Where a body must return a value, omit the trailing `;`.**
   `signup:` / `signin:` captures and `fn` bodies are SurrealQL blocks
   whose last statement is the result — a trailing `;` there returns
   nothing. (A view's block is the opposite: its query is
   `;`-terminated by grammar.)
6. **Durations value-normalize at emit.** SDL's `expiry: 7d;` emits
   `CHANGEFEED 1w`; `30d` emits `4w2d`; `365d` emits `1y`. Never
   compare or assert duration spellings — compare values.
7. **Secrets never live in SDL.** `user password:` warns (SDL-L0002) —
   prefer a pre-computed argon2 hash in `passhash:`. JWT symmetric
   (hs*) verify keys and all issuer keys accept exactly one spelling:
   `'[REDACTED]'` (SDL-E1056) — provision them outside SDL. A user
   with no credential is define-once (emits `IF NOT EXISTS`).
8. **Never echo `INFO` output back into SDL or DDL.** INFO redacts
   secrets as `'[REDACTED]'` and materializes defaults; the file is
   the sole source of truth for secrets, and `normalize` — not
   copy-paste — is how database state enters a file.
9. **Keywords are lowercase** in SDL (the DDL comes out uppercase).
   SurrealQL aliases are accepted (`ns`, `db`, `kv`, `columns`) and
   codegen emits the canonical word. Keyword-colliding names take
   backticks, SurrealQL's own rule.

## The linter (advisory — the traps it catches)

`surform lint` evaluates the *resolved* state (merged with `--info`
scopes when given; two findings sit outside that rule and evaluate
the authored file: SDL-L0011 and SDL-L0012) and exits 1 when any
finding exists. `check`/`build` output is never affected.

A deliberate exception is suppressed at its site with an allow
comment — statement-scoped (leading comments of one statement, or the
same-line comment after its `;`), comma-listed codes, and the
directive's own problems linted SDL-L0023 (unknown code, misspelled
marker, nothing to attach to — never suppressible itself):

```sdl
// surform-lint-allow: SDL-L0001
table countries { permissions: full; };   // public reference data, by design
```

| code | finding |
| --- | --- |
| SDL-L0001 | table permissions `full` — anyone, even unauthenticated, can access |
| SDL-L0002 | plaintext `password:` on a `user` statement |
| SDL-L0003 | secret-less user (define-once) |
| SDL-L0004 | `edgengram`/`ngram` min > max |
| SDL-L0005 | duplicate entry in roles/tokenizers/filters |
| SDL-L0006 | relation without `enforced;` |
| SDL-L0007 | field/index/event at top level that could nest in its table |
| SDL-L0008 | param without `value:` |
| SDL-L0009 | api with no effective `any` catch-all (or no groups at all) |
| SDL-L0010 | record-typed `in`/`out` field rewriting a relation endpoint |
| SDL-L0011 | access duration side the access kind cannot keep |
| SDL-L0012 | `raw` body the pinned parser rejects |
| SDL-L0013 | applying `'[REDACTED]'` as credential material |
| SDL-L0014 | table `delete: full;` (cannot round-trip) |
| SDL-L0015 | computed field vs a field defined under it |
| SDL-L0016 | `reference;` on a non-record-shaped kind |
| SDL-L0017 | `id` field kind not a record-id key kind |
| SDL-L0018 | `flexible;` on a non-SCHEMAFULL table |
| SDL-L0019 | `inline;` field on a non-relation host |
| SDL-L0020 | table with more than 255 `inline;` fields |
| SDL-L0021 | mixin defined but never spread |
| SDL-L0022 | a later spread's member shadows an earlier spread's (last-wins) |
| SDL-L0023 | a `surform-lint-allow:` comment's own problem (unknown code, malformed marker, no statement to attach to) |
| SDL-L0024 | a referenced table/analyzer/`fn::` path (or a `fn::`-body param) that nothing in the linted files defines — advisory and scope-keyed; `mod::` is covered by its module, `silo::`/`ml::` by their version tails, builtin paths resolve bare, a `param` definition satisfies a body read, and non-fn-body params are muted (client-bindable) |
| SDL-L0026 | an `include` naming a file already spliced — first inclusion wins, this declaration is a no-op |

Diagnostics are `SDL-Exxxx`: E0xxx lexing/capture/file entry, E1xxx
statement grammar and fold-time validations, E3xxx the INFO
interchange reader (`--info` tags and file contents). The three the
declarative namespace made daily work: `SDL-E1062` — a duplicate
definition (every family — table, param, fn, user, …; any file, any
position; the old cross-file table last-wins is gone); `SDL-E0015` —
a define-time dependency cycle (view chains and param chains; the
message names the chain); and `SDL-E0016` — a define-time dependency
the compiler cannot place — its target stands after the next
`namespace`/`database` statement (emission reorders within a scope
section, never across one). The message text is self-contained;
each code names its rule.

## Drift and adoption (`--info` interchange)

`migrate generate` and `normalize` compare/merge against rendered
`INFO` output,
handed over as interchange files — one `--info` tag per scope
(`migrate pull` produces them straight from the live database — its
bullet below):

```sh
surform migrate generate schema.surdl \
  --info root=info-root.txt \
  --info ns=acme=info-ns.txt \
  --info db=acme/shop=info-db.txt      # exit 1 when drift exists
surform normalize schema.surdl --in-place --info db=acme/shop=info-db.txt
```

Captures taken after a module registration pair fine: the engine keys
the INFO entry `mod::<name>` (its storage derivation) while SDL spells
the bare name — the reader pairs either and records the bare name.

The version pair swaps the database side for a second schema file —
the migration that takes a database at the v1 state to v2:

```sh
surform migrate generate --from schema-v1.surdl --to schema-v2.surdl
```

Same report, same script, `--check` rides; the input modes are
mutually exclusive — the versions pair, `--info`, or a bare FILE
(the baseline: every definition apply-pending, the script defines
from nothing) — and every invocation names exactly one complete
mode; a lone side or a mix of two modes' flags is
a usage error (exit 2). Scope rules here: a scope only v2 declares is new
(its namespace/database and content define), a scope only v1 declares
is retired (one WARNING-anchored `REMOVE DATABASE`/`REMOVE
NAMESPACE` whose block names every finding it consumes), scopes both
declare compare ordinarily.

- `root=<path>` — `INFO FOR ROOT` output; `ns=<name>=<path>` —
  `INFO FOR NS`; `db=<ns>/<name>=<path>` — `INFO FOR DB` output **plus
  each table's `INFO FOR TABLE` object under a reserved `"@tables"`
  sibling key**. At least one tag is REQUIRED (`normalize` without
  one refuses at the CLI — it would silently be a disguised `fmt`);
  missing scopes simply don't merge. Producing these
  files is external tooling (the `surreal` CLI or an SDK query) —
  surform itself has no `pull`.
- `migrate generate` prints the drift report (`~` changed, `−`
  database-only, `+` SDL-only → apply-pending, `·` expected-unmanaged
  → informational) then the convergence script: defines first
  (SdlOnly + drifted definitions as `DEFINE … OVERWRITE` from the
  file's side, in the compiler's emission order — a standalone
  field/event/index define renders inside its host table's emission,
  after the host's `DEFINE TABLE` and before the nested members, a
  raw define slots through its header's `ON <table>` the same way
  (no readable `ON` keeps its place), a view sorts behind every table
  its SELECT reads and a raw fulltext index behind its analyzer
  (either side may be a raw block), every define-time value slot (a
  param's VALUE, a sequence's BATCH/START/TIMEOUT counters, a
  user's/access's DURATION sides, an access jwt's url:, a bucket's
  backend:, api middleware args, config default's sides) sorts
  behind the param/fn/module it reads when that target defines in
  the same scope — reader, target, or both raw — and
  a member the table block nests renders once inside that emission),
  removals last
  (`IF EXISTS` everywhere, children before tables before analyzers),
  one `BEGIN TRANSACTION`/`COMMIT` PER SCOPE SECTION — each frame
  labeled `-- section: <scope> (N defines, M removals)` in the
  report's scope vocabulary — with each
  section's `USE` switch outside its frame (so are the
  namespace/database owner defines, ahead of their scope's section;
  a failed statement rolls its own section back — sections before it
  stay applied; a
  fulltext index
  over a record-holding table builds at define and cannot see an
  analyzer the same transaction defines — let that analyzer converge
  first), and a
  `SURFORM WARNING` comment block on every data-destroying line
  (`REMOVE TABLE` deletes the documents; `REMOVE DATABASE`/
  `NAMESPACE` the whole subtree — the retired-scope block names every
  finding it consumes), a one-line `-- note:` above any UNIQUE index
  define whose table exists database-side (the one row-validating
  define — it fails over colliding rows; the note says when the
  index's fields cover re-keyed fields), and a `SURFORM REQUIRED: DATA
  MIGRATION` block — an empty fill-in region between
  `<<<`/`END <<<` markers, with a runnable `e.g.` line (fill =
  delete its `-- e.g. ` prefix) — immediately
  before every removal that orphans data: a removed table (its
  documents destroyed; the WARNING rides too) and a removed field
  (the values remain on the rows, outside every field definition,
  until you clean them up —
  the slot asks for the copy first, then hints the cleanup). Fill
  slots before running: the queries ride their section's
  transaction, before their removals, after the scope's defines (a
  target the migration creates is already valid). The report lists
  every slot in a
  manifest (`== required data-migration slots ==`); `--check` shows
  it too. A CHANGED FIELD TYPE stages the table instead (issue-#65
  semantics, engine-verified: the define re-validates NO rows, but a
  data step rewriting the values fails under the OLD kind — the error
  names the existing row, reading like a define-time rejection): the
  section opens with the STAGED PRELUDE — `DEFINE TABLE OVERWRITE t
  TYPE ANY SCHEMALESS;` (schema mode and permissions drop to the
  engine defaults: schemaless accepts every write, a no-permissions
  table denies every non-privileged session, and the window never
  commits; SDL-DECLARED tables only — an undeclared host is
  unmanaged database-side, no shell rides, the relax line alone
  lifts the kind), then each re-keyed field as a MINIMAL `DEFINE
  FIELD
  OVERWRITE f ON t TYPE any;` (field definitions survive a table
  overwrite; no default/value/assert/flexible/readonly rides — they
  would constrain or rewrite the data step, and flexible + `any` is
  an engine parse error), then one REQUIRED section per
  re-keyed table (the backfill runs under the relaxed kinds, before
  the final defines, same transaction; its prose names the re-keyed
  fields and their new kinds, and each spellable re-key carries a
  runnable `e.g.` line — the engine's own `<kind>` cast for the
  scalar kinds, `UPDATE t SET age = <string> age;` (engine-verified
  — the fn-call casts like `string::from` are rejected, no such
  builtin), the record's id for `record<...>`), and the final
  defines — a
  DECLARED staged table's OWN definition always rides (a converged
  table gets a fabricated restore so the shell never persists).
  Views and
  lightweight relations never stage. After the defines, a commented
  `SURFORM NOTE: INDEX REBUILD` block offers each qualifying index's
  `REMOVE INDEX …` + re-define (every UNIQUE index on the table plus
  every index whose fields name a re-keyed field — a UNIQUE build is
  the one row-validating define: it fails if two rows collide); the
  report carries the corrected kind note AND a `== staged migrations
  ==` manifest. Events/analyzers are catalog machinery — neither
  block. No rename is ever
  auto-detected: the human names the move, the slot names the
  stakes. The report leads with a summary line (findings, defines,
  removals, data-migration slots, staged tables); the script header
  closes with its own counts of the same vocabulary plus the frame
  count (its defines half, staged set, and slot count — collapsed
  scopes' obligations die with the owner REMOVE — can differ from the
  report's). `--check` prints the report only.
- `migrate generate --save` writes the script as a numbered DRAFT
  under `migrations/` (`NNNN_<name>.surql`; the number is the highest
  existing draft's plus one, a collision an error — never a silent
  bump; the name defaults to the SDL file's stem slug-folded,
  `--name` spells it, `--dir` moves the directory) and prints the
  report, the file's path, and the next command (`fill the SURFORM
  SLOT regions, then: surform migrate commit <file>`). A script
  spanning SEVERAL scopes saves as a draft GROUP instead — one draft
  per scope section (below). The
  draft's tool-owned header —
  `-- surform migration NNNN — <name>` / `-- generated:` /
  `-- source:` / `-- source-sha256:` (the hash of the source's
  canonical SDL at generation — the pin commit re-derives) /
  `-- input:` (the diff's INPUTS: `versions from=<file> to=<file>`,
  `info <tag specs>`, `file path=<file>` — the bare-FILE baseline —
  or a revert's `from=(live) to=<sidecar>`;
  whitespace-carrying paths ride double-quoted, and the bare
  `info`/`versions` of older drafts still parse) / `-- status: draft`
  — is the identity `migrate commit` re-reads; a blank line separates
  it from the script's own header. In the DRAFT the unfilled slot regions
  are delimited by bare git-style resolution markers —
  `<<<<<<< SURFORM SLOT (<scope>: <family> <key>[ ON <table>])` …
  `>>>>>>> SURFORM SLOT END` — an unresolved draft is deliberately
  un-runnable SurrealQL; resolve by replacing the whole delimited
  region with your query (the stdout script keeps its comment
  banners and never renders markers — it runs today). A successful
  save exits 0 — the draft is the artifact, so `generate --save &&
  migrate commit …` composes; the drift exit stays with the
  report-only runs (plain and `--check`). A converged run prints one
  `(converged)` line. The pile-up guard: regenerating a script the
  directory already drafts prints `note: an existing draft carries
  this exact script: <file>` and saves anyway (informational — the
  stale draft may hold fill-in-progress edits); `--save --replace <n>`
  takes the named draft's number instead of stacking — the draft must
  carry THIS exact script (the hash must match), then it is removed
  and the new draft saves under its number; a missing, committed, or
  differently-scripted target refuses (both hashes named) — and a
  multi-scope script under `--replace` refuses too (a group's members
  are not one script; delete a stale group by hand and regenerate).
  Adoption:
  `migrate generate schema.surdl` — the bare FILE, the schema's
  desired state from nothing — is the baseline recipe: every
  definition rides apply-pending and 0001 records the state to build
  toward. Run against a database already converged to the file, the
  OVERWRITE apply is a no-op on the schema and 0001 records where it
  stands (no slots; the ledger row is the ancestor every later
  migration descends from). Its byte-identical longhand is an empty
  `--from` side (`: > empty.surdl`).
- DRAFT GROUPS (multi-scope schemas): the file format wraps exactly
  one transaction per file, so a script spanning several scopes saves
  as ONE DRAFT PER SCOPE SECTION under one number —
  `0005a_<name>.surql`, `0005b_<name>.surql` — the letters ordering
  the group, the NAME order the apply order. Each member is exactly
  one transaction, carries its own scope's slot regions and its own
  hoisted `USE` context (the script's comment header rides every
  member), and shares the group's ONE header identity plus its own
  `-- group: 0005 a/3` line (the group's number — the ledger row's;
  the once-per-number contract holds, number 5 IS the group — the
  member's letter, the total). The save's stdout names every member
  and its commit command in order. `--replace` and `renumber` refuse
  a group member (delete the stale group by hand and regenerate); the
  pile-up note stays a single-draft feature. Committing follows the
  letters: a member's file name must carry its header's letter, an
  earlier member still uncommitted refuses a later member's commit
  (the group applies whole), the earlier members' commits write NO
  ledger region, and the LAST member's commit splices the block — the
  group records once (the number, the last member's filled hash, the
  base slug). Push applies a group whole (below); status drives the
  members' verdicts from the group's row and renders them grouped.
  A member's NAME and HEADER must keep agreeing — a lettered file
  name over a headerless draft (or the mirror) refuses the walk
  before anything is sent.
- `migrate commit <file> -m "message"` certifies a filled draft:
  refuses (exit 1) while any marker remains — each unresolved slot
  named — refuses a draft whose FILE NAME carries a different number
  than its header (or none — the ledger self-records the header's
  number while push/status walk the file names; `migrate renumber`
  is the repair), and refuses a draft carrying statements AFTER its
  transaction frame (the script must be exactly one
  `BEGIN TRANSACTION; … COMMIT;` block: the engine runs the frame and
  anything after it independently, so a tail failure would leave the
  frame committed — its ledger row self-recorded — while a push
  re-run skipped the file, the tail never applying) — then rewrites
  the file. The header is marked
  `-- status: committed sha256:<hash> <message>` (the hash is
  SHA-256 over the filled script body's normal form:
  `BEGIN TRANSACTION;` … `COMMIT;`, newline-normalized, minus the
  ledger region below — header edits are not drift, and the
  normalization is what makes an unchanged file's re-commit
  recognize itself; line endings normalize at parse, so the
  committed file is always LF). The source is pinned too: the
  header's `-- source-sha256:` (recorded at generate) must match
  what the re-loaded source canonicalizes to at commit — a source
  edited after generate refuses naming BOTH hashes (the ledger would
  store a revert substrate the script never applies); a draft
  without the line (legacy) carries no such check. A source that
  will not OPEN at commit (a run from another directory — the line
  resolves against the CURRENT working directory) refuses with the
  mechanism named and both ways out (commit from the generation
  root, or record an absolute path). The migration ledger's
  statements are appended
  INSIDE the script's transaction before its final `COMMIT;`:
  `DEFINE TABLE IF NOT EXISTS _surform_migrations` + typed fields
  (number, name, hash, message, applied_at, sdl) and ONE idempotent
  `INSERT IGNORE` keyed on the migration number — applying the file
  through any SurrealDB tooling records itself (run twice: one row;
  `applied_at` is `time::now()`, the true APPLY time). Numbers are
  unique per DATABASE, not just per directory (the record id IS the
  number) — one migrations directory per database. The appended
  region (banner through the final `COMMIT;`) is do-not-edit: edits
  there are not drift — they execute as edited, surfacing at apply
  time. The `sdl`
  column stores the canonical SDL of the state the migration
  converges to — commit re-loads the header's `-- source:` file
  (resolved against the working directory the commit runs from), so
  it must be present and clean (the revert substrate; a missing file
  refuses NAMING the mechanism and both ways out — generation root
  or an absolute path). Commit is
  once per file: unchanged re-commit → "already committed"; edited
  → "edited after its commit" (the ledger keeps the recorded hash;
  `migrate status` surfaces the drift). `--ledger-table` renames the
  ledger table in both statements. (A GROUP member commits per the
  group bullet above: the name's letter must match the header's, the
  letters commit in order, and only the last member's commit splices
  the ledger region.)
- `migrate renumber <file> <n>` renumbers a draft — the header's
  `-- surform migration NNNN` line and the file name take `n`
  TOGETHER (commit refuses the pair's disagreement — the one
  operation that cannot split the identity). A committed file
  refuses: the ledger records its number, and renumbering history
  would orphan the row — a merge-renamed committed file renames back
  by hand (`git mv`); `n` must fit the four-digit numbering
  (0001–9999) and no other draft in the directory may carry it;
  everything below the first header line rides through untouched. A
  group member refuses outright (the letter arithmetic and the group
  contract move together).
- `migrate push --endpoint <url> --ns <ns> --db <db> [--user U
  --pass P]` applies the directory's committed migrations to a live
  database through SurrealDB's raw HTTP `POST /sql` endpoint (never
  the SDK): number order, one HTTP call per file — the file's own
  `BEGIN TRANSACTION; … COMMIT;` wraps it, so a failed statement
  rolls the whole file back and the engine saw nothing. The ledger
  is read first (`SELECT number, name, hash …` through the same
  endpoint); ledger-recorded files (number + hash equal) skip, a
  number the ledger holds with a DIFFERENT hash refuses (another
  migration owns it — one migrations directory per database), the
  first failure stops the run with the engine's own message and the
  count applied before it, and a re-run resumes by the ledger. A
  draft GROUP (above) applies whole: every member in name order — a
  mid-group failure leaves the earlier members applied but UNRECORDED
  (the row is the last member's to write), so the re-run re-applies
  them (`DEFINE … OVERWRITE` is idempotent; a data-migration query
  filled into an earlier member RUNS AGAIN — keep those idempotent;
  the failure report says so); an applied
  group (the row carries the last member's hash) skips every member;
  a group only PARTLY committed blocks the run (refusal; the dry run
  classifies it `blocked`), and a structurally broken group (a letter
  gap, a member count disagreeing with the headers' total, a
  name/header disagreement) refuses before any request. A
  missing ledger table (fresh database) is an empty ledger — but
  ONLY when the not-found names the TABLE: a not-found on the
  namespace or database (a mistyped `--ns`/`--db`) and a permission
  failure are errors, never a silent "nothing applied". The first
  push applies everything and each file's ledger block creates
  the table. `--endpoint` takes the server ROOT (`/sql` is
  appended; a root carrying `?` or `#` refuses — it would corrupt
  the appended path); every connection coordinate falls back to its
  environment variable when the flag is absent — `SURFORM_ENDPOINT`,
  `SURFORM_NS`, `SURFORM_DB`, `SURFORM_DB_USER`, `SURFORM_DB_PASS`,
  `SURFORM_DB_TOKEN`
  (preferred for secrets; the flag wins, and a present-but-empty
  variable counts as given) — the Basic pair takes both
  or neither (neither speaks to a `--unauthenticated` server);
  `--token`/`SURFORM_DB_TOKEN` rides `Authorization: Bearer` (the
  caller minted it — one `POST /signin`; the pipeline never mints or
  refreshes) and is exclusive with the Basic pair: both kinds at once
  refuse before dialing, the refusal naming both spellings; a
  tty run with a user but no password ASKS (`password:` on stderr,
  one plain read — no credential ever prints on any path), and the
  coordinates never print on any path. `push --dry-run` answers
  "what would this push do?" with ZERO migration POSTs: the entire
  pre-flight (directory walk, drift checks, the one ledger SELECT —
  the endpoint is still required) and the plan print — would-apply /
  already-applied / skip-draft / would-refuse, that last (a ledger
  number held with a different hash, both hashes named) exiting 1,
  so the dry run gates CI; every pre-flight refusal refuses it
  exactly as it refuses a push. Uncommitted drafts skip (commit declares
  history; push ships it), and a committed file edited after its
  commit refuses the whole run before anything is sent. So does a
  committed file carrying statements OUTSIDE its one transaction
  frame — content after the `COMMIT;` close, or a second
  `BEGIN … COMMIT` block (the rollback guarantee needs exactly one
  transaction; commit refuses the same shape). A script
  whose text carries `USE NS … DB …` naming OTHER targets refuses —
  the whole file is scanned, the `USE` lines above the transaction
  frames included: a
  USE switches the session mid-request (verified against
  3.3.0), so the migration and its ledger self-record would land in
  a different database than the one push reads the ledger from —
  point `--ns`/`--db` at the names the schemas declare. A group's
  earlier members are the deliberate exception in part (each carries
  its OWN scope's USE — every request is its own session); the LAST
  member keeps the full rule (its script writes the ledger row).
  `--dir` and
  `--ledger-table` thread as on commit.
- `migrate status [--endpoint <url> --ns <ns> --db <db>] […]` compares
  the migrations directory against the live ledger — one
  `SELECT number, name, hash` through the same endpoint (status
  never applies anything). Per file: applied (the ledger's hash
  equals the file's CURRENT script hash), pending (committed,
  unpushed), draft (no committed header), DRIFTED (current ≠ ledger
  — edited after the database applied it, or another directory owns
  the number; both hashes named); plus ledger rows with no local
  file. A draft GROUP's members render grouped — the verdict is the
  GROUP row's, judged through the last member (`applied — group 0005
  a/2`; the recording member drifted renders DRIFTED, the earlier
  members applied — the one blind spot: an EARLIER member edited
  after the apply still renders applied (the row carries only the
  last member's hash; push's drift check is the guard)). Exit 0 when directory and ledger agree, 1 on any DRIFTED or
  missing — the CI gate for history integrity, not progress.
  WITHOUT an endpoint (flag or `SURFORM_ENDPOINT`) the run is
  OFFLINE: the local classification only — draft and
  committed-per-header per file, the ledger-derived state rendering
  `committed — ledger state unknown (no endpoint)` — nothing dials,
  no credentials are asked, exit 0 always (nothing can be DRIFTED or
  MISSING without a ledger). With an endpoint, status ALWAYS reads
  the ledger (an empty directory
  still has a ledger side), and the local walk is pre-flight (a
  missing directory, duplicate numbers, an unparseable file refuse
  before the dial).
- `migrate revert --to <number|hash-prefix> --endpoint … --ns … --db …`
  drafts the migration that undoes a recorded one: the ledger row's
  stored `sdl` column is the `--to` side and the LIVE database the
  `--from` side, fetched as `INFO FOR DB;` + one
  `INFO FOR TABLE <name>;` per table over the same endpoint and fed
  through the same compare `migrate generate` runs — the draft is
  the script `generate --from <live> --to <recorded>` would emit,
  slots riding wherever the recorded state orphans data. Two files
  land in the migrations directory: the next numbered draft and a
  `.surdl` SIDECAR holding the recorded state verbatim (the draft's
  `-- source:`, so a later commit re-records exactly that state —
  reverting the revert works; the sidecar must survive until that
  commit and is deletable after). NOTHING is applied: fill the slots,
  commit, push. The ledger table never joins the compare (no revert
  can draft its removal), and the compare runs at DATABASE scope:
  the recorded schema must declare `--ns`/`--db`'s database (a
  namespace-free schema compares under its database name), anything
  else refuses rather than draft a silent no-op. Numbers are 1–4
  digits; hash prefixes 4–64 hex characters; ambiguity names the
  candidates; an unreadable target never dials.
- `migrate pull --endpoint <url> --ns <ns> --db <db> [--user U
  --pass P] [--dir <DIR>]` produces the interchange the `--info`
  tags consume: `INFO FOR ROOT`, `INFO FOR NS`, `INFO FOR DB`, and
  one `INFO FOR TABLE` per table ride the same `POST /sql` endpoint
  push applies through — the scopes as separate queries in one POST,
  the tables in a second once the db result names them; NOTHING but
  INFO is ever sent. The results land as three files —
  `surform-info-root.txt`, `surform-info-ns-<ns>.txt`,
  `surform-info-db-<ns>-<db>.txt`, the db file carrying every
  table's child map under the reserved `"@tables"` key — rendered
  by the engine's own printer, the exact text the `--info` reader
  walks, and the ready-made `migrate generate` command carries the
  database tag only: the wider compares (root, ns) print as named
  advice lines — a scope the schema file does not manage classifies
  as removals, so read the drift report before running a wider
  compare's script. Under a restricted credential (a db-level
  bearer) a scope's permission refusal DROPS that scope — no file
  for it, the advice names the fuller credential that completes the
  pull — while any other refusal refuses the pull naming its scope;
  the db scope never degrades. A re-pull refreshes exactly the
  names it writes (hand-written interchange files are not pull's to
  touch; a degraded re-pull leaves an earlier full pull's files in
  place — ledgered #106);
  `--dir` moves where they land (created when missing; the printed
  command names them there). The coordinates and credentials resolve
  exactly as on push (flag over environment, both-or-neither, the
  tty ask, nothing credential-bearing prints on any path); a
  namespace/database name carrying a path separator or `=` refuses
  BEFORE the dial (it could not ride the file name and its
  `--info` tag), and every refusal —
  the endpoint, the credentials, a failed INFO, a short response
  array — names itself and writes nothing.
- `normalize` merges database state INTO the file — adopt what the
  database holds, elide engine defaults (`PERMISSIONS FULL` on fields,
  `BATCH 1000 START 0`), preserve what INFO drops (`strict;`,
  `concurrently;`, secrets), fold server-synthesized bookkeeping
  (`tags.*` subfields, relations' materialized `in`/`out`). Running it
  twice changes nothing the second time. Requires at least one
  `--info` tag (a scope-less run is a usage error — it would silently
  be `fmt`; `fmt` is the no-merge path).
- In CI: `surform migrate generate … --check || echo "schema drifted"`
  is the signal;
  `surform fmt --in-place && git diff --exit-code` catches hand-edits.

## Where the ground truth lives

In this repository, `spec/README.md` holds the design rules and
`spec/<statement>.md` the per-statement grammar — the authority this
skill condenses. When a spec and this skill ever disagree, re-verify
with the binary and trust the spec.
