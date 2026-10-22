//! `migrate revert` — a convenience over generate, not a down-chain:
//! the ledger's stored SDL for a number or hash
//! becomes the `--to` side of a fresh convergence draft, the LIVE
//! database the from side. The command GENERATES and stops — the user
//! follows the normal fill → commit → push flow; nothing is ever
//! applied here (generator discipline).
//!
//! # The data path
//!
//! Both sides ride the push client ([`super::push`]) — one data path:
//!
//! - **The recorded state** comes from the ledger's `sdl` column —
//!   the canonical flattened SDL `commit` stored (the state the
//!   migration converges TO, self-contained wherever the working tree
//!   has drifted since). One wider SELECT
//!   (`SELECT number, name, hash, sdl FROM <t>;` — the row shape
//!   engine-probed 3.3.0: `sdl` rides as a JSON string, newlines
//!   intact) and a client-side lookup; no WHERE clause is invented.
//! - **The live state** is the database's rendered schema, fetched
//!   over the same endpoint: `INFO FOR DB;` plus one
//!   `INFO FOR TABLE <name>;` per table — exactly the assembly the
//!   `--info` interchange's `db=` scope spells (the reserved
//!   [`TABLE_CHILDREN_KEY`] map; without the children the compare
//!   could not see field-level drift, and removals are the point of
//!   a revert). Engine-probed 3.3.0 (and the vendored
//!   `exec/operators/info/{database,table}.rs` agree): both results
//!   arrive as JSON OBJECTS whose leaves are the definitions' DDL
//!   strings, so the response converts directly into the interchange
//!   [`InfoObject`] the readers ingest — object → object, string →
//!   DDL, ANY other leaf refuses ([`RevertError::InfoShape`];
//!   fail-closed — no shape is invented for one the probe never
//!   showed). The ingestion is the SAME
//!   [`ReadModel::push_scope`](crate::info::normalize::ReadModel::
//!   push_scope) the `--info` mode runs, and the comparison the SAME
//!   comparator the version pair runs — the `--from` side merely
//!   arrives from the database instead of a file. The generated draft
//!   is therefore byte-for-byte the script
//!   `migrate generate --from <live> --to <recorded>` would emit.
//!
//! # The draft (and its sidecar)
//!
//! The stored SDL is written VERBATIM to a `.surdl` sidecar beside
//! the draft (`NNNN_revert-to-….surdl` — invisible to the numbered
//! `NNNN_*.surql` walk), and the draft's `-- source:` names it. That
//! is what makes the revert draft COMMittable: `migrate commit`
//! re-loads the header's source to fill the new ledger row's `sdl`
//! column, and the sidecar re-loads to exactly the recorded state —
//! so reverting the revert works, and a pointer at the (long since
//! moved-on) working schema never lies in the ledger. The pair is
//! written all-or-nothing in effect: a draft-write failure removes
//! the sidecar it had just created — no invisible orphan outlives a
//! failed run (a `.surdl` is invisible to every walk, so a leftover
//! would collide every re-run at the same number with no way
//! forward).
//!
//! # The scope
//!
//! Revert compares at DATABASE scope — the scope the ledger lives
//! in. The stored SDL must declare the target database (the
//! `db=<ns>/<db>` spelling of `--ns`/`--db`, or the namespace-free
//! pseudo-scope when the file declares no namespace); anything else
//! refuses — a scope the compare never ingested makes no claims, and
//! a silent empty draft would be a false "converged". Root- and
//! namespace-scope content in the stored SDL stays out of the
//! revert's view (documented; the migration ledger is a
//! database-level history).
//!
//! The LEDGER TABLE itself is the pipeline's machinery, and the live
//! fetch leaves it out of the compare: it is a real table (every
//! migration's self-record creates it), so an unfiltered INFO would
//! classify it database-only and the draft would remove it — wiping
//! the history the revert is about to append to. The `--ledger-table`
//! name is dropped from the live `tables` section before ingestion;
//! no revert can draft its removal.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use super::json::Json;
use super::push::PushError;
use super::transport::{Target, TransportError, agent, info_results, post_sql, read_ledger_rows};
use super::{DraftError, InputMode, name_slug, next_draft_number, scan_markers, write_draft};
use crate::info::diff::diff;
use crate::info::normalize::{ReadModel, ScopeTag, declared_scopes};
use crate::info::read::{InfoObject, InfoValue};
use crate::info::script::{SlotStyle, convergence_script_styled, ident_text};
use crate::parser::parse_file_composable;

