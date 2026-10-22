//! Shared helpers for the integration suites.
//!
//! Layer 2 (combination oracle, per the testing policy): the embedded
//! official parser — `surrealdb_core::syn`, the same code the server
//! runs — is the oracle for anything SDL emits.
//!
//! Each suite compiles this module by `#[path]` and uses a subset of
//! it — the dead-code allow states that structurally (a shared test
//! home, not per-crate dead helpers).

#![allow(dead_code)]

use surrealdb_syn::ParserConfig;

/// The combination oracle's clause menus: one home
/// for every family's cross-product generator, shared by the pair
/// suite, the `#[ignore]` full suite, and the audit below.
pub mod menus;

/// The oracle must accept this DDL.
pub fn assert_oracle_accepts(ddl: &str) {
    surrealdb_core::syn::parse(ddl)
        .unwrap_or_else(|error| panic!("oracle rejected the DDL: {error}\n--- DDL ---\n{ddl}"));
}

/// The oracle must reject this DDL.
pub fn assert_oracle_rejects(ddl: &str) {
    assert!(
        surrealdb_core::syn::parse(ddl).is_err(),
        "oracle accepted the DDL:\n{ddl}"
    );
}

/// The oracle must reject this DDL, even parsing with the
/// experimental capabilities enabled (the reject-side twin of
/// [`assert_oracle_accepts_files`]).
pub fn assert_oracle_rejects_files(ddl: &str) {
    use surrealdb_rpc::capabilities::{Capabilities, ExperimentalTarget, Targets};
    let capabilities =
        Capabilities::none().with_experimental(Targets::Some(std::collections::HashSet::from([
            ExperimentalTarget::Files,
            ExperimentalTarget::Surrealism,
        ])));
    assert!(
        surrealdb_core::syn::parse_with_capabilities(ddl, &capabilities, &ParserConfig::default())
            .is_err(),
        "oracle accepted the DDL:\n{ddl}"
    );
}

/// The oracle must accept this DDL, parsing with the experimental
/// capabilities enabled (`files`, `surrealism`). `DEFINE BUCKET` is
/// gated *inside* the parser, so bucket DDL needs this variant;
/// the construction mirrors `src/expr.rs` (README codegen model item
/// 4: SDL does not model server capabilities).
pub fn assert_oracle_accepts_files(ddl: &str) {
    use surrealdb_rpc::capabilities::{Capabilities, ExperimentalTarget, Targets};
    let capabilities =
        Capabilities::none().with_experimental(Targets::Some(std::collections::HashSet::from([
            ExperimentalTarget::Files,
            ExperimentalTarget::Surrealism,
        ])));
    surrealdb_core::syn::parse_with_capabilities(ddl, &capabilities, &ParserConfig::default())
        .unwrap_or_else(|error| panic!("oracle rejected the DDL: {error}\n--- DDL ---\n{ddl}"));
}

/// The same capabilities-on parse as [`assert_oracle_accepts_files`],
/// returning the embed's own `ToSql` print — the round-trip leg for
/// byte-identity claims.
pub fn oracle_accepts_files_print(ddl: &str) -> String {
    use surrealdb_rpc::capabilities::{Capabilities, ExperimentalTarget, Targets};
    use surrealdb_types::ToSql as _;
    let capabilities =
        Capabilities::none().with_experimental(Targets::Some(std::collections::HashSet::from([
            ExperimentalTarget::Files,
            ExperimentalTarget::Surrealism,
        ])));
    surrealdb_core::syn::parse_with_capabilities(ddl, &capabilities, &ParserConfig::default())
        .unwrap_or_else(|error| panic!("oracle rejected the DDL: {error}\n--- DDL ---\n{ddl}"))
        .to_sql()
}

/// The fmt fixed point, asserted over the oracle
/// suites' full SDL cross products and the fmt corpus: canonical SDL
/// re-parses clean, holds the same definition (the span-blind
/// comparison — carried alias spellings and kind spellings never
/// count), and formats to itself byte for byte.
pub fn assert_fmt_fixed_point(sdl: &str) {
    use surform::info::write::write_canonical;
    use surform::parser::parse_file;

    let (ast, diags) = parse_file(sdl);
    assert!(diags.is_empty(), "{sdl}: {diags:?}");
    let once = write_canonical(&ast);
    let (reparsed, diags) = parse_file(&once);
    assert!(diags.is_empty(), "fmt output stopped parsing:\n{once}");
    assert!(
        ast.same_definition(&reparsed),
        "fmt changed the definition:\n--- SDL ---\n{sdl}\n--- fmt ---\n{once}"
    );
    let twice = write_canonical(&reparsed);
    assert_eq!(twice, once, "fmt is not idempotent:\n{once}");
}

/// A live-captured db interchange object (INFO FOR DB plus the
/// reserved `@tables` child map), the reverse-path proptests'
/// fixture — mutated like the readers' CORPUS fan. (The differ's
/// totality proptest mutates the same
/// interchange the merge proptest does.)
pub const INTERCHANGE_FIXTURE: &str = r#"{
  accesses: {  },
  analyzers: {  },
  apis: {  },
  buckets: {  },
  configs: {  },
  functions: {  },
  modules: {  },
  params: { p: 'DEFINE PARAM $p VALUE 2 PERMISSIONS FULL' },
  sequences: { s: 'DEFINE SEQUENCE s BATCH 1000 START 5' },
  tables: {
    t: 'DEFINE TABLE t TYPE NORMAL SCHEMALESS PERMISSIONS NONE'
  },
  users: {  },
  "@tables": {
    t: {
      events: {  },
      fields: {
        a: 'DEFINE FIELD a ON t TYPE int PERMISSIONS FULL',
        m: 'DEFINE FIELD m ON t TYPE array<int> PERMISSIONS FULL',
        "m.*": 'DEFINE FIELD m.* ON t TYPE int PERMISSIONS FULL'
      },
      indexes: {
        i: 'DEFINE INDEX i ON t FIELDS a UNIQUE'
      },
      lives: {  },
      tables: {  }
    }
  }
}"#;

/// Truncates and splices `edits` into `text` — the readers' CORPUS
/// mutation strategy, shared by the reverse-path proptests.
pub fn corrupt(text: &str, cut: usize, edits: &[(usize, u8)]) -> String {
    let mut bytes = text.as_bytes().to_vec();
    bytes.truncate(cut % (bytes.len() + 1));
    for (at, byte) in edits {
        let at = *at % (bytes.len() + 1);
        bytes.insert(at, *byte);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
