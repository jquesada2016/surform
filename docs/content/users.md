Users are the accounts that sign in to SurrealDB itself — sysadmins,
service accounts, analysts — each at a level (root, namespace, or
database) with roles. Your application's end users are *not* these;
they are [record users](access.html).

## The shape

```sdl
namespace acme;
database shop;

/// CI deployment account.
user deployer {
  on: namespace;
  passhash: '$argon2id$v=19$…';
  roles: editor, viewer;
  duration {
    token: 1h;
    session: 12h;
  };
};
```

`on:` is required and sets the level: `root`, `namespace`, or
`database` (short spellings `kv`/`ns`/`db` are accepted). A root
user connects without choosing a namespace; a database user must.

## Credentials and the re-apply rule

Three credential slots exist:

- **`passhash:`** — a pre-computed argon2 hash. **Preferred**: your
  password never appears in the file, so it can live in git.
- **`password:`** — plaintext. It works, but the linter warns every
  time (`SDL-L0002`) because the file is now carrying a live secret.
- **`passscram:`** — a SCRAM-SHA-256 verifier (SurrealDB 3.3).
  Additive with the pass clauses — an explicit verifier wins over the
  password-derived one. Generate it with a SCRAM-capable client; the
  compiler shape-checks the string at parse time and rejects garbage
  before the database ever sees it.

The rule: **how you re-apply depends on whether the user carries a
secret.** A user with `password:`, `passhash:`, or `passscram:` is
re-applied with `OVERWRITE` — it converges like everything else. A
user with *no* secret is define-once (`IF NOT EXISTS` in the emitted
DDL): re-applying a secret-less definition would otherwise wipe the
stored credential and brick the account:

```sdl
namespace acme;
database shop;

user analyst {
  on: database;
  roles: viewer;
};
```

The emitted DDL carries `IF NOT EXISTS` instead of `OVERWRITE`. The
linter still flags the secret-less user (`SDL-L0003`) — usually it is
a placeholder you meant to fill in.

## Provisioned SCRAM verifiers

The verifier format is
`SCRAM-SHA-256$<iterations>:<salt>$<stored key>:<server key>` with
standard-base64 parts (the keys are exactly 32 bytes → 44 base64
chars). This is SurrealDB's own known-answer verifier — its password
is `pencil` — handy for smoke tests:

```sdl
namespace acme;
database shop;

user migrator {
  on: database;
  passhash: '$argon2id$v=19$m=19456,t=2,p=1$abc';
  passscram: 'SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=';
};
```

One caution in the other direction: `'[REDACTED]'` pasted back from a
database dump is rejected at parse time — the sentinel is a redaction
marker, not a usable verifier:

```sdl-error expect=SDL-E1059
namespace acme;
database shop;

user migrator {
  on: database;
  passscram: '[REDACTED]';
};
```

## Roles and durations

Roles are `owner`, `editor`, `viewer` (the default) — rendered
uppercase in the DDL. Two durations bound a sign-in: how long a
generated token lives (`token:`, default `1h`) and how long the
session may last (`session:`, default unlimited — `none`).

One operational note: every executed `DEFINE USER` — including a
re-apply — rotates the account's internal session code and signs out
its existing sessions. The database gives no warning when this
happens; avoid re-applying mid-session.

## Where next

- [Access (sign-in & tokens)](access.html) — your end users'
  authentication, which is a different statement entirely.
- [The linter](lint.html) — the plaintext-password and secret-less
  warnings.
