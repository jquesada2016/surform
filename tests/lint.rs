//! The linter's behavior: every finding kind's
//! fire/not-fire legs, the resolved-vs-source distinction (elision
//! never demotes lint visibility; the authored-basis lints), span
//! hygiene (adoptee-only findings render sourceless — a merged
//! statement's span points into DDL fragment text and must never
//! label the authored file), the engine-predicate mirrors, and the
//! promotion trigger chains verbatim.

use surform::ast::SdlFile;
use surform::info::normalize::{Interchange, ReadModel, merge};
use surform::lint::{Finding, FindingKind, LintAllowProblem, lint};
use surform::parser;
use surform::refs;

/// Parses, asserting the fixture is clean — a lint test with parse
/// diagnostics is testing the parser.
fn parse(src: &str) -> SdlFile {
    let (file, diags) = parser::parse_file(src);
    assert!(diags.is_empty(), "fixture must parse cleanly: {diags:?}");
    file
}

/// Lints the file alone — the no-`--info` mode, resolved over the
/// EXPANDED form (the CLI's and the editor's wiring: a spread member
/// is a field the database would hold). Mixin-free fixtures expand to
/// themselves, so the existing legs are unchanged by this.
fn lint_alone(src: &str) -> Vec<Finding> {
    let file = parse(src);
    let resolved = surform::expand::expand(&file);
    lint(&file, &resolved, src)
}

fn codes(findings: &[Finding]) -> Vec<&'static str> {
    findings.iter().map(|f| f.kind.code_str()).collect()
}

/// The one finding with this code, asserting uniqueness.
fn one<'a>(findings: &'a [Finding], code: &str) -> &'a Finding {
    let hits: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.kind.code_str() == code)
        .collect();
    assert_eq!(hits.len(), 1, "expected exactly one {code}: {findings:?}");
    hits[0]
}

/// Hand-fed merge, the partially-applied-file shape (live-printer
/// DDL literals, `hand_fed_merge`'s discipline in tests/normalize.rs).
fn lint_merged(sdl: &str, db_text: &str) -> Vec<Finding> {
    let db_obj = surform::info::read::read_info_object(db_text).expect("db INFO walks");
    let model = ReadModel::from_interchange(Interchange {
        root: None,
        namespaces: &[],
        databases: &[("test".to_string(), "scratch".to_string(), &db_obj)],
    })
    .expect("the interchange reads");
    let file = parse(sdl);
    let resolved = merge(&file, &model);
    lint(&file, &resolved, sdl)
}

// ---------------------------------------------------------------------
// SDL-L0001 — world-open table permissions
// ---------------------------------------------------------------------

#[test]
fn permissions_full_fires() {
    let findings = lint_alone("table t { permissions: full; };");
    assert_eq!(codes(&findings), ["SDL-L0001"]);
}

#[test]
fn full_view_fires() {
    // Views carry the clause too (live-probed: it parses and defines)
    // — `full` on a view is the same world-open grant, so the old
    // lint's view carve-out does not come back.
    let findings = lint_alone(
        "table t;\
         \ntable view v { select * from t; permissions: full; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0001"]);
}

#[test]
fn all_operation_groups_full_fires() {
    // The long spelling of `permissions: full;` — every operation
    // covered, every value `full` — is the same world-open grant. It
    // also carries SDL-L0014 (a `delete: full` group is present, and
    // the shorthand's INFO round-trip does not apply to it).
    let findings = lint_alone(
        "table t { permissions { select, create: full; update: full; delete: full; }; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0001", "SDL-L0014"]);
}

#[test]
fn secure_defaults_never_fire() {
    // Omission IS the secure default (implicit `none`) — the case the
    // old lint wrong-footed — and so are explicit `none` (deliberate
    // denial, spelled out) and partial openings (a scoped `select:
    // full` is the author's own choice).
    assert!(lint_alone("table t { field id: int; };").is_empty());
    assert!(lint_alone("table a { permissions: none; };").is_empty());
    assert!(lint_alone("table c { permissions { select: full; }; };").is_empty());
}

#[test]
fn database_full_fires_through_merge() {
    // The resolved-state story: the SDL file never spells a
    // permissions clause, but the database holds the table wide open —
    // the reader adopts `PERMISSIONS FULL` and the lint evaluates the
    // merged state.
    let findings = lint_merged(
        "namespace test;\ndatabase scratch;\ntable t { field id: int; };",
        "{ tables: { t: 'DEFINE TABLE t TYPE NORMAL SCHEMALESS \
         PERMISSIONS FULL' } }",
    );
    assert_eq!(codes(&findings), ["SDL-L0001"]);
}

#[test]
fn adoptee_table_fires_sourceless() {
    // The span-hygiene leg: an adoptee-only statement has no authored
    // span, and the merged statement's read-side span (into DDL
    // fragment text) must never label the file.
    let findings = lint_merged(
        "namespace test;\ndatabase scratch;",
        "{ tables: { adoptee: 'DEFINE TABLE adoptee TYPE NORMAL \
         SCHEMALESS PERMISSIONS FULL' } }",
    );
    let finding = one(&findings, "SDL-L0001");
    assert!(finding.span.is_none(), "adoptee findings render sourceless");
    assert!(finding.key == "adoptee");
}

// ---------------------------------------------------------------------
// SDL-L0002 / L0003 / L0005 — users
// ---------------------------------------------------------------------

#[test]
fn user_lints_fire_in_one_pass() {
    let findings = lint_alone(
        "user a { on: root; password: 'x'; roles: owner, owner; };\
         \nuser b { on: root; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0002", "SDL-L0005", "SDL-L0003"]);
}

#[test]
fn passhash_user_never_fires_plaintext_or_secretless() {
    assert!(lint_alone("user a { on: root; passhash: '$argon2id$v=19'; };").is_empty());
}

/// A scram-only user carries a working credential (the engine keeps
/// the verifier; OVERWRITE re-apply is safe) — L0003 stays silent.
/// (The passscram SENTINEL has no lint arm: the parser's SDL-E1059
/// rejects it outright, so the resolved state can never carry one.)
#[test]
fn passscram_user_never_fires_secretless() {
    // surrealdb-iam's KAT verifier (the menus' SCRAM_KAT).
    const KAT: &str = "SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20=";
    let findings = lint_alone(&format!("user a {{ on: root; passscram: '{KAT}'; }};"));
    assert!(findings.is_empty(), "{findings:?}");
}

// ---------------------------------------------------------------------
// SDL-L0004 / L0005 — analyzers
// ---------------------------------------------------------------------

