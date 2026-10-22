//! Canonical placement: the spec's dependency-bounded
//! topological rule (spec/README.md, "canonical output is a total
//! function") over the merge's per-scope sections.
//!
//! A [`Section`] is one contiguous scope run of the merged file: the
//! merged authored statements in order — the fixed skeleton, authored
//! order never changes — plus the scope's adopted statements. Each
//! adoptee slots into the earliest position after the last statement
//! it depends on and before the first statement that depends on it,
//! with the readers' family/map order as the tie-break among
//! equally-bounded adoptees. The dependency relation is the spec's:
//! an index on its fields' table and (fulltext) its analyzer; an
//! event on its table; a field on nothing; a view on the tables its
//! query reads (the engine's own dependency set — the `FROM` targets
//! its define-time validation iterates, verified against the v3.2.4
//! source); an analyzer on its `function:` fn; every statement on its
//! enclosing section (the slot floor).
//!
//! Nested placement is the default — the readers already nest table
//! children — and yields to the dependency bound only for a fulltext
//! index whose analyzer sits after its table: the index lifts to top
//! level directly after its analyzer (indexes have no dependents, so
//! nothing reflows). The non-nested form is the linter's.
//!
//! A standalone authored `field`/`index`/`event` whose table carries
//! the same definition nested folds away (the duplication
//! family): nested is the canonical placement. A divergent standalone
//! stays — its drift is the differ's.

use std::collections::{BTreeMap, BTreeSet};

use surrealdb_core::syn;
use surrealdb_sql::TopLevelExpr;
use surrealdb_sql::access_type::{AccessType, JwtAccess, JwtAccessVerify};
use surrealdb_sql::statements::define::DefineStatement;
use surrealdb_sql::statements::define::config::ConfigInner;
use surrealdb_syn::ParserConfig;
use surrealdb_types::ToSql as _;

use crate::ast::{FnKind, IndexKind, Kind, Stmt, TableKind};
use crate::expr::experimental_capabilities;
use crate::lexer::{CaptureFollow, CaptureStopWord, Scanner};

use super::normalize::{Family, ScopeKey, classify, elide_and_fold, kind_text_equal};

/// One contiguous scope run of the merged file.
pub(super) struct Section {
    pub(super) scope: ScopeKey,
    /// The merged authored statements, in authored order.
    pub(super) skeleton: Vec<Stmt>,
    /// Each skeleton statement's provenance, parallel to `skeleton`:
    /// the authored input's index, or `None` for a synthesized
    /// anchor (the merge's tail sections).
    pub(super) skeleton_prov: Vec<Option<usize>>,
    /// The scope's adopted statements (elide + fold applied), in the
    /// readers' family/map order.
    pub(super) adoptees: Vec<Stmt>,
}

impl Section {
    pub(super) fn new(scope: ScopeKey) -> Self {
        Section {
            scope,
            skeleton: Vec::new(),
            skeleton_prov: Vec::new(),
            adoptees: Vec::new(),
        }
    }

    /// Pushes an authored skeleton statement with its input index.
    pub(super) fn push_authored(&mut self, stmt: Stmt, input: usize) {
        self.skeleton.push(stmt);
        self.skeleton_prov.push(Some(input));
    }

    /// Pushes a synthesized skeleton statement (no authored home).
    pub(super) fn push_synthesized(&mut self, stmt: Stmt) {
        self.skeleton.push(stmt);
        self.skeleton_prov.push(None);
    }
}

/// Places every section and flattens the file. The second vector runs
/// parallel to the first — each output statement's provenance: the
/// index of the authored input it merged from, or `None` when the
/// statement is adopted (or synthesized) and has no authored home.
pub(super) fn place_sections(sections: Vec<Section>) -> (Vec<Stmt>, Vec<Option<usize>>) {
    sections
        .into_iter()
        .flat_map(|section| {
            let (items, provenance) = place_section(section);
            items.into_iter().zip(provenance)
        })
        .unzip()
}

/// Places one section: the twin fold, the earliest-gap loop, then the
/// fulltext lift. Dependency slots: slot `i` (0..=n) sits between
/// `skeleton[i-1]` and `skeleton[i]` — slot 0 is the section head,
/// slot `n` the tail.
fn place_section(section: Section) -> (Vec<Stmt>, Vec<Option<usize>>) {
    let Section {
        scope,
        mut skeleton,
        mut skeleton_prov,
        adoptees,
    } = section;
    // The twin fold runs BEFORE the lift, and that order is
    // load-bearing for idempotence: re-applying a lifted file leaves
    // the top-level index authored while the table re-adopts the same
    // definition nested — the fold removes the authored duplicate, and
    // the lift then re-extracts exactly one. Lift-then-fold would
    // duplicate it instead.
    drop_standalone_twins(&mut skeleton, &mut skeleton_prov, &adoptees);
    let n = skeleton.len();
    // Content never precedes a section's opening statement; the ROOT
    // region has no opener (users/accesses carry their `on:`,
    // `config default` hoists — position-safe at the head).
    let floor = match scope {
        ScopeKey::Root => 0,
        _ => n.min(1),
    };

    // Authored statements by match key — raw statements included
    // under the family + name their DEFINE header carries (a raw
    // route is a spelling deviation, not a structural one: a surfaced
    // child's dependency on its raw table must still resolve).
    let authored_at: BTreeMap<(Family, String), usize> = skeleton
        .iter()
        .enumerate()
        .filter_map(|(i, stmt)| stmt_key(stmt).map(|key| (key, i)))
        .collect();

    // The slot an adoptee may not pass: the first authored statement
    // that depends on it pins it before itself (slot k is before
    // A[k]). A dep target the section does not hold resolves nowhere —
    // no edge (hand-fed mismatch; the differ owns it).
    let mut limits = vec![n; adoptees.len()];
    for (k, stmt) in skeleton.iter().enumerate() {
        for dep in authored_dep_names(stmt) {
            for (j, adoptee) in adoptees.iter().enumerate() {
                if stmt_key(adoptee).is_some_and(|key| key == dep) {
                    limits[j] = limits[j].min(k);
                }
            }
        }
    }

    let keys: Vec<Option<(Family, String)>> = adoptees.iter().map(stmt_key).collect();
    let deps: Vec<Vec<(Family, String)>> = adoptees.iter().map(own_dep_names).collect();

    // The earliest-gap loop: repeatedly place the ready adoptee with
    // the smallest (slot, family, entry index). Ready = every dep
    // resolves to an authored statement or a placed adoptee. The slot
    // is the max dep slot, floored, clamped down to the limit — a
    // contradicting window (dep after the dependent) only arises in
    // hand-fed mismatched input, and the authored dependent wins since
    // authored order is fixed.
    let mut slots: Vec<Option<usize>> = vec![None; adoptees.len()];
    let mut order: Vec<Vec<usize>> = vec![Vec::new(); n + 1];
    let mut placed = 0usize;
    let bound_of = |j: usize, slots: &[Option<usize>]| -> (Option<usize>, bool) {
        let mut bound = Some(floor);
        for dep in &deps[j] {
            if let Some(&i) = authored_at.get(dep) {
                bound = bound.map(|b| b.max(i + 1));
                continue;
            }
            if let Some(dj) = keys.iter().position(|key| *key == Some(dep.clone())) {
                // A dep target the section does not hold (hand-fed
                // mismatch): no edge, the floor applies.
                match slots[dj] {
                    Some(g) => bound = bound.map(|b| b.max(g)),
                    None => return (None, false),
                }
            }
        }
        (bound, true)
    };
    loop {
        let mut best: Option<(usize, Family, usize)> = None;
        for j in 0..adoptees.len() {
            if slots[j].is_some() {
                continue;
            }
            let (bound, ready) = bound_of(j, &slots);
            if !ready {
                continue;
            }
            let family = keys[j].as_ref().map_or(Family::Raw, |(f, _)| *f);
            let slot = bound.map_or(floor, |b| b.min(limits[j]).max(floor));
            let candidate = (slot, family, j);
            if best.is_none_or(|b| candidate < b) {
                best = Some(candidate);
            }
        }
        match best {
            Some((slot, _, j)) => {
                slots[j] = Some(slot);
                order[slot].push(j);
                placed += 1;
            }
            None => break,
        }
    }
    // Deadlock = a dependency cycle among adoptees (two views reading
    // each other — a real engine round trip cannot define one, this
    // is hand-fed only): force the lowest key unplaced, unresolved
    // adoptee deps treated as no-edge. Deterministic; the differ owns
    // the diagnosis.
    while placed < adoptees.len() {
        let j = (0..adoptees.len())
            .find(|&j| slots[j].is_none())
            .expect("placed undercounts the unplaced");
        let (bound, _) = bound_of(j, &slots);
        let slot = bound.map_or(floor, |b| b.min(limits[j]).max(floor));
        slots[j] = Some(slot);
        order[slot].push(j);
        placed += 1;
    }

    let mut out: Vec<Stmt> = Vec::with_capacity(n + adoptees.len());
    let mut out_prov: Vec<Option<usize>> = Vec::with_capacity(n + adoptees.len());
    let mut skeleton_iter = skeleton.into_iter().zip(skeleton_prov);
    for (slot, at_slot) in order.iter().enumerate() {
        if slot > 0
            && let Some((stmt, prov)) = skeleton_iter.next()
        {
            out.push(stmt);
            out_prov.push(prov);
        }
        for j in at_slot {
            out.push(adoptees[*j].clone());
            out_prov.push(None);
        }
    }
    out.extend(skeleton_iter.map(|(stmt, prov)| {
        out_prov.push(prov);
        stmt
    }));

    lift_analyzer_bound_indexes(out, out_prov)
}

