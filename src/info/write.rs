//! The canonical SDL writer: the one AST → SDL text
//! function serving `fmt`, the normalizer's output, and the
//! differ's rendering.
//!
//! Style (pinned at the writer's introduction; the blank-line rule
//! added 2026-09-28
//! at the user's ask): clauses in each statement's grammar-block order
//! — the order its spec file prints, extracted statement by statement;
//! canonical spellings; `///` doc-comment lines above their statement;
//! one statement per line; 2-space indent; one blank line between
//! top-level statements — except a run of adjacent `include`
//! statements, which packs tight as one import group (a statement's
//! leading comments bind into its group, so the break always falls
//! between definitions); a single
//! trailing newline. The assignment
//! short form is taken when only its value is present
//! (shortest-spelling rule); representation-equal states otherwise
//! print their shortest shape without eliding anything — **fmt never
//! elides an authored clause** (elision is the normalizer's, over
//! resolved state).
//!
//! Authored aliases are preserved, never rewritten (codegen-model
//! item 4): `ns`/`db` keywords, `on: kv;`-class levels,
//! `snowball(eng)`-class language aliases, and the index `columns:`
//! key ride the AST's [`Authored`]/[`Carried`] provenance and print
//! as authored. Everything the AST holds by value prints canonically:
//! kinds through the kind printer, strings re-quoted, expressions as
//! their stored embedded-printer canon, durations and floats through
//! their value `Display`.
//!
//! The output is a total function of the AST and always re-parses;
//! both properties are property-tested (the fmt fixed point).

use crate::ast::{
    Access, AccessDuration, AccessKind, Analyzer, Api, ApiAnyBlock, Bucket, Carried, ClauseComment,
    Config, Database, DefaultConfig, Distance, DocComment, Event, Executable, Field, FieldDefault,
    FieldPath, FieldPathSeg, Filter, FnKind, Function, Graphql, Ident, Include, Index, IndexKind,
    Introspection, JwtIssuer, JwtSource, JwtVerify, Level, Middleware, Mixin, Module, Namespace,
    OnDelete, OpPermissions, Param, Pass, Permission, PermissionOp, Raw, Role, SdlFile, Sequence,
    Spread, Stmt, Table, TableKind, Tokenizer, User, VectorType,
};
use crate::keywords::{escape_with, quoted_string, sdl_ident_needs_escape, word_shaped};
use crate::kind::canonical_kind_text;

/// The default layout budget — the `--max-width` default, owned here
/// beside the writer whose lines it bounds (the crate's copy lives in
/// the private layout pass).
pub const DEFAULT_MAX_WIDTH: usize = crate::pretty::DEFAULT_MAX_WIDTH;

/// Writes the canonical SDL for a parsed file. An empty file writes
/// an empty string. One blank line separates adjacent top-level
/// statements, except a run of adjacent `include` statements, which
/// packs tight (one import group) — the break still falls between the
/// group and every other definition. A statement's leading comments
/// and trailing comments emit inside its own group (write_stmt), so a
/// break never detaches a comment from its anchor.
pub fn write_canonical(file: &SdlFile) -> String {
    write_canonical_with(file, crate::pretty::DEFAULT_MAX_WIDTH)
}

/// [`write_canonical`] under an explicit layout budget — the
/// `fmt`/`normalize` `--max-width` flag's entry.
pub fn write_canonical_with(file: &SdlFile, max_width: usize) -> String {
    let mut w = Writer {
        out: String::new(),
        indent: 0,
        pretty: true,
        max_width,
    };
    for (i, stmt) in file.items.iter().enumerate() {
        if i > 0 && !(is_include(stmt) && is_include(&file.items[i - 1])) {
            w.out.push('\n');
        }
        write_stmt(&mut w, stmt, false);
    }
    w.comments(&file.trailing_comments);
    w.out
}

/// Whether a top-level statement is an `include` — the writer's
/// pack-tight test for the import group.
fn is_include(stmt: &Stmt) -> bool {
    matches!(stmt, Stmt::Include(_))
}

/// The canonical SDL for ONE statement — the ONE canonical function's
/// statement arm: the differ's report renders both
/// resolved statements of a drift finding through this, and segments
/// the texts at clause boundaries. The writer's shape is the
/// segmentation's contract: every clause is exactly one line (values
/// never wrap) and block bodies indent — so a clause boundary IS a
/// line boundary (the one multi-line exception, a `raw` body, is
/// verbatim by definition and never segmented). Hence the flat mode:
/// the report renderer never runs the expression layout pass, even
/// though the file writer does — both sides of a delta render through
/// this same function, so the report stays internally consistent.
pub(crate) fn statement_text(stmt: &Stmt) -> String {
    let mut w = Writer {
        out: String::new(),
        indent: 0,
        pretty: false,
        max_width: crate::pretty::DEFAULT_MAX_WIDTH,
    };
    write_stmt(&mut w, stmt, false);
    w.out
}

struct Writer {
    out: String,
    indent: usize,
    /// Whether expression clause values run the layout pass
    /// (pretty.rs). The file writer sets this; the differ's report
    /// renderer keeps clauses single-line for its line-based
    /// segmentation.
    pretty: bool,
    /// The layout budget (pretty.rs) this writer's values break
    /// under — the `fmt`/`normalize` `--max-width` flag threads here.
    max_width: usize,
}

impl Writer {
    /// One full line: the indent, the text, the newline.
    fn line(&mut self, text: &str) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    /// One expression clause value onto a line buffer being built
    /// WITHOUT the line's indent prefix (the layout needs the real
    /// column, so `push_canon` adds the line's indent width itself).
    /// Flat mode emits the canon verbatim.
    fn push_canon(&self, buf: &mut String, canon: &str) {
        self.push_canon_at(self.indent, buf, canon);
    }

    /// [`Writer::push_canon`] for a line that will print at `indent`
    /// levels — the buffered-body paths call this with the OUTER
    /// writer's indent when a head built here prints there. The base
    /// indent is the LINE's own column: a laid-out value's closers
    /// (`)` / `}`) land back at the clause key's column and its
    /// content indents one unit in — body statements under a block
    /// opener, chain operands under their key.
    fn push_canon_at(&self, indent: usize, buf: &mut String, canon: &str) {
        if !self.pretty {
            buf.push_str(canon);
            return;
        }
        crate::pretty::write_value(
            buf,
            canon,
            indent * 2 + crate::pretty::current_column(buf),
            &"  ".repeat(indent),
            "  ",
            self.max_width,
        );
    }

    /// Plain `//` comment lines at the current indent — the file
    /// writer's layout preservation (rule 5). The differ's flat mode
    /// never emits them: its clause segmentation is a line protocol,
    /// and a comment line would read as a clause.
    fn comments(&mut self, comments: &[String]) {
        if !self.pretty {
            return;
        }
        for line in comments {
            self.line(&format!("//{line}"));
        }
    }

    /// Whether a clause path carries an after-note — the short-form
    /// guard: a same-line CLAUSE note has nowhere to sit on a one-line
    /// short form (its re-capture cannot find the clause), so its
    /// presence keeps the block form. Before-notes flatten safely
    /// (they print above the statement and re-capture as its leading
    /// lines), and so does the statement's close note — the short
    /// form's line IS the statement's final line.
    fn has_after(anchors: &[ClauseComment], path: &str) -> bool {
        anchors
            .iter()
            .any(|anchor| anchor.path == path && !anchor.after.is_empty())
    }

    /// One short-form statement line (`param $x = 5;`): the collapsed
    /// clause's before-notes flatten above it, and the statement's
    /// close note joins the line — the short form's final line, which
    /// re-capture reads back as the close note.
    fn short_form_line(&mut self, anchors: &[ClauseComment], path: &str, head: &str) {
        let (before, _) = self.anchored(anchors, path);
        self.comments(before);
        let (_, close) = self.anchored(anchors, "");
        let mut line = head.to_string();
        line.push_str(&Self::after_suffix(close));
        self.line(&line);
    }

    /// The anchor lookup for one clause path (rule 5): its
    /// before-notes and after-notes. Flat mode (the differ's report
    /// renderer) sees none — its line protocol has no comment lines.
    fn anchored<'a>(
        &self,
        anchors: &'a [ClauseComment],
        path: &str,
    ) -> (&'a [String], &'a [String]) {
        if !self.pretty {
            return (&[], &[]);
        }
        match anchors.iter().find(|anchor| anchor.path == path) {
            Some(anchor) => (&anchor.before, &anchor.after),
            None => (&[], &[]),
        }
    }

    /// The after-note line suffix: `//` glued to the note's own text
    /// (which conventionally begins with a space) — re-capture reads
    /// back the same text, so the suffix is a fixed point. A second
    /// note joins with its own `//`.
    fn after_suffix(after: &[String]) -> String {
        if after.is_empty() {
            String::new()
        } else {
            format!(" //{}", after.join(" //"))
        }
    }

    /// [`Writer::line`] with its clause anchors: before-notes above,
    /// the after-note joined onto the line's tail.
    fn line_a(&mut self, anchors: &[ClauseComment], path: &str, text: &str) {
        let (before, after) = self.anchored(anchors, path);
        self.comments(before);
        let mut line = text.to_string();
        line.push_str(&Self::after_suffix(after));
        self.line(&line);
    }

    /// One `key: value;` clause line with its clause anchors.
    fn clause_a<T: std::fmt::Display>(
        &mut self,
        anchors: &[ClauseComment],
        path: &str,
        key: &str,
        value: &T,
    ) {
        let (before, after) = self.anchored(anchors, path);
        self.comments(before);
        let mut line = format!("{key}: {value};");
        line.push_str(&Self::after_suffix(after));
        self.line(&line);
    }

    /// One `key: <expr>;` clause line with its anchors (the laid-out
    /// value keeps its anchors: before above, after joined after the
    /// `;` on whichever line it lands).
    fn canon_clause_a(&mut self, anchors: &[ClauseComment], path: &str, key: &str, canon: &str) {
        self.canon_list_clause_a(anchors, path, key, std::slice::from_ref(&canon));
    }

    /// The comma-list clause line with its anchors.
    fn canon_list_clause_a(
        &mut self,
        anchors: &[ClauseComment],
        path: &str,
        key: &str,
        items: &[&str],
    ) {
        let (before, after) = self.anchored(anchors, path);
        self.comments(before);
        let mut line = String::new();
        line.push_str(key);
        line.push_str(": ");
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                line.push_str(", ");
            }
            self.push_canon(&mut line, item);
        }
        line.push(';');
        line.push_str(&Self::after_suffix(after));
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(&line);
        self.out.push('\n');
    }

    /// A block clause with its anchors: before-notes above
    /// the `key {` line, the after-note joined onto the `};` line.
    fn block_a(
        &mut self,
        anchors: &[ClauseComment],
        path: &str,
        key: &str,
        body: impl FnOnce(&mut Self),
    ) {
        let (before, after) = self.anchored(anchors, path);
        self.comments(before);
        self.line(&format!("{key} {{"));
        self.indent += 1;
        body(self);
        self.indent -= 1;
        let mut close = String::from("};");
        close.push_str(&Self::after_suffix(after));
        self.line(&close);
    }

    /// The block close with the statement's empty-path anchor: the
    /// same-line close note joins the `};` line.
    fn close_a(&mut self, anchors: &[ClauseComment], trailing: &[String]) {
        self.indent += 1;
        self.comments(trailing);
        self.indent -= 1;
        let (_, after) = self.anchored(anchors, "");
        let mut close = String::from("};");
        close.push_str(&Self::after_suffix(after));
        self.line(&close);
    }

    /// A `prefix: <kind>` head whose kind is a top-level union longer
    /// than the budget: the first arm rides the key line and the rest
    /// break operator-led, one per line (`| arm`), with `suffix`
    /// closing the last arm. Returns false when no break is warranted
    /// — the line fits, or there is no top-level ` | ` to break at —
    /// and the caller emits the flat line. Layout only: the canonical
    /// kind text regenerates from the parsed AST on every write, so
    /// the break is never stored and the output stays a fixed point.
    fn kind_union_clause(&mut self, prefix: &str, kind: &str, suffix: &str) -> bool {
        // Flat mode (the differ's report renderer) pins every clause
        // to one line — its segmentation protocol cannot absorb a
        // broken header.
        if !self.pretty {
            return false;
        }
        let Some(arms) = union_arms(kind) else {
            return false;
        };
        if arms.len() < 2 {
            return false;
        }
        let flat_len = self.indent * 2
            + prefix.chars().count()
            + 2
            + kind.chars().count()
            + suffix.chars().count();
        if flat_len <= self.max_width {
            return false;
        }
        self.line(&format!("{prefix}: {}", arms[0]));
        self.indent += 1;
        let last = arms.len() - 1;
        for (i, arm) in arms.iter().enumerate().skip(1) {
            let mut line = format!("| {arm}");
            if i == last {
                line.push_str(suffix);
            }
            self.line(&line);
        }
        self.indent -= 1;
        true
    }

    fn doc_comment(&mut self, comment: &DocComment) {
        for line in &comment.lines {
            if line.is_empty() {
                self.line("///");
            } else {
                self.line(&format!("/// {line}"));
            }
        }
    }
}

