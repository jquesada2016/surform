//! `migrate push` — applies committed migration files to a live
//! database through SurrealDB's raw HTTP `POST /sql` endpoint
//! (the HTTP endpoint, never the
//! SDK's remote engines).
//!
//! This module is the pipeline's shared data path, split into two
//! halves: the WIRE half ([`super::transport`] — the
//! target, the POST, the response walk, the ledger read, the
//! wire-error family) is its own module every command dials through,
//! while this module keeps the migration PIPELINE — the directory
//! walk ([`collect`]), the pre-flight, and the apply loop.
//!
//! # The USE guard (engine-probed, live-smoke-caught)
//!
//! The convergence scripts carry `USE NS <ns> DB <db>;` from the
//! SDL's declarations — above each section's transaction frame, since
//! the 2026-10-08 per-scope amendment — and a body `USE` SWITCHES the
//! HTTP session for every later statement whether it sits inside a
//! transaction or not — probed: `USE NS test DB scratch;` mid-body
//! landed the next statement's table in `test/scratch` while the
//! request headers named another namespace/database. Unchecked, a
//! script whose USE disagrees with `--ns`/`--db` would apply THERE
//! while push's skip-SELECT reads HERE — the ledger's self-record and
//! the skip check in two different databases, skip-applied broken.
//! The pre-flight therefore refuses any committed file whose text
//! carries a USE line naming other targets ([`PushError::
//! UseTargetMismatch`]) or one it cannot parse ([`PushError::
//! UseTargetUnreadable`]) — the whole FILE text, the frame's outside
//! included, where the emitter now puts them. GROUP-AWARE since the
//! draft groups (issue #87): every HTTP request is its own session,
//! so a group's EARLIER members may carry their own scope's hoisted
//! `USE` (that is the split's point) without touching the ledger —
//! the mismatch refusal rides only the RECORDING member, whose script
//! writes the row the skip-SELECT reads; unreadable USE lines refuse
//! for every file either way. One upstream quirk the
//! same smoke caught, on record: a hyphenated namespace/database rides
//! the generated USE UNQUOTED (`USE NS surform-probe2 …`), and the
//! engine parses the `-` as subtraction — the apply fails with the
//! engine's own parse error, named per-statement; that is the script
//! renderer's frozen shape, not push's to fix.
//!
//! # The frame guard (probed ground)
//!
//! The rollback report's truth leans on the committed file being
//! EXACTLY one transaction ([`super::trailing_frame_content`] holds
//! the full ground): the engine runs a frame and anything after it
//! INDEPENDENTLY — probed 3.3.0 — so a committed body carrying
//! statements past its `COMMIT;` frame close (a hand-edit's appended
//! tail, or a second `BEGIN`…`COMMIT` block) that fails out there
//! leaves the frame COMMITTED, its self-recording ledger row
//! surviving, while push would report "the failed file's one
//! transaction rolled back — nothing from it is in the database" and
//! the re-run would skip the file as already applied: the tail would
//! NEVER apply. Both spellings refuse pre-flight
//! ([`PushError::TrailingStatements`], the first offending line
//! named, zero requests made) — the same shape `commit` refuses, so
//! the defect cannot enter the ledger's history in the first place.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use super::transport::{Target, TransportError, judge, post_sql, read_ledger};
use super::{Draft, DraftError};
use crate::info::script::ident_text;

/// A migration file push examined, before anything remote happens.
/// Shared with status (the same walk, the same hash re-derivation —
/// one classification substrate).
pub(crate) struct Candidate {
    pub(crate) number: u32,
    pub(crate) name: String,
    /// The report-row key: the file's ledger identity spelled the way
    /// the directory spells it (`0001_add-users`, or a group member's
    /// `0005b_shop` — the letter rides the spelling).
    pub(crate) key: String,
    pub(crate) path: PathBuf,
    /// The parsed draft — the header carries the committed state, the
    /// normalized text is the POST body when the file applies.
    pub(crate) draft: Draft,
    /// The header's recorded hash — `Some` for a committed file, `None`
    /// for a draft (which push skips: commit declares a migration part
    /// of the database's history; push ships committed history).
    pub(crate) committed: Option<String>,
    /// The draft-group position (issue #87) — `Some` for one member of
    /// a multi-scope save's group, `None` for a standalone draft.
    pub(crate) group: Option<super::GroupPos>,
}

impl Candidate {
    /// The drift check's hash: the file's CURRENT filled-script hash —
    /// the exact re-derivation re-commit and status run
    /// ([`super::filled_script`], the fixed point of commit's rewrite).
    pub(crate) fn current_hash(&self) -> String {
        super::sha256_hex(super::filled_script(&self.draft.body).as_bytes())
    }

