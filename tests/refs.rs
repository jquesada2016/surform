//! Golden fixtures for the reference extractor
//! ([`surform::refs`]): one capture site / classification case
//! per test, plain asserts over (kind, name) set equality with
//! site/stmt pinned where meaningful — no insta goldens. The corpus
//! leg at the bottom rides the per-commit menus (totality +
//! determinism only; the audit already pins the doc counts).

#[path = "common/mod.rs"]
mod common;

use std::collections::BTreeSet;

use surform::parser;
use surform::refs::{self, DynamicWhy, RefKind, Report, Site};

fn extract(src: &str) -> Report {
    let (file, diags) = parser::parse_file(src);
    assert!(diags.is_empty(), "the fixture must parse clean: {diags:?}");
    refs::extract_file(&file)
}

fn extract_composable(src: &str) -> Report {
    let (file, diags) = parser::parse_file_composable(src);
    assert!(diags.is_empty(), "the fixture must parse clean: {diags:?}");
    refs::extract_file(&file)
}

/// Every (kind, name) pair in the report, deduplicated for set
/// assertions — the report itself stays positional (no dedup).
fn pairs(report: &Report) -> BTreeSet<(RefKind, &str)> {
    report
        .refs
        .iter()
        .map(|r| (r.kind, r.name.as_str()))
        .collect()
}

/// The pairs at one capture site.
fn pairs_at(report: &Report, site: Site) -> BTreeSet<(RefKind, &str)> {
    report
        .refs
        .iter()
        .filter(|r| r.site == site)
        .map(|r| (r.kind, r.name.as_str()))
        .collect()
}

fn dynamics(report: &Report, why: DynamicWhy) -> Vec<(usize, Site)> {
    report
        .dynamics
        .iter()
        .filter(|d| d.why == why)
        .map(|d| (d.stmt, d.site))
        .collect()
}

// ---------------------------------------------------------------------
// Tier 1: the SDL surfaces
// ---------------------------------------------------------------------

#[test]
fn record_and_table_kinds_are_table_refs() {
    let report = extract(
        "table person { field partner: option<record<person | ghost>>;\
         field tags: array<table<tag>>; field loc: geometry<point>; };",
    );
    assert_eq!(
        pairs(&report),
        BTreeSet::from([
            (RefKind::Table, "person"),
            (RefKind::Table, "ghost"),
            (RefKind::Table, "tag"),
        ])
    );
    // Kinds are structural: every ref rides its statement at Other.
    assert!(
        report
            .refs
            .iter()
            .all(|r| r.stmt == 0 && r.site == Site::Other)
    );
}

#[test]
fn field_default_with_subquery() {
    let report = extract(
        "table t { field code: string { default: (select value id from codes where active = true limit 1); }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::FieldDefault),
        BTreeSet::from([(RefKind::Table, "codes")])
    );
    assert_eq!(report.refs[0].stmt, 0);
}

#[test]
fn field_assert_with_fn_and_value_reads() {
    let report = extract(
        "table t { field email: string { assert: string::is_email($value) && fn::check($value); }; };",
    );
    // `string::is::email` is an ordinary builtin — no ref, no mute.
    assert_eq!(
        pairs_at(&report, Site::FieldAssert),
        BTreeSet::from([(RefKind::Fn, "check"), (RefKind::Param, "value"),])
    );
    assert_eq!(dynamics(&report, DynamicWhy::DynamicCall), vec![]);
}

#[test]
fn field_value_and_computed_sites() {
    let report = extract(
        "table t { field score: int { value: math::max([$value, 1]); };\
         field total { computed: $price * $qty; }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::FieldValue),
        BTreeSet::from([(RefKind::Param, "value")])
    );
    assert_eq!(
        pairs_at(&report, Site::FieldComputed),
        BTreeSet::from([(RefKind::Param, "price"), (RefKind::Param, "qty")])
    );
}

#[test]
fn table_and_field_permissions() {
    let report = extract(
        "table t { permissions { select, create: $auth.admin || $this.owner; };\
         field x: string { permissions { select: $auth.admin == true && $value != none; }; }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::TablePermission),
        BTreeSet::from([(RefKind::Param, "auth"), (RefKind::Param, "this")])
    );
    assert_eq!(
        pairs_at(&report, Site::FieldPermission),
        BTreeSet::from([(RefKind::Param, "auth"), (RefKind::Param, "value")])
    );
}

