//! The `migrate push` suite: the HTTP boundary is
//! MOCKED — a hand-rolled `/sql` server on a localhost port
//! (`common/mock_sql.rs`) that records every request and answers from
//! the shapes the docs page and the 3.3.0 probe captured. The legs
//! pin the request SHAPE against the doc-verified contract, the apply
//! loop (number order, skip applied, stop on the first failure with
//! the how-far report), both auth spellings and the redaction
//! guarantee, the ledger SELECT parse (including the fresh-database
//! missing-table case), and the local refusals (missing/empty dir,
//! drift, duplicate numbers, a number the ledger already holds).
//! Stdout stability beyond these pins is the existing suites' job.

#[path = "common/mod.rs"]
mod common;
#[path = "common/mock_sql.rs"]
mod mock_sql;

use assert_cmd::Command;
use mock_sql::{
    MockSql, Scripted, err_element, json_string, ledger_row, missing_table_element, ok_element,
    select_element,
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
    let dir = std::env::temp_dir().join(format!("surform-n2-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the workdir creates");
    dir.to_string_lossy().into_owned()
}

/// A committed migration file, built through the REAL lib machinery —
/// the body, the ledger block (`ledger_statements`), the splice, the
/// hash (`filled_script` ∘ `sha256_hex`), the header — then a
/// round-trip assert (the parse re-derives the hash; push's drift
/// check would refuse a sloppy builder). Returns the path and the
/// recorded hash (the ledger-row fixtures skip on it).
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
    // The builder's own sanity: the file parses as committed and the
    // hash re-derives (the fixed point the drift check leans on).
    let draft = parse_draft(&text).expect("the built file parses");
    let committed = draft
        .header
        .committed
        .as_ref()
        .expect("the header is committed");
    assert_eq!(committed.hash, hash, "the recorded hash re-derives");
    assert_eq!(
        committed.hash,
        sha256_hex(filled_script(&draft.body).as_bytes())
    );
    let path = dir.join(format!("{number:04}_{name}.surql"));
    std::fs::write(&path, &text).expect("the committed file writes");
    (path, hash)
}

/// A push run against the mock.
fn push(endpoint: &str, dir: &Path) -> Command {
    let mut command = cli();
    command
        .args([
            "migrate",
            "push",
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

/// The all-OK one-element answer (the mock never counts statements;
/// the judge only reads statuses).
fn ok_script() -> Scripted {
    Scripted {
        status: 200,
        body: format!("[{}]", ok_element()),
    }
}

/// The script a failing apply gets: the 3.3.0 probe's transaction-
/// abort shape — the root cause (AlreadyExists/Kind Table) riding
/// AFTER the NotExecuted consequences, the COMMIT refusing.
fn failed_apply_script() -> Scripted {
    Scripted {
        status: 200,
        body: format!(
            "[{},{},{},{}]",
            ok_element(),
            err_element(
                "Query",
                Some("NotExecuted"),
                "The query was not executed due to a failed transaction"
            ),
            err_element(
                "AlreadyExists",
                Some("Table"),
                "The table 'user' already exists"
            ),
            err_element(
                "Query",
                Some("NotExecuted"),
                "Cannot COMMIT: the transaction was aborted due to a prior error"
            )
        ),
    }
}

fn ledger_answer(rows: &[String]) -> Scripted {
    Scripted {
        status: 200,
        body: format!("[{}]", select_element(&format!("[{}]", rows.join(",")))),
    }
}

const USER: &str = "pushuser";
const PASS: &str = "pushpass-secret";

/// The same run, with the flags' credential form.
fn push_with_creds(endpoint: &str, dir: &Path) -> Command {
    let mut command = push(endpoint, dir);
    command.args(["--user", USER, "--pass", PASS]);
    command
}

/// The request SHAPE, pinned against the doc-verified contract: POST
/// `<root>/sql`; `Accept: application/json`, `Surreal-NS`,
/// `Surreal-DB` (required), `Authorization: Basic base64(user:pass)`
/// (the docs' `-u` spelling); the ledger SELECT first — its projected
/// columns in the body — then one POST per pending file carrying the
/// file's FULL text (header comments and ledger block included).
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
    let (_b, hash_b) = committed_file(
        &migrations,
        2,
        "add-indexes",
        "DEFINE INDEX i ON `user` FIELDS name;\n",
        "index the users",
        "_surform_migrations",
    );
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("0002_add-indexes"), "{stdout}");
    let requests = mock.requests();
    assert_eq!(requests.len(), 3, "one SELECT + one POST per pending file");
    let select = &requests[0];
    assert_eq!(select.method, "POST");
    assert_eq!(select.path, "/sql");
    assert_eq!(select.header("accept"), Some("application/json"));
    assert_eq!(select.header("surreal-ns"), Some("test"));
    assert_eq!(select.header("surreal-db"), Some("scratch"));
    assert_eq!(
        select.header("authorization"),
        Some(format!("Basic {}", b64(&format!("{USER}:{PASS}"))).as_str()),
        "the Basic header is the docs' `-u` spelling"
    );
    assert_eq!(
        select.body,
        "SELECT number, name, hash FROM _surform_migrations;"
    );
    // The applies ride in NUMBER order: 0001 then 0002, each the
    // file's full text.
    let first_apply = &requests[1];
    assert_eq!(first_apply.path, "/sql");
    let file_text = std::fs::read_to_string(migrations.join("0001_add-users.surql")).unwrap();
    assert_eq!(
        first_apply.body, file_text,
        "the POST body is the file's full text"
    );
    let second_apply = &requests[2];
    assert_eq!(
        second_apply.body,
        std::fs::read_to_string(migrations.join("0002_add-indexes.surql")).unwrap()
    );
    let _ = (hash_a, hash_b);
}

/// ASCII base64 (RFC 4648) — the tests' own expected-header spelling.
fn b64(bytes: &str) -> String {
    surform::migrate::base64::encode(bytes.as_bytes())
}

/// The apply loop: number order, skip applied, STOP on the first
/// failure — the later pending file is never attempted — and the
/// report carries the engine's ROOT CAUSE (the probed shape buries it
/// behind NotExecuted consequences) plus how far the run got.
#[test]
fn order_skip_stop_and_the_how_far_report() {
    let dir = workdir("stop");
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
    let (_c, _hash_c) = committed_file(
        &migrations,
        3,
        "breaks",
        "DEFINE TABLE `user`;\n",
        "m3",
        "_surform_migrations",
    );
    let _d = committed_file(
        &migrations,
        4,
        "never-attempted",
        "DEFINE TABLE fourth;\n",
        "m4",
        "_surform_migrations",
    );
    let mock = MockSql::start(
        vec![
            ledger_answer(&[ledger_row(1, "add-users", &hash_a)]),
            ok_script(),
            failed_apply_script(),
        ],
        ok_script(),
    );
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stdout.contains("0001_add-users"),
        "the applied row: {stdout}"
    );
    assert!(stdout.contains("already applied"), "{stdout}");
    assert!(stdout.contains("0002_add-indexes"), "{stdout}");
    assert!(stdout.contains("0003_breaks"), "{stdout}");
    assert!(
        !stdout.contains("0004_never-attempted"),
        "the stop stops BEFORE the next file: {stdout}"
    );
    assert!(
        stderr.contains("0003_breaks failed — The table 'user' already exists"),
        "the engine's root cause rides the report: {stderr}"
    );
    assert!(
        stderr.contains("applied 1 of 3 committed migration(s)"),
        "the how-far count: {stderr}"
    );
    assert!(
        stderr.contains("rolled back"),
        "the file's atomicity is stated: {stderr}"
    );
    // One SELECT, the 0002 apply, the 0003 apply — and nothing after.
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1].body,
        std::fs::read_to_string(migrations.join("0002_add-indexes.surql")).unwrap()
    );
    assert_eq!(
        requests[2].body,
        std::fs::read_to_string(migrations.join("0003_breaks.surql")).unwrap()
    );
}

