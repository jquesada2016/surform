# SDL Spec — global conventions and syntax rules

Status: **all 16 DEFINE statements consolidated; `mixin` (SDL-only,
no DEFINE counterpart) added 2026-09-29; `include` (SDL-only file
composition) added 2026-10-04; `type` aliases (SDL-only named type
expansion) added 2026-10-25**.

Target: SurrealDB **v3.3** — the sole supported floor (the 2026-09-25
floor move; the embed pins `=3.3.0`, the shipped binary is 3.3.0). The
tool translates SDL → SurrealQL DDL and SurrealQL DDL → SDL through
the embedded official parser.

## Verification workflow

Grammar facts in these specs come from two sources, in order of
authority:

1. **The official parser source** — the vendored pinned crates are the
   ground truth (no `v3.3.0` git tag exists upstream and public `main`
   lags the shipped binary — do not clone a branch for ground truth):
   ```sh
   ls ~/.cargo/registry/src/*/surrealdb-{core,syn,sql,rpc}-3.3.0/src
   ```
   The grammar lives in `surrealdb-syn`'s `src/parser/` (AST
   definitions in `surrealdb-sql/src/`, emitters/`ToSql` beside them —
   the emitter is the authority on INFO output shape and clause
   order).
2. **The embedded engine** (2026-09-19; replaces disposable
   servers), when behavior must be observed: run the statement
   through the `surrealdb` crate in embedded mode via the SDK —
   in-memory or a local file, no sockets, no server instances.
   Tests and any real-DB check go through this path; the `surreal`
   CLI is not part of this repo's workflow. (The shipped `surreal`
   3.3.0 binary is the behavioral arbiter where the SDK path and the
   binary could disagree — none observed at the floor move.)
3. **The spec's own example fences are self-verifying.** A runnable
   example carries an info string: `sdl` marks complete statement(s)
   that must parse, emit, and pass the stage-2 DDL oracle;
   `sdl-body` marks a clause-level fragment, verified the same way
   wrapped in a minimal table statement (`table doc schemafull { …
   };`); `sdl-error expect=<codes>` marks a statement the strict
   pipeline must reject with exactly those codes (include.md's
   file-context contract). Unlabeled fences are grammar sketches — no
   claim.
   `tests/spec_examples.rs` checks every labeled fence per commit (a
   file with an Examples section must carry at least one), and the
   docs site renders these fences through docgen's pipeline
   (`sdl` fences get the live-editor treatment; `sdl-body` renders a
   build-verified static panel). The fence vocabulary is the
   docs/content one, so `sdl no-exec` marks the engine-gated shapes
   (module, bucket).

Version-skew note: the docs at surrealdb.com describe 3.3.0 grammar
SDL does not yet author (ACCESS `CONTEXT`, JWT `AUDIENCE`, USER
`PASSSCRAM`; `OPENID_CONNECT`/`SAML` remain parser-unverified) —
support lands in sequence after the floor move (TABLE
`LIGHTWEIGHT`/`INLINE EDGES` have landed, as has FIELD
`INLINE`); until then the readers fail closed on the remaining new
shapes (probe-pinned 2026-09-25).

## Design rules (settled)