/// An identifier through the SDL escape rule: bare when a plain
/// non-reserved word, backtick-escaped otherwise (the SurrealQL
/// keyword table plus SDL's own reservation, `raw`).
fn push_ident(out: &mut String, raw: &str) {
    if sdl_ident_needs_escape(raw) {
        out.push('`');
        escape_with(out, raw, '`');
        out.push('`');
    } else {
        out.push_str(raw);
    }
}

/// A `$`-parameter name: the shape rule only (upstream prints
/// keyword-colliding names bare — `$table` — probe-verified).
fn push_param_name(out: &mut String, raw: &str) {
    if word_shaped(raw) {
        out.push_str(raw);
    } else {
        out.push('`');
        escape_with(out, raw, '`');
        out.push('`');
    }
}

/// A field path: `.`-joined segments, wildcards as `*`.
fn push_field_path(out: &mut String, path: &FieldPath) {
    for (i, segment) in path.segments.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        match segment {
            FieldPathSeg::Name(name) => push_ident(out, &name.raw),
            FieldPathSeg::Wildcard => out.push('*'),
        }
    }
}

fn idents_joined(idents: &[Ident], separator: &str) -> String {
    let mut out = String::new();
    for (i, ident) in idents.iter().enumerate() {
        if i > 0 {
            out.push_str(separator);
        }
        push_ident(&mut out, &ident.raw);
    }
    out
}

// ---- statement dispatch ----

/// One statement (plus its leading `//` comments and its doc comment)
/// at the current indent. `nested` is set inside a `table` block:
/// fields, indexes, and events omit their `on:` there (the host table
/// is the `on`).
fn write_stmt(w: &mut Writer, stmt: &Stmt, nested: bool) {
    w.comments(stmt_comments(stmt));
    match stmt {
        Stmt::Namespace(stmt) => write_namespace(w, stmt),
        Stmt::Sequence(stmt) => write_sequence(w, stmt),
        Stmt::Param(stmt) => write_param(w, stmt),
        Stmt::Database(stmt) => write_database(w, stmt),
        Stmt::Bucket(stmt) => write_bucket(w, stmt),
        Stmt::User(stmt) => write_user(w, stmt),
        Stmt::Analyzer(stmt) => write_analyzer(w, stmt),
        Stmt::Event(stmt) => write_event(w, stmt, nested),
        Stmt::Module(stmt) => write_module(w, stmt),
        Stmt::Function(stmt) => write_function(w, stmt),
        Stmt::Config(stmt) => write_config(w, stmt),
        Stmt::Index(stmt) => write_index(w, stmt, nested),
        Stmt::Api(stmt) => write_api(w, stmt),
        Stmt::Access(stmt) => write_access(w, stmt),
        Stmt::Table(stmt) => write_table(w, stmt),
        Stmt::Field(stmt) => write_field(w, stmt, nested),
        Stmt::Mixin(stmt) => write_mixin(w, stmt),
        Stmt::Spread(stmt) => write_spread(w, stmt),
        Stmt::Raw(stmt) => write_raw(w, stmt),
        Stmt::Include(stmt) => write_include(w, stmt),
    }
}

/// A statement's leading `//` lines — one read for the dispatch above.
fn stmt_comments(stmt: &Stmt) -> &[String] {
    match stmt {
        Stmt::Namespace(stmt) => &stmt.comments,
        Stmt::Sequence(stmt) => &stmt.comments,
        Stmt::Param(stmt) => &stmt.comments,
        Stmt::Database(stmt) => &stmt.comments,
        Stmt::Bucket(stmt) => &stmt.comments,
        Stmt::User(stmt) => &stmt.comments,
        Stmt::Analyzer(stmt) => &stmt.comments,
        Stmt::Event(stmt) => &stmt.comments,
        Stmt::Module(stmt) => &stmt.comments,
        Stmt::Function(stmt) => &stmt.comments,
        Stmt::Index(stmt) => &stmt.comments,
        Stmt::Api(stmt) => &stmt.comments,
        Stmt::Access(stmt) => &stmt.comments,
        Stmt::Table(stmt) => &stmt.comments,
        Stmt::Field(stmt) => &stmt.comments,
        Stmt::Mixin(stmt) => &stmt.comments,
        Stmt::Spread(stmt) => &stmt.comments,
        Stmt::Raw(stmt) => &stmt.comments,
        Stmt::Include(stmt) => &stmt.comments,
        // The config variants carry their comment fields on the inner
        // structs; their writers emit both ends themselves.
        Stmt::Config(_) => &[],
    }
}

fn write_namespace(w: &mut Writer, stmt: &Namespace) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    let mut head = String::new();
    head.push_str(keyword_word(&stmt.keyword, "namespace"));
    head.push(' ');
    push_ident(&mut head, &stmt.name.raw);
    head.push(';');
    // `namespace` has no clauses: the empty-path anchor (a same-line
    // note after the statement) is the only resident.
    w.line_a(&stmt.clause_comments, "", &head);
}

