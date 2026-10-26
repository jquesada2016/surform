//! The normalizer's merge core: the pre-merge SDL
//! source × the INFO read-model → canonical SDL, under
//! spec/README.md's normalization protocol. ADOPT takes the
//! database's read state per matched statement as the merge base;
//! ELIDE drops clauses equal to their SurrealQL defaults (canonical
//! SDL is minimal — the writer's form selection does the rest);
//! PRESERVE re-attaches what INFO drops but SDL means; FOLD removes
//! the server-synthesized `.*` subfields and relation `in`/`out`
//! fields the readers faithfully carried.
//!
//! Retro-application is a merge, not an overwrite: an SDL statement
//! with no INFO counterpart stays authored in place (its drift is the
//! differ's); an INFO entry with no SDL counterpart is
//! adopted (elide + fold only — no preserve source). Output is a
//! total function of the two inputs. Spelling carriage: where a
//! clause's canonical value is unchanged, the authored alias spelling
//! rides ([`Authored`]/[`Carried`]) — aliases are never rewritten on
//! retro-apply, only on emitted DDL.
//!
//! Placement is sectioned, not a flat append: the walk produces
//! per-scope [`Section`]s — merged authored skeleton + the scope's
//! adoptees — and [`place_sections`](super::place) orders them
//! by the spec's dependency-bounded topological rule.
//! A scope with no SDL section still synthesizes its bare
//! `namespace`/`database` statements at the file's end, grouped so an
//! anchor is never separated from its content.

use std::collections::{BTreeMap, BTreeSet};

use super::diff::state_equal;
use super::place::{Section, place_sections, raw_match_key};
use crate::ast::{
    Access, AccessDuration, AccessKind, Authored, Carried, Config, Database, Event, Field,
    FieldPath, Filter, Function, Ident, IndexKind, JwtSource, JwtVerify, Level, Namespace,
    OpPermissions, Permission, PermissionGroup, PermissionOp, Role, SdlFile, Sequence, Stmt, Table,
    TableKind, TypeAlias, User,
};
use crate::diag::Diag;
use crate::expand::{
    AliasResolutions, expand, landing_members, resolve_kind_text, rewrite_stmt_kinds,
};
use crate::info::read::{
    InfoObject, InfoValue, Markers, ReadEntry, read_accesses, read_analyzers, read_apis,
    read_buckets, read_configs, read_databases, read_default_config, read_functions, read_modules,
    read_namespaces, read_params, read_sequences, read_tables, read_users,
};
use crate::kind::{inner_kind, kinds_equal, parse_kind_ast};

/// The merge walk's mixin registry — the document's WHOLE mixin set
/// (the declarative namespace: any position), the same set `expand`
/// resolves against.
pub(crate) type Mixins<'a> = Vec<(&'a str, &'a [Stmt])>;

// ---------------------------------------------------------------------
// The read model
// ---------------------------------------------------------------------

/// The db interchange file's reserved sibling key: table name → that
/// table's `INFO FOR TABLE` object (the 2026-09-24
/// amendment — INFO's `tables` values are DDL strings, so no in-band
/// shape can carry the child maps, and a table named like a DB section
/// key rules out table-named keys). INFO never prints such a key.
pub const TABLE_CHILDREN_KEY: &str = "@tables";

/// One statement family in the INFO map's key order (alphabetical —
/// the INFO objects' own `BTreeMap` iteration order, which is the
/// protocol's tie-break among adoptees).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Family {
    Access,
    Analyzer,
    Api,
    Bucket,
    /// SDL-only — the linter's allow-comment findings (SDL-L0023);
    /// nothing classifies to it and no INFO side exists (the
    /// [`Family::Mixin`] precedent: the enum hosts linter-only
    /// families past the read side's own).
    Comment,
    Config,
    Database,
    Event,
    Field,
    Function,
    Index,
    /// SDL-only — the linter's diamond-include advisory (SDL-L0026);
    /// nothing classifies to it and no INFO side exists (the
    /// [`Family::Mixin`] precedent: the enum hosts linter-only
    /// families past the read side's own).
    Include,
    /// SDL-only ([`crate::ast::Mixin`]) — never a read-side family
    /// (INFO cannot carry mixins); only the linter's authored
    /// findings carry it.
    Mixin,
    /// SDL-only ([`crate::ast::TypeAlias`]) — never a read-side family
    /// (INFO stores only concrete kinds, so the database cannot carry
    /// an alias); the statement itself pairs with no read entry and
    /// stays authored through the merge, the [`Family::Mixin`]
    /// precedent. (The FIELD spellings are Unit T2's preserve arm.)
    TypeAlias,
    Module,
    Namespace,
    Param,
    Raw,
    Sequence,
    Table,
    User,
}

/// A statement's (family, key) — the merge's match key. Keys are the
/// readers' entry names (the INFO map keys, decoded).
pub(crate) fn classify(stmt: &Stmt) -> (Family, String) {
    match stmt {
        Stmt::Access(s) => (Family::Access, s.name.raw.clone()),
        Stmt::Analyzer(s) => (Family::Analyzer, s.name.raw.clone()),
        Stmt::Api(s) => (Family::Api, s.path.clone()),
        Stmt::Bucket(s) => (Family::Bucket, s.name.raw.clone()),
        Stmt::Config(s) => (
            Family::Config,
            match s {
                Config::Api(_) => "api",
                Config::Graphql(_) => "graphql",
                Config::Default(_) => "default",
            }
            .to_string(),
        ),
        Stmt::Database(s) => (Family::Database, s.name.raw.clone()),
        Stmt::Event(s) => (Family::Event, s.name.raw.clone()),
        Stmt::Field(s) => (Family::Field, field_path_key(&s.name)),
        Stmt::Function(s) => (Family::Function, s.name.segments.join("::")),
        Stmt::Index(s) => (Family::Index, s.name.raw.clone()),
        Stmt::Module(s) => (
            Family::Module,
            s.name.as_ref().map(|n| n.raw.clone()).unwrap_or_default(),
        ),
        Stmt::Namespace(s) => (Family::Namespace, s.name.raw.clone()),
        Stmt::Param(s) => (Family::Param, s.name.raw.clone()),
        Stmt::Raw(_) => (Family::Raw, String::new()),
        Stmt::Sequence(s) => (Family::Sequence, s.name.raw.clone()),
        Stmt::Table(s) => (Family::Table, s.name.raw.clone()),
        // SDL-only statements: the merge runs on the AUTHORED file
        // (spread-preserving — the spreads reconstruct after the
        // verbs, `restore_spreads`), and no read entry ever pairs
        // with these arms (INFO cannot carry mixins), so a mixin
        // statement stays authored through the walk and a spread
        // entry rides its table's reconstruction — the family never
        // reaches a leftover adoptee. Total, never a panic, for the
        // library caller and the CLI alike.
        Stmt::Mixin(s) => (Family::Mixin, s.name.raw.clone()),
        Stmt::TypeAlias(s) => (Family::TypeAlias, s.name.raw.clone()),
        Stmt::Spread(s) => (Family::Mixin, s.name.raw.clone()),
        // `include` (spec/include.md) rides the Raw family: SDL-only,
        // no read entry ever pairs with it, and the composed-root
        // refusal keeps it out of the merge's walk entirely.
        Stmt::Include(_) => (Family::Raw, String::new()),
        Stmt::User(s) => (Family::User, s.name.raw.clone()),
    }
}

/// A field path's key spelling — the INFO map key's shape
/// (`matrix.*.*`). One home, two consumers: the merge's match key
/// and the linter's anchor lookup (a resolved field's finding must
/// look up the authored statement by the same spelling).
pub(crate) fn field_path_key(path: &FieldPath) -> String {
    path.to_text()
}

/// One scope's read entries by family. Per-family order is the
/// reader's (the INFO map's); cross-family adoption walks families in
/// [`Family`]'s alphabetical order.
#[derive(Debug, Default, Clone)]
pub struct ScopeEntries {
    families: BTreeMap<Family, Vec<ReadEntry>>,
}

impl ScopeEntries {
    pub(super) fn push(&mut self, entry: ReadEntry) {
        let (family, _) = classify(&entry.stmt);
        self.families.entry(family).or_default().push(entry);
    }

    /// Takes the entry a statement matches, if any — matching consumes
    /// (one matching home: the differ's [`Self::take_named`]). A raw
    /// statement matches by its DEFINE header's family AND name — the
    /// reader pushes raw-routed entries into the Raw family's bucket
    /// under their parsed names, and classify's empty key would never
    /// consume one (the entry would re-adopt on every merge, growing
    /// the file by a duplicate per run). A raw table and a raw
    /// analyzer can share a name; the header decides.
    fn take(&mut self, stmt: &Stmt) -> Option<ReadEntry> {
        if let Stmt::Raw(raw) = stmt {
            let key = super::place::raw_match_key(&raw.body)?;
            let entries = self.families.get_mut(&Family::Raw)?;
            let at = entries.iter().position(|entry| match &entry.stmt {
                Stmt::Raw(entry_raw) => {
                    super::place::raw_match_key(&entry_raw.body).as_ref() == Some(&key)
                }
                _ => false,
            })?;
            return Some(entries.remove(at));
        }
        let (family, key) = classify(stmt);
        self.take_named(family, &key)
    }

    /// The entries no SDL statement matched, family order — the
    /// adoptees.
    fn leftovers(&self) -> impl Iterator<Item = &ReadEntry> {
        self.families.values().flatten()
    }

    /// The adoptees outside one family (namespace statements never
    /// insert mid-walk; database statements adopt as their synthesized
    /// blocks instead of bare ns-scope statements).
    fn leftovers_except(&self, family: Family) -> impl Iterator<Item = &ReadEntry> {
        self.families
            .iter()
            .filter(move |(f, _)| **f != family)
            .flat_map(|(_, entries)| entries.iter())
    }

    /// The Family-scoped leftovers by name, in entry order.
    fn leftover_names(&self, family: Family) -> Vec<String> {
        self.families
            .get(&family)
            .map(|entries| entries.iter().map(|e| e.name.clone()).collect())
            .unwrap_or_default()
    }

    /// The entries of one family, in entry order (the differ's lookup).
    pub(super) fn family(&self, family: Family) -> impl Iterator<Item = &ReadEntry> {
        self.families.get(&family).into_iter().flatten()
    }

    /// Takes the named entry of one family — the differ's structured
    /// match (matching consumes, like the merge's).
    pub(super) fn take_named(&mut self, family: Family, name: &str) -> Option<ReadEntry> {
        let entries = self.families.get_mut(&family)?;
        let at = entries.iter().position(|entry| entry.name == name)?;
        Some(entries.remove(at))
    }

    /// Takes the entry at one position of a family — the differ's raw
    /// counterpart match (the header decides the family).
    pub(super) fn take_at(&mut self, family: Family, at: usize) -> Option<ReadEntry> {
        let entries = self.families.get_mut(&family)?;
        (at < entries.len()).then(|| entries.remove(at))
    }