/// The interchange's reserved per-table child-map key — the spelling
/// the db-scope ingestion reads under ([`TABLE_CHILDREN_KEY`]'s
/// contract; the live fetch assembles the same shape).
use crate::info::normalize::TABLE_CHILDREN_KEY;

/// A located ledger row with its revert substrate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FullRow {
    pub(crate) name: String,
    pub(crate) hash: String,
    /// The canonical SDL the row's migration converged to — `None`
    /// when the row carries no readable `sdl` string (a hand-made
    /// ledger row; refused at lookup, never guessed around).
    pub(crate) sdl: Option<String>,
}

/// Everything revert refuses, with its user-facing text (the CLI
/// prints `error: {e}` and exits 1).
#[derive(Debug)]
pub enum RevertError {
    /// The `--to` argument is neither a migration number (1–4 digits)
    /// nor a hash prefix (4–64 hex characters).
    TargetUnreadable(String),
    /// No ledger row matches the number or hash prefix.
    RowNotFound { spec: String },
    /// The hash prefix matches several rows — the candidates, named.
    RowAmbiguous {
        spec: String,
        candidates: Vec<(u32, String, String)>,
    },
    /// The matched row carries no readable `sdl` column — nothing to
    /// converge toward.
    RowSdlMissing { number: u32 },
    /// The stored SDL does not parse (it should always — it is this
    /// tool's own canonical output; a hand-edited ledger row is the
    /// likely cause). The diagnostics ride pre-rendered.
    StoredSchema { diagnostics: Vec<String> },
    /// The stored SDL never declares the database `--ns`/`--db` name —
    /// the compare would silently make no claims.
    StoredSchemaScope {
        ns: String,
        db: String,
        databases: Vec<(String, String)>,
    },
    /// The live INFO results carried a shape the probed contract does
    /// not spell (a leaf that is neither a DDL string nor an object).
    InfoShape { detail: String },
    /// The live schema's own read failed (a DDL string the reverse
    /// readers diagnose, for instance) — fail-closed, the diagnostics
    /// ride pre-rendered.
    LiveInfo { diagnostics: Vec<String> },
    /// The wire layer (transport, HTTP, ledger/schema reads) — the
    /// push error's own text is the message.
    Wire(PushError),
    /// The draft or sidecar could not be written.
    Draft(DraftError),
    /// The sidecar file already exists — either a concurrent writer
    /// took the numbering or an earlier failed run left the file
    /// behind; nothing was overwritten, and the message names both
    /// recoveries.
    SidecarCollision(PathBuf),
    /// The draft write failed after this run had already created the
    /// sidecar — the sidecar was removed with the failed draft
    /// (nothing left behind), and the draft failure's own text rides.
    DraftWrite { detail: String },
    /// The draft write failed AND the sidecar this run had just
    /// created could not be removed either — the named file is this
    /// run's leftover (the create-new proved no other writer can
    /// claim it), and the message says to delete it by hand.
    SidecarLeft {
        path: PathBuf,
        detail: String,
        reason: String,
    },
    /// The rendered draft's own slot markers failed to re-scan — the
    /// markers are this binary's own output, so this is a renderer
    /// bug (commit would refuse the same draft downstream); the
    /// refusal surfaces here, before anything is written.
    SlotsUnscannable(Vec<String>),
    /// The sidecar could not be written.
    Unwritable { path: PathBuf, reason: String },
}

