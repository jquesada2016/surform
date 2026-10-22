//! The migration FILE pipeline's suite: the saved
//! drafts (naming, numbering, the header, the git-style slot
//! resolution markers), `migrate commit` (the refusals — markers,
//! message, re-commit, duplicate numbers — and the happy path's
//! hash + splice + header mark), the ledger's engine-executed
//! idempotence, and the end-to-end leg: generate → fill → commit →
//! EXECUTE the committed file against an embedded engine built at
//! the from-state, the ledger row landing with the migration.
//! Stdout byte-stability is the EXISTING cli/diff suites' job (the
//! stdout path is untouched); the marker-absence legs here pin the
//! carriers' split.

#[path = "common/mod.rs"]
mod common;
#[path = "common/exec.rs"]
mod exec;

use assert_cmd::Command;

const NS: &str = "test";
const DB: &str = "scratch";

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

/// A fresh per-test working directory (the drafts land there, so the
/// `migrations/` default and relative source paths resolve inside).
fn workdir(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("surform-n1-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the workdir creates");
    dir.to_string_lossy().into_owned()
}

/// The version-pair fixture: v1 holds `user` and `legacy` (the
/// drop-with-a-slot direction), v2 holds `user` only.
fn version_pair(dir: &str) -> (String, String) {
    let from = std::path::Path::new(dir).join("v1.surdl");
    let to = std::path::Path::new(dir).join("v2.surdl");
    std::fs::write(
        &from,
        "namespace test;\ndatabase scratch;\ntable `user` {\n  schemaless;\n};\ntable legacy;\n",
    )
    .expect("v1 writes");
    std::fs::write(
        &to,
        "namespace test;\ndatabase scratch;\ntable `user` {\n  schemaless;\n};\n",
    )
    .expect("v2 writes");
    (
        from.to_string_lossy().into_owned(),
        to.to_string_lossy().into_owned(),
    )
}

/// The live-INFO fixture (the cli.rs shape): the SDL declares `user`
/// alone; the interchange holds `user` and — when `stale` — a
/// database-only `legacy` table (the slot class).
fn info_fixture(dir: &str, stale: bool) -> Vec<String> {
    let base = std::path::Path::new(dir);
    let sdl = base.join("schema.surdl");
    std::fs::write(
        &sdl,
        "namespace test;\ndatabase scratch;\ntable `user` {\n  schemaless;\n};\n",
    )
    .expect("the sdl writes");
    std::fs::write(
        base.join("info-root.txt"),
        "{ namespaces: { test: 'DEFINE NAMESPACE test' } }\n",
    )
    .expect("root writes");
    std::fs::write(
        base.join("info-ns.txt"),
        "{ databases: { scratch: 'DEFINE DATABASE scratch' } }\n",
    )
    .expect("ns writes");
    let mut tables = "`user`: 'DEFINE TABLE user TYPE NORMAL SCHEMALESS'".to_string();
    let mut children = "`user`: { fields: {  }, indexes: {  }, events: {  } }".to_string();
    if stale {
        tables.push_str(", legacy: 'DEFINE TABLE legacy TYPE NORMAL SCHEMALESS'");
        children.push_str(", legacy: { fields: {  }, indexes: {  }, events: {  } }");
    }
    std::fs::write(
        base.join("info-db.txt"),
        format!("{{\n  tables: {{ {tables} }},\n  \"@tables\": {{ {children} }}\n}}\n"),
    )
    .expect("info writes");
    let p = |name: &str| base.join(name).to_string_lossy().into_owned();
    [
        vec![sdl.to_string_lossy().into_owned()],
        vec!["--info".to_string(), format!("root={}", p("info-root.txt"))],
        vec![
            "--info".to_string(),
            format!("ns=test={}", p("info-ns.txt")),
        ],
        vec![
            "--info".to_string(),
            format!("db=test/scratch={}", p("info-db.txt")),
        ],
    ]
    .concat()
}

/// The slot region exactly as the draft renders it for the fixture's
/// `legacy` table — the fill template the legs splice out.
fn legacy_slot_region() -> String {
    "\
<<<<<<< SURFORM SLOT (db:test/scratch: table legacy)
-- - Write the export, archive, or move query between the
--   markers; it runs before the removal, in this same
--   transaction.
-- e.g. INSERT INTO archive (SELECT * FROM legacy);
>>>>>>> SURFORM SLOT END
"
    .to_string()
}

/// The FIELD-slot fixture: the SDL declares `user` alone; the
/// database's `user` carries an extra `gone` field — DatabaseOnly
/// drift whose removal is catalog-only, so the draft carries the slot
/// ALONE, no WARNING prose (every byte between the defines and the
/// region is apostrophe-free — the clean-repro shape:
/// the table-slot layout's WARNING prose carries its own apostrophe
/// and would re-sync the walk before the region).
fn field_fixture(dir: &str) -> Vec<String> {
    let base = std::path::Path::new(dir);
    let sdl = base.join("schema.surdl");
    std::fs::write(
        &sdl,
        "namespace test;\ndatabase scratch;\ntable `user` {\n  schemaless;\n};\n",
    )
    .expect("the sdl writes");
    std::fs::write(
        base.join("info-root.txt"),
        "{ namespaces: { test: 'DEFINE NAMESPACE test' } }\n",
    )
    .expect("root writes");
    std::fs::write(
        base.join("info-ns.txt"),
        "{ databases: { scratch: 'DEFINE DATABASE scratch' } }\n",
    )
    .expect("ns writes");
    std::fs::write(
        base.join("info-db.txt"),
        "{\n  tables: { user: 'DEFINE TABLE user TYPE NORMAL SCHEMALESS' },\n  \"@tables\": { user: { fields: { gone: 'DEFINE FIELD gone ON user TYPE string' }, indexes: {  }, events: {  } } } }\n",
    )
    .expect("info writes");
    let p = |name: &str| base.join(name).to_string_lossy().into_owned();
    [
        vec![sdl.to_string_lossy().into_owned()],
        vec!["--info".to_string(), format!("root={}", p("info-root.txt"))],
        vec![
            "--info".to_string(),
            format!("ns=test={}", p("info-ns.txt")),
        ],
        vec![
            "--info".to_string(),
            format!("db=test/scratch={}", p("info-db.txt")),
        ],
    ]
    .concat()
}

// ---------------------------------------------------------------------
// `generate --save`: the numbered, marked draft
// ---------------------------------------------------------------------

/// The saved draft: numbered file under the default `migrations/`,
/// the five-line header, the slot region delimited by the bare
/// markers (the WARNING anchor persists as a comment), stdout
/// carrying the report and the draft's path — and the PLAIN run's
/// stdout staying marker-free with the comment banners (the
/// carriers' split).
#[test]
fn save_writes_the_numbered_marked_draft() {
    let dir = workdir("save");
    let args = info_fixture(&dir, true);
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .arg("--save")
        .assert()
        .success(); // a successful save exits 0 — the draft is the artifact (#71)
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("\u{2212} db:test/scratch table legacy (required data-migration slot)\n"),
        "stdout: {stdout}"
    );
    assert_eq!(
        stdout.lines().nth_back(1).expect("the path line"),
        "migrations/0001_schema.surql",
        "stdout: {stdout}"
    );
    // The next move is named (issue #90: --save stdout names the
    // command, not just the file).
    assert_eq!(
        stdout.lines().last().expect("the next-move line"),
        "fill the SURFORM SLOT regions, then: surform migrate commit migrations/0001_schema.surql",
        "stdout: {stdout}"
    );
    let draft =
        std::fs::read_to_string(std::path::Path::new(&dir).join("migrations/0001_schema.surql"))
            .expect("the draft exists");
    // The header: the format contract's five lines, the stem-named
    // slug, the source as given (the fixture hands absolute paths) —
    // pinned through the parser, plus the raw first line.
    assert!(
        draft.starts_with("-- surform migration 0001 — schema\n-- generated: "),
        "the draft:\n{draft}"
    );
    let parsed = surform::migrate::parse_draft(&draft).expect("the draft parses");
    assert_eq!(parsed.header.number, 1);
    assert_eq!(parsed.header.name, "schema");
    assert!(
        parsed.header.generated.starts_with('2') && parsed.header.generated.ends_with('Z'),
        "the generated stamp: {:?}",
        parsed.header.generated
    );
    assert_eq!(parsed.header.source, args[0]);
    // The `-- input:` line records the diff's inputs (issue #75):
    // the fixture's three tag specs, verbatim and in order (the args
    // interleave the `--info` flags: [file, --info, spec, ...]).
    assert_eq!(
        parsed.header.input,
        surform::migrate::InputMode::Info {
            tags: vec![args[2].clone(), args[4].clone(), args[6].clone()]
        }
    );
    assert_eq!(parsed.header.committed, None);
    // The slot region: bare markers with the slot's name, the shared
    // guidance prose inside, the WARNING anchor OUTSIDE (permanent).
    assert!(draft.contains(&legacy_slot_region()), "the draft:\n{draft}");
    assert!(draft.contains("-- >>> SURFORM WARNING: DATA LOSS <<<\n"));
    assert!(!draft.starts_with("BEGIN TRANSACTION;\n")); // the header precedes the frame
    assert!(draft.contains("BEGIN TRANSACTION;\n") && draft.ends_with("COMMIT;\n"));
    // The stdout path never renders markers and keeps the banners.
    let plain = cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .assert()
        .failure();
    let stdout = String::from_utf8(plain.get_output().stdout.clone()).expect("UTF-8");
    assert!(!stdout.contains("<<<<<<<"), "stdout: {stdout}");
    assert!(
        stdout.contains("-- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("== migration script =="),
        "stdout: {stdout}"
    );
}

