//! The composition loader (spec/include.md) — the read half of
//! `include`: walks the file graph from the root the CLI was given,
//! splices every included file's statements at the include's
//! position (depth-first, pre-order), and hands the consumers the
//! FLATTENED document plus the file map that attributes each
//! statement to its home.
//!
//! The invariant the whole loader hangs on: **flatten(root) ≡ root** —
//! the flattened document compiles, lints, and diffs identically to
//! the same statements in one file. Nothing here reorders: the splice
//! only extends the stream, so every positional rule (a `database`
//! binding to its nearest preceding `namespace`) reads the flattened
//! stream exactly as it reads a single file's. Spread resolution is
//! not positional since the declarative namespace: a mixin spread
//! resolves against the composed document's GLOBAL mixin set (the
//! loader runs [`crate::parser::resolve_spreads_homed`] over the
//! flattened stream as its last pass — a typo'd spread fails the
//! load, which is why the committal `fmt --in-place` reports it;
//! fmt's print path never loads, so it never runs the pass).
//!
//! The walk runs in three passes:
//!
//! 1. **Discovery** — the include graph, by parsing each file ONCE
//!    (the parse is the semantic parse: spreads parse as opaque
//!    names, so no priming exists to order): DFS pre-order, and the
//!    path-level diagnostics (missing file SDL-E0008, absolute path
//!    SDL-E0012, the same file included twice by one file SDL-E0014).
//!    A file splices ONCE: a reach of an already-completed file is a
//!    diamond no-op carrying the SDL-L0026 advisory; a reach of an
//!    IN-PROGRESS file (the stack) is a cycle back-edge and legal —
//!    the include graph declares participation, not an elaboration
//!    order, and splice-once makes every cycle terminate.
//! 2. **Splice** — the parsed items concatenate in discovery order:
//!    runs of each file's own statements between its includes, the
//!    flattened document plus the file map (homes, per-statement
//!    home tags, per-home scan blocks for the composed lint).
//! 3. **Resolution** — the composition's semantics over the flattened
//!    document: the global spread pass (SDL-E1061 unresolved, plus
//!    the two landed-member validations the resolution owns), then
//!    the order pass ([`crate::order`]) — duplicate definitions
//!    (SDL-E1062, every family) and true define-time cycles
//!    (SDL-E0015).
//!
//! Resolution is relative to the including file's directory,
//! canonicalized. Any diagnostic fails the whole load — the consumers
//! never see a partial composition.
//!
//! Comments move with the splice (rule 5's anchor classes, inert for
//! every current consumer but kept for the flattened document's
//! fidelity): an include's leading comments go above the first
//! spliced statement (or, when nothing splices, above the next
//! statement in its own file), its same-line note after the last
//! spliced one, and an included file's file-tail comments follow its
//! last spliced statement.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::{ClauseComment, Include, SdlFile, Stmt};
use crate::diag::Diag;
use crate::lint::{self, ComposePart, DiamondInclude, SourceScan};
use crate::parser;

/// One participating file: its canonical path, the spelling
/// diagnostics name (relative to the root's directory when one
/// exists), and its text.
#[derive(Debug)]
pub struct Home {
    pub path: PathBuf,
    pub display: String,
    pub text: String,
}

/// A loaded composition: the flattened document and its file map.
/// `item_homes` runs parallel to `file.items`; `item_positions` runs
/// parallel too — each statement's position in its home's authored
/// item list (includes included, so the write-back can rebuild every
/// file around its `include` statements — [`Self::includes_by_home`]).
/// The blocks are the contiguous per-home statement runs in splice
/// order — the composed lint's input ([`Loaded::lint_parts`]).
/// `home_files` runs parallel to `homes` — each home's OWN parsed
/// file (the fmt tree writer formats these). `diamonds` are the
/// discovery's SDL-L0026 facts — one per no-op declaration the
/// composed lint renders.
#[derive(Debug)]
pub struct Loaded {
    pub file: SdlFile,
    pub homes: Vec<Home>,
    pub home_files: Vec<SdlFile>,
    pub item_homes: Vec<usize>,
    pub item_positions: Vec<usize>,
    pub includes_by_home: Vec<Vec<(usize, Include)>>,
    pub diamonds: Vec<DiamondInclude>,
    scans: Vec<SourceScan>,
    blocks: Vec<Block>,
}