impl fmt::Display for RevertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RevertError::TargetUnreadable(spec) => write!(
                f,
                "the revert target `{spec}` is neither a migration number \
                 (1–4 digits, e.g. `3` or `0003`) nor a hash prefix \
                 (4–64 hexadecimal characters)"
            ),
            RevertError::RowNotFound { spec } => write!(
                f,
                "no ledger row matches `{spec}` — `migrate status` lists what \
                 the database holds"
            ),
            RevertError::RowAmbiguous { spec, candidates } => {
                write!(
                    f,
                    "the hash prefix `{spec}` matches {} ledger rows — name more characters:",
                    candidates.len()
                )?;
                for (number, name, hash) in candidates {
                    write!(f, "\n  - {number:04}_{name} (sha256:{hash})")?;
                }
                Ok(())
            }
            RevertError::RowSdlMissing { number } => write!(
                f,
                "the ledger row {number:04} carries no readable `sdl` column — \
                 the recorded state is the revert substrate, and this row has \
                 none (a hand-made ledger row?)"
            ),
            RevertError::StoredSchema { diagnostics } => {
                write!(
                    f,
                    "the ledger's stored SDL does not parse — it is this \
                           tool's own canonical output, so a hand-edited ledger \
                           row is the likely cause:"
                )?;
                for diagnostic in diagnostics {
                    write!(f, "\n  - {diagnostic}")?;
                }
                Ok(())
            }
            RevertError::StoredSchemaScope { ns, db, databases } => write!(
                f,
                "the stored SDL never declares database `{db}` under namespace \
                 `{ns}` — a scope the compare never ingested makes no claims, \
                 so the revert refuses rather than draft a silent no-op. The \
                 stored schema declares: {}",
                if databases.is_empty() {
                    "(no databases)".to_string()
                } else {
                    databases
                        .iter()
                        .map(|(n, d)| format!("{n}/{d}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
            RevertError::InfoShape { detail } => write!(
                f,
                "the live schema's INFO result carries a shape the probed /sql \
                 contract does not spell — {detail}"
            ),
            RevertError::LiveInfo { diagnostics } => {
                write!(f, "reading the live schema failed:")?;
                for diagnostic in diagnostics {
                    write!(f, "\n  - {diagnostic}")?;
                }
                Ok(())
            }
            RevertError::Wire(error) => write!(f, "{error}"),
            RevertError::Draft(error) => write!(f, "{error}"),
            RevertError::SidecarCollision(path) => write!(
                f,
                "the recorded-state file {} already exists — nothing was \
                 overwritten. Either another `surform migrate` writer took \
                 the numbering just now (re-run this command once it \
                 finishes), or the file is a leftover from an earlier run \
                 that failed mid-write (verify it is a revert sidecar for \
                 this same target, delete it, then re-run)",
                path.display()
            ),
            RevertError::DraftWrite { detail } => write!(
                f,
                "the revert draft could not be written — {detail}. The \
                 recorded-state sidecar written alongside it was removed \
                 with the failed draft (nothing left behind); resolve the \
                 cause and re-run the command"
            ),
            RevertError::SidecarLeft {
                path,
                detail,
                reason,
            } => write!(
                f,
                "the revert draft could not be written — {detail}; the \
                 recorded-state sidecar {} written alongside it could not \
                 be removed either ({reason}) — it is this run's leftover, \
                 no other writer can claim it: delete it by hand, then \
                 re-run",
                path.display()
            ),
            RevertError::SlotsUnscannable(details) => {
                write!(
                    f,
                    "the rendered draft's slot markers do not re-scan — a \
                     surform bug (the markers are this tool's own output; \
                     please report it), and the draft was not written:"
                )?;
                for detail in details {
                    write!(f, "\n  - {detail}")?;
                }
                Ok(())
            }
            RevertError::Unwritable { path, reason } => {
                write!(f, "the file {} cannot be written: {reason}", path.display())
            }
        }
    }
}

impl From<PushError> for RevertError {
    fn from(error: PushError) -> Self {
        RevertError::Wire(error)
    }
}

impl From<TransportError> for RevertError {
    fn from(error: TransportError) -> Self {
        RevertError::Wire(error.into())
    }
}

impl From<DraftError> for RevertError {
    fn from(error: DraftError) -> Self {
        RevertError::Draft(error)
    }
}

/// The revert's product: where the draft landed, what it converges
/// toward, and the slots the user must fill before committing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevertOutcome {
    /// The new draft (`NNNN_revert-to-….surql`).
    pub draft: PathBuf,
    /// The recorded state's sidecar (`.surdl`) — the draft's
    /// `-- source:`, written verbatim from the ledger's `sdl` column.
    pub source: PathBuf,
    /// The draft's number (the directory's next).
    pub number: u32,
    /// The ledger row the draft converges toward.
    pub row_number: u32,
    pub row_name: String,
    pub row_hash: String,
    /// The unresolved slot names the draft carries (empty when the
    /// recorded state orphans no data).
    pub slots: Vec<String>,
}