    /// The leftovers with their family, family order within a scope —
    /// the differ's database-only walk.
    pub(super) fn leftovers_with_family(&self) -> impl Iterator<Item = (Family, &ReadEntry)> {
        self.families
            .iter()
            .flat_map(|(family, entries)| entries.iter().map(move |entry| (*family, entry)))
    }

    fn is_empty(&self) -> bool {
        self.families.values().all(Vec::is_empty)
    }
}

/// The read model over a multi-scope interchange: ROOT, each namespace
/// section, each database section. Scope identity is the interchange's
/// explicit tags — the merge never infers scope.
#[derive(Debug, Default, Clone)]
pub struct ReadModel {
    pub root: ScopeEntries,
    /// (ns name, entries) — `databases`, ns users, ns accesses.
    pub namespaces: Vec<(String, ScopeEntries)>,
    /// (ns name, db name, entries) — the DB scope (tables + children
    /// and every other DB section).
    pub databases: Vec<(String, String, ScopeEntries)>,
    /// The scopes a tag actually presented (pushed even when the
    /// scope's INFO was empty) — the differ's "a scope never ingested
    /// contributes nothing" guard: an SDL statement in a scope the
    /// model never examined can neither match nor classify (an
    /// apply-pending claim there would be false — the database side
    /// was never looked at). The field is private by necessity: the
    /// invariant only holds when scopes arrive through
    /// [`Self::push_scope`] / [`Self::from_interchange`] — a model
    /// hand-built through the public entry fields bypasses the
    /// tracking and the differ (correctly) reports nothing about it.
    ingested: BTreeSet<ScopeKey>,
}

/// The interchange inputs for [`ReadModel::from_interchange`]: each
/// scope's rendered INFO object, tagged. The db objects may carry the
/// [`TABLE_CHILDREN_KEY`] child map.
pub struct Interchange<'a> {
    pub root: Option<&'a InfoObject>,
    pub namespaces: &'a [(String, &'a InfoObject)],
    pub databases: &'a [(String, String, &'a InfoObject)],
}

/// One interchange scope's tag: which scope a rendered
/// INFO object belongs to, as the CLI's `--info` spelling carries it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ScopeTag {
    Root,
    Namespace(String),
    /// (namespace, database) — the empty namespace segment is the
    /// pseudo-scope ("no preceding namespace statement").
    Database(String, String),
}

impl ReadModel {
    /// Runs the readers per scope. Reader diagnostics propagate — the
    /// merge does not run over a scope that failed to read (the CLI
    /// reports and exits; per-statement recovery is a ledgered CLI
    /// corner).
    pub fn from_interchange(input: Interchange<'_>) -> Result<ReadModel, Vec<Diag>> {
        let mut model = ReadModel::default();
        let mut diags = Vec::new();
        if let Some(root) = input.root {
            diags.extend(
                model
                    .push_scope(ScopeTag::Root, root)
                    .err()
                    .unwrap_or_default(),
            );
        }
        for (name, info) in input.namespaces {
            diags.extend(
                model
                    .push_scope(ScopeTag::Namespace(name.clone()), info)
                    .err()
                    .unwrap_or_default(),
            );
        }
        for (ns, name, info) in input.databases {
            diags.extend(
                model
                    .push_scope(ScopeTag::Database(ns.clone(), name.clone()), info)
                    .err()
                    .unwrap_or_default(),
            );
        }
        if diags.is_empty() {
            Ok(model)
        } else {
            Err(diags)
        }
    }

    /// Ingests one interchange scope: runs that scope's readers and
    /// extends the model. One scope's diagnostics return together, so
    /// the CLI can report them against that scope's own text (their
    /// spans anchor there).
    pub fn push_scope(&mut self, tag: ScopeTag, info: &InfoObject) -> Result<(), Vec<Diag>> {
        let mut diags = Vec::new();
        match tag {
            ScopeTag::Root => {
                self.ingested.insert(ScopeKey::Root);
                ingest_root(info, &mut self.root, &mut diags);
            }
            ScopeTag::Namespace(name) => {
                let mut scope = ScopeEntries::default();
                ingest_namespace(info, &mut scope, &mut diags);
                self.ingested.insert(ScopeKey::Namespace(name.clone()));
                self.namespaces.push((name, scope));
            }
            ScopeTag::Database(ns, name) => {
                let mut scope = ScopeEntries::default();
                ingest_database(info, &mut scope, &mut diags);
                self.ingested
                    .insert(ScopeKey::Database(ns.clone(), name.clone()));
                self.databases.push((ns, name, scope));
            }
        }
        if diags.is_empty() { Ok(()) } else { Err(diags) }
    }

    /// Whether a tag presented this scope — the differ's gate for
    /// making any claim about an SDL statement ([`ScopeKey::Root`]
    /// counts only when a `root=` tag was given).
    pub(super) fn ingested(&self, scope: &ScopeKey) -> bool {
        self.ingested.contains(scope)
    }

    /// Registers a scope as ingested — the counterpart for the test
    /// suites building a model entry-by-entry, bypassing
    /// [`Self::push_scope`].
    #[cfg(test)]
    pub(super) fn mark_ingested(&mut self, scope: ScopeKey) {
        self.ingested.insert(scope);
    }

    /// The version pair's read-side synthesis: the
    /// FROM schema file lowered into the slot the interchange model
    /// holds, so ONE comparator serves every `migrate generate` input
    /// mode — the DB pair (`--info`), the SDL×SDL pair
    /// (`--from`/`--to`), and the baseline's empty-FROM side all diff
    /// against this model.
    ///
    /// The from side plays the interchange's role, honestly: its
    /// entries hold the AUTHORED statements (markers default — nothing
    /// was redacted, no sentinel and no auto-seed exists on an
    /// authored side), named by the match key the walk pairs on —
    /// [`classify`]'s key, or a raw body's `DEFINE` header
    /// ([`raw_match_key`], the empty key when the header never
    /// classifies) — exactly the names the DB readers file raw-routed
    /// entries under. The comparator resolves BOTH sides through the
    /// one `resolve`, so the entries need no lowering of their own and
    /// the comparator grows no from-side special case. The expansion
    /// is the SAME one the authored side runs inside the differ (the
    /// database only ever holds a spread's members as concrete
    /// definitions, and v1 was applied expanded).
    ///
    /// The scope universe is the UNION of both files' walked scopes
    /// (plus Root): the DB pair's ingestion gate exists because an
    /// un-examined database side supports no claims — the version
    /// pair's from side is complete by construction (a database
    /// applied at v1 holds exactly what v1 declares), so every scope
    /// either file mentions is examined. A scope the TO file dropped
    /// is from-only content — the script's collapse logic (the TO
    /// file's own declarations) retires it under the owner REMOVE; a
    /// scope only the TO file declares is genuinely apply-pending —
    /// the script defines it, scope owners first.
    pub fn version_pair_model(from: &SdlFile, to: &SdlFile) -> ReadModel {
        let mut model = ReadModel::default();
        model.ingested.insert(ScopeKey::Root);
        let expanded = expand(from);
        let mut walk = ScopeWalk::new();
        for stmt in &expanded.items {
            let scope = walk.lookup_scope(stmt);
            model.ingested.insert(scope.clone());
            let name = match stmt {
                Stmt::Raw(raw) => raw_match_key(&raw.body)
                    .map(|(_, key)| key)
                    .unwrap_or_default(),
                _ => classify(stmt).1,
            };
            Self::entries_for(&mut model, &scope).push(ReadEntry {
                name,
                stmt: stmt.clone(),
                markers: Markers::default(),
            });
            walk.advance(stmt);
        }
        // The TO side's scopes are examined too (its entries are the
        // diff's SDL side, never entries here) — the walk is the same
        // one the differ runs, so the ingested set is exactly the
        // scopes the comparison can query.
        mark_file_scopes_ingested(&mut model, to);
        model
    }

    /// The bare-file baseline's read-side synthesis (issue #101): the
    /// SDL file plays the TO side and the FROM side is EMPTY — the run
    /// carries no live state to converge from. Every scope the file
    /// mentions is examined (the same universe rule as the version
    /// pair, the union minus the empty side) and holds NO entries, so
    /// every finding is apply-pending (`SdlOnly`) and nothing can
    /// classify as a removal — the comparator's removal arm fires on
    /// model entries alone. That is the safe-by-construction direction
    /// the first-bootstrap baseline needs: an SDL-only run cannot
    /// remove anything, by the shape of the model.
    pub fn baseline_model(to: &SdlFile) -> ReadModel {
        let mut model = ReadModel::default();
        model.ingested.insert(ScopeKey::Root);
        mark_file_scopes_ingested(&mut model, to);
        model
    }

    /// The entries of one scope, created on first touch — the
    /// synthesis' insertion home (Root is a struct field; namespaces
    /// and databases are per-scope vecs).
    fn entries_for<'a>(model: &'a mut ReadModel, scope: &ScopeKey) -> &'a mut ScopeEntries {
        match scope {
            ScopeKey::Root => &mut model.root,
            ScopeKey::Namespace(ns) => {
                let at = model.namespaces.iter().position(|(name, _)| name == ns);
                let at = match at {
                    Some(at) => at,
                    None => {
                        model.namespaces.push((ns.clone(), ScopeEntries::default()));
                        model.namespaces.len() - 1
                    }
                };
                &mut model.namespaces[at].1
            }
            ScopeKey::Database(ns, db) => {
                let at = model
                    .databases
                    .iter()
                    .position(|(sns, sdb, _)| sns == ns && sdb == db);
                let at = match at {
                    Some(at) => at,
                    None => {
                        model
                            .databases
                            .push((ns.clone(), db.clone(), ScopeEntries::default()));
                        model.databases.len() - 1
                    }
                };
                &mut model.databases[at].2
            }
        }
    }
}

/// Marks every scope one SDL file's statements name as ingested — the
/// TO-side walk both file-pair syntheses share: `version_pair_model`
/// unions it with the from side's scopes, the baseline
/// ([`ReadModel::baseline_model`]) is the walk alone, the from side
/// empty. The entries ride the FROM side only; this walk computes
/// exactly the ingested set — the same one the differ runs — so the
/// comparison can query every scope the file mentions.
fn mark_file_scopes_ingested(model: &mut ReadModel, file: &SdlFile) {
    let expanded = expand(file);
    let mut walk = ScopeWalk::new();
    for stmt in &expanded.items {
        model.ingested.insert(walk.lookup_scope(stmt));
        walk.advance(stmt);
    }
}

