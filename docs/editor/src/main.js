/* The playground + the interactive examples: CodeMirror 6 editors over
   the wasm bindings (`check`, `fmt`, `lint`, `version` — their
   contracts live in editor/src/lib.rs of the `surform-editor` crate).
   This side converts the reports into squiggles, panels, and badges:
   check drives the DDL/canonical panes and the compile verdict, lint's
   advisories ride under the compile diagnostics, and fmt rewrites the
   buffer on demand.

   Two consumers share one factory:
   - the playground page (the `pg-*` ids, full chrome), and
   - every `figure.example[data-interactive]` the docs builder stamps
     with a JSON island — hydrated lazily, as it scrolls into view,
     with a Reset button that restores the verified original.

   Everything runs in the tab — there is no server. The keyword list
   arrives as a JSON island the docs builder stamps in at build time,
   so highlighting cannot drift from the compiler that verified the
   examples. */

import {
  EditorView,
  Decoration,
  ViewPlugin,
  keymap,
  drawSelection,
} from "@codemirror/view";
import {
  StateField,
  StateEffect,
} from "@codemirror/state";
import {
  defaultKeymap,
  history,
  historyKeymap,
  indentWithTab,
} from "@codemirror/commands";
import { closeBrackets, closeBracketsKeymap } from "@codemirror/autocomplete";
import {
  StreamLanguage,
  syntaxHighlighting,
  HighlightStyle,
} from "@codemirror/language";
import {
  tags as t,
  highlightTree,
  classHighlighter,
} from "@lezer/highlight";
/* The OFFICIAL SurrealQL grammar — SurrealDB's own parser artifact
   (@surrealdb/lezer, same repo as @surrealdb/codemirror). It inks the
   generated-SurrealQL panes; the SDL side stays our tokenizer, since
   SDL is ours to color. */
import { parser as surrealql } from "@surrealdb/lezer";

/* The wasm-bindgen glue — a committed build artifact this bundle
   imports and inlines; the wasm itself loads from next to the bundle
   at runtime. The initializer rides the default export (`--target
   web` glue shape). */
import init, { check, fmt, lint, version } from "../../assets/editor/surform_editor.js";
import { posToOffset, diagRange, debounce } from "./positions.js";
import { surrealqlRegions } from "./surrealql-regions.js";

/* ---------- the data islands the docs builder stamps in ---------- */

function island(id, fallback) {
  const el = document.getElementById(id);
  if (!el) return fallback;
  try {
    return JSON.parse(el.textContent);
  } catch {
    return fallback;
  }
}
const KEYWORDS = new Set(island("surform-keywords", []));
const EXAMPLES = island("surform-examples", []);

/* ---------- SDL mode: a stream tokenizer ---------- */
/* Deliberately shallow: SDL is brace-and-clause text; the compiler is
   the authority and the squiggles carry its verdict. The tokenizer
   makes the shape readable — the four SurrealQL comment syntaxes,
   strings, quoted idents, numbers (durations ride their digits), and
   the compiler's own keyword set, folded case-insensitively like the
   SurrealQL lexer's (a pasted `DEFINE TABLE` highlights too; SDL's
   own spellings are lowercase). */

const sdlMode = StreamLanguage.define({
  startState: () => ({ blockComment: false }),
  token(stream, state) {
    if (state.blockComment) {
      while (!stream.eol()) {
        if (stream.match("*/")) {
          state.blockComment = false;
          return "comment";
        }
        stream.next();
      }
      return "comment";
    }
    if (
      stream.match("//") ||
      stream.match("--") ||
      stream.match("#")
    ) {
      stream.skipToEnd();
      return "comment";
    }
    if (stream.match("/*")) {
      while (!stream.eol()) {
        if (stream.match("*/")) return "comment";
        stream.next();
      }
      state.blockComment = true;
      return "comment";
    }
    const ch = stream.peek();
    if (ch === "'" || ch === '"' || ch === "`") {
      stream.next();
      while (!stream.eol()) {
        const c = stream.next();
        if (c === "\\") stream.next();
        else if (c === ch) break;
      }
      return "string";
    }
    if (/[0-9]/.test(ch)) {
      stream.eatWhile(/[0-9a-zA-Z_.]/);
      return "number";
    }
    if (/[A-Za-z_]/.test(ch)) {
      stream.eatWhile(/[A-Za-z0-9_]/);
      // `eatWhile` returns a boolean — the consumed word is
      // `current()`; the lookup folds case like the SurrealQL lexer.
      return KEYWORDS.has(stream.current().toLowerCase())
        ? "keyword"
        : "variableName";
    }
    stream.next();
    return null;
  },
});