/// Re-run after a failure resumes correctly: once 0003 is fixed (the
/// ledger records the earlier files), the re-run SELECTs, skips
/// 0001–0003 by the ledger, and applies only what remains.
#[test]
fn rerun_after_failure_resumes() {
    let dir = workdir("resume");
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
    let (_b, hash_b) = committed_file(
        &migrations,
        2,
        "add-indexes",
        "DEFINE INDEX i ON `user` FIELDS name;\n",
        "m2",
        "_surform_migrations",
    );
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .success();
    // The database now holds both rows (each file self-recorded).
    mock.push_script(ledger_answer(&[
        ledger_row(1, "add-users", &hash_a),
        ledger_row(2, "add-indexes", &hash_b),
    ]));
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("nothing to apply"), "{stdout}");
    let requests = mock.requests();
    // Run one: SELECT + two applies. Run two: the SELECT alone —
    // both files skip by the ledger, nothing POSTs.
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[3].body,
        "SELECT number, name, hash FROM _surform_migrations;"
    );
}

/// The redaction guarantee: the password (and its Basic encoding)
/// never appear in any output on the success path — or the failure
/// path, where the server's own error body DOES ride through (server
/// text, not credential bytes). The env-var credential form lands the
/// same header the flag form does.
#[test]
fn credentials_stay_out_of_every_output() {
    let dir = workdir("redact");
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
    // Success path: nothing about the credentials prints.
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap()
        + &String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        !output.contains(PASS),
        "the password never prints: {output}"
    );
    assert!(
        !output.contains(&b64(&format!("{USER}:{PASS}"))),
        "the Basic secret never prints"
    );
    assert!(!output.contains(USER), "the user never prints either");
    // Failure path: the 401's plain-text body rides, the credentials
    // still do not.
    let refusing = MockSql::start(
        vec![],
        Scripted {
            status: 401,
            body: "The password did not verify".to_string(),
        },
    );
    let assert = push_with_creds(&refusing.endpoint(), &migrations)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("HTTP 401") && stderr.contains("The password did not verify"),
        "the server's answer surfaces: {stderr}"
    );
    assert!(!stderr.contains(PASS) && !stderr.contains(USER), "{stderr}");
    // The env-var form rides the child's environment (the .env call —
    // never the test process's).
    let env_mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push(&env_mock.endpoint(), &migrations)
        .env("SURFORM_DB_USER", USER)
        .env("SURFORM_DB_PASS", PASS)
        .assert()
        .success();
    assert_eq!(
        env_mock.requests()[0].header("authorization"),
        Some(format!("Basic {}", b64(&format!("{USER}:{PASS}"))).as_str()),
        "env credentials land the same Basic header"
    );
    let _ = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
}

/// Half a credential is a refusal, flag or environment — and the
/// unauthenticated form (a server run with `--unauthenticated`)
/// sends NO Authorization header at all.
#[test]
fn credential_arity_and_the_anonymous_form() {
    let dir = workdir("arity");
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
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let stderr_of = |assert: assert_cmd::assert::Assert| {
        String::from_utf8(assert.get_output().stderr.clone()).unwrap()
    };
    let half = push(&mock.endpoint(), &migrations)
        .args(["--user", USER])
        .assert()
        .failure();
    assert!(stderr_of(half).contains("without a password"));
    let half = push(&mock.endpoint(), &migrations)
        .args(["--pass", PASS])
        .assert()
        .failure();
    assert!(stderr_of(half).contains("without a user"));
    let half = push(&mock.endpoint(), &migrations)
        .env("SURFORM_DB_USER", USER)
        .assert()
        .failure();
    assert!(stderr_of(half).contains("without a password"));
    // Anonymous: no header at all.
    let anonymous = MockSql::start(vec![ledger_answer(&[])], ok_script());
    push(&anonymous.endpoint(), &migrations).assert().success();
    assert_eq!(
        anonymous.requests()[0].header("authorization"),
        None,
        "no credentials, no Authorization header"
    );
}

const TOKEN: &str = "push-jwt.bearer.token";

