/* The region scanner's self-checks: `node
   surrealql-regions.selfcheck.mjs` — plain Node, no dependencies (the
   editors/vscode/selfcheck.mjs precedent). Kept OUT of the module
   itself so the browser bundle never sees a node:fs import. Three
   layers: the design-carrying shapes (the verbatim bodies, the
   expression terminals, the capture skip set), malformed-input
   totality, and the shipped docs/examples corpus. */

import { surrealqlRegions } from "./surrealql-regions.js";
import { readFileSync, readdirSync } from "node:fs";

const here = new URL(".", import.meta.url);
const regionsOf = (s) => surrealqlRegions(s).map(([a, b]) => s.slice(a, b));

let failures = 0;
const check = (label, ok) => {
  if (!ok) {
    failures += 1;
    console.error(`FAIL ${label}`);
  } else {
    console.log(`ok   ${label}`);
  }
};

check(
  "expression clause: simple",
  JSON.stringify(regionsOf("field email: string { assert: $value != none; };")) ===
    JSON.stringify([" $value != none;"]),
);
check(
  "field headers never fire keys: a field named `value`",
  JSON.stringify(regionsOf("field value: string;")) === JSON.stringify([]),
);
check(
  "object-schema members never fire keys",
  JSON.stringify(regionsOf("field meta: { value: string, default: 1 };")) ===
    JSON.stringify([]),
);
check(
  "two-word key: on delete",
  JSON.stringify(
    regionsOf("field owner: record { reference { on delete: cascade; } };"),
  ) === JSON.stringify([" cascade;"]),
);
check(
  "assignment short form: param",
  JSON.stringify(regionsOf("param $limit = 50;")) === JSON.stringify([" 50;"]),
);
check(
  "assignment value with a string: the start must not drift",
  JSON.stringify(regionsOf("param $msg = 'hello';")) === JSON.stringify([" 'hello';"]),
);
check(
  "object-literal values capture through their closing brace and ';'",
  JSON.stringify(regionsOf("param $obj = { a: 'x' };")) ===
    JSON.stringify([" { a: 'x' };"]),
);
check(
  "flagship: authenticate's object literal captures fully",
  JSON.stringify(
    regionsOf(
      "access j ON all type record { authenticate: { if $auth.blocked { throw 'no'; } }; };",
    ),
  ) === JSON.stringify([" { if $auth.blocked { throw 'no'; } };"]),
);
check(
  "a table NAMED view is not a view query",
  JSON.stringify(regionsOf("table view { schemafull; };")) === JSON.stringify([]),
);
check(
  "raw body: brace-balanced, closing brace stays SDL",
  JSON.stringify(regionsOf("raw { DEFINE TABLE x TYPE NORMAL; };")) ===
    JSON.stringify([" DEFINE TABLE x TYPE NORMAL; "]),
);
check(
  "fn body: the first block only",
  regionsOf("fn bump($x: int) { RETURN $x + 1; } { permissions: full; };").length ===
    2,
);
check(
  "view query: ';'-terminated inside the block",
  JSON.stringify(regionsOf("table view stats { SELECT count() FROM click; };")) ===
    JSON.stringify([" SELECT count() FROM click;"]),
);
check(
  "';' inside a string does not terminate",
  regionsOf("field e: string { assert: string::is_email($value); };").length === 1,
);
const semi = regionsOf("assert: 'a;b';");
check(
  "';' inside a string literal is skipped",
  semi.length === 1 && semi[0] === " 'a;b';",
);
check(
  "comment syntaxes are skipped inside captures",
  regionsOf("assert: $x -- semi; gone\n  + 1;").length === 1 &&
    regionsOf("assert: /* ; */ $x;").length === 1,
);
check("quoted idents: ⟨a;b⟩ inside a capture", regionsOf("assert: ⟨a;b⟩.x;").length === 1);
check(
  "backtick idents: ';'-bearing ident inside a capture",
  JSON.stringify(regionsOf("field x: string { assert: `a;b` != $v; };")) ===
    JSON.stringify([" `a;b` != $v;"]),
);
check(
  "backtick-quoted view name",
  JSON.stringify(regionsOf("table view `name` { SELECT 1; };")) ===
    JSON.stringify([" SELECT 1;"]),
);
check(
  "angle-quoted view name",
  JSON.stringify(regionsOf("table view ⟨name⟩ { SELECT 1; };")) ===
    JSON.stringify([" SELECT 1;"]),
);
check(
  "comma-grouped ops",
  JSON.stringify(regionsOf("permissions { select, create: full; };")).includes(" full;"),
);
check(
  "config default's keys",
  regionsOf("config default { database: db('x'); };").length === 1,
);
check(
  "malformed: a stray '}' ends a capture without running away",
  JSON.stringify(regionsOf("assert: $x } more;")) === JSON.stringify([" $x "]),
);
check(
  "totality: garbage in, no exception",
  surrealqlRegions('::: {{{ "unterminated').every(([a, b]) => a <= b),
);

/* corpus: every shipped example scans without throwing. */
let corpusRegions = 0;
for (const file of readdirSync(new URL("../../../docs/examples/", here)).filter((f) =>
  f.endsWith(".surdl"),
)) {
  const text = readFileSync(new URL(`../../../docs/examples/${file}`, here), "utf8");
  corpusRegions += surrealqlRegions(text).length;
}
check(`corpus scanned (${corpusRegions} regions)`, corpusRegions > 0);

if (failures > 0) {
  console.error(`\n${failures} failure(s)`);
  process.exit(1);
}
console.log("\nall region checks passed");
