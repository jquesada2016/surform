//! The differ's comparator core: the pre-merge SDL
//! source × the INFO read-model → a typed drift model, per
//! (scope, family, key). Both sides resolve to minimal form FIRST —
//! the merge's own [`elide_and_fold`](super::normalize) plus kind
//! algebra canonicalization and the representation-equivalence folds —
//! so a converged pair is one that would survive retro-apply and
//! re-apply byte-stably; anything else is drift, in one of four
//! classes (the report renders over these findings and the
//! convergence script renders the migration; a converged key emits no
//! finding — an absent key IS converged).
//!
//! - [`DriftClass::Drift`] — both sides hold it and the definitions
//!   differ; the finding carries both resolved statements (clause
//!   deltas are the report's rendering; the script defines the SDL
//!   side).
//! - [`DriftClass::DatabaseOnly`] — the database holds it, the SDL
//!   file doesn't: a REMOVE candidate, carrying the `ON <table>` edge
//!   the script's child-before-parent order needs.
//! - [`DriftClass::SdlOnly`] — the SDL file holds it, the database
//!   doesn't: apply-pending — the script defines it, `DEFINE …
//!   OVERWRITE` from this statement (codegen model item 3, as the
//!   migration decision rewrote it).
//! - [`DriftClass::ExpectedUnmanaged`] — informational: the
//!   auto-seeded default namespace (its comment fingerprint), which
//!   the SDL file never declared.
//!
//! The comparison contract (spec/README.md's differ bullet, as
//! amended): authored spellings never participate ([`Authored`]'s
//! value-only equality, [`Carried`]'s always-equal), spans never
//! participate (`Module::from_span` zeroed), kinds compare under
//! the merge's kind algebra (canonicalized inside the
//! resolved form — one mechanism, object keys class- and order-blind
//! per upstream's `BTreeMap`), and expressions compare over canonical
//! text modulo the single-statement block unwrap (probe-pinned: the
//! printer keeps a braced single statement braced and parenthesizes
//! the bare form — `signin: select …;` and `signin: { select …; };`
//! are one definition under the unwrap).
//!
//! Non-comparability doubles the merge's preserve list: what INFO
//! cannot report never drifts. User secrets skip outright (presence
//! is the only fact); access duration sides the access's own
//! capability silently drops compare converged when the read side is
//! unset (the linter flags the authored side); the
//! url-issuer ALGORITHM never renders; a bearer's `jwt { … }` is
//! stored but never rendered; `database strict;` and index
//! `concurrently;` are server-side blindness; table `delete: full;`
//! is ambiguous-absent and one-directional by construction (the
//! projection drops the delete slot unless it carries a real
//! expression). The representation-equivalence arms land here: the
//! read's symmetric issuer mirror and the authored
//! `any { permissions: full; }` both fold to their absence.

use crate::ast::{
    Access, AccessDuration, AccessKind, Api, ApiAnyBlock, Field, FieldDefault, Function, Index,
    IndexKind, JwtIssuer, JwtSource, JwtVerify, OnDelete, OpPermissions, Permission,
    PermissionGroup, PermissionOp, SdlFile, Stmt, Table, TableKind,
};
use crate::kind::{normalize_kind, parse_kind_ast, write_kind};
use crate::lexer::{Scanner, Span};

use super::normalize::{Family, ReadModel, ScopeKey, ScopeWalk, classify, elide_and_fold};
use super::place::{raw_match_key, raw_on_table};
use super::read::{Markers, ReadEntry};

// ---------------------------------------------------------------------
// The drift model
// ---------------------------------------------------------------------

/// One drift finding: a (scope, family, key) triple and its class.
/// A converged key emits no finding.
#[derive(Debug, Clone)]
pub struct DriftFinding {
    pub scope: ScopeKey,
    pub family: Family,
    pub key: String,
    pub class: DriftClass,
    /// Whether the finding's `ON <table>` edge (field/index/event —
    /// the database-side statement for database-only findings) names
    /// a table the read model holds at this scope. The staged-
    /// migration taxonomy reads it: a UNIQUE index define over a
    /// table that exists database-side builds over existing rows
    /// (probe-pinned: the build fails when two rows collide), while
    /// a define whose table is itself apply-pending runs over an
    /// empty key range. `false` for every edgeless statement.
    pub on_table_exists: bool,
}

/// The class of one (scope, family, key) triple.
#[derive(Debug, Clone)]
pub enum DriftClass {
    /// Both sides hold it and the resolved definitions differ. The
    /// finding carries both resolved statements (the report
    /// segments them at the family's clause keys) and the read
    /// entry's markers (the redacted-access note, the raw route's
    /// name).
    Drift {
        sdl: Box<Stmt>,
        database: Box<Stmt>,
        markers: Box<Markers>,
    },
    /// The database holds it, the SDL file doesn't: a REMOVE
    /// candidate. `on_table` is the `ON <table>` edge (index/event/
    /// field, raw-routed alike) the script's child-before-parent
    /// order needs.
    DatabaseOnly {
        database: Box<Stmt>,
        on_table: Option<String>,
        markers: Box<Markers>,
    },
    /// The SDL file holds it, the database doesn't: apply-pending.
    /// The script defines it — `DEFINE … OVERWRITE` rendered from
    /// this authored statement (codegen model item 3, as the
    /// migration decision rewrote it).
    SdlOnly { sdl: Box<Stmt> },
    /// The auto-seeded default namespace — informational, never a
    /// REMOVE candidate and never drift, unless the SDL file declares
    /// the namespace (then it compares ordinarily).
    ExpectedUnmanaged { database: Box<Stmt> },
}

/// The comparator: the SDL file × the read model → the drift
/// findings. Matching rides the merge's own (scope, family, key)
/// walk — [`ScopeWalk`] plus the per-statement lookup scopes — so a
/// statement compares against exactly the entry retro-apply would
/// adopt. The diff covers exactly the scopes the `--info` tags
/// presented; a scope never ingested contributes nothing.
///
/// The SDL side compares in its EXPANDED form (spec/mixin.md) — the
/// expansion runs here, not at the caller: the database only ever
/// holds a spread's members as concrete definitions, so the findings
/// are exactly those definitions' drift, and no caller can forget
/// the lowering (the authored file passes as-is; mixin statements
/// and spread entries drop out of the comparison entirely).
pub fn diff(sdl: &SdlFile, read: &ReadModel) -> Vec<DriftFinding> {
    let lowered = crate::expand::expand(sdl);
    let sdl = &lowered;
    let mut remaining = read.clone();
    let mut findings = Vec::new();
    // The SDL's standalone children, by scope and host — the
    // placement mirror's lookup ([`SdlStandalone`], issue #51).
    // Collected in a pre-pass with its own scope walk: a table's
    // compare_children runs at the TABLE's walk position, which may
    // precede the standalone in source order.
    let mut standalone_walk = ScopeWalk::new();
    let mut standalones: Vec<SdlStandalone<'_>> = Vec::new();
    for stmt in &sdl.items {
        let scope = standalone_walk.lookup_scope(stmt);
        match stmt {
            Stmt::Field(field) => {
                let (family, key) = classify(stmt);
                standalones.push(SdlStandalone {
                    scope,
                    host: &field.on.raw,
                    family,
                    key,
                });
            }
            Stmt::Event(event) => {
                let (family, key) = classify(stmt);
                standalones.push(SdlStandalone {
                    scope,
                    host: &event.on.raw,
                    family,
                    key,
                });
            }
            Stmt::Index(index) => {
                let (family, key) = classify(stmt);
                standalones.push(SdlStandalone {
                    scope,
                    host: &index.on.raw,
                    family,
                    key,
                });
            }
            _ => {}
        }
        standalone_walk.advance(stmt);
    }
    let mut walk = ScopeWalk::new();
    for stmt in &sdl.items {
        let scope = walk.lookup_scope(stmt);
        match_sdl_statement(
            &mut remaining,
            read,
            scope,
            stmt,
            &mut findings,
            &standalones,
        );
        walk.advance(stmt);
    }
    // Every entry no SDL statement matched is database-side: REMOVE
    // candidates, or the auto-seeded namespace's informational class.
    let declared: Vec<String> = sdl
        .items
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Namespace(ns) => Some(ns.name.raw.clone()),
            _ => None,
        })
        .collect();
    for (scope, entry) in remaining_leftovers(&remaining) {
        let database = resolve(entry.stmt.clone());
        let (family, key) = finding_key(&entry.stmt);
        let class = match family {
            Family::Namespace
                if entry.markers.auto_seeded_default_namespace && !declared.contains(&key) =>
            {
                DriftClass::ExpectedUnmanaged {
                    database: Box::new(database),
                }
            }
            _ => DriftClass::DatabaseOnly {
                on_table: on_table_of(&entry.stmt),
                database: Box::new(database),
                markers: Box::new(entry.markers.clone()),
            },
        };
        findings.push(DriftFinding {
            on_table_exists: on_table_exists(read, &scope, &entry.stmt),
            scope,
            family,
            key,
            class,
        });
    }
    findings
}

/// Whether the statement's `ON <table>` edge names a table the read
/// model holds at this scope — the ORIGINAL model (a matched table
/// still exists database-side; only the walk's `remaining` clone
/// loses entries). The staged-migration taxonomy's only model fact.
fn on_table_exists(read: &ReadModel, scope: &ScopeKey, stmt: &Stmt) -> bool {
    on_table_of(stmt).is_some_and(|table| read_holds_table(read, scope, &table))
}

