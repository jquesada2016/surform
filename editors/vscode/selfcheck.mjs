/* The grammar's self-checks: `node selfcheck.mjs` — plain Node, no
   dependencies (the docs/editor/positions.js precedent for pure-module
   checks). Three layers, cheapest first:
     1. the tmLanguage JSON parses and has the expected shape;
     2. EVERY match/begin/end pattern compiles — the grammar is
        authored in a small checked subset of Oniguruma: everything is
        plain RegExp except the leading `(?i)` case-insensitivity
        group, which this check translates to the JS `i` flag (the
        translation is the documented contract, not an accident);
     3. spot matches on the rules that carry the language's design
        (their BEGIN patterns; the depth-0 terminator behavior itself
        needs a real TM engine and is out of scope for a line-walking
        smoke). */

import { readFileSync, readdirSync } from "node:fs";

const here = new URL(".", import.meta.url);
const grammar = JSON.parse(
  readFileSync(new URL("syntaxes/surform.tmLanguage.json", here), "utf8"),
);

let failures = 0;
function check(label, ok) {
  if (!ok) {
    failures += 1;
    console.error(`FAIL ${label}`);
  } else {
    console.log(`ok   ${label}`);
  }
}

/* TM dialect → JS: `(?i)` becomes the `i` flag. */
function toJs(source, extra = "") {
  let flags = extra;
  const body = source.replace(/^\(\?i\)/, () => {
    flags += "i";
    return "";
  });
  return new RegExp(body, flags);
}

/* 1 — shape */
check("scopeName is source.surform", grammar.scopeName === "source.surform");
check(
  "official grammar delegation present",
  JSON.stringify(grammar).includes('"source.surrealql"'),
);
const manifest = JSON.parse(readFileSync(new URL("package.json", here), "utf8"));
check(
  "manifest packs the official SurrealQL extension",
  (manifest.extensionPack ?? []).includes("surrealdb.surrealql"),
);
check(
  "no hard extensionDependencies (the fallback design must never block activation)",
  !("extensionDependencies" in manifest),
);
check(
  "manifest activates for the language and owns the formatter entrypoint",
  manifest.main === "./extension.js" &&
    (manifest.activationEvents ?? []).includes("onLanguage:surform"),
);
check(
  "the surform.path setting is declared",
  manifest.contributes?.configuration?.properties?.["surform.path"]?.type ===
    "string",
);

/* 2 — every pattern compiles */
const patterns = [];
function walk(node, path) {
  if (Array.isArray(node)) {
    node.forEach((child, i) => walk(child, `${path}[${i}]`));
    return;
  }
  if (node && typeof node === "object") {
    for (const key of ["match", "begin", "end"]) {
      if (typeof node[key] === "string") {
        patterns.push([`${path}.${key}`, node[key]]);
      }
    }
    for (const [key, value] of Object.entries(node)) {
      if (key !== "patterns" && typeof value === "object") {
        walk(value, `${path}.${key}`);
      }
    }
    if (Array.isArray(node.patterns)) walk(node.patterns, `${path}.patterns`);
  }
}
walk(grammar.patterns, "top");
for (const [name, source] of Object.entries(grammar.repository ?? {})) {
  walk(source, `#${name}`);
}
for (const [where, source] of patterns) {
  try {
    toJs(source);
  } catch (err) {
    failures += 1;
    console.error(`FAIL compile ${where}: ${source} — ${err.message}`);
  }
}
check(`all ${patterns.length} patterns compile`, failures === 0);

/* 3 — spot matches (repository rules by name; non-global — a global
   regex's lastIndex would leak between test() calls) */
const rule = (name) => grammar.repository[name];

const heads = toJs(rule("statement-heads").match, "m");
check("head: `table person {` matches", heads.test("table person {"));
check("head: indented nested field matches", heads.test("  field name: string;"));
check("head: a clause VALUE is not a head", !heads.test("  on: namespace;"));
check("head: `mixin` is a statement head", heads.test("mixin stamp {"));
// A bare field named `mixin:` inks its keyword like every
// statement-word-named field does (`table:` behaves the same) — the
// heads rule never excluded those.

