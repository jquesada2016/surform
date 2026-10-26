//! The flatten oracle (spec/include.md) — the composition
//! machinery's test spine. The invariant: **flatten(root) ≡ root** — a composed
//! project compiles to byte-identical DDL and produces the same lint
//! findings as its flattened single file. The negative legs pin the
//! loader's diagnostics: missing files, absolute paths, the
//! duplicate-include error, the included-file scope ban, and
//! file-scoped allow suppression. The declarative-namespace legs pin
//! the new semantics: cycles are legal (splice-once), a forward
//! spread resolves, a diamond advises SDL-L0026 (opt-out by the
//! allow comment). The write-back legs pin the
//! distribution: order fidelity, per-home canonicalization, twin-fold
//! alignment, and the unplaceable refusals.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use miette::Diagnostic as _;
use surform::codegen;
use surform::lint;
use surform::load;

/// The fixture tree: preamble → shared mixins → two tables (sharing
/// one diamond-composed id file, one spreading a cross-file mixin) →
/// the root's own table.
const FIXTURE_ROOT: &str = "tests/fixtures/include/root.surdl";
const FIXTURE_FLAT: &str = "tests/fixtures/include/flat.surdl";

fn load_root() -> load::Loaded {
    let path = Path::new(FIXTURE_ROOT);
    let text = std::fs::read_to_string(path).expect("fixture reads");
    load::load(path, FIXTURE_ROOT, text).expect("the fixture tree loads clean")
}

/// The finding identity the oracle compares — spans are per-file and
/// differ between tree and flattening, so the identity is everything
/// the message names.
fn finding_key(f: &lint::Finding) -> String {
    format!(
        "{:?}|{:?}|{}|{}",
        f.scope,
        f.family,
        f.key,
        f.kind.code_str()
    )
}

/// flatten(root) ≡ root, emission leg: the tree's flattened document
/// compiles to exactly the flat file's DDL.
#[test]
fn flattened_tree_emits_like_the_flat_file() {
    let loaded = load_root();
    let (flat, diags) =
        surform::parser::parse_file(&std::fs::read_to_string(FIXTURE_FLAT).expect("fixture reads"));
    assert!(diags.is_empty(), "unexpected diags: {diags:?}");
    assert_eq!(codegen::emit(&loaded.file), codegen::emit(&flat));
}

/// flatten(root) ≡ root, lint leg: the same findings (modulo the
/// per-file spans) as the flat file's.
#[test]
fn flattened_tree_lints_like_the_flat_file() {
    let loaded = load_root();
    let flat_src = std::fs::read_to_string(FIXTURE_FLAT).expect("fixture reads");
    let (flat, diags) = surform::parser::parse_file(&flat_src);
    assert!(diags.is_empty(), "unexpected diags: {diags:?}");
    let resolved = surform::expand::expand(&flat);
    let parts = loaded.lint_parts();
    // The diamond advisory is composition-only by nature — the flat
    // twin has no include to anchor it on — so it rides its own tests
    // and stays out of the like-for-like set.
    let composed: BTreeSet<String> =
        lint::lint_composed(&loaded.file, &resolved, &parts, &loaded.diamonds)
            .iter()
            .filter(|f| f.kind.code_str() != "SDL-L0026")
            .map(finding_key)
            .collect();
    let flat_findings: BTreeSet<String> = lint::lint(&flat, &resolved, &flat_src)
        .iter()
        .map(finding_key)
        .collect();
    assert_eq!(composed, flat_findings);
}

/// The diamond composes once: two files include the same id file;
/// its sequence lands exactly once, at the first include's position.
#[test]
fn diamond_splices_once() {
    let loaded = load_root();
    let ddl = codegen::emit(&loaded.file);
    assert_eq!(
        ddl.matches("DEFINE SEQUENCE").count(),
        1,
        "the shared file splices once: {ddl}"
    );
    // At the FIRST site: before the users table, which both includes
    // precede.
    let first = ddl.find("DEFINE SEQUENCE").expect("the sequence is there");
    let users = ddl
        .find("DEFINE TABLE OVERWRITE users")
        .expect("users is there");
    assert!(first < users);
}

/// A cross-file spread resolves: the loaded tree's diagnostics are
/// empty (the fixture's users table spreads a mixin from another
/// file). The same leaf parsed standalone is CLEAN too — parse
/// carries spreads opaque — but the resolution pass (the check-class
/// contract) reports the spread that names nothing in a standalone
/// document.
#[test]
fn cross_file_spread_resolves_under_the_loader() {
    let loaded = load_root();
    assert!(loaded.homes.len() >= 5, "the whole tree participated");
    let leaf = std::fs::read_to_string("tests/fixtures/include/tables/users.surdl")
        .expect("fixture reads");
    let (ast, diags) = surform::parser::parse_file_composable(&leaf);
    assert!(
        diags.is_empty(),
        "parse carries the spread opaque: {diags:?}"
    );
    let unresolved = surform::parser::resolve_spreads(&ast);
    assert!(
        unresolved
            .iter()
            .any(|diag| diag.code().unwrap().to_string() == "SDL-E1061"),
        "standalone, the spread resolves to nothing: {unresolved:?}"
    );
}

/// The document's mixin set is GLOBAL: a file's own body spreads a
/// mixin composed by ITS OWN include (not the root's) — the same
/// global set the flattening resolves against, or the tree fails
/// where the flat file compiles.
#[test]
fn a_nested_include_feeds_the_global_set() {
    let dir = tempdir();
    write(&dir.join("mix.surdl"), "mixin m { field x: int; };\n");
    write(
        &dir.join("a.surdl"),
        "include 'mix.surdl';\ntable t { ...m; };\n",
    );
    write(&dir.join("root.surdl"), "include 'a.surdl';\n");
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (flat, diags) =
        surform::parser::parse_file("mixin m { field x: int; };\ntable t { ...m; };\n");
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        codegen::emit(&loaded.file),
        codegen::emit(&flat),
        "flatten(root) ≡ root through two levels"
    );
}

/// The parent-redefines-its-own-child's-mixin direction collides
/// (SDL-E1062), exactly as the flat file rejects it — the resolution
/// pass catches the name two files define.
#[test]
fn a_parent_redefining_a_composed_mixin_collides() {
    let dir = tempdir();
    write(&dir.join("cmix.surdl"), "mixin m { field x: int; };\n");
    write(
        &dir.join("a.surdl"),
        "include 'cmix.surdl';\nmixin m { field y: int; };\ntable t { ...m; };\n",
    );
    write(&dir.join("root.surdl"), "include 'a.surdl';\n");
    let err = load_err(&dir.join("root.surdl"), "root.surdl");
    let (code, _) = first_error(&err);
    assert_eq!(code, "SDL-E1062", "the flat file's own rejection");
}