/// One contiguous run of one home's statements in the flattened
/// stream: the home-local range into the home's scan (one scan entry
/// per statement, includes included).
#[derive(Debug)]
struct Block {
    home: usize,
    statements: std::ops::Range<usize>,
}

impl Loaded {
    /// The composed lint's parts: one per block, in splice order,
    /// each carrying its home's scan and the block's statement range.
    /// (Directive problems need no tail flag — they render once per
    /// home from the whole scan.)
    pub fn lint_parts(&self) -> Vec<ComposePart<'_>> {
        self.blocks
            .iter()
            .map(|block| ComposePart {
                scan: &self.scans[block.home],
                home: block.home,
                statements: block.statements.clone(),
            })
            .collect()
    }
}

/// A failed load: every diagnostic collected across the tree, each
/// tagged with the home whose text its span renders against.
#[derive(Debug)]
pub struct LoadFailure {
    pub homes: Vec<Home>,
    pub errors: Vec<(Diag, usize)>,
}

/// Loads the composition rooted at `root` (already read: the CLI's
/// read diagnoses its own failure). `display` is the spelling the
/// root's own diagnostics name — the CLI passes the user's path.
pub fn load(root: &Path, display: &str, text: String) -> Result<Loaded, LoadFailure> {
    // The root canonicalizes like every other file: the back-edge
    // check and the diamond dedupe compare canonical paths, and a
    // child including the root back must find the root on the stack.
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut loader = Loader {
        homes: Vec::new(),
        scans: Vec::new(),
        items: Vec::new(),
        item_homes: Vec::new(),
        item_positions: Vec::new(),
        includes_by_home: Vec::new(),
        trailing_comments: Vec::new(),
        blocks: Vec::new(),
        errors: Vec::new(),
        parses: HashMap::new(),
        done: HashMap::new(),
        declared: Vec::new(),
        diamonds: Vec::new(),
        stack: Vec::new(),
        root_dir: root
            .parent()
            .map(|dir| dir.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".")),
    };
    // Pass 1: the include graph (each file parsed in place).
    let tree = match loader.discover(&root, display.to_string(), text) {
        Some(tree) => tree,
        None => {
            return Err(LoadFailure {
                homes: loader.homes,
                errors: loader.errors,
            });
        }
    };
    if !loader.errors.is_empty() {
        return Err(LoadFailure {
            homes: loader.homes,
            errors: loader.errors,
        });
    }
    // Every home's parse rides along with the composition (the fmt
    // tree writer formats these; discovery parsed each home exactly
    // once). Cloned before the splice, which consumes the parses.
    let home_files: Vec<SdlFile> = (0..loader.homes.len())
        .map(|home| {
            loader
                .parses
                .get(&home)
                .expect("discovery parses every home")
                .clone()
        })
        .collect();
    // Pass 2: the splice.
    let mut parses = std::mem::take(&mut loader.parses);
    loader.splice(&tree, &mut parses);
    // Pass 3: the composition's check-class semantics in one call
    // ([`crate::order::check_document`]) — the global spread
    // resolution, the duplicate definitions (SDL-E1062), the true
    // cycles (SDL-E0015). Anchors on the per-statement spans the
    // home scans carry (each scan entry is the home's authored
    // statement at the stream item's authored position).
    let item_spans: Vec<crate::lexer::Span> = loader
        .items
        .iter()
        .enumerate()
        .map(|(at, _)| {
            let home = loader.item_homes[at];
            let position = loader.item_positions[at];
            loader.scans[home].statements[position].span
        })
        .collect();
    loader.errors.extend(crate::order::check_document(
        &loader.items,
        &loader.item_homes,
        &item_spans,
    ));
    if !loader.errors.is_empty() {
        return Err(LoadFailure {
            homes: loader.homes,
            errors: loader.errors,
        });
    }
    Ok(Loaded {
        file: SdlFile {
            items: loader.items,
            trailing_comments: loader.trailing_comments,
        },
        homes: loader.homes,
        home_files,
        item_homes: loader.item_homes,
        item_positions: loader.item_positions,
        includes_by_home: loader.includes_by_home,
        diamonds: loader.diamonds,
        scans: loader.scans,
        blocks: loader.blocks,
    })
}

