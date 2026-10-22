//! The status vectors: the guest's report is the CLI's
//! `surform migrate status` render byte-for-byte, and the
//! classification is the four-state machinery (applied / pending /
//! draft / DRIFTED per file, missing per ledger row). The carriers
//! are synthetic (no filesystem in-guest): the `files` JSON array and
//! the real /sql response envelope of the ledger SELECT. The live
//! dispatch (module vs the actual command, byte-diffed on the same
//! database) ran in the probe.

use surform_surrealism::status_report;

const HASH_A: &str = "aaaaaaaa1234aaaaaaaa1234aaaaaaaa1234aaaaaaaa1234aaaaaaaa1234aaa1";
const HASH_B: &str = "bbbbbbbb5678bbbbbbbb5678bbbbbbbb5678bbbbbbbb5678bbbbbbbb5678bbb2";
const HASH_C: &str = "cccccccc90abcccccccc90abcccccccc90abcccccccc90abcccccccc90abccc3";

/// A realistic /sql envelope for the ledger SELECT (the shape the
/// engine answers with — one element per statement).
fn ledger_ok(rows: &str) -> String {
    format!("[{{\"result\":{rows},\"status\":\"OK\",\"time\":\"153.417µs\",\"type\":null}}]")
}

fn row(number: u32, name: &str, hash: &str) -> String {
    format!("{{\"hash\":\"{hash}\",\"name\":\"{name}\",\"number\":{number}}}")
}

fn file_entry(number: u32, name: &str, hash: Option<&str>) -> String {
    match hash {
        Some(hash) => format!(
            "{{\"committed\":true,\"hash\":\"{hash}\",\"name\":\"{name}\",\"number\":{number}}}"
        ),
        None => format!("{{\"committed\":false,\"name\":\"{name}\",\"number\":{number}}}"),
    }
}

/// The full mix: every state in one report — applied, DRIFTED (both
/// hashes named, short-hash width), draft, pending, and ledger rows
/// with no local file. The report bytes are the CLI's own render.
#[test]
fn the_report_is_the_cli_render_across_all_states() {
    let files = format!(
        "[{},{},{},{}]",
        file_entry(1, "init", Some(HASH_A)),
        file_entry(2, "rename", Some(HASH_A)),
        file_entry(3, "shape", None),
        file_entry(4, "gone", Some(HASH_C)),
    );
    let widened = format!(
        "{{\"hash\":\"{HASH_A}\",\"name\":\"widened\",\"number\":6,\"sdl\":\"DEFINE TABLE x;\"}}"
    );
    let ledger = ledger_ok(&format!(
        "[{},{},{},{},{}]",
        row(1, "init", HASH_A),
        row(2, "rename", HASH_B),
        row(3, "shape", HASH_C),
        row(5, "lost", HASH_B),
        widened,
    ));
    // 0004_gone is pending (no ledger row); 0005_lost and 0006_widened
    // have no local file — TWO missing rows. A projected extra column
    // (`sdl`) rides untouched.
    let report = status_report(&files, &ledger).expect("the carriers are well-formed");
    let expected = concat!(
        "== status ==\n",
        "  0001_init                            applied\n",
        "  0002_rename                          DRIFTED — file sha256:aaaaaaaa1234…, ledger sha256:bbbbbbbb5678…\n",
        "  0003_shape                           draft (commit it first)\n",
        "  0004_gone                            pending\n",
        "  0005_lost                            in the ledger, no local file\n",
        "  0006_widened                         in the ledger, no local file\n",
        "\n",
        "4 migration file(s): 1 applied, 1 pending, 1 draft, 1 drifted; 2 ledger row(s) without a local file\n",
    );
    assert_eq!(report, expected);
}

/// The empty directory AND the empty (fresh-database) ledger: the
/// CLI's no-machinery line, minus the directory name the module's
/// carriers never carry.
#[test]
fn the_empty_state_reports_no_machinery() {
    let report = status_report("[]", &ledger_ok("[]")).expect("empty carriers are well-formed");
    assert_eq!(
        report,
        "== status ==\n\nno migration files and the ledger holds no records\n",
    );
}

/// The fresh-database branch is the NARROWED one (kind NotFound +
/// details.kind Table): the ERR element rides as an empty ledger, so
/// pending files still classify.
#[test]
fn a_missing_ledger_table_is_the_empty_ledger() {
    let files = format!("[{}]", file_entry(1, "init", Some(HASH_A)));
    let missing_table = "[{\"result\":\"The table '_surform_migrations' does not exist\",\"status\":\"ERR\",\"time\":\"1ms\",\"type\":null,\"kind\":\"NotFound\",\"details\":{\"kind\":\"Table\"}}]";
    let report = status_report(&files, missing_table).expect("the fresh database is not an error");
    let expected = concat!(
        "== status ==\n",
        "  0001_init                            pending\n",
        "\n",
        "1 migration file(s): 1 pending\n",
    );
    assert_eq!(report, expected);
}