/// The bearer credential (issue #99): `--token` rides EVERY POST as
/// `Authorization: Bearer <token>`, the environment variable lands
/// the same header, a flag wins over its environment variable, and
/// the token never prints anywhere — the same guarantees the
/// user/password pair's legs pin above.
#[test]
fn the_bearer_token_rides_the_wire() {
    let dir = workdir("token");
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
    let output_of = |assert: assert_cmd::assert::Assert| {
        let output = assert.get_output();
        String::from_utf8(output.stdout.clone()).unwrap()
            + &String::from_utf8(output.stderr.clone()).unwrap()
    };
    // The flag form: the ledger SELECT and the apply both carry it.
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push(&mock.endpoint(), &migrations)
        .args(["--token", TOKEN])
        .assert()
        .success();
    for request in mock.requests() {
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str()),
            "the bearer token rides every POST: {}",
            request.body
        );
    }
    assert!(!output_of(assert).contains(TOKEN), "the token never prints");
    // The env-var form rides the child's environment.
    let env_mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    push(&env_mock.endpoint(), &migrations)
        .env("SURFORM_DB_TOKEN", TOKEN)
        .assert()
        .success();
    assert_eq!(
        env_mock.requests()[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str()),
        "the env token lands the same Bearer header"
    );
    // Precedence: a flag over its environment variable — the FLAG's
    // token rides, never the env one.
    let flag_mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push(&flag_mock.endpoint(), &migrations)
        .env("SURFORM_DB_TOKEN", "env-token")
        .args(["--token", TOKEN])
        .assert()
        .success();
    assert_eq!(
        flag_mock.requests()[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str()),
        "the flag token wins over the environment"
    );
    assert!(!output_of(assert).contains("env-token"));
}

/// The two credential kinds are exclusive: a token alongside a user
/// or password — flag or environment, either kind, a full pair or
/// half one — refuses before anything dials.
#[test]
fn the_token_and_basic_refuse_each_other() {
    let dir = workdir("token-basic");
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
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let stderr_of = |assert: assert_cmd::assert::Assert| {
        String::from_utf8(assert.get_output().stderr.clone()).unwrap()
    };
    for args in [
        vec!["--token", TOKEN, "--user", USER],
        vec!["--token", TOKEN, "--pass", PASS],
        vec!["--token", TOKEN, "--user", USER, "--pass", PASS],
    ] {
        let assert = push(&mock.endpoint(), &migrations)
            .args(&args)
            .assert()
            .failure();
        let stderr = stderr_of(assert);
        assert!(stderr.contains("exclusive"), "{stderr}");
        assert!(
            stderr.contains("SURFORM_DB_TOKEN") && stderr.contains("SURFORM_DB_USER"),
            "the refusal names both kinds' spellings: {stderr}"
        );
        assert!(!stderr.contains(TOKEN), "the token never prints: {stderr}");
    }
    // The environment kind conflicts too (a token env var over a user
    // flag), and a refusal never sends a request.
    let assert = push(&mock.endpoint(), &migrations)
        .env("SURFORM_DB_TOKEN", TOKEN)
        .args(["--user", USER])
        .assert()
        .failure();
    assert!(stderr_of(assert).contains("exclusive"));
    assert!(mock.requests().is_empty(), "a refusal never dials");
}

/// The fresh-database case: the ledger table does not exist yet — the
/// probed missing-table ERR (kind NotFound, details.kind Table) means
/// an EMPTY ledger, and every pending file applies.
#[test]
fn the_fresh_database_pushes_everything() {
    let dir = workdir("fresh");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let (_a, _hash_a) = committed_file(
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
    let mock = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!("[{}]", missing_table_element("_surform_migrations")),
        }],
        ok_script(),
    );
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("applied 2 migration file(s)"), "{stdout}");
    assert_eq!(mock.requests().len(), 3, "SELECT + two applies");
}

/// The ledger already holds the number under a DIFFERENT hash —
/// another migration owns it on this database (the
/// one-directory-per-database rule) — a fail-closed refusal naming
/// both hashes.
#[test]
fn a_number_the_ledger_holds_differently_refuses() {
    let dir = workdir("taken");
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
    let other = "f".repeat(64);
    assert_ne!(other, hash_a);
    let mock = MockSql::start(
        vec![ledger_answer(&[ledger_row(1, "someone-else", &other)])],
        ok_script(),
    );
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("number 0001"), "{stderr}");
    assert!(
        stderr.contains(&other) && stderr.contains(&hash_a),
        "both hashes named: {stderr}"
    );
    assert_eq!(mock.requests().len(), 1, "nothing applied past the refusal");
}

/// Drafts (uncommitted files) skip — commit declares history, push
/// ships it — while their committed siblings apply.
#[test]
fn drafts_skip_and_committed_siblings_apply() {
    let dir = workdir("drafts");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let (_a, _hash_a) = committed_file(
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
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("0002_work-in-progress"), "{stdout}");
    assert!(stdout.contains("skipped — draft"), "{stdout}");
    assert!(stdout.contains("0001_add-users"), "{stdout}");
    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "SELECT + the one committed apply");
    assert_eq!(
        requests[1].body,
        std::fs::read_to_string(migrations.join("0001_add-users.surql")).unwrap()
    );
}

/// A committed file edited after its commit refuses the WHOLE run
/// before anything is sent — the drift check is pre-flight.
#[test]
fn drift_refuses_before_anything_is_sent() {
    let dir = workdir("drift");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let (path_a, _hash_a) = committed_file(
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
    // The post-commit edit: one byte class inside the hashed region.
    let text = std::fs::read_to_string(&path_a).unwrap();
    std::fs::write(
        &path_a,
        text.replace("DEFINE TABLE `user`;", "DEFINE TABLE `users`;"),
    )
    .unwrap();
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("edited after its commit"), "{stderr}");
    assert!(stderr.contains("0001_add-users.surql"), "{stderr}");
    assert_eq!(
        mock.requests().len(),
        0,
        "the drift refusal never touches the network"
    );
}

/// A committed file whose CERTIFIED body already carries content
/// below its frame close — the defect path the drift check cannot
/// see, because the recorded hash covers the filled script INCLUDING
/// the tail (commit certified it). Built through the real lib
/// machinery with the tail inside the hashed body; the ledger block
/// splices before the body's LAST `COMMIT;` exactly as commit would.
fn tail_bearing_committed_file(dir: &Path, number: u32, name: &str, tail: &str) -> PathBuf {
    use surform::migrate::{
        filled_script, ledger_statements, parse_draft, sha256_hex, splice_before_commit,
    };
    let body = format!("BEGIN TRANSACTION;\nDEFINE TABLE `user`;\nCOMMIT;\n{tail}");
    let hash = sha256_hex(filled_script(&body).as_bytes());
    let block = ledger_statements(
        "_surform_migrations",
        number,
        name,
        &hash,
        "m1",
        "namespace test;\n",
    );
    let new_body = splice_before_commit(&body, &block).expect("the body carries COMMIT");
    let text = format!(
        "-- surform migration {number:04} — {name}\n\
         -- generated: 2026-10-08T00:00:00Z\n\
         -- source: v2.surdl\n\
         -- input: info\n\
         -- status: committed sha256:{hash} m1\n\
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
        "the drift check passes: the hash covers the tail"
    );
    let path = dir.join(format!("{number:04}_{name}.surql"));
    std::fs::write(&path, &text).expect("the committed file writes");
    path
}

