/* The formatter's engine room: spawn the surform binary over the
   stdin pipe and map the outcome onto four shapes. Pure Node — no
   `vscode` import — so fmtselfcheck.mjs can drive it directly; this
   module must never learn about the editor. */

"use strict";

const { execFile } = require("node:child_process");

const TIMEOUT_MS = 10_000;
const MAX_BUFFER = 16 * 1024 * 1024;

/* formatSource(bin, text) → Promise<result>
   - { ok: true, formatted }  the canonical SDL (exit 0)
   - { ok: false, kind: "not-found", message }
   - { ok: false, kind: "diagnostics", diagnostics: [{ code, message }] }
     the buffer did not parse — the compiler's `--json` objects
   - { ok: false, kind: "failed", message }
     everything else: crash, timeout, stderr that is not the protocol */
function formatSource(bin, text) {
  return new Promise((resolve) => {
    const child = execFile(
      bin,
      ["fmt", "--json"],
      { timeout: TIMEOUT_MS, maxBuffer: MAX_BUFFER, encoding: "utf8" },
      (error, stdout, stderr) => {
        if (error && error.code === "ENOENT") {
          resolve({ ok: false, kind: "not-found", message: `no binary at '${bin}'` });
          return;
        }
        if (error && error.killed) {
          resolve({
            ok: false,
            kind: "failed",
            message: `timed out after ${TIMEOUT_MS / 1000}s`,
          });
          return;
        }
        if (!error) {
          resolve({ ok: true, formatted: stdout });
          return;
        }
        const diagnostics = parseDiagnostics(stderr);
        if (diagnostics) {
          resolve({ ok: false, kind: "diagnostics", diagnostics });
          return;
        }
        resolve({
          ok: false,
          kind: "failed",
          message:
            trimmed(stderr) ||
            (typeof error.code === "number"
              ? `exited with code ${error.code}`
              : error.message),
        });
      },
    );
    // A binary that exits without draining stdin — a stale surform
    // whose `fmt` still required a file, say — makes a >64KB write
    // fail with EPIPE; without a listener that is an uncaught
    // exception in the extension host.
    child.stdin.on("error", () => {});
    child.stdin.end(text);
  });
}

/* The canonical writer emits LF, but NOT everywhere: upstream prints
   regex-literal interiors verbatim, so canonical output can carry raw
   newlines inside a regex. The CRLF conversion therefore only touches
   a `\n` no `\r` precedes — an interior `\r\n` survives every round,
   and a bare interior `\n` flips to CRLF exactly once. An unguarded
   replace would compound (`\r\n` → `\r\r\n` → …) on every format. */
function toDocumentEol(formatted, eol) {
  if (eol !== "\r\n") return formatted;
  return formatted.replace(/(?<!\r)\n/g, "\r\n");
}

/* One miette `--json` object per stderr line; any non-JSON line means
   the output is not the protocol (a crash banner, say) and the raw
   text surfaces instead. */
function parseDiagnostics(stderr) {
  const diagnostics = [];
  for (const line of stderr.split("\n")) {
    const trimmedLine = line.trim();
    if (!trimmedLine) continue;
    try {
      const parsed = JSON.parse(trimmedLine);
      diagnostics.push({ code: parsed.code ?? "", message: parsed.message ?? "" });
    } catch {
      return null;
    }
  }
  return diagnostics.length > 0 ? diagnostics : null;
}

function trimmed(text) {
  return (text || "").trim();
}

module.exports = { formatSource, toDocumentEol };
