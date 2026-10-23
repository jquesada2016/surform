# Contributing to Surform

Thank you for looking at Surform. The repo documents itself — start
here before changing anything:

- [spec/README.md](https://github.com/jquesada2016/surform/blob/main/spec/README.md) — the language specification:
  design rules, grammar notation, permission forms, the codegen and
  normalization model. A behavior change starts as a spec change; the
  spec is the source of truth. One file per `DEFINE` statement sits
  beside it, plus the SDL-only `mixin` and `include`.
- [docs/](https://github.com/jquesada2016/surform/tree/main/docs) — the
  user-facing documentation site
  ([surform.josequesada.dev](https://surform.josequesada.dev));
  [docs/README.md](https://github.com/jquesada2016/surform/blob/main/docs/README.md)
  has the build recipe;
  [editor/](https://github.com/jquesada2016/surform/tree/main/editor) is
  the wasm bindings crate behind the playground;
  [editors/vscode/](https://github.com/jquesada2016/surform/blob/main/editors/vscode/README.md)
  is the VS Code extension.
- [GitHub issues](https://github.com/jquesada2016/surform/issues) —
  known open issues (`fix-me`) and accepted corners (`accepted-corner`).
  Check them before filing a bug.

## How it is built

- The compiler embeds the **official SurrealQL parser** (`surrealdb-core`
  3.3.0, exact-pinned, experimental capabilities enabled) for expression
  clauses and for reading DDL back; the SDL front end itself — lexer,
  recursive-descent parser, diagnostics — is hand-written.
- One emission target: SurrealDB 3.3.
- The pinned 3.3.0 parser source in the cargo registry outranks the
  online docs: no `v3.3.0` git tag exists upstream and public `main`
  lags the shipped binary. Verify SurrealQL behavior against the
  vendored crates
  (`ls ~/.cargo/registry/src/*/surrealdb-{core,syn,sql,rpc}-3.3.0/src`)
  or the embedded engine — never invented, never verified against a
  cloned branch.
- **SurrealDB access is embedded-only**: tests and tools run the
  `surrealdb` crate in embedded mode (in-memory or a local file) — no
  server instances, no sockets.
- Single crate: package, lib, and bin are all `surform`; `docs`,
  `editor`, and the two Surrealism guest crates are workspace members.
  Stable Rust, edition 2024, MSRV 1.94.

## Building and testing

Rust stable, pinned in `rust-toolchain.toml` (MSRV 1.94). The local
gate, run before every commit:

```sh
cargo build                                        # plain — test builds can mask a bare-build break
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

The `--workspace` forms are load-bearing: the plain forms cover the
root package only and silently skip the member crates.

Four testing layers, cheapest first:

1. **`insta` snapshots** — codegen goldens for every statement type
   and rendered error messages.
2. **Combination oracle** (per-commit) — the clause-pair cross product
   per statement type: generate SDL → emit DDL → the embedded parser
   must accept it → DDL→SDL→DDL byte-stable fixed point. The clause
   menus live in one home, `tests/common/menus.rs`;
   `tests/oracle_audit.rs` pins that every generator genuinely varies.
3. **`proptest` panic guard** — random garbage and
   mutated-but-plausible SDL: no panics, every failure a well-formed
   diagnostic.
4. **Embedded-exec smoke** — representative valid DDL must execute
   cleanly through the embedded engine (the parse oracle cannot see
   catalog-level rejections).

Notes:

- Tests that run the embedded SurrealDB engine rely on dependencies
  compiling at `opt-level 3` (configured in the workspace root) —
  never drop that override for speed, and never trade away our own
  crates' debug assertions for it.
- The full cross-product oracle suite (~4,500 documents through
  parse → emit → exec → INFO → read → merge) runs manually, not
  per-commit: `cargo test --test oracle_full -- --ignored
  --test-threads=4`.
- There is no CI; the gate above is the contract.
- The docs site builds with `cargo run -p surform-docs` — a broken
  example anywhere in it is a build error (see docs/README.md).
- Editor-facing artifacts track compiler behavior: a change to the
  spec, codegen, diagnostics, linter, formatter, or reader also
  rebuilds the committed wasm bundle (recipe in docs/README.md) and
  re-runs the VS Code extension's self-checks (`selfcheck.mjs` for
  grammar changes, `fmtselfcheck.mjs` for formatter and diagnostic
  changes — neither rides the cargo gate).

## Code discipline

- No generic `utils`/`helpers` modules — name modules for their
  responsibility.
- Never silence a lint with `#[allow(...)]` where restructuring is the
  fix; warnings are the remaining-work signal.
- Prefer iterator chains over indexed loops where they read better.

## Pull requests

- One coherent change per PR; say what you changed and what you ran.
- SurrealQL behavior must be verified as above and recorded in the
  spec.
- New dependencies are a conversation first. The compiler crate's
  approved set: the exact-pinned `surrealdb-core`/`-rpc`/`-syn`/
  `-sql`/`-types` family, `miette`, `strsim`, `clap`, `ureq`;
  dev-deps `surrealdb`, `tokio`, `insta`, `proptest`, `assert_cmd`.
  The member crates pin their own stacks (the Surrealism guest SDK
  `surrealism`, and `wasm-bindgen` with `serde`/`serde_json` in the
  editor crate) — the same rule there.

## License

By contributing you agree your contributions are available under the
project's licenses, MIT OR Apache-2.0. Note the binary embeds
SurrealDB's parser (BSL 1.1 — see THIRD-PARTY-NOTICES.md).

---

If Surform is useful to you, I'm available for work —
[josequesada.dev](https://josequesada.dev).
