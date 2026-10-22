//! The `migrate revert` suite: the HTTP boundary is
//! MOCKED (the shared `/sql` mock) for the wire legs — the row lookup
//! (number and hash-prefix spellings, ambiguity, zero matches), the
//! generated draft's shape (the marker-bearing slots, the `.surdl`
//! sidecar carrying the recorded state verbatim), the ledger table's
//! immunity, and the refusals — plus the EXECUTED round trip: a real
//! pipeline (generate → fill → commit → apply) against the EMBEDDED
//! engine, the mock carrying only the wire, ending with a revert
//! draft that is filled, committed, and pushed back to v1's schema
//! with all three ledger records present.

#[path = "common/mod.rs"]
mod common;
#[path = "common/mock_sql.rs"]
mod mock_sql;

use assert_cmd::Command;
use mock_sql::{
    MockSql, Scripted, err_element, json_string, ledger_row_sdl, missing_table_element, ok_element,
    select_element,
};
use std::path::Path;

fn cli() -> Command {
    let mut command = Command::cargo_bin("surform").expect("the surform binary builds");
    // Hermetic against the developer's shell: the credential
    // environment reaches a run only through a test's explicit `.env`
    // (an inherited SURFORM_DB_* would flip the arity, pairing, and
    // exclusivity legs). A later `.env` on the same variable wins.
    command
        .env_remove("SURFORM_DB_USER")
        .env_remove("SURFORM_DB_PASS")
        .env_remove("SURFORM_DB_TOKEN");
    command
}

fn workdir(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("surform-n3-revert-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the workdir creates");
    dir.to_string_lossy().into_owned()
}

/// The recorded state (the ledger row's `sdl` column): database
/// `test/scratch` at v1 — one schemafull table, one field.
const V1_SDL: &str = "namespace test;\ndatabase scratch;\n\ntable user {\n  schemafull;\n  field email: string;\n};\n";

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// A second hash sharing a prefix with [`HASH_A`] — the ambiguity leg.
const HASH_B: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab";

fn revert_cmd(endpoint: &str, dir: &Path, to: &str) -> Command {
    let mut command = cli();
    command
        .args([
            "migrate",
            "revert",
            "--endpoint",
            endpoint,
            "--ns",
            "test",
            "--db",
            "scratch",
            "--dir",
        ])
        .arg(dir)
        .args(["--to", to]);
    command
}

/// The db-level INFO answer: the full section map with the given
/// `tables` (name → DEFINE TABLE text).
fn info_db(tables: &[(&str, &str)]) -> Scripted {
    let body: Vec<String> = tables
        .iter()
        .map(|(name, ddl)| format!("{}:{}", json_string(name), json_string(ddl)))
        .collect();
    Scripted {
        status: 200,
        body: format!(
            "[{}]",
            select_element(&format!(
                r#"{{"accesses":{{}},"analyzers":{{}},"apis":{{}},"buckets":{{}},"configs":{{}},"functions":{{}},"models":{{}},"modules":{{}},"params":{{}},"sequences":{{}},"tables":{{{}}},"users":{{}}}}"#,
                body.join(",")
            ))
        ),
    }
}

/// The table-level INFO answer: the 3.3.0 section set with the given
/// `fields`.
fn info_table(fields: &[(&str, &str)]) -> Scripted {
    let body: Vec<String> = fields
        .iter()
        .map(|(name, ddl)| format!("{}:{}", json_string(name), json_string(ddl)))
        .collect();
    Scripted {
        status: 200,
        body: format!(
            "[{}]",
            select_element(&format!(
                r#"{{"events":{{}},"fields":{{{}}},"indexes":{{}},"lives":{{}},"tables":{{}}}}"#,
                body.join(",")
            ))
        ),
    }
}

fn ok_script() -> Scripted {
    Scripted {
        status: 200,
        body: format!("[{}]", ok_element()),
    }
}

/// The scripted answers for a revert against the v2 live state
/// (tables `extra` + `user`; `user` carries `email` + `name`): the
/// wide ledger SELECT, INFO FOR DB, then INFO FOR TABLE per table in
/// the ingestion's sorted order.
fn v2_script(hash: &str) -> Vec<Scripted> {
    vec![
        Scripted {
            status: 200,
            body: format!(
                "[{}]",
                select_element(&format!("[{}]", ledger_row_sdl(1, "v1", hash, V1_SDL)))
            ),
        },
        info_db(&[
            (
                "extra",
                "DEFINE TABLE extra TYPE NORMAL SCHEMALESS PERMISSIONS NONE",
            ),
            (
                "user",
                "DEFINE TABLE user TYPE NORMAL SCHEMAFULL PERMISSIONS NONE",
            ),
        ]),
        info_table(&[]),
        info_table(&[
            (
                "email",
                "DEFINE FIELD email ON user TYPE string PERMISSIONS FULL",
            ),
            (
                "name",
                "DEFINE FIELD name ON user TYPE string PERMISSIONS FULL",
            ),
        ]),
    ]
}