/// A directive problem anchored on an INCLUDE statement (whose scan
/// entry no splice block covers) still reports — composed lint reads
/// each home's whole scan for problems.
#[test]
fn an_allow_problem_on_an_include_still_reports() {
    let dir = tempdir();
    write(
        &dir.join("leaf.surdl"),
        "// surform-lint-allow: SDL-L9999\ninclude 'other.surdl';\ntable t {\n  permissions: full;\n};\n",
    );
    write(&dir.join("other.surdl"), "table other;\n");
    write(&dir.join("root.surdl"), "include 'leaf.surdl';\n");
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let resolved = surform::expand::expand(&loaded.file);
    let parts = loaded.lint_parts();
    let findings = lint::lint_composed(&loaded.file, &resolved, &parts, &loaded.diamonds);
    assert!(
        findings.iter().any(|f| f.kind.code_str() == "SDL-L0023"),
        "the typo'd code above the include reports: {findings:?}"
    );
}

/// The alias namespace is composition-global for the linter too: an
/// alias a CHILD declares and the ROOT uses is used (no finding), and
/// a child's dead alias anchors its SDL-L0027 in the child's own home
/// — the same finding the flattened twin produces.
#[test]
fn an_alias_s_use_and_death_are_composition_wide() {
    let dir = tempdir();
    write(
        &dir.join("types.surdl"),
        "type Email = string;\ntype Dead = int;\n",
    );
    write(
        &dir.join("root.surdl"),
        "include 'types.surdl';\ntable t { a: Email; };\n",
    );
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let resolved = surform::expand::expand(&loaded.file);
    let parts = loaded.lint_parts();
    let findings = lint::lint_composed(&loaded.file, &resolved, &parts, &loaded.diamonds);
    let unused: Vec<_> = findings
        .iter()
        .filter(|f| f.kind.code_str() == "SDL-L0027")
        .collect();
    assert_eq!(unused.len(), 1, "{findings:?}");
    assert_eq!(unused[0].key, "Dead");
    // The finding renders against the DECLARING file (the child).
    assert_eq!(unused[0].home, 1);
}

/// A cycle is LEGAL (the declarative namespace): the include graph
/// declares participation, not an elaboration order, and splice-once
/// makes every cycle terminate. A includes B includes A — both bodies
/// land, B's at its first declaration, the back-edge splicing
/// nothing.
#[test]
fn a_cycle_splices_once_and_loads() {
    let dir = tempdir();
    write(&dir.join("a.surdl"), "include 'b.surdl';\ntable a;\n");
    write(&dir.join("b.surdl"), "include 'a.surdl';\ntable b;\n");
    let loaded = load_ok(&dir.join("a.surdl"), "a.surdl");
    assert_eq!(loaded.homes.len(), 2, "both files participate");
    let ddl = codegen::emit(&loaded.file);
    assert!(ddl.contains("DEFINE TABLE OVERWRITE a"), "{ddl}");
    assert!(ddl.contains("DEFINE TABLE OVERWRITE b"), "{ddl}");
    // Splice-once: b's body lands exactly once, at a's include (the
    // DFS tiebreak), before a's own statement.
    assert_eq!(ddl.matches("DEFINE TABLE OVERWRITE b").count(), 1, "{ddl}");
    let b_at = ddl.find("DEFINE TABLE OVERWRITE b").expect("b is there");
    let a_at = ddl.find("DEFINE TABLE OVERWRITE a").expect("a is there");
    assert!(b_at < a_at, "pre-order: the child splices first: {ddl}");
}

/// The root is in the cycle's reach: a child including the root back
/// is a legal back-edge (the root canonicalizes like every other
/// file — a non-canonical root entry would silently re-splice its
/// body instead).
#[test]
fn a_child_including_the_root_is_a_back_edge() {
    let dir = tempdir();
    write(&dir.join("root.surdl"), "include 'leaf.surdl';\ntable t;\n");
    write(
        &dir.join("leaf.surdl"),
        "include 'root.surdl';\ntable leaf;\n",
    );
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let ddl = codegen::emit(&loaded.file);
    assert!(ddl.contains("DEFINE TABLE OVERWRITE leaf"), "{ddl}");
    assert_eq!(
        ddl.matches("DEFINE TABLE OVERWRITE t").count(),
        1,
        "the root's body splices once: {ddl}"
    );
}

/// A forward spread resolves (the declarative namespace): the root's
/// own table spreads a mixin composed AFTER it — the old positional
/// rule rejected this shape outright.
#[test]
fn a_forward_spread_resolves() {
    let dir = tempdir();
    write(&dir.join("mix.surdl"), "mixin m { field x: int; };\n");
    write(
        &dir.join("root.surdl"),
        "table t { ...m; };\ninclude 'mix.surdl';\n",
    );
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (flat, diags) =
        surform::parser::parse_file("table t { ...m; };\nmixin m { field x: int; };\n");
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        codegen::emit(&loaded.file),
        codegen::emit(&flat),
        "flatten(root) ≡ root with the mixin composed after its spread"
    );
}

/// A missing file is SDL-E0008 anchored on its include statement —
/// rendered against the INCLUDING file's text.
#[test]
fn a_missing_file_is_e0008_on_its_include() {
    let dir = tempdir();
    write(&dir.join("root.surdl"), "include 'gone.surdl';\ntable t;\n");
    let err = load_err(&dir.join("root.surdl"), "root.surdl");
    let (code, _) = first_error(&err);
    assert_eq!(code, "SDL-E0008");
    // Anchored: the diagnostic renders against the including file.
    let rendered = render_first(&err);
    assert!(rendered.contains("include 'gone.surdl'"), "{rendered}");
}

/// An absolute path is SDL-E0012 even when the file exists — a
/// schema tree must be portable.
#[test]
fn an_absolute_path_is_rejected() {
    let dir = tempdir();
    write(&dir.join("leaf.surdl"), "table leaf;\n");
    let absolute = dir.join("leaf.surdl");
    let absolute = absolute.to_string_lossy();
    write(
        &dir.join("root.surdl"),
        &format!("include '{absolute}';\ntable t;\n"),
    );
    let err = load_err(&dir.join("root.surdl"), "root.surdl");
    let (code, _) = first_error(&err);
    assert_eq!(code, "SDL-E0012");
}