#[test]
fn event_when_then_sites() {
    let report = extract(
        "event audit { on: t; when: $event == 'CREATE' && $before.active != $after.active;\
         then: (create log set id = $after.id); };",
    );
    // The standalone `on:` host is a Table ref.
    assert_eq!(
        pairs(&report),
        BTreeSet::from([
            (RefKind::Table, "t"),
            (RefKind::Table, "log"),
            (RefKind::Param, "event"),
            (RefKind::Param, "before"),
            (RefKind::Param, "after"),
        ])
    );
    assert_eq!(
        pairs_at(&report, Site::EventWhen),
        BTreeSet::from([
            (RefKind::Param, "event"),
            (RefKind::Param, "before"),
            (RefKind::Param, "after"),
        ])
    );
    assert_eq!(
        pairs_at(&report, Site::EventThen),
        BTreeSet::from([(RefKind::Table, "log"), (RefKind::Param, "after"),])
    );
    assert_eq!(
        report.refs[0].stmt, 0,
        "the on: host rides the event's own statement"
    );
}

/// The view leg: FROM targets AND graph edges inside the view body —
/// this one proves the view reader works.
#[test]
fn view_select_targets_and_graph_edges() {
    let report = extract(
        "table view gv { select author, ->(knows, buddy) AS edges, ->?, <~called \
         FROM person WHERE ->likes->post != none AND $this.active != none; };",
    );
    assert_eq!(
        pairs_at(&report, Site::View),
        BTreeSet::from([
            (RefKind::Table, "person"),
            (RefKind::Table, "knows"),
            (RefKind::Table, "buddy"),
            (RefKind::Table, "called"),
            (RefKind::Table, "likes"),
            (RefKind::Table, "post"),
            (RefKind::Param, "this"),
        ])
    );
    // The view table's own name is a definition, not a ref.
    assert!(!pairs(&report).contains(&(RefKind::Table, "gv")));
    assert_eq!(report.refs[0].stmt, 0);
}

#[test]
fn relation_endpoints_are_table_refs() {
    let report = extract("table person -> wrote -> post;");
    assert_eq!(
        pairs(&report),
        BTreeSet::from([(RefKind::Table, "person"), (RefKind::Table, "post")])
    );
    // A self-relation still holds its endpoints — positional, no dedup.
    let report = extract("table person -> knows -> person;");
    let endpoints: Vec<_> = report.refs.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(endpoints, vec!["person", "person"]);
    // Unnamed sides hold nothing.
    let report = extract("table relation edge;");
    assert!(report.refs.is_empty());
}

#[test]
fn fulltext_analyzer_and_count_where() {
    let report = extract(
        "table t { field body: string; };\
         index fulltext f { on: t; fields: body; analyzer: english; };\
         index count c { on: t; where: body != none && fn::eligible($this); };",
    );
    assert_eq!(
        pairs(&report),
        BTreeSet::from([
            (RefKind::Analyzer, "english"),
            (RefKind::Table, "t"),
            (RefKind::Fn, "eligible"),
            (RefKind::Param, "this"),
        ])
    );
    let analyzer = report
        .refs
        .iter()
        .find(|r| r.kind == RefKind::Analyzer)
        .expect("the analyzer ref");
    assert_eq!((analyzer.stmt, analyzer.site), (1, Site::Other));
}

#[test]
fn analyzer_function_paths_are_fn_refs() {
    let report = extract("analyzer stem { function: fn::my::stem; };");
    assert_eq!(pairs(&report), BTreeSet::from([(RefKind::Fn, "my::stem")]));
    // `mod::` heads lose their prefix the same way.
    let report = extract("analyzer stem { function: mod::demo::stem; };");
    assert_eq!(
        pairs(&report),
        BTreeSet::from([(RefKind::Fn, "demo::stem")])
    );
}

#[test]
fn param_value_site() {
    let report = extract("param $g { value: fn::make($base) + string::len('x'); };");
    assert_eq!(
        pairs_at(&report, Site::ParamValue),
        BTreeSet::from([(RefKind::Fn, "make"), (RefKind::Param, "base")])
    );
}

#[test]
fn access_capture_sites() {
    let report = extract(
        "access rec { on: db; type: record;\
         signup: create user set email = $email, password = crypto::argon2::generate($password);\
         signin: select * from user where email = $email;\
         authenticate: $auth.active && fn::allow($token);\
         context: { tier: $token.tier }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::AccessSignup),
        BTreeSet::from([
            (RefKind::Table, "user"),
            (RefKind::Param, "email"),
            (RefKind::Param, "password"),
        ])
    );
    assert_eq!(
        pairs_at(&report, Site::AccessSignin),
        BTreeSet::from([(RefKind::Table, "user"), (RefKind::Param, "email"),])
    );
    assert_eq!(
        pairs_at(&report, Site::AccessAuthenticate),
        BTreeSet::from([
            (RefKind::Param, "auth"),
            (RefKind::Param, "token"),
            (RefKind::Fn, "allow"),
        ])
    );
    assert_eq!(
        pairs_at(&report, Site::AccessContext),
        BTreeSet::from([(RefKind::Param, "token"),])
    );
}

