//! `migrate status` — compares the migrations directory against the
//! live migration ledger.
//!
//! # The data source
//!
//! The first cut read the ledger through the `--info`
//! interchange, on the assumption that a regular table shows in
//! captured INFO. It does not help: INFO is SCHEMA, not rows —
//! `INFO FOR DB` reports the ledger table's `DEFINE TABLE` text in its
//! `tables` section (probe + vendored `exec/operators/info/database.rs`)
//! and never the ledger's ROWS, so the number/name/hash comparisons
//! status exists to make have no interchange carrier. Status therefore
//! reads the ledger ROWS live through the push client
//! ([`super::push`]) — the same `POST /sql` endpoint, the same
//! `SELECT number, name, hash FROM <t>;` projection push's
//! skip-applied reads, the same fresh-database branch (a missing
//! ledger table on kind+subject is an empty ledger, anything else is
//! an error). One connectivity surface, one data path; the HTTP
//! boundary stays mocked in tests, and status — a report — never
//! applies anything.
//!
//! The LIVE run (an endpoint given) ALWAYS makes the ledger read
//! (even for an empty directory): the ledger-only side of the
//! comparison — rows the directory has no file for — is one of the
//! states it reports. The OFFLINE run ([`status_offline`], issue
//! #77: no endpoint on the CLI) skips the ledger entirely — the
//! local classification only, [`FileState::Committed`] per committed
//! file, exit-consistent by construction.
//!
//! # The classification
//!
//! Per migration file (the [`super::push`] directory walk: number
//! order, duplicates refuse, a numbered non-migration refuses):
//!
//! - **applied** — the ledger records the number and the file's
//!   CURRENT filled-script hash ([`super::filled_script`] ∘
//!   [`super::sha256_hex`], the exact re-derivation re-commit and
//!   push run) equals the ledger's hash: what is on disk is what the
//!   database recorded.
//! - **pending** — the ledger does not record the number: committed,
//!   not yet pushed (the pipeline's normal un-pushed state).
//! - **draft** — the file carries no committed header: work in
//!   progress, not yet history (push's skip verdict, reported not
//!   fatal).
//! - **committed** — the offline run's committed verdict: the header
//!   says committed, the ledger state is UNKNOWN (no endpoint to
//!   ask).
//! - **DRIFTED** — the ledger records the number with a DIFFERENT
//!   hash: the file was edited after the database recorded it (or
//!   another directory's migration owns the number). Both hashes are
//!   named; the same class push refuses to apply and commit refuses
//!   to re-certify.
//!
//! Per ledger row with no local file: **missing** — the database
//! holds history the directory lost (a deleted file, a checked-out
//! branch without it).
//!
//! # The exit rule
//!
//! Exit 0 when directory and ledger are CONSISTENT — every file
//! applied/pending/draft and every ledger row has a file; exit 1 when
//! any DRIFTED or MISSING exists. Pending is not an inconsistency (a
//! CI gate on a pre-push commit must pass); drift and missing are —
//! they are the corruption-class states push and commit already
//! refuse to compound. The CI-friendly rule: `surform migrate status
//! … || echo incoherent` gates history integrity, not progress.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::push::{PushError, collect};
use super::transport::{Target, agent, post_sql, read_ledger};

/// One migration file's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileState {
    /// The ledger records the number with this file's current hash.
    Applied,
    /// Committed, not yet in the ledger — not yet pushed.
    Pending,
    /// No committed header — work in progress.
    Draft,
    /// Committed, and the LEDGER WAS NEVER READ — the offline run
    /// (issue #77: `status` without an endpoint). The local
    /// classification is all there is; nothing can be DRIFTED or
    /// MISSING without a ledger, so the exit rule reads consistent.
    Committed,
    /// The ledger records the number with a different hash: the file
    /// was edited after the database recorded it (or another
    /// directory's migration owns the number). Both hashes are named
    /// — the file's current script hash and the ledger's.
    Drifted {
        file_hash: String,
        ledger_hash: String,
    },
}

/// One migration file's report row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReport {
    pub number: u32,
    pub name: String,
    /// The report-row key: the file's ledger identity spelled the way
    /// the directory spells it (`0001_add-users`, or a group member's
    /// `0005b_shop`).
    pub key: String,
    pub path: PathBuf,
    /// The draft-group position (`None` for a standalone draft) —
    /// the report's grouped rendering reads it.
    pub group: Option<super::GroupPos>,
    pub state: FileState,
}

/// A ledger row the directory has no file for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingRow {
    pub number: u32,
    pub name: String,
    pub hash: String,
}

/// The status report: the directory's files in number order, then the
/// ledger's missing rows in number order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusOutcome {
    pub files: Vec<FileReport>,
    pub missing: Vec<MissingRow>,
}

impl StatusOutcome {
    /// The exit rule: any DRIFTED file or MISSING row is an
    /// inconsistency (module doc, "The exit rule") — the CI signal.
    pub fn inconsistent(&self) -> bool {
        self.files
            .iter()
            .any(|file| matches!(file.state, FileState::Drifted { .. }))
            || !self.missing.is_empty()
    }
}

