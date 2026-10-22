//! The `migrate status` suite: the HTTP boundary is
//! MOCKED — the same hand-rolled `/sql` server push's suite uses —
//! and the legs pin the ledger read (ONE request: the narrow
//! projection push reads, the fresh-database branch, the error
//! surface), the four-state classification plus the ledger-only
//! missing state, and the exit rule (0 consistent, 1 on any DRIFTED
//! or MISSING). Status never applies anything: no request beyond the
//! SELECT, ever.

#[path = "common/mod.rs"]
mod common;
#[path = "common/mock_sql.rs"]
mod mock_sql;

use assert_cmd::Command;
use mock_sql::{
    MockSql, Scripted, err_element, ledger_row, missing_table_element, ok_element, select_element,
};
use std::path::{Path, PathBuf};

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
    let dir = std::env::temp_dir().join(format!("surform-n3-status-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the workdir creates");
    dir.to_string_lossy().into_owned()
}

/// A committed migration file, built through the REAL lib machinery —
/// the same builder push's suite pins (the hash re-derivation is the
/// builder's own sanity check).
fn committed_file(
    dir: &Path,
    number: u32,
    name: &str,
    script: &str,
    message: &str,
    ledger_table: &str,
) -> (PathBuf, String) {
    use surform::migrate::{
        filled_script, ledger_statements, parse_draft, sha256_hex, splice_before_commit,
    };
    let body = format!("BEGIN TRANSACTION;\n{script}COMMIT;\n");
    let hash = sha256_hex(filled_script(&body).as_bytes());
    let block = ledger_statements(
        ledger_table,
        number,
        name,
        &hash,
        message,
        "namespace test;\n",
    );
    let new_body = splice_before_commit(&body, &block).expect("the body carries COMMIT");
    let text = format!(
        "-- surform migration {number:04} — {name}\n\
         -- generated: 2026-10-08T00:00:00Z\n\
         -- source: v2.surdl\n\
         -- input: info\n\
         -- status: committed sha256:{hash} {message}\n\
         {new_body}"
    );
    let draft = parse_draft(&text).expect("the built file parses");
    let committed = draft
        .header
        .committed
        .as_ref()
        .expect("the header is committed");
    assert_eq!(
        committed.hash,
        sha256_hex(filled_script(&draft.body).as_bytes()),
        "the recorded hash re-derives"
    );
    let path = dir.join(format!("{number:04}_{name}.surql"));
    std::fs::write(&path, &text).expect("the committed file writes");
    (path, hash)
}

/// A status run against the mock.
fn status(endpoint: &str, dir: &Path) -> Command {
    let mut command = cli();
    command
        .args([
            "migrate",
            "status",
            "--endpoint",
            endpoint,
            "--ns",
            "test",
            "--db",
            "scratch",
            "--dir",
        ])
        .arg(dir);
    command
}

/// The offline run (issue #77): status with NO endpoint — flag or
/// environment — classifies locally only (draft and committed per
/// file, the ledger-derived state unknown), exits 0 (nothing can be
/// DRIFTED without a ledger), and never dials: there is no mock to
/// fail the assertion if it did.
#[test]
fn offline_status_classifies_locally_and_never_dials() {
    let dir = workdir("offline");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    committed_file(
        &migrations,
        1,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "m1",
        "_surform_migrations",
    );
    std::fs::write(
        migrations.join("0002_work-in-progress.surql"),
        "-- surform migration 0002 — work-in-progress\n\
         -- generated: 2026-10-08T00:00:00Z\n\
         -- source: v2.surdl\n\
         -- input: info\n\
         -- status: draft\n\
         BEGIN TRANSACTION;\n\
         COMMIT;\n",
    )
    .unwrap();
    let assert = cli()
        .args(["migrate", "status", "--dir"])
        .arg(&migrations)
        .env_remove("SURFORM_ENDPOINT")
        .env_remove("SURFORM_NS")
        .env_remove("SURFORM_DB")
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("0001_add-users")
            && stdout.contains("committed — ledger state unknown (no endpoint)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("0002_work-in-progress") && stdout.contains("draft (commit it first)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("(offline — the ledger was not read, no endpoint)"),
        "{stdout}"
    );
}