1. **The header carries identity, not configuration.** The base shape is
   `keyword <name>`; per-statement grammatical elements may join it — a
   variant slot (`index hnsw <name>`), an endpoint arrow (`table user ->
   wrote -> post`, RELATE's own order), a signature (`fn name(params) ->
   ret`), or a type (`email: string`). Every option, qualifier, and
   expression lives inside the braces as a clause. Blocks: `{ … };`.
2. **Clause forms**
   - value clause — `key: <capture>;`
   - flag — `key;`
   - sub-block — `key { clauses };` (note: `key {` with no `:` stays in
     SDL parsing; `key: {` begins a SurrealQL capture whose first brace
     is part of the expression)
   - Clause keys may be **multi-word** (`default always:`, `on delete:`,
     `include original;`). Some statements carry **structural
     sub-blocks** whose head is not a clause key (API method groups
     `get, post { … };` and `any { … };`).
3. **Value clauses come in exactly two kinds, declared per clause in
   each statement's grammar:**
   - **keyword clause** — special-cased by SDL: the value matches a
     fixed keyword set, a simple literal (string, number, SurrealQL
     duration), or a small SDL-lexed grammar — comma lists of
     keywords/idents, parenthesized numeric pairs (`bm25: (1.2, 0.75);`),
     name paths (`fn::strip_markup`, `silo::org::pkg::<1.2.0>`), or
     f-strings (`f'…'`). Never passed to the SurrealQL parser.
   - **expression clause** — raw SurrealQL: the `:` switches the lexer
     into capture mode and everything after it — bare, parenthesized, or
     braced, single- or multi-line — is captured verbatim until a `;` at
     **bracket depth 0** (any `(`, `{`, `[` opened during capture raises
     depth, its closer lowers it; content inside string literals `'…'`/
     `"…"` is skipped — SurrealQL `\` escapes are honored — so `;` or
     brackets in strings never terminate). That depth-0 `;` is consumed
     by SDL and returns control to SDL parsing. The captured span is
     then handed to the embedded official SurrealQL parser.
     **Comment model inside captured spans:** SurrealQL has four comment
     syntaxes — `--`, `//`, `#` (line comments) and `/* … */` (block,
     ends at the first `*/`, no nesting; unterminated is an error). All
     four are legal input inside captures, and the capture scanner must
     skip their content (a `;`, quote, or bracket inside a comment
     never affects termination; line comments end at LF, CRLF, lone CR,
     U+2028/U+2029/U+0085 — the scanner mirrors `syn/lexer/byte.rs`).
   The scanner's skip set is: string literals (prefixed forms `d'`, `s'`,
   `u'`, `b'`, `f'`, `r'` with either quote style, with `\` escapes),
   comments (above), **quoted identifiers** (`` `…` `` and `⟨…⟩``,
   whose interiors may legally contain brackets, semicolons, quotes,
   and comment openers; `\` escapes honored), and **regex literals**
   (`/…/` — the interior's `\` escapes the next byte and the first
   unescaped `/` closes; the opener is judged by the prime-vs-infix
   predecessor discrimination the migration marker scanner and the
   printer repair share — a clause-head or operator-word predecessor
   opens, an operand-ender divides; a `/…/` whose interior holds
   quotes or semicolons therefore captures whole, and an unterminated
   one refuses at its opening `/`, SDL-E0001's regex variant).
   A residue stays ledgered (issue #93): a regex directly after a
   clause-head keyword at odd word-chain parity reads division — the
   same clause-head-operand ambiguity family as issue #62 — and
   captures as content; the structural fix is parser-driven spans.
   Comments inside captures
     are **dropped at canonicalization** (spans are re-printed from the
     embedded parser's output, which carries no comments) — fn bodies,
     event/api `then:` blocks, and views therefore cannot keep them.
     Outside captures, SDL comments are `//` and `///` only, and rule 5
     gives `//` lines a layout-preservation story at the canonical
     writer.
     The lexer may use the same scan-to-`;` mechanism for both kinds;
     the difference is what validates the result (SDL grammar vs the
     SurrealQL parser). **Comma-list expression clauses** (declared
     per-statement, e.g. index `fields:`, event `then:`) split the
     captured span at depth-0 `,` under the same scanner skip set;
     each item goes to the embedded parser. Consequences for
     expression clauses: a `;`
     inside balanced braces is legal mid-capture — `authenticate: { if
     $auth.blocked { throw 'no'; } };` captures the whole block — and
     SurrealQL's own semicolon semantics apply inside verbatim (omit the
     trailing `;` where the body must return its value, e.g.
     `signup:`/`signin:`).
4. **Types pass through verbatim** — `option<T>`, `record<T>`,
   `array<T, N>`, `set<T>`, unions (`number | string`), literals,
   `geometry<T>`, inline object schemas (`{ a: string, b: number }`),
   and the remaining SurrealQL kinds (`any`, `null`, `bool`, `bytes`,
   `decimal`, `range`, `regex`, `function`, `table<T>`, `file<T>`,
   literal bool/number/duration/array kinds — `sql/kind.rs` is the
   authority; the lists here are illustrative, not exhaustive). Types
   are **SDL-lexed, never expression-captured**: after `name:`,
   the SDL type parser (mirroring SurrealQL's kind grammar, including
   `<…>` interiors and brace-bearing object types that end at their
   balanced `}`) consumes the type; the field header then ends at `=`,
   `;`, or the clause block `{`. **Kind normalization:** SurrealQL's
   parser desugars `option<T>` to the union `none | T` and normalizes
   unions (flatten, dedupe, singleton-collapse) — so INFO returns
   `none | T` for SDL's `option<T>`. Types compare by parsed kind AST
   under that normalization (`option<T>` ≡ `none | T`, `int | int` ≡
   `int`); the canonical SDL spelling folds a union that is exactly
   `none | T` back to `option<T>`. **Type aliases**
   ([type-alias.md](type-alias.md), 2026-10-25) ride this grammar:
   a bare Capitalized word at a kind position is an alias REFERENCE
   (the Capitalized/lowercase split is the whole disambiguation —
   SurrealQL's kind heads are lowercase), resolved against the
   document's alias set by the check-class pass and rewritten to the
   expansion's text before emission; the kind text an authored file
   carries still rides verbatim (the writer never resolves a
   reference), and the strict grammar the reverse readers use accepts
   no reference (INFO stores only concrete kinds).
5. **Comments.** `//` is a plain human-readable comment. It is not a
   definition: comparisons (the differ, `same_definition`, the fmt
   fixed point) strip it, and codegen emits none. But it IS layout —
   the canonical SDL writer (fmt, normalize output) re-emits every
   `//` line at its anchor. The anchor model, outermost in: a comment
   above a statement is its leading block; a comment above a CLAUSE
   (at any depth — statement blocks, sub-blocks, per-operation
   permission groups, api method groups) anchors to that clause and
   rides it through the writer's grammar-block reordering; a comment
   on the SAME LINE as a terminator (`key: value; // note`) is that
   clause's after-note and re-emits on the clause's own line (this is
   how a complex SurrealQL expression clause carries its explanation);
   a note after a statement's own terminator joins the statement's
   final line. Lines inside a block that anchored to no child print
   just before that block's close; lines after the last definition
   print at end of file. A clause's before-notes flatten onto the
   statement when the writer takes a short form (they print above it
   and re-capture as leading lines); a same-line after-note keeps the
   block form (the short-form guard — one-line forms cannot re-capture
   a clause note). A note on a clause the writer itself elides drops
   with it (the graphql `none` family, which the representation fold
   prints as nothing). The writer also separates adjacent top-level
   statements with one blank line — except a run of adjacent
   `include` statements, which packs tight as one import group (the
   group still breaks from every other definition). A statement's
   leading and trailing comments emit inside its own group, so the
   break always falls between definitions and never detaches a comment
   (or a `///` doc block) from its anchor. A comment between a
   statement's keyword and its name (or any other seam with no anchor
   before the next) re-anchors to the next anchor — nothing is lost.
   Comments inside captured spans (fn bodies, `then:` blocks, view
   queries, expression clause values, fn parameter lists — see
   the comment model in rule 3) have no SDL anchor and drop; a field
   kind's `//` lines re-anchor like any other anchorless line. Text after `//` rides verbatim (trailing whitespace
   trimmed); the writer emits `//` plus that text, so fmt is
   idempotent. `///` is a doc comment: a contiguous `///` block
   immediately above a definition (no blank line) becomes its
   SurrealQL `COMMENT` (lines joined with `\n`); authored `//` lines
   above it print above it. `///` is accepted **only where the target
   statement's SurrealQL grammar has a `COMMENT` clause** — `sequence`
   and `config` lack one, and `///` is rejected there.
6. **No sugar.** `unique` is a variant slot, not a flag; defaults are
   expressed by omission. The only implicit behavior is the
   doc-comment → `COMMENT` mapping — with **one licensed deviation
   class, compile-time textual expansion**: `mixin` spreads
   ([mixin.md](mixin.md), 2026-09-29) and `type` aliases
   ([type-alias.md](type-alias.md), 2026-10-25). A spread expands
   into member definitions and an alias into kind text, and nothing
   more: no inheritance, no merge, no runtime counterpart — the
   database only ever sees the expanded `DEFINE`s and kinds.
7. Statements are `;`-terminated. Indentation is cosmetic. **SDL is
   order-DEPENDENT in clauses, order-INDEPENDENT in definitions** (the
   declarative namespace, 2026-10-05/06). Within a statement, repeated
   clauses fold with **last one wins** — exact parity with
   SurrealQL's positional clause loops. Across statements,
   definitions may stand anywhere in the stream — above, below, or in
   another file of the composition ([include.md](include.md)):
   `build` emits DDL in the COMPILER's own order — a stable
   topological sort over the engine's define-time-evaluated edges
   (a view over the tables its top `FROM` names — `TbNotFound`
   otherwise; a fulltext index over its analyzer and the fields it
   names on schemafull tables; a param's `VALUE`, a sequence's
   sides, a user's/access durations, a jwt `url:`/`key:`, a bucket
   `backend:`, api middleware arguments, and `config default`'s
   sides, over the params and fns those expressions read — and over
   the `mod::`/`silo::` modules those expressions call (the engine
   resolves `mod::<first-segment>` and the silo catalog name at
   define) — the
   engine evaluates each at define: a forward fn call is
   `FcNotFound`, a forward param silently stores `none`). Authored
   order is the sort's tie-break, so an already-ordered document
   emits byte-identically, scope sections never move (a
   `namespace`/`database` heads its section), and `fmt` never
   reorders — it is presentation, not semantics. A reference the
   engine evaluates at RUNTIME (fn bodies, table/field permissions,
   events, field defaults and computed bodies, access
   signup/signin/authenticate) carries no edge at all: a fn
   referencing a table referencing a fn, in any arrangement, is
   legal. Three failure classes remain: the same family + name defined
   TWICE — any file, any position — is an error (SDL-E1062; the one
   chosen loss of the model is last-wins overrides between
   definitions; `OVERWRITE` survives only as the emitted DDL's
   re-apply default); a define-time CYCLE — a view chain reading
   itself, a param chain feeding itself — is SDL-E0015 with the
   chain named, because no application order exists; and a
   define-time dependency the sort cannot place — the target stands
   in a LATER scope section, after the next `namespace`/`database`
   statement, and emission never crosses a leader — is SDL-E0016.
   Within a scope section the guarantee is unconditional FOR
   IN-DOCUMENT TARGETS — a reference nothing in the document defines
   stays legal (a live database may hold it; the advisory lints'
   territory). The order-free
   mixin spread ([mixin.md](mixin.md)) resolved globally since its
   landing: a `...<name>;` resolves against the document's WHOLE
   mixin set; a name resolving to nothing is an error (SDL-E1061),
   never a silent empty expansion; a `mixin` statement itself emits
   nothing. The composition's one positional rule is scope:
   `namespace`/`database` head the composition preamble (before the
   first include), and included files carry no scope statements.
8. Escape hatch: `raw { … };` forwards verbatim DDL for anything SDL
   cannot express (legal SurrealQL shapes outside SDL's surface — e.g.
   legacy `VS` scoring data — and future statements). Its body is
   captured brace-balanced with the global scanner discipline (rule 3's
   full skip set: strings, comments, quoted identifiers), like `fn`
   bodies. Top-level placement only — `raw` is a reserved word in every
   block context (quoting or the `field` prefix escapes a field named
   `raw`); the interior emits with only its edges trimmed, every other
   byte verbatim (comments and spelling included); validation is
   advisory, never a hard error — a body the embedded parser rejects is
   still carried and emitted (rule 8 exists for exactly that class),
   flagged by the linter (SDL-L0012); and it takes no doc
   comment (no COMMENT clause to hang one on).
9. **Parity with the SurrealQL grammar.** Where SurrealQL makes a clause
   optional, the SDL clause is optional — omission emits nothing and
   SurrealQL's own default applies. SDL's deliberate deviations from
   strict omission-parity, each licensed in its file: (1) table
   `TYPE NORMAL` emission for bare tables, (2) the field type
   requirement, (3) tables always emit the schema flag
   (`schemaless;`/`schemafull;` never elided — INFO byte-stability).
   SurrealQL's keyword aliases are accepted wherever SurrealQL accepts
   them, spelled lowercase like all SDL keywords: `ns` ≡ `namespace`,
   `db` ≡ `database`, `kv` ≡ `root`, `columns` ≡ `fields`, … These are
   lexer-level aliases — the SurrealQL lexer maps the `NS`/`DB`/`KV`
   identifiers onto the canonical keyword tokens (verified live:
   `DEFINE ACCESS … ON NS` and `ON KV` both parse and execute) — and
   codegen always emits the canonical full word (`on: kv;` →
   `ON ROOT`). SDL keywords are lowercase only —
   SurrealQL's case-insensitivity is not inherited; codegen
   uppercases keywords when emitting DDL. Identifiers follow
   SurrealQL's rules (bare idents; `` `backticked` `` when colliding
   with keywords) — SDL accepts what SurrealQL accepts. Duration
   literals follow SurrealQL: integer + unit (`ns`, `us`/`µs`, `ms`,
   `s`, `m`, `h`, `d`, `w`, `y`), compounds like `1h30m`; integers
   only.
10. **Discriminated statements are ADTs.** Where SurrealQL's grammar
    branches on a keyword (ACCESS `type:`, TABLE normal/relation/view,
    FIELD regular/computed, INDEX unique/count/fulltext/hnsw/diskann, …),
    the SDL AST is an enum: the discriminant selects the variant; each
    variant declares its own clauses with their required/optional
    status, and validation applies only to the selected variant.
    Required vs optional follows SurrealQL's grammar (rule 9).
11. **Current syntax only.** SDL covers the current, non-deprecated
    SurrealQL grammar of the sole supported floor: SurrealDB **3.3**
    (2026-09-25 — the former 3.2 emission target and its
    `--target` downgrade machinery retired; before that date `module`
    had targeted the documented 3.3 grammar ahead of the floor as the
    one licensed exception). Deprecated or removed forms — `DEFINE
    SCOPE`, `SEARCH ANALYZER`, `DEFER`, `TYPE future`, `DEFINE MODEL`,
    MTREE/standalone-`VECTOR` indexes — are excluded (verified: none
    parse in the pinned parser).

## Grammar notation (used by every statement file)

- **The sigil is the shape (2026-09-27).** A clause key is
  one or more words; comma-listed keys share one value
  (`select, create: full;`), and api method heads comma-list into one
  shared block (`delete, get { … }` — rule 2: heads, not keys). `:`
  appears exactly when a value follows, ending the key
  (`permissions: full`, `on delete: ignore`); a `{` opens a sub-block
  only where a key ends — after `:` or `=` it belongs to the value
  (`then: { … };`, `param $x = { a: 1 };`). A key with no `:` and no
  block is a flag, and flags may be phrases (`schemafull;`,
  `include original;`). Three bodies are SurrealQL verbatim, not
  clause grammar — the view's query (plus its trailing SDL clauses),
  the function's body, and `raw`'s contents. A statement-owned `=`
  assignment short form (`param`, `sequence`, the field computed
  header — the Assignment short form, below) is that statement's own
  syntax, not a clause sigil. A non-view table block also carries
  sigil-led **spread entries** — `...<name>;` — which are body entries
  (like nested definition heads), not clauses: no key, no colon, and
  the leading `...` is the whole grammar
  ([mixin.md](mixin.md)).
- `<expr>` marks a **SurrealQL expression clause** (captured from `:` to
  the depth-0 `;`, parsed by the embedded SurrealQL parser); inline
  keyword sets, `<string>`/`<duration>`/`<number>`, and other literal
  shapes mark **special-cased keyword/literal clauses** (validated by
  SDL).
- Grammar blocks group clauses under `// required:` and `// optional:`
  comments; alternative groups state their constraint inline. A
  statement's discriminant clause (e.g. `type:`) is implicitly required —
  it selects the variant.
- Discriminated statements show one sub-grammar per variant plus an ADT
  sketch; `field?` in the sketch means `Option<…>`.
- Every definition (and sub-definition) may be preceded by a `///`
  doc-comment block, shown in grammar blocks as `/// <doc comment>` —
  it becomes the SurrealQL `COMMENT` (rule 5; `sequence` and `config`
  excepted).
- **Permission clauses** (`permissions:`) — exactly two forms exist in
  SurrealQL, and each statement supports at most one of them (or none):

  1. **Single-value form** —
     - `permissions: none;`
     - `permissions: full;`
     - `permissions: <expr>;` — **only on the statements using
       `parse_permission_value`** (api, bucket, config-api, function,
       module, param): SDL writes no `where` keyword — the clause
       context makes it implicit; codegen emits
       `PERMISSIONS WHERE <expr>`. Field and table use the
       per-operation parser, which accepts only `NONE | FULL | FOR` —
       a bare `permissions: <expr>;` is invalid there (their
       `none | full` shorthand is listed below).
  2. **Per-operation form** —
     ```
     permissions {
       select: none | full | <expr>;
       create, update: none | full | <expr>;
       delete: none | full | <expr>;      // TABLE only — fields reject delete
     };
     ```
     Operations are comma-groupable; codegen emits
     `PERMISSIONS FOR <ops> WHERE <expr>` per line (with `NONE`/`FULL`
     spelled out). Omitted-operation semantics are **asymmetric and
     verified**: in a TABLE block, unlisted operations are NONE (the
     parser seeds deny); in a FIELD block, unlisted operations are FULL
     (the parser seeds allow). SurrealQL also accepts the single-value
     `PERMISSIONS NONE | FULL` on tables and fields (verified) — SDL
     allows it as the all-operations shorthand. Note the sub-block is
     colon-less (`permissions {`, not `permissions: {`) — a colon after
     the key would start a SurrealQL expression capture whose first
     brace belongs to the expression (design rule 2); all SDL
     sub-blocks follow this.

  Per-statement support (ground-truthed from the 3.3.0 parser — the
  vendored `surrealdb-syn`/`surrealdb-sql` crates; originally from the
  v3.2.4 parser call
  sites — `parse_permission_value` = single-value; `parse_permission` =
  per-operation):

  | statement | form |
  | --- | --- |
  | access | — (no permissions clause) |
  | analyzer | — |
  | api — per action group and `any` | single-value |
  | bucket | single-value |
  | config `api` variant | single-value |
  | config `graphql` / `default` | — |
  | database | — |
  | event | — |
  | field | per-operation (`select`/`create`/`update`; no `delete`) + single-value `none|full` shorthand |
  | function | single-value (default full) |
  | index | — |
  | module | single-value |
  | namespace | — |
  | param | single-value |
  | sequence | — |
  | table | per-operation (all four ops) + single-value `none|full` shorthand |
  | user | — (uses `roles:` instead) |

- A statement with a fixed, exclusive variant set names the variant in
  the header (`config api { … };`, `index hnsw <name> { … };`) rather
  than nesting a single sub-block. Where the statement also has a name,
  the variant slot sits between keyword and name. A variant may also be
  **implied by syntax or by a bare default**: a table whose header
  continues with an arrow is a relation, spelled in RELATE's own order —
  `table [<from-list>] -> <name> -> [<to-list>]`, the name always
  flanked by `->` on both sides, either side list omittable
  (`table -> <name> ->;` is the unconstrained relation, equivalent to
  `table relation <name>;`), `any` forces ANY,
  and a bare table is NORMAL
  (emitted explicitly — SDL's fail-closed default, deviating
  deliberately from SurrealQL's bare-schemaless→ANY materialization).
- **Dependent clause groups** use the flag-plus-sub-block pattern: a bare
  flag selects defaults (`async;` → retry 1, maxdepth 3; `reference;` →
  on delete ignore), and a colon-less sub-block configures the parameters
  (`async { retry: 5; maxdepth: 8; };`). Parameters never appear as
  standalone clauses outside their parent's block. A bare flag plus a
  block of the same key is legal (the block configures; `async; async
  { retry: 2; };` ≡ `async { retry: 2; };`).
- **The clause block is optional when empty** — `namespace acme;` is
  equivalent to `namespace acme { };`.
- **Assignment short form:** a statement whose only practically-required
  clause is a single value may write `keyword <name> = <expr>;`
  (`param $limit = 50;`, `sequence n = 1000;`, computed field
  `slug: string = string::slugify(title);`); any additional clause
  requires the block form — a trailing block after `=` is rejected
  because a depth-0 `{` following an expression is an object literal
  under the capture rule.
- **`fn` is the one two-block statement:** after the signature (fn
  keyword, `$`-prefixed params, `->` return — SurrealQL-faithful), the
  first `{ … }` is the verbatim SurrealQL body (captured brace-balanced,
  string- and comment-aware per rule 3's comment model) and the optional
  second `{ … }` holds ordinary SDL clauses (`permissions:`,
  `graphql { … }`).
- **Keyless body blocks:** a variant whose only content is a single
  expression takes it directly, `;`-terminated, with no clause key — a
  view's block is its query (`table view stats { select …; };`),
  optionally followed by ordinary clauses (`permissions { … };`).
- **Expression rendering at codegen:** captured spans are parsed by the
  embedded SurrealQL parser and **re-printed from its AST** — never
  echoed as raw source. The embedded printer renders statement-like bare
  expressions wrapped in `( … )` and braced blocks as blocks; either
  authoring form is legal and yields valid DDL. A few clause slots print
  unwrapped by the emitters (e.g. API `THEN` actions) — the per-statement
  files note their slot's behavior. One embedded-print quirk repairs on
  SDL's side: upstream's `Function::Module` arm prints `mod:: name::fn(…)`
  (a stray space its sibling arms do not carry — one the engine's own
  INFO print shares); the canon keeps the glued spelling, repaired at
  both print doors (the clause slice and the read path's re-print)
  outside strings, comments, regex literals, and quoted identifiers. The differ compares expressions
  over their canonical text, modulo the single-statement block and
  balanced-paren unwrap (the printer keeps a braced single statement
  braced and parenthesizes the bare form — the unwrap reduces both to
  one text), so `signin: select …;` and `signin: { select …; }` are
  equivalent.
- **Expression layout at the writers:** the canonical expression text
  is the embedded printer's own single-line print and stays the
  canonical form everywhere (the AST's `canon`, differ comparisons,
  diagnostics). The two human-facing writers — DDL emission and the
  canonical SDL writer — additionally run a **layout pass** over values
  that would push their line past the width budget — 100 columns by
  default (`fmt`/`normalize` take `--max-width`; 2026-09-28 raised the
  const from the official formatter's 80 default, 2026-10-01 made it a
  writer parameter): logical `AND`/`OR`
  chains break before the operator — loosest operator first, so an
  `OR` group sits per line and its `AND` comparisons break within the
  group only when the group itself overflows — function-call argument
  lists and array/set/object literals break one item per line, and
  statement blocks print one statement per line — a Wadler-Lindig
  document algebra ported from the official SurrealQL formatter
  shipped in Surrealist (`@surrealdb/surql-fmt`). Every token still
  comes from the embedded printer (leaf shapes print through it;
  operand parentheses reuse its own predicates, prefix operators `!`/
  `-`/`<kind>` included), a flat render of a laid-out value is
  byte-identical to the canon or the canon rides verbatim, and
  laid-out values reparse to the same AST — `fmt` stays a total,
  idempotent function. Two audited exceptions to byte-identity, both
  reparse-verified against the canon's own tree: a broken mixed
  `AND`/`OR` chain wraps with explicit parens around its `AND` groups
  (parens that exist only in the broken layout, making the precedence
  the flat form implies visible), and the embedded print's one
  non-flat detail — a bracketed IF/ELSE branch holding several
  statements prints one statement per line even in the flat ToSql —
  is collapsed to one line before the layout applies. Values shorter
  than 40 characters never break (their line's length comes from
  elsewhere).
- **Statement skeleton layout (2026-10-01):** the DDL emission's
  clause skeleton is itself one width-fitting group — the surql-fmt
  `shouldStatementBreak`/clause-per-line model. The statement head is
  glued (keyword, name, and the header clauses: ON/TYPE — a relation's
  sides and a view's `AS SELECT` included — FIELDS, OVERWRITE, and
  SDL's flag riders); every other top-level clause is a break
  candidate. A statement whose one-line spelling fits the budget
  emits byte-identical to the old glue; one past it lays
  clause-per-line, each clause one indent unit in, its expression
  values re-laid at their final columns (so a value's continuations
  nest under its clause instead of carrying the next clause glued
  after them). View bodies and SELECT values lay out by the same
  model's SELECT skeleton: projections one per line indented under
  `SELECT` (when there are several and the statement breaks), then
  `FROM`/`WHERE`/`GROUP BY` each on their own line at the
  statement's indentation. A clause that is itself one unbreakable
  token run (a quoted verifier, an HNSW parameter chain) rides its
  line past the budget — the model breaks BETWEEN clauses, not
  inside a clause's own parameters. The same total-function and
  reparse guarantees as the value pass hold.
- **`<fn-path>` notation:** an SDL-lexed name path — idents joined by
  `::`, prefixed `fn::` or `mod::` where the consuming clause's grammar
  requires or permits a prefix (analyzer `function:` requires one;
  middleware names take it bare or prefixed; graphql include/exclude
  lists require the `fn::` prefix); `mod::` is gated on the
  experimental surrealism capability; never a call. Segments may be
  keyword-like words (identifier-like token classes — Keywords,
  Languages, Algorithms, Distances, VectorTypes — are accepted bare as
  names and path segments, SurrealQL parity).
- **Reserved words** in block context: statement keywords (`table`,
  `field`, `index`, `event`, `fn`, `access`, …), variant words (`any`,
  `view`, `relation`, `unique`, `count`, `fulltext`, `hnsw`, `diskann`,
  `normal`), clause keys (`on`, `type`, `permissions`, `drop`, …), and
  `raw`. The `field` prefix escapes field names that collide with any
  active clause key.

## Codegen output model

1. **Session context is emitted.** After `DEFINE NAMESPACE <ns>;` the
   script emits `USE NS <ns>;`; after `DEFINE DATABASE <db>;` it emits
   `USE NS <ns> DB <db>;` — the `db` binds to the nearest preceding
   `namespace` statement in the file. *(Amended 2026-09-19: binding is
   best-effort — no statement requires another. A database with no
   preceding namespace emits its `DEFINE` without a `USE` line and
   without an error; convergence and diffing operate on definitions,
   never on session context.)* The emitted `.surql` is
   standalone-runnable (`surreal sql -e … < file.surql`) and correct
   when a file crosses ns/db boundaries.
   Recommended layout remains one namespace + one database per file.
2. **`OVERWRITE` is the default for every emitted `DEFINE`.** SDL is the
   declared desired state — applying an SDL file *converges* every
   definition it contains to match it (verified: plain `DEFINE` errors
   on re-apply — `The table 'x' already exists`; `OVERWRITE` redefines
   cleanly). `CONFIG` mandates `OVERWRITE` for its singletons. Sole
   exception: **define-once users** (see [user.md](user.md)). Emit
   templates and examples in the per-statement files **omit the
   `OVERWRITE` prefix for readability — implementations always emit
   it** (positioned after the statement keyword, before the name).
   *(The 2026-10-06 migration tooling kept this rule
   unchanged: `migrate generate`'s defines are `OVERWRITE`
   unconditionally — no `IF NOT EXISTS` or plain-`DEFINE` codegen
   flag exists.)*
   **Mixin expansion precedes emission** ([mixin.md](mixin.md)): a
   spread expands to its member statements in place at the spread's
   position — explicitly overshadowed members and members a later
   spread's same-keyed member beats are skipped (last-occurrence-wins);
   a `mixin` statement emits nothing.
   **Alias expansion precedes emission**
   ([type-alias.md](type-alias.md)): every kind referencing an alias
   is rewritten to the expansion's text (chained aliases resolve
   recursively) and a `type` statement emits nothing — the emitted
   DDL can never carry an alias name, which is what makes the
   feature's meaning independent of the server version consuming it.
   **Composition precedes expansion** ([include.md](include.md)): the
   loader flattens the include tree (parse each file separately,
   concatenate the statement lists — file-local spans preserved) into
   the stream expansion and emission already consume; an `include`
   statement itself emits nothing and never survives past the
   flattening.
   **Ordering precedes emission** (rule 7, the declarative
   namespace): the lowered stream sorts — a stable topological order
   over the frozen define-time edge table, authored order the
   tie-break, scope sections immovable — before the writer walks it;
   the `USE` session lines and the ROOT-region hoist of `config
   default` read the SORTED stream, so they ride whatever order the
   sort chose.
   **Target-version policy:** there is exactly one emission target —
   SurrealDB **3.3** (as amended 2026-09-25: the former 3.2-vs-3.3
   choice retired together with its `--target` flag and the SDL-E2xxx
   rejections; a module's `FROM … UNSIGNED` form is the only
   spelling). Two implicit-breakage guards: (a) codegen
   backtick-escapes identifiers against the **full keyword set** of
   the supported target, so a future keyword acquisition can never
   reinterpret an SDL identifier (a floor bump re-extracts the set);
   (b) load-bearing defaults are always emitted explicitly (bare
   tables emit `TYPE NORMAL`; tables emit the schema flag), the
   linter covers the security-adjacent omissions, and the
   normalization loop absorbs any other server-side default drift
   between versions.
3. **The migration is the convergence script.** Overwriting never
   removes definitions absent from the SDL; the migration tool
   compares SDL against `INFO` and emits BOTH directions as one
   script — the SDL-only and drifted definitions as `DEFINE …
   OVERWRITE` from the authored SDL side (defines first), the
   database-only ones as removals (last).
   *(Implemented 2026-09-24 (the removal half) and 2026-10-06 (the
   convergence emitter, retiring `surform diff` for `surform migrate
   generate`): `surform migrate generate` prints the drift report and
   the convergence script. Defines render from the AUTHORED
   statements — an SdlOnly or Diverged finding carries the SDL side
   by construction, so no raw-body adoptee and no sentinel can leak
   through a define — in the compiler's emission order (rule 7's
   sort), with the one migration-only addition: a standalone
   field/event/index define renders inside its host table's emission,
   after the host's `DEFINE TABLE` (which must run first when the
   database holds the table in a form the engine refuses to hang a
   member on — a LIGHTWEIGHT relation) and before the nested members
   (a nested index naming the child refuses a SCHEMAFULL table that
   lacks it — `FdNotFound` — and the table's emission is one unit, so
   no order of separate statements satisfies both edges), and a
   member the authored table nests renders once, inside that
   emission. A raw-authored define rides the same rule through its
   header's `ON <table>` edge (its host's emission unfolds so the
   raw define runs between the head and the members — it cannot
   join the nested list, codegen prints index/event/field members
   only); a raw body whose header carries no readable `ON` keeps
   its sort position. A settle pass behind the sort synthesizes the
   define-time edges a raw body carries or hides (a raw body
   registers nothing in the order pass's dependency registry; the
   readers re-parse the body and walk EVERY statement of it — the
   block renders contiguously, so the body's edge set is the union
   of its statements' edges, an edgeless statement leaves its
   siblings' standing, and a body that fails to parse entirely reads
   no edge — fail-closed, the engine refuses it visibly), which
   closes the frozen edge table for both parities — the typed graph
   rides the sort, each raw end rides the pass: a
   view whose source table defines in the same scope sorts behind it
   — either side raw or typed, the engine walks the view's source
   tables at define and refuses `TbNotFound` otherwise — a raw
   fulltext index sorts behind its analyzer (the
   `FULLTEXT ANALYZER` clause place.rs's `raw_index_analyzer` reads;
   unreachable through the family order today — Analyzer defines
   before Index — but the pass holds the invariant), and the
   value-read edges (the frozen table's rows 6–13) hold whenever
   either end is raw: a reader family's define-time value slot — a
   param's `VALUE`, a sequence's `BATCH`/`START`/`TIMEOUT`, a user's
   `DURATION` sides, an access's jwt `url:`/`key:` + `DURATION`
   sides, a bucket's `backend:`, api middleware args, `config
   default`'s sides — sorts behind the param/fn/module it reads when
   that target defines in the same scope, reader or target (or both)
   raw (a raw-defined param read early rolls the transaction back;
   an access `url:` read early silently stores `'NONE'`). The pass
   orders by the stable topological sort of its synthesized edges and
   prunes the cycles only raw readers can close (a typed cycle is
   E0015's business, but the raw bodies' reads are invisible to it):
   an unorderable reader cycle keeps its positions and fails visibly
   at the engine.
   The removals keep the
   original contract: every `REMOVE` carries
   `IF EXISTS`, which no-ops on absence, so re-runs are idempotent
   for typed statements and single-statement raw bodies —
   the remove-side mirror of `OVERWRITE` (it does not soften
   catalog-integrity refusals — an analyzer still referenced by a
   fulltext index errors `AzInUse`); a multi-statement raw block
   re-pairs by its first header alone on re-run, so its unpaired
   tail statements re-drift (the ledgered corner); referenced
   before referencing
   (children Event/Field/Index, then Table, then the Analyzer its
   indexes point at — `REMOVE DATABASE` takes its namespace from the
   session), then `REMOVE NAMESPACE` lines last; the script emits the
   scope's `USE NS` / `USE NS DB` session lines (codegen model item
   1), and collapses a scope whose owning namespace/database the SDL
   never declares to the single `REMOVE NAMESPACE`/`REMOVE DATABASE`
   — reachable through the library API and the
   version pair (the `--from`/`--to` mode): the DB pair's `--info`
   tag validation (E3005) refuses undeclared scopes — the data-loss
   guard where the database side is real — while the version pair
   reaches the collapse, its WARNING anchors carrying the data-loss
   role. The script wraps ONE `BEGIN
   TRANSACTION`/`COMMIT` PER SCOPE SECTION, each section's `USE`
   switch OUTSIDE its frame — the 2026-10-08 amendment of the
   original one-transaction wrap (an alleged silent no-op of
   statements after a mid-transaction `USE` did not reproduce under
   matched conditions on either 3.3.0 engine, embedded or `/sql` —
   the reproducer had captured the header's scope — so the posture
   is the one needing no
   engine behavior: a session switch never rides inside a
   transaction, and a failed statement rolls its own section back,
   sections before it staying applied). DDL rides transactions —
   probed against the embedded 3.3
   engine (2026-10-06): defines, removes,
   `REMOVE DATABASE`/`NAMESPACE` included. One executed
   boundary (2026-10-06): a fulltext index
   over a record-holding table builds at define and reads the
   PRE-transaction catalog, so an analyzer the same transaction
   defines is invisible to it — that pair refuses in either order;
   the repair is a converged analyzer (an empty table's build defers,
   so the migration-time pair lands). The migration FILE format
   wraps exactly one frame, and the draft writers keep that
   invariant per file: `generate --save` writes a multi-scope
   script as a draft GROUP — one draft per scope section, lettered
   `0005a_`/`0005b_`, applied in name order, the ledger row
   recorded once from the last member — while `revert` still
   refuses a multi-scope script before any file lands — it stays a
   stdout/module artifact, both of which apply
   multi-frame bodies. The Surrealism module's `apply` variant
   (`surform-surrealism-apply`) consumes these scripts in-database
   with two shapes the host import forces (probed 2026-10-09): the
   host `sql` call parses ONE
   expression, so the frames and `USE` lines never ride — the
   script's statements execute one host call each, in order — and
   the script's atomicity arrives at the dispatch boundary instead
   of the frame's (the module's host writes ride the invoking
   statement's transaction: a failed apply rolls back whole, a
   successful one commits whole). The one-call-per-statement shape
   rides the GENERATOR's own boundaries: the emitter hands the flow
   its statements marked at emission (`ConvergenceScript`), so
   nothing re-derives them from the rendered bytes — the lexical
   re-splitter this flow once rode merged printer output
   (a whitespace-led regex, a keyword-table
   false premise, a position recursion that missed the codegen's
   compound clause heads) and was retired 2026-10-07. A script
   carrying the `SURFORM WARNING`/`REQUIRED` markers refuses there —
   the fill-in flow needs a human the module cannot pause for — and
   so does ANY `REMOVE` statement among the marked statements (the
   markers cover only the marked
   removals — tables, fields, scopes — while every other family's
   catalog removal drafts bare; the additive-only gate refuses it by
   name), and so does any raw-authored statement (its boundaries are
   the author's, which the generator does not mark). The in-database
   apply is therefore additive-only literally: it defines and never
   removes. *(2026-10-07: the calling
   surface ships — the embeddable migration snippet, a header-less `.surdl`
   appended to the caller's schema, whose `fn::surform::*` wrappers
   bridge the capture (the scaffolding sections and the default
   ledger pruned wrapper-side, each table's rendered `INFO FOR
   TABLE` assembled under `@tables` — pure SurrealQL, no module
   capability needed) and the status ledger envelope in-database —
   with the working-today flow documented on the docs site; the
   snippet is a first-class corpus document
   (`docs/examples/surform-migrate*.surdl`), pinned by the guest
   crate's vector suite and verified at docgen build time.)*
   Every
   data-destroying statement — `REMOVE TABLE` (deletes the table's
   key range: the documents go with it), `REMOVE DATABASE` and
   `REMOVE NAMESPACE` (the same for their whole subtree) — carries a
   greppable `SURFORM WARNING` comment block above it; and the two
   data-orphaning removal classes carry a greppable
   `SURFORM REQUIRED: DATA MIGRATION` block immediately before the
   removal (or its manual-removal fallback) — a DatabaseOnly TABLE
   (its documents destroyed) under
   the WARNING alarm, and a DatabaseOnly FIELD (whose dropped values
   remain on the rows, outside every field definition) alone: an empty editable region
   between the `… <<<` and `… END <<<` markers, with one commented
   real-SurrealQL `e.g.` line, where the user writes the
   export/move query — the slots are text, so they cannot break the
   transaction (every name the prose interpolates folds every
   character the vendored lexer ends a `--` comment on — the control
   characters plus the Unicode line separators U+2028/U+2029 — to
   visible escape spellings, so the line stays one
   comment for any database-side name — a backtick ident may hold
   any bytes), they derive from the findings like every statement
   (the script stays a pure function of the findings' multiset),
   and they inherit their findings' truth (the ledgered #51
   placement-blindness class would carry a bogus slot). Every other
   family is catalog/rebuild machinery — no slot, and a Drift
   finding retypes without destroying values (the rows keep them;
   the new kind governs future writes), so it never slots — the
   report notes a field kind change informationally and lists every
   slot in a manifest section (scope, family, key, the stakes) that
   `--check` shows too. No rename is ever auto-detected — the human
   names the move, the slot names the stakes. The comparison
   covers exactly the scopes the `--info` tags present: a statement
   in a scope no tag presented makes no claim either way. Exit 1 when
   drift exists in either direction; expected-unmanaged findings (the
   auto-seeded default namespace) are informational and never trip
   the exit; `--check` prints the report only — the CI gate.
   *(The SDL↔SDL version pair, 2026-10-07:
   `migrate generate --from v1.surdl --to v2.surdl` lowers the FROM
   file into the read-side slot — `ReadModel::version_pair_model`,
   the entries holding the authored expanded statements under their
   match-key names, raw bodies filed by their `DEFINE` header exactly
   as the DB readers file raw-routed entries — so ONE comparator, one
   report, and one script renderer serve both input modes; the
   comparison notion is unchanged, `INFO`-read-back convergence. The
   scope universe is the union of both files' walked scopes plus
   root — the from side is a complete state description, unlike the
   DB pair's partial tags — so a scope only the TO version declares
   is genuinely apply-pending (the script defines its owners first),
   and a scope the TO version drops is from-only content the script's
   collapse logic retires under the owner `REMOVE`s: this mode makes
   the collapse emission CLI-reachable, the WARNING anchors carrying
   the data-loss role E3005 plays for typed tags. The two input modes
   are one required clap group (then two; three since 2026-10-16,
   #101 — the bare FILE baseline joins the pair and INFO; every
   invocation still names exactly one complete mode, a lone side or
   a mixed pair is a usage error): every invocation names exactly one
   complete mode — a bare run, a lone side, or a mix of the two
   modes' flags is a usage error before anything loads. The
   convergence property — build v1 into the embedded engine, run the
   v1→v2 script, the engine's INFO re-diffs clean against v2 — is
   executed in the test suite.)*
   *(The migration as a FILE, 2026-10-08:
   `migrate generate --save` writes the script as a numbered draft
   under `migrations/` — `NNNN_<name>.surql`, the number the highest
   existing draft's plus one and a collision an error (the write is
   `create_new`, never a silent bump), the name the SDL file's stem
   slug-folded or the `--name` flag's, the tool-owned five-line
   metadata header carrying the identity `commit` re-reads. The
   saved draft's unfilled REQUIRED slots are delimited by bare
   git-style resolution markers — `<<<<<<< SURFORM SLOT (<scope>:
   <family> <key>)` … `>>>>>>> SURFORM SLOT END` — an unresolved
   draft is deliberately un-runnable SurrealQL, the guidance and
   `e.g.` lines inside the region stay comments, and the WARNING
   anchors persist as documentation in both carriers; the stdout
   script keeps its comment banners and never renders markers, and
   every other byte is shared (the multiset-purity property extends
   to saved files, the style riding the renderer as a parameter).
   `migrate commit` refuses while any marker remains (each
   unresolved slot named), requires `-m`, and rewrites the file:
   the header marked `-- status: committed sha256:<hash> <message>`
   — the hash SHA-256 (in-repo, vector-tested; no dependency this
   unit) over the FILLED script body, the bytes `BEGIN
   TRANSACTION;`…`COMMIT;` minus the appended ledger region, so the
   ledger keeps identifying the user's script after the rewrite —
   and the migration ledger's statements spliced INSIDE the script's
   transaction before its final `COMMIT;`: `DEFINE TABLE IF NOT
   EXISTS _surform_migrations` + typed `DEFINE FIELD IF NOT EXISTS`
   columns (number, name, hash, message, applied_at datetime, sdl
   string; `--ledger-table` overrides the name in both statements)
   and ONE idempotent record insert — `INSERT IGNORE` keyed on the
   record id (the migration number; the idempotence executed against
   the embedded engine: two runs, one row — vendored
   `InsertStatement.ignore` prints ` IGNORE`, the executor folds the
   duplicate-id conflict into a skip under `is_ignore()`),
   `applied_at` reading `time::now()` so the ledger records the true
   APPLY time whatever tooling runs the script. The `sdl` column
   holds the canonical SDL of the flattened document the migration
   converges to — commit re-loads the header's `-- source:` path
   (fail-closed) — the revert substrate, self-contained where the
   authored file's includes are not. Commit is once per file: hash
   unchanged, "already committed"; hash changed, "edited after its
   commit" — the ledger keeps the recorded hash and `status`
   surfaces the drift; and a different draft carrying the same
   number in the same directory refuses (the two-branches-one-number
   collision — the ledger keys on the number).)*
   *(2026-10-08: `migrate push`, the apply
   half, through SurrealDB's raw HTTP `POST /sql` endpoint (the
   HTTP surface, never the SDK's remote
   engines). The wire contract is doc-cited against
   surrealdb.com/docs (the REST API's HTTP-protocol and errors pages)
   and engine-probed on 3.3.0: POST the body to `<root>/sql` with
   `Accept: application/json`, `Surreal-NS`, `Surreal-DB` and
   optionally `Authorization: Basic base64(user:pass)`; the response
   is a JSON array, one element per statement (`status` `"OK"`/
   `"ERR"`, the message string in `result` on ERR), HTTP 200 even
   when statements fail — the request-level failures (400 parse,
   401 credentials, 403 denied, 413 size, 504 timeout) carry the
   status instead. The apply loop: number order, one POST per file
   (the file's own transaction wraps it — a failed statement rolls
   the whole file back, the probe confirming the aborted
   transaction's elements report `NotExecuted`/`Cancelled` with the
   root cause carrying its own kind), ledger-recorded (number + hash)
   skips, the first failure stopping with the engine's message and
   the how-far count, a re-run resuming by the ledger. The ledger
   read is `SELECT number, name, hash FROM <ledger>;` over the same
   endpoint; a missing table is the probed `kind: "NotFound"`,
   `details.kind: "Table"` ERR — the fresh-database empty ledger,
   matched on kind AND the details.kind subject — while any other
   kind, or NotFound on any other subject (a missing namespace or
   database — a mistyped `--ns`/`--db` — reports the same top-level
   kind), is an error. Credentials ride flags or environment —
   `SURFORM_DB_USER`/`SURFORM_DB_PASS` (Basic), or a bearer token the
   caller minted (`--token`/`SURFORM_DB_TOKEN`; `Authorization:
   Bearer <token>` carried verbatim — the pipeline never mints or
   refreshes it; the two kinds are exclusive, issue #99) — and are
   never printed; header values are control-char-checked up
   front so no error path can echo them. The client is `ureq`
   (blocking — the CLI stays sync; rustls, no native-tls; the
   pipeline's one dependency addition), with in-repo base64 encode and
   a vector-tested read-only JSON reader — the response walk needs
   nothing more. Three engine-probed guards: the pre-flight drift
   check refuses the WHOLE run when any committed file no longer
   hashes to its header (nothing is sent), the USE guard refuses
   a script whose body carries `USE NS … DB …` naming other targets
   (a body `USE` switches the HTTP session — probed — so the
   migration and its ledger self-record would land in another
   database than the one the skip-SELECT reads), and the frame guard
   refuses a committed body carrying anything non-blank/non-comment
   after its `COMMIT;` frame close (a hand-edit's appended tail, or a
   second `BEGIN`…`COMMIT` block — the engine runs the frame and the
   tail independently, so a tail failure would leave the frame
   committed and self-recorded under a false "rolled back" report,
   the re-run skipping the file and the tail never applying;
   `commit` refuses the same shape, so the defect cannot enter the
   ledger's history). The HTTP boundary
   is mocked in tests — a hand-rolled `TcpListener` server recording
   requests and answering the documented shapes; no live server in
   any repo workflow.)*
   *(2026-10-08: `migrate status` + `migrate
   revert` — the pipeline closes its loop. STATUS reads the ledger
   ROWS live through the shared HTTP client — an `--info`-interchange
   first cut was set aside: INFO is schema, not rows, so the
   ledger's number/name/hash rows have no interchange carrier — one
   `SELECT number, name, hash FROM <ledger>;` (push's exact
   projection, the fresh-database branch included), then a local
   classification: applied (the ledger's hash equals the file's
   current `filled_script` hash — current-vs-ledger
   by definition; the header's recorded hash is not consulted), pending
   (committed, unpushed), draft (no committed header), DRIFTED
   (current ≠ ledger — both hashes named), plus ledger rows with no
   local file. Unlike push, status ALWAYS makes the ledger read (an
   empty directory still has a ledger side — the missing state), and
   the local walk is pre-flight (a missing directory, duplicate
   numbers, an unparseable file refuse before the dial). Exit rule:
   0 when directory and ledger agree, 1 on any DRIFTED or MISSING —
   pending/draft are the pipeline's normal states, drift and missing
   are the corruption classes push and commit already refuse to
   compound. REVERT is a convenience over generate, never a
   down-chain: the ledger's stored `sdl` column (the canonical
   flattened state the migration converged to) is the `--to` side and
   the LIVE database the `--from` side, fetched over the same
   endpoint as `INFO FOR DB;` + one `INFO FOR TABLE <name>;` per
   table — engine-probed 3.3.0 (and the vendored info operators
   agree): both results arrive as JSON objects whose leaves are the
   definitions' DDL strings, so the responses convert directly into
   the interchange `InfoObject`, the per-table maps riding the
   reserved `@tables` key, and the ingestion is the SAME
   `ReadModel::push_scope` the `--info` mode runs against the SAME
   comparator the version pair runs — the draft is byte-for-byte the
   script `generate --from <live> --to <recorded>` would emit, slots
   riding wherever the recorded state orphans data. Any INFO leaf
   that is neither a DDL string nor an object refuses (fail-closed —
   no shape invented). The row lookup is client-side over the wider
   `SELECT number, name, hash, sdl` projection (the row shape
   engine-probed): 1–4 digits spell a number (the ledger is
   number-unique per database), anything else a 4–64-hex hash prefix,
   ambiguity naming the candidates, and the spelling check is
   pre-flight (an unreadable target never dials). The draft is
   written via the `--save` machinery with a `.surdl` SIDECAR
   carrying the recorded state verbatim — the draft's `-- source:`,
   so a later `commit` re-loads it into the new row's `sdl` column
   and reverting the revert works; the revert command GENERATES and
   stops, never applies. Two boundaries: the LEDGER TABLE never
   joins the compare (it is the pipeline's machinery — an
   unfiltered INFO would classify it database-only and draft its
   removal, wiping the history the revert appends to), and the
   compare runs at DATABASE scope with the stored schema required to
   declare `--ns`/`--db`'s database (the namespace-free pseudo-scope
   spelling accepted) — a scope the compare never ingested makes no
   claims, and a silent empty draft would be a false "converged".
   The executed round trip pins the whole loop: apply v1, apply v2,
   revert-draft toward v1, fill, commit, push — the database back at
   v1's schema, all three ledger records present, and record 3's
   substrate byte-identical to record 1's.)*
   *(The bare-FILE baseline, 2026-10-16, issue #101, makes the
   usage grammar's third spelling real: `migrate generate
   schema.surdl` — no tags, no pair — is the SDL-only TO side with
   an EMPTY FROM side, the read model `ReadModel::baseline_model`
   (every scope the file mentions ingested, zero entries), so every
   finding is apply-pending and nothing can classify as a removal —
   the model holds no entries for the removal arm to fire on. The
   report and script are byte-identical to the version pair with an
   empty file on the from side; the draft header records the mode as
   `-- input: file path=<file>` (`InputMode::File`, its own strict
   parse — exactly one `path=` token, no bare legacy posture). The
   required group's refusals flip deliberately: the bare file is a
   complete mode, so its old missing-`--info` refusal is gone; the
   conflicts are load-bearing on BOTH pair members — clap waives a
   `requires` when a conflicting argument is present, so a bare file
   beside `--to` would waive the missing `--from` and silently ride
   the baseline without `--to`'s `conflicts_with_all`).)*
4. **Normalization protocol (retro-apply).** After applying, the tool
   runs `INFO` and parses the database's *normalized* DDL back into the
   SDL AST — `INFO` output is SurrealQL, readable by the embedded
   parser, which is configured with **all experimental capabilities
   enabled** (`files`, `surrealism`) so legal server state always
   round-trips (sole syntactic exception: `module`'s grammar — the
   embed's query path parses with default capabilities, so
   `DEFINE MODULE` is engine-rejected there ("Surrealism modules are
   not enabled"), no live INFO can carry a module entry, and SDL owns
   module DDL parsing; the parse-level gate fires with the surrealism
   capability — probed at the floor move; and `config`, whose INFO
   entries are header-less
   fragments — the reverse
   parser prepends `DEFINE CONFIG OVERWRITE` before parsing; the
   `default` config is only available from `INFO FOR ROOT` as a
   structured value). SDL itself does not model server capabilities —
   capability-gated constructs (`mod::` paths, `bucket`, `module`) are
   accepted by SDL; the server enforces its capabilities at define
   time. The 1:1 clause mapping makes the reverse conversion
   mechanical; the tool then rewrites the SDL file in **canonical
   form**. The database is the sole authority on normalization
   (materialized values may change between server versions); SDL never
   replicates that logic. Retro-application is a **merge, not an
   overwrite**:

   - **adopt** everything the database materializes — fulltext
     `ANALYZER like BM25(1.2,0.75)`, event `WHEN true`, graphql `TABLES
     AUTO FUNCTIONS AUTO`, HNSW's derived `M0`/`LM`, field/function
     `PERMISSIONS FULL`, user `ROLES VIEWER` / `DURATION FOR TOKEN 1h` /
     `FOR SESSION none`, event `ASYNC RETRY 1 MAXDEPTH 3`, reference
     `ON DELETE IGNORE`, and api `FOR any` + `PERMISSIONS` on every
     clause (the absent-`any` rule: no THEN, no middleware, and the
     elided default FULL folds to absence) — **except record-access
     issuance treated as sentinel-shaped**: the
     auto-issued `WITH JWT … HS512 KEY '[REDACTED]'` block reads
     through the ordinary adoption (the shape-only key rule made the
     printed signature the canonical literal; see
     [access.md](access.md));
   - then **elide any clause whose value equals the statement's
     SurrealQL default** — canonical SDL is minimal (`PERMISSIONS FULL`
     on an unpermissioned field disappears; `BATCH 1000 START 0`
     disappears; a `retry: 1; maxdepth: 3;` block collapses to bare
     `async;`; `FOR SESSION none` disappears). Elision never demotes
     lint visibility: advisory lints (world-open table permissions
     SDL-L0001, plaintext passwords SDL-L0002, secret-less users
     SDL-L0003, relations without `enforced;` SDL-L0006) evaluate the
     *resolved* state, not the source text;
   - **preserve** what `INFO` drops but SDL means — database `strict;`
     (active yet absent from `INFO FOR NS`), index `concurrently;`
     (transient build directive), access bearer `jwt { … }` clauses
     (stored, never rendered) and issuer `algorithm:`, table
     `delete: full;` (`FOR delete FULL` is elided by
     the emitter while re-parsing yields NONE — a definition with
     `delete: full` must never be downgraded by retro-apply), and user
     secrets (`INFO` shows `PASSHASH '[REDACTED]'` — keep the SDL
     value; the differ treats secrets as opaque; `PASSHASH ''` on a
     secret-less user is likewise never adopted as a literal; the 3.3
     `PASSSCRAM '[REDACTED]'` presence signal strips at read — the
     sentinel is define-rejected, so only the SDL side ever carries
     `passscram:`). ACCESS
     key secrets sit OUT of the preserve list: the
     shape-only rule made `'[REDACTED]'` the only authorable spelling
     for symmetric verify keys and every issuer key, so the redacted
     INFO text IS the canonical SDL — the read adopts it and marks
     `Markers::redacted` (only asymmetric PUBLIC verify keys render
     verbatim and ride as ordinary literals); and `mixin` statements
     with their spread entries (SDL-only structure —
     [mixin.md](mixin.md)'s reverse-path rule: the spread survives,
     a state-equal member is covered by it, a diverged member
     materializes as the explicit override);
   - **fold server-synthesized subfields:** for every field whose type
     has an inner kind (`array<T>`, `set<T>` — sized variants included —
     and unions containing them; a bare `array`/`set`'s inner kind is
     `any`, so the recursion below writes nothing), SurrealDB
     materializes a stored `name.*` subfield with the inner kind (and
     inherited `flexible;`), and
     recurses until the next inner kind would be exactly `any` (such a
     level is not written): `matrix: array<array<int>>` synthesizes
     both `matrix.*: array<int>` and `matrix.*.*: int`. The inner kind
     of a union is the union of its arms' inner kinds, dropping arms
     that have none (`int | array<string>` → `.*: string`). INFO lists
     all synthesized levels. The reverse parser folds them back
     deepest-first: an entry folds when its kind equals its parent's —
     possibly also-folded — inner kind **under union normalization
     applied to both sides** (flatten/dedupe/singleton-collapse, the
     same algebra mirrored in rule 4's `option<T>` ≡ `none | T`; this
     local normalization is a licensed exception to "the database is
     the sole authority" — it is a pure function of the kind AST), it
     carries only default clauses, and its `flexible` equals the
     parent's (inherited — it never blocks folding). Folded entries
     are omitted from canonical SDL and are never REMOVE candidates; a
     `.*` entry with its own clauses or a divergent kind is
     user-meaningful and kept. Relation tables likewise materialize
     `in`/`out` field definitions from their endpoints — the reverse
     parser folds them into the table's relation shape (see
     [table.md](table.md));
   - **form selection:** the assignment/short form is used when only its
     value is present and everything else is elided; otherwise the
     block form. Fields, indexes, and events normalize to their
     **nested** placement inside the table block. `COMMENT` strings map
     back to `///` lines (embedded `\n` splits into lines); a COMMENT
     that is not a plain string literal routes the statement to `raw`.
   - **canonical output is a total function of the parsed INFO state
     and the pre-merge SDL source (which is assumed rule-7
     self-contained — no dangling references):** rewritten blocks emit clauses in
     their grammar-block order (as printed in each statement file);
     **adopted statements are inserted at the earliest position after
     the last existing statement they depend on and before the first
     existing statement that depends on them; among adoptees, the
     dependency bound takes precedence over INFO-map order exactly as
     it does over nesting** (a dependency-respecting topological
     order, with INFO-map order — spanning the INFO queries in scope
     order: ROOT, then NS, then DB, then TABLE-level in table order —
     as the tie-break). The dependency relation is: an index on
     its fields' table and (for fulltext) its analyzer; a field on
     nothing; an event on its table; a view on the tables its query
     reads; an analyzer on its `function:` fn; a table carrying a
     spread on the mixin it spreads (rule 7 — only authored tables
     carry one, the reader never produces a spread; a mixin's
     fulltext index members pin their analyzers before it the same
     way); every statement on its
     enclosing namespace/database section. **Nested placement is the
     default, but yields to the dependency bound:** when an adopted
     field/index/event's dependency sits after its table statement
     (e.g. an adopted fulltext index whose analyzer follows the
     table), the adoptee stays at top level in its section at the
     pinned position and the linter flags the non-nested form
     (SDL-L0007).
     Multi-scope files adopt per ns/db section, never across a
     `database` boundary. Equivalent forms prefer
     the shortest spelling (the shorthand over the block —
     `permissions: full;` over the all-ops block; bare `async;` over
     the configured block; `on: kv;` stays as authored — aliases are
     NOT rewritten on retro-apply, only on emitted DDL); HNSW `lm`
     elision compares with a pinned relative epsilon of 1e-9.
     `OVERWRITE`/`IF NOT EXISTS` kind prefixes are stripped on
     re-parse; alias spellings normalize on DDL emission (`on: kv;` →
     `ON ROOT`); on re-parse both `DIST` and `DISTANCE` are accepted
     and canonical SDL says `distance:`.

   INFO sources per scope: `INFO FOR ROOT` (namespaces, root users, root
   accesses, default config), `INFO FOR NS` (databases, ns users, ns
   accesses), `INFO FOR DB` (tables — with `INFO FOR TABLE` for
   fields/indexes/events — functions, analyzers, params, sequences,
   accesses, apis, buckets, configs, modules, db users). INFO prints
   some keywords lowercase (`FOR any`, `FOR get`) — case-insensitive
   input; canonical SDL is lowercase. API note: INFO
   always emits a `FOR ANY … PERMISSIONS …` clause; it maps back to an
   absent `any` exactly when it carries no `THEN`, no middleware, and
   `PERMISSIONS FULL`.

## The linter

`surform lint <file> [--info <tag>=<path>…]` reports advisory findings —
the `SDL-Lxxxx` block, miette-rendered like diagnostics but never
diagnostics themselves (`check`/`build` output is unchanged) — and
exits 1 when any exist (the clippy convention). Findings evaluate the
**resolved state**: the merge's placed output when `--info` scopes are
given (elision never demotes lint visibility), the file itself
otherwise. Two families sit outside that rule by necessity, because
the read side can never carry their subject: the access duration
sides an access's own capability silently drops (SDL-L0011) and rule
8's raw bodies (SDL-L0012) evaluate the authored file. Findings
anchor on authored spans only; adoptee-only findings (a statement the
authored file does not carry) render sourceless, and a sentinel about
to be applied (SDL-L0013) is likewise reported wherever the resolved
state holds it.

**Allow comments** suppress a finding at its site: a
`// surform-lint-allow: SDL-L0001[, SDL-L0006…]` comment excuses
exactly the codes it names on exactly ONE statement — every plain
comment in the statement's leading trivia (blank lines between the
comments and the statement do not break attachment — fmt collapses
them) plus the comment trailing the statement's `;` on the same line.
A comment inside a statement's body is never an attachment point (the
scope is the statement, not the clause). The directive is a lint-time
scan of the authored source (comments
are layout, never definition — rule 5 keeps the marker riding its
anchor through fmt, normalize, and the retro-apply carry). A finding
is suppressed when its authored span falls inside the allowed
statement and its code is named; statements are whole (an allow on a
table covers findings anchored inside its body), and sourceless
findings (adoptee-only) have no statement to excuse them and never
suppress. The directive's own failures are findings too (SDL-L0023):
an unknown code, a malformed marker (any comment starting
`surform-lint` that is not the exact directive), or a comment with no
following statement — none suppressible, so a broken allow cannot
silence its own diagnosis. Codes are exact spellings from the
registry below; SDL-E error codes never appear (errors are not
findings).

| code | finding | basis |
| --- | --- | --- |
| SDL-L0001 | table permissions `full` (anyone, even unauthenticated users, can access) | resolved |
| SDL-L0002 | plaintext `password:` | resolved |
| SDL-L0003 | secret-less user | resolved |
| SDL-L0004 | `edgengram`/`ngram` min > max | resolved |
| SDL-L0005 | duplicate in roles/tokenizers/filters | resolved |
| SDL-L0006 | relation without `enforced;` | resolved |
| SDL-L0007 | top-level field/index/event nestable in-scope | resolved |
| SDL-L0008 | param without `value:` | resolved |
| SDL-L0009 | api with no effective `any` catch-all (or no groups at all) | resolved |
| SDL-L0010 | record-typed `in`/`out` field rewriting a relation endpoint | resolved |
| SDL-L0011 | access duration side the kind's capability drops | authored |
| SDL-L0012 | raw body the pinned parser rejects | authored |
| SDL-L0013 | applying defines `[REDACTED]` as credential material | both |
| SDL-L0014 | table `delete: full;` (cannot round-trip or drift) | resolved |
| SDL-L0015 | computed field vs a field under it | resolved |
| SDL-L0016 | `reference;` on a non-record-shaped kind | resolved |
| SDL-L0017 | `id` field kind not a record-id key kind | resolved |
| SDL-L0018 | `flexible;` on a non-SCHEMAFULL host | resolved |
| SDL-L0019 | `inline;` on a non-`type relation` host | resolved |
| SDL-L0020 | table carrying more than 255 `inline;` fields | resolved |
| SDL-L0021 | mixin defined but never spread (emits nothing anywhere) | authored |
| SDL-L0022 | a later spread's member shadows an earlier spread's (last-wins resolution) | authored |
| SDL-L0023 | a `surform-lint-allow:` directive's own problem (unknown code, malformed marker, nothing to attach to) | authored |
| SDL-L0024 | a referenced table/analyzer/`fn::` path (or a `fn::`-body param) nothing defines | authored |
| SDL-L0025 | RETIRED (2026-10-06) — the compiler owns emission order (rule 7); a late definition reorders, a cycle is SDL-E0015 | — |
| SDL-L0026 | a diamond include — a file the composition already spliced, declared again (first inclusion wins) | authored |
| SDL-L0027 | type alias defined but never referenced (expands nothing, emits nothing) | authored |

**The reference lints** consume the reference extractor's
report over the authored stream. Their contract is the extractor's,
applied to definitions: advisory, never a diagnostic — the catalog is
flat, so an undefined target is legal SDL.

- **SDL-L0024** flags a statically-resolved reference whose target no
  linted file defines — the authored definitions union the resolved
  state's, so an interchange-adopted definition satisfies a reference,
  and every definition is scope-keyed (a same-named table in another
  database does not satisfy a reference — catalog lookups are per
  database). Typo-class catches: a permission subquery's table, a
  fulltext index's analyzer, a `fn::` path. A `mod::` call is covered
  by its module (the engine loads `mod::name` by the call's first
  segment), `silo::`/`ml::` names by their version tails — deployed
  artifacts, never `DEFINE`-able — and a name in the builtin path
  table resolves as a builtin (api middleware heads are licensed
  bare). Param reads are sound ONLY inside `fn::` bodies: the engine
  runs them on `Context::new_isolated`, whose value lookup falls
  through to the parent solely for the session four; the record trio
  `$this`/`$self`/`$parent` binds through the CursorDoc on a
  document-bound call, and a `param` definition RESOLVES inside a
  body (the param read's catalog fall-through, `get_db_param`) — the
  definition set covers it. What never binds inside a body is a
  client-bound value; everywhere else a permission or default can
  read one, so those param reads stay muted.
- **SDL-L0025** is retired (2026-10-06, the declarative namespace's
  semantics unit): the apply-order advisory existed because the apply
  order was the authored order — with `build` emitting the compiler's
  own stable topological order over the define-time edges (rule 7), a
  definition standing "later" in the stream reorders silently, the
  one placement no order can fix is the SDL-E0015 diagnostic, and the
  one placement the sort will not attempt (across a scope section) is
  SDL-E0016. The number keeps its gap, like every retired code.
- **SDL-L0026** (the declarative namespace) anchors on a diamond
  include: a file the discovery walk already completed that another
  file then declares splices nothing — first inclusion wins. It
  advises rather than errors because self-sufficient module trees
  produce it systematically; the opt-out is the allow comment above
  the no-op include (the one finding that anchors on an include's
  span, whose scan entry survives outside every splice block). The
  same file included twice by ONE file is the SDL-E0014 error
  instead — the dedupe would silently hide the duplicate.
- **SDL-L0027** (the alias namespace) anchors on a declaration no
  kind references: expansion never rewrites anything and the
  statement emits nothing — the unspread-mixin class in the alias
  namespace. The reference set spans the whole composition (fields,
  fn args/returns, another alias's right-hand side), so a chain's
  head stays used while any link names it — pruning cascades one
  finding at a time — and a use inside a `raw` body is SurrealQL text
  (rule 8) and never counts. It advises: the alias namespace collides
  with nothing (the mixin rule), so the dead name is harmless.

## Statement files

| file | statement | status |
| --- | --- | --- |
| [access.md](access.md) | DEFINE ACCESS | ✅ consolidated (2026-09-18) |
| [analyzer.md](analyzer.md) | DEFINE ANALYZER | ✅ consolidated (2026-09-18) |
| [api.md](api.md) | DEFINE API | ✅ consolidated (2026-09-18) |
| [bucket.md](bucket.md) | DEFINE BUCKET | ✅ consolidated (2026-09-18) |
| [config.md](config.md) | DEFINE CONFIG | ✅ consolidated (2026-09-18) |
| [database.md](database.md) | DEFINE DATABASE | ✅ consolidated (2026-09-18) |
| [event.md](event.md) | DEFINE EVENT | ✅ consolidated (2026-09-18) |
| [field.md](field.md) | DEFINE FIELD | ✅ consolidated (2026-09-18) |
| [function.md](function.md) | DEFINE FUNCTION | ✅ consolidated (2026-09-18) |
| [include.md](include.md) | INCLUDE — SDL-only file composition | ✅ consolidated (2026-10-04) |
| [index.md](index.md) | DEFINE INDEX | ✅ consolidated (2026-09-18) |
| [mixin.md](mixin.md) | MIXIN — SDL-only, no DEFINE counterpart | ✅ consolidated (2026-09-29) |
| [module.md](module.md) | DEFINE MODULE | ✅ consolidated (the 3.3 floor's spelling; 2026-09-18) |
| [namespace.md](namespace.md) | DEFINE NAMESPACE | ✅ consolidated (2026-09-18) |
| [param.md](param.md) | DEFINE PARAM | ✅ consolidated (2026-09-18) |
| [sequence.md](sequence.md) | DEFINE SEQUENCE | ✅ consolidated (2026-09-18) |
| [table.md](table.md) | DEFINE TABLE | ✅ consolidated (2026-09-18) |
| [type-alias.md](type-alias.md) | TYPE — SDL-only type aliases | ✅ consolidated (2026-10-25) |
| [user.md](user.md) | DEFINE USER | ✅ consolidated (2026-09-18) |