/// Parses one `--info` spec (the tagged form): `root=<path>`,
/// `ns=<name>=<path>`, `db=<ns>/<name>=<path>`. Separators split at
/// their first occurrence after the tag prefix, so a path may hold `=`.
/// The diagnostic's span anchors the spec text, which the CLI feeds
/// the renderer as its source.
pub fn parse_info_spec(spec: &str) -> Result<(ScopeTag, String), Diag> {
    // The malformed class carries its own help (issue #90): the
    // generic text speaks to scope declarations, not spelling.
    const SPELLING: &str =
        "spell the tag `root=<path>`, `ns=<name>=<path>`, or `db=<ns>/<name>=<path>`";
    let invalid = |at: usize, message: String| Diag::InterchangeTagInvalid {
        span: crate::lexer::Span::point(at),
        message,
        help: Some(SPELLING),
    };
    let Some((tag, rest)) = spec.split_once('=') else {
        return Err(invalid(
            0,
            format!("expected `root=`, `ns=`, or `db=` — `{spec}` has no `=`"),
        ));
    };
    match tag {
        "root" => {
            if rest.is_empty() {
                Err(invalid(5, "the `root=` tag needs a path".to_string()))
            } else {
                Ok((ScopeTag::Root, rest.to_string()))
            }
        }
        "ns" => match rest.split_once('=') {
            Some((name, path)) if !name.is_empty() && !path.is_empty() => {
                Ok((ScopeTag::Namespace(name.to_string()), path.to_string()))
            }
            _ => Err(invalid(
                3,
                "the `ns=` tag needs `<name>=<path>`".to_string(),
            )),
        },
        "db" => {
            let Some((ns, db_and_path)) = rest.split_once('/') else {
                return Err(invalid(
                    3,
                    "the `db=` tag needs `<ns>/<name>=<path>`".to_string(),
                ));
            };
            match db_and_path.split_once('=') {
                Some((name, path)) if !name.is_empty() && !path.is_empty() => Ok((
                    ScopeTag::Database(ns.to_string(), name.to_string()),
                    path.to_string(),
                )),
                _ => Err(invalid(
                    3 + ns.len() + 1,
                    "the `db=` tag needs `<ns>/<name>=<path>`".to_string(),
                )),
            }
        }
        other => Err(invalid(
            0,
            format!("unknown tag prefix `{other}=` — expected `root=`, `ns=`, or `db=`"),
        )),
    }
}

/// Validates one parsed `--info` tag against the SDL file's declared
/// scopes (the scope-mismatch family, promoted at the CLI): a
/// tag naming a scope the file never declares cannot have come from
/// one applied file (codegen emits no `USE` for the missing level),
/// and a `db=` tag with an empty namespace segment requires the
/// namespace-free file. Duplicates are the CLI's parse-side check.
pub fn validate_scope_tag(tag: &ScopeTag, sdl: &SdlFile) -> Result<(), Diag> {
    // The undeclared-scope class keeps the generic help (the generic
    // text IS this class's fix: mirror the file's own declarations).
    let invalid = |message: String| Diag::InterchangeTagInvalid {
        span: crate::lexer::Span::point(0),
        message,
        help: None,
    };
    let (namespaces, databases) = declared_scopes(sdl);
    match tag {
        ScopeTag::Root => Ok(()),
        ScopeTag::Namespace(name) => {
            if namespaces.contains(name) {
                Ok(())
            } else {
                Err(invalid(format!(
                    "the `ns=` tag names `{name}`, which the SDL file never declares"
                )))
            }
        }
        ScopeTag::Database(ns, name) => {
            if ns.is_empty() {
                if namespaces.is_empty() {
                    Ok(())
                } else {
                    Err(invalid(
                        "the `db=` tag's empty namespace segment requires a \
                         namespace-free file, but the file declares namespaces"
                            .to_string(),
                    ))
                }
            } else if databases.contains(&(ns.clone(), name.clone())) {
                Ok(())
            } else {
                Err(invalid(format!(
                    "the `db=` tag names `{ns}/{name}`, which the SDL file never declares"
                )))
            }
        }
    }
}

/// The scopes an SDL file declares, as the merge's walk binds them: a
/// `database` statement lives in the namespace open at its position
/// (the empty-ns pseudo-scope when none is).
pub fn declared_scopes(sdl: &SdlFile) -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    let mut namespaces = BTreeSet::new();
    let mut databases = BTreeSet::new();
    let mut current_ns = String::new();
    for stmt in &sdl.items {
        match stmt {
            Stmt::Namespace(ns) => {
                current_ns = ns.name.raw.clone();
                namespaces.insert(current_ns.clone());
            }
            Stmt::Database(db) => {
                databases.insert((current_ns.clone(), db.name.raw.clone()));
            }
            _ => {}
        }
    }
    (namespaces, databases)
}

fn collect(entries: Vec<Result<ReadEntry, Diag>>, scope: &mut ScopeEntries, diags: &mut Vec<Diag>) {
    for entry in entries {
        match entry {
            Ok(entry) => scope.push(entry),
            Err(diag) => diags.push(diag),
        }
    }
}

fn ingest_root(info: &InfoObject, scope: &mut ScopeEntries, diags: &mut Vec<Diag>) {
    collect(read_namespaces(info), scope, diags);
    collect(read_users(info), scope, diags);
    collect(read_accesses(info), scope, diags);
    match read_default_config(info) {
        Ok(Some(entry)) => scope.push(entry),
        Ok(None) => {}
        Err(diag) => diags.push(diag),
    }
}

fn ingest_namespace(info: &InfoObject, scope: &mut ScopeEntries, diags: &mut Vec<Diag>) {
    collect(read_databases(info), scope, diags);
    collect(read_users(info), scope, diags);
    collect(read_accesses(info), scope, diags);
}

fn ingest_database(info: &InfoObject, scope: &mut ScopeEntries, diags: &mut Vec<Diag>) {
    // The tables ride their reserved child map; without the key the
    // tables read with no children (the child sections don't surface).
    let children: BTreeMap<String, InfoObject> = match info.0.get(TABLE_CHILDREN_KEY) {
        Some(InfoValue::Object(map)) => map
            .0
            .iter()
            .filter_map(|(name, value)| match value {
                InfoValue::Object(obj) => Some((name.clone(), obj.clone())),
                _ => None,
            })
            .collect(),
        _ => BTreeMap::new(),
    };
    collect(read_tables(info, &children), scope, diags);
    collect(read_functions(info), scope, diags);
    collect(read_analyzers(info), scope, diags);
    collect(read_params(info), scope, diags);
    collect(read_sequences(info), scope, diags);
    collect(read_accesses(info), scope, diags);
    collect(read_apis(info), scope, diags);
    collect(read_buckets(info), scope, diags);
    collect(read_configs(info), scope, diags);
    collect(read_modules(info), scope, diags);
    collect(read_users(info), scope, diags);
}

// ---------------------------------------------------------------------
// The merge
// ---------------------------------------------------------------------

/// A file's scope as the walk sees it: ROOT until the first
/// `namespace`, that namespace's scope until its first `database`,
/// that database's scope after. A `database` with no preceding
/// namespace binds to the empty-ns pseudo-scope (codegen's own
/// no-USE posture).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScopeKey {
    Root,
    Namespace(String),
    /// (namespace, database) — the empty namespace segment is the
    /// pseudo-scope ("no preceding namespace statement").
    Database(String, String),
}

/// One SDL file's scope walk — the binding of each statement to the
/// interchange scope it matches in. One home, three consumers: the
/// merge places by it, the differ compares by it, and the linter
/// anchors by it, so a
/// statement can never land in one scope for retro-apply, another
/// for drift reporting, and a third for advisory findings.
#[derive(Debug, Clone)]
pub(crate) struct ScopeWalk {
    pub(crate) current_ns: String,
    pub(crate) current_db: String,
    /// The walk's open scope (statements without their own level ride
    /// it).
    pub(crate) scope: ScopeKey,
}

impl ScopeWalk {
    pub(crate) fn new() -> Self {
        ScopeWalk {
            current_ns: String::new(),
            current_db: String::new(),
            scope: ScopeKey::Root,
        }
    }

    /// The scope a statement matches in: `namespace` lives at ROOT
    /// (its `namespaces` section), `database` ALWAYS lives in its
    /// namespace's scope (the NS `databases` section — a second
    /// `database` statement must match there even after the first one
    /// opened a db scope), `config default` is ROOT-region by design
    /// (codegen hoists it), and users/accesses carry their level in
    /// the statement text (`on:`, read at every tier).
    /// Everything else matches the walk's open scope.
    pub(crate) fn lookup_scope(&self, stmt: &Stmt) -> ScopeKey {
        match stmt {
            Stmt::Namespace(_) | Stmt::Config(Config::Default(_)) => ScopeKey::Root,
            Stmt::Database(_) => ScopeKey::Namespace(self.current_ns.clone()),
            Stmt::User(user) => level_scope(
                user.on.value,
                self.current_ns.clone(),
                self.current_db.clone(),
            ),
            Stmt::Access(access) => level_scope(
                access.on.value,
                self.current_ns.clone(),
                self.current_db.clone(),
            ),
            _ => self.scope.clone(),
        }
    }

    /// Advances the walk over one statement — the scope-opening
    /// statements rebind it (a `database` opens its db scope, a
    /// `namespace` opens its ns scope); everything else leaves it.
    pub(crate) fn advance(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Database(db) => {
                self.current_db = db.name.raw.clone();
                self.scope = ScopeKey::Database(self.current_ns.clone(), self.current_db.clone());
            }
            Stmt::Namespace(ns) => {
                self.current_ns = ns.name.raw.clone();
                self.scope = ScopeKey::Namespace(self.current_ns.clone());
            }
            _ => {}
        }
    }
}

/// Merges the pre-merge SDL file against the read model → the
/// canonical SDL file: matching + the four verbs here, canonical
/// placement over the resulting sections.
pub fn merge(sdl: &SdlFile, read: &ReadModel) -> SdlFile {
    merge_with_provenance(sdl, read).0
}