/// The connection coordinates fall back to the environment (issue
/// #76): status with no coordinate flags reads the env triple.
#[test]
fn the_connection_falls_back_to_the_environment() {
    let dir = workdir("env-coordinates");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let (_file, hash) = committed_file(
        &migrations,
        1,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "m1",
        "_surform_migrations",
    );
    let mock = MockSql::start(
        vec![ledger_answer(&[ledger_row(1, "add-users", &hash)])],
        ledger_answer(&[ledger_row(1, "add-users", &hash)]),
    );
    let assert = cli()
        .args(["migrate", "status", "--dir"])
        .arg(&migrations)
        .env("SURFORM_ENDPOINT", mock.endpoint())
        .env("SURFORM_NS", "test")
        .env("SURFORM_DB", "scratch")
        .assert()
        .success();
    let select = &mock.requests()[0];
    assert_eq!(select.header("surreal-ns"), Some("test"));
    assert_eq!(select.header("surreal-db"), Some("scratch"));
    assert_eq!(
        select.body,
        "SELECT number, name, hash FROM _surform_migrations;"
    );
    // The stdout: the applied verdict from the env-sourced read.
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("0001_add-users"), "{stdout}");
    assert!(stdout.contains("applied"), "{stdout}");
}

fn ledger_answer(rows: &[String]) -> Scripted {
    Scripted {
        status: 200,
        body: format!("[{}]", select_element(&format!("[{}]", rows.join(",")))),
    }
}

const USER: &str = "statususer";
const PASS: &str = "statuspass-secret";

fn status_with_creds(endpoint: &str, dir: &Path) -> Command {
    let mut command = status(endpoint, dir);
    command.args(["--user", USER, "--pass", PASS]);
    command
}

/// The request shape, pinned: ONE request — `POST <root>/sql`, the
/// contract's headers with the Basic spelling, the ledger SELECT's
/// narrow projection in the body — and the applied row reported.
#[test]
fn the_request_shape_pins_the_contract() {
    let dir = workdir("shape");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let (_a, hash_a) = committed_file(
        &migrations,
        1,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "add the users",
        "_surform_migrations",
    );
    let mock = MockSql::start(
        vec![ledger_answer(&[ledger_row(1, "add-users", &hash_a)])],
        Scripted {
            status: 500,
            body: "unreachable".to_string(),
        },
    );
    let assert = status_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("== status =="), "{stdout}");
    assert!(
        stdout.contains("0001_add-users") && stdout.contains("applied"),
        "{stdout}"
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "status makes exactly ONE request");
    let select = &requests[0];
    assert_eq!(select.method, "POST");
    assert_eq!(select.path, "/sql");
    assert_eq!(select.header("accept"), Some("application/json"));
    assert_eq!(select.header("surreal-ns"), Some("test"));
    assert_eq!(select.header("surreal-db"), Some("scratch"));
    assert_eq!(
        select.header("authorization"),
        Some(
            format!(
                "Basic {}",
                surform::migrate::base64::encode(format!("{USER}:{PASS}").as_bytes())
            )
            .as_str()
        )
    );
    assert_eq!(
        select.body, "SELECT number, name, hash FROM _surform_migrations;",
        "the narrow projection — the sdl column stays off status's wire"
    );
}

/// The bearer credential (issue #99) rides status's ONE request too —
/// flag or environment (`SURFORM_DB_TOKEN`) land the same
/// `Authorization: Bearer` spelling.
#[test]
fn the_bearer_token_rides_the_ledger_read() {
    let dir = workdir("token");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let (_a, hash_a) = committed_file(
        &migrations,
        1,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "m1",
        "_surform_migrations",
    );
    let script = || {
        MockSql::start(
            vec![ledger_answer(&[ledger_row(1, "add-users", &hash_a)])],
            Scripted {
                status: 500,
                body: "unreachable".to_string(),
            },
        )
    };
    // The flag form.
    let mock = script();
    status(&mock.endpoint(), &migrations)
        .args(["--token", "status-jwt"])
        .assert()
        .success();
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some("Bearer status-jwt"),
        "the flag token rides the ledger SELECT"
    );
    // The env-var form.
    let mock = script();
    cli()
        .args(["migrate", "status", "--dir"])
        .arg(&migrations)
        .env("SURFORM_ENDPOINT", mock.endpoint())
        .env("SURFORM_NS", "test")
        .env("SURFORM_DB", "scratch")
        .env("SURFORM_DB_TOKEN", "status-jwt")
        .assert()
        .success();
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some("Bearer status-jwt"),
        "the env token rides the ledger SELECT"
    );
}