/// The row-lookup spellings: the number (`1`, `0001`), a hash prefix,
/// and the full hash all locate the same row; the requests pin the
/// wide projection and the INFO assembly.
#[test]
fn the_row_lookup_spellings() {
    for tag in [
        "number",
        "zero-number",
        "prefix",
        "prefix-upper",
        "full-hash",
    ] {
        let dir = workdir(tag);
        let migrations = Path::new(&dir).join("migrations");
        let mock = MockSql::start(v2_script(HASH_A), ok_script());
        let to = match tag {
            "number" => "1",
            "zero-number" => "0001",
            "prefix" => &HASH_A[..12],
            "prefix-upper" => &HASH_A[..12].to_ascii_uppercase(),
            _ => HASH_A,
        };
        let assert = revert_cmd(&mock.endpoint(), &migrations, to)
            .assert()
            .success();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
        assert!(stdout.contains("== revert =="), "{tag}: {stdout}");
        assert!(
            stdout.contains("0001_v1") && stdout.contains("draft:"),
            "{tag}: {stdout}"
        );
        let requests = mock.requests();
        assert_eq!(requests.len(), 4, "{tag}: SELECT + INFO DB + two tables");
        assert_eq!(
            requests[0].body, "SELECT number, name, hash, sdl FROM _surform_migrations;",
            "{tag}: the wide projection — the revert read"
        );
        assert_eq!(requests[1].body, "INFO FOR DB;", "{tag}");
        assert_eq!(
            requests[2].body, "INFO FOR TABLE extra;",
            "{tag}: sorted order"
        );
        assert_eq!(
            requests[3].body, "INFO FOR TABLE `user`;",
            "{tag}: ident_text's quoting rides the child query"
        );
        assert!(
            migrations.join("0001_revert-to-0001-v1.surql").exists(),
            "{tag}"
        );
        let _ = assert;
    }
}

/// The bearer credential (issue #99) rides revert's read legs too:
/// one `--token` and EVERY request — the wide ledger SELECT and the
/// INFO legs — carries `Authorization: Bearer <token>`.
#[test]
fn the_bearer_token_rides_every_read_leg() {
    let dir = workdir("token");
    let migrations = Path::new(&dir).join("migrations");
    let mock = MockSql::start(v2_script(HASH_A), ok_script());
    revert_cmd(&mock.endpoint(), &migrations, "1")
        .args(["--token", "revert-jwt"])
        .assert()
        .success();
    let requests = mock.requests();
    assert_eq!(requests.len(), 4, "SELECT + INFO DB + two tables");
    for request in &requests {
        assert_eq!(
            request.header("authorization"),
            Some("Bearer revert-jwt"),
            "the bearer token rides every POST: {}",
            request.body
        );
    }
}

/// The lookup failures: an unknown number, a matching-nothing prefix,
/// an ambiguous prefix (both candidates named), and unreadable
/// targets — and a fresh database (missing ledger table) is the empty
/// ledger, hence row-not-found.
#[test]
fn the_lookup_failures() {
    let dir = workdir("failures");
    let migrations = Path::new(&dir).join("migrations");
    let stderr_of = |assert: assert_cmd::assert::Assert| {
        String::from_utf8(assert.get_output().stderr.clone()).unwrap()
    };
    // Unknown number and matching-nothing prefix.
    let mock = MockSql::start(v2_script(HASH_A), ok_script());
    let assert = revert_cmd(&mock.endpoint(), &migrations, "7")
        .assert()
        .failure();
    assert!(stderr_of(assert).contains("no ledger row matches `7`"));
    let mock = MockSql::start(v2_script(HASH_A), ok_script());
    let assert = revert_cmd(&mock.endpoint(), &migrations, &"c".repeat(12))
        .assert()
        .failure();
    assert!(
        stderr_of(assert).contains("no ledger row matches"),
        "a zero-match prefix refuses"
    );
    // Ambiguity: both rows share the 12-char prefix; the error names
    // both candidates (number, name, hash).
    let mut rows = v2_script(HASH_A);
    rows[0] = Scripted {
        status: 200,
        body: format!(
            "[{}]",
            select_element(&format!(
                "[{},{}]",
                ledger_row_sdl(1, "v1", HASH_A, V1_SDL),
                ledger_row_sdl(2, "v1-again", HASH_B, V1_SDL),
            ))
        ),
    };
    let mock = MockSql::start(rows, ok_script());
    let assert = revert_cmd(&mock.endpoint(), &migrations, &HASH_A[..12])
        .assert()
        .failure();
    let stderr = stderr_of(assert);
    assert!(stderr.contains("matches 2 ledger rows"), "{stderr}");
    assert!(
        stderr.contains("0001_v1") && stderr.contains("0002_v1-again"),
        "{stderr}"
    );
    assert!(stderr.contains(HASH_B), "{stderr}");
    // Unreadable targets: a non-hex spelling, and a prefix below the
    // four-character floor.
    for to in ["xyz", "12g4", "abc"] {
        let mock = MockSql::start(v2_script(HASH_A), ok_script());
        let assert = revert_cmd(&mock.endpoint(), &migrations, to)
            .assert()
            .failure();
        let stderr = stderr_of(assert);
        assert!(
            stderr.contains("neither a migration number") && stderr.contains("hash prefix"),
            "{to}: {stderr}"
        );
        assert_eq!(
            mock.requests().len(),
            0,
            "{to}: an unreadable target never dials"
        );
    }
    // A fresh database: the missing-table ERR is the empty ledger —
    // no row matches.
    let mock = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!("[{}]", missing_table_element("_surform_migrations")),
        }],
        ok_script(),
    );
    let assert = revert_cmd(&mock.endpoint(), &migrations, "1")
        .assert()
        .failure();
    assert!(stderr_of(assert).contains("no ledger row matches `1`"));
}

