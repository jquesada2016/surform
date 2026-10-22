/* The SDL → SurrealQL region scanner: where in an SDL document the
   language is SurrealQL-verbatim, as [start, end] offsets.

   Single source of truth for the editors' embedded-SurrealQL ink
   (main.js's sqlInk plugin). It consumes the spec's
   partition — the three verbatim bodies (fn bodies, view queries,
   raw) plus every expression clause and the `=` assignment short
   form — and the scanner discipline that captures them: strings
   (six prefixes × two quote styles, `\` escapes), the four SurrealQL
   comment syntaxes, and quoted identifiers (`…` and ⟨…⟩) are skipped;
   captures end at a `;` at bracket depth 0 (a stray depth-0 `}` ends
   one early — malformed input degrades, never runs away). Field
   headers are skipped as a unit first (rule 4: types are SDL-lexed,
   never captures — an object-schema member named `value` or `default`
   must not fire the expression-clause keys).

   Pure and total: malformed input yields fewer/shorter regions, never
   an exception. `selfcheck` below runs under Node. */

const EXPR_KEYS = new Set([
  "value",
  "default",
  "assert",
  "computed",
  "when",
  "then",
  "where",
  "signup",
  "signin",
  "authenticate",
  "url",
  "key",
  "context",
  "backend",
  "batch",
  "start",
  "timeout",
  "token",
  "session",
  "grant",
  "middleware",
  "columns",
  "fields",
  "permissions",
  "database",
  "namespace",
  "select",
  "create",
  "update",
  "delete",
]);

/* `key word, key word: …` comma groups share one value — the per-op
   permission words are the only keys that group. */
const GROUP_KEYS = new Set(["select", "create", "update", "delete"]);

const isWordChar = (c) => /[A-Za-z0-9_-]/.test(c);
const isPrefixLetter = (c) => /[dsubfrDSUBFR]/.test(c);

