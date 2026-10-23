> **Like what you see here?** Please consider [hiring me](https://josequesada.dev).

# Surform

**SurrealDB schemas, declared.** Surform is a declarative Schema
Definition Language (SDL) for SurrealDB v3 with a compiler, `surform`,
that turns a schema file into SurrealQL `DEFINE` DDL — and reads the
database's rendered INFO output back, so your schema file and your
database never drift apart silently.

Write `schema.surdl`:

```sdl
namespace acme;
database shop;

table user {
  schemafull;
  field email: string { assert: string::is_email($value); };
  field name: string;
  index unique email_unique {
    fields: email;
  };
};
```

then:

```sh
surform check schema.surdl   # validate
surform build schema.surdl   # emit SurrealQL DDL
surform fmt schema.surdl     # canonical style
surform migrate generate schema.surdl --info db=acme/shop=info.db.txt
                             # what drifted + the script that converges it
surform migrate generate --from schema-v1.surdl --to schema-v2.surdl --save
                             # a numbered draft under migrations/ (fill the
                             # SURFORM SLOT regions, then commit it)
surform migrate commit migrations/0001_schema-v2.surql -m "to v2"
                             # hash + ledger-record the migration, inside its transaction
surform migrate push --endpoint http://127.0.0.1:8000 --ns acme --db shop
                             # apply the committed migrations in number order,
                             # skipping what the ledger already records
surform migrate status --endpoint http://127.0.0.1:8000 --ns acme --db shop
                             # the directory against the ledger — applied /
                             # pending / DRIFTED / missing (exit 1 on drift)
surform migrate revert --to 1 --endpoint http://127.0.0.1:8000 --ns acme --db shop
                             # draft the way back to a recorded state (fill,
                             # commit, push — nothing applies at revert time)
```

A schema can span several files: `include 'tables/users.surdl';`
composes them into one stream that compiles, lints, and migrates
exactly like its flattening ([spec/include.md](https://github.com/jquesada2016/surform/blob/main/spec/include.md)).

The language specification lives in
[spec/](https://github.com/jquesada2016/surform/tree/main/spec) — start
with
[spec/README.md](https://github.com/jquesada2016/surform/blob/main/spec/README.md)
for global conventions, syntax rules, and
the verification workflow; each `DEFINE` statement has its own file there
(e.g. [spec/access.md](https://github.com/jquesada2016/surform/blob/main/spec/access.md)).
User-facing documentation lives at
[surform.josequesada.dev](https://surform.josequesada.dev) (sources
under [docs/](https://github.com/jquesada2016/surform/tree/main/docs)). A VS Code extension — syntax highlighting for
`.surdl`, Format Document through the `surform` binary, diagnostics in
an output channel — lives in
[editors/vscode/](https://github.com/jquesada2016/surform/blob/main/editors/vscode/README.md) (package a `.vsix` with
`vsce`; the overview is on the docs site). An agent-harness skill —
[`.agents/skills/surform/`](https://github.com/jquesada2016/surform/blob/main/.agents/skills/surform/SKILL.md) — teaches
coding agents the language and the CLI; copy the folder to
`~/.agents/skills/surform/` to install it in your own harness.

Status: under active development;
[CONTRIBUTING.md](https://github.com/jquesada2016/surform/blob/main/CONTRIBUTING.md)
has the build and test recipe. A Surrealism module — the same compiler
packaged for in-database dispatch as
`mod::surform::compile`/`converge`/`status`
([surform-surrealism/](https://github.com/jquesada2016/surform/tree/main/surform-surrealism)) — builds
alongside the CLI in two artifacts: the zero-capability default and
an apply variant whose `mod::surform_apply::apply(sdl)` converges the
invoking session's database and executes the script at the caller's
own permissions. The `.surli` artifacts ship as release assets
(checksummed; byte-deterministic to rebuild), and the working-today
flow — registration, the embeddable migration snippet, and migrating
from within SurrealQL — is documented on the docs site ("Surform
inside SurrealDB").

## License

Surform's code is dual-licensed under either of
[MIT](https://github.com/jquesada2016/surform/blob/main/LICENSE-MIT) or
[Apache-2.0](https://github.com/jquesada2016/surform/blob/main/LICENSE-APACHE),
at your option.

The binary and the playground wasm embed SurrealDB's SurrealQL parser,
which SurrealDB Ltd. licenses under the Business Source License 1.1 —
see [THIRD-PARTY-NOTICES.md](https://github.com/jquesada2016/surform/blob/main/THIRD-PARTY-NOTICES.md) for its text and
the full dependency attribution.
