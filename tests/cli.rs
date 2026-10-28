//! CLI behavior: `surform check` / `surform build` exit codes, output
//! routing (DDL to stdout, diagnostics to stderr), the `--json`
//! renderer, the friendly missing-file diagnostic, and the `raw`
//! passthrough.

use assert_cmd::Command;

fn cli() -> Command {
    Command::cargo_bin("surform").expect("the surform binary builds")
}

fn fixture(name: &str) -> String {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("fixtures");
    path.push(name);
    path.to_string_lossy().into_owned()
}

const CLEAN_DDL: &str = "\
DEFINE NAMESPACE OVERWRITE acme COMMENT 'Acme Corporation';
USE NS acme;
DEFINE NAMESPACE OVERWRITE staging;
USE NS staging;
DEFINE SEQUENCE IF NOT EXISTS invoice_number START 1000;
DEFINE PARAM OVERWRITE $default_limit VALUE 50 COMMENT 'Default page size for list endpoints';
";

/// The database fixture pins the full statement batch: the spec's
/// canonical clause order, the `USE NS … DB` session line bound to
/// the nearest preceding namespace, and the canonical duration
/// print: SDL's `expiry: 7d` stores one week of seconds and emits
/// `1w` (the server's own greedy decomposition; the value, not the
/// spelling, is what round-trips). The mixin pins the expansion
/// contract — the statement itself emits nothing, its members
/// expand in place at the spread's position (spec/mixin.md). The
/// user/analyzer statements pin
/// the define-once `OVERWRITE` choice (a secret-carrying user) and
/// the keyword-escaped analyzer name (`english` is a SurrealQL
/// language keyword); the module pins the one 3.3-unconditional
/// emission; the function pins the two-block form's body canon and
/// trailing-clause order; the configs pin the singleton `OVERWRITE`,
/// the spec-minimal graphql families, and the ROOT-region
/// hoist of `config default` ahead of the file's first `USE` line;
/// the index pins the standalone `on:` form and the kind clause after
/// FIELDS; the api pins the hoisted `FOR ANY` clause and the
/// minimal-form PERMISSIONS (printed only where SDL wrote one); the
/// access pins the jwt variant's URL form with only its written
/// duration side.
const APP_DDL: &str = "\
DEFINE NAMESPACE OVERWRITE acme COMMENT 'Acme Corporation';
DEFINE CONFIG OVERWRITE DEFAULT NAMESPACE 'acme' DATABASE 'shop';
USE NS acme;
DEFINE DATABASE OVERWRITE app
    CHANGEFEED 1w INCLUDE ORIGINAL
    STRICT
    COMMENT 'Primary application database';
USE NS acme DB app;
DEFINE SEQUENCE IF NOT EXISTS invoice_number START 1000;
DEFINE PARAM OVERWRITE $default_limit VALUE 50 COMMENT 'Default page size for list endpoints';
DEFINE USER OVERWRITE deployer ON NAMESPACE
    PASSHASH '$argon2id$v=19$'
    ROLES EDITOR, VIEWER
    COMMENT 'CI deployment account';
DEFINE ANALYZER OVERWRITE `english`
    TOKENIZERS CLASS
    FILTERS SNOWBALL(ENGLISH), LOWERCASE
    COMMENT 'English text search';
DEFINE EVENT OVERWRITE user_created ON `user`
    WHEN $event = 'CREATE'
    THEN { CREATE log SET user = $after.id }
    COMMENT 'Log every user creation';
DEFINE EVENT OVERWRITE webhook_fanout ON invoice
    ASYNC RETRY 5 MAXDEPTH 8
    THEN (CREATE delivery SET invoice = $after.id),
    (UPDATE stats:webhooks SET fired = fired + 1);
DEFINE MODULE OVERWRITE mod::color FROM f\"modules:/color.surli\" UNSIGNED COMMENT 'Colour helpers';
DEFINE FUNCTION OVERWRITE fn::can_edit($p: record<post>) -> bool {
    RETURN $auth.id = $p.author OR $auth.role = 'admin'
}
    PERMISSIONS WHERE $auth.admin = true
    COMMENT 'Post permissions';
DEFINE CONFIG OVERWRITE API
    MIDDLEWARE api::timeout(5s), api::log()
    PERMISSIONS WHERE $auth.admin = true;
DEFINE CONFIG OVERWRITE GRAPHQL
    TABLES INCLUDE `user`, `post`
    FUNCTIONS NONE
    DEPTH 4
    COMPLEXITY 100
    INTROSPECTION NONE;
DEFINE INDEX OVERWRITE by_account_thread ON message FIELDS account, thread_id
    COMMENT 'Inbox thread lookup';
DEFINE API OVERWRITE '/admin/*rest'
    FOR ANY MIDDLEWARE api::timeout(5s) THEN { { status: 404, body: { error: 'no such route' } } }
    FOR get, delete PERMISSIONS WHERE $auth.admin = true THEN { { status: 200 } }
    COMMENT 'Admin API with fallback';
DEFINE ACCESS OVERWRITE corp_sso ON DATABASE
    TYPE JWT URL 'https://sso.acme.example/jwks'
    DURATION FOR SESSION 8h
    COMMENT 'SSO token verification';
DEFINE TABLE OVERWRITE `user` TYPE NORMAL SCHEMAFULL COMMENT 'Application users';
DEFINE FIELD OVERWRITE created_at ON `user` TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at ON `user` TYPE datetime;
DEFINE FIELD OVERWRITE email ON `user` TYPE string
    ASSERT string::is_email($value)
    DEFAULT 'unknown@example.com';
DEFINE FIELD OVERWRITE display_name ON `user`
    COMPUTED string::uppercase(email)
    COMMENT 'Derived display name';
";

#[test]
fn check_clean_succeeds_silently() {
    let assert = cli()
        .args(["check", &fixture("clean.surdl")])
        .assert()
        .success();
    assert.get_output().stdout.is_empty();
    assert.get_output().stderr.is_empty();
}

#[test]
fn build_clean_emits_ddl_to_stdout() {
    let assert = cli()
        .args(["build", &fixture("clean.surdl")])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    assert_eq!(String::from_utf8(stdout).expect("UTF-8"), CLEAN_DDL);
}

/// The database-bearing fixture compiles end-to-end through the CLI:
/// flags, the changefeed sub-block, and the session USE line.
#[test]
fn build_app_emits_database_and_use_ns_db() {
    let assert = cli()
        .args(["build", &fixture("app.surdl")])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    assert_eq!(String::from_utf8(stdout).expect("UTF-8"), APP_DDL);
}

/// A spread naming no mixin of the document is the reference
/// diagnostic (spec/mixin.md — the resolution pass's finding) —
/// exit 1, no DDL, the code on stderr.
#[test]
fn check_mixin_broken_fails_with_the_reference_code() {
    let assert = cli()
        .args(["check", &fixture("mixin-broken.surdl")])
        .assert()
        .failure();
    assert.get_output().stdout.is_empty();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E1061"), "stderr: {stderr}");
    assert!(
        stderr.contains("no mixin of the document: `timestamped`"),
        "stderr: {stderr}"
    );
}

/// `normalize` is spread-preserving (spec/mixin.md's reverse-path
/// rule — the E1066 gate is retired): a mixin-carrying file
/// merges instead of refusing — a converged spread survives with its
/// members covered, a DIVERGED member materializes as the explicit
/// override (the overshadow rule performing the adoption), and the
/// loop is idempotent. The no-`--info` call that used to trip the
/// gate now round-trips the file's canonical form.
#[test]
fn normalize_preserves_and_materializes_spreads() {
    let sdl_path = std::env::temp_dir().join("surform-normalize-mixins.surdl");
    std::fs::write(
        &sdl_path,
        "namespace acme;\ndatabase shop;\n\
         mixin stamp {\n  created_at: datetime;\n  updated_at: datetime;\n};\n\
         table entry schemafull {\n  ...stamp;\n  title: string;\n};\n",
    )
    .expect("fixture writes");
    let converged = std::env::temp_dir().join("surform-normalize-mixins.info");
    std::fs::write(
        &converged,
        "{\n  tables: { entry: 'DEFINE TABLE entry TYPE NORMAL SCHEMAFULL' },\n  \
         \"@tables\": {\n    entry: {\n      fields: {\n        created_at: 'DEFINE FIELD \
         created_at ON entry TYPE datetime',\n        title: 'DEFINE FIELD title ON entry TYPE \
         string',\n        updated_at: 'DEFINE FIELD updated_at ON entry TYPE datetime' },\n      \
         indexes: {  },\n      events: {  }\n    }\n  }\n}\n",
    )
    .expect("fixture writes");
    // The converged state: the spread survives, its members covered.
    let assert = cli()
        .args([
            "normalize",
            &sdl_path.to_string_lossy(),
            "--info",
            &format!("db=acme/shop={}", converged.to_string_lossy()),
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout,
        "namespace acme;\n\ndatabase shop;\n\nmixin stamp {\n  field created_at: datetime;\n  \
         field updated_at: datetime;\n};\n\ntable entry {\n  schemafull;\n  ...stamp;\n  field \
         title: string;\n};\n",
        "stdout: {stdout}"
    );
    // The drifted state: `created_at` gained a default in the
    // database — it materializes as the explicit override while the
    // spread (and the converged `updated_at` under it) survives.
    let drifted = std::env::temp_dir().join("surform-normalize-mixins-drift.info");
    std::fs::write(
        &drifted,
        std::fs::read_to_string(&converged).expect("reads").replace(
            "DEFINE FIELD created_at ON entry TYPE datetime'",
            "DEFINE FIELD created_at ON entry TYPE datetime DEFAULT time::now()'",
        ),
    )
    .expect("fixture writes");
    let assert = cli()
        .args([
            "normalize",
            &sdl_path.to_string_lossy(),
            "--info",
            &format!("db=acme/shop={}", drifted.to_string_lossy()),
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout,
        "namespace acme;\n\ndatabase shop;\n\nmixin stamp {\n  field created_at: datetime;\n  \
         field updated_at: datetime;\n};\n\ntable entry {\n  schemafull;\n  ...stamp;\n  field \
         created_at: datetime {\n    default: time::now();\n  };\n  field title: string;\n};\n",
        "stdout: {stdout}"
    );
    // Idempotence on the drifted merge's own output.
    let round1 = std::env::temp_dir().join("surform-normalize-mixins-round1.surdl");
    std::fs::write(&round1, stdout.clone()).expect("fixture writes");
    let assert = cli()
        .args([
            "normalize",
            &round1.to_string_lossy(),
            "--info",
            &format!("db=acme/shop={}", drifted.to_string_lossy()),
        ])
        .assert()
        .success();
    let round2 = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(round2, stdout, "the merge loop must not churn");
    // The no-`--info` call is a usage error (issue #70): with no
    // scopes to merge, normalize would silently be a disguised `fmt`
    // — with `--in-place` it would even rewrite the file. `fmt` is
    // the no-merge path; normalize refuses at the CLI (exit 2).
    let assert = cli()
        .args(["normalize", &sdl_path.to_string_lossy()])
        .assert()
        .code(2);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("--info"), "stderr: {stderr}");
}

#[test]
fn check_broken_fails_with_the_diagnostic_code() {
    let assert = cli()
        .args(["check", &fixture("broken.surdl")])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E1003"), "stderr: {stderr}");
    assert!(stderr.contains("unknown clause `oops`"), "stderr: {stderr}");
    // The statement after the broken one is still diagnosed-for: the
    // file parsed far enough that recovery worked. Nothing else is
    // reported — one error, one statement.
    assert!(!stderr.contains("SDL-E0007"), "stderr: {stderr}");
}

#[test]
fn build_broken_emits_no_ddl() {
    let assert = cli()
        .args(["build", &fixture("broken.surdl")])
        .assert()
        .failure();
    assert.get_output().stdout.is_empty();
}

#[test]
fn json_flag_renders_one_object_per_diagnostic() {
    let assert = cli()
        .args(["--json", "check", &fixture("broken.surdl")])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("\"code\": \"SDL-E1003\""),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("\"url\""), "stderr: {stderr}");
}