#[test]
fn analyzer_min_max_and_duplicates_fire() {
    let findings = lint_alone(
        "analyzer a {\
         \n  tokenizers: class, class;\
         \n  filters: edgengram(10, 2), ngram(3, 1), snowball(english), \
         snowball(en);\
         \n};",
    );
    assert_eq!(
        codes(&findings),
        ["SDL-L0004", "SDL-L0004", "SDL-L0005", "SDL-L0005"]
    );
}

#[test]
fn ordered_bounds_and_distinct_lists_do_not_fire() {
    assert!(lint_alone("analyzer a { filters: edgengram(2, 10), ngram(2, 10); };").is_empty());
}

// ---------------------------------------------------------------------
// SDL-L0006 — relations without enforced
// ---------------------------------------------------------------------

#[test]
fn unenforced_relation_fires() {
    let findings = lint_alone(
        "table follow;\
         \ntable -> user -> follow;",
    );
    assert_eq!(codes(&findings), ["SDL-L0006"]);
}

#[test]
fn enforced_relation_does_not_fire() {
    let findings = lint_alone("table relation follows { enforced; };");
    assert!(codes(&findings).is_empty());
}

#[test]
fn lightweight_relation_implies_enforced() {
    // The parse-fold: `lightweight` implies `enforced` (the engine's
    // define normalization — INFO always prints both words), so the
    // unenforced-relation lint never fires under it.
    assert!(
        lint_alone(
            "table person;\
         \ntable post;\
         \ntable person -> likes -> post lightweight;"
        )
        .is_empty()
    );
}

// ---------------------------------------------------------------------
// SDL-L0007 — non-nested placement
// ---------------------------------------------------------------------

#[test]
fn standalone_children_with_in_scope_hosts_fire() {
    let findings = lint_alone(
        "table t {};\
         \nfield f: int { on: t; };\
         \nindex i { on: t; fields: f; };\
         \nevent e { on: t; then: 1; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0007", "SDL-L0007", "SDL-L0007"]);
}

#[test]
fn children_without_in_scope_hosts_do_not_fire() {
    // A field on nothing is accepted best-effort; its host lives in
    // another file — SDL-L0024's class, excused (a partial file is
    // the allow directive's reason to exist).
    assert!(
        lint_alone("// surform-lint-allow: SDL-L0024\nfield f: int { on: elsewhere; };").is_empty()
    );
}

#[test]
fn view_hosts_are_not_nestable_so_never_fire() {
    // A view's block IS its query — nothing can nest there.
    assert!(
        lint_alone(
            "table t;\
         \ntable view v { select * from t; };\
         \nfield f: int { on: v; };"
        )
        .is_empty()
    );
}

#[test]
fn nested_children_do_not_fire() {
    assert!(lint_alone("table t { field f: int; index i { fields: f; }; };").is_empty());
}

// ---------------------------------------------------------------------
// SDL-L0008 / L0009 — params and apis
// ---------------------------------------------------------------------

#[test]
fn valueless_param_fires() {
    let findings = lint_alone("param $x;");
    assert_eq!(codes(&findings), ["SDL-L0008"]);
    assert!(lint_alone("param $x = 5;").is_empty());
}

#[test]
fn zero_for_api_fires() {
    let findings = lint_alone("api '/x' { };");
    assert_eq!(codes(&findings), ["SDL-L0009"]);
}

#[test]
fn empty_any_catch_all_fires_even_with_groups() {
    let findings = lint_alone("api '/x' { any { }; get { then: 1; }; };");
    assert_eq!(codes(&findings), ["SDL-L0009"]);
}

/// The engine materializes a catch-all for every api (INFO always
/// prints `FOR ANY …`), so an any-less api carries the implicit
/// no-op — and the reader's absent-`any` rule folds the authored
/// empty block onto the same shape, keeping both modes consistent.
#[test]
fn any_less_api_fires_the_implicit_noop_catch_all() {
    let findings = lint_alone("api '/x' { get { then: 1; }; };");
    assert_eq!(codes(&findings), ["SDL-L0009"]);
}

#[test]
fn configured_apis_do_not_fire() {
    assert!(lint_alone("api '/x' { any { then: 1; }; };").is_empty());
    assert!(lint_alone("api '/x' { any { permissions: none; }; };").is_empty());
}

// ---------------------------------------------------------------------
// SDL-L0010 — relation endpoint rewrite
// ---------------------------------------------------------------------

#[test]
fn record_typed_in_out_fields_on_relations_fire() {
    let findings = lint_alone(
        "table follow;\
         \ntable -> user -> follow {\
         \n  enforced;\
         \n  permissions: none;\
         \n  field in: record<user>;\
         \n  field out: array<record<user>>;\
         \n  field plain: string;\
         \n};",
    );
    assert_eq!(codes(&findings), ["SDL-L0010", "SDL-L0010"]);
}

#[test]
fn non_record_in_out_and_non_relation_hosts_do_not_fire() {
    // The rewrite hazard is the record-typed shape; a plain field
    // named `in` on a relation is unverified engine behavior — the
    // lint stays conservative. A non-relation host has no endpoints
    // to rewrite.
    assert!(
        lint_alone(
            "table follow;\
         \ntable -> user -> follow {\
         \n  enforced;\
         \n  permissions: none;\
         \n  field in: string;\
         \n};"
        )
        .is_empty()
    );
    assert!(
        lint_alone(
            "table user;\
         \ntable t { permissions: none; field in: record<user>; };"
        )
        .is_empty()
    );
}

// ---------------------------------------------------------------------
// SDL-L0011 — capability-dropped duration sides
// ---------------------------------------------------------------------

#[test]
fn jwt_without_issuer_drops_the_token_side() {
    // The spec's own INFO edge: verify-only cannot issue tokens.
    let findings = lint_alone(
        "access a { on: database; type: jwt; algorithm: es256; \
         key: 'MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE'; \
         duration { token: 1h; }; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0011"]);
}

#[test]
fn jwt_with_issuer_keeps_the_token_side() {
    // The issuer key is shape-only in SDL (SDL-E1056 pins it to the
    // sentinel), so a token-capable jwt access always carries the
    // sentinel-apply advisory too — never the capability finding.
    let findings = lint_alone(
        "access a { on: database; type: jwt; algorithm: es256; \
         key: 'MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE'; \
         issuer { key: '[REDACTED]'; }; \
         duration { token: 1h; }; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0013"]);
}

#[test]
fn jwt_never_takes_grants() {
    let findings = lint_alone(
        "access a { on: database; type: jwt; algorithm: es256; \
         key: 'MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE'; \
         issuer { key: '[REDACTED]'; }; \
         duration { grant: 1d; }; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0011", "SDL-L0013"]);
}

