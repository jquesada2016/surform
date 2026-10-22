`api` statements turn your database into an HTTP endpoint: each one
maps a route path to SurrealQL responses per HTTP method — the routes
live in the schema file, next to the tables they serve.

## The shape

The "name" is a route path string. Method groups are the structural
sub-blocks; each group's `then:` is the response — an object with
`status`, `body`, `headers`, and other fields:

```sdl
namespace acme;
database shop;

/// Health check.
api '/health' {
  get {
    then: { { status: 200, body: { ok: true } } };
  };
};
```

The double braces are not a typo: the outer pair is SurrealQL's
block, the inner is the response object literal.

## Paths: static, dynamic, wildcard

Segments tell SurrealDB how to match: `:name` captures one segment,
`*name` swallows the rest (and must end the path):

```sdl
namespace acme;
database shop;

table user {
  schemafull;
  field email: string;
};

api '/users/:id' {
  get {
    then: {
      { status: 200, body: type::record('user', $request.params.id) }
    };
  };
};
```

Inside `then:`, `$request` carries `body`, `headers`, `params`,
`method`, `query`, `context`, and `request_id`.

## Groups, permissions, fallbacks

Methods comma-group when they share a response; each method may
appear in only one group. Every group takes `permissions:` and
`middleware:`; the `any` block is the endpoint-wide catch-all —
endpoint middleware plus a fallback response:

```sdl
namespace acme;
database shop;

api '/admin/*rest' {
  any {
    middleware: api::timeout(5s);
    then: { { status: 404, body: { error: 'no such route' } } };
  };
  get, delete {
    permissions: $auth.admin = true;
    then: { { status: 200 } };
  };
  post {
    permissions: none;
    then: { { status: 204 } };
  };
};
```

Method names are lowercase in the DDL (SurrealDB prints `FOR get`),
and SDL accepts only the lowercase spelling — an uppercase head is
SDL-E1035, with the lowercase form as the did-you-mean.

## The implied `any`

A small reverse nicety: the database always reports a `FOR any`
clause even when you never wrote one — as long as that implied clause
carries no middleware, no fallback, and default permissions,
[normalize](normalize.html) folds it away and your file stays clean.
An explicitly empty `any { };` reads as exactly that no-op, which the
linter flags (`SDL-L0009`) — say what you mean: give the endpoint a
real fallback or drop the block.

## Where next

- [Config](config.html) — the global `config api` middleware and
  permissions every route inherits.
- [Functions](functions.html) — move complex response logic into
  `fn::` calls.