/// Whether a table `name` stands in the model at `scope`.
fn read_holds_table(read: &ReadModel, scope: &ScopeKey, name: &str) -> bool {
    scope_entries(read, scope).is_some_and(|entries| {
        entries
            .family(Family::Table)
            .any(|entry| matches!(&entry.stmt, Stmt::Table(table) if table.name.raw == name))
    })
}

/// The read model's leftovers, scope order (root, namespaces,
/// databases) and family order within a scope.
fn remaining_leftovers(read: &ReadModel) -> Vec<(ScopeKey, &ReadEntry)> {
    let mut out: Vec<(ScopeKey, &ReadEntry)> = read
        .root
        .leftovers_with_family()
        .map(|(_, entry)| (ScopeKey::Root, entry))
        .collect();
    for (name, entries) in &read.namespaces {
        out.extend(
            entries
                .leftovers_with_family()
                .map(|(_, entry)| (ScopeKey::Namespace(name.clone()), entry)),
        );
    }
    for (ns, db, entries) in &read.databases {
        out.extend(
            entries
                .leftovers_with_family()
                .map(|(_, entry)| (ScopeKey::Database(ns.clone(), db.clone()), entry)),
        );
    }
    out
}

/// A finding's (family, key): [`classify`] for parsed statements, the
/// raw body's `DEFINE` header for raw ones (falling back to the
/// family-less key when the header doesn't parse — both sides then go
/// unmatched).
fn finding_key(stmt: &Stmt) -> (Family, String) {
    match stmt {
        Stmt::Raw(raw) => raw_match_key(&raw.body).unwrap_or((Family::Raw, String::new())),
        other => classify(other),
    }
}

/// The `ON <table>` edge a database-only statement carries (the
/// REMOVE script's child-before-parent order needs it).
fn on_table_of(stmt: &Stmt) -> Option<String> {
    match stmt {
        Stmt::Index(index) => Some(index.on.raw.clone()),
        Stmt::Event(event) => Some(event.on.raw.clone()),
        Stmt::Field(field) => Some(field.on.raw.clone()),
        Stmt::Raw(raw) => raw_on_table(&raw.body),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Matching: the SDL statement's side of the walk
// ---------------------------------------------------------------------

/// The lowered SDL's standalone child, by scope and host — the
/// placement mirror's lookup (issue #51): when compare_children's
/// database-side arm finds a database child whose (family, key) an
/// SDL standalone authored against the same host also carries, the
/// child is DEFERRED to the SDL side's own twin path — which folds
/// it (converged) or files the Drift (diverged). Filing
/// database-only there was the phantom: the SDL manages the child,
/// so nothing is database-only.
struct SdlStandalone<'a> {
    scope: ScopeKey,
    host: &'a str,
    family: Family,
    key: String,
}

/// Matches one SDL statement against its scope's entries and files
/// the finding. Unmatched standalones (field/index/event) fall back
/// to their table's nested twin before classifying SDL-only.
fn match_sdl_statement(
    remaining: &mut ReadModel,
    read: &ReadModel,
    scope: ScopeKey,
    stmt: &Stmt,
    findings: &mut Vec<DriftFinding>,
    standalones: &[SdlStandalone<'_>],
) {
    // A scope no tag presented supports no claim: an apply-pending
    // verdict there would be false (the database side was never
    // examined). Ingested scopes are tracked even when their INFO was
    // empty — those DO classify (an empty interchange genuinely lacks
    // the statement). The check rides the head: `root` is a struct
    // field, so an absent scope is not always a missing entry.
    if !read.ingested(&scope) {
        return;
    }
    let scope_entries = match scope_entries_mut(remaining, &scope) {
        Some(entries) => entries,
        None => {
            classify_unmatched(read, scope, stmt, findings);
            return;
        }
    };
    let (family, key) = finding_key(stmt);
    // 1. The structured twin.
    if let Some(entry) = scope_entries.take_named(family, &key) {
        compare_matched(
            read,
            FindingIdentity { scope, family, key },
            stmt,
            &entry,
            findings,
            standalones,
        );
        return;
    }
    // 2. A raw counterpart (raw-vs-raw by body, or the raw-vs-
    //    structured cross — drift with the route named).
    if let Some(entry) = take_raw_counterpart(scope_entries, &family, &key) {
        compare_matched(
            read,
            FindingIdentity { scope, family, key },
            stmt,
            &entry,
            findings,
            standalones,
        );
        return;
    }
    // 3. An api statement whose loose route key matches (the catalog
    //    re-prints its parsed path, so the escape flip and slash
    //    collapse move the printed key — pairing is by segment text,
    //    comparison by the typed route).
    if let Stmt::Api(api) = stmt {
        let route = route_key(&api.path);
        let name = scope_entries
            .family(Family::Api)
            .find(|entry| route_key(&entry.name) == route)
            .map(|entry| entry.name.clone());
        if let Some(name) = name {
            let entry = scope_entries
                .take_named(Family::Api, &name)
                .expect("found above");
            compare_matched(
                read,
                FindingIdentity {
                    scope,
                    family: Family::Api,
                    key: api.path.clone(),
                },
                stmt,
                &entry,
                findings,
                standalones,
            );
            return;
        }
    }
    classify_unmatched(read, scope, stmt, findings);
}

/// The raw-routed entry whose DEFINE header names this family + key.
/// Raw entries all ride the `Family::Raw` bucket (the readers file
/// them by their own `classify`); the header decides the family.
fn take_raw_counterpart(
    entries: &mut super::normalize::ScopeEntries,
    family: &Family,
    key: &str,
) -> Option<ReadEntry> {
    let candidates: Vec<usize> = entries
        .family(Family::Raw)
        .enumerate()
        .filter(|(_, entry)| {
            entry.name == key
                && matches!(
                    &entry.stmt,
                    Stmt::Raw(raw) if raw_match_key(&raw.body).is_some_and(|(f, _)| f == *family)
                )
        })
        .map(|(at, _)| at)
        .collect();
    let at = *candidates.first()?;
    entries.take_at(Family::Raw, at)
}

/// An unmatched SDL statement: standalone field/index/event compare
/// against their table's nested twin (converged standalones were
/// already folded by placement, so anything divergent surfaces here);
/// everything else is the ordinary SDL-only class.
fn classify_unmatched(
    read: &ReadModel,
    scope: ScopeKey,
    stmt: &Stmt,
    findings: &mut Vec<DriftFinding>,
) {
    let (family, key) = finding_key(stmt);
    // An all-unset `config default` stores nothing in the engine (the
    // `none` sentinel folds to absence in `resolve`), so it never has
    // a read counterpart — the converged-with-absence shape, not an
    // apply-pending definition.
    if let Stmt::Config(crate::ast::Config::Default(default)) = stmt
        && side_unset(&default.namespace)
        && side_unset(&default.database)
    {
        return;
    }
    let twin = match stmt {
        Stmt::Field(field) => nested_twin(read, &scope, &field.on.raw, family, &key),
        Stmt::Index(index) => nested_twin(read, &scope, &index.on.raw, family, &key),
        Stmt::Event(event) => nested_twin(read, &scope, &event.on.raw, family, &key),
        _ => None,
    };
    let class = match twin {
        Some(twin) => {
            let database = resolve(twin.clone());
            let sdl = resolve(stmt.clone());
            if unwrapped_eq(&sdl, &database) {
                // The converged standalone: placement's twin fold
                // removes it on retro-apply — not a finding.
                return;
            }
            DriftClass::Drift {
                sdl: Box::new(sdl),
                database: Box::new(database),
                markers: Box::default(),
            }
        }
        None => DriftClass::SdlOnly {
            sdl: Box::new(resolve(stmt.clone())),
        },
    };
    findings.push(DriftFinding {
        on_table_exists: on_table_exists(read, &scope, stmt),
        scope,
        family,
        key,
        class,
    });
}

/// A scope's entries in the model — the immutable lookup both
/// `nested_twin` and [`read_holds_table`] share.
fn scope_entries<'a>(
    read: &'a ReadModel,
    scope: &ScopeKey,
) -> Option<&'a super::normalize::ScopeEntries> {
    match scope {
        ScopeKey::Root => Some(&read.root),
        ScopeKey::Namespace(ns) => read
            .namespaces
            .iter()
            .find(|(name, _)| name == ns)
            .map(|(_, entries)| entries),
        ScopeKey::Database(ns, db) => read
            .databases
            .iter()
            .find(|(sns, sdb, _)| sns == ns && sdb == db)
            .map(|(_, _, entries)| entries),
    }
}

/// The read model's table `name` in `scope`, searched for a nested
/// child matching (family, key) — the twin an authored standalone
/// compares against. Searches the ORIGINAL model (the table may have
/// matched an SDL table already; its children still pair).
fn nested_twin<'a>(
    read: &'a ReadModel,
    scope: &ScopeKey,
    table_name: &str,
    family: Family,
    key: &str,
) -> Option<&'a Stmt> {
    let entries = scope_entries(read, scope)?;
    entries
        .family(Family::Table)
        .filter_map(|entry| match &entry.stmt {
            Stmt::Table(table) if table.name.raw == table_name => Some(table),
            _ => None,
        })
        .find_map(|table| {
            table
                .nested
                .iter()
                .find(|nested| classify(nested) == (family, key.to_string()))
        })
}