/// The generated draft's shape: the header (input `versions`, source
/// = the sidecar), the sidecar carrying the recorded state VERBATIM,
/// the marker-bearing slots for the data-orphaning removals, and the
/// stdout guidance naming the fill → commit → push road. Nothing is
/// applied: the only requests are the reads.
#[test]
fn the_generated_draft_shape() {
    let dir = workdir("shape");
    let migrations = Path::new(&dir).join("migrations");
    let mock = MockSql::start(v2_script(HASH_A), ok_script());
    let assert = revert_cmd(&mock.endpoint(), &migrations, "1")
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let draft_path = migrations.join("0001_revert-to-0001-v1.surql");
    let sidecar_path = migrations.join("0001_revert-to-0001-v1.surdl");
    assert!(draft_path.exists(), "{stdout}");
    assert!(sidecar_path.exists(), "{stdout}");
    // The sidecar is the recorded state, byte for byte.
    let sidecar = std::fs::read_to_string(&sidecar_path).unwrap();
    assert_eq!(sidecar, V1_SDL, "the recorded state rides verbatim");
    // The draft's header: source names the sidecar, the `-- input:`
    // line records the diff's inputs (issue #75 — the live side has
    // no file spelling: `from=(live)`, the sidecar as `to`), status
    // draft.
    let draft = std::fs::read_to_string(&draft_path).unwrap();
    assert!(
        draft.starts_with("-- surform migration 0001 — revert-to-0001-v1\n"),
        "{draft}"
    );
    assert!(
        draft.contains(&format!("-- source: {}", sidecar_path.display())),
        "{draft}"
    );
    assert!(
        draft.contains(&format!(
            "-- input: versions from=(live) to={}",
            sidecar_path.display()
        )),
        "{draft}"
    );
    assert!(draft.contains("-- status: draft\n"), "{draft}");
    // The data-orphaning removals carry their resolution markers
    // (the field's marker carries the ON edge — issue #86).
    assert!(
        draft.contains("<<<<<<< SURFORM SLOT (db:test/scratch: table extra)"),
        "{draft}"
    );
    assert!(
        draft.contains("<<<<<<< SURFORM SLOT (db:test/scratch: field name ON user)"),
        "{draft}"
    );
    assert!(draft.contains(">>>>>>> SURFORM SLOT END"), "{draft}");
    // The stdout names the slots and the road onward — and says
    // nothing was applied.
    assert!(
        stdout.contains("2 unresolved SURFORM SLOT region(s)"),
        "{stdout}"
    );
    assert!(stdout.contains("db:test/scratch: table extra"), "{stdout}");
    assert!(stdout.contains("nothing is applied yet"), "{stdout}");
    assert!(stdout.contains("migrate commit"), "{stdout}");
    assert!(stdout.contains("migrate push"), "{stdout}");
    assert_eq!(
        mock.requests().len(),
        4,
        "the reads only — revert never applies"
    );
}