#[test]
fn missing_file_is_a_friendly_diagnostic() {
    let assert = cli()
        .args(["check", "/nonexistent/nope.surdl"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E0008"), "stderr: {stderr}");
    assert!(stderr.contains("cannot read"), "stderr: {stderr}");
    assert!(stderr.contains("the file must exist"), "stderr: {stderr}");
}

/// A DIRECTORY argument is its own E0008 diagnostic (issue #90): the
/// generic help ("the file must exist") is wrong twice over for a
/// path that plainly exists — the kind branch names the directory.
#[test]
fn a_directory_argument_names_itself() {
    let dir = std::env::temp_dir().join("surform-directory-arg");
    std::fs::create_dir_all(&dir).expect("the dir writes");
    for cmd in ["check", "build", "fmt", "lint"] {
        let assert = cli().args([cmd, dir.to_str().unwrap()]).assert().failure();
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
        assert!(stderr.contains("SDL-E0008"), "{cmd}: {stderr}");
        assert!(stderr.contains("it is a directory"), "{cmd}: {stderr}");
        assert!(
            stderr.contains("pass the schema file's path"),
            "{cmd}: {stderr}"
        );
    }
    // The migrate file reader branches the same way.
    let assert = cli()
        .args(["migrate", "commit", dir.to_str().unwrap(), "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("it is a directory"), "{stderr}");
}

/// The silo module builds through the CLI (the one executable shape
/// the `app.surdl` batch doesn't carry): the DDL prints to stdout,
/// self-named (no alias).
#[test]
fn silo_module_builds() {
    let assert = cli()
        .args(["build", &fixture("silo-module.surdl")])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout,
        "DEFINE MODULE OVERWRITE silo::acme::utils::<1.2.0> UNSIGNED;\n"
    );
}

// ---- raw ----

/// The raw statement's DDL reaches stdout verbatim — comments and
/// interior spelling/indentation included; only the edges trim.
#[test]
fn build_emits_raw_verbatim() {
    let assert = cli()
        .args(["build", &fixture("raw.surdl")])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout,
        "// interior comments ride along\n  DEFINE PARAM OVERWRITE $smoke VALUE 1;\n"
    );
}

// ---- `fmt` ----

const FMT_INPUT: &str = "\
/// Acme Corporation
ns acme;
db app { strict; changefeed { expiry: 7d; include original; }; };
user deployer { on: kv; passhash: '$argon2id$v=19$'; roles: editor, viewer; };
table `user` { schemafull; field email: string { assert: string::is_email($value); }; };
";

const FMT_OUTPUT: &str = "\
/// Acme Corporation
ns acme;

db app {
  strict;
  changefeed {
    expiry: 1w;
    include original;
  };
};

user deployer {
  on: kv;
  passhash: '$argon2id$v=19$';
  roles: editor, viewer;
};

table `user` {
  schemafull;
  field email: string {
    assert: string::is_email($value);
  };
};
";

/// `fmt` writes the canonical SDL to stdout: the simple statements in
/// source order, doc comments carried, assignment short forms kept.
#[test]
fn fmt_writes_canonical_sdl_to_stdout() {
    let assert = cli()
        .args(["fmt", &fixture("clean.surdl")])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout,
        "// The spec's namespace, sequence, and param examples, in one clean file.\n/// Acme Corporation\nnamespace acme;\n\nnamespace staging;\n\nsequence invoice_number = 1000;\n\n/// Default page size for list endpoints\nparam $default_limit = 50;\n",
        "stdout: {stdout}"
    );
}

/// The full-style fixture through stdout, aliases and all.
#[test]
fn fmt_carries_aliases_and_canonicalizes_layout() {
    let path = std::env::temp_dir().join("surform-fmt-stdout.surdl");
    std::fs::write(&path, FMT_INPUT).expect("fixture writes");
    let assert = cli()
        .args(["fmt", path.to_string_lossy().as_ref()])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(stdout, FMT_OUTPUT);
}

/// `--max-width` narrows the layout budget: a kind union that fits at
/// the default rides one line and breaks under a narrow budget, and
/// the narrow output is still fmt-stable at its own width.
#[test]
fn fmt_max_width_governs_the_layout_budget() {
    let input = "\
table t {
  field choice: 'alpha' | 'beta' | 'gamma' | 'delta';
};
";
    let path = std::env::temp_dir().join("surform-fmt-max-width.surdl");
    std::fs::write(&path, input).expect("fixture writes");
    let assert = cli()
        .args(["fmt", path.to_string_lossy().as_ref()])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("field choice: 'alpha' | 'beta' | 'gamma' | 'delta';"),
        "fits the default budget: {stdout}"
    );
    let assert = cli()
        .args(["fmt", path.to_string_lossy().as_ref(), "--max-width", "40"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("'alpha'\n    | 'beta'\n    | 'gamma'\n    | 'delta';"),
        "breaks under 40: {stdout}"
    );
    // The narrow output is a fixed point at its own width.
    let narrow = std::env::temp_dir().join("surform-fmt-max-width-40.surdl");
    std::fs::write(&narrow, &stdout).expect("fixture writes");
    let assert = cli()
        .args([
            "fmt",
            narrow.to_string_lossy().as_ref(),
            "--max-width",
            "40",
        ])
        .assert()
        .success();
    let stdout2 = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(stdout2, stdout, "fmt∘fmt must be fmt at 40");
    let assert = cli()
        .args(["fmt", path.to_string_lossy().as_ref(), "--max-width", "120"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("field choice: 'alpha' | 'beta' | 'gamma' | 'delta';"),
        "fits a wide budget: {stdout}"
    );
    // Out of range: a clap error, not a panic.
    cli()
        .args(["fmt", path.to_string_lossy().as_ref(), "--max-width", "3"])
        .assert()
        .failure();
}

/// `normalize` takes the same flag: the re-emitted file breaks under
/// the narrowed budget (no `--info` scopes with content — the merge
/// is identity — so the writer is the only variable; the required
/// tag rides an empty root interchange, issue #70).
#[test]
fn normalize_max_width_governs_the_layout_budget() {
    let path = std::env::temp_dir().join("surform-normalize-max-width.surdl");
    std::fs::write(
        &path,
        "table t {\n  field choice: 'alpha' | 'beta' | 'gamma' | 'delta';\n};\n",
    )
    .expect("fixture writes");
    let empty = std::env::temp_dir().join("surform-normalize-max-width.info");
    std::fs::write(&empty, "{}\n").expect("fixture writes");
    let assert = cli()
        .args([
            "normalize",
            path.to_string_lossy().as_ref(),
            "--info",
            &format!("root={}", empty.to_string_lossy()),
            "--max-width",
            "40",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("'alpha'\n    | 'beta'\n    | 'gamma'\n    | 'delta';"),
        "breaks under 40: {stdout}"
    );
}

/// The issue repro: an fn body's module-qualified call keeps the
/// glued spelling through fmt — upstream's `Function::Module` print
/// arm emits `mod:: name::fn(…)` (the engine's own INFO print
/// carries the space), and the canon repairs it instead of
/// reproducing it. `fn::` paths never carried the quirk.
#[test]
fn fmt_keeps_module_calls_glued() {
    let input = "\
namespace x;

database x;

fn probe() {
  LET $a = fn::inventory::thumbnail_key(f\"attachments:/a.jpg\");
  RETURN mod::thumbnail::render(b\"00\", 512)
};
";
    let path = std::env::temp_dir().join("surform-fmt-module-call.surdl");
    std::fs::write(&path, input).expect("fixture writes");
    let assert = cli()
        .args(["fmt", path.to_string_lossy().as_ref()])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("RETURN mod::thumbnail::render(b\"00\", 512);"),
        "stdout: {stdout}"
    );
    assert!(
        !stdout.contains("mod:: "),
        "the quirk space survived: {stdout}"
    );
    // And the repaired form is fmt-stable.
    let second = std::env::temp_dir().join("surform-fmt-module-call-2.surdl");
    std::fs::write(&second, &stdout).expect("fixture writes");
    let assert = cli()
        .args(["fmt", second.to_string_lossy().as_ref()])
        .assert()
        .success();
    let stdout2 = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(stdout2, stdout, "fmt∘fmt must be fmt");
}

/// `--in-place` rewrites the file, and a second run is a no-op
/// (fmt∘fmt = fmt).
#[test]
fn fmt_in_place_is_idempotent() {
    let path = std::env::temp_dir().join("surform-fmt-inplace.surdl");
    std::fs::write(&path, FMT_INPUT).expect("fixture writes");
    cli()
        .args(["fmt", "--in-place", path.to_string_lossy().as_ref()])
        .assert()
        .success();
    let once = std::fs::read_to_string(&path).expect("reads");
    assert_eq!(once, FMT_OUTPUT);
    cli()
        .args(["fmt", "--in-place", path.to_string_lossy().as_ref()])
        .assert()
        .success();
    let twice = std::fs::read_to_string(&path).expect("reads");
    assert_eq!(twice, once, "the second run must not churn");
}

/// A dirty parse is never formatted nor rewritten: the diagnostics
/// report, the exit is 1, and the file keeps its bytes.
#[test]
fn fmt_refuses_a_dirty_parse() {
    let before = std::fs::read_to_string(fixture("broken.surdl")).expect("reads");
    let assert = cli()
        .args(["fmt", &fixture("broken.surdl")])
        .assert()
        .failure();
    assert.get_output().stdout.is_empty();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E1003"), "stderr: {stderr}");
    assert_eq!(
        std::fs::read_to_string(fixture("broken.surdl")).expect("reads"),
        before,
        "the file must be untouched"
    );
}

/// A missing file diagnoses like `check` does.
#[test]
fn fmt_missing_file_is_a_friendly_diagnostic() {
    let assert = cli()
        .args(["fmt", "/nonexistent/nope.surdl"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E0008"), "stderr: {stderr}");
}

// ---- fmt stdin: the editor pipe ----

/// No file means stdin in, canonical SDL out — byte-identical to the
/// same text through a file. This is the pipe the VS Code formatter
/// (and any editor integration) drives with an unsaved buffer.
#[test]
fn fmt_reads_stdin_when_no_file_is_given() {
    let assert = cli().arg("fmt").write_stdin(FMT_INPUT).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(stdout, FMT_OUTPUT);
}

/// `-` names the same pipe.
#[test]
fn fmt_dash_reads_stdin_too() {
    let assert = cli()
        .args(["fmt", "-"])
        .write_stdin(FMT_INPUT)
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(stdout, FMT_OUTPUT);
}

/// A dirty buffer is diagnosed, never formatted — the editor surfaces
/// the JSON diagnostics instead of mangling the document. The same
/// broken bytes the file-mode test feeds, over the pipe.
#[test]
fn fmt_stdin_refuses_a_dirty_parse() {
    let broken = std::fs::read_to_string(fixture("broken.surdl")).expect("reads");
    let assert = cli()
        .args(["--json", "fmt"])
        .write_stdin(broken)
        .assert()
        .failure();
    assert.get_output().stdout.is_empty();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("\"code\": \"SDL-E1003\""),
        "stderr: {stderr}"
    );
}

/// `--in-place` without a file is a usage error: clap names the
/// missing path before any stdin is read.
#[test]
fn fmt_in_place_without_a_file_is_a_usage_error() {
    let assert = cli()
        .args(["fmt", "--in-place"])
        .write_stdin(FMT_INPUT)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("<FILE>"), "stderr: {stderr}");
}

/// `--in-place -` cannot rewrite the pipe: a named conflict, never a
/// surprise file called `-`.
#[test]
fn fmt_in_place_dash_is_a_named_conflict() {
    let assert = cli()
        .args(["fmt", "--in-place", "-"])
        .write_stdin(FMT_INPUT)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("cannot rewrite stdin"), "stderr: {stderr}");
    assert!(
        !std::path::Path::new("-").exists(),
        "no file named `-` may appear"
    );
}

// ---- fmt --check: the CI gate ----

/// A canonical input is silent success — the check convention's
/// clean run: exit 0, nothing on stdout, nothing on stderr.
#[test]
fn fmt_check_canonical_input_is_silent() {
    let path = std::env::temp_dir().join("surform-fmt-check-clean.surdl");
    std::fs::write(&path, FMT_OUTPUT).expect("fixture writes");
    let assert = cli()
        .args(["fmt", "--check", path.to_string_lossy().as_ref()])
        .assert()
        .success();
    assert!(assert.get_output().stdout.is_empty(), "stdout: silent");
    assert!(assert.get_output().stderr.is_empty(), "stderr: silent");
}

/// A non-canonical input exits 1 and renders what would change as a
/// diff on stderr — finding-class output (stdout stays empty), with
/// the `--max-width` budget composing into the verdict.
#[test]
fn fmt_check_non_canonical_exits_1_with_a_diff() {
    let path = std::env::temp_dir().join("surform-fmt-check-messy.surdl");
    std::fs::write(&path, FMT_INPUT).expect("fixture writes");
    let assert = cli()
        .args(["fmt", "--check", path.to_string_lossy().as_ref()])
        .assert()
        .failure();
    assert!(assert.get_output().stdout.is_empty(), "stdout: empty");
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    for marker in [
        "--- ",
        "+++ ",
        "@@ -1,5 +1,23 @@",
        "\n-db app {",
        "\n+db app {",
    ] {
        assert!(stderr.contains(marker), "missing `{marker}` in:\n{stderr}");
    }
    // The same file canonical at the default budget is NOT canonical
    // under a narrow one — the budget is part of the verdict.
    let union = "\
table t {
  field choice: 'alpha' | 'beta' | 'gamma' | 'delta';
};
";
    let path = std::env::temp_dir().join("surform-fmt-check-width.surdl");
    std::fs::write(&path, union).expect("fixture writes");
    cli()
        .args(["fmt", "--check", path.to_string_lossy().as_ref()])
        .assert()
        .success();
    cli()
        .args([
            "fmt",
            "--check",
            path.to_string_lossy().as_ref(),
            "--max-width",
            "40",
        ])
        .assert()
        .failure();
}

/// The pipe is an input to `--check` too: a canonical buffer is
/// silent success, a dirty one exits 1 with the diff.
#[test]
fn fmt_check_reads_stdin() {
    let assert = cli()
        .args(["fmt", "--check"])
        .write_stdin(FMT_OUTPUT)
        .assert()
        .success();
    assert!(assert.get_output().stdout.is_empty(), "stdout: silent");
    assert!(assert.get_output().stderr.is_empty(), "stderr: silent");
    let assert = cli()
        .args(["fmt", "--check"])
        .write_stdin(FMT_INPUT)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("@@"), "stderr: {stderr}");
}

