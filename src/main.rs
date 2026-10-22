//! `surform` — the Surform CLI: SurrealDB schemas, declared.
//!
//! `check` parses and reports every diagnostic; `build` additionally
//! emits the DDL when the file is clean. Diagnostics render through
//! miette: graphical (fancy theme on a terminal, plain when piped so
//! output stays stable) or, with `--json`, one JSON object per
//! diagnostic — both to stderr; DDL goes to stdout.

use clap::{CommandFactory, Parser, Subcommand};
use std::collections::HashSet;
use std::io::IsTerminal;
use surform::ast::{SdlFile, Stmt};
use surform::info::normalize::{
    ReadModel, merge, merge_with_provenance, parse_info_spec, validate_scope_tag,
};
use surform::info::read::read_info_object;
use surform::info::write::{DEFAULT_MAX_WIDTH, write_canonical_with};
use surform::load::{self, Loaded};
use surform::writeback;
use surform::{codegen, diag::Diag, expand, lint, parser};

#[derive(Parser)]
#[command(
    name = "surform",
    version,
    about = "Declarative SurrealDB schemas — SDL to SurrealQL DDL and back"
)]
struct Cli {
    /// Render diagnostics as JSON (one object per line) instead of the
    /// graphical format
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse an SDL file and report diagnostics
    Check {
        /// Path to the .surdl file
        file: String,
    },
    /// Compile an SDL file to SurrealQL DDL
    Build {
        /// Path to the .surdl file
        file: String,
    },
    /// Write an SDL file's canonical form
    ///
    /// Plain `//` comments are layout, not definition: they re-emit
    /// at their anchors (above the statement or block child they
    /// preceded; anchorless lines trail inside their block) while
    /// never changing what a file means, and `///` doc comments are
    /// preserved as before. Comments inside captured SurrealQL bodies
    /// (fn bodies, `then:` blocks, views) are not SDL structure and
    /// drop. With no file (or `-`), the source reads from stdin and
    /// the canonical form prints to stdout — the pipe editors format
    /// an unsaved buffer through; `--in-place` needs a real file to
    /// rewrite.
    ///
    /// `--check` compares instead of printing: exit 0, silent, when
    /// the input is already canonical; exit 1 with a diff-style
    /// rendering of what would change when it is not — the CI gate
    /// without the `--in-place` plus `git diff` dance. A generated
    /// migration script (the `-- surform migration` draft header) is
    /// refused in every mode: `--` is SurrealQL comment syntax, not
    /// SDL's, and fmt does not reflow migration scripts — the schema
    /// to format is the header's `-- source:` file.
    Fmt {
        /// Path to the .surdl file (omit, or `-`, to read stdin)
        file: Option<String>,
        /// Check instead of print: exit 0 silent when the input is
        /// already canonical, exit 1 with a diff of what would change
        #[arg(long, conflicts_with = "in_place")]
        check: bool,
        /// Rewrite in place instead of printing — a composition root
        /// writes the whole tree (every participating file formats
        /// from its own loader parse)
        #[arg(long, requires = "file")]
        in_place: bool,
        /// The layout budget the formatted output's expression values
        /// and kind unions break under, in columns (the official
        /// formatter's `--max-line-length`)
        #[arg(long, value_name = "N", default_value_t = DEFAULT_MAX_WIDTH)]
        max_width: usize,
    },
    /// Merge rendered INFO interchange into an SDL file and re-emit
    /// the canonical SDL
    ///
    /// Each `--info` scope merges into the file
    /// (adopt/elide/preserve/fold), placement orders the result, and
    /// the writer prints it. Scope tags: `root=<path>`,
    /// `ns=<name>=<path>`, `db=<ns>/<name>=<path>` — repeat `--info`
    /// per scope; missing scopes simply don't merge, and at least one
    /// tag is required (a scope-less run has nothing to adopt and
    /// would silently be a disguised `fmt`).
    ///
    /// A composition root normalizes as a project: the merge runs
    /// over the flattened document and `--in-place` returns every
    /// merged statement to the file that authored it (an adoptee with
    /// no writable position refuses — SDL-E0013); without
    /// `--in-place` the flattened canonical form prints.
    ///
    /// The db interchange file carries each table's INFO FOR TABLE
    /// child map in a reserved `"@tables"` sibling key. Until
    /// `surform` grows a pull command, a trivial shell loop assembles
    /// it.
    Normalize {
        /// Path to the .surdl file
        file: String,
        /// Interchange scope: `root=<path>`, `ns=<name>=<path>`, or
        /// `db=<ns>/<name>=<path>` — repeat `--info` per scope;
        /// required (a scope-less run has nothing to adopt and would
        /// silently be a disguised `fmt`)
        #[arg(long = "info", value_name = "SPEC", required = true)]
        info: Vec<String>,
        /// Rewrite the file in place instead of printing to stdout
        #[arg(long)]
        in_place: bool,
        /// The layout budget the re-emitted file's expression values
        /// and kind unions break under, in columns
        #[arg(long, value_name = "N", default_value_t = DEFAULT_MAX_WIDTH)]
        max_width: usize,
    },
    /// Schema migrations — generate, commit, push, status, revert
    ///
    /// `generate` prints the drift report to stdout, followed by the
    /// full convergence script — defines (SDL-only and drifted
    /// definitions) as `DEFINE … OVERWRITE` from the authored SDL
    /// side in the compiler's emission order, then the database-only
    /// removals (`IF EXISTS`, children before parents, the
    /// data-destroying classes carrying `SURFORM WARNING` anchors).
    /// One transaction per scope section, each section's `USE`
    /// outside its frame (a failed statement rolls its own section
    /// back — sections before it stay applied). `--save` writes a
    /// draft only when the script is ONE transaction (single-scope;
    /// the file format wraps exactly one) — a multi-scope script
    /// applies from the printed output. Exit 1 when drift exists in
    /// either direction and nothing was saved (SDL-only definitions
    /// count — they are apply-pending; a successful `--save` exits 0
    /// — the draft is the artifact); expected-unmanaged findings are
    /// informational and never trip the exit.
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
    /// Lint an SDL file — advisory findings, exit 1 when any exist
    ///
    /// The findings read the resolved state: the merge's placed
    /// output when `--info` scopes are given, the file itself
    /// otherwise. Findings never affect `check`/`build`; exit 1 when
    /// any exist (the clippy convention).
    Lint {
        /// Path to the .surdl file
        file: String,
        /// Interchange scope: `root=<path>`, `ns=<name>=<path>`, or
        /// `db=<ns>/<name>=<path>` — repeat `--info` per scope
        #[arg(long = "info", value_name = "SPEC")]
        info: Vec<String>,
    },
}

/// The three input modes' members, as ONE required group:
/// a run naming NO member is a clap usage error, before anything
/// loads. `requires` alone cannot do this — it is one-directional
/// (it fires only when its arg is PRESENT), so the all-absent
/// invocation used to pass clap and reach the migrator unarmed.
/// The pairing stays on the members: each flag requires its partner,
/// the version-pair side conflicts with the other modes' sides, and
/// the positional file's mode reads off its company — bare it is the
/// baseline (the SDL-only TO side), with `--info` tags it is that
/// mode's SDL side — so every partial or mixed spelling is also a
/// usage error.
#[derive(clap::Args)]
#[group(id = "input", required = true, multiple = true)]
struct GenerateInput {
    /// Path to the .surdl file the migration starts FROM - the
    /// `--from`/`--to` mode's older schema version (mutually
    /// exclusive with the other modes)
    #[arg(
        long,
        requires = "to",
        conflicts_with_all = ["file", "info"],
        value_name = "FILE"
    )]
    from: Option<String>,
    /// Path to the .surdl file the migration converges TO - the
    /// `--from`/`--to` mode's newer schema version. The conflicts are
    /// load-bearing on BOTH pair members: clap waives a `requires`
    /// when a conflicting argument is present, so `--to` alone refuses
    /// for its missing `--from`, but a bare file beside `--to` would
    /// waive that requirement - without the conflict the run would
    /// silently ride the baseline mode and drop the flag.
    #[arg(
        long,
        requires = "from",
        conflicts_with_all = ["file", "info"],
        value_name = "FILE"
    )]
    to: Option<String>,
    /// Path to the .surdl file. Alone it is the baseline mode: the
    /// file is the whole desired state and the FROM side is empty, so
    /// every definition is apply-pending - the first-bootstrap
    /// baseline in one command. With `--info` tags it is that mode's
    /// SDL side instead.
    #[arg(value_name = "FILE")]
    file: Option<String>,
    /// Interchange scope: `root=<path>`, `ns=<name>=<path>`, or
    /// `db=<ns>/<name>=<path>` — at least one; the migration
    /// covers exactly the scopes the tags present (the `--info`
    /// mode)
    #[arg(long = "info", value_name = "SPEC", requires = "file")]
    info: Vec<String>,
}

/// The three credential flags, exactly as every live command takes
/// them — ONE home (the #99 unit's reflection: the trio was
/// hand-copied across push/pull/status/revert and the copies had
/// already drifted). Flattened into each command at the same position
/// the flags always sat, so the help order holds and a credential
/// change is one edit.
#[derive(clap::Args)]
struct CredentialArgs {
    /// The database user (falls back to `SURFORM_DB_USER`; give BOTH
    /// user and password, or NEITHER — neither speaks to a server run
    /// with `--unauthenticated`)
    #[arg(long, value_name = "USER")]
    user: Option<String>,
    /// The user's password (falls back to `SURFORM_DB_PASS`; on a
    /// terminal the run ASKS when neither flag nor environment
    /// supplies it — prefer those anyway: a flag lands in shell
    /// history)
    #[arg(long, value_name = "PASS")]
    pass: Option<String>,
    /// The bearer token (falls back to `SURFORM_DB_TOKEN`; EXCLUSIVE
    /// with `--user`/`--pass`) — the caller mints it (one `POST
    /// /signin` with the db-level credential) and the run sends
    /// `Authorization: Bearer <token>`
    #[arg(long, value_name = "TOKEN")]
    token: Option<String>,
}