/// The sidecar collision (the numbering leg the CLI can stage — the
/// `.surdl` is invisible to the walk, so a pre-placed file does not
/// bump the draft number): the run refuses, NOTHING is overwritten,
/// and the error names both recoveries — the concurrent writer's
/// re-run and the leftover's check-then-delete.
#[test]
fn a_sidecar_collision_names_both_recoveries() {
    let dir = workdir("sidecar-collision");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let stuck = migrations.join("0001_revert-to-0001-v1.surdl");
    std::fs::write(&stuck, "an earlier run's leftover\n").unwrap();
    let mock = MockSql::start(v2_script(HASH_A), ok_script());
    let assert = revert_cmd(&mock.endpoint(), &migrations, "1")
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(
        stderr.contains("re-run this command once it finishes"),
        "{stderr}: the concurrent-writer recovery"
    );
    assert!(
        stderr.contains("delete it, then re-run"),
        "{stderr}: the leftover recovery"
    );
    assert_eq!(
        std::fs::read_to_string(&stuck).unwrap(),
        "an earlier run's leftover\n",
        "nothing was overwritten"
    );
    assert!(
        !migrations.join("0001_revert-to-0001-v1.surql").exists(),
        "no draft lands on a collided sidecar"
    );
}

/// The converged case: the live schema already matches the recorded
/// state — no draft, no sidecar, nothing written.
#[test]
fn converged_writes_nothing() {
    let dir = workdir("converged");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let mock = MockSql::start(
        vec![
            Scripted {
                status: 200,
                body: format!(
                    "[{}]",
                    select_element(&format!("[{}]", ledger_row_sdl(1, "v1", HASH_A, V1_SDL)))
                ),
            },
            info_db(&[(
                "user",
                "DEFINE TABLE user TYPE NORMAL SCHEMAFULL PERMISSIONS NONE",
            )]),
            info_table(&[(
                "email",
                "DEFINE FIELD email ON user TYPE string PERMISSIONS FULL",
            )]),
        ],
        ok_script(),
    );
    let assert = revert_cmd(&mock.endpoint(), &migrations, "1")
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("(converged"), "{stdout}");
    assert!(
        std::fs::read_dir(&migrations).unwrap().count() == 0,
        "nothing was written: {stdout}"
    );
}

/// The ledger table is the pipeline's machinery: the live INFO's
/// `_surform_migrations` entry never joins the compare — no child
/// fetch for it, and the draft never removes it.
#[test]
fn the_ledger_table_is_never_revert_material() {
    let dir = workdir("ledger-immune");
    let migrations = Path::new(&dir).join("migrations");
    let mock = MockSql::start(
        vec![
            Scripted {
                status: 200,
                body: format!(
                    "[{}]",
                    select_element(&format!("[{}]", ledger_row_sdl(1, "v1", HASH_A, V1_SDL)))
                ),
            },
            info_db(&[
                (
                    "_surform_migrations",
                    "DEFINE TABLE _surform_migrations TYPE NORMAL SCHEMAFULL PERMISSIONS NONE",
                ),
                (
                    "user",
                    "DEFINE TABLE user TYPE NORMAL SCHEMAFULL PERMISSIONS NONE",
                ),
            ]),
            // A v2-only field gives the compare something to draft (a
            // pure-converge scenario would write nothing to assert
            // against); the assertions below stay on the ledger's
            // absence.
            info_table(&[
                (
                    "email",
                    "DEFINE FIELD email ON user TYPE string PERMISSIONS FULL",
                ),
                (
                    "name",
                    "DEFINE FIELD name ON user TYPE string PERMISSIONS FULL",
                ),
            ]),
        ],
        ok_script(),
    );
    let assert = revert_cmd(&mock.endpoint(), &migrations, "1")
        .assert()
        .success();
    let requests = mock.requests();
    assert_eq!(requests.len(), 3, "no child fetch for the ledger table");
    for request in &requests[1..] {
        assert!(
            !request.body.contains("_surform_migrations"),
            "the ledger table never joins the compare: {}",
            request.body
        );
    }
    let draft = std::fs::read_to_string(migrations.join("0001_revert-to-0001-v1.surql")).unwrap();
    assert!(
        !draft.contains("REMOVE TABLE IF EXISTS _surform_migrations"),
        "no revert can draft the ledger's removal: {draft}"
    );
    let _ = assert;
}

/// The scope guard: a stored SDL that never declares `--db`'s
/// database refuses, naming what the schema declares — never a silent
/// empty draft.
#[test]
fn the_scope_guard_refuses() {
    let dir = workdir("scope");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let other_db = "namespace test;\ndatabase elsewhere;\n\ntable user {\n  schemafull;\n  field email: string;\n};\n";
    let mock = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!(
                "[{}]",
                select_element(&format!("[{}]", ledger_row_sdl(1, "v1", HASH_A, other_db)))
            ),
        }],
        ok_script(),
    );
    let assert = revert_cmd(&mock.endpoint(), &migrations, "1")
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("never declares database `scratch`"),
        "{stderr}"
    );
    assert!(stderr.contains("test/elsewhere"), "{stderr}");
    assert_eq!(
        mock.requests().len(),
        1,
        "the scope guard fires before the live fetch"
    );
}

