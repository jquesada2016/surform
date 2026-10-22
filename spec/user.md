# DEFINE USER

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_user`),
credential validation (`surrealdb-iam`'s `ScramCredential` — the
PASSSCRAM parse-time check), executor
(`expr/statements/define/user.rs`), emitter
(`sql/statements/define/user.rs` + `catalog/schema/user.rs` — INFO
output shape). Cross-checked against a local in-memory 3.2.3 server
(2026-09-18).

**User secrets protocol (verified hazards — the codegen model's item 4
spells out the global rules):**

- `INFO` round-trips the secret as `PASSHASH '[REDACTED]'` — and that
  literal *parses fine as DDL*, so no tooling path may ever echo INFO
  output back into SDL or DDL (SDL is the sole source of truth for
  secrets; prefer `passhash:` with a pre-computed argon2 hash, or
  codegen-time env substitution — the linter warns on plaintext
  `password:` SDL-L0002, and on a sentinel-valued secret about to be
  applied SDL-L0013).
- The 3.3 `PASSSCRAM` clause (below) is a secret under the same
  protocol, with one sharper corner: its INFO print
  (`PASSSCRAM '[REDACTED]'` — presence-signalled whenever a SCRAM
  verifier is stored, e.g. for every `password:` user) is
  **define-rejected** — upstream validates the verifier at DDL parse —
  so the clause can never echo back at all (the reverse reader strips
  it before validation, and an SDL `passscram: '[REDACTED]'` spelling
  is itself rejected at parse — SDL-E1059 names the trap; there is no
  sentinel-apply lint for passscram: no apply could ever work).
- `DEFINE USER OVERWRITE` **without** a pass clause clears the existing
  credential (`PASSWORD 'x'` → `PASSHASH ''` — the account is bricked;
  INFO likewise shows `PASSHASH ''` for secret-less users: empty is
  not redacted). A `passscram:`-only user keeps a working credential
  through re-apply (the verifier updates; the empty hash is probed
  harmless) — so scram-carrying counts as secret-carrying below.
- Therefore: **secret-less users are define-once** — codegen emits
  `IF NOT EXISTS` for them (roles/durations don't converge; the
  linter flags them SDL-L0003; retro-apply preserves the SDL
  roles/durations rather than adopting INFO's). Users carrying an
  SDL secret — `password:`, `passhash:`, or `passscram:` — converge
  normally with `OVERWRITE`.

Always-materialized in INFO (adopt + elide at default): `ROLES VIEWER`
(default role, uppercased), `DURATION FOR TOKEN 1h` (default) and
`FOR SESSION none` (both durations always printed; `PASSWORD` never
appears — always `PASSHASH`).

Level rules (documented): root owners may define users at any level;
namespace owners at namespace/database; database owners at database
only. Record users are not created with this statement (see
[access.md](access.md)).

```rust
struct User {
  name: Ident,
  on: Level,                           // root|kv | namespace|ns | database|db
  pass: Option<Pass>,                  // Password(str) | Hash(str) — mutually
                                       //   exclusive (parser-enforced)
  scram: Option<String>,               // passscram — ADDITIVE with pass
  roles: Vec<Role>,                    // owner | editor | viewer; default
                                       //   viewer; duplicates accepted
  token_duration: Option<Expr>,        // default 1h
  session_duration: Option<Expr>,      // default none
  comment: Option<DocComment>,         // from ///
}
```

```
/// <doc comment>                        // optional — emitted as COMMENT
user <name> {
  // required:
  on: root | kv | namespace | ns | database | db;

  // optional — at most ONE of:
  password: <string>;                   // plaintext — linter warns
  passhash: <string>;                   // pre-computed argon2 hash — preferred

  // optional — additive with the pass clauses (upstream stores it in
  // its own slot; an explicit verifier wins over the password-derived
  // one):
  passscram: <verifier>;                // SCRAM-SHA-256 verifier — shape-
                                        //   validated at parse (SDL-E1059)

  roles: owner | editor | viewer, …;    // comma list; default viewer

  duration {
    token: <expr>;                      // partial blocks emit only the
    session: <expr>;                    //   present sides
  };
};
```

`passscram:` takes a SCRAM-SHA-256 verifier string —
`SCRAM-SHA-256$<iterations>:<salt>$<stored key>:<server key>` with
standard-base64 parts, iterations ∈ [4096, 2²⁴], salt ≥ 1 byte, keys
exactly 32 bytes (the keys are then exactly 44 base64 chars; the
salt's length is the producer's choice — 16 bytes → 24 chars for
upstream's generator, the reference producer).
Upstream validates the verifier AT DDL PARSE
(`ScramCredential::from_verifier_string`; a bad value is rejected
before the executor — live-probed 2026-09-25: `'[REDACTED]'` →
"missing mechanism separator", `4095` iterations → "iteration count
out of range (must be between 4096 and 16777216)") — SDL mirrors the
check eagerly at the fold (`SDL-E1059 InvalidPassscram`: accepted SDL
must emit engine-parseable DDL; the kind-grammar precedent). The
mirror is charset+length arithmetic — the bit-canonicality corners a
real base64 decoder also rejects stay apply-time-loud (the engine's
parse is the final gate either way). Handle verifier material as a
secret — it enables server impersonation and offline guessing — and
generate it with a SCRAM-capable client (or upstream's own
credential generator), never by hand.

Emit (canonical order): `DEFINE USER <name> ON <LEVEL> [PASSWORD x |
PASSHASH x] [PASSSCRAM x] [ROLES …] [DURATION FOR TOKEN …, FOR
SESSION …] [COMMENT …];` — `ON <LEVEL>` always emits the canonical
full word (`on: kv;` → `ON ROOT`, `on: ns;` → `ON NAMESPACE` — the
aliases are input-only lexer conveniences) — with `OVERWRITE` for
secret-carrying users (pass or passscram) and `IF NOT EXISTS`
(between `USER` and the name) for secret-less ones. Role atoms render
uppercased. Note (operational): every executed `DEFINE USER` —
including `OVERWRITE` re-apply — generates a fresh internal session
code, invalidating existing sessions; invisible to INFO.

## Reverse parser (INFO → SDL)

Sources per level: `INFO FOR ROOT` / `INFO FOR NS` / `INFO FOR DB`
(each has a `users` map; the same name may exist at several levels —
re-attach `on:` by source). Secrets never round-trip (`[REDACTED]` for
set ones, `''` for empty — preserve the SDL value / absence, differ
skips). The 3.3 `PASSSCRAM '[REDACTED]'` clause strips at read before
validation (the sentinel is define-rejected — it can never survive
the canonicalizing parse; a non-sentinel PASSSCRAM spelling is
printer drift and fails loud). Adopt-and-elide the materialized
defaults listed above.
Define-once users: preserve SDL `roles:`/`duration` (no adoption).

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): passhash + multi-role +
durations ✅; `PASSWORD` + `PASSHASH` together → parse error ✅
(`Can't set both a passhash and a password`); unknown role →
`expected an existent role` ✅ (hardcoded enum); INFO redaction ✅;
OVERWRITE-without-pass clearing the credential ✅. PASSSCRAM probes
(2026-09-25): verifier-only, password+verifier, and
passhash+verifier all define ✅ (INFO: `PASSSCRAM '[REDACTED]'`, the
empty hash left un-redacted); OVERWRITE re-apply with the verifier
only ✅ (hash clears, the credential survives); bad verifiers are
parse-rejected ✅ (mechanism/iterations/messages above).