#[test]
fn fn_body_with_locals_subquery_and_graph_tail() {
    let report = extract(
        "fn audit::trail($who: record<person>) {\
         let $seen = (select value id from viewed where viewer = $who limit 1);\
         if $seen != none { return $seen.*; };\
         for $v in (select * from post where author = $who limit 10) {\
         create log set who = $who, what = $v.id; };\
         return $who->wrote->post; };",
    );
    // The fn's own name is a definition, not a ref.
    assert!(!pairs(&report).contains(&(RefKind::Fn, "audit::trail")));
    // The arg kind's record target rides Tier 1 at Other; the body's
    // tables ride FnBody; every param read resolves to a local (the
    // fn arg, the LET, the FOREACH param).
    assert_eq!(
        pairs(&report),
        BTreeSet::from([
            (RefKind::Table, "person"),
            (RefKind::Table, "viewed"),
            (RefKind::Table, "post"),
            (RefKind::Table, "log"),
            (RefKind::Table, "wrote"),
        ])
    );
    assert_eq!(
        pairs_at(&report, Site::Other),
        BTreeSet::from([(RefKind::Table, "person")])
    );
    assert!(report.refs.iter().all(|r| r.kind != RefKind::Param));
}

#[test]
fn config_graphql_and_api_middleware() {
    let report = extract(
        "config graphql { tables { include: user, `order`; };\
         functions { include: fn::a::b, fn::c; }; };\
         api '/x' { get { middleware: api::timeout(5s), fn::audit('write'),\
         mod::pkg::check(1000); then: (create log); }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::Other),
        BTreeSet::from([
            (RefKind::Table, "user"),
            (RefKind::Table, "order"),
            (RefKind::Fn, "a::b"),
            (RefKind::Fn, "c"),
            // Middleware heads: bare / fn:: / mod:: all lose their prefix.
            (RefKind::Fn, "api::timeout"),
            (RefKind::Fn, "audit"),
            (RefKind::Fn, "pkg::check"),
        ])
    );
    // The action's `then` body rides the per-request ApiAction site.
    assert_eq!(
        pairs_at(&report, Site::ApiAction),
        BTreeSet::from([(RefKind::Table, "log")])
    );
    // stmt 0 is the config, stmt 1 the api statement.
    let api_pairs: BTreeSet<_> = report
        .refs
        .iter()
        .filter(|r| r.stmt == 1)
        .map(|r| (r.kind, r.name.as_str()))
        .collect();
    assert_eq!(
        api_pairs,
        BTreeSet::from([
            (RefKind::Fn, "api::timeout"),
            (RefKind::Fn, "audit"),
            (RefKind::Fn, "pkg::check"),
            (RefKind::Table, "log"),
        ])
    );
}

#[test]
fn mixin_members_walk_and_spreads_are_mixin_refs() {
    let report = extract(
        "mixin stamp { f: int { default: rand() * 2; };\
         index f_idx { fields: f; };\
         event f_evt { when: $before.f != $after.f; then: 1; }; };\
         table m { ...stamp; };",
    );
    assert_eq!(
        pairs(&report),
        BTreeSet::from([
            (RefKind::Param, "before"),
            (RefKind::Param, "after"),
            (RefKind::Mixin, "stamp"),
        ])
    );
    // The members attribute to the MIXIN's statement (0); the spread
    // to the spreading table (1).
    let spread = report
        .refs
        .iter()
        .find(|r| r.kind == RefKind::Mixin)
        .expect("the spread ref");
    assert_eq!((spread.stmt, spread.site), (1, Site::Other));
    assert!(
        report
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Param)
            .all(|r| r.stmt == 0)
    );
}

#[test]
fn multi_subject_graph_empty_target_and_reference_edge() {
    // The paren multi-subject graph yields one ref per subject; `?`
    // targets every edge (no names); `<~` reference edges carry the
    // same subjects.
    let report = extract(
        "table q { field z1: string { value: $value->(a, b); };\
         field z2: string { value: $value->?; };\
         field z3: string { value: $value<~back; }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::FieldValue),
        BTreeSet::from([
            (RefKind::Param, "value"),
            (RefKind::Table, "a"),
            (RefKind::Table, "b"),
            (RefKind::Table, "back"),
        ])
    );
}

/// A graph range subject's bounds are key literals whose array and
/// object forms carry expressions — the range subtree walks with the
/// subject's table.
#[test]
fn graph_range_bounds_are_walked() {
    let report = extract(
        "table q { field z1: string { value: $value->t:[$p]..3; };\
         field z2: string { value: $value->t:{a: $p}..; }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::FieldValue),
        BTreeSet::from([
            (RefKind::Param, "value"),
            (RefKind::Table, "t"),
            (RefKind::Param, "p"),
        ])
    );
}

