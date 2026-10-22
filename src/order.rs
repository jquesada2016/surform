//! The declarative namespace's semantics (spec/README.md rule 7): the
//! FROZEN define-time dependency-edge table, the duplicate-definition
//! rule (SDL-E1062, every family), the chain-named cycle diagnostic
//! (SDL-E0015), and the compiler-owned emission order — the stable
//! topological sort `build` emits under.
//!
//! The model — "the declarative namespace": one
//! namespace, resolved lazily; the compiler derives application
//! order; errors on true ambiguity and true cycles. Authored order is
//! presentation: the emitted DDL orders itself so the engine accepts
//! it, and a definition may stand anywhere in the stream — above,
//! below, or in another file of the composition.
//!
//! ## The frozen edge table
//!
//! The sort runs over EXACTLY the slots the pinned 3.3.0 engine
//! evaluates when the DEFINE statement itself executes
//! (`define_statement_compute`, `legacy/expr/statements/define/*`);
//! every other slot is stored verbatim and evaluated at runtime,
//! which is EDGE-FREE by the representability guarantee (a
//! fn referencing a table referencing a fn, in any arrangement, is no
//! error and no finding). Each row: the slot, the define-time
//! behavior when its reference is missing, and the verdict.
//!
//! | # | statement → slot | define-time behavior | verdict |
//! |---|---|---|---|
//! | 1 | view → top `FROM` tables | hard `TbNotFound` (`define/table.rs:251-269`, `view.source_tables()`) | EDGE, cycle class view↔view |
//! | 2 | view body (WHERE, projections, subqueries) | executed once at define for the initial materialization, but data-conditioned — fails only when source rows exist | no edge (canonical order still safe) |
//! | 3 | relation `IN`/`OUT` endpoints | stored verbatim, typed auto-fields; no existence check | NO EDGE |
//! | 4 | fulltext index → its analyzer | `AzNotFound` at define when the table holds records; stored dangling + first-write failure otherwise (the build is blocking unless `concurrently`) | EDGE (build-conditioned) |
//! | 4b | index → the fields it names | hard `FdNotFound` at define on a SCHEMAFULL table; no check on schemaless | EDGE (schema-conditioned; we order unconditionally) |
//! | 5 | analyzer → `function: fn::…` | stored verbatim, resolved lazily at use time | NO EDGE |
//! | 6 | param `VALUE <expr>` | evaluated, result stored: a forward `$param` silently stores `none`; an undefined `fn::` is hard `FcNotFound` | EDGE, cycle class param↔param |
//! | 7 | sequence `START`/`BATCH`/`TIMEOUT` | all three computed at define; a missing param is a hard cast error on START/BATCH, silent `NONE` on TIMEOUT | EDGE (value) |
//! | 8 | user `DURATION` sides | computed at define; missing param silently `NONE`, undefined fn hard | EDGE (value) |
//! | 9 | access jwt `url:`/`key:`/issuer `key:` + `duration:` sides | computed at define: `key:` hard-coerces (missing param errors), `url:` silently stores the string `'NONE'` | EDGE (value) |
//! | 10 | access signup/signin/authenticate/context | stored, runtime-evaluated (context's define check is shape-only) | NO EDGE |
//! | 11 | bucket `backend:` | computed + coerced at define; missing param is a hard error | EDGE (value) |
//! | 12 | api middleware ARGS | computed at define, values stored (the middleware fn NAME is stored unvalidated — no edge to it) | EDGE (value) |
//! | 13 | config default `namespace:`/`database:` | computed at define for non-ident spellings (bare idents store verbatim); missing param silently none | EDGE (value, soft) |
//! | 14 | config graphql families | stored verbatim | NO EDGE |
//! | 15 | field kinds (`record<t>`/`table<t>`) | stored verbatim, no existence check | NO EDGE |
//! | 16–20 | permissions, field VALUE/ASSERT/DEFAULT/COMPUTED, events, fn bodies, `reference on delete` | runtime-evaluated (per-write, per-request, per-purge) | NO EDGE |
//!
//! Two probe-pinned facts bound the member forms: a standalone
//! `DEFINE INDEX`/`DEFINE EVENT`/`DEFINE FIELD` AUTO-CREATES its
//! `on:` table when missing (schemaless shell), so there is no
//! member→host edge; and statements in one query commit per
//! statement, so emission order is exactly what the engine sees.
//!
//! ## The two passes
//!
//! - [`check_document`] — the check-class pass (every entry that
//!   resolves spreads: `check`, `build`, `lint`, `diff`, `normalize`,
//!   the playground): duplicate definitions (SDL-E1062), true cycles
//!   (SDL-E0015), and unplaceable cross-section dependencies
//!   (SDL-E0016), over the AUTHORED stream. fmt never runs it.
//! - [`emission_order`] — the stable topological sort over the
//!   LOWERED (spread-expanded) stream, inside `codegen::emit`:
//!   sections partitioned at the scope statements (a `namespace`/
//!   `database` never moves), nodes ordered by the edges with
//!   authored order as the tie-break, so the sort is deterministic
//!   and an already-ordered document emits byte-identically. Intra-
//!   table blocks sort their nested members the same way (fields
//!   before the indexes that name them). The sort always terminates:
//!   cycle participants fall back to authored order (unreachable
//!   after [`check`], but `emit` stays total for the library
//!   caller). fmt keeps its never-reorder contract.
//!
//! Resolution of an edge target is scope-keyed
//! ([`crate::info::normalize::ScopeWalk`] — the one scope home) and
//! names the FIRST definition of the key; a target nothing defines is
//! no edge (the advisory lints' territory — out-of-document targets
//! stay legal). Cross-scope references get no edge: they cannot
//! resolve in the engine either.