fn scope_entries_mut<'a>(
    read: &'a mut ReadModel,
    scope: &ScopeKey,
) -> Option<&'a mut super::normalize::ScopeEntries> {
    match scope {
        ScopeKey::Root => Some(&mut read.root),
        ScopeKey::Namespace(ns) => read
            .namespaces
            .iter_mut()
            .find(|(name, _)| name == ns)
            .map(|(_, entries)| entries),
        ScopeKey::Database(ns, db) => read
            .databases
            .iter_mut()
            .find(|(sns, sdb, _)| sns == ns && sdb == db)
            .map(|(_, _, entries)| entries),
    }
}

// ---------------------------------------------------------------------
// Comparing: resolve, then the per-family contract
// ---------------------------------------------------------------------

/// A finding's (scope, family, key) identity — the triple every
/// class rides, threaded through the compare arms as one value.
struct FindingIdentity {
    scope: ScopeKey,
    family: Family,
    key: String,
}

/// One matched pair: resolve both sides, compare, and file the
/// finding (a converged pair emits nothing). Tables compare their own
/// clauses at the table key and their nested children individually.
fn compare_matched(
    read: &ReadModel,
    identity: FindingIdentity,
    sdl: &Stmt,
    entry: &ReadEntry,
    findings: &mut Vec<DriftFinding>,
    standalones: &[SdlStandalone<'_>],
) {
    let database = resolve(entry.stmt.clone());
    let sdl = resolve(sdl.clone());
    // The tables' nested children pair individually (a child delta is
    // its own finding, and an SDL standalone's twin lives there).
    if let (Stmt::Table(sdl_table), Stmt::Table(db_table)) = (&sdl, &database) {
        if !table_clauses_equal(sdl_table, db_table) {
            findings.push(DriftFinding {
                on_table_exists: false,
                scope: identity.scope.clone(),
                family: identity.family,
                key: identity.key.clone(),
                class: DriftClass::Drift {
                    sdl: Box::new(sdl.clone()),
                    database: Box::new(database.clone()),
                    markers: Box::new(entry.markers.clone()),
                },
            });
        }
        compare_children(identity.scope, sdl_table, db_table, findings, standalones);
        return;
    }
    if unwrapped_eq(&sdl, &database) {
        return;
    }
    findings.push(DriftFinding {
        on_table_exists: on_table_exists(read, &identity.scope, &sdl),
        scope: identity.scope,
        family: identity.family,
        key: identity.key,
        class: DriftClass::Drift {
            sdl: Box::new(sdl),
            database: Box::new(database),
            markers: Box::new(entry.markers.clone()),
        },
    });
}

/// The nested children of one matched table pair: matched children
/// compare at their own key; unmatched ones are the ordinary
/// SDL-only / database-only classes (a database-only child carries
/// the host table's ON edge — a `REMOVE FIELD … ON t` must run while
/// `t` exists). Both arms pin `on_table_exists` TRUE — this walk
/// only runs for a table that matched, so the host stands
/// database-side by construction. The placement mirror (issue #51):
/// a leftover database child an SDL STANDALONE authored against this
/// host also carries is DEFERRED to the SDL side's own twin path
/// (fold or Drift there) — the SDL manages the child, so
/// database-only would be the phantom.
fn compare_children(
    scope: ScopeKey,
    sdl_table: &Table,
    db_table: &Table,
    findings: &mut Vec<DriftFinding>,
    standalones: &[SdlStandalone<'_>],
) {
    let mut db_children: Vec<&Stmt> = db_table.nested.iter().collect();
    for sdl_child in &sdl_table.nested {
        let (family, key) = classify(sdl_child);
        let at = db_children
            .iter()
            .position(|child| classify(child) == (family, key.clone()));
        let resolved_sdl = resolve(sdl_child.clone());
        let class = match at {
            Some(at) => {
                let db_child = resolve(db_children.remove(at).clone());
                match unwrapped_eq(&resolved_sdl, &db_child) {
                    true => continue,
                    false => DriftClass::Drift {
                        sdl: Box::new(resolved_sdl),
                        database: Box::new(db_child),
                        markers: Box::default(),
                    },
                }
            }
            None => DriftClass::SdlOnly {
                sdl: Box::new(resolved_sdl),
            },
        };
        findings.push(DriftFinding {
            on_table_exists: true,
            scope: scope.clone(),
            family,
            key,
            class,
        });
    }
    for child in db_children {
        let (family, key) = finding_key(child);
        if standalones.iter().any(|s| {
            s.scope == scope && s.host == sdl_table.name.raw && s.family == family && s.key == key
        }) {
            // The placement mirror (issue #51): the SDL authors this
            // child as a standalone against this host — its own walk
            // folds it (converged) or files the Drift (diverged);
            // database-only here would be the phantom.
            continue;
        }
        findings.push(DriftFinding {
            on_table_exists: true,
            scope: scope.clone(),
            family,
            key,
            class: DriftClass::DatabaseOnly {
                on_table: Some(sdl_table.name.raw.clone()),
                database: Box::new(resolve(child.clone())),
                markers: Box::default(),
            },
        });
    }
}

/// Resolve one side to minimal form: the merge's own elide + fold
/// layer, kind texts canonicalized through the merge's kind algebra
/// (one mechanism — structural equality then compares kinds
/// algebra-equal, object keys class- and order-blind), the
/// representation-equivalence folds, the span zero
/// (`Module::from_span`), and the comment-field
/// strip (comments are layout, never definition — an SDL-side note
/// never drifts against its comment-free read twin).
pub(super) fn resolve(stmt: Stmt) -> Stmt {
    let mut stmt = elide_and_fold(stmt);
    canonicalize_kinds(&mut stmt);
    fold_equivalences(&mut stmt);
    crate::ast::strip_comment_fields(&mut stmt);
    if let Stmt::Module(module) = &mut stmt {
        module.from_span = Span::point(0);
    }
    stmt
}

/// Kind texts rewrite to their algebra-normalized canonical spelling
/// (field kinds everywhere, function args and returns).
fn canonicalize_kinds(stmt: &mut Stmt) {
    match stmt {
        Stmt::Field(field) => canon_field_kinds(field),
        Stmt::Function(function) => {
            for (_, kind) in &mut function.args {
                canon_kind(kind);
            }
            if let Some(returns) = &mut function.returns {
                canon_kind(returns);
            }
        }
        Stmt::Table(table) => {
            for nested in &mut table.nested {
                canonicalize_kinds(nested);
            }
        }
        _ => {}
    }
}

fn canon_field_kinds(field: &mut Field) {
    if let Some(kind) = &mut field.kind {
        canon_kind(kind);
    }
}

fn canon_kind(kind: &mut crate::ast::Kind) {
    if let Some(ast) = parse_kind_ast(&kind.text) {
        kind.text = write_kind(&normalize_kind(&ast));
    }
}

/// The representation-equivalence folds: both sides resolve through
/// them, so an authored spelling of an INFO-unrepresentable default
/// converges against the read's absence.
fn fold_equivalences(stmt: &mut Stmt) {
    match stmt {
        // The authored `any { permissions: full; }` ≡ the
        // absent-`any` fold — the printer's always-materialized
        // `FOR any PERMISSIONS FULL` is absence, not an authored
        // block (the reader folds it; this is the authored side's
        // symmetric arm). The same default elision rides every
        // METHOD GROUP's permissions: the printer always
        // materializes a group's `PERMISSIONS FULL` and the reader
        // elides it (`elide_default_permission`) — the authored
        // `full` is the spelling of absence (the dogfood tour's
        // catch; the fold rode only the `any` block before).
        Stmt::Api(api) => {
            api.any = api.any.take().filter(not_the_absent_any);
            for action in &mut api.actions {
                action.config.permissions = action
                    .config
                    .permissions
                    .take()
                    .filter(|permission| *permission != Permission::Full);
            }
        }
        // The read's symmetric issuer mirror (an issuer
        // whose key is absent and whose algorithm is absent or
        // forced-equal to a symmetric verify key) ≡ the authored
        // absent issuer.
        Stmt::Access(access) => fold_issuer_mirrors(access),
        // The authored `none` side of `config default` ≡ the unset
        // side: INFO FOR ROOT's `defaults` omits unset sides
        // entirely, so an all-`none` statement has no read
        // counterpart by construction (the converged-with-absence
        // early return in `classify_unmatched` completes the pair —
        // the full-product oracle's catch).
        Stmt::Config(crate::ast::Config::Default(default)) => {
            if side_unset(&default.namespace) {
                default.namespace = None;
            }
            if side_unset(&default.database) {
                default.database = None;
            }
        }
        _ => {}
    }
}

/// A `config default` side that holds no default — absent, or the
/// `none` sentinel (the SDL spelling of "no default on this side";
/// the embedded printer's canon is `NONE`).
fn side_unset(side: &Option<crate::ast::Expr>) -> bool {
    side.as_ref()
        .is_none_or(|expr| expr.canon.eq_ignore_ascii_case("none"))
}

/// The absent-`any` shape: no middleware, no fallback, and no
/// SDL-meaningful permission (FULL is the elided default; `none` or
/// an expression is authored denial and stays).
fn not_the_absent_any(block: &ApiAnyBlock) -> bool {
    !block.config.middleware.is_empty()
        || block.fallback.is_some()
        || block
            .config
            .permissions
            .as_ref()
            .is_some_and(|permission| *permission != Permission::Full)
}

fn fold_issuer_mirrors(access: &mut Access) {
    let mirror = match &access.kind {
        AccessKind::Jwt { source } => issuer_mirror_of(source),
        AccessKind::Record {
            jwt: Some(source), ..
        } => issuer_mirror_of(source),
        AccessKind::Bearer {
            jwt: Some(source), ..
        } => issuer_mirror_of(source),
        _ => None,
    };
    let Some(algorithm) = mirror else {
        return;
    };
    let source = match &mut access.kind {
        AccessKind::Jwt { source } => source,
        AccessKind::Record {
            jwt: Some(source), ..
        } => source,
        AccessKind::Bearer {
            jwt: Some(source), ..
        } => source,
        _ => return,
    };
    if source.issuer.as_ref().is_some_and(|issuer| {
        issuer.key.is_none() && issuer.algorithm.is_none_or(|a| a == algorithm)
    }) {
        source.issuer = None;
    }
}

/// The symmetric verify key whose issuer would mirror — `None` when
/// the verification source is not a symmetric local key.
fn issuer_mirror_of(source: &JwtSource) -> Option<crate::ast::Algorithm> {
    match &source.verify {
        JwtVerify::Key { algorithm, .. } if algorithm.is_symmetric() => Some(*algorithm),
        _ => None,
    }
}

/// The comparison entry: structural equality over the resolved forms,
/// with the unwrap normalization applied symmetrically first and the
/// per-family non-comparability arms on top.
fn unwrapped_eq(sdl: &Stmt, database: &Stmt) -> bool {
    match (unwrapped(sdl.clone()), unwrapped(database.clone())) {
        (Stmt::User(mut a), Stmt::User(mut b)) => {
            // Secrets skip outright (spec/user.md: presence is the
            // only fact — INFO redacts them, the read never carries
            // one; PASSSCRAM is stripped at read outright).
            a.pass = None;
            b.pass = None;
            a.scram = None;
            b.scram = None;
            a == b
        }
        (Stmt::Access(a), Stmt::Access(b)) => access_eq(&a, &b),
        (Stmt::Table(a), Stmt::Table(b)) => table_clauses_equal(&a, &b),
        // `database strict;` and index `concurrently;` never render in
        // INFO (server-side blindness / a transient build directive) —
        // the SDL side's flag never drifts (the merge preserves it).
        (Stmt::Database(mut a), Stmt::Database(mut b)) => {
            a.strict = false;
            b.strict = false;
            a == b
        }
        (Stmt::Index(mut a), Stmt::Index(mut b)) => {
            a.concurrently = false;
            b.concurrently = false;
            a == b
        }
        (Stmt::Field(mut a), Stmt::Field(mut b)) => {
            permissions_eq(a.permissions.as_ref(), b.permissions.as_ref(), true) && {
                a.permissions = None;
                b.permissions = None;
                a == b
            }
        }
        (Stmt::Api(mut a), Stmt::Api(mut b)) => {
            // Paths compare by RE-PARSE (the path-flip
            // home): the catalog re-prints its parsed path, so the
            // segment-initial escape flip and slash collapse are
            // exactly what text comparison cannot see.
            route_segments(&a.path) == route_segments(&b.path) && {
                a.path = String::new();
                b.path = String::new();
                a == b
            }
        }
        (Stmt::Raw(a), Stmt::Raw(b)) => a.body.trim() == b.body.trim(),
        (a, b) => a == b,
    }
}

/// Structural equality with the unwrap normalization applied to every
/// expression's canonical text.
fn unwrapped(stmt: Stmt) -> Stmt {
    let mut stmt = stmt;
    unwrap_statement_exprs(&mut stmt);
    stmt
}

/// The state-equality verdict the pipeline shares: both sides
/// resolved to minimal form ([`resolve`]) and compared under
/// [`unwrapped_eq`] — the differ's own convergence notion, consumed
/// by the spread-preserving merge (a spread member the database
/// state-equals is covered by its spread; anything else is drift).
/// ONE notion on both sides of the pipeline is load-bearing: a
/// member the merge drops as covered must be exactly one the differ
/// reports clean on the lowered file.
pub(super) fn state_equal(sdl: &Stmt, database: &Stmt) -> bool {
    unwrapped_eq(&resolve(sdl.clone()), &resolve(database.clone()))
}

fn access_eq(sdl: &Access, database: &Access) -> bool {
    sdl.on == database.on
        && sdl.authenticate == database.authenticate
        && sdl.context == database.context
        && sdl.comment == database.comment
        && duration_eq(&sdl.duration, &database.duration)
        && access_kind_eq(&sdl.kind, &database.kind)
}

/// The access `duration` sides: the read's unset side compares as
/// converged against any authored side (the access's own capability
/// silently drops the side — spec/access.md; the linter flags the
/// authored side). A side the read DOES carry must match.
fn duration_eq(sdl: &Option<AccessDuration>, database: &Option<AccessDuration>) -> bool {
    let Some(database) = database else {
        return true;
    };
    let Some(sdl) = sdl else {
        return false;
    };
    let side = |sdl: &Option<crate::ast::Expr>, database: &Option<crate::ast::Expr>| match database
    {
        None => true,
        Some(value) => sdl.as_ref() == Some(value),
    };
    side(&sdl.grant, &database.grant)
        && side(&sdl.token, &database.token)
        && side(&sdl.session, &database.session)
}

fn access_kind_eq(sdl: &AccessKind, database: &AccessKind) -> bool {
    match (sdl, database) {
        (AccessKind::Jwt { source: a }, AccessKind::Jwt { source: b }) => jwt_source_eq(a, b),
        (
            AccessKind::Record {
                signup,
                signin,
                jwt,
                refresh,
            },
            AccessKind::Record {
                signup: db_signup,
                signin: db_signin,
                jwt: db_jwt,
                refresh: db_refresh,
            },
        ) => {
            signup == db_signup
                && signin == db_signin
                && refresh == db_refresh
                && jwt_opt_eq(jwt, db_jwt)
        }
        // A bearer's `jwt { … }` is stored but never rendered — the
        // SDL's whole sub-block never drifts (spec/access.md).
        (
            AccessKind::Bearer { subject, .. },
            AccessKind::Bearer {
                subject: db_subject,
                ..
            },
        ) => subject == db_subject,
        _ => false,
    }
}

/// The JWT sources: verification sources compare structurally (the
/// sentinel keys compare textually equal by the shape-only rule —
/// both sides spell `'[REDACTED]'`), the issuer's ALGORITHM never
/// renders so only its key compares, and the mirror fold already
/// reduced both sides' mirrored issuers to absence.
fn jwt_source_eq(a: &JwtSource, b: &JwtSource) -> bool {
    let verify = match (&a.verify, &b.verify) {
        (JwtVerify::Url(x), JwtVerify::Url(y)) => x == y,
        (
            JwtVerify::Key {
                algorithm: alg_a,
                key: key_a,
            },
            JwtVerify::Key {
                algorithm: alg_b,
                key: key_b,
            },
        ) => alg_a == alg_b && key_a == key_b,
        _ => return false,
    };
    verify && a.audience == b.audience && issuer_key_eq(&a.issuer, &b.issuer)
}

fn issuer_key_eq(a: &Option<JwtIssuer>, b: &Option<JwtIssuer>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.key == b.key,
        _ => false,
    }
}

fn jwt_opt_eq(a: &Option<JwtSource>, b: &Option<JwtSource>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => jwt_source_eq(a, b),
        // A jwt-less record's auto-issued signature: the engine
        // materializes a random HS512 key whose INFO print is the
        // sentinel pair (the reader folds the issuer mirror, resolve
        // folds it to absence). The signature is absence's own
        // materialization — the indistinguishability corner —
        // so it converges against the authored jwt-less record.
        (None, Some(source)) => is_the_auto_issue_signature(source),
        _ => false,
    }
}

