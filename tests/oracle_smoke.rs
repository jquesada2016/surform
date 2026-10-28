//! Layer-2 oracle smoke: the embedded official parser (`surrealdb_core::syn`,
//! the same code the server runs) accepts our DDL and rejects broken DDL.
//! The clause-combination suites below are the per-commit pair-product
//! layer for the emitted statements.

mod common;

use common::{
    assert_oracle_accepts, assert_oracle_accepts_files, assert_oracle_rejects,
    assert_oracle_rejects_files, oracle_accepts_files_print,
};
use surform::codegen::emit;
use surform::parser::parse_file;
use surrealdb_core::syn as oracle_syn;

#[test]
fn oracle_accepts_a_define() {
    assert_oracle_accepts("DEFINE TABLE user;");
}

#[test]
fn oracle_rejects_broken_define() {
    assert_oracle_rejects("DEFINE TABL user;");
}

/// The emitted namespace shape — `OVERWRITE`, session `USE`,
/// `COMMENT` — parses as emitted.
#[test]
fn oracle_accepts_emitted_namespace_shapes() {
    assert_oracle_accepts("DEFINE NAMESPACE OVERWRITE acme;\nUSE NS acme;\n");
    assert_oracle_accepts(
        "DEFINE NAMESPACE OVERWRITE acme COMMENT 'Acme Corporation';\nUSE NS acme;\n",
    );
    // The shapes the escaping rules rely on.
    assert_oracle_accepts("DEFINE NAMESPACE OVERWRITE `table`;\nUSE NS `table`;\n");
    assert_oracle_accepts("DEFINE NAMESPACE OVERWRITE acme COMMENT \"it's\";\nUSE NS acme;\n");
    assert_oracle_accepts(
        "DEFINE NAMESPACE OVERWRITE acme COMMENT 'a\\nb\\u{8}c';\nUSE NS acme;\n",
    );
}

/// Raw DDL shapes sequence emission produces must all parse —
/// including keyword-colliding names (backticked) and non-literal
/// expressions in every slot.
#[test]
fn oracle_accepts_emitted_sequence_shapes() {
    assert_oracle_accepts("DEFINE SEQUENCE IF NOT EXISTS row_ids;");
    assert_oracle_accepts("DEFINE SEQUENCE IF NOT EXISTS invoice_number START 1000;");
    assert_oracle_accepts(
        "DEFINE SEQUENCE IF NOT EXISTS order_number BATCH 100 START 5000 TIMEOUT 5s;",
    );
    assert_oracle_accepts("DEFINE SEQUENCE IF NOT EXISTS `table`;");
    // Probe-verified: TIMEOUT takes a full expression, and
    // the `none` sentinel (spellable in SDL, elided by the server's
    // own printer) re-parses and stores as the same absent sentinel.
    assert_oracle_accepts("DEFINE SEQUENCE IF NOT EXISTS n TIMEOUT time::now();");
    assert_oracle_accepts("DEFINE SEQUENCE IF NOT EXISTS n START { 1; 2 };");
    assert_oracle_accepts("DEFINE SEQUENCE IF NOT EXISTS n TIMEOUT NONE;");
    // The operator's repositioning form is valid SurrealQL too; the
    // emitted DDL only ever spells the define-once prefix (the
    // reader's prefix stripping is pinned in src/info/read.rs).
    assert_oracle_accepts("DEFINE SEQUENCE OVERWRITE n START 1000;");
    // The pipeline end-to-end for the sentinel: SDL → DDL → oracle.
    let (file, diags) = parse_file("sequence n { timeout: none; };");
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        emit(&file),
        "DEFINE SEQUENCE IF NOT EXISTS n TIMEOUT NONE;\n"
    );
}

/// The clause order is FIXED in SurrealQL: `START` before `BATCH` is
/// a parse error, and sequence has no `COMMENT` clause (both
/// probe-verified; SDL's free order is normalized by codegen, never
/// by omission).
#[test]
fn oracle_rejects_sequence_order_and_comment() {
    assert_oracle_rejects("DEFINE SEQUENCE n START 100 BATCH 50;");
    assert_oracle_rejects("DEFINE SEQUENCE n BATCH 50 START 100 COMMENT 'x';");
    assert_oracle_rejects("DEFINE SEQUENCE n COMMENT 'x';");
}

/// Raw DDL shapes param emission produces must all parse — including
/// bare VALUE-less params, keyword-colliding and quoted names,
/// statement-like values (paren-wrapped), and object values.
#[test]
fn oracle_accepts_emitted_param_shapes() {
    assert_oracle_accepts("DEFINE PARAM OVERWRITE $x;");
    assert_oracle_accepts("DEFINE PARAM OVERWRITE $x VALUE 50;");
    assert_oracle_accepts("DEFINE PARAM OVERWRITE $x VALUE 42 PERMISSIONS NONE;");
    assert_oracle_accepts("DEFINE PARAM OVERWRITE $x PERMISSIONS FULL;");
    assert_oracle_accepts(
        "DEFINE PARAM OVERWRITE $x VALUE 1 COMMENT 'doc' PERMISSIONS WHERE level > 10;",
    );
    // Probe-verified: keyword collisions stay bare after `$`;
    // quoted spellings emit backticked; the constant-literal spellings
    // `NaN`/`Infinity` emit backticked like upstream's shape rule.
    assert_oracle_accepts("DEFINE PARAM OVERWRITE $table VALUE 1;");
    assert_oracle_accepts("DEFINE PARAM OVERWRITE $`weird name` VALUE 1;");
    assert_oracle_accepts("DEFINE PARAM OVERWRITE $`NaN` VALUE 1;");
    // Quoted digit-led names are accepted upstream and emitted
    // backticked by the same shape rule.
    assert_oracle_accepts("DEFINE PARAM OVERWRITE $`9lives` VALUE 1;");
    // Statement-like expressions print wrapped by the server's own
    // emitter; objects keep their braces.
    assert_oracle_accepts("DEFINE PARAM OVERWRITE $x VALUE (CREATE user);");
    assert_oracle_accepts("DEFINE PARAM OVERWRITE $x VALUE { a: 1 };");
}