/// The draft GROUP (issue #87): a TO file spanning two databases
/// renders a two-frame script — one transaction per scope section —
/// and `--save` writes it as a GROUP: one draft per scope section
/// (`0001a_…`/`0001b_…`), each file exactly one transaction, the same
/// header identity on every member plus its own `-- group:` line, the
/// stdout naming both members and their commit commands in name
/// order. The stdout script still prints, the direct-apply carrier.
#[test]
fn save_writes_a_multi_scope_draft_group() {
    let dir = workdir("save-multiscope");
    let from = std::path::Path::new(&dir).join("v1.surdl");
    let to = std::path::Path::new(&dir).join("v2.surdl");
    std::fs::write(
        &from,
        "namespace test;\ndatabase scratch;\ntable a;\ndatabase other;\ntable b;\n",
    )
    .expect("v1 writes");
    std::fs::write(
        &to,
        "namespace test;\ndatabase scratch;\ntable a2;\ndatabase other;\ntable b2;\n",
    )
    .expect("v2 writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate", "--from"])
        .arg(&from)
        .args(["--to"])
        .arg(&to)
        .arg("--save")
        .assert()
        .success(); // a successful save exits 0 — the draft is the artifact (#71)
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    // The stdout names both members, in NAME order, each with its next
    // move.
    let member_line = |at: char| format!("migrations/0001{at}_v2.surql");
    assert!(
        stdout.contains(&format!(
            "== draft ==\n{}\n{}",
            member_line('a'),
            member_line('b')
        )),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "fill the SURFORM SLOT regions, then: surform migrate commit {}",
            member_line('a')
        )) && stdout.contains(&format!(
            "fill the SURFORM SLOT regions, then: surform migrate commit {}",
            member_line('b')
        )),
        "stdout: {stdout}"
    );
    // The members: one transaction each, the same identity, own group
    // line; the databases apply in the emitter's sorted order —
    // `other` (a) before `scratch` (b) — each with its hoisted full
    // USE switch.
    let mut source_stamps = Vec::new();
    for (at, database) in [('a', "other"), ('b', "scratch")] {
        let path = std::path::Path::new(&dir).join(member_line(at));
        let text = std::fs::read_to_string(&path).expect("the member exists");
        assert_eq!(text.matches("BEGIN TRANSACTION;").count(), 1, "{text}");
        assert_eq!(text.matches("COMMIT;").count(), 1, "{text}");
        let parsed = surform::migrate::parse_draft(&text).expect("the member parses");
        assert_eq!(parsed.header.number, 1);
        let group = parsed.header.group.expect("the group line");
        assert_eq!(group.letter, at);
        assert_eq!(group.total, 2);
        assert_eq!(group.is_last(), at == 'b');
        assert_eq!(parsed.header.source, to.to_string_lossy());
        source_stamps.push((parsed.header.generated.clone(), parsed.header.input.clone()));
        assert!(
            text.contains(&format!("-- section: db:test/{database} ")),
            "{text}"
        );
        assert!(
            text.contains(&format!("USE NS test DB {database};")),
            "{text}"
        );
        let foreign = if at == 'a' { "scratch" } else { "other" };
        assert!(
            !text.contains(&format!("USE NS test DB {foreign};")),
            "{text}"
        );
    }
    assert_eq!(source_stamps[0], source_stamps[1], "one run, one identity");
    // The direct-apply carrier still prints the whole script.
    let plain = cli()
        .current_dir(&dir)
        .args(["migrate", "generate", "--from"])
        .arg(&from)
        .args(["--to"])
        .arg(&to)
        .assert()
        .failure();
    let stdout = String::from_utf8(plain.get_output().stdout.clone()).expect("UTF-8");
    assert!(stdout.contains("== migration script =="), "{stdout}");
    assert_eq!(stdout.matches("BEGIN TRANSACTION;").count(), 2, "{stdout}");
}

/// `--save --replace` on a multi-scope schema refuses: the replace
/// takes ONE draft's number with ONE script, and a group is neither —
/// the refusal names the group save and the delete-by-hand repair.
#[test]
fn replace_refuses_a_multi_scope_script() {
    let dir = workdir("replace-multiscope");
    let from = std::path::Path::new(&dir).join("v1.surdl");
    let to = std::path::Path::new(&dir).join("v2.surdl");
    std::fs::write(
        &from,
        "namespace test;\ndatabase scratch;\ntable a;\ndatabase other;\ntable b;\n",
    )
    .expect("v1 writes");
    std::fs::write(
        &to,
        "namespace test;\ndatabase scratch;\ntable a2;\ndatabase other;\ntable b2;\n",
    )
    .expect("v2 writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate", "--from"])
        .arg(&from)
        .args(["--to"])
        .arg(&to)
        .args(["--save", "--replace", "1"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("2 transaction frames"), "{stderr}");
    assert!(stderr.contains("draft GROUP"), "{stderr}");
    assert!(
        !std::path::Path::new(&dir).join("migrations").exists(),
        "nothing was saved"
    );
}