/// The auto-issue signature: a symmetric sentinel verify key with no
/// issuer left. An authored asymmetric or URL issuer on the database
/// side is a real definition and still drifts against absence.
fn is_the_auto_issue_signature(source: &JwtSource) -> bool {
    source.issuer.is_none()
        && source.audience.is_none()
        && matches!(
            &source.verify,
            JwtVerify::Key { algorithm, key }
                if algorithm.is_symmetric() && key.canon == crate::parser::SECRET_SENTINEL_CANON
        )
}

/// The table's own clauses (children pair separately). The schema
/// flag's absence is `SCHEMALESS` (an authored `None` and the read's
/// `Some(false)` are one definition — the differ owns the
/// default-equivalence arm the medium readers' disposition deferred),
/// and the
/// permissions clause compares through the per-op projection.
fn table_clauses_equal(a: &Table, b: &Table) -> bool {
    permissions_eq(a.permissions.as_ref(), b.permissions.as_ref(), false)
        && Table {
            permissions: None,
            schema_full: None,
            nested: Vec::new(),
            ..a.clone()
        } == Table {
            permissions: None,
            schema_full: None,
            nested: Vec::new(),
            ..b.clone()
        }
        && a.schema_full.unwrap_or(false) == b.schema_full.unwrap_or(false)
}