#[derive(Subcommand)]
enum MigrateCommand {
    /// Generate the convergence migration: the drift report, then the
    /// script (or report only, with `--check`)
    ///
    /// Three input modes, exactly one: the bare SDL file (the
    /// baseline - the file is the whole desired state and the FROM
    /// side is empty, so every definition is apply-pending), the SDL
    /// file against rendered INFO interchange (the file plus `--info`
    /// tags), or schema version against schema version (`--from` and
    /// `--to` - the migration converges a database at the from state
    /// to the to state). A run naming neither mode - or an incomplete
    /// mix - is a usage error.
    Generate {
        /// The three input modes' members (see `GenerateInput`)
        #[command(flatten)]
        input: GenerateInput,
        /// Report only: print the drift report and exit 1 on drift —
        /// no script (the CI gate)
        #[arg(long)]
        check: bool,
        /// Save the migration as a numbered, headered DRAFT file
        /// under `--dir` instead of printing the script: the drift
        /// report still prints, the script goes to the file with its
        /// unfilled REQUIRED slots delimited by git-style resolution
        /// markers (bare text — the draft is deliberately un-runnable
        /// until you replace each region with your query). A
        /// successful save exits 0 — the draft is the artifact;
        /// `--check` (report only) keeps the drift exit.
        #[arg(long, conflicts_with = "check")]
        save: bool,
        /// The drafts directory (`--save`); created when missing
        #[arg(long, value_name = "DIR", default_value = surform::migrate::DEFAULT_DRAFTS_DIR, requires = "save")]
        dir: std::path::PathBuf,
        /// The migration's name (the file's `<name>`, the ledger's);
        /// default: the SDL file's stem, slug-folded. (`--save`)
        #[arg(long, value_name = "NAME", requires = "save")]
        name: Option<String>,
        /// Take over a draft's number instead of stacking a new one:
        /// the draft `<N>` already on disk must carry THIS exact
        /// script (its hash must match the regeneration) — then it is
        /// removed and the new draft saves under its number. Refuses
        /// a missing, committed, or differently-scripted target — a
        /// plain `--save` is the escape hatch. (`--save`)
        #[arg(long, value_name = "N", requires = "save")]
        replace: Option<u32>,
    },
    /// Commit a saved, filled migration draft into the ledger
    ///
    /// Refuses while any `SURFORM SLOT` resolution marker remains
    /// (each unresolved slot named), hashes the filled script
    /// (SHA-256), and rewrites the file — the header marked
    /// committed (hash + message) and the migration ledger's
    /// statements appended INSIDE the script's transaction, so
    /// applying the file through any SurrealDB tooling records
    /// itself. The ledger's `sdl` column re-loads the draft's
    /// `-- source:` file (it must be present and clean) and stores
    /// its canonical form — the revert substrate. Commit is once per
    /// file: a committed file refuses (hash unchanged — already
    /// committed; hash changed — edited after commit, the ledger
    /// keeps the recorded hash; regenerate instead).
    Commit {
        /// The draft file to commit
        file: String,
        /// The commit message — the ledger record's `message`
        #[arg(short, long, value_name = "TEXT")]
        message: String,
        /// The ledger table the statements name (both the `DEFINE
        /// TABLE` and the `INSERT`)
        #[arg(
            long,
            value_name = "TABLE",
            default_value = surform::migrate::DEFAULT_LEDGER_TABLE,
            value_parser = reject_empty_ledger_table
        )]
        ledger_table: String,
    },
    /// Apply committed migrations to a live database over HTTP
    ///
    /// The transport is SurrealDB's raw HTTP `POST /sql` endpoint:
    /// number order, one HTTP call per file (the file's own
    /// `BEGIN TRANSACTION;` … `COMMIT;` wraps it atomically — a failed
    /// statement rolls the whole file back), files the ledger already
    /// records (number + hash match) skip, and the FIRST failure
    /// stops the run with the engine's error and the count applied
    /// before it. Uncommitted drafts are skipped (commit declares
    /// history; push ships it); a committed file edited after its
    /// commit (hash drift) refuses the whole run before anything is
    /// sent. `--dry-run` runs the entire pre-flight and prints the
    /// plan without sending a single migration POST. Credentials ride
    /// `--user`/`--pass`, a bearer token `--token` (the environment
    /// pairs `SURFORM_DB_USER`/`SURFORM_DB_PASS`/`SURFORM_DB_TOKEN` —
    /// the recommended form for secrets; the token is exclusive with
    /// the user/password pair) and never enter any output.
    Push {
        /// The migrations directory
        #[arg(long, value_name = "DIR", default_value = surform::migrate::DEFAULT_DRAFTS_DIR)]
        dir: std::path::PathBuf,
        /// The server ROOT to apply to (`http://host:8000`,
        /// `https://…`) — `/sql` is appended automatically; falls
        /// back to `SURFORM_ENDPOINT`
        #[arg(long, value_name = "URL")]
        endpoint: Option<String>,
        /// The namespace the migrations run in (the `Surreal-NS`
        /// header); falls back to `SURFORM_NS`
        #[arg(long, value_name = "NS")]
        ns: Option<String>,
        /// The database the migrations run in (the `Surreal-DB`
        /// header); falls back to `SURFORM_DB`
        #[arg(long, value_name = "DB")]
        db: Option<String>,
        /// The credentials — `--user`/`--pass` or `--token` (see
        /// [`CredentialArgs`])
        #[command(flatten)]
        credentials: CredentialArgs,
        /// The ledger table the skip-applied SELECT reads
        #[arg(
            long,
            value_name = "TABLE",
            default_value = surform::migrate::DEFAULT_LEDGER_TABLE,
            value_parser = reject_empty_ledger_table
        )]
        ledger_table: String,
        /// Plan the push without applying: the ENTIRE pre-flight (the
        /// directory walk, the drift checks, the one ledger SELECT)
        /// and the plan — would-apply / already-applied / skip-draft /
        /// would-refuse — with ZERO migration POSTs. A would-refuse
        /// row exits 1 (the real push would refuse there); the
        /// endpoint is still required (the ledger read is the point).
        #[arg(long)]
        dry_run: bool,
    },
    /// Fetch the database's rendered INFO as interchange files
    ///
    /// The in-tool producer for the `--info` interchange (issue #78):
    /// `INFO FOR ROOT`, `INFO FOR NS`, `INFO FOR DB`, and one
    /// `INFO FOR TABLE` per table ride the same HTTP `POST /sql`
    /// transport push applies through, and the results land as the
    /// three interchange files the `--info` mode consumes — the db
    /// file carrying every table's child map under the reserved
    /// `"@tables"` key. stdout then prints the ready-made `migrate
    /// generate` command naming them. NOTHING is sent but the INFO
    /// statements, and nothing is written but the three files.
    Pull {
        /// The directory the interchange files land in (created when
        /// missing); the printed command names them here
        #[arg(long, value_name = "DIR", default_value = ".")]
        dir: std::path::PathBuf,
        /// The server ROOT to read from (`http://host:8000`) — `/sql`
        /// is appended automatically; falls back to `SURFORM_ENDPOINT`
        #[arg(long, value_name = "URL")]
        endpoint: Option<String>,
        /// The namespace to read (the `Surreal-NS` header); falls
        /// back to `SURFORM_NS`
        #[arg(long, value_name = "NS")]
        ns: Option<String>,
        /// The database to read (the `Surreal-DB` header); falls back
        /// to `SURFORM_DB`
        #[arg(long, value_name = "DB")]
        db: Option<String>,
        /// The credentials — `--user`/`--pass` or `--token` (see
        /// [`CredentialArgs`])
        #[command(flatten)]
        credentials: CredentialArgs,
    },
    /// Compare the migrations directory against the live migration
    /// ledger
    ///
    /// Per file — applied (the ledger's
    /// hash matches the file's current script), pending (committed,
    /// not yet pushed), draft (no committed header), DRIFTED (the
    /// ledger records the number with a DIFFERENT hash — edited after
    /// the database recorded it, both hashes named); plus ledger rows
    /// the directory has no file for (missing). One ledger SELECT
    /// through the same HTTP `/sql` endpoint push applies through —
    /// status never applies anything. Without an endpoint (flag or
    /// environment) the run is OFFLINE: the local classification only
    /// — draft and committed per file, the ledger-derived states
    /// rendering as unknown — and exit 0 always (nothing can be
    /// DRIFTED or MISSING without a ledger). Exit 0 when directory
    /// and ledger are consistent, 1 when any DRIFTED or MISSING
    /// exists (the CI gate: history integrity, not progress).
    Status {
        /// The migrations directory
        #[arg(long, value_name = "DIR", default_value = surform::migrate::DEFAULT_DRAFTS_DIR)]
        dir: std::path::PathBuf,
        /// The server ROOT to read from (`http://host:8000`) — `/sql`
        /// is appended automatically; falls back to `SURFORM_ENDPOINT`.
        /// Without an endpoint (flag or environment) the run is
        /// OFFLINE: the local classification only — draft and
        /// committed per file, the ledger-derived states rendering as
        /// unknown — and exit 0
        #[arg(long, value_name = "URL")]
        endpoint: Option<String>,
        /// The namespace the migrations run in (the `Surreal-NS`
        /// header); falls back to `SURFORM_NS` (required only when an
        /// endpoint is given)
        #[arg(long, value_name = "NS")]
        ns: Option<String>,
        /// The database the migrations run in (the `Surreal-DB`
        /// header); falls back to `SURFORM_DB` (required only when an
        /// endpoint is given)
        #[arg(long, value_name = "DB")]
        db: Option<String>,
        /// The credentials — `--user`/`--pass` or `--token` (see
        /// [`CredentialArgs`])
        #[command(flatten)]
        credentials: CredentialArgs,
        /// The ledger table the SELECT reads
        #[arg(
            long,
            value_name = "TABLE",
            default_value = surform::migrate::DEFAULT_LEDGER_TABLE,
            value_parser = reject_empty_ledger_table
        )]
        ledger_table: String,
    },
    /// Draft the migration that undoes a recorded one
    ///
    /// A convenience over generate, not a down-chain: the
    /// ledger row named by `--to` (its number or hash prefix) carries
    /// the schema the migration converged to; revert fetches the LIVE
    /// database's schema and drafts the convergence from there to the
    /// recorded state — the same script `migrate generate --from
    /// <live> --to <recorded>` would emit, slots riding wherever the
    /// recorded state orphans data. The draft (plus a `.surdl` sidecar
    /// holding the recorded state verbatim — the draft's `-- source:`,
    /// so a later commit re-records exactly that state) lands in the
    /// migrations directory; NOTHING is applied: fill the slots,
    /// commit, push — the revert travels the same road as any
    /// migration.
    Revert {
        /// The ledger row to revert TO — its number (1–4 digits,
        /// e.g. `3` or `0003`) or its hash's prefix (4–64 hex
        /// characters)
        #[arg(long, value_name = "N|HASH")]
        to: String,
        /// The migrations directory (created when missing)
        #[arg(long, value_name = "DIR", default_value = surform::migrate::DEFAULT_DRAFTS_DIR)]
        dir: std::path::PathBuf,
        /// The server ROOT to read from (`http://host:8000`) — `/sql`
        /// is appended automatically; falls back to `SURFORM_ENDPOINT`
        #[arg(long, value_name = "URL")]
        endpoint: Option<String>,
        /// The namespace the migrations run in (the `Surreal-NS`
        /// header); falls back to `SURFORM_NS`
        #[arg(long, value_name = "NS")]
        ns: Option<String>,
        /// The database the migrations run in (the `Surreal-DB`
        /// header); falls back to `SURFORM_DB`
        #[arg(long, value_name = "DB")]
        db: Option<String>,
        /// The credentials — `--user`/`--pass` or `--token` (see
        /// [`CredentialArgs`])
        #[command(flatten)]
        credentials: CredentialArgs,
        /// The ledger table the SELECT reads
        #[arg(
            long,
            value_name = "TABLE",
            default_value = surform::migrate::DEFAULT_LEDGER_TABLE,
            value_parser = reject_empty_ledger_table
        )]
        ledger_table: String,
    },
    /// Move a draft to another number — the header's number line and
    /// the FILE NAME change together, the one operation that cannot
    /// split the identity commit's mismatch check refuses (push walks
    /// file names; the ledger self-records the header's). Refuses a
    /// COMMITTED migration (the number is the ledger's record id) and
    /// a number another draft already carries. The body rides
    /// untouched — a draft's number lives only in its first header
    /// line.
    Renumber {
        /// The draft file to renumber
        file: std::path::PathBuf,
        /// The new number (1–9999 — the four-digit file convention)
        #[arg(value_name = "N")]
        number: u32,
    },
}