/// The status run: the directory walk (local, refusing before any
/// request — [`super::push::collect`]'s substrate), then ONE ledger
/// SELECT, then the classification. Nothing is ever applied.
pub fn status(
    dir: &std::path::Path,
    ledger_table: &str,
    target: &Target,
) -> Result<StatusOutcome, PushError> {
    target.validate()?;
    // The local walk FIRST (push's pre-flight discipline): a missing
    // directory, a duplicate number, or an unparseable file refuses
    // before anything is sent.
    let candidates = collect(dir)?;
    let agent = agent();
    let select = format!(
        "SELECT number, name, hash FROM {};",
        crate::info::script::ident_text(ledger_table)
    );
    let ledger: BTreeMap<u32, super::transport::LedgerRow> =
        read_ledger(post_sql(&agent, target, &select)?)?;
    let mut files = Vec::new();
    for run in candidates.chunk_by(|a, b| a.number == b.number) {
        if run[0].group.is_none() {
            // The standalone shape: the file's own hash vs the row.
            let candidate = &run[0];
            let state = match &candidate.committed {
                None => FileState::Draft,
                // The comparison's definition: the file's
                // CURRENT script hash against the LEDGER's — the
                // header's recorded hash is not consulted (what is on
                // disk vs what the database recorded is the whole
                // question).
                Some(_) => {
                    let current = candidate.current_hash();
                    match ledger.get(&candidate.number) {
                        None => FileState::Pending,
                        Some(row) if row.hash == current => FileState::Applied,
                        Some(row) => FileState::Drifted {
                            file_hash: current,
                            ledger_hash: row.hash.clone(),
                        },
                    }
                }
            };
            files.push(FileReport {
                number: candidate.number,
                name: candidate.name.clone(),
                key: candidate.key.clone(),
                group: candidate.group,
                path: candidate.path.clone(),
                state,
            });
            continue;
        }
        // The group shape (issue #87): the members' verdicts are
        // driven by the GROUP's row through the RECORDING member (the
        // header's last member — the one whose commit spliced the
        // block), never by per-member hash compares (the row carries
        // one hash, the last member's). Draft members render Draft;
        // a group with no committed recording member renders its
        // committed members Pending (nothing to judge the row by —
        // and push refuses to apply such a half at all).
        let number = run[0].number;
        let verdict: Option<Option<bool>> = run
            .iter()
            .find(|candidate| {
                candidate.group.is_some_and(|group| group.is_last())
                    && candidate.committed.is_some()
            })
            .map(|recording| {
                ledger
                    .get(&number)
                    .map(|row| row.hash == recording.current_hash())
            });
        for candidate in run {
            let state = match &candidate.committed {
                None => FileState::Draft,
                Some(_) => match verdict {
                    None | Some(None) => FileState::Pending,
                    Some(Some(true)) => FileState::Applied,
                    Some(Some(false)) => {
                        if candidate.group.is_some_and(|group| group.is_last()) {
                            // The recording member is the drifted one;
                            // its hashes are the story.
                            FileState::Drifted {
                                file_hash: candidate.current_hash(),
                                ledger_hash: ledger
                                    .get(&number)
                                    .map(|row| row.hash.clone())
                                    .unwrap_or_default(),
                            }
                        } else {
                            // An earlier member applied with the
                            // group (the walk is whole) — only the
                            // recording member can drift the row.
                            FileState::Applied
                        }
                    }
                },
            };
            files.push(FileReport {
                number: candidate.number,
                name: candidate.name.clone(),
                key: candidate.key.clone(),
                group: candidate.group,
                path: candidate.path.clone(),
                state,
            });
        }
    }
    let local_numbers: std::collections::BTreeSet<u32> =
        files.iter().map(|file| file.number).collect();
    let missing = ledger
        .into_iter()
        .filter(|(number, _)| !local_numbers.contains(number))
        .map(|(number, row)| MissingRow {
            number,
            name: row.name,
            hash: row.hash,
        })
        .collect();
    Ok(StatusOutcome { files, missing })
}

/// The OFFLINE run (issue #77): the directory walk and the local
/// classification only — draft and committed-per-header per file, no
/// ledger comparison, no missing rows (there is no ledger to miss
/// from). NEVER dials: half of status is local state, and a dead
/// endpoint cannot take that half away. Exit-consistent by
/// construction (no [`FileState::Drifted`], no missing).
pub fn status_offline(dir: &std::path::Path) -> Result<StatusOutcome, PushError> {
    let files = collect(dir)?
        .into_iter()
        .map(|candidate| FileReport {
            number: candidate.number,
            name: candidate.name,
            key: candidate.key,
            group: candidate.group,
            path: candidate.path,
            state: match candidate.committed {
                Some(_) => FileState::Committed,
                None => FileState::Draft,
            },
        })
        .collect();
    Ok(StatusOutcome {
        files,
        missing: Vec::new(),
    })
}