/// The per-op projection both permission clauses compare through.
/// `field` seeds unlisted operations at FULL (the parser's own seed;
/// tables seed NONE). Entries at the statement's elide-default value
/// drop (they are the default by either spelling), the delete slot
/// drops unless it carries a real expression (INFO cannot report
/// delete-full — the projection is one-directional there), and the
/// last group mentioning an operation wins (SurrealQL's own fold).
fn permissions_eq(a: Option<&OpPermissions>, b: Option<&OpPermissions>, field: bool) -> bool {
    let domain: &[PermissionOp] = match field {
        true => &[
            PermissionOp::Select,
            PermissionOp::Create,
            PermissionOp::Update,
        ],
        false => &[
            PermissionOp::Select,
            PermissionOp::Create,
            PermissionOp::Update,
            PermissionOp::Delete,
        ],
    };
    let elide_default = match field {
        true => Permission::Full,
        false => Permission::None,
    };
    project_permissions(a, domain, &elide_default) == project_permissions(b, domain, &elide_default)
}

fn project_permissions(
    permissions: Option<&OpPermissions>,
    domain: &[PermissionOp],
    elide_default: &Permission,
) -> Vec<(PermissionOp, Permission)> {
    domain
        .iter()
        .filter_map(|op| {
            let value = match permissions {
                None | Some(OpPermissions::None) => Permission::None,
                Some(OpPermissions::Full) => Permission::Full,
                Some(OpPermissions::Groups(groups)) => {
                    // The last group mentioning the operation wins;
                    // unlisted operations ride the statement's seed
                    // (tables seed NONE, fields FULL — the value that
                    // elides IS the seed).
                    match groups.iter().rev().find(|group| group.ops.contains(op)) {
                        Some(group) => group.value.clone(),
                        None => elide_default.clone(),
                    }
                }
            };
            // A real expression always compares; every other value at
            // the elide-default spelling is the default (dropped), and
            // the delete slot never carries a comparable non-expression
            // (INFO cannot report delete-full or merge delete-none).
            let comparable = match &value {
                Permission::Expr(_) => true,
                other => *other != *elide_default && *op != PermissionOp::Delete,
            };
            comparable.then_some((*op, value))
        })
        .collect()
}

/// An api route path's comparison key: its segments, classified
/// (static / `:param` / `*wildcard`), with the segment-initial
/// `\:` escape decoded (the storable position — the escape flip is
/// exactly what this comparison sees) and empty segments dropped
/// (the slash collapse).
#[derive(Debug, Clone, PartialEq, Eq)]
enum RouteSeg {
    Static(String),
    Param(String),
    Wildcard(String),
}

fn route_segments(path: &str) -> Vec<RouteSeg> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            if let Some(escaped) = segment.strip_prefix("\\:") {
                RouteSeg::Static(format!(":{escaped}"))
            } else if let Some(name) = segment.strip_prefix('*') {
                RouteSeg::Wildcard(name.to_string())
            } else if let Some(name) = segment.strip_prefix(':') {
                RouteSeg::Param(name.to_string())
            } else {
                RouteSeg::Static(segment.to_string())
            }
        })
        .collect()
}

/// The api match key's loose form: the segments' raw texts, escape
/// decoded — a static-escaped `\:x` and a printed `:x` param share
/// the loose key (the flip must PAIR to report its drift), while the
/// slash collapse and the typed classification stay in
/// [`route_segments`]'s hands.
fn route_key(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.strip_prefix('\\').unwrap_or(segment).to_string())
        .collect()
}

// ---------------------------------------------------------------------
// The block-unwrap (expressions compare modulo a braced single
// statement)
// ---------------------------------------------------------------------

/// Rewrites every expression clause's canonical text with its
/// single-statement block unwrapped (recursively): `signin: { select
/// …; }` compares equal to `signin: select …;` (probe-pinned: the
/// printer keeps the braced form braced and parenthesizes the bare
/// form, so the two authored spellings are distinct texts of one
/// definition). Function bodies ride the same visitor — the canon and
/// the engine print agree byte-for-byte, so the unwrap is inert there
/// and multi-statement bodies keep their braces.
fn unwrap_statement_exprs(stmt: &mut Stmt) {
    visit_stmt_exprs(stmt, &mut |expr| {
        expr.canon = unwrap_expr_text(&expr.canon);
    });
}

/// The expression text's normalized form: single-statement block
/// braces and balanced outer parens unwrap to fixpoint (both are
/// grouping-only spellings — the probe pinned the printer's shapes:
/// a braced single statement stays braced, the bare form prints
/// parenthesized, so the two authored spellings of one clause hold
/// `(X)` and `{ X }`).
fn unwrap_expr_text(text: &str) -> String {
    let mut current = text.to_string();
    while let Some(next) =
        unwrap_single_statement_block(&current).or_else(|| unwrap_outer_parens(&current))
    {
        current = next;
    }
    current
}

/// `(X)` → `X` when the parens balance and nothing trails them.
fn unwrap_outer_parens(text: &str) -> Option<String> {
    let mut outer = Scanner::new(text);
    outer.skip_surrealql_trivia();
    if outer.peek_byte() != Some(b'(') {
        return None;
    }
    let span = outer.capture_balanced_parens().ok()?;
    outer.skip_surrealql_trivia();
    if !outer.is_at_end() {
        return None;
    }
    // The balanced capture's span is the interior (the opener and
    // closer were consumed).
    Some(span.slice(text).trim().to_string())
}

/// `{ X }` → `X` when the braces balance, the interior holds exactly
/// one depth-0 statement, and nothing trails the block. Anything else
/// stays as written.
fn unwrap_single_statement_block(text: &str) -> Option<String> {
    let mut outer = Scanner::new(text);
    outer.skip_surrealql_trivia();
    if outer.peek_byte() != Some(b'{') {
        return None;
    }
    let span = outer.capture_balanced_braces().ok()?;
    outer.skip_surrealql_trivia();
    if !outer.is_at_end() {
        return None;
    }
    // The balanced capture's span is the interior (the braces were
    // consumed).
    let interior = span.slice(text);
    let mut inner = Scanner::new(interior);
    let mut statement: Option<&str> = None;
    loop {
        inner.skip_surrealql_trivia();
        if inner.is_at_end() {
            break;
        }
        if statement.is_some() {
            return None; // a second statement — the braces are meaningful
        }
        // Depth-0 `;` or end of source ends the statement (the
        // printer's single-statement canon drops the inner `;`);
        // a depth-0 closer is not a statement end — no unwrap.
        let (segment, _) = inner.capture_until_word(&[]).ok()?;
        statement = Some(segment.slice(interior));
    }
    Some(statement?.trim().to_string())
}

/// Visits every expression clause of one statement (the fixed AST —
/// each site listed once).
fn visit_stmt_exprs(stmt: &mut Stmt, f: &mut impl FnMut(&mut crate::ast::Expr)) {
    match stmt {
        Stmt::Namespace(_)
        | Stmt::Analyzer(_)
        | Stmt::Raw(_)
        | Stmt::Include(_)
        | Stmt::TypeAlias(_) => {}
        Stmt::Module(module) => visit_permission(&mut module.permissions, f),
        Stmt::Sequence(sequence) => {
            visit_option(&mut sequence.start, f);
            visit_option(&mut sequence.batch, f);
            visit_option(&mut sequence.timeout, f);
        }
        Stmt::Param(param) => {
            visit_option(&mut param.value, f);
            visit_permission(&mut param.permissions, f);
        }
        Stmt::Database(_) => {}
        Stmt::Bucket(bucket) => {
            visit_option(&mut bucket.backend, f);
            visit_permission(&mut bucket.permissions, f);
        }
        Stmt::User(user) => {
            visit_option(&mut user.token_duration, f);
            visit_option(&mut user.session_duration, f);
        }
        Stmt::Event(event) => {
            visit_option(&mut event.when, f);
            for then in &mut event.then {
                f(then);
            }
        }
        Stmt::Function(function) => visit_function_exprs(function, f),
        Stmt::Config(crate::ast::Config::Api(api)) => {
            visit_permission(&mut api.permissions, f);
            for middleware in &mut api.middleware {
                for arg in &mut middleware.args {
                    f(arg);
                }
            }
        }
        Stmt::Config(crate::ast::Config::Graphql(_)) => {}
        Stmt::Config(crate::ast::Config::Default(default)) => {
            visit_option(&mut default.namespace, f);
            visit_option(&mut default.database, f);
        }
        Stmt::Index(index) => visit_index_exprs(index, f),
        Stmt::Api(api) => visit_api_exprs(api, f),
        Stmt::Access(access) => visit_access_exprs(access, f),
        Stmt::Table(table) => {
            if let TableKind::View { query } = &mut table.kind {
                f(query);
            }
            visit_op_permissions(&mut table.permissions, f);
            for nested in &mut table.nested {
                visit_stmt_exprs(nested, f);
            }
        }
        Stmt::Field(field) => visit_field_exprs(field, f),
        // The differ expands the SDL side itself (`diff`'s first
        // act), so neither reaches an expression walk; the arms stay
        // total (and correct) for any caller — a mixin's members
        // carry the expression clauses.
        Stmt::Mixin(mixin) => {
            for member in &mut mixin.members {
                visit_stmt_exprs(member, f);
            }
        }
        Stmt::Spread(_) => {}
    }
}

fn visit_function_exprs(function: &mut Function, f: &mut impl FnMut(&mut crate::ast::Expr)) {
    f(&mut function.body);
    visit_permission(&mut function.permissions, f);
}