fn main() {
    let cli = Cli::parse();
    if !cli.json {
        // Fancy graphical rendering only on a real terminal; piped
        // output takes the plain theme — no colors, no hyperlink
        // escapes — so tests and logs are stable.
        miette::set_hook(Box::new(|_| {
            if std::io::stderr().is_terminal() {
                Box::new(miette::GraphicalReportHandler::new())
            } else {
                Box::new(
                    miette::GraphicalReportHandler::new_themed(miette::GraphicalTheme::none())
                        .with_links(false),
                )
            }
        }))
        .expect("the report hook installs exactly once");
    }
    match cli.command {
        Command::Check { file } => {
            load_or_exit(&file, cli.json);
        }
        Command::Build { file } => {
            let loaded = load_or_exit(&file, cli.json);
            let ddl = codegen::emit(&loaded.file);
            print!("{ddl}");
        }
        Command::Fmt {
            file,
            check,
            in_place,
            max_width,
        } => {
            let max_width = validated_width(max_width);
            if in_place && file.as_deref() == Some("-") {
                Cli::command()
                    .error(
                        clap::error::ErrorKind::ArgumentConflict,
                        "--in-place cannot rewrite stdin — pass a file path",
                    )
                    .exit();
            }
            if check {
                // The verdict mode: format under the same budget and
                // compare — exit 0, silent, when the input is already
                // canonical; exit 1 with the diff on stderr when it is
                // not (finding-class output; the diff printer notes
                // the ledgered EPIPE corner). `--in-place` cannot ride
                // along (clap names the conflict).
                let (source, name) = read_fmt_source(file.as_deref(), cli.json);
                let ast = fmt_source_or_exit(&source, &name, cli.json);
                let canonical = write_canonical_with(&ast, max_width);
                if canonical == source {
                    return;
                }
                print_check_diff(&name, &source, &canonical);
                std::process::exit(1);
            }
            if in_place {
                // The tree writer (issue #45): a file is rewritten as
                // the TREE it anchors. The load validates every
                // participant first — a dirty composition is never
                // rewritten, and no file changes until every home has
                // formatted clean — then each home formats from its
                // own loader parse and writes back when its
                // canonical form differs. A file without includes
                // loads as a one-home tree: the single-file behavior
                // is the degenerate case, not a separate path.
                let path = file.as_deref().expect("in_place implies a file path");
                // The read rides `read_fmt_source` so the
                // migration-script gate fires BEFORE any parse — the
                // load would otherwise die in the SDL-E1001/E1002 wall
                // the gate replaces.
                let (text, _) = read_fmt_source(Some(path), cli.json);
                let loaded = load_text_or_exit(path, text, cli.json);
                let formatted: Vec<String> = loaded
                    .home_files
                    .iter()
                    .map(|home_file| write_canonical_with(home_file, max_width))
                    .collect();
                for (home, text) in loaded.homes.iter().zip(&formatted) {
                    if home.text == *text {
                        continue;
                    }
                    if let Err(reason) = std::fs::write(&home.path, text) {
                        report(
                            &Diag::FileUnwritable {
                                path: home.display.clone(),
                                reason: reason.to_string(),
                            },
                            None,
                            cli.json,
                        );
                        std::process::exit(1);
                    }
                }
                return;
            }
            // A dirty parse is never formatted: the diagnostics
            // report and the exit come first. No file (or `-`) reads
            // the pipe — the editor contract. Print mode stays
            // per-file: the named file's authored text (a root prints
            // its own body, include statements and all). Read and
            // parse run through the same gated pair `--check` uses.
            let (source, name) = read_fmt_source(file.as_deref(), cli.json);
            let ast = fmt_source_or_exit(&source, &name, cli.json);
            let sdl = write_canonical_with(&ast, max_width);
            print!("{sdl}");
        }
        Command::Normalize {
            file,
            info,
            in_place,
            max_width,
        } => {
            let max_width = validated_width(max_width);
            let source = read_file_or_exit(&file, cli.json);
            // The same parse peeks the includes, and the peek gates
            // the diagnostics: a composition root's spreads resolve
            // through the loader's resolution pass, so its parse-diag
            // report is noise — the composed branch loads first and
            // the loader's diagnostics are the report. A standalone
            // file resolves through the same global pass before
            // merging (a typo'd spread must not normalize silently).
            let (ast, diags) = parser::parse_file_composable(&source);
            let composed = ast
                .items
                .iter()
                .any(|stmt| matches!(stmt, Stmt::Include(_)));
            if composed {
                let loaded = load_or_exit(&file, cli.json);
                let model = ingest_info_model(&info, &loaded.file, cli.json);
                let (merged, provenance) = merge_with_provenance(&loaded.file, &model);
                if in_place {
                    let plan = match writeback::plan(&loaded, &merged, &provenance, max_width) {
                        Ok(plan) => plan,
                        Err(diag) => {
                            report(&diag, Some((&file, &loaded.homes[0].text)), cli.json);
                            std::process::exit(1);
                        }
                    };
                    for (home, text) in &plan {
                        let target = &loaded.homes[*home];
                        if let Err(reason) = std::fs::write(&target.path, text) {
                            report(
                                &Diag::FileUnwritable {
                                    path: target.display.clone(),
                                    reason: reason.to_string(),
                                },
                                None,
                                cli.json,
                            );
                            std::process::exit(1);
                        }
                    }
                } else {
                    // The preview is the merged FLATTENED document —
                    // what the project deploys as.
                    let sdl = write_canonical_with(&merged, max_width);
                    print!("{sdl}");
                }
                return;
            }
            if !diags.is_empty() {
                for diag in &diags {
                    report(diag, Some((&file, &source)), cli.json);
                }
                std::process::exit(1);
            }
            // The standalone file runs the same check-class contract
            // the loader runs ([`order::check_document`]): a spread
            // naming nothing, a duplicate definition, or a define-time
            // cycle must not normalize silently.
            let spans: Vec<surform::lexer::Span> = lint::scan_source(&source)
                .statements
                .iter()
                .map(|s| s.span)
                .collect();
            let diags = surform::order::check_document(&ast.items, &[], &spans);
            for (diag, _) in &diags {
                report(diag, Some((&file, &source)), cli.json);
            }
            if !diags.is_empty() {
                std::process::exit(1);
            }
            // The merge itself is spread-preserving (spec/mixin.md's
            // reverse-path rule): spreads re-attach, a diverged member
            // materializes as the explicit override.
            let model = ingest_info_model(&info, &ast, cli.json);
            let sdl = write_canonical_with(&merge(&ast, &model), max_width);
            if in_place {
                if let Err(reason) = std::fs::write(&file, sdl) {
                    report(
                        &Diag::FileUnwritable {
                            path: file.clone(),
                            reason: reason.to_string(),
                        },
                        None,
                        cli.json,
                    );
                    std::process::exit(1);
                }
            } else {
                print!("{sdl}");
            }
        }
        Command::Migrate {
            command:
                MigrateCommand::Generate {
                    input:
                        GenerateInput {
                            from,
                            to,
                            file,
                            info,
                        },
                    check,
                    save,
                    dir,
                    name,
                    replace,
                },
        } => {
            // A dirty input never migrates: the load reports every
            // diagnostic and the exit comes first — both modes load
            // through the composition (the version pair's spreads
            // resolve through the SAME expansion the authored side
            // runs; a composed root deploys as the flat whole,
            // spec/include.md). The differ expands the SDL side
            // itself (spec/mixin.md): the database only ever holds a
            // spread's members as concrete definitions, so the
            // findings are exactly the drift of those definitions.
            // The version pair lowers the FROM file into the
            // read-side slot (`version_pair_model` — the one
            // comparator, the FROM side playing the interchange's
            // role) and diffs the TO file against it; the
            // `--info` mode ingests the interchange scopes and diffs
            // the file. The script's collapse logic reads the TO
            // file's declarations either way: a scope the TO version
            // dropped retires under the owner REMOVE (WARNING), one
            // it added defines from nothing.
            let (sdl, findings) = if let (Some(from_path), Some(to_path)) = (&from, &to) {
                let from_loaded = load_or_exit(from_path, cli.json);
                let to_loaded = load_or_exit(to_path, cli.json);
                let model = ReadModel::version_pair_model(&from_loaded.file, &to_loaded.file);
                let findings = surform::info::diff::diff(&to_loaded.file, &model);
                (to_loaded.file, findings)
            } else if let Some(file) = &file {
                let loaded = load_or_exit(file, cli.json);
                // No `--info` tags: the bare FILE is the BASELINE mode
                // (issue #101) — the file is the desired state, the
                // FROM side is empty, and every finding is
                // apply-pending; nothing can classify as a removal
                // because the model holds no entries. With tags it is
                // the `--info` mode, unchanged.
                let model = if info.is_empty() {
                    ReadModel::baseline_model(&loaded.file)
                } else {
                    ingest_info_model(&info, &loaded.file, cli.json)
                };
                let findings = surform::info::diff::diff(&loaded.file, &model);
                (loaded.file, findings)
            } else {
                // Unreachable through clap: the `input` group is
                // required (an all-absent run dies there) and every
                // partial spelling trips a member's `requires`. The
                // usage error stands here anyway — a derive
                // regression degrades to a diagnostic, never a panic.
                Cli::command()
                    .error(
                        clap::error::ErrorKind::MissingRequiredArgument,
                        "exactly one input mode is required: the positional file with \
                         --info tags, or --from with --to",
                    )
                    .exit()
            };
            // (the restructured generate arm rides from dx/cli-surface below)
            // Expected-unmanaged is informational: it never trips
            // the exit.
            let tripping = findings.iter().any(|f| {
                !matches!(
                    f.class,
                    surform::info::diff::DriftClass::ExpectedUnmanaged { .. }
                )
            });
            let text = surform::info::report::report(&findings);
            if text.is_empty() {
                // A converged run says so in ONE line — the two
                // banners (the drift header plus the script or draft
                // section) spelled one bit (issue #90.5).
                println!("(converged)");
                return;
            }
            print!("== drift ==\n{text}");
            if check {
                // Report only: the drift exit is the CI signal.
                if tripping {
                    std::process::exit(1);
                }
                return;
            }
            if save {
                // The saved draft: the script with its unfilled
                // REQUIRED slots delimited by resolution markers
                // goes to the numbered file; stdout keeps the
                // report, names the file, and names the next move.
                // A state with nothing to migrate writes nothing
                // (the draft section says so). A successful save is
                // a successful run — the draft is the artifact
                // (issue #71); the drift exit stays with the
                // report-only paths (--check above, and the plain
                // run below).
                let marked = surform::info::script::convergence_script_styled(
                    &findings,
                    &sdl,
                    surform::info::script::SlotStyle::ResolutionMarkers,
                );
                if marked.is_empty() {
                    print!("== draft ==\n(converged)\n");
                } else {
                    let (source, input) = if let Some(to) = &to {
                        (
                            to.as_str(),
                            // The diff's inputs ride the header (issue
                            // #75): both files, as given.
                            surform::migrate::InputMode::Versions {
                                from: from
                                    .clone()
                                    .expect("the input group pairs --from with --to"),
                                to: to.clone(),
                            },
                        )
                    } else {
                        let source = file.as_deref().expect("the input group names a file");
                        // The header records the run's inputs (issue
                        // #75): the baseline spells its file, never
                        // the bare `info` legacy posture (that line
                        // means pre-#75 drafts).
                        let input = if info.is_empty() {
                            surform::migrate::InputMode::File {
                                path: source.to_string(),
                            }
                        } else {
                            surform::migrate::InputMode::Info { tags: info.clone() }
                        };
                        (source, input)
                    };
                    // The source's canonical-form hash (issue #68):
                    // DEFAULT_MAX_WIDTH — commit's own constant, so the
                    // header's hash matches what commit re-derives —
                    // pins the state the draft was generated against,
                    // and commit refuses a source edited in between.
                    let source_sha256 = surform::migrate::sha256_hex(
                        write_canonical_with(&sdl, DEFAULT_MAX_WIDTH).as_bytes(),
                    );
                    let save = surform::migrate::DraftSave {
                        name: name.as_deref(),
                        source,
                        source_sha256: Some(&source_sha256),
                        input,
                        generated: &surform::migrate::now_utc(),
                        marked_script: &marked,
                    };
                    // The frame count routes the save: the file
                    // format's one-transaction invariant holds per
                    // file, so a multi-scope script cannot be one
                    // draft (issue #87's ground).
                    let frames = surform::migrate::frame_count(&marked);
                    // The pile-up note (issue #74): a byte-identical
                    // script already lives in the directory — say so
                    // before the next number stacks again. Informational
                    // (the stale draft may be EXACTLY what a fill-in-
                    // progress edit wants to keep); `--replace` is the
                    // take-over answer. Skipped when replacing — the
                    // replace flow already answers the question — and
                    // for the multi-scope shape (issue #87): a group's
                    // members are not one script, and the note's
                    // take-over does not exist for a group (delete the
                    // stale group by hand).
                    if frames == 1
                        && replace.is_none()
                        && let Some(hash) = surform::migrate::marked_script_hash(&marked)
                        && let Some(existing) = surform::migrate::find_identical_draft(&dir, &hash)
                        && let Some(file) = existing.file_name()
                    {
                        println!(
                            "note: an existing draft carries this exact script: {}",
                            file.to_string_lossy()
                        );
                    }
                    // The router: one frame takes the single-draft
                    // writers (save or replace); two or more take the
                    // GROUP writer — one draft per scope section,
                    // numbered `NNNNa`/`NNNNb`, applied in name order
                    // (issue #87). A multi-frame script under
                    // `--replace` still reaches the single writer,
                    // whose gate refuses it naming the group save; a
                    // zero-frame script (owner defines only) refuses
                    // there too.
                    match replace {
                        Some(number) => {
                            let draft_path = replace_draft_or_exit(&dir, number, save, cli.json);
                            let path = draft_path.display();
                            print!("== draft ==\n{path}\n");
                            println!(
                                "fill the SURFORM SLOT regions, then: surform migrate commit {path}"
                            );
                        }
                        None if frames >= 2 => {
                            let paths = save_draft_group(&dir, save, cli.json);
                            print!("== draft ==");
                            for path in &paths {
                                print!("\n{}", path.display());
                            }
                            println!();
                            for path in &paths {
                                println!(
                                    "fill the SURFORM SLOT regions, then: surform migrate \
                                     commit {}",
                                    path.display()
                                );
                            }
                        }
                        None => {
                            let draft_path = save_draft(&dir, save, cli.json);
                            let path = draft_path.display();
                            print!("== draft ==\n{path}\n");
                            println!(
                                "fill the SURFORM SLOT regions, then: surform migrate commit {path}"
                            );
                        }
                    }
                }
                return;
            }
            let script = surform::info::script::convergence_script(&findings, &sdl);
            print!("== migration script ==");
            if script.is_empty() {
                print!("\n(converged)\n");
            } else {
                print!("\n{script}");
            }
            if tripping {
                std::process::exit(1);
            }
        }
        Command::Migrate {
            command:
                MigrateCommand::Commit {
                    file,
                    message,
                    ledger_table,
                },
        } => {
            // The order is the contract's: parse (identity),
            // the already-committed refusal, the identity check (the
            // file name's number vs the header's — after the
            // committed refusal, whose repair `renumber` refuses for
            // committed files), the marker scan (each unresolved slot
            // named — never a bare refusal), the duplicate-number
            // scan, THEN the source load, the hash, the ledger splice,
            // and the rewrite. `-m` is clap's (a missing message never
            // reaches here).
            let text = read_file_or_exit(&file, cli.json);
            let draft = match surform::migrate::parse_draft(&text) {
                Ok(draft) => draft,
                Err(error) => fail(&error.to_string(), cli.json),
            };
            if let Some(committed) = &draft.header.committed {
                let current = surform::migrate::sha256_hex(
                    surform::migrate::filled_script(&draft.body).as_bytes(),
                );
                if current == committed.hash {
                    fail(
                        &format!(
                            "this migration is already committed (the file still hashes \
                         sha256:{current}) — commit is once per draft; push applies it"
                        ),
                        cli.json,
                    );
                }
                fail(
                    &format!(
                        "this draft was edited after its commit (the file now hashes \
                     sha256:{current}, the header records sha256:{}) — the ledger keeps \
                     the recorded hash and status reports the drift; regenerate a fresh \
                     draft instead",
                        committed.hash
                    ),
                    cli.json,
                );
            }
            let path = std::path::Path::new(&file);
            // A bare filename's parent is the empty string — the
            // draft lives in the working directory, so the sibling and
            // duplicate scans read `.` (the empty path would fail the
            // read_dir and, fail-open, skip the scan).
            let dir = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            // The identity check (issue #67): the file name's number
            // and the header's must agree — push walks names, the
            // ledger self-records the header's, and a commit across
            // the mismatch splits the identity into permanent phantom
            // drift. A foreign name (no `NNNN_` stem) keeps the
            // pre-existing posture: push skips it entirely. AFTER the
            // committed checks: a committed file reports its own
            // state first (the mismatch refusal's `renumber` repair
            // refuses committed files — suggesting it there would be
            // a trap; the plain rename arm always works).
            if let Some(file_number) =
                surform::migrate::file_header_mismatch(file.as_ref(), draft.header.number)
            {
                fail(
                    &surform::migrate::DraftError::FileNameNumberMismatch {
                        file_number,
                        header_number: draft.header.number,
                    }
                    .to_string(),
                    cli.json,
                );
            }
            // The group identity checks (issue #87): a member's file
            // name must carry its header's letter (push walks the NAME
            // order; the header's letter decides which member records
            // the group — a swap applies the recording member first),
            // and the members commit in LETTER order (an earlier
            // sibling still an uncommitted draft refuses — the group
            // applies whole, so committing past an unfilled head would
            // let a push apply a group whose head never shipped).
            if let Some(group) = draft.header.group {
                if let Some(file_letter) = surform::migrate::file_letter_mismatch(path, group) {
                    fail(
                        &surform::migrate::DraftError::FileNameLetterMismatch {
                            number: group.number,
                            file_letter,
                            header_letter: group.letter,
                        }
                        .to_string(),
                        cli.json,
                    );
                }
                match surform::migrate::group_order_conflict(dir, group, path) {
                    Ok(Some(earlier)) => fail(
                        &surform::migrate::DraftError::GroupOrder {
                            number: group.number,
                            letter: group.letter,
                            earlier,
                        }
                        .to_string(),
                        cli.json,
                    ),
                    Ok(None) => {}
                    Err(error) => fail(&error.to_string(), cli.json),
                }
            }
            match surform::migrate::scan_markers(&draft.text) {
                Ok(slots) if slots.is_empty() => {}
                Ok(slots) => {
                    // One message carrying the per-slot lines — fail()
                    // prints it verbatim after `error: `, and the
                    // `--json` branch renders the same refusal as one
                    // JSON object (issue #73).
                    let mut message = format!(
                        "{} unresolved SURFORM SLOT region(s) — replace each \
                         delimited region with your query, then re-run:",
                        slots.len()
                    );
                    for slot in slots {
                        message.push_str(&format!("\n  - {slot}"));
                    }
                    fail(&message, cli.json);
                }
                Err(broken) => {
                    let mut message = String::from(
                        "the draft carries malformed marker line(s) — restore the \
                         `<<<<<<< SURFORM SLOT (…) … >>>>>>> SURFORM SLOT END` shape or \
                         resolve the region:",
                    );
                    for line in broken {
                        message.push_str(&format!("\n  {line}"));
                    }
                    fail(&message, cli.json);
                }
            }
            // The committed shape is ONE transaction frame closing the
            // script — enforced here (after the marker scan: resolve
            // the slots first, then the shape matters) so the defect
            // cannot enter the ledger's history (the engine runs the
            // frame and a tail independently: a tail failure would
            // leave the frame committed, self-recorded, under push's
            // false rollback report; the push-side guard's ground).
            if let Some((line, text)) = surform::migrate::trailing_frame_content(&draft.body) {
                fail(
                    &surform::migrate::DraftError::TrailingStatements { line, text }.to_string(),
                    cli.json,
                );
            }
            // The scan is KEY-based (issue #87): a group member's
            // letter rides the comparison, so its own group's
            // siblings — the same number, other letters — are never a
            // collision; a twin with the same number AND letter shape
            // is.
            match surform::migrate::number_conflict(
                dir,
                draft.header.number,
                draft.header.group.map(|group| group.letter),
                path,
            ) {
                Ok(Some(other)) => fail(
                    &surform::migrate::DraftError::DuplicateNumber {
                        number: draft.header.number,
                        other,
                    }
                    .to_string(),
                    cli.json,
                ),
                Ok(None) => {}
                Err(error) => fail(&error.to_string(), cli.json),
            }
            // The ledger's `sdl` column: the canonical form of the
            // source SDL, re-loaded at commit time (fail-closed —
            // `load_or_exit` reports every diagnostic and exits). A
            // source edited between generate and commit is the
            // state being certified here; regenerating the draft is
            // the repair when that is not what the user means. The
            // path resolves against THIS invocation's working
            // directory (it is recorded as given) — a commit run
            // from elsewhere needs the same root or an absolute
            // `-- source:` (documented in migrate.md). The
            // readability pre-check (issue #69) turns the bare
            // file error into the mechanism: the source supplies
            // the ledger's revert substrate, and the refusal names
            // both ways out.
            let source_path = std::path::Path::new(&draft.header.source);
            if !source_path.is_file() {
                fail(
                    &format!(
                        "the draft's '-- source:' file '{}' cannot be read from this \
                         directory — commit re-loads it to store the ledger's sdl (the \
                         revert substrate); run from the directory you generated from, \
                         or record an absolute path in the draft header",
                        draft.header.source
                    ),
                    cli.json,
                );
            }
            let loaded = load_or_exit(&draft.header.source, cli.json);
            let sdl_text = write_canonical_with(&loaded.file, DEFAULT_MAX_WIDTH);
            // The source-drift check (issue #68): the header pins the
            // source's canonical hash at GENERATE time; commit
            // re-canonicalizes the re-loaded source and refuses a
            // mismatch — the ledger's `sdl` (the revert substrate)
            // would otherwise describe a state this migration never
            // produces. Drafts predating the line carry no hash: the
            // legacy posture, no check.
            if let Some(recorded) = &draft.header.source_sha256 {
                let current = surform::migrate::sha256_hex(sdl_text.as_bytes());
                if current != *recorded {
                    fail(
                        &format!(
                            "the source SDL changed after this draft was generated — the \
                         header records sha256:{recorded}, the re-loaded source now \
                         canonicalizes to sha256:{current}; the ledger's sdl (the \
                         revert substrate) would describe a state this migration \
                         never produces — restore the recorded source (the header's \
                         '-- source:' path) or delete the draft and regenerate"
                        ),
                        cli.json,
                    );
                }
            }
            // The hash covers the FILLED script's normal form (the
            // draft's body, newline-normalized — a draft carries no
            // ledger region yet): what the user certified, not the
            // machinery commit appends. Hashing the normal form —
            // not the raw bytes — makes the recorded hash a fixed
            // point of commit's own rewrite: the splice writes the
            // newline-normalized spelling, so an unchanged file
            // re-derives this hash exactly (already committed), and
            // any edit inside the hashed region flips it (edited
            // after its commit).
            let hash = surform::migrate::sha256_hex(
                surform::migrate::filled_script(&draft.body).as_bytes(),
            );
            // The ledger block rides the LAST member only (issue #87):
            // the group records ONCE — the number, the LAST member's
            // filled-script hash, the base slug — from the member the
            // walk applies last. An earlier member's commit marks its
            // header and certifies its script, but splices no ledger
            // region (its body rides unchanged, and its hash is the
            // same fixed point of that no-op rewrite).
            let group = draft.header.group;
            let records = group.is_none_or(|group| group.is_last());
            // The frame must close whether or not this member records:
            // a group's earlier members skip the splice (the
            // NoCommit ground below lives there), but an unterminated
            // transaction would die at apply time, past push's
            // pre-flight — the refusal belongs at commit.
            if !surform::migrate::has_final_commit(&draft.body) {
                fail(
                    &surform::migrate::DraftError::NoCommit.to_string(),
                    cli.json,
                );
            }
            let new_body = if records {
                let block = surform::migrate::ledger_statements(
                    &ledger_table,
                    draft.header.number,
                    &draft.header.name,
                    &hash,
                    &message,
                    &sdl_text,
                );
                match surform::migrate::splice_before_commit(&draft.body, &block) {
                    Some(new_body) => new_body,
                    // The BEGIN frame stood (parse found it) — the missing
                    // thing is the final `COMMIT;` the splice anchors on,
                    // so the refusal names THAT (this arm once
                    // reused NoTransaction's text and blamed the frame).
                    None => fail(
                        &surform::migrate::DraftError::NoCommit.to_string(),
                        cli.json,
                    ),
                }
            } else {
                draft.body.clone()
            };
            let new_text = surform::migrate::mark_committed(&draft, &hash, &message, &new_body);
            if let Err(reason) = std::fs::write(path, new_text) {
                report(
                    &Diag::FileUnwritable {
                        path: file.clone(),
                        reason: reason.to_string(),
                    },
                    None,
                    cli.json,
                );
                std::process::exit(1);
            }
            println!("committed {}", path.display());
            println!("  hash: sha256:{hash}");
            match group {
                Some(group) if !group.is_last() => println!(
                    "  ledger: — (group member {}/{}: the group records once, \
                     from its last member)",
                    group.letter, group.total
                ),
                _ => println!(
                    "  ledger: {ledger_table} (number {:04})",
                    draft.header.number
                ),
            }
        }
        Command::Migrate {
            command:
                MigrateCommand::Push {
                    dir,
                    endpoint,
                    ns,
                    db,
                    credentials,
                    ledger_table,
                    dry_run,
                },
        } => {
            // The connection resolves here — each coordinate flag over
            // its environment, the credentials likewise (a tty prompt
            // when a terminal asks) — and then lives only inside the
            // push target. No path below prints any of it: the outcome
            // rows carry numbers and names, the error types carry
            // server text, and the target's Display is not printed
            // anywhere.
            let target = match resolve_connection(endpoint, ns, db, &credentials) {
                Ok(target) => target,
                Err(message) => fail(&message, cli.json),
            };
            if dry_run {
                // The plan (issue #77): the pre-flight ran, the ledger
                // was read, NOTHING was applied. The report stays
                // plain stdout (the push shape's inheritance); the
                // exit carries the would-refuse verdict — the CI gate
                // a dry run wants to be.
                let plan = match surform::migrate::push::plan(&dir, &ledger_table, &target) {
                    Ok(plan) => plan,
                    Err(error) => fail(&error.to_string(), cli.json),
                };
                println!("== push (dry run — nothing was sent) ==");
                for (_, key) in &plan.already {
                    push_line(key, "already applied");
                }
                for (_, key) in &plan.would_apply {
                    push_line(key, "would apply");
                }
                for (_, key) in &plan.drafts {
                    push_line(key, "skipped — draft (commit it first)");
                }
                for refusal in &plan.would_refuse {
                    push_line(
                        &refusal.name,
                        &format!(
                            "would refuse — the ledger records sha256:{}, the file \
                             hashes sha256:{}",
                            short_hash(&refusal.ledger_hash),
                            short_hash(&refusal.file_hash)
                        ),
                    );
                }
                for (_, key, reason) in &plan.blocked {
                    push_line(key, &format!("blocked — {reason}"));
                }
                println!();
                let total = plan.already.len()
                    + plan.would_apply.len()
                    + plan.drafts.len()
                    + plan.would_refuse.len()
                    + plan.blocked.len();
                if total == 0 {
                    println!("no migration files in {}", dir.display());
                } else {
                    let mut segments: Vec<String> = Vec::new();
                    if !plan.would_apply.is_empty() {
                        segments.push(format!("{} would apply", plan.would_apply.len()));
                    }
                    if !plan.already.is_empty() {
                        segments.push(format!("{} already applied", plan.already.len()));
                    }
                    if !plan.drafts.is_empty() {
                        segments.push(format!("{} draft(s) skipped", plan.drafts.len()));
                    }
                    if !plan.would_refuse.is_empty() {
                        segments.push(format!("{} would refuse", plan.would_refuse.len()));
                    }
                    if !plan.blocked.is_empty() {
                        segments.push(format!("{} blocked", plan.blocked.len()));
                    }
                    println!(
                        "dry run: {total} migration file(s) — {}; zero statements sent",
                        segments.join(", ")
                    );
                }
                if !plan.would_refuse.is_empty() || !plan.blocked.is_empty() {
                    std::process::exit(1);
                }
                return;
            }
            let outcome = match surform::migrate::push::push(&dir, &ledger_table, &target) {
                Ok(outcome) => outcome,
                Err(error) => fail(&error.to_string(), cli.json),
            };
            // The report is plain stdout (the `--json` inheritance is
            // the same one the draft banner carries — the pipeline
            // never promised JSON end-to-end).
            println!("== push ==");
            for (_, key) in &outcome.already {
                push_line(key, "already applied");
            }
            for (_, key) in &outcome.applied {
                push_line(key, "applied");
            }
            for (_, key) in &outcome.drafts {
                push_line(key, "skipped — draft (commit it first)");
            }
            if let Some(failure) = &outcome.failure {
                push_line(&failure.name, "FAILED");
            }
            let committed = outcome.already.len() + outcome.applied.len();
            if let Some(failure) = &outcome.failure {
                if !outcome.applied.is_empty() || !outcome.already.is_empty() {
                    println!();
                }
                eprintln!("error: {} failed — {}", failure.name, failure.reason);
                eprintln!(
                    "stopped at the first failure: applied {} of {} committed \
                     migration(s) (the failed file included); the failed file's one \
                     transaction rolled back — nothing from it is in the database. \
                     Fix the file and re-run push{}",
                    outcome.applied.len(),
                    committed + 1,
                    // The group resume contract, named where the
                    // operator reads it — true whether this was the
                    // group's head (nothing applied yet) or a later
                    // member: the members BEFORE the failed one re-apply
                    // on the re-run.
                    if failure.member_of_group {
                        ". This file is a draft group's member — the group \
                         applies whole, so the members before it (any that \
                         applied) RE-APPLY on the re-run (their defines are \
                         idempotent; a data-migration query you filled into \
                         one of them runs again)"
                    } else {
                        ""
                    }
                );
                std::process::exit(1);
            }
            println!();
            if outcome.applied.is_empty() {
                if committed == 0 && outcome.drafts.is_empty() {
                    println!("no migration files in {}", dir.display());
                } else {
                    println!("nothing to apply — every committed migration is applied");
                }
            } else {
                // "Migration" here is the LEDGER's unit: a draft
                // group's members are one migration, recorded once
                // (from the last member's transaction) — the count is
                // files applied, the record is per migration.
                println!(
                    "applied {} migration file(s) — the ledger recorded every \
                     migration inside its own transaction",
                    outcome.applied.len()
                );
            }
        }
        Command::Migrate {
            command:
                MigrateCommand::Pull {
                    dir,
                    endpoint,
                    ns,
                    db,
                    credentials,
                },
        } => {
            // The same connection discipline as push (issue #76's one
            // resolution, the same redaction guarantees — one data
            // path). The report is plain stdout (the pipeline's
            // inheritance); the printed command is the point.
            let target = match resolve_connection(endpoint, ns, db, &credentials) {
                Ok(target) => target,
                Err(message) => fail(&message, cli.json),
            };
            let outcome = match surform::migrate::pull::pull(&dir, &target) {
                Ok(outcome) => outcome,
                Err(error) => fail(&error.to_string(), cli.json),
            };
            println!("== pull ==");
            // The fetched scopes' rows only — a scope the credential
            // was refused (issue #103) wrote no file; the advice
            // block below names the refusals.
            if let Some(root) = &outcome.root {
                push_line(&root.display().to_string(), "written (INFO FOR ROOT)");
            }
            if let Some(ns) = &outcome.ns {
                push_line(
                    &ns.display().to_string(),
                    &format!("written (INFO FOR NS {})", target.ns),
                );
            }
            push_line(
                &outcome.db.display().to_string(),
                &format!(
                    "written (INFO FOR DB {}/{}, {} table(s))",
                    target.ns, target.db, outcome.tables
                ),
            );
            println!();
            println!("next:");
            println!("  {}", outcome.command);
            // The wider-compares advice (issue #100): the root and
            // namespace tags as deliberate acts, with the removals
            // fact that scopes them. The db-scoped command above is
            // the pasted default; these lines never ride it.
            for line in &outcome.wider_scopes {
                println!("  {line}");
            }
        }
        Command::Migrate {
            command:
                MigrateCommand::Status {
                    dir,
                    endpoint,
                    ns,
                    db,
                    credentials,
                    ledger_table,
                },
        } => {
            // The same connection discipline as push (the same
            // resolution, the same redaction guarantees — one data
            // path) — except the endpoint is OPTIONAL (issue #77):
            // without one (flag or environment) the run is OFFLINE,
            // the local classification only, and NO credentials are
            // resolved (nothing dials, so nothing authenticates). The
            // report stays plain stdout either way; the exit code
            // carries the verdict.
            let (outcome, offline) = match resolve_coordinate(endpoint, "SURFORM_ENDPOINT") {
                Ok(Some(endpoint)) => {
                    let ns = match require_coordinate(ns, "SURFORM_NS", "ns", "namespace") {
                        Ok(ns) => ns,
                        Err(message) => fail(&message, cli.json),
                    };
                    let db = match require_coordinate(db, "SURFORM_DB", "db", "database") {
                        Ok(db) => db,
                        Err(message) => fail(&message, cli.json),
                    };
                    let auth = match resolve_auth(&credentials) {
                        Ok(auth) => auth,
                        Err(message) => fail(&message, cli.json),
                    };
                    let target = surform::migrate::transport::Target {
                        endpoint,
                        ns,
                        db,
                        auth,
                    };
                    match surform::migrate::status::status(&dir, &ledger_table, &target) {
                        Ok(outcome) => (outcome, false),
                        Err(error) => fail(&error.to_string(), cli.json),
                    }
                }
                Ok(None) => match surform::migrate::status::status_offline(&dir) {
                    Ok(outcome) => (outcome, true),
                    Err(error) => fail(&error.to_string(), cli.json),
                },
                Err(message) => fail(&message, cli.json),
            };
            println!("== status ==");
            for file in &outcome.files {
                let verdict = match &file.state {
                    surform::migrate::status::FileState::Applied => "applied".to_string(),
                    surform::migrate::status::FileState::Pending => "pending".to_string(),
                    surform::migrate::status::FileState::Draft => {
                        "draft (commit it first)".to_string()
                    }
                    surform::migrate::status::FileState::Committed => {
                        "committed — ledger state unknown (no endpoint)".to_string()
                    }
                    surform::migrate::status::FileState::Drifted {
                        file_hash,
                        ledger_hash,
                    } => format!(
                        "DRIFTED — file sha256:{}, ledger sha256:{}",
                        short_hash(file_hash),
                        short_hash(ledger_hash),
                    ),
                };
                // A group member's verdict names its position — the
                // group's rows render grouped (the run is whole or
                // nothing).
                let verdict = match (file.group, &file.state) {
                    (
                        Some(group),
                        surform::migrate::status::FileState::Applied
                        | surform::migrate::status::FileState::Pending,
                    ) => format!(
                        "{verdict} — group {:04} {}/{}",
                        group.number, group.letter, group.total
                    ),
                    _ => verdict,
                };
                push_line(&file.key, &verdict);
            }
            for row in &outcome.missing {
                push_line(
                    &format!("{:04}_{}", row.number, row.name),
                    "in the ledger, no local file",
                );
            }
            println!();
            if outcome.files.is_empty() && outcome.missing.is_empty() {
                if offline {
                    println!(
                        "no migration files in {} (offline — no endpoint)",
                        dir.display()
                    );
                } else {
                    println!(
                        "no migration files in {} and the ledger holds no records",
                        dir.display()
                    );
                }
            } else {
                let mut segments: Vec<String> = Vec::new();
                let count = |state: &str| {
                    outcome
                        .files
                        .iter()
                        .filter(|file| match state {
                            "applied" => {
                                matches!(file.state, surform::migrate::status::FileState::Applied)
                            }
                            "pending" => {
                                matches!(file.state, surform::migrate::status::FileState::Pending)
                            }
                            "draft" => {
                                matches!(file.state, surform::migrate::status::FileState::Draft)
                            }
                            "committed" => {
                                matches!(file.state, surform::migrate::status::FileState::Committed)
                            }
                            _ => matches!(
                                file.state,
                                surform::migrate::status::FileState::Drifted { .. }
                            ),
                        })
                        .count()
                };
                for (label, n) in [
                    ("applied", count("applied")),
                    ("pending", count("pending")),
                    ("draft", count("draft")),
                    ("committed", count("committed")),
                    ("drifted", count("drifted")),
                ] {
                    if n > 0 {
                        segments.push(format!("{n} {label}"));
                    }
                }
                let mut summary = format!("{} migration file(s)", outcome.files.len());
                if !segments.is_empty() {
                    summary.push_str(&format!(": {}", segments.join(", ")));
                }
                if !outcome.missing.is_empty() {
                    summary.push_str(&format!(
                        "; {} ledger row(s) without a local file",
                        outcome.missing.len()
                    ));
                }
                if offline {
                    summary.push_str(" (offline — the ledger was not read, no endpoint)");
                }
                println!("{summary}");
            }
            // The exit rule: drifted and missing are the
            // inconsistency classes (the same drift push refuses to
            // apply past); pending and draft are the pipeline's
            // normal states — and the offline run cannot be
            // inconsistent by construction (no ledger, no drift, no
            // missing), which is the point: exit 0 always.
            if outcome.inconsistent() {
                std::process::exit(1);
            }
        }
        Command::Migrate {
            command:
                MigrateCommand::Revert {
                    to,
                    dir,
                    endpoint,
                    ns,
                    db,
                    credentials,
                    ledger_table,
                },
        } => {
            // The same connection discipline as push; revert GENERATES
            // and stops — nothing is applied here. The target's ns/db
            // spell again in the road-onward hint below.
            let target = match resolve_connection(endpoint, ns, db, &credentials) {
                Ok(target) => target,
                Err(message) => fail(&message, cli.json),
            };
            let outcome = match surform::migrate::revert::revert(&dir, &ledger_table, &target, &to)
            {
                Ok(Some(outcome)) => outcome,
                Ok(None) => {
                    println!("== revert ==");
                    println!("(converged — the database already matches the recorded state)");
                    return;
                }
                Err(error) => fail(&error.to_string(), cli.json),
            };
            println!("== revert ==");
            println!(
                "to: {} (sha256:{})",
                ledger_row_key(outcome.row_number, &outcome.row_name),
                short_hash(&outcome.row_hash)
            );
            println!("draft: {}", outcome.draft.display());
            println!(
                "  source: {} (the recorded state, verbatim — commit re-loads it)",
                outcome.source.display()
            );
            if outcome.slots.is_empty() {
                println!("  no data-migration slots — the recorded state orphans nothing");
            } else {
                println!(
                    "  {} unresolved SURFORM SLOT region(s) — fill each before committing:",
                    outcome.slots.len()
                );
                for slot in &outcome.slots {
                    println!("    - {slot}");
                }
            }
            println!();
            println!("nothing is applied yet — the revert travels the normal road:");
            println!(
                "  surform migrate commit {} -m \"revert to {}\"",
                outcome.draft.display(),
                ledger_row_key(outcome.row_number, &outcome.row_name)
            );
            println!(
                "  surform migrate push --endpoint … --ns {} --db {}",
                target.ns, target.db
            );
        }
        Command::Migrate {
            command: MigrateCommand::Renumber { file, number },
        } => {
            // The rename tool the mismatch refusal names (issue #67):
            // header number and file name move together — the one
            // operation that cannot split the identity push/status
            // walk. Every refusal (parse, committed, collision,
            // four-digit bounds) renders from DraftError's own texts.
            match surform::migrate::renumber_draft(&file, number) {
                Ok(outcome) if outcome.in_place => println!(
                    "renumbered in place (header -> {number:04}): {}",
                    outcome.to.display()
                ),
                Ok(outcome) => println!(
                    "renumbered {} -> {}",
                    outcome.from.display(),
                    outcome.to.display()
                ),
                Err(error) => fail(&error.to_string(), cli.json),
            }
        }
        Command::Lint { file, info } => {
            // A dirty composition never lints: the load reports every
            // diagnostic and the exit comes first.
            let loaded = load_or_exit(&file, cli.json);
            let resolved = if info.is_empty() {
                // The resolved state of a standalone file is its
                // expanded form (spec/mixin.md): a spread member is a
                // field the database would hold, so the field-level
                // lints must see it (the authored pass keeps the
                // original view — SDL-L0021 needs it).
                expand::expand(&loaded.file)
            } else {
                // Reader diagnostics exit fail-closed before any
                // lint runs (the normalize/diff posture). The merged
                // file carries its spreads (the spread-preserving
                // merge) — expansion re-injects their members so the
                // resolved view is exactly the standalone path's
                // shape modulo adopted drift.
                let model = ingest_info_model(&info, &loaded.file, cli.json);
                expand::expand(&merge(&loaded.file, &model))
            };
            let parts = loaded.lint_parts();
            let findings = lint::lint_composed(&loaded.file, &resolved, &parts, &loaded.diamonds);
            for finding in &findings {
                // Spans are per-home coordinates: each finding renders
                // against the participating file it came from.
                let home = &loaded.homes[finding.home];
                report(finding, Some((&home.display, &home.text)), cli.json);
            }
            if !findings.is_empty() {
                std::process::exit(1);
            }
        }
    }
}