/// The discovery pass's product: one node per participating file, in
/// splice order by construction. The node's parse lives in the
/// loader's `parses` map (discovery parsed each file exactly once —
/// its parse IS the semantic parse, spreads riding as opaque names);
/// the splice drains the map.
struct Discovered {
    home: usize,
    /// This file's include statements that resolved to a splice:
    /// (statement position in this file's item list, the child).
    includes: Vec<(usize, Discovered)>,
}

struct Loader {
    homes: Vec<Home>,
    scans: Vec<SourceScan>,
    items: Vec<Stmt>,
    item_homes: Vec<usize>,
    item_positions: Vec<usize>,
    /// Each home's authored `include` statements — (authored item
    /// position, the statement) in order, no-op diamonds included.
    /// An include is structural: the write-back rebuilds every file
    /// around these, and the re-splice order rides them.
    includes_by_home: Vec<Vec<(usize, Include)>>,
    /// The root's own file-tail comments (rule 5) — an included
    /// file's tail follows its last spliced statement instead.
    trailing_comments: Vec<String>,
    blocks: Vec<Block>,
    errors: Vec<(Diag, usize)>,
    /// Each home's parse, stored by discovery (the splice drains it).
    parses: HashMap<usize, SdlFile>,
    /// Canonical paths already discovered → the home that declared
    /// them first (the diamond advisory names it).
    done: HashMap<PathBuf, usize>,
    /// Per declarer home, the canonical paths it has already
    /// declared: the same file included twice by ONE file is the
    /// SDL-E0014 error — whoever composed it first.
    declared: Vec<std::collections::HashSet<PathBuf>>,
    /// The diamond discoveries (spec/include.md): a no-op declaration
    /// of an already-completed file, anchored where it splices
    /// nothing.
    diamonds: Vec<DiamondInclude>,
    /// The active DFS chain, canonical paths from the root down. A
    /// reach of any path on it is a cycle back-edge — legal.
    stack: Vec<PathBuf>,
    root_dir: PathBuf,
}

impl Loader {
    /// Registers a new home; returns its index.
    fn enter(&mut self, path: PathBuf, display: String, text: String) -> usize {
        let home = self.homes.len();
        self.homes.push(Home {
            path,
            display,
            text,
        });
        self.scans.push(SourceScan {
            statements: Vec::new(),
            tail_problems: Vec::new(),
        });
        self.includes_by_home.push(Vec::new());
        self.declared.push(std::collections::HashSet::new());
        home
    }

    /// The include-graph DFS. Returns `None` when any diagnostic
    /// during the walk — path-level (missing, absolute, duplicate) or
    /// a file's own parse, whose diags discovery collects — makes the
    /// tree unspliceable.
    fn discover(&mut self, root: &Path, display: String, text: String) -> Option<Discovered> {
        // The root enters before any include exists to resolve — its
        // path is the caller's, its display the user's spelling.
        let root_home = self.enter(root.to_path_buf(), display, text);
        self.stack.push(root.to_path_buf());
        let tree = self.discover_file(root_home);
        self.stack.pop();
        if self.errors.is_empty() {
            Some(tree)
        } else {
            None
        }
    }