/// [`merge`] plus each output statement's provenance — parallel to
/// the merged items, the index of the authored input the statement
/// merged from, or `None` when it was adopted or synthesized and has
/// no authored home. The write-back distribution's input
/// (spec/include.md): skeleton order never reorders, so a `Some`
/// chain reconstructs each authored file exactly.
pub fn merge_with_provenance(sdl: &SdlFile, read: &ReadModel) -> (SdlFile, Vec<Option<usize>>) {
    let mut read = read.clone();
    let mut sections: Vec<Section> = vec![Section::new(ScopeKey::Root)];
    // The scopes the walk visited (a section opened, or the ROOT
    // region holds a statement) — the tail synthesis gates on their
    // absence.
    let mut visited: BTreeSet<ScopeKey> = BTreeSet::new();
    // The namespaces the SDL file itself declares — the ROOT-tail
    // synthesis must not re-emit them.
    let mut declared_ns: BTreeSet<String> = BTreeSet::new();
    let mut walk = ScopeWalk::new();
    // The document's mixin set — GLOBAL, any position (the
    // declarative namespace; the registry `restore_spreads` resolves
    // against, the same set expansion resolves against).
    let mixins: Mixins<'_> = sdl
        .items
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Mixin(mixin) => Some((mixin.name.raw.as_str(), mixin.members.as_slice())),
            _ => None,
        })
        .collect();
    // The document's alias set — the same global shape the forward
    // expansion resolves against (first-wins on a duplicate,
    // unobservable on a cleared file, the registry's finding). The
    // preserve verb resolves the authored kinds against it; the
    // faults are the order pass's diagnostics.
    let aliases: Vec<(&str, &TypeAlias)> = sdl
        .items
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::TypeAlias(alias) => Some((alias.name.raw.as_str(), alias)),
            _ => None,
        })
        .collect();
    let (resolutions, _) = AliasResolutions::build(&aliases);
    for (input, stmt) in sdl.items.iter().enumerate() {
        let entry = take_for_scope(&mut read, &walk.lookup_scope(stmt), stmt);
        let merged_stmt = match entry {
            // A raw-routed statement is its own preservation (the
            // preserve verb's conclusion): the body is not
            // clause-addressable, so a match only CONSUMES — the
            // authored body stays verbatim.
            Some(_) if matches!(stmt, Stmt::Raw(_)) => stmt.clone(),
            Some(entry) => merge_stmt(stmt, &entry, &mixins, &resolutions),
            None => stmt.clone(),
        };
        // Sections open forward-only: a `database` statement opens its
        // db section (and is its skeleton's head); a `namespace`
        // statement is itself a ROOT statement — it rides the OPEN
        // ROOT section when one is current, else it OPENS a new one at
        // its authored point (moving it to the file head would
        // re-bind every later `database` statement to the wrong
        // namespace) — and it opens its ns section for the content
        // between it and the first database; everything else lands in
        // the open section.
        walk.advance(stmt);
        match stmt {
            Stmt::Database(_) => {
                visited.insert(walk.scope.clone());
                let mut section = Section::new(walk.scope.clone());
                section.push_authored(merged_stmt, input);
                sections.push(section);
            }
            Stmt::Namespace(ns) => {
                declared_ns.insert(ns.name.raw.clone());
                visited.insert(walk.scope.clone());
                if sections
                    .last()
                    .is_some_and(|section| section.scope == ScopeKey::Root)
                {
                    sections
                        .last_mut()
                        .expect("checked above")
                        .push_authored(merged_stmt, input);
                } else {
                    let mut section = Section::new(ScopeKey::Root);
                    section.push_authored(merged_stmt, input);
                    sections.push(section);
                }
                sections.push(Section::new(walk.scope.clone()));
            }
            _ => {
                let section = sections.last_mut().expect("the ROOT section always exists");
                section.push_authored(merged_stmt, input);
            }
        }
    }
    if sections
        .iter()
        .any(|section| section.scope == ScopeKey::Root && !section.skeleton.is_empty())
    {
        visited.insert(ScopeKey::Root);
    }

    // The visited scopes' adoptees attach to their sections; scopes
    // the walk never visited synthesize at the file's end. Namespace-
    // family leftovers never attach (a `namespace` statement would
    // re-bind every following scope) — they tail via
    // synthesized_sections; likewise ns-scope database leftovers adopt
    // as their synthesized blocks.
    for (key, entries) in visited_scope_adoptees(&read, &visited) {
        // A scope may hold several sections (a `namespace` after a
        // `database` opens a fresh ROOT one at its authored point; a
        // re-declared name opens another): its adoptees ride the LAST.
        let section = sections
            .iter_mut()
            .rev()
            .find(|section| section.scope == key)
            .expect("visited_scope_adoptees gates on the walk's sections");
        for entry in entries {
            section.adoptees.push(elide_and_fold(entry.stmt.clone()));
        }
    }

    sections.extend(synthesized_sections(&read, &visited, &declared_ns));
    let (items, provenance) = place_sections(sections);
    (
        SdlFile {
            items,
            // The source file's trailing `//` lines ride the merge (the
            // statements keep theirs through the clone-and-place walk).
            trailing_comments: sdl.trailing_comments.clone(),
        },
        provenance,
    )
}

/// The placement's match key for a statement — the write-back's
/// preamble rule reads it (does any authored statement still depend on
/// this adoptee?). [`super::place::stmt_key`] under a public name.
pub fn placement_key(stmt: &Stmt) -> Option<(Family, String)> {
    super::place::stmt_key(stmt)
}

/// The placement's dependency names for a statement — the write-back's
/// preamble rule reads them. [`super::place::authored_dep_names`]
/// under a public name.
pub fn placement_deps(stmt: &Stmt) -> Vec<(Family, String)> {
    super::place::authored_dep_names(stmt)
}

/// The per-scope adoptee lists for scopes the SDL walk visited (a
/// section exists), in scope order (root, namespaces, databases) and
/// family order within a scope. Root excludes namespace statements and
/// ns scopes exclude database statements — both are scope-opening and
/// adopt through [`synthesized_sections`] at the file's tail instead
/// of mid-walk (as do the scopes the walk never visited, in full).
fn visited_scope_adoptees<'a>(
    read: &'a ReadModel,
    visited: &BTreeSet<ScopeKey>,
) -> Vec<(ScopeKey, Vec<&'a ReadEntry>)> {
    let mut out = Vec::new();
    if visited.contains(&ScopeKey::Root) && !read.root.is_empty() {
        out.push((
            ScopeKey::Root,
            read.root.leftovers_except(Family::Namespace).collect(),
        ));
    }
    for (name, entries) in &read.namespaces {
        let key = ScopeKey::Namespace(name.clone());
        if visited.contains(&key) && !entries.is_empty() {
            out.push((key, entries.leftovers_except(Family::Database).collect()));
        }
    }
    for (ns, db, entries) in &read.databases {
        let key = ScopeKey::Database(ns.clone(), db.clone());
        if visited.contains(&key) && !entries.is_empty() {
            out.push((key, entries.leftovers().collect()));
        }
    }
    out
}

/// The file-tail sections: everything scope-opening the walk could not
/// place mid-file, plus the leftovers of scopes it never visited. The
/// tail is NAMESPACE-GROUPED — each group is one ns's anchor and all
/// of its content emitted ADJACENTLY (an anchor is never separated
/// from its blocks by another group's anchor, which would re-bind
/// them): first the rest of ROOT when the walk placed no root
/// statement (users/accesses carry `on:` and `config default` hoists,
/// so they are position-safe anywhere), then one group per namespace
/// that owes content or an anchor, then the empty pseudo-ns's
/// databases — scope-safe only when no namespace statement exists at
/// all (otherwise skipped; see the scope-mismatch family). Each
/// group is one or more [`Section`]s: the anchor as the skeleton, the
/// content as adoptees for placement to order.
fn synthesized_sections(
    read: &ReadModel,
    visited: &BTreeSet<ScopeKey>,
    declared_ns: &BTreeSet<String>,
) -> Vec<Section> {
    let mut out = Vec::new();
    if !visited.contains(&ScopeKey::Root) {
        let mut section = Section::new(ScopeKey::Root);
        section.adoptees = read
            .root
            .leftovers_except(Family::Namespace)
            .map(|e| elide_and_fold(e.stmt.clone()))
            .collect();
        if !section.adoptees.is_empty() {
            out.push(section);
        }
    }
    // The group names, deduped in first-occurrence order: ROOT-ns
    // entries (orphan anchors), the interchange's ns sections, then
    // db-tag namespaces.
    let mut groups: Vec<String> = Vec::new();
    let offer = |groups: &mut Vec<String>, name: &str| {
        if !groups.iter().any(|seen| seen == name) {
            groups.push(name.to_string());
        }
    };
    for entry in read
        .root
        .families
        .get(&Family::Namespace)
        .into_iter()
        .flatten()
    {
        offer(&mut groups, &entry.name);
    }
    for (name, _) in &read.namespaces {
        if !name.is_empty() {
            offer(&mut groups, name);
        }
    }
    for (ns, _, _) in &read.databases {
        if !ns.is_empty() {
            offer(&mut groups, ns);
        }
    }
    // Each group: the anchor (the ROOT-ns entry itself when the name is
    // listed there and the SDL never declared it — the entry may carry
    // the namespace's COMMENT — else a synthesized bare one; a DECLARED
    // ns re-anchors only when it owes content, the decided
    // idempotent re-define class) plus all of its content, adjacently.
    for name in &groups {
        let declared = declared_ns.contains(name);
        let section = read.namespaces.iter().find(|(n, _)| n == name);
        let root_listed = read
            .root
            .families
            .get(&Family::Namespace)
            .into_iter()
            .flatten()
            .any(|entry| entry.name == *name);
        let mut group: Vec<Stmt> = Vec::new();
        if let Some((_, entries)) = section {
            // The walk's ns-scope position only exists for statements
            // between the namespace and its first database — when it is
            // absent the section's non-database leftovers (ns users and
            // accesses, declared or not) tail here; when it exists they
            // placed mid-walk already.
            if !visited.contains(&ScopeKey::Namespace(name.clone())) {
                group.extend(
                    entries
                        .leftovers_except(Family::Database)
                        .map(|e| elide_and_fold(e.stmt.clone())),
                );
            }
        }
        let db_blocks = ns_database_blocks(read, visited, name);
        // A declared ns owes nothing when its content all placed
        // mid-walk or visited; everything else anchors by itself.
        if declared && group.is_empty() && db_blocks.is_empty() {
            continue;
        }
        let mut section = Section::new(ScopeKey::Namespace(name.clone()));
        if root_listed && !declared {
            // The ROOT entry IS the namespace definition.
            let entry = read
                .root
                .families
                .get(&Family::Namespace)
                .into_iter()
                .flatten()
                .find(|entry| entry.name == *name);
            section.push_synthesized(match entry {
                Some(entry) => elide_and_fold(entry.stmt.clone()),
                None => namespace_stmt(name),
            });
        } else {
            section.push_synthesized(namespace_stmt(name));
        }
        section.adoptees = group;
        out.push(section);
        out.extend(db_blocks);
    }
    // The empty pseudo-ns's databases: the empty tag means "no
    // preceding namespace statement", which only a namespace-free file
    // provides. With any namespace in play the position would be
    // scope-unsafe — skipped (the scope-mismatch family).
    if declared_ns.is_empty() && groups.is_empty() {
        for (ns, db, entries) in &read.databases {
            if ns.is_empty()
                && !entries.is_empty()
                && !visited.contains(&ScopeKey::Database(ns.clone(), db.clone()))
            {
                let mut section = Section::new(ScopeKey::Database(ns.clone(), db.clone()));
                section.push_synthesized(database_stmt(db));
                section.adoptees = entries
                    .leftovers()
                    .map(|e| elide_and_fold(e.stmt.clone()))
                    .collect();
                out.push(section);
            }
        }
    }
    out
}

/// The database blocks one ns section owes: every database its section
/// lists or carries a db object for, whose scope the walk never
/// visited — a bare `database` statement as the skeleton, the db
/// scope's adoptees as the content (single representation: the
/// ns-scope Family::Database leftovers name exactly these).
fn ns_database_blocks(read: &ReadModel, visited: &BTreeSet<ScopeKey>, ns: &str) -> Vec<Section> {
    let ns_entries = read
        .namespaces
        .iter()
        .find(|(name, _)| name == ns)
        .map(|(_, entries)| entries);
    let mut names = ns_entries
        .map(|entries| entries.leftover_names(Family::Database))
        .unwrap_or_default();
    for (_, db, _) in read.databases.iter().filter(|(sns, _, _)| sns == ns) {
        if !names.iter().any(|name| name == db) {
            names.push(db.clone());
        }
    }
    let mut out = Vec::new();
    for db in names {
        if visited.contains(&ScopeKey::Database(ns.to_string(), db.clone())) {
            continue;
        }
        let mut section = Section::new(ScopeKey::Database(ns.to_string(), db.clone()));
        section.push_synthesized(database_stmt(&db));
        if let Some((_, _, db_entries)) = read
            .databases
            .iter()
            .find(|(sns, sdb, _)| sns == ns && sdb == &db)
        {
            section.adoptees = db_entries
                .leftovers()
                .map(|e| elide_and_fold(e.stmt.clone()))
                .collect();
        }
        out.push(section);
    }
    out
}