/// The `--max-width` guard: a budget of zero would break every
/// multi-token value, and absurd values only weaken the guard's
/// meaning — bounded to a sane range instead.
fn validated_width(max_width: usize) -> usize {
    const FLOOR: usize = 20;
    const CEILING: usize = 10_000;
    if (FLOOR..=CEILING).contains(&max_width) {
        return max_width;
    }
    Cli::command()
        .error(
            clap::error::ErrorKind::ValueValidation,
            format!("--max-width must be between {FLOOR} and {CEILING}"),
        )
        .exit();
}

/// `--ledger-table`'s guard: an empty name would spell the
/// ledger's `DEFINE TABLE` with an empty identifier the embedded
/// parser refuses at apply — a usage error beats a blessed
/// un-runnable block.
fn reject_empty_ledger_table(name: &str) -> Result<String, String> {
    if name.is_empty() {
        Err("--ledger-table cannot be empty — the ledger statements would spell an empty identifier".to_string())
    } else {
        Ok(name.to_string())
    }
}

/// A tool failure with its one-line reason on stderr and exit 1 —
/// the migrate file pipeline's refusal shape (the SDL diagnostics
/// keep their `report` rendering; these are file-work errors). Under
/// `--json` the refusal renders as one one-line JSON object —
/// `{"error": …, "hint": …}`: the message's text before its first
/// ` — ` is the error, the rest (when present) the hint; a message
/// without the separator carries `error` alone (issue #73).
fn fail(message: &str, json: bool) -> ! {
    if json {
        let (error, hint) = match message.split_once(" — ") {
            Some((error, hint)) => (error, Some(hint)),
            None => (message, None),
        };
        eprintln!("{}", json_refusal(error, hint));
    } else {
        eprintln!("error: {message}");
    }
    std::process::exit(1);
}

