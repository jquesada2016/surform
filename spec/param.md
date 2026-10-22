# DEFINE PARAM

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_param`),
executor (`expr/statements/define/param.rs`), emitter
(`sql/statements/define/param.rs`). Cross-checked against a local
in-memory 3.2.3 server (2026-09-18); the floor move re-runs the corpus on the 3.3.0 embed (2026-09-25).

**Key semantic (verified):** the value expression is **evaluated once
at define time** and the *result* is stored — `param $x = { time::now()
};` round-trips as the frozen timestamp (`VALUE d'2026-09-18T…'`).
Params are define-time constants, not lazy computations. The
normalizer therefore replaces the SDL expression with the returned
literal on retro-apply.

The name keeps its `$` (SurrealQL-faithful). There is **no type
clause** (the parser has `VALUE`/`COMMENT`/`PERMISSIONS` only); if a
future SurrealQL adds one, it slots between the name and `=`.

**Two surface forms:**

- **short form** — `param $name = <expr>;` — value only, the common
  case. The `=` capture follows the same depth-0-`;` scan as the `:`
  capture (assignment short form, README notation).
- **block form** — `param $name { … };` — required when any other
  clause is present. A trailing block after the short form is rejected
  (a depth-0 `{` following an expression is an object literal under
  the capture rule — `param $x = { a: 1 };` is the object-valued short
  form, not a block opener).

`value:` itself is optional (SurrealQL parity — a clause-less param
parses and stores `VALUE NONE`); SDL accepts it, the linter flags it
as almost certainly unintended (SDL-L0008).

```rust
struct Param {
  name: Param,                          // $-prefixed, verbatim
  value: Option<Expr>,                  // evaluated once at define time
  permissions: Permission,              // single-value; default full
  comment: Option<DocComment>,          // from ///
}
```

```
/// <doc comment>                        // optional — emitted as COMMENT
param $<name> = <expr>;                 // short form

/// <doc comment>
param $<name> {                         // block form
  // optional (a param without it stores VALUE NONE — linted):
  value: <expr>;
  // optional:
  permissions: none | full | <expr>;    // default full; SurrealQL also
};                                      //   requires the expression be
                                        //   read-only (server-enforced)
```

Emit (canonical order; `OVERWRITE` implied): `DEFINE PARAM $<name>
VALUE <expr> [COMMENT …] [PERMISSIONS …];` — note the server's own
order is VALUE → COMMENT → PERMISSIONS with `PERMISSIONS` rendered
unconditionally (default FULL).

## Reverse parser (INFO → SDL)

INFO emits `VALUE` **always** (a valueless param renders `VALUE NONE`)
and `PERMISSIONS` always (default FULL). Canonical minimal form:
`VALUE NONE` maps to the clause-less/bare param (linted); `PERMISSIONS
FULL` elides; the returned value literal replaces the SDL expression
(define-time evaluation). Form selection: value present + everything
else default → short form; otherwise block. Params are listed by
`INFO FOR DB` (the `params` map).

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): literal value + `PERMISSIONS
NONE` ✅; expression value — stored evaluated ✅; bare param parses,
round-trips as `VALUE NONE` ✅; default permissions materialize `FULL`
✅.

## Examples (one per clause)

literal value + doc comment (short form):

```sdl
/// Default page size for list endpoints
param $default_limit = 50;
```

```surql
DEFINE PARAM $default_limit VALUE 50 COMMENT 'Default page size for list endpoints';
```

computed value — frozen at define time (the caveat, not a feature
demo):

```sdl
param $deployed_at = { time::now() };
```

```surql
DEFINE PARAM $deployed_at VALUE { time::now() };
```

object-valued short form (the `{` after `=` is the object literal, not
a block):

```sdl
param $endpoints = { read: '/api/read', write: '/api/write' };
```

permissions (block form — the clause that forces it):

```sdl
param $internal_seed {
  value: 42;
  permissions: none;
};
```

```surql
DEFINE PARAM $internal_seed VALUE 42 PERMISSIONS NONE;
```