fn namespace_stmt(name: &str) -> Stmt {
    Stmt::Namespace(Namespace {
        name: Ident::new(name),
        keyword: Carried::default(),
        comment: None,
        comments: Vec::new(),
        clause_comments: Vec::new(),
    })
}

fn database_stmt(name: &str) -> Stmt {
    Stmt::Database(Database {
        name: Ident::new(name),
        keyword: Carried::default(),
        strict: false,
        changefeed: None,
        comment: None,
        comments: Vec::new(),
        trailing_comments: Vec::new(),
        clause_comments: Vec::new(),
    })
}

/// The interchange scope for a statement's own `on:` level (users and
/// accesses): root, the current namespace's, or the current database's.
fn level_scope(level: Level, ns: String, db: String) -> ScopeKey {
    match level {
        Level::Root => ScopeKey::Root,
        Level::Namespace => ScopeKey::Namespace(ns),
        Level::Database => ScopeKey::Database(ns, db),
    }
}

fn take_for_scope(read: &mut ReadModel, scope: &ScopeKey, stmt: &Stmt) -> Option<ReadEntry> {
    let entries = match scope {
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
    };
    entries.and_then(|entries| entries.take(stmt))
}

// ---------------------------------------------------------------------
// The per-statement verbs: elide → fold → preserve → carry
// ---------------------------------------------------------------------

/// One matched statement's merge: the read state is the base (ADOPT),
/// then ELIDE default-equal clauses, FOLD the synthesized shapes, and
/// PRESERVE + spelling-carry from the SDL side. A matched table's
/// spreads reconstruct last ([`restore_spreads`]) — the authored
/// structure the read state cannot carry.
fn merge_stmt(
    sdl: &Stmt,
    entry: &ReadEntry,
    mixins: &Mixins<'_>,
    resolutions: &AliasResolutions,
) -> Stmt {
    let mut merged = elide_and_fold(entry.stmt.clone());
    merged.carry_comment_fields_from(sdl);
    preserve_and_carry(sdl, &mut merged, resolutions);
    if let (Stmt::Table(sdl_table), Stmt::Table(merged_table)) = (sdl, &mut merged) {
        restore_spreads(sdl_table, merged_table, mixins, resolutions);
    }
    merged
}

/// The spread-preserving reconstruction (spec/mixin.md's reverse-path
/// rule): a table's spread entries re-attach to the merged (read
/// base) table, and each LANDING member pairs against the read child
/// of its key — the same statements expansion emits ([`landing_members`],
/// one resolution home), the same convergence the differ rules
/// ([`state_equal`], one notion on both sides of the pipeline):
///
/// - **state-equal** — the read child DROPS: the spread entry covers
///   it (the merged file still declares it, through the spread). A
///   member's kinds resolve their alias references first (the
///   EXPANDED form, the differ's own posture), so a child holding the
///   expansion is covered by the spread that names the alias.
/// - **diverged** — the read child STAYS as an explicit member: the
///   materialization, the overshadow rule performing the adoption
///   (the spread survives with its member overshadowed
///   position-independently — the writer prints the override before
///   the spread, order being semantic-free there). Idempotent by
///   construction — round two sees the override as an explicit
///   member, its spread counterpart no longer lands, and the pair
///   converges.
/// - **no read child** — nothing to drop or materialize: the member
///   rides the spread (the mixin statement stays authored, so it
///   stays declared — apply-pending, the differ's finding on the
///   member, never a mixin-level finding).
///
/// Every spread entry re-attaches as an authored clone (comments and
/// rule-5 anchors ride; relative order between spreads is semantic
/// and preserved). A table the read side lacks never passes through
/// here at all — no read entry matched, the authored statement rides
/// the merge untouched.
fn restore_spreads(
    sdl_table: &Table,
    merged: &mut Table,
    mixins: &Mixins<'_>,
    resolutions: &AliasResolutions,
) {
    if !sdl_table
        .nested
        .iter()
        .any(|stmt| matches!(stmt, Stmt::Spread(_)))
    {
        return;
    }
    for member in landing_members(sdl_table, mixins).concat() {
        // The member compares in its EXPANDED form (the differ's own
        // posture — its kinds' alias references resolve first), so a
        // read child holding the expansion is covered by the spread.
        let mut member = member;
        rewrite_stmt_kinds(&mut member, resolutions);
        let key = classify(&member);
        if let Some(at) = merged
            .nested
            .iter()
            .position(|child| classify(child) == key)
            && state_equal(&member, &merged.nested[at])
        {
            merged.nested.remove(at);
        }
    }
    for entry in &sdl_table.nested {
        if let Stmt::Spread(spread) = entry {
            merged.nested.push(Stmt::Spread(spread.clone()));
        }
    }
}

/// The adoptee path (no SDL side): elision + folds only.
pub(super) fn elide_and_fold(stmt: Stmt) -> Stmt {
    match stmt {
        Stmt::Table(mut table) => {
            table.permissions = elide_table_permissions(table.permissions);
            table.nested = table.nested.into_iter().map(elide_and_fold).collect();
            fold_table_synthesized(&mut table);
            Stmt::Table(table)
        }
        Stmt::Field(mut field) => {
            field.permissions = elide_field_permissions(field.permissions);
            Stmt::Field(field)
        }
        Stmt::Index(mut index) => {
            index.kind = elide_index_kind(index.kind);
            Stmt::Index(index)
        }
        Stmt::Event(mut event) => {
            elide_event(&mut event);
            Stmt::Event(event)
        }
        Stmt::User(mut user) => {
            elide_user(&mut user);
            Stmt::User(user)
        }
        Stmt::Access(mut access) => {
            elide_access_durations(&mut access.duration);
            Stmt::Access(access)
        }
        Stmt::Sequence(mut sequence) => {
            elide_sequence(&mut sequence);
            Stmt::Sequence(sequence)
        }
        Stmt::Param(mut param) => {
            elide_full_permission(&mut param.permissions);
            Stmt::Param(param)
        }
        Stmt::Bucket(mut bucket) => {
            elide_full_permission(&mut bucket.permissions);
            Stmt::Bucket(bucket)
        }
        Stmt::Function(mut function) => {
            elide_full_permission(&mut function.permissions);
            Stmt::Function(function)
        }
        Stmt::Module(mut module) => {
            elide_full_permission(&mut module.permissions);
            Stmt::Module(module)
        }
        Stmt::Config(Config::Api(mut api)) => {
            elide_full_permission(&mut api.permissions);
            Stmt::Config(Config::Api(api))
        }
        // graphql/default carry the resolved state directly — nothing
        // elides (the writer prints the minimal form).
        other => other,
    }
}

/// `permissions: full` on the single-value statements is SurrealQL's
/// default and elides.
fn elide_full_permission(permissions: &mut Option<Permission>) {
    if *permissions == Some(Permission::Full) {
        *permissions = None;
    }
}

/// The matched path's SDL-side verbs: PRESERVE what INFO drops but SDL
/// means, and carry authored spellings whose values are unchanged.
fn preserve_and_carry(sdl: &Stmt, merged: &mut Stmt, resolutions: &AliasResolutions) {
    match (sdl, merged) {
        (Stmt::Namespace(sdl_ns), Stmt::Namespace(ns)) => {
            // The `ns` keyword alias is pure provenance — carried.
            ns.keyword = sdl_ns.keyword.clone();
        }
        (Stmt::Database(sdl_db), Stmt::Database(db)) => {
            // INFO drops `strict` while it is active — preserve.
            db.strict = sdl_db.strict;
            db.keyword = sdl_db.keyword.clone();
        }
        (Stmt::User(sdl_user), Stmt::User(user)) => {
            // INFO shows `PASSHASH '[REDACTED]'`/`''` — the SDL slots
            // win wholesale (secrets are never adopted as literals;
            // PASSSCRAM is stripped at read, so only the SDL side
            // ever carries it).
            user.pass = sdl_user.pass.clone();
            user.scram = sdl_user.scram.clone();
            carry_on(&sdl_user.on, &mut user.on);
        }
        (Stmt::Access(sdl_access), Stmt::Access(access)) => {
            preserve_access(sdl_access, access);
            carry_on(&sdl_access.on, &mut access.on);
        }
        (Stmt::Analyzer(sdl_analyzer), Stmt::Analyzer(analyzer)) => {
            carry_filters(&sdl_analyzer.filters, &mut analyzer.filters);
        }
        (Stmt::Index(sdl_index), Stmt::Index(index)) => {
            // CONCURRENTLY is a transient build directive — never in
            // INFO, always preserved.
            index.concurrently = sdl_index.concurrently;
            index.fields_key = sdl_index.fields_key.clone();
        }
        (Stmt::Field(sdl_field), Stmt::Field(field)) => {
            carry_field_kind(sdl_field, field, resolutions);
        }
        (Stmt::Function(sdl_fn), Stmt::Function(function)) => {
            carry_function_kinds(sdl_fn, function, resolutions);
        }
        (Stmt::Table(sdl_table), Stmt::Table(table)) => {
            preserve_table_permissions(sdl_table, table);
            // Nested definitions run the same SDL-side verbs against
            // their read pair (the field's kind spelling, the index's
            // concurrently + fields key — CONCURRENTLY never renders
            // in INFO), and carry their comments like the parent.
            for nested in &mut table.nested {
                let key = classify(nested);
                if let Some(sdl_nested) = sdl_table.nested.iter().find(|s| classify(s) == key) {
                    nested.carry_comment_fields_from(sdl_nested);
                    preserve_and_carry(sdl_nested, nested, resolutions);
                }
            }
        }
        _ => {}
    }
}

/// Rule 4's kind comparison, alias-aware: the authored text resolves
/// against the alias table first, so an alias-bearing kind carries
/// exactly when its EXPANSION equals the read state. A reference
/// resolving to nothing rides through and the comparison fails — the
/// read state's canonical spelling wins (the dirty-document posture;
/// the order pass owns that finding).
fn kinds_converge(authored: &str, read: &str, resolutions: &AliasResolutions) -> bool {
    kind_text_equal(&resolve_kind_text(authored, resolutions), read)
}

/// The authored kind spelling rides when the kinds are algebra-equal
/// through the alias table (rule 4 — value equality, spelling kept;
/// the read text is the canonical spelling otherwise).
fn carry_field_kind(sdl_field: &Field, field: &mut Field, resolutions: &AliasResolutions) {
    if let (Some(sdl_kind), Some(kind)) = (&sdl_field.kind, &field.kind)
        && kinds_converge(&sdl_kind.text, &kind.text, resolutions)
    {
        field.kind = Some(sdl_kind.clone());
    }
}

