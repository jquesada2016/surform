//! The drift report: the differ's findings as plain,
//! deterministic text — the `surform migrate generate` report leg,
//! like DDL, not a diagnostic (`--json` keeps rendering only the
//! input diagnostics). One line per finding under a class sigil,
//! drift findings carrying their CLAUSE-LEVEL deltas underneath.
//! The data-migration obligations: a finding whose
//! removal orphans data marks its row and lands in the report's
//! slot manifest (one line per REQUIRED slot — scope, family, key,
//! the stakes), so CI and humans see the unfilled obligations
//! without reading the whole script.
//!
//! Delta naming segments the two resolved statements' canonical SDL
//! ([`statement_text`]) at clause boundaries. The writer's shape is
//! the contract that makes this a line walk rather than a scanner
//! pass: every clause is exactly one line (values never wrap), block
//! bodies indent by two, and nested `field`/`index`/`event` children
//! are whole blocks — so a clause boundary IS a line boundary. The
//! one multi-line exception, a `raw` body, is verbatim by definition
//! and renders as a whole-body delta instead.
//!
//! Name handling (the script side's twin contract — the complete
//! rule): EVERY prose-line-embedded interpolation folds through the
//! script's [`comment_text`] sanitizer — the rows' and manifests'
//! names (scope labels, finding keys, table names, re-keyed field
//! keys), the delta texts at the print boundary ([`delta_text`]),
//! and the kind note's kind spellings. A backtick ident or a quoted
//! kind name may legally carry a raw line terminator (the vendored
//! lexer accepts any bytes), and the report is a line walk: the
//! writer's escape rule folds the C0 terminators but leaves NEL and
//! the Zl/Zp separators raw — three of the five line-comment
//! terminators [`comment_text`] folds; the SDL side rides source
//! spellings verbatim exactly for those three (a source raw LF
//! arrives in the kind's text already writer-spelled, `escape_with`
//! having folded it), so the folded escape spelling keeps every
//! report line ONE line, the tail never at column 0, on any
//! display that breaks on the separators. Sane spellings ride the
//! fold byte-identical. The delta fold is print-only (segmentation
//! and comparison stay on the raw texts, so a raw byte and its
//! folded spelling still differ). The prose itself rides the
//! 2026-10-16 readability dialect: plain hyphens, `->` for the
//! re-keyed-fields arrow, verb-led declaratives, imperatives only
//! for reader actions.

use std::collections::BTreeMap;

use crate::ast::{IndexKind, Stmt};
use crate::info::diff::{DriftClass, DriftFinding};
use crate::info::normalize::{Family, ScopeKey};
use crate::info::staging::{StagedTable, kind_changed, staged_tables};
use crate::info::write::statement_text;

use super::script::{comment_text, empty_key_hint};

/// The drift report over the comparator's findings. Findings sort by
/// (scope, family, key) — `ScopeKey`'s declaration order (root,
/// namespaces, databases) is the tag-natural one — so the text is a
/// pure function of the findings. A summary line leads (issue #79's
/// report half): the counts the rows and manifests below carry —
/// findings, defines, removals, data-migration slots, staged tables
/// (the manifest lists the re-keyed ones; a shell-only staged table
/// rides the count, not a row; the frame
/// count is the script header's alone: the report sees no SDL). The
/// slot manifest rides the same
/// sort: every REQUIRED data-migration slot the findings
/// carry, listed after the finding rows, so `--check`'s report-only
/// output shows the obligations too — and the staged-migration
/// manifest (issue #65) lists the tables the script re-keys through
/// the relaxed-shell sequence.
pub fn report(findings: &[DriftFinding]) -> String {
    let mut sorted: Vec<&DriftFinding> = findings.iter().collect();
    sorted.sort_by(|a, b| {
        (&a.scope, a.family, a.key.as_str()).cmp(&(&b.scope, b.family, b.key.as_str()))
    });
    let staged = staged_tables(findings);
    let mut out = String::new();
    if !sorted.is_empty() {
        // The summary line (issue #79's report half): the whole
        // drift at a glance, under the CLI's own `== drift ==`
        // heading — the counts the rows below carry. The frame
        // count is the script header's alone: the report sees no
        // SDL, so it cannot know how the script frames the work.
        let defines = sorted
            .iter()
            .filter(|f| {
                matches!(
                    f.class,
                    DriftClass::SdlOnly { .. } | DriftClass::Drift { .. }
                )
            })
            .count();
        let removals = sorted
            .iter()
            .filter(|f| matches!(f.class, DriftClass::DatabaseOnly { .. }))
            .count();
        let unmanaged = sorted
            .iter()
            .filter(|f| matches!(f.class, DriftClass::ExpectedUnmanaged { .. }))
            .count();
        let slots = sorted.iter().filter(|f| required_slot(f).is_some()).count();
        out.push_str(&format!(
            "{} - {}, {}, {}",
            count("finding", sorted.len()),
            count("define", defines),
            count("removal", removals),
            count("data-migration slot", slots),
        ));
        // The informational class closes the arithmetic (findings ≠
        // defines + removals when it is present); staged tables ride
        // the shared taxonomy (`staged_tables`), the script's own
        // summary spelling its post-SDL-filter number.
        if unmanaged > 0 {
            out.push_str(&format!(
                ", {} (informational)",
                count("expected-unmanaged", unmanaged)
            ));
        }
        out.push_str(&format!(", {}\n", count("staged table", staged.len())));
    }
    for finding in &sorted {
        render(&mut out, finding, &staged);
    }
    let manifest = slot_manifest(&sorted);
    if !manifest.is_empty() {
        out.push_str("\n== required data-migration slots ==\n");
        out.push_str(&manifest);
    }
    let staged_manifest = staged_manifest(&staged);
    if !staged_manifest.is_empty() {
        out.push_str("\n== staged migrations ==\n");
        out.push_str(&staged_manifest);
    }
    out
}

/// The REQUIRED data-migration slot class a finding carries
/// (source-verified engine facts): a database-only
/// TABLE orphans its documents (the REMOVE deletes the table's key
/// range) and a database-only FIELD orphans its values (`REMOVE
/// FIELD` is catalog-only — the values remain on the rows).
/// Every other family is catalog/rebuild machinery — no slot; and a
/// Drift finding retypes or rewrites without destroying any row's
/// values (the new kind governs future writes), so it never slots —
/// a kind change is the STAGED-migration taxonomy's business instead
/// (below: relaxed shell → REQUIRED data section → final definitions),
/// and the report notes it. A FIELD finding with no `on_table` edge
/// carries no slot EITHER
/// (a field slot names its host table — the prose and
/// the `e.g.` line both spell `ON <table>` — so without the edge
/// there is no honest slot, and gating HERE (the one home) keeps the
/// script's fail-closed posture, the slotted row marker, and this
/// manifest in lockstep by construction. The predicate is the ONE
/// home for the taxonomy: the script's slot emission and this
/// manifest cannot disagree — except the collapsed-scope corner
/// (ledgered, #56): a finding in a scope the SDL never declares
/// drops out of the script's collapse emission while this report —
/// which sees no SDL — still marks its row. The reach is the version
/// pair's (a scope the TO file drops; the `--info` mode's SDL-E3005
/// tag gate refuses it), so the manifest's stakes sentence hedges
/// accordingly — slot, or the retired scope's WARNING-anchored owner
/// REMOVE — and the row marker remains the ledgered gap.
pub(crate) enum SlotKind {
    Table,
    Field,
}

pub(crate) fn required_slot(finding: &DriftFinding) -> Option<SlotKind> {
    let DriftClass::DatabaseOnly { on_table, .. } = &finding.class else {
        return None;
    };
    match finding.family {
        Family::Table => Some(SlotKind::Table),
        Family::Field if on_table.is_some() => Some(SlotKind::Field),
        _ => None,
    }
}

