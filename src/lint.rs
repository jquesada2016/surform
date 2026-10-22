//! The linter: advisory findings over the resolved
//! state — `SDL-Lxxxx` codes, miette-rendered, never diagnostics
//! (`check`/`build` output is unchanged; `surform lint` exits 1 on any
//! finding, the clippy convention).
//!
//! Two passes over two views of the file. The RESOLVED pass walks the
//! merge's placed output when `--info` was given (elision never
//! demotes lint visibility — findings evaluate what the database
//! would hold, not the source text) and the authored file otherwise.
//! The AUTHORED pass walks the file as written: the findings whose
//! subject is authored intent the engine silently ignores (access
//! duration sides a capability drops, rule 8's raw bodies, sentinel
//! key material) can only ever be seen there — the read side never
//! carries them.
//!
//! Findings anchor on authored spans ONLY through the span map built
//! from the authored file's own text: `merge_stmt` adopts the read
//! statement as base, so merged statements carry spans into DDL
//! fragment text — labeling the user's file with them would point at
//! a nonexistent position. Adoptee-only findings render sourceless
//! (the message names the scope and the statement's key); an
//! ambiguous key (two authored statements claiming one (scope,
//! family, key)) drops to sourceless too — fail-closed, never a
//! wrong span. `raw` statements anchor positionally instead: their
//! match key is the empty string, so key lookups would collide.
//!
//! **Allow comments** (spec/README.md#the-linter): a
//! `// surform-lint-allow: SDL-L0001[, …]` comment excuses the codes
//! it names on ONE statement — the comment(s) leading a top-level
//! statement plus its same-line trailing comment, collected by a
//! lint-time scan of the authored source (comments are not parsed
//! structure; the scan rides the same statement walk that builds the
//! span map). A finding is suppressed when its span falls inside an
//! allowed statement and its code is named; sourceless findings have
//! no statement to excuse them and never suppress. The directive's
//! own problems (unknown codes, malformed markers, nothing to attach
//! to) are SDL-L0023 findings — immune to suppression, so a broken
//! allow cannot silence its own diagnosis.
//!
//! Code registry: `SDL-L0001`–`SDL-L0026`, one code per finding
//! kind (`SDL-L0025` is retired — the compiler orders emission
//! itself, so an apply-order advisory has nothing left to say).
//! Every kind's `url()` deep-links into the spec file that
//! licenses it (repo-relative, `Diag`'s convention).
//!
//! **The reference lint**: `SDL-L0024` consumes
//! [`crate::refs`]' extraction report — the same mute-never-error
//! contract, applied to definitions. It flags a statically-resolved
//! reference whose target no linted file defines (a typo, or a
//! target outside the composition — both legitimate, both
//! excusable); a target that IS defined is fine wherever it sits,
//! the compiler ordering emission itself. The flat catalog makes an
//! undefined target legal SDL, never a diagnostic. Params are sound
//! only inside `fn::` bodies: the engine
//! runs them on `Context::new_isolated` (`ctx/context.rs:271` —
//! empty values), whose value lookup falls through to the parent
//! solely for the protected session four (`PROTECTED_PARAM_NAMES`,
//! exactly [`crate::refs::SESSION_PARAMS`]); the record trio
//! `$this`/`$self`/`$parent` binds through the CursorDoc on a
//! document-bound call (`legacy/expr/param.rs:27`), and a `DEFINE
//! PARAM` resolves through the param read's catalog fall-through
//! (`get_db_param` — bodies cannot see client-bound values, but they
//! CAN see catalog params, so the definition set covers them). Every
//! other site can read client-bound params, so params there stay
//! muted.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::ast::{
    Access, AccessKind, Analyzer, Api, ApiAnyBlock, Field, FieldPathSeg, Filter, JwtSource,
    JwtVerify, MemberKey, OpPermissions, Pass, Permission, PermissionOp, Role, SdlFile, Spread,
    Stmt, Table, TableKind, Tokenizer,
};
use crate::info::normalize::{Family, ScopeKey, ScopeWalk, classify};
use crate::kind::{KindAst, normalize_kind, parse_kind_ast};
use crate::lexer::{Scanner, Span};
use crate::parser::SECRET_SENTINEL_CANON;
use crate::parser::SECRET_SENTINEL_CANON_DECODED as SENTINEL_DECODED;
use crate::refs::{DOC_SCOPE_PARAMS, RefKind, SESSION_PARAMS, Site};

/// One advisory finding: what was found, where (the resolved scope
/// and statement key — the message names them so sourceless findings
/// stay actionable), and the authored span when one is unambiguous.
#[derive(Debug, Clone)]
pub struct Finding {
    pub kind: FindingKind,
    pub scope: ScopeKey,
    pub family: Family,
    pub key: String,
    /// The authored statement's span (top-level statement
    /// granularity); `None` renders the finding sourceless.
    pub span: Option<Span>,
    /// The participating file the finding renders against — an index
    /// into the loader's homes ([`crate::load::Loaded`]), always `0`
    /// for the single-file entry. Spans are per-file coordinates, so
    /// the rendering source MUST be the finding's own home.
    pub home: usize,
}

/// Lints a file. `resolved` is the merge's placed output when
/// interchange was ingested, the file itself otherwise; `source` is
/// the authored file's text (the span map's only input). Findings
/// come back sorted by scope, family, key, then code.
pub fn lint(sdl: &SdlFile, resolved: &SdlFile, source: &str) -> Vec<Finding> {
    let scan = scan_statements(source);
    lint_composed(
        sdl,
        resolved,
        &[ComposePart {
            scan: &scan,
            home: 0,
            statements: 0..scan.statements.len(),
        }],
        &[],
    )
}

/// One diamond include's discovery facts (spec/include.md): a file
/// the discovery DFS had already COMPLETED that another file then
/// declares — the second declaration splices nothing, and the
/// advisory anchors there. `home`/`span` render the no-op declaration
/// against its own file; `path` is the authored literal; `declarer`
/// is the first declarer's display spelling (root-relative, like
/// every home's).
#[derive(Debug)]
pub struct DiamondInclude {
    pub home: usize,
    pub span: Span,
    pub path: String,
    pub declarer: String,
}

/// One participating file's contribution to a composed lint
/// (spec/include.md): the home's whole-source scan, the home index it
/// renders against, and the slice of its statement list this
/// CONTIGUOUS BLOCK of the flattened stream contributes. The loader
/// splices whole statement runs, so a home may contribute several
/// parts (its statements between two includes); the parts appear in
/// flatten order and their statement slices partition the home's
/// spliced statements. The slices feed the span map; suppression
/// reads the whole scan (an include statement's entry — outside every
/// slice, the splice having removed it — is the diamond advisory's
/// allow anchor), and the directive problems render home-scoped:
/// [`lint_composed`] reads each home's whole scan once.
pub struct ComposePart<'a> {
    pub scan: &'a SourceScan,
    pub home: usize,
    pub statements: std::ops::Range<usize>,
}