/// The same spelling carry over a function's argument and return
/// kinds (rule 4; the positions the forward rewrite
/// [`rewrite_stmt_kinds`] covers). Argument pairs ride positionally —
/// a converged pair carries its authored spelling, a divergent one
/// keeps the read state, and a count mismatch simply stops the zip.
fn carry_function_kinds(sdl: &Function, function: &mut Function, resolutions: &AliasResolutions) {
    for ((_, sdl_kind), (_, kind)) in sdl.args.iter().zip(function.args.iter_mut()) {
        if kinds_converge(&sdl_kind.text, &kind.text, resolutions) {
            *kind = sdl_kind.clone();
        }
    }
    if let (Some(sdl_returns), Some(returns)) = (&sdl.returns, &mut function.returns)
        && kinds_converge(&sdl_returns.text, &returns.text, resolutions)
    {
        *returns = sdl_returns.clone();
    }
}

fn preserve_access(sdl: &Access, access: &mut Access) {
    match (&sdl.kind, &mut access.kind) {
        // A bearer's `jwt { … }` is stored but never rendered —
        // preserve SDL's whole sub-block.
        (
            AccessKind::Bearer {
                subject: _,
                jwt: sdl_jwt,
            },
            AccessKind::Bearer { jwt, .. },
        ) => {
            if jwt.is_none() {
                *jwt = sdl_jwt.clone();
            }
        }
        // Record's printed signature ADOPTS (the sentinel rule made it
        // the canonical literal); SDL's shape
        // only fills the impossible absence.
        (AccessKind::Record { jwt: sdl_jwt, .. }, AccessKind::Record { jwt, .. }) => {
            if jwt.is_none() {
                *jwt = sdl_jwt.clone();
            }
        }
        // The issuer's `algorithm:` never renders — preserve it on the
        // URL path (where it is SDL-meaningful and must be emitted).
        // On the key path it is forced-equal to the verify algorithm —
        // dropped.
        (
            AccessKind::Jwt {
                source:
                    JwtSource {
                        verify: JwtVerify::Url(_),
                        audience: _,
                        issuer: sdl_issuer,
                    },
            },
            AccessKind::Jwt {
                source:
                    JwtSource {
                        verify: JwtVerify::Url(_),
                        audience: _,
                        issuer,
                    },
            },
        ) => {
            if let (Some(sdl_issuer), Some(issuer)) = (sdl_issuer, issuer)
                && issuer.algorithm.is_none()
            {
                issuer.algorithm = sdl_issuer.algorithm;
            }
        }
        _ => {}
    }
}

/// The table permissions preserve: the emitter elides `FOR delete
/// FULL` (INFO cannot distinguish delete-full from delete-none —
/// live-probed), so an SDL definition carrying delete-full must never
/// be downgraded by the adopt. SDL's whole clause wins there; the
/// read's view is a lossy projection.
fn preserve_table_permissions(sdl: &Table, table: &mut Table) {
    let Some(sdl_perms) = &sdl.permissions else {
        return;
    };
    if !carries_delete_full(sdl_perms) {
        return;
    }
    // The read's clause may have elided to absent (NONE is the table
    // default) — that absence is still not a delete-full.
    let read_has_delete_full = table.permissions.as_ref().is_some_and(carries_delete_full);
    if read_has_delete_full {
        return;
    }
    // The delete side re-inserts as its own group; every other
    // operation's ADOPTED value stays (the preserve is scoped to the
    // ambiguous op — the emitter elided it from INFO, nothing else).
    let group = PermissionGroup {
        ops: vec![PermissionOp::Delete],
        value: Permission::Full,
    };
    match &mut table.permissions {
        Some(OpPermissions::Groups(groups)) => groups.push(group),
        _ => table.permissions = Some(OpPermissions::Groups(vec![group])),
    }
}

fn carries_delete_full(perms: &OpPermissions) -> bool {
    match perms {
        OpPermissions::Full => true,
        OpPermissions::Groups(groups) => groups.iter().any(|group| {
            group.ops.contains(&PermissionOp::Delete) && group.value == Permission::Full
        }),
        OpPermissions::None => false,
    }
}

fn carry_on(sdl: &Authored<Level>, merged: &mut Authored<Level>) {
    // Value-equal (the only comparison `Authored` makes): the authored
    // alias spelling rides.
    if sdl == merged {
        *merged = *sdl;
    }
}

fn carry_filters(sdl: &Option<Vec<Filter>>, merged: &mut Option<Vec<Filter>>) {
    if let Some(sdl_filters) = sdl
        && let Some(filters) = merged
        && sdl_filters == filters
    {
        *merged = Some(sdl_filters.clone());
    }
}

// ---- ELIDE ----

/// The SurrealQL defaults the pinned printer materializes, as canon
/// text (the readers carry the printed text faithfully).
const DEFAULT_USER_TOKEN: &str = "1h";
const DEFAULT_USER_SESSION: &str = "NONE";
const DEFAULT_ACCESS_GRANT: &str = "4w2d"; // 30d value-normalized (live-probed)
const DEFAULT_ACCESS_TOKEN: &str = "1h";
const DEFAULT_ACCESS_SESSION: &str = "NONE";

fn elide_event(event: &mut Event) {
    // `WHEN true` materializes for every event.
    if event.when.as_ref().is_some_and(|when| when.canon == "true") {
        event.when = None;
    }
    // The async flag stays (bare `async;` prints `Async{None, None}`);
    // its 1/3 sides elide.
    if let Some(r#async) = &mut event.r#async {
        if r#async.retry == Some(1) {
            r#async.retry = None;
        }
        if r#async.maxdepth == Some(3) {
            r#async.maxdepth = None;
        }
    }
}

fn elide_user(user: &mut User) {
    if user.roles == vec![Role::Viewer] {
        user.roles = Vec::new();
    }
    if user
        .token_duration
        .as_ref()
        .is_some_and(|d| d.canon == DEFAULT_USER_TOKEN)
    {
        user.token_duration = None;
    }
    if user
        .session_duration
        .as_ref()
        .is_some_and(|d| d.canon == DEFAULT_USER_SESSION)
    {
        user.session_duration = None;
    }
}

fn elide_access_durations(duration: &mut Option<AccessDuration>) {
    let Some(sides) = duration.as_mut() else {
        return;
    };
    if sides
        .grant
        .as_ref()
        .is_some_and(|d| d.canon == DEFAULT_ACCESS_GRANT)
    {
        sides.grant = None;
    }
    if sides
        .token
        .as_ref()
        .is_some_and(|d| d.canon == DEFAULT_ACCESS_TOKEN)
    {
        sides.token = None;
    }
    if sides
        .session
        .as_ref()
        .is_some_and(|d| d.canon == DEFAULT_ACCESS_SESSION)
    {
        sides.session = None;
    }
    if sides.grant.is_none() && sides.token.is_none() && sides.session.is_none() {
        *duration = None;
    }
}

fn elide_sequence(sequence: &mut Sequence) {
    if sequence.batch.as_ref().is_some_and(|b| b.canon == "1000") {
        sequence.batch = None;
    }
    if sequence.start.as_ref().is_some_and(|s| s.canon == "0") {
        sequence.start = None;
    }
}

/// The field's permissions: the printer's equal-value merge re-spells
/// to the shortest SDL form (an all-ops group is the word), then the
/// FULL default elides.
fn elide_field_permissions(permissions: Option<OpPermissions>) -> Option<OpPermissions> {
    const FIELD_OPS: &[PermissionOp] = &[
        PermissionOp::Select,
        PermissionOp::Create,
        PermissionOp::Update,
    ];
    let permissions = respell_all_ops_group(permissions, FIELD_OPS);
    let permissions = drop_groups_at(permissions, Permission::Full);
    match permissions {
        Some(OpPermissions::Full) => None,
        other => other,
    }
}

/// The table's: NONE is the default — the word elides and NONE-valued
/// groups drop; FULL is SDL-meaningful.
fn elide_table_permissions(permissions: Option<OpPermissions>) -> Option<OpPermissions> {
    const TABLE_OPS: &[PermissionOp] = &[
        PermissionOp::Select,
        PermissionOp::Create,
        PermissionOp::Update,
        PermissionOp::Delete,
    ];
    let permissions = respell_all_ops_group(permissions, TABLE_OPS);
    let permissions = drop_groups_at(permissions, Permission::None);
    match permissions {
        Some(OpPermissions::None) => None,
        other => other,
    }
}

/// Groups whose value equals the statement's SurrealQL default drop —
/// the engine materializes every operation's side and canonical SDL is
/// minimal (the README elide bullet). All-default groups leave the
/// clause absent.
fn drop_groups_at(
    permissions: Option<OpPermissions>,
    default: Permission,
) -> Option<OpPermissions> {
    match permissions {
        Some(OpPermissions::Groups(groups)) => {
            let kept: Vec<_> = groups
                .into_iter()
                .filter(|group| group.value != default)
                .collect();
            match kept.is_empty() {
                true => None,
                false => Some(OpPermissions::Groups(kept)),
            }
        }
        other => other,
    }
}

/// One group covering exactly the full op set with one value is the
/// single-value word form.
fn respell_all_ops_group(
    permissions: Option<OpPermissions>,
    all: &[PermissionOp],
) -> Option<OpPermissions> {
    match permissions {
        Some(OpPermissions::Groups(groups)) => match groups.as_slice() {
            [group] if covers_exactly(&group.ops, all) => Some(match group.value {
                Permission::None => OpPermissions::None,
                Permission::Full => OpPermissions::Full,
                Permission::Expr(_) => OpPermissions::Groups(groups),
            }),
            _ => Some(OpPermissions::Groups(groups)),
        },
        other => other,
    }
}

fn covers_exactly(ops: &[PermissionOp], all: &[PermissionOp]) -> bool {
    ops.len() == all.len() && all.iter().all(|op| ops.contains(op))
}