/// Numbering: the next number is the highest existing draft's plus
/// one; `--name` overrides the stem default (slug-folded); `--dir`
/// moves the drafts directory; foreign files never join the count.
#[test]
fn numbering_names_and_dirs() {
    let dir = workdir("numbering");
    let args = info_fixture(&dir, true);
    let run = |extra: &[&str]| {
        cli()
            .current_dir(&dir)
            .args(["migrate", "generate"])
            .args(&args)
            .args(extra)
            .assert()
            .success() // every call here saves — the save exits 0 (#71)
    };
    run(&["--save"]);
    run(&["--save", "--name", "Drop Legacy!"]);
    // A foreign file never becomes a number.
    std::fs::write(std::path::Path::new(&dir).join("migrations/notes.txt"), "x")
        .expect("the foreign file");
    run(&["--save"]);
    let migrations = std::path::Path::new(&dir).join("migrations");
    let mut names: Vec<String> = std::fs::read_dir(&migrations)
        .expect("the dir reads")
        .map(|entry| {
            entry
                .expect("the entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "0001_schema.surql".to_string(),
            "0002_drop-legacy.surql".to_string(),
            "0003_schema.surql".to_string(),
            "notes.txt".to_string(),
        ]
    );
    // The override rides `--dir`; the other directory's numbering is
    // its own.
    run(&["--save", "--dir", "drafts"]);
    assert!(
        std::path::Path::new(&dir)
            .join("drafts/0001_schema.surql")
            .exists(),
        "the overridden directory holds its own 0001"
    );
}

/// The pile-up guard (issue #74): a second `--save` of the SAME
/// script notes the existing draft on stdout (informational — the
/// save proceeds), and `--save --replace <n>` takes the named draft's
/// number — the stale file gone, no third draft stacked.
#[test]
fn save_notes_an_identical_draft_and_replace_takes_its_number() {
    let dir = workdir("pileup");
    let args = info_fixture(&dir, true);
    let run = |extra: &[&str]| {
        cli()
            .current_dir(&dir)
            .args(["migrate", "generate"])
            .args(&args)
            .args(extra)
            .assert()
            .success()
    };
    run(&["--save"]);
    let assert = run(&["--save"]);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("note: an existing draft carries this exact script: 0001_schema.surql\n"),
        "the note names the existing draft: {stdout}"
    );
    let migrations = std::path::Path::new(&dir).join("migrations");
    // The replace: the new script takes 0002's place — the note stays
    // quiet (the replace IS the answer), the path line names the
    // taken number, and no 0003 stacks.
    let assert = run(&["--save", "--replace", "2"]);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(!stdout.contains("note: an existing draft"), "{stdout}");
    assert!(
        stdout.contains("\nmigrations/0002_schema.surql\n"),
        "the path line names the taken number: {stdout}"
    );
    let mut names: Vec<String> = std::fs::read_dir(&migrations)
        .expect("the dir reads")
        .map(|entry| {
            entry
                .expect("the entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "0001_schema.surql".to_string(),
            "0002_schema.surql".to_string()
        ],
        "the replace took the number; nothing stacked"
    );
}

/// `--replace`'s refusals (issue #74): a missing number, a draft
/// carrying a DIFFERENT script (both hashes named, the target
/// untouched), and a committed target (history is not replaceable);
/// `--replace` without `--save` is a usage error.
#[test]
fn replace_refuses_a_mismatched_missing_or_committed_target() {
    let dir = workdir("replace-refusals");
    let draft = saved_draft(&dir, &[]);
    // Missing number.
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(info_fixture(&dir, true))
        .args(["--save", "--replace", "99"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("no draft carries number 0099"), "{stderr}");
    // A different script: hand-edit the body first.
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    std::fs::write(
        &path,
        text.replace(
            "BEGIN TRANSACTION;\n",
            "BEGIN TRANSACTION;\n-- a fill in progress\n",
        ),
    )
    .expect("the edit writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(info_fixture(&dir, true))
        .args(["--save", "--replace", "1"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("does not carry this script") && stderr.contains("sha256:"),
        "{stderr}"
    );
    assert!(path.exists(), "the target survives the refusal");
    // Restore, fill, commit — then the committed refusal.
    std::fs::write(&path, text).expect("the restore writes");
    fill(&dir, &draft);
    cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "drop legacy"])
        .assert()
        .success();
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(info_fixture(&dir, true))
        .args(["--save", "--replace", "1"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("0001 is committed"), "{stderr}");
    // Without --save the flag is a usage error.
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(info_fixture(&dir, false))
        .args(["--replace", "1"])
        .assert()
        .code(2);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("Usage:"), "{stderr}");
}

/// `--save` and `--check` refuse each other at the CLI (usage error,
/// exit 2); `--dir`/`--name` without `--save` likewise.
#[test]
fn save_flag_arity() {
    let dir = workdir("arity");
    let args = info_fixture(&dir, false);
    for extra in [
        vec!["--save", "--check"],
        vec!["--dir", "drafts"],
        vec!["--name", "x"],
        vec!["--replace", "1"],
    ] {
        let assert = cli()
            .current_dir(&dir)
            .args(["migrate", "generate"])
            .args(&args)
            .args(&extra)
            .assert()
            .code(2);
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
        assert!(
            stderr.contains("Usage:"),
            "extra: {extra:?} — stderr: {stderr}"
        );
    }
    // A converged --save writes nothing and says so — one line.
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .arg("--save")
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(stdout, "(converged)\n", "{stdout}");
    assert!(!std::path::Path::new(&dir).join("migrations").exists());
}

// ---------------------------------------------------------------------
// the bare-FILE baseline (issue #101)
// ---------------------------------------------------------------------

/// The bare-FILE baseline (issue #101): the file alone is the TO side
/// with an EMPTY FROM side — the whole report rides apply-pending
/// (`+`, no removals, no slots), `--save` writes the numbered draft
/// whose header records the baseline input mode (`file path=…`), and
/// the draft commits as-is (nothing to fill). The report and the
/// script are byte-identical to the mode's longhand — the version
/// pair with a hand-made empty file on the from side — because the
/// baseline is that recipe with the scaffolding gone: one comparator,
/// one renderer, the input spelling alone.
#[test]
fn the_bare_file_baseline_saves_and_matches_the_empty_pair_longhand() {
    let sdl_text =
        "namespace test;\ndatabase scratch;\ntable `user` {\n  schemaless;\n};\ntable archive;\n";
    let dir = workdir("baseline");
    std::fs::write(std::path::Path::new(&dir).join("schema.surdl"), sdl_text)
        .expect("the sdl writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate", "schema.surdl", "--save"])
        .assert()
        .success(); // a successful save exits 0 — the draft is the artifact (#71)
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    // The summary counts the no-removals shape the baseline cannot
    // leave: an empty FROM side removes nothing, so nothing can
    // orphan data.
    assert!(
        stdout.starts_with(
            "== drift ==\n\
             4 findings - 4 defines, 0 removals, 0 data-migration slots, \
             0 staged tables\n"
        ),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("+ db:test/scratch table archive (apply-pending)\n"),
        "stdout: {stdout}"
    );
    assert_eq!(
        stdout.lines().nth_back(1).expect("the path line"),
        "migrations/0001_schema.surql",
        "stdout: {stdout}"
    );
    let draft =
        std::fs::read_to_string(std::path::Path::new(&dir).join("migrations/0001_schema.surql"))
            .expect("the draft exists");
    assert!(
        draft.contains("-- input: file path=schema.surdl\n"),
        "the draft:\n{draft}"
    );
    let parsed = surform::migrate::parse_draft(&draft).expect("the draft parses");
    assert_eq!(
        parsed.header.input,
        surform::migrate::InputMode::File {
            path: "schema.surdl".to_string()
        }
    );
    // The header's label round-trips through the strict parse.
    assert_eq!(
        surform::migrate::InputMode::from_label(&parsed.header.input.label()),
        Some(parsed.header.input.clone())
    );
    // The baseline removes nothing: no WARNING anchors, no slot
    // regions — the draft commits as-is. (The renderer's behavior
    // header legend names the marker vocabulary on every script, so
    // the assertions pin the anchor spellings, not the words.)
    assert!(
        !draft.contains("-- >>> SURFORM WARNING: DATA LOSS <<<\n"),
        "the draft:\n{draft}"
    );
    assert!(
        !draft.contains("<<<<<<< SURFORM SLOT"),
        "the draft:\n{draft}"
    );
    let commit = cli()
        .current_dir(&dir)
        .args([
            "migrate",
            "commit",
            "migrations/0001_schema.surql",
            "-m",
            "baseline: the schema as it stands",
        ])
        .assert()
        .success();
    let commit_out = String::from_utf8(commit.get_output().stdout.clone()).expect("UTF-8");
    assert!(commit_out.contains("committed"), "stdout: {commit_out}");

    // The longhand equivalence: the version pair with a hand-made
    // empty file on the from side — the recipe the docs taught before
    // the mode — produces the byte-identical report and script.
    let long = workdir("baseline-longhand");
    let long_path = std::path::Path::new(&long);
    std::fs::write(long_path.join("empty.surdl"), "").expect("the empty file writes");
    std::fs::write(long_path.join("schema.surdl"), sdl_text).expect("the sdl writes");
    let check_bare = cli()
        .current_dir(&dir)
        .args(["migrate", "generate", "schema.surdl", "--check"])
        .assert()
        .failure(); // the drift exit — every definition is apply-pending
    let check_pair = cli()
        .current_dir(&long)
        .args([
            "migrate",
            "generate",
            "--from",
            "empty.surdl",
            "--to",
            "schema.surdl",
            "--check",
        ])
        .assert()
        .failure();
    assert_eq!(
        check_bare.get_output().stdout,
        check_pair.get_output().stdout,
        "the reports diverge"
    );
    let plain_bare = cli()
        .current_dir(&dir)
        .args(["migrate", "generate", "schema.surdl"])
        .assert()
        .failure();
    let plain_pair = cli()
        .current_dir(&long)
        .args([
            "migrate",
            "generate",
            "--from",
            "empty.surdl",
            "--to",
            "schema.surdl",
        ])
        .assert()
        .failure();
    let plain_text = String::from_utf8(plain_bare.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        plain_text.contains("== migration script =="),
        "stdout: {plain_text}"
    );
    assert_eq!(
        plain_bare.get_output().stdout,
        plain_pair.get_output().stdout,
        "the scripts diverge"
    );
}

/// The refusal matrix around the new mode: a missing file still
/// refuses (the load's own diagnostic, before any mode logic), and an
/// empty SDL file converges — the baseline of nothing is nothing
/// (exit 0, one line, no drafts directory, `--save` included).
#[test]
fn the_bare_file_refusals_and_converged_shape() {
    let dir = workdir("baseline-refusals");
    // The missing file: the load refuses with its own diagnostic.
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate", "nope.surdl"])
        .assert()
        .failure();
    assert!(assert.get_output().stdout.is_empty());
    // An empty SDL file: nothing to define, nothing to converge.
    std::fs::write(std::path::Path::new(&dir).join("empty.surdl"), "")
        .expect("the empty file writes");
    for extra in [vec![], vec!["--save"]] {
        let assert = cli()
            .current_dir(&dir)
            .args(["migrate", "generate", "empty.surdl"])
            .args(&extra)
            .assert()
            .success();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
        assert_eq!(stdout, "(converged)\n", "{stdout}");
    }
    assert!(!std::path::Path::new(&dir).join("migrations").exists());
}

// ---------------------------------------------------------------------
// `migrate commit`: the refusals, then the happy path
// ---------------------------------------------------------------------

/// Saves one `legacy`-dropping draft into `dir` and returns its path
/// (unfilled).
fn saved_draft(dir: &str, extra: &[&str]) -> String {
    let args = info_fixture(dir, true);
    let mut all = vec!["migrate".to_string(), "generate".to_string()];
    all.extend(args.iter().cloned());
    all.push("--save".to_string());
    all.extend(extra.iter().map(|s| s.to_string()));
    let assert = cli().current_dir(dir).args(&all).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    stdout
        .lines()
        .nth_back(1)
        .expect("the path line")
        .to_string()
}

/// Replaces the slot region with the runnable fill (the user's edit).
fn fill(dir: &str, relative: &str) {
    let path = std::path::Path::new(dir).join(relative);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    let region = legacy_slot_region();
    assert!(
        text.contains(&region),
        "the draft carries the region:\n{text}"
    );
    std::fs::write(
        &path,
        text.replace(&region, "INSERT INTO archive (SELECT * FROM legacy);\n"),
    )
    .expect("the fill writes");
}

/// The refusals, in the contract's order: a missing message is
/// clap's (exit 2); an unfilled draft names every unresolved slot
/// (exit 1, never a bare refusal); a stray git token names the line;
/// a committed file refuses twice over — unchanged ("already
/// committed") and edited ("edited after its commit").
#[test]
fn commit_refusals() {
    let dir = workdir("refusals");
    let draft = saved_draft(&dir, &[]);
    // No message: a usage error before anything reads the file.
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft])
        .assert()
        .code(2);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("--message <TEXT>"), "{stderr}");
    // The unfilled draft: each slot named.
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("unresolved SURFORM SLOT region(s)")
            && stderr.contains("- db:test/scratch: table legacy"),
        "{stderr}"
    );
    // A stray git marker (not ours) is the malformed arm, named
    // verbatim.
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    std::fs::write(&path, format!("{text}<<<<<<< HEAD\n")).expect("the stray writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("malformed marker line(s)") && stderr.contains("<<<<<<< HEAD"),
        "{stderr}"
    );
    std::fs::write(&path, text).expect("the restore writes");
    // The happy path, then the re-commit refusal (unchanged file).
    fill(&dir, &draft);
    let assert = cli()
        .current_dir(&dir)
        .args([
            "migrate",
            "commit",
            &draft,
            "-m",
            "drop legacy after archiving",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        stdout.contains("committed migrations/0001_schema.surql"),
        "{stdout}"
    );
    assert!(
        stdout.contains("  ledger: _surform_migrations (number 0001)"),
        "{stdout}"
    );
    let hash_at = stdout.find("sha256:").expect("the hash line");
    let hash = stdout[hash_at + 7..].lines().next().unwrap().to_string();
    assert_eq!(hash.len(), 64, "{stdout}");
    // The header marks the commit; the hash is the FILLED script's
    // (the body minus the appended ledger region), re-derived the
    // way status will.
    let committed = std::fs::read_to_string(&path).expect("the committed file reads");
    let redraft = surform::migrate::parse_draft(&committed).expect("the committed file parses");
    let recorded = redraft
        .header
        .committed
        .as_ref()
        .expect("the commit is marked");
    assert_eq!(recorded.hash, hash);
    assert_eq!(recorded.message, "drop legacy after archiving");
    assert_eq!(
        surform::migrate::sha256_hex(surform::migrate::filled_script(&redraft.body).as_bytes()),
        hash,
        "the recorded hash identifies the filled script"
    );
    // The ledger statements ride INSIDE the transaction.
    let commit_at = committed.rfind("COMMIT;").expect("the frame's commit");
    let insert_at = committed
        .find("INSERT IGNORE INTO _surform_migrations")
        .expect("the insert");
    assert!(
        insert_at < commit_at,
        "the ledger lands inside the transaction"
    );
    // Re-commit: the unchanged file refuses as already committed.
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "again"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("already committed"), "{stderr}");
    // The edited file refuses as post-commit drift, the recorded
    // hash named.
    let edited = committed.replace(
        "REMOVE TABLE IF EXISTS legacy;",
        "REMOVE TABLE IF EXISTS legacy; REMOVE TABLE IF EXISTS other;",
    );
    std::fs::write(&path, edited).expect("the edit writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "again"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("edited after its commit") && stderr.contains(&format!("sha256:{hash}")),
        "{stderr}"
    );
}

