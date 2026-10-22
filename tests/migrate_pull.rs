//! The `migrate pull` suite (issue #78): the HTTP boundary is MOCKED
//! (`common/mock_sql.rs`) — the legs pin the request shape (two POSTs:
//! the three scopes as separate queries in one body, then one INFO FOR
//! TABLE per table in the db result's sorted (engine Value) key order;
//! nothing but INFO
//! is ever sent), the interchange files the `--info` mode actually
//! ingests (the acceptance leg runs the printed `migrate generate`
//! command against the pulled files), the env fallbacks and the
//! credential pairing, the unsafe-name refusal, and the named wire
//! errors (a per-scope ERR naming its scope, a short response array,
//! a non-200). The scope-aware degradation (issue #103) rides the
//! probed permission kind: a db-level credential pulls its database
//! alone, an ns-level one keeps its two readable scopes, and the db
//! scope never degrades. The branch key's engine-side evidence is
//! pinned in-process by `engine_scope_permissions.rs`.

#[path = "common/mod.rs"]
mod common;
#[path = "common/mock_sql.rs"]
mod mock_sql;

use assert_cmd::Command;
use mock_sql::{MockSql, Scripted, err_element, json_string};

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
    let dir = std::env::temp_dir().join(format!("surform-pull-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the workdir creates");
    dir.to_string_lossy().into_owned()
}

/// One `status: "OK"` INFO element whose result is the object text.
fn info_element(rows: &str) -> String {
    format!(r#"{{"result":{rows},"status":"OK","time":"1.000ms","type":"object"}}"#)
}

/// The ROOT scope's JSON (the engine's /sql serialization: leaves are
/// DDL strings; `system` carries numbers; `config` rides `null` —
/// pull renders it `NONE`, the readers' carried token).
fn root_json() -> String {
    format!(
        r#"{{"accesses":{{}},"config":null,"namespaces":{{"test":{}}},"nodes":{{}},"system":{{"available_parallelism":8}},"users":{{}}}}"#,
        json_string("DEFINE NAMESPACE test")
    )
}

/// The NS scope's JSON.
fn ns_json() -> String {
    format!(
        r#"{{"databases":{{"scratch":{}}},"users":{{}}}}"#,
        json_string("DEFINE DATABASE scratch")
    )
}

/// The DB scope's JSON with the named tables (each mapping to its
/// `DEFINE TABLE` DDL string).
fn db_json(tables: &[(&str, &str)]) -> String {
    let tables: String = tables
        .iter()
        .map(|(name, ddl)| format!("{}:{}", json_string(name), json_string(ddl)))
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"params":{{}},"tables":{{{tables}}},"users":{{}}}}"#)
}

/// A table's `INFO FOR TABLE` JSON.
fn table_json(fields: &[(&str, &str)]) -> String {
    let fields: String = fields
        .iter()
        .map(|(name, ddl)| format!("{}:{}", json_string(name), json_string(ddl)))
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"events":{{}},"fields":{{{fields}}},"indexes":{{}},"lives":{{}},"tables":{{}}}}"#)
}

/// The scopes leg's scripted answer: root, ns, db — position-mapped
/// to `INFO FOR ROOT; INFO FOR NS; INFO FOR DB;`.
fn scopes_script(root: &str, ns: &str, db: &str) -> Scripted {
    Scripted {
        status: 200,
        body: format!(
            "[{},{},{}]",
            info_element(root),
            info_element(ns),
            info_element(db)
        ),
    }
}

/// The tables leg's scripted answer: one element per named table.
fn tables_script(tables: &[String]) -> Scripted {
    let elements: String = tables
        .iter()
        .map(|body| info_element(body))
        .collect::<Vec<_>>()
        .join(",");
    Scripted {
        status: 200,
        body: format!("[{elements}]"),
    }
}