/// The frame guard, end to end: a committed file carrying statements
/// OUTSIDE its one transaction frame refuses BEFORE anything is sent
/// (zero requests — the drift guard's pattern), the offending line
/// named. The engine runs the frame and the tail independently
/// (probed 3.3.0): a tail failure would leave the frame committed —
/// its self-recording ledger row surviving — under a false rollback
/// report, and the re-run would skip the file as already applied, so
/// the tail would never apply. Both spellings land: the appended tail
/// and the second `BEGIN`…`COMMIT` block.
#[test]
fn a_frame_with_trailing_statements_refuses_before_anything_is_sent() {
    let dir = workdir("trailing");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    // The appended-tail spelling.
    tail_bearing_committed_file(
        &migrations,
        1,
        "smuggles-a-tail",
        "DEFINE TABLE smuggled;\n",
    );
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("outside its one transaction frame")
            && stderr.contains("`DEFINE TABLE smuggled;`"),
        "{stderr}"
    );
    assert!(stderr.contains("committed under"), "{stderr}");
    assert_eq!(mock.requests().len(), 0, "the frame guard is pre-flight");
    // The second-block spelling: the ledger would splice into the
    // LAST commit — inside the SECOND block, outside the frame the
    // file certifies — so the FIRST `COMMIT;` anchors and the second
    // block's BEGIN is the named offender. (Its own directory: the
    // pre-flight refuses the FIRST offending file, and 0001 above is
    // still one.)
    let migrations2 = Path::new(&dir).join("migrations2");
    std::fs::create_dir_all(&migrations2).unwrap();
    let second = tail_bearing_committed_file(
        &migrations2,
        2,
        "carries-a-second-block",
        "BEGIN TRANSACTION;\nDEFINE TABLE second;\nCOMMIT;\n",
    );
    let assert = push_with_creds(&mock.endpoint(), &migrations2)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("0002_carries-a-second-block") && stderr.contains("`BEGIN TRANSACTION;`"),
        "the second block's BEGIN is the offender: {stderr}"
    );
    assert_eq!(mock.requests().len(), 0);
    let _ = second;
}

/// Local refusals: two files carrying one number refuse (the order
/// would be ambiguous), before any remote call.
#[test]
fn duplicate_numbers_refuse() {
    let dir = workdir("dupe");
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
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("two drafts carry number 0001"), "{stderr}");
    assert_eq!(mock.requests().len(), 0);
}

/// The USE guard, end to end: a committed script whose body carries
/// `USE NS other DB other;` refuses before any request — a body USE
/// would move the statements (and the ledger self-record) to the
/// other database while push reads the ledger from the headers'
/// (engine-probed; the live smoke caught it). A MATCHING USE passes.
#[test]
fn a_use_target_mismatch_refuses_before_anything_is_sent() {
    let dir = workdir("use");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    committed_file(
        &migrations,
        1,
        "targets-elsewhere",
        "USE NS other DB otherdb;\nDEFINE TABLE there;\n",
        "m1",
        "_surform_migrations",
    );
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("USE NS other DB otherdb;"), "{stderr}");
    assert!(stderr.contains("switches the session"), "{stderr}");
    assert_eq!(mock.requests().len(), 0, "the guard is pre-flight");
    // The matching spelling rides.
    let (here, _hash) = committed_file(
        &migrations,
        2,
        "targets-here",
        "USE NS test DB scratch;\nDEFINE TABLE here;\n",
        "m2",
        "_surform_migrations",
    );
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        !stderr.contains("0002_targets-here"),
        "the matching USE does not refuse: {stderr}"
    );
    let _ = here;
}

/// The guard covers the WHOLE file text, the frame's outside
/// included: the convergence emitter places each section's `USE`
/// lines ABOVE its transaction frame (the 2026-10-08 per-scope
/// amendment), and a pre-frame USE switches the HTTP session all the
/// same — a mismatched one refuses pre-flight exactly like the
/// in-frame spelling. (The committed hash covers the body — the
/// parse cuts it at the frame line — so the pre-frame line rides the
/// file text beside it, which is exactly where the emitter puts one.)
#[test]
fn a_pre_frame_use_target_mismatch_refuses_too() {
    let dir = workdir("use-preframe");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let (path, _hash) = committed_file(
        &migrations,
        1,
        "pre-frame-use",
        "DEFINE TABLE there;\n",
        "m1",
        "_surform_migrations",
    );
    // Splice the emitter-shaped USE above the frame line, committed
    // file's hash untouched (the body cut starts at BEGIN).
    let text = std::fs::read_to_string(&path).expect("the committed file reads");
    let spliced = text.replacen(
        "BEGIN TRANSACTION;",
        "USE NS other DB otherdb;\nBEGIN TRANSACTION;",
        1,
    );
    assert_ne!(text, spliced, "the splice found the frame line");
    std::fs::write(&path, spliced).expect("the spliced file writes");
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push_with_creds(&mock.endpoint(), &migrations)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("USE NS other DB otherdb;"), "{stderr}");
    assert!(stderr.contains("switches the session"), "{stderr}");
    assert_eq!(mock.requests().len(), 0, "the guard is pre-flight");
}

/// A missing directory is an error (a pipeline pointed at nothing);
/// an empty one is a clean no-op; foreign files are invisible.
#[test]
fn missing_and_empty_dirs() {
    let dir = workdir("dirs");
    let missing = Path::new(&dir).join("nowhere");
    let mock = MockSql::start(vec![], ok_script());
    let assert = push(&mock.endpoint(), &missing).assert().failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("does not exist"), "{stderr}");
    assert!(stderr.contains("nowhere"), "{stderr}");
    assert_eq!(mock.requests().len(), 0, "the missing dir never dials");
    let empty = Path::new(&dir).join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(empty.join("README.md"), "not a migration\n").unwrap();
    let assert = push(&mock.endpoint(), &empty).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("no migration files"), "{stdout}");
    assert_eq!(mock.requests().len(), 0);
}