/// `--check` and `--in-place` cannot ride together: one refuses to
/// write, the other's whole job is writing.
#[test]
fn fmt_check_and_in_place_conflict() {
    let assert = cli()
        .args(["fmt", "--check", "--in-place", "x.surdl"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("cannot be used with"), "stderr: {stderr}");
}

// ---- fmt refuses migration scripts ----

/// A generated migration script (the draft header
/// `-- surform migration NNNN — <name>`) is refused with ONE clear
/// diagnostic — not the SDL-E1001/E1002 wall the parse would raise
/// (`--` is SurrealQL comment syntax, not SDL's). The generator is
/// the only formatter migration files will ever have; the schema to
/// format is the header's `-- source:` file.
#[test]
fn fmt_refuses_a_migration_script_file() {
    let path = std::env::temp_dir().join("surform-fmt-migration.surql");
    std::fs::write(&path, MIGRATION_DRAFT).expect("fixture writes");
    for args in [
        vec!["fmt"],
        vec!["fmt", "--check"],
        vec!["fmt", "--in-place"],
    ] {
        let assert = cli()
            .args(args)
            .arg(path.to_string_lossy().as_ref())
            .assert()
            .failure();
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
        assert!(stderr.contains("SDL-E0017"), "stderr: {stderr}");
        assert!(
            !stderr.contains("SDL-E1001") && !stderr.contains("SDL-E1002"),
            "the wall survived: {stderr}"
        );
        assert_eq!(
            stderr.matches("SDL-E0017").count(),
            1,
            "one diagnostic, not a wall: {stderr}"
        );
    }
    // The refusal never writes: `--in-place` leaves the bytes alone.
    assert_eq!(
        std::fs::read_to_string(&path).expect("reads"),
        MIGRATION_DRAFT,
        "the migration file must be untouched"
    );
}

/// The same refusal rides the pipe — the editor contract reads stdin.
#[test]
fn fmt_refuses_a_migration_script_on_stdin() {
    let assert = cli()
        .arg("fmt")
        .write_stdin(MIGRATION_DRAFT)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E0017"), "stderr: {stderr}");
    assert!(stderr.contains("<stdin>"), "stderr: {stderr}");
}

/// The detector anchors on the exact `-- surform migration ` prefix:
/// a normal SDL file whose FIRST line is a `//` comment — even one
/// naming surform migrations — formats as ever.
#[test]
fn fmt_formats_a_comment_led_first_line() {
    let input = "// surform migration 0001 — a decoy comment\nns  acme;\n";
    let assert = cli().arg("fmt").write_stdin(input).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout,
        "// surform migration 0001 — a decoy comment\nns acme;\n"
    );
}

const MIGRATION_DRAFT: &str = "\
-- surform migration 0001 — add-users
-- generated: 2026-10-07T18:04:05Z
-- source: schema.surdl
-- input: info db=acme/shop=info-db.txt
-- status: draft

BEGIN TRANSACTION;
DEFINE TABLE OVERWRITE `user`;
COMMIT;
";

// ---------------------------------------------------------------------
// `surform normalize`: the merge + canonical placement CLI
// ---------------------------------------------------------------------