/// The one-line refusal object, hand-escaped — serde_json is not a
/// dependency of the binary, and one flat object of strings is the
/// whole surface (control bytes spell themselves `\u00xx`).
fn json_refusal(error: &str, hint: Option<&str>) -> String {
    fn escape(into: &mut String, text: &str) {
        for ch in text.chars() {
            match ch {
                '"' => into.push_str("\\\""),
                '\\' => into.push_str("\\\\"),
                '\n' => into.push_str("\\n"),
                '\r' => into.push_str("\\r"),
                '\t' => into.push_str("\\t"),
                c if (c as u32) < 0x20 => into.push_str(&format!("\\u{:04x}", c as u32)),
                c => into.push(c),
            }
        }
    }
    let mut out = String::from("{\"error\": \"");
    escape(&mut out, error);
    out.push('"');
    if let Some(hint) = hint {
        out.push_str(", \"hint\": \"");
        escape(&mut out, hint);
        out.push('"');
    }
    out.push('}');
    out
}

/// One connection coordinate's environment read — the flag-over-
/// environment pattern every live command shares (issue #76; the
/// `SURFORM_DB_USER`/`SURFORM_DB_PASS` pairing was the model). A
/// present-but-empty value counts as given (unset it instead of
/// blanking it); a non-unicode value refuses with the fix named.
fn resolve_coordinate(flag: Option<String>, env_name: &str) -> Result<Option<String>, String> {
    match flag {
        Some(value) => Ok(Some(value)),
        None => match std::env::var(env_name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(format!(
                "the environment variable {env_name} is set but not valid unicode — \
                 unset it or fix its value"
            )),
        },
    }
}