/// Position rule 3: an included file carries no namespace/database —
/// SDL-E1070, against the included file's own text. The ROOT's
/// preamble is fine (the fixture proves it).
#[test]
fn an_included_file_carries_no_scopes() {
    let dir = tempdir();
    write(
        &dir.join("scoped.surdl"),
        "namespace acme;\ntable scoped;\n",
    );
    write(
        &dir.join("root.surdl"),
        "include 'scoped.surdl';\ntable t;\n",
    );
    let err = load_err(&dir.join("root.surdl"), "root.surdl");
    let (code, message) = first_error(&err);
    assert_eq!(code, "SDL-E1070", "{message}");
    let rendered = render_first(&err);
    assert!(rendered.contains("namespace acme"), "{rendered}");
}

/// A parse error inside an included file fails the load, rendered
/// against that file's own text.
#[test]
fn an_included_parse_error_fails_the_load() {
    let dir = tempdir();
    write(&dir.join("bad.surdl"), "table ;\n");
    write(&dir.join("root.surdl"), "include 'bad.surdl';\ntable t;\n");
    let err = load_err(&dir.join("root.surdl"), "root.surdl");
    let (code, _) = first_error(&err);
    assert!(
        code.starts_with("SDL-E1"),
        "the included file's own parse diagnostic: {code}"
    );
    let rendered = render_first(&err);
    assert!(rendered.contains("table ;"), "{rendered}");
}

/// Allow comments stay FILE-scoped: a directive in an included file
/// suppresses only that file's findings, and the root's suppresses
/// only the root's — even though per-file span coordinates overlap
/// numerically.
#[test]
fn allow_directives_stay_file_scoped() {
    let dir = tempdir();
    // Both files carry the same world-open table (SDL-L0001); only
    // the included file's is excused (a statement-level allow).
    write(
        &dir.join("leaf.surdl"),
        "// surform-lint-allow: SDL-L0001\ntable leaf {\n  permissions: full;\n};\n",
    );
    write(
        &dir.join("root.surdl"),
        "include 'leaf.surdl';\ntable mine {\n  permissions: full;\n};\n",
    );
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let resolved = surform::expand::expand(&loaded.file);
    let parts = loaded.lint_parts();
    let findings = lint::lint_composed(&loaded.file, &resolved, &parts, &loaded.diamonds);
    let l0001: Vec<_> = findings
        .iter()
        .filter(|f| f.kind.code_str() == "SDL-L0001")
        .collect();
    assert_eq!(
        l0001.len(),
        1,
        "exactly the root's finding survives: {l0001:?}"
    );
    assert!(
        loaded.homes[l0001[0].home].display.ends_with("root.surdl"),
        "the surviving finding renders against the root"
    );
}

/// The reverse direction: the ROOT's allow does not reach into the
/// composed file — the same suppression, exercised from the root's
/// side (the allow above the root's own table; the composed file's
/// world-open table still reports). The allow above an INCLUDE
/// statement itself excuses the diamond advisory now (its own test
/// below) — and still nothing else, because only that advisory
/// anchors on an include's span.
#[test]
fn the_roots_allow_does_not_reach_into_composed_files() {
    let dir = tempdir();
    write(
        &dir.join("leaf.surdl"),
        "table leaf {\n  permissions: full;\n};\n",
    );
    write(
        &dir.join("root.surdl"),
        "include 'leaf.surdl';\n// surform-lint-allow: SDL-L0001\ntable mine {\n  permissions: full;\n};\n",
    );
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let resolved = surform::expand::expand(&loaded.file);
    let parts = loaded.lint_parts();
    let findings = lint::lint_composed(&loaded.file, &resolved, &parts, &loaded.diamonds);
    let l0001: Vec<_> = findings
        .iter()
        .filter(|f| f.kind.code_str() == "SDL-L0001")
        .collect();
    assert_eq!(
        l0001.len(),
        1,
        "exactly the leaf's finding survives: {l0001:?}"
    );
    assert!(loaded.homes[l0001[0].home].display.ends_with("leaf.surdl"));
}

/// The same file included twice by ONE file is the hard error
/// (SDL-E0014): splice-once would silently absorb the duplicate —
/// the copy-paste smell to reject. Two spellings of one path are one
/// file (canonical dedupe).
#[test]
fn a_file_included_twice_by_one_file_errors() {
    let dir = tempdir();
    write(&dir.join("shared.surdl"), "param $x = 1;\n");
    write(
        &dir.join("root.surdl"),
        "include 'shared.surdl';\ninclude './shared.surdl';\n",
    );
    let err = load_err(&dir.join("root.surdl"), "root.surdl");
    let (code, message) = first_error(&err);
    assert_eq!(code, "SDL-E0014", "{message}");
    assert!(
        message.contains("root.surdl"),
        "the first declarer names: {message}"
    );
}

/// The duplicate rule tracks PER DECLARER: a file naming one path
/// twice is SDL-E0014 even when ANOTHER participant declared the file
/// first — the copy-paste smell must not hide behind an earlier
/// composition-wide declaration.
#[test]
fn a_duplicate_hides_behind_no_earlier_declarer() {
    let dir = tempdir();
    write(&dir.join("shared.surdl"), "param $x = 1;\n");
    write(&dir.join("a.surdl"), "include 'shared.surdl';\ntable a;\n");
    write(
        &dir.join("root.surdl"),
        "include 'a.surdl';\ninclude 'shared.surdl';\ninclude 'shared.surdl';\n",
    );
    let err = load_err(&dir.join("root.surdl"), "root.surdl");
    let (code, message) = first_error(&err);
    assert_eq!(code, "SDL-E0014", "{message}");
    assert!(
        message.contains("a.surdl"),
        "the error names the OTHER participant's earlier declaration, not \
         the declaring file: {message}"
    );
}

/// A file including ITSELF is a legal back-edge (an in-progress
/// reach): splice-once makes it a no-op.
#[test]
fn a_self_include_is_a_back_edge() {
    let dir = tempdir();
    write(&dir.join("root.surdl"), "include 'root.surdl';\ntable t;\n");
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let ddl = codegen::emit(&loaded.file);
    assert_eq!(
        ddl.matches("DEFINE TABLE OVERWRITE t").count(),
        1,
        "the body splices once: {ddl}"
    );
}