#[test]
fn record_grant_needs_refresh() {
    let findings = lint_alone(
        "access a { on: database; type: record; \
         duration { grant: 1d; token: 1h; }; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0011"]);
    assert!(
        lint_alone(
            "access a { on: database; type: record; refresh; \
         duration { grant: 1d; token: 1h; }; };"
        )
        .is_empty()
    );
}

#[test]
fn bearer_is_grant_and_token_capable() {
    assert!(
        lint_alone(
            "access a { on: database; type: bearer; for: user; \
         duration { grant: 1d; token: 1h; }; };"
        )
        .is_empty()
    );
}

// ---------------------------------------------------------------------
// SDL-L0012 / L0013 — raw bodies and sentinels
// ---------------------------------------------------------------------

#[test]
fn raw_body_the_pinned_parser_rejects_fires() {
    // 3.3-only shapes are rule 8's designated payload — the finding
    // is the advisory, never an error.
    let findings = lint_alone("raw { DEFINE TABLE future TYPE LIGHTWEIGHT; };");
    assert_eq!(codes(&findings), ["SDL-L0012"]);
}

#[test]
fn parseable_raw_bodies_do_not_fire() {
    assert!(lint_alone("raw { DEFINE TABLE t TYPE ANY SCHEMALESS; };").is_empty());
}

#[test]
fn sentinel_in_raw_body_fires() {
    let findings = lint_alone(
        "raw { DEFINE ACCESS a ON DATABASE TYPE JWT ALGORITHM HS512 \
         KEY '[REDACTED]'; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0013"]);
}

#[test]
fn symmetric_access_keys_fire_sentinel_apply() {
    // The shape-only rule pins hs* verify keys and every issuer key
    // to the sentinel — applying defines the literal as key material.
    let findings = lint_alone(
        "access a { on: database; type: jwt; algorithm: hs512; \
         key: '[REDACTED]'; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0013"]);
}

#[test]
fn asymmetric_public_verify_keys_do_not_fire() {
    assert!(
        lint_alone(
            "access a { on: database; type: jwt; algorithm: es256; \
         key: 'MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE'; };"
        )
        .is_empty()
    );
}

#[test]
fn sentinel_passhash_fires() {
    let findings = lint_alone("user a { on: root; passhash: '[REDACTED]'; };");
    assert_eq!(codes(&findings), ["SDL-L0013"]);
}

// ---------------------------------------------------------------------
// SDL-L0014 — the delete-full ambiguity
// ---------------------------------------------------------------------

#[test]
fn delete_full_group_fires_but_shorthand_does_not() {
    // L0014's hazard is the GROUP spelling: INFO elides
    // `FOR delete FULL`, so the stored state silently loses it. The
    // shorthand round-trips (INFO prints `PERMISSIONS FULL`) — but it
    // is world-open, so the new SDL-L0001 takes it instead.
    let findings = lint_alone(
        "table a { permissions { select: full; delete: full; }; };\
         \ntable b { permissions: full; };\
         \ntable c { permissions { delete: none; }; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0014", "SDL-L0001"]);
}

// ---------------------------------------------------------------------
// SDL-L0015 — the computed/subfield cross-statement conflict
// ---------------------------------------------------------------------

/// The trigger chain, verbatim: `table t { c.x: int; };` then
/// `field c { on: t; computed: 1; };` — the nested-subfield shape.
#[test]
fn nested_subfield_then_standalone_computed_fires() {
    let findings = lint_alone(
        "table t { permissions: none; field c.x: int; };\
         \nfield c { on: t; computed: 1; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0007", "SDL-L0015"]);
}

/// The mirror shape: a top-level computed and a nested subfield.
#[test]
fn standalone_computed_then_nested_subfield_fires() {
    let findings = lint_alone(
        "table t { permissions: none; field c = 1; };\
         \nfield c.x: int { on: t; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0007", "SDL-L0015"]);
}

#[test]
fn unrelated_field_names_do_not_fire() {
    // `stat` is not `stats` — the prefix walk is segment-wise.
    let findings = lint_alone(
        "table t { permissions: none; field stat: int; };\
         \nfield stats.views: int { on: t; };",
    );
    assert!(!codes(&findings).contains(&"SDL-L0015"));
}

// ---------------------------------------------------------------------
// SDL-L0016 / L0017 / L0018 — the kind-walk promotions
// ---------------------------------------------------------------------

/// The reference-predicate trigger, verbatim: `field s: string
/// { reference; };` — plus the specced rejection `record<user> |
/// string` and the accepted shapes. Every fixture rides an
/// SDL-L0024 allow: the host `t` and the example targets are
/// deliberately outside the file (a defined `t` would fire SDL-L0007
/// on the standalone field, the noise this suite pins away).
#[test]
fn reference_kind_predicate_pins_the_engine_set() {
    let rejects = [
        "field s: string { on: t; reference; };",
        "field s: int { on: t; reference; };",
        "field s: record<user> | string { on: t; reference; };",
        "field s: array<array<record<user>>> { on: t; reference; };",
        "field s: any { on: t; reference; };",
        "field s: none { on: t; reference; };",
        "field s: array<none> { on: t; reference; };",
    ];
    for src in rejects {
        let full = format!("// surform-lint-allow: SDL-L0024\n{src}");
        assert_eq!(
            codes(&lint_alone(&full)),
            ["SDL-L0016"],
            "must reject: {src}"
        );
    }
    let accepts = [
        "field s: record<user> { on: t; reference; };",
        "field s: record { on: t; reference; };",
        "field s: option<record<user>> { on: t; reference; };",
        "field s: none | record<user> { on: t; reference; };",
        "field s: array<record<user>> { on: t; reference; };",
        "field s: set<record<user>> { on: t; reference; };",
        "field s: array<option<record<user>>> { on: t; reference; };",
        "field s: [record<user>, record<post>] { on: t; reference; };",
        "field s: record<user> | array<record<user>> { on: t; reference; };",
    ];
    for src in accepts {
        // The host `t` is not in the file, so nothing else may fire.
        let full = format!("// surform-lint-allow: SDL-L0024\n{src}");
        assert!(lint_alone(&full).is_empty(), "must accept: {src}");
    }
}