/// Lints a (possibly composed) file: the single-file [`lint`] with
/// the span map assembled from per-home scans instead of one source.
/// Each part's statements align with a contiguous run of
/// `sdl.items` (the loader's splice order), so the span map zips
/// exactly as the single-file walk's did; suppression and the
/// allow-directive problems stay per-home (spans are per-file
/// coordinates and overlap across homes — a directive can only ever
/// excuse a statement from its OWN file). Findings carry the home to
/// render against. `diamonds` are the loader's diamond discoveries
/// ([`DiamondInclude`]) — the SDL-L0026 advisories, one per no-op
/// include declaration; the single-file entry passes none (no file
/// context, no diamonds).
pub fn lint_composed(
    sdl: &SdlFile,
    resolved: &SdlFile,
    parts: &[ComposePart<'_>],
    diamonds: &[DiamondInclude],
) -> Vec<Finding> {
    // The span map's input: per part, the block's statement spans
    // tagged with the home — flatten order, aligned with `sdl.items`.
    let anchored: Vec<(Span, usize)> = parts
        .iter()
        .flat_map(|part| {
            part.scan.statements[part.statements.clone()]
                .iter()
                .map(move |statement| (statement.span, part.home))
        })
        .collect();
    debug_assert_eq!(
        anchored.len(),
        sdl.items.len(),
        "the loader's splice contract: one scan per flattened statement"
    );
    let anchors = Anchors::new(sdl, &anchored);
    let mut findings = Vec::new();
    resolved_pass(resolved, &anchors, &mut findings);
    authored_pass(sdl, &anchored, &mut findings);
    reference_pass(sdl, resolved, &anchored, &mut findings);
    // The diamond advisories ride discovery, not the walks: each
    // anchors at its no-op declaration's span, in that file.
    for diamond in diamonds {
        findings.push(finding(
            FindingKind::DiamondInclude {
                declarer: diamond.declarer.clone(),
            },
            &ScopeKey::Root,
            Family::Include,
            diamond.path.clone(),
            (Some(diamond.span), diamond.home),
        ));
    }
    // The allow join runs after both passes: a suppression drops the
    // finding wherever it came from, then the directive's own
    // problems join the survivors — unsuppressible by construction
    // (their comment spans sit outside every statement span), with
    // the explicit kind immunity in [`is_suppressed`] as the
    // defense-in-depth that does not depend on that geography.
    findings.retain(|finding| !is_suppressed(finding, parts));
    // The directive problems are home-scoped: each home's whole scan
    // renders once, block slices or not — a malformed or unattached
    // directive above an include statement (whose scan entry no block
    // covers) reports exactly as it does in the single file.
    let mut rendered: BTreeSet<usize> = BTreeSet::new();
    for part in parts {
        if !rendered.insert(part.home) {
            continue;
        }
        let problems = part
            .scan
            .statements
            .iter()
            .flat_map(|statement| &statement.problems)
            .chain(part.scan.tail_problems.iter());
        for (problem, span) in problems {
            findings.push(finding(
                FindingKind::LintAllowProblem {
                    problem: problem.clone(),
                },
                &ScopeKey::Root,
                Family::Comment,
                allow_problem_key(problem),
                (Some(*span), part.home),
            ));
        }
    }
    findings.sort_by(|a, b| {
        (&a.scope, a.family, &a.key, a.kind.code_str()).cmp(&(
            &b.scope,
            b.family,
            &b.key,
            b.kind.code_str(),
        ))
    });
    findings
}

// ---------------------------------------------------------------------
// The statement scan — spans plus allow directives
// ---------------------------------------------------------------------

/// One statement's scan: its span, every lint code its allow comments
/// excuse, and the directive problems that drained into it (a
/// malformed or unattached directive belongs to the statement it sat
/// above — the composed entry attributes it to that statement's
/// home). `allow` holds registry-verified codes (unknown ones become
/// SDL-L0023 findings, never suppressors).
#[derive(Debug)]
pub struct StatementScan {
    pub span: Span,
    pub allow: BTreeSet<&'static str>,
    pub problems: Vec<(LintAllowProblem, Span)>,
}

/// One source text's statement walk: one scan per top-level statement
/// (in order, the span map's input) plus the directive problems that
/// never met a statement (EOF-flushed — the file tail's). This is the
/// old `statement_spans` walk grown a comment ledger — the capture
/// discipline is unchanged, and the walk now carries the legal
/// unterminated TAIL as a statement too (the parser accepts a final
/// statement without its `;`; its span runs to end of source).
///
/// Public as the composed entry's input unit: the loader scans each
/// participating file once and hands [`ComposePart`] slices to
/// [`lint_composed`].
#[derive(Debug)]
pub struct SourceScan {
    pub statements: Vec<StatementScan>,
    pub tail_problems: Vec<(LintAllowProblem, Span)>,
}

/// Scans one file's text for the lint's span map and allow ledger —
/// the composed entry's per-home input ([`lint_composed`]).
pub fn scan_source(text: &str) -> SourceScan {
    scan_statements(text)
}

/// One plain `//` comment's contribution to the allow ledger: the
/// registry codes it names (verified — unknown ones come back as
/// problems), anchored nowhere yet (the caller knows the comment's
/// span and what it attaches to).
struct AllowDirective {
    codes: BTreeSet<&'static str>,
    problems: Vec<LintAllowProblem>,
}

/// The allow-comment prefix, lowercase-exact (`surform-lint-alow:`
/// and friends are malformed directives, not markers — they become
/// SDL-L0023 findings through the `surform-lint` stem check).
const ALLOW_MARKER: &str = "surform-lint-allow:";

/// A comment names the tool but is not a well-formed directive only
/// when it starts with this stem — plain prose mentioning surform
/// mid-sentence never trips the check.
const ALLOW_STEM: &str = "surform-lint";

/// Reads one plain comment's allow directive. `text` is the
/// comment's preserved interior (everything after `//`, the
/// Scanner's rule-5 shape). `None` — an ordinary comment, no
/// directive. A directive with no codes, or a comment starting
/// `surform-lint` that is not the marker, is a problem (SDL-L0023),
/// never a suppressor.
fn parse_allow(text: &str) -> Option<AllowDirective> {
    let trimmed = text.trim_start();
    if let Some(list) = trimmed.strip_prefix(ALLOW_MARKER) {
        let mut codes = BTreeSet::new();
        let mut problems = Vec::new();
        let mut unknown_seen = BTreeSet::new();
        for code in list.split(',').map(str::trim).filter(|c| !c.is_empty()) {
            match canonical_lint_code(code) {
                Some(known) => {
                    codes.insert(known);
                }
                // A repeated unknown code warns once, like a repeated
                // known code suppresses once — the set discipline.
                None if unknown_seen.insert(code.to_string()) => {
                    problems.push(LintAllowProblem::UnknownCode(code.to_string()));
                }
                None => {}
            }
        }
        if codes.is_empty() && problems.is_empty() {
            // `surform-lint-allow:` naming nothing at all.
            problems.push(LintAllowProblem::Malformed {
                text: trimmed.to_string(),
            });
        }
        return Some(AllowDirective { codes, problems });
    }
    // The stem tripwire is case-insensitive (`// Surform-Lint-Alow:`
    // is a typo'd directive, not prose); the boundary guard keeps the
    // slice safe on multi-byte input. The exact-match arm above
    // already returned, so only near-misses reach here.
    if trimmed.is_char_boundary(ALLOW_STEM.len())
        && trimmed[..ALLOW_STEM.len()].eq_ignore_ascii_case(ALLOW_STEM)
    {
        return Some(AllowDirective {
            codes: BTreeSet::new(),
            problems: vec![LintAllowProblem::Malformed {
                text: trimmed.to_string(),
            }],
        });
    }
    None
}

/// The statement walk (the module doc's span-map contract): the
/// statement keyword through the statement's depth-0 `;` under the
/// lexer's capture discipline, now also draining each statement's
/// leading plain comments and same-line trailing comment for allow
/// directives. Leading comments attach GREEDILY across blank lines
/// (fmt collapses a blank between a comment and its statement, so
/// greedy is the fmt-stable rule); a comment with no following
/// statement is a problem, never a suppressor.
fn scan_statements(source: &str) -> SourceScan {
    let mut out = SourceScan {
        statements: Vec::new(),
        tail_problems: Vec::new(),
    };
    // The directives collected since the last statement — they attach
    // to the NEXT statement (leading position).
    let mut pending = Vec::new();
    let mut scan = Scanner::new(source);
    loop {
        // Leading trivia in ANY order: doc blocks and plain `//` lines
        // interleave freely above a statement (a plain comment can sit
        // above a doc block), and each plain line drains for an allow
        // directive. One loop for both classes — the old shape (a doc
        // pass then a plain pass) ended the walk on any doc block that
        // FOLLOWED a plain line, silently losing every statement span
        // after it.
        loop {
            scan.skip_trivia();
            if scan.at_doc_comment() {
                scan.take_doc_block();
                continue;
            }
            if scan.at_plain_comment() {
                let directive = take_comment_directive(&mut scan);
                if let Some(directive) = directive {
                    pending.push(directive);
                }
                continue;
            }
            break;
        }
        if scan.is_at_end() {
            flush_unattached(&mut pending, &mut out.tail_problems);
            break;
        }
        if scan.eat_byte(b';') {
            continue;
        }
        let start = scan.offset();
        if scan.read_word().is_none() {
            flush_unattached(&mut pending, &mut out.tail_problems);
            break;
        }
        match scan.capture_until(b";") {
            // The cursor rests after the `;` — the span is taken HERE,
            // before the trailing-comment probe, so it ends at the `;`
            // exactly as the span map always did (a trailing comment is
            // attachment metadata, never statement text).
            Ok((_, _)) => {
                let span = Span::new(start, scan.offset() - start);
                let mut allow = BTreeSet::new();
                let mut problems = Vec::new();
                drain(pending.drain(..), &mut allow, &mut problems);
                // The same-line trailing comment belongs to the
                // statement just closed (fmt keeps it on the line).
                if let Some((directive, comment_span)) =
                    trailing_comment_directive(&mut scan, source)
                {
                    drain([(directive, comment_span)], &mut allow, &mut problems);
                }
                out.statements.push(StatementScan {
                    span,
                    allow,
                    problems,
                });
            }
            // EOF (the legal final statement without its `;`) or a
            // stray closer (defensive — the parser rejects those, so
            // lint never runs on them). Either way a statement DID
            // start here, so the pending directives ATTACH — an allow
            // above a legal unterminated tail must work, not report
            // itself unattached — and the tail gets its span (to end
            // of source at EOF; to the stray closer on the defensive
            // variant), so its findings anchor instead of rendering
            // sourceless. No trailing probe: without a `;` there is
            // no terminator line to be same-line with (ledgered).
            Err(_) => {
                let mut allow = BTreeSet::new();
                let mut problems = Vec::new();
                drain(pending.drain(..), &mut allow, &mut problems);
                out.statements.push(StatementScan {
                    span: Span::new(start, scan.offset() - start),
                    allow,
                    problems,
                });
                break;
            }
        }
    }
    out
}

/// Consumes one plain comment and reads it for an allow directive,
/// returning the directive with the comment's text-span anchor (the
/// Scanner returns no span, so the caller-side arithmetic rebuilds
/// it: the interior starts two bytes in and runs `text.len()`).
fn take_comment_directive(scan: &mut Scanner<'_>) -> Option<(AllowDirective, Span)> {
    let interior_at = scan.offset() + 2;
    let text = scan.take_comment();
    parse_allow(&text).map(|directive| (directive, Span::new(interior_at, text.len())))
}

/// The same-line trailing comment after a statement's `;`, if the
/// rest of the line is one — the parser's `peek_same_line_note` model
/// exactly (all trivia skipped provisionally, the comment's line
/// compared against the `;`'s, cursor restored unless a plain `//`
/// comment starts on that line): what fmt re-anchors as the
/// statement's close note must attach as trailing HERE too, exotic
/// horizontal spacing included. Consumed here so the main loop never
/// re-reads it as a leading comment.
fn trailing_comment_directive(
    scan: &mut Scanner<'_>,
    source: &str,
) -> Option<(AllowDirective, Span)> {
    let restore = scan.offset();
    let terminator_line = crate::lexer::line_col(source, restore - 1).0;
    scan.skip_trivia();
    let same_line = scan.at_plain_comment()
        && crate::lexer::line_col(source, scan.offset()).0 == terminator_line;
    if !same_line {
        scan.seek(restore);
        return None;
    }
    take_comment_directive(scan)
}

/// Attaches drained directives: named codes join the statement's
/// allow set, problems ride with their comment spans.
fn drain(
    directives: impl IntoIterator<Item = (AllowDirective, Span)>,
    allow: &mut BTreeSet<&'static str>,
    problems: &mut Vec<(LintAllowProblem, Span)>,
) {
    for (directive, span) in directives {
        allow.extend(directive.codes);
        problems.extend(directive.problems.into_iter().map(|p| (p, span)));
    }
}

/// Directives that never met a statement (end of source, or the walk
/// ended defensively) — an allow with nothing to excuse.
fn flush_unattached(
    pending: &mut Vec<(AllowDirective, Span)>,
    tail_problems: &mut Vec<(LintAllowProblem, Span)>,
) {
    for (directive, span) in pending.drain(..) {
        if !directive.codes.is_empty() {
            tail_problems.push((LintAllowProblem::Unattached, span));
        }
        tail_problems.extend(directive.problems.into_iter().map(|p| (p, span)));
    }
}

/// Whether a finding is excused: its span must fall inside a
/// statement OF ITS OWN HOME whose allow set names its code (spans
/// are per-file coordinates and overlap across homes — only the
/// finding's home's ledger can suppress it, which is file-scoped
/// allows falling out of the per-home structure). The walk reads the
/// home's WHOLE scan, not just the block slices: the splice removes
/// every include statement, so an include's scan entry sits outside
/// every slice — the include-anchored advisory (SDL-L0026, the
/// diamond) suppresses through it, and nothing else can anchor there
/// (issue #43's geography closes with it; the slices still partition
/// every non-include statement). Sourceless findings never suppress
/// (no statement to excuse them), and SDL-L0023 — the allow
/// directive's own diagnosis — is immune by construction.
fn is_suppressed(finding: &Finding, parts: &[ComposePart<'_>]) -> bool {
    if matches!(finding.kind, FindingKind::LintAllowProblem { .. }) {
        return false;
    }
    let Some(span) = finding.span else {
        return false;
    };
    let code = finding.kind.code_str();
    parts
        .iter()
        .filter(|part| part.home == finding.home)
        .any(|part| {
            part.scan.statements.iter().any(|statement| {
                span.offset >= statement.span.offset
                    && span.offset + span.len <= statement.span.offset + statement.span.len
                    && statement.allow.contains(code)
            })
        })
}

/// The registry's canonical spelling for a code named in an allow
/// comment — `None` when the code does not exist (the SDL-L0023
/// case). Exact-match: the registry's spellings are the only ones.
fn canonical_lint_code(text: &str) -> Option<&'static str> {
    LINT_CODES.iter().copied().find(|code| *code == text)
}

/// Every registered lint code, ascending — [`FindingKind::code_str`]'s
/// value set, the allow comment's dictionary. The completeness test
/// in this file pins the two together (a new kind must land here).
const LINT_CODES: &[&str] = &[
    "SDL-L0001",
    "SDL-L0002",
    "SDL-L0003",
    "SDL-L0004",
    "SDL-L0005",
    "SDL-L0006",
    "SDL-L0007",
    "SDL-L0008",
    "SDL-L0009",
    "SDL-L0010",
    "SDL-L0011",
    "SDL-L0012",
    "SDL-L0013",
    "SDL-L0014",
    "SDL-L0015",
    "SDL-L0016",
    "SDL-L0017",
    "SDL-L0018",
    "SDL-L0019",
    "SDL-L0020",
    "SDL-L0021",
    "SDL-L0022",
    "SDL-L0023",
    "SDL-L0024",
    "SDL-L0026",
];

/// Whether `code` names a registered lint finding — public for
/// tooling that validates codes the way `code_str` publishes them.
pub fn is_known_code(code: &str) -> bool {
    canonical_lint_code(code).is_some()
}

/// The authored statements' (scope, family, key) → span map. Raw and
/// include statements never enter (their key is the empty string —
/// see the module doc); a key claimed by two statements is dropped
/// rather than guessed.
struct Anchors(BTreeMap<(ScopeKey, Family, String), (Span, usize)>);

impl Anchors {
    fn new(sdl: &SdlFile, anchored: &[(Span, usize)]) -> Self {
        let mut map = BTreeMap::new();
        let mut ambiguous: BTreeSet<(ScopeKey, Family, String)> = BTreeSet::new();
        let mut walk = ScopeWalk::new();
        for (stmt, anchor) in sdl.items.iter().zip(anchored) {
            let scope = walk.lookup_scope(stmt);
            walk.advance(stmt);
            if matches!(stmt, Stmt::Raw(_) | Stmt::Mixin(_) | Stmt::Include(_)) {
                continue;
            }
            let (family, key) = classify(stmt);
            let id = (scope, family, key);
            if map.insert(id.clone(), *anchor).is_some() {
                ambiguous.insert(id);
            }
        }
        for id in ambiguous {
            map.remove(&id);
        }
        Anchors(map)
    }

    fn get(&self, scope: &ScopeKey, family: Family, key: &str) -> Anchor {
        match self
            .0
            .get(&(scope.clone(), family, key.to_string()))
            .copied()
        {
            Some((span, home)) => (Some(span), home),
            // An ambiguous or absent statement renders sourceless,
            // against the root's file.
            None => (None, 0),
        }
    }
}

// ---------------------------------------------------------------------
// The resolved pass
// ---------------------------------------------------------------------

/// Walks the resolved statements: the statement-local lints, then
/// the cross-statement lints over the per-table field collections
/// (nested children plus standalones — the engine's
/// `all_tb_fields` view of a table).
fn resolved_pass(resolved: &SdlFile, anchors: &Anchors, out: &mut Vec<Finding>) {
    // The per-scope table index: name → the resolved table (host
    // lookups for nesting, schema flags, and relation endpoints).
    let mut tables: BTreeMap<(ScopeKey, String), &Table> = BTreeMap::new();
    // Every field of every table, nested and standalone alike.
    let mut fields: BTreeMap<(ScopeKey, String), Vec<&Field>> = BTreeMap::new();
    let mut walk = ScopeWalk::new();
    for stmt in &resolved.items {
        let scope = walk.lookup_scope(stmt);
        walk.advance(stmt);
        match stmt {
            Stmt::Table(table) => {
                tables.insert((scope.clone(), table.name.raw.clone()), table);
                for child in &table.nested {
                    if let Stmt::Field(field) = child {
                        fields
                            .entry((scope.clone(), table.name.raw.clone()))
                            .or_default()
                            .push(field);
                    }
                }
            }
            Stmt::Field(field) => {
                fields
                    .entry((scope.clone(), field.on.raw.clone()))
                    .or_default()
                    .push(field);
            }
            _ => {}
        }
    }

    let mut walk = ScopeWalk::new();
    for stmt in &resolved.items {
        let scope = walk.lookup_scope(stmt);
        walk.advance(stmt);
        match stmt {
            Stmt::Table(table) => {
                let anchor = anchors.get(&scope, Family::Table, &table.name.raw);
                table_lints(table, &scope, anchor, out);
            }
            Stmt::Field(field) => {
                let path_key = field_path_text(field);
                let anchor = anchors.get(&scope, Family::Field, &path_key);
                non_nested(
                    Family::Field,
                    format!("{}.{path_key}", field.on.raw),
                    &field.on.raw,
                    &scope,
                    &tables,
                    anchor,
                    out,
                );
            }
            Stmt::Index(index) => {
                let anchor = anchors.get(&scope, Family::Index, &index.name.raw);
                non_nested(
                    Family::Index,
                    index.name.raw.clone(),
                    &index.on.raw,
                    &scope,
                    &tables,
                    anchor,
                    out,
                );
            }
            Stmt::Event(event) => {
                let anchor = anchors.get(&scope, Family::Event, &event.name.raw);
                non_nested(
                    Family::Event,
                    event.name.raw.clone(),
                    &event.on.raw,
                    &scope,
                    &tables,
                    anchor,
                    out,
                );
            }
            Stmt::User(user) => {
                let anchor = anchors.get(&scope, Family::User, &user.name.raw);
                match &user.pass {
                    Some(Pass::Password(_)) => out.push(finding(
                        FindingKind::UserPlaintextPassword,
                        &scope,
                        Family::User,
                        user.name.raw.clone(),
                        anchor,
                    )),
                    Some(Pass::Hash(hash)) if hash == SENTINEL_DECODED => out.push(finding(
                        FindingKind::SentinelApply {
                            what: "user passhash",
                        },
                        &scope,
                        Family::User,
                        user.name.raw.clone(),
                        anchor,
                    )),
                    _ => {}
                }
                // A scram-only user carries a working credential (the
                // engine keeps the verifier; OVERWRITE re-apply is
                // safe) — not secret-less. (The passscram SENTINEL
                // needs no lint arm: the parser's SDL-E1059 rejects it
                // outright — unlike the passhash sentinel, the clause
                // can never round-trip, so there is no about-to-apply
                // state to warn on.)
                if user.pass.is_none() && user.scram.is_none() {
                    out.push(finding(
                        FindingKind::UserSecretless,
                        &scope,
                        Family::User,
                        user.name.raw.clone(),
                        anchor,
                    ));
                }
                duplicates(
                    "roles",
                    user.roles.iter().copied().map(role_word),
                    &scope,
                    Family::User,
                    &user.name.raw,
                    anchor,
                    out,
                );
            }
            Stmt::Analyzer(analyzer) => analyzer_lints(analyzer, &scope, anchors, out),
            Stmt::Param(param) => {
                if param.value.is_none() {
                    out.push(finding(
                        FindingKind::ParamValueOmitted,
                        &scope,
                        Family::Param,
                        param.name.raw.clone(),
                        anchors.get(&scope, Family::Param, &param.name.raw),
                    ));
                }
            }
            Stmt::Api(api) => api_lints(api, &scope, anchors, out),
            Stmt::Access(access) => access_sentinel_lints(access, &scope, anchors, out),
            _ => {}
        }
    }

    // The field lints, one home: every field of every table (nested
    // children and standalones in one per-table view — the engine's
    // `all_tb_fields`), then the cross-statement computed/subfield
    // conflicts over the same collection. A field whose host table is
    // not in the file runs the kind walks only (the codegen model's
    // best-effort posture — the engine owns the define-time check).
    for ((scope, table_name), table_fields) in &fields {
        let table = tables.get(&(scope.clone(), table_name.clone()));
        let table_anchor = anchors.get(scope, Family::Table, table_name);
        for field in table_fields {
            // A standalone field anchors its own statement when the
            // authored file carries it; nested children (and
            // adoptees) fall back to the table's span.
            let field_anchor = anchors.get(scope, Family::Field, &field_path_text(field));
            let anchor = if field_anchor.0.is_some() {
                field_anchor
            } else {
                table_anchor
            };
            if let Some(table) = table {
                field_against_host_lints(field, table, scope, anchor, out);
            }
            field_standalone_lints(field, scope, anchor, out);
        }
        // The 255-INLINE-fields cap (3.3): one finding per table over
        // the line — the count is a table aggregate, invisible to any
        // single field's parse (the nested form's per-field mirrors
        // all ran at the fold).
        let inline_count = table_fields.iter().filter(|f| f.inline).count();
        if inline_count > 255 {
            out.push(finding(
                FindingKind::TooManyInlineFields {
                    count: inline_count,
                },
                scope,
                Family::Field,
                table_name.clone(),
                table_anchor,
            ));
        }
        // One finding per colliding pair — the engine's
        // ComputedParentFieldConflict / ComputedNestedFieldConflict
        // class. The first-segment comparison is decoded
        // (`Ident.raw` is the unescaped interior, so a quoted `c`
        // pairs with a bare one — the engine's own path semantics).
        for computed in table_fields.iter().filter(|f| f.computed.is_some()) {
            let Some(computed_name) = computed.name.sole_name() else {
                continue;
            };
            for other in table_fields.iter() {
                if let [first, rest @ ..] = &other.name.segments[..]
                    && !rest.is_empty()
                    && matches!(first, FieldPathSeg::Name(name) if name.raw == computed_name.raw)
                    && !std::ptr::eq(*other, *computed)
                {
                    out.push(finding(
                        FindingKind::ComputedConflict {
                            computed: computed_name.raw.clone(),
                            other: field_path_text(other),
                        },
                        scope,
                        Family::Field,
                        format!("{table_name}.{}", field_path_text(other)),
                        table_anchor,
                    ));
                }
            }
        }
    }
}

/// The statement-local table lints: world-open permissions,
/// un-enforced relations, and the delete-full ambiguity.
fn table_lints(table: &Table, scope: &ScopeKey, anchor: Anchor, out: &mut Vec<Finding>) {
    if table.permissions.as_ref().is_some_and(permissions_all_full) {
        out.push(finding(
            FindingKind::TablePermissionsFull,
            scope,
            Family::Table,
            table.name.raw.clone(),
            anchor,
        ));
    }
    if let TableKind::Relation { enforced, .. } = &table.kind
        && !enforced
    {
        out.push(finding(
            FindingKind::RelationWithoutEnforced,
            scope,
            Family::Table,
            table.name.raw.clone(),
            anchor,
        ));
    }
    if let Some(OpPermissions::Groups(groups)) = &table.permissions
        && groups.iter().any(|group| {
            group.value == Permission::Full
                && group
                    .ops
                    .iter()
                    .any(|op| matches!(op, PermissionOp::Delete))
        })
    {
        out.push(finding(
            FindingKind::TableDeleteFullAmbiguous,
            scope,
            Family::Table,
            table.name.raw.clone(),
            anchor,
        ));
    }
}

/// The lints that need the field's host table: the relation-endpoint
/// rewrite and the flexible/schema-flag pairing (the promotions
/// whose trigger is cross-statement).
fn field_against_host_lints(
    field: &Field,
    table: &Table,
    scope: &ScopeKey,
    anchor: Anchor,
    out: &mut Vec<Finding>,
) {
    if let TableKind::Relation { .. } = &table.kind
        && let Some(name) = field.name.sole_name()
    {
        let field_name = name.raw.clone();
        if matches!(field_name.as_str(), "in" | "out")
            && field
                .kind
                .as_ref()
                .and_then(|kind| parse_kind_ast(&kind.text))
                .is_some_and(|ast| kind_contains_record(&ast))
        {
            out.push(finding(
                FindingKind::RelationEndpointRewrite {
                    field: field_name.clone(),
                },
                scope,
                Family::Field,
                format!("{}.{field_name}", table.name.raw),
                anchor,
            ));
        }
    }
    if field.flexible && table.schema_full != Some(true) {
        out.push(finding(
            FindingKind::FlexibleOnSchemalessHost,
            scope,
            Family::Field,
            format!("{}.{path}", table.name.raw, path = field_path_text(field)),
            anchor,
        ));
    }
    if field.inline && !matches!(table.kind, TableKind::Relation { .. }) {
        out.push(finding(
            FindingKind::InlineOnNonRelationHost,
            scope,
            Family::Field,
            format!("{}.{path}", table.name.raw, path = field_path_text(field)),
            anchor,
        ));
    }
}

/// `permissions: full;` and its long spelling — per-operation groups
/// covering all four operations with `full` values — grant everyone,
/// including unauthenticated connections, access to every record.
/// Omission (the secure default, implicit `none`), explicit `none`,
/// and partial openings never fire.
fn permissions_all_full(permissions: &OpPermissions) -> bool {
    const ALL_OPS: [PermissionOp; 4] = [
        PermissionOp::Select,
        PermissionOp::Create,
        PermissionOp::Update,
        PermissionOp::Delete,
    ];
    match permissions {
        OpPermissions::Full => true,
        OpPermissions::Groups(groups) => {
            groups.iter().all(|g| g.value == Permission::Full)
                && ALL_OPS
                    .iter()
                    .all(|op| groups.iter().any(|g| g.ops.contains(op)))
        }
        OpPermissions::None => false,
    }
}

/// The kind walks that do not need the host: `reference;`'s type
/// predicate and the `id` field's record-id-key compatibility.
fn field_standalone_lints(field: &Field, scope: &ScopeKey, anchor: Anchor, out: &mut Vec<Finding>) {
    let Some(kind) = field.kind.as_ref() else {
        return;
    };
    let Some(ast) = parse_kind_ast(&kind.text) else {
        return;
    };
    if field.reference.is_some() && !kind_allows_reference(&ast) {
        out.push(finding(
            FindingKind::ReferenceKind {
                kind: kind.text.clone(),
            },
            scope,
            Family::Field,
            field_path_text_with_host(field),
            anchor,
        ));
    }
    if field.name.sole_name().is_some_and(|name| name.raw == "id")
        && !kind_supports_record_id_key(&ast)
    {
        out.push(finding(
            FindingKind::IdFieldKind {
                kind: kind.text.clone(),
            },
            scope,
            Family::Field,
            field_path_text_with_host(field),
            anchor,
        ));
    }
}

/// The non-nested-placement flag: a top-level field/index/event
/// whose host table is declared in the same scope (views excepted —
/// a view's block is its query, nothing can nest there). `key` is
/// the finding's display key (a field's carries its host).
fn non_nested(
    family: Family,
    key: String,
    on: &str,
    scope: &ScopeKey,
    tables: &BTreeMap<(ScopeKey, String), &Table>,
    anchor: Anchor,
    out: &mut Vec<Finding>,
) {
    let nestable = tables
        .get(&(scope.clone(), on.to_string()))
        .is_some_and(|table| !table.kind.is_view());
    if nestable {
        out.push(finding(
            FindingKind::NonNestedPlacement {
                family: family_word(family),
            },
            scope,
            family,
            key,
            anchor,
        ));
    }
}

fn analyzer_lints(
    analyzer: &Analyzer,
    scope: &ScopeKey,
    anchors: &Anchors,
    out: &mut Vec<Finding>,
) {
    let anchor = anchors.get(scope, Family::Analyzer, &analyzer.name.raw);
    if let Some(filters) = &analyzer.filters {
        for filter in filters {
            let (word, min, max) = match filter {
                Filter::EdgeNgram(min, max) => ("edgengram", *min, *max),
                Filter::Ngram(min, max) => ("ngram", *min, *max),
                _ => continue,
            };
            if min > max {
                out.push(finding(
                    FindingKind::AnalyzerMinMax {
                        filter: word,
                        min,
                        max,
                    },
                    scope,
                    Family::Analyzer,
                    analyzer.name.raw.clone(),
                    anchor,
                ));
            }
        }
        duplicates(
            "filters",
            filters.iter().map(filter_word),
            scope,
            Family::Analyzer,
            &analyzer.name.raw,
            anchor,
            out,
        );
    }
    if let Some(tokenizers) = &analyzer.tokenizers {
        duplicates(
            "tokenizers",
            tokenizers.iter().map(tokenizer_word),
            scope,
            Family::Analyzer,
            &analyzer.name.raw,
            anchor,
            out,
        );
    }
}

fn api_lints(api: &Api, scope: &ScopeKey, anchors: &Anchors, out: &mut Vec<Finding>) {
    // The no-op shapes. The engine materializes a catch-all for every
    // api (INFO always prints `FOR ANY … PERMISSIONS …`), so an
    // absent `any` IS the implicit no-op catch-all — and the reader's
    // absent-`any` rule folds an authored empty `any { }` onto
    // exactly that shape (no THEN, no middleware, default FULL), so
    // the check is the same in both modes. A configured catch-all
    // (`then:`, middleware, or a permissions guard) is not a no-op.
    let no_op_catch_all = api.any.as_ref().is_none_or(is_noop_any);
    if no_op_catch_all {
        out.push(finding(
            FindingKind::ApiNoOp {
                // The zero-`FOR` body (no groups, no any) gets its own
                // message; every other shape is the catch-all's.
                catch_all: !(api.actions.is_empty() && api.any.is_none()),
            },
            scope,
            Family::Api,
            api.path.clone(),
            anchors.get(scope, Family::Api, &api.path),
        ));
    }
}

fn is_noop_any(any: &ApiAnyBlock) -> bool {
    // `permissions: full;` is the default spelled out (the absent-any
    // rule folds it to absence on the read side) — the resolved-state
    // posture, applied to the authored spelling too.
    any.fallback.is_none()
        && any.config.middleware.is_empty()
        && any
            .config
            .permissions
            .as_ref()
            .is_none_or(|permission| matches!(permission, Permission::Full))
}

/// The access-side half of the sentinel-apply advisory: every key
/// slot SDL's shape-only rule pins to the sentinel (symmetric verify
/// keys, issuer keys) defines the literal as real key material when
/// applied — and so does a sentinel spelled on an asymmetric verify
/// key, the one key slot that stays authorable with any literal
/// (INFO renders real public keys verbatim, so an adopted asymmetric
/// key never carries the sentinel; only an authored one can).
fn access_sentinel_lints(
    access: &Access,
    scope: &ScopeKey,
    anchors: &Anchors,
    out: &mut Vec<Finding>,
) {
    let anchor = anchors.get(scope, Family::Access, &access.name.raw);
    let mut check = |source: &JwtSource| {
        if let JwtVerify::Key { key, .. } = &source.verify
            && key.canon == SECRET_SENTINEL_CANON
        {
            out.push(finding(
                FindingKind::SentinelApply {
                    what: "access verification key",
                },
                scope,
                Family::Access,
                access.name.raw.clone(),
                anchor,
            ));
        }
        if let Some(issuer) = &source.issuer
            && issuer
                .key
                .as_ref()
                .is_some_and(|key| key.canon == SECRET_SENTINEL_CANON)
        {
            out.push(finding(
                FindingKind::SentinelApply {
                    what: "access issuer key",
                },
                scope,
                Family::Access,
                access.name.raw.clone(),
                anchor,
            ));
        }
    };
    match &access.kind {
        AccessKind::Jwt { source } => check(source),
        AccessKind::Record { jwt, .. } | AccessKind::Bearer { jwt, .. } => {
            if let Some(source) = jwt {
                check(source);
            }
        }
    }
}

// ---------------------------------------------------------------------
// The authored pass
// ---------------------------------------------------------------------

/// Walks the file as written (spans zipped positionally): the
/// capability-dropped access duration sides and rule 8's raw bodies.
fn authored_pass(sdl: &SdlFile, anchored: &[(Span, usize)], out: &mut Vec<Finding>) {
    // The mixin names some table spreads — SDL-L0021's complement
    // (order-blind, whole-file: a mixin spread anywhere is used).
    let spread_names: BTreeSet<&str> = sdl
        .items
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Table(table) => Some(table.nested.iter().filter_map(|nested| match nested {
                Stmt::Spread(spread) => Some(spread.name.raw.as_str()),
                _ => None,
            })),
            _ => None,
        })
        .flatten()
        .collect();
    // The member lists by mixin name — SDL-L0022's subject (the
    // shadow pairs one table's spreads create).
    let mixin_members: BTreeMap<&str, &Vec<Stmt>> = sdl
        .items
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Mixin(mixin) => Some((mixin.name.raw.as_str(), &mixin.members)),
            _ => None,
        })
        .collect();
    let mut walk = ScopeWalk::new();
    for (stmt, anchor) in sdl.items.iter().zip(anchored) {
        let (span, span_home) = *anchor;
        let scope = walk.lookup_scope(stmt);
        walk.advance(stmt);
        match stmt {
            Stmt::Access(access) => {
                if let Some(duration) = &access.duration {
                    let token_capable = match &access.kind {
                        AccessKind::Jwt { source } => source.issuer.is_some(),
                        AccessKind::Record { .. } | AccessKind::Bearer { .. } => true,
                    };
                    let grant_capable = match &access.kind {
                        AccessKind::Jwt { .. } => false,
                        AccessKind::Record { refresh, .. } => *refresh,
                        AccessKind::Bearer { .. } => true,
                    };
                    if duration.grant.is_some() && !grant_capable {
                        out.push(finding(
                            FindingKind::AccessDurationDropped { side: "grant" },
                            &scope,
                            Family::Access,
                            access.name.raw.clone(),
                            (Some(span), span_home),
                        ));
                    }
                    if duration.token.is_some() && !token_capable {
                        out.push(finding(
                            FindingKind::AccessDurationDropped { side: "token" },
                            &scope,
                            Family::Access,
                            access.name.raw.clone(),
                            (Some(span), span_home),
                        ));
                    }
                }
            }
            Stmt::Mixin(mixin) => {
                if !spread_names.contains(mixin.name.raw.as_str()) {
                    out.push(finding(
                        FindingKind::UnusedMixin,
                        &scope,
                        Family::Mixin,
                        mixin.name.raw.clone(),
                        (Some(span), span_home),
                    ));
                }
            }
            Stmt::Table(table) => {
                spread_shadow_lints(table, &mixin_members, &scope, span_home, out);
            }
            Stmt::Raw(raw) => {
                if let Err(message) = crate::expr::probe_statements(&raw.body) {
                    out.push(finding(
                        FindingKind::RawBodyUnparsable { message },
                        &scope,
                        Family::Raw,
                        raw_body_key(&raw.body),
                        (Some(span), span_home),
                    ));
                }
                if raw.body.contains(SENTINEL_DECODED) {
                    out.push(finding(
                        FindingKind::SentinelApply {
                            what: "key inside a raw statement body",
                        },
                        &scope,
                        Family::Raw,
                        raw_body_key(&raw.body),
                        (Some(span), span_home),
                    ));
                }
            }
            _ => {}
        }
    }
}