/// A fulltext-index lift: analyzer position, table position, and the
/// lifted statement itself.
struct Lift {
    analyzer_at: usize,
    table_at: usize,
    stmt: Stmt,
}

/// The fulltext-index yield: a nested index whose analyzer statement
/// sits after its table lifts to top level, directly after its
/// analyzer ("nested placement is the default, but yields to the
/// dependency bound"). A nested index whose analyzer is missing
/// entirely stays nested — nothing to order against (hand-fed
/// mismatch; the differ owns it).
fn lift_analyzer_bound_indexes(
    mut out: Vec<Stmt>,
    mut prov: Vec<Option<usize>>,
) -> (Vec<Stmt>, Vec<Option<usize>>) {
    let final_at: BTreeMap<(Family, String), usize> = out
        .iter()
        .enumerate()
        .filter_map(|(i, stmt)| stmt_key(stmt).map(|key| (key, i)))
        .collect();
    let mut lifts: Vec<Lift> = Vec::new();
    for (i, stmt) in out.iter().enumerate() {
        let Stmt::Table(table) = stmt else {
            continue;
        };
        for nested in &table.nested {
            let Stmt::Index(index) = nested else {
                continue;
            };
            let IndexKind::FullText {
                analyzer: Some(analyzer),
                ..
            } = &index.kind
            else {
                continue;
            };
            let analyzer_at = final_at.get(&(Family::Analyzer, analyzer.raw.clone()));
            if let Some(&analyzer_at) = analyzer_at
                && analyzer_at > i
            {
                lifts.push(Lift {
                    analyzer_at,
                    table_at: i,
                    stmt: nested.clone(),
                });
            }
        }
    }
    if lifts.is_empty() {
        return (out, prov);
    }
    // Remove from their tables (top-level positions never shift),
    // then splice each group in directly after its analyzer,
    // descending so earlier splices never shift pending ones. A lifted
    // index carries NO provenance: the table's slot stays the
    // table's, and the index places by its stream neighbors (directly
    // after its analyzer) — a second `Some(table)` entry would
    // overwrite the table in the write-back's per-home rebuild.
    let mut names_by_table: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for lift in &lifts {
        let Stmt::Index(index) = &lift.stmt else {
            unreachable!("lifts collect nested indexes only");
        };
        names_by_table
            .entry(lift.table_at)
            .or_default()
            .push(index.name.raw.clone());
    }
    for (table_at, names) in names_by_table {
        if let Some(Stmt::Table(table)) = out.get_mut(table_at) {
            table.nested.retain(
                |nested| !matches!(nested, Stmt::Index(ix) if names.contains(&ix.name.raw)),
            );
        }
    }
    let mut grouped: BTreeMap<usize, Vec<Stmt>> = BTreeMap::new();
    for Lift {
        analyzer_at, stmt, ..
    } in lifts
    {
        grouped.entry(analyzer_at).or_default().push(stmt);
    }
    for (analyzer_at, lifted) in grouped.into_iter().rev() {
        let width = lifted.len();
        out.splice(analyzer_at + 1..analyzer_at + 1, lifted);
        prov.splice(
            analyzer_at + 1..analyzer_at + 1,
            std::iter::repeat_n(None, width),
        );
    }
    (out, prov)
}

/// A match key for placement: [`classify`] for every parsed
/// statement; a raw statement under the family + name its `DEFINE`
/// header carries (the placement subset — only the families a
/// dependency can point into).
pub(super) fn stmt_key(stmt: &Stmt) -> Option<(Family, String)> {
    match stmt {
        Stmt::Raw(raw) => raw_match_key(&raw.body)
            .filter(|(family, _)| matches!(family, Family::Table | Family::Analyzer)),
        other => Some(classify(other)),
    }
}