/// The request-level failure layer: non-200 answers surface their
/// status and body verbatim (the probe's plain-text 401 and the
/// ledger read both ride this); a dead endpoint is a connection
/// error; endpoint spellings the tool cannot mean refuse up front.
#[test]
fn http_and_endpoint_errors_surface() {
    let dir = workdir("http");
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
    // The LEDGER read itself 401s (plain-text body, the probe's).
    let refusing = MockSql::start(
        vec![],
        Scripted {
            status: 401,
            body: "The password did not verify".to_string(),
        },
    );
    let assert = push_with_creds(&refusing.endpoint(), &migrations)
        .assert()
        .failure();
    assert!(
        stderr_of(assert).contains("the server answered HTTP 401 — The password did not verify")
    );
    // A dead port: the transport error, no panic — the URL it could
    // not reach is named (issue #90: connection failures name the
    // endpoint).
    let dead = format!("http://127.0.0.1:{}", pick_dead_port());
    let assert = push_with_creds(&dead, &migrations).assert().failure();
    let stderr = stderr_of(assert);
    assert!(stderr.contains("cannot reach"), "{stderr}");
    assert!(
        stderr.contains(&format!("{dead}/sql")),
        "the /sql surface is named: {stderr}"
    );
    assert!(stderr.contains("is the server up?"), "{stderr}");
    // The endpoint spelling the tool cannot mean: /sql already on the
    // path (the suffix is appended here), a non-http scheme, and a
    // `?`/`#` that would corrupt the appended /sql path.
    let assert = push_with_creds("http://host.example:8000/sql", &migrations)
        .assert()
        .failure();
    assert!(stderr_of(assert).contains("pass the server root"));
    let assert = push_with_creds("ftp://host.example", &migrations)
        .assert()
        .failure();
    assert!(stderr_of(assert).contains("http://"));
    let assert = push_with_creds("http://host.example:8000?a=b", &migrations)
        .assert()
        .failure();
    assert!(stderr_of(assert).contains("`?` or `#`"));
    let assert = push_with_creds("http://host.example:8000/#frag", &migrations)
        .assert()
        .failure();
    assert!(stderr_of(assert).contains("`?` or `#`"));
    // A padded endpoint: it would pass the scheme check on
    // the trimmed root and die in transport on the untrimmed URI — it
    // refuses up front with the named reason instead.
    let assert = push_with_creds(" http://host.example:8000", &migrations)
        .assert()
        .failure();
    assert!(stderr_of(assert).contains("whitespace"));
    let assert = push_with_creds("http://host.example:8000\t", &migrations)
        .assert()
        .failure();
    assert!(stderr_of(assert).contains("whitespace"));
}

/// The dry run (issue #77): the ENTIRE pre-flight and the plan with
/// ZERO migration POSTs — one ledger SELECT total, every verdict row
/// (already-applied / would-apply / skip-draft), and the nothing-sent
/// summary.
#[test]
fn dry_run_plans_without_sending_a_single_post() {
    let dir = workdir("dry-run");
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
    std::fs::write(
        migrations.join("0003_work-in-progress.surql"),
        "-- surform migration 0003 — work-in-progress\n\
         -- generated: 2026-10-08T00:00:00Z\n\
         -- source: v2.surdl\n\
         -- input: info\n\
         -- status: draft\n\
         BEGIN TRANSACTION;\n\
         COMMIT;\n",
    )
    .unwrap();
    let mock = MockSql::start(
        vec![ledger_answer(&[ledger_row(1, "add-users", &hash_a)])],
        ok_script(),
    );
    let assert = push(&mock.endpoint(), &migrations)
        .arg("--dry-run")
        .assert()
        .success();
    // The zero-POST property: ONE request, the SELECT — the pending
    // file's migration POST never happened.
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "one SELECT, zero migration POSTs");
    assert_eq!(
        requests[0].body,
        "SELECT number, name, hash FROM _surform_migrations;"
    );
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("== push (dry run — nothing was sent) =="),
        "{stdout}"
    );
    assert!(stdout.contains("0001_add-users"), "{stdout}");
    assert!(stdout.contains("already applied"), "{stdout}");
    assert!(stdout.contains("0002_add-indexes"), "{stdout}");
    assert!(stdout.contains("would apply"), "{stdout}");
    assert!(stdout.contains("0003_work-in-progress"), "{stdout}");
    assert!(stdout.contains("skipped — draft"), "{stdout}");
    assert!(
        stdout.contains("1 would apply, 1 already applied, 1 draft(s) skipped"),
        "the summary counts: {stdout}"
    );
    assert!(stdout.contains("zero statements sent"), "{stdout}");
}

/// The dry run's would-refuse verdict (issue #77): the ledger holds
/// the number with a DIFFERENT hash — the plan names the collision
/// (both hashes), the run exits 1 (the real push would refuse
/// there), and STILL nothing past the SELECT was sent.
#[test]
fn dry_run_reports_the_would_refuse_and_exits_one() {
    let dir = workdir("dry-run-refuse");
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
    let other_hash = "a".repeat(64);
    let mock = MockSql::start(
        vec![ledger_answer(&[ledger_row(1, "add-users", &other_hash)])],
        ok_script(),
    );
    let assert = push(&mock.endpoint(), &migrations)
        .arg("--dry-run")
        .assert()
        .code(1);
    assert_eq!(
        mock.requests().len(),
        1,
        "the would-refuse still sends nothing past the SELECT"
    );
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("would refuse"), "{stdout}");
    assert!(stdout.contains("the ledger records sha256:"), "{stdout}");
    assert!(stdout.contains("the file hashes sha256:"), "{stdout}");
}

/// A port with nothing listening (best effort: bind-and-drop leaves
/// it briefly reusable-dead).
fn pick_dead_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a port binds")
        .local_addr()
        .expect("an address")
        .port()
}