/// A DIAMOND (a file the discovery completed, declared again by
/// another file) is the SDL-L0026 advisory, anchored at the no-op
/// declaration and naming the first declarer. The no-op still
/// splices once.
#[test]
fn a_diamond_advises_l0026() {
    let dir = tempdir();
    write(&dir.join("shared.surdl"), "param $x = 1;\n");
    write(&dir.join("a.surdl"), "include 'shared.surdl';\ntable a;\n");
    write(&dir.join("b.surdl"), "include 'shared.surdl';\ntable b;\n");
    write(
        &dir.join("root.surdl"),
        "include 'a.surdl';\ninclude 'b.surdl';\n",
    );
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    assert_eq!(
        loaded.diamonds.len(),
        1,
        "b's declaration of shared is the no-op: {:?}",
        loaded.diamonds
    );
    assert_eq!(loaded.diamonds[0].declarer, "a.surdl");
    assert_eq!(loaded.homes.len(), 4, "root, a, b, shared");
    let ddl = codegen::emit(&loaded.file);
    assert_eq!(ddl.matches("DEFINE PARAM").count(), 1, "{ddl}");
    // The advisory rides the composed lint.
    let resolved = surform::expand::expand(&loaded.file);
    let parts = loaded.lint_parts();
    let findings = lint::lint_composed(&loaded.file, &resolved, &parts, &loaded.diamonds);
    let l0026: Vec<_> = findings
        .iter()
        .filter(|f| f.kind.code_str() == "SDL-L0026")
        .collect();
    assert_eq!(l0026.len(), 1, "{findings:?}", findings = findings);
    assert!(
        l0026[0].to_string().contains("a.surdl"),
        "the message names the first declarer: {l0026:?}"
    );
    // Anchored at the no-op declaration, rendered against ITS home.
    assert_eq!(l0026[0].home, 3, "b's own home (root, a, shared, b)");
}

/// The diamond advisory opts out through the allow comment above the
/// no-op include — the #43 geography closes: the include's scan
/// entry sits outside every splice block, and suppression reads the
/// home's whole scan for exactly this anchor class.
#[test]
fn the_diamond_advisory_suppresses_through_the_allow_comment() {
    let dir = tempdir();
    write(&dir.join("shared.surdl"), "param $x = 1;\n");
    write(&dir.join("a.surdl"), "include 'shared.surdl';\ntable a;\n");
    write(
        &dir.join("b.surdl"),
        "// surform-lint-allow: SDL-L0026\ninclude 'shared.surdl';\ntable b;\n",
    );
    write(
        &dir.join("root.surdl"),
        "include 'a.surdl';\ninclude 'b.surdl';\n",
    );
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    assert_eq!(loaded.diamonds.len(), 1);
    let resolved = surform::expand::expand(&loaded.file);
    let parts = loaded.lint_parts();
    let findings = lint::lint_composed(&loaded.file, &resolved, &parts, &loaded.diamonds);
    assert!(
        findings.iter().all(|f| f.kind.code_str() != "SDL-L0026"),
        "the allow above the no-op include excuses it: {findings:?}"
    );
}

/// A no-op diamond include is a no-op, not an error — and its
/// anchored comments move to its neighbors (the leading comment
/// leads what follows in its own file).
#[test]
fn a_deduped_include_is_a_no_op() {
    let dir = tempdir();
    write(
        &dir.join("shared.surdl"),
        "// shared preamble\nparam $x = 1;\n",
    );
    write(&dir.join("a.surdl"), "include 'shared.surdl';\n");
    write(
        &dir.join("root.surdl"),
        "include 'a.surdl';\n\n// a's own note\ninclude 'shared.surdl';\ntable root;\n",
    );
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let ddl = codegen::emit(&loaded.file);
    assert_eq!(ddl.matches("DEFINE PARAM").count(), 1, "{ddl}");
    assert_eq!(loaded.homes.len(), 3, "root, a, shared only");
}

/// A trailing no-op include's leading comments still land in the
/// flattened document (the file-tail slot — nothing follows them).
#[test]
fn a_trailing_noop_include_keeps_its_comments() {
    let dir = tempdir();
    write(&dir.join("shared.surdl"), "param $x = 1;\n");
    write(&dir.join("a.surdl"), "include 'shared.surdl';\n");
    write(
        &dir.join("root.surdl"),
        "include 'a.surdl';\n\n// trailing no-op re-include\ninclude 'shared.surdl';\n",
    );
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    assert_eq!(
        loaded.file.trailing_comments,
        vec![" trailing no-op re-include".to_string()],
        "the pending leading comments flush to the document tail (the \
         text keeps its authored leading space)"
    );
}

// ---------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------

/// Per-test scratch directory, unique by construction (the clock's
/// nanoseconds collide on macOS — the sequence counter names it) and
/// removed when the test's guard drops.
struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn tempdir() -> TempDir {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "surform-include-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("tempdir creates");
    TempDir(dir)
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("fixture writes");
}

fn load_err(root: &Path, display: &str) -> load::LoadFailure {
    let text = std::fs::read_to_string(root).expect("fixture reads");
    load::load(root, display, text).expect_err("the tree fails to load")
}

fn load_ok(root: &Path, display: &str) -> load::Loaded {
    let text = std::fs::read_to_string(root).expect("fixture reads");
    load::load(root, display, text).expect("the tree loads clean")
}

// ---------------------------------------------------------------------
// write-back distribution (spec/include.md)
// ---------------------------------------------------------------------

#[path = "common/exec.rs"]
mod exec;

use surform::info::normalize::{Interchange, ReadModel, TABLE_CHILDREN_KEY, merge_with_provenance};
use surform::info::read::{InfoObject, InfoValue, read_info_object};
use surform::writeback;
use surrealdb::Surreal;
use surrealdb::engine::local::Db;

/// The engine's INFO over the scopes the tree declares, with the
/// `INFO FOR TABLE` child maps assembled into the reserved
/// [`TABLE_CHILDREN_KEY`] — the interchange the CLI's `--info` files
/// carry.
async fn model_for(db: &Surreal<Db>, ns: &str, db_name: &str) -> ReadModel {
    db.use_ns(ns).use_db(db_name).await.ok();
    let root =
        read_info_object(&exec::info_for(db, "INFO FOR ROOT").await).expect("root INFO walks");
    let ns_obj = read_info_object(&exec::info_for(db, "INFO FOR NS").await).expect("ns INFO walks");
    let mut db_obj =
        read_info_object(&exec::info_for(db, "INFO FOR DB").await).expect("db INFO walks");
    let mut children = BTreeMap::new();
    if let Some(InfoValue::Object(tables)) = db_obj.0.get("tables") {
        for name in tables.0.keys() {
            let text = exec::info_for(db, &format!("INFO FOR TABLE {name}")).await;
            let obj = read_info_object(&text).expect("table INFO walks");
            children.insert(name.clone(), InfoValue::Object(obj));
        }
    }
    db_obj.0.insert(
        TABLE_CHILDREN_KEY.to_string(),
        InfoValue::Object(InfoObject(children)),
    );
    ReadModel::from_interchange(Interchange {
        root: Some(&root),
        namespaces: &[(ns.to_string(), &ns_obj)],
        databases: &[(ns.to_string(), db_name.to_string(), &db_obj)],
    })
    .expect("every scope reads")
}