/// The dependencies a statement's own placement binds it after
/// (adoptees' bounds; the spec's dependency relation).
fn own_dep_names(stmt: &Stmt) -> Vec<(Family, String)> {
    match stmt {
        Stmt::Index(index) => {
            let mut deps = vec![(Family::Table, index.on.raw.clone())];
            if let IndexKind::FullText {
                analyzer: Some(analyzer),
                ..
            } = &index.kind
            {
                deps.push((Family::Analyzer, analyzer.raw.clone()));
            }
            deps
        }
        Stmt::Event(event) => vec![(Family::Table, event.on.raw.clone())],
        // A field depends on nothing (spec/README.md's relation).
        Stmt::Field(_) => Vec::new(),
        Stmt::Table(table) => match &table.kind {
            TableKind::View { query } => view_tables(&query.canon)
                .into_iter()
                .map(|name| (Family::Table, name))
                .collect(),
            _ => Vec::new(),
        },
        Stmt::Analyzer(analyzer) => analyzer
            .function
            .as_ref()
            .filter(|path| path.kind == FnKind::Fn)
            .map(|path| vec![(Family::Function, path.segments.join("::"))])
            .unwrap_or_default(),
        Stmt::Raw(raw) => raw_dep_names(&raw.body),
        _ => Vec::new(),
    }
}

/// The body's query text for the embedded parser — the raw capture's
/// brace balance guarantees no trailing `;`, and a bare append is
/// byte-blind to a trailing line comment (the ledgered corner: the
/// `;` lands inside the comment, the parse fails, the body reads
/// fail-closed).
fn raw_query(body: &str) -> String {
    let mut query = String::with_capacity(body.len() + 1);
    query.push_str(body);
    if !query.ends_with(';') {
        query.push(';');
    }
    query
}

/// A raw body's own dependencies: an index's or event's `ON <table>`
/// (the placement dependency) and a table body carrying a view's
/// `AS SELECT` reads that FROM list — for EVERY statement of the body
/// (a multi-statement raw block renders contiguously, so the union of
/// its statements' edges is the body's edge set; a statement carrying
/// none of these kinds leaves the others' standing). The body
/// re-parses with the embedded parser — the same grammar the engine
/// applies to the rendered migration — and the edges read off the
/// typed AST through upstream's own fields (`DefineIndexStatement`
/// /`DefineEventStatement`'s `ON` target; `View`'s `what` list —
/// exactly the engine's own view dependency set, the list its
/// define-time validation iterates), so any legal clause order reads,
/// printed form or not. An unparseable body yields no edge, the floor
/// every raw reader here shares (deterministic; the same
/// hand-fed-mismatch posture as a missing target; the engine refuses
/// the body at parse time, visibly). The differ reuses the ON edge as
/// a database-only finding's `ON <table>` (the REMOVE script's
/// child-before-parent order).
pub(super) fn raw_dep_names(body: &str) -> Vec<(Family, String)> {
    let Ok(ast) = syn::parse_with_capabilities(
        &raw_query(body),
        &experimental_capabilities(),
        &ParserConfig::default(),
    ) else {
        return Vec::new();
    };
    let mut deps: Vec<(Family, String)> = Vec::new();
    for expr in &ast.expressions {
        let TopLevelExpr::Expr(surrealdb_sql::Expr::Define(define)) = expr else {
            continue;
        };
        match define.as_ref() {
            DefineStatement::Table(table) => {
                if let Some(view) = &table.view {
                    for name in &view.what {
                        push_dep(&mut deps, Family::Table, name.as_str());
                    }
                }
            }
            DefineStatement::Index(index) => {
                if let Some(table) = expr_table_name(&index.what) {
                    push_dep(&mut deps, Family::Table, &table);
                }
            }
            DefineStatement::Event(event) => {
                if let Some(table) = expr_table_name(&event.target_table) {
                    push_dep(&mut deps, Family::Table, &table);
                }
            }
            // No placement dependency of these kinds (an analyzer's
            // `function:` fn edge is order.rs's own placement read,
            // not this one).
            _ => {}
        }
    }
    deps
}

/// An `ON <table>` target's decoded name — upstream's grammar holds
/// it as the table expression (`Expr::Table`); any other spelling
/// reads no edge, never a guess.
fn expr_table_name(expr: &surrealdb_sql::Expr) -> Option<String> {
    match expr {
        surrealdb_sql::Expr::Table(name) => Some(name.as_str().to_string()),
        _ => None,
    }
}

fn push_dep(deps: &mut Vec<(Family, String)>, family: Family, name: &str) {
    let edge = (family, name.to_string());
    if !deps.contains(&edge) {
        deps.push(edge);
    }
}