use std::collections::{BTreeSet, HashMap};

use surrealdb_sql::Expr as SqlExpr;

use crate::ast::{AccessKind, Config, IndexKind, JwtSource, JwtVerify, SdlFile, Stmt, TableKind};
use crate::diag::Diag;
use crate::info::normalize::{Family, ScopeKey, ScopeWalk, classify, field_path_key};
use crate::lexer::Span;

// ---------------------------------------------------------------------
// The frozen edge extraction
// ---------------------------------------------------------------------

/// One edge target read off a statement's define-time slots.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// A view's top `FROM` table (row 1).
    Table(String),
    /// A fulltext index's analyzer (row 4).
    Analyzer(String),
    /// A param read in a define-time value slot (rows 6–13).
    Param(String),
    /// A fn call in a define-time value slot (rows 6–13).
    Fn(String),
    /// A `mod::` call in a define-time value slot — the engine
    /// resolves `mod::<first-segment>` at define (row 6's fn-call
    /// class, the module family's spelling).
    Module(String),
    /// A field an index names (row 4b) — `host` scopes it to the
    /// table both statements speak of (standalone forms carry their
    /// `on:`; a nested index resolves inside its own block).
    Field { host: String, name: String },
}

/// The define-time edges of one top-level statement (the frozen
/// table's rows; a table's nested fulltext indexes pin their
/// analyzers on the ENCLOSING statement — the block emits as a unit).
/// Structural paths that merely ride a statement (an api's middleware
/// NAMES, a graphql family's fns, an analyzer's function) are
/// deliberately absent — rows 5/12/14 store them unvalidated.
fn stmt_edges(stmt: &Stmt, out: &mut Vec<Target>) {
    match stmt {
        Stmt::Table(table) => {
            if let TableKind::View { query } = &table.kind {
                for name in view_from_tables(&query.canon) {
                    out.push(Target::Table(name));
                }
            }
            for member in &table.nested {
                if let Stmt::Index(idx) = member {
                    index_edges(idx, out);
                }
            }
        }
        Stmt::Index(idx) => index_edges(idx, out),
        Stmt::Param(param) => {
            if let Some(value) = &param.value {
                capture_edges(&value.canon, out);
            }
        }
        Stmt::Sequence(sequence) => {
            for expr in [&sequence.start, &sequence.batch, &sequence.timeout]
                .into_iter()
                .flatten()
            {
                capture_edges(&expr.canon, out);
            }
        }
        Stmt::User(user) => {
            for expr in [&user.token_duration, &user.session_duration]
                .into_iter()
                .flatten()
            {
                capture_edges(&expr.canon, out);
            }
        }
        Stmt::Bucket(bucket) => {
            if let Some(backend) = &bucket.backend {
                capture_edges(&backend.canon, out);
            }
        }
        Stmt::Access(access) => {
            jwt_edges(&access.kind, out);
            if let Some(duration) = &access.duration {
                for expr in [&duration.grant, &duration.token, &duration.session]
                    .into_iter()
                    .flatten()
                {
                    capture_edges(&expr.canon, out);
                }
            }
        }
        Stmt::Config(config) => match config {
            Config::Default(default) => {
                for expr in [&default.namespace, &default.database]
                    .into_iter()
                    .flatten()
                {
                    capture_edges(&expr.canon, out);
                }
            }
            Config::Api(api) => middleware_arg_edges(&api.middleware, out),
            Config::Graphql(_) => {}
        },
        Stmt::Api(api) => {
            for action in &api.actions {
                middleware_arg_edges(&action.config.middleware, out);
            }
            if let Some(any) = &api.any {
                middleware_arg_edges(&any.config.middleware, out);
            }
        }
        // No define-time slots (rows 3/5/10/14–20): namespace,
        // database, analyzer, function, module, mixin, event, field,
        // raw, include.
        _ => {}
    }
}

fn middleware_arg_edges(middleware: &[crate::ast::Middleware], out: &mut Vec<Target>) {
    for entry in middleware {
        for arg in &entry.args {
            capture_edges(&arg.canon, out);
        }
    }
}

/// Rows 4 + 4b for one index statement (the `on:` host carries the
/// field targets' table).
fn index_edges(idx: &crate::ast::Index, out: &mut Vec<Target>) {
    if let IndexKind::FullText {
        analyzer: Some(analyzer),
        ..
    } = &idx.kind
    {
        out.push(Target::Analyzer(analyzer.raw.clone()));
    }
    for field in &idx.fields {
        if let Some(name) = field_path_candidate(&field.canon) {
            out.push(Target::Field {
                host: idx.on.raw.clone(),
                name,
            });
        }
    }
}