const highlight = syntaxHighlighting(
  HighlightStyle.define([
    { tag: t.keyword, color: "var(--accent)", fontWeight: "600" },
    { tag: t.string, color: "var(--ok)" },
    { tag: t.comment, color: "var(--muted)", fontStyle: "italic" },
    { tag: t.number, color: "var(--warn)" },
    { tag: t.variableName, color: "var(--fg)" },
  ]),
);

/* ---------- embedded SurrealQL ink ---------- */
/* Where SDL is SurrealQL-verbatim — the three verbatim bodies and
   every expression clause; surrealql-regions.js owns that line, per
   the spec's capture rules — the official grammar colors the region
   instead of the SDL tokenizer. The marks carry INLINE styles so they
   beat the stream tokenizer's class rules without touching it; the
   --ink-* variables keep them theme-aware (an inline style cannot
   carry per-theme selectors). */

/* Matched by tag NAME, not object identity — String(tag) is the tag's
   name (composed tags read like "function(name)"), and this survives
   bundler module duplication. */
const sqlInkByName = new Map([
  ["keyword", "color: var(--accent); font-weight: 600"],
  ["typeName", "color: var(--accent); font-weight: 600"],
  ["bool", "color: var(--accent); font-weight: 600"],
  ["atom", "color: var(--accent); font-weight: 600"],
  ["macroName", "color: var(--accent); font-weight: 600"],
  ["string", "color: var(--ink-string)"],
  ["literal", "color: var(--ink-string)"],
  ["number", "color: var(--ink-number)"],
  ["comment", "color: var(--muted); font-style: italic"],
  ["lineComment", "color: var(--muted); font-style: italic"],
  ["blockComment", "color: var(--muted); font-style: italic"],
  ["variableName", "color: var(--ink-variable)"],
  ["propertyName", "color: var(--ink-variable)"],
  ["function(name)", "color: var(--ink-variable)"],
  ["className", "color: var(--ink-variable)"],
  /* other tags the grammar emits (special(string) escapes, plain name,
     operator, punctuation) deliberately fall through: the stream
     tokenizer's ink or the plain foreground is right for them. */
]);
const sqlInkHighlighter = {
  style(tagList) {
    for (const tag of tagList) {
      const style = sqlInkByName.get(String(tag));
      if (style) return style;
    }
    return null;
  },
};

const sqlInk = ViewPlugin.fromClass(
  class {
    constructor(view) {
      this.marks = this.build(view);
    }
    update(update) {
      if (update.docChanged) this.marks = this.build(update.view);
    }
    build(view) {
      const text = view.state.doc.toString();
      const marks = [];
      for (const [from, to] of surrealqlRegions(text)) {
        if (to <= from) continue;
        /* Same posture as paintSql: the parse is total, the guard is
           insurance — a throw costs this region its ink, never the
           editor. */
        try {
          const tree = surrealql.parse(text.slice(from, to));
          highlightTree(tree, sqlInkHighlighter, (a, b, style) => {
            if (b > a && style) {
              marks.push(
                Decoration.mark({ attributes: { style } }).range(from + a, from + b),
              );
            }
          });
        } catch {
          /* highlighting is cosmetic */
        }
      }
      return Decoration.set(marks, true);
    }
  },
  { decorations: (plugin) => plugin.marks },
);

/* The theme rides the site's CSS variables, so the theme toggle (a
   data-theme flip on <html>) restyles the editor for free. The
   height is the wrapper's business: the playground pins its editor
   box, the inline examples size to their content (capped in CSS). */