fn visit_index_exprs(index: &mut Index, f: &mut impl FnMut(&mut crate::ast::Expr)) {
    if let IndexKind::Count {
        where_: Some(where_),
    } = &mut index.kind
    {
        f(where_);
    }
    for field in &mut index.fields {
        f(field);
    }
}

fn visit_api_exprs(api: &mut Api, f: &mut impl FnMut(&mut crate::ast::Expr)) {
    for action in &mut api.actions {
        visit_permission(&mut action.config.permissions, f);
        for middleware in &mut action.config.middleware {
            for arg in &mut middleware.args {
                f(arg);
            }
        }
        f(&mut action.action);
    }
    if let Some(any) = &mut api.any {
        visit_permission(&mut any.config.permissions, f);
        for middleware in &mut any.config.middleware {
            for arg in &mut middleware.args {
                f(arg);
            }
        }
        visit_option(&mut any.fallback, f);
    }
}

fn visit_access_exprs(access: &mut Access, f: &mut impl FnMut(&mut crate::ast::Expr)) {
    visit_option(&mut access.authenticate, f);
    if let Some(duration) = &mut access.duration {
        visit_option(&mut duration.grant, f);
        visit_option(&mut duration.token, f);
        visit_option(&mut duration.session, f);
    }
    let source = match &mut access.kind {
        AccessKind::Jwt { source } => Some(source),
        AccessKind::Record {
            signup,
            signin,
            jwt,
            ..
        } => {
            visit_option(signup, f);
            visit_option(signin, f);
            jwt.as_mut()
        }
        AccessKind::Bearer { jwt, .. } => jwt.as_mut(),
    };
    if let Some(source) = source {
        visit_jwt_source(source, f);
    }
}

fn visit_jwt_source(source: &mut JwtSource, f: &mut impl FnMut(&mut crate::ast::Expr)) {
    match &mut source.verify {
        JwtVerify::Url(url) => f(url),
        JwtVerify::Key { key, .. } => f(key),
    }
    if let Some(issuer) = &mut source.issuer {
        visit_option(&mut issuer.key, f);
    }
}

fn visit_field_exprs(field: &mut Field, f: &mut impl FnMut(&mut crate::ast::Expr)) {
    match &mut field.default {
        Some(FieldDefault::Set(expr)) | Some(FieldDefault::Always(expr)) => f(expr),
        None => {}
    }
    visit_option(&mut field.value, f);
    visit_option(&mut field.assert, f);
    visit_option(&mut field.computed, f);
    if let Some(OnDelete::Then(expr)) = &mut field.reference {
        f(expr);
    }
    visit_op_permissions(&mut field.permissions, f);
}

fn visit_permission(
    permission: &mut Option<Permission>,
    f: &mut impl FnMut(&mut crate::ast::Expr),
) {
    if let Some(Permission::Expr(expr)) = permission {
        f(expr);
    }
}

fn visit_op_permissions(
    permissions: &mut Option<OpPermissions>,
    f: &mut impl FnMut(&mut crate::ast::Expr),
) {
    if let Some(OpPermissions::Groups(groups)) = permissions {
        for group in groups {
            visit_group(group, f);
        }
    }
}

fn visit_group(group: &mut PermissionGroup, f: &mut impl FnMut(&mut crate::ast::Expr)) {
    if let Permission::Expr(expr) = &mut group.value {
        f(expr);
    }
}

