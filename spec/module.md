# DEFINE MODULE

Global conventions and notation: [spec/README.md](README.md).

Ground truth: the **3.3.0 parser** (vendored `surrealdb-syn` — the
module grammar is real there, gated at parse time by the surrealism
*capability*; the 3.2.4 cargo-feature bail arm is gone). SDL's grammar
was authored against this form ahead of the engine floor (2026-09-18
decision); since the 2026-09-25 floor move (README design rule 11)
that form **is** the sole supported target — there is no other
spelling to choose, and the parse oracle now covers module emissions
(`tests/oracle_smoke.rs::module_ddl_parses_under_the_capability_gate`).

**Capability-gated (experimental):** SDL accepts the statement; the
server rejects it at define time without the surrealism capability
(`--allow-experimental=surrealism`; the `surreal sql` CLI needs the
matching `SURREAL_CAPS_ALLOW_EXPERIMENTAL` env since it parses
locally). 3.3.0 gate messages, probe-pinned (2026-09-25): plain parse
— `Use of `DEFINE MODULE` requires Surrealism modules`; the embedded
engine's query path (default capabilities) rejects identically, so a
capabilities-off capture never carries a module entry (issue #5) —
but a live server's capture DOES (issue #61): the
engine stores a named module under `mod::<name>` (the catalog's
`get_storage_name` derivation, source-verified), so the INFO key
carries the prefix while the definition's header — and SDL — spell
the bare name (upstream's parser eats `mod::` as grammar). The
reverse reader pairs either spelling and records the SDL-side bare
name, so a post-registration capture of a NAMED FILE module diffs
and merges like any other (the pairing covers the `mod::<name>`
storage family only; an anonymous-silo registration's raw
`silo::org::pkg::<maj.min.patch>` storage key refuses the entire
scope read — ledgered at #96). The capabilities-on parse oracle
accepts both executable forms (the printer materializes
`PERMISSIONS FULL` on the bare shapes; the permissions arm round-trips
byte-identical) through `ToSql` (the printer materializes
`PERMISSIONS` on modules — SDL's absent-permissions elision is
emission-side, rule 9).

**Historical version drift (superseded by the floor move):** the
3.2.4 parser spelled `DEFINE MODULE
[mod::<name> AS] (f"<file>" | silo::<org>::<pkg><maj.min.patch>)
[COMMENT] [PERMISSIONS]` — no `UNSIGNED`, `AS` instead of `FROM`,
no `::` before the silo version, and a lexer-broken silo version
literal there (adjacent integers merge into a float — verified). The
former `--target 3.2` downgrade machinery (SDL-E2001/E2002) retired
with the floor; see the README target-version policy.

```rust
struct Module {
  name: Option<Ident>,                // required for file executables
                                       //   (3.2.4 rejects storing
                                       //   anonymous file modules —
                                       //   verified); optional/redundant
                                       //   for silo (self-named) — and
                                       //   the documented 3.3 silo form
                                       //   takes no alias, so SDL rejects
                                       //   name+silo
  executable: Executable,             // File(f-string) | Silo { org, pkg, ver }
  unsigned: bool,                     // required flag in SDL (3.3)
  permissions: Permission,            // single-value; default full;
                                       //   exists in the parser,
                                       //   undocumented in the reference
  comment: Option<DocComment>,        // from ///
}
```

```
/// <doc comment>                        // optional — emitted as COMMENT
module [<name>] {
  // required — exactly one executable (SDL-lexed literals, per the
  // global keyword-clause grammar; neither is a SurrealQL expression):
  from: f'<path>';                      // surrealism file — f-string
                                         //   literal (f' or f"); name
                                         //   REQUIRED on the statement
  from: silo::<org>::<package>::<maj.min.patch>;
                                         // silo package — u32 triple;
                                         //   self-named: no <name>

  // required flag:
  unsigned;                             // no signature; only silo-published
                                         //   packages are signed

  // optional:
  permissions: none | full | <expr>;
};
```

Emit (3.3): `DEFINE MODULE [mod::<name> FROM] (f"<path>" |
silo::<org>::<pkg>::<ver>) UNSIGNED [PERMISSIONS …] [COMMENT …];`

## Reverse parser (INFO → SDL)

SDL owns module DDL parsing — the embedded engine's query path parses
with default capabilities, so `DEFINE MODULE` cannot execute there and
a capabilities-off capture carries no module entry (README item 4;
issue #5); a live server's capture does, keyed by the storage name
`mod::<name>` while the definition's header spells the bare name
(above, issue #61) — the reverse parser pairs either spelling and
records the SDL-side bare name. It accepts **both
dialects**: the 3.3 form above and the 3.2 `[mod::<name> AS]
(f"<file>" | silo::<org>::<pkg><1.2.0>)` (no `::` before the version,
no UNSIGNED — an absent UNSIGNED maps to `unsigned;` true, since 3.2
modules are implicitly unsigned). INFO (3.2) always appends
`PERMISSIONS …` (default FULL — elide) and uses the `AS` form; modules
are listed by `INFO FOR DB` (the `modules` map); name segments and
f-strings normalize per the general identifier/quote rules. The
version triple arrives as `<1.2.0>` in 3.2 output (lexer-broken shape
included verbatim by the catalog) — parse the digits tolerantly.
Round-trip asymmetry: a 3.2 server may hold `mod::x AS silo::…`
(name+silo), which SDL's grammar rejects — route to `raw`.

Empirically verified on 3.2.3 (capability on): gate message ✅; named
file module + `COMMENT` + `PERMISSIONS NONE` ✅ (round-trips as the
`AS` form); anonymous file module parses but is rejected at execution
(`A module without a name cannot be stored`) ✅; silo version literal
lexer bug ✅. The 3.3.0 floor move (2026-09-25) live-verified the
3.3 forms at the parse level (capability-gated: gate message, both
executable forms, the permissions arm — byte-identical `ToSql`
round-trip on the permissions shape; `tests/oracle_smoke.rs`); the
INFO-side shapes remain
spec-derived (no module can reach INFO on the embedded engine).

## Examples (one per clause)

local surrealism file + doc comment:

```sdl no-exec
/// Colour helpers
module color {
  from: f'modules:/color.surli';
  unsigned;
};
```

```surql
DEFINE MODULE mod::color FROM f"modules:/color.surli" UNSIGNED
  COMMENT 'Colour helpers';
```

silo-published package (self-named — no local name):

```sdl no-exec
module {
  from: silo::acme::utils::<1.2.0>;
  unsigned;
};
```

```surql
DEFINE MODULE silo::acme::utils::<1.2.0> UNSIGNED;
```

permissions (undocumented clause, present in the parser):

```sdl no-exec
module admin_tools {
  from: f'modules:/admin.surli';
  unsigned;
  permissions: $auth.admin = true;
};
```

```surql
DEFINE MODULE mod::admin_tools FROM f"modules:/admin.surli" UNSIGNED
  PERMISSIONS WHERE $auth.admin = true;
```
