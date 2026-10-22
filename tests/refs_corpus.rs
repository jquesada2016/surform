//! The corpus spike: the reference
//! extractor's fingerprint over the FULL menus corpus, pinned. Every
//! doc rides the exec corpus's file shape — the family setup
//! prepended to the generated doc, the same composite the ignored
//! `oracle_full` suite executes — so each doc's references are read
//! in the resolution context its targets define (setup-bound tables,
//! params, analyzers). The composite shape re-extracts the setup's
//! own statements once per doc — 8,630 of the pinned 12,452 Table
//! refs are that echo; the setups contribute none of the 1,668
//! param reads, so the param split's ratios are doc-derived.
//!
//! The three fork-decision tallies (these pins keep them from
//! drifting silently):
//!
//! - **clean** — extraction is fully static: zero dynamic mutes.
//! - **environment-resolvable** — every mute is an expression-led
//!   target ([`DynamicWhy::ExpressionLedTarget`]): a position the
//!   ambient environment model can type (`FROM $this.a`) once it
//!   lands.
//! - **dynamic** — at least one mute a checker can never get past:
//!   a [`DynamicWhy::DynamicCall`] (the table exists only at
//!   runtime) or an [`DynamicWhy::Opaque`] body; a muted doc lands
//!   in the worst bucket it carries.
//!
//! Under them, the enforcement-workload counts: mutes by why,
//! references by kind, and the PARAM split — how many param reads
//! the ambient table already explains (session, site extras) versus
//! a [`Stmt::Param`](surform::ast::Stmt::Param) definition
//! versus nothing (unknown — the linter's mute class).
//!
//! This leg runs per-commit instead of `#[ignore]`d: there is no CI
//! and no nightly — the local per-commit gate is the only gate this
//! repo has — and the totality leg already walks the full corpus
//! per-commit, so the marginal cost is one cheap pass. An ignored
//! pin is a pin nobody runs. A corpus or classifier edit that moves
//! a tally here must update the pins WITH the edit (the audit's own
//! re-pin protocol).

#[path = "common/mod.rs"]
mod common;

use std::collections::BTreeSet;

use surform::ast::Stmt;
use surform::parser;
use surform::refs::{self, DynamicWhy, RefKind, SESSION_PARAMS, Site, ambient_params};

/// Per family: `[docs, clean, env-resolvable-only, worst =
/// dynamic-call, worst = opaque]` — the doc-level fingerprint, in
/// [`common::menus::families`]' order. The four classes partition
/// `docs` (asserted below). Measured 2026-10-05: the whole
/// corpus extracts clean — zero dynamic mutes anywhere.
const PINNED_FAMILIES: &[(&str, [usize; 5])] = &[
    ("sequence", [48, 48, 0, 0, 0]),
    ("param", [20, 20, 0, 0, 0]),
    ("database", [12, 12, 0, 0, 0]),
    ("user", [432, 432, 0, 0, 0]),
    ("analyzer", [64, 64, 0, 0, 0]),
    ("bucket", [32, 32, 0, 0, 0]),
    ("event", [90, 90, 0, 0, 0]),
    ("function", [64, 64, 0, 0, 0]),
    ("config api", [16, 16, 0, 0, 0]),
    ("config graphql", [41, 41, 0, 0, 0]),
    ("config default", [15, 15, 0, 0, 0]),
    ("index", [526, 526, 0, 0, 0]),
    ("api", [80, 80, 0, 0, 0]),
    ("access", [405, 405, 0, 0, 0]),
    ("table", [688, 688, 0, 0, 0]),
    ("field", [2026, 2026, 0, 0, 0]),
    ("mixin", [36, 36, 0, 0, 0]),
];

/// Corpus-wide mutes by why: `[ExpressionLedTarget, DynamicCall,
/// Opaque]`.
const PINNED_MUTES: [usize; 3] = [0; 3];

/// Corpus-wide references by kind: `[Table, Fn, Param, Analyzer,
/// Mixin]` — what a checker would get to check.
const PINNED_REFS: [usize; 5] = [12_452, 174, 1_668, 13, 72];

/// Corpus-wide PARAM reads by how the environment explains them:
/// `[session ambient, site ambient, defined param, unknown]` —
/// ambient wins over defined when a name is both. The unknowns are
/// exactly the reads the ambient table leaves unbound on purpose:
/// access signup/signin's caller-supplied vars plus one
/// access-context subquery read of `$email` (163 together), a
/// param-permission `$user`, api middleware `$x`, and `$after` at a
/// COMPUTED field (the write-frame four never reach it).
const PINNED_PARAMS: [usize; 4] = [739, 751, 4, 174];

fn ref_index(kind: RefKind) -> usize {
    match kind {
        RefKind::Table => 0,
        RefKind::Fn => 1,
        RefKind::Param => 2,
        RefKind::Analyzer => 3,
        RefKind::Mixin => 4,
    }
}