/// The jwt source's define-time slots (row 9) — url, verify key,
/// issuer key. The audience list is string literals only (the
/// licensed deviation), so it carries no edges; the record/bearer
/// signup/signin/authenticate/context slots are runtime (row 10).
fn jwt_edges(kind: &AccessKind, out: &mut Vec<Target>) {
    match kind {
        AccessKind::Jwt { source } => jwt_source_edges(source, out),
        AccessKind::Record { jwt, .. } | AccessKind::Bearer { jwt, .. } => {
            if let Some(source) = jwt {
                jwt_source_edges(source, out);
            }
        }
    }
}

fn jwt_source_edges(source: &JwtSource, out: &mut Vec<Target>) {
    let slot = match &source.verify {
        JwtVerify::Url(expr) => Some(expr),
        JwtVerify::Key { key, .. } => Some(key),
    };
    if let Some(expr) = slot {
        capture_edges(&expr.canon, out);
    }
    if let Some(issuer) = &source.issuer
        && let Some(key) = &issuer.key
    {
        capture_edges(&key.canon, out);
    }
}

/// One capture's Param/Fn reads through the extractor's Tier-2 walk
/// (rows 6–13: the slots are exactly the extractor's define-time
/// sites, so its name spelling is the definition spelling).
fn capture_edges(canon: &str, out: &mut Vec<Target>) {
    let (params, fns, modules) = crate::refs::capture_param_fn_refs(canon);
    out.extend(params.into_iter().map(Target::Param));
    out.extend(fns.into_iter().map(Target::Fn));
    out.extend(modules.into_iter().map(Target::Module));
}

/// The view's top `FROM` tables — the engine's own define-validated
/// set (row 1). An unreadable canon (never the printer's shape) is
/// muted: no edges, never a guess.
fn view_from_tables(canon: &str) -> Vec<String> {
    let Some(select) = crate::refs::view_select(canon) else {
        return Vec::new();
    };
    select
        .what
        .iter()
        .filter_map(|target| match target {
            SqlExpr::Table(name) => Some(name.as_str().to_string()),
            _ => None,
        })
        .collect()
}

/// The field-path candidate one index `fields:` entry's canon names —
/// row 4b's read. The printer's ident quotes decode away (`` `b c` ``
/// → `b c`, `a.\`b c\`` → `a.b c`, `tags.*` stays `tags.*`: a quoted
/// segment is spelling, not classification); the surviving text must
/// be free of EXPRESSION machinery (calls, operators, literals,
/// graph steps) — those entries get no edge, and authored order
/// governs them. The edge itself fires only where the candidate
/// EQUALS a sibling field's [`field_path_key`], so a pathological
/// coincidence degrades to a harmless extra edge, never a wrong one.
fn field_path_candidate(canon: &str) -> Option<String> {
    let trimmed = canon.trim();
    let plain: String = trimmed.chars().filter(|&c| c != '`').collect();
    if plain.is_empty() {
        return None;
    }
    // Machinery a field path never carries. Everything else —
    // alphanumerics, `_`, `.`, the `*` wildcard, `-`, spaces inside
    // quoted segments — is ident text the sibling compare decides.
    const MACHINERY: &str = "()[],:;'\"$@=<>!&|+/%^~?{}";
    if plain.chars().any(|c| MACHINERY.contains(c)) {
        return None;
    }
    Some(plain)
}