/// INSERT `VALUES` column idioms are full idioms — graph parts and
/// `[value]`/`[WHERE …]` parts walked, exactly like index fields.
#[test]
fn insert_values_column_idioms_are_walked() {
    let report = extract(
        "fn i() { insert into t (a[$p]) values (1); };\
         fn j() { insert into t (->g->h[$p]) values (1); };\
         fn k() { insert into t (a[where ->w->x != none]) values (1); };",
    );
    assert_eq!(
        pairs_at(&report, Site::FnBody),
        BTreeSet::from([
            (RefKind::Table, "t"),
            (RefKind::Param, "p"),
            (RefKind::Table, "g"),
            (RefKind::Table, "h"),
            (RefKind::Table, "w"),
            (RefKind::Table, "x"),
        ])
    );
}

#[test]
fn record_id_literals_and_mocks_are_table_refs() {
    let report = extract(
        "table r { field x: string { value: array::includes([person:123, |mock_tb:5|], $value);\
         }; field y: string { default: (select * from person:9, |other_tb:1..3| limit 1) != none; }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::FieldValue),
        BTreeSet::from([
            (RefKind::Table, "person"),
            (RefKind::Table, "mock_tb"),
            (RefKind::Param, "value"),
        ])
    );
    assert_eq!(
        pairs_at(&report, Site::FieldDefault),
        BTreeSet::from([(RefKind::Table, "person"), (RefKind::Table, "other_tb"),])
    );
}

// ---------------------------------------------------------------------
// Classification: dynamics, recovery, shadowing, self-host
// ---------------------------------------------------------------------

#[test]
fn dynamic_mutes() {
    let report = extract(
        "table d { field x: string { value: type::table($p) == 'person'; }; };\
         fn probe() { return (select value id from $this.home limit 1); };",
    );
    // The table-hiding call with a non-literal argument mutes; the
    // param read inside it still reports; the string literal is
    // inert. The expression-led FROM mutes as a target and its
    // $this read reports.
    assert_eq!(
        dynamics(&report, DynamicWhy::DynamicCall),
        vec![(0, Site::FieldValue)]
    );
    assert_eq!(
        dynamics(&report, DynamicWhy::ExpressionLedTarget),
        vec![(1, Site::FnBody)]
    );
    assert_eq!(
        pairs(&report),
        BTreeSet::from([(RefKind::Param, "p"), (RefKind::Param, "this")])
    );
    assert!(!pairs(&report).contains(&(RefKind::Table, "person")));
}

#[test]
fn literal_recovery_from_table_hiding_calls() {
    let report = extract(
        "table lr { field a: string { value: type::table('known') == $value; };\
         field b: string { value: type::record('user', $value) != none; };\
         field c: string { value: type::record('post:9') != none; }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::FieldValue),
        BTreeSet::from([
            (RefKind::Table, "known"),
            (RefKind::Table, "user"),
            (RefKind::Table, "post"),
            (RefKind::Param, "value"),
        ])
    );
    // Recovered calls leave no dynamic record behind.
    assert!(report.dynamics.is_empty());
}

/// The FROM-position classification runs ONCE per target: recovery
/// never doubles, and a fn-led target is one Fn ref plus one target
/// mute — the count pin this region was missing.
#[test]
fn from_position_classifies_exactly_once() {
    let report = extract(
        "fn probe() { return (select * from type::table('person'))[0]; };\
         fn pick() { return (select * from fn::pick_fn())[0]; };",
    );
    let person_refs = report
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Table && r.name == "person")
        .count();
    assert_eq!(person_refs, 1, "recovery must not double-report");
    let pick_refs = report
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Fn && r.name == "pick_fn")
        .count();
    assert_eq!(pick_refs, 1);
    assert_eq!(
        dynamics(&report, DynamicWhy::ExpressionLedTarget),
        vec![(1, Site::FnBody)]
    );
    assert!(
        report
            .dynamics
            .iter()
            .all(|d| d.why != DynamicWhy::DynamicCall)
    );
}

#[test]
fn jwt_sub_blocks_on_record_and_bearer() {
    let report = extract(
        "access b { on: db; type: bearer; for: user; jwt { url: fn::jwks(); }; };\
         access r { on: db; type: record; jwt { algorithm: es256; key: fn::load_key(); }; };",
    );
    // stmt 0: the bearer jwt's url; stmt 1: the record jwt's key.
    let jwt_fns: BTreeSet<_> = report
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Fn)
        .map(|r| (r.stmt, r.name.as_str()))
        .collect();
    assert_eq!(jwt_fns, BTreeSet::from([(0, "jwks"), (1, "load_key")]));
    assert!(report.refs.iter().all(|r| r.site == Site::Other));
}