    /// The frame guard (module doc "The frame guard"): the committed
    /// body must be EXACTLY one transaction — content after the
    /// frame's `COMMIT;` close (an appended tail, or a second
    /// `BEGIN`…`COMMIT` block) refuses pre-flight, the offending line
    /// named, before anything is sent.
    fn check_frame(&self) -> Result<(), PushError> {
        match super::trailing_frame_content(&self.draft.body) {
            None => Ok(()),
            Some((line, text)) => Err(PushError::TrailingStatements {
                path: self.path.clone(),
                line,
                text,
            }),
        }
    }

    /// The USE-target guard (module doc "The USE guard"): every
    /// line-trimmed `USE …` statement in the FILE must name — when it
    /// names — this push's namespace and database. The scan covers the
    /// whole file text, not just the parsed body: the convergence
    /// emitter places each section's `USE` lines ABOVE its transaction
    /// frame, so a body-only scan would miss every one of them (and a
    /// body `USE` switches the session for every later statement all
    /// the same). The scan is line-level (case-insensitive keyword,
    /// whole trimmed line): a string whose content spells a USE line
    /// refuses too, which is the fail-closed direction (a real USE is
    /// indistinguishable from such a string at this level, and the
    /// cost of missing one is the split ledger). The header's `--`
    /// lines can never match — they start with `--`.
    ///
    /// GROUP-AWARE (issue #87): a multi-scope
    /// group's members each carry their OWN scope's hoisted `USE` —
    /// the whole point of the split — so the mismatch refusal rides
    /// only the RECORDING member (`records`; every request is its own
    /// HTTP session, an earlier member's switch never leaves its
    /// file). The recording member's USE decides where its ledger
    /// self-record lands, and the skip-SELECT reads this push's
    /// target — the two must agree, so the recording member keeps the
    /// full rule. Unreadable USE lines refuse for every file either
    /// way (fail-closed has no member discount).
    fn check_use_target(&self, target: &Target, records: bool) -> Result<(), PushError> {
        for line in self.draft.text.lines() {
            let trimmed = line.trim();
            if !trimmed.to_ascii_uppercase().starts_with("USE ") {
                continue;
            }
            let mut use_ns: Option<String> = None;
            let mut use_db: Option<String> = None;
            let mut tokens = trimmed.trim_end_matches(';').split_whitespace().skip(1);
            while let Some(token) = tokens.next() {
                match token.to_ascii_uppercase().as_str() {
                    "NS" => use_ns = tokens.next().map(unquote_ident),
                    "DB" => use_db = tokens.next().map(unquote_ident),
                    _ => {
                        return Err(PushError::UseTargetUnreadable {
                            path: self.path.clone(),
                            line: trimmed.to_string(),
                        });
                    }
                }
            }
            if !records {
                continue; // a non-recording member applies its own scope
            }
            if use_ns.as_deref().is_some_and(|ns| ns != target.ns)
                || use_db.as_deref().is_some_and(|db| db != target.db)
            {
                return Err(PushError::UseTargetMismatch(Box::new(UseMismatch {
                    path: self.path.clone(),
                    line: trimmed.to_string(),
                    use_ns,
                    use_db,
                    target_ns: target.ns.clone(),
                    target_db: target.db.clone(),
                })));
            }
        }
        Ok(())
    }
}

/// A USE statement's identifier: the surrounding backticks stripped
/// (the one quoting the convergence renderer can emit); anything else
/// rides as spelled.
fn unquote_ident(token: &str) -> String {
    token
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix('`'))
        .map(str::to_string)
        .unwrap_or_else(|| token.to_string())
}

