/* The formatter integration's self-checks: `node fmtselfcheck.mjs` —
   plain Node, driving the REAL binary through the exact spawn
   formatter.js performs (the selfcheck.mjs precedent for
   dependency-free checks). The binary comes from the repo's target/
   unless SURFORM_BIN says otherwise; run `cargo build` first. */

import { execFile as execFileCallback } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import fmt from "./formatter.js";

const { formatSource, toDocumentEol } = fmt;
const execFile = promisify(execFileCallback);

const here = new URL(".", import.meta.url);
const bin =
  process.env.SURFORM_BIN ??
  fileURLToPath(new URL("../../target/debug/surform", here));

let failures = 0;
function check(label, ok) {
  if (!ok) {
    failures += 1;
    console.error(`FAIL ${label}`);
  } else {
    console.log(`ok   ${label}`);
  }
}

if (!existsSync(bin)) {
  console.error(`FAIL the surform binary is missing at ${bin} — run cargo build`);
  process.exit(1);
}

const quickPath = fileURLToPath(
  new URL("../../docs/examples/quick.surdl", here),
);
const quick = readFileSync(quickPath, "utf8");
const broken = readFileSync(
  new URL("../../docs/examples/broken.surdl", here),
  "utf8",
);

/* 1 — the pipe and the file agree byte-for-byte: the extension sends
   unsaved buffer text over stdin, and the canonical writer must not
   care how the source arrived. */
const viaFile = await execFile(bin, ["fmt", quickPath]);
const viaPipe = await formatSource(bin, quick);
check(
  "stdin mode is byte-identical to file mode",
  viaPipe.ok && viaPipe.formatted === viaFile.stdout,
);

/* 2 — the fixed point: formatting the pipe's output changes nothing
   (guarded so a test-1 regression fails cleanly instead of throwing). */
const twice = viaPipe.ok
  ? await formatSource(bin, viaPipe.formatted)
  : undefined;
check(
  "the pipe's output is a fixed point",
  twice?.ok && twice.formatted === viaPipe.formatted,
);

/* 3 — a dirty buffer is rejected, never formatted, and the
   diagnostics arrive as structured { code, message } pairs. */
const dirty = await formatSource(bin, broken);
check(
  "a dirty buffer is rejected with diagnostics",
  !dirty.ok &&
    dirty.kind === "diagnostics" &&
    (dirty.diagnostics?.length ?? 0) > 0,
);
const first = dirty.diagnostics?.[0];
check(
  "diagnostics carry a code and a message",
  !!first && /^SDL-E\d{4}$/.test(first.code ?? "") && first.message.length > 0,
);

/* 4 — a missing binary maps to not-found (the one-time-toast path),
   not to a generic crash. */
const missing = await formatSource(`${bin}-selfcheck-missing`, quick);
check(
  "a missing binary maps to not-found",
  !missing.ok && missing.kind === "not-found",
);

/* 5 — the CRLF conversion is guarded: regex-literal interiors can
   carry raw newlines in canonical output (upstream prints regex
   source verbatim), so an interior `\r\n` must survive every round
   and the conversion must be idempotent — an unguarded replace
   compounded `\r\n` → `\r\r\n` → … per format. */
const mixed = "a\nb\r\nc";
const once = toDocumentEol(mixed, "\r\n");
check(
  "crlf conversion converts bare newlines once and keeps \\r\\n",
  once === "a\r\nb\r\nc" && toDocumentEol(once, "\r\n") === once,
);
const regexInterior = "/x\r\ny/\n";
const converted = toDocumentEol(regexInterior, "\r\n");
check(
  "a \\r\\n regex interior survives and never compounds",
  converted === "/x\r\ny/\r\n" && toDocumentEol(converted, "\r\n") === converted,
);
check("lf documents pass through untouched", toDocumentEol(mixed, "\n") === mixed);

if (failures > 0) {
  console.error(`\n${failures} failure(s)`);
  process.exit(1);
}
console.log("\nall formatter checks passed");