/// The connection coordinates fall back to the environment (issue
/// #76): endpoint, namespace, and database each ride their variable
/// when the flag is absent, the flag WINS over the environment when
/// both exist, and a missing coordinate refuses naming both ways.
#[test]
fn the_connection_falls_back_to_the_environment() {
    let dir = workdir("env-coordinates");
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
    // All three from the environment: no coordinate flags at all.
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    cli()
        .args(["migrate", "push", "--dir"])
        .arg(&migrations)
        .env("SURFORM_ENDPOINT", mock.endpoint())
        .env("SURFORM_NS", "test")
        .env("SURFORM_DB", "scratch")
        .assert()
        .success();
    let select = &mock.requests()[0];
    assert_eq!(select.header("surreal-ns"), Some("test"));
    assert_eq!(select.header("surreal-db"), Some("scratch"));
    // The flag wins over the environment: the env endpoint is a dead
    // port and the env names differ — success with the FLAG's headers
    // proves the precedence.
    let dead = format!("http://127.0.0.1:{}", pick_dead_port());
    let flag_mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    push(&flag_mock.endpoint(), &migrations)
        .env("SURFORM_ENDPOINT", &dead)
        .env("SURFORM_NS", "env-ns")
        .env("SURFORM_DB", "env-db")
        .assert()
        .success();
    assert_eq!(
        flag_mock.requests()[0].header("surreal-ns"),
        Some("test"),
        "the flag rode over the environment"
    );
    // A missing coordinate refuses, naming both ways (the variable
    // scrubbed — the parent environment must not supply it).
    let assert = cli()
        .args([
            "migrate",
            "push",
            "--endpoint",
            &mock.endpoint(),
            "--db",
            "scratch",
            "--dir",
        ])
        .arg(&migrations)
        .env_remove("SURFORM_NS")
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("no namespace — pass --ns or set SURFORM_NS"),
        "{stderr}"
    );
}

/// The response parser's other guard: a 200 whose body does not
/// spell the documented shapes refuses with the malformed detail —
/// never a panic, never a silent success.
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
    for body in ["", "not json", "{}", "[]", r#"[{"status": 7}]"#] {
        let mock = MockSql::start(
            vec![Scripted {
                status: 200,
                body: body.to_string(),
            }],
            ok_script(),
        );
        let assert = push_with_creds(&mock.endpoint(), &migrations)
            .assert()
            .failure();
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        assert!(
            stderr.contains("malformed") || stderr.contains("not a JSON array"),
            "body {body:?} refuses with the detail: {stderr}"
        );
        assert!(!stderr.contains("panicked"), "no panic on {body:?}");
    }
}

/// The statement-level ERR on the ledger SELECT with a kind the
/// fresh-database case does not cover is a real error, not an empty
/// ledger (the empty-table case itself — result [] — skips clean).
#[test]
fn ledger_read_errors_distinguish_not_found_from_real_errors() {
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
    // An empty-but-present ledger: result [] — skip nothing, apply.
    let empty = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!("[{}]", select_element("[]")),
        }],
        ok_script(),
    );
    push_with_creds(&empty.endpoint(), &migrations)
        .assert()
        .success();
    // A permission-class failure is NOT the fresh-database case: the
    // run refuses, carrying the engine's message.
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
        ok_script(),
    );
    let assert = push_with_creds(&denied.endpoint(), &migrations)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("reading the migration ledger failed"),
        "{stderr}"
    );
    assert!(stderr.contains("Not enough permissions"), "{stderr}");
    assert_eq!(
        denied.requests().len(),
        1,
        "nothing applies past the ledger refusal"
    );
    // A wrong --ns/--db rides the same top-level NotFound kind with a
    // different SUBJECT (the probe's shapes: Namespace, Database) — a
    // real error carrying the engine's message, never an
    // empty-ledger read that then applies into nowhere.
    for subject in ["Namespace", "Database"] {
        let wrong = MockSql::start(
            vec![Scripted {
                status: 200,
                body: format!(
                    "[{}]",
                    err_element(
                        "NotFound",
                        Some(subject),
                        &format!("The {subject} 'does-not-exist' does not exist")
                    )
                ),
            }],
            ok_script(),
        );
        let assert = push_with_creds(&wrong.endpoint(), &migrations)
            .assert()
            .failure();
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        assert!(
            stderr.contains("reading the migration ledger failed"),
            "{subject}: {stderr}"
        );
        assert!(
            stderr.contains("does-not-exist"),
            "the engine's message rides: {stderr}"
        );
        assert_eq!(
            wrong.requests().len(),
            1,
            "{subject}: nothing applies past the ledger refusal"
        );
    }
    let _ = json_string("unused helper silence");
}

// ---------------------------------------------------------------------
// Draft groups (issue #87): the members apply whole, in name order;
// the row records once, from the last member's script.
// ---------------------------------------------------------------------

/// A draft-group member, built through the format's real spellings —
/// the same header identity as the save writes, the `-- group:` line,
/// and (committed members only) the status line; the LAST member's
/// committed text carries the ledger block (the once-per-number
/// contract). Returns the path and the member's filled-script hash.
struct GroupMemberSpec<'a> {
    number: u32,
    letter: char,
    total: u32,
    name: &'a str,
    table: &'a str,
    committed: bool,
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
        let draft = parse_draft(&text).expect("the member parses");
        assert!(draft.header.committed.is_some());
        assert!(draft.header.group.expect("the group line").is_last() == (ordinal == total));
    }
    let path = dir.join(format!("{number:04}{letter}_{name}.surql"));
    std::fs::write(&path, &text).expect("the member writes");
    (path, hash)
}