/// The manifest: one line per REQUIRED slot — scope, family, key,
/// the ON edge, the stakes — under the findings' own (already
/// sorted) order. Empty when nothing orphans data. Every name on the
/// line rides the fold ([`row_head`], [`on_edge`]).
fn slot_manifest(sorted: &[&DriftFinding]) -> String {
    let mut out = String::new();
    for finding in sorted {
        let Some(slot) = required_slot(finding) else {
            continue;
        };
        let edge = match &finding.class {
            DriftClass::DatabaseOnly { on_table, .. } => on_edge(on_table.as_deref()),
            _ => String::new(),
        };
        // The stakes sentence names BOTH carriers honestly (the #56
        // corner): the report sees no SDL, so it cannot
        // know which arm the script took — where the scope survives,
        // the slot above the removal is the place to act; where the
        // script removes the whole namespace or database, the WARNING
        // above that REMOVE names the finding among its `-- includes`
        // lines (issue #85). Saying only the slot lied on the retired
        // arm.
        let stakes = match slot {
            SlotKind::Table => {
                "the removal deletes the table and every document in it; the \
                 script carries the fill-in slot above the removal; when the \
                 script removes a whole namespace or database instead, the \
                 WARNING above that REMOVE names this finding"
            }
            SlotKind::Field => {
                "the removal deletes the field definition only; the values \
                 remain on the rows; the script carries the fill-in slot above \
                 the removal; when the script removes a whole namespace or \
                 database instead, the WARNING above that REMOVE names this \
                 finding"
            }
        };
        out.push_str(&format!(
            "  {}{edge} - {stakes}\n",
            row_head(&finding.scope, finding.family, &finding.key)
        ));
    }
    out
}

/// The staged-migration manifest: one line per staged table — scope,
/// table, the re-keyed fields the data section migrates. Empty when
/// nothing stages. The fields' keys ride the finding order (the
/// differ's), deduplicated.
fn staged_manifest(staged: &[StagedTable<'_>]) -> String {
    let mut out = String::new();
    for entry in staged {
        if entry.rekeys.is_empty() {
            // A table-level-drift staging: the shell relaxes
            // permissions/schema mode, but nothing re-keys — no data
            // section, no manifest row.
            continue;
        }
        let fields: BTreeMap<&str, ()> = entry
            .rekeys
            .iter()
            .map(|finding| finding.key.as_str())
            .map(|key| (key, ()))
            .collect();
        let fields = fields
            .keys()
            .map(|key| format!("`{}`", comment_text(key)))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "  {} - the data section re-keys these fields: {fields}; \
             fill the section (or delete it) before commit\n",
            // The staged head is the row head's spelling with the
            // table family — one fold home for every name triple.
            row_head(entry.scope, Family::Table, entry.table)
        ));
    }
    out
}

/// A scope as the CLI's `--info` tags spell it — the report's and the
/// REMOVE script's shared scope vocabulary.
pub(crate) fn scope_label(scope: &ScopeKey) -> String {
    match scope {
        ScopeKey::Root => "root".to_string(),
        ScopeKey::Namespace(ns) => format!("ns:{ns}"),
        ScopeKey::Database(ns, db) => format!("db:{ns}/{db}"),
    }
}

/// A finding's row head — scope, family, key — the report rows', the
/// slot manifest's, and the staged manifest's shared spelling (a
/// staged table rides it with the table family). Every interpolated
/// name folds through [`comment_text`] (the module doc's contract):
/// a database-side name carrying a raw line terminator renders its
/// escape spelling on the ONE line, the tail never at column 0. Sane
/// names ride the fold byte-identical.
fn row_head(scope: &ScopeKey, family: Family, key: &str) -> String {
    format!(
        "{} {} {}",
        comment_text(&scope_label(scope)),
        family_word(family),
        comment_text(key)
    )
}

/// The database-only rows' ` ON <table>` tail — the report row's and
/// the slot manifest line's shared edge, the table name folded like
/// every interpolated name.
fn on_edge(on_table: Option<&str>) -> String {
    on_table
        .map(|table| format!(" ON {}", comment_text(table)))
        .unwrap_or_default()
}

/// A count with its noun, singular at one — the report summary's
/// and the script's section labels' shared count vocabulary (one
/// home, so the artifacts read alike). `count("removal", 0)` is the
/// honest zero: `0 removals`.
pub(crate) fn count(noun: &str, n: usize) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// A family's lowercase word — the report's and the REMOVE script's
/// shared family vocabulary.
pub(crate) fn family_word(family: Family) -> &'static str {
    match family {
        Family::Access => "access",
        Family::Analyzer => "analyzer",
        Family::Api => "api",
        Family::Bucket => "bucket",
        // Linter-only (allow comments) — never a drift family; the
        // word exists so the vocabulary stays total.
        Family::Comment => "comment",
        Family::Config => "config",
        Family::Database => "database",
        Family::Event => "event",
        Family::Field => "field",
        Family::Function => "function",
        Family::Index => "index",
        // Linter-only (the diamond advisory) — never a drift family;
        // the word exists so the vocabulary stays total.
        Family::Include => "include",
        Family::Mixin => "mixin",
        Family::Module => "module",
        Family::Namespace => "namespace",
        Family::Param => "param",
        Family::Raw => "raw",
        Family::Sequence => "sequence",
        Family::Table => "table",
        Family::User => "user",
    }
}

