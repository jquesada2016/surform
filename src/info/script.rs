//! The convergence script: the differ's findings rendered as the ONE SurrealQL
//! migration `surform migrate generate` emits. Defines first — the
//! SdlOnly and Diverged findings as `DEFINE … OVERWRITE` from the
//! authored SDL side, in the compiler's own emission order with ONE
//! migration-only addition (below) — then today's remove-script
//! ordering demoted to the tail: the database-only removals
//! (`IF EXISTS`), children before parents, `REMOVE DATABASE` then
//! `REMOVE NAMESPACE` last.
//!
//! The defines' addition to the emission order — the member→host rule: a
//! standalone child (a field/event/index) whose host table ALSO
//! defines in the same scope renders INSIDE the host's emission — its
//! statement joins the host's nested members, so it emits between the
//! host's table head and them. The emission-order sort carries no member→host
//! edge because a standalone member AUTO-CREATES a missing host
//! (order.rs's probe-pinned fact), and that premise holds only when
//! the host is absent or already in its SDL form; a migration can
//! hold the host in a form the engine refuses to hang a member on (a
//! LIGHTWEIGHT relation — vendored define/{field,event,index}.rs), so
//! the host's `OVERWRITE` (which replaces that form) must run first.
//! And the host's emission is ONE unit: a nested index naming the
//! child hard-fails `FdNotFound` on a SCHEMAFULL table that lacks it
//! (vendored define/index.rs), so when the host's authored define
//! nests such an index, no order of separate statements satisfies
//! both edges — the child must ride between the head and the members,
//! which slotting delivers (the intra-table sort then orders the
//! joined members by the frozen edges: fields before the indexes that
//! name them). The rule reads the child's typed `on:`; a raw-authored
//! define rides the same rule through its header's `ON <table>` edge
//! (place.rs's `raw_on_table`) — it cannot join the `nested` list
//! (codegen's table arm prints index/event/field members only), so
//! its host's emission unfolds into head, raw members, and typed
//! members as separate statements, each part byte-identical to its
//! slice of the one-unit render. A raw body whose header carries no
//! `ON` — or never parses to a family and key at all — keeps its
//! sort position, the fail-closed posture the removals tail's manual
//! branches take. And a child finding the host's own
//! emission carries — the authored table nests it — is dropped
//! outright: the member renders ONCE, inside that emission, never
//! standalone.
//!
//! The defines' second migration-only addition settles the edges the
//! sorted partition and `emission_order` cannot see — every edge with
//! a raw body on EITHER end. The contract is the FULL frozen edge
//! table (order.rs's module doc) for all four parities, single- and
//! multi-statement raw bodies alike: the typed graph rides
//! `emission_order`, and each raw end rides the settle pass. A raw
//! body registers no definition in `emission_order`'s dependency
//! registry (order.rs's `def_ident`), so a raw-defined TARGET cannot
//! resolve a typed edge (a typed view whose source table defines as
//! `raw { DEFINE TABLE … }` would render first and refuse
//! `TbNotFound` — vendored define/table.rs walks the view's
//! `source_tables()` at define), and a raw body's OWN slots never
//! register at all (a raw view table's `AS SELECT … FROM` list; a raw
//! fulltext index's `ANALYZER` clause; a raw reader family's
//! define-time value slots — place.rs's `raw_dep_names`/
//! `raw_index_analyzer`/`raw_value_reads`). A raw block renders
//! contiguously — one finding, one verbatim statement — so each
//! reader walks EVERY statement of the block and the body's edge set
//! is the UNION of its statements' edges: first-header classification
//! gates nothing, a statement that carries no edges leaves the
//! others' standing, and a block that fails to parse entirely reads
//! no edge (the fail-closed posture — the engine refuses it at parse
//! time, visibly). The
//! settle pass orders every such edge dependency-first: the
//! view→source-table edges for both parities of both ends, the raw
//! index's analyzer edge, and the value-read edges (order.rs's rows
//! 6–13 — a param's VALUE, a sequence's BATCH/START/TIMEOUT, a user's
//! DURATION sides, an access's jwt url/key + DURATION sides, a
//! bucket's BACKEND, api middleware ARGS, config DEFAULT sides) for a
//! typed carrier whose target defines raw (`emission_order`'s
//! registry is blind to it) and for a raw carrier whose target
//! defines either way. The typed twins — a typed carrier whose target
//! also defines typed — ride `emission_order` unchanged, and a raw
//! index MEMBER's analyzer needs no pass — the family order puts an
//! Analyzer define before every Index/Table position, and the unfold
//! splices the member inside its host's emission, after both. A
//! dependency the SDL never declares is broken authorship no ordering
//! repairs (the reader refuses at define or first write,
//! fail-visible). The pass reorders by the stable topological sort of
//! its synthesized edges (the current order the tie-break), so a
//! satisfied scope is a fixed point and the sort terminates
//! structurally — and because value readers can read value readers (a
//! raw↔raw param cycle, a class E0015's checker cannot see through
//! the raw bodies), the pass first prunes its own cycles: an
//! unorderable reader cycle keeps its positions, and the migration
//! fails visibly at the engine (the rollback) instead of hanging the
//! generator.
//!
//! A member whose host the SDL never declares does not converge — and
//! the auto-create premise is FLEXIBLE-blind. A PLAIN member defines
//! against the engine's auto-created schemaless shell (order.rs's
//! probe-pinned fact), the removals tail then removes the
//! database-only table behind it, and the next run re-creates the
//! shell — a define-then-remove cycle for a self-contradictory
//! document; every run is safe and announced (the header claims
//! re-runs are safe, not convergent). A FLEXIBLE member refuses
//! outright: the engine validates FLEXIBLE before the auto-create
//! (vendored define/field.rs — `get_or_add_tb` runs after) and fails
//! `TbNotFound` on the missing host, rolling the transaction back —
//! nothing lands, and no ordering repairs it (the host define is not
//! in the script — the SDL never declared it). The repair either way
//! is authoring the host or dropping the member.
//!
//! The wrapping is one `BEGIN TRANSACTION` / `COMMIT` PER SCOPE
//! SECTION: each scope's defines and removals share one frame, the
//! scope's `USE` switch emits OUTSIDE it, and a failed statement
//! rolls its own section back — sections before it stay applied.
//! (The 2026-10-08 amendment of the one-transaction wrap. The
//! USE-in-transaction alarm — statements after a mid-transaction `USE`
//! silently no-oping — did NOT reproduce under matched conditions on either
//! 3.3.0 engine, embedded or over `/sql`; the
//! reproducer had posted session headers that disagreed with
//! the script's `USE` and captured the header's scope. The chosen
//! posture is the one that needs no engine behavior to be safe: a
//! session switch never rides inside a transaction, and a failure
//! strands one scope, not the script.) DDL rides transactions
//! (defines, removes, `REMOVE DATABASE`/`NAMESPACE` included —
//! probed against the embedded 3.3 engine, 2026-10-06).
//! The script is idempotent for typed statements and
//! single-statement raw bodies (`OVERWRITE` redefines, `IF EXISTS`
//! no-ops on absence); a multi-statement raw block re-pairs by its
//! first header alone on re-run, so its unpaired tail statements
//! re-drift — the first-header pairing granularity behind the
//! re-diff corner. One executed boundary
//! (probed 2026-10-06): a fulltext index over a
//! record-holding table builds AT define and reads the
//! PRE-transaction catalog, so an analyzer the same transaction
//! defines is invisible to it — that pair refuses in EITHER order,
//! no ordering repairs it; the repair is a converged analyzer (an
//! empty table's build defers instead, so the migration-time pair is
//! fine).
//!
//! Sentinel safety: defines always render from AUTHORED statements —
//! an SdlOnly or Diverged finding carries the SDL side by
//! construction, so no raw-body adoptee and no sentinel can ever leak
//! through a define. Removals keep the fail-closed manual branches
//! (a raw body whose header never classified; an anonymous-silo
//! module whose key is the reader's cross-check name — the
//! known corner; a field key whose segments are not plain words — a
//! wildcard, or a quoted name containing a dot, which REMOVE FIELD
//! would re-spell as a different path).
//!
//! The grammar inventory is source-verified against the pinned 3.3.0
//! parser and printers — every family has REMOVE grammar, so the
//! manual-removal branches are fail-closed-only. `IF EXISTS` rides
//! every REMOVE: it no-ops on absence (source-verified) — the
//! remove-side mirror of define-side `OVERWRITE` (it does NOT soften
//! catalog-integrity refusals: an analyzer still referenced by a
//! fulltext index errors `AzInUse` regardless).
//!
//! Removal ordering is referenced-before-referencing lifted across
//! scopes: a section per scope in root → namespace → database order
//! (within a section the ranks are children — Event/Field/Index —
//! then Table, then Analyzer, whose removal must follow the indexes
//! that point at it), then the `REMOVE DATABASE` lines (their own
//! per-namespace frames), then `REMOVE NAMESPACE` lines last, so a
//! removal-implying parent always follows everything it implies.
//! `USE` lines re-emit at every context switch (codegen model item
//! 1's standalone-runnable convention), always OUTSIDE the frame
//! they precede — load-bearing for `REMOVE DATABASE`, which takes
//! its namespace from the session.
//!
//! Every data-destroying statement — `REMOVE TABLE` (deletes the
//! table's key range: the documents go with it), `REMOVE DATABASE`
//! and `REMOVE NAMESPACE` (the same for their whole subtree) —
//! carries a loud greppable `SURFORM WARNING` comment block above it.
//! A scope whose owning namespace/database the SDL never declares
//! collapses to the single `REMOVE NAMESPACE`/`REMOVE DATABASE` —
//! reachable through the library API (a hand-built read model) and
//! the CLI's `--from`/`--to` version pair (a scope the TO file
//! drops). The `--info` mode's tag validation (SDL-E3005) still
//! refuses undeclared scopes — the data-loss guard where the database
//! side is REAL; the version pair has no typed tags (both sides are
//! loaded files), so the collapse + WARNING mechanism carries the
//! data-loss role there.
//!
//! The REQUIRED data-migration slots ride the same anchor
//! mechanism. The taxonomy is the differ's own (`report.rs`'s
//! `required_slot` — the one home the script's emission and the
//! report's manifest share): a DatabaseOnly TABLE orphans its
//! documents (the REMOVE deletes the key range) and a DatabaseOnly
//! FIELD orphans its values (`REMOVE FIELD` is catalog-only — they
//! remain on the rows, outside every field definition); both carry a
//! greppable
//! `SURFORM REQUIRED: DATA MIGRATION` block immediately before their
//! removal (or its manual-removal fallback, the fail-closed arm) —
//! an empty editable region with one commented `e.g.` line in real
//! SurrealQL shapes (never invented behavior) — where the user
//! writes the export/move query. The region's outer delimiters
//! follow the carrier ([`SlotStyle`]): the stdout script's
//! comment banners (`… <<<` / `… END <<<` — it runs today), or the
//! saved drafts' bare git-style resolution markers (`<<<<<<<
//! SURFORM SLOT (scope: family key)` / `>>>>>>> SURFORM SLOT END` —
//! an unresolved draft is deliberately un-runnable SurrealQL;
//! `crate::migrate` owns the file pipeline and `commit`'s marker
//! scan). On a `REMOVE TABLE` the WARNING
//! anchor and the slot stack: the alarm outermost, then the fill-in
//! region, then the statement (the owner removes keep their WARNING
//! anchors alone — the removal text already names the implied table and
//! document loss, so no clause was added). Every other family
//! (event/index/analyzer, the owner lines) is catalog or rebuild
//! machinery — no slot: the taxonomy keeps the script quiet where
//! nothing is lost. A Drift finding retypes or rewrites without
//! destroying any row's values (the rows keep them; the new kind
//! governs future writes), so it never slots in the removals half —
//! a field KIND change is the STAGED-migration sequence's trigger
//! instead (issue #65, below), and the report carries the corrected
//! note.
//!
//! The staged-migration sequence (issue #65, probe-pinned against
//! the embedded 3.3.0 engine 2026-10-11): a field kind change never
//! re-validates existing rows at define time, but a data step that
//! rewrites the values fails while the old kind is still in force —
//! and the failure names the existing row, which reads like a
//! define-time rejection (the issue's false premise). The workable
//! order is define-then-migrate, so a re-keyed table's scope frame
//! opens with the staged prelude: a relaxed shell (`DEFINE TABLE …
//! TYPE ANY SCHEMALESS` — schema mode and permissions drop to the
//! engine defaults: schemaless accepts every write, permissions
//! deny every non-privileged session), the re-keyed fields' kind
//! relaxes (`TYPE any` — a field definition survives a table
//! overwrite, so the old kind would otherwise still gate the data
//! step), and one REQUIRED data section per re-keyed table between
//! the prelude and the final defines. Shells and relaxes are typed
//! defines on the carrier; the sections are text, like every slot.
//! The staged table's own definition always rides the final defines
//! — a converged table (its only drift was the re-keyed child)
//! gets a locally fabricated restore finding, because the relaxed
//! shell must not be the last word. Views and lightweight relations
//! never stage (a view's AS SELECT is the whole definition; a
//! lightweight host refuses child defines). After the final
//! defines, a commented `SURFORM NOTE: INDEX REBUILD` block offers
//! each qualifying index's REMOVE + re-define (every UNIQUE index
//! on the table — its build is the one row-validating define — plus
//! every index whose fields name a re-keyed field): the user's
//! call, since a data section that touches indexed fields very
//! likely needs the rebuild.
//!
//! The slots are text: they cannot break the transaction or the
//! ordering machinery, and they derive from the findings exactly as
//! the statements do — the script stays a pure function of the
//! findings' multiset, the permutation pins covering the slots with
//! the statements. A slot's move target defined in the same script is
//! available by construction: a scope's defines precede its removals
//! inside the scope's own frame — where every slot rides — so a query
//! the user fills in can reference the SDL's new tables in its own
//! scope (the placement rule —
//! immediately before the orphaning statement — puts every slot after
//! every define naturally). The slots INHERIT their findings' truth:
//! a bogus finding yields a bogus slot — the #51 placement-blindness
//! class (a standalone-authored child mis-filed database-only) WAS
//! that source, fixed in the differ (compare_children defers a child
//! the SDL standalone manages to the SDL side's own twin path). The
//! collapse is the second presentation gap: a DatabaseOnly finding
//! in a scope the SDL never declares (CLI-reachable through the
//! version pair — a scope the TO file drops; the `--info` mode's
//! SDL-E3005 tag gate refuses it) is consumed by the collapsed-scope
//! emission (owner REMOVE + WARNING, no slot) while `report()` —
//! which takes no SDL — still marks its row (its manifest sentence
//! hedges between the two carriers; the marker remains the gap);
//! ledgered on the tracker (#56), not fixed here. And no
//! rename is ever auto-detected: even an obvious candidate (same
//! table, one field out, one field in) gets no automatic suggestion —
//! the human names the move, the slot names the stakes.
//!
//! The anchors' prose is the one place database-side names reach the
//! script as COMMENT text — and a backtick identifier may hold any
//! bytes (the vendored lexer accepts them all), a raw newline
//! included, which would end the `--` comment and execute the tail
//! as SurrealQL inside the script's transaction frames. Every name
//! interpolated into an emitted comment line therefore folds through
//! one shared sanitizer ([`comment_text`]): every line-comment
//! terminator — the control characters plus the non-control Unicode
//! line separators U+2028/U+2029 the vendored lexer also breaks on —
//! renders as its visible escape spelling (`\n` → the two-character
//! form), so the line stays one comment for any name. The
//! query-syntax `e.g.` spellings ride it too — belt-and-braces
//! over the backtick escaping (`ident_text`), which already folds
//! newlines but passes other control characters verbatim inside the
//! quotes. The header and the fixed-text anchors interpolate
//! nothing.
//!
//! The comments' dialect is ASCII-only
//! markdown — `-- ##` section headers (each under a
//! [`RULE`] run at the major boundaries: every scope frame and every
//! region inside it), `-- -` bullets in the guidance blocks, and one
//! blank line between every block (around headers and rules, around
//! every anchor/slot/note/warning block, between the frame's
//! regions, before each `COMMIT;`), with plain hyphens wherever the
//! old text ran an em-dash. Two wrap widths hold the shape: the
//! header block and its summary at [`HEADER_WIDTH`], the guidance
//! prose at [`PROSE_WIDTH`], both greedy with continuations indented
//! under their text ([`wrap_prose`]). Frozen byte-identity survives
//! the restyle where the pipeline consumes the bytes: the three
//! `SURFORM` anchor tokens, the git-style slot markers, the
//! `-- section:` label spellings (the retired frames' variant keeps
//! the `-- section: <label>` prefix and rewrites only its prose
//! tail), and — in `crate::migrate` — the metadata keys and the
//! ledger banner. Every interpolated name folds through
//! [`comment_text`] — the anchors', the slots', and the scope
//! labels' alike (the `## Scope` headers' and both `-- section:`
//! variants' — a backticked scope name
//! may carry a raw newline, and sane labels ride the fold
//! byte-identical).

use std::collections::{BTreeMap, BTreeSet};

use super::place::{
    raw_dep_names, raw_index_analyzer, raw_match_key, raw_on_table, raw_value_reads,
};
use crate::ast::{
    Config, Database, Executable, FieldPathSeg, Ident, IndexKind, Level, Namespace, SdlFile, Stmt,
    TableKind,
};
use crate::info::diff::{DriftClass, DriftFinding};
use crate::info::normalize::{Family, ScopeKey, classify, declared_scopes};
use crate::info::report::{count, family_word, scope_label};
use crate::info::staging::index_qualifies;
use crate::keywords::{escape_with, ident_needs_escape, quoted_string, word_shaped};

/// The script's fixed header — the short explanation block
/// (three logical lines: what
/// generated the script, the re-run safety, the marker vocabulary),
/// hard-wrapped at [`HEADER_WIDTH`] with four-space continuations;
/// the summary line closes the block through [`header_lines`]. The
/// rest of the per-scope-section shape rides the body; the owner
/// defines' honest clause (they ride outside every frame — issue
/// #81's overclaim) is the `## Scope` headers' and the summary's
/// business now. Two stanza classes are CONDITIONAL, riding only the
/// scripts that use them ([`header_lines`]): the multi-statement raw
/// re-run corner (most scripts hold no raws), and — riding the
/// staged prelude's own header, not here — the staged stanza (most
/// scripts stage nothing). `pub`: the test pins interpolate it, so a
/// header edit touches one
/// spelling.
pub const HEADER: &str = "\
-- generated by `surform migrate generate` - the
--    convergence script.
-- Re-runs are safe: OVERWRITE redefines; IF EXISTS
--    no-ops on absence.
-- Lines marked SURFORM WARNING destroy data;
--    SURFORM REQUIRED marks fill-in data-migration
--    slots; SURFORM NOTE lines are advice.
";

/// The horizontal rule at major boundaries — one per `##` section
/// header (the scope frames and the staged/defines/index/removals
/// regions): `-- ` plus a run of sixty hyphens, the comments'
/// ASCII-only dialect (no box-drawing, no non-ASCII
/// decorations anywhere in the emitted comments). `pub`: the test
/// pins interpolate it beside [`HEADER`].
pub const RULE: &str = "-- ------------------------------------------------------------";

/// The retired-scope frames' shared `##` title (the db-group and
/// namespace frames' removals header — a wording edit touches one
/// const, the same one-spelling rule [`HEADER`] rides).
const RETIRED_TITLE: &str = "-- ## Removals - retired scopes";

/// The removals regions' shared intro line (the live frames', the
/// retired frames', and the draft-group members' one spelling).
const REMOVALS_INTRO: &str = "-- Each removal is idempotent (IF EXISTS): a re-run skips it.\n";

/// The header block's wrap width — the short header
/// (and its summary line) wraps greedily at 52 columns, continuations
/// indented four spaces after `-- `; the exact spelling above is this
/// rule's output for the fixed explanation lines.
const HEADER_WIDTH: usize = 52;

/// The guidance prose's wrap width — wider than the header (the
/// sample guidance lines run to 61 columns), continuations
/// indented four spaces after `-- ` (bullets align under their text,
/// [`wrap_prose`]'s explicit-continuation form).
const PROSE_WIDTH: usize = 62;

/// Greedy word wrap for one logical comment line: `text` (no comment
/// prefix) wraps so no line exceeds `width` columns where single words
/// allow — the first line rides `first_prefix` (e.g. `-- ` or
/// `-- - `), every continuation rides `cont_prefix` (e.g. `--    ` for
/// prose, `--   ` for a bullet's aligned tail). The prefixes carry
/// their own trailing space, so a line's first word joins with none;
/// later words join with one. A word longer than the width rides its
/// own overlong line — never split (a folded hostile name stays one
/// wrap-word). The wrap tokenizes on whitespace, so a name's
/// WHITESPACE RUN collapses in comment prose while the statement line
/// keeps the exact spelling — advisory text vs the contract; the
/// divergence corner is ledgered (#104). One home for every wrapped
/// comment the generator emits: the summary line, the raw-corner
/// stanza, the region intros, the anchor and slot prose.
fn wrap_prose(text: &str, first_prefix: &str, cont_prefix: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = String::from(first_prefix);
    for word in text.split_whitespace() {
        let at_prefix_start = line == first_prefix || line == cont_prefix;
        if at_prefix_start {
            line.push_str(word);
        } else if line.len() + 1 + word.len() > width {
            out.push_str(&line);
            out.push('\n');
            line = String::from(cont_prefix);
            line.push_str(word);
        } else {
            line.push(' ');
            line.push_str(word);
        }
    }
    out.push_str(&line);
    out.push('\n');
    out
}

/// One blank separator line, unless the text already ends blank — the
/// whitespace policy's single blank never doubles (a blank that
/// follows a block meets the next block's own separator here). The
/// header block's closing blank is NOT this helper's business: the
/// summary is inserted after the walk, so the assembly normalizes it
/// at the insertion site.
fn push_blank(out: &mut String) {
    if !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// One `##` region header block: the rule, the title, then the intro
/// lines under a lone `--` separator, then the blank that closes the
/// block ([`push_blank`] — the sample block).
/// `intro` is the pre-wrapped intro lines (already rendered by
/// [`wrap_prose`], empty when the header carries none) pushed
/// verbatim.
fn region_header(out: &mut String, title: &str, intro: &str) {
    push_blank(out);
    out.push_str(RULE);
    out.push('\n');
    out.push_str(title);
    out.push('\n');
    if !intro.is_empty() {
        out.push_str("--\n");
        out.push_str(intro);
    }
    push_blank(out);
}

/// The header's conditional tail, after the fixed block: the raw
/// corner when a raw body rides the script (issue #81 — unconditional
/// stanzas rode every script; the MULTI-statement pairing fact is
/// stated as the general rule it is, so a single-statement raw body
/// overclaims nothing), then the run's summary line (issue #79's
/// script half — the six counts, the frame count included: the header
/// is assembled over the rendered body, so the script can know what
/// the report cannot). No trailing blank: the walk's first element
/// pushed its own separator against the old HEADER tail BEFORE this
/// text is inserted — the assembly normalizes the block's closing
/// blank at the insertion site.
fn header_lines(raw_bodies: bool, summary: &str) -> String {
    let mut out = String::new();
    if raw_bodies {
        out.push_str(&wrap_prose(
            "Raw-authored statements ride this script: a multi-statement raw \
             block re-pairs by its first header alone on a re-run, so its \
             unpaired tail statements re-drift (the known ledgered corner).",
            "-- ",
            "--    ",
            HEADER_WIDTH,
        ));
    }
    out.push_str(summary);
    out
}

/// The summary line's one home — the counts as the walk computed them,
/// spelled `finding(s) - defines, removals, data-migration slots,
/// staged tables, frames`, wrapped at [`HEADER_WIDTH`] like the rest
/// of the header. `pub`: the test pins and the CLI-side expectations
/// interpolate it, so a summary edit touches one spelling.
pub fn summary_block(
    findings: usize,
    defines: usize,
    removals: usize,
    slots: usize,
    staged: usize,
    frames: usize,
    unmanaged: usize,
) -> String {
    let unmanaged = if unmanaged > 0 {
        format!(
            ", {} (informational)",
            count("expected-unmanaged", unmanaged)
        )
    } else {
        String::new()
    };
    wrap_prose(
        &format!(
            "summary: {} - {}, {}, {}, {}, {}{}",
            count("finding", findings),
            count("define", defines),
            count("removal", removals),
            count("data-migration slot", slots),
            count("staged table", staged),
            count("frame", frames),
            unmanaged,
        ),
        "-- ",
        "--    ",
        HEADER_WIDTH,
    )
}

/// The REQUIRED slots' outer delimiters, per carrier: the
/// stdout script keeps the greppable `-- >>>` comment banners — its
/// scripts run today, so the region must stay runnable SurrealQL.
/// A SAVED draft (`generate --save`) delimits each unfilled region
/// with BARE git-style resolution marker lines instead — an
/// unresolved draft is deliberately un-runnable SurrealQL, and
/// `migrate commit` refuses while any marker remains. The guidance
/// and `e.g.` lines are comments in both styles, and the `SURFORM
/// WARNING` anchors are permanent documentation in both — never
/// markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotStyle {
    /// `-- >>> SURFORM REQUIRED: DATA MIGRATION <<<` … `-- >>>
    /// SURFORM REQUIRED: END <<<` (the stdout script's comments).
    CommentBanners,
    /// `<<<<<<< SURFORM SLOT (<scope>: <family> <key>)` …
    /// `>>>>>>> SURFORM SLOT END` (the saved draft's bare lines).
    ResolutionMarkers,
}

/// The convergence script's two carriers. `text` is the whole script
/// — the operator-facing artifact, byte-identical across every call
/// path. `statements` is the script's EXECUTABLE statements, in
/// emission order, captured AT EMISSION — the generator's own
/// statement boundaries, not a re-parse of the text: a `;` inside a
/// regex literal, string, or comment can never merge two statements,
/// because no pass ever looks for one. The apply flow (the Surrealism
/// module's in-database applier) executes ONE host call per element —
/// the host `sql` import parses ONE expression per call and silently
/// drops everything after the first, so the exact boundary is
/// load-bearing there; a textual re-splitter re-deriving these
/// boundaries from the rendered bytes failed repeatedly
/// (three merges, three fixes) — retired: the generator
/// marks, nothing re-parses. `raw_bodies` are the raw-authored
/// statements (README rule 8) — verbatim escapes whose boundaries are
/// the AUTHOR's knowledge, never marked, never executed by apply (the
/// module refuses them loud; the CLI's committed-file path runs them
/// through the engine's own `POST /sql` statement parser, which owns
/// that boundary).
///
/// Context lines (`USE`, `BEGIN TRANSACTION`, `COMMIT`) and comments
/// never enter `statements` — the emitter writes them to the text
/// only — so a consumer needs no head tests to skip them.
#[derive(Default)]
pub struct ConvergenceScript {
    /// The full script text (empty = converged).
    pub text: String,
    /// The executable statements, in emission order — the frames'
    /// defines, the scope owners, the removals.
    pub statements: Vec<String>,
    /// The raw-authored bodies that rode the text (rule 8), unmarked.
    pub raw_bodies: Vec<String>,
}

impl ConvergenceScript {
    /// One define's emission: the statement's canonical PIECES (no
    /// session-context line — [`crate::codegen::emit_statement_pieces`];
    /// a table carrying its slotted children prints one statement per
    /// member, so the pieces are the true boundaries) ride the script
    /// and the carrier together, one home. The raw escape hatch (rule
    /// 8) is the deliberate exception: its verbatim body rides the
    /// TEXT only — its statement boundaries are the AUTHOR's
    /// knowledge, which the generator does not mark — so it lands in
    /// `raw_bodies`, the list the apply flow refuses loud.
    fn push_define(&mut self, stmt: &Stmt) {
        self.push_define_noted(stmt, |_| None);
    }

    /// [`ConvergenceScript::push_define`] with an optional comment
    /// line above any piece (issue #80's UNIQUE-index hazard note —
    /// a note is TEXT, never a carrier element, and the piece
    /// boundaries stay exactly the generator's own). The raw escape
    /// hatch (rule 8) never takes a note: its verbatim body rides
    /// the TEXT only, its statement boundaries the AUTHOR's
    /// knowledge.
    fn push_define_noted(&mut self, stmt: &Stmt, note_of: impl Fn(&str) -> Option<String>) {
        if let Stmt::Raw(raw) = stmt {
            if !raw.body.is_empty() {
                self.text
                    .push_str(&crate::codegen::emit_statement_alone(stmt));
                self.raw_bodies.push(raw.body.clone());
            }
            return;
        }
        for piece in crate::codegen::emit_statement_pieces(stmt) {
            if let Some(note) = note_of(&piece) {
                self.text.push_str(&note);
            }
            self.text.push_str(&piece);
            self.statements.push(statement_of(&piece));
        }
    }