    /// One file's discovery: its ONE parse (composable for the root;
    /// the included entry for everything else — the scope ban is the
    /// included file's own rule), stored for the splice; then its
    /// children in statement order.
    fn discover_file(&mut self, home: usize) -> Discovered {
        let (file, diags) = if home == 0 {
            parser::parse_file_composable(&self.homes[home].text)
        } else {
            parser::parse_file_included(&self.homes[home].text)
        };
        self.errors
            .extend(diags.into_iter().map(|diag| (diag, home)));
        // The include statements the children discover from (cloned
        // with their authored positions — the parse itself waits in
        // the map for the splice, and the splice re-matches the
        // children at these positions).
        let includes: Vec<(usize, Include)> = file
            .items
            .iter()
            .enumerate()
            .filter_map(|(position, stmt)| match stmt {
                Stmt::Include(include) => Some((position, include.clone())),
                _ => None,
            })
            .collect();
        self.parses.insert(home, file);
        let mut node = Discovered {
            home,
            includes: Vec::new(),
        };
        for (position, include) in &includes {
            if let Some(child) = self.discover_include(home, include) {
                node.includes.push((*position, child));
            }
        }
        node
    }

    /// Resolves one include statement from `home` and discovers the
    /// target (or records why not). `None` when the include resolves
    /// to nothing spliceable — missing, unreadable, absolute, a cycle
    /// back-edge (legal), the duplicate declaration (SDL-E0014), or
    /// the diamond no-op (advised at lint).
    fn discover_include(&mut self, parent: usize, include: &Include) -> Option<Discovered> {
        if Path::new(&include.path).is_absolute() {
            self.errors.push((
                Diag::IncludePathAbsolute {
                    span: include.span,
                    path: include.path.clone(),
                },
                parent,
            ));
            return None;
        }
        let joined = self.homes[parent]
            .path
            .parent()
            .map(|dir| dir.join(&include.path))
            .unwrap_or_else(|| PathBuf::from(&include.path));
        let canonical = match joined.canonicalize() {
            Ok(canonical) => canonical,
            Err(reason) => {
                self.errors.push((
                    Diag::FileUnreadable {
                        path: include.path.clone(),
                        reason: reason.to_string(),
                        span: Some(include.span),
                    },
                    parent,
                ));
                return None;
            }
        };
        if self.stack.contains(&canonical) {
            // Cycle back-edge: the include reaches a file still being
            // discovered (an ancestor, or the file itself). Legal —
            // the include graph declares participation, not an
            // elaboration order, and splice-once makes every cycle
            // terminate. The file spliced at its first declaration;
            // this statement splices nothing.
            return None;
        }
        // The duplicate-include rule precedes the diamond
        // classification: ONE file declaring the same path twice is
        // the SDL-E0014 hard error, whoever composed it first — the
        // copy-paste smell must not hide behind another participant's
        // earlier declaration.
        if !self.declared[parent].insert(canonical.clone()) {
            let first = self.done.get(&canonical).copied().unwrap_or(parent);
            self.errors.push((
                Diag::DuplicateInclude {
                    span: include.span,
                    path: include.path.clone(),
                    first: self.homes[first].display.clone(),
                },
                parent,
            ));
            return None;
        }
        if let Some(&first) = self.done.get(&canonical) {
            // Diamond: the file the DFS completed splices at its
            // first declaration; this declaration is a no-op, and
            // self-sufficient module trees produce it systematically
            // — advisory, opt-out by the allow comment (SDL-L0026).
            self.diamonds.push(DiamondInclude {
                home: parent,
                span: include.span,
                path: include.path.clone(),
                declarer: self.homes[first].display.clone(),
            });
            return None;
        }
        let text = match std::fs::read_to_string(&joined) {
            Ok(text) => text,
            Err(reason) => {
                self.errors.push((
                    Diag::FileUnreadable {
                        path: include.path.clone(),
                        reason: reason.to_string(),
                        span: Some(include.span),
                    },
                    parent,
                ));
                return None;
            }
        };
        let display = canonical
            .strip_prefix(&self.root_dir)
            .map(|rel| rel.to_string_lossy().into_owned())
            .unwrap_or_else(|_| canonical.to_string_lossy().into_owned());
        let child = self.enter(canonical.clone(), display, text);
        self.done.insert(canonical.clone(), parent);
        self.stack.push(canonical);
        let node = self.discover_file(child);
        self.stack.pop();
        Some(node)
    }