const pgTheme = EditorView.theme({
  "&": {
    backgroundColor: "var(--code-bg)",
    color: "var(--fg)",
    height: "100%",
    fontSize: "14.5px",
  },
  ".cm-scroller": {
    overflow: "auto",
    lineHeight: "1.55",
    fontFamily: "var(--mono)",
  },
  ".cm-content": { caretColor: "var(--accent)", padding: "10px 0" },
  ".cm-line": { padding: "0 14px" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--accent)" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
    { backgroundColor: "var(--accent-soft)" },
  ".cm-activeLine": {
    backgroundColor: "color-mix(in srgb, var(--fg) 4%, transparent)",
  },
  ".cm-gutters": { display: "none" },
});

/* ---------- squiggles ---------- */

const setDiags = StateEffect.define();

const diagField = StateField.define({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const e of tr.effects) {
      if (e.is(setDiags)) {
        const text = tr.state.doc.toString();
        const marks = [];
        for (const d of e.value) {
          if (!d.start || !d.end) continue;
          const range = diagRange(text, d.start, d.end);
          // Empty text has no ink for a point span; an empty range
          // would make CodeMirror's mark decoration throw.
          if (range.to <= range.from) continue;
          marks.push(
            Decoration.mark({
              class: d.severity === "warning" ? "pg-diag pg-warn" : "pg-diag pg-error",
              attributes: { title: `${d.code} ${d.message.replaceAll("\n", " ⏎ ")}` },
            }).range(range.from, range.to),
          );
        }
        return Decoration.set(marks, true);
      }
    }
    // A stale squiggle lies about where the error is: edits clear
    // them until the debounced check lands (~300 ms).
    return tr.docChanged ? Decoration.none : deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

/* ---------- shared rendering helpers ---------- */

function esc(text) {
  return text.replace(/[&<>"]/g, (c) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
  })[c]);
}

/* The generated-SurrealQL panes (`surform build` output): inked by
   the official grammar, not the SDL tokenizer. The caller supplies
   the exact text — a verified report's DDL — so painting never
   re-computes what a pane holds; the engine's verdict always
   outranks the ink. (The canonical and fmt panes hold SDL, so they
   stay with the build-time SDL ink.) A parse miss (or a throw)
   leaves the pane plain: highlighting is cosmetic. */
function paintSql(el, text) {
  try {
    const tree = surrealql.parse(text);
    let html = "";
    let pos = 0;
    highlightTree(tree, classHighlighter, (from, to, classes) => {
      html += esc(text.slice(pos, from));
      html += `<span class="${classes}">${esc(text.slice(from, to))}</span>`;
      pos = to;
    });
    html += esc(text.slice(pos));
    el.innerHTML = html;
  } catch {
    el.textContent = text;
  }
}

function setStatus(el, ok, count) {
  if (ok) {
    el.textContent = "✓ compiles clean";
    el.className = "badge pg-ok ex-status";
  } else {
    el.textContent = `${count} diagnostic${count === 1 ? "" : "s"}`;
    el.className = "badge pg-bad ex-status";
  }
}

/* The diagnostics list: site-styled entries — code chip, position,
   message, help — and a click jumps the editor to the offense. */
function renderCards(container, diagnostics, view) {
  container.replaceChildren();
  for (const d of diagnostics) {
    const item = document.createElement("div");
    item.className =
      "pg-diag-item" + (d.severity === "warning" ? " pg-diag-item-warning" : "");
    const where = d.start ? `${d.start.line}:${d.start.col}` : "";
    item.innerHTML =
      `<span class="pg-diag-code">${esc(d.code)}</span>` +
      (where ? `<span class="pg-diag-where">${where}</span>` : "") +
      `<span class="pg-diag-msg">${esc(d.message)}</span>` +
      (d.help ? `<span class="pg-diag-help">${esc(d.help)}</span>` : "");
    item.addEventListener("click", () => {
      if (!d.start) return;
      const pos = posToOffset(view.state.doc.toString(), d.start.line, d.start.col);
      view.dispatch({ selection: { anchor: pos }, scrollIntoView: true });
      view.focus();
    });
    container.appendChild(item);
  }
}

/* ---------- the wasm boot: one shared promise ---------- */
/* The glue memoizes init(), so any number of editors await the same
   load; the 1 MB-gz wasm is fetched once per visit, on first need. */

let wasmError = null;
const booted = init()
  .then(() => true)
  .catch((err) => {
    wasmError = String(err);
    return false;
  });

