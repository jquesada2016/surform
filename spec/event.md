# DEFINE EVENT

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_event`),
defaults (`catalog/schema/event.rs`), and emitter
(`sql/statements/define/event.rs`). Cross-checked against a local
in-memory 3.2.3 server (2026-09-18); the floor move re-runs the corpus on the 3.3.0 embed (2026-09-25). SDL blocks are free-ordered;
codegen emits SurrealQL's canonical order `ASYNC [RETRY n] [MAXDEPTH n]
WHEN … THEN … COMMENT …`. SurrealQL accepts `RETRY`/`MAXDEPTH` only
after `ASYNC` (verified parse errors otherwise); SDL groups them in
the `async` flag+sub-block pattern, and codegen emits them glued after
`ASYNC`.

```rust
struct Event {
  name: Ident,
  on: TableRef,                       // required standalone; implied when
                                       //   nested inside a table block
  when: Expr,                         // defaults to true — SurrealQL
                                       //   materializes WHEN true (verified)
  then: Vec<Expr>,                    // ≥ 1 required (parser enforces)
  async: Option<Async>,               // Async { retry = 1, maxdepth = 3 }
                                       //   retry/maxdepth: u16, unclamped —
                                       //   no 0–16 range exists (docs
                                       //   folklore; the parser's own tests
                                       //   parse MAXDEPTH 64)
  comment: Option<DocComment>,        // from ///
}
```

```
/// <doc comment>                        // optional — emitted as COMMENT
event <name> {
  // required:
  on: <table>;                        // the `TABLE` keyword of SurrealQL's
                                       //   ON TABLE is implicit in the value
  then: <expr>;                       // one expression (often a { block })
                                       //   or a comma list — SurrealQL's
                                       //   THEN a, b form; SDL splits the
                                       //   list on depth-0 commas and each
                                       //   item goes to the embedded parser

  // optional:
  when: <expr>;                       // omitted → SurrealQL stores WHEN true
  async;                              // flag — runs outside the triggering
                                       //   transaction (retry 1, maxdepth 3)
  async {
    retry: <number>;                  // u16, both optional; defaults 1 / 3
    maxdepth: <number>;               // retry 0 disables retries
  };
};
```

Event bodies see `$event` (`'CREATE' | 'UPDATE' | 'DELETE'`), `$before`
(the pre-change record; none on create), `$after` (the post-change
record; none on delete), `$value` (the record in question — pre on
delete, post otherwise), and `$input` (the query input).

Emit: `DEFINE EVENT <name> ON <table> [ASYNC [RETRY n] [MAXDEPTH n]]
[WHEN <expr>] THEN <expr>(, <expr>)* [COMMENT …];` — an omitted
`when:` is simply not emitted; SurrealQL materializes `WHEN true`
internally. The THEN items carry the v3.2.4 printer's `CoverStmts`
canon: a single-statement braced block prints without a trailing `;`
(`THEN { CREATE log SET x = 1 }`), a multi-statement one prints `;`
after every statement, and a reserved-word fn-path head is
backticked (`\`rand\`::bool()`).

## Reverse parser (INFO → SDL)

INFO emits the plain `DEFINE` in the canonical order with two
materializations: `WHEN true` (elide — it is the default) and, for
async events, `ASYNC RETRY n MAXDEPTH n` **always explicit** (bare
`ASYNC` never appears; defaults come back as `RETRY 1 MAXDEPTH 3`) —
canonical SDL collapses a block whose values equal 1/3 to the bare
`async;` flag. Events are listed by `INFO FOR TABLE <t>` (the
`events` map); canonical placement nests them into their table block
(`on:` implied). `COMMENT '…'` maps back to `///`.

THEN items arrive in the printer's `CoverStmts` shapes: a
statement-form body (`IF`/`CREATE`/…, and a fetch-carrying `RETURN`)
prints parenthesized, while value-shaped bodies print **bare** —
`THEN fn::audit::write($event, $after.id)`, `THEN THROW 'boom'`,
`THEN RETURN $after.id`, `THEN BREAK`, a bare call path — and the
reader's `THEN` stop confirms the word-led shapes (the same word
discipline the clause-value follows use: an operator word or the
capture's own next clause keyword stays content). The other bare
`CoverStmts` shapes fail closed loudly — the read rejects with
SDL-E3003, not a raw route (contrived authoring by design) —
`$`- and sign-led bodies (`THEN $x`, `THEN -1`; `-` is
the binary minus in a WHEN expression), bare array/set literals
(`THEN [1, 2]`), regex literals, `⟨…⟩`-/backtick-led words, and
unary `+`.

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): when+then ✅; missing `THEN` →
parse error ✅ (`THEN statement required`); `RETRY` without `ASYNC` →
parse error ✅ (`RETRY must be set after ASYNC`); `ASYNC RETRY 5
MAXDEPTH 8` with a block body ✅; multi-expression comma `THEN` ✅;
round-trip shows `WHEN true` materialization ✅; the bare word-led
INFO shapes (`THEN fn::…(…)`, `THEN THROW '…'`, `THEN RETURN …`,
`THEN BREAK`, multi-item `THEN fn::a(), THROW 'b'`) read and
re-emit verbatim ✅ (2026-09-30).

## Examples (one per clause)

when + then + doc comment:

```sdl
/// Log every user creation
event user_created {
  on: user;
  when: $event = 'CREATE';
  then: { create log set at = time::now(), user = $after.id; };
};
```

```surql
DEFINE EVENT user_created ON user WHEN $event = 'CREATE'
  THEN { CREATE log SET at = time::now(), user = $after.id }
  COMMENT 'Log every user creation';
```

then only (no when — materializes `WHEN true`):

```sdl
event touch_counter {
  on: post;
  then: (update counter:posts set total = total + 1);
};
```

```surql
DEFINE EVENT touch_counter ON post THEN (UPDATE counter:posts SET total = total + 1);
```

async (flag and configured forms — the block collapses to the flag
when values equal the 1/3 defaults):

```sdl
event webhook_fanout {
  on: invoice;
  async {
    retry: 5;
    maxdepth: 8;
  };
  then: {
    if rand::bool() { throw 'flaky downstream'; }
  };
};
```

```surql
DEFINE EVENT webhook_fanout ON invoice ASYNC RETRY 5 MAXDEPTH 8
  THEN { IF `rand`::bool() { THROW 'flaky downstream' } };
```

multi-expression then (comma list — SurrealQL's `THEN a, b` form):

```sdl
event double_bookkeeping {
  on: payment;
  when: $event = 'UPDATE' and $before.status != $after.status;
  then: (create ledger_entry set payment = $after.id),
        (update stats:payments set changes = changes + 1);
};
```

```surql
DEFINE EVENT double_bookkeeping ON payment
  WHEN $event = 'UPDATE' AND $before.status != $after.status
  THEN (CREATE ledger_entry SET payment = $after.id),
       (UPDATE stats:payments SET changes = changes + 1);
```

## Nesting

Events are table-scoped: inside a `table` block they omit `on:` (see
[table.md](table.md)); standalone they keep it — exactly one of the
two forms applies.