## Examples (one per clause)

passhash + roles + duration + doc comment:

```sdl
/// CI deployment account
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

```surql
DEFINE USER deployer ON NAMESPACE PASSHASH '$argon2id$v=19$…'
  ROLES EDITOR, VIEWER DURATION FOR TOKEN 1h, FOR SESSION 12h
  COMMENT 'CI deployment account';
```

root owner with password (linter warns on plaintext):

```sdl
user admin {
  on: root;
  password: 'change-me-immediately';
  roles: owner;
};
```

```surql
DEFINE USER admin ON ROOT PASSWORD 'change-me-immediately' ROLES OWNER;
```

database viewer, defaults only — define-once (no secret ⇒ `IF NOT
EXISTS` at codegen, linter flags it):

```sdl
user analyst {
  on: database;
};
```

```surql
DEFINE USER IF NOT EXISTS analyst ON DATABASE;
```

provisioned SCRAM verifier (additive — passhash coexists; the INFO
print redacts both and the reader strips the scram clause):

```sdl
user migrator {
  on: database;
  passhash: '$argon2id$v=19$m=19456,t=2,p=1$abc';
  passscram: 'SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=';
};
```

```surql
DEFINE USER OVERWRITE migrator ON DATABASE
  PASSHASH '$argon2id$v=19$m=19456,t=2,p=1$abc'
  PASSSCRAM 'SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=';
```
