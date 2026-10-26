//! The type-alias feature end to end (spec/type-alias.md): the
//! declaration grammar and its name rules, the Capitalized reference
//! at every legal kind position (and the name-list non-use), the
//! expansion's totality before emission, fmt's verbatim round trip,
//! the order pass's unknown-reference and cycle findings, and the
//! deferred flexible check. The mechanism rides the same pipeline as
//! the pair oracle's corpus (menus.rs's `type-alias` family) — these
//! tests pin the exact shapes that corpus only exercises.

mod common;

use common::assert_oracle_accepts_files;
use miette::Diagnostic as _;
use surform::codegen::emit;
use surform::info::write::write_canonical;
use surform::parser::{parse_file, parse_file_composable};

fn parsed(src: &str) -> surform::ast::SdlFile {
    let (file, diags) = parse_file(src);
    assert!(diags.is_empty(), "unexpected diags: {diags:?}");
    file
}

/// Every diagnostic code the strict pipeline holds for `src` — the
/// parse's own findings PLUS the check-class contract's (the order
/// pass: the cross-statement SDL-E1062, SDL-E0015, SDL-E1071, and
/// the deferred flexible check). Spans are point(0): only codes are
/// read.
fn codes(src: &str) -> Vec<String> {
    all_diags(src)
        .1
        .iter()
        .filter_map(|diag| diag.code().map(|code| code.to_string()))
        .collect()
}

/// The parse's findings plus the check-class contract's, one list.
fn all_diags(src: &str) -> (surform::ast::SdlFile, Vec<surform::diag::Diag>) {
    let (file, mut diags) = parse_file(src);
    let items = file.items.clone();
    let spans = vec![surform::lexer::Span::point(0); items.len()];
    diags.extend(
        surform::order::check_document(&items, &[], &spans)
            .into_iter()
            .map(|(d, _)| d),
    );
    (file, diags)
}

/// The declaration: `type <Name> = <kind>;` at the top level, the
/// kind captured like a field's (brace-bearing object kinds fold),
/// and fmt round-trips it verbatim.
#[test]
fn declaration_round_trips_through_fmt() {
    let file = parsed("type Email = string;\n");
    assert!(matches!(file.items[0], surform::ast::Stmt::TypeAlias(_)));
    let canonical = write_canonical(&file);
    assert_eq!(canonical, "type Email = string;\n");
    // The fixed point: the canonical form re-parses to the same
    // definition and re-prints to itself.
    let reparsed = parsed(&canonical);
    assert!(file.same_definition(&reparsed));
    assert_eq!(write_canonical(&reparsed), canonical);
    // Authored spelling rides verbatim (trivia inside the kind too —
    // the writer never re-spells a kind).
    let spaced = parsed("type Pair = { a : int };\n");
    assert_eq!(write_canonical(&spaced), "type Pair = { a : int };\n");
}

/// Every legal use position expands; the name-list position does
/// not (`record<Email>`'s interior is a table NAME at every case).
#[test]
fn expansion_covers_every_kind_position() {
    let cases = [
        (
            "type Email = string;\ntable t { a: Email; };",
            "TYPE string",
        ),
        (
            "type Email = string;\ntable t { a: option<Email>; };",
            "TYPE option<string>",
        ),
        (
            "type Email = string;\ntable t { a: Email | int; };",
            "TYPE string | int",
        ),
        (
            "type Email = string;\ntable t { a: { email: Email }; };",
            "TYPE { email: string }",
        ),
        (
            "type Email = string;\ntable t { a: array<Email, 3>; };",
            "TYPE array<string, 3>",
        ),
        (
            "type Email = string;\ntype Emails = array<Email>;\n\
             table t { a: Emails; };",
            "TYPE array<string>",
        ),
        (
            "type Email = string;\ntype Emails = array<Email>;\n\
             fn f($e: Email) -> Emails { return [$e]; };",
            "fn::f($e: string) -> array<string>",
        ),
    ];
    for (src, fragment) in cases {
        let ddl = emit(&parsed(src));
        assert!(
            ddl.contains(fragment),
            "`{src}` emitted\n{ddl}\nwhich lacks `{fragment}`"
        );
    }
    // The non-use: the interior name survives untouched.
    let ddl = emit(&parsed(
        "type Email = string;\ntable t { a: record<Email>; };",
    ));
    assert!(ddl.contains("TYPE record<Email>"), "{ddl}");
}