fn render(out: &mut String, finding: &DriftFinding, staged: &[StagedTable<'_>]) {
    let head = row_head(&finding.scope, finding.family, &finding.key);
    match &finding.class {
        DriftClass::Drift {
            sdl,
            database,
            markers,
        } => {
            out.push_str(&format!("~ {head}\n"));
            if let Some(route) = &markers.raw_routed {
                out.push_str(&format!("  raw route: {route:?}\n"));
            }
            if matches!(sdl.as_ref(), Stmt::Raw(_)) || matches!(database.as_ref(), Stmt::Raw(_)) {
                // A raw body is verbatim by definition: the whole body
                // is the delta, never clause segments.
                body_delta(out, sdl, database);
            } else {
                clause_delta(out, sdl, database);
            }
            unique_index_note(out, finding, sdl);
            if markers.redacted {
                out.push_str(
                    "  note: re-applying the database's form defines the literal \
                     '[REDACTED]' key\n",
                );
            }
            // The diverged-field kind note: a kind change
            // orphans no data — the engine applies no row validation
            // at define time, so the rows keep their values — but
            // "no data migration is required" is
            // false in the direction that bites: a data step that
            // REWRITES the values fails while the old kind is in
            // force (the error names the existing row, which reads
            // like a define-time rejection), so the workable order
            // is define-then-migrate and the script stages the
            // table. The staged claim rides the shared taxonomy —
            // the same `staged_tables` the script's emission walks.
            if kind_changed(sdl, database) {
                // The kind spelling folds through [`comment_text`] —
                // the note is ONE prose line and the kind is
                // prose-line-embedded: `write_kind`'s escape set
                // leaves NEL and the Zl/Zp separators raw, and the
                // SDL side rides the source spelling verbatim (the
                // same three-spelling
                // hole the delta fold closed, one surface over). A
                // hostile kind renders the folded escape spelling
                // like every interpolation.
                let spelling = |kind: &Option<crate::ast::Kind>| match kind {
                    Some(kind) => comment_text(&kind.text),
                    None => "(absent)".to_string(),
                };
                let (Stmt::Field(sdl_field), Stmt::Field(db_field)) =
                    (sdl.as_ref(), database.as_ref())
                else {
                    unreachable!("kind_changed only fires on a Field pair");
                };
                let staged_here = staged
                    .iter()
                    .any(|entry| entry.scope == &finding.scope && entry.table == sdl_field.on.raw);
                out.push_str(&format!(
                    "  note: the kind changes (db `{}` -> sdl `{}`); existing rows keep \
                     their values (the engine validates no rows at define time), but a \
                     data step that rewrites them fails while the old kind is in \
                     force; run the data step after the definitions{}\n",
                    spelling(&db_field.kind),
                    spelling(&sdl_field.kind),
                    if staged_here {
                        " - the script stages this table (relaxed shell -> REQUIRED \
                         data section -> final definitions)"
                    } else {
                        ""
                    },
                ));
            }
        }
        DriftClass::DatabaseOnly {
            on_table, database, ..
        } => {
            let edge = on_edge(on_table.as_deref());
            // A finding whose removal orphans data marks its row —
            // the script carries its REQUIRED slot (the manifest
            // below lists them all; the retired-scope arm is the
            // #56 corner — required_slot's doc has it).
            let slot = match required_slot(finding) {
                Some(_) => " (required data-migration slot)",
                None => "",
            };
            // An empty match key's producer — a header-less raw body
            // (its first line) or an anonymous-silo module (its
            // executable's spelling) — stands in (comment-safe; the
            // script's manual-removal note quotes the same hint), so
            // the row still names WHAT to remove. The head trims
            // first: the empty
            // key leaves a trailing space.
            let hint = empty_key_hint(finding.family, &finding.key, database)
                .map(|hint| format!(" - {hint}"))
                .unwrap_or_default();
            out.push_str(&format!("\u{2212} {}{edge}{slot}{hint}\n", head.trim_end()));
        }
        DriftClass::SdlOnly { sdl } => {
            out.push_str(&format!("+ {head} (apply-pending)\n"));
            unique_index_note(out, finding, sdl);
        }
        DriftClass::ExpectedUnmanaged { .. } => {
            out.push_str(&format!(
                "\u{b7} {head} (expected-unmanaged: auto-seeded by SurrealDB \
                 - declare it in SDL to manage)\n"
            ));
        }
    }
}

/// The unique-index note (issue #65's second probe-pinned class): a
/// UNIQUE index define over a table that exists database-side BUILDS
/// over the existing rows, and the define fails when any two rows
/// collide (`Database index … already contains …, with record …`) —
/// the one row-validating define the probe found. The SDL side
/// carries the (possibly new) kind; the finding's
/// `on_table_exists` gates it (an index whose table is itself
/// apply-pending runs over an empty key range). Unique-carrying
/// indexes only — a plain index builds unconstrained.
fn unique_index_note(out: &mut String, finding: &DriftFinding, sdl: &Stmt) {
    let Stmt::Index(index) = sdl else {
        return;
    };
    if index.kind != IndexKind::Unique || !finding.on_table_exists {
        return;
    }
    out.push_str(
        "  note: the unique index builds over existing rows - the define fails if \
         any two rows collide; clean or dedupe the data first (a data step before \
         this define, or a staged migration)\n",
    );
}

/// The whole-body delta: both canonical texts printed under their
/// side's label, continuation lines indented to the label. Every
/// printed line folds through [`comment_text`] ([`delta_text`]'s
/// contract — raw bodies ride the database side verbatim).
fn body_delta(out: &mut String, sdl: &Stmt, database: &Stmt) {
    out.push_str("  body:\n");
    for (label, stmt) in [("sdl", sdl), ("db", database)] {
        let text = statement_text(stmt);
        let mut lines = text.lines();
        match lines.next() {
            None => out.push_str(&format!("    {label}:\n")),
            Some(first) => {
                out.push_str(&format!("    {label}: {}\n", comment_text(first)));
                for line in lines {
                    out.push_str(&format!("        {}\n", comment_text(line)));
                }
            }
        }
    }
}

/// A delta text's printable form, folded line by line through
/// [`comment_text`] — the consumption boundary's fold. The writer's
/// escape rule folds the C0 terminators inside backtick idents and
/// strings, but NEL and the Zl/Zp separators ride through it raw —
/// three of the five line-comment terminators [`comment_text`]
/// folds — so a hostile name carrying one would otherwise render the
/// raw byte into the delta: any display that breaks on Zl/Zp puts an
/// attacker-chosen tail at column 0, reading as a finding of its
/// own. Real line breaks (a folded block's shape) stay real; only
/// the hostile terminators fold. Segmentation and comparison run on
/// the raw texts — folding is print-only, so a raw byte and its
/// folded spelling still differ.
fn delta_text(text: &str) -> String {
    text.lines()
        .map(comment_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The clause-level delta: both resolved texts segment at clause
/// boundaries, and the segment maps compare by key — a key held by
/// one side only names its side, a key held by both quotes both
/// texts (equal segments emit nothing: the delta names the
/// DIFFERENCE).
///
/// Three render rules keep the delta readable (issue #83):
///
/// - a nested `field`/`index`/`event` child segment is SKIPPED —
///   the differ compares every table's nested children individually
///   (`compare_children`), so each differing child already carries
///   its own finding row and the table's clause delta would
///   double-list it;
/// - a block clause the block path holds on ONE side only folds its
///   whole subtree into one delta (opener, body clauses, closer) —
///   the per-clause keys would otherwise split it into fragments
///   (an opener fragment here, a body-clause line there);
/// - a braced statement header renders trimmed of its trailing ` {`
///   (the kind delta reads `field x: string` vs `field x: int`, one
///   line — the statement closer is bookkeeping and never segments).
///
/// The doc-comment segment keys as `comment` (issue #86's vocabulary
/// half — the raw `///` spelling is lexer plumbing, not a delta
/// name), and when the comment is the ONLY delta the line says so.
fn clause_delta(out: &mut String, sdl: &Stmt, database: &Stmt) {
    let sdl_segments = segments(&statement_text(sdl));
    let db_segments = segments(&statement_text(database));
    let mut keys: Vec<String> = sdl_segments.keys().cloned().collect();
    for key in db_segments.keys() {
        if !sdl_segments.contains_key(key) {
            keys.push(key.clone());
        }
    }
    keys.sort();
    // The one-sided block roots: keys present on one side only whose
    // block path also holds body-clause keys — those fold (the whole
    // block is one delta), and their children never render alone.
    let folded: Vec<&String> = keys
        .iter()
        .filter(|key| {
            let one_sided = sdl_segments.contains_key(*key) != db_segments.contains_key(*key);
            one_sided
                && keys.iter().any(|other| {
                    other.len() > key.len() + 1 && other.starts_with(&format!("{key}."))
                })
        })
        .collect();
    // The comment-only delta flag: a comment segment exists and
    // every other key's segments are equal (they emit nothing — the
    // visible delta is the comment alone).
    let comment_key = "///";
    let comment_only = keys.iter().any(|key| key == comment_key)
        && keys
            .iter()
            .filter(|key| **key != comment_key)
            .all(|key| sdl_segments.get(key) == db_segments.get(key));
    for key in &keys {
        if is_child_key(key) {
            // The differ emitted this child as its own finding row.
            continue;
        }
        if folded
            .iter()
            .any(|root| key.as_str() != root.as_str() && key.starts_with(&format!("{root}.")))
        {
            continue;
        }
        // The header segment (key "") labels by role, not by an
        // empty word; the doc segment keys by its word.
        let (label, note): (String, &str) = if key.is_empty() {
            ("header".to_string(), "")
        } else if key == comment_key {
            (
                "comment".to_string(),
                if comment_only {
                    " (only the comment changed)"
                } else {
                    ""
                },
            )
        } else {
            (key.clone(), "")
        };
        let fold_side: Option<(&BTreeMap<String, String>, &str)> = if folded.contains(&key) {
            if sdl_segments.contains_key(key) {
                Some((&sdl_segments, "(only in sdl)"))
            } else {
                Some((&db_segments, "(only in database)"))
            }
        } else {
            None
        };
        if let Some((side_map, side)) = fold_side {
            // No note can ride a fold: a fold root is never the doc
            // segment, and its one-sidedness forces `comment_only`
            // false — the interpolation would be dead bytes.
            out.push_str(&format!(
                "    {label}: {side} `{}`\n",
                delta_text(&folded_block_text(side_map, key))
            ));
            continue;
        }
        match (sdl_segments.get(key), db_segments.get(key)) {
            (Some(sdl_text), Some(db_text)) => {
                if sdl_text != db_text {
                    out.push_str(&format!(
                        "    {label}: sdl `{}` / db `{}`{note}\n",
                        delta_text(trim_header(sdl_text)),
                        delta_text(trim_header(db_text))
                    ));
                }
            }
            (Some(sdl_text), None) => out.push_str(&format!(
                "    {label}: (only in sdl) `{}`{note}\n",
                delta_text(trim_header(sdl_text))
            )),
            (None, Some(db_text)) => out.push_str(&format!(
                "    {label}: (only in database) `{}`{note}\n",
                delta_text(trim_header(db_text))
            )),
            (None, None) => unreachable!("the key came from one of the maps"),
        }
    }
}

/// Whether a segment key names a nested child block — the
/// `field x` / `index ux` / `event e` shapes [`segments`] keys
/// children by (the differ emits each as its own finding row).
fn is_child_key(key: &str) -> bool {
    matches!(
        key.split(' ').next(),
        Some("field") | Some("index") | Some("event")
    ) && key.contains(' ')
}

/// A one-sided block's whole text: the opener line, then the block
/// path's body-clause lines recursively (a nested one-sided block —
/// access `jwt { verify { … } }` — emits its own opener, children,
/// and closer, so every nesting level closes and the quoted shape
/// stays balanced), then the root's closer at the opener's indent.
/// Siblings render in key order (alphabetical, the map's own — a
/// deterministic quote, not a source-order replay). The pieces are
/// the side's own segment texts — nothing re-prints from an AST.
fn folded_block_text(side: &BTreeMap<String, String>, root: &str) -> String {
    let opener = side[root].trim_end_matches('\n');
    let mut lines = vec![opener.to_string()];
    fold_children(side, root, &mut lines);
    let indent = opener.len() - opener.trim_start().len();
    lines.push(format!("{}{};", " ".repeat(indent), "}"));
    lines.join("\n")
}

/// One fold level's body lines: the DIRECT children of `prefix` (a
/// deeper key rides its own ancestor's recursion), each leaf clause
/// whole, each nested block recursing its own children and closing at
/// its opener's indent. An empty nested block (a `{`-ended key with
/// no children) closes immediately.
fn fold_children(side: &BTreeMap<String, String>, prefix: &str, lines: &mut Vec<String>) {
    let dot_prefix = format!("{prefix}.");
    for (key, body) in side.iter() {
        let Some(rest) = key.strip_prefix(&dot_prefix) else {
            continue;
        };
        if rest.contains('.') {
            continue;
        }
        let line = body.trim_end_matches('\n');
        let child_prefix = format!("{key}.");
        let has_children = side.keys().any(|other| other.starts_with(&child_prefix));
        let indent = line.len() - line.trim_start().len();
        lines.push(line.to_string());
        if has_children {
            fold_children(side, key, lines);
            lines.push(format!("{}{};", " ".repeat(indent), "}"));
        } else if line.ends_with('{') {
            lines.push(format!("{}{};", " ".repeat(indent), "}"));
        }
    }
}

/// The header segment's display text: a braced header drops its
/// trailing ` {` (the delta names the statement and its head
/// clauses, not the bookkeeping brace); anything else rides whole.
/// Multi-line bodies cannot reach here — the header is the first
/// line, the closer never segments.
fn trim_header(text: &str) -> &str {
    let trimmed = text.trim();
    trimmed.strip_suffix('{').map_or(trimmed, str::trim_end)
}

/// Segments one canonical statement text at clause boundaries: a
/// (key → segment text) map. The writer's invariants carry the walk:
///
/// - a `///` doc line is its own segment;
/// - the first non-doc line is the statement header (key `""`);
/// - a line whose first token is followed by `:` / `{` / `(` / `;`
///   starts a clause (the two-token spelling `on delete:` keys as
///   `on delete`; a `;`-terminated flag clause keys as its word); a
///   comma-list head (`get, post {`) is one clause whose key is the
///   whole list;
/// - a nested `field`/`index`/`event` head swallows its whole block
///   (children compare whole — the comparator compares them
///   individually when standalone);
/// - a block clause's opener pushes its key onto a path stack, and
///   body clauses nest under it (`jwt.verify.key` vs
///   `jwt.issuer.key`, api `get.then` vs `post.then`) — same-headed
///   clauses stay in distinct entries, so one can never overwrite
///   another's text and hide a delta;
/// - consecutive `///` doc lines accumulate into ONE segment (a
///   per-line key would keep only the last line);
/// - every other line continues the current segment (only a `raw`
///   body can hit this, and raw never segments here — the
///   round-trip pin in the tests rides this arm anyway).
///
/// The bookkeeping lines never delta: a block's `};` closer pops the
/// path stack and carries no text (the issue-#83 stray-closer fix —
/// it once glued onto the last body clause's segment), and the
/// statement's own `};` terminator drops entirely (it once rode the
/// header segment — the empty-brace blob). The nested child swallow
/// keeps its closer: the child segment renders nowhere (the differ
/// emits the child as its own finding), and the round-trip pin
/// covers its lines.
fn segments(text: &str) -> BTreeMap<String, String> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<(String, Vec<String>)> = None;
    let mut header_seen = false;
    // The head indent of the nested-child block being swallowed.
    let mut child_indent: Option<usize> = None;
    // The open block clauses as (full key path, indent). Segment keys
    // nest under their block's path — `jwt.verify.key` vs
    // `jwt.issuer.key`, api `get.then` vs `post.then` — which keeps
    // same-headed clauses in distinct map entries (a shared flat key
    // would let one clause's text overwrite another's and HIDE a
    // delta).
    let mut blocks: Vec<(String, usize)> = Vec::new();

    let flush = |map: &mut BTreeMap<String, String>,
                 current: &mut Option<(String, Vec<String>)>| {
        if let Some((key, lines)) = current.take() {
            map.insert(key, lines.join("\n") + "\n");
        }
    };

    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let stripped = line.trim_start();

        if let Some(open_indent) = child_indent {
            if let Some((_, lines)) = &mut current {
                lines.push(line.to_string());
            }
            if stripped == "};" && indent == open_indent {
                child_indent = None;
            }
            continue;
        }

        if stripped == "};" {
            // A block closer pops this level's frame (and anything
            // deeper, should a writer shape ever nest unevenly) and
            // carries no text — bookkeeping, never a delta.
            while blocks.last().is_some_and(|(_, at)| *at >= indent) {
                blocks.pop();
            }
            continue;
        }

        match segment_start(stripped, header_seen) {
            SegmentStart::None => {
                if let Some((_, lines)) = &mut current {
                    lines.push(line.to_string());
                }
            }
            SegmentStart::Doc => {
                // Consecutive doc lines accumulate into ONE segment —
                // a per-line key would let the map keep only the last
                // line and hide the others.
                match &mut current {
                    Some((key, lines)) if key == "///" => {
                        lines.push(line.to_string());
                    }
                    _ => {
                        flush(&mut map, &mut current);
                        current = Some(("///".to_string(), vec![line.to_string()]));
                    }
                }
            }
            SegmentStart::Header => {
                flush(&mut map, &mut current);
                header_seen = true;
                current = Some((String::new(), vec![line.to_string()]));
            }
            SegmentStart::Clause { key, child } => {
                flush(&mut map, &mut current);
                let full_key = match blocks.last() {
                    Some((path, _)) => format!("{path}.{key}"),
                    None => key.clone(),
                };
                current = Some((full_key.clone(), vec![line.to_string()]));
                if child && stripped.ends_with('{') {
                    child_indent = Some(indent);
                } else if stripped.ends_with('{') {
                    blocks.push((full_key, indent));
                }
            }
        }
    }
    flush(&mut map, &mut current);
    map
}

enum SegmentStart {
    None,
    Doc,
    Header,
    /// A clause head: the segment's key, and whether it is a nested
    /// child head (which swallows its block).
    Clause {
        key: String,
        child: bool,
    },
}

/// The clause-boundary test for one stripped line. `header_seen`
/// gates the header arm (the first non-doc line IS the header,
/// whatever its shape — `param $p = 1;` reads as a header, not a
/// clause named `param`).
fn segment_start(stripped: &str, header_seen: bool) -> SegmentStart {
    if stripped.starts_with("///") {
        return SegmentStart::Doc;
    }
    if !header_seen {
        return SegmentStart::Header;
    }
    let Some(head_len) = head_token_len(stripped) else {
        return SegmentStart::None;
    };
    let head = &stripped[..head_len];
    let raw_rest = &stripped[head_len..];
    let rest = raw_rest.trim_start();
    // A nested child: `field x: int;`, `index unique ux { … };`,
    // `event audited { … };` — the name is the field's second token
    // (its kind trails a `:`) and the token before the head line's
    // terminator for index/event (whose kind word may precede it).
    if matches!(head, "field" | "index" | "event")
        && let Some(name) = child_name(head, stripped)
    {
        return SegmentStart::Clause {
            key: format!("{head} {name}"),
            child: true,
        };
    }
    if rest.starts_with(':')
        || rest.starts_with('{')
        || rest.starts_with('(')
        || rest.starts_with(';')
    {
        return SegmentStart::Clause {
            key: clause_key(head, raw_rest),
            child: false,
        };
    }
    // A comma-list head — api method groups (`get, post { … };`).
    // The heads run to the opener; the list is ONE clause head, so
    // the group's body nests under its own path (`get, post.then` vs
    // `put, patch.then`) instead of colliding on a flat `then`.
    if rest.starts_with(',') && stripped.ends_with('{') {
        let heads = stripped[..stripped.len() - 1].trim_end();
        return SegmentStart::Clause {
            key: heads.to_string(),
            child: false,
        };
    }
    SegmentStart::None
}

/// The first token's length; `None` when the line starts
/// non-word-shaped (a continuation — only a `raw` body produces
/// these, and raw never segments here).
fn head_token_len(stripped: &str) -> Option<usize> {
    let len = stripped
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(stripped.len());
    (len > 0).then_some(len)
}

/// A nested child's name. Fields carry it as the second token (`x`
/// in `field x: int;` — the kind trails a `:`); index/event carry it
/// as the last token before the line's terminator (`ux` in
/// `index unique ux {`, `ct` in `index count ct;`), which for the
/// block-less field spelling `field x {` falls back to the second
/// token.
fn child_name(head: &str, stripped: &str) -> Option<String> {
    let tokens: Vec<&str> = stripped.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    if head == "field" {
        return Some(tokens[1].trim_end_matches([':', ';']).to_string());
    }
    let mut end = tokens.len();
    while end > 1 && (tokens[end - 1].ends_with('{') || tokens[end - 1] == "};") {
        // A block head's trailing `{` (or a bare `{` token) is not
        // the name.
        if tokens[end - 1].len() > 1 && tokens[end - 1] != "};" {
            break;
        }
        end -= 1;
    }
    if end > 1 && tokens[end - 1] != head {
        return Some(tokens[end - 1].trim_end_matches([';', '{']).to_string());
    }
    None
}

/// The clause key: the head word, widened to two tokens for the
/// `on delete:` spelling (exactly one separating space, a second
/// word, and the clause's own `:`).
fn clause_key(head: &str, raw_rest: &str) -> String {
    if !raw_rest.starts_with(' ') || raw_rest.starts_with("  ") {
        return head.to_string();
    }
    let second = &raw_rest[1..];
    let second_len = second
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(second.len());
    if second_len > 0 && second[second_len..].starts_with(':') {
        return format!("{head} {}", &second[..second_len]);
    }
    head.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::info::read::{Markers, RawRoute};

    fn canon(sdl: &str) -> String {
        let (file, diags) = crate::parser::parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        statement_text(file.items.first().expect("one statement"))
    }

    fn parse_stmt(text: &str) -> Stmt {
        let (file, diags) = crate::parser::parse_file(text);
        assert!(diags.is_empty(), "{text}: {diags:?}");
        file.items.into_iter().next().expect("one statement")
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

    /// The segmentation keys over a statement carrying the shapes
    /// the corpus rides: flag clauses, nested children (a whole
    /// block, name after the kind word), a block clause.
    #[test]
    fn segmentation_keys() {
        let text = canon(
            "table t { schemafull; field x: int; index unique ux { fields: x; }; \
             permissions { delete: full; }; };",
        );
        let map = segments(&text);
        let keys: Vec<&String> = map.keys().collect();
        assert_eq!(
            keys,
            [
                "",
                "field x",
                "index ux",
                "permissions",
                "permissions.delete",
                "schemafull"
            ]
        );
        // The nested child swallows its body: one segment, the whole
        // block (its own closer included — it renders nowhere).
        assert!(map["index ux"].contains("fields: x;"));
        // Coverage: every non-bookkeeping line of the text rides a
        // segment; and no segment but a swallowed child carries a
        // `};` closer (the bookkeeping drop — the issue-#83 fix).
        let seg_lines: std::collections::BTreeSet<&str> =
            map.values().flat_map(|s| s.lines()).collect();
        for line in text.lines().filter(|line| line.trim() != "};") {
            assert!(seg_lines.contains(line), "line lost: {line:?}");
        }
        for (key, segment) in &map {
            if !is_child_key(key) {
                assert!(
                    !segment.contains("};"),
                    "bookkeeping closer rode segment {key:?}: {segment:?}"
                );
            }
        }
    }

    /// Block-nested keys stay distinct — the jwt access's verify
    /// clauses are DIRECT (`algorithm:`/`key:`) while the issuer
    /// block's carry the same heads (`issuer.algorithm`); a flat key
    /// map would let the issuer's text overwrite the verify's and
    /// hide a delta. (The exposure guard + the issuer-compatibility
    /// gate force both keys to `'[REDACTED]'` and equal algorithms —
    /// the distinct KEYS are the pin.)
    #[test]
    fn segmentation_nested_block_paths() {
        let text = canon(
            "access a { on: db; type: jwt; algorithm: hs512; key: '[REDACTED]';
             issuer { algorithm: hs512; key: '[REDACTED]'; }; };",
        );
        let map = segments(&text);
        assert!(map.contains_key("algorithm"), "keys: {map:?}");
        assert!(map.contains_key("issuer.algorithm"), "keys: {map:?}");
        assert!(map.contains_key("issuer.key"), "keys: {map:?}");
        // Round-trip coverage, closers excepted (the bookkeeping drop).
        let seg_lines: std::collections::BTreeSet<&str> =
            map.values().flat_map(|s| s.lines()).collect();
        for line in text.lines().filter(|line| line.trim() != "};") {
            assert!(seg_lines.contains(line), "line lost: {line:?}");
        }
    }

    /// Consecutive doc lines accumulate into one segment — the map
    /// keeps every line, not just the last.
    #[test]
    fn segmentation_docs_accumulate() {
        let text = canon("/// first\n/// second\nparam $p = 1;");
        let map = segments(&text);
        assert!(map["///"].contains("first"));
        assert!(map["///"].contains("second"));
    }

    /// Comma-list api method heads (`get, post { … };`) are one
    /// clause whose key is the whole list — two groups' bodies then
    /// nest under distinct paths instead of colliding on a flat
    /// `then` (a drift in the FIRST group rendered nothing before
    /// this).
    #[test]
    fn segmentation_api_group_lists() {
        let text = canon(
            "api '/x' { get, post { then: (SELECT * FROM a); }; \
             put, patch { then: (SELECT * FROM b); }; };",
        );
        let map = segments(&text);
        assert!(map.contains_key("get, post.then"), "keys: {map:?}");
        assert!(map.contains_key("put, patch.then"), "keys: {map:?}");
        assert!(map["get, post.then"].contains("FROM a"));
        assert!(map["put, patch.then"].contains("FROM b"));
        // Round-trip coverage, closers excepted (the bookkeeping drop).
        let seg_lines: std::collections::BTreeSet<&str> =
            map.values().flat_map(|s| s.lines()).collect();
        for line in text.lines().filter(|line| line.trim() != "};") {
            assert!(seg_lines.contains(line), "line lost: {line:?}");
        }
    }

    /// A field's dotted path and the two-token `on delete:` spelling
    /// stay inside the nested child's whole-block segment.
    #[test]
    fn segmentation_nested_child_whole() {
        let text = canon("table t { field r: record<u> { reference { on delete: cascade; }; }; };");
        let map = segments(&text);
        assert_eq!(map.keys().collect::<Vec<_>>(), vec!["", "field r"]);
        assert!(map["field r"].contains("on delete: cascade;"));
    }

    /// A drift finding renders its clause deltas: a both-sided delta
    /// quotes both texts, a one-sided names its side, equal segments
    /// emit nothing. Findings sort by (scope, family, key) whatever
    /// the input order. The nested field's segment does NOT render
    /// under the table — the differ emits the child as its own
    /// finding row (the no-double-listing pin below).
    #[test]
    fn drift_delta_golden() {
        let a = parse_stmt("table t { schemafull; field x: int; };");
        let b = parse_stmt("table t { field x: string; };");
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Table,
            "t",
            a,
            b,
        )];
        assert_eq!(
            report(&findings),
            "1 finding - 1 define, 0 removals, 0 data-migration slots, \
             1 staged table\n\
             ~ db:app/main table t\n\
             \x20   schemafull: (only in sdl) `schemafull;`\n"
        );
    }

    /// The no-double-listing pin (issue #83): a drifted table and its
    /// drifted nested field carry TWO findings — the child renders as
    /// its own row once, and the table's clause delta carries no
    /// field segment.
    #[test]
    fn a_nested_child_never_double_lists() {
        let a = parse_stmt("table t { schemafull; field x: int; };");
        let b = parse_stmt("table t { field x: string; };");
        let findings = vec![
            drift(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                "t",
                a,
                b,
            ),
            drift(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "x",
                parse_stmt("field x: int { on: t; };"),
                parse_stmt("field x: string { on: t; };"),
            ),
        ];
        let text = report(&findings);
        // The child renders as its own row ONCE (its header's sdl and
        // db texts each carry the name — the row is the single one).
        assert_eq!(text.matches("~ db:app/main field x\n").count(), 1, "{text}");
        assert!(text.contains("    header: sdl `field x: int` / db `field x: string`\n"));
        assert!(!text.contains("    field x:"), "{text}");
    }

    /// A one-sided block folds its whole subtree into one delta
    /// (opener, body clauses, closer) — no opener fragment, no
    /// per-clause split, no stray `};` glued onto the last body
    /// clause (issue #83).
    #[test]
    fn a_one_sided_block_folds_whole() {
        let a = parse_stmt("table t { schemafull; permissions { delete: full; create: full; }; };");
        let b = parse_stmt("table t { schemafull; };");
        let text = report(&[drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Table,
            "t",
            a.clone(),
            b,
        )]);
        assert_eq!(
            text,
            "1 finding - 1 define, 0 removals, 0 data-migration slots, \
             1 staged table\n\
             ~ db:app/main table t\n\
             \x20   permissions: (only in sdl) `  permissions {\n\
             \x20   create: full;\n\
             \x20   delete: full;\n\
             \x20 };`\n",
            "{text}"
        );
        // A block held on BOTH sides renders only its differing body
        // clauses — the opener is equal, no fragment, no fold.
        let a = parse_stmt("table t { permissions { delete: full; create: none; }; };");
        let b = parse_stmt("table t { permissions { delete: none; }; };");
        let text = report(&[drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Table,
            "t",
            a,
            b,
        )]);
        assert_eq!(
            text,
            "1 finding - 1 define, 0 removals, 0 data-migration slots, \
             1 staged table\n\
             ~ db:app/main table t\n\
             \x20   permissions.create: (only in sdl) `create: none;`\n\
             \x20   permissions.delete: sdl `delete: full;` / db `delete: none;`\n",
            "{text}"
        );
    }

    /// A one-sided block holding a NESTED one-sided block folds
    /// balanced — every level closes at its own indent (the
    /// access-jwt shape, the segments doc's own example).
    #[test]
    fn a_one_sided_nested_block_folds_balanced() {
        let a = parse_stmt(
            "access ju { on: db; type: jwt; algorithm: hs512; key: '[REDACTED]'; \
             issuer { algorithm: hs512; key: '[REDACTED]'; }; };",
        );
        let b = parse_stmt("access ju { on: db; type: record; };");
        let text = report(&[drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Access,
            "ju",
            a,
            b,
        )]);
        assert!(
            text.contains(
                "\x20   issuer: (only in sdl) `  issuer {\n\
                 \x20   algorithm: hs512;\n\
                 \x20   key: '[REDACTED]';\n\
                 \x20 };`"
            ),
            "the nested block closes at its own indent:\n{text}"
        );
        // The outer block never folds (both sides hold it); the
        // issuer fold is the whole delta beside the type delta — the
        // equal header and `on:` emit nothing.
        assert!(!text.contains("    header:"));
        assert!(!text.contains("    on:"));
        assert!(
            text.contains("    type: sdl `type: jwt;` / db `type: record;`\n"),
            "{text}"
        );
    }

    /// A comment-only drift keys as `comment:` with the
    /// only-the-comment note (issue #86's vocabulary half); beside
    /// other deltas the label rides alone.
    #[test]
    fn a_comment_delta_keys_as_comment() {
        let only = report(&[drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Table,
            "t",
            parse_stmt("/// new doc\ntable t;"),
            parse_stmt("/// old doc\ntable t;"),
        )]);
        assert_eq!(
            only,
            "1 finding - 1 define, 0 removals, 0 data-migration slots, \
             1 staged table\n\
             ~ db:app/main table t\n\
             \x20   comment: sdl `/// new doc` / db `/// old doc` \
             (only the comment changed)\n",
            "{only}"
        );
        let beside = report(&[drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Table,
            "t",
            parse_stmt("/// new doc\ntable t { schemafull; };"),
            parse_stmt("/// old doc\ntable t;"),
        )]);
        assert!(beside.contains("    comment: sdl `/// new doc` / db `/// old doc`\n"));
        assert!(!beside.contains("only the comment changed"), "{beside}");
    }

    /// The class sigils and their notes: the database-only ON edge,
    /// apply-pending, expected-unmanaged, the raw route, and the
    /// redacted re-apply note.
    #[test]
    fn class_sigils_and_notes() {
        let a = parse_stmt("param $x = 1;");
        let b = parse_stmt("param $x = 2;");
        let mut markers: Box<Markers> = Box::default();
        markers.redacted = true;
        markers.raw_routed = Some(RawRoute::IndexLegacyVs);
        let findings = vec![
            DriftFinding {
                on_table_exists: false,
                scope: ScopeKey::Namespace("app".into()),
                family: Family::Index,
                key: "old".to_string(),
                class: DriftClass::DatabaseOnly {
                    database: Box::new(a.clone()),
                    on_table: Some("t".to_string()),
                    markers: Box::default(),
                },
            },
            DriftFinding {
                on_table_exists: false,
                scope: ScopeKey::Namespace("app".into()),
                family: Family::Param,
                key: "new".to_string(),
                class: DriftClass::SdlOnly {
                    sdl: Box::new(a.clone()),
                },
            },
            DriftFinding {
                on_table_exists: false,
                scope: ScopeKey::Root,
                family: Family::Namespace,
                key: "default".to_string(),
                class: DriftClass::ExpectedUnmanaged {
                    database: Box::new(a.clone()),
                },
            },
            DriftFinding {
                on_table_exists: false,
                scope: ScopeKey::Database("app".into(), "main".into()),
                family: Family::Param,
                key: "x".to_string(),
                class: DriftClass::Drift {
                    sdl: Box::new(a),
                    database: Box::new(b),
                    markers,
                },
            },
        ];
        assert_eq!(
            report(&findings),
            "4 findings - 2 defines, 1 removal, 0 data-migration slots, \
             1 expected-unmanaged (informational), 0 staged tables\n\
             \u{b7} root namespace default (expected-unmanaged: auto-seeded by \
             SurrealDB - declare it in SDL to manage)\n\
             \u{2212} ns:app index old ON t\n\
             + ns:app param new (apply-pending)\n\
             ~ db:app/main param x\n\
             \x20 raw route: IndexLegacyVs\n\
             \x20   header: sdl `param $x = 1;` / db `param $x = 2;`\n\
             \x20 note: re-applying the database's form defines the literal \
             '[REDACTED]' key\n"
        );
    }

    /// A raw body (or a raw-vs-structured cross) renders the
    /// whole-body delta — no clause segmentation over verbatim text.
    #[test]
    fn raw_body_delta() {
        let a = parse_stmt("raw { DEFINE FIELD x ON t; };");
        let b = parse_stmt("raw { DEFINE FIELD x ON t COMMENT 'x'; };");
        assert_eq!(
            report(&[drift(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Field,
                "x",
                a,
                b
            )]),
            "1 finding - 1 define, 0 removals, 0 data-migration slots, \
             0 staged tables\n\
             ~ db:app/main field x\n\
             \x20 body:\n\
             \x20   sdl: raw {\n\
             \x20       DEFINE FIELD x ON t;\n\
             \x20       };\n\
             \x20   db: raw {\n\
             \x20       DEFINE FIELD x ON t COMMENT 'x';\n\
             \x20       };\n"
        );
    }

    // ---------------------------------------------------------------
    // The slot manifest, the slotted rows, the kind note
    // ---------------------------------------------------------------

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

    /// The report legs: a database-only table and field mark their
    /// rows and land in the manifest (stakes one line each); a
    /// database-only event carries neither (the taxonomy's quiet
    /// half); a diverged field's kind change carries the
    /// informational note and no manifest entry; no slots, no
    /// manifest section. The whole text stays a pure function of the
    /// findings' order-independent multiset (the manifest rides the
    /// findings' own sort).
    #[test]
    fn slot_manifest_rows_and_the_kind_note() {
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
                parse_stmt("field gone: int { on: extra; };"),
                Some("extra"),
            ),
            db_only(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Event,
                "old",
                parse_stmt("event old { on: extra; when: true; then: (create x); };"),
                Some("extra"),
            ),
        ];
        let text = report(&findings);
        assert_eq!(
            text,
            "3 findings - 0 defines, 3 removals, 2 data-migration slots, \
             0 staged tables\n\
             \u{2212} db:app/main event old ON extra\n\
             \u{2212} db:app/main field gone ON extra (required data-migration slot)\n\
             \u{2212} db:app/main table stale (required data-migration slot)\n\
             \n\
             == required data-migration slots ==\n\
             \x20 db:app/main field gone ON extra - the removal deletes the field \
             definition only; the values remain on the rows; the script carries \
             the fill-in slot above the removal; when the script removes a whole \
             namespace or database instead, the WARNING above that REMOVE names \
             this finding\n\
             \x20 db:app/main table stale - the removal deletes the table and every \
             document in it; the script carries the fill-in slot above the \
             removal; when the script removes a whole namespace or database \
             instead, the WARNING above that REMOVE names this finding\n",
            "{text}"
        );
        // The multiset property: the manifest is order-independent.
        let mut shuffled = findings;
        shuffled.reverse();
        assert_eq!(report(&shuffled), text);
    }

    /// A diverged field whose kind changes carries the informational
    /// note (the rows keep their values; the new kind governs future
    /// writes) — and a drift on OTHER clauses alone carries none. No
    /// manifest section (a retype orphans nothing).
    #[test]
    fn the_kind_note_rides_kind_changes_only() {
        let kind_change = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: t; };"),
            parse_stmt("field x: int { on: t; };"),
        )];
        assert_eq!(
            report(&kind_change),
            "1 finding - 1 define, 0 removals, 0 data-migration slots, \
             1 staged table\n\
             ~ db:app/main field x\n\
             \x20   header: sdl `field x: string` / db `field x: int`\n\
             \x20 note: the kind changes (db `int` -> sdl `string`); existing rows keep \
             their values (the engine validates no rows at define time), but a data \
             step that rewrites them fails while the old kind is in force; run the \
             data step after the definitions - the script stages this table \
             (relaxed shell -> REQUIRED data section -> final definitions)\n\
             \n\
             == staged migrations ==\n\
             \x20 db:app/main table t - the data section re-keys these fields: `x`; \
             fill the section (or delete it) before commit\n",
            "{}",
            report(&kind_change)
        );
        let other_clause = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: int { on: t; default: 1; };"),
            parse_stmt("field x: int { on: t; };"),
        )];
        assert_eq!(
            report(&other_clause),
            "1 finding - 1 define, 0 removals, 0 data-migration slots, \
             0 staged tables\n\
             ~ db:app/main field x\n\
             \x20   default: (only in sdl) `default: 1;`\n",
            "{}",
            report(&other_clause)
        );
    }

    /// No orphaning findings, no manifest section — the report's
    /// summary line rides, the manifest heading does not.
    #[test]
    fn no_slots_no_manifest() {
        let findings = vec![drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Param,
            "p",
            parse_stmt("param $p = 1;"),
            parse_stmt("param $p = 2;"),
        )];
        let text = report(&findings);
        assert!(text.starts_with("1 finding - "));
        assert!(!text.contains("== required data-migration slots =="));
        assert!(!text.contains("== staged migrations =="));
    }

    // ---------------------------------------------------------------
    // The name fold (the script side shares it) and the report's
    // own voice/ASCII pins.
    // ---------------------------------------------------------------

    use crate::info::script::is_line_comment_terminator;
    use crate::info::script::voice::assert_voice_clean;

    /// The report's line invariant: no rendered line carries a raw
    /// control byte or line-comment terminator, and a shaped
    /// injection tail never reaches column 0 — a folded hostile name
    /// rides the visible escape spelling, never a line break. The
    /// terminator check is the vendored lexer's five-spelling set
    /// (`is_line_comment_terminator`): the Zl/Zp separators are not
    /// control characters, so the control-byte check alone misses
    /// the two display-side line breaks.
    fn assert_no_raw_terminator(text: &str) {
        for line in text.lines() {
            assert!(
                line.chars()
                    .all(|c| !c.is_control() && !is_line_comment_terminator(c)),
                "a terminator byte broke a report line: {line:?}"
            );
        }
        assert!(
            !text
                .lines()
                .any(|line| line.starts_with("REMOVE TABLE victim")),
            "the hostile tail reached column 0:\n{text}"
        );
    }

    /// The report-side twin of the script module's hostile-name pin:
    /// the report's
    /// row and manifest sites interpolate database-side names, and a
    /// backtick ident may carry a raw line terminator — the hostile
    /// name must render its folded escape spelling on ONE report
    /// line, on the row, in the slot manifest, and in the scope label
    /// alike (the row's tail at column 0 would read as a finding of
    /// its own). Sane names ride the fold byte-identical — the
    /// literal-expected pins above hold the spellings.
    #[test]
    fn a_hostile_name_folds_on_the_report_lines() {
        let hostile = "a\nREMOVE TABLE victim;--";
        let folded = comment_text(hostile);
        // The row and the slot manifest line: a database-only table
        // carrying the hostile key, scoped to a hostile namespace —
        // the scope label folds the same way.
        let findings = vec![db_only(
            ScopeKey::Namespace(hostile.into()),
            Family::Table,
            hostile,
            parse_stmt("table t;"),
            None,
        )];
        let text = report(&findings);
        assert!(
            text.lines()
                .any(|line| line.starts_with('\u{2212}') && line.contains(&folded)),
            "the row folds the name:\n{text}"
        );
        assert!(
            text.lines().any(|line| line.starts_with("  ")
                && line.contains(&folded)
                && line.contains("the removal deletes the table")),
            "the slot manifest line folds the name:\n{text}"
        );
        assert_no_raw_terminator(&text);
        // The staged manifest line: the hostile table name rides the
        // staged entry (built directly — the pin targets the
        // manifest's interpolation, not `staged_tables`).
        let finding = drift(
            ScopeKey::Database("app".into(), "main".into()),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: t; };"),
            parse_stmt("field x: int { on: t; };"),
        );
        let staged = vec![StagedTable {
            scope: &finding.scope,
            table: hostile,
            rekeys: vec![&finding],
        }];
        let manifest = staged_manifest(&staged);
        assert!(
            manifest
                .lines()
                .any(|line| line.starts_with("  ") && line.contains(&folded)),
            "the staged manifest line folds the name:\n{manifest}"
        );
        assert_no_raw_terminator(&manifest);
    }

    /// The DELTA texts' fold. The
    /// writer's escape rule folds the C0 terminators inside backtick
    /// idents but leaves NEL and the Zl/Zp separators raw — three of
    /// the five shared line-comment terminators — so a hostile name
    /// riding a statement's canonical text would put the raw byte on
    /// a delta line, and any display that breaks on Zl/Zp shows the
    /// attacker-chosen tail as a finding row of its own. The fold
    /// lives at the print boundary ([`delta_text`]); the pin walks
    /// the three spellings through the clause-delta header segment
    /// (the two sides' names differ, so the header delta renders and
    /// carries the name) and the raw body's whole-body delta
    /// (verbatim text), asserting the folded spelling on ONE line
    /// and no terminator byte anywhere.
    #[test]
    fn a_hostile_name_folds_in_the_delta_lines() {
        for hostile in [
            "a\u{85}REMOVE TABLE victim;--",
            "a\u{2028}REMOVE TABLE victim;--",
            "a\u{2029}REMOVE TABLE victim;--",
        ] {
            let folded = comment_text(hostile);
            // The clause-delta path: the statement's own name carries
            // the terminator (the writer leaves it raw), and the
            // differing header segments quote it.
            let findings = vec![drift(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Table,
                hostile,
                parse_stmt(&format!("table `{hostile}`;")),
                parse_stmt(&format!("table `{hostile}` {{ schemafull; }};")),
            )];
            let text = report(&findings);
            assert!(
                text.lines()
                    .any(|line| line.starts_with("    header:") && line.contains(&folded)),
                "the clause delta folds the name:\n{text}"
            );
            assert_no_raw_terminator(&text);
            // The whole-body delta path: a raw body rides verbatim.
            let findings = vec![drift(
                ScopeKey::Database("app".into(), "main".into()),
                Family::Raw,
                hostile,
                parse_stmt("raw { DEFINE THING ok; };"),
                parse_stmt(&format!("raw {{ DEFINE THING `{hostile}`; }};")),
            )];
            let text = report(&findings);
            assert!(
                text.lines()
                    .any(|line| line.starts_with("        ") && line.contains(&folded)),
                "the body delta folds the name:\n{text}"
            );
            assert_no_raw_terminator(&text);
        }
    }

    /// The report's dialect pin (the script's
    /// `the_generator_prose_stays_ascii_and_voiced`, report-side):
    /// every rendered byte is printable ASCII except the two FROZEN
    /// sigils (`\u{2212}` and `\u{b7}` — the sigil column is
    /// CI-greppable and stays), and the prose never addresses the
    /// reader, never sells, and never shouts — the fixture drives
    /// every prose path at once (the kind note with its staged arm
    /// and its folded hostile kind, the unique-index note, the
    /// redacted and raw-route notes, both slot carriers' manifest
    /// lines, the staged manifest, the expected-unmanaged row and
    /// its informational summary clause, the apply-pending row, the
    /// empty-key hint row, and the whole-body delta's labels).
    #[test]
    fn the_report_prose_stays_ascii_and_voiced() {
        let scope = ScopeKey::Database("app".into(), "main".into());
        // The kind-change drift: the kind note's staged arm + the
        // staged manifest row.
        let rekey = drift(
            scope.clone(),
            Family::Field,
            "x",
            parse_stmt("field x: string { on: items; };"),
            parse_stmt("field x: int { on: items; };"),
        );
        // The kind note's fold: a hostile kind rides the note (
        // `write_kind`'s escape set leaves U+2028 raw,
        // the SDL side rides the source spelling; the in-module path
        // is the SDL parse, and the INFO mode's `canonical_kind_text`
        // reaches the same raw spelling through `write_kind`).
        let hostile_kind = drift(
            scope.clone(),
            Family::Field,
            "kinded",
            parse_stmt("field kinded: record<`a\u{2028}b`> { on: items; };"),
            parse_stmt("field kinded: string { on: items; };"),
        );
        // The unique-index hazard note (the table exists db-side).
        let mut unique = drift(
            scope.clone(),
            Family::Index,
            "uq",
            parse_stmt("index unique uq { on: items; fields: x; };"),
            parse_stmt("index unique uq { on: items; fields: y; };"),
        );
        unique.on_table_exists = true;
        // The redacted re-apply note and the raw route.
        let mut markers: Box<Markers> = Box::default();
        markers.redacted = true;
        markers.raw_routed = Some(RawRoute::IndexLegacyVs);
        let param = DriftFinding {
            on_table_exists: false,
            scope: scope.clone(),
            family: Family::Param,
            key: "p".to_string(),
            class: DriftClass::Drift {
                sdl: Box::new(parse_stmt("param $p = 1;")),
                database: Box::new(parse_stmt("param $p = 2;")),
                markers,
            },
        };
        let findings = vec![
            param,
            rekey,
            hostile_kind,
            unique,
            // Both slot carriers: the table's and the field's manifest
            // lines.
            db_only(
                scope.clone(),
                Family::Table,
                "legacy",
                parse_stmt("table legacy;"),
                None,
            ),
            db_only(
                scope.clone(),
                Family::Field,
                "gone",
                parse_stmt("field gone: int { on: legacy; };"),
                Some("legacy"),
            ),
            // The expected-unmanaged row + the informational summary
            // clause.
            DriftFinding {
                on_table_exists: false,
                scope: ScopeKey::Root,
                family: Family::Namespace,
                key: "default".to_string(),
                class: DriftClass::ExpectedUnmanaged {
                    database: Box::new(parse_stmt("namespace default;")),
                },
            },
            // The apply-pending row.
            DriftFinding {
                on_table_exists: false,
                scope: scope.clone(),
                family: Family::Param,
                key: "fresh".to_string(),
                class: DriftClass::SdlOnly {
                    sdl: Box::new(parse_stmt("param $fresh = 1;")),
                },
            },
            // The empty-key hint row: the header-less raw body's
            // first line stands in for the name.
            DriftFinding {
                on_table_exists: false,
                scope: scope.clone(),
                family: Family::Raw,
                key: String::new(),
                class: DriftClass::DatabaseOnly {
                    database: Box::new(parse_stmt("raw { DEFINE THING odd ON legacy; };")),
                    on_table: None,
                    markers: Box::default(),
                },
            },
            // The whole-body delta: a raw pair renders the body
            // labels.
            drift(
                scope.clone(),
                Family::Raw,
                "rawf",
                parse_stmt("raw { DEFINE FIELD x ON t; };"),
                parse_stmt("raw { DEFINE FIELD x ON t TYPE int; };"),
            ),
        ];
        let text = report(&findings);
        for line in text.lines() {
            assert!(
                line.chars()
                    .all(|c| (c.is_ascii() && !c.is_control()) || c == '\u{2212}' || c == '\u{b7}'),
                "a non-ASCII or control byte reached the report: {line:?}"
            );
            assert_voice_clean(line);
        }
        // Coverage: every prose path rode the scan (renders-or-fail,
        // so the fixture cannot go vacuous).
        assert!(text.contains("note: the kind changes"), "{text}");
        assert!(text.contains("the script stages this table"), "{text}");
        assert!(
            text.contains("note: the unique index builds over existing rows"),
            "{text}"
        );
        assert!(
            text.contains("note: re-applying the database's form"),
            "{text}"
        );
        assert!(text.contains("raw route:"), "{text}");
        assert!(
            text.contains("== required data-migration slots =="),
            "{text}"
        );
        assert!(text.contains("== staged migrations =="), "{text}");
        assert!(
            text.contains("expected-unmanaged: auto-seeded by SurrealDB"),
            "{text}"
        );
        assert!(text.contains("(informational)"), "{text}");
        assert!(text.contains("(apply-pending)"), "{text}");
        assert!(
            text.lines()
                .any(|line| line.starts_with('\u{2212}')
                    && line.contains("raw - DEFINE THING odd")),
            "the empty-key hint row rides:\n{text}"
        );
        assert!(text.contains("  body:"), "{text}");
        assert!(text.contains("    sdl: raw {"), "{text}");
        // The kind note's fold rode the scan: the hostile kind
        // renders its folded spelling on the note line (the ASCII
        // scan above fails on the raw byte — renders-or-fail keeps
        // the arm honest), and the line invariant walks this
        // fixture's whole text.
        assert!(
            text.lines()
                .any(|line| line.starts_with("  note: the kind changes")
                    && line.contains(&comment_text("a\u{2028}b"))),
            "the kind note folds the hostile kind:\n{text}"
        );
        assert_no_raw_terminator(&text);
    }
}