    /// One owner define's emission (namespace/database — the walks'
    /// context setters): the TEXT keeps the statement's
    /// session-context follower (a `namespace` define carries its own
    /// `USE NS` — codegen model item 1, the bytes `build` prints),
    /// while the carrier takes the statement alone — the `USE` line is
    /// the script's own scope machinery, never a carrier element. Raw
    /// never rides the owner walk (the content sort filters owner
    /// families); the carrier stays total regardless — text-only,
    /// `push_define`'s raw contract.
    fn push_define_with_context(&mut self, stmt: &Stmt) {
        self.text.push_str(&crate::codegen::emit_statement(stmt));
        match stmt {
            Stmt::Raw(raw) => {
                if !raw.body.is_empty() {
                    self.raw_bodies.push(raw.body.clone());
                }
            }
            _ => {
                let text = crate::codegen::emit_statement_alone(stmt);
                self.statements.push(statement_of(&text));
            }
        }
    }
}

/// A carrier element's spelling — the statement's text, edges
/// trimmed, its generator-written terminator consumed (the retired
/// splitter's elements ended before the `;`; the display and
/// comparison forms keep that shape). A piece that somehow ends
/// otherwise rides verbatim (total — never panics on generator
/// output).
fn statement_of(piece: &str) -> String {
    let trimmed = piece.trim();
    trimmed.strip_suffix(';').unwrap_or(trimmed).to_owned()
}

/// The convergence script over the comparator's findings. A pure
/// function of the findings' MULTISET: they partition into sorted sets
/// before any line renders, and a sort-key tie — the differ's keys are
/// unqualified, so two standalone fields named `x` on different hosts
/// both file under one (scope, family, key) — breaks on the finding's
/// canonical statement text (`define_order_text`/`removal_order_text`),
/// a total order whose only collisions are text-equal findings, where
/// the order is irrelevant. Any permutation of the same findings
/// renders byte-identically. Empty when there is nothing to converge
/// (no defines, no removals).
pub fn convergence_script(findings: &[DriftFinding], sdl: &SdlFile) -> String {
    convergence_script_parts(findings, sdl).text
}

/// [`convergence_script`] with the structured carrier — the same
/// walk, returning both halves ([`ConvergenceScript`]). The apply
/// flow's entry: the script text for the operator/ledger hash, the
/// marked statements for execution.
pub fn convergence_script_parts(findings: &[DriftFinding], sdl: &SdlFile) -> ConvergenceScript {
    convergence_script_styled_parts(findings, sdl, SlotStyle::CommentBanners)
}

/// [`convergence_script`] with the slots' outer delimiters chosen —
/// the saved drafts' marker-carrier entry point (every
/// other byte is shared with the stdout path).
pub fn convergence_script_styled(
    findings: &[DriftFinding],
    sdl: &SdlFile,
    style: SlotStyle,
) -> String {
    convergence_script_styled_parts(findings, sdl, style).text
}

/// The styled walk proper — [`convergence_script_styled_parts`] is
/// the one home every entry above rides.
fn convergence_script_styled_parts(
    findings: &[DriftFinding],
    sdl: &SdlFile,
    style: SlotStyle,
) -> ConvergenceScript {
    let (declared_ns, declared_db) = declared_scopes(sdl);

    // The defines: SdlOnly + Diverged, partitioned into a (scope,
    // family, key)-sorted set before rendering — the finding's
    // canonical statement text the tie-break. A child finding the
    // host table's own emission carries is dropped first — the table
    // renders the member (one finding, one line). The staged
    // restores (below) join BEFORE the filter: a restored host
    // carries its standalone children exactly like a found one.
    let mut all_defines: Vec<&DriftFinding> = findings
        .iter()
        .filter(|f| {
            matches!(
                f.class,
                DriftClass::SdlOnly { .. } | DriftClass::Drift { .. }
            )
        })
        .collect();

    // The staged set (issue #65): the tables the script re-keys
    // through the relaxed shell → REQUIRED data section → final
    // definitions sequence. The findings-only exclusion (views and
    // lightweight relations never stage) already ran in
    // `staged_tables`; this walk adds the SDL-side check the
    // findings cannot see — a table the SDL declares view or
    // lightweight but whose finding is absent (converged) would
    // otherwise stage on a re-keyed field child, and the relax
    // lines would fail the transaction (a lightweight host refuses
    // child defines). The walk runs over the EXPANDED file (the
    // differ's own lowering): mixin-spread-landed tables and indexes
    // are invisible to the raw file's `Spread` placeholders, and a
    // spread-landed UNIQUE index must reach the rebuild note exactly
    // like an authored one.
    let lowered = crate::expand::expand(sdl);
    let sdl_tables = sdl_declared_tables(&lowered);
    let unstagable = |scope: &ScopeKey, table: &str| {
        sdl_tables
            .get(&(scope.clone(), table.to_string()))
            .is_some_and(|stmt| match stmt {
                Stmt::Table(table) => {
                    table.kind.is_view()
                        || matches!(
                            table.kind,
                            crate::ast::TableKind::Relation {
                                lightweight: true,
                                ..
                            }
                        )
                }
                _ => false,
            })
    };
    let staged_set = crate::info::staging::staged_tables(findings);
    let staged: Vec<StagedEmission<'_>> = staged_set
        .iter()
        .filter(|entry| !unstagable(entry.scope, entry.table))
        .map(|entry| StagedEmission {
            entry,
            // The shell (and with it the restore) rides only for a
            // table the SDL DECLARES: an undeclared host (a standalone
            // field's `on:` naming a table no `table` statement holds)
            // is unmanaged database-side — resetting its schema mode
            // and permissions through the shell would clobber state
            // the file never owned. The kind relax line alone lifts
            // the old kind (an OVERWRITE on the field, table-safe);
            // the section and the slot ride unchanged.
            declared: sdl_tables.contains_key(&(entry.scope.clone(), entry.table.to_string())),
            index_notes: sdl_index_notes(&lowered, entry),
        })
        .collect();

    // The restores: a DECLARED staged table whose own definition
    // converged (its only drift was the re-keyed child) has no Table
    // finding, so no define would ride — and the shell would be the
    // last word: TYPE ANY and the dropped permissions would PERSIST
    // past the script (probe-pinned: a field definition survives a
    // table overwrite — and so does a relaxed table mode). The SDL's
    // own declaration rides instead (a local fabricated finding — the
    // report never sees it; the database side stands the shell, the
    // state the restore overwrites). Undeclared hosts take no shell
    // (above), so they need no restore.
    let mut restores: Vec<DriftFinding> = Vec::new();
    for emission in &staged {
        let entry = emission.entry;
        if !emission.declared {
            continue;
        }
        let has_finding = findings.iter().any(|f| {
            f.family == Family::Table
                && &f.scope == entry.scope
                && f.key == entry.table
                && matches!(
                    f.class,
                    DriftClass::SdlOnly { .. } | DriftClass::Drift { .. }
                )
        });
        if has_finding {
            continue;
        }
        let sdl_stmt = sdl_tables
            .get(&(entry.scope.clone(), entry.table.to_string()))
            .copied()
            .cloned()
            .expect("the declared flag gates the lookup");
        restores.push(DriftFinding {
            on_table_exists: true,
            scope: entry.scope.clone(),
            family: Family::Table,
            key: entry.table.to_string(),
            class: DriftClass::Drift {
                sdl: Box::new(sdl_stmt),
                database: Box::new(shell_stmt(entry.table)),
                markers: Box::default(),
            },
        });
    }
    all_defines.extend(restores.iter());

    let mut defines: Vec<(&DriftFinding, String)> = all_defines
        .iter()
        .copied()
        .filter(|f| !host_emission_carries(f, &all_defines))
        .map(|f| (f, define_order_text(f)))
        .collect();
    defines.sort_by(|a, b| {
        (&a.0.scope, a.0.family, a.0.key.as_str(), a.1.as_str()).cmp(&(
            &b.0.scope,
            b.0.family,
            b.0.key.as_str(),
            b.1.as_str(),
        ))
    });
    let defines: Vec<&DriftFinding> = defines.into_iter().map(|(f, _)| f).collect();

    // The removals: the database-only candidates.
    let candidates: Vec<&DriftFinding> = findings
        .iter()
        .filter(|f| matches!(f.class, DriftClass::DatabaseOnly { .. }))
        .collect();

    if defines.is_empty() && candidates.is_empty() {
        return ConvergenceScript::default();
    }

    // A scope collapses when the SDL never declares its owning
    // namespace/database: everything in it is unmanaged, and the one
    // REMOVE implies its children. Reachable through the library API
    // (a hand-built read model) and the version pair (a scope the TO
    // file drops). The `--info` mode's tag validation
    // (SDL-E3005) can never ingest an undeclared scope, and that is
    // the data-loss guard where the database side is real: a
    // CLI-reachable `REMOVE DATABASE` behind a typo'd tag would
    // destroy a live database. The version pair has no typed tags —
    // both sides are loaded files — so the collapse + WARNING
    // mechanism carries the data-loss role there (the E3005
    // decision).
    let collapses = |scope: &ScopeKey| match scope {
        ScopeKey::Root => false,
        ScopeKey::Namespace(ns) => !declared_ns.contains(ns),
        ScopeKey::Database(ns, db) => !declared_db.contains(&(ns.clone(), db.clone())),
    };

    // Owner removals, collected from the collapsed scopes and the
    // owner-family findings, deduped into sorted sets. An owner
    // finding whose scope the SDL DOES declare is the key-mismatch
    // corner (a mangled INFO key) — fail-closed manual-removal:
    // removing an SDL-managed namespace/database behind a mangled
    // key would be data loss.
    let mut db_removes: BTreeSet<(String, String)> = BTreeSet::new();
    let mut ns_removes: BTreeSet<String> = BTreeSet::new();
    let mut owner_manual: BTreeSet<String> = BTreeSet::new();
    for finding in &candidates {
        match (&finding.scope, finding.family) {
            (ScopeKey::Database(ns, db), _) if collapses(&finding.scope) => {
                db_removes.insert((ns.clone(), db.clone()));
            }
            (ScopeKey::Namespace(ns), _) if collapses(&finding.scope) => {
                ns_removes.insert(ns.clone());
            }
            (ScopeKey::Namespace(ns), Family::Database) => {
                if declared_db.contains(&(ns.clone(), finding.key.clone())) {
                    owner_manual.insert(format!(
                        "-- manual removal required: {} database {} - the schema file \
                         declares it, so this is an INFO key mismatch, not an \
                         unmanaged database",
                        comment_text(&scope_label(&finding.scope)),
                        comment_text(&finding.key)
                    ));
                } else {
                    db_removes.insert((ns.clone(), finding.key.clone()));
                }
            }
            (ScopeKey::Root, Family::Namespace) => {
                if declared_ns.contains(&finding.key) {
                    owner_manual.insert(format!(
                        "-- manual removal required: root namespace {} - the schema \
                         file declares it, so this is an INFO key mismatch, not an \
                         unmanaged namespace",
                        comment_text(&finding.key)
                    ));
                } else {
                    ns_removes.insert(finding.key.clone());
                }
            }
            _ => {}
        }
    }

    // The collapse arm's consumed findings (issue #85): the content
    // findings (non-owner families) in collapsed scopes — the rows
    // the single owner REMOVE implies and the script names in its
    // WARNING block, one `-- includes` line each. Owner-family
    // findings ARE the removes (never listed as consumed).
    let mut consumed: BTreeMap<ScopeKey, Vec<String>> = BTreeMap::new();
    for finding in &candidates {
        if !collapses(&finding.scope) {
            continue;
        }
        if matches!(finding.family, Family::Namespace | Family::Database) {
            continue;
        }
        let edge = match &finding.class {
            DriftClass::DatabaseOnly { on_table, .. } => on_table
                .as_deref()
                .map(|table| format!(" ON {}", comment_text(table)))
                .unwrap_or_default(),
            _ => String::new(),
        };
        consumed
            .entry(finding.scope.clone())
            .or_default()
            .push(format!(
                "-- includes {} {}{} (the schema file no longer \
                 declares this {})",
                family_word(finding.family),
                comment_text(&finding.key),
                edge,
                match finding.scope {
                    ScopeKey::Database(..) => "database",
                    _ => "namespace",
                }
            ));
    }
    // The lines sort with their scope's content: the script stays a
    // pure function of the findings' MULTISET (the caller's input
    // order must not survive into the bytes).
    for lines in consumed.values_mut() {
        lines.sort();
    }

    // Content findings — everything but the owner families, in
    // non-collapsed scopes — sorted (scope, rank, family, key).
    // Within one scope `Family`'s alphabetical order is
    // children-first (Event/Field/Index precede Table), with ONE
    // catalog-integrity override: an ANALYZER removes after the
    // tables and indexes that reference it (upstream refuses
    // `REMOVE ANALYZER` while a fulltext index still points at it —
    // `AzInUse`, which `IF EXISTS` does not soften). So the
    // within-scope ranks are children (0) < Table (1) < Analyzer
    // (2): a removal whose target is referenced always follows its
    // references.
    let removal_rank = |family: Family| match family {
        Family::Analyzer => 2,
        Family::Table => 1,
        _ => 0,
    };
    let mut content: Vec<(&DriftFinding, String)> = candidates
        .iter()
        .copied()
        .filter(|f| !matches!(f.family, Family::Namespace | Family::Database))
        .filter(|f| !collapses(&f.scope))
        .map(|f| (f, removal_order_text(f)))
        .collect();
    content.sort_by(|a, b| {
        let (ra, rb) = (removal_rank(a.0.family), removal_rank(b.0.family));
        (&a.0.scope, ra, a.0.family, a.0.key.as_str(), a.1.as_str()).cmp(&(
            &b.0.scope,
            rb,
            b.0.family,
            b.0.key.as_str(),
            b.1.as_str(),
        ))
    });
    let content: Vec<&DriftFinding> = content.into_iter().map(|(f, _)| f).collect();

    // The UNIQUE-index apply-time hazards (issue #80): the emitted
    // unique index defines whose host table exists database-side —
    // the one row-validating define (probe-pinned). Walked over the
    // FULL define set (the host-carried children included: their
    // notes ride the host emission's pieces), keyed by (scope, the
    // finding's own emitted statement — the exact bytes the piece
    // spells).
    let hazards = unique_index_hazards(&all_defines, &staged);

    let mut script = ConvergenceScript::default();
    // The header: the fixed block, then the conditional tail. The
    // raw-corner stanza rides only when a raw body reaches the text
    // (a raw-authored define anywhere — a finding's statement, or a
    // raw member nested in a table's authored block); the summary
    // line inserts after the walk (the frame count reads the
    // rendered bytes), at the recorded offset.
    let any_raw = defines
        .iter()
        .chain(candidates.iter())
        .any(|finding| carries_raw(finding));
    script.text.push_str(HEADER);
    let summary_offset = script.text.len();
    let mut session = Session::default();
    // 1. The root section's frame — no `USE` precedes it (a
    //    root-scoped param defines under the runner's own session,
    //    the one session-bound root family, and the rest of the root
    //    families are session-independent here; a root param can
    //    never be a REMOVAL, so the removals half needs no context
    //    either).
    let root_defines = ordered_content(&defines, &ScopeKey::Root, &[]);
    let root_removals: Vec<&DriftFinding> = content
        .iter()
        .filter(|f| f.scope == ScopeKey::Root)
        .copied()
        .collect();
    emit_frame(
        &mut script,
        &ScopeKey::Root,
        &root_defines,
        &root_removals,
        &staged,
        &hazards,
        style,
    );

    // 2. The namespace owner defines — the context setters, OUTSIDE
    //    every frame (each emission carries its `USE NS`, and a USE
    //    line never sits inside a transaction), in the root session,
    //    sorted — the defines walk's own order. `session.adopt`
    //    records what each emission established.
    let root_namespaces: Vec<String> = defines
        .iter()
        .filter(|f| f.scope == ScopeKey::Root && f.family == Family::Namespace)
        .map(|f| f.key.clone())
        .collect();
    for ns in &root_namespaces {
        emit_define(
            &mut script,
            &ScopeKey::Root,
            Family::Namespace,
            ns,
            &defines,
        );
        session.adopt(&ScopeKey::Namespace(ns.clone()));
    }

    // 3. Per namespace: its database owner defines (outside the
    //    frame — under the namespace's session, before the
    //    namespace's content, the defines walk's own order), then the
    //    namespace's frame (its content defines, then its removals),
    //    then each database's frame. The stakeholders are the UNION
    //    of the defines' and the removals' scopes, so a
    //    removals-only namespace still gets its section; frames
    //    without content skip themselves.
    let mut ns_names: BTreeSet<String> = BTreeSet::new();
    for finding in defines.iter().chain(content.iter()) {
        match &finding.scope {
            ScopeKey::Namespace(ns) | ScopeKey::Database(ns, _) => {
                ns_names.insert(ns.clone());
            }
            ScopeKey::Root => {}
        }
    }
    for ns in &ns_names {
        let ns_scope = ScopeKey::Namespace(ns.clone());
        let db_owners: Vec<String> = defines
            .iter()
            .filter(|f| f.scope == ns_scope && f.family == Family::Database)
            .map(|f| f.key.clone())
            .collect();
        let ns_defines = ordered_content(&defines, &ns_scope, &[namespace_stmt(ns)]);
        let ns_removals: Vec<&DriftFinding> = content
            .iter()
            .filter(|f| f.scope == ns_scope)
            .copied()
            .collect();
        // The namespace context opens only when the namespace holds
        // something of its own — a db-content-only namespace skips
        // straight to the db sections, whose `enter` spells the full
        // `USE NS … DB …`.
        if !ns_defines.is_empty() || !ns_removals.is_empty() || !db_owners.is_empty() {
            session.enter(&mut script.text, &ns_scope);
        }
        for db in &db_owners {
            emit_define(&mut script, &ns_scope, Family::Database, db, &defines);
        }
        emit_frame(
            &mut script,
            &ns_scope,
            &ns_defines,
            &ns_removals,
            &staged,
            &hazards,
            style,
        );
        // Each database's frame under its own session context.
        let mut dbs: BTreeSet<String> = BTreeSet::new();
        for finding in defines.iter().chain(content.iter()) {
            if let ScopeKey::Database(scope_ns, db) = &finding.scope
                && scope_ns == ns
            {
                dbs.insert(db.clone());
            }
        }
        for db in &dbs {
            let db_scope = ScopeKey::Database(ns.clone(), db.clone());
            session.enter(&mut script.text, &db_scope);
            let db_defines = ordered_content(
                &defines,
                &db_scope,
                &[namespace_stmt(ns), database_stmt(db)],
            );
            let db_removals: Vec<&DriftFinding> = content
                .iter()
                .filter(|f| f.scope == db_scope)
                .copied()
                .collect();
            emit_frame(
                &mut script,
                &db_scope,
                &db_defines,
                &db_removals,
                &staged,
                &hazards,
                style,
            );
        }
    }

    // 4. The `REMOVE DATABASE` lines — grouped per namespace, each
    //    group its own frame under the namespace's session context
    //    (the `USE` outside it), after every content frame they
    //    imply.
    let mut db_groups: Vec<(&String, Vec<&String>)> = Vec::new();
    for (ns, db) in &db_removes {
        match db_groups.last_mut() {
            Some((group_ns, dbs)) if *group_ns == ns => dbs.push(db),
            _ => db_groups.push((ns, vec![db])),
        }
    }
    for (ns, dbs) in db_groups {
        if ns.is_empty() {
            // The pseudo-namespace scope: no USE spelling exists
            // (codegen's own posture) — an arbitrary namespace
            // applies.
            script.text.push_str(
                "-- pseudo-namespace scope: no USE spelling exists; an arbitrary \
                 namespace applies\n",
            );
        } else {
            session.enter(&mut script.text, &ScopeKey::Namespace((*ns).clone()));
        }
        region_header(&mut script.text, RETIRED_TITLE, REMOVALS_INTRO);
        script.text.push_str("BEGIN TRANSACTION;\n");
        for db in dbs {
            let scope = ScopeKey::Database((*ns).clone(), db.clone());
            script.text.push_str(&retired_section_label(&scope));
            let consumed = consumed.get(&scope).map(Vec::as_slice).unwrap_or(&[]);
            database_warning(&mut script.text, db, consumed);
            push_removal_line(
                &mut script,
                &format!("REMOVE DATABASE IF EXISTS {};", ident_text(db)),
            );
        }
        push_blank(&mut script.text);
        script.text.push_str("COMMIT;\n");
    }
    // 5. `REMOVE NAMESPACE` lines last — after every database they
    //    imply. Root-based, session-independent (source-verified);
    //    their own frame.
    if !ns_removes.is_empty() {
        region_header(&mut script.text, RETIRED_TITLE, REMOVALS_INTRO);
        script.text.push_str("BEGIN TRANSACTION;\n");
        for ns in &ns_removes {
            let scope = ScopeKey::Namespace(ns.clone());
            script.text.push_str(&retired_section_label(&scope));
            let consumed = consumed.get(&scope).map(Vec::as_slice).unwrap_or(&[]);
            namespace_warning(&mut script.text, ns, consumed);
            push_removal_line(
                &mut script,
                &format!("REMOVE NAMESPACE IF EXISTS {};", ident_text(ns)),
            );
        }
        push_blank(&mut script.text);
        script.text.push_str("COMMIT;\n");
    }
    // 6. The fail-closed owner corners, reported where the owner
    //    passes would have run — comments only, no frame.
    for line in &owner_manual {
        script.text.push_str(line);
        script.text.push('\n');
    }
    // 7. The summary (issue #79's script half): the six counts —
    //    findings, defines, removals, slots, staged tables, frames —
    //    inserted after the header block (the offset recorded before
    //    the walk). The frame count reads the rendered bytes, the
    //    same anchor `convergence_script_frames` and the file
    //    pipeline use. The counts are the SCRIPT's own: defines is
    //    the defines half (host-carried children render inside their
    //    hosts, fabricated restores ride), slots EXCLUDE the collapsed
    //    scopes (their obligations die with the owner REMOVE — the
    //    includes lines name them), staged is post-SDL-filter — so a
    //    number can legitimately differ from the report's same-named
    //    count, and the counts need not sum (the defines half is
    //    post-filter). Expected-unmanaged findings carry no work;
    //    when present they ride the informational clause, as in the
    //    report's line.
    let frames = script
        .text
        .lines()
        .filter(|line| *line == "COMMIT;")
        .count();
    let slots = candidates
        .iter()
        .filter(|f| !collapses(&f.scope) && super::report::required_slot(f).is_some())
        .count();
    let unmanaged = findings
        .iter()
        .filter(|f| matches!(f.class, DriftClass::ExpectedUnmanaged { .. }))
        .count();
    let summary = summary_block(
        findings.len(),
        defines.len(),
        candidates.len(),
        slots,
        staged.len(),
        frames,
        unmanaged,
    );
    let header_tail = header_lines(any_raw, &summary);
    script.text.insert_str(summary_offset, &header_tail);
    // Exactly one blank line closes the header block: the first body
    // element's own separator (`push_blank`) ran BEFORE this insert —
    // against the old HEADER tail — so it already sits here when the
    // first element is a frame, and is missing when it is a bare USE
    // or owner define. The summary's last byte is its own line's
    // newline, so one MORE newline here is the blank; normalize at
    // the insertion site.
    let after = summary_offset + header_tail.len();
    if !script.text[after..].starts_with('\n') {
        script.text.insert(after, '\n');
    }
    script
}

/// One removal's emission: the generator's own line rides the script
/// and the carrier together, one home — the mirror of
/// [`ConvergenceScript::push_define`]. The line's trailing newline is
/// the text's; the carrier's element is the trimmed statement.
fn push_removal_line(script: &mut ConvergenceScript, line: &str) {
    script.text.push_str(line);
    script.text.push('\n');
    script.statements.push(statement_of(line));
}

/// The script's transaction-frame count — one per scope section with
/// batchable content — counted from the rendered bytes, anchored the
/// way the file pipeline anchors them (`last_commit_line`'s exact
/// `COMMIT;` spelling; the slot regions carry only comments and
/// markers in an UNFILLED script, so no other line can spell it).
/// The load-bearing number for the draft writers: the migration file
/// format wraps exactly ONE frame per file (`parse_draft` refuses a
/// second block), so a script whose findings span several scope
/// sections cannot ride `generate --save` / `revert` — those refuse
/// before any file lands, and the script stays a stdout/module
/// artifact (both apply multi-frame bodies fine) — and a zero-frame
/// script (namespace/database owner defines only) has no frame for
/// the format to wrap.
pub fn convergence_script_frames(findings: &[DriftFinding], sdl: &SdlFile) -> usize {
    convergence_script(findings, sdl)
        .lines()
        .filter(|line| *line == "COMMIT;")
        .count()
}

// ---------------------------------------------------------------------
// The scope sections
// ---------------------------------------------------------------------

/// One scope section's transaction frame: the scope's `USE` context
/// is already established OUTSIDE it (the walk's `session.enter` or
/// an owner define's emission), the frame opens with its `##` header
/// block — rule, `## Scope <label>`, the frozen `-- section:` label
/// beneath (issue #82 — scope ownership was inferable only by tracing
/// USE lines; the label's spelling is the greppable contract, kept
/// byte-identical under
/// the `## Scope` header) — and the frame wraps the scope's staged prelude
/// (the relaxed shells, the relax lines, the REQUIRED data sections —
/// issue #65's sequence, always before the final defines), its
/// defines (the compiler's emission order — a UNIQUE-index hazard
/// note riding above any hazardous piece, issue #80), its index
/// rebuild notes, then its removals — a failed statement rolls this
/// section back and leaves the sections before it applied. Each
/// region opens with its own `##` header block, every block rides
/// blank-separated, and one blank line closes the frame before
/// `COMMIT;` (the whitespace policy). A section with neither half
/// emits nothing. The frame's own lines (`BEGIN`/`COMMIT`, the
/// section comments) ride the text only — never the structured
/// carrier.
fn emit_frame(
    script: &mut ConvergenceScript,
    scope: &ScopeKey,
    defines: &[Stmt],
    removals: &[&DriftFinding],
    staged: &[StagedEmission<'_>],
    hazards: &BTreeMap<(ScopeKey, String), String>,
    style: SlotStyle,
) {
    if defines.is_empty() && removals.is_empty() {
        return;
    }
    let staged: Vec<&StagedEmission<'_>> = staged
        .iter()
        .filter(|emission| emission.entry.scope == scope)
        .collect();
    push_blank(&mut script.text);
    script.text.push_str(RULE);
    script.text.push('\n');
    // The scope label folds through [`comment_text`] like every name
    // interpolation: a backticked scope
    // name may carry a raw newline, and the unfolded tail would start
    // its own column-0 line inside the header block. Sane labels ride
    // the fold byte-identical (the full-text pins hold the literals).
    let label = comment_text(&scope_label(scope));
    script.text.push_str(&format!("-- ## Scope {label}\n"));
    script.text.push_str(&format!(
        "-- section: {label} ({}, {})\n",
        count("define", defines.len()),
        count("removal", removals.len()),
    ));
    push_blank(&mut script.text);
    script.text.push_str("BEGIN TRANSACTION;\n");
    if !staged.is_empty() {
        emit_staged_prelude(script, &staged, style);
    }
    if !defines.is_empty() {
        region_header(
            &mut script.text,
            "-- ## Defines - the state the schema file describes",
            "-- Each define carries OVERWRITE: a re-run redefines it.\n",
        );
        for stmt in defines {
            let note_of = |piece: &str| hazard_note(hazards, scope, piece).map(str::to_string);
            script.push_define_noted(stmt, note_of);
        }
    }
    emit_index_notes(script, &staged);
    if !removals.is_empty() {
        region_header(
            &mut script.text,
            "-- ## Removals - objects the schema file no longer declares",
            REMOVALS_INTRO,
        );
        for finding in removals {
            emit_finding(script, finding, style);
        }
    }
    push_blank(&mut script.text);
    script.text.push_str("COMMIT;\n");
}

// ---------------------------------------------------------------------
// The staged-migration prelude (issue #65)
// ---------------------------------------------------------------------

/// One staged table's script emission: the findings' entry plus the
/// qualifying SDL-side indexes (the rebuild note's payload), gathered
/// once — the SDL walk the frame's defines cannot do (a staged table
/// whose own define is converged carries no finding, so its nested
/// indexes never reach the frame).
struct StagedEmission<'a> {
    entry: &'a crate::info::staging::StagedTable<'a>,
    /// Whether the SDL declares the table (the shell's and the
    /// restore's gate — an undeclared host is unmanaged
    /// database-side, and the script touches nothing it never
    /// owned).
    declared: bool,
    /// The SDL-side index statements the rebuild note offers (empty
    /// when none qualify).
    index_notes: Vec<&'a Stmt>,
}

/// The qualifying indexes for one staged table, from the SDL's
/// declarations (nested members and standalone findings on the
/// table): every UNIQUE index on it, plus every index whose fields
/// name a re-keyed field ([`index_qualifies`]). Scope-aware — a
/// same-named table at another scope contributes nothing.
fn sdl_index_notes<'a>(
    sdl: &'a SdlFile,
    entry: &crate::info::staging::StagedTable<'a>,
) -> Vec<&'a Stmt> {
    let rekeyed: Vec<String> = entry
        .rekeys
        .iter()
        .map(|finding| finding.key.clone())
        .collect();
    let mut walk = crate::info::normalize::ScopeWalk::new();
    let mut out = Vec::new();
    for stmt in &sdl.items {
        let scope = walk.lookup_scope(stmt);
        match stmt {
            Stmt::Table(table) if table.name.raw == entry.table && &scope == entry.scope => {
                out.extend(
                    table
                        .nested
                        .iter()
                        .filter(|nested| index_qualifies(nested, entry.table, &rekeyed)),
                );
            }
            one if &scope == entry.scope && index_qualifies(one, entry.table, &rekeyed) => {
                // A standalone index finding: its scope IS the walk's
                // (the differ pairs it at the same lookup).
                out.push(one);
            }
            _ => {}
        }
        walk.advance(stmt);
    }
    out
}