/// Everything push refuses BEFORE touching the database (or mid-loop,
/// fail-closed). The CLI prints `error: {e}` and exits 1. No variant
/// carries credential bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushError {
    /// The migrations directory does not exist — a configured pipeline
    /// pointing at nothing (an empty one is a normal no-op, not this).
    DirMissing(PathBuf),
    /// A candidate file or its directory could not be read.
    Unreadable { path: PathBuf, reason: String },
    /// A numbered file is not a surform migration draft/committed file.
    NotAMigration { path: PathBuf, reason: DraftError },
    /// Two files in the directory carry one number — the order would
    /// be ambiguous (commit refuses the same collision).
    DuplicateNumber { number: u32, a: PathBuf, b: PathBuf },
    /// A committed file no longer hashes to its header's recorded
    /// hash — edited after its commit (status's DRIFTED). The run
    /// refuses before ANY file applies: applying a later migration
    /// past a silently-drifted one would compound the drift.
    Drift {
        path: PathBuf,
        recorded: String,
        current: String,
    },
    /// A committed script carries statements OUTSIDE its one
    /// transaction frame — content after the body's `COMMIT;` frame
    /// close, or a second `BEGIN`…`COMMIT` block. The engine runs the
    /// frame and the tail independently (probed 3.3.0): a tail failure
    /// would leave the frame committed — its self-recording ledger row
    /// surviving — while push reported a full rollback, and the re-run
    /// would skip the file as already applied, so the tail would never
    /// apply. Refused pre-flight, the offending line named (`line` is
    /// 1-based within the script body); commit refuses the same shape,
    /// so the defect cannot enter the ledger's history.
    TrailingStatements {
        path: PathBuf,
        line: usize,
        text: String,
    },
    /// The wire layer refused — the endpoint spelling, a header, the
    /// HTTP answers, the connection, a schema/ledger read, or a
    /// response shape ([`TransportError`]; the pipeline's own text
    /// delegates to it verbatim).
    Wire(TransportError),
    /// The ledger already records this number with a DIFFERENT hash —
    /// another migration (another directory, per the one-directory-
    /// per-database rule) owns the number on this database.
    LedgerNumberTaken {
        number: u32,
        file: PathBuf,
        ledger_hash: String,
        file_hash: String,
    },
    /// The script carries a `USE` statement targeting a different
    /// namespace/database than the connection headers. Engine-probed
    /// 3.3.0: a body `USE` SWITCHES the session for every later
    /// statement — the migration (and its ledger self-record) would
    /// land THERE while push's skip-SELECT reads HERE, splitting the
    /// ledger across two databases and breaking skip-applied. The
    /// convergence scripts carry `USE NS … DB …;` from the SDL's
    /// declarations — point `--ns`/`--db` at the same names. The
    /// fields ride a boxed payload: six of them would blow the enum
    /// past clippy's `result_large_err` threshold for every function
    /// returning this error.
    UseTargetMismatch(Box<UseMismatch>),
    /// A `USE` line push cannot parse — refused, never guessed (a
    /// string whose content spells a USE line lands here too: the
    /// line-level scan cannot tell, and it refuses).
    UseTargetUnreadable { path: PathBuf, line: String },
    /// A structurally broken draft GROUP (issue #87) —
    /// the letters not contiguous from `a`, the file count disagreeing
    /// with the headers' total, a member's name letter disagreeing
    /// with its header. The walk refuses before anything is sent: a
    /// broken group's apply order and its once-per-number record are
    /// both undefined.
    GroupBroken { number: u32, reason: String },
    /// A file's NAME number and its header's number disagree — push
    /// applies under the name's number while the script's INSERT
    /// self-records the header's, splitting the identity (commit's
    /// `FileNameNumberMismatch` guards the same split at
    /// certification; the walk repeats it because renames are
    /// ungated).
    NameNumberMismatch {
        path: PathBuf,
        file_number: u32,
        header_number: u32,
    },
    /// A draft GROUP is only PARTLY committed: some members are still
    /// uncommitted drafts. Push applies a group WHOLE (the members in
    /// name order; the last records the row) — applying a half, the
    /// row could claim a group whose head never shipped. `member`
    /// names the first draft member; `recorded` says whether the
    /// ledger ALREADY carries the group's row (the corrupt posture —
    /// investigate before proceeding) or not (the normal
    /// commit-then-push road).
    GroupIncomplete {
        number: u32,
        member: PathBuf,
        recorded: bool,
    },
}