/// The classification: applied / pending / draft / DRIFTED per file,
/// plus the ledger-only MISSING row — and the exit rule: any drifted
/// or missing is exit 1.
#[test]
fn the_states_classify_and_the_exit_rule_fires() {
    let dir = workdir("states");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let (_a, hash_a) = committed_file(
        &migrations,
        1,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "m1",
        "_surform_migrations",
    );
    let (_b, _hash_b) = committed_file(
        &migrations,
        2,
        "add-indexes",
        "DEFINE INDEX i ON `user` FIELDS name;\n",
        "m2",
        "_surform_migrations",
    );
    let (path_c, hash_c) = committed_file(
        &migrations,
        3,
        "fix-thing",
        "DEFINE TABLE `audit`;\n",
        "m3",
        "_surform_migrations",
    );
    // The drift: a post-commit edit to 0003 (its hash moves off both
    // the header's and the ledger's).
    let text = std::fs::read_to_string(&path_c).unwrap();
    std::fs::write(
        &path_c,
        text.replace("DEFINE TABLE `audit`;", "DEFINE TABLE `audit2`;"),
    )
    .unwrap();
    // 0004: an uncommitted draft.
    std::fs::write(
        migrations.join("0004_work-in-progress.surql"),
        "-- surform migration 0004 — work-in-progress\n\
         -- generated: 2026-10-08T00:00:00Z\n\
         -- source: v2.surdl\n\
         -- input: info\n\
         -- status: draft\n\
         BEGIN TRANSACTION;\n\
         COMMIT;\n",
    )
    .unwrap();
    let other = "f".repeat(64);
    let mock = MockSql::start(
        vec![ledger_answer(&[
            ledger_row(1, "add-users", &hash_a),
            ledger_row(3, "fix-thing", &other),
            ledger_row(9, "lost-migration", &hash_c),
        ])],
        Scripted {
            status: 500,
            body: "unreachable".to_string(),
        },
    );
    let assert = status(&mock.endpoint(), &migrations).assert().failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("0001_add-users") && stdout.contains("applied"),
        "{stdout}"
    );
    assert!(
        stdout.contains("0002_add-indexes") && stdout.contains("pending"),
        "{stdout}"
    );
    assert!(
        stdout.contains("0003_fix-thing") && stdout.contains("DRIFTED"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&other[..12]) && stdout.contains("sha256:"),
        "both hashes named on the drifted row: {stdout}"
    );
    assert!(
        stdout.contains("0004_work-in-progress") && stdout.contains("draft (commit it first)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("0009_lost-migration") && stdout.contains("in the ledger, no local file"),
        "{stdout}"
    );
    assert!(
        stdout.contains("; 1 ledger row(s) without a local file"),
        "the summary carries the missing count: {stdout}"
    );
    assert_eq!(mock.requests().len(), 1, "classification is local");
}

/// Consistent states — applied, pending, draft — exit 0: the CI gate
/// gates integrity, not progress.
#[test]
fn consistent_states_exit_zero() {
    let dir = workdir("consistent");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let (_a, hash_a) = committed_file(
        &migrations,
        1,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "m1",
        "_surform_migrations",
    );
    committed_file(
        &migrations,
        2,
        "add-indexes",
        "DEFINE INDEX i ON `user` FIELDS name;\n",
        "m2",
        "_surform_migrations",
    );
    let mock = MockSql::start(
        vec![ledger_answer(&[ledger_row(1, "add-users", &hash_a)])],
        Scripted {
            status: 500,
            body: "unreachable".to_string(),
        },
    );
    let assert = status(&mock.endpoint(), &migrations).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("2 migration file(s): 1 applied, 1 pending"),
        "{stdout}"
    );
}

