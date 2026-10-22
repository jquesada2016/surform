# DEFINE CONFIG

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_config`,
`parse_default_config`, `parse_graphql_config`, `parse_api_config`) and
the catalog emitters (`catalog/schema/config.rs` — INFO output shape).
Cross-checked against a local in-memory 3.2.3 server (2026-09-18); the floor-move corpus re-runs on the 3.3.0 embed (2026-09-25).

Three facts you must know:

- **No `COMMENT` clause exists on DEFINE CONFIG** (parser-verified) —
  `///` doc comments are rejected on all three variants.
- **Configs are per-variant singletons** (verified: re-defining without
  `OVERWRITE` errors with `The config for <variant> already exists`,
  and the `default` slot is pre-seeded) — codegen always emits
  `DEFINE CONFIG OVERWRITE …`.
- **Unspecified GraphQL families default to NONE — expose nothing**
  (parser `unwrap_or_default()` → `TablesConfig::None`; verified from
  source). Only `introspection` defaults to auto. A bare `auto;`
  sets BOTH families to auto; a bare `none;` sets both to none.

`config` has no user-chosen name — the **variant is the header**
(exactly one is accepted, so it is flattened into the header rather
than nested as a sub-block). Scope: `config default` executes at ROOT
level; `config api` / `config graphql` at database level (namespace
level rejected); codegen places them accordingly relative to the
emitted `USE` statements.

```rust
enum Config {
  Api     { middleware?, permissions? },
  Graphql { tables: TablesCfg,         // auto | none | include(…) | exclude(…)
            functions: FnsCfg,         // resolved at parse: bare auto;/none;
            depth?, complexity?,       //   flags fold into both families
            introspection: auto|none },//   (last-one-wins per family)
  Default { namespace?, database? },   // ≥ one required; the unset side
}                                      //   emits NONE (SQL-AST display)
```

## Variant `config api { … }` — global api-endpoint defaults

```
config api {
  middleware: <call>, …;              // global <fn-path> + (args); the list
                                       //   splits on depth-0 commas; args are
                                       //   SurrealQL expressions evaluated
                                       //   once at define time (INFO returns
                                       //   them as computed values)
  permissions: none | full | <expr>;  // single-value; default full
};
```

## Variant `config graphql { … }`

```
config graphql {
  auto;                               // flag: tables AND functions → auto
  none;                               // flag: tables AND functions → none
  tables: auto | none;                // bare enums stay inline…
  tables { include: <table>, …; };    // …structured variant is a sub-block
  tables { exclude: <table>, …; };    //   holding exactly ONE of
  functions: auto | none;             //   include:/exclude: (both present in
  functions { include: <fn-path>, …; };//  one sub-block = parse error);
  functions { exclude: <fn-path>, …; };//  all lists non-empty (≥ 1); fn
                                       //  entries require the fn:: prefix
  depth: <number>;                    // u32
  complexity: <number>;               // u32
  introspection: auto | none;         // default auto
};
```

A bare `auto;`/`none;` flag and subsequent `tables:`/`functions:`
clauses fold last-one-wins per family (rule 7).

## Variant `config default { … }`

```
config default {
  namespace: <expr>;                  // ≥ one of the two required; the
  database: <expr>;                   //   unset side emits NONE
};
```

Emit: `DEFINE CONFIG OVERWRITE (API <cfg> | GRAPHQL <cfg> | DEFAULT
NAMESPACE <expr> DATABASE <expr>);` — GraphQL kind params emit in
SurrealQL keyword form; `introspection: auto` emits nothing (default).

## Reverse parser (INFO → SDL)

**INFO renders configs as header-less fragments**, not full DDL: the
catalog emits only `GRAPHQL TABLES … FUNCTIONS …`, `API MIDDLEWARE …
PERMISSIONS …`, or `DEFAULT NAMESPACE <ident>` — the reverse parser
prepends `DEFINE CONFIG OVERWRITE` before handing it to the embedded
parser (README item 4). Further verified shape facts:

- The `default` config's fragment emits **only the set side**, as a
  bare identifier (`DEFAULT NAMESPACE acme` — no quotes, no
  `DATABASE NONE`); map absent sides to unset.
- `API` fragments always carry `PERMISSIONS …` (default `FULL` — elide
  at default); middleware args arrive as computed values.
- GraphQL: `AUTO` materializes as `TABLES AUTO FUNCTIONS AUTO` → maps
  to `tables: auto;` + `functions: auto;` (the canonical writer emits
  the resolved per-family spellings — the bare `auto;` flag is an
  input-only fold, indistinguishable in the AST from the two-clause
  spelling); absent `INTROSPECTION` = auto (elide); absent families =
  none.
- The `default` config is ROOT-level and is exposed by `INFO FOR ROOT`
  under `defaults` as a **structured value** (not DDL): `{}` when
  unset, else `{ namespace?: string, database?: string }` with absent
  keys meaning unset — use that path to discover it; the `API` and
  `GRAPHQL` fragments above are what `INFO FOR DB … configs` yields
  (the `default` fragment form is unreachable through INFO in 3.2.4).

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): `API MIDDLEWARE … PERMISSIONS
NONE` ✅; full `GRAPHQL TABLES INCLUDE user, post FUNCTIONS NONE DEPTH
4 COMPLEXITY 100 INTROSPECTION NONE` ✅; bare `GRAPHQL AUTO` ✅;
`DEFAULT NAMESPACE 'acme' DATABASE 'blog'` ✅ (with OVERWRITE);
singleton + seeding behavior ✅.

## Examples (one per clause)

api variant — global endpoint middleware + permissions:

```sdl
config api {
  middleware: api::timeout(5s), api::log();
  permissions: $auth.admin = true;
};
```

```surql
DEFINE CONFIG OVERWRITE API
  MIDDLEWARE api::timeout(5s), api::log() PERMISSIONS WHERE $auth.admin = true;
```

graphql auto flag:

```sdl
config graphql {
  auto;
};
```

```surql
DEFINE CONFIG OVERWRITE GRAPHQL AUTO;
```

graphql tables/functions include-exclude:

```sdl
config graphql {
  tables { include: user, post; };
  functions { exclude: fn::legacy_report; };
};
```

```surql
DEFINE CONFIG OVERWRITE GRAPHQL TABLES INCLUDE user, post
  FUNCTIONS EXCLUDE fn::legacy_report;
```

graphql depth + complexity + introspection:

```sdl
config graphql {
  depth: 4;
  complexity: 100;
  introspection: none;
};
```

```surql
DEFINE CONFIG OVERWRITE GRAPHQL DEPTH 4 COMPLEXITY 100 INTROSPECTION NONE;
```

default both, and namespace-only (the clause-less side emits
`DATABASE NONE`; one `config default` per document — rule 7's
duplicates rule — so the two shapes are separate statements):

```sdl
config default {
  namespace: 'acme';
  database: 'blog';
};
```

```surql
DEFINE CONFIG OVERWRITE DEFAULT NAMESPACE 'acme' DATABASE 'blog';
```

```sdl
config default {
  namespace: 'acme';
};
```

```surql
DEFINE CONFIG OVERWRITE DEFAULT NAMESPACE 'acme' DATABASE NONE;
```