/// A loud fallback: any request beyond the script is a test failure
/// in disguise (an ERR element the assertions would trip over).
fn loud_fallback() -> Scripted {
    Scripted {
        status: 200,
        body: format!(
            "[{}]",
            err_element("NotImplemented", None, "the mock scripted no answer here")
        ),
    }
}

/// The permission-refusal element (issue #103, probed in-process —
/// `engine_scope_permissions.rs`): the stable `NotAllowed` kind with
/// the `Auth` reason and the IAM verdict message. The pull branches
/// on the kind, never the text.
fn refused_element() -> String {
    err_element(
        "NotAllowed",
        Some("Auth"),
        "IAM error: Not enough permissions to perform this action",
    )
}

/// A pull run against the mock, with the flags' coordinates.
fn pull_command(endpoint: &str, dir: &std::path::Path) -> Command {
    let mut command = cli();
    command
        .args([
            "migrate",
            "pull",
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

/// The SDL the acceptance legs compare against: it declares the
/// session scopes (the `--info` tags' validation) and nothing else —
/// so the db scope's tables are pure adoption drift.
fn sdl_file(dir: &str) -> String {
    let path = std::path::Path::new(&dir).join("schema.surdl");
    std::fs::write(&path, "namespace test;\ndatabase scratch;\n").expect("the sdl writes");
    path.display().to_string()
}

/// THE acceptance leg (issue #78's bar): pull against the mock, then
/// run the EXACT command pull printed — the tags load the pulled
/// files, the readers ingest them, and the db scope's table arrives
/// as adoption drift. The printed command is db-scoped (issue #100),
/// so the ingested compare is the db scope's — the pasted happy path
/// makes no claim about the wider scopes.
#[test]
fn the_printed_command_ingests_the_pulled_files() {
    let dir = workdir("accept");
    let out = std::path::Path::new(&dir).join("info");
    let mock = MockSql::start(
        vec![
            scopes_script(
                &root_json(),
                &ns_json(),
                &db_json(&[("user", "DEFINE TABLE user TYPE NORMAL SCHEMALESS")]),
            ),
            tables_script(&[table_json(&[(
                "email",
                "DEFINE FIELD email ON user TYPE string",
            )])]),
        ],
        loud_fallback(),
    );
    let assert = pull_command(&mock.endpoint(), &out).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let args = stdout
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("surform migrate generate schema.surdl")
        })
        .unwrap_or_else(|| panic!("the printed command is missing: {stdout}"));
    let sdl = sdl_file(&dir);
    // The exact printed command, run verbatim (the drift exit is the
    // compare's verdict — a LOAD failure would be an error, not 1).
    // The shell quoting the printer emits is single quotes; a shell
    // would strip them from each token — trim_matches is the same
    // operation for quote-free interiors (the names refuse `'`).
    let assert = cli()
        .arg("migrate")
        .arg("generate")
        .arg(&sdl)
        // The printer single-quotes each token; the shell strips the
        // quotes (the names refuse `'`, so a plain removal is the
        // same operation here).
        .args(args.split_whitespace().map(|arg| arg.replace('\'', "")))
        .assert()
        .failure()
        .code(1);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("user"),
        "the adoption drift names the table: {stdout}"
    );
    assert!(
        !stdout.contains("error:"),
        "a refusal would mean the files did not ingest: {stdout}"
    );
}

/// The converged acceptance: an empty database pulls, and the printed
/// command's run CONVERGES (exit 0) — the db-scoped compare over the
/// pulled file, byte-exact through the readers (the wider scopes are
/// untagged, so they make no claim — issue #100's scoping).
#[test]
fn the_pulled_files_converge_against_their_own_database() {
    let dir = workdir("converge");
    let out = std::path::Path::new(&dir).join("info");
    let mock = MockSql::start(
        vec![scopes_script(&root_json(), &ns_json(), &db_json(&[]))],
        loud_fallback(),
    );
    let assert = pull_command(&mock.endpoint(), &out).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let args = stdout
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("surform migrate generate schema.surdl")
        })
        .unwrap_or_else(|| panic!("the printed command is missing: {stdout}"));
    let sdl = sdl_file(&dir);
    let assert = cli()
        .arg("migrate")
        .arg("generate")
        .arg(&sdl)
        // The printer single-quotes each token; the shell strips the
        // quotes (the names refuse `'`, so a plain removal is the
        // same operation here).
        .args(args.split_whitespace().map(|arg| arg.replace('\'', "")))
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("(converged)"), "{stdout}");
}