/// Expansion is TOTAL before emission: no alias name can appear in
/// the emitted DDL (upstream's kind parser rejects unknown bare
/// words, so a leak is a hard error there — the totality is what
/// makes the feature server-version-immune).
#[test]
fn expansion_is_total_before_emission() {
    let file = parsed(
        "type Email = string;\ntype Emails = array<Email>;\n\
         table t schemafull {\n  a: Email;\n  b: option<Emails>;\n  c: { e: Email };\n};\n\
         fn f($e: Email) -> Emails { return [$e]; };",
    );
    let ddl = emit(&file);
    assert!(!ddl.contains("Email"), "an alias name leaked:\n{ddl}");
    // And the emitted DDL parses through the embedded engine's
    // parser (the stage-2 oracle).
    assert_oracle_accepts_files(&ddl);
}

/// Use before define — the declarative namespace: position is not
/// resolution's business (any file, any position; the check runs on
/// the composed document).
#[test]
fn use_before_define_resolves() {
    let file = parsed("table t { a: Email; };\ntype Email = string;\n");
    assert!(surform::parser::resolve_spreads(&file).is_empty());
    assert_eq!(
        emit(&file),
        "DEFINE TABLE OVERWRITE t TYPE NORMAL SCHEMALESS;\n\
         DEFINE FIELD OVERWRITE a ON t TYPE string;\n"
    );
}

/// The unknown reference: SDL-E1071, with a did-you-mean over the
/// document's alias set. A typo'd alias is an error, never a silent
/// kind; a lowercase unknown word stays the kind grammar's own
/// rejection (SDL-E1026).
#[test]
fn unknown_reference_is_e1071() {
    assert_eq!(codes("table t { a: Emial; };"), ["SDL-E1071"]);
    assert_eq!(
        codes("type Email = string;\ntable t { a: Mail; };"),
        ["SDL-E1071"]
    );
    // The suggestion names the near miss.
    let (_, diags) = all_diags("type Email = string;\ntable t { a: Emial; };");
    let diag = diags
        .iter()
        .find(|diag| diag.code().map(|c| c.to_string()).as_deref() == Some("SDL-E1071"))
        .expect("the unknown-reference finding");
    assert!(diag.to_string().contains("Emial"));
    let help = diag.help().expect("a help text").to_string();
    assert!(help.contains("`Email`"), "did-you-mean missing: {help}");
    // Lowercase unknown words keep the kind grammar's own code.
    assert_eq!(codes("table t { a: email; };"), ["SDL-E1026"]);
}

/// The name rules (SDL-E1072): not Capitalized, a reserved kind
/// head (ASCII-case-insensitively — `String` would lose every use
/// site to the `string` head), or the exact-case literal spellings.
/// (`2Fast` never reaches the name check: it is not an identifier at
/// all — the name position's own SDL-E1004.)
#[test]
fn invalid_names_are_e1072() {
    for name in ["email", "String", "ARRAY", "any2", "NaN", "Infinity"] {
        let src = format!("type {name} = int;\n");
        assert_eq!(codes(&src), ["SDL-E1072"], "`{name}` must be rejected");
    }
    assert_eq!(codes("type 2Fast = int;\n"), ["SDL-E1004"]);
    // A conforming name with a digit tail and underscores parses.
    assert!(codes("type User_V2 = int;\n").is_empty());
}

/// The duplicate name: SDL-E1062 within one file (the parse's own
/// registry — no last-one-wins fold).
#[test]
fn duplicate_name_is_e1062() {
    assert_eq!(
        codes("type Foo = int;\ntype Foo = string;\n"),
        ["SDL-E1062"]
    );
}

/// The chain cycle: SDL-E0015 names the chain; a self-cycle is the
/// length-1 chain.
#[test]
fn alias_cycles_are_e0015() {
    let (_, diags) = all_diags("type A = B;\ntype B = A;\n");
    let cycle = diags
        .iter()
        .find(|diag| diag.code().map(|c| c.to_string()).as_deref() == Some("SDL-E0015"))
        .expect("the cycle finding");
    assert!(cycle.to_string().contains("type alias `A`"), "{cycle}");
    assert!(cycle.to_string().contains("type alias `B`"), "{cycle}");
    assert_eq!(codes("type A = A;\n"), ["SDL-E0015"]);
    // An acyclic chain is legal and expands.
    assert!(codes("type A = B;\ntype B = int;\ntable t { a: A; };").is_empty());
    assert!(emit(&parsed("type A = B;\ntype B = int;\ntable t { a: A; };")).contains("TYPE int"));
}