/// The SDL and its database interchange (INFO FOR DB plus the reserved
/// `"@tables"` child map), written to fresh temp paths per test.
fn normalize_fixture(tag: &str) -> (String, String) {
    let sdl_path = std::env::temp_dir().join(format!("surform-normalize-{tag}.surdl"));
    let info_path = std::env::temp_dir().join(format!("surform-normalize-{tag}.info"));
    std::fs::write(
        &sdl_path,
        "namespace acme;\ndatabase shop;\ntable `user` {\n  schemaless;\n};\n",
    )
    .expect("fixture writes");
    std::fs::write(
        &info_path,
        "{\n  tables: { user: 'DEFINE TABLE user TYPE NORMAL SCHEMALESS \
         PERMISSIONS FULL' },\n  params: { limit: 'DEFINE PARAM $limit VALUE 10 \
         PERMISSIONS FULL' },\n  \"@tables\": {\n    user: {\n      fields: { \
         email: 'DEFINE FIELD email ON user TYPE string PERMISSIONS FULL' },\n      \
         indexes: {  },\n      events: {  }\n    }\n  }\n}\n",
    )
    .expect("fixture writes");
    (
        sdl_path.to_string_lossy().into_owned(),
        info_path.to_string_lossy().into_owned(),
    )
}

/// The merge adopts the db state, nests the child field, and places
/// the no-dependency param at the db section's earliest slot — ahead
/// of the table (the topological rule, not the interim tail append).
#[test]
fn normalize_merges_the_interchange_onto_the_file() {
    let (sdl, info) = normalize_fixture("stdout");
    let assert = cli()
        .args(["normalize", &sdl, "--info", &format!("db=acme/shop={info}")])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout,
        "namespace acme;\n\ndatabase shop;\n\nparam $limit = 10;\n\ntable `user` {\n  \
         schemaless;\n  permissions: full;\n  field email: string;\n};\n",
        "stdout: {stdout}"
    );
}

/// `--in-place` rewrites, and a second run is a no-op.
#[test]
fn normalize_in_place_is_idempotent() {
    let (sdl, info) = normalize_fixture("inplace");
    cli()
        .args([
            "normalize",
            "--in-place",
            &sdl,
            "--info",
            &format!("db=acme/shop={info}"),
        ])
        .assert()
        .success();
    let once = std::fs::read_to_string(&sdl).expect("reads");
    cli()
        .args([
            "normalize",
            "--in-place",
            &sdl,
            "--info",
            &format!("db=acme/shop={info}"),
        ])
        .assert()
        .success();
    let twice = std::fs::read_to_string(&sdl).expect("reads");
    assert_eq!(twice, once, "the second run must not churn");
}

/// A tag naming a scope the SDL never declares is the scope-mismatch
/// family: E3005, exit 1, clean stdout. The generic help IS this
/// class's fix (mirror the file's own declarations) — it stays.
#[test]
fn normalize_rejects_an_undeclared_scope() {
    let (sdl, info) = normalize_fixture("undeclared");
    let assert = cli()
        .args([
            "normalize",
            &sdl,
            "--info",
            &format!("db=other/shop={info}"),
        ])
        .assert()
        .failure();
    assert.get_output().stdout.is_empty();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E3005"), "stderr: {stderr}");
    assert!(
        stderr.contains("scope tags must mirror"),
        "stderr: {stderr}"
    );
}

/// A malformed spec text (no tag prefix) diagnoses E3005 — with the
/// malformed class's own help (issue #90: spelling, not declarations).
#[test]
fn normalize_rejects_a_malformed_tag() {
    let (sdl, info) = normalize_fixture("malformed");
    let assert = cli()
        .args(["normalize", &sdl, "--info", &info])
        .assert()
        .failure();
    assert.get_output().stdout.is_empty();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E3005"), "stderr: {stderr}");
    assert!(stderr.contains("spell the tag"), "stderr: {stderr}");
}

/// The same scope twice is ambiguous input: E3005, exit 1 — the
/// duplicate class's help says DELETE the repeat (issue #90).
#[test]
fn normalize_rejects_duplicate_scope_tags() {
    let (sdl, info) = normalize_fixture("duplicate");
    let spec = format!("db=acme/shop={info}");
    let assert = cli()
        .args(["normalize", &sdl, "--info", &spec, "--info", &spec])
        .assert()
        .failure();
    assert.get_output().stdout.is_empty();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E3005"), "stderr: {stderr}");
    assert!(
        stderr.contains("delete the repeated `--info` tag"),
        "stderr: {stderr}"
    );
}

/// `normalize` without `--info` is a clap usage error (issue #70),
/// not a disguised `fmt`: the merge has nothing to adopt, and with
/// `--in-place` the disguised run would even rewrite the file.
#[test]
fn normalize_without_info_is_a_usage_error() {
    let (sdl, _) = normalize_fixture("no-info");
    let assert = cli().args(["normalize", &sdl]).assert().code(2);
    assert.get_output().stdout.is_empty();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("required arguments were not provided"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("--info <SPEC>"), "stderr: {stderr}");
}

/// A pathological interchange nests past the object walk's depth
/// budget: the well-formed E3004 reports (the process exits 1 — no
/// stack-overflow abort).
#[test]
fn normalize_survives_a_depth_bomb() {
    let (sdl, _) = normalize_fixture("depth");
    let info = std::env::temp_dir().join("surform-normalize-depth.info");
    let mut text = "{a:".repeat(200);
    text.push('1');
    text.push_str(&"}".repeat(200));
    std::fs::write(&info, text).expect("fixture writes");
    let assert = cli()
        .args([
            "normalize",
            &sdl,
            "--info",
            &format!("db=acme/shop={}", info.to_string_lossy()),
        ])
        .assert()
        .failure();
    assert.get_output().stdout.is_empty();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E3004"), "stderr: {stderr}");
}

// ---------------------------------------------------------------------
// `surform migrate generate`: the drift report +
// convergence script CLI
// ---------------------------------------------------------------------

/// The SDL and its interchanges, written to fresh temp paths per
/// test. All three scopes are given — the migration covers exactly
/// the ingested scopes, so without a root/ns tag the authored
/// namespace and database would have no read entries to match and
/// would report apply-pending (defining them). The `stale` table
/// exists only in the interchange; `pending` only in the SDL.
struct MigrateFixture {
    sdl: String,
    root: String,
    ns: String,
    info: String,
}

fn migrate_fixture(tag: &str, stale: bool, pending: bool) -> MigrateFixture {
    migrate_fixture_with_field(tag, stale, pending, false)
}

/// The field variant: a field `old` the database holds on `user` and
/// the SDL never declares — a DatabaseOnly child (the host table
/// matches), the field-slot class.
fn migrate_fixture_with_field(
    tag: &str,
    stale: bool,
    pending: bool,
    field: bool,
) -> MigrateFixture {
    let sdl_path = std::env::temp_dir().join(format!("surform-migrate-{tag}.surdl"));
    let root_path = std::env::temp_dir().join(format!("surform-migrate-{tag}.root"));
    let ns_path = std::env::temp_dir().join(format!("surform-migrate-{tag}.ns"));
    let info_path = std::env::temp_dir().join(format!("surform-migrate-{tag}.info"));
    let mut sdl =
        "namespace acme;\ndatabase shop;\ntable `user` {\n  schemaless;\n};\n".to_string();
    if pending {
        sdl.push_str("table pending;\n");
    }
    std::fs::write(&sdl_path, sdl).expect("fixture writes");
    std::fs::write(
        &root_path,
        "{ namespaces: { acme: 'DEFINE NAMESPACE acme' } }\n",
    )
    .expect("fixture writes");
    std::fs::write(
        &ns_path,
        "{ databases: { shop: 'DEFINE DATABASE shop' } }\n",
    )
    .expect("fixture writes");
    // The hand-fed DDL mimics the LIVE printer's shapes (the
    // vacuous-fixture lesson): a plain table prints no PERMISSIONS
    // clause at all — an explicit `PERMISSIONS FULL` is stored state
    // and would rightly drift against a table with none.
    let mut tables = "user: 'DEFINE TABLE user TYPE NORMAL SCHEMALESS'".to_string();
    let fields = if field {
        "old: 'DEFINE FIELD old ON user TYPE int' "
    } else {
        " "
    };
    let mut children =
        format!("user: {{ fields: {{ {fields}}}, indexes: {{  }}, events: {{  }} }}");
    if stale {
        tables.push_str(", stale: 'DEFINE TABLE stale TYPE NORMAL SCHEMALESS'");
        children.push_str(", stale: { fields: {  }, indexes: {  }, events: {  } }");
    }
    std::fs::write(
        &info_path,
        format!("{{\n  tables: {{ {tables} }},\n  \"@tables\": {{ {children} }}\n}}\n"),
    )
    .expect("fixture writes");
    let p = |path: &std::path::PathBuf| path.to_string_lossy().into_owned();
    MigrateFixture {
        sdl: p(&sdl_path),
        root: p(&root_path),
        ns: p(&ns_path),
        info: p(&info_path),
    }
}

fn migrate_args(fixture: &MigrateFixture) -> Vec<String> {
    let mut args = vec![
        "migrate".to_string(),
        "generate".to_string(),
        fixture.sdl.clone(),
    ];
    for spec in [
        format!("root={}", fixture.root),
        format!("ns=acme={}", fixture.ns),
        format!("db=acme/shop={}", fixture.info),
    ] {
        args.push("--info".to_string());
        args.push(spec);
    }
    args
}

/// The converged script's fixed frame: the header (interpolated from
/// the lib's own constants — the header bytes live in ONE place), the
/// run's summary line, the scope's `USE` above the one frame, its
/// `## Scope` header block, the body inside it. The drift legs assert
/// around it.
fn script_frame(summary: &str, section: &str, use_line: &str, body: &str) -> String {
    format!(
        "== migration script ==\n\
         {}\
         {summary}\
         {use_line}\
         \n\
         {}\n\
         -- ## Scope db:acme/shop\n\
         {section}\
         \n\
         BEGIN TRANSACTION;\n\
         \n\
         {body}\
         \n\
         COMMIT;\n",
        surform::info::script::HEADER,
        surform::info::script::RULE,
    )
}

/// The run's wrapped summary line (the lib's one spelling, the
/// header's trailing blank included).
fn script_summary(findings: usize, defines: usize, removals: usize, slots: usize) -> String {
    format!(
        "{}\n",
        surform::info::script::summary_block(findings, defines, removals, slots, 0, 1, 0)
    )
}