/* ---------- the editor factory ---------- */
/* One instance per example (and one for the playground). `opts`:
   - parent     element the CodeMirror view mounts into
   - doc        the initial (verified) text; `reset()` restores it
   - ddl / canonical / fmtPane / cards
                output elements, present when the figure has the pane
   - status     the verdict badge element (optional)
   - onBooted(ok)  called once the wasm load settles (optional)

   Typing ergonomics: brackets close in pairs, Tab inserts indentation
   (Escape hands focus back to the page), Mod-Enter forces a check. */

function createEditor(opts) {
  let original = opts.doc;
  const view = new EditorView({
    doc: opts.doc,
    extensions: [
      history(),
      drawSelection(),
      pgTheme,
      highlight,
      sdlMode,
      sqlInk,
      diagField,
      keymap.of([
        {
          key: "Mod-Enter",
          run: () => {
            run.now();
            return true;
          },
        },
        {
          // Tab indents inside the editor; Escape hands focus back to
          // the page so keyboard users can still tab through.
          key: "Escape",
          run: (v) => {
            v.contentDOM.blur();
            return true;
          },
        },
      ]),
      closeBrackets(),
      keymap.of([
        ...closeBracketsKeymap,
        ...defaultKeymap,
        ...historyKeymap,
        indentWithTab,
      ]),
      EditorView.updateListener.of((u) => {
        if (u.docChanged) run();
      }),
    ],
    parent: opts.parent,
  });

  const run = debounce(() => {
    booted.then((ok) => {
      if (!ok) return;
      const text = view.state.doc.toString();
      const report = JSON.parse(check(text));
      // Advisories ride after the compile verdict on a clean parse —
      // the squiggles and the cards show both, warnings under errors.
      let diagnostics = report.diagnostics;
      if (report.ok) {
        const advisories = JSON.parse(lint(text));
        diagnostics = diagnostics.concat(advisories.diagnostics);
      }
      view.dispatch({ effects: setDiags.of(diagnostics) });
      if (opts.cards) renderCards(opts.cards, diagnostics, view);
      if (opts.ddl) paintSql(opts.ddl, report.ddl ?? "");
      if (opts.canonical) opts.canonical.textContent = report.canonical ?? "";
      if (opts.fmtPane) {
        const f = JSON.parse(fmt(text));
        opts.fmtPane.textContent = f.ok ? (f.formatted ?? "") : "";
      }
      if (opts.status) setStatus(opts.status, report.ok, report.diagnostics.length);
    });
  }, 300);

  function format() {
    booted.then((ok) => {
      if (!ok) return;
      const text = view.state.doc.toString();
      const result = JSON.parse(fmt(text));
      if (result.ok && result.formatted !== text) {
        view.dispatch({
          changes: { from: 0, to: view.state.doc.length, insert: result.formatted },
        });
        run.now();
      }
      view.focus();
    });
  }

  function setDoc(text) {
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: text },
    });
    original = text;
    run.now();
    view.focus();
  }

  function reset() {
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: original },
    });
    run.now();
    view.focus();
  }

  /* The boot gate: the wasm load settles once per visit; every editor
     fires its first check then (inline examples hydrate after boot,
     so their gate fires immediately on creation). A failed load
     surfaces on the editor's own status badge. */
  booted.then((ok) => {
    if (opts.onBooted) opts.onBooted(ok, wasmError);
    if (opts.status && !ok) {
      opts.status.textContent = "the compiler failed to load";
      opts.status.className = "badge pg-bad ex-status";
    }
    if (ok) run.now();
  });

  return { view, run, reset, format, setDoc };
}

/* ---------- the playground page (the `pg-*` ids) ---------- */

const $ = (id) => document.getElementById(id);
const pgRoot = $("pg-editor");