/// SDL-L0022: within one table body, a later spread's member
/// shadowing an earlier spread's same-keyed member (spec/mixin.md's
/// last-occurrence-wins resolution — the file compiles to the LATER
/// definition). Each shadowed pair is one finding, anchored at the
/// later spread's entry; an explicit member of the same key decides
/// the collision position-independently and suppresses the finding
/// (the author decided it — the L0011 precedent of surfacing what the
/// engine would otherwise decide silently).
fn spread_shadow_lints(
    table: &Table,
    mixin_members: &BTreeMap<&str, &Vec<Stmt>>,
    scope: &ScopeKey,
    home: usize,
    out: &mut Vec<Finding>,
) {
    // The resolved spreads in body order (relative order — the
    // semantics — survives the filter; the lint input is clean, an
    // unresolved spread is defensive only).
    let resolved: Vec<(&Spread, &Vec<Stmt>)> = table
        .nested
        .iter()
        .filter_map(|nested| match nested {
            Stmt::Spread(spread) => mixin_members
                .get(spread.name.raw.as_str())
                .map(|members| (spread, *members)),
            _ => None,
        })
        .collect();
    if resolved.len() < 2 {
        return;
    }
    let explicit: Vec<MemberKey> = table
        .nested
        .iter()
        .filter_map(|stmt| stmt.member_key())
        .collect();
    let member_keys: Vec<Vec<Option<MemberKey>>> = resolved
        .iter()
        .map(|(_, members)| {
            members
                .iter()
                .map(|stmt| stmt.member_key())
                .collect::<Vec<_>>()
        })
        .collect();
    // The resolution itself, one home with expansion: a finding names
    // a shadow only when its later member actually LANDS — an
    // even-later spread beats it in a three-way collision, and that
    // spread's own findings name the real winner.
    let lands = crate::expand::winning_members(&member_keys, &explicit);
    for (later, (later_spread, _)) in resolved.iter().enumerate() {
        for (pos, key) in member_keys[later].iter().enumerate() {
            let Some(key) = key else {
                continue;
            };
            if !lands[later][pos] {
                continue;
            }
            for (earlier, (earlier_spread, _)) in resolved.iter().take(later).enumerate() {
                if !member_keys[earlier]
                    .iter()
                    .flatten()
                    .any(|other| other == key)
                {
                    continue;
                }
                out.push(finding(
                    FindingKind::SpreadMemberShadow {
                        kind: key.0,
                        key: key.1.clone(),
                        earlier: earlier_spread.name.raw.clone(),
                        later: later_spread.name.raw.clone(),
                    },
                    scope,
                    Family::Table,
                    table.name.raw.clone(),
                    (Some(later_spread.span), home),
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------
// The engine-predicate mirrors (v3.2.4, source-verified)
// ---------------------------------------------------------------------

/// `Kind::allows_reference` mirrored over the structured kind —
/// upstream `validate_reference_options` (define/field.rs): record
/// kinds, unions whose every arm is record-or-`none`, one level of
/// `array`/`set` over either, and literal arrays of the same.
/// `record<user> | string` is the specced rejection. Upstream
/// compares the Either its parser already built (flatten + dedupe +
/// singleton-collapse), so the walk normalizes first — `option<none>`
/// IS bare `none` there and lands in the catch-all rejection, while
/// `none | none | record` flattens onto `option<record>` and passes.
/// A normalized `Option` holds the union of the surviving arms
/// (`none | record<a> | record<b>` folds to
/// `Option(Union([record<a>, record<b>]))` ≡ upstream
/// `Either([None, a, b])`), so every Option position distributes
/// over the arms — `none` itself is always a valid arm.
fn kind_allows_reference(kind: &KindAst) -> bool {
    match &normalize_kind(kind) {
        KindAst::Record(_) => true,
        KindAst::Option(inner) => option_arms_reference_valid(inner),
        KindAst::Union(arms) => arms.iter().all(|arm| reference_arm(arm, true)),
        KindAst::Sized {
            inner: Some(inner), ..
        } => match inner.as_ref() {
            KindAst::Record(_) => true,
            // array<option<T>> is upstream array<none | T>.
            KindAst::Option(inner) => option_arms_reference_valid(inner),
            KindAst::Union(arms) => arms.iter().all(|arm| reference_arm(arm, true)),
            _ => false,
        },
        KindAst::ArrayLit(kinds) => kinds.iter().all(|k| reference_arm(k, true)),
        _ => false,
    }
}

/// A normalized option's interior: a union's arms distribute (each
/// at the outer allowance), anything else is the single arm.
fn option_arms_reference_valid(inner: &KindAst) -> bool {
    match inner {
        KindAst::Union(arms) => arms.iter().all(|arm| reference_arm(arm, true)),
        arm => reference_arm(arm, true),
    }
}

/// Upstream's `valid`: one arm of a union or element of a literal
/// array, where `outer` is that position's nesting allowance. The
/// mirror is exact — `option<T>`/unions fold into upstream's
/// `Either` upstream of this walk, and `valid` has no Either arm, so
/// they are `false` in every nested position (`[none | record<t>]`
/// with `reference;` is an engine rejection, not an acceptance).
fn reference_arm(kind: &KindAst, outer: bool) -> bool {
    match kind {
        KindAst::Word("none") | KindAst::Record(_) => true,
        KindAst::Sized {
            inner: Some(inner), ..
        } => outer && reference_arm(inner, false),
        KindAst::ArrayLit(kinds) => outer && kinds.iter().all(|k| reference_arm(k, false)),
        _ => false,
    }
}

/// `RecordIdKeyLit::kind_supported` mirrored over the structured
/// kind (expr/record_id/key.rs): `any`, `int`, `number`, `string`,
/// `uuid`, `array` in every spelling, and `object`; int, string,
/// array, and object literals; a union whose every arm qualifies.
/// `none` never qualifies, so `option<T>` (its union unfolding)
/// always fails.
fn kind_supports_record_id_key(kind: &KindAst) -> bool {
    match kind {
        KindAst::Any => true,
        KindAst::Word(word) => matches!(*word, "int" | "number" | "string" | "uuid" | "object"),
        KindAst::Str(_) => true,
        // Number-literal kinds keep their source text — only integers
        // are record-id keys upstream (float and duration literals
        // are not).
        KindAst::Num(text) => text.parse::<i64>().is_ok(),
        KindAst::Sized { array: true, .. } => true,
        KindAst::Object(_) => true,
        KindAst::ArrayLit(_) => true,
        KindAst::Union(arms) => arms.iter().all(kind_supports_record_id_key),
        KindAst::Option(_) => false,
        _ => false,
    }
}

/// Whether a kind holds a record anywhere — the endpoint-rewrite
/// hazard's shape (a record-typed `in`/`out` field on a relation).
fn kind_contains_record(kind: &KindAst) -> bool {
    match kind {
        KindAst::Record(_) => true,
        KindAst::Option(inner)
        | KindAst::Sized {
            inner: Some(inner), ..
        } => kind_contains_record(inner),
        KindAst::Union(arms) | KindAst::ArrayLit(arms) => arms.iter().any(kind_contains_record),
        KindAst::Object(entries) => entries.iter().any(|(_, kind)| kind_contains_record(kind)),
        _ => false,
    }
}

// ---------------------------------------------------------------------
// The reference lint — SDL-L0024
// ---------------------------------------------------------------------

/// The reference lint over [`crate::refs`]' extraction report (see
/// the module doc for the soundness contract). It anchors the
/// OWNING statement like every other check, suppresses through the
/// allow machinery, and stays advisory: the flat catalog makes an
/// undefined target legal SDL, never a diagnostic.
///
/// It fires per (statement, kind, name) — the report is positional
/// and would repeat one typo per capture. A DEFINED target is never
/// flagged, wherever it sits: the compiler orders emission itself.
fn reference_pass(
    sdl: &SdlFile,
    resolved: &SdlFile,
    anchored: &[(Span, usize)],
    out: &mut Vec<Finding>,
) {
    let report = crate::refs::extract_file(sdl);
    if report.refs.is_empty() {
        return;
    }
    let defs = scoped_definition_index(sdl);
    let resolved_defs: BTreeSet<(ScopeKey, RefKind, String)> =
        scoped_definition_index(resolved).into_keys().collect();
    // The module names — a `mod::name::sub` call resolves against the
    // module `name` (`get_db_module` on the first segment), so a
    // module definition covers its calls' first segment. Scoped like
    // every definition: modules live per database.
    let mut module_walk = ScopeWalk::new();
    let mut modules: BTreeSet<(ScopeKey, String)> = BTreeSet::new();
    for stmt in sdl.items.iter().chain(resolved.items.iter()) {
        if let Stmt::Module(module) = stmt {
            let scope = module_walk.lookup_scope(stmt);
            if let Some(name) = &module.name {
                modules.insert((scope, name.raw.clone()));
            }
        }
        module_walk.advance(stmt);
    }
    // Each host statement's scope, in stream order (the findings
    // group under the owning statement like every other check).
    let mut walk = ScopeWalk::new();
    let scopes: Vec<ScopeKey> = sdl
        .items
        .iter()
        .map(|stmt| {
            let scope = walk.lookup_scope(stmt);
            walk.advance(stmt);
            scope
        })
        .collect();
    let mut seen: BTreeSet<(usize, RefKind, String)> = BTreeSet::new();
    for reference in &report.refs {
        if !seen.insert((reference.stmt, reference.kind, reference.name.clone())) {
            continue;
        }
        let scope = &scopes[reference.stmt];
        let defined = defs.contains_key(&(scope.clone(), reference.kind, reference.name.clone()))
            || resolved_defs.contains(&(scope.clone(), reference.kind, reference.name.clone()));
        if !defined {
            match reference.kind {
                // A fn-body read of anything outside the engine's
                // fn-body environment resolves to nothing: the
                // isolated context binds only the arguments (the
                // walker already shadows those), the session four,
                // and the doc trio on a document-bound call — a
                // client-bound value never reaches it. A `DEFINE
                // PARAM` DOES resolve inside a body (the param
                // read's catalog fall-through, get_db_param), which
                // the definition set above already covers.
                RefKind::Param if reference.site == Site::FnBody => {
                    if !SESSION_PARAMS.contains(&reference.name.as_str())
                        && !DOC_SCOPE_PARAMS.contains(&reference.name.as_str())
                    {
                        push_reference_finding(
                            out,
                            FindingKind::UndefinedReference {
                                kind: reference.kind,
                                name: reference.name.clone(),
                                site: site_phrase(reference.site),
                            },
                            sdl,
                            &scopes,
                            anchored,
                            reference.stmt,
                        );
                    }
                }
                // Everywhere else a param read may bind a client
                // value — muted, never guessed (module doc).
                RefKind::Param => {}
                // A `fn::` call whose path no function defines is a
                // typo UNLESS an engine-resolvable vocabulary covers
                // it: `mod::name::…` resolves against module `name`,
                // `silo::`/`ml::` names carry their version tail
                // (`::<major.minor.patch>`) — deployed artifacts,
                // never `DEFINE`-able — and a path the builtin table
                // knows resolves as a builtin (api middleware heads
                // are licensed bare).
                RefKind::Fn => {
                    let covered_by_module = reference
                        .name
                        .split("::")
                        .next()
                        .is_some_and(|first| modules.contains(&(scope.clone(), first.into())));
                    if !covered_by_module
                        && !reference.name.contains("::<")
                        && !is_builtin_path(&reference.name)
                    {
                        push_reference_finding(
                            out,
                            FindingKind::UndefinedReference {
                                kind: reference.kind,
                                name: reference.name.clone(),
                                site: site_phrase(reference.site),
                            },
                            sdl,
                            &scopes,
                            anchored,
                            reference.stmt,
                        );
                    }
                }
                RefKind::Table | RefKind::Analyzer | RefKind::Mixin => {
                    push_reference_finding(
                        out,
                        FindingKind::UndefinedReference {
                            kind: reference.kind,
                            name: reference.name.clone(),
                            site: site_phrase(reference.site),
                        },
                        sdl,
                        &scopes,
                        anchored,
                        reference.stmt,
                    );
                }
            }
        }
    }
}

/// Emits one reference finding anchored at its owning statement: the
/// host's (family, key) for sorting, its span and home for rendering
/// and suppression. References never anchor to raw or include
/// statements (both mute in the extractor), so the classify key is
/// always a real definition's.
fn push_reference_finding(
    out: &mut Vec<Finding>,
    kind: FindingKind,
    sdl: &SdlFile,
    scopes: &[ScopeKey],
    anchored: &[(Span, usize)],
    stmt: usize,
) {
    let (family, key) = classify(&sdl.items[stmt]);
    let (span, home) = anchored[stmt];
    out.push(finding(
        kind,
        &scopes[stmt],
        family,
        key,
        (Some(span), home),
    ));
}

/// The definition positions one file contributes, scoped:
/// (scope, kind, name) → the FIRST statement index defining it —
/// `OVERWRITE` redefinitions keep the earliest (the check reads
/// membership, not position). Params are in
/// (a `fn::` body's param read falls through to the catalog —
/// `get_db_param` — so a `DEFINE PARAM` satisfies a body read; the
/// session four and the doc trio never need a definition and stay
/// exempt at the check, not here).
fn scoped_definition_index(file: &SdlFile) -> BTreeMap<(ScopeKey, RefKind, String), usize> {
    let mut defs = BTreeMap::new();
    let mut walk = ScopeWalk::new();
    for (index, stmt) in file.items.iter().enumerate() {
        let scope = walk.lookup_scope(stmt);
        walk.advance(stmt);
        let (kind, name) = match stmt {
            Stmt::Table(table) => (RefKind::Table, table.name.raw.clone()),
            Stmt::Analyzer(analyzer) => (RefKind::Analyzer, analyzer.name.raw.clone()),
            Stmt::Function(function) => (RefKind::Fn, function.name.segments.join("::")),
            Stmt::Param(param) => (RefKind::Param, param.name.raw.clone()),
            Stmt::Mixin(mixin) => (RefKind::Mixin, mixin.name.raw.clone()),
            _ => continue,
        };
        defs.entry((scope, kind, name)).or_insert(index);
    }
    defs
}

/// Whether a mangled fn-path name is the engine's builtin vocabulary
/// (`surrealdb_sql::builtin_paths::PATHS`, the table the parser
/// resolves `a::b` paths against): such a reference resolves without
/// any definition — an api middleware head may be licensed bare.
fn is_builtin_path(name: &str) -> bool {
    surrealdb_sql::builtin_paths::PATHS
        .entries()
        .any(|(path, _)| path.as_ref().eq_ignore_ascii_case(name))
}

/// The capture site's phrase in the finding's sentence ("referenced
/// in this view's SELECT"). Exhaustive over the frozen site table —
/// a new site variant breaks this until it is phrased.
fn site_phrase(site: Site) -> &'static str {
    match site {
        Site::FieldDefault => "in this field's default",
        Site::FieldValue => "in this field's value clause",
        Site::FieldAssert => "in this field's assertion",
        Site::FieldComputed => "in this computed field",
        Site::FieldPermission => "in this field permission",
        Site::ReferenceOnDelete => "in this field's on-delete strategy",
        Site::TablePermission => "in this table permission",
        Site::BucketPermission => "in this bucket permission",
        Site::CountFilter => "in this count index's filter",
        Site::IndexFields => "in this index's fields",
        Site::EventWhen => "in this event's when clause",
        Site::EventThen => "in this event's then clause",
        Site::View => "in this view",
        Site::FnBody => "in this function body",
        Site::ParamValue => "in this param's value",
        Site::StatementPermission => "in this statement's permission",
        Site::ApiAction => "in this api action",
        Site::AccessSignup => "in this access's signup clause",
        Site::AccessSignin => "in this access's signin clause",
        Site::AccessAuthenticate => "in this access's authenticate clause",
        Site::AccessContext => "in this access's context clause",
        Site::Other => "at define time",
    }
}

/// A reference kind's word in the finding's sentence.
fn kind_word(kind: RefKind) -> &'static str {
    match kind {
        RefKind::Table => "table",
        RefKind::Fn => "function",
        RefKind::Param => "param",
        RefKind::Analyzer => "analyzer",
        RefKind::Mixin => "mixin",
    }
}

// ---------------------------------------------------------------------
// Small formatting helpers
// ---------------------------------------------------------------------

/// The authored anchor a finding renders through: the statement's
/// span when unambiguous (`None` renders sourceless) and the home
/// index its span coordinates belong to.
type Anchor = (Option<Span>, usize);

fn finding(
    kind: FindingKind,
    scope: &ScopeKey,
    family: Family,
    key: String,
    anchor: Anchor,
) -> Finding {
    let (span, home) = anchor;
    Finding {
        kind,
        scope: scope.clone(),
        family,
        key,
        span,
        home,
    }
}

/// Emits one finding per duplicated word (second and later
/// occurrences), in list order.
fn duplicates(
    list: &'static str,
    words: impl Iterator<Item = String>,
    scope: &ScopeKey,
    family: Family,
    key: &str,
    anchor: Anchor,
    out: &mut Vec<Finding>,
) {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for word in words {
        if !seen.insert(word.clone()) {
            out.push(finding(
                FindingKind::ListDuplicate { list, word },
                scope,
                family,
                key.to_string(),
                anchor,
            ));
        }
    }
}

/// A field's path text (`stats.views`, `tags.*`) — the shared key
/// spelling (the merge's match key; see
/// [`crate::info::normalize::field_path_key`]).
fn field_path_text(field: &Field) -> String {
    crate::info::normalize::field_path_key(&field.name)
}

/// A standalone field's path text with its host (`t.stats.views`) —
/// the resolved-pass key shape for field findings.
fn field_path_text_with_host(field: &Field) -> String {
    format!(
        "{}.{path}",
        field.on.raw,
        path = crate::info::normalize::field_path_key(&field.name)
    )
}

/// A raw body's finding key: the first word-shaped run, else a
/// length (raw keys are display-only — `Family::Raw`'s match key is
/// the empty string).
fn raw_body_key(body: &str) -> String {
    let trimmed = body.trim();
    let word_len = crate::lexer::word_prefix_len(trimmed);
    if word_len > 0 {
        trimmed[..word_len].to_string()
    } else {
        format!("{} bytes", trimmed.len())
    }
}

fn family_word(family: Family) -> &'static str {
    match family {
        Family::Index => "index",
        Family::Event => "event",
        Family::Field => "field",
        _ => "definition",
    }
}

fn role_word(role: Role) -> String {
    match role {
        Role::Owner => "owner",
        Role::Editor => "editor",
        Role::Viewer => "viewer",
    }
    .to_string()
}

fn tokenizer_word(tokenizer: &Tokenizer) -> String {
    match tokenizer {
        Tokenizer::Blank => "blank",
        Tokenizer::Camel => "camel",
        Tokenizer::Class => "class",
        Tokenizer::Punct => "punct",
    }
    .to_string()
}

fn filter_word(filter: &Filter) -> String {
    match filter {
        Filter::Ascii => "ascii".to_string(),
        Filter::Lowercase => "lowercase".to_string(),
        Filter::Uppercase => "uppercase".to_string(),
        Filter::EdgeNgram(min, max) => format!("edgengram({min}, {max})"),
        Filter::Ngram(min, max) => format!("ngram({min}, {max})"),
        Filter::Snowball(language) => {
            format!("snowball({})", language.value.as_str().to_lowercase())
        }
        Filter::Mapper(path) => format!("mapper('{path}')"),
    }
}

// ---------------------------------------------------------------------
// The finding kinds — codes, messages, helps, urls
// ---------------------------------------------------------------------

/// What is wrong with a `// surform-lint-allow:` directive itself
/// (SDL-L0023's shapes — one code, three failures, all anchoring at
/// the offending comment and none suppressible).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LintAllowProblem {
    /// A code the registry does not hold — the allow suppresses
    /// nothing (the typo trap; codes are exact spellings).
    UnknownCode(String),
    /// A comment starting `surform-lint` that is not a well-formed
    /// directive — misspelled marker, or a marker naming no codes.
    Malformed { text: String },
    /// The comment precedes no statement — an allow attaches to the
    /// statement that follows it, and at end of file there is none.
    Unattached,
}

/// The SDL-L0023 finding's display key (sorting plus the message's
/// subject when the span is all a renderer shows).
fn allow_problem_key(problem: &LintAllowProblem) -> String {
    match problem {
        LintAllowProblem::UnknownCode(code) => code.clone(),
        LintAllowProblem::Malformed { .. } => "a surform-lint comment".to_string(),
        LintAllowProblem::Unattached => "an allow comment".to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingKind {
    /// A table whose permissions are `full` — SurrealDB grants
    /// everyone, including unauthenticated connections, access to
    /// every record (the shorthand, or groups covering all four
    /// operations with `full` values).
    TablePermissionsFull,
    /// A user carrying a plaintext `password:`.
    UserPlaintextPassword,
    /// A user with no credential — the account cannot sign in.
    UserSecretless,
    /// An `edgengram(min, max)`/`ngram(min, max)` filter with
    /// `min > max` — SurrealQL accepts it and silently degrades.
    AnalyzerMinMax {
        filter: &'static str,
        min: u16,
        max: u16,
    },
    /// A repeated entry in a keyword list (roles, tokenizers,
    /// filters).
    ListDuplicate { list: &'static str, word: String },
    /// A relation without `enforced;` — RELATE accepts nonexistent
    /// endpoints.
    RelationWithoutEnforced,
    /// A top-level field/index/event whose table is declared in the
    /// same scope — nested placement is canonical.
    NonNestedPlacement { family: &'static str },
    /// A param without `value:` — stores `VALUE NONE`.
    ParamValueOmitted,
    /// An api that can never execute an action.
    ApiNoOp {
        /// The empty-`any` catch-all shape (vs the zero-`FOR` body).
        catch_all: bool,
    },
    /// A record-typed field literally named `in`/`out` on a relation
    /// — it rewrites the relation's endpoint.
    RelationEndpointRewrite { field: String },
    /// An access duration side the access's own capability silently
    /// drops (spec/access.md).
    AccessDurationDropped { side: &'static str },
    /// A raw body the pinned embedded parser rejects (rule 8's
    /// advisory).
    RawBodyUnparsable { message: String },
    /// Applying the file defines the literal `[REDACTED]` as real
    /// credential material.
    SentinelApply { what: &'static str },
    /// A table `delete: full;` — INFO elides it, so it cannot
    /// round-trip or drift.
    TableDeleteFullAmbiguous,
    /// A computed field and a field under it — SurrealDB rejects the
    /// second definition at define time.
    ComputedConflict { computed: String, other: String },
    /// `reference;` on a kind outside the reference-shaped set.
    ReferenceKind { kind: String },
    /// An `id` field whose kind is not a record-id key kind.
    IdFieldKind { kind: String },
    /// `flexible;` on a field whose table is not `schemafull;`.
    FlexibleOnSchemalessHost,
    /// `inline;` on a field whose table is not a `type relation`
    /// table (3.3 — the engine demands an existing TYPE RELATION host
    /// at define; the nested form's mirror is the table fold's, this
    /// is the standalone form's).
    InlineOnNonRelationHost,
    /// A table carrying more than 255 `inline;` fields (3.3 — the
    /// adjacency payload's value count is a single byte, the engine's
    /// define-time cap).
    TooManyInlineFields { count: usize },
    /// A mixin no table spreads (spec/mixin.md) — it emits nothing
    /// anywhere, dead weight in the file.
    UnusedMixin,
    /// A later spread's member shadows an earlier spread's same-keyed
    /// member (spec/mixin.md): collisions resolve
    /// last-occurrence-wins, so the file compiles to the LATER
    /// definition — the possibly-unintended class (two third-party
    /// mixins colliding unbeknownst to the author). An explicit
    /// member's overshadow stays unlinted (deliberate — the author
    /// decided it).
    SpreadMemberShadow {
        kind: &'static str,
        key: String,
        earlier: String,
        later: String,
    },
    /// A `// surform-lint-allow:` directive's own problem (SDL-L0023):
    /// an unknown code, a malformed marker, or nothing to attach to.
    /// Never suppressible — a broken allow cannot silence its own
    /// diagnosis.
    LintAllowProblem { problem: LintAllowProblem },
    /// A statically-resolved reference (a table, an analyzer, a
    /// `fn::` path, or a `fn::`-body param read) whose target no
    /// linted file defines (SDL-L0024). Advisory by the flat-catalog
    /// contract: the DDL applies; the target just is not here.
    UndefinedReference {
        kind: RefKind,
        name: String,
        /// The capture site's phrase ("in this table permission").
        site: &'static str,
    },
    /// A file the discovery DFS had already completed that another
    /// file then declares (SDL-L0026, spec/include.md): the second
    /// declaration splices nothing — first inclusion wins — and
    /// self-sufficient module trees produce this systematically, so
    /// it advises rather than errors (the same file declared twice by
    /// ONE file is the SDL-E0014 error class instead).
    DiamondInclude { declarer: String },
}

impl FindingKind {
    /// The literal `SDL-Lxxxx` code, uniform across renderers (public
    /// for tooling that groups findings by code).
    pub fn code_str(&self) -> &'static str {
        match self {
            FindingKind::TablePermissionsFull => "SDL-L0001",
            FindingKind::UserPlaintextPassword => "SDL-L0002",
            FindingKind::UserSecretless => "SDL-L0003",
            FindingKind::AnalyzerMinMax { .. } => "SDL-L0004",
            FindingKind::ListDuplicate { .. } => "SDL-L0005",
            FindingKind::RelationWithoutEnforced => "SDL-L0006",
            FindingKind::NonNestedPlacement { .. } => "SDL-L0007",
            FindingKind::ParamValueOmitted => "SDL-L0008",
            FindingKind::ApiNoOp { .. } => "SDL-L0009",
            FindingKind::RelationEndpointRewrite { .. } => "SDL-L0010",
            FindingKind::AccessDurationDropped { .. } => "SDL-L0011",
            FindingKind::RawBodyUnparsable { .. } => "SDL-L0012",
            FindingKind::SentinelApply { .. } => "SDL-L0013",
            FindingKind::TableDeleteFullAmbiguous => "SDL-L0014",
            FindingKind::ComputedConflict { .. } => "SDL-L0015",
            FindingKind::ReferenceKind { .. } => "SDL-L0016",
            FindingKind::IdFieldKind { .. } => "SDL-L0017",
            FindingKind::FlexibleOnSchemalessHost => "SDL-L0018",
            FindingKind::InlineOnNonRelationHost => "SDL-L0019",
            FindingKind::TooManyInlineFields { .. } => "SDL-L0020",
            FindingKind::UnusedMixin => "SDL-L0021",
            FindingKind::SpreadMemberShadow { .. } => "SDL-L0022",
            FindingKind::LintAllowProblem { .. } => "SDL-L0023",
            FindingKind::UndefinedReference { .. } => "SDL-L0024",
            FindingKind::DiamondInclude { .. } => "SDL-L0026",
        }
    }

    /// The spec file this code is licensed by.
    fn url_str(&self) -> &'static str {
        match self {
            FindingKind::TablePermissionsFull
            | FindingKind::RelationWithoutEnforced
            | FindingKind::TableDeleteFullAmbiguous => "spec/table.md",
            FindingKind::UserPlaintextPassword | FindingKind::UserSecretless => "spec/user.md",
            FindingKind::ListDuplicate { list: "roles", .. } => "spec/user.md",
            FindingKind::AnalyzerMinMax { .. } | FindingKind::ListDuplicate { .. } => {
                "spec/analyzer.md"
            }
            FindingKind::NonNestedPlacement { .. } => "spec/README.md#codegen-output-model",
            FindingKind::ParamValueOmitted => "spec/param.md",
            FindingKind::ApiNoOp { .. } => "spec/api.md",
            FindingKind::RelationEndpointRewrite { .. }
            | FindingKind::ComputedConflict { .. }
            | FindingKind::ReferenceKind { .. }
            | FindingKind::IdFieldKind { .. }
            | FindingKind::FlexibleOnSchemalessHost
            | FindingKind::InlineOnNonRelationHost
            | FindingKind::TooManyInlineFields { .. } => "spec/field.md",
            FindingKind::UnusedMixin | FindingKind::SpreadMemberShadow { .. } => "spec/mixin.md",
            FindingKind::LintAllowProblem { .. } => "spec/README.md#the-linter",
            FindingKind::UndefinedReference { .. } => "spec/README.md#the-linter",
            FindingKind::DiamondInclude { .. } => "spec/include.md",
            FindingKind::AccessDurationDropped { .. } => "spec/access.md",
            FindingKind::SentinelApply {
                what: "user passhash",
            } => "spec/user.md",
            FindingKind::SentinelApply { .. } => "spec/access.md",
            FindingKind::RawBodyUnparsable { .. } => "spec/README.md#design-rules-settled",
        }
    }

    /// One-line fix suggestion.
    fn help_message(&self) -> String {
        match self {
            FindingKind::TablePermissionsFull => {
                "`full` grants everyone — including unauthenticated \
                 connections — access to every record; omit the clause \
                 for the secure default (implicit `none`), or scope \
                 operations in a `permissions` block"
                    .to_string()
            }
            FindingKind::UserPlaintextPassword => {
                "prefer `passhash:` with a pre-computed argon2 hash, or \
                 inject the secret at apply time"
                    .to_string()
            }
            FindingKind::UserSecretless => {
                "add `password:`/`passhash:`, or drop the user from the file \
                 (codegen already defines it `IF NOT EXISTS`)"
                    .to_string()
            }
            FindingKind::AnalyzerMinMax { .. } => {
                "swap the bounds so min ≤ max — SurrealQL accepts any pair and \
                 silently degrades"
                    .to_string()
            }
            FindingKind::ListDuplicate { .. } => {
                "drop the repeats — SurrealQL accepts duplicates (parity) but \
                 they are noise"
                    .to_string()
            }
            FindingKind::RelationWithoutEnforced => {
                "add `enforced;` to make SurrealDB verify both endpoints at \
                 write time"
                    .to_string()
            }
            FindingKind::NonNestedPlacement { .. } => {
                "nest it inside the `table` block; the one exception is a \
                 fulltext index whose analyzer places after its table — the \
                 dependency bound lifts it, and this finding is expected there"
                    .to_string()
            }
            FindingKind::ParamValueOmitted => {
                "add a `value:` clause — a clause-less param stores VALUE NONE".to_string()
            }
            FindingKind::ApiNoOp { .. } => "give the endpoint an action (`then:`), guard it \
                 (`permissions:`), or remove it"
                .to_string(),
            FindingKind::RelationEndpointRewrite { .. } => {
                "spell the endpoints in the relation header \
                 (`table [<from>] -> <name> -> [<to>]`) instead of defining \
                 `in`/`out` fields"
                    .to_string()
            }
            FindingKind::AccessDurationDropped { side } if *side == "grant" => {
                "FOR GRANT survives only on bearer accesses and record \
                 accesses with `refresh;` — drop the side or change the kind"
                    .to_string()
            }
            FindingKind::AccessDurationDropped { .. } => {
                "FOR TOKEN survives on every kind except a jwt access with no \
                 issuer (verify-only) — drop the side or add an issuer"
                    .to_string()
            }
            FindingKind::RawBodyUnparsable { .. } => {
                "rule 8 exists for bodies the pinned parser cannot read \
                 (legacy `VS` scoring, 3.3-only shapes) — verify this is one \
                 of them; anything else should be an SDL statement"
                    .to_string()
            }
            FindingKind::SentinelApply { .. } => {
                "provision the real secret out-of-band — SDL is shape-only \
                 here, and re-applying redacted INFO output re-defines the \
                 sentinel as the credential"
                    .to_string()
            }
            FindingKind::TableDeleteFullAmbiguous => {
                "the clause still applies on execution; INFO elides \
                 `FOR delete FULL`, so the differ converges silently — verify \
                 the database's actual delete permission if it matters"
                    .to_string()
            }
            FindingKind::ComputedConflict { .. } => {
                "give the subfield its own top-level name, or drop the \
                 computed parent — SurrealDB rejects the pair at define time"
                    .to_string()
            }
            FindingKind::ReferenceKind { .. } => {
                "reference-shaped kinds are record kinds, unions and \
                 `option` over them, `array`/`set` of them, and literal \
                 arrays of them"
                    .to_string()
            }
            FindingKind::IdFieldKind { .. } => {
                "record-id key kinds are `any`, `int`, `number`, `string`, \
                 `uuid`, `array`, `object`, and int/string/array/object \
                 literals"
                    .to_string()
            }
            FindingKind::FlexibleOnSchemalessHost => {
                "add `schemafull;` to the table — SurrealDB rejects FLEXIBLE \
                 anywhere else"
                    .to_string()
            }
            FindingKind::InlineOnNonRelationHost => {
                "INLINE embeds into the edge-adjacency payloads — move the field to a \
                 `type relation` table, or drop `inline;`"
                    .to_string()
            }
            FindingKind::TooManyInlineFields { .. } => {
                "un-inline fields until the table carries at most 255 — the engine \
                 rejects the overflow at define"
                    .to_string()
            }
            FindingKind::UnusedMixin => {
                "spread it into a table (`...name;`) or delete it — an unspread \
                 mixin defines nothing"
                    .to_string()
            }
            FindingKind::SpreadMemberShadow { kind, .. } => {
                format!(
                    "the later spread's {kind} wholly wins at expansion — rename one \
                     member, or add an explicit {kind} on the table to decide"
                )
            }
            FindingKind::LintAllowProblem {
                problem: LintAllowProblem::UnknownCode(_),
            } => format!(
                "spell a registered code exactly, comma-separating several — \
                 the registry is {} through {}",
                LINT_CODES.first().copied().unwrap_or_default(),
                LINT_CODES.last().copied().unwrap_or_default(),
            ),
            FindingKind::LintAllowProblem {
                problem: LintAllowProblem::Malformed { .. },
            } => "the directive is `// surform-lint-allow: SDL-L0001, SDL-L0006` — \
                 the marker lowercase-exact, codes after the colon"
                .to_string(),
            FindingKind::LintAllowProblem {
                problem: LintAllowProblem::Unattached,
            } => "move the comment directly above the statement it excuses — an \
                 allow attaches to the statement that follows it"
                .to_string(),
            FindingKind::UndefinedReference {
                kind: RefKind::Param,
                ..
            } => "add it to the argument list, add a `param` definition, or \
                 read one of the bound names — a `fn::` body cannot see \
                 client-bound variables (the engine isolates the body's \
                 context)"
                .to_string(),
            FindingKind::UndefinedReference { .. } => {
                "define the target, check the spelling, or excuse the statement \
                 with `// surform-lint-allow: SDL-L0024` — a target outside the \
                 linted files is legitimate (SurrealDB's catalog is flat)"
                    .to_string()
            }
            FindingKind::DiamondInclude { .. } => {
                "this include splices nothing — first inclusion wins; keep it if \
                 the file is deliberately self-sufficient, or excuse it with \
                 `// surform-lint-allow: SDL-L0026` above the include"
                    .to_string()
            }
        }
    }

    /// The label's text (the finding's own Display carries the
    /// identity, so the label stays positional).
    fn label_text(&self) -> String {
        match self {
            FindingKind::TablePermissionsFull => "`permissions: full` on this table".to_string(),
            FindingKind::UserPlaintextPassword => "plaintext `password:`".to_string(),
            FindingKind::UserSecretless => "no credential".to_string(),
            FindingKind::AnalyzerMinMax { .. } => "`min` exceeds `max`".to_string(),
            FindingKind::ListDuplicate { word, .. } => format!("`{word}` repeats"),
            FindingKind::RelationWithoutEnforced => "not `enforced;`".to_string(),
            FindingKind::NonNestedPlacement { .. } => "standalone definition".to_string(),
            FindingKind::ParamValueOmitted => "no `value:`".to_string(),
            FindingKind::ApiNoOp { catch_all: false } => "no action can run".to_string(),
            FindingKind::ApiNoOp { .. } => "the catch-all runs nothing".to_string(),
            FindingKind::RelationEndpointRewrite { field, .. } => {
                format!("`{field}` rewrites the endpoint")
            }
            FindingKind::AccessDurationDropped { side } => {
                format!("`duration {side}:` is silently dropped")
            }
            FindingKind::RawBodyUnparsable { .. } => "the parser rejects this body".to_string(),
            FindingKind::SentinelApply { .. } => "sentinel key material".to_string(),
            FindingKind::TableDeleteFullAmbiguous => {
                "`delete: full;` cannot round-trip".to_string()
            }
            FindingKind::ComputedConflict { .. } => "the computed parent".to_string(),
            FindingKind::ReferenceKind { .. } => "`reference;` on this kind".to_string(),
            FindingKind::IdFieldKind { .. } => "the `id` field's kind".to_string(),
            FindingKind::FlexibleOnSchemalessHost => "`flexible;` here".to_string(),
            FindingKind::InlineOnNonRelationHost => "`inline;` here".to_string(),
            FindingKind::TooManyInlineFields { .. } => "the INLINE field count".to_string(),
            FindingKind::UnusedMixin => "this mixin".to_string(),
            FindingKind::SpreadMemberShadow { .. } => "the shadowing spread".to_string(),
            FindingKind::LintAllowProblem {
                problem: LintAllowProblem::UnknownCode(_),
            } => "unknown lint code".to_string(),
            FindingKind::LintAllowProblem {
                problem: LintAllowProblem::Malformed { .. },
            } => "not a well-formed directive".to_string(),
            FindingKind::LintAllowProblem {
                problem: LintAllowProblem::Unattached,
            } => "no statement follows".to_string(),
            FindingKind::UndefinedReference { name, .. } => {
                format!("`{name}` is not defined")
            }
            FindingKind::DiamondInclude { .. } => {
                "already included — first inclusion wins".to_string()
            }
        }
    }
}

fn scope_text(scope: &ScopeKey) -> String {
    match scope {
        ScopeKey::Root => "the root scope".to_string(),
        ScopeKey::Namespace(ns) => format!("namespace `{ns}`"),
        ScopeKey::Database(ns, db) => format!("database `{ns}/{db}`"),
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let where_ = scope_text(&self.scope);
        match &self.kind {
            FindingKind::TablePermissionsFull => write!(
                f,
                "table `{key}` in {where_}: permissions are `full` — anyone, \
                 even unauthenticated users, can access every record",
                key = self.key
            ),
            FindingKind::UserPlaintextPassword => write!(
                f,
                "user `{key}` in {where_} carries a plaintext `password:` — the \
                 literal rides in the file and in any emitted DDL",
                key = self.key
            ),
            FindingKind::UserSecretless => write!(
                f,
                "user `{key}` in {where_} has no credential — the account \
                 cannot sign in (codegen defines it `IF NOT EXISTS`, so an \
                 existing credential is kept but never converges)",
                key = self.key
            ),
            FindingKind::AnalyzerMinMax { filter, min, max } => write!(
                f,
                "analyzer `{key}`: `{filter}({min}, {max})` has min > max — \
                 SurrealDB accepts it and silently degrades",
                key = self.key
            ),
            FindingKind::ListDuplicate { list, word } => write!(
                f,
                "`{list}` on `{key}` in {where_} repeats `{word}`",
                key = self.key
            ),
            FindingKind::RelationWithoutEnforced => write!(
                f,
                "relation `{key}` in {where_} is not `enforced;` — RELATE accepts \
                 endpoints that do not exist",
                key = self.key
            ),
            FindingKind::NonNestedPlacement { family } => write!(
                f,
                "{family} `{key}` sits at top level while its table is declared \
                 in {where_} — nested placement is canonical",
                key = self.key
            ),
            FindingKind::ParamValueOmitted => write!(
                f,
                "param `{key}` in {where_} has no `value:` — SurrealDB stores \
                 VALUE NONE, almost certainly unintended",
                key = self.key
            ),
            FindingKind::ApiNoOp { catch_all: true } => write!(
                f,
                "api `{key}` in {where_} has no effective `any` catch-all — the \
                 engine's implicit one matches every unmatched method under FULL \
                 permissions and runs nothing",
                key = self.key
            ),
            FindingKind::ApiNoOp { .. } => write!(
                f,
                "api `{key}` in {where_} has no method groups and no `any` block \
                 — every request matches and nothing runs",
                key = self.key
            ),
            FindingKind::RelationEndpointRewrite { field } => write!(
                f,
                "field `{field}` on relation `{key}` in {where_} rewrites the \
                 relation's `{field}` endpoint — a redefinition of the table's \
                 endpoints, not a plain field",
                key = self.key
            ),
            FindingKind::AccessDurationDropped { side } => write!(
                f,
                "access `{key}` in {where_} declares `duration {side}: …;` but its \
                 own kind cannot use it — SurrealDB silently drops the side (it \
                 disappears from INFO)",
                key = self.key
            ),
            FindingKind::RawBodyUnparsable { message } => write!(
                f,
                "the pinned 3.3.0 parser rejects this `raw` statement in \
                 {where_}: {message}",
            ),
            FindingKind::SentinelApply { what } => write!(
                f,
                "applying `{key}` in {where_} defines the literal string \
                 `[REDACTED]` as the {what} — garbage credential material",
                key = self.key
            ),
            FindingKind::TableDeleteFullAmbiguous => write!(
                f,
                "table `{key}` in {where_} carries `delete: full;` — INFO elides \
                 `FOR delete FULL`, so the clause cannot round-trip or drift",
                key = self.key
            ),
            FindingKind::ComputedConflict { computed, other } => write!(
                f,
                "computed field `{computed}` and `{other}` collide on `{key}` \
                 in {where_} — SurrealDB rejects the second definition at \
                 define time",
                key = self.key
            ),
            FindingKind::ReferenceKind { kind } => write!(
                f,
                "field `{key}` in {where_}: `reference;` needs a record-shaped \
                 type — `{kind}` is not one, and SurrealDB rejects it at define",
                key = self.key
            ),
            FindingKind::IdFieldKind { kind } => write!(
                f,
                "field `{key}` in {where_}: the `id` field's type must be a \
                 record-id key kind — `{kind}` is not, and SurrealDB rejects it \
                 at define",
                key = self.key
            ),
            FindingKind::FlexibleOnSchemalessHost => write!(
                f,
                "field `{key}` in {where_} is `flexible;` but its table is not \
                 `schemafull;` — SurrealDB rejects it at define",
                key = self.key
            ),
            FindingKind::InlineOnNonRelationHost => write!(
                f,
                "field `{key}` in {where_} is `inline;` but its table is not a \
                 `type relation` table — SurrealDB rejects INLINE fields at define",
                key = self.key
            ),
            FindingKind::TooManyInlineFields { count } => write!(
                f,
                "table `{key}` in {where_} carries {count} INLINE fields — the \
                 adjacency payload's value count is a single byte, capping a \
                 table at 255",
                key = self.key
            ),
            FindingKind::UnusedMixin => write!(
                f,
                "mixin `{key}` in {where_} is never spread — it emits nothing \
                 anywhere",
                key = self.key
            ),
            FindingKind::SpreadMemberShadow {
                kind,
                key: member,
                earlier,
                later,
            } => write!(
                f,
                "table `{key}` in {where_}: the `{later}` spread's {kind} \
                 `{member}` shadows `{earlier}`'s — the later spread wins at \
                 expansion",
                key = self.key
            ),
            FindingKind::LintAllowProblem {
                problem: LintAllowProblem::UnknownCode(code),
            } => write!(
                f,
                "this allow comment names unknown lint code `{code}` — not in \
                 the registry ({} through {}), so it suppresses nothing",
                LINT_CODES.first().copied().unwrap_or_default(),
                LINT_CODES.last().copied().unwrap_or_default(),
            ),
            FindingKind::LintAllowProblem {
                problem: LintAllowProblem::Malformed { text },
            } => write!(
                f,
                "this `surform-lint…` comment is not a well-formed allow \
                 directive (`{text}`) — it suppresses nothing",
            ),
            FindingKind::LintAllowProblem {
                problem: LintAllowProblem::Unattached,
            } => write!(
                f,
                "this allow comment precedes no statement — an allow attaches \
                 to the statement that follows it, and none does",
            ),
            FindingKind::UndefinedReference {
                kind: RefKind::Param,
                name,
                site,
            } => write!(
                f,
                "param `${name}` is read {site} in `{key}` in {where_} but is \
                 none of the function's arguments and no `param` defines it — \
                 the read evaluates to none on every call (a `fn::` body binds \
                 its declared arguments, the session params, the catalog's \
                 params, and the record trio on a document-bound call)",
                key = self.key
            ),
            FindingKind::UndefinedReference { kind, name, site } => write!(
                f,
                "{kind_word} `{name}` is referenced {site} in `{key}` in \
                 {where_} but no linted file defines it",
                kind_word = kind_word(*kind),
                key = self.key
            ),
            FindingKind::DiamondInclude { declarer } => write!(
                f,
                "`{key}` is already included by `{declarer}` — first inclusion \
                 wins, so this declaration splices nothing",
                key = self.key
            ),
        }
    }
}

// The derive cannot express the literal `SDL-Lxxxx` spelling (hyphens
// are not path syntax), so `Diagnostic` is implemented by hand —
// `Diag`'s pattern.
impl miette::Diagnostic for Finding {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.kind.code_str()))
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.kind.url_str()))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.kind.help_message()))
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        self.span.map(|span| {
            Box::new(std::iter::once(miette::LabeledSpan::new(
                Some(self.kind.label_text()),
                span.offset,
                span.len,
            ))) as Box<dyn Iterator<Item = _> + '_>
        })
    }

    fn severity(&self) -> Option<miette::Severity> {
        // Findings are advisories — never diagnostics. The exit-1
        // convention is the CLI's, not a severity claim.
        Some(miette::Severity::Warning)
    }
}

