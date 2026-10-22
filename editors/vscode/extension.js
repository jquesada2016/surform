/* The extension's activation: one thing — a DocumentFormattingEditProvider
   that pipes the buffer through the surform binary's canonical writer.
   Everything spawn-shaped lives in formatter.js (pure Node,
   self-checked); this file only knows the editor API. */

"use strict";

const path = require("node:path");
const vscode = require("vscode");
const { formatSource, toDocumentEol } = require("./formatter.js");

// The missing-binary complaint is a one-time announcement: a toast on
// every format attempt would be noise.
let notFoundAnnounced = false;

function activate(context) {
  const output = vscode.window.createOutputChannel("Surform");
  const provider = {
    async provideDocumentFormattingEdits(document) {
      const text = document.getText();
      const result = await formatSource(resolveBin(document), text);
      if (!result.ok) {
        reportFailure(result, output, document);
        return [];
      }
      const formatted = toDocumentEol(
        result.formatted,
        document.eol === vscode.EndOfLine.CRLF ? "\r\n" : "\n",
      );
      if (formatted === text) return [];
      const range = new vscode.Range(
        document.positionAt(0),
        document.positionAt(text.length),
      );
      return [vscode.TextEdit.replace(range, formatted)];
    },
  };
  context.subscriptions.push(
    vscode.languages.registerDocumentFormattingEditProvider(
      { language: "surform" },
      provider,
    ),
    output,
  );
}

/* `surform.path` is a command name (PATH lookup) unless it carries a
   separator; a relative path resolves against the document's
   workspace folder, not the extension host's cwd. */
function resolveBin(document) {
  let configured = vscode.workspace
    .getConfiguration("surform", document.uri)
    .get("path", "surform");
  if (!configured) configured = "surform";
  if (configured.includes("/") || configured.includes("\\")) {
    if (path.isAbsolute(configured)) return configured;
    const folder = vscode.workspace.getWorkspaceFolder(document.uri);
    // Untitled documents have no folder: a relative path passes
    // through unresolved and surfaces as not-found.
    if (folder) return path.join(folder.uri.fsPath, configured);
  }
  return configured;
}

/* The buffer's convention wins over the writer's LF so a CRLF file
   does not come back mixed — the guarded conversion lives in
   formatter.js, pure and self-checked, because canonical output can
   carry raw newlines inside regex literals. */
function reportFailure(result, output, document) {
  if (result.kind === "not-found") {
    if (!notFoundAnnounced) {
      notFoundAnnounced = true;
      vscode.window.showErrorMessage(
        "surform not found — install it from a repository clone (cargo install --path .) or set the surform.path setting. Formatting stays a no-op until then.",
      );
    }
    return;
  }
  output.clear();
  output.appendLine(`${document.fileName}:`);
  if (result.kind === "diagnostics") {
    for (const diagnostic of result.diagnostics) {
      output.appendLine(`  ${describe(diagnostic)}`);
    }
  } else {
    output.appendLine(`  ${result.message}`);
  }
  output.show(true);
  const detail =
    result.kind === "diagnostics"
      ? describe(result.diagnostics[0]) || "the file has diagnostics — see the Surform output"
      : clipped(result.message);
  vscode.window.showErrorMessage(`surform fmt: ${detail}`);
}

function describe(diagnostic) {
  if (!diagnostic) return "";
  return clipped(
    `${diagnostic.code ? `${diagnostic.code}: ` : ""}${diagnostic.message}`,
  );
}

function clipped(text) {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > 200 ? `${flat.slice(0, 199)}…` : flat;
}

module.exports = { activate };