const includeStmt = toJs(rule("include-statement").match, "m");
check("include: the statement head matches at column 0", includeStmt.test(`include "tables/users.surdl";`));
check("include: case-folded head matches", includeStmt.test("INCLUDE 'a.surdl';"));
check("include: indented `include original;` is NOT the head (the changefeed flag)", !includeStmt.test("  include original;"));

const typeAlias = toJs(rule("type-alias-statement").match, "m");
check("type alias: the statement head matches", typeAlias.test("type Email = string;"));
check("type alias: indented head matches", typeAlias.test("  type Emails = array<Email>;"));
check("type alias: a lowercase name is NOT the head (the alias/kind split)", !typeAlias.test("type email = string;"));
check("type alias: an indented `type:` clause line is NOT the head (the access/index clause key)", !typeAlias.test("  type: jwt;"));
check("type alias: case-folded head matches", typeAlias.test("TYPE Email = string;"));

const aliasUse = toJs(rule("alias-use").match);
check("alias use: a Capitalized kind word inks", aliasUse.test("Email"));
check("alias use: a lowercase kind word is not the use rule", !aliasUse.test("string"));

const spread = toJs(rule("spread-entry").match, "m");
check("spread: `...name;` entry matches", spread.test("  ...stamp;"));
check("spread: quoted name matches", spread.test("...`my mix`;"));
check("spread: line-anchored — mid-line dots never fire", !spread.test("  x ...y;"));
check("spread: a bare dotted head is not a spread", !spread.test("  meta.a: int;"));
// A mixin body is a table block's body minus `on:` clauses (spec/mixin.md): its
// indented index/event member heads take head ink through the same
// line-anchored rule table bodies use — nothing new to ink, pinned
// here so the member model stays covered.
check("head: mixin-body `index` member matches", heads.test("  index slug_u {"));
check("head: mixin-body `event` member matches", heads.test("  event touched {"));

const exprBegin = toJs(rule("expression-clauses").patterns[0].begin, "m");
check("expression clause: `assert:` opens", exprBegin.test("assert: $before.points >= 0;"));
check("expression clause: opens mid-line in the canonical one-liner", exprBegin.test("field email: string { assert: $value != none; };"));
check("expression clause: multi-word key `default always:` opens", exprBegin.test("default always: time::now();"));
check("expression clause: comma-grouped ops open", exprBegin.test("select, create: full;"));
check("expression clause: config default's keys open", exprBegin.test("database: $db;"));
check("expression clause: a bare flag is not a clause value", !exprBegin.test("schemafull;"));

const permBegin = toJs(rule("permissions-clause").begin, "m");
check("permissions single-value form opens", permBegin.test("permissions: full;"));
check("permissions block form is not the value form", !permBegin.test("permissions {"));

const stringAt = (i, sample) => toJs(rule("strings").patterns[i].match).test(sample);
check(`prefixed string: r"…" matches`, stringAt(1, `r"a;b"`));
check(`format string: f'…' matches`, stringAt(0, `f"{a} name"`));
check(`plain string: escapes honored`, stringAt(2, `"a\\"b"`));

const identAt = (i, sample) => toJs(rule("quoted-idents").patterns[i].match).test(sample);
check("backticked ident matches", identAt(0, "`we ird`"));
check("angle ident matches", identAt(1, "⟨a;b⟩"));

const fnBegin = toJs(rule("fn-statement").begin, "m");
check("fn signature opens (single-segment name)", fnBegin.test("fn bump($x: int) {"));
check("fn signature opens (multi-segment name, return type)", fnBegin.test("fn util::slugify($s: string) -> string {"));
const viewBegin = toJs(rule("view-statement").begin, "m");
check("view header opens", viewBegin.test("table view stats {"));
check("raw escape hatch opens", toJs(rule("raw-statement").begin, "m").test("raw {"));
const paramBegin = toJs(rule("param-statement").begin, "m");
check("param assignment opens", paramBegin.test("param $limit = 50;"));
check("param block form opens", paramBegin.test("param $x {"));
const assignBegin = toJs(rule("assignment-expr").begin);
check("assignment short form consumes the `=`", assignBegin.test("= time::now();"));