/// A REQUIRED coordinate: flag over environment, else the refusal
/// naming both ways (rendered as error + hint under `--json`).
fn require_coordinate(
    flag: Option<String>,
    env_name: &str,
    flag_name: &str,
    what: &str,
) -> Result<String, String> {
    match resolve_coordinate(flag, env_name)? {
        Some(value) => Ok(value),
        None => Err(format!("no {what} — pass --{flag_name} or set {env_name}")),
    }
}

/// The credential resolution: flag over environment within each kind,
/// and the kinds exclusive — a bearer token (`--token` or
/// `SURFORM_DB_TOKEN`) refuses alongside ANY user or password, Basic
/// gives both or neither — and, the #76 addition, the TERMINAL: a
/// user without a password asks for it when stdin is a tty (the
/// question on stderr, the answer read plainly — std has no echo
/// suppression, and the value never enters any log, report, or
/// error). Non-tty keeps the pairing refusal. The exclusivity check
/// rides BEFORE the prompt: a doomed run never asks for a password it
/// cannot use.
fn resolve_auth(credentials: &CredentialArgs) -> Result<surform::migrate::transport::Auth, String> {
    let token = resolve_coordinate(credentials.token.clone(), "SURFORM_DB_TOKEN")?;
    let user = resolve_coordinate(credentials.user.clone(), "SURFORM_DB_USER")?;
    let mut pass = resolve_coordinate(credentials.pass.clone(), "SURFORM_DB_PASS")?;
    if token.is_some() && (user.is_some() || pass.is_some()) {
        return Err(
            "a token is given (--token or SURFORM_DB_TOKEN) alongside a user or \
             password (--user/--pass or SURFORM_DB_USER/SURFORM_DB_PASS) — the two \
             credential kinds are exclusive: authenticate with the token alone \
             (Bearer), or drop the token for the user/password pair (Basic)"
                .to_string(),
        );
    }
    if let (Some(_), None) = (&user, &pass)
        && std::io::stdin().is_terminal()
    {
        pass = Some(prompt_for_password()?);
    }
    match (user, pass) {
        (Some(user), Some(pass)) => Ok(surform::migrate::transport::Auth::Basic { user, pass }),
        (None, None) => match token {
            Some(token) => Ok(surform::migrate::transport::Auth::Token { token }),
            None => Ok(surform::migrate::transport::Auth::None),
        },
        (Some(_), None) => Err(
            "a user is given (--user or SURFORM_DB_USER) without a password — add \
             --pass, set SURFORM_DB_PASS, or re-run on a terminal to enter it"
                .to_string(),
        ),
        (None, Some(_)) => Err(
            "a password is given (--pass or SURFORM_DB_PASS) without a user — add \
             --user or set SURFORM_DB_USER"
                .to_string(),
        ),
    }
}

/// The tty password prompt: the question rides stderr (stdout carries
/// the command's report), the answer one plain stdin line. There is
/// deliberately NO echo suppression — std offers none, and shelling
/// out to a terminal driver (or a raw-mode dance) trades a broken
/// terminal on every crash path for hidden typing. The value lives in
/// the returned string alone.
fn prompt_for_password() -> Result<String, String> {
    use std::io::Write;
    let mut stderr = std::io::stderr();
    stderr
        .write_all(b"password: ")
        .and_then(|()| stderr.flush())
        .map_err(|reason| format!("cannot prompt on the terminal: {reason}"))?;
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|reason| format!("cannot read the password from the terminal: {reason}"))?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// The three live commands' connection resolution in ONE home (issue
/// #76): endpoint, namespace, and database — each flag over its
/// environment (`SURFORM_ENDPOINT`/`SURFORM_NS`/`SURFORM_DB`), each
/// required — plus the credentials (see [`resolve_auth`]). The
/// resolved values live only in the returned target.
fn resolve_connection(
    endpoint: Option<String>,
    ns: Option<String>,
    db: Option<String>,
    credentials: &CredentialArgs,
) -> Result<surform::migrate::transport::Target, String> {
    Ok(surform::migrate::transport::Target {
        endpoint: require_coordinate(endpoint, "SURFORM_ENDPOINT", "endpoint", "endpoint")?,
        ns: require_coordinate(ns, "SURFORM_NS", "ns", "namespace")?,
        db: require_coordinate(db, "SURFORM_DB", "db", "database")?,
        auth: resolve_auth(credentials)?,
    })
}

