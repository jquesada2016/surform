//! The staged-migration taxonomy (issue #65): which findings stage a
//! table through the relaxed shell → REQUIRED data section → final
//! definitions sequence, which fields re-key, and which indexes the
//! rebuild note offers. ONE home for the FINDINGS-LEVEL
//! classification — the report (the manifest and the corrected kind
//! note) and the script (the prelude's emission) both read it. The
//! SDL-side refinements (a converged view/lightweight DECLARATION the
//! findings cannot see, the shells' declared gate) live in the
//! script's own walk, which alone holds the file: a report row there
//! can mark a table the script then (correctly) declines to stage —
//! the #56 presentation-gap family, fail-safe in the report's
//! direction (an over-reported obligation, never a missed one).
//!
//! The probe-pinned engine facts the taxonomy stands on (3.3.0,
//! pinned 2026-10-11, tests/migrate_staged.rs): a field kind change
//! NEVER re-validates existing rows at define time (schemaful or
//! not, in or out of a transaction), but a data step that rewrites
//! the values FAILS while the old kind is still in force — the error
//! names the existing row, which reads like a define-time rejection
//! (the issue's false premise). The workable order is
//! define-then-migrate, hence the staging. A UNIQUE index over
//! duplicate rows is the ONE row-validating define; permission
//! tightening is catalog-only.

use crate::ast::{Stmt, TableKind};
use crate::info::diff::{DriftClass, DriftFinding};
use crate::info::normalize::{Family, ScopeKey};

/// Whether a Field finding's pair changes the kind — the staged-
/// migration trigger. Both resolved statements carry the canonical
/// kind texts, so the text comparison is the differ's own kind-AST
/// comparison.
pub(crate) fn kind_changed(sdl: &Stmt, database: &Stmt) -> bool {
    match (sdl, database) {
        (Stmt::Field(sdl_field), Stmt::Field(db_field)) => sdl_field.kind != db_field.kind,
        _ => false,
    }
}

/// One staged table: a table the script re-keys through the relaxed
/// shell → data section → final definitions sequence (issue #65).
pub(crate) struct StagedTable<'a> {
    pub scope: &'a crate::info::normalize::ScopeKey,
    /// The table's key (its name as the findings spell it).
    pub table: &'a str,
    /// The kind-changed field findings on this table — the sections'
    /// tenants (a table staged only by a table-level drift carries
    /// none: nothing re-keys, so no data section).
    pub rekeys: Vec<&'a DriftFinding>,
}

/// Whether a table statement's kind excludes staging: a view holds
/// no rows to re-key (its AS SELECT is the whole definition — a
/// `SCHEMALESS` shell would destroy it), and a lightweight host
/// refuses child defines, so the relax lines would fail the
/// transaction. The SDL-side twin (a converged declaration the
/// findings cannot see) is the script's own walk.
pub(crate) fn table_kind_stages(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Table(table) => {
            !table.kind.is_view()
                && !matches!(
                    table.kind,
                    TableKind::Relation {
                        lightweight: true,
                        ..
                    }
                )
        }
        _ => true,
    }
}

/// The staged set: every table a kind-changed field finding or a
/// table-level Drift finding stages. Excluded (never staged): a
/// table whose either side is a view or a lightweight relation, and
/// every apply-pending table (SdlOnly — its key range is empty; no
/// rows to re-key). Sorted by (scope, table key) — a pure function
/// of the findings.
pub(crate) fn staged_tables(findings: &[DriftFinding]) -> Vec<StagedTable<'_>> {
    let mut staged: Vec<StagedTable<'_>> = Vec::new();
    // The table-level drifts first (the entries the field pass joins)
    // — and the keys the table pass DECLINES (either side a view or a
    // lightweight relation) are recorded: the field pass must honor
    // them, or a kind-changed child would re-open a table whose
    // database side is a view and stage a shell over it (the
    // SDL's final defines would repair the
    // shape, but the ceremony — a REQUIRED section for a table with
    // no re-keyable rows — and the mid-transaction view retype are
    // never wanted).
    let mut declined: Vec<(&ScopeKey, &str)> = Vec::new();
    for finding in findings {
        if finding.family != Family::Table {
            continue;
        }
        let DriftClass::Drift { sdl, database, .. } = &finding.class else {
            continue;
        };
        if !table_kind_stages(sdl) || !table_kind_stages(database) {
            declined.push((&finding.scope, finding.key.as_str()));
            continue;
        }
        staged.push(StagedTable {
            scope: &finding.scope,
            table: &finding.key,
            rekeys: Vec::new(),
        });
    }
    // The kind-changed fields join their host's entry (or open one —
    // a converged table with a re-keyed field child stages too; its
    // shell relaxes nothing but the data section still applies) —
    // unless the host is one the table pass declined.
    for finding in findings {
        if finding.family != Family::Field {
            continue;
        }
        let DriftClass::Drift { sdl, database, .. } = &finding.class else {
            continue;
        };
        if !kind_changed(sdl, database) {
            continue;
        }
        let Stmt::Field(field) = sdl.as_ref() else {
            continue;
        };
        if declined
            .iter()
            .any(|(scope, key)| *scope == &finding.scope && *key == field.on.raw)
        {
            continue;
        }
        match staged
            .iter_mut()
            .find(|entry| entry.scope == &finding.scope && entry.table == field.on.raw)
        {
            Some(entry) => entry.rekeys.push(finding),
            None => staged.push(StagedTable {
                scope: &finding.scope,
                table: &field.on.raw,
                rekeys: vec![finding],
            }),
        }
    }
    staged.sort_by(|a, b| (&a.scope, a.table).cmp(&(&b.scope, b.table)));
    staged
}

/// Whether an index statement qualifies for the rebuild note: a
/// UNIQUE index on the staged table (its build is the one
/// row-validating define — it fails over colliding rows — and the
/// SDL's own define re-applies it, so the note doubles as the dedupe
/// reminder), or any index on it whose field expressions' canonical
/// texts equal a re-keyed field's key — exact equality, so a
/// transformed expression (`x * 2`) can FALSE-NEGATIVE (rewrites the
/// field, gets no note; the safe side — the note is advice, and the
/// UNIQUE arm still catches the failing builds). A nested member's
/// `on:` carries its host (the parser's fold fills it); the
/// empty-`on` acceptance is defense in depth.
pub(crate) fn index_qualifies(stmt: &Stmt, table: &str, rekeyed: &[String]) -> bool {
    let Stmt::Index(index) = stmt else {
        return false;
    };
    if !index.on.raw.is_empty() && index.on.raw != table {
        return false;
    }
    if matches!(index.kind, crate::ast::IndexKind::Unique) {
        return true;
    }
    index
        .fields
        .iter()
        .any(|expr| rekeyed.iter().any(|key| expr.canon.as_str() == key))
}