/// The combination oracle, pair-product layer (testing policy): every
/// presence combination of `sequence`'s three clauses crossed with
/// genuinely varied expression terminals — SDL → our DDL → the
/// embedded parser must accept it. The menus live in
/// [`common::menus`] (their one home; this suite and the full
/// product suite consume the same generators).
#[test]
fn oracle_sequence_clause_combinations() {
    let docs = common::menus::sequence_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(docs.len(), 4 * 3 * 4, "the full cross product must run");
}

/// Same layer for `param`: value terminal × permission form, plus the
/// clause-absent shapes.
#[test]
fn oracle_param_clause_combinations() {
    let docs = common::menus::param_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(docs.len(), 5 * 4, "the full cross product must run");
    // The short forms ride the same pipeline.
    for value in ["1 + 1", "5s", "{ a: 1 }"] {
        let sdl = format!("param $x = {value};");
        let (file, diags) = parse_file(&sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        assert_oracle_accepts(&emit(&file));
    }
}

/// Raw DDL shapes database emission produces must all parse — the
/// spec's canonical order (CHANGEFEED → STRICT → COMMENT) and the
/// server's own (STRICT, COMMENT, CHANGEFEED) alike, including the
/// `USE NS … DB` session lines.
#[test]
fn oracle_accepts_emitted_database_shapes() {
    assert_oracle_accepts("DEFINE DATABASE OVERWRITE blog;");
    assert_oracle_accepts("DEFINE DATABASE OVERWRITE blog STRICT;");
    assert_oracle_accepts("DEFINE DATABASE OVERWRITE blog COMMENT 'x';");
    assert_oracle_accepts("DEFINE DATABASE OVERWRITE blog CHANGEFEED 7d;");
    assert_oracle_accepts(
        "DEFINE DATABASE OVERWRITE blog CHANGEFEED 1h30m INCLUDE ORIGINAL STRICT COMMENT 'x';",
    );
    // The server's own clause order parses identically (the differ
    // compares ASTs, not clause order).
    assert_oracle_accepts(
        "DEFINE DATABASE OVERWRITE blog STRICT COMMENT 'x' CHANGEFEED 7d INCLUDE ORIGINAL;",
    );
    // Session context, including keyword-colliding names.
    assert_oracle_accepts("USE NS acme;\nDEFINE DATABASE OVERWRITE app;\nUSE NS acme DB app;\n");
    assert_oracle_accepts(
        "USE NS `table`;\nDEFINE DATABASE OVERWRITE `select`;\nUSE NS `table` DB `select`;\n",
    );
}

/// Same for bucket — validated through the files-capability oracle,
/// since `DEFINE BUCKET` is gated inside the parser itself.
#[test]
fn oracle_accepts_emitted_bucket_shapes() {
    assert_oracle_accepts_files("DEFINE BUCKET OVERWRITE shared;");
    assert_oracle_accepts_files("DEFINE BUCKET OVERWRITE b1 BACKEND 'memory' PERMISSIONS FULL;");
    assert_oracle_accepts_files(
        "DEFINE BUCKET OVERWRITE b1 READONLY BACKEND 'file:/srv/uploads' COMMENT 'x';",
    );
    assert_oracle_accepts_files("DEFINE BUCKET OVERWRITE b1 PERMISSIONS NONE;");
    assert_oracle_accepts_files("DEFINE BUCKET OVERWRITE b1 PERMISSIONS WHERE $auth.admin = true;");
}

/// The combination oracle, pair-product layer (testing policy):
/// `database` — strict × changefeed × doc comment.
#[test]
fn oracle_database_clause_combinations() {
    let docs = common::menus::database_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(docs.len(), 2 * 3 * 2, "the full cross product must run");
}

/// Raw DDL shapes user emission produces must all parse — the
/// define-once/`OVERWRITE` prefixes, the level spellings (canonical
/// full words), one- and two-sided durations, and the role list
/// (probe-pinned: the upstream parser seeds `VIEWER` and defaults
/// `1h`/`none` for absent clauses, so all these shapes parse).
#[test]
fn oracle_accepts_emitted_user_shapes() {
    assert_oracle_accepts(
        "DEFINE USER OVERWRITE deployer ON NAMESPACE PASSHASH '$argon2id$' ROLES EDITOR, VIEWER DURATION FOR TOKEN 1h, FOR SESSION 12h COMMENT 'CI deployment account';",
    );
    assert_oracle_accepts("DEFINE USER admin ON ROOT PASSWORD 'change-me' ROLES OWNER;");
    assert_oracle_accepts("DEFINE USER IF NOT EXISTS analyst ON DATABASE;");
    // The define-once shape with a COMMENT (a doc-commented
    // secret-less user) — the IF NOT EXISTS/COMMENT pairing.
    assert_oracle_accepts("DEFINE USER IF NOT EXISTS analyst ON DATABASE COMMENT 'doc';");
    // One-sided durations (the partial block).
    assert_oracle_accepts("DEFINE USER x ON ROOT DURATION FOR TOKEN 30m;");
    assert_oracle_accepts("DEFINE USER x ON ROOT DURATION FOR SESSION NONE;");
    // The pipeline end-to-end: absent roles/durations elide.
    let (file, diags) = parse_file("user analyst { on: db; };");
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        emit(&file),
        "DEFINE USER IF NOT EXISTS analyst ON DATABASE;\n"
    );
}