fn visit_option<T>(option: &mut Option<T>, f: &mut impl FnMut(&mut T)) {
    if let Some(value) = option {
        f(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Namespace;
    use crate::info::normalize::ScopeEntries;

    fn sdl_stmt(src: &str) -> Stmt {
        let (file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{src}: {diags:?}");
        assert_eq!(file.items.len(), 1, "{src}");
        file.items.into_iter().next().expect("checked above")
    }

    fn entry(name: &str, src: &str) -> ReadEntry {
        ReadEntry {
            name: name.to_string(),
            stmt: sdl_stmt(src),
            markers: Markers::default(),
        }
    }

    /// Diffs a one-statement SDL file against a ROOT-scoped entry.
    fn root_diff(sdl: &str, read: ReadEntry) -> Vec<DriftFinding> {
        let (file, diags) = crate::parser::parse_file(sdl);
        assert!(diags.is_empty(), "{sdl}: {diags:?}");
        let mut model = ReadModel::default();
        model.mark_ingested(ScopeKey::Root);
        model.root.push(read);
        diff(&file, &model)
    }

    fn class_of<'a>(
        findings: &'a [DriftFinding],
        family: Family,
        key: &str,
    ) -> Option<&'a DriftClass> {
        findings
            .iter()
            .find(|finding| finding.family == family && finding.key == key)
            .map(|finding| &finding.class)
    }

    fn assert_converged(sdl: &str, read: ReadEntry, family: Family, key: &str) {
        let findings = root_diff(sdl, read);
        assert!(
            findings
                .iter()
                .all(|finding| finding.family != family || finding.key != key),
            "expected `{key}` converged, found {findings:?}"
        );
    }

    fn assert_drift(sdl: &str, read: ReadEntry, family: Family, key: &str) {
        let findings = root_diff(sdl, read);
        assert!(
            matches!(
                class_of(&findings, family, key),
                Some(DriftClass::Drift { .. })
            ),
            "expected `{key}` drifted, found {findings:?}"
        );
    }

    // ---------------------------------------------------------------
    // Equivalence arms
    // ---------------------------------------------------------------

    /// Kinds compare under one algebra: spelling variance and
    /// the object-key class/order (upstream's `BTreeMap`) never drift;
    /// a different kind does.
    #[test]
    fn kinds_compare_under_the_algebra() {
        assert_converged(
            "table t { field x: record < person >; };",
            entry("t", "table t { field x: record<person>; };"),
            Family::Table,
            "t",
        );
        assert_converged(
            "table t { field x: { a: int, b: string }; };",
            entry("t", "table t { field x: { 'b': string, a: int }; };"),
            Family::Table,
            "t",
        );
        assert_converged(
            "table t { field x: { a: int }; };",
            entry("t", "table t { field x: { `a`: int }; };"),
            Family::Table,
            "t",
        );
        assert_drift(
            "table t { field x: { a: int }; };",
            entry("t", "table t { field x: { a: string }; };"),
            Family::Field,
            "x",
        );
    }

    /// The single-statement block unwrap: braced and bare expression
    /// clauses are one definition (the probe pinned the printer's
    /// shapes — braced stays braced, bare prints parenthesized; both
    /// sides' canon texts unwrap to the same statement).
    #[test]
    fn the_block_unwrap_equates_braced_and_bare_clauses() {
        // jwt is root-legal; `authenticate` carries the expression.
        assert_converged(
            "access a { on: root; type: jwt; url: 'https://x'; authenticate: select * from user; };",
            entry(
                "a",
                "access a { on: root; type: jwt; url: 'https://x'; authenticate: { select * from user; }; };",
            ),
            Family::Access,
            "a",
        );
        assert_converged(
            "access a { on: root; type: jwt; url: 'https://x'; authenticate: { select * from user; }; };",
            entry(
                "a",
                "access a { on: root; type: jwt; url: 'https://x'; authenticate: select * from user; };",
            ),
            Family::Access,
            "a",
        );
        assert_drift(
            "access a { on: root; type: jwt; url: 'https://x'; authenticate: select * from user; };",
            entry(
                "a",
                "access a { on: root; type: jwt; url: 'https://x'; authenticate: select * from post; };",
            ),
            Family::Access,
            "a",
        );
        // The unwrap only takes a WHOLE wrapping layer: non-wrapping
        // parens and multi-statement blocks keep their shape.
        assert_eq!(unwrap_expr_text("(a) + (b)"), "(a) + (b)");
        assert_eq!(
            unwrap_expr_text("{ create user set x = 1; select * from user; }"),
            "{ create user set x = 1; select * from user; }"
        );
    }

    /// Authored spellings never participate (`Authored` value-only
    /// equality): `on: kv;` and the read's canonical `on: root` are
    /// one definition.
    #[test]
    fn authored_aliases_never_drift() {
        assert_converged(
            "user u { on: kv; };",
            entry("u", "user u { on: root; };"),
            Family::User,
            "u",
        );
    }

    /// The schema flag's absence is SCHEMALESS — the authored `None`
    /// and the read's `Some(false)` are one definition (the differ's
    /// default-equivalence arm).
    #[test]
    fn schema_flag_absence_is_schemaless() {
        assert_converged(
            "table t;",
            entry("t", "table t { schemaless; };"),
            Family::Table,
            "t",
        );
        assert_drift(
            "table t;",
            entry("t", "table t { schemafull; };"),
            Family::Table,
            "t",
        );
    }

    /// The authored `any { permissions: full; }` ≡ the
    /// absent-`any` fold — the printer's always-materialized
    /// `FOR any PERMISSIONS FULL` is absence, not an authored block.
    #[test]
    fn any_permissions_full_folds_to_absence() {
        assert_converged(
            "api '/x' { any { permissions: full; }; };",
            entry("/x", "api '/x';"),
            Family::Api,
            "/x",
        );
        assert_converged(
            "api '/x';",
            entry("/x", "api '/x' { any { permissions: full; }; };"),
            Family::Api,
            "/x",
        );
        // An authored denial (`none`) is SDL-meaningful — it stays and
        // drifts against absence.
        assert_drift(
            "api '/x' { any { permissions: none; }; };",
            entry("/x", "api '/x';"),
            Family::Api,
            "/x",
        );
    }

    /// The read's symmetric issuer mirror
    /// (`issuer: Some({algorithm: None, key: None})` — what the
    /// reader's own mirror fold leaves) ≡ the authored absent issuer.
    #[test]
    fn the_issuer_mirror_converges_with_absence() {
        let mirrored = match sdl_stmt(
            "access a { on: root; type: jwt; algorithm: hs512; key: '[REDACTED]'; issuer { key: '[REDACTED]'; }; };",
        ) {
            Stmt::Access(mut access) => {
                if let AccessKind::Jwt { source } = &mut access.kind
                    && let Some(issuer) = &mut source.issuer
                {
                    // The reader's own `fold_issuer_mirror` shape.
                    issuer.key = None;
                    issuer.algorithm = None;
                }
                Stmt::Access(access)
            }
            _ => unreachable!("an access statement"),
        };
        assert_converged(
            "access a { on: root; type: jwt; algorithm: hs512; key: '[REDACTED]'; };",
            ReadEntry {
                name: "a".to_string(),
                stmt: mirrored,
                markers: Markers::default(),
            },
            Family::Access,
            "a",
        );
    }

    /// The reference slot's `on delete: <expr>` rides the unwrap —
    /// braced and bare actions are one definition (the printer
    /// parenthesizes the bare form; the canon keeps braces).
    #[test]
    fn the_reference_then_action_unwraps() {
        assert_converged(
            "table t { field r: record<u> { reference { on delete: create log set at = time::now(); }; }; };",
            entry(
                "t",
                "table t { field r: record<u> { reference { on delete: (create log set at = time::now()); }; }; };",
            ),
            Family::Table,
            "t",
        );
    }

    /// The capability-dropped duration sides: the read's unset side
    /// converges against any authored side (the engine silently drops
    /// it); a side the read DOES carry must match.
    #[test]
    fn capability_dropped_duration_sides_never_drift() {
        let base = "access a { on: root; type: jwt; url: 'https://x'; duration { token: 2h; }; };";
        assert_converged(
            base,
            entry(
                "a",
                "access a { on: root; type: jwt; url: 'https://x'; duration { token: 2h; }; };",
            ),
            Family::Access,
            "a",
        );
        assert_converged(
            base,
            entry("a", "access a { on: root; type: jwt; url: 'https://x'; };"),
            Family::Access,
            "a",
        );
        assert_drift(
            "access a { on: root; type: jwt; url: 'https://x'; };",
            entry(
                "a",
                "access a { on: root; type: jwt; url: 'https://x'; duration { token: 2h; }; };",
            ),
            Family::Access,
            "a",
        );
    }

    /// User secrets skip outright — presence is the only fact; the
    /// rest of the user compares.
    #[test]
    fn user_secrets_skip_and_the_rest_compares() {
        assert_converged(
            "user u { on: root; password: 'hunter2'; roles: owner; };",
            entry("u", "user u { on: root; roles: owner; };"),
            Family::User,
            "u",
        );
        assert_drift(
            "user u { on: root; password: 'hunter2'; roles: owner; };",
            entry("u", "user u { on: root; };"),
            Family::User,
            "u",
        );
    }

    /// Table `delete: full;` is one-directional (INFO cannot report
    /// it), the engine's equal-value group merge never false-drifts
    /// (the live-probed shapes: mixed group prints
    /// `FOR select, create NONE, FOR update FULL`; all-full collapses
    /// to the word), and a real delta still drifts.
    #[test]
    fn table_delete_full_is_one_directional() {
        assert_converged(
            "table t { permissions { delete: full; }; };",
            entry("t", "table t;"),
            Family::Table,
            "t",
        );
        // Comments never count as drift (rule 5: layout, not
        // definition) — leading, clause-anchored, and group-anchored
        // notes on the SDL side converge against the comment-free
        // read twin.
        assert_converged(
            "table t {\n  // above drop\n  drop;\n  permissions { // g\n  select: full; };\n};",
            entry("t", "table t { drop; permissions { select: full; }; };"),
            Family::Table,
            "t",
        );
        // The probe's mixed-group shape: SDL `{ delete, update: full; }`
        // vs the read's merged print.
        assert_converged(
            "table t { permissions { delete, update: full; }; };",
            entry(
                "t",
                "table t { permissions { select, create: none; update: full; }; };",
            ),
            Family::Table,
            "t",
        );
        // All-full collapses to the word on both sides.
        assert_converged(
            "table t { permissions { select, create, update: full; delete: full; }; };",
            entry("t", "table t { permissions: full; };"),
            Family::Table,
            "t",
        );
        // A real delta still drifts.
        assert_drift(
            "table t;",
            entry("t", "table t { permissions { update: full; }; };"),
            Family::Table,
            "t",
        );
    }

    /// The INFO-absent preserves never drift: `database strict;`
    /// (active-strict blindness) and index `concurrently;` (a
    /// transient build directive).
    #[test]
    fn info_absent_preserves_never_drift() {
        // A `database` statement matches in its namespace's scope —
        // the namespace-free file binds to the empty-ns pseudo-scope.
        let (file, diags) = crate::parser::parse_file("database d { strict; };");
        assert!(diags.is_empty(), "{diags:?}");
        let mut model = ReadModel::default();
        model.mark_ingested(ScopeKey::Namespace(String::new()));
        let mut scope = ScopeEntries::default();
        scope.push(entry("d", "database d;"));
        model.namespaces.push((String::new(), scope));
        let findings = diff(&file, &model);
        assert!(
            findings.is_empty(),
            "expected strict to converge, found {findings:?}"
        );
        assert_converged(
            "index i { on: t; fields: v; concurrently; };",
            entry("i", "index i { on: t; fields: v; };"),
            Family::Index,
            "i",
        );
    }

    /// A bearer's `jwt { … }` is stored but never rendered — the
    /// SDL's whole sub-block never drifts.
    #[test]
    fn bearer_jwt_never_drifts() {
        assert_converged(
            "access a { on: root; type: bearer; for: user; jwt { url: 'https://x'; }; };",
            entry("a", "access a { on: root; type: bearer; for: user; };"),
            Family::Access,
            "a",
        );
    }

    /// The URL-issuer ALGORITHM never renders in INFO — the authored
    /// algorithm never drifts against the read's absence (only the
    /// key compares), and a real issuer-presence delta still drifts.
    #[test]
    fn the_url_issuer_algorithm_never_drifts() {
        assert_converged(
            "access a { on: root; type: jwt; url: 'https://x'; issuer { algorithm: es256; key: '[REDACTED]'; }; };",
            entry(
                "a",
                "access a { on: root; type: jwt; url: 'https://x'; issuer { key: '[REDACTED]'; }; };",
            ),
            Family::Access,
            "a",
        );
        assert_drift(
            "access a { on: root; type: jwt; url: 'https://x'; };",
            entry(
                "a",
                "access a { on: root; type: jwt; url: 'https://x'; issuer { key: '[REDACTED]'; }; };",
            ),
            Family::Access,
            "a",
        );
    }

    /// `module`'s permissions ride the unwrap too (modules diff at
    /// database scope; the exec corpus cannot carry the family — the
    /// embedded build rejects module DDL — so this test simulates
    /// the printer's braced shape on the read side).
    #[test]
    fn module_permissions_unwrap() {
        let braced = {
            let (file, diags) = crate::parser::parse_file(
                "module m { from: f'b:/k'; unsigned; permissions: (select * from x); };",
            );
            assert!(diags.is_empty(), "{diags:?}");
            match &file.items[0] {
                Stmt::Module(module) => {
                    let mut module = module.clone();
                    if let Some(Permission::Expr(expr)) = &mut module.permissions {
                        expr.canon = "{ SELECT * FROM x; }".to_string();
                    }
                    Stmt::Module(module)
                }
                _ => unreachable!("a module statement"),
            }
        };
        assert_converged(
            "module m { from: f'b:/k'; unsigned; permissions: (select * from x); };",
            ReadEntry {
                name: "m".to_string(),
                stmt: braced,
                markers: Markers::default(),
            },
            Family::Module,
            "m",
        );
    }

    /// Sentinel keys compare textually equal by the shape-only rule —
    /// both sides spell `'[REDACTED]'` (and a genuinely different URL
    /// drifts, the redaction never hiding real deltas).
    #[test]
    fn sentinel_keys_compare_textually() {
        assert_converged(
            "access a { on: root; type: jwt; algorithm: hs512; key: '[REDACTED]'; };",
            entry(
                "a",
                "access a { on: root; type: jwt; algorithm: hs512; key: '[REDACTED]'; };",
            ),
            Family::Access,
            "a",
        );
        assert_drift(
            "access a { on: root; type: jwt; url: 'https://a'; };",
            entry("a", "access a { on: root; type: jwt; url: 'https://b'; };"),
            Family::Access,
            "a",
        );
    }

    /// Function bodies compare byte-wise — the canon and the engine
    /// print agree (probe-pinned), multi-statement bodies keep their
    /// braces, and a body delta drifts.
    #[test]
    fn function_bodies_compare_byte_wise() {
        assert_converged(
            "fn one($a: int) { return $a; };",
            entry("one", "fn one($a: int) { return $a; };"),
            Family::Function,
            "one",
        );
        assert_drift(
            "fn one($a: int) { return $a; };",
            entry("one", "fn one($a: int) { return 1; };"),
            Family::Function,
            "one",
        );
    }

    // ---------------------------------------------------------------
    // The odd classes
    // ---------------------------------------------------------------

    /// Api paths compare by RE-PARSE: the segment-initial escape flip
    /// (authored static `\:x` re-printed `/:x`) drifts with the path
    /// named; the slash collapse and param spellings converge.
    #[test]
    fn api_paths_compare_by_reparse() {
        let group = "get { then: create x; };";
        // SDL authors the segment-initial escape through the string
        // escape of the backslash itself (`'/\\:x'` → value `/\:x`).
        // The finding pairs the flipped print and keys on the SDL
        // side (the source of truth).
        assert_drift(
            &format!("api '/\\\\:x' {{ {group} }};"),
            entry("/:x", &format!("api '/:x' {{ {group} }};")),
            Family::Api,
            "/\\:x",
        );
        assert_converged(
            &format!("api '/a//b' {{ {group} }};"),
            entry("/a/b", &format!("api '/a/b' {{ {group} }};")),
            Family::Api,
            "/a/b",
        );
        // A stable static-vs-param pair stays converged.
        assert_converged(
            &format!("api '/:x' {{ {group} }};"),
            entry("/:x", &format!("api '/:x' {{ {group} }};")),
            Family::Api,
            "/:x",
        );
    }

    /// Raw bodies compare as trimmed text; a raw-vs-structured cross
    /// is drift with the route named (the finding carries the read's
    /// markers).
    #[test]
    fn raw_bodies_and_crosses() {
        assert_converged(
            "raw { DEFINE INDEX i ON t FIELDS a UNIQUE; };",
            ReadEntry {
                name: "i".to_string(),
                stmt: sdl_stmt("raw { DEFINE INDEX i ON t FIELDS a UNIQUE; };"),
                markers: Markers::default(),
            },
            Family::Index,
            "i",
        );
        // Cross: the SDL structured index against the read's raw route.
        let findings = root_diff(
            "index unique i { on: t; fields: a; };",
            ReadEntry {
                name: "i".to_string(),
                stmt: sdl_stmt("raw { DEFINE INDEX i ON t FIELDS a UNIQUE; };"),
                markers: Markers::default(),
            },
        );
        assert!(
            matches!(
                class_of(&findings, Family::Index, "i"),
                Some(DriftClass::Drift { .. })
            ),
            "expected the cross to drift, found {findings:?}"
        );
    }

    /// The auto-seeded default namespace's fingerprint is
    /// expected-unmanaged unless the SDL file declares the namespace
    /// (then it compares ordinarily).
    #[test]
    fn fingerprint_namespace_is_expected_unmanaged() {
        let fingerprinted = ReadEntry {
            name: "test".to_string(),
            stmt: Stmt::Namespace(Namespace {
                name: crate::ast::Ident::new("test"),
                keyword: crate::ast::Carried::default(),
                comment: Some(crate::ast::DocComment {
                    lines: vec!["Default namespace generated by SurrealDB".to_string()],
                }),
                comments: Vec::new(),
                clause_comments: Vec::new(),
            }),
            markers: Markers {
                auto_seeded_default_namespace: true,
                ..Markers::default()
            },
        };
        let (file, diags) = crate::parser::parse_file("database scratch;");
        assert!(diags.is_empty());
        let mut model = ReadModel::default();
        model.mark_ingested(ScopeKey::Root);
        model.root.push(fingerprinted.clone());
        let findings = diff(&file, &model);
        assert!(
            matches!(
                class_of(&findings, Family::Namespace, "test"),
                Some(DriftClass::ExpectedUnmanaged { .. })
            ),
            "expected expected-unmanaged, found {findings:?}"
        );
        // Declared: ordinary comparison (the fingerprint comment is a
        // delta the user owns).
        let (declared, diags) = crate::parser::parse_file("namespace test;");
        assert!(diags.is_empty());
        let mut model = ReadModel::default();
        model.mark_ingested(ScopeKey::Root);
        model.root.push(fingerprinted);
        let findings = diff(&declared, &model);
        assert!(
            matches!(
                class_of(&findings, Family::Namespace, "test"),
                Some(DriftClass::Drift { .. })
            ),
            "expected the declared fingerprint namespace to compare ordinarily, found {findings:?}"
        );
    }

    /// An unmatched standalone field/index/event compares against its
    /// table's nested twin: converged → no finding (placement folds
    /// it on retro-apply), divergent → drift, no twin → SDL-only.
    #[test]
    fn divergent_standalones_compare_against_their_twin() {
        // Converged twin: no finding at all.
        let (file, diags) =
            crate::parser::parse_file("table t { field x: int; }; field x: int { on: t; };");
        assert!(diags.is_empty());
        let mut model = ReadModel::default();
        model.mark_ingested(ScopeKey::Root);
        model.root.push(entry("t", "table t { field x: int; };"));
        assert!(diff(&file, &model).is_empty(), "expected a clean diff");
        // Divergent twin: drift, carrying the twin as the database side.
        let (file, diags) =
            crate::parser::parse_file("table t { field x: int; }; field x: string { on: t; };");
        assert!(diags.is_empty());
        let mut model = ReadModel::default();
        model.mark_ingested(ScopeKey::Root);
        model.root.push(entry("t", "table t { field x: int; };"));
        let findings = diff(&file, &model);
        assert!(
            matches!(
                class_of(&findings, Family::Field, "x"),
                Some(DriftClass::Drift { .. })
            ),
            "expected the divergent standalone to drift, found {findings:?}"
        );
        // No twin at all: the ordinary SDL-only class (the scope is
        // ingested — examined — and the host table genuinely absent).
        let (file, diags) = crate::parser::parse_file("field x: int { on: lonely; };");
        assert!(diags.is_empty());
        let mut model = ReadModel::default();
        model.mark_ingested(ScopeKey::Root);
        let findings = diff(&file, &model);
        assert!(
            matches!(
                class_of(&findings, Family::Field, "x"),
                Some(DriftClass::SdlOnly { .. })
            ),
            "expected SDL-only, found {findings:?}"
        );
    }

    /// Table children pair individually: a child delta is a child
    /// finding, and a database-only child carries the host table's ON
    /// edge (the REMOVE script's child-before-parent order).
    #[test]
    fn table_children_pair_individually() {
        let (file, diags) = crate::parser::parse_file("table t { field a: int; field b: int; };");
        assert!(diags.is_empty());
        let mut model = ReadModel::default();
        model.mark_ingested(ScopeKey::Root);
        model
            .root
            .push(entry("t", "table t { field a: string; field c: int; };"));
        let findings = diff(&file, &model);
        assert!(
            matches!(
                class_of(&findings, Family::Field, "a"),
                Some(DriftClass::Drift { .. })
            ),
            "expected child a to drift, found {findings:?}"
        );
        assert!(
            matches!(
                class_of(&findings, Family::Field, "b"),
                Some(DriftClass::SdlOnly { .. })
            ),
            "expected child b SDL-only, found {findings:?}"
        );
        match class_of(&findings, Family::Field, "c") {
            Some(DriftClass::DatabaseOnly { on_table, .. }) => {
                assert_eq!(on_table.as_deref(), Some("t"));
            }
            other => panic!("expected child c database-only, found {other:?} — {findings:?}"),
        }
    }

    /// A raw database-only leftover's ON edge reads through the
    /// body's own family: a field's ON follows its path, and a TABLE
    /// body never reports one (a view's FROM list is a placement
    /// dependency, not an ON edge).
    #[test]
    fn raw_on_edges_read_through_the_body_family() {
        let field = Stmt::Raw(crate::ast::Raw {
            body: "DEFINE FIELD f ON t;".to_string(),
            comments: Vec::new(),
            clause_comments: Vec::new(),
        });
        assert_eq!(on_table_of(&field).as_deref(), Some("t"));
        let dotted = Stmt::Raw(crate::ast::Raw {
            body: "DEFINE FIELD OVERWRITE m.* ON t TYPE int;".to_string(),
            comments: Vec::new(),
            clause_comments: Vec::new(),
        });
        assert_eq!(on_table_of(&dotted).as_deref(), Some("t"));
        let table = Stmt::Raw(crate::ast::Raw {
            body: "DEFINE TABLE v AS SELECT * FROM base SCHEMAFULL;".to_string(),
            comments: Vec::new(),
            clause_comments: Vec::new(),
        });
        assert_eq!(on_table_of(&table), None);
    }

    /// A database-only leftover carries its ON edge; an unmatched
    /// scope's database is the REMOVE candidate.
    #[test]
    fn database_only_findings_carry_on_edges() {
        let (file, diags) = crate::parser::parse_file("namespace test;");
        assert!(diags.is_empty());
        let mut model = ReadModel::default();
        model.mark_ingested(ScopeKey::Root);
        model.root.push(entry("scratch", "database scratch;"));
        let findings = diff(&file, &model);
        match class_of(&findings, Family::Database, "scratch") {
            Some(DriftClass::DatabaseOnly { on_table, .. }) => {
                assert_eq!(on_table.as_deref(), None);
            }
            other => panic!("expected database-only, found {other:?}"),
        }
    }
}