/// [`PushError::UseTargetMismatch`]'s boxed payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UseMismatch {
    /// The refusing file.
    pub path: PathBuf,
    /// The USE line, verbatim.
    pub line: String,
    /// What the line names (`None` when it leaves the side to the
    /// session headers).
    pub use_ns: Option<String>,
    pub use_db: Option<String>,
    /// What this push reads the ledger from.
    pub target_ns: String,
    pub target_db: String,
}

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PushError::DirMissing(path) => write!(
                f,
                "the migrations directory {} does not exist — generate drafts with \
                 `surform migrate generate --save` first, or pass `--dir`",
                path.display()
            ),
            PushError::Unreadable { path, reason } => {
                write!(f, "the file {} cannot be read: {reason}", path.display())
            }
            PushError::NotAMigration { path, reason } => write!(
                f,
                "the file {} is not a surform migration — {reason}",
                path.display()
            ),
            PushError::DuplicateNumber { number, a, b } => write!(
                f,
                "two drafts carry number {number:04}: {} and {} — the apply order \
                 would be ambiguous; resolve the collision before pushing",
                a.display(),
                b.display()
            ),
            PushError::Drift {
                path,
                recorded,
                current,
            } => write!(
                f,
                "the file {} was edited after its commit (the header records \
                 sha256:{recorded}, the file now hashes sha256:{current}) — status \
                 reports the drift; nothing was applied. Regenerate a fresh draft \
                 instead of pushing a drifted file",
                path.display()
            ),
            PushError::TrailingStatements { path, line, text } => write!(
                f,
                "the file {} carries a statement outside its one transaction frame \
                 — line {line} of the script: `{text}` — and is refused before \
                 anything is sent: the engine runs the frame and the tail \
                 independently, so a tail failure would leave the frame committed \
                 under a false rollback report. Move the statements inside the \
                 BEGIN…COMMIT frame, or commit them as their own migration",
                path.display()
            ),
            PushError::Wire(error) => write!(f, "{error}"),
            PushError::LedgerNumberTaken {
                number,
                file,
                ledger_hash,
                file_hash,
            } => write!(
                f,
                "the ledger already records number {number:04} with hash \
                 sha256:{ledger_hash}, but {} hashes sha256:{file_hash} — another \
                 migration owns this number on this database (one migrations \
                 directory per database); status can compare them",
                file.display()
            ),
            PushError::UseTargetMismatch(mismatch) => write!(
                f,
                "the file {} carries `{}` — the script's own USE would move the \
                 statements (and its ledger self-record) to namespace {}, database \
                 {}, but this push reads the ledger from namespace {}, database {}. \
                 A body USE switches the session (engine-probed 3.3.0); point \
                 --ns/--db at the script's names, or regenerate the draft",
                mismatch.path.display(),
                mismatch.line,
                mismatch.use_ns.as_deref().unwrap_or("(unchanged)"),
                mismatch.use_db.as_deref().unwrap_or("(unchanged)"),
                mismatch.target_ns,
                mismatch.target_db
            ),
            PushError::UseTargetUnreadable { path, line } => write!(
                f,
                "the file {} carries a USE line push cannot read — `{}` — and it is \
                 refused rather than guessed at (a body USE would move the session \
                 away from the ledger)",
                path.display(),
                line
            ),
            PushError::GroupBroken { number, reason } => write!(
                f,
                "the draft group {number:04} is broken — {reason}. The members are \
                 one run under one number (0001a_/0001b_…, applied in name order, \
                 recorded once from the last)"
            ),
            PushError::NameNumberMismatch {
                path,
                file_number,
                header_number,
            } => write!(
                f,
                "the file {} carries number {file_number:04} in its name but \
                 {header_number:04} in its header — push applies under the name's \
                 number while the script's ledger INSERT self-records the header's, \
                 splitting the identity; rename the file to {header_number:04}_… (or \
                 repair the header) before pushing",
                path.display()
            ),
            PushError::GroupIncomplete {
                number,
                member,
                recorded,
            } => write!(
                f,
                "the draft group {number:04} is only partly committed — {} is still \
                 an uncommitted draft. Push applies a group WHOLE (the members in \
                 name order; the last records the ledger row), so commit every \
                 member first{}. The members commit in letter order",
                member.display(),
                if *recorded {
                    " — and the ledger ALREADY records this group, which a \
                     partly-committed group cannot explain; investigate before \
                     pushing anything"
                } else {
                    ""
                }
            ),
        }
    }
}

impl From<TransportError> for PushError {
    fn from(error: TransportError) -> Self {
        PushError::Wire(error)
    }
}

/// How far a push run got. `failure` stops the loop — the engine saw
/// nothing of the failed file (its one transaction rolled back, the
/// probed transaction semantics), and everything after it never ran.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushOutcome {
    /// Applied in order: (number, name).
    pub applied: Vec<(u32, String)>,
    /// The ledger already recorded them (hash equal): skipped.
    pub already: Vec<(u32, String)>,
    /// Uncommitted drafts: skipped (commit declares history; push
    /// ships it).
    pub drafts: Vec<(u32, String)>,
    /// The first failure — the file and the engine's own message.
    pub failure: Option<PushFailure>,
}

/// The first failure's record: which file, the engine's words, the
/// count applied before it (the report's "how far it got").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushFailure {
    pub number: u32,
    pub name: String,
    pub reason: String,
    /// Whether the failed file is a draft-GROUP member (issue #87):
    /// the group's earlier members already applied (their
    /// transactions committed, unrecorded) and the re-run re-applies
    /// them — the failure report names that, so a slot query riding
    /// an earlier member is not silently re-executed.
    pub member_of_group: bool,
}

/// The pre-flight every remote leg shares: the target's validation,
/// the directory walk, the per-file drift/frame/USE checks (their
/// refusals stay HARD errors — nothing is classified past them),
/// then the one ledger SELECT (skipped when nothing is committed — an
/// empty directory never dials). [`push`] applies from it; [`plan`]
/// classifies it without ever POSTing a migration. The candidates
/// stay WHOLE (committed and draft members together, walk order) —
/// the group rules (apply whole, record once) need both halves.
struct Preflight {
    candidates: Vec<Candidate>,
    ledger: BTreeMap<u32, super::transport::LedgerRow>,
    agent: ureq::Agent,
}