/// The expansion must be an expressible kind within the ceiling
/// (spec/type-alias.md, the deep-chain review): a chain of option
/// wrappers composes `option<option<…>>` — each link legal, the
/// whole unexpressible (SDL-E1074, the expansion named); a
/// shape-legal chain past the 256-level nesting ceiling is SDL-E1073
/// with the measured depth, the first over-ceiling link named.
/// Neither can abort the pipeline: the resolution is iterative, a
/// chain never recurses per link.
#[test]
fn expansions_are_capped_and_shape_checked() {
    // The shape class: the first crossing link is the root cause.
    assert_eq!(
        codes("type A = option<int>;\ntype B = option<A>;\ntable t { a: B; };"),
        ["SDL-E1074"]
    );
    let (_, diags) = all_diags("type A = option<int>;\ntype B = option<A>;\n");
    let shape = diags
        .iter()
        .find(|diag| diag.code().map(|c| c.to_string()).as_deref() == Some("SDL-E1074"))
        .expect("the shape finding");
    assert!(
        shape.to_string().contains("`option<option<int>>`"),
        "{shape}"
    );
    // The gates hold at USE sites too: `option<A>` over an
    // option-alias composes the same unexpressible whole — legal
    // authored, refused before anything emits. The site's own wrapper
    // counts against the ceiling (`A255` sits at 256; one more option
    // is one level over, while the bare reference stays legal).
    assert_eq!(
        codes("type A = option<int>;\ntable t { a: option<A>; };"),
        ["SDL-E1074"]
    );
    let (_, diags) = all_diags("type A = option<int>;\ntable t { a: option<A>; };");
    let site = diags
        .iter()
        .find(|diag| diag.code().map(|c| c.to_string()).as_deref() == Some("SDL-E1074"))
        .expect("the site finding");
    assert!(site.to_string().contains("field `a`"), "{site}");
    let mut at_ceiling = String::from("type A0 = int;\n");
    for i in 1..256 {
        at_ceiling.push_str(&format!("type A{i} = array<A{}>;\n", i - 1));
    }
    at_ceiling.push_str("table t { bare: A255; wrapped: option<A255>; };\n");
    let (_, diags) = all_diags(&at_ceiling);
    let depth_codes: Vec<String> = diags
        .iter()
        .filter_map(|diag| diag.code().map(|c| c.to_string()))
        .collect();
    assert_eq!(depth_codes, ["SDL-E1073"]);
    assert!(
        diags
            .iter()
            .find(|diag| diag.code().map(|c| c.to_string()).as_deref() == Some("SDL-E1073"))
            .expect("the site depth finding")
            .to_string()
            .contains("field `wrapped`"),
        "the wrapper site is the offender"
    );
    // The gates reach mixin MEMBERS too — a spread lands the member
    // at every spreading table, so a member kind composes exactly
    // like a table field's (the reviewer's twin of the site gap).
    assert_eq!(
        codes(
            "type A = option<int>;\nmixin m { a: option<A>; };\n\
               table t schemafull { ...m; };"
        ),
        ["SDL-E1074"]
    );
    assert!(
        codes(
            "type Str = int;\nmixin m { x: Str { flexible; }; };\n\
                   table t schemafull { ...m; };"
        )
        .contains(&"SDL-E1050".to_string())
    );
    assert_eq!(
        codes("mixin m { a: Oops; };\ntable t schemafull { ...m; };"),
        ["SDL-E1071"]
    );
    // The depth class: a shape-legal array chain crosses the ceiling
    // at exactly 257 levels.
    let mut src = String::from("type A0 = int;\n");
    for i in 1..300 {
        src.push_str(&format!("type A{i} = array<A{}>;\n", i - 1));
    }
    src.push_str("table t { a: A299; };\n");
    let (_, diags) = all_diags(&src);
    let depth = diags
        .iter()
        .find(|diag| diag.code().map(|c| c.to_string()).as_deref() == Some("SDL-E1073"))
        .expect("the depth finding");
    assert!(depth.to_string().contains("`A256`"), "{depth}");
    assert!(depth.to_string().contains("257"), "{depth}");
    // Under the ceiling the chain expands, and no alias name leaks.
    let mut ok = String::from("type A0 = int;\n");
    for i in 1..50 {
        ok.push_str(&format!("type A{i} = array<A{}>;\n", i - 1));
    }
    ok.push_str("table t { a: A49; };\n");
    assert!(codes(&ok).is_empty());
    let ddl = emit(&parsed(&ok));
    // The innermost composition reads through, and the chain's full
    // depth materializes: 49 wrappers around `int`.
    assert!(ddl.contains("array<array<array<int>>"), "{ddl}");
    assert_eq!(ddl.matches("array<").count(), 49, "{ddl}");
    for i in 0..50 {
        assert!(
            !ddl.contains(&format!("A{i}")),
            "the alias name `A{i}` leaked:\n{ddl}"
        );
    }
}