/// The request shape, pinned against the doc-verified contract: two
/// POSTs — the three scope queries in ONE body, then one INFO FOR
/// TABLE per table in the db result's key order (the backtick-quoting
/// riding [`ident_text`]'s escape rule) — the contract's headers on
/// both, and NOTHING but INFO in any body.
#[test]
fn the_request_shape_pins_the_contract() {
    let dir = workdir("shape");
    let out = std::path::Path::new(&dir).join("info");
    let mock = MockSql::start(
        vec![
            scopes_script(
                &root_json(),
                &ns_json(),
                &db_json(&[
                    ("user", "DEFINE TABLE user TYPE NORMAL SCHEMALESS"),
                    ("we-ird", "DEFINE TABLE `we-ird` TYPE NORMAL SCHEMALESS"),
                ]),
            ),
            tables_script(&[table_json(&[]), table_json(&[])]),
        ],
        loud_fallback(),
    );
    pull_command(&mock.endpoint(), &out)
        .args(["--user", "pulluser", "--pass", "pullpass-secret"])
        .assert()
        .success();
    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "one POST per leg");
    let scopes = &requests[0];
    assert_eq!(scopes.method, "POST");
    assert_eq!(scopes.path, "/sql");
    assert_eq!(scopes.header("accept"), Some("application/json"));
    assert_eq!(scopes.header("surreal-ns"), Some("test"));
    assert_eq!(scopes.header("surreal-db"), Some("scratch"));
    assert_eq!(
        scopes.header("authorization"),
        Some(format!("Basic {}", mock_b64("pulluser:pullpass-secret")).as_str()),
        "the Basic header is the docs' `-u` spelling"
    );
    assert_eq!(
        scopes.body, "INFO FOR ROOT; INFO FOR NS; INFO FOR DB;",
        "the scopes ride as separate queries in one body"
    );
    let tables = &requests[1];
    assert_eq!(
        tables.body, "INFO FOR TABLE `user`; INFO FOR TABLE `we-ird`;",
        "the table queries follow the db result's key order, names quoted"
    );
    // Nothing but INFO was ever sent — the no-DDL property.
    for request in &requests {
        assert!(
            request.body.starts_with("INFO FOR "),
            "a non-INFO statement went over the wire: {}",
            request.body
        );
    }
    // The db file carries the assembled children under the reserved
    // key, both tables' child maps inside.
    let db_text =
        std::fs::read_to_string(out.join("surform-info-db-test-scratch.txt")).expect("db file");
    assert!(db_text.contains("\"@tables\""), "{db_text}");
    assert!(
        db_text.contains("'DEFINE TABLE user TYPE NORMAL SCHEMALESS'"),
        "{db_text}"
    );
    assert!(db_text.contains("we-ird"), "{db_text}");
}

