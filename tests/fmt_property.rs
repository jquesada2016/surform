//! The fmt fixed-point property over a statement-wide
//! corpus: every case must parse, format to re-parsable canonical SDL
//! holding the same definition, and format to itself byte for byte.
//! The oracle suites run the same leg over their full clause cross
//! products (`common::assert_fmt_fixed_point` — this file keeps its
//! own copy so it need not compile the oracle helpers; the two must
//! assert the same three invariants).

use surform::info::write::write_canonical;
use surform::parser::parse_file;

/// The fmt fixed point: canonical SDL re-parses clean, holds the same
/// definition (the span-blind comparison — carried alias spellings
/// and kind spellings never count), and formats to itself byte for
/// byte.
fn assert_fmt_fixed_point(sdl: &str) {
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

#[test]
fn fmt_fixed_point_over_the_corpus() {
    for sdl in [
        // Simple statements and their short forms.
        "namespace acme;",
        "ns acme;",
        "sequence n = 1 + 2;",
        "sequence n { batch: 10; };",
        "sequence n { timeout: none; };",
        "param $x = { a: 1 };",
        "param $x;",
        "/// doc\nparam $x = 50;",
        // Plain `//` comments survive fmt at their anchors (rule 5's
        // layout preservation) — leading, nested-child, trailing
        // before a close, and end of file.
        "// header note\nparam $x = 50;",
        "// above\n/// doc\nparam $x = 50;",
        "table t {\n  // above a field\n  field a: string;\n  // trails before the close\n};",
        "api '/x' {\n  get { then: 1; };\n  // pins the catch-all\n  any { permissions: none; };\n};",
        "param $x = 50;\n// tail note",
        // Collapsed/short-form writers emit trailing comments after
        // the collapsed line.
        "sequence s { start: 5000; // c\n };",
        "param $x { value: 50; // c\n };",
        "database d { // c\n };",
        "table t { field a { computed: 1 + 1; // c\n }; };",
        "fn f() { RETURN 1; } { // c\n };",
        "table t { field f: string { default: 'a'; }; }",
        "param $x { permissions: none; };",
        "db blog { strict; };",
        "database blog { changefeed { expiry: 1h30m; }; };",
        "bucket b { permissions: $action = 'read'; };",
        // Aliases in every carried class stay; values still compare.
        "user u { on: kv; password: 'p'; }",
        "user u { on: root; password: 'p'; }",
        "user u { on: ns; roles: viewer; duration { session: 2h; }; }",
        // The 3.3 additive clause: scram-only and passhash+scram.
        "user u { on: db; passscram: 'SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20='; }",
        "user u { on: db; passhash: '$argon2id$'; passscram: 'SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20='; }",
        "analyzer a { filters: snowball(ara), lowercase; }",
        "analyzer a { filters: snowball(arabic); }",
        "index i { on: t; columns: a, b; }",
        "index i { on: t; fields: a, b; }",
        // Medium statements.
        "event e { on: t; then: 1, 2; async { retry: 0; }; }",
        // Mixins and spreads (spec/mixin.md): fmt is the one writer
        // that never lowers — the spread round-trips at its position.
        "mixin base;",
        "mixin m { a: int; };",
        "mixin stamp {\n  created_at: datetime;\n  updated_at: datetime { default: time::now(); };\n};\n\ntable t schemafull {\n  ...stamp; // audit\n  title: string;\n  created_at: datetime { default: time::now(); };\n};",
        "mixin deep { meta.a: array<int>; label = string::uppercase(title); };",
        // The full member model (spec/mixin.md): a mixin body is a table block's
        // body minus `on:` clauses — indexes and events included.
        "mixin indexed {\n  slug: string;\n  index slug_u { fields: slug; };\n  event touched { when: slug != ''; then: 1; };\n};",
        "mixin bare { index by_email { fields: email; }; };",
        // The define-order corner's shape: a
        // spread carrying an index that names an explicit field —
        // the fields group prints before the spreads, so the fmtted
        // file's DDL still defines the field first.
        "mixin idx { index by_email { fields: email; }; };\ntable t schemafull {\n  email: string;\n  ...idx;\n};",
        // The canonical table-body layout: clauses, then fields, then
        // spreads (relative order preserved), then indexes, then
        // events — an out-of-group authored order canonicalizes into
        // the groups, and the fixed point holds from there. Fields
        // print before the spreads so a spread-carried index can
        // never define above an explicit field it names (the
        // define-order corner).
        "mixin m { c: int; };\nmixin n { d: string; };\ntable t schemafull {\n  event e { then: 1; };\n  index i { fields: a; };\n  b: int;\n  ...m;\n  a: int;\n  ...n;\n};",
        "table t { field a: int; field b = 1 + 1; index i { fields: a; }; event e { then: 1; }; }",
        "module m { from: f\"b:/k.surli\"; unsigned; }",
        "module { from: silo::org::pkg::<0.1.0>; unsigned; permissions: full; }",
        // Quoted silo segments decode to non-word-shaped interiors —
        // the writer must re-quote them;
        // keyword-like segments stay bare (the shape rule, not the
        // keyword rule).
        "module { from: silo::`my org`::`pkg x`::<1.0.0>; unsigned; };",
        "module { from: silo::fn::`pkg x`::<1.0.0>; unsigned; };",
        "fn go::deep($a: int, $b: option<string>) { RETURN $a; };",
        "fn go::deep($a: int) -> array<int, 10> { RETURN [$a]; } { permissions: none; };",
        "config api { middleware: a(), fn::b(1), mod::c::d(2s, 'x'); };",
        "config graphql { tables: auto; functions { exclude: fn::f, fn::g; }; };",
        "config default { namespace: 'ns'; };",
        // Index variants.
        "index i { on: t; fields: a; concurrently; }",
        "index unique u { on: t; fields: a; }",
        "index count c { on: t; where: x > 1; }",
        "index fulltext f { on: t; fields: body; highlights; }",
        "index hnsw h { on: t; fields: v; dimension: 3; distance: minkowski(2.5); }",
        "index diskann d { on: t; fields: v; dimension: 8; type: i8; distance: inner_product; }",
        // api: group order, method lists, fallback-less any.
        "api '/a' { any { }; get { then: 1; }; };",
        "api '/a/:id' { put, patch { middleware: m(); then: { ok }; }; };",
        // access variants.
        "access a { on: root; type: jwt; algorithm: rs256; key: $k; };",
        "access a { on: db; type: jwt; url: 'https://x/jwks'; issuer { algorithm: es256; key: '[REDACTED]'; }; authenticate: true; };",
        "access a { on: db; type: record; signup: 1; signin: 2; refresh; duration { grant: 30d; token: 1h; session: none; }; };",
        "access a { on: ns; type: bearer; for: user; };",
        // table kinds and nesting.
        "table t { };",
        "table t;",
        "table any t { drop; };",
        "table t schemafull { };",
        "table a | b -> edge -> c | d { enforced; changefeed { expiry: 1d; }; };",
        "table in_only -> edge -> { schemaless; };",
        // The 3.3 shapes: the header word (enforced implied — the
        // clause line never prints under it) and the caps.
        "table a -> likes -> b lightweight;",
        "table a -> likes -> b lightweight { inline edges: 8; inline references: 16; };",
        "table t { inline edges: 0; };",
        "table view v { SELECT * FROM t; };",
        "table view v { SELECT name FROM t WHERE x > 1 GROUP BY name; permissions: full; };",
        "table t { field a: int; field b = 1 + 1; index i { fields: a; }; event e { then: 1; }; }",
        "table t { field a: object { flexible; }; permissions: none; }",
        // field clause vocabulary.
        "field f: int { on: t; default: 0; value: 1; assert: $value > 0; readonly; }",
        "field f: int { on: t; default always: 0; }",
        "field f: record<u> { on: t; reference; }",
        "field f: record<u> { on: t; reference { on delete: $x; }; }",
        "field f: object { on: t; permissions { select: full; create, update: $auth; }; }",
        "field f: string { on: t; graphql { alias: 'f'; }; }",
        // The 3.3 access clauses: audience on the jwt
        // source (direct and the record sub-block), context on a
        // record access.
        "access a { on: db; type: jwt; url: 'u'; audience: 'aud1', 'aud2'; };",
        "access r { on: db; type: record; jwt { url: 'u'; audience: 'a'; }; refresh; };",
        "access c { on: db; type: record; signup: create user set email = $email; authenticate: $auth.active; context: { extra: 1 }; };",
        "access s { on: db; type: record; signin: { select id from user where email = $email }; context: (SELECT * FROM user WHERE email = $email); };",
        // The 3.3 INLINE flag: the block spelling, its
        // canonical neighbors, and the relation host.
        "field f: number { on: t; inline; }",
        "field f: number { on: t; readonly; inline; default: 1; }",
        "table a -> likes -> b { field score: object { inline; flexible; }; }",
        // Escaping: DDL keywords, the SDL-only reservations, weird
        // shapes.
        "table `table` { };",
        "table `view` { };",
        "table t { field `raw`: int; field `type`: string; field `a b`: string; }",
        "table t { field p: record<`my table`>; }",
        "param $`weird name` = 1;",
        "analyzer `event` { tokenizers: blank; };",
        // The kind printer heals bare constant-literal names — SDL's
        // kind reader rejects them outright,
        // so only quoted spellings can ride the AST.
        "table t { field a: record<`NaN`>; field b: { `Infinity`: int }; }",
        // raw: interior verbatim (the body's comments and spellings
        // ride the fixed point unchanged).
        "raw { DEFINE TABLE x TYPE ANY SCHEMALESS; /* semi-colons; inside */ }",
    ] {
        assert_fmt_fixed_point(sdl);
    }
}

/// Rule 5's clause anchoring: a `//` comment lands on the clause it
/// precedes (or, same-line, the clause it trails) at EVERY depth —
/// clause blocks, sub-blocks, per-operation permission groups, api
/// groups, nested statements — and rides that clause through the
/// writer's grammar-block reordering. Every case must hold the fixed
/// point AND keep every marker comment (the fixed point alone cannot
/// see a silent drop).
#[test]
fn fmt_anchors_comments_to_their_clauses_at_every_depth() {
    for sdl in [
        // Statement-level: before-notes above clauses, same-line
        // after-notes on the clause's own line, a close note on the
        // block's `};`.
        "sequence s { // m1\n batch: 10; // m2\n timeout: 5s; }; // m3",
        "param $p { // m1\n value: 1; // m2\n permissions: none; }; // m3",
        "database d { // m1\n strict; // m2\n changefeed { // m3\n expiry: 1h; // m4\n include original; }; }; // m5",
        "bucket b { // m1\n backend: 'x'; readonly; // m2\n permissions: full; };",
        "user u { // m1\n on: kv; // m2\n password: 'p'; // m3\n roles: viewer; duration { // m4\n session: 2h; }; };",
        "analyzer a { // m1\n function: fn::x; // m2\n tokenizers: blank; filters: snowball(english); };",
        "event e { // m1\n on: t; // m2\n when: x > 1; then: 1, 2; // m3\n async { // m4\n retry: 0; }; };",
        "module { // m1\n from: silo::org::pkg::<0.1.0>; // m2\n unsigned; };",
        "fn f() { RETURN 1; } { // m1\n permissions: none; }; // m2",
        "config api { // m1\n middleware: a(); // m2\n permissions: full; };",
        "config graphql { // m1\n depth: 3; // m2\n complexity: 100; };",
        "config default { // m1\n namespace: 'n'; // m2\n database: 'd'; };",
        // Index kinds, the alias key, and same-line notes.
        "index i { // m1\n on: t; // m2\n fields: a; // m3\n concurrently; };",
        "index fulltext f { // m1\n on: t; fields: body; // m2\n analyzer: az; highlights; };",
        "index hnsw h { // m1\n on: t; fields: v; dimension: 3; // m2\n distance: cosine; };",
        // Access: the variant clauses, the jwt sub-block chain, the
        // duration block.
        "access a { // m1\n on: db; // m2\n type: jwt; // m3\n url: 'u'; // m4\n audience: 'a'; issuer { // m5\n algorithm: es256; key: '[REDACTED]'; }; };",
        "access r { // m1\n on: db; type: record; signup: 1; // m2\n jwt { // m3\n algorithm: rs256; key: $k; }; refresh; duration { // m4\n grant: 30d; }; };",
        "access b { // m1\n on: ns; type: bearer; for: user; // m2\n };",
        // The per-operation permission groups anchor by group.
        "table t { permissions { // m1\n select: full; // m2\n create, update: $auth; // m3\n }; };",
        "field f: object { on: t; permissions { // m1\n select: full; // m2\n }; };",
        // The table's own clauses, and nested statements (their
        // clauses anchor through the nested writer).
        "table t { // m1\n schemafull; // m2\n drop; // m3\n inline edges: 4; graphql { // m4\n alias: 't'; }; };",
        "table t { field a: int { // m1\n default: 0; // m2\n readonly; }; index i { // m3\n fields: a; }; event e { // m4\n then: 1; }; };",
        "field f: record<u> { // m1\n on: t; reference { // m2\n on delete: cascade; }; };",
        // The api groups: each group's interior clauses and its own
        // close line.
        "api '/x' { // m1\n any { // m2\n permissions: none; }; get { // m3\n then: 1; // m4\n }; };",
        // The short-form flattening: a value-clause before-note
        // prints above the one-line statement and re-captures as its
        // leading comment (the fixed point holds either way).
        "param $x { // m1\n value: 5; };",
        "sequence s { // m1\n start: 5; };",
        // Same-line after-notes force the block form (the guard).
        "sequence s { start: 5; // m1\n };",
        "param $x { value: 5; // m1\n };",
        "table t { field f = 1 + 1; };",
        "table t { field f: int { computed: 1 + 1; // m1\n }; };",
        // The remaining path families: graphql's family clauses and
        // sub-blocks, access's tail clauses, the view's clauses, and
        // close notes on the statement forms without clause blocks.
        "config graphql { // m1\n tables: auto; // m2\n functions { // m3\n exclude: fn::f; }; };",
        "config graphql { // m1\n auto; };",
        "access a { // m1\n on: db; type: record; signin: 1; authenticate: true; // m2\n context: { x: 1 }; };",
        "table view v { SELECT * FROM t; // m1\n permissions: full; };",
        "namespace n; // m1",
        "raw { DEFINE TABLE x; }; // m1",
        // The pins: the close note keeps/rides the short
        // forms, the `columns:` alias anchors under its authored key,
        // and the graphql flag re-key merges with a real tables
        // anchor instead of shadowing it.
        "sequence s { start: 5; }; // m1",
        "param $x { value: 5; }; // m1",
        "table t { x: int { computed: 5; }; // m1\n };",
        "sequence s = 5; // m1",
        "param $x = 7; // m1",
        "index i { on: t; // m1\n columns: a; // m2\n };",
        "index i { // m1\n on: t; columns: a, b; };",
        "config graphql { tables { include: a; }; // m1\n auto; // m2\n };",
        "config graphql { auto; // m1\n tables: auto; // m2\n };",
        "table t { a: int = 1; // m1\n };",
    ] {
        assert_fmt_fixed_point(sdl);
        let (ast, _) = parse_file(sdl);
        let once = write_canonical(&ast);
        let markers = sdl.matches("// m").count();
        assert_eq!(
            once.matches("// m").count(),
            markers,
            "a marker comment dropped:\n--- SDL ---\n{sdl}\n--- fmt ---\n{once}"
        );
    }
    // Position pins: the marker count sees drops, not stable moves —
    // these assert the exact placement a moved-but-stable note keeps.
    let (ast, _) = parse_file("table t { a: int = 1; // m1\n b: int; };");
    let once = write_canonical(&ast);
    assert!(
        once.contains("field a: int = 1; // m1"),
        "the bare-head note must stay on the field's own line:\n{once}"
    );
    let (ast, _) = parse_file("sequence s { start: 5; }; // m1");
    let once = write_canonical(&ast);
    assert!(
        once.contains("sequence s = 5; // m1"),
        "the close note joins the short form's line:\n{once}"
    );
    let (ast, _) = parse_file("param $x { // m1\n value: 5; };");
    let once = write_canonical(&ast);
    assert!(
        once.starts_with("// m1\n"),
        "a flattened before-note prints above the statement:\n{once}"
    );
    // The canonical table-body layout (spec/mixin.md): an out-of-group authored
    // order groups into spreads (relative order preserved), fields,
    // indexes, events — and a member's anchored `//` note rides its
    // statement through the reorder. Spreads print first (the table's
    // inherited members read before its own definitions); the layout
    // is presentation only, the emitter ordering by rule 7.
    let (ast, _) = parse_file(
        "mixin extra { c: int; };\n\
         mixin more { d: string; };\n\
         table t schemafull {\n  event e { then: 1; }; // m1\n  index i { fields: a; };\n  \
         b: int;\n  ...extra;\n  a: int;\n  ...more; // m2\n};",
    );
    let once = write_canonical(&ast);
    assert_eq!(
        once,
        "mixin extra {\n  field c: int;\n};\n\nmixin more {\n  field d: string;\n};\n\n\
         table t {\n  schemafull;\n  ...extra;\n  ...more; // m2\n  field b: int;\n  \
         field a: int;\n  index i {\n    fields: a;\n  };\n  event e {\n    then: 1;\n  }; // m1\n};\n"
    );
    // The mixin body's own member groups: fields, then indexes, then
    // events (no spreads — mixin-into-mixin is v1-out).
    let (ast, _) =
        parse_file("mixin x {\n  event e { then: 1; };\n  a: int;\n  index i { fields: a; };\n};");
    let once = write_canonical(&ast);
    assert_eq!(
        once,
        "mixin x {\n  field a: int;\n  index i {\n    fields: a;\n  };\n  event e {\n    then: 1;\n  };\n};\n"
    );
}

/// The expression canon rides the embedded printer, so fmt output
/// holds the PRINTER's spelling — canonical, possibly different from
/// the authored one (`1  +  1` → `1 + 1`, uppercased keywords) — and
/// still the same definition.
#[test]
fn fmt_canonicalizes_expression_spellings() {
    let (ast, diags) = parse_file("sequence n = 1  +  1;");
    assert!(diags.is_empty());
    let out = write_canonical(&ast);
    assert_eq!(out, "sequence n = 1 + 1;\n");
}