export function surrealqlRegions(text) {
  const regions = [];
  const n = text.length;
  let i = 0;

  /* All skip helpers are PURE — they take the position to scan from
     and return the position after; none touch the outer cursor. The
     class of bug this kills: a region start read after a capture
     has run the shared cursor forward. */
  const skipRestOfLine = (j) => {
    while (j < n && text[j] !== "\n") j += 1;
    return j;
  };
  const skipBlockComment = (j) => {
    j += 2;
    while (j < n && !(text[j] === "*" && text[j + 1] === "/")) j += 1;
    return Math.min(n, j + 2);
  };
  /* start is ON the opening quote; returns just past the closing
     quote, honoring `\` escapes. */
  const skipStringBody = (quote, start) => {
    let k = start + 1;
    while (k < n) {
      const c = text[k];
      if (c === "\\") k += 2;
      else if (c === quote) return k + 1;
      else k += 1;
    }
    return n;
  };
  const skipQuotedIdent = (start, open, close) => {
    let k = start + open.length;
    while (k < n) {
      if (text[k] === "\\") k += 2;
      else if (text.startsWith(close, k)) return k + close.length;
      else k += 1;
    }
    return n;
  };
  /* The capture skip set: from j, advance past one skipped shape and
     return the new index — or -1 when the shape at j isn't skippable. */
  const skipShape = (j) => {
    const c = text[j];
    if (c === "/" && text[j + 1] === "/") return skipRestOfLine(j);
    if (c === "-" && text[j + 1] === "-") return skipRestOfLine(j);
    if (c === "#") return skipRestOfLine(j);
    if (c === "/" && text[j + 1] === "*") return skipBlockComment(j);
    if (c === "'" || c === '"') return skipStringBody(c, j);
    if (isPrefixLetter(c)) {
      const next = text[j + 1];
      if (next === "'" || next === '"') {
        const before = j === 0 || !isWordChar(text[j - 1]);
        return before ? skipStringBody(next, j + 1) : -1;
      }
    }
    if (c === "`") return skipQuotedIdent(j, "`", "`");
    if (c === "⟨") return skipQuotedIdent(j, "⟨", "⟩");
    return -1;
  };
  const readWord = (j) => {
    let k = j;
    while (k < n && isWordChar(text[k])) k += 1;
    return [text.slice(j, k), k];
  };
  /* The capture: from the first content character after `key:` (or
     inside a brace body), consume with the full skip set until a `;`
     at depth 0 (inclusive) — mirrors the Rust scanner: braces opened
     during the capture may legally close again (object literals,
     blocks), and only an unbalanced `}` (depth underflow) ends the
     capture early, as degraded malformed input. `openBrace` marks a
     brace-bodied capture (fn/raw bodies): depth starts at 1 and the
     matching `}` ends it, also exclusive. */
  const capture = (from, openBrace) => {
    let depth = openBrace ? 1 : 0;
    let j = from;
    while (j < n) {
      const skipped = skipShape(j);
      if (skipped !== -1) {
        j = skipped;
        continue;
      }
      const c = text[j];
      if (c === "(" || c === "{" || c === "[") {
        depth += 1;
        j += 1;
      } else if (c === ")" || c === "]") {
        depth -= 1;
        j += 1;
      } else if (c === "}") {
        depth -= 1;
        j += 1;
        if (depth < 0) return j - 1;
        if (openBrace && depth === 0) return j - 1;
      } else if (c === ";" && depth === 0) {
        return j + 1;
      } else {
        j += 1;
      }
    }
    return n;
  };
  /* Field headers: `field path: kind` — the type grammar is SDL's
     (rule 4), so the header is skipped as a unit until the statement's
     `;`, its clause block's `{`, or the computed `=` (which hands back
     for an assignment capture). Braces inside the type (object
     schemas) balance. Returns the resume index. */
  const skipFieldHeader = (start) => {
    let depth = 0;
    let sawType = false;
    let j = start;
    while (j < n) {
      const skipped = skipShape(j);
      if (skipped !== -1) {
        j = skipped;
        sawType = true;
        continue;
      }
      const c = text[j];
      if (isWordChar(c)) {
        sawType = true;
        j += 1;
        continue;
      }
      if (c === "{" || c === "(" || c === "[") {
        /* A '{' with no type since the ':' is the kind itself — an
           object schema, brace-bearing (rule 4), balanced here; a '{'
           after the type is the clause block. */
        if (depth === 0 && c === "{" && sawType) return j;
        depth += 1;
        sawType = true;
        j += 1;
      } else if (c === "}" || c === ")" || c === "]") {
        depth -= 1;
        j += 1;
      } else if (depth === 0 && c === ";") {
        return j;
      } else if (depth === 0 && c === "=") {
        const end = capture(j + 1, false);
        regions.push([j + 1, end]);
        return end;
      } else if (c === ":") {
        /* the type — and with it the object-schema question — starts
           after the colon; the path word must not count as type */
        sawType = false;
        j += 1;
      } else {
        j += 1;
      }
    }
    return n;
  };

  while (i < n) {
    const skipped = skipShape(i);
    if (skipped !== -1) {
      i = skipped;
      continue;
    }
    const c = text[i];
    if (isWordChar(c) && !/\d/.test(c)) {
      const [word, after] = readWord(i);
      const lower = word.toLowerCase();
      /* field headers first — their member names would otherwise read
         as clause keys. */
      if (lower === "field") {
        i = skipFieldHeader(after);
        continue;
      }
      if (lower === "raw" || lower === "fn") {
        /* Scan the header for the body's `{`: `fn` carries a
           signature (balanced parens, optional return); `raw` goes
           straight there. A `;` before any `{` aborts (malformed). */
        let depth = 0;
        let j = after;
        let found = -1;
        while (j < n) {
          const sk = skipShape(j);
          if (sk !== -1) {
            j = sk;
            continue;
          }
          const h = text[j];
          if (h === "(" || h === "<" || h === "[") depth += 1;
          else if (h === ")" || h === ">") depth = Math.max(0, depth - 1);
          else if (h === ";" && depth === 0) break;
          else if (h === "{" && depth === 0) {
            found = j;
            break;
          }
          j += 1;
        }
        if (found !== -1) {
          const end = capture(found + 1, true);
          regions.push([found + 1, end]);
          i = Math.max(end, found + 1);
          continue;
        }
        i = j;
        continue;
      }
      if (lower === "table") {
        /* `table view <name> {` — the query is the keyless body, a
           `;`-terminated capture inside the block. A name is REQUIRED
           between `view` and `{`: variant words are only variants when
           followed by one, so `table view { … };` is a plain table
           NAMED view. */
        let j = after;
        while (j < n && /\s/.test(text[j])) j += 1;
        const [nextWord, afterNext] = readWord(j);
        if (nextWord.toLowerCase() === "view") {
          j = afterNext;
          while (j < n && /\s/.test(text[j])) j += 1;
          if (isWordChar(text[j]) || text[j] === "`" || text[j] === "⟨") {
            const sk = skipShape(j);
            if (sk !== -1) {
              j = sk;
            } else {
              const [, nameAfter] = readWord(j);
              j = nameAfter;
            }
            while (j < n && /\s/.test(text[j])) j += 1;
            if (text[j] === "{") {
              const end = capture(j + 1, false);
              regions.push([j + 1, end]);
              i = Math.max(end, j + 1);
              continue;
            }
          }
        }
        i = afterNext;
        continue;
      }
      /* expression clauses: `key:` (with the two-word keys and the
         per-op comma groups). The top-level `=` assignment is handled
         below, outside the word branch. */
      let j = after;
      while (j < n && /\s/.test(text[j])) j += 1;
      let isExpr = false;
      if (text[j] === ":") {
        isExpr = EXPR_KEYS.has(lower);
      } else if (text[j] === ",") {
        isExpr = GROUP_KEYS.has(lower);
        while (isExpr) {
          j += 1;
          while (j < n && /\s/.test(text[j])) j += 1;
          const [groupWord, groupAfter] = readWord(j);
          if (!GROUP_KEYS.has(groupWord.toLowerCase())) {
            isExpr = false;
            break;
          }
          j = groupAfter;
          while (j < n && /\s/.test(text[j])) j += 1;
          if (text[j] === ",") continue;
          if (text[j] === ":") break;
          isExpr = false;
          break;
        }
      } else if (lower === "default" || lower === "on") {
        /* two-word keys: `default always:`, `on delete:` — the next
           word selects the pair; alone, the single-key branch above
           already handled `default:`. */
        const [second, secondAfter] = readWord(j);
        const pair = `${lower} ${second.toLowerCase()}`;
        if (pair === "default always" || pair === "on delete") {
          j = secondAfter;
          while (j < n && /\s/.test(text[j])) j += 1;
          isExpr = text[j] === ":";
        }
      }
      if (isExpr && text[j] === ":") {
        const end = capture(j + 1, false);
        regions.push([j + 1, end]);
        i = Math.max(end, j + 1);
        continue;
      }
      i = after;
      continue;
    }
    if (c === "=") {
      /* Bind the region start before capturing — capture() returns
         only an end, and the start reads from this loop's own
         position. */
      const start = i + 1;
      const end = capture(start, false);
      regions.push([start, end]);
      i = Math.max(end, start);
      continue;
    }
    i += 1;
  }
  return regions;
}