/// ASCII base64 (RFC 4648) — the tests' own expected-header spelling.
fn mock_b64(input: &str) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        out.push(TABLE[(b[0] >> 2) as usize] as char);
        out.push(TABLE[(((b[0] & 0x03) << 4) | (b[1] >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b[1] & 0x0F) << 2) | (b[2] >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(b[2] & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// The pull's file footprint: EXACTLY the three interchange files in
/// --dir (created when missing) — nothing else is written, and the
/// report rows carry the verdicts.
#[test]
fn pull_writes_nothing_but_the_info_files() {
    let dir = workdir("footprint");
    let out = std::path::Path::new(&dir).join("info");
    let mock = MockSql::start(
        vec![scopes_script(&root_json(), &ns_json(), &db_json(&[]))],
        loud_fallback(),
    );
    let assert = pull_command(&mock.endpoint(), &out).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("surform-info-root.txt"), "{stdout}");
    assert!(stdout.contains("surform-info-ns-test.txt"), "{stdout}");
    assert!(
        stdout.contains("surform-info-db-test-scratch.txt"),
        "{stdout}"
    );
    let mut entries: Vec<String> = std::fs::read_dir(&out)
        .expect("the dir exists")
        .map(|entry| {
            entry
                .expect("the entry reads")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec![
            "surform-info-db-test-scratch.txt".to_string(),
            "surform-info-ns-test.txt".to_string(),
            "surform-info-root.txt".to_string(),
        ],
        "the directory holds exactly the three interchange files"
    );
    // The root file's rendering carries the engine's NONE spelling
    // for the wire's null (the readers' carried token).
    let root_text = std::fs::read_to_string(out.join("surform-info-root.txt")).expect("root file");
    assert!(root_text.contains("config: NONE"), "{root_text}");
    assert!(
        root_text.contains("available_parallelism: 8"),
        "{root_text}"
    );
}

/// The exact printed command at the default `--dir` (`.`): bare file
/// names, ONE `--info` tag (the database's — issue #100: the wider
/// compares ride the advice lines, never the pasted command), and the
/// advice lines under it naming the wider tags and the removals fact.
#[test]
fn the_printed_command_at_the_default_dir() {
    let dir = workdir("printed");
    let mock = MockSql::start(
        vec![scopes_script(&root_json(), &ns_json(), &db_json(&[]))],
        loud_fallback(),
    );
    let assert = cli()
        .args([
            "migrate",
            "pull",
            "--endpoint",
            &mock.endpoint(),
            "--ns",
            "test",
            "--db",
            "scratch",
        ])
        .current_dir(&dir)
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let command_line = stdout
        .lines()
        .find(|line| line.trim().starts_with("surform migrate generate"))
        .unwrap_or_else(|| panic!("the printed command is missing: {stdout}"));
    assert_eq!(
        command_line.trim(),
        "surform migrate generate schema.surdl \
         --info 'db=test/scratch=surform-info-db-test-scratch.txt'",
        "the pasted command carries the db tag only"
    );
    // The advice lines: the wider tags (named with their written
    // paths, shell-ready) and the classification fact that scopes
    // them (issue #100's anchoring sentence).
    assert!(
        stdout.contains(
            "the wider compares: add --info root='surform-info-root.txt' and \
             --info 'ns=test=surform-info-ns-test.txt' to the command above"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("classifies as removals"),
        "the advice names what a wider compare classifies: {stdout}"
    );
    assert!(stdout.contains("read the drift report"), "{stdout}");
}

/// The coordinates fall back to their environment variables when the
/// flags are absent — the same resolution push runs (issue #76).
#[test]
fn the_environment_fallbacks_resolve() {
    let dir = workdir("env");
    let out = std::path::Path::new(&dir).join("info");
    let mock = MockSql::start(
        vec![scopes_script(&root_json(), &ns_json(), &db_json(&[]))],
        loud_fallback(),
    );
    cli()
        .args(["migrate", "pull", "--dir"])
        .arg(&out)
        .env("SURFORM_ENDPOINT", mock.endpoint())
        .env("SURFORM_NS", "test")
        .env("SURFORM_DB", "scratch")
        .assert()
        .success();
    let scopes = &mock.requests()[0];
    assert_eq!(scopes.header("surreal-ns"), Some("test"));
    assert_eq!(scopes.header("surreal-db"), Some("scratch"));
}

/// The credential pairing: a user without a password refuses before
/// anything dials (the give-both-or-neither discipline), and so does
/// a password without a user.
#[test]
fn the_credential_pairing_refuses_before_the_dial() {
    for args in [vec!["--user", "lonely"], vec!["--pass", "lonely"]] {
        let mock = MockSql::start(vec![], loud_fallback());
        let assert = cli()
            .arg("migrate")
            .arg("pull")
            .arg("--endpoint")
            .arg(mock.endpoint())
            .args(["--ns", "test", "--db", "scratch"])
            .args(&args)
            .assert()
            .failure();
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        assert!(stderr.contains("error:"), "{stderr}");
        assert!(mock.requests().is_empty(), "nothing dialed");
    }
}

/// The bearer credential (issue #99) rides pull's POSTs too: a
/// `--token` run carries `Authorization: Bearer <token>` on every
/// leg, and a token alongside a user refuses before the dial (the
/// same exclusivity push refuses).
#[test]
fn the_bearer_token_rides_the_legs_and_refuses_basic() {
    let dir = workdir("token");
    let out = std::path::Path::new(&dir).join("info");
    let mock = MockSql::start(
        vec![scopes_script(&root_json(), &ns_json(), &db_json(&[]))],
        loud_fallback(),
    );
    pull_command(&mock.endpoint(), &out)
        .args(["--token", "pull-jwt"])
        .assert()
        .success();
    for request in mock.requests() {
        assert_eq!(
            request.header("authorization"),
            Some("Bearer pull-jwt"),
            "the bearer token rides every POST: {}",
            request.body
        );
    }
    // The exclusivity refusal — before the dial, like the pairing's.
    let mock = MockSql::start(vec![], loud_fallback());
    let assert = pull_command(&mock.endpoint(), &out)
        .args(["--token", "pull-jwt", "--user", "pulluser"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("exclusive"), "{stderr}");
    assert!(mock.requests().is_empty(), "nothing dialed");
}

/// A coordinate whose name cannot ride the interchange file name
/// refuses before anything dials — a path separator would nest (or
/// escape) `--dir`.
#[test]
fn an_unsafe_name_refuses_before_the_dial() {
    for (ns, db) in [("a/b", "scratch"), ("test", "..\\x")] {
        let dir = workdir("unsafe");
        let mock = MockSql::start(vec![], loud_fallback());
        let assert = cli()
            .arg("migrate")
            .arg("pull")
            .arg("--endpoint")
            .arg(mock.endpoint())
            .args(["--ns", ns, "--db", db])
            .arg("--dir")
            .arg(&dir)
            .assert()
            .failure();
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        assert!(
            stderr.contains("cannot ride an interchange file name"),
            "{stderr}"
        );
        assert!(mock.requests().is_empty(), "nothing dialed");
    }
    // An `=` makes the printed `--info` tag un-parseable — refused
    // with its own reason, still before the dial.
    let dir = workdir("unsafe-eq");
    let mock = MockSql::start(vec![], loud_fallback());
    let assert = cli()
        .arg("migrate")
        .arg("pull")
        .arg("--endpoint")
        .arg(mock.endpoint())
        .args(["--ns", "a=b", "--db", "scratch"])
        .arg("--dir")
        .arg(&dir)
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("carries `=`"), "{stderr}");
    assert!(mock.requests().is_empty(), "nothing dialed");
}

/// A non-permission ERR names ITS scope and refuses the whole pull
/// (issue #103's self-explaining refusal): a mistyped `--ns`/`--db`
/// lands NotFound at the ns or db element; the pull cannot degrade
/// on it, because that scope's INFO is simply absent.
#[test]
fn a_wire_error_names_its_scope() {
    for (position, scope) in [(0, "root"), (1, "ns"), (2, "db")] {
        let dir = workdir("err");
        let mut elements = [
            info_element(&root_json()),
            info_element(&ns_json()),
            info_element(&db_json(&[])),
        ];
        elements[position] = err_element(
            "NotFound",
            Some("Database"),
            "The database 'scratch' does not exist",
        );
        let mock = MockSql::start(
            vec![Scripted {
                status: 200,
                body: format!("[{},{},{}]", elements[0], elements[1], elements[2]),
            }],
            loud_fallback(),
        );
        let assert = pull_command(&mock.endpoint(), std::path::Path::new(&dir))
            .assert()
            .failure();
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        assert!(
            stderr.contains(&format!("the {scope} scope read failed")),
            "{stderr}"
        );
        assert!(
            stderr.contains("The database 'scratch' does not exist"),
            "{stderr}"
        );
    }
}

/// The database scope's own permission refusal refuses the whole
/// pull — the db scope is the pull's subject and never degrades
/// (issue #103).
#[test]
fn a_refused_database_scope_refuses_the_pull() {
    let dir = workdir("refused-db");
    let mock = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!(
                "[{},{},{}]",
                info_element(&root_json()),
                info_element(&ns_json()),
                refused_element()
            ),
        }],
        loud_fallback(),
    );
    let assert = pull_command(&mock.endpoint(), std::path::Path::new(&dir))
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("the database scope was refused for this credential"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Not enough permissions to perform this action"),
        "the engine's message rides: {stderr}"
    );
}

/// A db-level credential's pull (issue #103's audience — the bearer
/// token's): the ROOT and NS elements ride the permission refusal
/// while DB is OK, and the pull proceeds with the db scope alone —
/// one file written, the db-only printed command, the advice naming
/// both refused scopes. The request shape is unchanged: the whole
/// three-statement batch still rides one POST.
#[test]
fn a_db_level_credential_pulls_its_scope() {
    let dir = workdir("dbcred");
    let out = std::path::Path::new(&dir).join("info");
    let mock = MockSql::start(
        vec![
            Scripted {
                status: 200,
                body: format!(
                    "[{},{},{}]",
                    refused_element(),
                    refused_element(),
                    info_element(&db_json(&[(
                        "user",
                        "DEFINE TABLE user TYPE NORMAL SCHEMALESS",
                    )]))
                ),
            },
            tables_script(&[table_json(&[(
                "email",
                "DEFINE FIELD email ON user TYPE string",
            )])]),
        ],
        loud_fallback(),
    );
    let assert = pull_command(&mock.endpoint(), &out)
        .args(["--token", "db-jwt"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    // The printed command is byte-identical to a full pull's — the
    // db tag only (issue #100's invariant, every pull shape).
    let command_line = stdout
        .lines()
        .find(|line| line.trim().starts_with("surform migrate generate"))
        .unwrap_or_else(|| panic!("the printed command is missing: {stdout}"));
    assert_eq!(
        command_line.trim(),
        format!(
            "surform migrate generate schema.surdl \
             --info 'db=test/scratch={}'",
            out.join("surform-info-db-test-scratch.txt").display()
        ),
        "the printed command is byte-identical to a full pull's — the db tag only"
    );
    // No wider tags exist to name; the refused scopes' note is the
    // whole advice.
    assert!(
        stdout.contains(
            "the root and namespace scopes were refused for this \
             credential (not permitted) - no files were written for \
             them; a credential that can read them completes the pull"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("the wider compares"), "{stdout}");
    assert!(!stdout.contains("classifies as removals"), "{stdout}");
    // The footprint: the db file only, full shape (the children ride).
    let entries: Vec<String> = std::fs::read_dir(&out)
        .expect("the dir exists")
        .map(|entry| {
            entry
                .expect("the entry reads")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        entries,
        vec!["surform-info-db-test-scratch.txt".to_string()],
        "the refused scopes wrote nothing"
    );
    let db_text =
        std::fs::read_to_string(out.join("surform-info-db-test-scratch.txt")).expect("db file");
    assert!(
        db_text.contains("'DEFINE FIELD email ON user TYPE string'"),
        "{db_text}"
    );
    // The wire: the scopes leg is still the whole batch.
    assert_eq!(
        mock.requests()[0].body,
        "INFO FOR ROOT; INFO FOR NS; INFO FOR DB;",
        "the batch is sent whole; the degradation is the read side's"
    );
}

/// A partial pull (an ns-level credential: ROOT refused, NS and DB
/// OK) keeps both fetched scopes — the ns file is written, the
/// wider-compares advice names the ns tag alone, and the refused
/// note names the root scope.
#[test]
fn a_partial_pull_keeps_its_fetched_scopes() {
    let dir = workdir("partial");
    let out = std::path::Path::new(&dir).join("info");
    let mock = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!(
                "[{},{},{}]",
                refused_element(),
                info_element(&ns_json()),
                info_element(&db_json(&[]))
            ),
        }],
        loud_fallback(),
    );
    let assert = pull_command(&mock.endpoint(), &out).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("written (INFO FOR NS test)"), "{stdout}");
    assert!(
        stdout.contains(
            format!(
                "the wider compares: add --info 'ns=test={}' to the command above",
                out.join("surform-info-ns-test.txt").display()
            )
            .as_str()
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "the root scope was refused for this credential (not \
             permitted) - no file was written for it; a credential \
             that can read it completes the pull"
        ),
        "{stdout}"
    );
    assert!(out.join("surform-info-ns-test.txt").exists(), "{stdout}");
    assert!(out.join("surform-info-db-test-scratch.txt").exists());
    assert!(
        !out.join("surform-info-root.txt").exists(),
        "the refused scope wrote nothing"
    );
}

/// A response array shorter than the leg's queries is a malformed
/// response — the position mapping would be a guess.
#[test]
fn a_short_response_array_refuses() {
    let dir = workdir("short");
    let mock = MockSql::start(
        vec![Scripted {
            status: 200,
            body: format!(
                "[{},{}]",
                info_element(&root_json()),
                info_element(&ns_json())
            ),
        }],
        loud_fallback(),
    );
    let assert = pull_command(&mock.endpoint(), std::path::Path::new(&dir))
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("2 element(s) for 3 INFO statement(s)"),
        "{stderr}"
    );
}

/// The request-level failure layer: a non-200 answer rides the body
/// verbatim (the probe's plain-text 401).
#[test]
fn the_http_layer_rides_verbatim() {
    let dir = workdir("http");
    let mock = MockSql::start(
        vec![Scripted {
            status: 401,
            body: "The password did not verify".to_string(),
        }],
        loud_fallback(),
    );
    let assert = pull_command(&mock.endpoint(), std::path::Path::new(&dir))
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("the server answered HTTP 401 — The password did not verify"),
        "{stderr}"
    );
}

