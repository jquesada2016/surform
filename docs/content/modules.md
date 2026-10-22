Modules are SurrealDB's Surrealism packages — sandboxed WebAssembly
units that bring new functions into the query language. They are
experimental: the server needs the `surrealism` capability
(`--allow-experimental=surrealism`), and Surform compiles the
statements but — like every tool that speaks to the embedded engine
without capabilities — cannot execute them for you.

## The two executables

A module wraps one of two things: a local surrealism file, or a
published silo package. Local files need a name; silo packages are
self-named:

```sdl no-exec
namespace acme;
database shop;

/// Colour helpers.
module color {
  from: f'modules:/color.surli';
  unsigned;
};
```

```sdl no-exec
namespace acme;
database shop;

module {
  from: silo::acme::utils::<1.2.0>;
  unsigned;
};
```

`unsigned;` is required in SDL — only silo-published packages carry
signatures. The rest of the clause grammar:

- `permissions:` — the single-value form (`none | full | <expr>`;
  default full). It is present in the parser though absent from
  SurrealDB's reference; SDL elides it at the default and the emitted
  DDL round-trips byte-identically.
- The silo version is a `maj.min.patch` triple after the package:
  `silo::<org>::<package>::<1.2.0>`. Silo packages are self-named, so
  the statement takes no name there (name + silo is a grammar error);
  a file executable requires one.

## Why these examples aren't applied

The embedded engine's query path runs with default capabilities,
where `DEFINE MODULE` is rejected, so these examples stop after DDL
validation (the *compiles clean* badge —
[verification](verification.html) lists what each badge means). A
capability-enabled server accepts the emitted DDL as-is; the docs
cannot run that step in-process. The same protocol applies to
[buckets without a backend](buckets.html#no-backend-you-need-a-global-bucket) on servers
with no global bucket.

## Where next

- [Surform inside SurrealDB](in-database.html) — Surform itself as a
  module: migrate a database from within SurrealQL.
- [Analyzers](analyzers.html) — modules' `mod::` functions can drive
  analyzer preprocessing.
- [Buckets](buckets.html) — the other experimental surface.
