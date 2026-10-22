Config statements set the per-database singletons — the GraphQL
interface's shape, global API-endpoint defaults, and the server's
default namespace/database. They have **no name**: the variant *is*
the header, and there is no `///` doc comment (no COMMENT clause
exists — use a plain `//` line instead).

## GraphQL: expose tables and functions

The GraphQL interface defaults to exposing **nothing**. Switch it on
explicitly:

```sdl
namespace acme;
database shop;

config graphql {
  auto;
};
```

`auto;` sets *both families* — tables and functions — to auto-expose.
Or curate the surface with include/exclude lists:

```sdl
namespace acme;
database shop;

table user {
  schemafull;
  field email: string;
};
table post {
  schemafull;
  field title: string;
};

config graphql {
  tables { include: user, post; };
  functions { exclude: fn::legacy_report; };
  depth: 4;
  complexity: 100;
  introspection: none;
};
```

- `tables:`/`functions:` each take `auto`, `none`, or a sub-block
  with exactly one of `include:`/`exclude:`.
- `depth:` and `complexity:` bound query cost.
- `introspection:` defaults to auto — `none;` hides the schema from
  clients.

## API: global endpoint defaults

Middleware and permissions applied to every [API route](api.html):
middleware runs in list order; its arguments are SurrealQL
expressions evaluated once at define time:

```sdl
namespace acme;
database shop;

config api {
  middleware: api::timeout(5s), api::log();
  permissions: $auth.admin = true;
};
```

## Default namespace and database

The server's answer to "connect with no namespace/database
selected". At least one side is required; the unset side stays unset.
This one executes at **root** level, above any `USE` line:

```sdl
namespace acme;
database shop;

config default {
  namespace: 'acme';
  database: 'shop';
};
```

## A note on round-trips

Configs are singletons — re-defining is what `OVERWRITE` is for, and
the compiler always emits it. The database reports configs as
header-less fragments (`GRAPHQL TABLES … FUNCTIONS …`), not full
statements; the reverse reader reassembles them, so
[normalize](normalize.html) folds them back into your file
faithfully.

## Where next

- [API routes](api.html) — the endpoints this config decorates.
- [Buckets](buckets.html) — the other capability-gated statement.