/// The revert run: locate the row, fetch the live schema, converge —
/// and write the draft. `Ok(None)` when the database already matches
/// the recorded state (nothing to write).
pub fn revert(
    dir: &Path,
    ledger_table: &str,
    target: &Target,
    to: &str,
) -> Result<Option<RevertOutcome>, RevertError> {
    target.validate()?;
    // The spelling check first — an unreadable target never dials.
    let spec = parse_target(to)?;
    let agent = agent();
    // The row: the wider projection (the sdl column rides along),
    // parsed with the SAME response walk and fresh-database branch
    // every ledger read uses.
    let select = format!(
        "SELECT number, name, hash, sdl FROM {};",
        ident_text(ledger_table)
    );
    let rows: BTreeMap<u32, FullRow> =
        read_ledger_rows(post_sql(&agent, target, &select)?, |row| {
            Ok(FullRow {
                name: row
                    .get("name")
                    .and_then(Json::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| TransportError::MalformedResponse {
                        detail: "a ledger row carries no `name` string".to_string(),
                    })?,
                hash: row
                    .get("hash")
                    .and_then(Json::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| TransportError::MalformedResponse {
                        detail: "a ledger row carries no `hash` string".to_string(),
                    })?,
                sdl: row.get("sdl").and_then(Json::as_str).map(str::to_string),
            })
        })?;
    let (row_number, row) = locate(&spec, to, &rows)?;
    let Some(sdl_text) = row.sdl.clone() else {
        return Err(RevertError::RowSdlMissing { number: row_number });
    };
    // The recorded state, parsed fail-closed: a canonical document of
    // this tool's own making must parse clean.
    let (stored, diags) = parse_file_composable(&sdl_text);
    if !diags.is_empty() {
        return Err(RevertError::StoredSchema {
            diagnostics: diags.iter().map(ToString::to_string).collect(),
        });
    }
    // The compare's scope: the database `--ns`/`--db` names, as the
    // stored file declares it (the namespace-free pseudo-scope when
    // the file declares no namespace — a versions-mode file's common
    // spelling).
    let (namespaces, databases) = declared_scopes(&stored);
    let scope = if databases.contains(&(target.ns.clone(), target.db.clone())) {
        ScopeTag::Database(target.ns.clone(), target.db.clone())
    } else if namespaces.is_empty() && databases.contains(&(String::new(), target.db.clone())) {
        ScopeTag::Database(String::new(), target.db.clone())
    } else {
        return Err(RevertError::StoredSchemaScope {
            ns: target.ns.clone(),
            db: target.db.clone(),
            databases: databases.iter().cloned().collect(),
        });
    };
    // The live state: INFO FOR DB, then one INFO FOR TABLE per table —
    // the db interchange's exact assembly, live. The ledger table is
    // dropped first (module doc: the pipeline's own machinery is
    // never revert material — removing it would wipe the history the
    // revert is about to append to).
    let db_info = fetch_info_object(&agent, target, "INFO FOR DB;")?;
    let mut live = db_info;
    if let Some(InfoValue::Object(tables)) = live.0.get_mut("tables") {
        tables.0.remove(ledger_table);
    }
    let children = match live.0.get("tables") {
        Some(InfoValue::Object(tables)) => {
            let mut children: BTreeMap<String, InfoValue> = BTreeMap::new();
            for name in tables.0.keys() {
                let query = format!("INFO FOR TABLE {};", ident_text(name));
                let child = fetch_info_object(&agent, target, &query)?;
                children.insert(name.clone(), InfoValue::Object(child));
            }
            children
        }
        _ => {
            return Err(RevertError::InfoShape {
                detail: "the `INFO FOR DB` result carries no `tables` object".to_string(),
            });
        }
    };
    live.0.insert(
        TABLE_CHILDREN_KEY.to_string(),
        InfoValue::Object(InfoObject(children)),
    );
    // The one comparator, live side in the read-model slot, recorded
    // side the file — the --info mode's exact ingestion and diff.
    let mut model = ReadModel::default();
    let live_diags = model.push_scope(scope, &live).err().unwrap_or_default();
    if !live_diags.is_empty() {
        return Err(RevertError::LiveInfo {
            diagnostics: live_diags.iter().map(ToString::to_string).collect(),
        });
    }
    let findings = diff(&stored, &model);
    let marked = convergence_script_styled(&findings, &stored, SlotStyle::ResolutionMarkers);
    if marked.is_empty() {
        return Ok(None);
    }
    // The frame gate BEFORE the sidecar churn (write_draft enforces
    // the same shape — this is the early, no-side-effects spelling):
    // the file format wraps exactly ONE transaction, so a
    // multi-scope stored file's per-scope-section script (or an
    // owner-defines-only one) is no draft.
    let frames = marked.lines().filter(|line| *line == "COMMIT;").count();
    if frames != 1 {
        return Err(RevertError::Draft(DraftError::FrameShape { frames }));
    }
    // The slot list FIRST: the markers are this binary's own output,
    // so a scan failure is a renderer bug, and it refuses before
    // anything is written (commit would refuse the same draft
    // downstream — the report belongs to the renderer's own command,
    // not to a stdout that quietly claims "no data-migration slots").
    let slots = scan_markers(&marked).map_err(RevertError::SlotsUnscannable)?;
    // The draft + its recorded-state sidecar. The number is computed
    // here for the sidecar's name and again inside `write_draft` (the
    // sidecar is invisible to the numbered walk, so both reads agree
    // short of a racing writer — the create-new writes catch that).
    std::fs::create_dir_all(dir).map_err(|reason| RevertError::Unwritable {
        path: dir.to_path_buf(),
        reason: reason.to_string(),
    })?;
    let number = next_draft_number(dir)?;
    let base = name_slug(&format!("revert-to-{row_number:04}-{}", row.name));
    let generated = super::now_utc();
    let (draft, sidecar) = write_sidecar_and_draft(dir, &base, number, &sdl_text, |source| {
        write_draft(
            dir,
            super::DraftSave {
                name: Some(&base),
                source,
                // The recorded state is already canonical (the ledger's
                // `sdl` column) — its hash pins the sidecar commit
                // re-canonicalizes (issue #68's check, from revert's
                // side).
                source_sha256: Some(&super::sha256_hex(sdl_text.as_bytes())),
                // The diff's inputs ride the header (issue #75): the
                // live side has no file spelling, so `from=(live)` —
                // the live schema AT REVERT TIME — and the sidecar
                // (the recorded state) as `to`.
                input: InputMode::Versions {
                    from: "(live)".to_string(),
                    to: source.to_string(),
                },
                generated: &generated,
                marked_script: &marked,
            },
        )
    })?;
    Ok(Some(RevertOutcome {
        slots,
        draft,
        source: sidecar,
        number,
        row_number,
        row_name: row.name.clone(),
        row_hash: row.hash.clone(),
    }))
}