/// Same layer for `user`: level spelling × pass × SCRAM verifier ×
/// roles × duration sides — SDL → our DDL → the embedded parser must
/// accept it (the verifier is parse-validated upstream, so a valid
/// KAT is load-bearing here).
#[test]
fn oracle_user_clause_combinations() {
    let docs = common::menus::user_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(
        docs.len(),
        6 * 3 * 2 * 3 * 4,
        "the full cross product must run"
    );
}

/// Raw DDL shapes analyzer emission produces must all parse —
/// including the `mod::` fn-path prefix, which like `DEFINE BUCKET`
/// is gated inside the parser (surrealism), so those shapes
/// validate through the capabilities-enabled oracle.
#[test]
fn oracle_accepts_emitted_analyzer_shapes() {
    assert_oracle_accepts(
        "DEFINE ANALYZER OVERWRITE english TOKENIZERS CLASS FILTERS SNOWBALL(ENGLISH), \
         LOWERCASE COMMENT 'English text search';",
    );
    // The server emitter's own joiners (TOKENIZERS comma-no-space,
    // EDGENGRAM without an inner space).
    assert_oracle_accepts("DEFINE ANALYZER OVERWRITE raw_split TOKENIZERS CAMEL,PUNCT;");
    assert_oracle_accepts("DEFINE ANALYZER OVERWRITE ac FILTERS LOWERCASE,EDGENGRAM(2,10);");
    assert_oracle_accepts("DEFINE ANALYZER OVERWRITE empty;");
    assert_oracle_accepts("DEFINE ANALYZER OVERWRITE preprocessed FUNCTION fn::strip_markup;");
    assert_oracle_accepts_files("DEFINE ANALYZER OVERWRITE m2 FUNCTION mod::demo::some;");
    // The pipeline end-to-end for a capability-gated shape.
    let (file, diags) = parse_file("analyzer m3 { function: mod::demo::some; };");
    assert!(diags.is_empty(), "{diags:?}");
    assert_oracle_accepts_files(&emit(&file));
}

/// Same layer for `analyzer`: function × tokenizers × filters, with
/// genuinely varied terminals on every axis (alias spellings,
/// wrapped lists, parameterized atoms).
#[test]
fn oracle_analyzer_clause_combinations() {
    let docs = common::menus::analyzer_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        let ddl = emit(&file);
        if sdl.contains("mod::") {
            assert_oracle_accepts_files(&ddl);
        } else {
            assert_oracle_accepts(&ddl);
        }
    }
    assert_eq!(docs.len(), 4 * 4 * 4, "the full cross product must run");
}

/// Same layer for `bucket`: backend × readonly × permission form,
/// through the files-capability oracle.
#[test]
fn oracle_bucket_clause_combinations() {
    let docs = common::menus::bucket_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts_files(&emit(&file));
    }
    assert_eq!(docs.len(), 4 * 2 * 4, "the full cross product must run");
}

/// Raw DDL shapes event emission produces must all parse — the
/// canonical order ON → ASYNC → WHEN → THEN → COMMENT, the absent
/// `when` emitting nothing, the ASYNC glue, the `", "` THEN joiner,
/// and the THEN block print (a single-statement block without a
/// trailing `;`, a multi-statement one `;`-terminated — all
/// probe-pinned against the engine's emitter (v3.2.4 at the time; re-verified at the 3.3.0 floor move); the backticked table name
/// follows the decided keyword-collision policy).
#[test]
fn oracle_accepts_emitted_event_shapes() {
    // Minimal: an absent `when` is simply not emitted.
    assert_oracle_accepts(
        "DEFINE EVENT OVERWRITE touch_counter ON post THEN (UPDATE counter:posts SET total = total + 1);",
    );
    // WHEN + THEN + COMMENT, keyword-colliding table backticked.
    assert_oracle_accepts(
        "DEFINE EVENT OVERWRITE user_created ON `user` WHEN $event = 'CREATE' \
         THEN { CREATE log SET at = time::now() } COMMENT 'Log every user creation';",
    );
    // The ASYNC glue and the brace-block THEN.
    assert_oracle_accepts(
        "DEFINE EVENT OVERWRITE webhook_fanout ON invoice ASYNC RETRY 5 MAXDEPTH 8 \
         THEN { IF rand::bool() { THROW 'flaky downstream'; } };",
    );
    // The multi-expression THEN with the server emitter's joiner.
    assert_oracle_accepts(
        "DEFINE EVENT OVERWRITE double_bookkeeping ON payment \
         WHEN $event = 'UPDATE' AND $before.status != $after.status \
         THEN (CREATE ledger_entry SET payment = $after.id), \
         (UPDATE stats:payments SET changes = changes + 1);",
    );
    // The pipeline end-to-end for the then-only example.
    let (file, diags) = parse_file(
        "event touch_counter { on: post; then: (update counter:posts set total = total + 1); };",
    );
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        emit(&file),
        // The THEN item fits the 100-column budget on the single
        // statement line, so the emission stays flat (pretty.rs).
        concat!(
            "DEFINE EVENT OVERWRITE touch_counter ON `post` THEN ",
            "(UPDATE counter:posts SET total = total + 1);\n"
        )
    );
}

