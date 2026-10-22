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
//! lower: fmt round-trips spreads.
//!
//! Resolution ([`winning_members`]) is one home shared with the
//! parser's table fold: its validations re-run over exactly the
//! members that land, so a loser is invisible to both.

use crate::ast::{Ident, MemberKey, SdlFile, Stmt, Table};

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

/// Expands every spread and drops the mixin statements. Total on any
/// parser-produced file: a spread resolving to nothing lands nothing —
/// the resolution pass ([`crate::parser::resolve_spreads`]) already
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
    let items = file
        .items
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Mixin(_) => None,
            Stmt::Table(table) => Some(Stmt::Table(expand_table(table, &mixins))),
            other => Some(other.clone()),
        })
        .collect();
    SdlFile {
        items,
        trailing_comments: file.trailing_comments.clone(),
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