/// The `--to` argument's parsed spelling: a migration NUMBER or a
/// HASH PREFIX.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetSpec {
    /// 1–4 digits, value 1..=9999 — the ledger is number-unique per
    /// database (the binding decision).
    Number(u32),
    /// 4–64 hex characters, case-folded — git's abbreviation
    /// convention; ambiguity names the candidates.
    HashPrefix(String),
}

/// Parses the spelling BEFORE anything is sent (an unreadable target
/// never dials). The split is deliberate: a 4-digit all-digit string
/// is a number, period — the numbering owns that spelling.
fn parse_target(to: &str) -> Result<TargetSpec, RevertError> {
    if !to.is_empty()
        && to.len() <= 4
        && to.bytes().all(|b| b.is_ascii_digit())
        && let Ok(number) = to.parse::<u32>()
    {
        return Ok(TargetSpec::Number(number));
    }
    let prefix = to.to_ascii_lowercase();
    if (4..=64).contains(&prefix.len()) && prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(TargetSpec::HashPrefix(prefix));
    }
    Err(RevertError::TargetUnreadable(to.to_string()))
}

/// Locates the ledger row the parsed spec names.
fn locate<'a>(
    spec: &TargetSpec,
    to: &str,
    rows: &'a BTreeMap<u32, FullRow>,
) -> Result<(u32, &'a FullRow), RevertError> {
    match spec {
        TargetSpec::Number(number) => {
            rows.get(number)
                .map(|row| (*number, row))
                .ok_or_else(|| RevertError::RowNotFound {
                    spec: to.to_string(),
                })
        }
        TargetSpec::HashPrefix(prefix) => {
            let matches: Vec<(u32, &FullRow)> = rows
                .iter()
                .filter(|(_, row)| row.hash.starts_with(prefix))
                .map(|(number, row)| (*number, row))
                .collect();
            match matches.as_slice() {
                [] => Err(RevertError::RowNotFound {
                    spec: to.to_string(),
                }),
                [(number, row)] => Ok((*number, row)),
                many => Err(RevertError::RowAmbiguous {
                    spec: to.to_string(),
                    candidates: many
                        .iter()
                        .map(|(number, row)| (*number, row.name.clone(), row.hash.clone()))
                        .collect(),
                }),
            }
        }
    }
}

