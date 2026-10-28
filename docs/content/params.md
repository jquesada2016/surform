Params and sequences are the two small database-side values a schema
often needs: named constants and counters.

## Params: define-time constants

A param is a `$`-named value evaluated **once, when the schema
applies** — the result is stored, not the expression:

```sdl
namespace acme;
database shop;

/// Default page size for list endpoints.
param $default_limit = 50;
```

The value is any SurrealQL expression — including an object literal,
since the `{` after `=` is just the value's first character:

```sdl
namespace acme;
database shop;

param $endpoints = { read: '/api/read', write: '/api/write' };
```

The define-time-freeze has a flip side: an expression like
`time::now()` evaluates once, at apply, and the *result* is what the
database stores — so `$t` reads as the moment the schema went on,
not the moment you query it. (Watch the braces, too: `{ time::now() }`
is a one-element [set](types.html), not a call.) Re-applying the
emitted DDL freezes a fresh timestamp each time.

### The block form

Anything beyond the value — permissions — needs the block form:

```sdl
namespace acme;
database shop;

param $internal_seed {
  value: 42;
  permissions: none;
};
```

A param with no `value:` at all is legal but almost never what you
meant — the linter says so (`SDL-L0008`):

```sdl-lint expect=SDL-L0008
namespace acme;
database shop;

param $placeholder;
```

## Sequences: monotonic counters

A sequence hands out ever-increasing numbers — invoice numbers, order
ids — distributed-safe and never rolled back, even in failed
transactions. Clients consume it with
`sequence::nextval('name')`:

```sdl
namespace acme;
database shop;

sequence invoice_number = 1000;
```

The short form sets the start value. The block form tunes the rest:
`batch:` (how many values each node reserves at a time — the
distributed-safety margin) and `timeout:` (the per-query acquisition
limit):

```sdl
namespace acme;
database shop;

sequence order_number {
  start: 5000;
  batch: 100;
  timeout: 5s;
};
```

With defaults everywhere, the bare form is the whole declaration —
and normalize returns it that short, folding the database's verbose
`BATCH 1000 START 0` away:

```sdl
namespace acme;
database shop;

sequence row_ids;
```

A sequence definition is **bootstrap-only**: the emitted form is
`DEFINE SEQUENCE IF NOT EXISTS`, so applying your build to a fresh
database seeds the counter at the declared start, and applying it
again leaves a running counter alone. That is deliberate — the
redefining form restarts the counter, re-issuing numbers already
handed out. The same holds for `batch:` and `timeout:`: changing them
in the file does not reconfigure a sequence that already exists.
Repositioning a live counter is a manual, destructive step that must
spell every clause you want to keep — an omitted clause falls back to
its default (`start` 0, `batch` 1000, `timeout` none):

```sql
DEFINE SEQUENCE OVERWRITE invoice_number BATCH 1000 START 5000;
```

A `timeout:`-only change has a non-destructive repair that leaves the
counter running:

```sql
ALTER SEQUENCE invoice_number TIMEOUT 5s;
```

One rule to remember: **`sequence` has no COMMENT clause in
SurrealQL**, so a `///` doc comment above one is rejected — use a
plain `//` line instead:

```sdl
namespace acme;
database shop;

// Invoice numbers start at 1000 — a plain comment, because sequence
// is one of the two statements (with config) that take no doc
// comment.
sequence invoice_number = 1000;
```

## Where next

- [Config](config.html) — the other global knobs.
- [The linter](lint.html) — the valueless-param warning.