/// The SDL's declared tables by (scope, name) — the staged walk's
/// exclusion source (view/lightweight never stage), the restores'
/// authored shapes, and the shells' declared gate (an undeclared
/// host takes neither shell nor restore).
fn sdl_declared_tables(sdl: &SdlFile) -> BTreeMap<(ScopeKey, String), &Stmt> {
    let mut walk = crate::info::normalize::ScopeWalk::new();
    let mut out = BTreeMap::new();
    for stmt in &sdl.items {
        let scope = walk.lookup_scope(stmt);
        if let Stmt::Table(table) = stmt {
            out.insert((scope, table.name.raw.clone()), stmt);
        }
        walk.advance(stmt);
    }
    out
}

/// The staged prelude, before the final defines: the relaxed shells
/// (`DEFINE TABLE … TYPE ANY SCHEMALESS` — schema mode and permissions
/// drop to the engine defaults: schemaless accepts every write,
/// permissions deny every non-privileged session, so the relaxed
/// window is both writable by the migration and closed to everyone
/// else) — SDL-declared tables only (an undeclared host is unmanaged
/// database-side; the shell would clobber state the file never
/// owned) — then the kind relaxes (`TYPE any` — probe-pinned: a field
/// definition SURVIVES a table overwrite, so the old kind would
/// otherwise still gate the data step), then one REQUIRED data
/// section per re-keyed table. The shells and relaxes ride the
/// carrier (typed defines); the sections are text, like every slot.
fn emit_staged_prelude(
    script: &mut ConvergenceScript,
    staged: &[&StagedEmission<'_>],
    style: SlotStyle,
) {
    // The header states only what the shape carries: a re-keying
    // staging names the data steps AND the engine-order contract
    // (issue #81 — the stanza once rode every script's header; most
    // scripts stage nothing); a shell-only one (a table-level drift
    // with no rekeys — a permissions change) names the relax alone
    // (advice text must not premise
    // sections that do not exist).
    let any_rekeys = staged
        .iter()
        .any(|emission| !emission.entry.rekeys.is_empty());
    let prose = |text: &str| wrap_prose(text, "-- ", "--    ", PROSE_WIDTH);
    let bullet = |text: &str| wrap_prose(text, "-- - ", "--   ", PROSE_WIDTH);
    if any_rekeys {
        let intro = format!(
            "{}{}{}{}{}",
            prose(
                "The shells below drop schema modes and permissions to the \
                   engine defaults: schemaless, permissions denied."
            ),
            prose("The kind relaxes below lift the old field kinds."),
            prose("A data step fails while the old kind stands."),
            prose("The final defines re-validate no rows."),
            bullet(
                "Write each data step between its markers; it runs before \
                    the final defines, in this same transaction."
            ),
        );
        region_header(
            &mut script.text,
            "-- ## Staged migration - the re-keyed tables take a data step",
            &intro,
        );
    } else {
        let intro = format!(
            "{}{}",
            prose(
                "The shells below drop schema modes and permissions to the \
                   engine defaults: schemaless, permissions denied."
            ),
            prose("The defines below carry the final state, in this same transaction."),
        );
        region_header(
            &mut script.text,
            "-- ## Staged migration - the shells relax the old definitions",
            &intro,
        );
    }
    for emission in staged {
        let entry = emission.entry;
        if emission.declared {
            let shell = shell_stmt(entry.table);
            script.push_define(&shell);
        }
        for finding in &entry.rekeys {
            let DriftClass::Drift { sdl, .. } = &finding.class else {
                continue;
            };
            let Stmt::Field(field) = sdl.as_ref() else {
                continue;
            };
            let relax = relaxed_field_stmt(field, entry.table);
            script.push_define(&relax);
        }
    }
    for emission in staged {
        if emission.entry.rekeys.is_empty() {
            continue;
        }
        staged_slot(script, emission, style);
    }
}

/// The relaxed shell define: `DEFINE TABLE OVERWRITE <t> TYPE ANY
/// SCHEMALESS;` — a minimal statement the SDL never declared (the one
/// deliberate exception to the defines region's "the state the schema
/// file describes" title; the final defines below restore the authored
/// shapes).
fn shell_stmt(table: &str) -> Stmt {
    Stmt::Table(crate::ast::Table {
        name: Ident::new(table.to_string()),
        kind: crate::ast::TableKind::Any,
        schema_full: Some(false),
        drop: false,
        changefeed: None,
        inline_edges: None,
        inline_references: None,
        permissions: None,
        graphql: None,
        nested: Vec::new(),
        comment: None,
        comments: Vec::new(),
        trailing_comments: Vec::new(),
        clause_comments: Vec::new(),
    })
}

/// A kind-changed field's relax define: the name, the table, `TYPE
/// any` — the loosest kind, since the old kind still stands
/// database-side and the data step's new values would violate it.
/// The `on:` fallback (a nested child always carries its host — the
/// parser's fold fills it, and the differ runs on the expanded file)
/// is defense in depth, not a shape the findings produce. Minimal
/// BY CONSTRUCTION — every clause is the default, none clones: a
/// `default:`/`value:` clause would FIRE on the data step's own
/// writes (a `value:` overwrites the author's migration data),
/// `assert:` would constrain it, `readonly;` would reject it
/// outright, `flexible;` demands an object-containing kind (the
/// engine rejects it on `any` — parse error), and every clause the
/// final state wants re-applies with the final define anyway.
/// (The struct-update form leaked all of these.)
fn relaxed_field_stmt(field: &crate::ast::Field, table: &str) -> Stmt {
    Stmt::Field(crate::ast::Field {
        name: field.name.clone(),
        kind: Some(crate::ast::Kind::new("any")),
        on: if field.on.raw.is_empty() {
            Ident::new(table.to_string())
        } else {
            field.on.clone()
        },
        default: None,
        value: None,
        assert: None,
        computed: None,
        readonly: false,
        flexible: false,
        inline: false,
        reference: None,
        permissions: None,
        graphql: None,
        comment: None,
        comments: Vec::new(),
        trailing_comments: Vec::new(),
        clause_comments: Vec::new(),
    })
}

/// The REQUIRED data section for one re-keyed table: the fill-in
/// region rides BETWEEN the relaxed prelude and the final defines —
/// the query the user writes runs under the relaxed kinds and before
/// the final definitions, the order the engine demands (probe-pinned:
/// the data step fails under the old kind; the define re-validates
/// nothing). The prose names the re-keyed fields and their NEW kinds
/// (issue #84 — the reader no longer looks them up in the relax
/// lines). Marker-gated like every slot: the saved draft is
/// un-committable until the region is filled or deleted.
///
/// The `e.g.` lines are real SurrealQL (the executed
/// legs run them — never invented behavior), one per spellable
/// re-keyed field with an engine-verified spelling:
/// - a `record<...>` new kind names the record table's
///   invented-but-runnable id (`record<seat>` → `seat:r1` — the
///   table-slot's `archive` precedent);
/// - a plain scalar new kind uses the engine's own cast spelling
///   `<kind> path` — probe-pinned against the embedded 3.3.0 engine
///   2026-10-11 (string, int, float, number, bool, datetime all
///   parse, execute, and convert; non-castable values fail loudly).
///   The fn-call spellings the issue suggested (`string::from(age)`
///   et al.) are probe-rejected — the engine answers `Parse error:
///   Invalid function/constant path` for each of them.
///
/// Any other shape (option<...>, array<...>, a non-spellable path)
/// drops the example — the prose carries the stakes, the human
/// writes the step.
fn staged_slot(script: &mut ConvergenceScript, emission: &StagedEmission<'_>, style: SlotStyle) {
    let entry = emission.entry;
    // The "staged" word disambiguates the marker name from a removal
    // slot's `{scope}: table {key}` — a kind-changed standalone field
    // on a table the SDL drops would otherwise name two REQUIRED
    // regions identically.
    let name = comment_text(&format!(
        "{}: staged table {}",
        scope_label(entry.scope),
        entry.table
    ));
    push_blank(&mut script.text);
    slot_open(&mut script.text, &name, style);
    script.text.push_str(&format!(
        "-- The definitions above relax {} for this data step.\n",
        comment_text(entry.table)
    ));
    let mut examples: Vec<String> = Vec::new();
    let mut fields: Vec<String> = Vec::new();
    for finding in &entry.rekeys {
        let DriftClass::Drift { sdl, .. } = &finding.class else {
            continue;
        };
        let Stmt::Field(field) = sdl.as_ref() else {
            continue;
        };
        let kind = field.kind.as_ref().map(|kind| kind.text.as_str());
        fields.push(format!(
            "`{}` -> {}",
            comment_text(&finding.key),
            // The kind spelling rides a `--` comment: a quoted ident
            // inside a `record<...>` kind may carry NEL/LS/PS (the
            // vendored line-comment eater terminates at all five
            // spellings), so the kind folds like every prose-line
            // name — the raw tail would execute inside the frame.
            comment_text(kind.unwrap_or("(absent)"))
        ));
        if !field_path_spellable(&finding.key, sdl) {
            continue;
        }
        let path = field_key_text(&finding.key);
        let table_spelled = comment_text(&ident_text(entry.table));
        match kind {
            // The probe-verified scalar cast spellings (issue #84).
            Some(kind @ ("string" | "int" | "float" | "number" | "bool" | "datetime")) => {
                examples.push(format!(
                    "-- e.g. UPDATE {table_spelled} SET {path} = <{kind}> {path};\n"
                ));
            }
            // The record shape: the new kind's
            // record table names the example's target.
            Some(kind) => {
                let inner = kind.strip_prefix("record<").and_then(|rest| {
                    rest.split(['>', ' ', '|'])
                        .find(|word| !word.is_empty() && word_shaped(word))
                });
                if let Some(inner) = inner {
                    examples.push(format!(
                        "-- e.g. UPDATE {table_spelled} SET {path} = {}:r1;\n",
                        comment_text(&ident_text(inner)),
                    ));
                }
            }
            None => {}
        }
    }
    if !fields.is_empty() {
        script
            .text
            .push_str(&format!("-- Re-keyed fields: {}.\n", fields.join(", ")));
    }
    script.text.push_str(&wrap_prose(
        "Write the data step between the markers; it runs after the relaxes \
         and before the final defines, in this same transaction.",
        "-- - ",
        "--   ",
        PROSE_WIDTH,
    ));
    script
        .text
        .push_str("-- - Delete the region when no data moves.\n");
    for example in &examples {
        script.text.push_str(example);
    }
    slot_close(&mut script.text, style);
}

/// The index-rebuild notes, after the final defines: the data
/// sections may rewrite fields these indexes cover, and a stale
/// index serves wrong answers until rebuilt (the user's call — the
/// block ships commented out). Qualifying (gathered per staged
/// table in [`sdl_index_notes`]): every UNIQUE index on the table
/// (its build also FAILS over colliding rows — the one
/// row-validating define, probe-pinned — and the SDL's own define
/// re-applies it, so the note doubles as the dedupe reminder), plus
/// every index whose fields name a re-keyed field. SDL-side indexes
/// only: a database-only index the SDL drops is removed by the
/// script's removals half anyway.
fn emit_index_notes(script: &mut ConvergenceScript, staged: &[&StagedEmission<'_>]) {
    // The qualifying emissions — a shell-only staging (no rekeys — no
    // data section) skips the block: its premise ("the data steps
    // above may rewrite fields") would be false, and with nothing
    // rewritten neither the stale-index nor the UNIQUE-over-duplicates
    // hazard arises.
    let qualified: Vec<&StagedEmission<'_>> = staged
        .iter()
        .copied()
        .filter(|emission| !emission.index_notes.is_empty() && !emission.entry.rekeys.is_empty())
        .collect();
    if qualified.is_empty() {
        return;
    }
    region_header(
        &mut script.text,
        "-- ## Index rebuilds - the data steps above may stale them",
        "",
    );
    for emission in qualified {
        let entry = emission.entry;
        push_blank(&mut script.text);
        script
            .text
            .push_str("-- >>> SURFORM NOTE: INDEX REBUILD <<<\n");
        script.text.push_str(&wrap_prose(
            "The data steps above may rewrite fields these indexes cover. A \
             stale index serves wrong answers until rebuilt. A unique index \
             define fails when two rewritten rows collide.",
            "-- ",
            "--    ",
            PROSE_WIDTH,
        ));
        script.text.push_str(&wrap_prose(
            "Uncomment the lines that apply; edit them to the final shapes.",
            "-- - ",
            "--   ",
            PROSE_WIDTH,
        ));
        // The pair is a COMMENTED runnable statement — the user
        // uncomments it. [`ident_text`]'s escape folds LF/CR to their
        // two-char spellings (comment-safe), but NEL/LS/PS ride
        // verbatim: a raw terminator would end the comment and
        // execute the tail inside the frame. The gate omits \n — the
        // remove line's is escape_with-folded, and `lines()` cannot
        // emit one in a define line. Folding to the `\u{…}` display
        // spelling WOULD round-trip (the vendored backtick unescape
        // decodes the braced form — lex_common_escape_sequence's `u`
        // arm), but it would fork a second ident-spelling rule beside
        // [`ident_text`]'s one-escaping-rule-one-home contract, for an
        // advisory note. The fail-closed shape is the slot examples'
        // (`field_path_spellable`): the pair drops for a spelling the
        // comment cannot carry, one prose line (once) points at the
        // schema file, and the note's other indexes stay
        // uncommentable.
        let mut unspellable = false;
        for stmt in &emission.index_notes {
            let Stmt::Index(index) = stmt else {
                continue;
            };
            // The staged entry's table is the honest spelling either
            // way (a nested member's `on:` already carries the host).
            let remove = format!(
                "-- REMOVE INDEX IF EXISTS {} ON {};",
                ident_text(&index.name.raw),
                ident_text(entry.table)
            );
            let define: Vec<String> = crate::codegen::emit_statement_pieces(stmt)
                .iter()
                .flat_map(|piece| piece.lines())
                .map(|line| format!("-- {line}"))
                .collect();
            let spellable = std::iter::once(remove.as_str())
                .chain(define.iter().map(String::as_str))
                .all(|line| !line.contains(['\r', '\u{85}', '\u{2028}', '\u{2029}']));
            if !spellable {
                unspellable = true;
                continue;
            }
            script.text.push_str(&remove);
            script.text.push('\n');
            for line in define {
                script.text.push_str(&line);
                script.text.push('\n');
            }
        }
        if unspellable {
            script.text.push_str(&wrap_prose(
                "An index here carries a name with a line break; a comment \
                 cannot spell it. The schema file holds the definition; \
                 remove and redefine the index by hand after the data step.",
                "-- ",
                "--    ",
                PROSE_WIDTH,
            ));
        }
        script.text.push_str("-- >>> SURFORM NOTE: END <<<\n");
    }
}

/// One scope's content defines in the COMPILER's emission order: the
/// findings' statements ride the same stable topological sort `build`
/// emits under (`order::emission_order` — the emission-order sort on the lowered
/// stream, authored order the tie-break; the partition's (family,
/// key) order is the authored stand-in), with the member→host rule
/// (the module doc) applied first: each standalone child of a host
/// that defines in this scope slots inside that host's emission, and
/// each raw-authored member rides there through its header's `ON`.
/// After the sort, the raw-edge settle pass re-orders what neither
/// the partition nor `emission_order` can see (the module doc's
/// second addition). Scope-leader statements are synthesized so the
/// sort's scope walk
/// and section runs match the real stream, then dropped from the
/// result — never re-defined: only a finding defines.
fn ordered_content(defines: &[&DriftFinding], scope: &ScopeKey, leaders: &[Stmt]) -> Vec<Stmt> {
    let mut content: Vec<Stmt> = defines
        .iter()
        .filter(|f| &f.scope == scope)
        // The owner families render as their section's context
        // setters (the caller's loops), never inside the sort.
        .filter(|f| !matches!(f.family, Family::Namespace | Family::Database))
        .map(|f| define_stmt(f).clone())
        .collect();
    if content.is_empty() {
        return content;
    }
    slot_children_into_their_hosts(&mut content);
    // The raw-authored members come out of the list (which is in
    // sorted-set order here, so the raws keep that order among
    // themselves) — they cannot join a table's `nested` members
    // (codegen's table arm prints index/event/field only), so each
    // rides its host's emission as its own statement, re-inserted
    // below between the head and the members. A raw whose header
    // carries no `ON <table>`, or whose host defines no finding here,
    // stays in the list and keeps its sort position (the fail-closed
    // posture the removals tail's manual branches share).
    let raw_hosts: Vec<Option<String>> = content
        .iter()
        .map(|stmt| raw_member_host(stmt, &content))
        .collect();
    let mut raw_members: Vec<(String, Stmt)> = Vec::new();
    let mut kept: Vec<Stmt> = Vec::with_capacity(content.len());
    for (stmt, host) in content.drain(..).zip(raw_hosts) {
        match host {
            Some(host) => raw_members.push((host, stmt)),
            None => kept.push(stmt),
        }
    }
    content = kept;
    let mut file = SdlFile {
        items: leaders.to_vec(),
        trailing_comments: Vec::new(),
    };
    file.items.append(&mut content);
    crate::order::emission_order(&mut file);
    file.items.drain(..leaders.len());
    settle_raw_body_edges(&mut file.items);
    unfold_raw_members(&mut file.items, raw_members);
    file.items
}

/// The member→host rule: a standalone child (Field/Event/Index) whose
/// host table ALSO defines in this content joins the host's nested
/// members, so the host's `DEFINE TABLE` head runs first (a LIGHTWEIGHT
/// host refuses the child's define; the module doc) and every member
/// that references the child finds it defined — a nested index naming
/// the child hard-fails `FdNotFound` on a SCHEMAFULL table that lacks
/// it, and the table statement and its members emit as ONE unit, so
/// no order of separate statements satisfies both edges: the child
/// must ride inside. The intra-table sort (`emission_order`, next in
/// `ordered_content`) then orders each host's joined members by the
/// frozen edges — fields before the indexes that name them — with the
/// children's sorted-set order as the tie-break. A child whose host
/// carries no define finding — absent (the engine auto-creates the
/// schemaless shell) or converged — keeps its sort position. The rule
/// reads the child's typed `on:`; the raw-authored twins ride their
/// header's `ON <table>` edge instead (`raw_member_host` +
/// `unfold_raw_members` — they cannot join `nested`, codegen's table
/// arm prints index/event/field members only).
fn slot_children_into_their_hosts(content: &mut Vec<Stmt>) {
    // Each child's host-table name, computed before any move: the
    // host must stand in this content (one finding per key, so a
    // name matches at most one table).
    let slot_host: Vec<Option<String>> = content
        .iter()
        .map(|stmt| {
            standalone_host(stmt).and_then(|host| {
                content
                    .iter()
                    .any(|other| table_name(other) == Some(host))
                    .then(|| host.to_string())
            })
        })
        .collect();
    if slot_host.iter().all(Option::is_none) {
        return;
    }
    let mut slotted: Vec<(String, Stmt)> = Vec::new();
    let mut kept: Vec<Stmt> = Vec::with_capacity(content.len());
    for (stmt, host) in content.drain(..).zip(slot_host) {
        match host {
            Some(host) => slotted.push((host, stmt)),
            None => kept.push(stmt),
        }
    }
    // Each host's children join its nested members in their original
    // (sorted-set) relative order; the intra-table sort places them.
    for (host, child) in slotted {
        kept.iter_mut()
            .find_map(|stmt| match stmt {
                Stmt::Table(table) if table.name.raw == host => Some(table),
                _ => None,
            })
            .expect("the host stood in the content")
            .nested
            .push(child);
    }
    *content = kept;
}

/// A standalone child's host-table name — the `on:` edge the
/// structured member forms carry; `None` for every other family (the
/// raw-authored members' edge is [`raw_member_host`]'s).
fn standalone_host(stmt: &Stmt) -> Option<&str> {
    match stmt {
        Stmt::Field(field) => Some(&field.on.raw),
        Stmt::Event(event) => Some(&event.on.raw),
        Stmt::Index(index) => Some(&index.on.raw),
        _ => None,
    }
}

/// A table statement's name; `None` for every other family.
fn table_name(stmt: &Stmt) -> Option<&str> {
    match stmt {
        Stmt::Table(table) => Some(&table.name.raw),
        _ => None,
    }
}

/// A raw-authored define's host-table name — the `ON <table>` its
/// header carries (place.rs's `raw_on_table`: a field body's ON
/// follows its path, an index/event body's rides the placement
/// dependency; the name compares against the typed table findings'
/// keys, so a spelling the body escapes differently simply fails to
/// match and keeps today's position) — when the named table stands in
/// this content. `None` is the fail-closed posture: a body whose
/// header never parses to a family and key, a family without ON
/// grammar (a raw table; a view's FROM list is a placement
/// dependency, never an ON edge), or a host absent (auto-create) or
/// converged — the raw keeps its sort position, the same posture the
/// removals tail's manual branches take.
fn raw_member_host(stmt: &Stmt, content: &[Stmt]) -> Option<String> {
    let raw = match stmt {
        Stmt::Raw(raw) => raw,
        _ => return None,
    };
    let host = raw_on_table(&raw.body)?;
    content
        .iter()
        .any(|other| table_name(other) == Some(host.as_str()))
        .then_some(host)
}

/// The define-time edges neither the defines partition nor
/// `emission_order` can see: every edge with a raw body on either end
/// (the module doc's second addition). A raw body registers nothing
/// in `emission_order`'s dependency registry, so the edges it CARRIES
/// and the edges that point INTO it are both blind spots:
///
/// - a view's source tables — a typed view's `TableKind::View` FROM
///   list (order.rs's own read: `refs::view_select` parses the canon,
///   the `FROM` targets are the sources its define walks) or a raw
///   view body's `AS SELECT … FROM` list (place.rs's `raw_dep_names`)
///   — over a source that DEFINES in this content, whatever parity
///   the source defines in (a raw source is the blind target; a typed
///   source of a raw view is the blind carrier);
/// - a raw fulltext index's `ANALYZER` clause (place.rs's
///   `raw_index_analyzer`) over an analyzer that defines in this
///   content;
/// - the value-read edges (order.rs's frozen rows 6–13) — a typed
///   reader's targets, which `emission_order` resolves only against
///   typed definitions (a raw-defined param/fn/module is the blind
///   target), and a raw reader's slots (place.rs's `raw_value_reads`),
///   which register nowhere at all — over a target that defines in
///   this content either way.
///
/// Each raw reader walks EVERY statement of its body — a raw block
/// renders contiguously (one finding, one verbatim statement), so the
/// union of its statements' edges is the body's edge set, wherever in
/// the block each edge-carrying statement stands (place.rs reads per
/// statement; a block that fails to parse entirely reads no edge).
///
/// The pass orders every enforced edge dependency-first: it builds
/// the dependency graph over the content's positions, prunes the
/// edges that close a cycle (a reader↔reader cycle — two raw params
/// reading each other — is unorderable; the edges drop, the positions
/// stand, and the migration fails visibly at the engine), then emits
/// the stable topological order — repeatedly the earliest statement
/// whose dependencies all emitted, the current order the tie-break.
/// A satisfied input is the identity, every enforced edge lands
/// dependency-before-reader, and each round places one statement, so
/// the pass terminates structurally; the prune removes the only class
/// that could stall a round (a typed cycle is E0015's business, but
/// the raw bodies' reads are invisible to it — the generator must not
/// hang where the checker never ran). An unreadable body carries no
/// edge, never a guess (the fail-closed posture: the reader keeps its
/// sort position).
fn settle_raw_body_edges(items: &mut Vec<Stmt>) {
    let edges: Vec<Vec<(Family, String)>> = items.iter().map(raw_body_edges).collect();
    // The dependency graph over positions: each statement's in-content
    // dependency positions, deduplicated, self-edges dropped (a
    // statement never violates against itself).
    let mut deps: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    for (at, edge_list) in edges.iter().enumerate() {
        for dep in edge_list {
            if let Some(target) = defines_key(items, dep)
                && target != at
                && !deps[at].contains(&target)
            {
                deps[at].push(target);
            }
        }
    }
    prune_cycle_edges(&mut deps);
    // The stable topological emission (Kahn, lowest position first).
    let mut remaining: Vec<usize> = deps.iter().map(Vec::len).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    for (at, ds) in deps.iter().enumerate() {
        for &dep in ds {
            dependents[dep].push(at);
        }
    }
    let mut placed = vec![false; items.len()];
    let mut old: Vec<Option<Stmt>> = items.drain(..).map(Some).collect();
    let mut out: Vec<Stmt> = Vec::with_capacity(old.len());
    for _ in 0..old.len() {
        let pick = (0..old.len()).find(|&i| !placed[i] && remaining[i] == 0);
        let pick = match pick {
            Some(pick) => pick,
            // Unreachable after the prune (the pruned graph is a
            // DAG); force the lowest remaining index so the sort
            // stays total regardless.
            None => (0..old.len())
                .find(|&i| !placed[i])
                .expect("statements remain"),
        };
        placed[pick] = true;
        for &dependent in &dependents[pick] {
            remaining[dependent] -= 1;
        }
        out.push(old[pick].take().expect("each statement emits once"));
    }
    *items = out;
}

/// Drops the edges that close a cycle — both ends in one strongly
/// connected component: an unorderable reader cycle cannot be
/// enforced, so its edges drop and the statements keep their
/// positions (the fail-closed posture; see [`settle_raw_body_edges`]).
/// Tarjan's SCC walk over the content's graph — a migration's
/// content is tens of nodes, so the recursion is bounded by the node
/// count.
fn prune_cycle_edges(deps: &mut [Vec<usize>]) {
    struct Walk<'a> {
        deps: &'a [Vec<usize>],
        indices: Vec<Option<usize>>,
        low: Vec<usize>,
        on_stack: Vec<bool>,
        ids: Vec<usize>,
        stack: Vec<usize>,
        counter: usize,
    }

    impl Walk<'_> {
        fn visit(&mut self, at: usize) {
            self.indices[at] = Some(self.counter);
            self.low[at] = self.counter;
            self.counter += 1;
            self.stack.push(at);
            self.on_stack[at] = true;
            for &dep in &self.deps[at] {
                match self.indices[dep] {
                    None => {
                        self.visit(dep);
                        self.low[at] = self.low[at].min(self.low[dep]);
                    }
                    Some(index) if self.on_stack[dep] => {
                        self.low[at] = self.low[at].min(index);
                    }
                    Some(_) => {}
                }
            }
            if self.low[at] == self.indices[at].unwrap() {
                let id = self.counter;
                while let Some(node) = self.stack.pop() {
                    self.on_stack[node] = false;
                    self.ids[node] = id;
                    if node == at {
                        break;
                    }
                }
                self.counter += 1;
            }
        }
    }

    let mut walk = Walk {
        deps,
        indices: vec![None; deps.len()],
        low: vec![0; deps.len()],
        on_stack: vec![false; deps.len()],
        ids: vec![usize::MAX; deps.len()],
        stack: Vec::new(),
        counter: 0,
    };
    for root in 0..deps.len() {
        if walk.indices[root].is_none() {
            walk.visit(root);
        }
    }
    let ids = walk.ids;
    for (at, ds) in deps.iter_mut().enumerate() {
        ds.retain(|&dep| ids[at] != ids[dep]);
    }
}

/// One statement's settle-pass edges — the dependencies whose
/// placement this pass enforces (see [`settle_raw_body_edges`]).
/// Typed statements contribute only the view's FROM list (their other
/// slots ride `emission_order`); a raw body contributes the UNION of
/// its statements' edges — dep names, analyzer clauses, and value-slot
/// reads wherever they stand in the (possibly multi-statement) block:
/// the block renders contiguously, so the union is the body's edge
/// set, and first-header classification (the differ's model) gates
/// nothing here. The fail-closed posture: a body that fails to parse
/// entirely carries no edge, never a guess. A dependency is enforced
/// only when its target defines in this content — anything else is
/// either `emission_order`'s resolved business, converged state
/// (already in the catalog), or broken authorship no ordering
/// repairs.
fn raw_body_edges(stmt: &Stmt) -> Vec<(Family, String)> {
    match stmt {
        Stmt::Table(table) => match &table.kind {
            TableKind::View { query } => view_source_tables(&query.canon)
                .into_iter()
                .map(|name| (Family::Table, name))
                .collect(),
            _ => Vec::new(),
        },
        Stmt::Raw(raw) => {
            let mut edges = raw_dep_names(&raw.body);
            edges.extend(
                raw_index_analyzer(&raw.body)
                    .into_iter()
                    .map(|name| (Family::Analyzer, name)),
            );
            edges.extend(raw_value_reads(&raw.body));
            edges
        }
        _ => typed_value_reads(stmt),
    }
}