/// Drift prints the report (the database-only sigil) and the
/// convergence script section — the removal carrying its WARNING
/// anchor — and exits 1.
#[test]
fn migrate_reports_drift_and_exits_1() {
    let fixture = migrate_fixture("drift", true, false);
    let assert = cli().args(migrate_args(&fixture)).assert().failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    let expected = format!(
        "== drift ==\n\
         1 finding - 0 defines, 1 removal, 1 data-migration slot, 0 staged tables\n\
         \u{2212} db:acme/shop table stale (required data-migration slot)\n\
         \n\
         == required data-migration slots ==\n\
         \x20 db:acme/shop table stale - the removal deletes the table and every \
         document in it; the script carries the fill-in slot above the removal; \
         when the script removes a whole namespace or database instead, the \
         WARNING above that REMOVE names this finding\n\
         {}",
        script_frame(
            &script_summary(1, 0, 1, 1),
            "-- section: db:acme/shop (0 defines, 1 removal)\n",
            "USE NS acme DB shop;\n",
            &format!(
                "{}\n\
                 -- ## Removals - objects the schema file no longer declares\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 -- >>> SURFORM WARNING: DATA LOSS <<<\n\
                 -- Removes table stale - every document in it is deleted.\n\
                 \n\
                 -- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n\
                 -- - Write the export, archive, or move query between the\n\
                 --   markers; it runs before the removal, in this same\n\
                 --   transaction.\n\
                 -- e.g. INSERT INTO archive (SELECT * FROM stale);\n\
                 -- >>> SURFORM REQUIRED: END <<<\n\
                 REMOVE TABLE IF EXISTS stale;\n",
                surform::info::script::RULE,
            ),
        )
    );
    assert_eq!(stdout, expected, "stdout: {stdout}");
}

/// An SDL-only definition defines (the apply-pending direction now
/// lands in the script as `DEFINE … OVERWRITE`), still exit 1.
#[test]
fn migrate_defines_the_sdl_only_side() {
    let fixture = migrate_fixture("pending", false, true);
    let assert = cli().args(migrate_args(&fixture)).assert().failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("+ db:acme/shop table pending (apply-pending)\n"),
        "stdout: {stdout}"
    );
    let expected = format!(
        "== drift ==\n\
         1 finding - 1 define, 0 removals, 0 data-migration slots, 0 staged tables\n\
         + db:acme/shop table pending (apply-pending)\n\
         {}",
        script_frame(
            &script_summary(1, 1, 0, 0),
            "-- section: db:acme/shop (1 define, 0 removals)\n",
            "USE NS acme DB shop;\n",
            &format!(
                "{}\n\
                 -- ## Defines - the state the schema file describes\n\
                 --\n\
                 -- Each typed define carries OVERWRITE: a re-run redefines it.\n\
                 \n\
                 DEFINE TABLE OVERWRITE pending TYPE NORMAL SCHEMALESS;\n",
                surform::info::script::RULE,
            ),
        )
    );
    assert_eq!(stdout, expected, "stdout: {stdout}");
}

/// A converged state is ONE `(converged)` line (the two banners said
/// one bit), exit 0.
#[test]
fn migrate_converged_exits_0() {
    let fixture = migrate_fixture("clean", false, false);
    let assert = cli().args(migrate_args(&fixture)).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(stdout, "(converged)\n", "stdout: {stdout}");
}

/// `--check` is report-only: the drift report — the slot manifest
/// included, so CI sees the unfilled data-migration obligations
/// without the script — prints, the script section never does, and
/// drift still exits 1 (the CI gate).
#[test]
fn migrate_check_is_report_only() {
    let fixture = migrate_fixture("check", true, false);
    let mut args = migrate_args(&fixture);
    args.push("--check".to_string());
    let assert = cli().args(args).assert().failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout,
        "== drift ==\n\
         1 finding - 0 defines, 1 removal, 1 data-migration slot, 0 staged tables\n\
         \u{2212} db:acme/shop table stale (required data-migration slot)\n\
         \n\
         == required data-migration slots ==\n\
         \x20 db:acme/shop table stale - the removal deletes the table and every \
         document in it; the script carries the fill-in slot above the removal; \
         when the script removes a whole namespace or database instead, the \
         WARNING above that REMOVE names this finding\n",
        "stdout: {stdout}"
    );
    // Converged --check exits 0, report only.
    let fixture = migrate_fixture("checkclean", false, false);
    let mut args = migrate_args(&fixture);
    args.push("--check".to_string());
    let assert = cli().args(args).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(stdout, "(converged)\n", "stdout: {stdout}");
}

/// Tag errors are the normalize pipeline's: they exit before any
/// diffing (the stdout stays header-free). The no-tags spelling is
/// the bare-FILE baseline mode now (issue #101 — its own suite in
/// tests/migrate.rs); the malformed, undeclared-scope, and duplicate
/// tag spellings still refuse with SDL-E3005.
#[test]
fn migrate_tag_errors_precede_diffing() {
    let fixture = migrate_fixture("tags", false, false);
    // Malformed.
    let assert = cli()
        .args(["migrate", "generate", &fixture.sdl, "--info", "bogus"])
        .assert()
        .failure();
    assert!(assert.get_output().stdout.is_empty());
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E3005"), "stderr: {stderr}");
    // Undeclared scope.
    let assert = cli()
        .args([
            "migrate",
            "generate",
            &fixture.sdl,
            "--info",
            &format!("ns=other={}", fixture.ns),
        ])
        .assert()
        .failure();
    assert!(assert.get_output().stdout.is_empty());
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E3005"), "stderr: {stderr}");
    // Duplicate.
    let tag = format!("db=acme/shop={}", fixture.info);
    let assert = cli()
        .args([
            "migrate",
            "generate",
            &fixture.sdl,
            "--info",
            &tag,
            "--info",
            &tag,
        ])
        .assert()
        .failure();
    assert!(assert.get_output().stdout.is_empty());
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E3005"), "stderr: {stderr}");
}