/// The id-kind compatibility mirror (upstream
/// `RecordIdKeyLit::kind_supported`). The allow prefix excuses the
/// out-of-file host, same as the reference-predicate suite above.
#[test]
fn id_field_kind_pins_the_record_id_key_set() {
    let rejects = [
        "field id: bool { on: t; };",
        "field id: float { on: t; };",
        "field id: record<user> { on: t; };",
        "field id: option<int> { on: t; };",
        "field id: duration { on: t; };",
        "field id: 1.5 { on: t; };",
        "field id: NaN { on: t; };",
    ];
    for src in rejects {
        let full = format!("// surform-lint-allow: SDL-L0024\n{src}");
        assert_eq!(
            codes(&lint_alone(&full)),
            ["SDL-L0017"],
            "must reject: {src}"
        );
    }
    let accepts = [
        "field id: any { on: t; };",
        "field id: int { on: t; };",
        "field id: number { on: t; };",
        "field id: string { on: t; };",
        "field id: uuid { on: t; };",
        "field id: array { on: t; };",
        "field id: array<int> { on: t; };",
        "field id: object { on: t; };",
        "field id: int | string { on: t; };",
        "field id: -3 { on: t; };",
        "field id: 'fixed' { on: t; };",
        "field id: [1, 2] { on: t; };",
    ];
    for src in accepts {
        // The host `t` is not in the file, so nothing else may fire.
        let full = format!("// surform-lint-allow: SDL-L0024\n{src}");
        assert!(lint_alone(&full).is_empty(), "must accept: {src}");
    }
}

/// The flexible trigger: the spec's own flexible example on a
/// bare table (SDL's default shape EMITS SCHEMALESS).
#[test]
fn flexible_on_schemaless_host_fires_nested_and_standalone() {
    let findings = lint_alone(
        "table t { permissions: none; field meta: object { flexible; }; };\
         \ntable u {};\
         \nfield extra: object { on: u; flexible; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0018", "SDL-L0007", "SDL-L0018"]);
    assert!(
        lint_alone("table t { schemafull; permissions: none; field meta: object { flexible; }; };")
            .is_empty()
    );
    // A host outside the file is SDL-L0024's advisory (excused here —
    // the flexible predicate itself stays silent; the engine's own
    // define-time rejection class is not this suite's subject).
    assert!(
        lint_alone(
            "// surform-lint-allow: SDL-L0024\nfield f: object { on: elsewhere; flexible; };"
        )
        .is_empty()
    );
}

/// The INLINE host half for STANDALONE fields (the nested form's
/// mirror is the table fold's parse error — SDL-E1044 — and never
/// reaches the linter): a non-relation host fires L0019, a relation
/// host and a host outside the file stay silent.
#[test]
fn inline_on_non_relation_host_fires_standalone() {
    let findings = lint_alone(
        "table t {};\
         \nfield score: number { on: t; inline; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0007", "SDL-L0019"]);
    // The nested form cannot reach the linter (the fold rejects it),
    // and a well-formed relation host never fires.
    let findings = lint_alone(
        "table person;\
         \ntable person -> likes -> person { enforced; permissions: none;\
         \n field score: number { inline; }; };",
    );
    assert!(findings.is_empty());
    // Host outside the file: SDL-L0024's advisory, excused (the
    // inline predicate itself stays silent).
    assert!(
        lint_alone("// surform-lint-allow: SDL-L0024\nfield f: number { on: elsewhere; inline; };")
            .is_empty()
    );
}

/// The 255-INLINE-fields cap (the adjacency payload's value count is
/// a single byte — the engine rejects the 256th at define): one
/// finding per table over the line, nothing at exactly 255, and the
/// count aggregates nested and standalone fields into one view.
#[test]
fn too_many_inline_fields_fires_per_table() {
    let mut src = String::from(
        "table person;\ntable person -> likes -> person { permissions: none; enforced;\n",
    );
    for n in 0..255 {
        src.push_str(&format!(" f{n:03}: number {{ inline; }};\n"));
    }
    src.push_str("};");
    assert!(lint_alone(&src).is_empty());
    src.push_str("field over: number { on: likes; inline; };");
    let findings = lint_alone(&src);
    // The key sort: the table-aggregate finding (`likes`) precedes the
    // standalone field's (`likes.over`).
    assert_eq!(codes(&findings), ["SDL-L0020", "SDL-L0007"]);
    match &findings[0].kind {
        FindingKind::TooManyInlineFields { count } => assert_eq!(*count, 256),
        other => panic!("expected TooManyInlineFields: {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Framework behavior
// ---------------------------------------------------------------------

#[test]
fn findings_sort_by_scope_family_key_then_code() {
    let findings = lint_alone(
        "user b { on: root; };\
         \nuser a { on: root; };\
         \nparam $z;\
         \nparam $a;",
    );
    // Param's key is the bare name (classify's shape, the INFO map
    // key); Family::Param sorts before Family::User.
    let keys: Vec<&str> = findings.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["a", "z", "a", "b"]);
    let codes: Vec<&str> = findings.iter().map(|f| f.kind.code_str()).collect();
    assert_eq!(codes, ["SDL-L0008", "SDL-L0008", "SDL-L0003", "SDL-L0003"]);
}

#[test]
fn authored_findings_carry_statement_spans() {
    let src = "user a { on: root;\n  password: 'x';\n};";
    let findings = lint_alone(src);
    let span = one(&findings, "SDL-L0002").span.expect("authored span");
    let labeled = &src[span.offset..span.offset + span.len];
    assert!(
        labeled.starts_with("user a"),
        "the label covers the statement"
    );
}

#[test]
fn clean_canonical_file_lints_silent() {
    // The spec's own example shapes: closed tables, hashed users,
    // valued params — zero findings.
    assert!(
        lint_alone(
            "namespace acme;\
         \ndatabase shop;\
         \ntable user;\
         \nuser deployer { on: database; passhash: '$argon2id$v=19$'; };\
         \ntable post {\
         \n  schemafull;\
         \n  permissions: none;\
         \n  field id: int;\
         \n  field author: record<user> { reference; };\
         \n  index by_author { fields: author; };\
         \n};\
         \nparam $page_size = 50;"
        )
        .is_empty()
    );
}

/// The no-op catch-all treats an explicit `permissions: full;` as
/// the default spelled out (the absent-`any` rule folds it to
/// absence on the read side) — resolved-state parity for the
/// authored spelling.
#[test]
fn explicit_full_any_is_still_a_noop() {
    let findings = lint_alone("api '/x' { any { permissions: full; }; };");
    assert_eq!(codes(&findings), ["SDL-L0009"]);
}

/// Upstream's `valid` has no Either arm: an `option<T>` (upstream's
/// `none | T`) in a LITERAL-ARRAY element is a define-time rejection,
/// not an acceptance.
#[test]
fn option_inside_a_literal_array_element_is_reference_rejected() {
    let findings = lint_alone(
        "// surform-lint-allow: SDL-L0024\nfield s: [none | record<user>] { on: t; reference; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0016"]);
}

/// Upstream's parser builds `option<T>` as a flattened, deduplicated
/// `Either` — `option<none>` IS bare `none` there (the singleton
/// collapse) and lands in the catch-all rejection, while the
/// doubled-`none` union flattens onto `option<record>` and passes.
#[test]
fn reference_kind_mirrors_the_either_algebra() {
    assert_eq!(
        codes(&lint_alone(
            "// surform-lint-allow: SDL-L0024\nfield s: option<none> { on: t; reference; };"
        )),
        ["SDL-L0016"]
    );
    assert!(
        lint_alone(
            "// surform-lint-allow: SDL-L0024\nfield s: none | none | record<user> { on: t; reference; };"
        )
        .is_empty()
    );
}

/// A sentinel spelled on an ASYMMETRIC verify key (the one slot that
/// stays authorable with any literal) is apply-danger too — INFO
/// renders real public keys verbatim, so only an authored one can
/// carry it.
#[test]
fn authored_sentinel_on_asymmetric_verify_key_fires() {
    let findings = lint_alone(
        "access a { on: database; type: jwt; algorithm: es256; \
         key: '[REDACTED]'; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0013"]);
}