/// The index kind's always-materialized shapes elide at their defaults
/// (spec/index.md): fulltext's `ANALYZER like` + `BM25(1.2,0.75)`;
/// HNSW/DISKANN's full param set — the derived `m0`/`lm` recompute
/// from the (possibly defaulted) `m`, `lm` at the pinned relative
/// epsilon 1e-9.
fn elide_index_kind(kind: IndexKind) -> IndexKind {
    match kind {
        IndexKind::FullText {
            analyzer,
            bm25,
            highlights,
        } => {
            let analyzer = match analyzer {
                Some(name) if name.raw == "like" => None,
                other => other,
            };
            let bm25 = match bm25 {
                Some(bm25) if bm25.0 == 1.2 && bm25.1 == 0.75 => None,
                other => other,
            };
            IndexKind::FullText {
                analyzer,
                bm25,
                highlights,
            }
        }
        IndexKind::Hnsw {
            dimension,
            distance,
            vector_type,
            m,
            m0,
            lm,
            efc,
            extend_candidates,
            keep_pruned_connections,
            hashed_vector,
        } => {
            let m = elide_u16_at_default(m, DEFAULT_HNSW_M);
            let m0_default = 2 * m.unwrap_or(DEFAULT_HNSW_M);
            IndexKind::Hnsw {
                dimension,
                distance: elide_distance(distance),
                vector_type: elide_vector_type(vector_type),
                m,
                m0: elide_u16_at_default(m0, m0_default),
                lm: elide_lm(lm, m.unwrap_or(DEFAULT_HNSW_M)),
                efc: elide_u16_at_default(efc, DEFAULT_HNSW_EFC),
                extend_candidates,
                keep_pruned_connections,
                hashed_vector,
            }
        }
        IndexKind::DiskAnn {
            dimension,
            distance,
            vector_type,
            degree,
            l_build,
            alpha,
            hashed_vector,
        } => IndexKind::DiskAnn {
            dimension,
            distance: elide_distance(distance),
            vector_type: elide_vector_type(vector_type),
            degree: elide_u16_at_default(degree, DEFAULT_DISKANN_DEGREE),
            l_build: elide_u16_at_default(l_build, DEFAULT_DISKANN_L_BUILD),
            alpha: elide_alpha(alpha),
            hashed_vector,
        },
        other => other,
    }
}

const DEFAULT_HNSW_M: u16 = 12;
const DEFAULT_HNSW_EFC: u16 = 150;
const DEFAULT_DISKANN_DEGREE: u16 = 64;
const DEFAULT_DISKANN_L_BUILD: u16 = 100;

fn elide_u16_at_default(value: Option<u16>, default: u16) -> Option<u16> {
    match value {
        Some(value) if value == default => None,
        other => other,
    }
}

fn elide_distance(distance: Option<crate::ast::Distance>) -> Option<crate::ast::Distance> {
    match distance {
        Some(crate::ast::Distance::Euclidean) => None,
        other => other,
    }
}

fn elide_vector_type(
    vector_type: Option<crate::ast::VectorType>,
) -> Option<crate::ast::VectorType> {
    match vector_type {
        Some(crate::ast::VectorType::F32) => None,
        other => other,
    }
}

fn elide_alpha(alpha: Option<crate::ast::Float>) -> Option<crate::ast::Float> {
    match alpha {
        Some(alpha) if alpha.0 == 1.2 => None,
        other => other,
    }
}

/// `lm` elides when it equals the derived `1/ln(m)` within the pinned
/// relative epsilon 1e-9 (spec/index.md — never string-compare). A
/// diverged derivation (m = 1: ln 1 = 0) never elides — the engine
/// prints `LM Infinity` there and the reader raw-routes the statement
/// before this comparison could run, but the guard keeps the
/// comparison total on its own terms too.
fn elide_lm(lm: Option<crate::ast::Float>, m: u16) -> Option<crate::ast::Float> {
    let derived = 1.0 / (m as f64).ln();
    if !derived.is_finite() {
        return lm;
    }
    match lm {
        Some(lm) if (lm.0 - derived).abs() <= 1e-9 * derived.abs() => None,
        other => other,
    }
}

// ---- FOLD ----

/// The table's synthesized shapes fold away (spec/table.md +
/// spec/field.md reverse sections): relation `in`/`out` fields whose
/// kind equals the endpoint list's kind with nothing but default
/// clauses, and server-synthesized `name.*` subfields deepest-first.
fn fold_table_synthesized(table: &mut Table) {
    fold_relation_fields(table);
    fold_dot_star_fields(&mut table.nested);
}

/// A relation's materialized `in`/`out` field definitions fold into
/// the relation shape, independent per side. The expected kind comes
/// from the (already-adopted) relation header: `record<from | …>`,
/// bare `record` for an unnamed side.
fn fold_relation_fields(table: &mut Table) {
    let TableKind::Relation { from, to, .. } = &table.kind else {
        return;
    };
    let sides = [("in", endpoint_kind(from)), ("out", endpoint_kind(to))];
    for (name, expected) in sides {
        table.nested.retain(|nested| {
            !matches!(
                nested,
                Stmt::Field(field)
                    if field.name.sole_name().is_some_and(|n| n.raw == name)
                        && field
                            .kind
                            .as_ref()
                            .is_some_and(|k| kind_text_equal(&k.text, &expected))
                        && field_is_default_shaped(field)
            )
        });
    }
}

/// The endpoint list's record kind text (`record`, or
/// `record<a | b>` in the endpoints' authored order).
pub(super) fn endpoint_kind(side: &Option<Vec<Ident>>) -> String {
    match side {
        None => "record".to_string(),
        Some(endpoints) => {
            let names: Vec<String> = endpoints.iter().map(|e| e.raw.clone()).collect();
            format!("record<{}>", names.join(" | "))
        }
    }
}

/// A folded `.*`/`in`/`out` entry carries only default clauses — after
/// elision that is an absent permissions clause and nothing else
/// (COMMENT included; `flexible` compares at the call site).
fn field_is_default_shaped(field: &Field) -> bool {
    field.permissions.is_none() && field_is_default_shaped_except_permissions(field)
}

/// The same shape with the permissions arm left to the caller — the
/// reader's implied-LIGHTWEIGHT check reads the pre-elision faithful
/// print (`PERMISSIONS FULL`, the field default the printer
/// materializes) where the merge sees the elided absence.
pub(super) fn field_is_default_shaped_except_permissions(field: &Field) -> bool {
    field.default.is_none()
        && field.value.is_none()
        && field.assert.is_none()
        && field.computed.is_none()
        && !field.readonly
        && field.reference.is_none()
        && field.graphql.is_none()
        && field.comment.is_none()
}

/// The server-synthesized `name.*` subfields fold deepest-first
/// (README item 4): walk every field's inner-kind chain — dotted paths
/// included (`a.b: array<int>` synthesizes `a.b.*`) — and drop the
/// chain's entries whose kind matches under the algebra and whose
/// `flexible` equals the parent's (inherited — it never blocks
/// folding). An entry with its own clauses or a divergent kind is
/// user-meaningful and kept (the chain stops there).
fn fold_dot_star_fields(nested: &mut Vec<Stmt>) {
    let chain_starts: Vec<String> = nested
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Field(field) if field.kind.is_some() => Some(field_path_key(&field.name)),
            _ => None,
        })
        .collect();
    for name in chain_starts {
        let Some(Stmt::Field(parent)) = nested
            .iter()
            .find(|stmt| matches!(stmt, Stmt::Field(f) if field_path_key(&f.name) == name))
        else {
            // An earlier chain step already folded this entry away.
            continue;
        };
        let Some(parent_kind_ast) = parent.kind.as_ref().and_then(|k| parse_kind_ast(&k.text))
        else {
            continue;
        };
        let mut parent_kind = parent_kind_ast;
        let mut parent_flexible = parent.flexible;
        let mut path = name;
        // The next synthesized level: `path.*`, kind = the parent's
        // inner kind.
        while let Some(inner) = inner_kind(&parent_kind) {
            let child_path = format!("{path}.*");
            // Copy the child's decision inputs out of the borrow — the
            // fold below mutates the list.
            let child = nested.iter().find_map(|stmt| match stmt {
                Stmt::Field(f) if field_path_key(&f.name) == child_path => Some((
                    f.kind.as_ref().and_then(|k| parse_kind_ast(&k.text)),
                    f.flexible,
                    field_is_default_shaped(f),
                )),
                _ => None,
            });
            let Some((Some(child_kind), child_flexible, default_shaped)) = child else {
                break;
            };
            if !kinds_equal(&child_kind, &inner)
                || child_flexible != parent_flexible
                || !default_shaped
            {
                break;
            }
            nested.retain(
                |stmt| !matches!(stmt, Stmt::Field(f) if field_path_key(&f.name) == child_path),
            );
            // The folded child is the next comparison's parent.
            parent_kind = child_kind;
            parent_flexible = child_flexible;
            path = child_path;
        }
    }
}