/// An empty directory with an empty ledger is the vacuous consistent
/// case — exit 0; with ledger rows it is the all-missing case —
/// exit 1.
#[test]
fn empty_dirs_classify() {
    let dir = workdir("empty");
    let empty = Path::new(&dir).join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let mock = MockSql::start(
        vec![ledger_answer(&[])],
        Scripted {
            status: 500,
            body: "unreachable".to_string(),
        },
    );
    let assert = status(&mock.endpoint(), &empty).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("no migration files in") && stdout.contains("the ledger holds no records"),
        "{stdout}"
    );
    // The all-missing spelling: status ALWAYS reads the ledger side —
    // even from an empty directory (the push rule — empty never
    // dials — is an apply-loop rule, not a report rule).
    let rows = MockSql::start(
        vec![ledger_answer(&[ledger_row(
            2,
            "elsewhere",
            &"a".repeat(64),
        )])],
        Scripted {
            status: 500,
            body: "unreachable".to_string(),
        },
    );
    let assert = status(&rows.endpoint(), &empty).assert().failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("0002_elsewhere") && stdout.contains("no local file"),
        "{stdout}"
    );
}

/// The fresh-database branch: the missing-TABLE ERR (kind NotFound,
/// details.kind Table) is the empty ledger — every file pending,
/// exit 0.
#[test]
fn the_fresh_database_reads_as_an_empty_ledger() {
    let dir = workdir("fresh");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    committed_file(
        &migrations,
        1,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "m1",
        "_surform_migrations",
    );
    let mock = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!("[{}]", missing_table_element("_surform_migrations")),
        }],
        Scripted {
            status: 500,
            body: "unreachable".to_string(),
        },
    );
    let assert = status(&mock.endpoint(), &migrations).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("0001_add-users") && stdout.contains("pending"),
        "{stdout}"
    );
    assert!(
        stdout.contains("1 migration file(s): 1 pending"),
        "{stdout}"
    );
}

/// A ledger read that is NOT the fresh-database case refuses with the
/// engine's message — a permission failure is never a silent "nothing
/// applied"; a wrong --ns/--db likewise (the NotFound subjects the
/// push suite pins).
#[test]
fn ledger_read_failures_refuse() {
    let dir = workdir("ledgerread");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    committed_file(
        &migrations,
        1,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "m1",
        "_surform_migrations",
    );
    let stderr_of = |assert: assert_cmd::assert::Assert| {
        String::from_utf8(assert.get_output().stderr.clone()).unwrap()
    };
    let denied = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!(
                "[{}]",
                err_element(
                    "NotAllowed",
                    Some("Target"),
                    "Not enough permissions to select"
                )
            ),
        }],
        Scripted {
            status: 500,
            body: "unreachable".to_string(),
        },
    );
    let assert = status(&denied.endpoint(), &migrations).assert().failure();
    assert!(
        stderr_of(assert).contains("reading the migration ledger failed — Not enough permissions"),
        "the engine's message rides"
    );
    let wrong = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!(
                "[{}]",
                err_element(
                    "NotFound",
                    Some("Namespace"),
                    "The namespace 'does-not-exist' does not exist"
                )
            ),
        }],
        Scripted {
            status: 500,
            body: "unreachable".to_string(),
        },
    );
    let assert = status(&wrong.endpoint(), &migrations).assert().failure();
    let stderr = stderr_of(assert);
    assert!(
        stderr.contains("reading the migration ledger failed"),
        "{stderr}"
    );
    assert!(stderr.contains("does-not-exist"), "{stderr}");
    assert_eq!(
        wrong.requests().len(),
        1,
        "nothing but the SELECT — status never applies"
    );
}