/// The zero-`FOR` body and the no-op catch-all render their own
/// messages (the `catch_all` payload's contract).
#[test]
fn api_noop_message_shapes_split() {
    let zero_for_findings = lint_alone("api '/x' { };");
    let zero_for = one(&zero_for_findings, "SDL-L0009");
    assert!(
        format!("{zero_for}").contains("no method groups and no `any` block"),
        "{}",
        zero_for
    );
    let catch_all_findings = lint_alone("api '/x' { get { then: 1; }; };");
    let catch_all = one(&catch_all_findings, "SDL-L0009");
    assert!(
        format!("{catch_all}").contains("implicit one matches every unmatched method"),
        "{}",
        catch_all
    );
}

/// A normalized `Option` holds the union of the surviving arms:
/// `none | record<a> | record<b>` folds to
/// `Option(Union([record<a>, record<b>]))` ≡ upstream
/// `Either([None, a, b])` — accepted, at the top level and inside
/// `array`/`set` alike.
#[test]
fn multi_arm_none_unions_pass_the_reference_predicate() {
    for src in [
        "field s: none | record<user> | record<post> { on: t; reference; };",
        "field s: option<record<user> | record<post>> { on: t; reference; };",
        "field s: array<none | record<user> | record<post>> { on: t; reference; };",
    ] {
        let full = format!("// surform-lint-allow: SDL-L0024\n{src}");
        assert!(lint_alone(&full).is_empty(), "must accept: {src}");
    }
    // The multi-arm union still rejects a non-record arm.
    assert_eq!(
        codes(&lint_alone(
            "// surform-lint-allow: SDL-L0024\nfield s: none | record<user> | string { on: t; reference; };"
        )),
        ["SDL-L0016"]
    );
}

/// The conflict walk's segment comparison is decoded (`Ident.raw`
/// is the unescaped interior): a quoted first segment pairs with a
/// bare computed name — the engine's own path semantics.
#[test]
fn quoted_segments_pair_with_bare_computed_names() {
    let findings = lint_alone(
        "table t { permissions: none; field c = 1; };\
         \nfield `c`.x: int { on: t; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0007", "SDL-L0015"]);
}

// ---------------------------------------------------------------------
// SDL-L0021 — a mixin no table spreads
// ---------------------------------------------------------------------

#[test]
fn unused_mixin_fires_and_a_spread_mixin_does_not() {
    let findings = lint_alone("mixin dead { a: int; };\ntable t { b: int; };");
    assert_eq!(codes(&findings), ["SDL-L0021"]);
    let finding = one(&findings, "SDL-L0021");
    assert_eq!(finding.key, "dead");
    assert!(finding.span.is_some(), "the authored statement anchors");

    let findings = lint_alone("mixin used { a: int; };\ntable t { ...used; };");
    assert!(findings.is_empty(), "{findings:?}");
}

/// The resolved view is the EXPANDED form: a spread member is a field
/// the database would hold, so the field-level lints fire on it. Like
/// any nested child's finding, it anchors on the TABLE's span — the
/// nearest authored statement (the member itself has none).
#[test]
fn field_lints_see_the_expanded_members() {
    let findings = lint_alone("mixin m { f: int { reference; }; };\ntable t { ...m; };");
    let finding = one(&findings, "SDL-L0016");
    assert_eq!(finding.key, "t.f");
    // The authored table statement's span (the mixin's own statement
    // is skipped by the anchor walk — it pairs with no finding).
    let table_offset = "mixin m { f: int { reference; }; };\n".len();
    assert_eq!(
        finding.span.map(|span| span.offset),
        Some(table_offset),
        "anchors on the spreading table: {:?}",
        finding.span
    );
}

// ---------------------------------------------------------------------
// SDL-L0022 — a later spread's member shadows an earlier spread's
// ---------------------------------------------------------------------

/// One shadowed pair, one finding — anchored at the LATER spread's
/// entry (the author's combining action), naming both mixins and the
/// member. Fields, indexes, and events all carry the lint (per-family
/// keys: a field `x` and an index `x` never collide with each other).
#[test]
fn a_later_spreads_member_shadows_an_earlier_spreads() {
    let findings = lint_alone(
        "mixin a { x: int; y: int; };\n\
         mixin b { x: string; };\n\
         table t { ...a; ...b; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0022"]);
    match &findings[0].kind {
        FindingKind::SpreadMemberShadow {
            kind,
            key,
            earlier,
            later,
        } => {
            assert_eq!(
                (*kind, key.as_str(), earlier.as_str(), later.as_str()),
                ("field", "x", "a", "b")
            );
        }
        other => panic!("expected SpreadMemberShadow: {other:?}"),
    }
    assert_eq!(findings[0].key, "t", "the subject is the spreading table");
    // The anchor is the later spread's `...b` site, not the table.
    let later_offset =
        "mixin a { x: int; y: int; };\nmixin b { x: string; };\ntable t { ...a; ".len();
    assert_eq!(findings[0].span.map(|span| span.offset), Some(later_offset));

    // Index members shadow too — and a field `x` beside an index `x`
    // is NOT a shadow (distinct namespaces).
    let findings = lint_alone(
        "mixin a { x: int; index by_x { fields: x; }; };\n\
         mixin b { index by_x { fields: x; concurrently; }; };\n\
         table t { ...a; ...b; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0022"]);
    assert!(matches!(
        &findings[0].kind,
        FindingKind::SpreadMemberShadow { kind: "index", key, .. } if key == "by_x"
    ));
    let findings = lint_alone(
        "mixin a { x: int; };\n\
         mixin b { index x { fields: z; }; };\n\
         table t { ...a; ...b; };",
    );
    assert!(
        findings.is_empty(),
        "kind-distinct keys never collide: {findings:?}"
    );
}