/// The write-back tree: preamble → one comment-annotated include →
/// the users file → the root's own account table. Deliberately
/// NON-canonical (four-space bodies the writer re-indents).
fn writeback_tree(dir: &Path) {
    write(
        &dir.join("root.surdl"),
        "namespace acme;\ndatabase shop;\n\n\
         // the user table lives in its own file\n\
         include 'tables/users.surdl';\n\n\
         table account {\n    field owner: record<users>;\n};\n",
    );
    std::fs::create_dir_all(dir.join("tables")).expect("tables dir");
    write(
        &dir.join("tables/users.surdl"),
        "table users {\n    schemafull;\n    field name: string;\n};\n",
    );
}

/// Writes a plan out (the CLI's `--in-place` loop, in-process).
fn apply(plan: &[(usize, String)], loaded: &load::Loaded) {
    for (home, text) in plan {
        std::fs::write(&loaded.homes[*home].path, text).expect("plan writes");
    }
}

/// The no-model write-back canonicalizes each home (an identity merge
/// distributes): includes survive verbatim with their annotations, a
/// root comment above an include rides the transfer onto the child's
/// first statement, the tree reloads to the same DDL (which proves
/// the preamble rules held), and a second plan writes nothing.
#[test]
fn the_no_model_write_back_canonicalizes_each_home() {
    let dir = tempdir();
    writeback_tree(&dir);
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged, provenance) = merge_with_provenance(&loaded.file, &ReadModel::default());
    let plan = writeback::plan(&loaded, &merged, &provenance, 80).expect("the tree distributes");
    // Both homes were non-canonical: both rewrite.
    assert_eq!(plan.len(), 2, "{plan:?}");
    apply(&plan, &loaded);
    let users_text = std::fs::read_to_string(dir.join("tables/users.surdl")).expect("users reads");
    // The transferred comment leads the child's first statement, and
    // the body re-indented to the canonical two spaces.
    assert!(
        users_text.starts_with("// the user table lives in its own file\n"),
        "{users_text}"
    );
    assert!(users_text.contains("  field name: string;"), "{users_text}");
    let root_text = std::fs::read_to_string(dir.join("root.surdl")).expect("root reads");
    // The annotation lives with the child's statement now (the
    // transfer is the flattened truth): the root's include is bare,
    // and the preamble keeps its shape.
    assert!(
        root_text.contains("include 'tables/users.surdl';"),
        "{root_text}"
    );
    assert!(
        !root_text.contains("// the user table lives in its own file"),
        "{root_text}"
    );
    assert!(
        root_text.starts_with("namespace acme;\n\ndatabase shop;\n"),
        "{root_text}"
    );
    // The rewritten tree loads and emits the merged DDL.
    let reloaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    assert_eq!(codegen::emit(&reloaded.file), codegen::emit(&merged));
    // The fixed point: a second plan writes nothing.
    let (merged2, provenance2) = merge_with_provenance(&reloaded.file, &ReadModel::default());
    assert!(
        writeback::plan(&reloaded, &merged2, &provenance2, 80)
            .expect("second plan")
            .is_empty()
    );
}

/// A twin-folded statement leaves its home without misaligning the
/// survivors: the root's standalone field folds away (users carries
/// the identical nested twin) and the remaining root statements keep
/// their authored positions.
#[test]
fn a_twin_folded_statement_leaves_its_home() {
    let dir = tempdir();
    write(
        &dir.join("root.surdl"),
        "namespace acme;\ndatabase shop;\n\n\
         include 'tables/users.surdl';\n\n\
         field name: string { on: users; };\n\n\
         table account {\n  field owner: record<users>;\n};\n",
    );
    std::fs::create_dir_all(dir.join("tables")).expect("tables dir");
    write(
        &dir.join("tables/users.surdl"),
        "table users {\n  field name: string;\n};\n",
    );
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged, provenance) = merge_with_provenance(&loaded.file, &ReadModel::default());
    // The standalone field folded away.
    assert!(
        merged
            .items
            .iter()
            .all(|stmt| !matches!(stmt, surform::ast::Stmt::Field(_)))
    );
    let plan = writeback::plan(&loaded, &merged, &provenance, 80).expect("distributes");
    apply(&plan, &loaded);
    let root_text = std::fs::read_to_string(dir.join("root.surdl")).expect("root reads");
    assert!(!root_text.contains("field name: string;"), "{root_text}");
    assert!(root_text.contains("table account"), "{root_text}");
    let reloaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    assert_eq!(codegen::emit(&reloaded.file), codegen::emit(&merged));
}

/// Adopted drift returns to the file that owns its scope: a new field
/// on users folds into the table's home file; a whole new table tails
/// into the root's body after its last statement. The rewritten tree
/// flattens to the merged DDL exactly, and a second normalize writes
/// nothing.
#[tokio::test]
async fn normalize_distributes_adopted_drift_to_its_homes() {
    let dir = tempdir();
    writeback_tree(&dir);
    let db = exec::mem_db().await;
    exec::exec_sdl(
        &db,
        "namespace acme;\ndatabase shop;\n\
         table users schemafull {\n  field name: string;\n  field active: bool;\n};\n\
         table account {\n  field owner: record<users>;\n};\n\
         table audit schemafull {\n  field stamp: datetime;\n};\n",
    )
    .await;
    let model = model_for(&db, "acme", "shop").await;
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged, provenance) = merge_with_provenance(&loaded.file, &model);
    let plan = writeback::plan(&loaded, &merged, &provenance, 80).expect("distributes");
    apply(&plan, &loaded);
    let users_text = std::fs::read_to_string(dir.join("tables/users.surdl")).expect("users reads");
    assert!(users_text.contains("field active: bool;"), "{users_text}");
    let root_text = std::fs::read_to_string(dir.join("root.surdl")).expect("root reads");
    // The adopted table's placement floor sits at its section's head —
    // inside the root's preamble region — so the relocation rule moves
    // it to the body's first position: after the include, before the
    // root's own table.
    let audit_at = root_text.find("table audit").expect("audit wrote back");
    let include_at = root_text
        .find("include 'tables/users.surdl';")
        .expect("the include stays");
    let account_at = root_text.find("table account").expect("account stays");
    assert!(
        audit_at > include_at && audit_at < account_at,
        "{root_text}"
    );
    // Order fidelity modulo the one relocated statement: the rewritten
    // tree flattens to the merged DDL's line multiset.
    let reloaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let lines = |ddl: &str| -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for line in ddl.lines() {
            *counts.entry(line.to_string()).or_insert(0) += 1;
        }
        counts
    };
    assert_eq!(
        lines(&codegen::emit(&reloaded.file)),
        lines(&codegen::emit(&merged))
    );
    // Convergence: a second normalize writes nothing.
    let (merged2, provenance2) = merge_with_provenance(&reloaded.file, &model);
    assert!(
        writeback::plan(&reloaded, &merged2, &provenance2, 80)
            .expect("second plan")
            .is_empty()
    );
}

