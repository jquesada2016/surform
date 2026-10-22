# DEFINE DATABASE

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the 3.3.0 parser (the `=3.3.0` embed — the 2026-09-25 floor move; originally pinned against v3.2.4) — parser (`parse_define_database`) +
`parts.rs` (`parse_changefeed`) and the emitters
(`sql/statements/define/database.rs` and `catalog/database.rs` — INFO
output shape). Cross-checked against a local in-memory 3.2.3 server
(2026-09-18); the floor-move corpus re-runs on the 3.3.0 embed (2026-09-25). Clause order free; no discriminant. A database binds to
the nearest preceding `namespace` statement in the SDL file; codegen
emits `DEFINE DATABASE` followed by `USE NS <ns> DB <db>;` (codegen
model item 1).

**`STRICT` decoded (verified from source and live):** the catalog
*stores* `strict`, but `INFO FOR NS` rebuilds the definition without
it — strict is active while absent from round-trips. Two consequences:
retro-apply **preserves** `strict;` from SDL (it can never be adopted,
removed, or converged from INFO), and the differ is blind to
server-side strict drift (a `strict` toggled outside SDL is invisible
— SDL re-apply enforces the SDL value). Do not use
`INFO … STRUCTURE` as the normalizer's source either — it drops the
changefeed as well.

```rust
struct Database {
  name: Ident,                          // SurrealQL identifier rules
  strict: bool,                         // undefined tables error instead of
                                        //   being implicitly created
  changefeed: Option<ChangeFeed>,       // { expiry: Duration,
  comment: Option<DocComment>,          //   include_original: bool }
}                                       // from ///
```

```
/// <doc comment>                        // optional — emitted as COMMENT
database <name> {
  // optional:
  strict;                              // flag — accessing an undefined table
                                       //   errors ('The table does not
                                       //   exist') instead of implicitly
                                       //   creating it
  changefeed {
    expiry: <duration>;                // required inside the sub-block;
                                       //   SurrealQL duration literal
    include original;                  // flag — also store pre-change records
  };
};
```

Emit: `DEFINE DATABASE <name> [CHANGEFEED <duration> [INCLUDE
ORIGINAL]] [STRICT] [COMMENT …];` (`OVERWRITE` implied per the codegen
model; the server's own canonical order differs — STRICT, COMMENT,
CHANGEFEED — both parse; the differ compares resolved state; the
active `strict;` flag never drifts — INFO cannot report it).

## Reverse parser (INFO → SDL)

Input contract: INFO emits the plain `DEFINE` (no kind prefix; strip
`OVERWRITE`/`IF NOT EXISTS` if hand-authored input carries them) with
the `STRICT` omission described above; changefeed durations may be
re-decomposed into other units (compare parsed duration values, never
spelling). `COMMENT '…'` maps back to `///` lines. Canonical SDL
elides nothing here (no defaults materialize beyond the comment
absence).

Empirically verified on the 3.3.0 embed (2026-09-25 floor move; first verified in-memory 3.2.3, 2026-09-18): `STRICT` active — `CREATE` on
an undeclared table errors ✅ while a non-strict database implicitly
creates it ✅; `CHANGEFEED 1h INCLUDE ORIGINAL` ✅; the INFO round-trip
drops `STRICT` while it is active ✅.

Duration literal findings (verified against the v3.2.4 source,
pinned by tests): the lexed shape is integer digits (`_` separators
stripped; a leading `_` lexes as an identifier, not digits) + unit
from `ns us µs ms s m h d w y`, integers only, compounds glued
(`1h30m`; `1h 30m` is two tokens and rejected in an SDL duration
clause); arithmetic is checked u64 (a year is 365 days), overflow is
an error. Canonical emission goes through the server's own printer:
the stored value prints as the greedy `y w d h m s ms µs ns`
decomposition with zero as `0ns` — so SDL's `expiry: 7d` emits
`CHANGEFEED 1w` (value-preserving re-decomposition, the same
never-compare-spelling rule the reverse parser applies).

## Examples (one per clause)

plain + doc comment:

```sdl
/// Primary application database
database blog;
```

```surql
DEFINE DATABASE blog COMMENT 'Primary application database';
```

strict:

```sdl
database blog {
  strict;
};
```

```surql
DEFINE DATABASE blog STRICT;
```

changefeed (expiry required; include original optional):

```sdl
database blog {
  changefeed {
    expiry: 7d;
    include original;
  };
};
```

```surql
DEFINE DATABASE blog CHANGEFEED 1w INCLUDE ORIGINAL;
```

(SDL's `7d` stores one week of seconds; canonical emission prints the
server's own value decomposition — `1w` — per the duration findings
above. The value, never the spelling, is what round-trips.)