/// An explicit member decides a collision position-independently —
/// the author's decision, unlinted (the deliberate class); a single
/// spread never shadows; the same pair in TWO tables fires per table.
#[test]
fn explicit_members_decide_and_single_spreads_stay_clean() {
    let findings = lint_alone(
        "mixin a { x: int; };\n\
         mixin b { x: string; };\n\
         table t { x: bool; ...a; ...b; };",
    );
    assert!(
        findings.is_empty(),
        "explicit overshadow is deliberate: {findings:?}"
    );
    let findings = lint_alone("mixin a { x: int; };\ntable t { ...a; };");
    assert!(findings.is_empty(), "{findings:?}");
    let findings = lint_alone(
        "mixin a { x: int; };\n\
         mixin b { x: string; };\n\
         table t { ...a; ...b; };\n\
         table u { ...a; ...b; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0022", "SDL-L0022"]);
    assert!(
        findings.iter().any(|finding| finding.key == "u"),
        "one finding per spreading table: {findings:?}"
    );
}

/// A three-way collision names only the REAL winner's shadows: c's
/// `x` beats both, so the findings pair c against a and b — the
/// a-vs-b pair never fires (b's `x` does not win at expansion, so
/// the finding's claim would be false).
#[test]
fn a_three_way_collision_names_only_the_landing_winner() {
    let findings = lint_alone(
        "mixin a { x: int; };\n\
         mixin b { x: string; };\n\
         mixin c { x: bool; };\n\
         table t { ...a; ...b; ...c; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0022", "SDL-L0022"]);
    let pairs: Vec<_> = findings
        .iter()
        .map(|finding| match &finding.kind {
            FindingKind::SpreadMemberShadow { earlier, later, .. } => {
                (earlier.as_str(), later.as_str())
            }
            other => panic!("expected SpreadMemberShadow: {other:?}"),
        })
        .collect();
    assert_eq!(
        pairs,
        [("a", "c"), ("b", "c")],
        "the winner c names both shadows"
    );
}

// ---------------------------------------------------------------------
// Allow comments — statement-scoped suppression (SDL-L0023's home)
// ---------------------------------------------------------------------

/// The core join: a leading allow comment excuses exactly its codes
/// on exactly its statement — the same finding on another statement
/// survives.
#[test]
fn allow_suppresses_statement_scoped() {
    let findings = lint_alone(
        "// surform-lint-allow: SDL-L0001\ntable countries { permissions: full; };\n\
         table other { permissions: full; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0001"]);
    assert_eq!(findings[0].key, "other", "countries' finding is gone");

    // A code the statement does not carry suppresses nothing — and
    // never another statement's finding.
    let findings =
        lint_alone("// surform-lint-allow: SDL-L0006\ntable countries { permissions: full; };");
    assert_eq!(codes(&findings), ["SDL-L0001"]);
}

/// Several codes on one line (comma-listed), several comment lines,
/// and blank lines between the comments and the statement (greedy
/// attachment — fmt collapses those blanks, so greedy is the stable
/// rule).
#[test]
fn allow_reads_comma_lists_across_comment_blocks_and_blanks() {
    let findings = lint_alone(
        "// a note first — not a directive\n\
         // surform-lint-allow: SDL-L0001, SDL-L0014\n\n\
         // surform-lint-allow: SDL-L0006\n\
         table t { permissions { select: full; create: full; update: full; delete: full; }; };",
    );
    assert!(findings.is_empty(), "{findings:?}");
}

/// The same-line trailing variant — the one-liner's natural home
/// (fmt keeps it on the line). The same-line test is the parser's
/// close-note model (all trivia skipped, then the line compared), so
/// exotic horizontal spacing — a no-break space between `;` and the
/// comment — attaches exactly like a plain space (fmt re-anchors it
/// as the close note, and the two models must agree).
#[test]
fn allow_trailing_same_line() {
    let findings =
        lint_alone("table u {};\nfield f: int { on: u; }; // surform-lint-allow: SDL-L0007");
    assert!(findings.is_empty(), "{findings:?}");
    let findings =
        lint_alone("table u {};\nfield f: int { on: u; };\u{a0}// surform-lint-allow: SDL-L0007");
    assert!(findings.is_empty(), "{findings:?}");
    // A trailing comment on a DIFFERENT line (the next statement's
    // leading trivia) does not excuse the previous statement.
    let findings =
        lint_alone("field f: int { on: u; };\n// surform-lint-allow: SDL-L0007\ntable u {};");
    assert_eq!(codes(&findings), ["SDL-L0007"]);
}

/// Suppression rides the finding's anchor, so the statement-level
/// granularity covers nested anchors: SDL-L0022's finding anchors at
/// the later spread INSIDE the table body, and the table statement's
/// allow excuses its whole statement.
#[test]
fn allow_on_a_table_covers_its_nested_spread_finding() {
    let findings = lint_alone(
        "mixin a { x: int; };\n\
         mixin b { x: string; };\n\
         // surform-lint-allow: SDL-L0022\n\
         table t { ...a; ...b; };",
    );
    assert!(findings.is_empty(), "{findings:?}");
}

/// Raw statements carry positional anchors — the allow rides the same
/// statement walk, so rule 8's advisory is suppressible (a deliberate
/// legacy body is its poster child).
#[test]
fn allow_covers_raw_statements() {
    let findings = lint_alone(
        "// surform-lint-allow: SDL-L0012\n\
         raw { DEFINE TABLE future TYPE LIGHTWEIGHT; };",
    );
    assert!(findings.is_empty(), "{findings:?}");
}

/// Mixins carry authored-pass findings — same walk, same join.
#[test]
fn allow_covers_the_unused_mixin() {
    let findings = lint_alone("// surform-lint-allow: SDL-L0021\nmixin dead { a: int; };");
    assert!(findings.is_empty(), "{findings:?}");
}

/// An unknown code warns (the typo trap) and never suppresses — and
/// a marker typo or an empty code list is flagged as malformed, so
/// no misspelling at a statement boundary fails silently (inside a
/// statement body comments are not attachment points at all — specced
/// silence). SDL-L0023 itself cannot be allowed.
#[test]
fn allow_problems_are_findings_and_never_suppress() {
    let findings = lint_alone(
        "// surform-lint-allow: SDL-L0001, SDL-L9999\n\
         table t { permissions: full; };",
    );
    // The mixed list works half-and-half: the registered code
    // suppresses, the unknown one warns.
    assert_eq!(codes(&findings), ["SDL-L0023"]);
    let warning = one(&findings, "SDL-L0023");
    assert!(warning.span.is_some(), "the comment anchors the warning");
    match &warning.kind {
        FindingKind::LintAllowProblem {
            problem: LintAllowProblem::UnknownCode(code),
        } => assert_eq!(code, "SDL-L9999"),
        other => panic!("expected UnknownCode: {other:?}"),
    }

    // Marker typos and empty lists: malformed, and the stem check is
    // case-insensitive so a case typo trips it too.
    for marker in [
        "surform-lint-alow: SDL-L0001",
        "surform-lint-allow SDL-L0001",
        "surform-lint-allow:",
        "Surform-Lint-Allow: SDL-L0001",
    ] {
        let findings = lint_alone(&format!("// {marker}\ntable t {{}};"));
        assert_eq!(
            codes(&findings),
            ["SDL-L0023"],
            "`{marker}` was not flagged"
        );
    }

    // An allow naming SDL-L0023 suppresses nothing: the warning's
    // span sits past the statement's `;` (outside every statement
    // span), and the kind immunity in the join is the defense in
    // depth behind that geography.
    let findings = lint_alone("table t {}; // surform-lint-allow: SDL-L9999, SDL-L0023");
    assert_eq!(codes(&findings), ["SDL-L0023"]);
}