    /// Pass 2: splices the parsed statements in discovery order.
    fn splice(&mut self, tree: &Discovered, parsed: &mut HashMap<usize, SdlFile>) {
        let file = parsed.remove(&tree.home).expect("discovery parsed it");
        let home = tree.home;
        self.scans[home] = lint::scan_source(&self.homes[home].text);
        // The includes' anchored comments, read before the item list
        // consumes (the loop owns the statements by value).
        //
        // The current run's bookkeeping, both in statement POSITIONS:
        // `run_open` opens the run (a range into the home's scan,
        // whose entries are the file's statements — includes
        // included); `consumed` counts pushed items (the stream
        // contributions), for the empty-file block check.
        let mut run_open = 0usize;
        let mut consumed = 0usize;
        let total_positions = file.items.len();
        let mut includes = tree.includes.iter().peekable();
        // Leading comments still awaiting a statement below them — a
        // no-op include's comments lead whatever comes next in this
        // file, exactly as they led the include.
        let mut forward_comments: Vec<String> = Vec::new();
        // This file's last own statement's stream index — a no-op
        // include's same-line note attaches after it.
        let mut parent_last: Option<usize> = None;
        for (position, stmt) in file.items.into_iter().enumerate() {
            let splices = matches!(includes.peek(), Some((child_at, _)) if *child_at == position);
            if splices {
                let (_, child) = includes.next().expect("peeked");
                self.close_run(home, run_open..position);
                let before = self.items.len();
                self.splice(child, parsed);
                // The include never survives; its comments move to the
                // splice's neighbors (spec/include.md). With nothing
                // spliced the neighbors are its own file's.
                let Stmt::Include(include) = stmt else {
                    unreachable!("a child-bearing position holds the include statement");
                };
                self.includes_by_home[home].push((position, include.clone()));
                let (mut leading, notes) = (
                    include.comments,
                    include
                        .clause_comments
                        .iter()
                        .flat_map(|anchor| anchor.after.iter().cloned())
                        .collect::<Vec<String>>(),
                );
                let spliced = before..self.items.len();
                if !spliced.is_empty() {
                    let mut all = std::mem::take(&mut forward_comments);
                    all.extend(leading);
                    leading = all;
                    let slots = self.items[spliced.start].comment_slots();
                    let mut own = Vec::new();
                    std::mem::swap(&mut own, &mut *slots.leading);
                    *slots.leading = leading;
                    slots.leading.extend(own);
                    if !notes.is_empty() {
                        let last = self.items[spliced.end - 1].comment_slots();
                        last.clause.push(ClauseComment {
                            path: String::new(),
                            before: Vec::new(),
                            after: notes,
                        });
                    }
                } else {
                    Self::transfer_noop_comments(
                        &mut forward_comments,
                        &mut parent_last,
                        &mut self.items,
                        leading,
                        notes,
                    );
                }
                run_open = position + 1;
                continue;
            }
            if let Stmt::Include(include) = stmt {
                // A no-op include — a diamond or a cycle back-edge
                // (discovery gave it no child; every path-level
                // failure already failed the load before the splice).
                // It never survives; its comments move to its own
                // file's neighbors, and its scan position splits the
                // run like any include's.
                self.includes_by_home[home].push((position, include.clone()));
                self.close_run(home, run_open..position);
                run_open = position + 1;
                let notes = include
                    .clause_comments
                    .iter()
                    .flat_map(|anchor| anchor.after.iter().cloned())
                    .collect();
                Self::transfer_noop_comments(
                    &mut forward_comments,
                    &mut parent_last,
                    &mut self.items,
                    include.comments,
                    notes,
                );
                continue;
            }
            let mut stmt = stmt;
            if !forward_comments.is_empty() {
                let pending = std::mem::take(&mut forward_comments);
                let slots = stmt.comment_slots();
                let mut own = Vec::new();
                std::mem::swap(&mut own, &mut *slots.leading);
                *slots.leading = pending;
                slots.leading.extend(own);
            }
            self.items.push(stmt);
            self.item_homes.push(home);
            self.item_positions.push(position);
            parent_last = Some(self.items.len() - 1);
            consumed += 1;
        }
        self.close_run(home, run_open..total_positions);
        // A statement-less file still contributes its (empty) block so
        // its EOF directive problems render exactly once.
        if consumed == 0 {
            self.blocks.push(Block {
                home,
                statements: 0..0,
            });
        }
        // This file's tail comments — led by any leading comments
        // still pending at end of file (a trailing no-op include's
        // authored above it). The root's stay the document's tail
        // (behind any tail an earlier statement-less child parked
        // there); an included file's follow its last spliced
        // statement (the stream tail here — a child ending in an
        // include has the grandchild's tail item last, and the
        // comments physically follow it).
        let mut tail = std::mem::take(&mut forward_comments);
        tail.extend(file.trailing_comments);
        if home == 0 {
            self.trailing_comments.extend(tail);
        } else if !tail.is_empty() {
            match self.items.last_mut() {
                Some(last) => last.comment_slots().clause.push(ClauseComment {
                    path: String::new(),
                    before: Vec::new(),
                    after: tail,
                }),
                // The stream holds nothing of this file's yet: park
                // the tail for the root's flush (merge — several
                // statement-less children may park in sequence).
                None => self.trailing_comments.extend(tail),
            }
        }
    }