/// A filled draft carrying statements AFTER its transaction frame
/// refuses at commit — both defect spellings, each with the offending
/// line named. The engine runs the frame and a tail independently
/// (probed 3.3.0): a tail failure would leave the frame committed —
/// its self-recording ledger row surviving — under a false rollback
/// report, and a re-run would skip the file as already applied, so
/// the tail would never apply. The defect is refused where it would
/// ENTER the ledger's history.
#[test]
fn commit_refuses_statements_outside_the_frame() {
    let dir = workdir("trailing");
    let draft = saved_draft(&dir, &[]);
    fill(&dir, &draft);
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    // The appended-tail spelling: one statement below the frame close.
    std::fs::write(&path, format!("{text}DEFINE TABLE smuggled;\n")).expect("the tail writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("outside its one transaction frame")
            && stderr.contains("`DEFINE TABLE smuggled;`"),
        "{stderr}"
    );
    // The second-block spelling: the FIRST `COMMIT;` anchors, so the
    // second block's BEGIN is the named offender (the ledger would
    // splice into the LAST commit, outside the certified frame).
    std::fs::write(
        &path,
        format!("{text}BEGIN TRANSACTION;\nDEFINE TABLE second;\nCOMMIT;\n"),
    )
    .expect("the second block writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("outside its one transaction frame")
            && stderr.contains("`BEGIN TRANSACTION;`"),
        "the second block's BEGIN is the offender: {stderr}"
    );
    // The non-canonical close spelling: SurrealQL folds
    // case and lexer whitespace, so a hand-forged `commit;` close is
    // the real frame close to the engine — the guard anchors on it and
    // the tail below it refuses (the exact-match anchor let it ride).
    let forged = text.replace("COMMIT;", "commit;");
    std::fs::write(&path, format!("{forged}DEFINE TABLE smuggled;\n"))
        .expect("the forged close writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("outside its one transaction frame")
            && stderr.contains("`DEFINE TABLE smuggled;`"),
        "the tail below the forged close is the offender: {stderr}"
    );
    // An unterminated block comment after the close: the
    // scan cannot bound what the engine reads past it, so the block's
    // opening line refuses at commit — never a file every push would
    // 400 at the request layer.
    std::fs::write(&path, format!("{text}/* note")).expect("the open block writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("outside its one transaction frame") && stderr.contains("`/* note`"),
        "the open block's opening line is the offender: {stderr}"
    );
    // Nothing was committed: the file still parses as a draft.
    let left = std::fs::read_to_string(&path).expect("the file reads");
    let redraft = surform::migrate::parse_draft(&left).expect("still parses");
    assert!(
        redraft.header.committed.is_none(),
        "the refused draft never committed"
    );
}

/// The recorded hash is a FIXED POINT of commit's own rewrite:
/// a draft whose body lacks the trailing newline commits, and
/// the unchanged file's immediate re-commit says "already committed"
/// (never "edited") — the splice's newline-normalized spelling is
/// what was hashed. A one-byte edit INSIDE the hashed region flips
/// it ("edited after its commit"). And the hand-forged ledger banner
/// — the accepted descope — now truly descopes: the forged region is
/// out of the hash, the commit is consistent from birth, and the
/// file re-commits clean.
#[test]
fn the_recorded_hash_is_a_fixed_point_of_the_rewrite() {
    // (a) The missing trailing newline.
    let dir = workdir("fixed-point");
    let draft = saved_draft(&dir, &[]);
    fill(&dir, &draft);
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    assert!(text.ends_with("COMMIT;\n"), "{text}");
    std::fs::write(&path, text.trim_end_matches('\n')).expect("the newline strips");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "no trailing newline"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    let hash_at = stdout.find("sha256:").expect("the hash line");
    let hash = stdout[hash_at + 7..].lines().next().unwrap().to_string();
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "again"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("already committed"), "{stderr}");
    // A one-byte edit inside the hashed region flips the hash.
    let committed = std::fs::read_to_string(&path).expect("the committed file reads");
    let edited = committed.replacen("legacy;", "legach;", 1);
    assert_ne!(edited, committed, "the edit lands");
    std::fs::write(&path, edited).expect("the edit writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "again"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("edited after its commit") && stderr.contains(&format!("sha256:{hash}")),
        "{stderr}"
    );

    // (b) The hand-forged ledger banner above the user's own
    // statement, pre-commit.
    let dir = workdir("fixed-point-banner");
    let draft = saved_draft(&dir, &[]);
    fill(&dir, &draft);
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    let forged = text.replacen(
        "COMMIT;\n",
        "-- surform: migration ledger — appended by `surform migrate commit`; \
         do not edit.\nDEFINE TABLE IF NOT EXISTS forged;\nCOMMIT;\n",
        1,
    );
    assert_ne!(forged, text, "the banner lands");
    std::fs::write(&path, forged).expect("the forgery writes");
    cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "forged banner"])
        .assert()
        .success();
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "again"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("already committed"), "{stderr}");
}

/// The probe, executed against the real CLI: a draft whose
/// HEADER is CRLF'd while the script region stayed LF — the mixed
/// shape whose drifted offsets once spliced the committed status line
/// mid-line (a garbled file, exit 0). Line endings normalize at
/// parse, so the commit produces a correct machine-readable header,
/// the file is clean LF, the recorded hash re-derives from it, and
/// the unchanged file's re-commit says "already committed".
#[test]
fn the_mixed_crlf_draft_commits_clean() {
    let dir = workdir("mixed-crlf");
    let draft = saved_draft(&dir, &[]);
    fill(&dir, &draft);
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    let header_end = text.find("BEGIN TRANSACTION;").expect("the frame");
    let mixed = format!(
        "{}{}",
        text[..header_end].replace('\n', "\r\n"),
        &text[header_end..]
    );
    assert_ne!(mixed, text, "the header is CRLF'd");
    std::fs::write(&path, mixed).expect("the mixed endings write");
    cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "mixed endings"])
        .assert()
        .success();
    let committed = std::fs::read_to_string(&path).expect("the committed file reads");
    // No `\r` garbage anywhere — the corruption shape
    // (`-- status: … committed sha256:…proberaft^M`) is gone.
    assert!(!committed.contains('\r'), "{committed}");
    let parsed = surform::migrate::parse_draft(&committed).expect("the header parses");
    let recorded = parsed.header.committed.as_ref().expect("marked committed");
    assert_eq!(recorded.message, "mixed endings");
    let status = committed.lines().nth(5).expect("the status line");
    assert!(
        status.starts_with("-- status: committed sha256:") && status.ends_with(" mixed endings"),
        "{status}"
    );
    // The recorded hash re-derives from the committed file, the way
    // status will.
    assert_eq!(
        surform::migrate::sha256_hex(surform::migrate::filled_script(&parsed.body).as_bytes()),
        recorded.hash,
        "the recorded hash survives the rewrite"
    );
    // The ledger block rode inside the transaction.
    assert!(committed.contains("INSERT IGNORE INTO _surform_migrations"));
    // Re-commit the unchanged file: already committed, never edited.
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "again"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("already committed"), "{stderr}");
}

/// The missing final `COMMIT;` (a hand-truncated tail): the BEGIN
/// frame plainly stands, so the refusal names the missing terminator
/// — the position the ledger block splices before — never the BEGIN
/// frame. Fail-closed before any write.
#[test]
fn the_missing_final_commit_refusal_names_the_terminator() {
    let dir = workdir("missing-commit");
    let draft = saved_draft(&dir, &[]);
    fill(&dir, &draft);
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    let truncated = text.replace("COMMIT;\n", "");
    assert_ne!(truncated, text, "the terminator strips");
    std::fs::write(&path, truncated).expect("the truncation writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "truncated"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("final `COMMIT;` line is missing"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("no `BEGIN TRANSACTION;` frame"),
        "{stderr}"
    );
    // Fail-closed before any write: the file still parses as an
    // uncommitted draft.
    let after = std::fs::read_to_string(&path).expect("the file reads");
    let parsed = surform::migrate::parse_draft(&after).expect("still a draft");
    assert_eq!(parsed.header.committed, None);
}

/// The angle-ident probe, END TO END through the real CLI:
/// a field-slot draft whose script line spells a table as
/// `⟨stanley's table⟩` — the apostrophe-in-a-name shape angle quotes
/// exist for — with the slot region left UNRESOLVED and one later
/// apostrophe restoring quote parity. The pre-fix walk opened a
/// spurious string at the ident's apostrophe (where the lexer is
/// inert inside `⟨…⟩`), swallowed both marker lines, and the later
/// `-- don't` closed it at EOF-adjacent code position: no fallback,
/// `Ok([])`, commit BLESSED the unresolved draft (exit 0). The scan's
/// `Carried::Angle` state names the slot instead, and the refusal
/// leaves the file byte-untouched.
#[test]
fn the_angle_ident_draft_refuses_naming_the_slot() {
    let dir = workdir("angle-ident");
    let args = field_fixture(&dir);
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .arg("--save")
        .assert()
        .success(); // the save exits 0 — the draft is the artifact (#71)
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    let draft = stdout
        .lines()
        .nth_back(1)
        .expect("the path line")
        .to_string();
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    // The marker name carries the ON edge (issue #86) — the
    // manifest's line, not a trimmed one.
    assert!(
        text.contains("<<<<<<< SURFORM SLOT (db:test/scratch: field gone ON user)"),
        "the field slot renders: {text}"
    );
    // The hostile hand-edit: the filled query line before the region,
    // the region unresolved, the parity apostrophe after it.
    let hostile = text
        .replace(
            "BEGIN TRANSACTION;\n",
            "BEGIN TRANSACTION;\nINSERT INTO ⟨stanley's table⟩ (SELECT * FROM user);\n",
        )
        .replace("COMMIT;\n", "-- don't forget the archive\nCOMMIT;\n");
    assert!(hostile.contains("⟨stanley's table⟩"), "{hostile}");
    std::fs::write(&path, &hostile).expect("the hostile write");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("unresolved SURFORM SLOT region(s)")
            && stderr.contains("- db:test/scratch: field gone"),
        "{stderr}"
    );
    // The refusal is scan-level: the file is byte-untouched.
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file reads"),
        hostile,
        "the refusal leaves the draft byte-untouched"
    );
}