/// Rule 4's kind comparison over kind texts.
pub(super) fn kind_text_equal(a: &str, b: &str) -> bool {
    match (parse_kind_ast(a), parse_kind_ast(b)) {
        (Some(a), Some(b)) => kinds_equal(&a, &b),
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Bm25, FieldPathSeg, Float, Kind, PermissionGroup};

    /// The spread-preserving merge (spec/mixin.md's reverse-path
    /// rule): against a converged read state the spread
    /// survives with its members covered; a diverged member
    /// materializes as the explicit override (the overshadow rule
    /// performing the adoption) while the converged siblings stay
    /// covered; an explicit SDL member pairs as before; and the
    /// loop is idempotent.
    #[test]
    fn the_merge_preserves_spreads_per_member() {
        let authored = "mixin stamp {\n  created_at: datetime;\n  updated_at: datetime;\n};\n\
                        table entry schemafull {\n  ...stamp;\n  title: string;\n};\n";
        let (sdl, diags) = crate::parser::parse_file(authored);
        assert!(diags.is_empty(), "{diags:?}");
        let merge_once = |read_sdl: &str| {
            let (read, read_diags) = crate::parser::parse_file(read_sdl);
            assert!(read_diags.is_empty(), "{read_diags:?}");
            let entry = ReadEntry {
                name: "entry".to_string(),
                stmt: read.items.into_iter().next().expect("one statement"),
                markers: crate::info::read::Markers::default(),
            };
            let mut model = ReadModel::default();
            model.mark_ingested(ScopeKey::Root);
            model.root.push(entry);
            let merged = merge(&sdl, &model);
            crate::info::write::write_canonical(&merged)
        };
        // Converged: the spread covers both members, the explicit
        // field stays.
        assert_eq!(
            merge_once(
                "table entry {\n  schemafull;\n  field created_at: datetime;\n  \
                 field updated_at: datetime;\n  field title: string;\n};"
            ),
            "mixin stamp {\n  field created_at: datetime;\n  field updated_at: datetime;\n};\n\
             \ntable entry {\n  schemafull;\n  ...stamp;\n  field title: string;\n};\n"
        );
        // Diverged member: the read child materializes as the
        // override; the spread (and the covered sibling) survives.
        let diverged_output = merge_once(
            "table entry {\n  schemafull;\n  field created_at: datetime { default: time::now(); };\n  \
             field updated_at: datetime;\n  field title: string;\n};",
        );
        assert_eq!(
            diverged_output,
            "mixin stamp {\n  field created_at: datetime;\n  field updated_at: datetime;\n};\n\
             \ntable entry {\n  schemafull;\n  ...stamp;\n  field created_at: datetime {\n    \
             default: time::now();\n  };\n  field title: string;\n};\n"
        );
        // Idempotence: the merge of the merged output (with the
        // override now explicit — its spread counterpart no longer
        // lands) reproduces it byte-for-byte.
        let (round1, diags) = crate::parser::parse_file(&diverged_output);
        assert!(diags.is_empty(), "{diags:?}");
        let (read, _) = crate::parser::parse_file(
            "table entry {\n  schemafull;\n  field created_at: datetime { default: time::now(); };\n  \
             field updated_at: datetime;\n  field title: string;\n};",
        );
        let entry = ReadEntry {
            name: "entry".to_string(),
            stmt: read.items.into_iter().next().expect("one statement"),
            markers: crate::info::read::Markers::default(),
        };
        let mut model = ReadModel::default();
        model.mark_ingested(ScopeKey::Root);
        model.root.push(entry);
        assert_eq!(
            crate::info::write::write_canonical(&merge(&round1, &model)),
            diverged_output
        );
        // A read state MISSING a member leaves it declared through
        // the spread (apply-pending — the differ's finding on the
        // member, never a mixin-level finding).
        assert_eq!(
            merge_once(
                "table entry {\n  schemafull;\n  field created_at: datetime;\n  \
                 field title: string;\n};"
            ),
            "mixin stamp {\n  field created_at: datetime;\n  field updated_at: datetime;\n};\n\
             \ntable entry {\n  schemafull;\n  ...stamp;\n  field title: string;\n};\n"
        );
    }

    /// The reverse path's alias carry (spec/type-alias.md): authored
    /// alias spellings ride when the read state algebra-equals the
    /// expansion — a field's kind and a function's argument/return
    /// kinds alike; divergence adopts the read state; and the loop is
    /// idempotent (merging the merged output against the same read
    /// state reproduces it byte-for-byte).
    #[test]
    fn the_merge_carries_alias_spellings() {
        let model_from = |read_sdl: &str| {
            let (read, diags) = crate::parser::parse_file(read_sdl);
            assert!(diags.is_empty(), "{diags:?}");
            let mut model = ReadModel::default();
            model.mark_ingested(ScopeKey::Root);
            for stmt in read.items {
                let (_, key) = classify(&stmt);
                model.root.push(ReadEntry {
                    name: key,
                    stmt,
                    markers: crate::info::read::Markers::default(),
                });
            }
            model
        };
        let authored = "type Email = string;\n\
                        table user schemafull {\n  contact: Email;\n};\n\
                        fn f($e: Email) -> Email { return $e; };\n";
        let (sdl, diags) = crate::parser::parse_file(authored);
        assert!(diags.is_empty(), "{diags:?}");
        let model = model_from(
            "table user {\n  schemafull;\n  field contact: string;\n};\n\
             fn f($e: string) -> string { return $e; };",
        );
        let once = crate::info::write::write_canonical(&merge(&sdl, &model));
        // The authored references ride at every kind position, and the
        // type statement rides untouched (no read counterpart).
        assert!(once.contains("type Email = string;"), "{once}");
        assert!(once.contains("field contact: Email;"), "{once}");
        assert!(once.contains("fn f($e: Email) -> Email"), "{once}");
        // Idempotence: the merged output re-merges to itself.
        let (reparsed, diags) = crate::parser::parse_file(&once);
        assert!(diags.is_empty(), "{once}");
        assert_eq!(
            crate::info::write::write_canonical(&merge(&reparsed, &model)),
            once
        );
        // Divergence adopts the read state: the expansion no longer
        // equals it, the canonical spelling wins.
        let diverged_model = model_from(
            "table user {\n  schemafull;\n  field contact: int;\n};\n\
             fn f($e: int) -> int { return $e; };",
        );
        let diverged = crate::info::write::write_canonical(&merge(&sdl, &diverged_model));
        assert!(diverged.contains("field contact: int;"), "{diverged}");
        assert!(diverged.contains("fn f($e: int) -> int"), "{diverged}");
    }

    /// The spread-member convergence rides the expansion too: a member
    /// whose kind references an alias compares against the read child
    /// in its EXPANDED form, so the covered child drops; a divergent
    /// child materializes as the explicit override. The authored
    /// spelling lives only in the mixin (the spread re-attaches as an
    /// authored clone).
    #[test]
    fn a_spread_member_compares_in_its_expanded_form() {
        let authored = "type Stamp = datetime;\n\
                        mixin stamp {\n  created_at: Stamp;\n};\n\
                        table entry schemafull {\n  ...stamp;\n};\n";
        let (sdl, diags) = crate::parser::parse_file(authored);
        assert!(diags.is_empty(), "{diags:?}");
        let merge_once = |read_sdl: &str| {
            let (read, read_diags) = crate::parser::parse_file(read_sdl);
            assert!(read_diags.is_empty(), "{read_diags:?}");
            let entry = ReadEntry {
                name: "entry".to_string(),
                stmt: read.items.into_iter().next().expect("one statement"),
                markers: crate::info::read::Markers::default(),
            };
            let mut model = ReadModel::default();
            model.mark_ingested(ScopeKey::Root);
            model.root.push(entry);
            crate::info::write::write_canonical(&merge(&sdl, &model))
        };
        // Converged: the expanded member state-equals the read child —
        // the spread covers it.
        assert_eq!(
            merge_once("table entry {\n  schemafull;\n  field created_at: datetime;\n};"),
            "type Stamp = datetime;\n\
             \nmixin stamp {\n  field created_at: Stamp;\n};\n\
             \ntable entry {\n  schemafull;\n  ...stamp;\n};\n"
        );
        // Diverged: the read child materializes as the override, the
        // spread survives.
        assert_eq!(
            merge_once("table entry {\n  schemafull;\n  field created_at: int;\n};"),
            "type Stamp = datetime;\n\
             \nmixin stamp {\n  field created_at: Stamp;\n};\n\
             \ntable entry {\n  schemafull;\n  ...stamp;\n  field created_at: int;\n};\n"
        );
    }

    /// Issue #35: the merge carries the SDL side's comments onto a
    /// MANAGED statement (defined both sides) — leading `//`, the
    /// `///` doc block, and clause anchors — while the read state
    /// stays the definition.
    #[test]
    fn managed_statements_keep_the_file_comments() {
        let (sdl, diags) = crate::parser::parse_file(
            "// the user side\n/// Users.\ntable user {\n  // above schemaless\n  schemaless;\n};",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let (read, read_diags) = crate::parser::parse_file("table user { schemaless; };");
        assert!(read_diags.is_empty());
        let entry = ReadEntry {
            name: "user".to_string(),
            stmt: read.items.into_iter().next().expect("one statement"),
            markers: crate::info::read::Markers::default(),
        };
        let mut model = ReadModel::default();
        model.mark_ingested(ScopeKey::Root);
        model.root.push(entry);
        let merged = merge(&sdl, &model);
        let out = crate::info::write::write_canonical(&merged);
        for expected in ["// the user side", "/// Users.", "  // above schemaless"] {
            assert!(out.contains(expected), "missing {expected:?} in:\n{out}");
        }
    }

    /// The pinned relative epsilon: `lm` values within 1e-9 relative of
    /// the derived 1/ln(m) elide; anything further (and the printer's
    /// own `f`-suffixed shape) does not.
    #[test]
    fn lm_elision_compares_at_the_relative_epsilon() {
        let derived = 1.0 / 12f64.ln();
        // The live-printed value (probe: `LM 0.40242960438184466f`).
        assert_eq!(elide_lm(Some(Float(derived)), 12), None);
        // A hand-authored near miss inside the epsilon.
        let inside = derived * (1.0 + 5e-10);
        assert_eq!(elide_lm(Some(Float(inside)), 12), None);
        // Outside the epsilon: kept.
        let outside = derived * (1.0 + 1e-6);
        assert_eq!(elide_lm(Some(Float(outside)), 12), Some(Float(outside)));
        // A different m's derived value does not elide for this m.
        assert!(elide_lm(Some(Float(1.0 / 30f64.ln())), 12).is_some());
        // m = 1: the derivation diverges (ln 1 = 0) — the guard keeps
        // every finite lm (the engine raw-routes the Infinity print
        // before this comparison could run).
        assert!(elide_lm(Some(Float(0.4)), 1).is_some());
    }

    /// The `.*` fold's divergent-kind keep, hand-fed (the engine
    /// rejects a divergent kind at define time, so no convergence leg
    /// can reach the branch): a `matrix.*` whose kind differs from the
    /// parent's inner kind is user-meaningful and stays.
    #[test]
    fn dot_star_divergent_kind_is_kept_hand_fed() {
        let field = |segments: Vec<FieldPathSeg>, kind: &str| {
            Stmt::Field(Field {
                name: FieldPath { segments },
                kind: Some(Kind::new(kind)),
                on: Ident::new("t"),
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
        };
        let mut table = Table {
            name: Ident::new("t"),
            kind: TableKind::Normal,
            schema_full: Some(false),
            drop: false,
            changefeed: None,
            inline_edges: None,
            inline_references: None,
            permissions: None,
            graphql: None,
            nested: vec![
                field(vec![FieldPathSeg::Name(Ident::new("matrix"))], "array<int>"),
                field(
                    vec![
                        FieldPathSeg::Name(Ident::new("matrix")),
                        FieldPathSeg::Wildcard,
                    ],
                    "string",
                ),
            ],
            comment: None,
            comments: Vec::new(),
            trailing_comments: Vec::new(),
            clause_comments: Vec::new(),
        };
        fold_table_synthesized(&mut table);
        assert_eq!(
            table.nested.len(),
            2,
            "a divergent `.*` kind is user-meaningful and kept"
        );
    }

    /// Fulltext elision hits only the materialized defaults.
    #[test]
    fn fulltext_elides_only_at_the_materialized_defaults() {
        let elided = elide_index_kind(IndexKind::FullText {
            analyzer: Some(Ident::new("like")),
            bm25: Some(Bm25(1.2, 0.75)),
            highlights: false,
        });
        assert!(matches!(
            elided,
            IndexKind::FullText {
                analyzer: None,
                bm25: None,
                ..
            }
        ));
        let kept = elide_index_kind(IndexKind::FullText {
            analyzer: Some(Ident::new("english")),
            bm25: Some(Bm25(1.2, 0.75)),
            highlights: false,
        });
        assert!(matches!(
            kept,
            IndexKind::FullText {
                analyzer: Some(_),
                bm25: None,
                ..
            }
        ));
    }

    /// The all-ops group respells to the word form; a multi-group
    /// clause and a partial group stay as-read.
    #[test]
    fn op_permission_respell_only_takes_all_ops_single_groups() {
        let table_ops = [
            PermissionOp::Select,
            PermissionOp::Create,
            PermissionOp::Update,
            PermissionOp::Delete,
        ];
        // The printer's equal-value merge: one group over all four ops.
        let merged = respell_all_ops_group(
            Some(OpPermissions::Groups(vec![PermissionGroup {
                ops: vec![
                    PermissionOp::Select,
                    PermissionOp::Create,
                    PermissionOp::Update,
                    PermissionOp::Delete,
                ],
                value: Permission::None,
            }])),
            &table_ops,
        );
        assert_eq!(merged, Some(OpPermissions::None));
        // Two groups (divergent values) stay as-read.
        let multi = Some(OpPermissions::Groups(vec![
            PermissionGroup {
                ops: vec![PermissionOp::Select],
                value: Permission::None,
            },
            PermissionGroup {
                ops: vec![PermissionOp::Create, PermissionOp::Update],
                value: Permission::Full,
            },
        ]));
        assert_eq!(respell_all_ops_group(multi.clone(), &table_ops), multi);
    }
}