/// A re-pull REFRESHES its own three names (overwrite semantics) and
/// still touches nothing else — the second run's files differ when
/// the database did.
#[test]
fn a_re_pull_refreshes_its_own_files() {
    let dir = workdir("repull");
    let out = std::path::Path::new(&dir).join("info");
    let mock = MockSql::start(
        vec![
            scopes_script(
                &root_json(),
                &ns_json(),
                &db_json(&[("user", "DEFINE TABLE user TYPE NORMAL SCHEMALESS")]),
            ),
            tables_script(&[table_json(&[])]),
            scopes_script(&root_json(), &ns_json(), &db_json(&[])),
        ],
        loud_fallback(),
    );
    pull_command(&mock.endpoint(), &out).assert().success();
    let db_path = out.join("surform-info-db-test-scratch.txt");
    let first = std::fs::read_to_string(&db_path).unwrap();
    assert!(
        first.contains("'DEFINE TABLE user TYPE NORMAL SCHEMALESS'"),
        "{first}"
    );
    // The hand-written file beside them stays untouched.
    let hand = out.join("my-hand-file.txt");
    std::fs::write(&hand, "hand-written").unwrap();
    pull_command(&mock.endpoint(), &out).assert().success();
    let second = std::fs::read_to_string(&db_path).unwrap();
    assert!(!second.contains("DEFINE TABLE"), "{second}");
    assert_eq!(
        std::fs::read_to_string(&hand).unwrap(),
        "hand-written",
        "the hand file is not pull's to touch"
    );
}
