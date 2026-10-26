//! Mixin expansion — the SDL→SDL lowering pass (spec/mixin.md).
//!
//! The one post-parse stage in the pipeline. Spread resolution is the
//! global pass's ([`crate::parser::resolve_spreads`]) — SDL-E1061/
//! E1062 are its findings, over the whole document's mixin set — so
//! this pass is infallible on any document that pass cleared: it
//! resolves each table's spread entries against the document's mixin
//! set (any position), clones the LANDED members into the table's
//! nested list at the spread's position (explicit table members
//! overshadow position-independently; among spreads the later
//! occurrence wins), injects the host table into each lowered
//! member's `on` (the member's parse-time placeholder), and drops the
//! `mixin` statements and spread entries from the lowered file.
//!
//! [`crate::codegen::emit`] runs this pass itself — emission's
//! property, so no caller can forget it. The canonical SDL writer
//! ([`crate::info::write::write_canonical`]) deliberately does NOT
//! lower: fmt round-trips spreads — and type aliases, whose leg this
//! pass also owns (spec/type-alias.md): the document's alias set is
//! collected, every `Kind` referencing one is rewritten to its
//! expansion's text (references inside kind positions only — a
//! `record<Name>` interior is a table name at every case), and the
//! `type` statements drop from the lowered file. Resolution and
//! cycles are the order pass's ([`crate::order::check_document`] —
//! SDL-E1071/E0015 are its findings), so this leg is infallible on
//! any document that pass cleared; on a dirty file an unresolved (or
//! cycle-tied) reference rides through unresolved — the emission is
//! advisory only, the mixin precedent.
//!
//! Resolution ([`winning_members`]) is one home shared with the
//! parser's table fold: its validations re-run over exactly the
//! members that land, so a loser is invisible to both.

use crate::ast::{Ident, MemberKey, SdlFile, Stmt, Table, TypeAlias};
use crate::kind::{KindAst, write_kind};
use std::collections::HashMap;

/// Which members of each spread entry land on the spreading table —
/// spec/mixin.md's resolution, the one home the parser's table-fold
/// validations and the expansion share (both must agree on what
/// lands):
///
/// - an explicit table member of the same [`MemberKey`] overshadows
///   position-independently (declaration over ordering — the loser is
///   skipped, never emitted-then-overwritten; engine parity: the
///   expanded DDL of a collision was always legal, `DEFINE …
///   OVERWRITE` twice applies the second);
/// - among spreads, the LAST occurrence of a key wholly wins — spread
///   order is semantic (the formatter preserves it, SDL-L0022 advises
///   on the possibly-unintended shadows);
/// - a member without a key (a statement kind the parser never mixes
///   in — hand-built ASTs only) never lands.
///
/// `keys` holds each entry's members' [`Stmt::member_key`] results in
/// member order; the result is the land/skip flag per entry, per
/// member.
pub fn winning_members(keys: &[Vec<Option<MemberKey>>], explicit: &[MemberKey]) -> Vec<Vec<bool>> {
    let mut lands: Vec<Vec<bool>> = keys
        .iter()
        .map(|members| members.iter().map(|key| key.is_some()).collect())
        .collect();
    for (entry, members) in keys.iter().enumerate() {
        for (pos, key) in members.iter().enumerate() {
            let Some(key) = key else {
                continue;
            };
            if explicit.contains(key)
                || keys
                    .iter()
                    .skip(entry + 1)
                    .any(|later| later.iter().flatten().any(|other| other == key))
            {
                lands[entry][pos] = false;
            }
        }
    }
    lands
}