if (pgRoot) {
  const statusEl = $("pg-status");
  const versionEl = $("pg-version");
  const editor = createEditor({
    parent: pgRoot,
    doc: EXAMPLES[0]?.content ?? "",
    ddl: $("pg-ddl-code"),
    canonical: $("pg-canon-code"),
    cards: $("pg-diags"),
    status: statusEl,
    onBooted(ok) {
      if (ok) {
        versionEl.textContent = version();
      } else {
        statusEl.textContent = "the compiler failed to load";
        statusEl.className = "badge pg-bad";
        versionEl.textContent = wasmError ?? "";
      }
    },
  });

  /* The example loader: the same verified bytes the docs pages show. */
  const select = $("pg-example");
  for (const [i, ex] of EXAMPLES.entries()) {
    const opt = document.createElement("option");
    opt.value = String(i);
    opt.textContent = ex.label;
    select.appendChild(opt);
  }
  select.addEventListener("change", () => {
    const ex = EXAMPLES[Number(select.value)];
    if (!ex) return;
    editor.setDoc(ex.content);
  });

  $("pg-compile").addEventListener("click", () => {
    editor.run.now();
    editor.view.focus();
  });
  $("pg-format").addEventListener("click", () => editor.format());
}

/* ---------- the interactive examples ---------- */
/* The docs builder stamps every editable example with a JSON island
   (the verified fence text) and data-pane attributes on its output
   panes. The static server-highlighted panels stay in the HTML until
   an example scrolls near the viewport — then the editor replaces
   the authored pane and the outputs go live. */

function hydrateExample(figure) {
  const islandEl = figure.querySelector("script.surform-example");
  if (!islandEl) return;
  let data;
  try {
    data = JSON.parse(islandEl.textContent);
  } catch {
    return;
  }
  if (!data || typeof data.body !== "string") return;
  const host = figure.querySelector('[data-pane="editor"] .codebox');
  if (!host) return;

  /* Output panes: the static verified content stays visible until
     the first live check replaces it. The canonical pane is NOT
     wired — its label promises the engine's own record (the merged
     readback the builder computed), while the browser can only
     produce authored-canonical — so it stays the verified truth. */
  const panes = {};
  for (const name of ["ddl", "fmt"]) {
    const el = figure.querySelector(`[data-pane="${name}"] pre code`);
    if (el) panes[name] = el;
  }
  let cards = null;
  const cardsBlock = figure.querySelector('[data-pane="diags"], [data-pane="lint"]');
  if (cardsBlock) {
    const codebox = cardsBlock.querySelector(".codebox");
    cards = document.createElement("div");
    cards.className = "pg-diags";
    cards.setAttribute("aria-live", "polite");
    if (codebox) codebox.replaceWith(cards);
    else cardsBlock.appendChild(cards);
  }

  /* The authored pane's static pre becomes the editor; the copy
     button goes with it (Reset restores the original text). The
     badge says what's happening while the wasm streams in. */
  const pre = host.querySelector("pre");
  const mount = document.createElement("div");
  mount.className = "ex-editor";
  if (pre) pre.replaceWith(mount);
  else host.appendChild(mount);
  const copy = host.querySelector(".copy");
  if (copy) copy.remove();

  const toolbar = figure.querySelector(".ex-toolbar");
  if (toolbar) {
    toolbar.hidden = false;
    const badge = toolbar.querySelector(".ex-status");
    if (badge) badge.textContent = "loading the compiler…";
  }
  const status = toolbar ? toolbar.querySelector(".ex-status") : null;

  const editor = createEditor({
    parent: mount,
    doc: data.body,
    ddl: panes.ddl ?? null,
    fmtPane: panes.fmt ?? null,
    cards,
    status,
  });

  toolbar
    ?.querySelector('[data-act="reset"]')
    ?.addEventListener("click", () => editor.reset());
  toolbar
    ?.querySelector('[data-act="format"]')
    ?.addEventListener("click", () => editor.format());
}

const figures = Array.from(
  document.querySelectorAll("figure.example[data-interactive]"),
);
if (figures.length) {
  if ("IntersectionObserver" in window) {
    /* Hydrate just before an example is needed — the wasm (1 MB gz)
       is fetched once, on the first example near the viewport. */
    const io = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          io.unobserve(entry.target);
          try {
            hydrateExample(entry.target);
          } catch (err) {
            console.warn("example hydration failed:", err);
          }
        }
      },
      { rootMargin: "240px 0px" },
    );
    figures.forEach((figure) => io.observe(figure));
  } else {
    figures.forEach((figure) => {
      try {
        hydrateExample(figure);
      } catch (err) {
        console.warn("example hydration failed:", err);
      }
    });
  }
}
