# DEFINE FUNCTION

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_function`),
AST/emitter (`sql/statements/define/function.rs`), body parser
(`syn/parser/object.rs::parse_block`). Cross-checked against a local
in-memory 3.2.3 server (2026-09-18); the floor move re-runs the corpus on the 3.3.0 embed (2026-09-25).

**Surface (the one two-block statement):**

- The keyword is **`fn`**; the name is the path *after* `fn::`
  (`fn util::slugify(…)` emits `DEFINE FUNCTION fn::util::slugify(…)`).
- **Params keep their `$`** — SurrealQL requires it (verified:
  `expected a parameter`); the SurrealQL AST stores names without the
  `$` and re-adds it at display — the SDL AST keeps the source form.
  Zero-param functions (`fn f() { … };`) are legal.
- **The `->` return arrow** is SurrealQL's own operator, kept verbatim;
  optional. Types pass through verbatim.
- **Positional two-block form:** the first `{ … }` after the signature
  is the SurrealQL body, captured **brace-balanced** with the global
  scanner discipline (strings, all four SurrealQL comment syntaxes, and
  quoted identifiers skipped — see README rule 3), ending at the
  matching `}`. SurrealQL block semantics apply: the last statement,
  sans trailing `;`, is the return value; an empty body is legal. The
  body is re-printed from the embedded parser's AST at codegen
  (single-statement bodies print without a trailing `;`;
  multi-statement bodies with one after every statement — INFO returns
  that shape, delivered single-line). The optional second `{ … }`
  holds ordinary SDL clauses. Param lists reject trailing commas
  (SurrealQL accepts them; SDL is deliberately stricter since INFO
  never emits one).
- **Optional params are trailing `option<T>`** — a call-time
  convention; SurrealQL does not enforce positionality (neither does
  SDL — parity).

```rust
struct Function {
  name: FnPath,                        // :: segments allowed; fn:: at emit
  args: Vec<(Param, Kind)>,            // $ kept verbatim in SDL
  returns: Option<Kind>,
  body: Expr,                          // required SurrealQL block
  permissions: Permission,             // single-value; default full
  graphql: Option<Graphql>,            // alias?, deprecated?
  comment: Option<DocComment>,         // from ///
}
```

```
/// <doc comment>                        // optional — emitted as COMMENT
fn <name>($<param>: <type>, …) [-> <type>] {
  <surrealql statements>              // body — last statement sans
                                       //   trailing `;` is the result
} {
  // optional:
  permissions: none | full | <expr>;    // default full
  graphql {                             // both clauses independently optional
    alias: <string>;                    // GRAPHQL_ALIAS
    deprecated: <string>;               // GRAPHQL_DEPRECATED
  };
};
```

Emit: `DEFINE FUNCTION fn::<name>([$<param>: <type>, …]) [-> <type>]
{ <body> } [PERMISSIONS …] [GRAPHQL_ALIAS …] [GRAPHQL_DEPRECATED …]
[COMMENT …];` (`OVERWRITE` implied per the codegen model). SDL's
trailing-clause order is its canonical choice; the server's ToSql
emits body → COMMENT → GRAPHQL_* → PERMISSIONS — both parse;
the differ compares resolved state (kinds under the algebra, expressions over canonical text) (a stored body compares byte-wise: the canon and the engine
print agree).

## Reverse parser (INFO → SDL)

INFO emits the plain `DEFINE` with `PERMISSIONS` **always** rendered
(default FULL — elide at default) and param names **without `$`** in
the AST (the display re-adds `$` — SDL keeps `$`). Single-statement
bodies print without a trailing `;`, multi-statement bodies with one
after every statement (both re-parse identically). The
`PERMISSIONS`-expression and `COMMENT` slots print wrapped in `( … )`
when statement-like (AST-transparent). Functions are listed
by `INFO FOR DB` (the `functions` map); name segments arrive escaped
per the general identifier rule (strip backticks). `COMMENT '…'` maps
back to `///`.

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): param without `$` → parse
error ✅; return type + `PERMISSIONS WHERE` ✅; optional trailing
`option<T>` param callable with the argument omitted ✅
(`fn::last_opt(5)` → `5`); multi-segment `fn::util::slugify` +
`GRAPHQL_ALIAS` ✅; zero-param form ✅; round-trip shows the body
without a trailing `;` and default `PERMISSIONS FULL` materializing ✅.

## Examples (one per clause)

signature + return + body + doc comment + options block:

```sdl
/// Whether the current user may edit this post
fn can_edit($p: record<post>) -> bool {
  return $auth.id = $p.author or $auth.role = 'admin';
} {
  permissions: $auth.admin = true;
};
```

```surql
DEFINE FUNCTION fn::can_edit($p: record<post>) -> bool {
  RETURN $auth.id = $p.author OR $auth.role = 'admin'
} PERMISSIONS WHERE $auth.admin = true
  COMMENT 'Whether the current user may edit this post';
```

body only — no options block, no return type; multi-statement body
whose last expression (sans `;`) is the result:

```sdl
fn tax_total($net: number, $rate: option<number>) {
  let $r = $rate ?? 0.15;
  let $tax = math::round($net * $r, 2);
  $net + $tax
};
```

```surql
DEFINE FUNCTION fn::tax_total($net: number, $rate: option<number>) {
  LET $r = $rate ?? 0.15;
  LET $tax = math::round($net * $r, 2);
  $net + $tax;
};
```

permissions none:

```sdl
fn purge_logs($before: datetime) {
  delete log where at < $before;
} {
  permissions: none;
};
```

zero params:

```sdl
fn ping() -> string {
  'pong'
};
```

optional trailing parameter (callable as `fn::last_opt(5)`):

```sdl
fn last_opt($req: number, $opt: option<number>) {
  $opt ?? $req;
};
```

multi-segment name + graphql:

```sdl
fn util::slugify($s: string) -> string {
  string::lowercase(string::replace($s, ' ', '-'))
} {
  graphql {
    alias: 'slugify';
  };
};
```

```surql
DEFINE FUNCTION fn::util::slugify($s: string) -> string {
  string::lowercase(string::replace($s, ' ', '-'))
} GRAPHQL_ALIAS 'slugify';
```