/// A TYPED statement's rows 6–13 value reads — order.rs's
/// `stmt_edges` value arms mirrored (order.rs itself untouched; the
/// migration needs the reads even where order.rs's registry cannot
/// resolve them — a raw-defined target).
/// The extractor is the typed rows' own (`refs::capture_param_fn_refs`
/// over each slot's canon), so the names are the definition
/// spellings. Only the VALUE classes: a view's FROM list and an
/// index's analyzer/fields keep their own frozen-row arms.
fn typed_value_reads(stmt: &Stmt) -> Vec<(Family, String)> {
    let mut reads = Vec::new();
    match stmt {
        Stmt::Param(param) => {
            if let Some(value) = &param.value {
                capture_value_reads(&value.canon, &mut reads);
            }
        }
        Stmt::Sequence(sequence) => {
            for expr in [&sequence.start, &sequence.batch, &sequence.timeout]
                .into_iter()
                .flatten()
            {
                capture_value_reads(&expr.canon, &mut reads);
            }
        }
        Stmt::User(user) => {
            for expr in [&user.token_duration, &user.session_duration]
                .into_iter()
                .flatten()
            {
                capture_value_reads(&expr.canon, &mut reads);
            }
        }
        Stmt::Bucket(bucket) => {
            if let Some(backend) = &bucket.backend {
                capture_value_reads(&backend.canon, &mut reads);
            }
        }
        Stmt::Access(access) => {
            jwt_source_reads(&access.kind, &mut reads);
            if let Some(duration) = &access.duration {
                for expr in [&duration.grant, &duration.token, &duration.session]
                    .into_iter()
                    .flatten()
                {
                    capture_value_reads(&expr.canon, &mut reads);
                }
            }
        }
        Stmt::Config(config) => match config {
            Config::Default(default) => {
                for expr in [&default.namespace, &default.database]
                    .into_iter()
                    .flatten()
                {
                    capture_value_reads(&expr.canon, &mut reads);
                }
            }
            Config::Api(api) => middleware_arg_reads(&api.middleware, &mut reads),
            Config::Graphql(_) => {}
        },
        Stmt::Api(api) => {
            for action in &api.actions {
                middleware_arg_reads(&action.config.middleware, &mut reads);
            }
            if let Some(any) = &api.any {
                middleware_arg_reads(&any.config.middleware, &mut reads);
            }
        }
        // No define-time value slot (the frozen table's other rows).
        _ => {}
    }
    reads
}

/// One value slot's Param/Fn/Module reads (order.rs's `capture_edges`
/// read mirrored).
fn capture_value_reads(canon: &str, reads: &mut Vec<(Family, String)>) {
    let (params, fns, modules) = crate::refs::capture_param_fn_refs(canon);
    reads.extend(params.into_iter().map(|name| (Family::Param, name)));
    reads.extend(fns.into_iter().map(|name| (Family::Function, name)));
    reads.extend(modules.into_iter().map(|name| (Family::Module, name)));
}

/// The jwt source's define-time slots (row 9) — order.rs's
/// `jwt_source_edges` read mirrored: the verify url/key and the
/// issuer key. The audience list is string literals only (the
/// licensed deviation); signup/signin/authenticate/context are
/// runtime-evaluated (row 10).
fn jwt_source_reads(kind: &crate::ast::AccessKind, reads: &mut Vec<(Family, String)>) {
    let source = match kind {
        crate::ast::AccessKind::Jwt { source } => Some(source),
        crate::ast::AccessKind::Record { jwt, .. } | crate::ast::AccessKind::Bearer { jwt, .. } => {
            jwt.as_ref()
        }
    };
    let Some(source) = source else {
        return;
    };
    match &source.verify {
        crate::ast::JwtVerify::Url(expr) => capture_value_reads(&expr.canon, reads),
        crate::ast::JwtVerify::Key { key, .. } => capture_value_reads(&key.canon, reads),
    }
    if let Some(issuer) = &source.issuer
        && let Some(key) = &issuer.key
    {
        capture_value_reads(&key.canon, reads);
    }
}

/// The middleware args' slots (row 12) — the fn NAMES are stored
/// unvalidated (no edge to them, the frozen table's record).
fn middleware_arg_reads(middleware: &[crate::ast::Middleware], reads: &mut Vec<(Family, String)>) {
    for entry in middleware {
        for arg in &entry.args {
            capture_value_reads(&arg.canon, reads);
        }
    }
}

/// The view's top `FROM` tables — the engine's own define-validated
/// set, order.rs's `view_from_tables` read mirrored (`refs::view_select`
/// parses the canon; the `FROM` targets are the sources the engine's
/// define walks). An unreadable canon carries no edge, never a guess.
fn view_source_tables(canon: &str) -> Vec<String> {
    let Some(select) = crate::refs::view_select(canon) else {
        return Vec::new();
    };
    select
        .what
        .iter()
        .filter_map(|target| match target {
            surrealdb_sql::Expr::Table(name) => Some(name.as_str().to_string()),
            _ => None,
        })
        .collect()
}

/// Where the content defines the (family, key) a settle edge names —
/// the typed classification, or a raw body's header match key — as a
/// position (`None` when nothing here defines it).
fn defines_key(items: &[Stmt], dep: &(Family, String)) -> Option<usize> {
    items.iter().position(|stmt| match stmt {
        Stmt::Raw(raw) => {
            raw_match_key(&raw.body).is_some_and(|(family, key)| family == dep.0 && key == dep.1)
        }
        other => classify(other) == (dep.0, dep.1.clone()),
    })
}

/// Re-inserts the raw-authored members into their hosts' emissions.
/// A table carrying them unfolds into the emission's parts as
/// separate statements — the bare head, then the raw field/event
/// members (no define-time edges; right behind the head, where the
/// host's `OVERWRITE` has already cleared a LIGHTWEIGHT form), then
/// the typed members with the raw index members spliced in after the
/// last typed field (a fulltext index refuses a SCHEMAFULL table
/// missing a field it names — vendored define/index.rs's
/// `FdNotFound`; typed or raw, every field it could name is defined
/// by then). Each part renders byte-identically to its slice of the
/// one-unit emission: codegen's nested loop calls the same printers
/// the top-level arms do, and the members carry their
/// parser-injected `on:`.
fn unfold_raw_members(items: &mut Vec<Stmt>, raw_members: Vec<(String, Stmt)>) {
    if raw_members.is_empty() {
        return;
    }
    let mut pending = raw_members;
    let mut out: Vec<Stmt> = Vec::with_capacity(items.len() + pending.len());
    for stmt in items.drain(..) {
        let mut mine: Vec<Stmt> = Vec::new();
        if let Some(host) = table_name(&stmt).map(str::to_string) {
            let (matched, rest): (Vec<_>, Vec<_>) =
                pending.into_iter().partition(|(name, _)| *name == host);
            pending = rest;
            mine = matched.into_iter().map(|(_, stmt)| stmt).collect();
        }
        if mine.is_empty() {
            out.push(stmt);
            continue;
        }
        let Stmt::Table(mut table) = stmt else {
            unreachable!("only a table statement matches a raw member's host key")
        };
        let mut nested = std::mem::take(&mut table.nested);
        out.push(Stmt::Table(table));
        // The raws carry a classifiable header by construction (the
        // host edge exists only for field/event/index) — the indexes
        // wait for the typed fields, the leaves ride behind the head.
        let (raw_indexes, raw_leaves): (Vec<Stmt>, Vec<Stmt>) = mine
            .into_iter()
            .partition(|stmt| {
                matches!(
                    stmt,
                    Stmt::Raw(raw)
                        if raw_match_key(&raw.body).is_some_and(|(family, _)| family == Family::Index)
                )
            });
        out.extend(raw_leaves);
        let splice = nested
            .iter()
            .rposition(|member| matches!(member, Stmt::Field(_)))
            .map_or(0, |at| at + 1);
        let tail: Vec<Stmt> = nested.drain(splice..).collect();
        out.extend(nested);
        out.extend(raw_indexes);
        out.extend(tail);
    }
    *items = out;
}

/// Whether a standalone child finding's host table also defines in
/// the same scope WITH the child nested — the table's emission
/// carries the member (codegen's table arm prints its nested
/// definitions), so the standalone render would duplicate it. The
/// typed `on:` names the host; the finding keys match the nested
/// members' classification exactly (the differ paired them that way).
fn host_emission_carries(child: &DriftFinding, defines: &[&DriftFinding]) -> bool {
    if !matches!(child.family, Family::Field | Family::Event | Family::Index) {
        return false;
    }
    let host = match define_stmt(child) {
        Stmt::Field(field) => &field.on.raw,
        Stmt::Event(event) => &event.on.raw,
        Stmt::Index(index) => &index.on.raw,
        _ => return false, // a raw-authored define: placement is its own
    };
    defines.iter().any(|candidate| {
        candidate.scope == child.scope
            && candidate.family == Family::Table
            && &candidate.key == host
            && match define_stmt(candidate) {
                Stmt::Table(table) => table
                    .nested
                    .iter()
                    .any(|member| classify(member) == (child.family, child.key.clone())),
                _ => false,
            }
    })
}

/// A define finding's authored statement (SdlOnly and Diverged both
/// carry the SDL side by construction — the sentinel note).
fn define_stmt(finding: &DriftFinding) -> &Stmt {
    match &finding.class {
        DriftClass::SdlOnly { sdl } | DriftClass::Drift { sdl, .. } => sdl,
        _ => unreachable!("the caller filters to the define classes"),
    }
}

/// The defines' sort-key tail — the finding's canonical statement
/// text, the exact bytes the script renders for it. Two findings can
/// share one (scope, family, key) — the differ's keys are
/// unqualified, so two standalone fields named `x` on different hosts
/// tie — and the text is the total order that cannot collide
/// usefully: text-equal findings render identically, so their
/// relative order is irrelevant to the bytes.
fn define_order_text(finding: &DriftFinding) -> String {
    crate::codegen::emit_statement(define_stmt(finding))
}

/// The removals' tie-break text — the database side's canonical
/// statement, the define classes' mirror (a deterministic function of
/// the finding, colliding only where the order is irrelevant).
fn removal_order_text(finding: &DriftFinding) -> String {
    match &finding.class {
        DriftClass::DatabaseOnly { database, .. } => crate::codegen::emit_statement(database),
        _ => unreachable!("the caller filters to DatabaseOnly"),
    }
}

/// One scope-owner define's rendered `DEFINE … OVERWRITE` line(s),
/// found by (scope, family, key) in the defines set. The TEXT keeps
/// the statement's session-context follower (a `namespace` define
/// carries its own `USE NS` — codegen model item 1, the bytes `build`
/// prints), while the carrier takes the statement alone
/// ([`ConvergenceScript::push_define`]'s contract): the `USE` line is
/// the script's own scope machinery, never a carrier element.
fn emit_define(
    script: &mut ConvergenceScript,
    scope: &ScopeKey,
    family: Family,
    key: &str,
    defines: &[&DriftFinding],
) {
    let Some(finding) = defines
        .iter()
        .find(|f| &f.scope == scope && f.family == family && f.key == key)
    else {
        return;
    };
    script.push_define_with_context(define_stmt(finding));
}

/// A synthesized `namespace` scope leader (the sort's walk context;
/// dropped before rendering).
fn namespace_stmt(ns: &str) -> Stmt {
    Stmt::Namespace(Namespace {
        name: Ident::new(ns.to_string()),
        keyword: Default::default(),
        comment: None,
        comments: Vec::new(),
        clause_comments: Vec::new(),
    })
}

/// A synthesized `database` scope leader (ditto).
fn database_stmt(db: &str) -> Stmt {
    Stmt::Database(Database {
        name: Ident::new(db.to_string()),
        keyword: Default::default(),
        strict: false,
        changefeed: None,
        comment: None,
        comments: Vec::new(),
        trailing_comments: Vec::new(),
        clause_comments: Vec::new(),
    })
}

// ---------------------------------------------------------------------
// The removals (one finding's render)
// ---------------------------------------------------------------------

/// One finding's removal render — WARNING anchor and REQUIRED slot
/// per the taxonomy, then the `REMOVE` line (or its fail-closed
/// manual-removal comment). The section's `USE` context is the
/// frame's, established by the walk before `BEGIN` — never here. The
/// anchors and slot banners ride the text only; the `REMOVE` line is
/// a generator-marked statement ([`push_removal_line`]).
fn emit_finding(script: &mut ConvergenceScript, finding: &DriftFinding, style: SlotStyle) {
    let DriftClass::DatabaseOnly {
        database, on_table, ..
    } = &finding.class
    else {
        return; // the caller filters to DatabaseOnly
    };
    // The anchors (the taxonomy — `report.rs`'s
    // `required_slot` is the one home): a TABLE carries the WARNING
    // alarm and the REQUIRED slot (the alarm outermost, then the
    // fill-in region, then the statement); a FIELD carries the slot
    // alone (catalog-only removal, orphaned values). Every other
    // family carries neither — the script stays quiet where nothing
    // is lost. The FIELD arm's `on_table` gate lives in
    // `required_slot` itself — the if-let here is only
    // the extraction, fail-closed if the invariant ever broke. The
    // slot NAME (the markers' parenthetical, the saved drafts'
    // `commit` refusal vocabulary) carries the ON edge when the
    // finding has one (issue #86 — the manifest spells
    // `field legacy_note ON user`; the marker is the manifest's
    // line, not a trimmed one — grep needs no mental edit), and folds
    // through [`comment_text`] so any database-side name keeps the
    // marker one line.
    let edge = on_table
        .as_deref()
        .map(|table| format!(" ON {table}"))
        .unwrap_or_default();
    let slot_name = comment_text(&format!(
        "{}: {} {}{edge}",
        scope_label(&finding.scope),
        family_word(finding.family),
        finding.key
    ));
    match super::report::required_slot(finding) {
        Some(super::report::SlotKind::Table) => {
            table_warning(&mut script.text, &finding.key);
            table_slot(&mut script.text, &finding.key, &slot_name, style);
        }
        Some(super::report::SlotKind::Field) => {
            if let Some(table) = on_table.as_deref() {
                field_slot(
                    &mut script.text,
                    &finding.key,
                    table,
                    database,
                    &slot_name,
                    style,
                );
            }
        }
        None => {}
    }
    match remove_line(finding.family, &finding.key, on_table.as_deref(), database) {
        Some(line) => push_removal_line(script, &line),
        None => {
            push_blank(&mut script.text);
            let mut note = format!(
                "-- manual removal required: {} {} {}",
                comment_text(&scope_label(&finding.scope)),
                family_word(finding.family),
                comment_text(&finding.key)
            );
            // An empty match key's producer — a header-less raw body
            // (its first line) or an anonymous-silo module (its
            // executable's spelling) — stands in (comment-safe; the
            // report row quotes the same hint), so the note still
            // names WHAT to remove. The keyless form trims before the
            // hint joins —
            // no double space; every other None arm keeps the plain
            // trim — no trailing space in the comment.
            if let Some(hint) = empty_key_hint(finding.family, &finding.key, database) {
                note = format!("{} - {hint}", note.trim_end());
            }
            script.text.push_str(note.trim_end());
            script.text.push('\n');
        }
    }
}

/// The WARNING anchor above a `REMOVE TABLE`: the engine deletes the
/// table's key range — the documents go with it (remove/table.rs's
/// `del_tb`; a view defined on it or an edge-holding lightweight
/// relation refuses first). Field/event/index removals are
/// catalog-only and carry no anchor. The blank line opens the block
/// (the whitespace policy); the slot region below carries its own.
fn table_warning(out: &mut String, table: &str) {
    push_blank(out);
    out.push_str("-- >>> SURFORM WARNING: DATA LOSS <<<\n");
    out.push_str(&wrap_prose(
        &format!(
            "Removes table {} - every document in it is deleted.",
            comment_text(table)
        ),
        "-- ",
        "--    ",
        PROSE_WIDTH,
    ));
}

/// The WARNING anchor above a `REMOVE DATABASE`: the whole subtree.
/// The collapse arm's consumed findings append one `-- includes`
/// line each (issue #85 — the REMOVE named nothing it destroyed).
fn database_warning(out: &mut String, db: &str, consumed: &[String]) {
    push_blank(out);
    out.push_str("-- >>> SURFORM WARNING: DATA LOSS <<<\n");
    out.push_str(&wrap_prose(
        &format!(
            "Removes database {} - every table and document in it is deleted.",
            comment_text(db)
        ),
        "-- ",
        "--    ",
        PROSE_WIDTH,
    ));
    for line in consumed {
        out.push_str(line);
        out.push('\n');
    }
}

/// The WARNING anchor above a `REMOVE NAMESPACE`: the whole subtree,
/// the collapse arm's consumed lines appended (issue #85).
fn namespace_warning(out: &mut String, ns: &str, consumed: &[String]) {
    push_blank(out);
    out.push_str("-- >>> SURFORM WARNING: DATA LOSS <<<\n");
    out.push_str(&wrap_prose(
        &format!(
            "Removes namespace {} - every database, table, and document in it is deleted.",
            comment_text(ns)
        ),
        "-- ",
        "--    ",
        PROSE_WIDTH,
    ));
    for line in consumed {
        out.push_str(line);
        out.push('\n');
    }
}

/// The retired scope's section label (issue #82's retired-frame
/// vocabulary) — the collapse emission's one comment per removed
/// scope, above its WARNING block. The `-- section: <label>` prefix
/// is the greppable contract (the live
/// frames' spelling; the retired variant keeps it byte-for-byte too)
/// — the prose tail rides the voice and ASCII rules, and the label
/// folds through [`comment_text`] like the live frames' (sane labels
/// ride byte-identical).
fn retired_section_label(scope: &ScopeKey) -> String {
    format!(
        "-- section: {} - retired (the schema file no longer declares this {})\n",
        comment_text(&scope_label(scope)),
        match scope {
            ScopeKey::Database(..) => "database",
            ScopeKey::Namespace(_) => "namespace",
            ScopeKey::Root => "scope",
        }
    )
}

/// Whether any statement a finding carries — either Drift side, the
/// removal's database side, a raw body, or a raw member nested in a
/// table's authored block — is raw-authored: the header's
/// conditional raw-corner stanza's gate.
fn carries_raw(finding: &DriftFinding) -> bool {
    fn stmt_raw(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Raw(_) => true,
            Stmt::Table(table) => table.nested.iter().any(stmt_raw),
            _ => false,
        }
    }
    match &finding.class {
        DriftClass::Drift { sdl, database, .. } => stmt_raw(sdl) || stmt_raw(database),
        DriftClass::SdlOnly { sdl } => stmt_raw(sdl),
        DriftClass::DatabaseOnly { database, .. } => stmt_raw(database),
        DriftClass::ExpectedUnmanaged { database } => stmt_raw(database),
    }
}