/// The deferred flexible check: a `flexible` field whose kind
/// references an alias decides on the EXPANSION — an object
/// expansion is legal, a non-object one is SDL-E1050 naming the
/// expansion.
#[test]
fn flexible_decides_on_the_expansion() {
    assert!(codes("type Conf = { a: int };\ntable t { c: Conf { flexible; }; };\n").is_empty());
    let (_, diags) = all_diags("type Str = int;\ntable t { x: Str { flexible; }; };\n");
    let flex = diags
        .iter()
        .find(|diag| diag.code().map(|c| c.to_string()).as_deref() == Some("SDL-E1050"))
        .expect("the flexible finding");
    assert!(
        flex.to_string().contains("the expansion of `Str`"),
        "{flex}"
    );
    // An unresolved reference stays the E1071's business (no
    // double-report).
    assert_eq!(
        codes("table t { x: Missing { flexible; }; };"),
        ["SDL-E1071"]
    );
}

/// Aliases ride mixin members: a spread's landed field kinds expand
/// like any other.
#[test]
fn mixin_members_expand_aliases() {
    let ddl = emit(&parsed(
        "type Stamp = datetime;\nmixin stamp { at: Stamp; };\n\
         table t schemafull { ...stamp; };",
    ));
    assert!(ddl.contains("TYPE datetime"), "{ddl}");
    assert!(!ddl.contains("Stamp"), "{ddl}");
}

/// The composable entry parses aliases like the strict one (only
/// `include` is composition-bound), and the statement takes `//`
/// comments at both ends (rule 5's anchoring) but rejects `///`
/// (no COMMENT target — it emits nothing).
#[test]
fn comments_and_parse_entries() {
    let src = "// the user's address\ntype Email = string; // one line\n";
    let canonical = write_canonical(&parsed(src));
    assert!(canonical.contains("// the user's address"), "{canonical}");
    assert!(canonical.contains("// one line"), "{canonical}");
    assert!(parse_file_composable(src).1.is_empty());
    let (_, diags) = parse_file("/// doc\ntype Email = string;\n");
    assert!(
        diags
            .iter()
            .any(|diag| diag.code().map(|c| c.to_string()).as_deref() == Some("SDL-E1008")),
        "the doc comment must be rejected: {diags:?}"
    );
    // A block body is not part of the grammar: the header IS the
    // declaration.
    let (_, diags) = parse_file("type Email = string { x: int; };");
    assert!(!diags.is_empty(), "a block form must not parse");
}

/// The lowered file drops the alias statements (emission's property,
/// the mixin precedent).
#[test]
fn aliases_drop_from_the_lowered_file() {
    let lowered = surform::expand::expand(&parsed("type Email = string;\ntable t { a: Email; };"));
    assert_eq!(lowered.items.len(), 1);
    assert!(matches!(&lowered.items[0], surform::ast::Stmt::Table(_)));
}

/// SDL-L0027: the alias nothing references. Every legal use position
/// counts — a field, a fn argument or return, another alias's
/// right-hand side (a chain's head stays used while any link names
/// it, so pruning cascades one finding at a time) — and a use inside a
/// `raw` body is SurrealQL text (rule 8) and does not. The allow
/// directive excuses the declaration.
#[test]
fn unused_alias_lints() {
    let findings = |src: &str| {
        let file = parsed(src);
        let resolved = surform::expand::expand(&file);
        surform::lint::lint(&file, &resolved, src)
    };
    // Used: clean.
    assert!(findings("type Email = string;\ntable t { a: Email; };").is_empty());
    // Unused: one finding, naming the dead alias.
    let unused = findings("type Email = string;\ntable t { a: string; };");
    assert_eq!(unused.len(), 1, "{unused:?}");
    assert_eq!(unused[0].kind.code_str(), "SDL-L0027");
    assert!(unused[0].to_string().contains("`Email`"), "{}", unused[0]);
    // Used only through another alias's right-hand side: the link is
    // used, the dead head is the finding.
    let link_used =
        findings("type Email = string;\ntype Emails = array<Email>;\ntable t { a: string; };");
    assert_eq!(link_used.len(), 1, "{link_used:?}");
    assert!(
        link_used[0].to_string().contains("`Emails`"),
        "{}",
        link_used[0]
    );
    // fn positions and mixin members count as uses.
    assert!(
        findings("type Email = string;\nfn f($e: Email) { return $e; };").is_empty(),
        "a fn argument is a use"
    );
    assert!(
        findings(
            "type Stamp = datetime;\nmixin m { at: Stamp; };\n\
             table t schemafull { ...m; };",
        )
        .is_empty(),
        "a mixin member is a use"
    );
    // A use inside a `raw` body is SurrealQL text — it does not count.
    let raw = findings("type Email = string;\nraw {\n  DEFINE TABLE Email;\n};");
    assert_eq!(raw.len(), 1, "{raw:?}");
    assert_eq!(raw[0].kind.code_str(), "SDL-L0027");
    // The allow directive excuses the declaration.
    let excused = "// surform-lint-allow: SDL-L0027\n\
                   type Email = string;\ntable t { a: string; };";
    assert!(findings(excused).is_empty(), "the allow must suppress");
}