/// An unapplied group applies EVERY member in name order — three
/// POSTs after the SELECT, a→b→c — and reports them applied.
#[test]
fn a_group_applies_whole_in_name_order() {
    let dir = workdir("group-apply");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    for (letter, table) in [('a', "alpha"), ('b', "beta"), ('c', "gamma")] {
        group_member(
            &migrations,
            GroupMemberSpec {
                number: 5,
                letter,
                total: 3,
                name: "shop",
                table,
                committed: true,
            },
            "_surform_migrations",
        );
    }
    let mock = MockSql::start(
        vec![ledger_answer(&[]), ok_script(), ok_script(), ok_script()],
        ok_script(),
    );
    let assert = push(&mock.endpoint(), &migrations).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("0005a_shop"), "{stdout}");
    assert!(stdout.contains("0005b_shop"), "{stdout}");
    assert!(stdout.contains("0005c_shop"), "{stdout}");
    let requests = mock.requests();
    assert_eq!(requests.len(), 4, "one SELECT + one POST per member");
    // The POSTs carry the members in NAME order.
    let bodies: Vec<String> = requests[1..]
        .iter()
        .map(|request| request.body.clone())
        .collect();
    assert!(bodies[0].contains("DEFINE TABLE `alpha`"), "{}", bodies[0]);
    assert!(bodies[1].contains("DEFINE TABLE `beta`"), "{}", bodies[1]);
    assert!(bodies[2].contains("DEFINE TABLE `gamma`"), "{}", bodies[2]);
}

/// An applied group skips EVERY member: the ledger holds the group's
/// row (the LAST member's hash) and nothing is POSTed.
#[test]
fn an_applied_group_skips_every_member() {
    let dir = workdir("group-skip");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let last_hash = group_member(
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
    )
    .1;
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
    let mock = MockSql::start(
        vec![ledger_answer(&[ledger_row(5, "shop", &last_hash)])],
        ok_script(),
    );
    let assert = push(&mock.endpoint(), &migrations).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_eq!(stdout.matches("already applied").count(), 2, "{stdout}");
    assert_eq!(mock.requests().len(), 1, "the SELECT only");
}

/// The mid-group failure contract: the run stops at the failed
/// member (the earlier members stay applied but UNRECORDED — the row
/// is the last member's), and the re-run re-applies them (OVERWRITE
/// makes it idempotent) and resumes to completion.
#[test]
fn a_mid_group_failure_rerun_resumes() {
    let dir = workdir("group-resume");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    for (letter, table) in [('a', "alpha"), ('b', "beta"), ('c', "gamma")] {
        group_member(
            &migrations,
            GroupMemberSpec {
                number: 5,
                letter,
                total: 3,
                name: "shop",
                table,
                committed: true,
            },
            "_surform_migrations",
        );
    }
    // First run: a applies, b fails — nothing is in the ledger (the
    // recording member never ran).
    let mock = MockSql::start(
        vec![ledger_answer(&[]), ok_script(), failed_apply_script()],
        ok_script(),
    );
    let assert = push(&mock.endpoint(), &migrations).assert().failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("0005a_shop"), "{stdout}");
    assert!(stdout.contains("0005b_shop"), "{stdout}");
    assert!(stdout.contains("FAILED"), "{stdout}");
    assert!(!stdout.contains("0005c_shop"), "c never ran: {stdout}");
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("0005b_shop failed"), "{stderr}");
    // The group resume contract, named where the operator reads it.
    assert!(stderr.contains("RE-APPLY on the re-run"), "{stderr}");
    assert_eq!(mock.requests().len(), 3, "SELECT + a + b (c never sent)");
    // The re-run: still no ledger row — a re-applies (idempotent),
    // b succeeds this time, c applies and its script self-records.
    let mock = MockSql::start(
        vec![ledger_answer(&[]), ok_script(), ok_script(), ok_script()],
        ok_script(),
    );
    let assert = push(&mock.endpoint(), &migrations).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_eq!(stdout.matches(" applied").count(), 3, "{stdout}");
    assert_eq!(mock.requests().len(), 4, "SELECT + three members");
}

/// A group only PARTLY committed blocks the run: a real push refuses
/// (the row could claim a group whose head never shipped); the dry
/// run classifies the group blocked and exits 1.
#[test]
fn a_partly_committed_group_blocks() {
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
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push(&mock.endpoint(), &migrations).assert().failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("only partly committed"), "{stderr}");
    assert!(stderr.contains("0005b_shop.surql"), "{stderr}");
    assert_eq!(
        mock.requests().len(),
        1,
        "the SELECT only — nothing applies"
    );
    // The dry run: the group is a blocked row, exit 1 (a fresh mock —
    // the first one's scripted ledger answer is spent).
    let dry = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let mut command = cli();
    command
        .args([
            "migrate",
            "push",
            "--endpoint",
            &dry.endpoint(),
            "--ns",
            "test",
            "--db",
            "scratch",
            "--dir",
        ])
        .arg(&migrations)
        .arg("--dry-run");
    let assert = command.assert().failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("blocked — the group is only partly committed"),
        "{stdout}"
    );
    assert!(stdout.contains("1 blocked"), "{stdout}");
}

/// A WHOLLY-draft group — the pipeline's default state right after
/// every multi-scope save — takes the standalone drafts' benign skip
/// verdict: it neither blocks the run nor stops unrelated committed
/// migrations from applying.
#[test]
fn a_wholly_draft_group_skips_benignly() {
    let dir = workdir("group-fresh");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    for letter in ['a', 'b'] {
        group_member(
            &migrations,
            GroupMemberSpec {
                number: 5,
                letter,
                total: 2,
                name: "shop",
                table: if letter == 'a' { "alpha" } else { "beta" },
                committed: false,
            },
            "_surform_migrations",
        );
    }
    // A committed standalone beside the fresh group: it must apply.
    committed_file(
        &migrations,
        1,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "m1",
        "_surform_migrations",
    );
    let mock = MockSql::start(vec![ledger_answer(&[]), ok_script()], ok_script());
    let assert = push(&mock.endpoint(), &migrations).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("0001_add-users"), "{stdout}");
    assert_eq!(stdout.matches("skipped — draft").count(), 2, "{stdout}");
    assert_eq!(mock.requests().len(), 2, "SELECT + the standalone only");
    // The dry run: the group's members are draft rows, nothing blocked.
    let dry = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let mut command = cli();
    command
        .args([
            "migrate",
            "push",
            "--endpoint",
            &dry.endpoint(),
            "--ns",
            "test",
            "--db",
            "scratch",
            "--dir",
        ])
        .arg(&migrations)
        .arg("--dry-run");
    let assert = command.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("1 would apply"), "{stdout}");
    assert!(stdout.contains("2 draft(s) skipped"), "{stdout}");
    assert!(!stdout.contains("blocked"), "{stdout}");
}