/// Row 4b's intra-block form: the (nested index position, field name)
/// pairs one table block's members pin. Only simple entries
/// ([`field_path_candidate`]) edge; the sibling field's spelling is
/// [`field_path_key`]'s.
fn nested_edges(members: &[Stmt]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (at, member) in members.iter().enumerate() {
        if let Stmt::Index(idx) = member {
            for field in &idx.fields {
                if let Some(name) = field_path_candidate(&field.canon) {
                    out.push((at, name));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------
// Definitions — the (scope, family, key) map both passes share
// ---------------------------------------------------------------------

/// A definition's identity. Mixins resolve against the composition's
/// GLOBAL set (the resolution pass's own shape — no scope), so their
/// key carries [`KeyScope::Global`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DefKey {
    scope: KeyScope,
    family: Family,
    key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum KeyScope {
    Global,
    Scoped(ScopeKey),
}

/// The definitions one stream registers, in first-occurrence order
/// (duplicates attach to the SECOND). Namespace/database statements
/// are scope anchors, not definitions (re-opening a scope is how a
/// composition interleaves); raw/include/spread entries register
/// nothing.
struct Definitions {
    first: HashMap<DefKey, usize>,
    duplicates: Vec<(usize, DefKey)>,
}

impl Definitions {
    fn new() -> Self {
        Definitions {
            first: HashMap::new(),
            duplicates: Vec::new(),
        }
    }

    fn register(&mut self, walk: &ScopeWalk, stmt: &Stmt, at: usize) {
        if let Some((family, key)) = def_ident(stmt) {
            self.register_key(walk, stmt, at, family, key);
        }
        // Table-block members deliberately stay OUT of the registry:
        // a standalone member and its table's nested twin are the
        // twin-fold workflow's INPUT (normalize folds the exact twin,
        // L0007 nudges the nesting), so the pair is legal SDL whose
        // fold is designed — issue #48's ledgered residue, not the
        // duplicates rule. Mixin members are the same on a second
        // count: their host is whichever table spreads the mixin, and
        // two mixins may legally carry one member key (the
        // explicit-overshadow resolution owns the landing).
    }

    fn register_key(
        &mut self,
        walk: &ScopeWalk,
        stmt: &Stmt,
        at: usize,
        family: Family,
        key: String,
    ) {
        let scope = match family {
            Family::Mixin => KeyScope::Global,
            _ => KeyScope::Scoped(walk.lookup_scope(stmt)),
        };
        let id = DefKey { scope, family, key };
        use std::collections::hash_map::Entry;
        match self.first.entry(id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(at);
            }
            Entry::Occupied(_) => self.duplicates.push((at, id)),
        }
    }

    fn resolve(&self, walk: &ScopeWalk, stmt: &Stmt, target: &Target) -> Option<usize> {
        // A user/access's define-time slots (durations, jwt url/key)
        // evaluate in the SESSION context the DDL's USE lines bind —
        // the section's scope, not the statement's catalog level (its
        // `on:`, which is what lookup_scope — and the duplicate
        // registry — key it by). Everything else resolves at its
        // catalog scope.
        let scope = KeyScope::Scoped(match stmt {
            Stmt::User(_) | Stmt::Access(_) => walk.scope.clone(),
            _ => walk.lookup_scope(stmt),
        });
        let (family, key) = match target {
            Target::Table(name) => (Family::Table, name.clone()),
            Target::Analyzer(name) => (Family::Analyzer, name.clone()),
            Target::Param(name) => (Family::Param, name.clone()),
            Target::Fn(name) => (Family::Function, name.clone()),
            Target::Module(name) => (Family::Module, name.clone()),
            Target::Field { host, name } => (Family::Field, format!("{host}::{name}")),
        };
        self.first.get(&DefKey { scope, family, key }).copied()
    }
}

/// A statement's (family, key) as the order pass keys it: the merge's
/// [`classify`], with the standalone member forms (field/index/event)
/// host-qualified — two fields named `x` on different tables are
/// different definitions. `None` for the statements that are no
/// definitions at all: the scope anchors, raw bodies, includes, and
/// spread entries (a spread REFERENCES its mixin; the mixin
/// statement itself is the definition).
fn def_ident(stmt: &Stmt) -> Option<(Family, String)> {
    match stmt {
        Stmt::Namespace(_)
        | Stmt::Database(_)
        | Stmt::Raw(_)
        | Stmt::Include(_)
        | Stmt::Spread(_) => None,
        Stmt::Field(field) => Some((
            Family::Field,
            format!("{}::{}", field.on.raw, field_path_key(&field.name)),
        )),
        Stmt::Index(idx) => Some((Family::Index, format!("{}::{}", idx.on.raw, idx.name.raw))),
        Stmt::Event(event) => Some((
            Family::Event,
            format!("{}::{}", event.on.raw, event.name.raw),
        )),
        // A module's identity: a named file module keys by its header
        // name; a silo module (self-named, `name: None`) keys by its
        // executable's `org::pkg::<maj.min.patch>` — the catalog name
        // a `silo::` call resolves. An anonymous FILE module is
        // parse-rejected, so it never reaches here from a clean
        // parse; register nothing rather than key two silos alike.
        Stmt::Module(module) => match (&module.name, &module.executable) {
            (Some(name), _) => Some((Family::Module, name.raw.clone())),
            (
                None,
                crate::ast::Executable::Silo {
                    org,
                    pkg,
                    major,
                    minor,
                    patch,
                },
            ) => Some((
                Family::Module,
                format!("{org}::{pkg}::<{major}.{minor}.{patch}>"),
            )),
            (None, crate::ast::Executable::File { .. }) => None,
        },
        other => Some(classify(other)),
    }
}

/// The message word for a family ("table `users` is already
/// defined").
fn family_word(family: Family) -> &'static str {
    match family {
        Family::Access => "access",
        Family::Analyzer => "analyzer",
        Family::Api => "api",
        Family::Bucket => "bucket",
        Family::Comment | Family::Include | Family::Raw => "statement",
        Family::Config => "config",
        Family::Database => "database",
        Family::Event => "event",
        Family::Field => "field",
        Family::Function => "function",
        Family::Index => "index",
        Family::Mixin => "mixin",
        Family::Module => "module",
        Family::Namespace => "namespace",
        Family::Param => "param",
        Family::Sequence => "sequence",
        Family::Table => "table",
        Family::User => "user",
    }
}

/// A cycle chain's word for one statement ("view `a`", "param `$x`").
fn node_word(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Table(table) => {
            if matches!(table.kind, TableKind::View { .. }) {
                format!("view `{}`", table.name.raw)
            } else {
                format!("table `{}`", table.name.raw)
            }
        }
        Stmt::Param(param) => format!("param `${}`", param.name.raw),
        Stmt::Function(function) => format!("fn `{}`", function.name.segments.join("::")),
        Stmt::Analyzer(analyzer) => format!("analyzer `{}`", analyzer.name.raw),
        other => match def_ident(other) {
            Some((family, key)) => format!("{} `{}`", family_word(family), key),
            None => "statement".to_string(),
        },
    }
}

// ---------------------------------------------------------------------
// The check pass — duplicates (E1062) and true cycles (E0015)
// ---------------------------------------------------------------------

/// The WHOLE check-class contract in one call — every entry that
/// resolves spreads runs exactly this (`check`, `build`, `lint`,
/// `diff`, `normalize`, the playground's check and lint): the global
/// spread resolution (SDL-E1061 and the resolution-owned validations)
/// plus this module's duplicates (SDL-E1062) and true cycles
/// (SDL-E0015), over the AUTHORED stream. One home, so a future
/// check-class pass cannot be wired into one consumer and forgotten
/// in another. `item_homes` tags each diagnostic with the home whose
/// text its span renders against (empty = one file); `item_spans`
/// runs parallel to `items` — each statement's authored span (the
/// loader derives it from the home scans, the single-file entries
/// from [`crate::lint::scan_source`]). fmt never calls this.
pub fn check_document(
    items: &[Stmt],
    item_homes: &[usize],
    item_spans: &[Span],
) -> Vec<(Diag, usize)> {
    let mut out = crate::parser::resolve_spreads_homed(items, item_homes);
    out.extend(check(items, item_homes, item_spans));
    out
}

/// The order half of the check-class contract: every duplicate
/// definition (SDL-E1062, any file any position — the parse's own
/// in-file mixin registry aside), every define-time cycle
/// (SDL-E0015, the chain named), and every unplaceable cross-section
/// dependency (SDL-E0016). Private to the module's own tests —
/// consumers call [`check_document`], which runs the spread
/// resolution too.
fn check(items: &[Stmt], item_homes: &[usize], item_spans: &[Span]) -> Vec<(Diag, usize)> {
    debug_assert_eq!(items.len(), item_spans.len(), "spans run parallel to items");
    let home_of = |at: usize| item_homes.get(at).copied().unwrap_or(0);
    let mut out: Vec<(Diag, usize)> = Vec::new();

    let mut walk = ScopeWalk::new();
    let mut definitions = Definitions::new();
    for (at, stmt) in items.iter().enumerate() {
        definitions.register(&walk, stmt, at);
        walk.advance(stmt);
    }
    for (at, id) in &definitions.duplicates {
        out.push((
            Diag::DuplicateDefinition {
                span: item_spans[*at],
                family: family_word(id.family),
                name: display_name(&items[*at]),
            },
            home_of(*at),
        ));
    }

    // The edge graph: statement → statement, resolved against the
    // first definition of each key — over the LOWERED stream, because
    // a mixin's members carry define-time edges (an index's fields,
    // its analyzer) that only exist once the spreads materialize into
    // their host tables. Expansion preserves top-level order minus
    // the mixin statements, so each lowered index maps to the authored
    // index whose span and home its findings render against.
    let lowered = crate::expand::expand(&crate::ast::SdlFile {
        items: items.to_vec(),
        trailing_comments: Vec::new(),
    });
    let lowered_to_authored: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, stmt)| !matches!(stmt, Stmt::Mixin(_)))
        .map(|(at, _)| at)
        .collect();
    debug_assert_eq!(lowered_to_authored.len(), lowered.items.len());
    let mut lowered_definitions = Definitions::new();
    let mut lowered_walk = ScopeWalk::new();
    for (at, stmt) in lowered.items.iter().enumerate() {
        lowered_definitions.register(&lowered_walk, stmt, at);
        lowered_walk.advance(stmt);
    }
    let mut targets: Vec<Vec<Target>> = vec![Vec::new(); lowered.items.len()];
    for (at, stmt) in lowered.items.iter().enumerate() {
        stmt_edges(stmt, &mut targets[at]);
    }
    let mut resolved: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    let mut walk = ScopeWalk::new();
    for (at, stmt) in lowered.items.iter().enumerate() {
        for target in &targets[at] {
            if let Some(dep) = lowered_definitions.resolve(&walk, stmt, target) {
                // A definition reading ITSELF: a view is define-legal
                // (the engine inserts the table before the FROM
                // check — replace_tb precedes source_tables()), but a
                // param self-read is a length-1 cycle no order
                // applies, so only the param self-edge stays for the
                // cycle pass. The emission sort drops every
                // self-edge; the check refuses the document first.
                let self_edge = dep == at;
                if (!self_edge || matches!(target, Target::Param(_)))
                    && !resolved[lowered_to_authored[at]].contains(&lowered_to_authored[dep])
                {
                    resolved[lowered_to_authored[at]].push(lowered_to_authored[dep]);
                }
            }
        }
        walk.advance(stmt);
    }
    let cycles = find_cycles(&resolved);
    for chain in &cycles {
        let anchor = *chain.iter().min().expect("a cycle holds nodes");
        let mut names: Vec<String> = chain.iter().map(|&at| node_word(&items[at])).collect();
        names.push(node_word(&items[chain[0]]));
        out.push((
            Diag::DefineCycle {
                span: item_spans[anchor],
                chain: names.join(" → "),
            },
            home_of(anchor),
        ));
    }

    // The unplaceable edges: a same-scope dependency the sort cannot
    // satisfy because a scope section separates the pair (the target
    // stands in a LATER section — reordering never crosses a leader).
    // The cycle diags dominate: an edge inside a reported cycle is
    // already refused by E0015, and pairing the two would name the
    // same defect twice.
    let runs = section_runs(items);
    let mut run_of = vec![0usize; items.len()];
    for (run_at, &(start, end, _)) in runs.iter().enumerate() {
        for slot in &mut run_of[start..end] {
            *slot = run_at;
        }
    }
    let mut in_cycle: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); items.len()];
    for (cycle_at, chain) in cycles.into_iter().enumerate() {
        for &node in &chain {
            in_cycle[node].insert(cycle_at);
        }
    }
    for (at, deps) in resolved.iter().enumerate() {
        for &dep in deps {
            if run_of[dep] > run_of[at] && in_cycle[at].is_disjoint(&in_cycle[dep]) {
                out.push((
                    Diag::CrossSectionDependency {
                        span: item_spans[at],
                        target: node_word(&items[dep]),
                    },
                    home_of(at),
                ));
            }
        }
    }
    out
}

