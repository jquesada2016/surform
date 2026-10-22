This page takes a small schema from an empty directory to a compiled,
apply-ready script.

## Install

Surform is a single Rust program with no runtime dependencies. You need
the Rust toolchain ([rustup](https://rustup.rs) installs it in one
command), then clone this repository and build the compiler:

```sh
cargo install --path .
```

That puts the `surform` binary on your PATH. Check it:

```cli-help cmd=
```

## The schema

Here is the quick-start schema — a shop with users and posts. This is
`examples/shop.surdl` in the docs sources; every panel on this page is
produced from this file. The examples on these pages are editable:
the verdict and the DDL recompute as you type, and Reset brings the
verified text back.

```sdl src=shop
```

- `///` **doc comments** become the definition's `COMMENT` in
  SurrealDB — your documentation travels with the schema.
- `field email: string { assert: … };` — anything after `:` is
  SurrealQL, checked by SurrealDB itself. Surform does not invent a
  second expression language.
- `index unique email_unique` is nested inside its table, the way you
  think about it; the compiler emits the standalone `DEFINE INDEX`
  SurrealDB expects.
- `field author: record<user>` is a graph-style reference — types pass
  through verbatim.

## Check it

`check` parses the file and reports problems. On a clean file it says
nothing at all — silence is success:

```session cmd=check src=shop
```

The same check on a file with a misspelled `permissions` clause:

```sdl-error expect=SDL-E1003
namespace acme;
database shop;

table person {
  schemafull;
  field name: string;
  permisions: full;
};
```

The diagnostic points at the offending line, names the code, and
suggests a fix. The [CLI reference](cli.html) has more on reading
diagnostics.

## Compile it

`build` prints the SurrealQL DDL to stdout — pipe it to a file, to
`surreal sql`, or to your deployment pipeline:

```session cmd=build src=shop
```

Two conventions the output relies on:

- Every emitted `DEFINE` carries **`OVERWRITE`** (one exception: a
  user with no credential defines once —
  [Users & roles](users.html)). Applying the script
  twice — or applying it over a database that already has some of the
  definitions — redefines cleanly instead of erroring. Your schema file
  is the *desired state*; applying it converges the database to it.
- After each `DEFINE NAMESPACE` / `DEFINE DATABASE` the compiler emits
  the matching **`USE`** line, so one script crosses namespace and
  database boundaries safely (a database with no preceding namespace
  omits the `USE`).

## Apply it

The build output is ordinary SurrealQL — run it anywhere SurrealDB 3.3
runs. For example, with the SurrealDB CLI installed:

```sh
surform build schema.surdl > schema.surql
surreal sql --conn ws://localhost:8000 --user root --pass … --ns acme --db shop < schema.surql
```

Surform itself never touches your database connection — it produces
the script, your existing tooling applies it. (Adopting a database —
the reverse trip — is [one command](normalize.html).)

## Keep it tidy

`fmt` prints the canonical form of a file: one clause style, one
clause order, shortest spellings. Team members stop arguing about
layout — the compiler owns it:

```sdl-fmt
namespace acme; database shop;
table `user` { schemafull; field name:string; field email: string { readonly; }; }
```

## Let the linter catch the traps

Some mistakes are legal but dangerous. This table compiles, applies,
and silently grants everyone everything — `permissions: full;` lets
anyone, even unauthenticated users, read and write every record:

```sdl-lint expect=SDL-L0001
namespace acme;
database shop;

table secrets {
  schemafull;
  field payload: string;
  permissions: full;
};
```

`surform lint` explains why and links to the fix. Omitting the clause
keeps the database's secure default — every operation denied — and
never warns. The full catalog of
warnings lives on [the linter page](lint.html).

## Where next

- [How a schema file works](basics.html) — files, ordering, comments,
  names, and the apply model in depth.
- [Tables](tables.html) is the first language page; the sidebar walks
  the rest.
- [The surform CLI](cli.html) — every command and flag.