fn preflight(dir: &Path, ledger_table: &str, target: &Target) -> Result<Preflight, PushError> {
    target.validate()?;
    let candidates = collect(dir)?;
    // Pre-flight across ALL files before any remote call: the drift
    // check (a committed file must still hash to its header's recorded
    // hash) fails the run closed — nothing applies past a drifted file.
    for candidate in &candidates {
        if candidate.committed.is_none() {
            continue; // drafts: nothing local to certify yet
        }
        let hash = candidate.committed.clone().expect("checked above");
        let current = candidate.current_hash();
        if current != hash {
            return Err(PushError::Drift {
                path: candidate.path.clone(),
                recorded: hash,
                current,
            });
        }
        candidate.check_frame()?;
        // The USE rule is group-aware: a group's
        // earlier members carry their OWN scope's hoisted USE — only
        // the recording member's session must match the target the
        // ledger row lands in.
        let records = candidate.group.is_none_or(|group| group.is_last());
        candidate.check_use_target(target, records)?;
    }
    let agent = super::transport::agent();
    // Skip-applied's read: the ledger's (number, hash) map. A missing
    // table is the fresh-database case — an empty ledger. Nothing
    // committed means nothing to skip-check: no request is made at
    // all (an empty directory never dials).
    let ledger = if candidates
        .iter()
        .all(|candidate| candidate.committed.is_none())
    {
        BTreeMap::new()
    } else {
        let select = format!(
            "SELECT number, name, hash FROM {};",
            ident_text(ledger_table)
        );
        read_ledger(post_sql(&agent, target, &select)?)?
    };
    Ok(Preflight {
        candidates,
        ledger,
        agent,
    })
}

/// The push run: number order, skip applied, stop on the first
/// failure. Local pre-flight first (every file read, parsed, and
/// hash-checked — a refusal here means NOTHING was sent), then one
/// ledger SELECT, then one POST per pending file — a draft GROUP
/// (issue #87) applying whole: every member in name order, the row
/// recorded once by the last member's own script, and an applied
/// group skipping every member.
pub fn push(dir: &Path, ledger_table: &str, target: &Target) -> Result<PushOutcome, PushError> {
    let Preflight {
        candidates,
        ledger,
        agent,
    } = preflight(dir, ledger_table, target)?;
    let mut outcome = PushOutcome::default();
    let mut drafts: Vec<(u32, String)> = Vec::new();
    'walk: for run in candidates.chunk_by(|a, b| a.number == b.number) {
        let number = run[0].number;
        if run[0].group.is_none() {
            // The standalone shape: one file, one number, one row. A
            // standalone DRAFT skips (commit declares history; push
            // ships it).
            let candidate = &run[0];
            let Some(hash) = candidate.committed.clone() else {
                drafts.push((candidate.number, candidate.key.clone()));
                continue;
            };
            match ledger.get(&number) {
                Some(row) if row.hash == hash => {
                    outcome
                        .already
                        .push((candidate.number, candidate.key.clone()));
                }
                Some(row) => {
                    return Err(PushError::LedgerNumberTaken {
                        number,
                        file: candidate.path.clone(),
                        ledger_hash: row.hash.clone(),
                        file_hash: hash,
                    });
                }
                None => match judge(post_sql(&agent, target, &candidate.draft.text)?) {
                    Ok(()) => outcome
                        .applied
                        .push((candidate.number, candidate.key.clone())),
                    Err(reason) => {
                        outcome.failure = Some(PushFailure {
                            number,
                            name: candidate.key.clone(),
                            reason,
                            member_of_group: false,
                        });
                        break 'walk;
                    }
                },
            }
            continue;
        }
        // The group shape (issue #87): apply WHOLE. A group with ANY
        // committed member and ANY draft member blocks the run (the
        // row could otherwise claim a group whose head never shipped)
        // — but a WHOLLY-draft group is the pipeline's default state
        // right after the save: nothing would apply, the block's
        // rationale cannot arise, and the members take the standalone
        // drafts' benign skip verdict.
        let draft_members: Vec<&Candidate> = run
            .iter()
            .filter(|candidate| candidate.committed.is_none())
            .collect();
        for member in &draft_members {
            drafts.push((member.number, member.key.clone()));
        }
        if draft_members.len() == run.len() {
            continue; // wholly draft: skip the group like any draft
        }
        if let Some(member) = draft_members.first() {
            return Err(PushError::GroupIncomplete {
                number,
                member: member.path.clone(),
                recorded: ledger.contains_key(&number),
            });
        }
        let last = run.last().expect("a run is never empty");
        let last_hash = last
            .committed
            .clone()
            .expect("the group is wholly committed");
        match ledger.get(&number) {
            // Applied: the row is the GROUP's (the once-per-number
            // contract) — every member skips.
            Some(row) if row.hash == last_hash => {
                for candidate in run {
                    outcome
                        .already
                        .push((candidate.number, candidate.key.clone()));
                }
            }
            Some(row) => {
                return Err(PushError::LedgerNumberTaken {
                    number,
                    file: last.path.clone(),
                    ledger_hash: row.hash.clone(),
                    file_hash: last_hash,
                });
            }
            None => {
                for candidate in run {
                    match judge(post_sql(&agent, target, &candidate.draft.text)?) {
                        Ok(()) => outcome
                            .applied
                            .push((candidate.number, candidate.key.clone())),
                        Err(reason) => {
                            // A mid-group failure leaves the earlier
                            // members applied but unrecorded — no row
                            // exists until the LAST member's script
                            // self-records — so the re-run re-applies
                            // them (OVERWRITE makes it idempotent) and
                            // resumes.
                            outcome.failure = Some(PushFailure {
                                number: candidate.number,
                                name: candidate.key.clone(),
                                reason,
                                member_of_group: true,
                            });
                            break 'walk;
                        }
                    }
                }
            }
        }
    }
    outcome.drafts = drafts;
    Ok(outcome)
}