/// A raw body's fulltext-index `ANALYZER <analyzer>` clauses — the
/// migration defines pass's edge (script.rs) — one per fulltext index
/// statement, wherever that statement stands in a multi-statement raw
/// block (the block renders contiguously, so the union is the body's
/// edge set; a target carried by two statements is one edge). The
/// body re-parses with the embedded parser — the same grammar the
/// engine applies to the rendered migration — and the clauses read
/// off the typed AST (`Index::FullText`'s analyzer), so any legal
/// clause order reads, printed form or not. No fulltext index — or a
/// body that fails to parse entirely — reads nothing: no edge, never
/// a guess.
pub(super) fn raw_index_analyzer(body: &str) -> Vec<String> {
    let Ok(ast) = syn::parse_with_capabilities(
        &raw_query(body),
        &experimental_capabilities(),
        &ParserConfig::default(),
    ) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for expr in &ast.expressions {
        let TopLevelExpr::Expr(surrealdb_sql::Expr::Define(define)) = expr else {
            continue;
        };
        let DefineStatement::Index(index) = define.as_ref() else {
            continue;
        };
        let surrealdb_sql::index::Index::FullText(params) = &index.index else {
            continue;
        };
        let name = params.az.as_str().to_string();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// A raw body's row 6–13 value-slot reads (the migration defines
/// pass's settle edge — script.rs): the `$param` / `fn::` / `mod::`
/// idents the reader families evaluate WHEN THE DEFINE EXECUTES. The
/// body re-parses with the embedded parser — the same grammar the
/// engine applies to the rendered migration — and EVERY statement's
/// slots read off the typed AST: a multi-statement raw block renders
/// contiguously, so the union of its statements' reads is the body's
/// edge set (a statement that is not a DEFINE, or a family without a
/// define-time slot, contributes nothing and leaves the other
/// statements' reads standing). The arms mirror order.rs's rows 6–13
/// through upstream's own fields (param VALUE; sequence
/// BATCH/START/TIMEOUT; user DURATION sides; access jwt verify/issuer
/// plus DURATION sides; bucket BACKEND; api middleware ARGS —
/// statement and `DEFINE CONFIG API` alike; config DEFAULT
/// NAMESPACE/DATABASE), so any legal clause order reads, printed form
/// or not. Each slot expression re-prints through its `ToSql` and
/// rides the typed rows' own extractor
/// ([`crate::refs::capture_param_fn_refs`]), so the extracted names
/// are the definition spellings. Absent or `NONE` slots read nothing;
/// an unparseable body reads no edge — the fail-closed posture the
/// other raw readers share (no edge, never a guess; the engine
/// refuses the body at parse time, visibly).
pub(super) fn raw_value_reads(body: &str) -> Vec<(Family, String)> {
    let Ok(ast) = syn::parse_with_capabilities(
        &raw_query(body),
        &experimental_capabilities(),
        &ParserConfig::default(),
    ) else {
        return Vec::new();
    };
    let mut reads: Vec<(Family, String)> = Vec::new();
    for expr in &ast.expressions {
        let TopLevelExpr::Expr(surrealdb_sql::Expr::Define(define)) = expr else {
            continue;
        };
        match define.as_ref() {
            DefineStatement::Param(param) => slot_reads(&param.value, &mut reads),
            DefineStatement::Sequence(sequence) => {
                for expr in [&sequence.batch, &sequence.start, &sequence.timeout] {
                    slot_reads(expr, &mut reads);
                }
            }
            DefineStatement::User(user) => {
                for expr in [&user.token_duration, &user.session_duration] {
                    slot_reads(expr, &mut reads);
                }
            }
            DefineStatement::Access(access) => {
                access_type_reads(&access.access_type, &mut reads);
                for expr in [
                    &access.duration.grant,
                    &access.duration.token,
                    &access.duration.session,
                ] {
                    slot_reads(expr, &mut reads);
                }
            }
            DefineStatement::Bucket(bucket) => {
                if let Some(backend) = &bucket.backend {
                    slot_reads(backend, &mut reads);
                }
            }
            DefineStatement::Api(api) => {
                middleware_reads(&api.config.middleware, &mut reads);
                for action in &api.actions {
                    middleware_reads(&action.config.middleware, &mut reads);
                }
            }
            DefineStatement::Config(config) => match &config.inner {
                ConfigInner::Api(api) => middleware_reads(&api.middleware, &mut reads),
                ConfigInner::Default(default) => {
                    slot_reads(&default.namespace, &mut reads);
                    slot_reads(&default.database, &mut reads);
                }
                ConfigInner::GraphQL(_) => {}
            },
            // No define-time value slot (the frozen table's other rows).
            _ => {}
        }
    }
    reads
}

/// One slot expression's Param/Fn/Module reads — the typed rows' own
/// capture ([`crate::refs::capture_param_fn_refs`]), deduplicated
/// across the statement's slots (a target read twice is one edge).
fn slot_reads(expr: &surrealdb_sql::Expr, out: &mut Vec<(Family, String)>) {
    let (params, fns, modules) = crate::refs::capture_param_fn_refs(&expr.to_sql());
    for (family, names) in [
        (Family::Param, params),
        (Family::Function, fns),
        (Family::Module, modules),
    ] {
        for name in names {
            if !out.contains(&(family, name.clone())) {
                out.push((family, name));
            }
        }
    }
}

/// The jwt payload's slots (the frozen table's row 9): the verify
/// key/url and the issuer key. The audience list is string literals
/// only (the licensed deviation) and record signup/signin are
/// runtime-evaluated (row 10) — neither carries a slot.
fn access_type_reads(access_type: &AccessType, out: &mut Vec<(Family, String)>) {
    match access_type {
        AccessType::Jwt(jwt) => jwt_reads(jwt, out),
        AccessType::Record(record) => jwt_reads(&record.jwt, out),
        AccessType::Bearer(bearer) => jwt_reads(&bearer.jwt, out),
    }
}

fn jwt_reads(jwt: &JwtAccess, out: &mut Vec<(Family, String)>) {
    match &jwt.verify {
        JwtAccessVerify::Key(key) => slot_reads(&key.key, out),
        JwtAccessVerify::Jwks(jwks) => slot_reads(&jwks.url, out),
    }
    if let Some(issue) = &jwt.issue {
        slot_reads(&issue.key, out);
    }
}

/// The middleware args' slots (row 12) — the fn NAMES are stored
/// unvalidated (no edge to them, the frozen table's record).
fn middleware_reads(
    middleware: &[surrealdb_sql::statements::define::config::api::Middleware],
    out: &mut Vec<(Family, String)>,
) {
    for entry in middleware {
        for arg in &entry.args {
            slot_reads(arg, out);
        }
    }
}

/// The dependencies an AUTHORED statement carries — its own bounds'
/// relation plus the analyzers of its nested fulltext indexes (an
/// authored table pins those analyzers before itself; an adopted
/// table's nested indexes are the lift's business instead, so the
/// table itself never moves for them). A mixin's fulltext index
/// members pin their analyzers the same way: the mixin sits above
/// every table spreading it (rule 7's authored order), so an adopted
/// analyzer slotting before the mixin defines before those tables'
/// expanded indexes.
pub(super) fn authored_dep_names(stmt: &Stmt) -> Vec<(Family, String)> {
    let mut deps = own_dep_names(stmt);
    let nested = match stmt {
        Stmt::Table(table) => &table.nested,
        Stmt::Mixin(mixin) => &mixin.members,
        _ => return deps,
    };
    for index in nested.iter().filter_map(|nested| match nested {
        Stmt::Index(index) => Some(index),
        _ => None,
    }) {
        if let IndexKind::FullText {
            analyzer: Some(analyzer),
            ..
        } = &index.kind
        {
            deps.push((Family::Analyzer, analyzer.raw.clone()));
        }
    }
    deps
}

/// The tables a view's canonical query reads: the SELECT's `FROM`
/// targets — exactly the engine's own view dependency set
/// (`View.what`, the list its define-time validation iterates). The
/// canon is the embedded printer's restricted select
/// (`SELECT <expr> FROM <t1, t2> [WHERE …] [GROUP …]` — `parse_view`'s
/// exact grammar), so the walk stops at the depth-0 bare `FROM`, then
/// at the depth-0 `WHERE`/`GROUP`; strings, comments, parens (a
/// subquery's own FROM), and quoted idents never read as either.
/// `scan_to_word` runs unguarded for this dialect: the printer prints
/// clause keywords uppercase, and an operand spelled exactly like a
/// stop word prints bare whenever the word is outside the printer's
/// own escape set (`EscapeIdent` backticks only `could_be_reserved` —
/// `from` is NOT one), so a depth-0 bare stop word is the clause
/// unless the canon carries such a spelling in operand position — a
/// projection field or idiom part spelled `from` (or `where`/`group`)
/// false-stops the walk, the same clause-head-spelled-operand family
/// as the splitter's word rule. Missing/odd shapes yield the empty
/// set — the view then orders by map order (total, deterministic);
/// the dialect-pinning tests fire before that could matter.
fn view_tables(canon: &str) -> BTreeSet<String> {
    let mut scan = Scanner::new(canon);
    let Ok(Some(word)) = scan.scan_to_word(&["FROM"]) else {
        return BTreeSet::new();
    };
    if !word.eq_ignore_ascii_case("FROM") {
        return BTreeSet::new();
    }
    // The cursor rests on the stop word: consume it, then take the
    // targets up to the next depth-0 clause word.
    scan.read_word();
    scan.skip_surrealql_trivia();
    let Ok((span, _)) = scan.capture_until_word(&[
        CaptureStopWord("WHERE", CaptureFollow::Any),
        CaptureStopWord("GROUP", CaptureFollow::Any),
    ]) else {
        return BTreeSet::new();
    };
    split_top_level_commas(span.slice(canon))
        .into_iter()
        .filter_map(|target| {
            let target = target.trim();
            if target.is_empty() {
                return None;
            }
            // The printer's `EscapeKwFreeIdent`: plain, or backticked /
            // ⟨angle-wrapped⟩ when the name is keyword-shaped.
            if let Some(inner) = target
                .strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'))
            {
                return Some(inner.to_string());
            }
            if let Some(inner) = target
                .strip_prefix('⟨')
                .and_then(|rest| rest.strip_suffix('⟩'))
            {
                return Some(inner.to_string());
            }
            Some(target.to_string())
        })
        .collect()
}

/// Splits on commas outside backtick/angle quoted idents (a table
/// named with a comma can only arrive escaped — the raw `,` is always
/// a separator).
fn split_top_level_commas(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut wrap: Option<char> = None;
    for ch in text.chars() {
        match (wrap, ch) {
            (None, '`' | '⟨') => {
                wrap = Some(ch);
                current.push(ch);
            }
            (Some('`'), '`') | (Some('⟨'), '⟩') => {
                wrap = None;
                current.push(ch);
            }
            (None, ',') => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    parts.push(current);
    parts
}

/// A raw body's `DEFINE <family> <name>` match key — every family's
/// header shape, with the INFO map key's own spelling (a field's
/// dotted path, a function's `fn::`-less path, a param's `$`-less
/// name, an api's decoded path, a config's variant). `None` when the
/// shape deviates (no match key — the differ classifies both sides
/// unmatched). The placement subset rides [`stmt_key`]'s filter.
pub(super) fn raw_match_key(body: &str) -> Option<(Family, String)> {
    let mut scan = Scanner::new(body);
    scan.skip_surrealql_trivia();
    let define = read_bare_word(&mut scan)?;
    if !define.eq_ignore_ascii_case("DEFINE") {
        return None;
    }
    let family = match read_bare_word(&mut scan)?.to_ascii_uppercase().as_str() {
        "TABLE" => Family::Table,
        "ANALYZER" => Family::Analyzer,
        "INDEX" => Family::Index,
        "EVENT" => Family::Event,
        "FIELD" => Family::Field,
        "FUNCTION" => Family::Function,
        "PARAM" => Family::Param,
        "SEQUENCE" => Family::Sequence,
        "NAMESPACE" => Family::Namespace,
        "DATABASE" => Family::Database,
        "USER" => Family::User,
        "ACCESS" => Family::Access,
        "API" => Family::Api,
        "BUCKET" => Family::Bucket,
        "MODULE" => Family::Module,
        "CONFIG" => Family::Config,
        _ => return None,
    };
    // The define-kind prefixes ride BEFORE the per-family key reads:
    // authored raw may carry them on any family (INFO never prints
    // them) — every arm below reads the key position, not the prefix.
    skip_define_kind_prefixes(&mut scan, body);
    // The variants with no name ride their variant key (config) or the
    // empty name (an anonymous silo module — classify's own shape).
    let name = match family {
        Family::Config => {
            let variant = read_bare_word(&mut scan)?;
            match variant.to_ascii_lowercase().as_str() {
                "api" | "graphql" | "default" => variant.to_ascii_lowercase(),
                _ => return None,
            }
        }
        Family::Module => {
            let first = read_bare_word(&mut scan)?;
            let executable_head =
                first.eq_ignore_ascii_case("FROM") || first.eq_ignore_ascii_case("UNSIGNED");
            match executable_head {
                true => String::new(),
                false => first,
            }
        }
        Family::Api => {
            scan.skip_surrealql_trivia();
            let span = scan.capture_string_literal().ok()?;
            string_literal_value(span.slice(body))?
        }
        Family::Field => {
            // The printer spells field paths bare (`m.*`, `a.b`) —
            // segments glued, the wildcard a literal `*`.
            let mut path = read_bare_word(&mut scan)?;
            while scan.peek_byte() == Some(b'.') {
                scan.eat_byte(b'.');
                path.push('.');
                if scan.peek_byte() == Some(b'*') {
                    scan.eat_byte(b'*');
                    path.push('*');
                } else {
                    path.push_str(&read_bare_word(&mut scan)?);
                }
            }
            path
        }
        Family::Function => {
            // `fn::a::b` — segments glued on `::`; the key drops the
            // prefix (the INFO map's own shape).
            let mut segments = vec![read_bare_word(&mut scan)?];
            while scan.peek_byte() == Some(b':')
                && scan.offset() + 1 < body.len()
                && body.as_bytes()[scan.offset() + 1] == b':'
            {
                scan.eat_byte(b':');
                scan.eat_byte(b':');
                segments.push(read_bare_word(&mut scan)?);
            }
            let joined = segments.join("::");
            joined
                .strip_prefix("fn::")
                .or_else(|| joined.strip_prefix("mod::"))
                .map(str::to_string)
                .unwrap_or(joined)
        }
        Family::Param => {
            // The `$` prefix never rides the key.
            scan.skip_surrealql_trivia();
            scan.eat_byte(b'$');
            read_bare_word(&mut scan)?
        }
        _ => read_bare_word(&mut scan)?,
    };
    Some((family, name))
}

/// Consumes the optional define-kind prefixes (`OVERWRITE` /
/// `IF NOT EXISTS` — authored raw may carry them, INFO never prints
/// them), leaving the cursor on the key position. Lookahead only:
/// when neither prefix is present nothing is consumed and the
/// caller's key read takes the word.
fn skip_define_kind_prefixes(scan: &mut Scanner, body: &str) {
    let peek = |scan: &mut Scanner| {
        scan.skip_surrealql_trivia();
        scan.peek_word()
            .map(|span| span.slice(body).to_ascii_uppercase())
    };
    match peek(scan).as_deref() {
        Some("OVERWRITE") => {
            scan.read_word();
        }
        Some("IF") => {
            scan.read_word();
            if peek(scan).as_deref() == Some("NOT") {
                scan.read_word();
                if peek(scan).as_deref() == Some("EXISTS") {
                    scan.read_word();
                }
            }
        }
        _ => {}
    }
}

/// A raw body's `ON <table>` edge — the differ's database-only
/// findings carry it (the REMOVE script's child-before-parent order).
/// Index and event bodies read the placement dependency's ON; a FIELD
/// body's ON follows its path; a TABLE body carries no ON grammar at
/// all (a view's FROM list is a placement dependency, never an ON
/// edge).
pub(super) fn raw_on_table(body: &str) -> Option<String> {
    match raw_match_key(body)?.0 {
        Family::Index | Family::Event => raw_dep_names(body)
            .into_iter()
            .find_map(|(family, name)| matches!(family, Family::Table).then_some(name)),
        Family::Field => {
            let mut scan = Scanner::new(body);
            scan.skip_surrealql_trivia();
            if !read_bare_word(&mut scan)?.eq_ignore_ascii_case("DEFINE") {
                return None;
            }
            if !read_bare_word(&mut scan)?.eq_ignore_ascii_case("FIELD") {
                return None;
            }
            skip_define_kind_prefixes(&mut scan, body);
            // The path: segments glued on `.`, the wildcard literal.
            read_bare_word(&mut scan)?;
            while scan.peek_byte() == Some(b'.') {
                scan.eat_byte(b'.');
                if scan.peek_byte() == Some(b'*') {
                    scan.eat_byte(b'*');
                } else {
                    read_bare_word(&mut scan)?;
                }
            }
            let on = read_bare_word(&mut scan)?;
            if !on.eq_ignore_ascii_case("ON") {
                return None;
            }
            read_on_table(&mut scan, body)
        }
        _ => None,
    }
}

/// The table name after `ON` — upstream's grammar takes an optional
/// `TABLE` keyword there (`DEFINE INDEX i ON TABLE t …` is legal DDL
/// the engine executes; the printer always prints the bare form). The
/// keyword rides BARE: a quoted `` `table` `` lexes as an ident
/// upstream (`eat(t!("TABLE"))` wants the keyword token), so the raw
/// spelling decides and the name can never be eaten.
fn read_on_table(scan: &mut Scanner, body: &str) -> Option<String> {
    scan.skip_surrealql_trivia();
    let is_keyword = scan
        .peek_word()
        .is_some_and(|span| span.slice(body).eq_ignore_ascii_case("TABLE"));
    if is_keyword {
        scan.read_word();
    }
    read_bare_word(scan)
}

/// The decoded value of a string literal's source text (`None` when
/// the literal is malformed).
fn string_literal_value(text: &str) -> Option<String> {
    crate::lexer::lex_string_literal(text).map(|(value, _)| value)
}

fn read_bare_word(scan: &mut Scanner) -> Option<String> {
    scan.skip_surrealql_trivia();
    // read_ident reads bare, backtick-, angle-, and quote-wrapped
    // idents alike (the decoded text returns) and yields Ok(None) on a
    // non-ident — a keyword-spelled name arrives escaped in canon.
    Some(scan.read_ident().ok()??.1)
}

/// A standalone's canonical placement is nested: when its table
/// carries the same definition, the standalone folds away (the
/// duplication family). Comparison is value equality after the
/// standalone's own default-equal clauses elide — the same base both
/// sides of the merge produce.
fn drop_standalone_twins(
    skeleton: &mut Vec<Stmt>,
    skeleton_prov: &mut Vec<Option<usize>>,
    adoptees: &[Stmt],
) {
    let drop: Vec<bool> = skeleton
        .iter()
        .map(|stmt| {
            let (family, table_name) = match stmt {
                Stmt::Field(field) => (Family::Field, field.on.raw.clone()),
                Stmt::Index(index) => (Family::Index, index.on.raw.clone()),
                Stmt::Event(event) => (Family::Event, event.on.raw.clone()),
                _ => return false,
            };
            let table = skeleton
                .iter()
                .chain(adoptees.iter())
                .find(|stmt| matches!(stmt, Stmt::Table(table) if table.name.raw == table_name));
            let Some(Stmt::Table(table)) = table else {
                return false;
            };
            let elided = elide_and_fold(stmt.clone());
            let key = classify(&elided).1;
            table
                .nested
                .iter()
                .find(|nested| classify(nested) == (family, key.clone()))
                .is_some_and(|twin| same_definition(&elided, twin))
        })
        .collect();
    if drop.iter().any(|drop| *drop) {
        let (kept, kept_prov): (Vec<_>, Vec<_>) = std::mem::take(skeleton)
            .into_iter()
            .zip(std::mem::take(skeleton_prov))
            .zip(&drop)
            .filter_map(|((stmt, prov), drop)| (!drop).then_some((stmt, prov)))
            .unzip();
        *skeleton = kept;
        *skeleton_prov = kept_prov;
    }
}

/// Value equality of two field/index/event definitions, `on:`
/// excepted (the standalone carries it; the nested twin inherits the
/// host) and kinds compared under the algebra.
fn same_definition(a: &Stmt, b: &Stmt) -> bool {
    match (a, b) {
        (Stmt::Field(a), Stmt::Field(b)) => {
            a.name == b.name
                && kinds_algebra_equal(&a.kind, &b.kind)
                && a.default == b.default
                && a.value == b.value
                && a.assert == b.assert
                && a.computed == b.computed
                && a.readonly == b.readonly
                && a.flexible == b.flexible
                && a.reference == b.reference
                && a.permissions == b.permissions
                && a.graphql == b.graphql
                && a.comment == b.comment
        }
        (Stmt::Index(a), Stmt::Index(b)) => {
            a.name == b.name
                && a.kind == b.kind
                && a.fields == b.fields
                && a.concurrently == b.concurrently
                && a.comment == b.comment
        }
        (Stmt::Event(a), Stmt::Event(b)) => {
            a.name == b.name
                && a.when == b.when
                && a.then == b.then
                && a.r#async == b.r#async
                && a.comment == b.comment
        }
        _ => false,
    }
}

fn kinds_algebra_equal(a: &Option<Kind>, b: &Option<Kind>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => kind_text_equal(&a.text, &b.text),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The view's dependency set is the SELECT's FROM list — the
    /// engine's own `View.what` — read off the printer's dialect: the
    /// clause `FROM` stops the walk, WHERE/GROUP end the targets, a
    /// subquery's own FROM (parenthesized) never does, and a
    /// keyword-named table arrives escaped.
    #[test]
    fn view_tables_reads_the_from_list_off_the_printer_dialect() {
        let deps = |canon: &str| view_tables(canon);
        assert_eq!(
            deps("SELECT title, body FROM post, user WHERE user.id = post.author"),
            BTreeSet::from(["post".to_string(), "user".to_string()])
        );
        assert_eq!(
            deps("SELECT count() FROM sale GROUP BY country"),
            BTreeSet::from(["sale".to_string()])
        );
        // A subquery's FROM is parenthesized — depth guards it.
        assert_eq!(
            deps("SELECT * FROM recent WHERE id IN (SELECT id FROM flagged)"),
            BTreeSet::from(["recent".to_string()])
        );
        // A keyword-named table prints escaped (canon from the printer);
        // a field named `order` (idiom segment) is content, not a stop.
        assert_eq!(
            deps("SELECT order.line FROM `order`"),
            BTreeSet::from(["order".to_string()])
        );
        // Missing FROM (never canon — parse_view demands it) is empty.
        assert!(deps("SELECT 1").is_empty());
    }

    /// Raw bodies' `DEFINE TABLE`/`DEFINE ANALYZER` headers yield the
    /// dep-target key; other families (and prefixed spellings) ride.
    /// [`raw_match_key`] reads every family's header (the differ's
    /// match keys); placement filters to its dep-target subset.
    #[test]
    fn raw_headers_name_the_dep_target_families() {
        assert_eq!(
            raw_match_key("DEFINE TABLE user TYPE ANY SCHEMALESS"),
            Some((Family::Table, "user".to_string()))
        );
        assert_eq!(
            raw_match_key("DEFINE TABLE OVERWRITE `order` TYPE ANY"),
            Some((Family::Table, "order".to_string()))
        );
        assert_eq!(
            raw_match_key("DEFINE TABLE IF NOT EXISTS ⟨a⟩ PERMISSIONS NONE"),
            Some((Family::Table, "a".to_string()))
        );
        assert_eq!(
            raw_match_key("DEFINE ANALYZER like TOKENIZERS CLASS"),
            Some((Family::Analyzer, "like".to_string()))
        );
        // The differ's keys: every family's header shape.
        assert_eq!(
            raw_match_key("DEFINE INDEX i ON t FIELDS v"),
            Some((Family::Index, "i".to_string()))
        );
        assert_eq!(
            raw_match_key("DEFINE FIELD m.* ON t TYPE int"),
            Some((Family::Field, "m.*".to_string()))
        );
        assert_eq!(
            raw_match_key("DEFINE FUNCTION fn::a::b($x: int) {;}"),
            Some((Family::Function, "a::b".to_string()))
        );
        assert_eq!(
            raw_match_key("DEFINE PARAM $p VALUE 1"),
            Some((Family::Param, "p".to_string()))
        );
        assert_eq!(
            raw_match_key("DEFINE API '/x' FOR any"),
            Some((Family::Api, "/x".to_string()))
        );
        assert_eq!(
            raw_match_key("DEFINE CONFIG GRAPHQL INTROSPECTION NONE"),
            Some((Family::Config, "graphql".to_string()))
        );
        // An anonymous silo module carries the empty name (classify's
        // own shape); a named one carries it.
        assert_eq!(
            raw_match_key("DEFINE MODULE FROM silo::o::p::1.0.0 UNSIGNED"),
            Some((Family::Module, String::new()))
        );
        assert_eq!(
            raw_match_key("DEFINE MODULE m FROM silo::o::p::1.0.0 UNSIGNED"),
            Some((Family::Module, "m".to_string()))
        );
        // The define-kind prefixes skip on EVERY family (authored raw
        // may carry them; INFO never prints them).
        assert_eq!(
            raw_match_key("DEFINE PARAM OVERWRITE $p VALUE 1"),
            Some((Family::Param, "p".to_string()))
        );
        assert_eq!(
            raw_match_key("DEFINE FIELD IF NOT EXISTS m ON t TYPE int"),
            Some((Family::Field, "m".to_string()))
        );
        assert_eq!(
            raw_match_key("DEFINE CONFIG OVERWRITE DEFAULT namespace 'a';"),
            Some((Family::Config, "default".to_string()))
        );
        assert_eq!(
            raw_match_key("DEFINE FUNCTION OVERWRITE fn::go() {;}"),
            Some((Family::Function, "go".to_string()))
        );
        // A non-header body has no key.
        assert_eq!(raw_match_key("SELECT 1"), None);
        // An index or event raw needs its ON table instead
        // (raw_dep_names) — placement's own dependency edge.
        assert_eq!(
            raw_dep_names("DEFINE INDEX i ON t FIELDS v HNSW DIMENSION 3"),
            vec![(Family::Table, "t".to_string())]
        );
        // The optional TABLE keyword after ON never reads as the name
        // (legal upstream DDL; the printer always prints bare).
        assert_eq!(
            raw_dep_names("DEFINE INDEX i ON TABLE t FIELDS v"),
            vec![(Family::Table, "t".to_string())]
        );
        assert_eq!(
            raw_match_key("DEFINE FIELD f ON TABLE t;").map(|(_, name)| name),
            Some("f".to_string())
        );
        assert_eq!(
            raw_on_table("DEFINE FIELD f ON TABLE t;"),
            Some("t".to_string())
        );
        assert_eq!(
            raw_on_table("DEFINE EVENT e ON TABLE t WHEN true THEN CREATE log"),
            Some("t".to_string())
        );
        // The keyword rides bare: a quoted ident spelling is a name
        // (upstream eats the TABLE keyword token, not an ident).
        assert_eq!(
            raw_dep_names("DEFINE INDEX i ON `table` FIELDS v"),
            vec![(Family::Table, "table".to_string())]
        );
        assert_eq!(
            raw_dep_names("DEFINE EVENT e ON t WHEN true THEN CREATE log"),
            vec![(Family::Table, "t".to_string())]
        );
        // A view-carrying raw table reads its FROM list; a non-view
        // table has none.
        assert_eq!(
            raw_dep_names("DEFINE TABLE recent TYPE ANY SCHEMALESS AS SELECT * FROM post, user"),
            vec![
                (Family::Table, "post".to_string()),
                (Family::Table, "user".to_string())
            ]
        );
        assert!(raw_dep_names("DEFINE TABLE plain TYPE ANY SCHEMALESS").is_empty());
        // Deviations yield no edge.
        assert!(raw_dep_names("DEFINE INDEX i FIELDS v").is_empty());
    }

    /// Commas inside quoted idents are not separators.
    #[test]
    fn top_level_comma_split_honors_quoted_wrappers() {
        assert_eq!(
            split_top_level_commas("a, `b,c`, ⟨d,e⟩, f"),
            vec!["a", " `b,c`", " ⟨d,e⟩", " f"]
        );
    }

    /// A prefixed raw header keeps its
    /// dependency edge — the embedded parser consumes the define-kind
    /// prefixes for the typed AST read, whatever the authored
    /// spelling.
    #[test]
    fn raw_dep_names_rides_the_define_prefixes() {
        assert_eq!(
            raw_dep_names("DEFINE INDEX IF NOT EXISTS i ON t FIELDS v"),
            vec![(Family::Table, "t".to_string())]
        );
        assert_eq!(
            raw_dep_names("DEFINE INDEX OVERWRITE i ON t FIELDS v"),
            vec![(Family::Table, "t".to_string())]
        );
        assert_eq!(
            raw_dep_names("DEFINE EVENT OVERWRITE e ON t THEN CREATE log"),
            vec![(Family::Table, "t".to_string())]
        );
    }

    /// The value-slot reader: every reader
    /// family's define-time slots yield the Param/Fn/Module reads —
    /// printed form or authored clause order, trailing clauses
    /// included — and non-reader families (and unparseable bodies)
    /// read nothing.
    #[test]
    fn raw_value_reads_walks_the_reader_families_define_time_slots() {
        let reads = |body: &str| raw_value_reads(body);
        // Param VALUE — evaluated at define; trailing PERMISSIONS ride.
        assert_eq!(
            reads("DEFINE PARAM OVERWRITE $a VALUE $b + fn::seed() PERMISSIONS NONE"),
            vec![
                (Family::Param, "b".to_string()),
                (Family::Function, "seed".to_string())
            ]
        );
        // Sequence — all three computed slots; TIMEOUT prints only
        // when set, so the authored form carries it.
        assert_eq!(
            reads("DEFINE SEQUENCE s BATCH $batch START $start TIMEOUT $t"),
            vec![
                (Family::Param, "batch".to_string()),
                (Family::Param, "start".to_string()),
                (Family::Param, "t".to_string())
            ]
        );
        // User DURATION sides (the printer always materializes both).
        assert_eq!(
            reads(
                "DEFINE USER legacy ON ROOT PASSWORD 'hunter2' ROLES OWNER \
                 DURATION FOR TOKEN 15m, FOR SESSION $sess"
            ),
            vec![(Family::Param, "sess".to_string())]
        );
        // Access — the jwt URL (trigger B's silent class) and
        // the duration sides.
        assert_eq!(
            reads(
                "DEFINE ACCESS ju ON DATABASE TYPE JWT URL $p \
                 DURATION FOR SESSION $sess"
            ),
            vec![
                (Family::Param, "p".to_string()),
                (Family::Param, "sess".to_string())
            ]
        );
        // Access — the local-key shape and the issuer key.
        assert_eq!(
            reads(
                "DEFINE ACCESS kv ON DATABASE TYPE JWT \
                 ALGORITHM HS512 KEY $verify WITH ISSUER KEY $issue"
            ),
            vec![
                (Family::Param, "verify".to_string()),
                (Family::Param, "issue".to_string())
            ]
        );
        // Bucket BACKEND — computed + coerced at define (the
        // hard-coercion class).
        assert_eq!(
            reads("DEFINE BUCKET b BACKEND $be"),
            vec![(Family::Param, "be".to_string())]
        );
        // Api middleware args — the statement form; the fn NAME stores
        // unvalidated (no edge to it).
        assert_eq!(
            reads("DEFINE API '/x' FOR any MIDDLEWARE fn::mw($limit) PERMISSIONS FULL"),
            vec![(Family::Param, "limit".to_string())]
        );
        // DEFINE CONFIG API — the config form's own middleware list.
        assert_eq!(
            reads("DEFINE CONFIG OVERWRITE API MIDDLEWARE fn::mw($x), fn::log()"),
            vec![(Family::Param, "x".to_string())]
        );
        // Config DEFAULT — the computed sides.
        assert_eq!(
            reads("DEFINE CONFIG DEFAULT NAMESPACE $ns DATABASE 'main'"),
            vec![(Family::Param, "ns".to_string())]
        );
        // mod:: calls read as MODULE targets (the row 6 fn-call class).
        assert_eq!(
            reads("DEFINE PARAM OVERWRITE $a VALUE mod::seed()"),
            vec![(Family::Module, "seed".to_string())]
        );
        // No define-time slot, no reader family, unparseable body:
        // nothing.
        assert!(reads("DEFINE TABLE OVERWRITE t SCHEMALESS").is_empty());
        assert!(reads("DEFINE ANALYZER like TOKENIZERS CLASS").is_empty());
        assert!(reads("SELECT 1").is_empty());
        assert!(reads("DEFINE PARAM $broken VALUE ===;").is_empty());
    }

    /// The value-slot reader walks EVERY statement of a
    /// multi-statement body: the union of the statements' reads is the
    /// body's edge set, wherever each reader statement stands — and a
    /// statement that is not a DEFINE leaves the others' standing
    /// (union what parses; a target carried by two statements is one
    /// edge).
    #[test]
    fn raw_value_reads_walks_every_statement_of_a_multistatement_body() {
        let reads = |body: &str| raw_value_reads(body);
        // The access's jwt url (statement 1) and the param's VALUE
        // (statement 3) union; the table between them carries no
        // slots. Access < Param is the partition order that ran the
        // reader first pre-fix — the union is what sees both.
        assert_eq!(
            reads(
                "DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $p; \
                 DEFINE TABLE OVERWRITE zzz SCHEMALESS; \
                 DEFINE PARAM OVERWRITE $a VALUE $b + 1;",
            ),
            vec![
                (Family::Param, "p".to_string()),
                (Family::Param, "b".to_string())
            ]
        );
        // A non-DEFINE statement between the defines does not zero the
        // others' reads.
        assert_eq!(
            reads("RETURN 1; DEFINE PARAM OVERWRITE $a VALUE $b + 1;"),
            vec![(Family::Param, "b".to_string())]
        );
        // The same target carried by two statements is one edge.
        assert_eq!(
            reads(
                "DEFINE PARAM OVERWRITE $a VALUE $b; \
                 DEFINE ACCESS OVERWRITE ju ON DATABASE TYPE JWT URL $b;",
            ),
            vec![(Family::Param, "b".to_string())]
        );
        // An unparseable body reads nothing — the whole-body
        // fail-closed position (the engine refuses it at parse time).
        assert!(reads("DEFINE PARAM $ok VALUE 1; DEFINE PARAM $x VALUE ===;").is_empty());
    }

    /// The placement reader walks EVERY statement of a
    /// multi-statement body: an index's `ON` and a view's `FROM`
    /// union, wherever each statement stands.
    #[test]
    fn raw_dep_names_walks_every_statement_of_a_multistatement_body() {
        assert_eq!(
            raw_dep_names(
                "DEFINE PARAM OVERWRITE $a VALUE 1; \
                 DEFINE INDEX OVERWRITE ix ON t FIELDS a; \
                 DEFINE TABLE OVERWRITE v AS SELECT x FROM post, user;",
            ),
            vec![
                (Family::Table, "t".to_string()),
                (Family::Table, "post".to_string()),
                (Family::Table, "user".to_string())
            ]
        );
        // The unparseable body keeps the whole-body fail-closed
        // position (no edge — the engine refuses it at parse time).
        assert!(raw_dep_names("DEFINE INDEX i ON t FIELDS ===;").is_empty());
    }

    /// The analyzer reader walks EVERY statement: one clause per
    /// fulltext index statement, wherever it stands (two statements
    /// naming the same analyzer are one edge).
    #[test]
    fn raw_index_analyzer_walks_every_statement_of_a_multistatement_body() {
        assert_eq!(
            raw_index_analyzer(
                "DEFINE PARAM OVERWRITE $p VALUE 1; \
                 DEFINE INDEX OVERWRITE ix ON t FIELDS a FULLTEXT ANALYZER arts; \
                 DEFINE INDEX OVERWRITE iy ON u FIELDS b FULLTEXT ANALYZER bm;",
            ),
            vec!["arts".to_string(), "bm".to_string()]
        );
        // A non-fulltext index (and an unparseable body) reads
        // nothing.
        assert!(raw_index_analyzer("DEFINE INDEX OVERWRITE ix ON t FIELDS a;").is_empty());
        assert!(raw_index_analyzer("DEFINE INDEX i ON t ANALYZER ===;").is_empty());
    }
}