fn write_sequence(w: &mut Writer, stmt: &Sequence) {
    let mut head = String::from("sequence ");
    push_ident(&mut head, &stmt.name.raw);
    // Shortest spelling: the assignment short form when `start` is
    // the only clause present — unless the start clause carries a
    // same-line after-note, which only the block form can hold (the
    // short-form guard; before-notes and the close note flatten onto
    // the short-form line).
    if let (Some(start), None, None) = (&stmt.start, &stmt.batch, &stmt.timeout)
        && !Writer::has_after(&stmt.clause_comments, "start")
    {
        head.push_str(" = ");
        w.push_canon(&mut head, &start.canon);
        head.push(';');
        w.short_form_line(&stmt.clause_comments, "start", &head);
        w.comments(&stmt.trailing_comments);
        return;
    }
    if stmt.start.is_none() && stmt.batch.is_none() && stmt.timeout.is_none() {
        // No clauses at all: the bare header (a present start either
        // took the short form above or needs its block for the note).
        head.push(';');
        w.line_a(&stmt.clause_comments, "", &head);
        w.comments(&stmt.trailing_comments);
        return;
    }
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    if let Some(start) = &stmt.start {
        w.canon_clause_a(&stmt.clause_comments, "start", "start", &start.canon);
    }
    if let Some(batch) = &stmt.batch {
        w.canon_clause_a(&stmt.clause_comments, "batch", "batch", &batch.canon);
    }
    if let Some(timeout) = &stmt.timeout {
        w.canon_clause_a(&stmt.clause_comments, "timeout", "timeout", &timeout.canon);
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_param(w: &mut Writer, stmt: &Param) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    let mut head = String::from("param $");
    push_param_name(&mut head, &stmt.name.raw);
    // Short form: value only (`permissions` absent — a param without
    // it stores VALUE NONE, so value-only means exactly the short
    // form's shape). Both absent collapses to the bare header (the
    // bare form `param $x;` is its authored shape too). A value
    // after-note keeps the block form (the short-form guard).
    match (&stmt.value, &stmt.permissions) {
        (Some(value), None) if !Writer::has_after(&stmt.clause_comments, "value") => {
            head.push_str(" = ");
            w.push_canon(&mut head, &value.canon);
            head.push(';');
            w.short_form_line(&stmt.clause_comments, "value", &head);
            w.comments(&stmt.trailing_comments);
            return;
        }
        (None, None) => {
            head.push(';');
            w.line_a(&stmt.clause_comments, "", &head);
            w.comments(&stmt.trailing_comments);
            return;
        }
        _ => {}
    }
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    if let Some(value) = &stmt.value {
        w.canon_clause_a(&stmt.clause_comments, "value", "value", &value.canon);
    }
    if let Some(permissions) = &stmt.permissions {
        write_permission(w, &stmt.clause_comments, permissions);
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_database(w: &mut Writer, stmt: &Database) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    let mut head = String::new();
    head.push_str(keyword_word(&stmt.keyword, "database"));
    head.push(' ');
    push_ident(&mut head, &stmt.name.raw);
    if !stmt.strict && stmt.changefeed.is_none() {
        head.push(';');
        w.line_a(&stmt.clause_comments, "", &head);
        w.comments(&stmt.trailing_comments);
        return;
    }
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    if stmt.strict {
        w.line_a(&stmt.clause_comments, "strict", "strict;");
    }
    if let Some(changefeed) = &stmt.changefeed {
        write_changefeed(w, &stmt.clause_comments, changefeed);
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_changefeed(
    w: &mut Writer,
    anchors: &[ClauseComment],
    changefeed: &crate::ast::ChangeFeed,
) {
    w.block_a(anchors, "changefeed", "changefeed", |w| {
        w.clause_a(anchors, "changefeed.expiry", "expiry", &changefeed.expiry);
        if changefeed.include_original {
            w.line_a(anchors, "changefeed.include original", "include original;");
        }
    });
}

fn write_bucket(w: &mut Writer, stmt: &Bucket) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    let mut head = String::from("bucket ");
    push_ident(&mut head, &stmt.name.raw);
    if stmt.backend.is_none() && !stmt.readonly && stmt.permissions.is_none() {
        head.push(';');
        w.line_a(&stmt.clause_comments, "", &head);
        w.comments(&stmt.trailing_comments);
        return;
    }
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    if let Some(backend) = &stmt.backend {
        w.canon_clause_a(&stmt.clause_comments, "backend", "backend", &backend.canon);
    }
    if stmt.readonly {
        w.line_a(&stmt.clause_comments, "readonly", "readonly;");
    }
    if let Some(permissions) = &stmt.permissions {
        write_permission(w, &stmt.clause_comments, permissions);
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_user(w: &mut Writer, stmt: &User) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    let mut head = String::from("user ");
    push_ident(&mut head, &stmt.name.raw);
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    // Grammar-block order: on, password/passhash, passscram, roles,
    // duration.
    let mut on = String::from("on: ");
    on.push_str(&level_word(stmt.on.value, stmt.on.spelling));
    on.push(';');
    w.line_a(&stmt.clause_comments, "on", &on);
    match &stmt.pass {
        Some(Pass::Password(value)) => {
            let mut quoted = String::new();
            quoted_string(&mut quoted, value);
            w.clause_a(&stmt.clause_comments, "password", "password", &quoted);
        }
        Some(Pass::Hash(value)) => {
            let mut quoted = String::new();
            quoted_string(&mut quoted, value);
            w.clause_a(&stmt.clause_comments, "passhash", "passhash", &quoted);
        }
        None => {}
    }
    if let Some(scram) = &stmt.scram {
        let mut quoted = String::new();
        quoted_string(&mut quoted, scram);
        w.clause_a(&stmt.clause_comments, "passscram", "passscram", &quoted);
    }
    if !stmt.roles.is_empty() {
        let roles = stmt
            .roles
            .iter()
            .map(|role| match role {
                Role::Owner => "owner",
                Role::Editor => "editor",
                Role::Viewer => "viewer",
            })
            .collect::<Vec<_>>()
            .join(", ");
        w.clause_a(&stmt.clause_comments, "roles", "roles", &roles);
    }
    if stmt.token_duration.is_some() || stmt.session_duration.is_some() {
        w.block_a(&stmt.clause_comments, "duration", "duration", |w| {
            if let Some(token) = &stmt.token_duration {
                w.canon_clause_a(
                    &stmt.clause_comments,
                    "duration.token",
                    "token",
                    &token.canon,
                );
            }
            if let Some(session) = &stmt.session_duration {
                w.canon_clause_a(
                    &stmt.clause_comments,
                    "duration.session",
                    "session",
                    &session.canon,
                );
            }
        });
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_analyzer(w: &mut Writer, stmt: &Analyzer) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    let mut head = String::from("analyzer ");
    push_ident(&mut head, &stmt.name.raw);
    if stmt.function.is_none() && stmt.tokenizers.is_none() && stmt.filters.is_none() {
        head.push(';');
        w.line_a(&stmt.clause_comments, "", &head);
        w.comments(&stmt.trailing_comments);
        return;
    }
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    if let Some(function) = &stmt.function {
        w.clause_a(
            &stmt.clause_comments,
            "function",
            "function",
            &fn_path_string(function),
        );
    }
    if let Some(tokenizers) = &stmt.tokenizers {
        let words = tokenizers
            .iter()
            .map(|t| match t {
                Tokenizer::Blank => "blank",
                Tokenizer::Camel => "camel",
                Tokenizer::Class => "class",
                Tokenizer::Punct => "punct",
            })
            .collect::<Vec<_>>()
            .join(", ");
        w.clause_a(&stmt.clause_comments, "tokenizers", "tokenizers", &words);
    }
    if let Some(filters) = &stmt.filters {
        let words = filters
            .iter()
            .map(filter_word)
            .collect::<Vec<_>>()
            .join(", ");
        w.clause_a(&stmt.clause_comments, "filters", "filters", &words);
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_event(w: &mut Writer, stmt: &Event, nested: bool) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    let mut head = String::from("event ");
    push_ident(&mut head, &stmt.name.raw);
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    // Grammar-block order: on, when, then, async (spec/event.md's own
    // listing, and the reading SurrealQL's `WHEN … THEN …` gives).
    if !nested {
        let mut on = String::from("on: ");
        push_ident(&mut on, &stmt.on.raw);
        on.push(';');
        w.line_a(&stmt.clause_comments, "on", &on);
    }
    if let Some(when) = &stmt.when {
        w.canon_clause_a(&stmt.clause_comments, "when", "when", &when.canon);
    }
    let then_items: Vec<&str> = stmt.then.iter().map(|expr| expr.canon.as_str()).collect();
    w.canon_list_clause_a(&stmt.clause_comments, "then", "then", &then_items);
    if let Some(r#async) = &stmt.r#async {
        match (&r#async.retry, &r#async.maxdepth) {
            (None, None) => w.line_a(&stmt.clause_comments, "async", "async;"),
            (retry, maxdepth) => {
                w.block_a(&stmt.clause_comments, "async", "async", |w| {
                    if let Some(retry) = retry {
                        w.clause_a(&stmt.clause_comments, "async.retry", "retry", retry);
                    }
                    if let Some(maxdepth) = maxdepth {
                        w.clause_a(
                            &stmt.clause_comments,
                            "async.maxdepth",
                            "maxdepth",
                            maxdepth,
                        );
                    }
                });
            }
        }
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_module(w: &mut Writer, stmt: &Module) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    let mut head = String::from("module");
    if let Some(name) = &stmt.name {
        head.push(' ');
        push_ident(&mut head, &name.raw);
    }
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    match &stmt.executable {
        Executable::File { bucket, key } => {
            let mut quoted = String::new();
            quoted_string(&mut quoted, &format!("{bucket}:{key}"));
            w.line_a(&stmt.clause_comments, "from", &format!("from: f{quoted};"));
        }
        Executable::Silo {
            org,
            pkg,
            major,
            minor,
            patch,
        } => {
            // Segments decode from (possibly quoted) idents — re-spell
            // them through the shape rule like every other name path.
            let mut from = String::from("from: silo::");
            push_path_segment(&mut from, org);
            from.push_str("::");
            push_path_segment(&mut from, pkg);
            from.push_str(&format!("::<{major}.{minor}.{patch}>;"));
            w.line_a(&stmt.clause_comments, "from", &from);
        }
    }
    w.line_a(&stmt.clause_comments, "unsigned", "unsigned;");
    if let Some(permissions) = &stmt.permissions {
        write_permission(w, &stmt.clause_comments, permissions);
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_function(w: &mut Writer, stmt: &Function) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    // Header: the signature, then the body block (the canon carries
    // its own braces), then — only when trailing clauses exist — the
    // second block (grammar-block order: permissions, graphql).
    let mut head = String::from("fn ");
    // The statement keyword IS the `fn` prefix (SDL writes `fn name`,
    // never `fn fn::name`); the parser always stores the name with
    // `FnKind::Fn`, so only the segments print.
    head.push_str(&fn_path_segments_string(&stmt.name));
    head.push('(');
    for (i, (param, kind)) in stmt.args.iter().enumerate() {
        if i > 0 {
            head.push_str(", ");
        }
        head.push('$');
        push_param_name(&mut head, &param.raw);
        head.push_str(": ");
        head.push_str(&canonical_kind_text(&kind.text));
    }
    head.push(')');
    if let Some(returns) = &stmt.returns {
        head.push_str(" -> ");
        head.push_str(&canonical_kind_text(&returns.text));
    }
    head.push(' ');
    w.push_canon(&mut head, &stmt.body.canon);
    let graphql = graphql_present(&stmt.graphql);
    if stmt.permissions.is_none() && !graphql {
        head.push(';');
        w.line_a(&stmt.clause_comments, "", &head);
        w.comments(&stmt.trailing_comments);
        return;
    }
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    if let Some(permissions) = &stmt.permissions {
        write_permission(w, &stmt.clause_comments, permissions);
    }
    write_graphql(w, &stmt.clause_comments, &stmt.graphql);
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_config(w: &mut Writer, stmt: &Config) {
    match stmt {
        Config::Api(config) => {
            w.comments(&config.comments);
            w.line("config api {");
            w.indent += 1;
            write_middleware_clause(w, &config.clause_comments, &config.middleware);
            if let Some(permissions) = &config.permissions {
                write_permission(w, &config.clause_comments, permissions);
            }
            w.indent -= 1;
            w.close_a(&config.clause_comments, &config.trailing_comments);
        }
        Config::Graphql(config) => {
            // Representation note: `Family::None` and
            // `Introspection::Auto` are both the unwritten default AND
            // spellable — the AST cannot distinguish, so both print
            // nothing (a file's authored `tables: none;` /
            // `introspection: auto;` normalizes away; the values
            // compare equal everywhere).
            w.comments(&config.comments);
            w.line("config graphql {");
            w.indent += 1;
            write_family(
                w,
                &config.clause_comments,
                "tables",
                &config.tables,
                |item| {
                    let mut out = String::new();
                    push_ident(&mut out, &item.raw);
                    out
                },
            );
            write_family(
                w,
                &config.clause_comments,
                "functions",
                &config.functions,
                fn_path_string,
            );
            if let Some(depth) = &config.depth {
                w.clause_a(&config.clause_comments, "depth", "depth", depth);
            }
            if let Some(complexity) = &config.complexity {
                w.clause_a(
                    &config.clause_comments,
                    "complexity",
                    "complexity",
                    complexity,
                );
            }
            if matches!(config.introspection, Introspection::None) {
                w.line_a(
                    &config.clause_comments,
                    "introspection",
                    "introspection: none;",
                );
            }
            w.indent -= 1;
            w.close_a(&config.clause_comments, &config.trailing_comments);
        }
        Config::Default(config) => {
            let DefaultConfig {
                namespace,
                database,
                comments,
                trailing_comments,
                clause_comments,
            } = config;
            w.comments(comments);
            w.line("config default {");
            w.indent += 1;
            if let Some(namespace) = namespace {
                w.canon_clause_a(clause_comments, "namespace", "namespace", &namespace.canon);
            }
            if let Some(database) = database {
                w.canon_clause_a(clause_comments, "database", "database", &database.canon);
            }
            w.indent -= 1;
            w.close_a(clause_comments, trailing_comments);
        }
    }
}

fn write_family<T>(
    w: &mut Writer,
    anchors: &[ClauseComment],
    key: &str,
    family: &crate::ast::Family<T>,
    item: impl Fn(&T) -> String,
) {
    match family {
        crate::ast::Family::None => {}
        crate::ast::Family::Auto => {
            w.clause_a(anchors, key, key, &"auto");
        }
        crate::ast::Family::Include(items) => {
            let list = items.iter().map(&item).collect::<Vec<_>>().join(", ");
            let include_path = format!("{key}.include");
            w.block_a(anchors, key, key, |w| {
                w.clause_a(anchors, &include_path, "include", &list);
            });
        }
        crate::ast::Family::Exclude(items) => {
            let list = items.iter().map(&item).collect::<Vec<_>>().join(", ");
            let exclude_path = format!("{key}.exclude");
            w.block_a(anchors, key, key, |w| {
                w.clause_a(anchors, &exclude_path, "exclude", &list);
            });
        }
    }
}

fn write_index(w: &mut Writer, stmt: &Index, nested: bool) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    let variant = match &stmt.kind {
        IndexKind::Plain => "",
        IndexKind::Unique => "unique ",
        IndexKind::Count { .. } => "count ",
        IndexKind::FullText { .. } => "fulltext ",
        IndexKind::Hnsw { .. } => "hnsw ",
        IndexKind::DiskAnn { .. } => "diskann ",
    };
    let mut head = format!("index {variant}");
    push_ident(&mut head, &stmt.name.raw);
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    // Grammar-block order: on, fields, concurrently, kind clauses.
    if !nested {
        let mut on = String::from("on: ");
        push_ident(&mut on, &stmt.on.raw);
        on.push(';');
        w.line_a(&stmt.clause_comments, "on", &on);
    }
    // `count` takes no fields (its AST list is empty — the grammar
    // forbids the clause); every other kind requires at least one, so
    // printing only non-empty lists never drops an authored clause.
    // The anchor path is the authored key too — the parser anchors
    // under whichever spelling the file used (`fields`/`columns`).
    if !stmt.fields.is_empty() {
        let fields: Vec<&str> = stmt.fields.iter().map(|expr| expr.canon.as_str()).collect();
        let fields_key = keyword_word(&stmt.fields_key, "fields");
        w.canon_list_clause_a(&stmt.clause_comments, fields_key, fields_key, &fields);
    }
    if stmt.concurrently {
        w.line_a(&stmt.clause_comments, "concurrently", "concurrently;");
    }
    match &stmt.kind {
        IndexKind::Count { where_ } => {
            if let Some(where_) = where_ {
                w.canon_clause_a(&stmt.clause_comments, "where", "where", &where_.canon);
            }
        }
        IndexKind::FullText {
            analyzer,
            bm25,
            highlights,
        } => {
            if let Some(analyzer) = analyzer {
                let mut clause = String::from("analyzer: ");
                push_ident(&mut clause, &analyzer.raw);
                clause.push(';');
                w.line_a(&stmt.clause_comments, "analyzer", &clause);
            }
            if let Some(bm25) = bm25 {
                w.line_a(
                    &stmt.clause_comments,
                    "bm25",
                    &format!("bm25: ({}, {});", bm25.0, bm25.1),
                );
            }
            if *highlights {
                w.line_a(&stmt.clause_comments, "highlights", "highlights;");
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
            w.clause_a(&stmt.clause_comments, "dimension", "dimension", dimension);
            if let Some(distance) = distance {
                w.clause_a(
                    &stmt.clause_comments,
                    "distance",
                    "distance",
                    &distance_word(*distance),
                );
            }
            if let Some(vector_type) = vector_type {
                w.clause_a(
                    &stmt.clause_comments,
                    "type",
                    "type",
                    &vector_type_word(*vector_type),
                );
            }
            if let Some(m) = m {
                w.clause_a(&stmt.clause_comments, "m", "m", m);
            }
            if let Some(m0) = m0 {
                w.clause_a(&stmt.clause_comments, "m0", "m0", m0);
            }
            if let Some(lm) = lm {
                w.clause_a(&stmt.clause_comments, "lm", "lm", &lm.0);
            }
            if let Some(efc) = efc {
                w.clause_a(&stmt.clause_comments, "efc", "efc", efc);
            }
            if *extend_candidates {
                w.line_a(
                    &stmt.clause_comments,
                    "extend_candidates",
                    "extend_candidates;",
                );
            }
            if *keep_pruned_connections {
                w.line_a(
                    &stmt.clause_comments,
                    "keep_pruned_connections",
                    "keep_pruned_connections;",
                );
            }
            if *hashed_vector {
                w.line_a(&stmt.clause_comments, "hashed_vector", "hashed_vector;");
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
        } => {
            w.clause_a(&stmt.clause_comments, "dimension", "dimension", dimension);
            if let Some(distance) = distance {
                w.clause_a(
                    &stmt.clause_comments,
                    "distance",
                    "distance",
                    &distance_word(*distance),
                );
            }
            if let Some(vector_type) = vector_type {
                w.clause_a(
                    &stmt.clause_comments,
                    "type",
                    "type",
                    &vector_type_word(*vector_type),
                );
            }
            if let Some(degree) = degree {
                w.clause_a(&stmt.clause_comments, "degree", "degree", degree);
            }
            if let Some(l_build) = l_build {
                w.clause_a(&stmt.clause_comments, "l_build", "l_build", l_build);
            }
            if let Some(alpha) = alpha {
                w.clause_a(&stmt.clause_comments, "alpha", "alpha", &alpha.0);
            }
            if *hashed_vector {
                w.line_a(&stmt.clause_comments, "hashed_vector", "hashed_vector;");
            }
        }
        IndexKind::Plain | IndexKind::Unique => {}
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_api(w: &mut Writer, stmt: &Api) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    let mut path = String::new();
    quoted_string(&mut path, &stmt.path);
    w.line(&format!("api {path} {{"));
    w.indent += 1;
    // Canonical placement (spec/api.md): the endpoint-wide `any`
    // block FIRST — "canonical SDL places it first, matching the
    // emitter" — then the method groups in source order.
    if let Some(ApiAnyBlock {
        config,
        fallback,
        comments,
        trailing_comments,
        clause_comments,
    }) = &stmt.any
    {
        write_api_group(
            w,
            None,
            config,
            fallback.as_ref(),
            comments,
            trailing_comments,
            clause_comments,
        );
    }
    for action in &stmt.actions {
        write_api_group(
            w,
            Some(&action.methods),
            &action.config,
            Some(&action.action),
            &action.comments,
            &action.trailing_comments,
            &action.clause_comments,
        );
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_api_group(
    w: &mut Writer,
    methods: Option<&[crate::ast::ApiMethod]>,
    config: &crate::ast::ApiGroupConfig,
    action: Option<&crate::ast::Expr>,
    comments: &[String],
    trailing_comments: &[String],
    anchors: &[ClauseComment],
) {
    w.comments(comments);
    match methods {
        Some(methods) => {
            let heads = methods
                .iter()
                .map(|method| method.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            w.line(&format!("{heads} {{"));
        }
        None => w.line("any {"),
    }
    w.indent += 1;
    // Grammar-block order: permissions, middleware, then.
    if let Some(permissions) = &config.permissions {
        write_permission(w, anchors, permissions);
    }
    write_middleware_clause(w, anchors, &config.middleware);
    if let Some(action) = action {
        w.canon_clause_a(anchors, "then", "then", &action.canon);
    }
    w.indent -= 1;
    w.close_a(anchors, trailing_comments);
}

fn write_access(w: &mut Writer, stmt: &Access) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    let mut head = String::from("access ");
    push_ident(&mut head, &stmt.name.raw);
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    // Grammar-block order: on, type, the variant's clauses, duration,
    // authenticate, context.
    let mut on = String::from("on: ");
    on.push_str(&level_word(stmt.on.value, stmt.on.spelling));
    on.push(';');
    w.line_a(&stmt.clause_comments, "on", &on);
    match &stmt.kind {
        AccessKind::Jwt { source } => {
            w.line_a(&stmt.clause_comments, "type", "type: jwt;");
            write_jwt_source(w, &stmt.clause_comments, "", source);
        }
        AccessKind::Record {
            signup,
            signin,
            jwt,
            refresh,
        } => {
            w.line_a(&stmt.clause_comments, "type", "type: record;");
            if let Some(signup) = signup {
                w.canon_clause_a(&stmt.clause_comments, "signup", "signup", &signup.canon);
            }
            if let Some(signin) = signin {
                w.canon_clause_a(&stmt.clause_comments, "signin", "signin", &signin.canon);
            }
            if let Some(jwt) = jwt {
                w.block_a(&stmt.clause_comments, "jwt", "jwt", |w| {
                    write_jwt_source(w, &stmt.clause_comments, "jwt.", jwt);
                });
            }
            if *refresh {
                w.line_a(&stmt.clause_comments, "refresh", "refresh;");
            }
        }
        AccessKind::Bearer { subject, jwt } => {
            w.line_a(&stmt.clause_comments, "type", "type: bearer;");
            match subject {
                crate::ast::BearerSubject::User => {
                    w.clause_a(&stmt.clause_comments, "for", "for", &"user");
                }
                crate::ast::BearerSubject::Record => {
                    w.clause_a(&stmt.clause_comments, "for", "for", &"record");
                }
            }
            if let Some(jwt) = jwt {
                w.block_a(&stmt.clause_comments, "jwt", "jwt", |w| {
                    write_jwt_source(w, &stmt.clause_comments, "jwt.", jwt);
                });
            }
        }
    }
    if let Some(duration) = duration_present(&stmt.duration) {
        w.block_a(&stmt.clause_comments, "duration", "duration", |w| {
            if let Some(grant) = &duration.grant {
                w.canon_clause_a(
                    &stmt.clause_comments,
                    "duration.grant",
                    "grant",
                    &grant.canon,
                );
            }
            if let Some(token) = &duration.token {
                w.canon_clause_a(
                    &stmt.clause_comments,
                    "duration.token",
                    "token",
                    &token.canon,
                );
            }
            if let Some(session) = &duration.session {
                w.canon_clause_a(
                    &stmt.clause_comments,
                    "duration.session",
                    "session",
                    &session.canon,
                );
            }
        });
    }
    if let Some(authenticate) = &stmt.authenticate {
        w.canon_clause_a(
            &stmt.clause_comments,
            "authenticate",
            "authenticate",
            &authenticate.canon,
        );
    }
    if let Some(context) = &stmt.context {
        w.canon_clause_a(&stmt.clause_comments, "context", "context", &context.canon);
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

/// A `duration` block with no side present folds to `None` at parse;
/// a defensive guard for normalizer-built ASTs keeps the same shape.
fn duration_present(duration: &Option<AccessDuration>) -> Option<&AccessDuration> {
    match duration {
        Some(duration)
            if duration.grant.is_some()
                || duration.token.is_some()
                || duration.session.is_some() =>
        {
            Some(duration)
        }
        _ => None,
    }
}

/// One JWT verification source: `prefix` is the anchor path prefix —
/// empty for the jwt variant's direct clauses, `"jwt."` inside a
/// `jwt { … }` sub-block.
fn write_jwt_source(w: &mut Writer, anchors: &[ClauseComment], prefix: &str, source: &JwtSource) {
    let path = |leaf: &str| format!("{prefix}{leaf}");
    match &source.verify {
        JwtVerify::Url(url) => w.canon_clause_a(anchors, &path("url"), "url", &url.canon),
        JwtVerify::Key { algorithm, key } => {
            w.clause_a(
                anchors,
                &path("algorithm"),
                "algorithm",
                &algorithm_word(*algorithm),
            );
            w.canon_clause_a(anchors, &path("key"), "key", &key.canon);
        }
    }
    if let Some(audience) = &source.audience {
        // One clause line, the values re-quoted (decoded at parse).
        let mut line = String::from("audience: ");
        for (i, value) in audience.iter().enumerate() {
            if i > 0 {
                line.push_str(", ");
            }
            quoted_string(&mut line, value);
        }
        line.push(';');
        w.line_a(anchors, &path("audience"), &line);
    }
    if let Some(issuer) = issuer_present(&source.issuer) {
        let issuer_path = path("issuer");
        w.block_a(anchors, &issuer_path, "issuer", |w| {
            if let Some(algorithm) = &issuer.algorithm {
                w.clause_a(
                    anchors,
                    &format!("{issuer_path}.algorithm"),
                    "algorithm",
                    &algorithm_word(*algorithm),
                );
            }
            if let Some(key) = &issuer.key {
                w.canon_clause_a(anchors, &format!("{issuer_path}.key"), "key", &key.canon);
            }
        });
    }
}

fn issuer_present(issuer: &Option<JwtIssuer>) -> Option<&JwtIssuer> {
    match issuer {
        Some(issuer) if issuer.algorithm.is_some() || issuer.key.is_some() => Some(issuer),
        _ => None,
    }
}

fn write_table(w: &mut Writer, stmt: &Table) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    // The kind rides the header, left to right (spec/table.md); the
    // unconstrained relation's canonical spelling is `table relation`.
    // An empty block collapses to the bare header (a view never does —
    // its block IS the query). `enforced` forces the block only when
    // it prints as its own clause line — under `lightweight` the
    // header word carries it (the implied flag is the shortest
    // spelling, and the AST's parse-fold never pairs the two).
    let empty = !matches!(stmt.kind, TableKind::View { .. })
        && stmt.schema_full.is_none()
        && !stmt.drop
        && stmt.changefeed.is_none()
        && stmt.permissions.is_none()
        && stmt.graphql.is_none()
        && stmt.inline_edges.is_none()
        && stmt.inline_references.is_none()
        && !matches!(
            stmt.kind,
            TableKind::Relation {
                enforced: true,
                lightweight: false,
                ..
            }
        )
        && stmt.nested.is_empty();
    let mut head = String::from("table");
    // A lightweight relation without both sides named would print the
    // word into the to-list position — a silently different valid
    // table (`-> name -> lightweight;` re-parses as an ENDPOINT, no
    // flag). Both parsers guarantee the sides; a library-built AST
    // that breaks the invariant is a bug.
    if let TableKind::Relation {
        lightweight: true,
        from,
        to,
        ..
    } = &stmt.kind
    {
        debug_assert!(
            from.is_some() && to.is_some(),
            "a LIGHTWEIGHT relation requires both sides named"
        );
    }
    match &stmt.kind {
        TableKind::Normal => {
            head.push(' ');
            push_ident(&mut head, &stmt.name.raw);
        }
        TableKind::Any => {
            head.push_str(" any ");
            push_ident(&mut head, &stmt.name.raw);
        }
        TableKind::Relation {
            from,
            to,
            enforced: _,
            lightweight,
        } => match (from, to) {
            (None, None) => {
                head.push_str(" relation ");
                push_ident(&mut head, &stmt.name.raw);
            }
            (from, to) => {
                if let Some(from) = from {
                    head.push(' ');
                    // The first from-side element must print bare: in
                    // that header position a quoted ident reads as the
                    // table's NAME (`table `user` -> …` parses as an
                    // in-only relation named user), so an
                    // escape-requiring first element is inexpressible
                    // in the arrow form and can never ride the AST.
                    for (i, ident) in from.iter().enumerate() {
                        if i > 0 {
                            head.push_str(" | ");
                            push_ident(&mut head, &ident.raw);
                        } else {
                            head.push_str(&ident.raw);
                        }
                    }
                }
                // The arrows print compact (`x->does->y`), the
                // relation sugar reading as one token chain. The
                // parser's arrow detection is whitespace-independent,
                // so the compact spelling round-trips. The trailing
                // `lightweight` flag keeps its leading space — without
                // it the word would glue onto the last endpoint ident
                // and re-read as one identifier.
                head.push_str("->");
                push_ident(&mut head, &stmt.name.raw);
                head.push_str("->");
                if let Some(to) = to {
                    head.push_str(&idents_joined(to, " | "));
                }
                // The LIGHTWEIGHT word trails the header — the same
                // greedy-read position the parser accepts it in (an
                // endpoint named `lightweight` reads as an endpoint;
                // the flag needs the list closed). Never on the
                // `(None, None)` spelling: the fold requires both
                // sides named, so that shape is unrepresentable.
                if *lightweight {
                    head.push_str(" lightweight");
                }
            }
        },
        TableKind::View { query } => {
            head.push_str(" view ");
            push_ident(&mut head, &stmt.name.raw);
            head.push_str(" {");
            w.line(&head);
            // The block IS the query — a keyless body expression —
            // then the optional trailing clauses, all inside the
            // block (write_table_clauses closes it). The query runs
            // the layout pass like any clause value: a long SELECT
            // rides clause-per-line (fields indented under SELECT,
            // FROM/WHERE/GROUP at the block's indentation).
            w.indent += 1;
            let mut line = String::new();
            w.push_canon(&mut line, &query.canon);
            line.push(';');
            w.line(&line);
            write_table_clauses(w, stmt, true);
            return;
        }
    }
    if empty {
        head.push(';');
        w.line_a(&stmt.clause_comments, "", &head);
        w.comments(&stmt.trailing_comments);
        return;
    }
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    write_table_clauses(w, stmt, false);
}

/// The non-view clause body (grammar-block order: schema flag, drop,
/// changefeed, permissions, graphql, the INLINE caps, enforced, then
/// the nested definitions in the canonical member layout —
/// [`write_members`]). Closes the block.
fn write_table_clauses(w: &mut Writer, stmt: &Table, view: bool) {
    if !view {
        if let Some(schema_full) = stmt.schema_full {
            // Both spellings fold into the one flag — an anchor rides
            // whichever word the file authored.
            let path = if schema_full {
                "schemafull"
            } else {
                "schemaless"
            };
            w.line_a(
                &stmt.clause_comments,
                path,
                if schema_full {
                    "schemafull;"
                } else {
                    "schemaless;"
                },
            );
        }
        if stmt.drop {
            w.line_a(&stmt.clause_comments, "drop", "drop;");
        }
        if let Some(changefeed) = &stmt.changefeed {
            write_changefeed(w, &stmt.clause_comments, changefeed);
        }
    }
    if let Some(permissions) = &stmt.permissions {
        write_op_permissions(w, &stmt.clause_comments, permissions);
    }
    write_graphql(w, &stmt.clause_comments, &stmt.graphql);
    // A view does not author the caps (SDL's view grammar omits them
    // and the reader routes a cap-bearing view to raw) — a
    // library-built AST that pairs them is a bug, mirroring the view
    // arm's schema/changefeed debug_assert in codegen. Outside the
    // `if !view` block: inside it `view` is statically false and the
    // assert would be vacuous.
    debug_assert!(!view || (stmt.inline_edges.is_none() && stmt.inline_references.is_none()));
    if !view {
        if let Some(cap) = stmt.inline_edges {
            w.line_a(
                &stmt.clause_comments,
                "inline edges",
                &format!("inline edges: {cap};"),
            );
        }
        if let Some(cap) = stmt.inline_references {
            w.line_a(
                &stmt.clause_comments,
                "inline references",
                &format!("inline references: {cap};"),
            );
        }
        // Never under `lightweight`: the header word carries the flag
        // (the AST's parse-fold pairs them; the shortest spelling).
        if matches!(
            &stmt.kind,
            TableKind::Relation {
                enforced: true,
                lightweight: false,
                ..
            }
        ) {
            w.line_a(&stmt.clause_comments, "enforced", "enforced;");
        }
        write_members(w, &stmt.nested);
    }
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

/// The canonical member layout table and mixin bodies share
/// (spec/table.md, spec/mixin.md): fields, then spreads (RELATIVE
/// ORDER PRESERVED — the last-occurrence-wins resolution's semantics
/// lives there), then indexes, then events; each group a stable sort
/// (authored order rides through inside a group, and comments
/// anchored to a member ride its statement through the reorder).
///
/// The group order is define-order safe (the engine validates an
/// index's field existence at define, schemafull tables): every
/// EXPLICIT field prints before the spreads (a spread-carried index
/// may name one, which is why fields come first),
/// the spreads print before the EXPLICIT indexes (one may name a
/// member field), and relative spread order carries the one ordering
/// members can depend on among themselves (a later spread's index
/// naming an earlier spread's field keeps its order; one naming a
/// LATER spread's field was already broken in the authored
/// SCHEMAFULL file — emission is authored order; schemaless tables
/// accept an index above its field, and the preserved spread order
/// carries those through unchanged). Events define against nothing.
/// A computed field and an index on it reject in BOTH orders, so no
/// working file holds the pair to break.
fn write_members(w: &mut Writer, members: &[Stmt]) {
    for member in members
        .iter()
        .filter(|member| matches!(member, Stmt::Field(_)))
    {
        write_stmt(w, member, true);
    }
    for member in members
        .iter()
        .filter(|member| matches!(member, Stmt::Spread(_)))
    {
        write_stmt(w, member, true);
    }
    for member in members
        .iter()
        .filter(|member| matches!(member, Stmt::Index(_)))
    {
        write_stmt(w, member, true);
    }
    for member in members
        .iter()
        .filter(|member| matches!(member, Stmt::Event(_)))
    {
        write_stmt(w, member, true);
    }
}

/// `mixin <name> { members };` (spec/mixin.md) — the SDL-only
/// statement: members print in the canonical member layout
/// ([`write_members`], `on:` omitted — the spreading tables are
/// foreign to the statement). An empty block collapses to the bare
/// header. No doc-comment line exists to print — `///` is rejected at
/// parse (no DDL emission, no COMMENT target; rule 5).
fn write_mixin(w: &mut Writer, stmt: &Mixin) {
    let mut head = String::from("mixin ");
    push_ident(&mut head, &stmt.name.raw);
    if stmt.members.is_empty() {
        head.push(';');
        w.line_a(&stmt.clause_comments, "", &head);
        w.comments(&stmt.trailing_comments);
        return;
    }
    head.push_str(" {");
    w.line(&head);
    w.indent += 1;
    write_members(w, &stmt.members);
    w.indent -= 1;
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

/// `...<name>;` — one spread-entry line at its nested position
/// (spec/mixin.md): the entry's leading comments and same-line note
/// anchor to it (rule 5, the empty clause path).
fn write_spread(w: &mut Writer, stmt: &Spread) {
    let mut line = String::from("...");
    push_ident(&mut line, &stmt.name.raw);
    line.push(';');
    w.line_a(&stmt.clause_comments, "", &line);
}

/// `include "<path>";` (spec/include.md) — the composition statement
/// survives fmt like `mixin` does (the canonical writer never
/// lowers): the authored path prints as a re-quoted string literal —
/// resolution is the loader's job, never the writer's.
fn write_include(w: &mut Writer, stmt: &Include) {
    let mut line = String::from("include ");
    quoted_string(&mut line, &stmt.path);
    line.push(';');
    w.line_a(&stmt.clause_comments, "", &line);
}

fn write_field(w: &mut Writer, stmt: &Field, nested: bool) {
    if let Some(comment) = &stmt.comment {
        w.doc_comment(comment);
    }
    // The block body, buffered: the shape decisions below (the
    // computed short form, the empty-block collapse) read its
    // emptiness instead of a hand-maintained clause census — one home
    // for "what prints in a field block".
    let mut body = Writer {
        out: String::new(),
        indent: w.indent + 1,
        pretty: w.pretty,
        max_width: w.max_width,
    };
    // Grammar-block order: on, default, value, assert, computed,
    // readonly, flexible, inline, reference, permissions, graphql
    // (spec/field.md's listing — inline trails flexible there, unlike
    // the DDL flag glue, which is upstream's own order). A
    // standalone computed-only field prints `computed:` — `on:` is
    // required, and a header `=` admits no trailing block.
    if !nested {
        let mut on = String::from("on: ");
        push_ident(&mut on, &stmt.on.raw);
        on.push(';');
        body.line_a(&stmt.clause_comments, "on", &on);
    }
    if let Some(default) = &stmt.default {
        match default {
            FieldDefault::Set(expr) => {
                body.canon_clause_a(&stmt.clause_comments, "default", "default", &expr.canon)
            }
            FieldDefault::Always(expr) => body.canon_clause_a(
                &stmt.clause_comments,
                "default always",
                "default always",
                &expr.canon,
            ),
        }
    }
    if let Some(value) = &stmt.value {
        body.canon_clause_a(&stmt.clause_comments, "value", "value", &value.canon);
    }
    if let Some(assert) = &stmt.assert {
        body.canon_clause_a(&stmt.clause_comments, "assert", "assert", &assert.canon);
    }
    if let Some(computed) = &stmt.computed {
        body.canon_clause_a(
            &stmt.clause_comments,
            "computed",
            "computed",
            &computed.canon,
        );
    }
    if stmt.readonly {
        body.line_a(&stmt.clause_comments, "readonly", "readonly;");
    }
    if stmt.flexible {
        body.line_a(&stmt.clause_comments, "flexible", "flexible;");
    }
    if stmt.inline {
        body.line_a(&stmt.clause_comments, "inline", "inline;");
    }
    if let Some(reference) = &stmt.reference {
        match reference {
            OnDelete::Ignore => body.line_a(&stmt.clause_comments, "reference", "reference;"),
            action => {
                body.block_a(
                    &stmt.clause_comments,
                    "reference",
                    "reference",
                    |body| match action {
                        OnDelete::Reject => body.clause_a(
                            &stmt.clause_comments,
                            "reference.on delete",
                            "on delete",
                            &"reject",
                        ),
                        OnDelete::Cascade => body.clause_a(
                            &stmt.clause_comments,
                            "reference.on delete",
                            "on delete",
                            &"cascade",
                        ),
                        OnDelete::Ignore => unreachable!("handled above"),
                        OnDelete::Unset => body.clause_a(
                            &stmt.clause_comments,
                            "reference.on delete",
                            "on delete",
                            &"unset",
                        ),
                        OnDelete::Then(expr) => body.canon_clause_a(
                            &stmt.clause_comments,
                            "reference.on delete",
                            "on delete",
                            &expr.canon,
                        ),
                    },
                )
            }
        }
    }
    if let Some(permissions) = &stmt.permissions {
        write_op_permissions(&mut body, &stmt.clause_comments, permissions);
    }
    write_graphql(&mut body, &stmt.clause_comments, &stmt.graphql);
    // Header: `field <path>[: <kind>]`, plus the `= <expr>` short
    // form when the computed expression is the only clause — nested
    // only, since a standalone field needs `on:`, which forces the
    // block form.
    let mut head = String::from("field ");
    push_field_path(&mut head, &stmt.name);
    let kind_text = stmt.kind.as_ref().map(|k| canonical_kind_text(&k.text));
    if let Some(kind) = &kind_text {
        head.push_str(": ");
        head.push_str(kind);
    }
    if nested
        && let Some(computed) = &stmt.computed
        && body.out.lines().count() == 1
        && !Writer::has_after(&stmt.clause_comments, "computed")
    {
        head.push_str(" = ");
        body.push_canon_at(w.indent, &mut head, &computed.canon);
        head.push(';');
        w.short_form_line(&stmt.clause_comments, "computed", &head);
        w.comments(&stmt.trailing_comments);
        return;
    }
    // A kind union too long for the budget breaks at its top-level
    // ` | ` — layout only (the canonical kind text regenerates from
    // the parsed AST on every write, so the break never stores). The
    // prefix is the head without `: <kind>`; the suffix closes the
    // header (`;` for the collapsed empty block, ` {` for the block).
    let union_head = |w: &mut Writer, kind: &str, suffix: &str| {
        let prefix_len = head.len() - kind.len() - 2;
        w.kind_union_clause(&head[..prefix_len], kind, suffix)
    };
    if nested && body.out.is_empty() {
        if let Some(kind) = &kind_text
            && union_head(w, kind, ";")
        {
            w.comments(&stmt.trailing_comments);
            return;
        }
        head.push(';');
        w.line_a(&stmt.clause_comments, "", &head);
        w.comments(&stmt.trailing_comments);
        return;
    }
    if let Some(kind) = &kind_text
        && union_head(w, kind, " {")
    {
        w.out.push_str(&body.out);
        w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
        return;
    }
    head.push_str(" {");
    w.line(&head);
    w.out.push_str(&body.out);
    w.close_a(&stmt.clause_comments, &stmt.trailing_comments);
}

fn write_raw(w: &mut Writer, stmt: &Raw) {
    w.line("raw {");
    // Interior bytes verbatim (rule 8): the trimmed body's own lines,
    // unmodified, then the closer. A same-line note after the
    // statement's `};` joins the closer (the empty-path anchor).
    let (_, after) = w.anchored(&stmt.clause_comments, "");
    let mut close = String::from("};");
    close.push_str(&Writer::after_suffix(after));
    for line in stmt.body.split('\n') {
        w.line(line);
    }
    w.line(&close);
}

// ---- shared clause shapes ----

/// Splits a kind at its top-level ` | ` — pipes outside every bracket
/// pair (angle brackets, parens, brackets, braces) and outside string
/// literals (`\` escapes honored). `None` when the kind carries no
/// top-level union: there is nothing to break at.
fn union_arms(kind: &str) -> Option<Vec<String>> {
    let mut depth = 0i32;
    let mut in_string: Option<char> = None;
    let mut current = String::new();
    let mut arms: Vec<String> = Vec::new();
    let mut chars = kind.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(quote) = in_string {
            current.push(c);
            if c == '\\' {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            } else if c == quote {
                in_string = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                in_string = Some(c);
                current.push(c);
            }
            '<' | '(' | '[' | '{' => {
                depth += 1;
                current.push(c);
            }
            '>' | ')' | ']' | '}' => {
                depth -= 1;
                current.push(c);
            }
            '|' if depth == 0 => {
                arms.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    arms.push(current.trim().to_string());
    (arms.len() > 1).then_some(arms)
}

/// The single-value permission form (spec/README permission form 1).
fn write_permission(w: &mut Writer, anchors: &[ClauseComment], permission: &Permission) {
    match permission {
        Permission::None => w.line_a(anchors, "permissions", "permissions: none;"),
        Permission::Full => w.line_a(anchors, "permissions", "permissions: full;"),
        Permission::Expr(expr) => {
            w.canon_clause_a(anchors, "permissions", "permissions", &expr.canon)
        }
    }
}

/// The per-operation form shared by `table` and `field` (form 2):
/// groups as written, operations comma-joined, values spelled out.
/// Group anchors ride the statement's map under `permissions.#<n>`,
/// n the emitted index (the parse order).
fn write_op_permissions(w: &mut Writer, anchors: &[ClauseComment], permissions: &OpPermissions) {
    match permissions {
        OpPermissions::None => w.line_a(anchors, "permissions", "permissions: none;"),
        OpPermissions::Full => w.line_a(anchors, "permissions", "permissions: full;"),
        OpPermissions::Groups(groups) => {
            w.block_a(anchors, "permissions", "permissions", |w| {
                for (index, group) in groups.iter().enumerate() {
                    let path = format!("permissions.#{index}");
                    let ops = group
                        .ops
                        .iter()
                        .map(|op: &PermissionOp| op.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    match &group.value {
                        Permission::None => w.line_a(anchors, &path, &format!("{ops}: none;")),
                        Permission::Full => w.line_a(anchors, &path, &format!("{ops}: full;")),
                        Permission::Expr(expr) => {
                            w.canon_clause_a(anchors, &path, &ops, &expr.canon)
                        }
                    }
                }
            });
        }
    }
}

fn write_middleware_clause(w: &mut Writer, anchors: &[ClauseComment], middleware: &[Middleware]) {
    if middleware.is_empty() {
        return;
    }
    let (before, after) = w.anchored(anchors, "middleware");
    w.comments(before);
    // Built like canon_list_clause: the clause key rides the line
    // buffer (so argument layouts see their real column), the indent
    // joins at the end.
    let mut line = String::new();
    line.push_str("middleware: ");
    for (i, entry) in middleware.iter().enumerate() {
        if i > 0 {
            line.push_str(", ");
        }
        line.push_str(&fn_path_string(&entry.path));
        // Parens are mandatory in SDL (`name()` for no arguments), so
        // the AST's empty args unambiguously prints `()`.
        line.push('(');
        for (j, arg) in entry.args.iter().enumerate() {
            if j > 0 {
                line.push_str(", ");
            }
            w.push_canon(&mut line, &arg.canon);
        }
        line.push(')');
    }
    line.push(';');
    line.push_str(&Writer::after_suffix(after));
    for _ in 0..w.indent {
        w.out.push_str("  ");
    }
    w.out.push_str(&line);
    w.out.push('\n');
}

/// Whether the graphql sub-options have anything to print (a `Some`
/// with both fields folded away prints nothing).
fn graphql_present(graphql: &Option<Graphql>) -> bool {
    matches!(graphql, Some(g) if g.alias.is_some() || g.deprecated.is_some())
}

/// The `graphql { alias; deprecated }` sub-options shared by
/// `function`, `table`, and `field` — printed in place for a caller
/// already inside a block; nothing prints when both are absent.
fn write_graphql(w: &mut Writer, anchors: &[ClauseComment], graphql: &Option<Graphql>) {
    if !graphql_present(graphql) {
        return;
    }
    let graphql = graphql.as_ref().expect("present per graphql_present");
    let (before, after) = w.anchored(anchors, "graphql");
    w.comments(before);
    w.line("graphql {");
    w.indent += 1;
    if let Some(alias) = &graphql.alias {
        let mut quoted = String::new();
        quoted_string(&mut quoted, alias);
        w.clause_a(anchors, "graphql.alias", "alias", &quoted);
    }
    if let Some(deprecated) = &graphql.deprecated {
        let mut quoted = String::new();
        quoted_string(&mut quoted, deprecated);
        w.clause_a(anchors, "graphql.deprecated", "deprecated", &quoted);
    }
    w.indent -= 1;
    let mut close = String::from("};");
    close.push_str(&Writer::after_suffix(after));
    w.line(&close);
}

// ---- spelling helpers (canonical SDL words) ----

/// The authored spelling when carried, else the canonical word —
/// the `ns`/`db` keywords and the `columns:` key.
fn keyword_word<'a>(carried: &Carried, canonical: &'a str) -> &'a str {
    carried.0.unwrap_or(canonical)
}

/// The level word: the authored alias when carried (`kv`), else the
/// canonical lowercase word.
fn level_word(level: Level, spelling: Option<&'static str>) -> String {
    match spelling {
        Some(word) => word.to_string(),
        None => match level {
            Level::Root => "root".to_string(),
            Level::Namespace => "namespace".to_string(),
            Level::Database => "database".to_string(),
        },
    }
}

fn filter_word(filter: &Filter) -> String {
    match filter {
        Filter::Ascii => "ascii".to_string(),
        Filter::Lowercase => "lowercase".to_string(),
        Filter::Uppercase => "uppercase".to_string(),
        Filter::EdgeNgram(min, max) => format!("edgengram({min}, {max})"),
        Filter::Ngram(min, max) => format!("ngram({min}, {max})"),
        Filter::Snowball(language) => format!(
            "snowball({})",
            language_word(language.value, language.spelling)
        ),
        Filter::Mapper(path) => {
            let mut quoted = String::new();
            quoted_string(&mut quoted, path);
            format!("mapper({quoted})")
        }
    }
}

/// The snowball language's SDL word: the authored alias when carried,
/// else the lowercase full name.
fn language_word(language: crate::ast::Language, spelling: Option<&'static str>) -> String {
    match spelling {
        Some(word) => word.to_string(),
        None => language.as_str().to_ascii_lowercase(),
    }
}

fn algorithm_word(algorithm: crate::ast::Algorithm) -> String {
    algorithm.as_str().to_ascii_lowercase()
}

fn distance_word(distance: Distance) -> String {
    match distance {
        Distance::Minkowski(order) => format!("minkowski({})", order.0),
        other => other.as_str().to_ascii_lowercase(),
    }
}

fn vector_type_word(vector_type: VectorType) -> String {
    vector_type.as_str().to_ascii_lowercase()
}

/// An `<fn-path>`: the prefix, then the `::`-joined segments through
/// the shape rule (keyword-like words bare, anything else backticked).
fn fn_path_string(path: &crate::ast::FnPath) -> String {
    let mut out = String::new();
    match path.kind {
        FnKind::Fn => out.push_str("fn::"),
        FnKind::Mod => out.push_str("mod::"),
        FnKind::Bare => {}
    }
    out.push_str(&fn_path_segments_string(path));
    out
}

/// The `::`-joined segments only — the `fn` statement's name shape
/// (its keyword carries the prefix).
fn fn_path_segments_string(path: &crate::ast::FnPath) -> String {
    let mut out = String::new();
    for (i, segment) in path.segments.iter().enumerate() {
        if i > 0 {
            out.push_str("::");
        }
        push_path_segment(&mut out, segment);
    }
    out
}

/// One name-path segment through the shape rule: bare when
/// word-shaped (keyword-like words included), backtick-escaped
/// otherwise.
fn push_path_segment(out: &mut String, segment: &str) {
    if word_shaped(segment) {
        out.push_str(segment);
    } else {
        out.push('`');
        escape_with(out, segment, '`');
        out.push('`');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Diag;
    use crate::parser::{parse_file, parse_file_composable};

    /// The golden leg: the source parses clean, formats, the output
    /// re-parses clean, holds the same definition (span-blind), and
    /// formats to itself — then the snapshot pins the exact text.
    /// Every statement's spec examples ride through here.
    fn assert_fmt_golden(name: &str, src: &str) {
        assert_fmt_golden_with(name, src, parse_file);
    }

    /// [`assert_fmt_golden`] over the composable parse — the entry
    /// `include` statements parse through (spec/include.md).
    fn assert_fmt_golden_composable(name: &str, src: &str) {
        assert_fmt_golden_with(name, src, parse_file_composable);
    }

    fn assert_fmt_golden_with(name: &str, src: &str, parse: fn(&str) -> (SdlFile, Vec<Diag>)) {
        let (ast, diags) = parse(src);
        assert!(diags.is_empty(), "{src}: {diags:?}");
        let once = write_canonical(&ast);
        let (reparsed, diags) = parse(&once);
        assert!(diags.is_empty(), "fmt output stopped parsing:\n{once}");
        assert!(
            ast.same_definition(&reparsed),
            "fmt changed the definition:\n--- SDL ---\n{src}\n--- fmt ---\n{once}"
        );
        assert_eq!(write_canonical(&reparsed), once, "not idempotent:\n{once}");
        insta::assert_snapshot!(name, once);
    }

    // ---- the simple five ----

    #[test]
    fn fmt_namespace_doc() {
        assert_fmt_golden("fmt-namespace-doc", "/// Acme Corporation\nnamespace acme;");
    }

    #[test]
    fn fmt_namespace_keyword_alias_carried() {
        // The authored `ns` stays; fmt never rewrites aliases.
        assert_fmt_golden("fmt-namespace-alias", "ns acme { };");
    }

    #[test]
    fn fmt_sequence_short_and_block() {
        assert_fmt_golden("fmt-sequence-short", "sequence invoice_number = 1000;");
        assert_fmt_golden(
            "fmt-sequence-block",
            "sequence order_number { batch: 100; start: 5000; timeout: 5s; };",
        );
    }

    #[test]
    fn fmt_param_short_and_block() {
        assert_fmt_golden(
            "fmt-param-short",
            "/// Default page size for list endpoints\nparam $default_limit = 50;",
        );
        assert_fmt_golden(
            "fmt-param-block",
            "param $guarded { permissions: $auth.role = 'admin'; value: 50; };",
        );
    }

    #[test]
    fn fmt_database_keyword_alias_carried() {
        assert_fmt_golden(
            "fmt-database",
            "/// Primary application database\ndb app { strict; changefeed { expiry: 7d; include original; }; };",
        );
    }

    #[test]
    fn fmt_bucket() {
        assert_fmt_golden(
            "fmt-bucket",
            "bucket uploads { backend: 'file:/srv/uploads'; readonly; permissions: full; };",
        );
    }

    // ---- the medium six ----

    #[test]
    fn fmt_user_level_alias_carried() {
        assert_fmt_golden(
            "fmt-user",
            "/// CI deployment account\nuser deployer { on: ns; passhash: '$argon2id$v=19$'; roles: editor, viewer; duration { token: 1h; }; };",
        );
    }

    #[test]
    fn fmt_user_passscram() {
        // The 3.3 additive clause — the canonical SDL order puts it
        // after the pass clauses, before roles (the DDL order).
        assert_fmt_golden(
            "fmt-user-passscram",
            "user provisioned { on: root; passscram: 'SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$nQpbZ77WudtqufPwikHXGRt6g2QJ4zns8bZLw273DRM=:jn2amWP1q1h+jgjy0YTO14S6/F02SV7taipOeB7ef20='; };",
        );
    }

    #[test]
    fn fmt_analyzer_language_alias_carried() {
        // `eng` is authored and stays authored; the canonical full
        // name would print for `snowball(english)`.
        assert_fmt_golden(
            "fmt-analyzer",
            "/// English text search\nanalyzer `english` { function: fn::strip_markup; tokenizers: blank, camel, class, punct; filters: snowball(eng), edgengram(2, 10), lowercase; };",
        );
    }

    #[test]
    fn fmt_event_async_forms() {
        assert_fmt_golden(
            "fmt-event",
            "/// Log every user creation\nevent user_created { on: `user`; then: { CREATE log SET user = $after.id }; when: $event = 'CREATE'; };",
        );
        assert_fmt_golden(
            "fmt-event-async",
            "event webhook_fanout { on: invoice; async { retry: 5; maxdepth: 8; }; then: (CREATE delivery SET invoice = $after.id), (UPDATE stats:webhooks SET fired = fired + 1); };",
        );
        assert_fmt_golden(
            "fmt-event-async-bare",
            "event e { on: t; async; then: 1; };",
        );
    }

    #[test]
    fn fmt_module() {
        assert_fmt_golden(
            "fmt-module",
            "/// Colour helpers\nmodule color { from: f\"modules:/color.surli\"; unsigned; };",
        );
        assert_fmt_golden(
            "fmt-module-silo",
            "module { from: silo::acme::palette::<1.2.0>; unsigned; };",
        );
    }

    #[test]
    fn fmt_function_two_blocks() {
        assert_fmt_golden(
            "fmt-function",
            "/// Post permissions\nfn can_edit($p: record<post>) -> bool { RETURN $auth.id = $p.author OR $auth.role = 'admin'; } { permissions: $auth.admin = true; };",
        );
        assert_fmt_golden(
            "fmt-function-graphql",
            "fn go::heavy($n: array<int, 5>) -> number { RETURN $n.len(); } { graphql { alias: 'heavyFn'; deprecated: 'use len'; }; };",
        );
    }

    #[test]
    fn fmt_config_three_variants() {
        assert_fmt_golden(
            "fmt-config-default",
            "config default { namespace: 'acme'; database: 'shop'; };",
        );
        assert_fmt_golden(
            "fmt-config-api",
            "config api { middleware: api::timeout(5s), api::log(); permissions: $auth.admin = true; };",
        );
        assert_fmt_golden(
            "fmt-config-graphql",
            "config graphql { tables { include: `user`, `post`; }; functions: none; depth: 4; complexity: 100; introspection: none; };",
        );
    }

    #[test]
    fn fmt_index_variants_and_columns_alias() {
        // `columns:` is authored and stays authored.
        assert_fmt_golden(
            "fmt-index-unique-columns",
            "index unique one_email { on: user; columns: email; };",
        );
        assert_fmt_golden(
            "fmt-index-fulltext",
            "/// Inbox thread lookup\nindex fulltext search { on: message; fields: body; analyzer: `english`; bm25: (1.2, 0.75); concurrently; };",
        );
        assert_fmt_golden(
            "fmt-index-hnsw",
            "index hnsw vec_idx { on: pt; fields: vec; dimension: 3; distance: cosine; type: f64; m: 16; m0: 32; lm: 0.5; efc: 200; extend_candidates; };",
        );
        assert_fmt_golden(
            "fmt-index-count",
            "index count done { on: task; where: done = true; };",
        );
        assert_fmt_golden(
            "fmt-index-diskann",
            "index diskann v { on: pt; fields: vec; dimension: 64; distance: cosine; degree: 50; l_build: 90; alpha: 1.2; hashed_vector; };",
        );
    }

    #[test]
    fn fmt_api_groups_and_any() {
        assert_fmt_golden(
            "fmt-api",
            "/// Admin API with fallback\napi '/admin/*rest' { any { middleware: api::timeout(5s); then: { { status: 404, body: { error: 'no such route' } } }; }; get, delete { permissions: $auth.admin = true; then: { { status: 200 } }; }; };",
        );
    }

    #[test]
    fn fmt_access_variants() {
        assert_fmt_golden(
            "fmt-access-jwt",
            "/// SSO token verification\naccess corp_sso { on: db; type: jwt; url: 'https://sso.acme.example/jwks'; duration { session: 8h; }; };",
        );
        assert_fmt_golden(
            "fmt-access-record",
            "access signing { on: db; type: record; signin: { SELECT * FROM user WHERE email = $email AND crypto::argon2::compare($pass, $auth.password) }; jwt { algorithm: hs512; key: '[REDACTED]'; issuer { algorithm: hs512; key: '[REDACTED]'; }; }; refresh; };",
        );
        assert_fmt_golden(
            "fmt-access-bearer",
            "access tokens { on: database; type: bearer; for: record; jwt { url: 'https://keys.example/jwks'; issuer { algorithm: es256; key: '[REDACTED]'; }; }; };",
        );
    }

    // ---- the nesting statements ----

    #[test]
    fn fmt_table_with_nested_definitions() {
        assert_fmt_golden(
            "fmt-table-nested",
            "/// Application users\ntable `user` { schemafull; permissions { select: $auth.id != none; delete: $auth.admin = true; }; field email: string { assert: string::is_email($value); default: 'unknown@example.com'; }; field display_name = string::uppercase(email); index unique one_email { fields: email; }; event audited { when: $event = 'UPDATE'; then: (CREATE audit SET who = $auth.id); }; };",
        );
    }

    #[test]
    fn fmt_table_relation_and_view() {
        // The caps print after permissions/graphql — the spec's
        // grammar-block order (2026-09-28 decision: permissions
        // first).
        assert_fmt_golden(
            "fmt-table-caps-after-permissions",
            "table capd { schemaless; permissions: full; inline edges: 8; inline references: 16; };",
        );
        assert_fmt_golden(
            "fmt-table-relation",
            "table user | bot->wrote->post { enforced; };",
        );
        assert_fmt_golden("fmt-table-relation-unconstrained", "table->wrote->;");
        assert_fmt_golden("fmt-table-relation-canonical", "table relation wrote { };");
        assert_fmt_golden(
            "fmt-table-view",
            "table view stats { SELECT region, count() AS n FROM purchase GROUP BY region; permissions: none; };",
        );
        assert_fmt_golden("fmt-table-any", "table any anything { schemaless; drop; };");
    }

    #[test]
    fn fmt_field_shapes() {
        assert_fmt_golden(
            "fmt-field-standalone",
            "field email: string { on: `user`; assert: string::is_email($value); };",
        );
        // The flag run's grammar-block order: readonly, flexible,
        // inline (spec/field.md's listing).
        assert_fmt_golden(
            "fmt-field-flags",
            "field meta: object { on: t; readonly; flexible; inline; };",
        );
        assert_fmt_golden(
            "fmt-field-computed-standalone",
            "field display_name: string { on: `user`; computed: string::uppercase(email); };",
        );
        assert_fmt_golden(
            "fmt-field-reference",
            "field author: record<user> { on: post; reference { on delete: cascade; }; };",
        );
        assert_fmt_golden(
            "fmt-field-permissions",
            "field secret: object { on: vault; flexible; readonly; permissions: none; };",
        );
        assert_fmt_golden(
            "fmt-field-default-always",
            "field seen_at: datetime { on: visit; default always: time::now(); };",
        );
    }

    #[test]
    fn fmt_raw_verbatim() {
        // The interior rides byte-for-byte: comments, spellings,
        // spacing — only the edges trim.
        assert_fmt_golden(
            "fmt-raw",
            "raw {\n  // a comment inside the escape hatch\n  DEFINE TABLE legacy AS SELECT * FROM old;\n};",
        );
    }

    #[test]
    fn fmt_comment_led_group() {
        // The separator blank falls before the whole group: a `//`
        // run leading a statement breaks from the previous definition
        // exactly as a bare statement head would — the comment never
        // glues to the statement above it.
        assert_fmt_golden(
            "fmt-comment-led-group",
            "table a { schemaless; };\n// the b side\ntable b { schemaless; };",
        );
    }

    #[test]
    fn fmt_include_group() {
        // The one exception to the blank-line separation: a run of
        // adjacent includes packs tight (the import-group style),
        // while the group still breaks from every other definition —
        // and a comment leading one include anchors forward into the
        // run, never back to the include above it.
        assert_fmt_golden_composable(
            "fmt-include-group",
            "table user;\ninclude \"a.sdl\";\ninclude \"b.sdl\";\n// picker feeds\ninclude \"c.sdl\";\nparam $x;",
        );
    }

    #[test]
    fn fmt_escaping_and_kinds() {
        // DDL keywords, the SDL-only reservations, and kind spellings
        // all round-trip.
        assert_fmt_golden(
            "fmt-escaping",
            "table `raw` { field `type`: option<int | string>; field `view`: set<record<person | post>>; };",
        );
        assert_fmt_golden(
            "fmt-kind-canonicalized",
            "table t { field u: none | int; field v: record < person >; field w: array<int,10>; };",
        );
    }

    /// The alias carry in one file: `ns`, `db`, `on: kv`,
    /// `snowball(eng)`, `columns:` — none rewritten, all comparing
    /// equal to their canonical spellings (the writer contract
    /// exercised end to end).
    #[test]
    fn fmt_alias_carry_never_rewrites() {
        let src = "ns acme;\nuser root_admin { on: kv; password: 'x'; };\nanalyzer a { filters: snowball(eng); };\nindex by_email { on: user; columns: email; };";
        let (ast, diags) = parse_file(src);
        assert!(diags.is_empty(), "{diags:?}");
        let once = write_canonical(&ast);
        assert!(once.contains("ns acme;"), "ns keyword churned:\n{once}");
        assert!(once.contains("on: kv;"), "level alias churned:\n{once}");
        assert!(
            once.contains("snowball(eng)"),
            "language alias churned:\n{once}"
        );
        assert!(
            once.contains("columns: email;"),
            "clause-key alias churned:\n{once}"
        );
        // The canonical spellings parse to the SAME AST (provenance
        // sits outside equality).
        let canonical = "namespace acme;\nuser root_admin { on: root; password: 'x'; };\nanalyzer a { filters: snowball(english); };\nindex by_email { on: user; fields: email; };";
        let (ast_canonical, diags) = parse_file(canonical);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(ast.same_definition(&ast_canonical));
        // …and the DDL still prints canonical words (codegen
        // unchanged by the carry).
        assert!(crate::codegen::emit(&ast).contains("ON ROOT"));
        assert!(crate::codegen::emit(&ast).contains("SNOWBALL(ENGLISH)"));
    }

    /// The mixin statement and its spread entries (spec/mixin.md):
    /// fmt is the one writer that never lowers — the spread
    /// round-trips at its position (the fixed-point asserts below
    /// prove `same_definition` survives), the statement's comments
    /// anchor like any statement's, and an empty mixin collapses to
    /// its bare header.
    #[test]
    fn fmt_mixin() {
        assert_fmt_golden(
            "fmt-mixin",
            "// shared audit columns\nmixin timestamped {\n  created_at: datetime;\n  \
             updated_at: datetime { default: time::now(); };\n};\n\ntable post schemafull {\n  \
             ...timestamped; // audit trail\n  title: string;\n  created_at: datetime; // \
             overrides the member\n};",
        );
        assert_fmt_golden("fmt-mixin-empty", "mixin base;");
    }
}