/// The regex probe, END TO END through the real CLI: the
/// filled line spells a regex literal (`/O'Brien/` — the apostrophe
/// is the natural in-regex spelling, and the parser enters
/// `compound::regex` at the value-position `/` token the lexer
/// dispatch does not route) with the slot region left UNRESOLVED and
/// one later apostrophe restoring quote parity. The pre-fix walk
/// opened a spurious string at the apostrophe and BLESSED the
/// unresolved draft (exit 0 — probe-proven); the scan's
/// `Carried::Regex` region refuses instead, naming the slot, the
/// file byte-untouched.
#[test]
fn the_regex_literal_draft_refuses_naming_the_slot() {
    let dir = workdir("regex-literal");
    let args = field_fixture(&dir);
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .arg("--save")
        .assert()
        .success(); // the save exits 0 — the draft is the artifact (#71)
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    let draft = stdout
        .lines()
        .nth_back(1)
        .expect("the path line")
        .to_string();
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    // The hostile hand-edit: the regex-literal filled line before the
    // region, the region unresolved, the parity apostrophe after it.
    let hostile = text
        .replace(
            "BEGIN TRANSACTION;\n",
            "BEGIN TRANSACTION;\nUPDATE person SET ok = /O'Brien/;\n",
        )
        .replace("COMMIT;\n", "-- don't forget the archive\nCOMMIT;\n");
    assert!(hostile.contains("/O'Brien/"), "{hostile}");
    std::fs::write(&path, &hostile).expect("the hostile write");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("unresolved SURFORM SLOT region(s)")
            && stderr.contains("- db:test/scratch: field gone"),
        "{stderr}"
    );
    // The refusal is scan-level: the file is byte-untouched.
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file reads"),
        hostile,
        "the refusal leaves the draft byte-untouched"
    );
}

/// The clean leg END TO END: a filled draft whose script carries
/// a regex literal AND a script function — no marker-shaped lines —
/// scans clean and COMMITS (hash, splice, header mark), then
/// re-commits as already committed (the recorded hash re-derives
/// from the regex/JS-bearing body).
#[test]
fn the_regex_and_js_draft_commits_clean() {
    let dir = workdir("regex-js");
    let draft = saved_draft(&dir, &[]);
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    let filled = text.replace(
        &legacy_slot_region(),
        "UPDATE person SET ok = /O'Brien/, f = function(){ return 1; };\n",
    );
    assert_ne!(filled, text, "the fill lands");
    std::fs::write(&path, &filled).expect("the fill writes");
    cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "regex and js"])
        .assert()
        .success();
    let committed = std::fs::read_to_string(&path).expect("the committed file reads");
    assert!(
        committed.contains("-- status: committed sha256:"),
        "{committed}"
    );
    // Once per file: the unchanged file re-commits as already done —
    // the hash machinery rode the regex/JS body byte-faithfully.
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "again"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("already committed"), "{stderr}");
}

/// The JS-spacing probe, END TO END through the real CLI:
/// the filled line carries a script function whose word `function`
/// is separated from its argument list by a NON-BREAKING SPACE (the
/// standard copy-paste artifact class), the probe's quote lines
/// inside the body, the slot region left UNRESOLVED, and one later
/// apostrophe restoring quote parity. The parser ACCEPTS the
/// spelling (the lexer's `eat_whitespace` takes NBSP as token
/// spacing, so `compound::javascript` opens); the pre-fix walk
/// folded the NBSP into the ident, never entered the compound, and
/// let `c'd;` open a spurious string that swallowed both marker
/// lines — `Ok([])`, commit BLESSED the unresolved draft (exit 0,
/// probe-proven). The lexer-faithful spacing set opens the compound
/// instead: the refusal names the slot, the file byte-untouched.
#[test]
fn the_unicode_spacing_draft_refuses_naming_the_slot() {
    let dir = workdir("unicode-spacing");
    let args = field_fixture(&dir);
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .arg("--save")
        .assert()
        .success(); // the save exits 0 — the draft is the artifact (#71)
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    let draft = stdout
        .lines()
        .nth_back(1)
        .expect("the path line")
        .to_string();
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    // The hostile hand-edit: the JS compound opened on the NBSP
    // spelling before the region, the probe's quote lines inside the
    // body, the region unresolved, the parity apostrophe after it.
    let hostile = text
        .replace(
            "BEGIN TRANSACTION;\n",
            "BEGIN TRANSACTION;\n\
             LET $f = function\u{A0}() {\n\
             -- a'b\n\
             c'd;\n\
             };\n",
        )
        .replace("COMMIT;\n", "-- t'ag\nCOMMIT;\n");
    assert!(hostile.contains("function\u{A0}()"), "{hostile}");
    std::fs::write(&path, &hostile).expect("the hostile write");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("unresolved SURFORM SLOT region(s)")
            && stderr.contains("- db:test/scratch: field gone"),
        "{stderr}"
    );
    // The refusal is scan-level: the file is byte-untouched.
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file reads"),
        hostile,
        "the refusal leaves the draft byte-untouched"
    );
}

/// The probe, END TO END through the real CLI: the draft's
/// single unresolved slot (its name carries the scope's `/` —
/// `db:test/scratch`) plus a raw-newline string whose content line is
/// a stray `<<<<<<< HEAD` (data quoting git conflict text).
/// The pre-fix walk advanced through the claimed opener as
/// code, the name's `/` opened a spurious regex region that reached
/// EOF, and the verdict fell to the line-based fallback — whose
/// malformed arm named the STRING's `<<<<<<< HEAD` line while the
/// real unresolved slot went unlisted (an invitation to "restore the
/// marker shape" inside a string literal). A claimed marker line is
/// tool text, not code: the walk stays put, the string content is
/// inert, and the refusal is the unresolved-slot one naming the REAL
/// slot, the file byte-untouched.
#[test]
fn the_head_in_a_string_draft_names_the_real_slot() {
    let dir = workdir("head-in-string");
    let args = field_fixture(&dir);
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .arg("--save")
        .assert()
        .success(); // the save exits 0 — the draft is the artifact (#71)
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    let draft = stdout
        .lines()
        .nth_back(1)
        .expect("the path line")
        .to_string();
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    assert!(
        text.contains("<<<<<<< SURFORM SLOT (db:test/scratch: field gone ON user)"),
        "the field slot renders: {text}"
    );
    // The hostile hand-edit: the region stays unresolved, and a
    // raw-newline string after it carries a marker-shaped line.
    let hostile = text.replace(
        "COMMIT;\n",
        "INSERT INTO audit (note) VALUES ('merge residue:\n<<<<<<< HEAD\n');\nCOMMIT;\n",
    );
    std::fs::write(&path, &hostile).expect("the hostile write");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("unresolved SURFORM SLOT region(s)")
            && stderr.contains("- db:test/scratch: field gone"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("malformed marker line(s)"),
        "the string content is not a malformed marker: {stderr}"
    );
    // The refusal is scan-level: the file is byte-untouched.
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file reads"),
        hostile,
        "the refusal leaves the draft byte-untouched"
    );
}

/// A hand-duplicated `-- status:` line refuses at commit: the parse
/// once took the LAST one, marking it while the
/// first (`-- status: draft`) survived into the committed file.
/// The duplicate is a header parse refusal — the file is
/// byte-untouched.
#[test]
fn the_duplicate_status_line_draft_refuses() {
    let dir = workdir("twin-status");
    let draft = saved_draft(&dir, &[]);
    fill(&dir, &draft);
    let path = std::path::Path::new(&dir).join(&draft);
    let text = std::fs::read_to_string(&path).expect("the draft reads");
    let doctored = text.replace("-- status: draft\n", "-- status: draft\n-- status: draft\n");
    assert_ne!(doctored, text, "the twin inserts");
    std::fs::write(&path, &doctored).expect("the doctored write");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("the `-- status:` line repeats"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file reads"),
        doctored,
        "the refusal leaves the draft byte-untouched"
    );
}

/// The probe, END TO END through the real CLI: the version
/// pair's TO-side schema is a file literally named `BEGIN
/// TRANSACTION;` (a legal path) — generate records the path as given
/// on the `-- source:` line, so the header text itself spells the
/// frame. The body start is the header loop's own frame-line offset,
/// so the lure line is inert: fill → commit runs clean end to end
/// (no panic — a panic is not an exit code), the recorded hash
/// re-derives from the committed file, and the unchanged file's
/// re-commit says "already committed". Pre-fix, the first-match
/// `text.find("BEGIN TRANSACTION;\n")` hit the header line and
/// `mark_committed` sliced a reversed range — panic, probe-proven.
#[test]
fn the_begin_transaction_named_source_commits_clean() {
    let dir = workdir("begin-frame-name");
    std::fs::write(
        std::path::Path::new(&dir).join("v1.surdl"),
        "namespace test;\ndatabase scratch;\ntable `user` {\n  schemaless;\n};\ntable legacy;\n",
    )
    .expect("v1 writes");
    std::fs::write(
        std::path::Path::new(&dir).join("BEGIN TRANSACTION;"),
        "namespace test;\ndatabase scratch;\ntable `user` {\n  schemaless;\n};\n",
    )
    .expect("the to-side writes");
    let assert = cli()
        .current_dir(&dir)
        .args([
            "migrate",
            "generate",
            "--from",
            "v1.surdl",
            "--to",
            "BEGIN TRANSACTION;",
            "--save",
        ])
        .assert()
        .success(); // the save exits 0 — the draft is the artifact (#71)
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout.lines().nth_back(1).expect("the path line"),
        "migrations/0001_begin-transaction.surql"
    );
    let draft = "migrations/0001_begin-transaction.surql";
    let text =
        std::fs::read_to_string(std::path::Path::new(&dir).join(draft)).expect("the draft reads");
    assert!(
        text.contains("-- source: BEGIN TRANSACTION;\n"),
        "the header records the path as given: {text}"
    );
    fill(&dir, draft);
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", draft, "-m", "drop legacy"])
        .assert()
        .success();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        !stderr.contains("panicked"),
        "no panic is an exit code: {stderr}"
    );
    let committed =
        std::fs::read_to_string(std::path::Path::new(&dir).join(draft)).expect("the file reads");
    let parsed = surform::migrate::parse_draft(&committed).expect("the committed file parses");
    let recorded = parsed.header.committed.as_ref().expect("marked committed");
    assert_eq!(recorded.message, "drop legacy");
    assert_eq!(
        surform::migrate::sha256_hex(surform::migrate::filled_script(&parsed.body).as_bytes()),
        recorded.hash,
        "the recorded hash re-derives from the committed file"
    );
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", draft, "-m", "again"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("already committed"), "{stderr}");
}