/// One INFO statement's answer, parsed against the probed shapes: a
/// 200 whose element is `status: "OK"` with a JSON-object `result`
/// ([`info_results`]'s one-query walk), converted by
/// [`json_to_info`].
fn fetch_info_object(
    agent: &ureq::Agent,
    target: &Target,
    query: &str,
) -> Result<InfoObject, RevertError> {
    let mut results = info_results(agent, target, query, 1)?;
    json_to_info(&results.remove(0))
}

/// The probed INFO shape's conversion into the interchange model:
/// JSON object → [`InfoObject`], JSON string → [`InfoValue::Ddl`],
/// ANY other leaf — a number, bool, null, or array where the probe
/// and the vendored info operators show only objects-of-strings —
/// refuses ([`RevertError::InfoShape`]; fail-closed, nothing
/// invented).
fn json_to_info(value: &Json) -> Result<InfoObject, RevertError> {
    let shape = |spelled: &str| RevertError::InfoShape {
        detail: format!(
            "a `{spelled}` leaf where the probed INFO shape carries only DDL \
             strings and objects"
        ),
    };
    let Json::Obj(members) = value else {
        return Err(shape("non-object result"));
    };
    let mut map = BTreeMap::new();
    for (key, value) in members {
        let converted = match value {
            Json::Str(text) => InfoValue::Ddl(text.clone()),
            Json::Obj(_) => InfoValue::Object(json_to_info(value)?),
            Json::Null => return Err(shape("null")),
            Json::Bool(_) => return Err(shape("boolean")),
            Json::Num(_) => return Err(shape("number")),
            Json::Arr(_) => return Err(shape("array")),
        };
        map.insert(key.clone(), converted);
    }
    Ok(InfoObject(map))
}

/// The sidecar's create-new write — the same guard class as the
/// draft's ([`super`]'s `write_create_new`): an existing target is
/// [`RevertError::SidecarCollision`], never a silent overwrite.
fn write_create_new(path: &Path, text: &str) -> Result<(), RevertError> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => std::io::Write::write_all(&mut file, text.as_bytes()).map_err(|reason| {
            RevertError::Unwritable {
                path: path.to_path_buf(),
                reason: reason.to_string(),
            }
        }),
        Err(reason) if reason.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(RevertError::SidecarCollision(path.to_path_buf()))
        }
        Err(reason) => Err(RevertError::Unwritable {
            path: path.to_path_buf(),
            reason: reason.to_string(),
        }),
    }
}