/// An allow comment with no following statement (end of file) is
/// flagged — nothing to attach to.
#[test]
fn unattached_allow_warns() {
    let findings = lint_alone("table t {};\n// surform-lint-allow: SDL-L0001");
    assert_eq!(codes(&findings), ["SDL-L0023"]);
    assert!(matches!(
        &one(&findings, "SDL-L0023").kind,
        FindingKind::LintAllowProblem {
            problem: LintAllowProblem::Unattached
        }
    ));
}

/// The legal unterminated tail (a final statement without its `;` —
/// the parser accepts it) is a STATEMENT: the allow attaches, the
/// suppression works, and the walk never reports it unattached (nor
/// renders its findings sourceless — the tail's span runs to end of
/// source).
#[test]
fn allow_works_on_the_unterminated_tail() {
    let findings = lint_alone("// surform-lint-allow: SDL-L0001\ntable t { permissions: full; }");
    assert!(findings.is_empty(), "{findings:?}");
    // Without the allow the finding fires, anchored (not sourceless).
    let findings = lint_alone("table t { permissions: full; }");
    let finding = one(&findings, "SDL-L0001");
    assert!(
        finding.span.is_some(),
        "the tail statement anchors: {finding:?}"
    );
}

/// Sourceless findings (adoptee-only statements) have no authored
/// statement — an allow comment has nothing to excuse and the finding
/// survives even though the file does carry an allow (attached to the
/// namespace statement, the nearest thing to the adoptee).
#[test]
fn sourceless_findings_are_not_allowable() {
    let sdl = "// surform-lint-allow: SDL-L0001\nnamespace test;\ndatabase scratch;";
    let findings = lint_merged(
        sdl,
        "{ tables: { adoptee: 'DEFINE TABLE adoptee TYPE NORMAL \
         SCHEMALESS PERMISSIONS FULL' } }",
    );
    // The adoptee's finding is sourceless (span None) — no authored
    // statement to excuse, so the allow suppresses nothing.
    let finding = one(&findings, "SDL-L0001");
    assert_eq!(finding.key, "adoptee");
    assert!(finding.span.is_none(), "the adoptee renders sourceless");
}

/// The marker round-trips the canonical writer: the file re-parses
/// and the suppression still holds — fmt and normalize keep the
/// comment at its anchor (verified at the binary level in the unit's
/// probes; this pins the writer path). BOTH forms pin: the leading
/// comment stays the statement's first leading note, and the
/// trailing comment stays on the statement's line (a writer change
/// that re-anchored it as the next statement's leading trivia would
/// silently retarget the allow).
#[test]
fn allow_survives_the_canonical_writer() {
    let source = "// surform-lint-allow: SDL-L0001\ntable countries {\n  permissions: full;\n};\n";
    let file = parse(source);
    let canonical = surform::info::write::write_canonical(&file);
    assert!(
        canonical.contains("// surform-lint-allow: SDL-L0001"),
        "the directive rides the writer: {canonical}"
    );
    let findings = lint_alone(&canonical);
    assert!(findings.is_empty(), "{findings:?}");

    let source =
        "table person {};\nfield nick: string { on: person; }; // surform-lint-allow: SDL-L0007\n";
    let file = parse(source);
    let canonical = surform::info::write::write_canonical(&file);
    // The multi-line statement's trailing comment rides its CLOSING
    // line (`};`) — still after the terminator, still before the next
    // statement, still attached.
    let directive_line = canonical
        .lines()
        .find(|line| line.contains("surform-lint-allow"))
        .expect("the trailing directive rides the writer");
    assert!(
        directive_line.contains("}; // surform-lint-allow"),
        "the trailing directive stays on its statement's closing line: {canonical}"
    );
    assert!(lint_alone(&canonical).is_empty(), "{canonical}");
}

// ---------------------------------------------------------------------
// SDL-L0024 — the reference lint
// ---------------------------------------------------------------------

