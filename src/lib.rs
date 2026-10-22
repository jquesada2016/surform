//! `surform` — a declarative Schema Definition Language (SDL)
//! that compiles to SurrealQL DDL for SurrealDB v3, and back
//! (DDL→SDL) for the INFO round-trip.
//!
//! Module map:
//!
//! - [`ast`] — SDL's own statement AST, one type per DEFINE statement
//! - [`lexer`] — hand-written SDL lexer: depth-0 `;` statement
//!   splitting, capture-mode spans, the full skip set (strings,
//!   comments, quoted identifiers)
//! - [`parser`] — hand-written recursive descent, one clause-loop per
//!   statement
//! - [`kind`] — the SDL kind grammar (README rule 4): text-in/text-out,
//!   mirroring v3.2.4's kind parser and carrying source spellings
//! - [`load`] — the composition loader (spec/include.md): the file
//!   graph from the root the CLI was given, spliced into the
//!   flattened document (flatten(root) ≡ root) with its file map
//! - `expr` — the embedded-expression bridge: capture → canonical
//!   DDL text via wrapper-statement parse + `ToSql` slice (private)
//! - [`diag`] — the `Diag` enum, `SDL-Exxxx` codes, miette wiring
//! - [`expand`] — the mixin lowering pass (SDL→SDL): spreads expand to
//!   their members before emission; fmt never lowers
//! - [`codegen`] — SDL→DDL emission; expression clauses re-printed
//!   from the embedded official parser's AST
//! - [`info`] — DDL→SDL normalization for the INFO round-trip
//! - [`lint`] — the advisory linter: `SDL-Lxxxx` findings over the
//!   resolved state
//! - [`migrate`] — the migration pipeline: the saved
//!   drafts' format (numbering, header, the slot resolution
//!   markers), `commit`'s hash + ledger statements, `push`'s HTTP
//!   `/sql` apply loop, `status`'s ledger-vs-directory classification,
//!   `revert`'s stored-state draft, and the in-repo SHA-256, base64,
//!   and JSON readers the pipeline needs
//! - [`order`] — the declarative namespace's semantics: the frozen
//!   define-time edge table, the duplicate/cycle checks (E1062/E0015),
//!   and the compiler-owned emission order (the stable topo sort
//!   `build` emits under)
//! - [`refs`] — the reference extractor: every
//!   table/fn/param/analyzer/mixin reference with its owning
//!   statement, plus dynamic mutes; emits no diagnostics (the linter
//!   consumes it)
//! - [`writeback`] — the write-back distribution (spec/include.md):
//!   `normalize` on a composition root returns every merged statement
//!   to the file that authored it
//! - `pretty` — the expression layout pass: canonical expression text
//!   re-wrapped across lines at the writers when it exceeds the width
//!   budget (private)
//!
//! The embedded official parser is `surrealdb_core::syn`
//! (`=3.3.0`), used for expression clauses and reverse parsing.

pub mod ast;
pub mod codegen;
pub mod diag;
pub mod expand;
mod expr;
pub mod info;
pub(crate) mod keywords;
pub(crate) mod kind;
pub mod lexer;
pub mod lint;
pub mod load;
pub mod migrate;
pub mod order;
pub mod parser;
pub(crate) mod pretty;
pub mod refs;
pub mod writeback;

/// This crate's version — the wasm bindings' `version()` reports it
/// alongside [`SURREALQL_VERSION`].
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The SurrealQL grammar generation the compiler targets — mirrors
/// the `=3.3.0` embed pins in Cargo.toml. A floor move is a
/// deliberate spec event that updates the pins and this
/// const together.
pub const SURREALQL_VERSION: &str = "3.3.0";

/// Every keyword spelling the embedded SurrealQL lexer classifies
/// (363 after the floor move, byte-order sorted) — the one source of
/// truth for keyword-aware tooling: the docs site's playground
/// highlights exactly this set, generated from the compiler at build
/// time so highlight and parser cannot drift.
pub fn surrealql_keywords() -> &'static [&'static str] {
    keywords::KEYWORDS_3_3_0
}

pub use diag::Diag;