/// Same layer for `event`: when terminal × then shape × async group —
/// SDL → our DDL → the embedded parser must accept it.
#[test]
fn oracle_event_clause_combinations() {
    let docs = common::menus::event_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(docs.len(), 3 * 6 * 5, "the full cross product must run");
}

/// The `module` gate, re-probed at the 3.3.0 floor move (2026-09-25):
/// the module grammar is REAL in the embed now, gated at parse time by
/// the surrealism experimental capability (the 3.2.4 cargo-feature
/// bail arm is gone) — plain `parse` rejects with the gate message,
/// the capabilities-on oracle accepts both executable forms (printing
/// `PERMISSIONS FULL` on the bare shapes; the permissions arm
/// round-trips byte-identical), and the module
/// permissions wrapper parses (the bridge's own slot). The EXEC side
/// stays blind: the SDK's query path parses with default capabilities
/// ("Surrealism modules are not enabled"), so no live INFO can carry a
/// module entry and the reverse reader remains SDL-side golden-only.
#[test]
fn module_ddl_parses_under_the_capability_gate() {
    // Plain parse: the gate fires with its own message.
    let error = oracle_syn::parse(
        "DEFINE MODULE OVERWRITE mod::color FROM f\"modules:/color.surli\" UNSIGNED;",
    )
    .expect_err("the capability gate rejects plain parses");
    assert!(
        error.to_string().contains("requires Surrealism modules"),
        "{error}"
    );
    // The 3.2 `AS` spelling stays grammar-rejected even under the
    // capability (3.3.0's module grammar is FROM-only).
    assert_oracle_rejects("DEFINE MODULE mod::x AS f'a:/b';");
    assert_oracle_rejects_files("DEFINE MODULE mod::x AS f'a:/b';");
    // Capabilities on: both executable forms parse, re-print, and
    // round-trip through `ToSql` — byte-identical for a shape that
    // carries a permissions clause, and `… UNSIGNED PERMISSIONS FULL;`
    // for the bare ones (the printer materializes PERMISSIONS on
    // modules; SDL's absent-permissions elision is emission-side).
    for ddl in [
        "DEFINE MODULE OVERWRITE mod::color FROM f\"modules:/color.surli\" UNSIGNED;",
        "DEFINE MODULE OVERWRITE silo::acme::utils::<1.2.0> UNSIGNED;",
    ] {
        let printed = oracle_accepts_files_print(ddl);
        let bare = ddl.strip_suffix(';').expect("ends with ;");
        assert_eq!(
            printed,
            format!("{bare} PERMISSIONS FULL;"),
            "the bare shapes print with PERMISSIONS FULL materialized"
        );
    }
    let with_perms = "DEFINE MODULE OVERWRITE mod::color FROM f\"modules:/color.surli\" \
                      UNSIGNED PERMISSIONS WHERE $auth.admin = true;";
    assert_eq!(
        oracle_accepts_files_print(with_perms),
        with_perms,
        "the permissions shape round-trips byte-identical"
    );
    // The full pipeline still compiles SDL → DDL; the parse oracle now
    // covers it too.
    let (file, diags) = parse_file("module color { from: f'modules:/color.surli'; unsigned; };");
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        emit(&file),
        "DEFINE MODULE OVERWRITE mod::color FROM f\"modules:/color.surli\" UNSIGNED;\n"
    );
    assert_oracle_accepts_files(&emit(&file));
}

/// Layer-2 for `raw` (rule 8): the parsable class rides the oracle
/// end-to-end (SDL → DDL → the embed accepts); the rejected class —
/// the shapes raw EXISTS for — is pinned to the boundary so a change
/// in the embed's reach re-opens the question. (The advisory contract
/// — never a hard error — is asserted SDL-side; the lint flagging the
/// rejected class is the linter's.)
#[test]
fn oracle_raw_statement() {
    let sdl = "raw {\n  // interior comments ride\n  DEFINE PARAM OVERWRITE $smoke VALUE 1;\n};";
    let (file, diags) = parse_file(sdl);
    assert!(diags.is_empty(), "{diags:?}");
    assert_oracle_accepts(&emit(&file));
    // The future/legacy class: the pinned parser
    // rejects the body, SDL still accepts the statement verbatim.
    let future = "DEFINE TABLE t TYPE LIGHTWEIGHT;";
    assert_oracle_rejects(future);
    let sdl = format!("raw {{\n  {future}\n}};");
    let (_, diags) = parse_file(&sdl);
    assert!(diags.is_empty(), "{diags:?}");
}