/// The sidecar-then-draft write pair, with the sidecar's cleanup: the
/// sidecar lands first (create-new — the collision guard), the draft
/// second, and a draft-write failure REMOVES the sidecar before the
/// error returns. An orphaned sidecar is invisible to every walk, so
/// a leftover would collide every re-run at the same number —
/// [`RevertError::SidecarCollision`] against a file nothing explains.
/// The removal races nothing: the create-new proved this run's
/// ownership of the file, so no concurrent writer's bytes can be
/// deleted. Its own failure surfaces ([`RevertError::SidecarLeft`])
/// rather than silently re-creating the poison. The `draft_write`
/// closure is the seam the tests inject their forced failures
/// through (the defended race cannot be staged black-box —
/// `next_draft_number` skips past every parseable name, so a
/// pre-placed draft only bumps the counter); it receives the
/// sidecar's path spelled as the draft's `-- source:` header will
/// carry it.
fn write_sidecar_and_draft(
    dir: &Path,
    base: &str,
    number: u32,
    sdl_text: &str,
    draft_write: impl FnOnce(&str) -> Result<PathBuf, DraftError>,
) -> Result<(PathBuf, PathBuf), RevertError> {
    let sidecar = dir.join(format!("{number:04}_{base}.surdl"));
    write_create_new(&sidecar, sdl_text)?;
    let source = sidecar.display().to_string();
    match draft_write(&source) {
        Ok(draft) => Ok((draft, sidecar)),
        Err(error) => match std::fs::remove_file(&sidecar) {
            Ok(()) => Err(RevertError::DraftWrite {
                detail: error.to_string(),
            }),
            Err(reason) => Err(RevertError::SidecarLeft {
                path: sidecar,
                detail: error.to_string(),
                reason: reason.to_string(),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam's signature — one injected draft-write failure.
    type DraftWrite = fn(&str) -> Result<PathBuf, DraftError>;

    /// The forced write-failure leg: the draft write
    /// fails after the sidecar was created — the defended race
    /// ([`DraftError::Collision`]) and an injected fs error — and NO
    /// sidecar is left behind; the error names the recovery.
    #[test]
    fn a_failed_draft_write_leaves_no_sidecar() {
        let dir = std::env::temp_dir().join(format!(
            "surform-revert-orphan-{}-{}",
            std::process::id(),
            line!(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch dir creates");
        let legs: [(&str, DraftWrite); 2] = [
            ("race", |_| {
                Err(DraftError::Collision(PathBuf::from("0007_race.surql")))
            }),
            ("fs-error", |_| {
                Err(DraftError::Unwritable {
                    path: PathBuf::from("0007_fs.surql"),
                    reason: "injected".to_string(),
                })
            }),
        ];
        for (tag, draft_write) in legs {
            let outcome = write_sidecar_and_draft(
                &dir,
                "revert-to-0001-v1",
                7,
                "the recorded state\n",
                draft_write,
            );
            let error = outcome.expect_err(&format!("{tag}: the injected failure rides"));
            let text = error.to_string();
            assert!(
                text.contains("re-run"),
                "{tag}: the error names the recovery: {text}"
            );
            assert!(
                !dir.join("0007_revert-to-0001-v1.surdl").exists(),
                "{tag}: no sidecar outlives the failed draft write"
            );
            assert_eq!(
                std::fs::read_dir(&dir).unwrap().count(),
                0,
                "{tag}: the directory is exactly as the run found it"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The cleanup's own failure leg: the sidecar cannot be removed —
    /// the error names the leftover file and its hand-delete remedy
    /// ([`RevertError::SidecarLeft`]) instead of pretending nothing
    /// was written. The closure swaps the just-created sidecar for a
    /// directory of the same name (the one removal failure stageable
    /// portably).
    #[test]
    fn an_uncleanable_sidecar_is_named_by_the_error() {
        let dir = std::env::temp_dir().join(format!(
            "surform-revert-orphan-{}-{}",
            std::process::id(),
            line!(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch dir creates");
        let sidecar = dir.join("0007_revert-to-0001-v1.surdl");
        let outcome = write_sidecar_and_draft(
            &dir,
            "revert-to-0001-v1",
            7,
            "the recorded state\n",
            move |source| {
                let stand_in = PathBuf::from(source);
                std::fs::remove_file(&stand_in).expect("the owned sidecar removes");
                std::fs::create_dir(&stand_in).expect("the stand-in dir creates");
                Err::<PathBuf, DraftError>(DraftError::Collision(PathBuf::from("0007_x.surql")))
            },
        );
        let error = outcome.expect_err("the removal failure surfaces");
        assert!(matches!(error, RevertError::SidecarLeft { .. }), "{error}");
        let text = error.to_string();
        assert!(text.contains(&sidecar.display().to_string()), "{text}");
        assert!(text.contains("delete it by hand"), "{text}");
        // The stand-in directory IS the leftover — the test's own
        // scratch, removed here.
        std::fs::remove_dir_all(&dir).expect("the scratch dir cleans");
    }

    /// The renderer-bug refusal's text: the
    /// scan failure rides pre-rendered, every detail named — the
    /// reporter must not understate the slot list.
    #[test]
    fn the_slots_unscannable_refusal_names_every_detail() {
        let error = RevertError::SlotsUnscannable(vec![
            "one broken marker".to_string(),
            "another".to_string(),
        ]);
        let text = error.to_string();
        assert!(text.contains("a surform bug"), "{text}");
        assert!(text.contains("the draft was not written"), "{text}");
        assert!(text.contains("one broken marker"), "{text}");
        assert!(text.contains("another"), "{text}");
    }
}
