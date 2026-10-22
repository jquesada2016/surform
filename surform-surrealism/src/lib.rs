//! `surform-surrealism` — Surform as a Surrealism module: the
//! compiler running INSIDE SurrealDB.
//!
//! The guest is a thin wrapper over the core. Three exports, all PURE
//! by design — `surrealism.toml` declares zero capabilities and the
//! surface makes no host calls, so every output is reproducible from
//! its inputs alone:
//!
//! - [`compile_sdl`] — SDL text → the same SurrealQL DDL the
//!   `surform build` CLI prints (byte-identical; an early spike proved
//!   the pipeline in-guest).
//! - [`converge_sdl`] — SDL text + the live database's rendered INFO
//!   → the same convergence script `surform migrate generate --info`
//!   prints (byte-identity pinned against CLI goldens).
//! - [`status_report`] — the serialized migrations-dir state + the
//!   ledger SELECT's /sql response → the same report `surform
//!   migrate status` prints (the four-state classification).
//!
//! The `apply` cargo feature carries the second,
//! capability-declaring artifact's machinery; the
//! `surform-surrealism-apply` package turns it on.
//!
//! This is a DIFFERENT ABI from the editor crate: Surrealism is a
//! WIT component (`wasm32-wasip2`, the `#[surrealism]` proc macro),
//! not wasm-bindgen — the two wasm surfaces share no code paths (the
//! editor wraps the same core, independently).
//!
//! Build recipe (README.md in this crate): `surreal module build
//! --out surform.surli .` — the `.surli` registers with
//! `DEFINE MODULE mod::surform FROM f"modules:/surform.surli"
//! UNSIGNED PERMISSIONS FULL` and dispatches as
//! `mod::surform::compile(...)`.

use std::collections::BTreeMap;
use std::path::Path;

use surform::{
    ast::SdlFile,
    codegen,
    info::diff::diff,
    info::normalize::{ReadModel, ScopeTag, TABLE_CHILDREN_KEY, declared_scopes},
    info::read::{InfoObject, InfoValue, read_info_object},
    info::script::{ConvergenceScript, convergence_script_parts},
    load,
    migrate::{
        DEFAULT_LEDGER_TABLE,
        json::{self, Json},
    },
};

/// The display name diagnostics cite: the caller hands the module a
/// STRING, not a file, so the name is a stable placeholder in the
/// same `.surdl` spelling the toolchain uses everywhere else.
const INPUT_NAME: &str = "schema.surdl";

/// The display name the live-INFO diagnostics cite — the caller's
/// captured interchange string, named after the file a `--info`
/// caller would have captured it into (the CLI reports its INFO
/// diagnostics against that file; the module's placeholder keeps the
/// render's shape stable).
const LIVE_INFO_NAME: &str = "live-info.txt";

/// SDL text → SurrealQL DDL: the module's `compile` export and the
/// one function its vectors test against. The pipeline is exactly
/// the CLI's `build` — [`load::load`] (the include-composition
/// loader) then [`codegen::emit`] — so the output is byte-identical
/// to `surform build` on the same self-contained document.
///
/// `include`-bearing documents fail closed: the Surrealism runtime
/// grants the guest no filesystem (probe-verified on SurrealDB 3.3 —
/// no WASI preopens), so the loader's include reads fail and every
/// failure renders as the loader's own diagnostics. The error string
/// is what SurrealQL callers see verbatim after the host's
/// `Function call error: ` prefix (proven in the spike).
pub fn compile_sdl(sdl: &str) -> Result<String, String> {
    match load::load(Path::new(INPUT_NAME), INPUT_NAME, sdl.to_owned()) {
        Ok(loaded) => Ok(codegen::emit(&loaded.file)),
        Err(failure) => Err(render_rejection(&failure)),
    }
}

/// SDL text + the live database's rendered INFO → the convergence
/// script: the module's `converge` export. The comparison is
/// the CLI's exact `migrate generate --info` machinery — the live side
/// ingested through [`read_info_object`] + [`ReadModel::push_scope`]
/// (the interchange's own readers), the SDL side the composition
/// loader's file, one [`diff`] pass, the plain
/// [`convergence_script`]. Empty output means the database and the
/// SDL already agree (the CLI prints `(converged)` for the same
/// input).
///
/// The `live_info` carrier is the RENDERED INFO — the interchange
/// object literal the engine produces for the caller with one
/// statement, `RETURN <string>(INFO FOR DB);` (probed on 3.3.0: the
/// /sql route has no text rendering — `Accept: text/plain` answers
/// 415 — and the `<string>` cast over the INFO value is the byte-exact
/// interchange text the read path consumes). A bare
/// `INFO FOR DB` capture compares tables at header level; the
/// full-fidelity capture assembles each table's rendered
/// `INFO FOR TABLE` under the reserved `"@tables"` key, exactly the
/// interchange file a `--info` caller writes (include the children
/// when field-level drift matters — removals are the point).
///
/// The scope is the ONE database the SDL declares (the
/// namespace-free pseudo-scope when the file declares no namespace) —
/// the module has no `--ns`/`--db` flags, the declaration IS the
/// scope; zero or several databases refuse. The capture must come
/// from the database the SDL declares: nothing in the interchange
/// names it, so the pairing is the caller's to get right (CLI-parity
/// — the CLI's `--info db=` tag asserts the pairing the same way).
/// Content in the SDL's
/// other scopes (root, namespace) makes no claim, exactly as an
/// untagged scope in the CLI's `--info` mode. The default ledger
/// table ([`DEFAULT_LEDGER_TABLE`]) is dropped from the capture
/// before ingestion (the revert precedent): the pipeline's own
/// machinery must never be drafted for removal by the comparison
/// that follows it.
///
/// Errors: broken SDL rejects under the [`compile_sdl`] contract
/// verbatim; a malformed capture, a shape the readers diagnose, or a
/// scope the compare never ingested rejects as
/// `live INFO rejected: N diagnostics` + the CLI-theme render; scope
/// resolution failures are plain messages.
pub fn converge_sdl(sdl: &str, live_info: &str) -> Result<String, String> {
    let (file, scope, mut info) = converge_prepare(sdl, live_info)?;
    converge_model(&file, scope, &mut info, live_info).map(|parts| parts.text)
}

