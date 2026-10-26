Functions are stored SurrealQL procedures — callable from queries,
other functions, events, and permission expressions. The keyword is
`fn`; parameters keep their `$`; the body is real SurrealQL.

## The shape

```sdl
namespace acme;
database shop;

/// Whether the current user may edit this post.
fn can_edit($p: record<post>) -> bool {
  return $auth.id = $p.author or $auth.role = 'admin';
} {
  permissions: $auth.admin = true;
};
```

Reading it: the signature `fn name($param: type) -> return_type`,
then a brace block with the **body**, then — optionally — a second
brace block with ordinary clauses (`permissions:`, GraphQL hints).
It is the one statement with two blocks.

The body's last statement, without a trailing `;`, is the return
value:

```sdl
namespace acme;
database shop;

fn tax_total($net: number, $rate: option<number>) {
  let $r = $rate ?? 0.15;
  let $tax = math::floor($net * $r);
  $net + $tax
};

fn ping() -> string {
  'pong'
};
```

Zero-parameter functions are legal (`fn ping() { … };`), and the
body is re-emitted in the engine's canonical form — the compiler
hands your SurrealQL to the official parser and prints its result,
so a stored body and your file agree after a round-trip.

## Permissions

Functions default to `permissions: full` — anyone who can run
queries can call them. Lock them down per function:

```sdl
namespace acme;
database shop;

table log {
  schemafull;
  field at: datetime;
};

fn purge_logs($before: datetime) {
  delete log where at < $before;
} {
  permissions: none;
};
```

`permissions:` takes `none`, `full`, or an expression (which the
compiler emits as `PERMISSIONS WHERE …`).

## Optional parameters

A trailing `option<T>` parameter may be omitted at the call site —
`fn::last_opt(5)` and `fn::last_opt(5, 2)` both work:

```sdl
namespace acme;
database shop;

fn last_opt($req: number, $opt: option<number>) {
  $opt ?? $req
};
```

## Names with segments, GraphQL hints

Names may carry `::` segments — `fn util::slugify(…)` defines
`fn::util::slugify`. The name never spells the prefix itself — the
`fn` keyword carries it, so `fn fn::weird(…)` is an error
(SDL-E1075): spelled as written it would define `fn::fn::weird`, a
different function. GraphQL hints ride the second block:

```sdl
namespace acme;
database shop;

fn util::slugify($s: string) -> string {
  string::lowercase(string::replace($s, ' ', '-'))
} {
  graphql {
    alias: 'slugify';
  };
};
```

## Where next

- [Access (sign-in & tokens)](access.html) — `signin:` and
  `signup:` take query blocks, projected with the same `Fields`
  machinery.
- [Events](events.html) — call `fn::…` from a trigger.
