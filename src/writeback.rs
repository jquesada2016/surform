//! The write-back distribution (spec/include.md) — the write half of
//! `include`: `normalize` on a composition root merges the FLATTENED
//! document, then returns every merged statement to the file that
//! authored it, rewriting the whole tree instead of one file.
//!
//! The invariant is **order fidelity**: the rewritten tree's
//! flattening reproduces the merged document's statement order
//! exactly, so define-order validity (an analyzer before the index
//! that names it, a field before its index) survives the round trip.
//! Provenance drives it — the merge reports, per output statement,
//! the authored input it came from:
//!
//! - A skeleton statement returns to its authored position in its
//!   home file (the loader's `item_positions`), even when siblings
//!   were folded away — positions are exact, never shifted.
//! - An adopted statement slots into the tree where its stream
//!   neighbors put it: immediately after the tree node that produced
//!   the last stream item before it (or before the first one after,
//!   at the document head). A splice re-emits a subtree contiguously,
//!   so re-flattening the rewritten files lands every adoptee exactly
//!   where the merge placed it.
//!
//! Two shapes have no literal position, because every file's layout
//! is `[ns/db preamble][includes][body]` (SDL-E1069): a scope
//! statement (legal in the preamble alone, where inserting one would
//! re-bind every scope below it), and any adoptee whose placement slot
//! fell inside a preamble. A preamble-bound adoptee relocates to the
//! home's first body position when no later dependent would land
//! outside its home — a same-home dependent always sits at or after
//! the relocation target, so the move keeps it ahead — and refuses
//! otherwise: a dependent keyed into another home sits in the include
//! subtree the adoptee would jump past (SDL-E0013).

use std::collections::BTreeMap;

use crate::ast::{Include, SdlFile, Stmt};
use crate::diag::Diag;
use crate::info::normalize::{Family, classify, placement_deps, placement_key};
use crate::info::write::write_canonical_with;
use crate::load::Loaded;

/// The merge's inputs the distribution reads: the merged document with
/// its provenance, and the loaded file map.
struct Sources<'a> {
    merged: &'a SdlFile,
    provenance: &'a [Option<usize>],
    loaded: &'a Loaded,
}

/// Plans the write-back: the (home index, new text) pairs for every
/// file whose canonical form differs from what was loaded. Pure —
/// nothing touches the disk, so the caller refuses before any file
/// changes when the distribution has no legal shape.
pub fn plan(
    loaded: &Loaded,
    merged: &SdlFile,
    provenance: &[Option<usize>],
    max_width: usize,
) -> Result<Vec<(usize, String)>, Diag> {
    debug_assert_eq!(
        merged.items.len(),
        provenance.len(),
        "provenance runs parallel to the merged items"
    );
    let sources = Sources {
        merged,
        provenance,
        loaded,
    };
    // Per home: the skeleton survivors by authored position, and the
    // adoptee buckets keyed by the authored position to emit BEFORE
    // (a preceded adoptee's key is the preceding item's position + 1;
    // a head adoptee's, the following item's own position). Each
    // home's preamble ends at its last include — keys at or below it
    // are preamble-bound (the relocation rule below).
    let home_count = loaded.homes.len();
    let preamble_ends: Vec<Option<usize>> = loaded
        .includes_by_home
        .iter()
        .map(|includes| includes.last().map(|(position, _)| *position))
        .collect();
    let mut survivors: Vec<BTreeMap<usize, &Stmt>> = vec![BTreeMap::new(); home_count];
    let mut buckets: Vec<BTreeMap<usize, Vec<&Stmt>>> = vec![BTreeMap::new(); home_count];
    // The last skeleton item's (home, authored position) — adoptees
    // trail it; the ones before ANY skeleton wait for the first (or
    // fall to the root when the document has none).
    let mut last: Option<(usize, usize)> = None;
    let mut head: Vec<&Stmt> = Vec::new();
    for (at, (stmt, origin)) in merged.items.iter().zip(provenance).enumerate() {
        match origin {
            Some(input) => {
                let home = loaded.item_homes[*input];
                let position = loaded.item_positions[*input];
                if !head.is_empty() {
                    let head = std::mem::take(&mut head);
                    for stmt in head {
                        place_adoptee(
                            &mut buckets[home],
                            preamble_ends[home],
                            home,
                            position,
                            stmt,
                            at,
                            &sources,
                        )?;
                    }
                }
                survivors[home].insert(position, stmt);
                last = Some((home, position));
            }
            None => {
                if matches!(stmt, Stmt::Namespace(_) | Stmt::Database(_)) {
                    return Err(unplaceable_diag(loaded, stmt));
                }
                match last {
                    Some((home, position)) => {
                        place_adoptee(
                            &mut buckets[home],
                            preamble_ends[home],
                            home,
                            position + 1,
                            stmt,
                            at,
                            &sources,
                        )?;
                    }
                    None => head.push(stmt),
                }
            }
        }
    }
    if !head.is_empty() {
        // No skeleton anywhere: the adoptees are the whole document,
        // and the root is the only honest home.
        let head = std::mem::take(&mut head);
        for stmt in head {
            place_adoptee(
                &mut buckets[0],
                preamble_ends[0],
                0,
                0,
                stmt,
                merged.items.len(),
                &sources,
            )?;
        }
    }

    let mut written = Vec::new();
    for (home, bucket) in buckets.iter().enumerate() {
        let includes: BTreeMap<usize, &Include> = loaded.includes_by_home[home]
            .iter()
            .map(|(position, include)| (*position, include))
            .collect();
        let home_survivors = &survivors[home];
        let total = includes
            .keys()
            .chain(home_survivors.keys())
            .chain(bucket.keys())
            .copied()
            .max()
            .map_or(0, |max| max + 1);
        let mut items = Vec::new();
        for position in 0..total {
            if let Some(adoptees) = bucket.get(&position) {
                items.extend(adoptees.iter().map(|stmt| (*stmt).clone()));
            }
            if let Some(include) = includes.get(&position) {
                // The loader's transfer already relocated the include's
                // comments onto the splice neighbors, so the rebuilt
                // file carries the bare statement — keeping them would
                // re-transfer a second copy on every re-normalize.
                items.push(Stmt::Include(Include {
                    comments: Vec::new(),
                    clause_comments: Vec::new(),
                    ..(*include).clone()
                }));
            } else if let Some(stmt) = home_survivors.get(&position) {
                items.push((*stmt).clone());
            }
            // A position holding neither is a twin-folded authored
            // statement: the gap closes, the order never shifts.
        }
        let file = SdlFile {
            items,
            // The document tail is the root's (an included file's tail
            // follows its last statement — the loader transferred it).
            trailing_comments: if home == 0 {
                merged.trailing_comments.clone()
            } else {
                Vec::new()
            },
        };
        let text = write_canonical_with(&file, max_width);
        if text != loaded.homes[home].text {
            written.push((home, text));
        }
    }
    Ok(written)
}