#[test]
fn destructure_and_recurse_nest_graphs() {
    let report = extract(
        "table q { field z1: string { value: $value.{ a: ->knows->person }; };\
         field z2: string { value: $value->knows.{2}(->likes->post.name); }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::FieldValue),
        BTreeSet::from([
            (RefKind::Param, "value"),
            (RefKind::Table, "knows"),
            (RefKind::Table, "person"),
            (RefKind::Table, "likes"),
            (RefKind::Table, "post"),
        ])
    );
}

#[test]
fn projection_and_lookup_alias_graphs() {
    let report = extract(
        "table q { field z1: string { value: (select ->knows->person AS ->likes->post from user) != none; };\
         field z2: string { value: $value->(knows WHERE x != none AS ->shares->doc); }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::FieldValue),
        BTreeSet::from([
            (RefKind::Param, "value"),
            (RefKind::Table, "user"),
            (RefKind::Table, "knows"),
            (RefKind::Table, "person"),
            (RefKind::Table, "likes"),
            (RefKind::Table, "post"),
            (RefKind::Table, "shares"),
            (RefKind::Table, "doc"),
        ])
    );
}

#[test]
fn postfix_call_args() {
    let report = extract("table q { field z: string { value: (|$x: int| $x)(person:1); }; };");
    // The closure arg is a local (unreported); the invocation
    // argument's record-id literal references person.
    assert_eq!(
        pairs_at(&report, Site::FieldValue),
        BTreeSet::from([(RefKind::Table, "person")])
    );
    assert!(report.refs.iter().all(|r| r.kind != RefKind::Param));
}

#[test]
fn let_and_closure_kinds() {
    let report = extract(
        "fn f() { let $x: record<person> = none;\
         let $g = |$u: record<user | team>, $v: record<log>| -> record<audit> { $u };\
         return $x; };",
    );
    assert_eq!(
        pairs_at(&report, Site::FnBody),
        BTreeSet::from([
            (RefKind::Table, "person"),
            (RefKind::Table, "user"),
            (RefKind::Table, "team"),
            (RefKind::Table, "log"),
            (RefKind::Table, "audit"),
        ])
    );
}

/// `mod::` and `silo::` calls are Fn refs, family prefix dropped
/// (Tier-1 FnPath convention; silo keeps its version shape).
/// `ml::name::<maj.min.patch>()` does not parse on the pinned 3.3.0
/// grammar (probe-verified at expression-start, operand, and
/// statement positions — the lexer routes `ml::name` through the
/// identifier-path arms), so `refs` keeps its `Function::Model` arm
/// (naming `name::<version>`) reachable only through ASTs an embed
/// bump could produce; no parse-clean fixture can pin it today.
#[test]
fn module_and_silo_calls_are_fn_refs() {
    let report = extract("fn m() { return mod::pkg::check(1) + silo::org::app::<1.2.3>(); };");
    assert_eq!(
        pairs_at(&report, Site::FnBody),
        BTreeSet::from([
            (RefKind::Fn, "pkg::check"),
            (RefKind::Fn, "org::app::<1.2.3>"),
        ])
    );
}

#[test]
fn locals_shadow_param_reads() {
    let report = extract(
        "fn shadow($p: int) {\
         let $q = $p + 1;\
         let $p = $q;\
         let $clo = |$inner: int| $inner + $p;\
         for $p in [1, 2] { let $deep = $p; };\
         return $p + $q + type::table($p) == 'tb'; };",
    );
    // Every param read resolves to a local binding — LET ($q, the
    // shadowing $p), the fn arg, the closure arg, the FOREACH param.
    assert!(report.refs.iter().all(|r| r.kind != RefKind::Param));
    // The table-hiding call still fails recovery on its non-literal
    // argument (locals do not make it static) — the mute stays.
    assert_eq!(
        dynamics(&report, DynamicWhy::DynamicCall),
        vec![(0, Site::FnBody)]
    );
}

#[test]
fn self_host_skip_and_standalone_on() {
    let report = extract(
        "table person { field name: string; index ix { fields: name; }; };\
         field nickname: string { on: person; };",
    );
    // The nested member's host is the enclosing table — emits nothing.
    assert_eq!(
        pairs_at(&report, Site::Other),
        BTreeSet::from([(RefKind::Table, "person"),])
    );
    let on = report
        .refs
        .iter()
        .find(|r| r.name == "person")
        .expect("the standalone field's on: ref");
    assert_eq!(
        on.stmt, 1,
        "the standalone field's host rides its own statement"
    );
}