/// Layer-2 shapes for `function` (spec/function.md): the body block
/// prints, the PERMISSIONS/GRAPHQL_* tails, and SDL's clause-order
/// divergence from the server emitter (SDL: PERMISSIONS → GRAPHQL_* →
/// COMMENT; the server ToSql: COMMENT → GRAPHQL_* → PERMISSIONS) —
/// both parse, the differ is AST-level.
#[test]
fn oracle_accepts_emitted_function_shapes() {
    // Minimal: body only, defaults elided.
    assert_oracle_accepts("DEFINE FUNCTION OVERWRITE fn::ping() { 'pong' };");
    // Permissions expression + COMMENT (SDL's order).
    assert_oracle_accepts(
        "DEFINE FUNCTION OVERWRITE fn::can_edit($p: record<post>) -> bool \
         { RETURN $auth.id = $p.author OR $auth.role = 'admin' } \
         PERMISSIONS WHERE $auth.admin = true COMMENT 'may edit';",
    );
    // The server emitter's own order — COMMENT → GRAPHQL_* →
    // PERMISSIONS — parses identically.
    assert_oracle_accepts(
        "DEFINE FUNCTION OVERWRITE fn::server_order() { 1 } COMMENT 'doc' \
         GRAPHQL_ALIAS 'a' GRAPHQL_DEPRECATED 'd' PERMISSIONS NONE;",
    );
    // The empty-body print upstream's emitter produces.
    assert_oracle_accepts("DEFINE FUNCTION OVERWRITE fn::empty() {;} GRAPHQL_ALIAS 'x';");
    // The pipeline end-to-end for the doc example.
    let (file, diags) = parse_file(
        "/// Whether the current user may edit this post\n\
         fn can_edit($p: record<post>) -> bool { return 1; } { permissions: none; };",
    );
    assert!(diags.is_empty(), "{diags:?}");
    // The emission breaks clause-per-line once the one-line spelling
    // passes the budget (the issue-#1 skeleton model); the pipeline
    // claim compares whitespace-collapsed.
    let flat = emit(&file).split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        flat,
        "DEFINE FUNCTION OVERWRITE fn::can_edit($p: record<post>) -> bool { RETURN 1 } \
         PERMISSIONS NONE COMMENT 'Whether the current user may edit this post';"
    );
}

/// Same layer for `function`: permissions form × graphql options ×
/// body shape — SDL → our DDL → the embedded parser must accept it.
#[test]
fn oracle_function_clause_combinations() {
    let docs = common::menus::function_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(docs.len(), 4 * 4 * 4, "the full cross product must run");
}