/// The preamble rule (see the module header): an adoptee keyed at or
/// below the home's last include has no literal position (SDL-E1069's
/// `[preamble][includes][body]` layout) — relocate to the home's
/// first body position when no later dependent would land outside the
/// home, refuse when one would.
fn place_adoptee<'a>(
    buckets: &mut BTreeMap<usize, Vec<&'a Stmt>>,
    preamble_end: Option<usize>,
    home: usize,
    key: usize,
    stmt: &'a Stmt,
    at: usize,
    sources: &Sources<'_>,
) -> Result<(), Diag> {
    let mut key = key;
    if let Some(last_include) = preamble_end
        && key <= last_include
    {
        let guarded =
            placement_key(stmt).is_some_and(|key| sources.relocation_blocked(at, &key, home));
        if guarded {
            return Err(unplaceable_diag(sources.loaded, stmt));
        }
        key = last_include + 1;
    }
    buckets.entry(key).or_default().push(stmt);
    Ok(())
}

impl Sources<'_> {
    /// Whether any later statement that depends on the adoptee's
    /// placement key would land OUTSIDE `home` after the write-back —
    /// the relocation guard's refusal condition. Authored dependents
    /// count by their home; adopted dependents count by the home their
    /// own placement gives them (the nearest preceding skeleton's —
    /// the same rule the distribution walk applies), because a
    /// dependent keyed into a foreign home sits inside the include
    /// subtree the adoptee would jump past. A same-home dependent is
    /// always safe: body positions sit at or after the relocation
    /// target (the home's first body position; the preamble's only
    /// residents, `namespace`/`database`, depend on nothing), and the
    /// bucket emits before the survivor at the same key, so the
    /// relocated adoptee stays ahead of every same-home dependent,
    /// adopted ones included (push order preserves their relative
    /// order).
    fn relocation_blocked(&self, from: usize, key: &(Family, String), home: usize) -> bool {
        let mut last_home = home;
        for (at, (stmt, origin)) in self.merged.items.iter().zip(self.provenance).enumerate() {
            if at <= from {
                continue;
            }
            let depends = placement_deps(stmt).iter().any(|dep| dep == key);
            match origin {
                Some(input) => {
                    let skeleton_home = self.loaded.item_homes[*input];
                    if depends && skeleton_home != home {
                        return true;
                    }
                    last_home = skeleton_home;
                }
                None => {
                    if depends && last_home != home {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// The unplaceable refusal: the span anchors the root's first
/// `include` (the composition that cannot absorb the statement).
fn unplaceable_diag(loaded: &Loaded, stmt: &Stmt) -> Diag {
    let statement = match stmt {
        Stmt::Namespace(ns) => format!("namespace `{}`", ns.name.raw),
        Stmt::Database(db) => format!("database `{}`", db.name.raw),
        other => {
            let (family, name) = classify(other);
            format!("{family:?} `{name}`").to_ascii_lowercase()
        }
    };
    let span = loaded.includes_by_home[0]
        .first()
        .map(|(_, include)| include.span)
        .unwrap_or_else(|| crate::lexer::Span::point(0));
    Diag::WriteBackUnplaceable { span, statement }
}