const fieldBegin = toJs(rule("field-header").begin, "m");
check("field header with dotted path opens", fieldBegin.test("field settings.theme: option<string> {"));
const objectField = toJs(rule("field-object-type").begin, "m");
check("object-schema field type opens", objectField.test("field meta: { value: string, default: 1 };"));
check("plain field header is not the object-type rule", !toJs(rule("field-header").begin, "m").source.includes("(\\{)"));

const api = toJs(rule("api-heads").match, "m");
check("api method head matches", api.test("get, post {"));
check("api `any` block matches", api.test("any {"));
check("api `delete:` value form is not a head", !api.test("delete: $after;"));

const keywordAt = (keys, sample) =>
  rule("keyword-clauses").patterns.some(
    (p) => p.begin && toJs(p.begin, "m").test(sample) && new RegExp(keys).test(p.begin),
  );
check("algorithm key rule exists and opens", keywordAt("algorithm", "algorithm: hs256;"));
check("access subject `for:` rule exists and opens", keywordAt("\\bfor\\b|\\(for\\)", "for: user;"));
check("graphql limits open as number clauses", keywordAt("complexity|depth", "complexity: 10;"));

/* 4 — corpus smoke: every real example file the site ships (the same
   bytes the compiler verifies at docgen build time) must run through
   the grammar's patterns without an exception, head-match at least
   once, and — across the corpus — the expression clauses the docs
   actually use must open. This is a smoke, not a tokenizer: a real
   TM engine carries begin/end state per position, this walks lines. */
const examplesDir = new URL("../../docs/examples/", here);
let headFiles = 0;
let exprOpens = 0;
const topRules = grammar.patterns
  .map((p) => p.include?.slice(1))
  .filter(Boolean)
  .map((name) => ({ name, rules: collect(name) }));
function collect(name, seen = new Set()) {
  if (seen.has(name)) return [];
  seen.add(name);
  const entry = rule(name);
  const out = [];
  if (entry?.match) out.push(toJs(entry.match, "m"));
  if (entry?.begin) out.push(toJs(entry.begin, "m"));
  for (const sub of entry?.patterns ?? []) {
    if (typeof sub.include === "string") out.push(...collect(sub.include.slice(1), seen));
    else if (sub.match) out.push(toJs(sub.match, "m"));
    else if (sub.begin) out.push(toJs(sub.begin, "m"));
  }
  return out;
}
const corpus = readdirSync(examplesDir).filter((f) => f.endsWith(".surdl"));
for (const file of corpus) {
  const text = readFileSync(new URL(file, examplesDir), "utf8");
  let matchedHead = false;
  for (const line of text.split("\n")) {
    try {
      for (const { name, rules } of topRules) {
        for (const re of rules) {
          if (re.test(line)) {
            if (name === "statement-heads") matchedHead = true;
            if (name === "expression-clauses") exprOpens += 1;
          }
          re.lastIndex = 0;
        }
      }
    } catch (err) {
      failures += 1;
      console.error(`FAIL corpus ${file}: ${err.message}`);
    }
  }
  if (matchedHead) headFiles += 1;
  else console.error(`FAIL corpus ${file}: no statement head matched`);
}
check(`corpus: every example file matched a statement head (${headFiles} files)`, headFiles > 0);
check(`corpus: expression clauses fire across the corpus (${exprOpens} opens)`, exprOpens > 0);

if (failures > 0) {
  console.error(`\n${failures} failure(s)`);
  process.exit(1);
}
console.log(`\nall checks passed (${patterns.length} patterns compiled)`);
