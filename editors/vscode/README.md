# Surform (SDL) — VS Code extension

Syntax highlighting and canonical formatting for Surform's Schema
Definition Language: the `.surdl` files (the canonical extension;
`.sdl` files are highlighted too) that the `surform` CLI compiles to
SurrealQL DDL. The grammar
(`syntaxes/surform.tmLanguage.json`, scope `source.surform`) mirrors
the language the compiler enforces — lowercase statement heads, clause
keys ending at `:` / `{` / `;`, the three SurrealQL-verbatim bodies
(`fn` bodies, view queries, `raw { … }`), and expression clauses
captured to their depth-0 `;`.

## The SurrealQL dependency

SurrealQL-verbatim regions delegate to **SurrealDB's official grammar
by scope** (`include: source.surrealql`), so embedded queries,
function bodies, and expressions get the same coloring SurrealDB's own
tooling ships. The official extension (`surrealdb.surrealql`) is
declared as an `extensionPack` member: installed alongside when
available, never *required*.

Nothing is copied from the official grammar — the reference is by
scope at runtime, which is also why this extension carries no
SurrealQL license obligations. Without the official extension
installed, the delegated regions keep string, comment, and bracket
coloring from this grammar's own fallback rules; highlighting is
cosmetic either way, and the compiler (`surform check`) remains the
authority on what is valid.

## Formatting

**Format Document** (⇧⌥F on macOS, Shift+Alt+F elsewhere) pipes the buffer through the
compiler: `surform fmt` with the text on stdin, the canonical SDL
back on stdout — exactly what the CLI would write. The spawn and the
outcome mapping live in `formatter.js` (pure Node, no `vscode`
import; `fmtselfcheck.mjs` drives it against the real binary); this
manifest's `extension.js` is only the editor-API shell.

The binary is looked up on `PATH`; the `surform.path` setting points
somewhere else (relative paths resolve against the workspace folder).
A buffer that does not parse is left untouched — the compiler's
`--json` diagnostics land in the "Surform" output channel and a toast
names the first one. A missing binary toasts once, then formatting
stays a silent no-op. Unchanged output edits nothing; the buffer's
line-ending convention wins over the writer's LF.

## Try it

From this folder: `code --install-extension` on a `.vsix` packaged
with `npx @vscode/vsce package`, or open the folder in VS Code and
press F5 to run the Extension Development Host. `node selfcheck.mjs`
runs the grammar's self-checks (JSON validity, every pattern compiles,
spot matches, the formatter's manifest wiring) and
`node fmtselfcheck.mjs` runs the formatter's (stdin ≡ file, fixed
point, dirty-buffer diagnostics, not-found) — plain Node, no
dependencies; the formatter check drives the repo's `target/debug/
surform` (or `SURFORM_BIN`).

## Notes

- `publisher` is a placeholder (`surform`); publishing to the
  Marketplace is a project-owner decision, like the other release
  surfaces.
- The extension inherits the repository's dual MIT OR Apache-2.0
  license (see LICENSE-MIT / LICENSE-APACHE at the repo root).
- Self-check patterns are kept to the JS-RegExp-compatible subset of
  Oniguruma so `selfcheck.mjs` can compile every pattern with the
  runtime it has.
- The grammar mirrors the language the compiler enforces, so it can
  go stale the same way the committed wasm can. Which check rides
  which change: spec grammar or clause-menu changes run
  `node selfcheck.mjs`; `fmt` or diagnostics-behavior changes run
  `node fmtselfcheck.mjs` (it pins the real binary through the
  extension's exact spawn path). Neither rides the cargo gate — run
  them whenever the matching surface changes and fix the drift.
  The formatter spawns the
  user's `surform` binary at runtime — no artifact to rebuild
  there; package a new `.vsix` only when this folder's own files
  change.