/// A mute's bucket AND its severity — 0 is the class a typed
/// environment absorbs ([`DynamicWhy::ExpressionLedTarget`]), 2 the
/// class nothing static ever reads ([`DynamicWhy::Opaque`]); the
/// index order IS the severity order.
fn mute_index(why: DynamicWhy) -> usize {
    match why {
        DynamicWhy::ExpressionLedTarget => 0,
        DynamicWhy::DynamicCall => 1,
        DynamicWhy::Opaque => 2,
    }
}

fn param_bucket(name: &str, site: Site, defined: &BTreeSet<&str>) -> usize {
    if SESSION_PARAMS.contains(&name) {
        0
    } else if ambient_params(site).contains(&name) {
        1
    } else if defined.contains(name) {
        2
    } else {
        3
    }
}

#[test]
fn corpus_tallies_hold_the_recorded_numbers() {
    let mut mutes = [0usize; 3];
    let mut kind_refs = [0usize; 5];
    let mut params = [0usize; 4];
    let mut corpus_docs = 0;
    let mut measured = Vec::new();
    let mut failures = Vec::new();

    let families = common::menus::families();
    assert_eq!(
        PINNED_FAMILIES.len(),
        families.len(),
        "the pin table and the menus disagree on the family count — a family would \
         escape the spike through the zip below; add or retire its pin row WITH the \
         menus edit"
    );
    for (pinned, family) in PINNED_FAMILIES.iter().zip(&families) {
        assert_eq!(
            pinned.0, family.name,
            "the pin table drifted from the menus' family order"
        );
        let mut docs = 0;
        let mut clean = 0;
        let mut worst = [0usize; 3];
        for sdl in (family.docs)() {
            docs += 1;
            let text = format!("{}{sdl}", family.setup);
            let (file, diags) = parser::parse_file(&text);
            assert!(
                diags.is_empty(),
                "{}: the composite file stopped parsing:\n{text}\n{diags:?}",
                family.name
            );
            let defined: BTreeSet<&str> = file
                .items
                .iter()
                .filter_map(|stmt| match stmt {
                    Stmt::Param(param) => Some(param.name.raw.as_str()),
                    _ => None,
                })
                .collect();
            let report = refs::extract_file(&file);
            for dynamic in &report.dynamics {
                mutes[mute_index(dynamic.why)] += 1;
            }
            for reference in &report.refs {
                kind_refs[ref_index(reference.kind)] += 1;
                if let RefKind::Param = reference.kind {
                    params[param_bucket(&reference.name, reference.site, &defined)] += 1;
                }
            }
            let class = match report
                .dynamics
                .iter()
                .map(|d| d.why)
                .max_by_key(|why| mute_index(*why))
            {
                None => {
                    clean += 1;
                    continue;
                }
                Some(why) => mute_index(why),
            };
            worst[class] += 1;
        }
        // The partition is a real check, not a construction: `docs`
        // counted every doc, the classes classify each one once — a
        // future skip-path trips this before it can shrink the pins.
        assert_eq!(
            clean + worst.iter().sum::<usize>(),
            docs,
            "{}: the doc classes stopped partitioning the docs",
            family.name
        );
        let row = [docs, clean, worst[0], worst[1], worst[2]];
        corpus_docs += row[0];
        measured.push((family.name, row));
        if row != pinned.1 {
            failures.push(format!(
                "{}: pinned {:?}, measured {:?}",
                family.name, pinned.1, row
            ));
        }
    }

    for (label, pinned, measured) in [
        ("mutes by why", &PINNED_MUTES[..], &mutes[..]),
        ("refs by kind", &PINNED_REFS[..], &kind_refs[..]),
        ("param split", &PINNED_PARAMS[..], &params[..]),
    ] {
        if pinned != measured {
            failures.push(format!("{label}: pinned {pinned:?}, measured {measured:?}"));
        }
    }
    assert_eq!(
        params.iter().sum::<usize>(),
        kind_refs[ref_index(RefKind::Param)],
        "the param split must partition the Param references"
    );

    // The paste-ready re-pin: the fresh table, in the pins' own
    // syntax, so a moved tally re-records mechanically.
    let table = format!(
        "docs: {corpus_docs}\n{}",
        measured
            .iter()
            .map(|(name, row)| format!("    ({name:?}, {row:?}),"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    println!("corpus tallies ({corpus_docs} docs):\n{table}");
    println!("mutes by why: {mutes:?}");
    println!("refs by kind: {kind_refs:?}");
    println!("param split:  {params:?}");
    assert!(
        failures.is_empty(),
        "the corpus tallies moved — update the pins WITH the edit that moved them:\n{}\n{table}",
        failures.join("\n")
    );
}