/// A lifted fulltext index places by its stream neighbors: the table's
/// nested index whose analyzer sits after the table lifts to top level
/// (the merge's placement), and the write-back must land it next to
/// the analyzer — not on the table's slot, which stays the table's.
#[test]
fn a_lifted_index_lands_beside_its_analyzer() {
    let dir = tempdir();
    write(
        &dir.join("root.surdl"),
        "namespace acme;\ndatabase shop;\n\n\
         include 'tables/users.surdl';\n\n\
         analyzer english {\n  tokenizers: class;\n};\n\n\
         table account {\n  field owner: record<users>;\n};\n",
    );
    std::fs::create_dir_all(dir.join("tables")).expect("tables dir");
    write(
        &dir.join("tables/users.surdl"),
        "table users schemafull {\n  field title: string;\n  index fulltext ft {\n    fields: title;\n    analyzer: english;\n  };\n};\n",
    );
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged, provenance) = merge_with_provenance(&loaded.file, &ReadModel::default());
    // The lift fired: the index is top level, after its analyzer.
    let ft_at = merged
        .items
        .iter()
        .position(|stmt| matches!(stmt, surform::ast::Stmt::Index(_)))
        .expect("the index lifted");
    let analyzer_at = merged
        .items
        .iter()
        .position(|stmt| matches!(stmt, surform::ast::Stmt::Analyzer(_)))
        .expect("the analyzer is skeleton");
    assert_eq!(ft_at, analyzer_at + 1);
    // The table survived: one input index per table statement.
    let table_provs: Vec<usize> = merged
        .items
        .iter()
        .zip(&provenance)
        .filter_map(|(stmt, prov)| match (stmt, prov) {
            (surform::ast::Stmt::Table(_), Some(input)) => Some(*input),
            _ => None,
        })
        .collect();
    assert_eq!(table_provs.len(), 2, "two tables, two provenances");
    assert_ne!(table_provs[0], table_provs[1], "distinct inputs");
    let plan = writeback::plan(&loaded, &merged, &provenance, 80).expect("distributes");
    apply(&plan, &loaded);
    let users_text = std::fs::read_to_string(dir.join("tables/users.surdl")).expect("users reads");
    assert!(!users_text.contains("fulltext"), "{users_text}");
    let root_text = std::fs::read_to_string(dir.join("root.surdl")).expect("root reads");
    let ft_at = root_text
        .find("index fulltext ft")
        .expect("the index wrote back");
    let analyzer_at = root_text
        .find("analyzer `english` {")
        .expect("the analyzer stays");
    assert!(ft_at > analyzer_at, "{root_text}");
    let reloaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    assert_eq!(codegen::emit(&reloaded.file), codegen::emit(&merged));
    let (merged2, provenance2) = merge_with_provenance(&reloaded.file, &ReadModel::default());
    assert!(
        writeback::plan(&reloaded, &merged2, &provenance2, 80)
            .expect("second plan")
            .is_empty()
    );
}

/// An adopted `database` (a scope statement) refuses the whole
/// write-back with SDL-E0013 — the plan is pure, so nothing wrote and
/// the tree still loads as it was.
#[tokio::test]
async fn an_adopted_scope_statement_refuses_the_write_back() {
    let dir = tempdir();
    writeback_tree(&dir);
    let db = exec::mem_db().await;
    exec::exec_sdl(
        &db,
        "namespace acme;\ndatabase shop;\n\
         table users schemafull {\n  field name: string;\n};\n\
         table account {\n  field owner: record<users>;\n};\n\
         database trace;\n",
    )
    .await;
    let model = model_for(&db, "acme", "shop").await;
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged, provenance) = merge_with_provenance(&loaded.file, &model);
    let error = writeback::plan(&loaded, &merged, &provenance, 80).expect_err("refuses");
    assert_eq!(
        error.code().map(|code| code.to_string()),
        Some("SDL-E0013".to_string())
    );
    let reloaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    assert!(!codegen::emit(&reloaded.file).contains("trace"));
}

/// A root-scope user exists only on the database side: its adoptee
/// slots into the root's preamble region, which no file may carry
/// (SDL-E1069). Dependency-free, it relocates to the root's first
/// body position — after the last include — and the tree reloads.
#[tokio::test]
async fn a_preamble_bound_adoptee_relocates_to_the_body() {
    let dir = tempdir();
    writeback_tree(&dir);
    let db = exec::mem_db().await;
    exec::exec_sdl(
        &db,
        "namespace acme;\ndatabase shop;\n\
         table users schemafull {\n  field name: string;\n};\n\
         table account {\n  field owner: record<users>;\n};\n",
    )
    .await;
    db.query("DEFINE USER writer ON ROOT PASSWORD 's3curetpass' ROLES EDITOR")
        .await
        .expect("the user defines")
        .check()
        .expect("the user applies");
    let model = model_for(&db, "acme", "shop").await;
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged, provenance) = merge_with_provenance(&loaded.file, &model);
    let plan = writeback::plan(&loaded, &merged, &provenance, 80).expect("relocates");
    apply(&plan, &loaded);
    let root_text = std::fs::read_to_string(dir.join("root.surdl")).expect("root reads");
    let user_at = root_text.find("user writer").expect("the user wrote back");
    let include_at = root_text
        .find("include 'tables/users.surdl';")
        .expect("the include stays");
    assert!(user_at > include_at, "{root_text}");
    // The relocated tree reloads (no E1069) and carries every DDL
    // statement the merge produced — order deliberately deviates at
    // the one relocated statement, so the comparison is the line
    // multiset.
    let reloaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let lines = |ddl: &str| -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for line in ddl.lines() {
            *counts.entry(line.to_string()).or_insert(0) += 1;
        }
        counts
    };
    assert_eq!(
        lines(&codegen::emit(&reloaded.file)),
        lines(&codegen::emit(&merged))
    );
    // Convergence: a second normalize writes nothing.
    let (merged2, provenance2) = merge_with_provenance(&reloaded.file, &model);
    assert!(
        writeback::plan(&reloaded, &merged2, &provenance2, 80)
            .expect("second plan")
            .is_empty()
    );
}

