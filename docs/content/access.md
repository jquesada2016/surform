Accesses are SurrealDB's authentication schemes: they define how
tokens are *verified* and, for your application's end users, how
accounts *sign up* and *sign in*. Three kinds exist, chosen with
`type:` — `record`, `jwt`, and `bearer`.

## Record access: your app's users

The most common kind. A record access turns an email-and-password
pair into a record user — SurrealDB runs your SurrealQL to create the
account (`signup`) and to check a sign-in (`signin`):

```sdl
namespace acme;
database shop;

table user {
  schemafull;
  field email: string;
  field password: string;
};

access account {
  on: database;
  type: record;
  signup: {
    create user set
      email = $email,
      password = crypto::argon2::generate($password)
  };
  signin: {
    select * from user
      where email = $email
      and crypto::argon2::compare(password, $password)
  };
};
```

No trailing `;` inside those captures — the clause's value must be
the returned result. `$email` and `$password` are what the client
sends to the `SIGNUP`/`SIGNIN` endpoints.

A post-auth check can inspect the signed-in record and reject with
`throw`:

```sdl
namespace acme;
database shop;

table user {
  schemafull;
  field email: string;
  field blocked: bool;
};

access gated {
  on: database;
  type: record;
  signin: select * from user where email = $email;
  authenticate: {
    if $auth.blocked { throw 'account suspended'; }
  };
};
```

## JWT access: verify tokens from somewhere else

The `jwt` kind does not issue anything (unless given an issuer) — it
*verifies* tokens. The verification source is either a JWKS `url:`
or an `algorithm:` + `key:` pair:

```sdl
namespace acme;
database shop;

/// Tokens issued by Acme SSO.
access corp_sso {
  on: database;
  type: jwt;
  url: 'https://sso.acme.com/.well-known/jwks.json';
};

access ops {
  on: root;
  type: jwt;
  algorithm: es256;
  key: 'ops_public_key_pem';
  duration { session: 8h; };
};
```

One rule governs which keys you may write:

- **Asymmetric verify keys** (`es*`, `eddsa`, `rs*`, `ps*`) are
  *public* keys — freely authorable, printed verbatim by the database.
- **Symmetric verify keys** (`hs*`) and **every issuer key** are
  secrets. The database redacts them in `INFO`, and Surform accepts
  exactly one spelling there: the literal `'[REDACTED]'`. Writing a
  real key in those slots is a parse error:

```sdl-error expect=SDL-E1056
namespace acme;
database shop;

access broken {
  on: database;
  type: jwt;
  algorithm: hs256;
  key: 'SUp3r-s3cret-key-material';
};
```

That is deliberate: your schema file lives in git, so secret material
must be provisioned outside it — that access class is applied
manually, once.

### Issuers and audiences

An `issuer` block turns the access into a token *issuer* alongside
verification. Its key follows the same sentinel rule; its algorithm
must equal the verify algorithm when verifying by local key:

```sdl
namespace acme;
database shop;

access partner_api {
  on: database;
  type: jwt;
  algorithm: es256;
  key: 'partners_public_key';
  issuer {
    key: '[REDACTED]';
  };
  duration { token: 1h; };
};
```

`audience:` (SurrealDB 3.3) — a comma list of strings the token's
`aud` claim must intersect:

```sdl
namespace acme;
database shop;

access corp_sso {
  on: database;
  type: jwt;
  url: 'https://sso.acme.com/.well-known/jwks.json';
  audience: 'shop-web', 'shop-mobile';
};
```

## Bearer access: long-lived grants

A bearer access hands out grant tokens — for `user` accounts (any
level) or `record` users (database level). Bearer accesses can also
issue their own JWTs with a `jwt` block:

```sdl
namespace acme;
database shop;

access ci_tokens {
  on: database;
  type: bearer;
  for: user;
  duration {
    grant: 30d;
    token: 1d;
  };
};

access device_keys {
  on: database;
  type: bearer;
  for: record;
  jwt {
    algorithm: hs256;
    key: '[REDACTED]';
  };
  duration { token: 365d; };
};
```

(`365d` prints as `1y` — SurrealDB's duration canon; the value is
unchanged.)

## Context: data for the session (SurrealDB 3.3)

`context:` evaluates at authentication and becomes `$session.data` —
a way to pin per-session facts your queries can read. It requires an
access method that authenticates a *record* (a record access, or a
bearer for records), and the expression must be read-only:

```sdl
namespace acme;
database shop;

table user {
  schemafull;
  field email: string;
  field plan: string;
};

access account_ctx {
  on: database;
  type: record;
  signin: select * from user where email = $email;
  authenticate: {
    if $auth.blocked { throw 'account suspended'; }
  };
  context: { plan: $auth.plan };
};
```

## Duration defaults

The three duration sides default to grant `30d`, token `1h`, session
unlimited — and an access without a `duration` block round-trips to
one without. A verify-only access (no issuer) can't issue
tokens, so a declared `token:` side there is dropped by the engine —
[the linter](lint.html) catches it (`SDL-L0011`).

## Where next

- [Users & roles](users.html) — the sysadmin accounts.
- [The linter](lint.html) — the duration-capability and sentinel
  warnings.
