/* Builds the playground's committed artifacts into docs/assets/editor/
   (the repo has no CI, so built outputs are committed):

     1. cargo build -p surform-editor --target wasm32-unknown-unknown --release
     2. wasm-bindgen --target web   (the CLI must match the crate's
        exact =0.2.128 pin)
     3. wasm-opt -Oz                (binaryen)
     4. content-hash the wasm and the bundle, patch the glue's wasm
        URL, write bundle.txt for docgen
     5. esbuild                     (this app + the glue, one bundle)

   The hashed names are the cache story: a changed compiler or app
   produces a changed hash, so browsers fetch fresh bytes instead of
   trusting heuristics — and THE WASM EMBEDS THE COMPILER, so this
   rebuild is also the only thing that carries a compiler change (a
   lint flip, an emission fix) into the site's interactive examples.
   Rebuild and commit whenever the binding, the compiler, this app,
   or anything wasm-size-relevant changes. docs/deploy.sh runs it on
   every deploy; docgen fails the build when the artifacts are
   missing. */

import { execSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readdirSync, readFileSync, renameSync, rmSync,
         statSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "../..");
const outDir = path.join(repo, "docs/assets/editor");
const cargoWasm = path.join(
  repo,
  "target/wasm32-unknown-unknown/release/surform_editor.wasm",
);
const glue = path.join(outDir, "surform_editor.js");
const wasm = path.join(outDir, "surform_editor_bg.wasm");

const step = (label, cmd) => {
  console.log(`\n== ${label}`);
  execSync(cmd, { stdio: "inherit", cwd: repo });
};

const size = (p) => {
  const bytes = statSync(p).size;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
};

const hash8 = (p) =>
  createHash("sha256").update(readFileSync(p)).digest("hex").slice(0, 8);

step(
  "cargo (wasm32, release)",
  "cargo build -p surform-editor --target wasm32-unknown-unknown --release",
);

mkdirSync(outDir, { recursive: true });
step(
  "wasm-bindgen (web glue)",
  `wasm-bindgen ${cargoWasm} --out-dir ${outDir} --target web`,
);
// The TS declarations serve wasm consumers, not the site.
rmSync(path.join(outDir, "surform_editor.d.ts"), { force: true });
rmSync(path.join(outDir, "surform_editor_bg.wasm.d.ts"), { force: true });

step("wasm-opt (-Oz)", `wasm-opt -Oz ${wasm} -o ${wasm}`);

/* Content-hash the wasm and point the glue's default init URL at the
   hashed name — a changed compiler changes the hash, so browsers can
   never serve last season's lint/compile behavior. */
const wasmHash = hash8(wasm);
const wasmNamed = `surform_editor_bg.${wasmHash}.wasm`;
renameSync(wasm, path.join(outDir, wasmNamed));
writeFileSync(glue, readFileSync(glue, "utf8").replaceAll("surform_editor_bg.wasm", wasmNamed));

step(
  "esbuild (the app)",
  `npx --prefix ${here} esbuild ${here}/src/main.js --bundle --format=esm ` +
    `--minify --outfile=${path.join(outDir, "editor.js")}`,
);
const bundleHash = hash8(path.join(outDir, "editor.js"));
const bundleNamed = `editor.${bundleHash}.js`;
renameSync(path.join(outDir, "editor.js"), path.join(outDir, bundleNamed));

/* docgen reads this and stamps the hashed script tag into every page
   with interactive examples. */
writeFileSync(path.join(outDir, "bundle.txt"), `${bundleNamed}\n`);

/* Drop artifacts from earlier builds so the committed directory stays
   exactly the current trio + manifest. */
for (const name of readdirSync(outDir)) {
  if (name === "surform_editor.js" || name === wasmNamed || name === bundleNamed ||
      name === "bundle.txt") {
    continue;
  }
  if (/^editor\..*\.js$/.test(name) || /^surform_editor_bg\..*\.wasm$/.test(name)) {
    rmSync(path.join(outDir, name));
  }
}

console.log(`
== artifacts (committed)
   docs/assets/editor/${wasmNamed}  ${size(path.join(outDir, wasmNamed))}
   docs/assets/editor/${glue ? "surform_editor.js" : ""}  ${size(glue)}
   docs/assets/editor/${bundleNamed}  ${size(path.join(outDir, bundleNamed))}
`);