/// A hand-renamed committed standalone — the name carrying 0009 while
/// the header says 0004 — refuses at the walk:
/// push would apply under the name's number while the script's INSERT
/// self-records the header's.
#[test]
fn a_name_number_disagreement_refuses_the_walk() {
    let dir = workdir("name-number");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let (path, _) = committed_file(
        &migrations,
        4,
        "add-users",
        "DEFINE TABLE `user`;\n",
        "m1",
        "_surform_migrations",
    );
    let renamed = migrations.join("0009_add-users.surql");
    std::fs::rename(&path, &renamed).expect("the rename");
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push(&mock.endpoint(), &migrations).assert().failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("0009") && stderr.contains("0004"),
        "{stderr}"
    );
    assert!(stderr.contains("splitting the identity"), "{stderr}");
    assert_eq!(
        mock.requests().len(),
        0,
        "the walk refuses before any request"
    );
}

/// A structurally broken group refuses before ANY request: a letter
/// gap (a, c — b missing) leaves the apply order and the record
/// undefined.
#[test]
fn a_broken_group_refuses_before_any_request() {
    let dir = workdir("group-broken");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    group_member(
        &migrations,
        GroupMemberSpec {
            number: 5,
            letter: 'a',
            total: 3,
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
            letter: 'c',
            total: 3,
            name: "shop",
            table: "gamma",
            committed: true,
        },
        "_surform_migrations",
    );
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push(&mock.endpoint(), &migrations).assert().failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("broken"), "{stderr}");
    assert!(stderr.contains("contiguous from `a`"), "{stderr}");
    assert_eq!(
        mock.requests().len(),
        0,
        "the walk refuses before any request"
    );
}

/// A file whose NAME carries the letter but whose header carries no
/// `-- group:` line refuses the walk (staged
/// against the binary before the fix: it counted as a standalone,
/// sorted past one, and was silently invisible to push/plan/status).
/// The mirror — a `-- group:` header under an unlettered name —
/// refuses with the same ground.
#[test]
fn a_name_header_letter_disagreement_refuses_the_walk() {
    let dir = workdir("group-name-header");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    // A committed standalone and a "member" whose header forgot the
    // group line — the lettered name must not ride the walk as
    // nothing.
    let (path, _) = group_member(
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
    let text = std::fs::read_to_string(&path).expect("the member reads");
    let headerless = text.replace("-- group: 0005 a/2\n", "");
    let twin = migrations.join("0005b_shop.surql");
    std::fs::write(&twin, headerless).expect("the headerless twin writes");
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push(&mock.endpoint(), &migrations).assert().failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("carries letter b"), "{stderr}");
    assert!(stderr.contains("no `-- group:` line"), "{stderr}");
    assert_eq!(mock.requests().len(), 0, "refused before any request");
    // The mirror: a group header under an unlettered name.
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
    let text = std::fs::read_to_string(migrations.join("0005b_shop.surql")).expect("reads");
    std::fs::remove_file(migrations.join("0005b_shop.surql")).expect("the lettered twin removes");
    let unlettered_name = migrations.join("0005_shop.surql");
    std::fs::write(&unlettered_name, text).expect("the unlettered twin writes");
    let mock = MockSql::start(vec![ledger_answer(&[])], ok_script());
    let assert = push(&mock.endpoint(), &migrations).assert().failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("carries no letter"), "{stderr}");
    assert!(stderr.contains("letter b"), "{stderr}");
}

/// The save/push intersection the suites had missed: a multi-scope
/// group's members each carry their OWN scope's
/// hoisted `USE` — exactly the shape the save writes — and push
/// applies them: the earlier member's mismatching USE is its own
/// request's session, while the RECORDING member's USE must name the
/// push target (its script writes the row the skip-SELECT reads). A
/// recording-member mismatch refuses.
#[test]
fn a_group_with_use_switches_pushes_by_its_recording_member() {
    let dir = workdir("group-use");
    let migrations = Path::new(&dir).join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let member = |letter: char, use_line: &str| {
        // The hash rides the parsed BODY (BEGIN..EOF — the USE sits
        // above it, like the save's preamble), so it hashes the frame
        // alone.
        let frame = format!("BEGIN TRANSACTION;\nDEFINE TABLE `t{letter}`;\nCOMMIT;\n");
        let hash = surform::migrate::sha256_hex(surform::migrate::filled_script(&frame).as_bytes());
        let body = format!("{use_line}\n{frame}");
        let mut text = format!(
            "-- surform migration 0005 — shop\n\
             -- generated: 2026-10-08T00:00:00Z\n\
             -- source: v2.surdl\n\
             -- input: info\n\
             -- group: 0005 {letter}/2\n\
             -- status: committed sha256:{hash} member {letter}\n\
             \n\
             "
        );
        if letter == 'b' {
            let block = surform::migrate::ledger_statements(
                "_surform_migrations",
                5,
                "shop",
                &hash,
                "the group records once",
                "namespace test;\n",
            );
            text.push_str(
                &surform::migrate::splice_before_commit(&body, &block)
                    .expect("the body carries COMMIT"),
            );
        } else {
            text.push_str(&body);
        }
        std::fs::write(migrations.join(format!("0005{letter}_shop.surql")), text)
            .expect("the member writes");
    };
    member('a', "USE NS test DB other;");
    member('b', "USE NS test DB scratch;");
    // The recording member names the push target: the group applies,
    // both members POST in name order.
    let mock = MockSql::start(
        vec![ledger_answer(&[]), ok_script(), ok_script()],
        ok_script(),
    );
    let assert = push(&mock.endpoint(), &migrations).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_eq!(stdout.matches(" applied").count(), 2, "{stdout}");
    assert_eq!(mock.requests().len(), 3, "SELECT + two members");
    // The recording member's mismatch refuses (the row would land
    // elsewhere): same files, other target.
    let other = MockSql::start(
        vec![ledger_answer(&[]), ok_script(), ok_script()],
        ok_script(),
    );
    let mut command = cli();
    command
        .args([
            "migrate",
            "push",
            "--endpoint",
            &other.endpoint(),
            "--ns",
            "test",
            "--db",
            "other",
            "--dir",
        ])
        .arg(&migrations);
    let assert = command.assert().failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("USE"), "{stderr}");
    assert!(stderr.contains("would move the statements"), "{stderr}");
}
