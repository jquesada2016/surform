# DEFINE ACCESS

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser
(`surrealdb/core/src/syn/parser/stmt/define.rs`: `parse_define_access`,
`parse_jwt`) and emitters (`sql/access_type.rs`, which define what `INFO`
renders). Cross-checked against a local in-memory 3.2.3 server
(2026-09-18); the floor-move corpus re-runs on the 3.3.0 embed (2026-09-25). SDL blocks are free-ordered; codegen emits SurrealQL's
canonical order (below). SurrealQL's parser accepts the shared clauses
(`comment`/`type`/`authenticate`/`duration`) in any order; the
positional requirements are only within the type clause (SIGNUP/SIGNIN
precede the `WITH` loop; the verification source precedes `WITH
ISSUER`) — codegen's canonical order satisfies all of them.

`type:` is the discriminant; the SDL AST models ACCESS as:

```rust
enum Access {
  Jwt    { on, source+audience?, issuer?, duration?, authenticate?, context?, comment? },
  Record { on, signup?, signin?, jwt?, refresh?, duration?, authenticate?, context?, comment? },
  Bearer { on, for, jwt?, duration?, authenticate?, context?, comment? },
}
// `field?` = Option<…>; unmarked = required.
// comment? is populated exclusively by the /// doc comment above the
// definition — there is no `comment:` clause.
```

## Variant `type: jwt;`

```
/// <doc comment>                        // optional — emitted as COMMENT
access <name> {
  // required:
  on: root | kv | namespace | ns | database | db;
  type: jwt;
  // verification source — required; `url:` XOR the pair
  // `algorithm:` + `key:`:
  url: <expr>;                            // JWKS endpoint
  algorithm: <algorithm>;                 // local key — <algorithm> is a
  key: <expr>;                            //   dedicated token: eddsa,
                                          //   es256, es384, es512,
                                          //   hs256, hs384, hs512,
                                          //   ps256, ps384, ps512,
                                          //   rs256, rs384, rs512
  // optional:
  issuer {                                // WITH ISSUER — issuing configuration.
    algorithm?: <algorithm>;              //   Compatibility: must equal the
    key: <expr>;                          //   verification algorithm when
  };                                      //   verifying by local key (and is
                                          //   then elided from canonical SDL,
                                          //   being forced-equal); URL-verified
                                          //   accesses accept any algorithm —
                                          //   a DECLARED algorithm must always
                                          //   be emitted, or SurrealDB
                                          //   silently materializes hs512.
                                          //   key: is REQUIRED unless
                                          //   auto-derived (symmetric verify
                                          //   mirrors its key); a key-less
                                          //   issuer materializes a RANDOM key
                                          //   — SDL validates, the linter
                                          //   rejects it.
  duration { grant?: <expr>; token?: <expr>; session?: <expr>; };
  authenticate: <expr>;                   // post-auth check; THROW rejects
};
```

Captured clause bodies render per the global expression-rendering rule:
bare statements print wrapped in `( … )`, braced blocks print as blocks
— both authoring forms are valid. One INFO edge: a verify-only access
(no issuer — explicit or symmetric-mirrored; URL-verified *or*
asymmetric-key-verified) cannot issue tokens, so INFO omits `FOR
TOKEN` entirely — a declared `duration { token: …; }` on such an
access disappears from round-trips (the differ tolerates it; the
linter flags it SDL-L0011). Grant-capable means: bearer accesses
always; record
accesses with `refresh;`; jwt accesses never — `FOR GRANT` appears in
INFO only for those, and grant/token durations on non-capable accesses
are accepted by SurrealQL and silently dropped. The 30d default
renders as `4w2d` in INFO — durations compare by parsed value
(`30d` ≡ `4w2d`), never by spelling. The same value-normalization is
what the embedded printer applies to every duration canon: SDL's `30d`
emits `4w2d`, and `365d` emits `1y` (the server's own greedy
decomposition; both re-parse to the identical value).