/// A would-refuse row of the dry-run plan: the ledger already
/// records the number with a DIFFERENT hash — a real push refuses
/// the run right there ([`PushError::LedgerNumberTaken`]); the plan
/// names the collision and carries on classifying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanRefusal {
    pub number: u32,
    pub name: String,
    pub ledger_hash: String,
    pub file_hash: String,
}

/// The dry-run plan (issue #77): the ENTIRE pre-flight — the
/// directory walk, the drift/frame/USE checks, the one ledger SELECT
/// — and the classification of every committed file with ZERO
/// migration POSTs. The verdicts are the apply loop's own:
/// already-applied (ledger hit, hash equal), would-apply (pending),
/// skip-draft, would-refuse (the ledger-number collision a real push
/// dies on — exit 1 material for the caller), and blocked (a draft
/// group only partly committed — a real push refuses the run; the
/// row carries the reason, exit 1 material like the refusals).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushPlan {
    pub would_apply: Vec<(u32, String)>,
    pub already: Vec<(u32, String)>,
    pub drafts: Vec<(u32, String)>,
    pub would_refuse: Vec<PlanRefusal>,
    /// (number, key, reason) — the partly-committed groups.
    pub blocked: Vec<(u32, String, String)>,
}

/// Plans a push without sending anything past the ledger SELECT —
/// the same pre-flight [`push`] runs, classified instead of applied.
pub fn plan(dir: &Path, ledger_table: &str, target: &Target) -> Result<PushPlan, PushError> {
    let Preflight {
        candidates, ledger, ..
    } = preflight(dir, ledger_table, target)?;
    let mut plan = PushPlan::default();
    for run in candidates.chunk_by(|a, b| a.number == b.number) {
        let number = run[0].number;
        if run[0].group.is_none() {
            let candidate = &run[0];
            let Some(hash) = candidate.committed.clone() else {
                plan.drafts.push((candidate.number, candidate.key.clone()));
                continue;
            };
            match ledger.get(&number) {
                None => plan
                    .would_apply
                    .push((candidate.number, candidate.key.clone())),
                Some(row) if row.hash == hash => {
                    plan.already.push((candidate.number, candidate.key.clone()))
                }
                Some(row) => plan.would_refuse.push(PlanRefusal {
                    number,
                    name: candidate.key.clone(),
                    ledger_hash: row.hash.clone(),
                    file_hash: hash,
                }),
            }
            continue;
        }
        let draft_members: Vec<&Candidate> = run
            .iter()
            .filter(|candidate| candidate.committed.is_none())
            .collect();
        for member in &draft_members {
            plan.drafts.push((member.number, member.key.clone()));
        }
        if draft_members.len() == run.len() {
            continue; // wholly draft: skip the group like any draft
        }
        if let Some(member) = draft_members.first() {
            let recorded = ledger.contains_key(&number);
            // The row's key is the first COMMITTED member (the part
            // that would apply but is blocked) — the head-draft shape
            // would otherwise double-report the draft.
            let blocked_key = run
                .iter()
                .find(|candidate| candidate.committed.is_some())
                .map(|candidate| candidate.key.clone())
                .unwrap_or_else(|| run[0].key.clone());
            plan.blocked.push((
                number,
                blocked_key,
                format!(
                    "the group is only partly committed — commit {} first{}",
                    member.key,
                    if recorded {
                        " (and the ledger already records this group — investigate)"
                    } else {
                        ""
                    }
                ),
            ));
            continue;
        }
        let last = run.last().expect("a run is never empty");
        let last_hash = last
            .committed
            .clone()
            .expect("the group is wholly committed");
        match ledger.get(&number) {
            None => {
                for candidate in run {
                    plan.would_apply
                        .push((candidate.number, candidate.key.clone()));
                }
            }
            Some(row) if row.hash == last_hash => {
                for candidate in run {
                    plan.already.push((candidate.number, candidate.key.clone()));
                }
            }
            Some(row) => plan.would_refuse.push(PlanRefusal {
                number,
                name: last.key.clone(),
                ledger_hash: row.hash.clone(),
                file_hash: last_hash,
            }),
        }
    }
    Ok(plan)
}

