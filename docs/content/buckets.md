Buckets are named file-storage locations — SurrealDB's
experimental **files** feature. The statement is accepted by SDL
always; the *server* gates it at define time unless the experimental
`files` capability is enabled (`surreal start
--allow-experimental=files`).

## The shape

```sdl
namespace acme;
database shop;

/// Ephemeral test uploads.
bucket scratch_uploads {
  backend: 'memory';
};
```

`backend:` names where files land — `'memory'` for ephemeral, or a
filesystem path like `'file:/srv/uploads'`. A filesystem backend is
checked against the server's runtime bucket-folder allowlist at
define time, so this one carries the *compiles clean* badge — the
path exists only on a server configured to allow it:

```sdl no-exec
namespace acme;
database shop;

bucket user_files {
  backend: 'file:/srv/uploads';
};
```

## Flags and permissions

`readonly;` freezes a bucket (reports stay, uploads don't), and the
permission expression sees `$action`, `$file`, and `$target`:

```sdl
namespace acme;
database shop;

bucket published_reports {
  backend: 'memory';
  readonly;
};

bucket admin_dumps {
  backend: 'memory';
  permissions: $auth.admin = true;
};
```

## No backend? You need a global bucket

A bucket with no `backend:` inherits the server's *global* bucket —
and defining one on a server that has none configured is a
define-time error. The file compiles fine (this shape is
parse-verified, not applied):

```sdl no-exec
namespace acme;
database shop;

bucket shared;
```

On a server with a global bucket configured, that one-liner is all a
shared drop-zone needs.

## Where next

- [Config](config.html) — the other singleton-style statements.
- [Types (kinds)](types.html) — the `file<T>` kind files flow
  through.