/// Expands every spread, rewrites every type-alias reference, and
/// drops the `mixin` and `type` statements. Total on any
/// parser-produced file: a spread resolving to nothing lands nothing
/// and a reference resolving to nothing rides through unresolved —
/// the resolution pass ([`crate::order::check_document`]) already
/// explained why, and a dirty file's emission is advisory only.
pub fn expand(file: &SdlFile) -> SdlFile {
    // The document's mixin set — GLOBAL, any position (the
    // declarative namespace): a spread resolves against every
    // definition in the document, not the ones above it. A duplicate
    // name cannot occur in a parser-produced file (the parse rejects
    // it within a file, the resolution pass across files), so
    // first-wins is unobservable.
    let mixins: Vec<(&str, &[Stmt])> = file
        .items
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Mixin(mixin) => Some((mixin.name.raw.as_str(), mixin.members.as_slice())),
            _ => None,
        })
        .collect();
    // The document's alias set — the same global shape. First-wins on
    // a duplicate (unobservable on a cleared file, the registry's
    // finding).
    let aliases: Vec<(&str, &TypeAlias)> = file
        .items
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::TypeAlias(alias) => Some((alias.name.raw.as_str(), alias)),
            _ => None,
        })
        .collect();
    let mut items: Vec<Stmt> = file
        .items
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Mixin(_) | Stmt::TypeAlias(_) => None,
            Stmt::Table(table) => Some(Stmt::Table(expand_table(table, &mixins))),
            other => Some(other.clone()),
        })
        .collect();
    // The alias leg runs AFTER the spreads: a spread's landed members
    // are ordinary statements by now, their kinds rewritten like any
    // other field's. Resolutions are built once, iteratively (a chain
    // never recurses per link — [`AliasResolutions::build`]); the
    // faults are the order pass's diagnostics, and on a dirty file an
    // unresolved reference rides through.
    let (resolutions, _) = AliasResolutions::build(&aliases);
    for stmt in &mut items {
        rewrite_stmt_kinds(stmt, &resolutions);
        if let Stmt::Table(table) = stmt {
            for nested in &mut table.nested {
                rewrite_stmt_kinds(nested, &resolutions);
            }
        }
    }
    SdlFile {
        items,
        trailing_comments: file.trailing_comments.clone(),
    }
}

/// Every `Kind` one statement carries, rewritten to its expansion
/// (fields' header kinds, functions' argument and return kinds). The
/// resolutions are shared across the whole document — one alias's
/// expansion is computed once.
pub(crate) fn rewrite_stmt_kinds(stmt: &mut Stmt, resolutions: &AliasResolutions) {
    match stmt {
        Stmt::Field(field) => {
            if let Some(kind) = &mut field.kind {
                kind.text = resolve_kind_text(&kind.text, resolutions);
            }
        }
        Stmt::Function(function) => {
            for (_, kind) in &mut function.args {
                kind.text = resolve_kind_text(&kind.text, resolutions);
            }
            if let Some(returns) = &mut function.returns {
                returns.text = resolve_kind_text(&returns.text, resolutions);
            }
        }
        _ => {}
    }
}

/// Resolves one kind text against the document's alias resolutions:
/// parsed permissively, every [`KindAst::Alias`] node substituted
/// with its alias's completed expansion, the result re-printed. A
/// reference with no resolution (unknown, cycle-tied, over-ceiling,
/// shape-invalid) rides through unresolved — the order pass owns the
/// findings (SDL-E1071/E0015/E1073/E1074), so this is infallible on
/// any document that pass cleared and advisory on a dirty one.
pub(crate) fn resolve_kind_text(text: &str, resolutions: &AliasResolutions) -> String {
    let Some(ast) = crate::kind::parse_kind_ast(text) else {
        return text.to_string();
    };
    write_kind(&resolve_ast(&ast, resolutions))
}