/// A ledger row without a readable `sdl` column refuses — the
/// recorded state is the whole substrate.
#[test]
fn a_row_without_sdl_refuses() {
    let dir = workdir("no-sdl");
    let migrations = Path::new(&dir).join("migrations");
    let mock = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!(
                "[{}]",
                select_element(&format!(
                    r#"[{{"hash":{},"name":{},"number":1}}]"#,
                    json_string(HASH_A),
                    json_string("v1")
                ))
            ),
        }],
        ok_script(),
    );
    let assert = revert_cmd(&mock.endpoint(), &migrations, "1")
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("no readable `sdl` column"), "{stderr}");
}

/// The wire failures surface: a non-OK INFO element (a missing
/// namespace among them) is a schema-read error, never a partial
/// draft; a malformed response refuses without panicking.
#[test]
fn the_wire_failures_surface() {
    let dir = workdir("wire");
    let migrations = Path::new(&dir).join("migrations");
    let stderr_of = |assert: assert_cmd::assert::Assert| {
        String::from_utf8(assert.get_output().stderr.clone()).unwrap()
    };
    // The INFO FOR DB element ERRs (a mistyped --ns lands NotFound).
    let rows = v2_script(HASH_A);
    let mut wrong_ns = rows.clone();
    wrong_ns[1] = Scripted {
        status: 200,
        body: format!(
            "[{}]",
            err_element(
                "NotFound",
                Some("Namespace"),
                "The namespace 'does-not-exist' does not exist"
            )
        ),
    };
    let mock = MockSql::start(wrong_ns, ok_script());
    let assert = revert_cmd(&mock.endpoint(), &migrations, "1")
        .assert()
        .failure();
    let stderr = stderr_of(assert);
    assert!(
        stderr.contains("reading the database schema failed"),
        "{stderr}"
    );
    assert!(stderr.contains("does-not-exist"), "{stderr}");
    assert!(
        !migrations.join("0001_revert-to-0001-v1.surql").exists(),
        "no draft on a failed fetch"
    );
    // A malformed response body refuses, never panics.
    for body in ["", "not json", "[]", r#"[{"status": 7}]"#] {
        let mock = MockSql::start(
            vec![Scripted {
                status: 200,
                body: body.to_string(),
            }],
            ok_script(),
        );
        let assert = revert_cmd(&mock.endpoint(), &migrations, "1")
            .assert()
            .failure();
        let stderr = stderr_of(assert);
        assert!(
            stderr.contains("malformed") || stderr.contains("not a JSON array"),
            "body {body:?}: {stderr}"
        );
        assert!(!stderr.contains("panicked"), "no panic on {body:?}");
    }
}

// ---------------------------------------------------------------------
// The executed round trip: generate → fill → commit → apply against
// the EMBEDDED engine (the mock carries only the wire), then revert →
// fill → commit → push — the database back at v1's schema, all three
// ledger records present, and the revert-of-the-revert substrate
// byte-identical to the original.
// ---------------------------------------------------------------------

/// One engine-side ledger row, from the SDK Value shape.
fn engine_row(row: &surrealdb::types::Value, key: &str) -> String {
    use surrealdb::types::ToSql;
    let surrealdb::types::Value::Object(map) = row else {
        panic!("a ledger row is an object: {row:?}");
    };
    match map.get(key) {
        Some(surrealdb::types::Value::String(text)) => text.clone(),
        Some(other) => other.to_sql(),
        None => panic!("the row carries no `{key}`: {map:?}"),
    }
}

/// The engine's Value → the wire's JSON text (the /sql serialization's
/// shape for the only values these queries return: objects, strings,
/// numbers). Anything else would be a shape the probe never showed —
/// the test fails loudly rather than serves fiction.
fn value_to_json(value: &surrealdb::types::Value) -> String {
    use surrealdb::types::{ToSql, Value};
    match value {
        Value::Object(map) => {
            let members: Vec<String> = map
                .iter()
                .map(|(key, value)| format!("{}:{}", json_string(key), value_to_json(value)))
                .collect();
            format!("{{{}}}", members.join(","))
        }
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(value_to_json).collect();
            format!("[{}]", items.join(","))
        }
        Value::String(text) => json_string(text),
        Value::Number(_) => value.to_sql(),
        Value::None | Value::Null => "null".to_string(),
        other => panic!("a shape the probe never showed: {other:?}"),
    }
}