/// The ledger-table override threads: BOTH the `DEFINE TABLE` and
/// the `INSERT IGNORE` name it.
#[test]
fn commit_threads_the_ledger_table() {
    let dir = workdir("ledger-table");
    let draft = saved_draft(&dir, &[]);
    fill(&dir, &draft);
    cli()
        .current_dir(&dir)
        .args([
            "migrate",
            "commit",
            &draft,
            "-m",
            "m",
            "--ledger-table",
            "app_migrations",
        ])
        .assert()
        .success();
    let committed = std::fs::read_to_string(std::path::Path::new(&dir).join(&draft))
        .expect("the committed file reads");
    assert!(
        committed.contains("DEFINE TABLE IF NOT EXISTS app_migrations SCHEMAFULL;"),
        "{committed}"
    );
    assert!(
        committed.contains("DEFINE FIELD IF NOT EXISTS number ON app_migrations TYPE int;"),
        "{committed}"
    );
    assert!(
        committed.contains("INSERT IGNORE INTO app_migrations (id, number,"),
        "{committed}"
    );
    assert!(!committed.contains("_surform_migrations"));
    // The EMPTY name is a usage error — the ledger block
    // would spell an empty identifier the embedded parser refuses at
    // apply. Clap rejects before anything reads the file.
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "m", "--ledger-table", ""])
        .assert()
        .code(2);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(stderr.contains("--ledger-table"), "{stderr}");
}

/// The two-branches-one-number collision: a second draft with the
/// same number refuses at commit (the ledger keys on the number —
/// two records could not exist).
#[test]
fn commit_refuses_a_duplicate_number() {
    let dir = workdir("dup");
    let draft = saved_draft(&dir, &[]);
    fill(&dir, &draft);
    // The other branch's twin: the same number, still an uncommitted
    // draft (its own history never saw a commit) — what a merge
    // delivers.
    let twin_text =
        std::fs::read_to_string(std::path::Path::new(&dir).join(&draft)).expect("the draft reads");
    cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "first"])
        .assert()
        .success();
    let twin = "migrations/0001_twin.surql";
    std::fs::write(std::path::Path::new(&dir).join(twin), &twin_text).expect("the twin writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", twin, "-m", "the twin"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("another draft already carries number 0001"),
        "{stderr}"
    );
    // The bare-filename spelling scans the working directory too
    // (an empty parent resolves to `.`): two filled drafts
    // sitting loose in the cwd, committed by name only, still
    // refuse on the shared number (the scan's `.` entries spell
    // `./0001_…`, so the exclusion keys on the file's own name).
    std::fs::write(
        std::path::Path::new(&dir).join("0001_bare.surql"),
        &twin_text,
    )
    .expect("the bare copy writes");
    std::fs::write(
        std::path::Path::new(&dir).join("0001_other.surql"),
        &twin_text,
    )
    .expect("the loose twin writes");
    let assert = cli()
        .current_dir(&dir)
        .args(["migrate", "commit", "0001_bare.surql", "-m", "bare"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    assert!(
        stderr.contains("another draft already carries number 0001"),
        "{stderr}"
    );
}

// ---------------------------------------------------------------------
// `--json` refusals (issue #73): fail() renders one one-line object
// ---------------------------------------------------------------------

/// The refusal's stderr as the parsed one-line JSON object: exactly
/// one line of stderr, it must parse as one JSON object, and `error`
/// is required (`hint` is optional — a message without a ` — ` split).
fn json_refusal_stderr(assert: &assert_cmd::assert::Assert) -> surform::migrate::json::Json {
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    let mut lines = stderr.lines();
    let line = lines.next().expect("one line of stderr");
    assert!(lines.next().is_none(), "exactly one line: {stderr}");
    let json = surform::migrate::json::parse(line).unwrap_or_else(|e| {
        panic!("one JSON object, got {line:?}: {e:?}");
    });
    assert!(
        json.get("error").and_then(|item| item.as_str()).is_some(),
        "{stderr}"
    );
    json
}

/// The re-commit refusal under `--json`: one object, the sentence the
/// error, the ` — ` tail the hint.
#[test]
fn json_already_committed_refusal_is_one_object() {
    let dir = workdir("json-recommit");
    let draft = saved_draft(&dir, &[]);
    fill(&dir, &draft);
    cli()
        .current_dir(&dir)
        .args(["migrate", "commit", &draft, "-m", "x"])
        .assert()
        .success();
    let assert = cli()
        .current_dir(&dir)
        .args(["--json", "migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let json = json_refusal_stderr(&assert);
    let error = json.get("error").and_then(|item| item.as_str()).unwrap();
    assert!(error.contains("already committed"), "{error}");
    assert!(error.contains("sha256:"), "{error}");
    let hint = json.get("hint").and_then(|item| item.as_str()).unwrap();
    assert!(hint.contains("commit is once per draft"), "{hint}");
}

/// The unresolved-slots refusal under `--json`: one object whose hint
/// still carries the per-slot lines (escaped newlines, one line of
/// stderr).
#[test]
fn json_unresolved_slots_refusal_is_one_object() {
    let dir = workdir("json-slots");
    let draft = saved_draft(&dir, &[]);
    let assert = cli()
        .current_dir(&dir)
        .args(["--json", "migrate", "commit", &draft, "-m", "x"])
        .assert()
        .failure();
    let json = json_refusal_stderr(&assert);
    let error = json.get("error").and_then(|item| item.as_str()).unwrap();
    assert!(
        error.contains("unresolved SURFORM SLOT region(s)"),
        "{error}"
    );
    let hint = json.get("hint").and_then(|item| item.as_str()).unwrap();
    assert!(hint.contains("replace each delimited region"), "{hint}");
    // The per-slot lines ride INSIDE the hint (the helper asserted the
    // stderr itself is one line — so these newlines were \-escaped on
    // the wire and the parser decoded them).
    assert!(
        hint.contains("\n  - db:test/scratch: table legacy"),
        "{hint}"
    );
}

/// The auth-pairing refusal under `--json` — pre-dial, so no network.
#[test]
fn json_auth_pairing_refusal_is_one_object() {
    let dir = workdir("json-auth");
    let assert = cli()
        .current_dir(&dir)
        .args([
            "--json",
            "migrate",
            "push",
            "--dir",
            &dir,
            "--endpoint",
            "http://127.0.0.1:8000",
            "--ns",
            "t",
            "--db",
            "t",
            "--user",
            "u",
        ])
        .env_remove("SURFORM_DB_USER")
        .env_remove("SURFORM_DB_PASS")
        .assert()
        .failure();
    let json = json_refusal_stderr(&assert);
    let error = json.get("error").and_then(|item| item.as_str()).unwrap();
    assert!(error.contains("without a password"), "{error}");
    let hint = json.get("hint").and_then(|item| item.as_str()).unwrap();
    assert!(hint.contains("SURFORM_DB_PASS"), "{hint}");
}

/// The token/Basic exclusivity refusal under `--json` — one object,
/// pre-dial, both credential kinds' spellings named in the hint.
#[test]
fn json_token_basic_conflict_refusal_is_one_object() {
    let dir = workdir("json-token");
    let assert = cli()
        .current_dir(&dir)
        .args([
            "--json",
            "migrate",
            "push",
            "--dir",
            &dir,
            "--endpoint",
            "http://127.0.0.1:8000",
            "--ns",
            "t",
            "--db",
            "t",
            "--token",
            "jwt",
            "--user",
            "u",
        ])
        .env_remove("SURFORM_DB_USER")
        .env_remove("SURFORM_DB_PASS")
        .env_remove("SURFORM_DB_TOKEN")
        .assert()
        .failure();
    let json = json_refusal_stderr(&assert);
    let error = json.get("error").and_then(|item| item.as_str()).unwrap();
    assert!(
        error.contains("a token is given") && error.contains("alongside a user or password"),
        "{error}"
    );
    assert!(
        error.contains("SURFORM_DB_TOKEN"),
        "the error names the token's spellings: {error}"
    );
    let hint = json.get("hint").and_then(|item| item.as_str()).unwrap();
    assert!(hint.contains("exclusive"), "{hint}");
    assert!(
        hint.contains("Bearer") && hint.contains("Basic"),
        "the hint names both credential kinds: {hint}"
    );
}

/// The missing-directory refusal under `--json` — push's pre-flight,
/// also pre-dial.
#[test]
fn json_missing_dir_refusal_is_one_object() {
    let dir = workdir("json-dir");
    let missing = std::path::Path::new(&dir).join("nope");
    let assert = cli()
        .current_dir(&dir)
        .args([
            "--json",
            "migrate",
            "push",
            "--dir",
            missing.to_str().unwrap(),
            "--endpoint",
            "http://127.0.0.1:8000",
            "--ns",
            "t",
            "--db",
            "t",
        ])
        .env_remove("SURFORM_DB_USER")
        .env_remove("SURFORM_DB_PASS")
        .assert()
        .failure();
    let json = json_refusal_stderr(&assert);
    let error = json.get("error").and_then(|item| item.as_str()).unwrap();
    assert!(error.contains("does not exist"), "{error}");
    let hint = json.get("hint").and_then(|item| item.as_str()).unwrap();
    assert!(hint.contains("--dir"), "{hint}");
}

// ---------------------------------------------------------------------
// The engine legs: the ledger's idempotence and the end-to-end
// ---------------------------------------------------------------------

/// The ledger block's spelling is executed — `ledger_statements`'s
/// exact output, run TWICE against the embedded engine: one row, no
/// error (the `INSERT IGNORE` skips the id the ledger already
/// holds), and the ledger table shows in `INFO FOR DB` (the
/// interchange probe, answered here for free).
#[tokio::test]
async fn the_ledger_block_executes_idempotently() {
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    let block = surform::migrate::ledger_statements(
        "_surform_migrations",
        7,
        "probe",
        &"a".repeat(64),
        "first apply",
        "namespace test;\ndatabase scratch;\n",
    );
    for _ in 0..2 {
        db.query(&block)
            .await
            .expect("the ledger block runs")
            .check()
            .expect("the ledger block applies");
    }
    use surrealdb::types::ToSql as _;
    let rows: surrealdb::types::Value = db
        .query("SELECT number, name, hash, message, sdl FROM _surform_migrations;")
        .await
        .expect("the read runs")
        .take(0)
        .expect("the read takes");
    let expected_hash = "a".repeat(64);
    assert_eq!(
        rows.to_sql(),
        format!(
            "[{{ hash: '{expected_hash}', message: 'first apply', name: 'probe', number: 7, \
             sdl: 'namespace test;\\ndatabase scratch;\\n' }}]"
        ),
        "exactly one row after the double apply: {rows:?}"
    );
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(
        info.contains("_surform_migrations"),
        "the ledger shows in INFO: {info}"
    );
}

/// The end-to-end leg (the unit's flagship): generate --save from the
/// v1→v2 version pair, fill the slot, commit, EXECUTE the committed
/// file's script against an engine built at v1 — the catalog
/// converges to v2, the move rides the fill, and the ledger lands
/// with the migration: one row, the recorded hash, the raw message,
/// and the `sdl` column holding the canonical TO-side text
/// (revert's substrate, byte-equal to `fmt`'s rendering of v2).
#[tokio::test]
async fn the_committed_file_executes_and_records_itself() {
    let dir = workdir("e2e");
    let (from, to) = version_pair(&dir);
    let assert = cli()
        .current_dir(&dir)
        .args([
            "migrate",
            "generate",
            "--from",
            &from,
            "--to",
            &to,
            "--save",
            "--name",
            "drop-legacy",
        ])
        .assert()
        .success(); // the save exits 0 — the draft is the artifact (#71)
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout.lines().nth_back(1).expect("the path line"),
        "migrations/0001_drop-legacy.surql"
    );
    fill(&dir, "migrations/0001_drop-legacy.surql");
    cli()
        .current_dir(&dir)
        .args([
            "migrate",
            "commit",
            "migrations/0001_drop-legacy.surql",
            "-m",
            "retire the legacy table",
        ])
        .assert()
        .success();
    let committed = std::fs::read_to_string(
        std::path::Path::new(&dir).join("migrations/0001_drop-legacy.surql"),
    )
    .expect("the committed file reads");
    let draft = surform::migrate::parse_draft(&committed).expect("the committed file parses");
    let recorded = draft.header.committed.as_ref().expect("marked committed");

    // The engine at the FROM state, then the committed script.
    let db = exec::mem_db().await;
    db.use_ns(NS).use_db(DB).await.ok();
    exec::exec_sdl(
        &db,
        "namespace test; database scratch; table `user` { schemaless; }; table legacy;",
    )
    .await;
    db.query("CREATE legacy SET x = 1; CREATE legacy SET x = 2;")
        .await
        .expect("the rows land")
        .check()
        .expect("the rows land");
    db.query(&draft.body)
        .await
        .expect("the committed script runs")
        .check()
        .expect("the committed script applies");

    // The catalog converged to v2; the fill moved the rows first.
    use surrealdb::types::ToSql as _;
    let moved: surrealdb::types::Value = db
        .query("SELECT value x FROM archive ORDER BY x;")
        .await
        .expect("the read runs")
        .take(0)
        .expect("the read takes");
    assert_eq!(moved.to_sql(), "[1, 2]", "the rows moved: {moved:?}");
    let info = exec::info_for(&db, "INFO FOR DB").await;
    assert!(!info.contains("legacy"), "the removal ran: {info}");
    assert!(info.contains("user"), "the surviving table: {info}");

    // The ledger: one row, self-recorded by the script's own run.
    let rows: surrealdb::types::Value = db
        .query("SELECT number, name, hash, message FROM _surform_migrations;")
        .await
        .expect("the ledger read runs")
        .take(0)
        .expect("the ledger read takes");
    assert_eq!(
        rows.to_sql(),
        format!(
            "[{{ hash: '{}', message: 'retire the legacy table', name: 'drop-legacy', number: 1 }}]",
            recorded.hash
        ),
        "one self-recorded row: {rows:?}"
    );
    // The `sdl` column holds the canonical TO-side text — revert's
    // substrate — byte-equal to the writer's rendering of v2 (the
    // value comes back as a Rust string, so the INSERT's
    // string-literal escapes are transparent to the claim).
    let (file, diags) =
        surform::parser::parse_file(&std::fs::read_to_string(&to).expect("v2 reads"));
    assert!(diags.is_empty());
    let canonical = surform::info::write::write_canonical(&file);
    let stored: surrealdb::types::Value = db
        .query("SELECT VALUE sdl FROM _surform_migrations")
        .await
        .expect("the sdl read runs")
        .take(0)
        .expect("the sdl read takes");
    // SELECT VALUE over the one-row table returns the one-element
    // array of the column's strings.
    let surrealdb::types::Value::Array(rows) = &stored else {
        panic!("the sdl read returns an array: {stored:?}");
    };
    let [sdl_value] = rows.as_slice() else {
        panic!("exactly one ledger row: {stored:?}");
    };
    let surrealdb::types::Value::String(stored_text) = sdl_value else {
        panic!("the sdl column holds a string: {sdl_value:?}");
    };
    assert_eq!(
        stored_text, &canonical,
        "the stored sdl IS the canonical v2"
    );
}

// -------------------------------------------------------------------
// The identity pair (issue #67): commit refuses a filename/header
// number mismatch, and `renumber` moves both together — the one
// operation that cannot split the identity push/status walk.
// -------------------------------------------------------------------

/// A saved draft renamed (the natural git-merge collision repair)
/// refuses commit: the mismatch names both numbers and both repairs.
#[test]
fn commit_refuses_a_number_mismatch() {
    let dir = workdir("mismatch");
    let args = info_fixture(&dir, true);
    cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .args(["--save"])
        .assert()
        .success();
    let draft = std::path::Path::new(&dir).join("migrations/0001_schema.surql");
    let renamed = std::path::Path::new(&dir).join("migrations/0009_schema.surql");
    std::fs::rename(&draft, &renamed).expect("the rename");
    let text = std::fs::read_to_string(&renamed).expect("the draft reads");
    std::fs::write(&renamed, text.replace(&legacy_slot_region(), "")).expect("the fill writes");
    let out = cli()
        .current_dir(&dir)
        .args(["migrate", "commit"])
        .arg(&renamed)
        .args(["-m", "renamed away from its number"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("0009"), "the file's number: {stderr}");
    assert!(stderr.contains("0001"), "the header's number: {stderr}");
    assert!(stderr.contains("renumber"), "the repair: {stderr}");
}

/// `renumber` moves the header's number and the file name together —
/// the healed mismatch commits clean.
#[test]
fn renumber_moves_header_and_filename_together() {
    let dir = workdir("renumber-ok");
    let args = info_fixture(&dir, true);
    cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .args(["--save"])
        .assert()
        .success();
    let draft = std::path::Path::new(&dir).join("migrations/0001_schema.surql");
    let renamed = std::path::Path::new(&dir).join("migrations/0009_schema.surql");
    std::fs::rename(&draft, &renamed).expect("the rename");
    cli()
        .current_dir(&dir)
        .args(["migrate", "renumber"])
        .arg(&renamed)
        .arg("0002")
        .assert()
        .success();
    let healed = std::path::Path::new(&dir).join("migrations/0002_schema.surql");
    assert!(!renamed.exists(), "the old name is gone");
    let text = std::fs::read_to_string(&healed).expect("the healed draft reads");
    let parsed = surform::migrate::parse_draft(&text).expect("the healed draft parses");
    assert_eq!(parsed.header.number, 2, "the header moved with the name");
    // The healed identity commits.
    let filled = text.replace(&legacy_slot_region(), "");
    std::fs::write(&healed, filled).expect("the fill writes");
    cli()
        .current_dir(&dir)
        .args(["migrate", "commit"])
        .arg(&healed)
        .args(["-m", "healed"])
        .assert()
        .success();
}

/// `renumber` refuses a number another draft already carries.
#[test]
fn renumber_refuses_a_collision() {
    let dir = workdir("renumber-clash");
    let args = info_fixture(&dir, true);
    for _ in 0..2 {
        cli()
            .current_dir(&dir)
            .args(["migrate", "generate"])
            .args(&args)
            .args(["--save"])
            .assert()
            .success();
    }
    let first = std::path::Path::new(&dir).join("migrations/0001_schema.surql");
    let out = cli()
        .current_dir(&dir)
        .args(["migrate", "renumber"])
        .arg(&first)
        .arg("0002")
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("0002"), "{stderr}");
    assert!(
        stderr.contains("0002_drop-legacy.surql") || stderr.contains("already"),
        "{stderr}"
    );
}

/// `renumber` refuses a COMMITTED migration — the number is the
/// ledger's record id; the renamed file would orphan the recorded
/// row.
#[test]
fn renumber_refuses_a_committed_file() {
    let dir = workdir("renumber-committed");
    let args = info_fixture(&dir, true);
    cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .args(["--save"])
        .assert()
        .success();
    let draft = std::path::Path::new(&dir).join("migrations/0001_schema.surql");
    let text = std::fs::read_to_string(&draft).expect("the draft reads");
    std::fs::write(&draft, text.replace(&legacy_slot_region(), "")).expect("the fill writes");
    cli()
        .current_dir(&dir)
        .args(["migrate", "commit"])
        .arg(&draft)
        .args(["-m", "committed first"])
        .assert()
        .success();
    let out = cli()
        .current_dir(&dir)
        .args(["migrate", "renumber"])
        .arg(&draft)
        .arg("0007")
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("committed"), "{stderr}");
    assert!(stderr.contains("ledger"), "{stderr}");
}

/// The bare-filename invocation (the natural form from inside the
/// migrations directory) still scans for collisions — an EMPTY
/// parent must normalize to `.`, never fail the scan open.
#[test]
fn renumber_from_inside_the_directory_still_refuses_a_collision() {
    let dir = workdir("renumber-bare");
    let args = info_fixture(&dir, true);
    for _ in 0..2 {
        cli()
            .current_dir(&dir)
            .args(["migrate", "generate"])
            .args(&args)
            .args(["--save"])
            .assert()
            .success();
    }
    let out = cli()
        .current_dir(std::path::Path::new(&dir).join("migrations"))
        .args(["migrate", "renumber"])
        .arg("0001_schema.surql")
        .arg("0002")
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("0002"), "{stderr}");
}