/// Local refusals happen before any request: a missing directory (a
/// pipeline pointed at nothing, push's rule), duplicate numbers (the
/// order/identity would be ambiguous).
#[test]
fn local_refusals_never_dial() {
    let dir = workdir("local");
    let missing = Path::new(&dir).join("nowhere");
    let mock = MockSql::start(
        vec![],
        Scripted {
            status: 500,
            body: "unreachable".to_string(),
        },
    );
    let assert = status(&mock.endpoint(), &missing).assert().failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("does not exist"), "{stderr}");
    assert_eq!(mock.requests().len(), 0, "the missing dir never dials");

    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    committed_file(
        &migrations,
        1,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "m1",
        "_surform_migrations",
    );
    committed_file(
        &migrations,
        1,
        "add-users-again",
        "DEFINE TABLE other;\n",
        "m1b",
        "_surform_migrations",
    );
    let assert = status(&mock.endpoint(), &migrations).assert().failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("two drafts carry number 0001"), "{stderr}");
    assert_eq!(mock.requests().len(), 0);
}

/// A response that does not spell the documented shapes refuses with
/// the malformed detail — never a panic.
#[test]
fn malformed_responses_refuse() {
    let dir = workdir("malformed");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    committed_file(
        &migrations,
        1,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "m1",
        "_surform_migrations",
    );
    for body in [
        "",
        "not json",
        "[]",
        r#"[{"status": 7}]"#,
        r#"[{"status":"OK"}]"#,
    ] {
        let mock = MockSql::start(
            vec![Scripted {
                status: 200,
                body: body.to_string(),
            }],
            Scripted {
                status: 500,
                body: "unreachable".to_string(),
            },
        );
        let assert = status(&mock.endpoint(), &migrations).assert().failure();
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        assert!(
            stderr.contains("malformed") || stderr.contains("not a JSON array"),
            "body {body:?} refuses with the detail: {stderr}"
        );
        assert!(!stderr.contains("panicked"), "no panic on {body:?}");
    }
    let _ = ok_element();
}

// ---------------------------------------------------------------------
// Draft groups (issue #87): the members' verdicts are driven by the
// GROUP's row through the recording (last) member.
// ---------------------------------------------------------------------

/// A draft-group member file, the same builder push's suite pins: the
/// shared header identity, the `-- group:` line, and — committed last
/// members only — the ledger block.
struct GroupMemberSpec<'a> {
    number: u32,
    letter: char,
    total: u32,
    name: &'a str,
    table: &'a str,
    committed: bool,
}

fn ok_script() -> Scripted {
    Scripted {
        status: 200,
        body: format!("[{}]", ok_element()),
    }
}

fn group_member(dir: &Path, spec: GroupMemberSpec<'_>, ledger_table: &str) -> (PathBuf, String) {
    use surform::migrate::{
        filled_script, ledger_statements, parse_draft, sha256_hex, splice_before_commit,
    };
    let GroupMemberSpec {
        number,
        letter,
        total,
        name,
        table,
        committed,
    } = spec;
    let body = format!("BEGIN TRANSACTION;\nDEFINE TABLE `{table}`;\nCOMMIT;\n");
    let hash = sha256_hex(filled_script(&body).as_bytes());
    let ordinal = letter as u32 - b'a' as u32 + 1;
    let mut text = format!(
        "-- surform migration {number:04} — {name}\n\
         -- generated: 2026-10-08T00:00:00Z\n\
         -- source: v2.surdl\n\
         -- input: info\n\
         -- group: {number:04} {letter}/{total}\n"
    );
    if committed {
        text.push_str(&format!(
            "-- status: committed sha256:{hash} member {letter}\n"
        ));
    } else {
        text.push_str("-- status: draft\n");
    }
    text.push('\n');
    if committed && ordinal == total {
        let block = ledger_statements(
            ledger_table,
            number,
            name,
            &hash,
            "the group records once",
            "namespace test;\n",
        );
        text.push_str(&splice_before_commit(&body, &block).expect("the body carries COMMIT"));
    } else {
        text.push_str(&body);
    }
    if committed {
        parse_draft(&text).expect("the member parses");
    }
    let path = dir.join(format!("{number:04}{letter}_{name}.surql"));
    std::fs::write(&path, &text).expect("the member writes");
    (path, hash)
}