#[test]
fn undefined_table_reference_fires_and_a_defined_one_passes() {
    let findings = lint_alone(
        "table page { permissions { select: (SELECT * FROM usr WHERE owner = $auth.id); }; };",
    );
    let finding = one(&findings, "SDL-L0024");
    match &finding.kind {
        FindingKind::UndefinedReference {
            kind: refs::RefKind::Table,
            name,
            site,
        } => {
            assert_eq!(name, "usr");
            assert_eq!(*site, "in this table permission");
        }
        other => panic!("expected UndefinedReference: {other:?}"),
    }
    assert!(format!("{finding}").contains("`usr`"), "{finding}");

    let findings = lint_alone(
        "table usr;\
         \ntable page { permissions { select: (SELECT * FROM usr WHERE owner = $auth.id); }; };",
    );
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn undefined_analyzer_reference_fires() {
    let findings =
        lint_alone("table post { index fulltext ix { fields: title; analyzer: nope; }; };");
    let finding = one(&findings, "SDL-L0024");
    match &finding.kind {
        FindingKind::UndefinedReference {
            kind: refs::RefKind::Analyzer,
            name,
            ..
        } => assert_eq!(name, "nope"),
        other => panic!("expected UndefinedReference: {other:?}"),
    }
}

#[test]
fn undefined_fn_call_fires_and_a_defined_one_passes() {
    let findings = lint_alone("table t { event e { when: true; then: fn::typo(); }; };");
    match &one(&findings, "SDL-L0024").kind {
        FindingKind::UndefinedReference {
            kind: refs::RefKind::Fn,
            name,
            ..
        } => assert_eq!(name, "typo"),
        other => panic!("expected UndefinedReference: {other:?}"),
    }
    let findings = lint_alone(
        "fn typo() { return 1; };\
         \ntable t { event e { when: true; then: fn::typo(); }; };",
    );
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn module_silo_and_ml_calls_stay_covered_but_a_stray_mod_does_not() {
    // `mod::time::now` resolves against module `time` (the engine
    // loads `mod::time` by the call's first segment); `silo::`/`ml::`
    // names carry their version tail and are deployed artifacts.
    let covered = [
        "module time { from: f'uploads:/modules/time.surrealism'; unsigned; };".to_string(),
        "table t { event e { when: true; then: mod::time::now(); }; };".to_string(),
        "table t { event e { when: true; then: ml::embed<1.0.0>(1); }; };".to_string(),
        "table t { event e { when: true; then: silo::org::pkg::<1.0.0>::run(); }; };".to_string(),
    ];
    assert!(lint_alone(&covered.join("\n")).is_empty());
    // A builtin path resolves without any definition — api middleware
    // heads are licensed bare, and the engine parses the name with the
    // ordinary fn-path parser (Function::Normal dispatch).
    assert!(lint_alone("config api { middleware: string::uppercase('x'); };").is_empty());
    // Without the module definition, the mod:: call is a typo class —
    // unless the mangled name doubles as builtin vocabulary
    // (`mod::time::now` mangles to `time::now`, a builtin path, and
    // stays muted: mute over guess), so this probe uses a name the
    // builtin table cannot cover.
    let findings = lint_alone("table t { event e { when: true; then: mod::custom::thing(); }; };");
    match &one(&findings, "SDL-L0024").kind {
        FindingKind::UndefinedReference {
            kind: refs::RefKind::Fn,
            name,
            ..
        } => assert_eq!(name, "custom::thing"),
        other => panic!("expected UndefinedReference: {other:?}"),
    }
}

#[test]
fn fn_body_params_fire_only_where_sound() {
    // `$b` is bound nowhere; `$a` is an argument; `$auth` is session
    // ambient; `$this` rides the doc trio on a document-bound call.
    let findings = lint_alone("fn f($a: string) { return $b + $a + $auth.id + $this.x; };");
    let finding = one(&findings, "SDL-L0024");
    match &finding.kind {
        FindingKind::UndefinedReference {
            kind: refs::RefKind::Param,
            name,
            ..
        } => assert_eq!(name, "b"),
        other => panic!("expected UndefinedReference: {other:?}"),
    }
    assert!(
        format!("{finding}").contains("no `param` defines it"),
        "{finding}"
    );
    // A `DEFINE PARAM` DOES satisfy a fn-body read — the engine's
    // param read falls through to the catalog (`get_db_param`) even
    // on the isolated body context (probe: `RETURN fn::probe()`
    // returns the param's value). The definition set covers it, so
    // the read is clean.
    assert!(
        lint_alone(
            "param $greeting = 'hi';\
             \nfn f() { return $greeting; };"
        )
        .is_empty()
    );
    // Everywhere else params may bind client values — muted, never
    // guessed.
    assert!(
        lint_alone(
            "param $org = 'acct';\
             \ntable t { permissions { select: org = $org; }; };"
        )
        .is_empty()
    );
    assert!(lint_alone("table t { field x = $client_value; };").is_empty());
}

#[test]
fn definitions_are_scope_keyed() {
    // `elsewhere` is defined in database two; the view in database one
    // selects from it — the same-named table in another scope does NOT
    // satisfy the reference (the engine bails `TbNotFound`: catalog
    // lookups are per database).
    let findings = lint_alone(
        "database two;\
         \ntable elsewhere;\
         \ndatabase one;\
         \ntable view v { select * from elsewhere; };",
    );
    match &one(&findings, "SDL-L0024").kind {
        FindingKind::UndefinedReference { name, .. } => assert_eq!(name, "elsewhere"),
        other => panic!("expected UndefinedReference: {other:?}"),
    }
    // Same database: defined, silent.
    let findings = lint_alone(
        "database one;\
         \ntable elsewhere;\
         \ntable view v { select * from elsewhere; };",
    );
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn field_reference_targets_warn_only_when_undefined() {
    // Field REFERENCE targets and kind surfaces are stored, not
    // resolved at apply time — the compiler orders emission anyway,
    // so a target defined later in the file stays silent (SDL-L0025,
    // the old apply-order advisory, is retired); only the undefined
    // form advises (SDL-L0024's class).
    let findings = lint_alone(
        "table t { field owner: record<org> { reference; }; };\
         \ntable org;",
    );
    assert!(findings.is_empty(), "{findings:?}");
    let findings = lint_alone("table t { field owner: record<org> { reference; }; };");
    match &one(&findings, "SDL-L0024").kind {
        FindingKind::UndefinedReference {
            kind: refs::RefKind::Table,
            name,
            ..
        } => assert_eq!(name, "org"),
        other => panic!("expected UndefinedReference: {other:?}"),
    }
}

#[test]
fn resolved_adopted_definitions_satisfy_references() {
    // The database holds `usr` — the merged state defines it, so the
    // reference resolves even though the authored file never declares
    // the table (the resolved pass evaluates what the DB would hold).
    let findings = lint_merged(
        "namespace test;\ndatabase scratch;\n\
         table page { permissions { select: (SELECT * FROM usr); }; };",
        "{ tables: { usr: 'DEFINE TABLE usr TYPE NORMAL SCHEMALESS' } }",
    );
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn duplicate_references_dedup_per_statement() {
    // The report is positional; the finding is not — one typo, one
    // finding per statement.
    let findings = lint_alone(
        "table t { permissions { select: (SELECT * FROM ghost) || (SELECT * FROM ghost); }; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0024"]);
}

#[test]
fn allow_suppresses_the_reference_lint() {
    let findings = lint_alone(
        "// surform-lint-allow: SDL-L0024\n\
         table view v { select * from ghost; };",
    );
    assert!(findings.is_empty(), "{findings:?}");
    // A statement-scoped allow does not leak to the next statement.
    let findings = lint_alone(
        "// surform-lint-allow: SDL-L0024\n\
         table view v { select * from ghost; };\
         \ntable w { permissions { select: (SELECT * FROM ghost); }; };",
    );
    assert_eq!(codes(&findings), ["SDL-L0024"]);
    // The retired SDL-L0025 is no longer in the registry — an allow
    // naming it is the unknown-code class (SDL-L0023), never a
    // suppressor.
    let findings = lint_alone(
        "// surform-lint-allow: SDL-L0025\n\
         table view v { select * from ghost; };",
    );
    match &one(&findings, "SDL-L0023").kind {
        FindingKind::LintAllowProblem {
            problem: LintAllowProblem::UnknownCode(code),
        } => assert_eq!(code, "SDL-L0025"),
        other => panic!("expected UnknownCode: {other:?}"),
    }
    assert_eq!(codes(&findings), ["SDL-L0023", "SDL-L0024"]);
}