#[test]
fn raw_and_include_are_opaque_mutes() {
    // Includes lead the file (E1069: after a definition they are
    // misplaced).
    let report = extract_composable(
        "include \"other.surdl\";\
         table t;\
         raw { DEFINE TABLE hand_built; };",
    );
    assert!(report.refs.is_empty());
    assert_eq!(
        dynamics(&report, DynamicWhy::Opaque),
        vec![(0, Site::Other), (2, Site::Other)]
    );
}

// ---------------------------------------------------------------------
// The ambient table
// ---------------------------------------------------------------------

#[test]
fn ambient_params_cover_every_site() {
    // Extras-only: the session four never repeat as site extras —
    // consumers union with SESSION_PARAMS.
    for (site, expected) in [
        (
            Site::FieldDefault,
            &["value", "before", "after", "input", "this", "self"][..],
        ),
        (
            Site::FieldValue,
            &["value", "before", "after", "input", "this", "self"][..],
        ),
        (
            Site::FieldAssert,
            &["value", "before", "after", "input", "this", "self"][..],
        ),
        (Site::FieldComputed, &["this", "self"][..]),
        (Site::ReferenceOnDelete, &["reference", "this", "self"][..]),
        (Site::TablePermission, &["this", "self", "parent"][..]),
        (
            Site::FieldPermission,
            &[
                "value", "before", "after", "input", "this", "self", "parent",
            ][..],
        ),
        (Site::BucketPermission, &["action", "file", "target"][..]),
        (Site::CountFilter, &["this", "self"][..]),
        (Site::IndexFields, &["this", "self"][..]),
        (
            Site::EventWhen,
            &["event", "before", "after", "value", "input", "this", "self"][..],
        ),
        (
            Site::EventThen,
            &["event", "before", "after", "value", "input", "this", "self"][..],
        ),
        (Site::View, &["this", "self"][..]),
        (Site::FnBody, &[][..]),
        (Site::ParamValue, &[][..]),
        (Site::StatementPermission, &[][..]),
        (Site::ApiAction, &["request"][..]),
        (Site::AccessSignup, &[][..]),
        (Site::AccessSignin, &[][..]),
        (Site::AccessAuthenticate, &[][..]),
        (Site::AccessContext, &[][..]),
        (Site::Other, &[][..]),
    ] {
        assert_eq!(
            refs::ambient_params(site),
            expected,
            "{site:?}: the ambient table drifted"
        );
        for extra in refs::ambient_params(site) {
            assert!(
                !refs::SESSION_PARAMS.contains(extra),
                "{site:?}: {extra} is session ambient, not a site extra"
            );
        }
    }
}

/// The convention pin: the session four are ambient at EVERY site —
/// consumers union with SESSION_PARAMS — so even an empty-extras row
/// (an access signup) still sees `$auth`, and an unset one resolves
/// to `none`.
#[test]
fn session_params_are_the_ambient_baseline() {
    assert_eq!(
        refs::SESSION_PARAMS,
        &["access", "auth", "token", "session"]
    );
    let union: BTreeSet<_> = refs::SESSION_PARAMS
        .iter()
        .chain(refs::ambient_params(Site::AccessSignup))
        .copied()
        .collect();
    assert!(
        union.contains("auth"),
        "the union must see the session ambient"
    );
}

/// The changed ambient rows ($this/$self at the field and event
/// sites) pinned against real reads: the extractor reports them at
/// exactly the sites the source-verified table claims.
#[test]
fn this_self_reads_report_at_the_amended_ambient_sites() {
    let report = extract(
        "table t { field a: string { default: $this.id + $self.id; };\
         field b { computed: $self.id; }; };\
         event e { on: t; when: $this.id != $self.id; then: $this.id; };",
    );
    let pairs = pairs_at(&report, Site::FieldDefault);
    assert!(
        pairs.contains(&(RefKind::Param, "this")),
        "FieldDefault: $this"
    );
    assert!(
        pairs.contains(&(RefKind::Param, "self")),
        "FieldDefault: $self"
    );
    let pairs = pairs_at(&report, Site::FieldComputed);
    assert!(
        pairs.contains(&(RefKind::Param, "self")),
        "FieldComputed: $self"
    );
    let pairs = pairs_at(&report, Site::EventWhen);
    assert!(
        pairs.contains(&(RefKind::Param, "this")),
        "EventWhen: $this"
    );
    assert!(
        pairs.contains(&(RefKind::Param, "self")),
        "EventWhen: $self"
    );
    let pairs = pairs_at(&report, Site::EventThen);
    assert!(
        pairs.contains(&(RefKind::Param, "this")),
        "EventThen: $this"
    );
    // Row content, pinned here AND in the exhaustive test — an earlier
    // refactor dropped these rows and the walker pairs alone did not
    // catch it, so the ambient table itself is asserted too.
    for site in [
        Site::FieldDefault,
        Site::FieldValue,
        Site::FieldAssert,
        Site::FieldComputed,
    ] {
        assert!(
            refs::ambient_params(site).contains(&"this"),
            "{site:?}: the $this row regressed"
        );
        assert!(
            refs::ambient_params(site).contains(&"self"),
            "{site:?}: the $self row regressed"
        );
    }
    for site in [Site::EventWhen, Site::EventThen] {
        assert!(
            refs::ambient_params(site).contains(&"this"),
            "{site:?}: the $this row regressed"
        );
        assert!(
            refs::ambient_params(site).contains(&"self"),
            "{site:?}: the $self row regressed"
        );
    }
}