/// The header-only repair: a mismatched file (name 0009, header 0001)
/// renumbered to ITS OWN number fixes the header in place; the plain
/// create-new path would name the file as its own collision.
#[test]
fn renumber_to_the_files_own_number_rewrites_in_place() {
    let dir = workdir("renumber-self");
    let args = info_fixture(&dir, true);
    cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .args(["--save"])
        .assert()
        .success();
    let renamed = std::path::Path::new(&dir).join("migrations/0009_schema.surql");
    let draft = std::path::Path::new(&dir).join("migrations/0001_schema.surql");
    std::fs::rename(&draft, &renamed).expect("the rename");
    let before = std::fs::read_to_string(&renamed).expect("reads");
    let out = cli()
        .current_dir(&dir)
        .args(["migrate", "renumber"])
        .arg(&renamed)
        .arg("0009")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout);
    assert!(stdout.contains("renumbered in place"), "{stdout}");
    assert!(renamed.exists(), "the name stays");
    assert!(!draft.exists(), "no twin appears");
    let after = std::fs::read_to_string(&renamed).expect("reads");
    let parsed = surform::migrate::parse_draft(&after).expect("still parses");
    assert_eq!(
        parsed.header.number, 9,
        "the header moved to the name's number"
    );
    let mut before_lines = before.lines();
    let mut after_lines = after.lines();
    assert_ne!(
        before_lines.next(),
        after_lines.next(),
        "the number line moved"
    );
    assert_eq!(
        before_lines.collect::<Vec<_>>(),
        after_lines.collect::<Vec<_>>(),
        "every other byte is preserved"
    );
}