/// The slots ride the CLI output: a database-only field (its host
/// table matches) and a database-only table both mark their report
/// rows, list in the manifest, and carry their slot blocks in the
/// script; `--check` shows the manifest without the script.
#[test]
fn migrate_slots_print_in_script_report_and_check() {
    let fixture = migrate_fixture_with_field("slots", true, false, true);
    let assert = cli().args(migrate_args(&fixture)).assert().failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    // The report rows carry the slot marker; the manifest lists both
    // obligations (field first — the (scope, family, key) sort).
    assert!(
        stdout.contains("\u{2212} db:acme/shop field old ON user (required data-migration slot)\n"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("== required data-migration slots ==\n"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains(
            "  db:acme/shop field old ON user - the removal deletes the field \
             definition only; the values remain on the rows; the script carries \
             the fill-in slot above the removal; when the script removes a whole \
             namespace or database instead, the WARNING above that REMOVE names \
             this finding\n"
        ),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains(
            "  db:acme/shop table stale - the removal deletes the table and every \
             document in it; the script carries the fill-in slot above the \
             removal; when the script removes a whole namespace or database \
             instead, the WARNING above that REMOVE names this finding\n"
        ),
        "stdout: {stdout}"
    );
    // The script carries both slot blocks — the field's alone (no
    // alarm), the table's under its WARNING.
    assert!(
        stdout
            .matches(">>> SURFORM REQUIRED: DATA MIGRATION <<<")
            .count()
            == 2
    );
    assert!(
        stdout.contains(
            "-- Removes field old on table user. The stored values remain\n\
             --    on the rows, outside every field definition.\n"
        ),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("-- e.g. UPDATE `user` SET moved = old;\n")
            && stdout.contains("--   copy is verified (e.g. UPDATE `user` UNSET old;).\n"),
        "stdout: {stdout}"
    );
    let field_slot_at = stdout.find(">>> SURFORM REQUIRED").expect("the field slot");
    let field_remove_at = stdout
        .find("REMOVE FIELD IF EXISTS old ON `user`;")
        .expect("remove");
    let warning_at = stdout.find(">>> SURFORM WARNING").expect("the alarm");
    let table_slot_at = stdout
        .rfind(">>> SURFORM REQUIRED: DATA MIGRATION")
        .expect("table slot");
    let table_remove_at = stdout
        .find("REMOVE TABLE IF EXISTS stale;")
        .expect("remove");
    assert!(
        field_slot_at < field_remove_at
            && field_remove_at < warning_at
            && warning_at < table_slot_at
            && table_slot_at < table_remove_at,
        "field slot, field removal, then the table's alarm + slot + removal"
    );
    // --check: the manifest without the script.
    let fixture = migrate_fixture_with_field("slotscheck", true, false, true);
    let mut args = migrate_args(&fixture);
    args.push("--check".to_string());
    let assert = cli().args(args).assert().failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("== required data-migration slots =="),
        "{stdout}"
    );
    assert!(!stdout.contains(">>> SURFORM"), "report only: {stdout}");
}

/// The auto-seeded fingerprint namespace is expected-unmanaged: it
/// prints, never REMOVEs, and never trips the exit.
#[test]
fn migrate_expected_unmanaged_is_informational() {
    let fixture = migrate_fixture("unmanaged", false, false);
    std::fs::write(
        &fixture.root,
        "{ namespaces: { acme: 'DEFINE NAMESPACE acme', other: 'DEFINE NAMESPACE \
         other COMMENT \\'Default namespace generated by SurrealDB\\'' } }\n",
    )
    .expect("fixture writes");
    let assert = cli().args(migrate_args(&fixture)).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("\u{b7} root namespace other (expected-unmanaged"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("(converged)"), "stdout: {stdout}");
}

// ---------------------------------------------------------------------
// `surform migrate generate --from/--to`: the
// version pair — schema version against schema version
// ---------------------------------------------------------------------

/// The version-pair fixture: two SDL files at fresh temp paths per
/// tag. `v1` holds the older state (a stale param, a `legacy` table),
/// `v2` the newer one (the param redefined, `legacy` dropped,
/// `archive` added).
fn version_pair_fixture(tag: &str) -> (String, String) {
    let from_path = std::env::temp_dir().join(format!("surform-migrate-{tag}-from.surdl"));
    let to_path = std::env::temp_dir().join(format!("surform-migrate-{tag}-to.surdl"));
    std::fs::write(
        &from_path,
        "namespace acme;\ndatabase shop;\nparam $p = 1;\ntable legacy;\n",
    )
    .expect("fixture writes");
    std::fs::write(
        &to_path,
        "namespace acme;\ndatabase shop;\nparam $p = 2;\ntable archive;\n",
    )
    .expect("fixture writes");
    let p = |path: &std::path::PathBuf| path.to_string_lossy().into_owned();
    (p(&from_path), p(&to_path))
}

/// The version-pair mode generates the same report + script shape the
/// `--info` mode does (one comparator behind both), drift exits 1,
/// and the identical pair converges with exit 0.
#[test]
fn migrate_version_pair_generates_and_converges() {
    let (from, to) = version_pair_fixture("pair");
    let assert = cli()
        .args(["migrate", "generate", "--from", &from, "--to", &to])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    // The report: the from-only side removes, the to-only side
    // defines, the drifted param changes.
    assert!(
        stdout.contains("\u{2212} db:acme/shop table legacy (required data-migration slot)\n"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("+ db:acme/shop table archive (apply-pending)\n"),
        "{stdout}"
    );
    assert!(stdout.contains("~ db:acme/shop param p\n"), "{stdout}");
    assert!(
        stdout.contains("== required data-migration slots =="),
        "{stdout}"
    );
    // The script: the moved-to table defines first, the dropped one
    // removes under its WARNING + slot.
    let define_at = stdout
        .find("DEFINE TABLE OVERWRITE archive")
        .expect("the define");
    let slot_at = stdout
        .find(">>> SURFORM REQUIRED: DATA MIGRATION")
        .expect("the slot");
    let remove_at = stdout
        .find("REMOVE TABLE IF EXISTS legacy;")
        .expect("the removal");
    assert!(
        define_at < slot_at && slot_at < remove_at,
        "stdout: {stdout}"
    );
    assert!(stdout.contains(">>> SURFORM WARNING"), "{stdout}");
    // Identical versions: converged, exit 0, no findings.
    let (from, _) = version_pair_fixture("pairclean");
    let assert = cli()
        .args(["migrate", "generate", "--from", &from, "--to", &from])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(stdout, "(converged)\n", "stdout: {stdout}");
}

/// `--check` rides the version pair: the report only (the slot
/// manifest included), no script, exit 1 on drift.
#[test]
fn migrate_version_pair_check_is_report_only() {
    let (from, to) = version_pair_fixture("paircheck");
    let assert = cli()
        .args([
            "migrate", "generate", "--from", &from, "--to", &to, "--check",
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout,
        "== drift ==\n\
         3 findings - 2 defines, 1 removal, 1 data-migration slot, 0 staged tables\n\
         ~ db:acme/shop param p\n\
         \x20   header: sdl `param $p = 2;` / db `param $p = 1;`\n\
         + db:acme/shop table archive (apply-pending)\n\
         \u{2212} db:acme/shop table legacy (required data-migration slot)\n\
         \n\
         == required data-migration slots ==\n\
         \x20 db:acme/shop table legacy - the removal deletes the table and every \
         document in it; the script carries the fill-in slot above the removal; \
         when the script removes a whole namespace or database instead, the \
         WARNING above that REMOVE names this finding\n",
        "stdout: {stdout}"
    );
}

/// The input modes stay exclusive where they must: `--from`/`--to`
/// refuse the positional file and `--info`; a lone `--to` refuses.
/// The bare positional file stopped refusing with issue #101 — it is
/// the baseline mode, with its own suite in tests/migrate.rs.
#[test]
fn migrate_version_pair_rejects_mixed_modes() {
    let (from, to) = version_pair_fixture("pairmix");
    let fixture = migrate_fixture("pairmixinfo", false, false);
    // --from with --info: the modes collide.
    let assert = cli()
        .args([
            "migrate",
            "generate",
            "--from",
            &from,
            "--to",
            &to,
            "--info",
            &format!("db=acme/shop={}", fixture.info),
        ])
        .assert()
        .failure();
    assert!(assert.get_output().stdout.is_empty());
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("cannot be used with"), "stderr: {stderr}");
    // A lone --to (or --from) refuses.
    let assert = cli()
        .args(["migrate", "generate", "--to", &to])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("--from"), "stderr: {stderr}");
}

/// The input-mode arity: every spelling that names no
/// complete mode is a clap usage error — exit 2 with the usage text,
/// never a panic. The all-absent run used to pass clap (`requires`
/// is one-directional — it fires only when its arg is PRESENT) and
/// reach the migrator's `expect`: exit 101. A required input group
/// makes all-absent a usage error; each member's `requires` (and the
/// pair side's conflicts) refuse every partial and mixed spelling.
/// The complete modes keep working — the pair legs and the `--info`
/// legs above, the bare file (the baseline, issue #101) in its own
/// suite.
#[test]
fn migrate_generate_arity_needs_one_complete_mode() {
    let (from, to) = version_pair_fixture("arity");
    let fixture = migrate_fixture("arity", false, false);
    let info = format!("db=acme/shop={}", fixture.info);
    let (from, to, sdl, info): (&str, &str, &str, &str) = (&from, &to, &fixture.sdl, &info);
    let legs: Vec<(Vec<&str>, &str)> = vec![
        (vec![], "required arguments were not provided"), // bare
        (
            vec!["--check"],
            "required arguments were not provided", // mode-less flags only
        ),
        (
            vec!["--json"],
            "required arguments were not provided", // the other global alone
        ),
        (
            vec!["--info", info],
            "required arguments were not provided", // --info alone
        ),
        (vec!["--from", from], "--to <FILE>"), // --from alone
        (vec!["--to", to], "--from <FILE>"),   // --to alone
        (vec![sdl, "--from", from], "cannot be used with"), // the modes mix
        (
            vec![sdl, "--to", to],
            "cannot be used with", // a pair side beside the file (the conflict is
                                   // load-bearing: clap waives --to's missing --from
                                   // when a conflicting arg is present)
        ),
        (
            vec!["--from", from, "--to", to, "--info", info],
            "cannot be used with", // the complete pair beside --info
        ),
        (
            vec!["--from", from, "--to", to, sdl],
            "cannot be used with", // the complete pair beside the file
        ),
    ];
    for (args, needle) in &legs {
        let assert = cli()
            .args(["migrate", "generate"])
            .args(args)
            .assert()
            .code(2);
        assert!(assert.get_output().stdout.is_empty(), "args: {args:?}");
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
        assert!(stderr.contains(needle), "args: {args:?} — stderr: {stderr}");
        assert!(
            stderr.contains("Usage:"),
            "args: {args:?} — stderr: {stderr}"
        );
    }
}

/// The retired scope (the #56 corner — CLI-reachable
/// through the version pair): the report still marks the dropped
/// scope's rows (the marker is the ledgered gap), but the manifest
/// sentence names BOTH carriers now — the fill-in slot where the
/// script carries one, the WARNING-anchored owner REMOVE where the
/// script removes the whole namespace or database — and the script
/// carries only the owner REMOVEs (no slot block: the subtree goes
/// whole).
#[test]
fn migrate_version_pair_collapse_reports_the_retired_scope_honestly() {
    let from = std::env::temp_dir().join("surform-migrate-collapse-from.surdl");
    let to = std::env::temp_dir().join("surform-migrate-collapse-to.surdl");
    std::fs::write(&from, "namespace acme;\ndatabase shop;\ntable legacy;\n")
        .expect("fixture writes");
    std::fs::write(&to, "").expect("fixture writes");
    let assert = cli()
        .args([
            "migrate",
            "generate",
            "--from",
            from.to_str().expect("UTF-8"),
            "--to",
            to.to_str().expect("UTF-8"),
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("\u{2212} db:acme/shop table legacy (required data-migration slot)\n"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains(
            "  db:acme/shop table legacy - the removal deletes the table and every \
             document in it; the script carries the fill-in slot above the \
             removal; when the script removes a whole namespace or database \
             instead, the WARNING above that REMOVE names this finding\n"
        ),
        "stdout: {stdout}"
    );
    // The script: the owner REMOVEs carry the data-loss role — the
    // WARNING anchors, and no REQUIRED block anywhere (the fixed
    // header's vocabulary line does not count; the marker syntax
    // does).
    assert!(
        stdout.contains("-- >>> SURFORM WARNING: DATA LOSS <<<"),
        "{stdout}"
    );
    assert!(
        stdout.contains("REMOVE DATABASE IF EXISTS shop;"),
        "{stdout}"
    );
    assert!(
        stdout.contains("REMOVE NAMESPACE IF EXISTS acme;"),
        "{stdout}"
    );
    assert!(!stdout.contains(">>> SURFORM REQUIRED"), "stdout: {stdout}");
}

/// A header-less raw body the TO version drops names itself: its
/// match key is empty (the header never classified), so
/// the report row and the script's manual-removal note quote the
/// body's first line — the nameless `− … raw` row and note used to
/// say nothing about WHAT to remove.
#[test]
fn migrate_version_pair_names_the_headerless_raw() {
    let from = std::env::temp_dir().join("surform-migrate-rawhint-from.surdl");
    let to = std::env::temp_dir().join("surform-migrate-rawhint-to.surdl");
    std::fs::write(
        &from,
        "namespace acme;\ndatabase shop;\nraw { DEFINE THING weird ON legacy; };\n",
    )
    .expect("fixture writes");
    std::fs::write(&to, "namespace acme;\ndatabase shop;\n").expect("fixture writes");
    let assert = cli()
        .args([
            "migrate",
            "generate",
            "--from",
            from.to_str().expect("UTF-8"),
            "--to",
            to.to_str().expect("UTF-8"),
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("\u{2212} db:acme/shop raw - DEFINE THING weird ON legacy;\n"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains(
            "-- manual removal required: db:acme/shop raw - DEFINE THING weird ON legacy;\n"
        ),
        "stdout: {stdout}"
    );
}

/// An anonymous-silo module the TO version drops names itself (the
/// empty-key hint's second producer): its match key is
/// empty (no `mod::` head — the REMOVE stays fail-closed, a
/// known corner), so the report row and the script's
/// manual-removal note quote the `from:` spelling — the nameless
/// `− … module` row and note used to say nothing about WHAT to
/// remove.
#[test]
fn migrate_version_pair_names_the_anonymous_silo_module() {
    let from = std::env::temp_dir().join("surform-migrate-modhint-from.surdl");
    let to = std::env::temp_dir().join("surform-migrate-modhint-to.surdl");
    std::fs::write(
        &from,
        "namespace test;\ndatabase scratch;\n\
         module { from: silo::acme::utils::<1.2.0>; unsigned; };\n",
    )
    .expect("fixture writes");
    std::fs::write(&to, "namespace test;\ndatabase scratch;\n").expect("fixture writes");
    let assert = cli()
        .args([
            "migrate",
            "generate",
            "--from",
            from.to_str().expect("UTF-8"),
            "--to",
            to.to_str().expect("UTF-8"),
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("\u{2212} db:test/scratch module - silo::acme::utils::<1.2.0>\n"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains(
            "-- manual removal required: db:test/scratch module - \
             silo::acme::utils::<1.2.0>\n"
        ),
        "stdout: {stdout}"
    );
    // The REMOVE stays fail-closed: nothing is ever reconstructed
    // from the reader's cross-check key (the ledgered corner).
    assert!(!stdout.contains("REMOVE MODULE"), "stdout: {stdout}");
}

/// `--json` rides the pair mode inertly: the migrate tail
/// is the text report/script for BOTH modes — the flag renders only
/// the input diagnostics, and a pair of clean files yields none.
#[test]
fn migrate_version_pair_json_renders_the_same_tail() {
    let (from, to) = version_pair_fixture("pairjson");
    let assert = cli()
        .args([
            "migrate", "generate", "--from", &from, "--to", &to, "--json",
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(stdout.starts_with("== drift ==\n"), "stdout: {stdout}");
    assert!(
        stdout.contains("== required data-migration slots =="),
        "stdout: {stdout}"
    );
}

// ---------------------------------------------------------------------
// `surform lint`
// ---------------------------------------------------------------------

/// A clean file exits zero and prints nothing (the clippy
/// convention).
#[test]
fn lint_clean_succeeds_silently() {
    let assert = cli()
        .args(["lint", &fixture("clean.surdl")])
        .assert()
        .success();
    assert.get_output().stdout.is_empty();
    assert.get_output().stderr.is_empty();
}

/// Findings render to stderr with their SDL-L codes and exit 1; the
/// stdout channel stays empty.
#[test]
fn lint_findings_exit_one_with_codes_on_stderr() {
    let assert = cli()
        .args(["lint", &fixture("linty.surdl")])
        .assert()
        .failure();
    assert.get_output().stdout.is_empty();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-L0002"), "stderr: {stderr}");
    assert!(stderr.contains("plaintext `password:`"), "stderr: {stderr}");
    assert!(stderr.contains("SDL-L0001"), "stderr: {stderr}");
    assert!(stderr.contains("help:"), "stderr: {stderr}");
}

/// The `--json` renderer emits one object per finding with the code.
#[test]
fn lint_json_emits_one_object_per_finding() {
    let assert = cli()
        .args(["lint", "--json", &fixture("linty.surdl")])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    let lines: Vec<&str> = stderr.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(lines.len(), 2, "stderr: {stderr}");
    for line in lines {
        assert!(line.contains("\"code\": \"SDL-L"), "line: {line}");
        // Findings are advisories: miette renders them at warning
        // severity (the exit-1 convention is the CLI's own).
        assert!(line.contains("\"severity\": \"warning\""), "line: {line}");
    }
}

/// A parse error never lints: the diagnostics report and exit first
/// (the fail-closed posture every command shares).
#[test]
fn lint_broken_file_reports_diagnostics_only() {
    let assert = cli()
        .args(["lint", &fixture("broken.surdl")])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E"), "stderr: {stderr}");
    assert!(!stderr.contains("SDL-L"), "stderr: {stderr}");
}

/// An invalid `--info` tag exits on the interchange diagnostic before
/// any finding renders (normalize/diff's E3005 posture).
#[test]
fn lint_invalid_info_tag_fails_closed() {
    let assert = cli()
        .args(["lint", &fixture("clean.surdl"), "--info", "db=nowhere/x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("SDL-E3005"), "stderr: {stderr}");
    assert!(!stderr.contains("SDL-L"), "stderr: {stderr}");
}

/// The `--info` mode lints the resolved state: an adoptee-only table
/// surfaces with its scope-qualified message and no source snippet
/// (adoptee findings render sourceless).
#[test]
fn lint_info_mode_flags_adoptees_sourceless() {
    let tag = format!("db=acme/app={}", fixture("lint-adoptee-info.json"));
    let assert = cli()
        .args(["lint", &fixture("lint-merge.surdl"), "--info", &tag])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    // The only finding is the adoptee: message-qualified, spanless.
    assert_eq!(stderr.matches("SDL-L").count(), 1, "stderr: {stderr}");
    assert!(
        stderr.contains("table `adoptee` in database `acme/app`"),
        "stderr: {stderr}"
    );
    // Sourceless: no snippet line (`,-[`) anchors the finding.
    assert!(!stderr.contains(",-["), "stderr: {stderr}");
}

/// Rule 5's layout preservation: `//` comments survive fmt at their
/// anchors — above statements, inside table and api blocks — and the
/// output is a fixed point that still parses to the same definition.
/// Comments inside captured SurrealQL bodies (fn bodies, `then:`
/// blocks) are NOT SDL structure and drop.
#[test]
fn fmt_preserves_plain_comments_at_their_anchors() {
    let src = "\
// file header note
// second header line
ns acme;
database app {
  // anchors above its clause
  strict;
};
table user {
  // above a field
  field email: string;
  // above an index
  index unique by_email { fields: email; };
};
api '/x' {
  get { then: 1; };
  // pins the catch-all: any must stay a deny
  any { permissions: none; };
};
// tail note after the last definition
";
    let child = cli()
        .args(["fmt"])
        .write_stdin(src)
        .output()
        .expect("fmt runs");
    assert!(child.status.success());
    let once = String::from_utf8(child.stdout).expect("UTF-8");

    // Every comment line survives, in order, at its anchor.
    for line in [
        "// file header note",
        "// second header line",
        "  // anchors above its clause",
        "  // above a field",
        "  // above an index",
        "  // pins the catch-all: any must stay a deny",
        "// tail note after the last definition",
    ] {
        assert!(once.contains(line), "missing {line:?} in:\n{once}");
    }
    // Anchored placement: header above the namespace, field comment
    // above the field, any-comment above the any block.
    let header = once.find("// file header note").expect("header");
    let ns = once.find("ns acme;").expect("ns");
    let field_c = once.find("// above a field").expect("field comment");
    let field = once.find("field email: string;").expect("field");
    let any_c = once.find("// pins the catch-all").expect("any comment");
    let any = once.find("any {").expect("any");
    let tail = once.find("// tail note").expect("tail");
    assert!(header < ns);
    assert!(field_c < field);
    assert!(any_c < any);
    assert!(tail > once.find("any {").expect("any present"));

    // The fixed point: formatting the output changes nothing, and the
    // output still parses clean.
    let twice = {
        let child = cli()
            .args(["fmt"])
            .write_stdin(once.as_str())
            .output()
            .expect("fmt runs");
        String::from_utf8(child.stdout).expect("UTF-8")
    };
    assert_eq!(twice, once, "comments broke the fixed point");
}

/// Comments are layout, not definition: fmt of a commented param and
/// fmt of the plain one differ by exactly the comment line — the
/// statement itself emits identically (the differ's contract, pinned
/// at the CLI boundary).
#[test]
fn comments_never_change_the_definition() {
    let fmt_of = |src: &str| {
        let child = cli()
            .args(["fmt"])
            .write_stdin(src)
            .output()
            .expect("fmt runs");
        assert!(child.status.success());
        String::from_utf8(child.stdout).expect("UTF-8")
    };
    assert_eq!(
        fmt_of("// why\nparam $limit = 50;"),
        format!("// why\n{}", fmt_of("param $limit = 50;"))
    );
}

/// The include contract at the CLI boundary (spec/include.md): fmt
/// parses the composable entry and round-trips the statement (file
/// and stdin alike) without resolving anything; check composes —
/// pointed at a root whose include target is missing, it exits 1
/// with exactly SDL-E0008, anchored on the include statement (the
/// loader's read half runs; the strict entry's SDL-E1068 belongs to
/// the contexts with no file system — the playground and doc
/// fences).
#[test]
fn fmt_round_trips_includes_check_resolves_them() {
    let src = "namespace acme;\n\ninclude 'tables/users.surdl';\n\ntable t;\n";
    // stdin fmt round-trips byte-stably.
    let once = {
        let child = cli()
            .args(["fmt"])
            .write_stdin(src)
            .output()
            .expect("fmt runs");
        assert!(child.status.success());
        String::from_utf8(child.stdout).expect("UTF-8")
    };
    assert_eq!(once, src);
    // File fmt round-trips too — the composable entry on a real path.
    let dir = std::env::temp_dir().join(format!("surform-include-fmt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("fixture dir writes");
    let path = dir.join("root.surdl");
    std::fs::write(&path, src).expect("fixture writes");
    let child = cli().args(["fmt"]).arg(&path).output().expect("fmt runs");
    assert!(child.status.success());
    assert_eq!(String::from_utf8(child.stdout).expect("UTF-8"), src);
    // check composes: the target is missing, so the load fails with
    // exactly the loader's missing-file code, anchored on the
    // include (the rendered snippet shows the root's own line).
    let child = cli()
        .args(["check"])
        .arg(&path)
        .output()
        .expect("check runs");
    assert!(!child.status.success());
    let err = String::from_utf8(child.stderr).expect("UTF-8");
    assert!(err.contains("SDL-E0008"), "{err}");
    assert!(
        !err.contains("SDL-E1068"),
        "the strict-entry code must not fire at the CLI: {err}"
    );
    assert!(err.contains("include 'tables/users.surdl'"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

/// The composed tree at the CLI boundary (spec/include.md), one leg
/// per subcommand against the fixture root: build emits the flat
/// file's DDL (flatten(root) ≡ root through the real binary), lint
/// renders the composed file's finding against its own file's text,
/// and normalize's no-model merge is a no-op on the canonical tree —
/// the distributed write-back's own legs live in tests/include.rs.
#[test]
fn composed_root_subcommands() {
    let root = "tests/fixtures/include/root.surdl";
    let flat = "tests/fixtures/include/flat.surdl";
    // build: byte-identical to the flat file's DDL.
    let ddl = |file: &str| {
        let child = cli().args(["build", file]).output().expect("build runs");
        assert!(child.status.success(), "build {file} failed");
        String::from_utf8(child.stdout).expect("UTF-8")
    };
    assert_eq!(ddl(root), ddl(flat));
    // lint: exit 1, and the world-open finding renders the INCLUDED
    // file's context (its own text is the snippet's source).
    let child = cli().args(["lint", root]).output().expect("lint runs");
    assert!(!child.status.success());
    let err = String::from_utf8(child.stderr).expect("UTF-8");
    assert!(err.contains("SDL-L0001"), "{err}");
    assert!(err.contains("field name: string"), "{err}");
    // normalize: without `--in-place` the composed root prints the
    // merged FLATTENED canonical form (the preview leg of
    // flatten(root) ≡ root; the in-place distribution's legs live in
    // tests/include.rs). The preview is canonical SDL — a fixed point
    // of the single-file normalizer. The required `--info` tag rides
    // an empty root interchange (issue #70): the no-model merge.
    let empty = std::env::temp_dir().join("surform-composed-empty.info");
    std::fs::write(&empty, "{}\n").expect("fixture writes");
    let no_model = format!("root={}", empty.to_string_lossy());
    let child = cli()
        .args(["normalize", root, "--info", &no_model])
        .output()
        .expect("normalize runs");
    assert!(child.status.success());
    let preview = String::from_utf8(child.stdout).expect("UTF-8");
    assert!(preview.contains("mixin timestamps {"), "{preview}");
    let dir =
        std::env::temp_dir().join(format!("surform-normalize-preview-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("preview dir writes");
    let preview_path = dir.join("preview.surdl");
    std::fs::write(&preview_path, &preview).expect("preview writes");
    let child = cli()
        .args(["normalize", "--in-place", "--info", &no_model])
        .arg(&preview_path)
        .output()
        .expect("normalize runs");
    assert!(child.status.success());
    assert_eq!(
        std::fs::read_to_string(&preview_path).expect("preview reads"),
        preview,
        "the composed preview must be a single-file normalize fixed point"
    );
    std::fs::remove_dir_all(&dir).ok();
    // fmt: the composition round-trips (never flattens) — includes
    // survive, and the canonical form is a fixed point (re-formatted
    // through a COPY of the whole tree: a composed root's canonical
    // text formats only next to its own includes).
    let child = cli().args(["fmt", root]).output().expect("fmt runs");
    assert!(child.status.success());
    let formatted = String::from_utf8(child.stdout).expect("UTF-8");
    assert!(formatted.contains("include 'mixins.surdl'"), "{formatted}");
    assert!(
        formatted.contains("include 'tables/users.surdl'"),
        "{formatted}"
    );
    let tree = std::env::temp_dir().join(format!("surform-composed-tree-{}", std::process::id()));
    for (from, to) in [
        ("tests/fixtures/include/root.surdl", "root.surdl"),
        ("tests/fixtures/include/mixins.surdl", "mixins.surdl"),
        (
            "tests/fixtures/include/tables/users.surdl",
            "tables/users.surdl",
        ),
        (
            "tests/fixtures/include/tables/sessions.surdl",
            "tables/sessions.surdl",
        ),
        (
            "tests/fixtures/include/shared/ids.surdl",
            "shared/ids.surdl",
        ),
    ] {
        let to = tree.join(to);
        std::fs::create_dir_all(to.parent().expect("has a parent")).expect("tree dirs");
        std::fs::copy(from, &to).expect("tree copies");
    }
    let copy = tree.join("root.surdl");
    let child = cli()
        .args(["fmt", "--in-place"])
        .arg(&copy)
        .output()
        .expect("fmt runs");
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&copy).expect("copy reads"),
        formatted
    );
    std::fs::remove_dir_all(&tree).ok();
}

// ---------------------------------------------------------------------
// fmt on a composed tree (issue #45)
// ---------------------------------------------------------------------

/// A three-file composition, seeded canonical: the root includes the
/// mixin file and the table file; the table spreads the mixin
/// (defined in a SIBLING — the shape a standalone parse cannot
/// resolve).
fn compose_tree(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("surform-fmt-tree-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("parts")).expect("tree dir creates");
    let root = dir.join("root.surdl");
    std::fs::write(
        &root,
        "include 'parts/m.surdl';\n\ninclude 'parts/t.surdl';\n",
    )
    .expect("root writes");
    let m = dir.join("parts/m.surdl");
    std::fs::write(&m, "mixin stamped {\n  field stamped_at: datetime;\n};\n").expect("m writes");
    let t = dir.join("parts/t.surdl");
    std::fs::write(
        &t,
        "table task {\n  ...stamped;\n  field name: string;\n};\n",
    )
    .expect("t writes");
    (root, m, t)
}

/// `--in-place` on a root rewrites the whole tree: an experimental
/// perturbation in an included file survives nothing, and the spread
/// rides fmt untouched (fmt is authored-preserving — expansion is
/// the load's, not the formatter's).
#[test]
fn fmt_in_place_on_a_root_writes_every_participant() {
    let (root, m, t) = compose_tree("writes");
    std::fs::write(&m, "mixin stamped {\n\n\n  stamped_at: datetime;\n};\n").expect("m perturbs");
    std::fs::write(
        &t,
        "table task {\n\n\n  field name: string;\n  ...stamped;\n};\n",
    )
    .expect("t perturbs");
    cli()
        .args(["fmt", "--in-place", root.to_string_lossy().as_ref()])
        .assert()
        .success();
    let m_text = std::fs::read_to_string(&m).expect("m reads");
    assert_eq!(
        m_text, "mixin stamped {\n  field stamped_at: datetime;\n};\n",
        "the mixin file is canonical again"
    );
    let t_text = std::fs::read_to_string(&t).expect("t reads");
    assert_eq!(
        t_text, "table task {\n  ...stamped;\n  field name: string;\n};\n",
        "the table file is canonical again, spread intact"
    );
}

/// The tree write is fmt-stable: a second run changes no bytes.
#[test]
fn fmt_in_place_on_a_root_is_idempotent_across_the_tree() {
    let (root, m, t) = compose_tree("idempotent");
    std::fs::write(
        &t,
        "table task {\n\n  field name: string;\n  ...stamped;\n};\n",
    )
    .expect("t perturbs");
    cli()
        .args(["fmt", "--in-place", root.to_string_lossy().as_ref()])
        .assert()
        .success();
    let m_once = std::fs::read_to_string(&m).expect("m reads");
    let t_once = std::fs::read_to_string(&t).expect("t reads");
    cli()
        .args(["fmt", "--in-place", root.to_string_lossy().as_ref()])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(&m).expect("m re-reads"),
        m_once,
        "the second run must not churn the mixin file"
    );
    assert_eq!(
        std::fs::read_to_string(&t).expect("t re-reads"),
        t_once,
        "the second run must not churn the table file"
    );
}

/// A dirty composition is never rewritten: the load failure exits
/// before ANY file changes — the root keeps its bytes even though it
/// alone would have formatted clean.
#[test]
fn fmt_in_place_never_touches_a_broken_tree() {
    let (root, m, t) = compose_tree("atomic");
    // Break the spread: no mixin of this name exists in the tree, so
    // the LOADER fails (the resolution pass reports the E1061).
    std::fs::write(&t, "table task {\n  ...ghosted;\n};\n").expect("t breaks");
    let root_before = std::fs::read_to_string(&root).expect("root reads");
    let m_before = std::fs::read_to_string(&m).expect("m reads");
    let assert = cli()
        .args(["fmt", "--in-place", root.to_string_lossy().as_ref()])
        .assert()
        .failure();
    assert!(
        String::from_utf8(assert.get_output().stderr.clone())
            .expect("UTF-8")
            .contains("SDL-E1061"),
        "the loader's diagnostic reports: {assert:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&root).expect("root re-reads"),
        root_before,
        "the root keeps its bytes"
    );
    assert_eq!(std::fs::read_to_string(&m).expect("m re-reads"), m_before);
}

/// Formatting an included file standalone WORKS now (the
/// declarative namespace's standalone-leaf win): parse carries
/// spreads opaque and fmt never resolves, so the leaf's own text
/// formats cleanly — its spread round-trips even though the mixin
/// lives in another file. Resolution stays check-class.
#[test]
fn fmt_on_an_included_leaf_formats_standalone() {
    let (root, _m, t) = compose_tree("help");
    let before = std::fs::read_to_string(&t).expect("t reads");
    let assert = cli()
        .args(["fmt", t.to_string_lossy().as_ref()])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout, before,
        "the leaf formats to its own canonical form (already canonical)"
    );
    let _ = root;
}

/// Print mode stays per-file: a root prints its own authored text
/// (include statements and all) and touches nothing on disk.
#[test]
fn fmt_print_mode_stays_per_file() {
    let (root, m, t) = compose_tree("print");
    let m_before = std::fs::read_to_string(&m).expect("m reads");
    let t_before = std::fs::read_to_string(&t).expect("t reads");
    let assert = cli()
        .args(["fmt", root.to_string_lossy().as_ref()])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout, "include 'parts/m.surdl';\ninclude 'parts/t.surdl';\n",
        "print mode formats the root's own text only (the include run packs tight)"
    );
    assert_eq!(std::fs::read_to_string(&m).expect("m re-reads"), m_before);
    assert_eq!(std::fs::read_to_string(&t).expect("t re-reads"), t_before);
}