/// The UNIQUE-index hazard map (issue #80): one note per emitted
/// unique index define whose host table exists database-side
/// (`on_table_exists` — an apply-pending table's key range is empty,
/// nothing to collide). Walked over the emitted define set TWO ways:
/// the Index-family findings (standalone or host-slotted children —
/// the host gate is the finding's own), and the unique index MEMBERS
/// nested in a drifted table's authored block (their host gate is the
/// table finding's `on_table_exists` — the members' pieces render
/// from that emission). The re-keyed annotation reads the staged
/// set: the index's field expressions naming a re-keyed field (the
/// same exact-equality rule [`crate::info::staging::index_qualifies`]
/// walks) name it in the note. Keyed by (scope, the emitted
/// statement's first line) — the exact bytes the frame's piece
/// spells, wherever the member renders.
fn unique_index_hazards(
    defines: &[&DriftFinding],
    staged: &[StagedEmission<'_>],
) -> BTreeMap<(ScopeKey, String), String> {
    fn note_for(
        staged: &[StagedEmission<'_>],
        scope: &ScopeKey,
        host: &str,
        index: &crate::ast::Index,
    ) -> String {
        let rekeyed: Vec<&str> = staged
            .iter()
            .filter(|e| e.entry.scope == scope && e.entry.table == host)
            .flat_map(|e| e.entry.rekeys.iter().map(|rf| rf.key.as_str()))
            .collect();
        let covered: Vec<String> = index
            .fields
            .iter()
            .filter(|expr| rekeyed.iter().any(|key| expr.canon == *key))
            .map(|expr| format!("`{}`", comment_text(&expr.canon)))
            .collect();
        if covered.is_empty() {
            wrap_prose(
                "note: this unique index builds over existing rows - the \
                 define fails if any two rows collide; clean or dedupe the \
                 data first",
                "-- ",
                "--    ",
                PROSE_WIDTH,
            )
        } else {
            wrap_prose(
                &format!(
                    "note: this unique index builds over existing rows - the \
                     define fails if any two rows collide; its fields cover \
                     the re-keyed field(s) {} - clean or dedupe before, \
                     rebuild after the data step",
                    covered.join(", ")
                ),
                "-- ",
                "--    ",
                PROSE_WIDTH,
            )
        }
    }
    let mut out = BTreeMap::new();
    // The keys are the emitted statement's FIRST line — the lookup
    // prefix-matches the piece's first line against it, so the key
    // carries no terminator trivia.
    let first_line = |stmt: &Stmt| {
        crate::codegen::emit_statement_alone(stmt)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    for finding in defines {
        match define_stmt(finding) {
            Stmt::Index(index) => {
                if index.kind != IndexKind::Unique
                    || !finding.on_table_exists
                    || index.on.raw.is_empty()
                {
                    continue;
                }
                out.insert(
                    (finding.scope.clone(), first_line(define_stmt(finding))),
                    note_for(staged, &finding.scope, &index.on.raw, index),
                );
            }
            // A drifted table's authored unique index members render
            // as that emission's pieces — their hazard rides the
            // table finding's own existence stamp.
            Stmt::Table(table) if finding.on_table_exists => {
                for member in &table.nested {
                    let Stmt::Index(index) = member else {
                        continue;
                    };
                    if index.kind != IndexKind::Unique {
                        continue;
                    }
                    out.insert(
                        (finding.scope.clone(), first_line(member)),
                        note_for(staged, &finding.scope, &table.name.raw, index),
                    );
                }
            }
            _ => {}
        }
    }
    out
}

/// The hazard note for one emitted piece — the piece's first line
/// prefix-matched against the map's emitted statements (a piece IS
/// the finding's own emission byte-for-byte; the prefix match keeps
/// the lookup free of terminator trivia).
fn hazard_note<'h>(
    hazards: &'h BTreeMap<(ScopeKey, String), String>,
    scope: &ScopeKey,
    piece: &str,
) -> Option<&'h str> {
    let first = piece.trim_start().lines().next()?;
    hazards
        .iter()
        .find(|((at, spelled), _)| at == scope && first.starts_with(spelled.as_str()))
        .map(|(_, note)| note.as_str())
}

/// The REQUIRED data-migration slot above a `REMOVE TABLE`: the
/// engine deletes the table's key range, so the fill-in region rides
/// between the WARNING anchor (the alarm, outermost) and the
/// statement. The user writes the export/archive/move query between
/// the markers; it runs before the removal, inside the section's own
/// frame — and the scope's defines precede its removals inside that
/// frame, so a target the same section defines is already in place.
/// The `e.g.` line is real SurrealQL (the executed legs run it); no
/// rename or target is ever auto-detected — the example's `archive`
/// name demonstrates the syntax, the human names the move. The
/// region's outer delimiters follow the carrier ([`SlotStyle`]): the
/// stdout comments vs the saved draft's bare markers — the prose is
/// byte-shared.
fn table_slot(out: &mut String, table: &str, name: &str, style: SlotStyle) {
    push_blank(out);
    slot_open(out, name, style);
    out.push_str(&wrap_prose(
        "Write the export, archive, or move query between the markers; it \
         runs before the removal, in this same transaction.",
        "-- - ",
        "--   ",
        PROSE_WIDTH,
    ));
    out.push_str(&format!(
        "-- e.g. INSERT INTO archive (SELECT * FROM {});\n",
        comment_text(&ident_text(table))
    ));
    slot_close(out, style);
}

/// The slot region's opening delimiter, per carrier — the comment
/// banner (stdout) or the bare resolution marker with the slot's
/// name (the saved draft; `commit` scans exactly this spelling).
fn slot_open(out: &mut String, name: &str, style: SlotStyle) {
    match style {
        SlotStyle::CommentBanners => {
            out.push_str("-- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n");
        }
        SlotStyle::ResolutionMarkers => {
            out.push_str(crate::migrate::SLOT_OPEN_PREFIX);
            out.push_str(name);
            out.push_str(")\n");
        }
    }
}

/// The slot region's closing delimiter, per carrier.
fn slot_close(out: &mut String, style: SlotStyle) {
    match style {
        SlotStyle::CommentBanners => out.push_str("-- >>> SURFORM REQUIRED: END <<<\n"),
        SlotStyle::ResolutionMarkers => {
            out.push_str(crate::migrate::SLOT_CLOSE);
            out.push('\n');
        }
    }
}

/// The REQUIRED data-migration slot above a field's removal (or its
/// manual-removal comment — the fail-closed arm): `REMOVE FIELD` is
/// catalog-only, so the slot names the table and the dropped field,
/// states that the values remain on the rows outside every field
/// definition until cleaned, and carries the in-scope cleanup hint.
/// The prose names are the raw key and table (the report row's
/// vocabulary — the only honest spelling for a wildcard key); the
/// `e.g.` lines spell real query syntax, so they use the escaped
/// spellings and only ride a path that spells
/// (`field_path_spellable` — a wildcard or a quoted name holding a
/// dot would make the example itself a lie, so it drops, the
/// fail-closed posture). The prose names fold through
/// [`comment_text`] like every comment interpolation; the region's
/// outer delimiters follow the carrier ([`SlotStyle`], the table
/// slot's contract).
fn field_slot(out: &mut String, key: &str, table: &str, stmt: &Stmt, name: &str, style: SlotStyle) {
    let spellable = field_path_spellable(key, stmt);
    push_blank(out);
    slot_open(out, name, style);
    out.push_str(&wrap_prose(
        &format!(
            "Removes field {} on table {}. The stored values remain on the \
             rows, outside every field definition.",
            comment_text(key),
            comment_text(table)
        ),
        "-- ",
        "--    ",
        PROSE_WIDTH,
    ));
    out.push_str(&wrap_prose(
        "Write the move query between the markers; it runs before the \
         removal, in this same transaction.",
        "-- - ",
        "--   ",
        PROSE_WIDTH,
    ));
    if spellable {
        let (table, field) = (
            comment_text(&ident_text(table)),
            comment_text(&field_key_text(key)),
        );
        out.push_str(&format!("-- e.g. UPDATE {table} SET moved = {field};\n"));
        out.push_str(&wrap_prose(
            &format!(
                "The values remain after this script; unset them once the \
                 copy is verified (e.g. UPDATE {table} UNSET {field};)."
            ),
            "-- - ",
            "--   ",
            PROSE_WIDTH,
        ));
    } else {
        out.push_str(&wrap_prose(
            "The values remain after this script; unset them once the copy \
             is verified.",
            "-- - ",
            "--   ",
            PROSE_WIDTH,
        ));
    }
    slot_close(out, style);
}

/// The runner's session context — the `USE` state the next line
/// executes under.
#[derive(Default)]
struct Session {
    ns: Option<String>,
    db: Option<String>,
}

impl Session {
    /// Enters the finding's scope context, emitting the `USE` lines
    /// the change needs. The start state is the runner's root
    /// session (root content emits no USE); the pseudo-namespace has
    /// no USE spelling (codegen's own posture), and an unset has no
    /// spelling at all — root content is emitted before any USE, so
    /// that transition never arises.
    fn enter(&mut self, out: &mut String, scope: &ScopeKey) {
        let (ns, db) = match scope {
            ScopeKey::Root => (None, None),
            ScopeKey::Namespace(ns) => (Some(ns.clone()), None),
            ScopeKey::Database(ns, db) => (Some(ns.clone()), Some(db.clone())),
        };
        if self.ns == ns && self.db == db {
            return;
        }
        match (&ns, &db) {
            (None, _) => {}
            (Some(ns), Some(db)) if !ns.is_empty() => {
                out.push_str(&format!(
                    "USE NS {} DB {};\n",
                    ident_text(ns),
                    ident_text(db)
                ));
            }
            (Some(_), Some(_)) => {
                out.push_str(
                    "-- pseudo-namespace scope: no USE spelling exists; an arbitrary \
                     namespace applies\n",
                );
            }
            (Some(ns), None) if !ns.is_empty() => {
                out.push_str(&format!("USE NS {};\n", ident_text(ns)));
            }
            // The pseudo-namespace at the ns level, same posture as
            // the db level above: no USE spelling exists.
            (Some(_), None) => {
                out.push_str(
                    "-- pseudo-namespace scope: no USE spelling exists; an arbitrary \
                     namespace applies\n",
                );
            }
        }
        self.ns = ns;
        self.db = db;
    }

    /// Records the session state a rendered define established by
    /// itself — a `namespace` define's emission carries its own
    /// `USE NS` line (codegen model item 1), so the tracker adopts
    /// it instead of emitting a duplicate.
    fn adopt(&mut self, scope: &ScopeKey) {
        if let ScopeKey::Namespace(ns) = scope {
            self.ns = Some(ns.clone());
            self.db = None;
        }
    }
}

/// One finding's REMOVE line — the bare statement (terminator, no
/// trailing newline; the emission sites add the newline) — `None` for
/// the fail-closed corners (the caller emits the manual-removal
/// comment).
fn remove_line(family: Family, key: &str, on_table: Option<&str>, stmt: &Stmt) -> Option<String> {
    let line = match family {
        Family::Namespace => format!("REMOVE NAMESPACE IF EXISTS {};", ident_text(key)),
        Family::Database => format!("REMOVE DATABASE IF EXISTS {};", ident_text(key)),
        Family::Table => format!("REMOVE TABLE IF EXISTS {};", ident_text(key)),
        Family::Analyzer => format!("REMOVE ANALYZER IF EXISTS {};", ident_text(key)),
        Family::Sequence => format!("REMOVE SEQUENCE IF EXISTS {};", ident_text(key)),
        Family::Bucket => format!("REMOVE BUCKET IF EXISTS {};", ident_text(key)),
        Family::Event => {
            let table = on_table?;
            format!(
                "REMOVE EVENT IF EXISTS {} ON {};",
                ident_text(key),
                ident_text(table)
            )
        }
        Family::Index => {
            let table = on_table?;
            format!(
                "REMOVE INDEX IF EXISTS {} ON {};",
                ident_text(key),
                ident_text(table)
            )
        }
        Family::Field => {
            let table = on_table?;
            // REMOVE FIELD spells dotted PATHS (the doc on
            // [`field_path_spellable`]): a non-spellable key fails
            // closed to the manual-removal comment.
            if !field_path_spellable(key, stmt) {
                return None;
            }
            format!(
                "REMOVE FIELD IF EXISTS {} ON {};",
                field_key_text(key),
                ident_text(table)
            )
        }
        Family::Param => format!("REMOVE PARAM IF EXISTS ${};", ident_text(key)),
        Family::Function => format!(
            "REMOVE FUNCTION IF EXISTS fn::{};",
            key.split("::")
                .map(ident_text)
                .collect::<Vec<_>>()
                .join("::")
        ),
        Family::Api => {
            let mut path = String::new();
            quoted_string(&mut path, key);
            format!("REMOVE API IF EXISTS {path};")
        }
        Family::User => {
            let base = base_word(stmt)?;
            format!("REMOVE USER IF EXISTS {} ON {base};", ident_text(key))
        }
        Family::Access => {
            let base = base_word(stmt)?;
            format!("REMOVE ACCESS IF EXISTS {} ON {base};", ident_text(key))
        }
        Family::Config => {
            let kind = config_word(stmt)?;
            format!("REMOVE CONFIG IF EXISTS {kind};")
        }
        Family::Module => match stmt {
            Stmt::Module(module) => {
                // `None` names the anonymous-silo corner: the key is
                // the reader's own cross-check name (ledgered) —
                // never reconstructed into grammar here. The
                // manual-removal note and report row still name the
                // executable — [`empty_key_hint`]'s module arm.
                let name = module.name.as_ref()?;
                format!("REMOVE MODULE IF EXISTS mod::{};", ident_text(&name.raw))
            }
            _ => return None,
        },
        Family::Raw => return None, // the header never classified
        // Defensive: mixins never reach the differ (the SDL side
        // arrives pre-expanded) and emit nothing to remove.
        Family::Mixin => return None,
        // Defensive: type aliases never reach the differ either (the
        // SDL side arrives pre-expanded) and emit nothing to remove.
        Family::TypeAlias => return None,
        // Defensive: the linter's allow-comment family never reaches
        // the differ either (nothing in INFO classifies to it).
        Family::Comment => return None,
        // Defensive: the linter's diamond-include family ditto —
        // nothing in INFO classifies to it.
        Family::Include => return None,
    };
    Some(line)
}

/// A SurrealQL identifier: bare when a plain non-keyword word,
/// backtick-escaped otherwise (the union-keyword rule). The migration
/// file pipeline (`crate::migrate`) shares it for the ledger table's
/// name, and the Surrealism guest composes its per-table capture
/// statements with it — one escaping rule, one home.
pub fn ident_text(raw: &str) -> String {
    if ident_needs_escape(raw) {
        let mut out = String::new();
        out.push('`');
        escape_with(&mut out, raw, '`');
        out.push('`');
        out
    } else {
        raw.to_string()
    }
}

/// A database-side name's spelling inside emitted COMMENT text: every
/// line-comment terminator folds to its visible escape spelling —
/// the SurrealQL-spellable ones mirroring [`escape_with`]'s table,
/// the rest as `\u{…}` — so the line stays ONE `--` comment for any
/// name. The terminator set is the vendored lexer's
/// (`eat_single_line_comment`, the one eater behind `--`, `//`, AND
/// `#`): LF, CR, NEL — all control characters — plus the Zl/Zp
/// separators U+2028/U+2029, which are NOT controls and spell
/// explicitly beside the Cc fold. The vector: a backtick identifier
/// may hold any bytes (the vendored lexer), and a raw terminator
/// would end the comment and execute the tail as SurrealQL inside
/// the script's transaction frames. Everything else renders verbatim —
/// `--` and backticks inside a comment line are inert text; the
/// query-syntax spellings ride it as the last step too (the backtick
/// escaping folds newlines, this also folds the terminators it
/// leaves verbatim inside the quotes). `pub(crate)`: the migration
/// file pipeline shares it — a header line or marker must stay ONE
/// line for any path or name, the same terminator set, one home —
/// and the drift report folds its interpolated names through it too
/// (the report's rows and manifests are line walks under the same
/// contract).
pub(crate) fn comment_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\0' => out.push_str("\\0"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{8}' => out.push_str("\\u{8}"),
            '\u{c}' => out.push_str("\\f"),
            '\u{2028}' => out.push_str("\\u{2028}"),
            '\u{2029}' => out.push_str("\\u{2029}"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// A byte of a SurrealQL line comment's terminator set — the vendored
/// lexer's `eat_single_line_comment`, the one eater behind `--`, `//`,
/// AND `#`: LF, CR, NEL U+0085, and the Zl/Zp separators
/// U+2028/U+2029 (which ride past an `is_control()` check — spelled
/// explicitly). `pub(crate)`: the migration file
/// pipeline's marker scan shares it (a scan's comment-inertness must
/// end exactly where the lexer's does) — one home for the vendored
/// terminator set.
pub(crate) fn is_line_comment_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// A character of the vendored lexer's token spacing —
/// `eat_whitespace`'s exact set (lexer/byte.rs:90-124): the ASCII
/// bytes {TAB, LF, VT, FF, CR, SP} plus — the non-ASCII bytes
/// `complete_char`-decoded, the same set `lex_char` re-routes to
/// `eat_whitespace` (lexer/char.rs) — {U+00A0, U+1680,
/// U+2000–U+200A, U+202F, U+205F, U+3000}. Whitespace produces NO
/// token, so this spacing may sit between any two tokens (the
/// migration pipeline's `function` trigger rides it — NBSP between
/// the word and its `(` is real spacing, not an ident continuation).
/// Anything else — invalid UTF-8 included — breaks the eat and
/// becomes the next token's first byte. `pub(crate)`: one home for
/// the vendored whitespace set, beside
/// [`is_line_comment_terminator`].
pub(crate) fn is_lexer_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// An empty match key's stand-in name: the
/// manual-removal note above and the report row both quote it, so a
/// nameless entry still says WHAT to remove. Two producers, both
/// version-pair-only (the DB pair's entries are named by their INFO
/// keys, never empty): the header-less raw body names its first
/// non-empty line, and the anonymous-silo module — whose REMOVE the
/// reader's key can never reconstruct ([`remove_line`]'s ledgered
/// corner) — names its executable's `from:` spelling. Comment-safe
/// either way. `None` whenever the key is present or the statement
/// is neither.
pub(super) fn empty_key_hint(family: Family, key: &str, database: &Stmt) -> Option<String> {
    if !key.is_empty() {
        return None;
    }
    match (family, database) {
        (Family::Raw, Stmt::Raw(raw)) => raw
            .body
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(comment_text),
        (Family::Module, Stmt::Module(module)) if module.name.is_none() => {
            Some(comment_text(&executable_text(&module.executable)))
        }
        _ => None,
    }
}

/// An executable's `from:` spelling — the documented silo form, or
/// the bucket+key file-string form (the same vocabulary codegen
/// prints, minus the word-shaping: this is a COMMENT hint, not
/// grammar — never reconstructed into a REMOVE).
fn executable_text(executable: &Executable) -> String {
    match executable {
        Executable::File { bucket, key } => format!("f\"{bucket}:{key}\""),
        Executable::Silo {
            org,
            pkg,
            major,
            minor,
            patch,
        } => format!("silo::{org}::{pkg}::<{major}.{minor}.{patch}>"),
    }
}

/// A field-path key (`a.b`) — each dotted segment escapes
/// independently (wildcards fail closed above, before this runs).
fn field_key_text(key: &str) -> String {
    key.split('.').map(ident_text).collect::<Vec<_>>().join(".")
}

/// Whether a field key spells as a `REMOVE FIELD` path (and as a
/// plain query path, the slot examples' standard): the AST carries
/// the truth a flattened key cannot — a Name segment holding a dot
/// (a quoted name like `a.b`) would re-emit as the path `a.b`, a
/// different object; a wildcard segment has no REMOVE spelling at
/// all. Either fails closed. A raw body keeps the key-based check
/// (its common shape is a genuine nested path). The removal line and
/// the slot's `e.g.` lines share the one check.
fn field_path_spellable(key: &str, stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Field(field) => field.name.segments.iter().all(|segment| match segment {
            FieldPathSeg::Name(name) => word_shaped(&name.raw),
            FieldPathSeg::Wildcard => false,
        }),
        _ => !key.split('.').any(|segment| !word_shaped(segment)),
    }
}

/// A user/access removal's `ON` base, from the resolved statement.
fn base_word(stmt: &Stmt) -> Option<String> {
    let level = match stmt {
        Stmt::User(user) => &user.on.value,
        Stmt::Access(access) => &access.on.value,
        _ => return None,
    };
    Some(match level {
        Level::Root => "ROOT".to_string(),
        Level::Namespace => "NAMESPACE".to_string(),
        Level::Database => "DATABASE".to_string(),
    })
}

/// A config removal's kind word, from the resolved statement.
fn config_word(stmt: &Stmt) -> Option<String> {
    match stmt {
        Stmt::Config(Config::Api(_)) => Some("API".to_string()),
        Stmt::Config(Config::Graphql(_)) => Some("GRAPHQL".to_string()),
        Stmt::Config(Config::Default(_)) => Some("DEFAULT".to_string()),
        _ => None,
    }
}

/// The tests' one voice-scan home (the plain-prose
/// contract): `cfg(test)` because only tests walk rendered prose, but
/// `pub(crate)` so the migration-writer tests share it — the group
/// writer's own `-- draft group member` line rides the same contract
/// the script fixture scans. One home, so the banned list cannot
/// drift between the two suites.
#[cfg(test)]
pub(crate) mod voice {
    /// The banned-vocabulary scan: word-boundary matched so a
    /// substring inside a longer word never fires. "Let's" rides a
    /// raw substring check — the apostrophe splits the word
    /// tokenizer.
    pub(crate) fn assert_voice_clean(line: &str) {
        let lower = line.to_ascii_lowercase();
        assert!(!lower.contains("let's"), "banned prose in comment: {line}");
        for word in [
            "you",
            "your",
            "simply",
            "just",
            "easily",
            "powerful",
            "robust",
            "seamless",
            "effortless",
        ] {
            assert!(
                !lower
                    .split(|c: char| !c.is_ascii_alphabetic())
                    .any(|part| part == word),
                "banned prose word `{word}` in comment: {line}"
            );
        }
        assert!(!line.contains('!'), "an exclamation in a comment: {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_stmt(sdl: &str) -> Stmt {
        let (file, diags) = crate::parser::parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        file.items.into_iter().next().expect("one statement")
    }

    fn parse_file_sdl(sdl: &str) -> SdlFile {
        let (file, diags) = crate::parser::parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        file
    }

    fn db_only(
        scope: ScopeKey,
        family: Family,
        key: &str,
        stmt: Stmt,
        on_table: Option<&str>,
    ) -> DriftFinding {
        DriftFinding {
            on_table_exists: false,
            scope,
            family,
            key: key.to_string(),
            class: DriftClass::DatabaseOnly {
                database: Box::new(stmt),
                on_table: on_table.map(String::from),
                markers: Box::default(),
            },
        }
    }

    fn sdl_only(scope: ScopeKey, family: Family, key: &str, stmt: Stmt) -> DriftFinding {
        DriftFinding {
            on_table_exists: false,
            scope,
            family,
            key: key.to_string(),
            class: DriftClass::SdlOnly {
                sdl: Box::new(stmt),
            },
        }
    }

    fn drift(
        scope: ScopeKey,
        family: Family,
        key: &str,
        sdl: Stmt,
        database: Stmt,
    ) -> DriftFinding {
        DriftFinding {
            on_table_exists: false,
            scope,
            family,
            key: key.to_string(),
            class: DriftClass::Drift {
                sdl: Box::new(sdl),
                database: Box::new(database),
                markers: Box::default(),
            },
        }
    }

    /// The header's tail, byte-what [`header_lines`] appends for a
    /// typed-only script — the wrapped summary plus the blank line
    /// that closes the header block. The pins interpolate [`HEADER`]
    /// plus this (the header bytes live in ONE place: a header edit
    /// touches the const, [`summary_block`], and the one full-text
    /// pin, never the twenty pins after it). The optional unmanaged
    /// count appends the informational clause.
    fn summary(
        findings: usize,
        defines: usize,
        removals: usize,
        slots: usize,
        staged: usize,
        frames: usize,
    ) -> String {
        summary_unmanaged(findings, defines, removals, slots, staged, frames, 0)
    }

    /// [`Self::summary`] with the expected-unmanaged count. The
    /// trailing blank is the header block's closing line (byte-what
    /// the assembly normalizes to).
    fn summary_unmanaged(
        findings: usize,
        defines: usize,
        removals: usize,
        slots: usize,
        staged: usize,
        frames: usize,
        unmanaged: usize,
    ) -> String {
        let mut out = summary_block(
            findings, defines, removals, slots, staged, frames, unmanaged,
        );
        out.push('\n');
        out
    }

    // ---------------------------------------------------------------
    // The defines
    // ---------------------------------------------------------------

    /// An SDL-only definition renders as its `DEFINE … OVERWRITE` —
    /// the convergence emitter's core semantics (the demoted
    /// remove-script's pending note is gone: apply-pending IS the
    /// define now).
    #[test]
    fn sdl_only_renders_an_overwrite_define() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![sdl_only(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Table,
            "pending",
            parse_stmt("table pending;"),
        )];
        assert_eq!(
            convergence_script(&findings, &sdl),
            format!(
                "{HEADER}{}\
                 USE NS app DB main;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope db:app/main\n\
                 -- section: db:app/main (1 define, 0 removals)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Defines - the state the schema file describes\n\
                 --\n\
                 -- Each define carries OVERWRITE: a re-run redefines it.\n\
                 \n\
                 DEFINE TABLE OVERWRITE pending TYPE NORMAL SCHEMALESS;\n\
                 \n\
                 COMMIT;\n",
                summary(1, 1, 0, 0, 0, 1),
            )
        );
    }

    /// A Diverged finding renders the AUTHORED side — the database's
    /// drifted form appears nowhere.
    #[test]
    fn drift_renders_the_authored_side() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Param,
            "p",
            parse_stmt("param $p = 2;"),
            parse_stmt("param $p = 1;"),
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains("DEFINE PARAM OVERWRITE $p VALUE 2;"),
            "{script}"
        );
        assert!(!script.contains("VALUE 1"), "{script}");
    }

    /// The defines' member→host rule: a
    /// standalone child define whose host table ALSO defines renders
    /// inside the host's emission — the table head first, then the
    /// child — so the (family, key) stand-in's field-first order
    /// resolves: the host's `OVERWRITE` must run first (a LIGHTWEIGHT
    /// host refuses the child's define and the whole transaction rolls
    /// back; the executed pins live in tests/diff.rs). The host's
    /// nested list is empty here, so the child rides right behind the
    /// head. Unrelated defines keep the stand-in order (the
    /// emission-order tie-break: the param stays ahead of the table).
    #[test]
    fn a_child_defines_after_its_host_table() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![
            sdl_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "t",
                parse_stmt("table t;"),
            ),
            sdl_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "x",
                parse_stmt("field x: int { on: t; };"),
            ),
            sdl_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Param,
                "p",
                parse_stmt("param $p = 1;"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let param_at = script
            .find("DEFINE PARAM OVERWRITE $p")
            .expect("param define");
        let table_at = script
            .find("DEFINE TABLE OVERWRITE t")
            .expect("table define");
        let field_at = script
            .find("DEFINE FIELD OVERWRITE x ON t")
            .expect("field define");
        assert!(param_at < table_at && table_at < field_at, "{script}");
    }

    /// The dedup: a nested-authored child's
    /// finding suppresses when the host table's own finding renders —
    /// the table's emission carries the member, so the standalone
    /// render would duplicate it (two findings, one define line).
    /// (The standalone-authored child — whose host finding carries
    /// nothing nested — still renders: the test above.)
    #[test]
    fn a_nested_child_renders_once_inside_its_hosts_define() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![
            drift(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "likes",
                parse_stmt("table likes { field x: int; };"),
                parse_stmt("table likes;"),
            ),
            sdl_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "x",
                parse_stmt("field x: int { on: likes; };"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        assert_eq!(
            script.matches("DEFINE FIELD OVERWRITE x ON likes").count(),
            1,
            "{script}"
        );
        let table_at = script
            .find("DEFINE TABLE OVERWRITE likes")
            .expect("table define");
        let field_at = script
            .find("DEFINE FIELD OVERWRITE x ON likes")
            .expect("the nested member");
        assert!(table_at < field_at, "{script}");
    }

    /// The host's AUTHORED define nests an index
    /// naming the standalone child — the table statement pins that
    /// edge (order.rs's table arm pushes the nested indexes' field
    /// targets), and the table's emission is one unit, so the child
    /// must ride BETWEEN the head and the members: after it, the
    /// nested index defines against a SCHEMAFULL table that lacks the
    /// field and hard-fails `FdNotFound` (the executed pin is
    /// `a_nested_index_child_slots_inside_the_hosts_emission` in
    /// tests/diff.rs); before the head, a LIGHTWEIGHT host refuses the
    /// child. Slotting delivers the only order that satisfies both.
    #[test]
    fn a_child_named_by_a_nested_index_slots_inside_the_hosts_emission() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![
            drift(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "t",
                parse_stmt("table t { schemafull; index fulltext ix { fields: x; }; };"),
                parse_stmt("table t;"),
            ),
            sdl_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "x",
                parse_stmt("field x: int { on: t; };"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let table_at = script
            .find("DEFINE TABLE OVERWRITE t")
            .expect("the host head");
        let field_at = script
            .find("DEFINE FIELD OVERWRITE x ON t")
            .expect("the slotted child");
        let index_at = script
            .find("DEFINE INDEX OVERWRITE ix ON t")
            .expect("the nested member");
        assert!(
            table_at < field_at && field_at < index_at,
            "the child rides between the head and the naming member:\n{script}"
        );
        // The nested index finding is the host emission's (dropped) —
        // the member renders once, inside the table's define.
        assert_eq!(
            script.matches("DEFINE INDEX OVERWRITE ix").count(),
            1,
            "{script}"
        );
    }

    /// Two standalone children of one host, each named by a nested
    /// index: both slot inside the host's emission, the fields before
    /// the indexes that name them (the frozen 4b edge), in their
    /// sorted-set order among themselves.
    #[test]
    fn two_children_slot_inside_their_hosts_emission_fields_first() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![
            drift(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "t",
                parse_stmt(
                    "table t { schemafull; index fulltext ix { fields: x; };
                      index fulltext iy { fields: y; }; };",
                ),
                parse_stmt("table t;"),
            ),
            sdl_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "y",
                parse_stmt("field y: int { on: t; };"),
            ),
            sdl_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "x",
                parse_stmt("field x: int { on: t; };"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let table_at = script
            .find("DEFINE TABLE OVERWRITE t")
            .expect("the host head");
        let x_at = script
            .find("DEFINE FIELD OVERWRITE x ON t")
            .expect("child x");
        let y_at = script
            .find("DEFINE FIELD OVERWRITE y ON t")
            .expect("child y");
        let ix_at = script
            .find("DEFINE INDEX OVERWRITE ix ON t")
            .expect("member ix");
        let iy_at = script
            .find("DEFINE INDEX OVERWRITE iy ON t")
            .expect("member iy");
        assert!(
            table_at < x_at && x_at < ix_at && ix_at < y_at && y_at < iy_at,
            "fields before the indexes that name them:\n{script}"
        );
    }

    /// The db-define + ns-content order: the
    /// section's `USE NS` opens, the database define renders before
    /// the namespace content — and carries NO `USE` line of its own
    /// (a one-statement emission has no session context), so the
    /// session tracker stays coherent and the content defines in the
    /// namespace scope.
    #[test]
    fn a_database_define_carries_no_use_and_precedes_ns_content() {
        let sdl = parse_file_sdl("namespace app;");
        let findings = vec![
            sdl_only(
                ScopeKey::Namespace("app".into()),
                Family::Database,
                "main",
                parse_stmt("database main;"),
            ),
            sdl_only(
                ScopeKey::Namespace("app".into()),
                Family::Param,
                "p",
                parse_stmt("param $p = 1;"),
            ),
        ];
        assert_eq!(
            convergence_script(&findings, &sdl),
            format!(
                "{HEADER}{}\
                 USE NS app;\n\
                 DEFINE DATABASE OVERWRITE main;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope ns:app\n\
                 -- section: ns:app (1 define, 0 removals)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Defines - the state the schema file describes\n\
                 --\n\
                 -- Each define carries OVERWRITE: a re-run redefines it.\n\
                 \n\
                 DEFINE PARAM OVERWRITE $p VALUE 1;\n\
                 \n\
                 COMMIT;\n",
                summary(2, 2, 0, 0, 0, 1),
            )
        );
    }

    /// A namespace define rides the root section (its emission
    /// carries the `USE NS`), and its scope's content defines follow
    /// under that session context.
    #[test]
    fn a_namespace_define_opens_its_scope() {
        let sdl = parse_file_sdl("namespace app;");
        let findings = vec![
            sdl_only(
                ScopeKey::Root,
                Family::Namespace,
                "app",
                parse_stmt("namespace app;"),
            ),
            sdl_only(
                ScopeKey::Namespace("app".into()),
                Family::Param,
                "p",
                parse_stmt("param $p = 1;"),
            ),
        ];
        assert_eq!(
            convergence_script(&findings, &sdl),
            format!(
                "{HEADER}{}\
                 DEFINE NAMESPACE OVERWRITE app;\n\
                 USE NS app;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope ns:app\n\
                 -- section: ns:app (1 define, 0 removals)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Defines - the state the schema file describes\n\
                 --\n\
                 -- Each define carries OVERWRITE: a re-run redefines it.\n\
                 \n\
                 DEFINE PARAM OVERWRITE $p VALUE 1;\n\
                 \n\
                 COMMIT;\n",
                summary(2, 2, 0, 0, 0, 1),
            )
        );
    }

    /// No defines and no removals render an empty script — the
    /// converged state (an expected-unmanaged finding is
    /// informational and renders nothing).
    #[test]
    fn converged_renders_empty() {
        let sdl = parse_file_sdl("namespace app;");
        assert_eq!(convergence_script(&[], &sdl), "");
    }

    // ---------------------------------------------------------------
    // The removals tail
    // ---------------------------------------------------------------

    /// A child removes before its host table, in the db scope's
    /// `USE` context — the load-bearing child-before-parent rule —
    /// and each data-orphaning removal carries its anchor cluster:
    /// the field (catalog-only, values survive as residue) the
    /// REQUIRED slot alone; the table the WARNING alarm with the
    /// REQUIRED slot between it and the statement.
    #[test]
    fn children_remove_before_their_table() {
        let sdl = parse_file_sdl("namespace app; database main; table user;");
        let findings = vec![
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "user",
                parse_stmt("table user;"),
                None,
            ),
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "x",
                parse_stmt("field x: int { on: user; };"),
                Some("user"),
            ),
        ];
        assert_eq!(
            convergence_script(&findings, &sdl),
            format!(
                "{HEADER}{}\
                 USE NS app DB main;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope db:app/main\n\
                 -- section: db:app/main (0 defines, 2 removals)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Removals - objects the schema file no longer declares\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 -- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n\
                 -- Removes field x on table user. The stored values remain on\n\
                 --    the rows, outside every field definition.\n\
                 -- - Write the move query between the markers; it runs before\n\
                 --   the removal, in this same transaction.\n\
                 -- e.g. UPDATE `user` SET moved = x;\n\
                 -- - The values remain after this script; unset them once the\n\
                 --   copy is verified (e.g. UPDATE `user` UNSET x;).\n\
                 -- >>> SURFORM REQUIRED: END <<<\n\
                 REMOVE FIELD IF EXISTS x ON `user`;\n\
                 \n\
                 -- >>> SURFORM WARNING: DATA LOSS <<<\n\
                 -- Removes table user - every document in it is deleted.\n\
                 \n\
                 -- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n\
                 -- - Write the export, archive, or move query between the\n\
                 --   markers; it runs before the removal, in this same\n\
                 --   transaction.\n\
                 -- e.g. INSERT INTO archive (SELECT * FROM `user`);\n\
                 -- >>> SURFORM REQUIRED: END <<<\n\
                 REMOVE TABLE IF EXISTS `user`;\n\
                 \n\
                 COMMIT;\n",
                summary(2, 0, 2, 2, 0, 1),
            )
        );
    }

    /// Sections open with their `USE` context and run root → ns →
    /// per-db; content sorts (family, key) within a scope.
    #[test]
    fn scope_sections_use_and_order() {
        let sdl = parse_file_sdl("namespace app; database main; database other;");
        let findings = vec![
            db_only(
                ScopeKey::Namespace("app".into()),
                Family::Param,
                "p",
                parse_stmt("param $p = 1;"),
                None,
            ),
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "user",
                parse_stmt("table user;"),
                None,
            ),
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Event,
                "e",
                parse_stmt("event e { on: user; when: true; then: (create x); };"),
                Some("user"),
            ),
            db_only(
                ScopeKey::Database("app".into(), "other".into()),
                Family::Sequence,
                "s",
                parse_stmt("sequence s = 5;"),
                None,
            ),
        ];
        assert_eq!(
            convergence_script(&findings, &sdl),
            format!(
                "{HEADER}{}\
                 USE NS app;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope ns:app\n\
                 -- section: ns:app (0 defines, 1 removal)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Removals - objects the schema file no longer declares\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 REMOVE PARAM IF EXISTS $p;\n\
                 \n\
                 COMMIT;\n\
                 USE NS app DB main;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope db:app/main\n\
                 -- section: db:app/main (0 defines, 2 removals)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Removals - objects the schema file no longer declares\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 REMOVE EVENT IF EXISTS e ON `user`;\n\
                 \n\
                 -- >>> SURFORM WARNING: DATA LOSS <<<\n\
                 -- Removes table user - every document in it is deleted.\n\
                 \n\
                 -- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n\
                 -- - Write the export, archive, or move query between the\n\
                 --   markers; it runs before the removal, in this same\n\
                 --   transaction.\n\
                 -- e.g. INSERT INTO archive (SELECT * FROM `user`);\n\
                 -- >>> SURFORM REQUIRED: END <<<\n\
                 REMOVE TABLE IF EXISTS `user`;\n\
                 \n\
                 COMMIT;\n\
                 USE NS app DB other;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope db:app/other\n\
                 -- section: db:app/other (0 defines, 1 removal)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Removals - objects the schema file no longer declares\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 REMOVE SEQUENCE IF EXISTS s;\n\
                 \n\
                 COMMIT;\n",
                summary(4, 0, 4, 1, 0, 3),
            )
        );
    }

    /// An undeclared database scope collapses: the content
    /// suppresses, the single REMOVE implies it — reachable from the
    /// library and the version-pair CLI (the `--info`
    /// mode's tag validation remains the data-loss guard there),
    /// with the REMOVE DATABASE WARNING anchor.
    #[test]
    fn collapse_undeclared_database() {
        let sdl = parse_file_sdl("namespace app;");
        let findings = vec![
            db_only(
                ScopeKey::Database("app".into(), "stale".into()),
                Family::Table,
                "t",
                parse_stmt("table t;"),
                None,
            ),
            db_only(
                ScopeKey::Database("app".into(), "stale".into()),
                Family::Field,
                "x",
                parse_stmt("field x: int { on: t; };"),
                Some("t"),
            ),
        ];
        assert_eq!(
            convergence_script(&findings, &sdl),
            format!(
                "{HEADER}{}\
                 USE NS app;\n\
                 \n\
                 {RULE}\n\
                 -- ## Removals - retired scopes\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 -- section: db:app/stale - retired (the schema file no longer \
                 declares this database)\n\
                 \n\
                 -- >>> SURFORM WARNING: DATA LOSS <<<\n\
                 -- Removes database stale - every table and document in it is\n\
                 --    deleted.\n\
                 -- includes field x ON t (the schema file no longer declares \
                 this database)\n\
                 -- includes table t (the schema file no longer declares this \
                 database)\n\
                 REMOVE DATABASE IF EXISTS stale;\n\
                 \n\
                 COMMIT;\n",
                summary(2, 0, 2, 0, 0, 1),
            )
        );
        // The includes lines sort with their scope's content — any
        // permutation of the same findings renders byte-identically
        // (the multiset contract reaches the collapse arm).
        let script = convergence_script(&findings, &sdl);
        let mut shuffled = findings.clone();
        shuffled.reverse();
        assert_eq!(convergence_script(&shuffled, &sdl), script);
        let mut rotated = findings;
        rotated.rotate_left(1);
        assert_eq!(convergence_script(&rotated, &sdl), script);
    }

    /// Root content needs no USE; a root-scope namespace finding is
    /// the REMOVE NAMESPACE owner line, emitted last under its
    /// WARNING anchor.
    #[test]
    fn root_content_and_namespace_owner() {
        let sdl = parse_file_sdl("");
        let findings = vec![
            db_only(
                ScopeKey::Root,
                Family::Namespace,
                "junk",
                parse_stmt("namespace junk;"),
                None,
            ),
            db_only(
                ScopeKey::Root,
                Family::User,
                "legacy",
                parse_stmt("user legacy { on: root; password: 'hunter2'; roles: owner; };"),
                None,
            ),
        ];
        assert_eq!(
            convergence_script(&findings, &sdl),
            format!(
                "{HEADER}{}\
                 {RULE}\n\
                 -- ## Scope root\n\
                 -- section: root (0 defines, 1 removal)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Removals - objects the schema file no longer declares\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 REMOVE USER IF EXISTS legacy ON ROOT;\n\
                 \n\
                 COMMIT;\n\
                 \n\
                 {RULE}\n\
                 -- ## Removals - retired scopes\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 -- section: ns:junk - retired (the schema file no longer declares \
                 this namespace)\n\
                 \n\
                 -- >>> SURFORM WARNING: DATA LOSS <<<\n\
                 -- Removes namespace junk - every database, table, and\n\
                 --    document in it is deleted.\n\
                 REMOVE NAMESPACE IF EXISTS junk;\n\
                 \n\
                 COMMIT;\n",
                summary(2, 0, 2, 0, 0, 2),
            )
        );
    }

    /// The per-family spellings: api by quoted printed path key,
    /// param `$`-prefixed, function's `fn::` path without parens.
    #[test]
    fn per_family_spellings() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            db_only(
                scope.clone(),
                Family::Param,
                "x",
                parse_stmt("param $x = 1;"),
                None,
            ),
            db_only(
                scope.clone(),
                Family::Function,
                "a::b",
                parse_stmt("fn a::b($x: int) { return $x; };"),
                None,
            ),
            db_only(
                scope.clone(),
                Family::Api,
                "/old path",
                parse_stmt("api '/old path' { any { permissions: full; }; };"),
                None,
            ),
            db_only(
                scope.clone(),
                Family::Config,
                "default",
                parse_stmt("config default { namespace: 'app'; database: 'main'; };"),
                None,
            ),
            db_only(
                scope,
                Family::Access,
                "old_access",
                parse_stmt("access old_access { on: db; type: record; };"),
                None,
            ),
        ];
        assert_eq!(
            convergence_script(&findings, &sdl),
            format!(
                "{HEADER}{}\
                 USE NS app DB main;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope db:app/main\n\
                 -- section: db:app/main (0 defines, 5 removals)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Removals - objects the schema file no longer declares\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 REMOVE ACCESS IF EXISTS old_access ON DATABASE;\n\
                 REMOVE API IF EXISTS '/old path';\n\
                 REMOVE CONFIG IF EXISTS DEFAULT;\n\
                 REMOVE FUNCTION IF EXISTS fn::a::b;\n\
                 REMOVE PARAM IF EXISTS $x;\n\
                 \n\
                 COMMIT;\n",
                summary(5, 0, 5, 0, 0, 1),
            )
        );
    }

    /// An owner-family finding for a DECLARED owner is the INFO
    /// key-mismatch corner — fail-closed manual-removal, never a
    /// REMOVE behind a mangled key.
    #[test]
    fn declared_owner_finding_is_manual() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![db_only(
            ScopeKey::Namespace("app".into()),
            Family::Database,
            "main",
            parse_stmt("database main;"),
            None,
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains(
                "-- manual removal required: ns:app database main - the schema file \
                 declares it, so this is an INFO key mismatch, not an unmanaged \
                 database\n"
            ),
            "{script}"
        );
        assert!(script.starts_with(HEADER));
        assert!(script.starts_with("-- generated by `surform migrate generate`"));
    }

    /// Defines and removals in one script: the scope's defines first,
    /// the removal banner between, one frame around the scope's
    /// section, its `USE` above the frame.
    #[test]
    fn defines_precede_removals() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![
            sdl_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Param,
                "fresh",
                parse_stmt("param $fresh = 1;"),
            ),
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "stale",
                parse_stmt("table stale;"),
                None,
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let define_at = script
            .find("DEFINE PARAM OVERWRITE $fresh")
            .expect("the define");
        let banner_at = script.find("-- ## Removals").expect("the removal header");
        let remove_at = script
            .find("REMOVE TABLE IF EXISTS stale;")
            .expect("the remove");
        assert!(define_at < banner_at && banner_at < remove_at, "{script}");
        assert!(script.starts_with(&format!(
            "{HEADER}{}\
             USE NS app DB main;\n\
             \n\
             {RULE}\n\
             -- ## Scope db:app/main\n\
             -- section: db:app/main (1 define, 1 removal)\n\
             \n\
             BEGIN TRANSACTION;\n",
            summary(2, 1, 1, 1, 0, 1),
        )));
        assert!(script.ends_with("COMMIT;\n"));
    }

    /// The amendment's structural pin: NO `USE` line ever sits inside
    /// a `BEGIN…COMMIT` frame, whatever the scope layout. The corpus
    /// carries everything at once — namespace owner defines (whose
    /// own emission carries the `USE NS`), database owner defines,
    /// content defines, and a removal — across two namespaces; the
    /// byte pin shows the per-scope frames and their outside `USE`
    /// switches.
    #[test]
    fn no_use_line_sits_inside_a_frame() {
        let sdl = parse_file_sdl(
            "namespace alpha; database one; table keep1; \
             namespace beta; database two; table keep2;",
        );
        let findings = vec![
            sdl_only(
                ScopeKey::Root,
                Family::Namespace,
                "alpha",
                parse_stmt("namespace alpha;"),
            ),
            sdl_only(
                ScopeKey::Namespace("alpha".into()),
                Family::Database,
                "one",
                parse_stmt("database one;"),
            ),
            sdl_only(
                ScopeKey::Database("alpha".into(), "one".into()),
                Family::Table,
                "fresh1",
                parse_stmt("table fresh1;"),
            ),
            db_only(
                ScopeKey::Database("alpha".into(), "one".into()),
                Family::Table,
                "stale1",
                parse_stmt("table stale1;"),
                None,
            ),
            sdl_only(
                ScopeKey::Root,
                Family::Namespace,
                "beta",
                parse_stmt("namespace beta;"),
            ),
            sdl_only(
                ScopeKey::Namespace("beta".into()),
                Family::Database,
                "two",
                parse_stmt("database two;"),
            ),
            sdl_only(
                ScopeKey::Database("beta".into(), "two".into()),
                Family::Table,
                "fresh2",
                parse_stmt("table fresh2;"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        assert_eq!(
            script,
            format!(
                "{HEADER}{}\
                 DEFINE NAMESPACE OVERWRITE alpha;\n\
                 USE NS alpha;\n\
                 DEFINE NAMESPACE OVERWRITE beta;\n\
                 USE NS beta;\n\
                 USE NS alpha;\n\
                 DEFINE DATABASE OVERWRITE one;\n\
                 USE NS alpha DB one;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope db:alpha/one\n\
                 -- section: db:alpha/one (1 define, 1 removal)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Defines - the state the schema file describes\n\
                 --\n\
                 -- Each define carries OVERWRITE: a re-run redefines it.\n\
                 \n\
                 DEFINE TABLE OVERWRITE fresh1 TYPE NORMAL SCHEMALESS;\n\
                 \n\
                 {RULE}\n\
                 -- ## Removals - objects the schema file no longer declares\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 -- >>> SURFORM WARNING: DATA LOSS <<<\n\
                 -- Removes table stale1 - every document in it is deleted.\n\
                 \n\
                 -- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n\
                 -- - Write the export, archive, or move query between the\n\
                 --   markers; it runs before the removal, in this same\n\
                 --   transaction.\n\
                 -- e.g. INSERT INTO archive (SELECT * FROM stale1);\n\
                 -- >>> SURFORM REQUIRED: END <<<\n\
                 REMOVE TABLE IF EXISTS stale1;\n\
                 \n\
                 COMMIT;\n\
                 USE NS beta;\n\
                 DEFINE DATABASE OVERWRITE two;\n\
                 USE NS beta DB two;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope db:beta/two\n\
                 -- section: db:beta/two (1 define, 0 removals)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Defines - the state the schema file describes\n\
                 --\n\
                 -- Each define carries OVERWRITE: a re-run redefines it.\n\
                 \n\
                 DEFINE TABLE OVERWRITE fresh2 TYPE NORMAL SCHEMALESS;\n\
                 \n\
                 COMMIT;\n",
                summary(7, 6, 1, 1, 0, 2),
            )
        );
        assert_no_use_inside_frames(&script);
    }

    /// The walk-level check behind the byte pins: between any
    /// `BEGIN TRANSACTION;` and its `COMMIT;`, no line starts a `USE`
    /// statement — the emitter's standing contract (the session
    /// switches ride outside
    /// the frames they open).
    fn assert_no_use_inside_frames(script: &str) {
        let mut inside = false;
        for line in script.lines() {
            if line == "BEGIN TRANSACTION;" {
                assert!(!inside, "a frame opened inside a frame:\n{script}");
                inside = true;
                continue;
            }
            if line == "COMMIT;" {
                assert!(inside, "a COMMIT without a frame:\n{script}");
                inside = false;
                continue;
            }
            if inside {
                assert!(
                    !line.starts_with("USE "),
                    "a USE sits inside a frame:\n{script}"
                );
            }
        }
        assert!(!inside, "an unclosed frame:\n{script}");
    }

    /// The frame count the draft writers gate on
    /// ([`convergence_script_frames`]): one per content-bearing scope
    /// section — one for a single-scope script, two for the
    /// two-database corpus above — and zero for an owner-defines-only
    /// script (nothing batchable; the file format's frame would wrap
    /// nothing, so `generate --save` refuses it).
    #[test]
    fn frame_count_is_one_per_scope_section() {
        let single = parse_file_sdl("namespace app; database main;");
        let findings = vec![sdl_only(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Table,
            "t",
            parse_stmt("table t;"),
        )];
        assert_eq!(convergence_script_frames(&findings, &single), 1);
        let owners_only = parse_file_sdl("namespace alpha;");
        let owner_findings = vec![sdl_only(
            ScopeKey::Root,
            Family::Namespace,
            "alpha",
            parse_stmt("namespace alpha;"),
        )];
        assert_eq!(convergence_script_frames(&owner_findings, &owners_only), 0);
        // The multi-scope corpus pins two frames; reuse its shape.
        let multi = parse_file_sdl("namespace alpha; database one; namespace beta; database two;");
        let multi_findings = vec![
            sdl_only(
                ScopeKey::Database("alpha".into(), "one".into()),
                Family::Table,
                "fresh1",
                parse_stmt("table fresh1;"),
            ),
            sdl_only(
                ScopeKey::Database("beta".into(), "two".into()),
                Family::Table,
                "fresh2",
                parse_stmt("table fresh2;"),
            ),
        ];
        assert_eq!(convergence_script_frames(&multi_findings, &multi), 2);
    }

    /// Non-spellable keys fail closed to the manual-removal comment:
    /// a raw body whose header never classified (empty key — no
    /// trailing space), a wildcard field key, and a quoted field name
    /// containing a dot (REMOVE FIELD would re-spell it as a
    /// different PATH). The field slots still ride — the data stakes
    /// hold whatever spells the removal — but their `e.g.` lines
    /// drop: a non-spellable path would make the example itself a
    /// lie. The raw family carries no slot (catalog machinery).
    #[test]
    fn fail_closed_arms() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            db_only(
                scope.clone(),
                Family::Raw,
                "",
                parse_stmt("param $x = 1;"),
                None,
            ),
            db_only(
                scope.clone(),
                Family::Field,
                "matrix.*.*",
                parse_stmt("field matrix.*.*: int { on: t; };"),
                Some("t"),
            ),
            db_only(
                scope,
                Family::Field,
                "a.b",
                // The QUOTED single-segment name — its flattened key
                // equals the path `a.b`, but the AST's one Name
                // segment carries the dot, so the fail-closed arm
                // (not the path spelling) fires.
                parse_stmt("field `a.b`: int { on: t; };"),
                Some("t"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        assert_eq!(
            script,
            format!(
                "{HEADER}{}\
                 USE NS app DB main;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope db:app/main\n\
                 -- section: db:app/main (0 defines, 3 removals)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Removals - objects the schema file no longer declares\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 -- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n\
                 -- Removes field a.b on table t. The stored values remain on\n\
                 --    the rows, outside every field definition.\n\
                 -- - Write the move query between the markers; it runs before\n\
                 --   the removal, in this same transaction.\n\
                 -- - The values remain after this script; unset them once the\n\
                 --   copy is verified.\n\
                 -- >>> SURFORM REQUIRED: END <<<\n\
                 \n\
                 -- manual removal required: db:app/main field a.b\n\
                 \n\
                 -- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n\
                 -- Removes field matrix.*.* on table t. The stored values\n\
                 --    remain on the rows, outside every field definition.\n\
                 -- - Write the move query between the markers; it runs before\n\
                 --   the removal, in this same transaction.\n\
                 -- - The values remain after this script; unset them once the\n\
                 --   copy is verified.\n\
                 -- >>> SURFORM REQUIRED: END <<<\n\
                 \n\
                 -- manual removal required: db:app/main field matrix.*.*\n\
                 \n\
                 -- manual removal required: db:app/main raw\n\
                 \n\
                 COMMIT;\n",
                summary(3, 0, 3, 2, 0, 1),
            )
        );
    }

    /// The catalog-integrity override: an analyzer removes AFTER the
    /// table and index that reference it (upstream refuses
    /// `REMOVE ANALYZER` while a fulltext index points at it —
    /// `AzInUse`, which `IF EXISTS` does not soften), even though
    /// alphabetical order would put it first. The table carries the
    /// WARNING anchor; the index and analyzer (catalog-only) none.
    #[test]
    fn analyzer_removes_after_its_references() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            db_only(
                scope.clone(),
                Family::Analyzer,
                "arts",
                parse_stmt("analyzer arts { tokenizers: class; };"),
                None,
            ),
            db_only(
                scope.clone(),
                Family::Index,
                "ix",
                parse_stmt("index fulltext ix { on: t; fields: x; analyzer: arts; };"),
                Some("t"),
            ),
            db_only(scope, Family::Table, "t", parse_stmt("table t;"), None),
        ];
        assert_eq!(
            convergence_script(&findings, &sdl),
            format!(
                "{HEADER}{}\
                 USE NS app DB main;\n\
                 \n\
                 {RULE}\n\
                 -- ## Scope db:app/main\n\
                 -- section: db:app/main (0 defines, 3 removals)\n\
                 \n\
                 BEGIN TRANSACTION;\n\
                 \n\
                 {RULE}\n\
                 -- ## Removals - objects the schema file no longer declares\n\
                 --\n\
                 -- Each removal is idempotent (IF EXISTS): a re-run skips it.\n\
                 \n\
                 REMOVE INDEX IF EXISTS ix ON t;\n\
                 \n\
                 -- >>> SURFORM WARNING: DATA LOSS <<<\n\
                 -- Removes table t - every document in it is deleted.\n\
                 \n\
                 -- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n\
                 -- - Write the export, archive, or move query between the\n\
                 --   markers; it runs before the removal, in this same\n\
                 --   transaction.\n\
                 -- e.g. INSERT INTO archive (SELECT * FROM t);\n\
                 -- >>> SURFORM REQUIRED: END <<<\n\
                 REMOVE TABLE IF EXISTS t;\n\
                 REMOVE ANALYZER IF EXISTS arts;\n\
                 \n\
                 COMMIT;\n",
                summary(3, 0, 3, 1, 0, 1),
            )
        );
    }

    /// The script is a pure function of the findings: the same set
    /// in a shuffled order renders byte-identically — the slot
    /// blocks included (they derive from the findings exactly as the
    /// statements do).
    #[test]
    fn pure_function_of_the_findings() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![
            sdl_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Param,
                "fresh",
                parse_stmt("param $fresh = 1;"),
            ),
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "stale",
                parse_stmt("table stale;"),
                None,
            ),
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "gone",
                parse_stmt("field gone: int { on: stale; };"),
                Some("stale"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains("-- >>> SURFORM REQUIRED: DATA MIGRATION <<<"),
            "the slot-bearing corpus is the point: {script}"
        );
        let mut shuffled = findings.clone();
        shuffled.reverse();
        assert_eq!(script, convergence_script(&shuffled, &sdl));
        let mut rotated = findings.clone();
        rotated.rotate_left(1);
        assert_eq!(script, convergence_script(&rotated, &sdl));
        rotated.rotate_left(1);
        assert_eq!(script, convergence_script(&rotated, &sdl));
    }

    /// The slot taxonomy's quiet half: event/index/analyzer (and
    /// every non-table non-field family) is catalog or rebuild
    /// machinery — no REQUIRED slot, no WARNING anchor, the script
    /// stays silent where nothing is lost.
    #[test]
    fn catalog_families_carry_neither_slot_nor_warning() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            db_only(
                scope.clone(),
                Family::Event,
                "e",
                parse_stmt("event e { on: t; when: true; then: (create x); };"),
                Some("t"),
            ),
            db_only(
                scope.clone(),
                Family::Index,
                "ix",
                parse_stmt("index unique ix { on: t; fields: x; };"),
                Some("t"),
            ),
            db_only(
                scope.clone(),
                Family::Analyzer,
                "arts",
                parse_stmt("analyzer arts { tokenizers: class; };"),
                None,
            ),
            db_only(
                scope.clone(),
                Family::Param,
                "p",
                parse_stmt("param $p = 1;"),
                None,
            ),
            db_only(
                scope,
                Family::Access,
                "old",
                parse_stmt("access old { on: db; type: record; };"),
                None,
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        assert!(
            !script.contains(">>> SURFORM REQUIRED:") && !script.contains(">>> SURFORM WARNING:"),
            "nothing here orphans data:\n{script}"
        );
    }

    // ---------------------------------------------------------------
    // The UNIQUE-index apply-time hazard note (issue #80): the one
    // row-validating define names itself in the script, at the line
    // that will fail.
    // ---------------------------------------------------------------

    /// A unique index define over a table that exists database-side
    /// carries the one-line hazard note above its define; its
    /// apply-pending twin (an empty key range — nothing to collide)
    /// carries none.
    #[test]
    fn a_unique_index_over_a_live_table_names_the_hazard() {
        let sdl = parse_file_sdl("namespace app; database main; table t;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let mut live = sdl_only(
            scope.clone(),
            Family::Index,
            "ix",
            parse_stmt("index unique ix { on: t; fields: email; };"),
        );
        live.on_table_exists = true;
        let pending = sdl_only(
            scope,
            Family::Index,
            "ux",
            parse_stmt("index unique ux { on: t; fields: other; };"),
        );
        let script = convergence_script(&[live, pending], &sdl);
        let note_at = script
            .find("-- note: this unique index builds over existing rows")
            .expect("the hazard note rides");
        let define_at = script
            .find("DEFINE INDEX OVERWRITE ix")
            .expect("the define");
        assert!(
            note_at < define_at,
            "the note rides above its define:\n{script}"
        );
        let ux_at = script.find("DEFINE INDEX OVERWRITE ux").expect("twin");
        assert!(
            !script[..ux_at].ends_with("clean or dedupe the data first\n"),
            "the apply-pending twin carries no note:\n{script}"
        );
        assert_eq!(script.matches("-- note:").count(), 1, "{script}");
    }

    /// A drifted table's authored unique index member carries the
    /// note above its piece (the table emission's), and the fields
    /// naming a re-keyed field say so — the dedupe-before,
    /// rebuild-after order.
    #[test]
    fn the_unique_hazard_names_its_rekeyed_fields() {
        let sdl = parse_file_sdl(
            "namespace app; database main;\n\
             table member_of {\n\
             index unique uq { fields: x, out; };\n\
             index plain_ix { fields: out; };\n\
             field x: string;\n\
             field out: int;\n\
             };\n",
        );
        let mut finding = drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: member_of; };"),
            parse_stmt("field x: int { on: member_of; };"),
        );
        finding.on_table_exists = true;
        let script = convergence_script(&[finding], &sdl);
        assert!(
            script.contains(
                "-- note: this unique index builds over existing rows - the\n\
                 --    define fails if any two rows collide; its fields cover\n\
                 --    the re-keyed field(s) `x` - clean or dedupe before,\n\
                 --    rebuild after the data step\n"
            ),
            "{script}"
        );
        // The note rides above the uq piece, inside the frame — the
        // member pieces render after the table head (the emission's
        // own shape), so the head's final define precedes it.
        let note_at = script.find("-- note:").expect("the note");
        let define_at = script
            .find("DEFINE INDEX OVERWRITE uq")
            .expect("the member define");
        let final_at = script
            .find("DEFINE FIELD OVERWRITE x ON member_of TYPE string;")
            .expect("the final define");
        assert!(final_at < note_at && note_at < define_at, "{script}");
        // The plain index over the untouched field carries none — the
        // uq note is the script's only hazard note.
        assert_eq!(script.matches("-- note:").count(), 1, "{script}");
    }

    // ---------------------------------------------------------------
    // The header's honesty (issue #81) and the run summary (issue
    // #79's script half)
    // ---------------------------------------------------------------

    /// The raw-corner stanza is CONDITIONAL: a script carrying a raw
    /// body reads the multi-statement corner line; a typed-only
    /// script does not (most scripts hold no raws).
    #[test]
    fn the_raw_corner_stanza_rides_only_raw_scripts() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let raw = vec![sdl_only(
            scope.clone(),
            Family::Field,
            "x",
            parse_stmt("raw { DEFINE FIELD OVERWRITE x ON t TYPE int; };"),
        )];
        let typed = vec![sdl_only(scope, Family::Table, "t", parse_stmt("table t;"))];
        let raw_script = convergence_script(&raw, &sdl);
        let typed_script = convergence_script(&typed, &sdl);
        assert!(
            raw_script.contains("-- Raw-authored statements ride this script:"),
            "{raw_script}"
        );
        assert!(
            !typed_script.contains("raw-authored statements"),
            "{typed_script}"
        );
        // Both carry the unconditional re-run line and the summary.
        assert!(raw_script.contains("-- Re-runs are safe: OVERWRITE redefines;"));
        assert!(typed_script.contains(&summary(1, 1, 0, 0, 0, 1)));
    }

    /// The collapse arm names what the owner REMOVE consumes
    /// (issue #85): a namespace-scope finding under a retiring
    /// namespace appends its `-- includes` line inside the WARNING
    /// block — the destroyed content is not silent.
    #[test]
    fn a_collapsing_namespace_names_its_consumed_findings() {
        let sdl = parse_file_sdl("namespace app;");
        let findings = vec![db_only(
            ScopeKey::Namespace("stray".into()),
            Family::Param,
            "p",
            parse_stmt("param $p = 1;"),
            None,
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains(
                "-- section: ns:stray - retired (the schema file no longer \
                 declares this namespace)\n"
            ),
            "{script}"
        );
        assert!(
            script.contains(
                "-- includes param p (the schema file no longer declares this \
                 namespace)\n"
            ),
            "{script}"
        );
        // The includes line sits inside the WARNING block, above the
        // REMOVE.
        let warning_at = script
            .find("-- >>> SURFORM WARNING: DATA LOSS <<<\n")
            .expect("the warning");
        let includes_at = script.find("-- includes param p").expect("the includes");
        let remove_at = script
            .find("REMOVE NAMESPACE IF EXISTS stray;")
            .expect("it");
        assert!(
            warning_at < includes_at && includes_at < remove_at,
            "{script}"
        );
    }

    /// The section labels (issue #82): every frame names its scope in
    /// the report's scope_label vocabulary, with the frame's own
    /// counts — including the root frame.
    #[test]
    fn every_frame_names_its_scope() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Param,
                "p",
                parse_stmt("param $p = 1;"),
            ),
            db_only(
                scope,
                Family::Param,
                "old",
                parse_stmt("param $old = 1;"),
                None,
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains("-- section: db:app/main (1 define, 1 removal)\n"),
            "{script}"
        );
        let root = vec![db_only(
            ScopeKey::Root,
            Family::Param,
            "legacy",
            parse_stmt("param $legacy = 1;"),
            None,
        )];
        assert!(
            convergence_script(&root, &sdl).contains("-- section: root (0 defines, 1 removal)\n")
        );
    }

    /// The summary reconciles expected-unmanaged findings like the
    /// report's line: they carry no work, and when present the
    /// informational clause closes the arithmetic.
    #[test]
    fn the_summary_reconciles_unmanaged_findings() {
        let sdl = parse_file_sdl("namespace app;");
        let findings = vec![
            sdl_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "t",
                parse_stmt("table t;"),
            ),
            DriftFinding {
                on_table_exists: false,
                scope: ScopeKey::Root,
                family: Family::Namespace,
                key: "default".to_string(),
                class: DriftClass::ExpectedUnmanaged {
                    database: Box::new(parse_stmt("namespace default;")),
                },
            },
        ];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains(&summary_unmanaged(2, 1, 0, 0, 0, 1, 1)),
            "{script}"
        );
        assert!(
            script.contains("1 expected-unmanaged (informational)"),
            "{script}"
        );
    }

    /// The collapsed scopes' slot obligations die with the owner
    /// REMOVE: the summary's slot count EXCLUDES them (the includes
    /// lines name the findings; no REQUIRED region exists to fill).
    #[test]
    fn the_summary_excludes_the_collapsed_scopes_slots() {
        let sdl = parse_file_sdl("namespace app;");
        let findings = vec![
            db_only(
                ScopeKey::Database("app".into(), "stale".into()),
                Family::Table,
                "t",
                parse_stmt("table t;"),
                None,
            ),
            db_only(
                ScopeKey::Database("app".into(), "stale".into()),
                Family::Field,
                "x",
                parse_stmt("field x: int { on: t; };"),
                Some("t"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains(&summary(2, 0, 2, 0, 0, 1)),
            "no slots survive the collapse:\n{script}"
        );
        assert!(
            !script.contains("SURFORM REQUIRED: DATA MIGRATION"),
            "no REQUIRED region rides the collapse:\n{script}"
        );
    }

    /// A Diverged field retypes without destroying values (the rows
    /// keep them; the new kind governs future writes) — never a slot
    /// in the script; the REPORT carries the informational note
    /// instead (pinned with the report tests).
    #[test]
    fn a_diverged_field_without_a_kind_change_never_carries_a_slot() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: t; default: 'b'; };"),
            parse_stmt("field x: string { on: t; default: 'a'; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            !script.contains(">>> SURFORM REQUIRED:") && !script.contains(">>> SURFORM WARNING:"),
            "a non-kind retype orphans nothing and stages nothing:\n{script}"
        );
        assert!(
            !script.contains("## Staged migration"),
            "no staged prelude without a kind change:\n{script}"
        );
    }

    // ---------------------------------------------------------------
    // The staged migrations (issue #65): a kind change re-keys the
    // table through the relaxed shell → REQUIRED data section →
    // final definitions sequence. The probe-pinned order: the data
    // step fails under the old kind, the define re-validates nothing.
    // ---------------------------------------------------------------

    /// The staged shape for the issue's exact class: the shell and
    /// the kind relax ride BEFORE the data section, the final define
    /// (the SDL's own kind) rides AFTER it — all inside the section's
    /// transaction frame.
    #[test]
    fn a_kind_change_stages_the_table() {
        let sdl = parse_file_sdl("namespace app; database main; table member_of; table t;");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: member_of; };"),
            parse_stmt("field x: int { on: member_of; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        let shell_at = script
            .find("DEFINE TABLE OVERWRITE member_of TYPE ANY SCHEMALESS;\n")
            .expect("the relaxed shell rides");
        let relax_at = script
            .find("DEFINE FIELD OVERWRITE x ON member_of TYPE any;\n")
            .expect("the kind relax rides");
        let slot_at = script
            .find("-- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n")
            .expect("the data section rides");
        let final_at = script
            .find("DEFINE FIELD OVERWRITE x ON member_of TYPE string;\n")
            .expect("the final define rides the SDL's kind");
        let begin_at = script.find("BEGIN TRANSACTION;\n").expect("the frame");
        let commit_at = script.rfind("COMMIT;\n").expect("the frame");
        assert!(
            begin_at < shell_at
                && shell_at < relax_at
                && relax_at < slot_at
                && slot_at < final_at
                && final_at < commit_at,
            "the staged order is shell → relax → section → final, all in the frame:\n{script}"
        );
        // The scalar re-key's e.g. line (issue #84): the probe-pinned
        // cast spelling, the prose naming the re-keyed field.
        assert!(
            script.contains("-- e.g. UPDATE member_of SET x = <string> x;\n"),
            "{script}"
        );
        assert!(
            script.contains("-- Re-keyed fields: `x` -> string.\n"),
            "{script}"
        );
        assert!(
            !script.contains(":r1;"),
            "the record shape is not this example:\n{script}"
        );
        assert!(
            !script.contains("<<<<<<<"),
            "the stdout carrier keeps comment banners, never bare markers:\n{script}"
        );
    }

    /// The hostile-kind pin (the report's
    /// script-surface twin, one category over): a kind spelling
    /// interpolated into the `-- Re-keyed fields:` comment — a quoted
    /// ident inside a `record<...>` kind may carry NEL/LS/PS (the
    /// vendored line-comment eater terminates at all five spellings),
    /// and the RAW tail would execute as SurrealQL inside the frame.
    /// The kind folds through [`comment_text`] like every prose-line
    /// name: the visible escape spelling rides ONE comment line, the
    /// tail never executable.
    #[test]
    fn a_hostile_kind_folds_in_the_rekeyed_fields_comment() {
        for sep in ['\u{85}', '\u{2028}', '\u{2029}'] {
            let hostile = format!("record<`a{sep}REMOVE TABLE victim;--`>");
            let sdl = parse_file_sdl("namespace app; database main; table member_of; table t;");
            let findings = vec![drift(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "x",
                parse_stmt(&format!("field x: {hostile} {{ on: member_of; }};")),
                parse_stmt("field x: string { on: member_of; };"),
            )];
            let script = convergence_script(&findings, &sdl);
            let folded = comment_text(&hostile);
            assert!(
                script
                    .lines()
                    .any(|line| line.starts_with("-- Re-keyed fields: ") && line.contains(&folded)),
                "the kind folds on U+{:X}:\n{script}",
                sep as u32
            );
            // The tail-invariant rides the comment-scoped helper
            // below (all five terminators via
            // `is_line_comment_terminator`): `lines()` splits on \n
            // only, so a line-starts-with walk cannot see a
            // NEL/LS/PS-led tail, and the raw byte legitimately rides
            // the final define's STATEMENT line (backtick idents take
            // any bytes — the statement contract).
            assert_comment_lines_carry_no_controls(&script);
        }
    }

    /// The hostile-INDEX pin: the index-rebuild note's pair is a COMMENTED runnable
    /// statement — the user uncomments it — and a hostile UNIQUE
    /// index name rides `ident_text` verbatim (NEL included; LF/CR
    /// arrive escape_with-folded). Folding to the `\u{…}` display
    /// spelling would round-trip (the vendored backtick unescape
    /// decodes the braced form), but it would fork a second
    /// ident-spelling rule beside `ident_text`'s
    /// one-escaping-rule-one-home contract — so the fail-closed
    /// shape is the slot examples': the pair DROPS for a spelling
    /// the comment cannot carry, one prose line points at the schema
    /// file, and the note's other indexes stay uncommentable. The
    /// raw byte never rides a comment.
    #[test]
    fn a_hostile_index_name_drops_its_note_pair() {
        let sdl = parse_file_sdl(
            "namespace app; database main; table member_of; table t;\n\
             table member_of {\n\
             index unique uq { fields: out; };\n\
             index unique `a\u{85}REMOVE TABLE victim;--` { fields: x; };\n\
             field x: string;\n\
             field out: int;\n\
             };",
        );
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: member_of; };"),
            parse_stmt("field x: int { on: member_of; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        // The note still serves the clean index (uncomment-and-run
        // stays useful).
        assert!(
            script.contains("-- REMOVE INDEX IF EXISTS uq ON member_of;\n"),
            "the clean index's uncommentable line rides:\n{script}"
        );
        // The hostile pair drops whole: no COMMENT line spells the
        // name. (The staged DEFINES region still re-emits the index
        // as a STATEMENT line — raw NEL inside backticks is the
        // statement contract, valid SurrealQL the vendored lexer
        // accepts; only the commented pair is the comment surface.)
        for line in script.lines().filter(|line| line.starts_with("--")) {
            assert!(
                !line.contains("REMOVE TABLE victim"),
                "the hostile pair must drop whole:\n{line}"
            );
        }
        // The pointer names the honest path (the schema file).
        assert!(
            script
                .lines()
                .any(|line| line.starts_with("-- An index here carries a name")),
            "the pointer rides:\n{script}"
        );
        assert_comment_lines_carry_no_controls(&script);
    }

    /// The e.g. line's record shape: a re-key onto `record<seat>`
    /// spells the invented-but-runnable target (`seat:r1`), the
    /// table-slot's `archive` precedent — real SurrealQL, the human
    /// names the real target.
    #[test]
    fn the_staged_example_names_the_record_target() {
        let sdl = parse_file_sdl("namespace app; database main; table member_of; table t;");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "in",
            parse_stmt("field in: record<seat> { on: member_of; };"),
            parse_stmt("field in: record<user> | record<fleet_account> { on: member_of; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains("-- e.g. UPDATE member_of SET `in` = seat:r1;\n"),
            "{script}"
        );
    }

    /// The e.g. line's fail-closed edge: a kind outside the cast menu
    /// (`option<string>`) spells prose only — no invented cast, no
    /// record id — while the prose still names the re-keyed field.
    #[test]
    fn an_option_kind_drops_the_example_but_names_the_field() {
        let sdl = parse_file_sdl("namespace app; database main; table t;");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: option<string> { on: t; };"),
            parse_stmt("field x: int { on: t; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(!script.contains("-- e.g. UPDATE"), "{script}");
        assert!(
            script.contains("-- Re-keyed fields: `x` -> option<string>.\n"),
            "{script}"
        );
    }

    /// The relax line is MINIMAL: a clause-carrying kind change (the
    /// struct-update form leaked default/value/
    /// assert/flexible into the relaxed window, and flexible + `any`
    /// is an engine parse error) emits exactly the three-part define.
    #[test]
    fn the_relax_line_carries_no_clauses() {
        let sdl = parse_file_sdl("namespace app; database main; table member_of; table t;");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "m",
            parse_stmt("field m: object { on: t; flexible; default: {}; assert: m != NONE; };"),
            parse_stmt("field m: any { on: t; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains("DEFINE FIELD OVERWRITE `m` ON t TYPE any;\n"),
            "the minimal relax rides:\n{script}"
        );
        for clause in [
            "FLEXIBLE",
            "DEFAULT",
            "ASSERT",
            "VALUE",
            "READONLY",
            "REFERENCE",
        ] {
            assert!(
                !script.contains(&format!("TYPE any {clause}")),
                "no clause leaks into the relaxed window ({clause}):\n{script}"
            );
        }
    }

    /// A staged table the SDL never declares (a standalone field's
    /// host) takes NO shell and NO restore: it is unmanaged
    /// database-side, and the script touches nothing it never owned —
    /// the kind relax line alone lifts the old kind, the section and
    /// the slot ride unchanged.
    #[test]
    fn an_undeclared_host_takes_no_shell() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: unmanaged; };"),
            parse_stmt("field x: int { on: unmanaged; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            !script.contains("DEFINE TABLE OVERWRITE unmanaged"),
            "no shell on an undeclared host:\n{script}"
        );
        assert!(
            script.contains("DEFINE FIELD OVERWRITE x ON unmanaged TYPE any;\n"),
            "the relax rides:\n{script}"
        );
        assert!(
            script.contains("-- >>> SURFORM REQUIRED: DATA MIGRATION <<<\n"),
            "the section rides:\n{script}"
        );
    }

    /// The field pass honors the table pass's DECLINED keys: a table
    /// whose database side is a view never stages, even when a
    /// kind-changed field child would re-open it (the shell would
    /// retype the view mid-transaction
    /// for no re-keyable rows).
    #[test]
    fn a_declined_view_host_never_reopens_through_its_field() {
        let sdl = parse_file_sdl("namespace app; database main; table v;");
        let findings = vec![
            drift(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "v",
                parse_stmt("table v;"),
                parse_stmt("table view v { select x from base; };"),
            ),
            drift(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "x",
                parse_stmt("field x: string { on: v; };"),
                parse_stmt("field x: int { on: v; };"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        assert!(
            !script.contains("DEFINE TABLE OVERWRITE v TYPE ANY SCHEMALESS"),
            "the declined view host takes no shell:\n{script}"
        );
        assert!(
            !script.contains(">>> SURFORM REQUIRED:"),
            "no data section for a view (no re-keyable rows):\n{script}"
        );
    }

    /// A mixin-spread-landed UNIQUE index reaches the rebuild note:
    /// the SDL walk runs over the EXPANDED file (the raw file's
    /// spread placeholders carry no members).
    #[test]
    fn a_spread_landed_index_reaches_the_rebuild_note() {
        let sdl = parse_file_sdl(
            "namespace app; database main;\n\
             mixin edge_base {\n\
             index unique uq { fields: x, out; };\n\
             };\n\
             table member_of {\n\
             ...edge_base;\n\
             field x: string;\n\
             field out: int;\n\
             };\n",
        );
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: member_of; };"),
            parse_stmt("field x: int { on: member_of; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        let note_at = script
            .find("-- >>> SURFORM NOTE: INDEX REBUILD <<<\n")
            .expect("the note rides the spread-landed index");
        let remove_at = script
            .find("-- REMOVE INDEX IF EXISTS uq ON member_of;\n")
            .expect("the spread-landed index rebuilds");
        assert!(note_at < remove_at, "{script}");
    }

    /// The saved-draft carrier: the staged section's delimiters are
    /// the bare resolution markers — `commit` refuses while they
    /// remain — with the prose byte-shared.
    #[test]
    fn the_staged_section_uses_resolution_markers_in_saved_drafts() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: t; };"),
            parse_stmt("field x: int { on: t; };"),
        )];
        let marked = convergence_script_styled(&findings, &sdl, SlotStyle::ResolutionMarkers);
        assert!(
            marked.contains("<<<<<<< SURFORM SLOT (db:app/main: staged table t)\n"),
            "{marked}"
        );
        assert!(marked.contains(">>>>>>> SURFORM SLOT END\n"), "{marked}");
        assert!(
            !marked.contains(">>> SURFORM REQUIRED:"),
            "the bare carrier never mixes banners:\n{marked}"
        );
    }

    /// A table-level drift with no kind change (permissions) stages a
    /// shell only — the relaxed window opens, but nothing re-keys, so
    /// no data section and no manifest row.
    #[test]
    fn a_permissions_drift_stages_a_shell_without_a_data_section() {
        let sdl = parse_file_sdl("namespace app; database main; table t;");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Table,
            "t",
            parse_stmt("table t { permissions: full; };"),
            parse_stmt("table t { permissions: none; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains("DEFINE TABLE OVERWRITE t TYPE ANY SCHEMALESS;\n"),
            "the shell relaxes the mode and permissions:\n{script}"
        );
        assert!(
            !script.contains(">>> SURFORM REQUIRED:"),
            "nothing re-keys, so no data section:\n{script}"
        );
    }

    /// A view table never stages — its AS SELECT is the whole
    /// definition, and a `SCHEMALESS` shell would destroy it (the
    /// SDL-side exclusion the findings cannot see).
    #[test]
    fn a_view_table_never_stages() {
        let sdl =
            parse_file_sdl("namespace app; database main; table view v { select x from base; };");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: v; };"),
            parse_stmt("field x: int { on: v; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            !script.contains("DEFINE TABLE OVERWRITE v"),
            "the view never takes a shell:\n{script}"
        );
        assert!(
            !script.contains("## Staged migration"),
            "no staged prelude:\n{script}"
        );
    }

    /// A lightweight relation never stages — a lightweight host
    /// refuses child defines, so the relax lines would fail the
    /// transaction.
    #[test]
    fn a_lightweight_table_never_stages() {
        let sdl = parse_file_sdl(
            "namespace app; database main; table user->member_of->post lightweight;",
        );
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "role",
            parse_stmt("field role: string { on: member_of; };"),
            parse_stmt("field role: int { on: member_of; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            !script.contains("## Staged migration"),
            "the lightweight host never takes the prelude:\n{script}"
        );
    }

    /// The index-rebuild note rides the staged table after the final
    /// defines: every UNIQUE index on it, plus every index whose
    /// fields name a re-keyed field — commented REMOVE + the SDL's
    /// own define, the user's call.
    #[test]
    fn the_index_rebuild_note_rides_the_staged_table() {
        let sdl = parse_file_sdl(
            "namespace app; database main;\n\
             table member_of {\n\
             index unique edge_unique { fields: x, out; };\n\
             index plain_ix { fields: x; };\n\
             index other_ix { fields: out; };\n\
             field x: string;\n\
             field out: int;\n\
             };\n",
        );
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: member_of; };"),
            parse_stmt("field x: int { on: member_of; };"),
        )];
        let script = convergence_script(&findings, &sdl);
        let note_at = script
            .find("-- >>> SURFORM NOTE: INDEX REBUILD <<<\n")
            .expect("the note rides");
        let unique_at = script
            .find("-- REMOVE INDEX IF EXISTS edge_unique ON member_of;\n")
            .expect("the unique index rebuilds");
        let plain_at = script
            .find("-- REMOVE INDEX IF EXISTS plain_ix ON member_of;\n")
            .expect("the plain index over the re-keyed field rebuilds");
        let final_at = script
            .find("DEFINE FIELD OVERWRITE x ON member_of TYPE string;\n")
            .expect("the final define");
        let other_at = script.find("other_ix");
        assert!(final_at < note_at && note_at < unique_at && unique_at < plain_at);
        assert!(
            other_at.is_none() || !script[note_at..].contains("-- REMOVE INDEX IF EXISTS other_ix"),
            "the untouched index stays out of the note:\n{script}"
        );
        // The SDL's own defines re-apply uncommented.
        assert!(
            script
                .contains("DEFINE INDEX OVERWRITE edge_unique ON member_of FIELDS x, out UNIQUE;"),
            "{script}"
        );
    }

    /// The staged defines ride the structured carrier — the shell and
    /// the relax lines are typed statements the apply flow executes.
    #[test]
    fn the_carrier_carries_the_staged_defines() {
        let sdl = parse_file_sdl("namespace app; database main; table t;");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: t; };"),
            parse_stmt("field x: int { on: t; };"),
        )];
        let parts = convergence_script_parts(&findings, &sdl);
        let shell = crate::codegen::emit_statement_alone(&shell_stmt("t"));
        assert!(
            parts
                .statements
                .iter()
                .any(|stmt| stmt.starts_with("DEFINE TABLE OVERWRITE t")),
            "the shell rides the carrier:\n{}",
            parts.text
        );
        assert!(
            parts
                .statements
                .iter()
                .any(|stmt| stmt.starts_with("DEFINE FIELD OVERWRITE x ON t TYPE any")),
            "the relax rides the carrier:\n{}",
            parts.text
        );
        let _ = shell;
    }

    // ---------------------------------------------------------------
    // The saved drafts' git-style resolution markers. The
    // stdout path keeps the comment banners (byte-frozen elsewhere in
    // this module); the marker carrier swaps ONLY the two delimiter
    // lines — bare text, an unresolved draft is deliberately
    // un-runnable SurrealQL — and shares every byte between them.
    // ---------------------------------------------------------------

    /// The marker carrier: the region opens with the slot's NAME
    /// (scope: family key) and closes with the END token; the WARNING
    /// anchor stays a permanent comment OUTSIDE the region; the
    /// guidance prose is byte-shared with the banner carrier's.
    #[test]
    fn the_saved_draft_delimits_slots_with_resolution_markers() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "stale",
                parse_stmt("table stale;"),
                None,
            ),
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "gone",
                parse_stmt("field gone: int { on: stale; };"),
                Some("stale"),
            ),
        ];
        let marked = convergence_script_styled(&findings, &sdl, SlotStyle::ResolutionMarkers);
        // The regions: named openers, the END token, no banners.
        assert!(
            marked.contains("<<<<<<< SURFORM SLOT (db:app/main: table stale)\n"),
            "{marked}"
        );
        // The ON edge rides the field's marker (issue #86) — the
        // manifest's line, not a trimmed one.
        assert!(
            marked.contains("<<<<<<< SURFORM SLOT (db:app/main: field gone ON stale)\n"),
            "{marked}"
        );
        assert_eq!(
            marked.matches(crate::migrate::SLOT_CLOSE).count(),
            2,
            "{marked}"
        );
        assert!(!marked.contains(">>> SURFORM REQUIRED"), "{marked}");
        // The WARNING anchor persists (documentation, never a marker).
        assert!(
            marked.contains("-- >>> SURFORM WARNING: DATA LOSS <<<\n"),
            "{marked}"
        );
        // The prose between the delimiters is byte-shared: strip the
        // two carriers' delimiter lines and the scripts agree.
        let strip = |script: &str| -> String {
            script
                .lines()
                .filter(|line| {
                    !line.starts_with(crate::migrate::SLOT_OPEN_PREFIX)
                        && *line != crate::migrate::SLOT_CLOSE
                        && *line != "-- >>> SURFORM REQUIRED: DATA MIGRATION <<<"
                        && *line != "-- >>> SURFORM REQUIRED: END <<<"
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(
            strip(&marked),
            strip(&convergence_script(&findings, &sdl)),
            "only the delimiters differ between the carriers"
        );
    }

    /// The multiset purity extends to the saved files: the marker
    /// carrier is the same permutation-stable render the banner
    /// carrier pins (the draft writer adds only an order-independent
    /// header on top).
    #[test]
    fn the_marked_script_stays_a_pure_function_of_the_findings() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![
            sdl_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Param,
                "fresh",
                parse_stmt("param $fresh = 1;"),
            ),
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "stale",
                parse_stmt("table stale;"),
                None,
            ),
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "gone",
                parse_stmt("field gone: int { on: stale; };"),
                Some("stale"),
            ),
        ];
        let marked = convergence_script_styled(&findings, &sdl, SlotStyle::ResolutionMarkers);
        let mut shuffled = findings.clone();
        shuffled.reverse();
        assert_eq!(
            marked,
            convergence_script_styled(&shuffled, &sdl, SlotStyle::ResolutionMarkers)
        );
        let mut rotated = findings.clone();
        rotated.rotate_left(1);
        assert_eq!(
            marked,
            convergence_script_styled(&rotated, &sdl, SlotStyle::ResolutionMarkers)
        );
    }

    // ---------------------------------------------------------------
    // Comment-text sanitization. A backtick ident
    // carries any bytes (the vendored lexer), so database-side names
    // reach the anchors' prose with newlines — and a shaped SurrealQL
    // tail — in hand; every comment interpolation folds.
    // ---------------------------------------------------------------

    /// The sanitizer's fold table: the SurrealQL-spellable controls
    /// mirror `escape_with`'s spellings, the rest spell `\u{…}` — the
    /// vendored lexer's NON-control terminators U+2028/U+2029 spelled
    /// explicitly beside NEL (`is_control` alone
    /// misses the Zl/Zp separators) — and everything else rides
    /// verbatim (`--` and backticks inside a comment line are
    /// inert text; the drift report folds its interpolated names
    /// through the same sanitizer).
    #[test]
    fn comment_text_folds_control_characters() {
        assert_eq!(comment_text("plain"), "plain");
        assert_eq!(comment_text("a\nb"), "a\\nb");
        assert_eq!(comment_text("a\rb"), "a\\rb");
        assert_eq!(comment_text("a\tb"), "a\\tb");
        assert_eq!(comment_text("a\0b"), "a\\0b");
        assert_eq!(comment_text("a\u{8}b"), "a\\u{8}b");
        assert_eq!(comment_text("a\u{c}b"), "a\\fb");
        assert_eq!(comment_text("a\u{1}b"), "a\\u{1}b");
        assert_eq!(comment_text("a\u{7f}b"), "a\\u{7f}b");
        assert_eq!(comment_text("a\u{85}b"), "a\\u{85}b");
        assert_eq!(comment_text("a\u{2028}b"), "a\\u{2028}b");
        assert_eq!(comment_text("a\u{2029}b"), "a\\u{2029}b");
        assert_eq!(comment_text("—`x`--/*"), "—`x`--/*");
    }

    /// The whole script's comment-text invariant: no `--` line ever
    /// carries a raw control byte or line-comment terminator,
    /// whatever the names hold. (The STATEMENT lines legitimately
    /// can — a backtick ident accepts any bytes, and folding there
    /// would change the name.)
    fn assert_comment_lines_carry_no_controls(script: &str) {
        for line in script.lines().filter(|line| line.starts_with("--")) {
            assert!(
                line.chars()
                    .all(|c| !c.is_control() && !is_line_comment_terminator(c)),
                "a comment-breaking character reached a comment line: {line}"
            );
        }
    }

    /// A hostile TABLE name — the shaped injection tail:
    /// a raw newline ends the `--` comment and the tail executes as
    /// SurrealQL inside the script's transaction frames — folds in the
    /// slot prose to the visible `\n` spelling. The slot line stays
    /// one comment (the tail never starts a line) and the legitimate
    /// removal spells the name backtick-escaped as before.
    #[test]
    fn a_hostile_table_name_folds_in_its_slot_prose() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let hostile = "legacy\nREMOVE TABLE victim;--";
        let findings = vec![db_only(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Table,
            hostile,
            parse_stmt(&format!("table `{hostile}`;")),
            None,
        )];
        let script = convergence_script(&findings, &sdl);
        // The WARNING prose folds the name (the fold keeps it one
        // wrap-word, so it rides one comment line whole).
        assert!(
            script
                .lines()
                .any(|line| line.starts_with("-- Removes table ")
                    && line.contains(&comment_text(hostile))),
            "the prose folds the name:\n{script}"
        );
        // The injected form — the tail starting its own line — never
        // appears; the only removal of `victim` is inert comment text.
        assert!(
            !script
                .lines()
                .any(|line| line.starts_with("REMOVE TABLE victim")),
            "the tail must stay inert:\n{script}"
        );
        // The legitimate removal still spells the real name (escaped).
        assert!(
            script.contains(&format!("REMOVE TABLE IF EXISTS {};", ident_text(hostile))),
            "the removal targets the real name:\n{script}"
        );
        assert_comment_lines_carry_no_controls(&script);
    }

    /// The vendored lexer's NON-control line
    /// terminators — U+2028 (LS) and U+2029 (PS) are Zl/Zp and ride
    /// past the Cc fold — fold at a slot site too, one pin per
    /// spelling. A name carrying either separator plus the shaped
    /// tail stays ONE comment line (the tail never starts a line).
    #[test]
    fn a_hostile_name_with_unicode_line_separators_folds_in_its_slot_prose() {
        let sdl = parse_file_sdl("namespace app; database main;");
        for sep in ['\u{2028}', '\u{2029}'] {
            let hostile = format!("legacy{sep}REMOVE TABLE victim;--");
            let findings = vec![db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                &hostile,
                parse_stmt(&format!("table `{hostile}`;")),
                None,
            )];
            let script = convergence_script(&findings, &sdl);
            assert!(
                script.lines().any(|line| {
                    line.starts_with("-- Removes table ")
                        && line.contains(&format!(
                            "legacy\\u{{{:x}}}REMOVE TABLE victim;--",
                            sep as u32
                        ))
                }),
                "the prose folds the separator U+{0:X}:\n{script}",
                sep as u32
            );
            assert!(
                !script
                    .lines()
                    .any(|line| line.starts_with("REMOVE TABLE victim")),
                "the tail must stay inert:\n{script}"
            );
            assert_comment_lines_carry_no_controls(&script);
        }
    }

    /// The SCOPE labels interpolate raw
    /// names too — the `## Scope` header and both `-- section:`
    /// variants — and a backticked namespace name may carry a raw
    /// newline (the vendored lexer accepts it; `scope_label` joins it
    /// verbatim). Every label site folds through [`comment_text`]:
    /// the visible `\n` spelling rides ONE comment line and the
    /// shaped tail never starts a column-0 line — in the live frame's
    /// header pair and in the retired frame's label alike. Sane
    /// labels ride the fold byte-identical (the full-text pins hold
    /// the `db:app/main` literals).
    #[test]
    fn a_hostile_scope_name_folds_in_the_scope_labels() {
        let hostile = "a\nREMOVE TABLE victim;--";
        let folded = comment_text(hostile);
        let sdl = parse_file_sdl("namespace app;");
        // The LIVE frame: a param removal scoped to the hostile
        // namespace renders the `## Scope` header + the section label
        // under `ns:<hostile>`.
        let findings = vec![sdl_only(
            ScopeKey::Namespace(hostile.into()),
            Family::Param,
            "p",
            parse_stmt("param $p = 1;"),
        )];
        let script = convergence_script(&findings, &sdl);
        for prefix in ["-- ## Scope ns:", "-- section: ns:"] {
            assert!(
                script
                    .lines()
                    .any(|line| line.starts_with(prefix) && line.contains(&folded)),
                "the {prefix} label folds the name:\n{script}"
            );
        }
        assert!(
            !script
                .lines()
                .any(|line| line.starts_with("REMOVE TABLE victim")),
            "the tail must stay inert:\n{script}"
        );
        assert_comment_lines_carry_no_controls(&script);
        // The RETIRED frame: a finding scoped to a database the SDL
        // does not declare collapses to the retired scope's frame —
        // its `-- section: db:<hostile>/main - retired` label folds
        // too.
        let findings = vec![db_only(
            ScopeKey::Database(hostile.into(), "main".into()),
            Family::Table,
            "t",
            parse_stmt("table t;"),
            None,
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script
                .lines()
                .any(|line| line.starts_with("-- section: db:")
                    && line.contains(&folded)
                    && line.contains(" - retired ")),
            "the retired label folds the name:\n{script}"
        );
        assert!(
            !script
                .lines()
                .any(|line| line.starts_with("REMOVE TABLE victim")),
            "the tail must stay inert:\n{script}"
        );
        assert_comment_lines_carry_no_controls(&script);
    }

    /// A hostile FIELD name on a hostile table: the slot prose folds
    /// both (the key through the prose, the table beside it), the
    /// non-spellable key drops the `e.g.` line as before, and the
    /// fail-closed manual-removal note folds too — a pre-existing
    /// interpolation site.
    #[test]
    fn a_hostile_field_name_folds_in_its_prose_and_the_manual_note() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let hostile = "f\nREMOVE TABLE victim;--";
        let findings = vec![db_only(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            hostile,
            // A raw body keeps the key-based spellability check — a
            // newline is never word-shaped, so the fail-closed arm
            // fires and the note rides.
            parse_stmt("raw { DEFINE FIELD x ON t; };"),
            Some("t\nx"),
        )];
        let script = convergence_script(&findings, &sdl);
        // The slot prose folds both names (each folded name rides one
        // wrap-word, so neither can start a line).
        assert!(
            script
                .lines()
                .any(|line| line.starts_with("-- Removes field ")
                    && line.contains(&comment_text(hostile))),
            "the prose folds the field name:\n{script}"
        );
        assert!(
            script
                .lines()
                .any(|line| line.starts_with("--") && line.contains(&comment_text("t\nx"))),
            "the prose folds the table name:\n{script}"
        );
        assert!(
            script.contains(&format!(
                "-- manual removal required: db:app/main field {}",
                comment_text(hostile)
            )),
            "the manual note folds:\n{script}"
        );
        assert!(
            !script
                .lines()
                .any(|line| line.starts_with("REMOVE TABLE victim")),
            "the tail must stay inert:\n{script}"
        );
        assert_comment_lines_carry_no_controls(&script);
    }

    /// The `e.g.` spellings fold too (defense in depth): the backtick
    /// escaping already folds newlines, but passes OTHER control
    /// characters verbatim inside the quotes — a table name holding
    /// U+0001 renders the visible spelling in the example line, not a
    /// raw control byte.
    #[test]
    fn the_eg_spellings_fold_control_characters_too() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![db_only(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "a",
            parse_stmt("field a: int { on: t; };"),
            Some("\u{1}x"),
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains("-- e.g. UPDATE `\\u{1}x` SET moved = a;\n"),
            "the example folds the table spelling:\n{script}"
        );
        assert!(
            script.contains("--   copy is verified (e.g. UPDATE `\\u{1}x` UNSET a;).\n"),
            "the cleanup hint folds it too:\n{script}"
        );
        assert_comment_lines_carry_no_controls(&script);
    }

    /// The owner-manual notes fold (the key-mismatch corner's
    /// interpolation sites): an SDL-DECLARED hostile namespace and
    /// database — the SDL's own quoted idents accept the bytes —
    /// reach the note through the scope label and the key, folded.
    #[test]
    fn a_hostile_owner_key_folds_in_the_owner_manual_note() {
        let sdl = parse_file_sdl("namespace `a\nb`; database `c\nd`;");
        let findings = vec![db_only(
            ScopeKey::Namespace("a\nb".into()),
            Family::Database,
            "c\nd",
            parse_stmt("database `c\nd`;"),
            None,
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            script.contains(
                "-- manual removal required: ns:a\\nb database c\\nd - the schema \
                 file declares it, so this is an INFO key mismatch, not an \
                 unmanaged database\n"
            ),
            "the owner note folds:\n{script}"
        );
        assert_comment_lines_carry_no_controls(&script);
    }

    /// The gate, pinned on BOTH sides: the reader-unreachable
    /// `on_table: None` field arm carries no slot anywhere — no
    /// REQUIRED block in the script (the fail-closed posture), and —
    /// now that the gate lives in `required_slot` itself — no slotted
    /// row marker and no manifest row either (lockstep by
    /// construction, not by two conditions agreeing).
    #[test]
    fn a_field_without_an_on_edge_carries_no_slot_anywhere() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let findings = vec![db_only(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "gone",
            // The arm is reader-unreachable (INFO's printer always
            // spells `ON t`); the resolved statement's shape is
            // irrelevant to the gate — a raw body keeps the fixture
            // honest without pretending the differ produced this.
            parse_stmt("raw { DEFINE FIELD x ON t; };"),
            None,
        )];
        let script = convergence_script(&findings, &sdl);
        assert!(
            !script.contains(">>> SURFORM REQUIRED:") && !script.contains(">>> SURFORM WARNING:"),
            "no slot without a nameable table:\n{script}"
        );
        let text = crate::info::report::report(&findings);
        assert!(
            !text.contains("== required data-migration slots =="),
            "no row marker, no manifest either:\n{text}"
        );
    }

    /// Tied keys break on the canonical statement text: two standalone
    /// fields sharing the unqualified
    /// key — legal SDL, CLI-reachable — tie in the (scope, family,
    /// key) sort, and so do their removals twins. The text's total
    /// order fixes their relative order (ON t before ON u), and any
    /// permutation of the same findings renders byte-identically.
    #[test]
    fn tied_keys_break_on_the_statement_text_and_permutations_stay_byte_identical() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Field,
                "x",
                parse_stmt("field x: string { on: u; };"),
            ),
            sdl_only(
                scope.clone(),
                Family::Field,
                "x",
                parse_stmt("field x: int { on: t; };"),
            ),
            db_only(
                scope.clone(),
                Family::Field,
                "x",
                parse_stmt("field x: option<string> { on: u; };"),
                Some("u"),
            ),
            db_only(
                scope,
                Family::Field,
                "x",
                parse_stmt("field x: option<int> { on: t; };"),
                Some("t"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let t_define_at = script
            .find("DEFINE FIELD OVERWRITE x ON t TYPE int")
            .expect("the t define");
        let u_define_at = script
            .find("DEFINE FIELD OVERWRITE x ON u TYPE string")
            .expect("the u define");
        assert!(
            t_define_at < u_define_at,
            "the text orders the tie:\n{script}"
        );
        let t_remove_at = script
            .find("REMOVE FIELD IF EXISTS x ON t;")
            .expect("the t remove");
        let u_remove_at = script
            .find("REMOVE FIELD IF EXISTS x ON u;")
            .expect("the u remove");
        assert!(t_remove_at < u_remove_at, "{script}");
        let mut reversed = findings.clone();
        reversed.reverse();
        assert_eq!(script, convergence_script(&reversed, &sdl));
        let mut rotated = findings;
        rotated.rotate_left(1);
        assert_eq!(script, convergence_script(&rotated, &sdl));
    }

    /// The member→host rule's raw twin: a raw
    /// define whose header carries `ON <table>` slots inside its
    /// host's emission like a typed child — after the host's
    /// `DEFINE TABLE` (the sorted set alone would run it first: Field
    /// sorts before Table, and a LIGHTWEIGHT host refuses the
    /// member — the executed pin is
    /// `a_raw_member_define_slots_inside_the_lightweight_promotion`
    /// in tests/diff.rs). The host's nested list is empty here, so
    /// the slot is the raw's whole story.
    #[test]
    fn a_raw_member_slots_inside_its_hosts_emission() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(scope.clone(), Family::Table, "t", parse_stmt("table t;")),
            sdl_only(
                scope,
                Family::Field,
                "x",
                parse_stmt("raw { DEFINE FIELD OVERWRITE x ON t TYPE int; };"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let table_at = script
            .find("DEFINE TABLE OVERWRITE t")
            .expect("the host define");
        let raw_at = script
            .find("DEFINE FIELD OVERWRITE x ON t TYPE int;")
            .expect("the raw member, verbatim");
        assert!(table_at < raw_at, "the host defines first:\n{script}");
    }

    /// The raw index member splices AFTER its host's typed fields: a
    /// fulltext index refuses a SCHEMAFULL table missing a field it
    /// names (vendored define/index.rs's `FdNotFound`), so the raw
    /// index rides behind the typed field the host's authored define
    /// nests — the unfold's splice point — never before the head
    /// (the sorted set alone would run it first: Index sorts before
    /// Table). The nested field finding is the host emission's
    /// (dropped — the member renders once).
    #[test]
    fn a_raw_index_member_splices_after_its_hosts_typed_fields() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            drift(
                scope.clone(),
                Family::Table,
                "t",
                parse_stmt("table t { schemafull; permissions: full; field a: int; };"),
                parse_stmt("table t;"),
            ),
            sdl_only(
                scope.clone(),
                Family::Field,
                "a",
                parse_stmt("field a: int { on: t; };"),
            ),
            sdl_only(
                scope,
                Family::Index,
                "ix",
                parse_stmt("raw { DEFINE INDEX OVERWRITE ix ON t FIELDS a; };"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let table_at = script
            .find("DEFINE TABLE OVERWRITE t")
            .expect("the host head");
        let field_at = script
            .find("DEFINE FIELD OVERWRITE a ON t")
            .expect("the typed member");
        let raw_at = script
            .find("DEFINE INDEX OVERWRITE ix ON t FIELDS a;")
            .expect("the raw index, verbatim");
        assert!(
            table_at < field_at && field_at < raw_at,
            "head, typed field, then the raw index:\n{script}"
        );
        assert_eq!(
            script.matches("DEFINE INDEX OVERWRITE ix").count(),
            1,
            "{script}"
        );
    }

    /// The fail-closed posture: a raw
    /// define whose header never parses to a family and key, and one
    /// whose family carries no ON grammar (a param), keep today's
    /// sort position — the raw author's placement stands, the same
    /// posture the removals tail's manual branches take. (Param and
    /// Raw sort before Table, so both raws render ahead of the table
    /// define, exactly as before the rule existed.)
    #[test]
    fn raw_defines_without_a_readable_on_keep_their_sort_position() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Param,
                "p",
                parse_stmt("raw { DEFINE PARAM OVERWRITE $p VALUE 1; };"),
            ),
            sdl_only(
                scope.clone(),
                Family::Raw,
                "",
                parse_stmt("raw { SELECT 1; };"),
            ),
            sdl_only(scope, Family::Table, "t", parse_stmt("table t;")),
        ];
        let script = convergence_script(&findings, &sdl);
        let param_at = script
            .find("DEFINE PARAM OVERWRITE $p VALUE 1;")
            .expect("the raw param, verbatim");
        let select_at = script.find("SELECT 1;").expect("the raw body, verbatim");
        let table_at = script
            .find("DEFINE TABLE OVERWRITE t")
            .expect("the table define");
        assert!(
            param_at < select_at && select_at < table_at,
            "the sort positions stand:\n{script}"
        );
    }

    // ---------------------------------------------------------------
    // The raw-edge settle pass
    // ---------------------------------------------------------------

    /// The settle pass's first trigger: a TYPED view whose source table
    /// defines as a RAW body. The partition's key order runs the view
    /// first ("v" < "zzz") and the typed FROM edge cannot resolve
    /// against a raw target (`def_ident` registers raw bodies as
    /// nothing) — the settle pass moves the view behind the raw
    /// source, whose `OVERWRITE` must run first (the engine walks the
    /// view's `source_tables()` at define and refuses `TbNotFound`
    /// otherwise; the executed pin is
    /// `a_view_defines_after_its_raw_defined_source` in tests/diff.rs).
    #[test]
    fn a_view_defines_after_its_raw_source() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Table,
                "v",
                parse_stmt("table view v { select author from zzz; };"),
            ),
            sdl_only(
                scope,
                Family::Table,
                "zzz",
                parse_stmt("raw { DEFINE TABLE OVERWRITE zzz SCHEMALESS; };"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let zzz_at = script
            .find("DEFINE TABLE OVERWRITE zzz SCHEMALESS;")
            .expect("the raw source, verbatim");
        let v_at = script
            .find("DEFINE TABLE OVERWRITE v TYPE ANY SCHEMALESS AS SELECT author FROM zzz;")
            .expect("the typed view");
        assert!(zzz_at < v_at, "the source defines first:\n{script}");
    }

    /// The second trigger — the other parity: a RAW view body
    /// whose source defines as a TYPED table. The raw body carries no
    /// registry entry, so its own FROM edge is the blind one; the
    /// settle pass reads it (`raw_dep_names`) and moves the raw view
    /// behind the typed source.
    #[test]
    fn a_raw_view_defines_after_its_typed_source() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Table,
                "v",
                parse_stmt("raw { DEFINE TABLE OVERWRITE v AS SELECT author FROM zzz; };"),
            ),
            sdl_only(scope, Family::Table, "zzz", parse_stmt("table zzz;")),
        ];
        let script = convergence_script(&findings, &sdl);
        let zzz_at = script
            .find("DEFINE TABLE OVERWRITE zzz TYPE NORMAL SCHEMALESS;")
            .expect("the typed source");
        let v_at = script
            .find("DEFINE TABLE OVERWRITE v AS SELECT author FROM zzz;")
            .expect("the raw view, verbatim");
        assert!(zzz_at < v_at, "the source defines first:\n{script}");
    }

    /// A deferred view's own dependents follow: the view chain
    /// v1 → v2 → raw zzz (v1 reads v2, v2 reads zzz). The typed edge
    /// already stands v2 before v1; deferring v2 behind zzz strands
    /// v1, and the pass's fixpoint moves v1 too — every source defines
    /// before the view that reads it.
    #[test]
    fn a_deferred_view_takes_its_dependents_with_it() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Table,
                "v1",
                parse_stmt("table view v1 { select author from v2; };"),
            ),
            sdl_only(
                scope.clone(),
                Family::Table,
                "v2",
                parse_stmt("table view v2 { select author from zzz; };"),
            ),
            sdl_only(
                scope,
                Family::Table,
                "zzz",
                parse_stmt("raw { DEFINE TABLE OVERWRITE zzz SCHEMALESS; };"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let zzz_at = script
            .find("DEFINE TABLE OVERWRITE zzz SCHEMALESS;")
            .expect("the raw source");
        let v2_at = script
            .find("DEFINE TABLE OVERWRITE v2")
            .expect("the mid view");
        let v1_at = script
            .find("DEFINE TABLE OVERWRITE v1")
            .expect("the top view");
        assert!(
            zzz_at < v2_at && v2_at < v1_at,
            "each view lands behind its source:\n{script}"
        );
    }

    /// A view whose source defines nowhere in this content keeps its
    /// position — the pass enforces only in-content dependencies
    /// (absent = the auto-create/converged postures; the source is
    /// nobody's define here, so there is nothing to move behind).
    #[test]
    fn a_view_whose_source_defines_nowhere_keeps_its_position() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Table,
                "v",
                parse_stmt("table view v { select author from zzz; };"),
            ),
            sdl_only(scope, Family::Param, "p", parse_stmt("param $p = 1;")),
        ];
        let script = convergence_script(&findings, &sdl);
        let param_at = script.find("DEFINE PARAM OVERWRITE $p").expect("the param");
        let v_at = script.find("DEFINE TABLE OVERWRITE v").expect("the view");
        assert!(param_at < v_at, "the partition order stands:\n{script}");
    }

    /// The settle pass's analyzer arm, exercised directly on a
    /// hand-built order the family sort can never produce today (the
    /// defines partition puts Analyzer before Index, so the twin is
    /// unreachable through the assembly — the arm
    /// is the defense that keeps it unreachable if the ordering ever
    /// changes): a raw fulltext index standing before its analyzer
    /// moves behind it. The index body's spelling is the vendored
    /// grammar's (`FULLTEXT ANALYZER <ident>` — parser stmt/define.rs).
    #[test]
    fn the_settle_pass_moves_a_raw_index_behind_its_analyzer() {
        let items = vec![
            parse_stmt("raw { DEFINE INDEX OVERWRITE ix ON t FIELDS a FULLTEXT ANALYZER arts; };"),
            parse_stmt("analyzer arts { tokenizers: class; };"),
        ];
        let mut items = items;
        settle_raw_body_edges(&mut items);
        let arts_at = items
            .iter()
            .position(|stmt| matches!(stmt, Stmt::Analyzer(_)))
            .expect("the analyzer");
        let raw_at = items
            .iter()
            .position(|stmt| matches!(stmt, Stmt::Raw(_)))
            .expect("the raw index");
        assert!(arts_at < raw_at, "the analyzer defines first: {items:?}");
    }

    /// The settle pass's view arm on a hand-built violated order —
    /// the mechanism the partition-order tests above pin through the
    /// whole script, isolated: a typed view standing before its raw
    /// source moves directly behind it.
    #[test]
    fn the_settle_pass_moves_a_view_behind_its_raw_source() {
        let mut items = vec![
            parse_stmt("table view v { select author from zzz; };"),
            parse_stmt("raw { DEFINE TABLE OVERWRITE zzz SCHEMALESS; };"),
        ];
        settle_raw_body_edges(&mut items);
        let zzz_at = items
            .iter()
            .position(|stmt| matches!(stmt, Stmt::Raw(_)))
            .expect("the raw source");
        let v_at = items
            .iter()
            .position(|stmt| matches!(stmt, Stmt::Table(_)))
            .expect("the view");
        assert!(zzz_at < v_at, "the source defines first: {items:?}");
    }

    // ---------------------------------------------------------------
    // The settle pass's value-read edges
    // ---------------------------------------------------------------

    /// Trigger A: a TYPED param whose VALUE reads a
    /// param that defines as a RAW body. The partition's key order
    /// runs `$a` first ("a" < "b") and the typed VALUE edge cannot
    /// resolve against a raw target (`def_ident` registers raw bodies
    /// as nothing) — the settle pass moves `$a` behind `$b`, whose
    /// VALUE must be defined when `$a`'s evaluates (the engine rolls
    /// the ONE transaction back otherwise; the executed pin is
    /// `a_param_defines_after_its_raw_defined_dependency` in
    /// tests/diff.rs).
    #[test]
    fn a_typed_param_defines_after_its_raw_defined_dependency() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Param,
                "a",
                parse_stmt("param $a = $b + 1;"),
            ),
            sdl_only(
                scope,
                Family::Param,
                "b",
                parse_stmt("raw { DEFINE PARAM OVERWRITE $b VALUE 2; };"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let b_at = script
            .find("DEFINE PARAM OVERWRITE $b VALUE 2;")
            .expect("the raw param, verbatim");
        let a_at = script
            .find("DEFINE PARAM OVERWRITE $a VALUE $b + 1;")
            .expect("the typed param");
        assert!(b_at < a_at, "the dependency defines first:\n{script}");
    }

    /// The reverse parity (trigger B's twin): a RAW
    /// access whose jwt `url:` reads a param that defines TYPED. The
    /// family partition runs the access first (Access < Param), and
    /// the raw body's own slots register nowhere in `emission_order`
    /// — the settle pass reads them (`raw_value_reads`) and moves the
    /// access behind the param. Pre-fix this landed silently wrong:
    /// the URL computed over a missing param stores the string
    /// `'NONE'` (the executed pin is
    /// `an_access_url_resolves_its_param_in_the_catalog` in
    /// tests/diff.rs).
    #[test]
    fn a_raw_access_defines_after_its_typed_defined_param() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Access,
                "ju",
                parse_stmt("access ju { on: db; type: jwt; url: $p; };"),
            ),
            sdl_only(
                scope,
                Family::Param,
                "p",
                parse_stmt("param $p = 'https://x/jwks';"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let p_at = script
            .find("DEFINE PARAM OVERWRITE $p")
            .expect("the typed param");
        let ju_at = script
            .find("DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $p")
            .expect("the raw access, verbatim");
        assert!(p_at < ju_at, "the param defines first:\n{script}");
    }

    /// The value-read cycle (the settle pass's new mechanism constraint):
    /// two raw params reading each other — a class E0015's checker
    /// cannot see through (the reads live inside the raw bodies) and
    /// no ordering satisfies. The pass prunes the cycle's edges and
    /// keeps the sorted positions: the generator terminates, the
    /// script emits, and the migration fails visibly at the engine
    /// (the rollback) — never a hang.
    #[test]
    fn a_value_reader_cycle_keeps_the_sorted_positions() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Param,
                "a",
                parse_stmt("raw { DEFINE PARAM OVERWRITE $a VALUE $b + 1; };"),
            ),
            sdl_only(
                scope,
                Family::Param,
                "b",
                parse_stmt("raw { DEFINE PARAM OVERWRITE $b VALUE $a + 1; };"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let a_at = script
            .find("DEFINE PARAM OVERWRITE $a VALUE $b + 1;")
            .expect("the raw param a");
        let b_at = script
            .find("DEFINE PARAM OVERWRITE $b VALUE $a + 1;")
            .expect("the raw param b");
        assert!(
            a_at < b_at,
            "the pruned cycle leaves the sorted order standing:\n{script}"
        );
    }

    /// The cycle prune on a hand-built cyclic order — the mechanism
    /// isolated: mutual value readers pass through unchanged (and the
    /// pass returns — termination is the point).
    #[test]
    fn the_settle_pass_prunes_a_value_reader_cycle() {
        let mut items = vec![
            parse_stmt("raw { DEFINE PARAM OVERWRITE $a VALUE $b + 1; };"),
            parse_stmt("raw { DEFINE PARAM OVERWRITE $b VALUE $a + 1; };"),
        ];
        settle_raw_body_edges(&mut items);
        assert_eq!(items.len(), 2, "the pass is total on a cyclic graph");
        let a_at = items
            .iter()
            .position(|stmt| matches!(stmt, Stmt::Raw(raw) if raw.body.contains("$a VALUE")))
            .expect("raw a");
        let b_at = items
            .iter()
            .position(|stmt| matches!(stmt, Stmt::Raw(raw) if raw.body.contains("$b VALUE")))
            .expect("raw b");
        assert!(a_at < b_at, "the sorted order stands: {items:?}");
    }

    /// A MULTI-statement raw body's edges union across its
    /// statements: the access's jwt url (statement 1) and the view's
    /// source (statement 2) BOTH settle the block behind its targets,
    /// wherever each edge-carrying statement stands — first-header
    /// classification (Access, which sorts before Param and Table)
    /// gates nothing. Pre-fix the block rendered first and the url
    /// silently stored 'NONE' (the executed pin is
    /// `a_multistatement_raw_body_resolves_its_url_and_dep_edges` in
    /// tests/diff.rs).
    #[test]
    fn a_multistatement_raw_body_unions_its_edges_across_statements() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Param,
                "p",
                parse_stmt("param $p = 'https://x/jwks';"),
            ),
            sdl_only(
                scope.clone(),
                Family::Table,
                "zzz",
                parse_stmt("table zzz;"),
            ),
            sdl_only(
                scope,
                Family::Access,
                "ju",
                parse_stmt(
                    "raw { DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $p; \
                     DEFINE TABLE OVERWRITE v AS SELECT author FROM zzz; };",
                ),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let p_at = script
            .find("DEFINE PARAM OVERWRITE $p")
            .expect("the typed param");
        let zzz_at = script
            .find("DEFINE TABLE OVERWRITE zzz TYPE NORMAL SCHEMALESS;")
            .expect("the typed source");
        let raw_at = script
            .find("DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $p")
            .expect("the raw body, verbatim");
        assert!(
            p_at < raw_at && zzz_at < raw_at,
            "both edges settle the block behind its targets:\n{script}"
        );
    }

    /// A typed reader whose target defines typed keeps the
    /// `emission_order` answer (the settle pass's edge is satisfied —
    /// no move): an access over a TYPED param renders the param first
    /// through the resolved graph, exactly as before the settle arm
    /// existed.
    #[test]
    fn a_typed_access_over_a_typed_param_stays_emission_ordered() {
        let sdl = parse_file_sdl("namespace app; database main;");
        let scope = ScopeKey::Database("app".into(), "main".into());
        let findings = vec![
            sdl_only(
                scope.clone(),
                Family::Access,
                "ju",
                parse_stmt("access ju { on: db; type: jwt; url: $p; };"),
            ),
            sdl_only(
                scope,
                Family::Param,
                "p",
                parse_stmt("param $p = 'https://x/jwks';"),
            ),
        ];
        let script = convergence_script(&findings, &sdl);
        let p_at = script
            .find("DEFINE PARAM OVERWRITE $p")
            .expect("the typed param");
        let ju_at = script
            .find("DEFINE ACCESS OVERWRITE ju")
            .expect("the access");
        assert!(p_at < ju_at, "emission_order's answer stands:\n{script}");
    }

    // ---------------------------------------------------------------
    // The comments' voice and ASCII pins:
    // every comment byte the generator emits is printable ASCII, and
    // the prose never addresses the reader, never sells, and never
    // shouts. The fixture drives every prose path at once (both slot
    // carriers, every anchor class, the collapse arms, the fail-closed
    // corners, the staged prelude, the raw stanza); the fixed
    // constants ride beside it. (The scan itself lives in one
    // `cfg(test)` home above — the migration-writer tests share it.)
    // ---------------------------------------------------------------

    use super::voice::assert_voice_clean;

    /// The dialect scan over one rendered script: printable-ASCII-only
    /// COMMENT bytes (the dialect's surface; a comment-scoped
    /// non-ASCII byte subsumes the terminator check — NEL/LS/PS are
    /// non-ASCII), and the voice rule on every comment line.
    /// Statement lines are exempt byte-wise: a hostile NAME rides
    /// them as DATA verbatim (backtick idents take any bytes — the
    /// statement contract), which the fixture's hostile-named index
    /// exercises deliberately.
    fn assert_dialect_clean(script: &str) {
        for line in script.lines().filter(|line| line.starts_with("--")) {
            assert!(
                line.bytes().all(|byte| (0x20..=0x7e).contains(&byte)),
                "a non-ASCII byte reached a comment line: {line}"
            );
            assert_voice_clean(line);
        }
    }

    /// The voice and ASCII pins: the fixed constants, then a fixture
    /// whose findings drive EVERY prose path the generator owns —
    /// the raw stanza, the scope and region headers, the staged
    /// prelude and its slot, the index-rebuild note, the unique-index
    /// hazard note, the table and field slots, the collapse arms
    /// (retired scopes, includes lines, the pseudo-namespace frame),
    /// the owner-manual and manual-removal notes, the summary's
    /// conditional unmanaged clause, and the index-note pair's
    /// spellability pointer — through BOTH slot carriers.
    /// (The group writer's own prose line rides the same scan in the
    /// migration-writer tests; the two suites share one voice home.)
    #[test]
    fn the_generator_prose_stays_ascii_and_voiced() {
        for line in HEADER.lines().chain(RULE.lines()) {
            assert!(
                line.bytes().all(|byte| (0x20..=0x7e).contains(&byte)),
                "a non-ASCII byte in the fixed header: {line}"
            );
            assert_voice_clean(line);
        }
        let sdl = parse_file_sdl(
            "namespace app; database main; table user;\n\
             table member_of {\n\
             index unique uq { fields: x, out; };\n\
             index unique uq2 { fields: out; };\n\
             index unique `a\u{85}REMOVE TABLE victim;--` { fields: out; };\n\
             field x: string;\n\
             field out: int;\n\
             };\n",
        );
        // uq2 gives the unique-index hazard note its EMPTY-covered arm
        // (its fields cover no re-keyed field) beside uq's covered one;
        // the hostile-named index drives the note pair's spellability
        // drop and its pointer prose.
        let scope = ScopeKey::Database("app".into(), "main".into());
        let mut rekey = drift(
            scope.clone(),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: member_of; };"),
            parse_stmt("field x: int { on: member_of; };"),
        );
        rekey.on_table_exists = true;
        let findings = vec![
            // The raw stanza rides (a raw body defines).
            sdl_only(
                scope.clone(),
                Family::Field,
                "rawf",
                parse_stmt("raw { DEFINE FIELD OVERWRITE rawf ON user TYPE int; };"),
            ),
            // Defines region + the unique-index hazard note.
            sdl_only(
                scope.clone(),
                Family::Param,
                "fresh",
                parse_stmt("param $fresh = 1;"),
            ),
            rekey,
            // Field slot + table WARNING + table slot.
            db_only(
                scope.clone(),
                Family::Field,
                "gone",
                parse_stmt("field gone: int { on: user; };"),
                Some("user"),
            ),
            // The field slot's non-spellable arm: the `e.g.` lines
            // drop, the prose-only cleanup bullet rides.
            db_only(
                scope.clone(),
                Family::Field,
                "a.b",
                parse_stmt("field `a.b`: int { on: user; };"),
                Some("user"),
            ),
            db_only(
                scope.clone(),
                Family::Table,
                "user",
                parse_stmt("table user;"),
                None,
            ),
            // The fail-closed manual-removal note (empty raw key).
            db_only(
                scope.clone(),
                Family::Raw,
                "",
                parse_stmt("param $x = 1;"),
                None,
            ),
            // The collapse arm: a retired database (with a consumed
            // field, for an includes line) and the pseudo-namespace
            // frame (an empty namespace spelling).
            db_only(
                ScopeKey::Database("app".into(), "stale".into()),
                Family::Table,
                "t",
                parse_stmt("table t;"),
                None,
            ),
            db_only(
                ScopeKey::Database("app".into(), "stale".into()),
                Family::Field,
                "f",
                parse_stmt("field f: int { on: t; };"),
                Some("t"),
            ),
            db_only(
                ScopeKey::Database("".into(), "shop".into()),
                Family::Table,
                "t2",
                parse_stmt("table t2;"),
                None,
            ),
            // The owner-manual corner (an INFO key mismatch) and a
            // retired namespace.
            db_only(
                ScopeKey::Namespace("app".into()),
                Family::Database,
                "main",
                parse_stmt("database main;"),
                None,
            ),
            db_only(
                ScopeKey::Root,
                Family::Namespace,
                "junk",
                parse_stmt("namespace junk;"),
                None,
            ),
            // The summary's conditional informational tail (an
            // expected-unmanaged finding carries no work; the clause
            // rides the summary line, voice-scanned with the rest).
            DriftFinding {
                on_table_exists: false,
                scope: ScopeKey::Root,
                family: Family::Namespace,
                key: "default".to_string(),
                class: DriftClass::ExpectedUnmanaged {
                    database: Box::new(parse_stmt("namespace default;")),
                },
            },
        ];
        let script = convergence_script(&findings, &sdl);
        assert_dialect_clean(&script);
        // The unmanaged clause really rendered (the scan above is
        // meaningful only over the bytes it covers).
        assert!(
            script.contains("1 expected-unmanaged (informational)"),
            "the fixture must drive the summary's informational tail:\n{script}"
        );
        assert_dialect_clean(&convergence_script_styled(
            &findings,
            &sdl,
            SlotStyle::ResolutionMarkers,
        ));
        // The staged prelude's SHELL-ONLY intro (a permissions drift
        // re-keys nothing — the other header's prose, which the
        // re-keyed fixture above never renders).
        let shell_sdl = parse_file_sdl("namespace app; database main; table t;");
        let shell_findings = vec![drift(
            scope.clone(),
            Family::Table,
            "t",
            parse_stmt("table t { permissions: full; };"),
            parse_stmt("table t { permissions: none; };"),
        )];
        assert_dialect_clean(&convergence_script(&shell_findings, &shell_sdl));
    }

    /// The header const is the wrap rule's own output: the three
    /// logical explanation lines through [`wrap_prose`] at
    /// [`HEADER_WIDTH`] spell [`HEADER`] byte-for-byte (the module
    /// doc's claim, enforced — a header or width edit that breaks the
    /// rule's shape cannot land silently).
    #[test]
    fn the_header_const_is_the_wrap_rules_output() {
        let header = format!(
            "{}{}{}",
            wrap_prose(
                "generated by `surform migrate generate` - the \
                 convergence script.",
                "-- ",
                "--    ",
                HEADER_WIDTH,
            ),
            wrap_prose(
                "Re-runs are safe: OVERWRITE redefines; IF EXISTS \
                 no-ops on absence.",
                "-- ",
                "--    ",
                HEADER_WIDTH,
            ),
            wrap_prose(
                "Lines marked SURFORM WARNING destroy data; SURFORM \
                 REQUIRED marks fill-in data-migration slots; SURFORM \
                 NOTE lines are advice.",
                "-- ",
                "--    ",
                HEADER_WIDTH,
            ),
        );
        assert_eq!(header, HEADER, "the header drifted from the wrap rule");
    }
}
