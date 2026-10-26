//! The generator vacuity guard (the testing policy's
//! "generators must genuinely vary" rule, formalized): every menu's
//! product count is exact, every doc is distinct (a collapsed
//! generator hides terminals), and every TERMINAL VALUE appears in
//! at least one doc — the fixture lesson (a menu whose values
//! the pipeline silently rejects runs vacuous forever).
//!
//! Cheap by construction (string work only — no parse, no exec), so
//! this rides the per-commit gate.

#[path = "common/mod.rs"]
mod common;

use common::menus;
use std::collections::BTreeSet;

/// The exact product count per family — a dropped loop or a
/// short-circuited generator breaks this before anything else.
/// The exec-eligibility pin rides here too (per-commit): a marker
/// edit that silently grows the skip set changes the eligible
/// count and breaks the pin before the nightly layer runs.
#[test]
fn every_menu_hits_its_exact_product_count() {
    for family in menus::families() {
        let docs = (family.docs)();
        assert_eq!(
            docs.len(),
            family.expected_count,
            "{}: the product must be exact",
            family.name
        );
        let execd = docs.iter().filter(|doc| family.execs(doc)).count();
        assert_eq!(
            execd, family.expected_execd,
            "{}: the exec-eligible count moved — update the pin WITH the \
             marker edit",
            family.name
        );
        assert!(execd > 0, "{}: no doc is exec-eligible", family.name);
    }
}

/// Distinct docs only: a generator that collapses different clause
/// choices onto one doc (a formatting bug, a forgotten push) hides
/// terminals from every downstream layer.
#[test]
fn every_menu_doc_is_distinct() {
    for family in menus::families() {
        let docs = (family.docs)();
        let distinct: BTreeSet<&String> = docs.iter().collect();
        assert_eq!(
            distinct.len(),
            docs.len(),
            "{}: collapsed docs in the product",
            family.name
        );
    }
}

/// Every terminal the menus spell appears in the generated corpus —
/// the vacuity guard's core. The terminals are the distinctive
/// spellings each menu was written to vary (alias spellings,
/// parameterized atoms, clause forms); a menu edit that orphans one
/// (renames a value, drops a loop arm) fails here.
#[test]
fn every_menu_terminal_appears_in_the_corpus() {
    let corpus: String = menus::families()
        .iter()
        .flat_map(|family| (family.docs)())
        .collect();
    let terminals = [
        // sequence / param / bucket: expression terminals and forms.
        "rand() * 2",
        "time::now()",
        "string::split('1.2.3', '.')[0]",
        "if $x { 1 } else { 2 }",
        "string::lowercase('MEMORY')",
        // database / table: changefeed and flag spellings.
        "include original",
        "schemafull schemaless",
        // table: the 3.3 LIGHTWEIGHT header word and the INLINE caps.
        " lightweight {",
        "lightweight",
        "inline edges: 8;",
        "inline references: 16;",
        // user: level aliases, pass forms, the SCRAM verifier, duration sides.
        "on: kv;",
        "passhash: '$argon2id$'",
        "passscram: 'SCRAM-SHA-256$4096:",
        "session: 12h;",
        // analyzer: the parameterized atoms and the alias.
        "snowball(en)",
        "edgengram(2, 10)",
        "mapper('/tmp/x.txt')",
        "mod::demo::some",
        // event: the async group's sides.
        "maxdepth: 9;",
        // function / api / config: middleware heads and graphql.
        "graphql { alias: 'f'; deprecated: 'old'; };",
        "api::timeout(5s)",
        "fn::audit('write')",
        "mod::pkg::check(1000)",
        "tables { exclude: `table`; };",
        "introspection",
        // index: the variant vocabularies.
        "columns: email;",
        "minkowski(3)",
        "m: 127;",
        "bm25: (2, 0);",
        "highlights;",
        "degree: 1;",
        // access: sources, issuers, variants + the 3.3 clauses.
        "algorithm: eddsa; key: 'k';",
        " audience: 'aud1', 'aud2';",
        " context: { extra: 1, tier: 'gold' };",
        "crypto::argon2::generate($password)",
        "refresh;",
        "authenticate: { if $auth.blocked { throw 'suspended'; } };",
        // field: the kind grammar's families and actions.
        "record<`NaN` | `Infinity`>",
        "default always: 'x';",
        "assert: $value != none;",
        "on delete: unset;",
        "<~person",
        "tags.*: string;",
        // field: the 3.3 INLINE flag (the relation-hosted docs).
        " inline;",
        // mixin: the spread entries, the per-kind members,
        // and the collision/override terminals.
        "...a;",
        "...b;",
        "f: bool;",
        "g: float;",
        "f: decimal;",
        "h: int;",
        "index h_idx { fields: h; };",
        "event f_evt { then: 1; };",
        // type-alias: the declaration, the sized use, and the
        // name-list non-use.
        "type Email = string;",
        "array<Email, 3>;",
        "record<Email>;",
    ];
    for terminal in terminals {
        assert!(
            corpus.contains(terminal),
            "the terminal `{terminal}` appears nowhere in the generated corpus"
        );
    }
}

/// The audit's own coverage: every family the menus table registers
/// is nonempty and its generator round-trips through the count pin
/// (the table and the fns cannot drift apart silently).
#[test]
fn the_menu_table_is_complete() {
    let names: Vec<&str> = menus::families().iter().map(|f| f.name).collect();
    assert_eq!(names.len(), 18, "eighteen families");
    for expected in [
        "sequence",
        "param",
        "database",
        "user",
        "analyzer",
        "bucket",
        "event",
        "function",
        "config api",
        "config graphql",
        "config default",
        "index",
        "api",
        "access",
        "table",
        "field",
        "mixin",
        "type-alias",
    ] {
        assert!(names.contains(&expected), "missing family: {expected}");
    }
}