/// The two per-document evaluation sites: a field's custom
/// `on delete` strategy binds `$reference` + the remote record as
/// `$this`/`$self` (`doc/purge.rs:628-661`); a count index's filter
/// evaluates against the old and new documents (`doc/index.rs:73-88`).
#[test]
fn reference_on_delete_and_count_filter_sites() {
    let report = extract(
        "table p { field partner: record<p> { reference { on delete: $reference.id; }; }; };\
         index count c { on: p; where: $this.n > 1; };",
    );
    let reference = report
        .refs
        .iter()
        .find(|r| r.site == Site::ReferenceOnDelete)
        .expect("the on-delete strategy's $reference read");
    assert_eq!(
        (reference.stmt, reference.kind, reference.name.as_str()),
        (0, RefKind::Param, "reference")
    );
    let filter = report
        .refs
        .iter()
        .find(|r| r.site == Site::CountFilter)
        .expect("the count filter's $this read");
    assert_eq!(
        (filter.stmt, filter.kind, filter.name.as_str()),
        (1, RefKind::Param, "this")
    );
}

/// An index `fields:` entry is a FULL expression evaluated per
/// write/build like the count filter (`doc/index.rs:69-71` evaluate
/// the cols for both documents via `idiom_compute(…, Some(doc))`
/// :252) — a param read and a graph edge inside one classify at
/// IndexFields.
#[test]
fn index_fields_expressions_are_index_fields() {
    let report = extract("table t { field n: int; index f { fields: ->rel->dest, $p; }; };");
    assert_eq!(
        pairs_at(&report, Site::IndexFields),
        BTreeSet::from([
            (RefKind::Param, "p"),
            (RefKind::Table, "rel"),
            (RefKind::Table, "dest"),
        ])
    );
    let entry = report
        .refs
        .iter()
        .find(|r| r.site == Site::IndexFields)
        .expect("an IndexFields read");
    assert_eq!(entry.stmt, 0);
}

/// The permission ambient rows ($self at table/field; the verified
/// bucket set) pinned against real reads at those capture sites.
#[test]
fn permission_rows_report_their_ambient_params() {
    let report = extract(
        "table t { permissions { select: $self.id != none; }; };\
         table f_t { field x: string { permissions { select: $self.id != none; }; }; };\
         bucket b { permissions: $action == 'LIST' && $file.bucket == 'b'\
         && $target != none && $auth.id != none; };",
    );
    let pairs = pairs_at(&report, Site::TablePermission);
    assert!(
        pairs.contains(&(RefKind::Param, "self")),
        "TablePermission: $self"
    );
    let pairs = pairs_at(&report, Site::FieldPermission);
    assert!(
        pairs.contains(&(RefKind::Param, "self")),
        "FieldPermission: $self"
    );
    assert_eq!(
        pairs_at(&report, Site::BucketPermission),
        BTreeSet::from([
            (RefKind::Param, "action"),
            (RefKind::Param, "file"),
            (RefKind::Param, "target"),
            (RefKind::Param, "auth"),
        ])
    );
    // The bucket reads classify at the bucket's own site, not Other.
    assert!(
        report
            .refs
            .iter()
            .all(|r| r.site != Site::Other || r.stmt != 2 || r.kind != RefKind::Param)
    );
}

/// The standalone-define permission captures (param, function,
/// module) classify at their shared site and see the session ambient.
#[test]
fn statement_permissions_bind_session_auth() {
    let report = extract(
        "param $p { permissions: $auth.id != none; };\
         fn f() { return 1; };\
         module m { from: f'b:/k'; unsigned; permissions: $auth.id != none; };",
    );
    assert_eq!(
        pairs_at(&report, Site::StatementPermission),
        BTreeSet::from([(RefKind::Param, "auth")])
    );
    // Site attribution: stmt 0 is the param, stmt 2 the module; no
    // permission read lands on Other.
    let sites: Vec<_> = report
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Param && r.name == "auth")
        .map(|r| (r.stmt, r.site))
        .collect();
    assert_eq!(
        sites,
        vec![
            (0, Site::StatementPermission),
            (2, Site::StatementPermission)
        ]
    );
    assert!(
        report
            .refs
            .iter()
            .all(|r| r.site != Site::Other || r.kind != RefKind::Param)
    );
}

