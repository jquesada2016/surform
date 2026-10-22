An event is a trigger: when a record in its table changes, its
SurrealQL runs. `when:` filters which changes fire; `then:` is the
action (or a comma list of actions).

## The shape

```sdl
namespace acme;
database shop;

/// Log every user creation.
event user_created {
  on: user;
  when: $event = 'CREATE';
  then: { create log set at = time::now(), user = $after.id; };
};
```

`on:` names the table (standalone form — nested inside the table
block, the line disappears). The body sees SurrealDB's event
variables:

| variable | value |
|---|---|
| `$event` | `'CREATE'`, `'UPDATE'`, or `'DELETE'` |
| `$before` | the record before the change (nothing on create) |
| `$after` | the record after the change (nothing on delete) |
| `$value` | the record itself — pre-change on delete, post otherwise |

## Then-only: react to everything

Omitting `when:` means *every* change — the database records that as
the literal `WHEN true`, and normalize folds it back to omission:

```sdl
namespace acme;
database shop;

table post {
  schemafull;
  field title: string;
};

event touch_counter {
  on: post;
  then: (update counter:posts set total = total + 1);
};
```

## Async: run outside the transaction

`async;` detaches the action from the triggering write. The flag
carries defaults (retry once, depth 3); the block form tunes them,
and the block collapses back to the bare flag when the values equal
the defaults:

```sdl
namespace acme;
database shop;

table invoice {
  schemafull;
  field total: number;
};

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

## Multi-step reactions

`then:` takes a comma list — several actions, one trigger:

```sdl
namespace acme;
database shop;

table payment {
  schemafull;
  field status: string;
};

event double_bookkeeping {
  on: payment;
  when: $event = 'UPDATE' and $before.status != $after.status;
  then: (create ledger_entry set payment = $after.id),
        (update stats:payments set changes = changes + 1);
};
```

## Nested form

Inside its table, an event drops the `on:` line — the usual nesting
rule:

```sdl
namespace acme;
database shop;

table audit_example {
  schemafull;
  field title: string;

  event audit_title {
    when: $event = 'UPDATE' and $before.title != $after.title;
    then: { create audit set at = time::now(), id = $after.id; };
  };
};
```

## Where next

- [Tables](tables.html#nesting-fields-indexes-events-live-in-the-table)
  — where events usually live.
- [Functions](functions.html) — call your own functions from `then:`.