### Secret key slots are shape-only (the exposure guard, SDL-E1056)

A symmetric (hs*) verify `key:` and every issuer `key:` are SECRET
MATERIAL — the engine redacts them in INFO and they must be
provisioned outside SDL (this access class is run manually, so its
secrets are never committed). SDL accepts exactly one spelling there:
the literal `'[REDACTED]'` (either quote style — canonical SDL spells
single quotes). Authoring any other expression — a real key, a
parameter, a computed value — fails with SDL-E1056. Asymmetric verify
keys render verbatim in INFO (they are public keys) and stay freely
authorable, as does `url:`. The engine additionally requires a
symmetric issuer key to EQUAL its verify key (upstream rejects
otherwise at define); the sentinel rule satisfies that by
construction. Consequence for the differ: never auto-emit
`DEFINE` for a secret-bearing access whose key slots hold the
sentinel — applying `KEY '[REDACTED]'` would define a garbage key
(issue #6).

## The 3.3 clauses: `audience:` and `context:`

Both landed with SurrealDB 3.3 (probe-pinned live, 2026-09-25).

- **`audience: '<value>', …;`** — a non-empty comma list of string
  literals on the JWT source: the jwt variant's direct clauses and
  record/bearer's `jwt { … }` sub-blocks (membership: the variant
  owners diagnose a misplaced spelling). When present, verification
  requires the token's `aud` claim and an intersection with the list.
  The engine evaluates each value at define, string-checks it
  (`AUDIENCE 'a', 1 + 1` → "Expected `string` but found `2`"), and
  stores strings — SDL authors literals only, the one shape that
  round-trips (the expression-comma-list grammar is upstream's; the
  literal-only restriction is SDL's licensed deviation). DDL print
  position: after the verification source, before `WITH ISSUER`
  (comma-space separated); the INFO print adopts decoded.
- **`context: <expr>;`** — the authorization payload evaluated at
  authentication and exposed as `$session.data`; stored unevaluated
  (INFO re-prints the expression). The engine's define-time shape
  demand is mirrored at the fold: the clause requires an access
  method that authenticates a record — a record access, or a bearer
  for records, at the database level (a jwt spelling dies in the
  membership pass — the owners message; anything else carries the
  engine's "authenticates a record" message). The read-only half of
  the engine's check ("it must be read-only") is semantic — the
  engine's to say, per the standing division of labor.

Emit (canonical order, `OVERWRITE` implied per the codegen model):
`DEFINE ACCESS <name> ON <LEVEL> TYPE <TYPE> [SIGNUP (…)] [SIGNIN (…)]
[WITH REFRESH] [WITH JWT … [AUDIENCE …] [WITH ISSUER …]]
[AUTHENTICATE …] [CONTEXT …] [DURATION …] [COMMENT …];` — clause
bodies are re-printed from the embedded parser's AST, statement-like
values wrapped in `( … )`.

## Variant `type: record;` — database level only (parser-enforced)

```
access <name> {
  // required:
  on: database | db;
  type: record;
  // optional — grammar-optional; typically signin is present, and signup
  // is omitted when accounts are created by db users rather than
  // self-registered:
  signup: <expr>;
  signin: <expr>;
  jwt { <source + optional issuer { … } — same shape and source
         requirement as the jwt variant> };
                                          // issues its own tokens (WITH JWT)
  refresh;                                // WITH REFRESH — refresh-token grants
  duration { grant?: <expr>; token?: <expr>; session?: <expr>; };
  authenticate: <expr>;
};
```

## Variant `type: bearer;`

```
access <name> {
  // required:
  on: root | kv | namespace | ns | database | db;
  type: bearer;
  for: user | record;                     // required; record subject is
                                          //   database-level only (parser-enforced)
  // optional:
  jwt { <source + optional issuer { … } — same shape and source
         requirement as the jwt variant> };
                                          // bearer-issued JWTs (WITH JWT)
  duration { grant?: <expr>; token?: <expr>; session?: <expr>; };
  authenticate: <expr>;
};
```

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): record SIGNIN-only ✅, SIGNUP-only
✅, neither ✅; jwt without a source ❌ (`expected ALGORITHM, or URL`);
bearer without FOR ❌ (`expected FOR`); missing ON ❌ (`expected ON`);
`ON NS`/`ON KV` alias forms parse ✅; AUDIENCE (2026-09-25):
one and two values ✅, the multi-value print `AUDIENCE 'aud1', 'aud2'`
✅, record/bearer `jwt { … }` sub-blocks ✅, a non-string value
define-rejected ✅; CONTEXT: record + bearer-for-record ✅, jwt and
bearer-for-user and root-level rejected (the "authenticates a record"
message) ✅, a non-read-only expression rejected ("it must be
read-only") ✅, INFO prints the payload after AUTHENTICATE, before
DURATION ✅.

## Reverse parser (INFO → SDL)

Verified against the v3.2.4 emitters — three ACCESS-specific hazards:

- **INFO data loss (preserve in SDL):** a bearer access's `jwt { … }`
  clause is stored but never rendered, and an issuer's `algorithm:` is
  never rendered (only `WITH ISSUER KEY`). Both join the normalizer's
  preserve list.
- **`DURATION` always materializes:** `FOR SESSION` is always printed
  (`NONE` when unset), `FOR TOKEN` only when the access can issue
  tokens, `FOR GRANT` only for grant-capable accesses. The
  SurrealQL-side defaults that materialize are **grant 30d, token 1h,
  session none** (verified in `sql/access.rs`) — canonical SDL elides
  any duration clause whose value equals these defaults, so an access
  with no `duration` block round-trips to no `duration` block. (The
  READ side carries the materialized sides faithfully — the elision
  is the normalizer's.)
- Record accesses with no `jwt { … }` round-trip printing the
  auto-issued `WITH JWT ALGORITHM HS512 KEY '[REDACTED]' WITH ISSUER
  KEY '[REDACTED]'`. The reader adopts it through the ordinary
  `WITH JWT` arm and the mirror fold — the read shape is the authored
  sentinel block with the key-less mirrored issuer, which re-emits
  byte-identically. (Pre-redesign the random keys were
  SDL-managed-only; the sentinel rule made the printed signature
  adoptable.) Conversely, when SDL declares an hs* verify key with no
  `issuer { … }`, SurrealQL's implied issuer mirrors that key — the
  printed mirror reads back to the key-less issuer (`issuer.key:
  None`). An explicit `issuer { key: '[REDACTED]'; }` on a symmetric
  key is textually identical to that mirror and reads as it —
  indistinguishable by design (issue #6).
- The issuer's `algorithm:` never renders on either path (live-probed
  even with an authored one on the URL path): the read carries
  absence, and SDL's URL-path emission of a declared algorithm is the
  one emit-more divergence — preserve.
- Root-level accesses are listed by `INFO FOR ROOT` (its `accesses`
  entry).
- SurrealQL permits a TYPE-less definition (implicitly RECORD) — the
  reverse parser accepts it and maps to `type: record;`.
- Note: **INFO redacts ACCESS key secrets** (corrected 2026-09-21
  against the v3.2.4 catalog emitter and the embedded engine — an
  earlier revision of this file claimed otherwise). The catalog's
  only ToSql path builds a redacted copy of the access type
  (`catalog/schema/access.rs`, `to_sql_definition` →
  `AccessType::redacted`): symmetric (hs*) verify keys and every
  issuer key render `[REDACTED]`, while asymmetric verify keys render
  verbatim (they are public keys). SDL's own
  shape-only rule makes `'[REDACTED]'` the only authorable spelling
  in exactly those slots, the sentinel round-trips by construction:
  the read adopts it as the canonical literal and marks the entry
  `Markers::redacted` (the `PASSHASH '[REDACTED]'` precedent). A real
  hs* key remains reachable only through raw SurrealQL (the exec
  smoke pins the engine-side redaction through that spelling).

## Examples (one per clause)

jwt + url + doc comment:

```sdl
/// Tokens issued by Acme SSO
access corp_sso {
  on: database;
  type: jwt;
  url: 'https://sso.acme.com/.well-known/jwks.json';
};
```

```surql
DEFINE ACCESS corp_sso ON DATABASE TYPE JWT
  URL 'https://sso.acme.com/.well-known/jwks.json'
  COMMENT 'Tokens issued by Acme SSO';
```

jwt + algorithm/key + duration (on root):

```sdl
access ops {
  on: root;
  type: jwt;
  algorithm: es256;
  key: 'ops_public_key_pem';
  duration { session: 8h; };
};
```

```surql
DEFINE ACCESS ops ON ROOT TYPE JWT ALGORITHM ES256 KEY 'ops_public_key_pem'
  DURATION FOR SESSION 8h;
```

record + signup/signin (no trailing `;` inside the captures — the value
must be returned):

```sdl
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

```surql
DEFINE ACCESS account ON DATABASE TYPE RECORD
  SIGNUP { CREATE user SET email = $email, password = crypto::argon2::generate($password) }
  SIGNIN { SELECT * FROM user WHERE email = $email AND crypto::argon2::compare(password, $password) };
```

(braced captures print as blocks — the rendering rule above; an
earlier revision of this example showed parenthesized output, which is
the braceless-authoring form's shape.)

inline braceless expression clause:

```sdl
access gated {
  on: database;
  type: record;
  signin: select * from user where email = $email;
  authenticate: {
    if $auth.blocked { throw 'account suspended'; }
  };
};
```

record + refresh + jwt issuance (canonical emit order — SIGNIN before
the WITH clauses):

```sdl
access member {
  on: database;
  type: record;
  refresh;
  signin: { select id from user where email = $email and crypto::argon2::compare(password, $password) };
  jwt {
    algorithm: eddsa;
    key: 'member_signing_key';
    issuer {
      key: '[REDACTED]';
    };
  };
};
```

```surql
DEFINE ACCESS member ON DATABASE TYPE RECORD
  SIGNIN ( SELECT id FROM user WHERE email = $email AND crypto::argon2::compare(password, $password) )
  WITH REFRESH
  WITH JWT ALGORITHM EDDSA KEY 'member_signing_key'
  WITH ISSUER KEY '[REDACTED]';
```

jwt + issuer (asymmetric: verify public — freely authorable — issue
private, shape-only):

```sdl
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

bearer for user + grant duration:

```sdl
access ci_tokens {
  on: database;
  type: bearer;
  for: user;
  duration {
    grant: 30d;
    token: 1d;
  };
};
```

bearer for record (database level only) + bearer-issued JWTs — note
the emission: `TYPE BEARER FOR … WITH JWT …` must carry the `WITH JWT`
clause even though INFO never renders it (verified):

```sdl
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

```surql
DEFINE ACCESS device_keys ON DATABASE TYPE BEARER FOR RECORD
  WITH JWT ALGORITHM HS256 KEY '[REDACTED]'
  DURATION FOR TOKEN 1y;
```

(`365d` canonizes to `1y` — the duration value-normalization above.)

## Open items

`OPENID_CONNECT`/`SAML` are described as "since v3.3.0" in the docs
but have no parser evidence yet (absent from v3.2.4, and not among the
engine-verified 3.3.0 additions); they remain unauthorable in SDL.
The rest of the shipped 3.3 access surface IS authorable: `CONTEXT`
and jwt `AUDIENCE` landed alongside the user surface's
`PASSSCRAM` (2026-09-25; engine-verified).