/// The first diagnostic's `(code, message)` from a failed load.
fn first_error(failure: &load::LoadFailure) -> (String, String) {
    let (diag, _) = failure.errors.first().expect("at least one diagnostic");
    let code = diag.code().unwrap().to_string();
    (code, diag.to_string())
}

/// Renders the first diagnostic against its home's text the way the
/// CLI does — the graphical snippet must name the right file's
/// content.
fn render_first(failure: &load::LoadFailure) -> String {
    let (diag, home) = failure.errors.first().expect("at least one diagnostic");
    let report =
        miette::Report::new(diag.clone()).with_source_code(failure.homes[*home].text.clone());
    format!("{report:?}")
}

/// A view created in SurrealQL adopts as structured SDL when its INFO
/// shape matches SDL's fixed view emission (`TYPE ANY SCHEMALESS`):
/// the adoptee places after the tables its query reads, and the tree
/// converges.
#[tokio::test]
async fn a_structured_view_adopts_and_converges() {
    let dir = tempdir();
    writeback_tree(&dir);
    let db = exec::mem_db().await;
    exec::exec_sdl(
        &db,
        "namespace acme;\ndatabase shop;\n\
         table users schemafull {\n  field name: string;\n};\n\
         table account {\n  field owner: record<users>;\n};\n",
    )
    .await;
    db.use_ns("acme").use_db("shop").await.ok();
    db.query("DEFINE TABLE v AS SELECT * FROM users PERMISSIONS FULL")
        .await
        .expect("the view defines")
        .check()
        .expect("the view applies");
    let model = model_for(&db, "acme", "shop").await;
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged, provenance) = merge_with_provenance(&loaded.file, &model);
    let plan = writeback::plan(&loaded, &merged, &provenance, 80).expect("distributes");
    apply(&plan, &loaded);
    // The view's dependency bound sits it directly after its FROM
    // table, so its stream neighbor — and home — is the users file.
    let users_text = std::fs::read_to_string(dir.join("tables/users.surdl")).expect("users reads");
    assert_eq!(
        users_text.matches("table view v").count(),
        1,
        "{users_text}"
    );
    let reloaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged2, provenance2) = merge_with_provenance(&reloaded.file, &model);
    let plan2 = writeback::plan(&reloaded, &merged2, &provenance2, 80).expect("second plan");
    assert!(plan2.is_empty(), "{plan2:?}");
}

/// A raw-routed statement converges: a view whose INFO shape deviates
/// from SDL's fixed emission (`TYPE NORMAL` here — the text is
/// rewritten before the model reads it) routes verbatim to `Raw`. It
/// adopts once, and its second normalize must CONSUME its entry — the
/// authored raw matches by its DEFINE header's name — instead of
/// re-adopting a duplicate on every run. Its home follows the
/// dependency bound, exactly as the structured twin's does: after its
/// `FROM` table, in the users file (the reparse-based raw dep read
/// unified the parities — the scanner-era read missed the view's
/// source and trailed the adoptee into the root).
#[tokio::test]
async fn a_raw_routed_view_adopts_once_and_converges() {
    let dir = tempdir();
    writeback_tree(&dir);
    let db = exec::mem_db().await;
    exec::exec_sdl(
        &db,
        "namespace acme;\ndatabase shop;\n\
         table users schemafull {\n  field name: string;\n};\n\
         table account {\n  field owner: record<users>;\n};\n",
    )
    .await;
    db.use_ns("acme").use_db("shop").await.ok();
    db.query("DEFINE TABLE v AS SELECT * FROM users PERMISSIONS FULL")
        .await
        .expect("the view defines")
        .check()
        .expect("the view applies");
    // The raw route: TYPE NORMAL on a view deviates from SDL's fixed
    // `TYPE ANY SCHEMALESS`, so the reader routes it verbatim.
    db.use_ns("acme").use_db("shop").await.ok();
    let root =
        read_info_object(&exec::info_for(&db, "INFO FOR ROOT").await).expect("root INFO walks");
    let ns_obj =
        read_info_object(&exec::info_for(&db, "INFO FOR NS").await).expect("ns INFO walks");
    let mut db_obj = read_info_object(
        &exec::info_for(&db, "INFO FOR DB")
            .await
            .replace("TYPE ANY", "TYPE NORMAL"),
    )
    .expect("db INFO walks");
    let mut children = BTreeMap::new();
    if let Some(InfoValue::Object(tables)) = db_obj.0.get("tables") {
        for name in tables.0.keys() {
            let text = exec::info_for(&db, &format!("INFO FOR TABLE {name}")).await;
            let obj = read_info_object(&text).expect("table INFO walks");
            children.insert(name.clone(), InfoValue::Object(obj));
        }
    }
    db_obj.0.insert(
        TABLE_CHILDREN_KEY.to_string(),
        InfoValue::Object(InfoObject(children)),
    );
    let model = ReadModel::from_interchange(Interchange {
        root: Some(&root),
        namespaces: &[("acme".to_string(), &ns_obj)],
        databases: &[("acme".to_string(), "shop".to_string(), &db_obj)],
    })
    .expect("every scope reads");
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged, provenance) = merge_with_provenance(&loaded.file, &model);
    // The route really is raw: the merged document carries the
    // verbatim body.
    assert!(
        merged
            .items
            .iter()
            .any(|stmt| matches!(stmt, surform::ast::Stmt::Raw(_))),
        "the deviant view must route raw"
    );
    let plan = writeback::plan(&loaded, &merged, &provenance, 80).expect("distributes");
    apply(&plan, &loaded);
    // The dependency bound places the adoptee the same way it places
    // its structured twin (the reparse-based raw dep read unified the
    // parities): directly after its FROM table, so its stream
    // neighbor — and home — is the users file. The ONCE is the point:
    // exactly one copy in the whole tree, never one per re-run.
    let once = std::fs::read_to_string(dir.join("root.surdl")).expect("root reads");
    let users_once = std::fs::read_to_string(dir.join("tables/users.surdl")).expect("users reads");
    assert_eq!(
        once.matches("DEFINE TABLE v").count() + users_once.matches("DEFINE TABLE v").count(),
        1,
        "the raw view adopts exactly once in the tree: {once}\n--- users.surdl ---\n{users_once}"
    );
    assert!(
        users_once.contains("raw {\nDEFINE TABLE v"),
        "the raw view homes after its FROM table, the users file:\n{users_once}"
    );
    // The fixed point (the raw rematch): the authored raw consumes its
    // entry — the second plan is empty and the text is byte-stable.
    let reloaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged2, provenance2) = merge_with_provenance(&reloaded.file, &model);
    let plan2 = writeback::plan(&reloaded, &merged2, &provenance2, 80).expect("second plan");
    assert!(plan2.is_empty(), "{plan2:?}");
}

