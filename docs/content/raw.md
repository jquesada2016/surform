SDL covers the whole current SurrealQL definition grammar — but not
every byte the engine accepts, and tomorrow's engine may accept more.
`raw` is the escape hatch: a block whose contents pass through to the
DDL byte for byte.

## The mechanism

Everything between the braces is captured verbatim — comments,
spacing, spelling — and emitted unchanged (only the leading and
trailing edges are trimmed):

```sdl
namespace acme;
database shop;

raw {
  // This comment travels to the DDL — raw keeps every interior byte,
  // unlike SDL's own comments which codegen drops.
  DEFINE PARAM OVERWRITE $raw_demo VALUE 1;
};
```

Use it for legal SurrealQL shapes outside SDL's surface — legacy
index options, a statement SDL has not grown yet — so your schema
file stays complete while the language catches up.

## The rules raw plays by

- **Top level only.** `raw` is a reserved word inside every block —
  no nesting. (A field literally named `raw` escapes via backticks or
  the `field` prefix.)
- **No doc comments.** `///` above a raw block is rejected — there is
  no COMMENT clause to hang one on.
- **Validation is advisory.** A body the SurrealQL parser rejects is
  still carried and still emitted — the lint below tells you, and
  never blocks. Rule 8 exists for exactly the class of bodies whose
  validity the pinned parser cannot confirm.
- **Your responsibility at apply time.** The compiler passes the body
  through; the engine is the final judge.

## When raw body is broken

```sdl-lint expect=SDL-L0012
namespace acme;
database shop;

raw {
  DEFINE NOT_A_REAL_STATEMENT foo;
};
```

`surform lint` names the body's problem; `check` and `build` still
succeed.

## Where next

- [How a schema file works](basics.html) — where raw fits in the file
  model.
- [The linter](lint.html) — the raw-body advisory (`SDL-L0012`).