/// The directory's migration files in walk order: every
/// `NNNN[<letter>]_*.surql` name joins, everything else is invisible,
/// each file parses (a numbered file that is not
/// a migration refuses — it claims to be one), and the number-runs
/// validate (issue #87): a number carries either ONE standalone file
/// or a COMPLETE letter run `a..` — one file per letter, contiguous
/// from `a`, the file count equal to the headers' total, every
/// member's name letter and number agreeing with its header. Anything
/// else — a twin key, a standalone mixed with members, a letter gap,
/// a short run — refuses before anything remote happens. Shared with
/// status — the classification substrate is ONE walk.
pub(crate) fn collect(dir: &Path) -> Result<Vec<Candidate>, PushError> {
    let entries = std::fs::read_dir(dir).map_err(|reason| match reason.kind() {
        std::io::ErrorKind::NotFound => PushError::DirMissing(dir.to_path_buf()),
        _ => PushError::Unreadable {
            path: dir.to_path_buf(),
            reason: reason.to_string(),
        },
    })?;
    let mut numbered: Vec<(super::DraftKey, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(key) = super::draft_key_of(&name.to_string_lossy()) else {
            continue;
        };
        numbered.push((key, entry.path()));
    }
    numbered.sort_by_key(|(key, _)| *key);
    for window in numbered.windows(2) {
        if window[0].0 == window[1].0 {
            return Err(PushError::DuplicateNumber {
                number: window[0].0.number,
                a: window[0].1.clone(),
                b: window[1].1.clone(),
            });
        }
    }
    let candidates: Vec<Candidate> = numbered
        .into_iter()
        .map(|(key, path)| {
            let text = std::fs::read_to_string(&path).map_err(|reason| PushError::Unreadable {
                path: path.clone(),
                reason: reason.to_string(),
            })?;
            let draft = super::parse_draft(&text).map_err(|reason| PushError::NotAMigration {
                path: path.clone(),
                reason,
            })?;
            let group = draft.header.group;
            // The name's letter and the header's `-- group:` line must
            // AGREE — a lettered name over a headerless draft would
            // otherwise count as a standalone in the run's shape check
            // below while sorting past one (the shape the walk reads
            // split down the middle: members visited, or files
            // silently skipped), and a renamed member whose letter
            // VALUE disagrees with its header would apply in name
            // order while rendering the header's key. The walk-level
            // mirror of commit's `FileNameLetterMismatch`.
            if key.letter != group.map(|group| group.letter) {
                let reason = format!(
                    "the file's name carries {} but its header carries {} — \
                     rename the file and the header together (a group member's \
                     letter rides BOTH; `-- group:` lines come from \
                     `generate --save` on a multi-scope schema)",
                    key.letter
                        .map_or("no letter".to_string(), |l| format!("letter {l}")),
                    group.map_or("no `-- group:` line".to_string(), |g| {
                        format!("letter {}", g.letter)
                    }),
                );
                return Err(PushError::GroupBroken {
                    number: key.number,
                    reason,
                });
            }
            // The walk-level mirror of commit's FileNameNumberMismatch:
            // a file whose NAME number and header number disagree
            // applies under the name's number while its INSERT
            // self-records the header's — the identity split, refused
            // at the walk (renames are ungated; members and standalones
            // alike).
            if key.number != draft.header.number {
                return Err(PushError::NameNumberMismatch {
                    path: path.clone(),
                    file_number: key.number,
                    header_number: draft.header.number,
                });
            }
            Ok(Candidate {
                number: key.number,
                key: match group {
                    Some(group) => {
                        format!(
                            "{:04}{letter}_{}",
                            key.number,
                            draft.header.name,
                            letter = group.letter
                        )
                    }
                    None => format!("{:04}_{}", key.number, draft.header.name),
                },
                name: draft.header.name.clone(),
                path,
                committed: draft
                    .header
                    .committed
                    .clone()
                    .map(|committed| committed.hash),
                draft,
                group,
            })
        })
        .collect::<Result<_, PushError>>()?;
    for run in candidates.chunk_by(|a, b| a.number == b.number) {
        validate_run(run)?;
    }
    Ok(candidates)
}

