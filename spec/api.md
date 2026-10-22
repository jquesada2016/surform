# DEFINE API

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_api`,
`parse_api_config`), AST (`sql/statements/define/api.rs`), and emitters
(`ApiAction`/`ApiConfig::fmt_sql`, which define INFO output). 
Cross-checked against a local in-memory 3.2.3 server (2026-09-18); the floor-move corpus re-runs on the 3.3.0 embed (2026-09-25).

The "name" is a **route path**: SDL restricts it to a single string
literal. The path grammar is SurrealDB's own (`api/path.rs`): static
segments literal, `:name` matches one segment, `:name<type>` a typed
segment (SurrealQL type grammar), `*name` matches the rest and must
terminate the path; `\:`/`\*` escape literals. SDL passes the literal
through and SurrealDB validates at define time. The block contains
zero or more method-group structural sub-blocks (a fallback-only
`api '/x' { any { then: …; }; };` is legal) plus at most one `any`
block; there is no ADT discriminant.

```rust
struct Api {
  path: StringLit,                   // SDL: string literal only
  actions: Vec<ApiAction>,           // one per method-group sub-block
  any: Option<AnyBlock>,             // endpoint-wide config + fallback
  comment: Option<DocComment>,       // from ///
}
struct ApiAction { methods: Vec<Method>, action: Expr, config: ApiConfig }
struct AnyBlock { config: ApiConfig, fallback: Option<Expr> }
struct ApiConfig { middleware: Vec<Middleware>, permissions: Permission }
struct Middleware { name: FnPath, args: Vec<Expr> }   // global <fn-path>
                                                      // rule + (args);
                                                      // args evaluated at
                                                      //   define time