/// The display name a duplicate diagnostic carries — the definition's
/// own spelling (never the pass's host-qualified member key; params
/// spell with their `$`).
fn display_name(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Field(field) => field_path_key(&field.name),
        Stmt::Index(idx) => idx.name.raw.clone(),
        Stmt::Event(event) => event.name.raw.clone(),
        Stmt::Param(param) => format!("${}", param.name.raw),
        // A module keys by its real identity (a silo module's
        // executable name), not classify's empty-key fallback.
        Stmt::Module(_) => def_ident(stmt).map(|(_, key)| key).unwrap_or_default(),
        other => classify(other).1,
    }
}

/// The distinct cycles of a statement graph, each as a node loop
/// starting at its smallest index (rotation-normalized, so one cycle
/// reports once). Colors: white = unvisited, grey = on the current
/// DFS path, black = fully explored.
fn find_cycles(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Grey,
        Black,
    }
    let mut color = vec![Color::White; edges.len()];
    let mut stack: Vec<usize> = Vec::new();
    let mut found: BTreeSet<Vec<usize>> = BTreeSet::new();

    for root in 0..edges.len() {
        if color[root] != Color::White {
            continue;
        }
        // Iterative DFS with an explicit frame stack (node +
        // next-edge cursor).
        let mut frames: Vec<(usize, usize)> = vec![(root, 0)];
        color[root] = Color::Grey;
        stack.push(root);
        while let Some(&(node, cursor)) = frames.last() {
            if cursor < edges[node].len() {
                let next = edges[node][cursor];
                frames.last_mut().expect("frame present").1 += 1;
                match color[next] {
                    Color::White => {
                        color[next] = Color::Grey;
                        stack.push(next);
                        frames.push((next, 0));
                    }
                    Color::Grey => {
                        // A back edge: the cycle is the stack slice
                        // from `next` up.
                        let start = stack
                            .iter()
                            .position(|&n| n == next)
                            .expect("grey = on stack");
                        let mut cycle: Vec<usize> = stack[start..].to_vec();
                        let min_at = cycle
                            .iter()
                            .enumerate()
                            .min_by_key(|&(_, n)| n)
                            .expect("a cycle holds nodes")
                            .0;
                        cycle.rotate_left(min_at);
                        found.insert(cycle);
                    }
                    Color::Black => {}
                }
            } else {
                color[node] = Color::Black;
                stack.pop();
                frames.pop();
            }
        }
    }
    found.into_iter().collect()
}