// -------------------------------------------------------------------
// The source pin (issue #68): the draft header records the source
// SDL's canonical-form hash at generate; commit re-canonicalizes the
// re-loaded source and refuses drift.
// -------------------------------------------------------------------

/// The saved draft carries the `-- source-sha256:` line and parse
/// reads it back; editing the source after the save refuses commit
/// naming both hashes; a draft predating the line commits clean (the
/// legacy posture).
#[test]
fn commit_refuses_a_source_edited_after_generate() {
    let dir = workdir("source-pin");
    let args = info_fixture(&dir, true);
    cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .args(["--save"])
        .assert()
        .success();
    let draft_path = std::path::Path::new(&dir).join("migrations/0001_schema.surql");
    let text = std::fs::read_to_string(&draft_path).expect("the draft reads");
    let parsed = surform::migrate::parse_draft(&text).expect("the draft parses");
    let recorded = parsed
        .header
        .source_sha256
        .as_ref()
        .expect("the source pin rides at generate");
    assert_eq!(recorded.len(), 64, "a full sha256 hex");

    // The source drifts (a field the database never saw), the slots
    // are filled, commit refuses — both hashes named.
    let sdl = std::path::Path::new(&dir).join("schema.surdl");
    let drifted = std::fs::read_to_string(&sdl).expect("reads").replace(
        "  schemaless;\n};",
        "  schemaless;\n  field email: string;\n};",
    );
    std::fs::write(&sdl, drifted).expect("the drift writes");
    let filled = text.replace(&legacy_slot_region(), "");
    std::fs::write(&draft_path, &filled).expect("the fill writes");
    let out = cli()
        .current_dir(&dir)
        .args(["migrate", "commit"])
        .arg(&draft_path)
        .args(["-m", "drifted source"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("source SDL changed"), "{stderr}");
    assert!(stderr.contains(recorded), "the recorded hash: {stderr}");
    assert!(
        stderr.contains("canonicalizes to sha256:"),
        "the current hash: {stderr}"
    );

    // The legacy posture: a draft predating the line commits clean.
    let legacy = filled
        .lines()
        .filter(|line| !line.starts_with("-- source-sha256: "))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&draft_path, format!("{legacy}\n")).expect("the legacy writes");
    let reparsed =
        surform::migrate::parse_draft(&std::fs::read_to_string(&draft_path).expect("reads"))
            .expect("the legacy draft parses");
    assert!(reparsed.header.source_sha256.is_none(), "no line, no pin");
    cli()
        .current_dir(&dir)
        .args(["migrate", "commit"])
        .arg(&draft_path)
        .args(["-m", "legacy posture"])
        .assert()
        .success();
}

/// The missing-source refusal names the mechanism (issue #69): the
/// draft's '-- source:' line, the ledger-sdl purpose, and both ways
/// out — never a bare file error.
#[test]
fn commit_from_elsewhere_names_the_source_mechanism() {
    let dir = workdir("source-elsewhere");
    let args = info_fixture(&dir, true);
    cli()
        .current_dir(&dir)
        .args(["migrate", "generate"])
        .args(&args)
        .args(["--save"])
        .assert()
        .success();
    let draft_path = std::path::Path::new(&dir).join("migrations/0001_schema.surql");
    let text = std::fs::read_to_string(&draft_path).expect("reads");
    let filled = text.replace(&legacy_slot_region(), "");
    std::fs::write(&draft_path, &filled).expect("the fill writes");
    // The recorded source vanishes (a deterministic elsewhere: the
    // relative path cannot resolve).
    std::fs::rename(
        std::path::Path::new(&dir).join("schema.surdl"),
        std::path::Path::new(&dir).join("schema-moved.surdl"),
    )
    .expect("the source move");
    let out = cli()
        .current_dir(&dir)
        .args(["migrate", "commit"])
        .arg(&draft_path)
        .args(["-m", "elsewhere"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("-- source:"), "{stderr}");
    assert!(stderr.contains("revert substrate"), "{stderr}");
    assert!(stderr.contains("absolute path"), "{stderr}");
}

/// Strips every SURFORM SLOT region (the opening marker through the
/// closing marker, the guidance prose inside) — the user's fill, for
/// fixtures whose slots need no real data migration.
fn strip_slot_regions(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_region = false;
    for line in text.lines() {
        if line.starts_with("<<<<<<< SURFORM SLOT") {
            in_region = true;
            continue;
        }
        if line.starts_with(">>>>>>> SURFORM SLOT END") {
            in_region = false;
            continue;
        }
        if !in_region {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The draft group's commit lifecycle (issue #87): committing past an
/// unfilled earlier member refuses; in order, the earlier member's
/// commit writes NO ledger region (its body stays bare) while the
/// LAST member's splices the block — the group records once, under
/// the one number, from the last member's filled script.
#[test]
fn a_group_commits_in_letter_order_and_records_from_the_last() {
    let dir = workdir("group-commit");
    let from = std::path::Path::new(&dir).join("v1.surdl");
    let to = std::path::Path::new(&dir).join("v2.surdl");
    std::fs::write(
        &from,
        "namespace test;\ndatabase scratch;\ntable a;\ndatabase other;\ntable b;\n",
    )
    .expect("v1 writes");
    std::fs::write(
        &to,
        "namespace test;\ndatabase scratch;\ntable a2;\ndatabase other;\ntable b2;\n",
    )
    .expect("v2 writes");
    cli()
        .current_dir(&dir)
        .args(["migrate", "generate", "--from"])
        .arg(&from)
        .args(["--to"])
        .arg(&to)
        .args(["--save", "--name", "shop"])
        .assert()
        .success();
    let member_path = |letter: char| {
        std::path::Path::new(&dir)
            .join("migrations")
            .join(format!("0001{letter}_shop.surql"))
    };
    for letter in ['a', 'b'] {
        let path = member_path(letter);
        let text = std::fs::read_to_string(&path).expect("the member reads");
        std::fs::write(&path, strip_slot_regions(&text)).expect("the fill writes");
    }
    // The letter-swap guard, on b's still-uncommitted text under a
    // name carrying the WRONG letter: the file name's letter and the
    // header's `-- group:` line must agree.
    let b_draft = std::fs::read_to_string(member_path('b')).expect("the member b draft reads");
    let swapped = std::path::Path::new(&dir).join("migrations/0001a_swapped.surql");
    std::fs::write(&swapped, &b_draft).expect("the swap writes");
    let out = cli()
        .current_dir(&dir)
        .args([
            "migrate",
            "commit",
            "migrations/0001a_swapped.surql",
            "-m",
            "swap",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("carries letter a"), "{stderr}");
    assert!(stderr.contains("says b"), "{stderr}");
    assert!(stderr.contains("push walks the NAME order"), "{stderr}");
    // The swap twin leaves — it would be the order scan's earlier
    // member below.
    std::fs::remove_file(&swapped).expect("the swap twin removes");
    // A non-recording member whose frame lost its close refuses too
    // (the NoCommit ground lives in the splice arm the earlier
    // members skip): an unterminated transaction would otherwise die
    // at apply time, past push's pre-flight.
    let a_path = member_path('a');
    let a_text = std::fs::read_to_string(&a_path).expect("the member reads");
    std::fs::write(&a_path, a_text.replace("COMMIT;\n", "")).expect("the truncation writes");
    let out = cli()
        .current_dir(&dir)
        .args([
            "migrate",
            "commit",
            "migrations/0001a_shop.surql",
            "-m",
            "truncated",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(
        stderr.contains("final `COMMIT;` line is missing"),
        "{stderr}"
    );
    std::fs::write(&a_path, &a_text).expect("the member restores");
    // Out of order: member b refuses while a is an uncommitted draft.
    let out = cli()
        .current_dir(&dir)
        .args([
            "migrate",
            "commit",
            "migrations/0001b_shop.surql",
            "-m",
            "out of order",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("commit in letter order"), "{stderr}");
    assert!(stderr.contains("0001a_shop.surql"), "{stderr}");
    assert!(stderr.contains("uncommitted draft"), "{stderr}");
    // In order: member a commits — no ledger region lands.
    let out = cli()
        .current_dir(&dir)
        .args([
            "migrate",
            "commit",
            "migrations/0001a_shop.surql",
            "-m",
            "head of the group",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout);
    assert!(stdout.contains("group member a/2"), "{stdout}");
    assert!(stdout.contains("records once"), "{stdout}");
    let a_text = std::fs::read_to_string(member_path('a')).expect("reads");
    assert!(
        !a_text.contains("surform: migration ledger"),
        "no ledger region rides an earlier member:\n{a_text}"
    );
    let a_parsed = surform::migrate::parse_draft(&a_text).expect("the committed member parses");
    assert!(a_parsed.header.committed.is_some());
    assert!(a_parsed.header.group.is_some());
    // The last member records the group.
    let out = cli()
        .current_dir(&dir)
        .args([
            "migrate",
            "commit",
            "migrations/0001b_shop.surql",
            "-m",
            "tail of the group",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout);
    assert!(
        stdout.contains("ledger: _surform_migrations (number 0001)"),
        "{stdout}"
    );
    let b_text = std::fs::read_to_string(member_path('b')).expect("reads");
    assert!(
        b_text.contains("surform: migration ledger"),
        "the last member carries the block:\n{b_text}"
    );
    let b_parsed = surform::migrate::parse_draft(&b_text).expect("the committed member parses");
    let committed = b_parsed.header.committed.as_ref().expect("committed");
    // The recorded hash is the group's: the LAST member's filled
    // script, re-derivable from the committed file (the fixed point).
    assert_eq!(
        committed.hash,
        surform::migrate::sha256_hex(surform::migrate::filled_script(&b_parsed.body).as_bytes())
    );
}