/// A preamble-bound adoptee whose only dependents share its home
/// relocates instead of refusing: an adopted analyzer whose fulltext
/// index folded into the ROOT's own table (same home) moves to the
/// body's first position — ahead of its dependent — and the tree
/// converges. The refusing shape is the FOREIGN dependent (an index
/// inside a composed file).
#[tokio::test]
async fn a_same_home_dependent_relocates_rather_than_refuses() {
    let dir = tempdir();
    write(
        &dir.join("root.surdl"),
        "namespace acme;\ndatabase shop;\n\n\
         include 'tables/users.surdl';\n\n\
         table doc {\n  field title: string;\n};\n",
    );
    std::fs::create_dir_all(dir.join("tables")).expect("tables dir");
    write(
        &dir.join("tables/users.surdl"),
        "table users schemafull {\n  field name: string;\n};\n",
    );
    let db = exec::mem_db().await;
    exec::exec_sdl(
        &db,
        "namespace acme;\ndatabase shop;\n\
         analyzer english {\n  tokenizers: class;\n};\n\
         table users schemafull {\n  field name: string;\n};\n\
         table doc schemafull {\n  field title: string;\n  index fulltext ft {\n    fields: title;\n    analyzer: english;\n  };\n};\n",
    )
    .await;
    let model = model_for(&db, "acme", "shop").await;
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged, provenance) = merge_with_provenance(&loaded.file, &model);
    // The analyzer keyed inside the preamble region relocates (its
    // dependent table shares the root home) instead of refusing.
    let plan = writeback::plan(&loaded, &merged, &provenance, 80).expect("relocates");
    apply(&plan, &loaded);
    let root_text = std::fs::read_to_string(dir.join("root.surdl")).expect("root reads");
    let analyzer_at = root_text
        .find("analyzer `english` {")
        .expect("the analyzer wrote back");
    let include_at = root_text
        .find("include 'tables/users.surdl';")
        .expect("the include stays");
    let doc_at = root_text.find("table doc").expect("doc stays");
    assert!(
        analyzer_at > include_at && analyzer_at < doc_at,
        "{root_text}"
    );
    // Define order holds on reload, modulo the one relocation (line
    // multiset), and the tree converges.
    let reloaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let lines = |ddl: &str| -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for line in ddl.lines() {
            *counts.entry(line.to_string()).or_insert(0) += 1;
        }
        counts
    };
    assert_eq!(
        lines(&codegen::emit(&reloaded.file)),
        lines(&codegen::emit(&merged))
    );
    let (merged2, provenance2) = merge_with_provenance(&reloaded.file, &model);
    assert!(
        writeback::plan(&reloaded, &merged2, &provenance2, 80)
            .expect("second plan")
            .is_empty()
    );
}

/// A preamble-bound adoptee with an ADOPTED dependent keyed into a
/// foreign home refuses: the dependent's placement bound rises past
/// the root's authored table in the composed file, so it keys there —
/// inside the include subtree the relocation would jump past — and
/// relocating would define the view before the table it reads.
#[tokio::test]
async fn an_adopted_foreign_dependent_refuses_the_relocation() {
    let dir = tempdir();
    write(
        &dir.join("root.surdl"),
        "namespace acme;\ndatabase shop;\n\n\
         include 'g.surdl';\n\n\
         table meta {\n  field name: string;\n};\n",
    );
    write(
        &dir.join("g.surdl"),
        "table post {\n  field title: string;\n};\n",
    );
    let db = exec::mem_db().await;
    exec::exec_sdl(
        &db,
        "namespace acme;\ndatabase shop;\n\
         table user;\n\
         table post {\n  field title: string;\n};\n\
         table view feed {\n  SELECT * FROM user, post;\n};\n\
         table meta {\n  field name: string;\n};\n",
    )
    .await;
    let model = model_for(&db, "acme", "shop").await;
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged, provenance) = merge_with_provenance(&loaded.file, &model);
    // The repro shape holds: the `user` adoptee keys into the root's
    // preamble region, and the `feed` view (which reads it) keyed into
    // g.surdl after the authored `post`.
    let error = writeback::plan(&loaded, &merged, &provenance, 80).expect_err("refuses");
    assert_eq!(
        error.code().map(|code| code.to_string()),
        Some("SDL-E0013".to_string())
    );
    // Plan purity: nothing wrote, the tree loads as it was.
    let reloaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    assert!(!codegen::emit(&reloaded.file).contains("feed"));
}

/// The refusing branch for a FOREIGN AUTHORED dependent: an adopted
/// analyzer whose only dependent is an index authored inside a
/// composed file cannot relocate — the analyzer would land past that
/// file's include subtree, after the index that names it.
#[tokio::test]
async fn a_foreign_authored_dependent_refuses_the_relocation() {
    let dir = tempdir();
    write(
        &dir.join("root.surdl"),
        "namespace acme;\ndatabase shop;\n\n\
         include 'g.surdl';\n",
    );
    write(
        &dir.join("g.surdl"),
        "table post schemafull {\n  field title: string;\n  index fulltext ft {\n    fields: title;\n    analyzer: english;\n  };\n};\n",
    );
    let db = exec::mem_db().await;
    exec::exec_sdl(
        &db,
        "namespace acme;\ndatabase shop;\n\
         analyzer english {\n  tokenizers: class;\n};\n\
         table post schemafull {\n  field title: string;\n  index fulltext ft {\n    fields: title;\n    analyzer: english;\n  };\n};\n",
    )
    .await;
    let model = model_for(&db, "acme", "shop").await;
    let loaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    let (merged, provenance) = merge_with_provenance(&loaded.file, &model);
    let error = writeback::plan(&loaded, &merged, &provenance, 80).expect_err("refuses");
    assert_eq!(
        error.code().map(|code| code.to_string()),
        Some("SDL-E0013".to_string())
    );
    let reloaded = load_ok(&dir.join("root.surdl"), "root.surdl");
    assert!(!codegen::emit(&reloaded.file).contains("analyzer"));
}