/// Any OTHER failure subject or kind surfaces with the engine's
/// message — a mistyped capture (namespace not found) and a
/// permission failure are never silently "nothing applied".
#[test]
fn other_ledger_failures_surface_the_engine_message() {
    let files = format!("[{}]", file_entry(1, "init", Some(HASH_A)));
    let legs = [
        (
            "[{\"result\":\"The namespace 'nope' does not exist\",\"status\":\"ERR\",\"time\":\"1ms\",\"type\":null,\"kind\":\"NotFound\",\"details\":{\"kind\":\"Namespace\"}}]",
            "kind NotFound",
            "The namespace 'nope' does not exist",
        ),
        (
            "[{\"result\":\"IAM not allowed\",\"status\":\"ERR\",\"time\":\"1ms\",\"type\":null,\"kind\":\"NotAllowed\",\"details\":{\"kind\":\"Table\"}}]",
            "kind NotAllowed",
            "IAM not allowed",
        ),
    ];
    for (envelope, needle_kind, needle_message) in legs {
        let err = status_report(&files, envelope)
            .expect_err("a failed ledger SELECT is not an empty ledger");
        assert!(err.starts_with("the ledger SELECT failed "), "{err}");
        assert!(err.contains(needle_kind), "{err}");
        assert!(err.contains(needle_message), "{err}");
    }
}

/// Ambiguity: duplicate numbers refuse on EITHER side — the draft
/// numbering and the ledger numbering are unique per database.
#[test]
fn duplicate_numbers_refuse_on_either_side() {
    let files = format!(
        "[{},{}]",
        file_entry(1, "init", Some(HASH_A)),
        file_entry(1, "again", Some(HASH_B)),
    );
    let err = status_report(&files, &ledger_ok("[]")).expect_err("duplicate file numbers");
    assert!(err.contains("carries number 0001 twice (init)"), "{err}");
    let files = format!("[{}]", file_entry(1, "init", Some(HASH_A)));
    let ledger = ledger_ok(&format!(
        "[{},{}]",
        row(1, "one", HASH_A),
        row(1, "two", HASH_B),
    ));
    let err = status_report(&files, &ledger).expect_err("duplicate ledger numbers");
    assert!(err.contains("carries number 0001 twice (one)"), "{err}");
}

/// Carrier shape errors: malformed JSON, wrong containers, missing
/// or ill-typed fields — every refusal names the carrier and the
/// defect, never a panic.
#[test]
fn carrier_shape_errors_name_the_defect() {
    let files = format!("[{}]", file_entry(1, "init", Some(HASH_A)));
    let empty_ledger = ledger_ok("[]");
    let draft_with_hash =
        format!("[{{\"committed\":false,\"hash\":\"{HASH_A}\",\"name\":\"init\",\"number\":1}}]");
    let legs: [(String, &str, &str); 11] = [
        (String::from("{not json"), "files", "malformed"),
        (
            String::from("{\"number\": 1}"),
            "files",
            "expected a JSON array",
        ),
        (
            String::from("[{\"number\": 1, \"name\": \"init\"}]"),
            "files",
            "no `committed` boolean",
        ),
        (
            String::from("[{\"number\": 1, \"name\": \"init\", \"committed\": true}]"),
            "files",
            "no `hash` string",
        ),
        (draft_with_hash, "files", "a draft is not history"),
        (
            String::from("[{\"name\": \"init\", \"committed\": true, \"hash\": \"x\"}]"),
            "files",
            "no `number` integer in the draft range",
        ),
        (
            // The CLI's draft numbering is 1..=9999 (src/migrate/revert.rs):
            // 0 is outside it, and the module pins the same rule.
            String::from(
                "[{\"number\": 0, \"name\": \"init\", \"committed\": true, \"hash\": \"x\"}]",
            ),
            "files",
            "no `number` integer in the draft range",
        ),
        (
            String::from(
                "[{\"number\": 10000, \"name\": \"init\", \"committed\": true, \"hash\": \"x\"}]",
            ),
            "files",
            "no `number` integer in the draft range",
        ),
        (String::from("not json at all"), "ledger_rows", "malformed"),
        (
            String::from("[]"),
            "ledger_rows",
            "the response array is empty",
        ),
        (
            ledger_ok("[{\"name\": \"init\", \"number\": 1}]"),
            "ledger_rows",
            "a ledger row carries no `hash` string",
        ),
    ];
    for (carrier, side, needle) in legs {
        let error = match side {
            "files" => {
                status_report(&carrier, &empty_ledger).expect_err("the files carrier refuses")
            }
            _ => status_report(&files, &carrier).expect_err("the ledger carrier refuses"),
        };
        assert!(
            error.contains("carrier is malformed") || error.contains("carries"),
            "{side}: {error}"
        );
        assert!(error.contains(needle), "{side}: {error}");
    }
}

/// The ledger result not being an array and the element carrying no
/// `status` — the envelope's own two degenerate shapes.
#[test]
fn envelope_shape_errors_refuse() {
    let files = format!("[{}]", file_entry(1, "init", Some(HASH_A)));
    let not_array =
        "[{\"result\":{\"number\":1},\"status\":\"OK\",\"time\":\"1ms\",\"type\":null}]";
    let error = status_report(&files, not_array).expect_err("a non-array result");
    assert!(error.contains("result is not an array"), "{error}");
    let no_status = "[{\"result\":[]}]";
    let error = status_report(&files, no_status).expect_err("no status string");
    assert!(error.contains("carries no `status` string"), "{error}");
}
