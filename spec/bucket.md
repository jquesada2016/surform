# DEFINE BUCKET

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_bucket`) and
emitter (`sql/statements/define/bucket.rs`, whose canonical clause
order is READONLY before BACKEND). Cross-checked against a local
in-memory 3.2.3 server (2026-09-18); the floor move re-runs the corpus on the 3.3.0 embed (2026-09-25). Clause order free; no
discriminant.

**Capability-gated (experimental):** SDL accepts the statement; the
server rejects it at define time unless the experimental `files`
capability is enabled (`surreal start --allow-experimental=files`, or
`SURREAL_CAPS_ALLOW_EXPERIMENTAL=files`) — and, when using the
`surreal sql` CLI, on the client too (the CLI parses locally). Runtime
extras: `file:` backends must appear in the runtime bucket-folder
allowlist; buckets without an explicit `backend:` require a global
bucket configured on the server. Verified against the v3.2.4 source:
the gate sits *inside the parser* — `parse_define_bucket`
rejects the statement unless the parser's `files_enabled` setting
holds — so any local parse of bucket DDL (expression
canonicalization, oracle, goldens) must enable the capability
(`parse_with_capabilities`); and the global-bucket rule fires at
define time in the embedded engine ("No global bucket has been
configured" for a clause-less bucket).

```rust
struct Bucket {
  name: Ident,
  backend: Option<Expr>,             // expression clause — any SurrealQL
                                     //   expression; 'memory' and 'file:/…'
                                     //   are the conventional literals;
                                     //   None → server's global bucket
  readonly: bool,
  permissions: Permission,           // single-value; default full — INFO
                                     //   materializes PERMISSIONS FULL
  comment: Option<DocComment>,       // from ///
}
```

```
/// <doc comment>                        // optional — emitted as COMMENT
bucket <name> {
  // optional (all clauses):
  backend: <expr>;                       // e.g. 'memory' | 'file:/srv/uploads'
  readonly;                              // flag
  permissions: none | full | <expr>;     // expr sees $action, $file, $target
};
```

Emit (canonical order; `OVERWRITE` implied per the codegen model):
`DEFINE BUCKET <name> [READONLY] [BACKEND <expr>] [PERMISSIONS …]
[COMMENT …];`

## Reverse parser (INFO → SDL)

INFO emits the plain `DEFINE` in the canonical order above with
`PERMISSIONS` **always** rendered (default `FULL` — elide at default
per the canonical-minimal rule). `COMMENT '…'` maps back to `///`.
Kind prefixes (`OVERWRITE`/`IF NOT EXISTS`), should they appear in
hand-authored input, are stripped. Nothing else materializes.

Empirically verified (in-memory 3.2.3, files capability on): `memory`
backend ✅ (`INFO` round-trip `DEFINE BUCKET b1 BACKEND 'memory'
PERMISSIONS FULL`); `file:/path` parses ✅ (runtime allowlist error as
expected); `READONLY` + `PERMISSIONS WHERE <expr>` parse ✅; clause-less
bucket parses ✅ (runtime 'No global bucket has been configured' until
one exists); bare permissions expression rejected ✅ (`expected
'NONE', 'FULL', or 'WHERE'`).

## Examples (one per clause)

memory backend + doc comment:

```sdl no-exec
/// Ephemeral test uploads
bucket scratch_uploads {
  backend: 'memory';
};
```

```surql
DEFINE BUCKET scratch_uploads BACKEND 'memory' COMMENT 'Ephemeral test uploads';
```

file backend (allowlisted at runtime):

```sdl no-exec
bucket user_files {
  backend: 'file:/srv/uploads';
};
```

readonly flag:

```sdl no-exec
bucket published_reports {
  backend: 'memory';
  readonly;
};
```

```surql
DEFINE BUCKET published_reports READONLY BACKEND 'memory';
```

permissions (single-value form — `$action`, `$file`, `$target`
available in the expression):

```sdl no-exec
bucket admin_dumps {
  permissions: $auth.admin = true;
};
```

```surql
DEFINE BUCKET admin_dumps PERMISSIONS WHERE $auth.admin = true;
```

no clauses — inherits the server's global bucket:

```sdl no-exec
bucket shared;
```