```

```
/// <doc comment>                        // optional — emitted as COMMENT
api <path-string> {
  // action groups — zero or more, any order; each method may appear in
  // only ONE group (SDL validates eagerly, including duplicates within
  // one head list; SurrealDB also errors at define time, verified):
  <method>, … {                          // get | post | put | patch |
                                         //   delete | trace — comma list,
                                         //   non-empty, no trailing comma
    // optional (clause order within a group is free — SurrealQL parity):
    permissions: none | full | <expr>;
    middleware: <call>, …;               // SDL-lexed comma list of
                                         //   name(args) calls — parens
                                         //   mandatory; the list is split
                                         //   on depth-0 commas (bracket-
                                         //   and string-aware, so commas
                                         //   inside args don't split);
                                         //   args are SurrealQL
                                         //   expressions evaluated once
                                         //   at define time
    // required:
    then: <expr>;                        // response object: status /
  };                                     //   body / headers / context

  // endpoint-wide config + fallback — at most ONE (an `any` block; a
  // second is a parse error, not a fold):
  any {
    permissions: none | full | <expr>;
    middleware: <call>, …;
    then: <expr>;                        // optional — the fallback response
  };
};
```

Notes:

- `then:` is an expression clause; a bare statement is legal, but the
  conventional form is a single braced object — `then: { { status: 200,
    body: { … } } };` — the outer braces are SurrealQL's block, the
    inner the response object literal. Bare object literals also parse;
    the conventional double-brace is what examples use. Note: the
    group `THEN` action slot prints **unwrapped** by the emitter (no
    parenthesization of bare statements); the `any` fallback slot
    stays wrapped like other statement-like values.
- Within a group, codegen emits `MIDDLEWARE …` before `PERMISSIONS …`
  (the emitters' canonical order), then `THEN …`. The `any` block may
  appear anywhere among the groups (SurrealQL parity); canonical SDL
  places it first, matching the emitter.
- Middleware names follow the global `<fn-path>` rule (bare or
  `fn::`/`mod::`-prefixed `::`-paths; `silo::` package refs and `ml::`
  model refs are licensed SDL exclusions — `raw` covers them).
  Middleware lists reject trailing commas, like method lists (trailing
  commas *inside* an argument list are SurrealQL-legal and pass
  through). Codegen uppercases keywords; method names render lowercase
  (`FOR get`) — and SDL's method heads are lowercase only: an
  uppercase head is SDL-E1035 (with the lowercase did-you-mean), the
  parser case-folds nothing.
- `$request` is available inside `then:` bodies with `body` /
  `headers` / `params` / `method` / `query` / `context` /
  `request_id`.
- Emit: `DEFINE API <path> [FOR ANY <config> [THEN <fallback>]]
  [FOR <methods> <config> THEN <action>]… [COMMENT …];` (`OVERWRITE`
  implied per the codegen model; an empty `any { }` emits `FOR ANY`
  and round-trips to an absent `any` — a no-op the linter flags,
  SDL-L0009).

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): basic action ✅; wildcard path +
`ANY` fallback + chained middleware + method groups ✅; per-action
`PERMISSIONS NONE` ✅ and `PERMISSIONS WHERE <expr>` ✅; duplicate method
across groups → define-time error ✅; `then:` required on method groups
✅ (`expected THEN`), optional on `any`.

## Reverse parser (INFO → SDL)

Verified from the emitters — two always-materialized shapes:

- INFO **always** emits a `FOR ANY <config>` clause (even when SDL had
  no `any` block) and **always** emits `PERMISSIONS …` on every `FOR`
  clause (default `FULL`). Reverse rule: a `FOR ANY` maps to an absent
  `any` block exactly when it carries no `THEN`, no middleware, and
  `PERMISSIONS FULL`; a group's `PERMISSIONS FULL` elides (canonical
  minimal form).
- Hand-authored DDL may fold multiple `FOR ANY` clauses config-last-wins
  with an earlier THEN surviving — SDL rejects that input as
  ambiguous (single `any` rule) and routes it to `raw`. The fold
  happens INSIDE the embedded parser, so the raw-route detector must
  run on the raw text before canonicalization (live-probed: define
  time folds too — the catalog never stores two `FOR any` clauses,
  and a folded shape is indistinguishable from an authored one).
- `then` bodies arrive re-printed from the AST; quote style normalizes
  to single quotes everywhere (SurrealDB's printer emits single quotes
  unless the string contains one). INFO prints `FOR any` and method
  lists lowercase; canonical SDL is lowercase, and SDL accepts only
  the lowercase spelling.
- **Path normalization:** INFO re-prints the *parsed* path AST, not the
  authored string — escapes vanish (`'\:x'` re-emits as `':x'`, which
  re-parses as a *dynamic* segment — a silent semantic flip),
  consecutive slashes collapse, segments re-join canonically with `/`.
  (Live-probed on the embedded 3.2.4 engine: the escape is storable
  only at the START of a segment — SDL `'/\\:x'`, value `/\:x` — and
  the stored `:x` segment prints as the flip `'/:x'`.) The reader
  only ever sees the post-flip printed text — an authored flipped
  path and a normally-authored dynamic segment are textually
  identical at read time, so flip detection is impossible there
  (decided 2026-09-23): the reader carries the printed path
  verbatim, and the DIFFER owns detection (re-parse both sides'
  paths and compare parse trees).
- INFO always emits the `FOR any` clause **first** regardless of input
  order; middleware name segments arrive backtick-escaped when they
  contain non-word characters or leading digits (strip backticks on
  parse).

## Examples (one per clause)

minimal action + doc comment:

```sdl
/// Health check
api '/health' {
  get {
    then: { { status: 200, body: { ok: true } } };
  };
};
```

```surql
DEFINE API '/health' FOR get THEN { { status: 200, body: { ok: true } } }
  COMMENT 'Health check';
```

method group + `$request`:

```sdl
api '/echo' {
  post, put {
    then: {
      {
        status: 200,
        body: { you_sent: $request.body, method: $request.method }
      }
    };
  };
};
```

dynamic path parameter:

```sdl
api '/users/:id' {
  get {
    then: { { status: 200, body: (type::record('user', $request.params.id)) } };
  };
};
```

permissions (each form) — none, full, expression:

```sdl
api '/mixed' {
  get {
    permissions: full;
    then: { { status: 200 } };
  };
  delete {
    permissions: none;
    then: { { status: 204 } };
  };
  trace {
    permissions: $auth.admin = true;
    then: { { status: 200 } };
  };
};
```

middleware:

```sdl
api '/slow' {
  get {
    middleware: api::timeout(1s);
    then: { { status: 200 } };
  };
};
```

wildcard + `any` fallback + endpoint-wide middleware + per-group
permissions:

```sdl
api '/admin/*rest' {
  any {
    middleware: api::timeout(5s);
    then: { { status: 404, body: { error: 'no such route' } } };
  };
  get, delete {
    permissions: $auth.admin = true;
    then: { { status: 200 } };
  };
};
```

```surql
DEFINE API '/admin/*rest'
  FOR ANY MIDDLEWARE api::timeout(5s) THEN { { status: 404, body: { error: 'no such route' } } }
  FOR get, delete PERMISSIONS WHERE $auth.admin = true THEN { { status: 200 } };
```