pub(crate) fn resolve_ast(ast: &KindAst, resolutions: &AliasResolutions) -> KindAst {
    match ast {
        KindAst::Alias(name) => resolutions
            .get(name)
            .cloned()
            .unwrap_or_else(|| ast.clone()),
        KindAst::Sized { array, inner, size } => KindAst::Sized {
            array: *array,
            inner: inner
                .as_ref()
                .map(|inner| Box::new(resolve_ast(inner, resolutions))),
            size: size.clone(),
        },
        KindAst::Option(inner) => KindAst::Option(Box::new(resolve_ast(inner, resolutions))),
        KindAst::Union(arms) => KindAst::Union(
            arms.iter()
                .map(|arm| resolve_ast(arm, resolutions))
                .collect(),
        ),
        KindAst::Object(pairs) => KindAst::Object(
            pairs
                .iter()
                .map(|(key, value)| (key.clone(), resolve_ast(value, resolutions)))
                .collect(),
        ),
        KindAst::ArrayLit(items) => KindAst::ArrayLit(
            items
                .iter()
                .map(|item| resolve_ast(item, resolutions))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The kind nesting ceiling: how many levels a kind's EXPANSION may
/// nest — an alias definition's or a use site's composition alike —
/// before the document is refused (SDL-E1073). A chain composes
/// nesting one link at a time, and every walk the expanded kind feeds
/// — the writer's, the algebra's, the engine's own parser — recurses
/// over it; the ceiling keeps that recursion bounded at a height no
/// real schema approaches and the deep-chain stack overflow (the
/// review's P1) cannot reach.
pub(crate) const ALIAS_NESTING_CEILING: usize = 256;

/// Why one alias's expansion stays unresolved. The order pass maps
/// each fault to its diagnostic at the FAULTING statement (the pair
/// carries the entry's index in the aliases slice — a cross-file
/// duplicate name anchors where the bad composition is, never at a
/// namesake); references RIDING an unresolved name are never faults
/// themselves (the root cause is reported once).
pub(crate) enum AliasFault {
    /// The expansion nests beyond [`ALIAS_NESTING_CEILING`].
    Overceiling { depth: usize },
    /// The expansion's shape is not a kind the strict grammar accepts
    /// (a chain of option wrappers composes `option<option<…>>` —
    /// legal per alias, unexpressible whole).
    InvalidShape { text: String },
}

/// A kind's expansion depth — every wrapper and container walk one
/// level, leaves (alias references included; the substitution
/// flattened them away) one. The expanded AST holds no alias nodes,
/// so the walk's height is the expansion's own nesting: bounded by
/// the authored depth plus [`ALIAS_NESTING_CEILING`].
pub(crate) fn expanded_depth(ast: &KindAst) -> usize {
    let children = |kids: &[&KindAst]| -> usize {
        kids.iter()
            .map(|kid| expanded_depth(kid))
            .max()
            .unwrap_or(0)
    };
    match ast {
        KindAst::Sized {
            inner: Some(inner), ..
        } => 1 + expanded_depth(inner),
        KindAst::Option(inner) => 1 + expanded_depth(inner),
        KindAst::Union(arms) => 1 + children(&arms.iter().collect::<Vec<_>>()),
        KindAst::Object(pairs) => 1 + children(&pairs.iter().map(|(_, v)| v).collect::<Vec<_>>()),
        KindAst::ArrayLit(items) => 1 + children(&items.iter().collect::<Vec<_>>()),
        _ => 1,
    }
}

/// The document's alias resolutions: each resolvable alias's
/// fully-expanded kind.
pub(crate) struct AliasResolutions {
    expansions: HashMap<String, KindAst>,
}

impl AliasResolutions {
    pub(crate) fn get(&self, name: &str) -> Option<&KindAst> {
        self.expansions.get(name)
    }

    /// Builds every alias's expansion ITERATIVELY — the review's P1:
    /// a chain must never recurse one frame per link. Dependencies
    /// resolve first (iterative DFS post-order), each expansion is
    /// one bounded walk over its own right-hand side substituting
    /// completed expansions by value, and the faults (unparseable
    /// RHS, cycles, over-ceiling depth, shape-invalid compositions)
    /// come back for the order pass's diagnostics. Every walk's
    /// recursion height is the right-hand side's OWN nesting — the
    /// depth the strict grammar already accepted — never a chain's.
    pub(crate) fn build<'a>(
        aliases: &[(&'a str, &'a TypeAlias)],
    ) -> (Self, Vec<(usize, AliasFault)>) {
        let mut faults: Vec<(usize, AliasFault)> = Vec::new();
        // Index-based graph: names in first-definition order, refs as
        // index vectors — the DFS borrows nothing but the arrays.
        let names: Vec<&'a str> = aliases.iter().map(|(name, _)| *name).collect();
        let mut parsed: Vec<Option<KindAst>> = Vec::with_capacity(aliases.len());
        let mut refs: Vec<Vec<usize>> = Vec::with_capacity(aliases.len());
        for (at, (_, alias)) in aliases.iter().enumerate() {
            parsed.push(crate::kind::parse_kind_ast(&alias.kind.text));
            refs.push(Vec::new());
            if parsed.last().unwrap().is_none() {
                // Never a parser-produced shape; refuse to touch it.
                faults.push((
                    at,
                    AliasFault::InvalidShape {
                        text: alias.kind.text.clone(),
                    },
                ));
            }
        }
        let index_of: HashMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(at, name)| (*name, at))
            .collect();
        for (at, ast) in parsed.iter().enumerate() {
            if let Some(ast) = ast {
                let mut out = Vec::new();
                collect_refs(ast, &mut out);
                refs[at] = out
                    .into_iter()
                    .filter_map(|name| index_of.get(name.as_str()).copied())
                    .collect();
            }
        }
        // Iterative DFS post-order — dependencies before dependents;
        // a cycle's members never reach post-order (they stay
        // unresolved, SDL-E0015's finding, reported once by the order
        // pass — not a fault here).
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Open,
            Done,
        }
        let mut marks = vec![None; aliases.len()];
        let mut order: Vec<usize> = Vec::new();
        let mut unresolved = vec![false; aliases.len()];
        for root in 0..aliases.len() {
            if marks[root].is_some() {
                continue;
            }
            let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
            marks[root] = Some(Mark::Open);
            while let Some((at, visited)) = stack.pop() {
                let edges = refs[at].as_slice();
                if visited < edges.len() {
                    stack.push((at, visited + 1));
                    let next = edges[visited];
                    match marks[next] {
                        None => {
                            marks[next] = Some(Mark::Open);
                            stack.push((next, 0));
                        }
                        Some(Mark::Open) => {
                            // A back-edge: the whole cycle stays
                            // unresolved.
                            unresolved[next] = true;
                        }
                        Some(Mark::Done) => {}
                    }
                } else {
                    marks[at] = Some(Mark::Done);
                    order.push(at);
                }
            }
        }
        // Dependencies-first resolution. `depths` rides beside the
        // expansions so an alias's nesting is measured NUMERICALLY —
        // never by walking a deep expanded tree.
        let mut depths: HashMap<String, usize> = HashMap::new();
        let mut resolutions = AliasResolutions {
            expansions: HashMap::new(),
        };
        for at in order {
            let name = names[at];
            if unresolved[at] {
                continue;
            }
            let Some(ast) = &parsed[at] else {
                continue;
            };
            if refs[at].iter().any(|dep| unresolved[*dep]) {
                // Riding an unresolved name: no expansion of our own,
                // no fault either (the root cause is reported once).
                unresolved[at] = true;
                continue;
            }
            let depth = ast_depth_against(ast, &depths);
            if depth > ALIAS_NESTING_CEILING {
                faults.push((at, AliasFault::Overceiling { depth }));
                unresolved[at] = true;
                continue;
            }
            let expanded = resolve_ast(ast, &resolutions);
            let text = write_kind(&expanded);
            if crate::kind::parse_kind_text(&text).is_none() {
                faults.push((at, AliasFault::InvalidShape { text }));
                unresolved[at] = true;
                continue;
            }
            depths.insert(name.to_string(), depth);
            resolutions.expansions.insert(name.to_string(), expanded);
        }
        (resolutions, faults)
    }
}