/// One number-run's shape check (the doc on [`collect`]). The run is
/// key-sorted, so a wholly-lettered run's NAME letters already ascend
/// from `a` — checking each member's header letter against the
/// position enforces BOTH contiguity and the name↔header agreement.
fn validate_run(run: &[Candidate]) -> Result<(), PushError> {
    let number = run[0].number;
    let lettered = run
        .iter()
        .filter(|candidate| candidate.group.is_some())
        .count();
    if lettered == 0 {
        return Ok(()); // one standalone file, one number
    }
    if lettered != run.len() {
        // A standalone sharing the number with members: two
        // identities on one number — the ledger could never tell
        // them apart.
        return Err(PushError::DuplicateNumber {
            number,
            a: run[0].path.clone(),
            b: run[run.len() - 1].path.clone(),
        });
    }
    let total = match run[0].group {
        Some(group) => group.total,
        None => unreachable!("the run is lettered"),
    };
    let letters: String = run
        .iter()
        .filter_map(|candidate| candidate.group.map(|group| group.letter))
        .collect();
    for (at, candidate) in run.iter().enumerate() {
        let expected = char::from(b'a' + at as u8);
        let Some(group) = candidate.group else {
            unreachable!("the run is lettered");
        };
        if group.letter != expected {
            let reason = format!(
                "the members' letters spell `{letters}` — the run must be \
                 contiguous from `a` (one file per letter; `{expected}` is missing \
                 or out of place)"
            );
            return Err(PushError::GroupBroken { number, reason });
        }
        if group.total != total {
            let reason = format!(
                "the members disagree on the group's size — some spell {total}, \
                 this one spells {}",
                group.total
            );
            return Err(PushError::GroupBroken { number, reason });
        }
    }
    if run.len() as u32 != total {
        let reason = format!(
            "the headers spell {total} members, the directory holds {} — a member \
             file is missing or was renamed off the convention",
            run.len()
        );
        return Err(PushError::GroupBroken { number, reason });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::transport::Auth;
    use super::*;

    /// The USE guard's line scan: a matching USE passes, a mismatching
    /// one refuses (partial forms compare only what they name), an
    /// unreadable one refuses, and a non-USE line is inert.
    #[test]
    fn the_use_guard_scans_lines() {
        let target = Target {
            endpoint: "http://h".into(),
            ns: "test".into(),
            db: "scratch".into(),
            auth: Auth::None,
        };
        let body = |use_line: Option<&str>| {
            let mut text = String::from("BEGIN TRANSACTION;\n");
            if let Some(line) = use_line {
                text.push_str(line);
                text.push('\n');
            }
            text.push_str("COMMIT;\n");
            text
        };
        let guard = |text: &str| {
            let draft = crate::migrate::parse_draft(&format!(
                "-- surform migration 0001 — t\n\
                 -- generated: T\n\
                 -- source: t.surdl\n\
                 -- input: info\n\
                 -- status: committed sha256:{}\n\
                 {text}",
                "a".repeat(64)
            ))
            .unwrap();
            Candidate {
                number: 1,
                name: "t".into(),
                key: "0001_t".into(),
                path: std::path::Path::new("/t").to_path_buf(),
                draft,
                committed: Some("a".repeat(64)),
                group: None,
            }
            .check_use_target(&target, true)
        };
        // The generated spelling, matching the target.
        assert_eq!(guard(&body(Some("USE NS test DB scratch;"))), Ok(()));
        assert_eq!(guard(&body(Some("use ns test db scratch;"))), Ok(()));
        assert_eq!(guard(&body(None)), Ok(()));
        // A backticked spelling unquotes before comparing.
        assert_eq!(guard(&body(Some("USE NS `test` DB `scratch`;"))), Ok(()));
        // Mismatches, whole and partial.
        let mismatch = guard(&body(Some("USE NS test DB other;"))).unwrap_err();
        assert!(matches!(mismatch, PushError::UseTargetMismatch(ref boxed)
                if boxed.use_db.as_deref() == Some("other")));
        assert!(guard(&body(Some("USE NS other;"))).is_err());
        assert!(guard(&body(Some("USE DB other;"))).is_err());
        // An unreadable token refuses; so does a string line that
        // spells a USE (the fail-closed direction).
        assert!(matches!(
            guard(&body(Some("USE NS test DB scratch WIDTH 9;"))),
            Err(PushError::UseTargetUnreadable { .. })
        ));
        assert!(matches!(
            guard(&body(Some("USE NS test DB scratch; extra;"))),
            Err(PushError::UseTargetUnreadable { .. })
        ));
        // A line that does not START with USE is inert — even one
        // carrying a quoted USE spelling (the string's first byte
        // protects it; a string line STARTING with `USE …` is
        // indistinguishable from a real USE and refuses above).
        assert_eq!(guard(&body(Some("'USE NS other DB other;"))), Ok(()));
    }
}