/// Replaces each `<<<<<<< SURFORM SLOT (name)` … `>>>>>>> SURFORM
/// SLOT END` region with its query — the fill, as the user performs
/// it.
fn fill(text: &str, slots: &[(&str, &str)]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix("<<<<<<< SURFORM SLOT (") {
            let name = rest.strip_suffix(')').unwrap_or(rest);
            let replacement = slots
                .iter()
                .find(|(slot, _)| *slot == name)
                .map(|(_, query)| *query)
                .unwrap_or_else(|| panic!("unplanned slot: {name}"));
            for follow in lines.by_ref() {
                if follow == ">>>>>>> SURFORM SLOT END" {
                    break;
                }
            }
            out.push(replacement.to_string());
        } else {
            out.push(line.to_string());
        }
    }
    let mut joined = out.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

#[tokio::test]
async fn the_reverted_then_pushed_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    use surrealdb::Surreal;
    use surrealdb::engine::local::Mem;
    use surrealdb::types::{ToSql, Value};

    let dir = workdir("roundtrip");
    let dir_path = Path::new(&dir);
    std::fs::write(dir_path.join("empty.surdl"), "")?;
    std::fs::write(dir_path.join("v1.surdl"), V1_SDL)?;
    std::fs::write(
        dir_path.join("v2.surdl"),
        "namespace test;\ndatabase scratch;\n\ntable user {\n  schemafull;\n  field email: string;\n  field name: string;\n};\n\ntable extra {\n  schemaless;\n};\n",
    )?;

    let db = Surreal::new::<Mem>(()).await?;
    db.use_ns("test").use_db("scratch").await?;

    // 0001: empty → v1. (a successful save exits 0 — the draft is the
    // artifact, #71.)
    cli()
        .current_dir(dir_path)
        .args([
            "migrate",
            "generate",
            "--from",
            "empty.surdl",
            "--to",
            "v1.surdl",
            "--save",
            "--name",
            "v1",
        ])
        .assert()
        .success();
    let one = dir_path.join("migrations/0001_v1.surql");
    assert!(one.exists(), "the baseline draft");
    cli()
        .current_dir(dir_path)
        .args([
            "migrate",
            "commit",
            "migrations/0001_v1.surql",
            "-m",
            "baseline",
        ])
        .assert()
        .success();
    // The apply: the file's full text through the engine — exactly
    // what a push POST carries — self-records its ledger row.
    let text = std::fs::read_to_string(&one)?;
    db.query(&text).await?.check()?;

    // 0002: v1 → v2 (defines only — no slots).
    cli()
        .current_dir(dir_path)
        .args([
            "migrate", "generate", "--from", "v1.surdl", "--to", "v2.surdl", "--save", "--name",
            "v2",
        ])
        .assert()
        .success();
    let two = dir_path.join("migrations/0002_v2.surql");
    assert!(two.exists(), "the v2 draft");
    let draft_two = std::fs::read_to_string(&two)?;
    assert!(
        !draft_two.contains("SURFORM SLOT"),
        "defines orphan nothing: {draft_two}"
    );
    cli()
        .current_dir(dir_path)
        .args([
            "migrate",
            "commit",
            "migrations/0002_v2.surql",
            "-m",
            "add name and extra",
        ])
        .assert()
        .success();
    let text = std::fs::read_to_string(&two)?;
    db.query(&text).await?.check()?;

    // Live data at v2 — the rows the revert's slots must move.
    db.query("CREATE user:1 SET email = 'a@b.c', name = 'Jane';")
        .await?
        .check()?;
    db.query("CREATE extra:1 SET note = 'x';").await?.check()?;

    // status: the engine's ledger, through the mock's wire — both
    // applied.
    let ledger: Vec<Value> = db
        .query("SELECT number, name, hash FROM _surform_migrations ORDER BY number;")
        .await?
        .take(0)?;
    assert_eq!(ledger.len(), 2, "two records after two migrations");
    let status_mock = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!(
                "[{}]",
                select_element(&format!(
                    "[{}]",
                    ledger
                        .iter()
                        .map(|row| ledger_row_sdl(
                            engine_row(row, "number").parse().unwrap(),
                            &engine_row(row, "name"),
                            &engine_row(row, "hash"),
                            "",
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                ))
            ),
        }],
        ok_script(),
    );
    // (The wide row helper serves the narrow read fine — status reads
    // only name/hash/number.)
    let assert = cli()
        .current_dir(dir_path)
        .args([
            "migrate",
            "status",
            "--endpoint",
            &status_mock.endpoint(),
            "--ns",
            "test",
            "--db",
            "scratch",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())?;
    assert!(
        stdout.contains("0001_v1") && stdout.contains("applied"),
        "{stdout}"
    );
    assert!(
        stdout.contains("0002_v2") && stdout.contains("applied"),
        "{stdout}"
    );

    // revert --to 1: the live state (v2, from the engine's own INFO)
    // converges toward the recorded v1. The engine's ledger table
    // rides its INFO — and must be immune.
    let engine_info_db: Value = db.query("INFO FOR DB").await?.take(0)?;
    let revert_rows: Vec<Value> = db
        .query("SELECT number, name, hash, sdl FROM _surform_migrations ORDER BY number;")
        .await?
        .take(0)?;
    // Row 1 is the revert target; its sdl column is the substrate.
    let row_one_sdl = engine_row(&revert_rows[0], "sdl");
    let revert_mock = MockSql::start(
        vec![
            Scripted {
                status: 200,
                body: format!(
                    "[{}]",
                    select_element(&format!(
                        "[{}]",
                        revert_rows
                            .iter()
                            .map(|row| ledger_row_sdl(
                                engine_row(row, "number").parse().unwrap(),
                                &engine_row(row, "name"),
                                &engine_row(row, "hash"),
                                &engine_row(row, "sdl"),
                            ))
                            .collect::<Vec<_>>()
                            .join(",")
                    ))
                ),
            },
            Scripted {
                status: 200,
                body: format!("[{}]", select_element(&value_to_json(&engine_info_db))),
            },
        ],
        // The per-table INFO answers, engine-derived (the fallback
        // answers every table the ingestion asks for — the mock
        // cannot parse the query, so each answer is served fresh from
        // the engine by table name below).
        ok_script(),
    );
    // The fallback cannot be engine-derived per table (it repeats one
    // answer); serve the two tables' answers explicitly — the
    // ingestion asks in sorted order: `_surform_migrations` filtered,
    // so `extra` then `user`.
    let extra_info: Value = db.query("INFO FOR TABLE extra").await?.take(0)?;
    let user_info: Value = db.query("INFO FOR TABLE user").await?.take(0)?;
    revert_mock.push_script(Scripted {
        status: 200,
        body: format!("[{}]", select_element(&value_to_json(&extra_info))),
    });
    revert_mock.push_script(Scripted {
        status: 200,
        body: format!("[{}]", select_element(&value_to_json(&user_info))),
    });
    let assert = cli()
        .current_dir(dir_path)
        .args([
            "migrate",
            "revert",
            "--endpoint",
            &revert_mock.endpoint(),
            "--ns",
            "test",
            "--db",
            "scratch",
            "--to",
            "1",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())?;
    assert!(
        stdout.contains("2 unresolved SURFORM SLOT region(s)"),
        "{stdout}"
    );
    let draft_path = dir_path.join("migrations/0003_revert-to-0001-v1.surql");
    let sidecar_path = dir_path.join("migrations/0003_revert-to-0001-v1.surdl");
    let draft_three = std::fs::read_to_string(&draft_path)?;
    assert!(
        draft_three.contains("<<<<<<< SURFORM SLOT (db:test/scratch: table extra)")
            && draft_three.contains("<<<<<<< SURFORM SLOT (db:test/scratch: field name ON user)"),
        "both removal slots ride: {draft_three}"
    );
    assert!(
        !draft_three.contains("_surform_migrations"),
        "the ledger table is invisible to the compare: {draft_three}"
    );
    // The sidecar is the recorded v1 — the ledger's CANONICAL form,
    // verbatim (commit stored canonical SDL, not the authored bytes).
    let sidecar = std::fs::read_to_string(&sidecar_path)?;
    assert_eq!(sidecar, row_one_sdl, "the recorded state rides verbatim");

    // Fill the slots the way the user does. v1 has no home for
    // `name` — and the fill runs BEFORE its removal (the slot sits
    // before it in the script), while the schemafull field still
    // coerces — so an `UPDATE … UNSET name` would be rejected
    // (NONE is not a string) and the honest fill here is the delete:
    // the data goes, which is exactly the destruction the slot warns
    // about. Same for the table slot.
    let filled = fill(
        &draft_three,
        &[
            ("db:test/scratch: table extra", "DELETE extra;"),
            ("db:test/scratch: field name ON user", "DELETE user:1;"),
        ],
    );
    assert!(
        !filled.contains("SURFORM SLOT"),
        "the fill resolves every region: {filled}"
    );
    std::fs::write(&draft_path, &filled)?;
    cli()
        .current_dir(dir_path)
        .args([
            "migrate",
            "commit",
            "migrations/0003_revert-to-0001-v1.surql",
            "-m",
            "revert to v1",
        ])
        .assert()
        .success();
    // The committed file is what push ships (header + the spliced
    // ledger block included).
    let committed_three = std::fs::read_to_string(&draft_path)?;
    assert!(
        !committed_three.contains("SURFORM SLOT"),
        "the committed file carries no markers"
    );

    // push: the mock carries the wire (the ledger SELECT answers the
    // engine's rows 1–2, so 0003 is the one pending apply); the
    // captured POST body executes on the engine — the same bytes the
    // server would run.
    let pre_ledger: Vec<Value> = db
        .query("SELECT number, name, hash FROM _surform_migrations ORDER BY number;")
        .await?
        .take(0)?;
    assert_eq!(pre_ledger.len(), 2);
    let push_mock = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!(
                "[{}]",
                select_element(&format!(
                    "[{}]",
                    pre_ledger
                        .iter()
                        .map(|row| ledger_row_sdl(
                            engine_row(row, "number").parse().unwrap(),
                            &engine_row(row, "name"),
                            &engine_row(row, "hash"),
                            "",
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                ))
            ),
        }],
        ok_script(),
    );
    cli()
        .current_dir(dir_path)
        .args([
            "migrate",
            "push",
            "--endpoint",
            &push_mock.endpoint(),
            "--ns",
            "test",
            "--db",
            "scratch",
        ])
        .assert()
        .success();
    let requests = push_mock.requests();
    assert_eq!(requests.len(), 2, "the SELECT + the one pending apply");
    let applied_body = requests[1].body.clone();
    assert_eq!(
        applied_body, committed_three,
        "push POSTs the committed file's text"
    );
    let mut response = db.query(&applied_body).await?;
    let errors = response.take_errors();
    if !errors.is_empty() {
        for (at, error) in &errors {
            eprintln!("statement {at} failed: {error}");
        }
        panic!("the revert apply failed (the failing statements above)");
    }

    // THE VERDICT: the database is back at v1's schema.
    let info: Value = db.query("INFO FOR DB").await?.take(0)?;
    let info = info.to_sql();
    assert!(info.contains("user"), "the v1 table stands: {info}");
    assert!(!info.contains("extra"), "the v2-only table is gone: {info}");
    let user: Value = db.query("INFO FOR TABLE user").await?.take(0)?;
    let user = user.to_sql();
    assert!(user.contains("email"), "{user}");
    assert!(
        !user.contains("DEFINE FIELD name"),
        "the v2-only field is gone: {user}"
    );
    // The data slots ran honestly: the fill chose the delete (v1 has
    // no home for `name`), so the row is gone with the field's
    // values; the user TABLE stands at v1's shape — and `extra` is
    // REMOVED, so a SELECT on it errors (the catalog-level proof).
    let rows: Vec<Value> = db.query("SELECT * FROM user;").await?.take(0)?;
    assert!(rows.is_empty(), "the fill's DELETE ran: {rows:?}");
    let extra_gone = match db.query("SELECT * FROM extra;").await {
        Err(_) => true,
        Ok(response) => response.check().is_err(),
    };
    assert!(extra_gone, "the removed table no longer answers");

    // The ledger has all THREE records — and record 3's revert
    // substrate is byte-identical to record 1's (the sidecar
    // re-loaded to the same canonical state: reverting the revert
    // works).
    let final_ledger: Vec<Value> = db
        .query("SELECT number, name, hash, sdl FROM _surform_migrations ORDER BY number;")
        .await?
        .take(0)?;
    assert_eq!(final_ledger.len(), 3, "all three records: {final_ledger:?}");
    assert_eq!(engine_row(&final_ledger[0], "name"), "v1");
    assert_eq!(engine_row(&final_ledger[1], "name"), "v2");
    assert_eq!(engine_row(&final_ledger[2], "name"), "revert-to-0001-v1");
    assert_eq!(
        engine_row(&final_ledger[2], "sdl"),
        engine_row(&final_ledger[0], "sdl"),
        "record 3's substrate is record 1's state — the fixed point"
    );
    assert_ne!(
        engine_row(&final_ledger[2], "hash"),
        engine_row(&final_ledger[0], "hash"),
        "the revert is its own migration (a different script, a different hash)"
    );

    // And the pipeline's own status agrees: three applied, exit 0.
    let status_mock = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!(
                "[{}]",
                select_element(&format!(
                    "[{}]",
                    final_ledger
                        .iter()
                        .map(|row| ledger_row_sdl(
                            engine_row(row, "number").parse().unwrap(),
                            &engine_row(row, "name"),
                            &engine_row(row, "hash"),
                            &engine_row(row, "sdl"),
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                ))
            ),
        }],
        ok_script(),
    );
    let assert = cli()
        .current_dir(dir_path)
        .args([
            "migrate",
            "status",
            "--endpoint",
            &status_mock.endpoint(),
            "--ns",
            "test",
            "--db",
            "scratch",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())?;
    assert!(
        stdout.contains("3 migration file(s): 3 applied"),
        "{stdout}"
    );
    Ok(())
}