/// The alias references one kind AST carries, in appearance order,
/// deduplicated.
pub(crate) fn collect_refs(ast: &KindAst, out: &mut Vec<String>) {
    match ast {
        KindAst::Alias(name) => {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        KindAst::Sized {
            inner: Some(inner), ..
        } => collect_refs(inner, out),
        KindAst::Option(inner) => collect_refs(inner, out),
        KindAst::Union(arms) | KindAst::ArrayLit(arms) => {
            for arm in arms {
                collect_refs(arm, out);
            }
        }
        KindAst::Object(pairs) => {
            for (_, value) in pairs {
                collect_refs(value, out);
            }
        }
        _ => {}
    }
}

/// A kind AST's alias-aware nesting depth: one level per wrapper and
/// container walk, an unresolved reference counts as a leaf. The
/// recursion height is the AST's OWN nesting (the right-hand side
/// the strict grammar accepted), never a chain's — referenced
/// aliases contribute their already-computed numbers.
fn ast_depth_against(ast: &KindAst, depths: &HashMap<String, usize>) -> usize {
    let children = |kids: &[&KindAst]| -> usize {
        kids.iter()
            .map(|kid| ast_depth_against(kid, depths))
            .max()
            .unwrap_or(0)
    };
    match ast {
        KindAst::Alias(name) => depths.get(name).copied().unwrap_or(1),
        KindAst::Sized {
            inner: Some(inner), ..
        } => 1 + ast_depth_against(inner, depths),
        KindAst::Option(inner) => 1 + ast_depth_against(inner, depths),
        KindAst::Union(arms) => 1 + children(&arms.iter().collect::<Vec<_>>()),
        KindAst::Object(pairs) => 1 + children(&pairs.iter().map(|(_, v)| v).collect::<Vec<_>>()),
        KindAst::ArrayLit(items) => 1 + children(&items.iter().collect::<Vec<_>>()),
        _ => 1,
    }
}

/// One table's nested list with its spreads resolved in place. When
/// the table carries no spread the statement clones unchanged.
fn expand_table(table: &Table, mixins: &[(&str, &[Stmt])]) -> Table {
    if !table
        .nested
        .iter()
        .any(|stmt| matches!(stmt, Stmt::Spread(_)))
    {
        return table.clone();
    }
    let landed = landing_members(table, mixins);
    let mut spread_index = 0usize;
    let mut nested: Vec<Stmt> = Vec::with_capacity(table.nested.len());
    for entry in &table.nested {
        match entry {
            Stmt::Spread(_) => {
                nested.extend(landed[spread_index].iter().cloned());
                spread_index += 1;
            }
            other => nested.push(other.clone()),
        }
    }
    let mut table = table.clone();
    table.nested = nested;
    table
}

/// The members that LAND on this table, per spread entry in body
/// order: resolved against `mixins`, winner-filtered
/// ([`winning_members`]), and HOST-INJECTED — exactly the statements
/// expansion emits at each entry's position. One resolution home:
/// [`expand_table`] and the spread-preserving merge
/// ([`crate::info::normalize`]) consume the same result, so the
/// merge's coverage pairing can never disagree with emission. An
/// unresolved entry (a dirty file's SDL-E1061 spread) lands nothing.
pub fn landing_members(table: &Table, mixins: &[(&str, &[Stmt])]) -> Vec<Vec<Stmt>> {
    // The explicit member keys — position-independent overshadowing:
    // an explicit field, index, or event anywhere in the table wholly
    // replaces the same-keyed member (per family — a field `x` and an
    // index `x` never collide).
    let explicit: Vec<MemberKey> = table
        .nested
        .iter()
        .filter_map(|stmt| stmt.member_key())
        .collect();
    // Each spread entry's resolved members, in body order (an
    // unresolved entry holds its place as empty so the walk below and
    // the resolution stay index-aligned).
    let spreads: Vec<Option<&[Stmt]>> = table
        .nested
        .iter()
        .filter_map(|entry| match entry {
            Stmt::Spread(spread) => Some(
                mixins
                    .iter()
                    .find(|(name, _)| *name == spread.name.raw.as_str())
                    .map(|(_, members)| *members),
            ),
            _ => None,
        })
        .collect();
    let member_keys: Vec<Vec<Option<MemberKey>>> = spreads
        .iter()
        .map(|members| {
            members
                .map(|members| members.iter().map(|stmt| stmt.member_key()).collect())
                .unwrap_or_default()
        })
        .collect();
    let lands = winning_members(&member_keys, &explicit);
    spreads
        .iter()
        .enumerate()
        .map(|(entry, members)| {
            members
                .map(|members| {
                    members
                        .iter()
                        .enumerate()
                        .filter(|(pos, _)| lands[entry][*pos])
                        .map(|(_, member)| {
                            let mut lowered = member.clone();
                            inject_host(&mut lowered, &table.name);
                            lowered
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

/// The spreading table rides each lowered member's `on` — the
/// member's parse-time placeholder gives way to the host. Every
/// member family carries an `on`; other statement kinds are the
/// parser's never-mixed class (defensive no-op for hand-built ASTs).
fn inject_host(member: &mut Stmt, host: &Ident) {
    match member {
        Stmt::Field(field) => field.on = host.clone(),
        Stmt::Index(index) => index.on = host.clone(),
        Stmt::Event(event) => event.on = host.clone(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_file;

    fn expanded(src: &str) -> String {
        let (file, diags) = parse_file(src);
        assert!(diags.is_empty(), "unexpected diags: {diags:?}");
        let lowered = expand(&file);
        crate::info::write::write_canonical(&lowered)
    }

    /// In-place expansion with the override skipped: the spread's
    /// members land at its position, the explicit field of the same
    /// name wholly replaces its member wherever it sits.
    #[test]
    fn spreads_expand_in_place_with_overshadow() {
        assert_eq!(
            expanded(
                "mixin stamp { a: int; b: int; };\n\
                 table t schemafull { ...stamp; x: string; b: string; };"
            ),
            "table t {\n  schemafull;\n  field a: int;\n  field x: string;\n  field b: string;\n};\n"
        );
    }

    /// The lowered member carries the spreading table (its `on`),
    /// so emission prints `ON` the host — and two tables spreading
    /// one mixin each get their own host.
    #[test]
    fn lowered_members_carry_their_host() {
        assert_eq!(
            expanded(
                "mixin stamp { a: int; };\ntable t schemafull { ...stamp; };\ntable u schemafull { ...stamp; };"
            ),
            "table t {\n  schemafull;\n  field a: int;\n};\n\ntable u {\n  schemafull;\n  field a: int;\n};\n"
        );
        let (file, _) = parse_file("mixin m { a: int; };\ntable t { ...m; };");
        let lowered = expand(&file);
        let Stmt::Table(table) = &lowered.items[0] else {
            panic!()
        };
        let Stmt::Field(field) = &table.nested[0] else {
            panic!()
        };
        assert_eq!(field.on.raw, "t");
    }

    /// Index and event members lower like fields: the host injects
    /// into their `on`, the nested DDL emission prints them, and the
    /// explicit same-named index/event on the table overshadows
    /// position-independently.
    #[test]
    fn index_and_event_members_lower_with_their_host() {
        let (file, _) = parse_file(
            "mixin stamp {\n  slug: string;\n  index slug_u { fields: slug; };\n  \
             event touched { when: slug != ''; then: 1; };\n};\n\
             table t schemafull { ...stamp; };",
        );
        let lowered = expand(&file);
        let Stmt::Table(table) = &lowered.items[0] else {
            panic!()
        };
        let [Stmt::Field(field), Stmt::Index(index), Stmt::Event(event)] = table.nested.as_slice()
        else {
            panic!("three members lower: {:?}", table.nested)
        };
        assert_eq!(field.on.raw, "t");
        assert_eq!(index.on.raw, "t");
        assert_eq!(index.name.raw, "slug_u");
        assert_eq!(event.on.raw, "t");
        assert_eq!(event.name.raw, "touched");
        // The explicit twins decide, wherever they sit.
        let (file, _) = parse_file(
            "mixin stamp {\n  slug: string;\n  index slug_u { fields: slug; };\n  \
             event touched { then: 1; };\n};\n\
             table t schemafull {\n  ...stamp;\n  index slug_u { fields: slug; \
             concurrently; };\n  event touched { then: 2; };\n};",
        );
        let lowered = expand(&file);
        let Stmt::Table(table) = &lowered.items[0] else {
            panic!()
        };
        let [Stmt::Field(_), Stmt::Index(index), Stmt::Event(event)] = table.nested.as_slice()
        else {
            panic!()
        };
        assert!(index.concurrently, "the explicit index won");
        assert_eq!(
            event.then[0].canon, "2",
            "the explicit event won: {:?}",
            event.then
        );
    }

    /// Collisions resolve last-occurrence-wins among spreads
    /// (spec/mixin.md's amendment): the later spread's member wholly
    /// wins, the loser is skipped — never emitted-then-overwritten —
    /// and an explicit member beats every spread wherever it sits.
    /// Per family: a field `x`, an index `x`, and an event `x` never
    /// collide with each other.
    #[test]
    fn spread_collisions_resolve_last_occurrence_wins() {
        assert_eq!(
            expanded(
                "mixin a { x: int; };\nmixin b { x: string; };\n\
                 table t schemafull { ...a; ...b; };"
            ),
            "table t {\n  schemafull;\n  field x: string;\n};\n"
        );
        // Three spreads: the last one's member wins over both earlier.
        assert_eq!(
            expanded(
                "mixin a { x: int; };\nmixin b { x: string; };\nmixin c { x: bool; };\n\
                 table t schemafull { ...a; ...b; ...c; };"
            ),
            "table t {\n  schemafull;\n  field x: bool;\n};\n"
        );
        // The explicit member decides position-independently.
        assert_eq!(
            expanded(
                "mixin a { x: int; };\nmixin b { x: string; };\n\
                 table t schemafull { x: bool; ...a; ...b; };"
            ),
            "table t {\n  schemafull;\n  field x: bool;\n};\n"
        );
        // Per-family keys: same name, different kinds, all land.
        assert_eq!(
            expanded(
                "mixin a { x: int; index x { fields: x; }; event x { then: 1; }; };\n\
                 table t schemafull { ...a; };"
            ),
            "table t {\n  schemafull;\n  field x: int;\n  index x {\n    fields: x;\n  };\n  \
               event x {\n    then: 1;\n  };\n};\n"
        );
    }

    /// A mixin statement is dropped from the lowered file; an unused
    /// mixin drops with it (the lint's subject, not emission's).
    #[test]
    fn mixin_statements_drop_from_the_lowered_file() {
        assert_eq!(
            expanded("mixin unused { a: int; };\ntable t schemafull { x: string; };"),
            "table t {\n  schemafull;\n  field x: string;\n};\n"
        );
    }

    /// The declarative namespace: a spread resolves against the
    /// document's whole mixin set, so a mixin defined BELOW the table
    /// resolves like any other (the resolution pass owns existence —
    /// [`crate::parser::resolve_spreads`] reports the typo class).
    #[test]
    fn a_forward_reference_resolves() {
        let (file, diags) = parse_file("table t { ...late; };\nmixin late { a: int; };");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            crate::parser::resolve_spreads(&file).is_empty(),
            "position is not resolution's business"
        );
        let lowered = expand(&file);
        assert_eq!(
            crate::info::write::write_canonical(&lowered),
            "table t {\n  field a: int;\n};\n"
        );
    }
}