// ---------------------------------------------------------------------
// The emission order — the stable topological sort
// ---------------------------------------------------------------------

/// The scope sections of one statement stream: (start, end, leader)
/// runs where `leader` marks a run OPENED by a `namespace`/`database`
/// statement. A leader never sorts — it is the scope law, not an
/// ordering opinion; a run's opening statement that is NOT a leader
/// (the file head before any scope statement) sorts like any other.
/// Both passes consume this: the check pass's cross-section rule and
/// the sort's movable sets must partition identically, or a dep the
/// check calls unplaceable could sort.
fn section_runs(items: &[Stmt]) -> Vec<(usize, usize, bool)> {
    let is_scope = |stmt: &Stmt| matches!(stmt, Stmt::Namespace(_) | Stmt::Database(_));
    let mut runs: Vec<(usize, usize, bool)> = Vec::new();
    let mut start = 0usize;
    for (at, stmt) in items.iter().enumerate() {
        if is_scope(stmt) && at != start {
            runs.push((start, at, is_scope(&items[start])));
            start = at;
        }
    }
    runs.push((start, items.len(), items.get(start).is_some_and(is_scope)));
    runs
}

/// Orders `file.items` for emission: sections (maximal runs under one
/// scope binding) sort their statements by the frozen edges with
/// authored order as the tie-break, and every table block sorts its
/// nested members the same way. Deterministic: the same statements in
/// the same authored order always emit identically, and an
/// already-ordered document is a fixed point. Runs on the LOWERED
/// (spread-expanded) stream — `codegen::emit`'s only caller shape.
pub fn emission_order(file: &mut SdlFile) {
    // The scope-keyed definition map over the WHOLE stream: a target
    // resolves anywhere its scope matches. A dep pointing outside its
    // own run is either already satisfied (an earlier run) or
    // unplaceable across a scope leader (a later run) — the former is
    // automatic, the latter drops here because the check-class pass
    // (SDL-E0016) refuses the document before emission ever sees it;
    // a library caller skipping the check gets authored order among
    // the run boundaries, never a panic.
    let mut walk = ScopeWalk::new();
    let mut definitions = Definitions::new();
    // The scope state BEFORE each statement — a run's walk seeds from
    // its head's state, or a run opened by a `database` would forget
    // the namespace bound before it.
    let mut states: Vec<ScopeWalk> = Vec::with_capacity(file.items.len());
    for (at, stmt) in file.items.iter().enumerate() {
        states.push(walk.clone());
        definitions.register(&walk, stmt, at);
        walk.advance(stmt);
    }
    let mut targets: Vec<Vec<Target>> = vec![Vec::new(); file.items.len()];
    for (at, stmt) in file.items.iter().enumerate() {
        stmt_edges(stmt, &mut targets[at]);
    }

    // Scope sections: a namespace/database statement opens a run and
    // never moves (the preamble scope law). Everything between two
    // leaders sorts within its run.
    for &(run_start, run_end, leader) in section_runs(&file.items).iter() {
        let size = run_end - run_start;
        if size < 2 {
            continue;
        }
        // Body-local deps over the SORTABLE statements (a leader
        // opening the run never sorts). The walk processes a
        // statement with the scope its predecessors bound, then
        // advances over it.
        let sortable: Vec<usize> = (run_start..run_end)
            .filter(|&at| !(leader && at == run_start))
            .collect();
        if sortable.len() < 2 {
            continue;
        }
        let local_of: HashMap<usize, usize> = sortable
            .iter()
            .enumerate()
            .map(|(local, &at)| (at, local))
            .collect();
        let mut deps: Vec<Vec<usize>> = vec![Vec::new(); sortable.len()];
        let mut walk = states[run_start].clone();
        for (offset, stmt) in file.items[run_start..run_end].iter().enumerate() {
            let at = run_start + offset;
            if let Some(&local) = local_of.get(&at) {
                for target in &targets[at] {
                    if let Some(dep) = definitions.resolve(&walk, stmt, target)
                        && run_start <= dep
                        && dep < run_end
                        && dep != at
                        // Nothing sorts before a run's leader (and no
                        // edge can name one: scope statements define
                        // nothing) — the guard is by construction.
                        && !(leader && dep == run_start)
                    {
                        deps[local].push(local_of[&dep]);
                    }
                }
            }
            walk.advance(stmt);
        }
        let order = stable_topo(sortable.len(), &deps);
        let mut section: Vec<Stmt> = file.items.drain(run_start..run_end).collect();
        let (head, body_owned) = if leader {
            let head = section.remove(0);
            (Some(head), section)
        } else {
            (None, section)
        };
        let mut slots: Vec<Option<Stmt>> = body_owned.into_iter().map(Some).collect();
        let mut body: Vec<Stmt> = order
            .into_iter()
            .map(|local| slots[local].take().expect("each sortable index taken once"))
            .collect();
        let mut items = Vec::with_capacity(size);
        if let Some(head) = head {
            items.push(head);
        }
        items.append(&mut body);
        debug_assert_eq!(items.len(), size);
        file.items.splice(run_start..run_start, items);
    }

    // Intra-block: every table's nested members sort fields before
    // the indexes that name them (row 4b); a nested fulltext index's
    // analyzer already rode the table's own top-level edge.
    for stmt in &mut file.items {
        if let Stmt::Table(table) = stmt {
            let members = std::mem::take(&mut table.nested);
            let mut deps: Vec<Vec<usize>> = vec![Vec::new(); members.len()];
            for (index_at, name) in nested_edges(&members) {
                for (sibling_at, sibling) in members.iter().enumerate() {
                    if let Stmt::Field(field) = sibling
                        && field_path_key(&field.name) == name
                        && sibling_at != index_at
                    {
                        deps[index_at].push(sibling_at);
                    }
                }
            }
            let order = stable_topo(members.len(), &deps);
            let mut slots: Vec<Option<Stmt>> = members.into_iter().map(Some).collect();
            table.nested = order
                .into_iter()
                .map(|local| slots[local].take().expect("each member index taken once"))
                .collect();
        }
    }
}