/// Kind terminals ride the parameter list and the return type: each
/// shape must survive SDL → DDL → the embedded parser.
#[test]
fn oracle_function_kind_terminals() {
    let kinds: [&str; 10] = [
        "option<number>",
        "int | float",
        "record<post | user>",
        "array<float, 10>",
        "geometry<point | line>",
        "'read' | 'write'",
        "{ a: string, b: option<int> }",
        "[1, 'two']",
        "-3 | 4",
        "1h",
    ];
    let mut count = 0;
    for kind in kinds {
        let sdl = format!("fn kinds($v: {kind}) {{ 1 }};");
        let (file, diags) = parse_file(&sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        assert_oracle_accepts(&emit(&file));
        let sdl = format!("fn ret_kind() -> {kind} {{ 1 }};");
        let (file, diags) = parse_file(&sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        assert_oracle_accepts(&emit(&file));
        count += 2;
    }
    assert_eq!(count, 2 * 10, "every kind terminal ran in both positions");
}

/// Layer-2 shapes for `config` (spec/config.md): SDL's spec-minimal
/// emissions (the bare/compact graphql forms diverge from upstream
/// ToSql's unconditional `TABLES … FUNCTIONS …` — both re-parse
/// identically) plus the pipeline end-to-end for a spec example.
/// Config has no capability gate, so plain syn parses everything.
#[test]
fn oracle_accepts_emitted_config_shapes() {
    // The api variant: middleware entries with varied heads/args, the
    // permissions tail (elided and spelled).
    assert_oracle_accepts("DEFINE CONFIG OVERWRITE API;");
    assert_oracle_accepts("DEFINE CONFIG OVERWRITE API PERMISSIONS NONE;");
    assert_oracle_accepts(
        "DEFINE CONFIG OVERWRITE API MIDDLEWARE api::timeout(5s), api::log() \
         PERMISSIONS WHERE $auth.admin = true;",
    );
    assert_oracle_accepts(
        "DEFINE CONFIG OVERWRITE API MIDDLEWARE fn::util::guard((CREATE audit)), \
         timeout(1_000);",
    );
    // The graphql variant: the spec-minimal shapes and the structured
    // pairs.
    assert_oracle_accepts("DEFINE CONFIG OVERWRITE GRAPHQL;");
    assert_oracle_accepts("DEFINE CONFIG OVERWRITE GRAPHQL AUTO;");
    assert_oracle_accepts(
        "DEFINE CONFIG OVERWRITE GRAPHQL TABLES INCLUDE `user`, `post` \
         FUNCTIONS EXCLUDE fn::legacy_report;",
    );
    assert_oracle_accepts(
        "DEFINE CONFIG OVERWRITE GRAPHQL TABLES AUTO FUNCTIONS NONE DEPTH 1000 \
         COMPLEXITY 100 INTROSPECTION NONE;",
    );
    // Upstream's own unconditional print parses too (the differ's
    // input shape).
    assert_oracle_accepts("DEFINE CONFIG OVERWRITE GRAPHQL TABLES NONE FUNCTIONS NONE;");
    // The default variant at both scopes; the ROOT-level hoist lands
    // it before the USE lines.
    assert_oracle_accepts(
        "DEFINE CONFIG OVERWRITE DEFAULT NAMESPACE 'acme' DATABASE NONE;\nUSE NS acme;\n",
    );
    assert_oracle_accepts("DEFINE CONFIG OVERWRITE DEFAULT NAMESPACE NONE DATABASE 'blog';");
    // The pipeline end-to-end for the spec's api example.
    let (file, diags) = parse_file(
        "config api { middleware: api::timeout(5s), api::log(); permissions: \
         $auth.admin = true; };",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let flat = emit(&file).split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        flat,
        "DEFINE CONFIG OVERWRITE API MIDDLEWARE api::timeout(5s), api::log() \
         PERMISSIONS WHERE $auth.admin = true;"
    );
    assert_oracle_accepts(&emit(&file));
}

/// Same layer for `config api`: the middleware-head × permissions
/// pair product with genuinely varied argument terminals — SDL → our
/// DDL → the embedded parser must accept it. `mod::` heads are gated
/// *inside* the parser (surrealism), so their combos ride the
/// experimental-capability helper like bucket DDL.
#[test]
fn oracle_config_api_clause_combinations() {
    let docs = common::menus::config_api_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(docs.len(), 4 * 4, "the full cross product must run");
    // The `mod::` head combos through the capability gate.
    for args in ["", "1", "1, 'two'"] {
        let sdl = format!("config api {{ middleware: mod::pkg::check({args}); }};");
        let (file, diags) = parse_file(&sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        assert_oracle_accepts_files(&emit(&file));
    }
}

/// Same layer for `config graphql`: family × family (word and
/// sub-block forms), the flag folds over them, and the limits trio —
/// SDL → our DDL → the embedded parser must accept it.
#[test]
fn oracle_config_graphql_clause_combinations() {
    let docs = common::menus::config_graphql_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(
        docs.len(),
        5 * 4 + 4 + 3 * 2 * 3 - 1,
        "the family, flag, and limits products must run"
    );
}

/// Same layer for `config default`: namespace × database expression
/// terminals (the both-absent combo is the parse-rejected shape, so
/// the product skips it).
#[test]
fn oracle_config_default_clause_combinations() {
    let docs = common::menus::config_default_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(
        docs.len(),
        4 * 4 - 1,
        "the cross product minus the empty combo"
    );
}

/// Same layer for `index`, per kind (legal combos only — the DISKANN
/// restrictions and arity rules are SDL-side parse errors, covered by
/// the parser unit tests): field terminals × the per-variant clause
/// vocabularies, SDL → our DDL → the embedded parser must accept it.
#[test]
fn oracle_index_clause_combinations() {
    let docs = common::menus::index_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(
        docs.len(),
        4 * 2 * 2 + 3 * 2 + 3 * 3 * 2 + 6 * 3 * 3 * 2 * 4 + 3 * 3 * 3 * 2,
        "every per-kind cross product must run"
    );
}

/// Negative layer for `index`: shapes upstream's own parser rejects
/// stay rejected when they arrive through SDL's emission grammar —
/// including the arity and domain rules SDL mirrors.
#[test]
fn oracle_rejects_broken_index_ddl() {
    assert_oracle_rejects("DEFINE INDEX OVERWRITE x ON t;");
    assert_oracle_rejects("DEFINE INDEX OVERWRITE x ON t FIELDS;");
    assert_oracle_rejects("DEFINE INDEX OVERWRITE x ON t FIELDS a COUNT;");
    assert_oracle_rejects("DEFINE INDEX OVERWRITE x ON t FIELDS a, b FULLTEXT;");
    assert_oracle_rejects("DEFINE INDEX OVERWRITE x ON t FIELDS a HNSW DIMENSION;");
    assert_oracle_rejects("DEFINE INDEX OVERWRITE x ON t FIELDS a HNSW DIMENSION 70000;");
    assert_oracle_rejects("DEFINE INDEX OVERWRITE x ON t FIELDS a HNSW DIMENSION 8 M 200;");
    assert_oracle_rejects(
        "DEFINE INDEX OVERWRITE x ON t FIELDS a DISKANN DIMENSION 8 DIST CHEBYSHEV;",
    );
    assert_oracle_rejects("DEFINE INDEX OVERWRITE x ON t FIELDS a DISKANN DIMENSION 8 TYPE I64;");
    assert_oracle_rejects(
        "DEFINE INDEX OVERWRITE x ON t FIELDS a DISKANN DIMENSION 8 DIST COSINE_NORMALIZED TYPE I8;",
    );
    assert_oracle_rejects("DEFINE INDEX OVERWRITE x ON t FIELDS a DISKANN DIMENSION 8 DEGREE 0;");
}

/// Negative layer for `config`: shapes upstream's own parser rejects
/// stay rejected when they arrive through SDL's emission grammar.
#[test]
fn oracle_rejects_broken_config_ddl() {
    assert_oracle_rejects("DEFINE CONFIG OVERWRITE;");
    assert_oracle_rejects("DEFINE CONFIG OVERWRITE API MIDDLEWARE;");
    assert_oracle_rejects("DEFINE CONFIG OVERWRITE API MIDDLEWARE api::timeout;");
    assert_oracle_rejects("DEFINE CONFIG OVERWRITE GRAPHQL TABLES;");
    assert_oracle_rejects("DEFINE CONFIG OVERWRITE GRAPHQL TABLES INCLUDE;");
    assert_oracle_rejects("DEFINE CONFIG OVERWRITE GRAPHQL DEPTH -1;");
    assert_oracle_rejects("DEFINE CONFIG OVERWRITE DEFAULT;");
}

/// Same layer for `api`: group terminals × the shared config clause
/// vocabulary × the `any` block's shapes, SDL → our DDL → the
/// embedded parser must accept it. (The duplicate-method and
/// single-`any` rules are SDL-side parse errors — upstream accepts
/// the folded `FOR ANY` shape, so they are not oracle-reject
/// material; the parser unit tests cover them.)
#[test]
fn oracle_api_clause_combinations() {
    let docs = common::menus::api_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        // The `mod::` head is gated inside the parser
        // (surrealism), so the menu rides the experimental
        // capabilities like bucket DDL — the same construction SDL
        // canonicalizes under.
        assert_oracle_accepts_files(&emit(&file));
    }
    assert_eq!(
        docs.len(),
        3 * 4 * 4 + 4 * 4 * 2,
        "the group and any cross products must run"
    );

    // Path terminals: static, dynamic, wildcard, an escaped
    // backslash, and the apostrophe that flips the quote character.
    for path in [
        "api '/health'",
        "api '/users/:id'",
        "api '/admin/*rest'",
        "api '/e\\\\:x'",
        "api \"/it's\"",
    ] {
        for clause in ["", " permissions: none;"] {
            let sdl = format!("{path} {{ get {{{clause} then: {{ {{ status: 200 }} }}; }}; }};");
            let (file, diags) = parse_file(&sdl);
            assert!(diags.is_empty(), "{sdl}: {diags:?}");
            assert_oracle_accepts(&emit(&file));
        }
    }

    // The bare and empty-body forms emit the zero-`FOR` statement —
    // legal upstream (probe-pinned), so the oracle must accept it.
    for sdl in ["api '/x';", "api '/x' { };", "api '/x' { any { }; };"] {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        assert_oracle_accepts(&emit(&file));
    }
}

/// Negative layer for `api`: shapes upstream's own parser rejects
/// stay rejected when they arrive through SDL's emission grammar.
#[test]
fn oracle_rejects_broken_api_ddl() {
    // A method group without THEN (SDL rejects at parse too —
    // upstream: `expected THEN`).
    assert_oracle_rejects("DEFINE API OVERWRITE '/x' FOR get;");
    assert_oracle_rejects("DEFINE API OVERWRITE '/x' FOR get PERMISSIONS NONE;");
    assert_oracle_rejects("DEFINE API OVERWRITE '/x' FOR get THEN;");
    assert_oracle_rejects("DEFINE API OVERWRITE '/x' FOR;");
    assert_oracle_rejects("DEFINE API OVERWRITE '/x' FOR bogus THEN 1;");
    assert_oracle_rejects("DEFINE API OVERWRITE '/x' FOR get MIDDLEWARE THEN 1;");
    assert_oracle_rejects("DEFINE API OVERWRITE '/x' FOR get MIDDLEWARE api::t THEN 1;");
}

/// The `access` layer (spec/access.md): the three variants' clause
/// cross products — sources × issuers × durations × levels × variant
/// extras — SDL → our DDL → the embedded parser must accept it.
/// ACCESS has no experimental-capability gate (unlike `module` and
/// `mod::` middleware), so plain acceptance suffices. Legality pairs
/// ride the data: an issuer algorithm is only generated in shapes the
/// source accepts (any on the URL path, forced-equal by local key),
/// and levels only where the variant allows them. Grant/token
/// durations on non-capable variants stay in the menu deliberately —
/// SurrealQL accepts and silently drops them (spec/access.md), so the
/// parse oracle must accept them too.
#[test]
fn oracle_access_clause_combinations() {
    let docs = common::menus::access_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(
        docs.len(),
        4 * 3 * 4 * 4 + 9 + 5 * 4 * 2 * 2 * 2 + 6 * 2 * 3 + 3 + 2 + 3,
        "every variant cross product must run"
    );
}

/// Negative layer for `access`: shapes upstream's own parser rejects
/// (spec/access.md's empirically verified list, plus the issuer
/// compatibility rule from `parse_jwt`) stay rejected when they
/// arrive through SDL's emission grammar.
#[test]
fn oracle_rejects_broken_access_ddl() {
    // jwt without a verification source (upstream: `expected
    // ALGORITHM, or URL`).
    assert_oracle_rejects("DEFINE ACCESS OVERWRITE a ON ROOT TYPE JWT;");
    // Missing ON (upstream: `expected ON`).
    assert_oracle_rejects("DEFINE ACCESS OVERWRITE a TYPE JWT URL 'u';");
    // Record is database-only (upstream parse-enforced).
    assert_oracle_rejects("DEFINE ACCESS OVERWRITE a ON ROOT TYPE RECORD;");
    assert_oracle_rejects("DEFINE ACCESS OVERWRITE a ON NAMESPACE TYPE RECORD;");
    // Bearer requires FOR (upstream: `expected FOR`).
    assert_oracle_rejects("DEFINE ACCESS OVERWRITE a ON ROOT TYPE BEARER;");
    // Bearer-for-record is database-only.
    assert_oracle_rejects("DEFINE ACCESS OVERWRITE a ON ROOT TYPE BEARER FOR RECORD;");
    assert_oracle_rejects("DEFINE ACCESS OVERWRITE a ON NAMESPACE TYPE BEARER FOR RECORD;");
    // Issuer algorithm compatibility (upstream: `a compatible
    // algorithm or no algorithm`).
    assert_oracle_rejects(
        "DEFINE ACCESS OVERWRITE a ON ROOT TYPE JWT ALGORITHM ES256 KEY 'k' \
         WITH ISSUER ALGORITHM HS256 KEY 's';",
    );
    // Unknown algorithm word.
    assert_oracle_rejects("DEFINE ACCESS OVERWRITE a ON ROOT TYPE JWT ALGORITHM BANANA KEY 'k';");
    // The record WITH loop takes JWT or REFRESH only.
    assert_oracle_rejects("DEFINE ACCESS OVERWRITE a ON DB TYPE RECORD SIGNIN 1 WITH BANANA;");
    // DURATION takes its FOR sides (a side keyword outside the set
    // rejects; a bare `DURATION;` with no sides is ACCEPTED upstream
    // — probe-pinned through the oracle — so it is not a negative).
    assert_oracle_rejects(
        "DEFINE ACCESS OVERWRITE a ON ROOT TYPE BEARER FOR USER DURATION FOR BANANA 1h;",
    );
}

/// The `table` layer (spec/table.md): the non-view kinds' clause
/// cross products — schema-flag spellings (header AND block; both
/// legal, last-one-wins) × drop × changefeed × permission forms ×
/// graphql — for the bare and `any` kinds; the relation kinds' side
/// shapes × enforced; the nested definitions; and the view's query ×
/// trailing clauses. TABLE has no experimental-capability gate, so
/// plain acceptance suffices. The SDL-side-only rules stay out (the
/// illegal ones are SDL parse errors, not oracle material): the
/// schema flags' fold and `enforced`'s kind membership ride the
/// fold-site validation.
#[test]
fn oracle_table_clause_combinations() {
    let docs = common::menus::table_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(
        docs.len(),
        2 * 4 * 2 * 3 * 6 * 2 + (2 * 2 * 2 * 4 + 4 * 2 * 2 * 4) + 4 + 3 * 4,
        "every kind cross product must run"
    );
}

/// Negative layer for `table`: shapes upstream's own parser rejects
/// (spec/table.md's empirically verified list) stay rejected when
/// they arrive through SDL's emission grammar.
#[test]
fn oracle_rejects_broken_table_ddl() {
    // Comma endpoint lists (`|` is the separator — verified).
    assert_oracle_rejects("DEFINE TABLE OVERWRITE r TYPE RELATION IN user, org SCHEMALESS;");
    // An unknown TYPE word.
    assert_oracle_rejects("DEFINE TABLE OVERWRITE t TYPE BANANA;");
    // The docs' `TYPE LIGHTWEIGHT` shorthand is no grammar at all —
    // LIGHTWEIGHT rides the relation sub-grammar only (`TYPE` takes
    // NORMAL|RELATION|ANY); SDL authors it as the trailing header
    // word. The rejection pin keeps the shorthand out.
    assert_oracle_rejects("DEFINE TABLE OVERWRITE t TYPE LIGHTWEIGHT;");
    // Tables take the per-op parser: no bare expression (`expected
    // 'NONE', 'FULL' or 'FOR'`).
    assert_oracle_rejects("DEFINE TABLE OVERWRITE t SCHEMALESS PERMISSIONS WHERE true;");
    // The permission op set is fixed.
    assert_oracle_rejects("DEFINE TABLE OVERWRITE t SCHEMALESS PERMISSIONS FOR BANANA FULL;");
    // Views are a RESTRICTED select: ORDER/LIMIT beyond WHERE/GROUP
    // rejects in upstream's parse_view.
    assert_oracle_rejects(
        "DEFINE TABLE OVERWRITE v TYPE ANY SCHEMALESS AS SELECT * FROM post ORDER BY id;",
    );
    // CHANGEFEED requires its duration value.
    assert_oracle_rejects("DEFINE TABLE OVERWRITE t SCHEMALESS CHANGEFEED;");
    // GRAPHQL_* take string literals.
    assert_oracle_rejects("DEFINE TABLE OVERWRITE t SCHEMALESS GRAPHQL_DEPRECATED 3;");
}

#[test]
fn oracle_field_clause_combinations() {
    let docs = common::menus::field_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(
        docs.len(),
        11 * 3 * 3 * 2 * 5 * 2 + 4 * 2 + 6 + 6 + 5 + 4 * 5 + 1,
        "every field cross product must run"
    );
}

#[test]
fn oracle_rejects_broken_field_ddl() {
    // The runtime parser's FLEXIBLE position is fixed after TYPE.
    assert_oracle_rejects("DEFINE FIELD OVERWRITE s ON t FLEXIBLE TYPE string;");
    // ... and it demands an object-bearing kind.
    assert_oracle_rejects("DEFINE FIELD OVERWRITE s ON t TYPE string FLEXIBLE;");
    assert_oracle_rejects("DEFINE FIELD OVERWRITE s ON t TYPE record<user> FLEXIBLE;");
    // Fields take no DELETE permission.
    assert_oracle_rejects("DEFINE FIELD OVERWRITE s ON t TYPE string PERMISSIONS FOR delete FULL;");
    // `any` cannot join a union (upstream leaves the `| x` unconsumed).
    assert_oracle_rejects("DEFINE FIELD OVERWRITE s ON t TYPE any | string;");
    // `option` interiors take concrete unions — no nested options.
    assert_oracle_rejects("DEFINE FIELD OVERWRITE s ON t TYPE option<option<int>>;");
    // Array sizes are unsigned integers.
    assert_oracle_rejects("DEFINE FIELD OVERWRITE s ON t TYPE array<int, -1>;");
    // (`record<int | string>` and the bare typeless `DEFINE FIELD s
    // ON t` are, by contrast, LEGAL upstream — the interior's name
    // reader takes keyword-like words too, and the typeless field is
    // exactly the licensed-deviation shape SDL routes through `raw`.
    // The probes are why neither is in this list.)
}

/// Same layer for `mixin` (family 17): the spread mechanism
/// — per-kind members, the explicit overshadow, the last-wins
/// collision — through parse, the fmt fixed point, and the embedded
/// parser's acceptance of the EXPANDED emission (the oracle sees the
/// lowered DDL; the merge/differ legs are the full layer's).
#[test]
fn oracle_mixin_clause_combinations() {
    let docs = common::menus::mixin_docs();
    for sdl in &docs {
        let (file, diags) = parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        common::assert_fmt_fixed_point(sdl);
        assert_oracle_accepts(&emit(&file));
    }
    assert_eq!(docs.len(), 3 * 2 * 3 * 2, "the full cross product must run");
}