    fn close_run(&mut self, home: usize, run: std::ops::Range<usize>) {
        if !run.is_empty() {
            self.blocks.push(Block {
                home,
                statements: run,
            });
        }
    }

    /// A no-op include's comment transfer: its leading comments go to
    /// the statement BELOW (or this file's tail when none remains),
    /// its same-line note after the last statement above. Neighbor
    /// transfer for the diamond dedupe and the empty file's splice.
    fn transfer_noop_comments(
        forward_comments: &mut Vec<String>,
        parent_last: &mut Option<usize>,
        items: &mut [Stmt],
        leading: Vec<String>,
        notes: Vec<String>,
    ) {
        forward_comments.extend(leading);
        match parent_last.map(|index| &mut items[index]) {
            Some(last) if !notes.is_empty() => last.comment_slots().clause.push(ClauseComment {
                path: String::new(),
                before: Vec::new(),
                after: notes,
            }),
            // No statement above: the note rides with the leading
            // comments (its position class is best-effort; nothing
            // reads it yet).
            _ => forward_comments.extend(notes),
        }
    }
}

#[cfg(test)]
mod tests {
    /// The loader's alignment contract: one scan entry per parsed
    /// statement — comments of every class interleaved, includes
    /// counted. The composed lint's block ranges slice these.
    #[test]
    fn scan_aligns_with_the_parse_through_comment_runs() {
        for src in [
            "namespace acme;",
            "/// doc\nnamespace acme;",
            "// The spec's namespace, sequence, and param examples, in one clean file.\n\
             /// Acme Corporation\nnamespace acme;\n\nnamespace staging;\n\n\
             sequence invoice_number = 1000;\n\n/// Default page size\nparam $x = 50;",
        ] {
            let scan = crate::lint::scan_source(src);
            let (file, diags) = crate::parser::parse_file_composable(src);
            assert!(diags.is_empty(), "{diags:?}");
            assert_eq!(
                scan.statements.len(),
                file.items.len(),
                "scan/parse alignment for {src:?}"
            );
        }
    }
}