impl std::error::Error for Finding {}

// ---------------------------------------------------------------------
// The registry pin and the directive reader's own legs
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// One value of every `FindingKind` variant. The count pin below
    /// ties this list to `LINT_CODES` one-for-one, and
    /// [`touches_every_variant`]'s wildcard-free match forces a new
    /// variant to touch THIS test — the residual escape (sample and
    /// registry both skipped in lockstep) is named there.
    fn every_kind() -> Vec<FindingKind> {
        vec![
            FindingKind::TablePermissionsFull,
            FindingKind::UserPlaintextPassword,
            FindingKind::UserSecretless,
            FindingKind::AnalyzerMinMax {
                filter: "ngram",
                min: 2,
                max: 1,
            },
            FindingKind::ListDuplicate {
                list: "roles",
                word: "editor".to_string(),
            },
            FindingKind::RelationWithoutEnforced,
            FindingKind::NonNestedPlacement { family: "field" },
            FindingKind::ParamValueOmitted,
            FindingKind::ApiNoOp { catch_all: true },
            FindingKind::RelationEndpointRewrite {
                field: "in".to_string(),
            },
            FindingKind::AccessDurationDropped { side: "grant" },
            FindingKind::RawBodyUnparsable {
                message: String::new(),
            },
            FindingKind::SentinelApply {
                what: "user passhash",
            },
            FindingKind::TableDeleteFullAmbiguous,
            FindingKind::ComputedConflict {
                computed: "c".to_string(),
                other: "c.x".to_string(),
            },
            FindingKind::ReferenceKind {
                kind: "string".to_string(),
            },
            FindingKind::IdFieldKind {
                kind: "float".to_string(),
            },
            FindingKind::FlexibleOnSchemalessHost,
            FindingKind::InlineOnNonRelationHost,
            FindingKind::TooManyInlineFields { count: 256 },
            FindingKind::UnusedMixin,
            FindingKind::SpreadMemberShadow {
                kind: "field",
                key: "f".to_string(),
                earlier: "a".to_string(),
                later: "b".to_string(),
            },
            FindingKind::LintAllowProblem {
                problem: LintAllowProblem::Unattached,
            },
            FindingKind::UndefinedReference {
                kind: RefKind::Table,
                name: "ghost".to_string(),
                site: "in this table permission",
            },
            FindingKind::DiamondInclude {
                declarer: "root.surdl".to_string(),
            },
        ]
    }

    /// The compiler-forced enumeration: a new `FindingKind` variant
    /// fails THIS match (E0004 non-exhaustive). That forces the
    /// pattern here — the SAMPLE list and the REGISTRY still ride on
    /// the count/membership asserts below (a variant whose sample and
    /// code are both skipped in lockstep escapes compilation; the
    /// registry tables in spec/README.md and docs/content/lint.md are
    /// what make that drift visible).
    fn touches_every_variant(kind: &FindingKind) {
        match kind {
            FindingKind::TablePermissionsFull
            | FindingKind::UserPlaintextPassword
            | FindingKind::UserSecretless
            | FindingKind::AnalyzerMinMax { .. }
            | FindingKind::ListDuplicate { .. }
            | FindingKind::RelationWithoutEnforced
            | FindingKind::NonNestedPlacement { .. }
            | FindingKind::ParamValueOmitted
            | FindingKind::ApiNoOp { .. }
            | FindingKind::RelationEndpointRewrite { .. }
            | FindingKind::AccessDurationDropped { .. }
            | FindingKind::RawBodyUnparsable { .. }
            | FindingKind::SentinelApply { .. }
            | FindingKind::TableDeleteFullAmbiguous
            | FindingKind::ComputedConflict { .. }
            | FindingKind::ReferenceKind { .. }
            | FindingKind::IdFieldKind { .. }
            | FindingKind::FlexibleOnSchemalessHost
            | FindingKind::InlineOnNonRelationHost
            | FindingKind::TooManyInlineFields { .. }
            | FindingKind::UnusedMixin
            | FindingKind::SpreadMemberShadow { .. }
            | FindingKind::LintAllowProblem { .. }
            | FindingKind::UndefinedReference { .. }
            | FindingKind::DiamondInclude { .. } => {}
        }
    }

    #[test]
    fn registry_holds_every_kind_and_vice_versa() {
        let kinds = every_kind();
        assert_eq!(
            kinds.len(),
            LINT_CODES.len(),
            "the sample list and the registry disagree — a new kind \
             landed without its code (or a sample went stale)"
        );
        // The wildcard-free match is what forces a new variant into
        // `every_kind` at all: without it, variant and registry could
        // both miss the count in step and pass.
        for kind in &kinds {
            touches_every_variant(kind);
        }
        let mut seen = BTreeSet::new();
        for kind in &kinds {
            let code = kind.code_str();
            assert!(
                is_known_code(code),
                "{code} is not in LINT_CODES — allow comments would \
                 reject it as unknown"
            );
            assert!(seen.insert(code), "{code} is claimed twice");
        }
    }

    #[test]
    fn allow_directive_reads_codes() {
        let directive = parse_allow(" surform-lint-allow: SDL-L0001").unwrap();
        assert!(directive.problems.is_empty());
        assert_eq!(directive.codes, BTreeSet::from(["SDL-L0001"]));
        // Comma list, stray spacing, empty segments, repeats.
        let directive =
            parse_allow("surform-lint-allow: SDL-L0006 ,SDL-L0001,, SDL-L0006").unwrap();
        assert!(directive.problems.is_empty());
        assert_eq!(directive.codes, BTreeSet::from(["SDL-L0001", "SDL-L0006"]));
        // A repeated unknown code warns once (the set discipline).
        let directive = parse_allow("surform-lint-allow: SDL-L0099, SDL-L0099").unwrap();
        assert!(directive.codes.is_empty());
        assert_eq!(directive.problems.len(), 1);
    }

    #[test]
    fn allow_directive_flags_its_own_problems() {
        // Unknown codes never join the suppressor set.
        let directive = parse_allow("surform-lint-allow: SDL-L0001, SDL-L0099").unwrap();
        assert_eq!(directive.codes, BTreeSet::from(["SDL-L0001"]));
        assert_eq!(
            directive.problems,
            [LintAllowProblem::UnknownCode("SDL-L0099".to_string())]
        );
        // No codes at all, a missing colon, a case typo: malformed,
        // never markers.
        for text in [
            " surform-lint-allow:",
            " surform-lint-allow SDL-L0001",
            " Surform-Lint-Allow: SDL-L0001",
            " surform-lint-alow: SDL-L0001",
        ] {
            let directive = parse_allow(text).unwrap();
            assert!(directive.codes.is_empty(), "{text} named codes");
            assert!(
                matches!(directive.problems[..], [LintAllowProblem::Malformed { .. }]),
                "{text} was not flagged malformed"
            );
        }
        // Ordinary prose (even mentioning the tool mid-sentence) and
        // error codes are not directives.
        assert!(parse_allow(" surform lints this table").is_none());
        assert!(parse_allow(" TODO: allow SDL-E1009 here").is_none());
    }
}
