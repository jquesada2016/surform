/* Pure helpers for the playground — no DOM, no CodeMirror, so this
   module smoke-tests in plain Node (node docs/editor/src/positions.js
   runs its self-checks). The coordinate conversion is the binding
   contract's other half: the wasm `check` reports 1-indexed (line,
   char-column) positions, CodeMirror wants UTF-16 offsets into the
   current document. */

/** Converts a 1-indexed (line, charCol) pair — columns in Unicode
    chars, the compiler's anchor semantics — to a UTF-16 offset into
    `text`. Clamps defensively: a position past the end of the text
    (or mid-line) lands on the nearest boundary, so a squiggle never
    throws. */
export function posToOffset(text, line1, charCol) {
  const lines = text.split("\n");
  const lineIdx = Math.min(Math.max(line1, 1), lines.length) - 1;
  let offset = 0;
  for (let i = 0; i < lineIdx; i++) {
    offset += lines[i].length + 1; // UTF-16 length + the newline
  }
  // Walk the target line in code points, counting UTF-16 units until
  // `charCol` chars are consumed (an astral char is one char, two
  // units — the desync the contract warns about).
  let units = 0;
  let chars = 1;
  for (const ch of lines[lineIdx]) {
    if (chars >= charCol) break;
    units += ch.codePointAt(0) > 0xffff ? 2 : 1;
    chars++;
  }
  return offset + units;
}

/** Converts both ends of a report diagnostic into a CodeMirror
    {from, to} pair. Point spans (end == start) get a non-empty range
    over the character at the position where there is one — an empty
    range renders nothing, and the compiler's carets deserve ink. An
    anchor at (or past) the very end of the text would render empty —
    CodeMirror THROWS on empty mark ranges — so there `from` pulls
    back over the last character instead. Only the empty text has no
    ink to give: that pair is empty, and the caller must skip it. */
export function diagRange(text, start, end) {
  let from = posToOffset(text, start.line, start.col);
  let to = Math.min(
    Math.max(from + 1, posToOffset(text, end.line, end.col)),
    text.length,
  );
  if (from >= to && to > 0) {
    from = to - 1;
  }
  return { from, to };
}

/** Debounce: trailing-edge, cancellable. The playground checks on a
    ~300 ms idle gap; Mod-Enter compiles now by cancelling the wait. */
export function debounce(fn, ms) {
  let timer = null;
  const wrapped = (...args) => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      fn(...args);
    }, ms);
  };
  wrapped.now = (...args) => {
    clearTimeout(timer);
    timer = null;
    fn(...args);
  };
  wrapped.cancel = () => {
    clearTimeout(timer);
    timer = null;
  };
  return wrapped;
}

/* ---------- self-checks (plain Node) ---------- */
if (
  typeof process !== "undefined" &&
  process.argv[1] &&
  import.meta.url.endsWith(process.argv[1].split("/").pop())
) {
  const assert = (cond, what) => {
    if (!cond) {
      console.error("FAIL:", what);
      process.exitCode = 1;
    } else {
      console.log("ok:", what);
    }
  };
  // ASCII: byte lengths coincide.
  assert(posToOffset("ab\ncd", 1, 1) === 0, "line 1 col 1");
  assert(posToOffset("ab\ncd", 1, 3) === 2, "line 1 col 3");
  assert(posToOffset("ab\ncd", 2, 1) === 3, "line 2 col 1");
  // The char-column pin from the binding's own test: the offense
  // after a 2-byte char reads col 15 in chars and col 16 in bytes —
  // and its UTF-16 offset is 14, because é is one BMP unit. All three
  // disagree; the compiler reports chars, CodeMirror wants units.
  assert(
    posToOffset("namespace `é` @;", 1, 15) === 14,
    "chars to UTF-16 units (é is one unit, two bytes)",
  );
  assert(
    posToOffset("namespace \u{1F600} @;", 1, 13) === 13,
    "astral char: 1 char, 2 UTF-16 units",
  );
  // Clamping never throws.
  assert(posToOffset("ab", 9, 9) === 2, "past-the-end clamps to text end");
  // A point span at text end inks the LAST character — an empty
  // range would make CodeMirror's mark decoration throw (a bare
  // `namespace` anchors there).
  const range = diagRange("ab", { line: 1, col: 3 }, { line: 1, col: 3 });
  assert(range.from === 1 && range.to === 2, "point span at text end inks the last char");
  const empty = diagRange("", { line: 1, col: 1 }, { line: 1, col: 1 });
  assert(empty.from === 0 && empty.to === 0, "empty text has no ink — caller skips");
  const range2 = diagRange("x;", { line: 1, col: 2 }, { line: 1, col: 2 });
  assert(range2.from === 1 && range2.to === 2, "point span gets ink over one char");
  let calls = 0;
  const d = debounce(() => calls++, 5);
  d();
  d();
  d();
  setTimeout(() => {
    assert(calls === 1, "debounce collapses the burst to one call");
    d.now();
    assert(calls === 2, "debounce.now is immediate");
  }, 30);
}
