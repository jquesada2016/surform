# DEFINE SEQUENCE

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_sequence`)
and AST/emitter (`sql/statements/define/sequence.rs`). Cross-checked
against a local in-memory 3.2.3 server (2026-09-18); the floor move re-runs the corpus on the 3.3.0 embed (2026-09-25).

Distributed, monotonically increasing number generators; values are
never rolled back, even in failed transactions. Consumed via
`sequence::nextval('<name>')`.

Notable parser facts:
- SurrealQL's clause order is **fixed**: `BATCH` before `START` before
  `TIMEOUT` (`START 100 BATCH 50` is a parse error — verified). SDL's
  block form is free-ordered; codegen emits the canonical order.
- **No `COMMENT` clause exists** (verified parse error) — `///` is
  rejected on `sequence` (one of the two such statements, with
  `config`; rule 5).
- Defaults: `start: 0`, `batch: 1000` — both materialize in INFO
  round-trips (verified).
- `timeout:` is a SurrealQL expression in the parser; SDL accepts the
  full expression per parity, with duration literals as the
  conventional value.

```rust
struct Sequence {
  name: Ident,
  start: Expr,                        // default 0
  batch: Expr,                        // values reserved per node; default 1000
  timeout: Expr,                      // absent = Literal::None sentinel
}
```

```
sequence <name> = <expr>;              // short form — start value only

sequence <name> {
  // optional:
  start: <expr>;                       // first value; default 0
  batch: <expr>;                       // values reserved per node at once;
                                       //   default 1000
  timeout: <expr>;                     // per-query acquisition limit;
};                                     //   duration literal conventional
```

Emit (canonical order; `OVERWRITE` implied): `DEFINE SEQUENCE <name>
[BATCH <expr>] [START <expr>] [TIMEOUT <expr>];` — regardless of SDL
clause order.

## Reverse parser (INFO → SDL)

INFO always renders `BATCH … START …` (defaults materialize:
`BATCH 1000 START 0`; `TIMEOUT` omitted when absent). Canonical
minimal form: both at defaults → bare `sequence <name>;`; `START`
non-default with everything else default → the short form
`sequence <name> = <start>;`; otherwise the block with non-default
clauses only. Form selection is thus total. Sequences are listed by
`INFO FOR DB` (the `sequences` map); kind prefixes strip.

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): `BATCH 50 START 100 TIMEOUT
5s` ✅; `START` before `BATCH` → parse error ✅; `COMMENT` → parse
error ✅; bare sequence defines and `nextval` returns 0 then 1 ✅;
round-trip materializes `BATCH 1000 START 0` ✅.

## Examples (one per clause)

short form — start value only:

```sdl
sequence invoice_number = 1000;
```

```surql
DEFINE SEQUENCE invoice_number START 1000;
```

bare (all defaults):

```sdl
sequence row_ids;
```

```surql
DEFINE SEQUENCE row_ids;
```

full block — start, batch, timeout:

```sdl
sequence order_number {
  start: 5000;
  batch: 100;
  timeout: 5s;
};
```

```surql
DEFINE SEQUENCE order_number BATCH 100 START 5000 TIMEOUT 5s;
```

usage (for orientation — not SDL):

```surql
sequence::nextval('invoice_number');   -- → 1000, then 1001, …
```
