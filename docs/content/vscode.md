The repository ships a VS Code extension for the language: syntax
highlighting for `.surdl` files, **Format Document** through the
`surform` binary, and the compiler's diagnostics in an output channel.
Its home is [`editors/vscode/`](https://github.com/jquesada2016/surform/blob/main/editors/vscode/README.md)
in the repository — the README there carries the packaging and
self-check details; this page is the overview.

## Install

There is no Marketplace listing yet — the manifest's `publisher` is a
placeholder, and publishing is a project-owner decision like the other
release surfaces. Two ways to run it.

Package a `.vsix` and install it:

```sh
cd editors/vscode
npx @vscode/vsce package
code --install-extension surform-<version>.vsix
```

Or open `editors/vscode/` in VS Code and press F5 — the Extension
Development Host.

## Syntax highlighting

The grammar (`syntaxes/surform.tmLanguage.json`, scope
`source.surform`) covers `.surdl` — the canonical extension — and
`.sdl`. It mirrors the language the compiler enforces: lowercase
statement heads, clause keys ending at `:` / `{` / `;`, the three
SurrealQL-verbatim bodies (`fn` bodies, view queries, `raw { … }`),
and expression clauses captured to their depth-0 `;`.

The SurrealQL-verbatim regions delegate to **SurrealDB's official
grammar by scope** (`include: source.surrealql`), so embedded queries
and function bodies get the same coloring SurrealDB's own tooling
ships. The official extension (`surrealdb.surrealql`) is declared as
an extension-pack member: installed alongside when available, never
required. Without it, the delegated regions keep this grammar's own
fallback string, comment, and bracket coloring. Highlighting is
cosmetic either way — `surform check` remains the authority on what is
valid.

## Formatting

**Format Document** (⇧⌥F on macOS, Shift+Alt+F elsewhere) pipes the
buffer through the compiler: `surform fmt` with the text on stdin, the
canonical SDL back on stdout — exactly what the
[CLI](fmt.html) would write, modulo the buffer's own line-ending
convention (the buffer wins over the writer's LF).

- The binary is looked up on `PATH`.
- A buffer that does not parse is left untouched — the diagnostics go
  to the output channel (below) and a toast names the first one.
- Unchanged output edits nothing.

## Diagnostics

The compiler's `--json` diagnostics from a refused format land in the
**Surform** output channel (View → Output) as `SDL-Exxxx: message` lines.
A missing binary toasts once, then
formatting stays a no-op until `surform` is findable again.

## Settings

| setting | default | what it does |
|---|---|---|
| `surform.path` | `surform` on `PATH` | the binary Format Document spawns; relative paths resolve against the workspace folder |

## Where next

- [Formatting](fmt.html) — the canonical writer the extension pipes
  through.
- [The surform CLI](cli.html) — the commands and flags behind all
  three surfaces.
- [`editors/vscode/README.md`](https://github.com/jquesada2016/surform/blob/main/editors/vscode/README.md)
  — the extension's own README: the grammar's self-checks
  (`selfcheck.mjs`), the formatter's (`fmtselfcheck.mjs`), licensing.