/// An applied group: the row holds the LAST member's hash — both
/// members render applied with their group position; nothing else is
/// reported (exit 0).
#[test]
fn an_applied_group_renders_grouped() {
    let dir = workdir("group-applied");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    group_member(
        &migrations,
        GroupMemberSpec {
            number: 5,
            letter: 'a',
            total: 2,
            name: "shop",
            table: "alpha",
            committed: true,
        },
        "_surform_migrations",
    );
    let (_, hash) = group_member(
        &migrations,
        GroupMemberSpec {
            number: 5,
            letter: 'b',
            total: 2,
            name: "shop",
            table: "beta",
            committed: true,
        },
        "_surform_migrations",
    );
    let mock = MockSql::start(
        vec![ledger_answer(&[ledger_row(5, "shop", &hash)])],
        ok_script(),
    );
    let assert = status(&mock.endpoint(), &migrations).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("0005a_shop") && stdout.contains("applied — group 0005 a/2"),
        "{stdout}"
    );
    assert!(
        stdout.contains("0005b_shop") && stdout.contains("applied — group 0005 b/2"),
        "{stdout}"
    );
    assert_eq!(mock.requests().len(), 1, "the SELECT only");
}

/// A pending group (committed, no row) renders pending on both
/// members; a DRIFTED recording member renders the last member
/// drifted (both hashes named) and the earlier member applied.
#[test]
fn a_drifted_group_names_the_recording_member() {
    let dir = workdir("group-drifted");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    group_member(
        &migrations,
        GroupMemberSpec {
            number: 5,
            letter: 'a',
            total: 2,
            name: "shop",
            table: "alpha",
            committed: true,
        },
        "_surform_migrations",
    );
    group_member(
        &migrations,
        GroupMemberSpec {
            number: 5,
            letter: 'b',
            total: 2,
            name: "shop",
            table: "beta",
            committed: true,
        },
        "_surform_migrations",
    );
    // No row: both pending.
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = status(&mock.endpoint(), &migrations).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_eq!(
        stdout.matches("pending — group 0005").count(),
        2,
        "{stdout}"
    );
    // A row with a DIFFERENT hash (another directory's group owns the
    // number): the recording member is the drifted one, the earlier
    // member applied — and the run is inconsistent (exit 1).
    let mock = MockSql::start(
        vec![ledger_answer(&[ledger_row(5, "shop", &"f".repeat(64))])],
        ok_script(),
    );
    let assert = status(&mock.endpoint(), &migrations).assert().failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("0005b_shop") && stdout.contains("DRIFTED"),
        "{stdout}"
    );
    assert!(
        stdout.contains("0005a_shop") && stdout.contains("applied"),
        "{stdout}"
    );
}

/// A partly-committed group: the draft members render draft, the
/// committed members pending (the row cannot be judged without the
/// recording member), exit 0 — and the offline run renders the same
/// files draft + committed, also exit 0.
#[test]
fn a_partly_committed_group_renders_per_member() {
    let dir = workdir("group-partial");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    group_member(
        &migrations,
        GroupMemberSpec {
            number: 5,
            letter: 'a',
            total: 2,
            name: "shop",
            table: "alpha",
            committed: true,
        },
        "_surform_migrations",
    );
    group_member(
        &migrations,
        GroupMemberSpec {
            number: 5,
            letter: 'b',
            total: 2,
            name: "shop",
            table: "beta",
            committed: false,
        },
        "_surform_migrations",
    );
    // Live, no row.
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = status(&mock.endpoint(), &migrations).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("0005a_shop") && stdout.contains("pending"),
        "{stdout}"
    );
    assert!(
        stdout.contains("0005b_shop") && stdout.contains("draft (commit it first)"),
        "{stdout}"
    );
    // Offline: the local classification only.
    let assert = cli()
        .args(["migrate", "status", "--dir"])
        .arg(&migrations)
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("0005a_shop") && stdout.contains("committed — ledger state unknown"),
        "{stdout}"
    );
    assert!(
        stdout.contains("0005b_shop") && stdout.contains("draft"),
        "{stdout}"
    );
}