/// The report's row key — the file's ledger identity spelled the way
/// the directory spells it (`0001_add-users`, or a group member's
/// `0005b_shop`): push/status rows carry it precomputed.
fn push_line(key: &str, verdict: &str) {
    println!("  {key:<36} {verdict}");
}

/// The LEDGER row's identity spelled the way the directory spells a
/// standalone (`0001_add-users`) — a group's row spells the base slug
/// (the row is the group's; no letter rides it).
fn ledger_row_key(number: u32, name: &str) -> String {
    format!("{number:04}_{name}")
}

/// A hash's display spelling for the report rows: the first 12 hex
/// characters (git-short-hash width) — safe on any input length (a
/// malformed ledger hash must never panic the report).
fn short_hash(hash: &str) -> String {
    format!("{}…", hash.chars().take(12).collect::<String>())
}

/// Writes the marked script as the next numbered draft under `dir`
/// via the library writer, exiting 1 on every refusal — the CLI's
/// thin front for [`surform::migrate::write_draft`]. The save options
/// (the header's payload — the source path and its canonical-form
/// hash, issue #68's pin — plus the marked script) ride the library's
/// [`surform::migrate::DraftSave`].
fn save_draft(
    dir: &std::path::Path,
    save: surform::migrate::DraftSave,
    json: bool,
) -> std::path::PathBuf {
    match surform::migrate::write_draft(dir, save) {
        Ok(path) => path,
        Err(error) => fail(&error.to_string(), json),
    }
}

/// The multi-scope save's writer — [`surform::migrate::
/// write_draft_group`]'s thin front, exiting 1 on every refusal (the
/// same shape [`save_draft`] gives the single draft). Returns the
/// members in LETTER order (the apply order).
fn save_draft_group(
    dir: &std::path::Path,
    save: surform::migrate::DraftSave,
    json: bool,
) -> Vec<std::path::PathBuf> {
    match surform::migrate::write_draft_group(dir, save) {
        Ok(paths) => paths,
        Err(error) => fail(&error.to_string(), json),
    }
}

/// `--replace <n>`'s writer — [`surform::migrate::replace_draft`]'s
/// thin front, exiting 1 on every refusal (the same shape
/// [`save_draft`] gives the plain save). The hash gate is the
/// library's: the target draft must carry THIS exact script.
fn replace_draft_or_exit(
    dir: &std::path::Path,
    number: u32,
    save: surform::migrate::DraftSave,
    json: bool,
) -> std::path::PathBuf {
    match surform::migrate::replace_draft(dir, number, save) {
        Ok(path) => path,
        Err(error) => fail(&error.to_string(), json),
    }
}

/// The `--info` pipeline shared by `normalize` and `migrate`: specs
/// parse and validate against the SDL's declared scopes before any
/// interchange file opens (duplicates are a parse-side check), and
/// each scope's interchange text reads and ingests on its own — every
/// diagnostic of a scope anchors on that scope's text and exits
/// before any merge or migration runs.
fn ingest_info_model(info: &[String], ast: &SdlFile, json: bool) -> ReadModel {
    let mut specs: Vec<(surform::info::normalize::ScopeTag, String)> = Vec::new();
    let mut seen = HashSet::new();
    for spec in info {
        let parsed = match parse_info_spec(spec) {
            Ok(parsed) => parsed,
            Err(diag) => {
                report(&diag, Some((spec, spec)), json);
                std::process::exit(1);
            }
        };
        if !seen.insert(parsed.0.clone()) {
            report(
                &Diag::InterchangeTagInvalid {
                    span: surform::lexer::Span::point(0),
                    message: "this scope is already given by an earlier \
                              `--info` tag"
                        .to_string(),
                    // The duplicate class's fix is deletion (issue
                    // #90): the generic text would send the user
                    // re-declaring scopes.
                    help: Some("delete the repeated `--info` tag — one tag per scope"),
                },
                Some((spec, spec)),
                json,
            );
            std::process::exit(1);
        }
        if let Err(diag) = validate_scope_tag(&parsed.0, ast) {
            report(&diag, Some((spec, spec)), json);
            std::process::exit(1);
        }
        specs.push(parsed);
    }
    let mut model = ReadModel::default();
    for (tag, path) in &specs {
        let text = read_file_or_exit(path, json);
        let info = match read_info_object(&text) {
            Ok(info) => info,
            Err(diag) => {
                report(&diag, Some((path, &text)), json);
                std::process::exit(1);
            }
        };
        if let Err(diags) = model.push_scope(tag.clone(), &info) {
            for diag in &diags {
                report(diag, Some((path, &text)), json);
            }
            std::process::exit(1);
        }
    }
    model
}

/// One read of a CLI-named file, kind-branched on failure (issue
/// #90): a directory is its own SDL-E0008 diagnostic — the generic
/// unreadable help would say "the file must exist" of a path that
/// plainly exists as a directory. Every other error keeps the
/// generic unreadable report.
fn read_file_or_exit(path: &str, json: bool) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(reason) if reason.kind() == std::io::ErrorKind::IsADirectory => {
            report(
                &Diag::FileIsADirectory {
                    path: path.to_string(),
                },
                None,
                json,
            );
            std::process::exit(1);
        }
        Err(reason) => {
            report(
                &Diag::FileUnreadable {
                    path: path.to_string(),
                    reason: reason.to_string(),
                    span: None,
                },
                None,
                json,
            );
            std::process::exit(1);
        }
    }
}

/// Loads the composition rooted at the file argument, reporting every
/// diagnostic against its own file's text and exiting 1 if there are
/// any; otherwise returns the loaded composition — `check` stops
/// here; `build`/`migrate`/`lint` take the flattened document onward.
fn load_or_exit(file: &str, json: bool) -> Loaded {
    let text = read_file_or_exit(file, json);
    load_text_or_exit(file, text, json)
}

/// The load half of [`load_or_exit`], over an already-read text —
/// `fmt --in-place` reads first so its migration-script gate fires
/// before any parse, then hands the same bytes to the load.
fn load_text_or_exit(file: &str, text: String, json: bool) -> Loaded {
    match load::load(std::path::Path::new(file), file, text) {
        Ok(loaded) => loaded,
        Err(failure) => {
            for (diag, home) in &failure.errors {
                let home = &failure.homes[*home];
                report(diag, Some((&home.display, &home.text)), json);
            }
            std::process::exit(1);
        }
    }
}

/// `fmt`'s migration-script gate, riding every input mode (file,
/// stdin, print, `--in-place`, `--check`): a generated migration's
/// draft header (`-- surform migration NNNN — <name>`) means the text
/// is SurrealQL the migration pipeline owns — `--` is SurrealQL
/// comment syntax, not SDL's, so the parse below would die in a wall
/// of SDL-E1001/E1002 where ONE clear refusal belongs (SDL-E0017).
/// The gate anchors on the exact draft-header prefix, so a normal SDL
/// file whose first line is a `//` comment never trips it.
fn refuse_migration_script(source: &str, name: &str, json: bool) {
    if !surform::migrate::opens_with_draft_header(source) {
        return;
    }
    let len = source.lines().next().map_or(0, str::len);
    report(
        &Diag::MigrationScriptRefused {
            span: surform::lexer::Span::new(0, len),
        },
        Some((name, source)),
        json,
    );
    std::process::exit(1);
}

// ---- fmt --check's diff ------------------------------------------------

/// The `--check` diff's context window: unchanged lines shown around
/// each change group — the unified-diff convention.
const DIFF_CONTEXT: usize = 3;

/// One side's line, split from its text. The trailing newline is part
/// of the line's IDENTITY — a file ending without one is a real
/// difference (git's `\ No newline at end of file`), so the
/// canonicalization of `x` to `x\n` renders a change instead of
/// silently exiting 0 with nothing to show. Text+terminated is a
/// bijection with the split's raw pieces, so equal line sequences
/// mean byte-equal texts.
#[derive(PartialEq, Eq, Clone, Copy)]
struct DiffLine<'a> {
    text: &'a str,
    terminated: bool,
}

fn diff_lines(text: &str) -> Vec<DiffLine<'_>> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split_inclusive('\n')
        .map(|line| DiffLine {
            text: line.strip_suffix('\n').unwrap_or(line),
            terminated: line.ends_with('\n'),
        })
        .collect()
}

/// One step of the edit script: a line both sides share, a line only
/// the input has, or a line only the canonical form has. The index
/// addresses its own side's [`DiffLine`] slice.
#[derive(Clone, Copy)]
enum DiffOp {
    Same(usize),
    Out(usize),
    In(usize),
}

/// The LCS table above this cell count renders coarsely (the whole
/// changed middle out, then in) instead of allocating quadratically —
/// correct, just not minimal. The prefix/suffix trim keeps real
/// schema files far below it.
const DIFF_TABLE_CAP: usize = 4_000_000;