/// The compare's setup, shared by [`converge_sdl`] and the `apply`
/// flow's structured twin ([`apply::converge_parts`]): the SDL is
/// loaded (the [`compile_sdl`] rejection contract verbatim), its ONE
/// declared database scope resolved, and the rendered-INFO capture
/// ingested (malformed text and shapes the readers reject surface as
/// `live INFO rejected: N diagnostics` — the interchange reader's
/// walk first, the per-entry DDL readers during ingestion).
fn converge_prepare(sdl: &str, live_info: &str) -> Result<(SdlFile, ScopeTag, InfoObject), String> {
    let loaded = load::load(Path::new(INPUT_NAME), INPUT_NAME, sdl.to_owned())
        .map_err(|failure| render_rejection(&failure))?;
    let (ns, db) = database_scope(&loaded.file)?;
    let info = read_info_object(live_info).map_err(|diag| {
        render_info_rejection("live INFO rejected", std::slice::from_ref(&diag), live_info)
    })?;
    Ok((loaded.file, ScopeTag::Database(ns, db), info))
}

/// The compare's scope: the ONE database the SDL declares — the
/// `(namespace, database)` pair, the namespace empty for the
/// namespace-free pseudo-scope (a versions-mode file's common shape —
/// the revert decision). Shared verbatim by [`converge_sdl`] and
/// the `apply` flow.
fn database_scope(file: &SdlFile) -> Result<(String, String), String> {
    let (_, databases) = declared_scopes(file);
    match databases.len() {
        1 => {
            let (ns, db) = databases.iter().next().expect("exactly one declared");
            Ok((ns.clone(), db.clone()))
        }
        0 => Err(
            "the SDL never declares a database — converge compares the live \
             database's rendered INFO against the SDL's database scope, and \
             there is none to name. Author `database <name>;` (under its \
             `namespace`, if any) and re-run"
                .to_string(),
        ),
        _ => {
            let named = databases
                .iter()
                .map(|(ns, db)| {
                    if ns.is_empty() {
                        db.clone()
                    } else {
                        format!("{ns}/{db}")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "the SDL declares {} databases ({named}) — converge compares one \
                 database per call; hand the module one database's schema at a time",
                databases.len()
            ))
        }
    }
}

/// The comparison tail shared by [`converge_sdl`] and the `apply`
/// flow: the capture is ingested (the default ledger table dropped —
/// see the caller's contract), the diff runs, the plain
/// [`convergence_script_parts`] renders — BOTH carriers: the script
/// text and the generator-marked statements ([`ConvergenceScript`];
/// the apply flow executes one host call per marked statement, so no
/// pass ever re-derives the boundaries from the rendered bytes).
/// Empty text = converged.
fn converge_model(
    file: &SdlFile,
    scope: ScopeTag,
    info: &mut InfoObject,
    diag_text: &str,
) -> Result<ConvergenceScript, String> {
    // The pipeline's own machinery is never converge material (the
    // revert precedent): the ledger table is a real table every
    // migration's self-record creates, so an unfiltered capture would
    // classify it database-only and the script would draft its
    // removal — wiping the history the next apply appends to. Dropped
    // from BOTH the `tables` section and the reserved `@tables` child
    // map (a child without its table is the readers' SDL-E3002 shape
    // error).
    drop_default_ledger(info);
    let mut model = ReadModel::default();
    if let Err(diags) = model.push_scope(scope, info) {
        return Err(render_info_rejection(
            "live INFO rejected",
            &diags,
            diag_text,
        ));
    }
    Ok(convergence_script_parts(&diff(file, &model), file))
}

/// The default ledger table's removal from a capture's `tables`
/// section and reserved `@tables` child map — the drop
/// [`converge_model`] runs before ingestion.
fn drop_default_ledger(info: &mut InfoObject) {
    if let Some(InfoValue::Object(tables)) = info.0.get_mut("tables") {
        tables.0.remove(DEFAULT_LEDGER_TABLE);
    }
    if let Some(InfoValue::Object(children)) = info.0.get_mut(TABLE_CHILDREN_KEY) {
        children.0.remove(DEFAULT_LEDGER_TABLE);
    }
}

/// The migrations-dir state + the ledger SELECT's response → the
/// status report: the module's `status` export. The
/// classification is the CLI's exact `migrate status` machinery —
/// applied / pending / draft / DRIFTED per file, missing per ledger
/// row — and the report is the CLI's own render (the same rows, the
/// same verdicts, the same summary), byte-for-byte what the command
/// prints for the same inputs. The module's contract has no exit
/// code: the report IS the verdict — DRIFTED rows and missing-file
/// rows are the inconsistency classes a CI gate reads off the text.
///
/// The carriers (both JSON, parsed by the core's own RFC 8259 reader
/// — the hand-rolled-reader precedent, no second dependency):
///
/// - `files` — the serialized migrations-dir state, a JSON array of
///   `{"number": 1, "name": "init", "committed": true, "hash":
///   "<64 hex>"}` entries in any order (sorted here). `number` is the
///   draft numbering (1–9999, the CLI's own rule), `name` the
///   migration's name, and
///   `hash` the CLI's committed-file hash — sha256 over the filled
///   script (the file with its ledger region drained), the exact
///   bytes `migrate status` re-derives. A draft entry carries
///   `"committed": false` and no hash: drafts are work in progress,
///   not history.
/// - `ledger_rows` — the /sql response body of
///   `SELECT number, name, hash FROM <ledger>;` (the ledger envelope),
///   pasted verbatim. The module reads the envelope itself: an
///   `ERR` element whose kind is `NotFound` with `details.kind`
///   `Table` is the fresh-database empty ledger; any other failure
///   surfaces with the engine's message (a permission failure is
///   never silently "nothing applied").
///
/// Errors: malformed carriers, wrong shapes, duplicate numbers
/// (either side — the numbering is unique per database), and
/// committed entries without hashes refuse with plain messages; the
/// engine's own ledger failures ride its message.
pub fn status_report(files: &str, ledger_rows: &str) -> Result<String, String> {
    let files = parse_files(files)?;
    let ledger = parse_ledger(ledger_rows)?;
    Ok(render_status(&files, &ledger))
}

/// One serialized migration file: the draft numbering, the name, and
/// the committed-file hash (`None` for a draft).
struct FileEntry {
    number: u32,
    name: String,
    hash: Option<String>,
}

/// One ledger row: the ledger SELECT's projection.
struct LedgerRow {
    name: String,
    hash: String,
}

/// The `files` carrier: a JSON array of migration entries, sorted by
/// number, duplicates refused.
fn parse_files(files: &str) -> Result<Vec<FileEntry>, String> {
    let refuse = |detail: String| format!("the `files` carrier is malformed — {detail}");
    let document = json::parse(files).map_err(|error| refuse(error.to_string()))?;
    let Json::Arr(items) = &document else {
        return Err(refuse(
            "expected a JSON array of migration entries".to_string(),
        ));
    };
    let mut entries: Vec<FileEntry> = Vec::with_capacity(items.len());
    for (at, item) in items.iter().enumerate() {
        let Json::Obj(_) = item else {
            return Err(refuse(format!("entry {at} is not a JSON object")));
        };
        let number = item
            .get("number")
            .and_then(Json::as_integer)
            .filter(|n| (1..=9999).contains(n))
            .ok_or_else(|| {
                refuse(format!(
                    "entry {at} carries no `number` integer in the draft range (1–9999)"
                ))
            })? as u32;
        let name = item
            .get("name")
            .and_then(Json::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| refuse(format!("entry {at} carries no `name` string")))?
            .to_string();
        let committed = item
            .get("committed")
            .and_then(|value| match value {
                Json::Bool(flag) => Some(*flag),
                _ => None,
            })
            .ok_or_else(|| refuse(format!("entry {at} carries no `committed` boolean")))?;
        let hash = match committed {
            true => Some(
                item.get("hash")
                    .and_then(Json::as_str)
                    .filter(|hash| !hash.is_empty())
                    .ok_or_else(|| {
                        refuse(format!(
                            "committed entry {at} ({name}) carries no `hash` string — \
                             the committed-file hash is the comparison's substrate"
                        ))
                    })?
                    .to_string(),
            ),
            // A draft carries no hash: a present one is a caller bug
            // the module refuses rather than ignores (drafts are not
            // history; a hash there claims to be).
            false => {
                if matches!(item.get("hash"), Some(Json::Str(_))) {
                    return Err(refuse(format!(
                        "draft entry {at} ({name}) carries a `hash` — a draft is \
                         not history; commit it first"
                    )));
                }
                None
            }
        };
        entries.push(FileEntry { number, name, hash });
    }
    entries.sort_by_key(|entry| entry.number);
    for window in entries.windows(2) {
        if window[0].number == window[1].number {
            return Err(format!(
                "the `files` carrier carries number {:04} twice ({}) — the draft \
                 numbering is unique per database",
                window[0].number, window[0].name,
            ));
        }
    }
    Ok(entries)
}

/// The `ledger_rows` carrier: the /sql response body of the ledger
/// SELECT — the /sql envelope, its fresh-database branch and row shape
/// mirrored exactly (kind NotFound + details.kind Table = empty
/// ledger; anything else surfaces the engine's message).
fn parse_ledger(ledger_rows: &str) -> Result<BTreeMap<u32, LedgerRow>, String> {
    let document = json::parse(ledger_rows)
        .map_err(|error| format!("the `ledger_rows` carrier is malformed — {error}"))?;
    let Json::Arr(elements) = &document else {
        return Err(
            "the `ledger_rows` carrier must be the /sql response body of the \
             ledger SELECT — a JSON array"
                .to_string(),
        );
    };
    let first = elements.first().ok_or_else(|| {
        "the `ledger_rows` carrier is malformed — the response array is empty".to_string()
    })?;
    let status = first.get("status").and_then(Json::as_str).ok_or_else(|| {
        "the `ledger_rows` carrier is malformed — the response element carries \
             no `status` string"
            .to_string()
    })?;
    if status != "OK" {
        // The narrowed branch: ONLY a not-found TABLE is the
        // fresh-database empty ledger. A missing namespace or
        // database (a mistyped capture), a permission failure, any
        // other kind — a real error, never "nothing applied".
        let kind = element_kind(first);
        let reason = element_reason(first);
        let fresh_database =
            kind.as_deref() == Some("NotFound") && reason.as_deref() == Some("Table");
        if fresh_database {
            return Ok(BTreeMap::new());
        }
        return Err(format!(
            "the ledger SELECT failed (kind {}): {}",
            kind.as_deref().unwrap_or("unknown"),
            element_message(first),
        ));
    }
    let Json::Arr(rows) = first.get("result").unwrap_or(&Json::Null) else {
        return Err(
            "the `ledger_rows` carrier is malformed — the ledger SELECT's result \
             is not an array"
                .to_string(),
        );
    };
    let mut ledger: BTreeMap<u32, LedgerRow> = BTreeMap::new();
    for row in rows {
        let number = row
            .get("number")
            .and_then(Json::as_integer)
            .filter(|n| (0..=u32::MAX as i64).contains(n))
            .ok_or_else(|| {
                "the `ledger_rows` carrier is malformed — a ledger row carries no \
                 `number` integer"
                    .to_string()
            })? as u32;
        let parsed = LedgerRow {
            name: row
                .get("name")
                .and_then(Json::as_str)
                .ok_or_else(|| {
                    "the `ledger_rows` carrier is malformed — a ledger row carries \
                     no `name` string"
                        .to_string()
                })?
                .to_string(),
            hash: row
                .get("hash")
                .and_then(Json::as_str)
                .ok_or_else(|| {
                    "the `ledger_rows` carrier is malformed — a ledger row carries \
                     no `hash` string"
                        .to_string()
                })?
                .to_string(),
        };
        if let Some(previous) = ledger.get(&number) {
            return Err(format!(
                "the `ledger_rows` carrier carries number {number:04} twice ({}) — \
                 the ledger numbering is unique per database",
                previous.name,
            ));
        }
        ledger.insert(number, parsed);
    }
    Ok(ledger)
}

/// The response element's error KIND ("NotFound", "Query", …) — the
/// push client's exact reader (errors.md: "branch on `kind` first").
fn element_kind(element: &Json) -> Option<String> {
    element.get("kind").and_then(Json::as_str).map(String::from)
}

/// The element's reason WITHIN the kind — the nested `details.kind`
/// ("Table" under NotFound; "NotExecuted"/"Cancelled" under Query).
/// A narrowed copy of the push client's reader (src/migrate/push.rs):
/// the CLI's top-level-kind fallback is deliberately DROPPED — inert
/// here (the only use compares against "Table", which the fallback
/// can never return), and dropping it keeps the fresh-database
/// branch strict if this helper ever gains uses.
fn element_reason(element: &Json) -> Option<String> {
    element
        .get("details")
        .and_then(|details| details.get("kind"))
        .and_then(Json::as_str)
        .map(String::from)
}

/// The element's error message — the push client's shape: the
/// `result` string, falling back to the element's Debug (a server
/// outgrowing the contract surfaces its own shape).
fn element_message(element: &Json) -> String {
    match element.get("result").and_then(Json::as_str) {
        Some(message) => message.to_string(),
        None => format!("{element:?}"),
    }
}

/// The classification + report, the CLI `migrate status` render
/// byte-for-byte (minus the directory name the command prints in its
/// empty-state line — the module's carriers name no directory).
fn render_status(files: &[FileEntry], ledger: &BTreeMap<u32, LedgerRow>) -> String {
    let mut report = String::from("== status ==\n");
    let mut applied = 0u32;
    let mut pending = 0u32;
    let mut drafts = 0u32;
    let mut drifted = 0u32;
    let mut local_numbers = Vec::with_capacity(files.len());
    for file in files {
        local_numbers.push(file.number);
        let verdict = match &file.hash {
            // No committed header — work in progress (push's skip
            // verdict, reported not fatal).
            None => {
                drafts += 1;
                "draft (commit it first)".to_string()
            }
            // The comparison's definition: the file's
            // CURRENT hash against the LEDGER's (what is on disk vs
            // what the database recorded is the whole question).
            Some(current) => match ledger.get(&file.number) {
                None => {
                    pending += 1;
                    "pending".to_string()
                }
                Some(row) if row.hash == *current => {
                    applied += 1;
                    "applied".to_string()
                }
                Some(row) => {
                    drifted += 1;
                    format!(
                        "DRIFTED — file sha256:{}, ledger sha256:{}",
                        short_hash(current),
                        short_hash(&row.hash),
                    )
                }
            },
        };
        report.push_str(&format!(
            "  {:<36} {verdict}\n",
            push_key(file.number, &file.name)
        ));
    }
    let mut missing = 0u32;
    for (number, row) in ledger {
        if local_numbers.contains(number) {
            continue;
        }
        missing += 1;
        report.push_str(&format!(
            "  {:<36} in the ledger, no local file\n",
            push_key(*number, &row.name),
        ));
    }
    report.push('\n');
    if files.is_empty() && ledger.is_empty() {
        report.push_str("no migration files and the ledger holds no records\n");
        return report;
    }
    let mut summary = format!("{} migration file(s)", files.len());
    let segments: Vec<String> = [
        ("applied", applied),
        ("pending", pending),
        ("draft", drafts),
        ("drifted", drifted),
    ]
    .into_iter()
    .filter(|(_, count)| *count > 0)
    .map(|(label, count)| format!("{count} {label}"))
    .collect();
    if !segments.is_empty() {
        summary.push_str(&format!(": {}", segments.join(", ")));
    }
    if missing > 0 {
        summary.push_str(&format!("; {missing} ledger row(s) without a local file"));
    }
    report.push_str(&summary);
    report.push('\n');
    report
}

/// The report row's migration key — the CLI's `{number:04}_{name}`.
fn push_key(number: u32, name: &str) -> String {
    format!("{number:04}_{name}")
}

/// A hash's display spelling — the CLI's first-12-characters width,
/// safe on any input length (a malformed hash must never panic the
/// report).
fn short_hash(hash: &str) -> String {
    format!("{}…", hash.chars().take(12).collect::<String>())
}

/// The `apply` feature's machinery — the pure halves here,
/// the host-calling flow and the `apply` export in the wasm32 build
/// only (the SDK's host imports are component bindings; the native
/// rlib the tests run against stays free of them). Everything in this
/// module is mock-side testable: the legs feed it rendered captures
/// and script texts, no database involved.
///
/// The flow this machinery serves — `apply(sdl)`: load the SDL,
/// pre-flight the invoking session's scope against the SDL's one
/// declared database, capture the live schema through the
/// capability-gated `sql` host import (`RETURN <string>(INFO FOR
/// DB);` plus one cast per table, assembled under the reserved
/// `"@tables"` key exactly like a caller-assembled full-fidelity
/// capture), converge — the emitter hands back BOTH carriers: the
/// script text and its GENERATOR-MARKED statements
/// ([`ConvergenceScript`]; no pass ever re-derives a statement
/// boundary from the rendered bytes — the lexical
/// re-splitter was retired) — then the refusal
/// gates, then ONE host call per marked statement, in order. An
/// early probe pinned that execution shape: the host import parses
/// ONE expression per call — the script's own `USE` line and
/// `BEGIN`…`COMMIT` frame are top-level-only spellings it cannot run
/// (the probe: the text executes nothing, result NONE), and a
/// multi-statement string is silently first-expression-only (the tail
/// ignored, no error).
///
/// The gates, all before ANY host call touches the database, in
/// pinned order:
///
/// 1. [`apply::refuse_surform_markers`] — any SURFORM marker line
///    refuses (the markers ride the generator's comment anchors —
///    text, not carrier elements; the scan is line-based and
///    LINE-PREFIX EXACT over the tool's marker spellings, mirroring
///    the CLI `classify_marker_line`'s classification; a substring
///    scan misreads statement content carrying the banner mid-line).
/// 2. [`apply::refuse_removals`] — ANY `REMOVE` statement in the
///    carrier refuses (apply is ADDITIVE-ONLY, literally; the head
///    test is syntactic over
///    generator-guaranteed statements: every element's first token IS
///    its head keyword).
/// 3. [`apply::refuse_raw_bodies`] — any raw-authored body (README
///    rule 8) refuses: its statement boundaries are the AUTHOR's
///    knowledge, which the generator does not mark, so the flow
///    cannot execute it one-call-per-statement — the destructive and
///    raw classes both stay the caller's own session or the CLI's
///    generate → fill → commit → push flow (whose `POST /sql` body
///    the ENGINE's statement parser owns).
///
/// The transaction the script cannot carry arrives at the dispatch
/// boundary instead (live-probed on 3.3.0): the module's host writes
/// ride the INVOKING STATEMENT's transaction — a failed apply (any
/// `Err` the export returns) rolls back everything the call did, the
/// ledger row included, and a successful apply commits whole. The
/// statements still execute in order and the self-record rides last,
/// so the row exists only for fully applied scripts — but the
/// atomicity is the engine's, at apply-call granularity, not the
/// script text's.
#[cfg(feature = "apply")]
pub mod apply {
    use surform::info::script::{ConvergenceScript, ident_text};
    use surform::migrate::{
        DEFAULT_LEDGER_TABLE, SLOT_CLOSE, SLOT_OPEN_PREFIX, ledger_statement_list, sha256_hex,
    };

    /// The ledger row's name for applies — the CLI's name slot is a
    /// migration name; an in-database apply has none, and `apply` says
    /// exactly what wrote the row (rows sort as `NNNN_apply`).
    pub const LEDGER_NAME: &str = "apply";

    /// The ledger row's message — the CLI's `--message` slot, spelled
    /// for the writer that has no flag to carry one.
    pub const LEDGER_MESSAGE: &str = "applied in-database by the surform apply module";

    /// The apply flow's structured twin of [`super::converge_sdl`] —
    /// the exact same pipeline (load, scope, ingest, diff, the plain
    /// emitter), returning the emitter's BOTH carriers
    /// ([`ConvergenceScript`]): the script text (what the operator
    /// would read, what the ledger hash covers) and its
    /// generator-marked statements (what the flow executes, one host
    /// call per element). THE STRUCTURAL FIX under the whole apply
    /// path: the statement boundaries are captured AT EMISSION — the
    /// codegen walk knows where each statement starts and ends — so
    /// the flow never re-derives them lexically — the emitter cannot
    /// merge, because it never looks.
    pub fn converge_parts(sdl: &str, live_info: &str) -> Result<ConvergenceScript, String> {
        let (file, scope, mut info) = super::converge_prepare(sdl, live_info)?;
        super::converge_model(&file, scope, &mut info, live_info)
    }

    /// The raw-body gate: any raw-authored statement (README rule 8)
    /// in the convergence refuses. A raw body is the SDL's declared
    /// escape hatch — SurrealQL captured VERBATIM — so its statement
    /// boundaries are the author's knowledge, which the generator
    /// does not mark: the body rides the carrier's `raw_bodies`
    /// (never `statements`), and executing it one-call-per-marked-
    /// statement is impossible without re-deriving those boundaries
    /// lexically (the retired splitter's whole failure surface — a
    /// regex, string, or `;` misread inside the body merges or
    /// splits it wrongly). The refusal is loud and names the bodies;
    /// the escape is the caller's own session or the CLI flow —
    /// `surform migrate push` sends the whole committed file as ONE
    /// `POST /sql` body whose statement parser is the ENGINE's own
    /// (the boundary's true owner), so raw shapes ride that path
    /// faithfully.
    pub fn refuse_raw_bodies(raw_bodies: &[String]) -> Result<(), String> {
        if raw_bodies.is_empty() {
            return Ok(());
        }
        let named = raw_bodies
            .iter()
            .map(|body| format!("\n  raw {{ {body} }};"))
            .collect::<String>();
        Err(format!(
            "the convergence script carries {} raw-authored statement(s) — their \
             boundaries are the author's, not the generator's, so apply cannot \
             execute them one host call per statement. Run them in your own \
             session, or take the script through the CLI flow (surform migrate \
             generate → commit → push), whose /sql body the engine's own parser \
             splits\n\
             raw statement(s):{named}",
            raw_bodies.len(),
        ))
    }

    /// The generator's comment-banner prefix — every CommentBanners
    /// anchor the script.rs slot emitters push starts with it (the
    /// three spellings: `-- >>> SURFORM REQUIRED: DATA MIGRATION <<<`,
    /// `-- >>> SURFORM REQUIRED: END <<<`,
    /// `-- >>> SURFORM WARNING: DATA LOSS <<<`, all pinned against the
    /// golden). Mirrored with a cross-reference — the spellings' home
    /// is `src/info/script.rs` (`table_slot`/`slot_open`/`slot_close`),
    /// which this guest cannot import as constants; the parity test
    /// pins the three lines.
    const BANNER_PREFIX: &str = "-- >>> SURFORM ";

    /// One line's marker classification — LINE-PREFIX EXACT over the
    /// tool's marker spellings, the CLI `classify_marker_line`'s
    /// (src/migrate.rs) semantics mirrored: a line STARTING with the
    /// slot opener, EQUAL to the slot closer, or STARTING with the
    /// banner prefix is a marker line. The slot pair rides the CLI's
    /// own pub constants (referenced across the crate boundary — one
    /// home); the banner prefix is mirrored above. NOT the CLI's
    /// git-stray classes (any `<<<<<<<`/`>>>>>>>`-leading line): see
    /// [`refuse_surform_markers`]'s doc for the deliberate delta.
    fn is_surform_marker_line(line: &str) -> bool {
        line.starts_with(BANNER_PREFIX) || line.starts_with(SLOT_OPEN_PREFIX) || line == SLOT_CLOSE
    }
    /// The data-loss gate: a convergence script carrying SURFORM slot
    /// markers refuses. The markers ride every MARKED removal the
    /// generator drafts (the REQUIRED data-migration fill-in, the
    /// WARNING data-loss banner — tables, fields, scopes) — the
    /// fill-ins exist because a human must move the data before the
    /// removal runs, and the module cannot pause for a fill-in its
    /// caller never sees (the caller hands the module SDL, not the
    /// script). This gate alone does NOT make apply additive-only:
    /// every other family drafts its REMOVE bare, which is what
    /// [`refuse_removals`] refuses (the two
    /// gates together are the additive-only contract). The scan is
    /// deliberately line-based on the SCRIPT TEXT — the markers are
    /// the generator's COMMENT anchors, emitted as their own lines
    /// beside the statement, never carrier elements, so no scan over
    /// [`ConvergenceScript::statements`] could ever see them — and a
    /// marker line is classified PREFIX-EXACT, mirroring the CLI's
    /// `classify_marker_line` (src/migrate.rs): the line must START
    /// with a tool marker spelling — the generator's comment-banner
    /// prefix ([`BANNER_PREFIX`], the CommentBanners spellings the
    /// script.rs slot emitters push) or the slot pair (the CLI's own
    /// pub constants, referenced — one home). A substring scan would
    /// misread content: `contains(">>> SURFORM ")` refused a purely
    /// additive schema whose comment VALUE carried the banner
    /// mid-line. Content cannot false-positive the prefix scan: the
    /// printer escapes newlines inside strings and idents
    /// (surrealdb-types 3.3.0 `utils/escape.rs` `EscapeWriter`,
    /// SingleLine), so a value never continues onto its own line, and
    /// a SurrealQL statement never begins `-- ` or `<<<<<<<` — a line
    /// STARTING with a marker spelling can only be a generator anchor
    /// (or the slot pair, which this flow never emits; pinned
    /// anyway). The CLI's git-stray classes — any `<<<<<<<`/
    /// `>>>>>>>`-leading line — are deliberately NOT carried: those
    /// guard hand-carried draft files, while this script is generator
    /// output, so the shapes are unrepresentable here (and a raw body
    /// carrying one refuses at [`refuse_raw_bodies`] regardless).
    pub fn refuse_surform_markers(script: &str) -> Result<(), String> {
        let marked: Vec<&str> = script
            .lines()
            .filter(|line| is_surform_marker_line(line))
            .collect();
        if marked.is_empty() {
            return Ok(());
        }
        Err(format!(
            "the convergence script carries {} SURFORM marker line(s) — a \
             data-migration fill-in and/or data-loss warning the module cannot \
             pause for. Apply converges only what the pipeline auto-applies; \
             resolve the destructive drift through the CLI flow (surform migrate \
             generate → fill the slots → commit → push), or apply your own \
             removal first and re-run\n\
             first marker: {}",
            marked.len(),
            marked[0].trim(),
        ))
    }

    /// The additive-only gate: ANY `REMOVE` statement in the
    /// generator-marked statements refuses — apply never removes,
    /// which is what makes the "additive-only" claim literally
    /// true. The marker gate covers only the MARKED
    /// data-loss class (tables, fields, scopes — the generator's slot
    /// banners); every other family drafts its removal bare
    /// (`remove_line`: analyzers, sequences, events, indexes, params,
    /// functions, apis, users, accesses, configs — and a bucket the
    /// scaffolding drop somehow missed), so without this gate a
    /// converge of a database carrying, say, a db user the SDL never
    /// declares would draft `REMOVE USER IF EXISTS … ON DATABASE;`
    /// unmarked and EXECUTE it. The test is now
    /// SYNTACTIC over [`ConvergenceScript::statements`] (run after
    /// [`converge_parts`], before any host call): the boundaries are
    /// the emitter's own, so a statement's first token IS its head
    /// keyword — the whole-word, case-folded head test needs no depth
    /// reasoning, and a `REMOVE` inside an expression or string can
    /// never be a head because a statement is never an expression's
    /// text. The refusal names every offending statement; the escape
    /// is the caller's own session (run the removals yourself, then
    /// re-apply — the next convergence drafts only what still
    /// differs) or the CLI's destructive flow (generate → fill →
    /// commit → push).
    pub fn refuse_removals(statements: &[String]) -> Result<(), String> {
        let removals: Vec<&str> = statements
            .iter()
            .map(String::as_str)
            .filter(|statement| {
                statement
                    .split_ascii_whitespace()
                    .next()
                    .is_some_and(|head| head.trim_end_matches(';').eq_ignore_ascii_case("REMOVE"))
            })
            .collect();
        if removals.is_empty() {
            return Ok(());
        }
        let named = removals
            .iter()
            .map(|statement| format!("\n  {statement}"))
            .collect::<String>();
        Err(format!(
            "the convergence script carries {} REMOVE statement(s) — apply is \
             additive-only: it never removes. Run the removal(s) yourself in \
             this session and re-apply (the next convergence drafts only what \
             still differs), or take the destructive class through the CLI \
             flow (surform migrate generate → fill the slots → commit → \
             push)\n\
             removal(s):{named}",
            removals.len(),
        ))
    }

    /// The USE guard's module form: apply converges the INVOKING
    /// SESSION's database — the module's host calls run at the
    /// invoking session's scope and it can neither read nor switch it
    /// otherwise (the `USE` spelling is one of the lines the host
    /// import cannot execute). The SDL's declared database must be the
    /// session's, or apply refuses before anything is captured or run.
    /// The namespace-free pseudo-scope matches any namespace (the
    /// SDL's compare scope is the database itself); the database name
    /// must always match.
    pub fn session_scope_matches(
        session_ns: Option<&str>,
        session_db: Option<&str>,
        sdl_ns: &str,
        sdl_db: &str,
    ) -> Result<(), String> {
        let (Some(ns), Some(db)) = (session_ns, session_db) else {
            return Err(
                "apply runs in the invoking session's namespace and database — \
                 the session names none. Scope the session (its ns/db headers, \
                 the RPC USE, or `USE NS … DB …`) and re-run"
                    .to_string(),
            );
        };
        if (sdl_ns.is_empty() || sdl_ns == ns) && sdl_db == db {
            return Ok(());
        }
        let sdl_scope = if sdl_ns.is_empty() {
            sdl_db.to_string()
        } else {
            format!("{sdl_ns}/{sdl_db}")
        };
        Err(format!(
            "the SDL declares database {sdl_scope} but the session is scoped to \
             {ns}/{db} — apply converges the session's own database, at the \
             invoking session's permissions (no escalation). Switch the session \
             to the database the SDL declares, or hand apply that database's SDL"
        ))
    }

    /// The per-table capture statement — the same `<string>` cast the
    /// db-level capture rides, one statement per table, the table
    /// spelled by the shared ident rule (the core's `ident_text`).
    pub fn table_capture_statement(table: &str) -> String {
        format!("RETURN <string>(INFO FOR TABLE {});", ident_text(table))
    }

    /// The capture's table names — the `tables` section's keys, the
    /// default ledger table excluded (the pipeline's own machinery is
    /// never converge material: it is never read for children either,
    /// and never enters the `@tables` map). The flow captures one
    /// `INFO FOR TABLE` per returned name, in this order (sorted —
    /// BTreeMap keys).
    pub fn table_names(info: &surform::info::read::InfoObject) -> Vec<String> {
        use surform::info::read::InfoValue;
        match info.0.get("tables") {
            Some(InfoValue::Object(tables)) => tables
                .0
                .keys()
                .filter(|name| *name != DEFAULT_LEDGER_TABLE)
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The apply flow's own scaffolding, dropped from its capture
    /// before ingestion — the ledger-table precedent extended to the
    /// machinery the flow itself rides:
    ///
    /// - `modules` — a Surrealism registration rides EVERY capture of
    ///   a module-bearing database (apply's always is one), and the
    ///   entries refuse the readers outright (the probe-recorded corner:
    ///   the INFO keys an `org::name` registration `mod::x`, the
    ///   readers expect SDL-shaped names). SDL cannot author a
    ///   module, so the section holds nothing to converge TO. Live
    ///   evidence: one apply failed exactly there —
    ///   `live INFO rejected: 2 diagnostics`, both SDL-E3002 on the
    ///   module entries.
    /// - `buckets` — the module loads from a bucket, and a
    ///   database-only bucket drafts `REMOVE BUCKET IF EXISTS …`
    ///   (live-verified through the CLI generator): without the drop
    ///   an apply whose SDL doesn't declare the bucket would delete
    ///   the module's own bytes — the flow is broken, so the section
    ///   rides out and bucket convergence stays the CLI's flow).
    ///
    /// The CLI's `--info` caller keeps the converge contract unchanged: its
    /// carrier is the caller's, and the exclusion duty is the
    /// caller's (the recorded docs gap).
    pub fn drop_scaffolding(info: &mut surform::info::read::InfoObject) {
        info.0.remove("modules");
        info.0.remove("buckets");
    }

    /// The next ledger number: one past the ledger's highest, 1 for an
    /// empty (or absent) ledger, refused past the CLI's own draft
    /// ceiling (the numbering the status machinery parses is
    /// 1–9999 — a ledger that full refuses rather than rolls over).
    pub fn next_ledger_number(highest: Option<u32>) -> Result<u32, String> {
        let next = highest.map_or(1, |n| n.saturating_add(1));
        if next > 9999 {
            return Err(format!(
                "the migration ledger's numbering is full (the draft range tops \
                 out at 9999; the highest recorded number is {highest:?}) — an \
                 apply cannot self-record past it"
            ));
        }
        Ok(next)
    }

    /// The converged outcome — the CLI's own `(converged)` word, plus
    /// the honesty note that nothing ran and the ledger is untouched.
    pub fn converged_summary() -> String {
        "(converged) — nothing to apply, the ledger untouched".to_string()
    }

    /// The applied outcome: the statement count and the self-record's
    /// ledger key.
    pub fn applied_summary(statements: usize, number: u32) -> String {
        format!(
            "applied {statements} statement(s); recorded {} in the ledger",
            ledger_key(number),
        )
    }

    /// The failed outcome — the engine's error verbatim plus the
    /// dispatch-boundary truth (the module doc's live-probed
    /// transaction): the engine rolled back everything this call did,
    /// so nothing applied and no ledger row exists. Re-running is the
    /// recovery: the next convergence drafts only what still differs
    /// (OVERWRITE redefines; IF EXISTS no-ops). The failing
    /// statement's ordinal is named — that is what the caller fixes.
    pub fn failed_summary(applied: usize, total: usize, error: &str) -> String {
        format!(
            "statement {} of {total} failed: {error}\n\
             the apply is one transaction at the dispatch boundary: the engine \
             rolled back everything this call did, and no ledger row was \
             written. Resolve the failure and re-run — the next convergence \
             drafts only what still differs",
            applied + 1,
        )
    }

    /// The report row's migration key — the CLI's `{number:04}_{name}`.
    fn ledger_key(number: u32) -> String {
        format!("{number:04}_{LEDGER_NAME}")
    }

    /// The ledger self-record's statements, in order — the pipeline's
    /// own block in its generator-marked form
    /// ([`ledger_statement_list`]: the engine-verified spelling,
    /// `DEFINE TABLE IF NOT EXISTS` + typed fields + `INSERT IGNORE`
    /// keyed on the number). The block's INSERT carries `time::now()`,
    /// granted to the apply artifact by exact name.
    pub fn ledger_ride(number: u32, hash: &str, sdl: &str) -> Vec<String> {
        ledger_statement_list(
            DEFAULT_LEDGER_TABLE,
            number,
            LEDGER_NAME,
            hash,
            LEDGER_MESSAGE,
            sdl,
        )
    }

    /// The script hash — the CLI's committed-file definition applied
    /// to the module's shape: sha256 over the executed script's bytes
    /// (the ledger region never splices into the script text here, so
    /// the script IS its own drained form).
    pub fn script_hash(script: &str) -> String {
        sha256_hex(script.as_bytes())
    }
}

/// The host-calling flow behind the `apply` export — wasm
/// only: the SDK's host imports are component bindings. The steps and
/// their guarantees are [`apply`]'s module doc; each host call rides
/// the capability-gated `sql` import at the invoking session's auth.
#[cfg(all(target_arch = "wasm32", feature = "apply"))]
fn apply_sdl(sdl: &str) -> Result<String, String> {
    use surform::info::read::{InfoObject, InfoValue};
    use surform::info::script::ident_text;
    use surrealdb_types::SurrealValue;
    use surrealism::sql;

    /// The SDK's `host sql: ` wrapper prefix is stripped — the
    /// engine's message rides verbatim (probed shape: `The table 'x'
    /// does not exist`). `impl Display`: the SDK's error type is its
    /// own (anyhow's, unlinked here) — Display is the contract the
    /// WIT boundary already commits to.
    fn host_message(error: impl std::fmt::Display) -> String {
        let text = error.to_string();
        match text.strip_prefix("host sql: ") {
            Some(rest) => rest.to_string(),
            None => text,
        }
    }

    /// One host `sql` call expecting the string carrier (the
    /// captures).
    fn host_string(query: &str) -> Result<String, String> {
        sql::<_, String>(query).map_err(host_message)
    }

    /// One host `sql` call expecting the optional-string carrier (the
    /// session scope).
    fn host_opt_string(query: &str) -> Result<Option<String>, String> {
        sql::<_, Option<String>>(query).map_err(host_message)
    }

    /// One host `sql` call accepting any result (the statements).
    fn host_value(query: &str) -> Result<surrealdb_types::Value, String> {
        sql::<_, surrealdb_types::Value>(query).map_err(host_message)
    }

    let loaded = load::load(Path::new(INPUT_NAME), INPUT_NAME, sdl.to_owned())
        .map_err(|failure| render_rejection(&failure))?;
    let (sdl_ns, sdl_db) = database_scope(&loaded.file)?;

    // The session pre-flight (the USE guard, the module form): the
    // invoking session's scope must already be the SDL's database —
    // the module's host calls run at the invoking session's scope, and
    // the `USE` line the script carries is one of the statements the
    // import cannot execute.
    let session_ns = host_opt_string("RETURN session::ns();")?;
    let session_db = host_opt_string("RETURN session::db();")?;
    apply::session_scope_matches(
        session_ns.as_deref(),
        session_db.as_deref(),
        &sdl_ns,
        &sdl_db,
    )?;

    // The capture: the db-level cast, then one cast per table,
    // assembled under the reserved `"@tables"` key — the exact
    // full-fidelity interchange a caller-assembled capture carries.
    // The default ledger table's presence in the capture decides how
    // the self-record's number is learned (the SELECT needs the table
    // to exist; the capture is the live truth).
    let db_capture = host_string("RETURN <string>(INFO FOR DB);")?;
    let mut info: InfoObject = read_info_object(&db_capture).map_err(|diag| {
        render_info_rejection(
            "live INFO rejected",
            std::slice::from_ref(&diag),
            &db_capture,
        )
    })?;
    // The flow's own scaffolding (its registration, its bucket) is in
    // the capture by construction — dropped before ingestion, the
    // ledger-table precedent extended ([`apply::drop_scaffolding`] is
    // the contract and its evidence).
    apply::drop_scaffolding(&mut info);
    let ledger_present = matches!(
        info.0.get("tables"),
        Some(InfoValue::Object(tables)) if tables.0.contains_key(DEFAULT_LEDGER_TABLE)
    );
    let mut children = InfoObject(std::collections::BTreeMap::new());
    for name in apply::table_names(&info) {
        let text = host_string(&apply::table_capture_statement(&name))?;
        let parsed = read_info_object(&text).map_err(|diag| {
            render_info_rejection(
                &format!("live INFO FOR TABLE {name} rejected"),
                std::slice::from_ref(&diag),
                &text,
            )
        })?;
        children.0.insert(name, InfoValue::Object(parsed));
    }
    info.0
        .insert(TABLE_CHILDREN_KEY.to_owned(), InfoValue::Object(children));

    let parts = converge_model(
        &loaded.file,
        surform::info::normalize::ScopeTag::Database(sdl_ns, sdl_db),
        &mut info,
        &db_capture,
    )?;
    if parts.text.is_empty() {
        return Ok(apply::converged_summary());
    }

    // The gates, before ANY host call touches the database, in pinned
    // order: the marker gate first (a marked script's fill-ins need a
    // human — its precedence is pinned), then the additive-only scan
    // over the GENERATOR-MARKED statements (any REMOVE refuses —
    // apply never removes), then the raw-body gate (a raw body's
    // boundaries are the author's, unmarked — it never executes
    // here). No lexical statement splitting exists anywhere in the
    // flow: the emitter marked the boundaries.
    apply::refuse_surform_markers(&parts.text)?;
    apply::refuse_removals(&parts.statements)?;
    apply::refuse_raw_bodies(&parts.raw_bodies)?;

    // The execution: ONE host call per generator-marked statement, in
    // order — exact boundaries, no re-parse. A failure stops the
    // apply — and the engine rolls back everything the call did at
    // the dispatch boundary (all statements, the ledger row
    // included); the failed summary rides the error verbatim.
    let total = parts.statements.len();
    let mut applied = 0usize;
    for statement in &parts.statements {
        if let Err(error) = host_value(statement) {
            return Err(apply::failed_summary(applied, total, &error));
        }
        applied += 1;
    }

    // The self-record: the pipeline's ledger block, executed after
    // every schema statement succeeded — the row exists only for fully
    // applied scripts.
    let highest = if ledger_present {
        let value = host_value(&format!(
            "SELECT VALUE number FROM {};",
            ident_text(DEFAULT_LEDGER_TABLE)
        ))?;
        let numbers = <Vec<i64> as SurrealValue>::from_value(value)
            .map_err(|error| format!("the migration ledger's numbers are unreadable: {error}"))?;
        numbers
            .into_iter()
            .filter_map(|n| u32::try_from(n).ok())
            .max()
    } else {
        None
    };
    let number = apply::next_ledger_number(highest)?;
    let ride = apply::ledger_ride(number, &apply::script_hash(&parts.text), sdl);
    for statement in &ride {
        host_value(statement).map_err(|error| {
            format!(
                "the ledger self-record failed: {error}\n\
                 the apply is one transaction at the dispatch boundary: the \
                 engine rolled back everything this call did — nothing applied, \
                 no ledger row. Resolve the failure and re-run"
            )
        })?;
    }
    Ok(apply::applied_summary(total, number))
}

/// The rejection contract: the spike's proven prefix (its
/// `Function call error: SDL rejected: 1 diagnostics` evidence) plus
/// each diagnostic rendered with the CLI's piped theme — the stable
/// no-color, no-link graphical form `main.rs` installs for non-tty
/// output. Each diagnostic renders against its OWN home's display and
/// text, the CLI `load_or_exit` loop indexed exactly, so the string
/// reads byte-for-byte like a piped `surform check` on the same
/// failure.
fn render_rejection(failure: &load::LoadFailure) -> String {
    let mut out = format!("SDL rejected: {} diagnostics", failure.errors.len());
    for (diag, home) in &failure.errors {
        let home = &failure.homes[*home];
        out.push('\n');
        render_diagnostic(&mut out, diag, &home.display, &home.text);
    }
    out
}

/// The INFO-side rejection render — the same handler and shape as the
/// SDL contract, under a prefix that names the failing carrier. The
/// diagnostics anchor the caller's interchange text (the
/// interchange-reader span contract), so the render reads like the
/// CLI's report against the captured `--info` file.
fn render_info_rejection(prefix: &str, diags: &[surform::Diag], text: &str) -> String {
    let mut out = format!("{prefix}: {} diagnostics", diags.len());
    for diag in diags {
        out.push('\n');
        render_diagnostic(&mut out, diag, LIVE_INFO_NAME, text);
    }
    out
}

/// One diagnostic in the CLI's piped theme (no color, no links) —
/// the shared renderer both rejection contracts ride.
fn render_diagnostic(out: &mut String, diag: &surform::Diag, display: &str, text: &str) {
    let handler = miette::GraphicalReportHandler::new_themed(miette::GraphicalTheme::none())
        .with_links(false);
    let report = miette::Report::new(diag.clone()).with_source_code(miette::NamedSource::new(
        std::borrow::Cow::Owned(display.to_string()),
        text.to_string(),
    ));
    // Rendering into a String cannot fail (the CLI's JSON
    // renderer carries the same expectation).
    handler
        .render_report(out, report.as_ref())
        .expect("diagnostic rendering into a String cannot fail");
}

// The Surrealism export surface. The SDK's WIT bindings target the
// component runtime; the exports exist only in the wasm32-wasip2
// build the module packager consumes (the native rlib the tests and
// clippy run against stays macro-free).
#[cfg(target_arch = "wasm32")]
mod guest {
    use surrealism::surrealism;

    /// `mod::surform::compile(sdl)` — the SDL-to-DDL compile, in the
    /// database. Errors surface as
    /// `Function call error: <the Err string>` verbatim.
    #[surrealism(comment = "SDL text -> SurrealQL DDL via the surform compiler")]
    fn compile(sdl: String) -> Result<String, String> {
        super::compile_sdl(&sdl)
    }

    /// `mod::surform::converge(sdl, live_info)` — the DB-pair
    /// convergence, in the database: the live database's rendered
    /// INFO (capture it with `RETURN <string>(INFO FOR DB);`) against
    /// the SDL's desired state; the returned script converges the
    /// database onto the SDL (empty string = converged). Errors
    /// surface as `Function call error: <the Err string>` verbatim.
    #[surrealism(
        comment = "SDL text + rendered live INFO -> the convergence script (surform migrate generate --info, in the database)"
    )]
    fn converge(sdl: String, live_info: String) -> Result<String, String> {
        super::converge_sdl(&sdl, &live_info)
    }

    /// `mod::surform::status(files, ledger_rows)` — the four-state
    /// migration classification, in the database: the serialized
    /// migrations-dir state + the ledger SELECT's /sql response → the
    /// CLI's status report (empty string never returns; the report is
    /// the contract — DRIFTED and missing rows are the
    /// inconsistency classes a CI gate reads off the text). Errors
    /// surface as `Function call error: <the Err string>` verbatim.
    #[surrealism(
        comment = "serialized migration files + ledger SELECT response -> the migration status report (surform migrate status, in the database)"
    )]
    fn status(files: String, ledger_rows: String) -> Result<String, String> {
        super::status_report(&files, &ledger_rows)
    }

    /// `mod::surform::apply(sdl)` — the apply variant (the
    /// `apply` feature's artifact only): converge + execute, at the
    /// INVOKING SESSION'S AUTH. The session's permissions govern every
    /// executed statement — no escalation. The call's writes are one
    /// transaction at the dispatch boundary: a failed apply rolls
    /// back, a successful one commits whole. See the crate README's
    /// security section before enabling this artifact. Errors surface
    /// as `Function call error: <the Err string>` verbatim.
    #[cfg(feature = "apply")]
    #[surrealism(
        writeable,
        comment = "SDL text -> converge the session's database onto it and execute (at the invoking session's permissions; one transaction per call — a failed apply rolls back)"
    )]
    fn apply(sdl: String) -> Result<String, String> {
        super::apply_sdl(&sdl)
    }
}
