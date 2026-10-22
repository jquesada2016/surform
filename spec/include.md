# include

Global conventions and notation: [spec/README.md](README.md).

Ground truth: **this spec** — `include` is an SDL-only statement with
no SurrealQL counterpart (like `mixin`, spec/mixin.md). No upstream
parser is involved: the embedded SurrealQL parser never sees one (the
composition lowers before emission), and INFO never reports one (the
database's catalog is flat — composition is a source-tree property).
The statement, its position rules, and the composition semantics are
defined here and pinned by the flatten oracle (`tests/include.rs`).

## What it is

`include "path";` composes another schema file into this one: the
included file's statements take the include statement's place in the
stream (depth-first, pre-order — an included file's own `include`
statements splice at their positions). The composition is defined to
be **semantically identical to the flattened single file**: a project
and its flattening compile to byte-identical DDL, produce the same
lint findings, and resolve mixin spreads against the same global
mixin set (the declarative namespace — spec/mixin.md). That invariant
(`flatten(root) ≡ root`) is the oracle every composition behavior is
tested against.

Files read top to bottom, but the stream order is PRESENTATION, not
semantics (README rule 7, the declarative namespace): `build` emits
the compiler's own stable topological order over the define-time
edges, with authored order as the tie-break — composition only
extends the stream, so the flattening's order still decides every
tie. There is no namespace effect of any kind: names land in
SurrealDB's flat catalog exactly as a single file's would.

```rust
struct Include {
  path: String,                 // the authored literal, carried verbatim
  span: Span,                   // head word + literal — diagnostics
  comments, clause_comments,    // rule 5's anchors (no doc comment)
}
```

```
include 'tables/users.surdl';
```

The path is one string literal (both quote styles; `\` escapes decode
through the shared table). Its VALUE is carried exactly as authored —
the canonical writer re-quotes it through the shared string quoter,
like every string in the file. Resolution happens only at load time
(below); an include statement in text no loader reads is inert.

## Position rules (the composition preamble)

One order rule remains, and it is scope's, not composition's:

1. `namespace`/`database` statements may appear only **before** the
   file's first `include` — the composition preamble. They scope
   everything below them, the spliced content included. A scope
   statement after an include is SDL-E1069 (the misplaced statement's
   head is named). Includes THEMSELVES are legal anywhere in the
   file: an include declares participation, not an elaboration point,
   and the splice lands at its position.
2. An **included** file may not carry `namespace`/`database` at all —
   those statements re-scope everything below them, so one inside a
   splice would silently re-scope the rest of the importing file
   (SDL-E1070, diagnosed by the loader against the included file's
   own text).

Comments may sit anywhere (rule 5's anchoring is unchanged — they
attach to the include statement like any statement's).

## Resolution, dedup, cycles (the loader)

The loader walks the tree from the root file the CLI was given:

- Paths resolve **relative to the including file's directory** — never
  the process CWD, never `SURREALDL_*` search paths. Absolute paths
  are rejected (a schema tree must be portable; SDL-E0012, anchored
  on the include statement); `..` segments are fine.
- A file splices **once**: includes dedupe by canonicalized path,
  first include wins. What a second declaration MEANS depends on who
  declares it:
  - The same file included twice **by one file** is a hard error
    (SDL-E0014, anchored on the second declaration) — splice-once
    would silently absorb it, and the copy-paste smell is the same
    class SDL-E1067 rejects for member keys. The rule tracks per
    declarer: naming one path twice is the error even when another
    participant declared the file first. An IN-PROGRESS target (the
    bullet below) is the carve-out — a back-edge never splices, so
    nothing is silently absorbed, and the reach stays legal however
    often it repeats.
  - A file the discovery walk already COMPLETED, declared again by
    another file (a **diamond** — two files including one shared
    file), is the SDL-L0026 advisory, anchored on the no-op
    declaration ("already included by `X.surdl`; first inclusion
    wins"). It advises rather than errors because self-sufficient
    module trees produce it systematically; the opt-out is the allow
    comment above the no-op include (`// surform-lint-allow:
    SDL-L0026`) — the one finding anchored on an include's span,
    whose scan entry survives outside every splice block.
  - A reach of an **in-progress** file (the discovery stack — an
    ancestor, or the file itself) is a cycle **back-edge** and legal:
    the include graph declares participation, not an elaboration
    order, and splice-once makes every cycle terminate. Discovery
    order (depth-first, first-visit) is the tiebreak for where a
    shared file splices. The same definition — one family, one name —
    in two files is the SDL-E1062 error like any other duplicate
    (README rule 7): stream position decides nothing.
- A missing or unreadable file is SDL-E0008, anchored on its include
  statement.
- Each file parses separately — every file's diagnostics carry spans
  into that file's own text (the CLI renders each against its own
  source). The flattened document is the concatenation of the parsed
  statement lists, with a file map attributing statements to homes;
  the loader's last pass resolves every spread against the flattened
  document's global mixin set (spec/mixin.md), so a typo'd spread
  fails the load in every check-class context.

An included file's trailing `//` lines (rule 5's file-tail comments)
move onto its last spliced statement; an include statement's own
anchored comments move onto the splice neighbors (leading comments to
the first spliced statement's, a same-line note to the last spliced
statement's — the include itself never survives into the stream).

## Strict contexts (no loader, no statement)

`include` parses only in the composable entry the loader and the
formatter use. Every context that parses inline text — the playground,
`check`-class paths on buffers, docgen's runnable fences — takes the
strict entry, which rejects the statement:

```sdl-error expect=SDL-E1068
include 'tables/users.surdl';
```

The diagnostic is the contract, not a limitation to apologize for:
those contexts have no file system, and a composition they cannot
resolve must not silently compile as if the include were absent.
Formatting is the one strict-adjacent exception with a real use —
`surform fmt` (file or stdin) parses the composable entry and
round-trips the statement without resolving it; fmt never flattens
and never resolves (expansion precedent: the canonical writer never
lowers). Since parse carries spreads as opaque names, ANY single file
formats standalone — a participant whose spreads resolve only
through the composition included.

`fmt --in-place` on a file is the committal mode and routes through
the loader: the tree validates exactly as `check` would — the load
now includes the composition's global spread resolution, so a typo'd
spread is never rewritten around (a dirty composition is never
rewritten) — and every participating file formats from its own loader
parse (`Loaded::home_files`) and writes back when its canonical form
differs. Print mode stays per-file and tolerant; the write is
load-gated.

## Mixin interaction (the global mixin set)

A spread resolves against the composed document's WHOLE mixin set —
any file, any position (spec/mixin.md's resolution rule). Three
consequences, all free:

- Composition order is presentation, not semantics: a mixin may live
  in a file composed after its spread, and shared mixins need no
  preamble discipline.
- A mixin in the ROOT's body is visible to every composed file —
  shared mixins may live anywhere in the tree.
- Duplicate mixin names across files collide exactly as in one file
  (SDL-E1062 — the order pass catches the name two files define),
  and so does every other family: the duplicates rule is one class,
  any file, any position (README rule 7).

## Reverse parser (INFO → SDL)

None. INFO output is the flat catalog — it cannot report which file a
definition came from, so there is nothing to read back. `migrate
generate` composes before comparing, so a composed root compares
against the database as the flat whole it deploys as.

## Write-back distribution (normalize on a composed root)

`normalize --in-place` on a composition root returns every merged
statement to the file that authored it; without `--in-place` the
merged FLATTENED document prints — the preview of what the project
deploys as. The invariant is order fidelity: the rewritten tree's
flattening reproduces the merged document's statement order. Since
emission order is the compiler's (README rule 7), the flattening is
an AUTHORED order like any other — the re-pinned form: whatever the
rewritten tree's flattening, its topological sort equals the merged
document's emission, because the sort is deterministic over
(statements, authored ties) and the flattening preserves both. Order
fidelity keeps `flatten(root) ≡ root` byte-exact on the DDL side.

- A skeleton statement (one the merge kept from the authored input)
  returns to its authored position in its home file, even when a
  sibling folded away — positions are exact, never shifted.
- An adopted statement (one the merge brought in) slots where its
  stream neighbors put it: after the tree node that produced the last
  statement before it, or before the first one after at the document
  head.
- Every file's layout is `[ns/db preamble][includes][body]` — the
  write-back's own canonical layout (authored files may interleave
  includes with the body; the rebuilt tree does not), so a
  preamble-bound adoptee has no literal position.
  Adoptees with no dependent outside their home relocate to the
  home's first body position — a same-home dependent always sits at
  or after the relocation target, so the move cannot break define
  order (an adopted dependent counts by the home its own placement
  gives it). An adoptee that a dependent in ANOTHER file still points
  at refuses the whole write-back (SDL-E0013): that dependent lives
  in the include subtree the adoptee would jump past — as does any
  adopted `namespace`/`database`, whose only legal position is the
  preamble and whose insertion there would re-bind every scope below
  it.
- A raw-routed statement (the reader's verbatim route for shapes
  SDL's fixed emission cannot carry) matches its read entry by its
  DEFINE header's family and name, and a match PRESERVES the authored
  body — a raw body is not clause-addressable, so the match only
  consumes. Without the rematch the entry would re-adopt on every
  normalize, growing the file a duplicate per run.
- An include's comments transferred to the splice neighbors at load
  (above), so the rebuilt file carries the bare statement — a second
  copy in the root would re-transfer on every re-normalize. A comment
  above an `include` therefore lives in the composed file after the
  first write-back.
- Unchanged files are not rewritten (byte-compare); a plan is pure,
  so a refusal means nothing was written.

## Examples (one per rule)

Position rule 1 — the composition preamble, then the body:

```
namespace acme;
database shop;

include 'mixins.surdl';        // shared mixin definitions
include 'tables/users.surdl';

table account {
    ...timestamps;             // resolves — the composition's global
                               //   mixin set holds it
};
```

Position rule 2 — ns/db is the root's alone:

```
namespace acme;                // SDL-E1070 when this file is included
database shop;
```

A shared mixin file (its position in the tree is free — resolution
is global):

```
mixin timestamps {
    field created_at: datetime { default: time::now(); };
    field updated_at: datetime { default: time::now(); };
};
```