/// The line-level edit script: common prefix and suffix ride as
/// [`DiffOp::Same`], the trimmed middle goes through an LCS dynamic
/// program (deletions preferred on ties, so `-` lines precede `+`
/// lines within a change group).
fn diff_ops(before: &[DiffLine<'_>], after: &[DiffLine<'_>]) -> Vec<DiffOp> {
    let prefix = before.iter().zip(after).take_while(|(a, b)| a == b).count();
    let suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let a_mid = &before[prefix..before.len() - suffix];
    let b_mid = &after[prefix..after.len() - suffix];
    let mut ops = Vec::new();
    if a_mid.len().saturating_mul(b_mid.len()) > DIFF_TABLE_CAP {
        ops.extend((0..a_mid.len()).map(|index| DiffOp::Out(prefix + index)));
        ops.extend((0..b_mid.len()).map(|index| DiffOp::In(prefix + index)));
    } else {
        // table[i * stride + j]: the LCS length of a_mid[i..] and
        // b_mid[j..]; built bottom-up, walked forward.
        let (n, m) = (a_mid.len(), b_mid.len());
        let stride = m + 1;
        let mut table = vec![0u32; (n + 1) * stride];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                table[i * stride + j] = if a_mid[i] == b_mid[j] {
                    table[(i + 1) * stride + j + 1] + 1
                } else {
                    table[(i + 1) * stride + j].max(table[i * stride + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if a_mid[i] == b_mid[j] {
                ops.push(DiffOp::Same(prefix + i));
                i += 1;
                j += 1;
            } else if table[(i + 1) * stride + j] >= table[i * stride + j + 1] {
                ops.push(DiffOp::Out(prefix + i));
                i += 1;
            } else {
                ops.push(DiffOp::In(prefix + j));
                j += 1;
            }
        }
        ops.extend((i..n).map(|index| DiffOp::Out(prefix + index)));
        ops.extend((j..m).map(|index| DiffOp::In(prefix + index)));
    }
    let mut full = Vec::with_capacity(prefix + ops.len() + suffix);
    full.extend((0..prefix).map(DiffOp::Same));
    full.extend(ops);
    full.extend((before.len() - suffix..before.len()).map(DiffOp::Same));
    full
}

/// One rendered hunk row: its marker (` `, `-`, `+`) and its line.
struct DiffRow<'a> {
    marker: char,
    line: DiffLine<'a>,
    /// The 0-based line index on the input side (`None` for an
    /// added line).
    a_index: Option<usize>,
    /// The 0-based line index on the canonical side (`None` for a
    /// removed line).
    b_index: Option<usize>,
}

/// The `--check` diff, git-unified style: `---`/`+++` headers (the
/// canonical side named), `@@ -a,c +b,c @@` hunk headers, three
/// context lines around each change group (groups within 2× the
/// window merge into one hunk), and the `\ No newline at end of
/// file` marker. Callers only reach here when the texts differ, and
/// differing texts always yield at least one change row (line
/// identity round-trips the bytes).
fn render_check_diff(name: &str, before: &str, after: &str) -> String {
    // The side letters follow the hunk headers and [`DiffRow`]: `a`
    // is the input, `b` the canonical form.
    let a_lines = diff_lines(before);
    let b_lines = diff_lines(after);
    let ops = diff_ops(&a_lines, &b_lines);
    let mut rows: Vec<DiffRow<'_>> = Vec::with_capacity(ops.len());
    let (mut a_pos, mut b_pos) = (0usize, 0usize);
    for op in ops {
        let row = match op {
            DiffOp::Same(index) => {
                let row = DiffRow {
                    marker: ' ',
                    line: a_lines[index],
                    a_index: Some(a_pos),
                    b_index: Some(b_pos),
                };
                a_pos += 1;
                b_pos += 1;
                row
            }
            DiffOp::Out(index) => {
                let row = DiffRow {
                    marker: '-',
                    line: a_lines[index],
                    a_index: Some(a_pos),
                    b_index: None,
                };
                a_pos += 1;
                row
            }
            DiffOp::In(index) => {
                let row = DiffRow {
                    marker: '+',
                    line: b_lines[index],
                    a_index: None,
                    b_index: Some(b_pos),
                };
                b_pos += 1;
                row
            }
        };
        rows.push(row);
    }
    // The change rows' positions in `rows`, the hunk seeds.
    let changes: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| (row.marker != ' ').then_some(index))
        .collect();
    let mut out = format!("--- {name}\n+++ {name} (canonical)\n");
    let mut seed = 0;
    while seed < changes.len() {
        // One hunk: from this seed's change, absorb following changes
        // whose unchanged run to them leaves the contexts touching —
        // git's rule (xdiff's `xdl_get_hunk`): merge while the gap of
        // unchanged lines is at most 2× the window (the difference in
        // row indices is that gap plus the row itself).
        let first = changes[seed];
        let mut last = seed;
        while last + 1 < changes.len() && changes[last + 1] - changes[last] <= 2 * DIFF_CONTEXT + 1
        {
            last += 1;
        }
        let start = rows.len().min(first.saturating_sub(DIFF_CONTEXT));
        let end = rows.len().min(changes[last] + DIFF_CONTEXT + 1);
        let hunk = &rows[start..end];
        let a_first = hunk.iter().filter_map(|row| row.a_index).min();
        let b_first = hunk.iter().filter_map(|row| row.b_index).min();
        // Git's convention: an empty side's start is the position the
        // lines would occupy — the 0-based line before the insertion
        // point (`-0,0` for a hunk at the very top); otherwise the
        // 1-based line the hunk opens on.
        let a_start = match a_first {
            Some(index) => index + 1,
            None => a_pos_at(&rows, start),
        };
        let b_start = match b_first {
            Some(index) => index + 1,
            None => b_pos_at(&rows, start),
        };
        let a_count = hunk.iter().filter(|row| row.a_index.is_some()).count();
        let b_count = hunk.iter().filter(|row| row.b_index.is_some()).count();
        out.push_str(&format!(
            "@@ -{a_start},{a_count} +{b_start},{b_count} @@\n"
        ));
        for row in hunk {
            out.push(row.marker);
            out.push_str(row.line.text);
            out.push('\n');
            if !row.line.terminated {
                out.push_str("\\ No newline at end of file\n");
            }
        }
        seed = last + 1;
    }
    out
}

/// The input side's 0-based position at `rows[start]` — the count of
/// input lines emitted before that row.
fn a_pos_at(rows: &[DiffRow<'_>], start: usize) -> usize {
    rows[..start].iter().filter_map(|row| row.a_index).count()
}

/// The canonical side's 0-based position at `rows[start]`.
fn b_pos_at(rows: &[DiffRow<'_>], start: usize) -> usize {
    rows[..start].iter().filter_map(|row| row.b_index).count()
}

/// Prints the `--check` diff to stderr in one write. The output is
/// finding-class (exit 1's report), and it shares the ledgered EPIPE
/// corner (#32): an early-closing pipe makes `eprintln!` panic — the
/// standing corner this printer deliberately does not worsen or fix.
fn print_check_diff(name: &str, before: &str, after: &str) {
    eprint!("{}", render_check_diff(name, before, after));
}

/// `fmt`'s input read, gated: the source text plus the name its
/// diagnostics and `--check` diff render under — stdin (`None` or
/// `-`) reads as `<stdin>`, a file as the path as given. The
/// migration-script gate rides EVERY fmt read here — print,
/// `--check`, and the `--in-place` tree writer all read through this
/// one home, so no mode can parse past the gate.
fn read_fmt_source(file: Option<&str>, json: bool) -> (String, String) {
    match file {
        None | Some("-") => {
            let source = read_stdin_or_exit(json);
            refuse_migration_script(&source, "<stdin>", json);
            (source, "<stdin>".to_string())
        }
        Some(path) => {
            let source = read_file_or_exit(path, json);
            refuse_migration_script(&source, path, json);
            (source, path.to_string())
        }
    }
}

/// `fmt`'s parse half: the composable entry (spec/include.md) — fmt
/// round-trips `include` statements without resolving them, spreads
/// parse as opaque names and re-print, so ANY single file formats
/// standalone (the declarative namespace's standalone-leaf win).
/// Resolution is check-class; fmt is not. Same shape as
/// [`check_source_or_exit`] so the read half is shared.
fn fmt_source_or_exit(source: &str, name: &str, json: bool) -> SdlFile {
    let (ast, diags) = parser::parse_file_composable(source);
    if !diags.is_empty() {
        for diag in &diags {
            report(diag, Some((name, source)), json);
        }
        std::process::exit(1);
    }
    ast
}

/// The stdin pipe editors format an unsaved buffer through: the whole
/// text to EOF. A failed read diagnoses like an unreadable file.
fn read_stdin_or_exit(json: bool) -> String {
    use std::io::Read;
    let mut source = String::new();
    if let Err(reason) = std::io::stdin().read_to_string(&mut source) {
        report(
            &Diag::FileUnreadable {
                path: "<stdin>".to_string(),
                reason: reason.to_string(),
                span: None,
            },
            None,
            json,
        );
        std::process::exit(1);
    }
    source
}

/// Renders one diagnostic or lint finding to stderr: JSON (one
/// object) or the graphical handler (already theme-configured by the
/// hook — printed via `Debug`, which miette's hook routes). `source`
/// is attached for span rendering when the item came from a parsed
/// file, NAMED so a composed root's per-file snippets say which file
/// they show (the CLI argument names only the root); findings without
/// an authored span render sourceless.
fn report<D>(item: &D, source: Option<(&str, &str)>, json: bool)
where
    D: miette::Diagnostic + Clone + Send + Sync + 'static,
{
    let report = miette::Report::new(item.clone());
    let report = match source {
        None => report,
        Some((name, source)) => {
            report.with_source_code(miette::NamedSource::new(name, source.to_owned()))
        }
    };
    if json {
        let mut out = String::new();
        miette::JSONReportHandler::new()
            .render_report(&mut out, report.as_ref())
            .expect("JSON rendering into a String cannot fail");
        eprintln!("{out}");
    } else {
        eprintln!("{report:?}");
    }
}

#[cfg(test)]
mod diff_tests {
    use super::render_check_diff;

    /// One changed line mid-file: a single hunk, the change as an
    /// out/in pair, context lines on both sides.
    #[test]
    fn one_changed_line_is_one_hunk() {
        let diff = render_check_diff(
            "schema.surdl",
            "namespace acme;\nns acme\ndatabase shop;\n",
            "namespace acme;\nns acme;\ndatabase shop;\n",
        );
        assert_eq!(
            diff,
            [
                "--- schema.surdl",
                "+++ schema.surdl (canonical)",
                "@@ -1,3 +1,3 @@",
                " namespace acme;",
                "-ns acme",
                "+ns acme;",
                " database shop;",
                "",
            ]
            .join("\n")
        );
    }

    /// The trailing newline is part of a line's identity: `x` vs
    /// `x\n` is a rendered change carrying the git marker — not the
    /// silent no-hunks exit the raw line text alone would produce.
    #[test]
    fn missing_trailing_newline_renders_the_marker() {
        let diff = render_check_diff("<stdin>", "ns acme;", "ns acme;\n");
        assert_eq!(
            diff,
            [
                "--- <stdin>",
                "+++ <stdin> (canonical)",
                "@@ -1,1 +1,1 @@",
                "-ns acme;",
                "\\ No newline at end of file",
                "+ns acme;",
                "",
            ]
            .join("\n")
        );
    }

    /// Changes far apart split into two hunks; changes within 2x the
    /// context window share one.
    #[test]
    fn hunk_merging_follows_the_context_window() {
        let mut before = String::new();
        let mut after = String::new();
        for i in 0..40 {
            before.push_str(&format!("line {i};\n"));
            let line = if i == 5 || i == 34 {
                format!("LINE {i};\n")
            } else {
                format!("line {i};\n")
            };
            after.push_str(&line);
        }
        let diff = render_check_diff("f.surdl", &before, &after);
        assert_eq!(
            diff.lines().filter(|line| line.starts_with("@@")).count(),
            2,
            "two hunks: {diff}"
        );
        // The 5 and 34 changes are far apart; pull one change to
        // within the window of the other and they merge.
        let mut after2 = String::new();
        for i in 0..40 {
            let line = if i == 5 || i == 8 {
                format!("LINE {i};\n")
            } else {
                format!("line {i};\n")
            };
            after2.push_str(&line);
        }
        let diff = render_check_diff("f.surdl", &before, &after2);
        assert_eq!(
            diff.lines().filter(|line| line.starts_with("@@")).count(),
            1,
            "one merged hunk: {diff}"
        );
        // The exact git boundary: a change 6 unchanged lines away
        // (its 3 trailing context lines touch the next one's 3
        // leading lines) still merges; 7 lines apart splits.
        let build = |second: usize| {
            let mut text = String::new();
            for i in 0..40 {
                let line = if i == 5 || i == second {
                    format!("LINE {i};\n")
                } else {
                    format!("line {i};\n")
                };
                text.push_str(&line);
            }
            text
        };
        for (second, hunks) in [(12, 1), (13, 2)] {
            let diff = render_check_diff("f.surdl", &before, &build(second));
            assert_eq!(
                diff.lines().filter(|line| line.starts_with("@@")).count(),
                hunks,
                "change at {second}: {hunks} hunk(s): {diff}"
            );
        }
    }

    /// An insertion with following context rides one hunk whose
    /// before-side counts only that context row; the `-0,0`
    /// empty-side header is the EMPTY file's shape (the next test).
    #[test]
    fn insertion_at_the_top_keeps_its_context() {
        let diff = render_check_diff("f.surdl", "ns acme;\n", "/// doc\nns acme;\n");
        assert_eq!(
            diff,
            [
                "--- f.surdl",
                "+++ f.surdl (canonical)",
                "@@ -1,1 +1,2 @@",
                "+/// doc",
                " ns acme;",
                "",
            ]
            .join("\n")
        );
    }

    /// Empty input against a canonical file: everything added.
    #[test]
    fn empty_input_is_all_added() {
        let diff = render_check_diff("f.surdl", "", "ns acme;\n");
        assert_eq!(
            diff,
            [
                "--- f.surdl",
                "+++ f.surdl (canonical)",
                "@@ -0,0 +1,1 @@",
                "+ns acme;",
                "",
            ]
            .join("\n")
        );
    }

    /// Equal line TEXTS with different termination flags are a
    /// change — the identity round-trips the bytes.
    #[test]
    fn termination_difference_is_a_change() {
        let diff = render_check_diff("f.surdl", "a\n", "a");
        assert!(
            diff.contains("-a\n") && diff.contains("+a\n"),
            "diff: {diff}"
        );
    }
}