/// The stable topological order: Kahn's algorithm picking the lowest
/// original index among the satisfied each round; a cycle (only
/// reachable before `check` ran) forces the lowest remaining index,
/// which keeps the sort total and deterministic.
fn stable_topo(size: usize, deps: &[Vec<usize>]) -> Vec<usize> {
    let mut remaining: Vec<usize> = deps.iter().map(|d| d.len()).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); size];
    for (at, ds) in deps.iter().enumerate() {
        for &dep in ds {
            dependents[dep].push(at);
        }
    }
    let mut placed = vec![false; size];
    let mut order = Vec::with_capacity(size);
    for _ in 0..size {
        let pick = (0..size).find(|&i| !placed[i] && remaining[i] == 0);
        let pick = match pick {
            Some(pick) => pick,
            None => (0..size).find(|&i| !placed[i]).expect("nodes remain"),
        };
        placed[pick] = true;
        for &dependent in &dependents[pick] {
            remaining[dependent] -= 1;
        }
        order.push(pick);
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> crate::ast::SdlFile {
        let (file, diags) = crate::parser::parse_file_composable(src);
        assert!(diags.is_empty(), "{diags:?}");
        file
    }

    /// The single-file check shape: spans from the authored text's
    /// scan (one per statement, in order).
    fn spanned(src: &str) -> Vec<Span> {
        crate::lint::scan_source(src)
            .statements
            .iter()
            .map(|s| s.span)
            .collect()
    }

    #[test]
    fn stable_topo_prefers_authored_order() {
        // No deps: identity.
        assert_eq!(stable_topo(3, &[vec![], vec![], vec![]]), vec![0, 1, 2]);
        // b depends on a (dep listed on b): a first.
        assert_eq!(stable_topo(2, &[vec![], vec![0]]), vec![0, 1]);
        // a depends on b: b first, a second (the only legal order).
        assert_eq!(stable_topo(2, &[vec![1], vec![]]), vec![1, 0]);
        // Tie: c depends on nothing, a and b free — authored order
        // wins.
        assert_eq!(stable_topo(3, &[vec![], vec![2], vec![]]), vec![0, 2, 1]);
    }

    #[test]
    fn emission_order_moves_a_forward_param_before_its_reader() {
        let mut file = parse("param $a = $b + 1;\nparam $b = 2;");
        emission_order(&mut file);
        let order: Vec<&str> = file
            .items
            .iter()
            .map(|stmt| match stmt {
                Stmt::Param(param) => param.name.raw.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(order, ["b", "a"]);
        // Already ordered: a fixed point.
        let mut again = parse("param $b = 2;\nparam $a = $b + 1;");
        emission_order(&mut again);
        let order: Vec<&str> = again
            .items
            .iter()
            .map(|stmt| match stmt {
                Stmt::Param(param) => param.name.raw.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(order, ["b", "a"]);
    }

    #[test]
    fn emission_order_never_moves_scope_leaders() {
        // Two led sections: the leaders stay first; each section's
        // body sorts within it. The first section's `param $a` reads
        // `param $b` from the NEXT section — cross-run, unfixable,
        // stays put.
        let mut file = parse(
            "namespace acme;\nparam $a = $b + 1;\ndatabase app;\nparam $c = $d + 1;\nparam $d = 2;",
        );
        emission_order(&mut file);
        let order: Vec<String> = file
            .items
            .iter()
            .map(|stmt| match stmt {
                Stmt::Namespace(ns) => format!("ns {}", ns.name.raw),
                Stmt::Database(db) => format!("db {}", db.name.raw),
                Stmt::Param(param) => format!("param {}", param.name.raw),
                _ => "?".to_string(),
            })
            .collect();
        assert_eq!(
            order,
            ["ns acme", "param a", "db app", "param d", "param c",]
        );
        // A led section's body still sorts (d feeds c).
        let mut file = parse("namespace acme;\ndatabase app;\nparam $c = $d + 1;\nparam $d = 2;");
        emission_order(&mut file);
        let order: Vec<&str> = file
            .items
            .iter()
            .map(|stmt| match stmt {
                Stmt::Namespace(_) => "ns",
                Stmt::Database(_) => "db",
                Stmt::Param(param) => param.name.raw.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(order, ["ns", "db", "d", "c"]);
    }

    #[test]
    fn emission_order_sorts_nested_fields_before_their_indexes() {
        let mut file = parse("table t {\nindex ixt { fields: title; };\nfield title: string;\n};");
        emission_order(&mut file);
        let Stmt::Table(table) = &file.items[0] else {
            panic!("a table");
        };
        let first = matches!(&table.nested[0], Stmt::Field(_));
        assert!(first, "field lands before the index that names it");
    }

    #[test]
    fn check_reports_the_param_cycle_with_the_chain_named() {
        let src = "param $a = $b + 1;\nparam $b = $a + 1;";
        let file = parse(src);
        let diags = check(&file.items, &[], &spanned(src));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(matches!(diags[0].0, Diag::DefineCycle { .. }));
        let rendered = diags[0].0.to_string();
        assert!(rendered.contains("param `$a`"), "{rendered}");
        assert!(rendered.contains("param `$b`"), "{rendered}");
    }

    #[test]
    fn check_reports_duplicate_definitions_of_every_family() {
        let src = "table users;\ntable users;\nparam $x = 1;\nparam $x = 2;";
        let file = parse(src);
        let diags = check(&file.items, &[], &spanned(src));
        assert_eq!(diags.len(), 2, "{diags:?}");
        let rendered: Vec<String> = diags.iter().map(|(diag, _)| diag.to_string()).collect();
        assert!(
            rendered[0].contains("table `users` is already defined"),
            "{rendered:?}"
        );
        assert!(
            rendered[1].contains("param `$x` is already defined"),
            "{rendered:?}"
        );
    }

    #[test]
    fn check_leaves_scope_reopened_namespaces_legal() {
        let src = "namespace acme;\ndatabase one;\nnamespace acme;\ndatabase two;";
        let file = parse(src);
        assert!(check(&file.items, &[], &spanned(src)).is_empty());
    }

    #[test]
    fn check_leaves_same_named_members_on_different_hosts_legal() {
        let src = "table a;\ntable b;\nfield x: int { on: a; };\nfield x: string { on: b; };";
        let file = parse(src);
        assert!(check(&file.items, &[], &spanned(src)).is_empty());
    }
}