/// All three api permission shapes evaluate in the invocation chain's
/// permission loop (`api/invocation.rs:119-124`): the `any` block
/// compiles to the STATEMENT-level api config (`FOR ANY <config>`),
/// whose permissions chain for EVERY routed request in both arms.
#[test]
fn api_permissions_classify_as_statement_permissions() {
    let report = extract(
        "config api { permissions: $auth.id != none; };\
         api '/x' { get { permissions: $auth.id != none; then: 1; }; };\
         api '/a' { any { permissions: $auth.id != none; }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::StatementPermission),
        BTreeSet::from([(RefKind::Param, "auth")])
    );
    let evaluated: Vec<_> = report
        .refs
        .iter()
        .filter(|r| r.site == Site::StatementPermission)
        .map(|r| r.stmt)
        .collect();
    assert_eq!(
        evaluated,
        vec![0, 1, 2],
        "config-global + action-group + any-block all evaluate"
    );
    // No permission read lands anywhere else.
    assert!(
        report
            .refs
            .iter()
            .all(|r| r.name != "auth" || r.site == Site::StatementPermission)
    );
}

/// An api action's `then` body and the `any` fallback evaluate PER
/// REQUEST with `$request` bound (`api/invocation.rs:239-249`) — the
/// request param and the body's tables classify at ApiAction.
#[test]
fn api_then_bodies_see_request() {
    let report = extract(
        "api '/x' { get { then: (create log set body = $request.body); }; };\
         api '/a' { any { then: (create audit set who = $request.who); }; };",
    );
    assert_eq!(
        pairs_at(&report, Site::ApiAction),
        BTreeSet::from([
            (RefKind::Param, "request"),
            (RefKind::Table, "log"),
            (RefKind::Table, "audit"),
        ])
    );
    let stmts: Vec<_> = report
        .refs
        .iter()
        .filter(|r| r.site == Site::ApiAction)
        .map(|r| r.stmt)
        .collect();
    assert_eq!(stmts, vec![0, 0, 1, 1]);
}

/// The `any` arm's middleware ARGUMENTS are define-time stored values
/// (`api/invocation.rs:334` — they stay at Other), while its fallback
/// `then` body rides the per-request ApiAction site.
#[test]
fn api_any_middleware_args_stay_define_time() {
    let report = extract(
        "api '/a' { any { middleware: fn::gate($req);\
         then: (create log set at = $request.at); }; };",
    );
    // The fallback then body: per-request site.
    assert_eq!(
        pairs_at(&report, Site::ApiAction),
        BTreeSet::from([(RefKind::Param, "request"), (RefKind::Table, "log"),])
    );
    // The middleware path and its argument: define-time.
    assert_eq!(
        pairs_at(&report, Site::Other),
        BTreeSet::from([(RefKind::Fn, "gate"), (RefKind::Param, "req"),])
    );
}

/// The FOREACH range evaluates before the loop param binds
/// (`legacy/expr/statements/foreach.rs:23` computes, the
/// per-iteration child context binds after): the range's read of the
/// same-named param reports, the body's stays local.
#[test]
fn foreach_range_reads_the_outer_binding() {
    let report = extract(
        "param $v { value: 1; };\
         fn f() { for $v in [$v, 2] { return $v; }; };",
    );
    let reads: Vec<_> = report
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Param && r.name == "v")
        .map(|r| (r.stmt, r.site))
        .collect();
    assert_eq!(
        reads,
        vec![(1, Site::FnBody)],
        "only the range's read reports"
    );
}

// ---------------------------------------------------------------------
// The corpus totality leg
// ---------------------------------------------------------------------

/// Every per-commit corpus doc extracts without panicking, and
/// extraction is deterministic (run twice, identical reports). No ref
/// counts are pinned — the audit owns the corpus's own invariants.
#[test]
fn corpus_extracts_total_and_deterministic() {
    for family in common::menus::families() {
        for (index, doc) in (family.docs)().into_iter().enumerate() {
            let (file, _) = parser::parse_file(&doc);
            let first = refs::extract_file(&file);
            let second = refs::extract_file(&file);
            assert_eq!(
                first, second,
                "{} doc #{index}: extraction must be deterministic",
                family.name
            );
        }
    }
}
